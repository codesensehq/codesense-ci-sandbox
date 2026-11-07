// SPDX-License-Identifier: MIT
//! Cross-cutting helpers shared across `context_repo/` primitives.
//!
//! What lives here: commit-identity constants, two process-wide locks
//! (`BRANCH_LOCK` for local ref mutations, `REMOTE_LOCK` for network
//! IO), thin git2 utilities (`is_not_found`, `local_branch_exists`,
//! `remote_branch_exists`, `find_linked_worktree`,
//! `is_valid_git_linkfile`, `validate_team_name`) that multiple
//! primitives need and that return raw `git2::Error` / a minimal
//! `TeamNameError` so callers can map into their own typed error
//! variants at the call site.
//!
//! What does NOT live here: story-specific helpers stay in their own
//! modules. This module is reserved for items needed by two or more
//! primitives.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::domain::{DomainError, Team};

// ---------- Commit identity ----------

/// Author/committer name for every codesense-generated scaffold commit.
/// Single source of truth — B.2, B.3, and B.4 all import this constant.
pub(super) const COMMIT_AUTHOR_NAME: &str = "CodeSense";

/// Author/committer email for every codesense-generated scaffold
/// commit. Domain is codesense-owned; the `noreply@` local-part signals
/// "don't reply to this" without pretending at a bot identity we
/// haven't registered. See B.2 Design Notes for the identity
/// rationale.
pub(super) const COMMIT_AUTHOR_EMAIL: &str = "noreply@codesense.ai";

// ---------- Process-wide lock ----------

/// Process-wide mutex serializing branch-mutation helpers in
/// `context_repo/` (team rename, content writes, etc.).
///
/// Distinct from `scaffold::SCAFFOLD_LOCK` — a concurrent scaffold
/// on one org and a branch operation on a different org should run
/// in parallel, not serialize on each other.
pub(super) static BRANCH_LOCK: Mutex<()> = Mutex::new(());

/// Run `f` while holding `BRANCH_LOCK`. Exposed so out-of-module callers
/// (e.g. `install::migration_pts` for PTS-2) can serialize their entire
/// read → plan → mutate → commit sequence against the existing in-module
/// callers (`content.rs`, `team_rename.rs`, `workdir_write.rs`) without widening the lock's own visibility.
///
/// Poisoned-lock recovery matches every in-module call site: if a previous
/// holder panicked while holding the lock, we recover the inner data and
/// continue — `()` carries no invariants worth preserving.
pub fn with_branch_lock<R>(f: impl FnOnce() -> R) -> R {
    let _lock = BRANCH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    f()
}

/// Process-wide serialization for context_repo/ network-IO operations
/// (clone, push, fetch). Distinct from BRANCH_LOCK: REMOTE_LOCK
/// protects network operations, BRANCH_LOCK protects local ref mutations.
pub(super) static REMOTE_LOCK: Mutex<()> = Mutex::new(());

// ---------- Per-clone keyed lock (SW-EW-6 D1) ----------

/// Registry of per-clone locks, keyed by the canonicalized clone path.
///
/// `BRANCH_LOCK` above is process-wide: correct but over-broad for
/// operations that target *distinct* git clones. Epic
/// integration-worktree assembly (SW-EW-6), by contrast, adds a worktree
/// to each of N *distinct* source clones — those adds have no shared git
/// state and must run in parallel. Two adds against the *same* clone,
/// however, do race on libgit2's `.git/worktrees/` bookkeeping and ref
/// creation, so they must serialize.
///
/// `with_clone_lock` gives exactly that granularity: distinct clones run
/// concurrently, same-clone operations serialize. The registry mutex is
/// held only long enough to fetch-or-insert the per-clone `Arc<Mutex<()>>`;
/// the caller then holds the per-clone lock for the duration of `f`.
static CLONE_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();

