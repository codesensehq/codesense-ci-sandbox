// SPDX-License-Identifier: MIT
//! Debounced filesystem watcher + RAII suppression guard.
//!
//! [`Watcher`] is a thin wrapper around `notify::RecommendedWatcher` that
//! resolves each event path to a parent directory and hands it off to a
//! shared [`Dispatcher`]. The watcher itself does not interpret event
//! kinds beyond filtering noise (notify error events, paths with no
//! parent).
//!
//! [`Suppression`] is the RAII guard that downstream stories (SS.4a/b)
//! will hold while codesense itself writes to a path it is also watching,
//! so the watcher does not see its own writes. It mirrors the existing
//! `HealGuard::new(set, paths)` shape in `commands/watch.rs:68-85` —
//! exact-same pattern, consumer-agnostic name, dispatcher-owned set —
//! with the key generalization that the suppression set is **refcounted**
//! (see [`super::dispatcher::SuppressionHandle`]) so overlapping guards
//! compose safely.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use log::{debug, warn};
use notify::{
    Config, ErrorKind as NotifyErrorKind, RecommendedWatcher, RecursiveMode,
    Watcher as NotifyWatcher,
};

use super::dispatcher::{Dispatcher, SuppressionHandle};
use super::Error;

/// One directory to watch, plus whether to recurse into it.
#[derive(Clone, Debug)]
pub struct WatchDir {
    pub path: PathBuf,
    pub recursive: bool,
}

/// SS-4a iter-4 Y1: outcome of [`Watcher::watch`]. Distinguishes
/// fresh-attach (caller should roll back on failure) from dedup-skip
/// (the underlying kernel watch was attached by a prior caller — this
/// call did nothing, and a rollback `unwatch` would erroneously detach
/// the still-relied-on watch).
///
/// Both variants carry the canonical key used for bookkeeping, so
/// callers that need the canonical (e.g., to record in their own
/// rollback list) can extract it uniformly.
///
/// `#[must_use]` (iter-5 Z1): the variant discrimination IS the
/// contract. A `let _ = watcher.watch(d);` would silently discard the
/// rollback obligation; the compiler now warns. `Watcher::new`'s
/// initial-set loop explicitly binds the outcome so the discard is
/// deliberate.
///
/// `#[non_exhaustive]` (iter-5 Z2): future stories (SS-4b, SS-4c) may
/// add variants (e.g., `Upgraded` for recursive-flag promotion). This
/// annotation forces consumers to write a `_ =>` arm so adding a
/// variant later isn't a breaking change.
#[derive(Debug, Clone)]
#[must_use = "WatchOutcome encodes the rollback obligation; discarding it loses the Attached vs AlreadyAttached distinction"]
#[non_exhaustive]
pub enum WatchOutcome {
    /// `inner.watch(&canon, mode)` succeeded; the kernel watch is owned
    /// by this `Watcher::watch` call. A failing batch must call
    /// `unwatch(&canon)` to roll back the attach.
    Attached(PathBuf),
    /// The canonical key was already in the bookkeeping set (a prior
    /// `watch` call attached it). This call is a silent no-op — DO NOT
    /// `unwatch(&canon)` on rollback or the prior owner's attach is
    /// destroyed.
    AlreadyAttached(PathBuf),
}

impl WatchOutcome {
    /// The canonical key, regardless of which variant.
    pub fn canon(&self) -> &Path {
        match self {
            WatchOutcome::Attached(p) | WatchOutcome::AlreadyAttached(p) => p,
        }
    }
}

/// Configuration for a [`Watcher`] instance.
///
/// The debounce window lives on the [`Dispatcher`] — `WatchSetup` only
/// describes which directories to attach the underlying notify watcher
/// to. (Earlier drafts carried a redundant `debounce_window_ms` field
/// here; it was removed during the SS-1 code review because two
/// independently-configured windows on `WatchSetup` and `Dispatcher`
/// invited misconfiguration with no compile-time guard.)
#[derive(Clone, Debug)]
pub struct WatchSetup {
    pub dirs: Vec<WatchDir>,
}

