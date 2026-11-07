// SPDX-License-Identifier: MIT
//! SW-WV-C4 acceptance tests for the auto-drafted handoff packet.
//!
//! Covers AC-1 (a packet needs a story key + a finding; `to_role`/`refs` optional;
//! `is_draft` is always 1 — draft ≠ authority, the schema `CHECK` pins it), AC-2
//! (the packet is readable data, newest-first), AC-3 (`handoff_advance_by_session`
//! resolves the roster key and advances `signal → handoff` only when a packet exists
//! — the hook's core; the wiring into `build_signal_done_hooks` is code-review-verified
//! per DW-C4-1), AC-4 (audit co-commit). The two-seam registry cross-check +
//! `EXPECTED_MCP_TOOLS` live in `mcp/handler.rs` + `commands/interactive_gate.rs`
//! alongside the verb.

use rusqlite::params;

use super::*;
use crate::weft::store::WeftStore;

fn open_temp() -> (tempfile::TempDir, WeftStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
    (dir, store)
}

/// Seed a live worker roster row driven to `signal` (the state the handoff hook
/// requires), via the sanctioned A2/A3 handlers so the row matches production.
fn seed_signalled_worker(store: &WeftStore, story_id: &str, session_id: &str) {
    crate::weft::wv_provision_worker_row(store, story_id, "impl", "root").unwrap();
    let r = crate::weft::wv_dispatch_worker_row(store, story_id, session_id, None).unwrap();
    assert_eq!(r, crate::weft::MutateResult::Updated);
    crate::weft::wv_worker_signal(store, story_id).unwrap();
}

fn roster_status(store: &WeftStore, story_id: &str) -> Option<String> {
    let conn = store.conn().unwrap();
    conn.query_row(
        "SELECT status FROM wv_roster WHERE name=?1",
        params![story_id],
        |r| r.get(0),
    )
    .ok()
}

fn audit_ops_for(store: &WeftStore) -> Vec<(String, String)> {
    let conn = store.conn().unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT operation, tbl FROM wv_write_log WHERE tbl='wv_handoff_records' ORDER BY id",
        )
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

// ---- AC-1: a packet needs a story key + a finding; the rest is optional. ----

#[test]
fn report_handoff_persists_the_drafted_packet() {
    let (_d, store) = open_temp();
    let refs = vec![serde_json::json!("PR#42")];
    let id =
        wv_report_handoff(&store, "SW-X-1", "sess-1", "review", "found a race", &refs).unwrap();
    assert!(id > 0);

    let rows = handoff_state(&store, "SW-X-1").unwrap();
    assert_eq!(rows.len(), 1);
    let p = &rows[0];
    assert_eq!(p.story_id, "SW-X-1");
    assert_eq!(p.session_id, "sess-1");
    assert_eq!(p.to_role, "review");
    assert_eq!(p.finding, "found a race");
    assert_eq!(p.refs, serde_json::json!(["PR#42"]));
    // AC-1: a handoff is ALWAYS a draft — draft ≠ authority.
    assert!(p.is_draft);
}

#[test]
fn report_handoff_allows_empty_role_and_refs() {
    let (_d, store) = open_temp();
    let id = wv_report_handoff(&store, "SW-X-1", "sess-1", "", "a finding", &[]).unwrap();
    assert!(id > 0);
    let rows = handoff_state(&store, "SW-X-1").unwrap();
    assert_eq!(rows[0].to_role, "");
    assert_eq!(rows[0].refs, serde_json::json!([]));
}

#[test]
fn report_handoff_refuses_blank_finding() {
    let (_d, store) = open_temp();
    let err = wv_report_handoff(&store, "SW-X-1", "sess-1", "review", "   ", &[]).unwrap_err();
    assert!(matches!(
        err,
        WeftStoreError::Handoff(HandoffError::MissingRequiredField { field: "finding" })
    ));
    // Nothing persisted on a refusal.
    assert!(handoff_state(&store, "SW-X-1").unwrap().is_empty());
}

#[test]
fn report_handoff_refuses_blank_session_id() {
    // A blank session id would store an orphan the `handoff-record` hook (which matches
    // by the completing session's real id) can never find — refuse it at the writer.
    let (_d, store) = open_temp();
    let err = wv_report_handoff(&store, "SW-X-1", "  ", "review", "a finding", &[]).unwrap_err();
    assert!(matches!(
        err,
        WeftStoreError::Handoff(HandoffError::MissingRequiredField {
            field: "session_id"
        })
    ));
    assert!(handoff_state(&store, "SW-X-1").unwrap().is_empty());
}

