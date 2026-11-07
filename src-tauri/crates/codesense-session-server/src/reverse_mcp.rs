// SPDX-License-Identifier: MIT
//! Private daemon ingress for session-authenticated MCP. The SSH relay owns no
//! state here. A request is never replayed: on link loss the machine answers
//! the agent's stream itself (`offline_mcp`) and hands it back to a
//! returning desktop.
use crate::host::{lock, Conn, Host};
use codesense_host_proto::agent_cli::NativeLaunch;
use codesense_host_proto::{broker, reverse_mcp as wire, Frame, Kind};
use codesense_pty_core::{domain::PtySessionId, runner::PtySpec};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering},
    mpsc, Arc, Mutex, Weak,
};
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(5);
/// How long a returning desktop has to answer a stream's replayed
/// `initialize` before the machine keeps answering it.
const REATTACH_DEADLINE: Duration = Duration::from_secs(10);
/// How long the outbox delivery waits for each answer.
const FLUSH_DEADLINE: Duration = Duration::from_secs(15);
/// The most lines an agent sends while its stream is handed back.
const MAX_HELD: usize = 64;
const MAX_PENDING: usize = 64;
/// `InboxWaitV1`: how long a desktop relays an agent's CodeSense stream before the agent
/// counts as having its tools although it has not listed them again since (it is not
/// listing them now).
const TOOLS_SETTLE: Duration = Duration::from_secs(5);
/// `InboxWaitV1`: how long a desktop relays it before the agent counts as having its
/// tools even while a listing of them is still unanswered.
const TOOLS_CAP: Duration = Duration::from_secs(30);
/// What a pending `tools/list` is recorded as (MCP tool names hold no `/`).
const LISTING: &str = "tools/list";
const MODE_SUPERVISOR: u8 = 5;

fn remove_configuration(
    config: &Path,
    settings: Option<&crate::native_launch::OwnedSettings>,
) -> std::io::Result<()> {
    if let Some(settings) = settings {
        let _ = settings.remove_if_unchanged(config);
    }
    std::fs::remove_file(config)
}

struct Session {
    created: Instant,
    conn: Conn,
    nonce: String,
    server_ids: Vec<String>,
    launch: NativeLaunch,
    agent_startup: Option<wire::AgentStartup>,
    owned_settings: Option<crate::native_launch::OwnedSettings>,
    remote_entries: Vec<wire::RemoteStdioEntry>,
    resolved: HashMap<String, crate::owned_supervisors::ResolvedServer>,
    grant_live: bool,
    /// The desktop (its viewer) that relays this session, or did:
    /// when it returns, it gets the session's CodeSense MCP back.
    desktop: Option<String>,
    /// The connection a returning desktop relays this session's
    /// CodeSense MCP on, and the binding nonce its routine secret keys.
    relay: Option<(Conn, String)>,
    config: PathBuf,
    pid: Option<(u32, u64)>,
    // Retain the SAME endpoint sent to children; the opposite endpoint differs.
    capability: UnixStream,
    _peer: UnixStream,
}
/// One MCP stream of an agent's bridge.
struct Stream {
    session: String,
    socket: UnixStream,
    out: mpsc::SyncSender<Out>,
    link: Arc<Link>,
}

impl Stream {
    /// An agent's own CodeSense stream of `session`, once initialized: only those hold
    /// an agent's tool list and are handed back.
    fn is_agent_of(&self, session: &str) -> bool {
        self.is_codesense_of(session) && lock(&self.link.init).is_some()
    }

    /// A CodeSense stream of `session`'s, other than the outbox delivery's.
    fn is_codesense_of(&self, session: &str) -> bool {
        self.link.session == session
            && !self.link.flusher
            && self.link.server_id == wire::CODESENSE_SERVER
    }
}

/// What the stream's writer sends the agent.
enum Out {
    /// Bytes the desktop sent on `channel` (whole lines reach the agent).
    Desktop { channel: u32, bytes: Vec<u8> },
    /// A line the machine answers.
    Line(String),
    /// The desktop closed `channel` of a relayed server other than
    /// CodeSense: what came before is written, then the stream closes.
    End { channel: u32 },
}

/// Where a stream's lines go.
enum Route {
    /// Relayed by a desktop, on `channel` of its connection.
    Desktop {
        conn: Conn,
        channel: u32,
        /// When this relay began.
        since: Instant,
        /// The desktop answered a `tools/list` of the agent's during this relay (the
        /// agent lists after `initialize` and after being told its list changed).
        listed: bool,
    },
    /// Being handed back to a returning desktop on `channel`: the
    /// agent's lines wait in `held` until it answers the replayed
    /// `initialize`.
    Reattaching {
        conn: Conn,
        channel: u32,
        held: Vec<String>,
    },
    /// Answered by the machine (`offline_mcp`).
    Local,
}

impl Route {
    /// Relayed by a desktop from now on; the agent lists its tools again (read only for
    /// an agent's CodeSense stream).
    fn relayed(conn: Conn, channel: u32) -> Self {
        Route::Desktop {
            conn,
            channel,
            since: Instant::now(),
            listed: false,
        }
    }

    fn desktop(&self) -> Option<(&Conn, u32)> {
        match self {
            Route::Desktop { conn, channel, .. } | Route::Reattaching { conn, channel, .. } => {
                Some((conn, *channel))
            }
            Route::Local => None,
        }
    }
}

/// A stream's state, shared by its reader, its writer and the host.
struct Link {
    session: String,
    server_id: String,
    /// Taken before `pending` when both are held.
    route: Mutex<Route>,
    /// The agent's requests sent to the desktop and not answered yet (taken after
    /// `route` when both are held):
    /// `(id key, what)`.
    pending: Mutex<Vec<(String, String)>>,
    /// The agent's `initialize`, replayed to a returning desktop.
    init: Mutex<Option<String>>,
    /// The outbox delivery's own stream, not an agent's.
    flusher: bool,
}

impl Link {
    /// Whether the agent has the desktop's CodeSense tools (an inbox notice waits for
    /// them): the desktop relays its stream, and the agent listed them since, or the
    /// desktop has relayed it a while with no listing unanswered.
    fn has_tools(&self) -> bool {
        let route = lock(&self.route);
        let Route::Desktop { since, listed, .. } = &*route else {
            return false;
        };
        // An unanswered `initialize` holds them back like an unanswered listing.
        let listing = || {
            lock(&self.pending)
                .iter()
                .any(|(_, what)| what == LISTING || what == "initialize")
        };
        *listed || since.elapsed() >= if listing() { TOOLS_CAP } else { TOOLS_SETTLE }
    }
}

/// The owner of the bindings this host makes for the agents it starts on its
/// own (machine-armed routines): no desktop relays them, so their CodeSense
/// MCP is always answered on the machine.
pub(crate) const HOST_OWNER: u64 = u64::MAX;

/// What one agent stream queues for its writer: room for a whole line of the
/// longest legal size (a full CodeSense tools/list is hundreds of KiB) plus a
/// few more chunks. The desktop sends a line's chunks back to back with no
/// flow control, so a smaller queue turns a legal burst into an overrun.
const STREAM_QUEUE_CHUNKS: usize = wire::LINE_BYTES / wire::CHUNK_BYTES + wire::QUEUE_CHUNKS;

pub(crate) struct ReverseMcp {
    host: Weak<Host>,
    path: PathBuf,
    sessions: Mutex<HashMap<String, Session>>,
    streams: Mutex<HashMap<u64, Stream>>,
    next_channel: AtomicU32,
    next_stream: AtomicU64,
    pending: AtomicUsize,
    /// Each session's calls queued while no desktop relays it,
    /// kept on disk past the session (and a restart) until delivered.
    outbox: Mutex<HashMap<String, crate::offline_mcp::Outbox>>,
    outbox_store: crate::offline_mcp::OutboxStore,
    /// The relay a returning desktop gave the outbox of a session that has
    /// ended (no binding to hold it).
    orphan_relay: Mutex<HashMap<String, (Conn, String)>>,
    /// The connections whose desktop named itself (a current desktop: it
    /// runs each outbox id once, and its refusal is final).
    named: Mutex<std::collections::HashSet<u64>>,
    /// When each session's hand-back was last restarted by its agent.
    restarted: Mutex<HashMap<String, Instant>>,
    /// Outbox ids: unique to this host lifetime, then in sequence.
    outbox_boot: String,
    outbox_seq: AtomicU64,
    /// The sessions whose outbox and streams are being handed to
    /// a desktop (`true`: run again when done), and how many run at once.
    syncing: Mutex<HashMap<String, bool>>,
    sync_running: AtomicUsize,
    /// How long a returning desktop has to answer a replayed `initialize`,
    /// and each outbox delivery (tests shorten them).
    reattach_deadline: Mutex<Duration>,
    flush_deadline: Mutex<Duration>,
    /// The first retry's wait; each next one doubles, to [`SYNC_BACKOFF_CAP`].
    sync_backoff: Mutex<Duration>,
    me: Weak<Self>,
}

/// How many sessions are handed back to desktops at once.
const MAX_SYNCING: usize = 8;
/// How many times a hand-back is retried before waiting for the
/// next bind.
const SYNC_ATTEMPTS: u32 = 8;
const SYNC_BACKOFF_CAP: Duration = Duration::from_secs(30);
/// After the attempts above, the hand-back is tried again this
/// much later, and an agent's own call restarts it at most this often.
const SYNC_RETRY_LATER: Duration = Duration::from_secs(180);
const SYNC_RESTART_EVERY: Duration = Duration::from_secs(30);

