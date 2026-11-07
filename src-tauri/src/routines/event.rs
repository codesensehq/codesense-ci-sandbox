// SPDX-License-Identifier: MIT
//! Event triggers: a routine that fires when an agent finishes
//! (`{kind: agent_done, from_agent?}`).
//!
//! The dashboard stream producer offers every finished session here, before
//! the reactive-automation path filters anything out: a routine run's own agent
//! finishing is an event like any other, and the loop guard in admission is
//! what stops a routine re-firing itself. Each fire is keyed by the finished
//! session, so a delta delivered twice admits once.
//!
//! A finished agent fires a team routine only when it worked in that team, and
//! a personal routine only when the routine's own account launched it. A
//! session whose team or launching account is not known fires neither and
//! leaves no row: that is any agent no launch recorded (a terminal the user
//! started it in by hand), and a skip for each would bury the history of every
//! routine that fires on any agent.

use tauri::{AppHandle, Manager, Runtime};

use crate::agents::{AgentKey, AgentRevisionRef};
use crate::commands::dashboard_launch::{DashboardLaunchState, SessionOwner};
use crate::commands::sprint_run::causation::{CausalEdge, Causation};
use crate::coordinator::StoreResult;
use crate::dashboard_stream::SessionStatus;

use super::admit::{self, Admission, EventCause, Fire};
use super::discover::DiscoveredRoutine;
use super::model::{RoutineDef, RoutineKey, RoutineTrigger, EVENT_AGENT_DONE};
use super::runs::RoutineRun;
use super::state::RoutineStateStore;
use super::tick::{Account, RoutineTicker, RunSource};

/// An agent session that signalled it is done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDoneEvent {
    pub session_id: String,
    pub status: SessionStatus,
    pub summary: Option<String>,
    /// The named agent the session runs, as `<key>@<revision>`, when it was
    /// launched as one.
    pub agent: Option<String>,
    /// The routine whose run the session was, when the launch registry
    /// holds its provenance. The run log is asked first, so a session whose
    /// entry is already gone is still recognized.
    pub routine_key: Option<String>,
    /// Who launched the session and where, when its launch recorded it.
    pub owner: Option<SessionOwner>,
}

impl AgentDoneEvent {
    /// The event as a launch registry sees `session_id`.
    pub(crate) fn from_registry(
        launch: Option<&DashboardLaunchState>,
        session_id: &str,
        status: SessionStatus,
        summary: Option<String>,
    ) -> Self {
        Self {
            session_id: session_id.to_string(),
            status,
            summary,
            agent: launch.and_then(|l| l.agent_revision_for_session(session_id)),
            routine_key: launch.and_then(|l| l.routine_key_for_session(session_id)),
            owner: launch.and_then(|l| l.owner_for_session(session_id)),
        }
    }

    /// The `{{trigger.*}}` fields of a run it fires.
    fn payload(&self) -> serde_json::Value {
        serde_json::json!({
            "session_id": self.session_id,
            "status": self.status,
            "summary": self.summary,
            "agent": self.agent,
        })
    }

    /// The key of the agent the session runs, from its revision stamp.
    fn agent_key(&self) -> Option<AgentKey> {
        AgentRevisionRef::key_of(self.agent.as_deref()?)
    }
}

/// Whether routine `key` (defined by `def`) has an `agent_done` trigger that
/// a session running the agent `agent_key` finishing matches. A trigger naming
/// an agent matches only a session launched as that agent; one naming none
/// matches any finished session.
pub fn matches(key: &RoutineKey, def: &RoutineDef, agent_key: Option<&AgentKey>) -> bool {
    def.triggers.iter().any(|t| match t {
        RoutineTrigger::Event(spec) if spec.kind == EVENT_AGENT_DONE => match &spec.from_agent {
            None => true,
            Some(agent) => match (agent.named_key(key), agent_key) {
                (Ok(wanted), Some(finished)) => wanted == *finished,
                _ => false,
            },
        },
        _ => false,
    })
}

/// Whether a finished session falls in a routine's reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    In,
    Out,
    /// The session's team or launching account is not known.
    Unknown,
}

/// Whether a session owned by `owner` is in reach of routine `key`: a team
/// routine reaches sessions in its own org and team, a personal routine the
/// sessions its account launched in any team of its own org.
pub fn scope(key: &RoutineKey, owner: Option<&SessionOwner>) -> Scope {
    let Some(owner) = owner else {
        return Scope::Unknown;
    };
    match (&key.account, &owner.principal) {
        // SLW-PT-19: a personal routine is filed under ONE org — it reaches its
        // account's sessions in that org only, never another org's (whose session,
        // summary and agent would otherwise flow in through `{{trigger.*}}`).
        (Some(account), Some(launched_by)) if account == launched_by && owner.org == key.org => {
            Scope::In
        }
        (Some(_), Some(_)) => Scope::Out,
        (Some(_), None) => Scope::Unknown,
        (None, _) if owner.org != key.org => Scope::Out,
        (None, _) => match &owner.team {
            Some(team) if *team == key.team => Scope::In,
            Some(_) => Scope::Out,
            None => Scope::Unknown,
        },
    }
}

