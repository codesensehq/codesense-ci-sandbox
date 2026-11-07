// SPDX-License-Identifier: MIT
use super::*;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
struct Relay {
    reader: BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
    writer: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    task: tokio::task::JoinHandle<()>,
    active: Arc<AtomicBool>,
}
impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Relay {
    async fn new(fixture: &http_fixture::Fixture) -> Self {
        let grant = config::parse(
            &fixture.configuration(),
            "mcpServers",
            "codesense",
            None,
            std::path::Path::new("/tmp"),
            |key| (key == "MCP_HTTP_FIXTURE_TOKEN").then(|| http_fixture::SENTINEL.into()),
        )
        .unwrap();
        Self::with_grant(grant).await
    }
    async fn with_grant(grant: Grant) -> Self {
        Self::with_identity(grant, None).await
    }
    async fn with_identity(
        mut grant: Grant,
        identity: Option<(
            Arc<identity::Identity>,
            codesense_pty_core::activity::peer::PeerProcess,
        )>,
    ) -> Self {
        let server = grant.remove("desktop-http").unwrap();
        let (remote, desktop) = tokio::io::duplex(16 * 1024);
        let active = Arc::new(AtomicBool::new(true));
        let state = active.clone();
        let valid: Validate = Arc::new(move || {
            let state = state.clone();
            Box::pin(async move {
                if state.load(Ordering::SeqCst) {
                    Ok(())
                } else {
                    Err("retired".into())
                }
            })
        });
        let task = tokio::spawn(async move {
            let _ = transport::serve_with_namespace(server, desktop, valid, None, identity).await;
        });
        let (reader, writer) = tokio::io::split(remote);
        let mut relay = Self {
            reader: BufReader::new(reader),
            writer,
            task,
            active,
        };
        let result=relay.call(1,"initialize",json!({"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1"}})).await;
        assert_eq!(result["result"]["serverInfo"]["name"], "desktop-http");
        relay
            .send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await;
        relay
    }
    async fn send(&mut self, value: Value) {
        let mut data = serde_json::to_vec(&value).unwrap();
        data.push(b'\n');
        self.writer.write_all(&data).await.unwrap();
    }
    async fn read(&mut self) -> Option<Value> {
        let mut line = String::new();
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.reader.read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        if n == 0 {
            None
        } else {
            Some(serde_json::from_str(&line).unwrap())
        }
    }
    async fn call(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await;
        self.read().await.unwrap()
    }
}
#[tokio::test]
async fn local_mcp_http_preserves_authenticated_prompts_resources_and_session() {
    let fixture = http_fixture::Fixture::start().await;
    let mut relay = Relay::new(&fixture).await;
    let prompts = relay.call(2, "prompts/list", json!({})).await;
    assert_eq!(prompts["result"]["prompts"][0]["name"], "fixture-prompt");
    let resource = relay
        .call(3, "resources/read", json!({"uri":"fixture://resource"}))
        .await;
    assert_eq!(
        resource["result"]["contents"][0]["text"],
        "authenticated resource"
    );
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    let result = relay
        .call(4, "tools/call", json!({"name":"safe","arguments":{}}))
        .await;
    assert_eq!(result["result"]["content"][0]["text"], "authenticated:true");
    assert!(!result.to_string().contains(http_fixture::SENTINEL));
    assert_eq!(fixture.initializes.load(Ordering::SeqCst), 1);
    relay.active.store(false, Ordering::SeqCst);
    assert!(relay.read().await.is_none());
}
#[tokio::test]
async fn local_mcp_http_expired_or_lost_mutations_are_never_replayed() {
    for tool in ["expire", "lost"] {
        let fixture = http_fixture::Fixture::start().await;
        let mut relay = Relay::new(&fixture).await;
        relay.send(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":tool,"arguments":{}}})).await;
        let failed = relay.read().await.expect("the client transport stays open");
        assert_eq!(failed["id"], 2);
        assert_eq!(failed["error"]["message"], "CodeSense did not answer in time; outcome unknown; inspect state before repeating writes");
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while fixture.initialized.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let recovered = relay
            .call(3, "tools/call", json!({"name":"safe","arguments":{}}))
            .await;
        assert_eq!(
            recovered["result"]["content"][0]["text"],
            "authenticated:true"
        );
        assert_eq!(
            fixture.expired_mutations.load(Ordering::SeqCst)
                + fixture.lost_mutations.load(Ordering::SeqCst),
            1
        );
    }
}
#[tokio::test]
async fn local_mcp_http_oversized_result_closes_only_its_channel() {
    let fixture = http_fixture::Fixture::start().await;
    let mut relay = Relay::new(&fixture).await;
    relay.send(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"oversized","arguments":{}}})).await;
    assert!(relay.read().await.is_none());
    let mut other = Relay::new(&fixture).await;
    assert!(other.call(2, "resources/list", json!({})).await["result"]["resources"].is_array());
}

