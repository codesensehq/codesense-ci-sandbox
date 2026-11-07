// SPDX-License-Identifier: MIT
//! The `routine_runs` lifecycle: the phases a run row moves through, the reasons
//! a fire is not run, and the statements every transition shares.
//!
//! ```text
//! admit ─► queued ─► running ─► succeeded | failed | needs_you | ended | timed_out | cancelled
//!   │        ▲
//!   │        └── held (queued, reason `held`) until the active run ends
//!   └─► skipped (reason) | coalesced (linked to the active run)
//! ```
//!
//! A `queued` row with no reason is admitted: it has a runnable coordinator run
//! and the claim loop will dispatch it. A held row waits for the run ahead of it
//! and carries its dispatch request in `request` until it is released.
//!
//! `succeeded`, `needs_you` and a signalled `failed` are the agent's own
//! verdict through `signal_done`. A run whose agent exits cleanly without one
//! is `ended`: CodeSense passes no verdict on it.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::coordinator::sqlite::enqueue_on_accept_in;
use crate::coordinator::{DeliveryRef, StoreResult};

/// Run phases as stored in `routine_runs.phase`.
pub mod phase {
    pub const QUEUED: &str = "queued";
    pub const RUNNING: &str = "running";
    pub const SUCCEEDED: &str = "succeeded";
    pub const FAILED: &str = "failed";
    pub const NEEDS_YOU: &str = "needs_you";
    /// The agent exited cleanly without calling `signal_done`.
    pub const ENDED: &str = "ended";
    pub const TIMED_OUT: &str = "timed_out";
    pub const CANCELLED: &str = "cancelled";
    pub const SKIPPED: &str = "skipped";
    pub const COALESCED: &str = "coalesced";
    /// SW-AGR-9: a wake run's task was typed into a live session.
    pub const DELIVERED: &str = "delivered";
    /// SW-AGR-9: a wake run's task was not typed (its `reason` says why).
    pub const UNDELIVERED: &str = "undelivered";
    /// SW-AGR-9: a wake run's write was not confirmed; it may have reached the
    /// session and is never re-sent (R5).
    pub const UNKNOWN: &str = "unknown";
}

/// Why a run was not run, or how it failed, as stored in `routine_runs.reason`.
/// A failure's explanation, when there is one, is the run's `detail` (wake, machine and
/// admission explanations still use `result_text` until SV-ENG-114).
pub mod reason {
    pub const BUDGET_REACHED: &str = "budget_reached";
    pub const PAUSED: &str = "paused";
    /// A run by hand of a recipe each Mac runs that is off on this Mac.
    pub const OFF_HERE: &str = "off_here";
    pub const ALREADY_RUNNING: &str = "already_running";
    pub const QUEUE_FULL: &str = "queue_full";
    pub const TARGET_UNAVAILABLE: &str = "target_unavailable";
    pub const HELD: &str = "held";
    /// The agent's process exited non-zero or crashed without signalling.
    pub const EXITED_WITH_ERROR: &str = "exited_with_error";
    pub const INTERRUPTED: &str = "interrupted";
    pub const REVISION_MISSING: &str = "revision_missing";
    /// The run's routine key belongs to another account than the one firing it.
    pub const ACCOUNT_MISMATCH: &str = "account_mismatch";
    /// The routine's agent does not resolve: unknown, deleted, or not one
    /// this account can see.
    pub const AGENT_UNAVAILABLE: &str = "agent_unavailable";
    pub const TASK_UNRENDERABLE: &str = "task_unrenderable";
    pub const LAUNCH_FAILED: &str = "launch_failed";
    /// An event fire refused so a routine cannot re-fire itself, or fire
    /// faster than its event rate allows.
    pub const LOOP_GUARD: &str = "loop_guard";
    /// SW-AGR-2: an `inbox_idle` wake left the same mail unread too often.
    pub const IGNORED_WAKES: &str = "ignored_wakes";
}

/// The adapter id of a routine run's coordinator delivery.
pub const DELIVERY_ADAPTER: &str = "routine";

/// How many runs a `queue` routine may hold behind its active run.
pub const MAX_HELD: i64 = 3;

/// A run that is not finished: admitted, held or running.
pub(crate) const LIVE: &str = "phase IN ('queued', 'running')";

/// A run that was admitted and has ended: it will not change again. Fires
/// recorded as `skipped` or `coalesced` never ran, so they are not here.
pub(crate) const TERMINAL: &str =
    "phase IN ('succeeded', 'failed', 'needs_you', 'ended', 'timed_out', 'cancelled', \
     'delivered', 'undelivered', 'unknown')";

/// The run that blocks the next one: admitted or running (a held run waits).
const ACTIVE: &str = "(phase = 'running' OR (phase = 'queued' AND reason IS NULL))";

