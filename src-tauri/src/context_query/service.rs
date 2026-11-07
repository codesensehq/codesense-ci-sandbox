// SPDX-License-Identifier: MIT
//! The running index: one background worker that owns the [`IndexState`],
//! builds it at startup, applies watcher-reported paths (debounced), and
//! reconciles every root on an interval (catching content no watcher covers,
//! like another org's repo after a git pull, and root-set changes such as a
//! newly synced team). Every change publishes a new snapshot and fires the
//! change callback (the app emits [`CHANGE_EVENT`]).
//!
//! Readers call [`ContextIndexService::snapshot`] — an `Arc` clone under a brief
//! read lock — and query with no lock held, off the UI thread.
//!
//! A cursor walk reads one snapshot: the generation that answered its first
//! page stays pinned (at most [`MAX_PINS`], each [`PIN_IDLE`] after its last
//! page and [`PIN_MAX`] at most), so its later pages come from it while the
//! index moves on. Past that, the cursor is stale. A `text` walk pages only the
//! current generation.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::engine::{self, QueryError, Viewer};
use super::index::{changed_orgs, IndexRoot, IndexState, Snapshot};
use super::model::{ContextQuery, ContextQueryResult, Layer};

/// The Tauri event fired after the index changes.
pub const CHANGE_EVENT: &str = "context-index-changed";

/// Quiet period that coalesces a burst of watcher events into one refresh.
const DEBOUNCE: Duration = Duration::from_millis(150);
/// How often every root is re-walked (mtime-diffed, so cheap when idle).
const RECONCILE_INTERVAL: Duration = Duration::from_secs(60);
/// Paths carried in one change event, at most (consumers re-query anyway).
const MAX_EVENT_PATHS: usize = 200;
/// Queued watcher paths, at most; past it the queue collapses into one re-walk.
const MAX_PENDING_PATHS: usize = 20_000;
/// Snapshot generations kept for cursor walks, at most; a new pin evicts the
/// least recently paged one.
pub const MAX_PINS: usize = 4;
/// A pinned generation is dropped this long after its last page …
pub const PIN_IDLE: Duration = Duration::from_secs(30);
/// … and this long after it was pinned, however often it is paged.
pub const PIN_MAX: Duration = Duration::from_secs(120);

/// What changed, for subscribers. Paths are capped; orgs are complete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexChange {
    pub generation: u64,
    pub paths: Vec<String>,
    /// Every org with a changed file.
    pub orgs: Vec<String>,
    /// True when all-orgs content changed (it shows in every org).
    pub all_orgs: bool,
    /// True when `paths` was capped, or when the whole index was replaced (a
    /// profile switch): subscribers should re-run whatever their scope.
    pub truncated: bool,
}

/// One profile's footprint: whose it is and its content roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Footprint {
    pub owner: u64,
    pub roots: Vec<IndexRoot>,
    /// SW-DNS-2: the configured summary headings rows' `summary` follows
    /// (empty: the defaults).
    pub summary_headings: Vec<String>,
}

/// The selected profile's footprint, or `None` while no profile is selected —
/// the index is then empty and keeps reporting `indexing`.
pub type RootsFn = Arc<dyn Fn() -> Option<Footprint> + Send + Sync>;
#[derive(Clone)]
pub struct SnapshotChange {
    pub observer_id: String,
    /// The footprint that produced these snapshots, before any later profile switch.
    pub owner: Option<u64>,
    pub epoch: u64,
    pub previous: Arc<Snapshot>,
    pub current: Arc<Snapshot>,
    pub change: IndexChange,
    pub replaced: bool,
}
pub type ObserveFn = Arc<dyn Fn(SnapshotChange) + Send + Sync>;
pub type ChangeFn = Arc<dyn Fn(IndexChange) + Send + Sync>;

#[derive(Default)]
struct Pending {
    paths: Vec<PathBuf>,
    reconcile: bool,
    started: bool,
}

