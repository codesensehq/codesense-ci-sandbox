// SPDX-License-Identifier: MIT
//! Foundation primitives for the Seamless Sync engine.
//!
//! `content_engine` owns the substrate that downstream stories build on:
//! debounced filesystem watching, atomic copy + hash, per-directory event
//! dispatch with suppression, and `.git/info/exclude` block management.
//! Built additively in story SS.1 — every existing watcher / install /
//! gitignore code path remains in place until its dedicated cutover story
//! (SS.4a for the watcher, SS.5 for git-exclude, SS.11 for the final
//! sweep).
//!
//! [`file_mode`] reconstitutes path-static destination mode bits (the
//! executable bit for `.claude/hooks/codesense-*` scripts) at materialize
//! time, layer-agnostically.
//!
//! Consumers (forward references):
//! - SS.4a: replaces `commands/watch.rs` watcher + `HealGuard`
//! - SS.4b: hash-skip materialization via [`atomic_io`] + [`Dispatcher`]
//! - SS.5:  migrates `.gitignore` block consumers to [`git_exclude`]
//! - Future story-dir epic: reuses every primitive in this module
//!
//! Design invariant: this module has **zero dependencies on its eventual
//! consumers**. The only outward edge is [`From<Error> for AppError`] in
//! [`error`] so commands can map module failures at the Tauri boundary.
//!
//! ## Mechanism vs. policy seam (SW-CP-33)
//!
//! After SW-CP-31/32 converged hydration onto one engine, the surviving sync
//! machinery splits into two halves across a documented internal seam, so a future
//! *second* consumer of sync / reverse-sync becomes a lift, not a rewrite:
//!
//! - **Mechanism (the reuse candidate — this module):** the sync-state store
//!   ([`sync_state_store`], keyed on the `install_target` schema — a legacy name kept to
//!   avoid a DB migration), the materialize primitive
//!   ([`sync::materialize`]: the hash-guarded `verify_and_remove_dest` /
//!   [`atomic_io`] + fsync barrier), the wave coordinator (locks / supersede / dedup in
//!   [`sync::wave`]), and the suppression / echo gate ([`dispatcher::SuppressionHandle`]).
//!   None of it knows *what* codesense projects or *why*.
//! - **Policy (codesense-specific — the `crate::install` module):** the projection
//!   mapping (`ProjectionConfig` / `story_linker`), reverse inversion
//!   (`resolve_reverse_target`), and the [`sync::wave::WaveReason`] set — *what* to
//!   materialize and *which direction*. The mechanism above merely executes it.
//!
//! The seam currently spans two top-level modules whose *names* still carry a retired
//! concept (this mechanism module persists to the `install_target` schema; the policy
//! lives in a module named `install`). That is a documented known-item, not renamed
//! here.
//!
//! **Non-goal (SW-CP-33):** do NOT extract a generic reusable sync framework yet. There
//! is exactly one consumer today (team context → worktrees); a speculative abstraction
//! would reintroduce the same two-engines drift SW-CP-31/32 just removed. The seam is
//! documented so the genericity can be lifted out when a real second consumer appears —
//! not before.

pub mod atomic_io;
pub mod canvas_libraries;
pub(crate) mod context_lifecycle;
pub mod destination;
pub mod dispatcher;
mod error;
pub mod file_mode;
pub mod git_exclude;
pub mod layers;
pub mod lockfile;
pub mod mtime_cache;
pub mod sync;
pub mod sync_state_store;
pub mod watcher;

