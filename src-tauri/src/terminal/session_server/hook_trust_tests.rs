// SPDX-License-Identifier: MIT
//! Saved-Agent attribution decisions use the capability received from the selected host.
#![cfg(test)]
use super::*;
use crate::agents::launch::{forget_launch_env, project_and_apply_for_cli};
use crate::agents::resolve::tests::alice;
use crate::attribution::prefs::{AttributionMode, AttributionPrefs};
use crate::attribution::testing::Launch;
use crate::attribution::{AttributionStatus, OffReason};
use crate::commands::{mcp_scope, pty_backend};
use codesense_pty_core::agent_cli::{descriptor, AgentCli};
use tauri::Manager;

struct HostHandshake {
    root: tempfile::TempDir,
    server: Option<std::thread::JoinHandle<()>>,
    app: tauri::App<tauri::test::MockRuntime>,
    _runtime: tokio::runtime::Runtime,
}

impl HostHandshake {
    fn new(supported: bool) -> Self {
        let root = short_root();
        let listener = UnixListener::bind(socket_path(root.path())).unwrap();
        let mut hello = mock_hello(instance_id(root.path()));
        if supported {
            hello.capabilities.push(Capability::AttributionHookTrustV1);
        }
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            greet_client(&stream, &hello, 0);
            while let Some((rid, request)) = next_call(&stream) {
                assert!(matches!(request, Request::Listen));
                answer(&stream, rid, Ok(Reply::Done));
            }
        });
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(Arc::new(TerminalHost::new(
            ServerIdentity::ephemeral("attribution-test"),
            Arc::new(RecordingSink::default()),
            runtime.handle().clone(),
        )));
        app.manage(SessionServerState::at(Some(root.path().to_path_buf())));
        Self {
            root,
            server: Some(server),
            app,
            _runtime: runtime,
        }
    }

    fn preconnect(&self) {
        self.app
            .state::<SessionServerState>()
            .connect_or_launch(
                self.root.path(),
                Arc::new(RecordingSink::default()),
                no_launch,
            )
            .unwrap();
    }

    fn connected(&self) -> bool {
        self.app.state::<SessionServerState>().connected().is_some()
    }

    fn support_for_spawn(&self, session: &str) -> bool {
        pty_backend::attribution_hook_trust_for_spawn(self.app.handle(), AgentCli::Codex, session)
            .unwrap()
    }
}

impl Drop for HostHandshake {
    fn drop(&mut self) {
        // A failed lazy-resolution assertion must still release the listener thread.
        if !self.connected() {
            self.preconnect();
        }
        if let Some(client) = self.app.state::<SessionServerState>().connected() {
            client.sever();
        }
        if let Some(server) = self.server.take() {
            server.join().unwrap();
        }
    }
}

