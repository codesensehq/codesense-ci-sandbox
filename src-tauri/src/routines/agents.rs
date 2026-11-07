// SPDX-License-Identifier: MIT
//! How routines use agents: which routines reference an agent (so it is not
//! deleted out from under them), how a routine's agent reads in a list, and
//! the warnings a routine's task gets against its agent on save.
//!
//! Everything here is read-only and advisory. A routine whose agent no longer
//! resolves is still listed and still fires; its runs fail `agent_unavailable`
//! (see [`super::dispatch`]).

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;

use crate::agents::adapter::LaunchMode;
use crate::agents::launch::{self, AgentLaunch, PrepareOptions};
use crate::agents::launch_check::{LaunchWarning, LaunchWarningCode};
use crate::agents::{AgentKey, AgentReferrers};
use crate::domain::Principal;

use super::discover::discover_routines;
use super::model::{AgentRef, RoutineDef, RoutineKey, RoutineTrigger};

/// The routines that reference an agent: as their runner or as the agent an
/// `agent_done` trigger waits for. Only routines the caller can see count, so
/// a teammate's personal routine is invisible here and can be stranded by a
/// delete; it then fails each run visibly.
pub struct RoutineReferrers;

pub(crate) struct CallerRoutineReferrers<'a>(
    pub &'a crate::context_repo::caller_scope::CallerScope,
);

impl AgentReferrers for RoutineReferrers {
    fn referrers(&self, home: &Path, principal: &Principal, key: &AgentKey) -> Vec<String> {
        CallerRoutineReferrers(&Default::default()).referrers(home, principal, key)
    }
    fn owned(&self, home: &Path, principal: &Principal, key: &AgentKey) -> Vec<String> {
        CallerRoutineReferrers(&Default::default()).owned(home, principal, key)
    }
}

