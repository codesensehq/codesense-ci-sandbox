// SPDX-License-Identifier: MIT
//! The sidecar's runtime host: one `codesense-pty-core` [`TerminalHost`], built
//! through its one constructor exactly as the app builds its in-process host.
//! Every request maps to one [`TerminalDomain`] call on that host (or, for a
//! spawn, resolving its intent against this host's [`HostEnvironment`], then
//! [`LocalPtyRunner`] + register); nothing here reads a PTY, batches
//! output, owns an emulator terminal, or signals a process.
//!
//! The registry's side effects leave through [`Relay`], the sidecar's one
//! [`TerminalEventSink`]: each event is pushed, unchanged, to every connection
//! that completed the handshake for this instance and asked for events. With
//! no client connected the runtime keeps running and its terminals keep
//! filling; only the pushes stop. A connection's keystrokes arrive as raw
//! bytes on the input channels it opened, and the viewports it attached are
//! detached when it closes. The host's terminal layout lives here with its
//! sessions: every layout verb is one call on the same [`TerminalHost`], and a
//! connection's tab openings are abandoned when it closes.

use std::collections::{HashMap, HashSet};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use codesense_host_proto::{
    agent_cli::NativeLaunch, Capability, Census, EmulatorHello, Frame, Hello, PeerHello,
    FIRST_STREAM_CHANNEL, PROTO_MAJOR, PROTO_MINOR,
};
use codesense_pty_core::domain::{
    PortUnavailable, PtySessionId, RespawnStart, TerminalDomain, TerminalDomainEvent,
    TerminalEventSink, ViewportId,
};
use codesense_pty_core::emulator::{EmulatorConfig, WORKER_BIN};
use codesense_pty_core::layout::{LayoutError, OpenTab, ServerIdentity};
use codesense_pty_core::protocol::{Egress, Message, Reply, Request};
use codesense_pty_core::runner::{LocalPtyRunner, PtyRunner};
use codesense_pty_core::spawn::{HostEnvironment, SpawnIntent};
use codesense_pty_core::term_state::StreamId;
use codesense_pty_core::{SessionRegistration, TerminalHost};

/// How many frames may wait for a connection before it counts as stalled.
pub(crate) const CONNECTION_BACKLOG: usize = 4096;

/// One runtime operation, run on the blocking pool.
// Shared across connections: reverse traffic cannot consume the terminal writer's
// entire generic frame backlog. The writer releases each encoded-byte charge.
static REVERSE_EGRESS_BYTES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
const REVERSE_EGRESS_LIMIT: usize = 2 * 1024 * 1024;
pub(crate) struct QueuedFrame {
    frame: Frame,
    charge: usize,
}
impl std::ops::Deref for QueuedFrame {
    type Target = Frame;
    fn deref(&self) -> &Frame {
        &self.frame
    }
}
impl Drop for QueuedFrame {
    fn drop(&mut self) {
        REVERSE_EGRESS_BYTES.fetch_sub(self.charge, std::sync::atomic::Ordering::AcqRel);
    }
}

type Operation = dyn FnOnce(&Host) -> Result<Reply, PortUnavailable> + Send;

/// How long a typed line's turn may take to show in its agent's activity (its
/// prompt hook) after the Enter.
const SUBMIT_TURN_GRACE: Duration = Duration::from_millis(1500);

/// Whether typed input may reach an agent: native-capable startup first needs
/// observation; a CLI accepting bracketed paste also needs that terminal mode.
/// Exited agents leave a shell, whose input requires neither gate.
fn prompt_takes_input(
    activity: Option<&codesense_pty_core::activity::Snapshot>,
    bracketed: impl FnOnce() -> Option<bool>,
) -> bool {
    // Native-capable CLIs can prove initial readiness through their bound engine.
    // Bracketed paste alone may already be on while a startup menu is showing.
    if activity.is_some_and(|snapshot| {
        snapshot.exit.is_none()
            && !snapshot.observed
            && codesense_pty_core::agent_cli::adapter(snapshot.provider)
                .control()
                .native_readiness
    }) {
        return false;
    }
    !pastes_bracketed(activity) || bracketed() != Some(false)
}

/// How to drive the CLI `activity` shows running (not exited).
fn running_control(
    activity: Option<&codesense_pty_core::activity::Snapshot>,
) -> Option<codesense_pty_core::agent_cli::Control> {
    activity
        .filter(|s| s.exit.is_none())
        .map(|s| codesense_pty_core::agent_cli::adapter(s.provider).control())
}

/// Whether `activity` is a CLI that has not exited and is typed into as one bracketed
/// paste.
fn pastes_bracketed(activity: Option<&codesense_pty_core::activity::Snapshot>) -> bool {
    running_control(activity).is_some_and(|control| control.bracketed_paste)
}

/// Whether `line` is one of the running CLI's own commands (`/model`, `/compact now`),
/// not text that starts like a path (`/tmp/out is ready`).
fn cli_command(line: &str, activity: Option<&codesense_pty_core::activity::Snapshot>) -> bool {
    let Some(prefix) = running_control(activity).and_then(|control| control.command_prefix) else {
        return false;
    };
    let word = line.split_whitespace().next().unwrap_or("");
    word.strip_prefix(prefix).is_some_and(|name| {
        name.starts_with(|c: char| c.is_ascii_alphabetic())
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'))
    })
}

/// Whether an agent idle at turn `generation` of `invocation` has started a turn
/// since: a new turn, a new invocation, or working on one now.
fn turn_started(
    invocation: &str,
    generation: u64,
    now: Option<&codesense_pty_core::activity::Snapshot>,
) -> bool {
    use codesense_pty_core::activity::State;
    now.is_some_and(|s| {
        s.identity.invocation_id != invocation
            || s.turn_generation > generation
            || matches!(
                s.state,
                State::Thinking | State::Working | State::NeedsPermission
            )
    })
}

/// How many writes may wait for one session's input thread.
const INPUT_BACKLOG: usize = 1024;

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A poisoned lock still guards valid data; one panicked thread must not take
    // every surviving session down with it.
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// One client connection: a bounded frame channel drained onto the socket by
/// the connection's writer thread, the connection's output channels, and the
/// input channels it opened. A connection that stops reading is closed rather
/// than buffered for without limit, so its client sees the connection end and
/// reconnects.
#[derive(Clone)]
pub(crate) struct Conn {
    machine_stats: Arc<std::sync::atomic::AtomicBool>,
    activity: Arc<std::sync::atomic::AtomicBool>,
    /// The connection asked for wake outcomes (`WakeOutcomes`).
    wake: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) id: u64,
    out: mpsc::SyncSender<QueuedFrame>,
    socket: Arc<UnixStream>,
    egress: Arc<Mutex<Egress>>,
    inputs: Arc<Mutex<HashMap<u32, PtySessionId>>>,
}

impl Conn {
    pub(crate) fn new(id: u64, out: mpsc::SyncSender<QueuedFrame>, socket: UnixStream) -> Self {
        Self {
            machine_stats: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            activity: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            wake: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            id,
            out,
            socket: Arc::new(socket),
            egress: Arc::default(),
            inputs: Arc::default(),
        }
    }

    /// Queue the frames closing output channels, as `close` yields them.
    fn send_closes(&self, close: impl FnOnce(&mut Egress) -> Result<Vec<Frame>, String>) {
        match close(&mut lock(&self.egress)) {
            Ok(frames) => {
                for frame in frames {
                    self.send(frame);
                }
            }
            Err(error) => log::warn!("connection {}: close not sent: {error}", self.id),
        }
    }

    /// Answer a frame that could not be taken as a call.
    pub(crate) fn reply_unparseable(&self, message: String) {
        self.reply(crate::server::UNPARSEABLE_RID, Err(message));
    }

    pub(crate) fn reply(&self, rid: u64, result: Result<Reply, String>) {
        match Frame::control(&Message::Reply { rid, result }) {
            Ok(frame) => {
                self.send(frame);
            }
            Err(error) => log::warn!("reply {rid} not sent: {error}"),
        }
    }

    /// Queue `event`'s frames. `false` when the connection is gone or was
    /// just closed for falling behind. The frames are queued under the output
    /// channel lock, so a channel's announcement precedes its first batch.
    fn push(&self, event: TerminalDomainEvent) -> bool {
        if matches!(event, TerminalDomainEvent::MachineStats { .. })
            && !self
                .machine_stats
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return true;
        }
        if matches!(event, TerminalDomainEvent::Activity { .. })
            && !self.activity.load(std::sync::atomic::Ordering::Acquire)
        {
            return true;
        }
        if matches!(event, TerminalDomainEvent::Wake { .. })
            && !self.wake.load(std::sync::atomic::Ordering::Acquire)
        {
            return true;
        }
        let mut egress = lock(&self.egress);
        match egress.frames(event) {
            Ok(frames) => frames.into_iter().all(|frame| self.send(frame)),
            Err(error) => {
                // This connection can carry no more output (its channels
                // ran out): close it, so its client reconnects afresh.
                log::warn!("closing connection {}: {error}", self.id);
                let _ = self.socket.shutdown(std::net::Shutdown::Both);
                false
            }
        }
    }

    /// Queue `frame`; never blocks. `false` when the connection is gone or was
    /// just closed for falling behind.
    /// This connection's desktop heartbeats: a read that waits
    /// longer than `silence` ends the connection (a half-open link is reaped
    /// in seconds, not when TCP gives up).
    pub(crate) fn expect_heartbeats(&self, silence: Duration) {
        let _ = self.socket.set_read_timeout(Some(silence));
    }

    /// End this connection (its desktop reconnected on another):
    /// its reader sees the end and the host disconnects it.
    pub(crate) fn close(&self) {
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
    }

    pub(crate) fn send(&self, frame: Frame) -> bool {
        if frame.kind == codesense_host_proto::Kind::MCP_DATA {
            let bytes = frame.payload.len() + 9;
            if REVERSE_EGRESS_BYTES
                .fetch_update(
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Acquire,
                    |used| {
                        used.checked_add(bytes)
                            .filter(|next| *next <= REVERSE_EGRESS_LIMIT)
                    },
                )
                .is_err()
            {
                return false;
            }
        }
        let charge = if frame.kind == codesense_host_proto::Kind::MCP_DATA {
            frame.payload.len() + 9
        } else {
            0
        };
        match self.out.try_send(QueuedFrame { frame, charge }) {
            Ok(()) => true,
            Err(mpsc::TrySendError::Full(_frame)) => {
                log::warn!("connection {} stopped reading; closing it", self.id);
                let _ = self.socket.shutdown(std::net::Shutdown::Both);
                false
            }
            Err(mpsc::TrySendError::Disconnected(_frame)) => false,
        }
    }
}

/// Where one request's reply goes.
struct Responder {
    rid: u64,
    conn: Conn,
}

impl Responder {
    fn send(self, result: Result<Reply, String>) {
        self.conn.reply(self.rid, result);
    }
}

/// Where a queued write's result goes: the connection that sent it, or a
/// wake delivery waiting for it.
enum Answer {
    Conn(Responder),
    Wake(mpsc::Sender<Result<Reply, String>>),
}

impl Answer {
    fn send(self, result: Result<Reply, String>) {
        match self {
            Self::Conn(reply) => reply.send(result),
            Self::Wake(tx) => {
                let _ = tx.send(result);
            }
        }
    }
}

/// One queued write for a session's input thread.
struct WriteJob {
    data: Vec<u8>,
    reply: Answer,
    // Keep ownership until an accepted write has actually settled. A wake's
    // write runs under the claim its delivery holds instead.
    _permit: Option<WritePermit>,
}

/// The sidecar's event sink: pushes every registry event to the subscribed
/// connections, and retires a removed session's input thread.
#[derive(Default)]
struct Relay {
    #[cfg(target_os = "linux")]
    reverse: Mutex<std::sync::Weak<crate::reverse_mcp::ReverseMcp>>,
    subscribers: Mutex<HashMap<u64, Conn>>,
    inputs: Mutex<HashMap<PtySessionId, mpsc::SyncSender<WriteJob>>>,
    writable: Arc<Mutex<WritableClaims>>,
    /// Every attached viewport stream and its session, bound on each listening
    /// connection — including one that starts listening later — so its output
    /// channel closes with the session's output. Lock order: a session's
    /// replay lock (held by the batcher while it emits), then `subscribers`,
    /// then a connection's egress, then `streams`.
    streams: Mutex<HashMap<StreamId, PtySessionId>>,
}

impl TerminalEventSink for Relay {
    fn emit(&self, event: TerminalDomainEvent) -> Result<(), PortUnavailable> {
        match &event {
            TerminalDomainEvent::SessionRemoved { session, .. } => {
                // Dropping the sender ends the input thread once its queue drains.
                lock(&self.inputs).remove(session);
                lock(&self.writable).remove(session);
                self.unbind_session(session);
                #[cfg(target_os = "linux")]
                if let Some(reverse) = lock(&self.reverse).upgrade() {
                    reverse.retire(session.as_str());
                }
            }
            TerminalDomainEvent::Exit { session, .. } => {
                self.unbind_session(session);
                #[cfg(target_os = "linux")]
                if let Some(reverse) = lock(&self.reverse).upgrade() {
                    reverse.retire(session.as_str());
                }
            }
            _ => {}
        }
        let mut delivered = false;
        lock(&self.subscribers).retain(|_, conn| {
            let sent = conn.push(event.clone());
            delivered |= sent;
            sent
        });
        if delivered {
            Ok(())
        } else {
            Err(PortUnavailable(
                "no session server client is listening".to_string(),
            ))
        }
    }
}

impl Relay {
    /// The reverse-MCP broker, when it runs.
    #[cfg(target_os = "linux")]
    fn reverse(&self) -> Option<Arc<crate::reverse_mcp::ReverseMcp>> {
        lock(&self.reverse).upgrade()
    }

    /// Push events to `conn` from now on, its egress knowing every attached
    /// viewport stream.
    fn listen(&self, conn: &Conn) {
        let mut subscribers = lock(&self.subscribers);
        let mut egress = lock(&conn.egress);
        for (stream, session) in lock(&self.streams).iter() {
            egress.bind(*stream, session.clone());
        }
        drop(egress);
        subscribers.insert(conn.id, conn.clone());
    }

    /// `stream` is a viewport of `session`: on every listening connection its
    /// output channel closes with the session's output.
    fn bind_stream(&self, stream: StreamId, session: &PtySessionId) {
        let subscribers = lock(&self.subscribers);
        lock(&self.streams).insert(stream, session.clone());
        for conn in subscribers.values() {
            lock(&conn.egress).bind(stream, session.clone());
        }
    }

    /// `stream`'s viewport detached: close its output channel everywhere.
    fn close_stream(&self, stream: StreamId) {
        let subscribers = lock(&self.subscribers);
        lock(&self.streams).remove(&stream);
        for conn in subscribers.values() {
            conn.send_closes(|egress| Ok(egress.close(stream)?.into_iter().collect()));
        }
    }

    /// `session`'s id left without an exit (a warm handoff renamed it): close
    /// its output channels everywhere.
    fn close_session(&self, session: &PtySessionId) {
        let subscribers = lock(&self.subscribers);
        self.unbind_session(session);
        for conn in subscribers.values() {
            conn.send_closes(|egress| egress.close_session(session));
        }
    }

    /// `session`'s output ended: forget its streams' bindings (each egress
    /// closes their channels itself).
    fn unbind_session(&self, session: &PtySessionId) {
        lock(&self.streams).retain(|_, owner| owner != session);
    }
}

/// `InboxWaitV1`: who hears of a session's inbox notices (see `Host::inbox_audience`).
enum InboxAudience {
    #[cfg(target_os = "linux")]
    Desktop(String),
    #[cfg(target_os = "linux")]
    Nobody,
    Every,
}

impl InboxAudience {
    /// Whether the desktop named `identity` is in it.
    fn admits(&self, identity: Option<&str>) -> bool {
        match self {
            #[cfg(target_os = "linux")]
            Self::Desktop(desktop) => identity == Some(desktop.as_str()),
            #[cfg(target_os = "linux")]
            Self::Nobody => false,
            Self::Every => {
                let _ = identity;
                true
            }
        }
    }
}

/// `InboxWaitV1`: this host's inbox reports, each to its session's own desktop.
struct InboxRoute(std::sync::Weak<Host>);

impl codesense_pty_core::inbox_host::InboxOutlet for InboxRoute {
    fn fate(&self, session: &str, token: &str, fate: codesense_pty_core::inbox_host::Fate) {
        if let Some(host) = self.0.upgrade() {
            host.emit_inbox(session, fate.event(session, token));
        }
    }

    fn waiter(&self, state: codesense_pty_core::domain::InboxWaiterState) {
        if let Some(host) = self.0.upgrade() {
            let session = state.session.as_str().to_string();
            host.emit_inbox(&session, TerminalDomainEvent::InboxWaiter { state });
        }
    }

    fn recover(
        &self,
        session: &str,
        notice: &codesense_pty_core::agent_cli::Notice<'_>,
    ) -> codesense_pty_core::agent_cli::SubmitOutcome {
        use codesense_pty_core::agent_cli::SubmitOutcome;
        let Some(host) = self.0.upgrade() else {
            return SubmitOutcome::RefusedNoEffect;
        };
        if self.has_tools(session) != Some(true) {
            return SubmitOutcome::RefusedNoEffect;
        }
        let Some(wake) = host.wake() else {
            return SubmitOutcome::RefusedNoEffect;
        };
        let desktop = self.audience(session).unwrap_or_default();
        wake.submit_inbox_notice(
            PtySessionId::new(session),
            notice.text,
            codesense_pty_core::session_wake::Originator {
                desktop,
                routine_host: None,
            },
            &|| self.has_tools(session) == Some(true),
        )
    }

    fn audience(&self, session: &str) -> Option<String> {
        match self.0.upgrade()?.inbox_audience(session) {
            #[cfg(target_os = "linux")]
            InboxAudience::Desktop(desktop) => Some(desktop),
            #[cfg(target_os = "linux")]
            InboxAudience::Nobody => Some(String::new()),
            InboxAudience::Every => None,
        }
    }

    fn has_tools(&self, session: &str) -> Option<bool> {
        // A relayed agent has its tools only while a desktop relays them; any other
        // reaches its own desktop's CodeSense directly.
        #[cfg(target_os = "linux")]
        if let Some(reverse) = self.0.upgrade().and_then(|host| host.relay.reverse()) {
            if reverse.binding(session).is_some() {
                return reverse.has_tools(session);
            }
        }
        let host = self.0.upgrade()?;
        let ready = lock(&host.connections).values().any(|held| {
            held.local_mcp_sessions
                .contains(&PtySessionId::new(session))
        });
        Some(ready)
    }
}

/// A session id's spawns in flight, whether a kill arrived for it meanwhile,
/// and the cancels waiting for those spawns to be torn down.
#[derive(Default)]
struct InFlight {
    spawns: usize,
    killed: bool,
    cancels: Vec<Responder>,
}

/// The hosted runtime plus the instance identity it answers the handshake with.
pub struct Host {
    machine_stats_directory: Mutex<Option<PathBuf>>,
    machine_stats_schedule: Mutex<crate::machine_stats::WatchSchedule>,
    machine_stats_changed: Arc<tokio::sync::Notify>,
    machine_stats_started: std::sync::atomic::AtomicBool,
    activity_service: Mutex<Option<codesense_pty_core::activity::ActivityService>>,
    executors: crate::remote_workspace::executor::Executors,
    #[cfg(target_os = "linux")]
    pub(crate) reverse: Mutex<Option<Arc<crate::reverse_mcp::ReverseMcp>>>,
    #[cfg(target_os = "linux")]
    pub(crate) owned_supervisors: Arc<crate::owned_supervisors::OwnedSupervisors>,
    instance_id: String,
    /// When a viewing client connection (one that attached a terminal view
    /// during its life) last closed: until [`LEFTOVER_GRACE`] after it, the
    /// last client's exit reap leaves other apps' sessions alone, as that
    /// app may only be reconnecting. A connection that never viewed a
    /// terminal (a probe, a CLI or MCP call) does not count. Written and read
    /// under the `connections` lock.
    last_client_left: Mutex<Option<Instant>>,
    /// The app (its hello's client id) that opened each session, by
    /// spawn or warm handoff; an id no longer in the registry is pruned when
    /// read. A session opened by an older client has none.
    openers: Mutex<HashMap<PtySessionId, String>>,
    /// When each app (by client id) closed its last connection, until
    /// it connects again. Written and read under the `connections` lock.
    clients_left: Mutex<HashMap<String, Instant>>,
    /// When this host process started, on the wall clock.
    started_at: std::time::SystemTime,
    /// How far the tests moved this host's clock ahead, in ms.
    #[cfg(any(test, feature = "test-support"))]
    clock_ahead_ms: std::sync::atomic::AtomicU64,
    pub(crate) terminal: Arc<TerminalHost>,
    /// What spawns resolve against: this process's environment.
    environment: HostEnvironment,
    relay: Arc<Relay>,
    runtime: tokio::runtime::Handle,
    spawning: Mutex<Spawning>,
    /// What each handshaken connection holds, released when it closes.
    connections: Mutex<HashMap<u64, Held>>,
    writable: Arc<Mutex<WritableClaims>>,
    replacement_root: Option<std::path::PathBuf>,
    private_workspace_root: Mutex<Option<(std::path::PathBuf, u64)>>,
    /// `SessionWakeV1`, once started.
    wake: Mutex<Option<Arc<crate::session_wake::SessionWake>>>,
    /// `RoutineArmV1`, once started (after `SessionWakeV1`).
    arms: Mutex<Option<Arc<crate::routine_arm::RoutineArms>>>,
    /// The agent sessions this host is starting on its own, by
    /// session: `(agent, workspace)`. With every connection's reserved tabs,
    /// the one flight per agent and workspace a launch-if-absent checks and
    /// reserves under this lock.
    agent_flights: Mutex<HashMap<PtySessionId, (String, String)>>,
    /// How long a machine-armed routine's launch is waited for.
    arm_launch_timeout: Mutex<Duration>,
    /// How long a desktop that bound its routine secret (and so
    /// heartbeats) may stay silent before its link is taken as dead.
    liveness: Mutex<Duration>,
    /// Tests: how long a machine-armed routine's spawn stalls first.
    #[cfg(test)]
    pub(crate) arm_spawn_delay: Mutex<Option<Duration>>,
    /// Tests: the shell command a machine-armed routine's agent runs as.
    #[cfg(test)]
    pub(crate) arm_test_command: Mutex<Option<String>>,
    /// Tests: runs inside `finish_tab`, between taking the reservation out
    /// of `openings` and placing the tab.
    #[cfg(test)]
    #[allow(clippy::type_complexity)]
    pub(crate) finish_tab_hook: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// When viewer input last reached each session (ms since the epoch).
    input_seen: Mutex<HashMap<PtySessionId, crate::session_wake::InputStamp>>,
    #[cfg(test)]
    replacement_drain_error: Mutex<Option<String>>,
}

const ATTACHED_ELSEWHERE: &str = "attached elsewhere";

/// How long a machine-armed routine's agent launch is waited for. Past it,
/// the run is `unknown` (it may still start) and a spawn that registers later
/// is stopped.
const ARM_LAUNCH_TIMEOUT: Duration = Duration::from_secs(60);
/// A desktop that bound its routine secret heartbeats every 5 s,
/// but a Mac in App Nap or asleep skips many; only this much silence means
/// its link is dead (a reconnecting desktop closes its older link at once).
pub(crate) const DESKTOP_LIVENESS: Duration = Duration::from_secs(180);

/// What became of a machine-armed routine's spawn, between the thread that
/// waits for it and the task that runs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArmSpawnGate {
    Waiting,
    /// The spawn answered in time.
    Answered,
    /// The wait timed out: a session that registers now is stopped.
    Abandoned,
}

/// What a machine-armed routine's agent is spawned with.
#[cfg(unix)]
struct ArmPrepared {
    spec: codesense_pty_core::runner::PtySpec,
    registration: SessionRegistration,
    writer: Option<codesense_pty_core::activity::Writer>,
    /// The reverse-MCP binding's owner: a desktop's connection, or the host.
    owner: Option<u64>,
}

/// `(agent without revision, workspace)` a tab's tags name, if it runs a
/// saved agent.
fn tab_agent(tags: &codesense_pty_core::layout::Tags) -> Option<(&str, Option<&str>)> {
    let key = tags.get("remoteAgentKey")?;
    Some((
        key.split('@').next().unwrap_or(key),
        tags.get("remoteWorkspaceId").map(String::as_str),
    ))
}

/// Whether a tab tagged `tags` runs `agent` in `workspace` (a tab that names
/// no workspace counts: it could be there).
fn runs_agent(tags: &codesense_pty_core::layout::Tags, agent: &str, workspace: &str) -> bool {
    tab_agent(tags).is_some_and(|(a, ws)| a == agent && ws.is_none_or(|ws| ws == workspace))
}

/// Whether a tab tagged `tags` skips permission checks (a raw `bypass`
/// launch).
fn tab_bypasses(tags: &codesense_pty_core::layout::Tags) -> bool {
    tags.get("remotePermissionMode")
        .is_some_and(|mode| mode.contains("bypass"))
}

#[derive(Default)]
struct WritableClaim {
    protected: bool,
    removed: bool,
    owner: Option<u64>,
    in_flight: usize,
}

#[derive(Default)]
struct WritableClaims {
    sessions: HashMap<PtySessionId, WritableClaim>,
}

impl WritableClaims {
    fn claim(&mut self, session: &PtySessionId, owner: u64) -> Result<(), String> {
        let claim = self.sessions.entry(session.clone()).or_default();
        if claim.owner == Some(owner) {
            return Ok(());
        }
        if claim.owner.is_some() || claim.in_flight != 0 {
            return Err(ATTACHED_ELSEWHERE.into());
        }
        claim.protected = true;
        claim.removed = false;
        claim.owner = Some(owner);
        Ok(())
    }

    fn release(&mut self, session: &PtySessionId, owner: u64) -> Result<(), String> {
        let Some(claim) = self.sessions.get_mut(session) else {
            return Ok(());
        };
        if claim.owner.is_some_and(|held| held != owner) {
            return Err(ATTACHED_ELSEWHERE.into());
        }
        claim.owner = None;
        Ok(())
    }

    fn remove(&mut self, session: &PtySessionId) {
        let Some(claim) = self.sessions.get_mut(session) else {
            return;
        };
        if claim.in_flight == 0 {
            self.sessions.remove(session);
        } else {
            claim.removed = true;
            claim.owner = None;
        }
    }

    fn disconnect(&mut self, owner: u64) {
        for claim in self.sessions.values_mut() {
            if claim.owner == Some(owner) {
                claim.owner = None;
            }
        }
    }
}

/// Accepted work retains the claim without holding a mutex across PTY I/O.
/// A closing connection cannot transfer its session while its writes settle.
struct WritePermit {
    claims: Arc<Mutex<WritableClaims>>,
    session: PtySessionId,
}

impl WritePermit {
    fn acquire(
        claims: &Arc<Mutex<WritableClaims>>,
        session: &PtySessionId,
        owner: u64,
    ) -> Result<Self, String> {
        let mut held = lock(claims);
        let claim = held.sessions.entry(session.clone()).or_default();
        if claim.protected && claim.owner != Some(owner) {
            return Err(if claim.owner.is_some() {
                ATTACHED_ELSEWHERE.into()
            } else {
                "writable attachment required".into()
            });
        }
        claim.in_flight += 1;
        Ok(Self {
            claims: claims.clone(),
            session: session.clone(),
        })
    }
}

impl Drop for WritePermit {
    fn drop(&mut self) {
        let mut held = lock(&self.claims);
        if let Some(claim) = held.sessions.get_mut(&self.session) {
            claim.in_flight -= 1;
            if (!claim.protected || claim.removed) && claim.in_flight == 0 {
                held.sessions.remove(&self.session);
            }
        }
    }
}

/// How long the last client to quit must have been alone, and a
/// non-persisting session without a viewport, before that client's exit reap
/// takes the session as a leftover of an app that crashed or was force-quit.
/// A reconnecting app is back well within it (its reconnect retries at most
/// 30 s apart).
const LEFTOVER_GRACE: Duration = Duration::from_secs(5 * 60);

/// What one connection holds in the host: the viewport streams it attached,
/// and the tabs it reserved whose spawn it has not finished.
#[derive(Default)]
struct Held {
    /// The viewer the connection's hello named.
    viewer: Option<String>,
    /// The secret its desktop accepts the CodeSense MCP of the
    /// agents this host starts with, and the connection to relay it on.
    routine_bind: Option<(String, Conn)>,
    /// The desktop identity its bind named (per install and
    /// person); the viewer stands in for an older desktop.
    desktop: Option<String>,
    viewports: HashSet<StreamId>,
    openings: HashMap<PtySessionId, OpenTab>,
    uncertain_openings: HashSet<PtySessionId>,
    /// Respawns in place it began and has not finished.
    respawns: HashSet<PtySessionId>,
    /// It attached a terminal view at some point: an app, not a probe.
    viewed: bool,
    /// The app process its hello named (`client_id`), the same on
    /// every connection that app makes.
    client: Option<String>,
    /// Initialized local MCP links reported by this app connection, cleared with it.
    local_mcp_sessions: HashSet<PtySessionId>,
}

impl Held {
    /// Who this connection's desktop is, for handing agents back.
    fn identity(&self) -> Option<String> {
        self.desktop.clone().or_else(|| self.viewer.clone())
    }
}

/// The emulator worker shipped beside the host binary at `binary`. Both carry
/// the same suffix: none where installed (the app bundle, a remote machine's
/// version directory), `-<target triple>` among the build artifacts.
pub fn emulator_worker_beside(binary: &Path) -> PathBuf {
    let name = binary
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let suffix = name
        .strip_prefix(crate::install::BINARY)
        .unwrap_or_default();
    binary.with_file_name(format!("{WORKER_BIN}{suffix}"))
}

/// The directory the activity socket lives in. A private-account server's root
/// is `/proc/self/fd/N`, its pinned root directory: a symlink, which the
/// activity service rightly refuses as a root, and a path that means nothing
/// to the agent hooks (other processes) the socket path is handed to. Name the
/// real directory instead, once it is checked to be the very one the fd holds
/// (without this, no agent on a private-account machine was ever
/// observed, so every wake there ended `unobservable` and no timer could be set).
fn activity_root(root: &Path) -> Result<PathBuf, String> {
    if !root.starts_with("/proc/self/fd") {
        return Ok(root.to_path_buf());
    }
    let real = std::fs::read_link(root).map_err(|error| format!("activity root: {error}"))?;
    if !real.is_absolute() || !same_directory(root, &real) {
        return Err("activity root is not the pinned root directory".into());
    }
    Ok(real)
}

