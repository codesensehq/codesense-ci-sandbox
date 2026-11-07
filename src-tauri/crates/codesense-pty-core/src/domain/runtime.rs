// SPDX-License-Identifier: MIT
//! The Tauri-free PTY session runtime. Owns the two byte-driven coordination
//! protocols — the subscribe warm-gate wait and the submit
//! settle/confirm/nudge loop — over a [`TerminalRegistry`]
//! and an injected [`ClockPort`]. State is reached only through the registry
//! trait and time only through the clock; nothing here names `AppHandle`, a
//! Tauri window, or a command DTO.
//!
//! The runtime owns only the *coordination-heavy* ops (subscribe, submit_line);
//! the trivial state ops (resize/write/is_idle/kill) are called on the
//! `TerminalRegistry` directly — keeping the keystroke `write` path free of a
//! clock construction it does not use. The [`TerminalDomain`](super::TerminalDomain)
//! contract is implemented by the host, over this runtime.
//!
//! Error text is `String` and preserved verbatim from the pre-extraction
//! commands (the FE distinguishes, e.g., a not-found `write` from a lock error).

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};

use super::registry::TerminalRegistry;
use super::types::ClockPort;
use crate::emulator::TerminalSnapshot;
use crate::term_state::{SharedTermState, StreamId};

/// The byte that submits a line — a carriage return, which works in both a
/// shell (the line discipline maps CR→NL) and a raw-mode agent TUI.
const PTY_SUBMIT_KEY: &str = "\r";
/// Poll cadence for the submit settle/confirm loops.
const SUBMIT_POLL_MS: u64 = 5;
/// The least time between an agent's text and its Enter. Claude Code and Codex
/// fold an Enter that arrives with (or right after) a burst of text into that
/// text as a newline, so the Enter must come later, as its own keystroke.
const AGENT_ENTER_GAP_MIN_MS: u64 = 200;
/// The most time spent waiting for the text's echo to go quiet before Enter;
/// a TUI that keeps redrawing (a busy spinner) still gets its Enter.
const AGENT_ENTER_GAP_MAX_MS: u64 = 1000;
/// Bracketed-paste delimiters (xterm mode 2004).
const PASTE_START: &str = "\x1b[200~";
const PASTE_END: &str = "\x1b[201~";
/// Poll cadence for the subscribe warm-gate wait.
const SUBSCRIBE_POLL_MS: u64 = 2;
/// Bounded budget for the warm-handoff gate wait.
const SUBSCRIBE_GATE_WAIT_MS: u64 = 1500;

/// A monotonic millisecond clock backed by [`Instant`] — the production
/// [`ClockPort`]. Tauri-free (std only); tests inject an advancing fake.
///
/// Intended to be **short-lived**: one is constructed per command invocation, so
/// `base.elapsed()` stays tiny and the `u128 → u64` millisecond narrowing below
/// can never truncate (it would only matter after ~584M years of a single
/// clock's life). The saturating cast makes that explicit rather than relying on
/// the caller keeping it short-lived.
pub struct MonotonicClock {
    base: Instant,
}

impl MonotonicClock {
    pub fn new() -> Self {
        Self {
            base: Instant::now(),
        }
    }
}

impl Default for MonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

impl ClockPort for MonotonicClock {
    fn now_millis(&self) -> u64 {
        self.base.elapsed().as_millis().min(u64::MAX as u128) as u64
    }
}

/// A session's current output watermark; `u64::MAX` on a poisoned lock. The
/// sentinel stops [`wait_for_growth`](PtySessionRuntime::wait_for_growth) from
/// mis-reading a poisoned lock as "output resumed" (`MAX > MAX` is false); the
/// settle wait reads the constant `MAX` as quiet and proceeds (harmless — the
/// ensuing write then fails fast).
fn seq_of(shared: &Arc<Mutex<SharedTermState>>) -> u64 {
    shared.lock().map(|g| g.seq).unwrap_or(u64::MAX)
}

/// What a viewport subscribe hands back — the emulator's terminal a display is
/// built from (the stream carries output with `seq_end > snapshot.seq` and
/// resizes with `resize_gen > snapshot.resize_gen`) plus the identity/metadata
/// the command layer echoes into `SubscribeResponse`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SubscribeResult {
    pub snapshot: TerminalSnapshot,
    pub stream_id: StreamId,
    pub story_id: Option<String>,
    pub workspace_id: Option<String>,
    pub epic_id: Option<String>,
}

/// The session runtime: coordination logic over a live registry + a clock.
pub struct PtySessionRuntime<'a> {
    registry: &'a dyn TerminalRegistry,
    clock: &'a dyn ClockPort,
}

impl<'a> PtySessionRuntime<'a> {
    pub fn new(registry: &'a dyn TerminalRegistry, clock: &'a dyn ClockPort) -> Self {
        Self { registry, clock }
    }

