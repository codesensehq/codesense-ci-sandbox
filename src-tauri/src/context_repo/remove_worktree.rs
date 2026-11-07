// SPDX-License-Identifier: MIT
//! Tear down a SW-EW-6 integration worktree off a source clone.
//!
//! Removes both the git metadata at `.git/worktrees/{name}/` and the
//! on-disk working-tree directory. Refuses if the worktree has uncommitted
//! changes or is locked, recovering gracefully from orphaned metadata or
//! broken linkfiles.
//!
//! Refusal contract: no `force` flag — refusals are typed errors
//! (`UncommittedChanges`, `Locked`) and the caller decides recovery.
//!
//! Does NOT fetch, does NOT delete any branch ref, does NOT touch the
//! main branch beyond what libgit2's `Worktree::prune` touches
//! internally.

use std::fmt;
use std::path::{Path, PathBuf};

use super::branch_common::{self, with_clone_lock};
use super::worktree::integration_worktree_name;

// ---------- Types ----------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoveWorktreeOutcome {
    /// Fully present → fully removed: both the
    /// `.git/worktrees/{branch_name}/` metadata and the
    /// `{org_root}/worktrees/{branch_name}/` on-disk directory
    /// were deleted.
    Removed,

    /// The linked worktree's `.git` linkfile was corrupt (missing,
    /// a symlink, a directory, unreadable, or content that does
    /// not start with `gitdir: `). The dirty check was SKIPPED
    /// because the worktree can't be opened as a repo without a
    /// valid linkfile. Metadata + on-disk directory were removed
    /// anyway because a broken worktree is already unusable — the
    /// user can't commit or read content from it regardless. This
    /// is the expected outcome for worktree-repair flows that
    /// tear down the broken state and re-create it.
    RemovedBroken,

    /// The linked worktree's metadata existed but
    /// `Worktree::validate()` failed — typically because the
    /// working-tree directory was missing or the `.git` linkfile
    /// inside it was deleted. Both metadata and any remaining
    /// on-disk directory were removed. This is the recovery path
    /// after a user `rm -rf` or a crash-between-steps.
    OrphanCleaned,

    /// Nothing to do: `find_linked_worktree` returned `None` and
    /// the on-disk directory was not present. Idempotent re-call
    /// outcome.
    AlreadyRemoved,
}

#[derive(Debug)]
pub enum RemoveWorktreeError {
    /// `org_root` is a relative path.
    NotAbsolute { path: PathBuf },

    /// `org_root` exists as a directory but does not contain a
    /// `.git` entry. Only operates on existing scaffolded
    /// repos. Uses `.exists()` (not `.is_dir()`) so linked
    /// worktrees where `.git` is a file also pass.
    NotAScaffoldedRepo { path: PathBuf },

    /// The worktree has uncommitted local changes — modified
    /// tracked files, staged changes, or untracked files
    /// (ignored files do NOT trigger this). Caller must resolve
    /// the dirty state before retrying.
    UncommittedChanges {
        branch_name: String,
        worktree_path: PathBuf,
    },

    /// The worktree has a `.git/worktrees/{name}/locked` marker.
    /// Refuses to prune locked worktrees — the caller must
    /// explicitly unlock (out-of-band) before retrying. `reason`
    /// is the content of the locked file if libgit2 returned
    /// one.
    Locked {
        branch_name: String,
        worktree_path: PathBuf,
        reason: Option<String>,
    },

    /// A `git2` call failed with a human-readable step label.
    /// Step labels: `"open repo"`, `"find existing worktree"`,
    /// `"check worktree lock"`, `"validate worktree"`,
    /// `"open worktree as repo"`, `"read worktree status"`,
    /// `"prune orphan worktree"`, `"prune worktree"`,
    /// `"prune broken worktree"`,
    /// `"cleanup stale worktree directory"`.
    Git {
        step: &'static str,
        source: git2::Error,
    },
}

impl fmt::Display for RemoveWorktreeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAbsolute { path } => write!(
                f,
                "remove_integration_worktree requires an absolute path, got: {}",
                path.display()
            ),
            Self::NotAScaffoldedRepo { path } => {
                write!(f, "not a scaffolded context repo: {}", path.display())
            }
            Self::UncommittedChanges {
                branch_name,
                worktree_path,
            } => write!(
                f,
                "refusing to remove '{branch_name}': worktree at {} has uncommitted changes",
                worktree_path.display()
            ),
            Self::Locked {
                branch_name,
                worktree_path,
                reason: Some(r),
            } => write!(
                f,
                "refusing to remove '{branch_name}': worktree at {} is locked: {r}",
                worktree_path.display()
            ),
            Self::Locked {
                branch_name,
                worktree_path,
                reason: None,
            } => write!(
                f,
                "refusing to remove '{branch_name}': worktree at {} is locked",
                worktree_path.display()
            ),
            Self::Git { step, .. } => write!(f, "git operation failed during {step}"),
        }
    }
}

