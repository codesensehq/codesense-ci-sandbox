// SPDX-License-Identifier: MIT
mod local_scoped;

use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

const SOCKET_PATH_SUFFIX: &str = ".codesense/mcp.sock";
const RECONNECT_FIRST_DELAY: Duration = Duration::from_millis(500);
const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(4);
const CHANNEL_BUFFER: usize = 256;

fn socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("CODESENSE_MCP_SOCK") {
        return path.into();
    }
    dirs::home_dir()
        .expect("could not determine home directory")
        .join(SOCKET_PATH_SUFFIX)
}

/// Exit with error on stderr. Claude Code treats ALL stderr as errors,
/// so only write for fatal conditions.
fn fatal(msg: &str) -> ! {
    eprintln!("{}", msg);
    std::process::exit(1);
}

/// Connect to the UDS. Returns the stream or None if connection failed.
async fn connect_uds(path: &PathBuf) -> Option<UnixStream> {
    UnixStream::connect(path).await.ok()
}

/// Reconnect with exponential backoff, capped at [`RECONNECT_MAX_DELAY`], for as
/// long as the client stays: CodeSense may be restarting or busy, and giving up
/// would cost the agent its CodeSense tools until it is restarted. A request the
/// client sends meanwhile is answered with an error at once rather than left
/// waiting. `None` once stdin closes.
///
/// `delay` is the wait before the next attempt and grows with each one, kept by
/// the caller so a connection that fails its handshake does not reset the backoff.
async fn reconnect(
    endpoint: &Endpoint,
    stdin_rx: &mut mpsc::Receiver<String>,
    stdout_tx: &mpsc::Sender<String>,
    delay: &mut Duration,
) -> Option<UnixStream> {
    loop {
        let wait = tokio::time::sleep(*delay);
        *delay = (*delay * 2).min(RECONNECT_MAX_DELAY);
        tokio::pin!(wait);
        loop {
            tokio::select! {
                () = &mut wait => break,
                msg = stdin_rx.recv() => {
                    let message = envelope(&msg?);
                    // A notification, or a null id (read as none), gets no answer.
                    if message.method.is_some() && message.id.is_some() {
                        let advice = if message.can_retry_with_request_id() {
                            "CodeSense is not reachable right now. Retry with the same requestId once it reconnects."
                        } else {
                            "CodeSense is not reachable right now; this call was not sent. Retry once it reconnects."
                        };
                        let answer = error_answer(message.id.unwrap(), advice);
                        if stdout_tx.send(answer).await.is_err() {
                            return None;
                        }
                    }
                }
            }
        }

        if let Some(stream) = endpoint.connect().await {
            return Some(stream);
        }
    }
}

#[cfg(test)]
async fn reconnect_uds(
    path: &PathBuf,
    stdin_rx: &mut mpsc::Receiver<String>,
    stdout_tx: &mpsc::Sender<String>,
    delay: &mut Duration,
) -> Option<UnixStream> {
    reconnect(&Endpoint::Socket(path.clone()), stdin_rx, stdout_tx, delay).await
}

/// Send a line to a UDS write half and flush.
async fn send_line(
    writer: &mut tokio::io::WriteHalf<UnixStream>,
    msg: &str,
) -> Result<(), std::io::Error> {
    writer.write_all(msg.as_bytes()).await?;
    if !msg.ends_with('\n') {
        writer.write_all(b"\n").await?;
    }
    writer.flush().await
}

/// Read one line from a UDS read half.
async fn recv_line(
    reader: &mut BufReader<tokio::io::ReadHalf<UnixStream>>,
) -> Result<String, std::io::Error> {
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "connection closed",
        ));
    }
    Ok(line.trim_end_matches('\n').to_string())
}

/// The env var a CodeSense terminal tab exports to everything it runs.
const TAB_SESSION_ENV: &str = "CODESENSE_TAB_SESSION_ID";

/// The tab session this bridge runs under, when a CodeSense tab started it.
fn tab_session_from_env() -> Option<String> {
    std::env::var(TAB_SESSION_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Replace identity metadata with the identity supplied by CodeSense's launch.
fn with_session_meta(init_msg: &str, session: Option<&str>) -> String {
    let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(init_msg) else {
        return init_msg.to_string();
    };
    let Some(params) = parsed.get_mut("params").and_then(|p| p.as_object_mut()) else {
        return init_msg.to_string();
    };
    let meta = params
        .entry("_meta")
        .or_insert_with(|| serde_json::Value::Object(Default::default()));
    let Some(meta) = meta.as_object_mut() else {
        return init_msg.to_string();
    };
    let changed =
        meta.remove("codesenseSessionId").is_some() | meta.remove("codesenseLaunchToken").is_some();
    if let Some(session) = session {
        meta.insert("codesenseSessionId".into(), session.into());
    } else if !changed {
        return init_msg.to_string();
    }
    parsed.to_string()
}

/// Perform MCP initialize handshake: send init, relay response to stdout,
/// then wait for and forward notifications/initialized from stdin.
async fn do_initial_handshake(
    writer: &mut tokio::io::WriteHalf<UnixStream>,
    reader: &mut BufReader<tokio::io::ReadHalf<UnixStream>>,
    stdin_rx: &mut mpsc::Receiver<String>,
    stdout_tx: &mpsc::Sender<String>,
    session: Option<&str>,
) -> Result<String, ()> {
    // Get initialize from stdin. A request before it (a version probe such as
    // `server/discover`, or a ping) is answered here: CodeSense speaks only the
    // `initialize` handshake, so the client falls back to it on this same stream.
    let init_msg = loop {
        let Some(msg) = stdin_rx.recv().await else {
            return Err(());
        };
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&msg) else {
            fatal("An MCP message before initialize is not valid JSON.");
        };
        match parsed.get("method").and_then(|m| m.as_str()) {
            // Returned as stored, so a reconnect replays the same identity.
            Some("initialize") => break with_session_meta(&msg, session),
            Some(method) => {
                if let Some(answer) = pre_initialize_answer(method, parsed.get("id")) {
                    let _ = stdout_tx.send(answer).await;
                }
            }
            None => fatal("Expected 'initialize' as first MCP message."),
        }
    };

    // Forward to UDS
    if send_line(writer, &init_msg).await.is_err() {
        fatal("CodeSense disconnected during initialization.");
    }

    // Read response, forward to stdout
    match recv_line(reader).await {
        Ok(response) => {
            let _ = stdout_tx.send(response).await;
        }
        Err(_) => fatal("CodeSense disconnected during initialization."),
    }

    // Get notifications/initialized from stdin and forward
    if let Some(notif) = stdin_rx.recv().await {
        if send_line(writer, &notif).await.is_err() {
            fatal("CodeSense disconnected during initialization.");
        }
    }

    Ok(init_msg)
}

/// The answer to a request before `initialize`: a ping's empty result, else "method
/// not found". A notification (no id) has none.
fn pre_initialize_answer(method: &str, id: Option<&serde_json::Value>) -> Option<String> {
    let id = id.filter(|id| !id.is_null())?;
    let answer = if method == "ping" {
        serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": {} })
    } else {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": "Method not found" },
        })
    };
    Some(answer.to_string())
}

/// How long a reconnect waits for CodeSense to answer the replayed `initialize`.
const REPLAY_TIMEOUT: Duration = Duration::from_secs(10);

/// The JSON-RPC error code a request gets when its connection was lost before it
/// was answered.
const CONNECTION_LOST: i64 = -32000;

/// The top-level fields of a JSON-RPC message the relay needs. Every other field
/// is skipped, so a large tool result is not built into a value.
#[derive(Default, serde::Deserialize)]
struct Envelope {
    id: Option<serde_json::Value>,
    method: Option<String>,
    params: Option<serde_json::Value>,
}

