// SPDX-License-Identifier: MIT
//! How a routine run ends. [`finish`] is the one terminal writer: every ending
//! (the agent's `signal_done`, a process exit, the watchdog, a stop, a restart
//! that finds the session gone) goes through its compare-and-swap on
//! `phase`, so the first ending wins and a later one changes nothing.
//!
//! Commands use their exit status as the verdict. For an agent, only
//! `signal_done` gives a run a verdict. An agent that exits cleanly
//! without one leaves the run `ended`, with no result; a headless process
//! that exits non-zero or crashes is `failed`. A tab's exit status is not
//! known, so a tab closed without a signal simply ends.
//!
//! There are no retries: the next occurrence is the retry. The run's summary is
//! its `result_text`, and a succeeded run's summary becomes the routine's
//! `last_result_text`, the `{{last_run_summary}}` of its next run. Nothing is
//! delivered anywhere else.

use std::collections::HashSet;

use rusqlite::{params, OptionalExtension};

use crate::commands::pty_backend::Liveness;
use crate::coordinator::StoreResult;
use crate::dashboard_stream::SessionStatus;

use super::model::Policy;
use super::runs::{self, phase, reason, RoutineRun};
use super::state::RoutineStateStore;

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ending {
    Command {
        phase: &'static str,
        code: Option<i32>,
        output: String,
    },
    /// The agent called `signal_done`.
    Signal {
        status: SessionStatus,
        summary: Option<String>,
    },
    /// The agent's process ended without signalling: cleanly (`error` is
    /// `None`), or non-zero or by a crash, with what it reported. What a
    /// signal-only CLI reported explains the ending; it is never its result.
    Exited {
        error: Option<String>,
        signal_only: bool,
    },
    /// The watchdog stopped it past `max_runtime`.
    TimedOut,
    /// Stopped by hand.
    Cancelled,
    /// The app restarted and its session was gone.
    Interrupted,
    /// It could not be started.
    Failed {
        reason: &'static str,
        detail: String,
    },
    /// SW-AGR-9: a wake run whose delivery ended it (`undelivered`,
    /// `unknown` or `coalesced`), queued or after its delivery.
    Wake {
        phase: &'static str,
        reason: Option<&'static str>,
        detail: Option<String>,
    },
}

impl Ending {
    /// A clean exit without a signal: no verdict, and nothing to explain.
    pub const ENDED: Self = Self::Exited {
        error: None,
        signal_only: false,
    };

    fn phase(&self) -> &'static str {
        match self {
            Self::Command { phase, .. } => phase,
            Self::Signal { status, .. } => match status {
                SessionStatus::Success => phase::SUCCEEDED,
                SessionStatus::NeedsAttention => phase::NEEDS_YOU,
                SessionStatus::Failed => phase::FAILED,
            },
            Self::Exited { error: None, .. } => phase::ENDED,
            Self::TimedOut => phase::TIMED_OUT,
            Self::Cancelled => phase::CANCELLED,
            Self::Exited { error: Some(_), .. } | Self::Interrupted | Self::Failed { .. } => {
                phase::FAILED
            }
            Self::Wake { phase: ended, .. } => ended,
        }
    }

    fn reason(&self) -> Option<&'static str> {
        match self {
            Self::Exited { error: Some(_), .. } => Some(reason::EXITED_WITH_ERROR),
            Self::Interrupted => Some(reason::INTERRUPTED),
            Self::Failed { reason, .. } => Some(reason),
            Self::Wake { reason, .. } => *reason,
            _ => None,
        }
    }

    fn result_text(&self) -> Option<&str> {
        match self {
            Self::Command { output, .. } => Some(output),
            Self::Signal { summary, .. } => summary.as_deref(),
            Self::Exited {
                error,
                signal_only: false,
            } => error.as_deref(),
            Self::Wake { detail, .. } => detail.as_deref(),
            _ => None,
        }
    }

    /// CodeSense's explanation of why the run ended, kept apart from what its agent produced.
    fn detail(&self) -> Option<&str> {
        match self {
            Self::Failed { detail, .. } => Some(detail),
            Self::Exited {
                error,
                signal_only: true,
            } => error.as_deref(),
            _ => None,
        }
    }

    /// Endings that can reach a run before it started: a stop of a queued or
    /// held run, or a dispatch that could not start it.
    fn may_end_unstarted(&self) -> bool {
        matches!(
            self,
            Self::Cancelled | Self::Failed { .. } | Self::Wake { .. }
        )
    }
}

