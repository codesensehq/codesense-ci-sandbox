// SPDX-License-Identifier: MIT
// InboxWaitV1: real-SSH proof that a remote agent's inbox waiter waits on its
// machine's host and is handed the mail notice this Mac pushes.
//
// Over the owned Docker machine, with the real private terminal server: an agent (a
// script typed in a session's shell, observed by the host as a typed CLI is) runs the
// host helper's `--inbox-wait`, as Claude runs its hook, naming itself as the agent
// process. This Mac reads the machine's waiters and sees it waiting, while no MCP
// stream was opened for it.
//
// This Mac pushes a notice: the helper prints it and exits 2, and the host acks it to
// this Mac. A repeated push of the same notice is acked again, and nothing waits to be
// woken twice.
//
// Refusals: another desktop's push for the session is refused, and a helper run
// outside the agent's process lineage (with its token) is never admitted.
//
// A Codex (a fake qualified one, typed in another session's shell) has no waiter: it runs
// on its engine, which its host starts a turn on with the pushed notice once it is idle;
// while it works, its next tool call's check hands the next one over. Nothing is ever
// typed into its terminal.

#[test]
#[ignore = "owned real SSH: a remote inbox waiter waits on its host and wakes on a pushed notice (InboxWaitV1)"]
fn remote_inbox_wait_over_real_ssh() {
    use crate::commands::host_connection::RemoteTransportOwner;
    use crate::terminal::session_server::{Ledger, SessionServerClient};
    use codesense_pty_core::domain::{PtySessionId, TerminalDomain, TerminalDomainEvent};
    use codesense_pty_core::spawn::SpawnIntent;
    use codesense_session_server::testing::{registration, RecordingSink};
    docker_as_root("command -v python3 >/dev/null || (apt-get update -qq && apt-get install -y -qq --no-install-recommends python3 >/dev/null)");
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let store = FakeStore::default();
    let mut environment = env(root.path());
    environment.registry = Registry::new(environment.codesense_home.clone(), Principal::from(41));
    let (authority, _retire) = crate::commands::auth::private_authority::test_snapshot(41);
    environment.private_authority = Some(authority);
    let params: crate::mcp::machine_tools::DraftOperationParams = serde_json::from_value(serde_json::json!({"input":{"mode":"private-account","label":"Inbox wait fixture","host":"127.0.0.1","port":var("CODESENSE_REMOTE_HOST_PORT").parse::<u16>().unwrap(),"username":"dev","signIn":{"method":"keyFile","keyPath":var("CODESENSE_REMOTE_HOST_KEY")}}})).unwrap();
    let mut draft: MachineDraftInput = params.input.into();
    draft.trusted_host_key_fp = test_connection(&environment, &store, None, &draft)
        .unwrap()
        .steps[0]
        .fingerprint
        .clone();
    let saved = save(&environment, &store, None, &draft, &Scope::Personal, false).unwrap();
    private_setup::enroll(&environment, &store, Some(&saved.machine_id), &draft).unwrap();
    install_server(&environment, &store, Some(&saved.machine_id), &draft).unwrap();
    save(
        &environment,
        &store,
        Some(&saved.machine_id),
        &draft,
        &Scope::Personal,
        true,
    )
    .unwrap();
    private_setup::set_remote_mcp_control(&environment, &saved.machine_id, true).unwrap();
    let environment = Arc::new(environment);
    let marker = uuid::Uuid::new_v4().simple().to_string();
    let dir = format!("/tmp/inbox-wait-{marker}");
    docker_as_dev(&format!("mkdir -m700 '{dir}'"));
    // The agent: it runs its inbox waiter as Claude runs the hook, naming itself as
    // the waiter's agent process, and records how the waiter ended.
    let script = r#"import json,os,subprocess,sys,time
d=sys.argv[1]
e={k:os.environ[k] for k in ("CODESENSE_ACTIVITY_HOST","CODESENSE_ACTIVITY_SOCKET","CODESENSE_ACTIVITY_TOKEN")}
open(d+"/agent.json","w").write(json.dumps(e))
wait=[e["CODESENSE_ACTIVITY_HOST"]+"/activity-helper","--inbox-wait","claude"]
p=subprocess.Popen(wait+["Stop",str(os.getpid())],stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
while not os.path.exists(d+"/rearm"): time.sleep(0.2)
# A tool call's re-arm by the same agent while its waiter waits: answered at once.
t=time.time()
q=subprocess.run(wait+["PostToolUse",str(os.getpid())],stdin=subprocess.DEVNULL,capture_output=True,text=True,timeout=30)
open(d+"/rearm.json","w").write(json.dumps({"code":q.returncode,"seconds":time.time()-t}))
out,err=p.communicate()
open(d+"/result.json","w").write(json.dumps({"code":p.returncode,"stderr":err}))
time.sleep(600)
"#;
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(script);
    docker_as_dev(&format!(
        "printf '%s' '{encoded}' | base64 -d > '{dir}/agent.py'"
    ));
    let file = |name: &str| container_file(&format!("{dir}/{name}"));
    let wait_for = |name: &str| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while file(name).is_empty() {
            assert!(std::time::Instant::now() < deadline, "{name} missing");
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    };
    // However the proof ends, the agent does not outlive it.
    struct Agent(String);
    impl Drop for Agent {
        fn drop(&mut self) {
            let _ = Command::new("docker")
                .args([
                    "exec",
                    &var("CODESENSE_REMOTE_HOST_CONTAINER"),
                    "pkill",
                    "-f",
                    &self.0,
                ])
                .status();
        }
    }
    let _agent = Agent(format!("{dir}/agent.py"));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let connect = |desktop: &'static str| {
            let (environment, store, machine) =
                (environment.clone(), &store, saved.machine_id.clone());
            async move {
                let transport = open_pinned_saved(&environment, store, &machine).unwrap();
                let admitted = RemoteTransportOwner::from_pinned(transport)
                    .into_pending()
                    .admit(
                        std::time::Duration::from_secs(5),
                        tokio_util::sync::CancellationToken::new(),
                    )
                    .await
                    .unwrap();
                let sink = Arc::new(RecordingSink::default());
                let managed = SessionServerClient::from_remote_admitted(
                    admitted,
                    sink.clone(),
                    Arc::<Ledger>::default(),
                    None,
                )
                .await
                .unwrap();
                let namespace = managed.domain_client().namespace().clone();
                let manager = crate::commands::host_connection_manager::test_adopt_real_private_client(&machine, managed).await;
                let target = manager.target_for_namespace(&namespace).unwrap();
                let client = manager.capture_admitted_backend(&target).unwrap().client();
                // The desktop says who it is, as the app does on connect.
                let secret = format!(
                    "{}{}",
                    uuid::Uuid::new_v4().simple(),
                    uuid::Uuid::new_v4().simple()
                );
                client.accept_routine_bindings(
                    secret.clone(),
                    Arc::new(|_: &str| None),
                    tokio::runtime::Handle::current(),
                );
                client.routine_arm_bind_secret(&secret, Some(desktop)).unwrap();
                (manager, client, sink)
            }
        };
        let (manager, client, sink) = connect("inbox-mac.41").await;
        assert!(client.supports_inbox_wait(), "the host runs inbox waiters");
        let session = PtySessionId::new(format!("inbox-{marker}"));
        // Every MCP stream this Mac is asked to serve for the session is counted.
        let streams = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let opened = streams.clone();
        client
            .prepare_reverse_mcp(
                &session,
                format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple()),
                Arc::new(move |_stream| {
                    opened.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }),
            )
            .unwrap();
        client
            .spawn(
                SpawnIntent {
                    remote_launch: Default::default(),
                    // Typed in the tab's shell, as a person types `claude`: the host
                    // names it as the session's agent. Not the shell's last command,
                    // which a shell runs in its own place: the shell stays above it.
                    command: Some(format!(
                        "\"$CODESENSE_ACTIVITY_HOST/activity-helper\" --activity-run claude -- python3 '{dir}/agent.py' '{dir}'; sleep 600"
                    )),
                    argv: None,
                    shell: Some("/bin/sh".into()),
                    cwd: "/home/dev".into(),
                    cols: 200,
                    rows: 40,
                    shell_integration: false,
                    starship_preset: None,
                    api_port: None,
                    env_remove: vec![],
                    env: vec![],
                    agent_env: Vec::new(),
                    login_path: None,
                },
                registration(session.as_str(), None, false),
            )
            .unwrap();
        wait_for("agent.json");

        // The waiter waits on the host, and its desktop sees it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let waiters = client.inbox_waiters().unwrap();
            if waiters.iter().any(|w| w.session == session && w.waiting) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "no waiter: {waiters:?}");
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        // AC2: waiting holds no MCP stream of this Mac's.
        assert_eq!(streams.load(std::sync::atomic::Ordering::SeqCst), 0);
        // The waiter is the agent's own: the same agent's re-arm defers to it at once.
        docker_as_dev(&format!("touch '{dir}/rearm'"));
        wait_for("rearm.json");
        let rearm: serde_json::Value = serde_json::from_str(&file("rearm.json")).unwrap();
        assert_eq!(rearm["code"], 0, "{rearm}");
        assert!(rearm["seconds"].as_f64().unwrap() < 5.0, "{rearm}");
        assert!(file("result.json").is_empty(), "the re-arm woke nothing");

        // Another desktop's push for the session is refused.
        let (other_manager, other, _other_sink) = connect("other-mac.42").await;
        assert!(other.inbox_mail(&session, "You have 9 new messages.", "other-1").is_err());
        assert!(file("result.json").is_empty(), "nothing woke the agent");

        // This Mac's push: printed by the waiter, which wakes the agent (exit 2), and
        // acked back once printed.
        let notice = format!(
            "You have 1 new message. Call read_messages with session_id {}.",
            session.as_str()
        );
        client.inbox_mail(&session, &notice, "tok-1").unwrap();
        wait_for("result.json");
        let result: serde_json::Value = serde_json::from_str(&file("result.json")).unwrap();
        assert_eq!(result["code"], 2, "{result}");
        assert_eq!(result["stderr"].as_str().unwrap().trim_end(), notice);
        let acks = || {
            sink.events()
                .into_iter()
                .filter(|event| {
                    matches!(event, TerminalDomainEvent::InboxAck { session: acked, token } if *acked == session && token == "tok-1")
                })
                .count()
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while acks() == 0 {
            assert!(std::time::Instant::now() < deadline, "no ack: {:?}", sink.events());
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        // The same notice pushed again (its ack lost): acked again, not held to wake
        // the agent twice.
        client.inbox_mail(&session, &notice, "tok-1").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while acks() < 2 {
            assert!(std::time::Instant::now() < deadline, "no re-ack: {:?}", sink.events());
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        // A helper outside the agent's process lineage is never admitted, even with
        // the agent's token: the host has no waiter for the session (the agent's has
        // ended).
        let agent: serde_json::Value = serde_json::from_str(&file("agent.json")).unwrap();
        let env = |key: &str| agent[key].as_str().unwrap().to_string();
        docker_as_dev(&format!(
            "test -x '{host}/activity-helper' && (CODESENSE_ACTIVITY_SOCKET='{}' CODESENSE_ACTIVITY_TOKEN='{}' timeout 6 '{host}/activity-helper' --inbox-wait claude Stop $$ </dev/null >/dev/null 2>&1 &)",
            env("CODESENSE_ACTIVITY_SOCKET"),
            env("CODESENSE_ACTIVITY_TOKEN"),
            host = env("CODESENSE_ACTIVITY_HOST"),
        ));
        let watch = std::time::Instant::now() + std::time::Duration::from_secs(4);
        while std::time::Instant::now() < watch {
            let waiters = client.inbox_waiters().unwrap();
            assert!(
                !waiters.iter().any(|w| w.session == session && w.waiting),
                "an outsider took the session's place: {waiters:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        assert_eq!(streams.load(std::sync::atomic::Ordering::SeqCst), 0);
        let _ = client.kill(&session);

        // A Codex: typed in a shell, observed by its host, which prepares its hooks and runs
        // it on its engine.
        let fake = base64::engine::general_purpose::STANDARD.encode(include_str!(
            "../../../crates/codesense-session-server/tests/fixtures/fake-codex.py"
        ));
        docker_as_dev(&format!(
            "printf '%s' '{fake}' | base64 -d > '{dir}/codex' && chmod 700 '{dir}/codex' && : > '{dir}/launcher'"
        ));
        // However the proof ends, the fake Codex does not outlive it either.
        let _codex_agent = Agent(format!("{dir}/codex"));
        let codex = PtySessionId::new(format!("inbox-codex-{marker}"));
        // Its desktop serves its MCP, as for any agent tab it opens: its inbox reports go
        // to that desktop.
        client
            .prepare_reverse_mcp(
                &codex,
                format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple()),
                Arc::new(|_stream| {}),
            )
            .unwrap();
        client
            .spawn(
                SpawnIntent {
                    remote_launch: Default::default(),
                    command: Some(format!(
                        "\"$CODESENSE_ACTIVITY_HOST/activity-helper\" --activity-run codex -- '{dir}/codex' -- hi; sleep 600"
                    )),
                    argv: None,
                    shell: Some("/bin/sh".into()),
                    cwd: "/home/dev".into(),
                    cols: 200,
                    rows: 40,
                    shell_integration: false,
                    starship_preset: None,
                    api_port: None,
                    env_remove: vec![],
                    env: vec![],
                    agent_env: Vec::new(),
                    login_path: None,
                },
                registration(codex.as_str(), None, false),
            )
            .unwrap();
        wait_for("ready");
        // Idle at its prompt after its first turn, it reads as one new mail reaches.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let waiters = client.inbox_waiters().unwrap();
            if waiters.iter().any(|w| w.session == codex && w.waiting) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "Codex never reached: {waiters:?}");
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        let fate_of = |token: &'static str| {
            sink.events().into_iter().find_map(|event| match event {
                TerminalDomainEvent::InboxAck { session, token: t } if session == codex && t == token => Some("told"),
                TerminalDomainEvent::InboxRelease { session, token: t } if session == codex && t == token => Some("released"),
                TerminalDomainEvent::InboxUncertain { session, token: t } if session == codex && t == token => Some("uncertain"),
                _ => None,
            })
        };
        let until_fate = |token: &'static str| async move {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            loop {
                if let Some(fate) = fate_of(token) {
                    return fate;
                }
                assert!(std::time::Instant::now() < deadline, "no fate for {token}");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        };
        // Idle: started a turn with the notice, headed by its wake key, through its engine,
        // and told once the engine took it.
        let idle = format!(
            "You have 1 new message. Call read_messages with session_id {}.",
            codex.as_str()
        );
        client.inbox_mail(&codex, &idle, "codex-1").unwrap();
        assert_eq!(until_fate("codex-1").await, "told");
        wait_for("prompt");
        assert_eq!(file("prompt"), format!("[inbox codex-1]\n{idle}"));
        // Working on that turn: no turn is started over it; its next tool call's check hands
        // it over, as context.
        let checked = format!(
            "You have 2 new messages. Call read_messages with session_id {}.",
            codex.as_str()
        );
        client.inbox_mail(&codex, &checked, "codex-2").unwrap();
        docker_as_dev(&format!("touch '{dir}/finish-tool'"));
        assert_eq!(until_fate("codex-2").await, "told");
        docker_as_dev(&format!("touch '{dir}/finish-tool-2'"));
        wait_for("stopped");
        let handed: Vec<serde_json::Value> = file("hooks.log")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .filter(|hook: &serde_json::Value| !hook["stdout"].as_str().unwrap().is_empty())
            .collect();
        assert_eq!(handed.len(), 1, "{handed:?}");
        assert_eq!(handed[0]["event"], "PostToolUse");
        assert_eq!(handed[0]["agent"], serde_json::Value::Null, "not to its subagent");
        let output: serde_json::Value =
            serde_json::from_str(handed[0]["stdout"].as_str().unwrap()).unwrap();
        assert_eq!(output["hookSpecificOutput"]["additionalContext"], checked.as_str());
        assert_eq!(file("prompt"), format!("[inbox codex-1]\n{idle}"));
        // Mail after its last tool call goes as a turn of its own once the turn has ended.
        let after = format!(
            "You have 3 new messages. Call read_messages with session_id {}.",
            codex.as_str()
        );
        client.inbox_mail(&codex, &after, "codex-3").unwrap();
        assert_eq!(until_fate("codex-3").await, "told");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while file("prompt") != format!("[inbox codex-3]\n{after}") {
            assert!(std::time::Instant::now() < deadline, "the turn-end notice never started a turn");
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        // Its TUI ran attached to its engine, and nothing was typed into its terminal.
        assert!(file("tui").contains("--remote unix://"), "{}", file("tui"));
        assert_eq!(file("typed"), "");
        let _ = client.kill(&codex);
        println!(
            "REMOTE_INBOX_WAIT_PASS machine={} session={} waited_on_host=true streams=0 woke_exit={} acks={} other_desktop=refused outside_lineage=refused codex_idle=told codex_checked=told codex_turn_end=told",
            saved.machine_id,
            session.as_str(),
            result["code"],
            acks(),
        );
        other_manager.disconnect(&saved.machine_id).await.unwrap();
        manager.disconnect(&saved.machine_id).await.unwrap();
    });
}
