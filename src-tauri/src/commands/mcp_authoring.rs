// SPDX-License-Identifier: MIT
//! The FE-facing projection of a launch's MCP allowlist onto the interactive PTY
//! launch path.
//!
//! [`project_mcp_scope_config`] is a thin FE wrapper over SW-AN-41's
//! [`super::mcp_scope::project_from_worktree`]: it projects the effective set for
//! an **interactive** launch to an ephemeral 0600 config keyed by the PTY
//! `session_id` (so the existing `kill_session` teardown reaps it, `pty.rs`) and
//! returns the path the FE hands to `--mcp-config`. Zero projection logic is
//! reimplemented here.

use std::path::Path;

use tauri::AppHandle;

#[cfg(test)]
use codesense_pty_core::agent_cli::McpScope;
use codesense_pty_core::agent_cli::{descriptor, AgentCli, LaunchSettings};

use super::async_helpers::await_blocking;

/// SW-AN-42 → SW-AN-45: project the effective MCP config for an **interactive** launch and
/// return the launch additions (agent-neutral) the FE composes — `extraArgs` (a `config_flag`
/// agent's flags, with the RAW config path since the FE's `buildAgentLaunchCommand` re-quotes
/// every token) and an opaque one-use environment binding for `env_dir` agents. A thin
/// FE-callable wrapper over the one `project_scope` seam — the
/// `session_id` MUST be the launcher's PTY session id so the existing `kill_session →
/// remove_scope_config` teardown (`pty.rs`) reaps the 0600 secret-bearing scope dir.
///
/// The runtime is the built-in one whose CLI is `launch.binary`; a launch of any other CLI
/// gets an empty outcome (the FE adds nothing), never another agent's flags. A runtime's
/// projection propagates the loud error on a missing/malformed config — fail the launch
/// rather than over-expose tools.
/// `mcp_allow = []` is a valid bridge-only scope.
///
/// `mcp_allow = None` is a launch with no allowlist: the agent keeps its own servers and
/// gets the bridge (and, for Claude, its status hooks) from per-session files, nothing
/// written into `workdir`. Any config a previous run of the session left behind and no
/// live claim owns is removed first (the scope dir is created exclusively), so an agent
/// relaunched in a shell that outlived the app gets a fresh one.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn project_mcp_scope_config(
    app: AppHandle,
    session_id: String,
    workdir: String,
    mcp_allow: Option<Vec<String>>,
    account: Option<super::agent_accounts::ResolveAgentAccountInput>,
    launch_args: Option<Vec<String>>,
    launch: Option<ScopeLaunch>,
) -> Result<super::mcp_scope::ScopeOutcome, String> {
    project_mcp_scope_config_with_backend(
        app,
        session_id,
        workdir,
        mcp_allow,
        account,
        launch_args,
        launch,
        None,
    )
    .await
}

