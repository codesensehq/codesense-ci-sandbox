// SPDX-License-Identifier: MIT
//! The private control socket grants a ticket; session RPC uses the keeper's relay.
use super::*;
use serde_json::{json, Value};
use std::{
    fs,
    io::{self, BufRead, Read, Write},
    net::SocketAddr,
    os::unix::{
        fs::{FileTypeExt, MetadataExt},
        net::UnixStream,
    },
    path::Path,
    time::{Duration, Instant},
};
use tungstenite::{client::IntoClientRequest, protocol::WebSocketConfig, Message, WebSocket};

const BUDGET: Duration = Duration::from_secs(10);
const PROTOCOL: &str = "hermes-gateway-v1";
const MAX_MESSAGE: usize = 4 * 1024 * 1024;

fn unavailable() -> io::Error {
    io::Error::other("Hermes's authenticated gateway is unavailable")
}

fn private_node(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(unavailable());
    }
    Ok(())
}

fn control(home: &Path, verb: &str, params: Value) -> io::Result<Value> {
    private_node(home)?;
    let mut path = home.join("gateway.sock");
    if !path.exists() {
        let pointer = home.join("gateway.sock.path");
        private_node(&pointer)?;
        if !fs::symlink_metadata(&pointer)?.file_type().is_file() {
            return Err(unavailable());
        }
        path = fs::read_to_string(pointer)?.trim().into();
        private_node(path.parent().ok_or_else(unavailable)?)?;
    }
    private_node(&path)?;
    if !fs::symlink_metadata(&path)?.file_type().is_socket() {
        return Err(unavailable());
    }
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(BUDGET))?;
    stream.set_write_timeout(Some(BUDGET))?;
    let mut request =
        serde_json::to_vec(&json!({"protocol":1,"id":1,"verb":verb,"params":params}))?;
    request.push(b'\n');
    stream.write_all(&request)?;
    let mut line = String::new();
    io::BufReader::new(stream)
        .take(65537)
        .read_line(&mut line)?;
    if line.len() > 65536 || !line.ends_with('\n') {
        return Err(unavailable());
    }
    let response: Value = serde_json::from_str(&line)?;
    if response["ok"] != true || response["protocol"] != 1 || response["id"] != 1 {
        return Err(unavailable());
    }
    Ok(response["result"].clone())
}

struct Endpoint {
    addr: SocketAddr,
    profile: String,
    instance: String,
}
fn discover(home: &Path) -> io::Result<Endpoint> {
    let payload = control(home, "identify", json!({}))?;
    if payload["runtime_protocol"] != 1 || payload["state"] != "ready" {
        return Err(unavailable());
    }
    let canonical = fs::canonicalize(home)?;
    let profiles = payload["served_profiles"]
        .as_array()
        .ok_or_else(unavailable)?;
    let mut matches = profiles.iter().filter(|profile| {
        profile["home"]
            .as_str()
            .and_then(|home| fs::canonicalize(home).ok())
            .as_ref()
            == Some(&canonical)
    });
    let profile = matches
        .next()
        .and_then(|profile| profile["profile_id"].as_str())
        .ok_or_else(unavailable)?;
    if matches.next().is_some() {
        return Err(unavailable());
    }
    let origin = payload["api_origin"]
        .as_str()
        .and_then(|url| url.strip_prefix("http://"))
        .ok_or_else(unavailable)?;
    let addr: SocketAddr = origin.parse().map_err(|_| unavailable())?;
    if !addr.ip().is_loopback() || addr.port() == 0 {
        return Err(unavailable());
    }
    let instance = payload["instance_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(unavailable)?;
    Ok(Endpoint {
        addr,
        profile: profile.into(),
        instance: instance.into(),
    })
}

pub(super) fn connect(
    endpoint: &Path,
    session: Option<&str>,
    home: Option<&Path>,
) -> io::Result<Box<dyn Channel>> {
    let home = home.ok_or_else(unavailable)?;
    let found = discover(home)?;
    let grant = control(
        home,
        "session-ticket",
        json!({"profile_id":found.profile,"instance_id":found.instance,"purpose":"interactive"}),
    )?;
    if grant["profile_id"] != found.profile
        || grant["instance_id"] != found.instance
        || grant["runtime_protocol"] != 1
    {
        return Err(unavailable());
    }
    let ticket = grant["ticket"]
        .as_str()
        .filter(|ticket| !ticket.is_empty())
        .ok_or_else(unavailable)?;
    let mut request = format!("ws://{}/api/ws", found.addr)
        .into_client_request()
        .map_err(|_| unavailable())?;
    request.headers_mut().insert(
        tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL,
        format!("{PROTOCOL}, hermes-gateway-ticket.{ticket}")
            .parse()
            .map_err(|_| unavailable())?,
    );
    let stream = UnixStream::connect(endpoint)?;
    stream.set_read_timeout(Some(BUDGET))?;
    stream.set_write_timeout(Some(BUDGET))?;
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE));
    // Never include handshake errors/headers in diagnostics: they contain the ticket.
    let (socket, response) = tungstenite::client::client_with_config(request, stream, Some(config))
        .map_err(|_| unavailable())?;
    if response
        .headers()
        .get("Sec-WebSocket-Protocol")
        .and_then(|h| h.to_str().ok())
        != Some(PROTOCOL)
    {
        return Err(unavailable());
    }
    let mut gateway = Gateway {
        socket,
        shown: None,
        next_id: 0,
        receipt: None,
        ready: false,
    };
    let list = gateway
        .request("session.list", json!({}))
        .map_err(|_| unavailable())?;
    let sessions = list["sessions"].as_array().ok_or_else(unavailable)?;
    let selected = match session {
        Some(id) => sessions.iter().find(|row| row["session_id"] == id),
        None if sessions.len() == 1 => sessions.first(),
        _ => None,
    };
    if let Some(id) = selected.and_then(|row| row["session_id"].as_str()) {
        let snapshot = gateway
            .request("session.resume", json!({"session_id":id}))
            .map_err(|_| unavailable())?;
        if snapshot["session_id"] == id {
            gateway.ready = lifecycle_hooks_ready(home)
                && snapshot
                    .pointer("/info/mcp_servers")
                    .and_then(Value::as_array)
                    .is_some_and(|servers| {
                        servers.iter().any(|server| {
                            server["name"] == "codesense"
                                && server["connected"] == true
                                && server["tools"].as_u64().is_some_and(|count| count > 0)
                        })
                    });
            gateway.shown = Some((id.into(), snapshot["running"] == false));
        }
    }
    Ok(Box::new(gateway))
}

