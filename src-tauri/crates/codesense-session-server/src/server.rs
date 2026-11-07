// SPDX-License-Identifier: MIT
//! The Unix-domain-socket server for the session-server sidecar. Bind,
//! permissions, and stale-socket handling mirror the app's
//! `mcp/socket_server.rs`; every message is a `codesense-host-proto` frame.
//!
//! The socket and its singleton lock live under the instance's app-data root,
//! never at a fixed path, so an instance with its own root (a sandbox or a test
//! run) gets its own sidecar. Every connection first receives the sidecar's
//! hello and census; one whose own hello is not of this instance at this
//! protocol major is then closed, having learnt only those. Each
//! connection runs on its own thread with its own outbound frame channel and
//! writer thread; replies go to the connection that asked, events to every
//! connection that asked for them. Control calls and raw input frames are
//! taken in the order they arrive.

use std::collections::{HashSet, VecDeque};
use std::fs::File;
use std::io::BufReader;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use codesense_host_proto::{read_frame, read_hello, write_frame, Envelope, Kind};
use codesense_pty_core::protocol::{instance_id, lock_path, socket_path, Message};

use crate::disconnect_log::{self, Event};
use crate::host::{Conn, Host, CONNECTION_BACKLOG};

/// Pause after a failed `accept` so a persistent error cannot spin the CPU.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(50);

/// The `rid` of a reply to a frame that could not be taken as a call.
pub(crate) const UNPARSEABLE_RID: u64 = 0;

/// The path named by `<flag> <path>` in `args`: the instance root
/// (`--root`, which serving always takes; only `attach` defaults it) or the
/// directory of the resources the app ships (`--resources`).
pub fn path_arg(mut args: impl Iterator<Item = String>, flag: &str) -> Option<PathBuf> {
    while let Some(arg) = args.next() {
        if arg == flag {
            return args.next().map(PathBuf::from);
        }
    }
    None
}

/// Ensure the instance root exists with 0700 permissions.
fn ensure_root_dir(dir: &Path) -> std::io::Result<()> {
    if !dir.exists() {
        std::fs::create_dir_all(dir)?;
    }
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// Hold an exclusive `flock` on the instance's lock file for the life of the
/// returned handle. Only one sidecar per instance can hold it, which makes the
/// stale-socket check below race-free. `AddrInUse` if another sidecar holds it.
fn acquire_singleton_lock(root: &Path) -> std::io::Result<File> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(lock_path(root))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let err = std::io::Error::last_os_error();
        return Err(if err.kind() == std::io::ErrorKind::WouldBlock {
            std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                "another codesense-session-server holds the lock",
            )
        } else {
            err
        });
    }
    // Held now: forget the pid a previous holder recorded, so the file never
    // names a process that no longer serves the instance.
    file.set_len(0)?;
    Ok(file)
}

/// With the singleton lock held, a socket file left behind is stale unless
/// something still answers on it. Only a refused connection proves that no
/// one is listening; any other error is left alone.
fn handle_existing_socket(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    match UnixStream::connect(path) {
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "another codesense-session-server is already listening",
        )),
        Err(err) if err.kind() == std::io::ErrorKind::ConnectionRefused => {
            match std::fs::remove_file(path) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
                _ => Ok(()),
            }
        }
        // Gone between the existence check and the probe: nothing to clear.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// A bound sidecar socket, the instance identity it serves, and the singleton
/// lock that must outlive it.
pub struct Bound {
    pub listener: UnixListener,
    pub instance_id: String,
    lock: File,
}

impl Bound {
    /// Record this process as the one serving the instance, in its lock
    /// file, so a client can end a server that no longer answers. Call it in
    /// the process that goes on to serve (after the detach).
    pub fn record_pid(&self) -> std::io::Result<()> {
        use std::os::unix::fs::FileExt;
        self.lock.set_len(0)?;
        self.lock
            .write_all_at(std::process::id().to_string().as_bytes(), 0)
    }
}

