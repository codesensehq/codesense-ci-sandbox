// SPDX-License-Identifier: MIT
//! The live PTY session registry: spawn registration, the reader/batcher
//! pipeline, teardown + reaping, the dead-session cap, and the liveness /
//! persistence queries. Every host side effect leaves through the injected
//! [`TerminalEventSink`]; blocking work (reaps) runs on the injected blocking
//! pool.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use log::{error, info, trace, warn};

use crate::batcher::{run_pty_batcher, PTY_BATCH_WINDOW, PTY_IDLE_TIMEOUT, PTY_MAX_BATCH_BYTES};
use crate::domain::{
    AttachOutcome, PtySessionId, ReapOutcome, SessionView, TeardownReason, TerminalDomainEvent,
    TerminalEventSink, TerminalRegistry,
};
use crate::emulator::{
    EmulatorDump, EmulatorLink, EmulatorText, PendingReply, SessionEmulator, TerminalSnapshot,
    TextMode, DEFAULT_SCROLLBACK,
};
use crate::layout::LayoutModel;
use crate::osc::{OscEvent, OscEventAt, OscParser};
use crate::process::has_live_descendant;
use crate::runner::{ChildHandle, PtySession};
use crate::term_state::{SessionRuntime, SharedTermState, StreamId, SubscriberHandle};

/// Correlation metadata — the opaque story / workspace / epic keys threaded
/// through the spawn → warm-handoff chain and stamped onto a session. This
/// groups the three `Option<String>`s into one value so a call site cannot
/// transpose two same-typed args.
///
/// No validation — the frontend is the authority on whether the keys exist.
/// `SessionMeta::default()` (all `None`) is the warm-pool-member sentinel the
/// persistence classifier relies on (a pool member spawns with all-`None` and
/// gains real keys only at handoff).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionMeta {
    pub story_id: Option<String>,
    pub workspace_id: Option<String>,
    pub epic_id: Option<String>,
}

/// A live PTY plus the sovereign-PTY substrate it owns. `pty` drives
/// resize/write/kill; the extra fields hold the source of truth for terminal
/// *state*.
pub(crate) struct TerminalSession {
    pub(crate) pty: PtySession,

    /// Opaque correlation keys. Set at spawn; the warm handoff stamps them
    /// at warm-pool handoff (a pool member spawns with all `None` and gains them
    /// when handed off). Pool members are excluded everywhere precisely because
    /// all three are `None`.
    pub(crate) story_id: Option<String>,
    pub(crate) workspace_id: Option<String>,
    pub(crate) epic_id: Option<String>,

    /// Byte counter, emulator terminal and viewport subscriber registry, behind
    /// a per-session lock so the batcher fan-out and an attach never contend
    /// on the global session map. The batcher holds its own clone of
    /// this `Arc`.
    pub(crate) shared: Arc<Mutex<SharedTermState>>,

    /// Working directory reported by the shell via OSC 7, updated by the
    /// batcher side-tap on every prompt. Behind its own lock so the batcher
    /// writes it and `session_cwd` reads it without locking the session map.
    pub(crate) cwd: Arc<Mutex<Option<PathBuf>>>,

    /// RF-perf-17: the STABLE directory the shell was spawned in, captured once
    /// at spawn and moved only by a warm handoff. Distinct from `cwd` (the OSC-7
    /// live cwd, which starts `None` and wanders as the user `cd`s): reverse-sync
    /// liveness must anchor a session to the worktree it was OPENED for, not
    /// wherever its shell has since navigated. `None` only for a non-worktree
    /// shell — such a session simply never contributes to the live-CLI set.
    pub(crate) home_cwd: Option<PathBuf>,

    /// Live, mutable control shared with the batcher thread — the emitted
    /// session id (renamed at handoff), the pool-member flag, and the output
    /// gate. The batcher reads these every batch so warm-pool handoff is seen by
    /// the already-running batcher without rebuilding it.
    pub(crate) runtime: Arc<SessionRuntime>,
}

impl TerminalSession {
    /// PID of the shell while it is still running — `None` once it exited (the
    /// numeric PID may then be recycled by the OS, so inspecting its
    /// descendants would be unsafe) or when the driver exposes none.
    fn live_pid(&self) -> Option<u32> {
        if self.pty.is_alive() {
            self.pty.pid()
        } else {
            None
        }
    }

    /// Whether a viewport is attached right now. On lock contention assume
    /// `contended` (callers choose the conservative reading).
    fn has_subscribers_or(&self, contended: bool) -> bool {
        self.shared
            .try_lock()
            .map(|g| !g.subscribers.is_empty())
            .unwrap_or(contended)
    }
}

/// The registry-side identity and substrate settings for a freshly spawned
/// [`PtySession`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionRegistration {
    pub session_id: String,
    pub meta: SessionMeta,
    /// The resolved spawn directory (`None` for a non-worktree shell) — the
    /// session's reverse-sync liveness anchor.
    pub home_cwd: Option<PathBuf>,
    /// Run the OSC 7/133 side-tap (shell integration on an interactive shell).
    pub osc_enabled: bool,
    /// Spawn an unattached warm-pool member.
    pub is_pool_member: bool,
    /// Scrollback lines of the session's emulator terminal: the display's, so
    /// a rebuilt display holds what the user could scroll back to.
    #[serde(default = "default_scrollback")]
    pub scrollback: u32,
}

fn default_scrollback() -> u32 {
    DEFAULT_SCROLLBACK
}

/// RF-perf-6: cap on retained naturally-exited PTY sessions. Each dead
/// session pins its emulator terminal's scrollback, so an unbounded dead set
/// grows RSS per closed-but-undismissed agent across a long, many-worktree
/// session. Retaining the 8 most-recently-dead keeps the common "reopen a
/// just-closed agent" case while bounding the tail. Tune against
/// RF-perf-5's `pty_dead` telemetry. LIVE sessions are never counted here.
const PTY_DEAD_SESSION_CAP: usize = 8;

/// How long an attach waits for the emulator's snapshot: the worker answers
/// after the output queued before it and a serialize of the terminal, well
/// under a second even for a full scrollback.
const SUBSCRIBE_SNAPSHOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Injectable failure point for the reader/batcher thread start, so the
/// failed-start teardown is testable without exhausting the OS thread table.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackgroundThreadFailure {
    None,
    Reader,
    Batcher,
}

/// The process-global PTY session registry.
pub struct PtyState {
    pub(crate) activity: Arc<crate::activity::ActivityOwner>,
    pub(crate) sessions: Mutex<HashMap<String, TerminalSession>>,
    /// `StreamId → per-session shared state`, so an unsubscribe can find and
    /// detach a subscriber from a `stream_id` alone in O(1).
    pub(crate) stream_index: Mutex<HashMap<StreamId, Arc<Mutex<SharedTermState>>>>,
    /// The host's one side-effect adapter.
    pub(crate) sink: Arc<dyn TerminalEventSink>,
    /// The bounded blocking pool reaps run on.
    pub(crate) blocking: tokio::runtime::Handle,
    /// The host's layout: an exited session that still shows as a tab keeps
    /// its scrollback until the tab is closed, so the dead-session cap spares it.
    layout: Arc<LayoutModel>,
    /// Respawns in place under way, by session id: the reaped session's
    /// emulator terminal until the new session registers (it takes it over),
    /// then the mark that the id's process is the respawn's own.
    respawns: Mutex<HashMap<String, RespawnSlot>>,
    /// The host's emulator worker queue; every session's terminal goes
    /// through it.
    pub(crate) emulator: Arc<EmulatorLink>,
    /// Emit a retained session's exit again when it is used as if it still
    /// ran ([`Self::redeliver_exits_on_use`]).
    redeliver_on_use: AtomicBool,
}

