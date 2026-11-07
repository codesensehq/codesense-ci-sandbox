// SPDX-License-Identifier: MIT
//! SW-WV-E3 store-layer tests for the Tier-2 approval window.
//!
//! Covers the window lifecycle on `wv_approvals`:
//!   - open arms the surfaced-item guard (session + budget stamped);
//!   - the pure verdict (`wv_check_t2_window`) applies BOTH guards — presence-pause
//!     freezes the clock, and a spent budget only `Lapsed`s in the surfacing session;
//!   - auto-close ratifies ONLY a lapsed window in its surfacing session, and never
//!     clobbers an owner's explicit reply;
//!   - the single-sited pause/resume-all pair banks away-time so the owner keeps a
//!     full window of present time (ARCHITECTURE §4.1);
//!   - the migration heals an E1/E2-era db with defaults that can't false-ratify;
//!   - every mutation co-commits an audit row.
//!
//! Time is injected (`now: i64` epoch-seconds) so the elapsed math is deterministic
//! — no real clock, no sleeps.

use super::*;
use crate::weft::store::WeftStore;
use crate::weft::{BlastRadius, Confidence, T2WindowVerdict};

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

/// Insert a pending T2 approval (one risk signal so the picker keeps the preferred
/// T2) and return its id.
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

fn audit_count(store: &WeftStore, operation: &str) -> i64 {
    store
        .conn()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM wv_write_log WHERE tbl='wv_approvals' AND operation=?1",
            params![operation],
            |r| r.get(0),
        )
        .unwrap()
}

// ============================== migration ==============================

