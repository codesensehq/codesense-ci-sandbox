// SPDX-License-Identifier: MIT
//! SW-WV-B3 acceptance tests for the five sibling ledgers (approvals, decisions,
//! watermarks, roster, attention) and their named write handlers.
//!
//! Covers AC-1 (decisions + blank-`ends_when` guard), AC-2 (roster relation +
//! peer-deprovision refusal + upsert idempotency), AC-3 (approval tier dispatch +
//! settle), AC-4 (watermark upsert idempotency + source enum), AC-5 (concurrent
//! no-dup ids), AC-6 (migration idempotency over a B1/B2 db), AC-7 (off-thread /
//! non-blocking T3 on a single-threaded runtime), AC-8 (attention + blank-`summary`
//! guard + state transition), AC-9 (audit co-commit).

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use super::*;
use crate::weft::store::WeftStore;

fn open_temp() -> (tempfile::TempDir, WeftStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
    (dir, store)
}

/// A stub notifier that records the `(session_id, approval_id, claim)` it was
/// surfaced with — the AC-7 injection point. Mirrors the `Arc<Mutex<…>>` shape the
/// real `AskUserRegistry` is held behind.
#[derive(Default)]
struct CapturingNotifier {
    seen: Arc<Mutex<Vec<(String, i64, String)>>>,
}
impl ApprovalNotifier for CapturingNotifier {
    fn notify_t3(&self, session_id: &str, approval_id: i64, claim: &str) -> String {
        self.seen
            .lock()
            .unwrap()
            .push((session_id.to_string(), approval_id, claim.to_string()));
        format!("corr-{approval_id}")
    }
}