#[tokio::test]
async fn local_mcp_http_retirement_cancels_blocked_and_queued_dispatch() {
    let fixture = http_fixture::Fixture::start().await;
    let mut relay = Relay::new(&fixture).await;
    relay.send(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"block","arguments":{}}})).await;
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while fixture.blocked.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    relay.send(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"safe","arguments":{}}})).await;
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    relay.active.store(false, Ordering::SeqCst);
    assert!(relay.read().await.is_none());
    fixture.unblock.notify_one();
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    assert_eq!(fixture.safe_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.blocked.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn local_mcp_http_missing_login_reports_authentication_required() {
    let fixture = http_fixture::Fixture::start().await;
    let mut grant = config::parse(
        &fixture.configuration(),
        "mcpServers",
        "codesense",
        None,
        std::path::Path::new("/tmp"),
        |_| Some("wrong-fixture-token".into()),
    )
    .unwrap();
    let (mut remote, desktop) = tokio::io::duplex(16 * 1024);
    let valid: Validate = Arc::new(|| Box::pin(async { Ok(()) }));
    let task = tokio::spawn(serve(grant.remove("desktop-http").unwrap(), desktop, valid));
    remote.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1\"}}}\n").await.unwrap();
    let mut line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        BufReader::new(remote).read_line(&mut line),
    )
    .await
    .unwrap()
    .unwrap();
    let response: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["error"]["code"], -32001);
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("authentication required"));
    task.await.unwrap();
    assert_eq!(fixture.initializes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn local_mcp_http_auth_failure_never_relabels_accepted_pending_mutation() {
    let fixture = http_fixture::Fixture::start().await;
    let mut relay = Relay::new(&fixture).await;
    relay.send(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"accepted","arguments":{}}})).await;
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while fixture.blocked.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    relay.send(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"unauthenticated","arguments":{}}})).await;
    let response = relay.read().await.unwrap();
    assert_eq!(response["id"], 3);
    assert_eq!(response["error"]["code"], -32001);
    assert!(relay.read().await.is_none());
    assert_eq!(fixture.blocked.load(Ordering::SeqCst), 1);
}

