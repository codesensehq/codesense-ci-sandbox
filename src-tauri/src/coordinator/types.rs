// SPDX-License-Identifier: MIT
//! Domain vocabulary for the durable orchestration coordinator.
//!
//! Every type here names a *coordination* fact — a claim, a run event, a
//! delivery. None of them mirror a metadata value; metadata
//! (status, rules, definitions, config) stays authoritative in YAML/frontmatter
//! (AD-11). The `OrchestrationStore` trait ([`super::store`]) is expressed purely
//! in these types so a future non-SQLite impl can satisfy the same contract.

use serde::{Deserialize, Serialize};

/// Opaque worker/actor identity, used for **claim attribution only** — never for
/// a role check (AD-2: no RBAC). The future `authorize(actor, verb, subject?)`
/// shim (AU-89) is where gating lands; this spike introduces no permission model.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkerId(pub String);

impl WorkerId {
    pub fn new(id: impl Into<String>) -> Self {
        WorkerId(id.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A durable, append-only run event. The event log is the coordinator's evidence
/// projection (ARCH §5/§9): materialized rows are rebuildable, never a second
/// source of truth. `payload` is scrubbed at persist (AC-11) so the store can
/// never become a channel that leaks an unredacted secret to disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunEvent {
    pub run_id: String,
    /// Opaque coordination-event label (e.g. `"claimed"`, `"resumed"`). Not a
    /// metadata status value.
    pub kind: String,
    /// Free-form event detail. Redacted through `secret_scan::redact_secrets`
    /// before it reaches a DB row.
    pub payload: String,
}

/// The winner's claim on a runnable run (AC-2). Returned to exactly one worker;
/// every concurrent loser sees `None`. Carries the lease deadline written with the
/// claim so a recovering process can tell a live claim from an abandoned one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnableClaim {
    pub run_id: String,
    pub worker: WorkerId,
    /// The opaque, caller-supplied lease deadline stamped on the claim. Compared
    /// against a caller-supplied cutoff by `recover` — the store never reads a clock.
    pub lease_expiry: i64,
}

/// A coordination-only reference to one durable delivery — the two identity fields
/// of the dedupe key, nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryRef {
    pub adapter_id: String,
    pub delivery_key: String,
}

/// What a restart scan finds durably waiting to be re-offered (AC-4/AC-5). `recover`
/// reclaims expired claims in place and returns this report of the durable work; the
/// live re-drive of these into a dispatcher is SW-AU-80D, not this story.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryReport {
    /// How many abandoned (`claimed`, lease expired) runs were returned to `runnable`.
    pub reclaimed_claims: u64,
    /// Runs currently `runnable` — to re-offer to a claimer (SW-AU-80D).
    pub runnable_runs: Vec<String>,
    /// Deliveries landed but not yet consumed by dispatch — to re-admit (SW-AU-80D).
    pub pending_deliveries: Vec<DeliveryRef>,
}

/// Lifecycle state of a coordinated run. This is *coordination* state — the
/// scheduler's view of where a run sits in the durable queue — not a
/// user-facing status metadata value (AD-11).
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    /// Eligible for `claim_ready`.
    Runnable,
    /// Claimed by a worker (CAS winner).
    Claimed,
    /// Terminal.
    Done,
}

#[cfg(test)]
impl RunState {
    pub fn as_db(self) -> &'static str {
        match self {
            RunState::Runnable => "runnable",
            RunState::Claimed => "claimed",
            RunState::Done => "done",
        }
    }
}
