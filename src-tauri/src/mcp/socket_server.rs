// SPDX-License-Identifier: MIT
use rmcp::model::{ClientJsonRpcMessage, ClientRequest, ErrorData, ServerJsonRpcMessage};
use rmcp::service::{RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::async_rw::AsyncRwTransport;
use rmcp::transport::Transport;
use rmcp::{RoleServer, ServerHandler, ServiceExt};
use std::future::Future;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::UnixListener;

use super::handler::CodesenseHandler;

/// Returns the socket path: ~/.codesense/mcp.sock
fn socket_path() -> PathBuf {
    crate::commands::paths::codesense_home_dir()
        .expect("could not determine home directory")
        .join("mcp.sock")
}

/// Ensures ~/.codesense/ exists with 0700 permissions.
fn ensure_socket_dir(path: &Path) -> std::io::Result<()> {
    let dir = path.parent().expect("socket path must have parent");
    if !dir.exists() {
        std::fs::create_dir_all(dir)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Detects whether an existing socket file is stale (no process listening)
/// or live (another CodeSense instance).
fn handle_existing_socket(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "Another CodeSense instance is already running (MCP socket is live)",
        )),
        Err(_) => {
            log::info!(target: "mcp", "Removing stale socket file: {:?}", path);
            std::fs::remove_file(path)?;
            Ok(())
        }
    }
}

/// The socket file this process last bound, by device and inode. Another
/// instance on the same home finds the socket live and does not bind, so it must
/// not remove it at exit either.
static BOUND_SOCKET: std::sync::Mutex<Option<(u64, u64)>> = std::sync::Mutex::new(None);

fn file_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(path).ok()?;
    Some((metadata.dev(), metadata.ino()))
}

/// Binds the UDS listener, then sets 0600 permissions on the socket file.
/// Uses post-bind chmod instead of umask manipulation (umask is process-wide
/// and not thread-safe in a multi-threaded Tauri app).
fn bind_listener(path: &Path) -> std::io::Result<UnixListener> {
    let listener = UnixListener::bind(path)?;
    *BOUND_SOCKET.lock().unwrap_or_else(|p| p.into_inner()) = file_identity(path);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(listener)
}

/// How often an instance that found the socket served elsewhere checks again.
const TAKEOVER_POLL: std::time::Duration = std::time::Duration::from_secs(3);

/// How long an instance waits before trying again after the socket failed to bind.
const BIND_RETRY: std::time::Duration = std::time::Duration::from_secs(30);

/// The lock held for the life of the instance serving the socket. The kernel
/// drops it when that process ends, however it ends, which is what lets a
/// waiting instance take over without two of them binding at once.
static SOCKET_LOCK: std::sync::OnceLock<std::fs::File> = std::sync::OnceLock::new();

fn lock_path(socket: &Path) -> PathBuf {
    socket.with_extension("sock.lock")
}

/// Take the socket lock, without waiting. `Ok(false)` while another process holds it.
fn try_lock_socket(socket: &Path) -> std::io::Result<bool> {
    use std::os::fd::AsRawFd;
    if SOCKET_LOCK.get().is_some() {
        return Ok(true);
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path(socket))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = std::io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(libc::EWOULDBLOCK) => Ok(false),
            _ => Err(error),
        };
    }
    let _ = SOCKET_LOCK.set(file);
    Ok(true)
}

/// Bind the socket for this instance, once it holds the lock and no process
/// answers on the socket (an older build serves it without taking the lock).
/// `AddrInUse` while another instance serves it.
fn claim_socket(path: &Path) -> std::io::Result<UnixListener> {
    if !try_lock_socket(path)? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "Another CodeSense instance is serving the MCP socket",
        ));
    }
    handle_existing_socket(path)?;
    bind_listener(path)
}

/// Returns the canonical socket path for cleanup from outside this module.
pub fn mcp_socket_path() -> PathBuf {
    socket_path()
}

/// Whether the socket file at `path` is the one this process bound.
pub fn mcp_socket_bound_here(path: &Path) -> bool {
    let bound = *BOUND_SOCKET.lock().unwrap_or_else(|p| p.into_inner());
    bound.is_some_and(|bound| file_identity(path) == Some(bound))
}

