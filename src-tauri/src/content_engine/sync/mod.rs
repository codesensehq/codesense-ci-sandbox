// SPDX-License-Identifier: MIT
//! Sync engine — the unified forward/reverse materialization model.
//!
//! One coherent design after the Seamless-Worktrees refactor (the `SS.4b` /
//! `SS.4c` milestone framing this module grew up under is retired):
//!
//! 1. **No projection engine (SW-XS-8).** Content reaches a workspace only
//!    through worktree creation, agent preparation (Step 2) and the explicit
//!    Step 3 calls, all on the preparation engine (`agents::prepare`). What
//!    remains here is the legacy store's teardown and the wave coordinator's
//!    locks.
//! 2. **No automatic reverse flow (SW-XS-4).** Worktree edits reach team content
//!    only through an explicit operation — `reverse_sync_worktree` (an agent's
//!    MCP call) or the human's Publish. The reverse watcher, the dirty-set drain,
//!    the entry / new-file sweeps, the focus / OSC-133 / open claims, the team main-clone watch and the background conflict registry are
//!    all gone, and so are the reverse primitives: SW-XS-8's explicit publish
//!    (`agents::prepare::publish`) is the only path from a workspace to team
//!    content.
//! 3. **No directory ownership (SW-XS-5).** Per-dir context-vs-install is
//!    manifest-derived (SLW-UD-5 replaced the old positional `_bmad-output`
//!    rule); the team-level install tier was folded into two-way content and
//!    retired (SLW-UD-8). Nothing marks a directory as owned any more.
//! 4. **Uniform team chain.** There is no Personal/Org tier — SLW-IN-4
//!    collapsed the old fixed 6-tier ladder into a uniform team chain and
//!    SW-LT-7 removed the reserved org-root team. SW-XS-8 deleted the install
//!    tiers and their resolver; team content is the one layer.
//!
//! Module shape:
//!
//! - [`materialize`] — what teardown still needs of the old per-destination
//!   primitive: the hash-guarded `verify_and_remove_dest`.
//! - [`wave`] — wave orchestration: `Wave`, `WaveCoordinator`, `WaveReason`,
//!   `WaveDeps`, `WaveResult`, and the supersede policy that drives cross-repo
//!   serialization + cancellation.
//! - [`teardown`] — the whole-repo detach/uninstall inverse: hash-guarded
//!   unmaterialize-all + exclude-strip + lockfile delete, composed from
//!   `materialize`'s shared `verify_and_remove_dest`.
//!
//! Re-exports preserve the historical import surface so existing call sites and
//! tests compile unchanged.
//!
//! ## Intentionally retained (SLW-LC-3 sweep — not missed cleanup)
//!
//! The legacy-decommission sweep removed only genuinely-dead code. These old
//! shapes are *demoted or reused*, not dead — mirrors the legacy-cleanup
//! README "Removed vs retained" table:
//!
//! - **Byte-copy write** (`atomic_io::write_tmp_fsync_rename_suppressed`) — the
//!   full-copy fallback when CoW `clone_or_copy` can't block-share (off-volume /
//!   unsupported FS). Live. (SLW-LC-3 removed the callerless no-fsync *byte*
//!   writer; the surviving no-fsync primitive is the CoW writer
//!   `clone_tmp_rename_suppressed_no_fsync`.)
//! - **`PRIORITY_ORDER`** (`content_engine::layers`) — now the one
//!   `TeamProjection` layer (SW-XS-8 deleted the install tiers).
//! - **`INSTALL_DIR`** (`context_repo`) — SLW-UD-8 retired the install *tier*,
//!   but the constant still names per-repo install dirs.
//! - **The SS-7 / SW-CP-2 / SW-CP-5 deletion-provenance comments** in
//!   `install::story_linker` — navigational tombstones for already-removed
//!   functions; kept so a future reader isn't left asking "why did this vanish?"
//!
pub mod materialize;
// SW-SY-8 (AC-3): residual CPU forensic counters — extends the SW-SY-7
// `sync::timing` spine (same gate) to attribute the architecture.md-deferred
// residuals (walk_subtree, context-file-change emits, reverse backstop/drain/
// new-file ticks, OSC-133 re-arm). Inert-when-off; see `residual_metrics`.
pub mod residual_metrics;
pub mod teardown;
pub mod wave;

/// Unix-epoch **milliseconds** (saturating to 0 before the epoch) — the
/// worktree-index sync stamps' granularity.
pub fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub use teardown::{reconcile_reattach, teardown_repo, teardown_repo_blocking, TeardownReport};
pub use wave::{
    build_engine_runtime, ctx_with_team, DetachFn, EngineRuntimeGuard, InFlightWave,
    RepoWaveCounts, Wave, WaveCoordinator, WaveDeps, WaveError, WaveReason, WaveResult,
    WaveTrigger,
};
