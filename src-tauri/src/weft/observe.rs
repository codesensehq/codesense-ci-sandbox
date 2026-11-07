// SPDX-License-Identifier: MIT
//! SW-WV-A4 — the observe-only read primitives a controller or triage role uses to
//! WATCH a worker without holding any orchestration verb: `wv_read_recent` (plain
//! text + a seq watermark) and `wv_wait` (a non-polling resolver for the `"done"`
//! transition).
//!
//! # Off-thread invariant (AC-8)
//!
//! The `wv_read_recent` terminal read is the emulator text read
//! ([`crate::commands::pty_backend::visible_text`]), on the blocking pool, never
//! on the async reactor. [`wait_for_done`] is pure async
//! event-plumbing: it drives a [`tokio::sync::oneshot`]-shaped resolution off two
//! injected channels wrapped in [`tokio::time::timeout`], with NO `std::thread::sleep`
//! and NO busy-poll loop. The resolver holds no lock and touches no SQLite.
//!
//! # `WeftOsc133Relay` — why a broadcast fan-out, not a direct subscribe
//!
//! [`crate::terminal::Osc133Bus::subscribe`] registers a PERMANENT listener with no
//! unsubscribe. A per-call `wv_wait` cannot subscribe directly — leaked listeners
//! would accumulate one per wait. Instead A4 arms ONE permanent bus subscriber at
//! startup ([`WeftOsc133Relay`]) that fans every event into a
//! [`tokio::sync::broadcast`] channel; each `wv_wait` takes a per-call
//! `broadcast::Receiver` (naturally per-call, dropped on resolution — no permanent
//! state).

use std::time::Duration;

use tokio::sync::{broadcast, mpsc};

use crate::terminal::Osc133Event;

/// Channel capacity for the OSC-133 broadcast fan-out. A short burst of command-end
/// events between two `wv_wait` polls is absorbed; an overflow only costs a lagged
/// receiver a `RecvError::Lagged` (handled as "keep waiting"), never a leak.
pub const OSC133_RELAY_CAPACITY: usize = 64;

/// The startup-registered, permanent [`crate::terminal::Osc133Bus`] subscriber that
/// fans every OSC-133 event into a [`broadcast::Sender`] (SW-WV-A4, Task 2). Managed
/// Tauri state. `wv_wait` creates a per-call receiver via [`Self::subscribe`] and
/// drops it on resolution.
#[derive(Debug, Clone)]
pub struct WeftOsc133Relay(broadcast::Sender<Osc133Event>);

impl WeftOsc133Relay {
    /// Build the relay and hand back its first receiver. The bus listener the caller
    /// registers at startup calls [`Self::sender`]`.send(ev)` for every event; the
    /// returned receiver may be dropped immediately (subsequent per-call receivers
    /// come from [`Self::subscribe`]).
    pub fn new() -> (Self, broadcast::Receiver<Osc133Event>) {
        let (tx, rx) = broadcast::channel(OSC133_RELAY_CAPACITY);
        (WeftOsc133Relay(tx), rx)
    }

    /// The broadcast sender the startup bus listener pushes into.
    pub fn sender(&self) -> broadcast::Sender<Osc133Event> {
        self.0.clone()
    }

    /// A fresh per-call receiver for one `wv_wait`. Dropped on resolution — broadcast
    /// receivers are per-call by construction and leave no permanent state.
    pub fn subscribe(&self) -> broadcast::Receiver<Osc133Event> {
        self.0.subscribe()
    }
}

impl Default for WeftOsc133Relay {
    fn default() -> Self {
        WeftOsc133Relay::new().0
    }
}

/// A resolution pushed onto the `wv_wait` done-channel from the Tauri
/// `mcp:agent-done` listener (SW-WV-A4, AC-2). `signal_done` is authoritative: this
/// is the verdict the WORKER (or the watchdog) reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitResolution {
    /// The worker signalled a terminal finish; `status` is the verbatim wire value
    /// (`"success"` / `"failed"` / `"needs-attention"`).
    Done { status: String },
}