/// One respawn in place, as the registry sees it.
enum RespawnSlot {
    /// Begun: the next session registered under the id is the respawn's, and
    /// takes over the reaped session's emulator terminal (none when there was
    /// no session to reap).
    Armed(Option<SessionEmulator>),
    /// The respawn's process registered under the id.
    Registered,
}

impl RespawnSlot {
    /// A respawn that ended without taking over the reaped session's terminal
    /// frees it.
    fn abandon(self) {
        if let Self::Armed(Some(emulator)) = self {
            emulator.dispose();
        }
    }
}

impl PtyState {
    /// Built only by [`TerminalHost::new`](crate::TerminalHost::new), which
    /// wraps `sink` so `layout` follows every exit and removal.
    pub(crate) fn new(
        sink: Arc<dyn TerminalEventSink>,
        blocking: tokio::runtime::Handle,
        layout: Arc<LayoutModel>,
        adapters: crate::agent_cli::Adapters,
    ) -> Self {
        Self {
            activity: Arc::new(crate::activity::ActivityOwner::with_sink(
                sink.clone(),
                adapters,
            )),
            sessions: Mutex::new(HashMap::new()),
            stream_index: Mutex::new(HashMap::new()),
            sink,
            blocking,
            layout,
            respawns: Mutex::new(HashMap::new()),
            emulator: EmulatorLink::new(),
            redeliver_on_use: AtomicBool::new(false),
        }
    }

    /// Forget `session_id`'s respawn once it finished: its carried scrollback
    /// and its registered mark.
    pub fn drop_respawn(&self, session_id: &str) {
        let removed = self
            .respawns
            .lock()
            .ok()
            .and_then(|mut respawns| respawns.remove(session_id));
        if let Some(slot) = removed {
            slot.abandon();
        }
    }

    /// A respawn of `session_id` abandoned before it finished (its spawn failed
    /// or panicked, or its client went away): its carried scrollback goes, but
    /// a process it already registered keeps its mark, so the next respawn
    /// revives that process instead of refusing it.
    pub fn abandon_respawn(&self, session_id: &str) {
        let removed = self.respawns.lock().ok().and_then(|mut respawns| {
            matches!(respawns.get(session_id), Some(RespawnSlot::Armed(..)))
                .then(|| respawns.remove(session_id))
                .flatten()
        });
        if let Some(slot) = removed {
            slot.abandon();
        }
    }

    /// Begin a respawn of `session_id` that reaped nothing (no session was
    /// left under the id): the next session registered there is its own.
    pub fn arm_respawn(&self, session_id: &str) {
        if let Ok(mut respawns) = self.respawns.lock() {
            respawns
                .entry(session_id.to_string())
                .or_insert(RespawnSlot::Armed(None));
        }
    }

    /// Whether the process under `session_id` registered for a respawn of it
    /// (and not, say, a process that outlived its terminal). Consumes the mark.
    pub fn take_respawned(&self, session_id: &str) -> bool {
        self.respawns.lock().is_ok_and(|mut respawns| {
            matches!(respawns.get(session_id), Some(RespawnSlot::Registered))
                && respawns.remove(session_id).is_some()
        })
    }

    /// The shared state a session registered as `session_id` starts with. Its
    /// emulator terminal is a new one, or the reaped session's, told of the new
    /// process (the worker carries its normal buffer and marks the seam). A
    /// dev build records the stream when asked to.
    fn starting_state(
        &self,
        session_id: &str,
        size: (u16, u16),
        scrollback: u32,
    ) -> SharedTermState {
        let mut shared = SharedTermState::default();
        #[cfg(debug_assertions)]
        {
            shared.recorder = crate::recorder::PtyRecorder::from_env(session_id);
        }
        let carried_emulator =
            self.respawns
                .lock()
                .ok()
                .and_then(|mut respawns| match respawns.get_mut(session_id) {
                    Some(slot @ RespawnSlot::Armed(..)) => {
                        match std::mem::replace(slot, RespawnSlot::Registered) {
                            RespawnSlot::Armed(emulator) => emulator,
                            RespawnSlot::Registered => None,
                        }
                    }
                    _ => None,
                });
        shared.emulator = Some(match carried_emulator {
            Some(mut emulator) => {
                emulator.respawn(shared.seq, size);
                emulator
            }
            None => SessionEmulator::start(&self.emulator, size, scrollback, shared.seq),
        });
        shared
    }

    /// Register a session a [`PtyRunner`](crate::runner::PtyRunner) just
    /// spawned: insert it (retiring any same-id owner), bound the retained-dead
    /// set, announce the live worktree, and start its reader + batcher threads.
    /// Synchronous and potentially blocking (a displaced or capped session's
    /// teardown) — hosts call it off their async workers. On failure the session
    /// is torn down; a thread-start failure is also handed to the sink as an
    /// `Expire` teardown.
    pub fn register(
        &self,
        mut pty: PtySession,
        registration: SessionRegistration,
    ) -> Result<(), String> {
        let SessionRegistration {
            session_id,
            meta,
            home_cwd,
            osc_enabled,
            is_pool_member,
            scrollback,
        } = registration;
        let Some(reader) = pty.take_reader() else {
            pty.discard();
            return Err("PTY reader unavailable".to_string());
        };
        let child = pty.child();

        // Per-session substrate state. The batcher gets its own clone of the `Arc`
        // and never touches the global session map on the hot path.
        // A respawn in place starts with the ended session's terminal.
        let shared = Arc::new(Mutex::new(self.starting_state(
            &session_id,
            pty.size(),
            scrollback,
        )));
        // Shared cwd handle written by the OSC 7 side-tap in the batcher.
        let cwd: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
        // Shared runtime control. The batcher reads the (mutable) session id,
        // pool-member flag, and output gate from this every batch, so a warm-pool
        // handoff (rename + clear flag + arm gate) is reflected live.
        let runtime = Arc::new(SessionRuntime::new(session_id.clone(), is_pool_member));
        if !is_pool_member {
            if let Some(incarnation) = self.activity.register(&session_id, pty.pid()) {
                self.activity
                    .attach_child(&session_id, &incarnation, pty.child());
                let _ =
                    runtime
                        .activity
                        .set((self.activity.clone(), session_id.clone(), incarnation));
            }
        }

        // `insert` returns any displaced same-id session — a respawn-in-place
        // without an explicit kill (e.g. auto-resume reusing the session id after
        // natural exit). Tear that old session down after releasing the map lock
        // to keep the lock order acyclic (prune takes shared→stream_index).
        self.insert_session_reaping_displaced(
            session_id.clone(),
            TerminalSession {
                pty,
                story_id: meta.story_id,
                workspace_id: meta.workspace_id,
                epic_id: meta.epic_id,
                shared: shared.clone(),
                cwd: cwd.clone(),
                home_cwd: home_cwd.clone(),
                runtime: runtime.clone(),
            },
        )?;

        // RF-perf-6: bound the retained-dead session set now that the map
        // has grown. The just-added session is alive → never a victim.
        // Non-blocking (`try_lock`-or-skip).
        self.enforce_dead_session_cap(PTY_DEAD_SESSION_CAP);

        // RF-perf-17: a newly-spawned session makes its worktree live for
        // reverse sync. Pool members are skipped: they boot in a neutral warm dir
        // and become live for their real worktree at handoff.
        if !is_pool_member {
            if let Some(dir) = home_cwd {
                let _ = self
                    .sink
                    .emit(TerminalDomainEvent::WorktreeLive { cwd: dir });
            }
        }

        // Reader thread: blocking read loop, sends raw byte chunks through channel.
        // Batcher thread: coalesces chunks on a time window, delivers valid UTF-8.
        let (tx, rx) = mpsc::channel::<Option<Vec<u8>>>();
        let sink = self.sink.clone();
        self.spawn_session_background_threads(
            &session_id,
            (format!("pty-read-{session_id}"), move || {
                read_pty(reader, tx)
            }),
            (format!("pty-batch-{session_id}"), move || {
                run_session_batcher(&*sink, rx, &shared, &runtime, &cwd, osc_enabled, &child)
            }),
            BackgroundThreadFailure::None,
        )
    }

