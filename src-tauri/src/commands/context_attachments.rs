// SPDX-License-Identifier: MIT
//! Local-only context attachment entrypoints shared by desktop IPC and local MCP.
use std::path::PathBuf;

use tauri::{AppHandle, Emitter, Manager};

use crate::context_repo::{codesense_docs, context_attachments, sample_source_repo};
use crate::domain::Principal;

async fn attachment_identity_without_start(
    app: &AppHandle,
) -> Result<(PathBuf, Principal), String> {
    let home = super::paths::codesense_home_dir().ok_or("cannot resolve home directory")?;
    Ok((home, super::local_profile::active(app)?.principal))
}

#[tauri::command]
pub async fn attach_context_repository(
    app: AppHandle,
    clone_path: String,
) -> Result<super::org::ConnectOrgResult, String> {
    let (home, principal) = attachment_identity_without_start(&app).await?;
    let result = super::await_blocking("attach context repository", move || {
        let attachment =
            context_attachments::attach(&home, &principal, &PathBuf::from(clone_path))?;
        let loaded_org = super::registry::load_attached_org(&attachment.slug, &attachment.root);
        Ok(super::org::ConnectOrgResult {
            org_slug: attachment.slug,
            org_root: attachment.root.to_string_lossy().into_owned(),
            clone_outcome: "attached".into(),
            loaded_org,
        })
    })
    .await?;
    // SW-OC-26: the footprint changed — re-index it.
    crate::context_query::reset_footprint(&app);
    app.emit("context-attachments-changed", ())
        .map_err(|e| format!("Context attached, but refresh notification failed: {e}"))?;
    Ok(result)
}

/// The bundled CodeSense docs context repo, placed and registered for the active profile.
#[derive(Debug, serde::Serialize)]
pub struct CodesenseDocsResult {
    pub org_slug: String,
    pub org_root: String,
    pub team: String,
    pub readme_path: String,
    /// `placed` on a profile's first launch, `updated` when a newer bundle replaced an
    /// untouched copy, `kept` otherwise.
    pub placement: &'static str,
}

/// Places the bundled CodeSense docs repo (a Tauri resource) under the CodeSense home
/// and registers it. Runs no git beyond a local libgit2 checkout of the bundled commit.
#[tauri::command]
pub async fn ensure_codesense_docs(app: AppHandle) -> Result<CodesenseDocsResult, String> {
    let (home, principal) = attachment_identity_without_start(&app).await?;
    let archive_path = app
        .path()
        .resolve(
            "resources/codesense-docs.tar.gz",
            tauri::path::BaseDirectory::Resource,
        )
        .map_err(|e| format!("Cannot locate the bundled CodeSense docs: {e}"))?;
    super::await_blocking("ensure codesense docs", move || {
        let archive = std::fs::read(&archive_path).map_err(|e| {
            format!(
                "Cannot read the bundled CodeSense docs at {}: {e}",
                archive_path.display()
            )
        })?;
        let placed = codesense_docs::ensure_placed(&home, &principal, &archive)?;
        Ok(CodesenseDocsResult {
            org_slug: placed.attachment.slug,
            org_root: placed.attachment.root.to_string_lossy().into_owned(),
            team: codesense_docs::bundle_info().team.clone(),
            readme_path: placed.readme_path.to_string_lossy().into_owned(),
            placement: match placed.placement {
                codesense_docs::Placement::Placed => "placed",
                codesense_docs::Placement::Updated => "updated",
                codesense_docs::Placement::Kept => "kept",
            },
        })
    })
    .await
}

/// The bundled sample source repo (SW-ODC-11), placed per profile.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SampleSourceRepoResult {
    pub root: String,
    pub bundled_commit: String,
    /// `placed`, `updated`, `kept`, or `refused` — AC-11 posture.
    pub placement: &'static str,
    pub available: bool,
    /// Set when `placement` is `refused` (AC-11).
    pub refused_reason: Option<String>,
}

/// Places the bundled sample source repo (a Tauri resource) under the CodeSense home.
/// SW-ODC-11. Non-destructive + idempotent: a non-matching existing directory is kept
/// and the result reports `placement: "refused"` so dashboards can fall back to
/// seed-data-only mode without a live repo (AC-11). Mirrors `ensure_codesense_docs`.
#[tauri::command]
pub async fn ensure_sample_repo(app: AppHandle) -> Result<SampleSourceRepoResult, String> {
    let (home, principal) = attachment_identity_without_start(&app).await?;
    let archive_path = app
        .path()
        .resolve(
            "resources/sample-source-repo.tar.gz",
            tauri::path::BaseDirectory::Resource,
        )
        .map_err(|e| format!("Cannot locate the bundled sample source repo: {e}"))?;
    super::await_blocking("ensure sample source repo", move || {
        let archive = std::fs::read(&archive_path).map_err(|e| {
            format!(
                "Cannot read the bundled sample source repo at {}: {e}",
                archive_path.display()
            )
        })?;
        let placed = sample_source_repo::ensure_placed(&home, &principal, &archive)?;
        Ok(SampleSourceRepoResult {
            root: placed.root.to_string_lossy().into_owned(),
            bundled_commit: placed.bundled_commit,
            placement: match placed.placement {
                sample_source_repo::Placement::Placed => "placed",
                sample_source_repo::Placement::Updated => "updated",
                sample_source_repo::Placement::Kept => "kept",
                sample_source_repo::Placement::Refused => "refused",
            },
            available: !matches!(placed.placement, sample_source_repo::Placement::Refused),
            refused_reason: placed.refused_reason,
        })
    })
    .await
}