/// Route one `notify` result into `dispatcher`: the body of the kernel
/// watcher's callback, a free function so tests can feed it synthetic events
/// (including coalesced directory-level and rescan-flagged ones) without a
/// kernel stream.
fn route_event(dispatcher: &Dispatcher, res: Result<notify::Event, notify::Error>) {
    let event = match res {
        Ok(e) => e,
        Err(e) => {
            warn!(
                target: "content_engine::watcher",
                "notify error event: {}",
                e
            );
            return;
        }
    };
    // SW-RV-1 (AC-7): a kernel rescan/overflow signal
    // (`notify::event::Flag::Rescan` — FSEvents `MustScanSubDirs` /
    // inotify `IN_Q_OVERFLOW`) means events were dropped or coalesced
    // and a subtree must be re-scanned. Recover event-driven — fire a
    // scoped rescan for each carried subtree, or a global rescan when
    // the backend delivers no path (inotify overflow) — instead of
    // waiting for the (now sampled/slow) poll. This is what lets the
    // poll retire from its steady-state drop-recovery job without
    // opening a staleness window (the mechanism Watchman / git
    // `fsmonitor` use). The rescan path bypasses debounce+suppression
    // by design (see `Dispatcher::dispatch_rescan`); a normal dispatch
    // of the same paths would only redundantly re-trigger the same
    // whole-worktree claim, so we take one path or the other.
    if event.need_rescan() {
        if event.paths.is_empty() {
            dispatcher.dispatch_rescan_all();
        } else {
            for path in &event.paths {
                dispatcher.dispatch_rescan(path);
            }
        }
        return;
    }
    for path in &event.paths {
        dispatcher.dispatch(path);
    }
}

#[cfg(test)]
pub(crate) fn route_test_event(dispatcher: &Dispatcher, event: notify::Event) {
    route_event(dispatcher, Ok(event));
}

/// Active filesystem watcher. Owns the underlying `notify` watcher;
/// drops it (stopping watch threads) when the [`Watcher`] is dropped.
///
/// SS-4a: interior-mutability shape (`Mutex<RecommendedWatcher>`) so
/// dynamic `watch` / `unwatch` after construction can take `&self`. This
/// matches how `commands/watch.rs::WatcherState` holds the watcher
/// behind `Arc<Watcher>` and serves both `start_watching` (initial set)
/// and `add_watch` / `remove_watch` (per-repo incremental changes) from
/// the same instance.
pub struct Watcher {
    inner: Mutex<RecommendedWatcher>,
    /// Set of `(canonical_path, recursive)` pairs currently attached to
    /// `inner`. Used for runtime dedup (idempotent `watch`) and for
    /// scrubbing on `unwatch` so a caller that doesn't remember the
    /// `recursive` flag can still drop the watch cleanly.
    watched: Mutex<HashSet<(PathBuf, bool)>>,
}

impl Watcher {
    /// Build a watcher that feeds events into `dispatcher`. Handlers may
    /// be registered on the dispatcher either before or after this call —
    /// the dispatcher consults its handler map at dispatch time, and
    /// events whose ancestors have no registered handler are silently
    /// dropped with a `trace!` log.
    ///
    /// `setup.dirs` may be empty; the caller can attach directories
    /// later via [`Watcher::watch`]. Duplicate `WatchDir` entries (same
    /// canonical path + same `recursive` flag) are deduplicated, so a
    /// caller passing the same directory twice does not get an opaque
    /// "watch failed" error from the second `notify::watch` call.
    pub fn new(setup: WatchSetup, dispatcher: Arc<Dispatcher>) -> Result<Self, Error> {
        let dispatcher_for_closure = Arc::clone(&dispatcher);
        let inner = RecommendedWatcher::new(
            move |res: Result<notify::Event, notify::Error>| {
                route_event(&dispatcher_for_closure, res)
            },
            Config::default(),
        )?;

        let watcher = Self {
            inner: Mutex::new(inner),
            watched: Mutex::new(HashSet::new()),
        };
        // Defer to `watch` so the initial-set path and the incremental
        // add path share one implementation (dedup, canonicalize, attach,
        // bookkeeping) — single source of truth for "watch a directory."
        //
        // iter-5 Z1: explicitly bind the `WatchOutcome` so the
        // `#[must_use]` enum doesn't warn. The initial-set loop has no
        // rollback path (a partial-failure Self drop on `?` kills the
        // whole watcher), so distinguishing Attached vs AlreadyAttached
        // is genuinely irrelevant here.
        for dir in setup.dirs {
            let _outcome = watcher.watch(dir)?;
        }
        Ok(watcher)
    }

