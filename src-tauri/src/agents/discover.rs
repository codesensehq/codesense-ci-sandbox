// SPDX-License-Identifier: MIT
//! Find every agent the user can see: each synced org's scratch space plus
//! every locally-synced team, independent of which team is active in the UI.
//! A personal agent is keyed by the org whose scratch space holds it.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::context_repo::scratch_space::is_scratch_team;
use crate::domain::Principal;
use crate::routines::discover::OwnerNames;

use super::defs;
use super::keys::AgentKey;
use super::model::{is_valid_id, Agent};
use super::roots;
use super::service::AgentError;

#[cfg(test)]
mod caller_scope_tests;

/// Whose space an agent lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentScope {
    Personal,
    Team,
    /// A starter the app ships; read-only.
    Builtin,
}

impl AgentScope {
    pub(crate) fn of_team(team: &str) -> Self {
        if is_scratch_team(team) {
            Self::Personal
        } else {
            Self::Team
        }
    }
}

/// One agent found on disk, tagged for display.
#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredAgent {
    pub key: AgentKey,
    pub scope: AgentScope,
    pub org: String,
    pub team: String,
    /// The creator's display name from the org's people registry, falling back
    /// to the raw `created_by`. Display only.
    pub created_by_display: String,
    #[serde(skip)]
    pub dir: PathBuf,
    pub agent: Agent,
}

/// Every agent visible to `principal`, in a stable order (personal first, then
/// teams as discovered, agents by id within a team). Archived agents are left
/// out unless `include_archived`. Malformed definitions are logged and skipped
/// so one bad file never hides the rest.
pub fn discover_agents(
    home: &Path,
    principal: &Principal,
    include_archived: bool,
) -> Result<Vec<DiscoveredAgent>, AgentError> {
    discover_agents_for(home, principal, include_archived, &Default::default())
}

pub(crate) fn discover_agents_for(
    home: &Path,
    principal: &Principal,
    include_archived: bool,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<Vec<DiscoveredAgent>, AgentError> {
    let mut names = OwnerNames::default();
    let mut out = Vec::new();
    for (root, agents_dir) in roots::team_roots(home, principal)? {
        if !scope.allows(&agents_dir) {
            continue;
        }
        let org_root = roots::org_root(home, principal, &root.org, None)?;
        for (dir, agent) in defs::scan(&agents_dir) {
            if !scope.allows(&dir.join(defs::AGENT_FILE)) {
                continue;
            }
            let agent = match agent {
                Ok(agent) => agent,
                Err(e) => {
                    log::warn!(target: "agents", "skipping unreadable agent: {e}");
                    continue;
                }
            };
            let dir_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if agent.id != dir_name || !is_valid_id(&agent.id) {
                log::warn!(
                    target: "agents",
                    "skipping agent at {}: id {:?} is invalid or does not match its directory",
                    dir.display(),
                    agent.id
                );
                continue;
            }
            if agent.is_archived() && !include_archived {
                continue;
            }
            out.push(DiscoveredAgent {
                key: AgentKey::scoped(principal, &root.org, &root.team, &agent.id),
                scope: AgentScope::of_team(&root.team),
                org: root.org.clone(),
                team: root.team.clone(),
                created_by_display: names.display(&root.org, &org_root, &agent.created_by),
                dir,
                agent,
            });
        }
    }
    Ok(out)
}
