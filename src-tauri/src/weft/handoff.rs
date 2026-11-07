// SPDX-License-Identifier: MIT
//! SW-WV-C4 — the auto-drafted handoff packet (handoff, draft ≠ authority).
//!
//! At task end a worker may report a **handoff packet** — an auto-drafted finding to
//! pass to whoever picks the job up next (e.g. impl → review). The packet is
//! **data, never instructions** (ARCHITECTURE §3.3): a finding to *report*, never an
//! order to *obey*. The schema shape enforces it — there is no "instruction" /
//! "command" column, only a `finding` (what the worker found) and an advisory
//! `to_role` (who it is *for*, not who it commands). Every packet is a **draft**:
//! `is_draft` may only ever be `1` (a `CHECK` pins it), and no verb promotes a
//! handoff to authority. A drafted handoff is a candidate for a human/controller to
//! act on, never a self-issued directive one worker binds another with.
//!
//! This module owns the `wv_handoff_records` transitions (the sanctioned single
//! writer, ARCHITECTURE §1.2 — the whole `src/weft/` tree is the sanctioned home).
//! Every mutation co-commits its `wv_write_log` audit row in the SAME IMMEDIATE
//! transaction via [`super::sqlite::write_audit_public`] (AUDIT-ATOMIC §1.6). Every
//! function is synchronous SQLite on a caller-supplied [`WeftStore`]; the MCP
//! handler + the `signal_done` hook run them on the blocking pool (§1.6).
//!
//! # Ordering with `signal_done` (the `handoff-record` hook)
//!
//! The roster `signal → handoff` transition (A3's [`super::sqlite::wv_worker_handoff`])
//! requires the worker to already be in `signal` — which the FIRST `signal_done` hook
//! (`roster-update`) sets. A worker therefore reports its packet with
//! [`wv_report_handoff`] BEFORE it calls `signal_done` (C7's exit-contract order); the
//! `handoff-record` hook C4 registers into `build_signal_done_hooks` then runs AFTER
//! `roster-update` and, when a packet exists for the completing session, advances the
//! roster to `handoff`. Persisting the packet and flipping the roster status are thus
//! split across the two edges — the packet write is the durable record, the hook is
//! the status advance — so the worker never has to be `signal` before it can draft.

use rusqlite::{params, OptionalExtension};

use super::sqlite::{is_blank, now_iso, wv_auto_append, wv_worker_handoff, WvWriteError};
use super::store::{WeftStore, WeftStoreError, WeftStoreResult};

/// The `wv_handoff_records` table name — a compile-time constant threaded to the
/// audit-write seam (never user input).
const HANDOFF: &str = "wv_handoff_records";

/// One auto-drafted handoff packet as [`handoff_state`] returns it. The packet is
/// data, never instructions: `finding` is what the worker found, `to_role` is who it
/// is *for* (advisory), and `is_draft` is always `true` (draft ≠ authority).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftedHandoff {
    pub id: i64,
    /// The worker/roster key the packet was drafted under.
    pub story_id: String,
    /// The reporting worker's completing session.
    pub session_id: String,
    /// The role the finding is *for* (e.g. `review`) — advisory routing, not a
    /// binding assignment. Empty when the worker names no successor role.
    pub to_role: String,
    /// The auto-drafted finding — what the worker found, to report, never to obey.
    pub finding: String,
    /// Canonical refs the finding points at (a JSON array; `[]` when none).
    pub refs: serde_json::Value,
    /// Always `true` — a handoff is a draft, never authority. Surfaced so a reader
    /// (C5/closure, the Board) can see at a glance the packet carries no directive.
    pub is_draft: bool,
    pub created_at: String,
}

/// Typed refusals from the handoff verbs — distinct from a storage error so the
/// caller maps each to the right MCP outcome (a validation refusal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandoffError {
    /// A blank/whitespace `story_id`, `session_id`, or `finding` — a handoff with no
    /// key to draft under, no completing session to match on, or no finding to report,
    /// is refused (AC-1). `to_role`/`refs` are optional (not every handoff names a
    /// successor or cites a ref).
    MissingRequiredField { field: &'static str },
}

impl std::fmt::Display for HandoffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HandoffError::MissingRequiredField { field } => write!(
                f,
                "handoff {field} must not be blank (a handoff needs a story key and a finding to report)"
            ),
        }
    }
}

impl std::error::Error for HandoffError {}

/// Persist an auto-drafted handoff packet (AC-1, AC-2). Refuses a blank `story_id`
/// or `finding`. The packet is written `is_draft=1` (the schema `CHECK` forbids any
/// other value) — a handoff is a finding to report, never an order to obey, and it
/// never carries authority. `to_role`/`refs` are optional. Returns the AUTOINCREMENT
/// id. Audit row co-committed (AC-4). Mirrors [`super::dispatch::wv_write_dispatch`].
pub fn wv_report_handoff(
    store: &WeftStore,
    story_id: &str,
    session_id: &str,
    to_role: &str,
    finding: &str,
    refs: &[serde_json::Value],
) -> Result<i64, WeftStoreError> {
    if is_blank(story_id) {
        return Err(WeftStoreError::from_handoff(
            HandoffError::MissingRequiredField { field: "story_id" },
        ));
    }
    // `session_id` is required, not optional: the `handoff-record` `signal_done` hook
    // resolves the completing session's real id and matches a packet by
    // `(story_id, session_id)`. A blank session id would store an orphaned packet the
    // hook can never find (the roster would silently stay `signal`). Refuse it at the
    // sanctioned writer so no caller (MCP verb defaulting a missing arg, a future Tauri
    // door) can persist an unmatchable draft.
    if is_blank(session_id) {
        return Err(WeftStoreError::from_handoff(
            HandoffError::MissingRequiredField {
                field: "session_id",
            },
        ));
    }
    if is_blank(finding) {
        return Err(WeftStoreError::from_handoff(
            HandoffError::MissingRequiredField { field: "finding" },
        ));
    }
    let refs_json = serde_json::Value::Array(refs.to_vec()).to_string();
    let now = now_iso();
    let mut conn = store.conn()?;
    // Route the AUTOINCREMENT insert through the sanctioned append helper — it owns the
    // IMMEDIATE tx, co-commits the `wv_write_log` audit row, and returns the minted id
    // (AC-4). `is_draft` is written `1` unconditionally; the schema `CHECK(is_draft=1)`
    // is the belt (draft ≠ authority — no verb ever promotes a handoff).
    let id = wv_auto_append(
        &mut conn,
        "INSERT INTO wv_handoff_records \
         (story_id, session_id, to_role, finding, refs, is_draft, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)",
        params![story_id, session_id, to_role, finding, refs_json, now],
        HANDOFF,
    )?;
    Ok(id)
}