/// Starts the MCP server on a Unix domain socket.
/// Called from Tauri's `setup()` hook.
pub fn start_mcp_server(app_handle: tauri::AppHandle) {
    use tauri::Manager;
    app_handle.manage(std::sync::Arc::new(
        super::local_tools::Connections::default(),
    ));
    if std::env::var("CODESENSE_DISABLE_MCP").is_ok() {
        log::info!(target: "mcp", "MCP server disabled via CODESENSE_DISABLE_MCP");
        return;
    }

    let path = socket_path();

    tauri::async_runtime::spawn(async move {
        if let Err(e) = ensure_socket_dir(&path) {
            log::error!(target: "mcp", "Failed to create socket directory: {}", e);
            return;
        }

        loop {
            let listener = claim_with_retry(&path).await;
            log::info!(target: "mcp", "MCP server listening on {:?}", path);

            // The socket file can be removed under a live listener (an older
            // build removes it at exit whoever bound it). Bridges reconnect by
            // path, so a lost file is bound again.
            let mut check = tokio::time::interval(TAKEOVER_POLL);
            check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    accepted = listener.accept() => match accepted {
                        Ok((stream, _addr)) => {
                            // Who is on the other end, so a tool acting for
                            // "your own session" can check the session it names.
                            let peer = codesense_pty_core::activity::peer::of_socket(&stream);
                            let handler = CodesenseHandler::new(app_handle.clone()).with_local_peer(peer);
                            tokio::spawn(serve_local_stream(handler, stream));
                        }
                        Err(e) => {
                            log::error!(target: "mcp", "MCP accept error: {}", e);
                            // An error such as running out of file descriptors repeats
                            // at once; pause so the loop does not spin.
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        }
                    },
                    _ = check.tick() => {
                        if !mcp_socket_bound_here(&path) {
                            log::warn!(target: "mcp", "MCP socket at {:?} was removed or replaced; binding it again", path);
                            break;
                        }
                    }
                }
            }
        }
    });
}

/// Bind the socket for this instance, waiting while another instance serves it
/// and taking it over once that instance is gone, so agents in this instance's
/// tabs are not left without CodeSense tools for the rest of its life. A failure
/// to bind is retried too: this instance may hold the lock, and giving up would
/// leave every instance without the socket.
async fn claim_with_retry(path: &Path) -> UnixListener {
    let mut last_logged: Option<String> = None;
    loop {
        match claim_socket(path) {
            Ok(listener) => return listener,
            Err(e) => {
                let waiting = e.kind() == std::io::ErrorKind::AddrInUse;
                let message = e.to_string();
                if last_logged.as_deref() != Some(message.as_str()) {
                    if waiting {
                        log::info!(target: "mcp", "{}; serving it once that instance is gone", e);
                    } else {
                        log::error!(target: "mcp", "Failed to bind MCP socket at {:?}: {}; retrying", path, e);
                    }
                    last_logged = Some(message);
                }
                tokio::time::sleep(if waiting { TAKEOVER_POLL } else { BIND_RETRY }).await;
            }
        }
    }
}

async fn serve_local_stream<S>(handler: CodesenseHandler, stream: S)
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let lifetime = handler.local_tools_lifetime();
    serve_with_local_tools(handler, stream, Some(lifetime)).await;
}

/// Same MCP router and framing for local UDS and an authenticated reverse stream.
pub(crate) async fn serve_stream<H, S>(handler: H, stream: S)
where
    H: ServerHandler,
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    serve_with_local_tools(handler, stream, None).await;
}

async fn serve_with_local_tools<H, S>(
    handler: H,
    stream: S,
    local_tools: Option<super::local_tools::Lifetime>,
) where
    H: ServerHandler,
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    log::debug!(target: "mcp", "MCP client connected");
    let (read, write) = tokio::io::split(stream);
    let transport = PastVersionProbes {
        inner: AsyncRwTransport::new_server(read, write),
        probing: true,
        local_tools,
    };
    match handler.serve(transport).await {
        Ok(running) => {
            if let Err(e) = running.waiting().await {
                log::debug!(target: "mcp", "MCP session ended: {}", e);
            }
        }
        // A client gone before `initialize` (an agent ended, a channel torn down).
        Err(e @ rmcp::service::ServerInitializeError::ConnectionClosed(_)) => {
            log::debug!(target: "mcp", "MCP client left before initialize: {}", e)
        }
        Err(e) => log::warn!(target: "mcp", "MCP serve error: {}", e),
    }
    log::debug!(target: "mcp", "MCP client disconnected");
}

