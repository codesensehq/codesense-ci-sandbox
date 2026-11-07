// SPDX-License-Identifier: MIT
//! Wave orchestration — async fan-out + perf budget.
//!
//! SLW-RI-2 · RI-2-ALLOW-FILE: engine-plane module. By RI-1's plane isolation
//! waves run on the `cs-engine` runtime, and every `with_blocking` mint here is
//! inside a `spawn_blocking` / `thread::scope` closure or a test — never on the
//! command-dispatch workers. The `ri2_backstop` scan opts this file out of the
//! per-call marker requirement wholesale (see `commands/ri2_backstop.rs`).
//!
//! [`Wave`] is the verb at this layer: given a [`WaveReason`], a
//! [`ResolverContext`], and a list of [`AttachedCodeRepo`] destinations,
//! it runs the per-repo body for its reason across every destination and
//! emits Tauri events at start + completion.
//!
//! SW-XS-3/SW-XS-4: no wave is automatic any more, in either direction.
//! SW-XS-8: the explicit MCP operations run on the preparation engine
//! (`agents::prepare`), never as a wave. The one remaining reason is the
//! migration cutover.
//!
//! [`WaveCoordinator`] is the process-wide registry that:
//! - Holds a per-target keyed wave lock (SLW-RI-4) so waves that share a target
//!   `code_repo_path` run one at a time, while disjoint targets run concurrently
//!   on the engine runtime (bounded by the engine-pool semaphore) (AC-4).
//! - Tracks in-flight waves so a new wave can supersede earlier ones per
//!   the [`WaveReason::supersedes`] policy (AC-5).
//! - Owns the per-code-repo lockfile mutex map (AC-6) — the finer-grained
//!   lockfile-mutation lock, distinct from the wave-level keyed lock.
//! - Stashes the active team's target on team change
//!   (`stash_active_projection_on_team_change`) — read by the agent reconcile
//!   and the read-only publish preview to resolve attached repos.
//!
//! [`WaveDeps`] is the dependency-injection envelope: production code
//! builds it via [`WaveDeps::from_app`] from a `tauri::AppHandle`; tests
//! and benches build it directly with a recording or no-op emit closure.
//! This decouples the wave engine from the Tauri runtime (D-17).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use log::{debug, info, warn};
use serde::Serialize;
use tauri::{Emitter, Manager};
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use super::super::layers::ResolverContext;
// SW-CP-34: `LayerId` is only referenced by tests now that the `SourceLayerEvent`
// variant (its sole production use) is retired.
#[cfg(test)]
use super::super::layers::LayerId;
use super::super::lockfile::AttachedCodeRepo;
use super::super::Error;
use crate::commands::watch::WatcherState;
use crate::domain::SourceRepoSlug;
use crate::install::frontmatter_cache::SharedFrontmatterCache;

// Re-export so the wave module's public API exposes related helpers
// alongside the wave types. AttachedCodeRepo lives in lockfile (it's a
// lockfile-derived concept). ctx_with_team lives in layers (pure ctx
// data manipulation).
pub use super::super::layers::ctx_with_team;

// =============================================================================
// AC-1: WaveReason / AttachedCodeRepo / Wave / WaveResult / WaveDeps
// =============================================================================

/// Why a wave is firing.
///
/// Variants encode user intent so the supersede policy (AC-5) can scope
/// cancellation correctly.
///
/// SW-XS-3: the automatic forward reasons (`Startup`, `TeamSwitch`,
/// `ContentLayerEvent`, `WorktreeDerive`, `EpicInheritance`) are gone — nothing
/// ambient writes content into a workspace any more. SW-XS-4: the automatic
/// reverse reasons (`ReverseSync`, `ReverseDirtyBatch`, `ReverseClaim`) are gone
/// too — nothing adopts worktree edits into team content on its own. SW-XS-8: the
/// agent reconcile is gone too (explicit operations run on the preparation
/// engine); what remains is the migration cutover.
///
/// `MigrationCutover.explicit_names = Some(_)` lets SS-12 pass the
/// migration set verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaveReason {
    MigrationCutover { explicit_names: Option<Vec<String>> },
}

/// SR-0 (Q1): the *code path that enqueued a wave*, captured at the enqueue
/// site — orthogonal to [`WaveReason`] (the semantic why), so the idle-storm
/// question "which enqueue site fired" has a second axis. SW-XS-4: the automatic
/// reverse triggers are gone; what remains is the agent reconcile.
/// Set beside `wave.reason` in each `enqueue_*` method and carried on the
/// in-flight registry so the gated `wave enqueued` log line names the trigger;
/// the histogram is computed by the SR-0 soak script over those lines (no
/// in-process counter). Also seeds SR-1's future scoping + the arch-review M-4
/// "marginal cost" metric, so it is a reusable seam, not throwaway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaveTrigger {
    /// Origin not distinguished (tests, and the plain `enqueue` delegate).
    Unspecified,
}

/// SW-XS-4 test probe — the "enqueue counter". Records every wave enqueue (per
/// target path) so the no-automatic-reverse tests can assert a stimulus enqueued
/// nothing, and the explicit-publish tests that it did.
#[cfg(test)]
pub(crate) mod intake_probe {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    static INTAKE: Mutex<Option<HashMap<PathBuf, usize>>> = Mutex::new(None);

    fn key(path: &Path) -> PathBuf {
        super::WaveCoordinator::normalize_target_path(path)
    }

    pub(crate) fn record(path: &Path) {
        let mut guard = INTAKE.lock().unwrap_or_else(|e| e.into_inner());
        *guard
            .get_or_insert_with(HashMap::new)
            .entry(key(path))
            .or_default() += 1;
    }

    /// Wave enqueues (any reason) that targeted `path` since process start.
    pub(crate) fn count(path: &Path) -> usize {
        let guard = INTAKE.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .as_ref()
            .and_then(|m| m.get(&key(path)).copied())
            .unwrap_or(0)
    }
}

impl WaveTrigger {
    /// Stable lower-kebab token for the `wave enqueued` log line — the SR-0
    /// soak parser keys off these exact strings, so treat them as a wire
    /// contract (an external scale-soak parser reads them; do not rename).
    pub fn as_str(self) -> &'static str {
        match self {
            WaveTrigger::Unspecified => "unspecified",
        }
    }
}

impl WaveReason {
    /// SR-0 (Q1): parser-friendly discriminant token for the `wave enqueued`
    /// log line — the variant name only, never the field payload (paths/teams
    /// contain spaces that would break the soak parser's field split). Wire
    /// contract with an external scale-soak parser; keep tokens stable.
    pub fn kind_str(&self) -> &'static str {
        match self {
            WaveReason::MigrationCutover { .. } => "migration_cutover",
        }
    }

    /// SLW-RI-4: does this wave write into the **shared source** (the team's
    /// context repo)? SW-XS-8: no wave does any more — an explicit publish
    /// (`reverse_sync_worktree`) runs on the preparation engine, never as a
    /// wave.
    pub fn writes_shared_source(&self) -> bool {
        false
    }

    /// AC-5 canonical supersede policy. Pure function over the matrix in
    /// the story's AC-5 section. `A.supersedes(B)` returns `true` iff a
    /// newly-enqueued wave `A` should cancel an in-flight wave `B`.
    pub fn supersedes(&self, other: &Self) -> bool {
        use WaveReason::*;
        match (self, other) {
            // MigrationCutover supersedes anything (including itself —
            // a second migration cancels the first as a safety net per
            // AC-5 footnote 2).
            (MigrationCutover { .. }, _) => true,
        }
    }
}

/// A single materialization wave. Constructed by callers (Tauri commands,
/// dispatcher handlers, boot init); driven to completion by [`Wave::run`].
pub struct Wave {
    pub reason: WaveReason,
    /// The active team's resolver context.
    pub ctx: ResolverContext,
    pub affected: Vec<AttachedCodeRepo>,
    pub cancel: CancellationToken,
    /// RF-perf-24 (AC-5): who requested this wave's reverse promote — carried
    /// here (not inside the reason) so the reason stays a light supersede
    /// discriminant. SW-XS-8: no remaining wave reverse-promotes, so every wave
    /// leaves the default `Auto`. Read at the single completion stamp seam in
    /// [`run_repo_dispatch`] to attribute [`stamp_reverse_sync`].
    pub initiator: crate::install::worktree_index::SyncInitiator,
    /// SW-XS-4: which way a wave may copy. Defaults to `Forward`; SW-XS-8: no
    /// wave sets `Reconcile` any more (`reverse_sync_worktree` runs on the
    /// preparation engine).
    pub direction: crate::install::story_linker::SyncMode,
}

/// SLW-RI-5 (AC-4): default fan-out cap for the content-edit materialize
/// fan-out (`Wave::run`'s per-repo loop). The common few-file edit is overhead-bound and scales near-linearly, but a
/// heavy multi-file edit across many worktrees still shares one disk on the
/// batched-fsync tail, so keep it bounded; `CS_MATERIALIZE_PARALLELISM`
/// overrides it per disk.
pub const CONTENT_FANOUT_CAP: usize = 8;

/// SLW-RI-6 (AC-3): backoff bounds for the permit×key requeue loop in
/// [`WaveCoordinator::acquire_engine_permit_and_keys`]. When a permit-holding
/// wave finds a target key contended, it drops BOTH the permit and any partial
/// key-guards and sleeps before retrying — so a permit is never occupied by a
/// key-blocked wave (the field's permit-squat throughput-collapse). The sleep
/// is what keeps the requeue from busy-spinning; it is not FIFO, which is safe
/// because same-key ordering is causal/mutual-exclusion, never lock-order (see
/// the SLW-RI-6 spike). Exponential min→max; `CS_REQUEUE_BACKOFF_MS` overrides
/// the floor (the escape hatch, mirroring `CS_*` fan-out knobs). Kept sub-ms
/// small: a requeue only fires under genuine key contention, and the key-holder
/// it waits on is always RUNNABLE (it holds a permit and is making progress),
/// so a short backoff drains fast without thrashing.
const REQUEUE_BACKOFF_MIN: Duration = Duration::from_micros(250);
const REQUEUE_BACKOFF_MAX: Duration = Duration::from_millis(8);

/// SLW-RI-6 (review R1): after this many failed permit+`try_lock` cycles, a wave
/// stops spinning and escalates to the FAIR blocking key acquire (see
/// [`WaveCoordinator::acquire_engine_permit_and_keys`]). Pure `try_lock`+backoff
/// prevents the permit-squat but NOT *starvation*: a broad-key-set wave
/// (a reverse wave on the per-org root) can lose the all-or-nothing `try_lock` race indefinitely against a
/// sustained stream of narrow single-key derives when the pool has >1 permit
/// (several narrows hold their keys at once, so "all N keys free at one instant"
/// may never recur). The escalation restores FIFO forward progress. At the
/// ~250µs→8ms backoff schedule this is ~200ms of contention before escalating —
/// long enough that transient contention resolves on the fast path, short enough
/// that a real burst escalates promptly.
const REQUEUE_STARVATION_THRESHOLD: u32 = 32;

/// SLW-RI-6 (AC-3): resolve the requeue backoff floor — `REQUEUE_BACKOFF_MIN`,
/// overridable by `CS_REQUEUE_BACKOFF_MS` (whole ms; the per-machine escape
/// hatch). Never below 1µs (a zero floor would busy-spin).
fn requeue_backoff_min() -> Duration {
    std::env::var("CS_REQUEUE_BACKOFF_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis)
        .filter(|d| !d.is_zero())
        .unwrap_or(REQUEUE_BACKOFF_MIN)
        .max(Duration::from_micros(1))
}

/// SLW-RI-5: shared bounded fan-out width resolver — `min(available cores,
/// cap)`, overridable by `env_var` (a per-disk escape hatch, mirroring
/// `CS_FSYNC_PARALLELISM`). Never exceeds `n_units`, never drops below 1.
fn fanout_parallelism(n_units: usize, cap: usize, env_var: &str) -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let width = std::env::var(env_var)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or_else(|| cores.min(cap));
    width.min(n_units).max(1)
}

/// SLW-RI-5 (AC-4): resolve the content-fan-out width — `min(available cores,
/// CONTENT_FANOUT_CAP)`, overridable by `CS_MATERIALIZE_PARALLELISM`.
fn content_fanout_parallelism(n_units: usize) -> usize {
    fanout_parallelism(n_units, CONTENT_FANOUT_CAP, "CS_MATERIALIZE_PARALLELISM")
}

/// SLW-RI-5 (AC-4/AC-6): the effective fan-out width for a wave, gated by
/// direction. Shared-source reasons (`writes_shared_source()`; SW-XS-8: none
/// remain) resolve to **1** so RI-4's
/// per-org source serialization is preserved by construction (a future
/// shared-source reason added to the loop can't accidentally fan out). Forward
/// multi-repo reasons (`MigrationCutover`) get the bounded content width. Single-repo waves collapse to 1 via the `n_units`
/// clamp regardless.
fn fanout_width_for_reason(reason: &WaveReason, n_units: usize) -> usize {
    if reason.writes_shared_source() {
        1
    } else {
        content_fanout_parallelism(n_units)
    }
}

impl Wave {
    /// Build a fresh wave with a freshly-allocated cancellation token.
    pub fn new(reason: WaveReason, ctx: ResolverContext, affected: Vec<AttachedCodeRepo>) -> Self {
        Self {
            reason,
            ctx,
            affected,
            cancel: CancellationToken::new(),
            initiator: crate::install::worktree_index::SyncInitiator::Auto,
            direction: crate::install::story_linker::SyncMode::Forward,
        }
    }

    /// SW-XS-4: set which way a wave may copy (see [`Wave::direction`]).
    pub fn with_direction(mut self, direction: crate::install::story_linker::SyncMode) -> Self {
        self.direction = direction;
        self
    }

    /// RF-perf-24 (AC-5): attribute this wave's reverse promote to `initiator`.
    /// A cheap consuming setter, so an on-demand caller can tag provenance without
    /// widening [`Wave::new`]'s signature across its call sites.
    pub fn with_initiator(
        mut self,
        initiator: crate::install::worktree_index::SyncInitiator,
    ) -> Self {
        self.initiator = initiator;
        self
    }

    /// Fire the wave's cancellation token. Cooperative — in-flight
    /// `spawn_blocking` materializations finish; the per-name loop
    /// checks the token between calls and bails early.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}

/// Per-code-repo materialization tally. Sums populate the
/// `materialize_counts` field of `sync-wave-completed` (AC-9), with the
/// frontend's `skipped` collapsing both Skipped* variants.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct RepoWaveCounts {
    pub written: usize,
    pub skipped_already_current: usize,
    pub skipped_locally_modified: usize,
    pub unmaterialized: usize,
    pub no_op: usize,
    /// RF-perf-24 (AC-5): dest→content-layer edits promoted back this pass (SW-XS-8:
    /// no wave does any more). Internal-only (`#[serde(skip)]` — never in the
    /// `sync-wave-completed` payload); the single signal the completion stamp reads
    /// to know an on-demand flush actually reverse-promoted.
    #[serde(skip)]
    pub reverse_written: usize,
}

impl RepoWaveCounts {}

/// SS-FU-19 AC-1: per-target stage Pareto, surfaced out of the forward
/// reconcile wave so the cascade harness can aggregate a multi-target
/// breakdown without scraping `info!` logs. Post-install-retirement the
/// forward wave only records itself against the store — the materialize fan-out
/// buckets (resolve/read/hash/write/fsync/enumerate/classify/git-exclude and the
/// name/written counts) had no producer left, so the profile carries just the
/// store-persist, lock-wait, and total spans; `other_ns` is the derived remainder (name resolution,
/// suppression bookkeeping, tokio handoff). Deliberately **not** serialized
/// into the `sync-wave-completed` IPC payload — it rides on `WaveResult`
/// behind `#[serde(skip)]` and is consumed in-process only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoStageProfile {
    pub total_ns: u128,
    /// Store persist (`upsert`) at wave end.
    pub lockfile_persist_ns: u128,
    /// Wait to acquire the per-repo wave lock (`coord.acquire_repo_lock`).
    /// Measures contention; does NOT change locking.
    pub lock_wait_ns: u128,
    /// Unbucketed remainder — DERIVED, never assigned directly. See
    /// [`RepoStageProfile::compute_other_ns`]. Should trend → 0.
    pub other_ns: u128,
}

impl RepoStageProfile {
    /// Sum of every *named* nanosecond bucket. Single source of truth so a
    /// future bucket added to the struct but forgotten in the `other`
    /// derivation is caught by `named_ns_covers_every_bucket` (below) rather
    /// than silently re-inflating `other`. (SR-0 refactor: `other` is a
    /// computed drift-alarm, not a hand-maintained remainder.)
    pub fn named_ns(&self) -> u128 {
        [self.lockfile_persist_ns, self.lock_wait_ns]
            .into_iter()
            .fold(0u128, u128::saturating_add)
    }

    /// `total - named`, saturating. The remainder after every named bucket:
    /// suppression bookkeeping + tokio handoff/join + trace overhead. Assign
    /// `other_ns` from this exactly once, at profile construction.
    pub fn compute_other_ns(&self) -> u128 {
        self.total_ns.saturating_sub(self.named_ns())
    }
}

#[cfg(test)]
mod sr0_stage_profile_tests {
    use super::{RepoStageProfile, WaveTrigger};

    /// SR-0 (Q1): the `wave enqueued` / stage-profile tokens are a wire
    /// contract with an external scale-soak parser. They must be stable,
    /// non-empty, and contain no whitespace (the parser splits fields on it).
    #[test]
    fn wave_trigger_tokens_are_parser_safe() {
        {
            let s = WaveTrigger::Unspecified.as_str();
            assert!(!s.is_empty(), "trigger token must be non-empty");
            assert!(
                !s.chars().any(char::is_whitespace),
                "trigger token {s:?} must not contain whitespace"
            );
        }
        // Spot-check a couple of exact tokens (rename = breakage the parser sees).
        assert_eq!(WaveTrigger::Unspecified.as_str(), "unspecified");
    }

    /// The count of *named* nanosecond buckets on `RepoStageProfile` (every
    /// field except `total_ns`/`other_ns`). If a new bucket is added to the
    /// struct, bump this AND add it to `named_ns()` — the
    /// `named_ns_covers_every_bucket` test fails loudly if you forget.
    const NAMED_BUCKET_COUNT: u128 = 2;

    /// SR-0 drift alarm: `named_ns()` must sum EVERY named bucket. Setting
    /// each to 1 means the sum equals the bucket count — a new field left out
    /// of `named_ns()` makes this fail, so `other` can't silently re-inflate.
    #[test]
    fn named_ns_covers_every_bucket() {
        let p = RepoStageProfile {
            total_ns: 0,
            lockfile_persist_ns: 1,
            lock_wait_ns: 1,
            other_ns: 0,
        };
        assert_eq!(
            p.named_ns(),
            NAMED_BUCKET_COUNT,
            "named_ns() must sum every named bucket exactly once — a mismatch \
             means a bucket was added to the struct but not to named_ns()"
        );
    }

    /// `other` is the derived remainder `total - named`, saturating to 0 when
    /// the named buckets exceed `total` (clock skew across sub-Instants).
    #[test]
    fn compute_other_ns_is_saturating_remainder() {
        let mut p = RepoStageProfile {
            total_ns: 100,
            lock_wait_ns: 30,
            lockfile_persist_ns: 20,
            ..Default::default()
        };
        assert_eq!(p.compute_other_ns(), 50, "100 - (30+20) = 50");

        p.other_ns = p.compute_other_ns();
        assert_eq!(p.other_ns, 50);

        // Named exceeds total → saturate to 0, never underflow-panic.
        p.lock_wait_ns = 1_000;
        assert_eq!(p.compute_other_ns(), 0, "named > total saturates to 0");
    }
}

/// Per-error surface in `WaveResult.errors`. `logical_name` is `None`
/// for lockfile-level errors (read fail / write fail / path math fail);
/// `Some` for per-name materialize errors.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WaveError {
    pub code_repo_path: PathBuf,
    pub logical_name: Option<String>,
    pub message: String,
}

/// Return value of [`Wave::run`]. Tally + errors + duration + cancel
/// flag. Serialized into `sync-wave-completed` payload (AC-9).
///
/// Iter-2 (Blind H2): `duration_ms` is `u64`, not `u128`. JavaScript
/// `Number` safely handles integers up to `2^53 - 1` (≈ 9 PB ms — well
/// over a year), which covers every realistic wave duration. `u128`
/// trips serde-json's `arbitrary_precision` requirement and is
/// fragile across the Tauri IPC bridge.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct WaveResult {
    pub per_repo: HashMap<PathBuf, RepoWaveCounts>,
    pub errors: Vec<WaveError>,
    pub duration_ms: u64,
    pub cancelled: bool,
    /// SW-XS-4: files the explicit reconcile did NOT publish because they changed
    /// on both sides since the last sync (never written, either side). Reported on
    /// this operation's result and its `sync-wave-completed` payload — there is no
    /// background conflict event or registry. Kept apart from `errors` so a
    /// conflict does not mark the operation failed for the files that did publish.
    pub conflicts: Vec<WaveError>,
    /// SS-FU-19 AC-1: one entry per materialized target (success path
    /// only — error-aborted targets contribute none). In-process profiling
    /// channel; `#[serde(skip)]` keeps it off the Tauri IPC bridge so the
    /// `sync-wave-completed` payload is unchanged.
    #[serde(skip)]
    pub stage_profiles: Vec<RepoStageProfile>,
}

/// Production deps injected at [`Wave::run`] call time. Decouples the
/// wave engine from `tauri::AppHandle` / `WatcherState` so tests and
/// benches can drive the wave without standing up a Tauri runtime
/// (D-17). Cheap-clone (the inner `Arc<dyn Fn>` + `SuppressionHandle`
/// are both clone-friendly).
#[derive(Clone)]
pub struct WaveDeps {
    pub suppression: super::super::SuppressionHandle,
    /// Topic + JSON payload emit closure. Production wires through
    /// `app.emit(topic, payload)`; tests pass a recording closure; the
    /// criterion bench passes a no-op.
    pub emit: Arc<dyn Fn(&'static str, serde_json::Value) + Send + Sync>,
    /// SW-CP-2 (D8): FrontmatterCache handle, required by the projection
    /// path (`run_repo_projection` → `materialize_manifest_projections`).
    /// `Some` in production (`from_app` pulls the managed cache); `None`
    /// for install-only waves and tests that never enqueue a
    /// `ContentLayerEvent`.
    pub cache: Option<SharedFrontmatterCache>,
    /// SW-CP-2 (D8): whether this wave may run the projection path. `true`
    /// only for content waves; `false` for install waves. (SW-XS-14: this used
    /// to carry the per-repo staleness config, retired with the setting.)
    pub projection_enabled: bool,
}

impl WaveDeps {
    /// Production constructor. Pulls the dispatcher's suppression
    /// handle from the Tauri-managed [`WatcherState`] and binds
    /// `app.emit`. Returns [`Error::WatcherStateNotRegistered`] if
    /// `WatcherState` isn't yet registered (precondition gate — should
    /// never fire in production because the Tauri setup registers
    /// `WatcherState` before any command can fire).
    pub fn from_app<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Result<Self, Error> {
        let watcher_state = app
            .try_state::<WatcherState>()
            .ok_or(Error::WatcherStateNotRegistered)?;
        let suppression = watcher_state.dispatcher.suppression_handle();
        let app_for_emit = app.clone();
        let emit: Arc<dyn Fn(&'static str, serde_json::Value) + Send + Sync> =
            Arc::new(move |topic, payload| {
                let _ = app_for_emit.emit(topic, payload);
            });
        // SW-CP-2 (D8): the FrontmatterCache is harmless for install waves
        // (they never read it); content waves require it. `projection_staleness`
        // is per-repo and only known at the content watcher, so `from_app`
        // leaves it `None` — the content handler builds its own `WaveDeps`.
        let cache = app
            .try_state::<SharedFrontmatterCache>()
            .map(|s| s.inner().clone());
        Ok(Self {
            suppression,
            emit,
            cache,
            projection_enabled: false,
        })
    }
}

// =============================================================================
// AC-4 / AC-5 / AC-6 / D-9: WaveCoordinator
// =============================================================================

/// In-flight wave registry entry. Held in the coordinator's
/// `in_flight: Vec<InFlightWave>` so [`WaveCoordinator::enqueue`] can
/// walk for supersede + cancel.
///
/// Iter-2 (Edge HIGH-2): `wave_id` is a process-monotonic unique key
/// so the spawned task's [`InFlightGuard`] removes its own entry
/// (not a sibling entry that happens to share a `reason` value).
///
/// SW-WL-2: `affected` carries the normalized target `code_repo_path`s
/// this wave writes into, so the wave-in-flight gate
/// ([`WaveCoordinator::detach_now_or_defer`]) can answer "is a wave
/// writing to path P?" off this same registry — no parallel counter.
/// Paths are normalized via [`WaveCoordinator::normalize_target_path`]
/// at registration so they compare equal to the watcher's
/// (independently normalized) `repo_path`.
#[derive(Clone)]
pub struct InFlightWave {
    pub context: Option<(PathBuf, String, String)>,
    pub wave_id: u64,
    pub reason: WaveReason,
    /// SR-0 (Q1): the enqueue-site trigger, carried beside `reason` so the
    /// gated `wave enqueued` line and any in-flight introspection can name it.
    pub trigger: WaveTrigger,
    pub cancel: CancellationToken,
    pub affected: Vec<PathBuf>,
}

/// Process-wide wave coordinator (D-9). Registered as Tauri-managed
/// state — `app.state::<Arc<WaveCoordinator>>()` from Tauri commands;
/// dispatcher handlers capture `Arc<WaveCoordinator>` by `move` at
/// build time.
///
/// **Sync/async lock split** (D-9): the keyed `wave_target_locks` (inner) and
/// `repo_locks` (inner) are tokio (held across awaits inside `Wave::run`);
/// `in_flight` and the two lock maps' outer guards are `std::sync::Mutex`
/// because their critical sections are short and don't await — letting
/// dispatcher handler closures call [`enqueue`] from sync context without
/// wrapping every call in `tokio::spawn`.
pub struct WaveCoordinator {
    /// SLW-RI-4: per-target keyed wave lock. Replaces the whole-engine
    /// `global_lock`: instead of one mutex serializing every wave across all
    /// attached worktrees, a wave acquires (in `enqueue`'s spawn body) the
    /// per-key `TokioMutex` for each normalized `code_repo_path` in its
    /// `affected` set — in canonical sorted order (deadlock-free) — and holds
    /// them across `wave.run()`. Disjoint targets acquire different mutexes and
    /// run concurrently on the engine runtime; same-target waves still serialize
    /// on one mutex, preserving supersede + the per-surface `Syncing→Synced`
    /// status-write ordering (and RS-2's reverse↔forward echo ordering, which
    /// holds because the forward fan's write-target set and its lock-key set are
    /// both the same `affected` list — see [`acquire_wave_target_locks`]). Same
    /// registry shape as `repo_locks` but a SEPARATE map: `repo_locks` is
    /// acquired per-repo *inside* `wave.run()`, so reusing one map would
    /// re-enter and self-deadlock. Shared FrontmatterCache state is no longer
    /// lock-guarded (SLW-RI-3): reads take an immutable snapshot, writes serialize
    /// per-source through `FrontmatterCache::rcu`.
    wave_target_locks: StdMutex<HashMap<PathBuf, Arc<TokioMutex<()>>>>,

    /// SLW-RI-4 (AC-7): bounds the number of concurrent `wave.run` bodies to the
    /// engine pool ([`engine_worker_count`]). Acquired (permit-first, before the
    /// per-key locks) in `enqueue`'s spawn body so a wave-storm — a 40-worktree
    /// launch — can't spawn 40 simultaneous wave bodies whose `spawn_blocking`
    /// offloads would swamp the shared blocking pool. `Arc` so a spawned task can
    /// hold an owned permit for its whole life.
    engine_semaphore: Arc<Semaphore>,

    /// Supersede registry.
    in_flight: StdMutex<Vec<InFlightWave>>,

    /// SW-XS-3: every wave ever enqueued on this coordinator. Read by the
    /// removal tests to prove a stimulus (watcher event, attach, story move,
    /// terminal going live) enqueues nothing — an observable that fails if a
    /// forward trigger comes back.
    enqueued: std::sync::atomic::AtomicU64,

    /// SW-XS-3: attempts at any entry point. SW-XS-8: the last one (the legacy
    /// agent reconcile) is gone, so nothing counts here; removal tests assert 0.
    entries: std::sync::atomic::AtomicU64,

    /// Per-code-repo lockfile lock map. Outer std::sync::Mutex because
    /// map insert/clone is a sync critical section (no awaits inside
    /// the outer guard). Inner per-repo Mutex<()> is tokio because the
    /// lock IS held across `Wave::run`'s per-repo `spawn_blocking`
    /// awaits. Distinct from `wave_target_locks` (which the whole wave
    /// holds): this is the finer-grained lockfile-mutation lock taken
    /// per-repo inside the run loop.
    repo_locks: StdMutex<HashMap<PathBuf, Arc<TokioMutex<()>>>>,
    remote_destination_locks:
        StdMutex<HashMap<RemoteDestinationLockKey, std::sync::Weak<TokioMutex<()>>>>,

    /// SW-WL-2: wave-in-flight gate. Maps a normalized target
    /// `code_repo_path` → (`context_repo_path` → deferred detach) the close
    /// path registered because a wave was writing into that target at close
    /// time. The last wave to drain that target fires + removes ALL of the
    /// path's detaches (see [`InFlightGuard::drop`]). Empty in steady state.
    ///
    /// **Why the inner map keyed by `context_repo_path`:** the watcher
    /// registry (`path_to_repo`) allows MULTIPLE subscribers at one
    /// `repo_path` differing only in `context_repo_path` (SW-WL-1's
    /// composite identity). Two such subscribers can both close while one
    /// wave writes into their shared path; a flat `PathBuf → DetachFn` map
    /// would let the second defer silently overwrite the first, leaking the
    /// first subscriber's watcher. Keying the inner map by `context_repo_path`
    /// gives each subscriber its own slot — and lets `cancel_pending_detach`
    /// (re-attach) cancel ONE subscriber's detach without touching its
    /// siblings. The drain side is path-scoped (fires every subscriber at a
    /// drained path), matching how a wave makes the watcher load-bearing for
    /// the whole path regardless of context.
    ///
    /// **Lock order (load-bearing, SW-WL-2 AC-10):** `in_flight` →
    /// `pending_detach` → (the watcher's `watcher_lock`, acquired *inside*
    /// the fired [`DetachFn`]). Every mutation of `pending_detach` and
    /// every firing of a [`DetachFn`] happens while `in_flight` is held,
    /// so the drain check and the detach are atomic w.r.t. a concurrent
    /// [`enqueue`] (AC-7). Never acquire these in the reverse order.
    pending_detach: StdMutex<HashMap<PathBuf, HashMap<String, DetachFn>>>,

    /// SW-CP-2 (D9): the active team's target — its `ResolverContext` plus the
    /// team's `source_repos` — stashed by
    /// [`stash_active_projection_on_team_change`](WaveCoordinator::stash_active_projection_on_team_change).
    /// SW-XS-3/SW-XS-4: no automatic wave reads it any more; the agent reconcile
    /// and the read-only publish preview resolve the active team's attached repos
    /// from it. `None` until the first team is active.
    active_projection_target: StdMutex<Option<ProjectionTarget>>,

    /// SLW-RI-1: a [`Handle`](tokio::runtime::Handle) to the dedicated
    /// content/sync **engine runtime**. Injected once at app setup
    /// ([`set_engine_handle`]) so
    /// [`enqueue`](Self::enqueue)'s `wave.run` spawn runs on that runtime's
    /// workers instead of Tauri's command-dispatch runtime — heavy engine work
    /// (a wave that blocks a worker) can then never starve command dispatch
    /// (stuck terminals / lost input). `None` in tests / benches / before setup
    /// → `enqueue` falls back to `tauri::async_runtime::spawn` (today's
    /// behavior), so the injection is additive, never a test regression.
    ///
    /// **A `Handle`, deliberately NOT `Arc<Runtime>`:** the runtime is *owned*
    /// by [`EngineRuntimeGuard`] in app state, not here. A wave task captures
    /// `Arc<WaveCoordinator>`, so if the coordinator owned the runtime, a wave
    /// finishing on an engine worker could drop the runtime from inside its own
    /// worker thread (tokio panic). A `Handle` does not keep the runtime alive,
    /// so that ownership cycle can't form. Interior mutability + injection seam
    /// keep the coordinator runtime-free for tests (D-17).
    engine_handle: StdMutex<Option<tokio::runtime::Handle>>,
}

/// SW-CP-2: the active team's content-projection fan-out target. Captured
/// at team-activation time so a content edit re-materializes every attached
/// code repo for the team without re-deriving the team/slugs from the
/// triggering filesystem path (which the flat PTS-2 layout doesn't carry).
#[derive(Clone)]
struct ProjectionTarget {
    ctx: ResolverContext,
    source_repos: Vec<SourceRepoSlug>,
}

/// SW-WL-2: a detach the close path deferred until in-flight waves drain.
/// Boxed closure rather than stored data so the watcher-side detach logic
/// stays in `watch.rs` (the closure captures the `AppHandle` + `WatchConfig`
/// and replays `remove_watch_inner`) — the wave engine invokes the
/// capability without naming the watcher, mirroring the `WaveDeps.emit`
/// dependency-injection seam (D-17). Tests inject a recording closure.
pub type DetachFn = Arc<dyn Fn() + Send + Sync>;

/// SLW-RI-1: hard ceiling on engine worker threads. Guards the
/// `CS_ENGINE_WORKERS` override against an absurd value (e.g. `100000`) that
/// would make `tokio::runtime::Builder::build()` eagerly spawn that many OS
/// threads → startup panic / resource blowup.
const ENGINE_WORKERS_CEILING: usize = 64;

/// SLW-RI-1 (AC-1): build the dedicated content/sync **engine runtime** — a
/// bounded, named multi-thread Tokio runtime that [`WaveCoordinator::enqueue`]
/// runs `wave.run` on, isolated from Tauri's command-dispatch runtime so engine
/// work can never starve command dispatch (the SLW-FX-7 freeze class).
///
/// Bounded worker pool: `min(cores, 4)`, clamped to `[2, ENGINE_WORKERS_CEILING]`
/// — SLW-RI-4 keyed the wave lock per-target, so disjoint worktrees now run
/// `wave.run` in parallel across these workers (the `engine_semaphore`, sized to
/// this same count, is the concurrency ceiling); the workers also serve each
/// wave's `spawn_blocking` offloads (FX-7/CO-4) + FX-5's `thread::scope` fan-out.
/// The floor of 2 keeps a wave that awaits engine progress
/// from deadlocking a single-worker pool, and the ceiling caps a malformed
/// `CS_ENGINE_WORKERS` override. Returned **by value** so the caller can hand a
/// [`Handle`](tokio::runtime::Handle) to the coordinator while a dedicated
/// [`EngineRuntimeGuard`] owns the runtime for a clean, bounded shutdown.
/// Built once at app setup; the handle is injected via
/// [`WaveCoordinator::set_engine_handle`].
pub fn build_engine_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(engine_worker_count())
        .thread_name("cs-engine")
        .enable_all()
        .build()
}

/// SLW-RI-4 (AC-7): the bounded engine worker count — `min(cores, 4)` clamped to
/// `[2, ENGINE_WORKERS_CEILING]`, honoring the `CS_ENGINE_WORKERS` override.
/// Extracted from [`build_engine_runtime`] so the coordinator's wave-concurrency
/// [`Semaphore`](tokio::sync::Semaphore) is sized to the SAME pool the waves run
/// on: keyed locking lets disjoint waves run in parallel, and this cap keeps a
/// wave-storm (e.g. a 40-worktree launch) from spawning more concurrent
/// `wave.run` bodies than the engine can serve — the wave body's heavy work
/// offloads to the shared `spawn_blocking` pool, which the worker count alone
/// would NOT bound, so the semaphore is the real ceiling.
pub fn engine_worker_count() -> usize {
    std::env::var("CS_ENGINE_WORKERS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .min(4)
        })
        .clamp(2, ENGINE_WORKERS_CEILING)
}