/// True when the completing `session_id` reported at least one handoff packet — the
/// gate the `handoff-record` `signal_done` hook checks before flipping the roster to
/// `handoff`. Read-only.
pub fn handoff_reported_for_session(
    store: &WeftStore,
    story_id: &str,
    session_id: &str,
) -> WeftStoreResult<bool> {
    let conn = store.conn()?;
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM wv_handoff_records \
             WHERE story_id=?1 AND session_id=?2 LIMIT 1",
            params![story_id, session_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

/// The `signal_done` hook core: if the completing session reported a handoff packet,
/// advance its roster row `signal → handoff` (AC-3). Runs AFTER the `roster-update`
/// hook (which set `signal`), so the row is in `signal` by the time this reads it.
/// Returns:
/// - `Ok(true)`  — a packet existed and the roster advanced to `handoff`;
/// - `Ok(false)` — no live worker for the session, no packet reported, or the row was
///   not in `signal` (a non-worker completion, a worker that drafted no handoff, or a
///   worker already advanced) — a graceful skip so the hook never fails a normal
///   `signal_done`.
///
/// A store fault still propagates as `Err` (the registry isolates it behind its own
/// `Result`). An illegal-transition CAS loss is NOT an error here — a worker that
/// reported a packet but is not (or no longer) in `signal` is a graceful skip, not a
/// failed completion.
pub fn handoff_advance_by_session(
    store: &WeftStore,
    session_id: &str,
) -> Result<bool, WeftStoreError> {
    handoff_advance_for_worker(store, session_id, session_id)
}

pub(crate) fn handoff_advance_for_worker(
    store: &WeftStore,
    worker_key: &str,
    session_id: &str,
) -> Result<bool, WeftStoreError> {
    if is_blank(session_id) {
        return Ok(false);
    }
    // Resolve the story this live session belongs to (A2/A3 roster key). Match only
    // `signal` — the roster-update hook has already run, and `handoff`/`deprovision`
    // are terminal (nothing left to advance).
    let conn = store.conn()?;
    let story_id: Option<String> = conn
        .query_row(
            "SELECT name FROM wv_roster WHERE session_id=?1 AND status='signal'",
            params![worker_key],
            |r| r.get(0),
        )
        .optional()?;
    drop(conn);
    let Some(story_id) = story_id else {
        return Ok(false); // no signalled worker for this session — graceful skip
    };
    // Only advance when the worker actually drafted a packet: no packet → the worker
    // skips the optional handoff step and goes straight `signal → deprovision` (A3).
    if !handoff_reported_for_session(store, &story_id, session_id)? {
        return Ok(false);
    }
    match wv_worker_handoff(store, &story_id) {
        Ok(()) => Ok(true),
        // A3 maps an illegal-transition CAS loss (the row was not in `signal` — already
        // advanced, absent, or a concurrent winner) to `WvWriteError::Invalid`. Treat it
        // as a graceful skip, never a failed completion. A genuine store fault
        // (`WvWriteError::Store`) propagates.
        Err(WvWriteError::Invalid(_)) => Ok(false),
        Err(WvWriteError::Store(e)) => Err(e),
    }
}

/// Read the handoff packets a story accumulated, newest first (AC-2) — the drafted
/// findings a reader (C5/closure, the Board) inspects. Read-only.
pub fn handoff_state(store: &WeftStore, story_id: &str) -> WeftStoreResult<Vec<DraftedHandoff>> {
    let conn = store.conn()?;
    let mut stmt = conn.prepare(
        "SELECT id, story_id, session_id, to_role, finding, refs, is_draft, created_at \
         FROM wv_handoff_records WHERE story_id=?1 ORDER BY id DESC",
    )?;
    let rows = stmt
        .query_map(params![story_id], |r| {
            let refs_raw: String = r.get(5)?;
            let is_draft_int: i64 = r.get(6)?;
            Ok(DraftedHandoff {
                id: r.get(0)?,
                story_id: r.get(1)?,
                session_id: r.get(2)?,
                to_role: r.get(3)?,
                finding: r.get(4)?,
                // Stored as a JSON string; fall back to an empty array if a legacy row
                // ever held something unparseable (never on a C4-written row).
                refs: serde_json::from_str(&refs_raw)
                    .unwrap_or_else(|_| serde_json::Value::Array(Vec::new())),
                is_draft: is_draft_int != 0,
                created_at: r.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests;
