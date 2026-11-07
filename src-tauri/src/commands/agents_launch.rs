// SPDX-License-Identifier: MIT
//! The frontend's interactive launch of a named agent.
//!
//! The renderer spawns an agent as a login shell plus an injected launch line,
//! so this is the PTY spawn site of [`crate::agents::launch`]: the renderer
//! mints the session id, asks [`agent_launch_prepare`] for that session, and
//! spawns with exactly what comes back. The command prepares the agent, projects
//! its MCP scope and writes its generated permission files for that session;
//! the renderer never computes args, scope or permissions itself.
//!
//! What the launch prepared cannot be held across the IPC return, so it is
//! staged on the launch registry under the session id: the agent revision the
//! session runs, a Claude agent's per-launch settings file, and a Codex agent's
//! launch, whose scoped-home keys a relaunch writes again. The bind records them
//! on the session's entry, so the revision comes from what was prepared, never
//! from the renderer. Either way the file goes when the session does, or when
//! its spawn fails.

use std::path::Path;

use log::warn;
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::agents::adapter::{LaunchMode, ResolvedLaunchProfile};
use crate::agents::keys::AgentKey;
use crate::agents::launch::{self, AgentLaunch, LaunchSettingsFile, PrepareOptions};
use crate::agents::launch_check::LaunchWarning;
use crate::agents::service;
use crate::commands::dashboard_launch::{DashboardLaunchState, StagedAgentLaunch};
use crate::commands::mcp_scope::{ScopeCreation, ScopeOutcome};
use crate::domain::Principal;

use super::async_helpers::await_blocking;

/// The account the PTY spawn resolves for an agent launch: the agent's runtime,
/// with this user's binding for the agent as the explicit override.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentLaunchAccount {
    pub runtime: codesense_pty_core::agent_cli::AgentCli,
    pub override_account_id: Option<String>,
    pub override_account_generation: Option<u64>,
}

/// Everything the renderer spawns an agent's terminal with.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedAgentLaunch {
    /// The runtime's CLI as this app resolved it, which the launch line runs.
    pub agent_path: String,
    /// Canonical prepared product target; other launch kinds retain their supplied cwd.
    pub cwd: Option<String>,
    /// The runtime's launch profile for the agent. `args` already carry the
    /// flags for the files written for this launch (Claude's `--settings`).
    pub launch: ResolvedLaunchProfile,
    pub account: AgentLaunchAccount,
    /// `"<key>@<revision>"`, recorded on the session at bind.
    pub agent: String,
    pub manifest_hash: String,
    /// The skill names the startup manifest provides, recorded on the session at bind.
    pub provided_skills: Vec<String>,
    pub warnings: Vec<LaunchWarning>,
    /// The agent's own first prompt, for a launch that brings none.
    pub default_prompt: Option<String>,
    pub default_role: Option<String>,
    pub prompt_in_argv: bool,
    /// The agent's display name: the title its terminal tab gets when the
    /// launcher names none.
    pub name: String,
    /// The projected MCP scope: its launch flags and the opaque environment
    /// binding the spawn consumes (Codex's scoped `CODEX_HOME`).
    pub scope: ScopeOutcome,
}

/// What the launch of `launch` (its CLI at `agent_path`) for session `session_id` in `cwd` writes and
/// runs with: the MCP scope projected and the generated entries written for
/// that session (paths raw, since the renderer quotes every argv token). The
/// settings file and the scope creation are returned for the caller to hold.
pub(crate) fn apply_for_session(
    launch: AgentLaunch,
    agent_path: &Path,
    session_id: &str,
    cwd: &Path,
    account_home: Option<&Path>,
    attribution_hook_trust: bool,
) -> Result<
    (
        PreparedAgentLaunch,
        Option<ScopeCreation>,
        Option<LaunchSettingsFile>,
    ),
    String,
> {
    let args = launch.launch.args.clone();
    let version = launch.cli_version.clone();
    let cli = super::mcp_scope::LaunchCli::new(agent_path, &args)
        .with_version(version.as_deref())
        .with_attribution_hook_trust(attribution_hook_trust);
    apply_for_session_with_cli(launch, agent_path, session_id, cwd, account_home, &cli)
}

