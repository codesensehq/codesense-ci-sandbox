// SPDX-License-Identifier: MIT
//! The local worktree editor's file services beyond plain read/write: the
//! save-time conflict check, a file's committed (`HEAD`) bytes for the change
//! gutter, and the quick-open file list. Every path passes the same
//! registered-worktree gate as `write_worktree_file`; the remote twins live in
//! `remote_source.rs`.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::shared::hash::sha256_hex;
use serde::Serialize;

use super::files::validate_worktree_path_owned;

/// Prefix of the error a save returns when disk moved since the buffer's
/// baseline. The editor matches on it to open its conflict flow instead of
/// reporting a generic write failure.
pub const CONFLICT_PREFIX: &str = "conflict:";

/// Quick-open lists at most this many files; past it the list is marked
/// truncated so the palette can say so.
const MAX_LISTED_FILES: usize = 50_000;

/// A committed file larger than this has no gutter baseline. It matches the
/// remote reply cap (4 MiB), above the editor's 1.5 MB open limit, so a file
/// that grew past that limit still diffs against a smaller committed version.
const MAX_HEAD_BYTES: usize = 4 * 1024 * 1024;

/// The lock token for "the file must not exist yet" (the editor recreates a
/// file deleted under unsaved edits, and must not overwrite one that came back).
pub const ABSENT_TOKEN: &str = "absent";

