// SPDX-License-Identifier: MIT
//! SW-AU-96B: per-file footprint auto-capture from a run's work-product worktree.
//!
//! SW-AU-96 auto-captures a run's work product as a single branch `Entity` so a run's footprint
//! never depends on agent goodwill — but only at branch granularity. This closes that gap: at run
//! completion the harness diffs the run's worktree against its merge-base baseline and records one
//! `FootprintEntry { kind: File, .. }` per changed file, so the footprint lists the actual files the
//! run touched even when the agent reported nothing (AD-15, agent-independent, deterministic).
//!
//! The diff reuses the shipped source-worktree change-set spine
//! ([`source_worktree_changeset_impl`]) — a local libgit2 diff of the merge-base baseline against
//! the worktree, with no network — and the entries land through the SAME `record_step_result`
//! harness auto-capture seam SW-AU-96 shipped, on the SAME node the branch-level work-product entity
//! attaches to. They therefore fold through the one `read_run_evidence` reader on both authority
//! paths with zero git I/O on the render path.
//!
//! Why capture at completion, not at read (unlike SW-AU-96's branch entity): the worktree git tree
//! is ephemeral — reaped after the run — so the per-file diff must be snapshotted while it is still
//! live. SW-AU-96 can fold its branch entity at read time only because the branch *name* is durable
//! (`outcome.work_product`); a tree is not. The two nonetheless converge on one node via one reader.

use std::path::Path;

use super::run_store::{
    self, ArtifactKind, FootprintChange, FootprintEntry, StepResultEvent, DEFAULT_STEP_NODE_ID,
    MAX_REPORTED_ENTRIES,
};
use crate::commands::git_sync::{source_worktree_changeset_impl, PublishFile, PublishFileStatus};

/// Map a source change-set into per-file footprint entries. Pure (no I/O) so the classification is
/// unit-tested without a repo. An added file is `Created`, any other change is `Modified`; a
/// deletion maps to neither `FootprintChange` variant, so it is dropped — the footprint lists files
/// the run produced or changed that still exist (AC-7). The `reference` is the change-set's
/// repo-relative path, matching the form an agent reports so the two dedupe by `(kind, reference)`.
///
/// The change-set's rename detection collapses a rename into a `Modified` at the post-rename path
/// (the pre-rename path is not emitted), and a copy rides `Added → Created` — both honest: the file
/// that exists after the run is what the footprint names.
fn file_footprint_entries(changeset: &[PublishFile]) -> Vec<FootprintEntry> {
    changeset
        .iter()
        .filter_map(|file| {
            let change = match file.status {
                PublishFileStatus::Added => FootprintChange::Created,
                PublishFileStatus::Modified => FootprintChange::Modified,
                PublishFileStatus::Deleted => return None,
            };
            Some(FootprintEntry {
                kind: ArtifactKind::File,
                reference: file.path.clone(),
                change,
            })
        })
        .collect()
}

/// Build the scrubbed, bounded per-file footprint event for a run's worktree, or `None` when there
/// is nothing to record — the worktree is unreachable / not a repo / the diff failed (AC-6 degrade),
/// or the diff is clean (AC-2 no-op). Split from [`capture_per_file_footprint`] so the diff→entries
/// shaping is unit-testable without writing to the run-store.
fn footprint_event(worktree_dir: &Path, story_id: &str, run_id: &str) -> Option<StepResultEvent> {
    // `None` base_ref ⇒ merge-base with origin's default branch (the git_sync base ladder) — the
    // run's OWN delta, not unrelated default-branch movement since it forked (AC-1). Local-only, no
    // network. Any failure (reaped worktree, non-repo path, git error) degrades to no entries (AC-6),
    // logged so a missing footprint is distinguishable from a genuinely clean diff after the fact.
    let changeset = match source_worktree_changeset_impl(worktree_dir, None) {
        Ok(changeset) => changeset,
        Err(error) => {
            log::debug!(target: "run_store", "per-file footprint skipped for run {run_id}: {error}");
            return None;
        }
    };
    let footprint = file_footprint_entries(&changeset);
    if footprint.is_empty() {
        return None;
    }
    if footprint.len() > MAX_REPORTED_ENTRIES {
        // AC-5: `scrubbed()` bounds the event to MAX_REPORTED_ENTRIES below; log the overflow so a
        // very large run's truncated footprint is auditable rather than a silent count clamp.
        log::debug!(
            target: "run_store",
            "per-file footprint for run {run_id} truncated to {MAX_REPORTED_ENTRIES} of {} files",
            footprint.len()
        );
    }
    // Same node the branch-level work-product entity attaches to, so an agent's own reports and both
    // auto-captures converge on one node and dedupe (AC-3). `scrubbed()` bounds to
    // `MAX_REPORTED_ENTRIES` and redacts each reference (AC-5).
    Some(
        StepResultEvent {
            story_id: story_id.to_string(),
            run_id: run_id.to_string(),
            node_id: DEFAULT_STEP_NODE_ID.to_string(),
            summary: None,
            outputs: Vec::new(),
            artifacts: Vec::new(),
            footprint,
        }
        .scrubbed(),
    )
}

