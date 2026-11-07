// SPDX-License-Identifier: MIT
//! Portable saved-Agent inputs, separated from desktop account eligibility.
use super::{keys::AgentKey, manifest::StartupManifest};
use codesense_host_proto::agent_cli::NativeLaunch;
use codesense_host_proto::reverse_mcp::AgentStartup;
use codesense_pty_core::agent_cli::{validate_startup, AgentCli};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CapturedAgent {
    pub workspace_handle: String,
    /// The Agent's display name: its tab's title when the launch names none.
    pub name: String,
    #[serde(default)]
    pub default_role: Option<String>,
    pub manifest: StartupManifest,
    pub runtime: AgentCli,
    pub startup: AgentStartup,
    pub mcp_allow: Vec<String>,
    /// SW-XS-8: the merge-marked workspace files the launch built (the deep
    /// merge of each across its layers), which Step 2 writes on the machine.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_files: Vec<super::manifest::GeneratedEntry>,
}
impl CapturedAgent {
    pub(crate) fn key(&self) -> String {
        self.manifest.agent.key.to_string()
    }
}

pub(crate) fn prepare(
    home: &std::path::Path,
    principal: &crate::domain::Principal,
    key: &AgentKey,
    org: String,
    team: String,
    workspace_handle: String,
) -> Result<CapturedAgent, String> {
    // The shared resolver captures the exact saved revision and manifest. Its
    // desktop account preview is intentionally neither admitted nor transported.
    let launch = super::launch::prepare(
        home,
        principal,
        key,
        super::launch::PrepareOptions {
            target_team: Some((org, team)),
            mode_override: Some(crate::agents::adapter::LaunchMode::Interactive),
            ..Default::default()
        },
    )?;
    if launch.launch.bridge_tools.is_some() {
        return Err(
            "bridge_tools is not enforced on remote machines yet; remove it or launch locally"
                .into(),
        );
    }
    if !launch.manifest.errors.is_empty() {
        return Err(format!(
            "saved-Agent startup manifest is invalid: {}",
            serde_json::to_string(&launch.manifest.errors).map_err(|e| e.to_string())?
        ));
    }
    let runtime = launch.runtime;
    let mut settings = launch
        .generated
        .first()
        .ok_or("saved-Agent permissions are missing")?
        .content
        .clone();
    for essential in &launch.manifest.essentials {
        match essential.id.as_str() {
            "codesense-bridge" => {}
            "codesense-notify-settings" => {
                crate::commands::content_engine::merge_remote_notify_hooks(&mut settings, runtime)?
            }
            _ => {
                return Err(format!(
                    "remote startup cannot apply required essential {}",
                    essential.id
                ))
            }
        }
    }
    let startup = AgentStartup {
        args: launch.launch.args,
        settings,
        prompt: launch.default_prompt,
        env: launch.launch.env,
    };
    validate_startup(&startup, NativeLaunch::Cli(runtime))?;
    let workspace_files = launch
        .generated
        .iter()
        .filter(|entry| entry.target == super::manifest::GeneratedTarget::WorkspaceFile)
        .cloned()
        .collect();
    Ok(CapturedAgent {
        workspace_files,
        workspace_handle,
        name: launch.name,
        default_role: launch.queue_role,
        manifest: launch.manifest,
        runtime,
        startup,
        mcp_allow: launch
            .launch
            .mcp_allow
            .ok_or("saved Agent must declare an MCP allowlist")?,
    })
}

/// Start a captured Agent with this launch's own first prompt, in place of the Agent's
/// default one, as a local launch does. The machine passes it to the Agent at start.
pub(crate) fn start_with_prompt(
    agent: &mut CapturedAgent,
    prompt: Option<String>,
) -> Result<(), String> {
    let Some(prompt) = prompt.filter(|p| !p.trim().is_empty()) else {
        return Ok(());
    };
    agent.startup.prompt = Some(prompt);
    validate_startup(&agent.startup, NativeLaunch::Cli(agent.runtime))
}

/// The named-launch admission seam: completed preparation is the only route
/// to the durable commit/spawn callback. Restored attempted intents skip both.
pub(crate) async fn drive_prepared<P, S, T>(
    preparing: bool,
    prepare: P,
    commit_spawn: impl FnOnce(T) -> S,
) -> Result<(), String>
where
    P: std::future::Future<Output = Result<T, String>>,
    S: std::future::Future<Output = Result<(), String>>,
{
    if !preparing {
        return Err("spawn was already attempted; attach to the original session".into());
    }
    let ready = prepare.await?;
    commit_spawn(ready).await
}

