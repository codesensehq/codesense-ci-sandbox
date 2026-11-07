// SPDX-License-Identifier: MIT
//! Launching a named agent: the one prepare step every agent launch calls, and
//! the one writer of the agent's generated launch files.
//!
//! [`prepare`] wraps [`super::resolve_agent`] and returns exactly what a launch
//! runs ([`AgentLaunch`]): the runtime's launch profile, this user's account,
//! the generated permission entries for the effective mode, the provenance to
//! record and the warnings to show. It reads and writes nothing else, so no
//! path re-implements resolution. Product launches apply the captured selected
//! manifest before spawning; other workspace kinds retain their existing behavior.
//!
//! [`apply_generated`] hands the generated launch settings over just before spawn,
//! the way the runtime's descriptor says it takes them:
//! - a settings flag (Claude `--settings <file>`): a fresh per-launch file outside
//!   every workspace and published tree, so two agents launched into one folder
//!   never see each other's rules. The returned guard owns the file; dropping it
//!   removes the file.
//! - config overrides (Codex `-c key=value`): one per key, in the launch recipe,
//!   so a relaunch of that recipe runs under the same keys.
//!
//! Step 2 (`WantSet::add_workspace_files`, SW-XS-8) writes the merge-marked files the
//! manifest built as generated entries into a managed surface, with
//! managed-record semantics; product bootstrap calls it under the destination
//! lock, before the MCP scope reads the workspace.
//!
//! The agent launches go through three spawn sites, each of which receives an
//! [`AgentLaunch`] and is where the lifecycle bootstrap will consume it, between
//! `prepare` and spawn:
//! - the managed spine (`mcp_workspace::launch_managed_dir_session` →
//!   `spawn_and_bind`), used by `open_workspace` and trigger launches;
//! - `pty::spawn_from_args`, the frontend interactive launch (wired to agents
//!   separately, through the same two calls);
//! - `dashboard_launch::launch_headless_session`, the PTY-less run.
//!
//! CodeSense checks nothing at runtime: the CLI enforces what is written, a
//! product startup errors block spawning, while advisory warnings are returned.

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::agents::adapter::{Attribution, LaunchMode, ResolvedLaunchProfile};
use crate::commands::agent_accounts::ResolveAgentAccountInput;
use crate::commands::dashboard_launch::RunProvenance;
use crate::commands::mcp_scope::{self, ScopeOutcome};
use crate::commands::paths::ensure_private_dir;
use crate::domain::Principal;
use crate::install::agent_account_store::{AccountResolutionReason, ResolvedAgentAccount};
use codesense_pty_core::agent_cli::{
    find_settings_flag, Descriptor, LaunchSettings, SettingsFlagError,
};

use super::keys::AgentKey;
use super::launch_check::{self, LaunchWarning};
use super::manifest::{AgentRevisionRef, GeneratedEntry, GeneratedTarget};
use super::model;
use super::permissions;
use super::resolve::{self, ResolveOptions, ResolvedAccountPolicy};
use super::service;
use codesense_pty_core::agent_cli::AgentCli;

/// Minimum age for launch settings cleanup; session liveness still gates removal
/// because the CLI can outlive the desktop app.
const SETTINGS_STALE_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

/// How to prepare.
#[derive(Debug, Clone, Default)]
pub struct PrepareOptions<'a> {
    /// The `(org, team)` the launch runs against; `None` is the agent's own team.
    pub target_team: Option<(String, String)>,
    /// The mode this launch runs in when the launcher or routine overrides the
    /// agent's declared one.
    pub mode_override: Option<LaunchMode>,
    /// The prompt the launch starts with, checked for warnings. `None` checks
    /// the agent's own default prompt.
    pub prompt: Option<&'a str>,
    /// An account for this launch only, in place of the user's binding, which
    /// stays as it is.
    pub account: Option<resolve::ChosenAccount>,
}

/// Everything one launch of one agent by one user runs with.
#[derive(Debug, Clone)]
pub struct AgentLaunch {
    /// The runtime's launch profile with the agent's identity (`id` is the
    /// agent key), args, MCP allowlist and the effective mode.
    pub launch: ResolvedLaunchProfile,
    pub runtime: AgentCli,
    /// The runtime descriptor's account policy.
    pub account_policy: ResolvedAccountPolicy,
    /// This user's bound account, passed as the launch's explicit override.
    pub account_override: Option<String>,
    /// What the account resolver picked when this launch was prepared. Its
    /// generation is the one the launch asks for.
    pub account_preview: ResolvedAgentAccount,
    /// Why the account store could not be read, when it could not.
    pub account_store_error: Option<String>,
    pub agent: AgentRevisionRef,
    /// The manifest hash, covering the generated entries below.
    pub manifest_hash: String,
    /// Exact resolved startup and authority captured before asynchronous launch work.
    pub manifest: super::manifest::StartupManifest,
    pub startup_request_id: uuid::Uuid,
    pub startup_context: super::product_bootstrap::LaunchContext,
    /// How many manifest errors there are. Recorded, never blocking here.
    pub manifest_errors: usize,
    /// The generated permission entries for the effective mode.
    pub generated: Vec<GeneratedEntry>,
    pub warnings: Vec<LaunchWarning>,
    /// The agent's own first prompt, for a launch that has none of its own.
    pub default_prompt: Option<String>,
    /// Effective queue membership requested for this prepared launch.
    pub queue_role: Option<String>,
    /// The agent's display name, which its terminal tab is titled with when the
    /// launcher gives no title of its own.
    pub name: String,
    /// The release of the CLI this launch spawns, once its binary is resolved.
    pub cli_version: Option<String>,
}

