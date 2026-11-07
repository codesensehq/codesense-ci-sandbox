// SPDX-License-Identifier: MIT
//! SW-WV-E1 acceptance tests for the endorsement-gate store layer
//! (`wv_assertions`, `wv_recipient_acks`) and its named write handlers.
//!
//! Covers AC-1 (tables created on open + migrate over a B3 db + round-trip),
//! AC-3 (recipient-ack never changes disposition), and AC-5 (CAS disposition
//! transition + settle-attention teardown). The MCP-verb / Tauri-command behavior
//! (AC-2/AC-4/AC-6/AC-7) is exercised at the handler seam (mcp/handler.rs tests,
//! commands/weft_ledger.rs tests) since those need an `AppHandle` / mock session.

use super::*;
use crate::weft::store::WeftStore;

fn open_temp() -> (tempfile::TempDir, WeftStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
    (dir, store)
}

fn table_exists(store: &WeftStore, table: &str) -> bool {
    let conn = store.conn().unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        params![table],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
        == 1
}

fn audit_rows(store: &WeftStore, table: &str) -> i64 {
    let conn = store.conn().unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM wv_write_log WHERE tbl=?1",
        params![table],
        |r| r.get(0),
    )
    .unwrap()
}

// ============================== AC-1: table creation ==============================

#[test]
fn wv_assertions_table_created_on_open() {
    let (_dir, store) = open_temp();
    assert!(table_exists(&store, "wv_assertions"));
    assert!(table_exists(&store, "wv_recipient_acks"));
}

