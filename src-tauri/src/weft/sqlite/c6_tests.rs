// SPDX-License-Identifier: MIT
//! SW-WV-C6 acceptance tests for the decision-harvest norm: the detect → draft →
//! approve → link loop over `wv_decisions`' harvest columns.
//!
//! Covers AC-1 (schema migrate over a B3-level table), AC-2 (`wv_set_harvest_artifact`
//! state machine + idempotency + guards), AC-3 (human approval closes the loop via the
//! `wv_settle_approval` `harvest:` path + the four guards), AC-4 (overdue-harvest query
//! with all four exclusion values), AC-5 (`wv_mark_harvest_not_required` guard +
//! idempotency), AC-6 (bidirectional link is complete after approval), AC-7 (the new
//! store handlers are synchronous, and the MCP read verb is off-thread).

use chrono::TimeZone;

use super::*;
use crate::weft::store::WeftStore;

fn open_temp() -> (tempfile::TempDir, WeftStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
    (dir, store)
}

/// Insert a decision through the sanctioned handler and return its id. `date` is
/// supplied so the overdue window (AC-4) can be exercised.
fn write_decision(store: &WeftStore, date: &str) -> i64 {
    wv_write_decision(
        store,
        date,
        "a locked decision",
        "SW-WV-C6",
        "never",
        "sam",
        None,
    )
    .unwrap()
}

fn harvest_state(store: &WeftStore, id: i64) -> (String, Option<String>) {
    let conn = store.conn().unwrap();
    conn.query_row(
        "SELECT harvest_status, harvest_artifact FROM wv_decisions WHERE id=?1",
        params![id],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
    )
    .unwrap()
}

fn is_already_approved(e: &WvWriteError) -> bool {
    matches!(e, WvWriteError::Invalid(v) if v.0.contains("already approved"))
}
fn is_no_drafted_artifact(e: &WvWriteError) -> bool {
    matches!(e, WvWriteError::Invalid(v) if v.0.contains("no drafted artifact"))
}
fn is_invalid_kind(e: &WvWriteError) -> bool {
    matches!(e, WvWriteError::Invalid(v) if v.0.contains("file | pr | entity"))
}

// ============================== AC-1: schema migrate ===========================