    /// Attach a viewport: wait (bounded) for the warm-handoff gate to open, then
    /// snapshot + register atomically against the batcher. A display listens
    /// before it subscribes, so output after the snapshot is never missed. Preserves the exact first-paint discipline
    /// so a live terminal's identity/scrollback/stream position survive with no
    /// respawn or resize-nudge.
    pub fn subscribe(&self, session: &str) -> Result<SubscribeResult, String> {
        let view = self.registry.session_view(session)?;

        // Warm-handoff gate: wait (bounded) for the post-cd prompt-start so first
        // paint comes from the snapshot, not a live event that races the FE's
        // listen() registration. No lock held while waiting.
        if view.runtime.suppress_until_prompt.load(Ordering::Relaxed) {
            let deadline = self
                .clock
                .now_millis()
                .saturating_add(SUBSCRIBE_GATE_WAIT_MS);
            while view.runtime.suppress_until_prompt.load(Ordering::Relaxed)
                && self.clock.now_millis() < deadline
            {
                sleep(Duration::from_millis(SUBSCRIBE_POLL_MS));
            }
        }

        let attach = self.registry.attach_viewport(session, &view.shared)?;
        Ok(SubscribeResult {
            snapshot: attach.snapshot,
            stream_id: attach.stream_id,
            story_id: view.story_id,
            workspace_id: view.workspace_id,
            epic_id: view.epic_id,
        })
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn submit_line(
        &self,
        session: &str,
        line: &str,
        quiet_ms: u64,
        confirm_ms: u64,
        timeout_ms: u64,
        max_nudges: u32,
    ) -> Result<bool, String> {
        self.submit_line_as(
            session, line, quiet_ms, confirm_ms, timeout_ms, max_nudges, false,
        )
    }

    /// Submit a single line, running the byte-driven settle → write →
    /// confirm/nudge protocol. `Ok(true)` when the submit was confirmed from
    /// output resumption; `Ok(false)` when the line stayed held through every
    /// nudge / the overall timeout (surfaced as `send-not-submitted`). With
    /// `bracket` the text goes as one bracketed paste even on a single line,
    /// for a TUI that has turned bracketed paste on. Its end is then marked,
    /// so the Enter after it is never read as part of the paste however the
    /// two arrive.
    #[allow(clippy::too_many_arguments)]
    pub fn submit_line_as(
        &self,
        session: &str,
        line: &str,
        quiet_ms: u64,
        confirm_ms: u64,
        timeout_ms: u64,
        max_nudges: u32,
        bracket: bool,
    ) -> Result<bool, String> {
        // Clone the shared handle so output can be observed without holding the
        // `sessions` lock across the settle/confirm sleeps. A missing session is
        // a hard error (parity with `write`).
        let shared = self.registry.shared_handle(session)?;

        // saturating: `timeout_ms` is unvalidated FE input; a pathological value
        // must clamp to "effectively infinite", never wrap to a tiny deadline.
        let overall_deadline = self.clock.now_millis().saturating_add(timeout_ms);
        self.run_submit_protocol_as(
            &shared,
            line,
            bracket,
            quiet_ms,
            confirm_ms,
            overall_deadline,
            max_nudges,
            |data| self.registry.write(session, data.as_bytes()),
        )
    }

    /// Submit a single line into a BARE SHELL tab (the `run_in_terminal_tab`
    /// path), with an ACCURATE `submitted` and cold-tab readiness.
    ///
    /// The agent-TUI [`submit_line`](Self::submit_line) protocol batches its write
    /// as a possible paste and confirms the submit by watching for output to
    /// *resume* — a heuristic that is right for a raw-mode TUI but wrong for a
    /// plain shell: a shell command that emits little/no extra output (`cd`,
    /// `export`, a no-op) never trips the growth probe, so the old path returned
    /// `submitted:false` **even though the line + CR was already written** — a false
    /// negative that invites a retry that double-runs the command (each retry also
    /// sends a stray Enter).
    ///
    /// A shell instead submits deterministically on the carriage return (line
    /// discipline maps CR→NL), so the accurate contract is simply: **once the
    /// prompt has settled, write `line` + CR exactly once ⇒ `submitted:true`**; no
    /// output-growth confirm, no nudges (a stray nudge CR at a settled shell is
    /// exactly the phantom Enter this contract rules out).
    ///
    /// Fresh/cold-tab readiness: a freshly opened tab cold-spawns its PTY on render,
    /// so its prompt may not exist yet. Wait (bounded by `timeout_ms`) for output to
    /// APPEAR and go quiet (the prompt rendered). If it never settles in the bound,
    /// return `Ok(false)` having written **NOTHING** — so a retry is safe, not
    /// additive. `Ok(true)` therefore means the line was actually written + entered;
    /// `Ok(false)` means nothing was sent.
    ///
    /// A missing session is a hard `Err` (parity with `write`).
    pub fn submit_shell_line(
        &self,
        session: &str,
        line: &str,
        quiet_ms: u64,
        timeout_ms: u64,
    ) -> Result<bool, String> {
        let shared = self.registry.shared_handle(session)?;
        let overall_deadline = self.clock.now_millis().saturating_add(timeout_ms);
        self.run_shell_submit_protocol(&shared, line, quiet_ms, overall_deadline, |data| {
            self.registry.write(session, data.as_bytes())
        })
    }

    /// The shell settle → write protocol. Waits for the prompt to render
    /// and go quiet (`require_start` — a cold shell begins at seq 0); on a settle it
    /// writes `line` + CR ONCE and reports `true`; on a never-settling prompt it
    /// writes nothing and reports `false`. Split out (like [`run_submit_protocol`])
    /// so it is unit-testable over an observed watermark + an injected `write`.
    fn run_shell_submit_protocol(
        &self,
        shared: &Arc<Mutex<SharedTermState>>,
        line: &str,
        quiet_ms: u64,
        overall_deadline: u64,
        mut write: impl FnMut(&str) -> Result<(), String>,
    ) -> Result<bool, String> {
        // Settle: the shell prompt must have RENDERED (output appeared) and gone
        // quiet before we write. `require_start=true` — a freshly cold-spawned shell
        // starts at seq 0, and writing before its prompt exists would report a
        // cold tab's write as not submitted.
        let settled =
            self.wait_until_stable(shared, quiet_ms, SUBMIT_POLL_MS, overall_deadline, true);
        if !settled {
            // The prompt never settled in the bound — write NOTHING so a caller retry
            // is not additive (no stray Enter, no half-typed line).
            return Ok(false);
        }
        // The line + its submit key, in ONE write — a settled shell submits on the CR.
        write(&format!("{line}{PTY_SUBMIT_KEY}"))?;
        Ok(true)
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    fn run_submit_protocol(
        &self,
        shared: &Arc<Mutex<SharedTermState>>,
        line: &str,
        quiet_ms: u64,
        confirm_ms: u64,
        overall_deadline: u64,
        max_nudges: u32,
        write: impl FnMut(&str) -> Result<(), String>,
    ) -> Result<bool, String> {
        self.run_submit_protocol_as(
            shared,
            line,
            false,
            quiet_ms,
            confirm_ms,
            overall_deadline,
            max_nudges,
            write,
        )
    }

    /// The agent-agnostic settle → write → confirm/nudge protocol over a session's
    /// output counter, `bracket` as in [`agent_input_text_as`]. Observes `shared`'s
    /// watermark and writes through `write`, so it is unit-testable without a real
    /// terminal.
    #[allow(clippy::too_many_arguments)]
    fn run_submit_protocol_as(
        &self,
        shared: &Arc<Mutex<SharedTermState>>,
        line: &str,
        bracket: bool,
        quiet_ms: u64,
        confirm_ms: u64,
        overall_deadline: u64,
        max_nudges: u32,
        mut write: impl FnMut(&str) -> Result<(), String>,
    ) -> Result<bool, String> {
        // 1. Settle: don't write until the agent has stopped rendering.
        let _ = self.wait_until_stable(shared, quiet_ms, SUBMIT_POLL_MS, overall_deadline, false);

        // 2. Write the text, then — once the TUI has taken it in — Enter as its
        //    own keystroke. Text and Enter in one write read as a paste, whose
        //    Enter becomes a newline instead of a submit.
        let text = agent_input_text_as(line, bracket);
        if text.is_empty() {
            // Nothing to type: a lone Enter would submit whatever draft is there.
            return Ok(false);
        }
        write(&text)?;
        let written_at = self.clock.now_millis();
        let gap_deadline = written_at
            .saturating_add(AGENT_ENTER_GAP_MAX_MS)
            .min(overall_deadline);
        let _ = self.wait_until_stable(
            shared,
            AGENT_ENTER_GAP_MIN_MS,
            SUBMIT_POLL_MS,
            gap_deadline,
            false,
        );
        // The floor is a real pause even when the budget is spent (a busy agent
        // that never went quiet): the Enter must not land in the text's burst.
        let waited = self.clock.now_millis().saturating_sub(written_at);
        if waited < AGENT_ENTER_GAP_MIN_MS {
            sleep(Duration::from_millis(AGENT_ENTER_GAP_MIN_MS - waited));
        }
        write(PTY_SUBMIT_KEY)?;

        // 3. Confirm the submit from output resumption; nudge a discrete CR if held.
        let mut nudges = 0u32;
        loop {
            // Let our write's input-box redraw (echo) render, so the growth probe
            // below measures a response rather than the echo of what we just typed.
            let confirm_deadline = self
                .clock
                .now_millis()
                .saturating_add(confirm_ms)
                .min(overall_deadline);
            self.sleep_until(confirm_deadline);

            let baseline = seq_of(shared);
            let grow_deadline = self
                .clock
                .now_millis()
                .saturating_add(confirm_ms)
                .min(overall_deadline);
            if self.wait_for_growth(shared, baseline, grow_deadline) {
                return Ok(true); // output resumed past the echo ⇒ submitted
            }
            if nudges >= max_nudges || self.clock.now_millis() >= overall_deadline {
                break; // nudge budget or overall time spent; never block further
            }
            // Still quiet ⇒ the line is sitting in the input box ⇒ discrete submit key.
            write(PTY_SUBMIT_KEY)?;
            nudges += 1;
        }
        Ok(false) // never confirmed a resume ⇒ line may still be held
    }

    /// Block until the session's output has been unchanged for `window_ms`, or
    /// `deadline`. The single "stable for a window" poll shared by the submit
    /// quiet-waits.
    ///
    /// `require_start` is the one variation point: the shell-run submit must
    /// first see output APPEAR (`seq > 0`) before it counts quiet — a freshly
    /// cold-spawned shell starts at seq 0, and writing before its prompt exists
    /// is lost. The agent submit instead starts already-quiet (the session has
    /// been running), so it counts from entry.
    ///
    /// Returns `true` when the quiet window was actually observed within
    /// `deadline`, `false` when it returned because time ran out first. The
    /// agent submit-settle caller ignores the bool (they
    /// proceed either way); the shell-run submit uses it to distinguish a
    /// prompt that settled (safe to write) from a cold tab that never rendered its
    /// prompt in time (write NOTHING, so a retry is not additive).
    fn wait_until_stable(
        &self,
        shared: &Arc<Mutex<SharedTermState>>,
        window_ms: u64,
        poll_ms: u64,
        deadline: u64,
        require_start: bool,
    ) -> bool {
        let mut last_seq = if require_start { 0 } else { seq_of(shared) };
        let mut quiet_since: Option<u64> = if require_start {
            None
        } else {
            Some(self.clock.now_millis())
        };
        loop {
            let now = self.clock.now_millis();
            if now >= deadline {
                return false;
            }
            let seq = seq_of(shared);
            let stable = if require_start {
                seq > 0 && seq == last_seq
            } else {
                seq == last_seq
            };
            if stable {
                match quiet_since {
                    Some(since) if now.saturating_sub(since) >= window_ms => return true,
                    Some(_) => {}
                    None => quiet_since = Some(now),
                }
            } else {
                last_seq = seq;
                quiet_since = None;
            }
            sleep(Duration::from_millis(poll_ms));
        }
    }

    /// Return `true` as soon as output grows beyond `baseline`, else `false` at
    /// `deadline`. Distinct knowledge from [`wait_until_stable`] — it waits for
    /// output to APPEAR, not to STOP.
    fn wait_for_growth(
        &self,
        shared: &Arc<Mutex<SharedTermState>>,
        baseline: u64,
        deadline: u64,
    ) -> bool {
        loop {
            if seq_of(shared) > baseline {
                return true;
            }
            if self.clock.now_millis() >= deadline {
                return false;
            }
            sleep(Duration::from_millis(SUBMIT_POLL_MS));
        }
    }

    /// Sleep until `deadline` (no-op if already past). Bounded by the caller.
    fn sleep_until(&self, deadline: u64) {
        let now = self.clock.now_millis();
        if deadline > now {
            sleep(Duration::from_millis(deadline - now));
        }
    }
}

/// True when `line` types nothing into an agent: only whitespace and paste
/// delimiters. Callers refuse such input up front, since the agent protocol
/// would write nothing and could not tell that apart from an unconfirmed submit.
pub fn is_blank_agent_input(line: &str) -> bool {
    strip_paste_markers(line).trim().is_empty()
}

/// `line` with every paste delimiter removed, repeated until nothing changes:
/// removing one marker can join the pieces of another.
fn strip_paste_markers(line: &str) -> String {
    let mut text = line.to_string();
    loop {
        let stripped = text.replace(PASTE_START, "").replace(PASTE_END, "");
        if stripped == text {
            return text;
        }
        text = stripped;
    }
}

#[cfg(test)]
pub(crate) fn agent_input_text(line: &str) -> String {
    agent_input_text_as(line, false)
}

/// An agent prompt as the bytes to type: trailing line breaks dropped (Enter is
/// sent on its own), paste delimiters inside it removed so it cannot end a
/// paste early, and a multi-line prompt sent as one bracketed paste so its line
/// breaks stay line breaks instead of submitting it piecemeal.
/// With `bracket` (a TUI that has turned bracketed paste on), a single line is
/// bracketed too, so the Enter after it is never read as part of the paste.
pub(crate) fn agent_input_text_as(line: &str, bracket: bool) -> String {
    let text = strip_paste_markers(line);
    let text = text.trim_end_matches(['\r', '\n']).to_string();
    if !text.is_empty() && (bracket || text.contains(['\r', '\n'])) {
        format!("{PASTE_START}{text}{PASTE_END}")
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::super::registry::{AttachOutcome, SessionView};
    use super::*;
    use crate::term_state::{SessionRuntime, SubscriberHandle};
    use std::collections::HashMap;

    // ---- A Tauri-free registry the deterministic suite drives the REAL runtime
    // over (not `FakeDomain`), so a regression in the moved coordination bodies is
    // caught here rather than passing vacuously. --------------------------------

    struct FakeSession {
        shared: Arc<Mutex<SharedTermState>>,
        runtime: Arc<SessionRuntime>,
        idle: bool,
        writes: Arc<Mutex<Vec<Vec<u8>>>>,
        write_fails: bool,
    }

    struct FakeRegistry {
        sessions: Mutex<HashMap<String, FakeSession>>,
        killed: Mutex<Vec<String>>,
        /// When set, `attach_viewport` returns the "no longer live" error the real
        /// `PtyState` raises when a same-id respawn displaces the session between
        /// snapshot and index-insert — so the runtime's propagation of that failure
        /// (rather than swallowing it) is covered.
        attach_displaced: std::sync::atomic::AtomicBool,
    }

    impl FakeRegistry {
        fn new() -> Self {
            Self {
                sessions: Mutex::new(HashMap::new()),
                killed: Mutex::new(Vec::new()),
                attach_displaced: std::sync::atomic::AtomicBool::new(false),
            }
        }

        fn insert(&self, id: &str) -> Arc<Mutex<SharedTermState>> {
            let shared = Arc::new(Mutex::new(SharedTermState::default()));
            self.sessions.lock().unwrap().insert(
                id.to_string(),
                FakeSession {
                    shared: shared.clone(),
                    runtime: Arc::new(SessionRuntime::new(id.to_string(), false)),
                    idle: true,
                    writes: Arc::new(Mutex::new(Vec::new())),
                    write_fails: false,
                },
            );
            shared
        }

        /// The live control handle for a session, so a test can arm/clear the
        /// warm-handoff `suppress_until_prompt` gate from another thread.
        fn runtime_of(&self, id: &str) -> Arc<SessionRuntime> {
            self.sessions
                .lock()
                .unwrap()
                .get(id)
                .unwrap()
                .runtime
                .clone()
        }

        fn set_write_fails(&self, id: &str) {
            self.sessions
                .lock()
                .unwrap()
                .get_mut(id)
                .unwrap()
                .write_fails = true;
        }

        fn displace_on_attach(&self) {
            self.attach_displaced
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }

        fn writes_of(&self, id: &str) -> Arc<Mutex<Vec<Vec<u8>>>> {
            self.sessions
                .lock()
                .unwrap()
                .get(id)
                .unwrap()
                .writes
                .clone()
        }
    }

    impl TerminalRegistry for FakeRegistry {
        fn session_view(&self, session: &str) -> Result<SessionView, String> {
            let sessions = self.sessions.lock().unwrap();
            let s = sessions
                .get(session)
                .ok_or_else(|| format!("subscribe_terminal: session not found: {}", session))?;
            Ok(SessionView {
                shared: s.shared.clone(),
                runtime: s.runtime.clone(),
                story_id: None,
                workspace_id: None,
                epic_id: None,
            })
        }

        fn shared_handle(&self, session: &str) -> Result<Arc<Mutex<SharedTermState>>, String> {
            self.sessions
                .lock()
                .unwrap()
                .get(session)
                .map(|s| s.shared.clone())
                .ok_or_else(|| format!("submit_pty_line: session not found: {}", session))
        }

        fn attach_viewport(
            &self,
            session: &str,
            shared: &Arc<Mutex<SharedTermState>>,
        ) -> Result<AttachOutcome, String> {
            if self
                .attach_displaced
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                return Err(format!(
                    "subscribe_terminal: session {} is no longer live",
                    session
                ));
            }
            let stream_id = StreamId::next();
            let mut g = shared.lock().map_err(|e| format!("Lock error: {}", e))?;
            let snapshot = TerminalSnapshot {
                cols: 80,
                rows: 24,
                seq: g.seq,
                resize_gen: 0,
                scrollback: crate::emulator::DEFAULT_SCROLLBACK,
                vt: String::new(),
            };
            g.subscribers.insert(stream_id, SubscriberHandle::default());
            Ok(AttachOutcome {
                snapshot,
                stream_id,
            })
        }

        fn resize(&self, _session: &str, _cols: u16, _rows: u16) -> Result<(), String> {
            Ok(())
        }

        fn write(&self, session: &str, bytes: &[u8]) -> Result<(), String> {
            let sessions = self.sessions.lock().unwrap();
            let s = sessions
                .get(session)
                .ok_or_else(|| format!("write_pty: session not found: {}", session))?;
            if s.write_fails {
                return Err("Write error: fake".to_string());
            }
            s.writes.lock().unwrap().push(bytes.to_vec());
            Ok(())
        }

        fn is_idle(&self, session: &str) -> Result<bool, String> {
            Ok(self
                .sessions
                .lock()
                .unwrap()
                .get(session)
                .map(|s| s.idle)
                .unwrap_or(true))
        }

        fn kill(&self, session: &str) {
            self.sessions.lock().unwrap().remove(session);
            self.killed.lock().unwrap().push(session.to_string());
        }
    }

    fn submit_test_shared() -> Arc<Mutex<SharedTermState>> {
        Arc::new(Mutex::new(SharedTermState::default()))
    }

    /// Simulate the agent emitting `n` bytes of output (bumps `seq`).
    fn bump_output(shared: &Arc<Mutex<SharedTermState>>, n: usize) {
        shared.lock().unwrap().seq += n as u64;
    }

    // ---- The submit settle → write → confirm → nudge protocol -------

    /// A held line (no output resumes after the write) is nudged with a discrete
    /// CR up to `max_nudges` times, then the protocol returns.
    #[test]
    fn submit_protocol_nudges_a_held_line() {
        let reg = FakeRegistry::new();
        let clock = MonotonicClock::new();
        let rt = PtySessionRuntime::new(&reg, &clock);
        let shared = submit_test_shared();
        let writes = std::cell::RefCell::new(Vec::<String>::new());
        let res =
            rt.run_submit_protocol(&shared, "hello", 5, 10, clock.now_millis() + 600, 2, |d| {
                writes.borrow_mut().push(d.to_string());
                Ok(())
            });
        assert_eq!(res, Ok(false), "a line that never resumes is NOT confirmed");
        let w = writes.borrow();
        assert_eq!(w[0], "hello", "the text is written without its Enter");
        assert_eq!(w[1], "\r", "Enter follows as its own keystroke");
        assert_eq!(
            w.len(),
            4,
            "text + Enter + exactly max_nudges=2 nudges: {:?}",
            *w
        );
        assert!(
            w[1..].iter().all(|s| s == "\r"),
            "Enter and nudges are bare CRs: {:?}",
            *w
        );
    }

    /// A held line that resumes output AFTER the first discrete nudge is confirmed
    /// submitted (`true`) and nudged exactly once — the paste-hold rescue path.
    #[test]
    fn submit_protocol_confirms_after_a_nudge() {
        let reg = FakeRegistry::new();
        let clock = MonotonicClock::new();
        let rt = PtySessionRuntime::new(&reg, &clock);
        let shared = submit_test_shared();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let nudged = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let bg_shared = shared.clone();
        let bg_stop = stop.clone();
        let bg_nudged = nudged.clone();
        let bg = std::thread::spawn(move || {
            while !bg_nudged.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(1));
            }
            while !bg_stop.load(Ordering::Relaxed) {
                bump_output(&bg_shared, 4);
                std::thread::sleep(Duration::from_millis(2));
            }
        });

        let writes = std::cell::RefCell::new(Vec::<String>::new());
        let enters = std::cell::Cell::new(0u32);
        // A wide confirm window (vs the bg's 2ms cadence) so the post-nudge growth
        // is seen within one confirm pass — the assertion is "exactly ONE nudge".
        let res = rt.run_submit_protocol(
            &shared,
            "hello",
            5,
            200,
            clock.now_millis() + 4000,
            3,
            |d| {
                writes.borrow_mut().push(d.to_string());
                if d == "\r" {
                    // The first CR is the Enter; the second is the nudge.
                    enters.set(enters.get() + 1);
                    if enters.get() == 2 {
                        nudged.store(true, Ordering::Relaxed);
                    }
                }
                Ok(())
            },
        );
        stop.store(true, Ordering::Relaxed);
        bg.join().unwrap();

        assert_eq!(
            res,
            Ok(true),
            "the nudge submitted the held line ⇒ confirmed"
        );
        let w = writes.borrow();
        assert_eq!(
            *w,
            vec!["hello".to_string(), "\r".into(), "\r".into()],
            "text + Enter + exactly ONE nudge before confirm",
        );
    }

