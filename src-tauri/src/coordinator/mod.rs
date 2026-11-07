// SPDX-License-Identifier: MIT
//! The machine-local coordinator store.
//!
//! A single embedded SQLite database, behind the [`OrchestrationStore`] trait,
//! holds runtime coordination state: runnable/claimed runs and their leases, the
//! append-only run-event log, deliveries and run-request pointers, plus the
//! routine runtime tables (`crate::routines::state`). It gives the one property
//! loose JSONL cannot — atomic compare-and-set for claims — while every metadata
//! fact stays authoritative in YAML/frontmatter. The file lives at
//! [`default_store_path`]: `{codesense_home}/orchestration/coordinator.sqlite3`.

pub mod fault;
pub mod sqlite;
pub mod store;
pub mod types;

#[cfg(test)]
mod tests;

pub use sqlite::{default_store_path, SqliteOrchestrationStore};
pub use store::{OrchestrationStore, StoreError, StoreResult};
#[cfg(test)]
pub use types::RunState;
pub use types::{DeliveryRef, RunEvent, WorkerId};