impl AgentLaunch {
    /// The skill names this launch's startup manifest provides to its CLI.
    pub fn provided_skills(&self) -> Vec<String> {
        super::provides::derive(self.runtime, &self.manifest).skills
    }

    /// Records the release of the CLI this launch spawns, and warns when it is outside the
    /// range CodeSense is qualified on.
    pub fn note_cli_version(&mut self, version: Option<&str>) {
        self.cli_version = version.map(str::to_owned);
        self.warnings
            .extend(launch_check::cli_version_warning(self.runtime, version));
    }

    /// `"<key>@<revision>"`, the provenance every agent launch records.
    pub fn revision_label(&self) -> String {
        self.agent.to_string()
    }

    /// The built-in descriptor the agent runs on.
    pub fn descriptor(&self) -> &'static Descriptor {
        codesense_pty_core::agent_cli::descriptor(self.runtime)
    }

    /// The account intent the launch-time resolver applies: the runtime's own
    /// policy, this user's binding as the explicit override, at the generation
    /// the preview used.
    pub fn account_input(&self) -> ResolveAgentAccountInput {
        ResolveAgentAccountInput {
            trusted_policy: Some(self.account_policy.clone()),
            override_account_generation: self
                .account_override
                .as_ref()
                .and(self.account_preview.generation),
            override_account_id: self.account_override.clone(),
            use_system_default: false,
            runtime: Some(self.runtime),
        }
    }

    /// Read the current cap even when this launch pinned an older agent revision.
    pub fn refuse_over_budget(&self) -> Result<(), String> {
        crate::session_usage::spend_caps::refuse_agent(
            &self.startup_context.home,
            &self.startup_context.principal,
            &self.agent.key,
        )
    }

    pub(crate) fn requires_spend_holds(&self) -> Result<bool, String> {
        let agent = super::service::get(
            &self.startup_context.home,
            &self.startup_context.principal,
            &self.agent.key,
        )
        .map_err(|e| e.to_string())?
        .agent;
        Ok(crate::session_usage::spend_caps::agent_status(
            &self.startup_context.home,
            &self.agent.key.to_string(),
            &agent,
        )?
        .effective
            != Default::default())
    }

    /// Refuse before spawn when the preview already says the account cannot
    /// run it (a binding to another provider's account, a detached account, an
    /// unreadable store), with the preview's reason.
    pub fn refuse_unusable_account(&self) -> Result<(), String> {
        self.refuse_over_budget()?;
        self.refuse_launch_prerequisites()?;
        if let Some(error) = &self.account_store_error {
            return Err(format!("agent account is unavailable: {error}"));
        }
        if self.account_preview.spawn_eligible {
            return Ok(());
        }
        Err(account_refusal(self.account_preview.reason))
    }

    pub fn refuse_launch_prerequisites(&self) -> Result<(), String> {
        let admission = super::adapter::launch_admission(
            self.runtime,
            &self.startup_context.home,
            &self.startup_context.principal,
            &self.manifest,
            &self.launch.args,
            &super::content_reader::FsContentReader,
            None,
        );
        match admission.reason {
            Some(reason) => Err(reason),
            None => Ok(()),
        }
    }

    /// `base` (or, with none, a fresh run under `handle`) with this launch's
    /// agent revision and manifest hash on it.
    pub fn provenance(&self, base: Option<RunProvenance>, handle: &str) -> RunProvenance {
        let mut provenance = base.unwrap_or_else(|| RunProvenance {
            run_id: format!("agent-launch:{}", uuid::Uuid::new_v4()),
            story_id: handle.to_string(),
            ..Default::default()
        });
        provenance.agent = Some(self.revision_label());
        provenance.manifest_hash = Some(self.manifest_hash.clone());
        provenance
    }
}

/// The refusal for an account the resolver says cannot run a launch, spelled
/// with the resolver's wire reason (`provider-mismatch`, `detached`, ...).
pub(crate) fn account_refusal(reason: AccountResolutionReason) -> String {
    let reason = serde_json::to_value(reason)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{reason:?}"));
    format!("agent account is unavailable: {reason}")
}

/// Prepare `key` for a launch by `principal`. The only error is an agent that
/// cannot be identified; everything else is data on the result.
pub fn prepare(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    options: PrepareOptions<'_>,
) -> Result<AgentLaunch, String> {
    prepare_with(
        &super::content_reader::FsContentReader,
        home,
        principal,
        key,
        options,
    )
}

pub(crate) fn prepare_with(
    reader: &dyn super::content_reader::ContentReader,
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    options: PrepareOptions<'_>,
) -> Result<AgentLaunch, String> {
    check_personal_org(
        home,
        principal,
        key,
        options
            .target_team
            .as_ref()
            .map(|(org, team)| (org.as_str(), team.as_str())),
    )?;
    let (agent, resolved) = resolve::resolve_pinned_with(
        reader,
        home,
        principal,
        key,
        ResolveOptions {
            target_team: options.target_team.clone(),
        },
    )
    .map_err(|e| e.to_string())?;

    prepare_resolved(reader, home, principal, key, options, agent, resolved)
}

