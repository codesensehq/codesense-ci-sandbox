// SPDX-License-Identifier: MIT
//! SS-4a: watcher cutover onto `content_engine`.
//!
//! Before SS-4a, this module owned a `notify::RecommendedWatcher`, a
//! per-key debounce map, an in-house `HealGuard` refcount-less mutex
//! set, and ~250 lines of per-event-type branching inside one giant
//! closure. After SS-4a:
//!
//! - The watcher itself is `content_engine::watcher::Watcher` (held
//!   behind an `Arc` so dynamic `add_watch` / `remove_watch` calls can
//!   share ownership with `start_watching`).
//! - Debounce + suppression live on the shared
//!   `content_engine::dispatcher::Dispatcher` (one instance per
//!   `WatcherState`, lifetime = app lifetime).
//! - Per-event-type branching is decomposed into per-role handler
//!   builders (`build_context_handler`, `build_reverse_handler`,
//!   `build_codesense_internal_handler`). Each registers against a
//!   specific directory via `Dispatcher::register_handler`.
//! - `HealGuard` is gone — install / heal sites construct
//!   `content_engine::watcher::Suppression` against the dispatcher's
//!   handle directly.
//!
//! Cache state (`Arc<RwLock<FrontmatterCache>>`) is resolved per event
//! via `app.try_state` rather than captured at handler-build time. This
//! mirrors the legacy closure's behavior and means the handler stays
//! valid across cache-availability changes during the app lifetime.
//!
//! The Tauri command surface (`start_watching`, `stop_watching`,
//! `add_watch`, `remove_watch`) and every TS-observable event name and
//! payload shape are unchanged. This is a strict mechanical cutover.

use crate::shared::lock_ext::LockOrErr;
use std::collections::{HashMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use log::{debug, error, info, trace, warn};
use serde::Deserialize;
use tauri::{Emitter, Manager};

use crate::commands::async_helpers::await_blocking;
use crate::content_engine::sync::{DetachFn, WaveCoordinator};
use crate::content_engine::{
    self as content_engine, DispatchHandler, Dispatcher, WatchDir, WatchOutcome, WatchSetup,
    Watcher as ContentWatcher,
};
use crate::install::capture;
use crate::install::frontmatter_cache::{FrontmatterCache, SharedFrontmatterCache};

/// Minimum interval between IPC emissions for the same path.
///
/// 50 ms coalesces burst writes (git checkout) while letting rename
/// event pairs (~50-200 ms apart on macOS FSEvents) through. See story
/// F.27 for the original rationale; the constant is now passed once to
/// `Dispatcher::new` at app startup.
const DEBOUNCE_MS: u64 = 50;

/// R-2: soft cap on the dispatcher's per-path debounce map. The legacy
/// `watch.rs` `should_emit` used the same constant; the dispatcher's
/// threshold-triggered age-based eviction mirrors that contract.
const DEBOUNCE_MAX_ENTRIES: usize = 1000;

/// R-2: max age (ms) for debounce entries before eviction is allowed.
/// Comfortably exceeds `DEBOUNCE_MS` so we never evict an active window.
const DEBOUNCE_EVICT_AGE_MS: u64 = 60_000;

/// SS-4a: how long a `Suppression` entry stays live past the guard's
/// scope. Outlives FSEvents/inotify delivery latency so events queued
/// by the kernel during a codesense-driven write but delivered *after*
/// the write call returns still see the path as suppressed.
///
/// **Transitional value of 5 s** — the current Suppression callers are
/// `heal_install` and `reinstall_source_repo` (legacy install commands
/// that operate over the v2 lockfile's `install_symlinks` +
/// `story_symlinks` arrays — up to ~30 symlinks unlink/recreate, plus
/// optional Core refresh + gitignore rewrite, plus possible org context
/// scaffolding on reinstall). Heal can wall-clock past 500 ms; we need
/// the grace to outlive the operation, not just FSEvents latency.
///
/// **Future shrink to 500 ms** — SS-11 retires `heal_install` /
/// `reinstall_source_repo` / `install_source_repo`. The remaining
/// Suppression consumer is SS-4b's `materialize_one` (single atomic
/// copy + rename + chmod + hash record — sub-second). At that point
/// 500 ms is comfortably more than enough. Reduce when SS-11 lands.
///
/// iter-5 Z6 collateral-suppression caveat: during a heal that
/// includes the user's `_bmad-output/...` story symlinks in its
/// suppression set, any user-driven edit to a story file in that same
/// time window is **silently filtered** from the watcher emit stream.
/// The user's save still lands on disk, but the pipeline rescan +
/// `context-file-change` emit don't fire until the next save outside
/// the grace window. Acceptable trade-off — heals are short and rare,
/// and the alternative (no suppression) re-introduces the symlink-
/// churn event flood that R-2 closed.
const SUPPRESSION_GRACE_MS: u64 = 5_000;

pub struct WatcherState {
    /// Active watcher, set by `start_watching`. Held behind `Arc` so
    /// `add_watch` / `remove_watch` can clone the handle without
    /// taking the watcher out of the slot.
    pub(crate) watcher: Mutex<Option<Arc<ContentWatcher>>>,
    /// Dispatcher lives for the app's lifetime — it owns the debounce
    /// map, the handler registry, and the suppression set. Survives
    /// `stop_watching` so the warm caches don't re-grow from scratch on
    /// the next `start_watching` call. (`pub` because `commands/install.rs`
    /// reaches `state.dispatcher.suppression_handle()` for the
    /// `Suppression` RAII guard at the two heal/reinstall call sites.)
    pub dispatcher: Arc<Dispatcher>,
    /// Directory → list of subscribing `WatchConfig`s. Populated as a
    /// side-effect of `start_watching` / `add_watch`; consulted by
    /// `remove_watch` to know which dirs to detach for a given config
    /// and by `stop_watching` to know which dispatcher handlers to
    /// unregister.
    ///
    /// SS-FU-1: multi-value (`Vec<WatchConfig>`) so two source repos
    /// resolving to the same shared canonical dir (same team's
    /// `context_output`, same org's `_codesense/`) both stay reachable.
    /// Handler closures read this Vec at dispatch time and fan out one
    /// emit per subscriber, so the Vec is **on the hot dispatch path** —
    /// the lock must be released before per-subscriber emit/side-effect
    /// work (clone the Vec briefly).
    pub(crate) path_to_repo: Arc<Mutex<HashMap<PathBuf, Vec<WatchConfig>>>>,
    /// SW-WL-4: dirs whose kernel watch is owned by the TEAM lifecycle
    /// (the active team's `context_output`), NOT by any per-workspace
    /// subscriber. Maintained by [`register_team_context_watch`] on team
    /// activation/switch. The per-workspace teardown
    /// ([`remove_watch_inner`]) must never `unwatch`/`unregister` a member
    /// even when its subscriber `Vec` empties: `ContentWatcher.watched` is
    /// membership-not-refcount, so a single stray per-workspace `unwatch`
    /// would detach the always-on watch for everyone. Only the team layer
    /// detaches these (on switch). At most one entry in the steady state
    /// (the active team); a set keeps the switch transition window safe.
    pub(crate) team_context_dirs: Arc<Mutex<HashSet<PathBuf>>>,
    /// Registered profile scratch root, retained before native startup for one-shot recovery.
    pub(crate) scratch_root: Mutex<Option<PathBuf>>,
    /// Native reader intent: physical org root, org identity, and team; lineage is read afresh.
    recovery_team: Mutex<Option<(PathBuf, String, String)>>,
    /// Serializes the complete async active-team transaction, from validation
    /// through every team-scoped watch/state/reload commit. Watcher replacement
    /// deliberately stays outside this activation-order stream.
    pub(crate) team_activation_lock: Arc<tokio::sync::Mutex<()>>,
    /// SW-WU-14: the active team's watched `<clone>/.git/worktrees/` admin dirs.
    /// Maintained by [`register_managed_clone_worktree_watch`] on team activation/switch.
    /// Like [`team_context_dirs`](Self::team_context_dirs) these are TEAM-lifecycle-owned
    /// (no per-workspace subscriber), so the switch handler detaches the previous team's
    /// dirs — keeping the watch-set equal to the scan-set, which `discover_worktrees` scopes
    /// to the ACTIVE team's clones. Held across the whole diff→detach→attach to serialize a
    /// fire-and-forget A→B→A switch (mirrors the `team_context_dirs` guard discipline).
    pub(crate) worktree_admin_dirs: Arc<Mutex<HashSet<PathBuf>>>,
    /// SW-WL-6: highest whole-watcher lifecycle SEQUENCE requested — the guard
    /// that keeps a stop-vs-restart deterministic now that both
    /// [`start_watching`] and [`stop_watching`] run off the main thread (the
    /// old sync `stop_watching` blocked the IPC thread, implicitly draining
    /// before any following start; async removes that ordering).
    ///
    /// The sequence is minted by the **single-threaded JS caller**
    /// (`startWatching`/`stopWatching` in `commands.ts` — a monotonic
    /// `++counter`), so its VALUES encode true JS intent order. A Rust-side
    /// stamp cannot: Tauri spawns `async` commands onto a **multi-threaded**
    /// runtime (`async_runtime::spawn` → `tokio::Runtime::new`), so a command's
    /// prologue runs at first-poll on an arbitrary worker — entry order does NOT
    /// track intent order. Each command `fetch_max`es its seq here at entry
    /// (order-independent — a max), and its worker re-reads the max **inside
    /// `watcher_lock`** (see [`lifecycle_seq_is_current`]), no-oping unless its
    /// own seq is still the max. Last intent (highest seq) therefore wins
    /// regardless of blocking-pool execution order: whoever acquires the lock
    /// last reads the final max and bails unless it is the winner.
    /// `add_watch`/`remove_watch` don't touch it — they mutate subscribers, not
    /// the watcher slot.
    ///
    /// **Lifetime-alignment invariant:** this high-water outlives the webview
    /// (it lives in Tauri app state; the backend survives a reload / HMR). The
    /// JS counter must therefore NOT reset to 0 on reload — else its fresh
    /// `1,2,3…` would all lose `== max` and every post-reload start/stop would
    /// silently bail. `commands.ts` seeds the counter from `sessionStorage`,
    /// whose lifetime matches this process (both reset only on app relaunch), so
    /// the two counters stay aligned.
    pub(crate) watcher_lifecycle_seq: AtomicU64,
}

impl Default for WatcherState {
    fn default() -> Self {
        Self {
            watcher: Mutex::new(None),
            dispatcher: Arc::new(Dispatcher::new(
                DEBOUNCE_MS,
                DEBOUNCE_MAX_ENTRIES,
                DEBOUNCE_EVICT_AGE_MS,
                SUPPRESSION_GRACE_MS,
            )),
            path_to_repo: Arc::new(Mutex::new(HashMap::new())),
            team_context_dirs: Arc::new(Mutex::new(HashSet::new())),
            scratch_root: Mutex::new(None),
            recovery_team: Mutex::new(None),
            team_activation_lock: Arc::new(tokio::sync::Mutex::new(())),
            worktree_admin_dirs: Arc::new(Mutex::new(HashSet::new())),
            watcher_lifecycle_seq: AtomicU64::new(0),
        }
    }
}

impl WatcherState {
    pub(crate) async fn lock_team_activation(&self) -> tokio::sync::OwnedMutexGuard<()> {
        Arc::clone(&self.team_activation_lock).lock_owned().await
    }

    /// SS-4c: cheap-clone accessor for the active `ContentWatcher` Arc,
    /// for callers (the per-team `content/` and reverse-dest watch
    /// registrations) that need to attach/detach dirs after the watcher
    /// is started. Returns `None` if `start_watching` hasn't run yet
    /// (e.g. the wave engine's content-layer attach fires before the
    /// frontend has loaded orgs + active team).
    pub fn watcher_handle(&self) -> Option<Arc<ContentWatcher>> {
        self.watcher
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(Arc::clone))
    }
}

/// A watch subscriber: one workspace's repo and its team content.
///
/// SW-XS-12 (XS-18 leftover): the former `override_glob_prefix` (B-7 /
/// SW-FX-56, the staleness-override glob prefix SW-XS-14 retired) is gone. An
/// older frontend that still sends it (or `pipeline_root`) deserializes: serde
/// ignores unknown keys here (no `deny_unknown_fields`).
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct WatchConfig {
    pub repo_path: String,
    pub context_repo_path: String,
}

/// Subscriber identity for the `path_to_repo` registry — the single
/// source of truth for "are these two configs the same logical
/// subscriber?". SW-WL-1: a synthetic-slug planning workspace can share
/// a `repo_path` with an attached source repo yet differ in
/// `context_repo_path` (the context repo encodes team + owner/repo); the
/// composite `(repo_path, context_repo_path)` keeps them DISTINCT so one
/// never silently evicts the other in `append_subscriber_with_replace`
/// (or any retract path). Re-registering an identical identity still
/// replaces in place (SS-FU-1 AC-7 idempotency preserved). Used by every
/// add/retract/rollback site so the identity is defined exactly once.
impl WatchConfig {
    fn subscriber_identity(&self) -> (&str, &str) {
        (self.repo_path.as_str(), self.context_repo_path.as_str())
    }
}

// SW-CP-2: `ResolvedCacheContext` / `resolve_cache_context` retired with
// `try_sync_staleness`. The content watcher now fans out to the whole team
// via the coordinator's stashed projection target, so the per-event path
// needs no slug/cache resolution here.

// SW-CP-2: `completed_at_changed`, `try_sync_staleness`, and
// `handle_context_file_change` were retired with `sync_epic_staleness`.
// The content watcher now enqueues a projection wave on every relevant
// content edit (`enqueue_content_projection`), so there is no longer a
// `completed_at`-gated symlink-staleness side path.

/// Directories derived from a WatchConfig. Centralizes path computation
/// so start_watching, add_watch, and remove_watch stay in sync.
struct WatchDirs {
    context_output: PathBuf,
    /// SS.8 (iter-2 patch): `{org_root}/_codesense/` so the OS watcher
    /// emits events for the new `worktree/{owner}/{repo}.yaml` location.
    /// `None` when `context_repo_path` doesn't carry enough info to derive
    /// the org root; in that case `_codesense/` is simply not watched (SW-WL-5
    /// retired the source-repo `repo_root` watch that previously observed the
    /// legacy `<source>/.codesense/worktree.yaml` location — it only ever fed
    /// the dead `fs-change` channel).
    codesense_internal_dir: Option<PathBuf>,
}

/// SW-WL-3: the dispatcher role of a watched dir — determines which handler it
/// gets and whether it is watched recursively. The single place the per-dir
/// kind is encoded, so [`WatchDirs::watch_candidates`] can be the one source
/// both the slow-path registration loop and the fast-path decision read.
#[derive(Clone, Copy)]
enum DirRole {
    /// Recursive `context-file-change` handler (`context_output`).
    Context,
    /// Recursive `_codesense/` namespace handler; eagerly `create_dir_all`'d.
    CodesenseInternal,
}

impl DirRole {
    /// Whether the OS watch for this role descends into subdirectories.
    /// Exhaustive on purpose: a new role must opt into recursion explicitly here
    /// rather than inherit a default.
    fn recursive(self) -> bool {
        match self {
            DirRole::Context | DirRole::CodesenseInternal => true,
        }
    }
}

/// SW-WL-3: one watched-dir candidate — a path plus the [`DirRole`] that drives
/// its handler + recursion.
struct WatchDirCandidate {
    path: PathBuf,
    role: DirRole,
}

fn resolve_watch_dirs(config: &WatchConfig) -> WatchDirs {
    let context_output = team_content_dir_for_watch(config)
        // Fallback to the raw context-repo path when parsing fails (mostly
        // test fixtures with synthetic paths). Production watchers always
        // resolve to the per-team content dir. SW-CP-18: the legacy
        // `viewer_root` join is retired — whole-root watches the content root.
        .unwrap_or_else(|| PathBuf::from(&config.context_repo_path));
    // SS.8 (iter-2 patch): derive `{org_root}/_codesense/` so we can
    // subscribe to events under the new yaml namespace. `derive_org_root`
    // strips a trailing `_team/{team}/...` from the context-repo path; for
    // paths already at the org root it returns the input unchanged, so the
    // helper covers both PTS-2 flat and legacy synthetic layouts.
    //
    // Iter-3 defense-in-depth: also require the derived path to be
    // absolute. Test fixtures or malformed callers can pass a relative
    // `context_repo_path`; without the guard, `Path::new("foo").join(...)`
    // becomes CWD-relative and the watcher would subscribe to a stray
    // directory wherever the Tauri process happens to be running.
    let codesense_internal_dir = crate::context_repo::derive_org_root(&config.context_repo_path)
        .map(|org| Path::new(org).join(crate::context_repo::CODESENSE_INTERNAL_DIR))
        .filter(|p| p.is_absolute());
    WatchDirs {
        context_output,
        codesense_internal_dir,
    }
}

impl WatchDirs {
    /// SW-WL-3 (AC-4): the dirs `register_directory_handlers` would attach for
    /// this config, in canonical order, each gated exactly as Phase 1 gates it:
    /// `context_output` is included iff it exists;
    /// `codesense_internal_dir` is always a candidate (the slow path
    /// `create_dir_all`s it before attaching). This is the SINGLE source both
    /// the slow-path registration loop and the fast-path decision
    /// ([`fast_path_candidate_dirs`](Self::fast_path_candidate_dirs)) derive
    /// from, so the two cannot drift: adding a watched dir is one new arm here,
    /// not a parallel edit the fast-path could miss.
    fn watch_candidates(&self) -> Vec<WatchDirCandidate> {
        let mut out = Vec::new();
        if self.context_output.exists() {
            out.push(WatchDirCandidate {
                path: self.context_output.clone(),
                role: DirRole::Context,
            });
        }
        if let Some(d) = &self.codesense_internal_dir {
            out.push(WatchDirCandidate {
                path: d.clone(),
                role: DirRole::CodesenseInternal,
            });
        }
        out
    }

    /// SW-WL-3 (AC-4): the candidate paths only. The fast-path attach takes IFF
    /// every one is already a `path_to_repo` key (kernel-watched +
    /// dispatcher-registered by a prior subscriber, since
    /// `retract_subscriber_collect_emptied` drops a key the moment its last
    /// subscriber leaves) — then the attach is a pure subscriber append with no
    /// `watcher_lock` and no kernel `inner.watch` calls.
    fn fast_path_candidate_dirs(&self) -> Vec<PathBuf> {
        self.watch_candidates()
            .into_iter()
            .map(|c| c.path)
            .collect()
    }
}

/// PTS-2: derive the per-team content dir that the watcher should observe
/// for context-side changes (post-migration the canonical write target is
/// `{org_root}/_team/{team}/content/`, not the legacy `_team/{team}/repos/.../_bmad-output`).
///
/// Returns `None` if `context_repo_path` doesn't match either layout — the
/// caller falls back to the legacy composition for backward compat.
fn team_content_dir_for_watch(config: &WatchConfig) -> Option<PathBuf> {
    let (team, _, _) = capture::parse_context_repo_parts(&config.context_repo_path)?;
    let org_root = crate::context_repo::derive_org_root(&config.context_repo_path)?;
    Some(crate::context_repo::path_to_team_content_dir(
        Path::new(org_root),
        &team,
    ))
}

/// Whether an event should trigger a re-verify / projection wave.
///
/// SW-CP-18: convention-agnostic. The legacy `viewer_root` / `install_dirs`
/// literal arms are retired — whole-root projects every top-level content dir,
/// so there is no sub-convention to filter on. An event beneath a
/// directly-watched dir (the team content root, a projection dest, or the
/// `_codesense/` namespace) is relevant by construction. The `.codesense` /
/// `_codesense` ancestor arm and the managed repo-root names remain as defense
/// for events reported against a parent watch and for the settings-panel
/// `worktree.yaml` create/remove signal.
fn is_relevant_event(event_path: &Path, watched_dir: &Path) -> bool {
    // Any event under a directly-watched dir is relevant.
    if event_path.starts_with(watched_dir) {
        return true;
    }

    // Defense for events delivered against a parent watch: context repos live
    // under `~/.codesense/...` (every content event has a `.codesense`
    // ancestor) and the SS.8 org-level `_codesense/` holds the worktree yaml
    // the settings panel listens for.
    for ancestor in event_path.ancestors() {
        if ancestor.ends_with(".codesense") || ancestor.ends_with("_codesense") {
            return true;
        }
    }

    // Managed repo-root names we always treat as relevant wherever they appear
    // (settings-panel create/remove signals).
    let Some(file_name) = event_path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    matches!(
        file_name,
        "_bmad" | ".gitignore" | ".codesense" | "_codesense"
    )
}

// ---------------------------------------------------------------------------
// SS-4a: handler builders
// ---------------------------------------------------------------------------

/// Build the base 3-field JSON payload that every watcher event emits.
/// `install-file-change` handlers may add `conflict` / `filename` /
/// `dest` on top per the R-5 enrichment contract.
fn base_event_payload(event_path: &Path, config: &WatchConfig) -> serde_json::Value {
    serde_json::json!({
        "repo_path": config.repo_path,
        "context_repo_path": config.context_repo_path,
        "file_path": event_path.to_string_lossy(),
    })
}

/// SS-FU-1: type alias for the multi-value subscriber registry. Read
/// at dispatch time by the fan-out handlers; written by
/// `register_directory_handlers` / `remove_watch`.
type PathToRepo = Arc<Mutex<HashMap<PathBuf, Vec<WatchConfig>>>>;

/// SS-FU-1: append `config` to the subscriber Vec at `dir` with
/// **replace-by-subscriber-identity** semantics. If an existing entry
/// has the same identity (`WatchConfig::subscriber_identity`,
/// SW-WL-1: composite `(repo_path, context_repo_path)`) as the incoming
/// config, it is dropped before the new entry is appended — the latest
/// WatchConfig fields (viewer_root, install_dirs, etc.) win for that
/// subscriber identity. AC-7's idempotent-re-registration contract.
///
/// SW-WL-1: two configs sharing a `repo_path` but differing in
/// `context_repo_path` (e.g. a synthetic-slug planning workspace
/// coinciding with an attached source repo) are DISTINCT subscribers
/// and coexist — the prior `repo_path`-only key silently evicted one.
///
/// Returns the displaced subscriber (if any matched `repo_path`) so a
/// failed Phase 2b can restore prior state — re-registering an existing
/// `repo_path` and then failing attach must not destroy the previously-
/// working subscription (review iter-1 finding).
///
/// Returns `Err` only on mutex poisoning.
/// Append `config`'s subscriber to `dir`'s Vec with replace-by-identity, over
/// an already-held map guard. Split out so both the single-dir
/// [`append_subscriber_with_replace`] and the multi-dir fast-path
/// [`try_fast_path_attach`] (which holds ONE lock across a check + N appends)
/// share the replace logic without re-locking.
fn append_subscriber_locked(
    map: &mut HashMap<PathBuf, Vec<WatchConfig>>,
    dir: &Path,
    config: &WatchConfig,
) -> Option<WatchConfig> {
    let entry = map.entry(dir.to_path_buf()).or_default();
    // Capture the first match (invariant: at most one matching
    // `repo_path` per dir) THEN drop ALL matches via `retain` so any
    // future-bug-introduced duplicates are cleared too (review iter-2:
    // `position`+`remove` weakened the defensive cleanup vs the original
    // `retain` semantics).
    let displaced = entry
        .iter()
        .find(|c| c.subscriber_identity() == config.subscriber_identity())
        .cloned();
    entry.retain(|c| c.subscriber_identity() != config.subscriber_identity());
    entry.push(config.clone());
    displaced
}

fn append_subscriber_with_replace(
    path_map: &PathToRepo,
    dir: &Path,
    config: &WatchConfig,
) -> Result<Option<WatchConfig>, String> {
    let mut locked = path_map.lock_or_err()?;
    Ok(append_subscriber_locked(&mut locked, dir, config))
}

/// SW-WL-3 (AC-4): the fast-path attach decision + append, atomic under a
/// single `path_to_repo` lock. Returns `Ok(true)` and appends `config`'s
/// subscriber to every candidate dir IFF all candidates are already keys
/// (kernel-watched + dispatcher-registered by a prior subscriber). Returns
/// `Ok(false)` WITHOUT mutating when any candidate is net-new (caller takes
/// the slow path). Crucially this NEVER touches `watcher_lock`, so a workspace
/// open whose dirs are already covered cannot block on a concurrent
/// `start_watching` rebuild. Pure over `path_to_repo` — unit-testable without
/// an `AppHandle`.
fn try_fast_path_attach(
    path_to_repo: &PathToRepo,
    candidates: &[PathBuf],
    config: &WatchConfig,
) -> Result<bool, String> {
    if candidates.is_empty() {
        return Ok(false);
    }
    let mut map = path_to_repo.lock_or_err()?;
    // SW-WL-6 R-1: this `contains_key` guard closes ONE of the two close→reopen
    // orderings now that the immediate detach runs off-thread — if a concurrent
    // `remove_watch_inner` retract already emptied+removed the dir key, we
    // decline the fast path and rebuild under `watcher_lock`. It does NOT close
    // the reverse ordering (a fast-path append landing in the window *before*
    // the retract runs); see `remove_watch` for that documented low-severity
    // window.
    if !candidates.iter().all(|d| map.contains_key(d)) {
        return Ok(false);
    }
    for dir in candidates {
        append_subscriber_locked(&mut map, dir, config);
    }
    Ok(true)
}

/// SS-FU-1 review iter-1/iter-2/iter-3: roll back a partially-
/// completed Phase 2a (subscribers appended + dispatcher closures
/// registered) when a later step (Phase 2b attach OR a subsequent
/// Phase 2a iteration's `append_subscriber_with_replace` poison)
/// fails. Best-effort under poison.
///
/// `attempted` is a list of `(dir, displaced_subscriber)` pairs from
/// successful Phase 2a iterations. The function:
/// 1. **Deduplicates `attempted` by dir keeping FIRST entry's
///    `displaced`** (iter-3 fix): a duplicate dir in `attempted` —
///    e.g. a `reverse_dests` entry overlapping `context_output` —
///    would otherwise produce `displaced=Some(failing_config)` on
///    the second iteration (the helper finds its own just-pushed
///    value), and the restore step would re-push the failing config
///    back, "rolling forward" instead of rolling back. The FIRST
///    entry's `displaced` is the genuine prior owner (or None if
///    there wasn't one).
///
///    **Why first-wins is correct under Phase 1's deterministic
///    enumeration** (iter-4 doc clarification, iter-5 refined):
///    Phase 1 builds `registrations` in a fixed order —
///    `context_output`, then `reverse_dests[...]`, then
///    `codesense_internal_dir`. Each singleton dir-kind (context_output,
///    codesense_internal_dir) is pushed
///    at most ONCE per Phase 1; duplicates can only arise from a
///    later registration overlapping an earlier one (most commonly a
///    `reverse_dests[i]` colliding with a prior
///    singleton or another reverse_dests entry, but also possible if
///    a pathological config has `codesense_internal_dir == context_output`).
///    Whichever enumeration position appears FIRST for a given
///    duplicated dir captures `displaced` reflecting state from
///    BEFORE this Phase 2a batch started — the genuine prior owner
///    or `None`. Subsequent same-dir entries always see self-
///    displacement from earlier same-batch appends and so should be
///    discarded.
///
///    **Precondition** for the first-wins invariant: `attempted` is
///    built within a single `register_directory_handlers` invocation
///    for a single `config`. Cross-config interleaving (parallel
///    Phase 2a for multiple configs) would invalidate the invariant
///    by mixing displaced subscribers from different repo_paths into
///    the FIRST slot. The watcher_lock discipline at all current
///    call sites (start_watching_impl, add_watch_impl) prevents
///    this interleaving.
/// 2. Retracts the failing config's `repo_path` from each unique dir.
/// 3. **Restores genuine displaced subscribers** (under lock) and
///    tracks which dirs were ACTUALLY restored (iter-3 fix): a
///    poisoned restore lock previously left the dispatcher closure
///    preserved for a dir whose displaced was dropped — silent
///    event leak.
/// 4. Unregisters the dispatcher closure for emptied dirs that were
///    NOT restored. Dirs whose displaced was genuinely restored keep
///    their closure (the closure body reads from `path_to_repo`, so
///    it dispatches correctly to the restored subscriber).
///
/// All steps are best-effort under poison; a poisoned `path_map`
/// lock during restore is logged at `warn!` so ops can correlate
/// downstream missing-subscription reports.
fn rollback_phase_2a(
    path_map: &PathToRepo,
    dispatcher: &Arc<Dispatcher>,
    attempted: &[(PathBuf, Option<WatchConfig>)],
    config_identity: (&str, &str),
) {
    use std::collections::HashSet;

    // Step 1: dedupe attempted by dir, keeping the FIRST entry's
    // displaced (the genuine prior owner; subsequent same-dir entries
    // reflect intra-batch self-displacement, not a true prior).
    let mut seen_dirs: HashSet<PathBuf> = HashSet::new();
    let dedup_attempted: Vec<(PathBuf, Option<WatchConfig>)> = attempted
        .iter()
        .filter(|(d, _)| seen_dirs.insert(d.clone()))
        .cloned()
        .collect();

    // Step 2: retract. On poison, log immediately and unconditionally
    // (review iter-5 fix for iter-4 attribution gap: previously the
    // retract-poison warn fired only inside the restore-Err arm AND
    // only when any_displaced was true; the (retract-poison +
    // no-displaced) and (retract-poison + restore-ok) cells were
    // structurally silent, leaving ops with no signal that the
    // failing config might still be in path_to_repo).
    let dirs: Vec<PathBuf> = dedup_attempted.iter().map(|(d, _)| d.clone()).collect();
    let emptied_dirs = match retract_subscriber_collect_emptied(path_map, &dirs, config_identity) {
        Ok(emptied) => emptied,
        Err(_) => {
            warn!(
                target: "watch",
                "rollback_phase_2a: path_to_repo poisoned during retract; failing config {:?} may still be in path_to_repo",
                config_identity
            );
            Vec::new()
        }
    };

    // Step 3: restore. Track which dirs were ACTUALLY restored (not
    // just which COULD have been) so a poisoned restore lock doesn't
    // leave orphaned dispatcher closures.
    let any_displaced = dedup_attempted.iter().any(|(_, d)| d.is_some());
    let restored_dirs: HashSet<PathBuf> = if any_displaced {
        match path_map.lock() {
            Ok(mut locked) => {
                let mut restored = HashSet::new();
                for (dir, displaced) in &dedup_attempted {
                    if let Some(prior) = displaced {
                        locked.entry(dir.clone()).or_default().push(prior.clone());
                        restored.insert(dir.clone());
                    }
                }
                restored
            }
            Err(_) => {
                let intended_restore_count =
                    dedup_attempted.iter().filter(|(_, d)| d.is_some()).count();
                // Iter-6: include config_identity with `{:?}`
                // formatting for symmetry with the retract-poison
                // warn — ops can correlate downstream missing-
                // subscription reports back to the failing config
                // regardless of which step poisoned.
                warn!(
                    target: "watch",
                    "rollback_phase_2a: path_to_repo poisoned during restore for failing config {:?}; {} displaced subscriber(s) dropped",
                    config_identity,
                    intended_restore_count
                );
                HashSet::new()
            }
        }
    } else {
        HashSet::new()
    };

    // Step 4: unregister dispatcher closure for emptied dirs that
    // were NOT restored.
    for dir in &emptied_dirs {
        if !restored_dirs.contains(dir) {
            dispatcher.unregister_handler(dir);
        }
    }
}

