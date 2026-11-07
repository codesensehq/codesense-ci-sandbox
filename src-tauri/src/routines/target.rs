// SPDX-License-Identifier: MIT
//! Resolve a routine's content target and availability. Agent runs prepare a
//! separate working directory; commands use the target directly. A missing or
//! unmanaged target is unavailable, never replaced by some local directory.

use std::path::{Path, PathBuf};

use crate::context_repo::resolve_managed_clone_dir;
use crate::domain::{Principal, SourceRepoSlug};

use super::model::{RoutineDef, RoutineKey, RoutineTarget};

/// The team content dir a routine belongs to: its team workspace (an org's
/// scratch space for a personal routine).
fn team_content_dir(
    home: &Path,
    principal: &Principal,
    key: &RoutineKey,
) -> Result<PathBuf, String> {
    super::service::key_content_dir(home, principal, key).map_err(|e| e.to_string())
}

/// The folder a run of the personal routine `key` works in: its own folder of
/// the org's scratch space, made on first use. Never the space's `content/`,
/// which is overlaid into every worktree of the org: the launch writes the
/// agent's MCP and CLI config into its working dir. Unavailable while the org
/// has no scratch space.
fn personal_run_dir(
    home: &Path,
    principal: &Principal,
    key: &RoutineKey,
) -> Result<PathBuf, String> {
    let space = crate::context_repo::scratch_space::org_space(home, principal, &key.org)
        .ok_or_else(|| format!("{} has no private space on this machine", key.org))?;
    let dir = space.routine_run_dir(&key.id);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("cannot make the run folder '{}': {e}", dir.display()))?;
    // Run folders are scratch, not the user's items: keep them out of the
    // scratch repo's git status (`orgs/<owner>/<repo>/.run/`).
    let repo = crate::context_repo::scratch_space::scratch_root(home, principal);
    let entry = format!(
        "/orgs/*/*/{}/",
        crate::context_repo::scratch_space::ROUTINE_RUNS_DIR
    );
    if let Err(e) = crate::content_engine::git_exclude::add_entries(&repo, &[entry.as_str()]) {
        log::debug!(target: "routines", "could not exclude {entry} in the scratch repo: {e}");
    }
    prune_orphan_run_dirs(home, principal, key, &dir);
    canonical_dir(&dir, "run folder")?;
    Ok(dir)
}

/// Whether the personal routine `id` of `key`'s org still exists un-archived
/// here. Unreadable counts as alive: never delete on doubt.
fn personal_routine_is_live(
    home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    id: &str,
) -> bool {
    let sibling = RoutineKey::personal(principal, key.org.clone(), id);
    let Ok(dir) = super::service::routine_dir_of(home, principal, &sibling) else {
        return true;
    };
    match super::defs::read_def(&dir) {
        Ok(Some(def)) => !def.is_archived(),
        Ok(None) => false,
        Err(_) => true,
    }
}

/// The routines with a run queued or running here; `None` when the run store
/// cannot be read (then no run folder is removed: never delete on doubt).
fn routines_with_active_runs(home: &Path) -> Option<std::collections::HashSet<String>> {
    super::state::RoutineStateStore::open(home)
        .and_then(|store| store.active_routine_keys())
        .map_err(|e| log::debug!(target: "routines", "cannot read active runs: {e}"))
        .ok()
}

/// Bound the run folders of an org's scratch space: each run of a personal
/// routine sweeps away the folders of routines that are gone or archived (a
/// delete on another Mac, a hand edit), keeping `current` and any routine's
/// folder while it has a run queued or running (swept after that run ends).
fn prune_orphan_run_dirs(home: &Path, principal: &Principal, key: &RoutineKey, current: &Path) {
    let Some(runs) = current.parent() else {
        return;
    };
    let Some(active) = routines_with_active_runs(home) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(runs) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let id = entry.file_name().to_string_lossy().into_owned();
        if path == current
            || !path.is_dir()
            || !super::model::is_valid_id(&id)
            || active.contains(&RoutineKey::personal(principal, key.org.clone(), &id).to_string())
            || personal_routine_is_live(home, principal, key, &id)
        {
            continue;
        }
        if let Err(e) = std::fs::remove_dir_all(&path) {
            log::debug!(target: "routines", "could not remove run folder {}: {e}", path.display());
        }
    }
}

/// Remove the run folder of the personal routine `key`, once it is deleted
/// (archived). Team agent workspaces are retained separately. A folder already
/// gone is fine. While
/// the routine has a run queued or running its folder stays (the agent works
/// in it); the next run's sweep removes it after that run ends. A later run,
/// after a restore, makes it again.
pub fn remove_personal_run_dir(home: &Path, principal: &Principal, key: &RoutineKey) {
    if !key.is_personal() {
        return;
    }
    let idle =
        routines_with_active_runs(home).is_some_and(|active| !active.contains(&key.to_string()));
    if !idle {
        return;
    }
    let Some(space) = crate::context_repo::scratch_space::org_space(home, principal, &key.org)
    else {
        return;
    };
    let dir = space.routine_run_dir(&key.id);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => log::warn!(
            target: "routines",
            "could not remove the run folder '{}' of deleted routine {key}: {e}",
            dir.display()
        ),
    }
}

/// The content target of `def`, or why it is unavailable.
pub fn resolve(
    home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    def: &RoutineDef,
) -> Result<PathBuf, String> {
    match def.target.resolve().map_err(|e| e.to_string())? {
        RoutineTarget::TeamWorkspace if key.is_personal() => personal_run_dir(home, principal, key),
        RoutineTarget::TeamWorkspace => {
            let dir = team_content_dir(home, principal, key)?;
            let canon = canonical_dir(&dir, "team workspace")?;
            let registered_root =
                crate::context_repo::context_attachments::registered(home, principal, &key.org)?;
            let in_attachment = registered_root
                .as_ref()
                .is_some_and(|a| canon.starts_with(&a.root));
            if !in_attachment
                && !crate::install::worktree_index::is_codesense_managed_target_canon(&canon, home)
            {
                return Err(format!(
                    "team workspace '{}' is not managed by CodeSense",
                    dir.display()
                ));
            }
            Ok(dir)
        }
        RoutineTarget::Repo(slug) => {
            let parsed = SourceRepoSlug::parse_owner_repo(&slug)
                .ok_or_else(|| format!("repo '{slug}' is not an owner/repo name"))?;
            let root = crate::commands::attachments::read_org_workspace_root_dir(
                home, principal, &key.org,
            );
            let dir = resolve_managed_clone_dir(home, principal, root.as_deref(), &parsed);
            let canon = canonical_dir(&dir, &format!("managed clone of {slug}"))?;
            let under_root = root.and_then(|r| r.canonicalize().ok()).is_some_and(|r| {
                codesense_fs_core::gate::path_within_any_root(&canon, std::slice::from_ref(&r))
            });
            if !under_root
                && !crate::install::worktree_index::is_codesense_managed_target_canon(&canon, home)
            {
                return Err(format!(
                    "'{}' is not a CodeSense-managed clone of {slug}",
                    dir.display()
                ));
            }
            Ok(dir)
        }
    }
}

fn canonical_dir(dir: &Path, what: &str) -> Result<PathBuf, String> {
    if !dir.is_dir() {
        return Err(format!("{what} '{}' is not on this machine", dir.display()));
    }
    dir.canonicalize()
        .map_err(|e| format!("{what} '{}' is unreadable: {e}", dir.display()))
}