// AC-1: a pre-existing B3-level `wv_decisions` (no C6 columns) gains
// `harvest_artifact` + `harvest_status` on re-open via WeftStore::open, and a
// pre-existing row reads the `pending` default — no data loss.
#[test]
fn migrate_adds_harvest_columns_over_b3_shape() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("weft.sqlite3");

    // Synthesize the B3-level table inline WITHOUT the C6 columns, insert a row.
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS wv_decisions (\
               id INTEGER PRIMARY KEY AUTOINCREMENT, \
               date TEXT, decision TEXT, constrains TEXT, ends_when TEXT, \
               owner TEXT, source TEXT, status TEXT NOT NULL DEFAULT 'active');",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO wv_decisions (date, decision, constrains, ends_when, owner) \
             VALUES ('2026-01-01', 'legacy', 'SW-WV-B3', 'never', 'sam')",
            [],
        )
        .unwrap();
    } // connection dropped

    // Re-open via the sanctioned migration path.
    let store = WeftStore::open(&path).unwrap();
    let conn = store.conn().unwrap();

    let mut cols: Vec<String> = conn
        .prepare("PRAGMA table_info(wv_decisions)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    cols.sort();
    assert!(
        cols.iter().any(|c| c == "harvest_artifact"),
        "harvest_artifact must exist after migrate: {cols:?}"
    );
    assert!(
        cols.iter().any(|c| c == "harvest_status"),
        "harvest_status must exist after migrate: {cols:?}"
    );

    let (status, artifact): (String, Option<String>) = conn
        .query_row(
            "SELECT harvest_status, harvest_artifact FROM wv_decisions WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "pending", "pre-existing row defaults to pending");
    assert_eq!(artifact, None, "pre-existing row has no artifact yet");
}

// AC-1: a fresh decision written through the sanctioned handler defaults to pending.
#[test]
fn fresh_decision_defaults_to_pending() {
    let (_d, store) = open_temp();
    let id = write_decision(&store, "2026-09-18");
    let (status, artifact) = harvest_state(&store, id);
    assert_eq!(status, "pending");
    assert_eq!(artifact, None);
}

// ============================== AC-2: set artifact =============================

// AC-2: (1) set advances pending→drafted; (2) same-ref re-draft is an idempotent
// no-op leaving exactly one row; (3) different-ref re-draft replaces the ref;
// (4) an approved row refuses with AlreadyApproved; (5) an unknown kind refuses with
// InvalidArtifactKind; (6) the round-trip status stays drafted.
#[test]
fn set_harvest_artifact_state_machine() {
    let (_d, store) = open_temp();
    let id = write_decision(&store, "2026-09-18");

    // (1) pending → drafted
    wv_set_harvest_artifact(&store, id, "docs/adr-001.md", "file").unwrap();
    let (status, artifact) = harvest_state(&store, id);
    assert_eq!(status, "drafted");
    assert_eq!(artifact.as_deref(), Some("docs/adr-001.md"));

    // (2) idempotent same-ref no-op — still exactly one row
    wv_set_harvest_artifact(&store, id, "docs/adr-001.md", "file").unwrap();
    let count: i64 = store
        .conn()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM wv_decisions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "idempotent re-draft must not insert a row");

    // (3) different-ref re-draft is allowed and replaces the ref
    wv_set_harvest_artifact(&store, id, "docs/adr-001-v2.md", "file").unwrap();
    let (status, artifact) = harvest_state(&store, id);
    assert_eq!(status, "drafted");
    assert_eq!(artifact.as_deref(), Some("docs/adr-001-v2.md"));

    // (4) an approved row refuses
    let id2 = write_decision(&store, "2026-09-18");
    store
        .conn()
        .unwrap()
        .execute(
            "UPDATE wv_decisions SET harvest_status='approved', harvest_artifact='x' WHERE id=?1",
            params![id2],
        )
        .unwrap();
    let err = wv_set_harvest_artifact(&store, id2, "docs/other.md", "file").unwrap_err();
    assert!(is_already_approved(&err), "approved row refuses: {err}");

    // (5) unknown kind refuses
    let err = wv_set_harvest_artifact(&store, id, "docs/x.md", "branch").unwrap_err();
    assert!(is_invalid_kind(&err), "unknown kind refuses: {err}");

    // (6) the round-trip status stays drafted (step 3's value)
    let (status, _) = harvest_state(&store, id);
    assert_eq!(status, "drafted");
}

// AC-2: the `pr` and `entity` kinds are also accepted at the boundary.
#[test]
fn set_harvest_artifact_accepts_all_kinds() {
    let (_d, store) = open_temp();
    for kind in ["file", "pr", "entity"] {
        let id = write_decision(&store, "2026-09-18");
        wv_set_harvest_artifact(&store, id, "ref", kind).unwrap();
        assert_eq!(harvest_state(&store, id).0, "drafted");
    }
}

// ============================== AC-3: approval close ==========================

