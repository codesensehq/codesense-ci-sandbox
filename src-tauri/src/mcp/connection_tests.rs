// SPDX-License-Identifier: MIT
use super::{caller_tools, handler::*, local_identity};
use crate::commands::agent_inbox::{
    self,
    tests::{port, FakeHost},
    InboxHost,
};
use codesense_pty_core::activity::peer::{self, PeerProcess};
use rmcp::model::GetMeta;
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

/// What the terminal server answers for a live tab: its root, or no process to read
/// (a relay-proven caller's tab).
fn lookup(root: Option<PeerProcess>) -> local_identity::Lookup {
    root.map_or(
        local_identity::Lookup::NoRoot(crate::commands::pty_backend::Liveness::Known(true)),
        local_identity::Lookup::Root,
    )
}

fn initialize(
    state: &RwLock<Option<local_identity::Binding>>,
    session: &str,
    peer: PeerProcess,
    root: Option<PeerProcess>,
) {
    initialize_wire(
        state,
        json!({
            "jsonrpc":"2.0", "id":1, "method":"initialize", "params":{
                "protocolVersion":"2025-06-18", "capabilities":{},
                "clientInfo":{"name":"fixture", "version":"1"},
                "_meta":{"codesenseSessionId":session}
            }
        }),
        peer,
        root,
    );
}

fn initialize_wire(
    state: &RwLock<Option<local_identity::Binding>>,
    wire: Value,
    peer: PeerProcess,
    root: Option<PeerProcess>,
) {
    let wire: rmcp::model::ClientJsonRpcMessage = serde_json::from_value(wire).unwrap();
    let rmcp::model::ClientJsonRpcMessage::Request(request) = wire else {
        panic!("request")
    };
    let captured = local_identity::capture(
        initialize_session(None, request.request.get_meta()),
        Some(peer),
        lookup(root),
    )
    .unwrap();
    local_identity::note_initialize(state, captured);
}

async fn dispatch(
    host: &Arc<dyn InboxHost>,
    state: &RwLock<Option<local_identity::Binding>>,
    peer: PeerProcess,
    root: Option<PeerProcess>,
    name: &str,
    mut args: Value,
    completion: &std::path::Path,
) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
    let (name, arguments) = if name == "codesense_call_tool" {
        caller_tools::named_tool_request(serde_json::from_value(args).unwrap())?
    } else {
        (name.to_string(), args.as_object_mut().unwrap().clone())
    };
    let connection = local_identity::stored_session(state);
    if let Some(binding) = local_identity::stored(state) {
        local_identity::revalidate(&binding, Some(peer), lookup(root))?;
    }
    let mut arguments = arguments;
    local_identity::arguments(&name, &mut arguments, connection.as_deref(), None)?;
    let caller = local_identity::inbox_caller(None, connection.as_deref())?;
    let args = Value::Object(arguments);
    match name.as_str() {
        "send_message" => {
            caller_tools::send_message(host, caller, serde_json::from_value(args).unwrap()).await
        }
        "reply" => caller_tools::reply(host, caller, serde_json::from_value(args).unwrap()).await,
        "react" => {
            caller_tools::react(&**host, caller, serde_json::from_value(args).unwrap()).await
        }
        "read_messages" => {
            caller_tools::read_messages(&**host, caller, serde_json::from_value(args).unwrap())
                .await
        }
        "signal_done" => {
            caller_tools::signal_done(
                connection,
                serde_json::from_value(args).unwrap(),
                |session, params| async move {
                    let record =
                        json!({"session":session,"status":params.status,"summary":params.summary});
                    let bytes = serde_json::to_vec(&record).unwrap();
                    std::fs::write(completion, bytes).unwrap();
                    ok_json(json!({"recorded":true}))
                },
            )
            .await
        }
        _ => unreachable!(),
    }
}

fn result(result: rmcp::model::CallToolResult) -> Value {
    let text = result.content[0].as_text().unwrap();
    serde_json::from_str(&text.text).unwrap()
}

