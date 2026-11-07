// SPDX-License-Identifier: MIT
//! SW-WV-C1 — the 4-field dispatch record + the done-when contract.
//!
//! A dispatch is a **structural** brief with exactly four fields —
//! `task` / `context` / `constraints` / `acceptance` (the Done-When) — persisted to
//! `wv_dispatch_records` **before** the worker starts, so "finished" is externally
//! checkable rather than self-declared. Brief content is **data, never instructions**
//! (ARCHITECTURE §3.3); the fixed four-field shape enforces it. The done-when
//! evaluation checks a signalled result against the recorded `acceptance` — the
//! finish line was written before the work began.
//!
//! This module owns the `wv_dispatch_records` transitions (the sanctioned single
//! writer, ARCHITECTURE §1.2 — the whole `src/weft/` tree is the sanctioned home).
//! Every mutation co-commits its `wv_write_log` audit row in the SAME IMMEDIATE
//! transaction via [`super::sqlite::write_audit_public`] (AUDIT-ATOMIC §1.6); the
//! done-stamp carries the ONE-WRITER CAS guard (`WHERE state='open' AND …` +
//! `changes()==1`) so a lost race / already-done record is a typed refusal, never a
//! silent no-op. Every function is synchronous SQLite on a caller-supplied
//! [`WeftStore`]; the MCP handler + the `signal_done` hook run them on the blocking
//! pool (§1.6).
//!
//! The `dispatch-done-stamp` hook C1 registers into `build_signal_done_hooks` is the
//! SECOND hook in the canonical order (`roster-update → dispatch-done-stamp → …`,
//! A3 sole owner of the registry + its order test — this story registers INTO it,
//! never re-orders or re-asserts).

use rusqlite::{params, OptionalExtension, TransactionBehavior};

use super::sqlite::{is_blank, now_iso, write_audit_public, wv_auto_append};
use super::store::{WeftStore, WeftStoreError, WeftStoreResult};

/// The `wv_dispatch_records` table name — a compile-time constant threaded to the
/// audit-write seam (never user input).
const DISPATCH: &str = "wv_dispatch_records";

/// One dispatch record as [`dispatch_state`] returns it (the externally-checkable
/// finish line + its lifecycle). `acceptance` is the Done-When, fixed at creation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchRecord {
    pub id: i64,
    pub story_id: String,
    pub task: String,
    pub context: String,
    pub constraints: String,
    pub acceptance: String,
    pub state: String,
    pub scope: Option<String>,
    pub session_id: Option<String>,
    pub created_at: String,
    pub done_at: Option<String>,
}

/// Typed refusals from the dispatch verbs — distinct from a storage error so the
/// caller maps each to the right MCP outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchError {
    /// A blank/whitespace `task` or `acceptance` — a dispatch with no checkable
    /// finish line (or no job) is refused (AC-1). The unbounded-Done-When failure is
    /// the exact hole this contract closes (mirrors B2's `ends_when` guard).
    MissingRequiredField { field: &'static str },
    /// `wv_mark_dispatch_done` found no `open` record to advance for the given key
    /// (already done / absent / a concurrent winner took it) — a typed refusal, not
    /// a silent no-op (AC-3).
    NotOpen,
}

impl std::fmt::Display for DispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DispatchError::MissingRequiredField { field } => write!(
                f,
                "dispatch {field} must not be blank (a dispatch needs a task and a checkable done-when)"
            ),
            DispatchError::NotOpen => write!(
                f,
                "no open dispatch record to mark done (already done, absent, or a concurrent winner took it)"
            ),
        }
    }
}

impl std::error::Error for DispatchError {}

/// Persist a 4-field dispatch brief in `state='open'` BEFORE the worker starts
/// (AC-1, AC-2). Refuses a blank `task` or `acceptance` (the Done-When). Empty
/// `context`/`constraints` are allowed — not every job constrains. The record's
/// `acceptance` is fixed at creation and never rewritten by the worker, so an
/// external reader (C3/C5, the Board) verifies the finish line without asking it.
/// Returns the AUTOINCREMENT id. Audit row co-committed (AC-6).
pub fn wv_write_dispatch(
    store: &WeftStore,
    story_id: &str,
    task: &str,
    context: &str,
    constraints: &str,
    acceptance: &str,
    scope: Option<&str>,
) -> Result<i64, WeftStoreError> {
    if is_blank(story_id) {
        return Err(WeftStoreError::from_dispatch(
            DispatchError::MissingRequiredField { field: "story_id" },
        ));
    }
    if is_blank(task) {
        return Err(WeftStoreError::from_dispatch(
            DispatchError::MissingRequiredField { field: "task" },
        ));
    }
    if is_blank(acceptance) {
        return Err(WeftStoreError::from_dispatch(
            DispatchError::MissingRequiredField {
                field: "acceptance",
            },
        ));
    }
    let now = now_iso();
    let mut conn = store.conn()?;
    // Route the AUTOINCREMENT insert through the sanctioned append helper — it owns
    // the IMMEDIATE tx, co-commits the `wv_write_log` audit row, and returns the
    // minted id (AC-6). Mirrors `wv_write_decision`.
    let id = wv_auto_append(
        &mut conn,
        "INSERT INTO wv_dispatch_records \
         (story_id, task, context, constraints, acceptance, state, scope, session_id, created_at, done_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, NULL, ?7, NULL)",
        params![story_id, task, context, constraints, acceptance, scope, now],
        DISPATCH,
    )?;
    Ok(id)
}