/// Refuse a save whose baseline no longer matches disk. `expected` is the
/// SHA-256 of the content the buffer was loaded from (or last saved as), or
/// [`ABSENT_TOKEN`]. A file that has since vanished fails a hash lock too (as
/// on the remote side): the editor reads again, sees it gone, and recreates
/// it only with an explicit "absent" lock.
pub(crate) fn check_save_baseline(path: &Path, expected: Option<&str>) -> Result<(), String> {
    let Some(expected) = expected else {
        return Ok(());
    };
    if expected == ABSENT_TOKEN {
        // Follows links: a dangling link is absent (the save replaces it).
        return match std::fs::metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Ok(_) => Err(format!(
                "{CONFLICT_PREFIX} {} was created since it was deleted",
                path.display()
            )),
            Err(e) => Err(format!(
                "failed to check {} before saving: {e}",
                path.display()
            )),
        };
    }
    match std::fs::read(path) {
        Ok(current) => {
            if sha256_hex(&current).eq_ignore_ascii_case(expected) {
                Ok(())
            } else {
                Err(format!(
                    "{CONFLICT_PREFIX} {} changed on disk since it was opened",
                    path.display()
                ))
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(format!(
            "{CONFLICT_PREFIX} {} was deleted since it was opened",
            path.display()
        )),
        Err(e) => Err(format!(
            "failed to check {} before saving: {e}",
            path.display()
        )),
    }
}

fn git(cwd: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(cwd)
        .args([
            "-c",
            "core.quotepath=off",
            "-c",
            "core.fsmonitor=false",
            "--literal-pathspecs",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// The committed bytes of `file`, or `None` when it is not in `HEAD` (new or
/// untracked), not in a repository, or too large to diff.
pub(crate) fn head_bytes(file: &Path) -> Option<Vec<u8>> {
    let dir = file.parent()?;
    let name = file.file_name()?.to_str()?;
    // `ls-tree` resolves the pathspec relative to `-C`, and answers absence
    // with empty output rather than an error.
    let listing = git(dir)
        .args(["ls-tree", "-z", "HEAD", "--", name])
        .output()
        .ok()
        .filter(|o| o.status.success())?
        .stdout;
    // The entry for exactly this file (paths print relative to `-C`).
    let meta = listing
        .split(|b| *b == 0)
        .filter_map(|r| std::str::from_utf8(r).ok())
        .filter_map(|r| r.split_once('\t'))
        .find(|(_, entry)| *entry == name)
        .map(|(meta, _)| meta)?;
    let mut fields = meta.split(' ');
    let (mode, kind, object) = (fields.next()?, fields.next()?, fields.next()?);
    // A symlink's blob is its target, not file content to diff against.
    if mode == "120000" || kind != "blob" || !object.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    // Size first, so an oversized blob is never buffered whole.
    let size = git(dir)
        .args(["cat-file", "-s", object])
        .output()
        .ok()
        .filter(|o| o.status.success())?
        .stdout;
    let size: usize = std::str::from_utf8(&size).ok()?.trim().parse().ok()?;
    if size > MAX_HEAD_BYTES {
        return None;
    }
    let blob = git(dir)
        .args(["cat-file", "blob", object])
        .output()
        .ok()
        .filter(|o| o.status.success())?
        .stdout;
    (blob.len() <= MAX_HEAD_BYTES).then_some(blob)
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeFileList {
    /// Paths relative to the worktree root, sorted.
    pub paths: Vec<String>,
    pub truncated: bool,
    /// The checked-out branch (the status bar shows it); the short commit when
    /// detached, `None` when Git cannot say.
    pub branch: Option<String>,
}

fn current_branch(root: &Path) -> Option<String> {
    let out = |args: &[&str]| -> Option<String> {
        let output = git(root).args(args).output().ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8(output.stdout).ok()?.trim().to_string();
        (!text.is_empty()).then_some(text)
    };
    match out(&["rev-parse", "--abbrev-ref", "HEAD"])? {
        head if head == "HEAD" => out(&["rev-parse", "--short", "HEAD"]),
        branch => Some(branch),
    }
}

/// Tracked plus untracked-but-not-ignored files under `root`.
pub(crate) fn list_files(root: &Path, limit: usize) -> Result<WorktreeFileList, String> {
    let output = git(root)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output()
        .map_err(|e| format!("git is unavailable: {e}"))?;
    if !output.status.success() {
        return Err(format!("{} is not a git worktree", root.display()));
    }
    let split = |bytes: &[u8]| -> Vec<String> {
        bytes
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .filter_map(|p| std::str::from_utf8(p).ok().map(str::to_string))
            .collect()
    };
    let mut paths = split(&output.stdout);
    // An untracked nested repository lists as `dir/`: a directory, not a file
    // quick open could open.
    paths.retain(|p| !p.ends_with('/'));
    // Tracked files deleted from the working tree are still in the index;
    // opening one could only fail, so quick open doesn't offer them.
    let deleted: std::collections::HashSet<String> = git(root)
        .args(["ls-files", "-z", "--deleted"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| split(&o.stdout).into_iter().collect())
        .unwrap_or_default();
    if !deleted.is_empty() {
        paths.retain(|p| !deleted.contains(p));
    }
    paths.sort();
    paths.dedup();
    let truncated = paths.len() > limit;
    paths.truncate(limit);
    Ok(WorktreeFileList {
        paths,
        truncated,
        branch: current_branch(root),
    })
}

/// A worktree file's committed text for the editor's change gutter. `None`
/// for a new/untracked file, a non-repository path, or non-UTF-8 bytes.
#[tauri::command]
pub async fn worktree_file_head(path: String) -> Result<Option<String>, String> {
    crate::commands::await_blocking("worktree_file_head", move || {
        let file = validate_worktree_path_owned(&path)?;
        Ok(head_bytes(&file).and_then(|bytes| String::from_utf8(bytes).ok()))
    })
    .await
}

/// Quick-open's file list for a local worktree root.
#[tauri::command]
pub async fn worktree_list_files(root: String) -> Result<WorktreeFileList, String> {
    crate::commands::await_blocking("worktree_list_files", move || {
        let root = validate_worktree_path_owned(&root)?;
        list_files(&root, MAX_LISTED_FILES)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(cwd: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(cwd)
            .args([
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), &["init", "-q", "-b", "main"]);
        dir
    }

    #[test]
    fn save_baseline_passes_on_match_and_refuses_a_moved_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "one").unwrap();
        assert!(check_save_baseline(&file, None).is_ok());
        assert!(check_save_baseline(&file, Some(&sha256_hex(b"one"))).is_ok());
        assert!(check_save_baseline(&file, Some(&sha256_hex(b"one").to_uppercase())).is_ok());
        std::fs::write(&file, "two").unwrap();
        let err = check_save_baseline(&file, Some(&sha256_hex(b"one"))).unwrap_err();
        assert!(err.starts_with(CONFLICT_PREFIX), "{err}");
        // A vanished file fails the hash lock (the editor re-reads and saves
        // with "absent" to recreate it).
        std::fs::remove_file(&file).unwrap();
        let err = check_save_baseline(&file, Some(&sha256_hex(b"one"))).unwrap_err();
        assert!(err.starts_with(CONFLICT_PREFIX), "{err}");
        // "absent" holds while the file is gone, and refuses one that came back.
        assert!(check_save_baseline(&file, Some(ABSENT_TOKEN)).is_ok());
        std::fs::write(&file, "back").unwrap();
        let err = check_save_baseline(&file, Some(ABSENT_TOKEN)).unwrap_err();
        assert!(err.starts_with(CONFLICT_PREFIX), "{err}");
        // A dangling link counts as absent (the atomic rename replaces it).
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(dir.path().join("nowhere"), &link).unwrap();
        assert!(check_save_baseline(&link, Some(ABSENT_TOKEN)).is_ok());
    }

    #[test]
    fn sha256_hex_matches_the_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn head_bytes_reads_committed_content_in_nested_dirs_and_none_otherwise() {
        let dir = repo();
        std::fs::create_dir_all(dir.path().join("src/deep")).unwrap();
        std::fs::write(dir.path().join("src/deep/lib.rs"), "committed\n").unwrap();
        run(dir.path(), &["add", "."]);
        run(dir.path(), &["commit", "-q", "-m", "c"]);
        std::fs::write(dir.path().join("src/deep/lib.rs"), "edited\n").unwrap();
        std::fs::write(dir.path().join("src/deep/new.rs"), "new\n").unwrap();
        assert_eq!(
            head_bytes(&dir.path().join("src/deep/lib.rs")),
            Some(b"committed\n".to_vec())
        );
        assert_eq!(head_bytes(&dir.path().join("src/deep/new.rs")), None);
        // A name with pathspec magic characters is matched literally.
        std::fs::write(dir.path().join("src/*.rs"), "star\n").unwrap();
        assert_eq!(head_bytes(&dir.path().join("src/*.rs")), None);
        // A committed symlink's blob is its target, never a file baseline.
        std::os::unix::fs::symlink("deep/lib.rs", dir.path().join("src/link.rs")).unwrap();
        run(dir.path(), &["add", "src/link.rs"]);
        run(dir.path(), &["commit", "-q", "-m", "link"]);
        assert_eq!(head_bytes(&dir.path().join("src/link.rs")), None);
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("x"), "x").unwrap();
        assert_eq!(head_bytes(&outside.path().join("x")), None);
    }

    #[test]
    fn list_files_includes_untracked_skips_ignored_and_truncates() {
        let dir = repo();
        std::fs::write(dir.path().join(".gitignore"), "dist/\n").unwrap();
        std::fs::write(dir.path().join("tracked.ts"), "").unwrap();
        run(dir.path(), &["add", "."]);
        run(dir.path(), &["commit", "-q", "-m", "c"]);
        std::fs::write(dir.path().join("untracked.ts"), "").unwrap();
        std::fs::create_dir(dir.path().join("dist")).unwrap();
        std::fs::write(dir.path().join("dist/out.js"), "").unwrap();
        assert_eq!(
            list_files(dir.path(), 10).unwrap(),
            WorktreeFileList {
                paths: vec![
                    ".gitignore".into(),
                    "tracked.ts".into(),
                    "untracked.ts".into()
                ],
                truncated: false,
                branch: Some("main".into()),
            }
        );
        assert!(list_files(dir.path(), 1).unwrap().truncated);
        // A tracked file deleted from the working tree isn't offered (opening
        // it could only fail).
        std::fs::remove_file(dir.path().join("tracked.ts")).unwrap();
        assert!(!list_files(dir.path(), 10)
            .unwrap()
            .paths
            .contains(&"tracked.ts".to_string()));
        // An untracked nested repository is a directory, not a file to offer.
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        run(&dir.path().join("nested"), &["init", "-q"]);
        std::fs::write(dir.path().join("nested/x.ts"), "").unwrap();
        assert!(list_files(dir.path(), 10)
            .unwrap()
            .paths
            .iter()
            .all(|p| !p.starts_with("nested")));
        let not_repo = tempfile::tempdir().unwrap();
        assert!(list_files(not_repo.path(), 10).is_err());
    }
}
