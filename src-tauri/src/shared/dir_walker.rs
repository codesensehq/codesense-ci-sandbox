// SPDX-License-Identifier: MIT
//! Recursive directory walker — DRY-extracted from two callers that each
//! had their own DFS implementation with divergent error semantics.

use std::fs::{self, Metadata};
use std::io;
use std::path::Path;

/// What the walker should do after the callback handles an entry.
pub enum WalkAction {
    /// Entry is a directory — descend into it.
    /// Has no effect if the entry's metadata reports non-directory.
    Recurse,
    /// Entry was handled (or skipped); continue with siblings.
    Skip,
}

/// Recursively visit every entry under `dir`. The callback always receives a
/// path for context, plus the `io::Result` of fetching that path's metadata:
///   - **Success at an entry**: `path = entry.path()`, `result = Ok(metadata)`
///   - **Metadata failure at an entry**: `path = entry.path()`, `result = Err(io_err)`
///   - **Entry-iter failure (DirEntry produced an Err)**: `path = parent dir`, `result = Err(io_err)`
///   - **Top-level `read_dir` failure**: `path = dir`, `result = Err(io_err)`
///
/// The callback returns:
///   - `Ok(WalkAction::Recurse)` — descend into `path` (only honored when the
///     callback received `Ok(metadata)` AND metadata reports a directory;
///     ignored for non-directory entries and for any error-path invocation
///     since the walker has no path to descend into in those cases)
///   - `Ok(WalkAction::Skip)` — move to the next sibling
///   - `Err(e)` — abort the entire walk and propagate
///
/// Passing a path on every callback invocation lets callers reconstruct
/// context-rich error messages (e.g. "failed to stat /foo/bar") without
/// the walker having to know what error type the caller wants to produce.
pub fn walk_dir<E, F>(dir: &Path, callback: &mut F) -> Result<(), E>
where
    F: FnMut(&Path, io::Result<Metadata>) -> Result<WalkAction, E>,
{
    let read = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) => {
            callback(dir, Err(e))?;
            return Ok(());
        }
    };
    for entry in read {
        let entry = match entry {
            Ok(e) => e,
            Err(io_err) => {
                callback(dir, Err(io_err))?;
                continue;
            }
        };
        let path = entry.path();
        let (action, recurse_path) = match fs::symlink_metadata(&path) {
            Ok(meta) => {
                let is_dir = meta.is_dir();
                let action = callback(&path, Ok(meta))?;
                (action, if is_dir { Some(path) } else { None })
            }
            Err(io_err) => {
                let action = callback(&path, Err(io_err))?;
                (action, None)
            }
        };
        if matches!(action, WalkAction::Recurse) {
            if let Some(p) = recurse_path {
                walk_dir(&p, callback)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::convert::Infallible;
    use std::path::PathBuf;
    use tempfile::tempdir;

    /// Collects every entry path the walker yields successfully, descending
    /// into every directory.
    fn collect_all(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        walk_dir(dir, &mut |path, result| -> Result<WalkAction, Infallible> {
            let meta = match result {
                Ok(m) => m,
                Err(_) => return Ok(WalkAction::Skip),
            };
            out.push(path.to_path_buf());
            if meta.is_dir() {
                Ok(WalkAction::Recurse)
            } else {
                Ok(WalkAction::Skip)
            }
        })
        .unwrap();
        out
    }

    #[test]
    fn recursion_finds_all_files_in_nested_tree() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("a")).unwrap();
        fs::create_dir(root.join("a/b")).unwrap();
        fs::write(root.join("top.txt"), b"").unwrap();
        fs::write(root.join("a/mid.txt"), b"").unwrap();
        fs::write(root.join("a/b/leaf.txt"), b"").unwrap();

        let found: HashSet<PathBuf> = collect_all(root).into_iter().collect();
        assert!(found.contains(&root.join("top.txt")));
        assert!(found.contains(&root.join("a")));
        assert!(found.contains(&root.join("a/mid.txt")));
        assert!(found.contains(&root.join("a/b")));
        assert!(found.contains(&root.join("a/b/leaf.txt")));
    }

    #[test]
    fn callback_returning_err_aborts_walk() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("a.txt"), b"").unwrap();
        fs::write(root.join("b.txt"), b"").unwrap();
        fs::write(root.join("c.txt"), b"").unwrap();

        let mut seen = 0usize;
        let result: Result<(), &'static str> = walk_dir(root, &mut |_, _| {
            seen += 1;
            if seen == 2 {
                Err("stop")
            } else {
                Ok(WalkAction::Skip)
            }
        });
        assert_eq!(result, Err("stop"));
        assert_eq!(
            seen, 2,
            "walker should stop on the iteration that returns Err"
        );
    }

    #[test]
    fn skip_on_dir_does_not_recurse() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("blocked")).unwrap();
        fs::write(root.join("blocked/should_not_be_seen.txt"), b"").unwrap();
        fs::write(root.join("seen.txt"), b"").unwrap();

        let mut visited = Vec::new();
        walk_dir(root, &mut |path,
                             _result|
         -> Result<WalkAction, Infallible> {
            visited.push(path.to_path_buf());
            // Skip everything — never request recursion.
            Ok(WalkAction::Skip)
        })
        .unwrap();

        assert!(visited.contains(&root.join("blocked")));
        assert!(visited.contains(&root.join("seen.txt")));
        assert!(
            !visited.contains(&root.join("blocked/should_not_be_seen.txt")),
            "child of Skip'd dir should not be visited"
        );
    }

    #[test]
    fn top_level_read_dir_failure_invokes_callback_with_err() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does_not_exist");

        let mut err_invocations: Vec<PathBuf> = Vec::new();
        let mut ok_invocations: usize = 0;
        walk_dir(&missing, &mut |path,
                                 result|
         -> Result<WalkAction, Infallible> {
            match result {
                Err(_) => err_invocations.push(path.to_path_buf()),
                Ok(_) => ok_invocations += 1,
            }
            Ok(WalkAction::Skip)
        })
        .unwrap();

        assert_eq!(err_invocations, vec![missing.clone()]);
        assert_eq!(ok_invocations, 0);
    }
}