impl Envelope {
    fn can_retry_with_request_id(&self) -> bool {
        self.method.as_deref() == Some("tools/call")
            && matches!(
                self.params
                    .as_ref()
                    .and_then(|p| p.get("name"))
                    .and_then(|n| n.as_str()),
                Some("send_message" | "reply" | "signal_done" | "send_role_message")
            )
            && self
                .params
                .as_ref()
                .and_then(|p| p.get("arguments"))
                .and_then(|a| a.get("requestId").or_else(|| a.get("request_id")))
                .and_then(|id| id.as_str())
                .is_some_and(|id| !id.trim().is_empty())
    }

    /// The `params.requestId` of a `notifications/cancelled`.
    fn cancelled_request(self) -> Option<serde_json::Value> {
        let mut params = self.params?;
        params.get_mut("requestId").map(serde_json::Value::take)
    }
}

fn envelope(msg: &str) -> Envelope {
    serde_json::from_str(msg).unwrap_or_default()
}

/// The requests sent to CodeSense that have no answer yet, keyed by their id as
/// written, so a lost connection can answer each of them instead of leaving the
/// client waiting forever.
#[derive(Default)]
struct Pending(std::collections::HashMap<String, (serde_json::Value, bool)>);

impl Pending {
    /// Note a message on its way from the client to CodeSense.
    fn sent(&mut self, msg: &str) {
        let message = envelope(msg);
        match (message.method.as_deref(), &message.id) {
            (Some("notifications/cancelled"), _) => {
                if let Some(id) = message.cancelled_request() {
                    self.0.remove(&id.to_string());
                }
            }
            (Some(_), Some(id)) => {
                self.0.insert(
                    id.to_string(),
                    (id.clone(), message.can_retry_with_request_id()),
                );
            }
            _ => {}
        }
    }

    /// Note a message on its way from CodeSense to the client.
    fn received(&mut self, msg: &str) {
        let message = envelope(msg);
        if message.method.is_none() {
            if let Some(id) = message.id {
                self.0.remove(&id.to_string());
            }
        }
    }

    /// An error answer for every request still waiting, which are then forgotten.
    fn lost(&mut self) -> Vec<String> {
        self.0
            .drain()
            .map(|(_, (id, can_retry_with_request_id))| {
                let advice = if can_retry_with_request_id {
                    "The connection to CodeSense was lost before this request was answered; it may have run. Retry with the same requestId."
                } else {
                    "The connection to CodeSense was lost before this request was answered; it may have run."
                };
                error_answer(id, advice)
            })
            .collect()
    }
}

/// A JSON-RPC error answer to the request `id`, for one CodeSense could not answer.
fn error_answer(id: serde_json::Value, message: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": CONNECTION_LOST, "message": message },
    })
    .to_string()
}

/// One whole line out of `buf`, or `None` while it holds only part of one. A line
/// is removed once taken; a partial line stays for the next read to finish.
fn take_line(buf: &mut Vec<u8>) -> Option<String> {
    if buf.last() != Some(&b'\n') {
        return None;
    }
    let line = String::from_utf8_lossy(&buf[..buf.len() - 1]).into_owned();
    buf.clear();
    Some(line)
}

/// Relay stdin→UDS and UDS→stdout over one connection until it drops (`Some`
/// with why) or stdin or stdout closes (`None`).
async fn relay(
    stdin_rx: &mut mpsc::Receiver<String>,
    write_half: &mut tokio::io::WriteHalf<UnixStream>,
    uds_reader: &mut BufReader<tokio::io::ReadHalf<UnixStream>>,
    stdout_tx: &mpsc::Sender<String>,
    pending: &mut Pending,
) -> Option<String> {
    // Bytes of CodeSense's next line read so far. It outlives each select!, so a
    // large reply split across reads is not cut when a client message wins the
    // race (`read_until` keeps what it read in `uds_buf`; `read_line` would drop it).
    let mut uds_buf = Vec::new();
    loop {
        tokio::select! {
            msg = stdin_rx.recv() => {
                match msg {
                    Some(m) => {
                        pending.sent(&m);
                        if let Err(error) = send_line(write_half, &m).await {
                            return Some(format!("write to CodeSense failed: {error}"));
                        }
                    }
                    None => return None, // stdin closed — clean exit
                }
            }
            result = uds_reader.read_until(b'\n', &mut uds_buf) => {
                match result {
                    Ok(0) => return Some("CodeSense closed the connection".into()),
                    Err(error) => return Some(format!("read from CodeSense failed: {error}")),
                    Ok(_) => {
                        // A line cut short by the connection closing is dropped.
                        let Some(line) = take_line(&mut uds_buf) else {
                            return Some("CodeSense closed the connection mid-message".into());
                        };
                        pending.received(&line);
                        if stdout_tx.send(line).await.is_err() {
                            return None; // stdout closed — exit
                        }
                    }
                }
            }
        }
    }
}

/// The bridge's records in the disconnect log CodeSense's terminal server also
/// writes (`<home>/logs/mcp-bridge-disconnect.log`, same line format), so a lost
/// MCP connection can be diagnosed after the fact.
struct DisconnectLog {
    path: PathBuf,
    session: String,
}

/// Size at which the disconnect log moves aside to `.1`, as the server's does.
const DISCONNECT_LOG_MAX_BYTES: u64 = 1024 * 1024;

impl DisconnectLog {
    /// The log beside the socket's directory, under the tab's session when known.
    fn new(socket: &std::path::Path, session: Option<&str>) -> Option<Self> {
        let path = socket
            .parent()?
            .join("logs")
            .join("mcp-bridge-disconnect.log");
        // One token, so a record stays one parseable line.
        let session = session.map_or_else(
            || format!("bridge-{}", std::process::id()),
            |s| s.replace(char::is_whitespace, "_"),
        );
        Some(Self { path, session })
    }

    /// Append one record. Best effort: a bridge must never fail over its log.
    fn record(&self, event: &str, reason: &str) {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let reason = reason.replace(['\r', '\n'], " ");
        let line = format!(
            "{timestamp} session={} event={event} reason={reason}\n",
            self.session
        );
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if std::fs::metadata(&self.path).is_ok_and(|m| m.len() >= DISCONNECT_LOG_MAX_BYTES) {
            let _ = std::fs::rename(&self.path, self.path.with_extension("log.1"));
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.path)
        {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

/// Longest server refusal a reconnect record carries.
const REFUSAL_MAX_CHARS: usize = 200;

/// Replay initialize handshake to a new CodeSense instance after reconnect.
/// Consumes the response (does NOT relay to Claude Code). Errs with the reason to
/// record when the connection fails or CodeSense refuses the replayed initialize.
async fn replay_handshake(
    writer: &mut tokio::io::WriteHalf<UnixStream>,
    reader: &mut BufReader<tokio::io::ReadHalf<UnixStream>>,
    stored_init: &str,
) -> Result<(), String> {
    let failed = |error: std::io::Error| format!("replayed initialize failed: {error}");
    send_line(writer, stored_init).await.map_err(failed)?;

    // Read the response (NOT relayed to Claude Code): CodeSense closes a
    // connection whose initialize it refuses.
    let response = recv_line(reader).await.map_err(failed)?;
    if let Some(error) = serde_json::from_str::<serde_json::Value>(&response)
        .ok()
        .and_then(|response| response.get("error").cloned())
    {
        let message = error
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("no message");
        let message: String = message.chars().take(REFUSAL_MAX_CHARS).collect();
        return Err(format!("replayed initialize refused: {message}"));
    }

    // Send notifications/initialized
    send_line(
        writer,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    )
    .await
    .map_err(failed)
}

fn main() {
    codesense_host_proto::nofile::raise(10_240);
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("create bridge runtime")
        .block_on(run());
}

async fn run() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--local-scope") {
        if let Err(error) = local_scoped::run(&args).await {
            fatal(&error);
        }
        return;
    }
    let (remote_path, server_id) = remote_arguments(&args).unwrap_or_else(|e| fatal(&e));
    let path = remote_path.clone().unwrap_or_else(socket_path);
    if remote_path.is_some()
        || std::env::var_os(codesense_host_proto::reverse_mcp::BROKER_ENV).is_some()
    {
        if let Err(error) = remote_bridge(path, server_id).await {
            fatal(&format!(
                "Remote CodeSense MCP closed; a pending operation may have executed: {error}"
            ));
        }
        return;
    }

    run_local(Endpoint::Socket(path)).await;
}

enum Endpoint {
    Socket(PathBuf),
    Scoped { descriptor: PathBuf, server: String },
}

impl Endpoint {
    async fn connect(&self) -> Option<UnixStream> {
        match self {
            Self::Socket(path) => connect_uds(path).await,
            Self::Scoped { descriptor, server } => {
                local_scoped::connect(descriptor, server).await.ok()
            }
        }
    }