/// Whether `pinned` (a `/proc/self/fd/N` root) and `named` are still one
/// directory.
fn same_directory(pinned: &Path, named: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(pinned), std::fs::symlink_metadata(named)) {
        (Ok(a), Ok(b)) => b.is_dir() && a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

fn wake_unavailable() -> PortUnavailable {
    PortUnavailable("session wake is not running on this terminal server".into())
}

fn arms_unavailable() -> PortUnavailable {
    PortUnavailable("routine arms are not running on this terminal server".into())
}

/// A layout verb's answer: its reply, or the layout's refusal whole.
fn layout(result: Result<Reply, LayoutError>) -> Result<Reply, PortUnavailable> {
    Ok(result.unwrap_or_else(|error| Reply::LayoutRefused { error }))
}

/// Spawn bookkeeping for kills that race a spawn: spawns in flight per id, and
/// ids cancelled before any spawn of theirs was read.
#[derive(Default)]
struct Spawning {
    in_flight: HashMap<String, InFlight>,
    cancelled: HashMap<String, Instant>,
    draining: bool,
}

/// How long a cancel stands for a spawn that has not been read yet: longer
/// than any client waits for a spawn reply.
const CANCEL_WINDOW: Duration = Duration::from_secs(60);

impl Host {
    /// Host a fresh terminal runtime for the instance `instance_id`, whose
    /// terminal server is `identity`, resolving spawns against `environment`.
    /// `runtime` runs the registry's blocking work (reaps) and every request.
    pub(crate) fn wake_budget_allowed(&self, session: &PtySessionId) -> bool {
        self.terminal
            .activity()
            .inbox()
            .wake_allowed(session.as_str())
    }

    pub fn require_wake_holds(&self) {
        self.terminal.activity().inbox().require_wake_holds();
    }

    pub fn new(
        instance_id: String,
        identity: ServerIdentity,
        environment: HostEnvironment,
        runtime: tokio::runtime::Handle,
    ) -> Arc<Self> {
        let sink_runtime = tokio::runtime::Handle::clone(&runtime);
        Self::build(instance_id, environment, runtime, None, move |relay| {
            TerminalHost::new(identity, relay, sink_runtime)
        })
    }

    #[cfg(any(target_os = "linux", test))]
    pub fn new_replaceable(
        instance_id: String,
        identity: ServerIdentity,
        environment: HostEnvironment,
        runtime: tokio::runtime::Handle,
        root: std::path::PathBuf,
    ) -> Arc<Self> {
        let sink_runtime = tokio::runtime::Handle::clone(&runtime);
        Self::build(
            instance_id,
            environment,
            runtime,
            Some(root),
            move |relay| TerminalHost::new(identity, relay, sink_runtime),
        )
    }

    fn build(
        instance_id: String,
        environment: HostEnvironment,
        runtime: tokio::runtime::Handle,
        replacement_root: Option<std::path::PathBuf>,
        terminal: impl FnOnce(Arc<Relay>) -> TerminalHost,
    ) -> Arc<Self> {
        let relay = Arc::new(Relay::default());
        let terminal = Arc::new(terminal(relay.clone()));
        // A connection drops what it falls behind on, so an exit can be lost:
        // a session used as if it still ran says again that it ended.
        terminal.redeliver_exits_on_use();
        let writable = relay.writable.clone();
        // `InboxWaitV1`: each session's notices go to its own desktop.
        Arc::new_cyclic(|me| {
            terminal
                .activity()
                .set_inbox_outlet(Arc::new(InboxRoute(me.clone())));
            Self {
                machine_stats_directory: Mutex::default(),
                machine_stats_schedule: Mutex::default(),
                machine_stats_changed: Arc::default(),
                machine_stats_started: std::sync::atomic::AtomicBool::new(false),
                activity_service: Mutex::new(None),
                executors: Default::default(),
                #[cfg(target_os = "linux")]
                reverse: Mutex::new(None),
                #[cfg(target_os = "linux")]
                owned_supervisors: Default::default(),
                instance_id,
                last_client_left: Mutex::new(None),
                openers: Mutex::default(),
                clients_left: Mutex::default(),
                started_at: std::time::SystemTime::now(),
                #[cfg(any(test, feature = "test-support"))]
                clock_ahead_ms: std::sync::atomic::AtomicU64::new(0),
                terminal,
                environment,
                relay,
                runtime,
                spawning: Mutex::new(Spawning::default()),
                connections: Mutex::default(),
                writable,
                replacement_root,
                private_workspace_root: Mutex::new(None),
                wake: Mutex::new(None),
                arms: Mutex::new(None),
                agent_flights: Mutex::default(),
                arm_launch_timeout: Mutex::new(ARM_LAUNCH_TIMEOUT),
                liveness: Mutex::new(DESKTOP_LIVENESS),
                #[cfg(test)]
                arm_spawn_delay: Mutex::new(None),
                #[cfg(test)]
                arm_test_command: Mutex::new(None),
                #[cfg(test)]
                finish_tab_hook: Mutex::new(None),
                input_seen: Mutex::default(),
                #[cfg(test)]
                replacement_drain_error: Mutex::new(None),
            }
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn machine_stats_is_watched(&self) -> bool {
        lock(&self.machine_stats_schedule).deadline().is_some()
    }

    fn machine_stats_sample(
        &self,
    ) -> Result<codesense_host_proto::machine_stats::MachineStats, PortUnavailable> {
        let workspace = lock(&self.machine_stats_directory)
            .clone()
            .or_else(|| {
                lock(&self.private_workspace_root)
                    .as_ref()
                    .map(|(root, _)| root.clone())
            })
            .or_else(|| self.replacement_root.clone())
            .or_else(dirs::home_dir)
            .ok_or_else(|| PortUnavailable("workspace directory unavailable".into()))?;
        // A private root may be a pinned descriptor. Label its real path for people.
        // A normally reaped worktree still has an existing ancestor on its volume.
        let workspace = workspace
            .ancestors()
            .find(|path| path.is_dir())
            .ok_or_else(|| PortUnavailable("workspace directory unavailable".into()))?;
        let workspace =
            std::fs::canonicalize(workspace).map_err(|error| PortUnavailable(error.to_string()))?;
        crate::machine_stats::sample(&workspace).map_err(PortUnavailable)
    }

    fn start_machine_stats(self: &Arc<Self>) {
        if self
            .machine_stats_started
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        let host = Arc::downgrade(self);
        let changed = self.machine_stats_changed.clone();
        self.runtime.spawn(async move {
            loop {
                let notified = changed.notified();
                let Some(owner) = host.upgrade() else { break; };
                let deadline = lock(&owner.machine_stats_schedule).deadline();
                drop(owner);
                match deadline {
                    None => notified.await,
                    Some(at) => tokio::select! {
                        () = notified => {},
                        () = tokio::time::sleep_until(at.into()) => {
                            let Some(owner) = host.upgrade() else { break; };
                            if lock(&owner.machine_stats_schedule).take_due(Instant::now()) {
                                let _ = tokio::task::spawn_blocking(move || {
                                    match owner.machine_stats_sample() {
                                        Ok(sample) => { let _ = owner.relay.emit(TerminalDomainEvent::MachineStats { sample }); }
                                        Err(error) => log::debug!("machine stats: {error}"),
                                    }
                                }).await;
                            }
                        }
                    }
                }
            }
        });
    }

    pub fn start_activity(&self, root: &Path) -> Result<(), String> {
        let mut service = lock(&self.activity_service);
        if service.is_some() {
            return Ok(());
        }
        let real = activity_root(root)?;
        // A private-account root (a pinned fd) holds an install that refuses
        // any file with a second hard link: the helper there is symbolic.
        let link = if real.as_path() == root {
            codesense_pty_core::activity::HelperLink::Hard
        } else {
            codesense_pty_core::activity::HelperLink::Symbolic
        };
        let started = codesense_pty_core::activity::ActivityService::start_with(
            &real,
            self.terminal.activity(),
            link,
        )
        .map_err(|error| error.to_string())?;
        // A pinned root renamed or swapped between the check and the bind:
        // the socket must still be in the very directory the fd holds.
        if real.as_path() != root && !same_directory(root, &real) {
            drop(started);
            return Err("activity root moved while its socket was bound".into());
        }
        *service = Some(started);
        Ok(())
    }

    /// Start `SessionWakeV1`, keeping its store under `root`. Needs the
    /// activity owner (always present) for idle; idempotent.
    pub fn start_wake(self: &Arc<Self>, root: &Path) -> Result<(), String> {
        self.start_wake_with(root, Arc::new(crate::session_wake::SystemClock))
    }

    pub(crate) fn start_wake_with(
        self: &Arc<Self>,
        root: &Path,
        clock: Arc<dyn crate::session_wake::Clock>,
    ) -> Result<(), String> {
        let mut slot = lock(&self.wake);
        if slot.is_some() {
            return Ok(());
        }
        let port = Arc::new(crate::session_wake::host_port::HostWakePort::new(
            Arc::downgrade(self),
        ));
        let wake = crate::session_wake::SessionWake::open(
            port,
            clock,
            Arc::new(crate::session_wake::ThreadRunner),
            self.terminal.activity().host_boot_id().to_string(),
            Some(root.join("wake")),
        );
        crate::session_wake::start_ticker(Arc::downgrade(&wake))
            .map_err(|error| format!("session wake ticker: {error}"))?;
        *slot = Some(wake);
        Ok(())
    }

    /// `SessionWakeV1`'s queue and timers, once started.
    pub fn wake(&self) -> Option<Arc<crate::session_wake::SessionWake>> {
        lock(&self.wake).clone().filter(|wake| wake.healthy())
    }

    /// Start `RoutineArmV1`, keeping its store under `root`. Needs
    /// `SessionWakeV1` (wakes are delivered through it); idempotent.
    ///
    /// A terminal server without reverse MCP (macOS) runs no routines without
    /// a desktop: an agent it started could not report its run done. It
    /// says so, and refuses every arm with that reason.
    pub fn start_arms(self: &Arc<Self>, root: &Path) -> Result<(), String> {
        let unsupported = (!cfg!(target_os = "linux")).then(|| {
            "routines run without a desktop only on Linux machines: on this machine an agent the routine started could not report its run done (routine_done needs reverse MCP, which macOS terminal servers do not run yet). Arm it on this Mac or on a Linux machine.".to_string()
        });
        self.start_arms_on(
            root,
            Arc::new(crate::session_wake::SystemClock),
            unsupported,
        )
    }

    #[cfg(test)]
    pub(crate) fn start_arms_with(
        self: &Arc<Self>,
        root: &Path,
        clock: Arc<dyn crate::session_wake::Clock>,
    ) -> Result<(), String> {
        self.start_arms_on(root, clock, None)
    }

    pub(crate) fn start_arms_on(
        self: &Arc<Self>,
        root: &Path,
        clock: Arc<dyn crate::session_wake::Clock>,
        unsupported: Option<String>,
    ) -> Result<(), String> {
        let mut slot = lock(&self.arms);
        if slot.is_some() {
            return Ok(());
        }
        if self.wake().is_none() {
            return Err("routine arms need session wakes".into());
        }
        let port = Arc::new(crate::routine_arm::host_port::HostArmPort::new(
            Arc::downgrade(self),
            root.to_path_buf(),
        ));
        let arms = crate::routine_arm::RoutineArms::open_with(
            port,
            clock,
            self.terminal.activity().host_boot_id().to_string(),
            Some(root.join("routine-arms")),
            unsupported,
            crate::routine_arm::STARTING_WAIT,
        );
        crate::routine_arm::start_ticker(Arc::downgrade(&arms))
            .map_err(|error| format!("routine arm ticker: {error}"))?;
        *slot = Some(arms);
        Ok(())
    }

    /// `RoutineArmV1`'s armed routines, once started.
    pub fn arms(&self) -> Option<Arc<crate::routine_arm::RoutineArms>> {
        lock(&self.arms).clone().filter(|arms| arms.healthy())
    }

    /// The live tabs of `target`'s agent in its workspace, launched
    /// by its owner (an untagged tab counts as the owner's: the private
    /// terminal server admits only its enrolled owner), and the ones being
    /// started there (by this host, or reserved by a connection) as busy. A
    /// tab that skips permission checks is a candidate only when the agent
    /// set `allowWake`. An arm whose workspace is not found yet
    /// has none.
    pub(crate) fn arm_candidates(
        &self,
        target: &codesense_pty_core::routine_arm::ArmTarget,
    ) -> Vec<crate::routine_arm::ArmCandidate> {
        if target.workspace_id == codesense_pty_core::routine_arm::UNPREPARED_WORKSPACE {
            return Vec::new();
        }
        let owned = |tags: &codesense_pty_core::layout::Tags| {
            runs_agent(tags, &target.agent, &target.workspace_id)
                && match (&target.owner, tags.get("remoteOwnerId")) {
                    (Some(owner), Some(launched_by)) => owner == launched_by,
                    _ => true,
                }
                && (target.allow_wake || !tab_bypasses(tags))
        };
        let Ok(entries) = self.terminal.tab_entries() else {
            return Vec::new();
        };
        let mut candidates: Vec<crate::routine_arm::ArmCandidate> = entries
            .into_iter()
            .enumerate()
            .filter(|(_, (_, tab))| !tab.state.is_exited())
            .filter(|(_, (_, tab))| owned(&tab.tags))
            .filter(|(_, (_, tab))| self.wake_session_live(&tab.session))
            .map(|(index, (_, tab))| {
                let idle = self
                    .wake_activity(&tab.session)
                    .is_some_and(|s| s.takes_input());
                crate::routine_arm::ArmCandidate {
                    session: tab.session,
                    idle,
                    recency: index as u64,
                    starting: false,
                }
            })
            .collect();
        let mut starting: Vec<PtySessionId> = lock(&self.agent_flights)
            .iter()
            .filter(|(_, (agent, workspace))| {
                *agent == target.agent && *workspace == target.workspace_id
            })
            .map(|(session, _)| session.clone())
            .collect();
        starting.extend(
            lock(&self.connections)
                .values()
                .flat_map(|held| held.openings.values())
                .filter(|open| owned(&open.tags))
                .map(|open| open.session.clone()),
        );
        for session in starting {
            if !candidates.iter().any(|c| c.session == session) {
                candidates.push(crate::routine_arm::ArmCandidate {
                    session,
                    idle: false,
                    recency: u64::MAX,
                    starting: true,
                });
            }
        }
        candidates
    }

    /// A session of `agent` running or being started in
    /// `workspace`, by anyone: a live tab, a tab a connection reserved, or a
    /// launch of this host's. The caller holds `flights` (the single flight's
    /// lock) across this check and its own reservation.
    fn agent_session_in(
        &self,
        flights: &HashMap<PtySessionId, (String, String)>,
        agent: &str,
        workspace: &str,
    ) -> Option<PtySessionId> {
        if let Some((session, _)) = flights
            .iter()
            .find(|(_, (a, ws))| a == agent && ws == workspace)
        {
            return Some(session.clone());
        }
        if let Some(open) = lock(&self.connections)
            .values()
            .flat_map(|held| held.openings.values())
            .find(|open| runs_agent(&open.tags, agent, workspace))
        {
            return Some(open.session.clone());
        }
        self.terminal
            .tab_entries()
            .ok()?
            .into_iter()
            .map(|(_, tab)| tab)
            .find(|tab| {
                !tab.state.is_exited()
                    && runs_agent(&tab.tags, agent, workspace)
                    && self.wake_session_live(&tab.session)
            })
            .map(|tab| tab.session)
    }

    /// Start a machine-armed routine's agent as `session`, in a new
    /// tab, from the launch recipe a desktop prepared. `if_absent`: only when
    /// no session of the agent runs or is being started in the workspace,
    /// checked and reserved in one step with every other launcher's
    /// launch-if-absent ([`LaunchFailure::Busy`] names the one found).
    ///
    /// Its CodeSense MCP is relayed by the desktop that manages the routine
    /// when it is connected (the full tool set), else answered on the
    /// machine (`routine_done`, `signal_done`, `desktop_offline`).
    ///
    /// A spawn not answered within the launch timeout is `unknown` (R5):
    /// its session stays in the single flight until the spawn ends, and one
    /// that registers after the timeout is stopped.
    #[cfg(unix)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn arm_launch(
        self: &Arc<Self>,
        target: &codesense_pty_core::routine_arm::ArmTarget,
        recipe: &codesense_pty_core::routine_arm::ArmLaunch,
        session: &PtySessionId,
        prompt: &str,
        config_dir: &Path,
        if_absent: bool,
        desktop: &str,
    ) -> Result<(), crate::routine_arm::LaunchFailure> {
        use crate::routine_arm::LaunchFailure;
        if lock(&self.spawning).draining {
            return Err(LaunchFailure::NotStarted(
                "terminal server is draining".into(),
            ));
        }
        // Architecture: autonomous launches share the removal admission fence.
        let admission = codesense_pty_core::remote_workspace::removal::admit_spawn(&target.cwd)
            .map_err(LaunchFailure::NotStarted)?;
        let mut tags = recipe.tags.clone();
        tags.insert(
            codesense_pty_core::routine_arm::TAG_ROUTINE_RUN_SESSION.into(),
            session.as_str().to_string(),
        );
        let open = OpenTab {
            viewer: codesense_pty_core::domain::ViewerId::new("routine"),
            workspace: recipe.workspace.clone(),
            session: session.clone(),
            descriptor: recipe.descriptor.clone(),
            name: None,
            role: None,
            tags,
            placement: codesense_pty_core::layout::Placement::Tab,
            index: None,
            activate: false,
        };
        {
            // One flight per agent and workspace: the check and the
            // reservation are one step against every launcher.
            let mut flights = lock(&self.agent_flights);
            if if_absent {
                if let Some(running) =
                    self.agent_session_in(&flights, &target.agent, &target.workspace_id)
                {
                    return Err(LaunchFailure::Busy(running));
                }
            }
            self.terminal
                .reserve_tab(&open)
                .map_err(|error| LaunchFailure::NotStarted(error.to_string()))?;
            flights.insert(
                session.clone(),
                (target.agent.clone(), target.workspace_id.clone()),
            );
        }
        let prepared = match self.arm_prepare(target, recipe, session, prompt, config_dir, desktop)
        {
            Ok(prepared) => prepared,
            Err(error) => {
                let detail = match &error {
                    LaunchFailure::NotStarted(e) | LaunchFailure::Unknown(e) => e.clone(),
                    LaunchFailure::Busy(s) => s.to_string(),
                };
                let _ = self.terminal.finish_tab(open, Err(detail));
                lock(&self.agent_flights).remove(session);
                return Err(error);
            }
        };
        let gate = Arc::new(Mutex::new(ArmSpawnGate::Waiting));
        let (answer, answered) = mpsc::channel();
        let host = self.clone();
        let task_gate = gate.clone();
        let task_session = session.clone();
        #[cfg(test)]
        let delay = *lock(&self.arm_spawn_delay);
        // The arm ticker is a thread of its own: it waits on the spawn the
        // host runtime runs.
        self.runtime.spawn(async move {
            let _admission = admission;
            #[cfg(test)]
            if let Some(delay) = delay {
                std::thread::sleep(delay);
            }
            let ArmPrepared {
                spec,
                registration,
                writer,
                owner,
            } = prepared;
            let terminal = host.terminal.clone();
            let result = async {
                let pty = LocalPtyRunner.spawn(spec).await.map_err(String::from)?;
                let pid = pty.pid();
                terminal.register(pty, registration).map(|()| pid)
            }
            .await;
            let abandoned = {
                let mut gate = lock(&task_gate);
                let abandoned = *gate == ArmSpawnGate::Abandoned;
                if !abandoned {
                    *gate = ArmSpawnGate::Answered;
                }
                abandoned
            };
            let outcome = host.arm_settle(open, &task_session, result, abandoned, writer, owner);
            lock(&host.agent_flights).remove(&task_session);
            let _ = answer.send(outcome);
        });
        let timeout = *lock(&self.arm_launch_timeout);
        match answered.recv_timeout(timeout) {
            Ok(outcome) => outcome,
            Err(_) => {
                let mut state = lock(&gate);
                if *state == ArmSpawnGate::Answered {
                    // It answered at the very moment the wait ran out.
                    drop(state);
                    return answered.recv().unwrap_or_else(|_| {
                        Err(LaunchFailure::Unknown(
                            "the agent's launch ended without an answer".into(),
                        ))
                    });
                }
                *state = ArmSpawnGate::Abandoned;
                Err(LaunchFailure::Unknown(format!(
                    "the agent did not start within {} s; it may still be starting, and is stopped if it does",
                    timeout.as_secs()
                )))
            }
        }
    }

    /// The spec, registration, activity writer and MCP binding a
    /// machine-armed routine's agent starts with.
    #[cfg(unix)]
    fn arm_prepare(
        self: &Arc<Self>,
        target: &codesense_pty_core::routine_arm::ArmTarget,
        recipe: &codesense_pty_core::routine_arm::ArmLaunch,
        session: &PtySessionId,
        prompt: &str,
        config_dir: &Path,
        desktop: &str,
    ) -> Result<ArmPrepared, crate::routine_arm::LaunchFailure> {
        use crate::routine_arm::LaunchFailure;
        let not_started = LaunchFailure::NotStarted;
        #[cfg(test)]
        if let Some(command) = lock(&self.arm_test_command).clone() {
            let _ = (recipe, prompt, config_dir, desktop);
            return Ok(ArmPrepared {
                spec: crate::testing::sh(&command).resolve(session.as_str(), &self.environment),
                registration: crate::testing::registration(session.as_str(), None, false),
                writer: None,
                owner: None,
            });
        }
        let intent = SpawnIntent {
            remote_launch: Some(recipe.launch),
            command: None,
            argv: None,
            shell: None,
            cwd: target.cwd.clone(),
            cols: 80,
            rows: 24,
            shell_integration: false,
            starship_preset: None,
            api_port: None,
            env_remove: vec![],
            env: vec![],
            agent_env: Vec::new(),
            login_path: None,
        }
        .with_agent_startup(Some(&recipe.agent_startup));
        let mut spec = intent.resolve(session.as_str(), &self.environment);
        let mut startup = recipe.agent_startup.clone();
        startup.prompt = Some(prompt.to_string());
        #[cfg(target_os = "linux")]
        let reverse = lock(&self.reverse).clone();
        #[cfg(not(target_os = "linux"))]
        let reverse: Option<()> = None;
        let owner: Option<u64> = match &reverse {
            #[cfg(target_os = "linux")]
            Some(reverse) => {
                // Relayed by the desktop that manages the routine while it is
                // connected (its full CodeSense MCP), else answered here.
                let owner = match self.routine_binder(desktop) {
                    Some((secret, conn)) => {
                        let nonce = codesense_pty_core::routine_arm::routine_binding_nonce(
                            &secret,
                            session.as_str(),
                        );
                        reverse
                            .provision_agent(
                                &conn,
                                session,
                                nonce,
                                codesense_host_proto::reverse_mcp::default_servers(),
                                Vec::new(),
                                recipe.launch,
                                Some(startup),
                            )
                            .map_err(not_started)?;
                        reverse.set_desktop(session.as_str(), self.identity_of(conn.id));
                        conn.id
                    }
                    None => {
                        reverse
                            .provision_host(session, recipe.launch, startup, desktop)
                            .map_err(not_started)?;
                        crate::reverse_mcp::HOST_OWNER
                    }
                };
                spec = reverse
                    .prepare(owner, session.as_str(), spec, Some(recipe.launch))
                    .map_err(|error| {
                        reverse.release(owner, session.as_str());
                        not_started(error)
                    })?;
                Some(owner)
            }
            #[cfg(not(target_os = "linux"))]
            Some(()) => None,
            None => {
                let _ = desktop;
                // An empty projection the agent CLI is pointed at.
                std::fs::create_dir_all(config_dir).map_err(|e| not_started(e.to_string()))?;
                let config = config_dir.join(format!("{}.json", session.as_str()));
                std::fs::write(&config, br#"{"mcpServers":{}}"#)
                    .map_err(|e| not_started(e.to_string()))?;
                crate::native_launch::prepare_agent(
                    &mut spec,
                    recipe.launch,
                    &config,
                    Some(&startup),
                    session.as_str(),
                )
                .map_err(not_started)?;
                None
            }
        };
        let provider = recipe.launch.cli();
        // The one status pipe (ActivityV2), as for any launch here: the host
        // is named, and the agent's own invocation reports through it.
        if let Some((name, root)) = lock(&self.activity_service)
            .as_ref()
            .and_then(|service| service.shell_env())
        {
            spec.command.env(name, root);
        }
        let writer = provider.and_then(|provider| {
            let socket = lock(&self.activity_service)
                .as_ref()
                .map(|service| service.path().to_string_lossy().into_owned())?;
            self.terminal
                .activity()
                .prepare_native(session.as_str(), provider, &socket)
                .ok()
        });
        if let Some(writer) = &writer {
            spec.command
                .env(codesense_pty_core::activity::SOCKET_ENV, &writer.socket);
            spec.command
                .env(codesense_pty_core::activity::TOKEN_ENV, &writer.token);
            crate::native_launch::report_inbox_update(
                &spec,
                self.terminal.activity(),
                &writer.token,
            );
        }
        let mut registration = SessionRegistration {
            session_id: session.as_str().to_string(),
            meta: Default::default(),
            home_cwd: Some(std::path::PathBuf::from(&target.cwd)),
            osc_enabled: false,
            is_pool_member: false,
            scrollback: codesense_pty_core::emulator::DEFAULT_SCROLLBACK,
        };
        registration.meta.workspace_id = Some(recipe.workspace.as_str().to_string());
        Ok(ArmPrepared {
            spec,
            registration,
            writer,
            owner,
        })
    }

    /// Settle a machine-armed routine's spawn: place its tab, or stop it (it
    /// registered after its launch was given up on, or its tab cannot be
    /// placed), or release what was prepared for it.
    #[cfg(unix)]
    fn arm_settle(
        &self,
        open: OpenTab,
        session: &PtySessionId,
        result: Result<Option<u32>, String>,
        abandoned: bool,
        writer: Option<codesense_pty_core::activity::Writer>,
        owner: Option<u64>,
    ) -> Result<(), crate::routine_arm::LaunchFailure> {
        use crate::routine_arm::LaunchFailure;
        #[cfg(target_os = "linux")]
        let reverse = lock(&self.reverse).clone();
        let release = |writer: Option<codesense_pty_core::activity::Writer>| {
            if let Some(writer) = writer {
                self.terminal
                    .activity()
                    .remove(session.as_str(), &writer.identity.session_incarnation);
            }
            #[cfg(target_os = "linux")]
            if let (Some(reverse), Some(owner)) = (&reverse, owner) {
                reverse.release(owner, session.as_str());
            }
            #[cfg(not(target_os = "linux"))]
            let _ = owner;
        };
        match result {
            Ok(_) if abandoned => {
                let _ = self.terminal.kill(session);
                let _ = self.terminal.finish_tab(
                    open,
                    Err("the routine's launch timed out; the late session was stopped".into()),
                );
                release(writer);
                Err(LaunchFailure::Unknown(
                    "the agent started after its launch timed out, and was stopped".into(),
                ))
            }
            Ok(pid) => {
                #[cfg(target_os = "linux")]
                if let (Some(reverse), Some(owner), Some(pid)) = (&reverse, owner, pid) {
                    if let Err(error) = reverse.commit(owner, session.as_str(), pid) {
                        log::warn!("routine agent {session}: MCP binding not committed: {error}");
                    }
                }
                #[cfg(not(target_os = "linux"))]
                let _ = pid;
                match self.terminal.finish_tab(open, Ok(())) {
                    Ok(_) => Ok(()),
                    // It runs but has no tab: end it rather than leave it unseen.
                    Err(error) => {
                        let _ = self.terminal.kill(session);
                        Err(LaunchFailure::Unknown(format!(
                            "the agent started but its tab could not be placed: {error}"
                        )))
                    }
                }
            }
            Err(error) => {
                release(writer);
                let _ = self.terminal.finish_tab(open, Err(error.clone()));
                Err(LaunchFailure::NotStarted(error))
            }
        }
    }

    /// The connection of `desktop` (the viewer that manages a
    /// routine) that gave this host its routine binding secret, if connected.
    #[cfg(target_os = "linux")]
    fn routine_binder(&self, desktop: &str) -> Option<(String, Conn)> {
        lock(&self.connections)
            .values()
            .filter(|held| held.identity().as_deref() == Some(desktop))
            .find_map(|held| held.routine_bind.clone())
    }

    /// Remember the routine binding secret connection `conn`'s
    /// desktop gave.
    fn set_routine_bind(
        &self,
        conn: &Conn,
        secret: String,
        desktop: Option<String>,
    ) -> Result<(), String> {
        if secret.len() != 64 || !secret.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("a routine binding secret is 64 hex digits".into());
        }
        if desktop.as_ref().is_some_and(|desktop| {
            desktop.is_empty()
                || desktop.len() > 128
                || !desktop.bytes().all(|b| b.is_ascii_graphic())
        }) {
            return Err("a desktop identity is 1-128 printable characters".into());
        }
        let named = desktop.is_some();
        let (identity, older, viewer) = {
            let mut connections = lock(&self.connections);
            let held = connections
                .get_mut(&conn.id)
                .ok_or("the connection closed")?;
            held.routine_bind = Some((secret.clone(), conn.clone()));
            if desktop.is_some() {
                held.desktop = desktop.clone();
            }
            let identity = held.identity();
            let viewer = held.viewer.clone();
            // The same desktop, by the identity it named, on an
            // older link: that link is stale (the desktop reconnected), so
            // it is closed now rather than waiting for its silence.
            let older: Vec<Conn> = match &desktop {
                Some(desktop) => connections
                    .iter()
                    .filter(|(id, other)| {
                        **id != conn.id && other.desktop.as_ref() == Some(desktop)
                    })
                    .filter_map(|(_, other)| other.routine_bind.as_ref().map(|(_, c)| c.clone()))
                    .collect(),
                None => Vec::new(),
            };
            (identity, older, viewer)
        };
        for stale in older {
            log::info!(
                "connection {}: its desktop reconnected as {}; closing the older link",
                stale.id,
                conn.id
            );
            stale.close();
        }
        // What reached this host before the desktop named itself
        // (its arms, timers, its routines' sessions and their queued calls)
        // was stamped with its viewer: it is the named desktop's now.
        if let (Some(desktop), Some(viewer)) = (&desktop, &viewer) {
            if let Some((host_part, _)) = desktop.rsplit_once('.') {
                self.restamp(viewer, host_part, desktop);
            }
        }
        // This desktop heartbeats; a link silent far past them is
        // dead, and its agents go to a desktop that is there.
        conn.expect_heartbeats(*lock(&self.liveness));
        // The agents this desktop relayed before it went away (or whose
        // routines it manages) get their CodeSense MCP back.
        // The broker is taken out of its lock first: naming the desktop reports inbox
        // states, which read who hears them.
        #[cfg(target_os = "linux")]
        {
            let reverse = lock(&self.reverse).clone();
            if let (Some(identity), Some(reverse)) = (identity, reverse) {
                reverse.name_desktop(conn.id, &identity);
                reverse.reattach(&identity, named, &secret, conn);
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = (identity, named);
        Ok(())
    }

    /// Stamp what `viewer` stood for, from install `host_part`,
    /// with `identity`.
    fn restamp(&self, viewer: &str, host_part: &str, identity: &str) {
        let arms = self.arms();
        if let Some(arms) = &arms {
            arms.restamp(viewer, host_part, identity);
        }
        if let Some(wake) = self.wake() {
            wake.restamp(viewer, host_part, identity);
        }
        #[cfg(target_os = "linux")]
        if let Some(reverse) = lock(&self.reverse).clone() {
            // A session's install: the arm whose run started or woke it.
            let mut installs = HashMap::new();
            if let Some(arms) = &arms {
                let hosts: HashMap<String, Option<String>> = arms
                    .list()
                    .armed
                    .into_iter()
                    .map(|info| (info.key, info.originator.routine_host))
                    .collect();
                use codesense_pty_core::routine_arm::ArmRunStatus;
                for run in arms.runs(0).runs {
                    let session = match run.status {
                        ArmRunStatus::Launched { session, .. }
                        | ArmRunStatus::Delivered { session, .. }
                        | ArmRunStatus::Queued { session, .. } => session,
                        _ => continue,
                    };
                    if let Some(host) = hosts.get(&run.key) {
                        installs.insert(session.as_str().to_string(), host.clone());
                    }
                }
            }
            reverse.restamp(viewer, host_part, identity, &|session| {
                installs.get(session).cloned().flatten()
            });
        }
    }

    /// Tests: how long a bound desktop may stay silent.
    #[cfg(test)]
    pub(crate) fn set_liveness(&self, silence: Duration) {
        *lock(&self.liveness) = silence;
    }

    /// Tests: how long a machine-armed routine's launch is waited for.
    #[cfg(test)]
    pub(crate) fn set_arm_launch_timeout(&self, timeout: Duration) {
        *lock(&self.arm_launch_timeout) = timeout;
    }

    /// Who connection `id`'s desktop is.
    fn identity_of(&self, id: u64) -> Option<String> {
        lock(&self.connections).get(&id).and_then(Held::identity)
    }

    /// Viewer input `bytes` reached `session` now. Input that can leave text
    /// in the prompt is stamped with where the agent was, so a draft is
    /// told from one already submitted (a submit starts a new turn).
    fn input_seen(&self, session: &PtySessionId, bytes: &[u8]) {
        use crate::session_wake::Clock;
        let now = crate::session_wake::SystemClock.mono_ms();
        let draft = codesense_pty_core::session_wake::may_draft(bytes)
            .then(|| self.terminal.activity().snapshot(session.as_str()))
            .flatten()
            .map(|s| (s.identity.invocation_id, s.turn_generation));
        let mut seen = lock(&self.input_seen);
        // Bounded by the live sessions: forget the gone ones now and then.
        if seen.len() > 4096 {
            seen.retain(|session, _| self.terminal.has_session(session) == Ok(true));
        }
        let stamp = seen.entry(session.clone()).or_default();
        stamp.at_ms = now;
        if draft.is_some() {
            stamp.draft = draft;
        } else if bytes == b"\x03" {
            // A lone Ctrl-C clears the agent's prompt.
            stamp.draft = None;
        }
    }

    pub(crate) fn last_input(
        &self,
        session: &PtySessionId,
    ) -> Option<crate::session_wake::InputStamp> {
        lock(&self.input_seen).get(session).cloned()
    }

    pub(crate) fn wake_session_live(&self, session: &PtySessionId) -> bool {
        self.terminal.has_session(session) == Ok(true) && !self.terminal.output_ended(session)
    }

    /// Whether `session`'s agent prompt takes input, waiting up to `timeout_ms`
    /// for it (see [`prompt_takes_input`]): a CLI still starting can swallow a
    /// paste's end, after which every Enter is a newline.
    fn agent_prompt_ready(&self, session: &PtySessionId, deadline: Instant) -> bool {
        loop {
            let activity = self.wake_activity(session);
            if prompt_takes_input(activity.as_ref(), || self.wake_bracketed_paste(session)) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// Baseline for proving submission, including a fresh invocation before its first
    /// hook. Capturing it grants no permission to type; prompt readiness is separate.
    fn idle_turn(
        activity: Option<&codesense_pty_core::activity::Snapshot>,
    ) -> Option<(String, u64)> {
        activity
            .filter(|s| {
                matches!(
                    s.state,
                    codesense_pty_core::activity::State::Idle
                        | codesense_pty_core::activity::State::Done
                ) && s.exit.is_none()
                    && codesense_pty_core::agent_cli::adapter(s.provider)
                        .control()
                        .submit_starts_turn
            })
            .map(|s| (s.identity.invocation_id.clone(), s.turn_generation))
    }

    /// Whether a line typed into an agent idle at `idle_at` was submitted: its
    /// agent started a turn. Output alone is no proof (a redraw, the echo of
    /// the typed text), so an agent still idle at the same turn a moment later
    /// was not shown to have taken it. An agent not seen idle before keeps what
    /// its output said.
    fn submit_confirmed(
        &self,
        session: &PtySessionId,
        idle_at: Option<(String, u64)>,
        output_said: bool,
    ) -> bool {
        let Some((invocation, generation)) = idle_at else {
            return output_said;
        };
        let deadline = Instant::now() + SUBMIT_TURN_GRACE;
        loop {
            if turn_started(
                &invocation,
                generation,
                self.wake_activity(session).as_ref(),
            ) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    pub(crate) fn wake_activity(
        &self,
        session: &PtySessionId,
    ) -> Option<codesense_pty_core::activity::Snapshot> {
        self.terminal.activity().snapshot(session.as_str())
    }

    /// For each query, whether its agent CLI is the session's foreground job:
    /// `None` when that cannot be told (the probe failed, or the session has
    /// no process id). Never a guessed `false` (a failed probe is
    /// never an agent that quit).
    pub(crate) fn wake_agents_in_foreground(
        &self,
        queries: &[(PtySessionId, String)],
    ) -> Vec<Option<bool>> {
        let sessions: Vec<PtySessionId> = queries.iter().map(|(s, _)| s.clone()).collect();
        let Ok(pids) = self.terminal.live_child_pids(&sessions) else {
            return vec![None; queries.len()];
        };
        let Ok(alive) = self.terminal.agents_alive(queries) else {
            return vec![None; queries.len()];
        };
        pids.iter()
            .zip(alive)
            .map(|(pid, alive)| pid.map(|_| alive))
            .chain(std::iter::repeat(None))
            .take(queries.len())
            .collect()
    }

    /// Whether `session`'s shell runs no job of its own; `None` when that
    /// cannot be read.
    pub(crate) fn wake_shell_idle(&self, session: &PtySessionId) -> Option<bool> {
        let pid = self
            .terminal
            .live_child_pids(std::slice::from_ref(session))
            .ok()?
            .first()
            .copied()
            .flatten()?;
        let jobs = codesense_pty_core::process::running_jobs_known(&[Some(pid)])
            .pop()
            .flatten()?;
        Some(jobs.foreground.is_none() && jobs.background.is_empty())
    }

    /// Whether `session`'s TUI has bracketed paste on, from its emulator's
    /// terminal; `None` when that cannot be read.
    pub(crate) fn wake_bracketed_paste(&self, session: &PtySessionId) -> Option<bool> {
        let snapshot = self
            .terminal
            .emulator_snapshot(session, Duration::from_secs(2))
            .ok()?;
        Some(snapshot.vt.contains("\x1b[?2004h"))
    }

    pub(crate) fn wake_output_seq(&self, session: &PtySessionId) -> Option<u64> {
        self.terminal.output_seq(session)
    }

    /// Write through `session`'s input queue, in order with its other input,
    /// and wait for the write to settle.
    pub(crate) fn wake_write(
        self: &Arc<Self>,
        session: &PtySessionId,
        bytes: &[u8],
    ) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.queue_write(
            session.clone(),
            WriteJob {
                data: bytes.to_vec(),
                reply: Answer::Wake(tx),
                _permit: None,
            },
        );
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(result) => result.map(|_| ()),
            // Still queued or stalled: it may yet be written.
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err("the session's input did not settle in time".to_string())
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err("the input queue closed before answering".to_string())
            }
        }
    }

    /// Whether [`wake_claim`](Self::wake_claim) would succeed now, taking
    /// nothing.
    pub(crate) fn wake_claim_available(&self, session: &PtySessionId, desktop: &str) -> bool {
        let connections = lock(&self.connections);
        let claims = lock(&self.writable);
        claims.sessions.get(session).is_none_or(|claim| {
            claim.in_flight == 0
                && claim.owner.is_none_or(|owner| {
                    !desktop.is_empty()
                        && connections.get(&owner).and_then(Held::identity).as_deref()
                            == Some(desktop)
                })
        })
    }

    /// Hold `session`'s writable attachment for a wake of `desktop`: free, or
    /// held by that desktop's own connection. Never taken from another.
    pub(crate) fn wake_claim(
        &self,
        session: &PtySessionId,
        desktop: &str,
    ) -> Option<Box<dyn Send>> {
        let connections = lock(&self.connections);
        let mut claims = lock(&self.writable);
        let claim = claims.sessions.entry(session.clone()).or_default();
        // Never alongside another write: no input in flight at all.
        let ours = claim.in_flight == 0
            && match claim.owner {
                None => true,
                Some(owner) => {
                    !desktop.is_empty()
                        && connections.get(&owner).and_then(Held::identity).as_deref()
                            == Some(desktop)
                }
            };
        if !ours {
            return None;
        }
        claim.in_flight += 1;
        drop(claims);
        drop(connections);
        Some(Box::new(WritePermit {
            claims: self.writable.clone(),
            session: session.clone(),
        }))
    }

    /// `InboxWaitV1`: who hears of `session`'s inbox notices and may push them. On a
    /// machine whose desktops relay their agents' CodeSense, a relayed session's own
    /// desktop (none while it is bound to none, or not relayed: its agent could not read
    /// its mail); on a host no desktop relays through (this Mac's), its one client.
    fn inbox_audience(&self, session: &str) -> InboxAudience {
        // Read through the relay's own handle on the broker: a caller may hold the host's.
        #[cfg(target_os = "linux")]
        if let Some(reverse) = self.relay.reverse() {
            return reverse
                .binding(session)
                .flatten()
                .map_or(InboxAudience::Nobody, InboxAudience::Desktop);
        }
        // A private machine whose broker did not start relays no one: no desktop's.
        #[cfg(target_os = "linux")]
        if lock(&self.private_workspace_root).is_some() {
            return InboxAudience::Nobody;
        }
        let _ = session;
        InboxAudience::Every
    }

    /// `InboxWaitV1`: push `event` about `session` to the listening connections of its
    /// audience that asked for inbox notices.
    fn emit_inbox(&self, session: &str, event: TerminalDomainEvent) {
        let audience = self.inbox_audience(session);
        let targets: Vec<Conn> = lock(&self.relay.subscribers)
            .values()
            .filter(|conn| audience.admits(self.identity_of(conn.id).as_deref()))
            .cloned()
            .collect();
        for conn in targets {
            conn.push(event.clone());
        }
    }

    /// Push `outcome` to the connections that asked for wake outcomes.
    pub(crate) fn emit_wake(&self, outcome: &codesense_pty_core::session_wake::WakeOutcome) {
        let _ = self.relay.emit(TerminalDomainEvent::Wake {
            outcome: Box::new(outcome.clone()),
        });
    }

    #[cfg(target_os = "linux")]
    pub fn start_reverse_mcp(self: &Arc<Self>, root: &Path) -> Result<(), String> {
        let reverse = crate::reverse_mcp::ReverseMcp::start(self, root)?;
        *lock(&self.relay.reverse) = Arc::downgrade(&reverse);
        *lock(&self.reverse) = Some(reverse);
        Ok(())
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn reverse_session_live(&self, session: &str, pid: u32) -> bool {
        self.terminal
            .live_child_pids(&[PtySessionId::new(session)])
            .ok()
            .is_some_and(|pids| pids.first() == Some(&Some(pid)))
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn reverse_connection_live(&self, id: u64) -> bool {
        lock(&self.connections).contains_key(&id)
    }
    pub(crate) fn executor_frame(&self, conn: &Conn, frame: Frame) {
        self.executors.receive(conn, frame);
    }
    pub(crate) fn reverse_frame(&self, conn: &Conn, frame: Frame) {
        #[cfg(target_os = "linux")]
        if let Some(reverse) = lock(&self.reverse).clone() {
            reverse.receive(conn, frame);
        }
        #[cfg(not(target_os = "linux"))]
        let _ = (conn, frame);
    }

    /// Serve private workspaces from `root` for `owner`, as an enrolled
    /// private account does (tests on every platform).
    #[cfg(test)]
    pub(crate) fn enable_private_workspaces_at(&self, root: PathBuf, owner: u64) {
        *lock(&self.private_workspace_root) = Some((root, owner));
    }

    #[cfg(target_os = "linux")]
    pub fn enable_private_workspaces(&self, root: &crate::private_account_native::ServeRoot) {
        *lock(&self.private_workspace_root) = Some((root.filesystem_root(), root.owner_id()));
    }

    /// Start this host's emulator worker `worker`, pinned under `root`, on a
    /// thread of its own so serving never waits for it. Without a worker the
    /// host keeps serving; the reason is logged.
    pub fn start_emulator(self: &Arc<Self>, worker: PathBuf, root: &Path) {
        let config = EmulatorConfig::new(worker, root.join("emulator"));
        let host = self.clone();
        let started = thread::Builder::new()
            .name("emulator-start".into())
            .spawn(move || {
                if let Err(error) = host.terminal.start_emulator(config) {
                    log::error!("serving without an emulator worker: {error}");
                }
            });
        if let Err(error) = started {
            log::error!("serving without an emulator worker: {error}");
        }
    }

    /// The hosted terminal runtime.
    #[cfg(any(test, feature = "test-support"))]
    pub fn terminal(&self) -> &TerminalHost {
        &self.terminal
    }

    /// Close every listening client's connection, as this process ending
    /// would.
    #[cfg(any(test, feature = "test-support"))]
    pub fn close_connections(&self) {
        for conn in lock(&self.relay.subscribers).values() {
            let _ = conn.socket.shutdown(std::net::Shutdown::Both);
        }
    }

    /// How many client connections (past their hello) it holds now.
    #[cfg(any(test, feature = "test-support"))]
    pub fn connection_count(&self) -> usize {
        lock(&self.connections).len()
    }

    /// Move this host's clock ahead by `by`, for what it times: how long a
    /// client has been alone.
    #[cfg(any(test, feature = "test-support"))]
    pub fn advance_clock(&self, by: Duration) {
        self.clock_ahead_ms
            .fetch_add(by.as_millis() as u64, std::sync::atomic::Ordering::Relaxed);
    }

    /// Now, on this host's clock.
    fn now(&self) -> Instant {
        #[cfg(any(test, feature = "test-support"))]
        return Instant::now()
            + Duration::from_millis(
                self.clock_ahead_ms
                    .load(std::sync::atomic::Ordering::Relaxed),
            );
        #[cfg(not(any(test, feature = "test-support")))]
        Instant::now()
    }

    /// What the exit reap of client connection `owner` may take beyond its
    /// own sessions. With another client connected, nothing. Alone, every
    /// unhanded warm-pool member; and once no other client has been
    /// connected for [`LEFTOVER_GRACE`], every non-persisting session no
    /// viewport has shown for that long too.
    fn last_client_reap(&self, owner: u64) -> (bool, Option<codesense_pty_core::LeftoverReap>) {
        let connections = lock(&self.connections);
        if !connections.keys().all(|id| *id == owner) {
            return (false, None);
        }
        let now = self.now();
        let others_long_gone = lock(&self.last_client_left)
            .is_none_or(|left| now.saturating_duration_since(left) > LEFTOVER_GRACE);
        drop(connections);
        let leftovers = others_long_gone.then_some(codesense_pty_core::LeftoverReap {
            now,
            grace: LEFTOVER_GRACE,
        });
        (true, leftovers)
    }

    /// This sidecar's first frames on every connection: its hello, then its
    /// census.
    pub(crate) fn greeting(&self) -> Result<[Frame; 2], String> {
        let identity = self.terminal.identity().map_err(|e| e.to_string())?;
        #[allow(unused_mut)]
        let mut hello = Hello {
            proto_major: PROTO_MAJOR,
            proto_minor: PROTO_MINOR,
            // The installed build rides along, so a desktop can tell
            // a newer build of the same version from this one.
            agent_version: crate::install::announced_version(
                crate::install::running_bundle_version().as_deref(),
            ),
            server_id: identity.server.id.as_str().to_string(),
            server_name: identity.server.name,
            epoch: identity.epoch,
            rev: identity.rev,
            instance_id: self.instance_id.clone(),
            emulator: Some(EmulatorHello::current()),
            capabilities: if cfg!(target_os = "linux") && self.replacement_root.is_some() {
                vec![
                    Capability::Terminals,
                    Capability::ShutdownForReplacement,
                    Capability::WritableAttachment,
                    Capability::RemoteTabRecovery,
                    Capability::AccountHome,
                    Capability::NativeRuntimeProbe,
                    Capability::WorkspaceHead,
                ]
            } else {
                vec![
                    Capability::Terminals,
                    Capability::WritableAttachment,
                    Capability::RemoteTabRecovery,
                    Capability::AccountHome,
                    Capability::NativeRuntimeProbe,
                    Capability::WorkspaceHead,
                ]
            },
            viewer_id: None,
            pid: std::process::id(),
            // The app release this server ships with, so the app
            // can say whether it runs its own build.
            build: Some(crate::install::build_stamp()),
            client_id: None,
        };
        hello.capabilities.push(Capability::WorktreeSafetyV1);
        hello.capabilities.push(Capability::AgentProcessV1);
        hello.capabilities.push(Capability::SessionPrompt);
        if crate::usage_receiver::advertised_spool().is_some() {
            hello.capabilities.push(Capability::UsageSpoolV1);
        }
        // The pipelined bundle upload and the explicit base update.
        hello.capabilities.push(Capability::BundlePipelineV1);
        hello.capabilities.push(Capability::WorkspaceBaseUpdateV1);
        hello.capabilities.push(Capability::WorkspaceEditorV1);
        hello.capabilities.push(Capability::AgentEnvV1);
        hello.capabilities.push(Capability::ExitRedeliveryV1);
        hello.capabilities.push(Capability::ScopedReapV1);
        hello.capabilities.push(Capability::ServerStatusV1);
        hello.capabilities.push(Capability::MachineStatsV1);
        if lock(&self.activity_service).is_some() {
            hello.capabilities.push(Capability::ActivityV1);
            hello.capabilities.push(Capability::SessionOutputV1);
            hello.capabilities.push(Capability::ActivityV2);
            hello.capabilities.push(Capability::AttributionHookTrustV1);
            // Inbox hooks reach this host through its activity ingress.
            hello.capabilities.push(Capability::InboxWaitV1);
            hello.capabilities.push(Capability::InboxWakeHoldsV1);
            hello.capabilities.push(Capability::LocalMcpReadinessV1);
        }
        if self.wake().is_some() {
            hello.capabilities.push(Capability::SessionWakeV1);
        }
        if self.arms().is_some() {
            hello.capabilities.push(Capability::RoutineArmV1);
        }
        let live = self.terminal.live_session_count();
        #[cfg(target_os = "linux")]
        if lock(&self.reverse).is_some() {
            hello.capabilities.push(Capability::ReverseMcp);
            hello.capabilities.push(Capability::ReverseMcpServers);
            hello.capabilities.push(Capability::NativeRemoteLaunch);
            hello.capabilities.push(Capability::AgentStartup);
            if crate::owned_supervisors::pidfd(std::process::id()).is_ok() {
                hello.capabilities.push(Capability::RemoteStdioMcp);
            }
        }
        if lock(&self.private_workspace_root).is_some() {
            hello.capabilities.push(Capability::RemoteExecutor);
            hello.capabilities.push(Capability::SelectedContextBatchV1);
            hello.capabilities.push(Capability::ContextManifestV1);
            hello
                .capabilities
                .push(Capability::ManifestWorkspaceExcludeV1);
            hello
                .capabilities
                .push(Capability::ForgetMissingWorkspaceV1);
            hello.capabilities.push(Capability::ManifestReadV1);
            hello.capabilities.push(Capability::ContextBatchZstdV1);
        }
        let census = Census {
            live_sessions: u32::try_from(live).unwrap_or(u32::MAX),
        };
        Ok([hello.frame()?, census.frame()])
    }

    /// Whether a client with this hello may go on: it belongs to this
    /// instance and speaks this protocol major.
    pub(crate) fn admits(&self, peer: &PeerHello) -> bool {
        matches!(peer, PeerHello::Compatible(hello) if hello.instance_id == self.instance_id)
    }

    /// `conn` completed the handshake: it may attach viewports and open tabs
    /// from now on.
    #[cfg(test)]
    pub(crate) fn connect(&self, conn: &Conn) {
        self.connect_as(conn, None);
    }

    /// [`connect`](Self::connect), for the viewer its hello named.
    pub(crate) fn connect_as(&self, conn: &Conn, viewer: Option<String>) {
        lock(&self.connections).insert(
            conn.id,
            Held {
                viewer,
                ..Held::default()
            },
        );
    }

    /// `conn`'s hello named the app process `client`: it is connected
    /// again, whenever it left.
    pub(crate) fn identify_client(&self, conn: u64, client: Option<String>) {
        let mut connections = lock(&self.connections);
        if let Some(held) = connections.get_mut(&conn) {
            if let Some(client) = &client {
                lock(&self.clients_left).remove(client);
            }
            held.client = client;
        }
    }

    /// `session` was opened by the app connection `conn` names, if it
    /// names one.
    fn note_opener(&self, conn: u64, session: &PtySessionId) {
        let client = lock(&self.connections)
            .get(&conn)
            .and_then(|held| held.client.clone());
        let mut openers = lock(&self.openers);
        match client {
            Some(client) => openers.insert(session.clone(), client),
            None => openers.remove(session),
        };
    }

    /// The app connection `conn` names holds `sessions` (a relaunched
    /// app's adopted background and headless runs): each that runs here and
    /// whose opener is gone or unknown is attributed to it from now on. A
    /// session whose opener is connected stays that app's: a claim never
    /// takes another live app's sessions. Nothing, for a connection that
    /// names no app.
    fn claim_sessions(&self, conn: u64, sessions: &[PtySessionId]) {
        let facts = self.terminal.session_facts();
        // Read apart from `openers`: the order is `openers`, then
        // `connections` (as `unowned_among` takes them).
        let (client, connected) = {
            let connections = lock(&self.connections);
            let Some(client) = connections.get(&conn).and_then(|held| held.client.clone()) else {
                return;
            };
            let connected: HashSet<String> = connections
                .values()
                .filter_map(|held| held.client.clone())
                .collect();
            (client, connected)
        };
        let present: HashSet<&PtySessionId> = facts.iter().map(|fact| &fact.session).collect();
        let mut openers = lock(&self.openers);
        for session in sessions.iter().filter(|session| present.contains(*session)) {
            let held_by_live_app = openers
                .get(session)
                .is_some_and(|opener| opener != &client && connected.contains(opener));
            if !held_by_live_app {
                openers.insert(session.clone(), client.clone());
            }
        }
    }

    /// The sessions no app holds, as of `now`, from `facts`: one opened
    /// (or last claimed) by an app that has had no connection for
    /// [`LEFTOVER_GRACE`] — a reconnecting or relaunched app is back well
    /// within it — that no viewport shows and that is not an entry of the
    /// layout, a tab or a hidden (background) one (every app shows those, and
    /// ends them there). So only a session outside the layout — a headless
    /// run, a background run outside the layout, a bare spawn — can be one. One with no recorded
    /// opener (an older client's) is never one: who holds it cannot be told.
    /// Takes `openers` (pruned to the sessions in `facts`) as held.
    fn unowned_among(
        &self,
        facts: &[codesense_pty_core::SessionFacts],
        openers: &mut HashMap<PtySessionId, String>,
        now: Instant,
    ) -> Vec<codesense_pty_core::SessionFacts> {
        let connected: HashSet<String> = lock(&self.connections)
            .values()
            .filter_map(|held| held.client.clone())
            .collect();
        let left = lock(&self.clients_left).clone();
        let present: HashSet<&PtySessionId> = facts.iter().map(|fact| &fact.session).collect();
        openers.retain(|session, _| present.contains(session));
        facts
            .iter()
            .filter(|fact| {
                !fact.pool_member
                    && !fact.viewed
                    && !fact.layout_entry
                    && openers.get(&fact.session).is_some_and(|client| {
                        !connected.contains(client)
                            && left.get(client).is_some_and(|at| {
                                now.saturating_duration_since(*at) >= LEFTOVER_GRACE
                            })
                    })
            })
            .cloned()
            .collect()
    }

    /// End `session` if — checked as it ends, under the lock a claim
    /// takes — it is still one no app holds. `Err` (nothing ends) when it is
    /// not, or no longer, such a session.
    fn end_unowned(&self, session: &PtySessionId) -> Result<(), PortUnavailable> {
        let facts = self.terminal.session_facts();
        let mut openers = lock(&self.openers);
        let unowned = self.unowned_among(&facts, &mut openers, self.now());
        if !unowned.iter().any(|fact| &fact.session == session) {
            return Err(PortUnavailable(format!(
                "{session} is not a session no app holds (any more); nothing was ended"
            )));
        }
        log::info!("ending session {session}: no app holds it");
        self.terminal.kill(session)
    }

    /// What this host holds, as client connection `asker` asks: its
    /// sessions, the apps listening (by app, not connection) and the sessions
    /// no app holds ([`Self::unowned_among`]).
    pub(crate) fn server_status(&self, asker: u64) -> codesense_pty_core::protocol::ServerStatus {
        use codesense_pty_core::protocol::{ServerStatus, UnownedSession};
        let all = self.terminal.session_facts();
        let facts: Vec<_> = all
            .iter()
            .filter(|fact| !fact.pool_member)
            .cloned()
            .collect();
        let agents = self.terminal.running_agent_sessions();
        let (clients, other_clients) = {
            let listening: Vec<u64> = lock(&self.relay.subscribers).keys().copied().collect();
            let connections = lock(&self.connections);
            let app = |conn: &u64| {
                connections
                    .get(conn)
                    .and_then(|held| held.client.clone())
                    .unwrap_or_else(|| format!("connection-{conn}"))
            };
            let asker_app = app(&asker);
            let apps: HashSet<String> = listening.iter().map(app).collect();
            let others = apps.iter().filter(|app| **app != asker_app).count();
            (apps.len() as u32, others as u32)
        };
        let unowned_facts = {
            let mut openers = lock(&self.openers);
            self.unowned_among(&all, &mut openers, self.now())
        };
        let pids: Vec<Option<u32>> = unowned_facts.iter().map(|fact| fact.pid).collect();
        let jobs = codesense_pty_core::process::running_jobs_known(&pids);
        let unowned = unowned_facts
            .into_iter()
            .zip(jobs)
            .map(|(fact, jobs)| UnownedSession {
                command: jobs.and_then(|jobs| {
                    jobs.foreground.or_else(|| {
                        (!jobs.background.is_empty()).then(|| jobs.background.join(", "))
                    })
                }),
                running_agent: fact.running && agents.contains(&fact.session),
                session: fact.session,
                started_at_ms: fact.started_at_ms,
                running: fact.running,
            })
            .collect();
        let running = facts.iter().filter(|fact| fact.running).count() as u32;
        ServerStatus {
            started_at_ms: self
                .started_at
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_millis() as u64),
            sessions: facts.len() as u32,
            running,
            running_agents: facts
                .iter()
                .filter(|fact| fact.running && agents.contains(&fact.session))
                .count() as u32,
            exited_kept: facts.len() as u32 - running,
            clients,
            other_clients,
            unowned,
            nofile: codesense_pty_core::nofile::usage(),
        }
    }

    /// `conn` closed: stop pushing events to it, detach the viewports it
    /// attached, and abandon the tabs it was opening — its client learns the
    /// open failed, so none of them may run: a spawn still in flight is
    /// cancelled, a spawned session is killed, and each reservation is
    /// dropped. Its other sessions keep running.
    pub(crate) fn disconnect(&self, conn: &Conn) {
        conn.machine_stats
            .store(false, std::sync::atomic::Ordering::Release);
        lock(&self.machine_stats_schedule).watch(conn.id, false, Instant::now());
        self.machine_stats_changed.notify_one();
        let held = {
            let mut connections = lock(&self.connections);
            let held = connections.remove(&conn.id).unwrap_or_default();
            // Under the `connections` lock, so a reap never sees this client
            // gone without seeing it leave.
            if held.viewed {
                *lock(&self.last_client_left) = Some(self.now());
            }
            // The app's last connection closed: its sessions' grace runs
            // from now.
            if let Some(client) = &held.client {
                if !connections
                    .values()
                    .any(|other| other.client.as_ref() == Some(client))
                {
                    lock(&self.clients_left).insert(client.clone(), self.now());
                }
            }
            held
        };
        self.executors.disconnect(conn.id);
        #[cfg(target_os = "linux")]
        if let Some(reverse) = lock(&self.reverse).clone() {
            let identity = held.identity();
            reverse.disconnect(conn.id, identity.clone());
            // The same desktop already came back on another link
            // (this one was half-open): its agents go there now.
            let back = identity.as_ref().and_then(|identity| {
                lock(&self.connections).values().find_map(|other| {
                    (other.identity().as_ref() == Some(identity))
                        .then(|| other.routine_bind.clone())
                        .flatten()
                        .map(|bind| (bind, other.desktop.is_some()))
                })
            });
            if let (Some(identity), Some(((secret, back), named))) = (identity, back) {
                reverse.reattach(&identity, named, &secret, &back);
            }
        }
        lock(&self.relay.subscribers).remove(&conn.id);
        lock(&self.writable).disconnect(conn.id);
        for stream in held.viewports {
            if let Err(error) = self.detach(stream) {
                log::warn!("detach of stream {} failed: {error}", stream.0);
            }
        }
        for (session, request) in held.openings {
            if let Some(in_flight) = lock(&self.spawning).in_flight.get_mut(session.as_str()) {
                in_flight.killed = true;
            }
            let _ = self.terminal.kill(&session);
            let abandoned = Err("open_tab: the connection closed while opening".to_string());
            let _ = self.terminal.finish_tab(request, abandoned);
        }
        for session in held.respawns {
            self.terminal.cancel_respawn(&session);
        }
    }

    /// Reserve a tab for `owner`, which spawns its session next. A
    /// launch-if-absent (a routine's wake that found no session of
    /// its agent) goes through the host's single flight: refused with
    /// [`AGENT_RUNNING`](codesense_pty_core::routine_arm::AGENT_RUNNING)
    /// while a session of the agent runs, or is being started, in that
    /// workspace, by anyone (this host's own routines included).
    fn reserve_tab(&self, owner: u64, request: OpenTab) -> Result<(), LayoutError> {
        let flight = request
            .tags
            .contains_key(codesense_pty_core::routine_arm::TAG_LAUNCH_IF_ABSENT)
            .then(|| tab_agent(&request.tags))
            .flatten()
            .map(|(agent, workspace)| {
                (agent.to_string(), workspace.unwrap_or_default().to_string())
            });
        let flights = flight.as_ref().map(|_| lock(&self.agent_flights));
        if let (Some((agent, workspace)), Some(flights)) = (&flight, &flights) {
            if let Some(running) = self.agent_session_in(flights, agent, workspace) {
                return Err(LayoutError::Spawn(format!(
                    "{}: {agent} is already running or starting in this workspace (session {running}); deliver to it instead",
                    codesense_pty_core::routine_arm::AGENT_RUNNING
                )));
            }
        }
        let mut connections = lock(&self.connections);
        let Some(held) = connections.get_mut(&owner) else {
            return Err(LayoutError::Unreachable(
                "open_tab: the connection closed while reserving".to_string(),
            ));
        };
        self.terminal.reserve_tab(&request)?;
        if let Some(path) = request
            .descriptor
            .as_ref()
            .and_then(|d| d.worktree_path.as_ref())
        {
            *lock(&self.machine_stats_directory) = Some(path.clone());
        }
        held.openings.insert(request.session.clone(), request);
        Ok(())
    }

    /// Begin a respawn in place for `owner`, which spawns the new process next;
    /// the hold is released if `owner` goes away before finishing.
    fn begin_respawn(&self, owner: u64, session: PtySessionId) -> Result<Reply, LayoutError> {
        if !lock(&self.connections).contains_key(&owner) {
            return Err(LayoutError::Unreachable(
                "respawn_tab: the connection closed while beginning".to_string(),
            ));
        }
        match self.terminal.begin_respawn(&session)? {
            RespawnStart::Reaped => {
                match lock(&self.connections).get_mut(&owner) {
                    Some(held) => {
                        held.respawns.insert(session);
                    }
                    // It went away meanwhile: nothing will finish this respawn.
                    None => self.terminal.cancel_respawn(&session),
                }
                Ok(Reply::Done)
            }
            RespawnStart::Revived(entry) => Ok(Reply::Tab {
                entry: Box::new(entry),
            }),
        }
    }

    fn reconcile_tab(&self, owner: u64, session: PtySessionId) -> Result<Reply, LayoutError> {
        {
            let mut connections = lock(&self.connections);
            let held = connections.get_mut(&owner).ok_or_else(|| {
                LayoutError::SpawnUnknown("original connection is unavailable".into())
            })?;
            if !held.openings.contains_key(&session) {
                return Err(LayoutError::UnknownSession(session));
            }
            // Install before observing the registry. A spawn racing this read
            // either appears now, or its completion commits the held tags.
            held.uncertain_openings.insert(session.clone());
        }
        if self.terminal.has_session(&session) == Ok(true) {
            self.finish_tab(owner, session, Ok(()))
        } else {
            Err(LayoutError::SpawnUnknown(
                "spawn is not yet confirmed".into(),
            ))
        }
    }

    fn finish_uncertain_spawn(&self, session: &PtySessionId) {
        let owner = lock(&self.connections)
            .iter()
            .find_map(|(owner, held)| held.uncertain_openings.contains(session).then_some(*owner));
        if let Some(owner) = owner {
            // Concurrent explicit reconciliation may already have committed it.
            let _ = self.finish_tab(owner, session.clone(), Ok(()));
        }
    }

    /// Finish the tab `owner` reserved for `session`.
    ///
    /// The single flight's lock is held from taking the
    /// reservation out of `openings` until the layout holds the tab, so a
    /// launch-if-absent never sees the agent's tab in neither place (it would
    /// start a second copy). Taken before `connections`, as `reserve_tab` and
    /// `arm_launch` do. The explicit finish, `reconcile_tab` and
    /// `finish_uncertain_spawn` all come through here.
    fn finish_tab(
        &self,
        owner: u64,
        session: PtySessionId,
        spawned: Result<(), String>,
    ) -> Result<Reply, LayoutError> {
        let _flight = lock(&self.agent_flights);
        let request = lock(&self.connections)
            .get_mut(&owner)
            .and_then(|held| {
                held.uncertain_openings.remove(&session);
                held.openings.remove(&session)
            })
            .ok_or(LayoutError::UnknownSession(session))?;
        #[cfg(test)]
        if let Some(hook) = lock(&self.finish_tab_hook).as_ref() {
            hook();
        }
        let entry = self.terminal.finish_tab(request, spawned)?;
        Ok(Reply::Tab {
            entry: Box::new(entry),
        })
    }

    /// Take one [`Kind::INPUT`](codesense_host_proto::Kind::INPUT) frame from
    /// `conn`: its bytes go, verbatim and in order, to the session its channel
    /// was opened for, answered on the frame's call id.
    pub(crate) fn input(self: &Arc<Self>, conn: &Conn, frame: &Frame) {
        let (rid, data) = match frame.input_parts() {
            Ok(parts) => parts,
            Err(error) => return conn.reply_unparseable(error),
        };
        let reply = Responder {
            rid,
            conn: conn.clone(),
        };
        if lock(&self.spawning).draining {
            return reply.send(Err("terminal server is draining".into()));
        }
        let session = lock(&conn.inputs).get(&frame.channel).cloned();
        match session {
            Some(session) => {
                let permit = match WritePermit::acquire(&self.writable, &session, conn.id) {
                    Ok(permit) => permit,
                    Err(error) => return reply.send(Err(error)),
                };
                self.input_seen(&session, data);
                self.queue_write(
                    session,
                    WriteJob {
                        data: data.to_vec(),
                        reply: Answer::Conn(reply),
                        _permit: Some(permit),
                    },
                );
            }
            None => reply.send(Err(format!(
                "write_pty: input channel {} is not open",
                frame.channel
            ))),
        }
    }

    /// Answer one request; its reply goes to `conn`. Writes run in order on the
    /// session's own input thread, so a child that stops reading its input
    /// stalls only that thread; everything else runs on the runtime, so a long
    /// submit or attach never holds up the connection. Each arm is one call
    /// into the hosted runtime.
    pub(crate) fn handle(self: &Arc<Self>, conn: &Conn, rid: u64, request: Request) {
        let reply = Responder {
            rid,
            conn: conn.clone(),
        };
        if !matches!(&request, Request::ShutdownForReplacement { .. })
            && lock(&self.spawning).draining
        {
            return reply.send(Err("terminal server is draining".into()));
        }
        let host = self.clone();
        let blocking = |op: Box<Operation>| {
            let host = host.clone();
            self.runtime
                .spawn_blocking(move || op(&host).map_err(String::from))
        };
        let done = |()| Reply::Done;
        let flag = |value| Reply::Flag { value };
        let task = match request {
            Request::StartRemoteExecutor { request } => {
                let private = lock(&self.private_workspace_root).clone();
                let conn = conn.clone();
                blocking(Box::new(move |host| {
                    let (root, owner) = private
                        .ok_or_else(|| PortUnavailable("private executor unavailable".into()))?;
                    host.executors
                        .start(&root, owner, conn.clone(), *request, || {
                            !lock(&host.spawning).draining
                                && lock(&host.connections).contains_key(&conn.id)
                        })
                        .map(|result| Reply::RemoteExecutorStarted { result })
                        .map_err(PortUnavailable)
                }))
            }
            Request::OpenRemoteExecutor { task_id, channel } => {
                // Keep local socket admission ordered before later channel-close frames.
                return reply.send(
                    self.executors
                        .open(conn.clone(), &task_id, channel)
                        .map(|_| Reply::Done),
                );
            }
            Request::StopRemoteExecutor { task_id } => {
                let owner = conn.id;
                let private = lock(&self.private_workspace_root).clone();
                blocking(Box::new(move |host| {
                    let (root, _) = private
                        .ok_or_else(|| PortUnavailable("private executor unavailable".into()))?;
                    host.executors
                        .stop(Some(&root), owner, &task_id)
                        .map(|_| Reply::Done)
                        .map_err(PortUnavailable)
                }))
            }
            Request::WorkspaceExport { request } => {
                let private = lock(&self.private_workspace_root).clone();
                let owner_conn = conn.id;
                blocking(Box::new(move |host| {
                    let (root, owner) = private.ok_or_else(|| {
                        PortUnavailable("private workspace export unavailable".into())
                    })?;
                    crate::remote_workspace::export::handle(&root, owner, *request, || {
                        !lock(&host.spawning).draining
                            && lock(&host.connections).contains_key(&owner_conn)
                    })
                    .map(|result| Reply::WorkspaceExport {
                        result: Box::new(result),
                    })
                    .map_err(PortUnavailable)
                }))
            }
            Request::WorkspaceHead { target } => {
                let private = lock(&self.private_workspace_root).clone();
                let owner_conn = conn.id;
                blocking(Box::new(move |host| {
                    let (root, owner) = private.ok_or_else(|| {
                        PortUnavailable("private workspace export unavailable".into())
                    })?;
                    crate::remote_workspace::export::head(&root, owner, &target, || {
                        !lock(&host.spawning).draining
                            && lock(&host.connections).contains_key(&owner_conn)
                    })
                    .map(|head| Reply::WorkspaceHead { head })
                    .map_err(PortUnavailable)
                }))
            }
            Request::WorkspaceContextBatch { request } => {
                let private = lock(&self.private_workspace_root).clone();
                let owner_conn = conn.id;
                blocking(Box::new(move |host| {
                    let (root, owner) = private.ok_or_else(|| {
                        PortUnavailable("private context batch service unavailable".into())
                    })?;
                    crate::remote_workspace::context_batch::handle(&root, owner, *request, || {
                        !lock(&host.spawning).draining
                            && lock(&host.connections).contains_key(&owner_conn)
                    })
                    .map(|result| Reply::WorkspaceContextBatch {
                        result: Box::new(result),
                    })
                    .map_err(PortUnavailable)
                }))
            }
            Request::WorkspaceFile { request } => {
                let private = lock(&self.private_workspace_root).clone();
                let owner_conn = conn.id;
                blocking(Box::new(move |host| {
                    let (root, owner) = private.ok_or_else(|| {
                        PortUnavailable("private workspace file service unavailable".into())
                    })?;
                    crate::remote_workspace::files::handle(&root, owner, *request, || {
                        !lock(&host.spawning).draining
                            && lock(&host.connections).contains_key(&owner_conn)
                    })
                    .map(|result| Reply::WorkspaceFile {
                        result: Box::new(result),
                    })
                    .map_err(PortUnavailable)
                }))
            }
            Request::WorkspaceManifest { request } => {
                let private = lock(&self.private_workspace_root).clone();
                let owner_conn = conn.id;
                let stream = conn.clone();
                blocking(Box::new(move |host| {
                    let (root, owner) = private.ok_or_else(|| {
                        PortUnavailable("private workspace manifest unavailable".into())
                    })?;
                    // Parts are queued on this connection ahead of the reply,
                    // which the runtime sends once this returns.
                    crate::remote_workspace::manifest::handle(
                        &root,
                        owner,
                        *request,
                        || {
                            !lock(&host.spawning).draining
                                && lock(&host.connections).contains_key(&owner_conn)
                        },
                        &mut |part| {
                            crate::remote_workspace::manifest::part_frame(rid, part)
                                .is_some_and(|frame| stream.send(frame))
                        },
                    )
                    .map(|result| Reply::WorkspaceManifest {
                        result: Box::new(result),
                    })
                    .map_err(PortUnavailable)
                }))
            }
            Request::ReconcileRemoteWorkspace { request } => {
                let private = lock(&self.private_workspace_root).clone();
                let owner_conn = conn.id;
                blocking(Box::new(move |host| {
                    let (root, owner) = private.ok_or_else(|| {
                        PortUnavailable("private workspace reconciliation unavailable".into())
                    })?;
                    crate::remote_workspace::reconcile(&root, owner, request, || {
                        !lock(&host.spawning).draining
                            && lock(&host.connections).contains_key(&owner_conn)
                    })
                    .map(|result| Reply::RemoteWorkspaceReceipt {
                        result: result.map(Box::new),
                    })
                    .map_err(PortUnavailable)
                }))
            }
            Request::InspectWorkspaceRepository { request } => {
                let private = lock(&self.private_workspace_root).clone();
                let owner_conn = conn.id;
                blocking(Box::new(move |host| {
                    let (root, owner) = private.ok_or_else(|| {
                        PortUnavailable("private repository inspection unavailable".into())
                    })?;
                    crate::remote_workspace::inspect_repository(&root, owner, request, || {
                        !lock(&host.spawning).draining
                            && lock(&host.connections).contains_key(&owner_conn)
                    })
                    .map(|knowledge| Reply::WorkspaceRepositoryKnowledge { knowledge })
                    .map_err(PortUnavailable)
                }))
            }
            Request::ForgetMissingWorkspace { request } => {
                let private = lock(&self.private_workspace_root).clone();
                let owner_conn = conn.id;
                blocking(Box::new(move |host| {
                    let (root, owner) = private.ok_or_else(|| {
                        PortUnavailable("private workspace cleanup unavailable".into())
                    })?;
                    crate::remote_workspace::forget_missing_workspace(&root, owner, request, || {
                        !lock(&host.spawning).draining
                            && lock(&host.connections).contains_key(&owner_conn)
                    })
                    .map(|()| Reply::Done)
                    .map_err(PortUnavailable)
                }))
            }
            Request::RemoveWorktree { request } => {
                let private = lock(&self.private_workspace_root).clone();
                let owner_conn = conn.id;
                blocking(Box::new(move |host| {
                    let (root, owner) = private.ok_or_else(|| {
                        PortUnavailable("private worktree removal unavailable".into())
                    })?;
                    let source_cwd = std::path::PathBuf::from(&request.worktree_path);
                    crate::remote_workspace::remove_worktree(&root, owner, request, || {
                        !lock(&host.spawning).draining
                            && lock(&host.connections).contains_key(&owner_conn)
                    }, || {
                        // Architecture: check the host's current sessions inside the removal lock.
                        let usage = host.terminal.worktree_usage(source_cwd.to_str().ok_or("source cwd unavailable")?).map_err(|e| e.to_string())?;
                        if !usage.entries.is_empty() || !usage.opening.is_empty() {
                            return Err(codesense_pty_core::layout::source_refusal("runningAgents: stop the agents running in or starting into the source worktree, then retry"));
                        }
                        Ok(())
                    })
                    .map(|result| Reply::WorktreeRemoved { result })
                    .map_err(PortUnavailable)
                }))
            }
            Request::CheckWorktreeDirtyRemote { request } => {
                let private = lock(&self.private_workspace_root).clone();
                let owner_conn = conn.id;
                blocking(Box::new(move |host| {
                    let (root, owner) = private
                        .ok_or_else(|| PortUnavailable("private dirty-check unavailable".into()))?;
                    crate::remote_workspace::check_worktree_dirty_remote(
                        &root,
                        owner,
                        request,
                        || {
                            !lock(&host.spawning).draining
                                && lock(&host.connections).contains_key(&owner_conn)
                        },
                    )
                    .map(|status| Reply::WorktreeDirtyRemote { status })
                    .map_err(PortUnavailable)
                }))
            }
            Request::UploadWorkspaceBundle { chunk } => {
                let private = lock(&self.private_workspace_root).clone();
                let owner_conn = conn.id;
                blocking(Box::new(move |host| {
                    let (root, owner) = private.ok_or_else(|| {
                        PortUnavailable("private bundle transfer unavailable".into())
                    })?;
                    crate::remote_workspace::upload(&root, owner, chunk, || {
                        !lock(&host.spawning).draining
                            && lock(&host.connections).contains_key(&owner_conn)
                    })
                    .map(|progress| Reply::WorkspaceBundleProgress { progress })
                    .map_err(PortUnavailable)
                }))
            }
            Request::BundleUploadStatus { request } => {
                let private = lock(&self.private_workspace_root).clone();
                let owner_conn = conn.id;
                blocking(Box::new(move |host| {
                    let (root, owner) = private.ok_or_else(|| {
                        PortUnavailable("private bundle transfer unavailable".into())
                    })?;
                    crate::remote_workspace::bundle_status(&root, owner, request, || {
                        !lock(&host.spawning).draining
                            && lock(&host.connections).contains_key(&owner_conn)
                    })
                    .map(|status| Reply::BundleUploadStatus { status })
                    .map_err(PortUnavailable)
                }))
            }
            Request::UploadBundleChunk { chunk } => {
                let private = lock(&self.private_workspace_root).clone();
                let owner_conn = conn.id;
                blocking(Box::new(move |host| {
                    let (root, owner) = private.ok_or_else(|| {
                        PortUnavailable("private bundle transfer unavailable".into())
                    })?;
                    crate::remote_workspace::stage_chunk(&root, owner, chunk, || {
                        !lock(&host.spawning).draining
                            && lock(&host.connections).contains_key(&owner_conn)
                    })
                    .map(|index| Reply::BundleChunkStaged { index })
                    .map_err(PortUnavailable)
                }))
            }
            Request::SealWorkspaceBundle { request } => {
                let private = lock(&self.private_workspace_root).clone();
                let owner_conn = conn.id;
                blocking(Box::new(move |host| {
                    let (root, owner) = private.ok_or_else(|| {
                        PortUnavailable("private bundle transfer unavailable".into())
                    })?;
                    crate::remote_workspace::seal_bundle(&root, owner, request, || {
                        !lock(&host.spawning).draining
                            && lock(&host.connections).contains_key(&owner_conn)
                    })
                    .map(|()| Reply::Done)
                    .map_err(PortUnavailable)
                }))
            }
            Request::UpdateWorkspaceBase { request } => {
                let private = lock(&self.private_workspace_root).clone();
                let owner_conn = conn.id;
                blocking(Box::new(move |host| {
                    let (root, owner) = private.ok_or_else(|| {
                        PortUnavailable("private workspace update unavailable".into())
                    })?;
                    crate::remote_workspace::base_update::update_base(
                        &root,
                        owner,
                        *request,
                        || {
                            !lock(&host.spawning).draining
                                && lock(&host.connections).contains_key(&owner_conn)
                        },
                    )
                    .map(|result| Reply::WorkspaceBaseUpdated {
                        result: Box::new(result),
                    })
                    .map_err(PortUnavailable)
                }))
            }
            Request::PrepareRemoteWorkspace { request } => {
                let private = lock(&self.private_workspace_root).clone();
                let owner_conn = conn.id;
                blocking(Box::new(move |host| {
                    let result = match private {
                        Some((root, owner)) => {
                            crate::remote_workspace::prepare(&root, owner, request, || {
                                !lock(&host.spawning).draining
                                    && lock(&host.connections).contains_key(&owner_conn)
                            })
                        }
                        None => codesense_pty_core::remote_workspace::PreparedWorkspace::failure(
                            "refused",
                            "workspace preparation requires a private enrolled daemon",
                        ),
                    };
                    if let Some(cwd) = result.cwd.as_ref() {
                        *lock(&host.machine_stats_directory) = Some(PathBuf::from(cwd));
                    }
                    Ok(Reply::RemoteWorkspacePrepared {
                        result: Box::new(result),
                    })
                }))
            }

            Request::Listen => {
                self.relay.listen(conn);
                return reply.send(Ok(Reply::Done));
            }
            Request::ShutdownForReplacement {
                nonce,
                server_id,
                epoch,
                process_start,
            } => {
                if self.replacement_root.is_none() {
                    return reply.send(Err("shutdown for replacement is unsupported".into()));
                }
                let identity = match self.terminal.identity() {
                    Ok(identity) => identity,
                    Err(error) => return reply.send(Err(error.to_string())),
                };
                if nonce.len() < 16
                    || nonce.len() > 128
                    || !nonce.bytes().all(|b| b.is_ascii_alphanumeric())
                    || identity.server.id.as_str() != server_id
                    || identity.epoch != epoch
                    || process_start != current_process_start().unwrap_or(0)
                {
                    return reply.send(Err("replacement shutdown identity mismatch".into()));
                }
                let marker = Reply::Retired {
                    nonce: nonce.clone(),
                    server_id: server_id.clone(),
                    epoch: epoch.clone(),
                    process_start,
                    sessions: u32::MAX,
                };
                {
                    let mut spawning = lock(&self.spawning);
                    if spawning.draining {
                        return reply.send(Err("terminal server is already draining".into()));
                    }
                    if let Err(error) = self.write_replacement_evidence("draining", &marker) {
                        return reply.send(Err(error));
                    }
                    spawning.draining = true;
                    for spawn in spawning.in_flight.values_mut() {
                        spawn.killed = true;
                    }
                }
                let host = self.clone();
                drop(self.runtime.spawn_blocking(move || {
                    let deadline = Instant::now() + Duration::from_secs(2);
                    while lock(&host.spawning)
                        .in_flight
                        .values()
                        .any(|s| s.spawns != 0)
                    {
                        if Instant::now() >= deadline {
                            return reply.send(Err("in-flight spawn drain timed out".into()));
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    let receipt = Reply::Retired {
                        nonce,
                        server_id,
                        epoch,
                        process_start,
                        sessions: 0,
                    };
                    let receipt = match host.retire_sessions_for_replacement(receipt, deadline) {
                        Ok(receipt) => receipt,
                        Err(error) => return reply.send(Err(error)),
                    };
                    reply.send(Ok(receipt));
                    std::thread::sleep(Duration::from_millis(25));
                    std::process::exit(0);
                }));
                return;
            }
            Request::ReleaseReverseMcp { session } => {
                #[cfg(target_os = "linux")]
                if let Some(reverse) = lock(&self.reverse).clone() {
                    reverse.release(conn.id, session.as_str());
                }
                #[cfg(not(target_os = "linux"))]
                let _ = session;
                return reply.send(Ok(Reply::Done));
            }
            Request::PreflightNativeRemote {
                session,
                cwd,
                launch,
            } => {
                let owner = conn.id;
                blocking(Box::new(move |host| {
                    if !lock(&host.connections).contains_key(&owner) {
                        return Err(PortUnavailable(
                            "native preflight connection retired".into(),
                        ));
                    }
                    #[cfg(target_os = "linux")]
                    {
                        let reverse = lock(&host.reverse)
                            .clone()
                            .ok_or_else(|| PortUnavailable("reverse MCP unavailable".into()))?;
                        let intent = SpawnIntent {
                            remote_launch: Some(launch),
                            command: None,
                            argv: None,
                            shell: None,
                            cwd,
                            cols: 80,
                            rows: 24,
                            shell_integration: false,
                            starship_preset: None,
                            api_port: None,
                            env_remove: vec![],
                            env: vec![],
                            agent_env: Vec::new(),
                            login_path: None,
                        };
                        let spec = intent.resolve(session.as_str(), &host.environment);
                        reverse
                            .prepare(owner, session.as_str(), spec, Some(launch))
                            .map_err(PortUnavailable)?;
                        Ok(Reply::Done)
                    }
                    #[cfg(not(target_os = "linux"))]
                    {
                        let _ = (session, cwd, launch);
                        Err(PortUnavailable(
                            "native remote preflight unavailable".into(),
                        ))
                    }
                }))
            }
            Request::ProbeNativeRuntime { cwd, launch } => blocking(Box::new(move |host| {
                let intent = SpawnIntent {
                    remote_launch: None,
                    command: None,
                    argv: None,
                    shell: None,
                    cwd,
                    cols: 80,
                    rows: 24,
                    shell_integration: false,
                    starship_preset: None,
                    api_port: None,
                    env_remove: vec![],
                    env: vec![],
                    agent_env: Vec::new(),
                    login_path: None,
                };
                let spec = intent.resolve("native-runtime-probe", &host.environment);
                crate::native_launch::runtime_installed(&spec, launch)
                    .map(|value| Reply::Flag { value })
                    .map_err(PortUnavailable)
            })),
            Request::PrepareReverseMcp {
                session,
                binding_nonce,
                server_ids,
                remote_entries,
                launch,
                agent_startup,
            } => {
                #[cfg(target_os = "linux")]
                let connections = lock(&self.connections);
                #[cfg(target_os = "linux")]
                let result = if !connections.contains_key(&conn.id) {
                    Err("connection closed".into())
                } else {
                    lock(&self.reverse)
                        .as_ref()
                        .ok_or_else(|| "reverse MCP unavailable".to_string())
                        .and_then(|reverse| {
                            reverse.provision_agent(
                                conn,
                                &session,
                                binding_nonce,
                                server_ids,
                                remote_entries,
                                launch,
                                agent_startup,
                            )?;
                            // Handed back to this desktop if it
                            // goes away and returns.
                            reverse.set_desktop(
                                session.as_str(),
                                connections.get(&conn.id).and_then(Held::identity),
                            );
                            Ok(())
                        })
                };
                #[cfg(not(target_os = "linux"))]
                let result: Result<(), String> = {
                    let _ = (
                        session,
                        binding_nonce,
                        server_ids,
                        remote_entries,
                        launch,
                        agent_startup,
                    );
                    Err("reverse MCP unavailable".into())
                };
                return reply.send(result.map(|_| Reply::Done));
            }
            Request::ClaimWritable { session } => {
                // Lock order is connections, then writable; disconnect first removes
                // the connection, so a late claim can never recreate its ownership.
                let connections = lock(&self.connections);
                let result = if !connections.contains_key(&conn.id) {
                    Err("writable attachment connection closed".into())
                } else if self.terminal.has_session(&session) != Ok(true) {
                    Err("writable attachment session not found".into())
                } else {
                    match lock(&self.writable).claim(&session, conn.id) {
                        Ok(()) => Ok(Reply::Done),
                        Err(_) => Ok(Reply::WritableBusy),
                    }
                };
                drop(connections);
                return reply.send(result);
            }
            Request::ReleaseWritable { session } => {
                let result = lock(&self.writable).release(&session, conn.id);
                return reply.send(result.map(done));
            }
            Request::OpenInput { session, channel } => {
                let mut inputs = lock(&conn.inputs);
                if channel < FIRST_STREAM_CHANNEL || inputs.contains_key(&channel) {
                    drop(inputs);
                    return reply.send(Err(format!(
                        "write_pty: input channel {channel} cannot be opened"
                    )));
                }
                inputs.insert(channel, session);
                return reply.send(Ok(Reply::Done));
            }
            Request::CloseInput { channel } => {
                lock(&conn.inputs).remove(&channel);
                return reply.send(Ok(Reply::Done));
            }
            Request::Spawn {
                intent,
                registration,
            } => {
                let mut spawning = lock(&self.spawning);
                if spawning.draining {
                    return reply.send(Err("terminal server is draining".into()));
                }
                let cancelled = spawning
                    .cancelled
                    .remove(&registration.session_id)
                    .is_some_and(|at| at.elapsed() < CANCEL_WINDOW);
                if cancelled {
                    drop(spawning);
                    // The lost spawn's caller already opened it elsewhere; a
                    // later caller may too.
                    return reply.send(Err(format!(
                        "spawn_pty: session {} was cancelled",
                        registration.session_id
                    )));
                }
                // Architecture: admission remains pending until the tab is registered.
                let admission =
                    match codesense_pty_core::remote_workspace::removal::admit_spawn(&intent.cwd) {
                        Ok(admission) => admission,
                        Err(error) => return reply.send(Err(error)),
                    };
                // In flight from the moment it is read, so a kill that follows
                // it on any connection is never lost.
                spawning
                    .in_flight
                    .entry(registration.session_id.clone())
                    .or_default()
                    .spawns += 1;
                drop(spawning);
                self.note_opener(
                    conn.id,
                    &PtySessionId::new(registration.session_id.as_str()),
                );
                let host = host.clone();
                let owner = conn.id;
                self.runtime.spawn(async move {
                    let _admission = admission;
                    host.spawn(owner, intent, registration).await
                })
            }
            // Inline: a resize is one quick ioctl, and applying resizes in
            // order is what keeps the PTY at the last size asked for.
            Request::Resize {
                viewer,
                session,
                cols,
                rows,
            } => {
                let _permit = match WritePermit::acquire(&self.writable, &session, conn.id) {
                    Ok(permit) => permit,
                    Err(error) => return reply.send(Err(error)),
                };
                return reply.send(
                    self.terminal
                        .resize(&viewer, &session, cols, rows)
                        .map(done)
                        .map_err(String::from),
                );
            }
            Request::Kill { session } => {
                #[cfg(target_os = "linux")]
                self.owned_supervisors.begin_drain(session.as_str());
                if let Some(in_flight) = lock(&self.spawning).in_flight.get_mut(session.as_str()) {
                    in_flight.killed = true;
                }
                blocking(Box::new(move |host| {
                    host.kill_with_supervisors(&session)
                        .map_err(PortUnavailable)
                }))
            }
            Request::CancelSpawn { session } => {
                #[cfg(target_os = "linux")]
                self.owned_supervisors.begin_drain(session.as_str());
                let mut spawning = lock(&self.spawning);
                if let Some(in_flight) = spawning.in_flight.get_mut(session.as_str()) {
                    // Answered by the spawn once it is torn down.
                    in_flight.killed = true;
                    in_flight.cancels.push(reply);
                    return;
                }
                // The lost spawn may still be unread even when an older session
                // of this id is registered: a respawn read later is refused.
                let registered = self.terminal.has_session(&session) == Ok(true);
                spawning
                    .cancelled
                    .retain(|_, at| at.elapsed() < CANCEL_WINDOW);
                spawning
                    .cancelled
                    .insert(session.as_str().to_string(), Instant::now());
                drop(spawning);
                blocking(Box::new(move |host| {
                    if registered {
                        return host
                            .kill_with_supervisors(&session)
                            .map_err(PortUnavailable);
                    }
                    Ok(Reply::Done)
                }))
            }
            Request::WakeDeliver { mut request } => {
                // The originating desktop is the connection's own identity,
                // never what the request claims.
                request.originator.desktop = self.identity_of(conn.id).unwrap_or_default();
                blocking(Box::new(move |host| {
                    let wake = host.wake().ok_or_else(wake_unavailable)?;
                    wake.deliver(*request)
                        .map(|outcome| Reply::WakeDelivery {
                            outcome: Box::new(outcome),
                        })
                        .map_err(PortUnavailable)
                }))
            }
            Request::WakeCancel {
                session,
                idempotency_key,
            } => {
                let originator = codesense_pty_core::session_wake::Originator {
                    desktop: self.identity_of(conn.id).unwrap_or_default(),
                    routine_host: None,
                };
                blocking(Box::new(move |host| {
                    let wake = host.wake().ok_or_else(wake_unavailable)?;
                    wake.cancel_delivery(&originator, &session, &idempotency_key)
                        .map(|outcome| Reply::WakeDelivery {
                            outcome: Box::new(outcome),
                        })
                        .ok_or_else(|| PortUnavailable("no such delivery".into()))
                }))
            }
            Request::WakeTimerSet { mut spec } => {
                spec.originator.desktop = self.identity_of(conn.id).unwrap_or_default();
                blocking(Box::new(move |host| {
                    let wake = host.wake().ok_or_else(wake_unavailable)?;
                    wake.set_timer(*spec)
                        .map(|timer| Reply::WakeTimer {
                            timer: Box::new(timer),
                        })
                        .map_err(PortUnavailable)
                }))
            }
            Request::WakeTimerList { session } => blocking(Box::new(move |host| {
                let wake = host.wake().ok_or_else(wake_unavailable)?;
                Ok(Reply::WakeTimers {
                    timers: wake.timers(session.as_ref()),
                })
            })),
            Request::WakeTimerCancel { session, timer_id } => blocking(Box::new(move |host| {
                let wake = host.wake().ok_or_else(wake_unavailable)?;
                Ok(flag(wake.cancel_timer(&session, &timer_id)))
            })),
            Request::InboxWakeHolds { generation, holds } => {
                let desktop = self.identity_of(conn.id);
                blocking(Box::new(move |host| {
                    if holds
                        .iter()
                        .any(|h| !host.inbox_audience(&h.session).admits(desktop.as_deref()))
                    {
                        return Err(PortUnavailable(
                            "wake holds belong to the session's desktop".into(),
                        ));
                    }
                    host.terminal
                        .activity()
                        .inbox()
                        .set_wake_holds(generation, holds);
                    Ok(Reply::Done)
                }))
            }
            Request::InboxMail {
                session,
                notice,
                token,
            } => {
                let desktop = self.identity_of(conn.id);
                blocking(Box::new(move |host| {
                    // A session's notices are its own desktop's: another one's inbox
                    // is not this session's.
                    if !host
                        .inbox_audience(session.as_str())
                        .admits(desktop.as_deref())
                    {
                        return Err(PortUnavailable(
                            "the session is bound to another desktop".into(),
                        ));
                    }
                    host.terminal
                        .activity()
                        .inbox_mail(session.as_str(), notice, token)
                        .map_err(PortUnavailable)?;
                    Ok(Reply::Done)
                }))
            }
            Request::LocalMcpSessions { sessions } => {
                let result = match lock(&self.connections).get_mut(&conn.id) {
                    Some(held) if held.client.is_some() => {
                        held.local_mcp_sessions = sessions.into_iter().collect();
                        Ok(Reply::Done)
                    }
                    _ => Err("local MCP readiness requires an admitted app connection".into()),
                };
                return reply.send(result);
            }
            Request::InboxWaiters => {
                let desktop = self.identity_of(conn.id);
                blocking(Box::new(move |host| {
                    let waiters = host
                        .terminal
                        .activity()
                        .inbox_waiters()
                        .into_iter()
                        .filter(|state| {
                            host.inbox_audience(state.session.as_str())
                                .admits(desktop.as_deref())
                        })
                        .collect();
                    Ok(Reply::InboxWaiters { waiters })
                }))
            }
            Request::WakeOutcomes { since } => {
                // Opt in before reading, as for activity: an outcome racing
                // the read is pushed too, and its seq tells them apart.
                conn.wake.store(true, std::sync::atomic::Ordering::Release);
                blocking(Box::new(move |host| {
                    let wake = host.wake().ok_or_else(wake_unavailable)?;
                    Ok(Reply::WakeOutcomes {
                        log: Box::new(wake.outcomes(since)),
                    })
                }))
            }
            Request::RoutineArm { arm } => {
                // Stamped with the desktop's own identity, so its
                // agents (and their queued calls) go back to it only.
                let desktop = self.identity_of(conn.id).unwrap_or_default();
                blocking(Box::new(move |host| {
                    let arms = host.arms().ok_or_else(arms_unavailable)?;
                    Ok(Reply::RoutineArmAnswer {
                        answer: Box::new(arms.arm(*arm, &desktop)),
                    })
                }))
            }
            Request::RoutineDisarm { key, epoch } => blocking(Box::new(move |host| {
                let arms = host.arms().ok_or_else(arms_unavailable)?;
                Ok(Reply::RoutineArmAnswer {
                    answer: Box::new(arms.disarm(&key, epoch)),
                })
            })),
            Request::RoutineArms => blocking(Box::new(move |host| {
                let arms = host.arms().ok_or_else(arms_unavailable)?;
                Ok(Reply::RoutineArms {
                    list: Box::new(arms.list()),
                })
            })),
            Request::RoutineArmRuns { since } => blocking(Box::new(move |host| {
                let arms = host.arms().ok_or_else(arms_unavailable)?;
                Ok(Reply::RoutineArmRuns {
                    log: Box::new(arms.runs(since)),
                })
            })),
            Request::RoutineArmFireNow { key, epoch, task } => blocking(Box::new(move |host| {
                let arms = host.arms().ok_or_else(arms_unavailable)?;
                Ok(Reply::RoutineArmFired {
                    answer: Box::new(arms.fire_now(&key, epoch, task)),
                })
            })),
            Request::RoutineArmComplete {
                session,
                run_id,
                outcome,
                summary,
            } => blocking(Box::new(move |host| {
                let arms = host.arms().ok_or_else(arms_unavailable)?;
                Ok(Reply::RoutineArmCompleted {
                    answer: Box::new(arms.complete_answer(
                        &session,
                        run_id.as_deref(),
                        &outcome,
                        &summary,
                    )),
                })
            })),
            Request::RoutineArmBindSecret { secret, desktop } => {
                return reply.send(
                    self.set_routine_bind(conn, secret, desktop)
                        .map(|()| Reply::Done),
                );
            }
            Request::IsIdle { session } => blocking(Box::new(move |host| {
                host.terminal.is_idle(&session).map(flag)
            })),
            Request::SubmitLine {
                session,
                line,
                quiet_ms,
                confirm_ms,
                timeout_ms,
                max_nudges,
            } => {
                let permit = match WritePermit::acquire(&self.writable, &session, conn.id) {
                    Ok(permit) => permit,
                    Err(error) => return reply.send(Err(error)),
                };
                blocking(Box::new(move |host| {
                    let _permit = permit;
                    // One budget for both: waiting for the prompt, then typing.
                    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
                    if !host.agent_prompt_ready(&session, deadline) {
                        return Err(PortUnavailable(format!(
                            "{}: the agent's prompt is not taking input yet; nothing was typed",
                            codesense_pty_core::session_wake::AGENT_INPUT_NOT_READY
                        )));
                    }
                    host.input_seen(&session, line.as_bytes());
                    let activity = host.wake_activity(&session);
                    let command = cli_command(&line, activity.as_ref());
                    // A command may start no turn (`/model`, `/help`): its output is
                    // all there is to tell it was taken.
                    let idle_at = (!command)
                        .then(|| Host::idle_turn(activity.as_ref()))
                        .flatten();
                    // A CLI with bracketed paste on is told where the text ends,
                    // so its Enter is never read as part of the paste. A command
                    // goes as typed, to be read as a command.
                    let bracket = !command
                        && pastes_bracketed(activity.as_ref())
                        && host.wake_bracketed_paste(&session) == Some(true);
                    // Typing keeps the time it needs to settle and confirm.
                    let left = deadline.saturating_duration_since(Instant::now()).max(
                        Duration::from_millis(
                            quiet_ms.saturating_add(confirm_ms.saturating_mul(2)),
                        ),
                    );
                    let outcome = host.terminal.submit_line_bracketed(
                        &session,
                        &line,
                        quiet_ms,
                        confirm_ms,
                        left.as_millis() as u64,
                        max_nudges,
                        bracket,
                    )?;
                    Ok(flag(host.submit_confirmed(
                        &session,
                        idle_at,
                        outcome.submitted,
                    )))
                }))
            }
            Request::SubmitShellLine {
                session,
                line,
                quiet_ms,
                timeout_ms,
            } => {
                let permit = match WritePermit::acquire(&self.writable, &session, conn.id) {
                    Ok(permit) => permit,
                    Err(error) => return reply.send(Err(error)),
                };
                self.input_seen(&session, line.as_bytes());
                blocking(Box::new(move |host| {
                    let _permit = permit;
                    host.terminal
                        .submit_shell_line(&session, &line, quiet_ms, timeout_ms)
                        .map(|outcome| flag(outcome.submitted))
                }))
            }
            Request::HandoffWarmMember {
                member,
                session,
                target_cwd,
                cols,
                rows,
                meta,
            } => {
                let opener = conn.id;
                blocking(Box::new(move |host| {
                    host.terminal.handoff_warm_member(
                        &member,
                        &session,
                        &target_cwd,
                        cols,
                        rows,
                        meta,
                    )?;
                    host.note_opener(opener, &session);
                    // The member's id is gone without an exit: close its output.
                    host.relay.close_session(&member);
                    Ok(Reply::Done)
                }))
            }
            // A client that predates the scoped reap still gets the reap it
            // always got: every non-persisting session on this server.
            Request::ReapNonPersisting => blocking(Box::new(move |host| {
                log::info!("exit reap of every non-persisting session, asked by an older client");
                host.terminal.reap_non_persisting().map(done)
            })),
            // The quitting client's own non-persisting sessions, and as the
            // last client what `last_client_reap` allows: nothing an app
            // left behind (a crash, a force quit) outlives the last app, and
            // an app that may only be reconnecting keeps its tabs.
            Request::ReapOwnedNonPersisting { sessions } => {
                let owner = conn.id;
                blocking(Box::new(move |host| {
                    let (unowned_pool, leftovers) = host.last_client_reap(owner);
                    host.terminal
                        .reap_owned_non_persisting(&sessions, unowned_pool, leftovers);
                    Ok(Reply::Done)
                }))
            }
            Request::Subscribe { session, viewport } => {
                let owner = conn.id;
                blocking(Box::new(move |host| {
                    host.attach(owner, &session, &viewport)
                }))
            }
            // Only the connection that attached a viewport detaches it.
            Request::Unsubscribe { stream } => {
                let owner = conn.id;
                blocking(Box::new(move |host| {
                    let owned = lock(&host.connections)
                        .get_mut(&owner)
                        .is_some_and(|held| held.viewports.remove(&stream));
                    if !owned {
                        return Ok(flag(false));
                    }
                    host.detach(stream).map(flag)
                }))
            }
            Request::HasSession { session } => blocking(Box::new(move |host| {
                host.terminal.has_session(&session).map(flag)
            })),
            Request::AliveSessionIds { sessions } => blocking(Box::new(move |host| {
                let sessions = host.terminal.alive_session_ids(&sessions)?;
                Ok(Reply::Sessions { sessions })
            })),
            Request::ClaimSessions { sessions } => {
                let claimer = conn.id;
                blocking(Box::new(move |host| {
                    host.claim_sessions(claimer, &sessions);
                    Ok(Reply::Done)
                }))
            }
            Request::EndUnownedSession { session } => {
                blocking(Box::new(move |host| host.end_unowned(&session).map(done)))
            }
            Request::MachineStatsWatch { watch } => {
                conn.machine_stats
                    .store(watch, std::sync::atomic::Ordering::Release);
                lock(&self.machine_stats_schedule).watch(conn.id, watch, Instant::now());
                self.start_machine_stats();
                self.machine_stats_changed.notify_one();
                blocking(Box::new(move |host| {
                    let sample = if watch {
                        Some(host.machine_stats_sample()?)
                    } else {
                        None
                    };
                    Ok(Reply::MachineStats { sample })
                }))
            }
            Request::MachineStats => blocking(Box::new(|host| {
                Ok(Reply::MachineStats {
                    sample: Some(host.machine_stats_sample()?),
                })
            })),
            Request::ServerStatus => {
                let asker = conn.id;
                blocking(Box::new(move |host| {
                    Ok(Reply::ServerStatus {
                        status: Box::new(host.server_status(asker)),
                    })
                }))
            }
            Request::RedeliverExits { sessions } => blocking(Box::new(move |host| {
                let sessions = host.terminal.redeliver_exits(&sessions)?;
                Ok(Reply::Sessions { sessions })
            })),
            Request::LiveSessionIds => blocking(Box::new(|host| {
                let sessions = host.terminal.live_session_ids()?;
                Ok(Reply::Sessions { sessions })
            })),
            Request::MutateActivity { mutation } => blocking(Box::new(move |host| {
                host.terminal
                    .activity_mutate(mutation)
                    .map(|snapshot| Reply::ActivityUpdated {
                        snapshot: Box::new(snapshot),
                    })
            })),
            Request::PrepareActivity { session, provider } => blocking(Box::new(move |host| {
                let socket = lock(&host.activity_service)
                    .as_ref()
                    .map(|service| service.path().to_string_lossy().into_owned())
                    .ok_or_else(|| PortUnavailable("activity ingress unavailable".into()))?;
                let writer = host
                    .terminal
                    .activity()
                    .prepare_native(session.as_str(), provider, &socket)
                    .map_err(|error| PortUnavailable(error.into()))?;
                Ok(Reply::ActivityWriter {
                    writer: Box::new(writer),
                })
            })),
            Request::PrepareShellActivity { session, provider } => {
                blocking(Box::new(move |host| {
                    host.terminal
                        .activity_prepare_shell(&session, provider)
                        .map(|launch| Reply::ShellActivity {
                            launch: Box::new(launch),
                        })
                }))
            }
            Request::SessionOutput {
                session,
                identity,
                turn_generation,
            } => blocking(Box::new(move |host| {
                host.terminal
                    .session_output(&session, &identity, turn_generation)
                    .map(|record| Reply::SessionOutput { record })
            })),
            Request::ActivitySnapshot => {
                // Opt in before reading: events racing the snapshot carry the
                // same invocation revision and can be reconciled by the client.
                conn.activity
                    .store(true, std::sync::atomic::Ordering::Release);
                blocking(Box::new(|host| {
                    Ok(Reply::ActivitySnapshot {
                        snapshots: host.terminal.activity().snapshots(),
                    })
                }))
            }
            Request::AgentsAlive { queries } => blocking(Box::new(move |host| {
                if queries.len() > 1024 || queries.iter().any(|(_, name)| name.len() > 256) {
                    return Err(PortUnavailable("agent probe exceeds bounds".into()));
                }
                let alive = host.terminal.agents_alive(&queries)?;
                Ok(Reply::AgentsAlive { alive })
            })),
            Request::LiveChildPids { sessions } => blocking(Box::new(move |host| {
                let pids = host.terminal.live_child_pids(&sessions)?;
                Ok(Reply::Pids { pids })
            })),
            Request::SessionLenAndDead => blocking(Box::new(|host| {
                let counts = host.terminal.session_len_and_dead()?;
                Ok(Reply::Counts { counts })
            })),
            Request::LiveSessionHomeDirs => blocking(Box::new(|host| {
                let dirs = host.terminal.live_session_home_dirs()?;
                Ok(Reply::Dirs { dirs })
            })),
            Request::SessionCwd { session } => blocking(Box::new(move |host| {
                let cwd = host.terminal.session_cwd(&session)?;
                Ok(Reply::Cwd { cwd })
            })),
            Request::SessionPrompt { session } => blocking(Box::new(move |host| {
                let cwd = host.terminal.session_cwd(&session)?;
                let prompt = codesense_pty_core::git_prompt::session_prompt(cwd, account_home());
                Ok(Reply::Prompt {
                    prompt: Box::new(prompt),
                })
            })),
            Request::AccountHome => blocking(Box::new(|_| {
                Ok(Reply::Cwd {
                    cwd: account_home(),
                })
            })),
            Request::UsageRead { after_seq, limit } => blocking(Box::new(move |_| {
                let spool = crate::usage_receiver::advertised_spool().ok_or_else(|| {
                    PortUnavailable("usage receiver is not running on this host".into())
                })?;
                let limit = (limit as usize).min(crate::usage_spool::MAX_READ_RECORDS);
                // Asking past `after_seq` says the desktop stored it, which lets the spool prune.
                let read = spool
                    .lock()
                    .map_err(|_| PortUnavailable("usage spool unavailable".into()))?
                    .read_past(after_seq, limit)
                    .map_err(|error| PortUnavailable(error.to_string()))?;
                Ok(Reply::UsageRead {
                    read: Box::new(read),
                })
            })),
            Request::ReadText {
                session,
                lines,
                mode,
            } => blocking(Box::new(move |host| {
                let text = host.terminal.read_terminal_text(&session, lines, mode)?;
                Ok(Reply::Text { text })
            })),
            Request::DebugDump { session } => blocking(Box::new(move |host| {
                let dump = host.terminal.debug_dump(&session)?;
                Ok(Reply::Dump {
                    dump: Box::new(dump),
                })
            })),
            Request::WorktreeUsage { source } => blocking(Box::new(move |host| {
                host.terminal
                    .worktree_usage(&source)
                    .map(|usage| Reply::WorktreeUsage { usage })
            })),
            Request::LayoutSnapshot {
                viewer,
                client_boot_id,
            } => {
                blocking(Box::new(move |host| {
                    layout(host.terminal.layout_snapshot(&viewer, &client_boot_id).map(
                        |snapshot| Reply::Layout {
                            snapshot: Box::new(snapshot),
                        },
                    ))
                }))
            }
            Request::ReserveTab { request } => {
                let owner = conn.id;
                blocking(Box::new(move |host| {
                    layout(host.reserve_tab(owner, request).map(done))
                }))
            }
            Request::ReconcileTab { session } => {
                let owner = conn.id;
                blocking(Box::new(move |host| {
                    layout(host.reconcile_tab(owner, session))
                }))
            }
            Request::FinishTab { session, spawned } => {
                let owner = conn.id;
                blocking(Box::new(move |host| {
                    layout(host.finish_tab(owner, session, spawned))
                }))
            }
            Request::BeginRespawn { session } => {
                let owner = conn.id;
                blocking(Box::new(move |host| {
                    layout(host.begin_respawn(owner, session))
                }))
            }
            Request::FinishRespawn { session, spawned } => {
                let owner = conn.id;
                blocking(Box::new(move |host| {
                    if let Some(held) = lock(&host.connections).get_mut(&owner) {
                        held.respawns.remove(&session);
                    }
                    layout(
                        host.terminal
                            .finish_respawn(&session, spawned)
                            .map(|entry| Reply::Tab {
                                entry: Box::new(entry),
                            }),
                    )
                }))
            }
            Request::CloseTab {
                viewer,
                session,
                mode,
            } => blocking(Box::new(move |host| {
                layout(
                    host.terminal
                        .close_tab(&viewer, &session, mode)
                        .map(|outcome| Reply::Closed { outcome }),
                )
            })),
            Request::PlaceTab {
                session,
                workspace,
                placement,
                index,
                descriptor,
            } => blocking(Box::new(move |host| {
                layout(
                    host.terminal
                        .place_tab(&session, &workspace, placement, index, descriptor)
                        .map(done),
                )
            })),
            Request::RenameTab { session, name } => blocking(Box::new(move |host| {
                layout(host.terminal.rename_tab(&session, name).map(done))
            })),
            Request::MoveTab { session, to_index } => blocking(Box::new(move |host| {
                layout(host.terminal.move_tab(&session, to_index).map(done))
            })),
            Request::SetActive {
                viewer,
                workspace,
                session,
            } => blocking(Box::new(move |host| {
                layout(
                    host.terminal
                        .set_active(&viewer, &workspace, session.as_ref())
                        .map(done),
                )
            })),
            Request::SetGrid { workspace, grid } => blocking(Box::new(move |host| {
                layout(host.terminal.set_grid(&workspace, grid).map(done))
            })),
            Request::SetViewHint {
                viewer,
                session,
                hint,
            } => blocking(Box::new(move |host| {
                layout(
                    host.terminal
                        .set_view_hint(&viewer, &session, hint)
                        .map(done),
                )
            })),
            Request::SetTags { session, patch } => blocking(Box::new(move |host| {
                layout(host.terminal.set_tags(&session, patch).map(done))
            })),
            Request::SetDescriptor {
                workspace,
                descriptor,
            } => blocking(Box::new(move |host| {
                layout(
                    host.terminal
                        .set_descriptor(&workspace, descriptor)
                        .map(done),
                )
            })),
            Request::CloseWorkspace { workspace } => blocking(Box::new(move |host| {
                layout(host.terminal.close_workspace(&workspace).map(done))
            })),
        };
        // A request that panicked still gets an answer, so its caller never
        // waits out its timeout.
        self.runtime.spawn(async move {
            reply.send(
                task.await
                    .unwrap_or_else(|e| Err(format!("the session server failed the request: {e}"))),
            )
        });
    }

    /// Attach a viewport of `session` for connection `owner`, whose stream the
    /// connection's close detaches. A connection that closed while the attach
    /// settled gets nothing attached.
    fn attach(
        &self,
        owner: u64,
        session: &PtySessionId,
        viewport: &ViewportId,
    ) -> Result<Reply, PortUnavailable> {
        let result = self.terminal.subscribe(session, viewport)?;
        let stream = result.stream_id;
        // Bound until it is detached, so the session's exit closes its output
        // channel. An exit that raced this binding already unbound the
        // session's streams; its output ended first, so the channel closes
        // here instead of waiting for the detach.
        self.relay.bind_stream(stream, session);
        if self.terminal.output_ended(session) {
            self.relay.close_stream(stream);
        }
        let owned = lock(&self.connections)
            .get_mut(&owner)
            .map(|held| {
                held.viewed = true;
                held.viewports.insert(stream)
            })
            .is_some();
        if !owned {
            if let Err(error) = self.detach(stream) {
                log::warn!("detach of stream {} failed: {error}", stream.0);
            }
            return Err(PortUnavailable(
                "subscribe_terminal: the connection closed while attaching".to_string(),
            ));
        }
        Ok(Reply::Subscribed {
            result: Box::new(result),
        })
    }

    /// Detach `stream` and close its output channel; whether it was attached.
    fn detach(&self, stream: StreamId) -> Result<bool, PortUnavailable> {
        let detached = self.terminal.unsubscribe(stream);
        self.relay.close_stream(stream);
        detached
    }

    fn queue_write(self: &Arc<Self>, session: PtySessionId, job: WriteJob) {
        let mut inputs = lock(&self.relay.inputs);
        // An unknown session gets the runtime's own answer and no input thread.
        // Checked under the inputs lock: a removal retires the entry under the
        // same lock, so a thread is never started for a session already gone.
        if self.terminal.has_session(&session) != Ok(true) {
            drop(inputs);
            let WriteJob {
                data,
                reply,
                _permit,
            } = job;
            return reply.send(
                self.terminal
                    .write(&session, &data)
                    .map(|()| Reply::Done)
                    .map_err(String::from),
            );
        }
        let job = match inputs.get(&session) {
            Some(input) => match input.try_send(job) {
                Ok(()) => return,
                Err(mpsc::TrySendError::Full(job)) => {
                    return job.reply.send(Err(format!(
                        "write_pty: input for session {session} is backed up"
                    )))
                }
                Err(mpsc::TrySendError::Disconnected(job)) => job,
            },
            None => job,
        };
        let (input, jobs) = mpsc::sync_channel::<WriteJob>(INPUT_BACKLOG);
        let terminal = self.terminal.clone();
        let target = session.clone();
        let started = thread::Builder::new()
            .name("pty-input".into())
            .spawn(move || {
                for WriteJob {
                    data,
                    reply,
                    _permit,
                } in jobs
                {
                    let write = || {
                        terminal
                            .write(&target, &data)
                            .map(|()| Reply::Done)
                            .map_err(String::from)
                    };
                    let result = if matches!(&reply, Answer::Wake(_)) {
                        terminal
                            .activity()
                            .inbox()
                            .while_wake_allowed(target.as_str(), write)
                            .unwrap_or_else(|| Err("automatic wake held by spend cap".into()))
                    } else {
                        write()
                    };
                    drop(_permit);
                    reply.send(result);
                }
            });
        match started {
            Ok(_) => {
                let _ = input.try_send(job);
                inputs.insert(session, input);
            }
            Err(error) => job.reply.send(Err(format!(
                "Failed to start the PTY input thread: {error}"
            ))),
        }
    }

    /// Resolve the intent here, spawn it through the built-in local driver and
    /// register the session — the same three steps the app takes in-process. A
    /// kill that arrived while the spawn was in flight tears the session down
    /// once it is registered, and the spawn answers [`Reply::Cancelled`].
    async fn spawn(
        self: Arc<Self>,
        owner: u64,
        intent: SpawnIntent,
        registration: SessionRegistration,
    ) -> Result<Reply, String> {
        let session = registration.session_id.clone();
        let spawned = self
            .clone()
            .spawn_and_register(owner, intent, registration)
            .await;
        let (killed, cancels) = {
            let mut spawning = lock(&self.spawning);
            let in_flight = spawning.in_flight.entry(session.clone()).or_default();
            in_flight.spawns = in_flight.spawns.saturating_sub(1);
            let killed = in_flight.killed;
            let cancels = if in_flight.spawns == 0 {
                spawning
                    .in_flight
                    .remove(&session)
                    .map(|done| done.cancels)
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            (killed, cancels)
        };
        // A spawn killed while in flight is cancelled whether or not it
        // succeeded: the caller must not open it anywhere else. Its cancels are
        // answered once the session is gone.
        if killed {
            let host = self.clone();
            let cleanup = self
                .runtime
                .spawn_blocking(move || host.kill_with_supervisors(&PtySessionId::new(session)))
                .await
                .map_err(|_| "cancelled spawn cleanup task failed".to_string())
                .and_then(|result| result);
            for cancel in cancels {
                cancel.send(cleanup.clone());
            }
            cleanup?;
            return Ok(Reply::Cancelled);
        }
        let pid = spawned?;
        self.finish_uncertain_spawn(&PtySessionId::new(session));
        Ok(Reply::Spawned { pid })
    }

    async fn spawn_and_register(
        self: Arc<Self>,
        owner: u64,
        intent: SpawnIntent,
        registration: SessionRegistration,
    ) -> Result<Option<u32>, String> {
        // Resolving writes the managed RC wrappers: off the async workers.
        let environment = self.environment.clone();
        let session = registration.session_id.clone();
        let resolving_session = session.clone();
        let activity_provider = intent.remote_launch.and_then(NativeLaunch::cli);
        let direct_argv = intent.argv.as_ref().is_some_and(|argv| !argv.is_empty());
        #[cfg(target_os = "linux")]
        let reverse = lock(&self.reverse).clone();
        // A tunnelled launch is prepared by the reverse MCP; a local one handed its whole
        // argv is prepared here, once its activity is bound.
        #[cfg(target_os = "linux")]
        let prepared_by_reverse = reverse.is_some();
        #[cfg(not(target_os = "linux"))]
        let prepared_by_reverse = false;
        let mut spec = self
            .runtime
            .spawn_blocking(move || {
                let launch = intent.remote_launch;
                #[cfg(target_os = "linux")]
                if let Some(reverse) = reverse {
                    // A saved Agent's variables come with its provisioned startup, under
                    // everything the spawn sets.
                    let intent = intent
                        .with_agent_startup(reverse.agent_startup(&resolving_session).as_ref());
                    let spec = intent.resolve(&resolving_session, &environment);
                    return reverse.prepare(owner, &resolving_session, spec, launch);
                }
                // Local managed agents supply their complete argv;
                // prepare_native below binds their activity without a tunnel.
                // A marked shell or native launch without argv still requires
                // live provisioning. Reject before resolution can write managed
                // RC wrappers, not just before its shell fallback can execute.
                if launch.is_some_and(|launch| launch.is_shell() || !direct_argv) {
                    return Err("remote launch requires live session MCP provisioning".into());
                }
                let _ = owner;
                Ok::<_, String>(intent.resolve(&resolving_session, &environment))
            })
            .await
            .map_err(|e| format!("PTY spawn resolution failed: {e}"))??;
        // Every shell names its host, so a CLI typed in it reports here.
        let host_root = lock(&self.activity_service)
            .as_ref()
            .and_then(|service| service.shell_env());
        if let Some((name, root)) = &host_root {
            spec.command.env(name, root);
        }
        let writer = activity_provider.and_then(|provider| {
            let socket = lock(&self.activity_service)
                .as_ref()
                .map(|service| service.path().to_string_lossy().into_owned())?;
            self.terminal
                .activity()
                .prepare_native(&session, provider, &socket)
                .ok()
        });
        if let Some(writer) = &writer {
            spec.command
                .env(codesense_pty_core::activity::SOCKET_ENV, &writer.socket);
            spec.command
                .env(codesense_pty_core::activity::TOKEN_ENV, &writer.token);
            // A local launch handed its whole argv runs no helper that would add its hooks,
            // so they are added here, as a machine adds a remote launch's.
            // Its hooks run the host root's stable helper, as a hand-typed CLI's do (and it
            // starts the CLI's engine): only once that helper is there.
            let helper = host_root
                .as_ref()
                .map(|(_, root)| format!("{root}/{}", codesense_pty_core::activity::HELPER_NAME))
                .filter(|helper| std::path::Path::new(helper).is_file());
            let direct = activity_provider.filter(|_| direct_argv && !prepared_by_reverse);
            if let (Some(provider), Some(helper)) = (direct, helper) {
                // Where a CLI whose settings ride a file gets its private copy.
                let directory = self
                    .terminal
                    .activity()
                    .launch_directory(&writer.token)
                    .map_err(|error| {
                        log::warn!("no launch directory for {session}; its hooks ride its argv only: {error}")
                    })
                    .ok();
                match self
                    .runtime
                    .spawn_blocking(move || {
                        prepare_direct_launch(&mut spec, provider, &helper, directory.as_deref())?;
                        Ok::<_, String>(spec)
                    })
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|prepared| prepared)
                {
                    Ok(prepared) => spec = prepared,
                    Err(error) => {
                        self.terminal
                            .activity()
                            .remove(&session, &writer.identity.session_incarnation);
                        return Err(format!("PTY spawn preparation failed: {error}"));
                    }
                }
            } else if direct.is_some() {
                // Not prepared (this host names no helper root): never told it qualified.
                crate::native_launch::set_release_qualified(&mut spec, None);
            }
            crate::native_launch::report_inbox_update(
                &spec,
                self.terminal.activity(),
                &writer.token,
            );
        }
        let result = async {
            let pty = LocalPtyRunner.spawn(spec).await.map_err(String::from)?;
            if lock(&self.spawning).draining {
                pty.discard();
                return Err("terminal server is draining".into());
            }
            let pid = pty.pid();
            let terminal = self.terminal.clone();
            self.runtime
                .spawn_blocking(move || terminal.register(pty, registration))
                .await
                .map_err(|e| format!("PTY registration task failed: {e}"))??;
            #[cfg(target_os = "linux")]
            if let (Some(reverse), Some(pid)) = (lock(&self.reverse).clone(), pid) {
                reverse.commit(owner, &session, pid)?;
            }
            Ok(pid)
        }
        .await;
        if result.is_err() {
            if let Some(writer) = writer {
                self.terminal
                    .activity()
                    .remove(&session, &writer.identity.session_incarnation);
            }
        }
        result
    }
}

/// Prepares `spec`, a local `provider` launch handed its whole argv, with its CLI's hooks
/// run by `helper` (which also starts the engine a CLI runs on), telling it whether its
/// release qualified (its `--version` is probed: off the async workers). One its host
/// cannot observe runs as given; a mandatory managed-home fold failure refuses the spawn.
#[cfg(any(test, feature = "test-support"))]
pub fn prepare_direct_launch_for_test(
    spec: &mut codesense_pty_core::runner::PtySpec,
    provider: codesense_pty_core::agent_cli::AgentCli,
    helper: &str,
) {
    prepare_direct_launch(spec, provider, helper, None)
        .expect("test managed configuration is writable");
}

fn prepare_direct_launch(
    spec: &mut codesense_pty_core::runner::PtySpec,
    provider: codesense_pty_core::agent_cli::AgentCli,
    helper: &str,
    directory: Option<&std::path::Path>,
) -> Result<(), String> {
    let home = codesense_pty_core::agent_cli::descriptor(provider)
        .config_home(|key| spec.command.get_env(key).map(|value| value.to_os_string()));
    let (qualified, update_required) = prepare_direct_argv_in(
        spec.command.get_argv_mut(),
        provider,
        helper,
        home,
        directory,
    )?;
    crate::native_launch::set_release_qualified(
        spec,
        if qualified {
            Some(true)
        } else if update_required {
            Some(false)
        } else {
            None
        },
    );
    Ok(())
}

/// `argv` prepared as [`prepare_direct_launch`] says: whether its release qualified.
#[cfg(test)]
fn prepare_direct_argv(
    argv: &mut Vec<std::ffi::OsString>,
    provider: codesense_pty_core::agent_cli::AgentCli,
    helper: &str,
    directory: Option<&std::path::Path>,
) -> bool {
    prepare_direct_argv_in(argv, provider, helper, None, directory)
        .expect("test launch configuration")
        .0
}

fn prepare_direct_argv_in(
    argv: &mut Vec<std::ffi::OsString>,
    provider: codesense_pty_core::agent_cli::AgentCli,
    helper: &str,
    home: Option<std::path::PathBuf>,
    directory: Option<&std::path::Path>,
) -> Result<(bool, bool), String> {
    let Some((program, arguments)) = argv.split_first() else {
        return Ok((false, false));
    };
    let prepared = codesense_pty_core::activity::prepare_direct(
        provider, program, arguments, helper, home, directory,
    );
    if let Some(error) = prepared.fatal_error {
        return Err(error);
    }
    let Some((arguments, qualified)) = prepared.arguments else {
        return Ok((false, prepared.inbox_update_required));
    };
    *argv =
        codesense_pty_core::activity::launch_argv(helper.as_ref(), provider, program, &arguments);
    Ok((qualified, prepared.inbox_update_required))
}

fn current_process_start() -> Result<u64, String> {
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", std::process::id()))
        .map_err(|e| e.to_string())?;
    stat.rsplit_once(") ")
        .and_then(|(_, f)| f.split_whitespace().nth(19))
        .ok_or_else(|| "malformed process stat".to_string())?
        .parse()
        .map_err(|_| "invalid process start".to_string())
}

impl Host {
    pub(crate) fn kill_with_supervisors(&self, session: &PtySessionId) -> Result<Reply, String> {
        #[cfg(target_os = "linux")]
        let deadline = Instant::now() + crate::owned_supervisors::DRAIN_TIME;
        #[cfg(target_os = "linux")]
        self.owned_supervisors.begin_drain(session.as_str());
        let terminal = self.terminal.kill(session);
        #[cfg(target_os = "linux")]
        self.owned_supervisors
            .wait(Some(session.as_str()), deadline)?;
        #[cfg(target_os = "linux")]
        if let Some(reverse) = lock(&self.reverse).clone() {
            reverse.cleanup_settled(Some(session.as_str()))?;
        }
        terminal.map(|()| Reply::Done).map_err(String::from)
    }

    fn retire_sessions_for_replacement(
        &self,
        mut receipt: Reply,
        deadline: Instant,
    ) -> Result<Reply, String> {
        #[cfg(test)]
        if let Some(error) = lock(&self.replacement_drain_error).take() {
            return Err(error);
        }
        #[cfg(target_os = "linux")]
        self.owned_supervisors.begin_all();
        let terminal = self.terminal.drain_for_replacement(deadline);
        #[cfg(target_os = "linux")]
        self.owned_supervisors.wait(None, deadline)?;
        #[cfg(target_os = "linux")]
        if let Some(reverse) = lock(&self.reverse).clone() {
            reverse.cleanup_settled(None)?;
        }
        let sessions = terminal?;
        let Reply::Retired {
            sessions: count, ..
        } = &mut receipt
        else {
            return Err("invalid replacement receipt".into());
        };
        *count = u32::try_from(sessions).unwrap_or(u32::MAX);
        self.write_retirement_receipt(&receipt)?;
        Ok(receipt)
    }

    fn write_retirement_receipt(&self, receipt: &Reply) -> Result<(), String> {
        self.write_replacement_evidence("retirement", receipt)
    }

    fn write_replacement_evidence(&self, kind: &str, receipt: &Reply) -> Result<(), String> {
        let root = self
            .replacement_root
            .as_ref()
            .ok_or("replacement unsupported")?;
        crate::replacement_evidence::write(root, kind, receipt)
    }
}

/// The account this host runs as: its home, when that is an absolute, existing
/// directory, with any symlink resolved once here, so paths derived from it
/// can be walked without following links below it. Read here, where the
/// account lives, never derived from a name.
fn account_home() -> Option<PathBuf> {
    crate::remote_workspace::resolved_account_home()
}

#[cfg(test)]
mod tests {
    fn snapshot(
        provider: codesense_pty_core::agent_cli::AgentCli,
        invocation: &str,
        generation: u64,
        state: codesense_pty_core::activity::State,
    ) -> codesense_pty_core::activity::Snapshot {
        codesense_pty_core::activity::Snapshot {
            session: "s".into(),
            identity: codesense_pty_core::activity::Identity {
                host_boot_id: "b".into(),
                session_incarnation: "i".into(),
                invocation_id: invocation.into(),
            },
            revision: 1,
            turn_generation: generation,
            provider,
            state,
            observed: true,
            limited: false,
            exit: None,
            asks: Vec::new(),
            reason: None,
            tool_open: None,
            activity_age_ms: None,
            subagents_open: 0,
        }
    }

    #[test]
    fn a_local_launch_handed_its_whole_argv_gets_its_clis_hooks_and_engine_once_its_release_qualified(
    ) {
        use codesense_pty_core::agent_cli::AgentCli;
        use std::ffi::OsString;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fake = |name: &str, version: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\necho '{version}'\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            codesense_pty_core::agent_cli::version_cache::known_release_for_test(&path, version);
            path.into_os_string()
        };
        let launch = |program: &OsString| -> Vec<OsString> {
            let mut argv = vec![program.clone()];
            argv.extend(["--model", "m", "--", "hi"].map(OsString::from));
            argv
        };
        // Every fake is written before any is run.
        let codex = fake("codex", "codex-cli 0.162.1");
        let floor = fake("floor-codex", "codex-cli 0.159.3");
        let below = fake("below-codex", "codex-cli 0.159.2");
        let old = fake("old-codex", "codex-cli 0.150.0");
        let claude = fake("claude", "2.1.300 (Claude Code)");
        let qualified = |program: &OsString| {
            let mut argv = launch(program);
            prepare_direct_argv(&mut argv, AgentCli::Codex, "/host/activity-helper", None)
                .then_some(argv)
        };
        // A qualified Codex runs on its engine, which the helper starts: its observer and
        // its inbox checks ride both its own options and its engine's, ahead of its prompt.
        let argv = qualified(&codex).expect("a qualified Codex is prepared");
        assert_eq!(
            argv[..2],
            [
                "/host/activity-helper",
                codesense_pty_core::activity::ENGINE_ARG
            ]
            .map(OsString::from)
        );
        let engine_launch = codesense_pty_core::activity::launch_of(&argv).unwrap();
        assert_eq!(engine_launch.program, codex.to_str().unwrap());
        assert_eq!(engine_launch.operands, ["--", "hi"]);
        for (form, options) in [
            ("standalone", &engine_launch.standalone),
            ("engine", &engine_launch.engine),
        ] {
            assert!(
                options
                    .iter()
                    .any(|arg| arg.contains("--activity-hook codex")),
                "{form}"
            );
            for event in ["UserPromptSubmit", "PostToolUse"] {
                assert!(
                    options
                        .iter()
                        .any(|arg| arg.starts_with(&format!("hooks.{event}="))
                            && arg.contains(&format!("--inbox-wait codex {event}"))),
                    "{form} {event}"
                );
            }
        }
        // Its floor qualifies; one older than its floor runs as given.
        assert!(qualified(&floor).is_some());
        let mut argv = launch(&below);
        assert!(!prepare_direct_argv(
            &mut argv,
            AgentCli::Codex,
            "/host/activity-helper",
            None
        ));
        assert_eq!(argv, launch(&below));
        let mut argv = launch(&old);
        assert!(!prepare_direct_argv(
            &mut argv,
            AgentCli::Codex,
            "/host/activity-helper",
            None
        ));
        assert_eq!(argv, launch(&old));
        // A CLI whose settings ride a file, given nowhere to write a copy, runs as given.
        let mut argv = launch(&claude);
        assert!(!prepare_direct_argv(
            &mut argv,
            AgentCli::Claude,
            "/host/activity-helper",
            None
        ));
        assert_eq!(argv, launch(&claude));
        // Given one, a saved Agent's generated settings file is folded, with its inbox
        // hooks kept, into a private copy that the flag names instead. A Claude is
        // recognised by its help flag, never probed, so it is not told it qualified.
        let generated = dir.path().join("generated.json");
        let inbox = r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"x --inbox-wait claude Stop"}]}]}}"#;
        std::fs::write(&generated, inbox).unwrap();
        let out = tempfile::tempdir().unwrap();
        let mut argv = vec![
            claude.clone(),
            "--settings".into(),
            generated.clone().into(),
        ];
        assert!(!prepare_direct_argv(
            &mut argv,
            AgentCli::Claude,
            "/host/activity-helper",
            Some(out.path()),
        ));
        let at = argv.iter().position(|arg| arg == "--settings").unwrap();
        assert_ne!(argv[at + 1], generated.as_os_str());
        let copy = std::fs::read_to_string(&argv[at + 1]).unwrap();
        // The `Stop` hooks, cut from the settings text: the copy is written as compact JSON with
        // sorted keys, and `StopFailure` is the key that follows `Stop`.
        let stop = copy.split("\"Stop\":[").nth(1).unwrap();
        let stop = stop.split("\"StopFailure\"").next().unwrap().to_owned();
        assert!(stop.contains("--activity-hook claude"), "{stop}");
        assert!(stop.contains("--inbox-wait claude Stop"), "{stop}");
        assert_eq!(stop.matches("--activity-hook claude").count(), 1, "{stop}");
        // Settings that already carry the observer are not given a second one.
        let folded = argv[at + 1].clone();
        let out_again = tempfile::tempdir().unwrap();
        let mut argv = vec![claude.clone(), "--settings".into(), folded.clone()];
        prepare_direct_argv(
            &mut argv,
            AgentCli::Claude,
            "/host/activity-helper",
            Some(out_again.path()),
        );
        assert_ne!(argv[2], folded, "it was prepared again");
        let again = std::fs::read_to_string(&argv[2]).unwrap();
        assert_eq!(again, copy);
        // A settings file that is missing leaves the launch as given.
        let missing = vec![
            claude.clone(),
            "--settings".into(),
            dir.path().join("gone.json").into(),
        ];
        let mut argv = missing.clone();
        prepare_direct_argv(
            &mut argv,
            AgentCli::Claude,
            "/host/activity-helper",
            Some(tempfile::tempdir().unwrap().path()),
        );
        assert_eq!(argv, missing);
    }

    #[test]
    fn home_config_write_failure_refuses_actual_host_spawn_before_any_process_starts() {
        use codesense_pty_core::agent_cli::AgentCli;
        use std::os::unix::fs::PermissionsExt;
        let root = crate::testing::short_root();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let home = root.path().join("managed-home");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("config.yaml"), "hooks: {pre_tool_call: [{matcher: '.*', command: '--hook-block hermes', fail_closed: true}]}\n").unwrap();
        let marker = root.path().join("started");
        let binary = root.path().join("hermes");
        std::fs::write(
            &binary,
            format!("#!/bin/sh\nprintf started > '{}'\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        codesense_pty_core::agent_cli::version_cache::known_release_for_test(
            &binary,
            "Hermes Agent v0.22.5",
        );
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = Host::new(
            "instance".into(),
            ServerIdentity::ephemeral("test"),
            crate::testing::host_environment(),
            runtime.handle().clone(),
        );
        host.start_activity(root.path()).unwrap();
        let mut intent = crate::testing::sh("");
        intent.remote_launch = Some(codesense_host_proto::agent_cli::NativeLaunch::Cli(
            AgentCli::Hermes,
        ));
        intent.argv = Some(vec![binary.to_string_lossy().into_owned()]);
        intent.cwd = root.path().to_string_lossy().into_owned();
        intent
            .env
            .push(("HERMES_HOME".into(), home.to_string_lossy().into_owned()));
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o500)).unwrap();
        let (conn, frames, _peer) = test_conn(128);
        host.connect(&conn);
        host.handle(
            &conn,
            1,
            Request::Spawn {
                intent,
                registration: crate::testing::registration("read-only-managed-home", None, false),
            },
        );
        let result = reply_to(&frames, 1);
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        if result.is_ok() {
            crate::testing::wait_until(|| marker.exists());
        }
        assert!(result
            .unwrap_err()
            .contains("Hermes Agent managed configuration could not be prepared"));
        assert!(
            !marker.exists(),
            "the agent executable must not run after its permission fold fails"
        );
    }

    #[test]
    fn a_saved_agent_hermes_launch_is_marked_release_qualified_on_the_direct_path() {
        use codesense_pty_core::{
            activity::{ActivityOwner, Event, Report, Scope, State, RELEASE_QUALIFIED_ENV},
            agent_cli::AgentCli,
        };
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let binary = home.path().join("hermes");
        std::fs::write(&binary, "#!/bin/sh\n:\n").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        codesense_pty_core::agent_cli::version_cache::known_release_for_test(
            &binary,
            "Hermes Agent v0.22.5",
        );
        std::fs::write(home.path().join("config.yaml"), "model: saved-model\n").unwrap();
        let mut spec = crate::native_launch::tests::shell_spec(home.path());
        spec.command.env("HERMES_HOME", home.path());
        let argv = vec![
            binary.into_os_string(),
            "chat".into(),
            "-q".into(),
            "hi".into(),
        ];
        *spec.command.get_argv_mut() = argv.clone();
        for _ in 0..2 {
            super::prepare_direct_launch(
                &mut spec,
                AgentCli::Hermes,
                "/host/activity-helper",
                None,
            )
            .unwrap();
            assert_eq!(
                spec.command.get_env(RELEASE_QUALIFIED_ENV),
                Some(std::ffi::OsStr::new("1"))
            );
            assert_eq!(spec.command.get_argv(), &argv);
        }
        let yaml = std::fs::read_to_string(home.path().join("config.yaml")).unwrap();
        assert_eq!(yaml.split('\n').filter(|line| *line == "hooks:").count(), 1);
        assert!(yaml.contains("model: saved-model"));
        assert!(yaml.contains("hooks_auto_accept: true"));
        assert!(!yaml.contains("$PPID"));
        // Hermes normalization arrives separately; use the genuine standard report seam.
        let owner = ActivityOwner::new();
        let writer = owner
            .prepare_native("hermes-test", AgentCli::Hermes, "unused")
            .unwrap();
        owner.register("hermes-test", Some(std::process::id()));
        let report = |event, id: &str| {
            owner
                .report_for_test(
                    &writer,
                    std::process::id(),
                    Report {
                        provider: AgentCli::Hermes,
                        event,
                        scope: Scope::Main,
                        report_id: id.into(),
                        provider_session: Some("hermes-native-session".into()),
                        turn: None,
                        tool: None,
                        tool_name: None,
                        reason: None,
                        agent: None,
                        subagents: None,
                    },
                )
                .unwrap()
        };
        report(Event::Started, "hermes-start");
        let snapshot = report(Event::Submitted, "hermes-main-turn");
        assert!(snapshot.observed);
        assert_eq!(snapshot.state, State::Thinking);
    }

    #[test]
    fn a_saved_agent_opencode_launch_is_marked_release_qualified_on_the_direct_path() {
        use codesense_pty_core::{activity::RELEASE_QUALIFIED_ENV, agent_cli::AgentCli};
        use std::ffi::OsString;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fake = |name: &str, version: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\necho '{version}'\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            codesense_pty_core::agent_cli::version_cache::known_release_for_test(&path, version);
            path.into_os_string()
        };
        let current = fake("opencode", "opencode v2.1.4");
        let next_major = fake("next-opencode", "opencode v3.0.1");
        let marked = |program: &OsString, expected: Option<&str>| {
            let argv: Vec<OsString> = [program.to_str().unwrap(), "--prompt", "hi"]
                .map(OsString::from)
                .to_vec();
            let mut spec = crate::native_launch::tests::shell_spec(dir.path());
            *spec.command.get_argv_mut() = argv.clone();
            super::prepare_direct_launch(
                &mut spec,
                AgentCli::OpenCode,
                "/host/activity-helper",
                None,
            )
            .unwrap();
            let mut expected_argv = argv.clone();
            if expected == Some("1") {
                expected_argv.insert(1, "--standalone".into());
            }
            assert_eq!(
                *spec.command.get_argv_mut(),
                expected_argv,
                "qualified launches use a private server"
            );
            spec.command
                .get_env(RELEASE_QUALIFIED_ENV)
                .map(|value| value.to_string_lossy().into_owned())
        };
        assert_eq!(marked(&current, Some("1")).as_deref(), Some("1"));
        assert_eq!(marked(&next_major, Some("0")).as_deref(), Some("0"));
    }

    #[test]
    fn cx3_starting_prompt_requires_a_started_turn_not_terminal_echo() {
        use codesense_pty_core::{activity::State, agent_cli::AgentCli};
        let mut starting = snapshot(AgentCli::Codex, "starting", 0, State::Idle);
        starting.observed = false;
        assert_eq!(
            super::Host::idle_turn(Some(&starting)),
            Some(("starting".into(), 0))
        );
        assert!(!super::turn_started("starting", 0, Some(&starting)));
        assert!(
            !prompt_takes_input(Some(&starting), || Some(true)),
            "a native-capable startup needs observation as well as bracketed paste"
        );
        let thinking = snapshot(AgentCli::Codex, "starting", 1, State::Thinking);
        assert!(super::turn_started("starting", 0, Some(&thinking)));
    }

    #[test]
    fn a_claude_takes_typed_input_only_once_bracketed_paste_is_on() {
        use codesense_pty_core::{activity::State::*, agent_cli::AgentCli::*};
        let claude = snapshot(Claude, "a", 1, Idle);
        assert!(!prompt_takes_input(Some(&claude), || Some(false)));
        assert!(prompt_takes_input(Some(&claude), || Some(true)));
        // A screen that cannot be read, another CLI, or no agent seen: as before.
        assert!(prompt_takes_input(Some(&claude), || None));
        let codex = snapshot(Codex, "a", 1, Idle);
        assert!(!prompt_takes_input(Some(&codex), || Some(false)));
        assert!(prompt_takes_input(Some(&codex), || Some(true)));
        assert!(prompt_takes_input(None, || Some(false)));
        // A Claude that exited leaves a shell, whatever it does with paste.
        let mut exited = snapshot(Claude, "a", 1, Idle);
        exited.exit = Some(codesense_pty_core::activity::Exit {
            code: Some(0),
            signal: None,
            requested: false,
        });
        assert!(prompt_takes_input(Some(&exited), || Some(false)));
    }

    #[test]
    fn a_typed_line_is_submitted_once_its_agent_starts_a_turn() {
        use codesense_pty_core::{activity::State::*, agent_cli::AgentCli::*};
        assert!(!turn_started("a", 3, Some(&snapshot(Claude, "a", 3, Idle))));
        assert!(!turn_started("a", 3, None));
        assert!(turn_started("a", 3, Some(&snapshot(Claude, "a", 4, Idle))));
        assert!(turn_started(
            "a",
            3,
            Some(&snapshot(Claude, "a", 3, Thinking))
        ));
        assert!(turn_started(
            "a",
            3,
            Some(&snapshot(Claude, "a", 3, NeedsPermission))
        ));
        assert!(turn_started("a", 3, Some(&snapshot(Claude, "b", 0, Idle))));
    }

    #[test]
    fn a_cli_command_is_a_command_word_not_a_path() {
        use codesense_pty_core::{activity::State::*, agent_cli::AgentCli::*};
        let claude = Some(snapshot(Claude, "a", 1, Idle));
        for command in ["/model", "  /compact now", "/mcp:tool x", "/release-notes"] {
            assert!(super::cli_command(command, claude.as_ref()), "{command}");
        }
        // With no CLI running there is no command to read it as.
        assert!(!super::cli_command("/model", None));
        let mut exited = snapshot(Claude, "a", 1, Idle);
        exited.exit = Some(codesense_pty_core::activity::Exit {
            code: Some(0),
            signal: None,
            requested: false,
        });
        assert!(!super::cli_command("/model", Some(&exited)));
        for text in [
            "/tmp/out is ready",
            "/ not one",
            "/123",
            "fix /model",
            "",
            "/",
        ] {
            assert!(!super::cli_command(text, claude.as_ref()), "{text}");
        }
    }

    use super::*;
    use crate::testing::WAIT;
    use codesense_pty_core::protocol::{Inbound, Ingress, Push};

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires the built supervisor binary; run in the Linux lifecycle suite"]
    fn remote_stdio_production_broker_registration_transport_retention_and_host_end() {
        use crate::testing::{registration, sh, wait_until};
        use codesense_host_proto::agent_cli::AgentCli;
        use codesense_host_proto::reverse_mcp as wire;
        let binary = std::env::var("CODESENSE_TEST_SUPERVISOR_BINARY").expect("built binary path");
        let (root, spec, _) = crate::native_launch::tests::fixture(
            NativeLaunch::Cli(AgentCli::Claude),
            "--mcp-config --strict-mcp-config",
        );
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = Host::new(
            "stdio-test".into(),
            ServerIdentity::ephemeral("stdio-test"),
            crate::testing::host_environment(),
            runtime.handle().clone(),
        );
        host.terminal.attach_echo_emulator();
        host.start_reverse_mcp(root.path()).unwrap();
        let (conn, frames, _peer) = test_conn(128);
        host.connect(&conn);
        let session = PtySessionId::new("owned-remote-fixture");
        let entry = wire::RemoteStdioEntry {
            id: wire::remote_server_id("fixture").unwrap(),
            logical_name: "fixture".into(),
            command: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "setsid /bin/sh -c 'echo $$ > descendant; sleep 60' & wait".into(),
            ],
            cwd: None,
            env: [("PATH".into(), "/usr/bin:/bin".into())].into(),
            env_refs: Default::default(),
        };
        let mut intent = sh("while [ ! -e start ]; do sleep 0.01; done; \"$SUPERVISOR\" --mcp-stdio-supervisor \"$BROKER\" \"$ENTRY\" & wait");
        intent.cwd = root.path().to_str().unwrap().into();
        intent.env.extend([
            ("SUPERVISOR".into(), binary),
            (
                "BROKER".into(),
                root.path().join("mcp.sock").to_str().unwrap().into(),
            ),
            ("ENTRY".into(), entry.id.clone()),
        ]);
        host.handle(
            &conn,
            1,
            Request::Spawn {
                intent,
                registration: registration(session.as_str(), None, false),
            },
        );
        let Ok(Reply::Spawned { pid: Some(pid) }) = reply_to(&frames, 1) else {
            panic!("native fixture spawn failed")
        };
        // The controlled shell is the native-client surrogate. All registration,
        // authority checks, supervisor ownership, disconnect and End are production paths.
        host.handle(
            &conn,
            2,
            Request::PrepareReverseMcp {
                session: session.clone(),
                binding_nonce: "e".repeat(64),
                server_ids: vec![],
                remote_entries: vec![entry.clone()],
                agent_startup: None,
                launch: NativeLaunch::Cli(AgentCli::Claude),
            },
        );
        assert_eq!(reply_to(&frames, 2), Ok(Reply::Done));
        let reverse = lock(&host.reverse).clone().unwrap();
        reverse
            .prepare(
                conn.id,
                session.as_str(),
                spec,
                Some(NativeLaunch::Cli(AgentCli::Claude)),
            )
            .unwrap();
        reverse.commit(conn.id, session.as_str(), pid).unwrap();
        std::fs::write(root.path().join("start"), b"go").unwrap();
        assert!(wait_until(|| root.path().join("descendant").exists()));
        let descendant: u32 = std::fs::read_to_string(root.path().join("descendant"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let descendant = crate::owned_supervisors::pidfd(descendant).unwrap();
        let config = root
            .path()
            .join(format!("mcp-session-{}.json", "e".repeat(64)));
        host.disconnect(&conn);
        assert!(
            config.exists(),
            "desktop transport loss removed session projection"
        );
        std::thread::sleep(Duration::from_millis(40));
        assert!(
            crate::testing::alive(pid),
            "transport loss killed native client"
        );
        host.connect(&conn);
        host.handle(
            &conn,
            3,
            Request::Kill {
                session: session.clone(),
            },
        );
        assert_eq!(reply_to(&frames, 3), Ok(Reply::Done));
        assert!(
            !config.exists(),
            "End receipt preceded private projection cleanup"
        );
        use std::os::fd::AsRawFd;
        let mut poll = libc::pollfd {
            fd: descendant.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(
            unsafe { libc::poll(&mut poll, 1, 0) },
            1,
            "End receipt preceded descendant cleanup"
        );
        host.handle(
            &conn,
            4,
            Request::PrepareReverseMcp {
                session,
                binding_nonce: "f".repeat(64),
                server_ids: vec![],
                remote_entries: vec![entry],
                agent_startup: None,
                launch: NativeLaunch::Cli(AgentCli::Claude),
            },
        );
        assert_eq!(reply_to(&frames, 4), Ok(Reply::Done));
    }

    #[test]
    fn agent_process_evidence_is_read_on_the_owner_and_includes_native_roots() {
        use crate::testing::{registration, sh, wait_until};
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = Host::new(
            "instance".into(),
            ServerIdentity::ephemeral("test"),
            crate::testing::host_environment(),
            runtime.handle().clone(),
        );
        host.terminal.attach_echo_emulator();
        let hello: Hello = host.greeting().unwrap()[0].decode_json().unwrap();
        assert!(hello.supports(&Capability::AgentProcessV1));
        assert!(hello.supports(&Capability::WorktreeSafetyV1));
        let (conn, frames, _peer) = test_conn(64);
        host.connect(&conn);
        host.handle(
            &conn,
            1,
            Request::Spawn {
                intent: sh("exec sleep 60"),
                registration: registration("native-agent", None, false),
            },
        );
        assert!(matches!(reply_to(&frames, 1), Ok(Reply::Spawned { .. })));
        let queries = vec![
            (PtySessionId::new("native-agent"), "sleep".into()),
            (PtySessionId::new("missing"), "sleep".into()),
        ];
        assert!(wait_until(|| {
            host.handle(
                &conn,
                2,
                Request::AgentsAlive {
                    queries: queries.clone(),
                },
            );
            reply_to(&frames, 2)
                == Ok(Reply::AgentsAlive {
                    alive: vec![true, false],
                })
        }));
        host.handle(
            &conn,
            3,
            Request::AgentsAlive {
                queries: vec![(PtySessionId::new("native-agent"), "a".repeat(257))],
            },
        );
        assert!(reply_to(&frames, 3).is_err());
        host.terminal
            .kill(&PtySessionId::new("native-agent"))
            .unwrap();
    }

    #[test]
    fn the_emulator_worker_is_found_beside_the_binary_with_its_suffix() {
        assert_eq!(
            emulator_worker_beside(Path::new(
                "/App.app/Contents/MacOS/codesense-session-server"
            )),
            Path::new("/App.app/Contents/MacOS/codesense-emulator")
        );
        assert_eq!(
            emulator_worker_beside(Path::new(
                "/repo/src-tauri/binaries/codesense-session-server-aarch64-apple-darwin"
            )),
            Path::new("/repo/src-tauri/binaries/codesense-emulator-aarch64-apple-darwin")
        );
        assert_eq!(
            emulator_worker_beside(Path::new(
                "/home/dev/.codesense/host/v1/codesense-session-server"
            )),
            Path::new("/home/dev/.codesense/host/v1/codesense-emulator")
        );
    }

    #[test]
    fn rm4d_only_replaceable_host_advertises_cooperative_shutdown() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let root = tempfile::tempdir().unwrap();
        let host = Host::new_replaceable(
            "instance".into(),
            ServerIdentity::ephemeral("test"),
            HostEnvironment::of_process(Some(root.path().to_path_buf()), None),
            runtime.handle().clone(),
            root.path().to_path_buf(),
        );
        let hello: Hello = host.greeting().unwrap()[0].decode_json().unwrap();
        assert_eq!(
            hello.supports(&Capability::ShutdownForReplacement),
            cfg!(target_os = "linux")
        );

        let unsupported = Host::new(
            "instance".into(),
            ServerIdentity::ephemeral("test"),
            HostEnvironment::of_process(None, None),
            runtime.handle().clone(),
        );
        let hello: Hello = unsupported.greeting().unwrap()[0].decode_json().unwrap();
        assert!(!hello.supports(&Capability::ShutdownForReplacement));
    }

    /// A host serving private workspaces decodes the zstd batch
    /// codec and says so; one that serves none does not.
    #[test]
    fn xs26_a_private_workspace_host_advertises_the_zstd_batch_codec() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let root = tempfile::tempdir().unwrap();
        let host = Host::new(
            "instance".into(),
            ServerIdentity::ephemeral("test"),
            HostEnvironment::of_process(None, None),
            runtime.handle().clone(),
        );
        let hello: Hello = host.greeting().unwrap()[0].decode_json().unwrap();
        assert!(!hello.supports(&Capability::ContextBatchZstdV1));
        host.enable_private_workspaces_at(root.path().to_path_buf(), 41);
        let hello: Hello = host.greeting().unwrap()[0].decode_json().unwrap();
        assert!(hello.supports(&Capability::ContextBatchZstdV1));
        assert!(hello.supports(&Capability::ContextManifestV1));
    }

    /// `InboxWaitV1`: a host no desktop relays through (this Mac's) hands any client its
    /// sessions' notices; a private machine relays its agents' CodeSense, so one whose
    /// broker did not start hands no one any.
    #[test]
    fn inbox_notices_go_to_this_macs_client_and_no_desktop_of_a_brokerless_machine() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let root = tempfile::tempdir().unwrap();
        let host = Host::new(
            "instance".into(),
            ServerIdentity::ephemeral("test"),
            HostEnvironment::of_process(None, None),
            runtime.handle().clone(),
        );
        assert!(host.inbox_audience("s").admits(None));
        assert!(host.inbox_audience("s").admits(Some("any-mac")));
        host.enable_private_workspaces_at(root.path().to_path_buf(), 41);
        #[cfg(target_os = "linux")]
        for identity in [None, Some("host-1.41")] {
            assert!(!host.inbox_audience("s").admits(identity), "{identity:?}");
        }
    }

    #[test]
    fn rm4d_draining_rejects_control_and_input_requests() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = Host::new(
            "instance".into(),
            ServerIdentity::ephemeral("test"),
            HostEnvironment::of_process(None, None),
            runtime.handle().clone(),
        );
        lock(&host.spawning).draining = true;
        let (conn, frames, _peer) = test_conn(4);
        host.handle(&conn, 7, Request::Listen);
        assert_eq!(
            reply_to(&frames, 7),
            Err("terminal server is draining".into())
        );
        host.input(&conn, &Frame::input(2, 8, b"ignored"));
        assert_eq!(
            reply_to(&frames, 8),
            Err("terminal server is draining".into())
        );
    }

    #[test]
    fn rm4d_drain_error_writes_no_retirement_receipt() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let root = tempfile::tempdir().unwrap();
        let host = Host::new_replaceable(
            "instance".into(),
            ServerIdentity::ephemeral("test"),
            HostEnvironment::of_process(Some(root.path().to_path_buf()), None),
            runtime.handle().clone(),
            root.path().to_path_buf(),
        );
        *lock(&host.replacement_drain_error) = Some("injected process-group signal failure".into());
        let receipt = Reply::Retired {
            nonce: "0123456789abcdef".into(),
            server_id: "server".into(),
            epoch: "epoch".into(),
            process_start: 1,
            sessions: 0,
        };
        assert_eq!(
            host.retire_sessions_for_replacement(receipt, Instant::now() + Duration::from_secs(1)),
            Err("injected process-group signal failure".into())
        );
        assert!(!root
            .path()
            .join("replacement-retirement-0123456789abcdef.json")
            .exists());
    }

    #[test]
    fn cleanup_refuses_cold_and_warm_launches_before_finish_tab() {
        use crate::testing::{registration, sh};
        use codesense_pty_core::domain::{ViewerId, WorkspaceKey};
        use codesense_pty_core::layout::{Placement, WorkspaceDescriptor};
        for warm in [false, true] {
            let fixture = crate::testing::RemoteWorkspaceFixture::new();
            let worktree_path = fixture.worktree.to_string_lossy().into_owned();
            let tip = std::process::Command::new("git")
                .current_dir(&fixture.worktree)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap();
            assert!(tip.status.success());
            let expected_tip = String::from_utf8(tip.stdout).unwrap().trim().to_string();
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let host = Host::new(
                "instance".into(),
                ServerIdentity::ephemeral("test"),
                crate::testing::host_environment(),
                runtime.handle().clone(),
            );
            *lock(&host.private_workspace_root) = Some((fixture.root().to_path_buf(), 41));
            let (conn, frames, _peer) = test_conn(64);
            host.connect(&conn);
            let session = PtySessionId::new("opening");
            host.handle(
                &conn,
                1,
                Request::ReserveTab {
                    request: OpenTab {
                        viewer: ViewerId::new("app"),
                        workspace: WorkspaceKey::new("team|work"),
                        session: session.clone(),
                        descriptor: Some(WorkspaceDescriptor {
                            worktree_path: Some(worktree_path.clone().into()),
                            ..Default::default()
                        }),
                        name: None,
                        role: None,
                        tags: Default::default(),
                        placement: Placement::Tab,
                        index: None,
                        activate: false,
                    },
                },
            );
            assert!(matches!(reply_to(&frames, 1), Ok(Reply::Done)));
            let mut intent = sh("cat");
            if !warm {
                intent.cwd = worktree_path.clone();
            }
            let spawned = if warm { "pool" } else { "opening" };
            host.handle(
                &conn,
                2,
                Request::Spawn {
                    intent,
                    registration: registration(spawned, None, warm),
                },
            );
            assert!(matches!(reply_to(&frames, 2), Ok(Reply::Spawned { .. })));
            if warm {
                host.handle(
                    &conn,
                    3,
                    Request::HandoffWarmMember {
                        member: PtySessionId::new("pool"),
                        session: session.clone(),
                        target_cwd: worktree_path.clone(),
                        cols: 80,
                        rows: 24,
                        meta: codesense_pty_core::SessionMeta::default(),
                    },
                );
                assert!(matches!(reply_to(&frames, 3), Ok(Reply::Done)));
            }
            // Admission has ended; the opening reservation is the remaining protection.
            assert!(host.terminal.tab_entries().unwrap().is_empty());
            host.handle(
                &conn,
                4,
                Request::RemoveWorktree {
                    request: codesense_pty_core::remote_workspace::RemoveWorktree {
                        expected_tip: expected_tip,
                        discard_uncommitted: false,
                        owner_id: 41,
                        repository_key: "github:example/fixture".into(),
                        repo_path: fixture.repo.to_string_lossy().into_owned(),
                        worktree_path: worktree_path.clone(),
                    },
                },
            );
            assert!(reply_to(&frames, 4).unwrap_err().contains("runningAgents"));
            assert!(std::path::Path::new(&worktree_path).exists());
            host.terminal.kill(&session).unwrap();
            host.disconnect(&conn);
        }
    }

    #[test]
    fn uncertain_reserved_tab_commits_tags_when_spawn_completes_later() {
        use crate::testing::{registration, sh};
        use codesense_pty_core::domain::ViewerId;
        use codesense_pty_core::layout::{Placement, WorkspaceDescriptor};
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = Host::new(
            "instance".into(),
            ServerIdentity::ephemeral("test"),
            crate::testing::host_environment(),
            runtime.handle().clone(),
        );
        let (conn, frames, _peer) = test_conn(64);
        host.connect(&conn);
        let session = PtySessionId::new("late-uncertain");
        let workspace = codesense_pty_core::domain::WorkspaceKey::new("team|work");
        host.reserve_tab(
            conn.id,
            OpenTab {
                viewer: ViewerId::new("app"),
                workspace: workspace.clone(),
                session: session.clone(),
                descriptor: Some(WorkspaceDescriptor {
                    display_name: "work".into(),
                    kind: "root".into(),
                    ..Default::default()
                }),
                name: None,
                role: None,
                tags: [("remoteRequestId".into(), "retained".into())].into(),
                placement: Placement::Tab,
                index: None,
                activate: false,
            },
        )
        .unwrap();
        assert!(matches!(
            host.reconcile_tab(conn.id, session.clone()),
            Err(LayoutError::SpawnUnknown(_))
        ));
        assert!(matches!(
            host.reconcile_tab(conn.id, PtySessionId::new("unrelated")),
            Err(LayoutError::UnknownSession(_))
        ));
        host.handle(
            &conn,
            1,
            Request::Spawn {
                intent: sh("cat"),
                registration: registration(session.as_str(), None, false),
            },
        );
        assert!(matches!(reply_to(&frames, 1), Ok(Reply::Spawned { .. })));
        let snapshot = host
            .terminal
            .layout_snapshot(
                &ViewerId::new("app"),
                &codesense_pty_core::domain::ClientBootId::new("reader"),
            )
            .unwrap();
        assert_eq!(
            snapshot.workspaces[&workspace].tabs[0]
                .tags
                .get("remoteRequestId")
                .map(String::as_str),
            Some("retained")
        );
        host.disconnect(&conn);
        assert_eq!(
            host.terminal.has_session(&session),
            Ok(true),
            "completed uncertain process survives client disconnect"
        );
        host.terminal.kill(&session).unwrap();
    }

    #[test]
    fn writable_claim_is_idempotent_and_inflight_writes_prevent_transfer() {
        let claims = Arc::new(Mutex::new(WritableClaims::default()));
        let session = PtySessionId::new("owned");
        lock(&claims).claim(&session, 1).unwrap();
        lock(&claims).claim(&session, 1).unwrap();
        assert_eq!(
            lock(&claims).claim(&session, 2),
            Err(ATTACHED_ELSEWHERE.into())
        );
        let permit = WritePermit::acquire(&claims, &session, 1).unwrap();
        assert!(WritePermit::acquire(&claims, &session, 2).is_err());
        lock(&claims).release(&session, 1).unwrap();
        assert!(lock(&claims).claim(&session, 2).is_err());
        drop(permit);
        // A released claim remains protected; omitting a claim cannot write.
        assert!(WritePermit::acquire(&claims, &session, 2).is_err());
        lock(&claims).claim(&session, 2).unwrap();
        assert!(lock(&claims).release(&session, 1).is_err());
        assert!(WritePermit::acquire(&claims, &session, 2).is_ok());
    }

    #[test]
    fn writable_claim_preserves_reads_and_disconnect_preserves_the_process() {
        use crate::testing::{registration, sh};
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = Host::new(
            "instance".into(),
            ServerIdentity::ephemeral("test"),
            crate::testing::host_environment(),
            runtime.handle().clone(),
        );
        host.terminal.attach_echo_emulator();
        let (first, first_frames, _first_peer) = test_conn(64);
        let (socket, _second_peer) = UnixStream::pair().unwrap();
        let (out, second_frames) = mpsc::sync_channel(64);
        let second = Conn::new(2, out, socket);
        host.connect(&first);
        host.connect(&second);
        let session = PtySessionId::new("claimed");
        host.handle(
            &first,
            1,
            Request::Spawn {
                intent: sh("cat"),
                registration: registration("claimed", None, true),
            },
        );
        assert!(matches!(
            reply_to(&first_frames, 1),
            Ok(Reply::Spawned { .. })
        ));
        host.handle(
            &first,
            2,
            Request::ClaimWritable {
                session: session.clone(),
            },
        );
        assert_eq!(reply_to(&first_frames, 2), Ok(Reply::Done));
        host.handle(
            &first,
            3,
            Request::ClaimWritable {
                session: session.clone(),
            },
        );
        assert_eq!(reply_to(&first_frames, 3), Ok(Reply::Done));
        host.handle(
            &second,
            4,
            Request::ClaimWritable {
                session: session.clone(),
            },
        );
        assert_eq!(reply_to(&second_frames, 4), Ok(Reply::WritableBusy));
        host.handle(
            &second,
            5,
            Request::Subscribe {
                session: session.clone(),
                viewport: ViewportId::new("reader"),
            },
        );
        let stream = match reply_to(&second_frames, 5).unwrap() {
            Reply::Subscribed { result } => result.stream_id,
            other => panic!("expected subscription, got {other:?}"),
        };
        host.handle(&second, 6, Request::Unsubscribe { stream });
        assert_eq!(reply_to(&second_frames, 6), Ok(Reply::Flag { value: true }));
        // Unsubscribe is not a writable release, nor can raw input bypass it.
        write(&host, &second, 7, "claimed", b"forbidden");
        assert_eq!(reply_to(&second_frames, 7), Err(ATTACHED_ELSEWHERE.into()));
        host.handle(
            &second,
            8,
            Request::SubmitLine {
                session: session.clone(),
                line: "forbidden".into(),
                quiet_ms: 0,
                confirm_ms: 0,
                timeout_ms: 1,
                max_nudges: 0,
            },
        );
        assert_eq!(reply_to(&second_frames, 8), Err(ATTACHED_ELSEWHERE.into()));
        host.handle(
            &second,
            9,
            Request::SubmitShellLine {
                session: session.clone(),
                line: "forbidden".into(),
                quiet_ms: 0,
                timeout_ms: 1,
            },
        );
        assert_eq!(reply_to(&second_frames, 9), Err(ATTACHED_ELSEWHERE.into()));
        write(&host, &first, 10, "claimed", b"allowed\n");
        assert_eq!(reply_to(&first_frames, 10), Ok(Reply::Done));
        host.disconnect(&first);
        assert_eq!(host.terminal.has_session(&session), Ok(true));
        host.handle(
            &first,
            11,
            Request::ClaimWritable {
                session: session.clone(),
            },
        );
        assert!(reply_to(&first_frames, 11).is_err());
        host.handle(
            &second,
            12,
            Request::ClaimWritable {
                session: session.clone(),
            },
        );
        assert_eq!(reply_to(&second_frames, 12), Ok(Reply::Done));
        host.handle(
            &second,
            13,
            Request::ReleaseWritable {
                session: session.clone(),
            },
        );
        assert_eq!(reply_to(&second_frames, 13), Ok(Reply::Done));
        host.handle(
            &second,
            14,
            Request::ClaimWritable {
                session: session.clone(),
            },
        );
        assert_eq!(reply_to(&second_frames, 14), Ok(Reply::Done));
        host.terminal.kill(&session).unwrap();
        host.disconnect(&second);
    }

    #[test]
    fn reverse_mcp_egress_charge_released_when_receiver_drops_queued_frames() {
        use std::sync::atomic::Ordering;
        let before = REVERSE_EGRESS_BYTES.load(Ordering::Acquire);
        let (conn, frames, _peer) = test_conn(2);
        assert!(conn.send(Frame {
            kind: codesense_host_proto::Kind::MCP_DATA,
            channel: 1,
            payload: vec![0; 32]
        }));
        assert_eq!(REVERSE_EGRESS_BYTES.load(Ordering::Acquire), before + 41);
        drop(frames);
        assert_eq!(REVERSE_EGRESS_BYTES.load(Ordering::Acquire), before);
        assert!(!conn.send(Frame {
            kind: codesense_host_proto::Kind::MCP_DATA,
            channel: 1,
            payload: vec![0; 32]
        }));
        assert_eq!(REVERSE_EGRESS_BYTES.load(Ordering::Acquire), before);
    }

    /// The reply to `rid` among the frames queued for a connection.
    fn reply_to(frames: &mpsc::Receiver<QueuedFrame>, rid: u64) -> Result<Reply, String> {
        let mut ingress = Ingress::default();
        loop {
            let frame = frames.recv_timeout(WAIT).expect("reply");
            if let Some(Inbound::Reply { rid: got, result }) = ingress.accept(&frame).unwrap() {
                if got == rid {
                    return result;
                }
            }
        }
    }

    // A timer a routine set before its desktop named
    // itself was stamped with the plain viewer; once the desktop names
    // itself the timer is its, so its fires still claim the session that
    // desktop holds.
    #[test]
    fn a_timer_set_before_its_desktop_named_itself_still_claims_the_session() {
        use codesense_pty_core::session_wake::{Originator, TimerSchedule, TimerSpec};
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = Host::new(
            "agr11".into(),
            ServerIdentity::ephemeral("agr11"),
            crate::testing::host_environment(),
            runtime.handle().clone(),
        );
        host.terminal.attach_echo_emulator();
        let root = tempfile::tempdir().unwrap();
        host.start_wake(root.path()).unwrap();
        let (conn, frames, _peer) = test_conn(64);
        host.connect_as(&conn, Some("app".into()));
        let session = PtySessionId::new("timed-r3");
        host.handle(
            &conn,
            1,
            Request::Spawn {
                intent: crate::testing::sh("cat"),
                registration: crate::testing::registration("timed-r3", None, false),
            },
        );
        assert!(matches!(reply_to(&frames, 1), Ok(Reply::Spawned { .. })));
        host.handle(
            &conn,
            2,
            Request::ClaimWritable {
                session: session.clone(),
            },
        );
        assert_eq!(reply_to(&frames, 2), Ok(Reply::Done));
        let wake = host.wake().unwrap();
        wake.set_timer(TimerSpec {
            session: session.clone(),
            schedule: TimerSchedule::After {
                after_ms: 3_600_000,
                every_ms: None,
            },
            until_ms: None,
            count: None,
            text: "check".into(),
            name: "r3".into(),
            created_by: "routine".into(),
            max_wait_ms: 60_000,
            originator: Originator {
                desktop: "app".into(),
                routine_host: Some("host-1".into()),
            },
            require_observed: false,
            min_gap_ms: None,
        })
        .unwrap();
        host.handle(
            &conn,
            3,
            Request::RoutineArmBindSecret {
                secret: "a".repeat(64),
                desktop: Some("host-1.41".into()),
            },
        );
        assert_eq!(reply_to(&frames, 3), Ok(Reply::Done));
        let timer = wake.timers(Some(&session)).pop().unwrap();
        assert_eq!(timer.originator.desktop, "host-1.41");
        assert!(host
            .wake_claim(&session, &timer.originator.desktop)
            .is_some());
        let _ = host.terminal.kill(&session);
    }

    #[test]
    fn machine_stats_watch_owns_the_sampler_and_disconnect_releases_it() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = Host::new(
            "instance".into(),
            ServerIdentity::ephemeral("test"),
            crate::testing::host_environment(),
            runtime.handle().clone(),
        );
        let workspace = tempfile::tempdir().unwrap();
        let worktree = workspace.path().join("worktree");
        std::fs::create_dir(&worktree).unwrap();
        *lock(&host.machine_stats_directory) = Some(worktree.clone());
        assert_eq!(
            host.machine_stats_sample().unwrap().disk_path,
            worktree.canonicalize().unwrap().display().to_string()
        );
        std::fs::remove_dir(&worktree).unwrap();
        assert_eq!(
            host.machine_stats_sample().unwrap().disk_path,
            workspace
                .path()
                .canonicalize()
                .unwrap()
                .display()
                .to_string()
        );
        let (conn, frames, _peer) = test_conn(32);
        host.connect(&conn);
        host.relay.listen(&conn);
        let (mut other, other_frames, _other_peer) = test_conn(32);
        other.id = 2;
        host.connect(&other);
        host.relay.listen(&other);
        assert!(lock(&host.machine_stats_schedule).deadline().is_none());
        host.handle(&conn, 1, Request::MachineStats);
        assert!(matches!(
            reply_to(&frames, 1),
            Ok(Reply::MachineStats { sample: Some(_) })
        ));
        assert!(lock(&host.machine_stats_schedule).deadline().is_none());
        host.handle(&conn, 2, Request::MachineStatsWatch { watch: true });
        assert!(matches!(
            reply_to(&frames, 2),
            Ok(Reply::MachineStats { sample: Some(_) })
        ));
        assert!(lock(&host.machine_stats_schedule).deadline().is_some());
        lock(&host.machine_stats_schedule).make_due();
        host.machine_stats_changed.notify_one();
        let mut ingress = Ingress::default();
        let pushed = frames.recv_timeout(WAIT).expect("watched resource push");
        assert!(matches!(
            ingress.accept(&pushed).unwrap(),
            Some(Inbound::Event(TerminalDomainEvent::MachineStats { .. }))
        ));
        assert!(
            other_frames.try_recv().is_err(),
            "unwatched connection receives no sample"
        );
        host.handle(&conn, 3, Request::MachineStatsWatch { watch: false });
        assert_eq!(
            reply_to(&frames, 3),
            Ok(Reply::MachineStats { sample: None })
        );
        assert!(lock(&host.machine_stats_schedule).deadline().is_none());
        host.handle(&conn, 4, Request::MachineStatsWatch { watch: true });
        assert!(reply_to(&frames, 4).is_ok());
        host.disconnect(&conn);
        assert!(lock(&host.machine_stats_schedule).deadline().is_none());
    }

    fn test_conn(capacity: usize) -> (Conn, mpsc::Receiver<QueuedFrame>, UnixStream) {
        let (socket, peer) = UnixStream::pair().unwrap();
        let (out, frames) = mpsc::sync_channel(capacity);
        (Conn::new(1, out, socket), frames, peer)
    }

    #[test]
    fn local_inbox_recovery_tracks_app_disconnect_and_rechecks_after_claim() {
        use codesense_pty_core::agent_cli::{Notice, SubmitOutcome};
        use codesense_pty_core::inbox_host::InboxOutlet;
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = Host::new(
            "instance".into(),
            ServerIdentity::ephemeral("test"),
            crate::testing::host_environment(),
            runtime.handle().clone(),
        );
        let rig = crate::session_wake::tests::rig();
        rig.idle("s");
        *lock(&host.wake) = Some(rig.wake.clone());
        let route = InboxRoute(Arc::downgrade(&host));
        let (conn, _frames, _peer) = test_conn(32);
        host.connect(&conn);
        assert_eq!(route.has_tools("s"), Some(false), "a probe is not an app");
        host.identify_client(conn.id, Some("app-process".into()));
        assert_eq!(
            route.has_tools("s"),
            Some(false),
            "terminal admission is not MCP readiness"
        );
        host.handle(
            &conn,
            100,
            Request::LocalMcpSessions {
                sessions: vec![PtySessionId::new("s")],
            },
        );
        assert_eq!(route.has_tools("s"), Some(true));
        let notice = Notice {
            text: "You have mail.",
            token: "held",
        };
        host.disconnect(&conn);
        assert_eq!(route.has_tools("s"), Some(false));
        assert_eq!(route.recover("s", &notice), SubmitOutcome::RefusedNoEffect);
        assert!(rig.writes().is_empty());

        host.connect(&conn);
        host.identify_client(conn.id, Some("restarted-app".into()));
        host.handle(
            &conn,
            101,
            Request::LocalMcpSessions {
                sessions: vec![PtySessionId::new("s")],
            },
        );
        assert_eq!(route.has_tools("s"), Some(true));
        // Disconnect after the route's initial readiness read, at the guarded
        // write's attachment claim: it must recheck before pasting anything.
        let gone = host.clone();
        let connection = conn.clone();
        rig.on_claim(Box::new(move || gone.disconnect(&connection)));
        assert_eq!(route.recover("s", &notice), SubmitOutcome::RefusedNoEffect);
        assert!(rig.writes().is_empty());

        host.connect(&conn);
        host.identify_client(conn.id, Some("restarted-app".into()));
        host.handle(
            &conn,
            101,
            Request::LocalMcpSessions {
                sessions: vec![PtySessionId::new("s")],
            },
        );
        assert_eq!(route.recover("s", &notice), SubmitOutcome::Uncertain);
        assert_eq!(
            rig.writes().len(),
            2,
            "one paste and Enter once tools return"
        );
    }

    #[test]
    fn local_inbox_waits_for_its_mcp_link_then_tells_held_mail_once() {
        use codesense_pty_core::inbox_host::InboxOutlet;
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = Host::new(
            "instance".into(),
            ServerIdentity::ephemeral("test"),
            crate::testing::host_environment(),
            runtime.handle().clone(),
        );
        let rig = crate::session_wake::tests::rig();
        rig.idle("s");
        *lock(&host.wake) = Some(rig.wake.clone());
        let route = Arc::new(InboxRoute(Arc::downgrade(&host)));
        rig.owner().set_inbox_outlet(route.clone());
        let (conn, frames, _peer) = test_conn(32);
        host.connect(&conn);
        host.identify_client(conn.id, Some("app".into()));
        host.relay.listen(&conn);
        rig.owner()
            .inbox_mail("s", "You have mail.".into(), "held".into())
            .unwrap();
        let inbox = rig.owner().inbox();
        // The terminal transport is connected while the bridge is still in backoff.
        assert_eq!(route.has_tools("s"), Some(false));
        inbox.recover(&*route, "s");
        assert!(inbox.holds("s"));
        assert!(rig.writes().is_empty());
        // Another session's initialized MCP connection cannot enable this one's mail.
        host.handle(
            &conn,
            1,
            Request::LocalMcpSessions {
                sessions: vec![PtySessionId::new("other")],
            },
        );
        inbox.recover(&*route, "s");
        assert!(rig.writes().is_empty());
        // The MCP initialized notification publishes the session snapshot on this link.
        host.handle(
            &conn,
            2,
            Request::LocalMcpSessions {
                sessions: vec![PtySessionId::new("s")],
            },
        );
        inbox.recover(&*route, "s");
        assert_eq!(rig.writes().len(), 2);
        let (socket, _peer) = UnixStream::pair().unwrap();
        inbox.serve(
            &*route,
            "s",
            codesense_pty_core::inbox_waiters::WaitArm::IfAbsent,
            codesense_pty_core::activity::peer::inspect(std::process::id()).unwrap(),
            true,
            Some("[inbox held]\nYou have mail."),
            socket,
        );
        for _ in 0..3 {
            inbox.recover(&*route, "s");
        }
        assert_eq!(rig.writes().len(), 2);
        let told = frames.try_iter().filter(|frame| matches!(frame.decode_json::<Message>(),
            Ok(Message::Push { push: Push::Event { event: TerminalDomainEvent::InboxAck { token, .. } } }) if token == "held")).count();
        assert_eq!(
            told, 1,
            "one durable told acknowledgement after prompt confirmation"
        );
        host.handle(
            &conn,
            3,
            Request::LocalMcpSessions {
                sessions: Vec::new(),
            },
        );
        assert_eq!(
            route.has_tools("s"),
            Some(false),
            "MCP loss clears readiness while terminal stays admitted"
        );
    }

    /// Write `data` to `session` as call `rid`, over an input channel of its
    /// own, as a client does.
    fn write(host: &Arc<Host>, conn: &Conn, rid: u64, session: &str, data: &[u8]) {
        host.handle(
            conn,
            rid + 1000,
            Request::OpenInput {
                session: PtySessionId::new(session),
                channel: 2,
            },
        );
        host.input(conn, &Frame::input(2, rid, data));
    }

    /// A write to an unknown session is answered with the runtime's own error
    /// and leaves no input thread behind.
    #[test]
    fn a_write_to_an_unknown_session_starts_no_input_thread() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = Host::new(
            "instance".into(),
            ServerIdentity::ephemeral("test"),
            crate::testing::host_environment(),
            runtime.handle().clone(),
        );
        let (conn, frames, _peer) = test_conn(8);
        write(&host, &conn, 7, "ghost", b"x");
        assert_eq!(
            reply_to(&frames, 7),
            Err("write_pty: session not found: ghost".to_string())
        );
        assert!(lock(&host.relay.inputs).is_empty());
    }

    /// A session's input thread is retired when the session leaves the
    /// registry, so a long-lived sidecar keeps none for closed sessions.
    #[test]
    fn a_spend_hold_stops_automatic_input_but_a_person_can_still_type() {
        use crate::testing::{registration, sh};
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = Host::new(
            "held".into(),
            ServerIdentity::ephemeral("test"),
            crate::testing::host_environment(),
            runtime.handle().clone(),
        );
        host.terminal.attach_echo_emulator();
        let (conn, frames, _peer) = test_conn(64);
        host.handle(
            &conn,
            1,
            Request::Spawn {
                intent: sh("sleep 60"),
                registration: registration("held-session", None, false),
            },
        );
        assert!(matches!(reply_to(&frames, 1), Ok(Reply::Spawned { .. })));
        let session = PtySessionId::new("held-session");
        host.terminal.activity().inbox().set_wake_holds(
            1,
            vec![codesense_pty_core::inbox_host::WakeHold {
                session: session.as_str().into(),
                reason: Some("day cap reached".into()),
            }],
        );
        assert!(host.wake_write(&session, b"automatic\r").is_err());
        write(&host, &conn, 2, session.as_str(), b"person typing");
        assert_eq!(reply_to(&frames, 2), Ok(Reply::Done));
        host.terminal.kill(&session).unwrap();
    }

    #[test]
    fn a_removed_sessions_input_thread_is_retired() {
        use crate::testing::{registration, sh, wait_until};
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = Host::new(
            "instance".into(),
            ServerIdentity::ephemeral("test"),
            crate::testing::host_environment(),
            runtime.handle().clone(),
        );
        let (conn, frames, _peer) = test_conn(64);
        let reply = |rid: u64| reply_to(&frames, rid);
        host.handle(
            &conn,
            1,
            Request::Spawn {
                intent: sh("sleep 60"),
                registration: registration("typed", None, false),
            },
        );
        assert!(matches!(reply(1), Ok(Reply::Spawned { .. })));
        write(&host, &conn, 2, "typed", b"x");
        assert_eq!(reply(2), Ok(Reply::Done));
        assert!(lock(&host.relay.inputs).contains_key("typed"));
        host.handle(
            &conn,
            3,
            Request::Kill {
                session: PtySessionId::new("typed"),
            },
        );
        assert_eq!(reply(3), Ok(Reply::Done));
        assert!(wait_until(|| lock(&host.relay.inputs).is_empty()));
    }

    /// Input on a channel the connection never opened is answered with an
    /// error on its own call id; a frame too short for its call id is refused.
    #[test]
    fn input_on_an_unopened_channel_is_refused() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = Host::new(
            "instance".into(),
            ServerIdentity::ephemeral("test"),
            crate::testing::host_environment(),
            runtime.handle().clone(),
        );
        let (conn, frames, _peer) = test_conn(8);
        host.input(&conn, &Frame::input(9, 4, b"x"));
        assert_eq!(
            reply_to(&frames, 4),
            Err("write_pty: input channel 9 is not open".to_string())
        );
        host.input(
            &conn,
            &Frame {
                kind: codesense_host_proto::Kind::INPUT,
                channel: 9,
                payload: vec![1],
            },
        );
        assert!(reply_to(&frames, crate::server::UNPARSEABLE_RID).is_err());
    }

    /// An input channel is opened only in the byte-stream range and only once.
    #[test]
    fn open_input_refuses_a_reserved_or_taken_channel() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = Host::new(
            "instance".into(),
            ServerIdentity::ephemeral("test"),
            crate::testing::host_environment(),
            runtime.handle().clone(),
        );
        let (conn, frames, _peer) = test_conn(8);
        let open = |rid, channel| {
            host.handle(
                &conn,
                rid,
                Request::OpenInput {
                    session: PtySessionId::new("s"),
                    channel,
                },
            )
        };
        open(1, 2);
        assert_eq!(reply_to(&frames, 1), Ok(Reply::Done));
        open(2, 2);
        assert!(reply_to(&frames, 2).is_err(), "a taken channel");
        open(3, 1);
        assert!(reply_to(&frames, 3).is_err(), "the reserved layout channel");
    }

    /// A connection that starts listening after a viewport attached still
    /// closes that stream's channel with the session's exit.
    #[test]
    fn a_later_listener_closes_an_earlier_viewports_channel() {
        let relay = Relay::default();
        let session = PtySessionId::new("s");
        relay.bind_stream(StreamId(9), &session);
        let (conn, frames, _peer) = test_conn(16);
        relay.listen(&conn);
        relay
            .emit(TerminalDomainEvent::StreamOutput {
                stream: StreamId(9),
                data: "x".into(),
                seq: 1,
            })
            .unwrap();
        relay
            .emit(TerminalDomainEvent::Exit {
                session,
                status: None,
                pool_member: false,
                requested: false,
                incarnation: 0,
            })
            .unwrap();
        let pushes: Vec<Push> = frames
            .try_iter()
            .filter_map(|frame| match frame.decode_json::<Message>() {
                Ok(Message::Push { push }) => Some(push),
                _ => None,
            })
            .collect();
        assert!(
            pushes.contains(&Push::OutputClosed { channel: 2 }),
            "{pushes:?}"
        );
        assert!(lock(&relay.streams).is_empty());
    }

    /// A viewport attached after its session's output ended — an exit that
    /// raced the attach — has its channel closed at once, never left open
    /// until the viewport detaches or its connection closes.
    #[test]
    fn a_viewport_attached_after_the_exit_is_closed_at_once() {
        use crate::testing::{registration, sh, wait_until};
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = Host::new(
            "instance".into(),
            ServerIdentity::ephemeral("test"),
            crate::testing::host_environment(),
            runtime.handle().clone(),
        );
        host.terminal.attach_echo_emulator();
        let (conn, frames, _peer) = test_conn(64);
        host.connect(&conn);
        host.handle(
            &conn,
            1,
            Request::Spawn {
                intent: sh("printf done"),
                registration: registration("ended", None, false),
            },
        );
        assert!(matches!(reply_to(&frames, 1), Ok(Reply::Spawned { .. })));
        let ended = PtySessionId::new("ended");
        assert!(wait_until(|| host.terminal.output_ended(&ended)));
        host.handle(
            &conn,
            2,
            Request::Subscribe {
                session: ended.clone(),
                viewport: ViewportId::new("late"),
            },
        );
        assert!(matches!(reply_to(&frames, 2), Ok(Reply::Subscribed { .. })));
        assert!(
            lock(&host.relay.streams).is_empty(),
            "no binding is left for an ended session's viewport"
        );
        host.terminal.kill(&ended).unwrap();
    }

    /// Only the connection that attached a viewport can detach it.
    #[test]
    fn an_unowned_stream_is_not_detached() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = Host::new(
            "instance".into(),
            ServerIdentity::ephemeral("test"),
            crate::testing::host_environment(),
            runtime.handle().clone(),
        );
        let (conn, frames, _peer) = test_conn(8);
        host.connect(&conn);
        host.handle(
            &conn,
            1,
            Request::Unsubscribe {
                stream: StreamId(4),
            },
        );
        assert_eq!(reply_to(&frames, 1), Ok(Reply::Flag { value: false }));
    }

    /// A connection that closes mid-open abandons it: the session it spawned
    /// for the tab is killed, never left running unplaced, and the id can be
    /// opened again.
    #[test]
    fn a_closed_connection_abandons_the_tabs_it_was_opening() {
        use crate::testing::{registration, sh, wait_until};
        use codesense_pty_core::domain::{ViewerId, WorkspaceKey};
        use codesense_pty_core::layout::{OpenTab, Placement, WorkspaceDescriptor};
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = Host::new(
            "instance".into(),
            ServerIdentity::ephemeral("test"),
            crate::testing::host_environment(),
            runtime.handle().clone(),
        );
        let (conn, frames, _peer) = test_conn(64);
        host.connect(&conn);
        let request = OpenTab {
            viewer: ViewerId::new("app"),
            workspace: WorkspaceKey::new("ws"),
            session: PtySessionId::new("opening"),
            descriptor: Some(WorkspaceDescriptor::default()),
            name: None,
            role: None,
            tags: Default::default(),
            placement: Placement::Tab,
            index: None,
            activate: true,
        };
        host.handle(
            &conn,
            1,
            Request::ReserveTab {
                request: request.clone(),
            },
        );
        assert_eq!(reply_to(&frames, 1), Ok(Reply::Done));
        host.handle(
            &conn,
            2,
            Request::Spawn {
                intent: sh("sleep 60"),
                registration: registration("opening", None, false),
            },
        );
        assert!(matches!(reply_to(&frames, 2), Ok(Reply::Spawned { .. })));

        // Another tab's spawn is still in flight as the connection closes.
        let racing = PtySessionId::new("racing");
        host.handle(
            &conn,
            3,
            Request::ReserveTab {
                request: OpenTab {
                    session: racing.clone(),
                    ..request.clone()
                },
            },
        );
        assert_eq!(reply_to(&frames, 3), Ok(Reply::Done));
        lock(&host.spawning)
            .in_flight
            .entry(racing.as_str().to_string())
            .or_default()
            .spawns += 1;

        host.disconnect(&conn);
        assert!(wait_until(
            || host.terminal.has_session(&request.session) == Ok(false)
        ));
        assert!(
            host.terminal.reserve_tab(&request).is_ok(),
            "the reservation was dropped"
        );
        assert!(
            lock(&host.spawning).in_flight[racing.as_str()].killed,
            "the spawn in flight is torn down as it lands"
        );
    }

    /// A respawn whose finish is lost (its connection closed after the spawn)
    /// releases its hold, and the next respawn of the tab finds the new
    /// process running and revives the entry instead of refusing it; a second
    /// begin while a respawn is held is refused as already in progress.
    #[test]
    fn a_lost_respawn_finish_is_revived_and_a_second_begin_is_refused() {
        use crate::testing::{registration, sh, wait_until};
        use codesense_pty_core::domain::{ClientBootId, ViewerId, WorkspaceKey};
        use codesense_pty_core::layout::{
            EntryState, LayoutError, OpenTab, Placement, WorkspaceDescriptor,
        };
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = Host::new(
            "instance".into(),
            ServerIdentity::ephemeral("test"),
            crate::testing::host_environment(),
            runtime.handle().clone(),
        );
        let viewer = ViewerId::new("app");
        let boot = ClientBootId::new("b");
        // A client attaches under its boot before opening any terminals.
        host.terminal
            .layout_snapshot(&viewer, &boot)
            .expect("the viewer attaches before the terminal opens");
        let session = PtySessionId::new("lost-finish");
        let request = OpenTab {
            viewer: viewer.clone(),
            workspace: WorkspaceKey::new("ws"),
            session: session.clone(),
            descriptor: Some(WorkspaceDescriptor::default()),
            name: None,
            role: None,
            tags: Default::default(),
            placement: Placement::Tab,
            index: None,
            activate: true,
        };
        let (opener, frames, _peer) = test_conn(64);
        host.connect(&opener);
        host.handle(&opener, 1, Request::ReserveTab { request });
        assert_eq!(reply_to(&frames, 1), Ok(Reply::Done));
        host.handle(
            &opener,
            2,
            Request::Spawn {
                intent: sh("exit 3"),
                registration: registration("lost-finish", None, false),
            },
        );
        assert!(matches!(reply_to(&frames, 2), Ok(Reply::Spawned { .. })));
        host.handle(
            &opener,
            3,
            Request::FinishTab {
                session: session.clone(),
                spawned: Ok(()),
            },
        );
        assert!(matches!(reply_to(&frames, 3), Ok(Reply::Tab { .. })));
        // Observe the real exit without attaching a viewer or pruning entries.
        assert!(
            wait_until(|| host
                .terminal
                .tab_entries()
                .map(|entries| entries
                    .iter()
                    .any(|(_, entry)| entry.session == session && entry.state.is_exited()))
                .unwrap_or(false)),
            "expected {session:?} to exit before the snapshot; entries: {:?}",
            host.terminal.tab_entries()
        );
        let snapshot = host
            .terminal
            .layout_snapshot(&viewer, &boot)
            .expect("the viewer snapshot succeeds");
        let entry = snapshot
            .workspaces
            .values()
            .flat_map(|workspace| &workspace.tabs)
            .find(|entry| entry.session == session);
        assert_eq!(
            entry.map(|entry| &entry.state),
            Some(&EntryState::Exited {
                exit_code: Some(3),
                signal: None,
            }),
            "the snapshot must retain {session:?} with its real exit status; snapshot: {snapshot:?}"
        );

        let (conn, frames, _peer) = test_conn(64);
        host.connect(&conn);
        host.handle(
            &conn,
            1,
            Request::BeginRespawn {
                session: session.clone(),
            },
        );
        assert_eq!(reply_to(&frames, 1), Ok(Reply::Done));
        host.handle(
            &conn,
            2,
            Request::BeginRespawn {
                session: session.clone(),
            },
        );
        assert_eq!(
            reply_to(&frames, 2),
            Ok(Reply::LayoutRefused {
                error: LayoutError::Restarting(session.clone())
            })
        );
        host.handle(
            &conn,
            3,
            Request::Spawn {
                intent: sh("sleep 60"),
                registration: registration("lost-finish", None, false),
            },
        );
        assert!(matches!(reply_to(&frames, 3), Ok(Reply::Spawned { .. })));
        // The FinishRespawn never arrives.
        host.disconnect(&conn);

        let (again, frames, _peer) = test_conn(64);
        host.connect(&again);
        host.handle(
            &again,
            1,
            Request::BeginRespawn {
                session: session.clone(),
            },
        );
        match reply_to(&frames, 1) {
            Ok(Reply::Tab { entry }) => {
                assert_eq!(entry.session, session);
                assert_eq!(entry.state, EntryState::Live);
            }
            other => panic!("expected the running process revived: {other:?}"),
        }
        let _ = host.terminal.kill(&session);
    }

    /// A subscribed connection that stops reading is closed and dropped rather
    /// than buffered for, so its client sees the connection end and reconnects.
    #[test]
    fn a_connection_that_stops_reading_is_closed() {
        let relay = Relay::default();
        let (conn, _undrained, peer) = test_conn(1);
        lock(&relay.subscribers).insert(conn.id, conn);
        let event = || TerminalDomainEvent::WorktreeLive {
            cwd: std::path::PathBuf::from("/tmp"),
        };
        assert!(relay.emit(event()).is_ok());
        assert!(
            relay.emit(event()).is_err(),
            "the full connection is dropped"
        );
        assert!(lock(&relay.subscribers).is_empty());
        let mut rest = Vec::new();
        use std::io::Read;
        (&peer).read_to_end(&mut rest).unwrap();
        assert!(rest.is_empty(), "the socket was closed");
    }

    // One flight per agent and workspace, shared by a desktop's
    // launch-if-absent (ReserveTab tagged `remoteLaunchIfAbsent`) and this
    // host's own routine launches; a launch that times out is `unknown`,
    // holds the flight until its spawn ends, and a late spawn is stopped.
    #[cfg(unix)]
    mod arm_flight {
        use super::*;
        use crate::routine_arm::LaunchFailure;
        use crate::testing::{registration, sh, wait_until};
        use codesense_pty_core::agent_cli::AgentCli;
        use codesense_pty_core::domain::{ViewerId, WorkspaceKey};
        use codesense_pty_core::layout::{OpenTab, Placement, Tags, WorkspaceDescriptor};
        use codesense_pty_core::routine_arm::{
            ArmLaunch, ArmTarget, AGENT_RUNNING, TAG_LAUNCH_IF_ABSENT, UNPREPARED_WORKSPACE,
        };

        const AGENT: &str = "acme:core/barb";

        fn host() -> (tokio::runtime::Runtime, Arc<Host>) {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let host = Host::new(
                "instance".into(),
                ServerIdentity::ephemeral("test"),
                crate::testing::host_environment(),
                runtime.handle().clone(),
            );
            *lock(&host.arm_test_command) = Some("sleep 60".into());
            (runtime, host)
        }

        fn target() -> ArmTarget {
            ArmTarget {
                agent: AGENT.into(),
                workspace_id: "objects".into(),
                cwd: "/tmp".into(),
                owner: None,
                allow_wake: false,
            }
        }

        fn agent_tags(extra: &[(&str, &str)]) -> Tags {
            let mut tags = Tags::new();
            tags.insert("remoteAgentKey".into(), format!("{AGENT}@3"));
            tags.insert("remoteWorkspaceId".into(), "objects".into());
            for (k, v) in extra {
                tags.insert((*k).into(), (*v).into());
            }
            tags
        }

        fn recipe() -> ArmLaunch {
            ArmLaunch {
                launch: NativeLaunch::Cli(AgentCli::Claude),
                agent_startup: codesense_host_proto::reverse_mcp::AgentStartup {
                    args: vec![],
                    settings: Default::default(),
                    prompt: None,
                    env: Default::default(),
                },
                prepared_paths: vec![],
                prepared_digest: String::new(),
                workspace: WorkspaceKey::new("ws"),
                descriptor: Some(WorkspaceDescriptor::default()),
                tags: agent_tags(&[]),
            }
        }

        fn open(session: &str, tags: Tags) -> OpenTab {
            OpenTab {
                viewer: ViewerId::new("app"),
                workspace: WorkspaceKey::new("ws"),
                session: PtySessionId::new(session),
                descriptor: Some(WorkspaceDescriptor::default()),
                name: None,
                role: None,
                tags,
                placement: Placement::Tab,
                index: None,
                activate: false,
            }
        }

        /// A desktop's launch-if-absent of the agent, reserved as `session`.
        fn desk_reserve(
            host: &Arc<Host>,
            conn: &Conn,
            frames: &mpsc::Receiver<QueuedFrame>,
            rid: u64,
            session: &str,
        ) -> Result<Reply, String> {
            host.handle(
                conn,
                rid,
                Request::ReserveTab {
                    request: open(session, agent_tags(&[(TAG_LAUNCH_IF_ABSENT, "1")])),
                },
            );
            match reply_to(frames, rid) {
                Ok(Reply::LayoutRefused { error }) => Err(error.to_string()),
                other => other,
            }
        }

        fn launch(host: &Arc<Host>, session: &str) -> Result<(), LaunchFailure> {
            host.arm_launch(
                &target(),
                &recipe(),
                &PtySessionId::new(session),
                "task",
                Path::new("/tmp"),
                true,
                "desk",
            )
        }

        #[test]
        fn an_armed_old_codex_warns_that_inbox_wake_needs_an_update() {
            let (root, _, _) = crate::native_launch::tests::fixture_answering(
                NativeLaunch::Cli(AgentCli::Codex),
                "--no-daemon --config",
                "codex-cli 0.158.0",
                "[]",
            );
            let shell = root.path().join("bin/bash");
            std::fs::copy(root.path().join("bin/login-shell"), &shell).unwrap();
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let host = Host::new(
                "instance".into(),
                ServerIdentity::ephemeral("test"),
                HostEnvironment::with_vars(
                    None,
                    None,
                    [
                        ("SHELL".into(), shell.to_str().unwrap().into()),
                        ("PATH".into(), "/usr/bin:/bin".into()),
                        ("HOME".into(), root.path().to_str().unwrap().into()),
                    ],
                ),
                runtime.handle().clone(),
            );
            let activity_root = tempfile::tempdir_in("/tmp").unwrap();
            std::fs::set_permissions(
                activity_root.path(),
                std::os::unix::fs::PermissionsExt::from_mode(0o700),
            )
            .unwrap();
            host.start_activity(activity_root.path()).unwrap();
            let mut recipe = recipe();
            recipe.launch = NativeLaunch::Cli(AgentCli::Codex);
            recipe.agent_startup.settings = "{}".parse().unwrap();
            let target = ArmTarget {
                cwd: root.path().to_str().unwrap().into(),
                ..target()
            };
            let prepared = host
                .arm_prepare(
                    &target,
                    &recipe,
                    &PtySessionId::new("armed-old-codex"),
                    "task",
                    &root.path().join("config"),
                    "desk",
                )
                .unwrap();
            assert!(prepared.writer.is_some());
            let snapshot = host
                .terminal
                .activity()
                .snapshot("armed-old-codex")
                .unwrap();
            assert_eq!(snapshot.reason.as_deref(), Some("inbox-update-required"));
            assert!(!snapshot.observed);
            assert!(snapshot.exit.is_none());
        }

        /// A fired arm starts its saved Agent with the agent's own variables (the arm's
        /// `agent_startup.env`) on the CLI's process, under CodeSense's: the session id is the
        /// launch's, `SHELL` is the host's, and the names that are always CodeSense's are never
        /// taken from the agent, set here or not.
        #[test]
        fn an_armed_agent_starts_with_its_own_variables_under_codesense_ones() {
            use std::os::unix::fs::PermissionsExt;
            let root = tempfile::tempdir().unwrap();
            let bin = root.path().join("bin");
            std::fs::create_dir(&bin).unwrap();
            let executable = |name: &str, body: String| {
                let path = bin.join(name);
                std::fs::write(&path, body).unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
                path
            };
            // The account's login shell answers a PATH of this directory alone, so the CLI
            // found there is the fixture below, never one installed on this machine.
            let shell = executable(
                "bash",
                format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", bin.display()),
            );
            let dump = root.path().join("env-dump");
            executable(
                "claude",
                format!(
                    "#!/bin/sh\nif [ \"$1\" = --help ]; then printf '%s\\n' '--mcp-config --strict-mcp-config --settings'; \
                     elif [ \"$1\" = --version ]; then printf '2.1.0 (Claude Code)'; \
                     else /usr/bin/env > '{dump}.part' && /bin/mv '{dump}.part' '{dump}'; /bin/sleep 2; fi\n",
                    dump = dump.display()
                ),
            );
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let host = Host::new(
                "instance".into(),
                ServerIdentity::ephemeral("test"),
                codesense_pty_core::spawn::HostEnvironment::with_vars(
                    None,
                    None,
                    [
                        ("SHELL", shell.to_str().unwrap()),
                        ("PATH", "/usr/bin:/bin"),
                        ("HOME", root.path().to_str().unwrap()),
                    ]
                    .map(|(key, value)| (key.to_string(), value.to_string())),
                ),
                runtime.handle().clone(),
            );
            let owned = [
                ("CODESENSE_TAB_SESSION_ID", "spoofed"),
                ("CODESENSE_API_PORT", "1"),
                ("CODESENSE_MCP_CONFIG", "/agent.json"),
                (codesense_pty_core::activity::HOST_ENV, "/agent"),
                (codesense_pty_core::activity::SOCKET_ENV, "/agent.sock"),
                (codesense_pty_core::activity::TOKEN_ENV, "agent"),
                ("SHELL", "/agent/sh"),
            ];
            let mut recipe = recipe();
            recipe.agent_startup.settings = "{}".parse().unwrap();
            recipe.agent_startup.env = owned
                .iter()
                .chain(&[
                    ("AGENT_ONLY", "yes"),
                    ("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1"),
                ])
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect();
            let target = ArmTarget {
                cwd: root.path().to_str().unwrap().into(),
                ..target()
            };
            assert_eq!(
                host.arm_launch(
                    &target,
                    &recipe,
                    &PtySessionId::new("armed-env"),
                    "task",
                    &root.path().join("config"),
                    true,
                    "desk",
                ),
                Ok(())
            );
            assert!(wait_until(|| dump.exists()), "the agent CLI ran");
            let text = std::fs::read_to_string(&dump).unwrap();
            let vars: std::collections::BTreeMap<&str, &str> = text
                .split('\n')
                .filter_map(|line| line.split_once('='))
                .collect();
            assert_eq!(vars.get("AGENT_ONLY"), Some(&"yes"));
            assert_eq!(vars.get("CLAUDE_CODE_DISABLE_AUTO_MEMORY"), Some(&"1"));
            assert_eq!(vars.get("CODESENSE_TAB_SESSION_ID"), Some(&"armed-env"));
            assert_eq!(vars.get("SHELL").copied(), shell.to_str());
            for (name, agent_value) in owned {
                assert_ne!(vars.get(name), Some(&agent_value), "{name} is CodeSense's");
            }
        }

        #[test]
        fn a_desktop_launch_if_absent_is_refused_while_the_machine_starts_the_agent() {
            let (_rt, host) = host();
            let (conn, frames, _peer) = test_conn(64);
            host.connect(&conn);
            *lock(&host.arm_spawn_delay) = Some(Duration::from_millis(400));
            let machine = {
                let host = host.clone();
                std::thread::spawn(move || launch(&host, "routine-a"))
            };
            assert!(wait_until(
                || lock(&host.agent_flights).contains_key(&PtySessionId::new("routine-a"))
            ));
            // While it starts: a desktop's launch-if-absent is refused, and
            // a wake sees it as the agent's session, busy.
            let refused = desk_reserve(&host, &conn, &frames, 1, "desk-a").unwrap_err();
            assert!(refused.starts_with(AGENT_RUNNING), "{refused}");
            let starting = host.arm_candidates(&target());
            assert_eq!(starting.len(), 1);
            assert!(starting[0].starting && starting[0].session.as_str() == "routine-a");
            assert_eq!(machine.join().unwrap(), Ok(()));
            // Up: still refused (it runs), and a wake's candidate, live.
            let refused = desk_reserve(&host, &conn, &frames, 2, "desk-b").unwrap_err();
            assert!(refused.starts_with(AGENT_RUNNING), "{refused}");
            let up = host.arm_candidates(&target());
            assert!(up.len() == 1 && !up[0].starting, "{up:?}");
            // An ordinary launch of the agent (no if-absent) is a person's
            // choice: never refused.
            host.handle(
                &conn,
                3,
                Request::ReserveTab {
                    request: open("by-hand", agent_tags(&[])),
                },
            );
            assert_eq!(reply_to(&frames, 3), Ok(Reply::Done));
        }

        #[test]
        fn the_machine_wakes_the_session_a_desktop_is_starting_instead_of_launching() {
            let (_rt, host) = host();
            let (conn, frames, _peer) = test_conn(64);
            host.connect(&conn);
            assert_eq!(
                desk_reserve(&host, &conn, &frames, 1, "desk-a"),
                Ok(Reply::Done)
            );
            assert_eq!(
                launch(&host, "routine-a"),
                Err(LaunchFailure::Busy(PtySessionId::new("desk-a")))
            );
            let starting = host.arm_candidates(&target());
            assert!(starting.len() == 1 && starting[0].starting, "{starting:?}");
            assert!(lock(&host.agent_flights).is_empty(), "nothing reserved");
        }

        // Concurrent fires: exactly one of a desktop's launch-if-absent and
        // the machine's wins, every time.
        #[test]
        fn concurrent_desktop_and_machine_launches_start_one_agent() {
            for round in 0..12u64 {
                let (_rt, host) = host();
                let (conn, frames, _peer) = test_conn(64);
                host.connect(&conn);
                let barrier = Arc::new(std::sync::Barrier::new(2));
                let machine = {
                    let (host, barrier) = (host.clone(), barrier.clone());
                    std::thread::spawn(move || {
                        barrier.wait();
                        launch(&host, "routine-a")
                    })
                };
                barrier.wait();
                let desk = desk_reserve(&host, &conn, &frames, round + 1, "desk-a");
                let machine = machine.join().unwrap();
                let won = [desk.is_ok(), machine.is_ok()];
                assert_eq!(
                    won.iter().filter(|w| **w).count(),
                    1,
                    "round {round}: desk {desk:?}, machine {machine:?}"
                );
            }
        }

        // Re-review MUST: between a desktop's launch-if-absent leaving
        // `openings` and its tab being placed, the machine's launch-if-absent
        // must still find it (Busy), never start a second copy.
        #[test]
        fn a_launch_if_absent_racing_a_desktop_tab_being_placed_is_busy() {
            let (_rt, host) = host();
            let (conn, frames, _peer) = test_conn(64);
            host.connect(&conn);
            assert_eq!(
                desk_reserve(&host, &conn, &frames, 1, "desk-a"),
                Ok(Reply::Done)
            );
            host.handle(
                &conn,
                2,
                Request::Spawn {
                    intent: sh("sleep 60"),
                    registration: registration("desk-a", None, false),
                },
            );
            assert!(matches!(reply_to(&frames, 2), Ok(Reply::Spawned { .. })));
            type Racer = Option<std::thread::JoinHandle<Result<(), LaunchFailure>>>;
            let racer: Arc<Mutex<Racer>> = Arc::default();
            {
                let weak = Arc::downgrade(&host);
                let racer = racer.clone();
                *lock(&host.finish_tab_hook) = Some(Box::new(move || {
                    let Some(host) = weak.upgrade() else { return };
                    let started = Arc::new(std::sync::Barrier::new(2));
                    let thread = {
                        let started = started.clone();
                        std::thread::spawn(move || {
                            started.wait();
                            launch(&host, "routine-a")
                        })
                    };
                    started.wait();
                    // Hold the window open: a launch that does not wait for
                    // the tab to be placed decides now.
                    std::thread::sleep(Duration::from_millis(400));
                    *lock(&racer) = Some(thread);
                }));
            }
            host.handle(
                &conn,
                3,
                Request::FinishTab {
                    session: PtySessionId::new("desk-a"),
                    spawned: Ok(()),
                },
            );
            assert!(matches!(reply_to(&frames, 3), Ok(Reply::Tab { .. })));
            *lock(&host.finish_tab_hook) = None;
            let thread = lock(&racer).take().expect("the hook ran");
            assert_eq!(
                thread.join().unwrap(),
                Err(LaunchFailure::Busy(PtySessionId::new("desk-a")))
            );
        }

        // A launch not answered in time is unknown; the flight holds
        // until its spawn ends, and a spawn that registers late is stopped.
        #[test]
        fn a_launch_that_times_out_is_unknown_holds_the_flight_and_a_late_spawn_is_stopped() {
            let (_rt, host) = host();
            let (conn, frames, _peer) = test_conn(64);
            host.connect(&conn);
            host.set_arm_launch_timeout(Duration::from_millis(100));
            *lock(&host.arm_spawn_delay) = Some(Duration::from_millis(800));
            let session = PtySessionId::new("routine-late");
            assert!(matches!(
                launch(&host, "routine-late"),
                Err(LaunchFailure::Unknown(_))
            ));
            assert!(lock(&host.agent_flights).contains_key(&session));
            let refused = desk_reserve(&host, &conn, &frames, 1, "desk-a").unwrap_err();
            assert!(refused.starts_with(AGENT_RUNNING), "{refused}");
            assert!(wait_until(|| lock(&host.agent_flights).is_empty()));
            assert!(wait_until(|| !host.wake_session_live(&session)));
            assert!(host
                .terminal
                .tab_entries()
                .unwrap()
                .iter()
                .all(|(_, tab)| tab.session != session));
        }

        // A tab that skips permission checks is a wake's
        // candidate only when the agent set allowWake; an arm whose
        // workspace is not found yet has none, not even untagged tabs.
        #[test]
        fn candidates_respect_the_wake_posture_and_the_unprepared_workspace() {
            let (_rt, host) = host();
            let (conn, frames, _peer) = test_conn(64);
            host.connect(&conn);
            let tabs = [
                ("plain", agent_tags(&[])),
                (
                    "raw-bypass",
                    agent_tags(&[("remotePermissionMode", "\"bypass\"")]),
                ),
            ];
            for (i, (session, tags)) in tabs.iter().enumerate() {
                let rid = 10 * (i as u64 + 1);
                host.handle(
                    &conn,
                    rid,
                    Request::ReserveTab {
                        request: open(session, tags.clone()),
                    },
                );
                assert_eq!(reply_to(&frames, rid), Ok(Reply::Done));
                host.handle(
                    &conn,
                    rid + 1,
                    Request::Spawn {
                        intent: sh("sleep 60"),
                        registration: registration(session, None, false),
                    },
                );
                assert!(matches!(
                    reply_to(&frames, rid + 1),
                    Ok(Reply::Spawned { .. })
                ));
                host.handle(
                    &conn,
                    rid + 2,
                    Request::FinishTab {
                        session: PtySessionId::new(*session),
                        spawned: Ok(()),
                    },
                );
                assert!(matches!(reply_to(&frames, rid + 2), Ok(Reply::Tab { .. })));
            }
            let names = |target: &ArmTarget| {
                let mut names: Vec<String> = host
                    .arm_candidates(target)
                    .into_iter()
                    .map(|c| c.session.as_str().to_string())
                    .collect();
                names.sort();
                names
            };
            assert_eq!(names(&target()), ["plain"]);
            let mut opted = target();
            opted.allow_wake = true;
            assert_eq!(names(&opted), ["plain", "raw-bypass"]);
            let mut unprepared = target();
            unprepared.workspace_id = UNPREPARED_WORKSPACE.into();
            assert!(names(&unprepared).is_empty());
        }

        #[test]
        fn a_routine_binding_secret_is_64_hex_digits() {
            let (_rt, host) = host();
            let (conn, frames, _peer) = test_conn(64);
            host.connect(&conn);
            host.handle(
                &conn,
                1,
                Request::RoutineArmBindSecret {
                    secret: "short".into(),
                    desktop: None,
                },
            );
            assert!(reply_to(&frames, 1).is_err());
            host.handle(
                &conn,
                2,
                Request::RoutineArmBindSecret {
                    secret: "a".repeat(64),
                    desktop: None,
                },
            );
            assert_eq!(reply_to(&frames, 2), Ok(Reply::Done));
        }
    }
}