fn apply_for_session_with_cli(
    launch: AgentLaunch,
    agent_path: &Path,
    session_id: &str,
    cwd: &Path,
    account_home: Option<&Path>,
    cli: &super::mcp_scope::LaunchCli<'_>,
) -> Result<
    (
        PreparedAgentLaunch,
        Option<ScopeCreation>,
        Option<LaunchSettingsFile>,
    ),
    String,
> {
    let (scope, creation, applied) = launch::project_and_apply_for_cli(
        &launch,
        session_id,
        cwd,
        account_home,
        Some(cli),
        str::to_string,
    )?;
    let mut profile = launch.launch.clone();
    applied.drop_agent_settings(&mut profile.args);
    profile.args.extend(applied.argv(str::to_string));
    // The same intent the scope was projected against, minus the policy, which the
    // spawn resolves again from the runtime.
    let input = launch.account_input();
    let account = AgentLaunchAccount {
        runtime: launch.runtime,
        override_account_id: input.override_account_id,
        override_account_generation: input.override_account_generation,
    };
    let prepared = PreparedAgentLaunch {
        agent_path: agent_path.to_string_lossy().into_owned(),
        cwd: None,
        launch: profile,
        account,
        agent: launch.revision_label(),
        manifest_hash: launch.manifest_hash.clone(),
        provided_skills: launch.provided_skills(),
        warnings: launch
            .warnings
            .iter()
            .cloned()
            .chain(crate::agents::launch_check::root_document_warnings(
                launch.runtime,
                cwd,
            ))
            .collect(),
        default_prompt: launch.default_prompt.clone(),
        default_role: launch.queue_role.clone(),
        prompt_in_argv: false,
        name: launch.name.clone(),
        scope,
    };
    Ok((prepared, creation, applied.settings))
}

/// The renderer/dashboard producer's one-use environment handoff, consumed by PTY spawn.
pub(crate) fn bind_prepared_launch_environment<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    prepared: &mut PreparedAgentLaunch,
    creation: Option<ScopeCreation>,
    requires_spend_holds: bool,
) -> Vec<(String, String)> {
    // Only an interactive launch owns its PTY; a headless one is captured as it is.
    prepared.scope.execution = if prepared.launch.mode == LaunchMode::Interactive {
        super::mcp_scope::scoped_execution(
            prepared.account.runtime,
            Path::new(&prepared.agent_path),
            super::mcp_scope::compose_args(
                prepared.launch.args.clone(),
                prepared.scope.extra_args.clone(),
            ),
        )
    } else {
        None
    };
    let recorded_scope_env = prepared.scope.extra_env.clone();
    prepared.scope.environment_binding = super::agent_accounts::mint_launch_environment(
        app,
        super::agent_accounts::LaunchEnvironment::for_agent(
            &prepared.launch,
            std::mem::take(&mut prepared.scope.extra_env),
            creation,
            requires_spend_holds,
        ),
    );
    recorded_scope_env
}

/// Interactive launch orchestration shared by the command and its seam tests.
/// Real selected startup must succeed before any scoped launch files are written.
pub(crate) async fn prepare_product_session(
    launch: AgentLaunch,
    agent_path: std::path::PathBuf,
    session_id: String,
    cwd: String,
    account_home: Option<std::path::PathBuf>,
    resources: Option<crate::agents::product_bootstrap::BootstrapResources>,
    attribution_hook_trust: bool,
) -> Result<
    (
        PreparedAgentLaunch,
        Option<ScopeCreation>,
        Option<LaunchSettingsFile>,
    ),
    String,
> {
    let product = crate::agents::product_bootstrap::prepare_with_resources(
        &launch,
        Path::new(&cwd),
        resources,
    )
    .await?;
    let canonical = product
        .canonical_root()
        .map(|root| root.to_string_lossy().into_owned());
    let cwd = canonical.clone().unwrap_or(cwd);
    let (mut prepared, creation, settings) =
        await_blocking("agent_launch_prepare.apply", move || {
            product.validate()?;
            apply_for_session(
                launch,
                &agent_path,
                &session_id,
                Path::new(&cwd),
                account_home.as_deref(),
                attribution_hook_trust,
            )
        })
        .await?;
    prepared.cwd = canonical;
    Ok((prepared, creation, settings))
}

