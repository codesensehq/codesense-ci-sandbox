// SPDX-License-Identifier: MIT
//! The in-process agent session event bus. This module owns:
//!   * the `tokio::sync::broadcast` hub agent signals fan into (`hub`) — the
//!     routine runtime's agent-done intake drains it;
//!   * the scoped/TTL read-capability token store the read-only HTTP routes
//!     accept from a dashboard render (`token`).
//!
//! Doc changes are not carried here: pages and in-app views query the context
//! engine (`crate::context_query`, SW-OC-26) and re-query on its change event.

pub mod frame;
pub mod hub;
pub mod token;

use serde::{Deserialize, Serialize};

use crate::roles::Phase;

pub use hub::DashboardStreamHub;
pub use token::{ScopedCapabilityStore, ScopedToken};

/// An agent `signal_done` status. `needs-attention` is first-class (SW-DX-7).
///
/// SW-AU-52 reuses this as the durable run-outcome `status` (`run_store::RunOutcomeEvent`), so it
/// carries serde: `kebab-case` matches the wire form the hand-written `from_wire`
/// already use (`needs-attention`), keeping serde and those mappers in agreement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionStatus {
    Success,
    Failed,
    NeedsAttention,
}

impl SessionStatus {
    /// Parse the wire value; unknown values are rejected by the MCP handler
    /// before they reach here, so an unknown maps to `None`.
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "success" => Some(SessionStatus::Success),
            "failed" => Some(SessionStatus::Failed),
            "needs-attention" => Some(SessionStatus::NeedsAttention),
            _ => None,
        }
    }
}

/// A session (agent) change, as carried on the hub.
#[derive(Debug, Clone, PartialEq)]
pub enum StatusDelta {
    SessionStatus {
        session_id: String,
        status: SessionStatus,
        summary: Option<String>,
    },
    SessionMessage {
        session_id: String,
        role: String,
        text: String,
    },
    /// SW-GW-4: a mid-run progress signal (`signal_progress`). Non-terminal
    /// sibling of `SessionStatus` — a fixed lifecycle `phase` (+ optional N-of-M
    /// and a safe-DOM `note`), carried over the same session-keyed channel.
    SessionProgress {
        session_id: String,
        phase: Phase,
        step: Option<u32>,
        total: Option<u32>,
        note: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_status_parses_the_wire_values_only() {
        assert_eq!(
            SessionStatus::from_wire("success"),
            Some(SessionStatus::Success)
        );
        assert_eq!(
            SessionStatus::from_wire("failed"),
            Some(SessionStatus::Failed)
        );
        assert_eq!(
            SessionStatus::from_wire("needs-attention"),
            Some(SessionStatus::NeedsAttention)
        );
        assert_eq!(SessionStatus::from_wire("done"), None);
    }
}
