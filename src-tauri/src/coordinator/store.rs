// SPDX-License-Identifier: MIT
//! The `OrchestrationStore` seam (AD-4 §trait).
//!
//! This is the swappable port: the durable coordinator is expressed as a trait so
//! the SQLite implementation ([`super::sqlite::SqliteOrchestrationStore`]) can be
//! replaced without touching callers. Every method is a *coordination* primitive
//! whose correctness the contract tests prove under restart + concurrency + fault
//! injection.

use super::fault::FaultPoint;
use super::types::{RecoveryReport, RunEvent, RunnableClaim, WorkerId};

/// Errors surfaced by the coordinator. Structured so a caller can distinguish an
/// injected fault (test-only) from a real storage failure; the source chain is
/// preserved via the message.
#[derive(Debug)]
pub enum StoreError {
    /// The underlying SQLite driver returned an error.
    Sqlite(rusqlite::Error),
    /// Creating the store's parent directory failed (e.g. first open under a
    /// fresh `codesense_home`).
    Io(std::io::Error),
    /// A fault-injection abort fired (test harness only, AC-8). Carries the
    /// point so a test can assert *where* it aborted.
    InjectedFault(FaultPoint),
    /// A caller-side step inside a coordinator transaction failed; its error
    /// message is preserved.
    Effect(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Sqlite(e) => write!(f, "orchestration store sqlite error: {e}"),
            StoreError::Io(e) => write!(f, "orchestration store io error: {e}"),
            StoreError::InjectedFault(p) => {
                write!(f, "orchestration store injected fault at {p:?}")
            }
            StoreError::Effect(m) => write!(f, "orchestration effect failed: {m}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StoreError::Sqlite(e) => Some(e),
            StoreError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Sqlite(e)
    }
}

impl From<crate::shared::sqlite::OpenError> for StoreError {
    fn from(e: crate::shared::sqlite::OpenError) -> Self {
        match e {
            crate::shared::sqlite::OpenError::Io(e) => StoreError::Io(e),
            crate::shared::sqlite::OpenError::Sqlite(e) => StoreError::Sqlite(e),
        }
    }
}

pub type StoreResult<T> = Result<T, StoreError>;

pub trait OrchestrationStore {
    /// Append a run event to the durable, append-only log. `payload` is scrubbed
    /// at persist. Returns the assigned monotonic `seq`.
    fn append(&self, ev: RunEvent) -> StoreResult<i64>;

    /// Atomically claim one runnable run for `worker`, or `None` if none remain.
    /// Compare-and-set: exactly one concurrent caller can win any given run. The
    /// claim is stamped with `lease_expiry` — an opaque, caller-supplied deadline
    /// `recover` later compares against a caller-supplied cutoff, so an abandoned
    /// claim can be reclaimed. The store never reads a clock (SW-AU-80B AC-3).
    ///
    /// Caller invariant: `lease_expiry` must be a deadline in the future relative to
    /// the cutoffs `recover` will be called with; a lease already past a future cutoff
    /// would be reclaimed immediately, re-offering a live claim. The store does not
    /// (and cannot, being clock-free) enforce this.
    fn claim_ready(
        &self,
        worker: &WorkerId,
        lease_expiry: i64,
    ) -> StoreResult<Option<RunnableClaim>>;

    /// Scan the durable store on restart (SW-AU-80B AC-4). Reclaims every abandoned
    /// claim — a `claimed` run whose `lease_expiry` is `< now_cutoff` — back to
    /// `runnable`, then returns the durable work still to be re-offered. Runs in one
    /// `IMMEDIATE` transaction so the reclaim and the scan are a consistent snapshot;
    /// a crash mid-reclaim rolls back and a retry reclaims idempotently. `now_cutoff`
    /// is caller-supplied — the store reads no clock.
    ///
    /// Reclaim caveat: returning an expired claim to `runnable` can overlap with a slow
    /// original owner that is still alive (a lease is a timeout, not a fence), so the
    /// claim loop's executor must tolerate a re-drive.
    fn recover(&self, now_cutoff: i64) -> StoreResult<RecoveryReport>;
}