    fn path(&self) -> &PathBuf {
        match self {
            Self::Socket(path) => path,
            Self::Scoped { descriptor, .. } => descriptor,
        }
    }
}

async fn run_local(endpoint: Endpoint) {
    // Long-lived channels
    let (stdin_tx, stdin_rx) = mpsc::channel::<String>(CHANNEL_BUFFER);
    let (stdout_tx, mut stdout_rx) = mpsc::channel::<String>(CHANNEL_BUFFER);

    // stdin_reader: long-lived, MUST keep reading to prevent 64KB pipe buffer backup.
    // Drops stdin_tx on EOF which closes the channel — main loop detects this and exits.
    // This is the primary shutdown mechanism. Claude Code's sequence: close stdin → SIGTERM → SIGKILL.
    // The bridge exits promptly on stdin EOF, so SIGTERM/SIGKILL are rarely needed.
    tokio::spawn(async move {
        let stdin = tokio::io::stdin();
        let mut reader = BufReader::new(stdin);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    let msg = line.trim_end_matches('\n').to_string();
                    if stdin_tx.send(msg).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        // stdin closed — drop sender, channel closes, main loop exits
    });

    // stdout_writer: long-lived, flushes remaining messages before exit
    tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(msg) = stdout_rx.recv().await {
            if stdout.write_all(msg.as_bytes()).await.is_err() {
                break;
            }
            if stdout.write_all(b"\n").await.is_err() {
                break;
            }
            if stdout.flush().await.is_err() {
                break;
            }
        }
        let _ = stdout.flush().await;
    });

    run_channel(endpoint, stdin_rx, stdout_tx, tab_session_from_env()).await;
}

async fn run_channel(
    endpoint: Endpoint,
    mut stdin_rx: mpsc::Receiver<String>,
    stdout_tx: mpsc::Sender<String>,
    tab_session: Option<String>,
) {
    let stream = match endpoint.connect().await {
        Some(stream) => stream,
        None => match reconnect(
            &endpoint,
            &mut stdin_rx,
            &stdout_tx,
            &mut RECONNECT_FIRST_DELAY.clone(),
        )
        .await
        {
            Some(stream) => stream,
            None => return,
        },
    };

    // Split stream and perform initial MCP handshake
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut uds_reader = BufReader::new(read_half);

    let stored_init = match do_initial_handshake(
        &mut write_half,
        &mut uds_reader,
        &mut stdin_rx,
        &stdout_tx,
        tab_session.as_deref(),
    )
    .await
    {
        Ok(init) => init,
        Err(_) => fatal("Initialization failed."),
    };

    // Main relay loop with reconnection.
    // stdin_rx stays in this scope — never moved into a spawn.
    let log = DisconnectLog::new(endpoint.path(), tab_session.as_deref());
    let record = |event: &str, reason: &str| {
        if let Some(log) = &log {
            log.record(event, reason);
        }
    };
    let mut pending = Pending::default();
    loop {
        let Some(why) = relay(
            &mut stdin_rx,
            &mut write_half,
            &mut uds_reader,
            &stdout_tx,
            &mut pending,
        )
        .await
        else {
            break;
        };

        // The new connection is a new server session that knows nothing of the old
        // one's requests, so each is answered now rather than never.
        let lost = pending.lost();
        record(
            "bridge_disconnect",
            &format!(
                "{why}; {} waiting request(s) answered with an error",
                lost.len()
            ),
        );
        for answer in lost {
            if stdout_tx.send(answer).await.is_err() {
                return;
            }
        }

        // Reconnect — stdin_rx still ours. A connection that does not finish the
        // replayed handshake is dropped and the next one tried, the backoff
        // carrying on rather than starting over.
        let away = std::time::Instant::now();
        let mut delay = RECONNECT_FIRST_DELAY;
        loop {
            let Some(new_stream) =
                reconnect(&endpoint, &mut stdin_rx, &stdout_tx, &mut delay).await
            else {
                return; // stdin closed — clean exit
            };
            let (new_read, mut new_write) = tokio::io::split(new_stream);
            let mut new_reader = BufReader::new(new_read);

            let replayed = tokio::time::timeout(
                REPLAY_TIMEOUT,
                replay_handshake(&mut new_write, &mut new_reader, &stored_init),
            )
            .await;
            match replayed {
                Ok(Ok(())) => {
                    record(
                        "bridge_reconnect",
                        &format!("connected again after {}s", away.elapsed().as_secs()),
                    );
                    // Swap in new connection for next relay iteration
                    write_half = new_write;
                    uds_reader = new_reader;
                    break;
                }
                Ok(Err(reason)) => record("bridge_reconnect_failed", &reason),
                Err(_) => record(
                    "bridge_reconnect_failed",
                    "CodeSense did not answer the replayed initialize in time",
                ),
            }
        }
    }
}

/// Remote mode authenticates once and never replays a possibly executed request.
fn remote_arguments(args: &[std::ffi::OsString]) -> Result<(Option<PathBuf>, String), String> {
    use codesense_host_proto::reverse_mcp as wire;
    let (path, id) = match args {
        [] => (None, wire::default_server()),
        [flag, path] if flag == "--remote-socket" => {
            (Some(PathBuf::from(path)), wire::default_server())
        }
        [flag, path, selector, id] if flag == "--remote-socket" && selector == "--server-id" => (
            Some(PathBuf::from(path)),
            id.to_str().ok_or("invalid server selector")?.to_owned(),
        ),
        _ => {
            return Err(
                "usage: codesense-bridge [--remote-socket <path> [--server-id <id>]]".into(),
            )
        }
    };
    wire::validate_server_id(&id)?;
    Ok((path, id))
}

