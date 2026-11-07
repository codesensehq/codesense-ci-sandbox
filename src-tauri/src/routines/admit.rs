// SPDX-License-Identifier: MIT
//! Admission: the one path from "this routine should fire" to a run row.
//!
//! Scheduled, catch-up, manual and event fires all come through [`admit`],
//! inside the caller's transaction. It records one `routine_runs` row whatever
//! the policy decides, so "why didn't it run?" always has an answer. Only an
//! admitted run becomes coordinator work: a runnable `runs` row, its dispatch
//! pointer, and a `routine` delivery already consumed, all in the same
//! transaction. Skipped, coalesced and held fires write no coordinator rows, so
//! a restart's `recover` never finds a pending routine delivery.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::commands::sprint_run::causation::{CausalEdge, Causation};
use crate::commands::sprint_run::loop_guard::{self, LoopBudgets, LoopVerdict};
use crate::coordinator::StoreResult;
use crate::domain::Principal;

use super::model::{OverlapPolicy, RoutineDef, RoutineKey};
use super::runs::{self, phase, reason};
use super::target;
use super::tick::RunSource;

/// One fire to admit.
#[derive(Debug, Clone)]
pub struct Fire<'a> {
    pub key: &'a RoutineKey,
    /// The personal space on the Mac that fires the run, which it is
    /// dispatched from; never the routine's creator by default.
    pub principal: &'a Principal,
    /// The routine as of this fire; its `revision` is pinned on the run.
    pub def: &'a RoutineDef,
    pub source: RunSource,
    /// The dedupe key. `None` for a manual fire, which is keyed by its own run id.
    pub occurrence_key: Option<&'a str>,
    /// Unix epoch milliseconds the fire was scheduled for, when it was.
    pub scheduled_at: Option<i64>,
    /// Unix epoch milliseconds of the admission.
    pub now: i64,
    /// Values for the routine's declared variables, over their defaults.
    pub variables: BTreeMap<String, String>,
    /// The event that fired the routine, for `{{trigger.*}}`.
    pub trigger_payload: Option<serde_json::Value>,
    /// Where an event fire came from, for the loop guard. `None` for every
    /// other fire.
    pub cause: Option<EventCause<'a>>,
}

/// What raised an event fire: a finished session, or a message a session queued
/// for a role.
#[derive(Debug, Clone, Copy)]
pub struct EventCause<'a> {
    /// The event, for a loop-guard skip's detail, e.g. `finished session s1`.
    pub origin: &'a str,
    /// The routine runs behind the session, oldest first: the run it was and
    /// the event-fired runs that led to it; empty for an agent started by
    /// anything else.
    pub causation: &'a Causation,
    /// Whether the fire counts against the routine's event rate
    /// ([`EVENT_RATE_LIMIT`] per [`EVENT_RATE_WINDOW_MS`]). Off for a fire per
    /// message (`role_pending`), where each event is distinct work and the
    /// chain alone stops a loop.
    pub rate_limited: bool,
}

/// How far back event fires of one routine count against its rate.
const EVENT_RATE_WINDOW_MS: i64 = 5 * 60 * 1000;

/// Event fires a routine may take in [`EVENT_RATE_WINDOW_MS`].
const EVENT_RATE_LIMIT: u32 = 1;

/// What admission decided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    /// Admitted: the claim loop will dispatch it.
    Queued,
    /// Waiting behind the active run (`overlap: queue`).
    Held,
    /// Folded into the active run (`overlap: coalesce`).
    Coalesced {
        active_run_id: String,
    },
    Skipped {
        reason: String,
    },
    /// The occurrence already has a run row; nothing was written.
    Duplicate,
}

/// The run row admission wrote (or found, for a duplicate) and its outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Admission {
    pub run_id: String,
    #[serde(flatten)]
    pub outcome: Outcome,
}

impl Admission {
    /// Whether the claim loop has new work.
    pub fn is_queued(&self) -> bool {
        self.outcome == Outcome::Queued
    }
}

/// The dispatch request of a routine run, stored as its coordinator pointer.
/// Tagged `kind: routine` so the claim loop routes it here before trying the
/// automation pointer shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename = "routine")]
pub struct RoutinePointer {
    pub run_id: String,
    pub routine_key: String,
    pub revision: u32,
    /// The personal space on the Mac that fired the routine; it locates the
    /// definition.
    pub principal: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<i64>,
    pub fired_at: i64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub variables: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_payload: Option<serde_json::Value>,
    /// SW-AGR-2: sessions (`<machine>/<session>`) a wake found ended after it
    /// picked them: never picked again for this run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ended_sessions: Vec<String>,
}

/// The routine pointer in `json`: `None` when it is not one (another pointer
/// shape), `Some(Err)` when it claims to be one but cannot be read.
pub fn parse_pointer(json: &str) -> Option<Result<RoutinePointer, String>> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    if value.get("kind").and_then(|k| k.as_str()) != Some("routine") {
        return None;
    }
    Some(serde_json::from_value(value).map_err(|e| e.to_string()))
}