/// Diff the run's work-product worktree and append its per-file footprint through the shipped
/// `record_step_result` harness auto-capture seam. Best-effort and never-fatal: an absent/reaped
/// worktree or a git failure records nothing and the step degrades to the branch-level entity
/// SW-AU-96 already shows (AC-6). Blocking git I/O — the caller MUST invoke it inside the completion
/// path's `await_blocking` boundary, never on the async executor or render thread (AC-5).
pub(crate) fn capture_per_file_footprint(worktree_dir: &Path, story_id: &str, run_id: &str) {
    if let Some(event) = footprint_event(worktree_dir, story_id, run_id) {
        run_store::record_step_result(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::git_sync::PublishFile;

    fn file(path: &str, status: PublishFileStatus) -> PublishFile {
        PublishFile {
            path: path.to_string(),
            status,
            badges: Vec::new(),
            rename_old: None,
            added: None,
            removed: None,
        }
    }

    /// AC-1: an added file maps to `Created`, a changed one to `Modified`.
    #[test]
    fn maps_added_to_created_and_modified_to_modified() {
        let entries = file_footprint_entries(&[
            file("a.rs", PublishFileStatus::Added),
            file("b.rs", PublishFileStatus::Modified),
        ]);
        assert_eq!(
            entries,
            vec![
                FootprintEntry {
                    kind: ArtifactKind::File,
                    reference: "a.rs".into(),
                    change: FootprintChange::Created,
                },
                FootprintEntry {
                    kind: ArtifactKind::File,
                    reference: "b.rs".into(),
                    change: FootprintChange::Modified,
                },
            ]
        );
    }

    /// AC-7: a deletion maps to neither `FootprintChange` variant, so it is dropped.
    #[test]
    fn drops_deletions() {
        let entries = file_footprint_entries(&[
            file("gone.rs", PublishFileStatus::Deleted),
            file("kept.rs", PublishFileStatus::Added),
        ]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].reference, "kept.rs");
        assert_eq!(entries[0].change, FootprintChange::Created);
    }

    fn init_repo_on_main(dir: &Path) -> git2::Repository {
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("main");
        git2::Repository::init_opts(dir, &opts).unwrap()
    }

    fn commit_worktree(repo: &git2::Repository, message: &str) {
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let parents: Vec<git2::Commit> = repo
            .head()
            .ok()
            .and_then(|h| h.peel_to_commit().ok())
            .into_iter()
            .collect();
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parent_refs)
            .unwrap();
    }

    /// AC-1 through the REAL diff seam: a worktree that adds `a.rs` and edits `b.rs` (vs its
    /// merge-base baseline) yields exactly the two `File` footprint entries with the right `change`,
    /// on the default node.
    #[test]
    fn worktree_add_and_edit_yields_two_file_entries() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo_on_main(dir.path());
        std::fs::write(dir.path().join("b.rs"), "old\n").unwrap();
        commit_worktree(&repo, "base");

        // The run's footprint: add a.rs, edit b.rs (uncommitted is diffed identically to committed
        // work against the merge-base baseline).
        std::fs::write(dir.path().join("a.rs"), "new\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "changed\n").unwrap();

        let event = footprint_event(dir.path(), "SW-AU-96B", "run-1").expect("has footprint");
        assert_eq!(event.node_id, DEFAULT_STEP_NODE_ID);
        let mut entries = event.footprint;
        entries.sort_by(|a, b| a.reference.cmp(&b.reference));
        assert_eq!(
            entries,
            vec![
                FootprintEntry {
                    kind: ArtifactKind::File,
                    reference: "a.rs".into(),
                    change: FootprintChange::Created,
                },
                FootprintEntry {
                    kind: ArtifactKind::File,
                    reference: "b.rs".into(),
                    change: FootprintChange::Modified,
                },
            ]
        );
    }

    /// AC-1/AC-2 through the real merge-base baseline: a run that COMMITS an add + edit on a feature
    /// branch forked from `main` yields the two File entries diffed against the fork point (not the
    /// branch tip) — the production path (committed work vs merge-base), distinct from the uncommitted
    /// diff the test above exercises.
    #[test]
    fn committed_branch_diff_against_merge_base_yields_entries() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo_on_main(dir.path());
        std::fs::write(dir.path().join("b.rs"), "old\n").unwrap();
        commit_worktree(&repo, "base"); // main @ base

        // Fork a feature branch at the base and commit the run's work onto it, so the baseline is the
        // merge-base (the fork point), not HEAD.
        let base_commit = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feature", &base_commit, false).unwrap();
        repo.set_head("refs/heads/feature").unwrap();
        std::fs::write(dir.path().join("a.rs"), "new\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "changed\n").unwrap();
        commit_worktree(&repo, "run work"); // feature @ add a.rs + edit b.rs

        let event = footprint_event(dir.path(), "SW-AU-96B", "run-1").expect("has footprint");
        let mut entries = event.footprint;
        entries.sort_by(|a, b| a.reference.cmp(&b.reference));
        assert_eq!(
            entries,
            vec![
                FootprintEntry {
                    kind: ArtifactKind::File,
                    reference: "a.rs".into(),
                    change: FootprintChange::Created,
                },
                FootprintEntry {
                    kind: ArtifactKind::File,
                    reference: "b.rs".into(),
                    change: FootprintChange::Modified,
                },
            ]
        );
    }

    /// AC-2: a clean worktree (no changes vs baseline) records nothing — a no-op, not an empty event.
    #[test]
    fn clean_worktree_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo_on_main(dir.path());
        std::fs::write(dir.path().join("b.rs"), "old\n").unwrap();
        commit_worktree(&repo, "base");

        assert!(footprint_event(dir.path(), "SW-AU-96B", "run-1").is_none());
    }

    /// AC-6: a path that is not a git repo degrades to no entries, never an error.
    #[test]
    fn non_repo_path_degrades_to_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(footprint_event(dir.path(), "SW-AU-96B", "run-1").is_none());
    }
}