fn lifecycle_hooks_ready(home: &Path) -> bool {
    let config = fs::read(home.join("config.yaml"))
        .ok()
        .and_then(|raw| serde_yaml::from_slice::<Value>(&raw).ok());
    let Some(config) = config else {
        return false;
    };
    config["hooks_auto_accept"] == true
        && OBSERVER_EVENTS.iter().all(|event| {
            config["hooks"][*event].as_array().is_some_and(|hooks| {
                hooks.iter().any(|hook| {
                    hook["command"]
                        .as_str()
                        .is_some_and(|command| command.ends_with(" --activity-hook hermes"))
                })
            })
        })
}

struct Gateway {
    socket: WebSocket<UnixStream>,
    shown: Option<(String, bool)>,
    next_id: u64,
    receipt: Option<String>,
    ready: bool,
}

enum RequestError {
    Refused,
    Lost,
}
impl Gateway {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, RequestError> {
        self.next_id += 1;
        let id = self.next_id;
        self.socket
            .send(Message::text(
                json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string(),
            ))
            .map_err(|_| RequestError::Lost)?;
        let until = Instant::now() + BUDGET;
        loop {
            let remaining = until.saturating_duration_since(Instant::now());
            if remaining.is_zero()
                || self
                    .socket
                    .get_ref()
                    .set_read_timeout(Some(remaining))
                    .is_err()
            {
                return Err(RequestError::Lost);
            }
            match self.socket.read().map_err(|_| RequestError::Lost)? {
                Message::Text(text) => {
                    let frame: Value =
                        serde_json::from_str(&text).map_err(|_| RequestError::Lost)?;
                    if frame["id"] != id {
                        continue;
                    }
                    if frame.get("error").is_some() {
                        return Err(RequestError::Refused);
                    }
                    return frame.get("result").cloned().ok_or(RequestError::Lost);
                }
                Message::Ping(_) => self.socket.flush().map_err(|_| RequestError::Lost)?,
                Message::Close(_) => return Err(RequestError::Lost),
                _ => {}
            }
        }
    }
}
impl Channel for Gateway {
    fn ready_session(&mut self) -> Option<String> {
        self.shown
            .as_ref()
            .filter(|(_, idle)| *idle && self.ready)
            .map(|(id, _)| id.clone())
    }
    fn start_turn(&mut self, text: &str, client_id: &str) -> SubmitOutcome {
        self.receipt = None;
        let Some(id) = self.ready_session() else {
            return SubmitOutcome::RefusedNoEffect;
        };
        match self.request(
            "prompt.submit",
            json!({"session_id":id,"input_id":client_id,"text":text}),
        ) {
            Ok(receipt) => {
                self.receipt = receipt["admission_id"]
                    .as_str()
                    .filter(|id| crate::activity::opaque(id))
                    .map(str::to_owned);
                // Even "queued"/"started" is not consumption evidence. Only the
                // admitted main pre_llm_call's platform_message_id can settle it.
                SubmitOutcome::Uncertain
            }
            Err(RequestError::Refused) => SubmitOutcome::RefusedNoEffect,
            Err(RequestError::Lost) => SubmitOutcome::Uncertain,
        }
    }
    fn receipt_id(&self) -> Option<&str> {
        self.receipt.as_deref()
    }
}

pub(super) fn resolve(home: &Path) -> io::Result<SocketAddr> {
    Ok(discover(home)?.addr)
}
