// SPDX-License-Identifier: MIT
//! The one place agents find a directory on disk: a team's agents and content
//! dirs, an org's scratch space's, the list of teams to scan, and the layers a
//! team's content resolves through (its chain hops).
//!
//! Every root goes through the context-repo resolvers (`path_to_org_root`,
//! `synced_contexts_in`, and the scratch-space resolver); nothing here builds a
//! path under the CodeSense home by hand, and nothing else in `agents` calls
//! those resolvers. A personal agent lives in ONE org's scratch space, named by
//! the org and the [`SCRATCH_TEAM`] marker, so it never surfaces as another
//! org's. The numeric local user id is read here alone, as a storage namespace.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::context_repo::scratch_space::{self, is_scratch_team, SCRATCH_TEAM};
use crate::context_repo::{path_to_org_root, path_to_team_config_dir, path_to_team_content_dir};
use crate::domain::Principal;
use crate::routines::PersonalOwner;

use super::defs::agents_dir;
use super::service::AgentError;

/// One team whose agents are scanned.
pub(crate) struct TeamRoot {
    pub org: String,
    pub team: String,
}

/// The numeric local user id that namespaces synced teams on this machine;
/// `None` for a principal that has none.
fn local_user_id(principal: &Principal) -> Option<u64> {
    principal.as_str().parse().ok()
}

/// The local checkout of context repo `org`.
pub(crate) fn org_root(
    home: &Path,
    principal: &Principal,
    org: &str,
    chosen: Option<&super::manifest_resolve::ContextRoot>,
) -> Result<PathBuf, AgentError> {
    if let Some(chosen) = chosen {
        return Ok(chosen.root.clone());
    }
    path_to_org_root(home, principal, org).map_err(AgentError::InvalidLocation)
}

/// The `agents/` dir of `principal`'s scratch space for `org`, or `None`
/// before that space exists.
pub fn personal_agents_dir(
    home: &Path,
    principal: &Principal,
    org: &str,
) -> Result<Option<PathBuf>, AgentError> {
    Ok(scratch_space::org_space(home, principal, org).map(|space| agents_dir(&space.config_dir())))
}

/// Refuses an `owner` other than `principal`: a personal agent is only ever
/// written into the caller's own space.
pub(crate) fn check_personal_owner(
    principal: &Principal,
    owner: &PersonalOwner,
) -> Result<(), AgentError> {
    if local_user_id(principal) == Some(owner.github_user_id) {
        Ok(())
    } else {
        Err(AgentError::InvalidLocation(
            "an Only me agent can only be created in your own private space".into(),
        ))
    }
}

/// The `agents/` dir of `owner`'s scratch space for the owner's org, creating
/// the space on first use. Refuses an owner other than `principal`.
pub(crate) fn create_personal_agents_dir(
    home: &Path,
    principal: &Principal,
    owner: &PersonalOwner,
) -> Result<PathBuf, AgentError> {
    check_personal_owner(principal, owner)?;
    let (space, _created) = scratch_space::ensure_org_space(home, principal, &owner.org)
        .map_err(|e| AgentError::InvalidLocation(e.to_string()))?;
    Ok(agents_dir(&space.config_dir()))
}

/// The `agents/` dir of `org/team` as `principal` sees it (the org's scratch
/// space for the scratch team). `None` when that scratch space does not exist
/// yet.
pub fn team_agents_dir(
    home: &Path,
    principal: &Principal,
    org: &str,
    team: &str,
) -> Result<Option<PathBuf>, AgentError> {
    if is_scratch_team(team) {
        return personal_agents_dir(home, principal, org);
    }
    let org_root = org_root(home, principal, org, None)?;
    Ok(Some(agents_dir(&path_to_team_config_dir(&org_root, team))))
}

/// The `content/` dir of `org/team` as `principal` sees it (the org's scratch
/// space for the scratch team).
pub fn team_content_dir(
    home: &Path,
    principal: &Principal,
    org: &str,
    team: &str,
) -> Result<PathBuf, AgentError> {
    if is_scratch_team(team) {
        return personal_content_dir(home, principal, org);
    }
    team_content_dir_at(home, principal, org, team, None)
}