fn prepare_resolved(
    reader: &dyn super::content_reader::ContentReader,
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    options: PrepareOptions<'_>,
    agent: model::Agent,
    resolved: resolve::ResolvedAgent,
) -> Result<AgentLaunch, String> {
    let admission = super::adapter::launch_admission(
        agent.runtime,
        home,
        principal,
        &resolved.manifest,
        &resolved.launch.args,
        reader,
        options.prompt.or(Some(agent.default_prompt.as_str())),
    );
    if !admission.allowed {
        return Err(admission.reason.unwrap_or_default());
    }
    let declared = match agent.launch {
        model::LaunchMode::Foreground => LaunchMode::Interactive,
        model::LaunchMode::Headless => LaunchMode::Headless,
    };
    let mode = options.mode_override.unwrap_or(declared);
    let mut launch = resolved.launch.clone();
    launch.mode = mode;
    launch.capture_raw_terminal = resolve::raw_capture(&resolved.account, mode);

    let mut resolved = resolved;
    if let Some(chosen) = &options.account {
        let consent = resolved.account.capture_raw_terminal;
        resolved.account = resolve::chosen_account(home, principal, key, agent.runtime, chosen);
        resolved.account.capture_raw_terminal = consent;
    }
    if mode != declared {
        let effective = match mode {
            LaunchMode::Interactive => model::LaunchMode::Foreground,
            LaunchMode::Headless => model::LaunchMode::Headless,
        };
        // The permission entries follow the effective mode; the merged
        // workspace files (SW-XS-14) do not depend on it and are kept, after.
        let workspace_files: Vec<GeneratedEntry> = resolved
            .manifest
            .generated
            .iter()
            .filter(|g| g.target == GeneratedTarget::WorkspaceFile)
            .cloned()
            .collect();
        resolved.manifest.generated = permissions::generate(&agent, agent.runtime, effective);
        resolved.manifest.generated.extend(workspace_files);
        resolved.manifest.manifest_hash = resolved.manifest.compute_hash();
    }
    // Checked against what this launch actually writes and starts with.
    let prompt = options
        .prompt
        .or(Some(agent.default_prompt.as_str()))
        .filter(|p| !p.trim().is_empty());
    let mut warnings = launch_check::warnings(&resolved, prompt);
    warnings.extend(launch_check::usage_not_recorded_warning(&agent));
    let manifest = resolved.manifest;

    let prepared = AgentLaunch {
        launch,
        runtime: agent.runtime,
        account_policy: resolved.account.policy.clone(),
        account_override: resolved.account.override_account_id.clone(),
        account_preview: resolved.account.preview.clone(),
        account_store_error: resolved.account.store_error.clone(),
        agent: resolved.agent.clone(),
        manifest_hash: manifest.manifest_hash.clone(),
        manifest_errors: manifest.errors.len(),
        generated: manifest.generated.clone(),
        startup_context: super::product_bootstrap::LaunchContext {
            home: home.to_path_buf(),
            principal: principal.clone(),
            target: super::ManifestTarget::for_launch(
                key,
                options
                    .target_team
                    .map(|(org, team)| super::ManifestTarget::team(org, team)),
            ),
        },
        manifest,
        startup_request_id: uuid::Uuid::new_v4(),
        warnings,
        default_prompt: Some(
            if codesense_pty_core::agent_cli::descriptor(agent.runtime)
                .prompt_flag
                .as_deref()
                .is_some_and(|flag| flag.ends_with('='))
            {
                agent.default_prompt.clone()
            } else {
                agent.default_prompt.trim().to_string()
            },
        )
        .filter(|p| !p.trim().is_empty()),
        queue_role: crate::queue_roles::effective_role(agent.role.as_deref(), None)?,
        name: agent.name.clone(),
        cli_version: None,
    };
    prepared.refuse_over_budget()?;
    Ok(prepared)
}

/// SLW-PT-19: a personal agent is filed under one org's scratch space and
/// launches only into that org — never into another org's workspace.
/// SLW-PT-20: nor into a team of it that has "Use my scratch items" off.
pub(crate) fn check_personal_org(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    target_team: Option<(&str, &str)>,
) -> Result<(), String> {
    match target_team {
        Some((org, _)) if key.is_personal() && key.org != org => Err(format!(
            "agent {key} is an Only me agent of {}; it cannot launch in {org}",
            key.org
        )),
        Some((org, team))
            if key.is_personal()
                && !crate::context_repo::scratch_space::team_uses_scratch(
                    home, principal, org, team,
                ) =>
        {
            Err(format!(
                "agent {key} is one of your private items, which are switched off in {org}/{team}"
            ))
        }
        _ => Ok(()),
    }
}

/// The agent a launcher named: a rendered key as is (archived or not), or a
/// bare id looked up in the target team first, then the caller's personal
/// space, then the starters (`claude`, `codex`), skipping archived agents.
pub fn find_agent(
    home: &Path,
    principal: &Principal,
    reference: &str,
    target_team: Option<(&str, &str)>,
) -> Result<AgentKey, String> {
    find_agent_for(home, principal, reference, target_team, &Default::default())
}

