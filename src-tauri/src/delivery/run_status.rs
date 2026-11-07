// SPDX-License-Identifier: MIT
//! A delivery's status as a wake run records it (R7, SPEC §9.6): `queued`,
//! then `delivered`, `coalesced` into another run, `undelivered(reason)` or
//! `unknown`. SW-AGR-2 writes these to the run row; the frontend reads the
//! same shape on every wake event.

use codesense_pty_core::session_wake::{DeliveryStatus, UndeliveredReason};
use serde::{Deserialize, Serialize};

use crate::routines::runs::phase;

/// One delivery's status, as the run row, the wake events and the tab's wake
/// history carry it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum WakeRunStatus {
    /// Waiting for the session to be idle, until `deadlineMs` (host clock).
    Queued {
        #[serde(rename = "deadlineMs")]
        deadline_ms: u64,
    },
    /// Typed into the session at `deliveredAtMs` (host clock).
    Delivered {
        #[serde(rename = "deliveredAtMs")]
        delivered_at_ms: u64,
    },
    /// A timer fire folded into the delivery `into` already queued.
    Coalesced {
        into: String,
    },
    Undelivered {
        reason: UndeliveredReason,
    },
    /// The write was not confirmed: it may have reached the session. Never
    /// re-sent, automatically or by a Retry (R5).
    Unknown {
        detail: String,
    },
}

impl WakeRunStatus {
    pub(crate) fn of(status: &DeliveryStatus) -> Self {
        match status {
            DeliveryStatus::Queued { deadline_ms } => Self::Queued {
                deadline_ms: *deadline_ms,
            },
            DeliveryStatus::Delivered { at_ms } => Self::Delivered {
                delivered_at_ms: *at_ms,
            },
            DeliveryStatus::Coalesced { into } => Self::Coalesced { into: into.clone() },
            DeliveryStatus::Undelivered { reason } => Self::Undelivered { reason: *reason },
            DeliveryStatus::Unknown { detail } => Self::Unknown {
                detail: detail.clone(),
            },
        }
    }

    /// Every status but `queued` is final.
    pub(crate) fn is_final(&self) -> bool {
        !matches!(self, Self::Queued { .. })
    }

    /// The run phase (`routine_runs.phase`) a wake run is in after this
    /// status. A delivered wake keeps its run `running` (it records
    /// `delivered_at`): the woken agent still finishes it with
    /// `routine_done`, `routine_publish_result` + `signal_done`, or the run's
    /// own end. Only `undelivered`, `unknown` and `coalesced` end the run.
    pub(crate) fn run_phase(&self) -> &'static str {
        match self {
            Self::Queued { .. } => phase::QUEUED,
            Self::Delivered { .. } => phase::RUNNING,
            Self::Coalesced { .. } => phase::COALESCED,
            // SW-AGR-13: inside the routine's `min_gap` with `overlap: skip`.
            Self::Undelivered {
                reason: UndeliveredReason::MinGap,
            } => phase::SKIPPED,
            Self::Undelivered { .. } => phase::UNDELIVERED,
            Self::Unknown { .. } => phase::UNKNOWN,
        }
    }

    /// Whether this status ends the wake run.
    pub(crate) fn ends_run(&self) -> bool {
        matches!(
            self,
            Self::Coalesced { .. } | Self::Undelivered { .. } | Self::Unknown { .. }
        )
    }

    /// When the text was typed (host clock), for the run's `delivered_at`.
    pub(crate) fn delivered_at_ms(&self) -> Option<u64> {
        match self {
            Self::Delivered { delivered_at_ms } => Some(*delivered_at_ms),
            _ => None,
        }
    }

    /// The run reason (`routine_runs.reason`): an undelivered run's reason.
    pub(crate) fn run_reason(&self) -> Option<&'static str> {
        match self {
            Self::Undelivered { reason } => Some(reason_wire(*reason)),
            _ => None,
        }
    }

    /// Whether the run row may offer a Retry. Never for `unknown` (the text
    /// may already be in the session, R5), and never where trying again
    /// cannot help: an unknown machine, an archived routine, a routine past
    /// one of its own wake limits, a session past the backstop.
    pub(crate) fn may_retry(&self) -> bool {
        match self {
            Self::Undelivered { reason } => !matches!(
                reason,
                UndeliveredReason::MachineUnknown
                    | UndeliveredReason::Archived
                    | UndeliveredReason::LimitReached
                    | UndeliveredReason::MaxWakes
                    | UndeliveredReason::MinGap
                    | UndeliveredReason::RateLimited
            ),
            _ => false,
        }
    }
}

/// The wire spelling of `reason` (`busy_timeout`, `server_restarted`, …).
pub(crate) fn reason_wire(reason: UndeliveredReason) -> &'static str {
    match reason {
        UndeliveredReason::BusyTimeout => "busy_timeout",
        UndeliveredReason::NeedsPreparation => "needs_preparation",
        UndeliveredReason::MachineOffline => "machine_offline",
        UndeliveredReason::MachineUnknown => "machine_unknown",
        UndeliveredReason::SessionEnded => "session_ended",
        UndeliveredReason::Unobservable => "unobservable",
        UndeliveredReason::AgentFailed => "agent_failed",
        UndeliveredReason::Archived => "archived",
        UndeliveredReason::ServerRestarted => "server_restarted",
        UndeliveredReason::LimitReached => "limit_reached",
        UndeliveredReason::DraftPending => "draft_pending",
        UndeliveredReason::MaxWakes => "max_wakes",
        UndeliveredReason::MinGap => "min_gap",
        UndeliveredReason::RateLimited => "rate_limited",
        UndeliveredReason::Unavailable => "unavailable",
    }
}