/// Internal relaunch projection retains the backend captured before its ended session is reaped.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn project_mcp_scope_config_with_backend(
    app: AppHandle,
    session_id: String,
    workdir: String,
    mcp_allow: Option<Vec<String>>,
    account: Option<super::agent_accounts::ResolveAgentAccountInput>,
    launch_args: Option<Vec<String>>,
    launch: Option<ScopeLaunch>,
    backend: Option<super::pty_backend::ResolvedTerminalBackend>,
) -> Result<super::mcp_scope::ScopeOutcome, String> {
    let mut launch = launch.unwrap_or_default();
    let scope = crate::context_repo::caller_scope::for_management_view(
        &app,
        launch.page_path.as_deref(),
        launch.frame_id.as_deref(),
    )?;
    if !matches!(scope, crate::context_repo::caller_scope::CallerScope::Human) {
        let ctx = scope.context(&app).await?;
        scope.record_page_launch(
            &app,
            &ctx.codesense_home,
            &ctx.principal(),
            &session_id,
            Path::new(&workdir),
            launch.team_key.as_deref(),
            launch.page_path.as_deref(),
        )?;
    }

    crate::agents::model::check_env(&launch.agent_env).map_err(|e| e.to_string())?;
    let agent_env: Vec<(String, String)> =
        std::mem::take(&mut launch.agent_env).into_iter().collect();
    // Project through the scope descriptor of the runtime whose CLI this is (Codex `env_dir`
    // vs Claude `config_flag`). Any other CLI gets nothing: never another agent's flags.
    let Some(runtime) = launch.binary.as_deref().and_then(runtime_of_binary) else {
        return Ok(super::mcp_scope::ScopeOutcome::default());
    };
    let expected_provider = crate::agents::adapter::provider_of(runtime);
    // A launch with no allowlist never relocates the agent's home, so its account is the
    // spawn's alone to resolve. The account is the runtime's.
    let account = account
        .filter(|_| mcp_allow.is_some() || launch.agent)
        .map(|input| super::agent_accounts::ResolveAgentAccountInput {
            runtime: Some(runtime),
            ..input
        });
    let resolved_binding = if let Some(input) = account {
        let (resolved, binding) = super::agent_accounts::resolve_for_launch(&app, input).await?;
        if !resolved.spawn_eligible {
            return Err(crate::agents::launch::account_refusal(resolved.reason));
        }
        Some(binding)
    } else {
        None
    };
    let descriptor = descriptor(runtime);
    // Config projection only: an unresolvable CLI projects without one, never refuses.
    let binary = super::mcp_workspace::resolve_agent_for_launch(&app, descriptor.binary.as_str())
        .await
        .ok()
        .map(|resolved| resolved.path);
    let launch_args = launch_args.unwrap_or_default();
    let account_home = await_blocking("project_mcp_scope_config.account", {
        move || {
            super::agent_accounts::resolved_launch_binding_home(
                resolved_binding.as_ref(),
                expected_provider,
            )
        }
    })
    .await?;
    let attribution_authority = if launch.agent {
        Some(super::route_to_context::resolve_ambient(&app).await?)
    } else {
        None
    };
    let host_app = app.clone();
    let (mut outcome, creation) = await_blocking("project_mcp_scope_config", move || {
        let trust = if launch.agent {
            attribution_hook_trust_for_projection(
                &host_app,
                runtime,
                &session_id,
                backend.as_ref(),
            )?
        } else {
            true
        };
        super::mcp_scope::remove_scope_config(&session_id);
        let cli = binary.as_deref().map(|binary| {
            super::mcp_scope::LaunchCli::new(binary, &launch_args)
                .with_attribution_hook_trust(trust)
        });
        let quote = |path: &str| path.to_string();
        if launch.agent {
            // SW-AN-52: this authoring path previews MCP projection outside the
            // full agent-launch spine, so there is no resolved content-layer
            // base to thread. Cwd-only projection — matches today's behavior.
            let (home, user) = attribution_authority
                .as_ref()
                .ok_or("missing launch profile")?;
            let principal = crate::domain::Principal::from(*user);
            return launch.project_agent_scope(
                runtime,
                &session_id,
                Path::new(&workdir),
                &mcp_allow.unwrap_or_default(),
                descriptor.mcp_scope.as_ref(),
                account_home.as_deref(),
                cli.as_ref(),
                (home, &principal),
                &launch_args,
            );
        }
        super::mcp_scope::project_profile_launch_for_cli(
            &session_id,
            Path::new(&workdir),
            mcp_allow.as_deref(),
            runtime,
            account_home.as_deref(),
            super::mcp_scope::ProfileLaunch {
                headless: false,
                bridge_override: launch.default_args,
            },
            cli.as_ref(),
            quote,
        )
    })
    .await?;
    bind_projected_scope_environment(&app, &mut outcome, creation, agent_env);
    Ok(outcome)
}

/// The host decision shared by regular scope projection and a pinned relaunch.
pub(crate) fn attribution_hook_trust_for_projection<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    runtime: AgentCli,
    session: &str,
    backend: Option<&super::pty_backend::ResolvedTerminalBackend>,
) -> Result<bool, String> {
    match backend {
        Some(backend) => Ok(!descriptor(runtime).attribution_hook_trust
            || super::pty_backend::ResolvedTerminalBackend::trusts_attribution_hooks(backend)),
        None => super::pty_backend::attribution_hook_trust_for_spawn(app, runtime, session),
    }
}

/// The authoring/respawn producer's actual one-use environment handoff.
pub(crate) fn bind_projected_scope_environment<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    outcome: &mut super::mcp_scope::ScopeOutcome,
    creation: Option<super::mcp_scope::ScopeCreation>,
    agent_env: Vec<(String, String)>,
) {
    outcome.environment_binding = super::agent_accounts::mint_launch_environment(
        app,
        super::agent_accounts::LaunchEnvironment {
            values: std::mem::take(&mut outcome.extra_env),
            scope: creation,
            agent_env,
            requires_spend_holds: false,
        },
    );
}

