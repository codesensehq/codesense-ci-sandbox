// SPDX-License-Identifier: MIT
//! Per-directory event dispatch with bounded debounce + suppression.
//!
//! The `Dispatcher` is the seam between the watcher (which knows only how
//! to translate raw `notify` events into "this path changed" signals) and
//! the consumers that care about specific directories. It owns:
//!
//! - the **handler map** keyed by directory — consumers register a
//!   [`DispatchHandler`] for the dir they care about;
//! - the **debounce map** keyed by file path — coalesces a burst to one
//!   leading dispatch plus, when anything was dropped inside the window, one
//!   trailing dispatch once `debounce_window_ms` has passed (so the burst's
//!   final state always reaches the handler);
//! - the **suppression set** — a *timestamped* path-set the watcher
//!   consults to drop codesense-driven writes, exposed to callers via
//!   [`SuppressionHandle`] and wrapped by the RAII [`super::Suppression`]
//!   guard.
//!
//! Eviction strategy for the debounce map is threshold-triggered
//! age-based, mirroring R-2's `should_emit` pattern in
//! `commands/watch.rs:334-346`: when the map exceeds `max_entries`,
//! entries older than `max_age_ms` are pruned.
//!
//! Suppression entries are kept past their guard's `Drop` for an extra
//! `suppression_grace_ms` window so kernel-delivered events that arrive
//! *after* a codesense-driven write completes still see the path as
//! suppressed. Without that grace the dispatcher would have a
//! fundamental race against FSEvents/inotify delivery latency (typically
//! 10–200 ms) — every "suppressed" write would still fire handlers as
//! soon as the kernel flushed.

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use log::{debug, error, trace, warn};

/// Per-directory event handler invoked on dispatch. Receives the path of
/// the file that changed. Event-kind discrimination (create/modify/delete)
/// is intentionally out of scope for SS.1 — SS.4b extends this signature
/// when hash-skip + delete handling lands.
///
/// Handler panics are caught by the dispatcher and logged at `error!`
/// level so a single misbehaving consumer cannot kill the notify thread.
pub type DispatchHandler = Arc<dyn Fn(&Path) + Send + Sync + 'static>;

/// SS-FU-13 AC-1 (test-only): lightweight tally of dispatch outcomes,
/// snapshotted via [`Dispatcher::counts`]. Each event resolves to exactly
/// one bucket — `suppressed` (dropped as a codesense self-write),
/// `debounced` (coalesced inside the debounce window), `delivered` (a
/// handler fired), or `no_handler` (no consumer registered for the path's
/// ancestor chain). The sum across all four equals total `dispatch` calls plus
/// one per trailing re-dispatch of a debounced path (SW-LN-B50).
/// Used by the cascade harness to quantify how much of a materialization
/// wave's event flood the suppression set actually absorbs vs. leaks.
///
/// Gated to `#[cfg(test)]`: this is discovery/test instrumentation, not
/// production telemetry. Release builds carry no dispatch counters (the
/// increments in `dispatch` compile to nothing). If live suppression
/// telemetry on real machines is ever wanted, that's a deliberate
/// follow-up that wires a consumer — not a silent ride-along here.
#[cfg(test)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DispatchCounts {
    pub delivered: u64,
    pub suppressed: u64,
    pub debounced: u64,
    pub no_handler: u64,
}

/// Internal atomic counters backing [`DispatchCounts`]. Relaxed ordering is
/// sufficient — these are diagnostic tallies, not synchronization state.
#[cfg(test)]
#[derive(Default)]
struct DispatchCounters {
    delivered: AtomicU64,
    suppressed: AtomicU64,
    debounced: AtomicU64,
    no_handler: AtomicU64,
}

/// One suppression-set entry: the wall-clock window (`valid_until`) that the cheap
/// time-gate ([`SuppressionHandle::is_suppressed`]) reads. A codesense self-write
/// extends this window so the FS echo it triggers is dropped before a handler runs.
#[derive(Clone, Copy, Debug)]
struct SuppressedEntry {
    valid_until: Instant,
}

/// Cheap-clone-able handle to a [`Dispatcher`]'s suppression set. Held
/// internally by [`super::Suppression`] for the lifetime of the RAII
/// guard. Construct via [`Dispatcher::suppression_handle`].
///
/// The underlying map is `PathBuf → SuppressedEntry`. `Suppression::new` performs a
/// monotonic `max`-update of `valid_until`: overlapping guards naturally compose
/// because each `new` call extends (never shortens) the existing window, and `Drop`
/// is a no-op (the time-based expiry takes over).
#[derive(Clone)]
pub struct SuppressionHandle {
    set: Arc<Mutex<HashMap<PathBuf, SuppressedEntry>>>,
    grace: Duration,
    max_entries: usize,
}

impl SuppressionHandle {
    /// Extend the suppression window for `paths` — codesense's own writes (materialize
    /// / heal / install / atomic_io) so the FS echo they trigger is dropped by the time
    /// gate before a handler runs. Computes stable keys OUTSIDE the lock — each
    /// `stable_key` is a syscall path, and we never hold the suppression mutex across
    /// kernel boundaries while other callers contend; the monotonic max-update makes
    /// this race-safe (a concurrent insert only pushes the timestamp *forward*, so
    /// overlapping guards compose). Poison-tolerant: a poisoned mutex logs `warn!` and
    /// no-ops (suppression degrades open; the reverse gate then falls back to the store
    /// authority).
    pub(crate) fn insert_paths(&self, paths: &[PathBuf]) {
        let valid_until = Instant::now() + self.grace;
        let keys: Vec<PathBuf> = paths.iter().map(|p| stable_key(p)).collect();
        match self.set.lock() {
            Ok(mut locked) => {
                for k in keys {
                    locked
                        .entry(k)
                        .and_modify(|e| e.valid_until = e.valid_until.max(valid_until))
                        .or_insert(SuppressedEntry { valid_until });
                }
                evict_expired_if_oversized(&mut locked, self.max_entries);
            }
            Err(_) => warn!(
                target: "content_engine::dispatcher",
                "suppression mutex poisoned; insert_paths is a no-op"
            ),
        }
    }

    /// SS-4a: drain the entire suppression set. Called by
    /// `commands/watch.rs::stop_watching` to prevent cross-session bleed —
    /// suppression entries from an in-flight heal/reinstall whose
    /// `valid_until` extends past watcher teardown would otherwise match
    /// paths the next session is watching. Poison-tolerant: a poisoned
    /// mutex logs `warn!` and the operation no-ops.
    pub fn clear(&self) {
        match self.set.lock() {
            Ok(mut locked) => locked.clear(),
            Err(_) => warn!(
                target: "content_engine::dispatcher",
                "suppression mutex poisoned; clear is a no-op"
            ),
        }
    }