/// A client may probe for a newer MCP version (`server/discover`) before it sends
/// `initialize`, and rmcp ends the session on any request before `initialize` but
/// `ping`. A server that speaks only the `initialize` handshake answers the probe with
/// an error, and the client then initializes on the same stream (MCP 2026-07-28,
/// versioning: backward compatibility). Each such request is answered "method not
/// found" here, and a notification is dropped; rmcp sees the stream from the first
/// other message on.
struct PastVersionProbes<T> {
    inner: T,
    probing: bool,
    // Clear readiness at transport EOF/error, before rmcp drains in-flight responses.
    local_tools: Option<super::local_tools::Lifetime>,
}

impl<T: Transport<RoleServer>> Transport<RoleServer> for PastVersionProbes<T> {
    type Error = T::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let slot = self.local_tools.as_ref().map(|lifetime| lifetime.0.clone());
        let send = self.inner.send(item);
        async move {
            let result = send.await;
            if result.is_err() {
                if let Some(slot) = slot {
                    super::local_tools::Lifetime::close(&slot);
                }
            }
            result
        }
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleServer>> {
        loop {
            let Some(message) = self.inner.receive().await else {
                self.local_tools.take();
                return None;
            };
            if !self.probing {
                return Some(message);
            }
            if let ClientJsonRpcMessage::Notification(_) = &message {
                // Before `initialize` a notification (a probe's cancellation) has no
                // effect, and rmcp would end the session on it.
                continue;
            }
            if let ClientJsonRpcMessage::Request(request) = &message {
                if !matches!(
                    request.request,
                    ClientRequest::InitializeRequest(_) | ClientRequest::PingRequest(_)
                ) {
                    let refusal = ErrorData::new(
                        rmcp::model::ErrorCode::METHOD_NOT_FOUND,
                        "Method not found",
                        None,
                    );
                    let answer = ServerJsonRpcMessage::error(refusal, Some(request.id.clone()));
                    self.inner.send(answer).await.ok()?;
                    continue;
                }
                self.probing = matches!(request.request, ClientRequest::PingRequest(_));
            } else {
                self.probing = false;
            }
            return Some(message);
        }
    }

    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        self.local_tools.take();
        self.inner.close()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{
        duplex, AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf,
    };

    #[tokio::test]
    async fn local_mcp_readiness_ends_at_eof_before_the_service_drains() {
        use super::super::local_tools::{ConnectionState, Connections, Lifetime};
        use std::sync::{Arc, Mutex};
        let snapshots = Arc::new(Mutex::new(Vec::new()));
        let recorded = snapshots.clone();
        let connections = Arc::new(Connections::default());
        let slot = Arc::new(Mutex::new(ConnectionState {
            connection: Some(connections.connect(
                "s".into(),
                Arc::new(move |sessions| {
                    recorded.lock().unwrap().push(sessions);
                }),
            )),
            closed: false,
        }));
        let (client, server) = duplex(64);
        let (read, write) = tokio::io::split(server);
        let mut transport = PastVersionProbes {
            inner: AsyncRwTransport::new_server(read, write),
            probing: true,
            local_tools: Some(Lifetime(slot.clone())),
        };
        drop(client);
        assert!(transport.receive().await.is_none());
        assert!(slot.lock().unwrap().connection.is_none());
        assert!(slot.lock().unwrap().closed);
        assert!(snapshots.lock().unwrap().last().unwrap().is_empty());
        // The transport remains alive here; readiness has already been cleared.
    }

    struct Server;
    impl ServerHandler for Server {}

