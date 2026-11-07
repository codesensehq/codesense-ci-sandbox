// SPDX-License-Identifier: MIT
//! SV-PBF-267: the viewer's file list, served from the context index snapshot.
//!
//! `list_context_files` used to walk the folder on every call (the idle-CPU
//! storm). It now filters the records the index already holds, memoised per
//! (generation, root) so repeated calls share one result.

use std::sync::{Arc, Mutex};

use super::index::Snapshot;
use crate::commands::file_index::{is_viewable_extension, ContextFile};
use crate::commands::files::is_derived_render_artifact;

/// Prefix of the error for a root the index does not hold; the frontend
/// recognises it to show a clear message.
pub const NOT_INDEXED_PREFIX: &str = "not indexed:";

/// Roots kept in the memo, least recently used evicted first.
pub const MAX_MEMO_ROOTS: usize = 64;

/// Per-process memo of filtered lists: the newest snapshot only, at most
/// [`MAX_MEMO_ROOTS`] roots.
pub struct FileListMemo {
    state: Mutex<MemoState>,
}

struct MemoState {
    /// (generation, snapshot identity) the entries were filtered from.
    stamp: Option<(u64, usize)>,
    /// Least recently used first.
    entries: Vec<(String, Arc<Vec<ContextFile>>)>,
}

impl FileListMemo {
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(MemoState {
                stamp: None,
                entries: Vec::new(),
            }),
        }
    }

    /// The viewable files under `root` at this snapshot. Calls at the same
    /// generation for the same root filter once and share one `Arc`.
    pub fn list(
        &self,
        snapshot: &Arc<Snapshot>,
        root: &str,
    ) -> Result<Arc<Vec<ContextFile>>, String> {
        let key = normalize_root(root);
        require_indexed_root(snapshot, &key)?;
        let stamp = (snapshot.generation, Arc::as_ptr(snapshot) as usize);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.stamp != Some(stamp) {
            state.stamp = Some(stamp);
            state.entries.clear();
        }
        if let Some(i) = state.entries.iter().position(|(k, _)| *k == key) {
            let hit = state.entries.remove(i);
            let files = Arc::clone(&hit.1);
            state.entries.push(hit);
            return Ok(files);
        }
        let files = Arc::new(list_viewable(snapshot, &key));
        if state.entries.len() >= MAX_MEMO_ROOTS {
            state.entries.remove(0);
        }
        state.entries.push((key, Arc::clone(&files)));
        Ok(files)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.state.lock().unwrap().entries.len()
    }
}

/// Forward slashes, no trailing slash: the form the index stores paths in.
fn normalize_root(root: &str) -> String {
    let s = root.replace('\\', "/");
    match s.trim_end_matches('/') {
        "" => "/".to_string(),
        t => t.to_string(),
    }
}

fn is_under(path: &str, dir: &str) -> bool {
    path == dir
        || path
            .strip_prefix(dir)
            .is_some_and(|r| r.starts_with('/') || dir.ends_with('/'))
}

/// Q3 (SPEC §7, decided): refuse a root the index does not hold, instead of
/// listing whatever path the caller names. Revert by deleting this call.
fn require_indexed_root(snapshot: &Snapshot, root: &str) -> Result<(), String> {
    let indexed = snapshot.roots.iter().any(|r| {
        let dir = r.dir.to_string_lossy().replace('\\', "/");
        is_under(root, dir.trim_end_matches('/'))
    });
    if indexed {
        Ok(())
    } else {
        Err(format!("{NOT_INDEXED_PREFIX} {root}"))
    }
}

/// Q3 (SPEC §7, decided): secret paths (`prod.env`, `.npmrc`, keys) never
/// reach the list. Dot components are already dropped by the caller. Revert by
/// making this return `false`.
fn is_secret_listing_path(rel: &str) -> bool {
    use codesense_fs_core::filters::is_secret_path;
    is_secret_path(rel) || rel.split('/').any(is_secret_path)
}

/// Today's walker filters: viewable extension, no dot components, no derived
/// `*.excalidraw.png` render, sorted by relative path. `root` is normalized.
fn list_viewable(snapshot: &Snapshot, root: &str) -> Vec<ContextFile> {
    let mut files: Vec<ContextFile> = snapshot
        .records
        .iter()
        .filter_map(|record| {
            if !is_under(&record.path, root) || record.path == root {
                return None;
            }
            let rel = record.path[root.len()..].trim_start_matches('/');
            let name = &record.file_name;
            if rel.split('/').any(|c| c.starts_with('.'))
                || is_derived_render_artifact(name)
                || !std::path::Path::new(name)
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(is_viewable_extension)
                || is_secret_listing_path(rel)
            {
                return None;
            }
            Some(ContextFile {
                name: name.clone(),
                relative_path: rel.to_string(),
                full_path: record.path.clone(),
            })
        })
        .collect();
    files.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    files
}
