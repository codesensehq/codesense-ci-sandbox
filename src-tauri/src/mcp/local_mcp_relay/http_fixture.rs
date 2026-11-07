// SPDX-License-Identifier: MIT
//! Controlled authenticated HTTP peer, shared by desktop and real-SSH relay proofs.
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
pub(crate) const SENTINEL: &str = "mcp3-desktop-http-fixture-only";
// Architecture: count a held peer until its handler/socket is actually retired.
struct ActivePeer(Arc<AtomicUsize>);
impl ActivePeer {
    fn new(count: Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(count)
    }
}
impl Drop for ActivePeer {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(crate) struct Fixture {
    pub(crate) url: String,
    pub(crate) initializes: Arc<AtomicUsize>,
    pub(crate) initialized: Arc<AtomicUsize>,
    pub(crate) expired_mutations: Arc<AtomicUsize>,
    pub(crate) lost_mutations: Arc<AtomicUsize>,
    pub(crate) blocked: Arc<AtomicUsize>,
    pub(crate) active_headers: Arc<AtomicUsize>,
    pub(crate) active_sse: Arc<AtomicUsize>,
    pub(crate) safe_calls: Arc<AtomicUsize>,
    /// Session GETs, and those that resume a stream with Last-Event-ID.
    pub(crate) gets: Arc<AtomicUsize>,
    pub(crate) resumes: Arc<AtomicUsize>,
    pub(crate) unblock: Arc<tokio::sync::Notify>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    pub(crate) async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let initializes = Arc::new(AtomicUsize::new(0));
        let initialized = Arc::new(AtomicUsize::new(0));
        let ready = initialized.clone();
        let expired = Arc::new(AtomicUsize::new(0));
        let lost = Arc::new(AtomicUsize::new(0));
        let blocked = Arc::new(AtomicUsize::new(0));
        let active_headers = Arc::new(AtomicUsize::new(0));
        let waiting = active_headers.clone();
        let active_sse = Arc::new(AtomicUsize::new(0));
        let active = active_sse.clone();
        let safe_calls = Arc::new(AtomicUsize::new(0));
        let unblock = Arc::new(tokio::sync::Notify::new());
        let gets = Arc::new(AtomicUsize::new(0));
        let resumes = Arc::new(AtomicUsize::new(0));
        let streams = (gets.clone(), resumes.clone());
        let (b, s, u) = (blocked.clone(), safe_calls.clone(), unblock.clone());
        let (i, e, l) = (initializes.clone(), expired.clone(), lost.clone());
        let task = tokio::spawn(async move {
            let mut peers = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted=listener.accept()=>{
                        let Ok((socket,_))=accepted else{break;};
                        let (i,e,l)=(i.clone(),e.clone(),l.clone());
                        let ready=ready.clone();
                        let active=active.clone();
                        let waiting=waiting.clone();
                    let (b,s,u)=(b.clone(),s.clone(),u.clone());
                        let streams=(streams.0.clone(),streams.1.clone());
                        peers.spawn(async move{let _=handle(socket,i,e,l,b,s,u,ready,active,waiting,streams).await;});
                    }
                    _=peers.join_next(),if !peers.is_empty()=>{}
                }
            }
        });
        Self {
            url,
            initializes,
            initialized,
            expired_mutations: expired,
            lost_mutations: lost,
            blocked,
            safe_calls,
            gets,
            resumes,
            active_sse,
            active_headers,
            unblock,
            task,
        }
    }
    pub(crate) fn configuration(&self) -> Value {
        json!({"mcpServers":{"desktop-http":{"type":"http","url":self.url,"env_http_headers":{"x-fixture-auth":"MCP_HTTP_FIXTURE_TOKEN"}}}})
    }
}
async fn reply(
    socket: &mut tokio::net::TcpStream,
    status: u16,
    body: Value,
    session: bool,
) -> std::io::Result<()> {
    let body = serde_json::to_vec(&body)?;
    let session = if session {
        "mcp-session-id: fixture-session\r\n"
    } else {
        ""
    };
    socket.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\n{session}Content-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await?;
    socket.write_all(&body).await
}
async fn handle(
    socket: tokio::net::TcpStream,
    initializes: Arc<AtomicUsize>,
    expired: Arc<AtomicUsize>,
    lost: Arc<AtomicUsize>,
    blocked: Arc<AtomicUsize>,
    safe_calls: Arc<AtomicUsize>,
    unblock: Arc<tokio::sync::Notify>,
    initialized: Arc<AtomicUsize>,
    active_sse: Arc<AtomicUsize>,
    active_headers: Arc<AtomicUsize>,
    (gets, resumes): (Arc<AtomicUsize>, Arc<AtomicUsize>),
) -> std::io::Result<()> {
    let mut reader = BufReader::new(socket);
    let mut first = String::new();
    reader.read_line(&mut first).await?;
    let method = first.split_whitespace().next().unwrap_or("").to_string();
    let mut length = 0usize;
    let mut auth = false;
    let mut session = false;
    let mut resume = false;
    let mut total = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        total += line.len();
        if total > 65536 {
            return Err(std::io::Error::other("fixture header limit"));
        }
        if line == "\r\n" || line == "\n" || line.is_empty() {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            match key.to_ascii_lowercase().as_str() {
                "content-length" => length = value.trim().parse().unwrap_or(usize::MAX),
                "x-fixture-auth" => auth = value.trim() == SENTINEL,
                "mcp-session-id" => session = value.trim() == "fixture-session",
                "last-event-id" => resume = true,
                _ => {}
            }
        }
    }
    if !auth {
        return reply(
            reader.get_mut(),
            401,
            json!({"error":"authentication required"}),
            false,
        )
        .await;
    }
    if method == "GET" {
        if !session {
            return reply(reader.get_mut(), 404, json!({}), false).await;
        }
        gets.fetch_add(1, Ordering::SeqCst);
        if resume {
            resumes.fetch_add(1, Ordering::SeqCst);
        }
        reader.get_mut().write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n: ready\n\n").await?;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            reader.get_mut().write_all(b": alive\n\n").await?;
        }
    }
    if method == "DELETE" {
        return reply(reader.get_mut(), 200, json!({}), false).await;
    }
    if length > 1024 * 1024 {
        return Err(std::io::Error::other("fixture body limit"));
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await?;
    let message: Value = serde_json::from_slice(&bytes)?;
    let id = message.get("id").cloned();
    let operation = message.get("method").and_then(Value::as_str).unwrap_or("");
    if operation == "initialize" {
        initializes.fetch_add(1, Ordering::SeqCst);
        return reply(reader.get_mut(),200,json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{},"resources":{},"prompts":{}},"serverInfo":{"name":"desktop-http","version":"1"}}}),true).await;
    }
    if !session {
        return reply(reader.get_mut(), 404, json!({}), false).await;
    }
    if id.is_none() {
        if operation == "notifications/initialized" {
            initialized.fetch_add(1, Ordering::SeqCst);
        }
        return reply(reader.get_mut(), 202, json!({}), false).await;
    }
    let result = match operation {
        "tools/list" => {
            json!({"tools":[{"name":"safe","description":"authenticated fixture","inputSchema":{"type":"object"}},{"name":"expire","inputSchema":{"type":"object"}},{"name":"lost","inputSchema":{"type":"object"}},{"name":"oversized","inputSchema":{"type":"object"}}]})
        }
        "prompts/list" => {
            json!({"prompts":[{"name":"fixture-prompt","description":"local authenticated prompt"}]})
        }
        "prompts/get" => {
            json!({"messages":[{"role":"user","content":{"type":"text","text":"authenticated prompt"}}]})
        }
        "resources/list" => {
            json!({"resources":[{"uri":"fixture://resource","name":"fixture-resource"}]})
        }
        "resources/read" => {
            json!({"contents":[{"uri":"fixture://resource","text":"authenticated resource"}]})
        }
        "tools/call" => match message
            .pointer("/params/name")
            .and_then(Value::as_str)
            .unwrap_or("")
        {
            "accepted" => {
                blocked.fetch_add(1, Ordering::SeqCst);
                reader.get_mut().write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n: accepted\n\n").await?;
                let _active = ActivePeer::new(active_sse);
                // Observe actual peer EOF: a task/socket stays counted until retirement.
                let mut byte = [0];
                tokio::select! { _=unblock.notified()=>{}, _=reader.read(&mut byte)=>{} }
                return Ok(());
            }
            // An SSE answer that closes before its response, asking for a resume in 10 ms.
            "sse-retry" => {
                blocked.fetch_add(1, Ordering::SeqCst);
                return reader.get_mut().write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\nid: 7\nretry: 10\n: pending\n\n").await;
            }
            // An SSE event over the relay's 1 MiB limit, after a resumable event id.
            "sse-oversized" => {
                blocked.fetch_add(1, Ordering::SeqCst);
                let event = format!("id: 7\ndata: {}\n\n", "X".repeat(1024 * 1024 + 16));
                reader.get_mut().write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").await?;
                return reader.get_mut().write_all(event.as_bytes()).await;
            }
            "unauthenticated" => return reply(reader.get_mut(), 401, json!({}), false).await,
            "block" => {
                blocked.fetch_add(1, Ordering::SeqCst);
                let _active = ActivePeer::new(active_headers);
                let mut byte = [0];
                tokio::select! { _=unblock.notified()=>{}, _=reader.read(&mut byte)=>return Ok(()) }
                json!({"content":[{"type":"text","text":"released"}]})
            }
            "safe" => {
                safe_calls.fetch_add(1, Ordering::SeqCst);
                json!({"content":[{"type":"text","text":"authenticated:true"}]})
            }
            "expire" => {
                expired.fetch_add(1, Ordering::SeqCst);
                return reply(reader.get_mut(), 404, json!({}), false).await;
            }
            "lost" => {
                lost.fetch_add(1, Ordering::SeqCst);
                return Ok(());
            }
            "oversized" => json!({"content":[{"type":"text","text":"X".repeat(1024*1024+16)}]}),
            _ => json!({"content":[{"type":"text","text":"authenticated:true"}]}),
        },
        _ => json!({}),
    };
    reply(
        reader.get_mut(),
        200,
        json!({"jsonrpc":"2.0","id":id,"result":result}),
        false,
    )
    .await
}
