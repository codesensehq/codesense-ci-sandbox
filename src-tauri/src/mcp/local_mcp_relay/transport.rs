// SPDX-License-Identifier: MIT
use super::{
    config::{Endpoint, Server},
    Validate,
};
use futures_util::{stream::FuturesUnordered, StreamExt};
#[cfg(unix)]
use rmcp::model::ServerJsonRpcMessage;
use rmcp::{model::ClientJsonRpcMessage, transport::Transport, RoleClient};
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc, time::Duration};
#[cfg(unix)]
use tokio::sync::Mutex;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader},
    sync::mpsc,
};
type PendingSend = std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;
const LIMIT: usize = 1024 * 1024;
const REQUESTS: usize = 32;
const DEADLINE: Duration = Duration::from_secs(120);

// Dedicated reader task keeps partial lines intact across authority/deadline selects.
async fn read_messages<R: AsyncRead + Unpin>(
    reader: R,
    tx: mpsc::Sender<Value>,
) -> Result<(), String> {
    let mut reader = BufReader::new(reader);
    loop {
        let mut line = Vec::new();
        loop {
            let bytes = reader
                .fill_buf()
                .await
                .map_err(|_| "MCP stream read failed")?;
            if bytes.is_empty() {
                return Ok(());
            }
            let count = bytes
                .iter()
                .position(|b| *b == b'\n')
                .map(|n| n + 1)
                .unwrap_or(bytes.len());
            if line.len() + count > LIMIT {
                return Err("MCP message exceeds limit".into());
            }
            line.extend_from_slice(&bytes[..count]);
            let done = bytes[count - 1] == b'\n';
            reader.consume(count);
            if done {
                break;
            }
        }
        let message = serde_json::from_slice(&line).map_err(|_| "invalid MCP message")?;
        tx.send(message).await.map_err(|_| "MCP stream closed")?;
    }
}
fn key(v: &Value) -> Option<String> {
    v.get("id")
        .filter(|v| v.is_string() || v.is_number())
        .map(Value::to_string)
}
fn permitted(server: &Server, name: &str) -> bool {
    !server.disabled_tools.contains(name)
        && server
            .enabled_tools
            .as_ref()
            .is_none_or(|set| set.contains(name))
}
async fn write_message<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    value: &Value,
) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| "invalid MCP response")?;
    if bytes.len() + 1 > LIMIT {
        return Err("MCP message exceeds limit".into());
    }
    bytes.push(b'\n');
    tokio::time::timeout(DEADLINE, writer.write_all(&bytes))
        .await
        .map_err(|_| "MCP downstream stalled")?
        .map_err(|_| "MCP stream closed".to_string())
}

// Architecture: authority and framing failures stay terminal across reconnects.
#[derive(Clone, Default)]
pub(super) struct Failures {
    pub authentication_required: Arc<std::sync::Mutex<Option<Value>>>,
    pub policy: Arc<std::sync::atomic::AtomicBool>,
}
impl Failures {
    pub(super) fn terminal(&self) -> bool {
        self.policy.load(std::sync::atomic::Ordering::Acquire)
    }
}

// Architecture: a failed request is never replayed: writes may already have committed.
const UNKNOWN_OUTCOME: &str =
    "CodeSense did not answer in time; outcome unknown; inspect state before repeating writes";
// Policy (open): reconnect delays grow from 250 ms to a 5 s cap; individual connects take at most 15 s.
const RETRY_MIN: Duration = Duration::from_millis(250);
const RETRY_MAX: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

async fn unknown_outcome<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    id: Value,
) -> Result<(), String> {
    write_message(writer, &rpc_error(id, -32000, UNKNOWN_OUTCOME)).await
}

