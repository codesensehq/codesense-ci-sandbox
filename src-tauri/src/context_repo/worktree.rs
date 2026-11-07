// SPDX-License-Identifier: MIT
//! Assemble writable integration worktrees off a source clone (SW-EW-6).
//!
//! [`add_integration_worktree`] adds a linked `git worktree` for an
//! integration branch into an epic workspace, creating the branch off the
//! clone's default branch when absent. Idempotent: re-calling for an
//! existing matching worktree returns `AlreadyExisted` without touching
//! disk or git state. Teardown lives in `remove_worktree`.

use std::fmt;
use std::path::{Path, PathBuf};

use super::branch_common::{self, is_not_found, with_clone_lock};
use super::path::{resolve_default_branch, DefaultBranchError};

// ---------- Types ----------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddWorktreeOutcome {
    /// A brand-new linked worktree was created at the requested path,
    /// checked out at `refs/heads/{branch_name}` and fully materialized.
    Created,

    /// A linked worktree with the expected name, path and HEAD branch
    /// already exists. No disk or git state was touched — this is the
    /// idempotent re-call path.
    AlreadyExisted,
}

#[derive(Debug)]
pub enum AddWorktreeError {
    /// A clone or worktree path is relative; both must be absolute.
    NotAbsolute { path: PathBuf },

    /// The clone path is not a git repository (no `.git` entry; a `.git`
    /// file is accepted so linked worktrees qualify).
    NotAScaffoldedRepo { path: PathBuf },

    /// The target directory already exists on disk and is NOT tracked as
    /// a linked worktree. Refuse rather than adding into (and possibly
    /// clobbering) a populated directory; the caller must remove it first.
    WorktreeDirOccupied { path: PathBuf },

    /// A linked worktree with the expected name exists but its on-disk
    /// path differs, its working-tree `.git` linkfile is missing/corrupt,
    /// or its HEAD points at a different branch. Refuse rather than
    /// overwrite — the existing worktree may hold uncommitted work.
    WorktreeDirConflict {
        name: String,
        expected_path: PathBuf,
        actual_path: PathBuf,
    },

    /// A `git2` call failed with a human-readable step label. Step
    /// labels: `"open repo"`, `"find branch ref"`,
    /// `"find existing worktree"`, `"open existing worktree as repo"`,
    /// `"read existing worktree HEAD"`, `"create linked worktree"`,
    /// `"validate new worktree"`, `"create parent directory"`.
    Git {
        step: &'static str,
        source: git2::Error,
    },

    /// SW-EW-6: the integration branch did not exist and no default
    /// branch could be resolved to create it from. Only produced by
    /// `add_integration_worktree`.
    DefaultBranch(DefaultBranchError),
}

impl fmt::Display for AddWorktreeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAbsolute { path } => write!(
                f,
                "add_integration_worktree requires an absolute path, got: {}",
                path.display()
            ),
            Self::NotAScaffoldedRepo { path } => {
                write!(f, "not a scaffolded context repo: {}", path.display())
            }
            Self::WorktreeDirOccupied { path } => write!(
                f,
                "refusing to add linked worktree: target directory already exists and is not a git worktree: {}",
                path.display()
            ),
            Self::WorktreeDirConflict {
                name,
                expected_path,
                actual_path,
            } => write!(
                f,
                "linked worktree '{name}' already exists at {} but was expected at {}",
                actual_path.display(),
                expected_path.display()
            ),
            Self::Git { step, .. } => write!(f, "git operation failed during {step}"),
            Self::DefaultBranch(source) => {
                write!(f, "could not resolve a default branch: {source}")
            }
        }
    }
}

impl std::error::Error for AddWorktreeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Git { source, .. } => Some(source),
            Self::DefaultBranch(source) => Some(source),
            Self::NotAbsolute { .. }
            | Self::NotAScaffoldedRepo { .. }
            | Self::WorktreeDirOccupied { .. }
            | Self::WorktreeDirConflict { .. } => None,
        }
    }
}

// ---------- Internal helpers ----------