/// Take the instance's singleton lock, clear a stale socket, bind, and chmod
/// the socket to 0600 (post-bind; umask is process-wide).
pub fn bind(root: &Path) -> std::io::Result<Bound> {
    ensure_root_dir(root)?;
    let lock = acquire_singleton_lock(root)?;
    let path = socket_path(root);
    handle_existing_socket(&path)?;
    let listener = UnixListener::bind(&path)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(Bound {
        listener,
        instance_id: instance_id(root),
        lock,
    })
}

/// How many viewer ids are remembered for telling a returning viewer from a new
/// one. A viewer that has been away longer than this many other viewers reads
/// as new on its next connect.
const SEEN_VIEWERS_CAP: usize = 1024;

/// The most recent viewer ids this process has seen, oldest evicted first.
#[derive(Default)]
struct SeenViewers {
    order: VecDeque<String>,
    ids: HashSet<String>,
}

impl SeenViewers {
    /// Remember `viewer`. Returns `false` if it was already remembered.
    fn insert(&mut self, viewer: &str) -> bool {
        if !self.ids.insert(viewer.to_string()) {
            return false;
        }
        self.order.push_back(viewer.to_string());
        if self.order.len() > SEEN_VIEWERS_CAP {
            if let Some(oldest) = self.order.pop_front() {
                self.ids.remove(&oldest);
            }
        }
        true
    }
}

/// Serve forever on `listener`. `root` is the instance root whose `logs`
/// directory receives the disconnect log.
pub fn serve(listener: &UnixListener, host: Arc<Host>, root: &Path) {
    static NEXT_CONNECTION: AtomicU64 = AtomicU64::new(1);
    let seen_viewers = Arc::new(Mutex::new(SeenViewers::default()));
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let host = host.clone();
                let root = root.to_path_buf();
                let seen_viewers = seen_viewers.clone();
                let id = NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed);
                let spawned = thread::Builder::new()
                    .name("pty-server-conn".into())
                    .spawn(move || handle_connection(stream, &host, id, &root, &seen_viewers));
                if let Err(err) = spawned {
                    log::error!("connection thread failed: {err}");
                }
            }
            Err(err) => {
                log::warn!("accept error: {err}");
                thread::sleep(ACCEPT_BACKOFF);
            }
        }
    }
}

