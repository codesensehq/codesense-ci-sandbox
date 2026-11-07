// SPDX-License-Identifier: MIT
//! SW-CP-28 C-layer: the team-level **projection-exclude read/write seam** the
//! Settings "Projection excludes" section invokes. Thin wrappers over
//! [`crate::install::projection_excludes`] that resolve the team config dir exactly
//! like the merge-mark / sprint-registry commands (`org_root` + `team` →
//! `path_to_team_config_dir`) and run the filesystem work on the blocking pool —
//! never the UI thread.
//!
//! The read command returns BOTH the in-code default globs and the team's raw
//! override patterns, so the FE renders defaults-as-toggles from backend data and
//! never hardcodes the credential globs (they would drift from `SECRET_GLOBS`).

use std::path::Path;

use serde::Serialize;

use crate::content_engine;
use crate::context_repo::path_to_team_config_dir;
use crate::install::projection_excludes::{load_patterns_from, write_patterns_to};

/// The Settings view model: the in-code defaults (rendered as toggles) plus the
/// team's raw override patterns (adds + gitignore-style `!`-negations) as written.
#[derive(Debug, Serialize)]
pub struct ProjectionExcludesView {
    /// The single-source-of-truth default globs (`SECRET_GLOBS`). The FE renders
    /// each as a pre-checked toggle; a `!<default>` in `patterns` unchecks it.
    pub defaults: Vec<String>,
    /// The team's override patterns exactly as stored — added excludes and
    /// `!`-negations. Empty for an unconfigured team.
    pub patterns: Vec<String>,
}

/// Read the team's projection-exclude view (defaults + overrides). A
/// missing/malformed store yields the defaults with an empty override list (never
/// an error — the loader is tolerant by construction). Filesystem read runs on the
/// blocking pool.
#[tauri::command]
pub async fn get_projection_excludes(
    org_root: String,
    team: String,
) -> Result<ProjectionExcludesView, String> {
    crate::commands::await_blocking("get_projection_excludes", move || {
        let config_dir = path_to_team_config_dir(Path::new(&org_root), &team);
        Ok(ProjectionExcludesView {
            defaults: codesense_fs_core::filters::SECRET_GLOBS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            patterns: load_patterns_from(&config_dir),
        })
    })
    .await
}

/// Write the team's projection-exclude override patterns (working-tree edit,
/// PR-reviewable via publish like every other team-config change). Each pattern is
/// validated first — a bad glob is rejected with its parse error so the UI can
/// surface it inline, rather than writing a config the resolver would silently
/// skip. The next projection wave re-resolves from the new mtime (no manual
/// refresh). Filesystem write runs on the blocking pool.
#[tauri::command]
pub async fn set_projection_excludes(
    org_root: String,
    team: String,
    patterns: Vec<String>,
) -> Result<(), String> {
    crate::commands::await_blocking("set_projection_excludes", move || {
        for p in &patterns {
            content_engine::validate_exclude_glob(p)
                .map_err(|e| format!("invalid exclude glob {p:?}: {e}"))?;
        }
        let config_dir = path_to_team_config_dir(Path::new(&org_root), &team);
        write_patterns_to(&config_dir, patterns).map_err(|e| e.to_string())
    })
    .await
}