/// A terminal transition that took effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    pub store_path: Option<std::path::PathBuf>,
    wait_for_command_output: bool,
    pub routine_key: String,
    pub run_id: String,
    pub phase: &'static str,
    /// The held run this ending released to the claim loop.
    pub released: Option<String>,
}

impl Finished {
    /// Emit `routines:changed` and wake the claim loop for a released run.
    pub fn announce(&self) {
        if !self.wait_for_command_output {
            if let Some(path) = &self.store_path {
                if let Err(e) = super::chain::offer(path, &self.run_id) {
                    log::warn!(target: "routines", "completion trigger failed for {}: {e}", self.run_id);
                }
            }
        }
        super::events::changed(&self.routine_key, &self.run_id);
        if let Some(released) = &self.released {
            super::events::changed(&self.routine_key, released);
            crate::commands::sprint_run::claim_loop::wake();
        }
    }
}

/// End `run_id` with `ending` at `now`, unless it already ended. In the same
/// transaction: the routine's last-run facts, and the release of its next held
/// run. `None` when the run had already ended (or does not exist).
pub fn finish(
    store: &RoutineStateStore,
    run_id: &str,
    ending: Ending,
    now: i64,
) -> StoreResult<Option<Finished>> {
    let finished = store.transaction(|tx| {
        let mut finished = finish_in(tx, run_id, ending, now)?;
        if let Some(finished) = &mut finished {
            finished.store_path = Some(store.path().to_path_buf());
            finished.released = runs::release_next_held(tx, &finished.routine_key)?;
        }
        Ok(finished)
    })?;
    if let Some(finished) = &finished {
        // The run's task closes with the run, whichever way it ended.
        crate::session_usage::task_outcome::close_run_task(store.path(), run_id, finished.phase);
    }
    Ok(finished)
}

