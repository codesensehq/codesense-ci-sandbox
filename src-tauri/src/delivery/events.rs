// SPDX-License-Identifier: MIT
//! What the desktop tells the frontend about wakes (SPEC §9.10, R4).
//!
//! - `wake-delivered`: the text was typed. Drives the scrollback annotation
//!   (`⏰ ‹marker›`, a desktop annotation: nothing is written to the output
//!   stream) and the tab's 10-second `Woken by ‹marker›` caption.
//! - `wake-outcome`: every status a delivery reaches, including `queued` (the
//!   agent row's `⏰ due`) and the final ones (the run row and the tab's wake
//!   history).
//!
//! The shapes are pinned by `src-tauri/src/fixtures/wake/wake-events.json`,
//! which the frontend tests import.

use serde::{Deserialize, Serialize};

use super::run_status::WakeRunStatus;

pub(crate) const WAKE_DELIVERED_EVENT: &str = "wake-delivered";
pub(crate) const WAKE_OUTCOME_EVENT: &str = "wake-outcome";
/// A terminal server's catch-up health (after every sync attempt).
pub(crate) const WAKE_SYNC_HEALTH_EVENT: &str = "wake-sync-health";
/// SW-AGR-6: a session's timers were set or cancelled (by its agent or the
/// person), so its tab re-reads them. A fire is told by `wake-outcome`.
pub(crate) const SESSION_TIMERS_CHANGED_EVENT: &str = "session-timers-changed";

/// How an outcome reached this desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OutcomeSource {
    /// The answer to this desktop's request.
    Reply,
    /// Pushed by the terminal server as it happened.
    Push,
    /// Caught up on (re)connect from the server's outcome log.
    Sync,
    /// Decided on this desktop: the request never reached a terminal server,
    /// or the server lost it.
    Local,
}

/// How many deliveries a tab's wake history keeps.
pub(crate) const WAKE_HISTORY_CAP: usize = 20;

/// What woke a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum WakeOrigin {
    /// A routine run (SW-AGR-2).
    #[serde(rename_all = "camelCase")]
    Routine { routine_key: String, run_id: String },
    /// A session timer (SW-AGR-3).
    #[serde(rename_all = "camelCase")]
    Timer { timer_id: Option<String> },
    /// The person at this desktop.
    Human,
    /// A delivery this desktop did not ask for (another desktop's, or one it
    /// forgot): shown, never recorded as a run here.
    Other,
}

impl WakeOrigin {
    pub(crate) fn run_id(&self) -> Option<&str> {
        match self {
            Self::Routine { run_id, .. } => Some(run_id),
            _ => None,
        }
    }
}

/// `wake-delivered`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WakeDeliveredEvent {
    pub machine_id: String,
    pub session_id: String,
    pub delivery_id: String,
    /// What the scrollback annotation names (`⏰ ‹marker›`).
    pub marker: String,
    /// The tab caption, `Woken by ‹marker›`.
    pub caption: String,
    pub origin: WakeOrigin,
    /// Host clock, milliseconds since the Unix epoch.
    pub delivered_at_ms: u64,
    pub source: OutcomeSource,
    /// Re-read after this desktop lost its record of it: shown (no fresh
    /// caption needed), never recorded as a run again.
    pub replayed: bool,
}

/// `wake-outcome`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WakeOutcomeEvent {
    pub machine_id: String,
    pub session_id: String,
    /// Empty when the delivery never reached a host (an offline machine).
    pub delivery_id: String,
    pub idempotency_key: String,
    pub marker: String,
    pub origin: WakeOrigin,
    #[serde(flatten)]
    pub status: WakeRunStatus,
    /// The run row may offer Retry (never for `unknown`).
    pub may_retry: bool,
    /// This desktop asked for it, so it records the run.
    pub ours: bool,
    pub source: OutcomeSource,
    /// Re-read after this desktop lost its record of it: shown, never
    /// recorded as a run again.
    pub replayed: bool,
    /// Host clock, milliseconds since the Unix epoch.
    pub at_ms: u64,
}

/// `wake-sync-health`: whether this desktop is caught up with a machine's
/// terminal server.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SyncHealth {
    pub machine_id: String,
    /// The last catch-up succeeded.
    pub ok: bool,
    /// Consecutive failed catch-ups.
    pub failures: u32,
    pub last_error: Option<String>,
    /// This desktop's clock.
    pub last_ok_ms: Option<u64>,
    /// The record could not be written to disk (outcomes are still shown,
    /// but an app restart may re-read them).
    pub save_error: Option<String>,
}

/// One row of a tab's wake history (its timer popover).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WakeHistoryEntry {
    pub delivery_id: String,
    pub marker: String,
    pub origin: WakeOrigin,
    #[serde(flatten)]
    pub status: WakeRunStatus,
    pub at_ms: u64,
}

/// `session-timers-changed`: whose timers to re-read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionTimersChanged {
    pub machine_id: String,
    pub session_id: String,
}

/// Where wake events go: the app window in production, a recorder in tests.
pub(crate) trait WakeEventSink: Send + Sync {
    fn delivered(&self, event: &WakeDeliveredEvent);
    fn outcome(&self, event: &WakeOutcomeEvent);
    fn sync_health(&self, health: &SyncHealth);
    /// A session's timers changed; nothing by default.
    fn timers_changed(&self, _event: &SessionTimersChanged) {}
    /// Something to tell a person once in the inbox (SW-AGR-13); nothing by
    /// default.
    fn notice(&self, _event: &WakeNoticeEvent) {}
}

/// What a [`WakeNoticeEvent`] is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WakeNoticeKind {
    /// The terminal server's backstop refused wakes into a session (once per
    /// session per hour); `text` names the top source.
    Backstop,
    /// A wake with limits was not sent: the machine's terminal server is too
    /// old to enforce them (once per machine per day).
    ServerOutdated,
}

/// One inbox note the delivery core asks for, whatever the wake's source (a
/// routine, a timer, a person).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WakeNoticeEvent {
    pub kind: WakeNoticeKind,
    pub machine_id: String,
    pub session_id: String,
    pub origin: WakeOrigin,
    pub text: String,
}

/// The caption a woken tab shows.
pub(crate) fn caption(marker: &str) -> String {
    format!("Woken by {marker}")
}