pub(crate) fn find_agent_for(
    home: &Path,
    principal: &Principal,
    reference: &str,
    target_team: Option<(&str, &str)>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<AgentKey, String> {
    let reference = reference.trim();
    if reference.contains('/') {
        let key = AgentKey::parse(reference)
            .ok_or_else(|| format!("{reference:?} is not an agent key or id"))?;
        if !scope.definition(home, principal, &key) {
            return Err(crate::context_repo::caller_scope::MISSING.into());
        }
        check_personal_org(home, principal, &key, target_team)?;
        return Ok(key);
    }
    if !model::is_valid_id(reference) {
        return Err(format!("{reference:?} is not an agent key or id"));
    }
    let team = target_team.map(|(org, team)| AgentKey::scoped(principal, org, team, reference));
    // A personal agent is the one filed under the target's org: another
    // org's scratch agent never answers for this one.
    // SLW-PT-20: never from a team with "Use my scratch items" off.
    let personal = target_team
        .filter(|(org, team)| {
            crate::context_repo::scratch_space::team_uses_scratch(home, principal, org, team)
        })
        .map(|(org, _)| AgentKey::personal(principal, org, reference));
    let starter = Some(AgentKey::builtin(reference)).filter(super::builtin::is_starter);
    let denied_team = team
        .as_ref()
        .is_some_and(|key| !scope.definition(home, principal, key));
    team.into_iter()
        .chain(personal)
        .chain(starter)
        .filter(|key| scope.definition(home, principal, key))
        .find_map(|key| match service::read_current(home, principal, &key) {
            // An archived agent is out of the lookup: a bare id names the next
            // place's agent (a key still names an archived one).
            Ok(agent) if agent.is_archived() => None,
            Ok(_) => Some(Ok(key)),
            // Only an absent team agent falls through to the personal space; an unreadable
            // one is reported, never silently replaced by a same-named personal agent.
            Err(service::AgentError::NotFound(_)) => None,
            Err(error) => Some(Err(format!("agent {key} cannot be read: {error}"))),
        })
        .unwrap_or_else(|| {
            if denied_team {
                return Err(crate::context_repo::caller_scope::MISSING.into());
            }
            Err(match target_team {
                Some((org, team)) => {
                    format!("no agent {reference:?} in {org}/{team} or your private space")
                }
                None => format!("no agent {reference:?} in your private space"),
            })
        })
}

/// [`find_agent`], then [`prepare`] it.
pub fn prepare_named(
    home: &Path,
    principal: &Principal,
    reference: &str,
    options: PrepareOptions<'_>,
) -> Result<AgentLaunch, String> {
    let team = options
        .target_team
        .as_ref()
        .map(|(org, team)| (org.as_str(), team.as_str()));
    let key = find_agent(home, principal, reference, team)?;
    prepare(home, principal, &key, options)
}

/// What [`apply_generated`] wrote for one launch.
#[derive(Debug, Default)]
pub struct AppliedLaunch {
    /// The per-launch settings file, when one was written.
    pub settings: Option<LaunchSettingsFile>,
    /// Whether the saved Agent's own settings flag was folded into that file, so its
    /// args must not carry a second one.
    pub merged_agent_settings: bool,
    /// Non-secret provider permission flags retained in the launch recipe.
    pub extra_args: Vec<String>,
    /// Settings delivered directly to the CLI's environment.
    pub extra_env: Vec<(String, String)>,
}

impl AppliedLaunch {
    /// Remove the saved Agent's own settings flag from `args` when it was folded into
    /// the launch's settings file: the CLI takes only the last one, so a second one
    /// would replace the merged file.
    pub fn drop_agent_settings(&self, args: &mut Vec<String>) {
        let Some(flag) = self
            .settings
            .as_ref()
            .filter(|_| self.merged_agent_settings)
            .map(LaunchSettingsFile::flag)
        else {
            return;
        };
        // The agent's own args lead `args`, so the first settings flag is the agent's,
        // whatever follows it.
        let first =
            (0..args.len()).find_map(|end| find_settings_flag(&args[..=end], flag).ok().flatten());
        if let Some(given) = first {
            args.drain(given.range);
        }
    }

    /// The argv to append for what was written, with each path shaped by
    /// `quote_path` for the caller's launch builder (a shell-joined line quotes
    /// it, a direct argv passes it raw).
    pub fn argv(&self, quote_path: impl Fn(&str) -> String) -> Vec<String> {
        let mut args = match &self.settings {
            Some(file) => vec![
                file.flag().to_owned(),
                quote_path(&file.path().to_string_lossy()),
            ],
            None => Vec::new(),
        };
        args.extend(self.extra_args.iter().map(|arg| quote_path(arg)));
        args
    }
}

/// A per-launch settings file and the flag that names it on the launch line. Dropping
/// it removes the file, so it lives exactly as long as whatever holds it (the session's
/// launch entry).
pub struct LaunchSettingsFile {
    path: PathBuf,
    flag: String,
}

impl LaunchSettingsFile {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The flag that names the file on the launch line.
    pub fn flag(&self) -> &str {
        &self.flag
    }
}

#[cfg(test)]
impl LaunchSettingsFile {
    /// Own an existing file as if a launch had written it.
    pub(crate) fn adopt(path: PathBuf) -> Self {
        Self {
            path,
            flag: "--settings".into(),
        }
    }
}

impl fmt::Debug for LaunchSettingsFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("LaunchSettingsFile")
            .field(&self.path)
            .finish()
    }
}