    /// Insert a session and fully retire any same-ID owner.
    pub(crate) fn insert_session_reaping_displaced(
        &self,
        session_id: String,
        session: TerminalSession,
    ) -> Result<(), String> {
        let displaced = match self.sessions.lock() {
            Ok(mut sessions) => sessions.insert(session_id, session),
            Err(error) => {
                session.pty.discard();
                return Err(format!("Lock error: {error}"));
            }
        };
        if let Some(old) = displaced {
            self.retire(old);
        }
        Ok(())
    }

    /// Kill a session that already left the map (its exit reports as
    /// requested), detach its viewports from the stream index, and reap it off
    /// the caller's path. Call with the map lock
    /// released (prune takes shared→stream_index).
    pub(crate) fn retire(&self, session: TerminalSession) {
        // A displaced session's exit is never redelivered onto the new one.
        session.runtime.retire_exit();
        // The exit this kill causes was asked for: say so before it happens.
        session.runtime.request_exit();
        if let Some((owner, id, incarnation)) = session.runtime.activity.get() {
            owner.remove(id, incarnation);
        }
        session.pty.kill();
        self.prune_subscriber_index(&session);
        let emulator = session
            .shared
            .lock()
            .ok()
            .and_then(|mut shared| shared.emulator.take());
        if let Some(emulator) = emulator {
            emulator.dispose();
        }
        session.pty.reap_in_background(&self.blocking);
    }

    pub(crate) fn spawn_session_background_threads<R, B>(
        &self,
        session_id: &str,
        reader: (String, R),
        batcher: (String, B),
        failure: BackgroundThreadFailure,
    ) -> Result<(), String>
    where
        R: FnOnce() + Send + 'static,
        B: FnOnce() + Send + 'static,
    {
        self.spawn_session_background_thread(
            session_id,
            "PTY reader",
            reader,
            failure == BackgroundThreadFailure::Reader,
        )?;
        self.spawn_session_background_thread(
            session_id,
            "PTY batcher",
            batcher,
            failure == BackgroundThreadFailure::Batcher,
        )
    }

    fn spawn_session_background_thread<F>(
        &self,
        session_id: &str,
        role: &str,
        (thread_name, task): (String, F),
        force_failure: bool,
    ) -> Result<(), String>
    where
        F: FnOnce() + Send + 'static,
    {
        let result = if force_failure {
            Err(std::io::Error::other("injected thread-spawn failure"))
        } else {
            std::thread::Builder::new()
                .name(thread_name)
                .spawn(task)
                .map(|_| ())
        };
        if let Err(error) = result {
            self.teardown(session_id, TeardownReason::Expire);
            return Err(format!("Failed to spawn {role} thread: {error}"));
        }
        Ok(())
    }

    /// Detach a removed session's still-attached viewports from the global stream
    /// index so their entries don't outlive the PTY. Collects ids under the
    /// per-session lock, then prunes the index without holding both locks at once
    /// (keeps the lock order acyclic vs. `unsubscribe`'s index→shared).
    fn prune_subscriber_index(&self, session: &TerminalSession) {
        let stream_ids: Vec<StreamId> = session
            .shared
            .lock()
            .map(|g| g.subscribers.keys().copied().collect())
            .unwrap_or_default();
        if !stream_ids.is_empty() {
            if let Ok(mut index) = self.stream_index.lock() {
                for id in &stream_ids {
                    index.remove(id);
                }
            }
        }
    }

    /// Remove a session from the map, retire it (kill, detach its viewports from
    /// the stream index, reap off the caller's path), and hand the host its
    /// teardown side effects. `reason` records WHY the session left — the single
    /// spelling every removal path shares so their teardown can never drift. A
    /// missing id is a no-op for the registry (the host is still told, so a stale
    /// dashboard binding is freed).
    pub(crate) fn teardown(&self, session_id: &str, reason: TeardownReason) {
        let removed = match self.sessions.lock() {
            Ok(mut sessions) => sessions.remove(session_id),
            Err(e) => {
                error!(target: "pty", "kill_session: lock error: {}", e);
                return;
            }
        };
        self.finish_teardown(session_id, reason, removed);
    }

    /// Remove `session_id` only if its process has ended — checked and removed
    /// under one lock, so a running session is never torn down. The removal's
    /// side effects (a `Reap`) are handed to the host before this returns.
    pub fn reap_ended(&self, session_id: &str, silent_counts_as_ended: bool) -> ReapOutcome {
        let removed = match self.sessions.lock() {
            Ok(mut sessions) => match sessions.get(session_id) {
                None => return ReapOutcome::Missing,
                Some(session)
                    if session.pty.is_alive()
                        && !(silent_counts_as_ended && session.runtime.output_ended()) =>
                {
                    return ReapOutcome::Running
                }
                Some(session) => {
                    // Carried to the session that takes its id next.
                    let emulator = session
                        .shared
                        .lock()
                        .ok()
                        .and_then(|mut shared| shared.emulator.take());
                    let replaced = self.respawns.lock().ok().and_then(|mut respawns| {
                        respawns.insert(session_id.to_string(), RespawnSlot::Armed(emulator))
                    });
                    if let Some(slot) = replaced {
                        slot.abandon();
                    }
                    sessions.remove(session_id)
                }
            },
            Err(e) => {
                error!(target: "pty", "reap_ended: lock error: {}", e);
                return ReapOutcome::Running;
            }
        };
        self.finish_teardown(session_id, TeardownReason::Reap, removed);
        ReapOutcome::Reaped
    }

    fn finish_teardown(
        &self,
        session_id: &str,
        reason: TeardownReason,
        removed: Option<TerminalSession>,
    ) {
        if let Some(session) = &removed {
            // Never redelivered past the removal announced below.
            session.runtime.retire_exit();
            // A deliberate removal is not a pool member's unexpected death:
            // the member's closing stream must not trigger the pool's
            // reclaim-and-refill (e.g. the exit-time complement kill).
            session
                .runtime
                .is_pool_member
                .store(false, Ordering::Relaxed);
        } else {
            warn!(target: "pty", "kill_session: session not found: {}", session_id);
        }
        // Only a reap hands the id on to a respawn; any other removal ends it.
        if reason != TeardownReason::Reap {
            self.drop_respawn(session_id);
        }
        // Announced before the child is killed, so the exit the kill causes
        // always follows the removal: the layout drops the entry once, never
        // first marking it exited (a second, racing layout change).
        let _ = self.sink.emit(TerminalDomainEvent::SessionRemoved {
            session: PtySessionId::new(session_id),
            reason,
        });
        if let Some(session) = removed {
            self.retire(session);
        }
    }