/// What the renderer says about the launch a scope is projected for.
#[derive(Debug, Default, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ScopeLaunch {
    pub frame_id: Option<String>,
    pub page_path: Option<String>,
    pub team_key: Option<String>,
    /// The CLI's path or basename, which names the built-in runtime the launch runs.
    pub binary: Option<String>,
    /// A named agent's relaunch: its scope alone, since its own per-launch settings file
    /// already rides its launch line.
    pub agent: bool,
    /// Captured effective CodeSense bridge tool grant; None is unrestricted, Some([]) denies all.
    pub bridge_tools: Option<Vec<String>>,
    /// The launch runs the launcher's default args rather than an agent's, which already
    /// carry the runtime's bridge override: add it.
    pub default_args: bool,
    /// A named agent's own environment variables (its launch's `env`), set on the spawn
    /// under every variable CodeSense sets, so a relaunch keeps them.
    pub agent_env: std::collections::BTreeMap<String, String>,
}

impl ScopeLaunch {
    /// The named-agent projection used by both initial authoring and exited-tab relaunch.
    pub(crate) fn project_agent_scope(
        &self,
        runtime: AgentCli,
        session_id: &str,
        workdir: &Path,
        mcp_allow: &[String],
        scope: Option<&codesense_pty_core::agent_cli::McpScope>,
        account_home: Option<&Path>,
        cli: Option<&super::mcp_scope::LaunchCli<'_>>,
        (codesense_home, principal): (&Path, &crate::domain::Principal),
        recorded_args: &[String],
    ) -> Result<
        (
            super::mcp_scope::ScopeOutcome,
            Option<super::mcp_scope::ScopeCreation>,
        ),
        String,
    > {
        if crate::agents::adapter::desktop(runtime).seed_home.is_some() {
            let launch = crate::agents::launch::remembered_home_launch(session_id)
                .ok_or("this launch's managed home settings are not available to relaunch; start the agent again from the Agents panel")?;
            if launch.runtime != runtime {
                return Err("cached launch runtime does not match this tab".into());
            }
            let (mut outcome, creation, applied) =
                crate::agents::launch::project_and_apply_for_cli(
                    &launch,
                    session_id,
                    workdir,
                    account_home,
                    cli,
                    str::to_string,
                )?;
            debug_assert!(
                applied.settings.is_none(),
                "home settings never own a separate file"
            );
            // The managed projection above already refused a launch without its exact
            // executable; a direct exec needs the same one, never a shell line.
            let cli = Option::ok_or(
                cli,
                "managed runtime requires its exact selected executable",
            )?;
            outcome.execution = super::mcp_scope::scoped_execution(
                runtime,
                cli.binary,
                super::mcp_scope::compose_args(recorded_args.to_vec(), outcome.extra_args.clone()),
            );
            return Ok((outcome, creation));
        }
        let (mut outcome, creation) = super::mcp_scope::project_scope_for_cli(
            session_id,
            workdir,
            mcp_allow,
            self.bridge_tools.as_deref(),
            scope,
            account_home,
            None,
            cli,
            str::to_string,
        )?;
        // Replay only non-attribution environment; capture is decided fresh below.
        // Env-based permissions remain required: without that cache OpenCode would start
        // on its native defaults, whereas flag/override-based settings ride the launch line.
        if let Some(env) = crate::agents::launch::remembered_launch_env(session_id) {
            super::mcp_scope::extend_launch_env(
                runtime,
                &mut outcome.extra_env,
                env.into_iter()
                    .filter(|(key, _)| !crate::attribution::launch::is_attribution_env(key)),
            );
        } else if matches!(descriptor(runtime).launch_settings, LaunchSettings::Env(_)) {
            super::mcp_scope::remove_scope_config(session_id);
            return Err(
                "this launch's permissions are not available to relaunch; start the agent again from the Agents panel"
                    .into(),
            );
        }
        let request = crate::attribution::launch::Request {
            codesense_home,
            principal,
            cwd: workdir,
            runtime,
        };
        let (args, env) = crate::agents::launch::refresh_attribution(
            &request,
            session_id,
            recorded_args,
            cli.is_none_or(|selected| selected.attribution_hook_trust),
        )?;
        if let Some(edit) = args {
            outcome.remove_args = Some(edit.remove_args);
            outcome.extra_args.extend(edit.extra_args);
        }
        super::mcp_scope::extend_launch_env(runtime, &mut outcome.extra_env, env);
        Ok((outcome, creation))
    }
}

/// The built-in runtime whose CLI is `binary`, by basename.
fn runtime_of_binary(binary: &str) -> Option<AgentCli> {
    codesense_pty_core::agent_cli::by_binary(Path::new(binary).file_name()?.to_str()?)
}