/// One generation a cursor walk is paging.
struct Pin {
    snapshot: Arc<Snapshot>,
    pinned_at: Instant,
    used_at: Instant,
}

#[derive(Default)]
struct Pins {
    kept: Vec<Pin>,
    /// The generation the index was last replaced at: nothing older is pinned
    /// again, so a query that raced the replacement cannot keep the old rows.
    floor: u64,
}

impl Pins {
    /// The pin of `generation`, marked used at `now`, once lapsed pins are dropped.
    fn touch_pin(&mut self, generation: u64, now: Instant) -> Option<&Pin> {
        Vec::retain(&mut self.kept, |p| {
            now.saturating_duration_since(p.used_at) < PIN_IDLE
                && now.saturating_duration_since(p.pinned_at) < PIN_MAX
        });
        let pin = Iterator::find(&mut self.kept.iter_mut(), |p| {
            p.snapshot.generation == generation
        })?;
        pin.used_at = now;
        Some(pin)
    }
}

#[derive(Default)]
struct Inner {
    observer_id: std::sync::OnceLock<String>,
    snapshot: RwLock<Arc<Snapshot>>,
    pins: Mutex<Pins>,
    /// Added to the clock, so tests can let pins lapse.
    #[cfg(test)]
    clock_skew: Mutex<Duration>,
    pending: Mutex<Pending>,
    wake: Condvar,
    /// Signalled on every publish, for [`ContextIndexService::wait_ready`].
    ready_gate: Mutex<()>,
    ready_cv: Condvar,
    /// Bumped by [`ContextIndexService::reset`]; a worker pass that started
    /// before a reset never publishes.
    epoch: std::sync::atomic::AtomicU64,
}

/// The shared index handle (Tauri managed state).
#[derive(Clone, Default)]
pub struct ContextIndexService {
    inner: Arc<Inner>,
}

impl ContextIndexService {
    pub fn new() -> Self {
        Self::default()
    }

