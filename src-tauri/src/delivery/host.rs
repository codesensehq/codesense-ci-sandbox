// SPDX-License-Identifier: MIT
//! The terminal server that runs a session: this Mac's local session server
//! or a connected machine's. Both speak `SessionWakeV1` through the same
//! client, so a wake takes one path wherever the session lives (SPEC §9.6).

use std::sync::Arc;

use codesense_pty_core::domain::PtySessionId;
use codesense_pty_core::session_wake::{
    DeliverRequest, OutcomeLog, TimerInfo, TimerSpec, WakeOutcome,
};

use crate::terminal::session_server::session_wake::{
    catch_up, WakeCallError, WakeCursor, WakeSync,
};
use crate::terminal::session_server::SessionServerClient;

/// A terminal server's `SessionWakeV1` surface.
pub(crate) trait WakeHost: Send + Sync {
    fn deliver(&self, request: DeliverRequest) -> Result<WakeOutcome, WakeCallError>;
    fn cancel(&self, session: &PtySessionId, key: &str) -> Result<WakeOutcome, WakeCallError>;
    fn timer_set(&self, spec: TimerSpec) -> Result<TimerInfo, WakeCallError>;
    fn timer_list(&self, session: Option<&PtySessionId>) -> Result<Vec<TimerInfo>, WakeCallError>;
    fn timer_cancel(&self, session: &PtySessionId, timer_id: &str) -> Result<bool, WakeCallError>;
    /// The outcome log after `since`; from then on this connection also
    /// receives each outcome as a push.
    fn outcomes(&self, since: u64) -> Result<OutcomeLog, WakeCallError>;
    /// Catch up from `cursor` (re-read from 0 after a server restart, minus
    /// what the cursor already took) and subscribe to outcome pushes.
    fn sync(&self, cursor: &mut WakeCursor) -> Result<WakeSync, WakeCallError> {
        catch_up(cursor, |since| self.outcomes(since))
    }
}

impl WakeHost for SessionServerClient {
    fn deliver(&self, request: DeliverRequest) -> Result<WakeOutcome, WakeCallError> {
        self.wake_deliver(request)
    }
    fn cancel(&self, session: &PtySessionId, key: &str) -> Result<WakeOutcome, WakeCallError> {
        self.wake_cancel(session, key)
    }
    fn timer_set(&self, spec: TimerSpec) -> Result<TimerInfo, WakeCallError> {
        self.wake_timer_set(spec)
    }
    fn timer_list(&self, session: Option<&PtySessionId>) -> Result<Vec<TimerInfo>, WakeCallError> {
        self.wake_timer_list(session)
    }
    fn timer_cancel(&self, session: &PtySessionId, timer_id: &str) -> Result<bool, WakeCallError> {
        self.wake_timer_cancel(session, timer_id)
    }
    fn outcomes(&self, since: u64) -> Result<OutcomeLog, WakeCallError> {
        self.wake_outcomes(since)
    }
    fn sync(&self, cursor: &mut WakeCursor) -> Result<WakeSync, WakeCallError> {
        self.wake_sync(cursor)
    }
}

/// Where a machine's terminal server stands right now.
pub(crate) enum HostLookup {
    Ready(Arc<dyn WakeHost>),
    /// Configured but not reachable now (disconnected, connecting,
    /// unreachable): `undelivered(machine_offline)`.
    Offline {
        detail: String,
    },
    /// No such machine, or not shared with this desktop:
    /// `undelivered(machine_unknown)`.
    Unknown,
    /// Reachable but cannot serve wakes (an incompatible build, or this Mac's
    /// terminal server not running).
    Unavailable {
        machine: String,
        detail: String,
    },
}

/// Find the terminal server of a machine (`local` for this Mac).
pub(crate) trait HostResolver: Send + Sync {
    fn resolve(&self, machine: &str) -> HostLookup;
}