#[test]
fn selected_agent_mcp_missing_or_disabled_is_required_failure() {
    let value = json!({"mcpServers": {"present": {"command":"fixture"}, "disabled": {"command":"fixture", "enabled":false}}});
    let grant = config::parse(
        &value,
        "mcpServers",
        "codesense",
        None,
        std::path::Path::new("/tmp"),
        |_| None,
    )
    .unwrap();
    assert!(
        require_selected_servers(&grant, &["present".into(), "codesense".into()], "codesense")
            .is_ok()
    );
    for name in ["missing", "disabled"] {
        assert!(
            require_selected_servers(&grant, &[name.into()], "codesense")
                .unwrap_err()
                .contains("missing or disabled")
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn local_mcp_stdio_reconnects_after_its_child_exits_without_replaying_calls() {
    stdio_request_failure("exit").await;
}

#[cfg(unix)]
#[tokio::test]
async fn local_mcp_stdio_send_timeout_replaces_partial_write_and_kernel_identity() {
    stdio_request_failure("send-timeout").await;
}

#[cfg(unix)]
#[tokio::test]
async fn local_mcp_stdio_response_timeout_keeps_completed_send_and_kernel_identity() {
    stdio_request_failure("response-timeout").await;
}

#[cfg(unix)]
async fn stdio_request_failure(failure: &str) {
    use codesense_host_proto::scoped_mcp::{self, Descriptor};
    use codesense_pty_core::activity::peer;
    let _home = crate::routines::test_support::TempHome::new();
    let parent = crate::commands::mcp_scope::mcp_scope_session_dir("unused")
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::create_dir_all(&parent).unwrap();
    let dir = tempfile::Builder::new()
        .prefix("replacement-child-")
        .tempdir_in(parent)
        .unwrap();
    let session = dir.path().file_name().unwrap().to_str().unwrap().to_owned();
    let descriptor = Descriptor {
        version: scoped_mcp::VERSION,
        socket: "/tmp/unused-fixture".into(),
        capability: "c".repeat(64),
    };
    scoped_mcp::write_descriptor(&dir.path().join("local-relay.json"), &descriptor).unwrap();
    let identity = identity::Identity::new(
        dir.path(),
        &session,
        descriptor.capability,
        Arc::new(|| Ok(())),
    );
    crate::commands::mcp_scope::bind_relay_identity(&session, &identity);
    let (socket, _other) = std::os::unix::net::UnixStream::pair().unwrap();
    let ingress = peer::of_socket(&socket).unwrap();
    crate::commands::mcp_scope::bind_fixture_relay_root(&session, ingress);
    let pid_file = dir.path().join("child-pid");
    let counter = dir.path().join("generation");
    let ready = dir.path().join("initialized");
    let partial = dir.path().join("partial-write");
    let calls = dir.path().join("calls");
    let script = r#"
import json, pathlib, sys, os, time
counter, ready, pid_file, partial, calls = map(pathlib.Path, sys.argv[1:6])
failure = sys.argv[6]
pid_file.write_text(str(os.getpid()))
generation = int(counter.read_text()) + 1 if counter.exists() else 1
counter.write_text(str(generation))
for line in sys.stdin.buffer.raw:
    message = json.loads(line)
    method = message.get('method')
    if method == 'notifications/initialized':
        ready.write_text(str(generation))
        if failure == 'send-timeout' and generation == 1:
            assert os.read(0, 1) == b'{'
            partial.write_text('started')
            while True:
                time.sleep(1)
        continue
    if 'id' not in message:
        continue
    if method == 'initialize':
        result = {'protocolVersion':'2024-11-05','capabilities':{'tools':{}},'serverInfo':{'name':'desktop-http','version':'1'}}
    else:
        name = message.get('params',{}).get('name')
        with calls.open('a') as log:
            log.write(str(generation) + ':' + str(name) + '\n')
        if name == 'drop':
            if failure == 'response-timeout':
                partial.write_text('accepted')
                continue
            sys.exit(0)
        result = {'content':[{'type':'text','text':str(generation)}]}
    print(json.dumps({'jsonrpc':'2.0','id':message['id'],'result':result}), flush=True)
"#;
    let grant = config::parse(
        &json!({"mcpServers":{"desktop-http":{
            "command":"/usr/bin/python3","args":["-u","-c",script,counter,ready,pid_file,partial,calls,failure]
        }}}),
        "mcpServers",
        "codesense",
        None,
        dir.path(),
        |_| None,
    )
    .unwrap();
    let mut relay = Relay::with_identity(grant, Some((identity.clone(), ingress))).await;
    let original_pid: u32 = std::fs::read_to_string(&pid_file).unwrap().parse().unwrap();
    let original = peer::inspect(original_pid).unwrap();
    assert!(crate::commands::mcp_scope::relay_peer_proven(
        &session, original
    ));
    let stall_stdin = failure == "send-timeout";
    relay.send(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"drop","arguments":{"payload":if stall_stdin {"x".repeat(512 * 1024)} else {String::new()}}}})).await;
    if failure != "exit" {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !partial.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the child must observe the request before advancing its deadline");
        tokio::time::pause();
        // Let timers fire in order: jumping past both nested send/write deadlines
        // can hide a canceled partial write behind the inner writer's error.
        tokio::time::sleep(std::time::Duration::from_secs(121)).await;
    }
    let failed = relay
        .read()
        .await
        .expect("a failed request must not close the client stream");
    assert_eq!(failed["id"], 2);
    assert_eq!(
        failed["error"]["message"],
        "CodeSense did not answer in time; outcome unknown; inspect state before repeating writes"
    );
    if failure != "exit" {
        tokio::time::resume();
    }
    if failure == "response-timeout" {
        assert_eq!(
            relay
                .call(3, "tools/call", json!({"name":"safe","arguments":{}}))
                .await["result"]["content"][0]["text"],
            "1"
        );
        assert_eq!(peer::inspect(original_pid).unwrap(), original);
        assert!(crate::commands::mcp_scope::relay_peer_proven(
            &session, original
        ));
        assert_eq!(std::fs::read_to_string(counter).unwrap(), "1");
        assert_eq!(std::fs::read_to_string(calls).unwrap(), "1:drop\n1:safe\n");
        return;
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if tokio::fs::read_to_string(&ready).await.ok().as_deref() == Some("2") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("send failure must retire the downstream and initialize a replacement child");
    let replacement_pid: u32 = std::fs::read_to_string(&pid_file).unwrap().parse().unwrap();
    assert_ne!(replacement_pid, original_pid);
    let replacement = peer::inspect(replacement_pid).unwrap();
    assert_ne!(replacement, original);
    assert_ne!(peer::inspect(original_pid), Some(original));
    assert!(
        crate::commands::mcp_scope::relay_peer_proven(&session, replacement),
        "the replacement bridge must receive its own kernel identity before serving calls"
    );
    assert!(!crate::commands::mcp_scope::relay_peer_proven(
        &session, original
    ));
    assert_eq!(
        relay
            .call(3, "tools/call", json!({"name":"safe","arguments":{}}))
            .await["result"]["content"][0]["text"],
        "2"
    );
    assert_eq!(
        std::fs::read_to_string(counter).unwrap(),
        "2",
        "a failed mutation was not replayed into another child"
    );
    assert_eq!(
        std::fs::read_to_string(calls).unwrap(),
        if stall_stdin {
            "2:safe\n"
        } else {
            "1:drop\n2:safe\n"
        },
        "the replacement must receive valid JSON and only the new call"
    );
}

#[tokio::test]
async fn local_mcp_policy_denial_still_answers_without_dispatch_or_reconnect() {
    let fixture = http_fixture::Fixture::start().await;
    let mut config = fixture.configuration();
    config["mcpServers"]["desktop-http"]["enabled_tools"] = json!(["safe"]);
    let grant = config::parse(
        &config,
        "mcpServers",
        "codesense",
        None,
        std::path::Path::new("/tmp"),
        |key| (key == "MCP_HTTP_FIXTURE_TOKEN").then(|| http_fixture::SENTINEL.into()),
    )
    .unwrap();
    let mut relay = Relay::with_grant(grant).await;
    let denied = relay
        .call(2, "tools/call", json!({"name":"lost","arguments":{}}))
        .await;
    assert_eq!(denied["error"]["code"], -32602);
    assert_eq!(fixture.lost_mutations.load(Ordering::SeqCst), 0);
    assert_eq!(
        relay
            .call(3, "tools/call", json!({"name":"safe","arguments":{}}))
            .await["result"]["content"][0]["text"],
        "authenticated:true"
    );
    assert_eq!(fixture.initializes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn local_mcp_http_timeout_retires_sse_tasks_and_sockets_and_next_call_works() {
    let fixture = http_fixture::Fixture::start().await;
    let mut relay = Relay::new(&fixture).await;
    for batch in 0..3 {
        let ids: std::collections::BTreeSet<u64> = (2 + batch * 32..34 + batch * 32).collect();
        for id in &ids {
            relay.send(json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"accepted","arguments":{}}})).await;
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while fixture.active_sse.load(Ordering::SeqCst) != 32 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(121)).await;
        let mut answered = std::collections::BTreeSet::new();
        for _ in &ids {
            let failed = relay.read().await.unwrap();
            assert_eq!(failed["error"]["message"], "CodeSense did not answer in time; outcome unknown; inspect state before repeating writes");
            assert!(answered.insert(failed["id"].as_u64().unwrap()));
        }
        tokio::time::resume();
        assert_eq!(
            answered, ids,
            "every expired call receives its own id exactly once"
        );
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while fixture.active_sse.load(Ordering::SeqCst) != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("expired SSE tasks and sockets must all retire before the next batch");
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while fixture.initialized.load(Ordering::SeqCst) < batch as usize + 2 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the same upstream session reconnects its retired HTTP downstream");
    }
    assert_eq!(
        fixture.blocked.load(Ordering::SeqCst),
        96,
        "expired mutations are never replayed"
    );
    assert_eq!(fixture.initializes.load(Ordering::SeqCst), 4);
    assert_eq!(
        relay
            .call(1000, "tools/call", json!({"name":"safe","arguments":{}}))
            .await["result"]["content"][0]["text"],
        "authenticated:true"
    );
}