/// SLW-RI-1 (AC-6): sole owner of the engine [`Runtime`](tokio::runtime::Runtime),
/// held in Tauri managed state — **never** on the `WaveCoordinator` and never
/// captured by a wave task. Two purposes:
///
/// 1. **Keeps the runtime alive (load-bearing).** *Something* must own the
///    `Runtime` for the app's lifetime, since the coordinator holds only a
///    [`Handle`](tokio::runtime::Handle) (which does not keep it alive). This
///    guard, in app state, is that owner.
/// 2. **Breaks the self-drop cycle (the real invariant).** A wave task captures
///    `Arc<WaveCoordinator>` — which holds only a `Handle` — so a wave finishing
///    on an engine worker thread can never be the runtime's last owner. The
///    runtime is therefore never dropped from inside its own worker thread
///    (which would panic: "Cannot drop a runtime in a context where blocking is
///    not allowed"). This holds regardless of *when* or *whether* this guard is
///    dropped — it is a property of the ownership graph, not of shutdown timing.
///
/// **On `Drop`:** if managed state is torn down (the guard drops), it shuts the
/// runtime down via [`Runtime::shutdown_background`] — non-blocking,
/// non-panicking, safe from any context — so a graceful teardown can't hang on a
/// heavy in-flight `spawn_blocking` (materialize / stalled fsync). NOTE: on a
/// normal macOS/Windows quit, tao's event loop calls `process::exit()` and
/// managed-state destructors do **not** run, so this `Drop` is a correctness
/// safety-net for teardown paths that *do* drop state (tests, `run_return`,
/// future graceful-shutdown), not the primary exit path. The panic-avoidance in
/// (2) is what actually ships; `process::exit` makes any exit trivially bounded.
pub struct EngineRuntimeGuard(Option<tokio::runtime::Runtime>);

impl EngineRuntimeGuard {
    pub fn new(rt: tokio::runtime::Runtime) -> Self {
        Self(Some(rt))
    }
}

impl Drop for EngineRuntimeGuard {
    fn drop(&mut self) {
        if let Some(rt) = self.0.take() {
            // Non-blocking, non-panicking — safe from any context, bounded exit.
            rt.shutdown_background();
        }
    }
}

impl Default for WaveCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl WaveCoordinator {
    /// RF-perf-5 (AC-3): `len()` snapshots of the three per-`code_repo_path`,
    /// insert-only coordinator maps for the gated memory sampler. Each is keyed by
    /// repo path with no eviction (RF-perf-6's target), so their lengths track a
    /// slow monotonic climb across a many-worktree session. `try_lock`-or-skip
    /// (`None` on contention) so the sampler never blocks a wave hot path.
    pub(crate) fn wave_target_locks_len(&self) -> Option<usize> {
        self.wave_target_locks.try_lock().ok().map(|m| m.len())
    }

    pub(crate) fn repo_locks_len(&self) -> Option<usize> {
        self.repo_locks.try_lock().ok().map(|m| m.len())
    }

    /// RF-perf-6 (AC-2): evict a torn-down worktree's per-repo coordinator entries
    /// so backend RSS tracks the LIVE working set instead of every repo ever seen.
    /// Drops `code_repo_path` from the three insert-only per-`code_repo_path` maps
    /// (`wave_target_locks`, `repo_locks`). These are the RF-perf-5 memory-lens counters that otherwise climb
    /// monotonically across a many-worktree session.
    ///
    /// Called from the worktree-removal teardown TAIL — AFTER the git prune has
    /// released this repo's `repo_locks` guard (see `remove_worktree`). Placing it there (not inside `flush_and_drop_lockfile`) is
    /// load-bearing: `prune_worktree_locked` re-acquires `repo_locks[code_repo_path]`
    /// across the git prune, so an earlier eviction would just be re-inserted. The
    /// (separate) reverse-sweep prune keeps its own established site.
    ///
    /// A lock entry is evicted ONLY when quiescent (`Arc::strong_count == 1` — the
    /// map holds the only reference). If any in-flight wave still holds the repo's
    /// lock it is left in place and reclaimed on a later teardown, so a concurrent
    /// wave is never robbed of its lock (R1). Re-creation is automatic and cheap: a
    /// re-attached worktree re-inserts on next use, so live-repo semantics are
    /// unchanged (AC-3).
    ///
    /// Sync + non-blocking: every map is an `StdMutex` with a short `retain`/`remove`
    /// critical section — no `.await`, no I/O — safe on the teardown path without
    /// stalling a command handler (R6).
    pub(crate) fn prune_repo(&self, code_repo_path: &Path) {
        Self::prune_lock_map(&self.wave_target_locks, code_repo_path);
        Self::prune_lock_map(&self.repo_locks, code_repo_path);
    }

    /// Build a fresh coordinator. Cheap; in production this is called
    /// once at app setup.
    pub fn new() -> Self {
        Self {
            wave_target_locks: StdMutex::new(HashMap::new()),
            engine_semaphore: Arc::new(Semaphore::new(engine_worker_count())),
            in_flight: StdMutex::new(Vec::new()),
            enqueued: std::sync::atomic::AtomicU64::new(0),
            entries: std::sync::atomic::AtomicU64::new(0),
            repo_locks: StdMutex::new(HashMap::new()),
            remote_destination_locks: StdMutex::new(HashMap::new()),
            pending_detach: StdMutex::new(HashMap::new()),
            active_projection_target: StdMutex::new(None),
            engine_handle: StdMutex::new(None),
        }
    }

    /// SLW-RI-1 (AC-1): inject a handle to the dedicated engine runtime. Called
    /// once at app setup with the handle of the
    /// `cs-engine` runtime built by [`build_engine_runtime`] — whose ownership
    /// lives in [`EngineRuntimeGuard`], not here (see the `engine_handle` field).
    /// Idempotent-overwrite; never unset in production. Tests leave it unset, so
    /// [`enqueue`](Self::enqueue) uses Tauri's runtime — byte-for-byte today's
    /// behavior (AC-7e).
    pub fn set_engine_handle(&self, handle: tokio::runtime::Handle) {
        *self.engine_handle.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
    }

    /// SLW-RI-1 (AC-2): the runtime handle [`enqueue`](Self::enqueue) spawns
    /// `wave.run` onto. `Some` once [`set_engine_handle`] has injected the engine
    /// runtime's handle (production); `None` before setup / in tests. Returns a
    /// `tauri::async_runtime::RuntimeHandle` so both branches of `enqueue`'s
    /// spawn yield the same `JoinHandle` type, and both work from the notify
    /// watcher thread: a `RuntimeHandle` carries its own runtime context
    /// (`.spawn` does `Handle::enter()` + `tokio::spawn`), so no ambient runtime
    /// is required — the same cross-thread guarantee `async_runtime::spawn` gave
    /// (AC-4). Cloning the `Handle` is cheap; the read lock is brief.
    fn engine_spawn_handle(&self) -> Option<tauri::async_runtime::RuntimeHandle> {
        self.engine_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|h| tauri::async_runtime::RuntimeHandle::Tokio(h.clone()))
    }

    /// SW-CP-34 (S2) / SW-CP-2 (D9): the team-activation seam the
    /// `set_active_team_for_wave` command calls on every team change. Stashes the
    /// active team's target (ctx + source_repos). SW-XS-3: a stash only — team
    /// activation never enqueues a wave.
    pub(crate) fn stash_active_projection_on_team_change(
        self: &Arc<Self>,
        ctx: &ResolverContext,
        source_repos: &[SourceRepoSlug],
    ) {
        self.stash_active_projection_target(ProjectionTarget {
            ctx: ctx.clone(),
            source_repos: source_repos.to_vec(),
        });
    }

    /// Retire only this principal's context, using the existing cancellation.
    pub(crate) fn detach_principal(&self, principal: &str) {
        let mut slot = self
            .active_projection_target
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if slot
            .as_ref()
            .is_some_and(|t| t.ctx.principal().as_str() == principal)
        {
            *slot = None;
        }
        drop(slot);
        for wave in self
            .in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
        {
            if wave
                .context
                .as_ref()
                .is_some_and(|(_, p, _)| p == principal)
            {
                wave.cancel.cancel();
            }
        }
    }

    pub(crate) fn detach_context(&self, home: &Path, principal: &str, slug: &str) {
        let mut slot = self
            .active_projection_target
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if slot.as_ref().is_some_and(|t| {
            t.ctx.codesense_home == home
                && t.ctx.principal().as_str() == principal
                && t.ctx.org_slug == slug
        }) {
            *slot = None;
        }
        drop(slot);
        for wave in self
            .in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
        {
            if wave
                .context
                .as_ref()
                .is_some_and(|(h, p, s)| h == home && p == principal && s == slug)
            {
                wave.cancel.cancel();
            }
        }
    }

    /// SW-CP-2 (D9): stash `target` into `active_projection_target`.
    fn stash_active_projection_target(&self, target: ProjectionTarget) {
        *self.active_projection_target.lock().expect("poisoned") = Some(target);
    }

    /// SW-XS-3: how many entry-point attempts this coordinator has seen.
    pub fn entry_attempts(&self) -> u64 {
        self.entries.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// SW-XS-3: how many waves have been enqueued on this coordinator.
    pub fn enqueued_count(&self) -> u64 {
        self.enqueued.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// SW-XS-3: is any wave in flight right now?
    pub fn has_wave_in_flight(&self) -> bool {
        !self
            .in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }

    /// SW-WL-2: normalize a target `code_repo_path` for gate identity.
    /// Both the wave engine (storing [`InFlightWave::affected`]) and the
    /// watcher (deriving the gate key from `WatchConfig.repo_path`) route
    /// through this ONE helper, so a symlinked / `/private`-prefixed /
    /// trailing-slash form on one side still matches the other (AC-2 /
    /// AC-8 / W8). Mirrors the watcher's own `canonicalize`-with-raw-
    /// fallback idiom (`watcher.rs`): a path that doesn't resolve falls
    /// back to its raw form, so two raw-equal inputs still match.
    pub fn normalize_target_path(path: &Path) -> PathBuf {
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    }

    /// SW-WL-2: is any in-flight wave writing into `target_norm`? Both the
    /// stored `affected` paths (normalized once at [`enqueue`]) and the
    /// query `target_norm` are in normalized form, so the compare is a
    /// plain `==`. Normalizing at enqueue (rather than at compare time)
    /// keeps the gate key STABLE across the wave's lifetime — a target
    /// deleted/recreated between defer and drain can't shift its key and
    /// strand a deferred detach. Caller holds the `in_flight` lock (passes
    /// the locked slice) so the check + any dependent mutation stay atomic
    /// (AC-7).
    fn target_in_flight(in_flight: &[InFlightWave], target_norm: &Path) -> bool {
        in_flight
            .iter()
            .any(|w| w.affected.iter().any(|p| p == target_norm))
    }

    /// SW-WL-2 (AC-2 / AC-3 / AC-7): the wave-in-flight gate. Given a
    /// target `code_repo_path` and the detach to perform, either run the
    /// detach NOW (no wave in flight for the target) or DEFER it (a wave
    /// is writing — store it so the last wave to drain fires it).
    ///
    /// Both branches run under the `in_flight` lock so a concurrent
    /// [`enqueue`] for the same target cannot interleave between the
    /// "in flight?" check and the detach/defer decision (AC-7). The
    /// immediate detach therefore also holds `in_flight` across the
    /// watcher's `watcher_lock` — the documented `in_flight` →
    /// `watcher_lock` order (AC-10). The critical section is a single
    /// config's teardown (a handful of kernel `unwatch` calls); the
    /// `enqueue` supersede walk it briefly contends with is itself short.
    ///
    /// `context` is the subscriber's `context_repo_path` — the inner
    /// `pending_detach` key, so two subscribers sharing `target` but
    /// differing in context each get their own slot (no overwrite).
    pub fn detach_now_or_defer(&self, target: &Path, context: &str, detach: DetachFn) {
        let in_flight = self.in_flight.lock().expect("in_flight poisoned");
        // Common case — nothing running — detaches now without a
        // `canonicalize` (no targets to compare against).
        if in_flight.is_empty() {
            detach();
            return;
        }
        let target_norm = Self::normalize_target_path(target);
        if Self::target_in_flight(&in_flight, &target_norm) {
            self.pending_detach
                .lock()
                .expect("pending_detach poisoned")
                .entry(target_norm)
                .or_default()
                .insert(context.to_string(), detach);
            // Deferred: InFlightGuard::drop fires it when the target drains.
        } else {
            // Immediate: no wave in flight for THIS target. Run under the
            // in_flight lock so a wave enqueuing for this target right now
            // blocks until the detach completes (the supersede-walk
            // register also takes in_flight), closing the check→detach
            // race symmetrically.
            detach();
        }
    }

    /// SW-WL-2 (AC-9): re-attach cancels a pending detach. When
    /// `add_watch` re-attaches a target that had a deferred detach queued
    /// (close → defer → re-open before the wave drained), drop the stale
    /// detach so it can't kill the now-live watcher. Granular: cancels only
    /// the `(target, context)` subscriber, leaving any sibling subscriber's
    /// deferred detach at the same path intact.
    ///
    /// **Acquires `in_flight` first** (then `pending_detach`) — the same
    /// order [`InFlightGuard::drop`] uses — so cancel and a concurrent
    /// drain are mutually exclusive (review iter-2). Combined with
    /// `add_watch` calling this BEFORE it re-registers, this makes the
    /// re-attach-vs-drain race deterministic: a deferred detach can only
    /// fire while its `pending_detach` entry still exists, which means no
    /// cancel has run, which (cancel-before-register) means the re-attach
    /// hasn't happened yet — so the fire always targets the pre-reopen
    /// watcher, never the freshly re-attached one. Holding `in_flight`
    /// (rather than just `pending_detach`) is what closes the window where
    /// a drain had already moved the detach into its fire list.
    pub fn cancel_pending_detach(&self, target: &Path, context: &str) {
        let _in_flight = self.in_flight.lock().expect("in_flight poisoned");
        let mut pending = self.pending_detach.lock().expect("pending_detach poisoned");
        if pending.is_empty() {
            return;
        }
        let target_norm = Self::normalize_target_path(target);
        if let Some(by_context) = pending.get_mut(&target_norm) {
            by_context.remove(context);
            if by_context.is_empty() {
                pending.remove(&target_norm);
            }
        }
    }

    /// SLW-RI-4: get-or-insert the per-key `TokioMutex` from a keyed lock
    /// registry (`repo_locks` or `wave_target_locks`). Returns the `Arc` with
    /// the outer std map guard already released, so the caller `.await`s the
    /// inner lock WITHOUT holding the map — the map never serializes on
    /// inner-lock contention. Poison-tolerant (`into_inner`): a panic while some
    /// *other* key's inner guard was held must not wedge the whole registry —
    /// which matters more now that keyed locks are genuinely contended (RI-4).
    fn keyed_lock_arc(
        map: &StdMutex<HashMap<PathBuf, Arc<TokioMutex<()>>>>,
        key: &Path,
    ) -> Arc<TokioMutex<()>> {
        let mut m = map.lock().unwrap_or_else(|e| e.into_inner());
        Arc::clone(
            m.entry(key.to_path_buf())
                .or_insert_with(|| Arc::new(TokioMutex::new(()))),
        )
    }

    /// RF-perf-6 (AC-2): the evict half of [`keyed_lock_arc`] — drop `key` from a
    /// keyed lock registry, but ONLY when the entry is quiescent
    /// (`Arc::strong_count == 1`, i.e. the map holds the only reference and no
    /// in-flight wave is holding this repo's inner `TokioMutex`). Shared by
    /// `wave_target_locks` and `repo_locks` (both minted by `keyed_lock_arc`), so
    /// the quiescence rule lives in exactly one place. A still-held lock is left in
    /// place and reclaimed on a later teardown — never robbed from a running wave
    /// (R1). Poison-tolerant recover-in-place, matching `keyed_lock_arc`.
    fn prune_lock_map(map: &StdMutex<HashMap<PathBuf, Arc<TokioMutex<()>>>>, key: &Path) {
        map.lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|k, v| k != key || Arc::strong_count(v) > 1);
    }

    /// AC-6: acquire the per-repo lockfile lock for `code_repo_path`.
    /// Creates the per-repo Mutex on first request. The outer map mutex is
    /// released (inside [`keyed_lock_arc`]) BEFORE awaiting the per-repo lock
    /// so the map doesn't serialize on inner-lock contention.
    pub async fn acquire_repo_lock(
        &self,
        code_repo_path: &Path,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        Self::keyed_lock_arc(&self.repo_locks, code_repo_path)
            .lock_owned()
            .await
    }

    pub(crate) async fn acquire_remote_destination_lock(
        &self,
        _ctx: &crate::content_engine::ResolverContext,
        _slug: &crate::domain::SourceRepoSlug,
        identity: &crate::content_engine::destination::RemoteDestinationIdentity,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        let key = identity.clone();
        let lock = {
            let mut locks = self
                .remote_destination_locks
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            locks.retain(|_, lock| lock.strong_count() > 0);
            match locks.get(&key).and_then(std::sync::Weak::upgrade) {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(TokioMutex::new(()));
                    locks.insert(key, Arc::downgrade(&lock));
                    lock
                }
            }
        };
        lock.lock_owned().await
    }

    /// SLW-RI-4 / SLW-RI-6: the canonical TOTAL order for a wave's lock key-set —
    /// sorted + deduped. The single source of the acquisition order both the
    /// blocking ([`acquire_wave_target_locks`]) and non-blocking
    /// ([`try_acquire_wave_target_locks`]) paths agree on, so two waves with
    /// overlapping key-sets can never AB-BA deadlock regardless of which path
    /// they take (AC-5). Callers pass keys ALREADY normalized via
    /// [`normalize_target_path`].
    fn canonical_lock_keys(keys: &[PathBuf]) -> Vec<PathBuf> {
        let mut sorted: Vec<PathBuf> = keys.to_vec();
        sorted.sort();
        sorted.dedup();
        sorted
    }

    /// RF-5: get-or-insert the per-key `Arc<Mutex>` from the **wave-target**
    /// registry (`wave_target_locks`). The single spelling of *which registry*
    /// the per-target wave locks live in, shared by the blocking
    /// ([`acquire_wave_target_locks`]) and non-blocking
    /// ([`try_acquire_wave_target_locks`]) paths so they can never key off
    /// different maps. A thin wrapper over [`keyed_lock_arc`]: the guard is
    /// acquired (`.lock_owned()` / `.try_lock_owned()`) at the call site, so the
    /// blocking-vs-`try_` distinction that is the whole point of the two paths
    /// stays explicit here rather than hidden behind a flag.
    fn wave_target_lock_arc(&self, key: &Path) -> Arc<TokioMutex<()>> {
        Self::keyed_lock_arc(&self.wave_target_locks, key)
    }

    /// SLW-RI-4 (AC-1 / AC-5 / AC-10): acquire the per-target wave locks for a
    /// wave's affected key-set — the replacement for the whole-engine
    /// `global_lock`. `keys` are the wave's `code_repo_path`s ALREADY normalized
    /// via [`normalize_target_path`] (the same key material the `InFlightWave`
    /// gate stores, so keying and superseding agree on target identity).
    ///
    /// Acquired in a canonical TOTAL order (sorted + deduped) so two waves with
    /// overlapping-but-different key-sets can never AB-BA deadlock (AC-5). The
    /// returned guards are held for the wave's whole life (dropped when
    /// `wave.run()` returns), exactly as `global_lock` was held.
    ///
    /// Same-key waves serialize on one mutex → preserves supersede + per-surface
    /// status-write ordering (AC-3). RS-2's reverse↔forward echo ordering (AC-10)
    /// is preserved *by construction*: a forward-fan wave's write-target set and
    /// its key-set are BOTH its `affected` list, so it writes an originator's
    /// dest **iff** the originator is in its key-set **iff** it shares the
    /// reverse-promote wave's key and serializes behind it. An empty key-set (a
    /// wave that mutates no per-target surface) acquires nothing and runs
    /// concurrently — safe because it does no per-target work, and every piece of
    /// coordinator-shared state it *could* touch (the FrontmatterCache via
    /// snapshot/RCU (SLW-RI-3); the coalesce / reverse-throttle / attached-cache
    /// maps via their own `StdMutex`) has its own finer-grained guard.
    async fn acquire_wave_target_locks(
        &self,
        keys: &[PathBuf],
    ) -> Vec<tokio::sync::OwnedMutexGuard<()>> {
        let sorted = Self::canonical_lock_keys(keys);
        // Acquire in sorted order. `keyed_lock_arc` releases the outer map guard
        // before returning the `Arc`, so the `.await` below never holds it.
        let mut guards = Vec::with_capacity(sorted.len());
        for key in &sorted {
            guards.push(self.wave_target_lock_arc(key).lock_owned().await);
        }
        guards
    }

    /// SLW-RI-6 (AC-3): the NON-BLOCKING sibling of
    /// [`acquire_wave_target_locks`]. Attempts every key in `sorted_keys`
    /// (which MUST be pre-sorted+deduped by the caller — same canonical total
    /// order, so this composes with the blocking variant's deadlock-freedom)
    /// via `try_lock_owned`. Returns `Some(guards)` **iff every key was
    /// uncontended**; on the FIRST contended key it drops any partial guards
    /// and returns `None`. Never `.await`s and never blocks — so a caller may
    /// hold an engine permit across it without risking the permit-squat window
    /// (a permit occupied by a wave *blocked* on a key). All-or-nothing: a
    /// partial key-set is never held across the caller's subsequent backoff.
    fn try_acquire_wave_target_locks(
        &self,
        sorted_keys: &[PathBuf],
    ) -> Option<Vec<tokio::sync::OwnedMutexGuard<()>>> {
        let mut guards = Vec::with_capacity(sorted_keys.len());
        for key in sorted_keys {
            match self.wave_target_lock_arc(key).try_lock_owned() {
                Ok(g) => guards.push(g),
                // Contended → drop the partial guards (here, as `guards` falls
                // out of scope) and signal the caller to release its permit +
                // requeue. Never hold a subset across the backoff.
                Err(_) => return None,
            }
        }
        Some(guards)
    }

    /// SLW-RI-6 (AC-3): acquire an engine permit AND the wave's full per-target
    /// key-set **without ever occupying a permit while blocked on a key** — the
    /// fix for the field's permit-squat throughput-collapse at N=10–20 opens
    /// (a broad-key-set wave held one of ~4 permits while parked on a narrow
    /// `WorktreeDerive`'s key, collapsing effective pool width).
    ///
    /// Discipline (spike-locked): **permit-first**, then `try_lock` all keys in
    /// canonical sorted order; on ANY contention drop the partial key-guards,
    /// **drop the permit**, back off, and retry from the top. The two invariants
    /// hold on the fast path by construction:
    /// - never hold a permit while *blocked* on a key (RI-6 — the squat is gone:
    ///   a contended key releases the permit instead of parking on it), and
    /// - never hold a key while *blocked* on a permit (RI-4's original concern —
    ///   keys are only ever `try_lock`ed while the permit is already in hand).
    ///
    /// The rare **anti-starvation escalation** (after `REQUEUE_STARVATION_THRESHOLD`
    /// failed cycles) deliberately relaxes the *second* invariant only — it holds
    /// keys while awaiting a permit — to guarantee a starved broad wave forward
    /// progress. It still never holds a permit while blocked on a key, and never
    /// holds either resource across a wait it could cycle on. See the loop body.
    ///
    /// RI-4 correctness survives: same-key mutual exclusion is unchanged
    /// (`try_lock` still yields exclusive ownership), and same-key *ordering* is
    /// causal / supersede-driven, never FIFO-lock-order (SLW-RI-6 spike §3), so
    /// the non-FIFO requeue re-orders nothing that matters. The permit-first
    /// order also keeps this deadlock-free: a requeuing wave holds NEITHER
    /// resource across its backoff `.await`, so it is a graph sink.
    async fn acquire_engine_permit_and_keys(
        self: &Arc<Self>,
        semaphore: &Arc<Semaphore>,
        lock_keys: &[PathBuf],
    ) -> (
        tokio::sync::OwnedSemaphorePermit,
        Vec<tokio::sync::OwnedMutexGuard<()>>,
    ) {
        let sorted = Self::canonical_lock_keys(lock_keys);
        let backoff_min = requeue_backoff_min();
        // The effective ceiling must never fall below the (operator-raisable)
        // floor, else `CS_REQUEUE_BACKOFF_MS` set above 8ms would be silently
        // clamped back down by the `.min()` below (review R1-MED).
        let backoff_max = REQUEUE_BACKOFF_MAX.max(backoff_min);
        let mut backoff = backoff_min;
        let mut attempts: u32 = 0;
        loop {
            // Wait for a permit holding NOTHING (a graph sink — never a key).
            let permit = Arc::clone(semaphore)
                .acquire_owned()
                .await
                .expect("engine_semaphore is never closed");
            // Permit in hand → try (never block) for every key.
            if let Some(guards) = self.try_acquire_wave_target_locks(&sorted) {
                return (permit, guards);
            }
            // A key is contended → do NOT squat the permit. Release it and the
            // partial guards, back off, retry. The key-holder is runnable (it
            // holds a permit + is making progress), so this drains fast.
            drop(permit);
            attempts = attempts.saturating_add(1);
            // Anti-starvation escalation (review R1-HIGH). Pure `try_lock`+backoff
            // cannot guarantee a broad-key-set wave ever wins against a sustained
            // stream of narrow derives on a >1-permit pool. After a threshold of
            // failed cycles, take the keys via the FAIR blocking acquire (FIFO per
            // key) while holding NO permit — the RI-6 invariant (never a permit
            // held while blocked on a key) is preserved — THEN take a permit. This
            // does hold keys while awaiting the permit (RI-4's original concern),
            // but ONLY on this rare escalation and boundedly: permits free as
            // running waves finish, and any same-key wave would serialize behind
            // this one anyway. Shared canonical sort ⇒ no AB-BA with a concurrent
            // acquirer; the wave still holds neither resource across a blocking
            // wait it could cycle on.
            if attempts >= REQUEUE_STARVATION_THRESHOLD {
                let guards = self.acquire_wave_target_locks(&sorted).await;
                let permit = Arc::clone(semaphore)
                    .acquire_owned()
                    .await
                    .expect("engine_semaphore is never closed");
                return (permit, guards);
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(backoff_max);
        }
    }

    /// AC-3 / AC-5: enqueue a wave. **Synchronous** (not `async fn`)
    /// so dispatcher handlers can call it directly. Supersede check +
    /// new-entry insert run under the `in_flight` std mutex; the actual
    /// wave runs on a task spawned onto the engine runtime, which acquires an
    /// engine-pool semaphore permit then its per-target keyed locks
    /// (SLW-RI-4) async before `wave.run()`.
    ///
    /// The spawn goes through Tauri's global runtime (not the free
    /// `tokio::spawn`) so callers on notify's watcher thread — which has no
    /// ambient Tokio runtime — don't panic. Registration stays synchronous,
    /// so a later enqueue always supersedes an earlier one for the same key.
    ///
    /// Returns a `tauri::async_runtime::JoinHandle<WaveResult>` (its
    /// `Future::Output` is `tauri::Result<WaveResult>`) so completion-aware
    /// callers can `.await` it; fire-and-forget callers drop the handle.
    ///
    /// Iter-2 patches:
    /// - **Auditor M2 / Task 3.2:** emit `sync-wave-started` from this
    ///   function (before the spawn + semaphore-permit / keyed-lock acquire) so the
    ///   UI flips to `active` immediately, even if a prior wave is
    ///   still draining.
    /// - **Edge HIGH-2:** in-flight entries are keyed by a unique
    ///   `wave_id: u64` (monotonic from `NEXT_WAVE_ID`), not by
    ///   `WaveReason` equality. Reason-equality cleanup was racy for
    ///   non-self-superseding reasons (`Startup`, distinct-name
    ///   `ContentLayerEvent`) where two in-flight entries could share a
    ///   reason value.
    pub fn enqueue(
        self: &Arc<Self>,
        wave: Wave,
        deps: WaveDeps,
    ) -> tauri::async_runtime::JoinHandle<WaveResult> {
        // SR-0 (Q1): plain `enqueue` keeps its trigger-free signature for the
        // ~25 test call sites and any path that hasn't classified its origin;
        // production `enqueue_*` methods call `enqueue_with_trigger` directly.
        self.enqueue_with_trigger(wave, deps, WaveTrigger::Unspecified)
    }

    /// SR-0 (Q1): [`enqueue`](Self::enqueue) carrying the enqueue-site
    /// [`WaveTrigger`]. Emits the gated `wave enqueued` line (histogram source)
    /// and records the trigger on the in-flight registry entry.
    pub fn enqueue_with_trigger(
        self: &Arc<Self>,
        mut wave: Wave,
        deps: WaveDeps,
        trigger: WaveTrigger,
    ) -> tauri::async_runtime::JoinHandle<WaveResult> {
        self.enqueued
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        #[cfg(test)]
        for repo in &wave.affected {
            intake_probe::record(&repo.code_repo_path);
        }
        // Automatic waves cannot target products or registered story worktrees,
        // including roots enumerated through an ordinary repository alias. When
        // admission cannot be read (the store is unavailable) the target is kept:
        // its receiver rechecks admission, writes nothing, and surfaces the store
        // failure as a `WaveError` instead of the wave silently doing nothing.
        let store = crate::content_engine::sync_state_store::SyncStateStore::open(
            crate::content_engine::sync_state_store::default_store_path(&wave.ctx.codesense_home),
        );
        let allowed = |slug: &SourceRepoSlug, root: &Path| {
            !crate::domain::is_epic_workspace_slug(slug)
                && store
                    .as_ref()
                    .ok()
                    .and_then(|store| {
                        store
                            .uses_explicit_context(
                                &crate::content_engine::context_lifecycle::local_identity(root),
                            )
                            .ok()
                    })
                    .is_none_or(|explicit| !explicit)
        };
        wave.affected
            .retain(|repo| allowed(&repo.slug, &repo.code_repo_path));
        let wave_id = NEXT_WAVE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let context_root = match crate::context_repo::path_to_org_root(
            &wave.ctx.codesense_home,
            &wave.ctx.principal(),
            &wave.ctx.org_slug,
        ) {
            Ok(root) => root,
            Err(message) => {
                let affected: Vec<_> = wave
                    .affected
                    .iter()
                    .map(|a| a.code_repo_path.clone())
                    .collect();
                let result = WaveResult {
                    errors: vec![WaveError {
                        code_repo_path: affected.first().cloned().unwrap_or_default(),
                        logical_name: None,
                        message,
                    }],
                    ..Default::default()
                };
                (deps.emit)(
                    EVENT_WAVE_COMPLETED,
                    emit_wave_completed_payload(wave_id, &wave.reason, &affected, &result),
                );
                return tauri::async_runtime::spawn(async move { result });
            }
        };
        let new_cancel_for_registry = wave.cancel.clone();

        // The wave's target `code_repo_path`s in RAW form — for the
        // `sync-wave-started` emit payload (the UI keys on the raw attach
        // path, so it must NOT see canonicalized forms).
        let affected_repos: Vec<PathBuf> = wave
            .affected
            .iter()
            .map(|a| a.code_repo_path.clone())
            .collect();

        // The same targets NORMALIZED — for the SW-WL-2 gate (the
        // `InFlightWave` registry entry + the `InFlightGuard`). Normalized
        // ONCE here, at enqueue, so the gate key is STABLE for the wave's
        // whole life: a target deleted/recreated between a deferred close
        // and the wave's drain can't shift its key and strand the detach.
        // (`canonicalize` here is off the UI thread, on the wave's own
        // task, and `affected` is small.)
        let gate_affected: Vec<PathBuf> = affected_repos
            .iter()
            .map(|p| Self::normalize_target_path(p))
            .collect();

        // Supersede walk + register (under in_flight std mutex).
        let new_reason_for_registry;
        {
            let mut in_flight = self.in_flight.lock().expect("poisoned");
            in_flight.retain(|other| {
                if wave.reason.supersedes(&other.reason) {
                    debug!(
                        target: "content_engine::sync::wave",
                        "supersede: cancelling in-flight wave_id={} reason={:?}",
                        other.wave_id,
                        other.reason
                    );
                    other.cancel.cancel();
                    false
                } else {
                    true
                }
            });
            new_reason_for_registry = wave.reason.clone();
            // Detach may have raced the initial root resolution. Recheck while
            // holding the cancellation registry so detach either sees this wave
            // or this wave observes the withdrawn authority.
            if crate::context_repo::path_to_org_root(
                &wave.ctx.codesense_home,
                &wave.ctx.principal(),
                &wave.ctx.org_slug,
            )
            .ok()
            .as_ref()
                != Some(&context_root)
            {
                wave.cancel.cancel();
            }
            in_flight.push(InFlightWave {
                context: Some((
                    wave.ctx.codesense_home.clone(),
                    wave.ctx.principal().as_str().to_string(),
                    wave.ctx.org_slug.clone(),
                )),
                wave_id,
                reason: new_reason_for_registry.clone(),
                trigger,
                cancel: new_cancel_for_registry,
                affected: gate_affected.clone(),
            });
        }

        // SR-0 (Q1): the histogram source. One `debug!` per enqueue naming the
        // wave_id, semantic reason KIND (a parser-friendly token, not the full
        // field-laden JSON), enqueue-site trigger, and post-live-filter target
        // count. `affected` here is already narrowed by
        // `apply_reverse_live_filter` at the `enqueue_*` call site, so pairing
        // this count with the `live filter:` cardinality line (emitted inside the
        // filters) proves the filter is scoping. Gate-and-skip is intrinsic: the
        // `debug!` macro evaluates its args (all trivial `&str`/`len`) only when
        // the `wave` target is enabled at debug, so a default `info` build pays
        // nothing. The soak script buckets by `reason × trigger`.
        debug!(
            target: "content_engine::sync::wave",
            "wave enqueued: id={} reason={} trigger={} affected={}",
            wave_id,
            new_reason_for_registry.kind_str(),
            trigger.as_str(),
            affected_repos.len(),
        );

        // Auditor M2 / Task 3.2: emit `sync-wave-started` BEFORE the
        // spawn body acquires the semaphore permit + per-target keyed locks —
        // UI flips to `active` even if a prior same-target wave is still draining.
        //
        // iter-5 (Edge HIGH-1): include `wave_id` in both started + completed
        // payloads so the UI indicator can correlate them. Without it, a
        // completed event for a superseded wave A can prematurely fade
        // the indicator while wave B is still running.
        (deps.emit)(
            EVENT_WAVE_STARTED,
            emit_wave_started_payload(wave_id, &new_reason_for_registry, &affected_repos),
        );

        // SLW-RI-1: run the wave on the dedicated ENGINE runtime when one has
        // been injected (`set_engine_handle`, production), else fall back to
        // Tauri's global runtime (tests / pre-setup) — behavior-preserving.
        // Routing `wave.run` off Tauri's command-dispatch workers is the whole
        // point: heavy engine work can then never starve command dispatch
        // (stuck terminal / lost input). This is the SINGLE choke point — every
        // wave (watcher-thread enqueue, coalesce timer, reverse-claim,
        // attach-reconcile) funnels through here to execute.
        //
        // NOT the free `tokio::spawn`: `enqueue` is called from dispatcher
        // handlers on notify's watcher thread, which has no ambient Tokio
        // runtime (child threads don't inherit one), so `tokio::spawn` there
        // panics ("no reactor running") — swallowed by the dispatcher's
        // `catch_unwind`, and the wave is silently lost. Both the engine
        // `RuntimeHandle` and `async_runtime::spawn` carry their own runtime
        // context (`.spawn` does `Handle::enter()` + `tokio::spawn`), so they
        // work from any OS thread. The supersede walk + in-flight registration
        // above stay synchronous on the caller's thread, so registration order
        // is preserved. The InFlightGuard removes our wave_id-keyed entry on
        // drop — panic-safe + cancel-safe.
        // SLW-RI-4: the wave's per-target lock key-set — its NORMALIZED
        // `code_repo_path`s (reuse `gate_affected`, already normalized once
        // above for the in-flight gate, so keying and superseding agree on
        // target identity). Cloned because `gate_affected` moves into the
        // `InFlightGuard`.
        let mut lock_keys = gate_affected.clone();
        // SLW-RI-4: a wave that writes the SHARED source (reverse promote /
        // claim-adopt) mutates a surface — the team's context repo — that its
        // `affected` (dest-only) key-set does NOT cover. Add the per-org source
        // root so two same-source reverse writers serialize (restoring the
        // conflict detection the old whole-engine `global_lock` gave), while
        // cross-org reverse waves and all forward waves (source readers) stay
        // concurrent. See [`WaveReason::writes_shared_source`].
        if wave.reason.writes_shared_source() {
            lock_keys.push(Self::normalize_target_path(&context_root));
        }
        let coord = Arc::clone(self);
        let semaphore = Arc::clone(&self.engine_semaphore);
        let run_wave = async move {
            let _guard = InFlightGuard {
                coord: Arc::clone(&coord),
                wave_id,
                affected: gate_affected,
            };
            // SLW-RI-6 (AC-3): acquire the engine permit AND the per-target keyed
            // locks through the no-squat discipline (permit-first → `try_lock`
            // keys → release permit + requeue on contention). Replaces the old
            // inline "permit.acquire().await THEN acquire_wave_target_locks().await"
            // whose middle `.await` parked a wave on a contended key WHILE it held
            // a permit — the ~4-permit pool then filled with permit-holding,
            // key-blocked waves and effective width collapsed (the field's
            // "sync breaks" at N=10–20 opens). RI-4's guarantees are preserved:
            // same-key mutual exclusion (`try_lock` is still exclusive), canonical
            // sorted acquisition (deadlock-free), and reverse per-org
            // serialization. Blind H5: explicit suffix-named bindings so a future
            // refactor can't rename them to `_` and drop the guards mid-run.
            let (_permit, _target_guards) = coord
                .acquire_engine_permit_and_keys(&semaphore, &lock_keys)
                .await;
            // SW-FX-50 (diagnostics): trace the permit's acquire→release lifecycle
            // at `debug` so the "a wave holds a permit and never returns it" stall
            // class is visible in a log capture (RUST_LOG) without a heavyweight
            // watchdog. `PermitDiag`'s Drop logs the release, and — like the permit
            // itself — fires on unwind, so a panicking wave's release is traced too.
            debug!(
                target: "content_engine::sync::wave",
                "wave {} acquired engine permit ({} now free)",
                wave_id,
                semaphore.available_permits()
            );
            let _permit_diag = PermitDiag { wave_id };
            wave.run(wave_id, coord, deps).await
        };
        match self.engine_spawn_handle() {
            Some(engine) => engine.spawn(run_wave),
            None => tauri::async_runtime::spawn(run_wave),
        }
    }

    /// SW-CP-2: enqueue a content-layer projection wave. Called by the
    /// content-layer watcher (`commands::watch`) and the view-override resync
    /// (`commands::install`) in place of the retired `sync_epic_staleness`
    /// path. Uses the active projection target stashed by
    /// [`stash_active_projection_on_team_change`](Self::stash_active_projection_on_team_change)
    /// (D9): the team's content fans out
    /// to EVERY attached code repo for the team (the flat content layout
    /// carries no per-file slug, and each repo's `materialize_projection`
    /// filters the cache to its own `source_repo`). `deps` must carry
    /// `cache` + `projection_staleness` (D8).
    ///
    /// Returns `None` (no wave enqueued) when no team is active yet (the
    /// content watcher can fire during the pre-team-activation startup
    /// window), when the team has no attached repos, or when enumeration
    /// fails.
    /// SW-CP-9: snapshot the active team's resolver context, for the watcher
    /// to resolve per-team projection dest dirs when registering reverse
    /// watches (`commands::watch::resolve_reverse_dest_prefixes`). The watcher swaps
    /// in the config's own team via [`ctx_with_team`], so only the shared
    /// `codesense_home` / `user_id` / `org_slug` are consumed from here.
    /// `None` until the first team is active — the caller falls back to the
    /// single story dest (SW-CP-4 behavior). Held only for the brief clone,
    /// so it never participates in the watcher lock order.
    pub fn active_ctx(&self) -> Option<ResolverContext> {
        self.active_projection_target
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|t| t.ctx.clone()))
    }

    /// SR-3: the active team's `(ctx, source_repos)` — the inputs to
    /// [`enumerate_attached_code_repos_for_team`], which enumerates every
    /// codesense-managed attached repo for the active team (story worktrees AND
    /// `_epic`/`_branch` clones — the set that actually draws sync waves). A cheap
    /// lock+clone; the enumeration itself (a filesystem walk) is left to the caller's
    /// blocking pool. `None` when no team is active (nothing attached to bound).
    pub fn active_target_repos(&self) -> Option<(ResolverContext, Vec<SourceRepoSlug>)> {
        self.active_projection_target
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|t| (t.ctx.clone(), t.source_repos.clone())))
    }
}