/// Shared terminal writer; callers release held work only after the whole operation ends.
fn finish_in(
    tx: &rusqlite::Connection,
    run_id: &str,
    ending: Ending,
    now: i64,
) -> StoreResult<Option<Finished>> {
    let may_end_unstarted = ending.may_end_unstarted();
    let ending =
        if ending.phase() == phase::SUCCEEDED && super::results::missing_candidate(tx, run_id)? {
            Ending::Failed {
                reason: "result_missing",
                detail: "Required structured result was not published".into(),
            }
        } else {
            ending
        };
    let from = if may_end_unstarted {
        runs::LIVE
    } else {
        "phase = 'running'"
    };
    let changed = tx.execute(
        &format!(
            "UPDATE routine_runs
                 SET phase = ?2, reason = ?3, result_text = COALESCE(?4, result_text),
                     ended_at = ?5, request = NULL, detail = ?6
                 WHERE run_id = ?1 AND {from}"
        ),
        params![
            run_id,
            ending.phase(),
            match &ending {
                Ending::Command {
                    phase: "failed",
                    code,
                    ..
                } => Some(format!(
                    "exit:{}",
                    code.map(|c| c.to_string())
                        .unwrap_or_else(|| "signal".into())
                )),
                _ => ending.reason().map(str::to_string),
            },
            ending.result_text(),
            now,
            Option::map(ending.detail(), crate::shared::secret_scan::redact_secrets),
        ],
    )?;
    if changed == 0 {
        return Ok(None);
    }
    let (routine_key, started_at, result_text, action): (
        String,
        Option<i64>,
        Option<String>,
        String,
    ) = tx.query_row(
        "SELECT routine_key, started_at, result_text, action FROM routine_runs WHERE run_id = ?1",
        params![run_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let phase = ending.phase();
    // A run stopped or refused before it started never ran: the routine's
    // last run is still the one before it.
    if let Some(started_at) = started_at {
        tx.execute(
            "INSERT INTO routine_state (routine_key, last_run_id, last_run_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(routine_key) DO UPDATE SET
                     last_run_id = excluded.last_run_id,
                     last_run_at = excluded.last_run_at",
            params![routine_key, run_id, started_at],
        )?;
    }
    if phase == phase::SUCCEEDED {
        tx.execute(
            "UPDATE routine_state SET last_result_text = ?2 WHERE routine_key = ?1",
            params![routine_key, result_text.unwrap_or_default()],
        )?;
    }
    super::results::complete(tx, run_id, phase == phase::SUCCEEDED, now)?;

    Ok(Some(Finished {
        store_path: None,
        // The watchdog settles the phase before killing; capture saves the tail next.
        wait_for_command_output: action == "command" && matches!(ending, Ending::TimedOut),
        routine_key,
        run_id: run_id.to_string(),
        phase,
        released: None,
    }))
}

pub(super) fn cancel_batch_in(
    tx: &rusqlite::Connection,
    ids: &[String],
    now: i64,
) -> StoreResult<Vec<Finished>> {
    let mut finished = Vec::new();
    for id in ids {
        if let Some(ended) = finish_in(tx, id, Ending::Cancelled, now)? {
            finished.push(ended);
        }
    }
    let mut released_keys = HashSet::new();
    for ended in &mut finished {
        if released_keys.insert(ended.routine_key.clone()) {
            ended.released = runs::release_next_held(tx, &ended.routine_key)?;
        }
    }
    Ok(finished)
}

/// [`finish`] and announce it. Errors are logged: an ending that cannot be
/// written is left for the watchdog.
pub fn finish_and_announce(store: &RoutineStateStore, run_id: &str, ending: Ending, now: i64) {
    match finish(store, run_id, ending, now) {
        Ok(Some(finished)) => finished.announce(),
        Ok(None) => {}
        Err(e) => log::warn!(target: "routines", "routine run {run_id}: finish failed: {e}"),
    }
}

/// A launched session's outcome, from the run-outcome funnel. A signal ends the
/// run with the agent's verdict; a process exit without one ends it as
/// [`exit_ending`] reads the exit classification.
pub fn on_session_outcome(
    run_id: &str,
    status: SessionStatus,
    summary: Option<String>,
    source: crate::commands::sprint_run::run_store::OutcomeSource,
    signal_only: bool,
) {
    use crate::commands::sprint_run::run_store::OutcomeSource;
    let Some(store) = app_store() else {
        return;
    };
    let ending = match source {
        OutcomeSource::Signal => Ending::Signal { status, summary },
        OutcomeSource::ProcessExit => exit_ending(status, summary, signal_only),
    };
    finish_and_announce(&store, run_id, ending, now_ms());
}

/// How a headless process exit with no signal ends its run. Only a clean exit
/// is `success` in the exit classification, and it ends the run with no
/// verdict: its stdout is not a result. A capture stopped by someone (a Stop
/// outside the routine's own) is a cancel, not an agent error. A capture that
/// hit its time limit timed out. Any other exit fails the run with what the
/// classification reported, scrubbed of secrets since run history is read back.
pub(super) fn exit_ending(
    status: SessionStatus,
    summary: Option<String>,
    signal_only: bool,
) -> Ending {
    match status {
        SessionStatus::Success => Ending::ENDED,
        SessionStatus::NeedsAttention => Ending::TimedOut,
        SessionStatus::Failed
            if summary.as_deref()
                == Some(crate::commands::dashboard_launch::HEADLESS_STOPPED_SUMMARY) =>
        {
            Ending::Cancelled
        }
        SessionStatus::Failed => Ending::Exited {
            error: Some(crate::shared::secret_scan::redact_secrets(
                summary
                    .as_deref()
                    .unwrap_or("the agent exited with an error"),
            )),
            signal_only,
        },
    }
}

/// A `signal_done` from a session this app holds no run context for (a visible
/// run that survived an app restart): end the `running` run recorded with that
/// session id, if there is one.
pub fn finish_by_session(
    store: &RoutineStateStore,
    session_id: &str,
    status: SessionStatus,
    summary: Option<String>,
    now: i64,
) -> StoreResult<Option<Finished>> {
    let run_id: Option<String> = store.transaction(|tx| {
        Ok(tx
            .query_row(
                "SELECT run_id FROM routine_runs WHERE session_id = ?1 AND phase = 'running'",
                params![session_id],
                |r| r.get(0),
            )
            .optional()?)
    })?;
    match run_id {
        Some(run_id) => finish(store, &run_id, Ending::Signal { status, summary }, now),
        None => Ok(None),
    }
}

/// SW-AGR-9: record a wake delivery's status on its run (R7). `queued`
/// binds the run to its session; `delivered` keeps it `running` with
/// `delivered_at` (the woken agent still finishes it); `undelivered` and
/// `coalesced` end it. `unknown` shows the run as `unknown` but is not
/// terminal for a wake run: the real outcome of a lost reply may still
/// arrive, and a `delivered` then upgrades it back to `running` (so
/// `routine_done` works). The delivery core reports `unknown` once more,
/// finally, when the wait runs out. `None` when nothing changed.
pub(crate) fn apply_wake_status(
    store: &RoutineStateStore,
    run_id: &str,
    target: &crate::delivery::authority::SessionRef,
    status: &crate::delivery::run_status::WakeRunStatus,
    now: i64,
) -> StoreResult<Option<&'static str>> {
    let session_id = target.session.as_str();
    // SW-AGR-2: the run never owns the session it woke (it is never killed
    // with the run), whichever path recorded it first.
    store.transaction(|tx| {
        tx.execute(
            "UPDATE routine_runs SET wake_machine = COALESCE(wake_machine, ?2) WHERE run_id = ?1",
            params![run_id, target.machine],
        )?;
        Ok(())
    })?;
    use crate::delivery::run_status::WakeRunStatus;
    if let WakeRunStatus::Unknown { detail } = status {
        let routine_key: Option<String> = store.transaction(|tx| {
            let changed = tx.execute(
                "UPDATE routine_runs SET phase = 'unknown', reason = NULL, session_id = ?2,
                        result_text = ?3, ended_at = ?4
                  WHERE run_id = ?1 AND phase IN ('queued', 'running', 'unknown')",
                params![run_id, session_id, detail, now],
            )?;
            if changed == 0 {
                return Ok(None);
            }
            Ok(tx
                .query_row(
                    "SELECT routine_key FROM routine_runs WHERE run_id = ?1",
                    params![run_id],
                    |r| r.get(0),
                )
                .optional()?)
        })?;
        return Ok(routine_key.map(|key| {
            super::events::changed(&key, run_id);
            phase::UNKNOWN
        }));
    }
    if status.ends_run() {
        let detail = match status {
            WakeRunStatus::Unknown { detail } => Some(detail.clone()),
            WakeRunStatus::Coalesced { into } => Some(format!("coalesced into delivery {into}")),
            _ => None,
        };
        let ending = Ending::Wake {
            phase: status.run_phase(),
            reason: status.run_reason(),
            detail,
        };
        return Ok(finish(store, run_id, ending, now)?.map(|finished| {
            finished.announce();
            finished.phase
        }));
    }
    let delivered_at = status.delivered_at_ms().map(|at| at as i64);
    let schema = store.transaction(|tx| {
        Ok(tx
            .query_row(
                "SELECT 1 FROM routine_result_runs WHERE run_id = ?1",
                params![run_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    })?;
    let changed = store.transaction(|tx| {
        Ok(match delivered_at {
            // From `unknown` too: the lost reply's real outcome upgrades it.
            Some(at) => tx.execute(
                "UPDATE routine_runs SET session_id = ?2,
                        started_at = COALESCE(started_at, ?3), delivered_at = ?3,
                        result_text = CASE WHEN phase = 'unknown' THEN NULL ELSE result_text END,
                        ended_at = NULL, reason = NULL, phase = 'running'
                  WHERE run_id = ?1 AND phase IN ('queued', 'running', 'unknown')",
                params![run_id, session_id, at],
            )?,
            None => tx.execute(
                "UPDATE routine_runs SET session_id = ?2 WHERE run_id = ?1 AND phase = 'queued'",
                params![run_id, session_id],
            )?,
        })
    })?;
    if changed > 0 && delivered_at.is_some() {
        super::dispatch::note_run_start(store, run_id, None);
    }
    if changed > 0 && delivered_at.is_some() && !schema {
        // §4.1.5: a delivered wake whose routine asks for no result is done
        // once it is typed in; only a result schema keeps it open.
        let ending = Ending::Wake {
            phase: phase::DELIVERED,
            reason: None,
            detail: None,
        };
        return Ok(finish(store, run_id, ending, now)?.map(|finished| {
            finished.announce();
            finished.phase
        }));
    }
    Ok((changed > 0).then(|| status.run_phase()))
}

/// Why `routine_done` changed nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutineDoneRefusal {
    /// No such run.
    NoSuchRun,
    /// The run is not the calling session's.
    NotThisSession,
    /// The run is not running (`phase` says where it stands).
    NotRunning { phase: String },
}

impl std::fmt::Display for RoutineDoneRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSuchRun => f.write_str("no such routine run"),
            Self::NotThisSession => f.write_str("that run is not this session's"),
            Self::NotRunning { phase } => write!(f, "that run is not running (it is {phase})"),
        }
    }
}