impl std::error::Error for RemoveWorktreeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Git { source, .. } => Some(source),
            _ => None,
        }
    }
}

// ---------- Internal helpers ----------

fn worktree_is_dirty(wt_repo: &git2::Repository) -> Result<bool, RemoveWorktreeError> {
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true)
        .include_ignored(false)
        .include_unmodified(false)
        .recurse_untracked_dirs(true);
    let statuses =
        wt_repo
            .statuses(Some(&mut opts))
            .map_err(|source| RemoveWorktreeError::Git {
                step: "read worktree status",
                source,
            })?;
    Ok(!statuses.is_empty())
}

/// Remove a SW-EW-6 integration worktree at a custom parent path, off a
/// specific source clone.
///
/// Refuses on uncommitted changes / locked, recovers orphans + broken
/// linkfiles, and keeps the branch. It:
/// - identifies the worktree by the name derived from `branch_name` via
///   [`integration_worktree_name`] (the single source of truth shared with
///   assembly), not the branch ref directly;
/// - targets the custom `worktree_path` (`{workspace_dir}/{owner}/{repo}/`)
///   for stale-dir cleanup;
/// - serializes on the per-clone lock keyed by `clone_path` (D1), so
///   teardown across distinct clones runs concurrently.
///
/// The `UncommittedChanges` refusal is exactly the "warn on uncommitted,
/// never auto-reset" contract (EW-6 AC-6): the caller surfaces it as a
/// warning and leaves the worktree (and its work) intact.
pub fn remove_integration_worktree(
    clone_path: &Path,
    worktree_path: &Path,
    branch_name: &str,
) -> Result<RemoveWorktreeOutcome, RemoveWorktreeError> {
    if !clone_path.is_absolute() {
        return Err(RemoveWorktreeError::NotAbsolute {
            path: clone_path.to_path_buf(),
        });
    }
    if !worktree_path.is_absolute() {
        return Err(RemoveWorktreeError::NotAbsolute {
            path: worktree_path.to_path_buf(),
        });
    }

    let worktree_name = integration_worktree_name(branch_name);
    with_clone_lock(clone_path, || {
        remove_worktree_locked(clone_path, &worktree_name, worktree_path)
    })
}