/// SS-FU-1: retract the subscriber matching `identity` from each
/// dir's Vec. Dirs whose Vec becomes empty are removed from the map
/// and collected into the returned list — callers use that list to
/// unregister dispatcher closures and `watcher.unwatch` only for
/// now-empty keys (AC-3 preserves siblings, AC-4 detaches on
/// last-subscriber removal).
///
/// SW-WL-1: `identity` is `WatchConfig::subscriber_identity` —
/// composite `(repo_path, context_repo_path)`. Retracting by the full
/// identity (not `repo_path` alone) ensures a teardown/rollback for one
/// subscriber cannot evict a colliding-`repo_path` sibling that differs
/// in `context_repo_path`.
fn retract_subscriber_collect_emptied(
    path_map: &PathToRepo,
    dirs: &[PathBuf],
    identity: (&str, &str),
) -> Result<Vec<PathBuf>, String> {
    let mut emptied = Vec::new();
    let mut locked = path_map.lock_or_err()?;
    for dir in dirs {
        if let Some(subs) = locked.get_mut(dir) {
            subs.retain(|c| c.subscriber_identity() != identity);
            if subs.is_empty() {
                locked.remove(dir);
                emptied.push(dir.clone());
            }
        }
    }
    Ok(emptied)
}

/// SW-CP-9: retract the subscriber matching `identity` from EVERY dir it
/// appears in, returning the dirs that became empty. Unlike
/// [`retract_subscriber_collect_emptied`] (which retracts from a caller-
/// supplied dir list), this scans the whole `path_to_repo` map — so teardown
/// releases every dir this subscriber actually attached, including reverse
/// dest dirs whose manifest membership may have changed since attach. The
/// manifest-resolved reverse set is NOT recomputable at detach
/// ([`remove_watch_inner`] has no [`ResolverContext`], and a recompute from
/// the current manifest would leak a since-removed projection's dest), so
/// retract-by-identity is both necessary and strictly more correct than the
/// recompute it replaces. Match is by full `(repo_path, context_repo_path)`
/// identity (SW-WL-1), so a colliding-`repo_path` sibling is left untouched.
fn retract_subscriber_all_keys_collect_emptied(
    path_map: &PathToRepo,
    identity: (&str, &str),
) -> Result<Vec<PathBuf>, String> {
    let mut emptied = Vec::new();
    let mut locked = path_map.lock_or_err()?;
    locked.retain(|dir, subs| {
        subs.retain(|c| c.subscriber_identity() != identity);
        if subs.is_empty() {
            emptied.push(dir.clone());
            false
        } else {
            true
        }
    });
    Ok(emptied)
}

/// SS-FU-1: snapshot the subscriber list for `dir` under a brief lock.
/// Returns an empty Vec if the dir has no subscribers or the mutex is
/// poisoned (poison-tolerant dispatch — never panics from the notify
/// worker thread).
fn snapshot_subscribers(path_to_repo: &PathToRepo, dir: &Path) -> Vec<WatchConfig> {
    match path_to_repo.lock() {
        Ok(map) => map.get(dir).cloned().unwrap_or_default(),
        Err(_) => {
            warn!(
                target: "watch",
                "path_to_repo mutex poisoned; dispatch produces no payloads"
            );
            Vec::new()
        }
    }
}

/// Resolve the nearest watched context root and its subscribers. An empty
/// subscriber list still identifies a team's live content watch; it must not
/// be confused with an unwatched path when nudging source-free surfaces.
pub(crate) fn subscribers_for_path(
    path_to_repo: &PathToRepo,
    file_path: &Path,
) -> Option<(PathBuf, Vec<WatchConfig>)> {
    // Resolve filesystem aliases before taking the subscriber-map lock.
    let key = crate::content_engine::dispatcher::stable_key(file_path);
    let map = match path_to_repo.lock() {
        Ok(m) => m,
        Err(_) => {
            warn!(
                target: "watch",
                "path_to_repo mutex poisoned; suppressed-write nudge produces no payloads"
            );
            return None;
        }
    };
    for ancestor in key.ancestors().chain(file_path.ancestors()) {
        if let Some(subs) = map.get(ancestor) {
            return Some((ancestor.to_path_buf(), subs.clone()));
        }
    }
    None
}

/// Share the live handler's team/subscriber routing without dispatching a wave.
fn suppressed_write_payloads(
    path_to_repo: &PathToRepo,
    file_path: &Path,
) -> Vec<serde_json::Value> {
    if let Some(payload) = super::scratch_watch::content_change_payload(file_path) {
        return vec![payload];
    }
    if is_transient_editor_file(file_path) {
        return Vec::new();
    }
    let Some((watched_dir, subscribers)) = subscribers_for_path(path_to_repo, file_path) else {
        return Vec::new();
    };
    if let Some(payload) = team_level_emit(&subscribers, file_path, &watched_dir) {
        return vec![payload];
    }
    subscribers
        .iter()
        .map(|config| base_event_payload(file_path, config))
        .collect()
}

/// SW-FX-26: a watcher-SUPPRESSED write fires no FS event, so the FE pipeline
/// nav never learns a newly-created sibling (canvas/image/doc) exists until a
/// reload. Emit the `context-file-change` event the FE already consumes
/// (`App.tsx`) DIRECTLY here — bypassing the suppression-gated dispatcher — so
/// the nav re-scans and surfaces the file (and any `supporting_docs:` badge)
/// live within ~1s.
///
/// The nudge is transition-free / read-only on the source layer (it triggers an
/// FE *read*/rescan only; reverse-sync loop prevention is unaffected). SW-XS-3:
/// no watcher event enqueues a projection wave at all any more.
///
/// Reuses the watcher's live `WatchConfig`s so the emitted `repo_path` /
/// `context_repo_path` are identical to the unsuppressed path's — the FE already
/// resolves that `context_repo_path` to a synthetic workspace key (SW-FX-16), so
/// re-deriving the string in Rust would risk a normalization mismatch and a
/// silent dead-key bump. (The emitted `file_path` is the raw written path; the
/// FE keys the refresh on `context_repo_path` and uses `file_path` only for the
/// SF-3 per-file fast-path, which falls back safely on any mismatch.)
///
/// Mirrors the live handler's transient-temp guard ([`build_context_handler`] via
/// [`is_transient_editor_file`]) for parity: the suppressed writers always pass a
/// final dest (the atomic temp is internal to `write_tmp_fsync_rename*`), so it's
/// a no-op today, but it keeps a future suppressed caller from nudging the FE
/// toward a temp path no viewer can match.
pub(crate) fn emit_context_change_for_suppressed_write<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    file_path: &Path,
) {
    if is_transient_editor_file(file_path) {
        return;
    }
    let Some(state) = app.try_state::<WatcherState>() else {
        return;
    };
    for payload in suppressed_write_payloads(&state.path_to_repo, file_path) {
        crate::content_engine::sync::residual_metrics::note_context_file_change_emit();
        let _ = app.emit("context-file-change", payload);
    }
    emit_inherited_change(app, file_path);
}

// SW-IS-F10 (S4): `ReverseSyncDefault` is RETIRED onto the shared
// [`ReverseCadenceMode`](crate::content_engine::sync::ReverseCadenceMode) — the up (publish) direction
// now holds the SAME enum the down (hydrate) direction does, so there is ONE sync-cadence
// model, not two (the AC-6 de-dup). The old three-way maps on:
//   - `Always`            → `ReverseCadenceMode::Live`     (admit the ambient live base every tick)
//   - `OnDemand` / `Off`  → `ReverseCadenceMode::OnDemand` (drop the base; flush on demand — the
//                                                    old `Off` folds in, it was FE-label-only)
//   - (new) `Interval`    → admits the base too: the reverse direction is a POLL, and the
//                           poll cadence IS the interval, so for the pull direction `Live`
//                           and `Interval` both admit the ambient base (they differ only for
//                           the push/forward direction's debounce). OnDemand alone drops it.
// So the reverse gate's derived boolean is exactly the shared `fans_out_ambiently()`
// predicate the forward gate uses — one algebra, both directions.

/// The workspace's own `(context, target)`: its story registration for this
/// user when it has exactly one, else its durable repository identity under
/// the active context.
pub(crate) fn workspace_target(
    ctx: &crate::content_engine::ResolverContext,
    root: &Path,
) -> Result<
    (
        crate::content_engine::ResolverContext,
        crate::agents::ManifestTarget,
    ),
    String,
> {
    let mine: Vec<_> =
        crate::install::worktree_index::story_registrations(&ctx.codesense_home, root)
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|(principal, _)| principal == &ctx.principal())
            .collect();
    if let [(_, record)] = mine.as_slice() {
        let team = crate::install::worktree_index::resolve_team(record)
            .ok_or_else(|| format!("{} is registered without a team", root.display()))?;
        let mut own = crate::commands::content_engine::derive_resolver_context(
            ctx.codesense_home.clone(),
            ctx.user_id,
            record.org_slug.clone(),
            Some(team.clone()),
        )
        .map_err(|e| e.to_string())?;
        own.context_root = ctx.context_root.clone();
        let target = crate::agents::ManifestTarget {
            context_root: ctx.context_root.clone(),
            org: record.org_slug.clone(),
            team,
            repo: Some(record.source_repo.clone()),
        };
        return Ok((own, target));
    }
    if mine.len() > 1 {
        return Err(format!("{} has ambiguous registrations", root.display()));
    }
    let slug = crate::mcp::context_operations::local_repository(ctx, root).map_err(|e| {
        format!(
            "{} is not an attached workspace: {}",
            root.display(),
            e.message
        )
    })?;
    let team = ctx
        .team_name
        .clone()
        .ok_or("no active team to resolve the workspace through")?;
    let target = crate::agents::ManifestTarget {
        context_root: ctx.context_root.clone(),
        org: ctx.org_slug.clone(),
        team,
        repo: Some(slug),
    };
    Ok((ctx.clone(), target))
}

/// SS-FU-1: higher-order helper that owns the lock-snapshot-iterate
/// scaffolding used by `build_codesense_internal_handler`. The caller
/// supplies only the per-subscriber emit closure.
///
/// `build_context_handler` does NOT use this helper because its
/// `FrontmatterCache::refresh_under_lock` call MUST run exactly once
/// per event (it mutates the cache, so per-subscriber refresh would
/// silently skip staleness sync for subscribers 2..N — see story
/// SS-FU-1 "Cache refresh must be hoisted").
fn build_fanout_handler<F>(
    path_to_repo: PathToRepo,
    dir: PathBuf,
    per_subscriber: F,
) -> DispatchHandler
where
    F: Fn(&Path, &WatchConfig) + Send + Sync + 'static,
{
    Arc::new(move |event_path: &Path| {
        let subscribers = snapshot_subscribers(&path_to_repo, &dir);
        for config in &subscribers {
            // SS-FU-1 review iter-1: wrap each per-subscriber call in
            // `catch_unwind` so a single misbehaving subscriber doesn't
            // drop events for the remaining subscribers in the snapshot.
            // The dispatcher's outer `catch_unwind` would otherwise catch
            // the panic but the iteration would break at the failing
            // subscriber — sibling subscribers silently lose the event.
            if let Err(panic) =
                catch_unwind(AssertUnwindSafe(|| per_subscriber(event_path, config)))
            {
                error!(
                    target: "watch",
                    "fan-out per_subscriber panicked for {} ({}); continuing with remaining subscribers. payload: {:?}",
                    event_path.display(),
                    config.repo_path,
                    panic
                );
            }
        }
    })
}

/// SW-CP-4 F4: is `path` an editor swap / backup / atomic-temp artifact that
/// must not be reverse-adopted into the source layer? Denylist (not allowlist) so
/// real content of any extension still round-trips: vim swaps (`.foo.swp`/`.swo`/
/// `.swx`), backups (`foo~`), vim's `4913` probe, our own + many editors' atomic
/// temps (`*.tmp.*`), emacs lockfiles (`.#foo`), and `.DS_Store`.
pub(crate) fn is_transient_editor_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return true; // un-nameable path — never adopt
    };
    name.ends_with('~')
        || name.ends_with(".swp")
        || name.ends_with(".swo")
        || name.ends_with(".swx")
        || name.contains(".tmp.")
        || name == "4913"
        || name == ".DS_Store"
        || name.starts_with(".#")
}

/// SS-FU-1: testable inner for [`build_context_handler`]. The cache
/// refresh runs ONCE per event (before the per-subscriber loop) because
/// `FrontmatterCache::refresh_under_lock` **mutates** the cache —
/// naive per-subscriber fan-out would have subscribers 2..N observe
/// `old == new` and silently skip `try_sync_staleness`. Per-subscriber:
/// `is_relevant_event` filter, emit, and (when `completed_at` changed
/// in the single refresh) `staleness_fn`.
///
/// Invariant: all subscribers at a shared `context_output` are by
/// construction the same team (`team_content_dir_for_watch` only
/// produces identical paths for configs sharing `(org_root, team)`),
/// so `team` resolved from any one subscriber suffices for the single
/// refresh.
///
/// `emit_fn` and `staleness_fn` callbacks let unit tests record per-
/// subscriber side-effects without a live `tauri::AppHandle`.
fn dispatch_context_event(
    event_path: &Path,
    watched_dir: &Path,
    subscribers: &[WatchConfig],
    cache: Option<&SharedFrontmatterCache>,
    mut emit_fn: impl FnMut(&WatchConfig, &serde_json::Value),
) {
    // SW-SC-11 (AC-2/AC-4): a transient atomic-temp sibling — the agent's
    // `<file>.tmp.<pid>.<hash>` write, or any editor swap/backup — is under the
    // watched content dir, so `is_relevant_event` would otherwise treat it as
    // relevant and forward the TEMP path as the change. The open viewer's
    // per-file gate keys on the real filename and can never match it, so the
    // canvas stays stale. Short-circuit BEFORE the cache refresh so the temp
    // produces none of the side-effects (emit, cache mutation, and — at the
    // `build_context_handler` entry — the SW-TS-1 move-detection). Symmetric
    // with `build_reverse_handler`'s existing `is_transient_editor_file`
    // early-return. The real rename destination arrives as its own routable
    // event (verified: macOS FSEvents emits `Modify(Name)` for the final path),
    // so dropping the temp never strands the change (AC-3).
    if is_transient_editor_file(event_path) {
        return;
    }

    // SW-WL-4 (AC-2): zero-subscriber skip of the FAN-OUT. The team context
    // watcher is always-on for the active team (SW-WL-4), so it fires even when
    // no workspace is open. With an empty subscriber snapshot there is nothing to
    // emit to or project into, so early-return before the fan-out. The snapshot
    // is captured before this guard (in `build_context_handler`), so a
    // subscriber closing mid-event cannot skip a refresh that was due (AC-4).
    //
    // SW-LN-B28: the story INDEX still owes this one file a refresh. The index is
    // what `resolve_story_path` / Run gate / MCP look a story up in, and nothing
    // else refreshes it on an external write — so without this, a story an agent
    // writes into a team with no open workspace (always the case for a local org
    // before a worktree opens) stays unresolvable until a restart. Kept cheap: one
    // file, pipeline docs only, never a framework install root.
    let Some(first) = subscribers.first() else {
        debug!(
            target: "watch",
            "context event with no workspace subscriber (index refresh only): {}",
            event_path.display()
        );
        refresh_index_without_subscribers(event_path, watched_dir, cache);
        return;
    };
    let team_opt =
        capture::parse_context_repo_parts(&first.context_repo_path).map(|(team, _, _)| team);
    // SW-CP-2: refresh the frontmatter cache ONCE before the emits, so the UI's
    // pipeline view and the story index reflect this edit.
    //
    // SW-XS-3: an edit no longer enqueues a projection wave — nothing copies
    // context into a workspace on its own. The watcher's remaining jobs are this
    // index refresh and the `context-file-change` emit below.
    if let (Some(c), Some(team)) = (cache, team_opt.as_deref()) {
        let _ = FrontmatterCache::refresh_under_lock(c, event_path, team);
    }

    for config in subscribers {
        if !is_relevant_event(event_path, watched_dir) {
            continue;
        }
        let payload = base_event_payload(event_path, config);
        // SS-FU-1 review iter-1: wrap each per-subscriber callback in
        // `catch_unwind` so a panic in `app.emit` doesn't drop the remaining
        // subscribers' events. The dispatcher's outer
        // catch_unwind catches the panic but breaks this loop early without
        // the per-call wrappers.
        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| emit_fn(config, &payload))) {
            error!(
                target: "watch",
                "dispatch_context_event emit_fn panicked for {} ({}); continuing. payload: {:?}",
                event_path.display(),
                config.repo_path,
                panic
            );
        }
    }
}

/// SW-LN-B28: refresh the story index for ONE changed file when the team content
/// watch fires with no workspace subscriber. The team is the watched content dir's
/// parent segment. Skips anything the index never keys on: a non-pipeline file, a path
/// outside the watched dir, and a framework install root (`.claude/`, `_bmad/`),
/// whose bursts would otherwise cost one read per file.
fn refresh_index_without_subscribers(
    event_path: &Path,
    watched_dir: &Path,
    cache: Option<&SharedFrontmatterCache>,
) {
    let Some(cache) = cache else {
        return;
    };
    if !is_relevant_event(event_path, watched_dir)
        || !crate::commands::files::is_pipeline_file(event_path)
        || team_level_change_payload(event_path, watched_dir).is_none()
    {
        return;
    }
    // The team is the watched content dir's parent (`…/{team}/content`) — read from the
    // watched dir, not the event path, so a `content` segment elsewhere in the home or org
    // path can never be mistaken for the team marker.
    let Some(team) = watched_dir
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
    else {
        return;
    };
    let _ = FrontmatterCache::refresh_under_lock(cache, event_path, team);
}

/// Map notify's canonical paths back to the registered root used by FE ownership.
/// Lexical rebasing also works after a delete; no event-time filesystem read.
fn registered_context_event_path(
    event_path: &Path,
    watched_dir: &Path,
    canonical_dir: &Path,
) -> PathBuf {
    if event_path.starts_with(watched_dir) {
        return event_path.to_path_buf();
    }
    event_path
        .strip_prefix(canonical_dir)
        .map(|relative| watched_dir.join(relative))
        .unwrap_or_else(|_| event_path.to_path_buf())
}

/// Handler for the context-output directory. Snapshots subscribers from
/// `path_to_repo`, resolves the cache from Tauri state, and delegates
/// to [`dispatch_context_event`] with a closure that emits
/// `context-file-change` per subscriber. SW-XS-3: it never enqueues a wave.
fn build_context_handler<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    path_to_repo: PathToRepo,
    dir: PathBuf,
) -> DispatchHandler {
    let watched_dir = dir.clone();
    // Watch registration already runs on the background team-activation worker.
    // notify reports canonical paths on macOS; FE ownership uses registered paths.
    let canonical_dir = std::fs::canonicalize(&dir)
        .unwrap_or_else(|_| crate::content_engine::dispatcher::stable_key(&dir));
    Arc::new(move |event_path: &Path| {
        let registered_path =
            registered_context_event_path(event_path, &watched_dir, &canonical_dir);
        let event_path = registered_path.as_path();
        // SW-SC-11 (AC-2): short-circuit transient atomic-temp siblings at the
        // handler entry — before the subscriber snapshot and the SW-TS-1
        // move-detection below (`existed_before` is false for an uncached temp,
        // which would otherwise drive a spurious `relink_worktrees_for_story`
        // for a `<base>.tmp.<pipeline-ext>` shape). `dispatch_context_event`
        // guards itself too (the unit-test seam + defense in depth); this entry
        // guard additionally spares the snapshot and the move-detection walk.
        if is_transient_editor_file(event_path) {
            return;
        }
        crate::context_query::note_path(&app, event_path);
        let subscribers = snapshot_subscribers(&path_to_repo, &dir);
        // G-8: no workspace subscriber → `dispatch_context_event` skips the
        // fan-out (SW-WL-4), but the team's own surfaces still need to rescan on
        // an external write. One lightweight, repo-less emit — sent AFTER the
        // dispatch below, so the rescan reads the index it just refreshed
        // (SW-LN-B28).
        let team_level = team_level_emit(&subscribers, event_path, &watched_dir);
        let cache_opt: Option<SharedFrontmatterCache> = app
            .try_state::<SharedFrontmatterCache>()
            .map(|s| s.inner().clone());
        // SW-TS-1 perf: snapshot whether this path was already cached
        // *before* dispatch refreshes it. A story newly appearing here is
        // the move signal; an in-place edit was already cached and skips
        // the reverse-lookup walk. Default `true` (skip) when the cache is
        // unavailable — move detection needs it anyway.
        let existed_before = cache_opt
            .as_ref()
            .map(|c| {
                FrontmatterCache::snapshot(c)
                    .entries
                    .contains_key(event_path)
            })
            .unwrap_or(true);
        dispatch_context_event(
            event_path,
            &watched_dir,
            &subscribers,
            cache_opt.as_ref(),
            |_config, payload| {
                // SW-SY-8 (AC-3): the per-path `context-file-change` IPC emit —
                // architecture.md Open-Questions #6's residual (~2,198 events/burst
                // to already-debounced renderer consumers). Count each emit so a
                // burst-correlated spike is attributable to the emit volume.
                crate::content_engine::sync::residual_metrics::note_context_file_change_emit();
                let _ = app.emit("context-file-change", payload.clone());
            },
        );
        if let Some(payload) = team_level {
            crate::content_engine::sync::residual_metrics::note_context_file_change_emit();
            let _ = app.emit("context-file-change", payload);
        }
        // SW-XS-20: this content dir may be a parent of the active team (a
        // parent with its own per-workspace watch): its nav refreshes too.
        emit_inherited_change(&app, event_path);

        // SW-TS-1 (AC-3): relink the worktree records of a story that moved
        // into this watched team. Only on *appearance* (perf gate above);
        // `dispatch_context_event` already refreshed the cache, so read the
        // story_id from it via the shared helper (no second disk read). This
        // is the watched-team half of the composed move detection; the
        // org-root move-watch (`register_worktree_move_watch`) covers teams
        // with no nearer content handler. SW-XS-3: index bookkeeping only —
        // no worktree is re-synced.
        if !existed_before {
            if let Some(cache) = cache_opt.as_ref() {
                if let Some(story_id) =
                    crate::commands::content_engine::story_id_at_path(cache, event_path)
                {
                    crate::commands::content_engine::relink_worktrees_for_story(
                        &story_id, event_path,
                    );
                }
            }
        }
    })
}

/// SW-FX-59: framework install roots that UD-8 folded into two-way content.
/// A burst across one of these (the fold migration, or a `bmad`/skills update)
/// must coalesce to ONE wave per root+team instead of one wave per file
/// (~2,272 waves for the fold, which wedges the renderer). Matched as an EXACT
/// top-level path COMPONENT — never a string prefix — so `_bmad-output` (the
/// authored two-way stories, which merely share the `_bmad` prefix) is never
/// caught. Single update point if a new framework root is folded later.
const FRAMEWORK_BURST_ROOTS: [&str; 2] = [crate::context_repo::CLAUDE_CONFIG_DIR, "_bmad"];

/// G-8: the team-level emit the context handler owes, if any — only with ZERO
/// workspace subscribers (a subscriber gets its own per-subscriber emit from
/// [`dispatch_context_event`], so emitting here too would double-bump).
fn team_level_emit(
    subscribers: &[WatchConfig],
    event_path: &Path,
    watched_dir: &Path,
) -> Option<serde_json::Value> {
    if !subscribers.is_empty() || !is_relevant_event(event_path, watched_dir) {
        return None;
    }
    team_level_change_payload(event_path, watched_dir)
}

/// G-8: the payload for a team-level `context-file-change` — emitted when the team
/// content watch fires with no workspace subscriber, so the team's own surfaces
/// (pipeline, board, hidden-stories badge) still rescan on an external write. It
/// names no repo (`repo_path`/`context_repo_path` empty): the FE routes it to the
/// team owning `file_path`. Root events refresh all open descendants. `None`
/// outside the watched dir and under a framework install root (`.claude/`,
/// `_bmad/`), whose bursts no team surface reads. Pure path math, no I/O.
fn team_level_change_payload(event_path: &Path, watched_dir: &Path) -> Option<serde_json::Value> {
    let rel = event_path.strip_prefix(watched_dir).ok()?;
    let root = rel.components().next().and_then(|c| c.as_os_str().to_str());
    if root.is_some_and(|root| FRAMEWORK_BURST_ROOTS.contains(&root)) {
        return None;
    }
    Some(serde_json::json!({
        "repo_path": "",
        "context_repo_path": "",
        "file_path": event_path.to_string_lossy(),
    }))
}

/// SW-XS-20: emit [`inherited_change_payload`] for `event_path` against the
/// app's active team — from both the team-dir watch and a content dir's own
/// handler, so a parent's change reaches the inheriting team's nav whichever
/// handler owns the path. No-op for a transient editor file or any other path.
fn emit_inherited_change<R: tauri::Runtime>(app: &tauri::AppHandle<R>, event_path: &Path) {
    if is_transient_editor_file(event_path) {
        return;
    }
    let active = app
        .try_state::<crate::mcp::state::ActiveTeamState>()
        .and_then(|state| state.get());
    let active = active
        .as_ref()
        .map(|a| (a.org_slug.as_str(), a.team_name.as_str()));
    if let Some(payload) = inherited_change_payload(event_path, active) {
        crate::content_engine::sync::residual_metrics::note_context_file_change_emit();
        let _ = app.emit("context-file-change", payload);
    }
}

/// SW-XS-20: the team-level `context-file-change` for a change in the content
/// of a team the active team inherits from — the [`team_level_change_payload`]
/// of the parent's content plus `inherited_by`, the active team's content dir,
/// which the FE routes to that team's nav (it shows the parent's files).
/// `None` for any other path, and under a framework install root.
fn inherited_change_payload(
    event_path: &Path,
    active: Option<(&str, &str)>,
) -> Option<serde_json::Value> {
    let (org_root, owner) =
        crate::context_repo::inheritance::inherited_by_active_team(event_path, active)?;
    let (_, active_team) = active?;
    let parent_content = crate::context_repo::path_to_team_content_dir(&org_root, &owner);
    let mut payload = team_level_change_payload(event_path, &parent_content)?;
    payload["inherited_by"] = serde_json::Value::String(
        crate::context_repo::path_to_team_content_dir(&org_root, active_team)
            .to_string_lossy()
            .into_owned(),
    );
    Some(payload)
}