    /// RF-perf-6: bound the retained-dead session set. Collects the
    /// eviction victims under ONE `sessions` `try_lock` (never stalls the
    /// terminal-I/O hot path), DROPS the lock, then routes each victim through
    /// [`Self::teardown`] with an `Expire` reason (a background reclaim, not a
    /// human kill). Live sessions, currently-VIEWED dead sessions, and dead
    /// sessions the layout still holds as tabs are never touched. If the map
    /// lock is busy the pass is skipped (the next spawn re-runs it) rather than
    /// blocking.
    pub(crate) fn enforce_dead_session_cap(&self, cap: usize) {
        let mut snapshot = {
            let Ok(sessions) = self.sessions.try_lock() else {
                return;
            };
            sessions
                .iter()
                .map(|(id, s)| {
                    // The same idempotent probe the liveness paths use, so
                    // counting does not perturb the table.
                    let alive = s.pty.is_alive();
                    // Contended → assume viewed: a session we cannot confirm is
                    // unviewed is conservatively RETAINED (never evict what might
                    // be on screen). `shared` is a leaf lock taken after the map
                    // lock here — consistent with the batcher ordering.
                    let has_subscribers = s.has_subscribers_or(true);
                    (id.clone(), alive, has_subscribers, s.runtime.exit_seq())
                })
                .collect::<Vec<(String, bool, bool, Option<u64>)>>()
        };
        // Outside the map lock: a session the layout holds counts as viewed.
        for (id, _, has_subscribers, _) in &mut snapshot {
            *has_subscribers |= self.layout.is_held(id);
        }
        for id in dead_sessions_to_evict(&snapshot, cap) {
            self.teardown(&id, TeardownReason::Expire);
        }
    }