impl Drop for LaunchSettingsFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Hand over `launch`'s generated launch settings for the launch of session
/// `session_id` in `cwd`, the way its runtime's descriptor takes them: a per-launch
/// settings file, or config overrides kept in the launch recipe. Merged workspace files
/// are written by Step 2 (`WantSet::add_workspace_files`).
pub fn apply_generated(
    launch: &AgentLaunch,
    session_id: &str,
    cwd: &Path,
    attribution_hook_trust: bool,
) -> Result<AppliedLaunch, String> {
    let mut applied = AppliedLaunch::default();
    let descriptor = codesense_pty_core::agent_cli::descriptor(launch.runtime);
    // Line-attribution capture for a managed worktree. It never fails the launch: a git-ai
    // that is missing or broken comes back as a status with nothing wired.
    let attribution_request = crate::attribution::launch::Request {
        codesense_home: &launch.startup_context.home,
        principal: &launch.startup_context.principal,
        cwd,
        runtime: launch.runtime,
    };
    let mut attribution = crate::attribution::launch::prepare_for_host(
        &attribution_request,
        std::env::var_os("PATH").as_deref(),
        attribution_hook_trust,
    );
    let settings_entries = launch
        .generated
        .iter()
        .filter(|entry| entry.target == GeneratedTarget::LaunchSettings)
        .count();
    if settings_entries > 1 {
        return Err(format!(
            "{} has {settings_entries} launch settings entries; a launch takes one",
            launch.revision_label()
        ));
    }
    for entry in &launch.generated {
        match entry.target {
            GeneratedTarget::LaunchSettings => match &descriptor.launch_settings {
                LaunchSettings::Flag(flag) => {
                    let mut content = entry.content.clone();
                    if launch
                        .manifest
                        .essentials
                        .iter()
                        .any(|essential| essential.id == "codesense-notify-settings")
                    {
                        crate::commands::content_engine::merge_required_launch_notify_hooks(
                            &mut content,
                            cwd,
                            launch.runtime,
                        )?;
                    } else {
                        crate::commands::content_engine::merge_launch_notify_hooks(
                            &mut content,
                            cwd,
                            launch.runtime,
                        );
                    }
                    if let Some(agent) = agent_settings(&launch.launch.args, flag)? {
                        content = merge_agent_settings(content, &agent, flag, cwd)?;
                        applied.merged_agent_settings = true;
                    }
                    crate::commands::content_engine::reconcile_attribution_hooks(
                        &mut content,
                        attribution.hook_command.as_deref(),
                    )
                    .ok_or("attribution hooks cannot be reconciled in launch settings")?;
                    applied.settings = Some(write_settings_file(
                        &launch_settings_root()?,
                        cwd,
                        session_id,
                        &content,
                        flag,
                    )?);
                }
                overrides @ LaunchSettings::Overrides(_) => {
                    // The keys a saved Agent's startup may carry, and nothing else.
                    let rules = &descriptor.startup;
                    let keys = entry
                        .content
                        .as_object()
                        .ok_or_else(|| format!("invalid generated {} permissions", rules.name))?;
                    if !rules.admits(keys) {
                        return Err(format!(
                            "unsupported generated {} permission key",
                            rules.name
                        ));
                    }
                    applied.extra_args = overrides
                        .overrides(&entry.content)
                        .ok_or_else(|| format!("invalid generated {} permissions", rules.name))?;
                }
                LaunchSettings::HomeConfig { file } => {
                    let home = mcp_scope::mcp_scope_session_dir(session_id);
                    let path = home.join(file);
                    let mut config: serde_json::Value =
                        serde_yaml::from_slice(&std::fs::read(&path).map_err(|e| e.to_string())?)
                            .map_err(|e| e.to_string())?;
                    if let Some(project) = super::adapter::desktop(launch.runtime).home_config {
                        project(launch, cwd, &mut config)?;
                    }
                    let staged = home.join(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
                    write_new_private(
                        &staged,
                        serde_yaml::to_string(&config)
                            .map_err(|e| e.to_string())?
                            .as_bytes(),
                    )?;
                    std::fs::rename(&staged, &path).map_err(|e| e.to_string())?;
                }
                LaunchSettings::Env(env_var) => {
                    let content = entry
                        .content
                        .as_object()
                        .ok_or_else(|| "generated launch settings must be an object".to_string())?;
                    applied.extra_env.push((
                        env_var.clone(),
                        serde_json::Value::Object(content.clone()).to_string(),
                    ));
                }
            },
            // Written into the workspace by Step 2 (`WantSet::add_workspace_files`).
            GeneratedTarget::WorkspaceFile => {}
        }
    }
    let argv_hook = attribution
        .hook_command
        .clone()
        .filter(|_| super::adapter::desktop(launch.runtime).attribution == Attribution::ArgvHooks);
    if let Some(command) = argv_hook {
        match codesense_pty_core::agent_cli::attribution_hook_flags(
            &command,
            &crate::attribution::launch::CODEX_HOOK_EVENTS,
            crate::attribution::launch::HOOK_TIMEOUT,
            &launch.launch.args,
        ) {
            Ok(flags) => applied.extra_args.extend(flags),
            // The agent's own hooks would make the launch refuse to start; capture waits.
            Err(reason) => attribution = attribution.unwired(&attribution_request, reason),
        }
    }
    applied.extra_env.extend(attribution.env);
    applied
        .extra_env
        .extend((super::adapter::desktop(launch.runtime).managed_launch_env)()?);
    if !applied.extra_env.is_empty() {
        remember_launch_env(session_id, &applied.extra_env);
    }
    Ok(applied)
}

/// Reconcile the recorded recipe with the same fresh plan used by an initial launch.
/// Only owned checkpoint hooks change; permissions, user hooks and other launch args stay.
#[allow(clippy::type_complexity)]
pub(crate) fn refresh_attribution(
    request: &crate::attribution::launch::Request<'_>,
    session_id: &str,
    arguments: &[String],
    attribution_hook_trust: bool,
) -> Result<
    (
        Option<codesense_pty_core::agent_cli::AttributionHookEdit>,
        Vec<(String, String)>,
    ),
    String,
> {
    let mut plan = crate::attribution::launch::prepare_for_host(
        request,
        std::env::var_os("PATH").as_deref(),
        attribution_hook_trust,
    );
    let refreshed = match super::adapter::desktop(request.runtime).attribution {
        Attribution::SettingsHooks => {
            if let Some(given) = find_settings_flag(arguments, "--settings")
                .map_err(|e| format!("invalid relaunch settings: {e:?}"))?
            {
                let path = Path::new(&given.value);
                if settings_file_session(path).as_deref() != Some(session_id) {
                    return Err("relaunch requires its CodeSense-owned settings file; start the agent again".into());
                }
                let mut settings: serde_json::Value =
                    crate::shared::json_io::read_json(path).map_err(|e| e.to_string())?;
                crate::commands::content_engine::reconcile_attribution_hooks(
                    &mut settings,
                    plan.hook_command.as_deref(),
                )
                .ok_or("attribution hooks cannot be reconciled in the relaunch settings")?;
                // Reuse the private-settings writer so replacement remains 0600. The old
                // session lease continues to own the original path after this atomic rename.
                let replacement = write_settings_file(
                    &launch_settings_root()?,
                    request.cwd,
                    session_id,
                    &settings,
                    "--settings",
                )?;
                std::fs::rename(LaunchSettingsFile::path(&replacement), path)
                    .map_err(|e| e.to_string())?;
            } else if plan.hook_command.is_some() {
                plan = plan.unwired(
                    request,
                    "the relaunch has no CodeSense-owned Claude settings file",
                );
            }
            None
        }
        Attribution::ArgvHooks => {
            let args = match codesense_pty_core::agent_cli::attribution_hook_edit(
                arguments,
                plan.hook_command.as_deref(),
                &crate::attribution::launch::CODEX_HOOK_EVENTS,
                crate::attribution::launch::HOOK_TIMEOUT,
            ) {
                Ok(args) => args,
                Err(reason) => {
                    plan = plan.unwired(request, reason);
                    codesense_pty_core::agent_cli::attribution_hook_edit(
                        arguments,
                        None,
                        &crate::attribution::launch::CODEX_HOOK_EVENTS,
                        crate::attribution::launch::HOOK_TIMEOUT,
                    )
                    .map_err(str::to_string)?
                }
            };
            Some(args)
        }
        Attribution::Unsupported(_) => None,
    };
    Ok((refreshed, plan.env))
}

/// The generated environment of each live saved-Agent launch, by session. It is not part of
/// the recorded launch line, so a relaunch in the ended tab takes it from here.
static LAUNCH_ENV: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, Vec<(String, String)>>>,
> = std::sync::LazyLock::new(Default::default);

fn remember_launch_env(session_id: &str, env: &[(String, String)]) {
    LAUNCH_ENV
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(session_id.to_string(), env.to_vec());
}

/// The generated environment `session_id` was launched with, if this run launched it.
pub(crate) fn remembered_launch_env(session_id: &str) -> Option<Vec<(String, String)>> {
    LAUNCH_ENV
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(session_id)
        .cloned()
}

/// Drop `session_id`'s remembered environment when its tab is closed for good.
pub(crate) fn forget_launch_env(session_id: &str) {
    LAUNCH_ENV
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(session_id);
    HOME_LAUNCHES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(session_id);
}

// Homes whose settings are files must be regenerated on a cached respawn too.
static HOME_LAUNCHES: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, AgentLaunch>>,
> = std::sync::LazyLock::new(Default::default);

pub(crate) fn remembered_home_launch(session_id: &str) -> Option<AgentLaunch> {
    HOME_LAUNCHES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(session_id)
        .cloned()
}

/// The saved Agent's own settings `flag` and its value (inline JSON, or a file path). Two
/// of them are refused, since the CLI would keep only the last.
fn agent_settings(args: &[String], flag: &str) -> Result<Option<String>, String> {
    match find_settings_flag(args, flag) {
        Ok(given) => Ok(given.map(|given| given.value)),
        Err(SettingsFlagError::NoValue) => Err(format!("the saved Agent's {flag} has no value")),
        Err(SettingsFlagError::Repeated) => Err(format!("a saved Agent may pass one {flag}")),
    }
}

/// The generated settings with the saved Agent's own settings (passed with `flag`)
/// folded in. Objects merge key by key, arrays union, and CodeSense's scalars win, so the
/// agent's keys survive and CodeSense's allows and hooks are kept. A relative file path
/// resolves against `cwd`, the directory the CLI runs in.
fn merge_agent_settings(
    generated: serde_json::Value,
    agent: &str,
    flag: &str,
    cwd: &Path,
) -> Result<serde_json::Value, String> {
    let mut merged: serde_json::Value = if agent.trim_start().starts_with('{') {
        serde_json::from_str(agent)
            .map_err(|error| format!("the saved Agent's {flag} is not JSON: {error}"))?
    } else {
        let path = cwd.join(agent);
        let text = std::fs::read_to_string(&path).map_err(|error| {
            format!(
                "the saved Agent's {flag} file {} cannot be read: {error}",
                path.display()
            )
        })?;
        serde_json::from_str(&text).map_err(|error| {
            format!(
                "the saved Agent's {flag} file {} is not JSON: {error}",
                path.display()
            )
        })?
    };
    if !merged.is_object() {
        return Err(format!("the saved Agent's {flag} must be a JSON object"));
    }
    merge_json(&mut merged, generated);
    Ok(merged)
}

fn merge_json(base: &mut serde_json::Value, over: serde_json::Value) {
    use serde_json::{map::Entry, Value};
    match (base, over) {
        (Value::Object(base), Value::Object(over)) => {
            for (key, value) in over {
                match base.entry(key) {
                    Entry::Occupied(mut existing) => merge_json(existing.get_mut(), value),
                    Entry::Vacant(slot) => {
                        slot.insert(value);
                    }
                }
            }
        }
        (Value::Array(base), Value::Array(over)) => {
            for value in over {
                if !base.contains(&value) {
                    base.push(value);
                }
            }
        }
        (base, over) => *base = over,
    }
}

/// Project the agent's MCP scope for `session_id` (as every scoped launch does), then
/// write its generated entries into that scope and beside it. `quote_path` shapes paths
/// for the caller's launch builder. A failure leaves no scope or settings file behind.
#[cfg(test)]
pub(crate) fn project_and_apply(
    launch: &AgentLaunch,
    session_id: &str,
    cwd: &Path,
    account_home: Option<&Path>,
    quote_path: impl Fn(&str) -> String + Send,
) -> Result<
    (
        ScopeOutcome,
        Option<crate::commands::mcp_scope::ScopeCreation>,
        AppliedLaunch,
    ),
    String,
> {
    let fixture = mcp_scope::inventory::Fixture::new();
    let cli = fixture.launch(&launch.launch.args);
    std::thread::scope(|threads| {
        threads
            .spawn(move || {
                project_and_apply_for_cli(
                    launch,
                    session_id,
                    cwd,
                    account_home,
                    Some(&cli),
                    quote_path,
                )
            })
            .join()
            .expect("scope fixture thread")
    })
}

pub(crate) fn project_and_apply_for_cli(
    launch: &AgentLaunch,
    session_id: &str,
    cwd: &Path,
    account_home: Option<&Path>,
    cli: Option<&mcp_scope::LaunchCli<'_>>,
    quote_path: impl Fn(&str) -> String,
) -> Result<
    (
        ScopeOutcome,
        Option<mcp_scope::ScopeCreation>,
        AppliedLaunch,
    ),
    String,
> {
    launch.refuse_launch_prerequisites()?;
    let allow = launch.launch.mcp_allow.clone().unwrap_or_default();
    // SW-AN-52: resolve the content-layer MCP config once per launch and thread
    // it to the projection step. The cwd `.mcp.json` deep-merges on top in both
    // the Claude and Codex paths — a server defined in a team's content-layer
    // config now reaches the agent instead of silently dropping.
    //
    // Fallback on `Err` (bytes-limit, parse, I/O): log and pass `None` so the
    // launch keeps today's cwd-only behavior rather than refusing. The user
    // sees the reason via `LaunchWarning::McpConfigUnresolvable`, emitted by
    // `agents::resolve::compose` and surfaced on `ResolvedAgent.warnings`.
    let canonical_base = match resolve::effective_mcp_config(
        &launch.startup_context.home,
        &launch.startup_context.principal,
        launch.runtime,
        &launch.startup_context.target,
    ) {
        Ok(value) => Some(value),
        Err(reason) => {
            log::warn!(
                target: "mcp_scope",
                "content-layer MCP config unresolvable for launch ({reason}); falling back to cwd .mcp.json alone"
            );
            None
        }
    };
    let mut selected_cli = cli.cloned();
    if let Some(selected) = &mut selected_cli {
        selected.runtime = Some(launch.runtime);
    }
    let (mut scope, creation) = mcp_scope::project_scope_for_cli(
        session_id,
        cwd,
        &allow,
        launch
            .launch
            .bridge_tools
            .as_ref()
            .map(|_| launch.launch.effective_bridge_tools.as_slice())
            .or_else(|| {
                super::adapter::desktop(launch.runtime)
                    .home_config
                    .map(|_| launch.launch.effective_bridge_tools.as_slice())
            }),
        launch.launch.mcp_scope.as_ref(),
        account_home,
        canonical_base.as_ref(),
        selected_cli.as_ref(),
        &quote_path,
    )?;
    let mut preparation = || {
        if let Some(seed) = super::adapter::desktop(launch.runtime).seed_home {
            let selected = selected_cli
                .as_ref()
                .ok_or("managed runtime requires its exact selected executable")?;
            let home_env = &launch.descriptor().home_env;
            let home = Iterator::find(&mut scope.extra_env.iter(), |(key, _)| key == home_env)
                .map(|(_, value)| PathBuf::from(value))
                .ok_or("managed runtime has no scoped home")?;
            let extra = seed(&home, selected.binary)?;
            mcp_scope::extend_launch_env(launch.runtime, &mut scope.extra_env, extra);
        }
        apply_generated(
            launch,
            session_id,
            cwd,
            cli.is_none_or(|selected| selected.attribution_hook_trust),
        )
    };
    let result = if super::adapter::desktop(launch.runtime).seed_home.is_some() {
        mcp_scope::prepare_created_scope(
            creation
                .as_ref()
                .ok_or("managed runtime has no scope creation")?,
            preparation,
        )
    } else {
        preparation()
    };
    let applied = match result {
        Ok(applied) => applied,
        Err(error) => {
            mcp_scope::remove_scope_config(session_id);
            return Err(error);
        }
    };
    // A runtime whose launch settings share its scope's env-content variable
    // (OpenCode's `OPENCODE_CONFIG_CONTENT`) has them folded into that one entry
    // here, so a spawn site that appends the settings env can never clobber the
    // scope's plugin and MCP config. Every spawn site consumes this merged
    // result, so the merge has exactly one source.
    mcp_scope::extend_launch_env(
        launch.runtime,
        &mut scope.extra_env,
        applied.extra_env.iter().cloned(),
    );
    if super::adapter::desktop(launch.runtime).seed_home.is_some() {
        HOME_LAUNCHES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_owned(), launch.clone());
    }
    Ok((scope, creation, applied))
}

