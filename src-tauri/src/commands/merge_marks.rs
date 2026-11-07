// SPDX-License-Identifier: MIT
//! SW-CP-24 C-layer: the merge-mark **set/clear seam** the CP-27 right-click
//! toggle invokes, plus the CP-27 **read seam** that
//! toggle uses to render tree state. Thin wrappers over
//! [`crate::install::merge_marks`] that resolve the team config dir exactly like
//! the sprint-registry commands (`org_root` + `team` → `path_to_team_config_dir`)
//! and run the filesystem work on the blocking pool — never the UI thread
//! (mirrors `save_sprint_registry` / `load_sprint_registry`).

use std::collections::BTreeMap;
use std::path::Path;

use crate::context_repo::path_to_team_config_dir;
use crate::install::merge_marks::{clear_merge_mark, load_merge_marks_from, set_merge_mark};

/// Mark a content-relative path as structured-merge (with optional per-array
/// `merge_key`s). Rejects a non-structured path (JSON/JSONC/YAML/TOML only).
#[tauri::command]
pub async fn set_content_merge_mark(
    org_root: String,
    team: String,
    logical_path: String,
    array_keys: BTreeMap<String, String>,
) -> Result<(), String> {
    crate::commands::await_blocking("set_content_merge_mark", move || {
        let config_dir = path_to_team_config_dir(Path::new(&org_root), &team);
        set_merge_mark(&config_dir, &logical_path, array_keys).map_err(|e| e.to_string())
    })
    .await
}

/// Clear a content-relative path's merge mark (idempotent — clearing an unmarked
/// path is a no-op success).
#[tauri::command]
pub async fn clear_content_merge_mark(
    org_root: String,
    team: String,
    logical_path: String,
) -> Result<(), String> {
    crate::commands::await_blocking("clear_content_merge_mark", move || {
        let config_dir = path_to_team_config_dir(Path::new(&org_root), &team);
        clear_merge_mark(&config_dir, &logical_path).map_err(|e| e.to_string())
    })
    .await
}

/// List every merge-marked content-relative path for a team (sorted). The CP-27
/// **read seam**: the toggle loads this once per team into a set that drives the
/// in-tree merge badge and the Mark/Unmark menu label. Filesystem read runs on
/// the blocking pool; a missing/malformed store yields an empty list (never an
/// error — the store loader is tolerant by construction).
#[tauri::command]
pub async fn list_content_merge_marks(
    org_root: String,
    team: String,
) -> Result<Vec<String>, String> {
    crate::commands::await_blocking("list_content_merge_marks", move || {
        let config_dir = path_to_team_config_dir(Path::new(&org_root), &team);
        Ok(load_merge_marks_from(&config_dir).marked_paths())
    })
    .await
}