    /// SS-4a iter-2 (M3): expose `is_suppressed` to crate-local tests so
    /// they can pin the empty-default invariant. Private outside the
    /// crate — handlers never need to consult suppression directly
    /// (the dispatcher does that internally on `dispatch`).
    pub(crate) fn is_suppressed(&self, path: &Path) -> bool {
        self.is_suppressed_internal(path)
    }

    fn is_suppressed_internal(&self, path: &Path) -> bool {
        // Compute the stable key outside the lock — `stable_key` is a
        // syscall path and we don't want to serialize the watcher
        // through the kernel.
        let key = stable_key(path);
        let now = Instant::now();
        match self.set.lock() {
            Ok(locked) => match locked.get(&key) {
                Some(entry) => now < entry.valid_until,
                None => false,
            },
            Err(_) => {
                // Fail-closed: a poisoned suppression mutex means we
                // can't tell whether codesense itself triggered the
                // event. Treating it as suppressed avoids the worst
                // outcome (handler floods firing on the engine's own
                // writes during heal/install). The poisoning is logged
                // so the degraded state is visible.
                warn!(
                    target: "content_engine::dispatcher",
                    "suppression mutex poisoned; treating event as suppressed (fail-closed)"
                );
                true
            }
        }
    }
}

/// Per-directory event dispatch with bounded debounce + suppression set.
/// Owns all the state that the watcher does not — handler registry,
/// debounce map, suppression set — so the watcher itself can stay
/// focused on translating raw notify events into path signals.
pub struct Dispatcher {
    /// Everything a dispatch touches, shared with the trailing-edge flusher
    /// thread (SW-LN-B50) so a debounced event can be re-dispatched once its
    /// window has passed.
    core: Arc<DispatchCore>,
    /// Explicit directory-scoped rescan consumers, kept separate from ordinary
    /// handlers: forwarding a directory-as-path can enqueue a bogus projection.
    /// Removed alongside the directory's ordinary handler on teardown.
    rescan_handlers: Mutex<HashMap<PathBuf, DispatchHandler>>,
    /// App-wide metadata recovery has no directory subscription: a dropped
    /// event can name an ancestor above a content root, or carry no path at all.
    context_index_rescan: Mutex<Option<Arc<dyn Fn(Option<&Path>) + Send + Sync>>>,
}

/// The dispatch state shared between [`Dispatcher`] and its trailing-edge
/// flusher thread.
struct DispatchCore {
    handlers: Mutex<HashMap<PathBuf, DispatchHandler>>,
    direct_file_filters: Mutex<HashMap<PathBuf, std::ffi::OsString>>,
    debounce: Mutex<HashMap<PathBuf, Instant>>,
    suppression: SuppressionHandle,
    debounce_window: Duration,
    max_entries: usize,
    max_age: Duration,
    /// SW-LN-B50: paths whose event was dropped inside the debounce window, each
    /// with the instant its trailing re-dispatch is due. The window is
    /// leading-edge (the first event fires at once), so without a trailing
    /// re-dispatch the LAST write of a burst — the one that leaves the file
    /// complete — could be dropped for good, and a consumer that read the file on
    /// the first event would keep a half-written view of it until the file
    /// changed again. One entry per path; `flusher_running` marks the single
    /// flusher thread that drains it.
    trailing: Mutex<TrailingQueue>,
    #[cfg(test)]
    counts: DispatchCounters,
}

#[derive(Default)]
struct TrailingQueue {
    due: HashMap<PathBuf, Instant>,
    flusher_running: bool,
}

impl Dispatcher {
    /// Build a dispatcher.
    ///
    /// * `debounce_window_ms` — coalesce window per file path; further
    ///   events for the same path within this window collapse into one
    ///   trailing dispatch when the window ends.
    /// * `max_entries` — soft cap on each internal map (debounce +
    ///   suppression). When exceeded, expired entries are pruned.
    /// * `max_age_ms` — debounce-map entry max age for eviction. Must
    ///   be greater than zero; a zero value would evict every
    ///   just-inserted entry and silently disable debounce.
    /// * `suppression_grace_ms` — how long suppression entries stay
    ///   live past the [`super::Suppression`] guard's `Drop`. Must
    ///   exceed the typical kernel event-delivery latency (FSEvents:
    ///   tens of ms; inotify-under-load: hundreds of ms). 500 ms is the
    ///   recommended default for darwin; tune up if hash-skip in SS.4b
    ///   shows persistent self-event leakage.
    ///
    /// # Panics
    ///
    /// Panics if `max_age_ms == 0` (a zero age evicts every fresh
    /// debounce entry) or if `suppression_grace_ms == 0` (a zero grace
    /// makes the suppression set instantly empty after each `Drop`,
    /// defeating the primitive's purpose).
    pub fn new(
        debounce_window_ms: u64,
        max_entries: usize,
        max_age_ms: u64,
        suppression_grace_ms: u64,
    ) -> Self {
        assert!(
            max_age_ms > 0,
            "Dispatcher::new requires max_age_ms > 0 (a zero age evicts every fresh entry)"
        );
        assert!(
            suppression_grace_ms > 0,
            "Dispatcher::new requires suppression_grace_ms > 0 (zero grace defeats suppression)"
        );
        Self {
            core: Arc::new(DispatchCore {
                handlers: Mutex::new(HashMap::new()),
                direct_file_filters: Mutex::new(HashMap::new()),
                debounce: Mutex::new(HashMap::new()),
                suppression: SuppressionHandle {
                    set: Arc::new(Mutex::new(HashMap::new())),
                    grace: Duration::from_millis(suppression_grace_ms),
                    max_entries,
                },
                debounce_window: Duration::from_millis(debounce_window_ms),
                max_entries,
                max_age: Duration::from_millis(max_age_ms),
                trailing: Mutex::new(TrailingQueue::default()),
                #[cfg(test)]
                counts: DispatchCounters::default(),
            }),
            rescan_handlers: Mutex::new(HashMap::new()),
            context_index_rescan: Mutex::new(None),
        }
    }

    /// SS-FU-13 AC-1 (test-only): snapshot the dispatch-outcome tallies.
    /// Cheap (four relaxed atomic loads). Consumed by the cascade harness;
    /// gated to `#[cfg(test)]` so release builds carry no counters. See
    /// [`DispatchCounts`].
    #[cfg(test)]
    pub fn counts(&self) -> DispatchCounts {
        DispatchCounts {
            delivered: self.core.counts.delivered.load(Ordering::Relaxed),
            suppressed: self.core.counts.suppressed.load(Ordering::Relaxed),
            debounced: self.core.counts.debounced.load(Ordering::Relaxed),
            no_handler: self.core.counts.no_handler.load(Ordering::Relaxed),
        }
    }

