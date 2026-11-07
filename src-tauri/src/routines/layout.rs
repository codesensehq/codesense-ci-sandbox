// SPDX-License-Identifier: MIT
//! Where a routine definition lives on disk (SW-AGR-1): under the agent that
//! runs it.
//!
//! ```text
//! {space}/config/agents/<agent-id>/routines/<id>/                    a team (or personal) agent of the same space
//! {space}/config/agents/_builtin/<starter-id>/routines/<id>/         a built-in starter
//! {scratch}/config/agents/<owner>~<repo>~<team>~<agent-id>/routines/<id>/   a personal routine on a team agent
//! {space}/config/agents/_unassigned/routines/<id>/                   an agent reference no path can name
//! {space}/config/routines/<id>/                                      legacy (pre SW-AGR-1); a tombstone once migrated
//! ```
//!
//! The agent is implied by the path, so a definition stored under its agent
//! writes no `agent:` key; every read through [`super::defs`] fills it from
//! the path ([`implied_agent`]). Only an `_unassigned` or a legacy file keeps
//! it. Each routine dir keeps its own `revisions/`, independent of the
//! agent's. A routine id is unique across its whole space (every agent dir and
//! the legacy dir), so `<org>/<team>/<id>` keys, run handles and the run store
//! need no agent to find a routine.
//!
//! Agent discovery skips every `_`-prefixed dir under `agents/`, and a dir
//! holding only `routines/` has no `agent.yml`, so routines never surface as
//! agents.

use std::path::{Path, PathBuf};

use crate::agents::AgentKey;
use crate::context_repo::scoped_key::BUILTIN_ORG;

use super::model::{AgentRef, RoutineKey};

/// `{space}/config/agents`.
pub const AGENTS_DIR: &str = "agents";
/// The dir under `agents/` that holds every starter's routines.
pub const BUILTIN_DIR: &str = "_builtin";
/// The dir under `agents/` for a routine whose agent reference names no place.
pub const COMMAND_DIR: &str = "_command";
pub const UNASSIGNED_DIR: &str = "_unassigned";
/// `routines/` under an agent dir, and the legacy `config/routines/`.
pub const ROUTINES_DIR: &str = "routines";
const CONFIG_DIR: &str = "config";
/// Separates the parts of a team agent's key in a personal space's agent dir.
const KEY_SEP: char = '~';

/// Where a routine dir sits, by the agent it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Home {
    /// Under `agents/<segment>/`: an agent of the space (`<id>`), or, in a
    /// personal space, a team agent (`<owner>~<repo>~<team>~<id>`).
    Agent(String),
    /// Under `agents/_builtin/<starter-id>/`.
    Builtin(String),
    /// Under `agents/_unassigned/`.
    Unassigned,
    Command,
    /// The legacy `config/routines/<id>/`.
    Legacy,
}

/// The agents dir of a space.
pub fn agents_dir(config_dir: &Path) -> PathBuf {
    config_dir.join(AGENTS_DIR)
}

/// The legacy `{space}/config/routines`.
pub fn legacy_routines_dir(config_dir: &Path) -> PathBuf {
    config_dir.join(ROUTINES_DIR)
}

/// The legacy `{space}/config/routines/<id>`.
pub fn legacy_routine_dir(config_dir: &Path, id: &str) -> PathBuf {
    legacy_routines_dir(config_dir).join(id)
}

/// The `routines/` dir of `home` in the space at `config_dir`.
pub fn home_routines_dir(config_dir: &Path, home: &Home) -> PathBuf {
    let agents = agents_dir(config_dir);
    match home {
        Home::Agent(segment) => agents.join(segment).join(ROUTINES_DIR),
        Home::Builtin(starter) => agents.join(BUILTIN_DIR).join(starter).join(ROUTINES_DIR),
        Home::Command => agents.join(COMMAND_DIR).join(ROUTINES_DIR),
        Home::Unassigned => agents.join(UNASSIGNED_DIR).join(ROUTINES_DIR),
        Home::Legacy => legacy_routines_dir(config_dir),
    }
}