/// One `routine_runs` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoutineRun {
    pub action: super::model::RoutineAction,
    #[serde(flatten)]
    pub command: super::command::Evidence,
    #[serde(rename = "causation", skip_serializing_if = "Option::is_none")]
    pub upstream: Option<RunCausation>,
    pub run_id: String,
    pub routine_key: String,
    pub revision: u32,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_payload: Option<serde_json::Value>,
    pub occurrence_key: Option<String>,
    pub phase: String,
    pub reason: Option<String>,
    pub linked_run_id: Option<String>,
    pub session_id: Option<String>,
    /// Unix epoch milliseconds.
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    /// What the run's agent (or command) produced: its completion signal, or its output.
    pub result_text: Option<String>,
    /// Why the run ended, when there is an explanation: CodeSense's (a launch refusal, a
    /// task that could not be filled in), or a signal-only CLI's classified exit summary,
    /// which is never its result. Wake, machine and admission explanations still use
    /// `result_text` until SV-ENG-114.
    pub detail: Option<String>,
    /// The agent the run launched, as `<key>@<revision>`, recorded when it
    /// moved to `running`.
    pub agent: Option<String>,
    /// The `max_runtime` of the revision the run was admitted with, in
    /// milliseconds. Kept on the run so the watchdog bounds it whoever is
    /// signed in when it runs long.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_runtime_ms: Option<i64>,
    /// An event-fired run's chain of routine runs (a serialized `Causation`).
    #[serde(skip)]
    pub causation: Option<String>,
    /// SW-AGR-9: when a wake run's task was typed into its live session
    /// (host clock, epoch ms). The run stays `running` until it ends.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<i64>,
    /// SW-AGR-2: the machine of the live session a wake run asked to wake
    /// (`session_id` is that session). Set only on a wake run: its session is
    /// never the run's to kill.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wake_machine: Option<String>,
    /// SW-AGR-2b: the machine this run started its agent on, when that is not
    /// this Mac (a remote wake's launch-if-absent, or a remote fresh run).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch_machine: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct RunCausation {
    pub routine_key: String,
    pub routine_name: String,
    pub run_id: String,
    pub phase: String,
}

impl RoutineRun {
    /// The session this run launched, the one a stop or a timeout may kill:
    /// never the live session a wake run woke.
    pub fn launched_session(&self) -> Option<&str> {
        match self.wake_machine {
            Some(_) => None,
            None => self.session_id.as_deref(),
        }
    }

    /// The machine `session_id` lives on, when it is not this Mac: where its
    /// liveness is asked and a kill is sent (SW-AGR-2b).
    pub fn session_machine(&self) -> Option<&str> {
        self.launch_machine
            .as_deref()
            .or(self.wake_machine.as_deref())
            .filter(|machine| {
                *machine != crate::commands::machines::LOCAL_MACHINE_ID
                    && *machine != super::wake::DEFERRED
            })
    }
}

pub(crate) const RUN_COLUMNS: &str =
    "run_id, routine_key, revision, source, occurrence_key, phase, \
     reason, linked_run_id, session_id, created_at, started_at, ended_at, result_text, \
     max_runtime_ms, causation, agent_ref, delivered_at, wake_machine, launch_machine, \
     COALESCE(request, (SELECT request FROM run_requests WHERE run_requests.run_id = routine_runs.run_id)), action, command_details, detail";

pub(crate) fn read_run(r: &rusqlite::Row<'_>) -> rusqlite::Result<RoutineRun> {
    let payload = r
        .get::<_, Option<String>>(19)?
        .as_deref()
        .and_then(super::admit::parse_pointer)
        .and_then(Result::ok)
        .and_then(|p| p.trigger_payload);
    Ok(RoutineRun {
        action: if r.get::<_, String>(20)? == "command" {
            super::model::RoutineAction::Command
        } else {
            super::model::RoutineAction::Agent
        },
        command: r
            .get::<_, Option<String>>(21)?
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default(),
        upstream: payload
            .as_ref()
            .filter(|p| p["kind"] == "routine_finished")
            .and_then(|p| serde_json::from_value(p.clone()).ok()),
        trigger_payload: payload,
        run_id: r.get(0)?,
        routine_key: r.get(1)?,
        revision: r.get::<_, i64>(2)?.try_into().unwrap_or(u32::MAX),
        source: r.get(3)?,
        occurrence_key: r.get(4)?,
        phase: r.get(5)?,
        reason: r.get(6)?,
        linked_run_id: r.get(7)?,
        session_id: r.get(8)?,
        created_at: r.get(9)?,
        started_at: r.get(10)?,
        ended_at: r.get(11)?,
        result_text: r.get(12)?,
        detail: r.get(22)?,
        max_runtime_ms: r.get(13)?,
        causation: r.get(14)?,
        agent: r.get(15)?,
        delivered_at: r.get(16)?,
        wake_machine: r.get(17)?,
        launch_machine: r.get(18)?,
    })
}

pub(crate) fn run_in(conn: &Connection, run_id: &str) -> rusqlite::Result<Option<RoutineRun>> {
    conn.query_row(
        &format!("SELECT {RUN_COLUMNS} FROM routine_runs WHERE run_id = ?1"),
        params![run_id],
        read_run,
    )
    .optional()
}

