// SPDX-License-Identifier: MIT
//! Session timing from the signals CodeSense already observes: the agent activity events (a
//! Claude hook, or a change in the host's activity snapshot of a Codex session) and the Codex
//! telemetry the receiver already ingests for tool-call counts. There is no new tracker: each signal adds a row
//! or a count to `session_turn`, and wall, active and idle time are derived when a record is read.
//!
//! A turn runs from the agent starting work to the agent stopping to wait. `ended_waiting_on` says
//! what it waited on, as a fact; no verdict is made about why it took as long as it did. Idle is
//! the gap from one turn's end to the next turn's start (or to the session's end).

use std::collections::HashMap;
use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use codesense_pty_core::activity::{Event, Snapshot, State};

use super::{db_err, UsageStore};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS session_turn (
    session_id       TEXT NOT NULL,
    turn_index       INTEGER NOT NULL,
    started_at_ms    INTEGER NOT NULL,
    ended_at_ms      INTEGER,
    ended_waiting_on TEXT,
    note             TEXT,
    tool_calls       INTEGER NOT NULL DEFAULT 0,
    -- Which signals reported this session's turns (a SOURCE_* name).
    source           TEXT,
    PRIMARY KEY (session_id, turn_index)
);
-- A session's end, kept apart from its record so it holds whichever is written first.
CREATE TABLE IF NOT EXISTS session_end (
    session_id TEXT PRIMARY KEY,
    ended_at_ms INTEGER NOT NULL
);
";

/// What a turn ended waiting on.
pub const WAITING_PROMPT: &str = "prompt";
pub const WAITING_PERMISSION: &str = "permission";
pub const WAITING_BLOCKED: &str = "blocked";
pub const WAITING_ERROR: &str = "error";
pub const WAITING_UNKNOWN: &str = "unknown";

/// Where a session's timing came from.
pub const SOURCE_HOOKS: &str = "claude-hooks";
/// Turn boundaries from the host's activity snapshots; tool-call counts from Codex telemetry.
pub const SOURCE_CODEX: &str = "codex-observer+otel-tools";
/// A headless run: its start and its completion are the turn, and Codex telemetry counts tools.
pub const SOURCE_HEADLESS: &str = "headless-capture";

/// One signal about a session's work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    /// An activity event, the vocabulary a Claude hook and a Codex snapshot change share. The
    /// `&'static str` names which source reported it.
    Activity(Event, &'static str),
    /// A `signal_progress` phase and its note.
    Progress { phase: String, note: Option<String> },
    /// Codex finished a tool call (`codex.tool_result`): the observer reports no tool counts.
    CodexTool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamped {
    pub session_id: String,
    pub at_ms: i64,
    pub signal: Signal,
}

/// One idle gap: the agent was waiting from `from_ms` to `to_ms` on `waiting_on`. `to_ms` is
/// `None` while the session is still waiting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdleGap {
    pub after_turn: i64,
    pub from_ms: i64,
    pub to_ms: Option<i64>,
    pub waiting_on: String,
    pub note: Option<String>,
}

/// A session's timing facts, derived from its turns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTiming {
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    /// Start to end, or to the end of the last finished turn while the session is open.
    pub wall_ms: i64,
    /// Time inside finished turns.
    pub active_ms: i64,
    /// `wall_ms - active_ms`.
    pub idle_ms: i64,
    pub turn_count: i64,
    pub tool_call_count: i64,
    pub timing_source: Option<String>,
    pub idle_gaps: Vec<IdleGap>,
}

/// Applies signals to the store, remembering the latest `signal_progress` per session.
#[derive(Default)]
pub struct TimingState {
    progress: HashMap<String, (String, Option<String>)>,
}

impl TimingState {
    pub fn apply(&mut self, store: &UsageStore, s: &Stamped) -> Result<(), String> {
        let id = &s.session_id;
        match &s.signal {
            Signal::Progress { phase, note } => {
                self.progress
                    .insert(id.clone(), (phase.clone(), note.clone()));
                Ok(())
            }
            Signal::Activity(event, source) => {
                let blocked = self
                    .progress
                    .get(id)
                    .filter(|(phase, _)| phase == "blocked");
                let ops = plan(*event, blocked.map(|(_, note)| note.as_deref()));
                if ops.contains(&Op::Forget) {
                    self.progress.remove(id);
                }
                run(store, id, s.at_ms, source, &ops)
            }
            // A tool result is telemetry about a turn that already happened. It counts there.
            Signal::CodexTool => store.note_late_tool(id, s.at_ms),
        }
    }
}

/// One step of the turn state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Op {
    /// Start a turn.
    Begin,
    /// Start a turn unless one is open: the agent resumed.
    Ensure,
    /// Count a tool call on the open turn.
    Tool,
    /// End the open turn, if any, waiting on `reason` (with a note).
    Close(&'static str, Option<String>),
    /// End the session at this time, once.
    EndSession,
    /// Drop the remembered `signal_progress`.
    Forget,
}

/// The transitions: what each activity event does to a session's turns. `blocked` is the note of
/// the latest `signal_progress` when its phase was `blocked`. Nothing else decides a turn's start
/// or end, whichever CLI reported the event.
pub(crate) fn plan(event: Event, blocked: Option<Option<&str>>) -> Vec<Op> {
    use Op::*;
    match event {
        Event::Submitted => vec![Forget, Close(WAITING_UNKNOWN, None), Begin],
        Event::ToolStarted => vec![Ensure, Tool],
        Event::ToolFinished => vec![Ensure],
        Event::Waiting => vec![Ensure, Close(WAITING_PERMISSION, None)],
        Event::Stopped => match blocked {
            Some(note) => vec![Close(WAITING_BLOCKED, note.map(str::to_owned))],
            None => vec![Close(WAITING_PROMPT, None)],
        },
        Event::Failed => vec![Close(WAITING_ERROR, None)],
        Event::Interrupted => vec![Close(WAITING_UNKNOWN, None)],
        Event::Ended => vec![Forget, Close(WAITING_UNKNOWN, None), EndSession],
        Event::Started
        | Event::SubagentStarted
        | Event::SubagentFinished
        | Event::Compacting
        | Event::Prompting => Vec::new(),
    }
}