    /// Register a handler keyed by `dir` and return the previous
    /// handler at that key (if any). Events whose path's first matching
    /// ancestor is `dir` are dispatched to `handler`. Re-keying the same
    /// `dir` replaces the previous handler. Safe to call before or
    /// after the [`super::Watcher`] is constructed *and* before the
    /// directory exists on disk — `stable_key` produces a parent-anchored
    /// key that stays valid once the leaf appears.
    ///
    /// SS-4a iter-3 X4: returns the displaced handler so a transactional
    /// caller (e.g. `register_directory_handlers` Phase 2a) can restore
    /// it on a later rollback, instead of removing the key entirely and
    /// losing the prior config's handler at shared dirs (legacy
    /// `install_dirs[i]` overlap across configs).
    ///
    /// Returns `None` if the mutex is poisoned (the operation no-ops and
    /// the warning is logged).
    pub fn register_handler(
        &self,
        dir: &Path,
        handler: DispatchHandler,
    ) -> Option<DispatchHandler> {
        let key = stable_key(dir);
        match self.core.handlers.lock() {
            Ok(mut locked) => locked.insert(key, handler),
            Err(_) => {
                warn!(
                    target: "content_engine::dispatcher",
                    "handler mutex poisoned; register is a no-op"
                );
                None
            }
        }
    }

    /// Register a directory-scoped rescan handler for
    /// `dir` — fired only by [`dispatch_rescan`](Self::dispatch_rescan) /
    /// [`dispatch_rescan_all`](Self::dispatch_rescan_all) on a kernel
    /// rescan/overflow signal. Callers register the SAME closure they pass to
    /// [`register_handler`]; consumers opt in explicitly, so a rescan never fires
    /// an ordinary forward/context handler. Overwrites any prior rescan handler
    /// at `dir` (idempotent re-attach). No-op on a poisoned mutex.
    pub fn register_rescan_handler(&self, dir: &Path, handler: DispatchHandler) {
        let key = stable_key(dir);
        match self.rescan_handlers.lock() {
            Ok(mut locked) => {
                locked.insert(key, handler);
            }
            Err(_) => warn!(
                target: "content_engine::dispatcher",
                "rescan-handler mutex poisoned; register is a no-op"
            ),
        }
    }

    /// Bind metadata reconciliation for the app's current synced footprint.
    /// Replaces the prior binding; directory teardown does not remove it.
    pub(crate) fn set_context_index_rescan_handler(
        &self,
        handler: Arc<dyn Fn(Option<&Path>) + Send + Sync>,
    ) {
        match self.context_index_rescan.lock() {
            Ok(mut current) => *current = Some(handler),
            Err(_) => warn!(
                target: "content_engine::dispatcher",
                "context-index rescan mutex poisoned; binding is a no-op"
            ),
        }
    }

    fn reconcile_context_index(&self, path: Option<&Path>) {
        let handler = match self.context_index_rescan.lock() {
            Ok(current) => current.clone(),
            Err(_) => {
                warn!(
                    target: "content_engine::dispatcher",
                    "context-index rescan mutex poisoned; reconciliation is a no-op"
                );
                None
            }
        };
        if let Some(handler) = handler {
            if catch_unwind(AssertUnwindSafe(|| handler(path))).is_err() {
                error!(
                    target: "content_engine::dispatcher",
                    "context-index rescan handler panicked; continuing scoped recovery"
                );
            }
        }
    }

    /// Restrict direct children of a noisy non-recursive root before suppression or debounce.
    /// Deeper paths retain their own independently registered consumers.
    pub(crate) fn filter_direct_file(&self, dir: &Path, file: &str) {
        let mut filters = self.core.direct_file_filters.lock().unwrap();
        filters.insert(dir.to_path_buf(), file.into());
        filters.insert(stable_key(dir), file.into());
    }

    /// Remove the handler keyed by `dir`. No-op if absent. Also removes any
    /// SW-RV-1 rescan handler at `dir`, so every teardown/rollback path that
    /// unregisters a dir clears its rescan handler in lockstep (no cross-session
    /// bleed of the reverse rescan registry).
    pub fn unregister_handler(&self, dir: &Path) {
        let key = stable_key(dir);
        let mut filters = self.core.direct_file_filters.lock().unwrap();
        filters.remove(dir);
        filters.remove(&key);
        drop(filters);
        match self.core.handlers.lock() {
            Ok(mut locked) => {
                locked.remove(&key);
            }
            Err(_) => warn!(
                target: "content_engine::dispatcher",
                "handler mutex poisoned; unregister is a no-op"
            ),
        }
        match self.rescan_handlers.lock() {
            Ok(mut locked) => {
                locked.remove(&key);
            }
            Err(_) => warn!(
                target: "content_engine::dispatcher",
                "rescan-handler mutex poisoned; unregister is a no-op"
            ),
        }
    }

    /// Cheap-clone handle to the suppression set, passed to
    /// [`super::Suppression::new`] so codesense-driven writes can be
    /// filtered out before they hit the debounce map or any handler.
    pub fn suppression_handle(&self) -> SuppressionHandle {
        self.core.suppression.clone()
    }

    /// Dispatch an event for `path`. Suppression is checked first so
    /// suppressed paths never enter the debounce map. The handler is
    /// looked up by walking the canonicalized ancestor chain.
    ///
    /// Handler panics are caught and logged at `error!` so a single
    /// misbehaving consumer cannot unwind the notify worker thread.
    pub(crate) fn dispatch(&self, path: &Path) {
        DispatchCore::dispatch(&self.core, path);
    }

    /// SW-RV-1 (AC-7): dispatch a **rescan/overflow** signal for `path`.
    ///
    /// Unlike [`dispatch`](Self::dispatch), this bypasses BOTH debounce and
    /// suppression. A kernel "you missed events, rescan here" signal
    /// (`notify::event::Flag::Rescan` — FSEvents `MustScanSubDirs` / inotify
    /// `IN_Q_OVERFLOW`) must never be coalesced away by the debounce window
    /// (it stands for an unknown set of dropped events, not a single file
    /// re-notify), nor mistaken for a codesense self-write (it carries no
    /// specific written file to match against the suppression set). It fires the
    /// ancestor handler exactly as a normal dispatch would; the reverse
    /// handler's response — a whole-worktree reverse claim — is the correct,
    /// directory-scoped drop recovery. Handler panics are caught so a single
    /// consumer cannot unwind the notify worker thread. Independently requests
    /// metadata reconciliation, even when no scoped handler matches.
    pub(crate) fn dispatch_rescan(&self, path: &Path) {
        self.reconcile_context_index(Some(path));
        match self.lookup_rescan_handler(path) {
            Some(handler) => fire_handler(&handler, path, "rescan "),
            None => trace!(
                target: "content_engine::dispatcher",
                "rescan: no reverse handler for {}",
                path.display()
            ),
        }
    }