impl ReverseMcp {
    pub(crate) fn start(host: &Arc<Host>, root: &Path) -> Result<Arc<Self>, String> {
        let path = root.join("mcp.sock");
        let bridge = std::env::current_exe()
            .map_err(|e| e.to_string())?
            .with_file_name(crate::install::BRIDGE);
        let config = root.join("mcp-config.json");
        let bridge = bridge.to_str().ok_or("invalid bridge path")?;
        let socket = path.to_str().ok_or("invalid socket path")?;
        // Root is the verified private daemon directory. Replace only our file;
        // refuse symlinks rather than following an agent-supplied destination.
        if std::fs::symlink_metadata(&config).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err("unsafe bridge configuration path".into());
        }
        use std::os::unix::fs::OpenOptionsExt;
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&config)
            .map_err(|e| e.to_string())?;
        wire::write_bridge_configuration(&mut output, bridge, socket)?;

        // The terminal daemon singleton has already been acquired by the caller.
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| e.to_string())?;
        }
        let listener = UnixListener::bind(&path).map_err(|e| e.to_string())?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let outbox_store = crate::offline_mcp::OutboxStore::at(Some(root.join("outbox")));
        let now = crate::offline_mcp::now_ms();
        let outbox = outbox_store
            .load()
            .into_iter()
            .filter_map(|mut outbox| {
                // Saved by an earlier run: its session did not outlive it (a
                // replacement drains every session first).
                outbox.end(now);
                if outbox.expired(now) {
                    let lost = outbox.drain();
                    outbox_store.record_lost(
                        &outbox.session,
                        &lost,
                        "its desktop did not come back within 30 days",
                        outbox.desktop.as_deref(),
                    );
                    outbox_store.keep(&outbox);
                    return None;
                }
                Some((outbox.session.clone(), outbox))
            })
            .collect();
        let outbox_boot = random_token()?[..16].to_string();
        let this = Arc::new_cyclic(|me| Self {
            host: Arc::downgrade(host),
            path,
            sessions: Mutex::default(),
            streams: Mutex::default(),
            next_channel: AtomicU32::new(1),
            next_stream: AtomicU64::new(1),
            pending: AtomicUsize::new(0),
            outbox: Mutex::new(outbox),
            outbox_store,
            orphan_relay: Mutex::default(),
            named: Mutex::default(),
            restarted: Mutex::default(),
            outbox_boot,
            outbox_seq: AtomicU64::new(1),
            syncing: Mutex::default(),
            sync_running: AtomicUsize::new(0),
            reattach_deadline: Mutex::new(REATTACH_DEADLINE),
            flush_deadline: Mutex::new(FLUSH_DEADLINE),
            sync_backoff: Mutex::new(Duration::from_millis(500)),
            me: me.clone(),
        });
        let weak = Arc::downgrade(&this);
        std::thread::spawn(move || loop {
            let Some(this) = weak.upgrade() else { break };
            match listener.accept() {
                Ok((socket, _)) => {
                    if this.pending.fetch_add(1, Ordering::AcqRel) >= MAX_PENDING {
                        this.pending.fetch_sub(1, Ordering::AcqRel);
                        continue;
                    }
                    std::thread::spawn(move || {
                        let _ = this.accept(socket);
                        this.pending.fetch_sub(1, Ordering::AcqRel);
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    drop(this);
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        });
        Ok(this)
    }

    #[cfg(test)]
    pub(crate) fn provision(
        &self,
        conn: &Conn,
        session: &PtySessionId,
        nonce: String,
        server_ids: Vec<String>,
        remote_entries: Vec<wire::RemoteStdioEntry>,
        launch: NativeLaunch,
    ) -> Result<(), String> {
        self.provision_agent(
            conn,
            session,
            nonce,
            server_ids,
            remote_entries,
            launch,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn provision_agent(
        &self,
        conn: &Conn,
        session: &PtySessionId,
        nonce: String,
        server_ids: Vec<String>,
        remote_entries: Vec<wire::RemoteStdioEntry>,
        launch: NativeLaunch,
        agent_startup: Option<wire::AgentStartup>,
    ) -> Result<(), String> {
        if let Some(agent) = &agent_startup {
            codesense_pty_core::agent_cli::validate_startup(agent, launch)?;
        }
        if nonce.len() != 64 || !nonce.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("invalid reverse binding nonce".into());
        }
        let server_ids = wire::selected_servers(server_ids)?;
        wire::validate_remote_entries(&server_ids, &remote_entries)?;
        if !remote_entries.is_empty() && launch.is_shell() {
            return Err("remote stdio MCP requires a native client launch".into());
        }
        let mut sessions = lock(&self.sessions);
        sessions.retain(|id, session| {
            let retain =
                session.pid.is_some() || session.created.elapsed() < Duration::from_secs(30);
            if !retain {
                let _ = remove_configuration(&session.config, session.owned_settings.as_ref());
                if let Some(host) = self.host.upgrade() {
                    host.owned_supervisors.begin_drain(id);
                    host.owned_supervisors.forget_settled(id, &session.nonce);
                }
            }
            retain
        });
        if let Some(existing) = sessions.get(session.as_str()) {
            return if existing.conn.id == conn.id
                && existing.nonce == nonce
                && existing.server_ids == server_ids
                && existing.launch == launch
                && existing.agent_startup == agent_startup
                && existing.remote_entries == remote_entries
                && existing.grant_live
            {
                Ok(())
            } else {
                Err("reverse session already provisioned".into())
            };
        }
        if sessions.len() >= 256 {
            return Err("reverse session limit reached".into());
        }
        let (capability, peer) = UnixStream::pair().map_err(|e| e.to_string())?;
        let root = self.path.parent().ok_or("broker root unavailable")?;
        let config = root.join(format!("mcp-session-{}.json", nonce));
        let installed = std::env::current_exe()
            .map_err(|e| e.to_string())?
            .with_file_name(crate::install::BRIDGE);
        use std::os::unix::fs::OpenOptionsExt;
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&config)
            .map_err(|_| "session MCP projection unavailable")?;
        let projection = (|| {
            let mut projection = wire::server_configuration(
                installed.to_str().ok_or("invalid bridge path")?,
                self.path.to_str().ok_or("invalid broker path")?,
                server_ids.clone(),
            )?;
            let supervisor = std::env::current_exe().map_err(|_| "supervisor unavailable")?;
            for entry in &remote_entries {
                projection["mcpServers"][&entry.id] = serde_json::json!({
                    "command": supervisor,
                    "args": ["--mcp-stdio-supervisor", self.path.to_str().ok_or("invalid broker path")?, &entry.id]
                });
            }
            serde_json::to_writer(&mut output, &projection)
                .map_err(|_| "session MCP projection unavailable")?;
            self.host
                .upgrade()
                .ok_or("session owner unavailable")?
                .owned_supervisors
                .provision(session.as_str(), &nonce)
        })();
        if let Err(error) = projection {
            let _ = remove_configuration(&config, None);
            return Err(error);
        }
        sessions.insert(
            session.as_str().into(),
            Session {
                created: Instant::now(),
                conn: conn.clone(),
                nonce,
                server_ids,
                launch,
                agent_startup,
                owned_settings: None,
                remote_entries,
                resolved: HashMap::new(),
                grant_live: true,
                desktop: None,
                relay: None,
                config,
                pid: None,
                capability,
                _peer: peer,
            },
        );
        Ok(())
    }

    pub(crate) fn prepare(
        &self,
        conn: u64,
        session: &str,
        mut spec: PtySpec,
        expected_launch: Option<NativeLaunch>,
    ) -> Result<PtySpec, String> {
        let sessions = lock(&self.sessions);
        let Some(binding) = sessions.get(session) else {
            return if expected_launch.is_none() {
                Ok(spec)
            } else {
                Err("remote launch requires live session MCP provisioning".into())
            };
        };
        if binding.launch != expected_launch.unwrap_or_default() {
            return Err("remote launch does not match session MCP provisioning".into());
        }
        if binding.conn.id != conn {
            return Err("reverse spawn owner changed".into());
        }
        let launch = binding.launch;
        let agent_startup = binding.agent_startup.clone();
        let config = binding.config.clone();
        let remote_entries = binding.remote_entries.clone();
        let incarnation = binding.nonce.clone();
        drop(sessions);
        let executable = std::env::current_exe().map_err(|e| e.to_string())?;
        let prepared = if launch.is_shell() {
            // A hand-started agent is a convenience: the shell opens without it.
            crate::native_launch::prepare_shell(&mut spec, &config)
                .map(|()| None)
                .or_else(|error| {
                    log::warn!("shell agent settings unavailable: {error}");
                    Ok::<_, String>(None)
                })
        } else {
            crate::native_launch::prepare_agent(
                &mut spec,
                launch,
                &config,
                agent_startup.as_ref(),
                session,
            )
        };
        match prepared {
            Ok(settings) => {
                let mut sessions = lock(&self.sessions);
                match sessions
                    .get_mut(session)
                    .filter(|binding| binding.conn.id == conn && binding.nonce == incarnation)
                {
                    Some(binding) => binding.owned_settings = settings,
                    // Retired meanwhile: nothing will clean up after it, so remove it now.
                    None => {
                        if let Some(settings) = settings {
                            let _ = settings.remove_if_unchanged(&config);
                        }
                    }
                }
            }
            Err(error) => {
                self.release_matching(conn, session, Some(&incarnation));
                return Err(error);
            }
        }
        let resolved = match resolve_remote_entries(&remote_entries, &spec) {
            Ok(resolved) => resolved,
            Err(error) => {
                self.release_matching(conn, session, Some(&incarnation));
                return Err(error);
            }
        };
        let mut sessions = lock(&self.sessions);
        let binding = sessions
            .get_mut(session)
            .filter(|binding| {
                binding.conn.id == conn && binding.grant_live && binding.nonce == incarnation
            })
            .ok_or("reverse spawn incarnation retired during preflight")?;
        binding.resolved = resolved;
        drop(sessions);
        // Alter only argv; all resolved cwd/env/integration and PTY sizing remain.
        let argv = spec.command.get_argv_mut();
        argv.splice(
            0..0,
            [
                executable.into_os_string(),
                "--mcp-session-bootstrap".into(),
                self.path.as_os_str().to_owned(),
                config.as_os_str().to_owned(),
            ],
        );
        Ok(spec)
    }

    pub(crate) fn commit(&self, conn: u64, session: &str, pid: u32) -> Result<(), String> {
        let start = process_start(pid)?;
        let mut sessions = lock(&self.sessions);
        if let Some(binding) = sessions.get_mut(session) {
            if binding.conn.id != conn {
                return Err("reverse spawn owner changed".into());
            }
            binding.pid = Some((pid, start));
        }
        Ok(())
    }

    pub(crate) fn release(&self, owner: u64, session: &str) {
        self.release_matching(owner, session, None);
    }
    fn release_matching(&self, owner: u64, session: &str, incarnation: Option<&str>) {
        let mut sessions = lock(&self.sessions);
        if !sessions
            .get(session)
            .is_some_and(|s| s.conn.id == owner && incarnation.is_none_or(|nonce| s.nonce == nonce))
        {
            return;
        }
        if let Some(binding) = sessions.get_mut(session).filter(|s| s.conn.id == owner) {
            if binding.pid.is_none() {
                let binding = sessions.remove(session).expect("binding was just found");
                let _ = remove_configuration(&binding.config, binding.owned_settings.as_ref());
                if let Some(host) = self.host.upgrade() {
                    host.owned_supervisors.begin_drain(session);
                    host.owned_supervisors
                        .forget_settled(session, &binding.nonce);
                }
            } else {
                binding.grant_live = false;
            }
        }
        drop(sessions);
        self.close_session_streams(session);
    }
    fn close_session_streams(&self, session: &str) {
        let channels: Vec<_> = lock(&self.streams)
            .iter()
            .filter(|(_, v)| v.session == session)
            .map(|(k, _)| *k)
            .collect();
        for channel in channels {
            self.close(channel);
        }
    }
    pub(crate) fn retire(self: &Arc<Self>, session: &str) {
        let nonce = lock(&self.sessions).get(session).map(|s| s.nonce.clone());
        if let Some(nonce) = nonce {
            self.retire_captured(session, nonce);
        }
    }
    fn retire_captured(self: &Arc<Self>, session: &str, nonce: String) {
        let Some(host) = self.host.upgrade() else {
            return;
        };
        let sessions = lock(&self.sessions);
        if sessions.get(session).is_none_or(|s| s.nonce != nonce) {
            return;
        }
        // Keep the incarnation check atomic with gate closure and stream
        // retirement. An old event can never drain a same-id replacement.
        host.owned_supervisors.begin_drain(session);
        self.close_session_streams(session);
        drop(sessions);
        let this = self.clone();
        let session = session.to_owned();
        // Event delivery can hold terminal locks: all waiting stays off that path.
        std::thread::spawn(move || {
            if host
                .owned_supervisors
                .wait(
                    Some(&session),
                    Instant::now() + crate::owned_supervisors::DRAIN_TIME,
                )
                .is_err()
            {
                return;
            }
            let matching = lock(&this.sessions).get(&session).map(|s| &s.nonce) == Some(&nonce);
            if matching {
                if let Err(error) = this.cleanup_settled(Some(&session)) {
                    log::warn!("{error}");
                }
            }
        });
    }
    pub(crate) fn cleanup_settled(&self, selected: Option<&str>) -> Result<(), String> {
        let host = self
            .host
            .upgrade()
            .ok_or("session cleanup owner unavailable")?;
        let mut sessions = lock(&self.sessions);
        let ids: Vec<_> = sessions
            .iter()
            .filter(|(id, binding)| {
                selected.is_none_or(|wanted| wanted == id.as_str())
                    && host.owned_supervisors.settled(id, &binding.nonce)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            let binding = sessions
                .get(&id)
                .ok_or("session cleanup identity changed")?;
            match remove_configuration(&binding.config, binding.owned_settings.as_ref()) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err("remote MCP private projection cleanup incomplete".into()),
            }
            host.owned_supervisors.forget_settled(&id, &binding.nonce);
            let desktop = binding.desktop.clone();
            sessions.remove(&id);
            self.session_ended(&id, desktop);
        }
        Ok(())
    }

    /// `session` ended. Its queued calls stay, for the desktop
    /// (`desktop`) that relayed it to take when it returns; with no desktop
    /// to take them they are recorded as not delivered.
    fn session_ended(&self, session: &str, desktop: Option<String>) {
        let mut outboxes = lock(&self.outbox);
        let Some(outbox) = outboxes.get_mut(session) else {
            return;
        };
        outbox.end(crate::offline_mcp::now_ms());
        if outbox.desktop.is_none() {
            outbox.desktop = desktop;
        }
        if outbox.desktop.is_none() {
            let lost = outbox.drain();
            self.outbox_store.record_lost(
                session,
                &lost,
                "the session ended and no desktop ever relayed it",
                None,
            );
        }
        self.outbox_store.keep(outbox);
        if outbox.is_empty() {
            outboxes.remove(session);
        }
    }

    /// `owner`'s connection ended (its desktop, `desktop`, went away). A
    /// running session's agent keeps its CodeSense MCP server: the machine
    /// answers it (`offline_mcp`) until a desktop takes it back.
    pub(crate) fn disconnect(&self, owner: u64, desktop: Option<String>) {
        let viewer = desktop;
        let mut released = Vec::new();
        {
            let mut sessions = lock(&self.sessions);
            for (id, binding) in sessions.iter_mut() {
                if binding.conn.id == owner {
                    if binding.pid.is_some() {
                        binding.grant_live = false;
                        if binding.desktop.is_none() {
                            binding.desktop = viewer.clone();
                        }
                    } else {
                        released.push(id.clone());
                    }
                }
                if binding
                    .relay
                    .as_ref()
                    .is_some_and(|(conn, _)| conn.id == owner)
                {
                    binding.relay = None;
                }
            }
        }
        lock(&self.orphan_relay).retain(|_, (conn, _)| conn.id != owner);
        lock(&self.named).remove(&owner);
        for session in released {
            self.release(owner, &session);
        }
        let relayed: Vec<_> = lock(&self.streams)
            .values()
            .filter(|stream| {
                lock(&stream.link.route)
                    .desktop()
                    .is_some_and(|(conn, _)| conn.id == owner)
            })
            .map(|stream| (stream.link.clone(), stream.out.clone()))
            .collect();
        for (link, out) in relayed {
            let answers = self.go_local(&link, None, false);
            deliver(out, answers);
        }
    }

    /// Whether `session`'s agent process `pid` still runs on this host,
    /// whoever relays it (a desktop, or none).
    fn alive(&self, session: &str, pid: (u32, u64)) -> bool {
        process_start(pid.0).ok() == Some(pid.1)
            && lock(&self.sessions)
                .get(session)
                .is_some_and(|s| s.pid == Some(pid))
            && self
                .host
                .upgrade()
                .is_some_and(|host| host.reverse_session_live(session, pid.0))
    }

    /// A binding for an agent this host starts on its own (a
    /// machine-armed routine's): owned by no desktop, so its CodeSense MCP is
    /// answered on the machine. `prepare` and `commit` take [`HOST_OWNER`].
    /// `desktop`: the desktop that manages the routine (its viewer), which
    /// gets the agent's CodeSense MCP when it connects.
    pub(crate) fn provision_host(
        &self,
        session: &PtySessionId,
        launch: NativeLaunch,
        agent_startup: wire::AgentStartup,
        desktop: &str,
    ) -> Result<(), String> {
        let (socket, _peer) = UnixStream::pair().map_err(|e| e.to_string())?;
        let (out, _gone) = mpsc::sync_channel(1);
        let conn = Conn::new(HOST_OWNER, out, socket);
        self.provision_agent(
            &conn,
            session,
            random_token()?,
            wire::default_servers(),
            Vec::new(),
            launch,
            Some(agent_startup),
        )?;
        if let Some(binding) = lock(&self.sessions).get_mut(session.as_str()) {
            binding.desktop = (!desktop.is_empty()).then(|| desktop.to_string());
        }
        Ok(())
    }
    fn close(&self, stream: u64) {
        if let Some(stream) = lock(&self.streams).remove(&stream) {
            let _ = stream.socket.shutdown(std::net::Shutdown::Both);
        }
    }
    pub(crate) fn receive(&self, conn: &Conn, frame: Frame) {
        let target = lock(&self.streams)
            .iter()
            .find(|(_, stream)| {
                lock(&stream.link.route)
                    .desktop()
                    .is_some_and(|(relay, channel)| relay.id == conn.id && channel == frame.channel)
            })
            .map(|(id, stream)| (*id, stream.link.clone(), stream.out.clone()));
        let Some((id, link, out)) = target else {
            return;
        };
        if frame.kind == Kind::MCP_DATA && frame.payload.len() <= wire::CHUNK_BYTES {
            let channel = frame.channel;
            if out
                .try_send(Out::Desktop {
                    channel,
                    bytes: frame.payload,
                })
                .is_ok()
            {
                return;
            }
        }
        // A desktop-configured server other than CodeSense ended (it exited,
        // overran its line limit, or its grant was revoked) while the desktop
        // is still here: the agent's stream ends with it, as the server's own
        // stdio would. Its calls in flight are not answered (nothing typed
        // stands in for a server that may have acted), and the stream is not
        // answered by the machine: `desktop_offline` is only for a desktop
        // that went away (`disconnect`).
        if link.server_id != wire::CODESENSE_SERVER && !link.flusher {
            // What the server sent before it closed still reaches the agent,
            // then the stream closes (the writer ends it, in order).
            if frame.kind == Kind::MCP_CLOSE
                && out
                    .try_send(Out::End {
                        channel: frame.channel,
                    })
                    .is_ok()
            {
                return;
            }
            // Overrun (or too far behind to take the end): closed now, and
            // the desktop's end of the channel with it.
            if self.end_relayed(&link, frame.channel) {
                if frame.kind != Kind::MCP_CLOSE {
                    conn.send(Frame {
                        kind: Kind::MCP_CLOSE,
                        channel: frame.channel,
                        payload: vec![],
                    });
                }
                self.close(id);
            }
            return;
        }
        // The desktop closed the CodeSense channel (or overran it): the
        // machine answers the stream from now on; the agent's server stays
        // up.
        let answers = self.go_local(&link, Some(frame.channel), false);
        deliver(out, answers);
    }

    /// `link`, a relayed server other than CodeSense, is no longer relayed
    /// on `channel`: no machine answers stand in for it, and its calls in
    /// flight stay unanswered. False when it was not routed there.
    fn end_relayed(&self, link: &Link, channel: u32) -> bool {
        {
            let mut route = lock(&link.route);
            if route.desktop().map(|(_, on)| on) != Some(channel) {
                return false;
            }
            *route = Route::Local;
            lock(&link.pending).clear();
        }
        true
    }

    fn live(&self, session: &str, pid: (u32, u64)) -> bool {
        let conn_id = lock(&self.sessions)
            .get(session)
            .filter(|s| s.pid == Some(pid) && s.grant_live)
            .map(|s| s.conn.id);
        process_start(pid.0).ok() == Some(pid.1)
            && self.host.upgrade().is_some_and(|host| {
                conn_id.is_some_and(|id| host.reverse_connection_live(id))
                    && host.reverse_session_live(session, pid.0)
            })
    }
    fn accept(self: &Arc<Self>, mut socket: UnixStream) -> Result<(), String> {
        let deadline = Instant::now() + DEADLINE;
        socket
            .set_read_timeout(Some(DEADLINE))
            .map_err(|e| e.to_string())?;
        socket
            .set_write_timeout(Some(DEADLINE))
            .map_err(|e| e.to_string())?;
        let (peer_pid, peer_uid) = peer_credentials(&socket)?;
        if peer_uid != unsafe { libc::geteuid() } {
            return Err("reverse ingress owner mismatch".into());
        }
        let peer_start = process_start(peer_pid)?;
        let mut mode = [0];
        socket.read_exact(&mut mode).map_err(|e| e.to_string())?;
        if mode == [wire::MODE_BOOTSTRAP] {
            loop {
                // No registry lock is held while waiting for spawn to register.
                let candidate = lock(&self.sessions).iter().find_map(|(id, s)| {
                    s.pid
                        .filter(|p| *p == (peer_pid, peer_start))
                        .map(|pid| (id.clone(), pid))
                });
                if let Some((id, pid)) = candidate {
                    if !self.alive(&id, pid) {
                        return Err("bootstrap session ended".into());
                    }
                    let capability = {
                        let sessions = lock(&self.sessions);
                        sessions
                            .get(&id)
                            .filter(|s| s.pid == Some(pid))
                            .ok_or("bootstrap retired")?
                            .capability
                            .try_clone()
                            .map_err(|e| e.to_string())?
                    };
                    auth_timeout(&socket, deadline)?;
                    return broker::send_fd(&socket, capability.as_raw_fd())
                        .map_err(|e| e.to_string());
                }
                if Instant::now() >= deadline {
                    return Err("bootstrap registration unavailable".into());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        if mode == [wire::MODE_REACQUIRE] {
            let (candidate, _) = self.lineage_session(peer_pid, peer_start)?;
            let capability = {
                let sessions = lock(&self.sessions);
                let session = sessions
                    .get(&candidate.0)
                    .filter(|s| s.pid == Some(candidate.1))
                    .ok_or("bridge session retired")?;
                session.capability.try_clone().map_err(|e| e.to_string())?
            };
            auth_timeout(&socket, deadline)?;
            broker::send_fd(&socket, capability.as_raw_fd()).map_err(|e| e.to_string())?;
            auth_timeout(&socket, deadline)?;
            socket.read_exact(&mut mode).map_err(|e| e.to_string())?;
        }
        if mode != [wire::MODE_MCP] && mode != [wire::MODE_MCP_SERVER] && mode != [MODE_SUPERVISOR]
        {
            return Err("invalid broker audience".into());
        }
        auth_timeout(&socket, deadline)?;
        let capability = broker::receive_fd(&socket).map_err(|e| e.to_string())?;
        let identity = broker::identity(capability.as_raw_fd()).map_err(|e| e.to_string())?;
        let bound = lock(&self.sessions)
            .iter()
            .find_map(|(id, s)| {
                (broker::identity(s.capability.as_raw_fd()).ok() == Some(identity))
                    .then(|| {
                        s.pid
                            .map(|pid| (id.clone(), pid, s.conn.clone(), s.nonce.clone()))
                    })
                    .flatten()
            })
            .ok_or("unknown session capability")?;
        drop(capability);
        let (session, pid, conn, nonce) = bound;
        if !self.alive(&session, pid) {
            return Err("session ended".into());
        }
        let challenge = random_token()?;
        let digest = Sha256::digest(challenge.as_bytes());
        auth_timeout(&socket, deadline)?;
        wire::write_challenge(&mut socket, &challenge).map_err(|e| e.to_string())?;
        drop(challenge);
        let response =
            wire::read_message(&mut socket, 128, |socket| auth_timeout(socket, deadline))?;
        let expected = Sha256::digest(
            response
                .strip_prefix(&format!("{} ", wire::AUDIENCE))
                .ok_or("wrong audience")?
                .trim_end_matches('\n')
                .as_bytes(),
        );
        if digest
            .iter()
            .zip(expected.iter())
            .fold(0u8, |different, (a, b)| different | (a ^ b))
            != 0
        {
            return Err("invalid one-use challenge".into());
        }
        let server_id = if mode == [wire::MODE_MCP_SERVER] || mode == [MODE_SUPERVISOR] {
            wire::read_server_selector(&mut socket, |s| auth_timeout(s, deadline))?
        } else {
            wire::default_server()
        };
        if mode == [MODE_SUPERVISOR] {
            let chain = process_chain(peer_pid)?;
            if chain.first().map(|entry| entry.1) != Some(peer_start)
                || !chain.iter().any(|entry| (entry.0, entry.1) == pid)
            {
                return Err("supervisor is not a managed native client descendant".into());
            }
            let config = lock(&self.sessions)
                .get(&session)
                .filter(|s| s.nonce == nonce && s.grant_live)
                .and_then(|s| s.resolved.get(&server_id))
                .cloned()
                .ok_or("remote MCP selected entry unavailable")?;
            let client = crate::owned_supervisors::pidfd(pid.0)?;
            let supervisor = crate::owned_supervisors::pidfd(peer_pid)?;
            if process_start(pid.0)? != pid.1 || process_start(peer_pid)? != peer_start {
                return Err("remote MCP process identity changed".into());
            }
            let host = self.host.upgrade().ok_or("session owner unavailable")?;
            return host.owned_supervisors.register(
                crate::owned_supervisors::SessionIncarnation {
                    session: &session,
                    nonce: &nonce,
                },
                socket,
                supervisor,
                &client,
                &config,
                self.live(&session, pid),
            );
        }
        // Redeem on this stream only, while exact session+owner is still present.
        if !lock(&self.sessions).get(&session).is_some_and(|s| {
            s.conn.id == conn.id && s.nonce == nonce && s.server_ids.contains(&server_id)
        }) {
            return Err("binding retired".into());
        }
        if !self.alive(&session, pid) {
            return Err("session ended".into());
        }
        // Relayed by its desktop (or one that came back for it), else
        // answered on the machine until one does.
        let relay = self.relay_for(&session, &server_id);
        self.run_stream(socket, session, server_id, relay, false)
    }

    /// The relay `session`'s stream of `server_id` goes through now: the
    /// desktop that bound it while it is connected, else (CodeSense only) a
    /// desktop that came back for it.
    pub(crate) fn relay_for(&self, session: &str, server_id: &str) -> Option<(Conn, String)> {
        let (original, returned) = {
            let sessions = lock(&self.sessions);
            let binding = sessions.get(session)?;
            (
                (binding.grant_live
                    && binding.conn.id != HOST_OWNER
                    && binding.server_ids.iter().any(|id| id == server_id))
                .then(|| (binding.conn.clone(), binding.nonce.clone())),
                (server_id == wire::CODESENSE_SERVER)
                    .then(|| binding.relay.clone())
                    .flatten(),
            )
        };
        let host = self.host.upgrade()?;
        original
            .filter(|(conn, _)| host.reverse_connection_live(conn.id))
            .or_else(|| returned.filter(|(conn, _)| host.reverse_connection_live(conn.id)))
    }

    fn next_channel(&self) -> Result<u32, String> {
        match self.next_channel.fetch_add(1, Ordering::AcqRel) {
            0 => Err("reverse channels exhausted".into()),
            channel => Ok(channel),
        }
    }

    /// Serve one MCP stream of `session`'s agent on `socket`, relayed on
    /// `relay` or answered here, until the agent closes it or its session
    /// ends. Never closed for a desktop's sake: the route changes under it.
    /// `flusher`: the outbox delivery's own stream.
    pub(crate) fn run_stream(
        self: &Arc<Self>,
        mut socket: UnixStream,
        session: String,
        server_id: String,
        relay: Option<(Conn, String)>,
        flusher: bool,
    ) -> Result<(), String> {
        let id = self.next_stream.fetch_add(1, Ordering::AcqRel);
        let (tx, rx) = mpsc::sync_channel(STREAM_QUEUE_CHUNKS);
        let link = Arc::new(Link {
            session: session.clone(),
            server_id: server_id.clone(),
            route: Mutex::new(Route::Local),
            pending: Mutex::default(),
            init: Mutex::default(),
            flusher,
        });
        {
            let mut streams = lock(&self.streams);
            // The outbox delivery (one per session at a time) never takes an
            // agent's place.
            if !flusher
                && (streams.values().filter(|s| !s.link.flusher).count() >= wire::MAX_STREAMS
                    || streams
                        .values()
                        .filter(|s| s.session == session && !s.link.flusher)
                        .count()
                        >= wire::MAX_SESSION_STREAMS)
            {
                return Err("reverse stream limit reached".into());
            }
            streams.insert(
                id,
                Stream {
                    session: session.clone(),
                    socket: socket.try_clone().map_err(|e| e.to_string())?,
                    out: tx.clone(),
                    link: link.clone(),
                },
            );
        }
        // At most STREAM_QUEUE_CHUNKS × 16KiB (~1MiB) queued per stream, plus
        // one partial line of at most LINE_BYTES held by its writer.
        let mut writer = socket.try_clone().map_err(|e| e.to_string())?;
        let weak = Arc::downgrade(self);
        let written = link.clone();
        std::thread::spawn(move || {
            // Desktop bytes reach the agent as whole lines, so the machine's
            // answers never land inside one.
            let mut partial: (u32, Vec<u8>) = (0, Vec::new());
            'out: for out in rx {
                let lines = match out {
                    Out::End { channel } => {
                        if let Some(this) = weak.upgrade() {
                            this.end_relayed(&written, channel);
                        }
                        break 'out;
                    }
                    Out::Line(mut line) => {
                        if !line.ends_with('\n') {
                            line.push('\n');
                        }
                        vec![line.into_bytes()]
                    }
                    Out::Desktop { channel, bytes } => {
                        if partial.0 != channel {
                            partial = (channel, Vec::new());
                        }
                        // Only the new bytes are searched: what was held
                        // already has no line end (a long line stays linear).
                        let mut from = partial.1.len();
                        partial.1.extend_from_slice(&bytes);
                        let mut lines = Vec::new();
                        while let Some(end) = partial.1[from..].iter().position(|b| *b == b'\n') {
                            let line: Vec<u8> = partial.1.drain(..=from + end).collect();
                            from = 0;
                            match weak.upgrade() {
                                Some(this) => lines.extend(
                                    this.desktop_line(&written, channel, &line).into_iter().map(
                                        |mut line| {
                                            if !line.ends_with('\n') {
                                                line.push('\n');
                                            }
                                            line.into_bytes()
                                        },
                                    ),
                                ),
                                None => lines.push(line),
                            }
                        }
                        if partial.1.len() > wire::LINE_BYTES {
                            partial.1.clear();
                        }
                        lines
                    }
                };
                for line in lines {
                    if wire::write_payload(&mut writer, &line).is_err() {
                        break 'out;
                    }
                }
            }
            if let Some(this) = weak.upgrade() {
                this.close(id);
            }
        });
        if let Some((conn, nonce)) = relay {
            let channel = self.next_channel()?;
            *lock(&link.route) = Route::relayed(conn.clone(), channel);
            if !send_open(&conn, &link, channel, &nonce) {
                let answers = self.go_local(&link, Some(channel), false);
                deliver(tx.clone(), answers);
            }
        }
        socket.set_read_timeout(None).map_err(|e| e.to_string())?;
        let result = (|| loop {
            let line = wire::read_message(&mut socket, wire::LINE_BYTES, |_| Ok(()))?;
            for answer in self.agent_line(&link, line) {
                if tx.send(Out::Line(answer)).is_err() {
                    return Err("reverse stream closed".into());
                }
            }
        })();
        self.close(id);
        if let Some((conn, channel)) = lock(&link.route).desktop() {
            conn.send(Frame {
                kind: Kind::MCP_CLOSE,
                channel,
                payload: vec![],
            });
        }
        result
    }

    /// One line from the agent: relayed, held for a returning desktop, or
    /// answered here. Returns the machine's answers.
    fn agent_line(&self, link: &Link, line: String) -> Vec<String> {
        use crate::offline_mcp as offline;
        // Only the machine names an outbox delivery (a forged id could make
        // the desktop skip a real call).
        let line = if link.flusher {
            line
        } else {
            offline::strip_outbox_meta(&line).unwrap_or(line)
        };
        let call = offline::parse(&line);
        if call
            .as_ref()
            .is_some_and(|call| call.method == "initialize")
        {
            // The desktop closed the stream on a line before `initialize` (Claude's version
            // probe, which the desktop's server refuses): the agent's `initialize` goes to
            // its desktop all the same, so the instructions it keeps are the desktop's.
            // Relayed before it is stored, so no hand-back takes the stream meanwhile.
            if !link.flusher && link.server_id == wire::CODESENSE_SERVER {
                self.relay_again(link);
            }
            *lock(&link.init) = Some(line.clone());
        }
        // Queued calls go out first, in order: one sent after them joins them.
        if call
            .as_ref()
            .is_some_and(|call| self.queues_behind(link, call))
        {
            return self.answer_locally(link, &line).into_iter().collect();
        }
        let mut route = lock(&link.route);
        match &mut *route {
            Route::Desktop { conn, channel, .. } => {
                let (conn, channel) = (conn.clone(), *channel);
                let key = call
                    .as_ref()
                    .map(|call| (offline::id_key(&call.id), call.what()));
                if let Some(key) = &key {
                    lock(&link.pending).push(key.clone());
                }
                if send_line(&conn, channel, &line) {
                    return Vec::new();
                }
                // The desktop went away under this line, which it never got
                // whole: the machine answers it, and every line after it.
                if let Some((id, _)) = &key {
                    lock(&link.pending).retain(|(pending, _)| pending != id);
                }
                drop(route);
                let mut answers = self.go_local(link, Some(channel), false);
                answers.extend(self.answer_locally(link, &line));
                answers
            }
            Route::Reattaching { held, .. } if held.len() < MAX_HELD => {
                held.push(line);
                Vec::new()
            }
            Route::Reattaching { .. } => {
                // Too much waits for the returning desktop: the machine
                // answers it all, and the hand-back is tried again.
                drop(route);
                let mut answers = self.go_local(link, None, true);
                answers.extend(self.answer_locally(link, &line));
                answers
            }
            Route::Local => {
                drop(route);
                // A desktop is there but the hand-back gave up: the agent's
                // own call tries it again (at most every 30 s).
                if !link.flusher && link.server_id == wire::CODESENSE_SERVER {
                    self.restart(&link.session);
                }
                self.answer_locally(link, &line).into_iter().collect()
            }
        }
    }

    /// `link`, answered by the machine, is relayed again on a new channel when its desktop is
    /// there; it stays the machine's when the desktop cannot be told.
    fn relay_again(&self, link: &Link) {
        if !matches!(*lock(&link.route), Route::Local) {
            return;
        }
        let Some((conn, nonce)) = self.relay_for(&link.session, &link.server_id) else {
            return;
        };
        let Ok(channel) = self.next_channel() else {
            return;
        };
        {
            let mut route = lock(&link.route);
            if !matches!(*route, Route::Local) {
                return;
            }
            *route = Route::relayed(conn.clone(), channel);
        }
        if !send_open(&conn, link, channel, &nonce) {
            let mut route = lock(&link.route);
            if route.desktop().is_some_and(|(_, on)| on == channel) {
                *route = Route::Local;
            }
        }
    }

    /// Restart `session`'s hand-back, rate-limited.
    fn restart(&self, session: &str) {
        {
            let mut restarted = lock(&self.restarted);
            if restarted
                .get(session)
                .is_some_and(|at| at.elapsed() < SYNC_RESTART_EVERY)
            {
                return;
            }
            restarted.insert(session.to_string(), Instant::now());
        }
        if let Some(this) = self.me.upgrade() {
            this.kick(session);
        }
    }

    /// Whether `call` on `link` joins its session's outbox although a
    /// desktop relays the stream: calls queued before it are not delivered
    /// yet.
    fn queues_behind(&self, link: &Link, call: &crate::offline_mcp::Call) -> bool {
        !link.flusher
            && link.server_id == wire::CODESENSE_SERVER
            && call
                .tool
                .as_deref()
                .is_some_and(|tool| crate::offline_mcp::OUTBOX_TOOLS.contains(&tool))
            && lock(&self.outbox)
                .get(&link.session)
                .is_some_and(|outbox| !outbox.is_empty())
    }

    /// One whole line the desktop sent on `channel`: what reaches the agent.
    /// The answer to a replayed `initialize` is the machine's, and so is a
    /// late answer to a request the machine already answered.
    fn desktop_line(&self, link: &Link, channel: u32, line: &[u8]) -> Vec<String> {
        use crate::offline_mcp as offline;
        let text = String::from_utf8_lossy(line).into_owned();
        let mut route = lock(&link.route);
        let current = route.desktop().is_some_and(|(_, on)| on == channel);
        let Some(id) = offline::response_id(&text) else {
            // The server's own requests and notifications.
            return if current && matches!(*route, Route::Desktop { .. }) {
                vec![text]
            } else {
                Vec::new()
            };
        };
        if id == serde_json::json!(offline::REATTACH_ID) {
            let Route::Reattaching {
                conn,
                channel: on,
                held,
            } = &mut *route
            else {
                return Vec::new();
            };
            if *on != channel {
                return Vec::new();
            }
            if offline::is_error(&text) {
                drop(route);
                return self.go_local(link, Some(channel), true);
            }
            let (conn, held) = (conn.clone(), std::mem::take(held));
            *route = Route::relayed(conn.clone(), channel);
            // The agent re-lists: the desktop's tools are back.
            let mut answers = vec![offline::LIST_CHANGED.to_string()];
            for (n, line) in held.iter().enumerate() {
                let call = offline::parse(line);
                if call
                    .as_ref()
                    .is_some_and(|call| self.queues_behind(link, call))
                {
                    answers.extend(self.answer_locally(link, line));
                    continue;
                }
                let key = call.map(|call| (offline::id_key(&call.id), call.what()));
                if let Some(key) = &key {
                    lock(&link.pending).push(key.clone());
                }
                if !send_line(&conn, channel, line) {
                    if let Some((id, _)) = &key {
                        lock(&link.pending).retain(|(pending, _)| pending != id);
                    }
                    drop(route);
                    answers.extend(self.go_local(link, Some(channel), false));
                    for unsent in &held[n..] {
                        answers.extend(self.answer_locally(link, unsent));
                    }
                    return answers;
                }
            }
            return answers;
        }
        if !current {
            return Vec::new();
        }
        let key = offline::id_key(&id);
        let mut pending = lock(&link.pending);
        match pending.iter().position(|(pending, _)| *pending == key) {
            Some(at) => {
                let (_, what) = pending.remove(at);
                if let Route::Desktop { listed, .. } = &mut *route {
                    *listed |= what == LISTING;
                }
                vec![text]
            }
            // Already answered by the machine: never a second answer.
            None => Vec::new(),
        }
    }

    /// The machine answers `link` from now on, if it is still routed on
    /// `channel` (any channel when `None`; only while being handed back when
    /// `reattaching`). Returns the answers the agent is owed: each request in
    /// flight to the desktop gets a typed `desktop_offline` (never replayed),
    /// each line held for a returning desktop is answered here, and the
    /// agent is told its tool list changed. While a desktop is still there
    /// for the session, handing the stream back is tried again.
    fn go_local(&self, link: &Link, channel: Option<u32>, reattaching: bool) -> Vec<String> {
        // Both under the route's lock: a relay that begins after it never sees these
        // requests as its own.
        let (previous, pending) = {
            let mut route = lock(&link.route);
            if channel.is_some_and(|channel| route.desktop().map(|(_, on)| on) != Some(channel))
                || (reattaching && !matches!(*route, Route::Reattaching { .. }))
            {
                return Vec::new();
            }
            let pending = std::mem::take(&mut *lock(&link.pending));
            (std::mem::replace(&mut *route, Route::Local), pending)
        };
        let init = lock(&link.init).clone();
        let mut answers: Vec<String> = pending
            .into_iter()
            .filter_map(|(id, what)| {
                // An `initialize` has no effects: the machine answers it, so the agent keeps
                // its CodeSense server.
                if what == "initialize" && !link.flusher {
                    return self.answer_locally(link, init.as_deref()?);
                }
                let id = serde_json::from_str(&id).ok()?;
                Some(if link.flusher {
                    crate::offline_mcp::delivery_refused(&id)
                } else {
                    crate::offline_mcp::in_flight_error(&id, &what)
                })
            })
            .collect();
        let was_relayed = previous.desktop().is_some();
        if let Some((conn, channel)) = previous.desktop() {
            conn.send(Frame {
                kind: Kind::MCP_CLOSE,
                channel,
                payload: vec![],
            });
        }
        if let Route::Reattaching { held, .. } = previous {
            for line in held {
                answers.extend(self.answer_locally(link, &line));
            }
        }
        if !link.flusher && was_relayed && lock(&link.init).is_some() {
            answers.push(crate::offline_mcp::LIST_CHANGED.to_string());
        }
        if !link.flusher && link.server_id == wire::CODESENSE_SERVER {
            if let Some(this) = self.me.upgrade() {
                this.kick(&link.session);
            }
        }
        answers
    }
}

/// Tells `conn`'s desktop that `link` is relayed on `channel`, bound by `nonce`. False when
/// the desktop is gone (or the frame could not be made).
fn send_open(conn: &Conn, link: &Link, channel: u32, nonce: &str) -> bool {
    wire::open_frame(
        channel,
        &wire::Open {
            session: link.session.clone(),
            binding_nonce: nonce.to_string(),
            server_id: link.server_id.clone(),
        },
    )
    .is_ok_and(|open| conn.send(open))
}

/// `line` to `conn` on `channel`, whole.
fn send_line(conn: &Conn, channel: u32, line: &str) -> bool {
    let mut line = line.trim_end_matches('\n').to_string();
    line.push('\n');
    line.as_bytes().chunks(wire::CHUNK_BYTES).all(|chunk| {
        conn.send(Frame {
            kind: Kind::MCP_DATA,
            channel,
            payload: chunk.into(),
        })
    })
}

/// The machine's `answers` to a stream's agent, off the caller's thread (a
/// connection's reader must never wait on an agent).
fn deliver(out: mpsc::SyncSender<Out>, answers: Vec<String>) {
    if answers.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        for answer in answers {
            if out.send(Out::Line(answer)).is_err() {
                return;
            }
        }
    });
}

/// Why an outbox delivery stopped.
#[derive(Debug)]
enum Stop {
    /// The desktop is there but would not take the session's calls.
    Refused(String),
    /// The desktop went away (or did not answer).
    Gone(String),
}

impl ReverseMcp {
    /// The machine's answer to one line of a stream no desktop relays:
    /// `routine_done` / `signal_done`, the session's own timers,
    /// the outbox, and a typed `desktop_offline` for everything else.
    fn answer_locally(&self, link: &Link, line: &str) -> Option<String> {
        use crate::offline_mcp as offline;
        use crate::routine_arm::local_mcp::Offline;
        let call = offline::parse(line);
        if link.flusher {
            return call.map(|call| offline::delivery_refused(&call.id) + "\n");
        }
        let session = link.session.as_str();
        let codesense = link.server_id == wire::CODESENSE_SERVER;
        let host = self.host.upgrade();
        if let (true, Some(call)) = (codesense, &call) {
            match call.tool.as_deref() {
                Some(tool @ ("list_wakeups" | "cancel_wakeup")) => {
                    if !offline::own_session(call, session) {
                        return Some(offline::other_session(&call.id, tool) + "\n");
                    }
                    if let Some(wake) = host.as_ref().and_then(|host| host.wake()) {
                        let own = PtySessionId::new(session);
                        let answer = if tool == "list_wakeups" {
                            offline::timers_answer(&call.id, wake.timers(Some(&own)))
                        } else {
                            match call.args.get("timerId").and_then(|id| id.as_str()) {
                                Some(timer) => {
                                    offline::cancel_answer(&call.id, wake.cancel_timer(&own, timer))
                                }
                                None => offline::invalid(&call.id, tool, "timerId"),
                            }
                        };
                        return Some(answer + "\n");
                    }
                }
                Some(tool) if offline::OUTBOX_TOOLS.contains(&tool) => {
                    return Some(self.enqueue(session, call, line) + "\n");
                }
                _ => {}
            }
        }
        let arms = host.as_ref().and_then(|host| host.arms());
        let why = if lock(&self.sessions)
            .get(session)
            .is_some_and(|s| s.conn.id == HOST_OWNER)
        {
            Offline::StartedByMachine
        } else {
            Offline::DesktopGone
        };
        let answer =
            crate::routine_arm::local_mcp::respond(line, session, arms.as_deref(), codesense, why)?;
        let answer = if codesense && call.is_some_and(|call| call.method == "tools/list") {
            offline::with_extra_tools(&answer)
        } else {
            answer
        };
        Some(answer + "\n")
    }

    /// Queue `call` (`line`) in `session`'s outbox, on disk: the agent's
    /// answer. A desktop that is there takes it at once.
    fn enqueue(&self, session: &str, call: &crate::offline_mcp::Call, line: &str) -> String {
        use crate::offline_mcp as offline;
        let tool = call.what();
        if let Some(field) = offline::missing(call) {
            return offline::invalid(&call.id, &tool, field);
        }
        let key = format!(
            "{}-{}",
            self.outbox_boot,
            self.outbox_seq.fetch_add(1, Ordering::AcqRel)
        );
        let desktop = lock(&self.sessions)
            .get(session)
            .and_then(|binding| binding.desktop.clone());
        let answer = {
            let mut outboxes = lock(&self.outbox);
            let used: usize = outboxes.values().map(|outbox| outbox.bytes()).sum();
            let outbox = outboxes
                .entry(session.to_string())
                .or_insert_with(|| offline::Outbox::new(session));
            if outbox.desktop.is_none() {
                outbox.desktop = desktop;
            }
            let pushed = outbox.push(
                key.clone(),
                line,
                offline::OUTBOX_HOST_BYTES.saturating_sub(used),
            );
            // Queued only once it is on disk: a call the machine could lose
            // is refused now, not answered "queued".
            let pushed = pushed.and_then(|position| match self.outbox_store.save(outbox) {
                Ok(()) => Ok(position),
                Err(error) => {
                    outbox.unpush(&key);
                    Err(format!("this machine could not save it ({error})"))
                }
            });
            if outbox.is_empty() {
                outboxes.remove(session);
            }
            match pushed {
                Ok(position) => offline::queued_answer(&call.id, &tool, &key, position),
                Err(why) => return offline::outbox_full_because(&call.id, &tool, &why),
            }
        };
        if let Some(this) = self.me.upgrade() {
            this.kick(session);
        }
        answer
    }

    /// The relay `session`'s queued calls and CodeSense streams
    /// are handed to now, if any: its desktop's, a returning desktop's, or
    /// (an ended session's) the desktop that came back for its outbox.
    fn sync_relay(&self, session: &str) -> Option<(Conn, String)> {
        self.relay_for(session, wire::CODESENSE_SERVER).or_else(|| {
            let relay = lock(&self.orphan_relay).get(session).cloned()?;
            let host = self.host.upgrade()?;
            host.reverse_connection_live(relay.0.id).then_some(relay)
        })
    }

    /// Tests: shorter hand-back deadlines and retries.
    #[cfg(test)]
    fn set_timing(&self, reattach: Duration, flush: Duration, backoff: Duration) {
        *lock(&self.reattach_deadline) = reattach;
        *lock(&self.flush_deadline) = flush;
        *lock(&self.sync_backoff) = backoff;
    }

    /// The desktop `identity` (an older one: its `viewer`)
    /// connected on `conn` and gave its routine secret. Sessions it relayed
    /// (or whose routine it manages) that no desktop relays now are handed
    /// back to it: their outbox first, in order, then their agents'
    /// CodeSense streams, each with its `initialize` replayed. So are the
    /// queued calls of such sessions that ended meanwhile. Nothing an agent
    /// sent before is replayed.
    pub(crate) fn reattach(
        self: &Arc<Self>,
        identity: &str,
        named: bool,
        secret: &str,
        conn: &Conn,
    ) {
        if named {
            lock(&self.named).insert(conn.id);
        }
        let keyed =
            |session: &str| codesense_pty_core::routine_arm::routine_binding_nonce(secret, session);
        // The desktop that relayed it (or whose routine started it), by its
        // identity only.
        let matches = |desktop: Option<&str>, _host_owned: bool| desktop == Some(identity);
        self.expire_ended();
        if named {
            self.note_lost(identity, secret, conn);
        }
        let candidates: Vec<(String, (u32, u64))> = {
            let mut sessions = lock(&self.sessions);
            sessions
                .iter_mut()
                .filter_map(|(id, binding)| {
                    let pid = binding.pid?;
                    let host_owned = binding.conn.id == HOST_OWNER;
                    let relayed = binding.grant_live && !host_owned;
                    if relayed || !matches(binding.desktop.as_deref(), host_owned) {
                        return None;
                    }
                    binding.relay = Some((conn.clone(), keyed(id)));
                    Some((id.clone(), pid))
                })
                .collect()
        };
        let mut kicks: Vec<String> = candidates
            .into_iter()
            .filter(|(session, pid)| self.alive(session, *pid))
            .map(|(session, _)| session)
            .collect();
        let orphans: Vec<String> = lock(&self.outbox)
            .values()
            .filter(|outbox| outbox.ended && !outbox.is_empty())
            .filter(|outbox| matches(outbox.desktop.as_deref(), true))
            .map(|outbox| outbox.session.clone())
            .collect();
        {
            let mut relays = lock(&self.orphan_relay);
            for session in &orphans {
                relays.insert(session.clone(), (conn.clone(), keyed(session)));
            }
        }
        kicks.extend(orphans);
        for session in kicks {
            self.kick(&session);
        }
    }

    /// Records stamped with `viewer` before their desktop named
    /// itself `identity` (install `host_part`): the bindings of sessions this
    /// host started for that install's routines, their ended outboxes and
    /// pending loss notes. `install_of` names a session's originating install.
    pub(crate) fn restamp(
        &self,
        viewer: &str,
        host_part: &str,
        identity: &str,
        install_of: &dyn Fn(&str) -> Option<String>,
    ) {
        let ours = |session: &str| {
            crate::offline_mcp::same_install(install_of(session).as_deref(), host_part)
        };
        for (session, binding) in lock(&self.sessions).iter_mut() {
            if binding.conn.id == HOST_OWNER
                && binding.desktop.as_deref() == Some(viewer)
                && ours(session)
            {
                binding.desktop = Some(identity.to_string());
            }
        }
        for outbox in lock(&self.outbox).values_mut() {
            if outbox.desktop.as_deref() == Some(viewer) && ours(&outbox.session) {
                outbox.desktop = Some(identity.to_string());
                self.outbox_store.keep(outbox);
            }
        }
        self.outbox_store.rename_notes(viewer, identity, ours);
    }

    /// The managed session process `peer_pid` (started at `peer_start`) runs under, by
    /// its kernel-verified ancestry: the session and its root process, and the chain.
    fn lineage_session(
        &self,
        peer_pid: u32,
        peer_start: u64,
    ) -> Result<((String, (u32, u64)), Vec<(u32, u64, u32)>), String> {
        let chain = process_chain(peer_pid)?;
        if chain.first().map(|entry| entry.1) != Some(peer_start) {
            return Err("bridge process identity changed".into());
        }
        let candidate = lock(&self.sessions)
            .iter()
            .find_map(|(id, s)| {
                s.pid
                    .filter(|pid| chain.iter().any(|entry| (entry.0, entry.1) == *pid))
                    .map(|pid| (id.clone(), pid))
            })
            .ok_or("bridge is not a managed session descendant")?;
        if !self.alive(&candidate.0, candidate.1) || process_chain(peer_pid)? != chain {
            return Err("bridge ancestry changed or session ended".into());
        }
        Ok((candidate, chain))
    }

    /// The saved-Agent startup `session` was provisioned with, if any.
    pub(crate) fn agent_startup(&self, session: &str) -> Option<wire::AgentStartup> {
        lock(&self.sessions)
            .get(session)
            .and_then(|s| s.agent_startup.clone())
    }

    /// `session`'s binding when it is relayed here: the desktop it is bound to, if any.
    /// `None` for a session this broker does not relay.
    pub(crate) fn binding(&self, session: &str) -> Option<Option<String>> {
        lock(&self.sessions).get(session).map(|s| s.desktop.clone())
    }

    /// The bound desktop of the sessions `conn` relays, named `identity` now
    /// that it said who it is (it was its viewer until then). Every session
    /// bound to `identity` is reported to it again (`InboxWaitV1`).
    pub(crate) fn name_desktop(&self, conn: u64, identity: &str) {
        let mut named = Vec::new();
        for (session, binding) in lock(&self.sessions).iter_mut() {
            if binding.conn.id == conn {
                binding.desktop = Some(identity.to_string());
            }
            if binding.desktop.as_deref() == Some(identity) {
                named.push(session.clone());
            }
        }
        // Those an older link of it was told of too: this link asked for its waiters
        // before it was named, and was given none.
        if let Some(host) = self.host.upgrade() {
            for session in named {
                host.terminal.activity().inbox_report_again(&session);
            }
        }
    }

    /// The desktop `identity` that relays `session`.
    pub(crate) fn set_desktop(&self, session: &str, identity: Option<String>) {
        if let Some(binding) = lock(&self.sessions).get_mut(session) {
            if identity.is_some() {
                binding.desktop = identity;
            }
        }
        // `InboxWaitV1`: its desktop hears whom the session's mail reaches at the next
        // sweep (the caller holds the host's connection registry, which reporting reads).
    }

    /// Ended sessions' queued calls older than 30 days: recorded as not
    /// delivered.
    fn expire_ended(&self) {
        let now = crate::offline_mcp::now_ms();
        let mut outboxes = lock(&self.outbox);
        for outbox in outboxes.values_mut().filter(|outbox| outbox.expired(now)) {
            let lost = outbox.drain();
            self.outbox_store.record_lost(
                &outbox.session,
                &lost,
                "its desktop did not come back within 30 days",
                outbox.desktop.as_deref(),
            );
            self.outbox_store.keep(outbox);
        }
        outboxes.retain(|_, outbox| !outbox.is_empty());
    }

    /// Tell desktop `identity` (back on `conn`) once, in one inbox note,
    /// which queued calls could not be delivered since it last connected.
    fn note_lost(&self, identity: &str, secret: &str, conn: &Conn) {
        let notes = self.outbox_store.take_notes(identity);
        let Some(session) = notes
            .last()
            .and_then(|entry| entry["session"].as_str())
            .map(str::to_string)
        else {
            return;
        };
        let key = format!(
            "{}{}-{}",
            crate::offline_mcp::NOTE_KEY_PREFIX,
            self.outbox_boot,
            self.outbox_seq.fetch_add(1, Ordering::AcqRel)
        );
        let live = lock(&self.sessions).contains_key(&session);
        let mut outboxes = lock(&self.outbox);
        let outbox = outboxes
            .entry(session.clone())
            .or_insert_with(|| crate::offline_mcp::Outbox::new(&session));
        outbox.desktop.get_or_insert_with(|| identity.to_string());
        if !live {
            outbox.end(crate::offline_mcp::now_ms());
        }
        if outbox
            .push(key, &crate::offline_mcp::lost_note(&notes), usize::MAX)
            .is_ok()
        {
            self.outbox_store.keep(outbox);
        }
        drop(outboxes);
        if !live {
            lock(&self.orphan_relay).insert(
                session.clone(),
                (
                    conn.clone(),
                    codesense_pty_core::routine_arm::routine_binding_nonce(secret, &session),
                ),
            );
        }
    }

    /// Hand `session` to its desktop (again): one worker per session, a few
    /// at once, retried with a capped backoff while a desktop is there for
    /// it and the hand-back does not hold.
    fn kick(self: &Arc<Self>, session: &str) {
        if self.sync_relay(session).is_none() {
            return;
        }
        {
            let mut syncing = lock(&self.syncing);
            if let Some(again) = syncing.get_mut(session) {
                *again = true;
                return;
            }
            syncing.insert(session.to_string(), false);
        }
        let this = self.clone();
        let session = session.to_string();
        std::thread::spawn(move || this.run_sync(&session));
    }

    fn run_sync(self: &Arc<Self>, session: &str) {
        while self.sync_running.fetch_add(1, Ordering::AcqRel) >= MAX_SYNCING {
            self.sync_running.fetch_sub(1, Ordering::AcqRel);
            std::thread::sleep(Duration::from_millis(50));
        }
        let mut attempt = 0u32;
        loop {
            if let Some(again) = lock(&self.syncing).get_mut(session) {
                *again = false;
            }
            let done = self.sync_once(session);
            let again = {
                let mut syncing = lock(&self.syncing);
                let again = syncing.get(session).copied().unwrap_or(false);
                let give_up =
                    !done && (attempt + 1 >= SYNC_ATTEMPTS || self.sync_relay(session).is_none());
                if (done && !again) || give_up {
                    syncing.remove(session);
                    if give_up && self.sync_relay(session).is_some() {
                        // Tried enough for now: once more a few minutes on.
                        let (this, session) = (self.clone(), session.to_string());
                        std::thread::spawn(move || {
                            std::thread::sleep(SYNC_RETRY_LATER);
                            this.kick(&session);
                        });
                    }
                    break;
                }
                again
            };
            if done {
                // Asked again meanwhile: once more, at once.
                attempt = 0;
                continue;
            }
            let _ = again;
            attempt += 1;
            let base = *lock(&self.sync_backoff);
            std::thread::sleep((base * 2u32.saturating_pow(attempt - 1)).min(SYNC_BACKOFF_CAP));
        }
        self.sync_running.fetch_sub(1, Ordering::AcqRel);
    }

    /// One hand-back of `session`: its outbox, fully, then its streams.
    /// Whether it holds (everything relayed, nothing queued).
    fn sync_once(self: &Arc<Self>, session: &str) -> bool {
        let Some((conn, nonce)) = self.sync_relay(session) else {
            return true;
        };
        if !self.flush_outbox(session, &conn, &nonce) {
            return false;
        }
        let local: Vec<_> = lock(&self.streams)
            .values()
            .filter(|stream| {
                stream.is_agent_of(session) && matches!(*lock(&stream.link.route), Route::Local)
            })
            .map(|stream| (stream.link.clone(), stream.out.clone()))
            .collect();
        for (link, out) in &local {
            self.start_reattach(link.clone(), out.clone(), &conn, &nonce);
        }
        let deadline = Instant::now() + *lock(&self.reattach_deadline) + Duration::from_secs(1);
        while Instant::now() < deadline
            && local
                .iter()
                .any(|(link, _)| matches!(*lock(&link.route), Route::Reattaching { .. }))
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        let relayed = lock(&self.streams)
            .values()
            .filter(|stream| stream.is_agent_of(session))
            .all(|stream| matches!(*lock(&stream.link.route), Route::Desktop { .. }));
        let drained = lock(&self.outbox)
            .get(session)
            .is_none_or(|outbox| outbox.is_empty());
        if drained && !lock(&self.sessions).contains_key(session) {
            lock(&self.orphan_relay).remove(session);
        }
        relayed && drained
    }

    /// Hand `link`'s stream to `conn`: open it there and replay the agent's
    /// `initialize` under the machine's own id; the agent's lines wait until
    /// the desktop answers it (or are answered here if it does not).
    fn start_reattach(
        self: &Arc<Self>,
        link: Arc<Link>,
        out: mpsc::SyncSender<Out>,
        conn: &Conn,
        nonce: &str,
    ) {
        use crate::offline_mcp as offline;
        let Some(replay) = lock(&link.init)
            .as_deref()
            .and_then(|init| offline::with_id(init, serde_json::json!(offline::REATTACH_ID)))
        else {
            return;
        };
        let Ok(channel) = self.next_channel() else {
            return;
        };
        {
            let mut route = lock(&link.route);
            if !matches!(*route, Route::Local) {
                return;
            }
            *route = Route::Reattaching {
                conn: conn.clone(),
                channel,
                held: Vec::new(),
            };
        }
        let sent = send_open(conn, &link, channel, nonce)
            && send_line(conn, channel, &replay)
            && send_line(
                conn,
                channel,
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            );
        if !sent {
            let answers = self.go_local(&link, Some(channel), true);
            deliver(out, answers);
            return;
        }
        let weak = Arc::downgrade(self);
        let deadline = *lock(&self.reattach_deadline);
        std::thread::spawn(move || {
            std::thread::sleep(deadline);
            if let Some(this) = weak.upgrade() {
                let answers = this.go_local(&link, Some(channel), true);
                deliver(out, answers);
            }
        });
    }

    /// Deliver `session`'s outbox to `conn`, in order, one call at a time,
    /// each under its outbox id, on a stream of its own. Whether it is
    /// empty after. Stops (keeping the rest) when the desktop goes away; a
    /// call whose answer was lost is delivered again next time under the
    /// same id, which the desktop runs once. An ended session's calls the
    /// desktop will not take are recorded as not delivered.
    fn flush_outbox(self: &Arc<Self>, session: &str, conn: &Conn, nonce: &str) -> bool {
        let empty = |this: &Self| {
            lock(&this.outbox)
                .get(session)
                .is_none_or(|outbox| outbox.is_empty())
        };
        // One delivery at a time, so order holds.
        let deadline = Instant::now() + *lock(&self.flush_deadline) * 2;
        loop {
            {
                let mut outbox = lock(&self.outbox);
                match outbox.get_mut(session) {
                    None => return true,
                    Some(queued) if queued.is_empty() => return true,
                    Some(queued) if !queued.flushing => {
                        queued.flushing = true;
                        break;
                    }
                    Some(_) => {}
                }
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let result = self.deliver_outbox(session, conn, nonce);
        let mut outboxes = lock(&self.outbox);
        if let Some(outbox) = outboxes.get_mut(session) {
            outbox.flushing = false;
            match &result {
                // Only a desktop that named itself (the very one that relayed
                // the session) refusing is final; an older desktop, matched
                // by viewer, may not be it.
                Err(Stop::Refused(why)) if outbox.ended && lock(&self.named).contains(&conn.id) => {
                    let lost = outbox.drain();
                    self.outbox_store.record_lost(
                        session,
                        &lost,
                        &format!(
                            "the session ended and its desktop would not take its calls: {why}"
                        ),
                        outbox.desktop.as_deref(),
                    );
                }
                Err(Stop::Refused(why) | Stop::Gone(why)) => {
                    log::info!("session {session}: outbox delivery stopped: {why}");
                }
                Ok(()) => {}
            }
            self.outbox_store.keep(outbox);
            if outbox.is_empty() {
                outboxes.remove(session);
            }
        }
        drop(outboxes);
        empty(self)
    }

    fn deliver_outbox(
        self: &Arc<Self>,
        session: &str,
        conn: &Conn,
        nonce: &str,
    ) -> Result<(), Stop> {
        use crate::offline_mcp as offline;
        use std::io::BufRead;
        let io = |e: std::io::Error| Stop::Gone(e.to_string());
        let (mine, theirs) = UnixStream::pair().map_err(io)?;
        mine.set_read_timeout(Some(*lock(&self.flush_deadline)))
            .map_err(io)?;
        let this = self.clone();
        let (owner, relay, binding) = (session.to_string(), conn.clone(), nonce.to_string());
        let runner = std::thread::spawn(move || {
            this.run_stream(
                theirs,
                owner,
                wire::CODESENSE_SERVER.into(),
                Some((relay, binding)),
                true,
            )
        });
        let mut writer = mine.try_clone().map_err(io)?;
        let mut reader = std::io::BufReader::new(mine);
        let named = lock(&self.named).contains(&conn.id);
        let live = || {
            self.host
                .upgrade()
                .is_some_and(|host| host.reverse_connection_live(conn.id))
        };
        // An answer the machine gave for a desktop that is gone, or one
        // that refused the stream (it closed it while connected).
        let stopped = |why: &str| {
            if live() {
                Stop::Refused(why.into())
            } else {
                Stop::Gone(why.into())
            }
        };
        let send = |writer: &mut UnixStream, line: &str| {
            writer
                .write_all(format!("{}\n", line.trim_end()).as_bytes())
                .map_err(io)
        };
        let answer = |reader: &mut std::io::BufReader<UnixStream>,
                      writer: &mut UnixStream,
                      id: &serde_json::Value| loop {
            let mut answer = String::new();
            if reader.read_line(&mut answer).map_err(io)? == 0 {
                return Err(Stop::Gone("delivery stream ended".into()));
            }
            // The server's own requests: a ping is answered, anything else
            // refused (this stream offers nothing).
            if let Some(request) = offline::parse(&answer) {
                let reply = if request.method == "ping" {
                    serde_json::json!({"jsonrpc":"2.0","id":request.id,"result":{}})
                } else {
                    serde_json::json!({"jsonrpc":"2.0","id":request.id,"error":{"code":-32601,"message":"not offered"}})
                };
                send(writer, &reply.to_string())?;
                continue;
            }
            if offline::response_id(&answer).as_ref() == Some(id) {
                return Ok(answer);
            }
        };
        let result = (|| {
            let init_id = serde_json::json!(offline::FLUSH_INIT_ID);
            let init = serde_json::json!({
                "jsonrpc": "2.0",
                "id": init_id,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "codesense-outbox", "version": "1"},
                    "_meta": {"codesenseSessionId": session}
                }
            });
            send(&mut writer, &init.to_string())?;
            let hello = answer(&mut reader, &mut writer, &init_id)?;
            if offline::is_error(&hello) {
                return Err(stopped("the desktop refused the delivery"));
            }
            send(
                &mut writer,
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            )?;
            loop {
                let Some(next) = lock(&self.outbox)
                    .get(session)
                    .and_then(|queued| queued.front())
                else {
                    return Ok(());
                };
                let ask = |key: &str,
                           line: &str,
                           writer: &mut UnixStream,
                           reader: &mut std::io::BufReader<UnixStream>| {
                    let line = offline::delivery_line(line, key)
                        .ok_or_else(|| Stop::Refused("an unreadable queued call".into()))?;
                    send(writer, &line)?;
                    let id = serde_json::json!(format!("{}{key}", offline::FLUSH_ID_PREFIX));
                    let answered = answer(reader, writer, &id)?;
                    if offline::is_desktop_offline(&answered) {
                        return Err(stopped("the desktop went away"));
                    }
                    Ok(answered)
                };
                let desktop = lock(&self.outbox)
                    .get(session)
                    .and_then(|queued| queued.desktop.clone());
                let answered = match ask(&next.key, &next.line, &mut writer, &mut reader) {
                    Ok(answered) => answered,
                    // An older desktop runs a redelivered id again: a call
                    // cut after it was sent may have run, so it is not
                    // delivered twice.
                    Err(Stop::Gone(why) | Stop::Refused(why)) if !named => {
                        self.outbox_store.record_lost(
                            session,
                            std::slice::from_ref(&next),
                            &format!("may have run: the delivery was cut ({why}) and an older desktop cannot take it again safely"),
                            desktop.as_deref(),
                        );
                        if let Some(queued) = lock(&self.outbox).get_mut(session) {
                            queued.delivered(&next.key);
                            self.outbox_store.keep(queued);
                        }
                        return Err(Stop::Gone(why));
                    }
                    Err(stop) => return Err(stop),
                };
                if offline::retry_later(&answered) {
                    return Err(Stop::Gone("the desktop asked for it again later".into()));
                }
                if offline::refused(&answered) {
                    // Refused on delivery: the person hears it, and the
                    // call is recorded as not delivered.
                    self.outbox_store.record_lost(
                        session,
                        std::slice::from_ref(&next),
                        &format!("refused on delivery: {}", answered.trim()),
                        desktop.as_deref(),
                    );
                    let is_note = offline::parse(&next.line)
                        .is_some_and(|call| call.tool.as_deref() == Some("inbox_post"));
                    let note_key = format!("{}-refused", next.key);
                    if let (false, Some(note)) =
                        (is_note, offline::refusal_note(&next, &answered, &note_key))
                    {
                        ask(&note_key, &note, &mut writer, &mut reader)?;
                    }
                }
                if let Some(queued) = lock(&self.outbox).get_mut(session) {
                    queued.delivered(&next.key);
                    self.outbox_store.keep(queued);
                }
            }
        })();
        let _ = reader.get_ref().shutdown(std::net::Shutdown::Both);
        let _ = runner.join();
        result
    }
}

impl Drop for ReverseMcp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn resolve_remote_entries(
    entries: &[wire::RemoteStdioEntry],
    spec: &PtySpec,
) -> Result<HashMap<String, crate::owned_supervisors::ResolvedServer>, String> {
    use std::os::unix::fs::MetadataExt;
    let mut resolved = HashMap::new();
    if entries.is_empty() {
        return Ok(resolved);
    }
    crate::owned_supervisors::pidfd(std::process::id())?;
    let remote: std::collections::BTreeMap<String, String> = spec
        .command
        .iter_full_env_as_str()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    let path = remote.get("PATH").ok_or("remote MCP PATH unavailable")?;
    let default_cwd = spec
        .command
        .get_cwd()
        .ok_or("remote MCP worktree unavailable")?;
    for entry in entries {
        entry.validate()?;
        let requested = Path::new(&entry.command);
        let command = if requested.is_absolute() {
            requested.to_path_buf()
        } else if requested.components().count() == 1 {
            std::env::split_paths(path).filter(|dir| dir.is_absolute()).map(|dir| dir.join(requested))
                .find(|file| codesense_pty_core::shell::is_executable_file(file))
                .ok_or("selected remote MCP executable missing; install it explicitly on the remote account")?
        } else {
            return Err("remote MCP executable must be absolute or a remote PATH name".into());
        };
        if !codesense_pty_core::shell::is_executable_file(&command) {
            return Err("selected remote MCP executable missing or not executable".into());
        }
        let metadata =
            std::fs::metadata(&command).map_err(|_| "remote MCP executable unavailable")?;
        if metadata.mode() & 0o6000 != 0 {
            return Err("privilege-changing remote MCP executable unsupported".into());
        }
        let cwd = entry
            .cwd
            .as_deref()
            .map(Path::new)
            .unwrap_or_else(|| Path::new(default_cwd));
        if !cwd.is_absolute() || !cwd.is_dir() {
            return Err("remote MCP cwd must be an existing remote absolute directory".into());
        }
        let mut env = std::collections::BTreeMap::new();
        for name in [
            "HOME", "PATH", "TMPDIR", "LANG", "LC_ALL", "USER", "LOGNAME",
        ] {
            if let Some(value) = remote.get(name) {
                env.insert(name.into(), value.clone());
            }
        }
        env.extend(entry.env.clone());
        for (target, source) in &entry.env_refs {
            let value = remote
                .get(source)
                .filter(|value| !value.is_empty())
                .ok_or("required remote MCP environment variable unavailable (value redacted)")?;
            env.insert(target.clone(), value.clone());
        }
        if env
            .values()
            .any(|value| value.len() > 16 * 1024 || value.contains('\0'))
        {
            return Err("remote MCP resolved environment exceeds limit (values redacted)".into());
        }
        // Session broker descriptors and capabilities never enter server configuration.
        if env.keys().any(|key| key.starts_with("CODESENSE_MCP_")) {
            return Err("reserved remote MCP environment setting".into());
        }
        resolved.insert(
            entry.id.clone(),
            crate::owned_supervisors::ResolvedServer {
                command: command.to_str().ok_or("invalid remote executable")?.into(),
                args: entry.args.clone(),
                cwd: cwd.to_str().ok_or("invalid remote cwd")?.into(),
                env,
            },
        );
    }
    for config in resolved.values() {
        if serde_json::to_vec(config)
            .map_err(|_| "remote MCP resolved configuration invalid")?
            .len()
            > crate::owned_supervisors::MAX_CONFIG_BYTES
        {
            return Err("remote MCP resolved configuration exceeds limit (values redacted)".into());
        }
    }
    Ok(resolved)
}

/// Accepts only a broker path and an opaque captured entry identity. No command,
/// arguments, cwd or environment supplied by the native client are accepted.
pub fn stdio_supervisor(args: &[std::ffi::OsString]) -> Result<(), String> {
    let [path, selected] = args else {
        return Err("invalid remote MCP supervisor arguments".into());
    };
    let selected = selected.to_str().ok_or("invalid remote MCP selection")?;
    wire::validate_server_id(selected)?;
    crate::owned_supervisors::initialize_supervisor()?;
    // Some clients preserve the bootstrap capability, others close it. Never
    // allow a preserved descriptor to leak onward into the selected program.
    if let Ok(value) = std::env::var(wire::BROKER_ENV) {
        if let Ok(fd) = value.parse::<i32>() {
            if fd > 2 {
                let _ = broker::set_inheritable(fd, false);
            }
        }
    }
    let mut socket =
        UnixStream::connect(Path::new(path)).map_err(|_| "remote MCP registration unavailable")?;
    let deadline = Instant::now() + DEADLINE;
    auth_timeout(&socket, deadline)?;
    wire::write_mode(&mut socket, wire::MODE_REACQUIRE)
        .map_err(|_| "remote MCP registration unavailable")?;
    let capability = broker::receive_fd(&socket).map_err(|_| "remote MCP authority unavailable")?;
    wire::write_mode(&mut socket, MODE_SUPERVISOR)
        .map_err(|_| "remote MCP registration unavailable")?;
    broker::send_fd(&socket, capability.as_raw_fd())
        .map_err(|_| "remote MCP authority unavailable")?;
    drop(capability);
    let challenge = wire::read_message(&mut socket, 128, |socket| auth_timeout(socket, deadline))?;
    if !challenge.starts_with(&format!("{} ", wire::AUDIENCE)) {
        return Err("remote MCP broker audience mismatch".into());
    }
    socket
        .write_all(challenge.as_bytes())
        .map_err(|_| "remote MCP challenge failed")?;
    wire::write_server_selector(&mut socket, selected)?;
    let client =
        broker::receive_fd(&socket).map_err(|_| "remote MCP lifetime registration refused")?;
    let mut size = [0; 4];
    socket
        .read_exact(&mut size)
        .map_err(|_| "remote MCP registration acknowledgement unavailable")?;
    let size = u32::from_be_bytes(size) as usize;
    if size > crate::owned_supervisors::MAX_CONFIG_BYTES {
        return Err("remote MCP configuration exceeds limit".into());
    }
    let mut bytes = vec![0; size];
    socket
        .read_exact(&mut bytes)
        .map_err(|_| "remote MCP registration acknowledgement unavailable")?;
    let config =
        serde_json::from_slice(&bytes).map_err(|_| "remote MCP captured configuration invalid")?;
    crate::owned_supervisors::supervise(socket, client, config)
}

pub fn bootstrap(args: &[std::ffi::OsString]) -> Result<(), String> {
    let (path, argv) = args.split_first().ok_or("missing broker socket")?;
    let (config, argv) = argv.split_first().ok_or("missing session MCP projection")?;
    let (program, arguments) = argv.split_first().ok_or("missing original command")?;
    let mut stream = UnixStream::connect(Path::new(path)).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(DEADLINE))
        .map_err(|e| e.to_string())?;
    wire::write_mode(&mut stream, wire::MODE_BOOTSTRAP).map_err(|e| e.to_string())?;
    let fd = broker::receive_fd(&stream).map_err(|e| e.to_string())?;
    broker::set_inheritable(fd.as_raw_fd(), true).map_err(|e| e.to_string())?;
    let installed = std::env::current_exe().map_err(|e| e.to_string())?;
    let bin = installed
        .parent()
        .ok_or("installed bridge directory unavailable")?;
    let path_value = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let error = std::process::Command::new(program)
        .args(arguments)
        .env("PATH", path_value)
        .env("CODESENSE_MCP_CONFIG", config)
        .env(
            "CODESENSE_MCP_BRIDGE",
            installed.with_file_name(crate::install::BRIDGE),
        )
        .env(wire::SOCKET_ENV, path)
        .env(wire::BROKER_ENV, fd.as_raw_fd().to_string())
        .exec();
    Err(format!("remote process exec failed: {error}"))
}
fn peer_credentials(stream: &UnixStream) -> Result<(u32, u32), String> {
    let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of_val(&peer) as libc::socklen_t;
    // SAFETY: SO_PEERCRED fills the bounded kernel credential structure.
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut peer as *mut libc::ucred).cast(),
            &mut len,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok((peer.pid as u32, peer.uid))
}
fn process_identity(pid: u32) -> Result<(u32, u64), String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|e| e.to_string())?;
    let rest = stat.rsplit_once(") ").ok_or("invalid process identity")?.1;
    let fields: Vec<_> = rest.split_whitespace().collect();
    let parent = fields
        .get(1)
        .ok_or("invalid parent identity")?
        .parse()
        .map_err(|_| "invalid parent")?;
    let start = fields
        .get(19)
        .ok_or("invalid process identity")?
        .parse()
        .map_err(|_| "invalid process start")?;
    Ok((parent, start))
}
fn process_start(pid: u32) -> Result<u64, String> {
    Ok(process_identity(pid)?.1)
}
fn process_chain(mut pid: u32) -> Result<Vec<(u32, u64, u32)>, String> {
    let mut chain = Vec::new();
    for _ in 0..64 {
        let (parent, start) = process_identity(pid)?;
        chain.push((pid, start, parent));
        if parent <= 1 {
            return Ok(chain);
        }
        if parent == pid {
            return Err("invalid process ancestry".into());
        }
        pid = parent;
    }
    Err("process ancestry exceeds bound".into())
}
fn random_token() -> Result<String, String> {
    let mut bytes = [0; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| e.to_string())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn auth_timeout(stream: &UnixStream, deadline: Instant) -> Result<(), String> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or("reverse authentication deadline expired")?;
    stream
        .set_read_timeout(Some(remaining))
        .and_then(|_| stream.set_write_timeout(Some(remaining)))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesense_pty_core::agent_cli::AgentCli;
    use codesense_pty_core::domain::TerminalDomainEvent;
    fn remote_entry() -> wire::RemoteStdioEntry {
        wire::RemoteStdioEntry {
            id: wire::remote_server_id("marker").unwrap(),
            logical_name: "marker".into(),
            command: "/bin/sh".into(),
            args: vec![],
            cwd: None,
            env: Default::default(),
            env_refs: Default::default(),
        }
    }
    #[test]
    fn old_preflight_cannot_release_or_retarget_replacement_incarnation() {
        use codesense_pty_core::layout::ServerIdentity;
        let (root, spec, _) = crate::native_launch::tests::fixture(
            NativeLaunch::Cli(AgentCli::Claude),
            "--mcp-config --strict-mcp-config",
        );
        let script = format!("#!/bin/sh\ntouch '{0}/preflight'; while [ ! -e '{0}/resume' ]; do /bin/sleep 0.01; done; printf '%s' '--mcp-config --strict-mcp-config'\n", root.path().display());
        // The controlled help probe blocks until the old grant is replaced.
        std::fs::write(
            root.path().join("bin/claude"),
            script.replace("touch ", "/bin/touch "),
        )
        .unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = Host::new(
            "preflight-race".into(),
            ServerIdentity::ephemeral("preflight-race"),
            crate::testing::host_environment(),
            runtime.handle().clone(),
        );
        let reverse = ReverseMcp::start(&host, root.path()).unwrap();
        let (socket, _peer) = UnixStream::pair().unwrap();
        let (tx, _rx) = mpsc::sync_channel(8);
        let conn = Conn::new(71, tx, socket);
        let session = PtySessionId::new("preflight-race");
        reverse
            .provision(
                &conn,
                &session,
                "a".repeat(64),
                vec![],
                vec![remote_entry()],
                NativeLaunch::Cli(AgentCli::Claude),
            )
            .unwrap();
        let running = reverse.clone();
        let id = session.as_str().to_owned();
        let old = std::thread::spawn(move || {
            running.prepare(71, &id, spec, Some(NativeLaunch::Cli(AgentCli::Claude)))
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while !root.path().join("preflight").exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        reverse.release(conn.id, session.as_str());
        let mut replacement = remote_entry();
        replacement
            .env
            .insert("MARKER".into(), "new incarnation".into());
        reverse
            .provision(
                &conn,
                &session,
                "b".repeat(64),
                vec![],
                vec![replacement],
                NativeLaunch::Cli(AgentCli::Claude),
            )
            .unwrap();
        reverse.retire_captured(session.as_str(), "a".repeat(64));
        assert!(
            !host
                .owned_supervisors
                .settled(session.as_str(), &"b".repeat(64)),
            "stale retirement closed the new registration gate"
        );
        std::fs::write(root.path().join("resume"), b"go").unwrap();
        assert!(old.join().unwrap().is_err());
        assert_eq!(
            lock(&reverse.sessions)[session.as_str()].nonce,
            "b".repeat(64)
        );
        let (_next_root, next_spec, _) = crate::native_launch::tests::fixture(
            NativeLaunch::Cli(AgentCli::Claude),
            "--mcp-config --strict-mcp-config",
        );
        reverse
            .prepare(
                conn.id,
                session.as_str(),
                next_spec,
                Some(NativeLaunch::Cli(AgentCli::Claude)),
            )
            .unwrap();
        assert_eq!(
            lock(&reverse.sessions)[session.as_str()]
                .resolved
                .values()
                .next()
                .unwrap()
                .env["MARKER"],
            "new incarnation"
        );
        reverse.release(conn.id, session.as_str());
    }

    #[test]
    fn remote_preflight_resolves_only_remote_values_and_refuses_missing_dependencies() {
        let (_root, mut spec, _) = crate::native_launch::tests::fixture(
            NativeLaunch::Cli(AgentCli::Claude),
            "--mcp-config --strict-mcp-config",
        );
        let mut entry = remote_entry();
        entry
            .env_refs
            .insert("MARKER".into(), "REMOTE_ONLY_MARKER".into());
        assert!(resolve_remote_entries(&[entry.clone()], &spec)
            .err()
            .unwrap()
            .contains("value redacted"));
        spec.command.env("REMOTE_ONLY_MARKER", "remote sentinel");
        spec.command
            .env("REMOTE_ONLY_MARKER", "x".repeat(17 * 1024));
        let error = resolve_remote_entries(&[entry.clone()], &spec)
            .err()
            .unwrap();
        assert!(error.contains("redacted") && error.len() < 100);
        spec.command.env("REMOTE_ONLY_MARKER", "remote sentinel");
        let resolved = resolve_remote_entries(&[entry.clone()], &spec).unwrap();
        assert_eq!(resolved[&entry.id].env["MARKER"], "remote sentinel");
        assert!(!resolved[&entry.id]
            .env
            .contains_key("CODESENSE_MCP_BROKER_FD"));
        entry.command = "/missing-controlled-executable".into();
        assert!(resolve_remote_entries(&[entry.clone()], &spec).is_err());
        entry.command = "/bin/sh".into();
        entry.cwd = Some("/missing-controlled-directory".into());
        assert!(resolve_remote_entries(&[entry.clone()], &spec).is_err());
        entry.cwd = None;
        assert!(resolve_remote_entries(&[entry], &spec).is_ok());
    }
    #[test]
    fn unrelated_peer_cannot_reacquire_and_failed_remote_preflight_removes_private_projection() {
        use codesense_pty_core::layout::ServerIdentity;
        let (root, spec, _) = crate::native_launch::tests::fixture(
            NativeLaunch::Cli(AgentCli::Claude),
            "--mcp-config --strict-mcp-config",
        );
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = Host::new(
            "remote-test".into(),
            ServerIdentity::ephemeral("remote-test"),
            crate::testing::host_environment(),
            runtime.handle().clone(),
        );
        let reverse = ReverseMcp::start(&host, root.path()).unwrap();
        let (socket, _peer) = UnixStream::pair().unwrap();
        let (tx, _rx) = mpsc::sync_channel(8);
        let conn = Conn::new(71, tx, socket);
        let session = PtySessionId::new("remote-fixture");
        let mut entry = remote_entry();
        entry
            .env_refs
            .insert("REQUIRED".into(), "MISSING_REMOTE_FIXTURE".into());
        reverse
            .provision(
                &conn,
                &session,
                "c".repeat(64),
                vec![],
                vec![entry],
                NativeLaunch::Cli(AgentCli::Claude),
            )
            .unwrap();
        let config = root
            .path()
            .join(format!("mcp-session-{}.json", "c".repeat(64)));
        let projection = std::fs::read_to_string(&config).unwrap();
        assert!(projection.contains("--mcp-stdio-supervisor"));
        assert!(!projection.contains("MISSING_REMOTE_FIXTURE"));
        let mut peer = UnixStream::connect(&reverse.path).unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        wire::write_mode(&mut peer, wire::MODE_REACQUIRE).unwrap();
        assert!(broker::receive_fd(&peer).is_err());
        reverse.release(999, session.as_str());
        assert!(config.exists(), "foreign release changed session ownership");
        assert!(reverse
            .prepare(
                conn.id,
                session.as_str(),
                spec,
                Some(NativeLaunch::Cli(AgentCli::Claude))
            )
            .is_err());
        assert!(!config.exists());
        reverse
            .provision(
                &conn,
                &session,
                "d".repeat(64),
                vec![],
                vec![remote_entry()],
                NativeLaunch::Cli(AgentCli::Claude),
            )
            .unwrap();
        lock(&reverse.sessions)
            .get_mut(session.as_str())
            .unwrap()
            .created = Instant::now() - Duration::from_secs(31);
        reverse
            .provision(
                &conn,
                &session,
                "e".repeat(64),
                vec![],
                vec![remote_entry()],
                NativeLaunch::Cli(AgentCli::Claude),
            )
            .unwrap();
        reverse.release(conn.id, session.as_str());
    }
    #[test]
    fn a_shell_tab_names_the_codex_server_and_release_removes_its_configuration() {
        use codesense_pty_core::layout::ServerIdentity;
        let root = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = Host::new(
            "shell-test".into(),
            ServerIdentity::ephemeral("shell-test"),
            crate::testing::host_environment(),
            runtime.handle().clone(),
        );
        let reverse = ReverseMcp::start(&host, root.path()).unwrap();
        let (socket, _peer) = UnixStream::pair().unwrap();
        let (tx, _rx) = mpsc::sync_channel(8);
        let conn = Conn::new(71, tx, socket);
        let session = PtySessionId::new("shell-tab-session");
        reverse
            .provision(
                &conn,
                &session,
                "f".repeat(64),
                vec![],
                vec![],
                NativeLaunch::Shell,
            )
            .unwrap();
        let config = root
            .path()
            .join(format!("mcp-session-{}.json", "f".repeat(64)));
        let settings = config.with_extension("settings.json");
        let prepared = reverse
            .prepare(
                conn.id,
                session.as_str(),
                crate::native_launch::tests::shell_spec(root.path()),
                None,
            )
            .unwrap();
        assert_eq!(prepared.command.get_argv()[1], "--mcp-session-bootstrap");
        assert!(prepared
            .command
            .get_env(crate::native_launch::CODEX_MCP_ENV)
            .is_some());
        // A typed Claude's hooks come with the host's observer: nothing is written.
        assert!(!settings.exists());
        // A shell that never started: its configuration goes with it.
        reverse.release(conn.id, session.as_str());
        assert!(!config.exists());
    }
    #[test]
    fn retired_native_provision_refuses_and_current_matching_provision_builds_native_argv() {
        use codesense_pty_core::layout::ServerIdentity;
        for launch in [
            NativeLaunch::Cli(AgentCli::Claude),
            NativeLaunch::Cli(AgentCli::Codex),
        ] {
            let (root, spec, _) = crate::native_launch::tests::fixture(
                launch,
                "--mcp-config --strict-mcp-config --no-daemon --config",
            );
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let host = Host::new(
                "native-test".into(),
                ServerIdentity::ephemeral("native-test"),
                crate::testing::host_environment(),
                runtime.handle().clone(),
            );
            let reverse = ReverseMcp::start(&host, root.path()).unwrap();
            let (socket, _peer) = UnixStream::pair().unwrap();
            let (tx, _rx) = mpsc::sync_channel(8);
            let conn = Conn::new(71, tx, socket);
            let session = PtySessionId::new("native-test-session");
            reverse
                .provision(
                    &conn,
                    &session,
                    "a".repeat(64),
                    vec!["desktop sentinel".into()],
                    vec![],
                    launch,
                )
                .unwrap();
            reverse.release(conn.id, session.as_str());
            let error = reverse
                .prepare(conn.id, session.as_str(), spec, Some(launch))
                .err()
                .unwrap();
            assert!(error.contains("live session MCP provisioning"));
            assert!(!root
                .path()
                .join(format!("mcp-session-{}.json", "a".repeat(64)))
                .exists());
            reverse
                .provision(
                    &conn,
                    &session,
                    "b".repeat(64),
                    vec!["desktop sentinel".into()],
                    vec![],
                    launch,
                )
                .unwrap();
            let (fixture_root, spec, _) = crate::native_launch::tests::fixture(
                launch,
                "--mcp-config --strict-mcp-config --no-daemon --config",
            );
            let prepared = reverse
                .prepare(conn.id, session.as_str(), spec, Some(launch))
                .unwrap();
            let argv = prepared.command.get_argv();
            assert_eq!(argv[1], "--mcp-session-bootstrap");
            assert_eq!(
                Path::new(&argv[4]),
                fixture_root
                    .path()
                    .join("bin")
                    .join(&codesense_pty_core::agent_cli::descriptor(launch.cli().unwrap()).binary)
            );
            assert_eq!(
                prepared.command.get_cwd().unwrap(),
                fixture_root.path().as_os_str()
            );
            reverse.release(conn.id, session.as_str());
            assert!(!root
                .path()
                .join(format!("mcp-session-{}.json", "b".repeat(64)))
                .exists());
        }
    }

    // An agent counts as having its tools once it listed them since the relay began, or
    // after a settle with no listing unanswered, or after a cap with one.
    #[test]
    fn an_unanswered_listing_holds_the_tools_back_until_the_cap() {
        let (out, _frames) = mpsc::sync_channel(1);
        let (socket, _peer) = UnixStream::pair().unwrap();
        let conn = Conn::new(1, out, socket);
        let link = Link {
            session: "s".into(),
            server_id: wire::CODESENSE_SERVER.into(),
            route: Mutex::new(Route::Local),
            pending: Mutex::default(),
            init: Mutex::default(),
            flusher: false,
        };
        let relayed = |ago: Duration, listed: bool| {
            *lock(&link.route) = Route::Desktop {
                conn: conn.clone(),
                channel: 1,
                since: Instant::now().checked_sub(ago).unwrap(),
                listed,
            };
        };
        assert!(!link.has_tools(), "the machine answers it");
        relayed(Duration::ZERO, false);
        assert!(!link.has_tools(), "relayed, not listed yet");
        lock(&link.pending).push(("1".into(), "tools/call".into()));
        lock(&link.pending).push(("2".into(), LISTING.into()));
        relayed(TOOLS_SETTLE + Duration::from_secs(1), false);
        assert!(!link.has_tools(), "its listing is still unanswered");
        relayed(TOOLS_CAP + Duration::from_secs(1), false);
        assert!(link.has_tools(), "the cap");
        lock(&link.pending).retain(|(key, _)| key != "2");
        relayed(Duration::ZERO, true);
        assert!(link.has_tools(), "listed");
        relayed(TOOLS_SETTLE + Duration::from_secs(1), false);
        assert!(link.has_tools(), "settled with no listing unanswered");
        lock(&link.pending).push(("3".into(), "initialize".into()));
        relayed(TOOLS_SETTLE + Duration::from_secs(1), false);
        assert!(!link.has_tools(), "its initialize is still unanswered");
    }

    // A remote agent keeps its CodeSense MCP across desktop
    // disconnects: the stream stays up, a call in flight is answered once
    // (typed, never replayed), the machine serves the session's timers and
    // queues its outbound messages, and a returning desktop gets the stream
    // back (outbox first, in order, each under its id) without restarting
    // the agent.
    mod reattach {
        use super::*;
        use crate::host::QueuedFrame;
        use crate::offline_mcp::{FLUSH_INIT_ID, OUTBOX_META, REATTACH_ID};
        use codesense_pty_core::domain::TerminalDomain;
        use codesense_pty_core::layout::ServerIdentity;
        use codesense_pty_core::protocol::{Inbound, Ingress, Reply, Request};
        use serde_json::{json, Value};
        use std::io::BufRead;

        const VIEWER: &str = "desktop-app";
        const SECRET: &str = "5ec2e75ec2e75ec2e75ec2e75ec2e75ec2e75ec2e75ec2e75ec2e75ec2e75ec2";

        struct Rig {
            _runtime: tokio::runtime::Runtime,
            root: tempfile::TempDir,
            host: Arc<Host>,
            reverse: Arc<ReverseMcp>,
            session: PtySessionId,
        }

        struct Desktop {
            conn: Conn,
            frames: mpsc::Receiver<QueuedFrame>,
            _peer: UnixStream,
        }

        impl Desktop {
            fn new(host: &Arc<Host>, id: u64) -> Self {
                let (socket, peer) = UnixStream::pair().unwrap();
                let (out, frames) = mpsc::sync_channel(512);
                let conn = Conn::new(id, out, socket);
                host.connect_as(&conn, Some(VIEWER.into()));
                Self {
                    conn,
                    frames,
                    _peer: peer,
                }
            }

            /// The next reverse-MCP frame this desktop is sent.
            #[track_caller]
            fn next(&self) -> Frame {
                let deadline = Instant::now() + crate::testing::WAIT;
                loop {
                    let left = deadline.saturating_duration_since(Instant::now());
                    let frame = self.frames.recv_timeout(left).expect("a reverse MCP frame");
                    if matches!(
                        frame.kind,
                        Kind::MCP_OPEN | Kind::MCP_DATA | Kind::MCP_CLOSE
                    ) {
                        return (*frame).clone();
                    }
                }
            }

            /// No reverse-MCP frame for a while.
            fn quiet(&self, wait: Duration) -> Option<Frame> {
                let deadline = Instant::now() + wait;
                while let Some(left) = deadline.checked_duration_since(Instant::now()) {
                    match self.frames.recv_timeout(left) {
                        Ok(frame)
                            if matches!(
                                frame.kind,
                                Kind::MCP_OPEN | Kind::MCP_DATA | Kind::MCP_CLOSE
                            ) =>
                        {
                            return Some((*frame).clone())
                        }
                        Ok(_) => {}
                        Err(_) => return None,
                    }
                }
                None
            }

            /// The next JSON-RPC line on `channel` (data frames reassembled).
            #[track_caller]
            fn line(&self, channel: u32) -> Value {
                let mut bytes = Vec::new();
                loop {
                    let frame = self.next();
                    assert_eq!(frame.channel, channel, "{frame:?}");
                    assert_eq!(frame.kind, Kind::MCP_DATA, "{frame:?}");
                    bytes.extend_from_slice(&frame.payload);
                    if bytes.ends_with(b"\n") {
                        return serde_json::from_slice(&bytes).unwrap();
                    }
                }
            }

            #[track_caller]
            fn open(&self) -> (u32, wire::Open) {
                let frame = self.next();
                assert_eq!(frame.kind, Kind::MCP_OPEN, "{frame:?}");
                (frame.channel, frame.decode_json().unwrap())
            }

            /// Give this desktop's routine secret, as an app does on connect.
            #[track_caller]
            fn bind(&self, host: &Arc<Host>, identity: Option<&str>) {
                host.handle(
                    &self.conn,
                    5,
                    Request::RoutineArmBindSecret {
                        secret: SECRET.into(),
                        desktop: identity.map(str::to_string),
                    },
                );
                assert_eq!(reply_to(&self.frames, 5), Ok(Reply::Done));
            }

            /// Take a whole outbox delivery, answering its initialize and
            /// its `count` calls: the calls, in order.
            #[track_caller]
            fn take_outbox(&self, host: &Arc<Host>, count: usize) -> Vec<Value> {
                let (channel, _) = self.open();
                assert_eq!(self.line(channel)["id"], FLUSH_INIT_ID);
                self.answer(
                    host,
                    channel,
                    json!({"jsonrpc":"2.0","id":FLUSH_INIT_ID,"result":{}}),
                );
                assert_eq!(self.line(channel)["method"], "notifications/initialized");
                let calls: Vec<Value> = (0..count)
                    .map(|_| {
                        let call = self.line(channel);
                        self.answer(
                            host,
                            channel,
                            json!({"jsonrpc":"2.0","id":call["id"],"result":{"content":[]}}),
                        );
                        call
                    })
                    .collect();
                let end = self.next();
                assert_eq!(
                    (end.kind, end.channel),
                    (Kind::MCP_CLOSE, channel),
                    "{end:?}"
                );
                calls
            }

            /// Take an agent's stream back, answering its replayed
            /// initialize: its channel.
            #[track_caller]
            fn take_stream(&self, host: &Arc<Host>) -> u32 {
                let (channel, _) = self.open();
                assert_eq!(self.line(channel)["id"], REATTACH_ID);
                assert_eq!(self.line(channel)["method"], "notifications/initialized");
                self.answer(
                    host,
                    channel,
                    json!({"jsonrpc":"2.0","id":REATTACH_ID,"result":{}}),
                );
                channel
            }

            fn answer(&self, host: &Arc<Host>, channel: u32, answer: Value) {
                host.reverse_frame(
                    &self.conn,
                    Frame {
                        kind: Kind::MCP_DATA,
                        channel,
                        payload: format!("{answer}\n").into_bytes(),
                    },
                );
            }
        }

        fn reply_to(frames: &mpsc::Receiver<QueuedFrame>, rid: u64) -> Result<Reply, String> {
            let mut ingress = Ingress::default();
            loop {
                let frame = frames.recv_timeout(crate::testing::WAIT).expect("reply");
                if let Some(Inbound::Reply { rid: got, result }) = ingress.accept(&frame).unwrap() {
                    if got == rid {
                        return result;
                    }
                }
            }
        }

        /// A host with reverse MCP and session wakes, a live session bound
        /// to `desktop`'s connection, as a desktop launch binds it.
        fn rig(
            desktop: &Desktop,
            host: Arc<Host>,
            runtime: tokio::runtime::Runtime,
            root: tempfile::TempDir,
        ) -> Rig {
            host.start_reverse_mcp(root.path()).unwrap();
            host.start_wake(root.path()).unwrap();
            let reverse = lock(&host.reverse).clone().unwrap();
            let session = PtySessionId::new(format!("agr11-{}", &random_token().unwrap()[..8]));
            host.handle(
                &desktop.conn,
                1,
                Request::Spawn {
                    intent: crate::testing::sh("cat"),
                    registration: crate::testing::registration(session.as_str(), None, false),
                },
            );
            let pid = match reply_to(&desktop.frames, 1) {
                Ok(Reply::Spawned { pid: Some(pid) }) => pid,
                other => panic!("spawn failed: {other:?}"),
            };
            reverse
                .provision(
                    &desktop.conn,
                    &session,
                    "a".repeat(64),
                    vec![],
                    vec![],
                    NativeLaunch::Shell,
                )
                .unwrap();
            reverse
                .commit(desktop.conn.id, session.as_str(), pid)
                .unwrap();
            Rig {
                _runtime: runtime,
                root,
                host,
                reverse,
                session,
            }
        }

        fn host() -> (Arc<Host>, tokio::runtime::Runtime, tempfile::TempDir) {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let host = Host::new(
                "agr11".into(),
                ServerIdentity::ephemeral("agr11"),
                crate::testing::host_environment(),
                runtime.handle().clone(),
            );
            host.terminal.attach_echo_emulator();
            (host, runtime, tempfile::tempdir_in("/tmp").unwrap())
        }

        /// The agent end of a CodeSense MCP stream the machine serves.
        struct Agent {
            write: UnixStream,
            read: std::io::BufReader<UnixStream>,
            served: std::thread::JoinHandle<Result<(), String>>,
            notes: Vec<Value>,
        }

        impl Agent {
            fn start(rig: &Rig) -> Self {
                let (agent, daemon) = UnixStream::pair().unwrap();
                let relay = rig
                    .reverse
                    .relay_for(rig.session.as_str(), wire::CODESENSE_SERVER);
                let reverse = rig.reverse.clone();
                let session = rig.session.as_str().to_string();
                let served = std::thread::spawn(move || {
                    reverse.run_stream(daemon, session, wire::CODESENSE_SERVER.into(), relay, false)
                });
                agent.set_read_timeout(Some(crate::testing::WAIT)).unwrap();
                Self {
                    write: agent.try_clone().unwrap(),
                    read: std::io::BufReader::new(agent),
                    served,
                    notes: Vec::new(),
                }
            }

            fn send(&mut self, line: Value) {
                self.write
                    .write_all(format!("{line}\n").as_bytes())
                    .unwrap();
            }

            fn call(&mut self, id: u64, tool: &str, args: Value) {
                self.send(json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":tool,"arguments":args}}));
            }

            /// The next answer (notifications are kept aside).
            #[track_caller]
            fn recv(&mut self) -> Value {
                loop {
                    let mut line = String::new();
                    assert!(
                        self.read.read_line(&mut line).unwrap() > 0,
                        "the stream closed"
                    );
                    let message: Value = serde_json::from_str(&line).unwrap();
                    if message.get("id").is_none() {
                        self.notes.push(message);
                        continue;
                    }
                    return message;
                }
            }

            /// How many tool-list changes the agent was told of.
            fn list_changes(&self) -> usize {
                self.notes
                    .iter()
                    .filter(|note| note["method"] == "notifications/tools/list_changed")
                    .count()
            }

            fn alive(&self) -> bool {
                !self.served.is_finished()
            }
        }

        fn init(id: u64) -> Value {
            json!({"jsonrpc":"2.0","id":id,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"agent","version":"1"},"_meta":{"codesenseSessionId":"tab"}}})
        }

        fn text(answer: &Value) -> Value {
            serde_json::from_str(answer["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
        }

        // An inbox notice waits until the agent can read it: its tools are the desktop's
        // only once a desktop relays its stream and it listed them again.
        #[test]
        fn the_agent_has_its_tools_only_once_it_listed_the_returning_desktops() {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig(&first, host, runtime, root);
            let (host, session) = (&rig.host, rig.session.as_str());
            let tools = || rig.reverse.has_tools(session);
            let has_tools = || tools() == Some(true);
            assert_eq!(tools(), None, "no CodeSense stream yet");
            let mut agent = Agent::start(&rig);
            let (channel, _) = first.open();
            assert_eq!(tools(), Some(false), "starting up through a desktop");
            agent.send(init(0));
            assert_eq!(first.line(channel)["method"], "initialize");
            first.answer(host, channel, json!({"jsonrpc":"2.0","id":0,"result":{}}));
            agent.recv();
            assert!(!has_tools(), "relayed, but its tools are not listed yet");
            agent.send(json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"who_am_i","arguments":{}}}));
            assert_eq!(first.line(channel)["method"], "tools/call");
            first.answer(host, channel, json!({"jsonrpc":"2.0","id":9,"result":{}}));
            agent.recv();
            assert!(!has_tools(), "another request's answer");
            let list = |agent: &mut Agent, desktop: &Desktop, channel: u32, id: u64| {
                agent.send(json!({"jsonrpc":"2.0","id":id,"method":"tools/list"}));
                assert_eq!(desktop.line(channel)["method"], "tools/list");
                desktop.answer(
                    host,
                    channel,
                    json!({"jsonrpc":"2.0","id":id,"result":{"tools":[]}}),
                );
                agent.recv();
            };
            list(&mut agent, &first, channel, 1);
            assert!(has_tools());

            host.disconnect(&first.conn);
            assert!(
                crate::testing::wait_until(|| !has_tools()),
                "it kept the desktop's tools after a disconnect"
            );

            let second = Desktop::new(host, 2);
            second.bind(host, None);
            let channel = second.take_stream(host);
            assert!(
                !has_tools(),
                "back, but the agent still holds the offline list"
            );
            list(&mut agent, &second, channel, 2);
            assert!(agent.list_changes() > 0, "told its tools changed");
            assert!(has_tools());

            // A second agent in the tab: the tools wait for its listing too.
            let mut nested = Agent::start(&rig);
            let (nested_channel, _) = second.open();
            assert!(!has_tools(), "a second agent starting up");
            nested.send(init(0));
            assert_eq!(second.line(nested_channel)["method"], "initialize");
            second.answer(
                host,
                nested_channel,
                json!({"jsonrpc":"2.0","id":0,"result":{}}),
            );
            nested.recv();
            assert!(!has_tools(), "the second agent has not listed its tools");
            list(&mut nested, &second, nested_channel, 1);
            assert!(has_tools());
        }

        // Disconnect mid-call, keep the agent's stream, then the
        // same stream reaches the returning desktop.
        #[test]
        fn the_stream_survives_a_disconnect_and_a_returning_desktop_gets_it_back_without_replays() {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig(&first, host, runtime, root);
            let host = &rig.host;
            let mut agent = Agent::start(&rig);
            let (channel, open) = first.open();
            assert_eq!(open.binding_nonce, "a".repeat(64));
            agent.send(init(0));
            assert_eq!(first.line(channel)["method"], "initialize");
            first.answer(
                host,
                channel,
                json!({"jsonrpc":"2.0","id":0,"result":{"capabilities":{"tools":{}}}}),
            );
            assert!(agent.recv()["result"].is_object());
            agent.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
            assert_eq!(first.line(channel)["method"], "notifications/initialized");
            // Relayed while connected.
            agent.call(1, "list_machines", json!({}));
            assert_eq!(first.line(channel)["id"], 1);
            first.answer(host, channel, json!({"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"FIRST_DESKTOP"}]}}));
            assert_eq!(
                agent.recv()["result"]["content"][0]["text"],
                "FIRST_DESKTOP"
            );
            // In flight when the desktop goes away.
            agent.call(7, "hydrate_worktree", json!({}));
            assert_eq!(first.line(channel)["id"], 7);
            host.disconnect(&first.conn);
            let in_flight = agent.recv();
            assert_eq!(in_flight["id"], 7, "{in_flight}");
            assert_eq!(in_flight["error"]["data"]["type"], "desktop_offline");
            assert!(in_flight["error"]["message"]
                .as_str()
                .unwrap()
                .contains("may or may not have run"));
            // A late answer from the old link is never a second answer.
            first.answer(host, channel, json!({"jsonrpc":"2.0","id":7,"result":{}}));

            // Away, the machine serves the session's own timers.
            let wake = host.wake().unwrap();
            let timer = wake
                .set_timer(codesense_pty_core::session_wake::TimerSpec {
                    session: rig.session.clone(),
                    schedule: codesense_pty_core::session_wake::TimerSchedule::After {
                        after_ms: 3_600_000,
                        every_ms: None,
                    },
                    until_ms: None,
                    count: None,
                    text: "check CI".into(),
                    name: "ci".into(),
                    created_by: "agent:test".into(),
                    max_wait_ms: 60_000,
                    originator: Default::default(),
                    require_observed: false,
                    min_gap_ms: None,
                })
                .unwrap();
            agent.call(
                8,
                "list_wakeups",
                json!({"sessionId": rig.session.as_str()}),
            );
            let listed = agent.recv();
            assert_eq!(listed["id"], 8, "{listed}");
            assert_eq!(
                text(&listed)["timers"][0]["timerId"],
                timer.timer_id.as_str(),
                "{listed}"
            );
            agent.call(9, "list_wakeups", json!({"sessionId": "someone-else"}));
            assert!(agent.recv()["error"].is_object());
            // Context-repo tools say why, typed.
            agent.call(10, "hydrate_worktree", json!({}));
            assert_eq!(agent.recv()["error"]["data"]["type"], "desktop_offline");
            // Outbound messages are queued, in order.
            agent.call(11, "send_message", json!({"to": "lead", "text": "first"}));
            let queued = agent.recv();
            assert_eq!(text(&queued)["data"]["queued"], true, "{queued}");
            agent.call(12, "signal_progress", json!({"phase": "second"}));
            assert_eq!(text(&agent.recv())["data"]["position"], 2);
            assert!(agent.alive(), "the agent's MCP server stays up");

            // The desktop comes back on a new connection.
            let second = Desktop::new(host, 2);
            second.bind(host, None);
            let keyed = codesense_pty_core::routine_arm::routine_binding_nonce(
                SECRET,
                rig.session.as_str(),
            );
            // The outbox first, on a stream of its own.
            let (outbox, open) = second.open();
            assert_eq!(open.binding_nonce, keyed);
            assert_eq!(open.session, rig.session.as_str());
            let hello = second.line(outbox);
            assert_eq!(hello["id"], FLUSH_INIT_ID);
            second.answer(
                host,
                outbox,
                json!({"jsonrpc":"2.0","id":FLUSH_INIT_ID,"result":{}}),
            );
            assert_eq!(second.line(outbox)["method"], "notifications/initialized");
            let mut keys = Vec::new();
            for (tool, body) in [("send_message", "first"), ("signal_progress", "second")] {
                let delivered = second.line(outbox);
                assert_eq!(delivered["params"]["name"], tool, "{delivered}");
                assert!(delivered.to_string().contains(body));
                let key = delivered["params"]["_meta"][OUTBOX_META]
                    .as_str()
                    .unwrap()
                    .to_string();
                assert_eq!(delivered["id"], format!("codesense-outbox:{key}"));
                keys.push(key);
                second.answer(
                    host,
                    outbox,
                    json!({"jsonrpc":"2.0","id":delivered["id"],"result":{"content":[]}}),
                );
            }
            assert_ne!(keys[0], keys[1]);
            assert_eq!(
                second.next().kind,
                Kind::MCP_CLOSE,
                "the delivery stream ends"
            );
            // Then the agent's own stream, its initialize replayed.
            let (channel, open) = second.open();
            assert_eq!(open.binding_nonce, keyed);
            let replay = second.line(channel);
            assert_eq!(replay["id"], REATTACH_ID);
            assert_eq!(replay["method"], "initialize");
            assert_eq!(replay["params"]["_meta"]["codesenseSessionId"], "tab");
            assert_eq!(second.line(channel)["method"], "notifications/initialized");
            // A call now waits for the desktop's initialize answer.
            agent.call(13, "list_machines", json!({}));
            assert!(
                second.quiet(Duration::from_millis(300)).is_none(),
                "held until initialized"
            );
            second.answer(
                host,
                channel,
                json!({"jsonrpc":"2.0","id":REATTACH_ID,"result":{"capabilities":{}}}),
            );
            let relayed = second.line(channel);
            assert_eq!(relayed["id"], 13, "nothing replayed before it: {relayed}");
            second.answer(host, channel, json!({"jsonrpc":"2.0","id":13,"result":{"content":[{"type":"text","text":"SECOND_DESKTOP"}]}}));
            let back = agent.recv();
            assert_eq!(
                back["id"], 13,
                "the replayed initialize's answer is the machine's: {back}"
            );
            assert_eq!(back["result"]["content"][0]["text"], "SECOND_DESKTOP");
            assert!(
                second.quiet(Duration::from_millis(300)).is_none(),
                "id 7 never replayed"
            );
            // Told to re-list on going local, and again once handed back.
            assert_eq!(agent.list_changes(), 2, "{:?}", agent.notes);
            assert!(agent.alive());
            drop(agent.write);
            let _ = rig.host.terminal.kill(&rig.session);
        }

        // A returning desktop that refuses the session: the machine keeps
        // answering, and a call held for the reattach is answered, not lost.
        #[test]
        fn a_refused_reattach_keeps_the_machine_answering() {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig(&first, host, runtime, root);
            let host = &rig.host;
            let mut agent = Agent::start(&rig);
            let (channel, _) = first.open();
            agent.send(init(0));
            first.line(channel);
            first.answer(host, channel, json!({"jsonrpc":"2.0","id":0,"result":{}}));
            agent.recv();
            host.disconnect(&first.conn);
            let second = Desktop::new(host, 2);
            second.bind(host, None);
            let (channel, _) = second.open();
            assert_eq!(second.line(channel)["id"], REATTACH_ID);
            second.line(channel);
            agent.call(3, "list_machines", json!({}));
            assert!(second.quiet(Duration::from_millis(200)).is_none());
            host.reverse_frame(
                &second.conn,
                Frame {
                    kind: Kind::MCP_CLOSE,
                    channel,
                    payload: vec![],
                },
            );
            let answered = agent.recv();
            assert_eq!(answered["id"], 3);
            assert_eq!(answered["error"]["data"]["type"], "desktop_offline");
            agent.call(4, "list_wakeups", json!({}));
            assert_eq!(agent.recv()["id"], 4);
            assert!(agent.alive());
            drop(agent.write);
            let _ = rig.host.terminal.kill(&rig.session);
        }

        // Idempotent delivery: an outbox call whose answer the link lost is
        // delivered again under the same id, once, on the next reconnect.
        #[test]
        fn an_outbox_call_cut_mid_delivery_is_redelivered_under_the_same_id() {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig(&first, host, runtime, root);
            let host = &rig.host;
            first.bind(host, Some("mac-a"));
            let mut agent = Agent::start(&rig);
            let (channel, _) = first.open();
            agent.send(init(0));
            first.line(channel);
            first.answer(host, channel, json!({"jsonrpc":"2.0","id":0,"result":{}}));
            agent.recv();
            host.disconnect(&first.conn);
            agent.call(1, "inbox_post", json!({"title": "once"}));
            assert_eq!(text(&agent.recv())["data"]["queued"], true);
            let deliver = |id: u64, answer: bool| -> (Desktop, String) {
                let desktop = Desktop::new(host, id);
                desktop.bind(host, Some("mac-a"));
                let (outbox, _) = desktop.open();
                desktop.line(outbox);
                desktop.answer(
                    host,
                    outbox,
                    json!({"jsonrpc":"2.0","id":FLUSH_INIT_ID,"result":{}}),
                );
                desktop.line(outbox);
                let delivered = desktop.line(outbox);
                let key = delivered["params"]["_meta"][OUTBOX_META]
                    .as_str()
                    .unwrap()
                    .to_string();
                if answer {
                    desktop.answer(
                        host,
                        outbox,
                        json!({"jsonrpc":"2.0","id":delivered["id"],"result":{}}),
                    );
                }
                (desktop, key)
            };
            let (cut, key) = deliver(2, false);
            host.disconnect(&cut.conn);
            let (back, again) = deliver(3, true);
            assert_eq!(
                again, key,
                "the same outbox id: the desktop answers it once"
            );
            assert_eq!(back.next().kind, Kind::MCP_CLOSE);
            // Then only the agent's stream: nothing is delivered a third time.
            let (channel, _) = back.open();
            assert_eq!(back.line(channel)["id"], REATTACH_ID);
            drop(agent.write);
            let _ = rig.host.terminal.kill(&rig.session);
        }

        /// A stream relayed by `desktop`, initialized: its channel.
        fn connected(rig: &Rig, desktop: &Desktop) -> (Agent, u32) {
            let mut agent = Agent::start(rig);
            let (channel, _) = desktop.open();
            agent.send(init(0));
            assert_eq!(desktop.line(channel)["method"], "initialize");
            desktop.answer(
                &rig.host,
                channel,
                json!({"jsonrpc":"2.0","id":0,"result":{}}),
            );
            agent.recv();
            agent.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
            desktop.line(channel);
            (agent, channel)
        }

        fn rig_with(
            first: &Desktop,
            host: Arc<Host>,
            runtime: tokio::runtime::Runtime,
            root: tempfile::TempDir,
        ) -> Rig {
            let rig = rig(first, host, runtime, root);
            rig.reverse.set_timing(
                Duration::from_millis(400),
                Duration::from_millis(400),
                Duration::from_millis(50),
            );
            rig
        }

        fn queued(agent: &mut Agent, id: u64, tool: &str, args: Value) -> String {
            agent.call(id, tool, args);
            let answer = agent.recv();
            assert_eq!(answer["id"], id);
            let body = text(&answer);
            assert_eq!(body["data"]["queued"], true, "{answer}");
            body["data"]["outboxId"].as_str().unwrap().to_string()
        }

        fn kill(rig: &Rig) {
            let _ = rig.host.terminal.kill(&rig.session);
        }

        // The same desktop binds again on a new link while
        // its old one is half-open; when the old one is reaped the agents
        // go to the new one.
        #[test]
        fn a_new_bind_before_the_old_link_drops_takes_the_agents_when_it_does() {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig_with(&first, host, runtime, root);
            let host = &rig.host;
            let (mut agent, _) = connected(&rig, &first);
            let second = Desktop::new(host, 2);
            second.bind(host, None);
            assert!(
                second.quiet(Duration::from_millis(300)).is_none(),
                "the old link still relays"
            );
            host.disconnect(&first.conn);
            let channel = second.take_stream(host);
            agent.call(1, "list_machines", json!({}));
            assert_eq!(second.line(channel)["id"], 1);
            drop(agent.write);
            kill(&rig);
        }

        // A delivery that times out is retried, and the
        // agent's stream is handed back only once the outbox drained.
        #[test]
        fn a_timed_out_delivery_is_retried_before_the_stream_is_handed_back() {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig_with(&first, host, runtime, root);
            let host = &rig.host;
            let (mut agent, _) = connected(&rig, &first);
            host.disconnect(&first.conn);
            queued(
                &mut agent,
                1,
                "send_message",
                json!({"to": "lead", "text": "x"}),
            );
            let second = Desktop::new(host, 2);
            second.bind(host, None);
            let (stalled, _) = second.open();
            assert_eq!(second.line(stalled)["id"], FLUSH_INIT_ID);
            // Not answered: the delivery gives up, and nothing else comes
            // before it is tried again.
            let end = second.next();
            assert_eq!(
                (end.kind, end.channel),
                (Kind::MCP_CLOSE, stalled),
                "{end:?}"
            );
            let calls = second.take_outbox(host, 1);
            assert_eq!(calls[0]["params"]["name"], "send_message");
            second.take_stream(host);
            drop(agent.write);
            kill(&rig);
        }

        // A returning desktop that does not answer the
        // replayed initialize in time: the held call is answered here, and
        // the hand-back is tried again.
        #[test]
        fn a_reattach_timeout_answers_the_held_call_and_retries() {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig_with(&first, host, runtime, root);
            let host = &rig.host;
            let (mut agent, _) = connected(&rig, &first);
            host.disconnect(&first.conn);
            let second = Desktop::new(host, 2);
            second.bind(host, None);
            let (silent, _) = second.open();
            assert_eq!(second.line(silent)["id"], REATTACH_ID);
            second.line(silent);
            agent.call(30, "list_machines", json!({}));
            let answered = agent.recv();
            assert_eq!(answered["id"], 30);
            assert_eq!(answered["error"]["data"]["type"], "desktop_offline");
            let end = second.next();
            assert_eq!((end.kind, end.channel), (Kind::MCP_CLOSE, silent));
            let channel = second.take_stream(host);
            agent.call(31, "list_machines", json!({}));
            assert_eq!(second.line(channel)["id"], 31);
            drop(agent.write);
            kill(&rig);
        }

        // A desktop overrunning the stream (an oversize
        // frame) moves it here; the call in flight is answered once and the
        // stream is handed back to the desktop still connected.
        #[test]
        fn an_overrun_answers_the_call_in_flight_once_and_hands_the_stream_back() {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig_with(&first, host, runtime, root);
            let host = &rig.host;
            let (mut agent, channel) = connected(&rig, &first);
            agent.call(40, "hydrate_worktree", json!({}));
            assert_eq!(first.line(channel)["id"], 40);
            host.reverse_frame(
                &first.conn,
                Frame {
                    kind: Kind::MCP_DATA,
                    channel,
                    payload: vec![b' '; wire::CHUNK_BYTES + 1],
                },
            );
            let answered = agent.recv();
            assert_eq!(answered["id"], 40);
            assert!(answered["error"]["message"]
                .as_str()
                .unwrap()
                .contains("may or may not have run"));
            let end = first.next();
            assert_eq!((end.kind, end.channel), (Kind::MCP_CLOSE, channel));
            let back = first.take_stream(host);
            agent.call(41, "list_machines", json!({}));
            assert_eq!(first.line(back)["id"], 41);
            drop(agent.write);
            kill(&rig);
        }

        // A long answer (a full tools/list is hundreds of KiB) reaches the
        // agent whole, however fast its chunks arrive: a burst of a legal
        // line is not an overrun.
        #[test]
        fn a_long_answer_arriving_in_a_burst_reaches_the_agent_whole() {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig(&first, host, runtime, root);
            let host = &rig.host;
            let (mut agent, channel) = connected(&rig, &first);
            agent.send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
            assert_eq!(first.line(channel)["id"], 2);
            let tools: Vec<Value> = (0..400)
                .map(|n| json!({"name": format!("tool_{n}"), "description": "x".repeat(2000)}))
                .collect();
            let answer = format!(
                "{}\n",
                json!({"jsonrpc":"2.0","id":2,"result":{"tools":tools}})
            );
            assert!(answer.len() > 32 * wire::CHUNK_BYTES && answer.len() < wire::LINE_BYTES);
            for chunk in answer.as_bytes().chunks(wire::CHUNK_BYTES) {
                host.reverse_frame(
                    &first.conn,
                    Frame {
                        kind: Kind::MCP_DATA,
                        channel,
                        payload: chunk.to_vec(),
                    },
                );
            }
            let listed = agent.recv();
            assert_eq!(listed["id"], 2);
            assert_eq!(
                listed["result"]["tools"].as_array().map(Vec::len),
                Some(400),
                "{}",
                listed["error"]
            );
            assert!(first.quiet(Duration::from_millis(200)).is_none());
            drop(agent.write);
            kill(&rig);
        }

        // A desktop-configured server other than CodeSense that ends while
        // its desktop is still connected (it exited mid-call, overran, or
        // its grant was revoked) ends the agent's stream: what it sent first
        // is delivered, its call in flight gets no stand-in answer, and the
        // machine does not take the stream over (`desktop_offline` is for a
        // desktop that went away).
        #[test]
        fn a_relayed_desktop_server_that_ends_closes_the_agents_stream() {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig(&first, host, runtime, root);
            let host = &rig.host;
            let (agent, daemon) = UnixStream::pair().unwrap();
            let reverse = rig.reverse.clone();
            let session = rig.session.as_str().to_string();
            let relay = Some((first.conn.clone(), "a".repeat(64)));
            let served = std::thread::spawn(move || {
                reverse.run_stream(daemon, session, "desktop-0".into(), relay, false)
            });
            agent.set_read_timeout(Some(crate::testing::WAIT)).unwrap();
            let mut write = agent.try_clone().unwrap();
            let mut read = std::io::BufReader::new(agent);
            let (channel, open) = first.open();
            assert_eq!(open.server_id, "desktop-0");
            write
                .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n")
                .unwrap();
            assert_eq!(first.line(channel)["id"], 1);
            write
                .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"mutate\"}}\n")
                .unwrap();
            assert_eq!(first.line(channel)["id"], 2);
            first.answer(
                host,
                channel,
                json!({"jsonrpc":"2.0","id":1,"result":{"tools":[]}}),
            );
            host.reverse_frame(
                &first.conn,
                Frame {
                    kind: Kind::MCP_CLOSE,
                    channel,
                    payload: vec![],
                },
            );
            let mut line = String::new();
            read.read_line(&mut line).unwrap();
            let answered: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(answered["id"], 1, "{answered}");
            line.clear();
            assert_eq!(read.read_line(&mut line).unwrap(), 0, "unexpected: {line}");
            drop(write);
            assert!(served.join().unwrap().is_err());
            assert!(first.quiet(Duration::from_millis(200)).is_none());
            kill(&rig);
        }

        // More than MAX_HELD lines while handed back: all
        // answered here, then the hand-back is tried again.
        #[test]
        fn too_many_held_lines_are_answered_here_and_the_hand_back_retried() {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig_with(&first, host, runtime, root);
            rig.reverse.set_timing(
                Duration::from_secs(20),
                Duration::from_millis(400),
                Duration::from_millis(50),
            );
            let host = &rig.host;
            let (mut agent, _) = connected(&rig, &first);
            host.disconnect(&first.conn);
            let second = Desktop::new(host, 2);
            second.bind(host, None);
            let (held, _) = second.open();
            second.line(held);
            second.line(held);
            for id in 100..(100 + MAX_HELD as u64 + 1) {
                agent.call(id, "list_machines", json!({}));
            }
            let ids: Vec<u64> = (0..=MAX_HELD)
                .map(|_| agent.recv()["id"].as_u64().unwrap())
                .collect();
            assert_eq!(ids, (100..(100 + MAX_HELD as u64 + 1)).collect::<Vec<_>>());
            let end = second.next();
            assert_eq!((end.kind, end.channel), (Kind::MCP_CLOSE, held));
            second.take_stream(host);
            drop(agent.write);
            kill(&rig);
        }

        // A call queued while the stream is relayed (the
        // window between a delivery and the hand-back) is delivered, and an
        // outbox call sent after it waits its turn instead of overtaking.
        #[test]
        fn a_call_sent_after_queued_ones_waits_its_turn() {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig_with(&first, host, runtime, root);
            let host = &rig.host;
            let (mut agent, channel) = connected(&rig, &first);
            let line = json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"send_message","arguments":{"to":"lead","text":"first"}}}).to_string();
            let call = crate::offline_mcp::parse(&line).unwrap();
            rig.reverse.enqueue(rig.session.as_str(), &call, &line);
            let (outbox, _) = first.open();
            assert_eq!(first.line(outbox)["id"], FLUSH_INIT_ID);
            let second_key = queued(
                &mut agent,
                50,
                "send_message",
                json!({"to": "lead", "text": "second"}),
            );
            first.answer(
                host,
                outbox,
                json!({"jsonrpc":"2.0","id":FLUSH_INIT_ID,"result":{}}),
            );
            first.line(outbox);
            let texts: Vec<Value> = (0..2)
                .map(|_| {
                    let call = first.line(outbox);
                    first.answer(
                        host,
                        outbox,
                        json!({"jsonrpc":"2.0","id":call["id"],"result":{}}),
                    );
                    call
                })
                .collect();
            assert_eq!(texts[0]["params"]["arguments"]["text"], "first");
            assert_eq!(texts[1]["params"]["arguments"]["text"], "second");
            assert_eq!(
                texts[1]["params"]["_meta"][OUTBOX_META],
                second_key.as_str()
            );
            assert_eq!(first.next().kind, Kind::MCP_CLOSE);
            // Drained: the next one goes straight through.
            agent.call(51, "send_message", json!({"to": "lead", "text": "third"}));
            assert_eq!(first.line(channel)["id"], 51);
            drop(agent.write);
            kill(&rig);
        }

        // Review SHOULD: an agent cannot name its own outbox id.
        #[test]
        fn a_forged_outbox_id_never_reaches_the_desktop() {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig_with(&first, host, runtime, root);
            let (mut agent, channel) = connected(&rig, &first);
            agent.send(json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"send_message","arguments":{"to":"a","text":"b"},"_meta":{OUTBOX_META:"stolen-key"}}}));
            let seen = first.line(channel);
            assert_eq!(seen["id"], 7);
            assert!(seen["params"]["_meta"].get(OUTBOX_META).is_none(), "{seen}");
            drop(agent.write);
            kill(&rig);
        }

        fn wait_ended(rig: &Rig) {
            let deadline = Instant::now() + crate::testing::WAIT;
            while !lock(&rig.reverse.outbox)
                .get(rig.session.as_str())
                .is_none_or(|outbox| outbox.ended)
            {
                assert!(Instant::now() < deadline, "the session never ended");
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        // An agent that queued calls and ended before its
        // desktop came back (a routine's, typically) still has them
        // delivered, in order.
        #[test]
        fn an_ended_sessions_queued_calls_reach_the_returning_desktop() {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig_with(&first, host, runtime, root);
            let host = &rig.host;
            let (mut agent, _) = connected(&rig, &first);
            host.disconnect(&first.conn);
            queued(
                &mut agent,
                1,
                "send_message",
                json!({"to": "lead", "text": "done"}),
            );
            queued(&mut agent, 2, "signal_progress", json!({"phase": "review"}));
            drop(agent.write);
            kill(&rig);
            wait_ended(&rig);
            let second = Desktop::new(host, 2);
            second.bind(host, None);
            let calls = second.take_outbox(host, 2);
            assert_eq!(calls[0]["params"]["name"], "send_message");
            assert_eq!(calls[1]["params"]["name"], "signal_progress");
            assert!(
                second.quiet(Duration::from_millis(300)).is_none(),
                "no stream to hand back"
            );
            assert!(lock(&rig.reverse.outbox)
                .get(rig.session.as_str())
                .is_none());
        }

        // Queued calls that cannot be delivered are recorded
        // as lost: the returning desktop will not take an ended session's
        // calls, or no desktop ever relayed the session.
        #[test]
        fn undeliverable_queued_calls_are_recorded_as_lost() {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig_with(&first, host, runtime, root);
            let host = &rig.host;
            first.bind(host, Some("mac-a"));
            let (mut agent, _) = connected(&rig, &first);
            host.disconnect(&first.conn);
            let key = queued(
                &mut agent,
                1,
                "send_message",
                json!({"to": "lead", "text": "x"}),
            );
            drop(agent.write);
            kill(&rig);
            wait_ended(&rig);
            let second = Desktop::new(host, 2);
            second.bind(host, Some("mac-a"));
            let (refused, _) = second.open();
            host.reverse_frame(
                &second.conn,
                Frame {
                    kind: Kind::MCP_CLOSE,
                    channel: refused,
                    payload: vec![],
                },
            );
            let deadline = Instant::now() + crate::testing::WAIT;
            while lock(&rig.reverse.outbox).contains_key(rig.session.as_str()) {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(20));
            }
            let lost = rig.reverse.outbox_store.lost();
            assert_eq!(lost[0]["outboxId"], key.as_str());
            assert!(lost[0]["why"].as_str().unwrap().contains("would not take"));

            // Never relayed by a known desktop: lost when it ends.
            let (host2, runtime2, root2) = self::host();
            let lone = Desktop::new(&host2, 1);
            let rig2 = rig_with(&lone, host2, runtime2, root2);
            let (mut agent2, channel) = connected(&rig2, &lone);
            rig2.host.reverse_frame(
                &lone.conn,
                Frame {
                    kind: Kind::MCP_CLOSE,
                    channel,
                    payload: vec![],
                },
            );
            let key2 = queued(&mut agent2, 1, "inbox_post", json!({"title": "t"}));
            // It ends while its desktop (which never said who it is, and
            // refused it) is still connected.
            drop(agent2.write);
            kill(&rig2);
            let deadline = Instant::now() + crate::testing::WAIT;
            while rig2.reverse.outbox_store.lost().is_empty() {
                assert!(Instant::now() < deadline, "no loss recorded");
                std::thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(
                rig2.reverse.outbox_store.lost()[0]["outboxId"],
                key2.as_str()
            );
            rig2.host.disconnect(&lone.conn);
        }

        // Queued calls survive a terminal-server restart and
        // are delivered to the desktop that returns.
        #[test]
        fn queued_calls_survive_a_terminal_server_restart() {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig_with(&first, host, runtime, root);
            let host = &rig.host;
            let (mut agent, _) = connected(&rig, &first);
            host.disconnect(&first.conn);
            let key = queued(
                &mut agent,
                1,
                "send_message",
                json!({"to": "lead", "text": "kept"}),
            );
            drop(agent.write);
            // A new terminal server on the same root.
            host.start_reverse_mcp(rig.root.path()).unwrap();
            let second = Desktop::new(host, 2);
            second.bind(host, None);
            let calls = second.take_outbox(host, 1);
            assert_eq!(calls[0]["params"]["_meta"][OUTBOX_META], key.as_str());
            kill(&rig);
        }

        // Review SHOULD: a stable desktop identity: another desktop of the
        // same person (the same viewer) does not take this one's agents.
        #[test]
        fn agents_go_back_only_to_the_desktop_that_relayed_them() {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            first.bind(&host, Some("mac-a"));
            let rig = rig_with(&first, host, runtime, root);
            rig.reverse
                .set_desktop(rig.session.as_str(), Some("mac-a".into()));
            let host = &rig.host;
            let (mut agent, _) = connected(&rig, &first);
            host.disconnect(&first.conn);
            let other = Desktop::new(host, 2);
            other.bind(host, Some("mac-b"));
            assert!(other.quiet(Duration::from_millis(300)).is_none());
            let back = Desktop::new(host, 3);
            back.bind(host, Some("mac-a"));
            let channel = back.take_stream(host);
            agent.call(1, "list_machines", json!({}));
            assert_eq!(back.line(channel)["id"], 1);
            drop(agent.write);
            kill(&rig);
        }

        // Review SHOULD: a desktop's refusal of a delivered call reaches the
        // person as an inbox note, and the call is recorded as not delivered.
        #[test]
        fn a_refused_delivery_is_noted_in_the_inbox() {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig_with(&first, host, runtime, root);
            let host = &rig.host;
            let (mut agent, _) = connected(&rig, &first);
            host.disconnect(&first.conn);
            queued(
                &mut agent,
                1,
                "send_message",
                json!({"to": "nobody", "text": "x"}),
            );
            let second = Desktop::new(host, 2);
            second.bind(host, None);
            let (outbox, _) = second.open();
            second.line(outbox);
            second.answer(
                host,
                outbox,
                json!({"jsonrpc":"2.0","id":FLUSH_INIT_ID,"result":{}}),
            );
            second.line(outbox);
            let call = second.line(outbox);
            second.answer(host, outbox, json!({"jsonrpc":"2.0","id":call["id"],"error":{"code":-32602,"message":"unknown recipient nobody"}}));
            let note = second.line(outbox);
            assert_eq!(note["params"]["name"], "inbox_post");
            assert!(note["params"]["arguments"]["context"]
                .as_str()
                .unwrap()
                .contains("unknown recipient"));
            second.answer(
                host,
                outbox,
                json!({"jsonrpc":"2.0","id":note["id"],"result":{}}),
            );
            assert_eq!(second.next().kind, Kind::MCP_CLOSE);
            assert!(rig.reverse.outbox_store.lost()[0]["why"]
                .as_str()
                .unwrap()
                .contains("refused on delivery"));
            second.take_stream(host);
            drop(agent.write);
            kill(&rig);
        }

        /// A stream relayed by `first` (named `identity`) whose desktop went
        /// away after its agent queued a call and ended.
        fn ended_with_a_queued_call(identity: &str) -> (Rig, Desktop, String) {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig_with(&first, host, runtime, root);
            first.bind(&rig.host, Some(identity));
            let (mut agent, _) = connected(&rig, &first);
            rig.host.disconnect(&first.conn);
            let key = queued(
                &mut agent,
                1,
                "send_message",
                json!({"to": "lead", "text": "x"}),
            );
            drop(agent.write);
            kill(&rig);
            wait_ended(&rig);
            (rig, first, key)
        }

        // An ended session's calls go only to the desktop
        // identity that relayed it, and a refusal is final only from that
        // named desktop: an older desktop matched by viewer refusing them
        // records nothing lost.
        #[test]
        fn an_ended_sessions_calls_go_only_to_its_own_desktop() {
            let (rig, _first, key) = ended_with_a_queued_call("mac-a");
            let host = &rig.host;
            let other = Desktop::new(host, 2);
            other.bind(host, Some("mac-b"));
            assert!(
                other.quiet(Duration::from_millis(300)).is_none(),
                "another Mac takes nothing"
            );
            // The same app before it named itself (viewer only): no match.
            let older = Desktop::new(host, 3);
            older.bind(host, None);
            assert!(older.quiet(Duration::from_millis(300)).is_none());
            assert!(rig.reverse.outbox_store.lost().is_empty());
            let own = Desktop::new(host, 4);
            own.bind(host, Some("mac-a"));
            let calls = own.take_outbox(host, 1);
            assert_eq!(calls[0]["params"]["_meta"][OUTBOX_META], key.as_str());
        }

        #[test]
        fn a_refusal_by_an_unnamed_desktop_loses_nothing() {
            // Relayed by an older desktop (no identity): its viewer is all
            // there is to match by, so its refusal is not final.
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig_with(&first, host, runtime, root);
            let host = &rig.host;
            let (mut agent, _) = connected(&rig, &first);
            host.disconnect(&first.conn);
            queued(
                &mut agent,
                1,
                "send_message",
                json!({"to": "lead", "text": "x"}),
            );
            drop(agent.write);
            kill(&rig);
            wait_ended(&rig);
            let older = Desktop::new(host, 2);
            older.bind(host, None);
            let (refused, _) = older.open();
            host.reverse_frame(
                &older.conn,
                Frame {
                    kind: Kind::MCP_CLOSE,
                    channel: refused,
                    payload: vec![],
                },
            );
            std::thread::sleep(Duration::from_millis(300));
            assert!(rig.reverse.outbox_store.lost().is_empty());
            assert!(
                lock(&rig.reverse.outbox).contains_key(rig.session.as_str()),
                "kept"
            );
        }

        // r2 LOW: an older desktop (no identity, no once-store) runs a
        // redelivered id again: a call cut mid-delivery may have run, so it
        // is not delivered twice.
        #[test]
        fn a_cut_delivery_to_an_unnamed_desktop_is_not_redelivered() {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig_with(&first, host, runtime, root);
            let host = &rig.host;
            let (mut agent, _) = connected(&rig, &first);
            host.disconnect(&first.conn);
            let key = queued(&mut agent, 1, "inbox_post", json!({"title": "once"}));
            let cut = Desktop::new(host, 2);
            cut.bind(host, None);
            let (outbox, _) = cut.open();
            cut.line(outbox);
            cut.answer(
                host,
                outbox,
                json!({"jsonrpc":"2.0","id":FLUSH_INIT_ID,"result":{}}),
            );
            cut.line(outbox);
            cut.line(outbox);
            host.disconnect(&cut.conn);
            let deadline = Instant::now() + crate::testing::WAIT;
            while rig.reverse.outbox_store.lost().is_empty() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(20));
            }
            let lost = rig.reverse.outbox_store.lost();
            assert_eq!(lost[0]["outboxId"], key.as_str());
            assert!(lost[0]["why"].as_str().unwrap().contains("may have run"));
            let back = Desktop::new(host, 3);
            back.bind(host, None);
            let (channel, _) = back.open();
            assert_eq!(
                back.line(channel)["id"],
                REATTACH_ID,
                "no second delivery, only the stream"
            );
            drop(agent.write);
            kill(&rig);
        }

        // The desktop is told, once, in one inbox note, which of
        // its agents' calls were lost.
        #[test]
        fn lost_calls_are_noted_once_when_their_desktop_connects() {
            let (rig, _first, _key) = ended_with_a_queued_call("mac-a");
            let host = &rig.host;
            let refusing = Desktop::new(host, 2);
            refusing.bind(host, Some("mac-a"));
            let (refused, _) = refusing.open();
            host.reverse_frame(
                &refusing.conn,
                Frame {
                    kind: Kind::MCP_CLOSE,
                    channel: refused,
                    payload: vec![],
                },
            );
            let deadline = Instant::now() + crate::testing::WAIT;
            while rig.reverse.outbox_store.lost().is_empty() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(20));
            }
            host.disconnect(&refusing.conn);
            let back = Desktop::new(host, 3);
            back.bind(host, Some("mac-a"));
            let calls = back.take_outbox(host, 1);
            assert_eq!(calls[0]["params"]["name"], "inbox_post");
            assert!(calls[0]["params"]["arguments"]["title"]
                .as_str()
                .unwrap()
                .contains("not delivered"));
            host.disconnect(&back.conn);
            let again = Desktop::new(host, 4);
            again.bind(host, Some("mac-a"));
            assert!(
                again.quiet(Duration::from_millis(300)).is_none(),
                "told once"
            );
        }

        // A call the machine cannot save is refused, typed, not
        // answered "queued".
        #[test]
        fn a_call_the_machine_cannot_save_is_refused() {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig_with(&first, host, runtime, root);
            let (mut agent, _) = connected(&rig, &first);
            rig.host.disconnect(&first.conn);
            let dir = rig.root.path().join("outbox");
            std::fs::remove_dir_all(&dir).unwrap();
            std::fs::write(&dir, b"not a directory").unwrap();
            agent.call(1, "send_message", json!({"to": "lead", "text": "x"}));
            let refused = agent.recv();
            assert_eq!(
                refused["error"]["data"]["type"], "desktop_offline",
                "{refused}"
            );
            assert!(refused["error"]["message"]
                .as_str()
                .unwrap()
                .contains("could not save"));
            assert!(lock(&rig.reverse.outbox)
                .get(rig.session.as_str())
                .is_none_or(|o| o.is_empty()));
            drop(agent.write);
            kill(&rig);
        }

        // A desktop that could not record an outbox id asks for
        // the call again: it stays queued and is delivered next time.
        #[test]
        fn a_retry_answer_keeps_the_call_queued() {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig_with(&first, host, runtime, root);
            let host = &rig.host;
            first.bind(host, Some("mac-a"));
            let (mut agent, _) = connected(&rig, &first);
            host.disconnect(&first.conn);
            let key = queued(
                &mut agent,
                1,
                "send_message",
                json!({"to": "lead", "text": "x"}),
            );
            let back = Desktop::new(host, 2);
            back.bind(host, Some("mac-a"));
            let (outbox, _) = back.open();
            back.line(outbox);
            back.answer(
                host,
                outbox,
                json!({"jsonrpc":"2.0","id":FLUSH_INIT_ID,"result":{}}),
            );
            back.line(outbox);
            let call = back.line(outbox);
            back.answer(host, outbox, json!({"jsonrpc":"2.0","id":call["id"],"error":{"code":-32603,"message":"not recorded","data":{"type":"outbox_retry"}}}));
            assert_eq!(back.next().kind, Kind::MCP_CLOSE);
            let again = back.take_outbox(host, 1);
            assert_eq!(again[0]["params"]["_meta"][OUTBOX_META], key.as_str());
            assert!(rig.reverse.outbox_store.lost().is_empty());
            drop(agent.write);
            kill(&rig);
        }

        // An ended routine session whose calls were stamped with
        // the plain viewer (its arm reached the machine before the desktop
        // named itself) is the named desktop's once it names itself: its
        // outbox goes there, and only an outbox of that install moves.
        #[test]
        fn calls_stamped_before_the_desktop_named_itself_reach_it() {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig_with(&first, host, runtime, root);
            let host = &rig.host;
            let (mut agent, _) = connected(&rig, &first);
            host.disconnect(&first.conn);
            let key = queued(
                &mut agent,
                1,
                "send_message",
                json!({"to": "lead", "text": "x"}),
            );
            drop(agent.write);
            kill(&rig);
            wait_ended(&rig);
            assert_eq!(
                lock(&rig.reverse.outbox)[rig.session.as_str()]
                    .desktop
                    .as_deref(),
                Some(VIEWER)
            );
            let session = rig.session.as_str().to_string();
            rig.reverse.restamp(VIEWER, "host-2", "host-2.41", &|s| {
                (s == session).then(|| "host-1".into())
            });
            assert_eq!(
                lock(&rig.reverse.outbox)[rig.session.as_str()]
                    .desktop
                    .as_deref(),
                Some(VIEWER),
                "another install's outbox stays"
            );
            rig.reverse.restamp(VIEWER, "host-1", "host-1.41", &|s| {
                (s == session).then(|| "host-1".into())
            });
            let named = Desktop::new(host, 2);
            named.bind(host, Some("host-1.41"));
            let calls = named.take_outbox(host, 1);
            assert_eq!(calls[0]["params"]["_meta"][OUTBOX_META], key.as_str());
        }

        // A relayed session bound to no desktop yet is no one's: every push is refused.
        #[test]
        fn a_push_for_a_session_bound_to_no_desktop_is_refused() {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig_with(&first, host, runtime, root);
            let host = &rig.host;
            host.handle(
                &first.conn,
                3,
                Request::InboxMail {
                    session: rig.session.clone(),
                    notice: "You have 1 new message.".into(),
                    token: "t-1".into(),
                },
            );
            assert!(reply_to(&first.frames, 3).is_err());
            kill(&rig);
        }

        // A relayed session's notices are its own desktop's: another desktop's push is
        // refused and its waiter list leaves the session out.
        #[test]
        fn another_desktops_push_for_a_session_is_refused() {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig_with(&first, host, runtime, root);
            let host = &rig.host;
            first.bind(host, Some("host-1.41"));
            let other = Desktop::new(host, 2);
            other.bind(host, Some("host-2.42"));
            let mail = |token: &str| Request::InboxMail {
                session: rig.session.clone(),
                notice: "You have 1 new message.".into(),
                token: token.into(),
            };
            host.handle(&other.conn, 3, mail("t-other"));
            assert!(reply_to(&other.frames, 3).is_err());
            host.handle(&other.conn, 4, Request::InboxWaiters);
            assert_eq!(
                reply_to(&other.frames, 4),
                Ok(Reply::InboxWaiters { waiters: vec![] })
            );
            host.handle(&first.conn, 5, mail("t-own"));
            assert_eq!(reply_to(&first.frames, 5), Ok(Reply::Done));
            // Both listen for inbox reports; only the session's own desktop hears them.
            for (desktop, rid) in [(&first, 6), (&other, 7)] {
                host.handle(&desktop.conn, rid, Request::Listen);
                assert_eq!(reply_to(&desktop.frames, rid), Ok(Reply::Done));
                host.handle(&desktop.conn, rid + 10, Request::InboxWaiters);
                assert!(reply_to(&desktop.frames, rid + 10).is_ok());
            }
            let (waiter, _helper) = UnixStream::pair().unwrap();
            let (activity, session) = (
                host.terminal.activity().clone(),
                rig.session.as_str().to_string(),
            );
            let agent = codesense_pty_core::activity::peer::inspect(std::process::id()).unwrap();
            std::thread::spawn(move || {
                let outlet = activity.inbox_outlet().unwrap();
                activity.inbox().serve(
                    &*outlet,
                    &session,
                    codesense_pty_core::inbox_waiters::WaitArm::Replace,
                    agent,
                    false,
                    None,
                    waiter,
                )
            });
            let inbox_event = |desktop: &Desktop, wait: Duration| {
                let mut ingress = Ingress::default();
                let deadline = Instant::now() + wait;
                while let Some(left) = deadline.checked_duration_since(Instant::now()) {
                    let Ok(frame) = desktop.frames.recv_timeout(left) else {
                        break;
                    };
                    if let Ok(Some(Inbound::Event(
                        event @ (TerminalDomainEvent::InboxWaiter { .. }
                        | TerminalDomainEvent::InboxAck { .. }),
                    ))) = ingress.accept(&frame)
                    {
                        return Some(event);
                    }
                }
                None
            };
            assert!(inbox_event(&first, crate::testing::WAIT).is_some());
            assert_eq!(inbox_event(&other, Duration::from_secs(1)), None);
            kill(&rig);
        }

        // An app that reconnects asks for its waiters before it names itself, and is
        // given none: it hears of them once it names itself.
        #[test]
        fn a_desktop_hears_of_its_waiters_once_it_names_itself() {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig_with(&first, host, runtime, root);
            let host = &rig.host;
            first.bind(host, Some("host-1.41"));
            // Its agent waits for mail, as it did before the app went away.
            let (waiter, _helper) = UnixStream::pair().unwrap();
            let (activity, session) = (
                host.terminal.activity().clone(),
                rig.session.as_str().to_string(),
            );
            let agent = codesense_pty_core::activity::peer::inspect(std::process::id()).unwrap();
            std::thread::spawn(move || {
                let outlet = activity.inbox_outlet().unwrap();
                activity.inbox().serve(
                    &*outlet,
                    &session,
                    codesense_pty_core::inbox_waiters::WaitArm::Replace,
                    agent,
                    false,
                    None,
                    waiter,
                )
            });
            assert!(crate::testing::wait_until(|| {
                host.terminal
                    .activity()
                    .inbox()
                    .state(rig.session.as_str())
                    .waiting
            }));
            host.disconnect(&first.conn);
            let back = Desktop::new(host, 2);
            host.handle(&back.conn, 2, Request::Listen);
            assert_eq!(reply_to(&back.frames, 2), Ok(Reply::Done));
            host.handle(&back.conn, 3, Request::InboxWaiters);
            assert_eq!(
                reply_to(&back.frames, 3),
                Ok(Reply::InboxWaiters { waiters: vec![] })
            );
            // Named by hand: the waiter is told before the bind is answered.
            host.handle(
                &back.conn,
                4,
                Request::RoutineArmBindSecret {
                    secret: SECRET.into(),
                    desktop: Some("host-1.41".into()),
                },
            );
            let mut ingress = Ingress::default();
            let told = loop {
                let frame = back
                    .frames
                    .recv_timeout(crate::testing::WAIT)
                    .expect("a waiter");
                if let Ok(Some(Inbound::Event(TerminalDomainEvent::InboxWaiter { state }))) =
                    ingress.accept(&frame)
                {
                    break state;
                }
            };
            assert_eq!(told.session, rig.session);
            assert!(told.waiting, "{told:?}");
            kill(&rig);
        }

        // Claude sends a version probe before `initialize`, and the desktop's server closes
        // the stream on it: the agent's `initialize` still reaches the desktop, whose
        // instructions are the ones the agent keeps.
        #[test]
        fn an_initialize_after_the_desktop_refused_a_probe_is_the_desktops() {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig(&first, host, runtime, root);
            let host = &rig.host;
            let mut agent = Agent::start(&rig);
            let (probe, _) = first.open();
            agent.send(json!({"jsonrpc":"2.0","id":0,"method":"server/discover"}));
            assert_eq!(first.line(probe)["method"], "server/discover");
            host.reverse_frame(
                &first.conn,
                Frame {
                    kind: Kind::MCP_CLOSE,
                    channel: probe,
                    payload: vec![],
                },
            );
            let refused = agent.recv();
            assert_eq!(refused["id"], 0);
            assert!(refused.get("error").is_some(), "{refused}");
            let closed = first.next();
            assert_eq!((closed.kind, closed.channel), (Kind::MCP_CLOSE, probe));
            agent.send(init(1));
            let (channel, _) = first.open();
            assert_ne!(channel, probe);
            assert_eq!(first.line(channel)["id"], 1);
            first.answer(
                host,
                channel,
                json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"codesense","version":"1"},"instructions":"desktop"}}),
            );
            assert_eq!(agent.recv()["result"]["instructions"], "desktop");
            assert!(
                first.quiet(Duration::from_millis(200)).is_none(),
                "no hand-back follows"
            );
            drop(agent.write);
            kill(&rig);
        }

        // A desktop that refuses the stream an `initialize` was relayed on: the machine
        // answers the `initialize`, so the agent keeps its CodeSense server.
        #[test]
        fn an_initialize_the_desktop_refuses_is_the_machines() {
            let (host, runtime, root) = host();
            let first = Desktop::new(&host, 1);
            let rig = rig(&first, host, runtime, root);
            let host = &rig.host;
            let mut agent = Agent::start(&rig);
            let (channel, _) = first.open();
            agent.send(init(1));
            assert_eq!(first.line(channel)["id"], 1);
            host.reverse_frame(
                &first.conn,
                Frame {
                    kind: Kind::MCP_CLOSE,
                    channel,
                    payload: vec![],
                },
            );
            let answer = agent.recv();
            assert_eq!(answer["id"], 1);
            assert!(answer["result"]["instructions"].is_string(), "{answer}");
            drop(agent.write);
            kill(&rig);
        }
    }
}

impl ReverseMcp {
    /// `InboxWaitV1`: whether `session`'s agent has its CodeSense tools, so it can read
    /// the mail it is told of; `None` when it has no CodeSense stream (see
    /// [`InboxOutlet::has_tools`](codesense_pty_core::inbox_host::InboxOutlet::has_tools)).
    pub(crate) fn has_tools(&self, session: &str) -> Option<bool> {
        let links: Vec<Arc<Link>> = lock(&self.streams)
            .values()
            // An agent's streams, and one starting up through a desktop (its `initialize`
            // not read yet). One opened while no desktop relayed it and never initialized
            // is never handed back, so it is not waited for.
            .filter(|stream| {
                stream.is_agent_of(session)
                    || (stream.is_codesense_of(session)
                        && matches!(*lock(&stream.link.route), Route::Desktop { .. }))
            })
            .map(|stream| stream.link.clone())
            .collect();
        // Every one: the waiting agent's own stream is not told apart from the others.
        (!links.is_empty()).then(|| links.iter().all(|link| link.has_tools()))
    }
}
