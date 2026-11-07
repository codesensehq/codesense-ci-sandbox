// SPDX-License-Identifier: MIT
//! Layout-resolver facade for commands plus retirement stubs for the IPC the
//! frontend still calls after managed repository migrations were removed.
use crate::context_repo::layout_resolver::{resolve_owned_layout, LegacyRepositoryPathLocator};
use crate::domain::{Principal, SourceRepoSlug};
use std::path::{Path, PathBuf};

#[tauri::command]
pub fn legacy_repository_statuses(org_slug: Option<String>) -> Vec<serde_json::Value> {
    let _ = org_slug;
    Vec::new()
}
#[tauri::command]
pub fn legacy_repository_local_state() -> &'static str {
    "ready"
}
#[tauri::command]
pub fn cancel_legacy_repository_job(job_key: String) -> Result<(), String> {
    let _ = job_key;
    Err("Managed repository migration is retired. Prepare local folders with your agent or file tools.".into())
}

pub fn resolve_tracked_repository_path(
    _codesense_home: &Path,
    _principal: Option<&Principal>,
    org_root: &Path,
    team: &str,
    slug: &SourceRepoSlug,
) -> Result<PathBuf, String> {
    crate::context_repo::layout_resolver::resolve_repository_root(org_root, team, slug)
}
pub fn resolve_worktree_config_path(
    _codesense_home: &Path,
    _principal: Option<&Principal>,
    org_root: &Path,
    _team: &str,
    slug: &SourceRepoSlug,
) -> Result<PathBuf, String> {
    let canonical = crate::context_repo::try_path_to_worktree_config(org_root, slug)
        .map_err(|e| e.to_string())?;
    resolve_owned_layout(
        &canonical,
        &LegacyRepositoryPathLocator::worktree_config(org_root, slug),
        org_root,
        None,
        slug,
    )
}