#[tokio::test]
async fn local_mcp_client_disconnect_cancels_a_blocked_http_post() {
    let fixture = http_fixture::Fixture::start().await;
    let mut relay = Relay::new(&fixture).await;
    relay.send(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"block","arguments":{}}})).await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while fixture.active_headers.load(Ordering::SeqCst) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Mirrors local_grant's forwarding winner dropping the relay future.
    drop(relay);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while fixture.active_headers.load(Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("upstream disconnect must cancel HTTP before the header deadline");
}

async fn standing_stream_opened(fixture: &http_fixture::Fixture) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while fixture.gets.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the session opens its standing SSE stream");
}

#[tokio::test]
async fn local_mcp_http_never_resumes_a_stream_the_server_asks_to_retry() {
    let fixture = http_fixture::Fixture::start().await;
    let mut relay = Relay::new(&fixture).await;
    standing_stream_opened(&fixture).await;
    relay.send(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"sse-retry","arguments":{}}})).await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while fixture.blocked.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // Far past the server's `retry: 10`: rmcp would resume here with Last-Event-ID 7.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(fixture.resumes.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.gets.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn local_mcp_http_oversized_sse_event_never_buys_another_request() {
    let fixture = http_fixture::Fixture::start().await;
    let mut relay = Relay::new(&fixture).await;
    standing_stream_opened(&fixture).await;
    relay.send(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"sse-oversized","arguments":{}}})).await;
    assert!(relay.read().await.is_none());
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(fixture.resumes.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.gets.load(Ordering::SeqCst), 1);
}