impl AgentReferrers for CallerRoutineReferrers<'_> {
    fn referrers(&self, home: &Path, principal: &Principal, key: &AgentKey) -> Vec<String> {
        let routines = match super::discover::discover_routines_for(home, principal, false, self.0)
        {
            Ok(routines) => routines,
            Err(e) => {
                // Refuse a delete rather than strand routines nobody could list.
                log::warn!(target: "routines", "routines unreadable while checking agent {key}: {e}");
                return vec![format!("routines could not be read: {e}")];
            }
        };
        routines
            .into_iter()
            .filter(|r| names_agent(&r.key, &r.def, key))
            .map(|r| r.key.to_string())
            .collect()
    }

    fn owned(&self, home: &Path, principal: &Principal, key: &AgentKey) -> Vec<String> {
        super::discover::discover_routines_for(home, principal, false, self.0)
            .map(|routines| {
                routines
                    .into_iter()
                    .filter(|r| r.agent_key.as_ref() == Some(key))
                    .map(|r| r.key.to_string())
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl RoutineReferrers {
    /// Every agent the routines `principal` can see name, each with the
    /// routines naming it (sorted): one read of the routines for a whole list
    /// of agents, counted the same way a delete counts them.
    pub fn by_agent(
        home: &Path,
        principal: &Principal,
    ) -> Result<HashMap<AgentKey, Vec<String>>, String> {
        let mut index: HashMap<AgentKey, Vec<String>> = HashMap::new();
        for routine in discover_routines(home, principal).map_err(|e| e.to_string())? {
            let routine_key = routine.key.to_string();
            for agent in named_agents(&routine.key, &routine.def) {
                index.entry(agent).or_default().push(routine_key.clone());
            }
        }
        for routines in index.values_mut() {
            routines.sort();
        }
        Ok(index)
    }
}

/// Every agent routine `routine` names, once each: its runner and the agents
/// its `agent_done` triggers wait for.
fn named_agents(routine: &RoutineKey, def: &RoutineDef) -> Vec<AgentKey> {
    let mut named: Vec<AgentKey> = std::iter::once(&def.agent)
        .chain(def.triggers.iter().filter_map(|t| match t {
            RoutineTrigger::Event(spec) => spec.from_agent.as_ref(),
            RoutineTrigger::Schedule(_) => None,
        }))
        .filter_map(|r: &AgentRef| r.named_key(routine).ok())
        .collect();
    named.sort_by_key(|k| k.to_string());
    named.dedup();
    named
}

/// Whether routine `routine` names `agent` anywhere.
fn names_agent(routine: &RoutineKey, def: &RoutineDef, agent: &AgentKey) -> bool {
    named_agents(routine, def).contains(agent)
}

/// A routine's agent as the Routines tab shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentDisplay {
    /// The agent's name, or the reference as written when it does not
    /// resolve.
    pub name: String,
    pub archived: bool,
    /// False when no agent the caller can see answers to the reference: the
    /// routine's runs fail `agent_unavailable` until it is fixed.
    pub resolvable: bool,
}

/// The agents a listing has read, by key: each agent file is read once
/// however many routines name it.
pub type DisplayCache = HashMap<AgentKey, Option<(String, bool)>>;

/// How routine `routine`'s agent reads for `principal`.
pub fn display(
    home: &Path,
    principal: &Principal,
    routine: &RoutineKey,
    def: &RoutineDef,
) -> AgentDisplay {
    display_cached(home, principal, routine, def, &mut DisplayCache::new())
}

/// [`display`], reading each agent once per `cache`.
pub fn display_cached(
    home: &Path,
    principal: &Principal,
    routine: &RoutineKey,
    def: &RoutineDef,
    cache: &mut DisplayCache,
) -> AgentDisplay {
    display_cached_for(home, principal, routine, def, cache, &Default::default())
}

pub(crate) fn display_cached_for(
    home: &Path,
    principal: &Principal,
    routine: &RoutineKey,
    def: &RoutineDef,
    cache: &mut DisplayCache,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> AgentDisplay {
    let agent = def
        .agent
        .key_for(routine)
        .ok()
        .filter(|key| scope.definition(home, principal, key))
        .and_then(|key| {
            cache
                .entry(key)
                .or_insert_with_key(|key| {
                    crate::agents::service::read_current(home, principal, key)
                        .ok()
                        .map(|agent| (agent.name.clone(), agent.is_archived()))
                })
                .clone()
        });
    match agent {
        Some((name, archived)) => AgentDisplay {
            name,
            archived,
            resolvable: true,
        },
        None => AgentDisplay {
            name: def.agent.to_string(),
            archived: false,
            resolvable: false,
        },
    }
}

/// Why the agent `agent` of routine `routine` could not be read, saying where
/// it lives when that is a team this Mac does not have: a team agent is only
/// there to run on a Mac that syncs its team.
fn unreadable(
    home: &Path,
    principal: &Principal,
    routine: &RoutineKey,
    agent: &AgentKey,
    error: impl std::fmt::Display,
) -> String {
    let other_team =
        !agent.is_personal() && (agent.org != routine.org || agent.team != routine.team);
    let team_here = || {
        crate::agents::roots::team_content_dir(home, principal, &agent.org, &agent.team)
            .is_ok_and(|dir| dir.is_dir())
    };
    if other_team && !team_here() {
        format!(
            "{error}: this agent lives in team {}/{}, which this Mac may not have",
            agent.org, agent.team
        )
    } else {
        error.to_string()
    }
}

/// The key of the agent `agent` names for routine `routine`, when
/// `principal` can see it; otherwise the refusal to show, which points at
/// `agent_list`.
pub fn visible_agent(
    home: &Path,
    principal: &Principal,
    routine: &RoutineKey,
    agent: &AgentRef,
) -> Result<AgentKey, String> {
    let key = agent.key_for(routine).map_err(|e| e.to_string())?;
    match crate::agents::service::read_current(home, principal, &key) {
        Ok(_) => Ok(key),
        Err(crate::agents::AgentError::NotFound(_)) => Err(unreadable(
            home,
            principal,
            routine,
            &key,
            format_args!(
                "no agent {:?} that you can see; see agent_list",
                agent.as_str()
            ),
        )),
        Err(e) => Err(unreadable(
            home,
            principal,
            routine,
            &key,
            format_args!("agent {key} cannot be read: {e}"),
        )),
    }
}

/// Routine `routine`'s agent prepared for a run of `prompt` by `principal`:
/// in the mode the routine's `visible` decides, whatever the agent declares,
/// against the routine's own space. The error says why it does not resolve.
pub fn prepare_run(
    home: &Path,
    principal: &Principal,
    routine: &RoutineKey,
    def: &RoutineDef,
    prompt: &str,
) -> Result<AgentLaunch, String> {
    prepare_run_for(home, principal, routine, def, prompt, &Default::default())
}

pub(crate) fn prepare_run_for(
    home: &Path,
    principal: &Principal,
    routine: &RoutineKey,
    def: &RoutineDef,
    prompt: &str,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<AgentLaunch, String> {
    let agent = def
        .agent
        .key_for(routine)
        .map_err(|e| format!("agent {}: {e}", def.agent))?;
    scope.require_agent(home, principal, &agent)?;
    let mode = if def.policy.visible {
        LaunchMode::Interactive
    } else {
        LaunchMode::Headless
    };
    launch::prepare_with(
        scope,
        home,
        principal,
        &agent,
        PrepareOptions {
            account: None,
            target_team: Some((routine.org.clone(), routine.team.clone())),
            mode_override: Some(mode),
            prompt: Some(prompt),
        },
    )
    .map_err(|e| {
        unreadable(
            home,
            principal,
            routine,
            &agent,
            format_args!("agent {agent}: {e}"),
        )
    })
}

/// What a run of routine `routine` would be warned about, for its task
/// against its agent in the mode the routine runs in (a leading `/command`
/// the agent does not start with, among others), and that its agent does not
/// resolve when it does not. Never a refusal.
pub fn save_warnings(
    home: &Path,
    principal: &Principal,
    routine: &RoutineKey,
    def: &RoutineDef,
) -> Vec<LaunchWarning> {
    save_warnings_for(home, principal, routine, def, &Default::default())
}

pub(crate) fn save_warnings_for(
    home: &Path,
    principal: &Principal,
    routine: &RoutineKey,
    def: &RoutineDef,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Vec<LaunchWarning> {
    if def.is_command() {
        return Vec::new();
    }
    let mut warnings = match prepare_run_for(home, principal, routine, def, &def.task, scope) {
        Ok(prepared) => prepared.warnings,
        Err(why) => vec![LaunchWarning {
            code: LaunchWarningCode::AgentUnavailable,
            message: format!("{why} — its runs will fail until it names an agent this Mac has"),
        }],
    };
    // SW-AGR-2 (§9.5): a wake into an agent that skips permission checks needs
    // its consent.
    if def.mode() == Ok(super::model::RoutineMode::Wake) {
        let agent = def
            .agent
            .key_for(routine)
            .ok()
            .filter(|key| scope.definition(home, principal, key))
            .and_then(|key| crate::agents::service::read_current(home, principal, &key).ok());
        if let Some(agent) = agent.filter(|a| a.is_escalating() && !a.allow_wake) {
            warnings.push(LaunchWarning {
                code: LaunchWarningCode::WakeNotAllowed,
                message: format!(
                    "{} runs without asking for permission, so waking it needs Allow wake in its definition; until then its wakes are refused",
                    agent.name
                ),
            });
        }
    }
    // SW-AGR-13: limits count wakes typed into a live session; a routine that
    // starts a new agent each fire makes none.
    if def.policy.has_wake_limits()
        && super::service::effective_mode(home, principal, routine, def)
            == super::model::RoutineMode::Fresh
    {
        warnings.push(LaunchWarning {
            code: LaunchWarningCode::WakeLimitsIgnored,
            message: "max_wakes, max_wakes_per_day and min_gap count wakes into a running agent; this routine starts a new agent on each fire, so they never apply (use overlap for fresh runs, or mode: wake)".into(),
        });
    }
    warnings
}