// AC-3 test 1 (happy path): set artifact, write a T3 harvest approval, settle
// ratified — the decision flips to approved AND the approval reads ratified.
#[test]
fn settle_ratified_harvest_closes_the_loop() {
    let (_d, store) = open_temp();
    let id = write_decision(&store, "2026-09-18");
    wv_set_harvest_artifact(&store, id, "docs/adr-001.md", "file").unwrap();

    let (approval_id, _, _) = wv_write_approval(
        &store,
        ApprovalTier::T3,
        false,
        crate::weft::BlastRadius::Low,
        crate::weft::Confidence::High,
        "approve the harvest artifact",
        Some(&format!("harvest:{id}")),
        None,
        Some("sess-1"),
        &NullNotifier,
    )
    .unwrap();

    assert!(wv_settle_approval(&store, approval_id, "ratified").unwrap());

    let (status, _) = harvest_state(&store, id);
    assert_eq!(status, "approved", "harvest closes on ratify");

    let disposition: String = store
        .conn()
        .unwrap()
        .query_row(
            "SELECT disposition FROM wv_approvals WHERE id=?1",
            params![approval_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(disposition, "ratified");
}

// AC-3 test 2 (null-draft guard): approving a decision with no artifact refuses and
// leaves the status pending.
#[test]
fn approve_harvest_refuses_null_artifact() {
    let (_d, store) = open_temp();
    let id = write_decision(&store, "2026-09-18");
    let err = wv_approve_harvest(&store, id, 999).unwrap_err();
    assert!(is_no_drafted_artifact(&err), "null artifact refuses: {err}");
    assert_eq!(harvest_state(&store, id).0, "pending", "status unchanged");
}

// AC-3 test 3 (already-approved guard): approving an already-approved decision refuses.
#[test]
fn approve_harvest_refuses_already_approved() {
    let (_d, store) = open_temp();
    let id = write_decision(&store, "2026-09-18");
    wv_set_harvest_artifact(&store, id, "docs/adr-001.md", "file").unwrap();
    wv_approve_harvest(&store, id, 1).unwrap();
    let err = wv_approve_harvest(&store, id, 1).unwrap_err();
    assert!(is_already_approved(&err), "double-approve refuses: {err}");
    assert_eq!(harvest_state(&store, id).0, "approved", "row unchanged");
}

// AC-3 test 4 (source-parse-failure guard): a `harvest:not-an-integer` approval, when
// settled ratified, returns Err, advances no decision row, and does not panic. The
// whole tx rolls back — the approval stays pending too.
#[test]
fn settle_ratified_corrupt_harvest_source_does_not_advance_any_row() {
    let (_d, store) = open_temp();
    let id = write_decision(&store, "2026-09-18");
    wv_set_harvest_artifact(&store, id, "docs/adr-001.md", "file").unwrap();

    let (approval_id, _, _) = wv_write_approval(
        &store,
        ApprovalTier::T3,
        false,
        crate::weft::BlastRadius::Low,
        crate::weft::Confidence::High,
        "corrupt pointer",
        Some("harvest:not-an-integer"),
        None,
        Some("sess-1"),
        &NullNotifier,
    )
    .unwrap();

    let res = wv_settle_approval(&store, approval_id, "ratified");
    assert!(res.is_err(), "corrupt harvest source is an Err: {res:?}");

    // No decision row advanced — every row is still drafted/pending, not approved.
    let approved_count: i64 = store
        .conn()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM wv_decisions WHERE harvest_status='approved'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(approved_count, 0, "no decision advanced to approved");
    assert_eq!(harvest_state(&store, id).0, "drafted", "decision unchanged");

    // The tx rolled back — the approval is still pending (settle undone).
    let disposition: String = store
        .conn()
        .unwrap()
        .query_row(
            "SELECT disposition FROM wv_approvals WHERE id=?1",
            params![approval_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(disposition, "pending", "corrupt-source settle rolled back");
}

// AC-3: a non-harvest approval settles normally without touching any decision.
#[test]
fn settle_ratified_non_harvest_source_is_a_plain_approval() {
    let (_d, store) = open_temp();
    let (approval_id, _, _) = wv_write_approval(
        &store,
        ApprovalTier::T3,
        false,
        crate::weft::BlastRadius::Low,
        crate::weft::Confidence::High,
        "some other claim",
        Some("test-run:foo"),
        None,
        Some("sess-1"),
        &NullNotifier,
    )
    .unwrap();
    assert!(wv_settle_approval(&store, approval_id, "ratified").unwrap());
}

// ============================== AC-4: overdue query ==========================

// AC-4: five decisions covering every exclusion value; only the pending+overdue+active
// one (row b) is returned.
#[test]
fn list_overdue_harvests_excludes_all_non_pending_values() {
    let (_d, store) = open_temp();
    let now = chrono::Utc::now().timestamp();
    let one_hour_ago = chrono::Utc
        .timestamp_opt(now - 3600, 0)
        .unwrap()
        .to_rfc3339();
    let twenty_five_hours_ago = chrono::Utc
        .timestamp_opt(now - 25 * 3600, 0)
        .unwrap()
        .to_rfc3339();

    // (a) now-1h, pending — not overdue
    write_decision(&store, &one_hour_ago);
    // (b) now-25h, pending — overdue, MUST be returned
    let id_b = write_decision(&store, &twenty_five_hours_ago);
    // (c) now-25h, drafted — excluded
    let id_c = write_decision(&store, &twenty_five_hours_ago);
    wv_set_harvest_artifact(&store, id_c, "r", "file").unwrap();
    // (d) now-25h, approved — excluded
    let id_d = write_decision(&store, &twenty_five_hours_ago);
    wv_set_harvest_artifact(&store, id_d, "r", "file").unwrap();
    wv_approve_harvest(&store, id_d, 1).unwrap();
    // (e) now-25h, not-required — excluded
    let id_e = write_decision(&store, &twenty_five_hours_ago);
    wv_mark_harvest_not_required(&store, id_e).unwrap();

    let overdue = wv_list_overdue_harvests(&store, now).unwrap();
    assert_eq!(overdue.len(), 1, "exactly one overdue row: {overdue:?}");
    assert_eq!(overdue[0].id, id_b, "the pending+overdue+active row");
}

// AC-4: a superseded (status<>'active') pending+overdue decision is excluded.
#[test]
fn list_overdue_harvests_excludes_superseded() {
    let (_d, store) = open_temp();
    let now = chrono::Utc::now().timestamp();
    let overdue_date = chrono::Utc
        .timestamp_opt(now - 25 * 3600, 0)
        .unwrap()
        .to_rfc3339();
    let id = write_decision(&store, &overdue_date);
    wv_retire_decision(&store, id).unwrap(); // status → superseded
    assert!(
        wv_list_overdue_harvests(&store, now).unwrap().is_empty(),
        "a superseded decision is not an overdue harvest"
    );
}

// AC-4: the query is a pure read — no side effects (no audit rows written).
#[test]
fn list_overdue_harvests_is_side_effect_free() {
    let (_d, store) = open_temp();
    let now = chrono::Utc::now().timestamp();
    let overdue_date = chrono::Utc
        .timestamp_opt(now - 25 * 3600, 0)
        .unwrap()
        .to_rfc3339();
    write_decision(&store, &overdue_date);
    let audit_before: i64 = store
        .conn()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM wv_write_log", [], |r| r.get(0))
        .unwrap();
    let _ = wv_list_overdue_harvests(&store, now).unwrap();
    let audit_after: i64 = store
        .conn()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM wv_write_log", [], |r| r.get(0))
        .unwrap();
    assert_eq!(audit_before, audit_after, "the query writes no audit row");
}

// ============================== AC-5: not-required ===========================

// AC-5: (1) an approved decision refuses; (2) a fresh decision transitions to
// not-required; (3) a second call is an idempotent no-op.
#[test]
fn mark_harvest_not_required_guard_and_idempotency() {
    let (_d, store) = open_temp();

    // (1) approved refuses
    let id_a = write_decision(&store, "2026-09-18");
    wv_set_harvest_artifact(&store, id_a, "r", "file").unwrap();
    wv_approve_harvest(&store, id_a, 1).unwrap();
    let err = wv_mark_harvest_not_required(&store, id_a).unwrap_err();
    assert!(is_already_approved(&err), "approved refuses: {err}");
    assert_eq!(harvest_state(&store, id_a).0, "approved", "unchanged");

    // (2) pending → not-required
    let id_b = write_decision(&store, "2026-09-18");
    wv_mark_harvest_not_required(&store, id_b).unwrap();
    assert_eq!(harvest_state(&store, id_b).0, "not-required");

    // (3) idempotent second call
    wv_mark_harvest_not_required(&store, id_b).unwrap();
    assert_eq!(harvest_state(&store, id_b).0, "not-required");
}

// ============================== AC-6: bidirectional link =====================

// AC-6: after the full happy-path sequence, both directions are complete and
// machine-traversable via the `harvest:<id>` source string (no JOIN).
#[test]
fn harvest_bidirectional_link_is_complete() {
    let (_d, store) = open_temp();
    let now = chrono::Utc::now().timestamp();
    let overdue_date = chrono::Utc
        .timestamp_opt(now - 25 * 3600, 0)
        .unwrap()
        .to_rfc3339();

    // (1) insert decision
    let id = write_decision(&store, &overdue_date);
    // (2) set artifact
    wv_set_harvest_artifact(&store, id, "docs/decisions/2026-09-18-foo.md", "file").unwrap();
    // (3) write T3 approval with source=harvest:<id>
    let (approval_id, _, _) = wv_write_approval(
        &store,
        ApprovalTier::T3,
        false,
        crate::weft::BlastRadius::Low,
        crate::weft::Confidence::High,
        "approve the harvest",
        Some(&format!("harvest:{id}")),
        None,
        Some("sess-1"),
        &NullNotifier,
    )
    .unwrap();
    // (4) settle ratified
    assert!(wv_settle_approval(&store, approval_id, "ratified").unwrap());

    // (5) decision direction
    let (status, artifact) = harvest_state(&store, id);
    assert_eq!(status, "approved");
    assert_eq!(
        artifact.as_deref(),
        Some("docs/decisions/2026-09-18-foo.md")
    );

    // (6) approval direction — disposition + the source backlink
    let (disposition, source): (String, String) = store
        .conn()
        .unwrap()
        .query_row(
            "SELECT disposition, source FROM wv_approvals WHERE id=?1",
            params![approval_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(disposition, "ratified");
    assert_eq!(source, format!("harvest:{id}"));
}

// ============================== AC-7: off-thread discipline ===================

// AC-7: blocking-pool coverage for C6 new verbs — the store handlers are synchronous
// (never `async fn`), and the MCP read verb goes through `await_blocking`. A grep-based
// contract check alongside B1's blocking-pool discipline.
// AC-7: blocking-pool coverage for C6 new verbs
#[test]
fn blocking_pool_c6_new_verbs_are_not_async_store_methods() {
    let store_src =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/weft/sqlite.rs"))
            .expect("weft/sqlite.rs must be readable");
    for name in [
        "async fn wv_set_harvest_artifact",
        "async fn wv_approve_harvest",
        "async fn wv_mark_harvest_not_required",
        "async fn wv_list_overdue_harvests",
    ] {
        assert!(
            !store_src.contains(name),
            "store handler must not be async: {name}"
        );
    }

    let handler_src =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/mcp/handler.rs"))
            .expect("mcp/handler.rs must be readable");
    // The `wv_list_overdue_harvests` MCP handler must run its store call off-thread.
    let idx = handler_src
        .find("mcp.wv_list_overdue_harvests")
        .expect("the verb's await_blocking label must appear in handler.rs");
    let window_start = idx.saturating_sub(200);
    assert!(
        handler_src[window_start..idx].contains("await_blocking"),
        "wv_list_overdue_harvests MCP handler must use await_blocking"
    );
}

// AC-3 unit: the harvest source-parse contract — a bare integer suffix parses; a
// non-integer suffix is a corrupt pointer; a non-harvest source is None.
#[test]
fn parse_harvest_source_contract() {
    assert_eq!(parse_harvest_source("harvest:42"), Some(Ok(42)));
    assert_eq!(parse_harvest_source("harvest:nope"), Some(Err(())));
    assert_eq!(parse_harvest_source("test-run:x"), None);
    assert_eq!(parse_harvest_source("no-colon"), None);
}
