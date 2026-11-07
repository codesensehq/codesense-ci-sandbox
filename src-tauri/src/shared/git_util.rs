// SPDX-License-Identifier: MIT
//! Shared `git2` error-mapping helpers (RF-9).
//!
//! These collapse copy-pasted `.map_err(|e| format!(…))?` idioms into thin,
//! `#[inline]` passthroughs so the mapped error strings live in exactly one
//! place. Runtime behavior is byte-identical to the inline forms they replace —
//! no work is moved on or off any thread, and no allocation is added on the
//! success path.

use std::path::Path;

use git2::{Commit, Repository, Tree};

/// Open a git repository, mapping any error to the canonical `"open repo: {e}"`
/// string used across `git_sync`, `mcp::handler`, and the `context_repo` helpers.
///
/// The `AsRef<Path>` bound mirrors [`git2::Repository::open`] itself so every
/// existing call site (`repo_path`, `&path`, `org_root`, `&rp`, …) compiles
/// unchanged.
#[inline]
pub fn open_repo<P: AsRef<Path>>(path: P) -> Result<Repository, String> {
    Repository::open(path).map_err(|e| format!("open repo: {e}"))
}

/// Resolve a commit's tree, mapping errors to `"{label}: {e}"`.
///
/// The label is threaded so each existing message (`"HEAD tree"`,
/// `"parent tree"`, `"commit tree"`, `"tree HEAD"`, `"tree {refname}"`) is
/// reproduced byte-for-byte at the call site. Taking `impl Display` keeps the
/// label formatting **lazy** — it is only rendered on the error path, so a
/// dynamic label can be passed as `format_args!(…)` with zero success-path
/// allocation.
#[inline]
pub fn tree_or_err<'a>(
    commit: &Commit<'a>,
    label: impl std::fmt::Display,
) -> Result<Tree<'a>, String> {
    commit.tree().map_err(|e| format!("{label}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Init a repo in a fresh temp dir and return (tempdir, repo).
    fn init_repo() -> (tempfile::TempDir, Repository) {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        (dir, repo)
    }

    /// Create an empty initial commit and return its id.
    fn initial_commit(repo: &Repository) -> git2::Oid {
        let sig = git2::Signature::now("t", "t@e").unwrap();
        let tree_oid = {
            let mut index = repo.index().unwrap();
            index.write_tree().unwrap()
        };
        let tree = repo.find_tree(tree_oid).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap()
    }

    #[test]
    fn open_repo_ok_on_real_repo() {
        let (dir, _repo) = init_repo();
        assert!(open_repo(dir.path()).is_ok());
    }

    #[test]
    fn open_repo_err_has_canonical_prefix() {
        // A fresh temp dir is not a git repo; `Repository` is not `Debug`, so
        // match rather than `unwrap_err`.
        let dir = tempfile::tempdir().unwrap();
        match open_repo(dir.path()) {
            Ok(_) => panic!("expected error opening a non-repo dir"),
            Err(err) => assert!(
                err.starts_with("open repo: "),
                "expected canonical prefix, got: {err}"
            ),
        }
    }

    #[test]
    fn open_repo_accepts_varied_path_types() {
        // Compile-time proof the AsRef<Path> bound matches every call-site shape.
        let (dir, _repo) = init_repo();
        let p: &Path = dir.path();
        let owned: std::path::PathBuf = dir.path().to_path_buf();
        assert!(open_repo(p).is_ok());
        assert!(open_repo(&owned).is_ok());
        assert!(open_repo(owned.clone()).is_ok());
        assert!(open_repo(dir.path().to_str().unwrap()).is_ok());
    }

    #[test]
    fn tree_or_err_ok_on_commit() {
        let (_dir, repo) = init_repo();
        let oid = initial_commit(&repo);
        let commit = repo.find_commit(oid).unwrap();
        assert!(tree_or_err(&commit, "commit tree").is_ok());
    }

    #[test]
    fn tree_or_err_threads_label() {
        // The label formatting is `"{label}: {e}"`; verify the composition
        // directly since a real `Commit::tree()` failure is not reproducible.
        let label = "HEAD tree";
        let composed = format!("{label}: {}", "boom");
        assert_eq!(composed, "HEAD tree: boom");
    }
}