    /// SW-RV-1 (AC-7): dispatch a **global** rescan to every registered REVERSE
    /// rescan handler (the `rescan_handlers` registry — never a forward/context
    /// handler), each keyed on its own watched directory. Used when the backend
    /// signals a drop with no path — inotify `IN_Q_OVERFLOW` delivers a global
    /// "the event queue overflowed" with no specific subtree, so every watched
    /// reverse worktree may have missed an event and each re-derives its own tree.
    /// FSEvents
    /// `MustScanSubDirs` carries the affected subtree path(s) and takes the
    /// scoped [`dispatch_rescan`](Self::dispatch_rescan) path instead. Snapshots
    /// the handler map under the lock, then fires outside it so a handler that
    /// re-enters the dispatcher cannot deadlock. Metadata reconciliation is
    /// requested even when there are no directory-scoped rescan consumers.
    pub(crate) fn dispatch_rescan_all(&self) {
        self.reconcile_context_index(None);
        let handlers: Vec<(PathBuf, DispatchHandler)> = match self.rescan_handlers.lock() {
            Ok(locked) => locked.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            Err(_) => {
                warn!(
                    target: "content_engine::dispatcher",
                    "rescan-handler mutex poisoned; global rescan is a no-op"
                );
                return;
            }
        };
        for (dir, handler) in handlers {
            fire_handler(&handler, &dir, "rescan ");
        }
    }

    /// SW-RV-1 (AC-7): the reverse-only variant of [`lookup_handler`], resolving
    /// against the `rescan_handlers` registry so a rescan never matches a
    /// forward/context handler.
    fn lookup_rescan_handler(&self, path: &Path) -> Option<DispatchHandler> {
        let handlers = self.rescan_handlers.lock().ok()?;
        lookup_ancestor(&handlers, path)
    }
}

impl DispatchCore {
    /// Dispatch an event for `path` (see [`Dispatcher::dispatch`]). An event
    /// dropped inside the debounce window schedules ONE trailing re-dispatch for
    /// when the window has passed (SW-LN-B50), so the final state of a burst
    /// always reaches the handler.
    fn dispatch(core: &Arc<Self>, path: &Path) {
        {
            let filters = core.direct_file_filters.lock().unwrap();
            if filters.contains_key(path)
                || path
                    .parent()
                    .and_then(|parent| filters.get(parent))
                    .is_some_and(|file| path.file_name() != Some(file.as_os_str()))
            {
                return;
            }
        }
        if core.suppression.is_suppressed(path) {
            #[cfg(test)]
            core.counts.suppressed.fetch_add(1, Ordering::Relaxed);
            trace!(
                target: "content_engine::dispatcher",
                "suppressed: {}",
                path.display()
            );
            return;
        }

        if !core.should_emit(path) {
            #[cfg(test)]
            core.counts.debounced.fetch_add(1, Ordering::Relaxed);
            Self::schedule_trailing(core, path);
            return;
        }

        match core.lookup_handler(path) {
            Some(handler) => {
                #[cfg(test)]
                core.counts.delivered.fetch_add(1, Ordering::Relaxed);
                fire_handler(&handler, path, "");
            }
            None => {
                #[cfg(test)]
                core.counts.no_handler.fetch_add(1, Ordering::Relaxed);
                trace!(
                    target: "content_engine::dispatcher",
                    "no handler for {}",
                    path.display()
                );
            }
        }
    }

    /// SW-LN-B50: queue a trailing re-dispatch of `path` one debounce window from
    /// now, and make sure the flusher thread is running. A path already queued
    /// keeps its earlier due time (one trailing dispatch per burst). The queue is
    /// bounded by `max_entries`; past it a burst's trailing edge is skipped (logged)
    /// and the leading dispatch plus the periodic backstops cover it, as before.
    ///
    /// The trailing dispatch runs the handler on the flusher thread, so a handler
    /// may run concurrently with one on the watcher thread. Handlers are already
    /// `Send + Sync` and every consumer's shared state is lock- or RCU-guarded.
    fn schedule_trailing(core: &Arc<Self>, path: &Path) {
        let due = Instant::now() + core.debounce_window;
        let spawn = {
            let mut q = core.trailing.lock().unwrap_or_else(|e| e.into_inner());
            if !q.due.contains_key(path) && q.due.len() >= core.max_entries {
                debug!(
                    target: "content_engine::dispatcher",
                    "trailing queue full ({}); no trailing dispatch for {}",
                    q.due.len(),
                    path.display()
                );
                return;
            }
            q.due.entry(path.to_path_buf()).or_insert(due);
            !std::mem::replace(&mut q.flusher_running, true)
        };
        if !spawn {
            return;
        }
        // Weak: a dropped dispatcher (tests, a torn-down watcher) ends the flusher
        // instead of being kept alive to fire handlers after teardown.
        let flusher_core = Arc::downgrade(core);
        let spawned = std::thread::Builder::new()
            .name("cs-dispatch-trailing".into())
            .spawn(move || Self::run_trailing_flusher(&flusher_core));
        if let Err(e) = spawned {
            warn!(
                target: "content_engine::dispatcher",
                "trailing flusher spawn failed ({e}); the next debounced event retries"
            );
            core.trailing
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .flusher_running = false;
        }
    }

    /// SW-LN-B50: drain the trailing queue — sleep until the earliest due path,
    /// re-dispatch every due path, and exit once the queue is empty or the
    /// dispatcher is gone. Entries are only ever added with `due = now + window`,
    /// so a later insert can never be due before the earliest queued one and a
    /// plain sleep is enough.
    fn run_trailing_flusher(weak: &std::sync::Weak<Self>) {
        loop {
            let Some(core) = weak.upgrade() else {
                return;
            };
            let ready: Vec<PathBuf> = {
                let mut q = core.trailing.lock().unwrap_or_else(|e| e.into_inner());
                let Some(next) = q.due.values().min().copied() else {
                    q.flusher_running = false;
                    return;
                };
                let now = Instant::now();
                if next > now {
                    drop(q);
                    drop(core);
                    std::thread::sleep(next - now);
                    continue;
                }
                let ready: Vec<PathBuf> = q
                    .due
                    .iter()
                    .filter(|(_, due)| **due <= now)
                    .map(|(p, _)| p.clone())
                    .collect();
                for p in &ready {
                    q.due.remove(p);
                }
                ready
            };
            for path in ready {
                Self::dispatch(&core, &path);
            }
        }
    }