/// RF-perf-20: reap a detached / gone worktree's durable state — drop this
/// worktree's install-store rows, so `enumerate_attached_code_repos_for_team`
/// (which re-derives attachment purely from the install store) stops seeing it.
///
/// The ONE reap definition, shared by the removal tail (`prune_worktree_locked`),
/// the launch reconcile (SW-XS-3/SW-XS-4 K7: worktree records and store targets
/// confirmed gone), and the explicit wave's gone-path detector
/// ([`run_repo_dispatch`]). Idempotent: `delete_all_for_path` on absent rows is a
/// no-op, so a double reap is harmless.
///
/// **O(1) and non-blocking:** one indexed store delete — no tree walk, no fan-out,
/// and no wave / repo lock is held across it. `code_repo_path` must be the exact enumerated form the sweep keys under so
/// the prune compares are exact (the callers pass the enumerated `AttachedCodeRepo`
/// path).
pub(crate) fn reap_detached_worktree(codesense_home: &Path, code_repo_path: &Path) {
    // SW-IS-3c: the store is the SOLE source of truth. Drop EVERY team's store row at
    // this worktree path (the reap acts on a physical worktree, which under the
    // composite key can own one row per team). Best-effort: an unavailable store or a
    // delete failure is warn-logged, not fatal.
    if let Some(store) = crate::content_engine::lockfile::open_sync_state_store(codesense_home) {
        if let Err(e) = store.delete_all_for_path(code_repo_path) {
            warn!(
                target: "content_engine::sync::wave",
                "reap: install-store delete failed for {} (store rows leak persist): {}",
                code_repo_path.display(), e
            );
        }
    } else {
        warn!(
            target: "content_engine::sync::wave",
            "reap: install store unavailable for {} — store rows not pruned",
            code_repo_path.display()
        );
    }
}

/// RF-perf-20: reap `attached` via [`reap_detached_worktree`] IFF it was genuinely
/// removed, returning whether the reap fired (for the `sync-repo-looks-gone`
/// payload's `reaped` field — AC-2). The gate is deliberately STRICTER than the
/// lenient `repo_path_resolves` (`is_dir`) skip the caller already applied, so a
/// transient never destroys durable state (AC-4) and SS-FU-17 AC-7's unmount-safety
/// is preserved:
///   * `try_exists() == Ok(false)` — the path DEFINITIVELY does not exist (ENOENT).
///     A permission / IO error is `Err(_)` and a non-dir-in-place is `Ok(true)`;
///     neither reaps, and both self-heal next tick via the caller's `is_dir` skip.
///   * parent still a directory — the worktree dir is gone but its CONTAINER
///     remains, i.e. THIS one worktree was removed (git worktree remove / external
///     rm / crash), NOT a whole-volume unmount (which takes the parent too → left
///     tracked → self-heals on remount).
///
/// Resolves OQ-3: ENOENT-specificity + parent-present IS the confirm, so no
/// multi-tick debounce is needed. Reappearance is safe (AC-3): the re-attach path
/// rebuilds the lockfile from scratch. Pure decision + O(1) reap — two stats plus
/// the unlink/prune, only on the rare gone path. Callers offload it to
/// `spawn_blocking` so the stat/unlink/prune never runs on the async runtime.
///
/// `try_exists()` follows symlinks, so a `code_repo_path` that is a symlink to a
/// now-deleted target reads absent and is reaped — acceptable (re-attach rebuilds,
/// AC-3), and codesense worktrees are real git dirs, never symlinks, so this is not
/// a live path.
fn try_reap_gone_worktree(ctx: &ResolverContext, attached: &AttachedCodeRepo) -> bool {
    reap_if_gone(&ctx.codesense_home, &attached.code_repo_path)
}

/// The path-only core of [`try_reap_gone_worktree`]: reap `code_repo_path`'s store
/// rows iff it is definitively absent AND its container still exists (one worktree
/// removed, not a volume unmounted). Returns whether the reap fired.
pub(crate) fn reap_if_gone(codesense_home: &Path, code_repo_path: &Path) -> bool {
    let definitely_absent = matches!(code_repo_path.try_exists(), Ok(false));
    let container_present = code_repo_path.parent().map(|p| p.is_dir()).unwrap_or(false);
    if !(definitely_absent && container_present) {
        return false;
    }
    reap_detached_worktree(codesense_home, code_repo_path);
    true
}

/// SW-XS-4 (K7): the launch-reconcile half of gone-path reaping for every LOCAL
/// store target of `user_id` — branch workspaces, epic dirs and managed clones
/// included, not only the story worktrees the worktree index records. Before
/// SW-XS-4 the reverse sweeps reached every attached target and reaped a gone one
/// in passing ([`try_reap_gone_worktree`] inside a wave); those waves are gone, so
/// the launch reconcile owns it. Same strict gate ([`reap_if_gone`]). Returns how
/// many targets were reaped.
pub(crate) fn reap_gone_store_targets(codesense_home: &Path, user_id: u64) -> usize {
    let Some(store) = crate::content_engine::lockfile::open_sync_state_store(codesense_home) else {
        return 0;
    };
    let targets = match store.local_targets(Some(user_id)) {
        Ok(targets) => targets,
        Err(e) => {
            warn!(
                target: "content_engine::sync::wave",
                "gone-target reap: enumerate failed — skipping: {}", e
            );
            return 0;
        }
    };
    let mut paths: Vec<PathBuf> = targets.into_iter().map(|(_, _, path)| path).collect();
    paths.dedup();
    paths
        .iter()
        .filter(|path| reap_if_gone(codesense_home, path))
        .count()
}

/// Monotonic counter for `InFlightWave.wave_id`. Process-wide; never
/// resets. Wraps at `u64::MAX` (effectively unreachable in practice —
/// would take 5 × 10^11 years at 1 wave/ns).
static NEXT_WAVE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Drop guard that removes the matching `in_flight` entry by `wave_id`
/// on drop — regardless of whether the spawned task panicked, was
/// cancelled, or completed normally.
///
/// SW-FX-50 (diagnostics): a Drop-logged marker for the engine-permit lifecycle.
/// Declared in `run_wave` right after the permit is acquired; its Drop logs the
/// release at `debug`. Because Drop runs on unwind, a wave that PANICS while
/// holding a permit still logs its release — so a log capture shows the full
/// acquire→release pairing, and an unpaired "acquired" with no "released" pins a
/// genuinely stuck/held permit without a heavyweight watchdog.
struct PermitDiag {
    wave_id: u64,
}

impl Drop for PermitDiag {
    fn drop(&mut self) {
        debug!(
            target: "content_engine::sync::wave",
            "wave {} released engine permit",
            self.wave_id
        );
    }
}

/// Iter-2 patch (Edge HIGH-2): cleanup uses unique `wave_id` rather
/// than `WaveReason` equality. The prior reason-equality approach was
/// racy for non-self-superseding reasons (`Startup`, distinct-name
/// `ContentLayerEvent`) where two in-flight entries could share a
/// reason value — the first finishing would wipe both.
///
/// SW-WL-2: drop is ALSO the wave-in-flight gate's drain trigger. After
/// removing this wave's entry, for each target it covered that now has
/// no remaining in-flight wave, it fires + clears any deferred detach the
/// close path queued (AC-3 / AC-5). Living in `drop` (not `Wave::run`'s
/// happy path) is what makes a panicking/cancelled wave still release a
/// deferred watcher (AC-5).
struct InFlightGuard {
    coord: Arc<WaveCoordinator>,
    wave_id: u64,
    /// NORMALIZED target paths this wave wrote into (SW-WL-2); same form
    /// as the `pending_detach` keys, so drop matches with a plain compare.
    /// Drop re-checks each for drain to fire deferred detaches.
    affected: Vec<PathBuf>,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        // Single `in_flight` acquisition guards: (1) removing our entry,
        // (2) the drain re-check, and (3) firing the deferred detaches —
        // so a concurrent `enqueue` for one of our targets can't slip in
        // between "no waves left" and "detach" (AC-7). Lock order
        // in_flight → pending_detach → (watcher_lock inside the DetachFn);
        // never reversed (AC-10).
        let Ok(mut in_flight) = self.coord.in_flight.lock() else {
            // Poisoned mutex is unrecoverable but not worth panicking-from-
            // drop; the registry is bounded by the supersede walk anyway.
            return;
        };
        in_flight.retain(|w| w.wave_id != self.wave_id);

        // Collect detaches for targets that just drained (still holding
        // in_flight). `pending_detach` is empty in steady state, so this
        // whole block is a cheap no-op on the hot path. A drained path
        // fires ALL of its subscribers' deferred detaches (the inner
        // context→DetachFn map), not just one.
        let mut to_fire: Vec<DetachFn> = Vec::new();
        if let Ok(mut pending) = self.coord.pending_detach.lock() {
            if !pending.is_empty() {
                for target_norm in &self.affected {
                    if !WaveCoordinator::target_in_flight(&in_flight, target_norm) {
                        if let Some(by_context) = pending.remove(target_norm) {
                            to_fire.extend(by_context.into_values());
                        }
                    }
                }
            }
        }
        // Fire while still holding `in_flight` so no wave can re-acquire a
        // target between drain confirmation and detach (AC-7). The DetachFn
        // takes the watcher's `watcher_lock` internally — in_flight →
        // watcher_lock, the one nesting direction (AC-10). Each fire is
        // isolated by `catch_unwind`: a panicking detach must not poison
        // `in_flight` (held here) nor skip the remaining sibling detaches.
        for detach in &to_fire {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| detach()));
        }
    }
}

// =============================================================================
// AC-2 / AC-11: Wave::run orchestration
// =============================================================================

const EVENT_WAVE_STARTED: &str = "sync-wave-started";
const EVENT_WAVE_COMPLETED: &str = "sync-wave-completed";

/// SS-FU-17 AC-6: emitted when a wave skips a code repo whose path no
/// longer resolves (moved / deleted / unmounted). The frontend surfaces
/// "this repo looks gone" with Forget / Re-attach affordances (AC-7).
const EVENT_REPO_LOOKS_GONE: &str = "sync-repo-looks-gone";

/// SS-FU-17 AC-6: does `code_repo_path` still resolve to a directory on
/// disk? A code repo is a directory; a moved / deleted / unmounted path
/// returns `false`. This is the single existence gate every wave funnels
/// through (`Wave::run`'s per-repo loop) — it covers every dispatch entry
/// point (`Startup`, `TeamSwitch`, `ContentLayerEvent`, …) without each
/// enumerator having to re-implement the check.
fn repo_path_resolves(code_repo_path: &Path) -> bool {
    code_repo_path.is_dir()
}

/// SS-FU-17 AC-6: payload for [`EVENT_REPO_LOOKS_GONE`] — the slug + the
/// dead path, so the UI can offer Forget / Re-attach keyed on the slug.
/// RF-perf-20 (AC-2): `reaped` tells the UI whether the engine already detached
/// the worktree (deleted its lockfile) — `true` → a single "removed / detached"
/// status for the SLW-WL-2 panel; `false` → a transient "looks gone" the engine
/// left tracked (unmount / mid-write). Because a reap drops the repo from
/// enumeration, the `reaped:true` variant fires exactly once (no stream).
fn repo_gone_payload(attached: &AttachedCodeRepo, reaped: bool) -> serde_json::Value {
    serde_json::json!({
        "owner": attached.slug.owner,
        "repo": attached.slug.repo,
        "code_repo_path": attached.code_repo_path,
        "reaped": reaped,
    })
}

impl Wave {
    /// Drive the wave to completion. See AC-2 for the per-repo recipe.
    ///
    /// Iter-2 patches:
    /// - **Auditor M2:** `sync-wave-started` is emitted by `enqueue`
    ///   BEFORE the permit + keyed-lock acquire. `Wave::run` no longer
    ///   duplicates that emit.
    /// - **Blind H2:** `duration_ms` is `u64` per `WaveResult` rework.
    ///
    /// Iter-5 (Edge HIGH-1 / Auditor H2): `wave_id` is threaded in from
    /// `enqueue` so the completed payload can carry the same identifier
    /// that started carried (UI correlation). `affected_repos` in the
    /// completed payload is sourced from `self.affected` (the full,
    /// upfront list) — NOT `result.per_repo.keys()` — so cancellation
    /// mid-loop doesn't shrink the denominator the UI reads as "N" in
    /// "synced X of N".
    pub async fn run(
        self,
        wave_id: u64,
        coord: Arc<WaveCoordinator>,
        deps: WaveDeps,
    ) -> WaveResult {
        let started = std::time::Instant::now();

        info!(
            target: "content_engine::sync::wave",
            "wave start: reason={:?} affected_repo_count={}",
            self.reason,
            self.affected.len()
        );

        let mut result = WaveResult::default();
        let mut cancelled = false;

        // SLW-RI-5 (AC-1/AC-2): materialize each affected repo through the
        // shared per-repo body (`run_repo_dispatch`), collecting an INDEPENDENT
        // outcome per repo into an `affected`-index-aligned `slots` vec, then
        // fold in canonical order (`fold_per_repo_outcomes` — the AC-3 parity
        // guard, order-independent). The width is direction-gated
        // (`fanout_width_for_reason`): a multi-repo `MigrationCutover` runs
        // bounded-concurrent;
        // reverse / shared-source reasons resolve to width 1 (serial) so RI-4's
        // per-org source serialization is preserved by construction (AC-6).
        //
        // No new lock or permit: the wave already holds its RI-4 per-target
        // keyed locks for EVERY repo in `affected` across the whole `run()`, and
        // it runs UNDER the engine-pool semaphore permit `enqueue` acquired.
        // `buffer_unordered` polls the per-repo futures on THIS task (never
        // re-entering `enqueue` or the permit); each body offloads its heavy
        // work to `spawn_blocking`, so N blocking jobs run at once on the
        // isolated engine runtime (RI-1), off the command plane.
        let width = fanout_width_for_reason(&self.reason, self.affected.len());
        let mut slots: Vec<Option<PerRepoOutcome>> =
            (0..self.affected.len()).map(|_| None).collect();

        if width <= 1 {
            // Serial path — reverse/shared-source, single-repo, or a 1-wide cap.
            // Byte-identical to the pre-RI-5 loop (cancel checked each boundary).
            for (idx, attached) in self.affected.iter().enumerate() {
                if self.cancel.is_cancelled() {
                    cancelled = true;
                    break;
                }
                slots[idx] = Some(
                    run_repo_dispatch(
                        &self.reason,
                        &self.ctx,
                        attached,
                        &deps,
                        &coord,
                        self.initiator,
                        self.direction,
                    )
                    .await,
                );
                if self.cancel.is_cancelled() {
                    cancelled = true;
                    break;
                }
            }
        } else {
            // Bounded-concurrent forward fan-out. Each future checks cancel at
            // entry, so a cancel stops DISPATCHING new repos while in-flight
            // ones drain — same on-disk/status outcome as the serial break
            // (AC-5): a repo running when cancel fired still completes + folds
            // (matching the serial post-repo insert), repos not yet started are
            // skipped. Results arrive in completion order but are slotted BY
            // INDEX, so the fold stays canonical-`affected`-order (AC-3).
            use futures_util::StreamExt as _;
            let reason = &self.reason;
            let ctx = &self.ctx;
            let cancel = &self.cancel;
            let deps_ref = &deps;
            let coord_ref = &coord;
            // RF-perf-24: `SyncInitiator` is `Copy` — bind it out of `self` so the
            // per-repo futures stay `Send + 'static` (same pattern as the `&self`
            // fields above).
            let initiator = self.initiator;
            let direction = self.direction;
            // Build the per-repo futures eagerly (lazy — not polled until
            // `buffer_unordered` drives them). Each future OWNS its
            // `AttachedCodeRepo` (a cheap clone: slug + one `PathBuf`) rather
            // than borrowing the iterator item — a borrowed `&AttachedCodeRepo`
            // captured across the `.await` introduces a universally-quantified
            // lifetime that makes the spawned `run()` future fail its
            // `Send + 'static` bound. The other captures are `&` to stable
            // `run()`-body locals (single lifetime), which is fine.
            let futs =
                self.affected
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(idx, attached)| async move {
                        if cancel.is_cancelled() {
                            return (idx, None);
                        }
                        let outcome = run_repo_dispatch(
                            reason, ctx, &attached, deps_ref, coord_ref, initiator, direction,
                        )
                        .await;
                        (idx, Some(outcome))
                    });
            let completed: Vec<(usize, Option<PerRepoOutcome>)> = futures_util::stream::iter(futs)
                .buffer_unordered(width)
                .collect()
                .await;
            for (idx, out) in completed {
                match out {
                    Some(outcome) => slots[idx] = Some(outcome),
                    // A repo skipped because cancel fired before it dispatched.
                    None => cancelled = true,
                }
            }
            // A cancel that fires AFTER `buffer_unordered` has already admitted
            // every repo (width ≥ remaining) leaves no future to hit the entry
            // gate, so the per-future `None` above never trips — yet the wave was
            // still cancelled/superseded. Fold the token state in directly so the
            // `cancelled` flag (and the `sync-wave-completed` label it drives)
            // matches the serial break's intent regardless of dispatch timing.
            cancelled |= self.cancel.is_cancelled();
        }
        fold_per_repo_outcomes(&mut result, &self.affected, slots, &deps.emit);

        result.cancelled = cancelled;
        // Blind H2: u128 → u64. Realistic waves max out in seconds;
        // saturating cast guards against the impossible-overflow case.
        result.duration_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;

        let summary = summarize(&result);
        info!(
            target: "content_engine::sync::wave",
            "wave done: reason={:?} duration_ms={} cancelled={} written={} skipped={} unmaterialized={} errors={}",
            self.reason,
            result.duration_ms,
            result.cancelled,
            summary.written,
            summary.skipped,
            summary.unmaterialized,
            result.errors.len()
        );

        // Iter-5 (Auditor H2): the completed event's `affected_repos` is
        // `self.affected`'s FULL path list, so its denominator matches the started
        // event even under cancellation.
        let completed_affected: Vec<PathBuf> = self
            .affected
            .iter()
            .map(|a| a.code_repo_path.clone())
            .collect();

        // AC-9: emit completed. Iter-5 (Auditor H2): pass `self.affected`'s full
        // path list so the payload's `affected_repos` matches the started event's
        // denominator under cancellation. Iter-5 (Edge HIGH-1): include `wave_id`
        // for UI correlation.
        (deps.emit)(
            EVENT_WAVE_COMPLETED,
            emit_wave_completed_payload(wave_id, &self.reason, &completed_affected, &result),
        );

        result
    }
}

#[derive(Debug, PartialEq, Eq)]
struct CountSummary {
    written: usize,
    skipped: usize,
    unmaterialized: usize,
}

fn summarize(result: &WaveResult) -> CountSummary {
    let mut written = 0usize;
    let mut skipped = 0usize;
    let mut unmaterialized = 0usize;
    for counts in result.per_repo.values() {
        written += counts.written;
        skipped += counts.skipped_already_current + counts.skipped_locally_modified;
        unmaterialized += counts.unmaterialized;
    }
    CountSummary {
        written,
        skipped,
        unmaterialized,
    }
}

/// SLW-RI-5: the independent outcome of materializing ONE repo in a wave's
/// fan-out. The pre-RI-5 serial loop mutated `result.errors` /
/// `result.per_repo` / `result.stage_profiles` in place; concurrent futures
/// cannot share `&mut`, so each per-repo body now RETURNS its outcome and the
/// caller folds them in canonical `affected` order — deterministic regardless
/// of completion order (the AC-3 parity guarantee). Mirrors SLW-FX-5's
/// `StoryProjectionOutcome`.
struct PerRepoOutcome {
    /// The repo this outcome is for (equals `attached.code_repo_path`). Kept so
    /// the fold can `insert` without re-deriving it from the `affected` slot.
    code_repo_path: PathBuf,
    /// `None` IFF the repo's path no longer resolves (gone) — the fold emits
    /// `EVENT_REPO_LOOKS_GONE` and inserts nothing, mirroring the old loop's
    /// `continue`. `Some(_)` for every dispatched repo (even an empty tally).
    counts: Option<RepoWaveCounts>,
    /// Per-repo errors, folded into `result.errors` in `affected` order.
    errors: Vec<WaveError>,
    /// Per-repo stage profiles (only the default `run_repo_reconcile` arm emits these).
    stage_profiles: Vec<RepoStageProfile>,
    /// SW-XS-4: per-repo both-diverged files (see [`WaveResult::conflicts`]),
    /// folded in `affected` order.
    conflicts: Vec<WaveError>,
    /// RF-perf-20: `true` IFF this gone-path outcome (`counts == None`) actually
    /// **reaped** the worktree — deleted its lockfile + pruned reverse state
    /// because the path was DEFINITIVELY absent (`try_exists() == Ok(false)`).
    /// `false` for every non-gone outcome and for a gone path we deliberately did
    /// NOT reap (transient unreadable / lockfile-path unresolved — AC-4). The fold
    /// forwards it into the `sync-repo-looks-gone` payload so the SLW-WL-2 panel can
    /// show "detached" vs. a transient "looks gone".
    reaped: bool,
}

/// SLW-RI-5 (AC-6): test-only probe that short-circuits `run_repo_dispatch` so
/// the concurrency of the fan-out can be observed WITHOUT standing up a real
/// cache / disk materialize. Global atomics (not TLS) because `buffer_unordered`
/// may poll bodies across runtime workers. Serialize probe tests with
/// `test_support::env_guard()` (they also set `CS_MATERIALIZE_PARALLELISM`).
/// Compiled out entirely in non-test builds — zero production cost.
#[cfg(test)]
mod ri5_probe {
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
    pub static ARMED: AtomicBool = AtomicBool::new(false);
    pub static DELAY_MS: AtomicU64 = AtomicU64::new(0);
    pub static INFLIGHT: AtomicUsize = AtomicUsize::new(0);
    pub static MAX_INFLIGHT: AtomicUsize = AtomicUsize::new(0);
    pub static STARTED: AtomicUsize = AtomicUsize::new(0);