fn worktree_points_at_branch(
    wt: &git2::Worktree,
    expected_branch: &str,
) -> Result<bool, AddWorktreeError> {
    let wt_repo =
        git2::Repository::open_from_worktree(wt).map_err(|source| AddWorktreeError::Git {
            step: "open existing worktree as repo",
            source,
        })?;
    // Unborn / detached HEAD = "this worktree is not pointing at the
    // expected branch", which is the same as a HEAD-mismatch from the
    // caller's perspective. Both paths funnel into WorktreeDirConflict
    // upstream.
    let head = match wt_repo.head() {
        Ok(head) => head,
        Err(err) if is_not_found(&err) => return Ok(false),
        Err(source) => {
            return Err(AddWorktreeError::Git {
                step: "read existing worktree HEAD",
                source,
            })
        }
    };
    let expected_ref = format!("refs/heads/{expected_branch}");
    Ok(head.name() == Ok(expected_ref.as_str()))
}

/// Verify a pre-existing linked worktree (found by name) is the idempotent
/// match for what the caller is about to add: same on-disk path, intact
/// `.git` linkfile, and HEAD on `branch_name`. `Ok(())` ⇒ idempotent
/// `AlreadyExisted`; `Err(WorktreeDirConflict)` ⇒ the name is taken by a
/// divergent worktree the caller must remove first. `name` labels the error.
///
/// Used by [`add_integration_worktree`]'s idempotency check.
///
/// Implementation notes:
/// - Canonicalize both paths before comparing — on macOS `/tmp/*`
///   canonicalizes to `/private/tmp/*` and libgit2 may return either form.
///   If EITHER canonicalize fails (e.g. the existing dir was deleted), treat
///   it as a conflict: the metadata is stale and the caller should prune it.
/// - The `.git` linkfile must be a regular file starting with `gitdir: `
///   (`is_valid_git_linkfile`), checked BEFORE opening the worktree as a repo
///   so a corrupt linkfile surfaces as a conflict, not a generic git error.
///   `Worktree::validate()` is intentionally NOT used — libgit2 never checks
///   the linkfile, only metadata/parent/dir existence.
fn verify_existing_worktree_matches(
    existing: &git2::Worktree,
    name: &str,
    expected_path: &Path,
    branch_name: &str,
) -> Result<(), AddWorktreeError> {
    let existing_path = existing.path().to_path_buf();
    let conflict = || AddWorktreeError::WorktreeDirConflict {
        name: name.to_string(),
        expected_path: expected_path.to_path_buf(),
        actual_path: existing_path.clone(),
    };

    let paths_match = match (existing_path.canonicalize(), expected_path.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if !paths_match {
        return Err(conflict());
    }
    if !branch_common::is_valid_git_linkfile(&existing_path.join(".git")) {
        return Err(conflict());
    }
    if !worktree_points_at_branch(existing, branch_name)? {
        return Err(conflict());
    }
    Ok(())
}

// ---------- Integration-worktree assembly (SW-EW-6) ----------

/// Outcome of [`add_integration_worktree`], carrying the resolved branch
/// and any non-fatal warnings (e.g. cross-volume fragility) alongside the
/// create/already-existed disposition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationWorktreeResult {
    /// Whether a new worktree was created or an idempotent re-call hit an
    /// existing one.
    pub outcome: AddWorktreeOutcome,
    /// The integration branch the worktree is checked out on.
    pub branch: String,
    /// `true` when this call created the integration branch off the
    /// resolved default (vs. the branch already existing).
    pub created_branch: bool,
    /// Non-fatal warnings to surface to the user (the worktree was still
    /// created). Currently: cross-volume fragility (D4).
    pub warnings: Vec<String>,
    /// The on-disk worktree path.
    pub path: PathBuf,
}

