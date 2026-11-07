// SPDX-License-Identifier: MIT
//! SW-WV-E2 acceptance tests for the tier-picker integration into
//! `wv_write_approval` and the three risk-signal columns.
//!
//! Covers AC-1 (three columns migrate + defaults), AC-3 (picker chooses the tier,
//! disposition follows, response shape), AC-5 (tier immutable across settle), and
//! AC-8 (audit co-commit). The pure-picker unit tests (AC-2) and the enum-boundary
//! tests (AC-7 unit half) live in `weft::tier_picker`; the MCP-verb tests (AC-4,
//! AC-7 boundary) live in `mcp::handler`.

use super::*;
use crate::weft::store::WeftStore;
use crate::weft::{BlastRadius, Confidence};

fn open_temp() -> (tempfile::TempDir, WeftStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
    (dir, store)
}

/// A notifier that must never be called by these tests' non-blocking paths — but
/// records nothing (T3 rows do surface a question; we only assert the row state).
struct SilentNotifier;
impl ApprovalNotifier for SilentNotifier {
    fn notify_t3(&self, _session_id: &str, _approval_id: i64, _claim: &str) -> String {
        String::new()
    }
}

fn tier_of(store: &WeftStore, id: i64) -> String {
    store
        .conn()
        .unwrap()
        .query_row(
            "SELECT tier FROM wv_approvals WHERE id=?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap()
}

fn disposition_of(store: &WeftStore, id: i64) -> String {
    store
        .conn()
        .unwrap()
        .query_row(
            "SELECT disposition FROM wv_approvals WHERE id=?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap()
}

// ============================== AC-1: migration ==============================

#[test]
fn three_risk_columns_heal_a_b3_era_db_with_defaults() {
    // A B3-era db lacks irreversible/blast_radius/confidence. Simulate one by
    // creating the id-only approval stub + the B3 columns manually, inserting a row
    // WITHOUT the E2 columns, then re-opening (the E2 ensure_column path runs).
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("weft.sqlite3");
    {
        let conn = crate::shared::sqlite::open_wal_db(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE wv_approvals (\
                id INTEGER PRIMARY KEY AUTOINCREMENT, \
                tier TEXT, claim TEXT, posted_at TEXT, source TEXT, \
                window_state TEXT NOT NULL DEFAULT 'open', \
                disposition TEXT NOT NULL DEFAULT 'pending');",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO wv_approvals (tier, claim, posted_at) VALUES ('T2', 'pre-e2', 'now')",
            [],
        )
        .unwrap();
    }
    // Re-open through WeftStore — the E2 ensure_column calls fire.
    let store = WeftStore::open(&path).unwrap();
    let conn = store.conn().unwrap();
    let (irreversible, blast, confidence): (i64, String, String) = conn
        .query_row(
            "SELECT irreversible, blast_radius, confidence FROM wv_approvals WHERE claim='pre-e2'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("the pre-E2 row survives and reads back the new columns with defaults");
    assert_eq!(irreversible, 0, "pre-E2 row defaults irreversible=0");
    assert_eq!(blast, "low", "pre-E2 row defaults blast_radius='low'");
    assert_eq!(confidence, "high", "pre-E2 row defaults confidence='high'");
}

#[test]
fn fresh_db_has_the_three_columns() {
    // AC-1 belt-and-suspenders (Task 5b): a fresh open (no pre-existing db) carries
    // the three columns — a write with the picker path succeeds and reads back.
    let (_d, store) = open_temp();
    // Two signals force T3 (proving the picker + columns end-to-end); a forced T3
    // needs a session_id for routing.
    let (id, _tier, _disp) = wv_write_approval(
        &store,
        ApprovalTier::T1,
        true,
        BlastRadius::High,
        Confidence::Low,
        "fresh",
        None,
        None,
        Some("sess-fresh"),
        &SilentNotifier,
    )
    .unwrap();
    let conn = store.conn().unwrap();
    let (irreversible, blast, confidence): (i64, String, String) = conn
        .query_row(
            "SELECT irreversible, blast_radius, confidence FROM wv_approvals WHERE id=?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(irreversible, 1);
    assert_eq!(blast, "high");
    assert_eq!(confidence, "low");
}

// ============================== AC-3: picker chooses the tier ==============================

#[test]
fn two_signals_force_t3_regardless_of_preferred_t1() {
    let (_d, store) = open_temp();
    // irreversible + high blast (two signals) → T3 even though preferred is T1.
    // A forced T3 needs a session_id.
    let (id, chosen, disposition) = wv_write_approval(
        &store,
        ApprovalTier::T1,
        true,
        BlastRadius::High,
        Confidence::High,
        "risky",
        None,
        None,
        Some("sess-forced"),
        &SilentNotifier,
    )
    .unwrap();
    assert_eq!(chosen, ApprovalTier::T3, "the picker forced T3");
    assert_eq!(tier_of(&store, id), "T3", "the DB stores the picker's tier");
    assert_eq!(disposition, "pending", "a forced T3 lands pending");
}

#[test]
fn one_signal_keeps_the_preferred_tier() {
    let (_d, store) = open_temp();
    // One signal (confidence=low) → preferred (T2) stands.
    let (id, chosen, _disp) = wv_write_approval(
        &store,
        ApprovalTier::T2,
        false,
        BlastRadius::Low,
        Confidence::Low,
        "one signal",
        None,
        None,
        None,
        &SilentNotifier,
    )
    .unwrap();
    assert_eq!(chosen, ApprovalTier::T2);
    assert_eq!(tier_of(&store, id), "T2");
}

#[test]
fn preferred_t3_with_zero_signals_is_not_downgraded() {
    let (_d, store) = open_temp();
    let (id, chosen, _disp) = wv_write_approval(
        &store,
        ApprovalTier::T3,
        false,
        BlastRadius::Low,
        Confidence::High,
        "declared t3",
        None,
        None,
        Some("sess-t3"),
        &SilentNotifier,
    )
    .unwrap();
    assert_eq!(
        chosen,
        ApprovalTier::T3,
        "no downgrade of a pre-declared T3"
    );
    assert_eq!(tier_of(&store, id), "T3");
}

#[test]
fn t1_zero_signals_auto_ratifies() {
    let (_d, store) = open_temp();
    let (id, chosen, disposition) = wv_write_approval(
        &store,
        ApprovalTier::T1,
        false,
        BlastRadius::Low,
        Confidence::High,
        "pre-authorized",
        None,
        None,
        None,
        &SilentNotifier,
    )
    .unwrap();
    assert_eq!(chosen, ApprovalTier::T1);
    assert_eq!(disposition, "ratified", "T1 auto-ratifies (spine §4.1)");
    assert_eq!(disposition_of(&store, id), "ratified");
}

#[test]
fn t2_insert_is_pending() {
    let (_d, store) = open_temp();
    let (id, _chosen, disposition) = wv_write_approval(
        &store,
        ApprovalTier::T2,
        false,
        BlastRadius::Low,
        Confidence::High,
        "lazy",
        None,
        None,
        None,
        &SilentNotifier,
    )
    .unwrap();
    assert_eq!(disposition, "pending");
    assert_eq!(disposition_of(&store, id), "pending");
}

// ============================== AC-5: tier immutable across settle ==============================

#[test]
fn settle_does_not_change_the_tier() {
    let (_d, store) = open_temp();
    // A forced T3 (two signals), then ratify it — the tier must remain T3.
    let (id, chosen, _disp) = wv_write_approval(
        &store,
        ApprovalTier::T1,
        true,
        BlastRadius::High,
        Confidence::High,
        "settle me",
        None,
        None,
        Some("sess-x"),
        &SilentNotifier,
    )
    .unwrap();
    assert_eq!(chosen, ApprovalTier::T3);
    assert!(wv_settle_approval(&store, id, "ratified").unwrap());
    assert_eq!(
        tier_of(&store, id),
        "T3",
        "the tier is immutable after insert (AC-5) — settle touched only disposition"
    );
    assert_eq!(disposition_of(&store, id), "ratified");
}

// ============================== AC-8: audit co-commit ==============================

#[test]
fn write_approval_co_commits_an_audit_row() {
    let (_d, store) = open_temp();
    let (id, _chosen, _disp) = wv_write_approval(
        &store,
        ApprovalTier::T1,
        false,
        BlastRadius::Low,
        Confidence::High,
        "audited",
        None,
        None,
        None,
        &SilentNotifier,
    )
    .unwrap();
    let conn = store.conn().unwrap();
    let (count, logged_id): (i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), MAX(row_id) FROM wv_write_log \
             WHERE tbl='wv_approvals' AND operation='append'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(count, 1, "exactly one approval audit row (AC-8)");
    assert_eq!(
        logged_id, id,
        "the audit row_id matches the inserted approval"
    );
}
