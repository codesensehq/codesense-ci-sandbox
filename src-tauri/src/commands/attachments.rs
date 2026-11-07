// SPDX-License-Identifier: MIT
#[cfg(test)]
use crate::context_repo::attachments_io::find_by_repo_path;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use log::{info, warn};
use serde::Serialize;

use crate::content_engine::layers::ResolverContext;
use crate::content_engine::sync::{
    reconcile_reattach, teardown_repo, TeardownReport, WaveCoordinator,
};
use crate::context_repo::{
    apply_registry_edit,
    attachments_io::{read_attachments, resolve_by_repo_path, write_attachments_atomic},
    canonical_remote_key, normalize_source_repo_url, path_to_org_root,
    path_to_user_attachments_file, RegistryEdit,
};
use crate::domain::validation::validate_non_empty_no_control;
use crate::domain::{
    parse_org_yml, Attachment, Attachments, CredentialRef, Principal, RegistryFile, SourceRepoSlug,
};
use crate::mcp::state::ActiveTeamState;

use super::async_helpers::await_blocking;

use super::paths::codesense_home_dir;

#[tauri::command]
pub async fn resolve_source_repo_attachment(
    repo_path: String,
    github_user_id: u64,
) -> Result<Option<Attachment>, String> {
    let home = codesense_home_dir().ok_or("cannot resolve home directory")?;
    let principal = Principal::from(github_user_id);
    let path = path_to_user_attachments_file(&home, &principal);
    let data = read_attachments(&path).map_err(|e| e.to_string())?;

    Ok(resolve_by_repo_path(&data.attachments, &repo_path)?.map(|i| data.attachments[i].clone()))
}

/// Upsert an attachment by `repo_path`.
/// - Insert: backend sets `attached_at` to now, ignores caller value.
/// - Update: preserves existing `attached_at`, regardless of caller value.
/// - `tracking_prompt_dismissed_at` is fully caller-controlled.
///
/// SS-FU-17 AC-8: on **insert** (a fresh attach / re-attach), runs the
/// slug-keyed re-attach reconcile — if a lockfile for this slug lingers at a
/// now-dead path (the repo moved), its materialization baseline is moved
/// forward to the new path so the next wave doesn't rebuild from empty. The
/// reconcile is best-effort: a failure logs and degrades to rebuild-from-
/// empty rather than blocking the attach.
fn provider_for_remote(remote_url: &str) -> Option<&'static str> {
    normalize_source_repo_url(remote_url)
        .ok()
        .and_then(|n| n.normalized_url.split('/').next().map(str::to_owned))
        .map(|host| crate::commands::forge::provider_for_host(&host).unwrap_or("git"))
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SaveAttachmentError {
    Failure { message: String },
}

impl SaveAttachmentError {
    fn failure(message: impl Into<String>) -> Self {
        Self::Failure {
            message: message.into(),
        }
    }
}

/// Inspect an existing checkout without any Git transport or provider access.
pub(crate) fn existing_checkout_attachment(path: &Path) -> Result<Attachment, String> {
    if !path.is_absolute() {
        return Err("Choose an absolute source checkout path".into());
    }
    let root = path
        .canonicalize()
        .map_err(|e| format!("Cannot access source checkout: {e}"))?;
    let repository = git2::Repository::open(&root)
        .map_err(|e| format!("Selected folder is not a Git checkout: {e}"))?;
    if repository
        .workdir()
        .and_then(|p| p.canonicalize().ok())
        .as_ref()
        != Some(&root)
    {
        return Err(
            "Select the source checkout root, not a subdirectory or bare repository".into(),
        );
    }
    let repo_path = root
        .to_str()
        .ok_or("Choose a source checkout with a UTF-8 path")?
        .to_string();
    let remote_url = repository
        .find_remote("origin")
        .ok()
        .and_then(|r| Result::ok(r.url()).map(str::to_string));
    Ok(Attachment {
        repo_path,
        remote_url,
        attached_at: None,
        tracking_prompt_dismissed_at: None,
        credential_ref: None,
        managed: false,
    })
}

#[tauri::command]
pub async fn save_source_repo_attachment(
    app: tauri::AppHandle,
    github_user_id: u64,
    attachment: Attachment,
    coord: tauri::State<'_, Arc<WaveCoordinator>>,
    active_team: tauri::State<'_, ActiveTeamState>,
) -> Result<(), SaveAttachmentError> {
    let principal = Principal::from(github_user_id);
    let home = codesense_home_dir()
        .ok_or_else(|| SaveAttachmentError::failure("cannot resolve home directory"))?;
    save_source_for_profile(
        &app,
        &home,
        &principal,
        attachment.repo_path.clone(),
        Some(attachment),
        coord.inner(),
        active_team.inner(),
    )
    .await
    .map_err(SaveAttachmentError::failure)
}

pub const SOURCE_ATTACHMENTS_CHANGED_EVENT: &str = "source-attachments-changed";

/// One inspected snapshot and one selected-profile write path for desktop and MCP.
/// `metadata: None` means repeat attachment preserves the existing row's metadata.
pub(crate) async fn save_source_for_profile<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    home: &Path,
    principal: &Principal,
    repo_path: String,
    metadata: Option<Attachment>,
    coord: &WaveCoordinator,
    active_team: &ActiveTeamState,
) -> Result<(), String> {
    use tauri::{Emitter, Manager};
    let watcher = app.try_state::<super::watch::WatcherState>();
    let _activation = match watcher.as_ref() {
        Some(watcher) => Some(watcher.lock_team_activation().await),
        None => None,
    };
    if super::local_profile::active(app)?.principal != *principal {
        return Err("Local profile changed; retry under the selected profile".into());
    }
    let mut inspected = await_blocking("inspect source checkout", move || {
        existing_checkout_attachment(Path::new(&repo_path))
    })
    .await?;
    validate_non_empty_no_control(&inspected.repo_path, "repo_path").map_err(|e| e.to_string())?;
    if let Some(remote_url) = inspected.remote_url.as_deref() {
        validate_non_empty_no_control(remote_url, "remote_url").map_err(|e| e.to_string())?;
    }
    let preserve_metadata = metadata.is_none();
    if let Some(metadata) = metadata {
        inspected.tracking_prompt_dismissed_at = metadata.tracking_prompt_dismissed_at;
    }
    let checkout = PathBuf::from(&inspected.repo_path);
    upsert_attachment_and_reconcile_mode(
        home,
        principal.numeric_id()?,
        inspected,
        coord,
        active_team,
        preserve_metadata,
    )
    .await?;
    if let Err(error) = app.emit(
        SOURCE_ATTACHMENTS_CHANGED_EVENT,
        serde_json::json!({"principal": principal.as_str()}),
    ) {
        log::warn!("Source checkout attached; refresh notification failed: {error}");
    }
    // Fill in origin's history once, bounded and in the background; reads never fetch.
    crate::attribution::history::fetch_after_attach(home, &checkout);
    Ok(())
}

/// SW-MC-1: the single upsert + insert-gated reconcile write path, shared by
/// **both** the attach-existing command ([`save_source_repo_attachment`]) and
/// the managed-clone command ([`clone_and_attach_source_repo`]). Factored out
/// of `save_source_repo_attachment` so a managed insert rides the EXACT same
/// `reconcile_reattach`-on-insert flow — one write path, not two, and
/// `managed = clone + the existing attach flow` holds by construction.
///
/// Takes plain `&WaveCoordinator` / `&ActiveTeamState` (NOT `tauri::State`) so
/// both commands — each a thin State-unwrapping shell — can call it. Upsert
/// semantics unchanged: insert sets `attached_at` to now; update preserves it.
/// The re-attach reconcile fires **only on insert** and is best-effort.
pub(crate) async fn upsert_attachment_and_reconcile(
    home: &Path,
    github_user_id: u64,
    attachment: Attachment,
    coord: &WaveCoordinator,
    active_team: &ActiveTeamState,
) -> Result<(), String> {
    upsert_attachment_and_reconcile_mode(
        home,
        github_user_id,
        attachment,
        coord,
        active_team,
        false,
    )
    .await
}

async fn upsert_attachment_and_reconcile_mode(
    home: &Path,
    github_user_id: u64,
    attachment: Attachment,
    coord: &WaveCoordinator,
    active_team: &ActiveTeamState,
    preserve_metadata: bool,
) -> Result<(), String> {
    let upserted =
        upsert_attachment_record(home, github_user_id, attachment, None, preserve_metadata)?;
    reconcile_after_upsert(home, github_user_id, &upserted, coord, active_team).await;
    Ok(())
}

/// What [`upsert_attachment_record`] wrote, for the reconcile that follows it.
#[derive(Debug, Clone)]
pub(crate) struct UpsertedAttachment {
    is_insert: bool,
    repo_path: String,
    remote_url: Option<String>,
}

/// The record half of [`upsert_attachment_and_reconcile`]: upsert by
/// `repo_path`. With `replacing_key`, every other row under that remote key is
/// dropped first, so a repo is bound to exactly one clone. Synchronous file
/// I/O: async callers run it on the blocking pool.
pub(crate) fn upsert_attachment_record(
    home: &Path,
    github_user_id: u64,
    mut attachment: Attachment,
    replacing_key: Option<&str>,
    preserve_metadata: bool,
) -> Result<UpsertedAttachment, String> {
    if let Some(url) = attachment.remote_url.as_deref() {
        let matching = attachment
            .credential_ref
            .iter()
            .filter(|binding| Some(binding.provider.as_str()) == provider_for_remote(url))
            .cloned()
            .collect::<Vec<_>>();
        attachment.credential_ref = match crate::commands::forge::select_provider_account(&matching)
        {
            crate::commands::forge::AccountSelection::One(binding) => Some(binding),
            crate::commands::forge::AccountSelection::None
            | crate::commands::forge::AccountSelection::Ambiguous => None,
        };
    }

    let path = path_to_user_attachments_file(home, &Principal::from(github_user_id));
    let mut data = read_attachments(&path).map_err(|e| e.to_string())?;

    let repo_path = attachment.repo_path.clone();
    let remote_url = attachment.remote_url.clone();

    let existing_idx = resolve_by_repo_path(&data.attachments, &attachment.repo_path)?;
    if let Some(key) = replacing_key {
        let preserved = existing_idx.map(|i| data.attachments[i].repo_path.clone());
        data.attachments.retain(|row| {
            preserved.as_ref() == Some(&row.repo_path)
                || attachment_key(row).as_deref() != Some(key)
        });
    }
    let existing_idx = resolve_by_repo_path(&data.attachments, &attachment.repo_path)?;
    let is_insert = existing_idx.is_none();
    match existing_idx {
        Some(idx) => {
            // Update: preserve existing attached_at
            let existing = &data.attachments[idx];
            attachment.attached_at = existing.attached_at;
            attachment.managed = existing.managed;
            if preserve_metadata {
                attachment.credential_ref = existing.credential_ref.clone();
                attachment.tracking_prompt_dismissed_at = existing.tracking_prompt_dismissed_at;
            }
            data.attachments[idx] = attachment;
        }
        None => {
            // Insert: backend owns attached_at
            let mut new = attachment;
            new.attached_at = Some(chrono::Utc::now());
            data.attachments.push(new);
        }
    }

    write_attachments_atomic(&path, &data).map_err(|e| e.to_string())?;
    crate::attribution::watcher::refresh(home);
    Ok(UpsertedAttachment {
        is_insert,
        repo_path,
        remote_url,
    })
}