/// Derive the libgit2 worktree *name* (the identifier registered under
/// `.git/worktrees/<name>/`) for an integration branch. Integration branches
/// contain `/` (`integration/SW-EW`), which is illegal in a worktree name;
/// map any char outside `[A-Za-z0-9._-]` to `-`. Deterministic so assembly
/// and teardown agree on the name for the same branch (single source of
/// truth — `remove_integration_worktree` calls this too).
pub(crate) fn integration_worktree_name(branch_name: &str) -> String {
    branch_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// True when `a` and `b` resolve to different filesystem volumes. Used to
/// warn (not fail) on cross-volume integration worktrees (D4): git worktrees
/// reference the clone's gitdir by absolute path (no object hardlinking), so
/// they *work* cross-volume but break if the clone's volume is unmounted.
#[cfg(unix)]
fn on_different_volumes(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(ma), Ok(mb)) => ma.dev() != mb.dev(),
        // Can't stat one side → can't tell → don't cry wolf.
        _ => false,
    }
}

#[cfg(not(unix))]
fn on_different_volumes(_a: &Path, _b: &Path) -> bool {
    false
}

/// Add a *writable* integration worktree for one source repo into an epic
/// workspace, off that repo's single clone (SW-EW-6 — the one net-new
/// engine-adjacent piece).
///
/// Contract:
///
/// 1. **Custom parent** — the worktree lands at `worktree_path`
///    (`{workspace_dir}/{owner}/{repo}/`).
/// 2. **Branch bootstrap** — if `refs/heads/{branch_name}` is absent it is
///    created off [`resolve_default_branch`]'s result (the ONE place
///    codesense creates a branch — benign creation off default; idempotent
///    if a concurrent peer created it first).
/// 3. **Per-clone lock** — serializes on [`with_clone_lock`] keyed by
///    `clone_path`, so adds against
///    distinct source clones run concurrently (D1/AC-4).
///
/// Idempotent: an existing matching worktree returns `AlreadyExisted`.
/// Cross-volume placement is a non-fatal warning in the result (D4/AC-7).
pub fn add_integration_worktree(
    clone_path: &Path,
    worktree_path: &Path,
    branch_name: &str,
) -> Result<IntegrationWorktreeResult, AddWorktreeError> {
    // Absolute-path checks run BEFORE lock acquisition.
    if !clone_path.is_absolute() {
        return Err(AddWorktreeError::NotAbsolute {
            path: clone_path.to_path_buf(),
        });
    }
    if !worktree_path.is_absolute() {
        return Err(AddWorktreeError::NotAbsolute {
            path: worktree_path.to_path_buf(),
        });
    }

    // Per-clone serialization (D1): distinct clones parallelize, same-clone
    // ops serialize. Held for the whole ensure-branch → add → validate
    // pipeline.
    with_clone_lock(clone_path, || {
        // Scaffold precondition: the clone must be a real git repo.
        if !clone_path.is_dir() || !clone_path.join(".git").exists() {
            return Err(AddWorktreeError::NotAScaffoldedRepo {
                path: clone_path.to_path_buf(),
            });
        }

        let repo = git2::Repository::open(clone_path).map_err(|source| AddWorktreeError::Git {
            step: "open repo",
            source,
        })?;

        // Ensure the integration branch exists; create it off the resolved
        // default if absent.
        let branch_ref_name = format!("refs/heads/{branch_name}");
        let branch_existed = match repo.find_reference(&branch_ref_name) {
            Ok(_) => true,
            Err(err) if is_not_found(&err) => false,
            Err(source) => {
                return Err(AddWorktreeError::Git {
                    step: "find branch ref",
                    source,
                })
            }
        };

        let mut created_branch = false;
        if !branch_existed {
            let default =
                resolve_default_branch(clone_path).map_err(AddWorktreeError::DefaultBranch)?;
            let default_commit = repo
                .find_branch(&default, git2::BranchType::Local)
                .and_then(|b| b.into_reference().peel_to_commit())
                .map_err(|source| AddWorktreeError::Git {
                    step: "peel default branch to commit",
                    source,
                })?;
            match repo.branch(branch_name, &default_commit, false) {
                Ok(_) => created_branch = true,
                // A concurrent peer (another epic workspace off this clone)
                // created it between our check and now — benign, reuse it.
                Err(err) if err.code() == git2::ErrorCode::Exists => {}
                Err(source) => {
                    return Err(AddWorktreeError::Git {
                        step: "create integration branch",
                        source,
                    })
                }
            }
        }

        // `branch_ref` must outlive the `repo.worktree(...)` call because
        // `WorktreeAddOptions::reference()` borrows it.
        let branch_ref =
            repo.find_reference(&branch_ref_name)
                .map_err(|source| AddWorktreeError::Git {
                    step: "find branch ref",
                    source,
                })?;

        // libgit2 worktree name (≠ branch ref): sanitize the `/` out.
        let worktree_name = integration_worktree_name(branch_name);

        // Idempotency / conflict discrimination — see
        // `verify_existing_worktree_matches`.
        if let Some(existing) =
            branch_common::find_linked_worktree(&repo, &worktree_name).map_err(|source| {
                AddWorktreeError::Git {
                    step: "find existing worktree",
                    source,
                }
            })?
        {
            verify_existing_worktree_matches(
                &existing,
                &worktree_name,
                worktree_path,
                branch_name,
            )?;
            return Ok(IntegrationWorktreeResult {
                outcome: AddWorktreeOutcome::AlreadyExisted,
                branch: branch_name.to_string(),
                created_branch,
                warnings: Vec::new(),
                path: worktree_path.to_path_buf(),
            });
        }

        // Stale-dir guard: the target must not already be occupied when git
        // doesn't track it (symlink_metadata so dangling symlinks count).
        if worktree_path.symlink_metadata().is_ok() {
            return Err(AddWorktreeError::WorktreeDirOccupied {
                path: worktree_path.to_path_buf(),
            });
        }

        // libgit2 requires the parent of the worktree path to exist.
        let parent = worktree_path
            .parent()
            .expect("integration worktree path always has a parent");
        std::fs::create_dir_all(parent).map_err(|source| AddWorktreeError::Git {
            step: "create parent directory",
            source: git2::Error::from_str(&format!(
                "failed to create {}: {}",
                parent.display(),
                source,
            )),
        })?;

        // Cross-volume fragility warning (D4) — detect once the parent
        // exists, never fail.
        let mut warnings = Vec::new();
        if on_different_volumes(clone_path, parent) {
            warnings.push(format!(
                "integration worktree {} is on a different volume than its clone {} — \
                 it will break if the clone's volume is unmounted",
                worktree_path.display(),
                clone_path.display(),
            ));
        }

        let mut opts = git2::WorktreeAddOptions::new();
        opts.reference(Some(&branch_ref));
        opts.lock(false);

        let worktree = repo
            .worktree(&worktree_name, worktree_path, Some(&opts))
            .map_err(|source| AddWorktreeError::Git {
                step: "create linked worktree",
                source,
            })?;
        worktree
            .validate()
            .map_err(|source| AddWorktreeError::Git {
                step: "validate new worktree",
                source,
            })?;

        Ok(IntegrationWorktreeResult {
            outcome: AddWorktreeOutcome::Created,
            branch: branch_name.to_string(),
            created_branch,
            warnings,
            path: worktree_path.to_path_buf(),
        })
    })
}