/// Step 2 of an agent launch on a machine (SW-XS-7): the one preparation
/// engine against the workspace, under the destination lock the caller
/// holds. A saved Agent gets its whole want set (its captured startup
/// manifest pinned first); a plain `claude` / `codex` gets its runtime's
/// default context. Nothing unchanged costs more than one manifest call.
/// A failed entry fails the launch (§11.7): the report is `Ready` or `Err`.
pub(crate) fn prepare_workspace(
    ctx: &crate::content_engine::ResolverContext,
    slug: &crate::domain::SourceRepoSlug,
    io: &dyn crate::content_engine::destination::DestinationIo,
    launch: NativeLaunch,
    agent: Option<(&StartupManifest, &str)>,
    workspace_files: &[super::manifest::GeneratedEntry],
    progress: &dyn Fn(&super::prepare::PrepareProgress),
) -> Result<super::prepare::PrepareReport, String> {
    use super::prepare;
    let crate::content_engine::destination::DestinationIdentity::Remote(identity) = io.identity()
    else {
        return Err("remote destination required".into());
    };
    let runtime = launch
        .cli()
        .ok_or("a shell has no agent context to prepare")?;
    let principal = ctx.principal();
    let target = super::ManifestTarget {
        context_root: None,
        org: ctx.org_slug.clone(),
        team: ctx.team_name.clone().unwrap_or_default(),
        repo: Some(slug.clone()),
    };
    let mut want = match agent {
        Some((manifest, _)) => prepare::WantSet::for_agent(
            &ctx.codesense_home,
            &principal,
            &manifest.agent.key,
            &target,
            false,
            Some(manifest),
        )?,
        None => {
            prepare::WantSet::for_runtime(&ctx.codesense_home, &principal, runtime, &target, false)?
        }
    };
    // SW-XS-8 (the XS-14 handoff): merged workspace files reach a machine
    // through the same engine, with the same managed-record rule.
    want.add_workspace_files(&ctx.codesense_home, &principal, workspace_files)?;
    let legacy = || prepare::legacy_rows_remote(ctx, slug, identity);
    let report = prepare::prepare(prepare::PrepareRequest {
        home: &ctx.codesense_home,
        principal: &principal,
        destination: prepare::Destination::Remote { io },
        want: &want,
        origin: match agent {
            Some((manifest, _)) => format!("step2:{}", manifest.agent.key),
            None => format!("step2:{}", runtime.as_str()),
        },
        workspace: identity.workspace_handle.clone(),
        agent: agent
            .map(|(_, name)| name.to_string())
            .unwrap_or_else(|| runtime.as_str().to_string()),
        legacy: &legacy,
        prune: false,
        progress,
        validate: &|| io.validate_authority(),
        explicit: Default::default(),
    })?;
    prepare::progress::finish(
        progress,
        &report,
        &identity.workspace_handle,
        agent.map(|(_, name)| name).unwrap_or(runtime.as_str()),
        "team content",
    );
    if let Some(failure) = report.failure() {
        return Err(failure);
    }
    log::info!(target: "agents", "{}", report.startup_note("team content"));
    Ok(report)
}

