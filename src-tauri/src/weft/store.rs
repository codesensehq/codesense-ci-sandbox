// SPDX-License-Identifier: MIT
//! [`WeftStore`] — the handle onto `weft.sqlite3`, and its error / result types
//! (SW-WV-B1).
//!
//! Mirrors the shape of [`crate::coordinator::store::StoreError`]:
//! a WAL-mode, one-connection-per-operation SQLite store where every
//! multi-statement mutation commits inside a single IMMEDIATE transaction. The
//! open + migration sequence and the write handlers live in
//! [`super::sqlite`]; this module owns the type surface.

use std::path::PathBuf;

/// Errors surfaced by the weft ledger store.
///
/// Deliberately does **NOT** carry a `NotOwner` variant: a single-writer CAS
/// loss is a *normal* outcome ([`MutateResult::NotOwner`]), not a failure.
/// Conflating the two would stop a caller telling an expected ownership miss
/// apart from a genuine storage error (Task 1 grounding).
#[derive(Debug)]
pub enum WeftStoreError {
    /// The underlying SQLite driver returned an error.
    Sqlite(rusqlite::Error),
    /// Creating the store's parent directory failed, or the filesystem refused
    /// WAL mode (surfaced by the shared open plumbing).
    Io(std::io::Error),
    /// A guarded transition matched no row because the target row was not in an
    /// expected prior state — surfaced rather than silently committing a
    /// half-state. Not used by B1's
    /// generic helpers, but required by the story (Task 1) and reserved for the
    /// A/B/C lifecycle handlers that need to distinguish an illegal transition
    /// from an ownership miss (covered by the module-level `dead_code` allow until
    /// B2+ constructs it).
    IllegalState {
        table: &'static str,
        id: i64,
        attempted: &'static str,
    },
    /// SW-WV-B2: a `wv_queue` lifecycle refusal that the caller surfaces as an
    /// error rather than a normal return — today only [`super::queue::QueueError::ClaimLost`]
    /// (the CAS-retry budget was exhausted). Ownership/state refusals from
    /// `wv_resolve_queue` are ordinary `Ok(Err(QueueError))` returns, not this.
    Queue(super::queue::QueueError),
    /// SW-WV-C1: a `wv_dispatch_records` refusal — a missing required field (no
    /// checkable Done-When) or a lost `open → done` CAS. The caller maps each to the
    /// right MCP outcome (a validation refusal vs. a state refusal).
    Dispatch(super::dispatch::DispatchError),
    /// SW-WV-C4: a `wv_handoff_records` refusal — a missing required field (a handoff
    /// with no story key or no finding). Maps to a validation refusal at the MCP seam.
    Handoff(super::handoff::HandoffError),
}

impl std::fmt::Display for WeftStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WeftStoreError::Sqlite(e) => write!(f, "weft store sqlite error: {e}"),
            WeftStoreError::Io(e) => write!(f, "weft store io error: {e}"),
            WeftStoreError::IllegalState {
                table,
                id,
                attempted,
            } => write!(
                f,
                "weft store illegal state: row {id} in {table} not in an expected prior state for {attempted}"
            ),
            WeftStoreError::Queue(e) => write!(f, "weft queue error: {e}"),
            WeftStoreError::Dispatch(e) => write!(f, "weft dispatch error: {e}"),
            WeftStoreError::Handoff(e) => write!(f, "weft handoff error: {e}"),
        }
    }
}

impl std::error::Error for WeftStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WeftStoreError::Sqlite(e) => Some(e),
            WeftStoreError::Io(e) => Some(e),
            WeftStoreError::IllegalState { .. } => None,
            WeftStoreError::Queue(e) => Some(e),
            WeftStoreError::Dispatch(e) => Some(e),
            WeftStoreError::Handoff(e) => Some(e),
        }
    }
}

impl WeftStoreError {
    /// SW-WV-B2: wrap a [`super::queue::QueueError`] the queue path chose to raise
    /// as an error (only `ClaimLost` today) into the store error type.
    pub fn from_queue(e: super::queue::QueueError) -> Self {
        WeftStoreError::Queue(e)
    }