/// The dir routine `id` has in `home`.
pub fn routine_dir_in(config_dir: &Path, home: &Home, id: &str) -> PathBuf {
    home_routines_dir(config_dir, home).join(id)
}

/// The home of a routine at `routine` run by `agent`: its own space's agent
/// dir, the starter dir, or, for a personal routine on a team agent of its
/// org, the full-key dir. Any other pairing (one `AgentRef::key_for` refuses)
/// is `None`.
pub fn home_for(routine: &RoutineKey, agent: &AgentKey) -> Option<Home> {
    if agent.is_builtin() {
        return crate::agents::model::is_valid_id(&agent.id)
            .then(|| Home::Builtin(agent.id.clone()));
    }
    if agent.org == routine.org && agent.team == routine.team && agent.account == routine.account {
        return Some(Home::Agent(agent.id.clone()));
    }
    if routine.is_personal() && !agent.is_personal() && agent.org == routine.org {
        // SW-CR-1: single-segment orgs (e.g. `notes`) are valid slugs now;
        // their flat agent-key dir is `<org>~<team>~<id>` (3 parts) instead of
        // the two-segment `<owner>~<repo>~<team>~<id>` (4 parts).
        let sep = KEY_SEP.to_string();
        let dir = match agent.org.split_once('/') {
            Some((owner, repo)) => [owner, repo, agent.team.as_str(), agent.id.as_str()].join(&sep),
            None => [agent.org.as_str(), agent.team.as_str(), agent.id.as_str()].join(&sep),
        };
        return Some(Home::Agent(dir));
    }
    None
}

/// The home a routine at `routine` naming `agent` belongs in: that agent's,
/// or `_unassigned` when the reference names no agent a path can hold.
pub fn home_for_ref(routine: &RoutineKey, agent: &AgentRef) -> Home {
    agent
        .key_for(routine)
        .ok()
        .and_then(|key| home_for(routine, &key))
        .unwrap_or(Home::Unassigned)
}

pub fn home_for_def(routine: &RoutineKey, def: &super::model::RoutineDef) -> Home {
    if def.is_command() {
        Home::Command
    } else {
        home_for_ref(routine, &def.agent)
    }
}

/// The agent reference a dir under `agents/` stands for, as a routine writes
/// it: the bare id, a team agent's key, or a starter's key.
fn agent_ref_of(home: &Home) -> Option<AgentRef> {
    match home {
        Home::Agent(segment) if segment.contains(KEY_SEP) => {
            let parts: Vec<&str> = segment.split(KEY_SEP).collect();
            match parts.as_slice() {
                // Two-segment org: `<owner>~<repo>~<team>~<id>`.
                [owner, repo, team, id] => {
                    Some(AgentRef::new(format!("{owner}/{repo}/{team}/{id}")))
                }
                // SW-CR-1 single-segment org: `<org>~<team>~<id>`.
                [org, team, id] => Some(AgentRef::new(format!("{org}/{team}/{id}"))),
                _ => None,
            }
        }
        Home::Agent(id) => Some(AgentRef::new(id.as_str())),
        Home::Builtin(starter) => Some(AgentRef::new(format!("{BUILTIN_ORG}/{starter}"))),
        Home::Command | Home::Unassigned | Home::Legacy => None,
    }
}

fn name_of(path: &Path) -> Option<&str> {
    path.file_name().and_then(|n| n.to_str())
}

/// The home of the routine dir `dir`, read from its path alone: the dir's
/// parent is `routines/`, under an agent dir of `{x}/config/agents/`, under
/// `_builtin/`, under `_unassigned`, or directly under `{x}/config/`.
pub fn home_of_dir(dir: &Path) -> Option<Home> {
    let routines = dir.parent()?;
    if name_of(routines)? != ROUTINES_DIR {
        return None;
    }
    let owner = routines.parent()?;
    let owner_name = name_of(owner)?;
    let above = owner.parent()?;
    let above_name = name_of(above)?;
    if above_name == AGENTS_DIR && above.parent().and_then(name_of) == Some(CONFIG_DIR) {
        return Some(match owner_name {
            COMMAND_DIR => Home::Command,
            UNASSIGNED_DIR => Home::Unassigned,
            name if name.starts_with('_') => return None,
            name => Home::Agent(name.to_string()),
        });
    }
    if above_name == BUILTIN_DIR
        && above.parent().and_then(name_of) == Some(AGENTS_DIR)
        && above.parent()?.parent().and_then(name_of) == Some(CONFIG_DIR)
    {
        return Some(Home::Builtin(owner_name.to_string()));
    }
    (owner_name == CONFIG_DIR).then_some(Home::Legacy)
}

