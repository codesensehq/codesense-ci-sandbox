// SPDX-License-Identifier: MIT
//! Attribution projection keeps the host captured before the real respawn reap.
#![cfg(test)]
use super::*;
use crate::agents::resolve::tests::alice;
use crate::attribution::testing::Launch;
use crate::attribution::AttributionStatus;
use crate::commands::{mcp_authoring, mcp_scope};
use codesense_pty_core::agent_cli::{descriptor, AgentCli};

fn check_respawn_attribution(captured_in_process: bool) {
    let _home = crate::routines::test_support::TempHome::new();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let local = in_process(&runtime);
    // Without its activity service this real host advertises no checkpoint trust.
    let mut sidecar = (!captured_in_process).then(TestServer::start);
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(local.clone());
    if let Some(sidecar) = sidecar.as_ref() {
        app.manage(SessionServerState::at(Some(sidecar.root().to_path_buf())));
    }
    let fixture = Launch::new(AgentCli::Codex);
    let _daemon = fixture.daemon_running();
    let session = format!("captured-host-{}", uuid::Uuid::new_v4());
    let session_id = sid(&session);
    let reg = registration(&session, None, false);
    if captured_in_process {
        local
            .open_tab(
                tab_request(&session),
                Box::new(|| {
                    register(&runtime, &local, "exit 0", reg);
                    Ok(())
                }),
            )
            .unwrap();
    } else {
        let selected = resolve_spawn_backend(app.handle(), LOCAL_MACHINE_ID, &session).unwrap();
        let client = ResolvedTerminalBackend::client(&selected).unwrap();
        assert!(!client.supports_attribution_hook_trust());
        sidecar.as_ref().unwrap().track(&session);
        client
            .open_tab(
                tab_request(&session),
                Box::new(|| {
                    client
                        .spawn(sh("exit 0"), reg)
                        .map(|_| ())
                        .map_err(|error| error.message)
                }),
            )
            .unwrap();
    }
    let captured = resolve_spawn_backend(app.handle(), LOCAL_MACHINE_ID, &session).unwrap();
    if captured_in_process {
        // Recovery arrives after the existing fallback session pins its current host.
        let recovered = sidecar.insert(TestServer::start());
        app.manage(SessionServerState::at(Some(recovered.root().to_path_buf())));
    }
    assert_eq!(
        ResolvedTerminalBackend::trusts_attribution_hooks(&captured),
        captured_in_process
    );
    assert!(captured.domain().has_session(&session_id).unwrap());
    assert!(wait_until(|| captured
        .domain()
        .layout_snapshot(&app_viewer(), &ClientBootId::new("captured-hook-trust"),)
        .ok()
        .is_some_and(|layout| layout
            .workspaces
            .values()
            .flat_map(|workspace| &workspace.tabs)
            .any(
                |tab| tab.session == session_id && tab.state.is_exited()
            ))));
    let inventory = mcp_scope::inventory::Fixture::new();
    let mut inspected = false;
    let result = respawn_tab_core(
        captured,
        &session_id,
        |_| Ok(()),
        |pinned| {
            // TerminalDomain::respawn_tab has called the actual host's begin_respawn.
            assert!(
                !pinned.domain().has_session(&session_id).unwrap(),
                "the registry was reaped before projection"
            );
            let recovered = resolve_spawn_backend(app.handle(), LOCAL_MACHINE_ID, &session)?;
            assert!(
                !ResolvedTerminalBackend::trusts_attribution_hooks(&recovered),
                "a fresh lookup now selects the recovered old sidecar"
            );
            let trust = mcp_authoring::attribution_hook_trust_for_projection(
                app.handle(),
                AgentCli::Codex,
                &session,
                Some(&pinned),
            )?;
            let cli = inventory
                .launch(&fixture.launch.launch.args)
                .with_attribution_hook_trust(trust);
            let (outcome, _) = mcp_authoring::ScopeLaunch {
                agent: true,
                ..Default::default()
            }
            .project_agent_scope(
                AgentCli::Codex,
                &session,
                &fixture.cwd,
                fixture
                    .launch
                    .launch
                    .mcp_allow
                    .as_deref()
                    .unwrap_or_default(),
                descriptor(AgentCli::Codex).mcp_scope.as_ref(),
                None,
                Some(&cli),
                (fixture.home(), &alice()),
                &fixture.launch.launch.args,
            )?;
            assert_eq!(
                outcome
                    .extra_args
                    .iter()
                    .any(|arg| arg.contains("checkpoint")),
                captured_in_process
            );
            assert_eq!(
                outcome
                    .extra_env
                    .iter()
                    .any(|(key, _)| key == "GIT_TRACE2_EVENT"),
                captured_in_process
            );
            let expected = if captured_in_process {
                AttributionStatus::Active
            } else {
                AttributionStatus::Error {
                    message:
                        "The terminal server is outdated; restart it to enable line attribution."
                            .into(),
                }
            };
            assert_eq!(fixture.status(), Some(expected));
            inspected = true;
            // The fixture inspects the projected recipe without running an agent CLI.
            Err("projection inspected".into())
        },
    );
    assert!(inspected);
    assert!(result.unwrap_err().contains("projection inspected"));
    mcp_scope::remove_scope_config(&session);
    crate::agents::launch::forget_launch_env(&session);
    if let Some(client) = app.state::<SessionServerState>().connected() {
        client.sever();
    }
}

#[test]
fn respawn_attribution_keeps_captured_in_process_host_after_real_reap() {
    check_respawn_attribution(true);
}

#[test]
fn respawn_attribution_refuses_capture_on_captured_old_sidecar_after_real_reap() {
    check_respawn_attribution(false);
}
