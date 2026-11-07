// SPDX-License-Identifier: MIT
//! Shared test fixtures for context_repo tests.

use std::path::Path;

/// Stage every file in `repo_root`'s working tree and commit it on HEAD.
///
/// Scaffolding leaves an unborn `main` with uncommitted seed files —
/// committing is the user's (or their agent's) job, not CodeSense's. Tests
/// that need a committed context repo call this to stand in for that
/// user commit.
pub fn commit_all(repo_root: &Path) {
    let repo = git2::Repository::open(repo_root).unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("t", "t@t").unwrap();
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, "user commit", &tree, &parents)
        .unwrap();
}

/// Initialize a plain *source* clone (NOT a context repo): a git repo with
/// one commit on `default_branch` and a tracked `README.md`, so integration
/// worktrees assembled off it have real content to check out. Shared by the
/// SW-EW-6 assembly/teardown tests across `worktree`, `remove_worktree`, and
/// `commands::content_engine`.
pub fn init_source_clone(dir: &Path, default_branch: &str) {
    let repo = git2::Repository::init(dir).unwrap();
    repo.set_head(&format!("refs/heads/{default_branch}"))
        .unwrap();
    std::fs::write(dir.join("README.md"), b"hi").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("README.md")).unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let sig = git2::Signature::now("t", "t@t").unwrap();
    repo.commit(
        Some(&format!("refs/heads/{default_branch}")),
        &sig,
        &sig,
        "init",
        &tree,
        &[],
    )
    .unwrap();
}