/// The agent the path of routine dir `dir` names, which every read fills in;
/// `None` for a legacy or `_unassigned` dir, whose file says it.
pub fn implied_agent(dir: &Path) -> Option<AgentRef> {
    agent_ref_of(&home_of_dir(dir)?)
}

/// The `config/` dir a routine dir sits in.
pub fn config_dir_of(dir: &Path) -> Option<PathBuf> {
    let routines = dir.parent()?;
    let config = match home_of_dir(dir)? {
        Home::Legacy => routines.parent()?,
        Home::Agent(_) | Home::Unassigned | Home::Command => {
            routines.parent()?.parent()?.parent()?
        }
        Home::Builtin(_) => routines.parent()?.parent()?.parent()?.parent()?,
    };
    Some(config.to_path_buf())
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    dirs.sort();
    dirs
}

/// Every `routines/` dir under the space's agents, sorted: each agent's, each
/// starter's, then `_unassigned`'s. Other `_`-prefixed dirs are skipped.
pub fn agent_routines_dirs(config_dir: &Path) -> Vec<(Home, PathBuf)> {
    let mut out = Vec::new();
    let mut unassigned = None;
    for agent_dir in subdirs(&agents_dir(config_dir)) {
        let Some(name) = name_of(&agent_dir).map(str::to_string) else {
            continue;
        };
        match name.as_str() {
            BUILTIN_DIR => {
                for starter in subdirs(&agent_dir) {
                    if let Some(starter_name) = name_of(&starter) {
                        out.push((
                            Home::Builtin(starter_name.to_string()),
                            starter.join(ROUTINES_DIR),
                        ));
                    }
                }
            }
            COMMAND_DIR => out.push((Home::Command, agent_dir.join(ROUTINES_DIR))),
            UNASSIGNED_DIR => unassigned = Some(agent_dir.join(ROUTINES_DIR)),
            _ if name.starts_with('_') => {}
            _ => out.push((Home::Agent(name), agent_dir.join(ROUTINES_DIR))),
        }
    }
    if let Some(dir) = unassigned {
        out.push((Home::Unassigned, dir));
    }
    out.retain(|(_, dir)| dir.is_dir());
    out
}

/// The dir routine `id` lives in under the space's agents, when it has one.
/// A routine id is unique across the space, so there is at most one; should a
/// sync bring two, the first in [`agent_routines_dirs`] order wins.
pub fn find_in_agents(config_dir: &Path, id: &str) -> Option<PathBuf> {
    agent_routines_dirs(config_dir)
        .into_iter()
        .map(|(_, routines)| routines.join(id))
        .find(|dir| dir.join(super::defs::ROUTINE_FILE).is_file())
}

/// Every routine dir under the space's agents, with its home, sorted by home
/// then id. Dirs with no `routine.yml` are left out.
pub fn scan_agents(config_dir: &Path) -> Vec<(Home, PathBuf)> {
    let mut out = Vec::new();
    for (home, routines) in agent_routines_dirs(config_dir) {
        for dir in subdirs(&routines) {
            if dir.join(super::defs::ROUTINE_FILE).is_file() {
                out.push((home.clone(), dir));
            }
        }
    }
    out
}

/// Whether routine id `id` is taken anywhere in the space: under any agent or
/// in the legacy dir (a routine an older client still writes there, or a
/// tombstone holding its id for one release).
pub fn id_taken(config_dir: &Path, id: &str) -> bool {
    find_in_agents(config_dir, id).is_some()
        || legacy_routine_dir(config_dir, id).exists()
        || agent_routines_dirs(config_dir)
            .iter()
            .any(|(_, routines)| routines.join(id).exists())
}