    /// What a status read needs of each session, warm-pool members
    /// included (each marked).
    pub fn session_facts(&self) -> Vec<SessionFacts> {
        let mut facts: Vec<SessionFacts> = {
            let Ok(sessions) = self.sessions.lock() else {
                return Vec::new();
            };
            sessions
                .iter()
                .map(|(id, s)| SessionFacts {
                    session: PtySessionId::new(id.as_str()),
                    running: s.pty.is_alive(),
                    pool_member: s.runtime.is_pool_member.load(Ordering::Relaxed),
                    // Contended: assume shown, never "nobody holds it".
                    viewed: s.has_subscribers_or(true),
                    layout_entry: false,
                    started_at_ms: s
                        .runtime
                        .started_at
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |since| since.as_millis() as u64),
                    pid: s.live_pid(),
                })
                .collect()
        };
        // Outside the map lock: whether it is an entry of the layout.
        for fact in &mut facts {
            fact.layout_entry = self.layout.has_entry(fact.session.as_str());
        }
        facts
    }

    /// `session_id`'s terminal as the emulator worker has parsed it, after
    /// every batch and resize made before this call.
    pub fn emulator_dump(
        &self,
        session_id: &str,
        timeout: std::time::Duration,
    ) -> Result<EmulatorDump, String> {
        self.emulator_request(session_id, |emulator, seq| emulator.dump(seq))?
            .dump(timeout)
    }

    /// `session_id`'s terminal as text (see [`TextMode`]), ordered like
    /// [`emulator_dump`](Self::emulator_dump).
    pub fn emulator_text(
        &self,
        session_id: &str,
        lines: u32,
        mode: TextMode,
        timeout: std::time::Duration,
    ) -> Result<EmulatorText, String> {
        self.emulator_request(session_id, |emulator, seq| emulator.text(seq, lines, mode))?
            .text(timeout)
    }

    /// `session_id`'s terminal serialized, ordered like
    /// [`emulator_dump`](Self::emulator_dump).
    pub fn emulator_snapshot(
        &self,
        session_id: &str,
        timeout: std::time::Duration,
    ) -> Result<TerminalSnapshot, String> {
        self.emulator_request(session_id, |emulator, seq| emulator.snapshot(seq))?
            .snapshot(timeout)
    }

    /// `session_id`'s full emulator terminal with its PTY's foreground process
    /// group, ordered like [`emulator_dump`](Self::emulator_dump).
    #[cfg(feature = "e2e-dump")]
    pub fn debug_dump(
        &self,
        session_id: &str,
        timeout: std::time::Duration,
    ) -> Result<crate::domain::DebugDump, String> {
        let fg_pgid = self
            .sessions
            .lock()
            .map_err(|e| format!("Lock error: {e}"))?
            .get(session_id)
            .and_then(|s| s.pty.foreground_pgid());
        let dump = self.emulator_dump(session_id, timeout)?;
        Ok(crate::domain::DebugDump {
            seq: dump.seq_end,
            resize_gen: dump.resize_gen,
            history_lost: dump.history_lost,
            fg_pgid,
            dump: dump.buffer,
        })
    }

    /// Bytes of `session_id`'s output enqueued for the emulator worker that it
    /// has not parsed yet; `None` for an unknown session.
    pub fn emulator_lag_bytes(&self, session_id: &str) -> Option<u64> {
        let shared = self.session_shared(session_id).ok()?;
        let key = shared.lock().ok()?.emulator.as_ref()?.key();
        Some(self.emulator.lag_bytes(key))
    }

    fn session_shared(&self, session_id: &str) -> Result<Arc<Mutex<SharedTermState>>, String> {
        self.sessions
            .lock()
            .map_err(|e| format!("Lock error: {e}"))?
            .get(session_id)
            .map(|s| s.shared.clone())
            .ok_or_else(|| format!("session not found: {session_id}"))
    }

    /// Enqueue a read of `session_id`'s terminal under its session lock, so it
    /// is answered after exactly the output delivered so far.
    fn emulator_request(
        &self,
        session_id: &str,
        ask: impl FnOnce(&mut SessionEmulator, u64) -> Option<PendingReply>,
    ) -> Result<PendingReply, String> {
        let shared = self.session_shared(session_id)?;
        let mut g = shared.lock().map_err(|e| format!("Lock error: {e}"))?;
        let seq = g.seq;
        let emulator = g
            .emulator
            .as_mut()
            .ok_or_else(|| format!("session {session_id} has no emulator terminal"))?;
        ask(emulator, seq).ok_or_else(|| "no emulator worker is running".to_string())
    }

    /// Detach a viewport. The PTY and its terminal keep running; only this
    /// subscriber's live-stream registration is removed. Idempotent — `false`
    /// if the stream is already gone (e.g. the PTY was killed first).
    pub fn unsubscribe(&self, stream_id: StreamId) -> Result<bool, String> {
        let shared = self
            .stream_index
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?
            .remove(&stream_id);
        if let Some(shared) = &shared {
            if let Ok(mut g) = shared.lock() {
                g.detach(&stream_id);
            }
        }
        Ok(shared.is_some())
    }

    /// Does `session_id` name a session the registry currently
    /// tracks? Cheap membership check (does NOT probe process liveness) — the MCP
    /// verbs' cross-talk guard uses it to reject a forged/stale, agent-supplied
    /// id. A poisoned lock conservatively reports "not known".
    pub fn has_session(&self, session_id: &str) -> bool {
        self.sessions
            .lock()
            .map(|s| s.contains_key(session_id))
            .unwrap_or(false)
    }

    /// Which of `session_ids` still have a RUNNING child shell?
    /// Distinct from [`Self::has_session`] on purpose — a naturally-exited
    /// session is intentionally RETAINED in the map so its terminal stays
    /// readable, so membership is NOT liveness. An exited child, a missing
    /// session, a poisoned lock, or a status-probe error all read dead.
    ///
    /// Batched under ONE `sessions` lock: the enumerate path probes every
    /// registry session in a single pass rather than re-locking the map — which
    /// is also the live-terminal I/O hot path — once per session.
    pub fn alive_session_ids(&self, session_ids: &[&str]) -> HashSet<String> {
        let Ok(sessions) = self.sessions.lock() else {
            return HashSet::new();
        };
        session_ids
            .iter()
            .filter(|id| sessions.get(**id).is_some_and(|s| s.pty.is_alive()))
            .map(|id| id.to_string())
            .collect()
    }

    /// Emit again the `Exit` of each of `session_ids` whose child has exited
    /// while the session stays retained, for a client that missed the
    /// original: a session that is still running, missing, or not yet past
    /// its batcher's exit is skipped. Returns the ids whose exit went out.
    pub fn redeliver_exits(&self, session_ids: &[&str]) -> HashSet<String> {
        session_ids
            .iter()
            .filter(|id| self.redeliver_exit(id))
            .map(|id| id.to_string())
            .collect()
    }

    /// Emit again the `Exit` of `session_id` if its child is gone and the
    /// session is retained. Emitted under the session's exit slot, after
    /// checking the id still names this session, so it can never land on a
    /// process that took the id next. Lock order: the exit slot, then
    /// `sessions`; the slot is never taken under `sessions`.
    fn redeliver_exit(&self, session_id: &str) -> bool {
        let runtime = {
            let Ok(sessions) = self.sessions.lock() else {
                return false;
            };
            match sessions.get(session_id) {
                Some(session) if !session.pty.is_alive() => session.runtime.clone(),
                _ => return false,
            }
        };
        runtime.redeliver_exit(
            || {
                self.sessions.lock().is_ok_and(|sessions| {
                    sessions
                        .get(session_id)
                        .is_some_and(|session| Arc::ptr_eq(&session.runtime, &runtime))
                })
            },
            |exit| {
                let _ = self.sink.emit(exit);
            },
        )
    }

    /// Something used `session_id` as if it still ran (a write failed, a
    /// viewport attached): when its child is gone, emit its exit again, as
    /// its client may never have heard it. Clients drop an exit they already
    /// have, so this repeats freely. Only a host whose clients can miss an
    /// event does this ([`Self::redeliver_exits_on_use`]).
    fn redeliver_exit_on_use(&self, session_id: &str) {
        if self.redeliver_on_use.load(Ordering::Relaxed) {
            self.redeliver_exit(session_id);
        }
    }

    /// Emit a retained session's exit again whenever it is used as if it
    /// still ran: for a host whose clients can miss events (a session server,
    /// whose connections drop what they fall behind on).
    pub fn redeliver_exits_on_use(&self) {
        self.redeliver_on_use.store(true, Ordering::Relaxed);
    }

    /// Every session whose child is RUNNING — the live-PTY arm of
    /// the union roster. Distinct from [`Self::alive_session_ids`] in two ways that
    /// the union enumerator needs:
    ///
    /// - it ENUMERATES the whole registry (no caller-supplied id list), so it also
    ///   surfaces the registry-less sessions the register-on-open ledger silently
    ///   missed (root workspace / notes / git-sync / `#`-named / any open path that
    ///   skipped `registerNavWorkspaceSession`);
    /// - it SURFACES a poisoned lock as `Err` instead of degrading to an empty set —
    ///   the roster must not silently drop the whole live-PTY arm on a lock failure.
    ///
    /// A naturally-exited session is intentionally RETAINED in the map for reattach
    /// (membership is NOT liveness), so an exited-but-replayed session reads dead and
    /// is EXCLUDED. Unattached warm-pool members are excluded too (a pool shell is
    /// not a roster session; it is renamed to the FE id at handoff). Returns the
    /// emitted `current_id` (post-handoff id), and
    /// drops a session whose id read back empty (a poisoned inner id lock).
    pub fn live_session_ids(&self) -> Result<HashSet<String>, String> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|e| format!("Lock error: {e}"))?;
        Ok(sessions
            .values()
            .filter(|s| !s.runtime.is_pool_member.load(Ordering::Relaxed) && s.pty.is_alive())
            .map(|s| s.runtime.current_id())
            .filter(|id| !id.is_empty())
            .collect())
    }

    /// The running shell PID of each of `session_ids` (in order) — `None` for a
    /// missing session, an exited shell, or a driver without PIDs. One
    /// `sessions`-lock pass; the caller walks process trees OUTSIDE the lock.
    pub fn live_child_pids(&self, session_ids: &[&str]) -> Result<Vec<Option<u32>>, String> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        Ok(session_ids
            .iter()
            .map(|id| sessions.get(*id).and_then(TerminalSession::live_pid))
            .collect())
    }

    /// RF-perf-5: `(total_sessions, dead_retained)` for the gated memory
    /// sampler. `try_lock`-or-skip (returns `None`) so the
    /// sampler never blocks the live-terminal I/O hot path.
    pub fn session_len_and_dead(&self) -> Option<(usize, usize)> {
        let sessions = self.sessions.try_lock().ok()?;
        let dead = sessions.values().filter(|s| !s.pty.is_alive()).count();
        Some((sessions.len(), dead))
    }

    /// How many sessions have a running child, not counting unattached
    /// warm-pool shells: the sessions restarting this host would end.
    pub fn live_session_count(&self) -> usize {
        let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions
            .values()
            .filter(|s| !s.runtime.is_pool_member.load(Ordering::Relaxed) && s.pty.is_alive())
            .count()
    }

    /// RF-perf-17: the stable spawn dirs of every session whose child is
    /// still RUNNING — the reverse-sync "live CLI workspace" set. Uses the
    /// immutable spawn anchor, NOT the OSC-7 live cwd, so a shell that `cd`s away
    /// doesn't drop its worktree. `try_lock`-or-`None` so it never blocks the
    /// live-terminal I/O hot path; a `None` tells the caller to FAIL OPEN.
    pub fn live_session_home_dirs(&self) -> Option<Vec<PathBuf>> {
        let sessions = self.sessions.try_lock().ok()?;
        Some(
            sessions
                .values()
                .filter(|s| s.pty.is_alive())
                .filter_map(|s| s.home_cwd.clone())
                .collect(),
        )
    }

    /// Whether `session_id`'s output has ended — its child's stream closed, or
    /// the session is gone — so a viewport attached now receives no more.
    /// The byte watermark of `session_id`'s output so far; `None` when gone.
    pub fn output_seq(&self, session_id: &str) -> Option<u64> {
        let shared = self.sessions.lock().ok()?.get(session_id)?.shared.clone();
        let seq = shared.lock().ok()?.seq;
        Some(seq)
    }

    pub fn output_ended(&self, session_id: &str) -> bool {
        self.sessions.lock().map_or(true, |sessions| {
            sessions
                .get(session_id)
                .is_none_or(|s| s.runtime.output_ended())
        })
    }

    /// The working directory one session last reported via OSC-7.
    /// `None` if the session is gone or never reported a cwd (shell integration
    /// off). Clones the cwd handle out from under the sessions-map lock, then
    /// reads it — so cwd reads never contend with spawn/kill.
    pub fn session_cwd(&self, session_id: &str) -> Option<PathBuf> {
        let cwd_arc = {
            let sessions = self.sessions.lock().ok()?;
            sessions.get(session_id).map(|s| s.cwd.clone())
        }?;
        let cwd = cwd_arc.lock().ok()?;
        cwd.clone()
    }

    /// Exit-time complement kill: kill every session that is not
    /// persisting (by emitted id), explicitly including warm-pool members, so
    /// nothing disposable outlives the app. The ONE exit-time reaper of a
    /// host with one client; a session server shared by several apps runs
    /// [`Self::kill_owned_non_persisting`] instead.
    /// Snapshot the ids under the lock, then kill outside it so teardown's own
    /// lock can't deadlock.
    pub fn kill_non_persisting(&self) {
        self.kill_non_persisting_where(|_, _, _| true);
    }

    /// The exit-time complement kill of one client: only the sessions in
    /// `owned` (the ones that client opened, its warm-pool members included)
    /// that are not persisting; with `unowned_pool` (the last client), also
    /// every unhanded warm-pool member (an idle shell its owner refills); and
    /// with `leftovers`, every non-persisting session no viewport has shown
    /// for longer than its grace: a crashed app's leftovers. Any other
    /// session another client opened is never touched.
    pub fn kill_owned_non_persisting(
        &self,
        owned: &HashSet<String>,
        unowned_pool: bool,
        leftovers: Option<LeftoverReap>,
    ) {
        self.kill_non_persisting_where(|id, pool_member, unviewed_since| {
            owned.contains(id)
                || (unowned_pool && pool_member)
                || leftovers.is_some_and(|reap| {
                    reap.now.saturating_duration_since(unviewed_since) > reap.grace
                })
        });
    }

    fn kill_non_persisting_where(&self, reapable: impl Fn(&str, bool, Instant) -> bool) {
        let to_kill: Vec<(String, &'static str)> = {
            let Ok(sessions) = self.sessions.lock() else {
                return;
            };
            sessions
                .values()
                .filter(|s| !session_is_persisting(s))
                .filter_map(|s| {
                    let pool_member = s.runtime.is_pool_member.load(Ordering::Relaxed);
                    let id = s.runtime.current_id();
                    // Non-persisting: no viewport is attached now.
                    let unviewed_since = s
                        .shared
                        .lock()
                        .ok()
                        .and_then(|g| g.unviewed_since)
                        .unwrap_or(s.runtime.created);
                    let reason = if pool_member {
                        "an unhanded warm-pool member"
                    } else {
                        "not persisting: no story id and no viewport"
                    };
                    reapable(&id, pool_member, unviewed_since).then_some((id, reason))
                })
                .collect()
        };
        for (id, reason) in to_kill {
            info!(target: "pty", "exit reap: killing session {id} ({reason})");
            self.teardown(&id, TeardownReason::Kill);
        }
    }

    /// Atomically detach every registry-owned session, including persisting
    /// and warm-pool entries, then synchronously TERM/KILL and reap them.
    /// A successful return is an empty-ownership proof for this registry.
    pub fn drain_for_replacement(&self, deadline: Instant) -> Result<usize, String> {
        let drained = {
            let mut sessions = self
                .sessions
                .lock()
                .map_err(|e| format!("Lock error: {e}"))?;
            sessions
                .drain()
                .map(|(_, session)| session)
                .collect::<Vec<_>>()
        };
        let count = drained.len();
        let errors = Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for session in drained {
                session.runtime.request_exit();
                if let Some((owner, id, incarnation)) = session.runtime.activity.get() {
                    owner.remove(id, incarnation);
                }
                self.prune_subscriber_index(&session);
                let errors = &errors;
                scope.spawn(move || {
                    if let Err(error) = session.pty.drain_for_replacement(deadline) {
                        errors.lock().unwrap_or_else(|e| e.into_inner()).push(error);
                    }
                });
            }
        });
        let errors = errors.into_inner().unwrap_or_else(|e| e.into_inner());
        if !errors.is_empty() {
            return Err(format!("replacement drain failures: {}", errors.join("; ")));
        }
        if self
            .sessions
            .lock()
            .map_err(|e| format!("Lock error: {e}"))?
            .is_empty()
        {
            Ok(count)
        } else {
            Err("terminal ownership was not empty after drain".into())
        }
    }
}

