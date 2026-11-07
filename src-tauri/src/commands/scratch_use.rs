// SPDX-License-Identifier: MIT
//! Where the user's scratch items apply, per team — the commands behind the
//! "Use my scratch items in ‹Team›" switch and the per-file "From scratch"
//! badge with its one-click "Use team's".
//!
//! The settings live in the scratch repo
//! ([`crate::context_repo::scratch_space::team_scratch_use`]) and every backend
//! fold reads them with the team the WORK belongs to. These commands only read
//! and write that store, then re-derive the affected worktrees so the overlay
//! follows at once. The badge feed reads what a worktree actually materialized
//! (its lockfile entries), so it never claims a file came from scratch when it
//! did not.
//!
//! All disk work runs on the blocking pool via [`await_blocking`].

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::Emitter;

use crate::content_engine::sync_state_store::TargetKey;
use crate::context_repo::scratch_space::{
    self, ALL_ORGS_PROJECTION_PREFIX, SCRATCH_PROJECTION_PREFIX,
};
use crate::domain::Principal;
use crate::install::worktree_index;

use super::async_helpers::await_blocking;
use super::route_to_context::resolve_ambient;

/// The event a change to a team's scratch-use settings emits, so every view of
/// that team (packs, automations, file badges) re-reads.
pub const SCRATCH_USE_CHANGED_EVENT: &str = "scratch-use-changed";

/// The `scratch-use-changed` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScratchUseChanged {
    pub org_slug: String,
    pub team: String,
}

/// Where a materialized worktree file came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ScratchFileOrigin {
    /// The org's scratch space.
    Scratch,
    /// The all-orgs folder (inert items carried into every org).
    AllOrgs,
}

/// One worktree file the scratch overlay touches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScratchOverlayFile {
    /// Worktree-relative path (`/`-separated).
    pub path: String,
    /// The content-relative path the per-team settings key on.
    pub logical: String,
    pub origin: ScratchFileOrigin,
    /// The team has its own file at this path, which the scratch file replaces.
    pub replaces_team: bool,
    /// "Use team's" is on here: the worktree holds the TEAM's file and the
    /// scratch copy is set aside (still in scratch).
    pub using_team: bool,
    /// The org and team of the worktree (the work's own, never the one on
    /// screen) — what Share / Copy / Use in all orgs act in.
    pub org_slug: String,
    pub team: String,
    /// For a file from the org's scratch space: the inert kind it would apply as
    /// in every org ("Use in all orgs"), or `None` when it cannot go there.
    pub inert_kind: Option<scratch_space::InertKind>,
}

/// The `(org, team)` a worktree belongs to — from its own index record (the
/// work), never the team on screen. `None` for a worktree CodeSense does not
/// track (nothing of scratch is materialized there).
fn worktree_team(
    home: &Path,
    principal: &Principal,
    code_repo_path: &Path,
) -> Result<Option<(String, String)>, String> {
    let record = worktree_index::read_record(home, principal, code_repo_path)
        .map_err(|e| format!("worktree index read failed: {e}"))?;
    Ok(record.and_then(|record| {
        worktree_index::resolve_team(&record).map(|team| (record.org_slug.clone(), team))
    }))
}

fn rel_of(dest: &Path, code_repo_path: &Path, logical: &str) -> String {
    dest.strip_prefix(code_repo_path)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| logical.to_string())
}

pub(crate) fn team_content_roots_at(
    home: &Path,
    principal: &Principal,
    org: &str,
    team: &str,
) -> Result<Vec<crate::context_repo::inheritance::ContentRoot>, String> {
    let ctx = super::content_engine::derive_resolver_context(
        home.to_path_buf(),
        principal.numeric_id()?,
        org.to_string(),
        Some(team.to_string()),
    )
    .map_err(|e| format!("{e:?}"))?;
    Ok(ctx
        .view()
        .map(|view| {
            std::iter::once(view.own_content())
                .chain(view.inherited.iter().map(|l| l.content()))
                .collect()
        })
        .unwrap_or_default())
}

pub(crate) fn team_has_file(
    roots: &[crate::context_repo::inheritance::ContentRoot],
    logical: &str,
) -> bool {
    roots.iter().any(|root| root.exists(logical))
}