/// Admit `event` for every routine in `routines` this app fires (armed here,
/// enabled) whose trigger it matches and whose reach takes it in, each run
/// dispatched from this Mac's `account`. Each admission is its own
/// transaction, so one routine's failure does not lose another's run.
pub fn offer(
    account: &Account,
    routines: &[DiscoveredRoutine],
    store: &RoutineStateStore,
    event: &AgentDoneEvent,
    now: i64,
) -> StoreResult<Vec<(String, Admission)>> {
    let host =
        super::host::local_host_id(&account.0).map_err(crate::coordinator::StoreError::Io)?;
    let finished = event.agent_key();
    let mut candidates = Vec::new();
    for routine in routines {
        if !routine.armed_here
            || routine.held()
            || !matches(&routine.key, &routine.def, finished.as_ref())
        {
            continue;
        }
        match store.is_enabled(&routine.key, &routine.def, &host) {
            Ok(true) => candidates.push(routine),
            Ok(false) => {}
            Err(e) => {
                log::warn!(target: "routines", "routine {}: state unreadable: {e}", routine.key)
            }
        }
    }
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let run = store.run_by_session(&event.session_id)?;
    let owner = event
        .owner
        .clone()
        .or_else(|| run.as_ref().and_then(owner_of_run));
    let causation = causation_of(run, event);
    let payload = event.payload();
    let origin = format!("finished session {}", event.session_id);
    let mut admissions = Vec::new();
    for routine in candidates {
        if scope(&routine.key, owner.as_ref()) != Scope::In {
            continue;
        }
        // SLW-PT-20: a personal routine never acts on a session of a team that has "Use my
        // scratch items" off — the team is the finished session's own, never the one on screen.
        if routine.key.account.is_some() {
            if let Some(team) = owner.as_ref().and_then(|o| o.team.as_deref()) {
                if !crate::context_repo::scratch_space::team_uses_scratch(
                    &account.0,
                    &account.1,
                    &routine.key.org,
                    team,
                ) {
                    continue;
                }
            }
        }
        let key = routine.key.to_string();
        let occurrence_key = format!("{key}@event:{}", event.session_id);
        let fire = Fire {
            key: &routine.key,
            principal: &account.1,
            def: &routine.def,
            source: RunSource::Event,
            occurrence_key: Some(&occurrence_key),
            scheduled_at: None,
            now,
            variables: Default::default(),
            trigger_payload: Some(payload.clone()),
            cause: Some(EventCause {
                origin: &origin,
                causation: &causation,
                rate_limited: true,
            }),
        };
        match store.transaction(|tx| admit::admit(tx, &account.0, &fire)) {
            Ok(admission) => admissions.push((key, admission)),
            Err(e) => log::warn!(
                target: "routines",
                "routine {key}: event fire of session {} failed: {e}",
                event.session_id
            ),
        }
    }
    Ok(admissions)
}

/// Where a routine run's session belongs, from its run row: the routine's org
/// and team, and for a personal routine its account. A team routine's run row
/// does not say which account launched it.
fn owner_of_run(run: &RoutineRun) -> Option<SessionOwner> {
    let key = RoutineKey::parse(&run.routine_key)?;
    Some(SessionOwner {
        principal: key.account,
        org: key.org,
        team: Some(key.team),
    })
}

/// The routine runs behind the finished session: the chain its run row
/// carries, or the edge of the routine whose run it was. The run log is asked
/// first, as it outlives the registry entry; the provenance covers a session
/// whose run row this app has not written.
fn causation_of(run: Option<RoutineRun>, event: &AgentDoneEvent) -> Causation {
    if let Some(chain) = run.as_ref().and_then(|r| r.causation.as_deref()) {
        match serde_json::from_str(chain) {
            Ok(chain) => return chain,
            Err(e) => log::warn!(
                target: "routines",
                "session {}: unreadable run chain, using its routine only: {e}",
                event.session_id
            ),
        }
    }
    let routine_key = run
        .map(|r| r.routine_key)
        .or_else(|| event.routine_key.clone());
    match routine_key {
        Some(key) => Causation::default().descend(CausalEdge::new(key.clone(), key)),
        None => Causation::default(),
    }
}

/// Offer a finished session to the app's routines without blocking the
/// caller: the registry is read now, while the session's entry is still
/// there, and discovery and admission run on the blocking pool.
pub fn offer_agent_done<R: Runtime>(
    app: &AppHandle<R>,
    session_id: &str,
    status: SessionStatus,
    summary: Option<String>,
) {
    if app.try_state::<RoutineTicker>().is_none() {
        return;
    }
    let event = AgentDoneEvent::from_registry(
        app.try_state::<DashboardLaunchState>().as_deref(),
        session_id,
        status,
        summary,
    );
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let offered = crate::commands::await_blocking("routine_offer_agent_done", move || {
            if let Some(ticker) = app.try_state::<RoutineTicker>() {
                ticker.offer_agent_done(&event);
            }
            Ok(())
        })
        .await;
        if let Err(e) = offered {
            log::warn!(target: "routines", "routine event offer failed: {e}");
        }
    });
}
