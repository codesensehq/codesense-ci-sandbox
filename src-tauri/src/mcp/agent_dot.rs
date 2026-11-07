// SPDX-License-Identifier: MIT
//! The MCP explicit-signal → terminal-dot bridge.
//!
//! `signal_done`, `signal_progress` and `ask_user` are Tier-1 explicit signals
//! (the agent DECLARED its state). Each maps through the canonical agent state
//! (`crate::agent_state`, parity-tested against the TS model) and is projected
//! onto the same `terminal-status-update` wire the Claude-hook receiver uses, so
//! the dot has ONE write channel whatever the signal's source. Each write is the
//! newest Tier-1 declaration for its session and replaces the previous one; the
//! liveness floor only fills an empty dot, so it never down-ranks these.
//!
//! Each emit is tagged `source: "mcp"` and carries the Tier-1 input behind its
//! status as `agent_signal` (the TS `ExplicitSignal` shape), which the frontend
//! folds into its agent-state record exactly as it folds a hook's `hook_event`.
//!
//! These emits are additive: the handlers keep their existing `mcp:agent-done`,
//! `mcp:agent-progress` and `mcp:ask-user` emits (Phase pill, banner) untouched.
//!
//! The [`DotLedger`] reconciles the MCP writes with the hook receiver's writes
//! for the same session. Every dot write that goes through it is emitted while
//! the ledger lock is held, so the writes are totally ordered with its
//! bookkeeping.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::agent_state::{self, AgentState};

/// The dot's wire event (shared with the hook receiver, `http/routes/terminal.rs`).
pub const TERMINAL_STATUS_EVENT: &str = "terminal-status-update";

/// Identifies an MCP-sourced write on the shared wire (a hook write carries
/// `hook_event` instead).
const MCP_SOURCE: &str = "mcp";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct TerminalStatusEvent {
    tab_session_id: String,
    status: &'static str,
    source: &'static str,
    agent_signal: McpSignal,
}

/// The Tier-1 input behind an MCP dot write, serialized as the TS
/// `ExplicitSignal` it is folded in as.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum McpSignal {
    /// `signal_done(outcome)`.
    SignalDone { outcome: &'static str },
    /// A blocking `ask_user` opened.
    AskUser,
    /// The last open `ask_user` resolved.
    AskResolved,
    /// `signal_progress(phase)`.
    SignalProgress {
        #[serde(serialize_with = "phase_wire")]
        phase: crate::roles::Phase,
    },
}

fn phase_wire<S: serde::Serializer>(p: &crate::roles::Phase, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(p.as_wire())
}

impl McpSignal {
    /// `signal_done`'s outcome as a signal; `None` for an off-enum outcome.
    fn signal_done(outcome: &str) -> Option<Self> {
        let outcome = *agent_state::SIGNAL_DONE_OUTCOMES
            .iter()
            .find(|o| **o == outcome)?;
        agent_state::signal_done_state(outcome)?;
        Some(McpSignal::SignalDone { outcome })
    }

    fn state(self) -> AgentState {
        match self {
            McpSignal::SignalDone { outcome } => {
                agent_state::signal_done_state(outcome).unwrap_or(AgentState::NeedsPermission)
            }
            McpSignal::AskUser => agent_state::ASK_USER_STATE,
            McpSignal::AskResolved => agent_state::ASK_RESOLVED_STATE,
            McpSignal::SignalProgress { phase } => agent_state::signal_progress_state(phase),
        }
    }
}

/// True when `session_id` names an interactive PTY whose child is still
/// RUNNING — the only sessions whose dot should be written. Membership alone is
/// not enough: an exited session is retained in the PTY map for replay, and a
/// write to it would resurrect a dead tab's dot. A headless session (no tab) or
/// a warm-pool placeholder never gets a dot entry either.
/// A session its terminal host cannot be asked about gets no dot write: it may have
/// exited unseen.
pub fn is_dot_session<R: Runtime>(app: &AppHandle<R>, session_id: &str) -> bool {
    !session_id.is_empty()
        && !crate::terminal::warm_pool::is_warm_pool_id(session_id)
        && crate::commands::pty_backend::alive_session_ids(app, &[session_id])
            .known_alive(session_id)
}

/// Decides whether the session's dot may be written. It may ask a terminal host
/// over its socket, so the ledger runs it off its lock (see
/// [`DotLedger::lock_with_liveness`]).
pub type LiveCheck<R> = fn(&AppHandle<R>, &str) -> bool;

use codesense_pty_core::activity::arbiter::{DotState as SessionDot, HookAdmission};

#[derive(Default)]
pub struct DotLedger {
    by_session: Mutex<HashMap<String, SessionDot>>,
    /// The PTY exits the ledger forgot, recorded under the ledger lock: a
    /// liveness check made off the lock is stale once its session is among
    /// those forgotten since.
    exits: Mutex<Exits>,
}

/// The latest exits the ledger forgot, each numbered in order.
#[derive(Default)]
struct Exits {
    count: u64,
    recent: VecDeque<(u64, String)>,
}

/// How many forgotten exits are remembered for the liveness re-check.
const RECENT_EXITS: usize = 256;

impl Exits {
    fn forgot(&mut self, session_id: &str) {
        self.count += 1;
        if self.recent.len() == RECENT_EXITS {
            self.recent.pop_front();
        }
        self.recent.push_back((self.count, session_id.to_string()));
    }

    /// Whether `session_id` may have exited since exit number `since`: it is
    /// among those forgotten after it, or they are no longer all remembered.
    fn exited_since(&self, since: u64, session_id: &str) -> bool {
        if self.count == since {
            return false;
        }
        let remembered_from = self.recent.front().map_or(self.count + 1, |(n, _)| *n);
        remembered_from > since + 1
            || self
                .recent
                .iter()
                .any(|(n, id)| *n > since && id == session_id)
    }
}