/// The wire shape `wv_wait` returns (SW-WV-A4). Serialised to camelCase JSON at the
/// MCP boundary. `resolved` false + `timed_out` true is the silence outcome (AC-4);
/// `resolved` true carries the `status` (and, on the OSC-133 path, an `exit_code`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WaitResult {
    /// A terminal signal (agent-done OR command-end) arrived before the timeout.
    pub resolved: bool,
    /// The verbatim done-status on an agent-done resolution, or `"done"` on an
    /// OSC-133 command-end resolution. `None` on a timeout.
    pub status: Option<String>,
    /// The shell exit code on an OSC-133 command-end resolution (AC-3). `None`
    /// otherwise.
    pub exit_code: Option<i32>,
    /// `Some(true)` when the timeout elapsed with neither signal (AC-4). `None` on a
    /// resolution.
    pub timed_out: Option<bool>,
}

impl WaitResult {
    /// The timed-out result (AC-4): no signal within the budget, no roster write.
    fn timed_out() -> Self {
        WaitResult {
            resolved: false,
            status: None,
            exit_code: None,
            timed_out: Some(true),
        }
    }

    /// Serialise to the camelCase wire JSON the MCP verb returns.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "resolved": self.resolved,
            "status": self.status,
            "exitCode": self.exit_code,
            "timedOut": self.timed_out,
        })
    }
}