/// SW-AGR-9 `routine_done`: end `run_id` with the agent's verdict, only if it
/// is `session_id`'s running run. Unlike `signal_done` it says nothing about
/// the session: the session keeps running, and any other run it holds is
/// untouched.
pub fn finish_for_session(
    store: &RoutineStateStore,
    run_id: &str,
    session_id: &str,
    status: SessionStatus,
    summary: Option<String>,
    now: i64,
) -> StoreResult<Result<Finished, RoutineDoneRefusal>> {
    let Some(run) = store.run(run_id)? else {
        return Ok(Err(RoutineDoneRefusal::NoSuchRun));
    };
    if run.session_id.as_deref() != Some(session_id) {
        return Ok(Err(RoutineDoneRefusal::NotThisSession));
    }
    if run.phase != phase::RUNNING {
        return Ok(Err(RoutineDoneRefusal::NotRunning { phase: run.phase }));
    }
    match finish(store, run_id, Ending::Signal { status, summary }, now)? {
        Some(finished) => Ok(Ok(finished)),
        // Ended between the read and the write: report where it stands now.
        None => Ok(Err(RoutineDoneRefusal::NotRunning {
            phase: store.run(run_id)?.map(|run| run.phase).unwrap_or_default(),
        })),
    }
}

/// [`finish_by_session`] against this app's coordinator, announced.
pub fn on_signal_without_context(session_id: &str, status: SessionStatus, summary: Option<String>) {
    let Some(store) = app_store() else {
        return;
    };
    match finish_by_session(&store, session_id, status, summary, now_ms()) {
        Ok(Some(finished)) => finished.announce(),
        Ok(None) => {}
        Err(e) => {
            log::warn!(target: "routines", "routine signal for session {session_id} failed: {e}")
        }
    }
}