/// The reconcile half of [`upsert_attachment_and_reconcile`].
pub(crate) async fn reconcile_after_upsert(
    home: &Path,
    github_user_id: u64,
    upserted: &UpsertedAttachment,
    coord: &WaveCoordinator,
    active_team: &ActiveTeamState,
) {
    let repo_path = &upserted.repo_path;
    // AC-8: re-attach reconcile, only on a fresh attach. The record is
    // already durably written; reconcile is best-effort on top — a
    // failure logs and degrades to rebuild-from-empty, never failing the
    // attach. It is awaited here (so it lands before the attach-triggered
    // wave) and briefly holds the new path's repo lock, so the command's
    // return can wait on an in-flight wave for the same path.
    if upserted.is_insert {
        match resolve_active_ctx_and_slug(
            active_team,
            home,
            github_user_id,
            upserted.remote_url.as_deref(),
        ) {
            Ok(Some((ctx, slug))) => {
                match reconcile_reattach(coord, &ctx, &slug, Path::new(repo_path)).await {
                    Ok(Some(superseded)) => info!(
                        target: "attachments",
                        "re-attach {repo_path}: superseded stale lockfile {}",
                        superseded.display()
                    ),
                    Ok(None) => {}
                    Err(e) => warn!(
                        target: "attachments",
                        "re-attach {repo_path}: reconcile failed (non-fatal, will rebuild from empty): {e}"
                    ),
                }
            }
            Ok(None) => {}
            Err(e) => warn!(
                target: "attachments",
                "re-attach {repo_path}: could not resolve context for reconcile (non-fatal): {e}"
            ),
        }
    }
}

/// Resolve the active `(ResolverContext, SourceRepoSlug)` for an
/// attachment's `remote_url`. Returns `Ok(None)` when there's no active team
/// or the remote doesn't yield a slug — callers treat that as "nothing
/// slug-keyed to act on." The slug is derived the same way the materialize
/// wave derives it (`normalize_source_repo_url → SourceRepoSlug`), so
/// it matches the key the lockfile was written under.
fn resolve_active_ctx_and_slug(
    active_team: &ActiveTeamState,
    home: &Path,
    github_user_id: u64,
    remote_url: Option<&str>,
) -> Result<Option<(ResolverContext, SourceRepoSlug)>, String> {
    let Some(remote_url) = remote_url else {
        return Ok(None);
    };
    let Some(slug) = normalize_source_repo_url(remote_url).ok().and_then(|n| {
        let host = n.normalized_url.split('/').next()?;
        let provider = crate::commands::forge::provider_for_host(host).unwrap_or("git");
        SourceRepoSlug::with_provider_and_host(n.owner, n.repo, provider, Some(host)).ok()
    }) else {
        return Ok(None);
    };
    let Some(active) = active_team.get() else {
        return Ok(None);
    };
    let ctx = super::content_engine::derive_resolver_context(
        home.to_path_buf(),
        github_user_id,
        active.org_slug,
        Some(active.team_name),
    )
    .map_err(|e| format!("derive_resolver_context failed: {e}"))?;
    Ok(Some((ctx, slug)))
}

/// Load the user's attachments and return a clone of the record for
/// `repo_path`, or a `not_found` error. Shared read-half of the detach
/// / managed-remove paths ([`delete_source_repo_attachment`],
/// [`remove_managed_clone`]).
fn resolve_attachment_record(
    home: &Path,
    github_user_id: u64,
    repo_path: &str,
) -> Result<Attachment, String> {
    let path = path_to_user_attachments_file(home, &Principal::from(github_user_id));
    let data = read_attachments(&path).map_err(|e| e.to_string())?;
    let idx = resolve_by_repo_path(&data.attachments, repo_path)?
        .ok_or_else(|| format!("not_found: no attachment with repo_path '{repo_path}'"))?;
    Ok(data.attachments[idx].clone())
}

/// Remove the record for `repo_path` from the user's attachments and
/// persist atomically; returns the removed [`Attachment`]. Shared
/// write-half of the detach / managed-remove paths — the single place
/// that mutates `attachments.yml` for a forget.
pub(crate) fn forget_attachment_record(
    home: &Path,
    github_user_id: u64,
    repo_path: &str,
) -> Result<Attachment, String> {
    let path = path_to_user_attachments_file(home, &Principal::from(github_user_id));
    let mut data = read_attachments(&path).map_err(|e| e.to_string())?;
    let idx = resolve_by_repo_path(&data.attachments, repo_path)?
        .ok_or_else(|| format!("not_found: no attachment with repo_path '{repo_path}'"))?;
    let removed = data.attachments.remove(idx);
    write_attachments_atomic(&path, &data).map_err(|e| e.to_string())?;
    crate::attribution::watcher::refresh(home);
    Ok(removed)
}

/// W-6 + SS-FU-17 AC-2: Remove an attachment by exact `repo_path` match,
/// tearing down the repo's materialized install FIRST.
///
/// Detach is the single user-facing uninstall verb. Teardown is
/// hash-guarded (local edits are never destroyed — they are reported in
/// [`TeardownReport::skipped_locally_modified`]) and runs under the wave's
/// per-repo lock on the blocking pool, so it neither races an in-flight
/// materialize wave nor stalls the UI.
///
/// **Failure coupling:** the attachment record is removed only AFTER
/// teardown returns `Ok`. A hard teardown failure (an unreadable or
/// un-deletable lockfile) propagates as `Err` via `?`, leaving the record
/// in place so the user can retry rather than orphaning the install behind
/// a forgotten record. Per-entry soft failures and skipped local edits
/// ride along in the returned report; the record is still removed.
///
/// Returns an error if no attachment exists for this path.
#[tauri::command]
pub async fn delete_source_repo_attachment(
    github_user_id: u64,
    repo_path: String,
    coord: tauri::State<'_, Arc<WaveCoordinator>>,
    active_team: tauri::State<'_, ActiveTeamState>,
) -> Result<TeardownReport, String> {
    validate_non_empty_no_control(&repo_path, "repo_path").map_err(|e| e.to_string())?;

    let home = codesense_home_dir().ok_or("cannot resolve home directory")?;
    let attachment = resolve_attachment_record(&home, github_user_id, &repo_path)?;

    // Teardown BEFORE the record is removed (AC-2). A hard failure here
    // short-circuits with `?` — the record stays, the user retries.
    let report = teardown_for_detach(
        coord.inner(),
        active_team.inner(),
        &home,
        github_user_id,
        &repo_path,
        &attachment,
    )
    .await?;

    forget_attachment_record(&home, github_user_id, &repo_path)?;
    Ok(report)
}

/// SW-OC-3: Detach a source repo from a team — the inverse of the attach flow's
/// `AddSourceRepoToTeam` registry edit. Applies the **existing**
/// `RemoveSourceRepoFromTeam` registry edit (drops the repo from the team's
/// `source_repos` in `teams.yml`, a local working-tree edit — no commit / PR).
fn untrack_source_repo(
    home: &Path,
    github_user_id: u64,
    org_slug: &str,
    team_name: &str,
    slug: &SourceRepoSlug,
) -> Result<RegistryEdit, String> {
    crate::context_repo::org_slug::require_valid_org_slug(org_slug)?;
    let principal = Principal::from(github_user_id);
    let org_root = path_to_org_root(home, &principal, org_slug)?;
    if !org_root.exists() {
        return Err(format!(
            "org root missing for {}: {}",
            org_slug,
            org_root.display(),
        ));
    }
    let edit = RegistryEdit::RemoveSourceRepoFromTeam {
        team_name: team_name.to_string(),
        owner: slug.owner.clone(),
        repo: slug.repo.clone(),
        provider: Some(slug.provider.clone()),
        host: slug.host.clone(),
    };
    // One branch-lock hold over the read-modify-write, like every registry edit.
    crate::context_repo::with_branch_lock(|| {
        let applied = apply_registry_edit(&edit, &org_root).map_err(|e| e.to_string())?;
        crate::context_repo::write_edit_to_workdir_locked(&org_root, &applied)
            .map_err(|e| e.to_string())
    })?;
    Ok(edit)
}

/// Whether `org_root`'s `teams.yml` has any team tracking `remote_key`. An
/// unreadable registry counts as tracking it, so a binding is never dropped on
/// a guess.
fn org_tracks(org_root: &Path, remote_key: &str) -> bool {
    let Ok(bytes) = std::fs::read(org_root.join("teams.yml")) else {
        return true;
    };
    let Ok(raw) = crate::domain::parse_teams_yml(&bytes) else {
        return true;
    };
    raw.teams
        .iter()
        .flat_map(|team| &team.source_repos)
        .any(|r| {
            SourceRepoSlug::with_provider_and_host(
                &r.owner,
                &r.repo,
                r.provider_id(),
                r.host.as_deref(),
            )
            .map_or(true, |tracked| tracked.remote_key() == remote_key)
        })
}

/// Forget the user's binding for `slug` — the record only, never the clone — unless
/// a team in any local org still tracks it. The record is per user, not per
/// team, so another team's view of the clone must survive this team dropping it.
/// Returns the path of the clone that was bound, if a binding was forgotten.
fn forget_binding_unless_tracked(
    home: &Path,
    github_user_id: u64,
    org_slug: &str,
    slug: &SourceRepoSlug,
) -> Result<Option<String>, String> {
    let principal = Principal::from(github_user_id);
    let key = slug.remote_key();
    let mut org_roots = vec![path_to_org_root(home, &principal, org_slug)?];
    let orgs = crate::commands::org::discover_local_orgs_impl(home, &principal)?;
    if orgs.iter().any(|org| org.availability_error.is_some()) {
        return Ok(None); // Unknown references cannot justify forgetting a binding.
    }
    org_roots.extend(orgs.into_iter().map(|o| PathBuf::from(o.org_root)));
    if org_roots.iter().any(|root| org_tracks(root, &key)) {
        return Ok(None);
    }
    let attachments_path = path_to_user_attachments_file(home, &principal);
    let data = read_attachments(&attachments_path).map_err(|e| e.to_string())?;
    let Some(record) = group_attachments_by_remote(&data).remove(&key) else {
        return Ok(None);
    };
    forget_attachment_record(home, github_user_id, &record.repo_path)?;
    Ok(Some(record.repo_path))
}

/// Stop `team_name` tracking `slug` and forget the user's binding when no team
/// still tracks it (the clone stays on disk), then tell every view. Once the
/// registry write lands, both change events fire even if forgetting the binding
/// fails, so no view is left stale. Returns the forgotten clone's path. Shared
/// by the Manage repos "Remove" action and the MCP tool; synchronous file I/O.
pub(crate) fn remove_source_repo_and_notify<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    home: &Path,
    github_user_id: u64,
    org_slug: &str,
    team_name: &str,
    slug: &SourceRepoSlug,
) -> Result<Option<String>, String> {
    let edit = untrack_source_repo(home, github_user_id, org_slug, team_name, slug)?;
    let forgotten = forget_binding_unless_tracked(home, github_user_id, org_slug, slug);
    crate::commands::registry_local::emit_org_registry_changed(
        app,
        &Principal::from(github_user_id),
        org_slug,
        &edit,
    );
    super::source_repo_add::emit_source_repo_attachments_changed(
        app,
        &Principal::from(github_user_id),
    );
    forgotten
}

