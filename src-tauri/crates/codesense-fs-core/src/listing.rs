// SPDX-License-Identifier: MIT
//! Single-level directory listing: filtered, symlink-resolved, sized, and sorted
//! directories-first in natural order.

use std::cmp::Ordering;
use std::fs;
use std::path::Path;

use serde::Serialize;

use crate::filters::is_hidden_from_listing;
use crate::natural_sort::natural_cmp;

#[derive(Debug, Clone, Serialize)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub extension: Option<String>,
    /// File size in bytes; `None` for directories (and when metadata is
    /// unreadable). Lets the in-app editor gate an oversized file
    /// to a placeholder BEFORE reading it — so a multi-MB file is never pulled
    /// over IPC into the read-only view (keeps the inset non-blocking).
    pub size: Option<u64>,
}

/// List one level of `dir` — ungated; [`crate::FsService::list`] is the only
/// caller and admits `dir` first. A missing directory lists as empty.
pub(crate) fn list_dir(dir: &Path) -> Result<Vec<FileEntry>, String> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| format!("failed to read dir: {}", e))? {
        let entry = entry.map_err(|e| format!("failed to read entry: {}", e))?;
        let file_type = entry
            .file_type()
            .map_err(|e| format!("failed to get type: {}", e))?;
        let name = entry.file_name().to_string_lossy().to_string();

        if is_hidden_from_listing(&name) {
            continue;
        }

        // Resolve THROUGH symlinks (`fs::metadata` follows; `DirEntry`'s
        // does not) so a symlink-to-dir lists as a navigable directory and a
        // symlink-to-file is sized by its target — what a re-listing and a
        // read actually traverse. Fall back to the no-follow `file_type` for a
        // broken symlink / loop (metadata errs): it then reads as a plain
        // (un-openable) file and degrades to the graceful error state. For
        // non-symlinks the two agree exactly, so regular entries (the whole
        // context repo) are unaffected.
        let resolved = fs::metadata(entry.path()).ok();
        let is_dir = resolved
            .as_ref()
            .map(|m| m.is_dir())
            .unwrap_or_else(|| file_type.is_dir());
        let extension = if !is_dir {
            Path::new(&name)
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_string())
        } else {
            None
        };
        // Byte size for files (None for dirs / unreadable
        // metadata) — from the same resolved (symlink-followed) metadata.
        let size = if is_dir {
            None
        } else {
            resolved.as_ref().map(|m| m.len())
        };

        entries.push(FileEntry {
            name,
            path: entry.path().display().to_string(),
            is_dir,
            extension,
            size,
        });
    }

    // Sort: directories first, then natural sort (numeric-aware)
    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => natural_cmp(&a.name, &b.name),
    });

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn names(entries: &[FileEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn test_list_directory_sorts_dirs_first() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("beta.md"), "b").unwrap();
        fs::write(dir.path().join("alpha.md"), "a").unwrap();
        fs::create_dir_all(dir.path().join("subdir")).unwrap();

        let entries = list_dir(dir.path()).unwrap();

        assert_eq!(entries[0].name, "subdir");
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].name, "alpha.md");
        assert_eq!(entries[1].extension.as_deref(), Some("md"));
        assert_eq!(entries[2].name, "beta.md");
    }

    // The listing reports a byte size for files (so the in-app
    // editor can gate an oversized file before reading it) and `None` for
    // directories.
    #[test]
    fn test_list_directory_reports_file_size() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("data.txt"), "hello world").unwrap(); // 11 bytes
        fs::create_dir_all(dir.path().join("sub")).unwrap();

        let entries = list_dir(dir.path()).unwrap();
        let entry = |name: &str| {
            let e = entries.iter().find(|e| e.name == name).unwrap();
            (e.is_dir, e.size)
        };

        assert_eq!(entry("data.txt"), (false, Some(11)));
        assert_eq!(entry("sub"), (true, None));
    }

    // A symlink pointing at a directory must resolve as
    // a navigable directory (is_dir=true, no size) — not an un-openable "file" —
    // because the tree re-lists through the link.
    #[cfg(unix)]
    #[test]
    fn test_list_directory_follows_symlink_to_dir() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("realdir")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("realdir"), dir.path().join("linkdir")).unwrap();

        let entries = list_dir(dir.path()).unwrap();
        let resolve = |name: &str| {
            let e = entries.iter().find(|e| e.name == name).unwrap();
            (e.is_dir, e.size)
        };

        assert_eq!(resolve("linkdir"), (true, None)); // symlink → dir, navigable
        assert_eq!(resolve("realdir"), (true, None));
    }

    #[test]
    fn test_natural_sort_dirs_first_with_numbers() {
        // AC 4 + dirs-first: directories sort first, then natural sort within each group
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("10-ux-polish")).unwrap();
        fs::create_dir_all(dir.path().join("2-context-repo")).unwrap();
        fs::create_dir_all(dir.path().join("1-tauri-react")).unwrap();
        fs::write(dir.path().join("10-story.md"), "").unwrap();
        fs::write(dir.path().join("2-story.md"), "").unwrap();
        fs::write(dir.path().join("1-story.md"), "").unwrap();

        let entries = list_dir(dir.path()).unwrap();

        // Directories first, naturally sorted
        assert_eq!(entries[0].name, "1-tauri-react");
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].name, "2-context-repo");
        assert!(entries[1].is_dir);
        assert_eq!(entries[2].name, "10-ux-polish");
        assert!(entries[2].is_dir);
        // Files second, naturally sorted
        assert_eq!(entries[3].name, "1-story.md");
        assert!(!entries[3].is_dir);
        assert_eq!(entries[4].name, "2-story.md");
        assert_eq!(entries[5].name, "10-story.md");
    }

    #[test]
    fn list_directory_visibility_partitions_a_real_directory() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".claude")).unwrap();
        fs::create_dir_all(dir.path().join("node_modules")).unwrap();
        fs::write(dir.path().join("README.md"), "r").unwrap();
        fs::write(dir.path().join(".DS_Store"), "d").unwrap();
        fs::write(dir.path().join(".env"), "SECRET=1").unwrap();
        // `.git` in a linked worktree is a FILE (gitdir pointer), not a dir —
        // the bare-name VCS guard drops both forms.
        fs::write(dir.path().join(".git"), "gitdir: /elsewhere").unwrap();

        let entries = list_dir(dir.path()).unwrap();

        assert_eq!(
            names(&entries),
            vec![".claude", "node_modules", "README.md"]
        );
    }

    #[test]
    fn a_file_entry_keeps_its_wire_shape() {
        let entry = FileEntry {
            name: "a.md".into(),
            path: "/wt/a.md".into(),
            is_dir: false,
            extension: Some("md".into()),
            size: Some(3),
        };
        assert_eq!(
            serde_json::to_string(&entry).unwrap(),
            r#"{"name":"a.md","path":"/wt/a.md","is_dir":false,"extension":"md","size":3}"#
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_broken_symlink_lists_as_an_unsized_file() {
        let dir = tempdir().unwrap();
        std::os::unix::fs::symlink(dir.path().join("gone"), dir.path().join("dangling")).unwrap();

        let entries = list_dir(dir.path()).unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "dangling");
        assert!(!entries[0].is_dir);
        assert_eq!(entries[0].size, None);
    }

    #[test]
    fn a_missing_directory_lists_empty() {
        let dir = tempdir().unwrap();
        assert!(list_dir(&dir.path().join("gone")).unwrap().is_empty());
    }
}