fn launch_settings_root() -> Result<PathBuf, String> {
    let home = crate::commands::paths::codesense_home_dir()
        .ok_or("home directory unavailable for launch settings")?;
    ensure_private_dir(&home)?;
    Ok(home.join("launch-settings"))
}

/// Separates the session id from the per-file nonce in a settings file name.
const SESSION_SEPARATOR: &str = "--";

/// The session a settings file was written for, from its name
/// (`<session>--<nonce>.json`).
fn settings_file_session(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let (session, _) = stem.rsplit_once(SESSION_SEPARATOR)?;
    (!session.is_empty()).then(|| session.to_string())
}

fn write_settings_file(
    dir: &Path,
    cwd: &Path,
    session_id: &str,
    content: &serde_json::Value,
    flag: &str,
) -> Result<LaunchSettingsFile, String> {
    if !crate::commands::workspace_open::is_single_path_segment(session_id) {
        return Err(format!("{session_id:?} cannot name a launch settings file"));
    }
    // Instance state remains private when a workspace contains the user's home.
    let private_state = crate::commands::paths::codesense_home_dir().is_some_and(|state| {
        dir.starts_with(&state) && state.starts_with(cwd) && state.as_path() != cwd
    });
    if dir.starts_with(cwd) && !private_state {
        return Err(format!(
            "the launch settings dir {} is inside the workspace {}",
            dir.display(),
            cwd.display()
        ));
    }
    ensure_private_dir(dir)?;
    let path = dir.join(format!(
        "{session_id}{SESSION_SEPARATOR}{}.json",
        uuid::Uuid::new_v4().simple()
    ));
    let bytes = serde_json::to_vec_pretty(content)
        .map_err(|e| format!("could not serialize launch settings: {e}"))?;
    write_new_private(&path, &bytes)?;
    Ok(LaunchSettingsFile {
        path,
        flag: flag.to_owned(),
    })
}