/// The app's sessions, as the run lifecycle needs them.
pub trait Sessions: Send + Sync {
    /// Kill every session launched for `run_id`, or, when this app holds none
    /// (a session that outlived an app restart), the recorded `session_id`.
    fn kill_run(&self, run_id: &str, session_id: Option<&str>);
    /// Whether `session_id`'s agent is still running anywhere this app can ask.
    fn liveness(&self, session_id: &str) -> Liveness<bool>;

    /// [`Self::liveness`] of a session on `machine` (`None`: this Mac). A
    /// session on a machine is asked of that machine; one it cannot reach
    /// is `Unknown`, never ended (SW-AGR-2b).
    fn liveness_on(&self, machine: Option<&str>, session_id: &str) -> Liveness<bool> {
        match machine {
            None => self.liveness(session_id),
            Some(_) => Liveness::Unknown,
        }
    }

    /// [`Self::kill_run`] of a run whose session is on `machine` (`None`:
    /// this Mac): the kill is sent to that machine.
    fn kill_on(&self, run_id: &str, machine: Option<&str>, session_id: Option<&str>) {
        match machine {
            None => self.kill_run(run_id, session_id),
            Some(machine) => {
                log::warn!(target: "routines", "routine run {run_id}: its session on {machine} cannot be stopped from here")
            }
        }
    }
}