#[test]
fn report_handoff_refuses_blank_story_id() {
    let (_d, store) = open_temp();
    let err = wv_report_handoff(&store, "  ", "sess-1", "review", "a finding", &[]).unwrap_err();
    assert!(matches!(
        err,
        WeftStoreError::Handoff(HandoffError::MissingRequiredField { field: "story_id" })
    ));
}

#[test]
fn is_draft_check_forbids_a_non_draft_row() {
    // AC-1 belt: the schema `CHECK(is_draft = 1)` makes an authoritative handoff
    // impossible even via a raw write — draft ≠ authority is a table invariant.
    let (_d, store) = open_temp();
    let conn = store.conn().unwrap();
    let res = conn.execute(
        "INSERT INTO wv_handoff_records (story_id, session_id, to_role, finding, refs, is_draft, created_at) \
         VALUES ('SW-X-1','sess-1','review','f','[]',0,'now')",
        [],
    );
    assert!(
        res.is_err(),
        "a non-draft (is_draft=0) handoff must be refused by the CHECK"
    );
}

// ---- AC-2: the packet is readable data, newest-first. ----

#[test]
fn handoff_state_returns_newest_first() {
    let (_d, store) = open_temp();
    wv_report_handoff(&store, "SW-X-1", "sess-1", "review", "first", &[]).unwrap();
    wv_report_handoff(&store, "SW-X-1", "sess-2", "review", "second", &[]).unwrap();
    let rows = handoff_state(&store, "SW-X-1").unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].finding, "second");
    assert_eq!(rows[1].finding, "first");
}

#[test]
fn handoff_state_empty_for_unknown_story() {
    let (_d, store) = open_temp();
    assert!(handoff_state(&store, "nope").unwrap().is_empty());
}

// ---- AC-3: the hook advances `signal → handoff` only when a packet exists. ----

#[test]
fn advance_by_session_flips_signal_to_handoff_when_packet_exists() {
    let (_d, store) = open_temp();
    seed_signalled_worker(&store, "SW-X-1", "sess-1");
    wv_report_handoff(&store, "SW-X-1", "sess-1", "review", "a finding", &[]).unwrap();

    let advanced = handoff_advance_by_session(&store, "sess-1").unwrap();
    assert!(advanced);
    assert_eq!(roster_status(&store, "SW-X-1").as_deref(), Some("handoff"));
}

#[test]
fn advance_by_session_is_a_noop_without_a_packet() {
    // A worker that reported no handoff skips the optional step (stays `signal`,
    // A3 will take it straight to `deprovision`).
    let (_d, store) = open_temp();
    seed_signalled_worker(&store, "SW-X-1", "sess-1");
    let advanced = handoff_advance_by_session(&store, "sess-1").unwrap();
    assert!(!advanced);
    assert_eq!(roster_status(&store, "SW-X-1").as_deref(), Some("signal"));
}

#[test]
fn advance_by_session_is_a_noop_for_unknown_session() {
    let (_d, store) = open_temp();
    let advanced = handoff_advance_by_session(&store, "ghost").unwrap();
    assert!(!advanced);
}

#[test]
fn advance_by_session_blank_is_a_noop() {
    let (_d, store) = open_temp();
    assert!(!handoff_advance_by_session(&store, "").unwrap());
}

#[test]
fn advance_by_session_is_idempotent() {
    // A second run (a re-fired hook) after the row is already `handoff` is a graceful
    // skip, not an error — the CAS loss is swallowed.
    let (_d, store) = open_temp();
    seed_signalled_worker(&store, "SW-X-1", "sess-1");
    wv_report_handoff(&store, "SW-X-1", "sess-1", "review", "a finding", &[]).unwrap();
    assert!(handoff_advance_by_session(&store, "sess-1").unwrap());
    // Second call: already in `handoff`, CAS loses → Ok(false), no error.
    assert!(!handoff_advance_by_session(&store, "sess-1").unwrap());
    assert_eq!(roster_status(&store, "SW-X-1").as_deref(), Some("handoff"));
}

// ---- AC-4: every mutation co-commits its audit row. ----

#[test]
fn report_handoff_co_commits_an_audit_row() {
    let (_d, store) = open_temp();
    wv_report_handoff(&store, "SW-X-1", "sess-1", "review", "a finding", &[]).unwrap();
    let ops = audit_ops_for(&store);
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].0, "append");
    assert_eq!(ops[0].1, "wv_handoff_records");
}