fn run(store: &UsageStore, id: &str, at_ms: i64, source: &str, ops: &[Op]) -> Result<(), String> {
    for op in ops {
        match op {
            Op::Begin => store.begin_turn(id, at_ms, source)?,
            Op::Ensure => store.ensure_turn(id, at_ms, source)?,
            Op::Tool => store.note_tool(id)?,
            Op::Close(reason, note) => store.close_turn(id, at_ms, reason, note.as_deref())?,
            Op::EndSession => store.end_session(id, at_ms)?,
            Op::Forget => {}
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimeFacts {
    pub wall_ms: i64,
    pub active_ms: i64,
    pub idle_ms: i64,
    pub sessions: usize,
}

impl TimeFacts {
    /// Sum each session's observed time, including overlapping sessions.
    pub(super) fn add(&mut self, wall: i64, active: i64, idle: i64) {
        self.wall_ms += wall;
        self.active_ms += active;
        self.idle_ms += idle;
    }
}

impl UsageStore {
    /// Wall, active and idle time of `session` inside `[from, to]`, from its turns; `None` when it
    /// has no turn.
    pub(super) fn interval_time(
        &self,
        session: &str,
        from: i64,
        to: i64,
    ) -> Result<Option<(i64, i64, i64)>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT started_at_ms, ended_at_ms FROM session_turn WHERE session_id = ?1")
            .map_err(db_err)?;
        let turns: Vec<(i64, Option<i64>)> = stmt
            .query_map(params![session], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(db_err)?
            .collect::<Result<_, _>>()
            .map_err(db_err)?;
        if turns.is_empty() {
            return Ok(None);
        }
        let active: i64 = turns
            .iter()
            .map(|(start, end)| (end.unwrap_or(to).min(to) - (*start).max(from)).max(0))
            .sum();
        let wall = (to - from).max(0);
        Ok(Some((wall, active.min(wall), (wall - active).max(0))))
    }

    fn open_turn(&self, session: &str) -> Result<Option<i64>, String> {
        self.conn
            .query_row(
                "SELECT turn_index FROM session_turn
                 WHERE session_id = ?1 AND ended_at_ms IS NULL ORDER BY turn_index DESC LIMIT 1",
                params![session],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_err)
    }

    fn begin_turn(&self, session: &str, at_ms: i64, source: &str) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO session_turn (session_id, turn_index, started_at_ms, source)
                 VALUES (?1, (SELECT COALESCE(MAX(turn_index) + 1, 0) FROM session_turn WHERE session_id = ?1), ?2, ?3)",
                params![session, at_ms, source],
            )
            .map(|_| ())
            .map_err(db_err)
    }

    /// Start a turn when none is open: the agent resumed after waiting.
    fn ensure_turn(&self, session: &str, at_ms: i64, source: &str) -> Result<(), String> {
        if self.open_turn(session)?.is_none() {
            self.begin_turn(session, at_ms, source)?;
        }
        Ok(())
    }

    fn note_tool(&self, session: &str) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE session_turn SET tool_calls = tool_calls + 1
                 WHERE session_id = ?1 AND ended_at_ms IS NULL",
                params![session],
            )
            .map(|_| ())
            .map_err(db_err)
    }

    /// Count a tool call on the turn that was running at `at_ms`: the latest turn that had started
    /// by then. Telemetry reaches the app apart from the host's snapshots, so a result can arrive
    /// after its turn closed; it never opens a turn, and one before any turn counts nowhere.
    fn note_late_tool(&self, session: &str, at_ms: i64) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE session_turn SET tool_calls = tool_calls + 1
                 WHERE session_id = ?1 AND turn_index = (
                     SELECT turn_index FROM session_turn
                     WHERE session_id = ?1 AND started_at_ms <= ?2
                     ORDER BY started_at_ms DESC, turn_index DESC LIMIT 1)",
                params![session, at_ms],
            )
            .map(|_| ())
            .map_err(db_err)
    }

    /// End the open turn, if any.
    fn close_turn(
        &self,
        session: &str,
        at_ms: i64,
        waiting_on: &str,
        note: Option<&str>,
    ) -> Result<(), String> {
        let Some(index) = self.open_turn(session)? else {
            return Ok(());
        };
        let end = at_ms;
        self.conn
            .execute(
                "UPDATE session_turn SET ended_at_ms = MAX(?3, started_at_ms), ended_waiting_on = ?4, note = ?5
                 WHERE session_id = ?1 AND turn_index = ?2",
                params![session, index, end, waiting_on, note.map(crate::shared::secret_scan::redact_secrets)],
            )
            .map(|_| ())
            .map_err(db_err)
    }

    /// The first end stands: a session is over once, however many signals say so.
    fn end_session(&self, session: &str, at_ms: i64) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO session_end (session_id, ended_at_ms) VALUES (?1, ?2)",
                params![session, at_ms],
            )
            .map(|_| ())
            .map_err(db_err)
    }

    /// A session's timing, derived from its turns; `None` when no turn was observed.
    pub fn read_session_timing(&self, session: &str) -> Result<Option<SessionTiming>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT turn_index, started_at_ms, ended_at_ms, ended_waiting_on, note, tool_calls, source
                 FROM session_turn WHERE session_id = ?1 ORDER BY turn_index",
            )
            .map_err(db_err)?;
        let turns: Vec<(
            i64,
            i64,
            Option<i64>,
            Option<String>,
            Option<String>,
            i64,
            Option<String>,
        )> = stmt
            .query_map(params![session], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })
            .map_err(db_err)?
            .collect::<Result<_, _>>()
            .map_err(db_err)?;
        let Some(first) = turns.first() else {
            return Ok(None);
        };
        let ended_at_ms: Option<i64> = self
            .conn
            .query_row(
                "SELECT ended_at_ms FROM session_end WHERE session_id = ?1",
                params![session],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_err)?;
        let timing_source = first.6.clone();
        let started_at_ms = first.1;
        let active_ms = turns
            .iter()
            .filter_map(|t| t.2.map(|end| end - t.1))
            .sum::<i64>();
        let mut idle_gaps = Vec::new();
        for (i, turn) in turns.iter().enumerate() {
            let Some(end) = turn.2 else { continue };
            let to_ms = match turns.get(i + 1) {
                Some(next) => Some(next.1),
                None => ended_at_ms,
            };
            idle_gaps.push(IdleGap {
                after_turn: turn.0,
                from_ms: end,
                to_ms,
                waiting_on: turn
                    .3
                    .clone()
                    .unwrap_or_else(|| WAITING_UNKNOWN.to_string()),
                note: turn.4.clone(),
            });
        }
        let last_end = turns
            .iter()
            .filter_map(|t| t.2)
            .max()
            .unwrap_or(started_at_ms);
        let wall_ms = (ended_at_ms.unwrap_or(last_end) - started_at_ms).max(0);
        Ok(Some(SessionTiming {
            started_at_ms,
            ended_at_ms,
            wall_ms,
            active_ms,
            idle_ms: (wall_ms - active_ms).max(0),
            turn_count: turns.len() as i64,
            tool_call_count: turns.iter().map(|t| t.5).sum(),
            timing_source,
            idle_gaps,
        }))
    }
}

