// SPDX-License-Identifier: MIT
//! SW-WA-D4 store-level acceptance tests for the controller-scoped posture SOURCE:
//! the default-Normal read on a fresh store, the corrupt-value fail-open (never Away),
//! and the Away/Normal transition firing the single-sited Tier-2 pause/resume through
//! the sanctioned `wv_pause_all_t2_windows` / `wv_resume_all_t2_windows` — never a
//! direct `paused_at` write. The pure classifier decision paths (AC-1..AC-4) live in
//! `weft/postures.rs`; the seam wrap + park/spawn routing live in
//! `commands/weft_controller_run.rs`. These exercise the store contract the
//! `wv_set_posture` / `wv_read_posture` verbs call.

use super::*;
use crate::weft::spawn::WorkerSubclass;
use crate::weft::store::WeftStore;
use crate::weft::{
    BlastRadius, Confidence, Posture, PostureConfig, PostureWindow, T2Directive, TimeOfDayWindow,
};

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

/// Seed an OPEN, pending Tier-2 window (so a pause has something to stamp).
fn open_t2_window(store: &WeftStore, claim: &str, opened_at: i64) -> i64 {
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
    wv_open_t2_window(store, id, "sess-A", 600, opened_at).unwrap();
    id
}

fn paused_at_of(store: &WeftStore, id: i64) -> Option<i64> {
    store
        .conn()
        .unwrap()
        .query_row(
            "SELECT paused_at FROM wv_approvals WHERE id=?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap()
}

fn pause_accumulated_of(store: &WeftStore, id: i64) -> i64 {
    store
        .conn()
        .unwrap()
        .query_row(
            "SELECT pause_accumulated_s FROM wv_approvals WHERE id=?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap()
}

// ============================ AC-7: default + fail-open ============================

#[test]
fn a_fresh_store_reads_normal_with_no_windows() {
    // Before any wv_set_posture, the posture layer is inert: Normal, no widening
    // windows — the controller behaves exactly as the D1 gate + D2 tier alone.
    let (_dir, store) = open_temp();
    let cfg = wv_read_posture(&store).unwrap();
    assert_eq!(cfg.posture(), Posture::Normal);
    assert!(cfg.windows().is_empty());
}

#[test]
fn a_corrupt_stored_posture_reads_as_normal_never_away() {
    // A stored value that no longer parses must fail open to the attended baseline
    // (Normal, which still enforces D1/D2) — never Away, which could strand earned work.
    let (_dir, store) = open_temp();
    {
        let conn = store.conn().unwrap();
        conn.execute(
            "INSERT INTO wv_posture (id, posture, windows, updated_at) \
             VALUES (1, 'garbage', '[]', 100)",
            [],
        )
        .unwrap();
    }
    let cfg = wv_read_posture(&store).unwrap();
    assert_eq!(
        cfg.posture(),
        Posture::Normal,
        "a corrupt posture string falls back to Normal, never Away"
    );
}

#[test]
fn set_then_read_round_trips_the_stored_config() {
    let (_dir, store) = open_temp();
    let night = TimeOfDayWindow::new(22 * 60, 6 * 60);
    let cfg = PostureConfig::new(
        Posture::Away,
        vec![PostureWindow::new(WorkerSubclass::Review, night)],
    );
    wv_set_posture(&store, &cfg, 1000).unwrap();
    let read = wv_read_posture(&store).unwrap();
    assert_eq!(
        read, cfg,
        "the stored posture + windows round-trip through the store"
    );
}

// ============================ AC-5: transition pause/resume ============================

#[test]
fn away_enter_pauses_all_t2_windows_and_exit_resumes_them() {
    let (_dir, store) = open_temp();
    let id = open_t2_window(&store, "review PR #42", 1000);
    assert_eq!(paused_at_of(&store, id), None, "window runs before Away");

    // Normal → Away: PauseAll fires, stamping paused_at.
    let out = wv_set_posture(&store, &PostureConfig::bare(Posture::Away), 1100).unwrap();
    assert!(out.changed, "Normal → Away is a value change");
    assert_eq!(out.directive, Some(T2Directive::PauseAll));
    assert_eq!(out.windows_affected, 1, "the one live window was paused");
    assert_eq!(
        paused_at_of(&store, id),
        Some(1100),
        "the away transition stamped paused_at via the single-sited pause fn"
    );

    // Away → Normal: ResumeAll fires, banking the paused span and clearing paused_at.
    let out = wv_set_posture(&store, &PostureConfig::bare(Posture::Normal), 1160).unwrap();
    assert!(out.changed, "Away → Normal is a value change");
    assert_eq!(out.directive, Some(T2Directive::ResumeAll));
    assert_eq!(out.windows_affected, 1, "the one paused window resumed");
    assert_eq!(
        paused_at_of(&store, id),
        None,
        "resume cleared the pause marker"
    );
    assert_eq!(
        pause_accumulated_of(&store, id),
        60,
        "resume banked the 60s paused span (1160 - 1100) so the owner keeps full budget"
    );
}

#[test]
fn setting_the_same_posture_is_idempotent_and_fires_no_directive() {
    let (_dir, store) = open_temp();
    let id = open_t2_window(&store, "review PR #7", 1000);

    // First Away → Away change pauses.
    let first = wv_set_posture(&store, &PostureConfig::bare(Posture::Away), 1100).unwrap();
    assert!(first.changed);
    assert_eq!(paused_at_of(&store, id), Some(1100));

    // A SECOND set to Away is a no-change — it fires neither pause nor resume, and the
    // already-paused window is untouched (paused_at stays at the FIRST stamp).
    let second = wv_set_posture(&store, &PostureConfig::bare(Posture::Away), 1200).unwrap();
    assert!(
        !second.changed,
        "setting the current posture is a no-op transition"
    );
    assert_eq!(second.directive, None, "a no-change set fires no directive");
    assert_eq!(second.windows_affected, 0);
    assert_eq!(
        paused_at_of(&store, id),
        Some(1100),
        "the window's paused_at is untouched by the idempotent re-set"
    );
}

#[test]
fn set_posture_never_writes_paused_at_directly_only_the_single_sited_fns_do() {
    // Single-sited rule (ARCHITECTURE §4.1): the ONLY code that writes `paused_at` is
    // wv_pause_all_t2_windows / wv_resume_all_t2_windows. wv_set_posture must reach them
    // through the free functions, never with its own UPDATE. Grep the source of the
    // transition site to assert it names the sanctioned functions and no raw paused_at write.
    let src = include_str!("../sqlite.rs");
    // Extract the wv_set_posture body region: from its signature to the function's own
    // closing brace at column 0 (`\n}\n`), so the region is the body ALONE — not the
    // next function's doc comment (which the naive "to next pub fn" cut would include).
    let start = src
        .find("pub fn wv_set_posture(")
        .expect("wv_set_posture is defined");
    let rest = &src[start..];
    let end = rest
        .find("\n}\n")
        .map(|i| i + 3)
        .expect("wv_set_posture has a closing brace");
    let body = &rest[..end];
    assert!(
        body.contains("wv_pause_all_t2_windows") && body.contains("wv_resume_all_t2_windows"),
        "the transition fires the sanctioned single-sited pause/resume functions"
    );
    assert!(
        !body.contains("paused_at"),
        "wv_set_posture must NEVER write paused_at directly — only the single-sited fns do"
    );
}