/// How many times a liveness check is retried when its session keeps exiting
/// while it runs; past that the session is taken as not live (no dot is
/// written).
const LIVENESS_RETRIES: usize = 4;

type Sessions<'a> = MutexGuard<'a, HashMap<String, SessionDot>>;

/// Apply `f` to the session's entry (created on demand), dropping it when it
/// no longer carries anything.
fn update<T>(map: &mut Sessions<'_>, session_id: &str, f: impl FnOnce(&mut SessionDot) -> T) -> T {
    let entry = map.entry(session_id.to_string()).or_default();
    let out = f(entry);
    if entry.is_empty() {
        map.remove(session_id);
    }
    out
}

impl DotLedger {
    fn lock(&self) -> Sessions<'_> {
        self.by_session.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Run `live` off the lock — it may ask a terminal host over its socket —
    /// then take the lock, with whether the session is live. An exit of this
    /// session the ledger forgot meanwhile re-runs the check (another session's
    /// never does), so a signal racing the child's death
    /// neither revives the dot nor leaves an entry the exit path already forgot.
    fn lock_with_liveness(
        &self,
        session_id: &str,
        live: impl Fn() -> bool,
    ) -> (Sessions<'_>, bool) {
        let exits = || self.exits.lock().unwrap_or_else(|p| p.into_inner());
        for _ in 0..LIVENESS_RETRIES {
            let since = exits().count;
            let alive = live();
            let map = self.lock();
            if !exits().exited_since(since, session_id) {
                return (map, alive);
            }
        }
        (self.lock(), false)
    }

    /// An agent signal writes the dot, when `live` (see
    /// [`Self::lock_with_liveness`]). The write supersedes an open ask's restore
    /// and replaces any latch with its own: it latches when it is a failure the
    /// operator must act on.
    fn agent_write(
        &self,
        session_id: &str,
        live: impl Fn() -> bool,
        signal: McpSignal,
        emit: impl FnOnce(McpSignal),
    ) {
        let (mut map, alive) = self.lock_with_liveness(session_id, live);
        if !alive {
            return;
        }
        update(&mut map, session_id, |e| {
            e.declare(latches(signal.state()));
        });
        emit(signal);
    }

    /// An ask opened: the dot is (re)set to `needs-permission`, the newest
    /// write again. The agent is still going, so a latch is released.
    /// Returns false (nothing written) when the session is not `live`.
    fn open_ask(&self, session_id: &str, live: impl Fn() -> bool, emit: impl FnOnce()) -> bool {
        let (mut map, alive) = self.lock_with_liveness(session_id, live);
        if !alive {
            return false;
        }
        update(&mut map, session_id, |e| {
            e.open_ask();
        });
        emit();
        true
    }

    /// An ask resolved. When it was the last open ask for the session, nothing
    /// else wrote the dot meanwhile, and the session is `live`, run `restore`
    /// (under the lock, so no other write can land between the check and the
    /// emit).
    fn close_ask(&self, session_id: &str, live: impl Fn() -> bool, restore: impl FnOnce()) {
        let (mut map, alive) = self.lock_with_liveness(session_id, live);
        let Some(entry) = map.get_mut(session_id) else {
            return;
        };
        let should_restore = entry.close_ask();
        if entry.is_empty() {
            map.remove(session_id);
        }
        if should_restore && alive {
            restore();
        }
    }

    /// A Claude hook wants to write the dot. Runs `emit` and returns true when
    /// admitted; returns false (the hook is not emitted, so neither the dot nor
    /// the frontend's agent record sees it) when the entry suppresses `event`:
    /// a latched failure suppresses every hook.
    ///
    /// `UserPromptSubmit` and `SessionEnd` (a new turn, the agent session
    /// ending) are always admitted: they release the latch and cancel an open
    /// ask's restore (an ask left open from an earlier turn is not blocking
    /// this one).
    ///
    /// Any other admitted hook also cancels an open ask's restore, except a
    /// `PreToolUse`: it cannot clear `needs-permission` on display (a sibling
    /// tool call, `agent_state::hook_clears_permission`), so the ask's
    /// `needs-permission` still stands and its resolution must restore.
    fn admit_hook(&self, session_id: &str, event: &str, emit: impl FnOnce()) -> bool {
        let mut map = self.lock();
        let decision = match map.get_mut(session_id) {
            None => HookAdmission::Admit,
            Some(entry) => {
                let decision = entry.admit_hook(event);
                if entry.is_empty() {
                    map.remove(session_id);
                }
                decision
            }
        };
        let admitted = decision == HookAdmission::Admit;
        if admitted {
            emit();
        }
        admitted
    }

    /// The session's PTY exited: drop everything, so an ask resolved by the
    /// death sweep afterwards finds nothing to restore.
    pub fn forget(&self, session_id: &str) {
        let mut map = self.lock();
        map.remove(session_id);
        self.exits
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .forgot(session_id);
    }

    #[cfg(test)]
    pub(crate) fn is_tracking(&self, session_id: &str) -> bool {
        self.lock().contains_key(session_id)
    }
}

/// Process-wide ledger: the MCP handler, the hook receiver and the PTY exit
/// path all mark it, and none owns the others.
pub fn ledger() -> &'static DotLedger {
    static LEDGER: OnceLock<DotLedger> = OnceLock::new();
    LEDGER.get_or_init(DotLedger::default)
}

fn turn_state<R: Runtime>(
    app: &AppHandle<R>,
) -> Option<tauri::State<'_, crate::commands::dashboard_launch::DashboardLaunchState>> {
    app.try_state::<crate::commands::dashboard_launch::DashboardLaunchState>()
}

