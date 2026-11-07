// SPDX-License-Identifier: MIT
use std::path::Path;

use super::branch_common::with_branch_lock;
use super::mutate::{AppliedEdit, RegistryEditError};

fn ensure_safe_relative_path(path: &Path, operation: &str) -> Result<(), RegistryEditError> {
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(RegistryEditError::Io(format!(
            "refusing to {operation} unsafe path {} (must be relative, no `..`)",
            path.display()
        )));
    }
    Ok(())
}

/// Write an `AppliedEdit`'s files into the context-repo working tree **without
/// committing**. This is the local-only registry-edit apply step: the edit sits
/// as a pending working-tree change until the user publishes it via
/// `/cs-publish` (which diffs the publish baseline → working tree, so an
/// uncommitted edit publishes identically to a committed one).
///
/// Holds `BRANCH_LOCK` for the whole write so concurrent registry edits can't
/// interleave writes to the same file (e.g. two edits both rewriting
/// `teams.yml`) — the same serialization guarantee the former commit path gave.
/// Parent dirs are created defensively so a first-write to a not-yet-scaffolded
/// path (e.g. a new team's `repos/.gitkeep`) can't fail on a missing directory.
///
/// The edit is left UNSTAGED (this never touches the git index). It surfaces in
/// the publish set purely as a working-tree delta — so a brand-new untracked path
/// (a new team's `repos/.gitkeep`) relies on `publish_set_impl`'s diff being
/// configured with `include_untracked(true).recurse_untracked_dirs(true)`
/// (`git_sync.rs`). `no_commit_edit_surfaces_in_publish_set` guards that coupling.
/// Files are written in turn with no rollback, so a mid-loop I/O failure can
/// leave a multi-file edit half-applied in the working tree — identical to the
/// former commit path, which also wrote each file before its single commit.
pub fn write_edit_to_workdir(
    org_root: &Path,
    applied: &AppliedEdit,
) -> Result<(), RegistryEditError> {
    with_branch_lock(|| write_edit_to_workdir_locked(org_root, applied))
}

/// Write an edit while the caller already holds the non-reentrant branch lock.
/// Transactional read-modify-write callers use this seam so validation and the
/// atomic replacement are covered by one lock acquisition.
pub fn write_edit_to_workdir_locked(
    org_root: &Path,
    applied: &AppliedEdit,
) -> Result<(), RegistryEditError> {
    // Reject the whole batch before touching disk. Otherwise a valid first file
    // followed by an unsafe path would leave a rejected edit partially applied.
    for (rel_path, _) in &applied.files {
        ensure_safe_relative_path(rel_path, "write")?;
    }
    for (rel_path, bytes) in &applied.files {
        let abs_path = org_root.join(rel_path);
        // SW-AU-25: temp-file + rename (parent dirs created within) so a
        // concurrent reader — e.g. the scheduler tick reading this file —
        // never observes a truncated/torn write.
        crate::shared::fs_atomic::write_bytes_atomic(&abs_path, bytes)
            .map_err(|e| RegistryEditError::Io(format!("write {}: {e}", rel_path.display())))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn make_applied_edit(files: Vec<(&str, &str)>) -> AppliedEdit {
        AppliedEdit {
            files: files
                .into_iter()
                .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
                .collect(),

            commit_title: "Test edit".into(),
        }
    }

    #[test]
    fn writes_files_to_working_tree() {
        let tmp = tempdir().unwrap();
        let applied = make_applied_edit(vec![("teams.yml", "teams:\n  - name: default\n")]);

        write_edit_to_workdir(tmp.path(), &applied).unwrap();

        let content = std::fs::read_to_string(tmp.path().join("teams.yml")).unwrap();
        assert!(content.contains("default"));
    }

    #[test]
    fn creates_missing_parent_dirs() {
        // A first-write to a nested, not-yet-existing path (mirrors a new team's
        // `payments/repos/.gitkeep`) must not fail on the missing directory.
        let tmp = tempdir().unwrap();
        let applied = make_applied_edit(vec![("payments/repos/.gitkeep", "")]);

        write_edit_to_workdir(tmp.path(), &applied).unwrap();

        assert!(tmp
            .path()
            .join("payments")
            .join("repos")
            .join(".gitkeep")
            .is_file());
    }

    #[test]
    fn does_not_create_a_git_commit() {
        // The write path never opens the repo — a working-tree write leaves
        // `main` untouched. Init a repo, write, assert `main` is unmoved.
        let tmp = tempdir().unwrap();
        let repo = git2::Repository::init(tmp.path()).unwrap();
        let sig = git2::Signature::now("test", "test@test.com").unwrap();
        let tree_oid = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let orig = repo
            .commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        let applied = make_applied_edit(vec![("people.yml", "people: []\n")]);
        write_edit_to_workdir(tmp.path(), &applied).unwrap();

        let head = repo
            .find_reference("refs/heads/main")
            .unwrap()
            .peel_to_commit()
            .unwrap();
        assert_eq!(head.id(), orig, "no new commit on main");
        // ...but the file is on disk, pending.
        assert!(tmp.path().join("people.yml").is_file());
    }

    #[test]
    fn writes_multiple_files_atomically_under_lock() {
        let tmp = tempdir().unwrap();
        let applied = make_applied_edit(vec![("a.yml", "content-a"), ("b.yml", "content-b")]);

        write_edit_to_workdir(tmp.path(), &applied).unwrap();

        assert_eq!(
            std::fs::read_to_string(tmp.path().join("a.yml")).unwrap(),
            "content-a"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("b.yml")).unwrap(),
            "content-b"
        );
    }

    #[test]
    fn locked_writer_rejects_paths_outside_the_org_root() {
        let tmp = tempdir().unwrap();
        let edit = make_applied_edit(vec![("../escape.txt", "nope")]);

        let error = write_edit_to_workdir_locked(tmp.path(), &edit).unwrap_err();

        assert!(error.to_string().contains("unsafe path"));
        assert!(!tmp.path().parent().unwrap().join("escape.txt").exists());
    }

    #[test]
    fn locked_writer_rejects_an_unsafe_batch_before_writing_any_file() {
        let tmp = tempdir().unwrap();
        let edit = make_applied_edit(vec![
            ("safe.txt", "must not land"),
            ("../escape.txt", "nope"),
        ]);

        assert!(write_edit_to_workdir_locked(tmp.path(), &edit).is_err());

        assert!(!tmp.path().join("safe.txt").exists());
        assert!(!tmp.path().parent().unwrap().join("escape.txt").exists());
    }

    #[test]
    fn write_error_surfaces_as_io_error() {
        // org_root points at a file, not a dir → create_dir_all/write fails.
        let tmp = tempdir().unwrap();
        let not_a_dir = tmp.path().join("regular-file");
        std::fs::write(&not_a_dir, "x").unwrap();

        let applied = make_applied_edit(vec![("nested/x.yml", "y")]);
        let err = write_edit_to_workdir(&not_a_dir, &applied).unwrap_err();
        assert!(matches!(err, RegistryEditError::Io(_)));
    }
}