    /// Attach a directory to the running watcher. Idempotent: a second
    /// call for the same canonical path + `recursive` flag returns
    /// `Ok(WatchOutcome::AlreadyAttached(canon))` without re-calling
    /// notify (which would error opaquely on some platforms).
    ///
    /// Returns [`WatchOutcome`] so callers can distinguish:
    /// - `Attached(canon)`: this call performed the kernel attach; a
    ///   failing-batch rollback must `unwatch(&canon)` to undo.
    /// - `AlreadyAttached(canon)`: another call owns the kernel watch;
    ///   rollback must NOT `unwatch(&canon)` or the prior owner's
    ///   attach is destroyed (SS-4a iter-4 Y1 — the dedup-skip case
    ///   used to return `Ok(canon)` indistinguishable from a real
    ///   attach, leading rollback to erroneously detach a still-needed
    ///   watch).
    ///
    /// Canonicalizes the input path before keying so `/var/foo` and
    /// `/private/var/foo` (macOS) — or any other symlink-aliased path —
    /// collapse to one watch entry. Falls back to the raw path when
    /// canonicalize fails so non-existent dirs still surface through the
    /// `notify::watch` error path.
    ///
    /// `&self` (not `&mut self`) so callers holding an `Arc<Watcher>` can
    /// add watches without giving up shared ownership.
    ///
    /// **Lock discipline:** holds `watched` and `inner` **simultaneously**
    /// for the entire operation. Acquisition order matches
    /// [`Watcher::unwatch`] (watched → inner), preventing the race window
    /// where one method drops `watched` mid-op and another sees stale
    /// bookkeeping (SS-4a iter-2 H2 fix).
    pub(crate) fn watched_paths(&self) -> Result<Vec<PathBuf>, Error> {
        Ok(self
            .watched
            .lock()
            .map_err(|_| Error::WatcherLockPoisoned)?
            .iter()
            .map(|(p, _)| p.clone())
            .collect())
    }

    pub fn watch(&self, dir: WatchDir) -> Result<WatchOutcome, Error> {
        let canon = std::fs::canonicalize(&dir.path).unwrap_or_else(|_| dir.path.clone());
        let key = (canon.clone(), dir.recursive);

        let mut watched = self
            .watched
            .lock()
            .map_err(|_| Error::WatcherLockPoisoned)?;
        let mut inner = self.inner.lock().map_err(|_| Error::WatcherLockPoisoned)?;

        if !watched.insert(key.clone()) {
            debug!(
                target: "content_engine::watcher",
                "skipping duplicate WatchDir {} (recursive={})",
                dir.path.display(),
                dir.recursive
            );
            return Ok(WatchOutcome::AlreadyAttached(canon));
        }

        let mode = if dir.recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        // SS-4a iter-3 X1: attach using the canonical path so notify's
        // internal watch table is keyed by canon (matching our
        // bookkeeping). Required for cross-platform rollback symmetry:
        // on Linux inotify, notify stores the raw input path in its
        // internal map. If we attached by raw but unwatched by canon
        // (per iter-2 M2), the unwatch would miss → kernel watch leaks
        // (swallowed by `is_not_attached_error`). Attaching by canon
        // makes raw == canon at notify's layer too.
        if let Err(e) = inner.watch(&canon, mode) {
            // Rollback the bookkeeping entry while still holding `watched`
            // so no other thread can observe the half-state.
            watched.remove(&key);
            return Err(e.into());
        }
        debug!(
            target: "content_engine::watcher",
            "watching {} (recursive={})",
            dir.path.display(),
            dir.recursive
        );
        Ok(WatchOutcome::Attached(canon))
    }