#[tauri::command]
pub async fn list_context_attachments(
    app: AppHandle,
) -> Result<Vec<super::org::OrgSummary>, String> {
    let (home, principal) = attachment_identity_without_start(&app).await?;
    super::await_blocking("list context attachments", move || {
        attachment_summaries(&home, &principal)
    })
    .await
}

#[tauri::command]
pub async fn detach_context_repository(app: AppHandle, org_slug: String) -> Result<(), String> {
    if codesense_docs::is_docs_slug(&org_slug) {
        return Err("The CodeSense docs context repo stays connected.".into());
    }
    let (home, principal) = attachment_identity_without_start(&app).await?;
    let watcher = app.try_state::<super::watch::WatcherState>();
    let _activation = match watcher.as_ref() {
        Some(state) => Some(state.lock_team_activation().await),
        None => None,
    };
    let detached_slug = org_slug.clone();
    let detach_app = app.clone();
    super::await_blocking("detach context repository", move || {
        let attachment = context_attachments::registered(&home, &principal, &org_slug)?;
        context_attachments::detach(&home, &principal, &org_slug)?;
        if let Some(coord) =
            detach_app.try_state::<std::sync::Arc<crate::content_engine::sync::WaveCoordinator>>()
        {
            coord.detach_context(&home, principal.as_str(), &org_slug);
        }
        if let Some(attachment) = attachment {
            super::watch::detach_context_watches(&detach_app, &attachment.root)?;
        }
        Ok(())
    })
    .await?;
    // SW-OC-26: stop serving the detached org's files at once, then re-index.
    crate::context_query::reset_footprint(&app);
    if let Some(state) = app.try_state::<crate::mcp::state::ActiveTeamState>() {
        state.clear_org(&detached_slug);
    }
    crate::context_repo::scratch_space::clear_active_org_if(&detached_slug);
    app.emit("context-attachments-changed", ())
        .map_err(|e| format!("Context detached, but refresh notification failed: {e}"))
}

/// Production cold-start orchestration; failed validation never declares a guest.
#[cfg(test)]
pub(crate) fn attach_with_session(
    home: &std::path::Path,
    principal: &Principal,
    selected: &std::path::Path,
    state: &super::guest_session::GuestSessionState,
) -> Result<context_attachments::ContextAttachment, String> {
    let attachment = context_attachments::attach(home, principal, selected)?;
    if principal == &Principal::local_guest() {
        state.declare(home, true)?;
    }
    Ok(attachment)
}

pub(crate) fn attachment_summaries(
    home: &std::path::Path,
    principal: &Principal,
) -> Result<Vec<super::org::OrgSummary>, String> {
    context_attachments::list(home, principal)?.into_iter().map(|attachment| {
        let availability_error = if crate::context_repo::path_to_managed_org_root(home, principal, &attachment.slug).exists() {
            Some("This identity also exists in managed storage; detach this registration to repair the collision".into())
        } else { context_attachments::available_root(&attachment).err() };
        Ok(super::org::attached_summary(attachment, availability_error))
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_summaries_report_origin() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("state");
        let root = context_attachments::tests::checkout(tmp.path(), "checkout", "acme/context");
        let who = Principal::local_guest();
        context_attachments::attach(&home, &who, &root).unwrap();

        let rows = attachment_summaries(&home, &who).unwrap();
        assert!(!rows[0].has_remote);
        assert_eq!(rows[0].remote_url, None);

        // An origin added after attaching shows on the next read, with no re-attach.
        let repo = git2::Repository::open(&root).unwrap();
        repo.remote("origin", "https://github.com/o/r.git").unwrap();
        let rows = attachment_summaries(&home, &who).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].is_attached);
        assert!(rows[0].has_remote);
        assert_eq!(
            rows[0].remote_url.as_deref(),
            Some("https://github.com/o/r.git")
        );

        repo.remote_set_url("origin", "git@github.com:o/r.git")
            .unwrap();
        let rows = attachment_summaries(&home, &who).unwrap();
        assert_eq!(
            rows[0].remote_url.as_deref(),
            Some("git@github.com:o/r.git")
        );

        repo.remote_set_url("origin", "https://bob:ghp_x@github.com/o/r.git") // trufflehog:ignore
            .unwrap();
        let rows = attachment_summaries(&home, &who).unwrap();
        assert_eq!(
            rows[0].remote_url.as_deref(),
            Some("https://github.com/o/r.git")
        );
        assert!(!format!("{rows:?}").contains("ghp_x"));
    }

    #[test]
    fn attachment_summaries_never_open_a_missing_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("state");
        let root = context_attachments::tests::checkout(tmp.path(), "checkout", "acme/context");
        let who = Principal::local_guest();
        context_attachments::attach(&home, &who, &root).unwrap();
        git2::Repository::open(&root)
            .unwrap()
            .remote("origin", "https://github.com/o/r.git")
            .unwrap();
        std::fs::remove_dir_all(&root).unwrap();

        let rows = attachment_summaries(&home, &who).unwrap();
        assert!(rows[0].availability_error.is_some());
        assert!(!rows[0].has_remote);
        assert_eq!(rows[0].remote_url, None);
    }
}