/// SW-OC-3: thin command wrapper for [`remove_source_repo_and_notify`] — detach
/// a source repo from a team, preserving the local clone. Powers the "Remove"
/// action in the Manage repos surface. Without a `provider`, the repo is the
/// team's tracked `owner/repo` (any forge).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn remove_source_repo_from_team(
    app: tauri::AppHandle,
    github_user_id: u64,
    org_slug: String,
    team_name: String,
    owner: String,
    repo: String,
    provider: Option<String>,
    host: Option<String>,
) -> Result<(), String> {
    validate_non_empty_no_control(&team_name, "team_name").map_err(|e| e.to_string())?;
    let home = codesense_home_dir().ok_or("cannot resolve home directory")?;
    use tauri::Manager;
    let watcher = app.state::<super::watch::WatcherState>();
    let _activation = watcher.lock_team_activation().await;
    app.state::<super::local_profile::LocalProfileState>()
        .check_expected(Some(&github_user_id.to_string()))?;
    let handle = app.clone();
    await_blocking("remove_source_repo_from_team", move || {
        let slug = match provider {
            Some(provider) => SourceRepoSlug::with_provider_and_host(owner, repo, provider, host)
                .map_err(|e| e.to_string())?,
            None => {
                let tracked = super::source_repo_add::team_source_repos(
                    &home,
                    &Principal::from(github_user_id),
                    &org_slug,
                    &team_name,
                )
                .map_err(|e| e.to_string())?;
                super::source_repo_add::resolve_tracked_repo(
                    &tracked,
                    &format!("{owner}/{repo}"),
                    &team_name,
                )
                .map_err(|e| e.to_string())?
            }
        };
        remove_source_repo_and_notify(&handle, &home, github_user_id, &org_slug, &team_name, &slug)
            .map(|_| ())
    })
    .await
}

/// Add a repo to a team from a URL or a local folder — the add-repo form's one
/// call. See [`super::source_repo_add::add_source_repo_to_team`].
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn add_source_repo_to_team(
    app: tauri::AppHandle,
    coord: tauri::State<'_, Arc<WaveCoordinator>>,
    active_team: tauri::State<'_, ActiveTeamState>,
    github_user_id: u64,
    org_slug: String,
    team_name: String,
    input: super::source_repo_add::AddRepoInput,
) -> Result<super::source_repo_add::TrackedSourceRepo, super::source_repo_add::AddRepoFailure> {
    let home =
        codesense_home_dir().ok_or_else(|| super::source_repo_add::AddRepoError::Failure {
            message: "cannot resolve home directory".to_string(),
        })?;
    let env = super::source_repo_add::AttachmentEnv {
        app: &app,
        coord: coord.inner(),
        active_team: active_team.inner(),
        home: &home,
        user_id: github_user_id,
    };
    super::source_repo_add::add_source_repo_to_team(&env, &org_slug, &team_name, input)
        .await
        .map_err(Into::into)
}

/// Bind a local clone to a tracked repo — the attach dialog's one call. The
/// folder must be a git working tree whose `origin` is that repo. See
/// [`super::source_repo_add::attach_source_repo_clone`].
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn attach_source_repo_clone(
    app: tauri::AppHandle,
    coord: tauri::State<'_, Arc<WaveCoordinator>>,
    active_team: tauri::State<'_, ActiveTeamState>,
    github_user_id: u64,
    repo: SourceRepoSlug,
    path: String,
) -> Result<super::source_repo_add::TrackedSourceRepo, super::source_repo_add::AddRepoFailure> {
    use super::source_repo_add::AddRepoError;
    let slug =
        SourceRepoSlug::with_provider_and_host(repo.owner, repo.repo, repo.provider, repo.host)
            .map_err(|e| AddRepoError::InvalidInput {
                reason: e.to_string(),
            })?;
    let home = codesense_home_dir().ok_or_else(|| AddRepoError::Failure {
        message: "cannot resolve home directory".to_string(),
    })?;
    let env = super::source_repo_add::AttachmentEnv {
        app: &app,
        coord: coord.inner(),
        active_team: active_team.inner(),
        home: &home,
        user_id: github_user_id,
    };
    super::source_repo_add::attach_source_repo_clone(&env, &slug, &path)
        .await
        .map_err(Into::into)
}

/// Resolve `(ctx, slug)` for `attachment` and run [`teardown_repo`] for the
/// active `(org, team)` context — where this repo's files are materialized.
///
/// Teardown is **scoped to the active team**: at any moment only the active
/// team's content is materialized on disk (team-switch unmaterializes the
/// previous team's files), so the active context is the one with files to
/// remove. When the slug or active team cannot be resolved — an attachment
/// with no parseable `remote_url`, or no team selected yet — there is no
/// slug-keyed lockfile to target, so teardown is skipped with a log line
/// and the detach proceeds with an empty report rather than blocking the
/// user from removing a record we cannot act on.
async fn teardown_for_detach(
    coord: &Arc<WaveCoordinator>,
    active_team: &ActiveTeamState,
    home: &Path,
    github_user_id: u64,
    repo_path: &str,
    attachment: &Attachment,
) -> Result<TeardownReport, String> {
    let Some((ctx, _slug)) = resolve_active_ctx_and_slug(
        active_team,
        home,
        github_user_id,
        attachment.remote_url.as_deref(),
    )?
    else {
        info!(
            target: "attachments",
            "detach {repo_path}: no active team or unresolvable slug; skipping teardown (nothing slug-keyed to remove)"
        );
        return Ok(TeardownReport::default());
    };

    teardown_repo(coord, &ctx, Path::new(repo_path))
        .await
        .map_err(|e| format!("teardown failed (attachment kept; retry): {e}"))
}

/// F-6: Group attachments into a lookup keyed by lowercase `"owner/repo"`.
///
/// Used by `buildSyntheticRepo` (via the `list_attachments_by_remote` Tauri
/// command) to resolve the real on-disk source-code path for a synthetic
/// workspace repo. URL normalization is delegated to
/// `normalize_source_repo_url`; rows without a `remote_url` or whose URL
/// fails to parse are silently skipped.
///
/// **Tie-breaker** for two rows that produce the same key: the row with the
/// more-recent `attached_at` wins. On `None` / equal timestamps, the first
/// row in file order wins (`Entry::Vacant` inserts, `Entry::Occupied` only
/// replaces when strictly newer). Duplicates emit an `info` log.
///
/// `pub(crate)` (SW-OB-11): the MCP `list_source_repos` handler reuses this
/// exact grouping (lowercase key + newest-wins) to join configured repos to
/// their bound account — no parallel implementation.
pub(crate) fn group_attachments_by_remote(data: &Attachments) -> HashMap<String, Attachment> {
    let mut by_remote: HashMap<String, Attachment> = HashMap::new();

    for row in &data.attachments {
        // SW-FG-3: the ONE shared canonicalization (also keys the backup-remote
        // binding map) so a URL never keys two ways between the two stores.
        // A remote-less row is a local repo, keyed the way the attach flow
        // names it (`local/<folder>` on the `local` host).
        let Some(key) = attachment_key(row) else {
            continue;
        };

        // P6 patch: avoid cloning `key` on the vacant (common) path.
        // `Entry::key()` lets us reference the key inside the Occupied
        // branch's log without holding a clone.
        match by_remote.entry(key) {
            Entry::Vacant(e) => {
                e.insert(row.clone());
            }
            Entry::Occupied(mut e) => {
                let existing_ts = e.get().attached_at;
                let new_ts = row.attached_at;
                // None < Some(_); equal timestamps keep existing (first wins).
                let should_replace = match (new_ts, existing_ts) {
                    (Some(new), Some(old)) => new > old,
                    (Some(_), None) => true,
                    (None, _) => false,
                };
                if should_replace {
                    // `e.get()` borrows immutably; we clone the existing
                    // repo_path so the log can fire before `e.insert()`'s
                    // &mut borrow conflicts.
                    let existing_path = e.get().repo_path.clone();
                    info!(
                        target: "attachments",
                        "duplicate remote for key '{}' — newer attached_at wins (existing repo_path: {}, replacing with: {})",
                        e.key(), existing_path, row.repo_path
                    );
                    e.insert(row.clone());
                } else {
                    info!(
                        target: "attachments",
                        "duplicate remote for key '{}' — keeping existing (existing repo_path: {}, ignored: {})",
                        e.key(), e.get().repo_path, row.repo_path
                    );
                }
            }
        }
    }

    by_remote
}

/// The key a record is looked up by: its remote's canonical `host/owner/repo`,
/// or `local/local/<folder>` for a remote-less record.
pub(crate) fn attachment_key(row: &Attachment) -> Option<String> {
    match &row.remote_url {
        Some(remote_url) => canonical_remote_key(remote_url),
        None => local_attachment_key(&row.repo_path),
    }
}

/// The lookup key for a local repo with no remote: `local/local/<folder>`,
/// matching the `{owner: "local", repo: <folder>, host: "local"}` slug the
/// attach-a-local-folder flow (and the bundled sample repos) track on a team.
fn local_attachment_key(repo_path: &str) -> Option<String> {
    let folder = Path::new(repo_path).file_name()?.to_str()?;
    Some(format!("local/local/{}", folder.to_ascii_lowercase()))
}

/// SW-MC-1 AC-5: XOR guard — is `owner/repo` already present as an attachment?
///
/// Keyed by **origin, not path.** A managed clone's target dir does not exist
/// at check time, so `find_by_repo_path` cannot catch a same-repo duplicate the
/// user attached at a *different* path. Reuses [`group_attachments_by_remote`]
/// — which derives the `owner/repo` key from the stored `remote_url` string (no
/// `Repository::open`, no disk I/O) — so a repo already attached anywhere (any
/// path, any casing) is detected. Remote-less records key under the `local`
/// host, so they never collide with a forge `owner/repo`. When `true`, the managed-clone command refuses (attach XOR
/// managed — one object store per repo).
#[cfg(test)]
pub(crate) fn is_repo_already_attached(data: &Attachments, owner: &str, repo: &str) -> bool {
    let key = SourceRepoSlug::new(owner, repo)
        .expect("owner/repo validated by command boundary")
        .remote_key();
    group_attachments_by_remote(data).contains_key(&key)
}

pub(crate) fn credential_ref_for_source_repo_for_principal(
    principal: &Principal,
    slug: &SourceRepoSlug,
) -> Result<Option<CredentialRef>, String> {
    let home = codesense_home_dir().ok_or_else(|| "codesense home unavailable".to_string())?;
    let path = path_to_user_attachments_file(&home, principal);
    let data = read_attachments(&path).map_err(|error| error.to_string())?;
    Ok(credential_ref_for_source_repo_in(&data, slug))
}

/// Resolve a credential binding using the complete source-repository identity.
pub(crate) fn credential_ref_for_source_repo_in(
    data: &Attachments,
    slug: &SourceRepoSlug,
) -> Option<CredentialRef> {
    group_attachments_by_remote(data)
        .get(&slug.remote_key())
        .and_then(|a| a.credential_ref.clone())
}

