// SPDX-License-Identifier: MIT
//! SW-WV-F5 store-layer tests for the YOU-OWE endorsement surface read.
//!
//! Covers [`wv_list_pending_approvals`]:
//!   - returns only `pending` rows (a settled row never surfaces as still-owed);
//!   - maps the E3 window columns onto the wire shape (duration / opened-at / banked pause);
//!   - computes `not_surfaced_but_expired` from the SAME effective-elapsed math the
//!     window verdict uses — true only for a spent, never-surfaced T2, false for a T3.
//!
//! Time is injected (`now: i64` epoch-seconds) so the elapsed math is deterministic.

use super::*;
use crate::weft::store::WeftStore;
use crate::weft::{BlastRadius, Confidence};

fn open_temp() -> (tempfile::TempDir, WeftStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
    (dir, store)
}

struct SilentNotifier;
impl ApprovalNotifier for SilentNotifier {
    fn notify_t3(&self, _session_id: &str, _approval_id: i64, _claim: &str) -> String {
        String::new()
    }
}

fn write_pending_t2(store: &WeftStore, claim: &str) -> i64 {
    let (id, chosen, disposition) = wv_write_approval(
        store,
        ApprovalTier::T2,
        false,
        BlastRadius::Low,
        Confidence::Low, // one signal — T2 stands
        claim,
        None,
        None,
        None,
        &SilentNotifier,
    )
    .unwrap();
    assert_eq!(chosen, ApprovalTier::T2);
    assert_eq!(disposition, "pending");
    id
}

fn write_pending_t3(store: &WeftStore, claim: &str) -> i64 {
    // Two high-stakes signals force T3 (two-of-three rule); a session id is required.
    let (id, chosen, disposition) = wv_write_approval(
        store,
        ApprovalTier::T3,
        true,
        BlastRadius::High,
        Confidence::Low,
        claim,
        None,
        None,
        Some("sess-owner"),
        &SilentNotifier,
    )
    .unwrap();
    assert_eq!(chosen, ApprovalTier::T3);
    assert_eq!(disposition, "pending");
    id
}

#[test]
fn lists_only_pending_approvals() {
    let (_dir, store) = open_temp();
    let t2 = write_pending_t2(&store, "deploy the config");
    let t3 = write_pending_t3(&store, "drop the prod table");
    // Settle the T2 — it must drop out of the pending read.
    assert!(wv_settle_approval(&store, t2, "ratified").unwrap());

    let rows = wv_list_pending_approvals(&store, 1_000).unwrap();
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    assert_eq!(ids, vec![t3], "only the still-pending T3 remains");
    assert_eq!(rows[0].tier, "T3");
    assert_eq!(rows[0].claim, "drop the prod table");
    assert!(
        !rows[0].not_surfaced_but_expired,
        "a T3 has no revocation window — never carries"
    );
}

#[test]
fn maps_window_columns_for_an_open_t2() {
    let (_dir, store) = open_temp();
    let id = write_pending_t2(&store, "restart the worker");
    // Arm the window at t=100 for a 60s budget in session sess-A.
    wv_open_t2_window(&store, id, "sess-A", 60, 100).unwrap();

    // Read at t=130 — 30s elapsed, budget not spent → open, not carried.
    let rows = wv_list_pending_approvals(&store, 130).unwrap();
    let row = &rows[0];
    assert_eq!(row.tier, "T2");
    assert_eq!(row.window_state, "open");
    assert_eq!(row.duration_s, Some(60));
    assert_eq!(row.opened_at, Some(100));
    assert_eq!(row.pause_accumulated_s, 0);
    assert_eq!(row.surfaced_session.as_deref(), Some("sess-A"));
    assert!(!row.not_surfaced_but_expired, "budget not yet spent");
}

#[test]
fn carries_a_spent_still_pending_t2_window() {
    let (_dir, store) = open_temp();
    // A T2 that was NEVER opened (no window) — not a lapsed window, does not carry.
    let never_opened = write_pending_t2(&store, "never surfaced");
    // A T2 opened then left to spend its budget while still pending — it did not
    // auto-close, so it carries to the next session (AC-3's carry chip).
    let spent = write_pending_t2(&store, "opened then spent");
    wv_open_t2_window(&store, spent, "sess-A", 10, 100).unwrap();

    // Read well past the budget (t=1000).
    let rows = wv_list_pending_approvals(&store, 1_000).unwrap();
    let spent_row = rows.iter().find(|r| r.id == spent).unwrap();
    let never_row = rows.iter().find(|r| r.id == never_opened).unwrap();
    assert!(
        spent_row.not_surfaced_but_expired,
        "a spent, still-pending T2 window did not auto-close — it carries"
    );
    assert!(
        !never_row.not_surfaced_but_expired,
        "a never-opened window is not a lapsed window"
    );
}

#[test]
fn a_paused_t2_window_never_carries() {
    let (_dir, store) = open_temp();
    let id = write_pending_t2(&store, "paused away");
    wv_open_t2_window(&store, id, "sess-A", 10, 100).unwrap();
    // Owner goes away — pause the clock. Even read far past the budget, a paused
    // window is frozen and never carries.
    wv_pause_all_t2_windows(&store, 105).unwrap();

    let rows = wv_list_pending_approvals(&store, 10_000).unwrap();
    let row = rows.iter().find(|r| r.id == id).unwrap();
    assert_eq!(row.window_state, "paused");
    assert!(!row.not_surfaced_but_expired, "a paused window is frozen");
}