// ---------- Tests ----------

#[cfg(test)]
mod integration_worktree_tests {
    use super::*;
    use crate::context_repo::test_fixtures::init_source_clone;

    fn head_branch(worktree_path: &Path) -> String {
        let repo = git2::Repository::open(worktree_path).unwrap();
        let head = repo.head().unwrap();
        let name = head.shorthand().unwrap().to_string();
        name
    }

    #[test]
    fn assembles_writable_worktree_and_creates_branch_off_default() {
        let clone = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        init_source_clone(clone.path(), "main");
        let wt_path = ws.path().join("alderco").join("codesense");

        let res = add_integration_worktree(clone.path(), &wt_path, "integration/SW-EW").unwrap();

        assert_eq!(res.outcome, AddWorktreeOutcome::Created);
        assert!(res.created_branch, "branch should have been auto-created");
        assert_eq!(res.branch, "integration/SW-EW");
        // Worktree materialized + writable (tracked file present on disk).
        assert!(wt_path.join("README.md").is_file());
        // Checked out on the integration branch, not main.
        assert_eq!(head_branch(&wt_path), "integration/SW-EW");
        // Branch now exists in the clone.
        let repo = git2::Repository::open(clone.path()).unwrap();
        assert!(repo.find_reference("refs/heads/integration/SW-EW").is_ok());
    }