pub use atomic_io::{
    clone_tmp_rename_suppressed_no_fsync, file_mtime_and_size, file_mtime_ns, fsync_barrier,
    fsync_path, hash_file, mtime_gate, remove, remove_atomic_suppressed, remove_suppressed, rename,
    rename_suppressed, write_atomic_suppressed, write_tmp_fsync_rename,
    write_tmp_fsync_rename_keep_mode, write_tmp_fsync_rename_suppressed, write_tmp_rename_no_fsync,
    BarrierOutcome,
};
pub use canvas_libraries::{
    bag_delta, read_library_at, read_overlay, read_team_library, seed_library_if_absent,
    team_libraries_dir, write_team_library_at, TEAM_LIBRARY_FILENAME,
};
pub use dispatcher::{DispatchHandler, Dispatcher, SuppressionHandle};
pub use error::Error;
pub use git_exclude::{
    add_entries, read_managed_entries, remove_entries, resolve_info_exclude_path,
};
pub use layers::{
    content_layer_root, ctx_with_team, is_installed_path, is_projection_excluded,
    is_safe_logical_name, list_layer, validate_exclude_glob, validate_org_slug,
    validate_path_segment, LayerId, ResolverContext, TeamExcludes, PRIORITY_ORDER,
};
pub use lockfile::{
    enumerate_attached_code_repos_for_slug, enumerate_attached_code_repos_for_team, lockfile_path,
    register_attached_code_repo, AttachedCodeRepo, Lockfile, MaterializedEntry, SCHEMA_VERSION,
};
pub use sync::{
    teardown_repo, teardown_repo_blocking, RepoWaveCounts, TeardownReport, Wave, WaveCoordinator,
    WaveDeps, WaveError, WaveReason, WaveResult,
};
pub use watcher::{Suppression, WatchDir, WatchOutcome, WatchSetup, Watcher};

/// Process-wide test-coordination primitives. Lives here so every
/// submodule's `#[cfg(test)]` block can share a single lock when mutating
/// process-wide state (e.g. environment variables) — one lock, one source
/// of truth, rather than each module owning its own `static ENV_LOCK`.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Acquire the process-wide env-mutation lock. Poison-tolerant: a
    /// prior panicking holder leaves the mutex poisoned but the env
    /// state is still safe to inspect under a fresh guard.
    pub(crate) fn env_guard() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// SW-XS-12: how long a real-watcher test waits for an outcome. Generous,
    /// because the kernel stream (FSEvents) attaches late and delivers late on a
    /// loaded machine; the helpers below return as soon as the outcome holds, so
    /// a passing run never waits this long.
    pub(crate) const WATCH_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

    /// SW-XS-12: poll `done` every 25 ms until it holds or `deadline` passes.
    /// Returns whether it held. Use instead of a fixed `sleep` + assert.
    pub(crate) fn eventually(
        deadline: std::time::Duration,
        mut done: impl FnMut() -> bool,
    ) -> bool {
        let start = std::time::Instant::now();
        loop {
            if done() {
                return true;
            }
            if start.elapsed() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    /// SW-XS-12: the stimulus side of a real-watcher test. Performs `act` (an
    /// idempotent filesystem write), then waits for `done`, performing `act`
    /// again every `retouch` until `done` holds or [`WATCH_DEADLINE`] passes.
    ///
    /// A watch registered moments ago may not be attached in the kernel yet, so
    /// a single write can be lost on a loaded machine; repeating the write once
    /// the stream is live makes the test depend on the outcome, not on timing.
    pub(crate) fn act_until(
        retouch: std::time::Duration,
        mut act: impl FnMut(),
        mut done: impl FnMut() -> bool,
    ) -> bool {
        let start = std::time::Instant::now();
        let mut last = std::time::Instant::now();
        act();
        loop {
            if done() {
                return true;
            }
            if start.elapsed() >= WATCH_DEADLINE {
                return false;
            }
            if last.elapsed() >= retouch {
                act();
                last = std::time::Instant::now();
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    /// Record `code_repo_path` as a codesense-managed clone of `principal`
    /// (an `attachments.yml` entry with `managed: true`), so the materialize
    /// guards admit it. A managed clone keeps automatic context; a registered
    /// story worktree does not (it uses explicit selected context), so fixtures
    /// that exercise automatic projection target a managed clone.
    pub(crate) fn register_managed_clone(
        codesense_home: &std::path::Path,
        principal: &crate::domain::Principal,
        code_repo_path: &std::path::Path,
    ) {
        use crate::context_repo::attachments_io::{read_attachments, write_attachments_atomic};
        let path = crate::context_repo::path_to_user_attachments_file(codesense_home, principal);
        std::fs::create_dir_all(path.parent().expect("attachments file has a parent"))
            .expect("create user home");
        let mut attachments = read_attachments(&path).unwrap_or_default();
        let repo_path = code_repo_path.to_string_lossy().to_string();
        attachments.attachments.retain(|a| a.repo_path != repo_path);
        attachments.attachments.push(crate::domain::Attachment {
            repo_path,
            remote_url: None,
            attached_at: None,
            tracking_prompt_dismissed_at: None,
            credential_ref: None,
            managed: true,
        });
        write_attachments_atomic(&path, &attachments).expect("register managed clone");
    }
}

pub(crate) mod git_worktree;