fn fixture_client() -> http::BoundedHttp {
    http::BoundedHttp {
        client: reqwest_mcp::Client::builder()
            .redirect(reqwest_mcp::redirect::Policy::none())
            .build()
            .unwrap(),
        valid: Arc::new(|| Box::pin(async { Ok(()) })),
        cancel: Default::default(),
        failures: Default::default(),
        stream: Default::default(),
        owns_stream: false,
    }
}
fn fixture_headers(
) -> std::collections::HashMap<reqwest_mcp::header::HeaderName, reqwest_mcp::header::HeaderValue> {
    [(
        reqwest_mcp::header::HeaderName::from_static("x-fixture-auth"),
        reqwest_mcp::header::HeaderValue::from_static(http_fixture::SENTINEL),
    )]
    .into()
}
fn client_message(value: Value) -> rmcp::model::ClientJsonRpcMessage {
    serde_json::from_value(value).unwrap()
}

// Mirrors rmcp 2.2's clone order; the standing_stream_opened proofs pin that order live.
#[tokio::test]
async fn local_mcp_http_a_resume_before_the_standing_get_never_takes_its_slot() {
    use rmcp::transport::streamable_http_client::StreamableHttpClient;
    let fixture = http_fixture::Fixture::start().await;
    let http = fixture_client();
    let (uri, session): (Arc<str>, Arc<str>) =
        (fixture.url.as_str().into(), "fixture-session".into());
    // A resume clone made before the session is initialized can never own the stream.
    let early = http.clone();
    http.post_message(
        uri.clone(),
        client_message(json!({"jsonrpc":"2.0","method":"notifications/initialized"})),
        Some(session.clone()),
        None,
        fixture_headers(),
    )
    .await
    .unwrap();
    let standing = http.clone();
    // A POST answered as SSE closes before the standing GET runs; rmcp resumes it
    // on a clone of the client, with no event id to send.
    let resume = http.clone();
    for client in [&early, &resume.clone()] {
        assert!(client
            .get_stream(uri.clone(), session.clone(), None, None, fixture_headers())
            .await
            .is_err());
    }
    assert_eq!(fixture.gets.load(Ordering::SeqCst), 0);
    let _stream = standing
        .get_stream(uri.clone(), session.clone(), None, None, fixture_headers())
        .await
        .expect("the standing stream still opens");
    // The standing stream's own resume runs on a clone and is refused too.
    assert!(standing
        .clone()
        .get_stream(uri, session, None, None, fixture_headers())
        .await
        .is_err());
    assert_eq!(fixture.gets.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.resumes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn local_mcp_http_policy_failure_stops_every_further_request() {
    use rmcp::transport::streamable_http_client::StreamableHttpClient;
    let fixture = http_fixture::Fixture::start().await;
    let http = fixture_client();
    let (uri, session): (Arc<str>, Arc<str>) =
        (fixture.url.as_str().into(), "fixture-session".into());
    http.post_message(
        uri.clone(),
        client_message(json!({"jsonrpc":"2.0","method":"notifications/initialized"})),
        Some(session.clone()),
        None,
        fixture_headers(),
    )
    .await
    .unwrap();
    let standing = http.clone();
    http.failures.policy.store(true, Ordering::Release);
    assert!(http
        .post_message(
            uri.clone(),
            client_message(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"safe","arguments":{}}})),
            Some(session.clone()),
            None,
            fixture_headers(),
        )
        .await
        .is_err());
    assert!(standing
        .get_stream(uri, session, None, None, fixture_headers())
        .await
        .is_err());
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(fixture.safe_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.gets.load(Ordering::SeqCst), 0);
}