/// The agent revision a bind may record: `label` (`"<key>@<revision>"`) when
/// that revision of that agent exists and resolves for `principal`.
pub(crate) fn verified_agent_revision(
    home: &Path,
    principal: &Principal,
    label: &str,
) -> Result<String, String> {
    let (key, revision) = label
        .trim()
        .rsplit_once('@')
        .ok_or_else(|| format!("{label:?} is not an agent revision"))?;
    let key = AgentKey::parse(key).ok_or_else(|| format!("{key:?} is not an agent key"))?;
    let revision: u32 = revision
        .parse()
        .map_err(|_| format!("{revision:?} is not a revision number"))?;
    service::get_pinned(home, principal, &key, revision)
        .map(|pinned| pinned.reference.to_string())
        .map_err(|e| e.to_string())
}

/// What checking a restored launch's recorded agent revision found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RestoredRevision {
    /// The revision resolves for the account; `binary` is the CLI its runtime runs.
    Verified { revision: String, binary: String },
    /// Not an agent revision this account can run: malformed, someone else's, or a
    /// revision its agent never had.
    Rejected(String),
    /// Not readable yet (its team's content not on disk yet, an I/O fault): ask again later.
    Transient(String),
}

/// Check a revision a restored launch recorded, the way a bind verifies one, telling an
/// agent that is not this account's apart from one whose files are not on disk yet.
pub(crate) fn restored_agent_revision(
    home: &Path,
    principal: &Principal,
    label: &str,
) -> RestoredRevision {
    use service::AgentError;
    let Some((key, revision)) = label.trim().rsplit_once('@') else {
        return RestoredRevision::Rejected(format!("{label:?} is not an agent revision"));
    };
    let Some(key) = AgentKey::parse(key) else {
        return RestoredRevision::Rejected(format!("{key:?} is not an agent key"));
    };
    let Ok(revision) = revision.parse::<u32>() else {
        return RestoredRevision::Rejected(format!("{revision:?} is not a revision number"));
    };
    match service::get_pinned(home, principal, &key, revision) {
        Ok(pinned) => RestoredRevision::Verified {
            revision: pinned.reference.to_string(),
            binary: codesense_pty_core::agent_cli::descriptor(pinned.agent.runtime)
                .binary
                .clone(),
        },
        Err(error @ AgentError::Io(_)) => RestoredRevision::Transient(error.to_string()),
        // The agent's team (or its whole context repo) may not be on disk yet.
        Err(error @ (AgentError::NotFound(_) | AgentError::InvalidLocation(_)))
            if !key.is_builtin() && key.resolves_for(principal) =>
        {
            RestoredRevision::Transient(error.to_string())
        }
        Err(error) => RestoredRevision::Rejected(error.to_string()),
    }
}

/// [`verified_agent_revision`] for the bind path: a revision that does not
/// verify is dropped with a warning, never failing the launch.
pub(crate) fn agent_revision_for_bind(
    home: &Path,
    principal: &Principal,
    label: &str,
) -> Option<String> {
    verified_agent_revision(home, principal, label)
        .inspect_err(|error| {
            warn!(target: "agents", "not recording agent revision {label:?} on a launch: {error}");
        })
        .ok()
}

/// Architecture: source and target admission precedes all preparation side effects.
pub(crate) fn admit_named_launch(
    home: &Path,
    principal: &Principal,
    scope: &crate::context_repo::caller_scope::CallerScope,
    reference: &str,
    target: Option<(&str, &str)>,
    cwd: &Path,
) -> Result<String, String> {
    if let Some((org, team)) = target {
        scope.require_launch_team(home, principal, org, team)?;
    }
    if let Some(owner) = crate::context_repo::caller_scope::workspace_team(home, principal, cwd) {
        scope.require_launch_team(home, principal, &owner.org, &owner.team)?;
    }
    launch::find_agent_for(home, principal, reference, target, scope).map(|key| key.to_string())
}