/// The scratch overlay files materialized into `code_repo_path` (a tracked
/// worktree), plus the paths where its team chose "Use team's". Blocking.
pub(crate) fn scratch_overlay_files_at(
    home: &Path,
    principal: &Principal,
    code_repo_path: &Path,
) -> Result<Vec<ScratchOverlayFile>, String> {
    let Some((org, team)) = worktree_team(home, principal, code_repo_path)? else {
        return Ok(Vec::new());
    };
    // Switched off for this team: nothing of scratch is in its worktrees, and no
    // "set aside" copy is on offer either.
    if !scratch_space::team_uses_scratch(home, principal, &org, &team) {
        return Ok(Vec::new());
    }
    let Some(store) = crate::content_engine::lockfile::open_sync_state_store(home) else {
        return Ok(Vec::new());
    };
    let key = TargetKey {
        org_slug: &org,
        team_name: Some(&team),
        code_repo_path,
    };
    let Some(lockfile) = store.read(&key).map_err(|e| e.to_string())? else {
        return Ok(Vec::new());
    };
    let team_roots = team_content_roots_at(home, principal, &org, &team)?;
    let using_team = scratch_space::team_scratch_use(home, principal, &org, &team).use_team_files;
    let scratch_content =
        scratch_space::org_space(home, principal, &org).map(|space| space.content_dir());
    let inert_in = |logical: &str| {
        scratch_content
            .as_ref()
            .and_then(|root| super::scratch_space_view::inert_kind_of(&root.join(logical), logical))
    };

    let mut out = Vec::new();
    for entry in &lockfile.entries {
        let key = entry.projection_key.as_deref().unwrap_or_default();
        let origin = if key.starts_with(SCRATCH_PROJECTION_PREFIX) {
            ScratchFileOrigin::Scratch
        } else if key.starts_with(ALL_ORGS_PROJECTION_PREFIX) {
            ScratchFileOrigin::AllOrgs
        } else {
            // A team file the user chose over their scratch copy.
            let set_aside = using_team.iter().any(|p| p == &entry.logical_name)
                && scratch_content
                    .as_ref()
                    .is_some_and(|root| root.join(&entry.logical_name).is_file());
            if set_aside {
                out.push(ScratchOverlayFile {
                    path: rel_of(&entry.dest_path, code_repo_path, &entry.logical_name),
                    logical: entry.logical_name.clone(),
                    origin: ScratchFileOrigin::Scratch,
                    replaces_team: true,
                    using_team: true,
                    org_slug: org.clone(),
                    team: team.clone(),
                    inert_kind: inert_in(&entry.logical_name),
                });
            }
            continue;
        };
        out.push(ScratchOverlayFile {
            path: rel_of(&entry.dest_path, code_repo_path, &entry.logical_name),
            logical: entry.logical_name.clone(),
            origin,
            replaces_team: origin == ScratchFileOrigin::Scratch
                && team_has_file(&team_roots, &entry.logical_name),
            using_team: false,
            org_slug: org.clone(),
            team: team.clone(),
            inert_kind: match origin {
                ScratchFileOrigin::Scratch => inert_in(&entry.logical_name),
                ScratchFileOrigin::AllOrgs => None,
            },
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn emit_changed(app: &tauri::AppHandle, home: &Path, org: &str, team: &str) {
    // Personal routines and agents re-read who they reach.
    crate::routines::discover::invalidate_discovery_cache(home);
    let _ = app.emit(
        SCRATCH_USE_CHANGED_EVENT,
        ScratchUseChanged {
            org_slug: org.to_string(),
            team: team.to_string(),
        },
    );
}

/// The profile + home a scratch-use command acts as.
struct Ambient {
    codesense_home: PathBuf,
    user_id: u64,
}

impl Ambient {
    async fn of(app: &tauri::AppHandle) -> Result<Self, String> {
        let (codesense_home, user_id) = resolve_ambient(app).await?;
        Ok(Self {
            codesense_home,
            user_id,
        })
    }

    fn principal(&self) -> Principal {
        Principal::from(self.user_id)
    }
}

/// Whether the user's scratch items apply in `org_slug/team`.
#[tauri::command]
pub async fn get_scratch_team_use(
    app: tauri::AppHandle,
    org_slug: String,
    team: String,
) -> Result<bool, String> {
    // Every command names its org/team (or worktree): only the profile is ambient,
    // so they work whether or not the backend has an active team yet.
    let ctx = Ambient::of(&app).await?;
    await_blocking("get_scratch_team_use", move || {
        Ok(scratch_space::team_uses_scratch(
            &ctx.codesense_home,
            &ctx.principal(),
            &org_slug,
            &team,
        ))
    })
    .await
}

/// "Use my scratch items in ‹team›": on or off. Stored in the scratch repo.
#[tauri::command]
pub async fn set_scratch_team_use(
    app: tauri::AppHandle,
    org_slug: String,
    team: String,
    on: bool,
) -> Result<(), String> {
    // Every command names its org/team (or worktree): only the profile is ambient,
    // so they work whether or not the backend has an active team yet.
    let ctx = Ambient::of(&app).await?;
    let (home, principal) = (ctx.codesense_home.clone(), ctx.principal());
    let (org, t) = (org_slug.clone(), team.clone());
    await_blocking("set_scratch_team_use", move || {
        scratch_space::set_team_uses_scratch(&home, &principal, &org, &t, on).map_err(String::from)
    })
    .await?;
    emit_changed(&app, &ctx.codesense_home, &org_slug, &team);
    Ok(())
}

/// The scratch overlay files of the worktree at `code_repo_path`, for the
/// per-file "From scratch" badge. Empty for an untracked worktree.
#[tauri::command]
pub async fn list_scratch_overlay_files(
    app: tauri::AppHandle,
    code_repo_path: String,
) -> Result<Vec<ScratchOverlayFile>, String> {
    // Every command names its org/team (or worktree): only the profile is ambient,
    // so they work whether or not the backend has an active team yet.
    let ctx = Ambient::of(&app).await?;
    await_blocking("list_scratch_overlay_files", move || {
        scratch_overlay_files_at(
            &ctx.codesense_home,
            &ctx.principal(),
            Path::new(&code_repo_path),
        )
    })
    .await
}

/// "Use team's" (`use_team = true`) at the scratch file `logical` of the
/// worktree at `code_repo_path` — for that worktree's team — or back to the
/// scratch file (`false`). The scratch file itself is never moved or deleted.
#[tauri::command]
pub async fn set_scratch_file_use_team(
    app: tauri::AppHandle,
    code_repo_path: String,
    logical: String,
    use_team: bool,
) -> Result<(), String> {
    // Every command names its org/team (or worktree): only the profile is ambient,
    // so they work whether or not the backend has an active team yet.
    let ctx = Ambient::of(&app).await?;
    let (home, principal) = (ctx.codesense_home.clone(), ctx.principal());
    let (org, team) = await_blocking("set_scratch_file_use_team", move || {
        let (org, team) = worktree_team(&home, &principal, Path::new(&code_repo_path))?
            .ok_or("this worktree is not tracked by CodeSense")?;
        scratch_space::set_team_file_uses_team(&home, &principal, &org, &team, &logical, use_team)
            .map_err(String::from)?;
        Ok::<_, String>((org, team))
    })
    .await?;
    emit_changed(&app, &ctx.codesense_home, &org, &team);
    Ok(())
}

#[cfg(test)]
mod inheritance_tests {
    use super::*;
    #[test]
    fn use_team_does_not_offer_unshared_ancestor_files() {
        let (home, root) = crate::context_repo::inheritance::test_fixture();
        std::fs::remove_file(root.join("child/content/secret.md")).unwrap();
        std::fs::remove_file(root.join("child/content/shared.md")).unwrap();
        let roots = team_content_roots_at(home.path(), &Principal::from(1u64), "acme/ctx", "child")
            .unwrap();
        assert!(team_has_file(&roots, "shared.md"));
        assert!(!team_has_file(&roots, "secret.md"));
        assert!(!team_has_file(&roots, ".claude/skills/grand/SKILL.md"));
    }
}