    /// Arm the probe with a per-body async delay (ms) and reset the counters.
    pub fn arm(delay_ms: u64) {
        use std::sync::atomic::Ordering::SeqCst;
        DELAY_MS.store(delay_ms, SeqCst);
        INFLIGHT.store(0, SeqCst);
        MAX_INFLIGHT.store(0, SeqCst);
        STARTED.store(0, SeqCst);
        ARMED.store(true, SeqCst);
    }
    pub fn disarm() {
        use std::sync::atomic::Ordering::SeqCst;
        ARMED.store(false, SeqCst);
    }
    pub fn max_inflight() -> usize {
        MAX_INFLIGHT.load(std::sync::atomic::Ordering::SeqCst)
    }
    pub fn started() -> usize {
        STARTED.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// SLW-RI-5: when the test probe is armed, record concurrency + sleep, then
/// return a deterministic synthetic outcome (`written = 1`) instead of the real
/// dispatch. Returns `None` in production (probe disarmed) so the real body
/// runs. The `#[cfg(test)]` gate erases this in release builds.
#[cfg(test)]
async fn ri5_probe_intercept(path: &Path) -> Option<PerRepoOutcome> {
    use std::sync::atomic::Ordering::SeqCst;
    if !ri5_probe::ARMED.load(SeqCst) {
        return None;
    }
    ri5_probe::STARTED.fetch_add(1, SeqCst);
    let cur = ri5_probe::INFLIGHT.fetch_add(1, SeqCst) + 1;
    ri5_probe::MAX_INFLIGHT.fetch_max(cur, SeqCst);
    let delay = ri5_probe::DELAY_MS.load(SeqCst);
    if delay > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
    }
    ri5_probe::INFLIGHT.fetch_sub(1, SeqCst);
    Some(PerRepoOutcome {
        code_repo_path: path.to_path_buf(),
        counts: Some(RepoWaveCounts {
            written: 1,
            ..Default::default()
        }),
        errors: Vec::new(),
        stage_profiles: Vec::new(),
        conflicts: Vec::new(),
        reaped: false,
    })
}

/// SLW-RI-5: materialize ONE repo and RETURN its independent outcome — no
/// shared `&mut result`. This is `Wave::run`'s per-repo body, lifted out
/// verbatim so it can run either serially (Task-1 fold-parity groundwork) or
/// under the bounded-concurrent driver. Gone-path detection returns
/// `counts: None` so the caller emits `EVENT_REPO_LOOKS_GONE` during the
/// ordered fold — keeping the emit order canonical even when bodies finish out
/// of order. The per-repo lock is taken *inside* each `run_repo_*` (disjoint
/// worktrees → disjoint locks), never `SWITCH_LOCK`, so concurrent bodies for
/// distinct repos never contend.
#[allow(clippy::too_many_arguments)]
async fn run_repo_dispatch(
    reason: &WaveReason,
    ctx: &ResolverContext,
    attached: &AttachedCodeRepo,
    _deps: &WaveDeps,
    coord: &Arc<WaveCoordinator>,
    // RF-perf-24 (AC-5): the wave's reverse-sync initiator, forwarded from
    // `Wave::initiator` for the completion stamp.
    initiator: crate::install::worktree_index::SyncInitiator,
    _direction: crate::install::story_linker::SyncMode,
) -> PerRepoOutcome {
    // SS-FU-17 AC-6: a code repo whose path no longer resolves (moved / deleted
    // / unmounted) must NOT be dispatched into git ops. Signal it back so the
    // ordered fold emits `EVENT_REPO_LOOKS_GONE` and inserts nothing (the old
    // loop's `continue`).
    if !repo_path_resolves(&attached.code_repo_path) {
        // RF-perf-20: reap a genuinely-removed worktree. `try_reap_gone_worktree`
        // gates on definitive-absence + container-present so it never reaps a
        // transient / unmounted volume (AC-4 / SS-FU-17 AC-7), and returns whether
        // it fired for the `sync-repo-looks-gone` payload (AC-2). Offloaded to
        // `spawn_blocking` so the stat + store delete never stall the async engine
        // worker. Join failure → `false` (don't claim a reap we can't confirm).
        let reaped = {
            let ctx = ctx.clone();
            let attached = attached.clone();
            tokio::task::spawn_blocking(move || try_reap_gone_worktree(&ctx, &attached))
                .await
                .unwrap_or(false)
        };
        return PerRepoOutcome {
            code_repo_path: attached.code_repo_path.clone(),
            counts: None,
            errors: Vec::new(),
            stage_profiles: Vec::new(),
            conflicts: Vec::new(),
            reaped,
        };
    }

    #[cfg(test)]
    if let Some(outcome) = ri5_probe_intercept(&attached.code_repo_path).await {
        return outcome;
    }

    let mut errors = Vec::new();
    let mut stage_profiles = Vec::new();
    let conflicts = Vec::new();
    let counts = match reason {
        // The migration cutover materializes nothing here post-install-retirement:
        // `run_repo_reconcile` only records the wave against the store. Listed
        // explicitly (no `_` wildcard) so a newly-added `WaveReason` is a compile
        // error here and forces a routing decision.
        WaveReason::MigrationCutover { .. } => {
            run_repo_reconcile(ctx, attached, coord, &mut errors, &mut stage_profiles).await
        }
    };

    // RF-perf-24 (AC-5): the single reverse-sync completion stamp. Fires once per repo
    // per wave whenever the explicit reconcile actually promoted ≥1 dest edit.
    // Attributed to the wave's `initiator` (`Agent` for the MCP reconcile). A no-op
    // when the repo has no worktree-index record (a raw-attached repo) — never an
    // error (see `stamp_reverse_sync`). Skipped on error so a failed promote can't
    // masquerade as a fresh sync. SW-XS-4: a both-diverged file is a conflict, not
    // an error — the files that DID publish still stamp.
    if should_stamp_reverse_sync(&errors, &counts) {
        if let Err(e) = crate::install::worktree_index::stamp_reverse_sync(
            &ctx.codesense_home,
            &ctx.principal(),
            &attached.code_repo_path,
            initiator,
            crate::content_engine::sync::now_unix_ms(),
        ) {
            warn!(
                target: "content_engine::sync::wave",
                "RF-perf-24: failed to stamp last-reverse-sync for {}: {}",
                attached.code_repo_path.display(), e
            );
        }
    }

    PerRepoOutcome {
        code_repo_path: attached.code_repo_path.clone(),
        counts: Some(counts),
        errors,
        stage_profiles,
        conflicts,
        reaped: false,
    }
}

/// RF-perf-24 / SW-XS-4: stamp the last-reverse-sync provenance iff the explicit
/// reconcile promoted ≥1 edit and hit no error. Conflicts are NOT errors (they
/// live in `conflicts`), so a conflicted file never hides the files that did
/// publish.
fn should_stamp_reverse_sync(errors: &[WaveError], counts: &RepoWaveCounts) -> bool {
    errors.is_empty() && counts.reverse_written > 0
}

/// SLW-RI-5 (AC-3): fold per-repo outcomes into `result` in canonical
/// `affected` order — the parity + reproducibility guard. `slots` is
/// index-aligned to `affected`: a `None` slot is a repo that was never
/// dispatched (cancellation stopped the fan-out); a `Some` slot with
/// `counts == None` is a gone path (emit `EVENT_REPO_LOOKS_GONE`, insert
/// nothing). Iterating `affected` (not completion order) makes `per_repo`,
/// `errors`, `stage_profiles`, and every downstream summary/payload
/// byte-identical to the serial baseline regardless of who finished first.
fn fold_per_repo_outcomes(
    result: &mut WaveResult,
    affected: &[AttachedCodeRepo],
    slots: Vec<Option<PerRepoOutcome>>,
    emit: &Arc<dyn Fn(&'static str, serde_json::Value) + Send + Sync>,
) {
    for (attached, slot) in affected.iter().zip(slots) {
        let Some(outcome) = slot else {
            continue;
        };
        match outcome.counts {
            None => {
                // Gone path — mirror the old loop's warn + emit + no insert. The
                // reap itself (lockfile delete + reverse-state prune) already ran in
                // `run_repo_dispatch` at detection (where `ctx` is in scope);
                // `outcome.reaped` carries whether it fired (RF-perf-20, AC-2), so
                // the emit stays here in canonical `affected` order.
                warn!(
                    target: "content_engine::sync::wave",
                    "skipping wave for gone path {} ({}/{}) — surfacing {} (reaped={})",
                    attached.code_repo_path.display(),
                    attached.slug.owner,
                    attached.slug.repo,
                    EVENT_REPO_LOOKS_GONE,
                    outcome.reaped
                );
                emit(
                    EVENT_REPO_LOOKS_GONE,
                    repo_gone_payload(attached, outcome.reaped),
                );
            }
            Some(counts) => {
                result.per_repo.insert(outcome.code_repo_path, counts);
                result.errors.extend(outcome.errors);
                result.stage_profiles.extend(outcome.stage_profiles);
                result.conflicts.extend(outcome.conflicts);
            }
        }
    }
}

// The forward, manifest-authoritative wave body. Post-install-retirement it no
// longer materializes: it records the wave against the target's store row
// (entries flow through unchanged).
async fn run_repo_reconcile(
    ctx: &ResolverContext,
    attached: &AttachedCodeRepo,
    coord: &Arc<WaveCoordinator>,
    errors: &mut Vec<WaveError>,
    stage_profiles: &mut Vec<RepoStageProfile>,
) -> RepoWaveCounts {
    // SR-0 (Q2): start the wall clock BEFORE the repo-lock acquire so lock
    // contention lands inside `total` and is attributable via `lock_wait_ns`
    // (the incident showed ~39 repo / 42 wave-target locks in flight). We only
    // *measure* the wait here — the locking itself is unchanged (a contention
    // fix is out of spike scope; hand to SR-1b).
    let reconcile_start = std::time::Instant::now();
    let _repo_guard = coord.acquire_repo_lock(&attached.code_repo_path).await;
    let lock_wait_ns: u128 = reconcile_start.elapsed().as_nanos();

    // SS-FU-11 AC-1: per-stage timing. The install-layer materialize fan-out
    // (and its resolve/read/hash/write breakdown) is retired — the only spans
    // this fn still measures are the lock wait and the store persist.
    // `lockfile_persist_ns` is the single end-of-wave store write, bound directly
    // to its one measured span below (like `lock_wait_ns` above).

    let counts = RepoWaveCounts::default();

    // SLW-FX-13 (AC-6): the managed-surface guard. The work below writes into
    // `attached.code_repo_path`'s store row. That path is caller-supplied
    // and reaches here UNVALIDATED — the dispatch gate `repo_path_resolves` only
    // checks `is_dir`, never managed-ness. A non-managed target (a raw-attached main
    // checkout) is a DELIBERATE, logged no-op ONCE per repo (never per file — AC-9),
    // before any reconcile/persist: codesense only touches worktrees / managed
    // clones, so managed content never lands in a user's primary checkout. Not an
    // error (this is the normal case for attached
    // repos until the managed-clone/worktree flow lands) — a graceful skip that
    // returns zero counts. Managed = a registered worktree OR under
    // `{codesense_home}/users/` (attached-repo worktrees live outside home, so the
    // worktree registry is the authoritative oracle — see
    // `worktree_index::is_codesense_managed_target`).
    // SW-MC-2 (AC-6): OR the `managed` marker (resolved once at enumeration) so a
    // managed clone at a configured `workspace_root_dir` (outside `~/.codesense`,
    // hence failing both predicate branches) still materializes its install layer.
    if !(attached.managed
        || crate::install::worktree_index::is_codesense_managed_target(
            &attached.code_repo_path,
            &ctx.codesense_home,
        ))
    {
        debug!(
            target: "content_engine::sync",
            "run_repo_reconcile: {} is not a codesense-managed surface — skipping store persist (no writes; worktrees/managed-clones only)",
            attached.code_repo_path.display()
        );
        return counts;
    }

    // Open the install store once — the SOLE source of truth for this target's
    // install-state (SW-IS-3b: the JSON lockfile write is gone below). Fail-closed:
    // SW-IS-4 made `open` self-healing (a corrupt DB is quarantined + rebuilt, never
    // surfaced), so a `None` here is a genuine environment fault (permissions/disk).
    // With no JSON fallback, a swallowed store failure would silently lose the wave's
    // state, so an unavailable store is a hard wave error, not a no-op.
    let store = match crate::content_engine::lockfile::open_sync_state_store(&ctx.codesense_home) {
        Some(store) => store,
        None => {
            errors.push(WaveError {
                code_repo_path: attached.code_repo_path.clone(),
                logical_name: None,
                message: "install store unavailable (open returned None)".to_string(),
            });
            return counts;
        }
    };

    // The lockfile is built from `ctx` + the target path with EMPTY entries: its
    // (org, team, path) identity equals the target by construction, and this reconcile
    // arm neither reads nor writes entries.
    let lockfile = crate::content_engine::lockfile::Lockfile::empty(
        ctx.org_slug.clone(),
        ctx.team_name.clone(),
        attached.code_repo_path.clone(),
    );

    // The install-layer materialize fan-out is retired: the wave no longer resolves
    // a per-reason logical-name set, spins up a parallel worker pool, folds a
    // mutation set, crosses an fsync barrier, or reconciles `.git/info/exclude`.
    // Real content hydration is the projection engine's separate path. This arm only
    // records the wave against the target; it never reads or writes a
    // `materialized_entry` row.

    // Record this wave against the target in the store — the SOLE writer this slice
    // (SW-IS-3b): the per-file ~11MB JSON `Lockfile::write` (the fsync-storm the epic
    // targets) is gone. `persist_reconcile` advances the reaper's recency without ever
    // touching a `materialized_entry` row (SW-CP-35). The runtime identity guard the full
    // `upsert` needed is gone: `lockfile` is built from `ctx` above, so its
    // (org, team, path) identity equals the target by construction. Keep that invariant as
    // a `debug_assert!` — zero release cost, but a future edit that lets the lockfile
    // identity drift from the target trips it in tests rather than silently keying a
    // different team's row. Fail-closed, mirroring the read: with no JSON fallback, a
    // persist failure is a `WaveError`, never a swallowed warn. Runs off the UI worker (the
    // sync engine's task). SR-0 (Q2): time the persist span.
    debug_assert!(
        lockfile.code_repo_path.as_os_str() == attached.code_repo_path.as_os_str()
            && lockfile.org_slug == ctx.org_slug
            && lockfile.team_name.as_deref() == ctx.team_name.as_deref(),
        "reconcile lockfile identity must equal the target by construction"
    );
    let persist_start = std::time::Instant::now();
    if let Err(e) = store.persist_reconcile(ctx.user_id, &attached.slug, &lockfile) {
        warn!(
            target: "content_engine::sync::wave",
            "install store write failed for {}: {}",
            attached.code_repo_path.display(),
            e
        );
        errors.push(WaveError {
            code_repo_path: attached.code_repo_path.clone(),
            logical_name: None,
            message: format!("install store write failed: {}", e),
        });
    }
    let lockfile_persist_ns: u128 = persist_start.elapsed().as_nanos();

    // The `.git/info/exclude` reconcile is retired with the install-layer fan-out —
    // it registered install-managed (`projection_key.is_none()`) entries, which are
    // no longer materialized here. Projection entries self-manage their exclude
    // registration dir-level via `story_linker`.

    // SS-FU-11 AC-1 / SR-0 (Q2): per-stage Pareto. Named buckets are nanosecond
    // sums across the wave; `total` is the per-repo wall clock. `other` is the
    // DERIVED remainder after every named bucket (see `compute_other_ns`) —
    // now only suppression bookkeeping + tokio handoff/join + trace overhead,
    // which SR-0 expects to be < 5% of `total` at N=40 (AC-5). ms with one
    // decimal so a 12 s wave's breakdown is human-scannable.
    let total_ns = reconcile_start.elapsed().as_nanos();
    let mut profile = RepoStageProfile {
        // Post-install-retirement only the lock-wait, store-persist and total spans
        // have a source; `other_ns` is derived below.
        total_ns,
        lockfile_persist_ns,
        lock_wait_ns,
        ..Default::default()
    };
    // `other` assigned exactly once, from the single-source-of-truth derivation.
    profile.other_ns = profile.compute_other_ns();
    let ms = |ns: u128| (ns as f64) / 1_000_000.0;
    info!(
        target: "content_engine::sync::wave",
        "wave stage profile: repo={} total={:.1}ms lockfile_persist={:.1}ms lock_wait={:.1}ms other={:.1}ms",
        attached.code_repo_path.display(),
        ms(profile.total_ns),
        ms(profile.lockfile_persist_ns),
        ms(profile.lock_wait_ns),
        ms(profile.other_ns),
    );

    // SS-FU-19 AC-1: hand the same per-stage breakdown to the caller so a
    // multi-target wave's Pareto is aggregable in-process (the harness)
    // rather than log-scraped. Only the success path records a profile —
    // an error-aborted target above returns early with no entry.
    stage_profiles.push(profile);

    counts
}

// =============================================================================
// AC-9 / AC-10: event payload helpers + WaveReasonView projection
// =============================================================================

/// AC-9 / AC-10 DRY helper: produce the discriminated-union JSON
/// projection of [`WaveReason`] used in both `sync-wave-started` and
/// `sync-wave-completed` payloads.
pub fn wave_reason_view(reason: &WaveReason) -> serde_json::Value {
    match reason {
        WaveReason::MigrationCutover { .. } => serde_json::json!({
            "kind": "MigrationCutover",
        }),
    }
}

/// AC-10 payload — `sync-wave-started`.
///
/// Iter-5 (Edge HIGH-1): `wave_id` is the same monotonic id used by the
/// `InFlightWave` registry; the UI indicator correlates started/completed
/// pairs by this id so a superseded wave's completed event can't fade
/// the indicator while a later wave is still running.
pub fn emit_wave_started_payload(
    wave_id: u64,
    reason: &WaveReason,
    affected_repos: &[PathBuf],
) -> serde_json::Value {
    serde_json::json!({
        "wave_id": wave_id,
        "reason": wave_reason_view(reason),
        "affected_repo_count": affected_repos.len(),
        "affected_repos": affected_repos,
    })
}

/// AC-9 payload — `sync-wave-completed`. The frontend's `materialize_counts.skipped`
/// is the sum of `skipped_already_current + skipped_locally_modified`
/// (per AC-9c the UI doesn't distinguish).
///
/// Iter-5 (Auditor H2): `affected_repos` is sourced from the wave's
/// full `affected` list (passed in by `Wave::run`), NOT `result.per_repo.keys()`.
/// Cancellation mid-loop shrinks `per_repo`; using the full list keeps
/// the "N" denominator stable across started + completed.
///
/// Iter-5 (Edge HIGH-1): `wave_id` matches the started event for UI correlation.
pub fn emit_wave_completed_payload(
    wave_id: u64,
    reason: &WaveReason,
    affected_repos: &[PathBuf],
    result: &WaveResult,
) -> serde_json::Value {
    let summary = summarize(result);
    serde_json::json!({
        "wave_id": wave_id,
        "reason": wave_reason_view(reason),
        "affected_repos": affected_repos,
        "materialize_counts": {
            "written": summary.written,
            "skipped": summary.skipped,
            "unmaterialized": summary.unmaterialized,
        },
        "errors": result.errors,
        "conflicts": result.conflicts,
        "duration_ms": result.duration_ms,
        "cancelled": result.cancelled,
    })
}

// =============================================================================
// Tests
// =============================================================================

/// The one construction of the sync-wave reconcile fixture, shared by the
/// `dhat-heap` leak gate and the `bench-support` perf bench so neither owns a
/// private copy. Builds an isolated home plus `repo_count` deterministic code
/// repos and returns the unrun [`Wave`] with its coordinator and no-op deps;
/// the caller drives `Wave::run` (and, for the leak gate, its registry probes).
/// `repo_count` is the fan-out axis — reconcile cost is `O(repos)` per
/// RF-perf-1's forward walk.
#[cfg(any(feature = "dhat-heap", feature = "bench-support"))]
pub(crate) fn build_sync_wave_fixture(
    codesense_home: &Path,
    repo_count: usize,
) -> Result<(Wave, Arc<WaveCoordinator>, WaveDeps, Vec<PathBuf>), String> {
    let fixture_root = codesense_home.join("sync-wave-fixture");
    let isolated_home = fixture_root.join("home");
    std::fs::create_dir_all(&isolated_home).map_err(|error| error.to_string())?;
    // A count of 0 is clamped to 1 so the fixture always exercises the wave with
    // at least one repo; callers pass a positive size from the bench matrix.
    let repos = repo_count.max(1);
    let mut repo_paths = Vec::with_capacity(repos);
    let mut affected = Vec::with_capacity(repos);
    for index in 0..repos {
        let code_repo_path = fixture_root.join(format!("code-repo-{index}"));
        std::fs::create_dir_all(&code_repo_path).map_err(|error| error.to_string())?;
        std::fs::write(code_repo_path.join("README.md"), b"sync-wave fixture\n")
            .map_err(|error| error.to_string())?;
        let slug = SourceRepoSlug::new("sync-wave", format!("fixture-{index}"))
            .map_err(|error| error.to_string())?;
        affected.push(AttachedCodeRepo::attached(slug, code_repo_path.clone()));
        repo_paths.push(code_repo_path);
    }
    let context = ResolverContext {
        context_root: None,
        codesense_home: isolated_home,
        user_id: 0,
        org_slug: "sync-wave".to_string(),
        team_name: Some("isolated".to_string()),
    };
    let coordinator = Arc::new(WaveCoordinator::new());
    let dispatcher = Arc::new(super::super::dispatcher::Dispatcher::new(
        0, 100, 60_000, 60_000,
    ));
    let dependencies = WaveDeps {
        suppression: dispatcher.suppression_handle(),
        emit: Arc::new(|_, _| {}),
        cache: None,
        projection_enabled: false,
    };
    let wave = Wave::new(
        WaveReason::MigrationCutover {
            explicit_names: None,
        },
        context,
        affected,
    );
    Ok((wave, coordinator, dependencies, repo_paths))
}

/// Opaque setup/run split for the perf bench. The fixture is built in the
/// benchmark's untimed setup closure; only [`BenchWaveFixture::run`] — the
/// `Wave::run` reconcile — is timed. Lives in this module because the bench
/// crate cannot name `Wave`/`WaveDeps`.
#[cfg(feature = "bench-support")]
pub(crate) struct BenchWaveFixture {
    wave: Wave,
    coordinator: Arc<WaveCoordinator>,
    deps: WaveDeps,
}

#[cfg(feature = "bench-support")]
impl BenchWaveFixture {
    pub(crate) fn build(codesense_home: &Path, repo_count: usize) -> Result<Self, String> {
        let (wave, coordinator, deps, _paths) =
            build_sync_wave_fixture(codesense_home, repo_count)?;
        Ok(Self {
            wave,
            coordinator,
            deps,
        })
    }

    pub(crate) async fn run(self) -> Result<(), String> {
        let result = self.wave.run(1, self.coordinator, self.deps).await;
        if result.errors.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "sync-wave bench fixture failed: {:?}",
                result.errors
            ))
        }
    }
}