/// The local clone bound to `slug` in the user's attachments, if its folder
/// still exists. Reads `attachments.yml` (the registry the attach tools write),
/// so an in-place checkout outside the codesense home resolves without an
/// install-store row — the install store is only written for managed clones
/// and synthetic targets (`register_attached_code_repo`), never for a user's
/// primary checkout (the managed-surface gate).
///
/// A missing file returns `Ok(None)` (`read_attachments` already treats absent
/// as empty). A stale row whose `repo_path` no longer exists on disk also
/// returns `Ok(None)` — with a `warn!` so an unexpected `no local clone`
/// refusal downstream is diagnosable — so the caller's fallback (install
/// store → typed refusal) runs. A malformed `attachments.yml` is `Err` and
/// the composite resolver treats it as a soft miss on the attachments branch
/// (logs and falls through to the store) rather than failing the whole
/// resolve.
pub(crate) fn attached_clone_path_for_slug(
    home: &Path,
    principal: &Principal,
    slug: &SourceRepoSlug,
) -> Result<Option<PathBuf>, String> {
    let path = path_to_user_attachments_file(home, principal);
    let data = read_attachments(&path).map_err(|e| e.to_string())?;
    let key = slug.remote_key();
    let Some(row) = group_attachments_by_remote(&data).get(&key).cloned() else {
        return Ok(None);
    };
    let repo_path = PathBuf::from(&row.repo_path);
    if !repo_path.is_dir() {
        warn!(
            target: "attachments",
            "attached clone for '{key}' is stale (repo_path '{}' is not a directory) — falling through to install store",
            row.repo_path
        );
        return Ok(None);
    }
    Ok(Some(repo_path))
}

/// F-6: Load all attachments for `github_user_id` and return a lookup keyed
/// by canonical lowercase `"host/owner/repo"`. Returns an empty map if `attachments.yml`
/// does not exist (consistent with `read_attachments` semantics).
#[tauri::command]
pub async fn list_attachments_by_remote(
    github_user_id: u64,
) -> Result<HashMap<String, Attachment>, String> {
    let home = codesense_home_dir().ok_or("cannot resolve home directory")?;
    let principal = Principal::from(github_user_id);
    let path = path_to_user_attachments_file(&home, &principal);
    let data = read_attachments(&path).map_err(|e| e.to_string())?;
    Ok(group_attachments_by_remote(&data))
}

/// SW-MC-2: expand a stored `workspace_root_dir` to a machine-local absolute
/// path. The value is SHARED via `org.yml` (per-org, git-synced), so it is
/// stored in a PORTABLE form: a leading `~` (its own path component) expands to
/// THIS machine's home dir — so a synced `~/code` lands each teammate's clones
/// under their own home — while an already-absolute value (e.g. a shared
/// `/mnt/team/code` mount) passes through unchanged. Returns `None` if the value
/// needs `~`-expansion but the home dir can't be resolved (best-effort: fall
/// back to the hidden default rather than build a literal `~` directory).
fn expand_workspace_root(raw: &Path) -> Option<PathBuf> {
    let mut components = raw.components();
    match components.next() {
        // A leading `~` component → replace it with the home dir; the remaining
        // components (empty for a bare `~`) join onto it.
        Some(std::path::Component::Normal(first)) if first == "~" => {
            Some(dirs::home_dir()?.join(components.as_path()))
        }
        _ => Some(raw.to_path_buf()),
    }
}

/// Read `config_flags.workspace_root_dir` from an org root's `org.yml`, expanded
/// to a machine-local absolute path (see [`expand_workspace_root`]). Best-effort
/// (missing / unreadable / malformed / unexpandable → `None`). Shared by the
/// single-org clone read and the all-orgs lifecycle sweep.
///
/// The value arrives from teammates over sync and could be hand-edited, so the
/// contract here is strict: `Some` ONLY for a usable **absolute** root. A value
/// that expands to a non-absolute path (empty, bare-relative, or a `~` with no
/// resolvable home) falls back to `None` (the hidden default) rather than
/// producing a clone target resolved against the process CWD.
fn read_workspace_root_from_org_root(org_root: &Path) -> Option<PathBuf> {
    let bytes = std::fs::read(org_root.join(RegistryFile::OrgYml.filename())).ok()?;
    let raw = parse_org_yml(&bytes)
        .ok()?
        .config_flags
        .workspace_root_dir?;
    let expanded = expand_workspace_root(&raw)?;
    expanded.is_absolute().then_some(expanded)
}

/// SW-MC-2: read this org's configured `workspace_root_dir` (if any) via a
/// lightweight single-file `org.yml` parse — NOT the 4-file [`load_org`]. A
/// missing / unreadable / malformed `org.yml`, or an org with no active team,
/// falls through to `None` (the hidden default); a config read must never
/// hard-fail a clone. One small synchronous read, consistent with the inline
/// `read_attachments` above — no blocking-pool hop needed.
pub(crate) fn read_org_workspace_root_dir(
    home: &Path,
    principal: &Principal,
    org_slug: &str,
) -> Option<PathBuf> {
    read_workspace_root_from_org_root(&path_to_org_root(home, principal, org_slug).ok()?)
}

/// SW-MC-2 (Option-C, lifecycle sites): every configured `workspace_root_dir`
/// across the user's orgs — the visible managed-clone roots that live OUTSIDE
/// `{home}/users/` and so are invisible to `is_codesense_managed_target`. Reads
/// each org's `org.yml` once; an empty vec means no org configured a root (pre-
/// MC-2 behavior). FS I/O (org walk + reads) → callers run it on the blocking pool.
fn all_configured_workspace_roots(home: &Path) -> Vec<PathBuf> {
    crate::context_repo::org_enumeration::enumerate_all_org_roots(home)
        .iter()
        .filter_map(|org_root| read_workspace_root_from_org_root(org_root))
        .collect()
}

/// SW-MC-2 (Option-C): is `path` a recognized codesense-managed clone surface,
/// INCLUDING a clone at one of the org-configured workspace roots? The lifecycle
/// guards (remove, health) OR the configured-root branch onto the pure predicate
/// so a managed clone at `{configured_root}/{owner}/{repo}` (outside `users/`) is
/// not wrongly refused on Remove / flagged "not syncing". The materialize hot path
/// stays on the pure predicate + the `managed` marker; this branch lives only at
/// the two per-action lifecycle sites, so it never adds config I/O to a hot path.
///
/// The configured-root branch reuses [`path_within_any_root`] — the SAME hardened
/// containment check `is_codesense_managed_target` uses for its `users/` branch —
/// NOT a raw `starts_with`. That normalizes `..`/`.` on both sides and rejects a
/// degenerate root (empty, non-absolute, bare `/`), so a corrupt/hand-edited
/// record path can't escape and a shallow root can't degrade the guard into
/// "allow any path" (which would let Remove `remove_dir_all` user territory).
///
/// [`path_within_any_root`]: codesense_fs_core::gate::path_within_any_root
fn is_recognized_managed_surface(path: &Path, home: &Path, configured_roots: &[PathBuf]) -> bool {
    crate::install::worktree_index::is_codesense_managed_target(path, home)
        || codesense_fs_core::gate::path_within_any_root(path, configured_roots)
}

/// SW-MC-3 AC-7: per-row health of a managed clone — whether codesense
/// still recognizes its directory as a materialize surface.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ManagedCloneHealth {
    pub repo_path: String,
    /// `false` ⇒ the clone's path is no longer a recognized managed
    /// surface (e.g. hand-moved outside a managed location) → the wave
    /// silently no-ops materialize; the FE shows the amber "not syncing"
    /// chip (MC-6) with Re-check / escalation (AC-9).
    pub recognized: bool,
}

/// AC-7 detection signal, batched: for every **managed** row, is its
/// directory still a recognized materialize surface? Uses the SAME
/// predicate the wave uses to decide whether to materialize
/// (`is_codesense_managed_target`), so the chip reflects real behavior.
/// Attached rows are excluded — health is only about clones codesense
/// materializes into.
///
/// SW-MC-2 (Option-C, mirrors the remove guard): a managed clone is `recognized`
/// if its path is under `{home}/users/` / a registered worktree OR under one of
/// the org-configured workspace roots — so a legit configured-root clone reports
/// `recognized: true`, while a genuinely hand-moved clone (outside every managed
/// root) still reports `false` (the intended AC-7 "not syncing" signal). The
/// configured-root set is read ONCE for the whole batch (this runs on the blocking
/// pool via `list_managed_clone_health`).
fn managed_clone_health(data: &Attachments, home: &Path) -> Vec<ManagedCloneHealth> {
    let configured_roots = all_configured_workspace_roots(home);
    data.attachments
        .iter()
        .filter(|a| a.managed)
        .map(|a| ManagedCloneHealth {
            repo_path: a.repo_path.clone(),
            recognized: is_recognized_managed_surface(
                Path::new(&a.repo_path),
                home,
                &configured_roots,
            ),
        })
        .collect()
}