    /// The current snapshot. Never blocks on indexing.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        Arc::clone(
            &self
                .inner
                .snapshot
                .read()
                .unwrap_or_else(|p| p.into_inner()),
        )
    }

    /// Run `q` as `viewer` through `run`, over the snapshot it pages: the
    /// current one for a first page, else the generation its cursor came from
    /// while that is pinned (paging it keeps the pin). `StaleCursor` once that
    /// generation is gone. Only a first page with a next page pins, so a walk's
    /// pin is never made again once it lapsed or was evicted. A `text` query
    /// matches file bodies as they are now, so it pages only the current
    /// generation and is never pinned.
    pub fn query_with(
        &self,
        q: &ContextQuery,
        viewer: &Viewer,
        run: impl FnOnce(&Snapshot) -> Result<ContextQueryResult, QueryError>,
    ) -> Result<ContextQueryResult, QueryError> {
        let generation = engine::cursor_generation(q, viewer)?;
        let current = self.snapshot();
        let snapshot = match generation {
            None => current,
            Some(g) if g == current.generation => {
                self.pins().touch_pin(g, self.pin_clock());
                current
            }
            Some(_) if q.text.is_some() => return Err(QueryError::StaleCursor),
            Some(g) => self
                .pins()
                .touch_pin(g, self.pin_clock())
                .map(|pin| Arc::clone(&pin.snapshot))
                .ok_or(QueryError::StaleCursor)?,
        };
        let result = run(&snapshot)?;
        if generation.is_none() && q.text.is_none() && result.next_cursor.is_some() {
            self.pin_first_page(&snapshot);
        }
        Ok(result)
    }

    /// Pin `generation` for a walk whose first page its caller cut itself (a
    /// byte cut), while it is still current.
    pub fn keep_walk(&self, generation: u64) {
        let current = self.snapshot();
        if current.generation == generation {
            self.pin_first_page(&current);
        }
    }

    fn pin_first_page(&self, snapshot: &Arc<Snapshot>) {
        let now = self.pin_clock();
        let mut pins = self.pins();
        if snapshot.generation < pins.floor || pins.touch_pin(snapshot.generation, now).is_some() {
            return;
        }
        if pins.kept.len() >= MAX_PINS {
            let lru = (0..pins.kept.len()).min_by_key(|&i| pins.kept[i].used_at);
            if let Some(lru) = lru {
                pins.kept.remove(lru);
            }
        }
        pins.kept.push(Pin {
            snapshot: Arc::clone(snapshot),
            pinned_at: now,
            used_at: now,
        });
    }

    fn pins(&self) -> MutexGuard<'_, Pins> {
        self.inner.pins.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn pin_clock(&self) -> Instant {
        let now = Instant::now();
        #[cfg(test)]
        let now = now
            + *self
                .inner
                .clock_skew
                .lock()
                .unwrap_or_else(|e| e.into_inner());
        now
    }

    /// Move the pins' clock forward by `by`.
    #[cfg(test)]
    pub(crate) fn advance_clock(&self, by: Duration) {
        *self
            .inner
            .clock_skew
            .lock()
            .unwrap_or_else(|e| e.into_inner()) += by;
    }

    /// A running index over `roots`, once its first build has published.
    #[cfg(test)]
    pub(crate) fn started(roots: Vec<IndexRoot>) -> Self {
        let svc = Self::new();
        svc.start(
            Arc::new(move || {
                Some(Footprint {
                    owner: 1,
                    roots: roots.clone(),
                    summary_headings: Vec::new(),
                })
            }),
            Arc::new(|_| {}),
        );
        assert!(
            svc.wait_ready(Duration::from_secs(10)).is_some(),
            "the index never became ready"
        );
        svc
    }

    /// Report `paths` changed and wait until the index published past `generation`.
    #[cfg(test)]
    pub(crate) fn settle(&self, paths: Vec<PathBuf>, generation: u64) {
        self.note_paths(paths);
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.snapshot().generation <= generation {
            assert!(
                Instant::now() < deadline,
                "the index never moved past generation {generation}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// About how many bytes the pins hold of their own: each snapshot's record
    /// list and relations (the records themselves are shared `Arc`s).
    #[cfg(test)]
    pub(crate) fn pinned_bytes(&self) -> usize {
        use std::mem::size_of_val;
        self.pins()
            .kept
            .iter()
            .map(|p| {
                let r = &p.snapshot.relations;
                let lists = [
                    &r.supporting_docs,
                    &r.backlinks,
                    &r.depends_on,
                    &r.dependents,
                ]
                .iter()
                .map(|l| size_of_val(&l[..]) + l.iter().map(|v| size_of_val(&v[..])).sum::<usize>())
                .sum::<usize>();
                let epics = size_of_val(&r.folder_epic[..])
                    + r.folder_epic
                        .iter()
                        .flatten()
                        .map(String::len)
                        .sum::<usize>();
                size_of_val(&p.snapshot.records[..]) + lists + epics
            })
            .sum()
    }

    /// Generations pinned now.
    #[cfg(test)]
    pub(crate) fn pinned(&self) -> Vec<u64> {
        self.pins()
            .kept
            .iter()
            .map(|p| p.snapshot.generation)
            .collect()
    }

    /// The snapshot once the first full build has published, waiting up to
    /// `timeout` for it; `None` when it never became ready in time.
    pub fn wait_ready(&self, timeout: Duration) -> Option<Arc<Snapshot>> {
        let deadline = std::time::Instant::now() + timeout;
        let mut gate = self
            .inner
            .ready_gate
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        loop {
            let snap = self.snapshot();
            if snap.ready {
                return Some(snap);
            }
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return None;
            }
            gate = self
                .inner
                .ready_cv
                .wait_timeout(gate, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Whether an observation still belongs to this index since its last reset.
    pub fn is_current(&self, change: &SnapshotChange) -> bool {
        self.inner.observer_id.get() == Some(&change.observer_id)
            && self.inner.epoch.load(std::sync::atomic::Ordering::SeqCst) == change.epoch
    }

    /// Queue paths a watcher reported. Cheap; the worker applies them.
    pub fn note_paths(&self, paths: impl IntoIterator<Item = PathBuf>) {
        let mut p = self.inner.pending.lock().unwrap_or_else(|e| e.into_inner());
        p.paths.extend(paths);
        if p.paths.len() > MAX_PENDING_PATHS {
            // A storm of events (or a stalled worker): one re-walk covers them all.
            p.paths.clear();
            p.reconcile = true;
        }
        self.inner.wake.notify_one();
    }

    /// The selected profile changed (selected, cleared, revoked) or an org was
    /// attached/detached: stop serving the old footprint at once — publish an
    /// empty, still-indexing snapshot — and re-walk for the new one.
    pub fn reset(&self) {
        // Bump the epoch under the snapshot write lock: `publish` re-checks the
        // epoch under that same lock, so a pass begun before this reset can
        // never install its (old-footprint) snapshot after it.
        {
            let mut snap = self
                .inner
                .snapshot
                .write()
                .unwrap_or_else(|p| p.into_inner());
            self.inner
                .epoch
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let generation = snap.generation + 1;
            *snap = Arc::new(Snapshot::build(Vec::new(), Vec::new(), generation, false));
            drop_pins(&self.inner, generation);
        }
        let mut p = self.inner.pending.lock().unwrap_or_else(|e| e.into_inner());
        p.paths.clear();
        p.reconcile = true;
        self.inner.wake.notify_one();
    }

    /// Ask for a full re-walk soon (e.g. after the synced team set changed).
    pub fn request_reconcile(&self) {
        let mut p = self.inner.pending.lock().unwrap_or_else(|e| e.into_inner());
        p.reconcile = true;
        self.inner.wake.notify_one();
    }

    /// Start with an internal observer that receives both parsed snapshots.
    pub fn start_observed(&self, roots: RootsFn, on_change: ChangeFn, observe: ObserveFn) {
        self.start_worker(roots, on_change, Some(observe));
    }

    /// Start the worker (idempotent).
    pub fn start(&self, roots: RootsFn, on_change: ChangeFn) {
        self.start_worker(roots, on_change, None);
    }

    fn start_worker(&self, roots: RootsFn, on_change: ChangeFn, observe: Option<ObserveFn>) {
        {
            let mut p = self.inner.pending.lock().unwrap_or_else(|e| e.into_inner());
            if p.started {
                return;
            }
            p.started = true;
        }
        let inner = Arc::clone(&self.inner);
        std::thread::Builder::new()
            .name("context-index".into())
            .spawn(move || {
                worker(
                    &inner,
                    roots.as_ref(),
                    on_change.as_ref(),
                    observe.as_deref(),
                )
            })
            .map(|_| ())
            .unwrap_or_else(
                |e| log::error!(target: "context_query", "index worker failed to start: {e}"),
            );
    }
}

/// The index was replaced at `generation`: no cursor pages the old rows again.
fn drop_pins(inner: &Inner, generation: u64) {
    let mut pins = inner.pins.lock().unwrap_or_else(|e| e.into_inner());
    pins.kept.clear();
    pins.floor = generation;
}

/// Install `state` as the served snapshot and announce the change — unless a
/// reset happened since the pass began (`epoch` is stale), in which case
/// nothing is installed and `false` is returned.
fn publish(
    inner: &Inner,
    state: &IndexState,
    ready: bool,
    changed: &[PathBuf],
    replaced: bool,
    owner: Option<u64>,
    epoch: u64,
    on_change: &dyn Fn(IndexChange),
    observe: Option<&(dyn Fn(SnapshotChange) + Send + Sync)>,
) -> bool {
    let (previous, current) = {
        let mut snap = inner.snapshot.write().unwrap_or_else(|p| p.into_inner());
        if inner.epoch.load(std::sync::atomic::Ordering::SeqCst) != epoch {
            return false;
        }
        let generation = snap.generation + 1;
        let current = Arc::new(state.snapshot(generation, ready));
        let previous = std::mem::replace(&mut *snap, Arc::clone(&current));
        if replaced {
            drop_pins(inner, generation);
        }
        (previous, current)
    };
    // Taking the gate orders this after any waiter's `ready` check, so the
    // wake-up cannot be lost.
    drop(inner.ready_gate.lock().unwrap_or_else(|e| e.into_inner()));
    inner.ready_cv.notify_all();
    let (orgs, all_orgs) = changed_orgs(state, changed);
    let change = IndexChange {
        generation: current.generation,
        paths: changed
            .iter()
            .take(MAX_EVENT_PATHS)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .collect(),
        orgs,
        all_orgs,
        truncated: replaced || changed.len() > MAX_EVENT_PATHS,
    };
    if let Some(observe) = observe {
        observe(SnapshotChange {
            observer_id: inner
                .observer_id
                .get_or_init(|| uuid::Uuid::new_v4().to_string())
                .clone(),
            owner,
            epoch,
            previous,
            current,
            change: change.clone(),
            replaced,
        });
    }
    on_change(change);
    true
}

/// What one pass of the worker does.
enum Step {
    Reconcile,
    Paths(Vec<PathBuf>),
}

/// Block until there is work: queued paths, a requested re-walk, or an overdue
/// periodic re-walk (overdue counts from the last re-walk, so a steady stream
/// of watcher events can't starve it). Then let a burst settle and take it all.
fn next_step(inner: &Inner, last_reconcile: std::time::Instant) -> Step {
    let mut p = inner.pending.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        let overdue = last_reconcile.elapsed() >= RECONCILE_INTERVAL;
        if overdue || p.reconcile || !p.paths.is_empty() {
            if overdue {
                p.reconcile = true;
            }
            break;
        }
        let wait = RECONCILE_INTERVAL.saturating_sub(last_reconcile.elapsed());
        p = inner
            .wake
            .wait_timeout(p, wait)
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
    drop(p);
    std::thread::sleep(DEBOUNCE);
    let mut p = inner.pending.lock().unwrap_or_else(|e| e.into_inner());
    let paths = std::mem::take(&mut p.paths);
    if std::mem::take(&mut p.reconcile) {
        Step::Reconcile
    } else {
        Step::Paths(paths)
    }
}

fn worker(
    inner: &Inner,
    roots: &(dyn Fn() -> Option<Footprint> + Send + Sync),
    on_change: &dyn Fn(IndexChange),
    observe: Option<&(dyn Fn(SnapshotChange) + Send + Sync)>,
) {
    use std::sync::atomic::Ordering::SeqCst;
    let mut state = IndexState::default();
    let mut owner: Option<u64> = None;
    let mut ready = false;
    let mut last_reconcile = std::time::Instant::now();
    let mut step = Step::Reconcile;
    let mut worker_epoch = inner.epoch.load(SeqCst);

    loop {
        let started = std::time::Instant::now();
        let epoch = inner.epoch.load(SeqCst);
        if epoch != worker_epoch {
            // Even an unchanged footprint must publish a new ready baseline.
            state = IndexState::default();
            owner = None;
            ready = false;
            worker_epoch = epoch;
        }
        let reconciling = matches!(step, Step::Reconcile);
        // A panic on one odd file (or in publishing) must not end indexing for
        // the session: drop the state and re-walk everything on the next pass.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut replaced = false;
            let changed = match step {
                Step::Reconcile => match roots() {
                    Some(footprint) => {
                        if owner != Some(footprint.owner) {
                            // Another profile: never carry the previous one's rows.
                            state = IndexState::default();
                            owner = Some(footprint.owner);
                            replaced = true;
                        }
                        state.set_summary_headings(footprint.summary_headings);
                        let changed = state.reconcile(footprint.roots);
                        let first = !ready;
                        ready = true;
                        if first {
                            log::info!(
                                target: "context_query",
                                "context index built: {} files under {} roots in {:?}",
                                state.len(),
                                state.roots().len(),
                                started.elapsed()
                            );
                        }
                        // The first build publishes even when empty, so queries
                        // stop reporting `indexing`.
                        return (first || replaced || !changed.is_empty())
                            .then_some((changed, replaced));
                    }
                    None => {
                        // No profile: serve nothing.
                        let had = owner.is_some() || state.len() > 0 || ready;
                        state = IndexState::default();
                        owner = None;
                        ready = false;
                        return had.then_some((Vec::new(), true));
                    }
                },
                Step::Paths(paths) => state.refresh_paths(&dedup(paths)),
            };
            (!changed.is_empty()).then_some((changed, replaced))
        }));
        match outcome {
            // A reset raced this pass: its result belongs to the old footprint.
            _ if inner.epoch.load(SeqCst) != epoch => {
                state = IndexState::default();
                owner = None;
                ready = false;
            }
            Ok(Some((changed, replaced))) => {
                let published = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    publish(
                        inner, &state, ready, &changed, replaced, owner, epoch, on_change, observe,
                    )
                }));
                if matches!(published, Ok(false)) {
                    // A reset landed between the pass and its publish: drop it.
                    state = IndexState::default();
                    owner = None;
                    ready = false;
                }
                if published.is_err() {
                    log::error!(target: "context_query", "publishing the index panicked; scheduling a full re-walk");
                    inner
                        .pending
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .reconcile = true;
                }
            }
            Ok(None) => {}
            Err(_) => {
                log::error!(target: "context_query", "index update panicked; scheduling a full re-walk");
                state = IndexState::default();
                owner = None;
                inner
                    .pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .reconcile = true;
            }
        }
        if reconciling {
            last_reconcile = std::time::Instant::now();
        }
        step = next_step(inner, last_reconcile);
    }
}