/// The live-registry seam the session runtime drives. Each method is one atomic
/// region over the registry, preserving the registry's lock order (`sessions` →
/// per-session `shared`/`writer` → `stream_index`) and the exact observable
/// error text.
impl TerminalRegistry for PtyState {
    fn session_view(&self, session: &str) -> Result<SessionView, String> {
        // Brief global-map lock: clone the per-session `Arc` + read immutable
        // metadata, then release before any heavy work.
        let sessions = self
            .sessions
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        let s = sessions.get(session).ok_or_else(|| {
            format!(
                "subscribe_terminal: {}: {}",
                crate::domain::SESSION_NOT_FOUND,
                session
            )
        })?;
        Ok(SessionView {
            shared: s.shared.clone(),
            runtime: s.runtime.clone(),
            story_id: s.story_id.clone(),
            workspace_id: s.workspace_id.clone(),
            epic_id: s.epic_id.clone(),
        })
    }

    fn shared_handle(&self, session: &str) -> Result<Arc<Mutex<SharedTermState>>, String> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        // A missing session is a hard error — parity with `write`: a
        // submit into a gone session must not look like success.
        sessions
            .get(session)
            .map(|s| s.shared.clone())
            .ok_or_else(|| {
                warn!(target: "pty", "submit_pty_line: session not found: {}", session);
                format!("submit_pty_line: session not found: {}", session)
            })
    }

    fn attach_viewport(
        &self,
        session: &str,
        shared: &Arc<Mutex<SharedTermState>>,
    ) -> Result<AttachOutcome, String> {
        // Per-session lock: register the new stream and enqueue the emulator's
        // snapshot atomically vs the batcher. The worker answers after
        // everything enqueued before it, so the snapshot holds exactly the
        // output and resizes emitted before the registration, and the stream
        // exactly those after (no gap, no duplicate).
        let stream_id = StreamId::next();
        let pending = {
            let mut g = shared.lock().map_err(|e| format!("Lock error: {}", e))?;
            g.subscribers.insert(stream_id, SubscriberHandle::default());
            #[cfg(test)]
            if g.snapshot_after_unlock {
                drop(g);
                std::thread::sleep(std::time::Duration::from_millis(50));
                g = shared.lock().map_err(|e| format!("Lock error: {}", e))?;
            }
            let seq = g.seq;
            g.emulator
                .as_mut()
                .ok_or_else(|| {
                    format!("subscribe_terminal: session {session} has no emulator terminal")
                })
                .and_then(|emulator| {
                    emulator.snapshot(seq).ok_or_else(|| {
                        "subscribe_terminal: no emulator worker is running".to_string()
                    })
                })
        };
        let undo = |shared: &Arc<Mutex<SharedTermState>>| {
            if let Ok(mut index) = self.stream_index.lock() {
                index.remove(&stream_id);
            }
            if let Ok(mut g) = shared.lock() {
                g.detach(&stream_id);
            }
        };
        let pending = match pending {
            Ok(pending) => pending,
            Err(error) => {
                undo(shared);
                return Err(error);
            }
        };

        // Index for O(1) stream-id-only unsubscribe. Not part of the byte-atomic
        // path above, so it is fine to take this lock separately.
        self.stream_index
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?
            .insert(stream_id, shared.clone());

        // Close the subscribe-vs-kill/exit race: if the session was removed (or
        // replaced by a same-id re-spawn) between the registration and this
        // index insert, undo so we never leave a dangling index entry / orphan
        // subscriber pointing at a dead session.
        let still_current = {
            let sessions = self
                .sessions
                .lock()
                .map_err(|e| format!("Lock error: {}", e))?;
            sessions
                .get(session)
                .map(|s| Arc::ptr_eq(&s.shared, shared))
                .unwrap_or(false)
        };
        if !still_current {
            undo(shared);
            return Err(format!(
                "subscribe_terminal: session {} is no longer live",
                session
            ));
        }

        match pending.snapshot(SUBSCRIBE_SNAPSHOT_TIMEOUT) {
            Ok(snapshot) => {
                // A viewport of an exited session: make sure its client heard
                // the exit, so the tab does not read as still running.
                self.redeliver_exit_on_use(session);
                Ok(AttachOutcome {
                    snapshot,
                    stream_id,
                })
            }
            Err(error) => {
                undo(shared);
                Err(format!("subscribe_terminal: {error}"))
            }
        }
    }

    fn resize(&self, session: &str, cols: u16, rows: u16) -> Result<(), String> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        if let Some(s) = sessions.get(session) {
            // The PTY resize and the emulator's are made under the per-session
            // lock, so no batch lands between the two: the worker resizes at
            // exactly the byte offset the PTY did.
            let mut shared = s.shared.lock().ok();
            s.pty
                .resize(cols, rows)
                .map_err(|e| format!("Resize error: {}", e))?;
            if let Some(g) = shared.as_mut() {
                let seq = g.seq;
                let resize_gen = g.emulator.as_mut().and_then(|emulator| {
                    emulator
                        .resize((cols, rows), seq)
                        .then(|| emulator.resize_gen())
                });
                if let Some(resize_gen) = resize_gen {
                    // In the stream, in order with the output: after the
                    // batches written at the old size, before any at the new.
                    for stream in g.subscribers.keys() {
                        let _ = self.sink.emit(TerminalDomainEvent::StreamResized {
                            stream: *stream,
                            at_seq: seq,
                            resize_gen,
                            cols,
                            rows,
                        });
                    }
                }
                #[cfg(debug_assertions)]
                g.record_resize(cols, rows);
            }
        } else {
            warn!(target: "pty", "resize_pty: session not found: {}", session);
        }
        Ok(())
    }

    fn write(&self, session: &str, bytes: &[u8]) -> Result<(), String> {
        // Trace every write here (not at the command edge) so the submit protocol's
        // per-line/nudge writes are logged identically to a direct `write_pty`.
        trace!(target: "pty", "write_pty: session_id={}", session);
        // Hold the map-wide `sessions` lock ONLY to clone out this
        // session's own writer handle, then release it before `write_all`+`flush`
        // so terminal A's I/O never blocks terminal B's keystroke on the map lock.
        let writer = {
            let sessions = self
                .sessions
                .lock()
                .map_err(|e| format!("Lock error: {}", e))?;
            match sessions.get(session) {
                Some(s) => s.pty.writer(),
                None => {
                    // An unknown session id is NOT a silent success —
                    // surface it so the FE can tell "not ready yet" from "gone".
                    warn!(target: "pty", "write_pty: session not found: {}", session);
                    return Err(format!("write_pty: session not found: {}", session));
                }
            }
        };
        let mut writer = writer.lock().map_err(|e| format!("Lock error: {}", e))?;
        let written = writer
            .write_all(bytes)
            .map_err(|e| {
                error!(target: "pty", "write_pty: write failed for session {}: {}", session, e);
                format!("Write error: {}", e)
            })
            .and_then(|()| {
                writer.flush().map_err(|e| {
                    error!(target: "pty", "write_pty: flush failed for session {}: {}", session, e);
                    format!("Flush error: {}", e)
                })
            });
        drop(writer);
        if written.is_err() {
            // A write into a child that is gone: its exit may never have
            // reached the client, which still treats the session as running.
            self.redeliver_exit_on_use(session);
        }
        written
    }

    fn is_idle(&self, session: &str) -> Result<bool, String> {
        // Read the running shell's pid under the lock; run the process-tree
        // enumeration OUTSIDE the lock so it never stalls other PTY commands. No
        // session, an exited shell, or no pid: nothing is running → idle.
        let pid = self.live_child_pids(&[session])?[0];
        Ok(pid.is_none_or(|pid| !has_live_descendant(pid)))
    }

    fn kill(&self, session: &str) {
        self.teardown(session, TeardownReason::Kill);
    }
}

