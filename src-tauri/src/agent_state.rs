// SPDX-License-Identifier: MIT
//! Rust mirror of the canonical per-session agent state (TS home:
//! `src/shared/lib/agent-state.ts`).
//!
//! The resolver and its precedence rules live in TypeScript. This module
//! mirrors only what the Rust side writes: the state literals, the status hook
//! events the status receiver maps, the hook → state table, the MCP signal
//! (`signal_done` / `ask_user` / `signal_progress`) → state tables, and the
//! state → terminal-dot wire projection. A parity test reads the TS source so the two
//! cannot drift.
//!
//! It is also the home of the one exit classifier (`classify_exit`) the
//! headless run outcome uses; the frontend classifies the interactive
//! `pty-exit` with its mirror (`classifyExit`). Both are checked against one
//! shared fixture (`fixtures/agent-state/exit-classification.json`).

use codesense_pty_core::activity::Event;
use codesense_pty_core::agent_cli::{adapter, AgentCli, StatusHook};

/// Every canonical agent state. Keep in the TS `AGENT_STATES` order. `Blocked` is
/// distinct from `NeedsPermission`: the agent's own self-report that it is held
/// (`signal_progress {phase:"blocked"}`), carrying a reason, while
/// `NeedsPermission` is a real approval gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Working,
    Thinking,
    NeedsPermission,
    Blocked,
    Done,
    Errored,
    Crashed,
    Idle,
}

/// Every state, in the TS `AGENT_STATES` order.
pub const AGENT_STATES: &[AgentState] = &[
    AgentState::Working,
    AgentState::Thinking,
    AgentState::NeedsPermission,
    AgentState::Blocked,
    AgentState::Done,
    AgentState::Errored,
    AgentState::Crashed,
    AgentState::Idle,
];

impl AgentState {
    /// The stable wire token (the TS literal).
    pub fn as_wire(&self) -> &'static str {
        match self {
            AgentState::Working => "working",
            AgentState::Thinking => "thinking",
            AgentState::NeedsPermission => "needs-permission",
            AgentState::Blocked => "blocked",
            AgentState::Done => "done",
            AgentState::Errored => "errored",
            AgentState::Crashed => "crashed",
            AgentState::Idle => "idle",
        }
    }

    /// Parse a wire token; off-enum is `None`.
    pub fn from_wire(s: &str) -> Option<Self> {
        AGENT_STATES.iter().copied().find(|st| st.as_wire() == s)
    }

    /// The `terminal-status-update` wire status for this state. The dot vocabulary
    /// is coarser than the full state set — `thinking` and `crashed` fold onto
    /// their nearest existing literal, and `blocked` folds to `needs-permission`
    /// (the richer kind is carried through the Needs-you lane, not the dot). `idle`
    /// is the transient clear the frontend turns into "no status" (the TS
    /// projection's `undefined`).
    pub fn terminal_dot_wire(&self) -> &'static str {
        match self {
            AgentState::Working | AgentState::Thinking => "running",
            AgentState::NeedsPermission | AgentState::Blocked => "needs-permission",
            AgentState::Done => "done",
            AgentState::Errored | AgentState::Crashed => "error",
            AgentState::Idle => "idle",
        }
    }
}

/// The status hooks the status receiver maps (TS `CLAUDE_HOOK_EVENTS`): every hook
/// event at which a CLI's own settings run CodeSense's status hook, from its adapter,
/// each once (the first CLI's, where two name the same event). The workspace hook wiring
/// registers exactly these.
pub fn status_hooks() -> impl Iterator<Item = &'static StatusHook> {
    static HOOKS: std::sync::LazyLock<Vec<&'static StatusHook>> = std::sync::LazyLock::new(|| {
        let mut seen = std::collections::HashSet::new();
        AgentCli::ALL
            .into_iter()
            .flat_map(|cli| adapter(cli).status_hooks())
            .filter(|hook| seen.insert(hook.event))
            .collect()
    });
    HOOKS.iter().copied()
}