/// Create `path` fresh (0600, `O_EXCL`, never following a symlink) and write
/// `bytes`; a partial write removes the file.
fn write_new_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(path)
        .map_err(|e| format!("could not create {}: {e}", path.display()))?;
    file.write_all(bytes).map_err(|e| {
        let _ = std::fs::remove_file(path);
        format!("could not write {}: {e}", path.display())
    })
}

/// Remove launch settings files a previous run left behind (a crash, or a
/// session that ended while the app was closed). A file goes only when it is
/// older than [`SETTINGS_STALE_AGE`] and `alive` says its session is not
/// running; an unreadable age is left alone, and when `alive` cannot tell
/// (`None`, a terminal host that cannot answer) nothing is removed. A session
/// can outlive the app, so its file must outlive the app's record of it.
pub fn sweep_stale_launch_settings(alive: impl FnOnce(&[&str]) -> Option<HashSet<String>>) {
    let Ok(root) = launch_settings_root() else {
        return;
    };
    sweep_settings_in(&root, SystemTime::now(), SETTINGS_STALE_AGE, alive);
}

fn sweep_settings_in(
    dir: &Path,
    now: SystemTime,
    max_age: Duration,
    alive: impl FnOnce(&[&str]) -> Option<HashSet<String>>,
) {
    let stale: Vec<(PathBuf, Option<String>)> = mcp_scope::stale_paths_in(dir, now, max_age)
        .into_iter()
        .map(|path| {
            let session = settings_file_session(&path);
            (path, session)
        })
        .collect();
    if stale.is_empty() {
        return;
    }
    let sessions: Vec<&str> = stale.iter().filter_map(|(_, s)| s.as_deref()).collect();
    let Some(live) = alive(&sessions) else {
        return;
    };
    for (path, session) in &stale {
        if session.as_ref().is_some_and(|s| live.contains(s)) {
            continue;
        }
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests;

/// Save a typed test agent and prepare its launch, refusing any manifest error.
#[cfg(test)]
pub(crate) fn prepare_typed_for_test(
    home: &Path,
    principal: &Principal,
    agent: model::Agent,
    placement: &super::service::Placement,
    options: PrepareOptions<'_>,
) -> Result<AgentLaunch, String> {
    let key = service::save_typed_for_test(home, principal, &agent, placement)?;
    let (agent, mut resolved) = resolve::resolve_pinned(
        home,
        principal,
        &key,
        ResolveOptions {
            target_team: options.target_team.clone(),
        },
    )
    .map_err(|e| e.to_string())?;
    if !Vec::is_empty(&resolved.manifest.errors) {
        return Err(format!(
            "typed fixture manifest errors: {:?}",
            resolved.manifest.errors
        ));
    }
    resolved.manifest.manifest_hash = resolved.manifest.compute_hash();
    let key = resolved.agent.key.clone();
    prepare_resolved(
        &super::content_reader::FsContentReader,
        home,
        principal,
        &key,
        options,
        agent,
        resolved,
    )
}