/// One session as a status read sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFacts {
    pub session: PtySessionId,
    /// Its process still runs.
    pub running: bool,
    /// An unhanded warm-pool shell.
    pub pool_member: bool,
    /// A viewport shows it.
    pub viewed: bool,
    /// It is an entry of the layout (a tab or a background entry, or one
    /// being opened).
    pub layout_entry: bool,
    /// When it started, in ms since the Unix epoch.
    pub started_at_ms: u64,
    /// Its running shell's pid.
    pub pid: Option<u32>,
}

/// The leftover part of the last client's exit reap: at `now`, a
/// non-persisting session no viewport has shown for longer than `grace` goes.
#[derive(Debug, Clone, Copy)]
pub struct LeftoverReap {
    pub now: Instant,
    pub grace: Duration,
}

/// The persistence classifier rule, pure so it is unit-testable without a live PTY:
/// a session is "persisting" — spared by the exit-time complement kill — iff it
/// is not a warm-pool member AND (it has a story id OR a live viewport).
pub(crate) fn is_persisting(
    is_pool_member: bool,
    has_story_id: bool,
    has_subscribers: bool,
) -> bool {
    !is_pool_member && (has_story_id || has_subscribers)
}

/// Apply [`is_persisting`] to a live session. Used ONLY off the FE path (the
/// exit-time complement kill).
fn session_is_persisting(session: &TerminalSession) -> bool {
    let has_subscribers = session
        .shared
        .lock()
        .map(|g| !g.subscribers.is_empty())
        .unwrap_or(false);
    is_persisting(
        session.runtime.is_pool_member.load(Ordering::Relaxed),
        session.story_id.is_some(),
        has_subscribers,
    )
}

/// RF-perf-6: pick which retained-dead sessions to evict so the dead set
/// stays bounded at `cap`. Pure so the cap + ordering + exclusion rules are
/// unit-tested without a real PTY. One input tuple per session:
/// `(id, alive, has_subscribers, exit_seq)`:
/// - `alive` — the child-status probe. A LIVE session is never a candidate.
/// - `has_subscribers` — whether a viewport is currently attached. A dead session
///   that is still being VIEWED (an open scrollback tab kept after natural exit)
///   is never evicted — that would yank the buffer out from under the viewport and
///   break reattach. Mirrors the `is_persisting` protection.
/// - `exit_seq` — the death-order stamp (`None` = alive OR dead-but-not-yet-
///   stamped). A dead-but-unstamped session is retained this pass (its output
///   may still be finalizing) and reconsidered next pass.
///
/// Among the eviction candidates (dead, unviewed, stamped) retain the `cap`
/// most-recently-dead (highest `exit_seq`) and return the rest, OLDEST-death-first,
/// as victims.
pub(crate) fn dead_sessions_to_evict(
    sessions: &[(String, bool, bool, Option<u64>)],
    cap: usize,
) -> Vec<String> {
    let mut candidates: Vec<(&str, u64)> = sessions
        .iter()
        .filter_map(
            |(id, alive, has_subscribers, seq)| match (alive, has_subscribers, seq) {
                // Dead, not currently viewed, and death-stamped → an eviction candidate.
                (false, false, Some(s)) => Some((id.as_str(), *s)),
                _ => None,
            },
        )
        .collect();
    if candidates.len() <= cap {
        return Vec::new();
    }
    candidates.sort_by_key(|(_, seq)| *seq); // oldest death first
    let evict_count = candidates.len() - cap;
    candidates
        .into_iter()
        .take(evict_count)
        .map(|(id, _)| id.to_string())
        .collect()
}

/// The reader thread body: a blocking read loop that sends raw byte chunks
/// through the channel, then `None` at end-of-stream.
fn read_pty(mut reader: Box<dyn Read + Send>, tx: mpsc::Sender<Option<Vec<u8>>>) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                let _ = tx.send(None);
                break;
            }
            Ok(n) => {
                if tx.send(Some(buf[..n].to_vec())).is_err() {
                    break;
                }
            }
            Err(_) => {
                let _ = tx.send(None);
                break;
            }
        }
    }
}