/// Advance the open dispatch record for the completing `session_id`, `open → done`,
/// stamping `session_id`/`done_at` (AC-3) — the done-when evaluation the
/// `dispatch-done-stamp` hook drives. This records that the worker signalled against
/// a recorded finish line; it does NOT itself judge success (that is C3's
/// verify-don't-fold + the human merge/accept, sub-epic E/F).
///
/// The record is resolved by its `story_id` (== the roster key the controller wrote
/// the brief under) and stamped with the completing `session_id`. Guarded
/// `WHERE story_id=? AND state='open'` + `changes()==1`: a lost CAS / already-done /
/// absent record returns [`DispatchError::NotOpen`], never a silent no-op. Audit
/// co-committed (AC-6).
pub fn wv_mark_dispatch_done(
    store: &WeftStore,
    story_id: &str,
    session_id: &str,
) -> Result<i64, WeftStoreError> {
    let now = now_iso();
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Read the target id under the tx so the audit key and the guard see one snapshot.
    let id: Option<i64> = tx
        .query_row(
            "SELECT id FROM wv_dispatch_records \
             WHERE story_id=?1 AND state='open' ORDER BY id LIMIT 1",
            params![story_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(id) = id else {
        return Err(WeftStoreError::from_dispatch(DispatchError::NotOpen));
    };
    let changed = tx.execute(
        "UPDATE wv_dispatch_records \
         SET state='done', session_id=?2, done_at=?3 \
         WHERE id=?1 AND state='open'",
        params![id, session_id, now],
    )?;
    if changed != 1 {
        // A concurrent winner flipped it between the read and the CAS — refuse rather
        // than commit a partial change; the tx drops (rollback) on return.
        return Err(WeftStoreError::from_dispatch(DispatchError::NotOpen));
    }
    write_audit_public(&tx, DISPATCH, "mutate", id)?;
    tx.commit()?;
    Ok(id)
}

/// The done-when evaluation the `signal_done` hook drives: resolve the roster row for
/// the completing session, then mark that story's open dispatch record done
/// (AC-3, AC-4). The hook has only the completing session's id, not the roster key,
/// so it resolves `story_id` here. Returns:
/// - `Ok(true)` — a record was marked done;
/// - `Ok(false)` — no live worker for the session, or no open dispatch record (a
///   non-worker completion, or a worker with no recorded brief) — a graceful skip so
///   the hook never fails a normal `signal_done`.
///
/// **Hook-order dependency (AC-4).** This is the SECOND `signal_done` hook; the FIRST
/// (roster-update) has ALREADY advanced the roster row `dispatch → signal` by the time
/// this runs (hooks execute sequentially in registration order). So the roster lookup
/// must accept BOTH the pre-hook `dispatch` state and the post-roster-update `signal`
/// state — matching only `status='dispatch'` here would find nothing in production and
/// silently never stamp the dispatch record done. `handoff`/`deprovision` are terminal
/// (the worker is gone) so they are excluded; a re-briefed story past those is a fresh
/// worker with a fresh roster row.
///
/// A store error still propagates as `Err` (the hook isolates it behind its own
/// `Result` at the registry level).
pub fn dispatch_done_by_session(
    store: &WeftStore,
    session_id: &str,
) -> Result<bool, WeftStoreError> {
    dispatch_done_for_worker(store, session_id, session_id)
}

pub(crate) fn dispatch_done_for_worker(
    store: &WeftStore,
    worker_key: &str,
    session_id: &str,
) -> Result<bool, WeftStoreError> {
    if is_blank(session_id) {
        return Ok(false);
    }
    // Resolve the story this live session belongs to (A2/A3 roster key). Accept both
    // `dispatch` and `signal` because the roster-update hook runs first (see above).
    let conn = store.conn()?;
    let story_id: Option<String> = conn
        .query_row(
            "SELECT name FROM wv_roster \
             WHERE session_id=?1 AND status IN ('dispatch','signal')",
            params![worker_key],
            |r| r.get(0),
        )
        .optional()?;
    drop(conn);
    let Some(story_id) = story_id else {
        return Ok(false); // no live worker for this session — graceful skip
    };
    match wv_mark_dispatch_done(store, &story_id, session_id) {
        Ok(_) => Ok(true),
        // No open dispatch record for this worker (never briefed, or already done) —
        // a graceful skip, not a failure of the completion path.
        Err(WeftStoreError::Dispatch(DispatchError::NotOpen)) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Read the dispatch records for a `story_id`, newest first (AC-2) — the
/// externally-checkable finish line + its lifecycle. Read-only.
pub fn dispatch_state(store: &WeftStore, story_id: &str) -> WeftStoreResult<Vec<DispatchRecord>> {
    let conn = store.conn()?;
    let mut stmt = conn.prepare(
        "SELECT id, story_id, task, context, constraints, acceptance, state, scope, \
                session_id, created_at, done_at \
         FROM wv_dispatch_records WHERE story_id=?1 ORDER BY id DESC",
    )?;
    let rows = stmt
        .query_map(params![story_id], |r| {
            Ok(DispatchRecord {
                id: r.get(0)?,
                story_id: r.get(1)?,
                task: r.get(2)?,
                context: r.get(3)?,
                constraints: r.get(4)?,
                acceptance: r.get(5)?,
                state: r.get(6)?,
                scope: r.get(7)?,
                session_id: r.get(8)?,
                created_at: r.get(9)?,
                done_at: r.get(10)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests;
