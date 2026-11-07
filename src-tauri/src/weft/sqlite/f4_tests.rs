// SPDX-License-Identifier: MIT
//! SW-WV-F4 store-layer tests for the review-at-altitude read path
//! (`wv_read_pending_assertions`) and the idempotency guarantees the ratify /
//! revoke Tauri commands rely on (AC-5/AC-6/AC-7).
//!
//! The AppHandle-dependent command bodies (`wv_ratify`/`wv_revoke`/
//! `wv_read_pending_assertions` handlers, the `mcp:agent-message` note route, the
//! FE-event emit) need a Tauri `MockRuntime` harness that does not exist in this
//! repo (DW-WV-E1-1) — those paths are code-review-verified. Here we prove the
//! substance F4 owns at the store seam: only `pending` rows surface, a settle drops
//! a row from the pending list, and the disposition CAS is idempotent (a second
//! settle loses cleanly).
//!
//! Off-thread discipline: the Tauri commands dispatch these calls via
//! `await_blocking` (which wraps `spawn_blocking`) — the safe Tauri-command path;
//! no RI-2-ALLOW marker required here (`with_blocking(` is the scanned escape hatch,
//! not used at these sites).

use super::*;
use crate::weft::store::WeftStore;

fn open_temp() -> (tempfile::TempDir, WeftStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
    (dir, store)
}

fn insert_pending(store: &WeftStore, run_id: &str, claim: &str) -> i64 {
    crate::weft::wv_insert_assertion(store, "sess-1", Some(run_id), claim, "slack:#general", None)
        .unwrap()
}

/// AC-1: `wv_read_pending_assertions` returns every `pending` row for the run,
/// ordered by id, and filters to the requested run.
#[test]
fn read_pending_returns_only_this_runs_pending_rows() {
    let (_dir, store) = open_temp();
    let a = insert_pending(&store, "run-1", "PR opened");
    let b = insert_pending(&store, "run-1", "tests green");
    let _other = insert_pending(&store, "run-2", "different run");

    let rows = crate::weft::wv_read_pending_assertions(&store, "run-1").unwrap();
    assert_eq!(rows.len(), 2, "both run-1 pending assertions surface");
    assert_eq!(rows[0].id, a);
    assert_eq!(rows[1].id, b, "ordered by insert id");
    assert!(rows.iter().all(|r| r.disposition == "pending"));
    assert!(rows.iter().all(|r| r.run_id.as_deref() == Some("run-1")));
    assert!(!rows[0].posted_at.is_empty(), "posted_at is projected");
}

/// AC-5: after a ratify (CAS pending→ratified) the row leaves the pending list —
/// the header disappears. A settled row can never surface as still-needing-judgment.
#[test]
fn ratify_drops_the_row_from_pending() {
    let (_dir, store) = open_temp();
    let id = insert_pending(&store, "run-1", "PR opened");
    assert_eq!(
        crate::weft::wv_read_pending_assertions(&store, "run-1")
            .unwrap()
            .len(),
        1
    );

    let won = crate::weft::wv_set_assertion_disposition(&store, id, "pending", "ratified").unwrap();
    assert!(won, "the first ratify wins the CAS");
    assert!(
        crate::weft::wv_read_pending_assertions(&store, "run-1")
            .unwrap()
            .is_empty(),
        "a ratified assertion is no longer pending"
    );
}

/// AC-6: a revoke (CAS pending→revoked) likewise drops the row from pending.
#[test]
fn revoke_drops_the_row_from_pending() {
    let (_dir, store) = open_temp();
    let id = insert_pending(&store, "run-1", "PR opened");
    let won = crate::weft::wv_set_assertion_disposition(&store, id, "pending", "revoked").unwrap();
    assert!(won);
    let row = crate::weft::wv_get_assertion(&store, id).unwrap().unwrap();
    assert_eq!(row.disposition, "revoked");
    assert!(crate::weft::wv_read_pending_assertions(&store, "run-1")
        .unwrap()
        .is_empty());
}

/// AC-5/AC-7: the disposition CAS is idempotent — a second settle on an
/// already-settled row loses (`won == false`) and never re-mutates. This is the
/// store-layer proof behind the command's `alreadySettled` guard.
#[test]
fn second_settle_loses_the_cas_and_does_not_remutate() {
    let (_dir, store) = open_temp();
    let id = insert_pending(&store, "run-1", "PR opened");
    assert!(crate::weft::wv_set_assertion_disposition(&store, id, "pending", "ratified").unwrap());

    // A second ratify from `pending` loses — the row is no longer pending.
    let won_again =
        crate::weft::wv_set_assertion_disposition(&store, id, "pending", "ratified").unwrap();
    assert!(
        !won_again,
        "a settle on an already-ratified row loses the CAS"
    );

    // And a revoke-after-ratify from `pending` also loses — the row stays ratified.
    let revoke_won =
        crate::weft::wv_set_assertion_disposition(&store, id, "pending", "revoked").unwrap();
    assert!(!revoke_won);
    let row = crate::weft::wv_get_assertion(&store, id).unwrap().unwrap();
    assert_eq!(
        row.disposition, "ratified",
        "the disposition is never re-mutated"
    );
}