/// SLW-IN-7: enumerate the flat team directories directly under `org_root`
/// (`{org_root}/{team}/`). Post-flatten a team is a slug-safe dir at the repo
/// root, so team dirs are the direct-child directories whose names are neither
/// `_`/`.`-prefixed system entries (`_org`, `_codesense`, `.git`) nor registry
/// files. `worktrees/` is EXCLUDED: it is a non-`_`-prefixed sibling of the
/// team dirs (`{org_root}/worktrees/{branch}/…` — full repo checkouts, heavy
/// churn) that the pre-flatten `_team/`-container watch never saw, so it must
/// not be swept in as a "team" now that teams live at the same level. Best-
/// effort: returns empty on a missing/unreadable org root (the caller retries
/// on the next team activation). Pure enumeration — no watch side effects.
fn flat_team_dirs(org_root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(org_root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter(|e| {
            e.file_name().to_str().is_some_and(|n| {
                !n.starts_with('_')
                    && !n.starts_with('.')
                    && n != crate::context_repo::WORKTREES_DIR
            })
        })
        .map(|e| e.path())
        .collect()
}

/// SW-TS-1 Task 5.1 — register the team-agnostic worktree move-watch. A story
/// moving into *any* team's `content/` folder re-derives the worktree(s)
/// working it (AC-3), regardless of which team is currently active.
///
/// SLW-IN-7: teams are now flat under `org_root` (no `_team/` container), so
/// this attaches one recursive watch per flat team dir ([`flat_team_dirs`])
/// rather than a single `_team/` subtree watch. Watching the per-team dirs
/// (not `org_root` itself) keeps `.git`/`_org`/`_codesense` churn off this
/// watch. New teams are picked up on the next activation (this is re-called
/// idempotently on every team activation).
///
/// **Survives team switches.** Unlike the retired install-root content-layer
/// watches (SW-CP-34), which were unwatched/rebuilt on every team switch, this
/// watch must persist across switches, so it lives here and is never added to a
/// per-switch-rebuilt set. The dispatcher's nearest-ancestor routing means this handler
/// fires only for team-dir paths with no nearer handler — i.e. story changes
/// in teams the per-config content watches don't cover. Watched teams are
/// handled by the composed call inside [`build_context_handler`], so exactly
/// one of the two fires per event.
///
/// Idempotent: safe to call on every team activation. [`ContentWatcher::watch`]
/// dedups the kernel attach and [`Dispatcher::register_handler`] refreshes
/// the captured `AppHandle`. A no-op (logged) when the watcher hasn't started
/// yet — the next team activation re-attempts.
pub fn register_worktree_move_watch<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &WatcherState,
    org_root: &Path,
    org_slug: &str,
) {
    let Some(watcher) = state.watcher_handle() else {
        debug!(
            target: "watch",
            "register_worktree_move_watch: watcher not started yet; deferring ({})",
            org_root.display()
        );
        return;
    };

    let org = org_slug.to_string();
    let panes = crate::panes::watch::app_coalescer(app);
    for team_dir in flat_team_dirs(org_root) {
        let story_moves: DispatchHandler = {
            let app = app.clone();
            let team_dir = team_dir.clone();
            let canonical = team_dir.canonicalize().unwrap_or_else(|_| team_dir.clone());
            let org = org.clone();
            let principal = crate::commands::local_profile::active(&app)
                .ok()
                .map(|p| p.principal);
            Arc::new(move |event_path: &Path| {
                if let (Some(principal), Some(team)) =
                    (&principal, team_dir.file_name().and_then(|s| s.to_str()))
                {
                    let path = registered_context_event_path(event_path, &team_dir, &canonical);
                    crate::media::note_config_path(&app, &team_dir, &org, team, &path);
                    crate::live_hub::agent_definitions::note_path(
                        &app, principal, &team_dir, &org, team, &path,
                    );
                }
                crate::commands::content_engine::handle_story_move_event(&app, event_path);
                // SW-XS-20: a parent team's file changed — the active child
                // team shows it in its nav, so that nav rescans too.
                emit_inherited_change(&app, event_path);
            })
        };
        // A change under the team's `panes/` reloads that custom right pane.
        let handler = crate::panes::watch::team_dir_handler(
            team_dir.clone(),
            org.clone(),
            Arc::clone(&panes),
            story_moves,
        );
        attach_recursive_watch(
            &watcher,
            &state.dispatcher,
            &team_dir,
            handler,
            "register_worktree_move_watch",
        );
    }
}

/// Attach a recursive kernel watch for `dir` with `handler`, creating the
/// directory first (non-fatal on failure — the dir may not exist yet on a
/// brand-new org/team; re-attempted on the next team activation). The
/// single source for the team-scoped attach idiom shared by
/// [`register_worktree_move_watch`] and [`register_team_context_watch`], so
/// the two can't drift. Idempotent: [`ContentWatcher::watch`] dedups the
/// kernel attach (`AlreadyAttached`) and [`Dispatcher::register_handler`]
/// benign-replaces the closure (refreshing its captured `AppHandle`).
pub(crate) fn attach_recursive_watch(
    watcher: &ContentWatcher,
    dispatcher: &Dispatcher,
    dir: &Path,
    handler: DispatchHandler,
    log_ctx: &str,
) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        warn!(
            target: "watch",
            "{}: create_dir_all({}) failed: {}",
            log_ctx, dir.display(), e
        );
        return;
    }
    dispatcher.register_handler(dir, handler);
    if let Err(e) = watcher.watch(WatchDir {
        path: dir.to_path_buf(),
        recursive: true,
    }) {
        warn!(
            target: "watch",
            "{}: watch({}) failed: {}",
            log_ctx, dir.display(), e
        );
    }
}

/// SW-WL-4: swap the owned team context dir to `new_dir`, returning the
/// previously-owned dir(s) that must now be detached. Pure set
/// bookkeeping, separated from the kernel attach in
/// [`register_team_context_watch`] so the diff is unit-testable without a
/// live watcher. Steady state holds at most one owned dir (the active
/// team); a switch leaves exactly the old dir as stale.
fn swap_team_context_dir(owned: &mut HashSet<PathBuf>, new_dir: &Path) -> Vec<PathBuf> {
    let stale: Vec<PathBuf> = owned
        .iter()
        .filter(|d| d.as_path() != new_dir)
        .cloned()
        .collect();
    for d in &stale {
        owned.remove(d);
    }
    owned.insert(new_dir.to_path_buf());
    stale
}

/// SW-WL-4: decide how to retire a stale team context dir on team switch.
/// The dir has already been removed from `team_context_dirs` (ownership
/// relinquished by [`swap_team_context_dir`]), so it reverts to a plain
/// per-workspace dir.
///
/// Returns `true` ("relinquish only — do NOT detach") when a per-workspace
/// subscriber still lingers at the dir (a hard switch racing the frontend's
/// fire-and-forget per-workspace `removeWatch` for the old team) OR when the
/// lock is poisoned (conservative: never yank a possibly-subscribed dir). In
/// that case the ordinary `remove_watch_inner` teardown reclaims the watch
/// when the last lingering subscriber closes.
///
/// Returns `false` ("detach") only when the dir has no subscriber — after
/// dropping its seeded empty key under the same lock — signalling the caller
/// to unregister the handler + unwatch the kernel watch.
fn relinquish_or_detach_stale_dir(path_to_repo: &PathToRepo, dir: &Path) -> bool {
    match path_to_repo.lock() {
        Ok(mut map) => match map.get(dir) {
            Some(subs) if !subs.is_empty() => true,
            _ => {
                map.remove(dir);
                false
            }
        },
        Err(_) => true,
    }
}

/// The watches team activation (`set_active_team_for_wave`) attaches for the
/// active team. SW-XS-4: only the team content watch — the always-on reverse
/// watch on the team's attached main clones is gone, so a main-clone edit is
/// never adopted automatically. A testable seam for the no-automatic-reverse tests.
pub(crate) fn register_team_activation_watches<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &WatcherState,
    org_root: &Path,
    org: &str,
    team: &str,
) {
    *state.recovery_team.lock().unwrap() =
        Some((org_root.to_path_buf(), org.to_owned(), team.to_owned()));
    register_team_context_watch(app, state, org_root, team);
    let team_dir = crate::context_repo::inheritance::team_view(org_root, org, team).own_team_dir();
    recover_context_sources_inner(app, Some(&team_dir), Some(org));
}

/// Catch up existing live consumers after activation or a dropped-event signal.
/// Reuses their normal deltas; an unchanged pane keeps its source revision.
pub(crate) fn recover_context_sources<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    scope: Option<&Path>,
) {
    recover_context_sources_inner(app, scope, None);
}

fn recover_context_sources_inner<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    scope: Option<&Path>,
    activated_org: Option<&str>,
) {
    let scope = scope.map(crate::content_engine::dispatcher::stable_key);
    let scope = scope.as_deref();
    let mut roots = crate::live_hub::files::recovery_roots(app, scope, activated_org.is_some());
    if let Some(state) = app.try_state::<WatcherState>() {
        let known = state.team_context_dirs.lock().unwrap().clone();
        let subscribers = state.path_to_repo.lock().unwrap().clone();
        for root in known.into_iter().chain(
            subscribers
                .values()
                .flatten()
                .map(|config| resolve_watch_dirs(config).context_output),
        ) {
            if scope
                .is_none_or(|path| crate::content_engine::dispatcher::paths_overlap(&root, path))
            {
                roots.insert((root, None));
            }
        }
        let recovery_team = state.recovery_team.lock().unwrap().clone();
        if let Some((org_root, org, team)) = &recovery_team {
            let view: Arc<crate::context_repo::inheritance::TeamView> =
                crate::context_repo::inheritance::team_view(org_root, org, team);
            let child = view.own_content().event_root().to_path_buf();
            if activated_org.is_some()
                || scope.is_none_or(|path| {
                    crate::content_engine::dispatcher::paths_overlap(&child, path)
                })
            {
                roots.insert((child.clone(), None));
            }
            for layer in &view.inherited {
                let content = crate::context_repo::inheritance::InheritedLayer::content(layer);
                if activated_org.is_some() || scope.is_none_or(|path| content.overlaps(path)) {
                    roots.insert((content.event_root().to_path_buf(), Some(child.clone())));
                }
            }
        }
        let scratch = state.scratch_root.lock().unwrap().clone();
        if let Some(scratch) = scratch {
            let mut private_roots: Vec<PathBuf> = Vec::new();
            if let Some(org) = activated_org {
                private_roots.extend(
                    crate::context_repo::scratch_space::org_space_in(&scratch, org)
                        .map(|space| space.content_dir()),
                );
            } else {
                if let Some((_, org, _)) = &recovery_team {
                    private_roots.extend(
                        crate::context_repo::scratch_space::org_space_in(&scratch, org)
                            .map(|space| space.content_dir()),
                    );
                }
                // The scratch layout is orgs/owner/repo/content; do not walk file descendants.
                for owner in std::fs::read_dir(scratch.join("orgs"))
                    .into_iter()
                    .flatten()
                    .filter_map(Result::ok)
                {
                    for repo in std::fs::read_dir(std::fs::DirEntry::path(&owner))
                        .into_iter()
                        .flatten()
                        .filter_map(Result::ok)
                    {
                        let org_space = std::fs::DirEntry::path(&repo);
                        if org_space.is_dir() {
                            private_roots.push(org_space.join("content"));
                        }
                    }
                }
            }
            for root in private_roots {
                if activated_org.is_some()
                    || scope.is_none_or(|path| {
                        crate::content_engine::dispatcher::paths_overlap(&root, path)
                    })
                {
                    roots.insert((root, None));
                }
            }
        }
    }
    for (root, inherited_by) in roots {
        let mut payload =
            super::scratch_watch::content_change_payload(&root).unwrap_or_else(|| {
                serde_json::json!({
                    "repo_path":"", "context_repo_path":"", "file_path":root,
                })
            });
        if let Some(child) = inherited_by {
            payload["inherited_by"] = serde_json::json!(child);
        }
        let _ = app.emit("context-file-change", payload);
    }
    if let Err(error) = crate::live_hub::pane_sources::recover(app, scope, activated_org.is_some())
    {
        warn!(target: "watch", "pane source recovery: {error}");
    }
}

/// SW-WL-4: attach the ACTIVE team's `context_output` watch
/// (`_team/{team}/content/`) on a lifecycle SEPARATE from per-workspace
/// watchers. Called from `set_active_team_for_wave`, which the frontend
/// invokes on first mount AND on every team switch — so this single call
/// covers launch attach, team-switch re-target, and idempotent same-team
/// re-calls (no launch-specific wiring needed).
///
/// Models [`register_worktree_move_watch`] for placement/decoupling, with
/// per-team diffing: the watched dir is per-team, so on a switch the previous
/// team's dir must be detached and the new one attached. The dir is recorded in
/// [`WatcherState::team_context_dirs`] so [`remove_watch_inner`] never
/// tears it down on per-workspace close — only this function detaches a
/// team context dir, and only when switching away from it.
///
/// Reuses [`build_context_handler`]: the dispatch path (snapshot
/// subscribers → zero-subscriber skip → single refresh → per-subscriber
/// fan-out) is identical to the per-workspace case; only the kernel-watch
/// lifecycle differs. The attach steps are all idempotent
/// (`create_dir_all`, `register_handler` benign-replace, `watch` →
/// `AlreadyAttached`), so re-asserting them on a same-team re-call is a
/// cheap no-op that also self-heals across a `stop_watching`/`start_watching`
/// cycle.
pub fn register_team_context_watch<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &WatcherState,
    org_root: &Path,
    team: &str,
) {
    let new_dir = crate::context_repo::path_to_team_content_dir(org_root, team);

    let Some(watcher) = state.watcher_handle() else {
        debug!(
            target: "watch",
            "register_team_context_watch: watcher not started yet; deferring ({})",
            new_dir.display()
        );
        return;
    };

    // Hold the `team_context_dirs` guard across the ENTIRE diff → detach →
    // seed → attach below — do NOT release it after the diff. The frontend
    // fires `set_active_team_for_wave` fire-and-forget and Tauri runs async
    // commands concurrently, so two invocations (rapid A→B→A) can overlap;
    // releasing the lock between diff and act would let one call's detach
    // race the other's attach of the same dir (orphaned watch, or the active
    // team's watch silently killed). Holding it serializes concurrent
    // registrations so the diff-then-act stays atomic. Lock
    // order is `team_context_dirs` → `path_to_repo` (and never the reverse
    // anywhere — `remove_watch_inner`/`start_watching_impl` both take
    // `team_context_dirs` only after releasing `path_to_repo`), so no AB-BA.
    // Steady state holds at most one owned dir; anything != new_dir is stale.
    let mut owned = state
        .team_context_dirs
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let stale_dirs = swap_team_context_dir(&mut owned, &new_dir);

    // Detach the previous team's context watch on switch. `swap_team_context_dir`
    // already relinquished team ownership (removed these from
    // `team_context_dirs`), so each stale dir reverts to a plain per-workspace
    // dir. Tear it down NOW only if no per-workspace subscriber still lingers
    // (the common case — the active team's content dir has no real subscriber).
    // If subscribers remain — e.g. a hard switch races the frontend's
    // fire-and-forget per-workspace `removeWatch` for the old team — DON'T yank
    // the watch out from under a still-open old-team workspace: leave it, and
    // because the dir is no longer a `team_context_dirs` member, the ordinary
    // `remove_watch_inner` teardown reclaims it when that last subscriber
    // closes. Decide + drop the seeded empty key atomically under one lock;
    // detach outside it. Poison-conservative: assume a subscriber lingers.
    for dir in &stale_dirs {
        if relinquish_or_detach_stale_dir(&state.path_to_repo, dir) {
            continue;
        }
        state.dispatcher.unregister_handler(dir);
        // NOTE: this unwatch removes the specific `_team/{team}/content/` watch,
        // but the org-root `_team/` recursive move-watch
        // (`register_worktree_move_watch`) still covers this subtree. Events
        // under the now-detached old-team content dir therefore reroute to the
        // (team-agnostic) move handler via nearest-ancestor dispatch — benign.
        if let Err(e) = watcher.unwatch(dir) {
            debug!(
                target: "watch",
                "register_team_context_watch: unwatch({}) error: {}",
                dir.display(), e
            );
        }
    }

    // Seed an empty subscriber key so per-workspace attaches for this team
    // fast-path (the dir is already a `path_to_repo` key). An empty key is
    // cheap: the handler uses a repo-less team refresh when no workspace
    // subscribes. Pure map insert — order-independent w.r.t. kernel attach.
    if let Ok(mut map) = state.path_to_repo.lock() {
        map.entry(new_dir.clone()).or_default();
    }

    attach_recursive_watch(
        &watcher,
        &state.dispatcher,
        &new_dir,
        build_context_handler(
            app.clone(),
            Arc::clone(&state.path_to_repo),
            new_dir.clone(),
        ),
        "register_team_context_watch",
    );
}

/// SW-WU-14: attach an always-on, non-recursive watch on each managed clone's
/// `.git/worktrees/` so a worktree created ANY way — including a manual
/// `git worktree add` with zero CodeSense involvement — surfaces in the panel with no
/// manual refresh. Git records every worktree as a subdir under
/// `<clone>/.git/worktrees/<name>/`, so a create/remove touches that dir; the handler does
/// nothing but fire the existing `worktree-changed` event and the pane's 150 ms-debounced
/// listener re-runs the authoritative scan (SW-WU-13).
///
/// `clone_dirs` is the SAME deduped clone set the scan enumerates
/// ([`crate::commands::worktree::resolve_managed_clone_dirs`]), so watch-set ≡ scan-set — a
/// clone can never be scanned-but-unwatched. Because epic integration worktrees are created
/// off an already-attached clone (`add_integration_worktree` → `repo.worktree`), this one
/// watch covers story AND epic worktrees.
///
/// Sibling of [`register_worktree_move_watch`] / [`register_team_reverse_watch`]: attached on
/// the team-activation seam (`set_active_team_for_wave`, first mount + every switch) and
/// **idempotent** — [`ContentWatcher::watch`] dedups the kernel attach (`AlreadyAttached`)
/// and [`Dispatcher::register_handler`] benign-replaces the closure, so re-asserting per
/// activation is a cheap no-op. A no-op (logged) when the watcher hasn't started yet — the
/// next activation re-attempts; the Layer-A scan covers the gap.
///
/// The handler runs on notify's background worker thread and does nothing but emit (a
/// non-blocking `app.emit`) — no git, no fs walk on any UI-facing thread. The SAME handler is
/// ALSO registered as a rescan handler so a dropped/overflowed FS event (FSEvents
/// `MustScanSubDirs` / inotify overflow) still recovers via `need_rescan` → `dispatch_rescan`
/// → emit. The watch is **non-recursive**: we only need the per-worktree `<name>/` admin
/// subdir to appear/disappear (worktree add/remove) — a recursive watch would additionally
/// fire on routine intra-worktree git churn (`HEAD`/`index`/`logs` under `<name>/`), driving
/// a needless scan on every commit/checkout.
///
/// `clone_dirs` should be resolved via the blocking pool (`resolve_managed_clone_dirs` shells
/// out to `git`); this function itself only does cheap syscalls (dir stat, `create_dir_all`,
/// kernel attach) like its sibling `register_*` watches.
pub fn register_managed_clone_worktree_watch<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &WatcherState,
    clone_dirs: &[PathBuf],
) {
    let Some(watcher) = state.watcher_handle() else {
        debug!(
            target: "watch",
            "register_managed_clone_worktree_watch: watcher not started yet; deferring ({} clones)",
            clone_dirs.len()
        );
        return;
    };

    // Derive the admin dir per clone, skipping any clone whose `.git` is not a real
    // directory: a bare / separated-git-dir layout would make `create_dir_all` materialize a
    // spurious `.git/worktrees` in the wrong place. Managed clones are always plain
    // (non-bare) `git clone`s, so this only guards a layout the scan also treats as
    // out-of-scope (`discover_worktrees_in`).
    let admin_dirs: Vec<PathBuf> = clone_dirs
        .iter()
        .filter(|clone| clone.join(".git").is_dir())
        .map(|clone| clone.join(".git").join("worktrees"))
        .collect();

    // ONE handler for every dir + both the normal and rescan registries: it ignores the
    // event path and just emits, so it carries no per-clone state — a single shared `Arc`.
    let handler: DispatchHandler = {
        let app = app.clone();
        Arc::new(move |_event_path: &Path| {
            crate::commands::worktree::emit_worktree_changed(&app);
        })
    };

    // Hold `worktree_admin_dirs` across the ENTIRE diff → detach → attach so a rapid
    // fire-and-forget A→B→A switch can't interleave one call's detach with another's attach
    // (mirrors how `register_team_context_watch` holds `team_context_dirs`). Detaching the
    // previous team's clones keeps watch-set ≡ scan-set: `discover_worktrees` scopes the scan
    // to the ACTIVE team's clones, so a prior team's clone must stop being watched on switch.
    let mut owned = state
        .worktree_admin_dirs
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    for dir in swap_worktree_admin_dirs(&mut owned, &admin_dirs) {
        // `unregister_handler` clears BOTH the normal and rescan registries for this dir.
        state.dispatcher.unregister_handler(&dir);
        if let Err(e) = watcher.unwatch(&dir) {
            debug!(
                target: "watch",
                "register_managed_clone_worktree_watch: unwatch({}) error: {}",
                dir.display(), e
            );
        }
    }
    attach_worktree_admin_watches(&watcher, &state.dispatcher, &admin_dirs, &handler);
}

/// SW-WU-14: swap `owned` (the prior activation's watched `.git/worktrees` dirs) for
/// `new_dirs` (the active team's), returning the dirs to DETACH — those in `owned` no longer
/// wanted. Pure set bookkeeping, separated from the kernel detach so the switch diff is
/// unit-testable without a live watcher (mirrors [`swap_team_context_dir`]).
fn swap_worktree_admin_dirs(owned: &mut HashSet<PathBuf>, new_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let wanted: HashSet<&PathBuf> = new_dirs.iter().collect();
    let stale: Vec<PathBuf> = owned
        .iter()
        .filter(|d| !wanted.contains(*d))
        .cloned()
        .collect();
    for d in &stale {
        owned.remove(d);
    }
    for d in new_dirs {
        owned.insert(d.clone());
    }
    stale
}

/// Pure wiring for [`register_managed_clone_worktree_watch`] (no `AppHandle`): NON-recursively
/// watch each `<clone>/.git/worktrees/` admin dir with `handler`, registering it in BOTH the
/// normal and rescan registries. Split out so the attach + rescan wiring is unit-testable
/// with a probe handler (mirrors the injectable-seam pattern of [`build_team_reverse_map`]).
fn attach_worktree_admin_watches(
    watcher: &ContentWatcher,
    dispatcher: &Dispatcher,
    admin_dirs: &[PathBuf],
    handler: &DispatchHandler,
) {
    for dir in admin_dirs {
        // Create the admin dir if the clone has never had a worktree (git creates
        // `.git/worktrees/` lazily on first `worktree add`); non-fatal if it can't.
        if let Err(e) = std::fs::create_dir_all(dir) {
            warn!(
                target: "watch",
                "register_managed_clone_worktree_watch: create_dir_all({}) failed: {}",
                dir.display(), e
            );
            continue;
        }
        dispatcher.register_handler(dir, Arc::clone(handler));
        // Drop-recovery: a rescan/overflow signal on this dir re-fires the same emit.
        dispatcher.register_rescan_handler(dir, Arc::clone(handler));
        // Non-recursive: fire on the `<name>/` subdir add/remove, not intra-worktree churn.
        if let Err(e) = watcher.watch(WatchDir {
            path: dir.clone(),
            recursive: false,
        }) {
            warn!(
                target: "watch",
                "register_managed_clone_worktree_watch: watch({}) failed: {}",
                dir.display(), e
            );
        }
    }
}

/// Handler for the org-level `_codesense/` namespace (SS-8). Any event
/// under that subtree is a `context-file-change` event — the settings
/// panel listens for these to auto-refresh when
/// `worktree/{owner}/{repo}.yaml` changes.
///
/// Distinct from [`build_context_handler`]: the `_codesense/` namespace
/// holds codesense-internal config (worktree.yaml), not story files, so
/// no `refresh_under_lock` / cache work / staleness sync. Plain
/// per-subscriber emit via [`build_fanout_handler`].
///
/// No `is_relevant_event` filter — anything under `_codesense/` is
/// load-bearing for the settings panel by definition.
fn build_codesense_internal_handler<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    path_to_repo: PathToRepo,
    dir: PathBuf,
) -> DispatchHandler {
    build_fanout_handler(path_to_repo, dir, move |event_path, config| {
        let _ = app.emit(
            "context-file-change",
            base_event_payload(event_path, config),
        );
    })
}

/// SS-FU-1: type alias for the optional Phase 2b attach injection
/// seam. Production passes `None` (defaults to `watcher.watch(...)`);
/// AC-6's rollback-isolation test passes a closure that returns `Err`
/// on a chosen registration to exercise the rollback path without
/// engineering a real notify failure (which is racy and
/// platform-specific).
type AttachOverride<'a> =
    Option<&'a (dyn Fn(&WatchDir) -> Result<WatchOutcome, content_engine::Error> + Sync)>;