/// Team-root path math shared by resolution and write destinations. It does not
/// require the folder to exist: the write owner may create it.
pub(crate) fn team_content_dir_at(
    home: &Path,
    principal: &Principal,
    org: &str,
    team: &str,
    chosen: Option<&super::manifest_resolve::ContextRoot>,
) -> Result<PathBuf, AgentError> {
    if let Some(why) = crate::content_engine::validate_org_slug(org)
        .or_else(|| crate::content_engine::validate_path_segment(team))
    {
        return Err(AgentError::InvalidLocation(why.into()));
    }
    Ok(path_to_team_content_dir(
        &org_root(home, principal, org, chosen)?,
        team,
    ))
}

/// The `content/` dir of `principal`'s scratch space for `org`.
pub fn personal_content_dir(
    home: &Path,
    principal: &Principal,
    org: &str,
) -> Result<PathBuf, AgentError> {
    scratch_space::org_space(home, principal, org)
        .map(|space| space.content_dir())
        .ok_or_else(|| {
            AgentError::InvalidLocation(format!(
                "user {principal} has no private space for {org} on this machine"
            ))
        })
}

/// The content root of `principal`'s all-orgs scratch folder: inert items
/// that resolve in every org.
pub fn all_orgs_content_dir(home: &Path, principal: &Principal) -> PathBuf {
    scratch_space::scratch_all_orgs_root(home, principal)
}

/// Whether `org/team` is a team agents may be written to: one the scan
/// visits.
pub(crate) fn is_synced_team(
    home: &Path,
    principal: &Principal,
    org: &str,
    team: &str,
) -> Result<bool, AgentError> {
    Ok(team_roots(home, principal)?
        .iter()
        .any(|(r, _)| r.org == org && r.team == team))
}

/// The active org's scratch space first, then every synced team, each `agents/`
/// dir once.
pub(crate) fn team_roots(
    home: &Path,
    principal: &Principal,
) -> Result<Vec<(TeamRoot, PathBuf)>, AgentError> {
    let Some(user_id) = local_user_id(principal) else {
        return Ok(Vec::new());
    };
    let contexts = crate::commands::route_to_context::synced_contexts_in(home, user_id)
        .map_err(AgentError::Io)?;
    // Viewport: the Agents tab lists only the ACTIVE org's personal agents — and none while
    // the active team has "Use my scratch items" off (SLW-PT-20). Launching never goes
    // through this listing — a personal agent resolves by its key, in its org.
    let scratch_listed = |org: &str| match scratch_space::active_team(home) {
        Some((active_org, team)) if active_org == org => {
            scratch_space::team_uses_scratch(home, principal, org, &team)
        }
        _ => true,
    };
    let mut roots: Vec<TeamRoot> = scratch_space::active_org(home)
        .filter(|org| contexts.iter().any(|c| c.org_slug == *org))
        .filter(|org| scratch_listed(org))
        .map(|org| TeamRoot {
            org,
            team: SCRATCH_TEAM.to_string(),
        })
        .into_iter()
        .collect();
    for ctx in contexts {
        let Some(team) = ctx.team_name else { continue };
        roots.push(TeamRoot {
            org: ctx.org_slug,
            team,
        });
    }
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut out = Vec::new();
    for root in roots {
        let Some(dir) = team_agents_dir(home, principal, &root.org, &root.team)? else {
            continue;
        };
        if seen.insert(dir.clone()) {
            out.push((root, dir));
        }
    }
    Ok(out)
}

pub(crate) fn team_view(
    home: &Path,
    principal: &Principal,
    org: &str,
    team: &str,
    chosen: Option<&super::manifest_resolve::ContextRoot>,
) -> Result<std::sync::Arc<crate::context_repo::inheritance::TeamView>, AgentError> {
    let root = org_root(home, principal, org, chosen)?;
    Ok(crate::context_repo::inheritance::team_view(
        &root, org, team,
    ))
}