/// `base`, then `base-2`, `base-3`, … : the first routine id not taken
/// anywhere in the space.
pub fn first_free_id(config_dir: &Path, base: &str) -> String {
    if !id_taken(config_dir, base) {
        return base.to_string();
    }
    (2u32..)
        .map(|n| format!("{base}-{n}"))
        .find(|c| !id_taken(config_dir, c))
        .unwrap_or_else(|| base.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Principal;

    fn team_key(id: &str) -> RoutineKey {
        RoutineKey::new("acme/api", "backend", id)
    }

    #[test]
    fn a_routine_lives_under_its_agent_its_starter_or_the_full_key_dir() {
        let routine = team_key("triage");
        assert_eq!(
            home_for(&routine, &AgentKey::new("acme/api", "backend", "barb")),
            Some(Home::Agent("barb".into()))
        );
        assert_eq!(
            home_for(&routine, &AgentKey::builtin("claude-unattended")),
            Some(Home::Builtin("claude-unattended".into()))
        );
        let alice = Principal::from(1u64);
        let personal = RoutineKey::personal(&alice, "acme/api", "journal");
        assert_eq!(
            home_for(&personal, &AgentKey::new("acme/api", "backend", "barb")),
            Some(Home::Agent("acme~api~backend~barb".into()))
        );
        assert_eq!(
            home_for(&personal, &AgentKey::personal(&alice, "acme/api", "mine")),
            Some(Home::Agent("mine".into()))
        );
        // Another team's agent has no place under a team routine's space.
        assert_eq!(
            home_for(&routine, &AgentKey::new("acme/api", "frontend", "barb")),
            None
        );
        assert_eq!(
            home_for_ref(&routine, &AgentRef::new("acme/api/frontend/barb")),
            Home::Unassigned
        );
    }

    // SW-CR-1: a personal routine on a team agent whose org is a single
    // segment (now a valid slug) is homed under a 3-part flat key dir
    // `<org>~<team>~<id>`, not misrouted to `_unassigned`.
    #[test]
    fn a_personal_routine_on_a_single_segment_org_team_agent_homes_under_flat_key() {
        let alice = Principal::from(1u64);
        let personal = RoutineKey::personal(&alice, "notes", "journal");
        assert_eq!(
            home_for(&personal, &AgentKey::new("notes", "backend", "barb")),
            Some(Home::Agent("notes~backend~barb".into()))
        );
        // The round-trip reverse-reads the 3-part dir name as `<org>/<team>/<id>`.
        assert_eq!(
            home_for_ref(&personal, &AgentRef::new("notes/backend/barb")),
            Home::Agent("notes~backend~barb".into())
        );
    }

    #[test]
    fn the_path_names_the_agent_and_round_trips() {
        let config = Path::new("/h/orgs/acme/api/backend/config");
        let cases = [
            (Home::Agent("barb".into()), Some("barb")),
            (Home::Builtin("claude".into()), Some("builtin/claude")),
            (
                Home::Agent("acme~api~backend~barb".into()),
                Some("acme/api/backend/barb"),
            ),
            // SW-CR-1: 3-part flat key for a single-segment org routes back
            // to the `<org>/<team>/<id>` agent ref.
            (
                Home::Agent("notes~backend~barb".into()),
                Some("notes/backend/barb"),
            ),
            (Home::Unassigned, None),
            (Home::Legacy, None),
        ];
        for (home, agent) in cases {
            let dir = routine_dir_in(config, &home, "triage");
            assert_eq!(home_of_dir(&dir), Some(home.clone()), "{}", dir.display());
            assert_eq!(
                implied_agent(&dir).map(|a| a.to_string()).as_deref(),
                agent,
                "{}",
                dir.display()
            );
            assert_eq!(config_dir_of(&dir).as_deref(), Some(config));
        }
        // Other `_` dirs under agents/ are no agent.
        assert_eq!(
            home_of_dir(&config.join("agents/_trash/routines/triage")),
            None
        );
    }
}
