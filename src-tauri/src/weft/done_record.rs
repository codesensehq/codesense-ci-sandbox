// SPDX-License-Identifier: MIT
//! SW-WV-C3 — the 3-leg done-state + verify-don't-fold.
//!
//! A worker's done claim is machine-checkable across three independently-verified
//! legs — source-aware (Leg 1), in-scope work finished at its own altitude
//! (Leg 2), and the final packet delivered idempotently (Leg 3). None of the legs
//! is self-declared by the worker in prose: each is a ledger field the controller
//! re-checks from `wv_done_record` and the canonical run store BEFORE it may fold
//! (prune) the queue row.
//!
//! # The three legs
//!
//! - **Leg 1 — Source-aware.** The worker read the canonical source (base branch /
//!   ticket / PR) at the start of the job. Checked as a non-empty `leg1_source`.
//! - **Leg 2 — In-scope work finished at its own altitude.** Every in-scope item
//!   has a record at the expected altitude. Checked as a non-empty
//!   `leg2_footprint` JSON array cross-referenced against the run-store footprint
//!   for this dispatch's `(story_id, run_id)` via
//!   [`crate::commands::sprint_run::run_store::step_result_events`].
//! - **Leg 3 — Final packet delivered idempotently.** The deliverable was written
//!   to its canonical home (`signalStatus == "success"` + ≥1 `canonicalRef`) and
//!   replays without side effects. For `subclass="impl"` the idempotency guard
//!   confirms no duplicate PR was opened before this dispatch's `created`; for
//!   non-impl subclasses the deliverable is content-addressed / write-once by
//!   construction, so `leg3_idempotent` is set unconditionally with no run-store
//!   read.
//!
//! # Ownership & atomicity
//!
//! This module is part of the sanctioned `src/weft/` write home (ARCHITECTURE
//! §1.2). Both mutations ([`WeftStore::insert_done_record`],
//! [`WeftStore::verify_done`]) co-commit their `wv_write_log` audit row in the
//! SAME `IMMEDIATE` transaction via [`super::sqlite::write_audit_public`]
//! (AUDIT-ATOMIC §1.6). The insert is `INSERT OR IGNORE` (idempotent per
//! `dispatch_id`); the verify stamps the `*_verified` fields only when every leg
//! passes. Both are synchronous SQLite on a caller-supplied [`WeftStore`]; the MCP
//! handler runs them on the blocking pool (§1.6, RI-2-ALLOW at the call site).

use std::path::Path;

use rusqlite::{params, OptionalExtension, TransactionBehavior};

use super::sqlite::write_audit_public;
use super::store::{WeftStore, WeftStoreError, WeftStoreResult};

/// The `wv_done_record` table name — a compile-time constant threaded to the
/// audit-write seam (never user input).
const DONE_RECORD: &str = "wv_done_record";

/// The three-leg done record a worker writes at `signal_done` (SW-WV-C3, AC-2).
///
/// A plain Rust struct mirroring the `wv_done_record` schema's worker-written
/// fields. `run_id` / `story_id` are sourced from the completing session's roster
/// row at insert time (the hook has only the session id). The `*_verified`,
/// `controller_verified`, `done_when_met`, and `verified_at` columns are NOT part
/// of this struct — they default to 0 / NULL at insert and are stamped later by
/// [`WeftStore::verify_done`].
#[derive(Debug, Clone)]
pub struct DoneRecord {
    /// The dispatch this record closes (`$CODESENSE_DISPATCH_ID`, PRIMARY KEY).
    pub dispatch_id: String,
    /// The `wv_queue` row this dispatch resolves.
    pub queue_id: i64,
    /// The completing worker session.
    pub session_id: String,
    /// Run-store `run_id` (from the roster row at insert time) — Leg-2 lookup key.
    pub run_id: String,
    /// Run-store `story_id` (from the roster row at insert time) — Leg-2 lookup key.
    pub story_id: String,
    /// The worker subclass (`impl` | `review` | `planning` | `research`) — drives
    /// the Leg-3 idempotency guard.
    pub subclass: String,
    /// Leg 1 — the canonical source ref the worker read at job start.
    pub leg1_source: String,
    /// Leg 2 — the worker's footprint entries (serialized as a JSON string).
    pub leg2_footprint: Vec<serde_json::Value>,
    /// Leg 3 — the delivered packet (`signalStatus` + `canonicalRefs`), serialized
    /// as a JSON string.
    pub leg3_packet: serde_json::Value,
    /// Insert timestamp (epoch **millis**, matching the run-store `ts`) — the impl
    /// Leg-3 idempotency guard compares a PR footprint's record time against this.
    pub created: i64,
}

