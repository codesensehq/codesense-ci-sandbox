// SPDX-License-Identifier: MIT
//! A Hermes gateway for tests: the private control socket that grants tickets, and the
//! relay at an engine's endpoint that speaks `hermes-gateway-v1`. What the gateway says
//! of its MCP servers comes from the caller, so a test derives it from real traffic.
use super::*;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::os::unix::{fs::PermissionsExt, net::UnixListener};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

/// The receipt the gateway gives every submitted prompt, independent of its input id.
pub const ADMISSION_ID: &str = "independent-provider-id";
pub const TICKET: &str = "single-use-test-ticket";
const SESSION: &str = "s";

/// What the gateway reports of its `codesense` MCP server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct McpServer {
    pub connected: bool,
    pub tools: u64,
}

/// What reached the gateway, in order.
#[derive(Clone, Debug, Default)]
pub struct Observed {
    /// Control-socket verbs and their params.
    pub control: Vec<(String, Value)>,
    /// The `Sec-WebSocket-Protocol` each relay connection offered.
    pub subprotocols: Vec<String>,
    /// Session RPC methods and their params.
    pub rpc: Vec<(String, Value)>,
}

impl Observed {
    pub fn prompts(&self) -> Vec<&Value> {
        self.rpc
            .iter()
            .filter(|(method, _)| method == "prompt.submit")
            .map(|(_, params)| params)
            .collect()
    }
}

/// A running gateway fixture; its sockets go with it.
pub struct GatewayFixture {
    observed: Arc<Mutex<Observed>>,
    done: Arc<AtomicBool>,
    sockets: [PathBuf; 2],
}

fn lock<T>(slot: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    slot.lock().unwrap_or_else(|p| p.into_inner())
}

impl GatewayFixture {
    /// Serves `home` (made private, with the config a managed launch writes) and the relay
    /// at `endpoint`. `mcp` answers each `session.resume`.
    pub fn start(
        home: &Path,
        endpoint: &Path,
        mcp: impl Fn() -> McpServer + Send + Sync + 'static,
    ) -> Self {
        std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700)).unwrap();
        let control_path = home.join("gateway.sock");
        let control = UnixListener::bind(&control_path).unwrap();
        std::fs::set_permissions(&control_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let relay = UnixListener::bind(endpoint).unwrap();
        let mut prepared = Prepare::new(vec![], json!({}), "/test/helper");
        Hermes.prepare(&mut prepared).unwrap();
        std::fs::write(
            home.join("config.yaml"),
            serde_yaml::to_string(&prepared.settings).unwrap(),
        )
        .unwrap();
        let canonical = std::fs::canonicalize(home).unwrap();
        let fixture = Self {
            observed: Arc::default(),
            done: Arc::default(),
            sockets: [control_path, endpoint.to_path_buf()],
        };
        let (observed, done) = (fixture.observed.clone(), fixture.done.clone());
        std::thread::spawn(move || {
            for stream in control.incoming() {
                if done.load(Ordering::SeqCst) {
                    return;
                }
                if let Ok(stream) = stream {
                    let (observed, canonical) = (observed.clone(), canonical.clone());
                    std::thread::spawn(move || control_call(stream, &observed, &canonical));
                }
            }
        });
        let (observed, done) = (fixture.observed.clone(), fixture.done.clone());
        let mcp = Arc::new(mcp);
        std::thread::spawn(move || {
            for stream in relay.incoming() {
                if done.load(Ordering::SeqCst) {
                    return;
                }
                if let Ok(stream) = stream {
                    let (observed, mcp) = (observed.clone(), mcp.clone());
                    std::thread::spawn(move || relay_session(stream, &observed, &*mcp));
                }
            }
        });
        fixture
    }

    pub fn observed(&self) -> Observed {
        lock(&self.observed).clone()
    }
}

impl Drop for GatewayFixture {
    fn drop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
        // Unblocks each accept loop, which then sees `done`.
        for socket in &self.sockets {
            let _ = std::os::unix::net::UnixStream::connect(socket);
        }
    }
}

fn control_call(stream: std::os::unix::net::UnixStream, observed: &Mutex<Observed>, home: &Path) {
    let mut line = String::new();
    let Ok(clone) = stream.try_clone() else {
        return;
    };
    if std::io::BufReader::new(clone).read_line(&mut line).is_err() {
        return;
    }
    let Ok(request) = serde_json::from_str::<Value>(&line) else {
        return;
    };
    let verb = request["verb"].as_str().unwrap_or_default().to_owned();
    lock(observed)
        .control
        .push((verb.clone(), request["params"].clone()));
    let result = match verb.as_str() {
        "identify" => json!({"runtime_protocol":1,"state":"ready","instance_id":"instance",
            "api_origin":"http://127.0.0.1:49152","served_profiles":[{"home":home,"profile_id":"profile"}]}),
        "session-ticket" => json!({"runtime_protocol":1,"profile_id":"profile",
            "instance_id":"instance","ticket":TICKET}),
        _ => return,
    };
    let _ = writeln!(
        &stream,
        "{}",
        json!({"protocol":1,"id":1,"ok":true,"result":result})
    );
}

fn relay_session(
    stream: std::os::unix::net::UnixStream,
    observed: &Mutex<Observed>,
    mcp: &dyn Fn() -> McpServer,
) {
    let Ok(mut socket) = tungstenite::accept_hdr(
        stream,
        |request: &tungstenite::handshake::server::Request,
         mut response: tungstenite::handshake::server::Response| {
            assert_eq!(request.uri().path(), "/api/ws");
            let offered = request.headers()[SEC_WEBSOCKET_PROTOCOL].to_str().unwrap();
            lock(observed).subprotocols.push(offered.into());
            response
                .headers_mut()
                .insert(SEC_WEBSOCKET_PROTOCOL, "hermes-gateway-v1".parse().unwrap());
            Ok(response)
        },
    ) else {
        return;
    };
    // Ends when the client closes: a read error.
    while let Ok(frame) = socket.read() {
        let Ok(text) = frame.into_text() else {
            continue;
        };
        let Ok(request) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let method = request["method"].as_str().unwrap_or_default().to_owned();
        lock(observed)
            .rpc
            .push((method.clone(), request["params"].clone()));
        let result = match method.as_str() {
            "session.list" => json!({"sessions":[{"session_id":SESSION}]}),
            "session.resume" => {
                let server = mcp();
                json!({"session_id":SESSION,"running":false,"info":{"mcp_servers":[
                    {"name":"codesense","connected":server.connected,"tools":server.tools}]}})
            }
            "prompt.submit" => json!({"admission_id":ADMISSION_ID,"status":"queued"}),
            _ => continue,
        };
        let sent = socket.send(tungstenite::Message::text(
            json!({"jsonrpc":"2.0","id":request["id"],"result":result}).to_string(),
        ));
        if sent.is_err() {
            return;
        }
    }
}