fn dedup(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.sort();
    paths.dedup();
    paths
}

/// Every content root in the user's footprint: each synced team's context
/// repo, the private space of each of those orgs, and all-orgs content.
/// Resolved through the existing resolvers (synced teams, scratch space).
pub fn footprint_roots(codesense_home: &Path, user_id: u64) -> Vec<IndexRoot> {
    use crate::context_repo::scratch_space;
    use crate::domain::Principal;

    let principal = Principal::from(user_id);
    let mut roots = Vec::new();
    let mut orgs = std::collections::BTreeSet::new();
    let contexts = crate::commands::route_to_context::synced_contexts_in(codesense_home, user_id)
        .unwrap_or_else(|e| {
            log::warn!(target: "context_query", "listing synced teams failed: {e}");
            Vec::new()
        });
    for ctx in contexts {
        let (Some(dir), Some(team)) = (ctx.team_content_dir(), ctx.team_name.clone()) else {
            continue;
        };
        orgs.insert(ctx.org_slug.clone());
        if let Some(scope_root) = dir.parent() {
            add_collection_roots(
                &mut roots,
                scope_root,
                crate::records::Scope::Team,
                Some(ctx.org_slug.clone()),
                Some(team.clone()),
            );
        }
        roots.push(IndexRoot {
            dir,
            org: Some(ctx.org_slug),
            team: Some(team),
            layer: Layer::Team,
        });
    }
    for org in orgs {
        if let Some(space) = scratch_space::org_space(codesense_home, &principal, &org) {
            add_collection_roots(
                &mut roots,
                &space.root(),
                crate::records::Scope::Private,
                Some(org.clone()),
                None,
            );
            roots.push(IndexRoot {
                dir: space.content_dir(),
                org: Some(org),
                team: None,
                layer: Layer::Private,
            });
        }
    }
    let all_orgs = scratch_space::scratch_all_orgs_root(codesense_home, &principal);
    roots.push(IndexRoot {
        dir: all_orgs,
        org: None,
        team: None,
        layer: Layer::AllOrgs,
    });
    roots.retain(|r| r.dir.is_dir());
    roots
}