/// Fetch-or-create the lock for `clone_path`, keyed by its canonicalized
/// path so symlinks / `.` / `..` aliases of one clone collapse to one
/// lock. Falls back to the path as-given when it cannot be canonicalized
/// (e.g. it does not exist yet) — distinct un-canonicalizable paths simply
/// get distinct locks, which is safe.
fn clone_lock_for(clone_path: &Path) -> Arc<Mutex<()>> {
    let key = std::fs::canonicalize(clone_path).unwrap_or_else(|_| clone_path.to_path_buf());
    let registry = CLONE_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = registry.lock().unwrap_or_else(|e| e.into_inner());
    map.entry(key)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Run `f` while holding the per-clone lock for `clone_path`.
///
/// Use this — NOT `with_branch_lock` — for git mutations scoped to a
/// specific clone (SW-EW-6 integration-worktree assembly + teardown), so
/// distinct clones parallelize. Poisoned-lock recovery matches the other
/// lock helpers: a panicked prior holder leaves `()` behind, which carries
/// no invariants worth preserving.
pub fn with_clone_lock<R>(clone_path: &Path, f: impl FnOnce() -> R) -> R {
    let lock = clone_lock_for(clone_path);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    f()
}

// ---------- git2 classification utility ----------

/// Errors from git2 that we treat as "this ref doesn't exist" (as
/// opposed to "something went wrong looking it up"). Used consistently
/// across every branch-existence check in `context_repo/` so the
/// `NotFound` / real-error split is uniform.
pub(crate) fn is_not_found(err: &git2::Error) -> bool {
    matches!(
        err.code(),
        git2::ErrorCode::NotFound | git2::ErrorCode::UnbornBranch
    )
}

// ---------- Branch-existence helpers ----------

/// Does `refs/heads/{branch_name}` exist in the local repo? Returns
/// raw `git2::Error` on non-NotFound failures — callers wrap into
/// their own typed error with an appropriate `step` label.
pub(crate) fn local_branch_exists(
    repo: &git2::Repository,
    branch_name: &str,
) -> Result<bool, git2::Error> {
    match repo.find_branch(branch_name, git2::BranchType::Local) {
        Ok(_) => Ok(true),
        Err(err) if is_not_found(&err) => Ok(false),
        Err(err) => Err(err),
    }
}

/// Does `refs/remotes/origin/{branch_name}` already exist? Returns
/// `true` even if the remote ref points at a commit we don't have
/// locally yet — the caller is responsible for `git fetch` +
/// tracking-branch setup before re-invoking the scaffold helper.
///
/// Called between the local-branch idempotency short-circuit and the
/// commit-creation path. If this returns `true`, the calling
/// scaffold helper refuses (with `TeamBranchError::RemoteBranchExists`
/// or `SharedBranchError::SharedBranchExistsOnRemote`) rather than
/// silently producing a local branch with divergent history.
pub(crate) fn remote_branch_exists(
    repo: &git2::Repository,
    branch_name: &str,
) -> Result<bool, git2::Error> {
    let ref_name = format!("refs/remotes/origin/{branch_name}");
    match repo.find_reference(&ref_name) {
        Ok(_) => Ok(true),
        Err(err) if is_not_found(&err) => Ok(false),
        Err(err) => Err(err),
    }
}

// ---------- Worktree helpers ----------

/// Look up a linked worktree by name, classifying a NotFound
/// result as `None`. Each caller wraps any non-NotFound
/// `git2::Error` in its own `Git { step, source }` variant.
pub(super) fn find_linked_worktree(
    repo: &git2::Repository,
    branch_name: &str,
) -> Result<Option<git2::Worktree>, git2::Error> {
    match repo.find_worktree(branch_name) {
        Ok(wt) => Ok(Some(wt)),
        Err(err) if is_not_found(&err) => Ok(None),
        Err(source) => Err(source),
    }
}

/// Validate a linked worktree's `.git` linkfile.
///
/// Returns `true` iff the `.git` entry is a regular file (NOT a
/// symlink or directory), is readable, and its content starts
/// with `b"gitdir: "` (git's documented linkfile format). Used
/// as a pre-check before `Repository::open_from_worktree` so
/// corrupt linkfiles surface as a specific typed error rather
/// than a generic libgit2 open failure.
pub(super) fn is_valid_git_linkfile(path: &std::path::Path) -> bool {
    use std::io::Read;
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !meta.file_type().is_file() {
        return false;
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut buf = [0u8; 256];
    let Ok(n) = file.by_ref().take(256).read(&mut buf) else {
        return false;
    };
    buf[..n].starts_with(b"gitdir: ")
}

// ---------- URL validation ----------

/// Minimal scheme check for git remote URLs. Returns `Ok(())` if the
/// URL starts with a supported scheme, `Err(reason)` otherwise. Each
/// caller wraps the reason into its own typed `InvalidUrl` variant.
pub(super) fn validate_git_url(url: &str) -> Result<(), &'static str> {
    if url.is_empty() {
        return Err("empty URL");
    }
    let ok = url.starts_with("https://")
        || url.starts_with("http://")
        || url.starts_with("ssh://")
        || url.starts_with("git@")
        || url.starts_with("file://");
    if !ok {
        return Err("unsupported scheme (expected https://, http://, ssh://, git@, or file://)");
    }
    Ok(())
}

// ---------- Team-name validation ----------

/// Error variants produced by `validate_team_name`. Minimal — callers
/// map these to their own story-specific error types (`TeamBranchError`
/// in B.3, `SharedBranchError` in B.4) at the call site.
#[derive(Debug)]
pub(super) enum TeamNameError {
    /// `Team::new` rejected the name (empty, invalid characters, etc.).
    InvalidSlug(DomainError),
    /// Name ends with the reserved `-meta` suffix. See B.3's
    /// `team_branch.rs` `ReservedTeamNameSuffix` variant doc for the
    /// content/metadata namespace collision rationale.
    ReservedMetaSuffix,
    /// SLW-IN-7 D-1 Class B: the name equals a reserved layer-marker directory
    /// (`install`/`content`/`repos`/`libraries`, [`super::path::LAYER_MARKER_DIRS`]),
    /// OR the `worktrees` root sibling. Post-flatten a team sits directly under
    /// `org_root`: a team named like a marker would make the flat path parsers
    /// ambiguous (`{org}/content/content/…`), and a team named `worktrees` would
    /// collide on-disk with the git worktrees container (`{org_root}/worktrees/`).
    /// All of these names are slug-safe, so `Team::new` alone does NOT reject
    /// them — this guard does.
    ReservedLayerMarker,
}

/// Validate a team name against `Team::new`'s slug-safe rules AND the
/// `-meta` suffix rejection that B.3 established. Returns the
/// validated `Team` so the caller can immediately use its
/// `content_branch_name()` / `meta_branch_name()` accessors.
///
/// The `-meta` rejection is symmetric across all callers — B.3 applies
/// it to both content and metadata branch creation, B.4 applies it to
/// both sides of the shared-branch team pair. Keeping the check here
/// means future callers automatically inherit the policy without
/// needing to remember it.
pub(super) fn validate_team_name(name: &str) -> Result<Team, TeamNameError> {
    let team = Team::new(name, vec![]).map_err(TeamNameError::InvalidSlug)?;
    if team.name.ends_with("-meta") {
        return Err(TeamNameError::ReservedMetaSuffix);
    }
    // SLW-IN-7 D-1 Class B: reject a team name equal to any layer-marker dir (so
    // the flat path delimiter stays un-collidable) or the `worktrees` root
    // sibling (so a flat team dir can't collide with the git worktrees
    // container). Referenced via the layout constants — the reservation can't
    // drift from the layout.
    if super::path::LAYER_MARKER_DIRS.contains(&team.name.as_str())
        || team.name == super::path::WORKTREES_DIR
    {
        return Err(TeamNameError::ReservedLayerMarker);
    }
    Ok(team)
}

#[cfg(test)]
mod clone_lock_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Barrier;
    use std::thread;
    use std::time::Duration;

    /// Two operations against the SAME clone path must serialize: the
    /// observed peak concurrency inside the critical section is exactly 1.
    #[test]
    fn same_clone_serializes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();

        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let handles: Vec<_> = (0..4)
            .map(|_| {
                let path = path.clone();
                let active = Arc::clone(&active);
                let peak = Arc::clone(&peak);
                thread::spawn(move || {
                    with_clone_lock(&path, || {
                        let cur = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(cur, Ordering::SeqCst);
                        thread::sleep(Duration::from_millis(20));
                        active.fetch_sub(1, Ordering::SeqCst);
                    });
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(peak.load(Ordering::SeqCst), 1, "same-clone ops overlapped");
    }

    /// Two operations against DISTINCT clone paths must run concurrently.
    /// Proven via a 2-party barrier inside the critical sections: if the
    /// locks were shared, the second thread could never reach the barrier
    /// (it would block on the lock the first thread holds while waiting),
    /// so both threads completing proves the sections overlapped.
    #[test]
    fn different_clones_overlap() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let path_a = dir_a.path().to_path_buf();
        let path_b = dir_b.path().to_path_buf();

        let barrier = Arc::new(Barrier::new(2));

        let b1 = Arc::clone(&barrier);
        let t1 = thread::spawn(move || {
            with_clone_lock(&path_a, || {
                b1.wait();
            });
        });
        let b2 = Arc::clone(&barrier);
        let t2 = thread::spawn(move || {
            with_clone_lock(&path_b, || {
                b2.wait();
            });
        });

        // Join from a watchdog thread so a regression (shared lock →
        // deadlock) fails the test instead of hanging CI forever.
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            t1.join().unwrap();
            t2.join().unwrap();
            let _ = tx.send(());
        });
        rx.recv_timeout(Duration::from_secs(5))
            .expect("distinct-clone locks did not overlap (deadlocked)");
    }

    /// Aliased spellings of one clone path (`dir` vs `dir/.`) canonicalize
    /// to the same key, so they share a lock and serialize.
    #[test]
    fn canonicalization_collapses_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let aliased = path.join(".");

        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let a1 = Arc::clone(&active);
        let p1 = Arc::clone(&peak);
        let path1 = path.clone();
        let t1 = thread::spawn(move || {
            with_clone_lock(&path1, || {
                let cur = a1.fetch_add(1, Ordering::SeqCst) + 1;
                p1.fetch_max(cur, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(20));
                a1.fetch_sub(1, Ordering::SeqCst);
            });
        });
        let a2 = Arc::clone(&active);
        let p2 = Arc::clone(&peak);
        let t2 = thread::spawn(move || {
            with_clone_lock(&aliased, || {
                let cur = a2.fetch_add(1, Ordering::SeqCst) + 1;
                p2.fetch_max(cur, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(20));
                a2.fetch_sub(1, Ordering::SeqCst);
            });
        });
        t1.join().unwrap();
        t2.join().unwrap();

        assert_eq!(
            peak.load(Ordering::SeqCst),
            1,
            "aliased paths did not share a lock"
        );
    }
}