    /// SW-WV-C1: wrap a [`super::dispatch::DispatchError`] into the store error type.
    pub fn from_dispatch(e: super::dispatch::DispatchError) -> Self {
        WeftStoreError::Dispatch(e)
    }

    /// SW-WV-C4: wrap a [`super::handoff::HandoffError`] into the store error type.
    pub fn from_handoff(e: super::handoff::HandoffError) -> Self {
        WeftStoreError::Handoff(e)
    }
}

impl From<rusqlite::Error> for WeftStoreError {
    fn from(e: rusqlite::Error) -> Self {
        WeftStoreError::Sqlite(e)
    }
}

impl From<std::io::Error> for WeftStoreError {
    fn from(e: std::io::Error) -> Self {
        WeftStoreError::Io(e)
    }
}

/// The shared open plumbing ([`crate::shared::sqlite::OpenError`]) distinguishes
/// a parent-dir/WAL failure (`Io`) from a SQLite-level failure (`Sqlite`);
/// preserve that split when converting into the store's own error.
impl From<crate::shared::sqlite::OpenError> for WeftStoreError {
    fn from(e: crate::shared::sqlite::OpenError) -> Self {
        match e {
            crate::shared::sqlite::OpenError::Io(io) => WeftStoreError::Io(io),
            crate::shared::sqlite::OpenError::Sqlite(sq) => WeftStoreError::Sqlite(sq),
        }
    }
}

/// The convenience result alias for weft store operations.
pub type WeftStoreResult<T> = Result<T, WeftStoreError>;

/// The outcome of a single-writer mutation ([`super::sqlite::wv_mutate_owned`]).
///
/// A CAS loss (`NotOwner`) is an ordinary return, not an error — the caller
/// decides whether losing the write is expected (another owner holds the row)
/// or a bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutateResult {
    /// The guarded UPDATE matched exactly one row and was applied.
    Updated,
    /// The guarded UPDATE matched no row — the caller is not the current owner
    /// of `(id, owner)`. The row is unchanged; no audit row was written.
    NotOwner,
}

/// The handle onto a team's `weft.sqlite3`.
///
/// Cheap to `clone` (just the path); a fresh connection is opened per operation
/// via [`Self::conn`], so a cloned handle shared across blocking-pool threads is
/// exactly the intended concurrency model (mirrors `SqliteOrchestrationStore`).
///
// BLOCKING-POOL ONLY — never call from the UI thread or a synchronous Tauri
// command handler. All `WeftStore` operations are synchronous SQLite calls and
// must run on Tauri's blocking thread pool or inside `tokio::task::spawn_blocking`
// (ARCHITECTURE §1.6). The `no_weft_store_call_on_ui_thread` CI test in
// `super::sqlite` fails the build if a `WeftStore` call reaches a
// `#[tauri::command]` body without an intervening `spawn_blocking`.
#[derive(Debug, Clone)]
pub struct WeftStore {
    pub(super) path: PathBuf,
    pub(super) pool: std::sync::Arc<std::sync::Mutex<Vec<rusqlite::Connection>>>,
}

/// A checked-out connection returns to its store after the operation.
pub struct StoreConnection {
    pub(super) connection: Option<rusqlite::Connection>,
    pub(super) pool: std::sync::Arc<std::sync::Mutex<Vec<rusqlite::Connection>>>,
}
impl std::ops::Deref for StoreConnection {
    type Target = rusqlite::Connection;
    fn deref(&self) -> &Self::Target {
        self.connection.as_ref().unwrap()
    }
}
impl std::ops::DerefMut for StoreConnection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.connection.as_mut().unwrap()
    }
}
impl Drop for StoreConnection {
    fn drop(&mut self) {
        if let Some(conn) = self.connection.take() {
            // A failed transaction is rolled back before the handle can be reused.
            if !conn.is_autocommit() {
                let _ = conn.execute_batch("ROLLBACK");
            }
            let mut pool = self.pool.lock().unwrap_or_else(|e| e.into_inner());
            if pool.len() < 4 {
                pool.push(conn);
            }
        }
    }
}

#[cfg(test)]
impl WeftStore {
    pub(crate) fn shares_pool(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.pool, &other.pool)
    }
}