/// What a Codex session's last snapshot said, to tell what changed in the next one.
#[derive(Clone, Copy)]
struct Seen {
    state: State,
    generation: u64,
}

fn running(state: State) -> bool {
    matches!(state, State::Thinking | State::Working)
}

/// The activity events a Codex session's change from `prev` to `snapshot` stands for. Tool
/// events are left out: the observer's snapshot coalesces them, so tool-call counts come from
/// Codex's telemetry instead. With no `prev` (a first snapshot, or the first after an app
/// restart) the store may still hold an open turn, so the events are the ones that fit either
/// case: a running state keeps an open turn open or starts one, and a stopped state closes one,
/// with its end unknown because the app did not see it happen.
fn events_between(prev: Option<Seen>, snapshot: &Snapshot) -> Vec<Event> {
    let now = snapshot.state;
    let Some(prev) = prev else {
        return match now {
            State::Thinking | State::Working => vec![Event::ToolFinished],
            State::NeedsPermission => vec![Event::Waiting],
            State::Errored | State::Crashed => vec![Event::Failed],
            State::Done | State::Idle => vec![Event::Interrupted],
        };
    };
    let was = prev.state;
    let was_active = running(was) || was == State::NeedsPermission;
    let new_turn = snapshot.turn_generation > prev.generation;
    match now {
        State::Thinking | State::Working if !was_active || new_turn => vec![Event::Submitted],
        // A permission answered: the turn runs on as a new segment.
        State::Thinking | State::Working if was == State::NeedsPermission => {
            vec![Event::ToolFinished]
        }
        State::NeedsPermission if was != State::NeedsPermission => vec![Event::Waiting],
        State::Done if was_active => vec![Event::Stopped],
        State::Errored | State::Crashed if was_active => vec![Event::Failed],
        State::Idle if was_active => vec![Event::Interrupted],
        _ => Vec::new(),
    }
}

/// The last snapshot state seen per Codex session, so the next snapshot's change can be told.
#[derive(Default)]
struct SeenStates(HashMap<String, Seen>);

impl SeenStates {
    fn get(&self, session: &str) -> Option<Seen> {
        self.0.get(session).copied()
    }

    fn insert(&mut self, session: &str, seen: Seen) {
        self.0.insert(session.to_string(), seen);
    }

    /// Forget a session that has ended, so the map holds only live ones.
    fn forget(&mut self, session: &str) {
        self.0.remove(session);
    }
}

static SEEN: std::sync::LazyLock<std::sync::Mutex<SeenStates>> =
    std::sync::LazyLock::new(Default::default);

/// Record turn boundaries only for an observed CLI whose adapter uses snapshot timing.
pub fn note_snapshot(snapshot: &Snapshot) {
    if !codesense_pty_core::agent_cli::descriptor(snapshot.provider).usage_snapshot_timing
        || !snapshot.observed
    {
        return;
    }
    let events = {
        let Ok(mut seen) = SEEN.lock() else { return };
        let events = events_between(seen.get(&snapshot.session), snapshot);
        seen.insert(
            &snapshot.session,
            Seen {
                state: snapshot.state,
                generation: snapshot.turn_generation,
            },
        );
        events
    };
    for event in events {
        crate::session_conversation::note_hook(&snapshot.session, event);
        note_now(&snapshot.session, Signal::Activity(event, SOURCE_CODEX));
    }
}

/// A session's process exited or the session was removed: end it once, and forget the snapshot
/// state kept for it. Idempotent, and a no-op for a session with no turns or record.
pub fn note_session_end(session_id: &str) {
    if let Ok(mut seen) = SEEN.lock() {
        seen.forget(session_id);
    }
    note_now(session_id, Signal::Activity(Event::Ended, SOURCE_CODEX));
}