async fn journey(session: &str, handle: &str, peer: PeerProcess, root: Option<PeerProcess>) {
    let fake = FakeHost::new();
    fake.tab(handle, session, "Fixture", true);
    let host = port(&fake);
    let state = RwLock::new(None);
    initialize(&state, session, peer, root);
    assert_eq!(
        local_identity::stored_session(&state).as_deref(),
        Some(session)
    );
    // Publication uses the stored verified identity too, including the relay bridge
    // whose process is outside the tab's direct ancestry.
    let mut publication = serde_json::Map::new();
    local_identity::arguments(
        "routine_publish_result",
        &mut publication,
        local_identity::stored_session(&state).as_deref(),
        None,
    )
    .unwrap();
    assert_eq!(
        local_identity::session(
            publication["sessionId"].as_str(),
            local_identity::stored_session(&state).as_deref()
        )
        .unwrap(),
        session
    );
    publication.insert("sessionId".into(), "another-run".into());
    assert!(local_identity::arguments(
        "routine_publish_result",
        &mut publication,
        local_identity::stored_session(&state).as_deref(),
        None
    )
    .is_err());
    let output = tempfile::tempdir().unwrap();
    let completion = output.path().join("completion.json");
    for wrapper in [false, true] {
        let call = |name: &str, args: Value| {
            if wrapper {
                (
                    "codesense_call_tool".to_string(),
                    json!({"name":name,"arguments":args}),
                )
            } else {
                (name.to_string(), args)
            }
        };
        let (name, args) = call("send_message", json!({"to":"human","text":"report"}));
        let sent = result(
            dispatch(&host, &state, peer, root, &name, args, &completion)
                .await
                .unwrap(),
        );
        let id = sent["messageId"].as_i64().unwrap();
        assert_eq!(
            fake.rows().iter().find(|m| m.id == id).unwrap().from,
            handle
        );
        let question =
            agent_inbox::send_message(&host, None, handle, "status?", None, None, None, None, None)
                .await
                .unwrap();
        let agent_inbox::SendResult::Sent {
            message_id: question,
            ..
        } = question
        else {
            panic!("sent")
        };
        let (name, args) = call("read_messages", json!({}));
        let read = result(
            dispatch(&host, &state, peer, root, &name, args, &completion)
                .await
                .unwrap(),
        );
        assert!(read["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["messageId"] == question));
        let (name, args) = call("reply", json!({"messageId":question,"text":"green"}));
        let reply = result(
            dispatch(&host, &state, peer, root, &name, args, &completion)
                .await
                .unwrap(),
        );
        let id = reply["messageId"].as_i64().unwrap();
        assert_eq!(
            fake.rows().iter().find(|m| m.id == id).unwrap().from,
            handle
        );
        let (name, args) = call("react", json!({"messageId":question,"emoji":"👍"}));
        let reacted = result(
            dispatch(&host, &state, peer, root, &name, args, &completion)
                .await
                .unwrap(),
        );
        assert_eq!(
            (reacted["status"].as_str(), reacted["outcome"].as_str()),
            (Some("reacted"), Some("added"))
        );
        let reactions = crate::weft::reactions::reactions_all(
            &crate::weft::WeftStore::open(fake.team_store("default")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            reactions
                .iter()
                .filter(|r| r.message_id == question)
                .map(|r| r.reactor.as_str())
                .collect::<Vec<_>>(),
            vec![handle]
        );
        let (name, args) = call(
            "signal_done",
            json!({"status":"success","summary":"checks passed"}),
        );
        assert_eq!(
            result(
                dispatch(&host, &state, peer, root, &name, args, &completion)
                    .await
                    .unwrap()
            )["recorded"],
            true
        );
        let record: Value = serde_json::from_slice(&std::fs::read(&completion).unwrap()).unwrap();
        assert_eq!(record["session"], session);
        assert_eq!(record["status"], "success");
    }
    local_identity::note_initialize(&state, None);
    assert!(dispatch(
        &host,
        &state,
        peer,
        root,
        "send_message",
        json!({"to":"human","text":"wrong"}),
        &completion
    )
    .await
    .is_err());
}

#[tokio::test]
async fn review_r1_process_proven_claude_runs_the_bound_receivers() {
    let (socket, _other) = std::os::unix::net::UnixStream::pair().unwrap();
    journey(
        "S1",
        "H1",
        peer::of_socket(&socket).unwrap(),
        peer::inspect(std::os::unix::process::parent_id()),
    )
    .await;
}

#[tokio::test]
async fn review_r1_relay_spawned_codex_runs_the_bound_receivers() {
    let _home_env = crate::routines::test_support::TempHome::new();
    use crate::mcp::local_mcp_relay::{config, local_grant};
    use codesense_host_proto::scoped_mcp::{self, Hello};
    let parent = crate::commands::mcp_scope::mcp_scope_session_dir("unused")
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::create_dir_all(&parent).unwrap();
    let scope = tempfile::Builder::new()
        .prefix("receiver-relay-")
        .tempdir_in(parent)
        .unwrap();
    let session = scope.path().file_name().unwrap().to_str().unwrap();
    let socket_dir = tempfile::tempdir_in("/tmp").unwrap();
    let global = socket_dir.path().join("global.sock");
    let listener = tokio::net::UnixListener::bind(&global).unwrap();
    let script="import socket,sys,time;sys.stdin.readline();s=socket.socket(socket.AF_UNIX);s.connect(sys.argv[1]);time.sleep(60)";
    let grant=config::parse_local_scope(&json!({"mcp_servers":{"codesense":{"command":"/usr/bin/python3","args":["-c",script,global]}}}),"mcp_servers",scope.path()).unwrap();
    let active = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let flag = active.clone();
    let descriptor = local_grant::start(
        scope.path(),
        grant,
        Default::default(),
        Arc::new(move || {
            if flag.load(std::sync::atomic::Ordering::SeqCst) {
                Ok(())
            } else {
                Err("retired".into())
            }
        }),
    )
    .unwrap();
    scoped_mcp::write_descriptor(&scope.path().join("local-relay.json"), &descriptor).unwrap();
    let hello = serde_json::to_vec(&Hello {
        version: scoped_mcp::VERSION,
        capability: descriptor.capability.clone(),
        server: "codesense".into(),
    })
    .unwrap();
    let mut early = tokio::net::UnixStream::connect(&descriptor.socket)
        .await
        .unwrap();
    early.write_u32(hello.len() as u32).await.unwrap();
    early.write_all(&hello).await.unwrap();
    assert!(
        early.read_u8().await.is_err(),
        "a prepared scope is not a trusted launch root"
    );
    drop(early);
    crate::commands::mcp_scope::bind_fixture_relay_root(
        session,
        peer::inspect(std::process::id()).unwrap(),
    );
    let mut client = tokio::net::UnixStream::connect(&descriptor.socket)
        .await
        .unwrap();
    client.write_u32(hello.len() as u32).await.unwrap();
    client.write_all(&hello).await.unwrap();
    assert_eq!(client.read_u8().await.unwrap(), 1);
    // Like the real bridge, the fixture introduces itself only after the relay can
    // forward input: that boundary guarantees its child registration is installed.
    client
        .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
        .await
        .unwrap();
    let (global_peer, _) =
        tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
    journey(
        session,
        "relay-worker",
        peer::of_socket(&global_peer).unwrap(),
        None,
    )
    .await;
    let caller = peer::of_socket(&global_peer).unwrap();
    let state = RwLock::new(None);
    initialize(&state, session, caller, None);
    let (direct, _other) = std::os::unix::net::UnixStream::pair().unwrap();
    assert!(
        local_identity::capture(
            Some(session.to_string()),
            peer::of_socket(&direct),
            lookup(None)
        )
        .is_err(),
        "a readable valid descriptor cannot bind an unrelated direct peer"
    );
    let descriptor_path = scope.path().join("local-relay.json");
    let rotated = scoped_mcp::Descriptor {
        capability: "d".repeat(64),
        ..descriptor.clone()
    };
    scoped_mcp::write_descriptor(&descriptor_path, &rotated).unwrap();
    let completion = scope.path().join("unused-completion.json");
    for lifecycle in ["rotated", "removed", "retired"] {
        match lifecycle {
            "removed" => std::fs::remove_file(&descriptor_path).unwrap(),
            "retired" => {
                scoped_mcp::write_descriptor(&descriptor_path, &descriptor).unwrap();
                crate::commands::mcp_scope::remove_scope_config(session);
            }
            _ => {}
        }
        assert_eq!(
            local_identity::stored_session(&state).as_deref(),
            Some(session)
        );
        for name in ["send_message", "reply", "read_messages", "signal_done"] {
            assert!(
                dispatch(
                    &port(&FakeHost::new()),
                    &state,
                    caller,
                    None,
                    name,
                    json!({}),
                    &completion
                )
                .await
                .is_err(),
                "cached {name} after {lifecycle}"
            );
        }
    }
    active.store(false, std::sync::atomic::Ordering::SeqCst);
    drop(client);
}

#[test]
fn review_r1_router_wiring_uses_capture_state_binding_and_shared_receivers() {
    let source = include_str!("handler.rs")
        .split_whitespace()
        .collect::<String>();
    assert!(source.contains("self.note_initialize(super::local_identity::capture("));
    assert!(source.contains("super::local_identity::note_initialize(&self.caller_session,binding)"));
    assert!(source.contains("super::local_identity::stored_session(&self.caller_session)"));
    assert!(source.contains("super::local_identity::revalidate(&binding,peer,local_session_lookup(&app,&binding.session),)"), "each bound call revalidates its captured peer");
    // Each accepted socket gets its own handler, so a proven root is never another
    // connection's.
    assert!(include_str!("socket_server.rs")
        .split_whitespace()
        .collect::<String>()
        .contains("lethandler=CodesenseHandler::new(app_handle.clone()).with_local_peer(peer);"));
    assert!(source.contains("caller_session:Arc::default(),"));
    for name in [
        "send_message",
        "read_messages",
        "reply",
        "signal_done",
        "named_tool_request",
    ] {
        assert!(
            source.contains(&format!("super::caller_tools::{name}(")),
            "{name} must use its tested receiver"
        );
    }
    assert!(["self", "handler"]
        .iter()
        .any(|receiver| source.contains(&format!(
            "{receiver}.bind_local_arguments(request.name.as_ref(),&mutarguments).await?"
        ))));
    assert!(source.contains("self.bind_local_arguments(name,&mutbound).await?"));
}

// Python is also used by the existing relay journey above. The admitted process
// connects itself, forks a detached socket holder, then exits on command. The
// test keeps its trusted launch root alive throughout both identity-bearing calls.
const INHERITED_SOCKET_CLIENT: &str = r#"
import json,os,socket,struct,sys
s=socket.socket(socket.AF_UNIX); s.settimeout(10); s.connect(sys.argv[1])
if sys.argv[3]:
    hello=sys.argv[3].encode(); s.sendall(struct.pack('>I',len(hello))+hello)
    assert s.recv(1)==b'\x01'
f=s.makefile('rwb',buffering=0)
def call(value):
    f.write(json.dumps(value).encode()+b'\n')
    return json.loads(f.readline())
call({'jsonrpc':'2.0','id':1,'method':'initialize','params':{
    'protocolVersion':'2025-06-18','capabilities':{},
    'clientInfo':{'name':'fixture','version':'1'},
    '_meta':{'codesenseSessionId':sys.argv[2]}}})
def message(i,text):
    return call({'jsonrpc':'2.0','id':i,'method':'tools/call','params':{
        'name':'send_message','arguments':{'to':'human','text':text}}})
assert 'result' in message(2,'before exit')
print('ready',flush=True); assert sys.stdin.readline()=='fork\n'
if os.fork(): os._exit(0)
os.setsid(); print(os.getpid(),flush=True)
assert sys.stdin.readline()=='call\n'
print(json.dumps(message(3,'after exit')),flush=True)
"#;

async fn inherited_socket_refused(relay: bool) -> bool {
    use crate::mcp::local_mcp_relay::{config, local_grant};
    use codesense_host_proto::scoped_mcp::{self, Hello};
    use std::{
        process::Stdio,
        sync::atomic::{AtomicBool, Ordering},
    };

    let parent = crate::commands::mcp_scope::mcp_scope_session_dir("unused")
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::create_dir_all(&parent).unwrap();
    let scope = tempfile::Builder::new()
        .prefix("inherited-peer-")
        .tempdir_in(parent)
        .unwrap();
    let session = scope.path().file_name().unwrap().to_str().unwrap();
    let root = peer::inspect(std::process::id()).unwrap();
    let sockets = tempfile::tempdir_in("/tmp").unwrap();
    let global = sockets.path().join("global.sock");
    let listener = tokio::net::UnixListener::bind(&global).unwrap();
    let active = Arc::new(AtomicBool::new(true));
    let (endpoint, hello) = if relay {
        // Introduce the deputy only after registration, as the real bridge does.
        let deputy = r#"
import json,socket,sys
first=sys.stdin.buffer.readline()
s=socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); f=s.makefile('rwb',buffering=0)
while first:
    f.write(first)
    if 'id' in json.loads(first):
        sys.stdout.buffer.write(f.readline()); sys.stdout.buffer.flush()
    first=sys.stdin.buffer.readline()
"#;
        let grant = config::parse_local_scope(
            &json!({"mcp_servers":{"codesense":{
                "command":"/usr/bin/python3","args":["-c",deputy,global]
            }}}),
            "mcp_servers",
            scope.path(),
        )
        .unwrap();
        let flag = active.clone();
        let descriptor = local_grant::start(
            scope.path(),
            grant,
            Default::default(),
            Arc::new(move || {
                if flag.load(Ordering::SeqCst) {
                    Ok(())
                } else {
                    Err("retired".into())
                }
            }),
        )
        .unwrap();
        scoped_mcp::write_descriptor(&scope.path().join("local-relay.json"), &descriptor).unwrap();
        crate::commands::mcp_scope::bind_fixture_relay_root(session, root);
        let hello = serde_json::to_string(&Hello {
            version: scoped_mcp::VERSION,
            capability: descriptor.capability,
            server: "codesense".into(),
        })
        .unwrap();
        (descriptor.socket, hello)
    } else {
        (global.clone(), String::new())
    };
    let mut client = tokio::process::Command::new("/usr/bin/python3")
        .args(["-c", INHERITED_SOCKET_CLIENT])
        .arg(endpoint)
        .arg(session)
        .arg(hello)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let original = peer::inspect(client.id().unwrap()).unwrap();
    let mut control = client.stdin.take().unwrap();
    let mut status = BufReader::new(client.stdout.take().unwrap()).lines();
    let (socket, _) = listener.accept().await.unwrap();
    let caller = peer::of_socket(&socket).unwrap();
    let mut wire = BufReader::new(socket);
    let state = RwLock::new(None);
    let mut line = String::new();
    wire.read_line(&mut line).await.unwrap();
    // The deputy lives outside the real session root; use its registered proof.
    let direct_root = (!relay).then_some(root);
    initialize_wire(
        &state,
        serde_json::from_str(&line).unwrap(),
        caller,
        direct_root,
    );
    wire.get_mut().write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},\"serverInfo\":{\"name\":\"fixture\",\"version\":\"1\"}}}\n")
        .await.unwrap();
    let fake = FakeHost::new();
    fake.tab("inherited-worker", session, "Fixture", true);
    let host = port(&fake);
    let mut refused = false;
    for exited in [false, true] {
        line.clear();
        wire.read_line(&mut line).await.unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        let reply = dispatch(
            &host,
            &state,
            caller,
            direct_root,
            request["params"]["name"].as_str().unwrap(),
            request["params"]["arguments"].clone(),
            &scope.path().join("unused-completion.json"),
        )
        .await;
        let response = match reply {
            Ok(value) => json!({"jsonrpc":"2.0","id":request["id"],"result":value}),
            Err(error) => {
                refused = true;
                json!({"jsonrpc":"2.0","id":request["id"],"error":error})
            }
        };
        let mut bytes = serde_json::to_vec(&response).unwrap();
        bytes.push(b'\n');
        wire.get_mut().write_all(&bytes).await.unwrap();
        if !exited {
            assert!(!refused, "the live original caller is admitted");
            assert_eq!(fake.rows()[0].from, "inherited-worker");
            assert_eq!(status.next_line().await.unwrap().as_deref(), Some("ready"));
            control.write_all(b"fork\n").await.unwrap();
            let detached: u32 = status.next_line().await.unwrap().unwrap().parse().unwrap();
            assert!(client.wait().await.unwrap().success());
            assert!(!peer::runs(original), "the admitted process has exited");
            assert!(peer::runs(root), "the trusted root remains live");
            assert!(!peer::descends_from(peer::inspect(detached).unwrap(), root));
            control.write_all(b"call\n").await.unwrap();
        }
    }
    let response: Value =
        serde_json::from_str(&status.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(response.get("error").is_some(), refused);
    assert_eq!(fake.rows().len(), if refused { 1 } else { 2 });
    active.store(false, Ordering::SeqCst);
    crate::commands::mcp_scope::remove_scope_config(session);
    refused
}

#[tokio::test]
async fn review_r3_an_exited_peer_cannot_keep_identity_through_an_inherited_socket() {
    let _home_env = crate::routines::test_support::TempHome::new();
    let direct = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        inherited_socket_refused(false),
    )
    .await
    .unwrap();
    let relay = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        inherited_socket_refused(true),
    )
    .await
    .unwrap();
    assert!(
        direct && relay,
        "inherited calls must be refused: direct={direct}, relay={relay}"
    );
}