#[cfg(test)]
fn project_mcp_scope_config_impl(
    session_id: &str,
    workdir: &Path,
    mcp_allow: &[String],
    scope: Option<&McpScope>,
) -> Result<super::mcp_scope::ScopeOutcome, String> {
    // Raw config path — the FE launch builder re-quotes every token, so quoting here would
    // double-quote. An unscoped/descriptor-less adapter yields an empty outcome.
    super::mcp_scope::project_scope(session_id, workdir, mcp_allow, scope, |p| p.to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::commands::mcp_scope::BRIDGE_SERVER_KEY;
    use serde_json::json;
    use serde_json::Value;

    fn write_mcp_json(dir: &Path, value: &Value) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join(".mcp.json"),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("sw-an-42-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn r4_respawn_reads_current_capture_preferences_for_both_clis() {
        use crate::agents::launch::{forget_launch_env, project_and_apply_for_cli};
        use crate::agents::resolve::tests::alice;
        use crate::attribution::prefs::{self, AttributionChoice};
        use crate::attribution::testing::Launch;
        let _home_env = crate::routines::test_support::TempHome::new();
        let mut observations = Vec::new();
        for runtime in [AgentCli::Claude, AgentCli::Codex] {
            for case in ["on-off", "off-on", "private-fork"] {
                let fixture = Launch::new(runtime);
                let _daemon = fixture.daemon_running();
                if case == "off-on" {
                    prefs::set_for_checkout(
                        fixture.home(),
                        &alice(),
                        &fixture.cwd.to_string_lossy(),
                        Some(AttributionChoice::Off),
                        None,
                    )
                    .unwrap();
                } else if case == "private-fork" {
                    prefs::set_for_checkout(
                        fixture.home(),
                        &alice(),
                        &fixture.cwd.to_string_lossy(),
                        Some(AttributionChoice::Auto),
                        None,
                    )
                    .unwrap();
                }
                let inventory = super::super::mcp_scope::inventory::Fixture::new();
                let initial_cli = inventory.launch(&fixture.launch.launch.args);
                let session = format!("fresh-capture-{}", uuid::Uuid::new_v4());
                let (_, _, applied) = project_and_apply_for_cli(
                    &fixture.launch,
                    &session,
                    &fixture.cwd,
                    None,
                    Some(&initial_cli),
                    str::to_string,
                )
                .unwrap();
                let mut argv = fixture.launch.launch.args.clone();
                argv.extend(applied.argv(str::to_string));
                super::super::mcp_scope::remove_scope_config(&session);
                match case {
                    "on-off" => prefs::set_for_checkout(
                        fixture.home(),
                        &alice(),
                        &fixture.cwd.to_string_lossy(),
                        Some(AttributionChoice::Off),
                        None,
                    )
                    .unwrap(),
                    "off-on" => prefs::set_for_checkout(
                        fixture.home(),
                        &alice(),
                        &fixture.cwd.to_string_lossy(),
                        Some(AttributionChoice::On),
                        None,
                    )
                    .unwrap(),
                    _ => git2::Repository::open(&fixture.cwd)
                        .unwrap()
                        .remote_set_url("origin", "https://github.com/someone/api.git")
                        .unwrap(),
                }
                let secret = ["ghp_", "abcdefghijklmnopqrstuvwxyz0123"].concat();
                argv.push(format!("--note={secret}"));
                argv.push("token=example1234".into());
                let cli = inventory.launch(&argv);
                let launch = ScopeLaunch {
                    agent: true,
                    ..Default::default()
                };
                let result = launch
                    .project_agent_scope(
                        runtime,
                        &session,
                        &fixture.cwd,
                        fixture
                            .launch
                            .launch
                            .mcp_allow
                            .as_deref()
                            .unwrap_or_default(),
                        descriptor(runtime).mcp_scope.as_ref(),
                        None,
                        Some(&cli),
                        (fixture.home(), &crate::agents::resolve::tests::alice()),
                        &argv,
                    )
                    .unwrap()
                    .0;
                let wire = serde_json::to_value(&result).unwrap();
                assert!(!wire.to_string().contains(&secret));
                assert!(!wire.to_string().contains("token=example1234"));
                // Removal is by complete exact overrides, never individual -c tokens.
                let mut args = argv.clone();
                if let Some(remove) = &result.remove_args {
                    for pair in remove.chunks_exact(2) {
                        let at = args.windows(2).position(|window| window == pair).unwrap();
                        args.drain(at..at + 2);
                    }
                    args.extend(result.extra_args.clone());
                }
                assert!(args.contains(&format!("--note={secret}")));
                assert!(args.contains(&"token=example1234".to_owned()));
                let settings = if runtime == AgentCli::Claude {
                    let path = codesense_pty_core::agent_cli::find_settings_flag(&args, "--settings")
                        .unwrap()
                        .unwrap()
                        .value;
                    std::fs::read_to_string(path).unwrap()
                } else {
                    args.join(" ")
                };
                let hooks = settings.contains(codesense_pty_core::agent_cli::ATTRIBUTION_HOOK_MARKER);
                let trace = result
                    .extra_env
                    .iter()
                    .any(|(key, _)| key == "GIT_TRACE2_EVENT");
                let nesting = result
                    .extra_env
                    .iter()
                    .any(|(key, value)| key == "GIT_TRACE2_EVENT_NESTING" && value == "0");
                observations.push((
                    runtime,
                    case,
                    hooks,
                    trace,
                    nesting,
                    fixture.status().unwrap(),
                ));
                super::super::mcp_scope::remove_scope_config(&session);
                forget_launch_env(&session);
            }
        }
        for (runtime, case, hooks, trace, nesting, status) in observations {
            // Notes stay local, so a repo moved to a public fork keeps capturing.
            let enabled = case != "on-off";
            assert_eq!(
                (hooks, trace, nesting),
                (enabled, enabled, enabled),
                "{runtime:?} {case}"
            );
            let expected = if enabled {
                crate::attribution::AttributionStatus::Active
            } else {
                crate::attribution::AttributionStatus::Off {
                    reason: crate::attribution::OffReason::DisabledForRepo,
                }
            };
            assert_eq!(status, expected, "{runtime:?} {case}");
        }
    }

    pub(crate) fn attribution_respawn(
        fixture: &crate::attribution::testing::Launch,
        runtime: AgentCli,
    ) {
        let inventory = super::super::mcp_scope::inventory::Fixture::new();
        let cli = inventory.launch(&fixture.launch.launch.args);
        let session = format!("install-respawn-{}", uuid::Uuid::new_v4());
        let (_, _, applied) = crate::agents::launch::project_and_apply_for_cli(
            &fixture.launch,
            &session,
            &fixture.cwd,
            None,
            Some(&cli),
            str::to_string,
        )
        .unwrap();
        let mut argv = fixture.launch.launch.args.clone();
        argv.extend(applied.argv(str::to_string));
        super::super::mcp_scope::remove_scope_config(&session);
        let mut request: super::super::pty_layout::RespawnScope = serde_json::from_value(json!({
            "cwd": fixture.cwd, "mcpAllow": fixture.launch.launch.mcp_allow,
            "binary": descriptor(runtime).binary, "agent": true, "launchArgs": argv,
        }))
        .unwrap();
        request
            .take_launch()
            .project_agent_scope(
                runtime,
                &session,
                &fixture.cwd,
                request.mcp_allow.as_deref().unwrap_or_default(),
                descriptor(runtime).mcp_scope.as_ref(),
                None,
                Some(&cli),
                (fixture.home(), &crate::agents::resolve::tests::alice()),
                &argv,
            )
            .unwrap();
        super::super::mcp_scope::remove_scope_config(&session);
        crate::agents::launch::forget_launch_env(&session);
    }

    #[test]
    fn r3_claude_and_codex_respawns_keep_the_generated_attribution_environment() {
        use crate::agents::launch::{forget_launch_env, project_and_apply_for_cli};
        use crate::attribution::testing::{assert_wired, Launch};
        let _home_env = crate::routines::test_support::TempHome::new();
        let mut observations = Vec::new();
        for runtime in [AgentCli::Claude, AgentCli::Codex] {
            let fixture = Launch::new(runtime);
            let _daemon = fixture.daemon_running();
            let inventory = super::super::mcp_scope::inventory::Fixture::new();
            let cli = inventory.launch(&fixture.launch.launch.args);
            let session = format!("attribution-respawn-{}", uuid::Uuid::new_v4());
            let (initial, _, applied) = project_and_apply_for_cli(
                &fixture.launch,
                &session,
                &fixture.cwd,
                None,
                Some(&cli),
                str::to_string,
            )
            .unwrap();
            let mut argv = fixture.launch.launch.args.clone();
            argv.extend(applied.argv(str::to_string));
            let settings: Option<Value> = applied
                .settings
                .as_ref()
                .map(|file| serde_json::from_slice(&std::fs::read(file.path()).unwrap()).unwrap());
            assert_wired(runtime, &argv, settings.as_ref(), &initial.extra_env);
            super::super::mcp_scope::remove_scope_config(&session);
            let mut request: super::super::pty_layout::RespawnScope =
                serde_json::from_value(json!({
                    "cwd": fixture.cwd,
                    "mcpAllow": fixture.launch.launch.mcp_allow,
                    "binary": descriptor(runtime).binary,
                    "agent": true,
                    "launchArgs": argv,
                }))
                .unwrap();
            let projected = request.take_launch().project_agent_scope(
                runtime,
                &session,
                &fixture.cwd,
                request.mcp_allow.as_deref().unwrap_or_default(),
                descriptor(runtime).mcp_scope.as_ref(),
                None,
                Some(&cli), (fixture.home(), &crate::agents::resolve::tests::alice()), &argv,);
            super::super::mcp_scope::remove_scope_config(&session);
            forget_launch_env(&session);
            let (projected, _) = projected.unwrap();
            for variable in ["GIT_TRACE2_EVENT", "GIT_TRACE2_EVENT_NESTING"] {
                let expected = initial
                    .extra_env
                    .iter()
                    .find(|(key, _)| key == variable)
                    .unwrap()
                    .clone();
                let actual = projected
                    .extra_env
                    .iter()
                    .find(|(key, _)| key == variable)
                    .cloned();
                observations.push((runtime, expected, actual));
            }
            // Projection refreshes MCP scope; the recorded argv/settings still carry the hooks.
            assert_wired(runtime, &argv, settings.as_ref(), &initial.extra_env);
        }
        for (runtime, expected, actual) in observations {
            assert_eq!(
                actual,
                Some(expected),
                "{runtime:?}: generated trace env must reach the new binding"
            );
        }
    }

    #[test]
    fn an_opencode_respawn_sets_its_reflog_marker_once_and_fresh() {
        use crate::agents::launch::{forget_launch_env, project_and_apply_for_cli};
        use crate::attribution::testing::{assert_reflog_marker, Launch};
        let _home_env = crate::routines::test_support::TempHome::new();
        let fixture = Launch::new(AgentCli::OpenCode);
        let inventory = super::super::mcp_scope::inventory::Fixture::new();
        let cli = inventory.launch(&fixture.launch.launch.args);
        let session = format!("unsupported-respawn-{}", uuid::Uuid::new_v4());
        let (initial, _, applied) = project_and_apply_for_cli(
            &fixture.launch,
            &session,
            &fixture.cwd,
            None,
            Some(&cli),
            str::to_string,
        )
        .unwrap();
        assert_reflog_marker(&initial.extra_env, Some("opencode"));
        super::super::mcp_scope::remove_scope_config(&session);
        let mut argv = fixture.launch.launch.args.clone();
        argv.extend(applied.argv(str::to_string));
        let mut request: super::super::pty_layout::RespawnScope = serde_json::from_value(json!({
            "cwd": fixture.cwd,
            "mcpAllow": fixture.launch.launch.mcp_allow,
            "binary": descriptor(AgentCli::OpenCode).binary,
            "agent": true,
            "launchArgs": argv,
        }))
        .unwrap();
        let projected = request.take_launch().project_agent_scope(
            AgentCli::OpenCode,
            &session,
            &fixture.cwd,
            request.mcp_allow.as_deref().unwrap_or_default(),
            descriptor(AgentCli::OpenCode).mcp_scope.as_ref(),
            None,
            Some(&cli),
            (fixture.home(), &crate::agents::resolve::tests::alice()),
            &argv,
        );
        super::super::mcp_scope::remove_scope_config(&session);
        forget_launch_env(&session);
        let (projected, _) = projected.unwrap();
        // The remembered marker is dropped and the fresh plan's is added: exactly one.
        assert_reflog_marker(&projected.extra_env, Some("opencode"));
    }

    #[test]
    fn opencode_cached_respawn_folds_settings_into_the_fresh_scope() {
        use crate::agents::launch::{
            forget_launch_env, prepare, project_and_apply_for_cli, PrepareOptions,
        };
        use crate::agents::resolve::tests::{alice, draft, save, team_content};

        let mut home_env = crate::routines::test_support::TempHome::new();
        let original_xdg = home_env.isolate_xdg_config_home();
        let root = tempfile::tempdir().unwrap();
        team_content(root.path(), &alice());
        let mut definition = draft("opencode-relaunch", AgentCli::OpenCode);
        definition.args.clear();
        definition.mcp.bridge_tools = Some(vec!["who_am_i".into()]);
        definition.permissions = Some(
            serde_json::from_value(json!({"opencode": {"permission": {"bash": "deny"}}})).unwrap(),
        );
        let key = save(root.path(), &alice(), definition);
        let prepared = prepare(root.path(), &alice(), &key, PrepareOptions::default()).unwrap();
        let cwd = root.path().join("cwd");
        std::fs::create_dir_all(&cwd).unwrap();
        let descriptor = descriptor(AgentCli::OpenCode);
        let scope = descriptor.mcp_scope.as_ref().unwrap();
        let config_path = cwd.join(&scope.config_filename);
        std::fs::write(
            &config_path,
            json!({"mcp": {"slack": {"type": "local", "command": ["slack-before"]}}}).to_string(),
        )
        .unwrap();
        let session = format!("opencode-cached-respawn-{}", uuid::Uuid::new_v4());
        let (_first, _creation, _applied) =
            project_and_apply_for_cli(&prepared, &session, &cwd, None, None, str::to_string)
                .unwrap();
        super::super::mcp_scope::remove_scope_config(&session);

        // Relaunch must project the current workspace config, not replay the old scope.
        std::fs::write(
            &config_path,
            json!({"mcp": {"slack": {"type": "local", "command": ["slack-after"]}}}).to_string(),
        )
        .unwrap();
        let mut request: super::super::pty_layout::RespawnScope = serde_json::from_value(json!({
            "cwd": cwd,
            "mcpAllow": prepared.launch.mcp_allow,
            "bridgeTools": prepared.launch.effective_bridge_tools,
            "binary": descriptor.binary,
            "agent": true,
            "launchArgs": prepared.launch.args,
        }))
        .unwrap();
        let relaunch = request.take_launch();
        let result = relaunch.project_agent_scope(
            AgentCli::OpenCode,
            &session,
            Path::new(&request.cwd),
            request.mcp_allow.as_deref().unwrap(),
            Some(scope),
            None,
            None, (root.path(), &crate::agents::resolve::tests::alice()), &[], );
        super::super::mcp_scope::remove_scope_config(&session);
        forget_launch_env(&session);
        let (outcome, _) = result.unwrap();
        let configs: Vec<Value> = outcome
            .extra_env
            .iter()
            .filter(|(name, _)| name == "OPENCODE_CONFIG_CONTENT")
            .map(|(_, value)| serde_json::from_str(value).unwrap())
            .collect();
        assert_eq!(
            configs.len(),
            1,
            "one effective config after cached respawn"
        );
        let config = &configs[0];
        assert_eq!(config["mcp"]["slack"]["command"], json!(["slack-after"]));
        assert_eq!(
            config["mcp"]["codesense"]["environment"]["CODESENSE_TAB_SESSION_ID"],
            session
        );
        let plugin =
            url::Url::from_directory_path(home_env.root().join(".codesense/opencode/codesense/"))
                .unwrap();
        assert_eq!(config["plugins"], json!([plugin.as_str()]));
        assert_eq!(config["share"], "disabled");
        assert_eq!(config["update"], "disable");
        let permissions = config["permissions"].as_array().unwrap();
        for (action, effect) in [
            ("*", "ask"),
            ("shell", "deny"),
            ("codesense_*", "deny"),
            ("codesense_who_am_i", "allow"),
        ] {
            assert!(permissions.contains(&json!({
                "action": action, "resource": "*", "effect": effect,
            })));
        }
        assert!(outcome.extra_env.contains(&(
            "XDG_CONFIG_HOME".into(),
            home_env
                .root()
                .join(".codesense/opencode/codesense/xdg")
                .to_string_lossy()
                .into_owned(),
        )));
        assert!(outcome.extra_env.contains(&(
            "CODESENSE_ORIGINAL_XDG_CONFIG_HOME".into(),
            original_xdg.to_string_lossy().into_owned(),
        )));
    }

    #[test]
    fn opencode_relaunch_refuses_a_session_without_cached_permissions() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let root = tempfile::tempdir().unwrap();
        let mut request: super::super::pty_layout::RespawnScope = serde_json::from_value(json!({
            "cwd": root.path(), "mcpAllow": [], "binary": "opencode", "agent": true,
        }))
        .unwrap();
        let session = format!("opencode-never-launched-{}", uuid::Uuid::new_v4());
        let result = request.take_launch().project_agent_scope(
            AgentCli::OpenCode,
            &session,
            root.path(),
            &[],
            descriptor(AgentCli::OpenCode).mcp_scope.as_ref(),
            None,
            None, (root.path(), &crate::agents::resolve::tests::alice()), &[], );
        super::super::mcp_scope::remove_scope_config(&session);
        assert!(result
            .err()
            .unwrap()
            .contains("permissions are not available to relaunch"));
    }

    #[test]
    fn respawn_preserves_bridge_policy_on_native_alias_and_relay_grant() {
        let _home_env = crate::routines::test_support::TempHome::new();
        for restriction in [Value::Null, json!([]), json!(["who_am_i"])] {
            let root = tempfile::tempdir().unwrap();
            let fixture = super::super::mcp_scope::inventory::Fixture::new();
            let args = Vec::new();
            let cli = fixture.launch(&args);
            let mut request: super::super::pty_layout::RespawnScope =
                serde_json::from_value(json!({
                    "cwd": root.path(), "mcpAllow": [], "binary": "codex",
                    "agent": true, "bridgeTools": restriction,
                }))
                .unwrap();
            let scope = descriptor(AgentCli::Codex);
            let session = format!("bridge-respawn-{}", uuid::Uuid::new_v4());
            let (projected, _creation) = request
                .take_launch()
                .project_agent_scope(
                    AgentCli::Codex,
                    &session,
                    root.path(),
                    &[],
                    scope.mcp_scope.as_ref(),
                    Some(root.path()),
                    Some(&cli), (root.path(), &crate::agents::resolve::tests::alice()), &[], )
                .unwrap();
            let dir = super::super::mcp_scope::mcp_scope_session_dir(&session);
            let config: toml::Value = toml::from_str(projected.extra_args.last().unwrap()).unwrap();
            let native = &config["mcp_servers"][&projected.server_aliases["codesense"]];
            let actual = native
                .get("enabled_tools")
                .map(|v| serde_json::to_value(v).unwrap());
            let grant: Value =
                serde_json::from_slice(&std::fs::read(dir.join("local-relay-grant.json")).unwrap())
                    .unwrap();
            // Clean before assertions, including a RED failure.
            super::super::mcp_scope::remove_scope_config(&session);
            assert_eq!(
                actual,
                (!restriction.is_null()).then_some(restriction.clone())
            );
            assert_eq!(grant["grant"]["codesense"]["enabled_tools"], restriction);
        }
    }

    #[test]
    fn project_returns_rendered_flags_pointing_at_the_scoped_config() {
        let _home_env = crate::routines::test_support::TempHome::new();
        // SW-AN-43: the command returns the rendered launch flags (agent-neutral), with
        // the RAW config path embedded (the FE re-quotes each token). The projected file
        // still carries the bridge + allowlisted server.
        let dir = temp_dir("project");
        write_mcp_json(
            &dir,
            &json!({
                "mcpServers": {
                    BRIDGE_SERVER_KEY: { "command": "/abs/bridge" },
                    "buildkite": { "command": "bk" }
                }
            }),
        );
        let sid = format!("shell-an42-{}", std::process::id());
        let outcome = project_mcp_scope_config_impl(
            &sid,
            &dir,
            &["buildkite".to_string()],
            descriptor(AgentCli::Claude).mcp_scope.as_ref(),
        )
        .unwrap();
        assert_eq!(
            outcome.extra_args.first().map(String::as_str),
            Some("--mcp-config")
        );
        assert_eq!(
            outcome.extra_args.last().map(String::as_str),
            Some("--strict-mcp-config")
        );
        assert!(
            outcome.extra_env.is_empty(),
            "Claude is config_flag → no env"
        );
        let path = &outcome.extra_args[1];
        let written: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let servers = written["mcpServers"].as_object().unwrap();
        assert!(
            servers.contains_key(BRIDGE_SERVER_KEY),
            "bridge force-included"
        );
        assert!(
            servers.contains_key("buildkite"),
            "allowlisted server present"
        );
        super::super::mcp_scope::remove_scope_config(&sid);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_launch_takes_the_runtime_of_its_cli_and_any_other_cli_gets_none() {
        assert_eq!(runtime_of_binary("codex"), Some(AgentCli::Codex));
        assert_eq!(
            runtime_of_binary("/opt/homebrew/bin/claude"),
            Some(AgentCli::Claude)
        );
        assert_eq!(runtime_of_binary("cursor-agent"), None);
        assert_eq!(runtime_of_binary("/x/acme"), None);
    }

    #[test]
    fn the_launch_options_default_when_absent() {
        let launch: ScopeLaunch = serde_json::from_value(json!({ "binary": "codex" })).unwrap();
        assert_eq!(launch.binary.as_deref(), Some("codex"));
        assert!(!launch.agent && !launch.default_args);
        let launch: ScopeLaunch =
            serde_json::from_value(json!({ "agent": true, "defaultArgs": true })).unwrap();
        assert!(launch.agent && launch.default_args);
    }

    #[test]
    fn project_scopes_a_folder_without_canonical_to_the_bridge() {
        let _home_env = crate::routines::test_support::TempHome::new();
        // The launch-time projection never launches unscoped: a folder with no canonical
        // `.mcp.json` gets a scoped config of the bridge alone, and nothing is written
        // into the folder.
        let dir = temp_dir("project-missing");
        std::fs::create_dir_all(&dir).unwrap();
        let sid = format!("shell-an42-missing-{}", std::process::id());
        let outcome = project_mcp_scope_config_impl(
            &sid,
            &dir,
            &[],
            descriptor(AgentCli::Claude).mcp_scope.as_ref(),
        )
        .unwrap();
        assert!(outcome
            .extra_args
            .iter()
            .any(|a| a == "--strict-mcp-config"));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        super::super::mcp_scope::remove_scope_config(&sid);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