/// The routine's admitted or running run, if any.
pub(crate) fn active_run(conn: &Connection, routine_key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        &format!(
            "SELECT run_id FROM routine_runs WHERE routine_key = ?1 AND {ACTIVE}
             ORDER BY created_at, rowid LIMIT 1"
        ),
        params![routine_key],
        |r| r.get(0),
    )
    .optional()
}

/// A wake routine's run that still waits to be typed in (SW-AGR-2): the only
/// run a later wake fire folds into. A delivered wake, or the one that launched
/// the agent, no longer blocks the next fire.
pub(crate) fn active_wake_run(
    conn: &Connection,
    routine_key: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT run_id FROM routine_runs
          WHERE routine_key = ?1 AND phase = 'queued' AND reason IS NULL
          ORDER BY created_at, rowid LIMIT 1",
        params![routine_key],
        |r| r.get(0),
    )
    .optional()
}

/// A paused routine keeps its held runs held until it is resumed.
fn is_paused(conn: &Connection, routine_key: &str) -> rusqlite::Result<bool> {
    Ok(conn
        .query_row(
            "SELECT enabled FROM routine_state WHERE routine_key = ?1",
            params![routine_key],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        == Some(0))
}

pub(crate) fn held_count(conn: &Connection, routine_key: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM routine_runs
         WHERE routine_key = ?1 AND phase = 'queued' AND reason = 'held'",
        params![routine_key],
        |r| r.get(0),
    )
}

/// Make `run_id` runnable for the claim loop: the coordinator run, its dispatch
/// pointer, and its `routine` delivery, consumed and linked in the same
/// transaction so a restart's `recover` never sees it pending.
pub(crate) fn enqueue(
    conn: &Connection,
    run_id: &str,
    occurrence_key: &str,
    request: &str,
) -> StoreResult<()> {
    conn.execute(
        "INSERT OR IGNORE INTO deliveries (adapter_id, delivery_key, state)
         VALUES (?1, ?2, 'pending')",
        params![DELIVERY_ADAPTER, occurrence_key],
    )?;
    enqueue_on_accept_in(
        conn,
        run_id,
        request,
        Some(&DeliveryRef {
            adapter_id: DELIVERY_ADAPTER.to_string(),
            delivery_key: occurrence_key.to_string(),
        }),
    )?;
    Ok(())
}

/// Record a fire that asks the human instead of running: a `needs_you` row
/// that never ran (SW-AGR-2: an `inbox_idle` wake ignored too often).
pub(crate) fn record_needs_you(
    conn: &Connection,
    routine_key: &str,
    revision: u32,
    occurrence_key: &str,
    now: i64,
    detail: &str,
) -> StoreResult<String> {
    let run_id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO routine_runs
             (run_id, routine_key, revision, source, occurrence_key, phase, reason,
              created_at, ended_at, result_text)
         VALUES (?1, ?2, ?3, 'event', ?4, ?5, ?6, ?7, ?7, ?8)",
        params![
            run_id,
            routine_key,
            revision,
            occurrence_key,
            phase::NEEDS_YOU,
            reason::IGNORED_WAKES,
            now,
            detail
        ],
    )?;
    Ok(run_id)
}

/// Release the routine's oldest held run to the claim loop when nothing is
/// active. Returns the released run id.
pub(crate) fn release_next_held(
    conn: &Connection,
    routine_key: &str,
) -> StoreResult<Option<String>> {
    if active_run(conn, routine_key)?.is_some() || is_paused(conn, routine_key)? {
        return Ok(None);
    }
    let next: Option<(String, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT run_id, occurrence_key, request FROM routine_runs
             WHERE routine_key = ?1 AND phase = 'queued' AND reason = 'held'
             ORDER BY created_at, rowid LIMIT 1",
            params![routine_key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((run_id, occurrence_key, request)) = next else {
        return Ok(None);
    };
    let (Some(occurrence_key), Some(request)) = (occurrence_key, request) else {
        // A held row always carries both; one without them can never dispatch.
        conn.execute(
            "UPDATE routine_runs SET phase = 'failed', reason = ?2, request = NULL
             WHERE run_id = ?1",
            params![run_id, reason::LAUNCH_FAILED],
        )?;
        return Ok(None);
    };
    conn.execute(
        "UPDATE routine_runs SET reason = NULL, request = NULL WHERE run_id = ?1",
        params![run_id],
    )?;
    enqueue(conn, &run_id, &occurrence_key, &request)?;
    Ok(Some(run_id))
}

/// Every routine that has a held run but nothing active: the ones a missed
/// release (a crash between terminalizing and releasing) left waiting.
pub(crate) fn stalled_queues(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT h.routine_key FROM routine_runs h
         WHERE h.phase = 'queued' AND h.reason = 'held'
           AND NOT EXISTS (
               SELECT 1 FROM routine_runs a
               WHERE a.routine_key = h.routine_key
                 AND (a.phase = 'running' OR (a.phase = 'queued' AND a.reason IS NULL)))",
    )?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    rows.collect()
}