    const PROBE: &str = r#"{"jsonrpc":"2.0","id":"server-discover-probe-1","method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#;
    pub(super) const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"claude-code","version":"2.1.300"}}}"#;
    const PING: &str = r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#;

    pub(super) struct Client {
        read: BufReader<ReadHalf<DuplexStream>>,
        write: WriteHalf<DuplexStream>,
    }

    impl Client {
        /// A client of `serve_stream` over a pipe.
        fn connect() -> Self {
            let (client, server) = duplex(64 * 1024);
            tokio::spawn(serve_stream(Server, server));
            Self::over(client)
        }

        /// The client end of a pipe whose other end is served by the caller.
        pub(super) fn over(client: DuplexStream) -> Self {
            let (read, write) = tokio::io::split(client);
            Self {
                read: BufReader::new(read),
                write,
            }
        }

        pub(super) async fn send(&mut self, line: &str) {
            self.write
                .write_all(format!("{line}\n").as_bytes())
                .await
                .unwrap();
        }

        pub(super) async fn answer(&mut self) -> serde_json::Value {
            let mut line = String::new();
            let read = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                self.read.read_line(&mut line),
            )
            .await
            .expect("an answer")
            .unwrap();
            assert!(read > 0, "the stream closed");
            serde_json::from_str(&line).unwrap()
        }

        async fn initializes(&mut self) {
            self.send(INITIALIZE).await;
            let initialized = self.answer().await;
            assert_eq!(initialized["id"], 0);
            assert!(
                initialized["result"]["protocolVersion"].is_string(),
                "{initialized}"
            );
        }
    }

    #[tokio::test]
    async fn a_version_probe_is_refused_and_initialize_follows_on_the_same_stream() {
        let mut client = Client::connect();
        client.send(PROBE).await;
        let refused = client.answer().await;
        assert_eq!(refused["id"], "server-discover-probe-1");
        assert_eq!(refused["error"]["code"], -32601);
        client.initializes().await;
    }

    #[tokio::test]
    async fn each_probe_is_refused_until_initialize() {
        let mut client = Client::connect();
        for id in ["a", "b"] {
            client
                .send(&PROBE.replace("server-discover-probe-1", id))
                .await;
            assert_eq!(client.answer().await["id"], id);
        }
        client.initializes().await;
    }

    #[tokio::test]
    async fn a_ping_before_initialize_is_answered_by_the_server() {
        let mut client = Client::connect();
        client.send(PING).await;
        let pong = client.answer().await;
        assert_eq!(pong["id"], 7);
        assert!(pong["result"].is_object(), "{pong}");
        client.initializes().await;
    }

    #[tokio::test]
    async fn a_notification_before_initialize_is_dropped() {
        let mut client = Client::connect();
        client.send(PROBE).await;
        client.answer().await;
        let cancelled = r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"server-discover-probe-1"}}"#;
        client.send(cancelled).await;
        client.initializes().await;
    }

    #[tokio::test]
    async fn an_initialize_with_no_probe_is_served_as_before() {
        Client::connect().initializes().await;
    }

    /// While another process holds the socket lock this instance does not bind;
    /// once that lock is released (the other instance quit or died) it takes the
    /// socket over, replacing the stale file left behind.
    #[tokio::test]
    async fn the_socket_is_taken_over_once_its_owner_is_gone() {
        use std::os::fd::AsRawFd;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("mcp.sock");
        std::fs::write(&path, b"").unwrap(); // the owner's socket, stale once it is gone

        let owner = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(lock_path(&path))
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(owner.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        let refused = claim_socket(&path).unwrap_err();
        assert_eq!(refused.kind(), std::io::ErrorKind::AddrInUse);
        assert!(path.exists(), "the owner's socket is left alone");

        drop(owner);
        let _listener = claim_socket(&path).unwrap();
        std::os::unix::net::UnixStream::connect(&path).expect("this instance now serves it");
        // Only the socket this process bound reads as its own (one test, since the
        // binding is recorded once per process).
        assert!(mcp_socket_bound_here(&path));

        // Another instance's socket at the same path after ours was replaced.
        std::fs::remove_file(&path).unwrap();
        let _theirs = std::os::unix::net::UnixListener::bind(&path).unwrap();
        assert!(!mcp_socket_bound_here(&path));
        assert!(!mcp_socket_bound_here(&dir.path().join("absent.sock")));
        assert_eq!(
            claim_socket(&path).unwrap_err().kind(),
            std::io::ErrorKind::AddrInUse,
            "a live socket another process bound is left to it"
        );

        // Once that one is gone, the instance holding the lock binds the path again.
        drop(_theirs);
        let _again = claim_socket(&path).unwrap();
        assert!(mcp_socket_bound_here(&path));
    }
}

#[cfg(test)]
#[path = "socket_server/hermes_readiness_tests.rs"]
mod hermes_readiness_tests;