/// The lock-free core of [`remove_integration_worktree`]. Callers MUST
/// hold the per-clone lock for `repo_root`.
///
/// `repo_root` is the clone the worktree belongs to; `worktree_name` is the
/// libgit2 worktree name; `expected_path` is the on-disk worktree directory
/// (used only for stale-dir cleanup on the AlreadyRemoved path). Never
/// deletes any branch ref.
fn remove_worktree_locked(
    repo_root: &Path,
    worktree_name: &str,
    expected_path: &Path,
) -> Result<RemoveWorktreeOutcome, RemoveWorktreeError> {
    // AC-13: scaffolded-repo precondition. Uses .exists() (not .is_dir())
    // so linked worktrees where .git is a file also pass.
    if !repo_root.is_dir() || !repo_root.join(".git").exists() {
        return Err(RemoveWorktreeError::NotAScaffoldedRepo {
            path: repo_root.to_path_buf(),
        });
    }

    // AC-14: open the main repo.
    let repo = git2::Repository::open(repo_root).map_err(|source| RemoveWorktreeError::Git {
        step: "open repo",
        source,
    })?;

    // AC-15: look up the existing worktree; handle the AlreadyRemoved path.
    let Some(worktree) =
        branch_common::find_linked_worktree(&repo, worktree_name).map_err(|source| {
            RemoveWorktreeError::Git {
                step: "find existing worktree",
                source,
            }
        })?
    else {
        // Metadata is gone. Check if the on-disk directory still exists
        // (crash between metadata-delete and dir-delete in a prior prune).
        // If so, clean it up so the next `add_*_worktree` call doesn't hit
        // WorktreeDirOccupied.
        if expected_path.exists() {
            std::fs::remove_dir_all(expected_path).map_err(|source| RemoveWorktreeError::Git {
                step: "cleanup stale worktree directory",
                source: git2::Error::from_str(&format!(
                    "failed to remove stale directory {}: {}",
                    expected_path.display(),
                    source,
                )),
            })?;
        }
        return Ok(RemoveWorktreeOutcome::AlreadyRemoved);
    };
    let worktree_path = worktree.path().to_path_buf();

    // AC-16: refuse locked worktrees. Runs BEFORE validate so a
    // locked-but-orphaned worktree still surfaces as Locked.
    match worktree
        .is_locked()
        .map_err(|source| RemoveWorktreeError::Git {
            step: "check worktree lock",
            source,
        })? {
        git2::WorktreeLockStatus::Locked(reason) => {
            return Err(RemoveWorktreeError::Locked {
                branch_name: worktree_name.to_string(),
                worktree_path,
                reason,
            });
        }
        git2::WorktreeLockStatus::Unlocked => {}
    }

    // AC-17: validate() discriminates orphan vs healthy.
    if worktree.validate().is_err() {
        let mut opts = git2::WorktreePruneOptions::new();
        opts.valid(false).locked(false).working_tree(true);
        worktree
            .prune(Some(&mut opts))
            .map_err(|source| RemoveWorktreeError::Git {
                step: "prune orphan worktree",
                source,
            })?;
        return Ok(RemoveWorktreeOutcome::OrphanCleaned);
    }

    // AC-18: .git linkfile integrity check → broken path or healthy path.
    if !branch_common::is_valid_git_linkfile(&worktree_path.join(".git")) {
        // Broken linkfile: can't open the worktree as a repo, so the
        // dirty check is impossible. Prune anyway — the worktree is
        // already unusable and refusing here would block C.11's repair flow.
        let mut opts = git2::WorktreePruneOptions::new();
        opts.valid(true).locked(false).working_tree(true);
        worktree
            .prune(Some(&mut opts))
            .map_err(|source| RemoveWorktreeError::Git {
                step: "prune broken worktree",
                source,
            })?;
        return Ok(RemoveWorktreeOutcome::RemovedBroken);
    }
    // Linkfile is valid — proceed to dirty check.

    // AC-19: open the worktree as a repo and run the dirty check.
    let wt_repo = git2::Repository::open_from_worktree(&worktree).map_err(|source| {
        RemoveWorktreeError::Git {
            step: "open worktree as repo",
            source,
        }
    })?;
    if worktree_is_dirty(&wt_repo)? {
        return Err(RemoveWorktreeError::UncommittedChanges {
            branch_name: worktree_name.to_string(),
            worktree_path,
        });
    }

    // AC-20: prune with full-removal options.
    // valid(true)        — we already validated; prune even though valid
    // locked(false)      — defense-in-depth: libgit2 re-checks the lock marker
    // working_tree(true) — delete the on-disk dir as part of the prune
    let mut opts = git2::WorktreePruneOptions::new();
    opts.valid(true).locked(false).working_tree(true);
    worktree
        .prune(Some(&mut opts))
        .map_err(|source| RemoveWorktreeError::Git {
            step: "prune worktree",
            source,
        })?;

    // AC-21: return Removed.
    Ok(RemoveWorktreeOutcome::Removed)
}

// ---------- Tests ----------

#[cfg(test)]
mod integration_worktree_removal_tests {
    use super::{remove_integration_worktree, RemoveWorktreeError, RemoveWorktreeOutcome};
    use crate::context_repo::add_integration_worktree;
    use crate::context_repo::test_fixtures::init_source_clone;
    use crate::context_repo::worktree::integration_worktree_name;
    use std::fs;
    use std::path::Path;

    const BRANCH: &str = "integration/SW-EW";

    #[test]
    fn removes_healthy_integration_worktree_and_keeps_branch() {
        let clone = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        init_source_clone(clone.path(), "main");
        let wt = ws.path().join("o").join("r");
        add_integration_worktree(clone.path(), &wt, BRANCH).unwrap();
        assert!(wt.exists());

        let outcome = remove_integration_worktree(clone.path(), &wt, BRANCH).unwrap();
        assert_eq!(outcome, RemoveWorktreeOutcome::Removed);
        assert!(!wt.exists(), "worktree dir should be gone");
        // Branch must survive teardown (AC-6 keep-the-branch).
        let repo = git2::Repository::open(clone.path()).unwrap();
        assert!(repo.find_reference(&format!("refs/heads/{BRANCH}")).is_ok());
    }