/// Admit one fire inside the caller's transaction. See the module docs.
pub fn admit(conn: &Connection, home: &Path, fire: &Fire<'_>) -> StoreResult<Admission> {
    let key = fire.key.to_string();
    if let Some(occurrence_key) = fire.occurrence_key {
        let existing: Option<String> = conn
            .query_row(
                "SELECT run_id FROM routine_runs WHERE occurrence_key = ?1",
                params![occurrence_key],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(run_id) = existing {
            return Ok(Admission {
                run_id,
                outcome: Outcome::Duplicate,
            });
        }
    }
    let run_id = uuid::Uuid::new_v4().to_string();
    let occurrence_key = fire
        .occurrence_key
        .map(str::to_string)
        .unwrap_or_else(|| format!("{key}@manual:{run_id}"));
    let principal = fire.principal;
    let pointer = RoutinePointer {
        run_id: run_id.clone(),
        routine_key: key.clone(),
        revision: fire.def.revision,
        principal: principal.as_str().to_string(),
        source: fire.source.as_str().to_string(),
        scheduled_at: fire.scheduled_at,
        fired_at: fire.now,
        variables: fire.variables.clone(),
        trigger_payload: fire.trigger_payload.clone(),
        ended_sessions: Vec::new(),
    };
    let request = serde_json::to_string(&pointer)
        .map_err(|e| crate::coordinator::StoreError::Effect(e.to_string()))?;

    let (decision, detail) = decide(conn, home, principal, fire, &key)?;
    // An event-fired run carries its chain, so the events its own agent raises
    // know every routine run behind them. A fire that starts no run has no
    // agent to raise any.
    let starts_run = matches!(decision, Outcome::Queued | Outcome::Held);
    let chain = fire
        .cause
        .filter(|_| starts_run)
        .map(|cause| {
            cause
                .causation
                .descend(CausalEdge::new(key.as_str(), key.as_str()))
        })
        .map(|chain| serde_json::to_string(&chain))
        .transpose()
        .map_err(|e| crate::coordinator::StoreError::Effect(e.to_string()))?;
    let (phase_value, reason_value, linked, held_request) = match &decision {
        Outcome::Queued => (phase::QUEUED, None, None, None),
        // Scrubbed like every coordinator pointer: it waits here in the clear.
        Outcome::Held => (
            phase::QUEUED,
            Some(reason::HELD),
            None,
            Some(crate::shared::secret_scan::redact_secrets(&request)),
        ),
        Outcome::Coalesced { active_run_id } => (
            phase::COALESCED,
            None,
            Some(active_run_id.as_str()),
            Some(crate::shared::secret_scan::redact_secrets(&request)),
        ),
        Outcome::Skipped { reason } => (phase::SKIPPED, Some(reason.as_str()), None, None),
        Outcome::Duplicate => unreachable!("decided before the duplicate check"),
    };
    let held_request = held_request.or_else(|| {
        fire.trigger_payload
            .as_ref()
            .map(|_| crate::shared::secret_scan::redact_secrets(&request))
    });
    conn.execute(
        "INSERT INTO routine_runs
             (run_id, routine_key, revision, source, occurrence_key, phase, reason,
              linked_run_id, created_at, request, result_text, max_runtime_ms, causation, action, command_details)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            run_id,
            key,
            fire.def.revision,
            fire.source.as_str(),
            occurrence_key,
            phase_value,
            reason_value,
            linked,
            fire.now,
            held_request,
            detail,
            super::finish::max_runtime_ms(&fire.def.policy),
            chain,
            if fire.def.is_command() { "command" } else { "agent" },
            fire.def.is_command().then(|| serde_json::to_string(&super::command::Evidence { argv: super::command::redacted_argv(&fire.def.command), ..Default::default() }).unwrap()),
        ],
    )?;
    if let Some(schema) = fire
        .def
        .result_schema
        .as_ref()
        .filter(|_| !fire.def.is_command())
    {
        conn.execute(
            "INSERT INTO routine_result_runs (run_id, principal, schema_json) VALUES (?1, ?2, ?3)",
            params![run_id, principal.as_str(), schema.to_string()],
        )?;
    }
    if decision == Outcome::Queued {
        runs::enqueue(conn, &run_id, &occurrence_key, &request)?;
    }
    Ok(Admission {
        run_id,
        outcome: decision,
    })
}