/// SW-MC-3 AC-7: compute the unhealthy-detection signal for all of the
/// user's managed clones in ONE call — invoked when the source surface
/// mounts and again on Re-check, so MC-6 renders health as a passed-in
/// prop and never fans out a per-row IPC probe (AC-10).
///
/// **AC-10 non-blocking:** the attachments read + predicate (which may
/// pay a worktree-index scan for a path outside `users/`) run on the
/// blocking pool.
#[tauri::command]
pub async fn list_managed_clone_health(
    github_user_id: u64,
) -> Result<Vec<ManagedCloneHealth>, String> {
    let home = codesense_home_dir().ok_or("cannot resolve home directory")?;
    await_blocking("list_managed_clone_health", move || {
        let att_path = path_to_user_attachments_file(&home, &Principal::from(github_user_id));
        let data = read_attachments(&att_path).map_err(|e| e.to_string())?;
        Ok(managed_clone_health(&data, &home))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test-only: the default-path helper is used by the SW-MC-3 lifecycle tests
    // below; prod code resolves via `resolve_managed_clone_dir`, so it isn't in
    // the module-level import.
    use crate::context_repo::path_to_managed_source_clone;
    use tempfile::tempdir;

    fn make_attachment(repo_path: &str) -> Attachment {
        Attachment {
            repo_path: repo_path.into(),
            remote_url: None,
            attached_at: None,
            tracking_prompt_dismissed_at: None,
            credential_ref: None,
            managed: false,
        }
    }

    fn write_org_yml(home: &Path, uid: u64, org_slug: &str, body: &str) {
        let org_root = path_to_org_root(home, &Principal::from(uid), org_slug).unwrap();
        std::fs::create_dir_all(&org_root).unwrap();
        std::fs::write(org_root.join("org.yml"), body).unwrap();
    }

    // ---------- SW-MC-2: org.yml workspace_root_dir read ----------

    #[test]
    fn read_org_workspace_root_dir_reads_configured_value() {
        let tmp = tempdir().unwrap();
        write_org_yml(
            tmp.path(),
            42,
            "acme",
            "slug: acme\ndisplay_name: Acme\ncreated_at: \"2026-01-01T00:00:00Z\"\nconfig_flags:\n  workspace_root_dir: /Users/dev/mycode\n",
        );
        assert_eq!(
            read_org_workspace_root_dir(tmp.path(), &Principal::from(42u64), "acme"),
            Some(PathBuf::from("/Users/dev/mycode")),
        );
    }

    #[test]
    fn read_org_workspace_root_dir_missing_file_is_none() {
        let tmp = tempdir().unwrap();
        assert_eq!(
            read_org_workspace_root_dir(tmp.path(), &Principal::from(42u64), "acme"),
            None
        );
    }

    #[test]
    fn read_org_workspace_root_dir_unset_is_none() {
        let tmp = tempdir().unwrap();
        write_org_yml(
            tmp.path(),
            42,
            "acme",
            "slug: acme\ndisplay_name: Acme\ncreated_at: \"2026-01-01T00:00:00Z\"\n",
        );
        assert_eq!(
            read_org_workspace_root_dir(tmp.path(), &Principal::from(42u64), "acme"),
            None
        );
    }

    #[test]
    fn read_org_workspace_root_dir_malformed_is_none() {
        // A malformed org.yml must never hard-fail the clone — fall through to
        // the hidden default (None).
        let tmp = tempdir().unwrap();
        write_org_yml(tmp.path(), 42, "acme", ":::garbage:::");
        assert_eq!(
            read_org_workspace_root_dir(tmp.path(), &Principal::from(42u64), "acme"),
            None
        );
    }

    #[test]
    fn expand_workspace_root_expands_leading_tilde() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(
            expand_workspace_root(Path::new("~/code/myrepos")),
            Some(home.join("code/myrepos")),
        );
        // Bare `~` → home.
        assert_eq!(expand_workspace_root(Path::new("~")), Some(home.clone()));
        // Absolute passes through unchanged (shared-mount case).
        assert_eq!(
            expand_workspace_root(Path::new("/mnt/team/code")),
            Some(PathBuf::from("/mnt/team/code")),
        );
    }

    #[test]
    fn read_org_workspace_root_dir_expands_tilde_to_home() {
        // The SHARED (synced) value is stored portable as `~/mycode`; the read
        // side expands it to THIS machine's home so a teammate's clones land
        // under their own home, not the author's absolute path.
        let tmp = tempdir().unwrap();
        write_org_yml(
            tmp.path(),
            42,
            "acme",
            "slug: acme\ndisplay_name: Acme\ncreated_at: \"2026-01-01T00:00:00Z\"\nconfig_flags:\n  workspace_root_dir: ~/mycode\n",
        );
        assert_eq!(
            read_org_workspace_root_dir(tmp.path(), &Principal::from(42u64), "acme"),
            Some(dirs::home_dir().unwrap().join("mycode")),
        );
    }

    #[test]
    fn read_org_workspace_root_dir_ignores_non_absolute_synced_value() {
        // A hand-edited / synced org.yml value that doesn't resolve to an absolute
        // path (empty or bare-relative) must fall back to None — never a
        // CWD-relative clone target.
        let base = "slug: acme\ndisplay_name: Acme\ncreated_at: \"2026-01-01T00:00:00Z\"\nconfig_flags:\n  workspace_root_dir: ";
        for bad in ["\"\"", "code/myrepos"] {
            let tmp = tempdir().unwrap();
            write_org_yml(tmp.path(), 42, "acme", &format!("{base}{bad}\n"));
            assert_eq!(
                read_org_workspace_root_dir(tmp.path(), &Principal::from(42u64), "acme"),
                None,
                "non-absolute `{bad}` must be ignored"
            );
        }
    }

    #[test]
    fn resolve_path_exact_match() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("attachments.yml");

        let data = crate::domain::Attachments {
            attachments: vec![
                make_attachment("/Users/dev/repo-a"),
                make_attachment("/Users/dev/repo-b"),
            ],
        };
        crate::context_repo::attachments_io::write_attachments_atomic(&path, &data).unwrap();

        let read = read_attachments(&path).unwrap();
        let found = find_by_repo_path(&read.attachments, "/Users/dev/repo-b");
        assert_eq!(found, Some(1));
    }

    #[test]
    fn resolve_case_sensitive() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("attachments.yml");

        let data = crate::domain::Attachments {
            attachments: vec![make_attachment("/Users/dev/Repo-A")],
        };
        crate::context_repo::attachments_io::write_attachments_atomic(&path, &data).unwrap();

        let read = read_attachments(&path).unwrap();
        assert_eq!(
            find_by_repo_path(&read.attachments, "/Users/dev/repo-a"),
            None
        );
        assert_eq!(
            find_by_repo_path(&read.attachments, "/Users/dev/Repo-A"),
            Some(0)
        );
    }

    #[test]
    fn resolve_missing_file_returns_empty() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("nonexistent.yml");
        let data = read_attachments(&path).unwrap();
        assert!(data.attachments.is_empty());
        assert_eq!(find_by_repo_path(&data.attachments, "/any"), None);
    }

    // --- save_source_repo_attachment logic tests ---

    /// Helper: simulate save logic without the Tauri command boundary
    /// (avoids needing codesense_home_dir).
    fn save_upsert(path: &std::path::Path, attachment: Attachment) -> Result<(), String> {
        validate_non_empty_no_control(&attachment.repo_path, "repo_path")
            .map_err(|e| e.to_string())?;
        if let Some(ref url) = attachment.remote_url {
            validate_non_empty_no_control(url, "remote_url").map_err(|e| e.to_string())?;
        }

        let mut data = read_attachments(path).map_err(|e| e.to_string())?;
        match find_by_repo_path(&data.attachments, &attachment.repo_path) {
            Some(idx) => {
                let existing_attached_at = data.attachments[idx].attached_at;
                data.attachments[idx] = attachment;
                data.attachments[idx].attached_at = existing_attached_at;
            }
            None => {
                let mut new = attachment;
                new.attached_at = Some(chrono::Utc::now());
                data.attachments.push(new);
            }
        }
        write_attachments_atomic(path, &data).map_err(|e| e.to_string())?;
        Ok(())
    }

    #[test]
    fn upsert_inserts_new_repo_path() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("attachments.yml");

        save_upsert(&path, make_attachment("/Users/dev/repo-a")).unwrap();
        let data = read_attachments(&path).unwrap();
        assert_eq!(data.attachments.len(), 1);
        assert_eq!(data.attachments[0].repo_path, "/Users/dev/repo-a");
    }

    #[test]
    fn upsert_replaces_existing_same_count() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("attachments.yml");

        save_upsert(&path, make_attachment("/Users/dev/repo-a")).unwrap();
        let mut updated = make_attachment("/Users/dev/repo-a");
        updated.remote_url = Some("https://github.com/org/repo.git".into());
        save_upsert(&path, updated).unwrap();

        let data = read_attachments(&path).unwrap();
        assert_eq!(data.attachments.len(), 1, "should replace, not append");
        assert_eq!(
            data.attachments[0].remote_url,
            Some("https://github.com/org/repo.git".into())
        );
    }

    #[test]
    fn upsert_attached_at_backend_owned_on_insert() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("attachments.yml");

        let mut a = make_attachment("/Users/dev/repo-a");
        a.attached_at = Some("2020-01-01T00:00:00Z".parse().unwrap());
        save_upsert(&path, a).unwrap();

        let data = read_attachments(&path).unwrap();
        // Backend should have overwritten with Utc::now(), not the caller's value.
        let attached = data.attachments[0].attached_at.unwrap();
        assert!(
            attached.timestamp() > 1700000000,
            "attached_at should be recent, got: {attached}"
        );
    }

    #[test]
    fn upsert_attached_at_preserved_on_update() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("attachments.yml");

        save_upsert(&path, make_attachment("/Users/dev/repo-a")).unwrap();
        let original_attached_at = read_attachments(&path).unwrap().attachments[0].attached_at;

        // Update with a different remote_url
        let mut updated = make_attachment("/Users/dev/repo-a");
        updated.remote_url = Some("https://changed.com".into());
        updated.attached_at = Some("1999-01-01T00:00:00Z".parse().unwrap());
        save_upsert(&path, updated).unwrap();

        let data = read_attachments(&path).unwrap();
        assert_eq!(
            data.attachments[0].attached_at, original_attached_at,
            "attached_at should be preserved from original insert"
        );
    }

    #[test]
    fn upsert_tracking_prompt_dismissed_at_caller_controlled() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("attachments.yml");

        save_upsert(&path, make_attachment("/Users/dev/repo-a")).unwrap();
        let data = read_attachments(&path).unwrap();
        assert_eq!(data.attachments[0].tracking_prompt_dismissed_at, None);

        // Update with dismissed_at
        let mut updated = make_attachment("/Users/dev/repo-a");
        let dismissed: chrono::DateTime<chrono::Utc> = "2025-06-01T00:00:00Z".parse().unwrap();
        updated.tracking_prompt_dismissed_at = Some(dismissed);
        save_upsert(&path, updated).unwrap();

        let data = read_attachments(&path).unwrap();
        assert_eq!(
            data.attachments[0].tracking_prompt_dismissed_at,
            Some(dismissed)
        );
    }

    #[test]
    fn upsert_rejects_empty_repo_path() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("attachments.yml");
        let result = save_upsert(&path, make_attachment(""));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("empty"));
    }

    #[test]
    fn upsert_rejects_control_chars_in_repo_path() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("attachments.yml");
        let result = save_upsert(&path, make_attachment("/Users/dev/repo\x00bad"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("control character"));
    }

    #[test]
    fn upsert_rejects_control_chars_in_remote_url() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("attachments.yml");
        let mut a = make_attachment("/Users/dev/repo");
        a.remote_url = Some("https://evil.com/\x01".into());
        let result = save_upsert(&path, a);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("control character"));
    }

    #[test]
    fn sequential_saves_different_paths_both_present() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("attachments.yml");

        save_upsert(&path, make_attachment("/Users/dev/repo-a")).unwrap();
        save_upsert(&path, make_attachment("/Users/dev/repo-b")).unwrap();

        let data = read_attachments(&path).unwrap();
        assert_eq!(data.attachments.len(), 2);
        assert_eq!(data.attachments[0].repo_path, "/Users/dev/repo-a");
        assert_eq!(data.attachments[1].repo_path, "/Users/dev/repo-b");
    }

    // ===== F-6: group_attachments_by_remote tests =====

    fn make_attachment_with_url(repo_path: &str, remote_url: &str) -> Attachment {
        Attachment {
            repo_path: repo_path.into(),
            remote_url: Some(remote_url.into()),
            attached_at: None,
            tracking_prompt_dismissed_at: None,
            credential_ref: None,
            managed: false,
        }
    }

    fn make_attachment_with_url_and_ts(repo_path: &str, remote_url: &str, ts: &str) -> Attachment {
        Attachment {
            repo_path: repo_path.into(),
            remote_url: Some(remote_url.into()),
            attached_at: Some(ts.parse().unwrap()),
            tracking_prompt_dismissed_at: None,
            credential_ref: None,
            managed: false,
        }
    }

    #[test]
    fn group_by_remote_empty_input_returns_empty_map() {
        let data = Attachments {
            attachments: vec![],
        };
        let result = group_attachments_by_remote(&data);
        assert!(result.is_empty());
    }

    #[test]
    fn group_by_remote_keys_a_remote_less_row_as_a_local_repo() {
        let data = Attachments {
            attachments: vec![Attachment {
                repo_path: "/Users/dev/samples/Sample-Api".into(),
                remote_url: None,
                attached_at: None,
                tracking_prompt_dismissed_at: None,
                credential_ref: None,
                managed: false,
            }],
        };
        let result = group_attachments_by_remote(&data);
        let slug =
            SourceRepoSlug::with_provider_and_host("local", "sample-api", "local", Some("local"))
                .unwrap();
        // The same key a team's `local/<folder>` source repo resolves under.
        assert_eq!(
            result.get(&slug.remote_key()).map(|a| a.repo_path.as_str()),
            Some("/Users/dev/samples/Sample-Api")
        );
        assert!(!is_repo_already_attached(&data, "local", "sample-api"));
    }

    #[test]
    fn group_by_remote_skips_rows_with_unparseable_url() {
        let data = Attachments {
            attachments: vec![
                make_attachment_with_url("/Users/dev/repo", "not a url"),
                // SW-FG-7: a supported non-GitHub forge now parses and is kept
                // (keyed by `host/owner/repo`); only the genuinely unparseable
                // row is skipped.
                make_attachment_with_url("/Users/dev/bitbucket-repo", "https://bitbucket.org/x/y"),
            ],
        };
        let result = group_attachments_by_remote(&data);
        assert_eq!(result.len(), 1, "only the unparseable row is skipped");
        assert_eq!(
            result.get("bitbucket.org/x/y").unwrap().repo_path,
            "/Users/dev/bitbucket-repo"
        );
    }

    #[test]
    fn group_by_remote_url_variants_collapse_to_same_key() {
        let variants = [
            ("/Users/dev/a", "https://github.com/Owner/Repo"),
            ("/Users/dev/b", "https://github.com/owner/repo.git"),
            ("/Users/dev/c", "git@github.com:OWNER/REPO.git"),
            ("/Users/dev/d", "git@github.com:owner/repo"),
        ];
        for (path, url) in &variants {
            let data = Attachments {
                attachments: vec![make_attachment_with_url(path, url)],
            };
            let result = group_attachments_by_remote(&data);
            assert_eq!(result.len(), 1, "url '{}' should produce 1 entry", url);
            assert!(
                result.contains_key("github.com/owner/repo"),
                "url '{}' should produce the canonical host-aware key, got keys: {:?}",
                url,
                result.keys().collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn group_by_remote_tie_breaker_newer_attached_at_wins() {
        let data = Attachments {
            attachments: vec![
                make_attachment_with_url_and_ts(
                    "/Users/dev/old-clone",
                    "https://github.com/owner/repo",
                    "2025-01-01T00:00:00Z",
                ),
                make_attachment_with_url_and_ts(
                    "/Users/dev/new-clone",
                    "https://github.com/owner/repo",
                    "2025-06-01T00:00:00Z",
                ),
            ],
        };
        let result = group_attachments_by_remote(&data);
        assert_eq!(result.len(), 1);
        assert_eq!(
            result.get("github.com/owner/repo").unwrap().repo_path,
            "/Users/dev/new-clone"
        );
    }

    #[test]
    fn group_by_remote_tie_breaker_older_loses_even_if_listed_first() {
        // Order: newer first, then older. Newer should still win (older
        // doesn't replace because its timestamp isn't strictly greater).
        let data = Attachments {
            attachments: vec![
                make_attachment_with_url_and_ts(
                    "/Users/dev/new-clone",
                    "https://github.com/owner/repo",
                    "2025-06-01T00:00:00Z",
                ),
                make_attachment_with_url_and_ts(
                    "/Users/dev/old-clone",
                    "https://github.com/owner/repo",
                    "2025-01-01T00:00:00Z",
                ),
            ],
        };
        let result = group_attachments_by_remote(&data);
        assert_eq!(
            result.get("github.com/owner/repo").unwrap().repo_path,
            "/Users/dev/new-clone"
        );
    }

    #[test]
    fn group_by_remote_both_none_timestamps_first_in_file_wins() {
        let data = Attachments {
            attachments: vec![
                make_attachment_with_url(
                    "/Users/dev/first-listed",
                    "https://github.com/owner/repo",
                ),
                make_attachment_with_url(
                    "/Users/dev/second-listed",
                    "https://github.com/owner/repo",
                ),
            ],
        };
        let result = group_attachments_by_remote(&data);
        assert_eq!(
            result.get("github.com/owner/repo").unwrap().repo_path,
            "/Users/dev/first-listed"
        );
    }

    #[test]
    fn group_by_remote_some_beats_none_regardless_of_order() {
        // Row with Some(attached_at) should beat row with None even if None is first.
        let data = Attachments {
            attachments: vec![
                make_attachment_with_url("/Users/dev/no-ts", "https://github.com/owner/repo"),
                make_attachment_with_url_and_ts(
                    "/Users/dev/has-ts",
                    "https://github.com/owner/repo",
                    "2025-01-01T00:00:00Z",
                ),
            ],
        };
        let result = group_attachments_by_remote(&data);
        assert_eq!(
            result.get("github.com/owner/repo").unwrap().repo_path,
            "/Users/dev/has-ts"
        );
    }

    // ===== SW-MC-1 AC-5: XOR guard (is_repo_already_attached) =====

    #[test]
    fn xor_guard_refuses_repo_already_attached_at_any_path() {
        let data = Attachments {
            attachments: vec![make_attachment_with_url(
                "/Users/dev/codesense",
                "https://github.com/alderco/codesense",
            )],
        };
        assert!(is_repo_already_attached(&data, "alderco", "codesense"));
    }

    #[test]
    fn xor_guard_is_case_insensitive_on_owner_and_repo() {
        let data = Attachments {
            attachments: vec![make_attachment_with_url(
                "/Users/dev/codesense",
                "https://github.com/AlderCo/CodeSense.git",
            )],
        };
        assert!(is_repo_already_attached(&data, "alderco", "codesense"));
        assert!(is_repo_already_attached(&data, "ALDERCO", "CODESENSE"));
    }

    #[test]
    fn xor_guard_allows_distinct_repo() {
        let data = Attachments {
            attachments: vec![make_attachment_with_url(
                "/Users/dev/codesense",
                "https://github.com/alderco/codesense",
            )],
        };
        assert!(!is_repo_already_attached(&data, "alderco", "codesense-mcp"));
        assert!(!is_repo_already_attached(&data, "other", "codesense"));
    }

    #[test]
    fn xor_guard_ignores_remoteless_records() {
        let data = Attachments {
            attachments: vec![make_attachment("/Users/dev/local-only")],
        };
        assert!(!is_repo_already_attached(&data, "any", "thing"));
    }

    // ===== SW-MC-1 AC-2/AC-3: managed insert rides the shared write path =====

    /// A `managed: true` insert through the shared upsert lands a managed
    /// record with the cloning account bound + backend-owned `attached_at` —
    /// the SAME insert semantics as attach-existing (AC-3 parity), differing
    /// only by the marker. No active team ⇒ reconcile skipped (still exercises
    /// the write path).
    #[tokio::test]
    async fn managed_upsert_writes_managed_record_bound_to_account() {
        let coord = WaveCoordinator::default();
        let active = active_team_state(false);
        let home = tempdir().unwrap();
        let att = Attachment {
            repo_path: "/home/uid/sources/alderco/codesense".into(),
            remote_url: Some("https://github.com/alderco/codesense".into()),
            attached_at: None,
            tracking_prompt_dismissed_at: None,
            credential_ref: Some(CredentialRef::github(42)),
            managed: true,
        };
        upsert_attachment_and_reconcile(home.path(), 42u64, att, &coord, &active)
            .await
            .unwrap();

        let path = path_to_user_attachments_file(home.path(), &Principal::from(42u64));
        let data = read_attachments(&path).unwrap();
        assert_eq!(data.attachments.len(), 1);
        assert!(data.attachments[0].managed, "record must be marked managed");
        assert_eq!(
            data.attachments[0].credential_ref,
            Some(CredentialRef::github(42))
        );
        assert!(
            data.attachments[0].attached_at.is_some(),
            "attached_at is backend-owned on insert (same as attach-existing)"
        );
        assert!(is_repo_already_attached(&data, "alderco", "codesense"));
    }

    // ===== SW-FG-7: credential-host invariant =====

    #[tokio::test]
    async fn upsert_does_not_relabel_an_account_for_another_provider() {
        let coord = WaveCoordinator::default();
        let active = active_team_state(false);
        let home = tempdir().unwrap();
        let att = Attachment {
            repo_path: "/home/uid/sources/acme/widget".into(),
            remote_url: Some("https://gitlab.com/acme/widget".into()),
            attached_at: None,
            tracking_prompt_dismissed_at: None,
            credential_ref: Some(CredentialRef::github(42)),
            managed: false,
        };
        upsert_attachment_and_reconcile(home.path(), 42u64, att, &coord, &active)
            .await
            .unwrap();

        let path = path_to_user_attachments_file(home.path(), &Principal::from(42u64));
        let data = read_attachments(&path).unwrap();
        assert_eq!(data.attachments.len(), 1);
        assert_eq!(data.attachments[0].credential_ref, None);
        assert_eq!(
            data.attachments[0].remote_url.as_deref(),
            Some("https://gitlab.com/acme/widget")
        );
    }

    /// A GitHub remote keeps its credential — the guard is a no-op.
    #[tokio::test]
    async fn upsert_keeps_github_credential_on_github_remote() {
        let coord = WaveCoordinator::default();
        let active = active_team_state(false);
        let home = tempdir().unwrap();
        let att = Attachment {
            repo_path: "/home/uid/sources/alderco/codesense".into(),
            remote_url: Some("https://github.com/alderco/codesense".into()),
            attached_at: None,
            tracking_prompt_dismissed_at: None,
            credential_ref: Some(CredentialRef::github(42)),
            managed: false,
        };
        upsert_attachment_and_reconcile(home.path(), 42u64, att, &coord, &active)
            .await
            .unwrap();

        let path = path_to_user_attachments_file(home.path(), &Principal::from(42u64));
        let data = read_attachments(&path).unwrap();
        assert_eq!(
            data.attachments[0].credential_ref,
            Some(CredentialRef::github(42))
        );
    }

    // ===== W-6: delete_source_repo_attachment tests =====

    /// Helper: simulate delete logic without the Tauri command boundary.
    fn delete_attachment(path: &std::path::Path, repo_path: &str) -> Result<(), String> {
        let mut data = read_attachments(path).map_err(|e| e.to_string())?;
        match find_by_repo_path(&data.attachments, repo_path) {
            Some(idx) => {
                data.attachments.remove(idx);
                write_attachments_atomic(path, &data).map_err(|e| e.to_string())?;
                Ok(())
            }
            None => Err(format!(
                "not_found: no attachment with repo_path '{repo_path}'"
            )),
        }
    }

    #[test]
    fn delete_happy_path_removes_matching_entry() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("attachments.yml");

        save_upsert(&path, make_attachment("/Users/dev/repo-a")).unwrap();
        save_upsert(&path, make_attachment("/Users/dev/repo-b")).unwrap();
        assert_eq!(read_attachments(&path).unwrap().attachments.len(), 2);

        delete_attachment(&path, "/Users/dev/repo-a").unwrap();

        let data = read_attachments(&path).unwrap();
        assert_eq!(data.attachments.len(), 1);
        assert_eq!(data.attachments[0].repo_path, "/Users/dev/repo-b");
    }

    #[test]
    fn delete_not_found_returns_error() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("attachments.yml");

        save_upsert(&path, make_attachment("/Users/dev/repo-a")).unwrap();
        let result = delete_attachment(&path, "/Users/dev/nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not_found"));
    }

    #[test]
    fn delete_empty_file_returns_error() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("attachments.yml");

        // File doesn't exist → read_attachments returns empty vec
        let result = delete_attachment(&path, "/any/path");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not_found"));
    }

    #[test]
    fn delete_last_entry_leaves_empty_list() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("attachments.yml");

        save_upsert(&path, make_attachment("/Users/dev/only-one")).unwrap();
        delete_attachment(&path, "/Users/dev/only-one").unwrap();

        let data = read_attachments(&path).unwrap();
        assert!(data.attachments.is_empty());
    }

    #[test]
    fn group_by_remote_distinct_remotes_produce_distinct_keys() {
        let data = Attachments {
            attachments: vec![
                make_attachment_with_url("/Users/dev/a", "https://github.com/foo/a"),
                make_attachment_with_url("/Users/dev/b", "https://github.com/foo/b"),
                make_attachment_with_url("/Users/dev/c", "https://github.com/bar/a"),
            ],
        };
        let result = group_attachments_by_remote(&data);
        assert_eq!(result.len(), 3);
        assert_eq!(
            result.get("github.com/foo/a").unwrap().repo_path,
            "/Users/dev/a"
        );
        assert_eq!(
            result.get("github.com/foo/b").unwrap().repo_path,
            "/Users/dev/b"
        );
        assert_eq!(
            result.get("github.com/bar/a").unwrap().repo_path,
            "/Users/dev/c"
        );
    }

    #[test]
    fn group_by_remote_separates_same_slug_on_different_hosts() {
        let data = Attachments {
            attachments: vec![
                make_attachment_with_url("/src/github", "https://github.com/acme/api"),
                make_attachment_with_url("/src/gitlab", "https://gitlab.com/acme/api"),
            ],
        };
        let result = group_attachments_by_remote(&data);
        assert_eq!(result["github.com/acme/api"].repo_path, "/src/github");
        assert_eq!(result["gitlab.com/acme/api"].repo_path, "/src/gitlab");
    }

    // ===== SS-FU-17 AC-2: teardown_for_detach coupling =====

    use crate::content_engine::sync::WaveCoordinator;
    use crate::mcp::state::{ActiveTeam, ActiveTeamState};
    use std::sync::Arc;

    fn active_team_state(set: bool) -> ActiveTeamState {
        let s = ActiveTeamState::default();
        if set {
            s.update(ActiveTeam {
                org_slug: "acme/team-context".into(),
                team_name: "default".into(),
            });
        }
        s
    }

    /// No `remote_url` → no slug-keyed lockfile to target → skip, empty
    /// report (the detach still proceeds in the command).
    #[tokio::test]
    async fn teardown_for_detach_skips_when_no_remote_url() {
        let coord = Arc::new(WaveCoordinator::default());
        let active = active_team_state(true);
        let home = tempdir().unwrap();
        let att = make_attachment("/Users/dev/repo-a"); // remote_url: None

        let report = teardown_for_detach(
            &coord,
            &active,
            home.path(),
            1234567u64,
            "/Users/dev/repo-a",
            &att,
        )
        .await
        .unwrap();
        assert_eq!(report, TeardownReport::default());
    }

    /// No active team → cannot resolve `(org, team)` → skip, empty report.
    #[tokio::test]
    async fn teardown_for_detach_skips_when_no_active_team() {
        let coord = Arc::new(WaveCoordinator::default());
        let active = active_team_state(false);
        let home = tempdir().unwrap();
        let att =
            make_attachment_with_url("/Users/dev/repo-a", "https://github.com/alderco/codesense");

        let report = teardown_for_detach(
            &coord,
            &active,
            home.path(),
            1234567u64,
            "/Users/dev/repo-a",
            &att,
        )
        .await
        .unwrap();
        assert_eq!(report, TeardownReport::default());
    }

    /// Active team + valid slug but no lockfile on disk → teardown is the
    /// idempotent no-op → empty report (detach of a never-materialized
    /// repo). Exercises the real slug-derivation + ctx-build + async
    /// `teardown_repo` path end to end.
    #[tokio::test]
    async fn teardown_for_detach_no_lockfile_returns_empty_report() {
        let coord = Arc::new(WaveCoordinator::default());
        let active = active_team_state(true);
        let home = tempdir().unwrap();
        let att =
            make_attachment_with_url("/Users/dev/repo-a", "https://github.com/alderco/codesense");

        let report = teardown_for_detach(
            &coord,
            &active,
            home.path(),
            1234567u64,
            "/Users/dev/repo-a",
            &att,
        )
        .await
        .unwrap();
        assert!(report.removed.is_empty() && report.skipped_locally_modified.is_empty());
    }

    // ===== SW-MC-3 AC-1/AC-3/AC-5/AC-8: update_managed_clone helpers =====

    fn managed_attachment(repo_path: &str, remote_url: Option<&str>) -> Attachment {
        Attachment {
            repo_path: repo_path.into(),
            remote_url: remote_url.map(str::to_string),
            attached_at: None,
            tracking_prompt_dismissed_at: None,
            credential_ref: None,
            managed: true,
        }
    }

    // ===== SW-MC-3 AC-2: record-forget helpers + remove_managed_clone =====

    /// Write an `attachments.yml` at the home-derived path for `uid`.
    fn write_user_attachments(home: &Path, uid: u64, atts: Vec<Attachment>) {
        let p = path_to_user_attachments_file(home, &Principal::from(uid));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        write_attachments_atomic(&p, &crate::domain::Attachments { attachments: atts }).unwrap();
    }

    fn read_user_attachments(home: &Path, uid: u64) -> Vec<Attachment> {
        read_attachments(&path_to_user_attachments_file(home, &Principal::from(uid)))
            .unwrap()
            .attachments
    }

    #[test]
    fn resolve_attachment_record_found_and_not_found() {
        let home = tempdir().unwrap();
        write_user_attachments(home.path(), 42, vec![make_attachment("/d/a")]);
        assert_eq!(
            resolve_attachment_record(home.path(), 42, "/d/a")
                .unwrap()
                .repo_path,
            "/d/a"
        );
        assert!(resolve_attachment_record(home.path(), 42, "/d/missing")
            .unwrap_err()
            .starts_with("not_found"));
    }

    #[test]
    fn forget_attachment_record_removes_and_returns() {
        let home = tempdir().unwrap();
        write_user_attachments(
            home.path(),
            42,
            vec![make_attachment("/d/a"), make_attachment("/d/b")],
        );
        let removed = forget_attachment_record(home.path(), 42, "/d/a").unwrap();
        assert_eq!(removed.repo_path, "/d/a");
        let left = read_user_attachments(home.path(), 42);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].repo_path, "/d/b");
    }

    // ===== SW-MC-3 AC-7: list_managed_clone_health =====

    #[test]
    fn managed_clone_health_recognizes_clone_under_users_and_flags_moved() {
        let home = tempdir().unwrap();
        // A managed clone at the default managed home (under users/) is
        // recognized; one hand-moved outside is not.
        let recognized_path = path_to_managed_source_clone(
            home.path(),
            &Principal::from(42u64),
            &SourceRepoSlug::new("o", "r").unwrap(),
        )
        .to_string_lossy()
        .into_owned();
        let moved_path = home.path().join("elsewhere").join("clone");
        let moved_path = moved_path.to_string_lossy().into_owned();

        let data = crate::domain::Attachments {
            attachments: vec![
                managed_attachment(&recognized_path, Some("https://github.com/o/r")),
                managed_attachment(&moved_path, Some("https://github.com/o/r2")),
                make_attachment("/some/attached/dir"), // managed=false → excluded
            ],
        };

        let health = managed_clone_health(&data, home.path());
        // Attached row excluded.
        assert_eq!(health.len(), 2);
        let by_path: std::collections::HashMap<_, _> = health
            .iter()
            .map(|h| (h.repo_path.as_str(), h.recognized))
            .collect();
        assert_eq!(by_path.get(recognized_path.as_str()), Some(&true));
        assert_eq!(by_path.get(moved_path.as_str()), Some(&false));
    }

    #[test]
    fn managed_clone_health_empty_when_no_managed_rows() {
        let home = tempdir().unwrap();
        let data = crate::domain::Attachments {
            attachments: vec![make_attachment("/d/a"), make_attachment("/d/b")],
        };
        assert!(managed_clone_health(&data, home.path()).is_empty());
    }

    // ===== SW-MC-2 (Option-C) × SW-MC-3 coupling: configured-root clones must
    // NOT be refused on Remove nor flagged "not syncing" =====

    /// Scaffold an org root whose `org.yml` configures `workspace_root_dir`.
    /// The `org.yml` is also what `enumerate_all_org_roots` recognizes (RF-31).
    fn scaffold_org_with_workspace_root(home: &Path, uid: u64, org_slug: &str, root: &Path) {
        let org_root = path_to_org_root(home, &Principal::from(uid), org_slug).unwrap();
        std::fs::create_dir_all(&org_root).unwrap();
        std::fs::write(
            org_root.join("org.yml"),
            format!(
                "slug: {org_slug}\ndisplay_name: Test\ncreated_at: \"2026-01-01T00:00:00Z\"\nconfig_flags:\n  workspace_root_dir: {}\n",
                root.display()
            ),
        )
        .unwrap();
    }

    #[test]
    fn managed_clone_health_recognizes_configured_root_clone() {
        // A clone under the configured root is recognized; one hand-moved OUTSIDE
        // every managed root is still flagged — the AC-7 signal stays meaningful.
        let home = tempdir().unwrap();
        let root = home.path().join("mycode");
        scaffold_org_with_workspace_root(home.path(), 42, "acme", &root);
        let configured = root.join("o").join("r").to_string_lossy().into_owned();
        let moved = home
            .path()
            .join("elsewhere")
            .join("clone")
            .to_string_lossy()
            .into_owned();

        let data = crate::domain::Attachments {
            attachments: vec![
                managed_attachment(&configured, Some("https://github.com/o/r")),
                managed_attachment(&moved, Some("https://github.com/o/r2")),
            ],
        };
        let health = managed_clone_health(&data, home.path());
        let by_path: std::collections::HashMap<_, _> = health
            .iter()
            .map(|h| (h.repo_path.as_str(), h.recognized))
            .collect();
        assert_eq!(
            by_path.get(configured.as_str()),
            Some(&true),
            "configured-root clone must be recognized"
        );
        assert_eq!(
            by_path.get(moved.as_str()),
            Some(&false),
            "a clone outside every managed root is still flagged"
        );
    }

    #[test]
    fn managed_clone_health_ignores_degenerate_configured_root() {
        // Mirror of the remove guard: a `/` root must not flip every managed row
        // to recognized=true (which would silently kill the AC-7 not-syncing chip).
        let home = tempdir().unwrap();
        scaffold_org_with_workspace_root(home.path(), 42, "acme", Path::new("/"));
        let arbitrary = "/Users/someone/important-project".to_string();
        let data = crate::domain::Attachments {
            attachments: vec![managed_attachment(
                &arbitrary,
                Some("https://github.com/o/r"),
            )],
        };
        let health = managed_clone_health(&data, home.path());
        assert_eq!(health.len(), 1);
        assert!(
            !health[0].recognized,
            "a degenerate `/` workspace root must not recognize arbitrary paths"
        );
    }

    #[test]
    fn managed_clone_health_recognizes_tilde_root_clone() {
        // Full portability chain: org.yml stores the SHARED value as `~/mycode`;
        // the lifecycle sweep expands `~` to THIS machine's home, so a clone at
        // `{home}/mycode/o/r` is recognized (not flagged "not syncing").
        let home = tempdir().unwrap();
        scaffold_org_with_workspace_root(home.path(), 42, "acme", Path::new("~/mycode"));
        let clone = dirs::home_dir()
            .unwrap()
            .join("mycode/o/r")
            .to_string_lossy()
            .into_owned();
        let data = crate::domain::Attachments {
            attachments: vec![managed_attachment(&clone, Some("https://github.com/o/r"))],
        };
        let health = managed_clone_health(&data, home.path());
        assert_eq!(health.len(), 1);
        assert!(
            health[0].recognized,
            "a clone under the expanded ~ workspace root must be recognized"
        );
    }

    // ===== SW-OC-3: remove_source_repo_from_team round-trip =====

    fn remove_source_repo_for_test(
        home: &Path,
        uid: u64,
        org: &str,
        team: &str,
        slug: &SourceRepoSlug,
    ) -> (Result<Option<String>, String>, Vec<String>) {
        use tauri::Listener;
        let app = crate::commands::source_repo_add::tests::mock_app();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        for name in [
            crate::commands::registry_local::ORG_REGISTRY_CHANGED_EVENT,
            crate::commands::source_repo_add::SOURCE_REPO_ATTACHMENTS_CHANGED_EVENT,
        ] {
            let sink = seen.clone();
            app.listen(name, move |_| sink.lock().unwrap().push(name.to_string()));
        }
        let result = remove_source_repo_and_notify(app.handle(), home, uid, org, team, slug);
        let events = seen.lock().unwrap().clone();
        (result, events)
    }

    fn bind(home: &Path, uid: u64, repo_path: &str, remote_url: &str) {
        let attachments_path = path_to_user_attachments_file(home, &Principal::from(uid));
        std::fs::create_dir_all(attachments_path.parent().unwrap()).unwrap();
        let mut data = read_attachments(&attachments_path).unwrap_or_default();
        data.attachments.push(Attachment {
            repo_path: repo_path.into(),
            remote_url: Some(remote_url.into()),
            attached_at: None,
            tracking_prompt_dismissed_at: None,
            credential_ref: None,
            managed: false,
        });
        write_attachments_atomic(&attachments_path, &data).unwrap();
    }

    #[test]
    fn removing_from_one_team_keeps_the_binding_another_team_still_uses() {
        let home = tempdir().unwrap();
        let uid = 5;
        let body = "teams:\n  - name: default\n    source_repos:\n\
             \x20     - owner: acme\n        repo: api\n  - name: web\n    source_repos:\n\
             \x20     - owner: acme\n        repo: api\n";
        write_teams_yml(home.path(), uid, "acme/ctx", body);
        bind(
            home.path(),
            uid,
            "/code/api",
            "https://github.com/acme/api.git",
        );
        let slug = SourceRepoSlug::new("acme", "api").unwrap();

        let (result, events) =
            remove_source_repo_for_test(home.path(), uid, "acme/ctx", "default", &slug);
        assert_eq!(result, Ok(None));
        assert_eq!(events.len(), 2);
        let data = read_attachments(&path_to_user_attachments_file(
            home.path(),
            &Principal::from(uid),
        ))
        .unwrap();
        assert_eq!(
            data.attachments.len(),
            1,
            "the web team still uses the clone"
        );

        // The last team dropping it forgets the binding.
        let (result, _) = remove_source_repo_for_test(home.path(), uid, "acme/ctx", "web", &slug);
        assert_eq!(result, Ok(Some("/code/api".to_string())));
    }

    #[test]
    fn a_team_in_another_org_keeps_the_binding_too() {
        let home = tempdir().unwrap();
        let uid = 5;
        write_teams_yml(
            home.path(),
            uid,
            "acme/ctx",
            "teams:\n  - name: default\n    source_repos:\n\
             \x20     - owner: acme\n        repo: api\n",
        );
        let other = path_to_org_root(home.path(), &Principal::from(uid), "beta/ctx").unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(
            other.join("org.yml"),
            "slug: beta/ctx\ndisplay_name: Beta\ncreated_at: \"2026-01-01T00:00:00Z\"\n",
        )
        .unwrap();
        std::fs::write(
            other.join("teams.yml"),
            "teams:\n  - name: ops\n    source_repos:\n\
             \x20     - owner: acme\n        repo: api\n",
        )
        .unwrap();
        git2::Repository::init(&other).unwrap();
        bind(home.path(), uid, "/code/api", "git@github.com:acme/api.git");

        let slug = SourceRepoSlug::new("acme", "api").unwrap();
        let (result, _) =
            remove_source_repo_for_test(home.path(), uid, "acme/ctx", "default", &slug);
        assert_eq!(result, Ok(None));
    }

    #[test]
    fn a_gitlab_repo_is_removed_and_unbound_by_its_own_key() {
        let home = tempdir().unwrap();
        let uid = 5;
        write_teams_yml(
            home.path(),
            uid,
            "acme/ctx",
            "teams:\n  - name: default\n    source_repos:\n\
             \x20     - owner: acme\n        repo: web\n        provider: gitlab\n        host: gitlab.com\n",
        );
        bind(
            home.path(),
            uid,
            "/code/web",
            "https://gitlab.com/acme/web.git",
        );
        let slug =
            SourceRepoSlug::with_provider_and_host("acme", "web", "gitlab", Some("gitlab.com"))
                .unwrap();
        let (result, _) =
            remove_source_repo_for_test(home.path(), uid, "acme/ctx", "default", &slug);
        assert_eq!(result, Ok(Some("/code/web".to_string())));
    }

    #[test]
    fn both_events_fire_once_the_registry_changed_even_if_unbinding_fails() {
        let home = tempdir().unwrap();
        let uid = 5;
        write_teams_yml(
            home.path(),
            uid,
            "acme/ctx",
            "teams:\n  - name: default\n    source_repos:\n\
             \x20     - owner: acme\n        repo: api\n",
        );
        let attachments_path = path_to_user_attachments_file(home.path(), &Principal::from(uid));
        std::fs::create_dir_all(attachments_path.parent().unwrap()).unwrap();
        std::fs::write(&attachments_path, "attachments: [ this is not yaml").unwrap();
        let slug = SourceRepoSlug::new("acme", "api").unwrap();

        let (result, events) =
            remove_source_repo_for_test(home.path(), uid, "acme/ctx", "default", &slug);
        assert!(result.is_err());
        assert_eq!(
            events,
            [
                crate::commands::registry_local::ORG_REGISTRY_CHANGED_EVENT,
                crate::commands::source_repo_add::SOURCE_REPO_ATTACHMENTS_CHANGED_EVENT,
            ]
        );
    }

    fn write_teams_yml(home: &Path, uid: u64, org_slug: &str, body: &str) {
        let org_root = path_to_org_root(home, &Principal::from(uid), org_slug).unwrap();
        std::fs::create_dir_all(&org_root).unwrap();
        std::fs::write(org_root.join("teams.yml"), body).unwrap();
    }

    #[test]
    fn remove_source_repo_detaches_and_preserves_clone() {
        let home = tempdir().unwrap();
        let uid = 42;
        let org = "acme/widgets-context";

        // Attach state: two repos tracked under the team; the target repo has an
        // attachment record pointing at an on-disk local clone.
        write_teams_yml(
            home.path(),
            uid,
            org,
            "teams:\n  - name: default\n    source_repos:\n\
             \x20     - owner: alderco\n        repo: codesense\n\
             \x20     - owner: org\n        repo: other\n",
        );

        let clone_dir = home.path().join("clones").join("codesense");
        std::fs::create_dir_all(&clone_dir).unwrap();
        std::fs::write(clone_dir.join("README.md"), b"local work").unwrap();
        let clone_path = clone_dir.to_string_lossy().to_string();

        let attachments_path = path_to_user_attachments_file(home.path(), &Principal::from(uid));
        std::fs::create_dir_all(attachments_path.parent().unwrap()).unwrap();
        let data = Attachments {
            attachments: vec![Attachment {
                repo_path: clone_path.clone(),
                remote_url: Some("https://github.com/alderco/codesense".into()),
                attached_at: None,
                tracking_prompt_dismissed_at: None,
                credential_ref: None,
                managed: false,
            }],
        };
        write_attachments_atomic(&attachments_path, &data).unwrap();

        let slug = SourceRepoSlug::new("alderco", "codesense").unwrap();
        remove_source_repo_for_test(home.path(), uid, org, "default", &slug)
            .0
            .unwrap();

        // teams.yml: target gone, sibling remains.
        let teams_text = std::fs::read_to_string(
            path_to_org_root(home.path(), &Principal::from(uid), org)
                .unwrap()
                .join("teams.yml"),
        )
        .unwrap();
        assert!(
            !teams_text.contains("codesense"),
            "detached repo should be gone from teams.yml"
        );
        assert!(
            teams_text.contains("other"),
            "sibling repo should still be tracked"
        );

        // attachment record: gone.
        let after = read_attachments(&attachments_path).unwrap();
        assert!(
            find_by_repo_path(&after.attachments, &clone_path).is_none(),
            "attachment record should be forgotten"
        );

        // local clone: PRESERVED on disk (detach is not uninstall).
        assert!(clone_dir.exists(), "local clone dir must survive detach");
        assert!(
            clone_dir.join("README.md").exists(),
            "local clone contents must survive detach"
        );
    }

    #[test]
    fn remove_source_repo_untracked_but_attached_still_forgets_record() {
        // A repo tracked-then-attached, but the record persists after the edit
        // even if never materialized — detach still clears the record.
        let home = tempdir().unwrap();
        let uid = 7;
        let org = "acme/ctx";
        write_teams_yml(
            home.path(),
            uid,
            org,
            "teams:\n  - name: default\n    source_repos:\n\
             \x20     - owner: solo\n        repo: repo\n",
        );
        let clone_dir = home.path().join("solo-repo");
        std::fs::create_dir_all(&clone_dir).unwrap();
        let clone_path = clone_dir.to_string_lossy().to_string();
        let attachments_path = path_to_user_attachments_file(home.path(), &Principal::from(uid));
        std::fs::create_dir_all(attachments_path.parent().unwrap()).unwrap();
        write_attachments_atomic(
            &attachments_path,
            &Attachments {
                attachments: vec![Attachment {
                    repo_path: clone_path.clone(),
                    remote_url: Some("git@github.com:solo/repo.git".into()),
                    attached_at: None,
                    tracking_prompt_dismissed_at: None,
                    credential_ref: None,
                    managed: false,
                }],
            },
        )
        .unwrap();

        let slug = SourceRepoSlug::new("solo", "repo").unwrap();
        remove_source_repo_for_test(home.path(), uid, org, "default", &slug)
            .0
            .unwrap();

        let after = read_attachments(&attachments_path).unwrap();
        assert!(
            after.attachments.is_empty(),
            "record forgotten via SSH-form URL match"
        );
        assert!(clone_dir.exists(), "clone preserved");
    }

    #[test]
    fn remove_source_repo_tracked_never_attached_is_ok() {
        // No attachment record at all — detach just drops the tracking edit.
        let home = tempdir().unwrap();
        let uid = 9;
        let org = "acme/ctx";
        write_teams_yml(
            home.path(),
            uid,
            org,
            "teams:\n  - name: default\n    source_repos:\n\
             \x20     - owner: solo\n        repo: repo\n",
        );
        let slug = SourceRepoSlug::new("solo", "repo").unwrap();
        remove_source_repo_for_test(home.path(), uid, org, "default", &slug)
            .0
            .unwrap();
        let teams_text = std::fs::read_to_string(
            path_to_org_root(home.path(), &Principal::from(uid), org)
                .unwrap()
                .join("teams.yml"),
        )
        .unwrap();
        assert!(!teams_text.contains("solo"), "repo untracked");
    }
}