/// Whether `cli` reports its own turn boundaries (its hooks). Where it does, a headless run's
/// capture boundaries would be a second report of the same turn, so they are left out; for a CLI
/// with none (Codex) they are the fallback.
pub fn has_native_turn_hooks(cli: codesense_pty_core::agent_cli::AgentCli) -> bool {
    codesense_pty_core::agent_cli::descriptor(cli)
        .project_hooks
        .is_some()
}

/// The events a headless run's launch stands for: its turn begins, unless the CLI's own hooks
/// report that.
fn headless_start_events(cli: codesense_pty_core::agent_cli::AgentCli) -> Vec<Event> {
    if has_native_turn_hooks(cli) {
        Vec::new()
    } else {
        vec![Event::Submitted]
    }
}

/// A headless run starts: with no native turn hooks, the run is one turn, begun at launch.
pub fn note_headless_start(session_id: &str, cli: codesense_pty_core::agent_cli::AgentCli) {
    for event in headless_start_events(cli) {
        note_now(session_id, Signal::Activity(event, SOURCE_HEADLESS));
    }
}

/// The events a headless run's completion stands for, by its `signal_done` status vocabulary: a
/// clean finish stops its turn at the prompt, a failure fails it, and a run needing attention
/// (a timeout, for one) stops with its end unknown. None for a CLI whose hooks report the turn.
fn headless_end_events(cli: codesense_pty_core::agent_cli::AgentCli, status: &str) -> Vec<Event> {
    if has_native_turn_hooks(cli) {
        return Vec::new();
    }
    match status {
        "failed" => vec![Event::Failed],
        "needs-attention" => vec![Event::Interrupted],
        _ => vec![Event::Stopped],
    }
}

/// A headless run completes: end its turn for the reason its outcome gives, then the session.
pub fn note_headless_end(
    session_id: &str,
    cli: codesense_pty_core::agent_cli::AgentCli,
    status: &str,
) {
    let events = headless_end_events(cli, status);
    if events.is_empty() {
        return;
    }
    for event in events {
        note_now(session_id, Signal::Activity(event, SOURCE_HEADLESS));
    }
    note_session_end(session_id);
}

static SINK: OnceLock<Sender<Stamped>> = OnceLock::new();

/// The writer thread: one store connection, signals applied in the order they arrive. It ends
/// when every sender is dropped. `path` is read again whenever the store has to be reopened.
fn start_writer(
    path: impl Fn() -> Result<std::path::PathBuf, String> + Send + 'static,
) -> (Sender<Stamped>, Option<std::thread::JoinHandle<()>>) {
    let (tx, rx) = channel::<Stamped>();
    let spawned = std::thread::Builder::new().name("session-timing".into()).spawn(move || {
        let mut state = TimingState::default();
        let mut store: Option<UsageStore> = None;
        for stamped in rx {
            if store.is_none() {
                store = path()
                    .and_then(|path| UsageStore::open(&path))
                    .map_err(|e| log::warn!(target: "session_usage", "session timing store not opened: {e}"))
                    .ok();
            }
            if let Some(open) = &store {
                if let Err(e) = state.apply(open, &stamped) {
                    log::warn!(target: "session_usage", "session timing not recorded: {e}");
                    store = None;
                }
            }
        }
    });
    match spawned {
        Ok(handle) => (tx, Some(handle)),
        Err(e) => {
            log::warn!(target: "session_usage", "session timing thread not started: {e}");
            (tx, None)
        }
    }
}

fn sink() -> &'static Sender<Stamped> {
    SINK.get_or_init(|| start_writer(super::store_path).0)
}

/// Record a signal about `session_id` at `at_ms`. Fire and forget: the writer is one thread with
/// its own store connection, so a hook never waits on the database. A test build records nothing.
pub fn note(session_id: &str, at_ms: i64, signal: Signal) {
    if cfg!(test) {
        return;
    }
    let _ = sink().send(Stamped {
        session_id: session_id.to_string(),
        at_ms,
        signal,
    });
}