/// Prepare the agent `agent_key` (a rendered key, or an id in `team` then the
/// caller's personal space) for the terminal session `session_id` the renderer
/// is about to spawn in `cwd`. `team` is the launch's team key (`org:team`).
/// Returns the launch to spawn with; warnings never block it.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn agent_launch_prepare(
    app: AppHandle,
    registry: State<'_, DashboardLaunchState>,
    agent_key: String,
    team: Option<String>,
    cwd: String,
    session_id: String,
    mode: Option<LaunchMode>,
    prompt: Option<String>,
    account_id: Option<String>,
    account_generation: Option<u64>,
    frame_id: Option<String>,
    page_path: Option<String>,
    new_instance: Option<bool>,
) -> Result<PreparedAgentLaunch, String> {
    let scope = crate::context_repo::caller_scope::for_management_view(
        &app,
        page_path.as_deref(),
        frame_id.as_deref(),
    )?;
    let ctx = scope.context(&app).await?;
    let target_team = team.as_deref().and_then(|key| {
        let (org, team) = super::pty_layout::parse_team_key(key);
        team.filter(|_| !org.is_empty())
            .map(|team| (org.to_string(), team.to_string()))
    });
    let agent_key = admit_named_launch(
        &ctx.codesense_home,
        &ctx.principal(),
        &scope,
        &agent_key,
        target_team
            .as_ref()
            .map(|(org, team)| (org.as_str(), team.as_str())),
        Path::new(&cwd),
    )?;
    let initial_prompt = prompt.clone();
    let (home, principal) = (ctx.codesense_home.clone(), ctx.principal());
    let mut prepared = await_blocking("agent_launch_prepare.prepare", move || {
        launch::prepare_named(
            &home,
            &principal,
            &agent_key,
            PrepareOptions {
                target_team,
                mode_override: mode,
                prompt: prompt.as_deref().filter(|p| !p.trim().is_empty()),
                account: account_id.map(|id| crate::agents::resolve::ChosenAccount {
                    id,
                    generation: account_generation,
                }),
            },
        )
    })
    .await?;
    prepared.refuse_unusable_account()?;
    let agent_key = prepared.agent.key.to_string();
    // The mode the launch resolved to (the agent's own, or the override).
    let interactive = prepared.launch.mode == LaunchMode::Interactive;
    let launch_cwd = cwd.clone();
    // Refused before anything is written: the renderer cannot spawn a missing CLI.
    let binary = prepared.launch.binary.clone();
    let resolved_binary = super::mcp_workspace::resolve_agent_for_launch(&app, &binary).await?;
    let agent_path = resolved_binary.path.clone();
    prepared.note_cli_version(resolved_binary.version.as_deref());
    // The scope is projected against the account the spawn will run as, so a
    // managed account's credentials sit beside the scoped config.
    let (resolved, binding) =
        super::agent_accounts::resolve_for_launch(&app, prepared.account_input()).await?;
    if !resolved.spawn_eligible {
        return Err(launch::account_refusal(resolved.reason));
    }
    let account_home = super::agent_accounts::resolved_launch_binding_home(
        Some(&binding),
        crate::agents::adapter::provider_of(prepared.runtime),
    )?;
    let requires_spend_holds = prepared.requires_spend_holds()?;
    let task_inputs = crate::session_usage::task_capture::TaskInputs::from_launch(
        &prepared,
        initial_prompt.as_deref(),
    );
    let attribution_hook_trust = if interactive {
        await_blocking("agent_launch_prepare.host", {
            let app = app.clone();
            let session = session_id.clone();
            let runtime = prepared.runtime;
            move || super::pty_backend::attribution_hook_trust_for_spawn(&app, runtime, &session)
        })
        .await?
    } else {
        true
    };
    let (mut prepared, creation, settings) = prepare_product_session(
        prepared,
        agent_path,
        session_id.clone(),
        cwd,
        account_home,
        crate::agents::product_bootstrap::resources(&app),
        attribution_hook_trust,
    )
    .await?;
    // Attended Starts in the same directory share one flight. Local routine
    // wakes have their own identity and never adopt an untagged Start.
    let recorded = recorded_launch_dir(&prepared, &launch_cwd);
    let flight = if interactive {
        start_flight(&app, &agent_key, &recorded, new_instance.unwrap_or(false))?
    } else {
        StartFlight { claim: None }
    };
    let role = prepared.default_role.clone();
    if let Some(launch_team) = team
        .as_deref()
        .and_then(super::dashboard_launch::launch_team_from_key)
    {
        prepared
            .scope
            .extra_env
            .extend(super::role_startup::environment(
                &launch_team,
                role.as_deref(),
            ));
    }
    let launch_team = team
        .as_deref()
        .and_then(super::dashboard_launch::launch_team_from_key);
    prepare_role_startup(
        &super::agent_inbox::AppInboxHost { app: app.clone() },
        &mut prepared,
        launch_team.as_ref(),
        &session_id,
        initial_prompt.as_deref(),
    )
    .await?;
    register_prepared_usage(
        &mut prepared,
        &session_id,
        team.as_deref(),
        &recorded,
        app.try_state::<crate::http::HttpApiPort>()
            .map(|port| port.0),
        crate::session_usage::store_path(),
    )
    .await;
    let recorded_scope_env =
        bind_prepared_launch_environment(&app, &mut prepared, creation, requires_spend_holds);
    registry.stage_agent_launch(
        &session_id,
        StagedAgentLaunch {
            agent_revision: prepared.agent.clone(),
            queue_role: role,
            settings,
            capture_raw_terminal: prepared.launch.capture_raw_terminal,
            cwd: Some(recorded.clone()),
            facts: Some(
                crate::session_usage::record::LaunchFacts::new(
                    prepared.account.runtime,
                    resolved_binary.version.as_deref(),
                    &prepared.launch.args,
                    prepared.launch.mcp_allow.clone(),
                    prepared.launch.effective_bridge_tools.clone(),
                    prepared.agent.clone(),
                    prepared.manifest_hash.clone(),
                    &recorded.to_string_lossy(),
                )
                .with_launch_env(
                    prepared.account.runtime,
                    &prepared.launch.env,
                    &recorded_scope_env,
                )
                .with_provided_skills(prepared.provided_skills.clone())
                .with_task(task_inputs),
            ),
            binary: Some(resolved_binary),
        },
    );
    flight.launched(&session_id);
    Ok(prepared)
}

