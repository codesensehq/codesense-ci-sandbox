// SPDX-License-Identifier: MIT
//! Find every routine the signed-in user can see, across all their teams.
//!
//! The scan is the union of every locally-synced org/team and every synced org's
//! scratch space, independent of which team is active in the UI: CodeSense is a
//! control plane, so what is on screen never gates what fires. A personal routine
//! is filed under one org and only ever acts inside that org. The Routines tab is
//! a viewport and lists only the active org's personal routines
//! ([`discover_routines_in_view`]).
//!
//! In each space it reads every routine under its agents ([`super::layout`]),
//! plus any legacy `config/routines/<id>` not yet migrated (a read-only docs
//! context, or one written by an older client since the last migration pass)
//! whose id no agent dir holds. A tombstone (`moved_to`) is never listed: its
//! routine is listed where it moved. A routine whose agent this Mac cannot
//! find is listed `unassigned`, and nothing fires it (SW-AGR-1).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;

use crate::context_repo::scratch_space::{self, is_scratch_team, SCRATCH_TEAM};
use crate::context_repo::{path_to_org_root, path_to_team_config_dir};
use crate::domain::Principal;

#[cfg(test)]
mod caller_scope_tests;

use crate::agents::AgentKey;

use super::defs;
use super::host::{is_armed_here, local_host_id};
use super::layout::{self, Home};
use super::migrate::{self, MOVED_TO};
use super::model::{RoutineDef, RoutineKey};
use super::service::RoutineError;

/// Whose space a routine lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutineScope {
    Personal,
    Team,
}

impl RoutineScope {
    pub(crate) fn of_team(team: &str) -> Self {
        if is_scratch_team(team) {
            Self::Personal
        } else {
            Self::Team
        }
    }
}

/// One non-archived routine found on disk, tagged for display.
#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredRoutine {
    pub key: RoutineKey,
    pub scope: RoutineScope,
    /// Whether this Mac is a place it runs: a shared job armed on this host,
    /// or a recipe each Mac runs. Whether it fires here is then this Mac's
    /// switch (`state::enabled_here`).
    pub armed_here: bool,
    /// The owner's display name from the org's people registry, falling back to
    /// the raw principal string.
    pub owner_display: String,
    /// The agent the routine belongs to (the one its path names), when its
    /// reference reads as one.
    pub agent_key: Option<AgentKey>,
    /// Its agent is nowhere this Mac can see (deleted, or a team this Mac does
    /// not sync): it is listed under "Unassigned" until someone picks an
    /// agent, and nothing fires it.
    pub unassigned: bool,
    /// Another routine of its space has the same id (a sync brought two):
    /// the first in [`layout::agent_routines_dirs`] order owns the key and
    /// this one is listed as a duplicate, which nothing fires.
    pub duplicate: bool,
    #[serde(skip)]
    pub dir: PathBuf,
    pub def: RoutineDef,
}

/// One team (or org scratch space) whose `config/routines/` is scanned.
struct TeamRoot {
    org: String,
    team: String,
    org_root: PathBuf,
    config_dir: PathBuf,
}

/// Every synced org's scratch space first, then every synced team.
fn team_roots(codesense_home: &Path, principal: &Principal) -> Result<Vec<TeamRoot>, RoutineError> {
    // A personal routine's key carries the numeric account id, and synced
    // teams are keyed by it, so a principal that is not one has nothing to scan.
    let Ok(user_id) = principal.as_str().parse::<u64>() else {
        return Ok(Vec::new());
    };
    let contexts = crate::commands::route_to_context::synced_contexts_in(codesense_home, user_id)
        .map_err(RoutineError::Io)?;
    // Every synced org's scratch space: a personal routine runs (scheduler, by hand)
    // in ITS org's context whenever that org is on this Mac, whatever org is on
    // screen. Only the Routines tab narrows to the active org
    // ([`discover_routines_in_view`]).
    let orgs: std::collections::BTreeSet<&str> =
        contexts.iter().map(|c| c.org_slug.as_str()).collect();
    let mut roots = Vec::new();
    for org in orgs {
        if let Some(space) = scratch_space::org_space(codesense_home, principal, org) {
            roots.push(TeamRoot {
                org: org.to_string(),
                team: SCRATCH_TEAM.to_string(),
                org_root: path_to_org_root(codesense_home, principal, org)
                    .map_err(RoutineError::Io)?,
                config_dir: space.config_dir(),
            });
        }
    }
    for ctx in &contexts {
        let Some(team) = ctx.team_name.clone() else {
            continue;
        };
        let org_root =
            path_to_org_root(codesense_home, principal, &ctx.org_slug).map_err(RoutineError::Io)?;
        roots.push(TeamRoot {
            org: ctx.org_slug.clone(),
            config_dir: path_to_team_config_dir(&org_root, &team),
            team,
            org_root,
        });
    }
    let mut seen: HashSet<PathBuf> = HashSet::new();
    roots.retain(|r| seen.insert(r.config_dir.clone()));
    Ok(roots)
}