/// The policy checks, in order: paused or off here (this Mac's switch, where
/// it runs), loop guard (event fires), overlap, target. A loop-guard skip also returns its detail for the run's
/// `result_text`.
fn decide(
    conn: &Connection,
    home: &Path,
    principal: &Principal,
    fire: &Fire<'_>,
    key: &str,
) -> StoreResult<(Outcome, Option<String>)> {
    let outcome = |outcome| Ok((outcome, None));
    let skipped = |reason: &str| Outcome::Skipped {
        reason: reason.to_string(),
    };
    // The switch is this Mac's for a routine it runs: a pause left here from
    // before a shared job moved to another Mac does not stop a run by hand,
    // and a recipe each Mac runs that is off here is skipped as off here, so
    // the user is told to turn it on for this Mac rather than that it paused.
    let host = super::host::local_host_id(home).map_err(crate::coordinator::StoreError::Io)?;
    if super::host::is_armed_here(fire.def, &host)
        && !super::state::enabled_here(super::state::switch_in(conn, key)?, fire.def, &host)
    {
        let why = if fire.def.runs_on() == Ok(super::model::RunsOn::EachMac) {
            reason::OFF_HERE
        } else {
            reason::PAUSED
        };
        return outcome(skipped(why));
    }
    if let Err(detail) =
        crate::session_usage::spend_caps::refuse_routine(home, principal, fire.key, fire.def)
    {
        return Ok((skipped(reason::BUDGET_REACHED), Some(detail)));
    }
    if let Some(cause) = &fire.cause {
        if let Some(detail) = loop_guarded(conn, key, cause, fire.now)? {
            return Ok((skipped(reason::LOOP_GUARD), Some(detail)));
        }
    }
    // SW-AGR-2: for a wake routine only a wake still waiting to be typed in
    // is active; a delivered one (or the run that launched the agent) never
    // holds back the next fire. A fire folds into the waiting one.
    let wake = super::service::effective_mode(home, principal, fire.key, fire.def)
        == super::model::RoutineMode::Wake;
    let active = if wake {
        runs::active_wake_run(conn, key)?
    } else {
        runs::active_run(conn, key)?
    };
    if let Some(active_run_id) = active {
        let overlap = match fire.def.policy.overlap {
            OverlapPolicy::Queue if wake => OverlapPolicy::Coalesce,
            overlap => overlap,
        };
        return outcome(match overlap {
            OverlapPolicy::Coalesce => Outcome::Coalesced { active_run_id },
            OverlapPolicy::Skip => skipped(reason::ALREADY_RUNNING),
            OverlapPolicy::Queue if runs::held_count(conn, key)? >= runs::MAX_HELD => {
                skipped(reason::QUEUE_FULL)
            }
            OverlapPolicy::Queue => Outcome::Held,
        });
    }
    // A run on another machine (a wake, or a fresh launch, SW-AGR-2b) works
    // in that machine's workspace: the target need not be cloned on this Mac.
    if super::wake::TargetMachine::parse(fire.def.target.machine.as_deref()).is_remote() {
        return outcome(Outcome::Queued);
    }
    if let Err(why) = target::resolve(home, principal, fire.key, fire.def) {
        log::info!(target: "routines", "routine {key} skipped: {why}");
        return outcome(skipped(reason::TARGET_UNAVAILABLE));
    }
    outcome(Outcome::Queued)
}

/// Why an event fire of `key` must not run, if it must not: this routine's
/// own run is behind the finished session (the edge `key -> key` is already on
/// its chain: its own run, or a cycle through other routines), or the routine
/// already took an event fire within
/// [`EVENT_RATE_WINDOW_MS`]. Only fires that started or joined a run count
/// against the rate: a skipped fire, for whatever reason, did no work, so a
/// steady stream of events still fires once per window, and a paused or busy
/// routine is not locked out once it can run again.
fn loop_guarded(
    conn: &Connection,
    key: &str,
    cause: &EventCause<'_>,
    now: i64,
) -> StoreResult<Option<String>> {
    let recent: i64 = if cause.rate_limited {
        conn.query_row(
            "SELECT COUNT(*) FROM routine_runs
             WHERE routine_key = ?1 AND source = 'event' AND created_at > ?2
               AND phase <> ?3",
            params![key, now - EVENT_RATE_WINDOW_MS, phase::SKIPPED],
            |r| r.get(0),
        )?
    } else {
        0
    };
    let edge = CausalEdge::new(key, key);
    let budgets = LoopBudgets {
        rate_limit: if cause.rate_limited {
            EVENT_RATE_LIMIT
        } else {
            u32::MAX
        },
        ..LoopBudgets::default()
    };
    let verdict = loop_guard::admit(
        cause.causation,
        &edge,
        false,
        &budgets,
        u32::try_from(recent).unwrap_or(u32::MAX),
    );
    // A repeat is named as such even when a budget also breached, so the row
    // says it was the routine's own chain and not merely its rate.
    let why = match cause.causation.ancestry.iter().position(|e| *e == edge) {
        _ if verdict == LoopVerdict::Admit => return Ok(None),
        Some(at) if at + 1 == cause.causation.ancestry.len() => "self-fire",
        Some(_) => "cycle",
        None => verdict.needs_attention_reason().unwrap_or("loop"),
    };
    Ok(Some(format!("{why} ({})", cause.origin)))
}