/// [`note`] now.
pub fn note_now(session_id: &str, signal: Signal) {
    note(session_id, chrono::Utc::now().timestamp_millis(), signal);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_usage::record::SessionRecord;
    use crate::session_usage::tests::store;
    use codesense_pty_core::agent_cli::AgentCli;

    fn hook(session: &str, at_ms: i64, event: &str) -> Stamped {
        Stamped {
            session_id: session.into(),
            at_ms,
            signal: Signal::Activity(
                crate::agent_state::hook_event(event).expect("a mapped hook"),
                SOURCE_HOOKS,
            ),
        }
    }

    fn snapshot(state: State, generation: u64) -> Snapshot {
        Snapshot {
            session: "cx".into(),
            identity: codesense_pty_core::activity::Identity {
                host_boot_id: "b".into(),
                session_incarnation: "i".into(),
                invocation_id: "inv".into(),
            },
            revision: 1,
            turn_generation: generation,
            provider: codesense_pty_core::agent_cli::AgentCli::Codex,
            state,
            observed: true,
            limited: false,
            exit: None,
            asks: Vec::new(),
            reason: None,
            tool_open: None,
            activity_age_ms: None,
            subagents_open: 0,
        }
    }

    #[test]
    fn a_codex_turn_ending_stores_the_conversation_whether_it_stopped_or_was_interrupted() {
        use crate::session_conversation::{nth_uuid, TestWriter};
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let w = TestWriter::install(root.join("usage.sqlite"), root.join("conv"), root.join("home"));
        for (n, end_state, session) in [(1u8, State::Done, "cx-done"), (2, State::Idle, "cx-int")] {
            w.seed_codex(session, &nth_uuid(n));
            let at = |state, generation| {
                let mut s = snapshot(state, generation);
                s.session = session.into();
                note_snapshot(&s);
            };
            at(State::Thinking, 1);
            assert!(SEEN.lock().unwrap().get(session).is_some());
            assert!(w.stored(session).is_none(), "{session}: the turn is still running");
            at(end_state, 1);
            let (conversation, calls) = w.stored(session).unwrap_or_else(|| panic!("{session}: stored"));
            assert_eq!(calls.len(), 3, "{session}");
            assert!(conversation.transcript.unavailable.contains_key("toolCall.isError"));
            note_session_end(session);
        }
    }

    #[test]
    fn snapshots_without_observed_snapshot_timing_do_not_checkpoint_or_enter_seen() {
        use crate::session_conversation::{nth_uuid, TestWriter};
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let w = TestWriter::install(root.join("usage.sqlite"), root.join("conv"), root.join("home"));
        for (n, provider, observed, session) in [
            (31u8, AgentCli::Claude, true, "snapshot-excluded-claude"),
            (32, AgentCli::OpenCode, true, "snapshot-excluded-opencode"),
            (33, AgentCli::Codex, false, "snapshot-unobserved-codex"),
        ] {
            // A capturable record makes an accidental checkpoint observable regardless of
            // which provider the activity snapshot names.
            w.seed_codex(session, &nth_uuid(n));
            for state in [State::Thinking, State::Done, State::Idle] {
                let mut s = snapshot(state, 1);
                s.session = session.into();
                s.provider = provider;
                s.observed = observed;
                note_snapshot(&s);
                assert!(SEEN.lock().unwrap().get(session).is_none(), "{session}");
                assert!(w.stored(session).is_none(), "{session}");
            }
        }
        assert_eq!(w.runs.get(), 0);
    }

    fn feed(state: &mut TimingState, store: &UsageStore, events: &[Stamped]) {
        for e in events {
            state.apply(store, e).unwrap();
        }
    }

    #[test]
    fn two_turns_and_a_gap_give_the_gap_and_its_reason() {
        let (_d, store) = store();
        store
            .write_session_record(&SessionRecord::new("tab"))
            .unwrap();
        let mut state = TimingState::default();
        feed(
            &mut state,
            &store,
            &[
                hook("tab", 1_000, "UserPromptSubmit"),
                hook("tab", 2_000, "PreToolUse"),
                hook("tab", 3_000, "PostToolUse"),
                hook("tab", 4_000, "PreToolUse"),
                hook("tab", 6_000, "Stop"),
                // The person takes 30s, then prompts again.
                hook("tab", 36_000, "UserPromptSubmit"),
                hook("tab", 40_000, "Stop"),
                hook("tab", 45_000, "SessionEnd"),
            ],
        );
        let t = store.read_session_timing("tab").unwrap().unwrap();
        assert_eq!(t.turn_count, 2);
        assert_eq!(t.tool_call_count, 2);
        assert_eq!(t.active_ms, 5_000 + 4_000);
        assert_eq!(t.wall_ms, 44_000);
        assert_eq!(t.idle_ms, 35_000);
        assert_eq!(t.timing_source.as_deref(), Some(SOURCE_HOOKS));
        assert_eq!(t.idle_gaps[0].from_ms, 6_000);
        assert_eq!(t.idle_gaps[0].to_ms, Some(36_000));
        assert_eq!(t.idle_gaps[0].waiting_on, WAITING_PROMPT);
        assert_eq!(
            t.idle_gaps[1].to_ms,
            Some(45_000),
            "the last gap runs to the session's end"
        );
    }

    #[test]
    fn what_a_turn_waited_on_is_a_fact_for_each_ending() {
        let (_d, store) = store();
        let mut state = TimingState::default();
        feed(
            &mut state,
            &store,
            &[
                hook("s", 0, "UserPromptSubmit"),
                hook("s", 10, "PermissionRequest"),
                // Resumed after the person allowed it.
                hook("s", 20, "PostToolUse"),
                hook("s", 30, "StopFailure"),
                hook("s", 40, "UserPromptSubmit"),
                Stamped {
                    session_id: "s".into(),
                    at_ms: 45,
                    signal: Signal::Progress {
                        phase: "blocked".into(),
                        note: Some("needs the staging key".into()),
                    },
                },
                hook("s", 50, "Stop"),
                hook("s", 60, "UserPromptSubmit"),
                hook("s", 70, "SessionEnd"),
            ],
        );
        let t = store.read_session_timing("s").unwrap().unwrap();
        let reasons: Vec<_> = t.idle_gaps.iter().map(|g| g.waiting_on.as_str()).collect();
        assert_eq!(reasons, ["permission", "error", "blocked", "unknown"]);
        assert_eq!(
            t.idle_gaps[2].note.as_deref(),
            Some("needs the staging key")
        );
    }

    #[test]
    fn timing_survives_a_reopen_mid_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        let mut state = TimingState::default();
        {
            let store = UsageStore::open(&path).unwrap();
            store
                .write_session_record(&SessionRecord::new("tab"))
                .unwrap();
            feed(
                &mut state,
                &store,
                &[
                    hook("tab", 100, "UserPromptSubmit"),
                    hook("tab", 200, "PreToolUse"),
                    hook("tab", 300, "Stop"),
                ],
            );
            feed(
                &mut state,
                &store,
                &[
                    hook("tab", 400, "UserPromptSubmit"),
                    hook("tab", 450, "PreToolUse"),
                ],
            );
        }
        // The app restarts with the second turn still open; a new connection carries on.
        let mut state = TimingState::default();
        let store = UsageStore::open(&path).unwrap();
        feed(
            &mut state,
            &store,
            &[hook("tab", 500, "PreToolUse"), hook("tab", 600, "Stop")],
        );
        let t = store.read_session_timing("tab").unwrap().unwrap();
        assert_eq!((t.turn_count, t.tool_call_count), (2, 3));
        assert_eq!(t.active_ms, 200 + 200);
        assert_eq!(t.ended_at_ms, None);
    }

    #[test]
    fn codex_snapshot_changes_stand_for_the_shared_activity_events() {
        let step = |was: Option<(State, u64)>, now: State, generation: u64| {
            events_between(
                was.map(|(state, generation)| Seen { state, generation }),
                &snapshot(now, generation),
            )
        };
        assert_eq!(step(None, State::Thinking, 1), [Event::ToolFinished]);
        assert_eq!(step(None, State::Done, 1), [Event::Interrupted]);
        assert_eq!(step(Some((State::Thinking, 1)), State::Working, 1), []);
        assert_eq!(
            step(Some((State::Working, 1)), State::NeedsPermission, 1),
            [Event::Waiting]
        );
        assert_eq!(
            step(Some((State::NeedsPermission, 1)), State::Working, 1),
            [Event::ToolFinished]
        );
        assert_eq!(
            step(Some((State::Working, 1)), State::Done, 1),
            [Event::Stopped]
        );
        assert_eq!(
            step(Some((State::Done, 1)), State::Thinking, 2),
            [Event::Submitted]
        );
        assert_eq!(
            step(Some((State::Working, 2)), State::Errored, 2),
            [Event::Failed]
        );
        assert_eq!(
            step(Some((State::Working, 2)), State::Idle, 2),
            [Event::Interrupted]
        );
        assert_eq!(step(Some((State::Done, 2)), State::Done, 2), []);
    }

    #[test]
    fn codex_turns_come_from_snapshots_and_tool_counts_from_telemetry() {
        let (_d, store) = store();
        store
            .write_session_record(&SessionRecord::new("cx"))
            .unwrap();
        let mut state = TimingState::default();
        let mut seen: Option<Seen> = None;
        // The host's snapshots and the telemetry's tool results, interleaved by time.
        let mut feed_snapshot =
            |state: &mut TimingState, at_ms: i64, now: State, generation: u64| {
                for event in events_between(seen, &snapshot(now, generation)) {
                    state
                        .apply(
                            &store,
                            &Stamped {
                                session_id: "cx".into(),
                                at_ms,
                                signal: Signal::Activity(event, SOURCE_CODEX),
                            },
                        )
                        .unwrap();
                }
                seen = Some(Seen {
                    state: now,
                    generation,
                });
            };
        feed_snapshot(&mut state, 1_000, State::Thinking, 1);
        for at_ms in [2_000, 3_000] {
            state
                .apply(
                    &store,
                    &Stamped {
                        session_id: "cx".into(),
                        at_ms,
                        signal: Signal::CodexTool,
                    },
                )
                .unwrap();
        }
        feed_snapshot(&mut state, 5_000, State::Done, 1);
        feed_snapshot(&mut state, 9_000, State::Thinking, 2);
        state
            .apply(
                &store,
                &Stamped {
                    session_id: "cx".into(),
                    at_ms: 9_500,
                    signal: Signal::CodexTool,
                },
            )
            .unwrap();
        let t = store.read_session_timing("cx").unwrap().unwrap();
        assert_eq!((t.turn_count, t.tool_call_count), (2, 3));
        assert_eq!(t.active_ms, 4_000, "the second turn is still open");
        assert_eq!(t.timing_source.as_deref(), Some(SOURCE_CODEX));
        assert_eq!(t.idle_gaps[0].from_ms, 5_000);
        assert_eq!(t.idle_gaps[0].to_ms, Some(9_000));
        assert_eq!(t.idle_gaps[0].waiting_on, WAITING_PROMPT);
    }

    #[test]
    fn a_session_with_no_turns_has_no_timing() {
        let (_d, store) = store();
        assert_eq!(store.read_session_timing("none").unwrap(), None);
    }

    #[test]
    fn the_writer_thread_records_signals_into_a_store_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        UsageStore::open(&path)
            .unwrap()
            .write_session_record(&SessionRecord::new("tab"))
            .unwrap();
        let (tx, handle) = start_writer({
            let path = path.clone();
            move || Ok(path.clone())
        });
        for (at_ms, event) in [
            (10, "UserPromptSubmit"),
            (20, "PreToolUse"),
            (30, "Stop"),
            (90, "SessionEnd"),
        ] {
            tx.send(hook("tab", at_ms, event)).unwrap();
        }
        // Dropping the sender ends the writer once it has applied everything sent.
        drop(tx);
        handle.expect("the writer thread started").join().unwrap();
        let t = UsageStore::open(&path)
            .unwrap()
            .read_session_timing("tab")
            .unwrap()
            .unwrap();
        assert_eq!(
            (t.turn_count, t.tool_call_count, t.active_ms, t.wall_ms),
            (1, 1, 20, 80)
        );
        assert_eq!(t.ended_at_ms, Some(90));
    }

    fn at(session: &str, at_ms: i64, signal: Signal) -> Stamped {
        Stamped {
            session_id: session.into(),
            at_ms,
            signal,
        }
    }

    fn activity(session: &str, at_ms: i64, event: Event) -> Stamped {
        at(session, at_ms, Signal::Activity(event, SOURCE_CODEX))
    }

    /// A tool result that was delivered late, in a batch, counts on the turn it happened in and
    /// never opens one.
    #[test]
    fn a_late_tool_result_counts_on_its_own_turn_and_opens_none() {
        let (_d, store) = store();
        store
            .write_session_record(&SessionRecord::new("cx"))
            .unwrap();
        let mut state = TimingState::default();
        feed(
            &mut state,
            &store,
            &[
                activity("cx", 1_000, Event::Submitted),
                activity("cx", 5_000, Event::Stopped),
                // The batched export arrives after the host said Done.
                at("cx", 3_000, Signal::CodexTool),
                at("cx", 4_000, Signal::CodexTool),
                activity("cx", 20_000, Event::Submitted),
                activity("cx", 24_000, Event::Stopped),
            ],
        );
        let t = store.read_session_timing("cx").unwrap().unwrap();
        assert_eq!(t.turn_count, 2, "no phantom turn");
        assert_eq!(t.tool_call_count, 2);
        assert_eq!(t.active_ms, 4_000 + 4_000, "the wait is not active");
    }

    #[test]
    fn a_tool_result_before_any_turn_is_dropped() {
        let (_d, store) = store();
        let mut state = TimingState::default();
        feed(&mut state, &store, &[at("cx", 500, Signal::CodexTool)]);
        assert_eq!(store.read_session_timing("cx").unwrap(), None);
    }

    /// A normal exit after an idle wait ends the session at the exit, and ending twice keeps
    /// the first end.
    #[test]
    fn an_exit_after_an_idle_wait_ends_the_session_once_with_the_final_idle() {
        let (_d, store) = store();
        store
            .write_session_record(&SessionRecord::new("cx"))
            .unwrap();
        let mut state = TimingState::default();
        feed(
            &mut state,
            &store,
            &[
                activity("cx", 1_000, Event::Submitted),
                activity("cx", 5_000, Event::Stopped),
                activity("cx", 65_000, Event::Ended),
                activity("cx", 99_000, Event::Ended),
            ],
        );
        let t = store.read_session_timing("cx").unwrap().unwrap();
        assert_eq!(t.ended_at_ms, Some(65_000));
        assert_eq!((t.wall_ms, t.active_ms, t.idle_ms), (64_000, 4_000, 60_000));
        assert_eq!(t.idle_gaps[0].to_ms, Some(65_000));
    }

    /// After a restart the process remembers no snapshot, but the store still has the open turn:
    /// the first snapshot that shows the agent stopped closes it.
    #[test]
    fn the_first_snapshot_after_a_restart_closes_a_persisted_open_turn() {
        let (_d, store) = store();
        store
            .write_session_record(&SessionRecord::new("cx"))
            .unwrap();
        let mut state = TimingState::default();
        feed(
            &mut state,
            &store,
            &[activity("cx", 1_000, Event::Submitted)],
        );
        // The app restarts: nothing is remembered about this session.
        for (now, closes_as) in [
            (State::Done, WAITING_UNKNOWN),
            (State::Errored, WAITING_ERROR),
        ] {
            let (_d2, store2) = {
                let (d, s2) = crate::session_usage::tests::store();
                s2.write_session_record(&SessionRecord::new("cx")).unwrap();
                (d, s2)
            };
            let mut st = TimingState::default();
            feed(&mut st, &store2, &[activity("cx", 1_000, Event::Submitted)]);
            for event in events_between(None, &snapshot(now, 1)) {
                st.apply(
                    &store2,
                    &at("cx", 9_000, Signal::Activity(event, SOURCE_CODEX)),
                )
                .unwrap();
            }
            let t = store2.read_session_timing("cx").unwrap().unwrap();
            assert_eq!(t.turn_count, 1, "{now:?}: the same turn, not a new one");
            assert_eq!(t.idle_gaps[0].waiting_on, closes_as, "{now:?}");
        }
        // A running first snapshot keeps the open turn open instead of splitting it.
        for event in events_between(None, &snapshot(State::Working, 1)) {
            state
                .apply(
                    &store,
                    &at("cx", 2_000, Signal::Activity(event, SOURCE_CODEX)),
                )
                .unwrap();
        }
        let t = store.read_session_timing("cx").unwrap().unwrap();
        assert_eq!(t.turn_count, 1);
        assert_eq!(t.ended_at_ms, None);
    }

    #[test]
    fn a_finished_sessions_snapshot_state_is_forgotten() {
        let mut seen = SeenStates::default();
        seen.insert(
            "cx",
            Seen {
                state: State::Done,
                generation: 3,
            },
        );
        assert!(seen.get("cx").is_some());
        seen.forget("cx");
        assert!(seen.get("cx").is_none());
    }

    #[test]
    fn the_transition_table_decides_every_turn_start_and_end() {
        use Op::*;
        let table: Vec<(Event, Option<Option<&str>>, Vec<Op>)> = vec![
            (
                Event::Submitted,
                None,
                vec![Forget, Close(WAITING_UNKNOWN, None), Begin],
            ),
            (Event::ToolStarted, None, vec![Ensure, Tool]),
            (Event::ToolFinished, None, vec![Ensure]),
            (
                Event::Waiting,
                None,
                vec![Ensure, Close(WAITING_PERMISSION, None)],
            ),
            (Event::Stopped, None, vec![Close(WAITING_PROMPT, None)]),
            (
                Event::Stopped,
                Some(Some("held")),
                vec![Close(WAITING_BLOCKED, Some("held".into()))],
            ),
            (
                Event::Stopped,
                Some(None),
                vec![Close(WAITING_BLOCKED, None)],
            ),
            (Event::Failed, None, vec![Close(WAITING_ERROR, None)]),
            (Event::Interrupted, None, vec![Close(WAITING_UNKNOWN, None)]),
            (
                Event::Ended,
                None,
                vec![Forget, Close(WAITING_UNKNOWN, None), EndSession],
            ),
            (Event::Compacting, None, vec![]),
        ];
        for (event, blocked, ops) in table {
            assert_eq!(plan(event, blocked), ops, "{event:?} {blocked:?}");
        }
    }

    #[test]
    fn ending_a_session_forgets_its_snapshot_state() {
        SEEN.lock().unwrap().insert(
            "ended-cx",
            Seen {
                state: State::Done,
                generation: 1,
            },
        );
        note_session_end("ended-cx");
        assert!(SEEN.lock().unwrap().get("ended-cx").is_none());
    }

    /// Every source of a session's lifecycle feeds the one activity handler: hooks, host
    /// snapshots, process exit, and a headless run's start and completion. A source that bypasses
    /// it leaves its sessions without timing, so each is pinned here.
    #[test]
    fn every_lifecycle_source_calls_the_one_activity_handler() {
        fn body<'a>(source: &'a str, signature: &str) -> &'a str {
            let start = source
                .find(signature)
                .unwrap_or_else(|| panic!("{signature} moved"));
            let end = source[start..]
                .find("\n}\n")
                .map_or(source.len(), |e| start + e);
            &source[start..end]
        }
        let dot = include_str!("../mcp/agent_dot.rs");
        let host = include_str!("../commands/pty_host.rs");
        let dashboard = include_str!("../commands/dashboard_launch.rs");
        for (name, text, needle) in [
            (
                "Claude hooks",
                body(dot, "pub fn admit_hook<"),
                "timing::note_now(",
            ),
            ("host snapshots", host, "timing::note_snapshot("),
            (
                "process exit and removal",
                host,
                "timing::note_session_end(",
            ),
            (
                "headless start",
                body(dashboard, "pub(crate) async fn launch_headless_session("),
                "timing::note_headless_start(",
            ),
            (
                "headless completion",
                body(dashboard, "async fn finish_headless_session("),
                "timing::note_headless_end(",
            ),
        ] {
            assert!(
                text.contains(needle),
                "{name} does not feed the timing handler ({needle})"
            );
        }
    }

    /// A headless run is one turn: begun at launch, its tools counted from telemetry, ended by
    /// its completion with the reason its outcome gives, and the session ended with it.
    #[test]
    fn a_headless_run_is_one_turn_with_its_tools_and_its_end() {
        for (status, reason) in [
            ("success", WAITING_PROMPT),
            ("failed", WAITING_ERROR),
            ("needs-attention", WAITING_UNKNOWN),
        ] {
            let (_d, store) = store();
            let mut state = TimingState::default();
            let mut feed_all = |signals: Vec<Stamped>| feed(&mut state, &store, &signals);
            let headless = |at_ms, event| at("hl", at_ms, Signal::Activity(event, SOURCE_HEADLESS));
            let mut signals = vec![headless(1_000, Event::Submitted)];
            signals.push(at("hl", 2_000, Signal::CodexTool));
            signals.push(at("hl", 3_000, Signal::CodexTool));
            signals.extend(
                headless_end_events(AgentCli::Codex, status)
                    .into_iter()
                    .map(|e| headless(8_000, e)),
            );
            signals.push(headless(8_000, Event::Ended));
            feed_all(signals);
            let t = store.read_session_timing("hl").unwrap().unwrap();
            assert_eq!((t.turn_count, t.tool_call_count), (1, 2), "{status}");
            assert_eq!(
                (t.active_ms, t.wall_ms, t.idle_ms),
                (7_000, 7_000, 0),
                "{status}"
            );
            assert_eq!(t.ended_at_ms, Some(8_000), "{status}");
            assert_eq!(t.timing_source.as_deref(), Some(SOURCE_HEADLESS));
            assert_eq!(t.idle_gaps[0].waiting_on, reason, "{status}");
        }
    }

    /// The session's end and turns do not depend on its record existing: a run that finishes
    /// before its record is written still has its timing.
    #[test]
    fn timing_holds_without_a_record() {
        let (_d, store) = store();
        let mut state = TimingState::default();
        feed(
            &mut state,
            &store,
            &[
                at(
                    "quick",
                    100,
                    Signal::Activity(Event::Submitted, SOURCE_HEADLESS),
                ),
                at(
                    "quick",
                    200,
                    Signal::Activity(Event::Stopped, SOURCE_HEADLESS),
                ),
                at(
                    "quick",
                    200,
                    Signal::Activity(Event::Ended, SOURCE_HEADLESS),
                ),
            ],
        );
        let t = store.read_session_timing("quick").unwrap().unwrap();
        assert_eq!(
            (t.ended_at_ms, t.timing_source.as_deref()),
            (Some(200), Some(SOURCE_HEADLESS))
        );
    }

    /// Headless capture boundaries are the fallback for a CLI with no turn hooks. Claude has them,
    /// so its headless run is the one turn its hooks report, and Codex is unchanged.
    #[test]
    fn headless_capture_is_the_fallback_only_for_a_cli_with_no_turn_hooks() {
        assert!(has_native_turn_hooks(AgentCli::Claude));
        assert!(!has_native_turn_hooks(AgentCli::Codex));
        assert_eq!(headless_start_events(AgentCli::Claude), []);
        assert_eq!(headless_start_events(AgentCli::Codex), [Event::Submitted]);
        assert_eq!(headless_end_events(AgentCli::Claude, "success"), []);
        assert_eq!(
            headless_end_events(AgentCli::Codex, "success"),
            [Event::Stopped]
        );

        // A headless Claude run: its hooks report one prompt, one tool and one stop; the launch
        // and completion add nothing, so there is one turn, from the hooks.
        let (_d, store) = store();
        let mut state = TimingState::default();
        let mut signals = Vec::new();
        for event in headless_start_events(AgentCli::Claude) {
            signals.push(at("hc", 1_000, Signal::Activity(event, SOURCE_HEADLESS)));
        }
        signals.extend([
            hook("hc", 1_500, "UserPromptSubmit"),
            hook("hc", 2_000, "PreToolUse"),
            hook("hc", 6_000, "Stop"),
        ]);
        for event in headless_end_events(AgentCli::Claude, "success") {
            signals.push(at("hc", 6_500, Signal::Activity(event, SOURCE_HEADLESS)));
        }
        feed(&mut state, &store, &signals);
        let t = store.read_session_timing("hc").unwrap().unwrap();
        assert_eq!((t.turn_count, t.tool_call_count), (1, 1));
        assert_eq!(t.timing_source.as_deref(), Some("claude-hooks"));
    }
}