/// The status hook at `event`, if the receiver maps it.
fn status_hook(event: &str) -> Option<&'static StatusHook> {
    status_hooks().find(|hook| hook.event == event)
}

/// The activity event a status hook reports; `None` for a hook the receiver does not map.
pub fn hook_event(event: &str) -> Option<Event> {
    status_hook(event).map(|hook| hook.reports)
}

/// Status hook event → state (TS `CLAUDE_HOOK_STATE`), through the activity event it
/// reports. `None` for an event the receiver does not map.
pub fn hook_state(event: &str) -> Option<AgentState> {
    match status_hook(event)?.reports {
        Event::Stopped => Some(AgentState::Done),
        Event::Failed => Some(AgentState::Errored),
        Event::Waiting => Some(AgentState::NeedsPermission),
        // A turn is open between its submit and its stop. Inside it the model is
        // reasoning (`thinking`) except while a tool runs, which the tool's start and
        // finish bracket (`working`).
        Event::Submitted | Event::ToolFinished => Some(AgentState::Thinking),
        Event::ToolStarted => Some(AgentState::Working),
        Event::Ended => Some(AgentState::Idle),
        Event::Started
        | Event::Interrupted
        | Event::SubagentStarted
        | Event::SubagentFinished
        | Event::Compacting
        | Event::Prompting => None,
    }
}

/// The MCP `signal_done` status values (TS `SIGNAL_DONE_OUTCOMES`).
pub const SIGNAL_DONE_OUTCOMES: &[&str] = &["success", "failed", "needs-attention"];

/// MCP `signal_done` status → state (TS `SIGNAL_DONE_STATE`). `None` for a
/// status the tool rejects.
pub fn signal_done_state(outcome: &str) -> Option<AgentState> {
    match outcome {
        "success" => Some(AgentState::Done),
        "failed" => Some(AgentState::Errored),
        // A completed outcome does not imply a permission prompt.
        "needs-attention" => Some(AgentState::Done),
        _ => None,
    }
}

/// A blocking MCP `ask_user` elicitation is open (TS `ASK_USER_STATE`).
pub const ASK_USER_STATE: AgentState = AgentState::NeedsPermission;

/// The `ask_user` resolved: it is a tool call inside a live turn, so the agent
/// is working again (TS `ASK_RESOLVED_STATE`).
pub const ASK_RESOLVED_STATE: AgentState = AgentState::Working;

/// Whether a status hook may replace a `needs-permission` on display (TS
/// `hookClearsPermission`): a tool starting while blocked is a sibling tool call, not
/// the approval.
pub fn hook_clears_permission(event: &str) -> bool {
    status_hook(event).is_none_or(|hook| hook.reports != Event::ToolStarted)
}

/// MCP `signal_progress` phase → state (TS `SIGNAL_PROGRESS_STATE`). Only
/// `blocked` means "needs a human"; every other phase is progress. `blocked` is
/// its own state, distinct from a `PermissionRequest`'s
/// `needs-permission` — the agent's own self-report that it is held, carrying a
/// reason the FE surfaces as `Blocked: <note>`.
pub fn signal_progress_state(phase: crate::roles::Phase) -> AgentState {
    use crate::roles::Phase;
    match phase {
        Phase::Blocked => AgentState::Blocked,
        Phase::Planning | Phase::Dev | Phase::Review | Phase::Pr | Phase::Done => {
            AgentState::Working
        }
    }
}

/// How a process ended, as far as its exit status says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitClass {
    /// Exit code 0, no signal.
    Clean,
    /// Killed by a signal, or a known nonzero exit code.
    Abnormal,
    /// Neither a code nor a signal is known.
    Unknown,
}

impl ExitClass {
    /// The wire token (the TS `ExitClass` literal).
    pub fn as_wire(&self) -> &'static str {
        match self {
            ExitClass::Clean => "clean",
            ExitClass::Abnormal => "abnormal",
            ExitClass::Unknown => "unknown",
        }
    }
}