/// What a machine's terminal server answered about one session.
pub(crate) fn remote_liveness(
    answer: Result<HashSet<codesense_pty_core::domain::PtySessionId>, String>,
    session_id: &str,
) -> Liveness<bool> {
    match answer {
        Ok(alive) => Liveness::Known(alive.iter().any(|id| id.as_str() == session_id)),
        Err(_) => Liveness::Unknown,
    }
}

/// The production [`Sessions`]: the launch registry and the terminal hosts.
pub struct AppSessions {
    app: tauri::AppHandle,
}

impl AppSessions {
    pub fn new(app: tauri::AppHandle) -> Self {
        Self { app }
    }
}

impl Sessions for AppSessions {
    fn kill_run(&self, run_id: &str, session_id: Option<&str>) {
        use tauri::Manager;
        let stopped = self
            .app
            .try_state::<crate::commands::dashboard_launch::DashboardLaunchState>()
            .map(|launch| {
                let targets = launch.drain_run(run_id);
                crate::commands::dashboard_launch::teardown_stop_targets(&self.app, targets)
            })
            .unwrap_or(0);
        if stopped == 0 {
            if let Some(session_id) = session_id {
                crate::commands::pty::kill_session(&self.app, session_id);
            }
        }
    }

    fn liveness(&self, session_id: &str) -> Liveness<bool> {
        use tauri::Manager;
        if self
            .app
            .try_state::<crate::commands::dashboard_launch::DashboardLaunchState>()
            .is_some_and(|s| s.has_live_headless_session(session_id))
        {
            return Liveness::Known(true);
        }
        // A tab can outlive its agent, so a terminal session counts only while
        // its child process runs, not while its host merely keeps the tab.
        match crate::commands::pty_backend::alive_session_ids(&self.app, &[session_id]) {
            Liveness::Known(alive) => Liveness::Known(alive.contains(session_id)),
            Liveness::Unknown => Liveness::Unknown,
        }
    }

    fn liveness_on(&self, machine: Option<&str>, session_id: &str) -> Liveness<bool> {
        let Some(machine) = machine else {
            return self.liveness(session_id);
        };
        // Asked of that machine's terminal server; one that cannot be reached
        // (offline, never admitted) is Unknown: its run is never ended for it.
        let answer = self.remote_client(machine, session_id).and_then(|client| {
            use codesense_pty_core::domain::TerminalDomain;
            client
                .alive_session_ids(&[codesense_pty_core::domain::PtySessionId::new(session_id)])
                .map_err(|e| e.to_string())
        });
        remote_liveness(answer, session_id)
    }

    fn kill_on(&self, run_id: &str, machine: Option<&str>, session_id: Option<&str>) {
        let (Some(machine), Some(session_id)) = (machine, session_id) else {
            return self.kill_run(run_id, session_id);
        };
        let killed = self.remote_client(machine, session_id).and_then(|client| {
            use codesense_pty_core::domain::TerminalDomain;
            client
                .kill(&codesense_pty_core::domain::PtySessionId::new(session_id))
                .map_err(|e| e.to_string())
        });
        if let Err(error) = killed {
            log::warn!(target: "routines", "routine run {run_id}: could not stop {session_id} on {machine}: {error}");
        }
    }
}