/// A notifier that must never be called (T1/T2 paths never surface a question).
struct PanicNotifier;
impl ApprovalNotifier for PanicNotifier {
    fn notify_t3(&self, _session_id: &str, _approval_id: i64, _claim: &str) -> String {
        panic!("notify_t3 must not be called for a non-T3 approval");
    }
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

// ============================== AC-1: wv_decisions ==============================

#[test]
fn decision_rejects_blank_ends_when() {
    let (_d, store) = open_temp();
    let err = wv_write_decision(
        &store,
        "2026-09-18",
        "no bare veto",
        "SW-WV-B3",
        "   ",
        "sam",
        None,
    )
    .unwrap_err();
    assert!(
        matches!(err, WvWriteError::Invalid(_)),
        "blank ends_when is a validation refusal, not a storage error: {err}"
    );
}

#[test]
fn decision_round_trip_insert_read_retire() {
    let (_d, store) = open_temp();
    let id = wv_write_decision(
        &store,
        "2026-09-18",
        "prefer rough specs",
        "SW-WV-B3",
        "never",
        "sam",
        Some("test-run"),
    )
    .unwrap();
    assert!(id >= 1);

    let conn = store.conn().unwrap();
    let (constrains, ends_when, status): (String, String, String) = conn
        .query_row(
            "SELECT constrains, ends_when, status FROM wv_decisions WHERE id=?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(constrains, "SW-WV-B3");
    assert_eq!(ends_when, "never");
    assert_eq!(status, "active", "a fresh decision defaults to active");
    drop(conn);

    assert!(
        wv_retire_decision(&store, id).unwrap(),
        "first retire succeeds"
    );
    assert!(
        !wv_retire_decision(&store, id).unwrap(),
        "a second retire is a no-op (already superseded)"
    );
    let conn = store.conn().unwrap();
    let status: String = conn
        .query_row(
            "SELECT status FROM wv_decisions WHERE id=?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "superseded");
}

// ============================== AC-2: wv_roster ==============================

fn peer_row(name: &str) -> WvRosterRow {
    WvRosterRow {
        name: name.to_string(),
        workspace: None,
        pane: None,
        kind: Some("controller".into()),
        subclass: None,
        lifecycle: "persistent".into(),
        owned: None,
        orch_owner: None,
        relation: "peer".into(),
        parent: None,
        current_task: None,
        cwd: None,
        status: Some("idle".into()),
        session_id: None,
        run_id: None,
    }
}

fn child_row(name: &str, scope: &str, parent: &str) -> WvRosterRow {
    WvRosterRow {
        relation: format!("child:{scope}"),
        parent: Some(parent.into()),
        ..peer_row(name)
    }
}

#[test]
fn roster_refuses_to_deprovision_a_peer() {
    let (_d, store) = open_temp();
    wv_upsert_roster(&store, &peer_row("peer-A")).unwrap();
    let err = wv_deprovision_roster(&store, "peer-A").unwrap_err();
    assert!(
        matches!(err, WvWriteError::Invalid(_)),
        "deprovisioning a peer is refused (observe-only): {err}"
    );
    // The row survives the refusal.
    assert_eq!(wv_list_roster(&store).unwrap().len(), 1);
}

#[test]
fn roster_child_can_be_deprovisioned() {
    let (_d, store) = open_temp();
    wv_upsert_roster(&store, &child_row("child-1", "epic-42", "root")).unwrap();
    assert!(
        wv_deprovision_roster(&store, "child-1").unwrap(),
        "a child row deprovisions"
    );
    assert!(
        wv_list_roster(&store).unwrap().is_empty(),
        "the child row is gone"
    );
    assert!(
        !wv_deprovision_roster(&store, "child-1").unwrap(),
        "deprovisioning a gone row is a no-op, not an error"
    );
}

#[test]
fn roster_upsert_is_idempotent() {
    let (_d, store) = open_temp();
    wv_upsert_roster(&store, &child_row("w-1", "epic-1", "root")).unwrap();
    // Second upsert with a changed field replaces, never duplicates.
    let mut updated = child_row("w-1", "epic-1", "root");
    updated.status = Some("running".into());
    wv_upsert_roster(&store, &updated).unwrap();
    let rows = wv_list_roster(&store).unwrap();
    assert_eq!(rows.len(), 1, "upsert on the name PK never duplicates");
    assert_eq!(rows[0].status.as_deref(), Some("running"));
}

#[test]
fn roster_rejects_a_malformed_relation() {
    let (_d, store) = open_temp();
    let mut bad = peer_row("w-x");
    bad.relation = "child:".into(); // empty scope
    assert!(matches!(
        wv_upsert_roster(&store, &bad).unwrap_err(),
        WvWriteError::Invalid(_)
    ));
    bad.relation = "sibling".into(); // not one of the two forms
    assert!(matches!(
        wv_upsert_roster(&store, &bad).unwrap_err(),
        WvWriteError::Invalid(_)
    ));
}

// ============================== AC-3 / AC-7: wv_approvals ==============================

#[test]
fn approval_t1_ratifies_immediately() {
    let (_d, store) = open_temp();
    let (id, _tier, disposition) = wv_write_approval(
        &store,
        ApprovalTier::T1,
        false,
        crate::weft::BlastRadius::Low,
        crate::weft::Confidence::High,
        "auto ok",
        None,
        None,
        None,
        &PanicNotifier,
    )
    .unwrap();
    assert_eq!(disposition, "ratified", "T1 auto-ratifies at insert");
    let conn = store.conn().unwrap();
    let got: String = conn
        .query_row(
            "SELECT disposition FROM wv_approvals WHERE id=?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(got, "ratified");
}

#[test]
fn approval_t3_pends_and_emits_with_session_id() {
    let (_d, store) = open_temp();
    let notifier = CapturingNotifier::default();
    let (id, _tier, disposition) = wv_write_approval(
        &store,
        ApprovalTier::T3,
        false,
        crate::weft::BlastRadius::Low,
        crate::weft::Confidence::High,
        "needs a human",
        Some("test-run"),
        None,
        Some("controller-sess"),
        &notifier,
    )
    .unwrap();
    assert_eq!(disposition, "pending", "T3 inserts pending");
    // (b) the notifier captured the correct session id + row id.
    let seen = notifier.seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "exactly one T3 emission");
    assert_eq!(seen[0].0, "controller-sess");
    assert_eq!(seen[0].1, id);
    // (c) the row is pending.
    let conn = store.conn().unwrap();
    let got: String = conn
        .query_row(
            "SELECT disposition FROM wv_approvals WHERE id=?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(got, "pending");
}

#[test]
fn approval_t3_requires_a_session_id() {
    let (_d, store) = open_temp();
    let notifier = CapturingNotifier::default();
    // Missing session_id → rejected BEFORE the notifier is touched.
    let err = wv_write_approval(
        &store,
        ApprovalTier::T3,
        false,
        crate::weft::BlastRadius::Low,
        crate::weft::Confidence::High,
        "q",
        None,
        None,
        None,
        &notifier,
    )
    .unwrap_err();
    assert!(matches!(err, WvWriteError::Invalid(_)));
    assert!(
        notifier.seen.lock().unwrap().is_empty(),
        "the registry was not touched on a rejected T3 call"
    );
}

#[test]
fn approval_t2_carries_the_window_state() {
    let (_d, store) = open_temp();
    let (id, _tier, disposition) = wv_write_approval(
        &store,
        ApprovalTier::T2,
        false,
        crate::weft::BlastRadius::Low,
        crate::weft::Confidence::High,
        "lazy consent",
        None,
        Some("2026-12-31T00:00:00Z"),
        None,
        &PanicNotifier,
    )
    .unwrap();
    assert_eq!(disposition, "pending");
    let conn = store.conn().unwrap();
    let window: String = conn
        .query_row(
            "SELECT window_state FROM wv_approvals WHERE id=?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(window, "2026-12-31T00:00:00Z");
}

#[test]
fn approval_settle_round_trip() {
    let (_d, store) = open_temp();
    let (id, _tier, _) = wv_write_approval(
        &store,
        ApprovalTier::T3,
        false,
        crate::weft::BlastRadius::Low,
        crate::weft::Confidence::High,
        "ratify me",
        None,
        None,
        Some("s"),
        &CapturingNotifier::default(),
    )
    .unwrap();
    assert!(wv_settle_approval(&store, id, "ratified").unwrap());
    assert!(
        !wv_settle_approval(&store, id, "revoked").unwrap(),
        "a second settle is a no-op (already settled)"
    );
    // An invalid disposition is refused.
    assert!(matches!(
        wv_settle_approval(&store, id, "maybe").unwrap_err(),
        WvWriteError::Invalid(_)
    ));
}

/// AC-7: the T3 write runs to completion on a SINGLE-THREADED Tokio runtime — a
/// `block_in_place` would panic/deadlock there. We call the handler directly (not
/// via Tauri invoke) on `tokio::test`'s default single-threaded runtime and assert
/// it returns `Ok` with a pending row and the captured session id. No wall-clock
/// assertion (CI timings are unreliable).
#[tokio::test]
async fn approval_t3_is_non_blocking_on_single_threaded_runtime() {
    let (_d, store) = open_temp();
    let notifier = CapturingNotifier::default();
    let (id, _tier, disposition) = wv_write_approval(
        &store,
        ApprovalTier::T3,
        false,
        crate::weft::BlastRadius::Low,
        crate::weft::Confidence::High,
        "off-thread ok",
        None,
        None,
        Some("async-sess"),
        &notifier,
    )
    .expect("handler returns Ok without panicking on a single-threaded runtime");
    assert_eq!(disposition, "pending");
    assert_eq!(notifier.seen.lock().unwrap()[0].0, "async-sess");
    let conn = store.conn().unwrap();
    let got: String = conn
        .query_row(
            "SELECT disposition FROM wv_approvals WHERE id=?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(got, "pending");
}

// ============================== AC-4: wv_watermarks ==============================

#[test]
fn watermark_upsert_is_idempotent_last_writer_wins() {
    let (_d, store) = open_temp();
    wv_upsert_watermark(&store, "github", "SW-WV-B3", "cursor-1").unwrap();
    wv_upsert_watermark(&store, "github", "SW-WV-B3", "cursor-2").unwrap();
    let conn = store.conn().unwrap();
    let (count, last): (i64, String) = conn
        .query_row(
            "SELECT COUNT(*), MAX(last_handled) FROM wv_watermarks WHERE source='github' AND scope='SW-WV-B3'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(count, 1, "two upserts on the same (source,scope) → one row");
    assert_eq!(last, "cursor-2", "the later last_handled wins");
}

#[test]
fn watermark_rejects_an_invalid_source() {
    let (_d, store) = open_temp();
    assert!(matches!(
        wv_upsert_watermark(&store, "email", "x", "c").unwrap_err(),
        WvWriteError::Invalid(_)
    ));
    // Every allowed source is accepted.
    for source in WV_WATERMARK_SOURCES {
        wv_upsert_watermark(&store, source, "s", "c").unwrap();
    }
}

// ============================== AC-8: wv_attention ==============================

#[test]
fn attention_rejects_blank_summary_at_creation() {
    let (_d, store) = open_temp();
    assert!(matches!(
        wv_upsert_attention(&store, None, None, Some("   "), None, None, None, None).unwrap_err(),
        WvWriteError::Invalid(_)
    ));
    assert!(
        matches!(
            wv_upsert_attention(&store, None, None, None, None, None, None, None).unwrap_err(),
            WvWriteError::Invalid(_)
        ),
        "a missing summary is also rejected at creation"
    );
}

#[test]
fn attention_round_trip_and_state_transition() {
    let (_d, store) = open_temp();
    let id = wv_upsert_attention(
        &store,
        None,
        None,
        Some("review needed"),
        Some("github"),
        Some("#2948"),
        Some("sam"),
        None,
    )
    .unwrap();
    let conn = store.conn().unwrap();
    let (state, summary): (String, String) = conn
        .query_row(
            "SELECT state, summary FROM wv_attention WHERE id=?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, "needs-you", "a fresh item defaults to needs-you");
    assert_eq!(summary, "review needed");
    drop(conn);

    // needs-you → waiting; summary is preserved on a state-only update.
    let same = wv_upsert_attention(
        &store,
        Some(id),
        Some("waiting"),
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(same, id);
    let conn = store.conn().unwrap();
    let (state, summary): (String, String) = conn
        .query_row(
            "SELECT state, summary FROM wv_attention WHERE id=?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, "waiting");
    assert_eq!(
        summary, "review needed",
        "a state-only update never blanks summary"
    );
}

#[test]
fn attention_rejects_an_invalid_state() {
    let (_d, store) = open_temp();
    assert!(matches!(
        wv_upsert_attention(
            &store,
            None,
            Some("nope"),
            Some("s"),
            None,
            None,
            None,
            None
        )
        .unwrap_err(),
        WvWriteError::Invalid(_)
    ));
}

// SLW-TL-4: a declared `need` round-trips on create, can be advanced on update, and
// an unknown need is a validation refusal (never persisted).
#[test]
fn attention_round_trips_a_declared_need() {
    let (_d, store) = open_temp();
    let id = wv_upsert_attention(
        &store,
        None,
        Some("needs-you"),
        Some("Approve the deploy?"),
        Some("post"),
        Some("run-42"),
        None,
        Some("needs-approval"),
    )
    .unwrap();
    let conn = store.conn().unwrap();
    let need: Option<String> = conn
        .query_row(
            "SELECT need FROM wv_attention WHERE id=?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(need.as_deref(), Some("needs-approval"));
    drop(conn);

    // Advance the need on update; a state-only update never blanks it (COALESCE).
    wv_upsert_attention(
        &store,
        Some(id),
        Some("done-unacked"),
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let conn = store.conn().unwrap();
    let need: Option<String> = conn
        .query_row(
            "SELECT need FROM wv_attention WHERE id=?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        need.as_deref(),
        Some("needs-approval"),
        "a state-only update preserves the declared need"
    );
}

#[test]
fn attention_rejects_an_invalid_need() {
    let (_d, store) = open_temp();
    assert!(matches!(
        wv_upsert_attention(
            &store,
            None,
            None,
            Some("s"),
            None,
            None,
            None,
            Some("needs-a-hug")
        )
        .unwrap_err(),
        WvWriteError::Invalid(_)
    ));
}

// SLW-TL-4: a legacy attention row (no declared need) still reads cleanly — the
// column is nullable, so an item written before the field carries `NULL`.
#[test]
fn attention_without_a_need_reads_null() {
    let (_d, store) = open_temp();
    let id = wv_upsert_attention(
        &store,
        None,
        None,
        Some("legacy row"),
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let conn = store.conn().unwrap();
    let need: Option<String> = conn
        .query_row(
            "SELECT need FROM wv_attention WHERE id=?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(need, None);
}

// ============================== AC-5: concurrent no-dup ids ==============================

fn concurrent_ids(
    store: &WeftStore,
    insert: impl Fn(&WeftStore) -> i64 + Send + Sync + Copy,
) -> Vec<i64> {
    const PER_THREAD: usize = 40;
    let ids: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    std::thread::scope(|s| {
        for _ in 0..2 {
            let ids = Arc::clone(&ids);
            s.spawn(move || {
                let mut local = Vec::with_capacity(PER_THREAD);
                for _ in 0..PER_THREAD {
                    local.push(insert(store));
                }
                ids.lock().unwrap().extend(local);
            });
        }
    });
    Arc::try_unwrap(ids).unwrap().into_inner().unwrap()
}

#[test]
fn decisions_concurrent_insert_has_no_duplicate_ids() {
    let (_d, store) = open_temp();
    let ids = concurrent_ids(&store, |store| {
        wv_write_decision(store, "d", "x", "art", "never", "o", None).unwrap()
    });
    let unique: BTreeSet<i64> = ids.iter().copied().collect();
    assert_eq!(unique.len(), ids.len(), "AUTOINCREMENT ids never collide");
}

#[test]
fn approvals_concurrent_insert_has_no_duplicate_ids() {
    let (_d, store) = open_temp();
    let ids = concurrent_ids(&store, |store| {
        wv_write_approval(
            store,
            ApprovalTier::T1,
            false,
            crate::weft::BlastRadius::Low,
            crate::weft::Confidence::High,
            "c",
            None,
            None,
            None,
            &PanicNotifier,
        )
        .unwrap()
        .0
    });
    let unique: BTreeSet<i64> = ids.iter().copied().collect();
    assert_eq!(unique.len(), ids.len());
}

#[test]
fn attention_concurrent_insert_has_no_duplicate_ids() {
    let (_d, store) = open_temp();
    let ids = concurrent_ids(&store, |store| {
        wv_upsert_attention(store, None, None, Some("s"), None, None, None, None).unwrap()
    });
    let unique: BTreeSet<i64> = ids.iter().copied().collect();
    assert_eq!(unique.len(), ids.len());
}

// ============================== AC-6: migration idempotency ==============================

#[test]
fn open_over_a_b1_b2_db_adds_all_tables_and_keeps_rows() {
    // Simulate a pre-B3 db: create the B1 stub schema (via open, which is B3-aware),
    // but prove idempotency by opening TWICE and inserting a queue row between.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("weft.sqlite3");

    // First open — materialize schema.
    let store = WeftStore::open(&path).unwrap();
    let mut conn = store.conn().unwrap();
    // A B2-shaped queue row (the queue table + its state column exist post-open).
    let queue_id = wv_append(
        &mut conn,
        "wv_queue",
        "INSERT INTO wv_queue (id) VALUES (?1)",
        &[],
    )
    .unwrap();
    drop(conn);
    drop(store);

    // Second open — must not fail and must not lose the queue row (AC-6).
    let store = WeftStore::open(&path).unwrap();
    let conn = store.conn().unwrap();
    let found: i64 = conn
        .query_row(
            "SELECT id FROM wv_queue WHERE id=?1",
            params![queue_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(found, queue_id, "the queue row survives the B3 open");

    // All five sibling tables exist and are writable through their handlers.
    drop(conn);
    wv_write_decision(&store, "d", "x", "a", "never", "o", None).unwrap();
    wv_upsert_roster(&store, &peer_row("r")).unwrap();
    wv_write_approval(
        &store,
        ApprovalTier::T1,
        false,
        crate::weft::BlastRadius::Low,
        crate::weft::Confidence::High,
        "c",
        None,
        None,
        None,
        &PanicNotifier,
    )
    .unwrap();
    wv_upsert_watermark(&store, "local", "s", "c").unwrap();
    wv_upsert_attention(&store, None, None, Some("s"), None, None, None, None).unwrap();
}

// ============================== AC-9: audit atomicity ==============================

#[test]
fn every_b3_write_co_commits_an_audit_row() {
    let (_d, store) = open_temp();
    // AC-9 representative assertion: a decision write logs exactly one wv_decisions
    // audit row whose row_id matches the inserted id.
    let id = wv_write_decision(&store, "d", "dec", "art", "never", "o", None).unwrap();
    let conn = store.conn().unwrap();
    let (count, logged_id): (i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), MAX(row_id) FROM wv_write_log WHERE tbl='wv_decisions' AND operation='append'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(count, 1, "exactly one decision audit row");
    assert_eq!(
        logged_id, id,
        "the audit row_id matches the inserted decision id"
    );
    drop(conn);

    // Every other B3 write also lands an audit row (co-commit holds across the family).
    wv_upsert_roster(&store, &peer_row("r")).unwrap();
    wv_write_approval(
        &store,
        ApprovalTier::T1,
        false,
        crate::weft::BlastRadius::Low,
        crate::weft::Confidence::High,
        "c",
        None,
        None,
        None,
        &PanicNotifier,
    )
    .unwrap();
    wv_upsert_watermark(&store, "local", "s", "c").unwrap();
    wv_upsert_attention(&store, None, None, Some("s"), None, None, None, None).unwrap();
    assert_eq!(audit_rows(&store, "wv_roster"), 1);
    assert_eq!(audit_rows(&store, "wv_approvals"), 1);
    assert_eq!(audit_rows(&store, "wv_watermarks"), 1);
    assert_eq!(audit_rows(&store, "wv_attention"), 1);
}