async fn connect_remote(path: PathBuf, server_id: String) -> Result<UnixStream, String> {
    use codesense_host_proto::{broker, reverse_mcp as wire};
    let stream =
        tokio::task::spawn_blocking(move || -> Result<std::os::unix::net::UnixStream, String> {
            use std::io::{Read, Write};
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut stream =
                std::os::unix::net::UnixStream::connect(path).map_err(|e| e.to_string())?;
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .map_err(|e| e.to_string())?;
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .map_err(|e| e.to_string())?;
            // Standard stdio clients close inherited descriptors. Reacquire the
            // same endpoint using the daemon's kernel-verified process lineage.
            use std::os::fd::AsRawFd;
            wire::write_mode(&mut stream, wire::MODE_REACQUIRE).map_err(|e| e.to_string())?;
            let capability = broker::receive_fd(&stream).map_err(|e| e.to_string())?;
            let selected = server_id != wire::CODESENSE_SERVER;
            wire::write_mode(
                &mut stream,
                if selected {
                    wire::MODE_MCP_SERVER
                } else {
                    wire::MODE_MCP
                },
            )
            .map_err(|e| e.to_string())?;
            broker::send_fd(&stream, capability.as_raw_fd()).map_err(|e| e.to_string())?;
            let mut challenge = Vec::new();
            loop {
                let remaining = deadline
                    .checked_duration_since(std::time::Instant::now())
                    .filter(|remaining| !remaining.is_zero())
                    .ok_or("broker authentication deadline expired")?;
                stream
                    .set_read_timeout(Some(remaining))
                    .map_err(|e| e.to_string())?;
                let mut byte = [0];
                stream.read_exact(&mut byte).map_err(|e| e.to_string())?;
                if challenge.len() >= 128 {
                    return Err("invalid broker challenge".into());
                }
                challenge.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
            if !challenge.starts_with(format!("{} ", wire::AUDIENCE).as_bytes()) {
                return Err("broker audience mismatch".into());
            }
            let remaining = deadline
                .checked_duration_since(std::time::Instant::now())
                .filter(|remaining| !remaining.is_zero())
                .ok_or("broker authentication deadline expired")?;
            stream
                .set_write_timeout(Some(remaining))
                .map_err(|e| e.to_string())?;
            stream.write_all(&challenge).map_err(|e| e.to_string())?;
            challenge.fill(0);
            if selected {
                wire::write_server_selector(&mut stream, &server_id)?;
            }
            stream.set_read_timeout(None).map_err(|e| e.to_string())?;
            stream.set_write_timeout(None).map_err(|e| e.to_string())?;
            stream.set_nonblocking(true).map_err(|e| e.to_string())?;
            Ok(stream)
        })
        .await
        .map_err(|e| e.to_string())??;
    UnixStream::from_std(stream).map_err(|e| e.to_string())
}

/// One stream for the agent's whole session. The machine keeps it open while
/// the desktop is away, answering it itself, and hands it back when the
/// desktop returns: it ends only when the machine retires the
/// session's MCP (the session ended or was released).
async fn remote_bridge(path: PathBuf, server_id: String) -> Result<(), String> {
    let stream = connect_remote(path, server_id).await?;
    let (mut read, mut write) = stream.into_split();
    let upload = async {
        let mut stdin = tokio::io::stdin();
        tokio::io::copy(&mut stdin, &mut write).await
    };
    let download = async {
        let mut stdout = tokio::io::stdout();
        tokio::io::copy(&mut read, &mut stdout).await
    };
    tokio::select! { result=upload => result.map(|_|()).map_err(|e|e.to_string()), result=download => {result.map_err(|e|e.to_string())?;Err("this machine retired the session's MCP".into())} }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio::net::UnixStream;

    #[test]
    fn remote_selector_is_explicit_and_legacy_defaults_to_codesense() {
        let args = |values: &[&str]| {
            values
                .iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>()
        };
        assert_eq!(remote_arguments(&[]).unwrap().1, "codesense");
        assert_eq!(
            remote_arguments(&args(&["--remote-socket", "/s"]))
                .unwrap()
                .1,
            "codesense"
        );
        assert_eq!(
            remote_arguments(&args(&[
                "--remote-socket",
                "/s",
                "--server-id",
                "custom name"
            ]))
            .unwrap()
            .1,
            "custom name"
        );
        assert!(remote_arguments(&args(&["--server-id", "custom"])).is_err());
        assert!(remote_arguments(&args(&[
            "--remote-socket",
            "/s",
            "--server-id",
            "bad\nname"
        ]))
        .is_err());
    }

    #[test]
    fn test_socket_path_ends_with_expected_suffix() {
        let path = socket_path();
        assert!(path.ends_with(".codesense/mcp.sock"));
    }

    #[test]
    fn test_socket_path_is_absolute() {
        let path = socket_path();
        assert!(path.is_absolute());
    }

    #[tokio::test]
    async fn test_send_line_appends_newline() {
        let (left, right) = UnixStream::pair().unwrap();
        let (_, mut write_half) = tokio::io::split(left);
        let (read_half, _) = tokio::io::split(right);
        let mut reader = BufReader::new(read_half);

        send_line(&mut write_half, "hello").await.unwrap();
        drop(write_half); // close so reader sees EOF after the line

        let mut buf = String::new();
        reader.read_line(&mut buf).await.unwrap();
        assert_eq!(buf, "hello\n");
    }

    #[tokio::test]
    async fn test_send_line_does_not_double_newline() {
        let (left, right) = UnixStream::pair().unwrap();
        let (_, mut write_half) = tokio::io::split(left);
        let (read_half, _) = tokio::io::split(right);

        send_line(&mut write_half, "hello\n").await.unwrap();
        drop(write_half);

        let mut buf = Vec::new();
        let mut reader = tokio::io::BufReader::new(read_half);
        reader.read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf, b"hello\n");
    }

    #[tokio::test]
    async fn test_recv_line_reads_complete_message() {
        let (left, right) = UnixStream::pair().unwrap();
        let (_, mut write_half) = tokio::io::split(left);
        let (read_half, _) = tokio::io::split(right);
        let mut reader = BufReader::new(read_half);

        write_half
            .write_all(b"{\"method\":\"test\"}\n")
            .await
            .unwrap();
        write_half.flush().await.unwrap();

        let line = recv_line(&mut reader).await.unwrap();
        assert_eq!(line, "{\"method\":\"test\"}");
    }

    #[tokio::test]
    async fn test_recv_line_strips_trailing_newline() {
        let (left, right) = UnixStream::pair().unwrap();
        let (_, mut write_half) = tokio::io::split(left);
        let (read_half, _) = tokio::io::split(right);
        let mut reader = BufReader::new(read_half);

        write_half.write_all(b"line with newline\n").await.unwrap();
        write_half.flush().await.unwrap();

        let line = recv_line(&mut reader).await.unwrap();
        assert_eq!(line, "line with newline");
    }

    #[tokio::test]
    async fn test_recv_line_returns_eof_on_closed_connection() {
        let (left, right) = UnixStream::pair().unwrap();
        drop(left); // close writer immediately
        let (read_half, _) = tokio::io::split(right);
        let mut reader = BufReader::new(read_half);

        let err = recv_line(&mut reader).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn test_send_recv_round_trip() {
        let (left, right) = UnixStream::pair().unwrap();
        let (_, mut write_half) = tokio::io::split(left);
        let (read_half, _) = tokio::io::split(right);
        let mut reader = BufReader::new(read_half);

        let msg = r#"{"jsonrpc":"2.0","method":"initialize","id":0}"#;
        send_line(&mut write_half, msg).await.unwrap();

        let received = recv_line(&mut reader).await.unwrap();
        assert_eq!(received, msg);
    }

    #[tokio::test]
    async fn test_replay_handshake_sends_init_and_initialized() {
        let (left, right) = UnixStream::pair().unwrap();
        let (read_left, mut write_left) = tokio::io::split(left);
        let (read_right, write_right) = tokio::io::split(right);

        let mut reader_left = BufReader::new(read_left);
        let mut writer_right = write_right;
        let mut reader_right = BufReader::new(read_right);

        let stored_init = r#"{"jsonrpc":"2.0","method":"initialize","id":0,"params":{}}"#;

        // Simulate server: read init, send response, read initialized
        let server = tokio::spawn(async move {
            // Read the stored initialize
            let mut line = String::new();
            reader_right.read_line(&mut line).await.unwrap();
            let init: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(init["method"], "initialize");

            // Send response
            writer_right
                .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{\"capabilities\":{}}}\n")
                .await
                .unwrap();
            writer_right.flush().await.unwrap();

            // Read notifications/initialized
            line.clear();
            reader_right.read_line(&mut line).await.unwrap();
            let notif: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(notif["method"], "notifications/initialized");
        });

        // Client side: replay handshake
        replay_handshake(&mut write_left, &mut reader_left, stored_init)
            .await
            .unwrap();

        server.await.unwrap();
    }

    /// A replayed initialize CodeSense refuses is a failed reconnect, logged with
    /// CodeSense's reason; the bridge goes on retrying until one is accepted.
    #[tokio::test]
    async fn a_refused_replayed_initialize_is_a_failed_reconnect() {
        let dir = std::env::temp_dir().join(format!("bridge-refused-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mcp.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let (stdin_tx, stdin_rx) = mpsc::channel::<String>(16);
        let (stdout_tx, _stdout_rx) = mpsc::channel::<String>(16);
        stdin_tx
            .send(r#"{"jsonrpc":"2.0","method":"initialize","id":0,"params":{}}"#.into())
            .await
            .unwrap();
        stdin_tx
            .send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.into())
            .await
            .unwrap();
        let bridge = tokio::spawn(run_channel(
            Endpoint::Socket(path.clone()),
            stdin_rx,
            stdout_tx,
            Some("tab-7".into()),
        ));
        let refusal = format!("can't confirm this tab {}", "x".repeat(300));
        let answers = [
            r#"{"jsonrpc":"2.0","id":0,"result":{}}"#.to_string(),
            serde_json::json!({"jsonrpc":"2.0","id":0,"error":{"code":-32603,"message":refusal}})
                .to_string(),
            r#"{"jsonrpc":"2.0","id":0,"result":{}}"#.to_string(),
        ];
        let mut accepted = None;
        for (i, answer) in answers.iter().enumerate() {
            let (stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
                .await
                .expect("the bridge connects")
                .unwrap();
            let (read, mut write) = tokio::io::split(stream);
            let mut reader = BufReader::new(read);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let init: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(init["method"], "initialize");
            write
                .write_all(format!("{answer}\n").as_bytes())
                .await
                .unwrap();
            if i == 1 {
                // Refused: CodeSense closes the connection.
                continue;
            }
            line.clear();
            reader.read_line(&mut line).await.unwrap();
            assert!(line.contains("notifications/initialized"));
            // The first connection is dropped to make the bridge reconnect; the
            // last is kept so the bridge settles on it.
            if i == 2 {
                accepted = Some((reader, write));
            }
        }
        let log = dir.join("logs").join("mcp-bridge-disconnect.log");
        let text = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let text = std::fs::read_to_string(&log).unwrap_or_default();
                if text.contains("event=bridge_reconnect ") {
                    return text;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the accepted replay is logged");
        let events: Vec<&str> = text
            .lines()
            .map(|line| line.split(" event=").nth(1).unwrap())
            .collect();
        assert_eq!(events.len(), 3, "{text}");
        assert!(events[0].starts_with("bridge_disconnect "), "{text}");
        let expected = format!(
            "bridge_reconnect_failed reason=replayed initialize refused: {}",
            refusal.chars().take(REFUSAL_MAX_CHARS).collect::<String>()
        );
        assert_eq!(events[1], expected);
        assert!(
            events[2].starts_with("bridge_reconnect reason=connected again"),
            "{text}"
        );
        drop(stdin_tx);
        drop(accepted);
        tokio::time::timeout(Duration::from_secs(10), bridge)
            .await
            .expect("the bridge ends with stdin")
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_replayed_initialize_answered_with_an_error_is_refused() {
        let (left, right) = UnixStream::pair().unwrap();
        let (read_left, mut write_left) = tokio::io::split(left);
        let mut reader_left = BufReader::new(read_left);
        let server = tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(right);
            let mut line = String::new();
            BufReader::new(read).read_line(&mut line).await.unwrap();
            write
                .write_all(
                    b"{\"jsonrpc\":\"2.0\",\"id\":0,\"error\":{\"message\":\"retry shortly\"}}\n",
                )
                .await
                .unwrap();
        });
        let stored_init = r#"{"jsonrpc":"2.0","method":"initialize","id":0,"params":{}}"#;
        assert_eq!(
            replay_handshake(&mut write_left, &mut reader_left, stored_init).await,
            Err("replayed initialize refused: retry shortly".into())
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn test_do_initial_handshake_validates_initialize_method() {
        let (left, right) = UnixStream::pair().unwrap();
        let (read_left, write_left) = tokio::io::split(left);
        let (read_right, mut write_right) = tokio::io::split(right);

        let mut writer = write_left;
        let mut reader = BufReader::new(read_left);
        let reader_right = BufReader::new(read_right);

        let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(16);
        let (stdout_tx, mut stdout_rx) = mpsc::channel::<String>(16);

        let init_msg = r#"{"jsonrpc":"2.0","method":"initialize","id":0,"params":{}}"#;
        let notif_msg = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;

        // Send messages that the handshake will read from stdin
        stdin_tx.send(init_msg.to_string()).await.unwrap();
        stdin_tx.send(notif_msg.to_string()).await.unwrap();

        // Simulate server: read init, send response, read initialized
        let server = tokio::spawn(async move {
            let mut reader = reader_right;
            let mut line = String::new();

            // Read initialize
            reader.read_line(&mut line).await.unwrap();

            // Send response
            write_right
                .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{\"capabilities\":{}}}\n")
                .await
                .unwrap();
            write_right.flush().await.unwrap();

            // Read notifications/initialized
            line.clear();
            reader.read_line(&mut line).await.unwrap();
        });

        let result =
            do_initial_handshake(&mut writer, &mut reader, &mut stdin_rx, &stdout_tx, None).await;

        assert!(result.is_ok());
        let stored = result.unwrap();
        assert_eq!(stored, init_msg);

        // Verify response was forwarded to stdout channel
        let response = stdout_rx.recv().await.unwrap();
        assert!(response.contains("capabilities"));

        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_version_probe_is_answered_and_initialize_goes_on_to_codesense() {
        let (left, right) = UnixStream::pair().unwrap();
        let (read_left, mut writer) = tokio::io::split(left);
        let (read_right, mut write_right) = tokio::io::split(right);
        let mut reader = BufReader::new(read_left);
        let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(16);
        let (stdout_tx, mut stdout_rx) = mpsc::channel::<String>(16);

        let probe = r#"{"jsonrpc":"2.0","id":"server-discover-probe-1","method":"server/discover","params":{}}"#;
        let cancelled = r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{}}"#;
        let ping = r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#;
        let init_msg = r#"{"jsonrpc":"2.0","method":"initialize","id":0,"params":{}}"#;
        let notif_msg = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        for line in [probe, cancelled, ping, init_msg, notif_msg] {
            stdin_tx.send(line.to_string()).await.unwrap();
        }

        let server = tokio::spawn(async move {
            let mut reader = BufReader::new(read_right);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            assert!(line.contains("\"initialize\""), "{line}");
            write_right
                .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{\"capabilities\":{}}}\n")
                .await
                .unwrap();
            line.clear();
            reader.read_line(&mut line).await.unwrap();
            assert!(line.contains("notifications/initialized"), "{line}");
        });

        let within = std::time::Duration::from_secs(5);
        let stored = tokio::time::timeout(
            within,
            do_initial_handshake(&mut writer, &mut reader, &mut stdin_rx, &stdout_tx, None),
        )
        .await
        .expect("the handshake")
        .unwrap();
        assert_eq!(stored, init_msg);

        let refused: serde_json::Value =
            serde_json::from_str(&stdout_rx.recv().await.unwrap()).unwrap();
        assert_eq!(refused["id"], "server-discover-probe-1");
        assert_eq!(refused["error"]["code"], -32601);
        let pong: serde_json::Value =
            serde_json::from_str(&stdout_rx.recv().await.unwrap()).unwrap();
        assert_eq!(pong["id"], 7);
        assert!(pong["result"].is_object());
        let initialized = stdout_rx.recv().await.unwrap();
        assert!(initialized.contains("capabilities"));
        tokio::time::timeout(within, server)
            .await
            .expect("the server")
            .unwrap();
    }

    #[test]
    fn an_external_bridge_cannot_forward_a_clients_identity_claim() {
        let init = r#"{"jsonrpc":"2.0","method":"initialize","id":0,"params":{"_meta":{"codesenseSessionId":"another-tab","codesenseLaunchToken":"client-claim"}}}"#;
        let out: serde_json::Value = serde_json::from_str(&with_session_meta(init, None)).unwrap();
        assert!(out["params"]["_meta"].get("codesenseSessionId").is_none());
        assert!(out["params"]["_meta"].get("codesenseLaunchToken").is_none());
    }

    #[test]
    fn bridge_identity_is_only_the_launched_session_hint() {
        let init = r#"{"params":{"_meta":{"codesenseSessionId":"other","codesenseLaunchToken":"readable-token","progressToken":7}}}"#;
        let out: serde_json::Value =
            serde_json::from_str(&with_session_meta(init, Some("own-tab"))).unwrap();
        assert_eq!(
            out["params"]["_meta"],
            serde_json::json!({"codesenseSessionId":"own-tab", "progressToken":7})
        );
    }

    #[test]
    fn session_meta_is_added_to_initialize_params() {
        let init = r#"{"jsonrpc":"2.0","method":"initialize","id":0,"params":{"protocolVersion":"2024-11-05"}}"#;
        let out: serde_json::Value =
            serde_json::from_str(&with_session_meta(init, Some("tab-7"))).unwrap();
        assert_eq!(out["params"]["_meta"]["codesenseSessionId"], "tab-7");
        assert_eq!(out["params"]["protocolVersion"], "2024-11-05");
    }

    #[test]
    fn session_meta_keeps_existing_meta_keys() {
        let init = r#"{"jsonrpc":"2.0","method":"initialize","id":0,"params":{"_meta":{"progressToken":1}}}"#;
        let out: serde_json::Value =
            serde_json::from_str(&with_session_meta(init, Some("tab-7"))).unwrap();
        assert_eq!(out["params"]["_meta"]["progressToken"], 1);
        assert_eq!(out["params"]["_meta"]["codesenseSessionId"], "tab-7");
    }

    #[test]
    fn no_session_or_odd_shapes_forward_untouched() {
        let init = r#"{"jsonrpc":"2.0","method":"initialize","id":0,"params":{}}"#;
        assert_eq!(with_session_meta(init, None), init);
        let no_params = r#"{"jsonrpc":"2.0","method":"initialize","id":0}"#;
        assert_eq!(with_session_meta(no_params, Some("tab-7")), no_params);
        let odd_meta = r#"{"method":"initialize","params":{"_meta":3}}"#;
        assert_eq!(with_session_meta(odd_meta, Some("tab-7")), odd_meta);
    }

    #[tokio::test]
    async fn test_do_initial_handshake_forwards_and_stores_the_session() {
        let (left, right) = UnixStream::pair().unwrap();
        let (read_left, write_left) = tokio::io::split(left);
        let (read_right, mut write_right) = tokio::io::split(right);
        let mut writer = write_left;
        let mut reader = BufReader::new(read_left);
        let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(16);
        let (stdout_tx, _stdout_rx) = mpsc::channel::<String>(16);
        stdin_tx
            .send(r#"{"jsonrpc":"2.0","method":"initialize","id":0,"params":{}}"#.to_string())
            .await
            .unwrap();
        stdin_tx
            .send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.to_string())
            .await
            .unwrap();
        let server = tokio::spawn(async move {
            let mut reader = BufReader::new(read_right);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let init: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
            write_right
                .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{}}\n")
                .await
                .unwrap();
            write_right.flush().await.unwrap();
            line.clear();
            reader.read_line(&mut line).await.unwrap();
            init
        });
        let stored = do_initial_handshake(
            &mut writer,
            &mut reader,
            &mut stdin_rx,
            &stdout_tx,
            Some("tab-9"),
        )
        .await
        .unwrap();
        let seen = server.await.unwrap();
        assert_eq!(seen["params"]["_meta"]["codesenseSessionId"], "tab-9");
        let stored: serde_json::Value = serde_json::from_str(&stored).unwrap();
        assert_eq!(stored["params"]["_meta"]["codesenseSessionId"], "tab-9");
    }

    #[tokio::test]
    async fn test_do_initial_handshake_returns_err_on_closed_stdin() {
        let (left, _right) = UnixStream::pair().unwrap();
        let (read_left, write_left) = tokio::io::split(left);
        let mut writer = write_left;
        let mut reader = BufReader::new(read_left);

        let (_stdin_tx, mut stdin_rx) = mpsc::channel::<String>(16);
        let (stdout_tx, _stdout_rx) = mpsc::channel::<String>(16);

        // Drop stdin_tx to simulate closed stdin
        drop(_stdin_tx);

        let result =
            do_initial_handshake(&mut writer, &mut reader, &mut stdin_rx, &stdout_tx, None).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_connect_uds_returns_none_for_nonexistent_socket() {
        let path = PathBuf::from("/tmp/nonexistent-codesense-test-socket.sock");
        let result = connect_uds(&path).await;
        assert!(result.is_none());
    }

    #[test]
    fn test_socket_path_suffix_matches_server() {
        // The bridge and server must use the same path.
        // Both define SOCKET_PATH_SUFFIX = ".codesense/mcp.sock"
        assert_eq!(SOCKET_PATH_SUFFIX, ".codesense/mcp.sock");
    }

    #[tokio::test]
    async fn test_reconnect_uds_connects_to_listener() {
        let dir = std::env::temp_dir().join(format!("bridge-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock_path = dir.join("reconnect-test.sock");

        // Start a UDS listener before reconnect
        let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();

        // Accept one connection in the background
        let accept_handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            stream // return to keep alive
        });

        // reconnect_uds should find the listener on first retry
        let (_stdin_tx, mut stdin_rx) = mpsc::channel::<String>(16);
        let (stdout_tx, _stdout_rx) = mpsc::channel::<String>(16);
        let result = reconnect_uds(
            &sock_path,
            &mut stdin_rx,
            &stdout_tx,
            &mut RECONNECT_FIRST_DELAY.clone(),
        )
        .await;
        assert!(
            result.is_some(),
            "reconnect_uds should connect to active listener"
        );

        drop(result);
        let _ = accept_handle.await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn connect_uds_returns_none_when_no_listener() {
        let path = PathBuf::from("/tmp/nonexistent-reconnect-test.sock");

        // reconnect_uds retries until stdin closes, so test connect_uds directly
        // for the "no listener" case, which reconnect_uds calls internally
        let result = connect_uds(&path).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_connect_uds_succeeds_with_active_listener() {
        let dir = std::env::temp_dir().join(format!("bridge-connect-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock_path = dir.join("connect-test.sock");

        let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();

        let accept_handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            stream
        });

        let result = connect_uds(&sock_path).await;
        assert!(
            result.is_some(),
            "connect_uds should succeed with active listener"
        );

        drop(result);
        let _ = accept_handle.await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Integration test: simulates the full relay pattern
    /// stdin (channel) → UDS write → UDS read → stdout (channel)
    #[tokio::test]
    async fn test_relay_stdin_to_uds_to_stdout() {
        let (left, right) = UnixStream::pair().unwrap();
        let (read_left, mut write_left) = tokio::io::split(left);
        let (read_right, mut write_right) = tokio::io::split(right);
        let mut uds_reader = BufReader::new(read_left);

        let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(16);
        let (stdout_tx, mut stdout_rx) = mpsc::channel::<String>(16);

        // Simulate server echo: read from UDS, write back
        let echo_server = tokio::spawn(async move {
            let mut reader = BufReader::new(read_right);
            let mut line = String::new();
            for _ in 0..3 {
                line.clear();
                if reader.read_line(&mut line).await.unwrap() == 0 {
                    break;
                }
                let response = format!("echo:{}", line.trim());
                write_right.write_all(response.as_bytes()).await.unwrap();
                write_right.write_all(b"\n").await.unwrap();
                write_right.flush().await.unwrap();
            }
        });

        // Send 3 messages through the relay
        for i in 0..3 {
            let msg = format!("msg-{}", i);
            stdin_tx.send(msg.clone()).await.unwrap();

            // Relay stdin→UDS
            let m = stdin_rx.recv().await.unwrap();
            send_line(&mut write_left, &m).await.unwrap();

            // Relay UDS→stdout
            let mut uds_line = String::new();
            uds_reader.read_line(&mut uds_line).await.unwrap();
            let trimmed = uds_line.trim_end_matches('\n').to_string();
            stdout_tx.send(trimmed).await.unwrap();

            // Verify stdout received the echo
            let received = stdout_rx.recv().await.unwrap();
            assert_eq!(received, format!("echo:msg-{}", i));
        }

        drop(write_left);
        let _ = echo_server.await;
    }

    /// Test that multiple messages queue in channel during simulated "disconnect"
    #[tokio::test]
    async fn test_messages_queue_in_channel_during_disconnect() {
        let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(CHANNEL_BUFFER);

        // Queue 5 messages (simulating stdin during reconnect)
        for i in 0..5 {
            stdin_tx.send(format!("queued-{}", i)).await.unwrap();
        }

        // Drain and verify all messages arrived in order
        for i in 0..5 {
            let msg = stdin_rx.recv().await.unwrap();
            assert_eq!(msg, format!("queued-{}", i));
        }
    }

    #[tokio::test]
    async fn test_channel_closes_when_sender_dropped() {
        let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(16);
        stdin_tx.send("last-msg".to_string()).await.unwrap();
        drop(stdin_tx);

        // Should receive the last message
        assert_eq!(stdin_rx.recv().await, Some("last-msg".to_string()));
        // Then None (channel closed)
        assert_eq!(stdin_rx.recv().await, None);
    }

    #[tokio::test]
    async fn test_send_line_error_on_closed_writer() {
        let (left, right) = UnixStream::pair().unwrap();
        let (_, mut write_half) = tokio::io::split(left);
        drop(right); // close the other end

        // First write might succeed (buffered), but flush or subsequent write should fail
        let mut failed = false;
        for _ in 0..10 {
            if send_line(&mut write_half, "test").await.is_err() {
                failed = true;
                break;
            }
        }
        assert!(failed, "send_line should eventually error on closed peer");
    }

    /// With no listener, reconnect_uds keeps trying well past the old 30 s limit,
    /// answers a request sent meanwhile with an error, and stops once stdin closes.
    /// Uses paused time to simulate minutes of retries in milliseconds.
    #[tokio::test(start_paused = true)]
    async fn reconnect_uds_retries_until_stdin_closes_and_answers_requests_meanwhile() {
        let path = PathBuf::from("/tmp/nonexistent-exhaust-test.sock");
        let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(16);
        let (stdout_tx, mut stdout_rx) = mpsc::channel::<String>(16);

        let client = async {
            tokio::time::sleep(Duration::from_secs(120)).await;
            stdin_tx
                .send(r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{}}"#.into())
                .await
                .unwrap();
            stdin_tx
                .send(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{}}"#.into())
                .await
                .unwrap();
            let answer: serde_json::Value =
                serde_json::from_str(&stdout_rx.recv().await.unwrap()).unwrap();
            tokio::time::sleep(Duration::from_secs(120)).await;
            drop(stdin_tx);
            answer
        };
        let mut delay = RECONNECT_FIRST_DELAY;
        let (result, answer) = tokio::join!(
            reconnect_uds(&path, &mut stdin_rx, &stdout_tx, &mut delay),
            client
        );
        assert_eq!(delay, RECONNECT_MAX_DELAY, "the backoff grew to its cap");

        assert!(result.is_none(), "it ends only when stdin closes");
        assert_eq!(answer["id"], 5);
        assert_eq!(answer["error"]["code"], CONNECTION_LOST);
        assert!(!answer["error"]["message"]
            .as_str()
            .unwrap()
            .contains("requestId"));
        assert!(
            stdout_rx.try_recv().is_err(),
            "a notification gets no answer"
        );
    }

    /// Test reconnect_uds succeeds on delayed listener start.
    /// Listener starts after first retry, simulating CodeSense restart.
    #[tokio::test(start_paused = true)]
    async fn test_reconnect_uds_succeeds_after_delay() {
        let dir = std::env::temp_dir().join(format!("bridge-delayed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock_path = dir.join("delayed-reconnect.sock");
        let sock_path_clone = sock_path.clone();

        // Start listener after a short delay (simulates CodeSense restarting)
        let listener_handle = tokio::spawn(async move {
            // Advance past first retry attempt
            tokio::time::sleep(Duration::from_millis(600)).await;
            let listener = tokio::net::UnixListener::bind(&sock_path_clone).unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            stream
        });

        let (_stdin_tx, mut stdin_rx) = mpsc::channel::<String>(16);
        let (stdout_tx, _stdout_rx) = mpsc::channel::<String>(16);
        let result = reconnect_uds(
            &sock_path,
            &mut stdin_rx,
            &stdout_tx,
            &mut RECONNECT_FIRST_DELAY.clone(),
        )
        .await;
        assert!(
            result.is_some(),
            "reconnect_uds should connect after listener starts"
        );

        drop(result);
        let _ = listener_handle.await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A reply bigger than one read, cut inside a multi-byte character, with a
    /// client message arriving between its two halves: the client still gets the
    /// whole reply, once, and the connection is not dropped.
    #[tokio::test]
    async fn a_large_reply_survives_a_client_message_arriving_mid_read() {
        let (left, right) = UnixStream::pair().unwrap();
        let (read_left, mut write_left) = tokio::io::split(left);
        let (read_right, mut write_right) = tokio::io::split(right);
        let mut uds_reader = BufReader::new(read_left);
        let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(16);
        let (stdout_tx, mut stdout_rx) = mpsc::channel::<String>(16);

        let text = format!("{}€{}", "a".repeat(200_000), "b".repeat(200_000));
        let reply = serde_json::json!({"jsonrpc":"2.0","id":1,"result":{"text":text}}).to_string();
        let cut = reply.find('€').unwrap() + 1;
        let (first_sent_tx, first_sent_rx) = tokio::sync::oneshot::channel();
        let wire = reply.clone();
        let server = tokio::spawn(async move {
            write_right
                .write_all(&wire.as_bytes()[..cut])
                .await
                .unwrap();
            write_right.flush().await.unwrap();
            first_sent_tx.send(()).unwrap();
            let mut reader = BufReader::new(read_right);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            assert!(line.contains("notifications/cancelled"), "{line}");
            write_right
                .write_all(&wire.as_bytes()[cut..])
                .await
                .unwrap();
            write_right.write_all(b"\n").await.unwrap();
            write_right.flush().await.unwrap();
            (reader, write_right)
        });

        let client = async {
            first_sent_rx.await.unwrap();
            // Let the relay start reading the first half before the client speaks.
            tokio::time::sleep(Duration::from_millis(50)).await;
            stdin_tx
                .send(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":9}}"#.into())
                .await
                .unwrap();
            let got = stdout_rx.recv().await.unwrap();
            drop(stdin_tx);
            got
        };
        let mut pending = Pending::default();
        let within = Duration::from_secs(10);
        let (dropped, got) = tokio::time::timeout(within, async {
            tokio::join!(
                relay(
                    &mut stdin_rx,
                    &mut write_left,
                    &mut uds_reader,
                    &stdout_tx,
                    &mut pending
                ),
                client
            )
        })
        .await
        .expect("the relay");

        assert_eq!(got, reply, "the client gets the whole reply");
        assert!(dropped.is_none(), "the connection is not treated as lost");
        let _keep_open = server.await.unwrap();
    }

    #[test]
    fn uncertain_retry_advice_only_names_request_id_for_supported_tools_with_a_key() {
        for name in [
            "send_message",
            "reply",
            "signal_done",
            "send_role_message",
            "open_terminal_tab",
            "send_to_session",
        ] {
            for key in [None, Some("mail-1")] {
                let mut pending = Pending::default();
                pending.sent(&serde_json::json!({"jsonrpc":"2.0", "id":1, "method":"tools/call", "params":{"name":name, "arguments":{"requestId":key}}}).to_string());
                let reply: serde_json::Value = serde_json::from_str(&pending.lost()[0]).unwrap();
                let message = reply["error"]["message"].as_str().unwrap();
                assert!(message.contains("may have run"));
                assert_eq!(
                    message.contains("same requestId"),
                    key.is_some()
                        && matches!(
                            name,
                            "send_message" | "reply" | "signal_done" | "send_role_message"
                        ),
                    "{name}, key={key:?}"
                );
            }
        }
    }

    /// A request still waiting when CodeSense drops the connection is answered
    /// with an error, so the client is not left waiting forever.
    #[tokio::test]
    async fn a_request_lost_with_its_connection_is_answered_with_an_error() {
        let (left, right) = UnixStream::pair().unwrap();
        let (read_left, mut write_left) = tokio::io::split(left);
        let mut uds_reader = BufReader::new(read_left);
        let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(16);
        let (stdout_tx, _stdout_rx) = mpsc::channel::<String>(16);

        let server = tokio::spawn(async move {
            let mut reader = BufReader::new(right);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            // CodeSense goes away without answering.
        });
        stdin_tx
            .send(r#"{"jsonrpc":"2.0","id":"call-4","method":"tools/call","params":{}}"#.into())
            .await
            .unwrap();

        let mut pending = Pending::default();
        let dropped = tokio::time::timeout(
            Duration::from_secs(5),
            relay(
                &mut stdin_rx,
                &mut write_left,
                &mut uds_reader,
                &stdout_tx,
                &mut pending,
            ),
        )
        .await
        .expect("the relay");
        server.await.unwrap();

        assert!(dropped.is_some());
        let answers = pending.lost();
        assert_eq!(answers.len(), 1);
        let answer: serde_json::Value = serde_json::from_str(&answers[0]).unwrap();
        assert_eq!(answer["id"], "call-4");
        assert_eq!(answer["error"]["code"], CONNECTION_LOST);
        assert!(pending.lost().is_empty(), "each request is answered once");
    }

    #[test]
    fn pending_forgets_answered_and_cancelled_requests_only() {
        let mut pending = Pending::default();
        pending.sent(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call"}"#);
        pending.sent(r#"{"jsonrpc":"2.0","id":2,"method":"tools/call"}"#);
        pending.sent(r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#);
        pending.sent(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        // A client's answer to a server request is not a request of its own.
        pending.sent(r#"{"jsonrpc":"2.0","id":"s1","result":{}}"#);

        pending.received(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#);
        pending.sent(
            r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2}}"#,
        );
        // A server request with the same id as a waiting request is not its answer.
        pending.received(r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#);

        let ids: Vec<serde_json::Value> = pending
            .lost()
            .iter()
            .map(|a| serde_json::from_str::<serde_json::Value>(a).unwrap()["id"].clone())
            .collect();
        assert_eq!(ids, vec![serde_json::json!(3)]);
    }

    #[test]
    fn a_request_with_non_object_params_is_still_tracked() {
        let mut pending = Pending::default();
        pending.sent(r#"{"jsonrpc":"2.0","id":8,"method":"custom","params":[1,2]}"#);
        pending.sent(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":"odd"}"#);
        assert_eq!(pending.lost().len(), 1);
    }

    #[test]
    fn the_disconnect_log_appends_records_and_moves_a_full_log_aside() {
        let dir = std::env::temp_dir().join(format!("bridge-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = DisconnectLog::new(&dir.join("mcp.sock"), Some("tab-3")).unwrap();
        log.record("bridge_disconnect", "CodeSense closed\nthe connection");
        log.record("bridge_reconnect", "connected again after 2s");

        let path = dir.join("logs").join("mcp-bridge-disconnect.log");
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(
            lines[0].ends_with(
                " session=tab-3 event=bridge_disconnect reason=CodeSense closed the connection"
            ),
            "{}",
            lines[0]
        );
        assert!(lines[1].contains("event=bridge_reconnect"));

        std::fs::write(&path, vec![b'x'; DISCONNECT_LOG_MAX_BYTES as usize]).unwrap();
        log.record("bridge_disconnect", "again");
        assert!(dir
            .join("logs")
            .join("mcp-bridge-disconnect.log.1")
            .exists());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().lines().count(),
            1,
            "a fresh log after the move"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn take_line_waits_for_the_newline() {
        let mut buf = b"{\"id\":1".to_vec();
        assert_eq!(take_line(&mut buf), None);
        assert_eq!(buf, b"{\"id\":1", "a partial line is kept");
        buf.extend_from_slice(b"}\n");
        assert_eq!(take_line(&mut buf).as_deref(), Some("{\"id\":1}"));
        assert!(buf.is_empty());
    }

    // ---- Remote mode against a fake machine broker -------------------------
    // The broker side of `connect_remote`: reacquire (`R`) -> capability fd ->
    // mode (`M`/`S`) -> fd back -> audience challenge echoed -> optional server
    // selector -> the MCP stream itself.

    /// What the fake broker does once the bridge is authenticated.
    type AfterAuth =
        Box<dyn FnOnce(&mut std::io::BufReader<std::os::unix::net::UnixStream>) + Send>;

    /// A one-shot broker on a fresh socket. Returns the socket path and the
    /// broker thread (joined to surface its assertions).
    fn fake_broker(
        name: &str,
        audience: &str,
        expect_mode: u8,
        expect_selector: Option<&str>,
        after_auth: AfterAuth,
    ) -> FakeBroker {
        use codesense_host_proto::broker;
        use std::io::{BufRead, Read, Write};
        let dir =
            std::env::temp_dir().join(format!("bridge-remote-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = TempDir(dir);
        let path = dir.0.join("broker.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let audience = audience.to_owned();
        let expect_selector = expect_selector.map(str::to_owned);
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut mode = [0u8];
            stream.read_exact(&mut mode).unwrap();
            assert_eq!(mode[0], codesense_host_proto::reverse_mcp::MODE_REACQUIRE);
            let (capability, _keep) = std::os::unix::net::UnixStream::pair().unwrap();
            use std::os::fd::AsRawFd;
            broker::send_fd(&stream, capability.as_raw_fd()).unwrap();
            stream.read_exact(&mut mode).unwrap();
            assert_eq!(mode[0], expect_mode);
            let _presented = broker::receive_fd(&stream).unwrap();
            let challenge = format!("{audience} one-use-nonce\n");
            stream.write_all(challenge.as_bytes()).unwrap();
            let mut reader = std::io::BufReader::new(stream);
            let mut echoed = String::new();
            if reader.read_line(&mut echoed).unwrap_or(0) == 0 {
                return; // the bridge refused the challenge and hung up
            }
            assert_eq!(
                echoed, challenge,
                "the bridge echoes the challenge verbatim"
            );
            if let Some(want) = expect_selector {
                let mut selector = String::new();
                reader.read_line(&mut selector).unwrap();
                assert_eq!(selector, format!("{}\n", serde_json::json!(want)));
            }
            after_auth(&mut reader);
        });
        FakeBroker {
            path,
            handle,
            _dir: dir,
        }
    }

    /// Removes a test's temp dir however the test ends (a failed assertion too).
    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct FakeBroker {
        path: PathBuf,
        handle: std::thread::JoinHandle<()>,
        _dir: TempDir,
    }
    impl FakeBroker {
        /// Joins the broker, re-raising its own assertion failure, so a test
        /// checks the bridge's result only after the broker side held.
        fn finish(self) {
            if let Err(panic) = self.handle.join() {
                std::panic::resume_unwind(panic);
            }
        }
    }

    fn write_json(
        reader: &mut std::io::BufReader<std::os::unix::net::UnixStream>,
        value: serde_json::Value,
    ) {
        use std::io::Write;
        let mut line = value.to_string();
        line.push('\n');
        reader.get_mut().write_all(line.as_bytes()).unwrap();
    }

    #[tokio::test]
    async fn connect_remote_refuses_a_broker_for_another_audience() {
        let broker = fake_broker(
            "wrong-audience",
            "someone-else",
            codesense_host_proto::reverse_mcp::MODE_MCP,
            None,
            Box::new(|_| unreachable!("the bridge must not answer a foreign challenge")),
        );
        let refused = connect_remote(
            broker.path.clone(),
            codesense_host_proto::reverse_mcp::default_server(),
        )
        .await;
        broker.finish();
        assert_eq!(refused.unwrap_err(), "broker audience mismatch");
    }

    #[tokio::test]
    async fn connect_remote_selects_a_named_server_after_the_challenge() {
        let broker = fake_broker(
            "named-server",
            codesense_host_proto::reverse_mcp::AUDIENCE,
            codesense_host_proto::reverse_mcp::MODE_MCP_SERVER,
            Some("github"),
            Box::new(|io| write_json(io, serde_json::json!({"hello":"github"}))),
        );
        let stream = connect_remote(broker.path.clone(), "github".into()).await;
        broker.finish();
        let stream = stream.unwrap();
        // The authenticated socket is the server's MCP stream from here on.
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert_eq!(line, "{\"hello\":\"github\"}\n");
    }
}