/// Classify an exit status (TS `classifyExit`). A signal wins over any code; an
/// empty signal name counts as none.
pub fn classify_exit(code: Option<i64>, signal: Option<&str>) -> ExitClass {
    if signal.is_some_and(|s| !s.is_empty()) {
        return ExitClass::Abnormal;
    }
    match code {
        Some(0) => ExitClass::Clean,
        Some(_) => ExitClass::Abnormal,
        None => ExitClass::Unknown,
    }
}

/// Classify a std process exit status (a signal death has no code).
pub fn classify_exit_status(status: Option<&std::process::ExitStatus>) -> ExitClass {
    let Some(status) = status else {
        return ExitClass::Unknown;
    };
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal().map(|n| n.to_string())
    };
    #[cfg(not(unix))]
    let signal: Option<String> = None;
    classify_exit(status.code().map(i64::from), signal.as_deref())
}

/// The last agent state each session's dot showed, keyed by session id, so
/// `list_sessions` can say whether an agent is working, waiting on a person or idle.
/// Fed from the dot's write path (admitted hooks and MCP signals alike), so it agrees
/// with the dot. In memory only: the next write after a restart refills it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TurnObservation {
    pub state: AgentState,
    pub open_tools: u32,
    pub activity_at: u64,
    pub generation: u64,
    pub announced: bool,
    pub incarnation: u64,
    pub exit: Option<LegacyAgentExit>,
}
/// Exit evidence attached to the existing legacy turn observation. Host snapshots
/// remain authoritative whenever the activity protocol supplies one.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LegacyAgentExit {
    pub sequence: u64,
    pub abnormal: bool,
    pub known: bool,
}
#[derive(Debug, Default)]
pub struct SessionTurnState {
    last: std::sync::Mutex<std::collections::HashMap<String, TurnObservation>>,
    exit_sequence: std::sync::atomic::AtomicU64,
}
impl SessionTurnState {
    pub fn record(&self, session_id: &str, state: AgentState) {
        if let Ok(mut last) = self.last.lock() {
            Self::store(&mut last, session_id, state);
        }
    }
    fn store(
        last: &mut std::collections::HashMap<String, TurnObservation>,
        session_id: &str,
        state: AgentState,
    ) {
        if state == AgentState::Idle {
            last.remove(session_id);
            return;
        }
        let previous = last.get(session_id).copied();
        let generation = previous.map_or(0, |p| p.generation);
        let open_tools = if matches!(state, AgentState::Done | AgentState::Errored) {
            0
        } else {
            previous.map_or(0, |p| p.open_tools)
        };
        last.insert(
            session_id.into(),
            TurnObservation {
                state,
                open_tools,
                generation,
                activity_at: chrono::Utc::now().timestamp_millis().max(0) as u64,
                incarnation: previous.map_or_else(
                    || chrono::Utc::now().timestamp_micros().max(0) as u64,
                    |p| p.incarnation,
                ),
                announced: previous.is_some_and(|p| p.announced),
                exit: None,
            },
        );
    }
    pub fn record_hook(&self, session_id: &str, event: &str) {
        let Some(mut state) = hook_state(event) else {
            return;
        };
        let Ok(mut last) = self.last.lock() else {
            return;
        };
        let old = last.get(session_id).copied();
        let tools = old.map_or(0, |p| p.open_tools);
        let tools = match event {
            "PreToolUse" => tools.saturating_add(1),
            "PostToolUse" | "PostToolUseFailure" => tools.saturating_sub(1),
            "Stop" | "StopFailure" | "SessionEnd" | "UserPromptSubmit" => 0,
            _ => tools,
        };
        if !hook_clears_permission(event)
            && old.is_some_and(|p| p.state == AgentState::NeedsPermission)
        {
            state = AgentState::NeedsPermission;
        }
        if state == AgentState::Thinking && tools > 0 {
            state = AgentState::Working;
        }
        Self::store(&mut last, session_id, state);
        if let Some(observation) = last.get_mut(session_id) {
            observation.open_tools = tools;
            observation.announced = true;
            if event == "UserPromptSubmit"
                || (event == "PreToolUse"
                    && old
                        .is_some_and(|p| matches!(p.state, AgentState::Done | AgentState::Errored)))
            {
                observation.generation += 1;
            }
        }
    }
    /// OSC command-end is the CLI's exit only after a CLI hook established it.
    /// A PTY exit belongs to that CLI only while its turn remains open.
    pub(crate) fn record_exit(
        &self,
        session: &str,
        code: Option<i64>,
        signalled: bool,
        pty: bool,
        requested: bool,
    ) -> bool {
        if requested {
            return false;
        }
        if !pty
            && code.is_some_and(|code| {
                [libc::SIGSTOP, libc::SIGTSTP, libc::SIGTTIN, libc::SIGTTOU]
                    .into_iter()
                    .any(|signal| code == i64::from(128 + signal))
            })
        {
            return false;
        }
        let Ok(mut last) = self.last.lock() else {
            return false;
        };
        let Some(observation) = last.get_mut(session) else {
            return false;
        };
        if !observation.announced
            || observation
                .exit
                .is_some_and(|exit| exit.known || code.is_none())
            || (pty
                && !matches!(
                    observation.state,
                    AgentState::Working
                        | AgentState::Thinking
                        | AgentState::NeedsPermission
                        | AgentState::Blocked
                ))
        {
            return false;
        }
        let abnormal = signalled
            || code.is_some_and(|code| code != 0)
            || (code.is_none()
                && matches!(
                    observation.state,
                    AgentState::Working
                        | AgentState::Thinking
                        | AgentState::NeedsPermission
                        | AgentState::Blocked
                ));
        observation.exit = Some(LegacyAgentExit {
            sequence: observation.exit.map_or_else(
                || {
                    self.exit_sequence
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                        + 1
                },
                |exit| exit.sequence,
            ),
            abnormal,
            known: code.is_some() || signalled,
        });
        observation.state = if abnormal {
            AgentState::Crashed
        } else if observation.state == AgentState::Errored {
            AgentState::Errored
        } else {
            AgentState::Done
        };
        observation.open_tools = 0;
        true
    }
    pub fn record_ready(&self, session_id: &str) {
        if let Ok(mut last) = self.last.lock() {
            if !last.contains_key(session_id) {
                Self::store(&mut last, session_id, AgentState::Done);
            }
        }
    }
    pub fn forget(&self, session_id: &str) {
        if let Ok(mut last) = self.last.lock() {
            last.remove(session_id);
        }
    }
    pub fn last(&self, session_id: &str) -> Option<AgentState> {
        self.observation(session_id).map(|p| p.state)
    }
    pub(crate) fn observation(&self, session_id: &str) -> Option<TurnObservation> {
        self.last.lock().ok()?.get(session_id).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AGENT_STATE_TS: &str = include_str!("../../src/shared/lib/agent-state.ts");

    /// The body between the first `open` after `const <name>` and its `close`.
    fn ts_literal_body<'a>(src: &'a str, name: &str, open: char, close: char) -> &'a str {
        let decl = format!("const {name}");
        let at = src
            .find(&decl)
            .unwrap_or_else(|| panic!("{decl} not found in agent-state.ts"));
        let start = at
            + src[at..]
                .find(open)
                .unwrap_or_else(|| panic!("{name}: no {open}"))
            + 1;
        let end = start
            + src[start..]
                .find(close)
                .unwrap_or_else(|| panic!("{name}: no {close}"));
        &src[start..end]
    }

    fn unquote(s: &str) -> String {
        s.trim().trim_matches('"').to_string()
    }

    /// A `[ "a", "b" ] as const` tuple, in order.
    fn ts_tuple(name: &str) -> Vec<String> {
        ts_literal_body(AGENT_STATE_TS, name, '[', ']')
            .split(',')
            .map(unquote)
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// A `{ key: "value", ... }` record; `undefined` maps to `None`. Comment
    /// lines are skipped.
    fn ts_record(name: &str) -> Vec<(String, Option<String>)> {
        ts_literal_body(AGENT_STATE_TS, name, '{', '}')
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with("//"))
            .map(|l| {
                let (k, v) = l
                    .trim_end_matches(',')
                    .split_once(':')
                    .unwrap_or_else(|| panic!("{name}: unparseable entry {l:?}"));
                let v = v.trim();
                (unquote(k), (v != "undefined").then(|| unquote(v)))
            })
            .collect()
    }

    #[test]
    fn agent_states_match_ts_tuple_in_order() {
        let rust: Vec<String> = AGENT_STATES
            .iter()
            .map(|s| s.as_wire().to_string())
            .collect();
        assert_eq!(rust, ts_tuple("AGENT_STATES"));
    }

    #[test]
    fn status_hook_events_match_ts_tuple_in_order() {
        let rust: Vec<String> = status_hooks().map(|h| h.event.to_string()).collect();
        assert_eq!(rust, ts_tuple("CLAUDE_HOOK_EVENTS"));
    }

    #[test]
    fn hook_state_matches_ts_table() {
        let ts = ts_record("CLAUDE_HOOK_STATE");
        assert_eq!(ts.len(), status_hooks().count(), "table is total: {ts:?}");
        for (event, state) in ts {
            assert_eq!(
                hook_state(&event).map(|s| s.as_wire().to_string()),
                state,
                "hook {event}"
            );
        }
    }

    #[test]
    fn terminal_dot_projection_matches_ts_table() {
        let ts = ts_record("AGENT_STATE_TERMINAL_DOT");
        assert_eq!(ts.len(), AGENT_STATES.len(), "projection is total: {ts:?}");
        for (state, dot) in ts {
            let st = AgentState::from_wire(&state)
                .unwrap_or_else(|| panic!("TS state {state:?} missing in Rust"));
            // The TS clear (`undefined`) is the wire's transient "idle".
            assert_eq!(
                st.terminal_dot_wire(),
                dot.as_deref().unwrap_or("idle"),
                "state {state}"
            );
        }
    }

    #[test]
    fn signal_done_state_matches_ts_table() {
        let ts = ts_record("SIGNAL_DONE_STATE");
        assert_eq!(ts_tuple("SIGNAL_DONE_OUTCOMES"), SIGNAL_DONE_OUTCOMES);
        assert_eq!(
            SIGNAL_DONE_OUTCOMES.len(),
            ts.len(),
            "table is total: {ts:?}"
        );
        for outcome in SIGNAL_DONE_OUTCOMES {
            assert!(signal_done_state(outcome).is_some(), "{outcome} maps");
        }
        for (outcome, state) in ts {
            assert_eq!(
                signal_done_state(&outcome).map(|s| s.as_wire().to_string()),
                state,
                "outcome {outcome}"
            );
        }
        assert_eq!(signal_done_state("done"), None);
    }

    fn ts_state_const(name: &str) -> String {
        let decl = format!("const {name}: AgentState = ");
        let at = AGENT_STATE_TS
            .find(&decl)
            .unwrap_or_else(|| panic!("{name} in TS"))
            + decl.len();
        let end = at + AGENT_STATE_TS[at..].find(';').expect("terminated");
        unquote(&AGENT_STATE_TS[at..end])
    }

    #[test]
    fn mcp_state_constants_match_ts() {
        for (name, state) in [
            ("ASK_USER_STATE", ASK_USER_STATE),
            ("ASK_RESOLVED_STATE", ASK_RESOLVED_STATE),
        ] {
            assert_eq!(state.as_wire(), ts_state_const(name), "{name}");
        }
    }

    #[test]
    fn only_pre_tool_use_leaves_needs_permission_standing() {
        assert!(AGENT_STATE_TS.contains("return event !== \"PreToolUse\";"));
        for hook in status_hooks() {
            assert_eq!(
                hook_clears_permission(hook.event),
                hook.event != "PreToolUse"
            );
        }
    }

    #[test]
    fn signal_progress_state_matches_ts_table() {
        let ts = ts_record("SIGNAL_PROGRESS_STATE");
        assert_eq!(
            ts.len(),
            crate::roles::PHASES.len(),
            "table is total: {ts:?}"
        );
        for (phase, state) in ts {
            let p = crate::roles::Phase::from_wire(&phase)
                .unwrap_or_else(|| panic!("TS phase {phase:?} missing in Rust"));
            assert_eq!(
                Some(signal_progress_state(p).as_wire().to_string()),
                state,
                "phase {phase}"
            );
        }
    }

    #[derive(serde::Deserialize)]
    struct ExitCase {
        code: Option<i64>,
        signal: Option<String>,
        requested: bool,
        class: String,
    }

    fn exit_cases() -> Vec<ExitCase> {
        #[derive(serde::Deserialize)]
        struct Fixture {
            cases: Vec<ExitCase>,
        }
        let raw = include_str!("fixtures/agent-state/exit-classification.json");
        serde_json::from_str::<Fixture>(raw)
            .expect("fixture parses")
            .cases
    }

    #[test]
    fn exit_classification_matches_the_shared_fixture() {
        let cases = exit_cases();
        assert!(cases.len() >= 10);
        for c in cases {
            let what = format!(
                "code {:?} signal {:?} requested {}",
                c.code, c.signal, c.requested
            );
            assert_eq!(
                classify_exit(c.code, c.signal.as_deref()).as_wire(),
                c.class,
                "{what}"
            );
        }
    }

    #[test]
    fn exit_class_literals_match_ts() {
        assert_eq!(ts_tuple("EXIT_CLASSES"), ["clean", "abnormal", "unknown"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_real_std_exit_status_classifies_by_code_and_signal() {
        use std::process::Command;
        let status = |script: &str| Command::new("sh").args(["-c", script]).status().unwrap();
        assert_eq!(
            classify_exit_status(Some(&status("exit 0"))),
            ExitClass::Clean
        );
        assert_eq!(
            classify_exit_status(Some(&status("exit 4"))),
            ExitClass::Abnormal
        );
        assert_eq!(
            classify_exit_status(Some(&status("kill -9 $$"))),
            ExitClass::Abnormal
        );
        assert_eq!(classify_exit_status(None), ExitClass::Unknown);
    }

    /// A number tuple `[17, 18] as const`.
    fn ts_numbers(name: &str) -> Vec<i32> {
        ts_tuple(name)
            .iter()
            .map(|n| n.parse().unwrap_or_else(|_| panic!("{name}: {n:?}")))
            .collect()
    }

    /// The stop-signal numbers the frontend treats as a suspend (128 + n is
    /// not an exit) are this host's STOP, TSTP, TTIN, TTOU.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn stop_signal_tables_match_this_hosts_signal_numbers() {
        let name = if cfg!(target_os = "macos") {
            "DARWIN_STOP_SIGNALS"
        } else {
            "LINUX_STOP_SIGNALS"
        };
        assert_eq!(
            ts_numbers(name),
            [libc::SIGSTOP, libc::SIGTSTP, libc::SIGTTIN, libc::SIGTTOU]
        );
    }

    #[test]
    fn wire_round_trips_and_rejects_off_enum() {
        for &s in AGENT_STATES {
            assert_eq!(AgentState::from_wire(s.as_wire()), Some(s));
        }
        for bad in ["", "running", "error", "Working", "needs_permission"] {
            assert_eq!(AgentState::from_wire(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_ready_agent_reads_idle_until_its_first_turn_and_never_overwrites_one() {
        let turns = SessionTurnState::default();
        turns.record_ready("s");
        assert_eq!(turns.last("s"), Some(AgentState::Done));
        turns.record_hook("s", "UserPromptSubmit");
        // A turn already began: readiness reported late leaves it standing.
        turns.record_ready("s");
        assert_eq!(turns.last("s"), Some(AgentState::Thinking));
    }
}