#[cfg(feature = "dhat-heap")]
pub(crate) async fn leak_gate_iteration(codesense_home: &Path) -> Result<(), String> {
    let (wave, coordinator, dependencies, repo_paths) = build_sync_wave_fixture(codesense_home, 1)?;
    let code_repo_path = repo_paths[0].clone();
    let baseline = (
        coordinator.wave_target_locks_len(),
        coordinator.repo_locks_len(),
    );
    let normalized = WaveCoordinator::normalize_target_path(&code_repo_path);
    let target_guards = coordinator
        .acquire_wave_target_locks(std::slice::from_ref(&normalized))
        .await;
    if coordinator.wave_target_locks_len().unwrap_or_default() == 0 {
        return Err("sync-wave fixture did not exercise the wave-target registry".to_string());
    }
    drop(target_guards);
    let result = wave.run(1, coordinator.clone(), dependencies).await;
    if !result.errors.is_empty() {
        return Err(format!("sync-wave fixture failed: {:?}", result.errors));
    }
    let exercised = (
        coordinator.wave_target_locks_len().unwrap_or_default(),
        coordinator.repo_locks_len().unwrap_or_default(),
    );
    if exercised.0 == 0 || exercised.1 == 0 {
        return Err(format!(
            "sync-wave fixture did not exercise owned registries: {exercised:?}"
        ));
    }
    coordinator.prune_repo(&code_repo_path);
    if normalized != code_repo_path {
        coordinator.prune_repo(&normalized);
    }
    let cardinalities = (
        coordinator.wave_target_locks_len(),
        coordinator.repo_locks_len(),
    );
    if cardinalities != baseline {
        return Err(format!(
            "sync-wave registries did not return to baseline: baseline={baseline:?}, final={cardinalities:?}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    // The async wave tests hold `test_support::env_guard()` (a process-global
    // serialization Mutex for `CODESENSE_*` env mutation) across `.await`
    // points by design — the guard must cover the whole test, awaits included,
    // to keep concurrent tests from racing the shared env. Dropping it early
    // would defeat that isolation, so the await-holding-lock lint is suppressed
    // for this test module.
    #![allow(clippy::await_holding_lock)]

    use super::super::super::lockfile::Lockfile;
    use super::*;
    use std::time::Instant;

    fn slug() -> SourceRepoSlug {
        SourceRepoSlug::new("alderco", "codesense").unwrap()
    }

    /// SW-IS-3c: seed install-state into the SQLite store (the JSON lockfile
    /// having been retired). Mirrors the just-persisted lockfile so store-backed
    /// readers (`read_through`, `enumerate_*`, `materialized_teams_for_repo`,
    /// `lockfile_mtime`, reap) observe it. `lf.org_slug`/`lf.team_name` must
    /// match the ctx the reader resolves against for the org/team-aware seek.
    fn seed_store(codesense_home: &Path, user_id: u64, slug: &SourceRepoSlug, lf: &Lockfile) {
        let store = crate::content_engine::sync_state_store::SyncStateStore::open(
            crate::content_engine::sync_state_store::default_store_path(codesense_home),
        )
        .unwrap();
        store.upsert(user_id, slug, lf).unwrap();
    }

    // ---------- SLW-RI-5: parallel fan-out ----------

    /// Build a bare multi-repo `MigrationCutover` wave over `n` resolvable temp-dir repos
    /// and run it directly (no coord/live-filter machinery). With `ri5_probe`
    /// armed, `run_repo_dispatch` short-circuits to a synthetic outcome that
    /// records fan-out concurrency — so these tests observe the driver WITHOUT
    /// a real cache/disk materialize. Returns the result plus the `home`
    /// TempDir (kept alive; AC-7 inspects its worktree index) and the repo
    /// TempDirs (kept alive so the paths resolve during `run`).
    async fn run_probe_content_wave(
        n: usize,
        precancel: bool,
    ) -> (WaveResult, tempfile::TempDir, Vec<tempfile::TempDir>) {
        let home = tempfile::TempDir::new().unwrap();
        let ctx = ResolverContext {
            context_root: None,
            codesense_home: home.path().to_path_buf(),
            user_id: 42,
            org_slug: "acme/api".to_string(),
            team_name: Some("default".to_string()),
        };
        let repos: Vec<tempfile::TempDir> =
            (0..n).map(|_| tempfile::TempDir::new().unwrap()).collect();
        let affected = repos
            .iter()
            .map(|r| AttachedCodeRepo::attached(slug(), r.path().to_path_buf()))
            .collect();
        let wave = Wave::new(
            WaveReason::MigrationCutover {
                explicit_names: None,
            },
            ctx,
            affected,
        );
        if precancel {
            wave.cancel.cancel();
        }
        let coord = Arc::new(WaveCoordinator::new());
        let dispatcher = Arc::new(super::super::super::dispatcher::Dispatcher::new(
            0, 100, 60_000, 60_000,
        ));
        let deps = WaveDeps {
            suppression: dispatcher.suppression_handle(),
            emit: Arc::new(|_, _| ()),
            cache: None,
            projection_enabled: false,
        };
        let result = wave.run(7, coord, deps).await;
        (result, home, repos)
    }

    /// AC-4: the content fan-out width mirrors the launch bound — `min(cores,
    /// cap)`, `CS_MATERIALIZE_PARALLELISM` override, clamped to `[1, n]`.
    #[test]
    fn content_fanout_parallelism_is_bounded_and_at_least_one() {
        let _g = super::super::super::test_support::env_guard();
        assert_eq!(content_fanout_parallelism(0), 1);
        assert!((1..=3).contains(&content_fanout_parallelism(3)));
        std::env::set_var("CS_MATERIALIZE_PARALLELISM", "2");
        assert_eq!(content_fanout_parallelism(10), 2, "env override honored");
        assert_eq!(content_fanout_parallelism(1), 1, "clamped to unit count");
        std::env::remove_var("CS_MATERIALIZE_PARALLELISM");
    }

    /// AC-6: reverse / shared-source reasons are NEVER parallelized (width 1,
    /// serial) — RI-4's per-org source serialization is preserved by
    /// construction; forward reasons DO parallelize. The concurrent driver only
    /// ever sees `!writes_shared_source()` reasons.
    #[test]
    fn ri5_reverse_gated_out_and_forward_admitted() {
        let _g = super::super::super::test_support::env_guard();
        // SW-XS-8: no wave writes the shared source any more.
        // Forward multi-repo reason CAN parallelize (bounded by the knob).
        std::env::set_var("CS_MATERIALIZE_PARALLELISM", "4");
        let fwd = WaveReason::MigrationCutover {
            explicit_names: None,
        };
        assert!(!fwd.writes_shared_source());
        assert_eq!(fanout_width_for_reason(&fwd, 8), 4, "forward fans out");
        std::env::remove_var("CS_MATERIALIZE_PARALLELISM");
    }

    /// AC-3: the fold is canonical-`affected`-order and byte-identical
    /// regardless of completion order. `slots` is index-aligned, so folding a
    /// scrambled-completion slotting equals folding an in-order one — `per_repo`,
    /// `errors` (in affected order), and `stage_profiles` all match.
    #[test]
    fn ri5_fanout_output_parity_order_independent() {
        let _g = super::super::super::test_support::env_guard();
        let affected: Vec<AttachedCodeRepo> = (0..4)
            .map(|i| AttachedCodeRepo::attached(slug(), PathBuf::from(format!("/repo/{i}"))))
            .collect();
        let mk = |i: usize| PerRepoOutcome {
            code_repo_path: PathBuf::from(format!("/repo/{i}")),
            counts: Some(RepoWaveCounts {
                written: i,
                ..Default::default()
            }),
            errors: vec![WaveError {
                code_repo_path: PathBuf::from(format!("/repo/{i}")),
                logical_name: None,
                message: format!("e{i}"),
            }],
            stage_profiles: vec![RepoStageProfile {
                total_ns: i as u128,
                ..Default::default()
            }],
            // SW-XS-4: one distinct conflict per repo so the fold's
            // order-independence can be asserted for `conflicts` too.
            conflicts: vec![WaveError {
                code_repo_path: PathBuf::from(format!("/repo/{i}")),
                logical_name: Some(format!("c{i}.md")),
                message: "conflict".into(),
            }],
            reaped: false,
        };
        let emit: Arc<dyn Fn(&'static str, serde_json::Value) + Send + Sync> = Arc::new(|_, _| ());

        // Canonical: slots filled in affected order.
        let mut a = WaveResult::default();
        let slots_a: Vec<Option<PerRepoOutcome>> = (0..4).map(|i| Some(mk(i))).collect();
        fold_per_repo_outcomes(&mut a, &affected, slots_a, &emit);

        // Scrambled completion order — index-slotted, so still canonical.
        let mut slots_b: Vec<Option<PerRepoOutcome>> = (0..4).map(|_| None).collect();
        for &i in &[2usize, 0, 3, 1] {
            slots_b[i] = Some(mk(i));
        }
        let mut b = WaveResult::default();
        fold_per_repo_outcomes(&mut b, &affected, slots_b, &emit);

        assert_eq!(a.per_repo, b.per_repo, "per_repo order-independent");
        assert_eq!(
            a.errors, b.errors,
            "errors fold in affected order regardless of completion"
        );
        assert_eq!(a.stage_profiles, b.stage_profiles);
        assert_eq!(
            a.conflicts, b.conflicts,
            "conflicts fold in affected order regardless of completion"
        );
        assert_eq!(
            a.conflicts
                .iter()
                .map(|c| c.logical_name.clone().unwrap())
                .collect::<Vec<_>>(),
            (0..4).map(|i| format!("c{i}.md")).collect::<Vec<_>>(),
        );
        // AC-3 letter: `summarize` (and thus the `sync-wave-completed` payload it
        // feeds) is a pure fold over `per_repo`, so it is byte-identical too.
        assert_eq!(summarize(&a), summarize(&b), "summary order-independent");
        // The errors are actually in canonical order (e0..e3), not completion.
        let msgs: Vec<&str> = b.errors.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(msgs, vec!["e0", "e1", "e2", "e3"]);
    }

    /// AC-3 (cont.): a `None` slot (a repo never dispatched — cancellation) and
    /// a gone slot (`counts: None`) both fold to "no per_repo entry", while
    /// present repos fold normally.
    #[test]
    fn ri5_fold_skips_absent_and_gone_slots() {
        let _g = super::super::super::test_support::env_guard();
        let affected: Vec<AttachedCodeRepo> = (0..3)
            .map(|i| AttachedCodeRepo::attached(slug(), PathBuf::from(format!("/repo/{i}"))))
            .collect();
        let emit: Arc<dyn Fn(&'static str, serde_json::Value) + Send + Sync> = Arc::new(|_, _| ());
        let slots: Vec<Option<PerRepoOutcome>> = vec![
            // repo 0: materialized
            Some(PerRepoOutcome {
                code_repo_path: PathBuf::from("/repo/0"),
                counts: Some(RepoWaveCounts {
                    written: 5,
                    ..Default::default()
                }),
                errors: vec![],
                stage_profiles: vec![],
                conflicts: vec![],
                reaped: false,
            }),
            // repo 1: never dispatched (cancel)
            None,
            // repo 2: gone path
            Some(PerRepoOutcome {
                code_repo_path: PathBuf::from("/repo/2"),
                counts: None,
                errors: vec![],
                stage_profiles: vec![],
                conflicts: vec![],
                reaped: false,
            }),
        ];
        let mut result = WaveResult::default();
        fold_per_repo_outcomes(&mut result, &affected, slots, &emit);
        assert_eq!(result.per_repo.len(), 1, "only the materialized repo folds");
        assert_eq!(result.per_repo[&PathBuf::from("/repo/0")].written, 5);
    }

    /// AC-2: disjoint worktrees materialize concurrently — repo B starts before
    /// repo A finishes. The instrumented probe records the max concurrent
    /// bodies in flight; with a 4-wide fan-out over 4 repos it exceeds 1.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn ri5_content_fanout_disjoint_repos_overlap() {
        let _g = super::super::super::test_support::env_guard();
        std::env::set_var("CS_MATERIALIZE_PARALLELISM", "4");
        ri5_probe::arm(60);
        let (result, _home, _repos) = run_probe_content_wave(4, false).await;
        let max = ri5_probe::max_inflight();
        ri5_probe::disarm();
        std::env::remove_var("CS_MATERIALIZE_PARALLELISM");
        assert!(!result.cancelled);
        assert_eq!(result.per_repo.len(), 4, "all 4 repos folded");
        assert!(
            max >= 2,
            "repo B started before repo A finished (overlap); max_inflight={max}"
        );
    }

    /// AC-4: in-flight bodies never exceed `min(cores, cap)`. With the knob
    /// pinned to 2 and 6 repos, the fan-out reaches — but never exceeds — 2.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn ri5_fanout_width_bounded() {
        let _g = super::super::super::test_support::env_guard();
        std::env::set_var("CS_MATERIALIZE_PARALLELISM", "2");
        ri5_probe::arm(40);
        let (result, _home, _repos) = run_probe_content_wave(6, false).await;
        let max = ri5_probe::max_inflight();
        ri5_probe::disarm();
        std::env::remove_var("CS_MATERIALIZE_PARALLELISM");
        assert_eq!(result.per_repo.len(), 6);
        assert!(max <= 2, "never exceeds cap=2 in flight; got {max}");
        assert!(max >= 2, "6 repos at cap 2 should reach the cap; got {max}");
    }

    /// AC-5: a pre-cancelled fan-out stops dispatching promptly — no repo body
    /// runs, `per_repo` is empty, and the wave reports `cancelled`. (The forward
    /// content path writes no per-surface status — AC-7 — so there is nothing to
    /// leave `Syncing`; the cancel guarantee reduces to "no work, flagged".)
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn ri5_fanout_cancel_parity() {
        let _g = super::super::super::test_support::env_guard();
        std::env::set_var("CS_MATERIALIZE_PARALLELISM", "4");
        ri5_probe::arm(50);
        let (result, _home, _repos) = run_probe_content_wave(4, true).await;
        let started = ri5_probe::started();
        ri5_probe::disarm();
        std::env::remove_var("CS_MATERIALIZE_PARALLELISM");
        assert!(result.cancelled, "pre-cancelled wave reports cancelled");
        assert!(
            result.per_repo.is_empty(),
            "no repo materialized after pre-cancel"
        );
        assert_eq!(started, 0, "no per-repo body ran under pre-cancel");
    }

    /// AC-5 (mid-flight): a cancel that fires WHILE bodies are in flight lets the
    /// already-dispatched repos drain + fold (matching the serial break's
    /// post-repo insert) while not-yet-started repos short-circuit. Crucially the
    /// wave still reports `cancelled` even when every in-flight future was
    /// admitted before the cancel (so none hit the entry gate) — the guard the
    /// concurrent `cancelled |= token` fold restores. Without it a superseded
    /// wide wave would mislabel as synced.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn ri5_fanout_cancel_midflight_drains_inflight() {
        use std::time::Duration;
        let _g = super::super::super::test_support::env_guard();
        std::env::set_var("CS_MATERIALIZE_PARALLELISM", "2"); // width 2
        ri5_probe::arm(200); // long body so we can cancel mid-flight

        let home = tempfile::TempDir::new().unwrap();
        let ctx = ResolverContext {
            context_root: None,
            codesense_home: home.path().to_path_buf(),
            user_id: 42,
            org_slug: "acme/api".to_string(),
            team_name: Some("default".to_string()),
        };
        let repos: Vec<tempfile::TempDir> =
            (0..6).map(|_| tempfile::TempDir::new().unwrap()).collect();
        let affected = repos
            .iter()
            .map(|r| AttachedCodeRepo::attached(slug(), r.path().to_path_buf()))
            .collect();
        let wave = Wave::new(
            WaveReason::MigrationCutover {
                explicit_names: None,
            },
            ctx,
            affected,
        );
        let token = wave.cancel.clone();
        let coord = Arc::new(WaveCoordinator::new());
        let dispatcher = Arc::new(super::super::super::dispatcher::Dispatcher::new(
            0, 100, 60_000, 60_000,
        ));
        let deps = WaveDeps {
            suppression: dispatcher.suppression_handle(),
            emit: Arc::new(|_, _| ()),
            cache: None,
            projection_enabled: false,
        };
        let handle = tokio::spawn(async move { wave.run(7, coord, deps).await });

        // Wait until at least one body is in flight, then cancel.
        while ri5_probe::started() == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        token.cancel();
        let result = handle.await.unwrap();
        let started = ri5_probe::started();
        ri5_probe::disarm();
        std::env::remove_var("CS_MATERIALIZE_PARALLELISM");

        assert!(
            result.cancelled,
            "mid-flight cancel must set cancelled even if no future hit the entry gate"
        );
        // Every body that STARTED drained and folded; nothing lost, nothing extra.
        assert_eq!(
            result.per_repo.len(),
            started,
            "in-flight repos drain + fold (per_repo == started)"
        );
        assert!(started >= 1, "at least the initial width was in flight");
        assert!(started < 6, "cancel stopped some repos from dispatching");
    }

    #[test]
    fn supersedes_migration_cutover_supersedes_everything() {
        let _g = super::super::super::test_support::env_guard();
        let a = WaveReason::MigrationCutover {
            explicit_names: None,
        };
        for b in [
            WaveReason::MigrationCutover {
                explicit_names: None,
            },
            WaveReason::MigrationCutover {
                explicit_names: Some(vec!["x".into()]),
            },
        ] {
            assert!(a.supersedes(&b), "MigrationCutover must supersede: {:?}", b);
        }
    }

    #[test]
    fn attached_context_detach_cancels_only_owned_waves() {
        let coord = Arc::new(WaveCoordinator::new());
        let home = PathBuf::from("/tmp/attachment-detach-home");
        let ctx = ResolverContext {
            context_root: None,
            codesense_home: home.clone(),
            user_id: 42,
            org_slug: "acme/context".into(),
            team_name: Some("default".into()),
        };
        coord.stash_active_projection_on_team_change(&ctx, &[]);
        let ours = CancellationToken::new();
        let other = CancellationToken::new();
        for (slug, cancel) in [
            ("acme/context", ours.clone()),
            ("other/context", other.clone()),
        ] {
            coord.in_flight.lock().unwrap().push(InFlightWave {
                context: Some((home.clone(), "42".into(), slug.into())),
                wave_id: 1,
                reason: WaveReason::MigrationCutover {
                    explicit_names: None,
                },
                trigger: WaveTrigger::Unspecified,
                cancel,
                affected: vec![],
            });
        }
        coord.detach_context(&home, "42", "acme/context");
        assert!(ours.is_cancelled());
        assert!(!other.is_cancelled());
        assert!(coord.active_projection_target.lock().unwrap().is_none());
    }

    #[test]
    fn profile_switch_cancels_only_old_principal_waves() {
        let coord = Arc::new(WaveCoordinator::new());
        let home = PathBuf::from("/tmp/attachment-detach-home");
        let ctx = ResolverContext {
            context_root: None,
            codesense_home: home.clone(),
            user_id: 42,
            org_slug: "acme/context".into(),
            team_name: Some("default".into()),
        };
        coord.stash_active_projection_on_team_change(&ctx, &[]);
        let ours = CancellationToken::new();
        let other = CancellationToken::new();
        for (principal, cancel) in [("42", ours.clone()), ("43", other.clone())] {
            coord.in_flight.lock().unwrap().push(InFlightWave {
                context: Some((home.clone(), principal.into(), "acme/context".into())),
                wave_id: 1,
                reason: WaveReason::MigrationCutover {
                    explicit_names: None,
                },
                trigger: WaveTrigger::Unspecified,
                cancel,
                affected: vec![],
            });
        }
        coord.detach_principal("42");
        assert!(ours.is_cancelled());
        assert!(!other.is_cancelled());
        assert!(coord.active_projection_target.lock().unwrap().is_none());
    }

    // ---------- WaveCoordinator basics ----------

    #[tokio::test]
    async fn coordinator_acquire_repo_lock_serializes_within_same_repo() {
        let _g = super::super::super::test_support::env_guard();
        let coord = Arc::new(WaveCoordinator::new());
        let path = PathBuf::from("/tmp/sync_test_repo_a");

        let g1 = coord.acquire_repo_lock(&path).await;
        let coord_clone = Arc::clone(&coord);
        let path_clone = path.clone();
        let acquired_2nd = Arc::new(StdMutex::new(false));
        let acquired_2nd_clone = Arc::clone(&acquired_2nd);
        let handle = tokio::spawn(async move {
            let _g2 = coord_clone.acquire_repo_lock(&path_clone).await;
            *acquired_2nd_clone.lock().unwrap() = true;
        });

        // Give the second acquire a chance to (try to) progress.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            !(*acquired_2nd.lock().unwrap()),
            "second lock must block while first is held"
        );

        drop(g1);
        handle.await.unwrap();
        assert!(*acquired_2nd.lock().unwrap());
    }

    #[tokio::test]
    async fn coordinator_acquire_repo_lock_does_not_block_across_repos() {
        let _g = super::super::super::test_support::env_guard();
        let coord = Arc::new(WaveCoordinator::new());
        let path_a = PathBuf::from("/tmp/sync_test_repo_a");
        let path_b = PathBuf::from("/tmp/sync_test_repo_b");

        let _g_a = coord.acquire_repo_lock(&path_a).await;

        // Different repo's lock should be acquirable immediately.
        let acquired = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            coord.acquire_repo_lock(&path_b),
        )
        .await;
        assert!(
            acquired.is_ok(),
            "different-repo lock must not block on a held repo lock"
        );
    }

    /// SS-FU-7 AC-7: regression guard. The per-path lockfile substrate
    /// change keys lockfile paths by `(team, slug, code_repo_path)`;
    /// `WaveCoordinator::acquire_repo_lock` was already path-keyed
    /// pre-SS-FU-7, but this test pins that semantic now that two
    /// `AttachedCodeRepo` entries sharing the same slug but different
    /// `code_repo_path` (main + worktree of the same source repo) is
    /// a first-class supported shape. Each entry must acquire a
    /// distinct lock; holding main's lock must NOT block the worktree
    /// from acquiring its own.
    #[tokio::test]
    async fn ssfu7_per_path_lock_isolates_main_and_worktree_for_same_slug() {
        let _g = super::super::super::test_support::env_guard();
        use super::super::super::lockfile::AttachedCodeRepo;
        use crate::domain::SourceRepoSlug;

        let coord = Arc::new(WaveCoordinator::new());
        let slug = SourceRepoSlug::new("alderco", "codesense").unwrap();
        let main = AttachedCodeRepo::attached(slug.clone(), PathBuf::from("/repos/codesense"));
        let worktree =
            AttachedCodeRepo::attached(slug.clone(), PathBuf::from("/repos/codesense-feat-x"));

        // Same slug — different code_repo_path. Acquire main's lock and
        // hold it; the worktree's lock must be acquirable immediately
        // because the lock map is keyed by code_repo_path, not slug.
        let _g_main = coord.acquire_repo_lock(&main.code_repo_path).await;

        let acquired = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            coord.acquire_repo_lock(&worktree.code_repo_path),
        )
        .await;
        assert!(
            acquired.is_ok(),
            "worktree lock must not block on a held main-repo lock for the same slug"
        );

        // Symmetric assertion — same-path re-acquisition WOULD block,
        // proving the lock IS path-scoped (not slug-scoped).
        let blocked = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            coord.acquire_repo_lock(&main.code_repo_path),
        )
        .await;
        assert!(
            blocked.is_err(),
            "same-path re-acquisition must block while main's guard is alive"
        );
    }

    // ---------- SLW-RI-4: keyed wave-target locks + engine semaphore ----------

    /// AC-2: a wave holding worktree A's target lock does NOT block a wave for a
    /// disjoint worktree B — the core throughput win (disjoint derives run
    /// concurrently on the engine runtime), the exact case the whole-engine
    /// `global_lock` serialized.
    #[tokio::test]
    async fn ri4_disjoint_target_waves_do_not_block() {
        let _g = super::super::super::test_support::env_guard();
        let coord = Arc::new(WaveCoordinator::new());
        let a = vec![PathBuf::from("/ri4/wt-a")];
        let b = vec![PathBuf::from("/ri4/wt-b")];

        let _held_a = coord.acquire_wave_target_locks(&a).await;
        let acquired_b = tokio::time::timeout(
            Duration::from_millis(100),
            coord.acquire_wave_target_locks(&b),
        )
        .await;
        assert!(
            acquired_b.is_ok(),
            "a wave for a disjoint target must not wait on another target's wave lock"
        );
    }

    /// RF-5 (AC-2): the deadlock-freedom property both wave-target lock paths
    /// rest on is a canonical TOTAL order. `canonical_lock_keys` must return its
    /// input **sorted and deduped** regardless of input order or duplicates, so
    /// the blocking ([`WaveCoordinator::acquire_wave_target_locks`]) and
    /// non-blocking ([`WaveCoordinator::try_acquire_wave_target_locks`]) paths
    /// acquire in one agreed order and can never AB-BA deadlock (AC-5). The
    /// behavioral deadlock tests exercise this only indirectly; this pins it.
    #[test]
    fn canonical_lock_keys_sorts_and_dedups() {
        let _g = super::super::super::test_support::env_guard();
        let keys = vec![
            PathBuf::from("/z/last"),
            PathBuf::from("/a/first"),
            PathBuf::from("/m/mid"),
            PathBuf::from("/a/first"), // duplicate of an earlier key
            PathBuf::from("/m/mid"),   // duplicate of an earlier key
        ];
        let canon = WaveCoordinator::canonical_lock_keys(&keys);
        assert_eq!(
            canon,
            vec![
                PathBuf::from("/a/first"),
                PathBuf::from("/m/mid"),
                PathBuf::from("/z/last"),
            ],
            "canonical lock order must be sorted + deduped"
        );
    }

    /// AC-3: two waves sharing a target serialize on that target's mutex — the
    /// second blocks until the first releases, exactly as `global_lock` ordered
    /// them. This is what preserves per-surface `Syncing→Synced` status-write
    /// ordering per key.
    #[tokio::test]
    async fn ri4_same_target_waves_serialize() {
        let _g = super::super::super::test_support::env_guard();
        let coord = Arc::new(WaveCoordinator::new());
        let key = vec![PathBuf::from("/ri4/wt-shared")];

        let held = coord.acquire_wave_target_locks(&key).await;
        let coord2 = Arc::clone(&coord);
        let key2 = key.clone();
        let acquired = Arc::new(StdMutex::new(false));
        let acquired2 = Arc::clone(&acquired);
        let h = tokio::spawn(async move {
            let _g = coord2.acquire_wave_target_locks(&key2).await;
            *acquired2.lock().unwrap() = true;
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !*acquired.lock().unwrap(),
            "second same-target wave must block while the first holds the key"
        );
        drop(held);
        h.await.unwrap();
        assert!(
            *acquired.lock().unwrap(),
            "second same-target wave proceeds once the first releases"
        );
    }

    /// AC-9: a multi-target structural fan-out (keys {A,B,C}, modeling
    /// the retired structural fan-out) serializes any wave sharing one of its
    /// targets ({B}) — the "no concurrent wave re-materializes the removed
    /// source" guarantee — while a wave for a disjoint target ({D}) runs
    /// concurrently.
    #[tokio::test]
    async fn ri4_multi_target_shares_serialize_disjoint_concurrent() {
        let _g = super::super::super::test_support::env_guard();
        let coord = Arc::new(WaveCoordinator::new());
        let fanout = vec![
            PathBuf::from("/ri4/a"),
            PathBuf::from("/ri4/b"),
            PathBuf::from("/ri4/c"),
        ];
        let _held = coord.acquire_wave_target_locks(&fanout).await;

        // Shares B → must block.
        let shares = tokio::time::timeout(
            Duration::from_millis(80),
            coord.acquire_wave_target_locks(&[PathBuf::from("/ri4/b")]),
        )
        .await;
        assert!(
            shares.is_err(),
            "a wave sharing a fan-out target must serialize behind it"
        );

        // Disjoint D → must proceed.
        let disjoint = tokio::time::timeout(
            Duration::from_millis(80),
            coord.acquire_wave_target_locks(&[PathBuf::from("/ri4/d")]),
        )
        .await;
        assert!(
            disjoint.is_ok(),
            "a wave for a target disjoint from the fan-out must not block"
        );
    }

    /// AC-5: two overlapping multi-target waves ({A,B} and {B,C}) both make
    /// progress — canonical sorted acquisition prevents an AB-BA deadlock. A
    /// naive arrival-order acquisition would let this hang.
    #[tokio::test]
    async fn ri4_overlapping_multi_target_waves_no_deadlock() {
        let _g = super::super::super::test_support::env_guard();
        let coord = Arc::new(WaveCoordinator::new());
        let ab = vec![PathBuf::from("/ri4/a"), PathBuf::from("/ri4/b")];
        // Deliberately unsorted input — the accessor sorts, so both waves still
        // acquire in the same canonical order (a → b → c).
        let cb = vec![PathBuf::from("/ri4/c"), PathBuf::from("/ri4/b")];

        let c1 = Arc::clone(&coord);
        let c2 = Arc::clone(&coord);
        let t1 = tokio::spawn(async move {
            let _g = c1.acquire_wave_target_locks(&ab).await;
            tokio::time::sleep(Duration::from_millis(10)).await;
        });
        let t2 = tokio::spawn(async move {
            let _g = c2.acquire_wave_target_locks(&cb).await;
            tokio::time::sleep(Duration::from_millis(10)).await;
        });
        let both = tokio::time::timeout(Duration::from_secs(2), async {
            t1.await.unwrap();
            t2.await.unwrap();
        })
        .await;
        assert!(
            both.is_ok(),
            "overlapping multi-target waves must not deadlock under sorted acquisition"
        );
    }

    /// AC-10: a reverse-promote wave (keys {originator}) and the forward-fan
    /// wave it triggers (keys {originator, sibling}) share the originator key
    /// and therefore serialize — the structural echo-prevention guarantee
    /// (the forward fan waits for the reverse promote's baseline, so it
    /// classifies the originator `Matches → no-op`, never a re-promote).
    #[tokio::test]
    async fn ri4_reverse_and_forward_fan_share_originator_key_serialize() {
        let _g = super::super::super::test_support::env_guard();
        let coord = Arc::new(WaveCoordinator::new());
        let originator = PathBuf::from("/ri4/originator");
        let reverse = vec![originator.clone()];
        let forward = vec![originator.clone(), PathBuf::from("/ri4/sibling")];

        let _reverse_held = coord.acquire_wave_target_locks(&reverse).await;
        let forward_blocked = tokio::time::timeout(
            Duration::from_millis(80),
            coord.acquire_wave_target_locks(&forward),
        )
        .await;
        assert!(
            forward_blocked.is_err(),
            "a forward fan sharing the originator key must serialize behind the reverse promote"
        );
    }

    /// AC-9 (source write-set): `writes_shared_source` is true for exactly the
    /// reverse reasons that promote/adopt into the shared context repo — the
    /// waves whose `affected` (dest-only) key-set under-approximates their true
    /// write-set — and false for source *readers* (forward / derive / switch).
    #[test]
    fn ri4_writes_shared_source_predicate() {
        let _g = super::super::super::test_support::env_guard();
        // SW-XS-8: the agent reconcile (the last shared-source writer) is gone.
        // Source readers must NOT take the source key (they'd needlessly
        // serialize against reverse writers, killing forward throughput).
        assert!(!WaveReason::MigrationCutover {
            explicit_names: None
        }
        .writes_shared_source());
    }

    /// AC-9 / RS-2 echo-safety: two reverse waves for DIFFERENT dest worktrees
    /// that promote the SAME shared source must serialize — otherwise both read
    /// the old source, both classify `matches → ReverseWrite`, and the second's
    /// divergent edit is silently clobbered instead of surfaced as a Conflict
    /// (the regression the old whole-engine `global_lock` prevented). They share
    /// the per-org source-root key even though their dest keys are disjoint.
    #[tokio::test]
    async fn ri4_reverse_waves_same_source_serialize_via_org_key() {
        let _g = super::super::super::test_support::env_guard();
        let coord = Arc::new(WaveCoordinator::new());
        let org_root = PathBuf::from("/ri4/orgs/acme");
        // Reverse wave for dest A: keys {destA, orgRoot}.
        let reverse_a = vec![PathBuf::from("/ri4/wt-a"), org_root.clone()];
        // Reverse wave for dest B (disjoint dest): keys {destB, orgRoot}.
        let reverse_b = vec![PathBuf::from("/ri4/wt-b"), org_root.clone()];

        let _held_a = coord.acquire_wave_target_locks(&reverse_a).await;
        // Despite disjoint DEST keys, the shared org-root key must serialize B.
        let b_blocked = tokio::time::timeout(
            Duration::from_millis(80),
            coord.acquire_wave_target_locks(&reverse_b),
        )
        .await;
        assert!(
            b_blocked.is_err(),
            "same-source reverse writers must serialize on the org-root key"
        );

        // A reverse wave for a DIFFERENT org (disjoint org key) stays concurrent.
        let reverse_other_org = vec![PathBuf::from("/ri4/wt-c"), PathBuf::from("/ri4/orgs/other")];
        let other_ok = tokio::time::timeout(
            Duration::from_millis(80),
            coord.acquire_wave_target_locks(&reverse_other_org),
        )
        .await;
        assert!(
            other_ok.is_ok(),
            "a reverse writer for a different org must not serialize on another org's source"
        );
    }

    /// AC-1: an empty key-set (a wave that mutates no per-target surface — e.g.
    /// a benign-empty content event) acquires nothing and never blocks; its only
    /// shared state is the snapshot/RCU cache (SLW-RI-3).
    #[tokio::test]
    async fn ri4_empty_key_set_acquires_nothing() {
        let _g = super::super::super::test_support::env_guard();
        let coord = Arc::new(WaveCoordinator::new());
        let guards = coord.acquire_wave_target_locks(&[]).await;
        assert!(guards.is_empty(), "empty key-set acquires no locks");
        let other = tokio::time::timeout(
            Duration::from_millis(80),
            coord.acquire_wave_target_locks(&[PathBuf::from("/ri4/x")]),
        )
        .await;
        assert!(
            other.is_ok(),
            "an empty-key wave must not block any target's wave"
        );
    }

    /// AC-7: the engine semaphore is sized to the engine worker pool and bounds
    /// concurrent wave bodies — once all permits are out, the next acquire
    /// blocks, so a wave-storm (40-worktree launch) cannot exceed the pool.
    #[tokio::test]
    async fn ri4_engine_semaphore_bounds_concurrency() {
        let _g = super::super::super::test_support::env_guard();
        let coord = Arc::new(WaveCoordinator::new());
        let n = engine_worker_count();
        assert_eq!(
            coord.engine_semaphore.available_permits(),
            n,
            "semaphore is sized to the engine worker count"
        );
        let mut held = Vec::new();
        for _ in 0..n {
            held.push(
                Arc::clone(&coord.engine_semaphore)
                    .acquire_owned()
                    .await
                    .unwrap(),
            );
        }
        assert_eq!(coord.engine_semaphore.available_permits(), 0);
        let over = tokio::time::timeout(
            Duration::from_millis(80),
            Arc::clone(&coord.engine_semaphore).acquire_owned(),
        )
        .await;
        assert!(
            over.is_err(),
            "no wave permit is available beyond the engine pool size"
        );
        drop(held);
    }

    // ---------- SW-FX-50: panic-safe permit + in_flight release ----------

    /// A minimal cache-free `WaveDeps` carrying the given `emit` — the shared
    /// harness for the miss-path panic tests.
    fn test_miss_deps(
        emit: Arc<dyn Fn(&'static str, serde_json::Value) + Send + Sync>,
    ) -> WaveDeps {
        let dispatcher = Arc::new(super::super::super::dispatcher::Dispatcher::new(
            0, 100, 60_000, 60_000,
        ));
        WaveDeps {
            suppression: dispatcher.suppression_handle(),
            emit,
            cache: None,
            projection_enabled: false,
        }
    }

    /// `WaveDeps` whose `emit` PANICS on `sync-wave-completed` — a genuine
    /// wave-task panic INSIDE `Wave::run`, after `run_wave` has already acquired
    /// the engine permit and registered the `InFlightGuard`. `started` is emitted
    /// synchronously in `enqueue` on the caller thread, so gating the panic on
    /// `completed` keeps the caller intact and blows up only the task.
    fn panic_on_completed_deps() -> WaveDeps {
        test_miss_deps(Arc::new(|event, _| {
            if event == EVENT_WAVE_COMPLETED {
                panic!("SW-FX-50: injected wave-task panic");
            }
        }))
    }

    /// A benign no-op `WaveDeps` — used to prove a permit freed by a prior
    /// panicking wave is reusable (the wave runs to completion).
    fn noop_deps() -> WaveDeps {
        test_miss_deps(Arc::new(|_, _| ()))
    }

    fn single_repo_wave(codesense_home: &Path, workspace_dir: PathBuf) -> Wave {
        let ctx = ResolverContext {
            context_root: None,
            codesense_home: codesense_home.to_path_buf(),
            user_id: 42,
            org_slug: "test/api".to_string(),
            team_name: Some("default".to_string()),
        };
        let affected = vec![AttachedCodeRepo::attached(slug(), workspace_dir.clone())];
        Wave::new(
            WaveReason::MigrationCutover {
                explicit_names: None,
            },
            ctx,
            affected,
        )
    }

    /// SW-FX-50 (AC-3): a wave whose body PANICS must release
    /// BOTH the engine-semaphore permit AND its `in_flight` registry entry — the
    /// permit is an `OwnedSemaphorePermit` local and the `InFlightGuard` is a
    /// stack guard in `run_wave`, so Rust's Drop-on-unwind reclaims both (there is
    /// no `panic = "abort"`). Locks that invariant against a future refactor that
    /// might, say, hold the permit in a field the panic path skips.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wave_panic_releases_permit_and_in_flight() {
        let _g = super::super::super::test_support::env_guard();
        let coord = Arc::new(WaveCoordinator::new());
        let n = engine_worker_count();
        assert_eq!(coord.engine_semaphore.available_permits(), n);

        let home = tempfile::TempDir::new().unwrap();
        let wave = single_repo_wave(
            home.path(),
            home.path().join("users/42/_epic/test/api/EPIC-1"),
        );

        // The task panics inside `run`; the join handle yields the panic as Err.
        let joined = coord.enqueue(wave, panic_on_completed_deps()).await;
        assert!(
            joined.is_err(),
            "the injected panic must surface as a JoinError"
        );

        // Drop-on-unwind must have returned the permit and drained the entry.
        assert_eq!(
            coord.engine_semaphore.available_permits(),
            n,
            "the engine permit leaked across the wave-task panic"
        );
        assert!(
            coord.in_flight.lock().unwrap().is_empty(),
            "the in_flight entry leaked across the wave-task panic"
        );
    }

    /// SW-FX-50 (AC-4): a long/high-wave-count session must not starve. Run
    /// `n + 1` consecutive panicking waves (n = pool size) — if any panic leaked
    /// its permit, the pool would drain and a later `run_wave` would park forever
    /// on `acquire_engine_permit_and_keys`, so the awaited handle would hang. Each
    /// completes under a timeout, and a final benign wave still acquires a permit
    /// and runs — proving permits are reclaimed across the panic storm.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn repeated_wave_panics_do_not_starve_the_engine_pool() {
        let _g = super::super::super::test_support::env_guard();
        let coord = Arc::new(WaveCoordinator::new());
        let n = engine_worker_count();
        let home = tempfile::TempDir::new().unwrap();

        for i in 0..=n {
            let wave = single_repo_wave(
                home.path(),
                home.path()
                    .join(format!("users/42/_epic/test/api/EPIC-{i}")),
            );
            let joined = tokio::time::timeout(
                Duration::from_secs(10),
                coord.enqueue(wave, panic_on_completed_deps()),
            )
            .await;
            assert!(
                joined.is_ok(),
                "wave {i} hung — a prior panic leaked a permit and starved the pool"
            );
            assert!(joined.unwrap().is_err(), "wave {i} was expected to panic");
        }

        // Pool fully reclaimed, and a benign wave still runs to completion.
        assert_eq!(coord.engine_semaphore.available_permits(), n);
        let benign = single_repo_wave(home.path(), home.path().join("users/42/_epic/test/api/OK"));
        let ran = tokio::time::timeout(Duration::from_secs(10), coord.enqueue(benign, noop_deps()))
            .await
            .expect("the post-storm wave must not hang");
        assert!(
            ran.is_ok(),
            "a fresh wave must acquire a permit after the panic storm"
        );
    }

    // ---------- SLW-RI-6: batch-open no-permit-squat ----------

    /// SLW-RI-6 (AC-3) — the permit-occupancy invariant, the RED-on-baseline
    /// test. A single engine permit; one wave (B) blocked on a contended target
    /// key. The invariant: **no permit is ever occupied by a wave blocked on a
    /// key**. On the BASELINE acquire order (`permit.acquire().await` then a
    /// BLOCKING `acquire_wave_target_locks`) B grabs the only permit and then
    /// parks on the key forever-until-freed, so the permit is squatted for the
    /// whole window — the pool's effective width collapses to zero. With
    /// `acquire_engine_permit_and_keys` B releases the permit on every failed
    /// try, so the permit is free for (almost) the whole window.
    ///
    /// We assert the invariant *directly* (does a permit ever free while B is
    /// blocked on the key?) rather than racing a second wave for it — the spike
    /// established the baseline is a throughput-collapse, NOT a deadlock, so a
    /// "second wave eventually gets through" assertion is loose (it does, given
    /// time) and permit-race-flaky. Direct observation is deterministic:
    /// baseline holds the permit stably at 0-available; the fix frees it every
    /// backoff cycle. (Verified RED against a temporary baseline-squat build of
    /// `acquire_engine_permit_and_keys`; GREEN with the shipped requeue.)
    #[tokio::test]
    async fn slw_ri6_permit_not_squatted_while_blocked_on_key() {
        let _g = super::super::super::test_support::env_guard();
        let coord = Arc::new(WaveCoordinator::new());
        // A one-permit pool — the smallest pool that makes squatting fatal.
        let sem = Arc::new(Semaphore::new(1));
        let contended = PathBuf::from("/ri6/contended");

        // Stand-in for a running narrow `WorktreeDerive`: hold the contended
        // key (but NOT the permit — the point is the *blocked* wave must not
        // hold the permit either).
        let held_key = coord
            .acquire_wave_target_locks(std::slice::from_ref(&contended))
            .await;

        // Wave B wants {permit, contended}. The key is held → under the no-squat
        // discipline it must take the permit, fail the `try_lock`, and RELEASE
        // the permit before backing off — never squat it.
        let coord_b = Arc::clone(&coord);
        let sem_b = Arc::clone(&sem);
        let key_b = contended.clone();
        let b = tokio::spawn(async move {
            let _held = coord_b
                .acquire_engine_permit_and_keys(&sem_b, &[key_b])
                .await;
        });

        // Let B get scheduled and settle into its acquire loop (or, on baseline,
        // into its permanent squat). 50ms is ages for a tokio spawn.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // THE INVARIANT: while B is blocked on the contended key, does the engine
        // permit ever become available? Baseline squat → never (B holds it the
        // whole window); fix → yes (freed on every backoff cycle). Sampling the
        // max availability over the window removes the sub-ms try window flake.
        let mut ever_free = false;
        for _ in 0..200 {
            if sem.available_permits() > 0 {
                ever_free = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert!(
            ever_free,
            "permit-squat: a wave blocked on a contended key held the only engine \
             permit for the entire window — the pool's effective width collapsed \
             to zero. The fix must release the permit while requeuing."
        );

        // Release the contended key; B must then complete (drains, no deadlock).
        drop(held_key);
        tokio::time::timeout(Duration::from_secs(2), b)
            .await
            .expect("blocked wave completes once the contended key frees")
            .expect("B task did not panic");
    }

    /// SLW-RI-6 (review R1-HIGH) — the anti-starvation escalation. Pure
    /// `try_lock`+backoff cannot guarantee a broad wave ever wins against
    /// sustained key contention on a >1-permit pool; after
    /// `REQUEUE_STARVATION_THRESHOLD` failed cycles the wave escalates to the
    /// FAIR blocking acquire. This exercises that branch on a 2-permit pool: the
    /// wave's key is pinned past the fast-path spin window, forcing escalation,
    /// then released — the wave must acquire (park FIFO on the mutex → handed the
    /// key on release) and complete holding its key, never spin forever. (A unit
    /// test can't deterministically reproduce *sustained* starvation without
    /// racy driver load; the guarantee is by construction — a fair blocking
    /// acquire is FIFO — and this pins that the escalation path is reachable and
    /// yields the key.)
    #[tokio::test]
    async fn slw_ri6_starved_wave_escalates_to_fair_acquire() {
        let _g = super::super::super::test_support::env_guard();
        let coord = Arc::new(WaveCoordinator::new());
        // >1 permit — the regime where pure try+backoff can starve a broad wave.
        let sem = Arc::new(Semaphore::new(2));
        let k = PathBuf::from("/ri6/starve");

        // Pin the wave's key past the ~200ms fast-path spin window so the wave
        // exhausts its `try_lock` budget and escalates to the fair acquire.
        let held = coord
            .acquire_wave_target_locks(std::slice::from_ref(&k))
            .await;

        let coord_b = Arc::clone(&coord);
        let sem_b = Arc::clone(&sem);
        let kb = k.clone();
        let b = tokio::spawn(
            async move { coord_b.acquire_engine_permit_and_keys(&sem_b, &[kb]).await },
        );

        // Hold well past the spin window (32 attempts over the 250µs→8ms ramp ≈
        // 200ms), then release into the escalated wave's fair blocking wait.
        tokio::time::sleep(Duration::from_millis(400)).await;
        drop(held);

        let done = tokio::time::timeout(Duration::from_secs(2), b).await;
        let (_permit, guards) = done
            .expect("escalated wave must complete via the fair acquire, not starve")
            .expect("wave task did not panic");
        assert_eq!(guards.len(), 1, "escalated wave holds its target key");
    }

    /// SLW-RI-6 (AC-7 capstone / AC-4 liveness) — the N≥20 open burst always
    /// drains, no loss. One broad wave contending N narrow keys + N narrow waves
    /// each holding one key, on a one-permit pool: ALL N+1 waves must acquire
    /// {permit, keys} and complete within a wall-clock bound. This is the
    /// coordinator-level half of the AC-7 harness (the app-level "all N
    /// `_bmad-output` trees materialize" check is inherently an e2e/manual
    /// artifact — a headless unit test has no worktrees to materialize). Passes
    /// on BOTH baseline and fix (baseline drains — slowly — it does not deadlock;
    /// see spike); its job is to pin that the no-squat requeue never *loses* or
    /// livelocks a wave under a realistic ≥20 burst.
    #[tokio::test]
    async fn slw_ri6_burst_drains_within_bound() {
        let _g = super::super::super::test_support::env_guard();
        let coord = Arc::new(WaveCoordinator::new());
        let sem = Arc::new(Semaphore::new(1));
        let n = 24usize; // AC-7: N ≥ 20 opens.
        let keys: Vec<PathBuf> = (0..n)
            .map(|i| PathBuf::from(format!("/ri6/burst/{i}")))
            .collect();

        // The broad wave contends every narrow key.
        let coord_broad = Arc::clone(&coord);
        let sem_broad = Arc::clone(&sem);
        let broad_keys = keys.clone();
        let broad = tokio::spawn(async move {
            let _held = coord_broad
                .acquire_engine_permit_and_keys(&sem_broad, &broad_keys)
                .await;
        });

        // N narrow waves, each one key. They and the broad wave all share the
        // one permit; none may be squatted out.
        let mut narrow = Vec::new();
        for key in keys {
            let coord_n = Arc::clone(&coord);
            let sem_n = Arc::clone(&sem);
            narrow.push(tokio::spawn(async move {
                let _held = coord_n.acquire_engine_permit_and_keys(&sem_n, &[key]).await;
                // Trivial "work" then release both.
                tokio::task::yield_now().await;
            }));
        }

        let all = tokio::time::timeout(Duration::from_secs(5), async {
            broad.await.unwrap();
            for t in narrow {
                t.await.unwrap();
            }
        })
        .await;
        assert!(
            all.is_ok(),
            "a burst of one broad + N narrow contenders on a 1-permit pool must \
             all drain within the bound (no permit-squat collapse)"
        );
    }

    // ---------- SW-WL-2: wave-in-flight gate ----------

    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A recording detach: returns a counter + a [`DetachFn`] that bumps
    /// it. Stand-in for the watcher-side `remove_watch_inner` replay so the
    /// gate is testable without a Tauri runtime (the D-17 DI seam).
    fn recording_detach() -> (Arc<AtomicUsize>, DetachFn) {
        let count = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&count);
        let detach: DetachFn = Arc::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
        });
        (count, detach)
    }

    /// Push a synthetic in-flight wave targeting `affected` (normalized as
    /// `enqueue` would). Returns the `wave_id` so the test can later drop a
    /// matching [`InFlightGuard`] to simulate the wave draining.
    fn push_in_flight(
        coord: &Arc<WaveCoordinator>,
        wave_id: u64,
        affected: &[&Path],
    ) -> Vec<PathBuf> {
        let norm: Vec<PathBuf> = affected
            .iter()
            .map(|p| WaveCoordinator::normalize_target_path(p))
            .collect();
        coord.in_flight.lock().unwrap().push(InFlightWave {
            context: None,
            wave_id,
            reason: WaveReason::MigrationCutover {
                explicit_names: None,
            },
            trigger: WaveTrigger::Unspecified,
            cancel: CancellationToken::new(),
            affected: norm.clone(),
        });
        norm
    }

    /// Drop a guard matching a pushed wave — fires the drain path exactly
    /// as the spawned wave task's guard does on completion/panic/cancel.
    fn drain_wave(coord: &Arc<WaveCoordinator>, wave_id: u64, affected: Vec<PathBuf>) {
        drop(InFlightGuard {
            coord: Arc::clone(coord),
            wave_id,
            affected,
        });
    }

    fn pending_len(coord: &Arc<WaveCoordinator>) -> usize {
        coord.pending_detach.lock().unwrap().len()
    }

    /// Number of distinct subscribers (contexts) deferred at `target`.
    fn pending_contexts(coord: &Arc<WaveCoordinator>, target: &Path) -> usize {
        let norm = WaveCoordinator::normalize_target_path(target);
        coord
            .pending_detach
            .lock()
            .unwrap()
            .get(&norm)
            .map_or(0, |by_context| by_context.len())
    }

    #[test]
    fn wl2_no_wave_in_flight_detaches_immediately() {
        let _g = super::super::super::test_support::env_guard();
        // AC-11: close with no in-flight wave → synchronous detach.
        let coord = Arc::new(WaveCoordinator::new());
        let target = PathBuf::from("/repos/codesense");
        let (fired, detach) = recording_detach();

        coord.detach_now_or_defer(&target, "ctx", detach);

        assert_eq!(fired.load(Ordering::SeqCst), 1, "must detach immediately");
        assert_eq!(pending_len(&coord), 0, "nothing deferred");
    }

    #[test]
    fn wl2_wave_in_flight_defers_then_fires_on_drain() {
        let _g = super::super::super::test_support::env_guard();
        // AC-3: close during a wave defers; the draining wave fires it.
        let coord = Arc::new(WaveCoordinator::new());
        let target = PathBuf::from("/repos/codesense");
        let aff = push_in_flight(&coord, 1, &[&target]);
        let (fired, detach) = recording_detach();

        coord.detach_now_or_defer(&target, "ctx", detach);
        assert_eq!(
            fired.load(Ordering::SeqCst),
            0,
            "must defer while in flight"
        );
        assert_eq!(pending_len(&coord), 1, "detach queued");

        drain_wave(&coord, 1, aff);
        assert_eq!(fired.load(Ordering::SeqCst), 1, "fires once wave drains");
        assert_eq!(pending_len(&coord), 0, "pending cleared after fire");
    }

    #[test]
    fn wl2_two_in_flight_waves_detach_waits_for_both() {
        let _g = super::super::super::test_support::env_guard();
        // AC-4: two in-flight waves on the same target (serialized under
        // that target's keyed lock, but both registered) → detach waits for BOTH.
        let coord = Arc::new(WaveCoordinator::new());
        let target = PathBuf::from("/repos/codesense");
        let aff1 = push_in_flight(&coord, 1, &[&target]);
        let aff2 = push_in_flight(&coord, 2, &[&target]);
        let (fired, detach) = recording_detach();

        coord.detach_now_or_defer(&target, "ctx", detach);
        assert_eq!(fired.load(Ordering::SeqCst), 0);

        drain_wave(&coord, 1, aff1);
        assert_eq!(
            fired.load(Ordering::SeqCst),
            0,
            "second wave still in flight — must stay deferred"
        );

        drain_wave(&coord, 2, aff2);
        assert_eq!(
            fired.load(Ordering::SeqCst),
            1,
            "fires only after both drain"
        );
    }

    #[test]
    fn wl2_panicking_wave_still_fires_deferred_detach() {
        let _g = super::super::super::test_support::env_guard();
        // AC-5: the drain trigger lives in InFlightGuard::drop, so a wave
        // that PANICS still releases a deferred detach (drop runs on unwind).
        let coord = Arc::new(WaveCoordinator::new());
        let target = PathBuf::from("/repos/codesense");
        let aff = push_in_flight(&coord, 1, &[&target]);
        let (fired, detach) = recording_detach();
        coord.detach_now_or_defer(&target, "ctx", detach);
        assert_eq!(fired.load(Ordering::SeqCst), 0);

        let coord_for_panic = Arc::clone(&coord);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _guard = InFlightGuard {
                coord: coord_for_panic,
                wave_id: 1,
                affected: aff,
            };
            panic!("wave blew up mid-materialization");
        }));

        assert!(result.is_err(), "the closure must have panicked");
        assert_eq!(
            fired.load(Ordering::SeqCst),
            1,
            "deferred detach must fire even when the wave panics"
        );
    }

    #[test]
    fn wl2_enqueue_during_drain_window_keeps_detach_deferred() {
        let _g = super::super::super::test_support::env_guard();
        // AC-7 (R2): a NEW wave arriving for the target after close keeps
        // the detach deferred — the drain re-check sees it under the same
        // in_flight lock, so detach doesn't fire prematurely.
        let coord = Arc::new(WaveCoordinator::new());
        let target = PathBuf::from("/repos/codesense");
        let aff1 = push_in_flight(&coord, 1, &[&target]);
        let (fired, detach) = recording_detach();
        coord.detach_now_or_defer(&target, "ctx", detach);

        // A background wave (e.g. a team-content edit fanning out to this
        // still-attached repo) enqueues for the same target.
        let aff3 = push_in_flight(&coord, 3, &[&target]);

        drain_wave(&coord, 1, aff1);
        assert_eq!(
            fired.load(Ordering::SeqCst),
            0,
            "newer in-flight wave must keep detach deferred"
        );

        drain_wave(&coord, 3, aff3);
        assert_eq!(
            fired.load(Ordering::SeqCst),
            1,
            "fires once the target truly drains"
        );
    }

    #[test]
    fn wl2_normalization_drift_target_matches() {
        let _g = super::super::super::test_support::env_guard();
        // AC-8 (W8/R1): the wave's code_repo_path and the watcher's
        // repo_path can differ by a symlink form. The shared normalizer
        // must bridge them — else the gate silently no-ops. Stored as the
        // resolved form (as a wave would), queried via the symlink form.
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real_repo");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link_repo");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        // Sanity: the two raw forms are genuinely different strings, so a
        // raw-string compare would MISS (this is the bug AC-8 guards).
        assert_ne!(real, link, "precondition: symlink and target differ raw");

        let coord = Arc::new(WaveCoordinator::new());
        // Wave registered under the resolved path.
        let aff = push_in_flight(&coord, 1, &[real.as_path()]);
        let (fired, detach) = recording_detach();

        // Close arrives keyed on the SYMLINK path (the watcher's repo_path).
        coord.detach_now_or_defer(&link, "ctx", detach);
        assert_eq!(
            fired.load(Ordering::SeqCst),
            0,
            "symlink-form target must still match the resolved-form wave"
        );
        assert_eq!(pending_len(&coord), 1);

        drain_wave(&coord, 1, aff);
        assert_eq!(fired.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn wl2_reattach_cancels_pending_detach() {
        let _g = super::super::super::test_support::env_guard();
        // AC-9 (R4): close → defer → re-open (re-attach) before the wave
        // drains. The stale deferred detach must be cancelled so it can't
        // kill the now-live watcher.
        let coord = Arc::new(WaveCoordinator::new());
        let target = PathBuf::from("/repos/codesense");
        let aff = push_in_flight(&coord, 1, &[&target]);
        let (fired, detach) = recording_detach();

        coord.detach_now_or_defer(&target, "ctx", detach);
        assert_eq!(pending_len(&coord), 1, "deferred while in flight");

        // Re-attach (add_watch) for the same target cancels the deferral.
        coord.cancel_pending_detach(&target, "ctx");
        assert_eq!(
            pending_len(&coord),
            0,
            "re-attach cleared the pending detach"
        );

        drain_wave(&coord, 1, aff);
        assert_eq!(
            fired.load(Ordering::SeqCst),
            0,
            "cancelled detach must NOT fire against the re-attached watcher"
        );
    }

    #[test]
    fn wl2_two_subscribers_same_path_both_defer_and_fire() {
        let _g = super::super::super::test_support::env_guard();
        // Review iter-1 (Major): two WatchConfigs share a `repo_path` but
        // differ in `context_repo_path` (SW-WL-1's composite-identity case).
        // Both close while one wave writes into the shared path; each
        // deferral must get its OWN slot (keyed by context) — the second
        // must NOT overwrite the first — and BOTH must fire on drain. A
        // flat PathBuf→DetachFn map would leak the first subscriber.
        let coord = Arc::new(WaveCoordinator::new());
        let target = PathBuf::from("/repos/shared");
        let aff = push_in_flight(&coord, 1, &[&target]);
        let (fired_a, detach_a) = recording_detach();
        let (fired_b, detach_b) = recording_detach();

        coord.detach_now_or_defer(&target, "ctx/a", detach_a);
        coord.detach_now_or_defer(&target, "ctx/b", detach_b);
        assert_eq!(pending_len(&coord), 1, "one path...");
        assert_eq!(
            pending_contexts(&coord, &target),
            2,
            "...with two distinct subscribers"
        );
        assert_eq!(fired_a.load(Ordering::SeqCst), 0);
        assert_eq!(fired_b.load(Ordering::SeqCst), 0);

        drain_wave(&coord, 1, aff);
        assert_eq!(
            fired_a.load(Ordering::SeqCst),
            1,
            "subscriber A's detach must fire"
        );
        assert_eq!(
            fired_b.load(Ordering::SeqCst),
            1,
            "subscriber B's detach must fire"
        );
        assert_eq!(pending_len(&coord), 0, "both cleared");
    }

    #[test]
    fn wl2_cancel_is_granular_per_context() {
        let _g = super::super::super::test_support::env_guard();
        // Review iter-1: re-attaching ONE subscriber must cancel only its
        // own deferral, leaving a sibling subscriber's deferred detach at
        // the same path intact.
        let coord = Arc::new(WaveCoordinator::new());
        let target = PathBuf::from("/repos/shared");
        let aff = push_in_flight(&coord, 1, &[&target]);
        let (fired_a, detach_a) = recording_detach();
        let (fired_b, detach_b) = recording_detach();
        coord.detach_now_or_defer(&target, "ctx/a", detach_a);
        coord.detach_now_or_defer(&target, "ctx/b", detach_b);

        // A re-attaches → cancels only A.
        coord.cancel_pending_detach(&target, "ctx/a");
        assert_eq!(
            pending_contexts(&coord, &target),
            1,
            "only B remains deferred"
        );

        drain_wave(&coord, 1, aff);
        assert_eq!(
            fired_a.load(Ordering::SeqCst),
            0,
            "A re-attached — must NOT fire"
        );
        assert_eq!(
            fired_b.load(Ordering::SeqCst),
            1,
            "B never re-attached — must fire"
        );
    }

    #[test]
    fn wl2_concurrent_ops_no_deadlock() {
        let _g = super::super::super::test_support::env_guard();
        // AC-6 / AC-10: hammer the gate's three lock-touching paths
        // (defer/immediate detach, re-attach cancel, wave drain) across
        // threads on overlapping targets. A lock-order inversion would
        // deadlock and hang the test (CI timeout); a clean run proves the
        // in_flight → pending_detach → watcher_lock order holds.
        let coord = Arc::new(WaveCoordinator::new());
        let targets: Vec<PathBuf> = (0..4)
            .map(|i| PathBuf::from(format!("/repos/r{i}")))
            .collect();

        let mut handles = Vec::new();
        for t in 0..8u64 {
            let coord = Arc::clone(&coord);
            let targets = targets.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..200u64 {
                    let target = &targets[(i as usize + t as usize) % targets.len()];
                    let wave_id = t * 1000 + i;
                    // Vary context per thread so multiple subscribers
                    // contend on the same path's inner map.
                    let context = format!("ctx{t}");
                    let aff = push_in_flight(&coord, wave_id, &[target.as_path()]);
                    let (_c, detach) = recording_detach();
                    coord.detach_now_or_defer(target, &context, detach);
                    if i % 3 == 0 {
                        coord.cancel_pending_detach(target, &context);
                    }
                    drain_wave(&coord, wave_id, aff);
                }
            }));
        }
        for h in handles {
            h.join().expect("worker thread must not panic or deadlock");
        }
        // All waves drained; registry + pending must be empty.
        assert!(coord.in_flight.lock().unwrap().is_empty());
        assert_eq!(pending_len(&coord), 0);
    }

    #[tokio::test]
    async fn enqueue_supersedes_in_flight_with_same_reason() {
        let _g = super::super::super::test_support::env_guard();
        // Use a synthetic recording emit to observe wave lifecycle.
        let coord = Arc::new(WaveCoordinator::new());
        let dispatcher = Arc::new(super::super::super::dispatcher::Dispatcher::new(
            0, 100, 60_000, 60_000,
        ));
        let suppression = dispatcher.suppression_handle();

        let events = Arc::new(StdMutex::new(Vec::<(String, serde_json::Value)>::new()));
        let events_clone = Arc::clone(&events);
        let emit: Arc<dyn Fn(&'static str, serde_json::Value) + Send + Sync> =
            Arc::new(move |topic, payload| {
                events_clone
                    .lock()
                    .unwrap()
                    .push((topic.to_string(), payload));
            });
        let deps = WaveDeps {
            suppression: suppression.clone(),
            emit: Arc::clone(&emit),
            cache: None,
            projection_enabled: false,
        };

        let ctx = ResolverContext {
            context_root: None,
            codesense_home: PathBuf::from("/tmp/sync_test_home_supersede"),
            user_id: 42,
            org_slug: "acme/api".to_string(),
            team_name: Some("default".to_string()),
        };

        // Hold every engine permit so wave A is still in flight (parked on the
        // pool) when wave B is enqueued — otherwise an instant A can drain and
        // leave the registry before B's supersede walk runs.
        let mut held = Vec::new();
        for _ in 0..engine_worker_count() {
            held.push(
                Arc::clone(&coord.engine_semaphore)
                    .acquire_owned()
                    .await
                    .unwrap(),
            );
        }

        // Wave A: MigrationCutover, no affected.
        // We chain two MigrationCutover waves; the second supersedes the first.
        let wave_a = Wave::new(
            WaveReason::MigrationCutover {
                explicit_names: None,
            },
            ctx.clone(),
            vec![],
        );
        let cancel_a = wave_a.cancel.clone();
        let _h_a = coord.enqueue(wave_a, deps.clone());

        let wave_b = Wave::new(
            WaveReason::MigrationCutover {
                explicit_names: None,
            },
            ctx.clone(),
            vec![],
        );
        let h_b = coord.enqueue(wave_b, deps.clone());
        drop(held);

        h_b.await.unwrap();

        // The first wave's cancel token should have fired.
        assert!(cancel_a.is_cancelled(), "wave A must have been cancelled");
    }

    // ---------- projection-plan / resolver-context fixtures ----------

    // ---------- SLW-FX-7: projection block offloaded to spawn_blocking ----------

    // ---------- emit payload helpers ----------

    #[test]
    fn emit_wave_started_payload_shape() {
        let _g = super::super::super::test_support::env_guard();
        let p = emit_wave_started_payload(
            42,
            &WaveReason::MigrationCutover {
                explicit_names: None,
            },
            &[PathBuf::from("/r1"), PathBuf::from("/r2")],
        );
        assert_eq!(p["wave_id"], 42);
        assert_eq!(p["affected_repo_count"], 2);
        assert_eq!(p["reason"]["kind"], "MigrationCutover");
        assert_eq!(p["affected_repos"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn emit_wave_completed_payload_collapses_skipped() {
        let _g = super::super::super::test_support::env_guard();
        let mut result = WaveResult::default();
        let counts = RepoWaveCounts {
            skipped_already_current: 3,
            skipped_locally_modified: 2,
            written: 1,
            unmaterialized: 4,
            ..Default::default()
        };
        result.per_repo.insert(PathBuf::from("/r"), counts);
        result.duration_ms = 123;

        let affected = vec![PathBuf::from("/r")];
        let p = emit_wave_completed_payload(
            7,
            &WaveReason::MigrationCutover {
                explicit_names: None,
            },
            &affected,
            &result,
        );
        assert_eq!(p["wave_id"], 7);
        assert_eq!(p["materialize_counts"]["skipped"], 5);
        assert_eq!(p["materialize_counts"]["written"], 1);
        assert_eq!(p["materialize_counts"]["unmaterialized"], 4);
        assert_eq!(p["duration_ms"], 123);
        assert_eq!(p["cancelled"], false);
        // Iter-5 (Auditor H2): affected_repos comes from the passed-in
        // list, not result.per_repo.keys().
        assert_eq!(p["affected_repos"].as_array().unwrap().len(), 1);
        // SW-FR-1: no surface_outcome passed → the field is JSON null (the FE
        // treats a missing/null outcome as "use the aggregate path").
        assert!(p["surface_outcome"].is_null());
    }

    /// SW-XS-4: the completed payload carries the explicit operation's conflicts
    /// (files changed on both sides, not published) and no longer the retired
    /// SW-SY-4 `changed_paths` per-path hint.
    #[test]
    fn emit_wave_completed_payload_carries_conflicts_not_changed_paths() {
        let _g = super::super::super::test_support::env_guard();
        let result = WaveResult {
            conflicts: vec![WaveError {
                code_repo_path: PathBuf::from("/r"),
                logical_name: Some("_bmad-output/a.md".into()),
                message: "conflict: a".into(),
            }],
            ..Default::default()
        };
        let p = emit_wave_completed_payload(
            1,
            &WaveReason::MigrationCutover {
                explicit_names: None,
            },
            &[PathBuf::from("/r")],
            &result,
        );
        assert!(p.get("changed_paths").is_none());
        let arr = p["conflicts"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["logical_name"], "_bmad-output/a.md");
        assert!(p["errors"].as_array().unwrap().is_empty());
    }

    /// shrinks below the wave's affected list. Verify the completed
    /// payload's `affected_repos` still carries the FULL list passed in
    /// — the "of N" denominator stays stable across started + completed.
    #[test]
    fn emit_wave_completed_payload_uses_full_affected_list_under_cancel() {
        let _g = super::super::super::test_support::env_guard();
        let mut result = WaveResult::default();
        // Only 1 repo materialized before cancel.
        result
            .per_repo
            .insert(PathBuf::from("/r1"), RepoWaveCounts::default());
        result.cancelled = true;

        // But the wave's affected list had 3.
        let affected = vec![
            PathBuf::from("/r1"),
            PathBuf::from("/r2"),
            PathBuf::from("/r3"),
        ];
        let p = emit_wave_completed_payload(
            11,
            &WaveReason::MigrationCutover {
                explicit_names: None,
            },
            &affected,
            &result,
        );
        assert_eq!(p["cancelled"], true);
        assert_eq!(p["affected_repos"].as_array().unwrap().len(), 3);
    }

    // ===== SS-4c AC-15 (added in iter-2 per Auditor H3/H4/M1) =====

    /// AC-15 bullet 3 / Auditor H3 / Task 10.4 — updated for SLW-RI-4: the
    /// per-target keyed wave lock blocks a new wave that SHARES A TARGET with a
    /// prior wave from running until the prior wave's `Wave::run` body finishes.
    /// (Pre-RI-4 this was the whole-engine `global_lock`; RI-4 keyed it, so this
    /// serialization now holds per-shared-target — both waves below are given the
    /// SAME `affected` target so they still serialize. Disjoint/empty-affected
    /// waves intentionally run concurrently now — see
    /// `ri4_disjoint_target_waves_do_not_block`.)
    ///
    /// Iter-3 strengthens iter-2's weak abs-diff assertion (Blind H2 /
    /// Auditor concern): the iter-2 test would have passed even with
    /// the lock broken — two near-simultaneous emits land
    /// adjacent in a seq counter regardless of serialization.
    ///
    /// We now observe the lock directly via a slow `sync-wave-completed`
    /// closure on wave A that:
    ///   1. Sets `wave_a_in_emit = true` (A still holds the shared target's
    ///      keyed lock — `_target_guards` outlive `wave.run`'s return here).
    ///   2. Sleeps ~80 ms (still holding the lock).
    ///   3. Sets `wave_a_in_emit = false`.
    ///
    /// Wave B's `emit-completed` closure snapshots `wave_a_in_emit` at
    /// the moment B's run body finishes. B shares A's target key, so with the
    /// per-target keyed lock in place B cannot ENTER its body until A's lock
    /// drops — which happens AFTER A's emit-completed closure returns (and flips
    /// the flag back to false). So the snapshot MUST be `false`. With the lock
    /// broken, B's body would run concurrently with A's 80 ms sleep window and
    /// capture `true`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shared_target_wave_lock_blocks_new_wave_until_prior_completes() {
        let _g = super::super::super::test_support::env_guard();
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;
        use tokio::sync::Notify;

        let coord = Arc::new(WaveCoordinator::new());
        let dispatcher = Arc::new(super::super::super::dispatcher::Dispatcher::new(
            0, 100, 60_000, 60_000,
        ));

        let wave_a_in_emit = Arc::new(AtomicBool::new(false));
        let wave_a_emit_completed_fired = Arc::new(AtomicBool::new(false));
        let wave_a_in_emit_at_b = Arc::new(AtomicBool::new(false));
        // Iter-4 (Blind iter-3 MED + Edge iter-3 #4-#6): handshake
        // primitive so the main task waits until A's spawned body
        // demonstrably holds the shared target's keyed lock before enqueueing B.
        // Replaces the iter-3 fixed 10ms wall-clock sleep, which
        // could spuriously pass under loaded CI when A's spawn is
        // scheduled later than B's.
        let wave_a_started_running = Arc::new(Notify::new());

        // Wave A: emit-started signals notify (proves spawn body
        // entered run); emit-completed flips flag, sleeps, clears.
        let flag_for_a = Arc::clone(&wave_a_in_emit);
        let completed_fired = Arc::clone(&wave_a_emit_completed_fired);
        let started_signal = Arc::clone(&wave_a_started_running);
        let deps_a = WaveDeps {
            suppression: dispatcher.suppression_handle(),
            emit: Arc::new(move |topic, _| {
                if topic == EVENT_WAVE_COMPLETED {
                    flag_for_a.store(true, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(80));
                    flag_for_a.store(false, Ordering::SeqCst);
                    completed_fired.store(true, Ordering::SeqCst);
                } else if topic == EVENT_WAVE_STARTED {
                    started_signal.notify_one();
                }
            }),
            cache: None,
            projection_enabled: false,
        };

        // Wave B: `emit-completed` snapshots the flag at body-exit.
        let flag_observed = Arc::clone(&wave_a_in_emit);
        let snapshot = Arc::clone(&wave_a_in_emit_at_b);
        let deps_b = WaveDeps {
            suppression: dispatcher.suppression_handle(),
            emit: Arc::new(move |topic, _| {
                if topic == EVENT_WAVE_COMPLETED {
                    snapshot.store(flag_observed.load(Ordering::SeqCst), Ordering::SeqCst);
                }
            }),
            cache: None,
            projection_enabled: false,
        };

        let ctx = ResolverContext {
            context_root: None,
            codesense_home: PathBuf::from("/tmp/sync_test_global_lock"),
            user_id: 42,
            org_slug: "acme/api".to_string(),
            team_name: Some("default".to_string()),
        };

        // SS-4c iter-2 moved sync-wave-started emit into `enqueue`,
        // so A's started fires BEFORE the spawned task even runs.
        // To prove A's body is in flight (holding the shared target's keyed lock),
        // we instead poll on `wave_a_in_emit` flipping true — that
        // ONLY happens inside A's run body's emit-completed.
        //
        // But waiting for emit-completed defeats the purpose (B
        // needs to enqueue WHILE A's body is mid-flight). The right
        // primitive: hook into emit-started (which fires from enqueue
        // for A, then for B), and after both are observed plus a
        // small spin to let A's spawn enter run, enqueue B. Simpler:
        // enqueue B immediately after A, and trust tokio's spawn
        // ordering for the spawned-but-not-yet-running window —
        // the assertion catches any concurrency regardless.
        // SLW-RI-4: both waves target the SAME repo, so they serialize on that
        // target's keyed lock (the invariant this test now guards).
        let shared_target =
            AttachedCodeRepo::attached(slug(), PathBuf::from("/tmp/ri4_shared_target_wt"));
        let wave_a = Wave::new(
            WaveReason::MigrationCutover {
                explicit_names: None,
            },
            ctx.clone(),
            vec![shared_target.clone()],
        );
        let h_a = coord.enqueue(wave_a, deps_a);

        // Yield to let A's spawned task progress past the semaphore-permit +
        // keyed-lock acquire into wave.run. `tokio::task::yield_now()` is reliable across
        // loaded runners since it just relinquishes the current task's
        // slice; combined with the 80ms emit sleep, A's lock-hold
        // window comfortably covers B's spawn + body.
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        let wave_b = Wave::new(
            WaveReason::MigrationCutover {
                explicit_names: None,
            },
            ctx.clone(),
            vec![shared_target.clone()],
        );
        let h_b = coord.enqueue(wave_b, deps_b);

        h_a.await.unwrap();
        h_b.await.unwrap();

        // Defense-in-depth: ensure A's emit-completed actually fired.
        // Otherwise a scheduling pathology where A never ran would
        // make the snapshot trivially `false` and pass spuriously.
        assert!(
            wave_a_emit_completed_fired.load(Ordering::SeqCst),
            "test scheduling pathology: A's emit-completed never fired — \
             assertion below would pass for the wrong reason"
        );
        assert!(
            !wave_a_in_emit_at_b.load(Ordering::SeqCst),
            "keyed lock failed: B's run body completed WHILE A's \
             emit-completed closure was still holding the shared target's \
             keyed lock (sleeping 80ms). Same-target wave bodies ran concurrently."
        );
        // Unused but kept to silence the unused-variable warning on the
        // notify handle (compiler doesn't infer that emit-started
        // signaling is observed indirectly via the spawn ordering).
        let _ = wave_a_started_running;
    }

    /// AC-15 bullet 5 / Auditor M1 / Task 10.6: `sync-wave-started`
    /// fires before any work happens (specifically before the permit +
    /// keyed locks are acquired in the spawned task — iter-2 moved this to
    /// `enqueue` itself); `sync-wave-completed` fires after `Wave::run`
    /// completes its per-repo loop. Recorded ordering proves both.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wave_emits_started_before_completed_with_repos_in_between() {
        let _g = super::super::super::test_support::env_guard();
        use std::sync::atomic::{AtomicU32, Ordering};

        let coord = Arc::new(WaveCoordinator::new());
        let dispatcher = Arc::new(super::super::super::dispatcher::Dispatcher::new(
            0, 100, 60_000, 60_000,
        ));

        let seq = Arc::new(AtomicU32::new(0));
        let started_at = Arc::new(AtomicU32::new(0));
        let completed_at = Arc::new(AtomicU32::new(0));
        let s = Arc::clone(&seq);
        let st = Arc::clone(&started_at);
        let co = Arc::clone(&completed_at);
        let deps = WaveDeps {
            suppression: dispatcher.suppression_handle(),
            emit: Arc::new(move |topic, _| {
                let n = s.fetch_add(1, Ordering::SeqCst) + 1;
                if topic == EVENT_WAVE_STARTED {
                    st.store(n, Ordering::SeqCst);
                } else if topic == EVENT_WAVE_COMPLETED {
                    co.store(n, Ordering::SeqCst);
                }
            }),
            cache: None,
            projection_enabled: false,
        };

        let ctx = ResolverContext {
            context_root: None,
            codesense_home: PathBuf::from("/tmp/sync_test_order"),
            user_id: 42,
            org_slug: "acme/api".to_string(),
            team_name: Some("default".to_string()),
        };
        let wave = Wave::new(
            WaveReason::MigrationCutover {
                explicit_names: None,
            },
            ctx,
            vec![],
        );
        coord.enqueue(wave, deps).await.unwrap();

        let s_at = started_at.load(Ordering::SeqCst);
        let c_at = completed_at.load(Ordering::SeqCst);
        assert!(s_at > 0, "sync-wave-started never fired (started_at = 0)");
        assert!(
            c_at > s_at,
            "sync-wave-completed fired before sync-wave-started: started={} completed={}",
            s_at,
            c_at
        );
    }

    /// The reverse-claim sweep's gone-path guard (its livelock fix) keys on this
    /// predicate: a torn-down / unmounted worktree path must resolve to `false`
    /// so the sweep skips it (no wave enqueued) rather than dispatching a wave
    /// that skip-and-re-enqueues forever. A real dir resolves `true`.
    #[test]
    fn repo_path_resolves_false_for_gone_dir_true_for_real_dir() {
        let _g = super::super::super::test_support::env_guard();
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().to_path_buf();
        assert!(repo_path_resolves(&real), "an existing dir must resolve");
        let gone = real.join("torn-down-epic-worktree");
        assert!(
            !repo_path_resolves(&gone),
            "a path with no working tree must NOT resolve (sweep skips it)"
        );
    }

    /// SS-FU-17 AC-6: a wave over a code repo whose path no longer resolves
    /// skips it (no phantom git ops against a dead path) and emits
    /// `sync-repo-looks-gone` instead of dispatching `run_repo_reconcile`.
    #[tokio::test]
    async fn wave_skips_gone_path_and_emits_repo_looks_gone() {
        let _g = super::super::super::test_support::env_guard();
        let coord = Arc::new(WaveCoordinator::new());
        let dispatcher = Arc::new(super::super::super::dispatcher::Dispatcher::new(
            0, 100, 60_000, 60_000,
        ));

        let recorded: Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let rec = Arc::clone(&recorded);
        let deps = WaveDeps {
            suppression: dispatcher.suppression_handle(),
            emit: Arc::new(move |topic, payload| {
                rec.lock().unwrap().push((topic.to_string(), payload));
            }),
            cache: None,
            projection_enabled: false,
        };

        let ctx = ResolverContext {
            context_root: None,
            codesense_home: PathBuf::from("/tmp/sync_test_gone"),
            user_id: 42,
            org_slug: "acme/api".to_string(),
            team_name: Some("default".to_string()),
        };

        // A path that does not resolve on disk → "gone".
        let gone = PathBuf::from("/nonexistent/codesense/gone-repo-xyz");
        let affected = vec![AttachedCodeRepo::attached(slug(), gone.clone())];

        let wave = Wave::new(
            WaveReason::MigrationCutover {
                explicit_names: None,
            },
            ctx,
            affected,
        );
        let result = coord.enqueue(wave, deps).await.unwrap();

        // No phantom wave ran against the dead path.
        assert!(
            !result.per_repo.contains_key(&gone),
            "gone path must not be dispatched into run_repo"
        );

        // The gone signal was surfaced for the UI (feeds AC-7).
        let events = recorded.lock().unwrap();
        let gone_event = events
            .iter()
            .find(|(topic, _)| topic == EVENT_REPO_LOOKS_GONE)
            .expect("expected sync-repo-looks-gone to be emitted");
        assert_eq!(
            gone_event.1["code_repo_path"],
            serde_json::json!(gone),
            "payload must carry the dead path"
        );
        assert!(gone_event.1["owner"].is_string());
        assert!(gone_event.1["repo"].is_string());
        // RF-perf-20 (AC-4): the CONTAINER `/nonexistent/codesense` is also absent,
        // so this reads as a whole-container disappearance (a possible unmount), NOT a
        // specific worktree removal — the engine must NOT reap it (SS-FU-17 AC-7
        // unmount-safety preserved).
        assert_eq!(
            gone_event.1["reaped"],
            serde_json::json!(false),
            "AC-4: not reaped when the container is also gone (unmount-safe)"
        );
    }

    /// RF-perf-20 (AC-1/AC-2): a gone worktree whose CONTAINER still exists (a
    /// specific `git worktree remove` / external rm / crash, not a volume unmount)
    /// is **reaped** — its lingering lockfile is deleted and its reverse state
    /// pruned — so `enumerate_attached_code_repos_for_team` stops returning it and
    /// the no-op `ReverseDirtyBatch` retry loop terminates after ONE detection. The
    /// emitted `sync-repo-looks-gone` carries `reaped: true` (AC-2).
    #[tokio::test]
    async fn wave_reaps_gone_worktree_when_container_present() {
        let _g = super::super::super::test_support::env_guard();
        use super::super::super::lockfile::{enumerate_attached_code_repos_for_team, Lockfile};

        let coord = Arc::new(WaveCoordinator::new());
        let dispatcher = Arc::new(super::super::super::dispatcher::Dispatcher::new(
            0, 100, 60_000, 60_000,
        ));
        let recorded: Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let rec = Arc::clone(&recorded);
        let deps = WaveDeps {
            suppression: dispatcher.suppression_handle(),
            emit: Arc::new(move |topic, payload| {
                rec.lock().unwrap().push((topic.to_string(), payload));
            }),
            cache: None,
            projection_enabled: false,
        };

        let home = tempfile::TempDir::new().unwrap();
        let ctx = ResolverContext {
            context_root: None,
            codesense_home: home.path().to_path_buf(),
            user_id: 42,
            org_slug: "acme/api".to_string(),
            team_name: Some("default".to_string()),
        };

        // Container (parent) exists; the worktree dir inside it does NOT → a
        // specific removal (reap fires), not a whole-volume unmount.
        let container = tempfile::TempDir::new().unwrap();
        let gone = container.path().join("removed-worktree");
        assert!(!repo_path_resolves(&gone), "worktree dir is absent");
        assert!(gone.parent().unwrap().is_dir(), "but its container remains");

        // Seed a lingering install target for the gone worktree (the leak this story fixes).
        let slugs = vec![slug()];
        seed_store(
            home.path(),
            ctx.user_id,
            &slug(),
            &Lockfile {
                schema_version: 3,
                org_slug: ctx.org_slug.clone(),
                team_name: ctx.team_name.clone(),
                code_repo_path: gone.clone(),
                entries: vec![],
            },
        );

        // BEFORE: attachment is derived from the store → the gone repo is attached.
        let before = enumerate_attached_code_repos_for_team(&ctx, &slugs).unwrap();
        assert!(
            before.iter().any(|a| a.code_repo_path == gone),
            "gone worktree is attached before reap (lockfile-derived)"
        );

        let affected = vec![AttachedCodeRepo::attached(slug(), gone.clone())];
        let wave = Wave::new(
            WaveReason::MigrationCutover {
                explicit_names: None,
            },
            ctx.clone(),
            affected,
        );
        let result = coord.enqueue(wave, deps).await.unwrap();
        assert!(
            !result.per_repo.contains_key(&gone),
            "gone path must not be dispatched into run_repo"
        );

        // AC-2: one legible event with reaped:true.
        let events = recorded.lock().unwrap();
        let gone_event = events
            .iter()
            .find(|(topic, _)| topic == EVENT_REPO_LOOKS_GONE)
            .expect("expected sync-repo-looks-gone to be emitted");
        assert_eq!(
            gone_event.1["reaped"],
            serde_json::json!(true),
            "AC-2: container-present removal is reaped"
        );

        // AC-1: the install target is dropped, so a subsequent enumerate no longer
        // returns the repo — the producer/drain never re-visit it and the loop terminates.
        let after = enumerate_attached_code_repos_for_team(&ctx, &slugs).unwrap();
        assert!(
            !after.iter().any(|a| a.code_repo_path == gone),
            "AC-1: reaped worktree no longer attached — retry loop terminates"
        );
    }

    // ===== SS-4c AC-7: performance budget — local-only =====

    /// AC-7: wave covering N simulated code repos × M logical names must
    /// finish under the configured budget (`CS_WAVE_BUDGET_MS`, default
    /// 2000ms) on M-series hardware.
    ///
    /// Marked `#[ignore]` so `cargo test` doesn't run it by default — CI
    /// runners are not M-series and the threshold isn't calibrated for
    /// them. Run via `cargo test --release -- --ignored
    /// perf_budget_wave_50_repos_x_30_names` locally pre-PR; the PR
    /// description records the measured median.
    ///
    /// To keep the test runtime reasonable on dev machines, the default
    /// fixture is scaled down to 10 repos × 10 names (= 100 mats); the
    /// AC's "50 × 30 = 1500" full scale is opt-in via env vars
    /// (`CS_WAVE_BUDGET_REPOS=50 CS_WAVE_BUDGET_NAMES=30`).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "perf budget — run locally on M-series via --ignored"]
    async fn perf_budget_wave_50_repos_x_30_names() {
        use std::time::Instant;

        let _g = super::super::super::test_support::env_guard();
        let empty_core = tempfile::TempDir::new().unwrap();
        let _core = ScopedEnvVar::set("CODESENSE_CORE_DEV_PATH", empty_core.path());

        // Scale (default tuned for laptop dev; opt into full AC scale
        // via env).
        let repos: usize = env_or("CS_WAVE_BUDGET_REPOS", 10);
        let names: usize = env_or("CS_WAVE_BUDGET_NAMES", 10);
        let budget_ms: u64 = env_or("CS_WAVE_BUDGET_MS", 2000);

        let codesense_home = tempfile::TempDir::new().unwrap();
        let (ctx, _code_repo_temps, affected) =
            plant_team_workspace_fixture(&codesense_home, repos, names);

        // Wave engine deps — no-op emit so the bench doesn't pay for
        // serde+IPC.
        let dispatcher = std::sync::Arc::new(super::super::super::dispatcher::Dispatcher::new(
            0, 100, 60_000, 60_000,
        ));
        let deps = WaveDeps {
            suppression: dispatcher.suppression_handle(),
            emit: Arc::new(|_, _| ()),
            cache: None,
            projection_enabled: false,
        };

        let coord = std::sync::Arc::new(WaveCoordinator::new());
        let wave = cutover_wave(ctx, affected);

        let started = Instant::now();
        let result = coord.enqueue(wave, deps).await.unwrap();
        let elapsed_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;

        eprintln!(
            "perf_budget_wave: repos={} names={} duration_ms={} (budget={})",
            repos, names, elapsed_ms, budget_ms
        );
        assert!(
            elapsed_ms < budget_ms,
            "wave exceeded budget: {} ms > {} ms (repos={}, names={})",
            elapsed_ms,
            budget_ms,
            repos,
            names
        );
        assert!(!result.cancelled, "wave was cancelled mid-run");
    }

    /// Test-only: read a scale/budget knob from the environment. Warns
    /// loudly (rather than silently defaulting) when the var is *set* but
    /// unparseable — so a typo'd gate invocation (e.g. `CS_..._MS=20O`)
    /// can't quietly run under the default budget and report a false pass.
    fn env_or<T>(key: &str, default: T) -> T
    where
        T: std::str::FromStr,
    {
        let Ok(raw) = std::env::var(key) else {
            return default;
        };
        match raw.parse() {
            Ok(v) => v,
            Err(_) => {
                eprintln!("WARNING: env {key} = {raw:?} is unparseable; using default");
                default
            }
        }
    }

    /// Test-only RAII: set an env var for the scope and remove it on drop,
    /// so a panic mid-test cannot leak it into later tests sharing the
    /// process. Declare *after* the temp dir it points at so it drops
    /// first (var removed before the dir is deleted).
    struct ScopedEnvVar(&'static str);
    impl ScopedEnvVar {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            std::env::set_var(key, value);
            Self(key)
        }
    }
    impl Drop for ScopedEnvVar {
        fn drop(&mut self) {
            std::env::remove_var(self.0);
        }
    }

    /// SLW-FX-13 test helper: record `code_repo_path` as a codesense-managed clone
    /// so the materialize guard admits it when the wave enumerates it from the
    /// store. Story worktrees use explicit selected context and automatic waves
    /// skip them, so a managed clone is the automatic-context target.
    fn register_test_worktree_path(ctx: &ResolverContext, code_repo_path: &std::path::Path) {
        super::super::super::test_support::register_managed_clone(
            &ctx.codesense_home,
            &ctx.principal(),
            code_repo_path,
        );
    }

    /// SLW-FX-13 test helper: make each directly-constructed target a managed
    /// clone — recorded in `attachments.yml` AND carrying the enumeration-resolved
    /// `managed` marker the materialize guard reads.
    fn register_test_worktrees(ctx: &ResolverContext, affected: &mut [AttachedCodeRepo]) {
        for a in affected {
            register_test_worktree_path(ctx, &a.code_repo_path);
            a.managed = true;
        }
    }

    /// Test-only: plant a TeamWorkspace source layer of `files` skills plus
    /// `targets` empty code-repo temp dirs, and build the matching
    /// `ResolverContext` + `AttachedCodeRepo` list. Shared by the perf
    /// budget and the SS-FU-13 cascade harness so the `users/42/orgs/test/api/…`
    /// layout and the planting loop live in one place. Returns the temp
    /// dirs so the caller keeps them alive for the test's duration.
    fn plant_team_workspace_fixture(
        codesense_home: &tempfile::TempDir,
        targets: usize,
        files: usize,
    ) -> (
        ResolverContext,
        Vec<tempfile::TempDir>,
        Vec<AttachedCodeRepo>,
    ) {
        let ctx = ResolverContext {
            context_root: None,
            codesense_home: codesense_home.path().to_path_buf(),
            user_id: 42,
            org_slug: "test/api".to_string(),
            team_name: Some("default".to_string()),
        };

        let ws_install = codesense_home
            .path()
            .join("users/42/orgs/test/api/default/install/.claude/skills");
        std::fs::create_dir_all(&ws_install).unwrap();
        for i in 0..files {
            std::fs::write(
                ws_install.join(format!("skill_{i:04}.md")),
                format!("# skill {i} — wave test fixture\n"),
            )
            .unwrap();
        }

        let code_repo_temps: Vec<tempfile::TempDir> = (0..targets)
            .map(|_| tempfile::TempDir::new().unwrap())
            .collect();
        let mut affected: Vec<AttachedCodeRepo> = code_repo_temps
            .iter()
            .map(|t| AttachedCodeRepo::attached(slug(), t.path().to_path_buf()))
            .collect();

        // SLW-FX-13: make these bare temp dirs pass the managed-target guard.
        register_test_worktrees(&ctx, &mut affected);

        (ctx, code_repo_temps, affected)
    }

    /// Test-only: build a `MigrationCutover` wave into the `"default"` team for
    /// the given targets. Keeps the `WaveReason` + team name in one place
    /// so they can't drift from `plant_team_workspace_fixture`'s
    /// `team_name: Some("default")`.
    fn cutover_wave(ctx: ResolverContext, affected: Vec<AttachedCodeRepo>) -> Wave {
        Wave::new(
            WaveReason::MigrationCutover {
                explicit_names: None,
            },
            ctx,
            affected,
        )
    }

    /// SS-FU-13 AC-0/AC-2: reproduce the watcher cascade without a
    /// many-worktree machine. Materialize a configurable `targets × files`
    /// burst through the REAL wave + a REAL `Watcher` over the target
    /// trees, then measure two things:
    ///
    /// - **What suppression absorbs vs. leaks** — `Dispatcher::counts()`
    ///   after the wave (`suppressed` = wave self-writes dropped,
    ///   `delivered` = leaked to a handler). This is the AC-1 breakdown.
    /// - **Post-wave handler latency** — write one genuine, *un-suppressed*
    ///   edit (the "probe") to a watched tree and time how long until its
    ///   handler fires while the watcher drains the wave's backlog. This is
    ///   the AC-0 gate metric: the hover hang the story is about.
    ///
    /// Default scale (4 × 50 = 200 mats) keeps `cargo test --ignored`
    /// quick and well under budget. Opt into AC-0 full scale + the tight
    /// 200 ms gate via env:
    /// ```text
    /// CS_CASCADE_TARGETS=20 CS_CASCADE_FILES=1800 CS_CASCADE_BUDGET_MS=200 \
    ///   cargo test --release -- --ignored cascade_burst_harness_ss_fu_13 --nocapture
    /// ```
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "AC-0/AC-2 cascade harness — run locally via --ignored"]
    async fn cascade_burst_harness_ss_fu_13() {
        use std::sync::Mutex as StdMutex;
        use std::time::{Duration, Instant};

        let _g = super::super::super::test_support::env_guard();
        let empty_core = tempfile::TempDir::new().unwrap();
        let _core = ScopedEnvVar::set("CODESENSE_CORE_DEV_PATH", empty_core.path());

        // Scale + budget (default tuned for a laptop; opt into AC-0 scale
        // and the 200 ms gate via env).
        let targets: usize = env_or("CS_CASCADE_TARGETS", 4);
        let files: usize = env_or("CS_CASCADE_FILES", 50);
        assert!(
            targets >= 1 && files >= 1,
            "CS_CASCADE_TARGETS and CS_CASCADE_FILES must be >= 1 (got {targets} x {files})"
        );
        // Generous default so the small-scale run passes reliably despite
        // baseline FSEvents delivery latency; AC-0 tightens to 200.
        let budget_ms: u64 = env_or("CS_CASCADE_BUDGET_MS", 2000);
        // Extra drain time AFTER the probe is measured, so the reported
        // suppressed/leaked counts reflect the whole flood. Does NOT gate
        // the probe — see below.
        let settle_ms: u64 = env_or("CS_CASCADE_SETTLE_MS", 1500);
        // Grace defaults to production **5 s** (`SUPPRESSION_GRACE_MS`) so
        // the "slow wave outruns its own suppression" race is measured
        // faithfully — a long grace (`CS_CASCADE_GRACE_MS=60000`) holds
        // suppression for the whole wave and masks exactly that race.
        let grace_ms: u64 = env_or("CS_CASCADE_GRACE_MS", 5_000);
        // SS-FU-19 AC-1: `CS_CASCADE_WATCHER=0` runs the wave with no
        // attached `Watcher` (and no probe / AC-0 gate) so the stage Pareto
        // reflects pure write cost — isolating it from the suppression /
        // dispatch contention a live watcher adds. Default `1` preserves
        // SS-FU-13's AC-0 behavior (watcher attached + probe gated).
        let watcher_on: bool = env_or::<u64>("CS_CASCADE_WATCHER", 1) != 0;

        let codesense_home = tempfile::TempDir::new().unwrap();
        let (ctx, code_repo_temps, affected) =
            plant_team_workspace_fixture(&codesense_home, targets, files);

        // One dispatcher; one recorder handler per target writing to a
        // shared log; one real recursive `Watcher` over every target tree
        // — the UI-freshness watcher reacting to the wave's writes. Config
        // mirrors production (`commands/watch.rs`): debounce 50 ms, max
        // 1000 entries, evict age 60 s, grace `grace_ms`.
        let dispatcher = std::sync::Arc::new(super::super::super::dispatcher::Dispatcher::new(
            50, 1000, 60_000, grace_ms,
        ));
        let log = std::sync::Arc::new(StdMutex::new(Vec::<PathBuf>::new()));
        // AC-1: attach the watcher + recorder handlers only when probing
        // (`watcher_on`). With it off, the wave still registers suppression
        // on the dispatcher (via `deps.suppression`) but no events are
        // observed — the Pareto then measures the wave in isolation.
        let _watcher = if watcher_on {
            for t in &code_repo_temps {
                let log_c = std::sync::Arc::clone(&log);
                let handler: super::super::super::dispatcher::DispatchHandler =
                    Arc::new(move |p: &Path| log_c.lock().unwrap().push(p.to_path_buf()));
                dispatcher.register_handler(t.path(), handler);
            }
            let watch_dirs = code_repo_temps
                .iter()
                .map(|t| super::super::super::watcher::WatchDir {
                    path: t.path().to_path_buf(),
                    recursive: true,
                })
                .collect();
            let watcher = super::super::super::watcher::Watcher::new(
                super::super::super::watcher::WatchSetup { dirs: watch_dirs },
                std::sync::Arc::clone(&dispatcher),
            )
            .unwrap();
            // Let FSEvents arm the watches before the wave writes.
            std::thread::sleep(Duration::from_millis(150));
            Some(watcher)
        } else {
            None
        };

        // Run the wave — materializes `files` into each of `targets`, each
        // write registering suppression on the shared dispatcher.
        let deps = WaveDeps {
            suppression: dispatcher.suppression_handle(),
            emit: Arc::new(|_, _| ()),
            cache: None,
            projection_enabled: false,
        };
        let coord = std::sync::Arc::new(WaveCoordinator::new());

        // AC-1 Pareto: aggregate the per-target `RepoStageProfile`s into one
        // stage breakdown. `sum_target_ns` is the wall-time *summed across
        // targets*; dividing by the actual `wave_ms` yields the effective
        // target-parallelism — ~1.0 today (targets run back-to-back), and the
        // headline AC-3/AC-6 number once Lever A flattens the per-target loop
        // (it should climb toward `cores`). Buckets attribute where the time
        // goes (write-bound → Lever A; hash/read-bound on skips → Lever B).
        // SR-0 note: `total_ns` now also includes each target's `lock_wait_ns`
        // (the repo-lock acquire), so under contention this ratio can read
        // slightly above the back-to-back baseline — it is a diagnostic
        // `eprintln`, not asserted.
        let print_pareto = |label: &str, result: &WaveResult, wave_ms: u64| {
            let profiles = &result.stage_profiles;
            let sum = |f: fn(&RepoStageProfile) -> u128| -> u128 { profiles.iter().map(f).sum() };
            let written: usize = result.per_repo.values().map(|c| c.written).sum();
            let skipped: usize = result
                .per_repo
                .values()
                .map(|c| c.skipped_already_current + c.skipped_locally_modified)
                .sum();
            let sum_target_ns = sum(|p| p.total_ns);
            let ms = |ns: u128| (ns as f64) / 1_000_000.0;
            let wave_ns = (wave_ms.max(1) as u128).saturating_mul(1_000_000);
            eprintln!(
                "cascade_pareto[{label}]: targets={} written={written} \
                 skipped={skipped} wave_ms={wave_ms} sum_target_ms={:.1} \
                 target_parallelism={:.2}x | lockfile_persist={:.1} \
                 lock_wait={:.1} other={:.1}",
                profiles.len(),
                ms(sum_target_ns),
                sum_target_ns as f64 / wave_ns as f64,
                ms(sum(|p| p.lockfile_persist_ns)),
                ms(sum(|p| p.lock_wait_ns)),
                ms(sum(|p| p.other_ns)),
            );
        };

        // A wave builder reused for the cold pass (below) and the warm pass
        // (after the probe). Each call rebuilds a fresh `team_switch_wave`
        // from cloned fixtures and drives it through the same coordinator —
        // the second pass therefore hits a now-populated lockfile and takes
        // the skip-already-current branch (the AC-1 cold-vs-warm split).
        let run_wave = || {
            let ctx = ctx.clone();
            let affected = affected.clone();
            let deps = deps.clone();
            let coord = std::sync::Arc::clone(&coord);
            async move {
                let wave = cutover_wave(ctx, affected);
                let started = Instant::now();
                let result = coord.enqueue(wave, deps).await.unwrap();
                let ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
                (result, ms)
            }
        };

        // ---- Cold pass: every name materializes. ----
        let (result, wave_ms) = run_wave().await;
        assert!(!result.cancelled, "cold wave was cancelled mid-run");
        print_pareto("cold", &result, wave_ms);

        // Probe + AC-0 gate run only with a live watcher (`watcher_on`);
        // with `CS_CASCADE_WATCHER=0` no handler can fire, so the cold/warm
        // Pareto lines are the sole output of this run.
        if watcher_on {
            // Probe FIRST — before any settle. A genuine, un-suppressed user
            // edit to a watched tree, timed immediately at wave completion so
            // it competes with the *live* event backlog. That contention IS
            // the post-wave hover hang (AC-0); measuring it after a settle
            // sleep would drain the backlog and mask the very thing under test.
            let probe = code_repo_temps[0].path().join("PROBE_user_edit.md");
            let probe_started = Instant::now();
            std::fs::write(&probe, b"user edit during/after wave\n").unwrap();
            // Wait generously past the budget so a genuine hang is reported as
            // a budget violation by the AC-0 assert below, not a bare panic.
            let deadline = Instant::now() + Duration::from_millis(budget_ms.max(5_000) + 2_000);
            let mut probe_ms: Option<u64> = None;
            while Instant::now() < deadline {
                let fired = log
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|p| p.ends_with("PROBE_user_edit.md"));
                if fired {
                    probe_ms =
                        Some(probe_started.elapsed().as_millis().min(u64::MAX as u128) as u64);
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            // A probe that never arrives within the (budget-relative) deadline
            // IS the worst-case hang — record the elapsed time so the AC-0
            // assert reports it informatively instead of a generic panic.
            let probe_ms = probe_ms.unwrap_or_else(|| {
                probe_started.elapsed().as_millis().min(u64::MAX as u128) as u64
            });

            // Now let the remaining backlog drain and snapshot the full
            // suppressed-vs-leaked breakdown (AC-1).
            std::thread::sleep(Duration::from_millis(settle_ms));
            let post_wave = dispatcher.counts();

            eprintln!(
                "cascade_harness: targets={} files={} (={} mats) wave_ms={} \
                 suppressed={} delivered={} debounced={} no_handler={} \
                 probe_latency_ms={} (budget={})",
                targets,
                files,
                targets * files,
                wave_ms,
                post_wave.suppressed,
                post_wave.delivered,
                post_wave.debounced,
                post_wave.no_handler,
                probe_ms,
                budget_ms,
            );

            // Sanity: the watcher actually observed the wave's event flood. A
            // dead watch or a no-op wave would leave every bucket at zero and
            // make a green result meaningless. Assert on the UNION, not on
            // `suppressed` alone — under a short grace, late-delivered wave
            // events land in delivered/debounced rather than suppressed, so a
            // `suppressed`-only check is grace-timing-sensitive and can flake.
            assert!(
                post_wave.suppressed + post_wave.delivered + post_wave.debounced > 0,
                "watcher observed no events — harness exercised nothing \
                 (suppressed={}, delivered={}, debounced={}); check watch arming / wave",
                post_wave.suppressed,
                post_wave.delivered,
                post_wave.debounced,
            );

            // AC-0 gate: post-wave UI-handler latency under budget (no hang).
            assert!(
                probe_ms < budget_ms,
                "post-wave probe hang: {} ms >= {} ms budget \
                 (targets={}, files={}, suppressed={}, delivered={})",
                probe_ms,
                budget_ms,
                targets,
                files,
                post_wave.suppressed,
                post_wave.delivered,
            );
        }

        // ---- Warm pass: lockfiles now populated, every name should skip.
        // Drives the AC-1 cold-vs-warm split — a warm wave's Pareto should
        // be dominated by resolve/read/hash on skips (the Lever B target),
        // with `written` near zero.
        let (warm, warm_ms) = run_wave().await;
        assert!(!warm.cancelled, "warm wave was cancelled mid-run");
        print_pareto("warm", &warm, warm_ms);
    }

    // ===== SLW-FX-13 install-boundary guard (AC-6) ===================

    /// AC-6: the managed-surface guard SKIPS an UNMANAGED `code_repo_path`
    /// at `run_repo_reconcile` — a deliberate no-op: ZERO writes land in the target
    /// (the store persist never runs), no per-repo work is
    /// counted, and it is NOT surfaced as an error (the normal case for a
    /// raw-attached checkout). Mirrors the positive stress setup but deliberately
    /// leaves the code repo UNREGISTERED (not a worktree, not under `users/`).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_repo_skips_unmanaged_target_zero_writes() {
        let _g = super::super::super::test_support::env_guard();
        let empty_core = tempfile::TempDir::new().unwrap();
        std::env::set_var("CODESENSE_CORE_DEV_PATH", empty_core.path());

        let codesense_home = tempfile::TempDir::new().unwrap();
        let ctx = ResolverContext {
            context_root: None,
            codesense_home: codesense_home.path().to_path_buf(),
            user_id: 42,
            org_slug: "test/api".to_string(),
            team_name: Some("default".to_string()),
        };

        // One install source-layer skill so a MANAGED target would materialize.
        let ws_install = codesense_home
            .path()
            .join("users/42/orgs/test/api/default/install/.claude/skills");
        std::fs::create_dir_all(&ws_install).unwrap();
        std::fs::write(ws_install.join("good.md"), b"good content").unwrap();

        // UNMANAGED target: a plausible user checkout — NOT under `users/`, NOT
        // registered in the worktree index (deliberately no `register_test_worktrees`).
        let user_checkout = tempfile::TempDir::new().unwrap();
        let affected = vec![AttachedCodeRepo::attached(
            slug(),
            user_checkout.path().to_path_buf(),
        )];

        let dispatcher = Arc::new(super::super::super::dispatcher::Dispatcher::new(
            0, 100, 60_000, 60_000,
        ));
        let deps = WaveDeps {
            suppression: dispatcher.suppression_handle(),
            emit: Arc::new(|_, _| ()),
            cache: None,
            projection_enabled: false,
        };
        let coord = Arc::new(WaveCoordinator::new());
        let wave = Wave::new(
            WaveReason::MigrationCutover {
                explicit_names: None,
            },
            ctx,
            affected,
        );
        let result = coord.enqueue(wave, deps).await.unwrap();

        // Graceful skip, NOT an error — the guard makes it a deliberate no-op.
        assert!(
            !result
                .errors
                .iter()
                .any(|e| e.code_repo_path == user_checkout.path()),
            "unmanaged target must be a silent skip, not a WaveError, got: {:?}",
            result.errors
        );
        // Safe skip: ZERO writes — no `.claude` tree landed in the checkout.
        assert!(
            !user_checkout.path().join(".claude").exists(),
            "unmanaged target must receive NO install writes"
        );
        // No per-repo work counted — the fan-out never ran.
        assert_eq!(
            result
                .per_repo
                .get(user_checkout.path())
                .map(|c| c.written)
                .unwrap_or(0),
            0,
            "no files should be written to an unmanaged target"
        );
    }

    // ===== regression: empty names must not panic the materialize pool

    /// A `Startup` wave fired against a team with zero resolvable names
    /// (every layer empty) drives the
    /// materialize pool with `total_names == 0`. Earlier shapes of the
    /// parallel restructure panicked here via `slice::chunks(0)`; pin
    /// it as a regression test.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_repo_handles_empty_names_without_panicking() {
        let _g = super::super::super::test_support::env_guard();
        let empty_core = tempfile::TempDir::new().unwrap();
        std::env::set_var("CODESENSE_CORE_DEV_PATH", empty_core.path());

        let codesense_home = tempfile::TempDir::new().unwrap();
        let ctx = ResolverContext {
            context_root: None,
            codesense_home: codesense_home.path().to_path_buf(),
            user_id: 42,
            org_slug: "test/api".to_string(),
            team_name: Some("empty".to_string()),
        };
        let code_repo = tempfile::TempDir::new().unwrap();
        let mut affected = vec![AttachedCodeRepo::attached(
            slug(),
            code_repo.path().to_path_buf(),
        )];
        register_test_worktrees(&ctx, &mut affected); // SLW-FX-13: managed-target guard

        let dispatcher = Arc::new(super::super::super::dispatcher::Dispatcher::new(
            0, 100, 60_000, 60_000,
        ));
        let deps = WaveDeps {
            suppression: dispatcher.suppression_handle(),
            emit: Arc::new(|_, _| ()),
            cache: None,
            projection_enabled: false,
        };
        let coord = Arc::new(WaveCoordinator::new());
        let wave = Wave::new(
            WaveReason::MigrationCutover {
                explicit_names: None,
            },
            ctx,
            affected.clone(),
        );
        let result = coord.enqueue(wave, deps).await.unwrap();

        assert!(
            result.errors.is_empty(),
            "wave with empty names produced errors: {:?}",
            result.errors
        );
        let counts = result.per_repo.get(code_repo.path()).unwrap();
        assert_eq!(counts.written, 0);
        assert_eq!(counts.skipped_already_current, 0);
        assert_eq!(counts.skipped_locally_modified, 0);
        assert_eq!(counts.unmaterialized, 0);

        std::env::remove_var("CODESENSE_CORE_DEV_PATH");
    }

    // ===== regression: projection entries must not be dropped by fold

    /// `story_linker` materializes projection entries (`projection_key
    /// = Some(_)`) into the same lockfile the wave persists, and those
    /// entries can share `logical_name` with a wave entry. An earlier
    /// shape of the parallel fold collapsed both kinds into one
    /// `HashMap<String, MaterializedEntry>` keyed by `logical_name`,
    /// silently dropping the projection entry on rebuild. Pin the
    /// invariant: a wave persists every projection entry verbatim,
    /// regardless of whether a wave entry shares its `logical_name`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_repo_preserves_projection_entries_through_fold() {
        let _g = super::super::super::test_support::env_guard();
        let empty_core = tempfile::TempDir::new().unwrap();
        std::env::set_var("CODESENSE_CORE_DEV_PATH", empty_core.path());

        let codesense_home = tempfile::TempDir::new().unwrap();
        let ctx = ResolverContext {
            context_root: None,
            codesense_home: codesense_home.path().to_path_buf(),
            user_id: 42,
            org_slug: "test/api".to_string(),
            team_name: Some("default".to_string()),
        };

        let logical = ".claude/skills/shared.md";
        let code_repo = tempfile::TempDir::new().unwrap();
        let mut affected = vec![AttachedCodeRepo::attached(
            slug(),
            code_repo.path().to_path_buf(),
        )];
        register_test_worktrees(&ctx, &mut affected); // SLW-FX-13: managed-target guard

        // Seed the store with a story_linker-style projection entry. With the
        // install-layer materialize fan-out retired, `run_repo_reconcile` reads the lockfile
        // through and persists it unchanged — the invariant now is simply that a
        // forward wave never drops the projection entries it read.
        let projection_entry = super::super::super::lockfile::MaterializedEntry {
            last_materialized_size: None,
            logical_name: logical.to_string(),
            dest_path: code_repo.path().join(logical),
            source_layer: LayerId::TeamProjection,
            source_path: PathBuf::from("/fake/source/projection"),
            last_materialized_hash: [0xAA; 32],
            last_materialized_mtime_ns: 1234,
            projection_key: Some("story".to_string()),
            source_oid: None,
        };
        let seeded = super::super::super::lockfile::Lockfile {
            schema_version: 3,
            org_slug: ctx.org_slug.clone(),
            team_name: ctx.team_name.clone(),
            code_repo_path: code_repo.path().to_path_buf(),
            entries: vec![projection_entry.clone()],
        };
        seed_store(
            codesense_home.path(),
            ctx.user_id,
            &affected[0].slug,
            &seeded,
        );

        let dispatcher = Arc::new(super::super::super::dispatcher::Dispatcher::new(
            0, 100, 60_000, 60_000,
        ));
        let deps = WaveDeps {
            suppression: dispatcher.suppression_handle(),
            emit: Arc::new(|_, _| ()),
            cache: None,
            projection_enabled: false,
        };
        let coord = Arc::new(WaveCoordinator::new());
        let wave = Wave::new(
            WaveReason::MigrationCutover {
                explicit_names: None,
            },
            ctx.clone(),
            affected.clone(),
        );
        let result = coord.enqueue(wave, deps).await.unwrap();

        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        // Read the persisted lockfile FROM THE STORE (the wave's sole writer now) and
        // assert the projection entry SURVIVES the wave verbatim — keyed by its composite
        // identity (logical_name, projection_key).
        let store = super::super::super::sync_state_store::SyncStateStore::open(
            super::super::super::sync_state_store::default_store_path(codesense_home.path()),
        )
        .unwrap();
        let persisted = store
            .read(&super::super::super::sync_state_store::TargetKey::from_ctx(
                &ctx,
                code_repo.path(),
            ))
            .unwrap()
            .expect("target persisted to the store");
        let preserved_projection = persisted
            .entries
            .iter()
            .find(|e| e.logical_name == logical && e.projection_key.as_deref() == Some("story"))
            .expect("projection entry must NOT be dropped by the wave");
        assert_eq!(
            preserved_projection, &projection_entry,
            "projection entry must pass through the wave unchanged"
        );

        std::env::remove_var("CODESENSE_CORE_DEV_PATH");
    }

    /// A reconcile wave (the common boot/team-switch no-op) must
    /// issue ZERO `materialized_entry` row ops — the reconcile arm persists through the
    /// entries-free `persist_reconcile` seam (no full `upsert`, SW-CP-35) — yet still
    /// advance the target's `updated_at` (its recency). A
    /// FRESH target with no store row still gets its row created through the same seam.
    /// Both proven through the real wave harness with a persistent trigger probe on the
    /// shared store DB (which observes writes; the read-side decode-elision is proven
    /// separately by `reconcile_noop_reads_no_entries_when_table_hidden`).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reconcile_noop_touches_recency_with_zero_entry_ops() {
        let _g = super::super::super::test_support::env_guard();
        let empty_core = tempfile::TempDir::new().unwrap();
        std::env::set_var("CODESENSE_CORE_DEV_PATH", empty_core.path());

        let codesense_home = tempfile::TempDir::new().unwrap();
        let ctx = ResolverContext {
            context_root: None,
            codesense_home: codesense_home.path().to_path_buf(),
            user_id: 42,
            org_slug: "test/api".to_string(),
            team_name: Some("default".to_string()),
        };

        // An EXISTING target seeded with a real projection entry — a subsequent no-op
        // reconcile must NOT touch that child row.
        let existing_repo = tempfile::TempDir::new().unwrap();
        // A FRESH target with no seeded store row — the reconcile must CREATE its row.
        let fresh_repo = tempfile::TempDir::new().unwrap();
        let mut affected = vec![
            AttachedCodeRepo::attached(slug(), existing_repo.path().to_path_buf()),
            AttachedCodeRepo::attached(slug(), fresh_repo.path().to_path_buf()),
        ];
        register_test_worktrees(&ctx, &mut affected); // SLW-FX-13: managed-target guard

        let projection_entry = super::super::super::lockfile::MaterializedEntry {
            last_materialized_size: None,
            logical_name: ".claude/skills/shared.md".to_string(),
            dest_path: existing_repo.path().join(".claude/skills/shared.md"),
            source_layer: LayerId::TeamProjection,
            source_path: PathBuf::from("/fake/source/projection"),
            last_materialized_hash: [0xAA; 32],
            last_materialized_mtime_ns: 1234,
            projection_key: Some("story".to_string()),
            source_oid: None,
        };
        seed_store(
            codesense_home.path(),
            ctx.user_id,
            &affected[0].slug,
            &super::super::super::lockfile::Lockfile {
                schema_version: 3,
                org_slug: ctx.org_slug.clone(),
                team_name: ctx.team_name.clone(),
                code_repo_path: existing_repo.path().to_path_buf(),
                entries: vec![projection_entry],
            },
        );

        // Open the SAME store DB the wave writes through and install a persistent
        // child-row op counter (triggers live in the schema, so they fire for the
        // wave's own connections too). A raw WAL connection on the same file — the
        // store's own `conn()` is module-private — is enough to install/read the probe.
        let store_path =
            super::super::super::sync_state_store::default_store_path(codesense_home.path());
        let store =
            super::super::super::sync_state_store::SyncStateStore::open(&store_path).unwrap();
        {
            let probe_conn = crate::shared::sqlite::open_wal_db(&store_path).unwrap();
            probe_conn
                .execute_batch(
                    "CREATE TABLE _rewrite_probe (n INTEGER NOT NULL);\n\
                     INSERT INTO _rewrite_probe VALUES (0);\n\
                     CREATE TRIGGER _probe_ins AFTER INSERT ON materialized_entry \
                         BEGIN UPDATE _rewrite_probe SET n = n + 1; END;\n\
                     CREATE TRIGGER _probe_del AFTER DELETE ON materialized_entry \
                         BEGIN UPDATE _rewrite_probe SET n = n + 1; END;\n\
                     CREATE TRIGGER _probe_upd AFTER UPDATE ON materialized_entry \
                         BEGIN UPDATE _rewrite_probe SET n = n + 1; END;",
                )
                .unwrap();
        }
        let probe = || -> i64 {
            let c = crate::shared::sqlite::open_wal_db(&store_path).unwrap();
            c.query_row("SELECT n FROM _rewrite_probe", [], |r| r.get::<_, i64>(0))
                .unwrap()
        };
        let existing_before = store
            .updated_at(existing_repo.path())
            .unwrap()
            .expect("seeded target has recency");

        // A strictly-later wall clock so the monotonic recency clamp can advance.
        std::thread::sleep(std::time::Duration::from_millis(2));

        let dispatcher = Arc::new(super::super::super::dispatcher::Dispatcher::new(
            0, 100, 60_000, 60_000,
        ));
        let deps = WaveDeps {
            suppression: dispatcher.suppression_handle(),
            emit: Arc::new(|_, _| ()),
            cache: None,
            projection_enabled: false,
        };
        let coord = Arc::new(WaveCoordinator::new());
        // `Startup` routes to `run_repo_reconcile`.
        let wave = Wave::new(
            WaveReason::MigrationCutover {
                explicit_names: None,
            },
            ctx.clone(),
            affected.clone(),
        );
        let result = coord.enqueue(wave, deps).await.unwrap();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        // The existing, unchanged target issued ZERO child-row ops (touch, not upsert).
        assert_eq!(
            probe(),
            0,
            "a no-op reconcile of an unchanged target must not touch any materialized_entry row"
        );
        // …yet its recency advanced (the reaper's `updated_at` bump is preserved).
        let existing_after = store.updated_at(existing_repo.path()).unwrap().unwrap();
        assert!(
            existing_after > existing_before,
            "the no-op reconcile still advances the target's recency \
             (before={existing_before:?}, after={existing_after:?})"
        );
        // The FRESH target's row was created (a bare UPDATE would have lost it).
        assert!(
            store.updated_at(fresh_repo.path()).unwrap().is_some(),
            "a fresh target's row must be created by the reconcile"
        );

        std::env::remove_var("CODESENSE_CORE_DEV_PATH");
    }

    /// SW-CP-35: the reconcile shell persists WITHOUT ever touching a
    /// `materialized_entry` row. The trigger probe
    /// above catches WRITES but not a READ decode (SQLite triggers don't fire on
    /// `SELECT`), and rusqlite is built without the `trace` feature, so there is no
    /// statement hook. Instead we HIDE the entries table: rename it out of the way on the
    /// shared store DB, then run the no-op reconcile wave. Any surviving
    /// `… FROM materialized_entry` on either the read or persist path would fail with
    /// "no such table" → a `WaveError`; a clean wave that still advances recency proves
    /// ZERO entries-table access across BOTH paths. (A `Startup` wave routes solely to
    /// `run_repo_reconcile`; the projection engine that legitimately reads/writes entries
    /// is enqueued separately.)
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reconcile_noop_reads_no_entries_when_table_hidden() {
        let _g = super::super::super::test_support::env_guard();
        let empty_core = tempfile::TempDir::new().unwrap();
        std::env::set_var("CODESENSE_CORE_DEV_PATH", empty_core.path());

        let codesense_home = tempfile::TempDir::new().unwrap();
        let ctx = ResolverContext {
            context_root: None,
            codesense_home: codesense_home.path().to_path_buf(),
            user_id: 42,
            org_slug: "test/api".to_string(),
            team_name: Some("default".to_string()),
        };

        // An existing target seeded WITH a real projection entry — so `materialized_entry`
        // holds rows a full `read_through` would decode. If the read still decoded them, it
        // would hit the hidden table and error.
        let existing_repo = tempfile::TempDir::new().unwrap();
        let mut affected = vec![AttachedCodeRepo::attached(
            slug(),
            existing_repo.path().to_path_buf(),
        )];
        register_test_worktrees(&ctx, &mut affected); // SLW-FX-13: managed-target guard

        let projection_entry = super::super::super::lockfile::MaterializedEntry {
            last_materialized_size: None,
            logical_name: ".claude/skills/shared.md".to_string(),
            dest_path: existing_repo.path().join(".claude/skills/shared.md"),
            source_layer: LayerId::TeamProjection,
            source_path: PathBuf::from("/fake/source/projection"),
            last_materialized_hash: [0xAA; 32],
            last_materialized_mtime_ns: 1234,
            projection_key: Some("story".to_string()),
            source_oid: None,
        };
        seed_store(
            codesense_home.path(),
            ctx.user_id,
            &affected[0].slug,
            &super::super::super::lockfile::Lockfile {
                schema_version: 3,
                org_slug: ctx.org_slug.clone(),
                team_name: ctx.team_name.clone(),
                code_repo_path: existing_repo.path().to_path_buf(),
                entries: vec![projection_entry],
            },
        );

        let store_path =
            super::super::super::sync_state_store::default_store_path(codesense_home.path());
        let store =
            super::super::super::sync_state_store::SyncStateStore::open(&store_path).unwrap();
        let before = store
            .updated_at(existing_repo.path())
            .unwrap()
            .expect("seeded target has recency");

        // Hide the entries table. `user_version` is already stamped current, so re-opening
        // the store applies no schema migration that would recreate it.
        {
            let c = crate::shared::sqlite::open_wal_db(&store_path).unwrap();
            c.execute_batch("ALTER TABLE materialized_entry RENAME TO materialized_entry_hidden;")
                .unwrap();
        }

        // A strictly-later wall clock so the monotonic recency clamp can advance.
        std::thread::sleep(std::time::Duration::from_millis(2));

        let dispatcher = Arc::new(super::super::super::dispatcher::Dispatcher::new(
            0, 100, 60_000, 60_000,
        ));
        let deps = WaveDeps {
            suppression: dispatcher.suppression_handle(),
            emit: Arc::new(|_, _| ()),
            cache: None,
            projection_enabled: false,
        };
        let coord = Arc::new(WaveCoordinator::new());
        let wave = Wave::new(
            WaveReason::MigrationCutover {
                explicit_names: None,
            },
            ctx.clone(),
            affected.clone(),
        );
        let result = coord.enqueue(wave, deps).await.unwrap();

        // A `… FROM materialized_entry` on the read or persist path would surface as a
        // store error here — the reconcile touches neither.
        assert!(
            result.errors.is_empty(),
            "the reconcile must not read or write the entries table (errors: {:?})",
            result.errors
        );
        // …and it still advanced the reaper's recency through the entries-free persist seam.
        let after = store.updated_at(existing_repo.path()).unwrap().unwrap();
        assert!(
            after > before,
            "the reconcile still advances recency without the entries table \
             (before={before:?}, after={after:?})"
        );

        std::env::remove_var("CODESENSE_CORE_DEV_PATH");
    }

    /// A team whose config still holds a stale `agent-packs.lock` (one that declares a
    /// repo-scope `.mcp.json` fragment) gets nothing from it: a reconcile wave leaves the
    /// worktree byte-identical (golden), writes no agent config, and raises no error.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reconcile_leaves_the_worktree_unchanged_with_a_stale_lock_file() {
        let _g = super::super::super::test_support::env_guard();
        let empty_core = tempfile::TempDir::new().unwrap();
        std::env::set_var("CODESENSE_CORE_DEV_PATH", empty_core.path());

        let codesense_home = tempfile::TempDir::new().unwrap();
        let ctx = ResolverContext {
            context_root: None,
            codesense_home: codesense_home.path().to_path_buf(),
            user_id: 42,
            org_slug: "test/api".to_string(),
            team_name: Some("default".to_string()),
        };
        let repo = tempfile::TempDir::new().unwrap();
        std::fs::write(repo.path().join("README.md"), b"hello\n").unwrap();
        std::fs::create_dir_all(repo.path().join(".claude")).unwrap();
        std::fs::write(repo.path().join(".claude/settings.json"), b"{}\n").unwrap();
        let mut affected = vec![AttachedCodeRepo::attached(
            slug(),
            repo.path().to_path_buf(),
        )];
        register_test_worktrees(&ctx, &mut affected); // SLW-FX-13: managed-target guard

        let org_root = crate::context_repo::path_to_org_root(
            &ctx.codesense_home,
            &ctx.principal(),
            &ctx.org_slug,
        )
        .unwrap();
        let config = org_root.join("default").join("config");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join("agent-packs.lock"),
            b"schema_version: 1\npacks:\n  alpha:\n    version: 1.0.0\n    source:\n      kind: team\n    config_additions:\n      claude:\n        - scope: repo\n          target: .mcp.json\n          fragment:\n            mcpServers:\n              alpha:\n                command: /alpha\n",
        )
        .unwrap();

        let snapshot = |root: &Path| -> Vec<(PathBuf, Vec<u8>)> {
            let mut files: Vec<(PathBuf, Vec<u8>)> = walkdir::WalkDir::new(root)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_file())
                .map(|e| {
                    (
                        e.path().strip_prefix(root).unwrap().to_path_buf(),
                        std::fs::read(e.path()).unwrap(),
                    )
                })
                .collect();
            files.sort();
            files
        };
        let before = snapshot(repo.path());

        let dispatcher = Arc::new(super::super::super::dispatcher::Dispatcher::new(
            0, 100, 60_000, 60_000,
        ));
        let deps = WaveDeps {
            suppression: dispatcher.suppression_handle(),
            emit: Arc::new(|_, _| ()),
            cache: None,
            projection_enabled: false,
        };
        let coord = Arc::new(WaveCoordinator::new());
        let wave = Wave::new(
            WaveReason::MigrationCutover {
                explicit_names: None,
            },
            ctx.clone(),
            affected.clone(),
        );
        let result = coord.enqueue(wave, deps).await.unwrap();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        assert_eq!(
            snapshot(repo.path()),
            before,
            "a stale lock file must not change the worktree"
        );
        assert!(!repo.path().join(".mcp.json").exists());

        std::env::remove_var("CODESENSE_CORE_DEV_PATH");
    }

    /// SW-IS-3b AC-3 (fail-closed): with the JSON write gone, the store is the wave's
    /// SOLE writer — a store that cannot be opened must FAIL the wave with a `WaveError`,
    /// never be silently swallowed (which would lose the wave's materialize state).
    /// Inject an open failure by planting a FILE where the `install-state/` dir must be
    /// created, so `open_sync_state_store` returns `None` (a real environment fault, not the
    /// corruption SW-IS-4 self-heals).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wave_fails_closed_when_the_store_cannot_be_opened() {
        let _g = super::super::super::test_support::env_guard();
        let empty_core = tempfile::TempDir::new().unwrap();
        std::env::set_var("CODESENSE_CORE_DEV_PATH", empty_core.path());

        let codesense_home = tempfile::TempDir::new().unwrap();
        let ctx = ResolverContext {
            context_root: None,
            codesense_home: codesense_home.path().to_path_buf(),
            user_id: 42,
            org_slug: "test/api".to_string(),
            team_name: Some("default".to_string()),
        };

        // A wave-resolvable name, so the wave has real state it must persist.
        let ws_install = codesense_home
            .path()
            .join("users/42/orgs/test/api/default/install/.claude/skills");
        std::fs::create_dir_all(&ws_install).unwrap();
        std::fs::write(ws_install.join("shared.md"), b"wave content").unwrap();

        let code_repo = tempfile::TempDir::new().unwrap();
        let mut affected = vec![AttachedCodeRepo::attached(
            slug(),
            code_repo.path().to_path_buf(),
        )];
        register_test_worktrees(&ctx, &mut affected);

        // Block the store: a FILE where `install-state/` must be created makes `open`
        // fail on parent-dir creation → `open_sync_state_store` returns None.
        std::fs::write(codesense_home.path().join("install-state"), b"not a dir").unwrap();

        let dispatcher = Arc::new(super::super::super::dispatcher::Dispatcher::new(
            0, 100, 60_000, 60_000,
        ));
        let deps = WaveDeps {
            suppression: dispatcher.suppression_handle(),
            emit: Arc::new(|_, _| ()),
            cache: None,
            projection_enabled: false,
        };
        let coord = Arc::new(WaveCoordinator::new());
        let wave = Wave::new(
            WaveReason::MigrationCutover {
                explicit_names: None,
            },
            ctx,
            affected.clone(),
        );
        let result = coord.enqueue(wave, deps).await.unwrap();

        assert!(
            result
                .errors
                .iter()
                .any(|e| e.message.contains("install store")),
            "a wave that cannot open the store must fail closed with a WaveError, got: {:?}",
            result.errors
        );

        std::env::remove_var("CODESENSE_CORE_DEV_PATH");
    }

    // SW-CP-34 (S2): `stash_active_projection_on_team_change` stashes the active
    // team's `(ctx, source_repos)` (read by the reverse machinery and the agent
    // reconcile). The seam takes a stash-less coordinator from "no active team" to
    // a fully-stashed active target, WITHOUT any watcher/deps_builder.
    #[test]
    fn stash_active_projection_on_team_change_populates_the_live_projection_stash() {
        let _g = super::super::super::test_support::env_guard();
        let coord = Arc::new(WaveCoordinator::new());
        // Precondition: nothing stashed.
        assert!(
            coord.active_target_repos().is_none(),
            "no team active yet → no projection target stashed"
        );

        let ctx = ResolverContext {
            context_root: None,
            codesense_home: PathBuf::from("/home/dev/.codesense"),
            user_id: 7,
            org_slug: "acme/api".to_string(),
            team_name: Some("default".to_string()),
        };
        let repos = vec![slug()];
        coord.stash_active_projection_on_team_change(&ctx, &repos);

        let (stashed_ctx, stashed_repos) = coord
            .active_target_repos()
            .expect("the seam must stash the active team's projection target");
        assert_eq!(
            stashed_ctx.team_name.as_deref(),
            Some("default"),
            "the active team's ctx is stashed"
        );
        assert_eq!(stashed_repos, repos, "the team's source_repos are stashed");
    }

    // SW-CP-11 T4 (AC-3, branch a): upstream changed while the surface was closed
    // → forward-materialized on reopen (dest catches up to source). Baseline=OLD,
    // source=NEW (upstream edit), dest=OLD (untouched).

    // SW-LN-B29: the FE names a local (no-remote) repo by its bare `owner/repo`, which parses as
    // a GitHub slug. The attach reconcile must run — and record the worktree's store rows — under
    // the TEAM's configured local slug, or the team's fan-out and refocus catch-up (which
    // enumerate by the team's slug) never find the worktree again.

    // SW-CP-11 T4 (AC-3, branch b): a dest edited while closed, source unchanged →
    // reverse-promoted on reopen, NOT clobbered. Baseline=OLD, source=OLD
    // (unchanged), dest=EDITED (local edit made while closed).

    // SW-CP-11 T4 (AC-3, branch c): both sides diverged from baseline → the
    // EXPLICIT (agent-initiated) reconcile still surfaces the conflict — SW-XS-4:
    // on its own result (`WaveResult::conflicts`), never a background
    // `conflict-detected` event or registry row — and writes neither side.

    // SW-CP-11 T4 (AC-3, delete posture): a dest DELETED while the surface was
    // closed is RESTORED from the source on reopen (forward), never reverse-
    // promoted as a source delete — codesense never auto-deletes a source file
    // from a dest deletion. Baseline=OLD, source=OLD, dest absent.

    // SW-CP-11 T4 (AC-3, orphan-delete posture): if a source is deleted upstream
    // while the surface is closed but the user edited the projected dest, reconcile-
    // on-open must NOT clobber the edit. The force-unmaterialize keeps the locally-
    // modified dest (now untracked), dropping only the lockfile entry. RED before
    // the guard: force-unmaterialize deleted the dest unconditionally.

    // ===== SLW-RS-2 AC-1/AC-8: reverse_entry_dirty (the sweep's per-entry gate) =====

    /// RF-perf-20: the shared reap primitive deletes the worktree's install target
    /// from the store (dropping the repo from `enumerate_attached_*`), and is
    /// idempotent.
    #[test]
    fn reap_detached_worktree_drops_install_state_idempotent() {
        let _g = super::super::super::test_support::env_guard();
        use super::super::super::lockfile::Lockfile;
        use super::super::super::sync_state_store::{
            default_store_path, SyncStateStore, TargetKey,
        };

        let home = tempfile::TempDir::new().unwrap();
        let ctx = ResolverContext {
            context_root: None,
            codesense_home: home.path().to_path_buf(),
            user_id: 7,
            org_slug: "acme/api".to_string(),
            team_name: Some("default".to_string()),
        };
        let repo = tempfile::TempDir::new().unwrap();

        // A lingering install target for the repo.
        seed_store(
            home.path(),
            ctx.user_id,
            &slug(),
            &Lockfile {
                schema_version: 3,
                org_slug: ctx.org_slug.clone(),
                team_name: ctx.team_name.clone(),
                code_repo_path: repo.path().to_path_buf(),
                entries: vec![],
            },
        );
        let store = SyncStateStore::open(default_store_path(home.path())).unwrap();
        let target_present = || {
            store
                .read(&TargetKey::from_ctx(&ctx, repo.path()))
                .unwrap()
                .is_some()
        };
        assert!(target_present(), "precondition: install target present");

        reap_detached_worktree(home.path(), repo.path());
        assert!(
            !target_present(),
            "reap deletes the install target from the store"
        );

        // Idempotent: a second reap on the now-missing target is a clean no-op.
        reap_detached_worktree(home.path(), repo.path());
        assert!(!target_present());
    }

    /// SW-XS-4 (K7): the launch reconcile reaps every confirmed-gone LOCAL store
    /// target — a branch workspace / epic dir / managed clone has no worktree-index
    /// record, and the reverse sweeps that used to reap it in passing are gone. A
    /// present target, and a target whose whole container vanished (an unmounted
    /// volume), are kept.
    #[test]
    fn launch_reconcile_reaps_gone_store_targets_only() {
        let _g = super::super::super::test_support::env_guard();
        use super::super::super::sync_state_store::{default_store_path, SyncStateStore};
        let home = tempfile::TempDir::new().unwrap();
        let parent = tempfile::TempDir::new().unwrap();
        let present = parent.path().join("present");
        let gone = parent.path().join("gone");
        let unmounted = PathBuf::from("/nonexistent-volume-xs4/branch");
        std::fs::create_dir_all(&present).unwrap();
        for path in [&present, &gone, &unmounted] {
            seed_store(
                home.path(),
                7,
                &slug(),
                &Lockfile {
                    schema_version: 3,
                    org_slug: "acme/api".to_string(),
                    team_name: Some("default".to_string()),
                    code_repo_path: path.clone(),
                    entries: vec![],
                },
            );
        }
        assert_eq!(reap_gone_store_targets(home.path(), 7), 1);
        let left: Vec<PathBuf> = SyncStateStore::open(default_store_path(home.path()))
            .unwrap()
            .local_targets(Some(7))
            .unwrap()
            .into_iter()
            .map(|(_, _, p)| p)
            .collect();
        assert!(left.contains(&present), "a present target is kept");
        assert!(left.contains(&unmounted), "an unmounted container is kept");
        assert!(!left.contains(&gone), "the confirmed-gone target is reaped");
    }

    /// SW-XS-4 (H2): a conflict rides `conflicts`, not `errors`, so it never
    /// suppresses the last-reverse-sync stamp for the files that did publish; a
    /// real error still does.
    #[test]
    fn reverse_sync_stamp_ignores_conflicts() {
        let published = RepoWaveCounts {
            reverse_written: 1,
            ..Default::default()
        };
        assert!(should_stamp_reverse_sync(&[], &published));
        assert!(!should_stamp_reverse_sync(&[], &RepoWaveCounts::default()));
        let error = WaveError {
            code_repo_path: PathBuf::from("/r"),
            logical_name: None,
            message: "store unavailable".into(),
        };
        assert!(!should_stamp_reverse_sync(&[error], &published));
    }

    /// RF-perf-20 (AC-1/AC-4): the gone-path reap gate. Reaps ONLY a genuine
    /// removal (path absent + container present); never a whole-container
    /// disappearance (possible unmount) or a non-dir-in-place (transient). Exercised
    /// directly (no wave) so the AC-4 matrix is deterministic.
    #[test]
    fn try_reap_gone_worktree_gates_on_absence_and_container() {
        let _g = super::super::super::test_support::env_guard();
        use super::super::super::lockfile::Lockfile;
        use super::super::super::sync_state_store::{
            default_store_path, SyncStateStore, TargetKey,
        };

        let home = tempfile::TempDir::new().unwrap();
        let ctx = ResolverContext {
            context_root: None,
            codesense_home: home.path().to_path_buf(),
            user_id: 9,
            org_slug: "acme/api".to_string(),
            team_name: Some("default".to_string()),
        };
        let store = SyncStateStore::open(default_store_path(home.path())).unwrap();
        // Seed a store install target for `path`.
        let seed_target = |path: &Path| {
            seed_store(
                home.path(),
                ctx.user_id,
                &slug(),
                &Lockfile {
                    schema_version: 3,
                    org_slug: ctx.org_slug.clone(),
                    team_name: ctx.team_name.clone(),
                    code_repo_path: path.to_path_buf(),
                    entries: vec![],
                },
            );
        };
        let target_present = |path: &Path| {
            store
                .read(&TargetKey::from_ctx(&ctx, path))
                .unwrap()
                .is_some()
        };
        let attached = |path: PathBuf| AttachedCodeRepo::attached(slug(), path);

        // (1) Genuine removal: container present, worktree dir absent → REAP.
        let container = tempfile::TempDir::new().unwrap();
        let gone = container.path().join("removed-worktree");
        seed_target(&gone);
        assert!(
            try_reap_gone_worktree(&ctx, &attached(gone.clone())),
            "AC-1: reaps a container-present removal"
        );
        assert!(!target_present(&gone), "install target deleted on reap");

        // (2) Whole-container gone (parent also absent) → NOT reaped (unmount-safe).
        let orphan = PathBuf::from("/nonexistent-rfperf20/container/worktree");
        seed_target(&orphan);
        assert!(
            !try_reap_gone_worktree(&ctx, &attached(orphan.clone())),
            "AC-4: does not reap when the container is also gone"
        );
        assert!(
            target_present(&orphan),
            "install target kept when not reaped"
        );

        // (3) Non-dir-in-place (a file where the worktree was) → NOT reaped. `is_dir`
        // is false (caller enters the gone branch) but `try_exists()` is Ok(true).
        let file_repo = container.path().join("now-a-file");
        std::fs::write(&file_repo, b"x").unwrap();
        seed_target(&file_repo);
        assert!(
            !repo_path_resolves(&file_repo),
            "a file does not resolve as a repo"
        );
        assert!(
            !try_reap_gone_worktree(&ctx, &attached(file_repo.clone())),
            "AC-4: does not reap a non-dir-in-place (try_exists == Ok(true))"
        );
        assert!(
            target_present(&file_repo),
            "install target kept when not reaped"
        );
    }

    /// RF-perf-6 (AC-2): `prune_repo` evicts a removed worktree's key from all four
    /// per-repo coordinator maps, leaves a DIFFERENT repo's entries intact, and —
    /// the quiescence guard (R1) — refuses to evict a lock a wave still holds
    /// (`Arc::strong_count > 1`), reclaiming it only once quiescent. The three
    /// instance maps are per-`WaveCoordinator` (hermetic); the one process-global
    /// (`SURFACE_SYNC_HIGH_WATER`) is probed by a unique tempdir key, so this can't
    /// collide with concurrent tests.
    #[test]
    fn prune_repo_evicts_quiescent_entries_and_spares_held_locks() {
        let _g = super::super::super::test_support::env_guard();
        let coord = WaveCoordinator::new();
        let gone = tempfile::TempDir::new().unwrap();
        let kept = tempfile::TempDir::new().unwrap();
        let gone_p = gone.path().to_path_buf();
        let kept_p = kept.path().to_path_buf();
        let lock = |m: &StdMutex<HashMap<PathBuf, Arc<TokioMutex<()>>>>, p: &PathBuf| {
            m.lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(p.clone(), Arc::new(TokioMutex::new(())));
        };

        // Seed all three maps for the departing repo, plus a second repo to prove
        // selectivity.
        lock(&coord.wave_target_locks, &gone_p);
        lock(&coord.repo_locks, &gone_p);
        lock(&coord.repo_locks, &kept_p);

        coord.prune_repo(&gone_p);

        // The departing repo is gone from all three maps…
        assert!(!coord
            .wave_target_locks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&gone_p));
        assert!(!coord
            .repo_locks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&gone_p));
        // …but the other repo's lock survives.
        assert!(
            coord
                .repo_locks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&kept_p),
            "a different repo's lock is untouched"
        );

        // Quiescence guard: a HELD lock (an in-flight wave keeps a clone) is NOT
        // evicted; once the holder drops, the next prune reclaims it.
        let held = Arc::new(TokioMutex::new(()));
        coord
            .repo_locks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(gone_p.clone(), Arc::clone(&held));
        coord.prune_repo(&gone_p);
        assert!(
            coord
                .repo_locks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&gone_p),
            "a still-held lock (strong_count > 1) survives the prune"
        );
        drop(held);
        coord.prune_repo(&gone_p);
        assert!(
            !coord
                .repo_locks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&gone_p),
            "once quiescent (strong_count == 1) the lock is reclaimed"
        );
    }

    // ---------- SLW-FX-23: reverse-scoped synthetic-surface admission ----------

    // ---------- RF-perf-17: reverse session-liveness composition ----------

    // ---- SW-FX-53: bounded reverse-wave fan-out (cap + fair carryover) ----

    // ===== SLW-RS-5: collect_reverse_deletes (the reverse delete pass) =====

    // AC-3/AC-8/AD-3 + task 7: RENAME end-state. RS-5 ships a worktree rename
    // `a→b` as its correct END-STATE — the delete of `a` (this story; the delete
    // half is exercised by `rs5_stable_source_absent_dest_confirms_then_deletes`)
    // plus the adopt of `b` (the pre-existing event-driven `ReverseTarget::Adopt`
    // path, RS-7-hardened). The ATOMIC pairing — correlating the Absent old-dest
    // with the new Adopt by blake3 within one poll tick and emitting a single
    // `StructuralChange::Rename` (whose source op is an atomic `fs::rename`) —
    // needs RS-6's ungated poll walk to enumerate the "new file" half in the SAME
    // tick (AD-3). Until then it is the documented correlation seam, pinned
    // `#[ignore]` so it fails loudly the day someone wires it without RS-6.
    #[test]
    #[ignore = "SLW-RS-6: atomic delete+adopt correlation (StructuralChange::Rename) lands with the ungated poll walk (AD-3)"]
    fn rs5_atomic_rename_pairing_is_rs6_composed() {
        let _g = super::super::super::test_support::env_guard();
        // Intentionally unimplemented: RS-5's rename support is the delete half
        // (tested) + the existing adopt half. The atomic single-op pairing is
        // additive and arrives with RS-6 — this marker guards against wiring it
        // early, before the poll can see the new-file half.
        unimplemented!("atomic StructuralChange::Rename correlation is RS-6 (AD-3)");
    }

    // SW-CP-11 T4 (AC-3, orphan-delete posture, negative case): a CLEAN orphan
    // dest (matches the baseline — no local edit) whose source was deleted upstream
    // IS still removed on reconcile-on-open. Confirms the non-clobber guard does not
    // over-retain unmodified orphans.

    // SW-CP-11 T5 (AC-6): a reconcile-on-attach wave emits per-surface
    // `sync-wave-started` + `sync-wave-completed` carrying the surface's
    // `code_repo_path`, so SW-TS-3 can bind per-surface status chips + the MCP
    // `get_sync_status` tool. Engine-side events only — TS-3 owns the surface.

    // ---------- SLW-RI-1: engine runtime plane isolation ----------

    /// SLW-RI-1 helper: minimal `WaveDeps` (recording-free emit, no cache) so
    /// these tests exercise the *spawn/routing* seam, not the wave body.
    fn ri1_deps() -> WaveDeps {
        let dispatcher = Arc::new(super::super::super::dispatcher::Dispatcher::new(
            0, 100, 60_000, 60_000,
        ));
        let emit: Arc<dyn Fn(&'static str, serde_json::Value) + Send + Sync> =
            Arc::new(|_topic, _payload| {});
        WaveDeps {
            suppression: dispatcher.suppression_handle(),
            emit,
            cache: None,
            projection_enabled: false,
        }
    }

    /// SLW-RI-1 helper: a `MigrationCutover` wave with no affected repos + no cache —
    /// runs to completion almost instantly (mirrors the supersede tests).
    fn ri1_wave() -> Wave {
        let ctx = ResolverContext {
            context_root: None,
            codesense_home: PathBuf::from("/tmp/ri1_engine_runtime_test"),
            user_id: 1,
            org_slug: "acme/api".to_string(),
            team_name: Some("default".to_string()),
        };
        Wave::new(
            WaveReason::MigrationCutover {
                explicit_names: None,
            },
            ctx,
            vec![],
        )
    }

    /// Drive a future to completion on a dedicated current-thread runtime — the
    /// stand-in for the "command" side. Kept separate from the engine runtime so
    /// the injecting tests can stay SYNC `#[test]`s: the test owns the engine
    /// `Runtime` (the coordinator only holds its `Handle`), and dropping a
    /// `Runtime` inside a `#[tokio::test]` async context panics ("Cannot drop a
    /// runtime in a context where blocking is not allowed"). A sync test drops it
    /// safely at scope end.
    fn ri1_block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread driver runtime")
            .block_on(f)
    }

    /// AC-1: the engine runtime handle is INJECTED, not baked into `new()`.
    /// Before injection there is no engine handle (so `enqueue` falls back to
    /// Tauri's runtime); after `set_engine_handle` there is one. Mirrors the
    /// `set_live_filter` injection seam — keeps `new()`/`default()` runtime-free
    /// for the ~hundreds of tests that build a coordinator.
    #[test]
    fn engine_handle_absent_until_injected() {
        let _g = super::super::super::test_support::env_guard();
        let coord = WaveCoordinator::new();
        assert!(
            coord.engine_spawn_handle().is_none(),
            "new() must be runtime-free (fallback path)"
        );
        let rt = build_engine_runtime().expect("build engine runtime");
        coord.set_engine_handle(rt.handle().clone());
        assert!(
            coord.engine_spawn_handle().is_some(),
            "engine handle present after injection"
        );
    }

    /// AC-1: the engine runtime builds and actually runs work; the bounded pool
    /// is floored at 2 (a wave awaiting engine progress can't deadlock a
    /// single-worker pool).
    #[test]
    fn engine_runtime_builds_and_runs() {
        let _g = super::super::super::test_support::env_guard();
        let rt = build_engine_runtime().expect("build engine runtime");
        assert_eq!(rt.block_on(async { 21 * 2 }), 42);
    }

    /// AC-2 (the mechanism behind AC-3): with an engine runtime injected, work
    /// routed through the coordinator's engine handle executes on a `cs-engine`
    /// worker — i.e. OFF Tauri's command-dispatch workers. Engine work that
    /// isn't on the command workers cannot starve command dispatch.
    #[test]
    fn engine_work_runs_on_cs_engine_thread() {
        let _g = super::super::super::test_support::env_guard();
        let coord = WaveCoordinator::new();
        let rt = build_engine_runtime().expect("build engine runtime");
        coord.set_engine_handle(rt.handle().clone());
        let handle = coord.engine_spawn_handle().expect("engine injected");
        let thread_name = ri1_block_on(async move {
            handle
                .spawn(async { std::thread::current().name().map(String::from) })
                .await
                .expect("engine task joins")
        });
        assert_eq!(
            thread_name.as_deref().map(|n| n.starts_with("cs-engine")),
            Some(true),
            "engine work must run on a cs-engine worker, got {:?}",
            thread_name
        );
    }

    /// AC-4: a wave enqueued from a plain OS thread with NO ambient Tokio runtime
    /// (the notify watcher-thread condition — a bare `tokio::spawn` there would
    /// panic "no reactor") still runs to completion on the engine runtime. No
    /// panic, no silent loss.
    #[test]
    fn enqueue_from_non_async_thread_runs_on_engine() {
        let _g = super::super::super::test_support::env_guard();
        let coord = Arc::new(WaveCoordinator::new());
        let rt = build_engine_runtime().expect("build engine runtime");
        coord.set_engine_handle(rt.handle().clone());
        let deps = ri1_deps();
        let wave = ri1_wave();
        let coord2 = Arc::clone(&coord);
        // enqueue OFF any runtime — proves the engine `RuntimeHandle` works from
        // a non-async OS thread (it carries its own runtime context).
        let handle = std::thread::spawn(move || coord2.enqueue(wave, deps))
            .join()
            .expect("enqueue thread must not panic");
        ri1_block_on(async move {
            handle
                .await
                .expect("wave runs to completion on the engine runtime");
        });
    }

    /// AC-7e: with NO engine runtime injected (default / test / pre-setup),
    /// `enqueue` falls back to Tauri's runtime and still runs the wave — the
    /// injection is additive, never a regression. (The entire existing suite
    /// relies on this fallback.)
    #[tokio::test]
    async fn enqueue_without_engine_runtime_falls_back_and_runs() {
        let _g = super::super::super::test_support::env_guard();
        let coord = Arc::new(WaveCoordinator::new());
        assert!(coord.engine_spawn_handle().is_none());
        let handle = coord.enqueue(ri1_wave(), ri1_deps());
        handle
            .await
            .expect("wave runs on the fallback (Tauri) runtime");
    }

    /// AC-6 / AC-7d: `EngineRuntimeGuard`'s drop shuts the engine runtime down via
    /// `shutdown_background()`, which returns IMMEDIATELY even with a long
    /// in-flight `spawn_blocking` (it detaches it). So app exit is bounded and
    /// never hangs on a heavy materialize / stalled fsync — a *plain* runtime
    /// drop, by contrast, would block until the 500ms blocking task finished.
    /// The generous 100ms bound is still an order of magnitude under 500ms.
    #[test]
    fn engine_runtime_guard_shutdown_is_non_blocking() {
        let _g = super::super::super::test_support::env_guard();
        let rt = build_engine_runtime().expect("build engine runtime");
        // A blocking task far longer than our assert bound — a plain drop would
        // wait for it; the guard's `shutdown_background` must not. Signal once the
        // task is ACTUALLY running so the assertion can't pass spuriously (a
        // regression to a plain drop is only observable while the task is live).
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        rt.spawn_blocking(move || {
            let _ = started_tx.send(());
            std::thread::sleep(Duration::from_millis(500));
        });
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("blocking task must start before we drop the guard");
        let guard = EngineRuntimeGuard::new(rt);
        let start = Instant::now();
        drop(guard);
        assert!(
            start.elapsed() < Duration::from_millis(100),
            "guard shutdown must not block on in-flight spawn_blocking ({}ms)",
            start.elapsed().as_millis()
        );
    }
}

type RemoteDestinationLockKey = crate::content_engine::destination::RemoteDestinationIdentity;