/// The batcher thread body: coalesce the reader's chunks, run the OSC side-tap,
/// count the bytes, deliver through the warm-handoff gate, and — when
/// the stream ends — report the child's exit.
fn run_session_batcher(
    sink: &dyn TerminalEventSink,
    rx: mpsc::Receiver<Option<Vec<u8>>>,
    shared: &Mutex<SharedTermState>,
    runtime: &SessionRuntime,
    cwd: &Mutex<Option<PathBuf>>,
    osc_enabled: bool,
    child: &ChildHandle,
) {
    // Side-tap OSC parser. Constructed only when shell integration is on,
    // so there is zero per-byte overhead when it is off.
    let mut osc = osc_enabled.then(OscParser::default);
    let mut osc_events: Vec<OscEventAt> = Vec::new();
    run_pty_batcher(
        rx,
        |data| {
            // The live emit id can change at warm-pool handoff (rename), so
            // read it from the shared runtime every batch.
            let sid = runtime.current_id();
            if let Some((owner, session, incarnation)) = runtime.activity.get() {
                owner.output(session, incarnation);
            }

            // OSC side-tap + gate offset: capture cwd (OSC 7), dispatch
            // prompt markers (OSC 133), and note where this batch's first
            // prompt-start begins (byte offset of its ESC).
            let mut prompt_start_offset: Option<usize> = None;
            if let Some(parser) = osc.as_mut() {
                osc_events.clear();
                parser.feed(data.as_bytes(), &mut osc_events);
                for ev in &osc_events {
                    match &ev.event {
                        OscEvent::Cwd(path) => {
                            // Dedupe path-equal OSC 7 writes so a
                            // prompt that re-emits the same cwd never bumps
                            // the frontend revision counter. One event per
                            // real `cd`, no churn. Taking the lock inline
                            // keeps the write + the change check atomic
                            // against a reader on another thread. An empty
                            // path (malformed OSC 7 `;file://;`) can't name
                            // a directory, so it's dropped before dedupe.
                            if path.as_os_str().is_empty() {
                                continue;
                            }
                            let changed = match cwd.lock() {
                                Ok(mut guard) => {
                                    let equal = guard.as_deref() == Some(path.as_path());
                                    if !equal {
                                        *guard = Some(path.clone());
                                    }
                                    !equal
                                }
                                Err(_) => false,
                            };
                            if changed {
                                let _ = sink.emit(TerminalDomainEvent::CwdChanged {
                                    session: PtySessionId::new(sid.as_str()),
                                    cwd: path.clone(),
                                });
                            }
                        }
                        marker => {
                            if let OscEvent::CommandEnd(code) = marker {
                                if let Some((owner, session, incarnation)) = runtime.activity.get()
                                {
                                    owner.shell_command_end(session, incarnation, *code);
                                }
                            }
                            if matches!(marker, OscEvent::PromptStart)
                                && prompt_start_offset.is_none()
                            {
                                prompt_start_offset = Some(ev.offset);
                            }
                            let _ = sink.emit(TerminalDomainEvent::PromptMarker {
                                session: PtySessionId::new(sid.as_str()),
                                marker: marker.clone(),
                            });
                        }
                    }
                }
            }

            // Output gate (warm-pool handoff). The append, the gate's
            // open transition, and the delivery all happen under the per-session
            // lock so the gate is atomic against the timeout-fallback guard —
            // whichever takes the lock first opens the gate; the other sees it
            // already open and never re-delivers the same bytes. While
            // suppressing, deliver nothing until the post-cd prompt-start, then
            // from that marker onward — byte-exact, no cd echo. The byte
            // counter always advances by the full batch.
            if let Ok(mut g) = shared.lock() {
                g.seq += data.len() as u64;
                // The absolute seq at the END of this batch. The gate only ever
                // trims the FRONT of `data`, so the delivered slice always ends
                // here.
                let seq = g.seq;
                let deliver: Option<&str> =
                    if !runtime.suppress_until_prompt.load(Ordering::Relaxed) {
                        Some(data)
                    } else if let Some(slice) = gated_slice(prompt_start_offset, data) {
                        runtime
                            .suppress_until_prompt
                            .store(false, Ordering::Relaxed);
                        g.held.clear();
                        Some(slice)
                    } else {
                        g.held.push_str(data);
                        None
                    };
                if let Some(out) = deliver {
                    #[cfg(debug_assertions)]
                    g.record_output(seq, out);
                    deliver_output(sink, &mut g.subscribers, &sid, out, seq);
                }
                // The emulator gets exactly what the displays get, in the
                // same order: a suppressed warm-pool echo is suppressed there
                // too.
                #[cfg(test)]
                let deliver = if runtime.feed_emulator_full_batch.load(Ordering::Relaxed) {
                    Some(data)
                } else {
                    deliver
                };
                if let (Some(out), Some(emulator)) = (deliver, g.emulator.as_mut()) {
                    emulator.feed(seq, out);
                }
            }
        },
        PTY_BATCH_WINDOW,
        PTY_IDLE_TIMEOUT,
        PTY_MAX_BATCH_BYTES,
    );
    // The stream ended: the child exited. A pool member's exit is the pool's to
    // reclaim + refill (dead-member self-heal); a handed-off member has
    // `is_pool_member=false` and follows the normal retain path.
    let exited_id = runtime.current_id();
    let pool_member = runtime.is_pool_member.load(Ordering::Relaxed);
    runtime.end_output();
    let status = child.exit_info_after_eof();
    if let Some((owner, id, incarnation)) = runtime.activity.get() {
        owner.exit(
            id,
            incarnation,
            crate::activity::Exit {
                code: status.as_ref().and_then(|status| status.exit_code),
                signal: status.as_ref().and_then(|status| status.signal.clone()),
                requested: runtime.exit_requested(),
            },
        );
    }
    let exit = TerminalDomainEvent::Exit {
        session: PtySessionId::new(exited_id),
        status,
        pool_member,
        requested: runtime.exit_requested(),
        incarnation: runtime.incarnation(),
    };
    if !pool_member {
        // Kept with the retained session: a client that never received this
        // exit (its connection fell behind or was away) can be told again.
        runtime.record_exit(exit.clone());
    }
    let _ = sink.emit(exit);
    if !pool_member {
        // RF-perf-6: stamp this retained session's death order so the
        // dead-session cap can retain the N most-recently-dead and evict the
        // older tail. Lock-free — the seam never re-locks the session map.
        runtime.mark_dead();
    }
    // The session is intentionally RETAINED after natural exit (non-pool): the
    // terminal must stay readable so a viewport can still attach and see the
    // final scrollback (lossless across detach). A dead session reads
    // idle. Reclamation is owned by an explicit kill (tab close) and the
    // dead-session cap. Pool members are reclaimed by the pool.
}

/// Deliver one chunk: fan it out to every attached viewport on its own stream
/// (tracking per-subscriber telemetry), then on the session-wide output channel.
/// The single delivery site shared by the batcher hot path and the warm-pool
/// timeout-fallback flush. The caller holds the per-session lock.
pub(crate) fn deliver_output(
    sink: &dyn TerminalEventSink,
    subscribers: &mut HashMap<StreamId, SubscriberHandle>,
    session: &str,
    data: &str,
    // The absolute stream position at the end of this chunk.
    seq: u64,
) {
    let n = data.len() as u64;
    for (stream, handle) in subscribers.iter_mut() {
        match sink.emit(TerminalDomainEvent::StreamOutput {
            stream: *stream,
            data: data.to_string(),
            seq,
        }) {
            Ok(()) => handle.bytes_sent += n,
            Err(_) => handle.emit_failures += 1,
        }
    }
    let _ = sink.emit(TerminalDomainEvent::Output {
        session: PtySessionId::new(session),
        data: data.to_string(),
        seq,
    });
}

/// While the warm-handoff gate is armed, the slice to deliver is
/// everything from the post-cd prompt-start marker onward — `None` until that
/// marker arrives in a batch. Pure so the byte-exact "no cd echo" cut is
/// unit-tested. `offset` is the ESC introducer index (ASCII → a char boundary).
pub(crate) fn gated_slice(prompt_offset: Option<usize>, data: &str) -> Option<&str> {
    // A parser regression (an offset past the end or off a char boundary)
    // delivers nothing rather than panicking the batcher hot path.
    prompt_offset.map(|off| data.get(off..).unwrap_or(""))
}