/// Register before spawn: the first export can arrive before the renderer binds a handle.
async fn register_prepared_usage(
    prepared: &mut PreparedAgentLaunch,
    session: &str,
    team_key: Option<&str>,
    cwd: &Path,
    api_port: Option<u16>,
    path: Result<std::path::PathBuf, String>,
) {
    prepared_usage_owner(prepared, session, team_key, cwd)
        .prepare_export(
            path,
            session,
            codesense_pty_core::agent_cli::descriptor(prepared.account.runtime)
                .telemetry
                .as_ref(),
            api_port,
            &mut prepared.scope.extra_env,
            &mut prepared.launch.args,
        )
        .await;
}

fn prepared_usage_owner(
    prepared: &PreparedAgentLaunch,
    session: &str,
    team_key: Option<&str>,
    cwd: &Path,
) -> crate::session_usage::LaunchOwner {
    let (org, team) = team_key
        .map(super::pty_layout::parse_team_key)
        .unwrap_or(("", None));
    crate::session_usage::LaunchOwner::new(
        (!org.is_empty()).then(|| org.to_string()),
        team.map(str::to_string),
        prepared.launch.id.clone(),
        session.to_string(),
        Some(cwd.to_string_lossy().into_owned()),
    )
}

/// The frontend starts exactly these arguments; its later bind never types startup facts.
pub(crate) async fn prepare_role_startup(
    host: &dyn super::agent_inbox::InboxHost,
    prepared: &mut PreparedAgentLaunch,
    team: Option<&super::dashboard_launch::LaunchTeam>,
    session: &str,
    initial_prompt: Option<&str>,
) -> Result<(), String> {
    let native_prompt = codesense_pty_core::agent_cli::descriptor(prepared.account.runtime)
        .prompt_flag
        .as_deref()
        .is_some_and(|flag| flag.ends_with('='));
    if (native_prompt && prepared.launch.mode != LaunchMode::Interactive)
        || (prepared.default_role.is_none() && !native_prompt)
    {
        return Ok(());
    }
    let prompt = initial_prompt
        .filter(|p| !p.trim().is_empty())
        .or(prepared.default_prompt.as_deref());
    let args = super::mcp_scope::compose_args(
        prepared.launch.args.clone(),
        prepared.scope.extra_args.clone(),
    );
    let plan = super::role_startup::start_argv(
        host,
        team,
        prepared.default_role.as_deref(),
        session,
        prepared.account.runtime,
        args,
        prompt,
        |plan| async { Ok(plan) },
    )
    .await?;
    prepared.launch.args = plan.launch_args;
    prepared.scope.extra_args.clear();
    prepared.default_prompt = None;
    prepared.prompt_in_argv = true;
    Ok(())
}