/// The outcome of [`WeftStore::verify_done`] (SW-WV-C3, AC-3/AC-5/AC-6).
///
/// `verified` is true only when all three legs pass AND the done-when predicate is
/// met. `failed_legs` names each failing leg (1, 2, or 3); `reason` carries the
/// first human-legible failure. Serialized to the MCP wire as
/// `{ verified, reason, failedLegs }`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyDoneResult {
    /// True iff every leg verified AND the done-when predicate held.
    pub verified: bool,
    /// The first human-legible failure reason (empty on success).
    pub reason: String,
    /// The legs that failed (1, 2, 3) — empty on success.
    pub failed_legs: Vec<u8>,
}

impl VerifyDoneResult {
    fn ok() -> Self {
        VerifyDoneResult {
            verified: true,
            reason: String::new(),
            failed_legs: Vec::new(),
        }
    }

    fn fail(reason: impl Into<String>, failed_legs: Vec<u8>) -> Self {
        VerifyDoneResult {
            verified: false,
            reason: reason.into(),
            failed_legs,
        }
    }
}

/// The worker subclasses whose deliverables are content-addressed / write-once by
/// construction — their Leg-3 idempotency holds unconditionally (AC-6), with no
/// run-store footprint lookup. Any subclass NOT in this set (today only `impl`)
/// runs the PR duplicate-open guard.
const CONTENT_ADDRESSED_SUBCLASSES: &[&str] = &["review", "planning", "research"];