    fn should_emit(&self, path: &Path) -> bool {
        let mut debounce = match self.debounce.lock() {
            Ok(d) => d,
            Err(_) => {
                warn!(
                    target: "content_engine::dispatcher",
                    "debounce mutex poisoned; emitting without debounce"
                );
                return true;
            }
        };
        let now = Instant::now();
        if let Some(prev) = debounce.get(path) {
            if now.duration_since(*prev) < self.debounce_window {
                return false;
            }
        }
        debounce.insert(path.to_path_buf(), now);
        if debounce.len() > self.max_entries {
            let max_age = self.max_age;
            // `<=` keeps just-inserted entries even when max_age is very
            // small — without this, a tight max_age combined with the
            // post-insert eviction would wipe the entry we just stored.
            debounce.retain(|_, instant| instant.elapsed() <= max_age);
        }
        true
    }

    fn lookup_handler(&self, path: &Path) -> Option<DispatchHandler> {
        let handlers = self.handlers.lock().ok()?;
        lookup_ancestor(&handlers, path)
    }
}

/// Resolve the nearest-ancestor handler for `path` in `map`. Walks the stable
/// key's ancestors first, then the raw input's — the raw fallback covers
/// synthetic test paths (`/dir/file.md`) where parent canonicalization fails
/// and the stable-key walk may not match the raw key a test inserted. Shared by
/// the normal ([`Dispatcher::lookup_handler`]) and rescan
/// ([`Dispatcher::lookup_rescan_handler`]) lookups so the ancestor-walk logic
/// lives in one place.
fn lookup_ancestor(
    map: &HashMap<PathBuf, DispatchHandler>,
    path: &Path,
) -> Option<DispatchHandler> {
    let key = stable_key(path);
    for ancestor in key.ancestors() {
        if let Some(handler) = map.get(ancestor) {
            return Some(handler.clone());
        }
    }
    for ancestor in path.ancestors() {
        if let Some(handler) = map.get(ancestor) {
            return Some(handler.clone());
        }
    }
    None
}

/// Invoke `handler` for `path`, catching panics so a single misbehaving
/// consumer cannot unwind the notify worker thread. The one panic-safety seam
/// shared by [`Dispatcher::dispatch`] and the SW-RV-1 rescan paths
/// ([`Dispatcher::dispatch_rescan`] / [`Dispatcher::dispatch_rescan_all`]) — a
/// prior copy-paste of this `catch_unwind` lived at each call site. `label`
/// (`""` for a normal dispatch, `"rescan "` for a rescan) keeps the error log
/// able to distinguish which path the panic came from.
fn fire_handler(handler: &DispatchHandler, path: &Path, label: &str) {
    if let Err(panic) = catch_unwind(AssertUnwindSafe(|| handler(path))) {
        error!(
            target: "content_engine::dispatcher",
            "{}handler panicked for {}; dispatch continues. payload: {:?}",
            label,
            path.display(),
            panic
        );
    }
}

/// Evict expired suppression entries when the map is over its size cap.
/// `is_suppressed` already filters by timestamp, so leaving expired
/// entries in place is correctness-safe; this is purely a memory bound.
fn evict_expired_if_oversized(map: &mut HashMap<PathBuf, SuppressedEntry>, max_entries: usize) {
    if map.len() > max_entries {
        let now = Instant::now();
        map.retain(|_, e| e.valid_until > now);
    }
}

/// Maximum number of ancestor canonicalize attempts before giving up.
/// Bounds the syscall cost when a path lives under a symlink loop
/// (ELOOP per ancestor); 64 is well above any realistic absolute path
/// depth on macOS/Linux but bounded enough to keep dispatch latency
/// O(1) in the pathological case.
const STABLE_KEY_MAX_DEPTH: usize = 64;

/// Directory overlap under either registered or native canonical spelling.
pub(crate) fn paths_overlap(left: &Path, right: &Path) -> bool {
    if left.starts_with(right) || right.starts_with(left) {
        return true;
    }
    let left = stable_key(left);
    let right = stable_key(right);
    left.starts_with(&right) || right.starts_with(&left)
}