#[test]
fn wv_assertions_migrates_over_b3_schema() {
    // Simulate a B3-level db: open once (creates the B3 sibling tables + a stub
    // assertion table), write a wv_approvals row, then re-open (idempotent E1
    // migration) and assert the approval row survives + the E1 tables exist.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("weft.sqlite3");
    let store = WeftStore::open(&path).unwrap();
    let (approval_id, _, _) = crate::weft::wv_write_approval(
        &store,
        crate::weft::ApprovalTier::T2,
        false,
        crate::weft::BlastRadius::Low,
        crate::weft::Confidence::High,
        "please endorse",
        None,
        Some("open"),
        None,
        &crate::weft::NullNotifier,
    )
    .unwrap();
    drop(store);

    // Re-open (the E1 forward-migration path runs again — must be lossless).
    let store = WeftStore::open(&path).unwrap();
    assert!(table_exists(&store, "wv_assertions"));
    assert!(table_exists(&store, "wv_recipient_acks"));
    let conn = store.conn().unwrap();
    let surviving: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wv_approvals WHERE id=?1",
            params![approval_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(surviving, 1, "the B3 wv_approvals row must survive E1 open");
}

#[test]
fn assertion_round_trip_insert_read() {
    let (_dir, store) = open_temp();
    let id = crate::weft::wv_insert_assertion(
        &store,
        "sess-1",
        Some("run-42"),
        "the sky is green",
        "slack:#general",
        None,
    )
    .unwrap();
    let row = crate::weft::wv_get_assertion(&store, id).unwrap().unwrap();
    assert_eq!(row.session_id, "sess-1");
    assert_eq!(row.run_id.as_deref(), Some("run-42"));
    assert_eq!(row.claim, "the sky is green");
    assert_eq!(row.channel, "slack:#general");
    assert_eq!(row.disposition, "pending", "insert is always pending");
    // Audit co-committed (AC-9-style).
    assert_eq!(audit_rows(&store, "wv_assertions"), 1);
}

#[test]
fn assertion_insert_rejects_blank_claim_and_channel() {
    let (_dir, store) = open_temp();
    assert!(crate::weft::wv_insert_assertion(&store, "s", None, "   ", "ch", None).is_err());
    assert!(crate::weft::wv_insert_assertion(&store, "s", None, "claim", "  ", None).is_err());
    assert!(crate::weft::wv_insert_assertion(&store, "  ", None, "claim", "ch", None).is_err());
}

// ===================== AC-3: recipient-ack ≠ ratification =====================

#[test]
fn recipient_ack_does_not_change_disposition() {
    let (_dir, store) = open_temp();
    let id = crate::weft::wv_insert_assertion(&store, "sess", None, "claim", "chan", None).unwrap();

    crate::weft::wv_insert_recipient_ack(&store, id, "alice", "seen").unwrap();
    crate::weft::wv_insert_recipient_ack(&store, id, "bob", "declined").unwrap();

    // The assertion stays pending — a recipient ack (even a decline) NEVER ratifies.
    let disp = crate::weft::wv_get_assertion_disposition(&store, id)
        .unwrap()
        .unwrap();
    assert_eq!(disp, "pending");

    // Two ack rows exist with the right values.
    let conn = store.conn().unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wv_recipient_acks WHERE assertion_id=?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
    let acks: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT ack FROM wv_recipient_acks WHERE assertion_id=?1 ORDER BY id")
            .unwrap();
        stmt.query_map(params![id], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(acks, vec!["seen".to_string(), "declined".to_string()]);
}

#[test]
fn recipient_ack_rejects_invalid_ack_value() {
    let (_dir, store) = open_temp();
    let id = crate::weft::wv_insert_assertion(&store, "s", None, "c", "ch", None).unwrap();
    assert!(crate::weft::wv_insert_recipient_ack(&store, id, "alice", "loved-it").is_err());
    assert!(crate::weft::wv_insert_recipient_ack(&store, id, "  ", "seen").is_err());
}

// =============== AC-5: CAS disposition transition + attention teardown ===============

#[test]
fn wv_set_assertion_disposition_cas_guard() {
    let (_dir, store) = open_temp();
    let id = crate::weft::wv_insert_assertion(&store, "s", None, "c", "ch", None).unwrap();

    // Winner: pending → ratified.
    let won = crate::weft::wv_set_assertion_disposition(&store, id, "pending", "ratified").unwrap();
    assert!(
        won,
        "the first transition off 'pending' wins (changes()==1)"
    );
    assert_eq!(
        crate::weft::wv_get_assertion_disposition(&store, id)
            .unwrap()
            .unwrap(),
        "ratified"
    );

    // Loser: a second pending→revoked matches no row (already ratified).
    let lost = crate::weft::wv_set_assertion_disposition(&store, id, "pending", "revoked").unwrap();
    assert!(
        !lost,
        "the guard fails once the row left 'pending' (CAS loss)"
    );
    assert_eq!(
        crate::weft::wv_get_assertion_disposition(&store, id)
            .unwrap()
            .unwrap(),
        "ratified",
        "a lost CAS must not clobber the winning state"
    );

    // Invalid next value is rejected before any write.
    assert!(crate::weft::wv_set_assertion_disposition(&store, id, "pending", "maybe").is_err());
}

#[test]
fn wv_settle_attention_transitions_matching_row_by_source_ref() {
    let (_dir, store) = open_temp();
    // Create a YOU-OWE attention row keyed on (source="assertion", ref="7"), as the
    // E1 assertion→attention hook does.
    let att_id = crate::weft::wv_upsert_attention(
        &store,
        None,
        Some("needs-you"),
        Some("Agent assertion pending your 👍 — slack:#general"),
        Some("assertion"),
        Some("7"),
        Some("sess"),
        None,
    )
    .unwrap();

    // Settle it (ratify teardown, D8).
    let settled =
        crate::weft::wv_settle_attention(&store, "assertion", "7", "done-unacked").unwrap();
    assert!(settled);
    let conn = store.conn().unwrap();
    let state: String = conn
        .query_row(
            "SELECT state FROM wv_attention WHERE id=?1",
            params![att_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "done-unacked");

    // Settling a non-existent (source, ref) is a no-op false, not an error.
    let noop =
        crate::weft::wv_settle_attention(&store, "assertion", "999", "done-unacked").unwrap();
    assert!(!noop);

    // Illegal target state is rejected.
    assert!(crate::weft::wv_settle_attention(&store, "assertion", "7", "bogus").is_err());
}

/// End-to-end D8 hook lifecycle at the store layer: post (insert + attention hook) →
/// ratify (CAS + attention teardown). Mirrors EXACTLY the sequence the
/// `wv_post_assertion` MCP verb and the `wv_ratify` Tauri command drive, so the
/// assertion→attention create/teardown contract is proven without an `AppHandle`.
#[test]
fn post_then_ratify_creates_and_tears_down_the_you_owe_attention_row() {
    let (_dir, store) = open_temp();

    // POST: insert the assertion, then upsert its YOU-OWE attention row keyed on
    // (source="assertion", ref=<id>) — the exact two-call closure the verb runs.
    let id =
        crate::weft::wv_insert_assertion(&store, "sess", Some("run-1"), "claim", "slack:#x", None)
            .unwrap();
    let summary = "Agent assertion pending your 👍 — slack:#x";
    crate::weft::wv_upsert_attention(
        &store,
        None,
        Some("needs-you"),
        Some(summary),
        Some("assertion"),
        Some(&id.to_string()),
        Some("sess"),
        None,
    )
    .unwrap();

    // The YOU-OWE row exists and needs-you.
    let conn = store.conn().unwrap();
    let (state, src): (String, String) = conn
        .query_row(
            "SELECT state, source FROM wv_attention WHERE source='assertion' AND ref=?1",
            params![id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, "needs-you");
    assert_eq!(src, "assertion");
    drop(conn);

    // RATIFY: CAS pending→ratified, then D8 teardown settles the attention row.
    assert!(crate::weft::wv_set_assertion_disposition(&store, id, "pending", "ratified").unwrap());
    assert!(
        crate::weft::wv_settle_attention(&store, "assertion", &id.to_string(), "done-unacked")
            .unwrap()
    );
    let conn = store.conn().unwrap();
    let state: String = conn
        .query_row(
            "SELECT state FROM wv_attention WHERE source='assertion' AND ref=?1",
            params![id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "done-unacked", "D8 teardown clears the YOU-OWE item");
}