#[cfg(test)]
mod activity_root_tests {
    use super::activity_root;
    use std::path::Path;

    #[test]
    fn a_plain_root_is_used_as_it_is() {
        assert_eq!(
            activity_root(Path::new("/home/dev/.codesense")).unwrap(),
            Path::new("/home/dev/.codesense")
        );
    }

    // A private-account install refuses any file with
    // a second hard link, so the shells' helper there must be a symbolic
    // link; the executable keeps exactly the links it had.
    #[test]
    fn a_symbolic_helper_never_adds_a_hard_link_to_the_binary() {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let exe = std::env::current_exe().unwrap();
        let before = std::fs::metadata(&exe).unwrap().nlink();
        let service = codesense_pty_core::activity::ActivityService::start_with(
            dir.path(),
            &std::sync::Arc::new(codesense_pty_core::activity::ActivityOwner::new()),
            codesense_pty_core::activity::HelperLink::Symbolic,
        )
        .unwrap();
        let helper = dir.path().join(codesense_pty_core::activity::HELPER_NAME);
        assert!(std::fs::symlink_metadata(&helper)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::metadata(&exe).unwrap().nlink(), before);
        drop(service);
    }

    // A private-account root is `/proc/self/fd/N`; the activity
    // socket goes in the real directory that fd holds.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_pinned_fd_root_names_its_real_directory() {
        use std::os::unix::io::AsRawFd;
        let dir = tempfile::tempdir().unwrap();
        let held = std::fs::File::open(dir.path()).unwrap();
        let root = format!("/proc/self/fd/{}", held.as_raw_fd());
        assert_eq!(
            activity_root(Path::new(&root)).unwrap(),
            std::fs::canonicalize(dir.path()).unwrap()
        );
        let service = codesense_pty_core::activity::ActivityService::start(
            &activity_root(Path::new(&root)).unwrap(),
            &std::sync::Arc::new(codesense_pty_core::activity::ActivityOwner::new()),
        );
        assert!(service.is_ok(), "the service starts in the real directory");
    }
}