/// Collections may live beside content, so the same footprint indexes their folders.
fn add_collection_roots(
    roots: &mut Vec<IndexRoot>,
    root: &Path,
    scope: crate::records::Scope,
    org: Option<String>,
    team: Option<String>,
) {
    let collections = crate::records::load_collections(root, scope).unwrap_or_else(|e| {
        log::warn!("collection index roots: {e}");
        if scope == crate::records::Scope::Private {
            vec![crate::records::Collection::notes()]
        } else {
            vec![]
        }
    });
    for collection in collections {
        if collection.folder.starts_with("content") {
            continue;
        }
        let Ok(dir) = crate::records::schema::guarded_path(root, &collection.folder) else {
            continue;
        };
        if !roots.iter().any(|r| r.dir == dir) {
            roots.push(IndexRoot {
                dir,
                org: org.clone(),
                team: team.clone(),
                layer: if scope == crate::records::Scope::Private {
                    Layer::Private
                } else {
                    Layer::Team
                },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    #[test]
    fn collection_roots_make_private_notes_and_declared_folders_queryable() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        write(
            &root.join("content/schema/journal.yaml"),
            "collection: journal\nfolder: journal\nid: slug\n",
        );
        write(
            &root.join("notes/abc-1.md"),
            "---\ntitle: Private memo\n---\nFind this memo",
        );
        write(&root.join("journal/day.md"), "Find this journal");
        let mut roots = vec![];
        add_collection_roots(
            &mut roots,
            &root,
            crate::records::Scope::Private,
            Some("acme/ctx".into()),
            None,
        );
        assert_eq!(roots.len(), 2);
        assert!(roots
            .iter()
            .all(|r| r.layer == Layer::Private && r.org.as_deref() == Some("acme/ctx")));
        let mut state = IndexState::default();
        state.reconcile(roots);
        let q = serde_json::from_value(
            serde_json::json!({"where":{"orgs":["acme/ctx"]},"text":"memo"}),
        )
        .unwrap();
        let result = state
            .snapshot(1, true)
            .query(
                &q,
                &crate::context_query::Viewer::default(),
                &crate::context_query::engine::DiskBodyReader,
            )
            .unwrap();
        assert_eq!(result.rows.len(), 1);
        assert!(result.rows[0].path.ends_with("notes/abc-1.md"));
    }

    #[test]
    fn worker_builds_then_applies_watcher_paths_and_fires_change() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("acme/default/content");
        write(
            &root.join("e/a.md"),
            "---\ntype: story\nstory_id: A-1\nstatus: todo\n---\n# A\n",
        );
        let roots_root = root.clone();
        let roots: RootsFn = Arc::new(move || {
            Some(Footprint {
                owner: 1,
                roots: vec![IndexRoot {
                    dir: roots_root.clone(),
                    org: Some("acme".into()),
                    team: Some("default".into()),
                    layer: Layer::Team,
                }],
                summary_headings: Vec::new(),
            })
        });
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let svc = ContextIndexService::new();
        svc.start_observed(
            roots,
            Arc::new(|_| {}),
            Arc::new(move |c| {
                assert!(c.current.ready);
                if c.current.generation == 1 {
                    assert!(!c.previous.ready);
                } else {
                    assert_eq!(c.previous.records.len(), 1);
                }
                let _ = tx.lock().unwrap().send(c.change);
            }),
        );
        let first = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(first.generation, 1);
        assert_eq!(svc.snapshot().records.len(), 1);

        write(
            &root.join("e/b.md"),
            "---\ntype: story\nstory_id: B-1\nstatus: done\n---\n",
        );
        svc.note_paths([root.join("e/b.md")]);
        let second = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(second.orgs, vec!["acme".to_string()]);
        assert!(second.paths[0].ends_with("e/b.md"));
        let snap = svc.snapshot();
        assert_eq!(snap.records.len(), 2);
        assert!(snap.ready);
    }

    #[test]
    fn a_profile_switch_drops_the_old_footprint_at_once_and_never_serves_it_again() {
        let tmp = tempfile::tempdir().unwrap();
        let alice = tmp.path().join("alice/scratch/orgs/acme/content");
        let bob = tmp.path().join("bob/scratch/orgs/acme/content");
        write(
            &alice.join("a.md"),
            "---\ntype: story\nstory_id: ALICE-PRIV\n---\n",
        );
        write(
            &bob.join("b.md"),
            "---\ntype: story\nstory_id: BOB-PRIV\n---\n",
        );
        // Which profile is selected: 0 = none, 1 = alice, 2 = bob.
        let who = Arc::new(std::sync::atomic::AtomicU64::new(1));
        let (w, a, b) = (Arc::clone(&who), alice.clone(), bob.clone());
        let roots: RootsFn = Arc::new(move || {
            let owner = w.load(std::sync::atomic::Ordering::SeqCst);
            let dir = match owner {
                1 => a.clone(),
                2 => b.clone(),
                _ => return None,
            };
            Some(Footprint {
                owner,
                roots: vec![IndexRoot {
                    dir,
                    org: Some("acme".into()),
                    team: None,
                    layer: Layer::Private,
                }],
                summary_headings: Vec::new(),
            })
        });
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let svc = ContextIndexService::new();
        svc.start(
            roots,
            Arc::new(move |c| {
                let _ = tx.lock().unwrap().send(c);
            }),
        );
        rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let ids = |svc: &ContextIndexService| -> Vec<String> {
            svc.snapshot()
                .records
                .iter()
                .map(|r| r.id().to_string())
                .collect()
        };
        assert_eq!(ids(&svc), vec!["ALICE-PRIV"]);

        // Switch to bob: the reset alone already serves nothing of alice's.
        who.store(2, std::sync::atomic::Ordering::SeqCst);
        svc.reset();
        assert!(ids(&svc).is_empty(), "nothing served between profiles");
        assert!(!svc.snapshot().ready);
        let change = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(change.truncated, "subscribers re-run on a replaced index");
        assert_eq!(ids(&svc), vec!["BOB-PRIV"]);

        // Deselect: the index empties.
        who.store(0, std::sync::atomic::Ordering::SeqCst);
        svc.reset();
        svc.request_reconcile();
        let _ = rx.recv_timeout(Duration::from_millis(1500));
        assert!(ids(&svc).is_empty());
    }

    #[test]
    fn a_pass_that_raced_a_reset_never_installs_its_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("old-profile/private");
        write(
            &root.join("p.md"),
            "---\ntype: story\nstory_id: OLD-PRIV\n---\n",
        );
        let mut state = IndexState::default();
        state.reconcile(vec![IndexRoot {
            dir: root,
            org: Some("acme".into()),
            team: None,
            layer: Layer::Private,
        }]);
        let svc = ContextIndexService::new();
        // The worker began its pass at this epoch …
        let epoch = svc.inner.epoch.load(std::sync::atomic::Ordering::SeqCst);
        // … the profile switched (reset) before it published …
        svc.reset();
        let fired = std::sync::atomic::AtomicBool::new(false);
        let published = publish(
            &svc.inner,
            &state,
            true,
            &[],
            false,
            Some(1),
            epoch,
            &|_| fired.store(true, std::sync::atomic::Ordering::SeqCst),
            None,
        );
        // … so nothing of the old profile is installed or announced.
        assert!(!published);
        assert!(!fired.load(std::sync::atomic::Ordering::SeqCst));
        assert!(svc.snapshot().records.is_empty());
        // A pass at the current epoch does publish.
        let now = svc.inner.epoch.load(std::sync::atomic::Ordering::SeqCst);
        assert!(publish(
            &svc.inner,
            &state,
            true,
            &[],
            false,
            Some(1),
            now,
            &|_| {},
            None
        ));
        assert_eq!(svc.snapshot().records.len(), 1);
    }
}