    #[test]
    fn object_store_is_shared_not_cloned() {
        let clone = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        init_source_clone(clone.path(), "main");
        let wt_path = ws.path().join("o").join("r");

        add_integration_worktree(clone.path(), &wt_path, "integration/SW-EW").unwrap();

        // A shared worktree's `.git` is a *file* (`gitdir: ...`) pointing at
        // the clone's commondir — NOT a populated `.git` directory.
        let dot_git = wt_path.join(".git");
        assert!(dot_git.is_file(), ".git should be a linkfile, not a dir");
        let contents = std::fs::read_to_string(&dot_git).unwrap();
        assert!(contents.starts_with("gitdir: "));
    }

    #[test]
    fn uses_branch_verbatim_when_it_already_exists() {
        let clone = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        init_source_clone(clone.path(), "main");
        // Pre-create the integration branch.
        {
            let repo = git2::Repository::open(clone.path()).unwrap();
            let commit = repo.head().unwrap().peel_to_commit().unwrap();
            repo.branch("integration/custom", &commit, false).unwrap();
        }
        let wt_path = ws.path().join("o").join("r");

        let res = add_integration_worktree(clone.path(), &wt_path, "integration/custom").unwrap();
        assert_eq!(res.outcome, AddWorktreeOutcome::Created);
        assert!(!res.created_branch, "branch already existed");
    }

    #[test]
    fn idempotent_re_add_returns_already_existed() {
        let clone = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        init_source_clone(clone.path(), "main");
        let wt_path = ws.path().join("o").join("r");

        let first = add_integration_worktree(clone.path(), &wt_path, "integration/SW-EW").unwrap();
        assert_eq!(first.outcome, AddWorktreeOutcome::Created);
        let second = add_integration_worktree(clone.path(), &wt_path, "integration/SW-EW").unwrap();
        assert_eq!(second.outcome, AddWorktreeOutcome::AlreadyExisted);
    }

    #[test]
    fn worktree_name_is_sanitized() {
        let clone = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        init_source_clone(clone.path(), "main");
        let wt_path = ws.path().join("o").join("r");

        add_integration_worktree(clone.path(), &wt_path, "integration/SW-EW").unwrap();

        let repo = git2::Repository::open(clone.path()).unwrap();
        // The libgit2 worktree NAME has `/` replaced by `-`.
        assert!(repo.find_worktree("integration-SW-EW").is_ok());
        assert!(repo.find_worktree("integration/SW-EW").is_err());
    }

    #[test]
    fn errors_when_no_default_branch_to_create_from() {
        let clone = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        // Empty repo: no commit, no branch, and the integration branch is absent.
        git2::Repository::init(clone.path()).unwrap();
        let wt_path = ws.path().join("o").join("r");

        let err =
            add_integration_worktree(clone.path(), &wt_path, "integration/SW-EW").unwrap_err();
        assert!(matches!(err, AddWorktreeError::DefaultBranch(_)));
    }

    #[test]
    fn rejects_relative_paths() {
        let err = add_integration_worktree(
            Path::new("relative/clone"),
            Path::new("/abs/wt"),
            "integration/SW-EW",
        )
        .unwrap_err();
        assert!(matches!(err, AddWorktreeError::NotAbsolute { .. }));
    }