impl AppSessions {
    /// The admitted terminal server of `machine`, revalidated.
    fn remote_client(
        &self,
        machine: &str,
        session_id: &str,
    ) -> Result<std::sync::Arc<crate::terminal::session_server::SessionServerClient>, String> {
        let backend =
            crate::commands::pty_backend::resolve_spawn_backend(&self.app, machine, session_id)?;
        backend.revalidate(&self.app)?;
        backend
            .client()
            .filter(|_| backend.is_remote())
            .ok_or_else(|| format!("{machine} is not connected"))
    }
}

/// Time out every `running` run older than the `max_runtime` it was admitted
/// with: the run is ended `timed_out` first, so the exit its kill causes loses
/// the compare-and-swap, then its sessions are killed. Returns how many timed
/// out.
pub fn watchdog(
    store: &RoutineStateStore,
    now: i64,
    sessions: &dyn Sessions,
) -> StoreResult<usize> {
    let mut timed_out = 0;
    for run in store.running_runs()? {
        let limit = run_limit_ms(&run);
        let started = run.started_at.unwrap_or(run.created_at);
        if now - started < limit {
            continue;
        }
        if let Some(finished) = finish(store, &run.run_id, Ending::TimedOut, now)? {
            log::info!(
                target: "routines",
                "routine {} run {} passed its max_runtime and was stopped",
                run.routine_key,
                run.run_id
            );
            // A woken session is the agent's own: never killed with its run.
            if run.wake_machine.is_none() {
                sessions.kill_on(&run.run_id, run.session_machine(), run.launched_session());
            }
            finished.announce();
            timed_out += 1;
        }
    }
    Ok(timed_out)
}

/// The longest runtime a routine may declare, and the bound of a run admitted
/// with none.
const MAX_RUNTIME_MS: i64 = 24 * 60 * 60 * 1000;

/// How long `run` may run, in milliseconds: the `max_runtime` it was admitted
/// with. The watchdog ends it past this, and a headless launch is given the
/// same ceiling so the routine's bound, not a generic one, governs it.
pub(super) fn run_limit_ms(run: &RoutineRun) -> i64 {
    run.max_runtime_ms.unwrap_or(MAX_RUNTIME_MS)
}

/// `policy`'s runtime bound in milliseconds, as a run is admitted with it.
pub(super) fn max_runtime_ms(policy: &Policy) -> i64 {
    policy
        .max_runtime_duration()
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(MAX_RUNTIME_MS)
}

/// End every `running` run whose session is known to be gone without a
/// recorded outcome: a tab closed, or its agent quit, without `signal_done`.
/// Its exit status is not known here, so the run simply ends. A headless run
/// is left to its own completion, which knows how its process exited. Unknown
/// liveness changes nothing. A run started before
/// `since` belongs to the restart reconcile, until the reconcile finds its
/// session still running and hands it over in `survivors`.
pub fn sweep_exited(
    store: &RoutineStateStore,
    now: i64,
    since: i64,
    survivors: &HashSet<String>,
    sessions: &dyn Sessions,
) -> StoreResult<usize> {
    let mut ended = 0;
    for run in store.running_runs()? {
        let Some(session_id) = run.session_id.as_deref() else {
            continue;
        };
        // Its process is gone before its completion records the exit: ending it
        // here would race a crash into a neutral ending.
        if crate::commands::dashboard_launch::is_headless_session_id(session_id) {
            continue;
        }
        let started_here = run.started_at.is_some_and(|t| t >= since);
        if !started_here && !survivors.contains(&run.run_id) {
            continue;
        }
        if sessions.liveness_on(run.session_machine(), session_id) != Liveness::Known(false) {
            continue;
        }
        if let Some(finished) = finish(store, &run.run_id, Ending::ENDED, now)? {
            finished.announce();
            ended += 1;
        }
    }
    Ok(ended)
}

/// Release the next held run of every routine whose queue a crash left with
/// nothing active.
pub fn release_stalled_queues(store: &RoutineStateStore) -> StoreResult<()> {
    let released: Vec<(String, String)> = store.transaction(|tx| {
        let mut out = Vec::new();
        for key in runs::stalled_queues(tx)? {
            if let Some(run_id) = runs::release_next_held(tx, &key)? {
                out.push((key, run_id));
            }
        }
        Ok(out)
    })?;
    for (key, run_id) in &released {
        super::events::changed(key, run_id);
    }
    if !released.is_empty() {
        crate::commands::sprint_run::claim_loop::wake();
    }
    Ok(())
}