/// The dir a Start's session is recorded as running in (its launch
/// registry `worktree_dir`, and the dir its wake flight is keyed on): the
/// prepared product root, else the dir the Start asked for.
pub(crate) fn recorded_launch_dir(
    prepared: &PreparedAgentLaunch,
    launch_cwd: &str,
) -> std::path::PathBuf {
    std::path::PathBuf::from(prepared.cwd.as_deref().unwrap_or(launch_cwd))
}

/// The prefix of a Start refused because the agent is already starting in
/// that dir: the session to show follows it (empty while it is unbound).
pub const ALREADY_STARTING: &str = "already-starting:";

/// A person's Start of `agent` (its key) in `cwd`, claimed in the
/// wakes' single flight. Dropped without [`StartFlight::launched`], it frees
/// the claim (the launch failed).
struct StartFlight {
    claim: Option<(
        std::sync::Arc<crate::routines::wake_app::AppWakeEnv>,
        crate::routines::wake::FlightKey,
    )>,
}

impl StartFlight {
    // The existing flight policy without the Tauri handle, so each launch intent can
    // be checked against the real wake store at the same preparation boundary.
    fn claim_key(
        flights: &crate::routines::wake::LaunchFlights,
        key: crate::routines::wake::FlightKey,
        new_instance: bool,
        now: i64,
    ) -> Result<Option<crate::routines::wake::FlightKey>, String> {
        match flights.claim_start(&key, now) {
            Ok(()) => Ok(Some(key)),
            // Explicit New may bypass somebody else's flight, never own or replace it.
            Err(_) if new_instance => Ok(None),
            Err(session) => Err(format!(
                "{ALREADY_STARTING} {}",
                session.unwrap_or_default()
            )),
        }
    }

    fn launched(mut self, session_id: &str) {
        if let Some((env, key)) = self.claim.take() {
            use crate::routines::wake::WakeEnv;
            env.flights().finish(
                &key,
                Some((session_id.to_string(), session_id.to_string())),
                crate::routines::finish::now_ms(),
            );
        }
    }
}

impl Drop for StartFlight {
    fn drop(&mut self) {
        if let Some((env, key)) = self.claim.take() {
            use crate::routines::wake::WakeEnv;
            env.flights()
                .finish(&key, None, crate::routines::finish::now_ms());
        }
    }
}

fn start_flight(
    app: &AppHandle,
    agent: &str,
    cwd: &Path,
    new_instance: bool,
) -> Result<StartFlight, String> {
    use crate::routines::wake::WakeEnv;
    use tauri::Manager;
    let Some(env) = app.try_state::<std::sync::Arc<crate::routines::wake_app::AppWakeEnv>>() else {
        return Ok(StartFlight { claim: None });
    };
    let key = crate::routines::wake::flight_key(agent, cwd, crate::routines::wake::LOCAL);
    let claim = StartFlight::claim_key(
        env.flights(),
        key,
        new_instance,
        crate::routines::finish::now_ms(),
    )?;
    Ok(StartFlight {
        claim: claim.map(|key| (env.inner().clone(), key)),
    })
}

#[cfg(test)]
pub(crate) mod tests;