    /// Detach a directory from the running watcher. Idempotent: calling
    /// `unwatch` on a path that was never attached (or was already
    /// detached) returns `Ok(())` rather than surfacing notify's
    /// `WatchNotFound` (or platform-equivalent "not attached") error —
    /// matches the contract `commands/watch.rs` has relied on since the
    /// legacy `notify::Watcher::unwatch` calls were `let _ = `-ignored.
    ///
    /// **Lock discipline (SS-4a iter-2 H2/H3 fix):** holds both
    /// `watched` and `inner` simultaneously across the entire operation,
    /// same order as [`Watcher::watch`] (watched → inner). Bookkeeping
    /// is scrubbed on **all** return paths — Ok, "not attached", and
    /// real errors. Rationale: after a real error, the kernel state is
    /// undefined; scrubbing bookkeeping gives callers a clean retry path
    /// via `watch()` rather than locking them into a permanent dedup-skip.
    /// The caller is responsible for handling the `Err` (retry, log, or
    /// propagate).
    ///
    /// **Trade-off documented (iter-3 X7):** scrubbing bookkeeping on
    /// real-error trusts notify's `is_not_attached_error` classification —
    /// any error NOT matching that predicate is presumed to mean the
    /// watch is no longer active. If notify returns a real error while
    /// the kernel still holds the watch (e.g., transient inotify
    /// `EBUSY`/`EINVAL` classified as `Io(Other)`), a subsequent
    /// `watch()` for the same canonical path passes the bookkeeping
    /// dedup check and re-attaches at the kernel level — potentially
    /// double-dispatching events. This is preferred over locking out
    /// retries; production hasn't seen the trigger, but a future
    /// adversarial test should pin behavior on a stubbed real-error.
    ///
    /// **Latency note (iter-3 X8):** the kernel-level `inner.unwatch`
    /// syscall runs while both `watched` and `inner` mutexes are held.
    /// Concurrent `Watcher::watch` / `unwatch` calls serialize on this
    /// syscall — typically milliseconds, but the simultaneous lock-hold
    /// is the price of cross-thread state coherency.
    ///
    /// Scrubs both the canonical and the raw input path from the
    /// bookkeeping set (and both `recursive` flags) so a caller that
    /// re-unwatches after the directory has been deleted (in which case
    /// `canonicalize` falls back to raw) still leaves the state coherent.
    pub fn unwatch(&self, path: &Path) -> Result<(), Error> {
        let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

        let mut watched = self
            .watched
            .lock()
            .map_err(|_| Error::WatcherLockPoisoned)?;
        let mut inner = self.inner.lock().map_err(|_| Error::WatcherLockPoisoned)?;

        // SS-4a iter-3 X1: detach by canonical path so notify's lookup
        // matches the canonical key we attached under (see `watch`).
        // Falls back to raw when canonicalize fails, which yields the
        // same path as `inner.watch` saw on the original attach.
        let unwatch_result = inner.unwatch(&canon);

        // Scrub bookkeeping on all return paths. For real errors the
        // kernel state is unknown; clearing bookkeeping unblocks a
        // retry via `watch()`. The simultaneous lock-hold means no
        // other thread can observe an intermediate state where notify
        // is detached but the bookkeeping says attached.
        watched.remove(&(canon.clone(), true));
        watched.remove(&(canon.clone(), false));
        if canon.as_path() != path {
            watched.remove(&(path.to_path_buf(), true));
            watched.remove(&(path.to_path_buf(), false));
        }

        match unwatch_result {
            Ok(()) => {
                debug!(
                    target: "content_engine::watcher",
                    "unwatched {}",
                    path.display()
                );
                Ok(())
            }
            Err(e) if is_not_attached_error(&e) => {
                debug!(
                    target: "content_engine::watcher",
                    "unwatch idempotent no-op (not attached): {}",
                    path.display()
                );
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }
}

/// SS-4a iter-2 H4 fix: classify a `notify::Error` as "the watch wasn't
/// attached." Includes only the explicit `WatchNotFound` / `PathNotFound`
/// variants plus `Io(NotFound)`. **`Io(InvalidInput)` is deliberately
/// NOT swallowed** — it covers many unrelated failures (descriptor
/// exhaustion, bad path encoding, malformed args) and silently treating
/// it as "not attached" would hide real errors from callers.
fn is_not_attached_error(e: &notify::Error) -> bool {
    if matches!(
        e.kind,
        NotifyErrorKind::WatchNotFound | NotifyErrorKind::PathNotFound
    ) {
        return true;
    }
    if let NotifyErrorKind::Io(io_err) = &e.kind {
        return io_err.kind() == std::io::ErrorKind::NotFound;
    }
    false
}

/// Construction-time guard that extends the dispatcher's suppression
/// window over a set of paths. Each call performs a monotonic
/// `max`-update on the path's `valid_until` timestamp: overlapping
/// callers naturally compose because no call can ever shorten an
/// existing window.
///
/// `Drop` is intentionally a **no-op**. The dispatcher's suppression
/// set is timestamped (not refcounted) and entries expire on their own
/// `valid_until: Instant`. This is the key fix for the
/// FSEvents/inotify delivery race: events that the kernel queues during
/// a codesense-driven write but delivers *after* the write call returns
/// still see the path as suppressed, as long as they arrive within the
/// dispatcher's `suppression_grace_ms` window.
///
/// Iter-1 used a refcounted multiset with `Drop` removing entries;
/// iter-2 (after code review surfaced the kernel-latency race) replaced
/// it with timestamped semantics. The struct + `Suppression::new` shape
/// is preserved for consumer ergonomics — `let _guard = ...` still
/// scopes the suppression visually, even though the timestamp does the
/// real work.
///
/// Poison-tolerant via [`SuppressionHandle`]: a poisoned mutex logs
/// `warn!` and the operation no-ops on insert, fails closed on lookup.
pub struct Suppression {
    _private: (),
}

impl Suppression {
    /// Extend the suppression window for `paths` by the dispatcher's
    /// configured grace duration. Monotonic max-update: never shortens
    /// a longer existing window. Safe to hold across `.await` points
    /// (the underlying insert acquires the suppression mutex only
    /// briefly).
    pub fn new(handle: &SuppressionHandle, paths: impl IntoIterator<Item = PathBuf>) -> Self {
        let paths: Vec<PathBuf> = paths.into_iter().collect();
        handle.insert_paths(&paths);
        Self { _private: () }
    }
}

// No `Drop` impl — timestamped expiry is the lifecycle.

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;
    use tempfile::TempDir;

    use crate::content_engine::dispatcher::DispatchHandler;
    use crate::content_engine::test_support::{act_until, eventually, WATCH_DEADLINE};

    fn recorder() -> (DispatchHandler, Arc<StdMutex<Vec<PathBuf>>>) {
        let log = Arc::new(StdMutex::new(Vec::<PathBuf>::new()));
        let log_clone = Arc::clone(&log);
        let handler: DispatchHandler = Arc::new(move |path: &Path| {
            log_clone.lock().unwrap().push(path.to_path_buf());
        });
        (handler, log)
    }

    /// SW-XS-12: whether the recorder holds a path ending in `name`.
    fn saw(log: &Arc<StdMutex<Vec<PathBuf>>>, name: &str) -> bool {
        log.lock().unwrap().iter().any(|p| p.ends_with(name))
    }

    /// How often a not-yet-observed stimulus is repeated (see
    /// [`act_until`]): long enough that a live stream answers first.
    const RETOUCH: Duration = Duration::from_millis(500);

    // SW-XS-12: these tests drive the real kernel watcher. They never assume
    // the stream is attached after a fixed sleep or that an event lands
    // within a fixed window: a stimulus is repeated until it is observed
    // ([`act_until`], bounded by [`WATCH_DEADLINE`]), and an absence is
    // asserted only after a later event proves the earlier one would already
    // have been delivered.

    #[test]
    fn watcher_dispatches_writes_to_handler() {
        let dir = TempDir::new().unwrap();
        let dispatcher = Arc::new(Dispatcher::new(50, 100, 60_000, 60_000));
        let (handler, log) = recorder();
        dispatcher.register_handler(dir.path(), handler);

        let _watcher = Watcher::new(
            WatchSetup {
                dirs: vec![WatchDir {
                    path: dir.path().to_path_buf(),
                    recursive: false,
                }],
            },
            Arc::clone(&dispatcher),
        )
        .unwrap();

        assert!(
            act_until(
                RETOUCH,
                || fs::write(dir.path().join("trigger.txt"), b"x").unwrap(),
                || !log.lock().unwrap().is_empty(),
            ),
            "expected at least one dispatch within {WATCH_DEADLINE:?}"
        );
    }

    #[test]
    fn watcher_dispatches_same_directory_atomic_replacements_and_delete() {
        let dir = TempDir::new().unwrap();
        let dispatcher = Arc::new(Dispatcher::new(1, 100, 60_000, 60_000));
        let (handler, log) = recorder();
        dispatcher.register_handler(dir.path(), handler);
        let _watcher = Watcher::new(
            WatchSetup {
                dirs: vec![WatchDir {
                    path: dir.path().to_path_buf(),
                    recursive: false,
                }],
            },
            Arc::clone(&dispatcher),
        )
        .unwrap();
        let observed_dir = std::fs::canonicalize(dir.path()).unwrap();
        let target = dir.path().join("pack.yml");
        let first = dir.path().join("pack.yml.1.tmp");
        let observed_target = observed_dir.join("pack.yml");
        let observed_first = observed_dir.join("pack.yml.1.tmp");
        // Repeating the first replacement is idempotent; it also waits out
        // the stream's attach.
        assert!(
            act_until(
                RETOUCH,
                || {
                    fs::write(&first, b"first").unwrap();
                    fs::rename(&first, &target).unwrap();
                },
                || log.lock().unwrap().iter().any(|path| {
                    path == &observed_first || path == &observed_target || path == &observed_dir
                }),
            ),
            "unexpected first replacement paths: {:?}",
            log.lock().unwrap()
        );
        let first_count = log.lock().unwrap().len();

        let second = dir.path().join("pack.yml.2.tmp");
        let observed_second = observed_dir.join("pack.yml.2.tmp");
        fs::write(&second, b"second").unwrap();
        fs::rename(&second, &target).unwrap();
        assert!(eventually(WATCH_DEADLINE, || {
            let paths = log.lock().unwrap();
            paths.len() > first_count
                && paths[first_count..].iter().any(|path| {
                    path == &observed_second || path == &observed_target || path == &observed_dir
                })
        }));
        let second_count = log.lock().unwrap().len();

        fs::remove_file(&target).unwrap();
        assert!(eventually(WATCH_DEADLINE, || {
            let paths = log.lock().unwrap();
            paths.len() > second_count
                && paths[second_count..]
                    .iter()
                    .any(|path| path == &observed_target || path == &observed_dir)
        }));
    }

    #[test]
    fn suppression_holds_across_kernel_event_latency() {
        // Iter-2 design property: the guard's `valid_until` outlives the
        // write call, so FSEvents (which can deliver tens to hundreds of
        // ms after the syscall returns — seconds on a loaded machine) still
        // sees the path as suppressed when the dispatcher consults
        // `is_suppressed`. The grace window here outlasts the test's own
        // deadline, so no delivery delay can outrun it; the suppression is
        // dropped immediately after the write.
        let dir = TempDir::new().unwrap();
        let grace_ms = 2 * WATCH_DEADLINE.as_millis() as u64;
        let dispatcher = Arc::new(Dispatcher::new(1, 100, 60_000, grace_ms));
        let (handler, log) = recorder();
        dispatcher.register_handler(dir.path(), handler);

        let _watcher = Watcher::new(
            WatchSetup {
                dirs: vec![WatchDir {
                    path: dir.path().to_path_buf(),
                    recursive: false,
                }],
            },
            Arc::clone(&dispatcher),
        )
        .unwrap();
        // The stream is live before the suppressed write.
        assert!(act_until(
            RETOUCH,
            || fs::write(dir.path().join("probe.txt"), b"p").unwrap(),
            || saw(&log, "probe.txt"),
        ));

        let blocked = dir.path().join("blocked.txt");
        let allowed = dir.path().join("allowed.txt");

        {
            // Drop is no-op under timestamped suppression; the entry
            // survives this scope.
            let _guard = Suppression::new(
                &dispatcher.suppression_handle(),
                std::iter::once(blocked.clone()),
            );
            fs::write(&blocked, b"x").unwrap();
        }

        // An un-suppressed write AFTER the blocked one: once it dispatches,
        // the stream has delivered the blocked write's event too. Under load
        // FSEvents may coalesce it into an event for the directory itself, so
        // either counts — after the write, never before. Under coalescing the
        // absence check below can pass vacuously; the dispatcher-level
        // suppression tests in dispatcher.rs are the authoritative check.
        let observed_dir = std::fs::canonicalize(dir.path()).unwrap();
        let mark = log.lock().unwrap().len();
        let mut n = 0;
        assert!(
            act_until(
                RETOUCH,
                || {
                    n += 1;
                    fs::write(&allowed, n.to_string()).unwrap()
                },
                || log.lock().unwrap()[mark..]
                    .iter()
                    .any(|p| p.ends_with("allowed.txt") || p == &observed_dir),
            ),
            "expected dispatch for un-suppressed path: {:?}",
            log.lock().unwrap()
        );
        assert!(
            !saw(&log, "blocked.txt"),
            "suppressed write must not dispatch even after the guard's scope ends, within grace window: {:?}",
            log.lock().unwrap()
        );
    }

    #[test]
    fn watch_adds_directory_after_construction() {
        // SS-4a AC-1: incremental watch after Watcher::new must
        // actually attach to notify and dispatch events for files
        // inside the newly-watched dir.
        let initial = TempDir::new().unwrap();
        let added = TempDir::new().unwrap();

        let dispatcher = Arc::new(Dispatcher::new(50, 100, 60_000, 60_000));
        let (handler, log) = recorder();
        dispatcher.register_handler(added.path(), handler);

        let watcher = Watcher::new(
            WatchSetup {
                dirs: vec![WatchDir {
                    path: initial.path().to_path_buf(),
                    recursive: false,
                }],
            },
            Arc::clone(&dispatcher),
        )
        .unwrap();

        // Initially no handler for the "added" dir and dir not watched.
        // Add it now via the post-construction method.
        let _outcome = watcher
            .watch(WatchDir {
                path: added.path().to_path_buf(),
                recursive: false,
            })
            .expect("post-construction watch must succeed");

        assert!(
            act_until(
                RETOUCH,
                || fs::write(added.path().join("after.txt"), b"x").unwrap(),
                || saw(&log, "after.txt"),
            ),
            "expected dispatch for post-construction watch within {WATCH_DEADLINE:?}: {:?}",
            log.lock().unwrap()
        );
    }

    #[test]
    fn unwatch_stops_dispatch_for_directory() {
        // SS-4a AC-1: after unwatch, file events under the detached dir
        // must no longer dispatch. A second, still-watched dir is the
        // barrier: its event, written after the detached dir's, arrives
        // only once the stream has delivered everything before it.
        let dir = TempDir::new().unwrap();
        let barrier = TempDir::new().unwrap();
        let dispatcher = Arc::new(Dispatcher::new(0, 100, 60_000, 60_000));
        let (handler, log) = recorder();
        dispatcher.register_handler(dir.path(), Arc::clone(&handler));
        dispatcher.register_handler(barrier.path(), handler);

        let watcher = Watcher::new(
            WatchSetup {
                dirs: vec![
                    WatchDir {
                        path: dir.path().to_path_buf(),
                        recursive: false,
                    },
                    WatchDir {
                        path: barrier.path().to_path_buf(),
                        recursive: false,
                    },
                ],
            },
            Arc::clone(&dispatcher),
        )
        .unwrap();

        // Sanity: watch is live → file write dispatches.
        assert!(
            act_until(
                RETOUCH,
                || fs::write(dir.path().join("before.txt"), b"x").unwrap(),
                || saw(&log, "before.txt"),
            ),
            "sanity: watched dir must dispatch before unwatch"
        );

        // Detach.
        watcher.unwatch(dir.path()).expect("unwatch must succeed");

        // Write into the detached dir, then into the barrier dir; once the
        // barrier's event arrives, an event for the detached write would
        // have too.
        fs::write(dir.path().join("after.txt"), b"y").unwrap();
        let observed_barrier = std::fs::canonicalize(barrier.path()).unwrap();
        let mark = log.lock().unwrap().len();
        let mut n = 0;
        assert!(
            act_until(
                RETOUCH,
                || {
                    n += 1;
                    fs::write(barrier.path().join("barrier.txt"), n.to_string()).unwrap()
                },
                // A coalesced event for the barrier dir itself counts too,
                // which can make the absence check below vacuous;
                // `route_event_never_delivers_for_a_torn_down_directory` is
                // the deterministic, authoritative check.
                || log.lock().unwrap()[mark..]
                    .iter()
                    .any(|p| p.ends_with("barrier.txt") || p == &observed_barrier),
            ),
            "the still-watched barrier dir must dispatch"
        );
        assert!(
            !saw(&log, "after.txt"),
            "unwatch must stop dispatch: {:?}",
            log.lock().unwrap()
        );
    }

    /// SW-XS-12 (review): the authoritative absence check for teardown. The
    /// real-watcher tests above accept a coalesced directory-level event as
    /// their barrier, so under coalescing their "never dispatched" checks can
    /// pass vacuously. This drives [`route_event`] directly with synthetic
    /// `notify` events — a file event, a coalesced event naming the directory
    /// itself, and a rescan-flagged event — for a directory torn down the way
    /// production removes a watch (`unregister_handler` + `unwatch`), and
    /// requires that none of them reaches a handler while the same shapes for
    /// a still-watched sibling all do.
    #[test]
    fn route_event_never_delivers_for_a_torn_down_directory() {
        use notify::event::{Flag, ModifyKind};
        use notify::{Event, EventKind};

        let gone = PathBuf::from("/xs12-unwatched");
        let kept = PathBuf::from("/xs12-still-watched");
        let dispatcher = Dispatcher::new(0, 100, 60_000, 60_000);
        let (handler, log) = recorder();
        let (rescan, rescan_log) = recorder();
        for dir in [&gone, &kept] {
            let _ = dispatcher.register_handler(dir, Arc::clone(&handler));
            dispatcher.register_rescan_handler(dir, Arc::clone(&rescan));
        }

        let modify =
            |p: &Path| Ok(Event::new(EventKind::Modify(ModifyKind::Any)).add_path(p.to_path_buf()));
        let rescan_of = |p: &Path| {
            Ok(Event::new(EventKind::Other)
                .add_path(p.to_path_buf())
                .set_flag(Flag::Rescan))
        };

        // Live: every shape is delivered for the torn-down dir before teardown.
        route_event(&dispatcher, modify(&gone.join("before.txt")));
        assert_eq!(*log.lock().unwrap(), vec![gone.join("before.txt")]);

        // Teardown as production does it; the kernel half is a no-op here
        // (nothing attached), the dispatcher half is what stops delivery of
        // any event still in flight from the kernel.
        dispatcher.unregister_handler(&gone);
        let watcher = Watcher::new(
            WatchSetup { dirs: vec![] },
            Arc::new(Dispatcher::new(0, 100, 60_000, 60_000)),
        )
        .unwrap();
        watcher
            .unwatch(&gone)
            .expect("unwatch of an unattached dir is a no-op");
        log.lock().unwrap().clear();

        for dir in [&gone, &kept] {
            route_event(&dispatcher, modify(&dir.join("after.txt")));
            route_event(&dispatcher, modify(dir));
            route_event(&dispatcher, rescan_of(&dir.join("sub")));
        }

        let delivered = log.lock().unwrap().clone();
        assert_eq!(
            delivered,
            vec![kept.join("after.txt"), kept.clone()],
            "only the still-watched dir's file and coalesced dir-level events dispatch"
        );
        assert_eq!(
            *rescan_log.lock().unwrap(),
            vec![kept.join("sub")],
            "a rescan for the torn-down dir reaches no handler"
        );
        assert!(
            !delivered
                .iter()
                .chain(rescan_log.lock().unwrap().iter())
                .any(|p| p.starts_with(&gone)),
            "nothing under the torn-down dir is delivered"
        );
    }

    #[test]
    fn watch_is_idempotent_on_duplicate_dir() {
        // SS-4a AC-1 + iter-4 Y1: post-construction watch with the
        // same canonical path + recursive flag must (a) be a silent
        // no-op (not surface a notify error) and (b) return
        // `WatchOutcome::AlreadyAttached` so callers in a transactional
        // batch can skip the rollback for that key.
        let dir = TempDir::new().unwrap();
        let dispatcher = Arc::new(Dispatcher::new(50, 100, 60_000, 60_000));
        let watcher = Watcher::new(WatchSetup { dirs: vec![] }, Arc::clone(&dispatcher)).unwrap();

        let wd = WatchDir {
            path: dir.path().to_path_buf(),
            recursive: false,
        };
        let first = watcher.watch(wd.clone()).expect("first watch must succeed");
        assert!(
            matches!(first, WatchOutcome::Attached(_)),
            "first watch must return Attached, got {:?}",
            first
        );

        let second = watcher
            .watch(wd)
            .expect("duplicate watch must be idempotent");
        assert!(
            matches!(second, WatchOutcome::AlreadyAttached(_)),
            "duplicate watch must return AlreadyAttached (so rollback knows not to unwatch); got {:?}",
            second
        );
        // The canonical path on both outcomes refers to the same key.
        assert_eq!(first.canon(), second.canon());
    }

    #[test]
    fn unwatch_is_idempotent_on_unknown_dir() {
        // SS-4a AC-1: unwatch against a path the substrate never
        // attached must not error. Maps notify's WatchNotFound → Ok.
        let dir = TempDir::new().unwrap();
        let dispatcher = Arc::new(Dispatcher::new(50, 100, 60_000, 60_000));
        let watcher = Watcher::new(WatchSetup { dirs: vec![] }, Arc::clone(&dispatcher)).unwrap();

        watcher
            .unwatch(dir.path())
            .expect("unwatch of never-attached path must be idempotent");
    }

    #[test]
    fn duplicate_watch_dirs_are_deduplicated() {
        let dir = TempDir::new().unwrap();
        let dispatcher = Arc::new(Dispatcher::new(50, 100, 60_000, 60_000));
        // Same dir + same recursive flag listed twice. Without dedup the
        // second `inner.watch` call would error.
        let setup = WatchSetup {
            dirs: vec![
                WatchDir {
                    path: dir.path().to_path_buf(),
                    recursive: false,
                },
                WatchDir {
                    path: dir.path().to_path_buf(),
                    recursive: false,
                },
            ],
        };
        let _ = Watcher::new(setup, dispatcher).expect("duplicate dirs must not fail");
    }
}