/// What [`reconcile_runs`] found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Runs whose session is gone, now `failed (interrupted)`.
    pub interrupted: Vec<String>,
    /// Runs whose session is still running; they stay `running`.
    pub alive: Vec<String>,
    /// Runs whose session's liveness could not be learned; they stay `running`.
    pub unknown: Vec<String>,
}

/// Settle `running` runs against their sessions (all of them, or only the
/// `only` run ids): a gone session's run is `failed (interrupted)`, a live or
/// unknown one is left running. Nothing is ever relaunched.
pub fn reconcile_runs(
    store: &RoutineStateStore,
    now: i64,
    sessions: &dyn Sessions,
    only: Option<&[String]>,
) -> StoreResult<ReconcileReport> {
    let only: Option<HashSet<&str>> = only.map(|ids| ids.iter().map(String::as_str).collect());
    let mut report = ReconcileReport::default();
    for run in store.running_runs()? {
        if only
            .as_ref()
            .is_some_and(|ids| !ids.contains(run.run_id.as_str()))
        {
            continue;
        }
        // A run that never recorded its session died mid-launch.
        let liveness = match run.session_id.as_deref() {
            Some(session_id) => sessions.liveness_on(run.session_machine(), session_id),
            None => Liveness::Known(false),
        };
        match liveness {
            Liveness::Known(true) => report.alive.push(run.run_id),
            Liveness::Unknown => report.unknown.push(run.run_id),
            Liveness::Known(false) => {
                if let Some(finished) = finish(store, &run.run_id, Ending::Interrupted, now)? {
                    finished.announce();
                    report.interrupted.push(run.run_id);
                }
            }
        }
    }
    Ok(report)
}

/// The startup reconcile: settle runs left `running` by the previous app, and
/// hand the rest to the routine tick: the ones whose liveness is unknown to
/// re-ask, and the ones still running to sweep once their session ends.
pub async fn reconcile_on_startup(app: &tauri::AppHandle) {
    let sessions = AppSessions::new(app.clone());
    let app = app.clone();
    let report = crate::commands::await_blocking("routines_reconcile", move || {
        let store = app_store().ok_or_else(|| "no routine store".to_string())?;
        reconcile_at_startup(&app, &store, now_ms(), &sessions).map_err(|e| e.to_string())
    })
    .await;
    if let Err(e) = report {
        log::warn!(target: "routines", "routine restart reconcile failed: {e}");
    }
}

/// [`reconcile_on_startup`] over `store` and `sessions`, handing what is left
/// running to the app's registered routine ticker.
pub(crate) fn reconcile_at_startup<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    store: &RoutineStateStore,
    now: i64,
    sessions: &dyn Sessions,
) -> StoreResult<ReconcileReport> {
    use tauri::Manager;
    let report = reconcile_runs(store, now, sessions, None)?;
    if !report.interrupted.is_empty() || !report.unknown.is_empty() {
        log::info!(
            target: "routines",
            "routine restart reconcile: {} interrupted, {} still running, {} unknown",
            report.interrupted.len(),
            report.alive.len(),
            report.unknown.len()
        );
    }
    if let Some(ticker) = app.try_state::<super::tick::RoutineTicker>() {
        ticker.recheck_liveness(report.unknown.iter().cloned());
        ticker.watch_survivors(report.alive.iter().cloned());
    }
    Ok(report)
}

/// This app's routine store, at the process's CodeSense home.
pub(crate) fn app_store() -> Option<RoutineStateStore> {
    let home = crate::commands::paths::codesense_home_dir()?;
    match RoutineStateStore::open(&home) {
        Ok(store) => Some(store),
        Err(e) => {
            log::warn!(target: "routines", "routine store unavailable: {e}");
            None
        }
    }
}

pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