/// The non-polling `wv_wait` core (SW-WV-A4, AC-2/AC-3/AC-4). Resolves the FIRST of:
/// (a) a [`WaitResolution::Done`] on `done_rx` (fed by the `mcp:agent-done` Tauri
/// listener), or (b) an OSC-133 `CommandEnd` on `osc_rx` (fed by [`WeftOsc133Relay`],
/// pre-filtered to the bound session by the relay/caller is NOT assumed — this fn
/// filters `osc_rx` events to `session_id` itself, so a shared broadcast is safe).
/// Whichever arrives first wins; the other source is dropped on return (no leak).
///
/// `signal_done` is authoritative: an agent-done and a command-end for the same
/// session race on the same `select!`, and the first-polled arm wins — a subsequent
/// signal is simply never read (the receivers drop). If neither fires within
/// `timeout`, returns [`WaitResult::timed_out`] and writes NO roster state (AC-4 —
/// that is the watchdog's job).
///
/// Pure async, no I/O, no `thread::sleep`, no busy loop (AC-8). Accepts abstract
/// channels so unit tests inject senders with no Tauri app context.
pub async fn wait_for_done(
    session_id: String,
    mut done_rx: mpsc::Receiver<WaitResolution>,
    mut osc_rx: broadcast::Receiver<Osc133Event>,
    timeout: Duration,
) -> WaitResult {
    let resolve = async {
        // Track each source's liveness so a CLOSED channel DISABLES only its own arm
        // (via the branch precondition) — never parks the whole `select!`, which would
        // abandon the still-live sibling arm. When both are closed nothing can ever
        // resolve, so we stop and let the outer timeout return `timed_out`.
        let mut done_open = true;
        let mut osc_open = true;
        loop {
            if !done_open && !osc_open {
                // No source can resolve — wait out the timeout (never busy-loop).
                std::future::pending::<()>().await;
            }
            tokio::select! {
                // Poll arms top-down: the worker's authoritative verdict (a) wins
                // over the observation-only OSC CommandEnd (b) when both are ready.
                biased;
                // (a) The worker's authoritative finish via `mcp:agent-done`.
                done = done_rx.recv(), if done_open => {
                    match done {
                        Some(WaitResolution::Done { status }) => {
                            return WaitResult {
                                resolved: true,
                                status: Some(status),
                                exit_code: None,
                                timed_out: None,
                            };
                        }
                        // Sender dropped with nothing queued — disable ONLY this arm;
                        // the OSC arm keeps being polled. A closed done-channel is not
                        // a resolution.
                        None => {
                            done_open = false;
                        }
                    }
                }
                // (b) OSC-133 shell-integration `CommandEnd` for THIS session.
                osc = osc_rx.recv(), if osc_open => {
                    match osc {
                        Ok(ev)
                            if ev.session_id == session_id
                                && ev.kind == crate::terminal::Osc133Kind::CommandEnd =>
                        {
                            return WaitResult {
                                resolved: true,
                                status: Some("done".to_string()),
                                exit_code: Some(ev.exit_code.unwrap_or(0)),
                                timed_out: None,
                            };
                        }
                        // A non-CommandEnd marker, or another session's event — keep
                        // waiting (loop re-polls both arms).
                        Ok(_) => continue,
                        // Lagged past the ring: a burst overflowed the buffer. The
                        // done-channel is still authoritative; keep waiting rather
                        // than mis-resolve.
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        // Relay sender gone (shutdown) — disable ONLY this arm; the
                        // done arm (and the timeout) still govern.
                        Err(broadcast::error::RecvError::Closed) => {
                            osc_open = false;
                        }
                    }
                }
            }
        }
    };

    match tokio::time::timeout(timeout, resolve).await {
        Ok(result) => result,
        Err(_elapsed) => WaitResult::timed_out(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{Osc133Event, Osc133Kind};

    // ---- AC-2: resolve on WaitResolution::Done without waiting the timeout ----

    #[tokio::test]
    async fn wv_wait_resolves_on_agent_done_event() {
        let (done_tx, done_rx) = mpsc::channel::<WaitResolution>(1);
        let (_osc_tx, osc_rx) = broadcast::channel::<Osc133Event>(8);

        let handle = tokio::spawn(wait_for_done(
            "s1".to_string(),
            done_rx,
            osc_rx,
            Duration::from_secs(5),
        ));
        done_tx
            .send(WaitResolution::Done {
                status: "success".to_string(),
            })
            .await
            .unwrap();

        let got = handle.await.unwrap();
        assert_eq!(
            got,
            WaitResult {
                resolved: true,
                status: Some("success".to_string()),
                exit_code: None,
                timed_out: None,
            }
        );
    }

    // ---- AC-3: resolve on OSC-133 CommandEnd ----

    #[tokio::test]
    async fn wv_wait_resolves_on_command_end_osc133() {
        let (_done_tx, done_rx) = mpsc::channel::<WaitResolution>(1);
        let (osc_tx, osc_rx) = broadcast::channel::<Osc133Event>(8);

        let handle = tokio::spawn(wait_for_done(
            "s1".to_string(),
            done_rx,
            osc_rx,
            Duration::from_secs(30),
        ));
        osc_tx
            .send(Osc133Event {
                session_id: "s1".to_string(),
                kind: Osc133Kind::CommandEnd,
                exit_code: Some(0),
            })
            .unwrap();

        let got = handle.await.unwrap();
        assert!(got.resolved);
        assert_eq!(got.status.as_deref(), Some("done"));
        assert_eq!(got.exit_code, Some(0));
        assert_eq!(got.timed_out, None);
    }

    #[tokio::test]
    async fn wv_wait_ignores_other_sessions_and_non_command_end_markers() {
        let (_done_tx, done_rx) = mpsc::channel::<WaitResolution>(1);
        let (osc_tx, osc_rx) = broadcast::channel::<Osc133Event>(8);

        let handle = tokio::spawn(wait_for_done(
            "s1".to_string(),
            done_rx,
            osc_rx,
            Duration::from_millis(80),
        ));
        // Another session's CommandEnd + this session's non-terminal markers — none
        // resolve; the wait times out.
        osc_tx
            .send(Osc133Event {
                session_id: "OTHER".to_string(),
                kind: Osc133Kind::CommandEnd,
                exit_code: Some(0),
            })
            .unwrap();
        osc_tx
            .send(Osc133Event {
                session_id: "s1".to_string(),
                kind: Osc133Kind::PromptStart,
                exit_code: None,
            })
            .unwrap();

        let got = handle.await.unwrap();
        assert_eq!(got.timed_out, Some(true));
        assert!(!got.resolved);
    }

    /// Regression: a CLOSED done-channel must disable only its own arm, never park
    /// the whole select — a subsequent OSC-133 CommandEnd still resolves the wait.
    #[tokio::test]
    async fn wv_wait_still_resolves_on_osc_after_done_channel_closes() {
        let (done_tx, done_rx) = mpsc::channel::<WaitResolution>(1);
        let (osc_tx, osc_rx) = broadcast::channel::<Osc133Event>(8);

        let handle = tokio::spawn(wait_for_done(
            "s1".to_string(),
            done_rx,
            osc_rx,
            Duration::from_secs(5),
        ));
        // Close the done-channel FIRST (drop its only sender), then fire OSC.
        drop(done_tx);
        // Give the resolver a chance to observe the closed done arm.
        tokio::task::yield_now().await;
        osc_tx
            .send(Osc133Event {
                session_id: "s1".to_string(),
                kind: Osc133Kind::CommandEnd,
                exit_code: Some(7),
            })
            .unwrap();

        let got = handle.await.unwrap();
        assert!(
            got.resolved,
            "OSC must still resolve after done-channel closed"
        );
        assert_eq!(got.status.as_deref(), Some("done"));
        assert_eq!(got.exit_code, Some(7));
    }

    /// Both channels closed → nothing can resolve → the timeout governs (no spin,
    /// no hang beyond the budget).
    #[tokio::test]
    async fn wv_wait_times_out_when_both_channels_close() {
        let start = std::time::Instant::now();
        let (done_tx, done_rx) = mpsc::channel::<WaitResolution>(1);
        let (osc_tx, osc_rx) = broadcast::channel::<Osc133Event>(8);
        drop(done_tx);
        drop(osc_tx);
        let got = wait_for_done("s1".to_string(), done_rx, osc_rx, Duration::from_millis(40)).await;
        assert_eq!(got.timed_out, Some(true));
        assert!(!got.resolved);
        // Bounded: must not spin (a busy loop over closed channels would peg the CPU
        // but still return ~at the budget; the real regression is a hang > budget).
        assert!(start.elapsed() < Duration::from_millis(500));
    }

    // ---- AC-4 / AC-8: timeout returns timed_out:true, no panic/leak, fast ----

    #[tokio::test]
    async fn wv_wait_times_out_with_timed_out_true() {
        let start = std::time::Instant::now();
        let (_done_tx, done_rx) = mpsc::channel::<WaitResolution>(1);
        let (_osc_tx, osc_rx) = broadcast::channel::<Osc133Event>(8);

        let got = wait_for_done("s1".to_string(), done_rx, osc_rx, Duration::ZERO).await;

        assert_eq!(
            got,
            WaitResult {
                resolved: false,
                status: None,
                exit_code: None,
                timed_out: Some(true),
            }
        );
        assert!(
            start.elapsed() < Duration::from_millis(100),
            "ZERO-timeout wait must return promptly, got {:?}",
            start.elapsed()
        );
    }

    /// AC-8: the resolver is oneshot/timeout-driven, never a busy-poll. A ZERO
    /// timeout with no events returns `timed_out` in well under 50 ms (a busy loop
    /// would spin or a `thread::sleep` would stall).
    #[tokio::test]
    async fn wv_wait_uses_oneshot_not_busy_poll() {
        let start = std::time::Instant::now();
        let (_done_tx, done_rx) = mpsc::channel::<WaitResolution>(1);
        let (_osc_tx, osc_rx) = broadcast::channel::<Osc133Event>(8);

        let got = wait_for_done("s1".to_string(), done_rx, osc_rx, Duration::ZERO).await;

        assert_eq!(got.timed_out, Some(true));
        assert!(
            start.elapsed() < Duration::from_millis(50),
            "non-blocking contract: expected <50ms, got {:?}",
            start.elapsed()
        );
    }

    // ---- signal_done authoritative when both fire ----

    #[tokio::test]
    async fn agent_done_wins_when_it_arrives_first() {
        let (done_tx, done_rx) = mpsc::channel::<WaitResolution>(1);
        let (osc_tx, osc_rx) = broadcast::channel::<Osc133Event>(8);

        // Queue the agent-done FIRST; the select polls it before the OSC arm.
        done_tx
            .send(WaitResolution::Done {
                status: "failed".to_string(),
            })
            .await
            .unwrap();
        osc_tx
            .send(Osc133Event {
                session_id: "s1".to_string(),
                kind: Osc133Kind::CommandEnd,
                exit_code: Some(0),
            })
            .unwrap();

        let got = wait_for_done("s1".to_string(), done_rx, osc_rx, Duration::from_secs(5)).await;
        // The authoritative worker verdict wins over the observation-only CommandEnd.
        assert_eq!(got.status.as_deref(), Some("failed"));
        assert_eq!(got.exit_code, None);
    }

    // ---- relay: per-call receivers are independent, capacity honored ----

    #[test]
    fn relay_subscribe_yields_independent_receivers() {
        let (relay, _first) = WeftOsc133Relay::new();
        let mut a = relay.subscribe();
        let mut b = relay.subscribe();
        relay
            .sender()
            .send(Osc133Event {
                session_id: "s".into(),
                kind: Osc133Kind::CommandEnd,
                exit_code: Some(1),
            })
            .unwrap();
        let ea = a.try_recv().unwrap();
        let eb = b.try_recv().unwrap();
        assert_eq!(ea.exit_code, Some(1));
        assert_eq!(eb.exit_code, Some(1));
    }
}