async fn pump<T, F, Fut>(
    server: &Server,
    stream: tokio::io::DuplexStream,
    valid: Validate,
    mut connect: F,
    failures: Failures,
    namespace_note: Option<&str>,
    codesense_bridge: bool,
) -> Result<(), String>
where
    T: Transport<RoleClient>,
    F: FnMut(tokio_util::sync::CancellationToken) -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let (tx, mut rx) = mpsc::channel(4);
    let reader_task = tokio::spawn(read_messages(reader, tx));
    let mut pending = BTreeMap::<String, (String, tokio::time::Instant)>::new();
    // Cancelled ids, held until DEADLINE so a late reply never answers a reused id.
    let mut cancelled = BTreeMap::<String, tokio::time::Instant>::new();
    let mut sends: FuturesUnordered<PendingSend> = FuturesUnordered::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut transport: Option<T> = None;
    let mut cancel = tokio_util::sync::CancellationToken::new();
    // Architecture: client forwarding can drop this future before explicit cleanup.
    // Every replacement connection keeps cancellation tied to the pump's lifetime.
    let mut _retire = cancel.clone().drop_guard();
    let mut retry_at = tokio::time::Instant::now();
    let mut backoff = RETRY_MIN;
    let mut initialize: Option<Value> = None;
    let mut handshake: Option<(String, tokio::time::Instant)> = None;
    let mut reconnecting = false;
    let work = async {
        // Architecture: create the initial downstream before consuming the client's initialize.
        match tokio::time::timeout(CONNECT_TIMEOUT, connect(cancel.clone())).await {
            Ok(Ok(connected)) => transport = Some(connected),
            _ => {
                cancel.cancel();
                if failures.terminal() {
                    return Err("MCP authority or policy failure".into());
                }
                reconnecting = true;
                retry_at = tokio::time::Instant::now() + backoff;
                backoff = (backoff * 2).min(RETRY_MAX);
            }
        }
        loop {
            let disconnected = tokio::select! {
                biased;
                _=tick.tick()=>{
                    valid().await?;
                    if failures.terminal(){return Err("MCP authority or policy failure".into());}
                    cancelled.retain(|_,at|at.elapsed()<DEADLINE);
                    // Architecture: expiry answers only that id; late replies are discarded below.
                    let expired: Vec<_> = pending.iter().filter(|(_,(_,at))|at.elapsed()>=DEADLINE).map(|(id,_)|id.clone()).collect();
                    // Architecture: rmcp owns accepted SSE tasks and queued sends at
                    // connection scope. Retire that entire downstream on HTTP expiry;
                    // no new work is admitted until close/drop has canceled all of it.
                    // The upstream session stays open and mutations are never replayed.
                    let retire_http = !expired.is_empty() && matches!(&server.endpoint, Endpoint::Http { .. });
                    for id in expired { pending.remove(&id); unknown_outcome(&mut writer,serde_json::from_str(&id).map_err(|_|"invalid MCP id")?).await?; }
                    retire_http || handshake.as_ref().is_some_and(|(_,at)|at.elapsed()>=CONNECT_TIMEOUT)
                }
                _=tokio::time::sleep_until(retry_at),if transport.is_none()=>{
                    valid().await?;
                    cancel = tokio_util::sync::CancellationToken::new();
                    _retire = cancel.clone().drop_guard();
                    match tokio::time::timeout(CONNECT_TIMEOUT,connect(cancel.clone())).await {
                        Ok(Ok(mut connected))=>{
                            if reconnecting {
                                if let Some(mut init)=initialize.clone() {
                                    let id=Value::String(format!("relay-reconnect-{}",uuid::Uuid::new_v4()));
                                    init["id"]=id.clone();
                                    handshake=Some((id.to_string(),tokio::time::Instant::now()));
                                    let sending=connected.send(serde_json::from_value(init).map_err(|_|"invalid MCP initialize")?);
                                    sends.push(Box::pin(async move {tokio::time::timeout(CONNECT_TIMEOUT,sending).await.map_err(|_|"MCP reconnect timed out".to_string())?.map_err(|_|"MCP reconnect failed".to_string())}));
                                } else { reconnecting=false; backoff=RETRY_MIN; }
                            }
                            transport=Some(connected);
                        }
                        _=>{
                            cancel.cancel();
                            if failures.terminal(){return Err("MCP authority or policy failure".into());}
                            retry_at=tokio::time::Instant::now()+backoff;
                            backoff=(backoff*2).min(RETRY_MAX);
                        }
                    }
                    false
                }
                Some(result)=sends.next(),if !sends.is_empty()=>{result.is_err()}
                message=rx.recv(),if sends.len()<REQUESTS=>{
                    let Some(message)=message else{return Ok(());};valid().await?;
                    let id=key(&message);let method=message.get("method").and_then(Value::as_str);
                    if method==Some("tools/call") && !message.pointer("/params/name").and_then(Value::as_str).is_some_and(|name|
                        permitted(server,name) && (!codesense_bridge || name != "codesense_call_tool" ||
                            message.pointer("/params/arguments/name").and_then(Value::as_str).is_some_and(|target| permitted(server,target.trim())))) {
                        if let Some(id)=message.get("id"){write_message(&mut writer,&rpc_error(id.clone(), -32602, "tool outside captured MCP policy")).await?;}
                        continue;
                    }
                    // Architecture: while disconnected/initializing, refuse new work without replaying it later.
                    if transport.is_none() || reconnecting {
                        if method.is_some() {if let Some(id)=message.get("id"){unknown_outcome(&mut writer,id.clone()).await?;}}
                        continue;
                    }
                    if method==Some("initialize") {initialize=Some(message.clone());}
                    // rmcp >= 2 drops the reply to a cancelled request; stop waiting for it.
                    // Tombstones share pending's cap; without room the id stays pending
                    // and expires as unknown outcome.
                    if method==Some("notifications/cancelled") {
                        if let Some(id)=message.pointer("/params/requestId").filter(|v|v.is_string()||v.is_number()).map(Value::to_string) {
                            if cancelled.len()<REQUESTS && pending.remove(&id).is_some() {cancelled.insert(id,tokio::time::Instant::now());}
                        }
                    }
                    if let (Some(id),Some(method))=(id,method){
                        if cancelled.contains_key(&id){
                            write_message(&mut writer,&rpc_error(message["id"].clone(), -32600, "request id reused before its cancelled call expired")).await?;
                            continue;
                        }
                        if pending.len()>=REQUESTS || pending.contains_key(&id){return Err("MCP request budget exceeded".into());}
                        pending.insert(id,(method.into(),tokio::time::Instant::now()));
                    }
                    let typed:ClientJsonRpcMessage=serde_json::from_value(message).map_err(|_|"unsupported MCP client message")?;
                    let sending=transport.as_mut().unwrap().send(typed);
                    // A canceled send may leave a partial frame; retire it just like a send error.
                    sends.push(Box::pin(async move {match tokio::time::timeout(DEADLINE,sending).await { Ok(Ok(()))=>Ok(()), _=>Err("MCP request outcome unknown".to_string()) }}));
                    false
                }
                message=async {match transport.as_mut(){Some(t)=>t.receive().await,None=>std::future::pending().await}}=>{
                    valid().await?;
                    if let Some(message)=message {
                        let mut value=serde_json::to_value(message).map_err(|_|"invalid MCP response")?;
                        if value.get("method").is_none(){
                            if handshake.as_ref().is_some_and(|(id,_)|key(&value).as_ref()==Some(id)) {
                                if value.get("error").is_some(){return Err("MCP reconnect initialization rejected".into());}
                                handshake=None; reconnecting=false; backoff=RETRY_MIN;
                                let sending=transport.as_mut().unwrap().send(serde_json::from_value(serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"})).unwrap());
                                sends.push(Box::pin(async move {sending.await.map_err(|_|"MCP reconnect failed".to_string())}));
                                continue;
                            }
                            let Some((method,_))=key(&value).and_then(|id|pending.remove(&id)) else {continue;};
                            if method=="initialize" {if let Some(note)=namespace_note {prepend_namespace_note(&mut value,note);}}
                            if method=="tools/list"{if let Some(tools)=value.pointer_mut("/result/tools").and_then(Value::as_array_mut){tools.retain(|tool|tool.get("name").and_then(Value::as_str).is_some_and(|name|permitted(server,name)));}}
                        }
                        write_message(&mut writer,&value).await?;
                        false
                    } else {true}
                }
            };
            if disconnected {
                // Authentication errors retain their originating pending id for the terminal response.
                if failures.terminal() {
                    return Err("MCP authority or policy failure".into());
                }
                for id in std::mem::take(&mut pending).into_keys() {
                    unknown_outcome(
                        &mut writer,
                        serde_json::from_str(&id).map_err(|_| "invalid MCP id")?,
                    )
                    .await?;
                }
                sends.clear();
                handshake = None;
                cancel.cancel();
                if let Some(mut old) = transport.take() {
                    let _ = tokio::time::timeout(Duration::from_secs(2), old.close()).await;
                }
                reconnecting = true;
                retry_at = tokio::time::Instant::now() + backoff;
                backoff = (backoff * 2).min(RETRY_MAX);
            }
        }
    };
    let monitor = async {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        loop {
            interval.tick().await;
            valid().await?;
            if failures.terminal() {
                return Err("MCP authority or policy failure".into());
            }
        }
        #[allow(unreachable_code)]
        Ok::<(), String>(())
    };
    let result = tokio::select! { result=work=>result, result=monitor=>result };
    cancel.cancel();
    reader_task.abort();
    let _ = reader_task.await;
    if let Some(mut transport) = transport {
        let _ = tokio::time::timeout(Duration::from_secs(2), transport.close()).await;
    }
    let rejected = failures
        .authentication_required
        .lock()
        .ok()
        .and_then(|mut id| id.take());
    if let Some(id) = rejected.filter(|id| pending.contains_key(&id.to_string())) {
        let response = rpc_error(
            id,
            -32001,
            "local MCP authentication required; CLI-owned OAuth sessions are not imported",
        );
        if valid().await.is_ok() {
            let monitor = async {
                loop {
                    if valid().await.is_err() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            };
            tokio::select! {biased; _=monitor=>{},_=tokio::time::timeout(Duration::from_secs(1),write_message(&mut writer,&response))=>{}}
        }
    }
    result
}

fn prepend_namespace_note(response: &mut Value, note: &str) {
    let Some(result) = response.get_mut("result").and_then(Value::as_object_mut) else {
        return;
    };
    let prior = result
        .get("instructions")
        .and_then(Value::as_str)
        .unwrap_or("");
    result.insert(
        "instructions".into(),
        Value::String(format!("{note}\n{prior}")),
    );
}

#[cfg(unix)]
struct Stdio {
    input: Arc<Mutex<tokio::process::ChildStdin>>,
    messages: mpsc::Receiver<Value>,
    reader: tokio::task::JoinHandle<Result<(), String>>,
    child: tokio::process::Child,
    pid: Option<u32>,
    _identity: Option<super::identity::Registration>,
}
#[cfg(unix)]
impl Drop for Stdio {
    fn drop(&mut self) {
        self.reader.abort();
        if let Some(pid) = self.pid {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
}
#[cfg(unix)]
impl Transport<RoleClient> for Stdio {
    type Error = std::io::Error;
    fn send(
        &mut self,
        item: ClientJsonRpcMessage,
    ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send + 'static {
        let input = self.input.clone();
        async move {
            let value = serde_json::to_value(item).map_err(std::io::Error::other)?;
            write_message(&mut *input.lock().await, &value)
                .await
                .map_err(std::io::Error::other)
        }
    }
    async fn receive(&mut self) -> Option<ServerJsonRpcMessage> {
        serde_json::from_value(self.messages.recv().await?).ok()
    }
    async fn close(&mut self) -> Result<(), Self::Error> {
        if let Some(pid) = self.pid {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        self.reader.abort();
        self.child.wait().await?;
        self.pid = None;
        Ok(())
    }
}

pub(super) async fn serve(
    server: Server,
    stream: tokio::io::DuplexStream,
    valid: Validate,
) -> Result<(), String> {
    serve_with_namespace(server, stream, valid, None, None).await
}

#[cfg(unix)]
async fn stdio_connection(
    server: Server,
    identity: Option<(
        Arc<super::identity::Identity>,
        codesense_pty_core::activity::peer::PeerProcess,
    )>,
    failures: Failures,
) -> Result<Stdio, String> {
    let Endpoint::Stdio {
        command,
        args,
        env,
        cwd,
    } = server.endpoint
    else {
        unreachable!()
    };
    let mut command = tokio::process::Command::new(command);
    command
        .args(args)
        .envs(env)
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .process_group(0);
    let mut child = command
        .spawn()
        .map_err(|_| "local MCP process unavailable")?;
    let pid = child.id().ok_or("local MCP process unavailable")?;
    let input = child.stdin.take().ok_or("MCP stdin unavailable")?;
    let output = child.stdout.take().ok_or("MCP stdout unavailable")?;
    let (tx, rx) = mpsc::channel(4);
    let reader_failures = failures.clone();
    let mut transport = Stdio {
        input: Arc::new(Mutex::new(input)),
        messages: rx,
        reader: tokio::spawn(async move {
            let result = read_messages(output, tx).await;
            if matches!(
                &result,
                Err(message) if matches!(message.as_str(), "MCP message exceeds limit" | "invalid MCP message")
            ) {
                reader_failures
                    .policy
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            result
        }),
        child,
        pid: Some(pid),
        _identity: None,
    };
    // Architecture: each replacement child is registered before forwarding initialize.
    transport._identity = match identity {
        Some((identity, ingress)) => {
            match tokio::task::spawn_blocking(move || identity.spawned(pid, ingress)).await {
                Ok(Ok(registration)) => Some(registration),
                _ => {
                    failures
                        .policy
                        .store(true, std::sync::atomic::Ordering::Release);
                    return Err("cannot verify MCP relay child".into());
                }
            }
        }
        None => None,
    };
    Ok(transport)
}

pub(super) async fn serve_with_namespace(
    server: Server,
    stream: tokio::io::DuplexStream,
    valid: Validate,
    namespace_note: Option<String>,
    identity: Option<(
        Arc<super::identity::Identity>,
        codesense_pty_core::activity::peer::PeerProcess,
    )>,
) -> Result<(), String> {
    valid().await?;
    let failures = Failures::default();
    let codesense_bridge = identity.is_some();
    match &server.endpoint {
        Endpoint::Remote(_) => Err("remote MCP cannot execute through the desktop relay".into()),
        #[cfg(not(unix))]
        Endpoint::Stdio { .. } => {
            Err("local MCP stdio relay is unsupported on this platform".into())
        }
        #[cfg(unix)]
        Endpoint::Stdio { .. } => {
            let connect = |_| stdio_connection(server.clone(), identity.clone(), failures.clone());
            pump(
                &server,
                stream,
                valid,
                connect,
                failures.clone(),
                namespace_note.as_deref(),
                codesense_bridge,
            )
            .await
        }
        Endpoint::Http { url, headers } => {
            use rmcp::transport::{
                streamable_http_client::StreamableHttpClientTransportConfig,
                StreamableHttpClientTransport,
            };
            let client = reqwest_mcp::Client::builder()
                .retry(reqwest_mcp::retry::never())
                .redirect(reqwest_mcp::redirect::Policy::none())
                .connect_timeout(CONNECT_TIMEOUT)
                .build()
                .map_err(|_| "MCP HTTP client unavailable")?;
            let mut config = StreamableHttpClientTransportConfig::with_uri(url.clone())
                .reinit_on_expired_session(false);
            config.channel_buffer_capacity = 4;
            config.retry_config =
                Arc::new(rmcp::transport::common::client_side_sse::NeverRetry::default());
            config.custom_headers = headers
                .iter()
                .map(|(k, v)| {
                    Ok((
                        k.parse().map_err(|_| "invalid MCP header")?,
                        v.parse().map_err(|_| "invalid MCP header")?,
                    ))
                })
                .collect::<Result<_, String>>()?;
            let connect = |cancel| {
                let transport = StreamableHttpClientTransport::with_client(
                    super::http::BoundedHttp {
                        client: client.clone(),
                        valid: valid.clone(),
                        cancel,
                        failures: failures.clone(),
                        stream: Default::default(),
                        owns_stream: false,
                    },
                    config.clone(),
                );
                std::future::ready(Ok(transport))
            };
            pump(
                &server,
                stream,
                valid.clone(),
                connect,
                failures.clone(),
                namespace_note.as_deref(),
                codesense_bridge,
            )
            .await
        }
    }
}

#[cfg(test)]
mod namespace_tests {
    #[test]
    fn alias_mapping_precedes_native_truncation_without_erasing_server_instructions() {
        let original = "original server instructions ".repeat(25_000);
        let mut response = serde_json::json!({"result":{"instructions":original}});
        super::prepend_namespace_note(
            &mut response,
            "canonical codesense -> codesense_scope_example",
        );
        let instructions = response["result"]["instructions"].as_str().unwrap();
        assert!(instructions.starts_with("canonical codesense -> codesense_scope_example\n"));
        assert!(instructions.ends_with(&original));
        // Codex 0.159.3 uses the first 512 KiB as namespace description.
        assert!(instructions[..512 * 1024].contains("codesense_scope_example"));
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use serde_json::json;

    struct Fake {
        sent: mpsc::UnboundedSender<Value>,
        replies: mpsc::UnboundedReceiver<rmcp::model::ServerJsonRpcMessage>,
    }
    impl Transport<RoleClient> for Fake {
        type Error = std::io::Error;
        fn send(
            &mut self,
            message: ClientJsonRpcMessage,
        ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send + 'static {
            let result = self.sent.send(serde_json::to_value(message).unwrap());
            async move { result.map_err(std::io::Error::other) }
        }
        async fn receive(&mut self) -> Option<rmcp::model::ServerJsonRpcMessage> {
            self.replies.recv().await
        }
        async fn close(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn unanswered_request_expires_without_closing_transport_or_other_requests() {
        let server = Server {
            endpoint: Endpoint::Stdio {
                command: "unused-fake-dispatcher".into(),
                args: vec![],
                env: Default::default(),
                cwd: "/tmp".into(),
            },
            enabled_tools: None,
            disabled_tools: Default::default(),
        };
        let (sent, mut calls) = mpsc::unbounded_channel();
        let (replies, rx) = mpsc::unbounded_channel();
        let mut transport = Some(Fake { sent, replies: rx });
        let (client, relay) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move {
            pump(
                &server,
                relay,
                Arc::new(|| Box::pin(async { Ok(()) })),
                |_| std::future::ready(Ok(transport.take().unwrap())),
                Failures::default(),
                None,
                false,
            )
            .await
        });
        let (read, mut write) = tokio::io::split(client);
        let mut read = BufReader::new(read);
        write_message(
            &mut write,
            &json!({"jsonrpc":"2.0","id":"slow","method":"tools/call","params":{"name":"slow"}}),
        )
        .await
        .unwrap();
        assert_eq!(calls.recv().await.unwrap()["id"], "slow");
        tokio::time::advance(Duration::from_secs(60)).await;
        write_message(&mut write, &json!({"jsonrpc":"2.0","id":"in-flight","method":"tools/call","params":{"name":"normal"}})).await.unwrap();
        assert_eq!(calls.recv().await.unwrap()["id"], "in-flight");
        tokio::time::advance(Duration::from_secs(61)).await;
        let mut line = String::new();
        assert!(
            read.read_line(&mut line).await.unwrap() > 0,
            "only this request expires; the relay must stay open"
        );
        let error: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(error["id"], "slow");
        assert_eq!(error["error"]["message"], "CodeSense did not answer in time; outcome unknown; inspect state before repeating writes");
        replies
            .send(
                serde_json::from_value(
                    json!({"jsonrpc":"2.0","id":"in-flight","result":{"content":[]}}),
                )
                .unwrap(),
            )
            .unwrap();
        line.clear();
        assert!(read.read_line(&mut line).await.unwrap() > 0);
        assert_eq!(
            serde_json::from_str::<Value>(&line).unwrap()["id"],
            "in-flight"
        );
        write_message(
            &mut write,
            &json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"next"}}),
        )
        .await
        .unwrap();
        assert_eq!(calls.recv().await.unwrap()["id"], 2);
        replies
            .send(
                serde_json::from_value(
                    json!({"jsonrpc":"2.0","id":"slow","result":{"content":[]}}),
                )
                .unwrap(),
            )
            .unwrap();
        replies
            .send(
                serde_json::from_value(json!({"jsonrpc":"2.0","id":2,"result":{"content":[]}}))
                    .unwrap(),
            )
            .unwrap();
        line.clear();
        assert!(read.read_line(&mut line).await.unwrap() > 0);
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["id"], 2);
        assert!(!task.is_finished());
        drop(write);
        drop(read);
        task.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_request_is_never_answered_late_reused_or_as_unknown_outcome() {
        let server = Server {
            endpoint: Endpoint::Http {
                url: "http://127.0.0.1:9/mcp".into(),
                headers: Default::default(),
            },
            enabled_tools: None,
            disabled_tools: Default::default(),
        };
        let (sent, mut calls) = mpsc::unbounded_channel();
        let (replies, rx) = mpsc::unbounded_channel();
        let mut transport = Some(Fake { sent, replies: rx });
        let (client, relay) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move {
            pump(
                &server,
                relay,
                Arc::new(|| Box::pin(async { Ok(()) })),
                // A second connect would mean the HTTP downstream was retired.
                |_| std::future::ready(transport.take().ok_or_else(|| "reconnected".to_string())),
                Failures::default(),
                None,
                false,
            )
            .await
        });
        let (read, mut write) = tokio::io::split(client);
        let mut read = BufReader::new(read);
        write_message(
            &mut write,
            &json!({"jsonrpc":"2.0","id":"slow","method":"tools/call","params":{"name":"slow"}}),
        )
        .await
        .unwrap();
        assert_eq!(calls.recv().await.unwrap()["id"], "slow");
        write_message(&mut write, &json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"slow"}})).await.unwrap();
        assert_eq!(
            calls.recv().await.unwrap()["method"],
            "notifications/cancelled"
        );
        // Reusing the cancelled id is refused and never forwarded.
        write_message(
            &mut write,
            &json!({"jsonrpc":"2.0","id":"slow","method":"tools/call","params":{"name":"again"}}),
        )
        .await
        .unwrap();
        let mut line = String::new();
        assert!(read.read_line(&mut line).await.unwrap() > 0);
        let refused: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(refused["id"], "slow");
        assert_eq!(refused["error"]["code"], -32600);
        // The cancelled call's late reply is dropped.
        replies
            .send(
                serde_json::from_value(
                    json!({"jsonrpc":"2.0","id":"slow","result":{"content":[]}}),
                )
                .unwrap(),
            )
            .unwrap();
        write_message(
            &mut write,
            &json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"next"}}),
        )
        .await
        .unwrap();
        assert_eq!(calls.recv().await.unwrap()["id"], 2);
        replies
            .send(
                serde_json::from_value(json!({"jsonrpc":"2.0","id":2,"result":{"content":[]}}))
                    .unwrap(),
            )
            .unwrap();
        line.clear();
        assert!(read.read_line(&mut line).await.unwrap() > 0);
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["id"], 2);
        // Past DEADLINE: no unknown-outcome error, the downstream survives, and the id is free.
        tokio::time::advance(Duration::from_secs(121)).await;
        write_message(
            &mut write,
            &json!({"jsonrpc":"2.0","id":"slow","method":"tools/call","params":{"name":"again"}}),
        )
        .await
        .unwrap();
        assert_eq!(calls.recv().await.unwrap()["id"], "slow");
        replies
            .send(
                serde_json::from_value(
                    json!({"jsonrpc":"2.0","id":"slow","result":{"content":[]}}),
                )
                .unwrap(),
            )
            .unwrap();
        line.clear();
        assert!(read.read_line(&mut line).await.unwrap() > 0);
        let answered: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(answered["id"], "slow");
        assert!(answered.get("error").is_none());
        assert!(!task.is_finished());
        drop(write);
        drop(read);
        task.await.unwrap().unwrap();
    }
}