/// Compute a stable key for `path`: canonicalize the longest existing
/// ancestor and re-join the missing tail.
///
/// Crucially, we **never** canonicalize the leaf directly — that would
/// make the key depend on whether the leaf exists at call time
/// (`Suppression::new` typically inserts before the file exists; the
/// watcher dispatches after the file exists; if canonicalize behavior
/// at the leaf level differs between those two calls, the suppression
/// lookup misses).
///
/// Fast path: parent exists and canonicalizes → `canon(parent) + leaf`.
/// Fallback: walk further up until some ancestor canonicalizes; re-join
/// every skipped component. Final fallback: raw input.
///
/// Relative paths emit a `debug!` warning — suppression/handler keys
/// are expected to be absolute, and a relative path will not match the
/// absolute paths the watcher delivers.
///
/// `pub(crate)` so the suppressed-write FE nudge
/// ([`crate::commands::watch::subscribers_for_path`], SW-FX-26) resolves a
/// written path to its watched-ancestor subscribers with the exact same key
/// normalization the dispatcher's [`Dispatcher::lookup_handler`] uses — a
/// divergent walk would miss on macOS `/private` symlink forms.
pub(crate) fn stable_key(path: &Path) -> PathBuf {
    if path.is_relative() {
        debug!(
            target: "content_engine::dispatcher",
            "stable_key called with relative path {:?}; suppression/handler keys are expected to be absolute",
            path
        );
    }
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    let mut cursor = path;
    for _ in 0..STABLE_KEY_MAX_DEPTH {
        let parent = match cursor.parent() {
            Some(p) => p,
            None => break,
        };
        if let Some(name) = cursor.file_name() {
            tail.push(name);
        }
        if let Ok(canon_parent) = std::fs::canonicalize(parent) {
            let mut out = canon_parent;
            for name in tail.iter().rev() {
                out.push(name);
            }
            return out;
        }
        cursor = parent;
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use std::thread;
    use std::time::Duration;

    fn recorder() -> (DispatchHandler, Arc<StdMutex<Vec<PathBuf>>>) {
        let log = Arc::new(StdMutex::new(Vec::<PathBuf>::new()));
        let log_clone = Arc::clone(&log);
        let handler: DispatchHandler = Arc::new(move |path: &Path| {
            log_clone.lock().unwrap().push(path.to_path_buf());
        });
        (handler, log)
    }

    #[test]
    fn hot_root_siblings_never_enter_suppression_debounce_or_ingress() {
        for (accepted, rejected) in [
            ("workspace-config.json", "app-state.json"),
            ("app-state.json", "workspace-config.json"),
        ] {
            let d = Dispatcher::new(50, 100, 60_000, 500);
            let root = Path::new("/settings");
            let (handler, log) = recorder();
            d.register_handler(root, handler);
            d.filter_direct_file(root, accepted);
            d.dispatch(root);
            for name in [
                "state.sqlite3",
                "state.sqlite3-wal",
                "state.sqlite3-shm",
                "CodeSense.log",
                "install.lock.json",
                "workspace-config.json.tmp",
                rejected,
            ] {
                d.dispatch(&root.join(name));
            }
            assert!(d.core.debounce.lock().unwrap().is_empty());
            assert!(d.core.trailing.lock().unwrap().due.is_empty());
            assert_eq!(d.counts(), DispatchCounts::default());
            assert!(log.lock().unwrap().is_empty());
            d.dispatch(&root.join(accepted));
            assert_eq!(log.lock().unwrap().len(), 1);
            d.dispatch(&root.join("users/42/scratch/orgs/acme/repo/config/profile.json"));
            assert_eq!(log.lock().unwrap().len(), 2);
        }
    }

    #[test]
    fn dispatch_invokes_registered_handler() {
        let d = Dispatcher::new(50, 100, 60_000, 500);
        let (handler, log) = recorder();
        d.register_handler(Path::new("/dir"), handler);
        d.dispatch(Path::new("/dir/file.md"));
        assert_eq!(log.lock().unwrap().len(), 1);
    }

    #[test]
    fn dispatch_coalesces_within_window() {
        let d = Dispatcher::new(200, 100, 60_000, 500);
        let (handler, log) = recorder();
        d.register_handler(Path::new("/dir"), handler);
        d.dispatch(Path::new("/dir/file.md"));
        d.dispatch(Path::new("/dir/file.md"));
        d.dispatch(Path::new("/dir/file.md"));
        assert_eq!(log.lock().unwrap().len(), 1);
    }

    /// SW-LN-B50: the events dropped inside the window collapse into exactly ONE
    /// trailing dispatch once it ends, so the burst's last write is never lost.
    #[test]
    fn dispatch_fires_one_trailing_dispatch_after_a_coalesced_burst() {
        let d = Dispatcher::new(40, 100, 60_000, 500);
        let (handler, log) = recorder();
        d.register_handler(Path::new("/dir"), handler);
        for _ in 0..3 {
            d.dispatch(Path::new("/dir/file.md"));
        }
        assert_eq!(log.lock().unwrap().len(), 1, "leading dispatch only");
        let start = std::time::Instant::now();
        while log.lock().unwrap().len() < 2 && start.elapsed() < Duration::from_secs(2) {
            thread::sleep(Duration::from_millis(5));
        }
        thread::sleep(Duration::from_millis(120));
        assert_eq!(
            log.lock().unwrap().len(),
            2,
            "one trailing dispatch for the burst"
        );
    }

    /// SW-LN-B50: the trailing queue is bounded by `max_entries`.
    #[test]
    fn trailing_queue_is_bounded() {
        let d = Dispatcher::new(5_000, 2, 60_000, 500);
        let (handler, _log) = recorder();
        d.register_handler(Path::new("/dir"), handler);
        for i in 0..10 {
            let p = PathBuf::from(format!("/dir/f{i}.md"));
            d.dispatch(&p);
            d.dispatch(&p);
        }
        assert!(d.core.trailing.lock().unwrap().due.len() <= 2);
    }

    /// SW-LN-B50: dropping the dispatcher ends its flusher without firing.
    #[test]
    fn dropped_dispatcher_fires_no_trailing_dispatch() {
        let d = Dispatcher::new(60, 100, 60_000, 500);
        let (handler, log) = recorder();
        d.register_handler(Path::new("/dir"), handler);
        d.dispatch(Path::new("/dir/file.md"));
        d.dispatch(Path::new("/dir/file.md"));
        drop(d);
        thread::sleep(Duration::from_millis(200));
        assert_eq!(log.lock().unwrap().len(), 1, "only the leading dispatch");
    }

    /// SW-LN-B50: a lone event (nothing dropped) gets no trailing dispatch.
    #[test]
    fn dispatch_without_a_dropped_event_has_no_trailing_dispatch() {
        let d = Dispatcher::new(20, 100, 60_000, 500);
        let (handler, log) = recorder();
        d.register_handler(Path::new("/dir"), handler);
        d.dispatch(Path::new("/dir/file.md"));
        thread::sleep(Duration::from_millis(100));
        assert_eq!(log.lock().unwrap().len(), 1);
    }

    #[test]
    fn dispatch_emits_again_after_window() {
        let d = Dispatcher::new(20, 100, 60_000, 500);
        let (handler, log) = recorder();
        d.register_handler(Path::new("/dir"), handler);
        d.dispatch(Path::new("/dir/file.md"));
        thread::sleep(Duration::from_millis(40));
        d.dispatch(Path::new("/dir/file.md"));
        assert_eq!(log.lock().unwrap().len(), 2);
    }

    #[test]
    fn dispatch_silently_drops_when_no_handler_registered() {
        let d = Dispatcher::new(50, 100, 60_000, 500);
        d.dispatch(Path::new("/orphan/file.md"));
    }

    #[test]
    fn debounce_map_is_bounded() {
        // max_entries = 1, max_age = 50ms → eviction past the threshold
        // keeps only entries inserted within the last 50ms.
        let d = Dispatcher::new(0, 1, 50, 500);
        let (handler, _log) = recorder();
        d.register_handler(Path::new("/dir"), handler);

        for i in 0..50 {
            let p = PathBuf::from(format!("/dir/file_{}.md", i));
            d.dispatch(&p);
        }
        thread::sleep(Duration::from_millis(120));
        let p = PathBuf::from("/dir/final.md");
        d.dispatch(&p);
        let len = d.core.debounce.lock().unwrap().len();
        assert!(len <= 2, "debounce map grew unbounded: len={}", len);
    }

    #[test]
    #[should_panic(expected = "max_age_ms > 0")]
    fn dispatcher_rejects_zero_max_age() {
        let _ = Dispatcher::new(50, 100, 0, 500);
    }

    #[test]
    #[should_panic(expected = "suppression_grace_ms > 0")]
    fn dispatcher_rejects_zero_grace() {
        let _ = Dispatcher::new(50, 100, 60_000, 0);
    }

    #[test]
    fn handler_lookup_walks_ancestors() {
        let d = Dispatcher::new(50, 100, 60_000, 500);
        let (handler, log) = recorder();
        d.register_handler(Path::new("/repo"), handler);
        d.dispatch(Path::new("/repo/nested/deep/file.md"));
        assert_eq!(log.lock().unwrap().len(), 1);
    }

    #[test]
    fn dispatch_counts_track_each_outcome_bucket() {
        // SS-FU-13 AC-1: each of the four distinct dispatch outcomes
        // increments exactly its own bucket — proving the partition. A long window
        // keeps the SW-LN-B50 trailing dispatch well past the assertions.
        let d = Dispatcher::new(5_000, 100, 60_000, 60_000);
        let (handler, _log) = recorder();
        d.register_handler(Path::new("/dir"), handler);

        // delivered: first event for a registered dir fires the handler.
        d.dispatch(Path::new("/dir/a.md"));
        // debounced: same path again inside the window is coalesced.
        d.dispatch(Path::new("/dir/a.md"));
        // suppressed: pre-registered on the suppression set, dropped first.
        let handle = d.suppression_handle();
        let supp = PathBuf::from("/dir/b.md");
        handle.insert_paths(std::slice::from_ref(&supp));
        d.dispatch(&supp);
        // no_handler: path with no registered ancestor.
        d.dispatch(Path::new("/orphan/c.md"));

        let c = d.counts();
        assert_eq!(c.delivered, 1, "delivered");
        assert_eq!(c.debounced, 1, "debounced");
        assert_eq!(c.suppressed, 1, "suppressed");
        assert_eq!(c.no_handler, 1, "no_handler");
    }

    #[test]
    fn suppression_handle_blocks_dispatch_within_grace_window() {
        let d = Dispatcher::new(50, 100, 60_000, 60_000);
        let (handler, log) = recorder();
        d.register_handler(Path::new("/dir"), handler);
        let handle = d.suppression_handle();
        let path = PathBuf::from("/dir/file.md");
        handle.insert_paths(std::slice::from_ref(&path));

        d.dispatch(&path);
        assert_eq!(log.lock().unwrap().len(), 0);
    }

    #[test]
    fn suppression_resumes_after_grace_window_expires() {
        // Very short grace lets us prove the entry decays without
        // making the test slow. Suppression check fires after the
        // grace window has elapsed → event passes through.
        let d = Dispatcher::new(0, 100, 60_000, 10);
        let (handler, log) = recorder();
        d.register_handler(Path::new("/dir"), handler);
        let handle = d.suppression_handle();
        let path = PathBuf::from("/dir/expires.md");
        handle.insert_paths(std::slice::from_ref(&path));

        thread::sleep(Duration::from_millis(50));
        d.dispatch(&path);
        assert_eq!(
            log.lock().unwrap().len(),
            1,
            "event should dispatch after suppression window expires"
        );
    }

    #[test]
    fn overlapping_inserts_extend_grace_window_monotonically() {
        // Two inserts on the same path. The second extends the timestamp
        // by another full grace window even though the first is still
        // "alive." This composes overlapping guards safely: no early-drop
        // can shorten the suppression because there is no decrement.
        let d = Dispatcher::new(0, 100, 60_000, 60_000);
        let (handler, log) = recorder();
        d.register_handler(Path::new("/dir"), handler);
        let handle = d.suppression_handle();
        let path = PathBuf::from("/dir/contested.md");

        handle.insert_paths(std::slice::from_ref(&path)); // window 1
        handle.insert_paths(std::slice::from_ref(&path)); // window 2 — extends
        d.dispatch(&path);
        assert_eq!(log.lock().unwrap().len(), 0);
    }

    #[test]
    fn insert_does_not_shorten_an_existing_longer_window() {
        let d = Dispatcher::new(0, 100, 60_000, 60_000);
        let handle = d.suppression_handle();
        let path = PathBuf::from("/dir/long.md");

        // First insert with long grace.
        handle.insert_paths(std::slice::from_ref(&path));
        // Simulate a second handle with a shorter grace by temporarily
        // shrinking the grace; max-update must keep the longer window.
        let mut short = handle.clone();
        short.grace = Duration::from_millis(1);
        short.insert_paths(std::slice::from_ref(&path));

        thread::sleep(Duration::from_millis(50));
        let (handler, log) = recorder();
        d.register_handler(Path::new("/dir"), handler);
        d.dispatch(&path);
        assert_eq!(
            log.lock().unwrap().len(),
            0,
            "longer window must survive a shorter-window insert"
        );
    }

    #[test]
    fn handler_panic_is_caught_and_dispatch_continues() {
        let d = Dispatcher::new(0, 100, 60_000, 500);

        let panicky: DispatchHandler = Arc::new(|_path: &Path| {
            panic!("intentional handler panic for test");
        });
        d.register_handler(Path::new("/p"), panicky);

        d.dispatch(Path::new("/p/file.md"));

        let (handler, log) = recorder();
        d.register_handler(Path::new("/q"), handler);
        d.dispatch(Path::new("/q/file.md"));
        assert_eq!(log.lock().unwrap().len(), 1);
    }

    #[test]
    fn unregister_stops_dispatching() {
        let d = Dispatcher::new(50, 100, 60_000, 500);
        let (handler, log) = recorder();
        d.register_handler(Path::new("/dir"), handler);
        d.dispatch(Path::new("/dir/a.md"));
        d.unregister_handler(Path::new("/dir"));
        d.dispatch(Path::new("/dir/b.md"));
        assert_eq!(log.lock().unwrap().len(), 1);
    }

    #[test]
    fn dispatch_rescan_fires_handler_bypassing_debounce() {
        // SW-RV-1 (AC-7): a rescan signal must NOT be coalesced by the debounce
        // window — it stands for an unknown set of dropped events. Even a path
        // just dispatched (which sets the debounce timestamp) fires again on
        // rescan. A huge debounce window proves the bypass is real, not a race.
        let d = Dispatcher::new(60_000, 100, 60_000, 60_000);
        let (handler, log) = recorder();
        d.register_handler(Path::new("/repo"), handler.clone());
        d.register_rescan_handler(Path::new("/repo"), handler);

        d.dispatch(Path::new("/repo/nested/file.md")); // sets debounce ts
        assert_eq!(log.lock().unwrap().len(), 1, "normal dispatch delivered");
        // A second NORMAL dispatch inside the 60s window would be debounced away…
        d.dispatch(Path::new("/repo/nested/file.md"));
        assert_eq!(
            log.lock().unwrap().len(),
            1,
            "…confirming the window is closed"
        );
        // …but a rescan for the same subtree still fires.
        d.dispatch_rescan(Path::new("/repo/nested"));
        assert_eq!(
            log.lock().unwrap().len(),
            2,
            "rescan must bypass debounce and fire the handler"
        );
    }

    #[test]
    fn dispatch_rescan_fires_handler_bypassing_suppression() {
        // SW-RV-1 (AC-7): a rescan carries no specific written file, so it must
        // not be dropped by the self-write suppression set — a codesense write
        // burst that overflows FSEvents is exactly when we most need to recover.
        let d = Dispatcher::new(0, 100, 60_000, 60_000);
        let (handler, log) = recorder();
        d.register_handler(Path::new("/repo"), handler.clone());
        d.register_rescan_handler(Path::new("/repo"), handler);
        let handle = d.suppression_handle();
        let suppressed = PathBuf::from("/repo/x.md");
        handle.insert_paths(std::slice::from_ref(&suppressed));

        // Normal dispatch of the suppressed path is dropped.
        d.dispatch(&suppressed);
        assert_eq!(
            log.lock().unwrap().len(),
            0,
            "suppressed normal dispatch dropped"
        );
        // Rescan fires regardless of suppression.
        d.dispatch_rescan(&suppressed);
        assert_eq!(
            log.lock().unwrap().len(),
            1,
            "rescan must bypass suppression and fire the handler"
        );
    }

    #[test]
    fn dispatch_rescan_no_handler_is_a_quiet_noop() {
        // A rescan for a subtree with no registered consumer must not panic.
        let d = Dispatcher::new(50, 100, 60_000, 500);
        d.dispatch_rescan(Path::new("/unwatched/deep/dir"));
    }

    #[test]
    fn context_index_rescan_binding_replaces_the_previous_callback() {
        let d = Dispatcher::new(50, 100, 60_000, 500);
        let old = Arc::new(AtomicU64::new(0));
        let previous = Arc::clone(&old);
        d.set_context_index_rescan_handler(Arc::new(move |_| {
            previous.fetch_add(1, Ordering::Relaxed);
        }));
        let current = Arc::new(AtomicU64::new(0));
        let replacement = Arc::clone(&current);
        d.set_context_index_rescan_handler(Arc::new(move |_| {
            replacement.fetch_add(1, Ordering::Relaxed);
        }));

        d.dispatch(Path::new("/unwatched/file.md"));
        assert_eq!(current.load(Ordering::Relaxed), 0);
        d.dispatch_rescan(Path::new("/unwatched/subtree"));
        d.dispatch_rescan_all();
        assert_eq!(old.load(Ordering::Relaxed), 0);
        assert_eq!(current.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn dispatch_rescan_all_fires_every_registered_handler() {
        // SW-RV-1 (AC-7): the global-overflow path (inotify IN_Q_OVERFLOW, no
        // path) must re-derive EVERY watched worktree — each handler fires once,
        // keyed on its own dir.
        let d = Dispatcher::new(50, 100, 60_000, 500);
        let (h_a, log_a) = recorder();
        let (h_b, log_b) = recorder();
        d.register_rescan_handler(Path::new("/repo_a"), h_a);
        d.register_rescan_handler(Path::new("/repo_b"), h_b);

        d.dispatch_rescan_all();

        assert_eq!(log_a.lock().unwrap().len(), 1, "repo_a handler fired once");
        assert_eq!(log_b.lock().unwrap().len(), 1, "repo_b handler fired once");
        // The path each handler receives is its own registered dir.
        assert!(log_a.lock().unwrap()[0].ends_with("repo_a"));
        assert!(log_b.lock().unwrap()[0].ends_with("repo_b"));
    }

    #[test]
    fn rescan_ignores_non_reverse_handlers() {
        // SW-RV-1 (review E2): a handler registered ONLY on the normal map (a
        // forward/context consumer) must NEVER fire on a rescan — rescan is
        // reverse-only, so firing a projection handler with a dir-path (bogus
        // targeted projection) can't happen. Both rescan paths ignore it.
        let d = Dispatcher::new(50, 100, 60_000, 500);
        let (fwd, fwd_log) = recorder();
        d.register_handler(Path::new("/forward"), fwd); // normal map ONLY

        d.dispatch_rescan(Path::new("/forward/sub"));
        d.dispatch_rescan_all();
        assert_eq!(
            fwd_log.lock().unwrap().len(),
            0,
            "a non-reverse (forward) handler must never fire on rescan"
        );

        // Sanity: it DOES still fire on a normal dispatch.
        d.dispatch(Path::new("/forward/sub/file.md"));
        assert_eq!(
            fwd_log.lock().unwrap().len(),
            1,
            "normal dispatch unaffected"
        );
    }

    #[test]
    fn unregister_handler_clears_the_rescan_registry_too() {
        // SW-RV-1 (review E2): `unregister_handler` removes from BOTH maps, so
        // every teardown/rollback path (which already calls it) clears the reverse
        // rescan handler in lockstep — no cross-session bleed.
        let d = Dispatcher::new(50, 100, 60_000, 500);
        let (h, log) = recorder();
        d.register_rescan_handler(Path::new("/repo"), h);
        d.unregister_handler(Path::new("/repo"));

        d.dispatch_rescan(Path::new("/repo/x"));
        d.dispatch_rescan_all();
        assert_eq!(
            log.lock().unwrap().len(),
            0,
            "rescan handler must be gone after unregister_handler"
        );
    }

    #[test]
    fn dispatch_rescan_all_survives_a_panicking_handler() {
        // One misbehaving consumer must not stop the others from recovering.
        let d = Dispatcher::new(50, 100, 60_000, 500);
        let panicky: DispatchHandler = Arc::new(|_p: &Path| panic!("boom"));
        let (good, log) = recorder();
        d.register_rescan_handler(Path::new("/bad"), panicky);
        d.register_rescan_handler(Path::new("/good"), good);

        d.dispatch_rescan_all(); // must not unwind

        assert_eq!(
            log.lock().unwrap().len(),
            1,
            "the good handler still fired after the bad one panicked"
        );
    }

    #[test]
    fn stable_key_uses_parent_canonicalize_even_when_leaf_exists() {
        // The key for "<tempdir>/missing.txt" and "<tempdir>/existing.txt"
        // must compute identically: parent canonicalized + leaf appended,
        // regardless of leaf-on-disk state. Pinning this prevents the
        // iter-2 "leaf-exists vs leaf-doesn't-exist" canonicalize drift
        // that caused suppression key divergence.
        let dir = tempfile::TempDir::new().unwrap();
        let existing = dir.path().join("existing.txt");
        std::fs::write(&existing, b"x").unwrap();
        let missing = dir.path().join("missing.txt");

        let k_existing = stable_key(&existing);
        let k_missing = stable_key(&missing);
        assert_eq!(k_existing.parent(), k_missing.parent());
        assert_eq!(k_existing.file_name(), Some(existing.file_name().unwrap()));
        assert_eq!(k_missing.file_name(), Some(missing.file_name().unwrap()));
    }
}
