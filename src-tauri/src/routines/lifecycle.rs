// SPDX-License-Identifier: MIT
//! An agent's routines follow the agent (SW-AGR-1, SPEC §3 and §9.2):
//!
//! - **Archive** the agent and its routines are archived too, each as its own
//!   revision marked `archived_with_agent`. Callers confirm first, listing
//!   [`active_owned`] (the tab's dialog; MCP's `archiveRoutines: true`).
//! - **Unarchive** the agent (`agent_unarchive`, never `agent_restore`, which
//!   restores an old revision of a live agent) and [`archived_with`] are offered
//!   back; [`restore_owned`] brings them back.
//! - **Delete** the agent is refused while [`active_owned`] is not empty
//!   (`agents::service::delete`, through `RoutineReferrers`).
//!
//! A routine belongs to the agent its dir names ([`super::layout`]); a
//! routine that only waits on an agent (`agent_done from_agent`) is not that
//! agent's.

use std::path::Path;

use serde::Serialize;

use crate::agents::AgentKey;
use crate::domain::Principal;

use super::discover::{discover_routines_with, migrate_all, DiscoveredRoutine};
use super::model::{Author, RoutineKey};
use super::service::{self, RoutineError};

/// One routine of an agent, as a confirm lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OwnedRoutine {
    pub key: RoutineKey,
    pub name: String,
}

impl From<&DiscoveredRoutine> for OwnedRoutine {
    fn from(r: &DiscoveredRoutine) -> Self {
        Self {
            key: r.key.clone(),
            name: r.def.name.clone(),
        }
    }
}

/// Every routine of `agent` the caller can see, archived ones included,
/// sorted by name then key.
fn owned(
    home: &Path,
    principal: &Principal,
    agent: &AgentKey,
) -> Result<Vec<DiscoveredRoutine>, RoutineError> {
    migrate_all(home, principal)?;
    let mut routines: Vec<DiscoveredRoutine> = discover_routines_with(home, principal, true)?
        .into_iter()
        // A duplicate does not own the key its listing shows: never act on it.
        .filter(|r| r.agent_key.as_ref() == Some(agent) && !r.duplicate)
        .collect();
    routines.sort_by(|a, b| {
        (a.def.name.as_str(), a.key.to_string()).cmp(&(b.def.name.as_str(), b.key.to_string()))
    });
    Ok(routines)
}

/// Each agent's routines, by agent: one discovery for a whole agent list.
#[derive(Debug, Default, Clone)]
pub struct AgentRoutines {
    /// Its active routines: what archiving it archives too (the confirm).
    pub active: Vec<OwnedRoutine>,
    /// The routines its archive archived, still archived: what restoring it
    /// offers back, shown until taken.
    pub archived_with: Vec<OwnedRoutine>,
}

/// [`AgentRoutines`] of every agent with any.
pub fn by_agent(
    home: &Path,
    principal: &Principal,
) -> Result<std::collections::HashMap<AgentKey, AgentRoutines>, RoutineError> {
    migrate_all(home, principal)?;
    let mut by_agent: std::collections::HashMap<AgentKey, AgentRoutines> =
        std::collections::HashMap::new();
    for routine in discover_routines_with(home, principal, true)? {
        let Some(agent) = routine.agent_key.clone().filter(|_| !routine.duplicate) else {
            continue;
        };
        let entry = by_agent.entry(agent).or_default();
        if !routine.def.is_archived() {
            entry.active.push(OwnedRoutine::from(&routine));
        } else if routine.def.archived_with_agent {
            entry.archived_with.push(OwnedRoutine::from(&routine));
        }
    }
    let order = |a: &OwnedRoutine, b: &OwnedRoutine| {
        (a.name.as_str(), a.key.to_string()).cmp(&(b.name.as_str(), b.key.to_string()))
    };
    for routines in by_agent.values_mut() {
        routines.active.sort_by(order);
        routines.archived_with.sort_by(order);
    }
    Ok(by_agent)
}

/// `agent`'s routines that are not archived: what archiving the agent
/// archives, and what keeps it from being deleted.
pub fn active_owned(
    home: &Path,
    principal: &Principal,
    agent: &AgentKey,
) -> Result<Vec<OwnedRoutine>, RoutineError> {
    Ok(owned(home, principal, agent)?
        .iter()
        .filter(|r| !r.def.is_archived())
        .map(OwnedRoutine::from)
        .collect())
}

/// `agent`'s routines that its own archive archived and that are still
/// archived: what unarchiving the agent offers back.
pub fn archived_with(
    home: &Path,
    principal: &Principal,
    agent: &AgentKey,
) -> Result<Vec<OwnedRoutine>, RoutineError> {
    Ok(owned(home, principal, agent)?
        .iter()
        .filter(|r| r.def.is_archived() && r.def.archived_with_agent)
        .map(OwnedRoutine::from)
        .collect())
}

/// Archive every active routine of `agent`, marked as archived with it.
/// Returns what was archived. Stops at the first failure: what was archived
/// before it stays archived.
pub fn archive_owned(
    home: &Path,
    principal: &Principal,
    agent: &AgentKey,
    author: &Author,
) -> Result<Vec<OwnedRoutine>, RoutineError> {
    let routines = active_owned(home, principal, agent)?;
    for routine in &routines {
        service::archive_with(home, principal, &routine.key, author, true)?;
    }
    Ok(routines)
}

/// Unarchive every routine `agent`'s archive archived. Returns what came
/// back. One that cannot come back (the active-routine cap) stops the rest,
/// which stay offered.
pub fn restore_owned(
    home: &Path,
    principal: &Principal,
    agent: &AgentKey,
    author: &Author,
) -> Result<Vec<OwnedRoutine>, RoutineError> {
    let routines = archived_with(home, principal, agent)?;
    for routine in &routines {
        service::unarchive(home, principal, &routine.key, author)?;
    }
    Ok(routines)
}