    /// When output resumes after the write (a real submit), the protocol confirms
    /// and sends NO nudge.
    #[test]
    fn submit_protocol_confirms_without_nudge_when_output_resumes() {
        let reg = FakeRegistry::new();
        let clock = MonotonicClock::new();
        let rt = PtySessionRuntime::new(&reg, &clock);
        let shared = submit_test_shared();
        let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let bg_shared = shared.clone();
        let bg_started = started.clone();
        let bg_stop = stop.clone();
        let bg = std::thread::spawn(move || {
            while !bg_started.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(1));
            }
            while !bg_stop.load(Ordering::Relaxed) {
                bump_output(&bg_shared, 4);
                std::thread::sleep(Duration::from_millis(2));
            }
        });

        let writes = std::cell::RefCell::new(Vec::<String>::new());
        let res =
            rt.run_submit_protocol(&shared, "hello", 5, 20, clock.now_millis() + 1000, 3, |d| {
                writes.borrow_mut().push(d.to_string());
                if d == "\r" {
                    started.store(true, Ordering::Relaxed);
                }
                Ok(())
            });
        stop.store(true, Ordering::Relaxed);
        bg.join().unwrap();

        assert_eq!(res, Ok(true), "output resumed ⇒ confirmed submitted");
        assert_eq!(
            *writes.borrow(),
            vec!["hello".to_string(), "\r".into()],
            "text then Enter, and no nudge when output resumes",
        );
    }

    /// The line is not written until output has settled (agent idle at prompt):
    /// the seq observed at write time is the final total, not a mid-burst value.
    #[test]
    fn submit_protocol_waits_for_output_to_settle_before_writing() {
        let reg = FakeRegistry::new();
        let clock = MonotonicClock::new();
        let rt = PtySessionRuntime::new(&reg, &clock);
        let shared = submit_test_shared();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let bg_shared = shared.clone();
        let bg_stop = stop.clone();
        let bg = std::thread::spawn(move || {
            for _ in 0..15 {
                if bg_stop.load(Ordering::Relaxed) {
                    return;
                }
                bump_output(&bg_shared, 4);
                std::thread::sleep(Duration::from_millis(3));
            }
        });

        let seq_at_write = std::cell::Cell::new(0u64);
        let wrote = std::cell::Cell::new(false);
        let res = rt.run_submit_protocol(&shared, "hi", 15, 5, clock.now_millis() + 2000, 0, |d| {
            if !wrote.get() && d.contains("hi") {
                wrote.set(true);
                seq_at_write.set(shared.lock().unwrap().seq);
            }
            Ok(())
        });
        stop.store(true, Ordering::Relaxed);
        bg.join().unwrap();

        assert_eq!(
            res,
            Ok(false),
            "no output after settle (max_nudges=0) ⇒ not confirmed"
        );
        assert_eq!(
            seq_at_write.get(),
            60,
            "settle waited until output stopped (15 * 4 bytes emitted first)",
        );
    }

    /// A write failure short-circuits the protocol with the error.
    #[test]
    fn submit_protocol_propagates_write_error() {
        let reg = FakeRegistry::new();
        let clock = MonotonicClock::new();
        let rt = PtySessionRuntime::new(&reg, &clock);
        let shared = submit_test_shared();
        let res = rt.run_submit_protocol(&shared, "hi", 2, 2, clock.now_millis() + 100, 2, |_d| {
            Err("boom".to_string())
        });
        assert_eq!(res, Err("boom".to_string()));
    }

    /// An agent's Enter is its own write, sent no sooner than the gap after the
    /// text: Claude Code and Codex read an Enter inside a text burst as a newline.
    #[test]
    fn agent_submit_sends_enter_separately_after_the_gap() {
        let reg = FakeRegistry::new();
        let clock = MonotonicClock::new();
        let rt = PtySessionRuntime::new(&reg, &clock);
        let shared = submit_test_shared();
        let writes = std::cell::RefCell::new(Vec::<(String, u64)>::new());
        let res = rt.run_submit_protocol(
            &shared,
            "fix it\n",
            5,
            5,
            clock.now_millis() + 2000,
            0,
            |d| {
                writes
                    .borrow_mut()
                    .push((d.to_string(), clock.now_millis()));
                // The TUI redraws its input box on the text: the echo.
                bump_output(&shared, 3);
                Ok(())
            },
        );
        assert_eq!(res, Ok(false));
        let w = writes.borrow();
        assert_eq!(w.len(), 2, "{:?}", *w);
        assert_eq!(w[0].0, "fix it", "trailing line breaks dropped");
        assert_eq!(w[1].0, "\r");
        assert!(
            w[1].1 - w[0].1 >= AGENT_ENTER_GAP_MIN_MS,
            "Enter waited the gap: {:?}",
            *w
        );
    }

    /// With `bracket`, a one-line prompt is typed as a closed paste and its Enter
    /// still goes on its own after it.
    #[test]
    fn a_bracketed_line_is_a_closed_paste_then_its_own_enter() {
        let reg = FakeRegistry::new();
        let clock = MonotonicClock::new();
        let rt = PtySessionRuntime::new(&reg, &clock);
        let shared = submit_test_shared();
        let writes = std::cell::RefCell::new(Vec::<String>::new());
        let _ = rt.run_submit_protocol_as(
            &shared,
            "fix it\n",
            true,
            5,
            5,
            clock.now_millis() + 2000,
            0,
            |d| {
                writes.borrow_mut().push(d.to_string());
                bump_output(&shared, 3);
                Ok(())
            },
        );
        let w = writes.borrow();
        assert_eq!(
            w.first().map(String::as_str),
            Some("\x1b[200~fix it\x1b[201~")
        );
        assert_eq!(w.get(1).map(String::as_str), Some("\r"), "{:?}", *w);
    }

    #[test]
    fn agent_input_text_brackets_only_multi_line_and_strips_paste_markers() {
        assert_eq!(agent_input_text("/clear"), "/clear");
        assert_eq!(agent_input_text("hello\r\n"), "hello");
        assert_eq!(agent_input_text("one\ntwo\n"), "\x1b[200~one\ntwo\x1b[201~");
        assert_eq!(
            agent_input_text("a\x1b[201~b\x1b[200~c"),
            "abc",
            "a payload can never end the paste early"
        );
        assert_eq!(
            agent_input_text("x\x1b[20\x1b[201~0~y"),
            "xy",
            "a marker split around another is stripped too"
        );
        assert_eq!(agent_input_text("\n\r\n"), "");
        assert_eq!(
            agent_input_text_as("hi there", true),
            "\x1b[200~hi there\x1b[201~",
            "a TUI with bracketed paste on is told where a one-line paste ends"
        );
        assert_eq!(
            agent_input_text_as("\n", true),
            "",
            "nothing to type stays nothing"
        );
    }

    #[test]
    fn blank_agent_input_is_whitespace_or_paste_markers_only() {
        assert!(is_blank_agent_input("\x1b[200~\x1b[201~"));
        assert!(is_blank_agent_input("\x1b[200~\n"));
        assert!(is_blank_agent_input(" \x1b[20\x1b[201~0~ \r\n"));
        assert!(is_blank_agent_input(" \n \x1b[201~ "));
        assert!(!is_blank_agent_input("/clear"));
        assert!(!is_blank_agent_input("\x1b[200~hi\x1b[201~"));
    }

    #[test]
    fn agent_submit_with_nothing_to_type_sends_nothing() {
        let reg = FakeRegistry::new();
        let clock = MonotonicClock::new();
        let rt = PtySessionRuntime::new(&reg, &clock);
        let shared = submit_test_shared();
        let writes = std::cell::RefCell::new(Vec::<String>::new());
        let res = rt.run_submit_protocol(&shared, "\n", 5, 5, clock.now_millis() + 200, 3, |d| {
            writes.borrow_mut().push(d.to_string());
            Ok(())
        });
        assert_eq!(res, Ok(false));
        assert!(
            writes.borrow().is_empty(),
            "no lone Enter: {:?}",
            writes.borrow()
        );
    }

    /// If output never quiets, the protocol still writes the line once (at the
    /// settle timeout, then its Enter after the fixed keystroke gap) and returns —
    /// it never hangs past the overall deadline by more than that gap.
    #[test]
    fn submit_protocol_writes_at_timeout_when_output_never_quiets() {
        let reg = FakeRegistry::new();
        let clock = MonotonicClock::new();
        let rt = PtySessionRuntime::new(&reg, &clock);
        let shared = submit_test_shared();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let bg_shared = shared.clone();
        let bg_stop = stop.clone();
        let bg = std::thread::spawn(move || {
            while !bg_stop.load(Ordering::Relaxed) {
                bump_output(&bg_shared, 4);
                std::thread::sleep(Duration::from_millis(1));
            }
        });

        let writes = std::cell::RefCell::new(Vec::<String>::new());
        let start = Instant::now();
        let res = rt.run_submit_protocol(&shared, "hi", 50, 10, clock.now_millis() + 120, 3, |d| {
            writes.borrow_mut().push(d.to_string());
            Ok(())
        });
        stop.store(true, Ordering::Relaxed);
        bg.join().unwrap();

        assert!(res.is_ok());
        assert!(
            start.elapsed() < Duration::from_millis(2000),
            "must return by ~the timeout, not hang",
        );
        assert_eq!(
            writes.borrow()[..2],
            ["hi".to_string(), "\r".into()],
            "the text and its Enter are still written once"
        );
    }

    /// A submit into a missing session is a hard `Err` (parity with `write`).
    #[test]
    fn submit_line_missing_session_is_err() {
        let reg = FakeRegistry::new();
        let clock = MonotonicClock::new();
        let rt = PtySessionRuntime::new(&reg, &clock);
        let Err(e) = rt.submit_line("nope", "hi", 1, 1, 10, 0) else {
            panic!("expected an Err for a missing session");
        };
        assert!(e.contains("session not found"), "error was: {e}");
    }

    // ---- The shell-run submit (accurate `submitted` + cold readiness) --

    /// A shell command that produces NO further output after the write (`cd`,
    /// `export`, a no-op) is still `submitted:true` — the settled shell submits on
    /// the CR, so `true` means written+entered. The old agent-injection protocol
    /// returned `false` here (its output-growth confirm never trips): a false
    /// negative.
    #[test]
    fn shell_submit_is_true_for_a_settled_prompt_even_with_no_further_output() {
        let reg = FakeRegistry::new();
        let clock = MonotonicClock::new();
        let rt = PtySessionRuntime::new(&reg, &clock);
        let shared = submit_test_shared();
        // A warm shell: its prompt has already rendered (seq > 0) and is quiet.
        bump_output(&shared, 8);
        let writes = std::cell::RefCell::new(Vec::<String>::new());
        let res =
            rt.run_shell_submit_protocol(&shared, "cd /x", 5, clock.now_millis() + 600, |d| {
                writes.borrow_mut().push(d.to_string());
                Ok(())
            });
        assert_eq!(res, Ok(true), "a settled shell submits on the CR ⇒ true");
        assert_eq!(
            *writes.borrow(),
            vec!["cd /x\r".to_string()],
            "the line + CR is written EXACTLY once — no nudge, no phantom Enter",
        );
    }

    #[test]
    fn fast_quiet_agent_start_is_submitted_once_without_growth_confirmation() {
        let reg = FakeRegistry::new();
        let clock = MonotonicClock::new();
        let rt = PtySessionRuntime::new(&reg, &clock);
        let shared = submit_test_shared();
        bump_output(&shared, 8); // The fresh shell prompt is ready.
        let writes = std::cell::RefCell::new(Vec::<String>::new());
        let result = rt.run_shell_submit_protocol(
            &shared,
            " agent --profile worker",
            5,
            clock.now_millis() + 600,
            |data| {
                writes.borrow_mut().push(data.to_string());
                // Startup completes before any confirmation baseline; the agent now
                // waits quietly for its initial prompt, just like the failed live run.
                bump_output(&shared, 24);
                Ok(())
            },
        );
        assert_eq!(result, Ok(true));
        assert_eq!(*writes.borrow(), vec![" agent --profile worker\r"]);

        // Negative calibration: the TUI protocol misclassifies that same quick burst
        // as unconfirmed, despite having written the command. No late output is added
        // merely to make the launch pass.
        let old_shared = submit_test_shared();
        bump_output(&old_shared, 8);
        let old_writes = std::cell::RefCell::new(Vec::<String>::new());
        let old_result = rt.run_submit_protocol(
            &old_shared,
            " agent --profile worker",
            5,
            5,
            clock.now_millis() + 600,
            0,
            |data| {
                old_writes.borrow_mut().push(data.to_string());
                bump_output(&old_shared, 24);
                Ok(())
            },
        );
        assert_eq!(
            old_result,
            Ok(false),
            "fast startup is not post-baseline growth"
        );
        assert_eq!(*old_writes.borrow(), vec![" agent --profile worker", "\r"]);
    }

    /// A cold tab whose prompt NEVER settles within the bound returns
    /// `submitted:false` having written NOTHING — so a caller retry is safe (not
    /// additive: no stray Enter, no half-typed line).
    #[test]
    fn shell_submit_never_settling_writes_nothing_and_is_false() {
        let reg = FakeRegistry::new();
        let clock = MonotonicClock::new();
        let rt = PtySessionRuntime::new(&reg, &clock);
        let shared = submit_test_shared();
        // seq stays 0 the whole time (the cold shell never renders a prompt) with a
        // require_start settle ⇒ it can never observe the quiet window.
        let writes = std::cell::RefCell::new(Vec::<String>::new());
        let res = rt.run_shell_submit_protocol(&shared, "ls", 5, clock.now_millis() + 60, |d| {
            writes.borrow_mut().push(d.to_string());
            Ok(())
        });
        assert_eq!(res, Ok(false), "never-settled prompt ⇒ not submitted");
        assert!(
            writes.borrow().is_empty(),
            "NOTHING was written — a retry is safe, not additive: {:?}",
            *writes.borrow(),
        );
    }

    /// A cold tab that renders its prompt after a delay settles, then submits
    /// once — the just-opened-tab happy path (bounded wait → single clean run).
    #[test]
    fn shell_submit_waits_for_a_cold_prompt_then_submits_once() {
        let reg = FakeRegistry::new();
        let clock = MonotonicClock::new();
        let rt = PtySessionRuntime::new(&reg, &clock);
        let shared = submit_test_shared();
        // Background: the cold shell renders its prompt after a brief delay, then
        // goes quiet — the settle must wait for it to appear.
        let bg_shared = shared.clone();
        let bg = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(5));
            bump_output(&bg_shared, 12);
        });
        let writes = std::cell::RefCell::new(Vec::<String>::new());
        let res = rt.run_shell_submit_protocol(&shared, "ls", 5, clock.now_millis() + 2000, |d| {
            writes.borrow_mut().push(d.to_string());
            Ok(())
        });
        bg.join().unwrap();
        assert_eq!(res, Ok(true), "the prompt rendered + settled ⇒ submitted");
        assert_eq!(
            *writes.borrow(),
            vec!["ls\r".to_string()],
            "one clean run — the line + CR exactly once",
        );
    }

    /// Two back-to-back shell submits that each report `true` write the
    /// command EXACTLY once each — two runs, no phantom extra Enters (the live
    /// double-run regression guard).
    #[test]
    fn shell_submit_two_true_runs_write_exactly_twice() {
        let reg = FakeRegistry::new();
        reg.insert("s");
        let shared = reg.shared_handle("s").unwrap();
        bump_output(&shared, 8); // warm prompt, already settled
        let clock = MonotonicClock::new();
        let rt = PtySessionRuntime::new(&reg, &clock);
        let writes = reg.writes_of("s");

        let a = rt.submit_shell_line("s", "echo one", 5, 600).unwrap();
        let b = rt.submit_shell_line("s", "echo two", 5, 600).unwrap();
        assert!(a && b, "both settled submits report true");

        let sent: Vec<String> = writes
            .lock()
            .unwrap()
            .iter()
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect();
        assert_eq!(
            sent,
            vec!["echo one\r".to_string(), "echo two\r".to_string()],
            "exactly two writes — one CR each, no phantom Enter: {sent:?}",
        );
    }

    /// A shell submit into a missing session is a hard `Err` (parity with `write`).
    #[test]
    fn shell_submit_missing_session_is_err() {
        let reg = FakeRegistry::new();
        let clock = MonotonicClock::new();
        let rt = PtySessionRuntime::new(&reg, &clock);
        let Err(e) = rt.submit_shell_line("nope", "hi", 1, 10) else {
            panic!("expected an Err for a missing session");
        };
        assert!(e.contains("session not found"), "error was: {e}");
    }

    /// A write failure at submit time surfaces as the error (never a false `true`).
    #[test]
    fn shell_submit_propagates_write_error() {
        let reg = FakeRegistry::new();
        reg.insert("s");
        reg.set_write_fails("s");
        let shared = reg.shared_handle("s").unwrap();
        bump_output(&shared, 8); // settled, so the protocol reaches the write
        let clock = MonotonicClock::new();
        let rt = PtySessionRuntime::new(&reg, &clock);
        let res = rt.submit_shell_line("s", "ls", 5, 600);
        assert!(res.is_err(), "a write failure is an Err, not Ok(true)");
    }

    // ---- subscribe: attach ------------------------------------------------------

    /// A session that has produced nothing yet is attached at once: the
    /// display listens before it subscribes, so its first output arrives on
    /// the stream instead of being waited for.
    #[test]
    fn subscribe_attaches_a_silent_session_without_waiting() {
        let reg = FakeRegistry::new();
        reg.insert("cold");
        let clock = MonotonicClock::new();
        let rt = PtySessionRuntime::new(&reg, &clock);
        let start = Instant::now();
        let r = rt.subscribe("cold").expect("subscribe succeeds");
        assert!(
            start.elapsed() < Duration::from_millis(200),
            "a silent session must not be waited on"
        );
        assert_eq!(r.snapshot.seq, 0);
    }

    /// Subscribe into a missing session is a hard `Err` (exact message preserved).
    #[test]
    fn subscribe_missing_session_is_err() {
        let reg = FakeRegistry::new();
        let clock = MonotonicClock::new();
        let rt = PtySessionRuntime::new(&reg, &clock);
        let Err(e) = rt.subscribe("gone") else {
            panic!("expected Err");
        };
        assert!(
            e.contains("subscribe_terminal: session not found"),
            "was: {e}"
        );
    }

    /// A submit whose write fails at the registry propagates the error, not `Ok`.
    #[test]
    fn submit_line_propagates_a_registry_write_error() {
        let reg = FakeRegistry::new();
        reg.insert("s");
        reg.set_write_fails("s");
        let clock = MonotonicClock::new();
        let rt = PtySessionRuntime::new(&reg, &clock);
        let res = rt.submit_line("s", "hi", 0, 0, 20, 0);
        assert_eq!(res, Err("Write error: fake".to_string()));
    }

    // ---- subscribe: the settle-skip guards + warm-gate + attach race ----------

    /// A warm-handoff session gates first paint until `suppress_until_prompt`
    /// clears; subscribe must wait for the gate so the snapshot includes the
    /// post-cd prompt rather than racing it onto the live stream.
    #[test]
    fn subscribe_waits_for_the_warm_handoff_gate() {
        let reg = FakeRegistry::new();
        let shared = reg.insert("warm");
        let runtime = reg.runtime_of("warm");
        runtime.suppress_until_prompt.store(true, Ordering::Relaxed);

        let bg_runtime = runtime.clone();
        let bg_shared = shared.clone();
        let bg = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(8));
            bump_output(&bg_shared, 12); // the prompt lands...
            bg_runtime
                .suppress_until_prompt
                .store(false, Ordering::Relaxed); // ...then the gate opens
        });

        let clock = MonotonicClock::new();
        let rt = PtySessionRuntime::new(&reg, &clock);
        let r = rt.subscribe("warm").expect("subscribe succeeds");
        bg.join().unwrap();
        assert_eq!(
            r.snapshot.seq, 12,
            "waited for the gate ⇒ snapshot includes the prompt"
        );
    }

    /// If the session is displaced by a same-id respawn between snapshot and index
    /// insert, `attach_viewport` fails "no longer live" — subscribe must propagate
    /// that, never treat a failed attach as success.
    #[test]
    fn subscribe_propagates_the_attach_displaced_error() {
        let reg = FakeRegistry::new();
        let shared = reg.insert("race");
        bump_output(&shared, 3);
        reg.displace_on_attach();
        let clock = MonotonicClock::new();
        let rt = PtySessionRuntime::new(&reg, &clock);
        let Err(e) = rt.subscribe("race") else {
            panic!("expected the displaced error");
        };
        assert!(e.contains("is no longer live"), "was: {e}");
    }

    // ---- The ClockPort seam is real: termination is clock-driven --------------

    struct FakeClock(u64);
    impl ClockPort for FakeClock {
        fn now_millis(&self) -> u64 {
            self.0
        }
    }

    /// With an injected clock already at the deadline (`timeout_ms == 0`), the
    /// settle + confirm/nudge loops terminate purely from the clock — no real
    /// waiting — proving time flows through the `ClockPort` seam, not `Instant`.
    #[test]
    fn the_clock_seam_drives_termination_without_wall_time() {
        let reg = FakeRegistry::new();
        reg.insert("s");
        let clock = FakeClock(1_000);
        let rt = PtySessionRuntime::new(&reg, &clock);
        let start = Instant::now();
        let res = rt.submit_line("s", "hi", 5, 5, 0, 3);
        assert_eq!(
            res,
            Ok(false),
            "deadline already reached ⇒ held, not confirmed"
        );
        // Only the fixed keystroke gap between the text and its Enter is slept;
        // every clock-driven wait ends at once on the frozen clock.
        assert!(
            start.elapsed() < Duration::from_millis(AGENT_ENTER_GAP_MIN_MS + 300),
            "clock-driven termination must not sleep to a wall-clock deadline"
        );
        assert_eq!(
            *reg.writes_of("s").lock().unwrap(),
            vec![b"hi".to_vec(), b"\r".to_vec()],
            "the text and its Enter are still written exactly once"
        );
    }
}
