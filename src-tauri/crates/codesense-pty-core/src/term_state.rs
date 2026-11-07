// SPDX-License-Identifier: MIT
//! The per-session substrate shared between the registry and the batcher: the
//! output byte counter, the viewport-subscriber registry and the session's
//! emulator terminal, plus the live runtime control a warm-pool handoff flips.
//!
//! A viewport subscribes on mount (the emulator's snapshot + the live stream)
//! and unsubscribes on unmount; the PTY keeps running regardless.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

/// Process-unique, monotonically increasing subscription identifier. A plain
/// `AtomicU64` counter is sufficient: PTY scope is small and a
/// counter cannot collide or be reused within a process lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct StreamId(pub u64);

impl StreamId {
    /// Allocate the next never-before-used id.
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        StreamId(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// Live, mutable per-session control shared (`Arc<SessionRuntime>`)
/// between the registry's session record and its long-lived batcher thread. The
/// batcher captures this once at spawn, so any of these that change at
/// warm-pool handoff — the session id (rename), the pool-member flag (clear),
/// the output gate (arm) — are seen by the already-running batcher without
/// rebuilding it.
pub struct SessionRuntime {
    pub(crate) activity: std::sync::OnceLock<(
        std::sync::Arc<crate::activity::ActivityOwner>,
        String,
        String,
    )>,
    /// The session id the batcher stamps on its output / exit / prompt-marker
    /// events. Mutable so a popped pool member can be renamed to the frontend's
    /// session id at handoff.
    pub session_id: Mutex<String>,
    /// `true` while this session is an unattached warm-pool member. Read by the
    /// batcher's natural-exit seam (dead-member self-heal) and by idle eviction
    /// (exempt pool members). Cleared at handoff.
    pub is_pool_member: AtomicBool,
    /// `true` while the live output stream is gated until the next OSC 133
    /// prompt-start (warm handoff). Lock-free read on the batcher hot path.
    pub suppress_until_prompt: AtomicBool,
    /// RF-perf-6: death-order stamp. `0` = not yet dead (alive, or dead
    /// but not yet stamped by the batcher); a positive value is the monotonic
    /// order in which the batcher observed this session's child exit. Read by the
    /// dead-session cap to retain the N most-recently-dead and evict the older
    /// tail. Stamped once, lock-free, so the batcher's exit seam never has to
    /// re-lock the global session map to record death order.
    exit_seq: AtomicU64,
    /// The child's output stream ended: set just before its `Exit` is emitted,
    /// so a viewport bound afterwards knows no more output will come.
    output_ended: AtomicBool,
    /// The host itself ended this session (a teardown killed its child), so
    /// the exit that follows was asked for, not a crash. Set before the kill.
    exit_requested: AtomicBool,
    /// The `Exit` the batcher emitted for this session (never a pool
    /// member's), kept while the session is retained so a client that never
    /// received it can be told again. Its lock orders a redelivery against
    /// the session leaving the registry.
    exit: Mutex<ExitSlot>,
    /// When the session was created: how long it has gone unviewed when no
    /// viewport ever attached.
    pub(crate) created: std::time::Instant,
    /// When the session was created, on the wall clock (for people).
    pub(crate) started_at: std::time::SystemTime,
    /// This process's incarnation: unique per spawn in this host process, and
    /// stamped on its `Exit`.
    incarnation: u64,
    /// Negative control for the tests: feed the emulator the whole batch
    /// instead of the slice the displays get.
    #[cfg(test)]
    pub(crate) feed_emulator_full_batch: AtomicBool,
}

/// The next process incarnation; `0` is reserved for "unknown".
static NEXT_INCARNATION: AtomicU64 = AtomicU64::new(1);

/// A session's recorded `Exit`, and whether the session left the registry.
#[derive(Default)]
pub(crate) struct ExitSlot {
    exit: Option<crate::domain::TerminalDomainEvent>,
    retired: bool,
}

/// RF-perf-6: process-global monotonic death counter. `0` is reserved to
/// mean "unstamped" on [`SessionRuntime::exit_seq`], so the first real stamp is
/// `1`. Only the batcher's natural-exit seam advances it (once per session
/// death), so contention is nil.
static DEAD_SESSION_SEQ: AtomicU64 = AtomicU64::new(1);

impl SessionRuntime {
    pub fn new(session_id: String, is_pool_member: bool) -> Self {
        Self {
            activity: std::sync::OnceLock::new(),
            session_id: Mutex::new(session_id),
            is_pool_member: AtomicBool::new(is_pool_member),
            suppress_until_prompt: AtomicBool::new(false),
            exit_seq: AtomicU64::new(0),
            output_ended: AtomicBool::new(false),
            exit_requested: AtomicBool::new(false),
            exit: Mutex::new(ExitSlot::default()),
            created: std::time::Instant::now(),
            started_at: std::time::SystemTime::now(),
            incarnation: NEXT_INCARNATION.fetch_add(1, Ordering::Relaxed),
            #[cfg(test)]
            feed_emulator_full_batch: AtomicBool::new(false),
        }
    }

    /// Mark the coming exit as requested by the host. Call before killing the
    /// child, so the reader's `Exit` sees it.
    pub fn request_exit(&self) {
        self.exit_requested.store(true, Ordering::SeqCst);
    }

    /// Whether the host asked for this session's exit.
    pub fn exit_requested(&self) -> bool {
        self.exit_requested.load(Ordering::SeqCst)
    }

    /// The child's output stream ended (its `Exit` is emitted or on its way).
    pub fn output_ended(&self) -> bool {
        self.output_ended.load(Ordering::SeqCst)
    }

    /// Record that the child's output stream ended.
    pub fn end_output(&self) {
        self.output_ended.store(true, Ordering::SeqCst);
    }

    /// This process's incarnation, stamped on its `Exit`.
    pub fn incarnation(&self) -> u64 {
        self.incarnation
    }

    /// Keep the `Exit` the batcher emitted, for delivering it again.
    pub(crate) fn record_exit(&self, exit: crate::domain::TerminalDomainEvent) {
        self.exit.lock().unwrap_or_else(|e| e.into_inner()).exit = Some(exit);
    }

    /// Hand the recorded `Exit` to `emit` while this session still holds its
    /// id (`current` is asked under the slot's lock), so it is emitted before
    /// the session's removal is announced, or not at all: a redelivered exit
    /// never lands on a process that took the id next.
    pub(crate) fn redeliver_exit(
        &self,
        current: impl FnOnce() -> bool,
        emit: impl FnOnce(crate::domain::TerminalDomainEvent),
    ) -> bool {
        let slot = self.exit.lock().unwrap_or_else(|e| e.into_inner());
        match &slot.exit {
            Some(exit) if !slot.retired && current() => {
                emit(exit.clone());
                true
            }
            _ => false,
        }
    }

    /// The session leaves the registry: its exit is never delivered again.
    /// Waits out a redelivery under way, so call it before announcing the
    /// removal.
    pub(crate) fn retire_exit(&self) {
        self.exit.lock().unwrap_or_else(|e| e.into_inner()).retired = true;
    }

    /// The session id to stamp on emitted events right now (clone under lock).
    pub fn current_id(&self) -> String {
        self.session_id
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// RF-perf-6: record this session's death order the first time its
    /// child exits (idempotent — a later call keeps the original stamp). Called
    /// by the batcher's natural-exit seam; lock-free so death-order recording
    /// never touches the global session map.
    pub fn mark_dead(&self) {
        if self.exit_seq.load(Ordering::Relaxed) != 0 {
            return;
        }
        let seq = DEAD_SESSION_SEQ.fetch_add(1, Ordering::Relaxed);
        // Only the first stamp wins; a lost CAS means another caller already
        // stamped it, and we simply drop this (harmless) unused seq.
        let _ = self
            .exit_seq
            .compare_exchange(0, seq, Ordering::Relaxed, Ordering::Relaxed);
    }

    /// RF-perf-6: this session's death-order stamp, or `None` while it is
    /// alive or dead-but-not-yet-stamped. The cap treats `None` as "not an
    /// eviction candidate yet" so a freshly-dead session whose output may still
    /// be finalizing is retained until the next pass.
    pub fn exit_seq(&self) -> Option<u64> {
        match self.exit_seq.load(Ordering::Relaxed) {
            0 => None,
            n => Some(n),
        }
    }
}

/// Per-subscriber telemetry counters for observability when a viewport
/// lags or has gone away. The live byte transport is the host's per-stream
/// event, keyed by the subscriber's [`StreamId`]; the host owns its name.
#[derive(Debug, Default)]
pub struct SubscriberHandle {
    pub bytes_sent: u64,
    pub emit_failures: u64,
}

/// The per-session shared state, guarded by its own `Mutex`. The batcher and
/// `subscribe`/`unsubscribe` contend only here — never on the global session
/// map — so an attach cannot stall another terminal's batcher or block a
/// UI-driven write. The same lock makes
/// snapshot-then-register atomic against the batcher's count-then-fan-out, so a
/// new subscriber sees exactly the bytes after its snapshot point — no gap, no
/// duplicate.
#[derive(Default)]
pub struct SharedTermState {
    /// Total bytes the session's PTY produced — every batch counted in full,
    /// even when the warm-pool gate delivers only a suffix of it or nothing.
    /// The absolute stream position `Output.seq_end` and snapshots are
    /// stamped with.
    pub seq: u64,
    /// The output the warm-handoff gate held back since it was armed, flushed
    /// if the prompt it waits for never comes; dropped when it does.
    pub(crate) held: String,
    pub subscribers: HashMap<StreamId, SubscriberHandle>,
    /// When the session's last viewport detached; `None` while one is
    /// attached or before any ever was (then its spawn counts).
    pub(crate) unviewed_since: Option<std::time::Instant>,
    /// The session's terminal in the host's emulator worker. Fed and resized
    /// under this lock, at the same ordering point as the subscribers, so the
    /// worker applies exactly the stream the displays get. `None` once the
    /// session is torn down (or while its terminal is carried to a respawn).
    pub(crate) emulator: Option<crate::emulator::SessionEmulator>,
    /// The dev-only stream recording, written under this lock at the ordering
    /// point of every delivery and resize.
    #[cfg(debug_assertions)]
    pub(crate) recorder: Option<crate::recorder::PtyRecorder>,
    /// Negative control for the tests: ask for an attach's snapshot after
    /// releasing this lock instead of with the registration.
    #[cfg(test)]
    pub(crate) snapshot_after_unlock: bool,
}

impl SharedTermState {
    /// Detach viewport `stream`; the last one to go starts the session's
    /// unviewed time.
    pub(crate) fn detach(&mut self, stream: &StreamId) {
        if self.subscribers.remove(stream).is_some() && self.subscribers.is_empty() {
            self.unviewed_since = Some(std::time::Instant::now());
        }
    }
}

impl SharedTermState {
    /// Record a delivery that ends the stream at `seq_end`.
    #[cfg(debug_assertions)]
    pub(crate) fn record_output(&mut self, seq_end: u64, data: &str) {
        if let Some(recorder) = self.recorder.as_mut() {
            recorder.output(seq_end, data.as_bytes());
        }
    }

    /// Record a resize applied at the stream's current end.
    #[cfg(debug_assertions)]
    pub(crate) fn record_resize(&mut self, cols: u16, rows: u16) {
        let at_seq = self.seq;
        if let Some(recorder) = self.recorder.as_mut() {
            recorder.resize(at_seq, cols, rows);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_runtime_mark_dead_is_monotonic_and_idempotent() {
        let a = SessionRuntime::new("a".to_string(), false);
        assert_eq!(a.exit_seq(), None, "alive session has no death stamp");
        a.mark_dead();
        let first = a.exit_seq().expect("stamped after mark_dead");
        a.mark_dead();
        assert_eq!(a.exit_seq(), Some(first), "second mark_dead is idempotent");

        let b = SessionRuntime::new("b".to_string(), false);
        b.mark_dead();
        let second = b.exit_seq().expect("stamped");
        assert!(
            second > first,
            "later death gets a strictly greater stamp ({second} > {first})"
        );
    }

    #[test]
    fn session_runtime_id_and_flags_are_live() {
        let rt = SessionRuntime::new("pool-1".into(), true);
        assert_eq!(rt.current_id(), "pool-1");
        assert!(rt.is_pool_member.load(Ordering::Relaxed));
        assert!(!rt.suppress_until_prompt.load(Ordering::Relaxed));

        // Handoff: rename + clear pool flag + arm gate.
        *rt.session_id.lock().unwrap() = "fe-42".into();
        rt.is_pool_member.store(false, Ordering::Relaxed);
        rt.suppress_until_prompt.store(true, Ordering::Relaxed);
        assert_eq!(rt.current_id(), "fe-42");
        assert!(!rt.is_pool_member.load(Ordering::Relaxed));
        assert!(rt.suppress_until_prompt.load(Ordering::Relaxed));
    }

    #[test]
    fn stream_ids_are_unique_and_monotonic() {
        let a = StreamId::next();
        let b = StreamId::next();
        let c = StreamId::next();
        assert!(a.0 < b.0 && b.0 < c.0);
        assert_ne!(a, b);
    }

    #[test]
    fn subscribers_register_and_deregister() {
        let mut s = SharedTermState::default();
        let id1 = StreamId::next();
        let id2 = StreamId::next();
        s.subscribers.insert(id1, SubscriberHandle::default());
        s.subscribers.insert(id2, SubscriberHandle::default());
        assert_eq!(s.subscribers[&id1].bytes_sent, 0);
        assert_eq!(s.subscribers.len(), 2);
        s.subscribers.remove(&id1);
        assert_eq!(s.subscribers.len(), 1);
        assert!(s.subscribers.contains_key(&id2));
    }
}