/// Record the state a dot write shows in the launch registry's turn-state cache, so
/// `list_sessions` reads what the dot reads. Called from both dot write paths (an
/// MCP signal's emit here, an admitted hook's emit through `record_hook`), under the
/// ledger lock like the emit.
fn record_turn_state<R: Runtime>(app: &AppHandle<R>, session_id: &str, state: AgentState) {
    if let Some(launch) = turn_state(app) {
        launch.turns().record(session_id, state);
    }
}

fn emit_signal<R: Runtime>(app: &AppHandle<R>, session_id: &str, signal: McpSignal) {
    record_turn_state(app, session_id, signal.state());
    let event = TerminalStatusEvent {
        tab_session_id: session_id.to_string(),
        status: signal.state().terminal_dot_wire(),
        source: MCP_SOURCE,
        agent_signal: signal,
    };
    if let Err(e) = app.emit(TERMINAL_STATUS_EVENT, &event) {
        log::warn!(target: "mcp", "agent dot: failed to emit {TERMINAL_STATUS_EVENT}: {e}");
    }
}

/// A Claude hook wants to write `session_id`'s dot: the hook receiver's entry
/// into the ledger ([`DotLedger::admit_hook`]). Runs `emit` and returns true
/// when admitted; false when suppressed, in which case nothing of the hook is
/// emitted.
pub fn admit_hook<R: Runtime>(
    app: &AppHandle<R>,
    ledger: &DotLedger,
    session_id: &str,
    event: &str,
    emit: impl FnOnce(),
) -> bool {
    let emit = || {
        if let Some(launch) = turn_state(app) {
            launch.turns().record_hook(session_id, event);
        }
        if let Some(reported) = crate::agent_state::hook_event(event) {
            crate::session_usage::timing::note_now(
                session_id,
                crate::session_usage::timing::Signal::Activity(
                    reported,
                    crate::session_usage::timing::SOURCE_HOOKS,
                ),
            );
            crate::session_conversation::note_hook(session_id, reported);
        }
        emit();
    };
    ledger.admit_hook(session_id, event, emit)
}

/// A declared failure the operator must act on holds against the same turn's
/// trailing hooks. `Blocked` is deliberately *not* latched : the
/// agent said it is held, and ANY new activity (hook event or progress tick)
/// should move the row on — the row's `Blocked: <note>` line clears with the
/// next observed signal.
fn latches(state: AgentState) -> bool {
    state == AgentState::Errored
}

fn agent_write<R: Runtime>(
    app: &AppHandle<R>,
    ledger: &DotLedger,
    session_id: &str,
    live: LiveCheck<R>,
    signal: McpSignal,
) {
    ledger.agent_write(
        session_id,
        || live(app, session_id),
        signal,
        |signal| emit_signal(app, session_id, signal),
    );
}

/// `signal_done(status)` → `done` / `errored` / `needs-permission`. An off-enum
/// status is a no-op (the tool rejects it first).
pub fn on_signal_done<R: Runtime>(
    app: &AppHandle<R>,
    ledger: &DotLedger,
    session_id: &str,
    status: &str,
    live: LiveCheck<R>,
) {
    let Some(signal) = McpSignal::signal_done(status) else {
        return;
    };
    agent_write(app, ledger, session_id, live, signal);
}

/// `signal_progress(phase)` → `blocked` for `blocked`, else `working`. The
/// `blocked` state projects onto the coarse `needs-permission` dot wire
/// (`AgentState::terminal_dot_wire`); the richer kind reaches the Needs-you lane
/// through `selectNeedsYouKind` reading the agent-state record directly.
pub fn on_signal_progress<R: Runtime>(
    app: &AppHandle<R>,
    ledger: &DotLedger,
    session_id: &str,
    phase: crate::roles::Phase,
    live: LiveCheck<R>,
) {
    agent_write(
        app,
        ledger,
        session_id,
        live,
        McpSignal::SignalProgress { phase },
    );
}

/// Holds an open `ask_user`'s `needs-permission` on the dot. A guard (not a
/// trailing call) so every exit of the tool call closes the ask:
/// [`AskDotGuard::resolve`] when the call returns an outcome to a waiting agent,
/// a plain drop when the call is abandoned (the client cancelled it, or dropped
/// the call mid-wait). Only a resolution restores `working`: an abandoned ask's
/// agent is not waiting on it, and was interrupted rather than resumed.
pub struct AskDotGuard<'a, R: Runtime> {
    app: AppHandle<R>,
    ledger: &'a DotLedger,
    session_id: String,
    live: LiveCheck<R>,
    resolved: bool,
}

impl<R: Runtime> AskDotGuard<'_, R> {
    /// The ask returned an outcome to the agent (answered, declined, dismissed,
    /// timed out, session swept): close it and restore `working`.
    pub fn resolve(mut self) {
        self.resolved = true;
    }
}

/// A blocking `ask_user` opened → `needs-permission`. `None` when `live`
/// (checked against the ledger's exits) says the session has no dot. When the guard
/// is resolved the dot clears back to `working`, unless another ask is still
/// open, a newer write replaced it meanwhile (an agent signal, a hook that
/// clears `needs-permission`, a new turn), the PTY exit path forgot the
/// session, or `live` says the child is no longer running (an exited session's
/// dot is cleared by `pty-exit`).
pub fn on_ask_user_open<'a, R: Runtime>(
    app: &AppHandle<R>,
    ledger: &'a DotLedger,
    session_id: &str,
    live: LiveCheck<R>,
) -> Option<AskDotGuard<'a, R>> {
    let opened = ledger.open_ask(
        session_id,
        || live(app, session_id),
        || emit_signal(app, session_id, McpSignal::AskUser),
    );
    opened.then(|| AskDotGuard {
        app: app.clone(),
        ledger,
        session_id: session_id.to_string(),
        live,
        resolved: false,
    })
}

