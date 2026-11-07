// SPDX-License-Identifier: MIT
// SW-AGR-11: real-SSH proof that a remote agent keeps its CodeSense MCP
// across a desktop disconnect.
//
// Over the owned Docker machine, with the real private terminal server and
// the installed bridge: an agent (a script driving the bridge from its
// session's `--mcp-config`, as the CLI does) calls a desktop tool through
// this Mac. This Mac's link is killed. The same agent process, its bridge
// never restarted, then lists its timers (answered by the machine), queues a
// message, and gets a typed `desktop_offline` for a context-repo tool. The
// link is restarted (a new connection, as a relaunched app makes): the
// machine delivers the queued message once, under its outbox id, and hands
// the agent's stream back; the agent's next call reaches this Mac again.
// Nothing it called before is replayed.

#[test]
#[ignore = "owned real SSH: the bridge survives a desktop disconnect and reattaches (SW-AGR-11)"]
fn remote_bridge_reconnect_over_real_ssh() {
    use crate::commands::host_connection::RemoteTransportOwner;
    use crate::terminal::session_server::{Ledger, SessionServerClient};
    use codesense_pty_core::domain::{PtySessionId, TerminalDomain};
    use codesense_pty_core::spawn::SpawnIntent;
    use codesense_session_server::testing::{registration, RecordingSink};
    docker_as_root("command -v python3 >/dev/null || (apt-get update -qq && apt-get install -y -qq --no-install-recommends python3 >/dev/null)");
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let store = FakeStore::default();
    let mut environment = env(root.path());
    environment.registry = Registry::new(environment.codesense_home.clone(), Principal::from(41));
    let (authority, _retire) = crate::commands::auth::private_authority::test_snapshot(41);
    environment.private_authority = Some(authority);
    let params: crate::mcp::machine_tools::DraftOperationParams = serde_json::from_value(serde_json::json!({"input":{"mode":"private-account","label":"Bridge reconnect fixture","host":"127.0.0.1","port":var("CODESENSE_REMOTE_HOST_PORT").parse::<u16>().unwrap(),"username":"dev","signIn":{"method":"keyFile","keyPath":var("CODESENSE_REMOTE_HOST_KEY")}}})).unwrap();
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
    let dir = format!("/tmp/agr11-{marker}");
    docker_as_dev(&format!("mkdir -m700 '{dir}'"));
    // The agent: one process, one bridge, for the whole proof.
    let script = r#"import json,os,subprocess,sys,time
d=sys.argv[1]; session=sys.argv[2]
c=json.load(open(os.environ["CODESENSE_MCP_CONFIG"]))["mcpServers"]["codesense"]
p=subprocess.Popen([c["command"]]+c["args"],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True,close_fds=True,env={"PATH":"/usr/bin:/bin"})
log=open(d+"/log.jsonl","a")
def call(i,method,params):
    p.stdin.write(json.dumps({"jsonrpc":"2.0","id":i,"method":method,"params":params})+"\n"); p.stdin.flush()
    while True:
        line=p.stdout.readline()
        if not line:
            log.write(json.dumps({"id":i,"closed":True,"bridge_exit":p.poll()})+"\n"); log.flush(); sys.exit(1)
        m=json.loads(line)
        if m.get("id")==i:
            log.write(json.dumps({"id":i,"answer":m,"bridge_pid":p.pid,"bridge_exit":p.poll()})+"\n"); log.flush(); return m
def tool(i,name,args): return call(i,"tools/call",{"name":name,"arguments":args})
def step(name): open(d+"/"+name,"w").close()
def wait(name):
    while not os.path.exists(d+"/"+name): time.sleep(0.2)
call(0,"initialize",{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"agr11","version":"1"}})
p.stdin.write(json.dumps({"jsonrpc":"2.0","method":"notifications/initialized"})+"\n"); p.stdin.flush()
tool(1,"list_machines",{})
step("connected"); wait("away")
tool(2,"list_wakeups",{"sessionId":session})
tool(3,"send_message",{"sessionId":session,"to":"lead","text":"AGR11_QUEUED"})
tool(4,"hydrate_worktree",{})
step("offline"); wait("back")
tool(5,"list_machines",{})
step("done")
time.sleep(600)
"#;
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(script);
    docker_as_dev(&format!("printf '%s' '{encoded}' | base64 -d > '{dir}/agent.py'"));
    let wait_for = |name: &str| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while container_file(&format!("{dir}/{name}")).is_empty()
            && std::process::Command::new("docker")
                .args(["exec", &var("CODESENSE_REMOTE_HOST_CONTAINER"), "test", "-e", &format!("{dir}/{name}")])
                .status()
                .map(|s| !s.success())
                .unwrap_or(true)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "agent step {name} missing; log: {}",
                container_file(&format!("{dir}/log.jsonl"))
            );
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    };
    let log = || -> Vec<serde_json::Value> {
        container_file(&format!("{dir}/log.jsonl"))
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let connect = || async {
            let transport = open_pinned_saved(&environment, &store, &saved.machine_id).unwrap();
            let admitted = RemoteTransportOwner::from_pinned(transport).into_pending().admit(std::time::Duration::from_secs(5), tokio_util::sync::CancellationToken::new()).await.unwrap();
            let managed = SessionServerClient::from_remote_admitted(admitted, Arc::new(RecordingSink::default()), Arc::<Ledger>::default(), None).await.unwrap();
            let namespace = managed.domain_client().namespace().clone();
            let manager = crate::commands::host_connection_manager::test_adopt_real_private_client(&saved.machine_id, managed).await;
            let target = manager.target_for_namespace(&namespace).unwrap();
            let client = manager.capture_admitted_backend(&target).unwrap().client();
            (manager, client)
        };
        let (manager, client) = connect().await;
        let session = PtySessionId::new(format!("agr11-{marker}"));
        // This Mac's CodeSense MCP, as far as the proof needs one: every
        // request is answered with which connection served it, and every
        // request seen is recorded.
        let first_seen = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let serve = recording_desktop("FIRST_DESKTOP", first_seen.clone());
        // This Mac says who it is, as the app does on connect: the agents it
        // relays come back to it, and its link is reaped if it goes silent.
        let first_secret = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        client.accept_routine_bindings(first_secret.clone(), Arc::new(|_: &str| None), tokio::runtime::Handle::current());
        client.routine_arm_bind_secret(&first_secret, Some("agr11-mac.41")).unwrap();
        client
            .prepare_reverse_mcp(&session, format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple()), Arc::new(move |stream| serve(String::new(), stream)))
            .unwrap();
        client
            .spawn(
                SpawnIntent {
                    remote_launch: Default::default(),
                    command: Some(format!("python3 '{dir}/agent.py' '{dir}' '{}'", session.as_str())),
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
        wait_for("connected");
        let timer = client
            .wake_timer_set(codesense_pty_core::session_wake::TimerSpec {
                session: session.clone(),
                schedule: codesense_pty_core::session_wake::TimerSchedule::After {
                    after_ms: 3_600_000,
                    every_ms: None,
                },
                until_ms: None,
                count: None,
                text: "AGR11 check CI".into(),
                name: "agr11".into(),
                created_by: "agent:agr11".into(),
                max_wait_ms: 60_000,
                originator: Default::default(),
                require_observed: false,
                min_gap_ms: None,
            })
            .unwrap();

        // The link is killed mid-session.
        drop(client);
        manager.disconnect(&saved.machine_id).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        docker_as_dev(&format!("touch '{dir}/away'"));
        wait_for("offline");

        // The link is restarted: a new connection, this Mac's routine secret.
        let (back_manager, back) = connect().await;
        let second_seen = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let serve = recording_desktop("SECOND_DESKTOP", second_seen.clone());
        let binder: crate::terminal::session_server::RoutineBinder =
            Arc::new(move |_session: &str| Some(serve.clone()));
        let secret = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        back.accept_routine_bindings(secret.clone(), binder, tokio::runtime::Handle::current());
        back.routine_arm_bind_secret(&secret, Some("agr11-mac.41")).unwrap();
        // The outbox is delivered, and the agent's stream handed back, before
        // the agent calls again.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !second_seen.lock().unwrap().iter().any(|m| m["id"] == "codesense-reattach") {
            assert!(std::time::Instant::now() < deadline, "no reattach: {:?}", second_seen.lock().unwrap());
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        docker_as_dev(&format!("touch '{dir}/back'"));
        wait_for("done");

        let entries = log();
        let by_id = |id: u64| entries.iter().find(|e| e["id"] == id).cloned().unwrap_or_else(|| panic!("no answer {id}: {entries:?}"));
        let text_of = |entry: &serde_json::Value| entry["answer"]["result"]["content"][0]["text"].as_str().unwrap_or_default().to_string();
        // Connected: relayed to this Mac.
        assert_eq!(text_of(&by_id(1)), "FIRST_DESKTOP:list_machines", "{entries:?}");
        // AC1: away, the machine answers the session's own timers.
        let listed: serde_json::Value = serde_json::from_str(&text_of(&by_id(2))).unwrap();
        assert_eq!(listed["timers"][0]["timerId"], timer.timer_id.as_str(), "{entries:?}");
        // Queued, and typed desktop_offline for a context-repo tool.
        let queued: serde_json::Value = serde_json::from_str(&text_of(&by_id(3))).unwrap();
        assert_eq!(queued["data"]["queued"], true, "{entries:?}");
        assert_eq!(by_id(4)["answer"]["error"]["data"]["type"], "desktop_offline", "{entries:?}");
        // AC2: the same agent's next call reaches this Mac again.
        assert_eq!(text_of(&by_id(5)), "SECOND_DESKTOP:list_machines", "{entries:?}");
        // One bridge throughout, still running.
        let pids: std::collections::BTreeSet<String> = entries.iter().map(|e| e["bridge_pid"].to_string()).collect();
        assert_eq!(pids.len(), 1, "one bridge: {entries:?}");
        assert!(entries.iter().all(|e| e["bridge_exit"].is_null()), "{entries:?}");
        // The queued message reached this Mac once, under its outbox id;
        // nothing the agent called before the disconnect was replayed.
        let second = second_seen.lock().unwrap().clone();
        let delivered: Vec<_> = second.iter().filter(|m| m["params"]["name"] == "send_message").collect();
        assert_eq!(delivered.len(), 1, "{second:?}");
        assert_eq!(delivered[0]["params"]["arguments"]["text"], "AGR11_QUEUED");
        let outbox_id = delivered[0]["params"]["_meta"]["codesenseOutboxId"].as_str().unwrap().to_string();
        assert_eq!(queued["data"]["outboxId"], outbox_id.as_str());
        let calls: Vec<_> = second.iter().filter(|m| m["method"] == "tools/call").map(|m| m["params"]["name"].as_str().unwrap_or_default().to_string()).collect();
        assert_eq!(calls, ["send_message", "list_machines"], "{second:?}");
        let first = first_seen.lock().unwrap().clone();
        assert!(!first.iter().any(|m| m["params"]["name"] == "send_message"), "{first:?}");
        println!(
            "REMOTE_BRIDGE_RECONNECT_PASS machine={} session={} connected={} away_list_wakeups={} away_send_message=queued({outbox_id}) away_hydrate_worktree={} back={} bridge_pid={} delivered_once={} replayed=none",
            saved.machine_id,
            session.as_str(),
            text_of(&by_id(1)),
            listed["timers"][0]["timerId"],
            by_id(4)["answer"]["error"]["data"]["type"],
            text_of(&by_id(5)),
            entries[0]["bridge_pid"],
            delivered.len(),
        );
        let _ = back.kill(&session);
        back_manager.disconnect(&saved.machine_id).await.unwrap();
    });
}

/// A desktop CodeSense MCP that answers every request `<name>:<tool or
/// method>` and records each request it sees.
fn recording_desktop(
    name: &'static str,
    seen: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
) -> Arc<dyn Fn(String, tokio::io::DuplexStream) + Send + Sync> {
    Arc::new(move |_server: String, stream: tokio::io::DuplexStream| {
        let seen = seen.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
            let (read, mut write) = tokio::io::split(stream);
            let mut lines = tokio::io::BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };
                seen.lock().unwrap().push(message.clone());
                let Some(id) = message.get("id").filter(|id| !id.is_null()) else {
                    continue;
                };
                let what = message["params"]["name"]
                    .as_str()
                    .or_else(|| message["method"].as_str())
                    .unwrap_or_default()
                    .to_string();
                let answer = serde_json::json!({"jsonrpc":"2.0","id":id,"result":{
                    "protocolVersion":"2024-11-05","capabilities":{"tools":{}},
                    "serverInfo":{"name":name,"version":"1"},
                    "content":[{"type":"text","text":format!("{name}:{what}")}],"isError":false}});
                if write.write_all(format!("{answer}\n").as_bytes()).await.is_err() {
                    return;
                }
            }
        });
    })
}