/// [`discover_routines`] narrowed for DISPLAY (the Routines tab): personal
/// routines of the active org only. Viewport only — never used to decide what
/// fires or runs.
pub fn discover_routines_in_view(
    codesense_home: &Path,
    principal: &Principal,
) -> Result<Vec<DiscoveredRoutine>, RoutineError> {
    let active = scratch_space::active_org(codesense_home);
    Ok(discover_routines(codesense_home, principal)?
        .into_iter()
        .filter(|r| !r.key.is_personal() || active.as_deref() == Some(r.key.org.as_str()))
        .collect())
}

/// Every non-archived routine visible to `principal`, in a stable order
/// (personal first, then teams as discovered, routines by home then id within
/// a team). Malformed definitions are logged and skipped so one bad file never
/// hides the rest.
pub fn discover_routines(
    codesense_home: &Path,
    principal: &Principal,
) -> Result<Vec<DiscoveredRoutine>, RoutineError> {
    discover_routines_with(codesense_home, principal, false)
}

/// Bring every legacy routine of every space `principal` can see under its
/// agent ([`migrate::migrate_space`]). Cheap once done: one stat per space
/// without a legacy dir, one read per tombstone. Never called under a routine
/// lock: it takes the legacy dir's own lock and, to re-apply an edit, a
/// migrated routine's.
pub fn migrate_all(codesense_home: &Path, principal: &Principal) -> Result<(), RoutineError> {
    migrate_all_for(codesense_home, principal, &Default::default())
}