impl<R: Runtime> Drop for AskDotGuard<'_, R> {
    fn drop(&mut self) {
        let (app, sid, live) = (&self.app, &self.session_id, self.live);
        let resolved = self.resolved;
        self.ledger.close_ask(
            sid,
            || resolved && live(app, sid),
            || emit_signal(app, sid, McpSignal::AskResolved),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesense_pty_core::domain::TerminalDomain;
    use std::sync::Arc;
    use tauri::{Listener, Manager};

    type Captured = Arc<Mutex<Vec<serde_json::Value>>>;

    /// Real PTY sessions through `codesense_pty_core`'s public API, so
    /// `is_dot_session` is exercised against the real registry.
    mod pty_fixture {
        use std::sync::{Arc, OnceLock};
        use std::time::{Duration, Instant};

        use codesense_pty_core::domain::{
            PortUnavailable, PtySessionId, TerminalDomain, TerminalDomainEvent, TerminalEventSink,
        };
        use codesense_pty_core::layout::ServerIdentity;
        use codesense_pty_core::runner::{LocalPtyRunner, PtyRunner, PtySpec};
        use codesense_pty_core::{SessionMeta, SessionRegistration, TerminalHost};
        use portable_pty::{CommandBuilder, PtySize};

        struct NoopSink;

        impl TerminalEventSink for NoopSink {
            fn emit(&self, _: TerminalDomainEvent) -> Result<(), PortUnavailable> {
                Ok(())
            }
        }

        fn runtime() -> &'static tokio::runtime::Runtime {
            static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
            RT.get_or_init(|| {
                tokio::runtime::Builder::new_current_thread()
                    .build()
                    .expect("pty fixture runtime")
            })
        }

        /// The in-process host, as the app manages it.
        pub fn host() -> Arc<TerminalHost> {
            Arc::new(TerminalHost::new(
                ServerIdentity::ephemeral("test"),
                Arc::new(NoopSink),
                runtime().handle().clone(),
            ))
        }

        /// Register a session running `sh -c script`.
        pub fn spawn(pty: &TerminalHost, id: &str, script: &str) {
            let mut command = CommandBuilder::new("sh");
            command.arg("-c");
            command.arg(script);
            let session = runtime()
                .block_on(LocalPtyRunner.spawn(PtySpec {
                    command,
                    size: PtySize {
                        rows: 24,
                        cols: 80,
                        pixel_width: 0,
                        pixel_height: 0,
                    },
                }))
                .expect("spawn");
            pty.register(
                session,
                SessionRegistration {
                    session_id: id.to_string(),
                    meta: SessionMeta::default(),
                    home_cwd: None,
                    osc_enabled: false,
                    is_pool_member: false,
                    scrollback: codesense_pty_core::emulator::DEFAULT_SCROLLBACK,
                },
            )
            .expect("register");
        }

        /// SIGKILL the session's child and wait until the registry sees it dead.
        pub fn kill_child(pty: &TerminalHost, id: &str) {
            let pid =
                pty.live_child_pids(&[PtySessionId::new(id)]).unwrap()[0].expect("live child");
            // SAFETY: signalling a child process this test spawned.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            wait_dead(pty, id);
        }

        pub fn wait_dead(pty: &TerminalHost, id: &str) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while pty
                .alive_session_ids(&[PtySessionId::new(id)])
                .unwrap()
                .contains(id)
            {
                assert!(Instant::now() < deadline, "{id} never exited");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    type MockHandle = AppHandle<tauri::test::MockRuntime>;

    fn app_with_capture() -> (tauri::App<tauri::test::MockRuntime>, Captured) {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let seen: Captured = Arc::default();
        let sink = seen.clone();
        app.listen(TERMINAL_STATUS_EVENT, move |event| {
            sink.lock()
                .unwrap()
                .push(serde_json::from_str(event.payload()).unwrap());
        });
        (app, seen)
    }

    fn statuses(seen: &Captured, session: &str) -> Vec<String> {
        seen.lock()
            .unwrap()
            .iter()
            .filter(|p| p["tab_session_id"] == session)
            .map(|p| p["status"].as_str().unwrap().to_string())
            .collect()
    }

    fn live(_: &MockHandle, _: &str) -> bool {
        true
    }

    fn gone(_: &MockHandle, _: &str) -> bool {
        false
    }

    /// The ask returned an outcome to its agent.
    fn resolve(ask: Option<AskDotGuard<'_, tauri::test::MockRuntime>>) {
        ask.expect("ask opened").resolve();
    }

    /// Drive a Claude hook through the ledger the way the receiver does: the
    /// event resolves to its state and is emitted only when admitted.
    fn hook(app: &MockHandle, ledger: &DotLedger, session: &str, event: &str) -> bool {
        let state = agent_state::hook_state(event).expect("mapped hook");
        admit_hook(app, ledger, session, event, || {
            let payload = serde_json::json!({
                "tab_session_id": session,
                "status": state.terminal_dot_wire(),
                "hook_event": event,
            });
            app.emit(TERMINAL_STATUS_EVENT, payload).unwrap();
        })
    }

    #[test]
    fn a_claude_stop_hook_stores_the_conversation_and_other_hooks_do_not() {
        use crate::session_conversation::{TestWriter, CLAUDE_ID, DEBOUNCE_MS};
        let (app, _seen) = app_with_capture();
        let ledger = DotLedger::default();
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let w = TestWriter::install(root.join("usage.sqlite"), root.join("conv"), root.join("home"));
        w.seed_claude("s-conv", CLAUDE_ID);
        for event in ["UserPromptSubmit", "PreToolUse", "PostToolUse"] {
            hook(app.handle(), &ledger, "s-conv", event);
        }
        assert!(w.stored("s-conv").is_none(), "no turn has ended");
        hook(app.handle(), &ledger, "s-conv", "Stop");
        let (conversation, calls) = w.stored("s-conv").expect("the Stop hook stored it");
        assert_eq!(conversation.transcript.turns.len(), 5);
        assert_eq!(calls.len(), 3);
        // A second turn ending within the minute is held; it runs when the minute is up.
        let runs = w.runs.get();
        hook(app.handle(), &ledger, "s-conv", "UserPromptSubmit");
        hook(app.handle(), &ledger, "s-conv", "Stop");
        assert_eq!(w.runs.get(), runs, "held by the debounce");
        w.advance(DEBOUNCE_MS);
        assert_eq!(w.runs.get(), runs + 1, "and run once when due");
    }

    #[test]
    fn signal_done_projects_each_outcome_onto_the_dot_wire() {
        let (app, seen) = app_with_capture();
        let ledger = DotLedger::default();
        for (outcome, session) in [
            ("success", "s-success"),
            ("failed", "s-failed"),
            ("needs-attention", "s-attn"),
        ] {
            on_signal_done(app.handle(), &ledger, session, outcome, live);
        }
        assert_eq!(statuses(&seen, "s-success"), ["done"]);
        assert_eq!(statuses(&seen, "s-failed"), ["error"]);
        assert_eq!(statuses(&seen, "s-attn"), ["done"]);
        // The hook receiver's wire shape, tagged as MCP-sourced and carrying
        // the Tier-1 input (the TS `ExplicitSignal`) behind the status.
        assert_eq!(
            seen.lock().unwrap()[0],
            serde_json::json!({
                "tab_session_id": "s-success",
                "status": "done",
                "source": "mcp",
                "agent_signal": { "kind": "signal-done", "outcome": "success" },
            })
        );
    }

    /// The variant after `s`, in a fixed order; `None` after the last. The
    /// match is exhaustive, so a new variant is a compile error here, and it
    /// only joins [`every_mcp_signal`] once it is linked into this chain.
    fn next_mcp_signal(s: McpSignal) -> Option<McpSignal> {
        match s {
            McpSignal::SignalDone { .. } => Some(McpSignal::AskUser),
            McpSignal::AskUser => Some(McpSignal::AskResolved),
            McpSignal::AskResolved => Some(McpSignal::SignalProgress {
                phase: crate::roles::Phase::Dev,
            }),
            McpSignal::SignalProgress { .. } => None,
        }
    }

    /// Every `McpSignal` variant, once, walked from the first.
    fn every_mcp_signal() -> Vec<McpSignal> {
        std::iter::successors(Some(McpSignal::SignalDone { outcome: "success" }), |s| {
            next_mcp_signal(*s)
        })
        .collect()
    }

    #[test]
    fn mcp_signal_kinds_are_the_ts_explicit_signal_kinds_minus_claude_hook() {
        const TS: &str = include_str!("../../../src/shared/lib/agent-state.ts");
        let decl = TS
            .find("export type ExplicitSignal =")
            .expect("ExplicitSignal in TS");
        let body = &TS[decl..decl + TS[decl..].find("\n\n").expect("ends")];
        let mut ts_kinds: Vec<&str> = body
            .split("kind: \"")
            .skip(1)
            .map(|rest| &rest[..rest.find('"').unwrap()])
            .collect();
        ts_kinds.sort_unstable();
        let mut rust_kinds: Vec<String> = every_mcp_signal()
            .into_iter()
            .map(|s| {
                serde_json::to_value(s).unwrap()["kind"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .chain(["claude-hook".to_string()])
            .collect();
        rust_kinds.sort_unstable();
        let unique: std::collections::BTreeSet<_> = rust_kinds.iter().collect();
        assert_eq!(unique.len(), rust_kinds.len(), "each variant once");
        assert_eq!(rust_kinds, ts_kinds);
        // Each kind's state is the TS table's (the constants are parity-tested
        // in `agent_state`); here, every variant resolves.
        for s in every_mcp_signal() {
            assert!(!s.state().terminal_dot_wire().is_empty());
        }
    }

    const SEQUENCE: &str = include_str!("fixtures/agent-dot-sequence.json");

    /// The shared golden sequence (the frontend replays the same file through
    /// its real listener): each step's emit, or none when a hook is suppressed.
    #[test]
    fn the_golden_mixed_hook_and_mcp_sequence_emits_exactly_the_fixture() {
        let (app, seen) = app_with_capture();
        let ledger = DotLedger::default();
        let golden: serde_json::Value = serde_json::from_str(SEQUENCE).unwrap();
        let sid = golden["tab_session_id"].as_str().unwrap();
        let mut ask = None;
        for step in golden["steps"].as_array().unwrap() {
            let name = step["step"].as_str().unwrap();
            let before = seen.lock().unwrap().len();
            match name {
                "ask_user open" => ask = on_ask_user_open(app.handle(), &ledger, sid, live),
                "ask_user resolve" => resolve(ask.take()),
                "ask_user abandon" => drop(ask.take()),
                _ => match name.split_once(' ').unwrap() {
                    // The probe is the frontend's; the ledger sees nothing.
                    ("liveness", _) => {}
                    ("hook", event) => {
                        hook(app.handle(), &ledger, sid, event);
                    }
                    ("signal_done", outcome) => {
                        on_signal_done(app.handle(), &ledger, sid, outcome, live)
                    }
                    ("signal_progress", phase) => {
                        let phase = crate::roles::Phase::from_wire(phase).unwrap();
                        on_signal_progress(app.handle(), &ledger, sid, phase, live)
                    }
                    _ => panic!("unknown step {name}"),
                },
            }
            let emitted = seen.lock().unwrap()[before..].to_vec();
            let expected: Vec<serde_json::Value> = Some(step["emitted"].clone())
                .filter(|e| !e.is_null())
                .into_iter()
                .collect();
            assert_eq!(emitted, expected, "step {name}");
        }
        assert!(!ledger.is_tracking(sid));
    }

    #[test]
    fn signal_progress_blocked_is_needs_permission_and_every_other_phase_is_running() {
        let (app, seen) = app_with_capture();
        let ledger = DotLedger::default();
        for &phase in crate::roles::PHASES {
            on_signal_progress(app.handle(), &ledger, "s1", phase, live);
        }
        let expected: Vec<&str> = crate::roles::PHASES
            .iter()
            .map(|p| {
                if *p == crate::roles::Phase::Blocked {
                    "needs-permission"
                } else {
                    "running"
                }
            })
            .collect();
        assert_eq!(statuses(&seen, "s1"), expected);
    }

    #[test]
    fn a_failed_finish_latches_against_the_same_turns_hooks_until_a_new_turn() {
        let (app, seen) = app_with_capture();
        let ledger = DotLedger::default();
        on_signal_done(app.handle(), &ledger, "s1", "failed", live);
        assert!(!hook(app.handle(), &ledger, "s1", "PostToolUse"));
        assert!(!hook(app.handle(), &ledger, "s1", "Stop"));
        assert_eq!(statuses(&seen, "s1"), ["error"]);
        // A new turn releases the latch; later hooks flow again.
        assert!(hook(app.handle(), &ledger, "s1", "UserPromptSubmit"));
        assert!(hook(app.handle(), &ledger, "s1", "Stop"));
        assert_eq!(statuses(&seen, "s1"), ["error", "running", "done"]);
        assert!(!ledger.is_tracking("s1"));
    }

    #[test]
    fn completed_attention_and_success_allow_trailing_hooks() {
        let (app, seen) = app_with_capture();
        let ledger = DotLedger::default();
        on_signal_done(app.handle(), &ledger, "attn", "needs-attention", live);
        assert!(hook(app.handle(), &ledger, "attn", "Stop"));
        assert_eq!(statuses(&seen, "attn"), ["done", "done"]);

        on_signal_done(app.handle(), &ledger, "ok", "success", live);
        assert!(!ledger.is_tracking("ok"));
        assert!(hook(app.handle(), &ledger, "ok", "Stop"));
        assert_eq!(statuses(&seen, "ok"), ["done", "done"]);
    }

    #[test]
    fn session_end_releases_the_latch_and_clears_the_dot() {
        let (app, seen) = app_with_capture();
        let ledger = DotLedger::default();
        on_signal_done(app.handle(), &ledger, "s1", "failed", live);
        assert!(!hook(app.handle(), &ledger, "s1", "Stop"));
        assert!(hook(app.handle(), &ledger, "s1", "SessionEnd"));
        assert_eq!(statuses(&seen, "s1"), ["error", "idle"]);
        assert!(!ledger.is_tracking("s1"));
    }

    #[test]
    fn a_new_agent_signal_or_pty_exit_releases_the_latch() {
        let (app, seen) = app_with_capture();
        let ledger = DotLedger::default();
        on_signal_done(app.handle(), &ledger, "s1", "failed", live);
        on_signal_progress(app.handle(), &ledger, "s1", crate::roles::Phase::Dev, live);
        assert!(hook(app.handle(), &ledger, "s1", "Stop"));
        assert_eq!(statuses(&seen, "s1"), ["error", "running", "done"]);

        on_signal_done(app.handle(), &ledger, "s2", "failed", live);
        ledger.forget("s2");
        assert!(hook(app.handle(), &ledger, "s2", "Stop"));
        assert_eq!(statuses(&seen, "s2"), ["error", "done"]);
    }

    #[test]
    fn a_self_reported_blocked_is_cleared_by_any_new_activity() {
        // `signal_progress {phase:"blocked"}` is distinct from a
        // real `PermissionRequest` — the agent said it is held, and ANY new
        // activity (hook event or progress tick) should move the row on. `Blocked`
        // is deliberately not in `latches`, so a trailing `PreToolUse` admits
        // and flips the dot back to `running`.
        let (app, seen) = app_with_capture();
        let ledger = DotLedger::default();
        on_signal_progress(
            app.handle(),
            &ledger,
            "sb",
            crate::roles::Phase::Blocked,
            live,
        );
        assert!(hook(app.handle(), &ledger, "sb", "PreToolUse"));
        // The dot moved: blocked projected to `needs-permission` on the wire,
        // then the hook pushed it to `running`.
        assert_eq!(statuses(&seen, "sb"), ["needs-permission", "running"]);
        // A fresh block followed by a progress tick also clears.
        on_signal_progress(
            app.handle(),
            &ledger,
            "sb2",
            crate::roles::Phase::Blocked,
            live,
        );
        on_signal_progress(app.handle(), &ledger, "sb2", crate::roles::Phase::Dev, live);
        assert_eq!(statuses(&seen, "sb2"), ["needs-permission", "running"]);
    }

    #[test]
    fn hooks_during_an_open_ask_are_admitted_and_those_that_clear_it_cancel_its_restore() {
        let (app, seen) = app_with_capture();
        let ledger = DotLedger::default();
        // A turn's Stop while an ask is open (e.g. one abandoned without the
        // server hearing) shows, and the late resolution does not revive working.
        for event in ["PostToolUse", "PostToolUseFailure", "Stop", "StopFailure"] {
            let ask = on_ask_user_open(app.handle(), &ledger, event, live);
            assert!(hook(app.handle(), &ledger, event, event), "{event}");
            resolve(ask);
            let wire = agent_state::hook_state(event).unwrap().terminal_dot_wire();
            assert_eq!(
                statuses(&seen, event),
                ["needs-permission", wire],
                "{event}"
            );
            assert!(!ledger.is_tracking(event));
        }
        // PreToolUse cannot clear needs-permission, so the restore still runs.
        let ask = on_ask_user_open(app.handle(), &ledger, "pre", live);
        assert!(hook(app.handle(), &ledger, "pre", "PreToolUse"));
        resolve(ask);
        assert_eq!(
            statuses(&seen, "pre"),
            ["needs-permission", "running", "running"]
        );
    }

    #[test]
    fn a_new_turn_or_session_end_cancels_an_open_asks_restore() {
        let (app, seen) = app_with_capture();
        let ledger = DotLedger::default();
        for (sid, release, wire) in [
            ("ups", "UserPromptSubmit", "running"),
            ("end", "SessionEnd", "idle"),
        ] {
            let ask = on_ask_user_open(app.handle(), &ledger, sid, live);
            assert!(hook(app.handle(), &ledger, sid, release));
            // The old ask times out later: it resolves, but restores nothing.
            resolve(ask);
            assert_eq!(statuses(&seen, sid), ["needs-permission", wire]);
            assert!(!ledger.is_tracking(sid));
        }
    }

    #[test]
    fn an_abandoned_ask_closes_without_restoring_working() {
        let (app, seen) = app_with_capture();
        let ledger = DotLedger::default();
        let ask = on_ask_user_open(app.handle(), &ledger, "s1", live);
        drop(ask);
        assert_eq!(statuses(&seen, "s1"), ["needs-permission"]);
        assert!(!ledger.is_tracking("s1"));
        // The session is not stuck: the next turn's hooks flow.
        assert!(hook(app.handle(), &ledger, "s1", "UserPromptSubmit"));
        assert!(hook(app.handle(), &ledger, "s1", "Stop"));
    }

    #[test]
    fn a_signal_racing_the_childs_death_writes_nothing_and_leaves_no_entry() {
        let (app, seen) = app_with_capture();
        app.manage(pty_fixture::host());
        let pty = app.state::<Arc<codesense_pty_core::TerminalHost>>();
        pty_fixture::spawn(&pty, "s1", "exit 0");
        pty_fixture::wait_dead(&pty, "s1");
        let ledger = DotLedger::default();
        // The real liveness check, against the ledger's exits.
        on_signal_done(app.handle(), &ledger, "s1", "failed", is_dot_session);
        on_signal_progress(
            app.handle(),
            &ledger,
            "s1",
            crate::roles::Phase::Blocked,
            is_dot_session,
        );
        assert!(on_ask_user_open(app.handle(), &ledger, "s1", is_dot_session).is_none());
        assert!(statuses(&seen, "s1").is_empty());
        assert!(!ledger.is_tracking("s1"));
    }

    #[test]
    fn ask_user_sets_needs_permission_and_resolution_restores_working() {
        let (app, seen) = app_with_capture();
        let ledger = DotLedger::default();
        let ask = on_ask_user_open(app.handle(), &ledger, "s1", live);
        assert_eq!(statuses(&seen, "s1"), ["needs-permission"]);
        resolve(ask);
        assert_eq!(statuses(&seen, "s1"), ["needs-permission", "running"]);
        assert!(!ledger.is_tracking("s1"), "a resolved ask leaves no entry");
    }

    #[test]
    fn a_newer_signal_during_the_ask_is_not_clobbered_on_resolution() {
        let (app, seen) = app_with_capture();
        let ledger = DotLedger::default();
        let ask = on_ask_user_open(app.handle(), &ledger, "s1", live);
        on_signal_done(app.handle(), &ledger, "s1", "success", live);
        resolve(ask);
        assert_eq!(statuses(&seen, "s1"), ["needs-permission", "done"]);

        // A hook write supersedes the restore too.
        let ask = on_ask_user_open(app.handle(), &ledger, "s2", live);
        assert!(hook(app.handle(), &ledger, "s2", "PermissionRequest"));
        resolve(ask);
        assert_eq!(
            statuses(&seen, "s2"),
            ["needs-permission", "needs-permission"]
        );
        assert!(!ledger.is_tracking("s2"));
    }

    #[test]
    fn a_pre_tool_use_during_an_ask_does_not_cancel_its_restore() {
        let (app, seen) = app_with_capture();
        let ledger = DotLedger::default();
        let ask = on_ask_user_open(app.handle(), &ledger, "s1", live);
        // Emitted, but it cannot clear the ask's needs-permission on display.
        assert!(hook(app.handle(), &ledger, "s1", "PreToolUse"));
        resolve(ask);
        assert_eq!(
            statuses(&seen, "s1"),
            ["needs-permission", "running", "running"]
        );
        assert_eq!(
            seen.lock().unwrap().last().unwrap()["agent_signal"],
            serde_json::json!({ "kind": "ask-resolved" })
        );
    }

    #[test]
    fn overlapping_asks_restore_only_when_the_last_one_resolves() {
        let (app, seen) = app_with_capture();
        let ledger = DotLedger::default();
        let first = on_ask_user_open(app.handle(), &ledger, "s1", live);
        let second = on_ask_user_open(app.handle(), &ledger, "s1", live);
        resolve(first);
        assert_eq!(
            statuses(&seen, "s1"),
            ["needs-permission", "needs-permission"]
        );
        resolve(second);
        assert_eq!(
            statuses(&seen, "s1"),
            ["needs-permission", "needs-permission", "running"]
        );
    }

    #[test]
    fn the_pty_exit_path_forgetting_the_session_blocks_the_restore() {
        let (app, seen) = app_with_capture();
        let ledger = DotLedger::default();
        let ask = on_ask_user_open(app.handle(), &ledger, "s1", live);
        // Exit order: `pty-exit`, forget, then the death sweep resolves the ask.
        ledger.forget("s1");
        resolve(ask);
        assert_eq!(statuses(&seen, "s1"), ["needs-permission"]);
        assert!(!ledger.is_tracking("s1"));
    }

    #[test]
    fn a_retained_exited_session_is_not_a_dot_session() {
        let (app, _) = app_with_capture();
        app.manage(pty_fixture::host());
        let pty = app.state::<Arc<codesense_pty_core::TerminalHost>>();
        pty_fixture::spawn(&pty, "exited", "exit 0");
        pty_fixture::wait_dead(&pty, "exited");
        // Still a member of the PTY map (retained for replay) but not running.
        assert_eq!(pty.has_session(&"exited".into()), Ok(true));
        assert!(!is_dot_session(app.handle(), "exited"));
    }

    #[test]
    fn an_ask_whose_child_exits_before_resolution_does_not_revive_the_dot() {
        let (app, seen) = app_with_capture();
        let ledger = DotLedger::default();
        app.manage(pty_fixture::host());
        let pty = app.state::<Arc<codesense_pty_core::TerminalHost>>();
        pty_fixture::spawn(&pty, "s1", "sleep 60");
        assert!(is_dot_session(app.handle(), "s1"));
        let ask = on_ask_user_open(app.handle(), &ledger, "s1", is_dot_session);
        pty_fixture::kill_child(&pty, "s1");
        assert_eq!(
            pty.has_session(&"s1".into()),
            Ok(true),
            "the exited session is retained"
        );
        resolve(ask);
        assert_eq!(statuses(&seen, "s1"), ["needs-permission"]);
    }

    #[test]
    fn a_non_dot_session_is_never_written() {
        let (app, seen) = app_with_capture();
        let ledger = DotLedger::default();
        on_signal_done(app.handle(), &ledger, "headless", "failed", gone);
        on_signal_progress(
            app.handle(),
            &ledger,
            "headless",
            crate::roles::Phase::Blocked,
            gone,
        );
        assert!(on_ask_user_open(app.handle(), &ledger, "headless", gone).is_none());
        assert!(seen.lock().unwrap().is_empty());
        assert!(!ledger.is_tracking("headless"));
    }

    #[test]
    fn is_dot_session_rejects_empty_warm_pool_and_unknown_ids() {
        let (app, _) = app_with_capture();
        // No terminal host managed: nothing is a live PTY.
        assert!(!is_dot_session(app.handle(), "s1"));
        app.manage(pty_fixture::host());
        assert!(!is_dot_session(app.handle(), ""));
        assert!(!is_dot_session(app.handle(), "s1"));
        let warm = format!("{}1", crate::terminal::warm_pool::WARM_POOL_PREFIX);
        assert!(!is_dot_session(app.handle(), &warm));
    }

    #[test]
    fn an_ask_call_dropped_mid_wait_closes_the_ask_without_restoring() {
        let (app, seen) = app_with_capture();
        let ledger = DotLedger::default();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async {
            let handle = app.handle().clone();
            let ledger = &ledger;
            let call = async move {
                let _ask = on_ask_user_open(&handle, ledger, "s1", live);
                std::future::pending::<()>().await;
            };
            // The client gives up on the call: the future is dropped mid-wait.
            let _ = tokio::time::timeout(std::time::Duration::from_millis(1), call).await;
        });
        assert_eq!(statuses(&seen, "s1"), ["needs-permission"]);
        assert!(!ledger.is_tracking("s1"));
    }

    /// The liveness check may ask a terminal host over its socket, so it
    /// runs OFF the ledger lock — and an exit the ledger forgets while it runs
    /// re-runs it, so a signal racing the child's death never revives the dot.
    #[test]
    fn the_liveness_check_runs_off_the_lock_and_sees_a_racing_exit() {
        use std::cell::Cell;
        let ledger = DotLedger::default();
        let checks = Cell::new(0);
        let wrote = Cell::new(false);
        ledger.agent_write(
            "s1",
            || {
                assert!(ledger.by_session.try_lock().is_ok(), "checked off the lock");
                checks.set(checks.get() + 1);
                if checks.get() == 1 {
                    // The child exits while the first check is in flight.
                    ledger.forget("s1");
                    true
                } else {
                    false
                }
            },
            McpSignal::AskUser,
            |_| wrote.set(true),
        );
        assert_eq!(checks.get(), 2, "the racing exit re-ran the check");
        assert!(!wrote.get());
        assert!(!ledger.is_tracking("s1"));

        // Another session's exit meanwhile never re-runs (or drops) the check.
        let checks = Cell::new(0);
        let wrote = Cell::new(false);
        ledger.agent_write(
            "s2",
            || {
                checks.set(checks.get() + 1);
                ledger.forget("other");
                true
            },
            McpSignal::AskUser,
            |_| wrote.set(true),
        );
        assert_eq!(checks.get(), 1);
        assert!(wrote.get());
    }

    #[test]
    fn closing_without_an_open_ask_restores_nothing() {
        let ledger = DotLedger::default();
        let mut restored = false;
        ledger.close_ask("s1", || true, || restored = true);
        assert!(!restored);
        assert!(!ledger.is_tracking("s1"));
    }
}