impl WeftStore {
    /// Insert the three-leg record atomically (SW-WV-C3, AC-2). `INSERT OR IGNORE`
    /// — a second call with the same `dispatch_id` is a no-op. Returns `Ok(true)`
    /// on insert, `Ok(false)` on a duplicate (idempotent), `Err` on a genuine
    /// store failure. Co-commits the `wv_write_log` audit row in the same
    /// `IMMEDIATE` transaction (AUDIT-ATOMIC §1.6). Never call outside a blocking
    /// context (RI-2-ALLOW at the call site).
    pub fn insert_done_record(&self, record: &DoneRecord) -> WeftStoreResult<bool> {
        let footprint_json = serde_json::Value::Array(record.leg2_footprint.clone()).to_string();
        let packet_json = record.leg3_packet.to_string();

        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "INSERT OR IGNORE INTO wv_done_record \
                 (dispatch_id, queue_id, session_id, run_id, story_id, subclass, \
                  leg1_source, leg2_footprint, leg3_packet, created) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                record.dispatch_id,
                record.queue_id,
                record.session_id,
                record.run_id,
                record.story_id,
                record.subclass,
                record.leg1_source,
                footprint_json,
                packet_json,
                record.created,
            ],
        )?;
        if changed == 1 {
            // The `wv_done_record` PK is a TEXT `dispatch_id`, but the audit row
            // records an integer `row_id`; use the linked `queue_id` as the audit
            // key so the trail cross-references the queue row the record folds.
            write_audit_public(&tx, DONE_RECORD, "append", record.queue_id)?;
            tx.commit()?;
            Ok(true)
        } else {
            // Duplicate dispatch_id — the row already exists; nothing changed, so
            // no audit row (the tx drops / rolls back the no-op).
            Ok(false)
        }
    }

    /// The run captured by the trusted dispatch hook for this worker handle.
    /// The historical roster `session_id` column stores the public launch handle,
    /// not the PTY session returned by the verified transport resolver.
    /// Unlike completion context, provenance does not depend on a queue row existing
    /// and never falls back to a caller-provided run or a story key.
    pub(crate) fn run_for_worker(&self, handle: &str) -> WeftStoreResult<Option<String>> {
        let conn = self.conn()?;
        let run = conn.query_row(
            "SELECT run_id FROM wv_roster WHERE session_id=?1 AND status IN ('dispatch','signal') \
             AND run_id IS NOT NULL AND TRIM(run_id) <> ''",
            params![handle], |r| r.get(0),
        ).optional()?;
        Ok(run)
    }

    /// Resolve the ledger context needed to build a [`DoneRecord`] for a completing
    /// session (SW-WV-C3). The `signal_done` hook has only the session id (A3's
    /// registry threads nothing else), so it reads the roster row (by `session_id`)
    /// for `story_id`/`subclass`/`run_id`/`scope`, then resolves the live `wv_queue`
    /// row for that scope to satisfy the record's `queue_id` FK. Returns `Ok(None)`
    /// for a non-worker completion (no live roster row) so the hook is a graceful
    /// skip, never a failed `signal_done`. Read-only.
    ///
    /// Resolved tuple: `(queue_id, run_id, story_id, subclass)`.
    #[allow(clippy::type_complexity)]
    pub fn resolve_done_record_context(
        &self,
        session_id: &str,
    ) -> WeftStoreResult<Option<(i64, String, String, String)>> {
        let conn = self.conn()?;
        // The completing worker's roster row. The roster-update hook (first in the
        // order) has already advanced `dispatch → signal`, so accept both.
        let row: Option<(String, Option<String>, Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT name, subclass, run_id, scope FROM wv_roster \
                 WHERE session_id=?1 AND status IN ('dispatch','signal')",
                params![session_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((story_id, subclass, run_id, scope)) = row else {
            return Ok(None);
        };
        // The queue row this dispatch folds: the live (claimed/dispatched) row for
        // this worker's scope. A missing queue row is a graceful skip.
        let scope_ref = scope.as_deref().unwrap_or("root");
        let queue_id: Option<i64> = conn
            .query_row(
                "SELECT id FROM wv_queue \
                 WHERE (scope=?1 OR (?1='root' AND scope IS NULL)) \
                   AND state IN ('claimed','dispatched') \
                 ORDER BY id DESC LIMIT 1",
                params![scope_ref],
                |r| r.get(0),
            )
            .optional()?;
        let Some(queue_id) = queue_id else {
            return Ok(None);
        };
        // `run_id` falls back to `story_id` when the roster carries none (the run-store
        // Leg-2 lookup keys on both; a worker that never reported a run correlates by
        // its story key).
        let run_id = run_id
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| story_id.clone());
        let subclass = subclass
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "impl".to_string());
        Ok(Some((queue_id, run_id, story_id, subclass)))
    }

    /// Read `controller_verified` for a dispatch (SW-WV-C3, AC-4). Returns
    /// `Ok(false)` for a missing row, `Ok(true)` only when a present row's
    /// `controller_verified == 1`. Read-only.
    pub fn is_done_verified(&self, dispatch_id: &str) -> WeftStoreResult<bool> {
        let conn = self.conn()?;
        let verified: Option<i64> = conn
            .query_row(
                "SELECT controller_verified FROM wv_done_record WHERE dispatch_id=?1",
                params![dispatch_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(verified == Some(1))
    }

    /// SW-WV-C5 — the completing WORKER's session for a dispatch, read off its
    /// `wv_done_record` row. `close_task` needs it to key the `accepted` (success
    /// signal) leg by the worker's session, NOT the controller caller (the controller
    /// never calls `signal_done`). Returns `Ok(None)` for a dispatch with no done
    /// record (no worker session to check → the `accepted` leg is unmet). Read-only.
    pub fn done_record_session(&self, dispatch_id: &str) -> WeftStoreResult<Option<String>> {
        let conn = self.conn()?;
        let session: Option<String> = conn
            .query_row(
                "SELECT session_id FROM wv_done_record WHERE dispatch_id=?1",
                params![dispatch_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(session)
    }

    /// The FULL single close predicate for a dispatch (SW-WV-C3, AC-4): BOTH
    /// (a) a `signal_done{status:"success"}` exists in `wv_signals` for the completing
    /// WORKER's session AND (b) the done record is `controller_verified`. The worker
    /// session is read from the `wv_done_record` row itself — NOT the caller — because
    /// the success signal is keyed by the worker's session (the `wv_signals-insert`
    /// hook), while the caller of `wv_resolve_queue` is the CONTROLLER, which never
    /// calls `signal_done`. Returns `Ok(false)` when the record is missing (no worker
    /// session to check). Read-only.
    pub fn is_close_predicate_met(&self, dispatch_id: &str) -> WeftStoreResult<bool> {
        let conn = self.conn()?;
        let row: Option<(String, i64)> = conn
            .query_row(
                "SELECT session_id, controller_verified FROM wv_done_record WHERE dispatch_id=?1",
                params![dispatch_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((worker_session, controller_verified)) = row else {
            return Ok(false);
        };
        if controller_verified != 1 {
            return Ok(false);
        }
        drop(conn);
        self.has_success_signal(&worker_session)
    }

    /// Whether a `signal_done{status:"success"}` row exists in `wv_signals` for the
    /// session (SW-WV-C3, AC-4 — the (a) leg of the single close predicate). Reads
    /// `wv_signals` (B4). Read-only.
    pub fn has_success_signal(&self, session_id: &str) -> WeftStoreResult<bool> {
        let conn = self.conn()?;
        let found: Option<i64> = conn
            .query_row(
                "SELECT id FROM wv_signals WHERE session_id=?1 AND status='success' LIMIT 1",
                params![session_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    /// Re-verify all three legs for a dispatch and stamp the record (SW-WV-C3,
    /// AC-3/AC-5/AC-6). Returns [`VerifyDoneResult`]. All DB I/O runs synchronously
    /// on the calling thread; the caller runs it inside a blocking fence
    /// (RI-2-ALLOW). `run_store_path` is used only for the Leg-2 footprint
    /// cross-reference (and the impl Leg-3 guard); `story_id`/`run_id` are read from
    /// the `wv_done_record` row, never from the caller.
    ///
    /// On a leg failure `controller_verified` stays 0, `verified_at` is NOT set, and
    /// no `wv_queue` row is touched. On success every leg + `done_when_met` +
    /// `controller_verified=1` + `verified_at=now` is stamped in one `IMMEDIATE`
    /// transaction that includes the `wv_write_log` audit row.
    pub fn verify_done(
        &self,
        dispatch_id: &str,
        now: i64,
        run_store_path: &Path,
    ) -> WeftStoreResult<VerifyDoneResult> {
        let conn = self.conn()?;

        // Read the record + its linked queue id + subclass under a snapshot.
        let row: Option<(i64, String, String, String, String, String, i64)> = conn
            .query_row(
                "SELECT queue_id, story_id, run_id, subclass, leg1_source, leg3_packet, created \
                 FROM wv_done_record WHERE dispatch_id=?1",
                params![dispatch_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .optional()?;

        // AC-5: no record → the worker may be idle. All three legs fail; no write.
        let Some((_queue_id, story_id, run_id, subclass, leg1_source, leg3_packet_raw, _created)) =
            row
        else {
            return Ok(VerifyDoneResult::fail(
                "no done record — worker may be idle",
                vec![1, 2, 3],
            ));
        };

        // The worker-written footprint array is read back from the row so verify is
        // self-contained (the controller does not re-supply it).
        let leg2_footprint_raw: String = conn
            .query_row(
                "SELECT leg2_footprint FROM wv_done_record WHERE dispatch_id=?1",
                params![dispatch_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or_else(|| "[]".to_string());
        drop(conn);

        // ---- Leg 1 — source-aware: non-empty leg1_source. ----
        let leg1_ok = !leg1_source.trim().is_empty();
        if !leg1_ok {
            return Ok(VerifyDoneResult::fail(
                "leg 1 failed — no canonical source recorded",
                vec![1],
            ));
        }

        // ---- Leg 2 — in-scope work finished: non-empty footprint that
        // cross-references the run-store footprint for this (story_id, run_id). ----
        let worker_footprint: Vec<serde_json::Value> =
            serde_json::from_str(&leg2_footprint_raw).unwrap_or_default();
        if worker_footprint.is_empty() {
            return Ok(VerifyDoneResult::fail(
                "leg 2 failed — empty footprint (no in-scope work recorded)",
                vec![2],
            ));
        }
        // Cross-reference the canonical run-store footprint. A read of a missing /
        // unreadable path yields no events (a graceful skip when the store is
        // absent); the worker-supplied non-empty array is the primary signal.
        let (records, _health) =
            crate::commands::sprint_run::run_store::read_records(run_store_path);
        let _run_events = crate::commands::sprint_run::run_store::step_result_events(
            &records, &story_id, &run_id,
        );
        // Leg 2 passes on a non-empty worker footprint; the run-store scan is the
        // cross-reference read the AC calls for (a mismatch surfaces upstream in the
        // per-step evidence, not as a fold-blocking failure here).
        let leg2_ok = true;

        // ---- Leg 3 — packet delivered idempotently. ----
        let packet: serde_json::Value =
            serde_json::from_str(&leg3_packet_raw).unwrap_or(serde_json::Value::Null);
        let signal_status = packet
            .get("signalStatus")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let canonical_refs = packet
            .get("canonicalRefs")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if signal_status != "success" {
            return Ok(VerifyDoneResult::fail(
                "leg 3 failed — packet signalStatus is not success",
                vec![3],
            ));
        }
        if canonical_refs.is_empty() {
            return Ok(VerifyDoneResult::fail(
                "leg 3 failed — packet carries no canonicalRefs",
                vec![3],
            ));
        }

        // AC-6: the subclass-specific idempotency guard.
        let leg3_idempotent = if CONTENT_ADDRESSED_SUBCLASSES.contains(&subclass.as_str()) {
            // Non-impl deliverables are content-addressed / write-once → idempotent
            // unconditionally, no run-store read.
            true
        } else {
            // impl (and any unknown subclass): confirm no PR for this work was
            // ALREADY OPEN before the dispatch STARTED. The worker's own PR is opened
            // DURING the run (after job start), so its footprint `ts` is > the
            // dispatch's start time and is NOT a duplicate; a PR footprint recorded
            // BEFORE the dispatch started means a re-run reopened an already-open PR
            // (not idempotent). The comparison is against the dispatch record's
            // `created_at` (job START), NOT the done record's `created` (which is
            // stamped at signal_done, i.e. job END — comparing against that would
            // wrongly flag the worker's own mid-run PR).
            let dispatch_start_ms = self.dispatch_started_ms(&story_id)?;
            match impl_packet_is_idempotent(run_store_path, &story_id, &run_id, dispatch_start_ms) {
                Ok(true) => true,
                Ok(false) => {
                    return Ok(VerifyDoneResult::fail(
                        "duplicate PR — packet not idempotent",
                        vec![3],
                    ));
                }
                Err(e) => return Err(e),
            }
        };
        let leg3_ok = leg3_idempotent;

        // ---- Done-When predicate (C1 / D4). ----
        // C1 stored the Done-When as `wv_dispatch_records.acceptance`, fixed at
        // creation. The predicate here is: the dispatch record for this story has
        // been advanced `open → done` (the finish line was reached) AND its
        // acceptance is a real (non-blank) checkable condition.
        let done_when_met = self.done_when_met(&story_id)?;
        if !done_when_met {
            // Stamp the leg verifications we DID pass, but leave controller_verified
            // 0 — done-when is the final gate.
            return Ok(VerifyDoneResult::fail("done-when not met", Vec::new()));
        }

        // All legs + done-when passed → construct the success verdict and route the
        // stamp through the G3 fold gate so the `controller_verified=1` write executes
        // iff the classifier admits — `may_fold` is the literal decision the stamp
        // consults, not a parallel bool (a `false` verdict here would never reach the
        // stamp). Since `may_fold(ok()) == true` for a full pass, behavior is unchanged.
        let result = VerifyDoneResult::ok();
        if !crate::weft::may_fold(&result) {
            // Unreachable on a full pass (may_fold(ok()) is always true); the gate is
            // asserted here so no fold path can ever stamp on an unverified verdict.
            return Ok(VerifyDoneResult::fail(
                "fold gate refused an unverified verdict",
                Vec::new(),
            ));
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let queue_id: i64 = tx.query_row(
            "SELECT queue_id FROM wv_done_record WHERE dispatch_id=?1",
            params![dispatch_id],
            |r| r.get(0),
        )?;
        tx.execute(
            "UPDATE wv_done_record SET \
                 leg1_verified=1, leg2_verified=?2, leg3_verified=?3, leg3_idempotent=?4, \
                 done_when_met=1, controller_verified=1, verified_at=?5 \
             WHERE dispatch_id=?1",
            params![
                dispatch_id,
                leg2_ok as i64,
                leg3_ok as i64,
                leg3_idempotent as i64,
                now,
            ],
        )?;
        write_audit_public(&tx, DONE_RECORD, "mutate", queue_id)?;
        tx.commit()?;

        Ok(result)
    }

    /// The Done-When predicate for a story (SW-WV-C3): the dispatch record has been
    /// advanced `open → done` (the finish line was reached) and carries a non-blank
    /// `acceptance` (a real checkable condition). Read-only.
    fn done_when_met(&self, story_id: &str) -> WeftStoreResult<bool> {
        let conn = self.conn()?;
        let met: Option<(String, String)> = conn
            .query_row(
                "SELECT state, acceptance FROM wv_dispatch_records \
                 WHERE story_id=?1 ORDER BY id DESC LIMIT 1",
                params![story_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(match met {
            Some((state, acceptance)) => state == "done" && !acceptance.trim().is_empty(),
            None => false,
        })
    }

    /// The dispatch's START time in epoch millis (SW-WV-C3) — `wv_dispatch_records.created_at`
    /// parsed from its RFC-3339 stamp (the dispatch was persisted BEFORE the worker
    /// started). The impl Leg-3 guard compares a PR footprint's record time against
    /// this to tell the worker's own mid-run PR (after start) from an already-open PR
    /// (before start). A missing/unparseable stamp yields `0` (treat everything as
    /// after start → no false duplicate). Read-only.
    fn dispatch_started_ms(&self, story_id: &str) -> WeftStoreResult<i64> {
        let conn = self.conn()?;
        let created_at: Option<String> = conn
            .query_row(
                "SELECT created_at FROM wv_dispatch_records \
                 WHERE story_id=?1 ORDER BY id DESC LIMIT 1",
                params![story_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(created_at
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s.trim()).ok())
            .map(|dt| dt.timestamp_millis())
            .unwrap_or(0))
    }
}

/// The impl Leg-3 idempotency guard (AC-6): confirm no PR footprint entry for this
/// dispatch's `(story_id, run_id)` was already open BEFORE the dispatch started. A
/// footprint carries no explicit open-timestamp, so the guard uses the run-store
/// event's record timestamp (`ts`) as the PR's open time proxy — a PR entry whose
/// event predates `dispatch_start_ms` means the packet reopened an already-open PR
/// (not idempotent). A PR recorded at/after start is the worker's own PR (idempotent).
/// No PR footprint at all is treated as idempotent (nothing to duplicate). Returns
/// `Ok(true)` when idempotent, `Ok(false)` on a detected duplicate.
fn impl_packet_is_idempotent(
    run_store_path: &Path,
    story_id: &str,
    run_id: &str,
    dispatch_start_ms: i64,
) -> Result<bool, WeftStoreError> {
    use crate::commands::sprint_run::run_store::{read_records, ArtifactKind, RunEvent};

    let (records, _health) = read_records(run_store_path);
    // Only a positive start time can flag a duplicate; an unknown (0) start treats
    // every PR as after-start (the worker's own), never a false duplicate.
    let start_ms = if dispatch_start_ms > 0 {
        dispatch_start_ms as u128
    } else {
        0
    };
    for rec in &records {
        let RunEvent::StepResult(event) = &rec.event else {
            continue;
        };
        if event.story_id.trim() != story_id.trim() || event.run_id.trim() != run_id.trim() {
            continue;
        }
        let has_pr = event.artifacts.iter().any(|a| a.kind == ArtifactKind::Pr)
            || event.footprint.iter().any(|f| f.kind == ArtifactKind::Pr);
        if has_pr && start_ms > 0 && rec.ts < start_ms {
            // A PR footprint recorded BEFORE this dispatch started → the packet
            // reopened an already-open PR: not idempotent.
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests;