#[test]
fn e3_columns_heal_an_e2_era_db_and_cannot_false_ratify() {
    // An E1/E2-era db lacks the five E3 window columns. Simulate one, insert a
    // pending T2 WITHOUT them, then re-open (the E3 ensure_column path fires).
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("weft.sqlite3");
    {
        let conn = crate::shared::sqlite::open_wal_db(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE wv_approvals (\
                id INTEGER PRIMARY KEY AUTOINCREMENT, \
                tier TEXT, claim TEXT, posted_at TEXT, source TEXT, \
                window_state TEXT NOT NULL DEFAULT 'open', \
                disposition TEXT NOT NULL DEFAULT 'pending', \
                irreversible INTEGER NOT NULL DEFAULT 0, \
                blast_radius TEXT NOT NULL DEFAULT 'low', \
                confidence TEXT NOT NULL DEFAULT 'high');",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO wv_approvals (tier, claim, posted_at) VALUES ('T2','pre-e3','now')",
            [],
        )
        .unwrap();
    }
    let store = WeftStore::open(&path).unwrap();
    let id: i64 = store
        .conn()
        .unwrap()
        .query_row(
            "SELECT id FROM wv_approvals WHERE claim='pre-e3'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    // The pre-E3 row has a NULL window_opened_at, so it is NotOpen and can never
    // lapse into consent — no false auto-ratify of a legacy row.
    let verdict = wv_check_t2_window(&store, id, "any-session", 10_000).unwrap();
    assert_eq!(verdict, T2WindowVerdict::NotOpen);
    assert!(!wv_close_t2_window_if_lapsed(&store, id, "any-session", 10_000).unwrap());
    assert_eq!(disposition_of(&store, id), "pending");
    // pause_accumulated_s defaults to 0.
    let banked: i64 = store
        .conn()
        .unwrap()
        .query_row(
            "SELECT pause_accumulated_s FROM wv_approvals WHERE id=?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(banked, 0);
}

// ============================== open ==============================

#[test]
fn open_arms_the_window_and_rejects_bad_input() {
    let (_d, store) = open_temp();
    let id = write_pending_t2(&store, "deploy staging");
    assert!(wv_open_t2_window(&store, id, "sess-A", 60, 1000).unwrap());
    assert_eq!(
        audit_count(&store, "open_window"),
        1,
        "open co-commits audit"
    );

    // Blank session + non-positive duration are refusals.
    let id2 = write_pending_t2(&store, "other");
    assert!(wv_open_t2_window(&store, id2, "  ", 60, 1000).is_err());
    assert!(wv_open_t2_window(&store, id2, "sess", 0, 1000).is_err());
    assert!(wv_open_t2_window(&store, id2, "sess", -5, 1000).is_err());
}

#[test]
fn reopening_a_live_window_is_refused_and_preserves_banking() {
    // Opening is a one-time arm: a second open on an already-opened window must NOT
    // clobber the opened-at / duration / banked pause (which would silently reset the
    // owner's window).
    let (_d, store) = open_temp();
    let id = write_pending_t2(&store, "reopen");
    wv_open_t2_window(&store, id, "sess-A", 100, 1000).unwrap();
    // Bank 80s of away time.
    wv_pause_all_t2_windows(&store, 1050).unwrap();
    wv_resume_all_t2_windows(&store, 1130).unwrap();
    // A second open is refused (window already armed).
    assert!(wv_open_t2_window(&store, id, "sess-B", 999, 1170).is_err());
    // The original window survives: 50 present seconds before the pause (1000..1050)
    // plus 40 after the resume (1130..1170) → elapsed=90 against the original 100s
    // duration, NOT reset to a fresh 0 by the refused reopen.
    match wv_check_t2_window(&store, id, "sess-A", 1170).unwrap() {
        T2WindowVerdict::Open {
            elapsed_s,
            remaining_s,
        } => {
            assert_eq!(elapsed_s, 90, "the original opened-at + banking survived");
            assert_eq!(remaining_s, 10);
        }
        other => panic!("expected Open, got {other:?}"),
    }
}

#[test]
fn surfaced_session_is_compared_trimmed() {
    // A caller passing a padded session must still lapse in the (trimmed) same
    // session — the stored + checked forms are both canonicalised.
    let (_d, store) = open_temp();
    let id = write_pending_t2(&store, "trim");
    wv_open_t2_window(&store, id, "  sess-A  ", 60, 1000).unwrap();
    assert_eq!(
        wv_check_t2_window(&store, id, "sess-A", 2000).unwrap(),
        T2WindowVerdict::Lapsed { elapsed_s: 1000 },
    );
}

#[test]
fn open_rejects_a_non_t2_or_settled_row() {
    let (_d, store) = open_temp();
    // A T1 auto-ratifies — no window to open.
    let (t1, _t, _d2) = wv_write_approval(
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
    assert!(wv_open_t2_window(&store, t1, "sess", 60, 1000).is_err());

    // A T2 that was already settled cannot be opened.
    let t2 = write_pending_t2(&store, "settled");
    assert!(wv_settle_approval(&store, t2, "revoked").unwrap());
    assert!(wv_open_t2_window(&store, t2, "sess", 60, 1000).is_err());
}

// ============================== check: the two guards ==============================

#[test]
fn open_window_within_budget_reports_open() {
    let (_d, store) = open_temp();
    let id = write_pending_t2(&store, "c");
    wv_open_t2_window(&store, id, "sess-A", 100, 1000).unwrap();
    // 40s in — still open, 60s remaining.
    match wv_check_t2_window(&store, id, "sess-A", 1040).unwrap() {
        T2WindowVerdict::Open {
            elapsed_s,
            remaining_s,
        } => {
            assert_eq!(elapsed_s, 40);
            assert_eq!(remaining_s, 60);
        }
        other => panic!("expected Open, got {other:?}"),
    }
}

#[test]
fn expired_in_surfacing_session_lapses_but_wrong_session_is_guarded() {
    let (_d, store) = open_temp();
    let id = write_pending_t2(&store, "surfaced-guard");
    wv_open_t2_window(&store, id, "sess-A", 60, 1000).unwrap();

    // Budget spent, but the check runs in a DIFFERENT session — the surfaced-item
    // guard blocks the close.
    assert_eq!(
        wv_check_t2_window(&store, id, "sess-B", 2000).unwrap(),
        T2WindowVerdict::ExpiredWrongSession { elapsed_s: 1000 },
    );
    // Same budget, the SURFACING session — now it may lapse.
    assert_eq!(
        wv_check_t2_window(&store, id, "sess-A", 2000).unwrap(),
        T2WindowVerdict::Lapsed { elapsed_s: 1000 },
    );
}

#[test]
fn paused_window_freezes_the_clock() {
    let (_d, store) = open_temp();
    let id = write_pending_t2(&store, "away");
    wv_open_t2_window(&store, id, "sess-A", 100, 1000).unwrap();
    // Owner goes away at t=1050 (50s elapsed).
    assert_eq!(wv_pause_all_t2_windows(&store, 1050).unwrap(), 1);
    // Much later — still paused, elapsed frozen at 50, never lapses.
    match wv_check_t2_window(&store, id, "sess-A", 99_999).unwrap() {
        T2WindowVerdict::Paused {
            elapsed_s,
            remaining_s,
        } => {
            assert_eq!(elapsed_s, 50, "clock frozen at the pause instant");
            assert_eq!(remaining_s, 50);
        }
        other => panic!("expected Paused, got {other:?}"),
    }
    assert!(
        !wv_close_t2_window_if_lapsed(&store, id, "sess-A", 99_999).unwrap(),
        "a paused window never auto-closes"
    );
}

// ============================== auto-close ==============================

#[test]
fn close_ratifies_only_a_lapsed_surfacing_session_window() {
    let (_d, store) = open_temp();
    let id = write_pending_t2(&store, "lazy consent");
    wv_open_t2_window(&store, id, "sess-A", 60, 1000).unwrap();

    // Wrong session: no-op.
    assert!(!wv_close_t2_window_if_lapsed(&store, id, "sess-B", 2000).unwrap());
    assert_eq!(disposition_of(&store, id), "pending");

    // Surfacing session, budget spent: ratify.
    assert!(wv_close_t2_window_if_lapsed(&store, id, "sess-A", 2000).unwrap());
    assert_eq!(disposition_of(&store, id), "ratified");
    assert_eq!(audit_count(&store, "window_lapsed_ratify"), 1);

    // Idempotent — a second close is a no-op.
    assert!(!wv_close_t2_window_if_lapsed(&store, id, "sess-A", 2000).unwrap());
}

#[test]
fn owner_revoke_beats_the_window_close() {
    // The wall never moves: an owner's explicit revoke wins over a lapse.
    let (_d, store) = open_temp();
    let id = write_pending_t2(&store, "revoked before lapse");
    wv_open_t2_window(&store, id, "sess-A", 60, 1000).unwrap();
    // Owner revokes.
    assert!(wv_settle_approval(&store, id, "revoked").unwrap());
    // The window can no longer auto-ratify — the pending guard fails.
    assert!(!wv_close_t2_window_if_lapsed(&store, id, "sess-A", 2000).unwrap());
    assert_eq!(disposition_of(&store, id), "revoked");
}

// ============================== pause / resume banking ==============================

#[test]
fn resume_banks_away_time_so_owner_keeps_full_present_window() {
    let (_d, store) = open_temp();
    let id = write_pending_t2(&store, "present-time");
    wv_open_t2_window(&store, id, "sess-A", 100, 1000).unwrap();
    // Present 0..30 (t=1030), away 30s..(t=1030->1130), back at t=1130.
    assert_eq!(wv_pause_all_t2_windows(&store, 1030).unwrap(), 1);
    assert_eq!(wv_resume_all_t2_windows(&store, 1130).unwrap(), 1);
    // 30 present seconds consumed, 100 away-seconds banked. At t=1170 (another 40
    // present), elapsed=70, still open — the away time did NOT eat the window.
    match wv_check_t2_window(&store, id, "sess-A", 1170).unwrap() {
        T2WindowVerdict::Open {
            elapsed_s,
            remaining_s,
        } => {
            assert_eq!(elapsed_s, 70);
            assert_eq!(remaining_s, 30);
        }
        other => panic!("expected Open, got {other:?}"),
    }
    // 30 more present seconds (t=1200): now lapsed in the surfacing session.
    assert_eq!(
        wv_check_t2_window(&store, id, "sess-A", 1200).unwrap(),
        T2WindowVerdict::Lapsed { elapsed_s: 100 },
    );
}

#[test]
fn pause_and_resume_all_are_idempotent_and_batch() {
    let (_d, store) = open_temp();
    let a = write_pending_t2(&store, "a");
    let b = write_pending_t2(&store, "b");
    wv_open_t2_window(&store, a, "sess-A", 100, 1000).unwrap();
    wv_open_t2_window(&store, b, "sess-B", 100, 1000).unwrap();

    // Both paused in one call.
    assert_eq!(wv_pause_all_t2_windows(&store, 1050).unwrap(), 2);
    // A second pause is a no-op (already paused).
    assert_eq!(wv_pause_all_t2_windows(&store, 1060).unwrap(), 0);
    assert_eq!(
        audit_count(&store, "pause_all_t2"),
        1,
        "only the batch that changed rows audits"
    );

    // Both resumed in one call; a second resume is a no-op.
    assert_eq!(wv_resume_all_t2_windows(&store, 1200).unwrap(), 2);
    assert_eq!(wv_resume_all_t2_windows(&store, 1300).unwrap(), 0);
    assert_eq!(audit_count(&store, "resume_all_t2"), 1);
}

#[test]
fn pause_all_ignores_unopened_and_settled_windows() {
    let (_d, store) = open_temp();
    let opened = write_pending_t2(&store, "opened");
    wv_open_t2_window(&store, opened, "sess-A", 100, 1000).unwrap();
    // An un-opened pending T2 (window_opened_at NULL) is not a live window.
    let _unopened = write_pending_t2(&store, "unopened");
    // A settled T2.
    let settled = write_pending_t2(&store, "settled");
    wv_open_t2_window(&store, settled, "sess-A", 100, 1000).unwrap();
    wv_settle_approval(&store, settled, "revoked").unwrap();

    // Only the one live, opened, pending window pauses.
    assert_eq!(wv_pause_all_t2_windows(&store, 1050).unwrap(), 1);
}