    #[test]
    fn refuses_dirty_integration_worktree_preserving_work() {
        let clone = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        init_source_clone(clone.path(), "main");
        let wt = ws.path().join("o").join("r");
        add_integration_worktree(clone.path(), &wt, BRANCH).unwrap();
        std::fs::write(wt.join("uncommitted.md"), b"wip").unwrap();

        // AC-6: warn-on-uncommitted, never auto-reset → surfaces as a
        // refusal the caller treats as "skip + warn"; work is preserved.
        let err = remove_integration_worktree(clone.path(), &wt, BRANCH).unwrap_err();
        assert!(matches!(
            err,
            RemoveWorktreeError::UncommittedChanges { .. }
        ));
        assert!(wt.exists(), "dirty worktree must be preserved");
    }

    #[test]
    fn idempotent_removal() {
        let clone = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        init_source_clone(clone.path(), "main");
        let wt = ws.path().join("o").join("r");
        add_integration_worktree(clone.path(), &wt, BRANCH).unwrap();

        assert_eq!(
            remove_integration_worktree(clone.path(), &wt, BRANCH).unwrap(),
            RemoveWorktreeOutcome::Removed
        );
        assert_eq!(
            remove_integration_worktree(clone.path(), &wt, BRANCH).unwrap(),
            RemoveWorktreeOutcome::AlreadyRemoved
        );
    }

    #[test]
    fn rejects_relative_paths() {
        let err = remove_integration_worktree(Path::new("rel/clone"), Path::new("/abs/wt"), BRANCH)
            .unwrap_err();
        assert!(matches!(err, RemoveWorktreeError::NotAbsolute { .. }));
    }