    #[test]
    fn refuses_when_target_dir_is_occupied_by_non_worktree() {
        let clone = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        init_source_clone(clone.path(), "main");
        let wt_path = ws.path().join("o").join("r");
        std::fs::create_dir_all(&wt_path).unwrap();
        std::fs::write(wt_path.join("user-file.txt"), b"keep me").unwrap();

        let err =
            add_integration_worktree(clone.path(), &wt_path, "integration/SW-EW").unwrap_err();
        assert!(
            matches!(err, AddWorktreeError::WorktreeDirOccupied { .. }),
            "expected WorktreeDirOccupied, got: {err}"
        );
        assert!(
            wt_path.join("user-file.txt").is_file(),
            "user file untouched"
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_when_target_is_dangling_symlink() {
        let clone = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        init_source_clone(clone.path(), "main");
        let wt_path = ws.path().join("r");
        std::os::unix::fs::symlink(ws.path().join("nowhere"), &wt_path).unwrap();

        let err =
            add_integration_worktree(clone.path(), &wt_path, "integration/SW-EW").unwrap_err();
        assert!(
            matches!(err, AddWorktreeError::WorktreeDirOccupied { .. }),
            "expected WorktreeDirOccupied, got: {err}"
        );
    }

    // A worktree whose working-tree `.git` linkfile was deleted is unopenable:
    // re-adding must report a conflict, never `AlreadyExisted`.
    #[test]
    fn reports_conflict_when_working_tree_git_linkfile_is_missing() {
        let clone = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        init_source_clone(clone.path(), "main");
        let wt_path = ws.path().join("o").join("r");
        add_integration_worktree(clone.path(), &wt_path, "integration/SW-EW").unwrap();
        std::fs::remove_file(wt_path.join(".git")).unwrap();

        let err =
            add_integration_worktree(clone.path(), &wt_path, "integration/SW-EW").unwrap_err();
        assert!(
            matches!(err, AddWorktreeError::WorktreeDirConflict { .. }),
            "expected WorktreeDirConflict, got: {err}"
        );
    }

    #[test]
    fn reports_conflict_when_existing_worktree_is_at_a_different_path() {
        let clone = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        init_source_clone(clone.path(), "main");
        add_integration_worktree(clone.path(), &ws.path().join("a"), "integration/SW-EW").unwrap();

        let err = add_integration_worktree(clone.path(), &ws.path().join("b"), "integration/SW-EW")
            .unwrap_err();
        assert!(
            matches!(err, AddWorktreeError::WorktreeDirConflict { .. }),
            "expected WorktreeDirConflict, got: {err}"
        );
    }

    // Display messages include offending context for every variant.
    #[test]
    fn display_messages_include_context_for_every_variant() {
        let not_absolute = AddWorktreeError::NotAbsolute {
            path: PathBuf::from("rel/path"),
        };
        assert!(format!("{not_absolute}").contains("rel/path"));

        let not_scaffolded = AddWorktreeError::NotAScaffoldedRepo {
            path: PathBuf::from("/tmp/empty"),
        };
        assert!(format!("{not_scaffolded}").contains("/tmp/empty"));

        let occupied = AddWorktreeError::WorktreeDirOccupied {
            path: PathBuf::from("/tmp/alpha/worktrees/team-backend"),
        };
        assert!(format!("{occupied}").contains("/tmp/alpha/worktrees/team-backend"));

        let conflict = AddWorktreeError::WorktreeDirConflict {
            name: "team-backend".to_string(),
            expected_path: PathBuf::from("/tmp/alpha/worktrees/team-backend"),
            actual_path: PathBuf::from("/tmp/beta/worktrees/team-backend"),
        };
        let conflict_msg = format!("{conflict}");
        assert!(conflict_msg.contains("team-backend"));
        assert!(conflict_msg.contains("/tmp/alpha/worktrees/team-backend"));
        assert!(conflict_msg.contains("/tmp/beta/worktrees/team-backend"));

        let git = AddWorktreeError::Git {
            step: "create linked worktree",
            source: git2::Error::from_str("boom"),
        };
        assert!(format!("{git}").contains("create linked worktree"));
    }
}