pub(crate) fn migrate_all_for(
    codesense_home: &Path,
    principal: &Principal,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<(), RoutineError> {
    for root in team_roots(codesense_home, principal)? {
        if !scope.allows(&root.config_dir) {
            continue;
        }
        migrate::migrate_space(
            codesense_home,
            principal,
            &root.org,
            &root.team,
            &root.config_dir,
        );
    }
    Ok(())
}

/// The routine dirs of one space, with the home each sits in: every one
/// under an agent, then each legacy one that is neither a tombstone nor
/// shadowed by an id an agent dir holds.
pub(crate) fn space_routine_dirs(
    config_dir: &Path,
) -> Vec<(Home, PathBuf, Result<RoutineDef, defs::DefsError>)> {
    let mut out: Vec<(Home, PathBuf, Result<RoutineDef, defs::DefsError>)> = Vec::new();
    let mut ids = HashSet::new();
    for (home, dir) in layout::scan_agents(config_dir) {
        let def = defs::read_def(&dir).and_then(|def| {
            def.ok_or_else(|| defs::DefsError::Io {
                path: dir.clone(),
                source: std::io::Error::from(std::io::ErrorKind::NotFound),
            })
        });
        if let Some(id) = dir.file_name().and_then(|n| n.to_str()) {
            ids.insert(id.to_string());
        }
        out.push((home, dir, def));
    }
    for (dir, def) in defs::scan(&layout::legacy_routines_dir(config_dir)) {
        let id = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if ids.contains(id) {
            continue;
        }
        if matches!(&def, Ok(def) if def.extra.contains_key(MOVED_TO)) {
            continue;
        }
        out.push((Home::Legacy, dir, def));
    }
    out
}

/// [`discover_routines`], archived routines included when `include_archived`.
pub fn discover_routines_with(
    codesense_home: &Path,
    principal: &Principal,
    include_archived: bool,
) -> Result<Vec<DiscoveredRoutine>, RoutineError> {
    discover_routines_for(
        codesense_home,
        principal,
        include_archived,
        &Default::default(),
    )
}

pub(crate) fn discover_routines_for(
    codesense_home: &Path,
    principal: &Principal,
    include_archived: bool,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<Vec<DiscoveredRoutine>, RoutineError> {
    // Read (and on first use, written) only once a routine is found: a scan
    // that finds none leaves no trace.
    let mut host: Option<String> = None;
    let mut owners = OwnerNames::default();
    let mut agents = AgentPresence::default();
    let mut out = Vec::new();
    for root in team_roots(codesense_home, principal)? {
        if !scope.allows(&root.config_dir) {
            continue;
        }
        // The first dir of an id owns its key, as `layout::find_in_agents`
        // resolves it; archived ones count, as they hold their id too.
        let mut ids: HashSet<String> = HashSet::new();
        for (_, dir, def) in space_routine_dirs(&root.config_dir) {
            if !scope.allows(&dir.join("routine.yml")) {
                continue;
            }
            let def = match def {
                Ok(def) => def,
                Err(e) => {
                    log::warn!(target: "routines", "skipping unreadable routine: {e}");
                    continue;
                }
            };
            let dir_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if def.id != dir_name {
                log::warn!(
                    target: "routines",
                    "skipping routine at {}: id {:?} does not match its directory",
                    dir.display(),
                    def.id
                );
                continue;
            }
            let duplicate = !ids.insert(def.id.clone());
            if duplicate {
                log::warn!(
                    target: "routines",
                    "routine at {} has the id of another routine of its space: listed as a duplicate, never fired",
                    dir.display()
                );
            }
            if def.is_archived() && !include_archived {
                continue;
            }
            let key = RoutineKey::scoped(principal, &root.org, &root.team, &def.id);
            let agent_key = def.agent.key_for(&key).ok();
            let unassigned = !def.is_command()
                && match &agent_key {
                    Some(agent) => {
                        !scope.definition(codesense_home, principal, agent)
                            || !agents.exists(codesense_home, principal, agent)
                    }
                    None => true,
                };
            let host = match &host {
                Some(host) => host,
                None => host.insert(
                    local_host_id(codesense_home).map_err(|e| RoutineError::Io(e.to_string()))?,
                ),
            };
            out.push(DiscoveredRoutine {
                scope: RoutineScope::of_team(&root.team),
                armed_here: is_armed_here(&def, host),
                owner_display: owners.display(&root.org, &root.org_root, &def.owner),
                agent_key,
                unassigned,
                duplicate,
                key,
                dir,
                def,
            });
        }
    }
    Ok(out)
}

/// Whether each agent a discovery meets exists here, read once per agent.
#[derive(Default)]
struct AgentPresence {
    known: HashMap<AgentKey, bool>,
}

impl AgentPresence {
    fn exists(&mut self, home: &Path, principal: &Principal, agent: &AgentKey) -> bool {
        *self
            .known
            .entry(agent.clone())
            .or_insert_with(|| agent_is_here(home, principal, agent))
    }
}

/// Whether `agent` is an agent `principal` can see on this Mac, archived or
/// not: a starter, or one whose definition reads.
/// Only an agent that is not there makes a routine Unassigned: one that is
/// there but does not read (a malformed file, a newer build's shape) keeps it
/// firing, and each run fails visibly (`agent_unavailable`).
pub(crate) fn agent_is_here(home: &Path, principal: &Principal, agent: &AgentKey) -> bool {
    if agent.is_builtin() {
        return crate::agents::builtin::is_starter(agent);
    }
    !matches!(
        crate::agents::service::read_current(home, principal, agent),
        Err(crate::agents::AgentError::NotFound(_))
    )
}

impl DiscoveredRoutine {
    /// Nothing fires it: its agent is not here, or another routine owns its id.
    pub fn held(&self) -> bool {
        self.unassigned || self.duplicate
    }
}

/// Per CodeSense home, bumped by every definition write. The scheduler caches
/// discovery and rescans when this moves, so a local edit is picked up on the
/// next tick rather than after the cache's lifetime.
static DISCOVERY_GENERATIONS: Mutex<BTreeMap<PathBuf, u64>> = Mutex::new(BTreeMap::new());

fn generations() -> std::sync::MutexGuard<'static, BTreeMap<PathBuf, u64>> {
    DISCOVERY_GENERATIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Mark every cached discovery of `codesense_home` stale.
pub(crate) fn invalidate_discovery_cache(codesense_home: &Path) {
    *generations()
        .entry(codesense_home.to_path_buf())
        .or_default() += 1;
}

/// The current discovery generation of `codesense_home`; a cache filled under
/// an older one is stale.
pub(crate) fn discovery_generation(codesense_home: &Path) -> u64 {
    generations().get(codesense_home).copied().unwrap_or(0)
}

/// Owner principal → display name, one people-registry load per org.
#[derive(Default)]
pub(crate) struct OwnerNames {
    by_org: HashMap<String, HashMap<String, String>>,
}

impl OwnerNames {
    pub(crate) fn display(&mut self, org: &str, org_root: &Path, owner: &str) -> String {
        let names = self.by_org.entry(org.to_string()).or_insert_with(|| {
            crate::domain::load_org(org.to_string(), org_root, None)
                .org
                .map(|o| {
                    o.people
                        .iter()
                        .filter_map(|p| {
                            Some((
                                p.github_user_id()?.to_string(),
                                p.display_name_or_username().to_string(),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default()
        });
        names
            .get(owner)
            .cloned()
            .unwrap_or_else(|| owner.to_string())
    }
}