/// Step 2 of a launch under the destination lock (SW-XS-2). The guard is
/// consumed here and dropped when Step 2 ends, failed or not, so nothing that
/// runs after it (the PTY spawn) can wait on, or hold, the destination lock.
pub(crate) async fn under_destination_lock<W, T>(
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
    step_two: W,
) -> Result<T, String>
where
    W: std::future::Future<Output = Result<T, String>>,
{
    let result = step_two.await;
    drop(guard);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::resolve::tests::{alice, draft, save, team_content, ORG, TEAM};

    #[test]
    fn remote_bridge_tool_policy_is_never_silently_dropped() {
        let root = tempfile::tempdir().unwrap();
        team_content(root.path(), &alice());
        for (index, tools) in [None, Some(vec![]), Some(vec!["who_am_i".to_string()])]
            .into_iter()
            .enumerate()
        {
            let mut definition = draft(&format!("remote-bridge-{index}"), AgentCli::Codex);
            definition.mcp.servers.clear();
            definition.mcp.bridge_tools = tools.clone();
            definition.permissions = Some(
                serde_json::from_value(serde_json::json!({"codex": {
                    "approval_policy":"never", "sandbox_mode":"read-only"
                }}))
                .unwrap(),
            );
            let key = save(root.path(), &alice(), definition.clone());
            let result = prepare(
                root.path(),
                &alice(),
                &key,
                ORG.into(),
                TEAM.into(),
                "prepared".into(),
            );
            if tools.is_some() {
                let error = result
                    .expect_err("remote preparation must not drop the bridge tool restriction");
                assert!(
                    error.contains("bridge_tools") && error.contains("remote"),
                    "{error}"
                );
            } else {
                let captured = result.unwrap();
                assert_eq!(captured.startup.settings["approval_policy"], "never");
                assert_eq!(captured.startup.settings["sandbox_mode"], "read-only");
                assert_eq!(captured.startup.args, definition.args);
                assert!(captured.mcp_allow.is_empty());
            }
        }
    }

    #[test]
    fn portable_agent_is_pinned_without_desktop_account_or_credentials() {
        let root = tempfile::tempdir().unwrap();
        team_content(root.path(), &alice());
        for runtime in [AgentCli::Claude, AgentCli::Codex] {
            let mut definition = draft(runtime.as_str(), runtime);
            definition.mcp.servers.clear();
            definition.env = [(
                "CLAUDE_CODE_DISABLE_AUTO_MEMORY".to_string(),
                "1".to_string(),
            )]
            .into_iter()
            .collect();
            let key = save(root.path(), &alice(), definition.clone());
            let provider = crate::agents::adapter::provider_of(runtime).unwrap();
            let account = crate::install::agent_account_store::create_profile(
                root.path(),
                &alice(),
                provider,
                "Not signed in",
            )
            .unwrap();
            crate::agents::binding::set(root.path(), &alice(), &key, &account.id).unwrap();
            let local =
                crate::agents::launch::prepare(root.path(), &alice(), &key, Default::default())
                    .unwrap();
            assert!(!local.account_preview.spawn_eligible);
            let portable = prepare(
                root.path(),
                &alice(),
                &key,
                ORG.into(),
                TEAM.into(),
                "prepared".into(),
            )
            .unwrap();
            assert_eq!(portable.manifest, local.manifest);
            assert_eq!(portable.key(), key.to_string());
            // The agent's env goes to the machine with its startup, which the host
            // applies under every variable it sets for the launch.
            assert_eq!(portable.startup.env, definition.env);
            validate_startup(&portable.startup, NativeLaunch::Cli(runtime)).unwrap();
            let json = serde_json::to_value(&portable).unwrap();
            assert!(json.get("account_preview").is_none());
            assert!(json.get("account_override").is_none());
            assert_eq!(
                serde_json::from_value::<CapturedAgent>(json).unwrap(),
                portable
            );
            // A launch's own first prompt replaces the Agent's default one; none keeps it.
            let mut started = portable.clone();
            start_with_prompt(&mut started, None).unwrap();
            assert_eq!(started.startup.prompt, portable.startup.prompt);
            start_with_prompt(&mut started, Some("  ".into())).unwrap();
            assert_eq!(started.startup.prompt, portable.startup.prompt);
            start_with_prompt(&mut started, Some("Fix the flaky test".into())).unwrap();
            assert_eq!(
                started.startup.prompt.as_deref(),
                Some("Fix the flaky test")
            );
            assert!(start_with_prompt(&mut started, Some("bad\0prompt".into())).is_err());
        }
    }

    /// SV-CDE-7: capture is a local-machine feature. A remote launch carries none of it and
    /// is reported `unsupported`.
    #[test]
    fn a_remote_launch_carries_no_git_ai_capture_and_reports_unsupported() {
        let root = tempfile::tempdir().unwrap();
        team_content(root.path(), &alice());
        for runtime in [AgentCli::Claude, AgentCli::Codex] {
            let mut definition = draft(runtime.as_str(), runtime);
            definition.mcp.servers.clear();
            let key = save(root.path(), &alice(), definition);
            let captured = prepare(
                root.path(),
                &alice(),
                &key,
                ORG.into(),
                TEAM.into(),
                "prepared".into(),
            )
            .unwrap();
            let wire = serde_json::to_string(&captured).unwrap();
            assert!(!wire.contains(codesense_pty_core::agent_cli::ATTRIBUTION_HOOK_MARKER));
            assert!(!wire.contains("GIT_TRACE2"));
        }
        assert_eq!(
            crate::attribution::remote_status(),
            crate::attribution::AttributionStatus::Unsupported {
                reason: "remote_machine".into()
            }
        );
    }

    /// SW-XS-2 AC3 (lock probe): Step 2 runs holding the destination lock, and
    /// the spawn callback runs with it released.
    #[tokio::test]
    async fn step_two_releases_the_destination_lock_before_the_spawn() {
        let lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
        let during = lock.clone();
        let at_spawn = lock.clone();
        let guard = lock.clone().lock_owned().await;
        drive_prepared(
            true,
            under_destination_lock(Some(guard), async move {
                assert!(during.try_lock().is_err(), "Step 2 runs under the lock");
                Ok(())
            }),
            |()| async move {
                assert!(
                    at_spawn.try_lock().is_ok(),
                    "the spawn runs with the destination lock released"
                );
                Ok(())
            },
        )
        .await
        .unwrap();
        // A failed Step 2 releases the lock too, and never reaches the spawn.
        let guard = lock.clone().lock_owned().await;
        let failed = drive_prepared(
            true,
            under_destination_lock(Some(guard), async {
                Err::<(), _>("step 2 failed".to_string())
            }),
            |()| async { panic!("a failed Step 2 must not spawn") },
        )
        .await;
        assert!(failed.is_err());
        assert!(lock.try_lock().is_ok());
    }
}