/// How long a new connection may take to send its handshake.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// Handle one client: a writer thread drains this connection's frames to the
/// socket while this thread greets it, reads its hello, then takes calls until
/// EOF. On disconnect the connection stops receiving events; its sessions keep
/// running, buffer-only.
fn handle_connection(
    stream: UnixStream,
    host: &Arc<Host>,
    id: u64,
    root: &Path,
    seen_viewers: &Mutex<SeenViewers>,
) {
    let session = disconnect_log::session_label(None, id);
    // Set once the hello names the viewer, so the writer can label its records.
    let viewer_slot = Arc::new(OnceLock::new());
    let Some(conn) = start_writer(&stream, id, root, viewer_slot.clone()) else {
        return;
    };
    match host.greeting() {
        Ok(greeting) => {
            for frame in greeting {
                conn.send(frame);
            }
        }
        Err(err) => {
            log::warn!("connection {id}: no greeting: {err}");
            disconnect_log::record(
                root,
                &session,
                Event::AcceptRefused,
                Some(err.to_string().as_str()),
            );
            return;
        }
    }
    let _ = stream.set_read_timeout(Some(HELLO_TIMEOUT));
    let mut reader = BufReader::new(stream);
    let (admitted, viewer, client) = match read_hello(&mut reader) {
        Ok(peer) => {
            let (viewer, client) = match &peer {
                codesense_host_proto::PeerHello::Compatible(hello) => {
                    (hello.viewer_id.clone(), hello.client_id.clone())
                }
                _ => (None, None),
            };
            (host.admits(&peer), viewer, client)
        }
        Err(err) => {
            log::warn!("connection {id}: bad handshake: {err}");
            disconnect_log::record(
                root,
                &session,
                Event::AcceptRefused,
                Some(err.to_string().as_str()),
            );
            return;
        }
    };
    if let Some(viewer) = &viewer {
        let _ = viewer_slot.set(viewer.clone());
    }
    let session = disconnect_log::session_label(viewer.as_deref(), id);
    if !admitted {
        disconnect_log::record(root, &session, Event::AcceptRefused, Some("peer not admitted"));
        return;
    }
    // A viewer seen before in this process is back on a new link.
    let returning = viewer.as_ref().is_some_and(|viewer| {
        !seen_viewers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(viewer)
    });
    host.connect_as(&conn, viewer);
    host.identify_client(conn.id, client);
    if returning {
        disconnect_log::record(root, &session, Event::Reconnect, Some("viewer connected again"));
    }
    // Why the link ended: `None` for a clean close, the read error otherwise.
    let mut lost: Option<String> = None;
    if reader.get_ref().set_read_timeout(None).is_ok() {
        loop {
            match read_frame(&mut reader) {
                Ok(Some(frame)) if frame.kind == Kind::CONTROL => {
                    match frame.decode_json::<Message>() {
                        Ok(Message::Call { rid, request }) => host.handle(&conn, rid, request),
                        Ok(_) => conn.reply_unparseable("bad request: not a call".to_string()),
                        // A newer minor's request is answered on its own rid.
                        Err(err) => match Envelope::of(&frame) {
                            Some(Envelope::Call { rid }) => {
                                conn.reply(rid, Err(format!("unsupported request: {err}")))
                            }
                            _ => conn.reply_unparseable(format!("bad request: {err}")),
                        },
                    }
                }
                Ok(Some(frame)) if frame.kind == Kind::INPUT => host.input(&conn, &frame),
                Ok(Some(frame)) if matches!(frame.kind, Kind::MCP_DATA | Kind::MCP_CLOSE) => {
                    host.reverse_frame(&conn, frame)
                }
                Ok(Some(frame))
                    if matches!(frame.kind, Kind::EXECUTOR_DATA | Kind::EXECUTOR_CLOSE) =>
                {
                    host.executor_frame(&conn, frame)
                }
                // A kind this build does not take.
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(err) => {
                    conn.reply_unparseable(err.clone());
                    lost = Some(err);
                    break;
                }
            }
        }
    }
    host.disconnect(&conn);
    disconnect_log::record(root, &session, Event::Disconnect, lost.as_deref());
}

/// Spawn the thread that drains this connection's frames onto the socket. It
/// exits once every sender is gone or a write fails, and a failed write is
/// recorded in the disconnect log.
fn start_writer(
    stream: &UnixStream,
    id: u64,
    root: &Path,
    viewer: Arc<OnceLock<String>>,
) -> Option<Conn> {
    let mut socket = stream.try_clone().ok()?;
    let control = stream.try_clone().ok()?;
    let (out, frames) = mpsc::sync_channel::<crate::host::QueuedFrame>(CONNECTION_BACKLOG);
    let root = root.to_path_buf();
    thread::Builder::new()
        .name("pty-server-writer".into())
        .spawn(move || {
            while let Ok(frame) = frames.recv() {
                if let Err(err) = write_frame(&mut socket, &frame) {
                    let session =
                        disconnect_log::session_label(viewer.get().map(String::as_str), id);
                    disconnect_log::record(
                        &root,
                        &session,
                        Event::WriteFailed,
                        Some(err.to_string().as_str()),
                    );
                    break;
                }
            }
        })
        .ok()?;
    Some(Conn::new(id, out, control))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seen_viewers_forgets_the_oldest_past_its_cap() {
        let mut seen = SeenViewers::default();
        for n in 0..SEEN_VIEWERS_CAP {
            assert!(seen.insert(&format!("v{n}")));
        }
        assert!(!seen.insert("v0"), "a viewer inside the cap is returning");
        assert!(seen.insert("fresh"));
        assert!(seen.insert("v0"), "the oldest was evicted, so it reads as new");
        assert_eq!(seen.ids.len(), SEEN_VIEWERS_CAP);
    }
}