#[test]
fn saved_agent_hook_trust_follows_host_hello_before_or_during_preparation() {
    let _home = crate::routines::test_support::TempHome::new();
    for supported in [false, true] {
        for already_connected in [false, true] {
            let host = HostHandshake::new(supported);
            if already_connected {
                host.preconnect();
            }
            let fixture = Launch::new(AgentCli::Codex);
            let _daemon = supported.then(|| fixture.daemon_running());
            let session = format!("hook-trust-{}", uuid::Uuid::new_v4());
            assert_eq!(host.connected(), already_connected);
            let trust = host.support_for_spawn(&session);
            assert!(
                host.connected(),
                "the production resolver connects before deciding trust"
            );
            assert_eq!(
                host.support_for_spawn(&session),
                trust,
                "duplicate resolution keeps its hello"
            );
            let inventory = mcp_scope::inventory::Fixture::new();
            let cli = inventory
                .launch(&fixture.launch.launch.args)
                .with_attribution_hook_trust(trust);
            let native = fixture.home().join("native-codex");
            std::fs::create_dir_all(&native).unwrap();
            let user_config = "# Keep these exact bytes\nmodel = 'gpt-5'\n[hooks]\nstop = [{ hooks = [{ type = 'command', command = 'echo user-hook' }] }]\n";
            std::fs::write(native.join("config.toml"), user_config).unwrap();
            let (scope, _, applied) = project_and_apply_for_cli(
                &fixture.launch,
                &session,
                &fixture.cwd,
                Some(&native),
                Some(&cli),
                str::to_string,
            )
            .unwrap();
            let mut argv = fixture.launch.launch.args.clone();
            argv.extend(applied.argv(str::to_string));
            assert_eq!(argv.iter().any(|arg| arg.contains("checkpoint")), supported);
            assert_eq!(
                scope
                    .extra_env
                    .iter()
                    .any(|(key, _)| key == "GIT_TRACE2_EVENT"),
                supported
            );
            if supported {
                assert_eq!(fixture.status(), Some(AttributionStatus::Active));
            } else {
                assert_eq!(fixture.status(), Some(AttributionStatus::Error {
                    message: "The terminal server is outdated; restart it to enable line attribution.".into(),
                }));
                assert!(
                    fixture.calls().is_empty(),
                    "old hosts never configure/start git-ai"
                );
                assert!(
                    !fixture.log().exists(),
                    "old hosts do not even detect git-ai"
                );
            }
            assert_eq!(
                std::fs::read_to_string(native.join("config.toml")).unwrap(),
                user_config
            );
            mcp_scope::remove_scope_config(&session);
            forget_launch_env(&session);
        }
    }
}

#[test]
fn saved_agent_restart_rechecks_host_hello_and_removes_checkpoint_wiring() {
    let _home = crate::routines::test_support::TempHome::new();
    let fixture = Launch::new(AgentCli::Codex);
    let _daemon = fixture.daemon_running();
    let inventory = mcp_scope::inventory::Fixture::new();
    let mut argv = fixture.launch.launch.args.clone();
    let session = format!("hook-trust-restart-{}", uuid::Uuid::new_v4());
    for supported in [true, false, true] {
        let host = HostHandshake::new(supported);
        let cli = inventory
            .launch(&argv)
            .with_attribution_hook_trust(host.support_for_spawn(&session));
        let (scope, _) = crate::commands::mcp_authoring::ScopeLaunch {
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
            &argv,
        )
        .unwrap();
        if let Some(remove) = scope.remove_args.as_ref() {
            // Relaunch removes the exact owned argument subsequences named by the edit.
            for group in remove.chunks_exact(2) {
                if let Some(at) = argv.windows(group.len()).position(|part| part == group) {
                    argv.drain(at..at + group.len());
                }
            }
        }
        argv.extend(scope.extra_args);
        assert_eq!(argv.iter().any(|arg| arg.contains("checkpoint")), supported);
        assert_eq!(
            scope
                .extra_env
                .iter()
                .any(|(key, _)| key == "GIT_TRACE2_EVENT"),
            supported
        );
        mcp_scope::remove_scope_config(&session);
    }
    // User Off wins even when the replacement host is old.
    fixture.set_prefs(AttributionPrefs {
        mode: Some(AttributionMode::Off),
        ..Default::default()
    });
    let host = HostHandshake::new(false);
    let cli = inventory
        .launch(&argv)
        .with_attribution_hook_trust(host.support_for_spawn(&session));
    let request = crate::attribution::launch::Request {
        codesense_home: fixture.home(),
        principal: &alice(),
        cwd: &fixture.cwd,
        runtime: AgentCli::Codex,
    };
    crate::agents::launch::refresh_attribution(
        &request,
        &session,
        &argv,
        cli.attribution_hook_trust,
    )
    .unwrap();
    assert_eq!(
        fixture.status(),
        Some(AttributionStatus::Off {
            reason: OffReason::DisabledForRepo
        })
    );
    forget_launch_env(&session);
}

#[test]
fn running_host_advertises_attribution_hook_trust() {
    let host = TestServer::start();
    host.host().start_activity(host.root()).unwrap();
    let client = SessionServerState::default()
        .connect_or_launch(host.root(), Arc::new(RecordingSink::default()), no_launch)
        .unwrap();
    assert!(client.supports_attribution_hook_trust());
    client.sever();
}