    /// Assemble a healthy integration worktree; returns (clone, workspace,
    /// worktree path, libgit2 metadata dir). Tempdirs must outlive the test.
    fn fixture_worktree() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        std::path::PathBuf,
        std::path::PathBuf,
    ) {
        let clone = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        init_source_clone(clone.path(), "main");
        let wt = ws.path().join("o").join("r");
        add_integration_worktree(clone.path(), &wt, BRANCH).unwrap();
        let metadata = clone
            .path()
            .join(".git")
            .join("worktrees")
            .join(integration_worktree_name(BRANCH));
        assert!(metadata.exists(), "fixture metadata dir present");
        (clone, ws, wt, metadata)
    }

    #[test]
    fn cleans_orphaned_metadata_when_worktree_dir_was_deleted() {
        let (clone, _ws, wt, metadata) = fixture_worktree();
        fs::remove_dir_all(&wt).unwrap();

        let outcome = remove_integration_worktree(clone.path(), &wt, BRANCH).unwrap();
        assert_eq!(outcome, RemoveWorktreeOutcome::OrphanCleaned);
        assert!(!metadata.exists(), "orphaned metadata removed");
        assert_eq!(
            remove_integration_worktree(clone.path(), &wt, BRANCH).unwrap(),
            RemoveWorktreeOutcome::AlreadyRemoved
        );
    }

    #[test]
    fn refuses_modified_tracked_file() {
        let (clone, _ws, wt, metadata) = fixture_worktree();
        fs::write(wt.join("README.md"), "modified content").unwrap();

        let err = remove_integration_worktree(clone.path(), &wt, BRANCH).unwrap_err();
        assert!(
            matches!(err, RemoveWorktreeError::UncommittedChanges { .. }),
            "expected UncommittedChanges, got: {err}"
        );
        assert!(
            wt.exists() && metadata.exists(),
            "refusal is side-effect-free"
        );
    }

    #[test]
    fn refuses_locked_worktree() {
        let (clone, _ws, wt, metadata) = fixture_worktree();
        let repo = git2::Repository::open(clone.path()).unwrap();
        repo.find_worktree(&integration_worktree_name(BRANCH))
            .unwrap()
            .lock(Some("testing lock"))
            .unwrap();

        let err = remove_integration_worktree(clone.path(), &wt, BRANCH).unwrap_err();
        assert!(
            matches!(err, RemoveWorktreeError::Locked { reason: Some(ref r), .. } if r == "testing lock"),
            "expected Locked with reason, got: {err}"
        );
        assert!(wt.exists(), "locked worktree dir preserved");
        assert!(metadata.join("locked").exists(), "lock marker preserved");
    }

    // Lock check runs BEFORE validate: a locked + orphaned worktree surfaces
    // as Locked, not OrphanCleaned.
    #[test]
    fn refuses_locked_orphaned_worktree() {
        let (clone, _ws, wt, _metadata) = fixture_worktree();
        {
            let repo = git2::Repository::open(clone.path()).unwrap();
            repo.find_worktree(&integration_worktree_name(BRANCH))
                .unwrap()
                .lock(Some("do not touch"))
                .unwrap();
        }
        fs::remove_dir_all(&wt).unwrap();

        let err = remove_integration_worktree(clone.path(), &wt, BRANCH).unwrap_err();
        assert!(
            matches!(err, RemoveWorktreeError::Locked { reason: Some(ref r), .. } if r == "do not touch"),
            "expected Locked for locked+orphaned worktree, got: {err}"
        );
    }

    #[test]
    fn removes_worktree_with_corrupt_linkfile_content_and_keeps_branch() {
        let (clone, _ws, wt, metadata) = fixture_worktree();
        fs::write(wt.join(".git"), b"not a linkfile").unwrap();

        let outcome = remove_integration_worktree(clone.path(), &wt, BRANCH).unwrap();
        assert_eq!(outcome, RemoveWorktreeOutcome::RemovedBroken);
        assert!(
            !wt.exists() && !metadata.exists(),
            "broken worktree removed"
        );
        let repo = git2::Repository::open(clone.path()).unwrap();
        assert!(repo.find_reference(&format!("refs/heads/{BRANCH}")).is_ok());
    }

    // A symlinked `.git` must fail `is_valid_git_linkfile` (which checks the
    // entry itself). The decoy target keeps libgit2's validate() happy.
    #[cfg(unix)]
    #[test]
    fn removes_worktree_with_symlink_linkfile() {
        let (clone, ws, wt, metadata) = fixture_worktree();
        let linkfile = wt.join(".git");
        let decoy = ws.path().join("decoy-gitdir");
        fs::write(&decoy, fs::read(&linkfile).unwrap()).unwrap();
        fs::remove_file(&linkfile).unwrap();
        std::os::unix::fs::symlink(&decoy, &linkfile).unwrap();

        let outcome = remove_integration_worktree(clone.path(), &wt, BRANCH).unwrap();
        assert_eq!(outcome, RemoveWorktreeOutcome::RemovedBroken);
        assert!(!wt.exists() && !metadata.exists());
    }

    // Crash recovery: metadata gone but the on-disk dir survived.
    #[test]
    fn already_removed_cleans_stale_worktree_dir() {
        let (clone, _ws, wt, metadata) = fixture_worktree();
        fs::remove_dir_all(&metadata).unwrap();

        let outcome = remove_integration_worktree(clone.path(), &wt, BRANCH).unwrap();
        assert_eq!(outcome, RemoveWorktreeOutcome::AlreadyRemoved);
        assert!(!wt.exists(), "stale on-disk dir cleaned up");
    }

    #[test]
    fn rejects_clone_that_is_not_a_repository() {
        let clone = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let err =
            remove_integration_worktree(clone.path(), &ws.path().join("wt"), BRANCH).unwrap_err();
        assert!(
            matches!(err, RemoveWorktreeError::NotAScaffoldedRepo { .. }),
            "expected NotAScaffoldedRepo, got: {err}"
        );
    }

    #[test]
    fn display_messages_include_context_for_every_variant() {
        let path = std::path::PathBuf::from("/some/path");

        let msg = RemoveWorktreeError::NotAbsolute { path: path.clone() }.to_string();
        assert!(msg.contains("/some/path"), "NotAbsolute: {msg}");

        let msg = RemoveWorktreeError::NotAScaffoldedRepo { path: path.clone() }.to_string();
        assert!(msg.contains("/some/path"), "NotAScaffoldedRepo: {msg}");

        let msg = RemoveWorktreeError::UncommittedChanges {
            branch_name: "test-alpha".to_string(),
            worktree_path: path.clone(),
        }
        .to_string();
        assert!(
            msg.contains("test-alpha"),
            "UncommittedChanges branch: {msg}"
        );
        assert!(msg.contains("/some/path"), "UncommittedChanges path: {msg}");

        let msg = RemoveWorktreeError::Locked {
            branch_name: "test-alpha".to_string(),
            worktree_path: path.clone(),
            reason: Some("hold on".to_string()),
        }
        .to_string();
        assert!(msg.contains("test-alpha"), "Locked(Some) branch: {msg}");
        assert!(msg.contains("hold on"), "Locked(Some) reason: {msg}");

        let msg = RemoveWorktreeError::Locked {
            branch_name: "test-alpha".to_string(),
            worktree_path: path.clone(),
            reason: None,
        }
        .to_string();
        assert!(msg.contains("test-alpha"), "Locked(None) branch: {msg}");

        let git_err = git2::Error::from_str("boom");
        let msg = RemoveWorktreeError::Git {
            step: "prune worktree",
            source: git_err,
        }
        .to_string();
        assert!(msg.contains("prune worktree"), "Git step: {msg}");
    }
}