/// SS-4a CR P3 / P4 + iter-2 M1 / M2 + SS-FU-1: transactional
/// registration of all directory handlers for a single `WatchConfig`.
///
/// Phase 1 (no locks): build the `(path, handler, recursive,
/// handler_type)` tuples for every dir the config wants to attach.
/// Each candidate is gated on `dir.exists()` (CR P4) — non-existent
/// dirs get neither bookkeeping, nor a handler, nor a notify attach.
///
/// Phase 2a (path_to_repo lock briefly per dir): for each registration,
/// append `config` to the subscriber Vec at the dir (replacing any
/// prior entry with the same `repo_path` — SS-FU-1 AC-7 dedup contract),
/// then idempotently register the dispatcher closure for the dir. The
/// closure is **config-agnostic** under the multi-value design (reads
/// subscribers from `path_to_repo` at dispatch time), so re-registering
/// for an already-subscribed shared dir is a benign replace with a
/// functionally-identical closure body. **Append-before-attach** so
/// events delivered between Phase 2a and Phase 2b via prior subscribers'
/// notify attach (shared dirs) still reach the new subscriber.
///
/// Phase 2b: notify attach (only when we have a watcher OR an attach
/// override). On attach failure, roll back: retract this config's
/// `repo_path` from each registered dir's Vec, unregister the
/// dispatcher closure for dirs that become empty, unwatch every
/// FRESHLY-attached canonical (iter-4 Y1 skip-`AlreadyAttached`
/// preserved).
///
/// When neither `watcher` nor `attach_override` is provided (D2:
/// pre-`start_watching` `add_watch` call), Phase 2b is skipped —
/// subscribers and dispatcher closure still commit so the legacy
/// contract is preserved.
///
/// **Cross-handler-type collision invariant (SS-FU-1):** when appending
/// to a non-empty Vec, the new registration's handler type matches the
/// existing subscribers' handler type **by construction**. `context_output`
/// (deterministic per `(org_root, team)` via `team_content_dir_for_watch`)
/// and `codesense_internal_dir` (deterministic per `org_root` via
/// `derive_org_root`) are the only cross-config-collision surfaces;
/// The invariant is enforced by the
/// shape of `resolve_watch_dirs` + the handler-type-per-dir-kind dispatch
/// in this function — not by a runtime guard. If a future call path
/// introduces a cross-handler-type collision, the symptom is that the
/// dispatcher closure registered last wins and the earlier subscribers
/// silently fan out through the wrong handler-type closure.
fn register_directory_handlers<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    dispatcher: &Arc<Dispatcher>,
    path_map: &PathToRepo,
    watcher: Option<&Arc<ContentWatcher>>,
    attach_override: AttachOverride<'_>,
    config: &WatchConfig,
) -> Result<(), String> {
    // SS-FU-1 review iter-1: catch the test-misuse case where both a real
    // watcher AND an attach_override are passed. The override would silently
    // win and the real watcher's `watch(...)` would never be called — masking
    // production behavior in tests. Both-None is also a valid state (the D2
    // pre-`start_watching` `add_watch` path skips Phase 2b entirely); the
    // assertion only forbids the both-Some confusion.
    debug_assert!(
        !(watcher.is_some() && attach_override.is_some()),
        "register_directory_handlers: `watcher` and `attach_override` must not both be Some (both-None is allowed for the pre-start_watching add_watch path)"
    );
    struct Registration {
        path: PathBuf,
        handler: DispatchHandler,
        recursive: bool,
    }

    // SW-XS-4: no projection dest dir is watched any more — a worktree edit is
    // never adopted automatically, so only the context and `_codesense/` dirs
    // are attached here.
    let wd = resolve_watch_dirs(config);

    // Phase 1: build registrations from the single candidate enumeration the
    // fast-path decision also reads (`wd.watch_candidates()`), so the two can't
    // drift. Each candidate's `role` selects its dispatcher handler + recursion;
    // the `CodesenseInternal` role additionally eagerly creates its dir (it may
    // not exist yet on a fresh install) and skips the subscription on failure.
    let mut registrations: Vec<Registration> = Vec::new();
    for WatchDirCandidate { path, role } in wd.watch_candidates() {
        let handler = match role {
            DirRole::Context => {
                build_context_handler(app.clone(), Arc::clone(path_map), path.clone())
            }
            // SS-8 fold (was iter-2/iter-3 legacy patch): subscribe to the
            // org-level `_codesense/` namespace so worktree/{owner}/{repo}.yaml
            // writes emit `context-file-change` (settings panel listens). The
            // dir may not exist yet on a fresh install — eagerly create so the
            // notify attach succeeds (idempotent + cheap); skip on failure.
            DirRole::CodesenseInternal => {
                if let Err(e) = std::fs::create_dir_all(&path) {
                    warn!(
                        target: "watch",
                        "could not create {} (codesense_internal_dir subscription skipped): {}",
                        path.display(),
                        e
                    );
                    continue;
                }
                build_codesense_internal_handler(app.clone(), Arc::clone(path_map), path.clone())
            }
        };
        registrations.push(Registration {
            recursive: role.recursive(),
            path,
            handler,
        });
    }

    // Phase 2a: append subscribers (replace-by-repo_path semantics for
    // AC-7), then idempotently register dispatcher closures. Same
    // handler-type at same dir produces a functionally-identical
    // closure body (reads from path_to_repo at dispatch time), so
    // re-registering for an already-subscribed shared dir is a benign
    // replace. SS-4a iter-3 X4's CLOSURE-level displaced-handler
    // capture is not used (the closure-shared design moots it); the
    // `Option<DispatchHandler>` returned by `register_handler` is
    // discarded.
    //
    // SS-FU-1 review iter-1: track each registration's **subscriber-
    // level displaced** (the prior `WatchConfig` with matching
    // `repo_path`, if any) so a Phase 2b attach failure can restore
    // it. Without this, re-registering an existing `repo_path` then
    // failing attach destroys the original subscription. Also track
    // which dirs we successfully appended to so a mid-loop
    // `append_subscriber_with_replace` poison-Err can roll back the
    // already-committed dirs.
    let mut attempted: Vec<(PathBuf, Option<WatchConfig>)> =
        Vec::with_capacity(registrations.len());
    for reg in &registrations {
        match append_subscriber_with_replace(path_map, &reg.path, config) {
            Ok(displaced) => {
                attempted.push((reg.path.clone(), displaced));
                let _ = dispatcher.register_handler(&reg.path, Arc::clone(&reg.handler));
            }
            Err(e) => {
                rollback_phase_2a(
                    path_map,
                    dispatcher,
                    &attempted,
                    config.subscriber_identity(),
                );
                return Err(e);
            }
        }
    }

    // Phase 2b: notify attach. Use the canonical key returned by
    // `watcher.watch` (or the injected override) for rollback so the
    // unwatch matches the bookkeeping key regardless of symlink
    // aliasing or post-watch directory deletion (iter-2 M2).
    //
    // iter-4 Y1: only record FRESHLY-attached canonicals in
    // `attached_canonicals`. `WatchOutcome::AlreadyAttached` means the
    // canonical key was already in the substrate's bookkeeping from a
    // prior owner's attach — calling `unwatch(canon)` on rollback would
    // destroy that prior owner's kernel watch. Filter dedup-skips out
    // of the rollback list.
    let has_attach_path = watcher.is_some() || attach_override.is_some();
    let mut attached_canonicals: Vec<PathBuf> = Vec::new();
    if has_attach_path {
        for reg in &registrations {
            trace!(
                target: "watch",
                "attaching {:?} (recursive={})",
                reg.path,
                reg.recursive
            );
            let watch_dir = WatchDir {
                path: reg.path.clone(),
                recursive: reg.recursive,
            };
            let result = match attach_override {
                Some(f) => f(&watch_dir),
                None => watcher
                    .expect("has_attach_path guarantees one of watcher/attach_override is Some")
                    .watch(watch_dir),
            };
            match result {
                Ok(WatchOutcome::Attached(canon)) => attached_canonicals.push(canon),
                Ok(WatchOutcome::AlreadyAttached(_)) => {
                    // Prior owner attached this canon; we did not.
                    // Skip rollback for this key — the kernel watch is
                    // still load-bearing for the prior owner.
                }
                Err(e) => {
                    warn!(
                        target: "watch",
                        "watch attach failed for {}: {} (rolling back batch)",
                        reg.path.display(),
                        e
                    );
                    // Substrate rollback: unwatch freshly-attached
                    // canonicals only (skip AlreadyAttached per Y1).
                    if let Some(w) = watcher {
                        for canon in &attached_canonicals {
                            if let Err(unwatch_err) = w.unwatch(canon) {
                                debug!(
                                    target: "watch",
                                    "rollback unwatch error for {}: {}",
                                    canon.display(),
                                    unwatch_err
                                );
                            }
                        }
                    }
                    // Bookkeeping rollback: retract this config's
                    // repo_path from each registered dir's Vec; restore
                    // displaced subscribers; unregister dispatcher
                    // closures for now-empty dirs. Best-effort under
                    // poison — preserves the original attach error
                    // rather than masking it with a lock error.
                    rollback_phase_2a(
                        path_map,
                        dispatcher,
                        &attempted,
                        config.subscriber_identity(),
                    );
                    return Err(format!(
                        "watch attach failed for {}: {}",
                        reg.path.display(),
                        e
                    ));
                }
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

/// SW-WL-6: true when `seq` is still the latest requested whole-watcher
/// lifecycle op (no later `start_watching`/`stop_watching` seq has arrived).
/// Callers load it **under `watcher_lock`** so the last lock-acquirer reads the
/// final [`WatcherState::watcher_lifecycle_seq`] max and only the winner (highest seq =
/// last JS intent) proceeds. Shared by both lifecycle impls so the guard's
/// contract is defined — and tested — in one place.
fn lifecycle_seq_is_current(state: &WatcherState, seq: u64) -> bool {
    state.watcher_lifecycle_seq.load(Ordering::SeqCst) == seq
}

#[tauri::command]
pub async fn start_watching<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    expected_principal: Option<String>,
    watch_configs: Vec<WatchConfig>,
    seq: u64,
) -> Result<(), String> {
    let activation_lock = app.state::<WatcherState>().team_activation_lock.clone();
    let _activation = activation_lock.lock().await;
    let expected = expected_principal
        .as_deref()
        .ok_or("Choose a local profile before watching workspaces")?;
    app.state::<crate::commands::local_profile::LocalProfileState>()
        .check_expected(Some(expected))?;

    info!(target: "watch", "start_watching: config_count={}, seq={seq}", watch_configs.len());
    // SW-WL-6: record the frontend-minted lifecycle seq (JS assigns it in true
    // intent order — see `watcher_lifecycle_seq`). `fetch_max` is order-independent, so
    // it's correct even though this prologue runs at first-poll on a worker.
    let state = app.state::<WatcherState>();
    state.watcher_lifecycle_seq.fetch_max(seq, Ordering::SeqCst);
    await_blocking("start_watching", move || {
        start_watching_impl(app, watch_configs, seq)
    })
    .await
}

pub(crate) fn start_watching_impl<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    watch_configs: Vec<WatchConfig>,
    seq: u64,
) -> Result<(), String> {
    for config in &watch_configs {
        validate_watch_context(config)?;
    }
    let state = app.state::<WatcherState>();

    // SS-4a iter-2 H1: hold `watcher_lock` across the ENTIRE body so
    // concurrent `add_watch` / `remove_watch` / `stop_watching` calls
    // cannot interleave with the rebuild. Without this, an add_watch
    // racing between the old-watcher drop and the new-watcher publish
    // can register handlers + bookkeeping that the new watcher never
    // attaches to (silent dispatch failure), and a partial-failure path
    // can leave the slot `None` with stale `path_to_repo` entries
    // (permanent silent degradation).
    //
    // iter-5 Z3 latency note: the lock is held across N configs ×
    // ~5-8 dirs each = up to ~25-80 kernel `inner.watch(&canon, mode)`
    // syscalls in `register_directory_handlers` Phase 2b. Concurrent
    // `add_watch` / `remove_watch` / `stop_watching` block on
    // `watcher_lock` for that duration. Typical wall-clock is
    // sub-second for ≤10 configs; the price of cross-thread state
    // coherency. Parallel to iter-3 X8's note on `Watcher::unwatch`
    // and iter-4 Y3's note on `add_watch_impl`.
    let mut watcher_lock = state.watcher.lock_or_err()?;

    // SW-WL-6: bail if a later start/stop was requested after us (checked
    // INSIDE `watcher_lock`, so whoever acquires the lock last sees the final
    // seq — last intent wins). Return before touching the watcher slot so a
    // superseding op owns the teardown/rebuild.
    if !lifecycle_seq_is_current(&state, seq) {
        debug!(target: "watch", "start_watching superseded (seq {seq}); skipping");
        return Ok(());
    }

    // CR P7: drop the OLD watcher BEFORE building the new one so the
    // old notify thread exits cleanly. With H1 holding `watcher_lock`
    // for the whole body, no concurrent caller can clone the Arc out
    // of the slot between this `None` write and the publish at the
    // bottom — so the drop is genuine (the only remaining Arc holder
    // is this local variable, dropped at end-of-scope on success).
    *watcher_lock = None;

    // Reset bookkeeping — start_watching is a full reset.
    let registered_before: Vec<PathBuf> = {
        let mut path_map = state.path_to_repo.lock_or_err()?;
        let keys: Vec<PathBuf> = path_map.keys().cloned().collect();
        path_map.clear();
        keys
    };
    for dir in &registered_before {
        state.dispatcher.unregister_handler(dir);
    }

    // SW-WL-4: a full watcher reset detaches every dir, including any
    // team-owned context watch, so clear the ownership marker too. The
    // active team's context watch is re-attached by the
    // `set_active_team_for_wave` call the frontend issues right after boot
    // (and `register_team_context_watch` is idempotent-re-attach).
    if let Ok(mut owned) = state.team_context_dirs.lock() {
        owned.clear();
    }
    // SW-WU-14: same rationale for the worktree `.git/worktrees/` watch marker — the reset
    // detaches its kernel watches, so clear the ownership set; `set_active_team_for_wave`
    // re-attaches idempotently right after boot.
    if let Ok(mut owned) = state.worktree_admin_dirs.lock() {
        owned.clear();
    }

    // Build a fresh watcher with no initial dirs — `register_directory_handlers`
    // calls `watcher.watch(...)` per directory so handler registration and
    // watch attach stay paired.
    let watcher = Arc::new(
        ContentWatcher::new(WatchSetup { dirs: vec![] }, Arc::clone(&state.dispatcher)).map_err(
            |e| {
                error!(target: "watch", "start_watching: failed to create watcher: {}", e);
                format!("failed to create watcher: {}", e)
            },
        )?,
    );

    // iter-2 H1 + SS-FU-1: on partial failure of any config, roll back
    // every config already committed in this start_watching so we
    // don't leave `path_to_repo` with stale entries pointing at a
    // watcher that's about to be dropped.
    let mut completed_configs: Vec<&WatchConfig> = Vec::new();
    for config in &watch_configs {
        if let Err(e) = register_directory_handlers(
            &app,
            &state.dispatcher,
            &state.path_to_repo,
            Some(&watcher),
            None,
            config,
        ) {
            warn!(
                target: "watch",
                "start_watching: register failed for {}: {} (rolling back batch)",
                config.repo_path,
                e
            );
            // SS-FU-1: surgical retract per completed config under
            // multi-value semantics — only drop entries matching the
            // completed config's repo_path, leaving siblings at shared
            // dirs intact. iter-3 X9's "discardable intermediate"
            // contract still holds because no watcher slot publishes
            // on this failure path; the surgical retract is a clarity
            // improvement (the scorched-earth `path_map.remove(d)` of
            // the legacy code wiped shared-dir entries belonging to
            // other completed configs).
            //
            // Also folds in the `codesense_internal_dir` that the
            // legacy `dirs` list omitted — a pre-existing latent gap
            // (org-level dir leaked through partial-failure rollback).
            // The same dir is included in `remove_watch::dirs_to_remove`
            // per iter-3; the rollback loop missed the parallel fix.
            for completed in &completed_configs {
                // SW-CP-9: retract by identity across all keys — covers the
                // manifest reverse dests this config attached, which the old
                // `resolve_watch_dirs(..).all_dirs()` recompute (story-dest
                // only) would have leaked.
                let emptied_dirs = retract_subscriber_all_keys_collect_emptied(
                    &state.path_to_repo,
                    completed.subscriber_identity(),
                )
                .unwrap_or_default();
                for d in &emptied_dirs {
                    state.dispatcher.unregister_handler(d);
                }
            }
            // Drop the in-flight watcher (the local variable's Arc
            // is the only remaining reference). Don't publish.
            drop(watcher);
            return Err(e);
        }
        completed_configs.push(config);
    }

    // Publish the replacement only after all watch registrations succeed.
    *watcher_lock = Some(watcher);
    drop(watcher_lock);
    crate::context_query::bind_rescan_reconciliation(&app, &state.dispatcher);
    crate::live_hub::configuration::watch_prefs(&app, true);

    Ok(())
}

/// SW-WL-6: async + off-thread for the same reason as [`remove_watch`] —
/// `*watcher_lock = None` (below) drops the whole `ContentWatcher`, whose
/// per-path `FsEventWatcher::stop()` can busy-spin ~13s on a stalled FSEvents
/// shutdown handshake. On the former **sync** command that ran on the main
/// thread and froze the UI (a `.hang`); off-thread confines the spin to a
/// worker. The [`WatcherState::watcher_lifecycle_seq`] guard keeps a stop-then-restart
/// deterministic now that both `stop_watching` and `start_watching` run on the
/// blocking pool: the frontend-minted seq (see [`WatcherState::watcher_lifecycle_seq`])
/// is `fetch_max`ed at command entry and re-checked in the impl under
/// `watcher_lock`, so last intent (highest seq) wins regardless of which worker
/// acquires the lock first.
#[tauri::command]
pub async fn stop_watching(
    app: tauri::AppHandle,
    expected_principal: Option<String>,
    seq: u64,
) -> Result<(), String> {
    let activation_lock = app.state::<WatcherState>().team_activation_lock.clone();
    let _activation = activation_lock.lock().await;

    let expected = expected_principal
        .as_deref()
        .ok_or("Choose a local profile before watching workspaces")?;
    app.state::<crate::commands::local_profile::LocalProfileState>()
        .check_expected(Some(expected))?;

    info!(target: "watch", "stop_watching: seq={seq}");
    app.state::<WatcherState>()
        .watcher_lifecycle_seq
        .fetch_max(seq, Ordering::SeqCst);
    await_blocking("stop_watching", move || {
        stop_watching_inner(&app.state::<WatcherState>(), seq)
    })
    .await
}

/// Pure-state body of [`stop_watching`] split out (like [`remove_watch_inner`])
/// so the seq supersession guard + teardown are unit-testable on a bare
/// `WatcherState` without a Tauri runtime.
fn stop_watching_inner(state: &WatcherState, seq: u64) -> Result<(), String> {
    // SS-4a CR P5: take `watcher_lock` FIRST and hold it across the
    // entire teardown so a concurrent `add_watch` (which clones the
    // watcher Arc under the same lock) cannot slip new handler
    // registrations in between the path_to_repo snapshot and the
    // unregister loop. Single critical section.
    let mut watcher_lock = state.watcher.lock_or_err()?;

    // SW-WL-6: bail if a later start/stop superseded us (checked inside
    // `watcher_lock`, matching `start_watching_impl` — last intent wins).
    if !lifecycle_seq_is_current(state, seq) {
        debug!(target: "watch", "stop_watching superseded (seq {seq}); skipping");
        return Ok(());
    }

    let mut path_map = state.path_to_repo.lock_or_err()?;
    let registered: Vec<PathBuf> = path_map.keys().cloned().collect();
    for dir in &registered {
        state.dispatcher.unregister_handler(dir);
    }
    path_map.clear();
    drop(path_map);

    // Drop the watcher Arc — the notify thread exits once the inner
    // `RecommendedWatcher` is dropped.
    *watcher_lock = None;

    // SS-4a CR P8: drain dispatcher suppression set to prevent cross-
    // session bleed. Suppression entries from an in-flight heal/reinstall
    // whose `valid_until` extends past watcher teardown would otherwise
    // match paths the next `start_watching` session is watching. Bounded
    // by `SUPPRESSION_GRACE_MS` (5 s today), but the explicit drain
    // makes the lifecycle deterministic.
    state.dispatcher.suppression_handle().clear();

    Ok(())
}

#[tauri::command]
pub async fn add_watch(
    app: tauri::AppHandle,
    expected_principal: Option<String>,
    watch_config: WatchConfig,
) -> Result<(), String> {
    let activation_lock = app.state::<WatcherState>().team_activation_lock.clone();
    let _activation = activation_lock.lock().await;
    let expected = expected_principal
        .as_deref()
        .ok_or("Choose a local profile before watching workspaces")?;
    app.state::<crate::commands::local_profile::LocalProfileState>()
        .check_expected(Some(expected))?;

    info!(target: "watch", "add_watch: repo_path={}", watch_config.repo_path);
    await_blocking("add_watch", move || add_watch_impl(app, watch_config)).await
}

fn validate_watch_context(config: &WatchConfig) -> Result<(), String> {
    if config.context_repo_path.trim().is_empty() {
        return Ok(());
    }
    let home =
        crate::commands::paths::codesense_home_dir().ok_or("cannot resolve home directory")?;
    crate::context_repo::context_attachments::require_context_path(
        &home,
        Path::new(&config.context_repo_path),
    )
}

fn add_watch_impl(app: tauri::AppHandle, watch_config: WatchConfig) -> Result<(), String> {
    validate_watch_context(&watch_config)?;
    let state = app.state::<WatcherState>();

    // SW-WL-2 (AC-9): re-attach cancels a pending detach, BEFORE we
    // re-register. If this target was closed mid-wave (detach deferred)
    // and is now being re-opened, the queued detach must not fire against
    // the watcher we're about to re-attach. Cancelling first (under the
    // coordinator's `in_flight` lock — see `cancel_pending_detach`) makes
    // the race deterministic: once we proceed past this point the pending
    // entry is gone, so any concurrent drain can only have fired against
    // the pre-reopen watcher, never the one we register below. Done before
    // acquiring `watcher_lock` so the lock graph stays acyclic (AC-10).
    if let Some(coord) = app.try_state::<Arc<WaveCoordinator>>() {
        coord.cancel_pending_detach(
            &PathBuf::from(&watch_config.repo_path),
            &watch_config.context_repo_path,
        );
    }

    // SS-4a iter-3 X2: hold `watcher_lock` across the ENTIRE body so a
    // concurrent `start_watching` cannot publish a new watcher between
    // our Arc clone and our registration. Without this, `add_watch`
    // could attach handlers to a watcher that's about to be replaced
    // (silent dispatch failure for the freshly-attached dirs).
    // Mirrors iter-2 H1's discipline in `start_watching_impl`.
    //
    // iter-4 Y3 latency note: the lock is held across N kernel
    // `inner.watch(&canon, mode)` syscalls in `register_directory_handlers`
    // Phase 2b (one per existing directory in this config; typically
    // 5-8 calls). Concurrent `start_watching` / `stop_watching` /
    // `remove_watch` block on `watcher_lock` for that duration — same
    // trade-off iter-3 X8 documented on `Watcher::unwatch` (the price
    // of cross-thread state coherency). Typical wall-clock is sub-100ms.
    let watcher_lock = state.watcher.lock_or_err()?;

    // SS-4a CR D2: when the watcher slot is empty (caller hasn't yet
    // run `start_watching`), the legacy `add_watch_impl` silently
    // inserted into `path_to_repo` and skipped the notify attach —
    // bookkeeping survived as a queued entry. Preserve that behavior:
    // register the dispatcher handlers + populate `path_to_repo` even
    // when `watcher` is `None`; skip Phase 2 (`watcher.watch(...)`).
    let watcher_ref = watcher_lock.as_ref();

    register_directory_handlers(
        &app,
        &state.dispatcher,
        &state.path_to_repo,
        watcher_ref,
        None,
        &watch_config,
    )
}

/// SW-WL-6: `remove_watch` / [`lazy_detach_workspace`] run the detach on a
/// blocking worker thread via [`await_blocking`], mirroring the attach commands
/// (`add_watch`, [`lazy_attach_workspace`]). The immediate (no-wave) branch of
/// [`gated_detach`] calls [`remove_watch_inner`] inline, whose `notify`
/// `unwatch` → `FsEventWatcher::stop()` can busy-spin ~13s waiting out a stalled
/// FSEvents shutdown handshake. On the former **sync** `#[tauri::command]` that
/// ran on the main thread and froze the whole UI (a `.hang`, not a crash);
/// off-thread confines the spin to a worker. The deferred branch already ran
/// off-thread on the drain thread — this makes the immediate branch match.
///
/// **R-1 (residual, documented-not-fixed — matches SW-WL-2 iter-1 / SW-WL-3
/// R-2 precedent):** moving the immediate detach off the main thread lets it run
/// concurrently with a subsequent same-identity re-attach ([`lazy_attach_workspace`],
/// already async) — i.e. close-then-reopen. A re-attach whose `path_to_repo`
/// lock lands in the ~1-statement window between this detach taking `watcher_lock`
/// ([`remove_watch_inner`]) and its retract ([`retract_subscriber_all_keys_collect_emptied`])
/// can fast-path-append an identity the retract then removes → a missing
/// watcher for a workspace that is (and stays) open. Low-severity by
/// **reachability**: the re-attach is always dispatched *after* the detach, so
/// the sub-µs window is effectively unreachable outside a pathological tight
/// loop; and the immediate branch runs **only when no wave is in flight** (an
/// in-flight wave takes the deferred branch, protected by `cancel_pending_detach`),
/// so nothing is being written — not dataloss. NOTE the impact is *persistent*,
/// not transient: if hit, that workspace's reverse/capture events are dropped
/// for as long as it stays open (it heals only on the next close→reopen or a
/// full `start_watching` reset), so this is severity-bounded by reachability,
/// not by duration. Deterministic fix if ever needed: give
/// [`try_fast_path_attach`] the `watcher_lock` (now cheap — post-SW-WL-6 both the
/// attach and detach run off the UI thread, so SW-WL-3 AC-4's "fast-path must not
/// touch `watcher_lock`" *UI-thread* rationale no longer applies on this path).
#[tauri::command]
pub async fn remove_watch(
    app: tauri::AppHandle,
    expected_principal: Option<String>,
    watch_config: WatchConfig,
) -> Result<(), String> {
    let activation_lock = app.state::<WatcherState>().team_activation_lock.clone();
    let _activation = activation_lock.lock().await;

    let expected = expected_principal
        .as_deref()
        .ok_or("Choose a local profile before watching workspaces")?;
    app.state::<crate::commands::local_profile::LocalProfileState>()
        .check_expected(Some(expected))?;

    info!(target: "watch", "remove_watch: repo_path={}", watch_config.repo_path);
    await_blocking("remove_watch", move || gated_detach(&app, &watch_config)).await
}

/// SW-WL-3 (F-2): the wave-gated detach shared by [`remove_watch`] and
/// [`lazy_detach_workspace`].
///
/// SW-WL-2: gate detach behind the wave-in-flight signal. If a wave is
/// materializing into this target, defer the actual detach until the wave
/// drains — otherwise the wave's own post-write FS events route to nobody and
/// lockfile state silently diverges from on-disk truth (the
/// `LocallyModified`-skip symptom). The gate returns immediately either way
/// and never blocks the (fire-and-forget) caller (W5): when no wave is in
/// flight it detaches now; when one is, it queues a deferred detach that the
/// last wave to drain fires (`InFlightGuard::drop`). WL-3's surface-bound
/// close routes through this same primitive, so it inherits the gate
/// (W4 / WL-3 AC-9).
/// Remove attached-context subscriptions through the existing wave drain gate.
/// Caller holds the active-team transaction lock; other contexts keep their watches.
pub(crate) fn retire_profile_watches(app: &tauri::AppHandle) -> Result<(), String> {
    let Some(state) = app.try_state::<WatcherState>() else {
        return Ok(());
    };
    state.watcher_lifecycle_seq.store(0, Ordering::SeqCst);
    let watcher = state.watcher_handle();
    let watched = watcher
        .as_ref()
        .map(|w| w.watched_paths())
        .transpose()
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    let configs: Vec<_> = state
        .path_to_repo
        .lock_or_err()?
        .values()
        .flatten()
        .cloned()
        .collect();
    state.team_context_dirs.lock_or_err()?.clear();
    state.worktree_admin_dirs.lock_or_err()?.clear();
    for config in configs {
        gated_detach(app, &config)?;
    }
    // Retire always-on handlers too (worktree moves and clone admin dirs are
    // intentionally absent from the per-workspace subscription map).
    for dir in watched {
        state.dispatcher.unregister_handler(&dir);
        if let Some(watcher) = &watcher {
            watcher.unwatch(&dir).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

pub(crate) fn detach_context_watches(app: &tauri::AppHandle, root: &Path) -> Result<(), String> {
    let Some(state) = app.try_state::<WatcherState>() else {
        return Ok(());
    };
    let configs: Vec<_> = state
        .path_to_repo
        .lock_or_err()?
        .values()
        .flatten()
        .filter(|c| Path::new(&c.context_repo_path).starts_with(root))
        .cloned()
        .collect();
    state
        .team_context_dirs
        .lock_or_err()?
        .retain(|p| !p.starts_with(root));
    for config in configs {
        gated_detach(app, &config)?;
    }
    let stale: Vec<_> = state
        .path_to_repo
        .lock_or_err()?
        .keys()
        .filter(|p| p.starts_with(root))
        .cloned()
        .collect();
    for dir in stale {
        if !relinquish_or_detach_stale_dir(&state.path_to_repo, &dir) {
            state.dispatcher.unregister_handler(&dir);
            if let Some(watcher) = state.watcher_handle() {
                let _ = watcher.unwatch(&dir);
            }
        }
    }
    Ok(())
}

fn gated_detach<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    watch_config: &WatchConfig,
) -> Result<(), String> {
    if let Some(coord) = app.try_state::<Arc<WaveCoordinator>>() {
        let coord = Arc::clone(coord.inner());
        let target = PathBuf::from(&watch_config.repo_path);
        // Deferred/immediate detach as one capability (DI seam, not a
        // module dep): captures the AppHandle + config and replays
        // `remove_watch_inner`. The coordinator invokes it without naming
        // the watcher. Error is logged-and-swallowed — the JS caller is
        // already fire-and-forget (`.catch(() => {})`), matching the prior
        // contract where the returned Result was discarded.
        let app_for_detach = app.clone();
        let cfg = watch_config.clone();
        let detach: DetachFn = Arc::new(move || {
            let state = app_for_detach.state::<WatcherState>();
            if let Err(e) = remove_watch_inner(&state, &cfg) {
                debug!(target: "watch", "deferred remove_watch failed: {}", e);
            }
        });
        // Key the deferral by the subscriber's composite identity
        // (repo_path, context_repo_path) — two subscribers sharing a
        // repo_path but differing in context (SW-WL-1) must not clobber
        // each other's deferred detach.
        coord.detach_now_or_defer(&target, &watch_config.context_repo_path, detach);
        Ok(())
    } else {
        // No coordinator registered (pre-`setup()`, or a bare test
        // harness): no waves can exist, so detach synchronously — today's
        // behavior, unchanged (AC-11).
        let state = app.state::<WatcherState>();
        remove_watch_inner(&state, watch_config)
    }
}

/// SW-WL-3 (AC-2): workspace-aware sibling of [`add_watch`]. The frontend
/// `WorkspaceOpenSet` invokes this on a workspace's 0→1 open transition.
/// SW-XS-3: attaching is watch bookkeeping only — it no longer enqueues a
/// reconcile, so opening or focusing a workspace never writes into it.
/// T2 attaches via the shared register path (slow path through
/// `add_watch_impl`); T3 adds the fast-path that skips
/// `register_directory_handlers` when every dir is already covered. Re-attach
/// cancels a pending detach (AC-9), inherited from `add_watch_impl`.
#[tauri::command]
pub async fn lazy_attach_workspace<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    expected_principal: Option<String>,
    workspace_id: String,
    source_repo_slug: String,
    watch_config: WatchConfig,
) -> Result<(), String> {
    let activation_lock = app.state::<WatcherState>().team_activation_lock.clone();
    let _activation = activation_lock.lock().await;
    let expected = expected_principal
        .as_deref()
        .ok_or("Choose a local profile before watching workspaces")?;
    app.state::<crate::commands::local_profile::LocalProfileState>()
        .check_expected(Some(expected))?;

    info!(
        target: "watch",
        "lazy_attach_workspace: workspace_id={}, repo_path={}, context_repo_path={}",
        workspace_id, watch_config.repo_path, watch_config.context_repo_path,
    );
    // SW-LN-B51: without a team context path the workspace never subscribes to the team
    // content watch, so it gets no `context-file-change` refresh. Say so rather than fail
    // silently.
    if !source_repo_slug.is_empty() && team_content_dir_for_watch(&watch_config).is_none() {
        warn!(
            target: "watch",
            "lazy_attach_workspace: {} has no team context path ({:?}); team context edits \
             will not refresh it until it re-attaches",
            watch_config.repo_path, watch_config.context_repo_path,
        );
    }

    await_blocking("lazy_attach_workspace", move || {
        lazy_attach_impl(app, watch_config)
    })
    .await?;
    // SW-XS-4: opening a workspace adopts nothing — the open re-claim is gone.
    Ok(())
}

/// SLW-FX-25: register directory handlers for `lazy_attach`'s slow path WITHOUT
/// holding the process-global `state.watcher` guard across the N recursive kernel
/// `inner.watch` syscalls, while staying safe against a concurrent
/// `start_watching` watcher swap.
///
/// **Why this exists.** The slow path used to hold `state.watcher` across the
/// whole `register_directory_handlers` call (N `canonicalize` + recursive kernel
/// watches over a freshly-created worktree checkout). Because that one
/// `std::sync::Mutex` also guards `start_watching`/`add_watch`/`remove_watch`, N
/// concurrent worktree creates each grabbed it in turn and parked the OS thread
/// across their registration — serializing the batch and blocking every other
/// watch-touching IPC (open, nav, scope change) behind the in-flight recursive
/// watch. That is the reported "creating multiple worktrees at once freezes the
/// UI" symptom. SLW-FX-24 moved this work onto the blocking pool; the residual is
/// the *lock scope*, one layer down.
///
/// **Why the naive fix is unsafe.** Cloning the `Arc`, dropping the guard, and
/// registering against the clone would reintroduce the watcher-swap race the lock
/// exists to prevent: a concurrent `start_watching` sets the slot to `None`,
/// clears `path_to_repo`, and republishes a fresh watcher — so our
/// `register_directory_handlers` (Phase 2a writes `path_to_repo`, Phase 2b
/// attaches kernel watches) would strand its handlers + kernel watches on the
/// *dropped* watcher → silent, permanent dispatch loss.
///
/// **The fix.** (A) Under the guard, clone the live watcher `Arc` and DROP the
/// guard, retaining the clone (`owned`) for the whole call. (If the slot is empty
/// — cold start or post-`stop_watching` — there is no watcher, so `register` does
/// bookkeeping only, Phase 2b is skipped, and there's nothing to parallelize: we
/// just register under the held guard, exactly like the old path, and return.)
/// (B) Run `register` lock-free against the owned clone — the common, uncontended
/// case, where concurrent attaches now run their kernel watches in parallel with
/// the global mutex free. (C) Re-acquire the guard and compare the CURRENT slot to `owned`
/// by `Arc` identity: if the slot still holds the same watcher, the optimistic
/// register stands; if a different watcher is live now (a `start_watching`
/// swapped it in during our window), re-run `register` while STILL HOLDING the
/// guard — the guaranteed-correct serialized fallback, behaviorally identical to
/// the pre-SLW-FX-25 path; if the slot is empty now (a `stop_watching` tore the
/// watcher down), there is nothing to attach to and phase-B's bookkeeping stands
/// for the next `start_watching`. Re-running is idempotent: Phase 2a's
/// `append_subscriber_with_replace` is replace-by-repo_path and Phase 2b's
/// `.watch()` returns `AlreadyAttached` for any already-keyed dir, so a partial
/// optimistic attempt heals without double-attaching.
///
/// **Why `Arc::ptr_eq`, not `watcher_lifecycle_seq`.** `start_watching` bumps the
/// lifecycle seq in its async prologue (`watch.rs` ~`:2196`) BEFORE it acquires
/// `state.watcher` and swaps the slot. So a phase-A snapshot can observe the
/// already-bumped seq while still cloning the OLD watcher; a seq-only phase-C
/// check would then read "current" and trust a register bound to the swapped-out
/// watcher (silent dispatch loss — the very bug this guards). Arc identity is
/// ordering-independent and exact, and is ABA-safe here because the retained
/// `owned` clone pins the old allocation, so a genuine swap is always a distinct
/// pointer.
///
/// `register` is handed the `Option<&Arc<ContentWatcher>>` the funnel expects and
/// must never itself lock `state.watcher` (it doesn't — the funnel takes the
/// watcher by parameter), so the phase-C re-run under the held guard cannot
/// self-deadlock.
fn register_revalidating_watcher_lifecycle<F>(
    state: &WatcherState,
    register: F,
) -> Result<(), String>
where
    F: Fn(Option<&Arc<ContentWatcher>>) -> Result<(), String>,
{
    // Phase A: take the guard and read the slot. Clone the live watcher Arc (kept
    // for the whole call so it pins the old allocation → phase-C `Arc::ptr_eq` is
    // ABA-safe), then drop the guard.
    let owned = {
        let guard = state.watcher.lock_or_err()?;
        match guard.as_ref() {
            // No watcher published yet (cold start, or post-`stop_watching`).
            // `register` runs BOOKKEEPING-ONLY here — Phase 2b (the N kernel
            // watches) is skipped when there's no watcher — so there is nothing to
            // keep off the lock and no swap to race. Register under the HELD guard,
            // exactly like the pre-SLW-FX-25 path (this also blocks a concurrent
            // `start_watching`, preserving the old serialized behavior for this
            // cheap, freeze-irrelevant case). The lock-free dance below is reserved
            // for the case that actually caused the freeze: a live watcher whose
            // recursive kernel watches we want to run off the global mutex.
            None => return register(None),
            Some(w) => Arc::clone(w),
        }
    };

    // Phase B: register lock-free against the owned clone. The N recursive kernel
    // watches run here with `state.watcher` FREE — concurrent attaches (and every
    // other watch-touching command) no longer serialize behind us.
    register(Some(&owned))?;

    // Phase C: re-validate against the CURRENT slot by Arc identity (see the
    // fn-doc for why not `watcher_lifecycle_seq`).
    let guard = state.watcher.lock_or_err()?;
    match guard.as_ref() {
        // The slot still holds the exact watcher we registered against → the
        // optimistic register stands.
        Some(current) if Arc::ptr_eq(&owned, current) => Ok(()),
        // The slot was emptied by a concurrent `stop_watching` AFTER our optimistic
        // attach. Nothing live to attach to; the kernel watches we made on `owned`
        // die when it drops at return, and the next `start_watching`'s
        // `path_to_repo.clear()` reaps any bookkeeping phase-B left. Re-registering
        // here would only re-stamp orphan entries the stop just cleared.
        None => Ok(()),
        // A DIFFERENT watcher is live — `start_watching` swapped it in during our
        // lock-free window. Re-register against it under the held guard.
        Some(_) => {
            debug!(
                target: "watch",
                "lazy_attach: watcher swapped during lock-free register; \
                 re-registering against the live watcher under the held guard"
            );
            register(guard.as_ref())
        }
    }
}

/// SW-WL-3 (AC-4): the fast-path/slow-path attach body for
/// [`lazy_attach_workspace`]. Surface-bound lifecycle multiplies attach
/// frequency (every workspace open), so the common case — a workspace whose
/// dirs are already watched by a sibling subscriber (SS-FU-1 fan-out) — MUST
/// NOT take `watcher_lock`, which `register_directory_handlers` holds across N
/// kernel `inner.watch` syscalls. Otherwise the UI thread blocks on open when
/// a `start_watching` rebuild is in flight. SLW-FX-25: the slow path likewise
/// no longer holds `watcher_lock` across the register — it delegates to
/// [`register_revalidating_watcher_lifecycle`] so concurrent creates don't
/// serialize on the global mutex (see that fn for the swap-safety argument).
fn lazy_attach_impl<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    watch_config: WatchConfig,
) -> Result<(), String> {
    validate_watch_context(&watch_config)?;
    let state = app.state::<WatcherState>();

    // AC-9: re-attach cancels a pending detach (same discipline as
    // `add_watch_impl`), BEFORE we touch `path_to_repo` / `watcher_lock` so
    // the lock graph stays acyclic.
    if let Some(coord) = app.try_state::<Arc<WaveCoordinator>>() {
        coord.cancel_pending_detach(
            &PathBuf::from(&watch_config.repo_path),
            &watch_config.context_repo_path,
        );
    }

    // The fast-path coverage check compares against the SAME dirs the slow-path
    // register would attach.
    let wd = resolve_watch_dirs(&watch_config);
    let candidates = wd.fast_path_candidate_dirs();

    // Fast-path: every dir this config would attach is already covered, so the
    // existing dispatcher closures (which read `path_to_repo` at dispatch time)
    // fan out to the freshly-appended subscriber. No `watcher_lock`, no kernel
    // calls — the UI thread cannot block here on a concurrent rebuild.
    if try_fast_path_attach(&state.path_to_repo, &candidates, &watch_config)? {
        debug!(
            target: "watch",
            "lazy_attach_workspace: fast-path append for {} ({} dirs, no watcher_lock)",
            watch_config.repo_path,
            candidates.len(),
        );
        return Ok(());
    }

    // Slow-path: at least one net-new dir needs a kernel watch. SLW-FX-25: do
    // NOT hold `watcher_lock` across the N recursive kernel watches (that
    // serialized concurrent creates and froze the UI). Register against a clone
    // of the live watcher with the guard dropped, then re-validate the watcher
    // lifecycle so a concurrent `start_watching` swap can't strand the attach on
    // a dropped watcher. AlreadyAttached dirs are handled idempotently inside
    // (subscriber replace + dedup-skip), which also makes the phase-C re-run safe.
    register_revalidating_watcher_lifecycle(&state, |watcher| {
        register_directory_handlers(
            &app,
            &state.dispatcher,
            &state.path_to_repo,
            watcher,
            None,
            &watch_config,
        )
    })
}

/// SW-WL-3 (AC-2 / AC-9): workspace-aware sibling of [`remove_watch`].
/// Invoked on a workspace's 1→0 close transition. Routes detach through
/// WL-2's wave-in-flight gate exactly like `remove_watch` (shared
/// [`gated_detach`]).
///
/// SW-WL-6: async + `await_blocking` for the same reason as [`remove_watch`] —
/// keep the (possibly ~13s-spinning) `notify` `stop()` off the main thread. See
/// [`remove_watch`] for the full rationale and the R-1 residual-race note.
#[tauri::command]
pub async fn lazy_detach_workspace(
    app: tauri::AppHandle,
    expected_principal: Option<String>,
    workspace_id: String,
    watch_config: WatchConfig,
) -> Result<(), String> {
    let activation_lock = app.state::<WatcherState>().team_activation_lock.clone();
    let _activation = activation_lock.lock().await;

    let expected = expected_principal
        .as_deref()
        .ok_or("Choose a local profile before watching workspaces")?;
    app.state::<crate::commands::local_profile::LocalProfileState>()
        .check_expected(Some(expected))?;

    info!(
        target: "watch",
        "lazy_detach_workspace: workspace_id={}, repo_path={}",
        workspace_id, watch_config.repo_path,
    );
    await_blocking("lazy_detach_workspace", move || {
        gated_detach(&app, &watch_config)
    })
    .await
}

/// SS-FU-1 review iter-1: pure-state body of [`remove_watch`] split out
/// so unit tests can exercise the wired-up retract+unregister path
/// directly on a `WatcherState` (no `tauri::AppHandle` required). The
/// `#[tauri::command]` wrapper above only adds logging and resolves
/// `state` from `app`. Auditor M-1.
fn remove_watch_inner(state: &WatcherState, watch_config: &WatchConfig) -> Result<(), String> {
    // SW-CP-9: teardown retracts by subscriber identity across ALL watched
    // dirs (below), so no dir recompute is needed here — which is required,
    // since remove_watch_inner has no `ResolverContext` to resolve the
    // manifest reverse-dest set and a recompute would leak since-removed
    // projection dests.

    // SS-4a iter-3 X3: hold `watcher_lock` across the ENTIRE body so a
    // concurrent `start_watching` cannot publish a new watcher between
    // our Arc clone and the unregister loop. Without this, the new
    // watcher's freshly-registered handler for a shared dir gets
    // unregistered by this call (silent dispatch failure). Mirrors
    // iter-2 H1 / iter-3 X2 discipline.
    let watcher_lock = state.watcher.lock_or_err()?;
    let watcher_ref = watcher_lock.as_ref();

    // SW-CP-9: retract THIS config's subscriber from EVERY dir it attached
    // (the org-level `_codesense/`, context, install, and all reverse dest
    // dirs), collecting dirs that became empty so we only unregister the
    // dispatcher closure + `watcher.unwatch` for keys no other config still
    // subscribes to (AC-3 preserves siblings; AC-4 detaches on last-subscriber
    // removal). Scanning all keys (vs the old `all_dirs()` recompute) is what
    // makes reverse-dest teardown manifest-independent. Match by subscriber
    // identity (`(repo_path, context_repo_path)`, SW-WL-1) — not full
    // WatchConfig equality — so callers that mutate non-identity fields (e.g.
    // viewer_root) and re-emit `remove_watch` still retract correctly, while a
    // colliding-`repo_path` sibling that differs in `context_repo_path` is left
    // untouched.
    let emptied_dirs = retract_subscriber_all_keys_collect_emptied(
        &state.path_to_repo,
        watch_config.subscriber_identity(),
    )?;

    // SW-WL-4: never tear down a team-owned context watch on per-workspace
    // close. When the active team's `context_output` loses its last
    // per-workspace subscriber, `retract_subscriber_collect_emptied`
    // reports it emptied and dropped its key — but the kernel watch +
    // dispatcher handler belong to the TEAM lifecycle, not this workspace.
    // `ContentWatcher.watched` is membership-not-refcount, so a single
    // `unwatch` here would detach the always-on watch for everyone. Re-seed
    // the empty key (keeps the dir a fast-path-able `path_to_repo` key) and
    // skip the unwatch/unregister; only `register_team_context_watch`
    // detaches these, and only on team switch.
    // Read poison-conservatively — RECOVER the guard rather than defaulting to
    // an empty set. An empty set here would make the `team_dirs.contains(dir)`
    // exclusion below silently fail and `unwatch` the team-owned dir — the
    // exact "detach the always-on watch for everyone" failure this story
    // exists to prevent. `into_inner()` matches `register_team_context_watch`.
    let team_dirs = match state.team_context_dirs.lock() {
        Ok(g) => g.clone(),
        Err(p) => p.into_inner().clone(),
    };

    for dir in &emptied_dirs {
        if team_dirs.contains(dir) {
            if let Ok(mut map) = state.path_to_repo.lock() {
                map.entry(dir.clone()).or_default();
            }
            continue;
        }
        state.dispatcher.unregister_handler(dir);
        if let Some(watcher) = watcher_ref {
            debug!(target: "watch", "remove_watch: detaching {:?}", dir);
            if let Err(e) = watcher.unwatch(dir) {
                debug!(target: "watch", "unwatch error for {}: {}", dir.display(), e);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    #[test]
    fn flat_team_dirs_excludes_worktrees_and_system_entries() {
        // SLW-IN-7: post-flatten, teams are direct children of `org_root`
        // alongside `worktrees/`, `_codesense/`, `.git/`. The move-watch
        // enumerator must return ONLY real team dirs — never `worktrees/` (heavy
        // repo-checkout churn the old `_team/`-container watch never saw).
        let tmp = tempfile::TempDir::new().unwrap();
        let org_root = tmp.path();
        for d in ["default", "backend", "worktrees", "_codesense", ".git"] {
            std::fs::create_dir_all(org_root.join(d)).unwrap();
        }
        std::fs::write(org_root.join("teams.yml"), b"teams: []\n").unwrap();

        let mut got: Vec<String> = flat_team_dirs(org_root)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        got.sort();
        assert_eq!(got, vec!["backend".to_string(), "default".to_string()]);
    }

    // SW-CP-4 F4: the reverse handler skips editor swap/backup/atomic-temp files
    // (so they're never auto-adopted into the source layer) while real content of
    // any extension passes.
    #[test]
    fn reverse_handler_skips_transient_editor_files() {
        for f in [
            "_bmad-output/.story.md.swp",
            "_bmad-output/story.md.swo",
            "_bmad-output/story.md~",
            "_bmad-output/4913",
            "_bmad-output/story.md.tmp.5123.7",
            "_bmad-output/.DS_Store",
            "_bmad-output/.#story.md",
        ] {
            assert!(
                is_transient_editor_file(Path::new(f)),
                "{f} should be skipped"
            );
        }
        for f in [
            "_bmad-output/story.md",
            "_bmad-output/impl/SW-CP-4.md",
            "docs/openapi.yaml",
            "_bmad-output/notes.txt",
        ] {
            assert!(
                !is_transient_editor_file(Path::new(f)),
                "{f} is real content"
            );
        }
    }

    #[test]
    fn test_watcher_state_default_is_idle() {
        // SS-4a (rewrite of test_watcher_state_default_is_empty): shape
        // changed — `healing_paths` gone, `dispatcher` added.
        //
        // iter-3 X5: strengthened from iter-2 M3's "assert false on
        // never-inserted path" (which would pass for the wrong reasons).
        // This version does a full insert → assert suppressed → clear
        // → assert NOT suppressed round-trip to actually prove that
        // `clear()` and `is_suppressed` honor their contracts, and that
        // a cloned handle shares state with the original.
        let state = WatcherState::default();
        assert!(state.watcher.lock().unwrap().is_none());
        assert!(state.path_to_repo.lock().unwrap().is_empty());

        let handle = state.dispatcher.suppression_handle();
        let path = PathBuf::from("/never-touched/file.md");

        // Default state: nothing suppressed.
        assert!(
            !handle.is_suppressed(&path),
            "default WatcherState must not report any path as suppressed"
        );

        // Insert via the handle; round-trip is_suppressed.
        handle.insert_paths(std::slice::from_ref(&path));
        assert!(
            handle.is_suppressed(&path),
            "after insert_paths, path must be suppressed"
        );

        // Cloned handle shares the underlying Arc<Mutex<...>>;
        // is_suppressed observes the same set as the original.
        let cloned = handle.clone();
        assert!(
            cloned.is_suppressed(&path),
            "cloned handle must share state with original (observe inserted path as suppressed)"
        );

        // clear() drains the set; both handles observe the drain.
        handle.clear();
        assert!(
            !handle.is_suppressed(&path),
            "after clear, path must no longer be suppressed (original handle)"
        );
        assert!(
            !cloned.is_suppressed(&path),
            "after clear, path must no longer be suppressed (cloned handle observes shared drain)"
        );

        // clear() on an already-empty set is a safe no-op.
        cloned.clear();
        assert!(!cloned.is_suppressed(&path));
    }

    // -----------------------------------------------------------------
    // SS.8 iter-3 patches: covers the watcher classifier + abs filter
    // -----------------------------------------------------------------

    #[test]
    fn test_iter3_resolve_watch_dirs_populates_codesense_internal_dir() {
        // For a well-formed flat context_repo_path containing
        // `/{team}/content/...`, resolve_watch_dirs derives the org root
        // and stores `{org_root}/_codesense/` so the watcher knows to
        // subscribe + classify events under it.
        let config = WatchConfig {
            repo_path: "/repo".into(),
            context_repo_path: "/home/dev/.codesense/users/42/orgs/acme/api/default/content".into(),
        };
        let wd = resolve_watch_dirs(&config);
        assert_eq!(
            wd.codesense_internal_dir,
            Some(PathBuf::from(
                "/home/dev/.codesense/users/42/orgs/acme/api/_codesense"
            )),
        );
    }

    #[test]
    fn test_iter3_resolve_watch_dirs_drops_relative_codesense_internal_dir() {
        // Defense-in-depth: a relative `context_repo_path` (test fixture
        // or malformed caller) must NOT cause the watcher to subscribe to
        // a CWD-relative `_codesense/` dir. resolve_watch_dirs's
        // `is_absolute()` filter drops it.
        let config = WatchConfig {
            repo_path: "/repo".into(),
            context_repo_path: "relative/_team/default/content".into(),
        };
        let wd = resolve_watch_dirs(&config);
        assert_eq!(wd.codesense_internal_dir, None);
    }

    #[tokio::test]
    async fn team_activation_transaction_serializes_all_final_ownership_to_later_team() {
        #[derive(Default)]
        struct Projection {
            active: &'static str,
            context_watch: &'static str,
            mutations: Vec<&'static str>,
        }

        let state = Arc::new(WatcherState::default());
        let projection = Arc::new(Mutex::new(Projection::default()));
        let a_acquired = Arc::new(tokio::sync::Notify::new());
        let release_a = Arc::new(tokio::sync::Notify::new());

        let a = {
            let state = Arc::clone(&state);
            let projection = Arc::clone(&projection);
            let a_acquired = Arc::clone(&a_acquired);
            let release_a = Arc::clone(&release_a);
            tokio::spawn(async move {
                let _transaction = state.lock_team_activation().await;
                a_acquired.notify_one();
                release_a.notified().await;
                let mut projection = projection.lock().unwrap();
                projection.active = "A";
                projection.context_watch = "A";
                projection.mutations.push("A");
            })
        };
        a_acquired.notified().await;
        let b = {
            let state = Arc::clone(&state);
            let projection = Arc::clone(&projection);
            tokio::spawn(async move {
                let _transaction = state.lock_team_activation().await;
                let mut projection = projection.lock().unwrap();
                projection.active = "B";
                projection.context_watch = "B";
                projection.mutations.push("B");
            })
        };
        tokio::task::yield_now().await;
        release_a.notify_one();
        a.await.unwrap();
        b.await.unwrap();

        let projection = projection.lock().unwrap();
        assert_eq!(projection.active, "B");
        assert_eq!(projection.context_watch, "B");
        assert_eq!(projection.mutations, vec!["A", "B"]);
    }

    // -----------------------------------------------------------------
    // SW-CP-9: manifest-driven reverse-watch enumeration
    // -----------------------------------------------------------------

    // SLW-IN-12: the reverse watch-set ctx is now built by `derive_resolver_context`
    // (full chain + provider). The builder itself is pinned in `commands::content_engine`
    // (`derive_resolver_context_carries_parent_chain_for_reverse_watch` for the chain,
    // `derive_resolver_context_resolves_provider_when_mapped_and_clone_present` for the
    // provider stamp), and that the stamped ctx actually surfaces the inherited /
    // provider-overlaid dest dirs in the enumerated watch set is pinned END-TO-END in
    // `install::story_linker` (`query_projection_dests_includes_parent_inherited_dests_across_surfaces`
    // and `query_projection_dests_includes_provider_overlaid_dest`) — together replacing
    // the deleted `reverse_dest_ctx` unit test.

    // SS-4a fold: the `is_codesense_internal_event` predicate is
    // retired with the legacy watcher closure — in the new
    // directory-routing architecture, `register_directory_handlers`
    // registers a dedicated `build_codesense_internal_handler` for
    // `wd.codesense_internal_dir`, so events under that subtree are
    // dispatched (and emitted as `context-file-change`) by routing,
    // not by per-event classification. The three iter-3 predicate
    // tests are deleted with the predicate.

    #[test]
    fn test_watch_config_tolerates_legacy_viewer_root_and_install_dirs() {
        // SW-CP-18: `viewer_root` + `install_dirs` are retired from WatchConfig.
        // A legacy FE payload that still carries them must deserialize cleanly
        // (serde ignores unknown fields — no `deny_unknown_fields`).
        let json = r#"{"repo_path":"/r","context_repo_path":"/c","viewer_root":"_bmad-output","install_dirs":["/r/_bmad-output"]}"#;
        let config: WatchConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.repo_path, "/r");
        assert_eq!(config.context_repo_path, "/c");
    }

    // -----------------------------------------------------------------------
    // B-6 tests
    // -----------------------------------------------------------------------

    /// SW-XS-14: the per-repo staleness threshold is gone. A payload from an
    /// older frontend that still carries it deserializes, and the key is ignored.
    #[test]
    fn test_watch_config_ignores_the_retired_threshold_key() {
        // Spelled in two halves so the retirement guard's symbol scan stays exact.
        let key = concat!("staleness", "_threshold");
        let json = format!(r#"{{"repo_path":"/r","context_repo_path":"/c","{key}":12}}"#);
        let config: WatchConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config.repo_path, "/r");
    }

    /// SW-XS-12 (XS-18 leftover): `override_glob_prefix` is gone from the
    /// watch config, and a payload from an older frontend that still carries
    /// it (or its pre-SW-FX-56 name) deserializes with the key ignored.
    #[test]
    fn test_watch_config_ignores_the_retired_override_glob_prefix_key() {
        for key in [
            concat!("override", "_glob_prefix"),
            concat!("pipeline", "_root"),
        ] {
            let json = format!(r#"{{"repo_path":"/r","context_repo_path":"/c","{key}":"impl"}}"#);
            let config: WatchConfig = serde_json::from_str(&json).unwrap();
            assert_eq!(
                config,
                WatchConfig {
                    repo_path: "/r".into(),
                    context_repo_path: "/c".into(),
                },
                "{key}"
            );
        }
    }

    // SW-CP-2: `test_completed_at_changed_*` removed with the
    // `completed_at_changed` helper (the staleness-gate it served is retired;
    // the content watcher now enqueues a wave on every relevant edit).

    // -----------------------------------------------------------------------
    // SS-4a: install handler payload computation (R-5 conflict enrichment)
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // SS-FU-1: multi-repo watcher routing on shared dirs
    // -----------------------------------------------------------------------

    /// SS-FU-1 helper: minimal WatchConfig fixture for the bookkeeping
    /// tests. `context_repo_path` includes `_team/{team}/content` so
    /// `parse_context_repo_parts` extracts team correctly when the test
    /// exercises that path (cache-refresh-once test).
    fn ssfu1_config(repo_path: &str, context_repo_path: &str) -> WatchConfig {
        WatchConfig {
            repo_path: repo_path.into(),
            context_repo_path: context_repo_path.into(),
        }
    }

    /// AC-1 + AC-2: two configs subscribed at one shared dir fan out
    /// to BOTH per-subscriber callbacks on dispatch, each carrying its
    /// own `WatchConfig.repo_path`. Replaces the legacy
    /// last-writer-wins behavior of `path_to_repo: HashMap<_, WatchConfig>`.
    #[test]
    fn shared_dir_fanout_emits_per_subscriber_with_distinct_repo_paths() {
        let path_map: PathToRepo = Arc::new(Mutex::new(HashMap::new()));
        let shared_dir = PathBuf::from("/shared/context");
        let cfg_a = ssfu1_config("/repo/a", "/ctx/a/_team/default/content");
        let cfg_b = ssfu1_config("/repo/b", "/ctx/b/_team/default/content");
        append_subscriber_with_replace(&path_map, &shared_dir, &cfg_a).unwrap();
        append_subscriber_with_replace(&path_map, &shared_dir, &cfg_b).unwrap();

        let recorded: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let rec_clone = Arc::clone(&recorded);
        let handler = build_fanout_handler(
            Arc::clone(&path_map),
            shared_dir.clone(),
            move |_event, config| {
                rec_clone.lock().unwrap().push(config.repo_path.clone());
            },
        );

        let dispatcher = Arc::new(Dispatcher::new(50, 100, 60_000, 500));
        dispatcher.register_handler(&shared_dir, handler);
        dispatcher.dispatch(&shared_dir.join("file.md"));

        let recs = recorded.lock().unwrap();
        assert_eq!(
            recs.len(),
            2,
            "fan-out must invoke per_subscriber once per subscriber"
        );
        assert!(recs.contains(&"/repo/a".to_string()));
        assert!(recs.contains(&"/repo/b".to_string()));
    }

    // -----------------------------------------------------------------------
    // SW-FX-26: suppressed-write FE nudge — nearest watched-root resolution
    // -----------------------------------------------------------------------

    /// AC-1/AC-2: a file created (suppressed) under a watched content root
    /// resolves to that root's subscribers — incl. a NON-pipeline `.excalidraw`,
    /// the case the old `is_pipeline` early-return dropped entirely.
    #[test]
    fn subscribers_for_path_resolves_nearest_ancestor() {
        let path_map: PathToRepo = Arc::new(Mutex::new(HashMap::new()));
        let content_root = PathBuf::from("/ctx/default/_team/default/content");
        let cfg = ssfu1_config("/repo/a", "/ctx/a/_team/default/content");
        append_subscriber_with_replace(&path_map, &content_root, &cfg).unwrap();

        // A canvas created deep under the watched root (suppressed apply_canvas).
        let canvas = content_root.join("_bmad-output/fixes/foo.excalidraw");
        let subs = subscribers_for_path(&path_map, &canvas)
            .map(|(_, subs)| subs)
            .unwrap_or_default();

        assert_eq!(subs.len(), 1, "nested file resolves the root's subscribers");
        assert_eq!(subs[0].repo_path, "/repo/a");
    }

    /// Nearest-ancestor (deepest watched dir wins) — mirrors the dispatcher's
    /// `lookup_handler` so the nudge targets exactly the handler that would have
    /// fired for an unsuppressed write.
    #[test]
    fn subscribers_for_path_picks_deepest_watched_ancestor() {
        let path_map: PathToRepo = Arc::new(Mutex::new(HashMap::new()));
        let outer = PathBuf::from("/ctx/_team/default/content");
        let inner = outer.join("nested");
        append_subscriber_with_replace(&path_map, &outer, &ssfu1_config("/repo/outer", "/ctx/o"))
            .unwrap();
        append_subscriber_with_replace(&path_map, &inner, &ssfu1_config("/repo/inner", "/ctx/i"))
            .unwrap();

        let subs = subscribers_for_path(&path_map, &inner.join("foo.excalidraw"))
            .map(|(_, subs)| subs)
            .unwrap_or_default();

        assert_eq!(subs.len(), 1);
        assert_eq!(
            subs[0].repo_path, "/repo/inner",
            "the deeper watched dir wins (nearest ancestor)"
        );
    }

    /// A write under no watched dir is a silent no-op (no panic, no payloads).
    #[test]
    fn subscribers_for_path_empty_when_unwatched() {
        let path_map: PathToRepo = Arc::new(Mutex::new(HashMap::new()));
        append_subscriber_with_replace(
            &path_map,
            &PathBuf::from("/ctx/_team/default/content"),
            &ssfu1_config("/repo/a", "/ctx/a"),
        )
        .unwrap();

        let subs =
            subscribers_for_path(&path_map, &PathBuf::from("/somewhere/else/foo.excalidraw"))
                .map(|(_, subs)| subs)
                .unwrap_or_default();
        assert!(subs.is_empty());
    }

    /// AC-1/AC-3 (FE synthetic-key match): the nudge payload carries the
    /// subscriber's OWN `context_repo_path` verbatim — that's the string the FE
    /// `syntheticKeyForContextRepoPath` matches by exact equality (SW-FX-16), so
    /// the refresh key bumps. Byte-identical to the unsuppressed watcher emit
    /// because both build it from the same live `WatchConfig` via
    /// `base_event_payload` (no Rust-side re-derivation that could drift).
    #[test]
    fn suppressed_write_nudge_payload_carries_subscriber_context_repo_path() {
        let path_map: PathToRepo = Arc::new(Mutex::new(HashMap::new()));
        let content_root = PathBuf::from("/ctx/_team/default/content");
        let ctx = "/ctx/acme/_team/default/content";
        append_subscriber_with_replace(&path_map, &content_root, &ssfu1_config("/repo/a", ctx))
            .unwrap();

        let canvas = content_root.join("epic/foo.excalidraw");
        let subs = subscribers_for_path(&path_map, &canvas)
            .map(|(_, subs)| subs)
            .unwrap_or_default();
        assert_eq!(subs.len(), 1);

        // Exactly what `emit_context_change_for_suppressed_write` would emit.
        let payload = base_event_payload(&canvas, &subs[0]);
        assert_eq!(payload["context_repo_path"], ctx);
        assert_eq!(payload["repo_path"], "/repo/a");
        assert_eq!(payload["file_path"], canvas.to_string_lossy().as_ref());
    }

    /// AC-3: `remove_watch` for one config preserves siblings. Retract
    /// A from a shared dir whose Vec contains [A, B] leaves [B] —
    /// `emptied` list is empty so the caller does NOT unregister the
    /// dispatcher closure or `watcher.unwatch` (sibling B still
    /// subscribes).
    #[test]
    fn retract_subscriber_preserves_siblings_at_shared_dir() {
        let path_map: PathToRepo = Arc::new(Mutex::new(HashMap::new()));
        let shared_dir = PathBuf::from("/shared/context");
        let cfg_a = ssfu1_config("/repo/a", "/ctx/a/_team/default/content");
        let cfg_b = ssfu1_config("/repo/b", "/ctx/b/_team/default/content");
        append_subscriber_with_replace(&path_map, &shared_dir, &cfg_a).unwrap();
        append_subscriber_with_replace(&path_map, &shared_dir, &cfg_b).unwrap();

        let emptied = retract_subscriber_collect_emptied(
            &path_map,
            std::slice::from_ref(&shared_dir),
            cfg_a.subscriber_identity(),
        )
        .unwrap();
        assert!(
            emptied.is_empty(),
            "shared dir retains B; must not be in emptied list"
        );

        let snapshot = snapshot_subscribers(&path_map, &shared_dir);
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].repo_path, "/repo/b");
    }

    /// SW-WL-3 AC-4: the fast-path appends under the brief `path_to_repo`
    /// lock and NEVER acquires `watcher_lock`. Proven with the story's hostile
    /// fixture: hold `watcher_lock` for the entire call and assert the append
    /// still completes — a `start_watching` rebuild holding `watcher_lock`
    /// cannot block a fully-covered workspace open.
    #[test]
    fn fast_path_attach_appends_when_covered_without_watcher_lock() {
        let state = WatcherState::default();
        let dir_a = PathBuf::from("/covered/a");
        let dir_b = PathBuf::from("/covered/b");
        let prior = ssfu1_config("/repo/prior", "/ctx/prior/_team/default/content");
        append_subscriber_with_replace(&state.path_to_repo, &dir_a, &prior).unwrap();
        append_subscriber_with_replace(&state.path_to_repo, &dir_b, &prior).unwrap();

        // Hostile fixture: hold watcher_lock for the whole fast-path call.
        let _held = state.watcher.lock().unwrap();

        let opening = ssfu1_config("/repo/opening", "/ctx/opening/_team/default/content");
        let took_fast = try_fast_path_attach(
            &state.path_to_repo,
            &[dir_a.clone(), dir_b.clone()],
            &opening,
        )
        .unwrap();

        assert!(took_fast, "all candidates covered → fast path taken");
        assert_eq!(snapshot_subscribers(&state.path_to_repo, &dir_a).len(), 2);
        assert_eq!(snapshot_subscribers(&state.path_to_repo, &dir_b).len(), 2);
    }

    /// SW-WL-3 AC-4: a single net-new dir forces the slow path, and the
    /// fast-path makes NO partial mutation to the already-covered dirs.
    #[test]
    fn fast_path_attach_declines_and_is_inert_when_a_dir_is_net_new() {
        let state = WatcherState::default();
        let covered = PathBuf::from("/covered/a");
        let net_new = PathBuf::from("/netnew/b");
        let prior = ssfu1_config("/repo/prior", "/ctx/prior/_team/default/content");
        append_subscriber_with_replace(&state.path_to_repo, &covered, &prior).unwrap();

        let opening = ssfu1_config("/repo/opening", "/ctx/opening/_team/default/content");
        let took_fast = try_fast_path_attach(
            &state.path_to_repo,
            &[covered.clone(), net_new.clone()],
            &opening,
        )
        .unwrap();

        assert!(!took_fast, "net-new dir forces the slow path");
        // No partial append: the covered dir still holds only the prior sub.
        assert_eq!(snapshot_subscribers(&state.path_to_repo, &covered).len(), 1);
        assert!(snapshot_subscribers(&state.path_to_repo, &net_new).is_empty());
    }

    /// SW-WL-3 AC-4: an empty candidate set is NOT a fast-path — the caller
    /// takes the slow path (an honest no-op register), never a vacuous
    /// "covered" that appends nothing yet reports success.
    #[test]
    fn fast_path_attach_declines_on_empty_candidates() {
        let state = WatcherState::default();
        let opening = ssfu1_config("/repo/opening", "/ctx/opening/_team/default/content");
        assert!(!try_fast_path_attach(&state.path_to_repo, &[], &opening).unwrap());
    }

    /// AC-4: `remove_watch` for the last subscriber empties the dir
    /// and reports it in the `emptied` list so the caller can call
    /// `dispatcher.unregister_handler` + `watcher.unwatch`.
    #[test]
    fn last_subscriber_retract_empties_dir_and_signals_for_unregister() {
        let path_map: PathToRepo = Arc::new(Mutex::new(HashMap::new()));
        let shared_dir = PathBuf::from("/shared/context");
        let cfg_a = ssfu1_config("/repo/a", "/ctx/a/_team/default/content");
        let cfg_b = ssfu1_config("/repo/b", "/ctx/b/_team/default/content");
        append_subscriber_with_replace(&path_map, &shared_dir, &cfg_a).unwrap();
        append_subscriber_with_replace(&path_map, &shared_dir, &cfg_b).unwrap();

        let emptied1 = retract_subscriber_collect_emptied(
            &path_map,
            std::slice::from_ref(&shared_dir),
            cfg_a.subscriber_identity(),
        )
        .unwrap();
        assert!(emptied1.is_empty());

        let emptied2 = retract_subscriber_collect_emptied(
            &path_map,
            std::slice::from_ref(&shared_dir),
            cfg_b.subscriber_identity(),
        )
        .unwrap();
        assert_eq!(
            emptied2,
            vec![shared_dir.clone()],
            "last-subscriber retract must signal dir for unregister"
        );

        let snapshot = snapshot_subscribers(&path_map, &shared_dir);
        assert!(
            snapshot.is_empty(),
            "shared dir entry must be removed after last retract"
        );
    }

    /// SW-WL-6 (AC-3): making the detach commands `async` moved only the
    /// *thread* the teardown runs on, not the teardown itself. The immediate
    /// (no-wave) branch of [`gated_detach`] calls [`remove_watch_inner`] inline;
    /// exercise that pure body directly on a bare `WatcherState` and assert it
    /// still retracts the subscriber and removes the emptied dir key — the
    /// invariant M1 must not regress. (Watcher is `None` on a default state, so
    /// the `notify` `unwatch`/`stop()` spin is skipped; we assert the retract,
    /// not the kernel teardown.)
    #[test]
    fn wl6_immediate_detach_still_retracts_subscriber() {
        let state = WatcherState::default();
        let dir = PathBuf::from("/shared/context");
        let cfg = ssfu1_config("/repo/a", "/ctx/a/_team/default/content");
        append_subscriber_with_replace(&state.path_to_repo, &dir, &cfg).unwrap();
        assert_eq!(snapshot_subscribers(&state.path_to_repo, &dir).len(), 1);

        remove_watch_inner(&state, &cfg).unwrap();

        assert!(
            snapshot_subscribers(&state.path_to_repo, &dir).is_empty(),
            "immediate detach must retract the subscriber + remove the emptied dir key"
        );
    }

    /// SW-WL-6 (AC-9): the shared guard decision used by BOTH lifecycle impls
    /// (`start_watching_impl` + `stop_watching_inner`). Current seq → proceed;
    /// a higher seq recorded (a later start/stop requested) → bail. Testing the
    /// helper covers the start-side guard, which can't be exercised directly
    /// without a Tauri runtime.
    #[test]
    fn wl6_lifecycle_seq_is_current_tracks_the_latest_request() {
        let state = WatcherState::default();
        state.watcher_lifecycle_seq.fetch_max(5, Ordering::SeqCst);
        assert!(lifecycle_seq_is_current(&state, 5), "seq 5 is the latest");
        assert!(
            !lifecycle_seq_is_current(&state, 4),
            "an earlier seq is superseded"
        );

        // A later op arrives (higher seq) — now 5 is superseded too.
        state.watcher_lifecycle_seq.fetch_max(6, Ordering::SeqCst);
        assert!(!lifecycle_seq_is_current(&state, 5));
        assert!(lifecycle_seq_is_current(&state, 6));

        // `fetch_max` ignores a stale lower seq arriving out of poll order.
        state.watcher_lifecycle_seq.fetch_max(3, Ordering::SeqCst);
        assert!(lifecycle_seq_is_current(&state, 6), "max is monotonic");
    }

    /// SLW-FX-25 test helper: a fresh, dir-less `ContentWatcher` Arc for exercising
    /// the re-validate's Arc-identity comparison (no kernel watches, no filesystem).
    fn slwfx25_empty_watcher(state: &WatcherState) -> Arc<ContentWatcher> {
        Arc::new(
            ContentWatcher::new(WatchSetup { dirs: vec![] }, Arc::clone(&state.dispatcher))
                .expect("build empty ContentWatcher"),
        )
    }

    /// SLW-FX-25 (AC-1 / AC-2): the uncontended slow-path attach registers exactly
    /// ONCE against the live watcher, with `state.watcher` DROPPED across the
    /// register — the property that stops N concurrent worktree creates from
    /// serializing on the global mutex (the batch-create UI freeze). `try_lock()`
    /// inside the register closure witnesses the guard is genuinely not held.
    #[test]
    fn slwfx25_uncontended_attach_registers_once_with_guard_dropped() {
        use std::sync::atomic::{AtomicBool, AtomicUsize};
        let state = WatcherState::default();
        let w1 = slwfx25_empty_watcher(&state);
        *state.watcher.lock().unwrap() = Some(Arc::clone(&w1));

        let calls = AtomicUsize::new(0);
        let guard_dropped = AtomicBool::new(false);
        let on_live_watcher = AtomicBool::new(false);
        register_revalidating_watcher_lifecycle(&state, |watcher| {
            calls.fetch_add(1, Ordering::SeqCst);
            // The global watcher mutex must be free WHILE we register — otherwise a
            // concurrent create (or any watch-touching IPC) would block behind us.
            guard_dropped.store(state.watcher.try_lock().is_ok(), Ordering::SeqCst);
            on_live_watcher.store(
                watcher.map(|w| Arc::ptr_eq(w, &w1)).unwrap_or(false),
                Ordering::SeqCst,
            );
            Ok(())
        })
        .unwrap();

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "no swap in the gap → exactly one lock-free register, no fallback re-run"
        );
        assert!(
            guard_dropped.load(Ordering::SeqCst),
            "register must run with `state.watcher` DROPPED (no batch-create serialization)"
        );
        assert!(
            on_live_watcher.load(Ordering::SeqCst),
            "register must run against the live slot watcher"
        );
    }

    /// SLW-FX-25 (AC-6, the load-bearing safety test): if a concurrent
    /// `start_watching` swaps the watcher (a DIFFERENT `Arc`) DURING the lock-free
    /// register, the attach must NOT trust the stale result — it re-runs `register`
    /// against the NEW watcher under the HELD guard. Witnesses both halves: phase-B
    /// runs against the OLD watcher guard-dropped, phase-C against the NEW watcher
    /// guard-held. This is exactly the interleaving the naive clone-drop-register
    /// fix (or a `watcher_lifecycle_seq`-only re-validate) would get wrong — silent
    /// dispatch loss on the swapped-out watcher.
    #[test]
    fn slwfx25_watcher_swap_mid_attach_reregisters_under_held_guard() {
        use std::sync::atomic::{AtomicBool, AtomicUsize};
        let state = WatcherState::default();
        let w1 = slwfx25_empty_watcher(&state);
        let w2 = slwfx25_empty_watcher(&state);
        *state.watcher.lock().unwrap() = Some(Arc::clone(&w1));

        let calls = AtomicUsize::new(0);
        let phase_b_old_dropped = AtomicBool::new(false);
        let phase_c_new_held = AtomicBool::new(false);
        register_revalidating_watcher_lifecycle(&state, |watcher| {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            let guard_free = state.watcher.try_lock().is_ok();
            if n == 0 {
                let on_old = watcher.map(|w| Arc::ptr_eq(w, &w1)).unwrap_or(false);
                phase_b_old_dropped.store(on_old && guard_free, Ordering::SeqCst);
                // Simulate a concurrent `start_watching` swapping in a fresh watcher.
                *state.watcher.lock().unwrap() = Some(Arc::clone(&w2));
            } else {
                let on_new = watcher.map(|w| Arc::ptr_eq(w, &w2)).unwrap_or(false);
                phase_c_new_held.store(on_new && !guard_free, Ordering::SeqCst);
            }
            Ok(())
        })
        .unwrap();

        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "a mid-attach watcher swap must trigger exactly one locked re-register"
        );
        assert!(
            phase_b_old_dropped.load(Ordering::SeqCst),
            "phase-B (optimistic) register runs against the OLD watcher with the guard DROPPED"
        );
        assert!(
            phase_c_new_held.load(Ordering::SeqCst),
            "phase-C (fallback) re-register runs against the NEW watcher with the guard HELD"
        );
    }

    /// SLW-FX-25 (Edge Case, review r1): if a concurrent `stop_watching` empties the
    /// slot DURING the lock-free register, the re-validate must NOT re-register —
    /// there is no live watcher to attach to, and re-running would re-stamp orphan
    /// `path_to_repo` bookkeeping the stop just cleared.
    #[test]
    fn slwfx25_stop_clears_slot_mid_attach_skips_fallback_register() {
        use std::sync::atomic::AtomicUsize;
        let state = WatcherState::default();
        let w1 = slwfx25_empty_watcher(&state);
        *state.watcher.lock().unwrap() = Some(Arc::clone(&w1));

        let calls = AtomicUsize::new(0);
        register_revalidating_watcher_lifecycle(&state, |_watcher| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                // Simulate a concurrent `stop_watching` tearing the watcher down.
                *state.watcher.lock().unwrap() = None;
            }
            Ok(())
        })
        .unwrap();

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a torn-down (None) slot at re-validate must NOT re-register orphan bookkeeping"
        );
    }

    /// SLW-FX-25 (cold slot, review r2): when no watcher is published yet (cold
    /// start / post-stop), `register` runs bookkeeping-only (Phase 2b — the kernel
    /// watches — is skipped with no watcher), so there is nothing to keep off the
    /// lock and no swap to race. The helper registers ONCE, against `None`, UNDER
    /// the held guard — byte-identical to the pre-SLW-FX-25 path — rather than
    /// dropping the guard for a lock-free window that would buy nothing.
    #[test]
    fn slwfx25_cold_slot_registers_once_under_held_guard() {
        use std::sync::atomic::{AtomicBool, AtomicUsize};
        let state = WatcherState::default();
        // Slot stays None — no watcher ever published.

        let calls = AtomicUsize::new(0);
        let guard_held = AtomicBool::new(false);
        let watcher_was_none = AtomicBool::new(false);
        register_revalidating_watcher_lifecycle(&state, |watcher| {
            calls.fetch_add(1, Ordering::SeqCst);
            // Non-reentrant std mutex: a `try_lock` Err here means THIS thread holds
            // the guard → the cold register runs under the held guard (not dropped).
            guard_held.store(state.watcher.try_lock().is_err(), Ordering::SeqCst);
            watcher_was_none.store(watcher.is_none(), Ordering::SeqCst);
            Ok(())
        })
        .unwrap();

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "cold slot → exactly one register, no lock-free window, no re-validate"
        );
        assert!(
            watcher_was_none.load(Ordering::SeqCst),
            "cold register runs with no watcher (bookkeeping-only, Phase 2b skipped)"
        );
        assert!(
            guard_held.load(Ordering::SeqCst),
            "cold-slot register runs UNDER the held guard — no lock-free path when there's \
             nothing to parallelize"
        );
    }

    /// SLW-FX-25 (robustness): a failure in the lock-free register propagates
    /// immediately and does NOT reach the re-validate/fallback — a failed attach
    /// must surface its error, not silently retry under lock.
    #[test]
    fn slwfx25_register_error_propagates_and_skips_fallback() {
        use std::sync::atomic::AtomicUsize;
        let state = WatcherState::default();
        let w1 = slwfx25_empty_watcher(&state);
        *state.watcher.lock().unwrap() = Some(Arc::clone(&w1));

        let calls = AtomicUsize::new(0);
        let result = register_revalidating_watcher_lifecycle(&state, |_watcher| {
            calls.fetch_add(1, Ordering::SeqCst);
            Err("attach exploded".to_string())
        });

        assert!(result.is_err(), "a phase-B register error must propagate");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "an error in the lock-free register must NOT trigger the fallback re-register"
        );
    }

    /// SLW-FX-25 (robustness, review r1): an error in the phase-C fallback register
    /// (after a swap) also propagates — the swap-then-fail interleave must not be
    /// silently swallowed.
    #[test]
    fn slwfx25_fallback_register_error_propagates() {
        use std::sync::atomic::AtomicUsize;
        let state = WatcherState::default();
        let w1 = slwfx25_empty_watcher(&state);
        let w2 = slwfx25_empty_watcher(&state);
        *state.watcher.lock().unwrap() = Some(Arc::clone(&w1));

        let calls = AtomicUsize::new(0);
        let result = register_revalidating_watcher_lifecycle(&state, |_watcher| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                *state.watcher.lock().unwrap() = Some(Arc::clone(&w2));
                Ok(())
            } else {
                Err("fallback attach exploded".to_string())
            }
        });

        assert!(
            result.is_err(),
            "a phase-C fallback register error must propagate"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "fallback ran once (against the new watcher) then errored"
        );
    }

    /// SW-WL-6 (AC-8): a `stop_watching` whose seq is still current tears down
    /// the watch bookkeeping (clears `path_to_repo`, nulls the watcher).
    #[test]
    fn wl6_stop_watching_inner_tears_down_when_seq_current() {
        let state = WatcherState::default();
        let dir = PathBuf::from("/shared/context");
        let cfg = ssfu1_config("/repo/a", "/ctx/a/_team/default/content");
        append_subscriber_with_replace(&state.path_to_repo, &dir, &cfg).unwrap();

        // Mirror the command: record the frontend seq, then run with it.
        let seq = 1;
        state.watcher_lifecycle_seq.fetch_max(seq, Ordering::SeqCst);
        stop_watching_inner(&state, seq).unwrap();

        assert!(
            snapshot_subscribers(&state.path_to_repo, &dir).is_empty(),
            "current-seq stop clears the watch bookkeeping"
        );
        assert!(state.watcher.lock().unwrap().is_none());
    }

    /// SW-WL-6 (AC-9): the stop-vs-restart guard. A `stop_watching` that was
    /// superseded by a later start/stop (higher seq) must be a NO-OP — it must
    /// not tear down the watcher the newer op now owns. This is the race that
    /// opened once the detach/stop moved off the (formerly serializing) main
    /// thread; the frontend-minted seq resolves it to last-intent-wins.
    #[test]
    fn wl6_stop_watching_inner_noops_when_superseded() {
        let state = WatcherState::default();
        let dir = PathBuf::from("/shared/context");
        let cfg = ssfu1_config("/repo/a", "/ctx/a/_team/default/content");
        append_subscriber_with_replace(&state.path_to_repo, &dir, &cfg).unwrap();

        // This stop is requested (seq N)...
        let seq = 1;
        state.watcher_lifecycle_seq.fetch_max(seq, Ordering::SeqCst);
        // ...but a later start/stop is requested afterwards (higher seq).
        state.watcher_lifecycle_seq.fetch_max(2, Ordering::SeqCst);

        stop_watching_inner(&state, seq).unwrap();

        assert_eq!(
            snapshot_subscribers(&state.path_to_repo, &dir).len(),
            1,
            "a superseded stop must NOT tear down the watch the later op owns"
        );
    }

    #[test]
    fn cp9_all_reverse_dests_retract_on_close() {
        // SW-CP-9 AC-7: a workspace watching multiple projection dest dirs
        // (story + docs + runbooks) retracts EVERY reverse dest on close via
        // the identity-scan teardown — manifest-independent, no leaked watch
        // even if the manifest changed since attach.
        let path_map: PathToRepo = Arc::new(Mutex::new(HashMap::new()));
        let cfg = ssfu1_config("/repo", "/ctx/_team/default/content");
        let dests = [
            PathBuf::from("/repo/_bmad-output"),
            PathBuf::from("/repo/docs"),
            PathBuf::from("/repo/runbooks"),
        ];
        for d in &dests {
            append_subscriber_with_replace(&path_map, d, &cfg).unwrap();
        }

        let emptied =
            retract_subscriber_all_keys_collect_emptied(&path_map, cfg.subscriber_identity())
                .unwrap();

        assert_eq!(emptied.len(), 3, "every reverse dest retracts on close");
        for d in &dests {
            assert!(emptied.contains(d), "{} retracted", d.display());
            assert!(
                snapshot_subscribers(&path_map, d).is_empty(),
                "{} key removed",
                d.display()
            );
        }
    }

    /// AC-6: rollback isolation. Simulates B's Phase 2b attach failure
    /// after A is fully registered and B has appended its subscribers
    /// at the shared dir + at B's unique dir. The rollback retracts B
    /// from all of B's reg dirs; A's subscription at shared dir
    /// survives. B's unique dir surfaces in the `emptied` list so the
    /// caller can unregister its dispatcher closure; the shared dir
    /// does NOT (A still subscribes).
    #[test]
    fn rollback_retract_preserves_sibling_after_failing_config() {
        let path_map: PathToRepo = Arc::new(Mutex::new(HashMap::new()));
        let shared_dir = PathBuf::from("/shared/context");
        let b_unique_dir = PathBuf::from("/repo/b/install");
        let cfg_a = ssfu1_config("/repo/a", "/ctx/a/_team/default/content");
        let cfg_b = ssfu1_config("/repo/b", "/ctx/b/_team/default/content");

        // A registered first.
        append_subscriber_with_replace(&path_map, &shared_dir, &cfg_a).unwrap();
        // B registers at both dirs (Phase 2a appends complete).
        append_subscriber_with_replace(&path_map, &shared_dir, &cfg_b).unwrap();
        append_subscriber_with_replace(&path_map, &b_unique_dir, &cfg_b).unwrap();

        // Phase 2b fails on B → rollback retracts B from all of B's
        // reg dirs.
        let emptied = retract_subscriber_collect_emptied(
            &path_map,
            &[shared_dir.clone(), b_unique_dir.clone()],
            cfg_b.subscriber_identity(),
        )
        .unwrap();

        let shared_subs = snapshot_subscribers(&path_map, &shared_dir);
        assert_eq!(
            shared_subs.len(),
            1,
            "A's subscription must survive B's rollback at shared dir"
        );
        assert_eq!(shared_subs[0].repo_path, "/repo/a");

        assert!(
            emptied.contains(&b_unique_dir),
            "B's unique dir must be in the emptied list"
        );
        assert!(
            !emptied.contains(&shared_dir),
            "shared dir must NOT be in emptied list — A still subscribed"
        );
    }

    /// AC-7: idempotent re-registration. Two appends with the same
    /// identity produce ONE entry (replace-in-place), and the second append
    /// hands back the first as the displaced subscriber. (SW-XS-12: a config
    /// is now its identity alone, so "the latest wins" has nothing more to
    /// observe.)
    #[test]
    fn append_subscriber_with_replace_is_idempotent_and_replaces_in_place() {
        let path_map: PathToRepo = Arc::new(Mutex::new(HashMap::new()));
        let dir = PathBuf::from("/some/dir");
        let config = ssfu1_config("/repo/a", "/ctx/a/_team/default/content");
        let first = append_subscriber_with_replace(&path_map, &dir, &config).unwrap();
        assert!(first.is_none());
        let displaced = append_subscriber_with_replace(&path_map, &dir, &config).unwrap();
        assert_eq!(displaced.as_ref(), Some(&config));

        let snapshot = snapshot_subscribers(&path_map, &dir);
        assert_eq!(
            snapshot,
            vec![config],
            "double registration of same repo_path must produce ONE entry"
        );
    }

    /// SW-WL-1 AC-3 (load-bearing): two configs with the SAME
    /// `repo_path` but DISTINCT `context_repo_path` are distinct logical
    /// subscribers and BOTH survive at a shared dir. Under the prior
    /// `repo_path`-only dedup key the second append silently evicted the
    /// first (snapshot len==1) — the synthetic-slug-planning-workspace-
    /// coincides-with-source-repo hazard. With composite identity
    /// neither displaces the other (len==2) and a dispatched event fans
    /// out to both. Symmetric across registration order; the
    /// discriminator is `context_repo_path` because `repo_path` is
    /// identical.
    #[test]
    fn colliding_repo_path_distinct_context_preserves_both_subscribers() {
        for (first, second) in [("/ctx/a", "/ctx/b"), ("/ctx/b", "/ctx/a")] {
            let path_map: PathToRepo = Arc::new(Mutex::new(HashMap::new()));
            let shared_dir = PathBuf::from("/repo/shared/context");
            let cfg_first = ssfu1_config("/repo/shared", &format!("{first}/_team/default/content"));
            let cfg_second =
                ssfu1_config("/repo/shared", &format!("{second}/_team/default/content"));

            append_subscriber_with_replace(&path_map, &shared_dir, &cfg_first).unwrap();
            let displaced =
                append_subscriber_with_replace(&path_map, &shared_dir, &cfg_second).unwrap();
            assert!(
                displaced.is_none(),
                "distinct context_repo_path must NOT displace a colliding repo_path (order {first} then {second})"
            );

            let snapshot = snapshot_subscribers(&path_map, &shared_dir);
            assert_eq!(
                snapshot.len(),
                2,
                "both subscribers must survive at the shared dir (order {first} then {second})"
            );

            // A dispatched event fans out to BOTH; discriminate on
            // context_repo_path since repo_path is identical.
            let recorded: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
            let rec_clone = Arc::clone(&recorded);
            let handler = build_fanout_handler(
                Arc::clone(&path_map),
                shared_dir.clone(),
                move |_event, config| {
                    rec_clone
                        .lock()
                        .unwrap()
                        .push(config.context_repo_path.clone());
                },
            );
            let dispatcher = Arc::new(Dispatcher::new(50, 100, 60_000, 500));
            dispatcher.register_handler(&shared_dir, handler);
            dispatcher.dispatch(&shared_dir.join("file.md"));

            let recs = recorded.lock().unwrap();
            assert_eq!(
                recs.len(),
                2,
                "event must fan out to both subscribers (order {first} then {second})"
            );
            assert!(recs.contains(&format!("{first}/_team/default/content")));
            assert!(recs.contains(&format!("{second}/_team/default/content")));
        }
    }

    /// SW-WL-1 AC-3b (baseline guard): two distinct-`repo_path`
    /// subscribers sharing one watched dir still BOTH receive events
    /// under the composite key. Passes today (distinct repo_path → never
    /// displaced); pins that the identity change doesn't regress the
    /// ordinary multi-repo fan-out the SS-FU-1 model relies on.
    #[test]
    fn shared_repo_root_fanout_two_repo_paths() {
        let path_map: PathToRepo = Arc::new(Mutex::new(HashMap::new()));
        let shared_root = PathBuf::from("/shared/repo/root");
        let cfg_a = ssfu1_config("/repo/a", "/ctx/a/_team/default/content");
        let cfg_b = ssfu1_config("/repo/b", "/ctx/b/_team/default/content");
        append_subscriber_with_replace(&path_map, &shared_root, &cfg_a).unwrap();
        append_subscriber_with_replace(&path_map, &shared_root, &cfg_b).unwrap();

        assert_eq!(snapshot_subscribers(&path_map, &shared_root).len(), 2);

        let recorded: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let rec_clone = Arc::clone(&recorded);
        let handler = build_fanout_handler(
            Arc::clone(&path_map),
            shared_root.clone(),
            move |_event, config| {
                rec_clone.lock().unwrap().push(config.repo_path.clone());
            },
        );
        let dispatcher = Arc::new(Dispatcher::new(50, 100, 60_000, 500));
        dispatcher.register_handler(&shared_root, handler);
        dispatcher.dispatch(&shared_root.join("file.md"));

        let recs = recorded.lock().unwrap();
        assert_eq!(
            recs.len(),
            2,
            "both distinct-repo_path subscribers must receive events"
        );
        assert!(recs.contains(&"/repo/a".to_string()));
        assert!(recs.contains(&"/repo/b".to_string()));
    }

    /// SW-WL-1 AC-3c (completeness — the rollback path): a Phase-2b
    /// failure for one subscriber must NOT evict a colliding-`repo_path`
    /// sibling that differs in `context_repo_path`. Same hazard as AC-3
    /// but on `rollback_phase_2a`'s retract (D2's fourth/fifth identity
    /// site). RED under a `repo_path`-only rollback key — the retract
    /// matches BOTH A and B, evicting A and unregistering the shared
    /// closure (snapshot len==0, no post-rollback dispatch). GREEN with
    /// composite identity: only B is retracted; A survives and keeps
    /// receiving events.
    #[test]
    fn colliding_repo_path_rollback_preserves_sibling() {
        let dispatcher = Arc::new(Dispatcher::new(50, 100, 60_000, 500));
        let path_map: PathToRepo = Arc::new(Mutex::new(HashMap::new()));
        let shared_dir = PathBuf::from("/repo/shared/context");

        // A: attached source repo. B: synthetic-slug planning workspace
        // sharing A's repo_path but with a distinct context_repo_path.
        let cfg_a = ssfu1_config("/repo/shared", "/ctx/source/_team/default/content");
        let cfg_b = ssfu1_config("/repo/shared", "/ctx/pw/_team/default/content");

        // A fully registered: Phase 2a append + dispatcher closure.
        append_subscriber_with_replace(&path_map, &shared_dir, &cfg_a).unwrap();
        let recorded: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let rec_clone = Arc::clone(&recorded);
        let handler = build_fanout_handler(
            Arc::clone(&path_map),
            shared_dir.clone(),
            move |_event, config| {
                rec_clone
                    .lock()
                    .unwrap()
                    .push(config.context_repo_path.clone());
            },
        );
        dispatcher.register_handler(&shared_dir, handler);

        // B appends (Phase 2a). Distinct identity → A is not displaced.
        let displaced = append_subscriber_with_replace(&path_map, &shared_dir, &cfg_b).unwrap();
        assert!(
            displaced.is_none(),
            "B must not displace A (distinct identity)"
        );
        assert_eq!(snapshot_subscribers(&path_map, &shared_dir).len(), 2);

        // B's Phase 2b fails → roll back B only.
        let attempted = vec![(shared_dir.clone(), displaced)];
        rollback_phase_2a(
            &path_map,
            &dispatcher,
            &attempted,
            cfg_b.subscriber_identity(),
        );

        // A survives at the shared dir; B is gone.
        let snapshot = snapshot_subscribers(&path_map, &shared_dir);
        assert_eq!(
            snapshot.len(),
            1,
            "rollback of B must leave A (colliding repo_path, distinct context) intact"
        );
        assert_eq!(
            snapshot[0].context_repo_path,
            "/ctx/source/_team/default/content"
        );

        // Dispatcher closure preserved (shared dir never emptied) → A
        // still receives events.
        dispatcher.dispatch(&shared_dir.join("after_rollback.md"));
        let recs = recorded.lock().unwrap();
        assert_eq!(
            *recs,
            vec!["/ctx/source/_team/default/content".to_string()],
            "A must still receive events after B's rollback"
        );
    }

    /// SS-FU-1 cache-refresh-once correctness: an event at a shared
    /// `context_output` dir refreshes the story index ONCE (before the
    /// per-subscriber loop) and emits `context-file-change` to every
    /// subscriber.
    #[test]
    fn dispatch_context_event_refreshes_cache_once_and_fires_staleness_for_all_subscribers() {
        use crate::context_repo::TEAM_CONTENT_DIR;
        use std::fs;

        let dir = std::env::temp_dir()
            .join("codesense_test")
            .join(format!("ss_fu_1_refresh_once_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let team = "default";
        let content_dir = dir.join(team).join(TEAM_CONTENT_DIR);
        fs::create_dir_all(&content_dir).unwrap();
        let story_path = content_dir.join("story.md");

        // Pre: in-progress, no completed_at → cache entry has completed_at=None.
        fs::write(
            &story_path,
            "---\nstatus: in-progress\nstory_id: S-1\nsource_repo: org/repo-a\n---\n# S",
        )
        .unwrap();
        let cache = FrontmatterCache::shared(FrontmatterCache::build_from_scan(&dir));

        // Modify: status=done, completed_at set. The refresh observes
        // (old=None_completed_at, new=Some_completed_at) →
        // completed_at_changed = true.
        fs::write(
            &story_path,
            "---\nstatus: done\nstory_id: S-1\nsource_repo: org/repo-a\ndone:\n  completed_at: 2025-06-01T00:00:00Z\n---\n# S",
        )
        .unwrap();

        // context_repo_path is the per-team content dir (PTS-2 flat).
        // parse_context_repo_parts extracts team="default" from it.
        let ctx_path = content_dir.to_string_lossy().to_string();
        // SW-CP-18: the event_path is under content_dir (the watched dir), so
        // `is_relevant_event`'s `starts_with(watched_dir)` arm makes it relevant
        // with no `install_dirs` knob needed.

        let cfg_a = WatchConfig {
            repo_path: "/repo/a".into(),
            context_repo_path: ctx_path.clone(),
        };
        let cfg_b = WatchConfig {
            repo_path: "/repo/b".into(),
            context_repo_path: ctx_path.clone(),
        };

        let emit_log: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let emit_rec = Arc::clone(&emit_log);

        dispatch_context_event(
            &story_path,
            &content_dir,
            &[cfg_a, cfg_b],
            Some(&cache),
            |config, _payload| {
                emit_rec.lock().unwrap().push(config.repo_path.clone());
            },
        );

        let emits = emit_log.lock().unwrap();
        assert_eq!(emits.len(), 2, "emit must fire once per subscriber");
        assert!(emits.contains(&"/repo/a".to_string()));
        assert!(emits.contains(&"/repo/b".to_string()));

        assert_eq!(
            cached_story_status(&cache.read().unwrap()).as_deref(),
            Some("done"),
            "the single pre-fan-out refresh must land the edit in the story index"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// SW-SC-11 AC-2/AC-4: a transient atomic-temp sibling
    /// (`*.excalidraw.tmp.<pid>.<hash>`) must produce NO context emit. Before the
    /// fix, `dispatch_context_event` treated the temp as relevant (it's under the
    /// watched content dir) and emitted its path into `changedPaths` — a path the
    /// open viewer's per-file gate can never match.
    #[test]
    fn dispatch_context_event_skips_transient_temp_sibling() {
        use crate::context_repo::TEAM_CONTENT_DIR;
        use std::fs;

        let dir = std::env::temp_dir()
            .join("codesense_test")
            .join(format!("sc11_temp_sibling_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let content_dir = dir.join("default").join(TEAM_CONTENT_DIR);
        fs::create_dir_all(&content_dir).unwrap();

        // The agent's atomic write: a temp sibling of an open canvas.
        let temp_path = content_dir.join("canvas-sample.excalidraw.tmp.99999.deadbeef");
        let cfg = WatchConfig {
            repo_path: "/repo/a".into(),
            context_repo_path: content_dir.to_string_lossy().to_string(),
        };

        let emit_log: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let emit_rec = Arc::clone(&emit_log);

        dispatch_context_event(
            &temp_path,
            &content_dir,
            std::slice::from_ref(&cfg),
            None,
            |config, _payload| emit_rec.lock().unwrap().push(config.repo_path.clone()),
        );

        assert!(
            emit_log.lock().unwrap().is_empty(),
            "a transient atomic-temp sibling must NOT emit context-file-change"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// SW-SC-11 AC-2: the `<base>.tmp.<pipeline-ext>` shape (e.g. `story.tmp.md`)
    /// matches `is_transient_editor_file` yet has a real pipeline extension, so
    /// without an entry guard `refresh_under_lock` would insert it into the
    /// frontmatter cache under the temp key. Assert the transient never mutates
    /// the cache.
    #[test]
    fn dispatch_context_event_skips_transient_temp_with_pipeline_ext_no_cache_mutation() {
        use crate::context_repo::TEAM_CONTENT_DIR;
        use std::fs;

        let dir = std::env::temp_dir()
            .join("codesense_test")
            .join(format!("sc11_temp_pipeline_ext_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let content_dir = dir.join("default").join(TEAM_CONTENT_DIR);
        fs::create_dir_all(&content_dir).unwrap();

        // Build the cache from the empty content dir FIRST (isolating the
        // dispatch refresh path from the separate build_from_scan walk), then
        // drop a transient with a REAL pipeline extension + valid frontmatter.
        let cache = FrontmatterCache::shared(FrontmatterCache::build_from_scan(&dir));
        let temp_path = content_dir.join("story.tmp.md");
        fs::write(
            &temp_path,
            "---\nstatus: in-progress\nstory_id: S-1\nsource_repo: org/repo-a\n---\n# S",
        )
        .unwrap();
        assert!(
            !cache.read().unwrap().entries.contains_key(&temp_path),
            "precondition: temp not yet cached"
        );

        let cfg = ssfu1_config("/repo/a", &content_dir.to_string_lossy());
        dispatch_context_event(
            &temp_path,
            &content_dir,
            std::slice::from_ref(&cfg),
            Some(&cache),
            |_c, _p| {},
        );

        assert!(
            !cache.read().unwrap().entries.contains_key(&temp_path),
            "a transient temp must NOT be inserted into the frontmatter cache"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // SW-WL-4: team-level always-on context watcher separation
    // -----------------------------------------------------------------------

    /// Helper: read the cached `status` of the entry whose file name is
    /// `story.md`, independent of the exact key build_from_scan used.
    #[cfg(test)]
    fn cached_story_status(cache: &FrontmatterCache) -> Option<String> {
        cache
            .entries
            .iter()
            .find(|(p, _)| p.file_name().map(|n| n == "story.md").unwrap_or(false))
            .and_then(|(_, e)| e.status().map(|s| s.to_string()))
    }

    /// G-8: the team content watch firing with ZERO workspace subscribers (only the
    /// team's pipeline/board are open) still owes those surfaces a refresh — an
    /// externally created story must reach the hidden-stories badge without a team
    /// re-enter. The team-level signal names no repo (the FE routes it by the
    /// content path) and skips framework install roots, whose bursts no team
    /// surface reads.
    #[test]
    fn a_team_level_change_is_signalled_for_team_content_but_not_framework_roots() {
        let content = Path::new("/u/.codesense/orgs/o/default/content");
        let story = content.join("_bmad-output/orphan-epic/ORPH-1-x.md");
        let payload = team_level_change_payload(&story, content).expect("a story change");
        assert_eq!(payload["repo_path"], "");
        assert_eq!(payload["context_repo_path"], "");
        assert_eq!(payload["file_path"], story.to_string_lossy().as_ref());

        for framework in [crate::context_repo::CLAUDE_CONFIG_DIR, "_bmad"] {
            let path = content.join(framework).join("skills/x/SKILL.md");
            assert!(
                team_level_change_payload(&path, content).is_none(),
                "{framework} bursts are not team-surface changes"
            );
        }
        assert!(
            team_level_change_payload(Path::new("/elsewhere/x.md"), content).is_none(),
            "a path outside the watched content dir"
        );
        assert!(team_level_change_payload(content, content).is_some());

        // Wired through `team_level_emit`: a subscriber's own emit covers it, so
        // the team-level emit fires only with zero subscribers.
        assert!(team_level_emit(&[], &story, content).is_some());
        let subscriber = ssfu1_config("/r", "/u/.codesense/orgs/o/default/repos/a/b");
        assert!(
            team_level_emit(std::slice::from_ref(&subscriber), &story, content).is_none(),
            "never double-bumps a subscribed change"
        );
    }

    #[test]
    fn suppressed_write_refreshes_private_content_without_team_subscribers() {
        let path_map: PathToRepo = Arc::new(Mutex::new(HashMap::new()));
        let content = PathBuf::from("/home/users/42/scratch/orgs/acme/plans/content");
        let file = content.join("2026/plan.md");
        let payloads = suppressed_write_payloads(&path_map, &file);
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0]["repo_path"], content.to_string_lossy().as_ref());
        assert_eq!(payloads[0]["file_path"], file.to_string_lossy().as_ref());
        assert!(suppressed_write_payloads(&path_map, &content.join("2026/4913")).is_empty());
    }

    #[test]
    fn suppressed_write_refreshes_source_free_team_without_workspace_subscribers() {
        let path_map: PathToRepo = Arc::new(Mutex::new(HashMap::new()));
        let content = PathBuf::from("/ctx/empty/content");
        path_map.lock().unwrap().insert(content.clone(), Vec::new());
        let file = content.join("docs/native-plan.md");
        let payloads = suppressed_write_payloads(&path_map, &file);
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0]["repo_path"], "");
        assert_eq!(payloads[0]["context_repo_path"], "");
        assert_eq!(payloads[0]["file_path"], file.to_string_lossy().as_ref());
        assert!(suppressed_write_payloads(&path_map, &content.join("_bmad/x.md")).is_empty());
        assert!(suppressed_write_payloads(&path_map, &content.join(".claude/x.md")).is_empty());
        assert!(suppressed_write_payloads(&path_map, &PathBuf::from("/unwatched/x.md")).is_empty());
    }

    #[test]
    fn suppressed_write_keeps_subscriber_fanout_without_extra_team_emit() {
        let path_map: PathToRepo = Arc::new(Mutex::new(HashMap::new()));
        let content = PathBuf::from("/ctx/empty/content");
        let a = ssfu1_config("/repo/a", "/ctx/a");
        let b = ssfu1_config("/repo/b", "/ctx/b");
        append_subscriber_with_replace(&path_map, &content, &a).unwrap();
        append_subscriber_with_replace(&path_map, &content, &b).unwrap();
        let file = content.join("docs/native-plan.md");
        assert_eq!(
            suppressed_write_payloads(&path_map, &file),
            vec![base_event_payload(&file, &a), base_event_payload(&file, &b)]
        );
        assert!(
            suppressed_write_payloads(&path_map, &content.join("native-plan.tmp.md")).is_empty()
        );
    }

    #[test]
    fn canonical_notify_event_reaches_registered_team_root_even_after_delete() {
        let registered = Path::new("/var/fixture/empty/content");
        let canonical = Path::new("/private/var/fixture/empty/content");
        let event = canonical.join("docs/deleted.md");
        let routed = registered_context_event_path(&event, registered, canonical);
        assert_eq!(routed, registered.join("docs/deleted.md"));
        let payload = team_level_emit(&[], &routed, registered).expect("team refresh");
        assert_eq!(payload["file_path"], routed.to_string_lossy().as_ref());
        assert_eq!(
            registered_context_event_path(&routed, registered, canonical),
            routed
        );
        let outside = Path::new("/private/var/fixture/empty/content-other/x.md");
        assert_eq!(
            registered_context_event_path(outside, registered, canonical),
            outside
        );
        assert!(team_level_emit(&[], outside, registered).is_none());
    }

    /// SW-XS-20: a change in a parent team's content is routed to the active
    /// child team's nav (`inherited_by`), which shows the parent's files; a
    /// change anywhere else gets no such emit.
    #[test]
    fn a_parent_team_change_is_signalled_to_the_inheriting_active_team() {
        let tmp = tempfile::tempdir().unwrap();
        let org_root = tmp.path().join("acme/ctx");
        let write = |rel: &str, body: &str| {
            let p = org_root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        write(
            "org.yml",
            "slug: acme/ctx\ndisplay_name: Acme\ncreated_at: \"2026-01-01T00:00:00Z\"\n",
        );
        write(
            "people.yml",
            "people:\n  - github_user_id: 1\n    github_username: alice\n",
        );
        write(
            "teams.yml",
            "teams:\n  - name: default\n    parent_team: parent\n  - name: parent\n  - name: other\n",
        );
        write("parent/config/sharing.yml", "share: ['content/**']\n");
        write("parent/content/epic/rule.md", "# rule\n");
        write("other/content/epic/x.md", "# x\n");
        write("default/content/epic/own.md", "# own\n");
        let active = Some(("acme/ctx", "default"));

        let changed = org_root.join("parent/content/epic/rule.md");
        let payload = inherited_change_payload(&changed, active).expect("a parent change");
        assert_eq!(payload["file_path"], changed.to_string_lossy().as_ref());
        assert_eq!(
            payload["inherited_by"],
            org_root.join("default/content").to_string_lossy().as_ref()
        );

        let sibling = org_root.join("other/content/epic/x.md");
        assert!(inherited_change_payload(&sibling, active).is_none());
        let own = org_root.join("default/content/epic/own.md");
        assert!(inherited_change_payload(&own, active).is_none());
        assert!(inherited_change_payload(&changed, None).is_none());
        let framework = org_root.join("parent/content/_bmad/x.md");
        assert!(inherited_change_payload(&framework, active).is_none());
    }

    /// SW-XS-20 (r2): a parent team whose content dir has its own handler (a
    /// workspace of the parent is open) still refreshes the inheriting active
    /// team's nav — `build_context_handler` emits `inherited_by` too.
    #[test]
    fn a_parent_content_handler_signals_the_inheriting_active_team() {
        let tmp = tempfile::tempdir().unwrap();
        let org_root = tmp.path().join("acme/ctx");
        let write = |rel: &str, body: &str| {
            let p = org_root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        write(
            "org.yml",
            "slug: acme/ctx\ndisplay_name: Acme\ncreated_at: \"2026-01-01T00:00:00Z\"\n",
        );
        write(
            "people.yml",
            "people:\n  - github_user_id: 1\n    github_username: alice\n",
        );
        write(
            "teams.yml",
            "teams:\n  - name: default\n    parent_team: parent\n  - name: parent\n",
        );
        write("parent/config/sharing.yml", "share: ['content/**']\n");
        write("parent/content/epic/rule.md", "# rule\n");
        std::fs::create_dir_all(org_root.join("default/content")).unwrap();

        let active = crate::mcp::state::ActiveTeamState::default();
        active.update(crate::mcp::state::ActiveTeam {
            org_slug: "acme/ctx".into(),
            team_name: "default".into(),
        });
        let app = tauri::test::mock_builder()
            .manage(active)
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let seen: Arc<StdMutex<Vec<serde_json::Value>>> = Arc::new(StdMutex::new(Vec::new()));
        {
            use tauri::Listener;
            let seen = Arc::clone(&seen);
            app.listen("context-file-change", move |event| {
                seen.lock()
                    .unwrap()
                    .push(serde_json::from_str(event.payload()).unwrap_or_default());
            });
        }
        let parent_content = org_root.join("parent/content");
        let handler = build_context_handler(
            app.handle().clone(),
            Arc::new(Mutex::new(HashMap::new())),
            parent_content.clone(),
        );
        handler(&parent_content.join("epic/rule.md"));

        let child = org_root
            .join("default/content")
            .to_string_lossy()
            .into_owned();
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_secs(2)
            && !seen
                .lock()
                .unwrap()
                .iter()
                .any(|p| p["inherited_by"] == child.as_str())
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .any(|p| p["inherited_by"] == child.as_str()),
            "the inheriting team is signalled: {:?}",
            seen.lock().unwrap()
        );
    }

    /// SW-LN-B28 (supersedes the SW-WL-4 AC-2 zero-subscriber cache skip): with NO
    /// per-workspace subscribers — a local org with nothing open, or any team whose
    /// worktrees are closed — a story written outside the app must still reach the
    /// story index, or `resolve_story_path` / Run gate refuse it until a restart. The
    /// dispatch refreshes the ONE changed file (a single stat + read) but emits
    /// nothing per-subscriber.
    #[test]
    fn dispatch_context_event_refreshes_index_but_no_fanout_with_zero_subscribers() {
        use crate::context_repo::TEAM_CONTENT_DIR;
        use std::fs;

        let dir = std::env::temp_dir()
            .join("codesense_test")
            .join(format!("wl4_zero_sub_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let content_dir = dir.join("default").join(TEAM_CONTENT_DIR);
        fs::create_dir_all(&content_dir).unwrap();
        let story_path = content_dir.join("story.md");
        fs::write(
            &story_path,
            "---\nstatus: in-progress\nstory_id: S-1\nsource_repo: org/repo-a\n---\n# S",
        )
        .unwrap();

        let cache = FrontmatterCache::shared(FrontmatterCache::build_from_scan(&dir));
        assert_eq!(
            cached_story_status(&cache.read().unwrap()).as_deref(),
            Some("in-progress"),
            "precondition: cache built from the in-progress file"
        );

        // Mutate on disk to `done` — the refresh must observe this.
        fs::write(
            &story_path,
            "---\nstatus: done\nstory_id: S-1\nsource_repo: org/repo-a\ndone:\n  completed_at: 2025-06-01T00:00:00Z\n---\n# S",
        )
        .unwrap();

        let emit_log: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let emit_rec = Arc::clone(&emit_log);

        dispatch_context_event(
            &story_path,
            &content_dir,
            &[], // zero subscribers
            Some(&cache),
            |config, _payload| emit_rec.lock().unwrap().push(config.repo_path.clone()),
        );

        assert_eq!(
            cached_story_status(&cache.read().unwrap()).as_deref(),
            Some("done"),
            "zero-subscriber dispatch refreshes the changed story in the index"
        );
        assert!(
            emit_log.lock().unwrap().is_empty(),
            "no per-subscriber emit with zero subscribers"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// SW-LN-B28: the zero-subscriber refresh stays cheap — a non-pipeline file and a
    /// framework install root (`.claude/`, `_bmad/`, whose bursts no index reads) are
    /// never read into the story index.
    #[test]
    fn zero_subscriber_refresh_skips_framework_roots_and_non_pipeline_files() {
        use crate::context_repo::TEAM_CONTENT_DIR;
        use std::fs;

        let tmp = tempfile::TempDir::new().unwrap();
        let content_dir = tmp.path().join("default").join(TEAM_CONTENT_DIR);
        let skill = content_dir.join(".claude/skills/x/SKILL.md");
        let bmad = content_dir.join("_bmad/core/story.md");
        let json = content_dir.join("notes/data.json");
        for (p, body) in [
            (&skill, "---\nstory_id: SK-1\nstatus: todo\n---\n"),
            (&bmad, "---\nstory_id: BM-1\nstatus: todo\n---\n"),
            (&json, "{}"),
        ] {
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, body).unwrap();
        }
        let cache = FrontmatterCache::shared(FrontmatterCache::new());
        for p in [&skill, &bmad, &json] {
            dispatch_context_event(p, &content_dir, &[], Some(&cache), |_, _| {});
        }
        assert!(
            cache.read().unwrap().entries.is_empty(),
            "framework roots and non-pipeline files never enter the index"
        );
    }

    /// SW-LN-B28 SEAM (AC-1): a story file written into team content by ANOTHER process,
    /// with no workspace open (zero subscribers — the local-org case), becomes resolvable
    /// by `story_id` through the REAL watcher → dispatcher → context dispatch path within
    /// seconds, with no restart and no app-authored write.
    #[test]
    fn seam_externally_written_story_resolves_by_id_without_restart() {
        use crate::content_engine::dispatcher::Dispatcher;
        use crate::content_engine::test_support::act_until;
        use crate::content_engine::watcher::{WatchDir, WatchSetup, Watcher};
        use crate::context_repo::TEAM_CONTENT_DIR;
        use std::time::Duration;

        let tmp = tempfile::TempDir::new().unwrap();
        let content_dir = tmp
            .path()
            .canonicalize()
            .unwrap()
            .join("general")
            .join(TEAM_CONTENT_DIR);
        std::fs::create_dir_all(content_dir.join("two-repo-epic")).unwrap();
        let cache = FrontmatterCache::shared(FrontmatterCache::new());

        let dispatcher = Arc::new(Dispatcher::new(50, 100, 60_000, 60_000));
        let handler_cache = cache.clone();
        let watched = content_dir.clone();
        dispatcher.register_handler(
            &content_dir,
            Arc::new(move |event_path: &Path| {
                // The production context handler's dispatch, with the zero-subscriber
                // snapshot a local org with no open workspace has.
                dispatch_context_event(event_path, &watched, &[], Some(&handler_cache), |_, _| {});
            }),
        );
        let _watcher = Watcher::new(
            WatchSetup {
                dirs: vec![WatchDir {
                    path: content_dir.clone(),
                    recursive: true,
                }],
            },
            Arc::clone(&dispatcher),
        )
        .unwrap();
        // An agent writes the story as a plain file. SW-XS-12: repeated (the
        // same bytes) until observed, so a kernel stream that attaches late on
        // a loaded machine cannot lose the only event.
        let mut resolved = None;
        act_until(
            Duration::from_millis(500),
            || {
                std::fs::write(
                    content_dir.join("two-repo-epic/TR-2-app-shows-health.md"),
                    "---\ntype: story\nstory_id: TR-2\nstatus: qa\nsource_repo: local/fixture-app\n---\n# TR-2\n",
                )
                .unwrap()
            },
            || {
                let snap = FrontmatterCache::snapshot(&cache);
                resolved =
                    crate::commands::dashboard_launch::resolve_story_path_in(&snap, "TR-2").ok();
                resolved.is_some()
            },
        );
        let resolved =
            resolved.expect("TR-2 must resolve by id after an external write, with no restart");
        assert_eq!(resolved.team, "general");
        assert!(resolved
            .story_file_path
            .ends_with("TR-2-app-shows-health.md"));
    }

    /// SW-XS-3 SEAM (F1, AC1 + AC2): a team content edit observed through the REAL
    /// watcher → dispatcher → `build_context_handler`, with three live worktree
    /// subscribers and a registered coordinator holding the active team's target,
    /// emits `context-file-change` to each subscriber and lands in the story index
    /// — and enqueues no wave and writes nothing into any worktree. On `main` the
    /// same handler enqueued a `ContentLayerEvent` wave for the edit.
    #[test]
    fn seam_content_edit_with_three_live_worktrees_refreshes_and_writes_nothing() {
        use crate::content_engine::dispatcher::Dispatcher;
        use crate::content_engine::test_support::act_until;
        use crate::content_engine::watcher::{WatchDir, WatchSetup, Watcher};
        use crate::context_repo::TEAM_CONTENT_DIR;
        use std::time::Duration;

        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let content_dir = root.join("general").join(TEAM_CONTENT_DIR);
        std::fs::create_dir_all(content_dir.join("epic")).unwrap();
        let worktrees: Vec<PathBuf> = (0..3).map(|i| root.join(format!("wt-{i}"))).collect();
        for wt in &worktrees {
            std::fs::create_dir_all(wt.join("_bmad-output")).unwrap();
            std::fs::write(wt.join("_bmad-output/existing.md"), b"old copy").unwrap();
        }
        let snapshot = |dirs: &[PathBuf]| {
            let mut out = Vec::new();
            for dir in dirs {
                let mut stack = vec![dir.clone()];
                while let Some(d) = stack.pop() {
                    for e in std::fs::read_dir(&d).unwrap() {
                        let p = e.unwrap().path();
                        if p.is_dir() {
                            stack.push(p);
                        } else {
                            out.push((p.clone(), std::fs::read(&p).unwrap()));
                        }
                    }
                }
            }
            out.sort();
            out
        };
        let before = snapshot(&worktrees);

        // The REAL handler, on a mock app that registers a WaveCoordinator with the
        // team's target stashed — the exact state in which `main` enqueued a
        // `ContentLayerEvent` per edit — plus the frontmatter cache.
        let coord = Arc::new(crate::content_engine::sync::WaveCoordinator::new());
        coord.stash_active_projection_on_team_change(
            &crate::content_engine::ResolverContext {
                context_root: None,
                codesense_home: root.clone(),
                user_id: 42,
                org_slug: "acme/context".to_string(),
                team_name: Some("general".to_string()),
            },
            &[crate::domain::SourceRepoSlug::new("acme", "api").unwrap()],
        );
        let cache = FrontmatterCache::shared(FrontmatterCache::new());
        // `WatcherState` makes `WaveDeps::from_app` succeed, so a reintroduced
        // enqueue would reach the coordinator instead of bailing before it.
        let app = tauri::test::mock_builder()
            .manage(cache.clone())
            .manage(Arc::clone(&coord))
            .manage(WatcherState::default())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let emits: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        {
            use tauri::Listener;
            let emits = Arc::clone(&emits);
            app.listen("context-file-change", move |event| {
                let payload: serde_json::Value =
                    serde_json::from_str(event.payload()).unwrap_or_default();
                emits.lock().unwrap().push(
                    payload["repo_path"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                );
            });
        }
        let path_to_repo: PathToRepo = Arc::new(Mutex::new(HashMap::from([(
            content_dir.clone(),
            worktrees
                .iter()
                .map(|wt| ssfu1_config(&wt.to_string_lossy(), &content_dir.to_string_lossy()))
                .collect(),
        )])));
        let dispatcher = Arc::new(Dispatcher::new(50, 100, 60_000, 60_000));
        dispatcher.register_handler(
            &content_dir,
            build_context_handler(app.handle().clone(), path_to_repo, content_dir.clone()),
        );
        let _watcher = Watcher::new(
            WatchSetup {
                dirs: vec![WatchDir {
                    path: content_dir.clone(),
                    recursive: true,
                }],
            },
            Arc::clone(&dispatcher),
        )
        .unwrap();
        // SW-XS-12: the edit is repeated (the same bytes) until both outcomes
        // are observed, so a late-attaching kernel stream cannot lose it.
        let mut resolved = false;
        let distinct_emits = || {
            let mut emitted = emits.lock().unwrap().clone();
            emitted.sort();
            emitted.dedup();
            emitted.len()
        };
        act_until(
            Duration::from_millis(500),
            || {
                std::fs::write(
                    content_dir.join("epic/XS-3-edit.md"),
                    "---\ntype: story\nstory_id: XS-EDIT\nstatus: ready\nsource_repo: acme/api\n---\n# edit\n",
                )
                .unwrap()
            },
            || {
                let snap = FrontmatterCache::snapshot(&cache);
                resolved =
                    crate::commands::dashboard_launch::resolve_story_path_in(&snap, "XS-EDIT")
                        .is_ok();
                resolved && distinct_emits() >= 3
            },
        );
        assert!(resolved, "the edit must reach the story index (AC2)");
        let mut emitted = emits.lock().unwrap().clone();
        emitted.sort();
        emitted.dedup();
        assert_eq!(
            emitted.len(),
            3,
            "context-file-change fires for every live worktree subscriber (AC2)"
        );
        // Give any (removed) fan-out time to have run before checking for writes.
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            coord.enqueued_count(),
            0,
            "a content edit enqueues no wave (AC1)"
        );
        assert_eq!(
            coord.entry_attempts(),
            0,
            "no coordinator entry point is reached"
        );
        assert!(!coord.has_wave_in_flight(), "no wave in flight (AC1)");
        assert_eq!(
            snapshot(&worktrees),
            before,
            "no worktree file is written by a content edit (AC1)"
        );
    }

    /// SW-LN-B50 SEAM (AC-2): the index reads a story while it is still half
    /// written, and the event for the write that completes it lands inside the
    /// dispatcher's debounce window. The story must still resolve by id within
    /// 5 s, with no further change to the file.
    #[test]
    fn seam_story_read_mid_write_resolves_once_the_write_completes() {
        use crate::content_engine::dispatcher::Dispatcher;
        use crate::content_engine::test_support::{eventually, WATCH_DEADLINE};
        use crate::context_repo::TEAM_CONTENT_DIR;

        let tmp = tempfile::TempDir::new().unwrap();
        let content_dir = tmp
            .path()
            .canonicalize()
            .unwrap()
            .join("general")
            .join(TEAM_CONTENT_DIR);
        std::fs::create_dir_all(content_dir.join("two-repo-epic")).unwrap();
        let cache = FrontmatterCache::shared(FrontmatterCache::new());

        let dispatcher = Arc::new(Dispatcher::new(50, 100, 60_000, 60_000));
        let handler_cache = cache.clone();
        let watched = content_dir.clone();
        dispatcher.register_handler(
            &content_dir,
            Arc::new(move |event_path: &Path| {
                dispatch_context_event(event_path, &watched, &[], Some(&handler_cache), |_, _| {});
            }),
        );

        let story = content_dir.join("two-repo-epic/TR-1-api-health-endpoint.md");
        // The create event: the index reads the file before its frontmatter is on disk.
        std::fs::write(&story, "---\ntype: story\n").unwrap();
        dispatcher.dispatch(&story);
        assert!(
            crate::commands::dashboard_launch::resolve_story_path_in(
                &FrontmatterCache::snapshot(&cache),
                "TR-1"
            )
            .is_err(),
            "the half-written file is not a story yet"
        );
        // The write that completes it, inside the debounce window.
        std::fs::write(
            &story,
            "---\ntype: story\nstory_id: TR-1\nstatus: ready\nsource_repo: local/fixture-api\n---\n# TR-1\n",
        )
        .unwrap();
        dispatcher.dispatch(&story);

        let mut resolved = None;
        eventually(WATCH_DEADLINE, || {
            let snap = FrontmatterCache::snapshot(&cache);
            resolved = crate::commands::dashboard_launch::resolve_story_path_in(&snap, "TR-1").ok();
            resolved.is_some()
        });
        let resolved = resolved.expect("TR-1 must resolve by id once its write completes");
        assert!(resolved
            .story_file_path
            .ends_with("TR-1-api-health-endpoint.md"));
    }

    /// SW-WL-4 AC-4: the refresh decision rides the CAPTURED snapshot, not a
    /// live re-read of `path_to_repo`. `dispatch_context_event` is handed an
    /// owned snapshot; a non-empty snapshot refreshes even if the live
    /// subscriber map has since emptied (subscriber closed mid-event). Here
    /// `path_to_repo` is irrelevant — passing a non-empty `&[cfg]` must
    /// refresh the cache to `done`.
    #[test]
    fn dispatch_context_event_refreshes_on_captured_snapshot_even_if_live_map_empty() {
        use crate::context_repo::TEAM_CONTENT_DIR;
        use std::fs;

        let dir = std::env::temp_dir()
            .join("codesense_test")
            .join(format!("wl4_snapshot_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let content_dir = dir.join("default").join(TEAM_CONTENT_DIR);
        fs::create_dir_all(&content_dir).unwrap();
        let story_path = content_dir.join("story.md");
        fs::write(
            &story_path,
            "---\nstatus: in-progress\nstory_id: S-1\nsource_repo: org/repo-a\n---\n# S",
        )
        .unwrap();

        let cache = FrontmatterCache::shared(FrontmatterCache::build_from_scan(&dir));
        assert_eq!(
            cached_story_status(&cache.read().unwrap()).as_deref(),
            Some("in-progress")
        );
        fs::write(
            &story_path,
            "---\nstatus: done\nstory_id: S-1\nsource_repo: org/repo-a\ndone:\n  completed_at: 2025-06-01T00:00:00Z\n---\n# S",
        )
        .unwrap();

        // context_repo_path = the per-team content dir → parse extracts team.
        let cfg = ssfu1_config("/repo/a", &content_dir.to_string_lossy());

        dispatch_context_event(
            &story_path,
            &content_dir,
            std::slice::from_ref(&cfg), // non-empty snapshot
            Some(&cache),
            |_c, _p| {},
        );

        assert_eq!(
            cached_story_status(&cache.read().unwrap()).as_deref(),
            Some("done"),
            "non-empty captured snapshot must refresh the cache regardless of the live subscriber map"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// SW-WL-4 AC-7 (the T1.3 linchpin): closing the LAST per-workspace
    /// subscriber of the active team's `context_output` must NOT tear down
    /// its kernel watch / dispatcher handler. The dir is team-owned
    /// (`team_context_dirs`), so `remove_watch_inner` re-seeds its empty key
    /// and skips unregister/unwatch — while the per-workspace install dirs
    /// ARE retracted. A plain recording handler (fires regardless of
    /// subscriber count) proves the handler survives.
    #[test]
    fn team_context_dir_survives_last_workspace_close() {
        let state = WatcherState::default();
        let ctx_path = "/test_org_wl4/_team/default/content";
        // Each workspace has its own per-workspace contrast dir (a non-team
        // dir that SHOULD be retracted on close) alongside the shared team
        // context dir. (Post-SW-WL-5 `WatchDirs` no longer has `repo_root`;
        // SW-CP-18 retired `install_dirs`, so the contrast dir is a synthetic
        // per-workspace path appended directly as a distinct subscriber key.)
        let cfg_a = WatchConfig {
            repo_path: "/repos/owner/a".into(),
            context_repo_path: ctx_path.into(),
        };
        let cfg_b = WatchConfig {
            repo_path: "/repos/owner/b".into(),
            context_repo_path: ctx_path.into(),
        };
        let wd_a = resolve_watch_dirs(&cfg_a);
        let wd_b = resolve_watch_dirs(&cfg_b);
        let team_dir = wd_a.context_output.clone();
        let install_a = PathBuf::from("/repos/owner/a/_contrast");
        let install_b = PathBuf::from("/repos/owner/b/_contrast");
        assert_eq!(
            team_dir, wd_b.context_output,
            "test invariant: both configs share the team context_output"
        );

        // Team layer owns the context watch (as register_team_context_watch would).
        state
            .team_context_dirs
            .lock()
            .unwrap()
            .insert(team_dir.clone());

        // Two workspaces subscribe at the shared team dir + their own install dir.
        append_subscriber_with_replace(&state.path_to_repo, &team_dir, &cfg_a).unwrap();
        append_subscriber_with_replace(&state.path_to_repo, &team_dir, &cfg_b).unwrap();
        append_subscriber_with_replace(&state.path_to_repo, &install_a, &cfg_a).unwrap();
        append_subscriber_with_replace(&state.path_to_repo, &install_b, &cfg_b).unwrap();

        // Plain recording handler at the team dir — fires on dispatch even
        // with zero subscribers, so we can distinguish "still registered"
        // from "unregistered" (a fan-out handler can't: empty subs = no work).
        let fired: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let fired_c = Arc::clone(&fired);
        let handler: DispatchHandler = Arc::new(move |_p: &Path| {
            fired_c.lock().unwrap().push("fired".into());
        });
        state.dispatcher.register_handler(&team_dir, handler);

        // Close BOTH workspaces (the whole team's open surfaces).
        remove_watch_inner(&state, &cfg_a).unwrap();
        remove_watch_inner(&state, &cfg_b).unwrap();

        {
            let map = state.path_to_repo.lock().unwrap();
            assert!(
                map.contains_key(&team_dir),
                "team context dir must remain a path_to_repo key after all workspaces close"
            );
            assert_eq!(
                map.get(&team_dir).map(|v| v.len()),
                Some(0),
                "team context dir must be present but with an empty subscriber Vec"
            );
            assert!(
                !map.contains_key(&install_a),
                "per-workspace install dir A must be retracted+removed"
            );
            assert!(
                !map.contains_key(&install_b),
                "per-workspace install dir B must be retracted+removed"
            );
        }

        // The team dispatcher handler must NOT have been unregistered.
        state
            .dispatcher
            .dispatch(&team_dir.join("teammate_pull.md"));
        assert!(
            !fired.lock().unwrap().is_empty(),
            "team context dispatcher handler must survive last-workspace close"
        );
    }

    /// SW-WL-4 T3.1: `swap_team_context_dir` set-diff that drives the
    /// team-switch re-target. Launch (empty → owns new, no stale); switch
    /// A→B (owns B, returns [A] to detach); idempotent same-team re-call
    /// (owns B, no stale).
    #[test]
    fn swap_team_context_dir_diffs_for_launch_switch_and_idempotent_recall() {
        let mut owned: HashSet<PathBuf> = HashSet::new();
        let a = PathBuf::from("/org/_team/alpha/content");
        let b = PathBuf::from("/org/_team/beta/content");

        // Launch: empty → own A, nothing stale.
        let stale = swap_team_context_dir(&mut owned, &a);
        assert!(stale.is_empty(), "launch has no previous team to detach");
        assert!(owned.contains(&a) && owned.len() == 1);

        // Switch A → B: detach A, own B.
        let stale = swap_team_context_dir(&mut owned, &b);
        assert_eq!(
            stale,
            vec![a.clone()],
            "switch must detach the old team dir"
        );
        assert!(owned.contains(&b) && !owned.contains(&a) && owned.len() == 1);

        // Idempotent same-team re-call: no stale, still owns B.
        let stale = swap_team_context_dir(&mut owned, &b);
        assert!(stale.is_empty(), "same-team re-call detaches nothing");
        assert!(owned.contains(&b) && owned.len() == 1);
    }

    /// SW-WL-4 (review M1): on team switch, a stale team context dir that STILL
    /// has a lingering per-workspace subscriber must be RELINQUISHED (left for
    /// `remove_watch_inner` to reclaim), not detached — otherwise a hard switch
    /// racing the frontend's fire-and-forget per-workspace `removeWatch` would
    /// yank the watch from a still-open old-team workspace. A stale dir with no
    /// subscriber is detached and its seeded empty key dropped.
    #[test]
    fn relinquish_or_detach_stale_dir_guards_lingering_subscribers() {
        let path_map: PathToRepo = Arc::new(Mutex::new(HashMap::new()));
        let with_sub = PathBuf::from("/org/_team/alpha/content");
        let empty_seeded = PathBuf::from("/org/_team/beta/content");
        let absent = PathBuf::from("/org/_team/gamma/content");

        let cfg = ssfu1_config("/repo/a", "/org/_team/alpha/content");
        append_subscriber_with_replace(&path_map, &with_sub, &cfg).unwrap();
        // Seed an empty key (as register_team_context_watch does on attach).
        path_map
            .lock()
            .unwrap()
            .entry(empty_seeded.clone())
            .or_default();

        // Lingering subscriber → relinquish (true); key + subscriber preserved.
        assert!(
            relinquish_or_detach_stale_dir(&path_map, &with_sub),
            "a stale dir with a live subscriber must be relinquished, not detached"
        );
        assert!(
            path_map.lock().unwrap().contains_key(&with_sub),
            "relinquished dir's key (and its subscriber) must be preserved"
        );

        // Empty seeded key → detach (false); key dropped under the same lock.
        assert!(
            !relinquish_or_detach_stale_dir(&path_map, &empty_seeded),
            "a stale dir with no subscriber must be detached"
        );
        assert!(
            !path_map.lock().unwrap().contains_key(&empty_seeded),
            "detached dir's seeded empty key must be removed"
        );

        // Absent key → detach (false); nothing to preserve or remove.
        assert!(
            !relinquish_or_detach_stale_dir(&path_map, &absent),
            "an absent stale dir is treated as detach"
        );
    }

    // -----------------------------------------------------------------------
    // SS-FU-1 review iter-1: wired-up AC-3/AC-4 + rollback restore tests
    // -----------------------------------------------------------------------

    /// Auditor M-1: pin the WIRED-UP `remove_watch_inner` contract — not
    /// just the helper. Verifies that after retracting the last subscriber
    /// at a shared dir, the dispatcher's registered handler is actually
    /// removed (subsequent dispatches produce no work). Catches a future
    /// regression that breaks the wiring between `retract_subscriber_collect_emptied`
    /// and `dispatcher.unregister_handler`.
    #[test]
    fn remove_watch_inner_unregisters_dispatcher_handler_only_when_dir_empties() {
        let state = WatcherState::default();
        // PTS-2 flat layout — context_repo_path includes
        // `_team/default/content` so resolve_watch_dirs produces a
        // deterministic shared `context_output` dir.
        let ctx_path = "/test_org_ssfu1/_team/default/content";

        let cfg_a = ssfu1_config("/repos/owner/a", ctx_path);
        let cfg_b = ssfu1_config("/repos/owner/b", ctx_path);
        let wd_a = resolve_watch_dirs(&cfg_a);
        let wd_b = resolve_watch_dirs(&cfg_b);

        // Derive the shared dir from resolve_watch_dirs rather than
        // hardcoding — keeps the test honest if resolve_watch_dirs
        // ever changes how it derives context_output (iter-2 LOW).
        let shared_dir = wd_a.context_output.clone();
        assert_eq!(
            shared_dir, wd_b.context_output,
            "test invariant: both configs must derive the same shared context_output"
        );

        // Pre-populate path_to_repo: both configs subscribed at the
        // shared context_output (the cross-config collision surface that
        // empties only when its last subscriber retracts).
        append_subscriber_with_replace(&state.path_to_repo, &shared_dir, &cfg_a).unwrap();
        append_subscriber_with_replace(&state.path_to_repo, &shared_dir, &cfg_b).unwrap();

        // Register a recording fan-out handler at the shared dir.
        let recorded: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let rec_clone = Arc::clone(&recorded);
        let handler = build_fanout_handler(
            Arc::clone(&state.path_to_repo),
            shared_dir.clone(),
            move |_event, config| {
                rec_clone.lock().unwrap().push(config.repo_path.clone());
            },
        );
        state.dispatcher.register_handler(&shared_dir, handler);

        // AC-3 wired: retract A. Shared dir still has B → handler must
        // remain registered → dispatch fires for B.
        remove_watch_inner(&state, &cfg_a).unwrap();
        state.dispatcher.dispatch(&shared_dir.join("after_a.md"));
        {
            let recs = recorded.lock().unwrap();
            assert_eq!(
                *recs,
                vec!["/repos/owner/b"],
                "after remove_watch_inner(A), shared dir still serves B and only B"
            );
        }

        // AC-4 wired: retract B. Shared dir empties → dispatcher handler
        // must be unregistered → subsequent dispatch produces NO work.
        recorded.lock().unwrap().clear();
        remove_watch_inner(&state, &cfg_b).unwrap();
        state.dispatcher.dispatch(&shared_dir.join("after_b.md"));
        let recs = recorded.lock().unwrap();
        assert!(
            recs.is_empty(),
            "after remove_watch_inner(B) (last subscriber), dispatcher closure must be unregistered (got {:?})",
            *recs
        );
    }

    /// SS-FU-1 review iter-1 + iter-2: `append_subscriber_with_replace`
    /// returns the displaced subscriber so `rollback_phase_2a` can
    /// restore it on a Phase 2b attach failure. Restore must happen
    /// BEFORE the dispatcher-unregister step so the restored
    /// subscriber's dispatch wiring stays intact — iter-2 caught a bug
    /// where the unregister ran first, leaving the restored subscriber
    /// in `path_to_repo` but with no closure routing events to it
    /// (silent event loss).
    ///
    /// This test verifies the full contract: (a) the displaced
    /// subscriber is returned by the append helper, (b) rollback
    /// restores it to `path_to_repo`, AND (c) a dispatch through the
    /// real dispatcher actually fires for the restored subscriber.
    #[test]
    fn rollback_phase_2a_restores_displaced_subscriber_and_preserves_dispatch() {
        let dispatcher = Arc::new(Dispatcher::new(50, 100, 60_000, 500));
        let path_map: PathToRepo = Arc::new(Mutex::new(HashMap::new()));
        let dir = PathBuf::from("/some/shared/dir");

        // Pre-state: subscriber A v1 registered + dispatcher closure
        // installed (mirroring what register_directory_handlers Phase
        // 2a would have done for the first registration).
        let cfg_a_v1 = ssfu1_config("/repos/a", "/ctx/_team/default/content");
        let prior = append_subscriber_with_replace(&path_map, &dir, &cfg_a_v1).unwrap();
        assert!(prior.is_none(), "first registration has no displaced");
        let recorded: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let rec_clone = Arc::clone(&recorded);
        let handler =
            build_fanout_handler(Arc::clone(&path_map), dir.clone(), move |_event, config| {
                rec_clone
                    .lock()
                    .unwrap()
                    .push(format!("{}@{}", config.repo_path, config.context_repo_path));
            });
        dispatcher.register_handler(&dir, handler);

        // Re-register A (same identity). The helper returns the displaced
        // v1. (SW-XS-12: a config is its identity alone now, so v1 and v2
        // are equal; what matters is that rollback puts A back.)
        let cfg_a_v2 = cfg_a_v1.clone();
        let displaced = append_subscriber_with_replace(&path_map, &dir, &cfg_a_v2).unwrap();
        assert_eq!(
            displaced.as_ref(),
            Some(&cfg_a_v1),
            "displaced subscriber must be the v1 config"
        );
        // Mid-state: only one entry visible.
        let mid_snapshot = snapshot_subscribers(&path_map, &dir);
        assert_eq!(mid_snapshot.len(), 1);

        // Simulate Phase 2b failure for v2 — invoke the same rollback
        // function the production code uses.
        let attempted = vec![(dir.clone(), displaced)];
        rollback_phase_2a(
            &path_map,
            &dispatcher,
            &attempted,
            cfg_a_v2.subscriber_identity(),
        );

        // (a) v1 restored in path_to_repo.
        let restored = snapshot_subscribers(&path_map, &dir);
        assert_eq!(
            restored.len(),
            1,
            "rollback must leave the prior subscriber in place"
        );
        assert_eq!(
            restored[0], cfg_a_v1,
            "rollback must restore v1, not leave empty"
        );

        // (b) dispatcher closure preserved: dispatching through the
        // real dispatcher fires the fan-out handler, which reads v1
        // from path_to_repo and records it. Iter-2 bug: previously,
        // the unregister ran before the restore so the closure was
        // gone — this assert would fail with `recs` empty.
        dispatcher.dispatch(&dir.join("dispatch_after_rollback.md"));
        let recs = recorded.lock().unwrap();
        assert_eq!(
            *recs,
            vec!["/repos/a@/ctx/_team/default/content"],
            "rollback must preserve dispatcher closure so restored subscriber receives events (iter-2 HIGH-1 regression guard)"
        );
    }

    /// SS-FU-1 review iter-3 + iter-4 + iter-5: duplicate dirs in
    /// `attempted` (user-supplied `install_dirs` overlapping a
    /// singleton dir) must NOT leak the failing config back into
    /// `path_to_repo` AND must release the dispatcher closure.
    /// The second iteration's `displaced` is the failing config
    /// itself (self-displacement), not a genuine prior owner.
    ///
    /// Iter-3 dedups by dir keeping FIRST. Iter-4 added a fan-out
    /// handler dispatch assertion. Iter-5 switches to a BARE
    /// invocation-tracker (`AtomicBool`) so the post-rollback
    /// assertion strictly distinguishes "closure unregistered"
    /// (handler not invoked) from "closure preserved but path_map
    /// empty" (handler invoked but fan-out snapshot was empty) —
    /// the iter-4 fan-out version couldn't tell those apart. Iter-5
    /// also adds a positive-control dispatch BEFORE rollback to
    /// prove handler registration is correct (so the post-rollback
    /// assertion isn't trivially satisfied by a misregistered key).
    #[test]
    fn rollback_phase_2a_dedupes_attempted_to_prevent_self_displacement_restore() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let dispatcher = Arc::new(Dispatcher::new(50, 100, 60_000, 500));
        let path_map: PathToRepo = Arc::new(Mutex::new(HashMap::new()));
        let dir = PathBuf::from("/dup/dir");

        let failing_cfg = ssfu1_config("/repos/x", "/ctx/_team/default/content");

        // Simulate Phase 2a appending failing_cfg twice at the same
        // dir (duplicate-dir scenario). First: displaced=None.
        // Second: displaced=Some(failing_cfg) (helper found its
        // first-push value).
        append_subscriber_with_replace(&path_map, &dir, &failing_cfg).unwrap();
        let self_displaced = append_subscriber_with_replace(&path_map, &dir, &failing_cfg).unwrap();
        assert_eq!(
            self_displaced.as_ref().map(|c| c.repo_path.as_str()),
            Some("/repos/x"),
            "second duplicate-dir append surfaces failing_cfg as its own displaced (self-displacement)"
        );

        // Iter-5: BARE invocation tracker — flips true iff dispatcher
        // actually finds + invokes the closure, independent of any
        // path_to_repo snapshot.
        let invoked = Arc::new(AtomicBool::new(false));
        let inv_clone = Arc::clone(&invoked);
        let handler: DispatchHandler = Arc::new(move |_path: &Path| {
            inv_clone.store(true, Ordering::Relaxed);
        });
        dispatcher.register_handler(&dir, handler);

        // Positive control: dispatch BEFORE rollback to prove the
        // handler is correctly registered + the dispatcher's
        // ancestor-walk lookup matches our synthetic `/dup/dir` key.
        // If this fails the rest of the test is meaningless.
        dispatcher.dispatch(&dir.join("positive_control_before_rollback.md"));
        assert!(
            invoked.load(Ordering::Relaxed),
            "positive control: handler must fire before rollback (proves correct registration)"
        );
        invoked.store(false, Ordering::Relaxed); // reset for post-rollback assertion

        let attempted = vec![(dir.clone(), None), (dir.clone(), self_displaced)];
        rollback_phase_2a(
            &path_map,
            &dispatcher,
            &attempted,
            failing_cfg.subscriber_identity(),
        );

        // (a) Rollback must remove the failing config completely.
        // FIRST entry's None says "no genuine prior" — dedup keeps
        // that, drops the second's self-displaced bogus restore.
        let snapshot = snapshot_subscribers(&path_map, &dir);
        assert!(
            snapshot.is_empty(),
            "duplicate-dir rollback must not leak the failing config back into path_to_repo (got {:?})",
            snapshot
        );

        // (b) Dispatcher closure must be unregistered. With a bare
        // tracker, `invoked` flipping to true would mean the closure
        // is still registered; staying false proves
        // `dispatcher.lookup_handler` returned None — the closure
        // was unregistered.
        dispatcher.dispatch(&dir.join("dispatch_after_dedup_rollback.md"));
        assert!(
            !invoked.load(Ordering::Relaxed),
            "duplicate-dir rollback must unregister the dispatcher closure (post-rollback dispatch must not invoke the bare tracker)"
        );
    }

    // -----------------------------------------------------------------------
    // SW-WU-14: managed-clone `.git/worktrees/` watch → emit worktree-changed
    // -----------------------------------------------------------------------

    /// Wire `attach_worktree_admin_watches` over one temp clone's `.git/worktrees/` admin dir
    /// with a probe handler (a counter in place of the real `emit_worktree_changed`). Returns
    /// the live watcher (kept alive by the caller), the dispatcher, the watched
    /// `.git/worktrees/` dir, and the fire-count the probe increments.
    fn wu14_probe(
        clone: &Path,
    ) -> (
        Arc<ContentWatcher>,
        Arc<Dispatcher>,
        PathBuf,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        use std::sync::atomic::AtomicUsize;
        let dispatcher = Arc::new(Dispatcher::new(50, 100, 60_000, 500));
        let watcher = Arc::new(
            ContentWatcher::new(WatchSetup { dirs: vec![] }, Arc::clone(&dispatcher)).unwrap(),
        );
        let fired = Arc::new(AtomicUsize::new(0));
        let handler: DispatchHandler = {
            let fired = Arc::clone(&fired);
            Arc::new(move |_p: &Path| {
                fired.fetch_add(1, Ordering::Relaxed);
            })
        };
        let git_worktrees = clone.join(".git").join("worktrees");
        attach_worktree_admin_watches(
            &watcher,
            &dispatcher,
            std::slice::from_ref(&git_worktrees),
            &handler,
        );
        (watcher, dispatcher, git_worktrees, fired)
    }

    /// AC-2: a change under a watched clone's `.git/worktrees/` (what an external
    /// `git worktree add` writes) dispatches the emit handler.
    #[test]
    fn wu14_worktrees_change_dispatches_emit() {
        let tmp = tempfile::TempDir::new().unwrap();
        let clone = tmp.path().join("clone");
        let (_watcher, dispatcher, git_worktrees, fired) = wu14_probe(&clone);

        // The per-worktree admin subdir + its HEAD file appear on `git worktree add`.
        dispatcher.dispatch(&git_worktrees.join("feature-x").join("HEAD"));
        assert_eq!(
            fired.load(Ordering::Relaxed),
            1,
            "a change under `.git/worktrees/` must fire the emit handler"
        );
    }

    /// AC-4: a dropped/overflowed FS event (FSEvents `MustScanSubDirs` / inotify overflow)
    /// routes through the rescan handler on the same dir → emit.
    #[test]
    fn wu14_rescan_routes_to_emit() {
        let tmp = tempfile::TempDir::new().unwrap();
        let clone = tmp.path().join("clone");
        let (_watcher, dispatcher, git_worktrees, fired) = wu14_probe(&clone);

        dispatcher.dispatch_rescan(&git_worktrees.join("feature-x"));
        assert_eq!(
            fired.load(Ordering::Relaxed),
            1,
            "a rescan/overflow signal must recover via the rescan handler → emit"
        );
    }

    /// AC-3: re-asserting the watch on the same clone dedups to `AlreadyAttached` — this is
    /// what makes per-activation re-assert a cheap no-op.
    #[test]
    fn wu14_reregistration_is_a_noop() {
        let tmp = tempfile::TempDir::new().unwrap();
        let clone = tmp.path().join("clone");
        let (watcher, _dispatcher, git_worktrees, _fired) = wu14_probe(&clone);

        let outcome = watcher
            .watch(WatchDir {
                path: git_worktrees,
                recursive: false,
            })
            .expect("re-watch must not error");
        assert!(
            matches!(outcome, WatchOutcome::AlreadyAttached(_)),
            "re-registering an already-watched clone dir must dedup to AlreadyAttached"
        );
    }

    /// SW-WU-14 detach-on-switch: activating team B after team A returns team A's now-stale
    /// `.git/worktrees` dirs (those not in B's set) for detach, and leaves `owned` equal to
    /// B's set — so the watch-set tracks the active team's scan-set instead of accumulating.
    #[test]
    fn wu14_swap_returns_stale_dirs_and_tracks_active_set() {
        let a1 = PathBuf::from("/clones/a1/.git/worktrees");
        let shared = PathBuf::from("/clones/shared/.git/worktrees");
        let b1 = PathBuf::from("/clones/b1/.git/worktrees");

        let mut owned = HashSet::new();
        // Team A activation: nothing stale (owned was empty), owned becomes {a1, shared}.
        let stale = swap_worktree_admin_dirs(&mut owned, &[a1.clone(), shared.clone()]);
        assert!(stale.is_empty(), "first activation detaches nothing");
        assert_eq!(owned, HashSet::from([a1.clone(), shared.clone()]));

        // Switch to team B (shares `shared`, drops `a1`, adds `b1`): only `a1` detaches.
        let stale = swap_worktree_admin_dirs(&mut owned, &[shared.clone(), b1.clone()]);
        assert_eq!(
            stale,
            vec![a1],
            "only the clone unique to A detaches; shared stays"
        );
        assert_eq!(
            owned,
            HashSet::from([shared, b1]),
            "owned tracks the active team's set exactly (no accumulation)"
        );
    }
    #[test]
    fn inherited_change_payload_excludes_private_changes_including_deletions() {
        let (_home, root) = crate::context_repo::inheritance::test_fixture();
        let active = Some(("acme/ctx", "child"));
        for owner in ["parent", "grand"] {
            assert!(
                inherited_change_payload(&root.join(owner).join("content/secret.md"), active)
                    .is_none()
            );
            let shared = root.join(owner).join("content/shared.md");
            assert!(inherited_change_payload(&shared, active).is_some());
            std::fs::remove_file(&shared).unwrap();
            assert!(inherited_change_payload(&shared, active).is_some());
        }
    }
}

#[cfg(test)]
mod prefs_bootstrap_tests {
    use super::*;
    use tauri::Manager;

    #[test]
    fn prefs_observation_attaches_when_the_existing_watcher_starts_without_a_team() {
        let data = tempfile::tempdir().unwrap();
        let path = data.path().join("app-state.json");
        let save = |wrap| {
            std::fs::write(
                &path,
                serde_json::to_vec(
                    &serde_json::json!({"viewer":{"settings":{"editorLineWrap":wrap}}}),
                )
                .unwrap(),
            )
            .unwrap()
        };
        save("on");
        let app = tauri::test::mock_builder()
            .manage(WatcherState::default())
            .manage(crate::live_hub::LiveHub::default())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let initial = crate::live_hub::configuration::prepare(
            app.handle(),
            data.path(),
            &crate::domain::Principal::from(42u64),
            "prefs",
            Some("global"),
        )
        .unwrap();
        crate::live_hub::subscribe(
            app.handle(),
            "prefs",
            &initial.view_key,
            None,
            Some("prefs-bootstrap"),
            None,
        )
        .unwrap();
        save("off");
        start_watching_impl(app.handle().clone(), vec![], 0).unwrap();
        let view = app
            .state::<crate::live_hub::LiveHub>()
            .snapshot(&initial.view_key, "prefs")
            .unwrap();
        assert!(
            view.snapshot.iter().any(|row| row["value"] == "off"),
            "watch startup must reconcile changes made before it could observe the file"
        );
        if let Some(watcher) = app.state::<WatcherState>().watcher_handle() {
            watcher.unwatch(data.path()).unwrap();
        }
        save("auto");
        app.state::<WatcherState>().dispatcher.dispatch(&path);
        let view = app
            .state::<crate::live_hub::LiveHub>()
            .snapshot(&initial.view_key, "prefs")
            .unwrap();
        assert!(view.snapshot.iter().any(|row| row["value"] == "auto"));
        if let Some(watcher) = app.state::<WatcherState>().watcher_handle() {
            let _ = watcher.unwatch(data.path());
        }
    }
}

#[cfg(test)]
#[path = "watch_runtime_tests.rs"]
mod runtime_tests;
