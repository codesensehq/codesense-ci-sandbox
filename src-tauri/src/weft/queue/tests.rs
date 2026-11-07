// SPDX-License-Identifier: MIT
//! SW-WV-B2 acceptance tests for the `wv_queue` lifecycle + `wv_key_ledger`.
//!
//! Covers AC-1 (idempotent forward-migrate from the B1 stub), AC-2 (live-max ids
//! under concurrency), AC-3 (four-state machine + single-owner CAS + non-owner
//! resolve refusal), AC-4 (prune-on-resolve + atomic key-ledger, incl. a
//! mid-transaction rollback), AC-5 (dedupe), AC-6 (scope immutable across claim),
//! AC-7 (off-thread: all four queue ops complete through `await_blocking` on a
//! single-threaded runtime — a direct blocking DB call on the reactor would
//! starve it and hang), and AC-9 (`queue_list` scoped/unscoped). AC-8 (capability
//! gating) lives in `commands/mcp_scope.rs` alongside the pack fixtures.

use std::sync::{Arc, Mutex};

use rusqlite::{params, TransactionBehavior};

use super::*;
use crate::weft::store::WeftStore;

fn open_temp() -> (tempfile::TempDir, WeftStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
    (dir, store)
}

fn read_state(store: &WeftStore, id: i64) -> Option<String> {
    let conn = store.conn().unwrap();
    conn.query_row("SELECT state FROM wv_queue WHERE id=?1", params![id], |r| {
        r.get(0)
    })
    .ok()
}

fn read_scope(store: &WeftStore, id: i64) -> Option<String> {
    let conn = store.conn().unwrap();
    conn.query_row("SELECT scope FROM wv_queue WHERE id=?1", params![id], |r| {
        r.get(0)
    })
    .ok()
    .flatten()
}

fn queue_row_count(store: &WeftStore, id: i64) -> i64 {
    let conn = store.conn().unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM wv_queue WHERE id=?1",
        params![id],
        |r| r.get(0),
    )
    .unwrap()
}

fn ledger_row(store: &WeftStore, queue_id: i64) -> Option<(i64, Option<String>)> {
    let conn = store.conn().unwrap();
    conn.query_row(
        "SELECT queue_id, ref FROM wv_key_ledger WHERE queue_id=?1",
        params![queue_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .ok()
}

// ---------- AC-1: idempotent create + forward-migrate from the B1 stub ----------

#[test]
fn queue_and_key_ledger_created_and_forward_migrated() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("weft.sqlite3");

    // Simulate a pre-existing B1-stub DB: `wv_queue(id)` only, with a row that must
    // survive the B2 forward-migration.
    {
        let conn = crate::shared::sqlite::open_wal_db(&path).unwrap();
        conn.execute_batch("CREATE TABLE IF NOT EXISTS wv_queue (id INTEGER PRIMARY KEY);")
            .unwrap();
        conn.execute("INSERT INTO wv_queue (id) VALUES (7)", [])
            .unwrap();
    }

    // First B2 open heals the columns + creates wv_key_ledger; the stub row survives.
    let store = WeftStore::open(&path).unwrap();
    let survived: i64 = store
        .conn()
        .unwrap()
        .query_row("SELECT id FROM wv_queue WHERE id=7", [], |r| r.get(0))
        .unwrap();
    assert_eq!(survived, 7, "the B1-stub row survives forward-migration");
    // wv_key_ledger now exists.
    store
        .conn()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM wv_key_ledger", [], |r| {
            r.get::<_, i64>(0)
        })
        .expect("wv_key_ledger created on B2 open");

    // Second open is a no-op that neither errors nor loses data (idempotent).
    let store2 = WeftStore::open(&path).unwrap();
    let still: i64 = store2
        .conn()
        .unwrap()
        .query_row("SELECT id FROM wv_queue WHERE id=7", [], |r| r.get(0))
        .unwrap();
    assert_eq!(still, 7, "a second open preserves all rows");
}

// ---------- AC-2: live-max ids, no duplicates under concurrent enqueue ----------

#[test]
fn concurrent_enqueue_mints_distinct_sequential_ids() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("weft.sqlite3");
    WeftStore::open(&path).unwrap();

    const N: usize = 10;
    let ids: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let mut handles = Vec::new();
    for i in 0..N {
        let path = path.clone();
        let ids = Arc::clone(&ids);
        handles.push(std::thread::spawn(move || {
            let store = WeftStore::open(&path).unwrap();
            let id = enqueue(
                &store,
                &format!("src-{i}"),
                None,
                None,
                None,
                None,
                None,
                100,
            )
            .unwrap();
            ids.lock().unwrap().push(id);
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let mut got = ids.lock().unwrap().clone();
    got.sort_unstable();
    assert_eq!(got.len(), N, "every enqueue returned an id");
    assert_eq!(
        got,
        (1..=N as i64).collect::<Vec<_>>(),
        "ten concurrent enqueues → ten distinct sequential ids, no gaps, no dups"
    );
}

// ---------- AC-3: four-state machine, single owner, non-owner refusal ----------

#[test]
fn four_state_lifecycle_pending_claimed_dispatched_resolved() {
    let (_dir, store) = open_temp();
    let id = enqueue(&store, "gh", None, Some("pr#1"), None, None, None, 100).unwrap();
    assert_eq!(read_state(&store, id).as_deref(), Some("pending"));

    let claimed = claim(&store, "ctl-A", None, 200).unwrap();
    assert_eq!(claimed, Some(id), "the pending row is claimed");
    assert_eq!(read_state(&store, id).as_deref(), Some("claimed"));

    // Simulate SW-WV-C1's dispatch stamp via the test seam.
    mark_dispatched_for_test(&store, id, "ctl-A", None, 300).unwrap();
    assert_eq!(read_state(&store, id).as_deref(), Some("dispatched"));

    // Resolve from `dispatched`: row pruned + key-ledger written.
    let out = resolve(&store, id, "ctl-A", 400).unwrap();
    assert!(out.is_ok(), "resolve from dispatched succeeds");
    assert_eq!(queue_row_count(&store, id), 0, "row pruned on resolve");
    let led = ledger_row(&store, id).expect("key-ledger row present");
    assert_eq!(led.0, id);
    assert_eq!(
        led.1.as_deref(),
        Some("pr#1"),
        "ref carried into the ledger"
    );
}

#[test]
fn resolve_from_claimed_when_no_dispatch_occurred() {
    let (_dir, store) = open_temp();
    let id = enqueue(&store, "gh", None, Some("issue#9"), None, None, None, 100).unwrap();
    claim(&store, "ctl-A", None, 200).unwrap();
    // No dispatch — resolve directly from `claimed`.
    let out = resolve(&store, id, "ctl-A", 400).unwrap();
    assert!(out.is_ok(), "resolve from claimed (no dispatch) succeeds");
    assert_eq!(queue_row_count(&store, id), 0);
    assert_eq!(
        ledger_row(&store, id).unwrap().1.as_deref(),
        Some("issue#9")
    );
}

#[test]
fn concurrent_claim_one_wins_other_gets_claim_lost_or_none() {
    // Two claimers race a single pending row: exactly one claims it; the other
    // either finds nothing (`Ok(None)`) or exhausts its retry budget
    // (`ClaimLost`) — never a second successful claim on the same row.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("weft.sqlite3");
    let store = WeftStore::open(&path).unwrap();
    let id = enqueue(&store, "gh", None, None, None, None, None, 100).unwrap();

    let p1 = path.clone();
    let p2 = path.clone();
    let h1 = std::thread::spawn(move || {
        let s = WeftStore::open(&p1).unwrap();
        claim(&s, "ctl-A", None, 200)
    });
    let h2 = std::thread::spawn(move || {
        let s = WeftStore::open(&p2).unwrap();
        claim(&s, "ctl-B", None, 200)
    });
    let r1 = h1.join().unwrap();
    let r2 = h2.join().unwrap();

    let wins = [&r1, &r2]
        .iter()
        .filter(|r| matches!(r, Ok(Some(x)) if *x == id))
        .count();
    assert_eq!(wins, 1, "exactly one claimer wins the single pending row");
    // The claimed_by of the row is one of the two, and the row is `claimed`.
    assert_eq!(read_state(&store, id).as_deref(), Some("claimed"));
}

#[test]
fn claim_returns_none_on_empty_queue() {
    let (_dir, store) = open_temp();
    assert_eq!(claim(&store, "ctl-A", None, 200).unwrap(), None);
}

#[test]
fn sw_av_27_two_session_race_is_arbitrated_by_cas_with_no_lease() {
    // SW-AV-27 seam: after the lease/controller was deleted, the durable-store CAS
    // (`wv_mutate_owned`, `changes()==1`) is the ONLY coordination. Two sessions race
    // one pending row with NO lease anywhere in the picture: exactly one wins the CAS
    // and the other is a clean no-op (`ClaimLost`/`None`, never a panic, never a second
    // claim). A drive verb (`claim`) called with no lease succeeds — it is ungated. This
    // is what makes "any session drives, no controller" safe.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("weft.sqlite3");
    let store = WeftStore::open(&path).unwrap();
    let id = enqueue(&store, "gh", None, None, None, None, None, 100).unwrap();

    // A lone claim with no lease/session authority SUCCEEDS (ungated drive verb).
    let winner = claim(&store, "session-A", None, 200).unwrap();
    assert_eq!(
        winner,
        Some(id),
        "an ungated claim (no lease) wins the pending row"
    );
    assert_eq!(read_state(&store, id).as_deref(), Some("claimed"));

    // A second session racing the SAME (now-claimed) row is a clean no-op via the CAS —
    // it finds nothing runnable, never a second successful claim on the same row.
    let loser = claim(&store, "session-B", None, 210).unwrap();
    assert_eq!(
        loser, None,
        "the second session no-ops cleanly — CAS already gave the row away"
    );
}

#[test]
fn non_owner_resolve_is_refused_and_row_unchanged() {
    let (_dir, store) = open_temp();
    let id = enqueue(&store, "gh", None, Some("pr#2"), None, None, None, 100).unwrap();
    claim(&store, "ctl-A", None, 200).unwrap();
    // caller-B is not the owner.
    let out = resolve(&store, id, "ctl-B", 400).unwrap();
    assert_eq!(out, Err(QueueError::OwnerMismatch));
    assert_eq!(
        read_state(&store, id).as_deref(),
        Some("claimed"),
        "row is unchanged after a non-owner resolve"
    );
    assert!(
        ledger_row(&store, id).is_none(),
        "no key-ledger row on a refused resolve"
    );
}

#[test]
fn resolving_a_pending_row_without_claiming_is_refused() {
    let (_dir, store) = open_temp();
    let id = enqueue(&store, "gh", None, None, None, None, None, 100).unwrap();
    // Never claimed → still pending. Resolve must refuse InvalidState.
    let out = resolve(&store, id, "ctl-A", 400).unwrap();
    assert_eq!(out, Err(QueueError::InvalidState));
    assert_eq!(read_state(&store, id).as_deref(), Some("pending"));
    assert_eq!(queue_row_count(&store, id), 1, "the pending row is intact");
}

// ---------- AC-4: prune-on-resolve + atomic key-ledger, incl. rollback ----------

#[test]
fn resolve_prunes_row_and_writes_key_ledger_atomically() {
    let (_dir, store) = open_temp();
    let id = enqueue(&store, "gh", None, Some("pr#42"), None, None, None, 100).unwrap();
    claim(&store, "ctl-A", None, 200).unwrap();
    resolve(&store, id, "ctl-A", 400).unwrap().unwrap();
    assert_eq!(queue_row_count(&store, id), 0, "queue row absent");
    let led = ledger_row(&store, id).expect("key-ledger row present");
    assert_eq!(led.0, id);
    assert_eq!(led.1.as_deref(), Some("pr#42"));
}

#[test]
fn mid_transaction_abort_leaves_no_half_prune() {
    // AC-4 path 2: manually replicate the resolve's DELETE-then-INSERT inside one
    // IMMEDIATE transaction, but abort (drop without commit) after the DELETE and
    // before the INSERT. The transaction boundary must roll the DELETE back — the
    // queue row survives and the key-ledger stays empty (no half-state).
    let (_dir, store) = open_temp();
    let id = enqueue(&store, "gh", None, Some("pr#7"), None, None, None, 100).unwrap();
    claim(&store, "ctl-A", None, 200).unwrap();

    {
        let mut conn = store.conn().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        tx.execute("DELETE FROM wv_queue WHERE id=?1", params![id])
            .unwrap();
        // Inject the abort: return before the INSERT + commit; the tx drops here,
        // rolling the DELETE back.
    }

    assert_eq!(
        queue_row_count(&store, id),
        1,
        "the DELETE rolled back — queue row still present (no half-prune)"
    );
    assert!(
        ledger_row(&store, id).is_none(),
        "the key-ledger row is absent — the abort left both tables in their pre-call state"
    );
}

// ---------- AC-5: dedupe protection via dedupe_key ----------

#[test]
fn duplicate_enqueue_with_same_dedupe_key_returns_existing_id() {
    let (_dir, store) = open_temp();
    let a = enqueue(&store, "gh", None, None, None, Some("k1"), None, 100).unwrap();
    let b = enqueue(&store, "gh", None, None, None, Some("k1"), None, 101).unwrap();
    assert_eq!(a, b, "second enqueue with the same key returns the same id");
    let conn = store.conn().unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wv_queue WHERE dedupe_key='k1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "only one row exists for the dedupe key");
}

#[test]
fn dedupe_key_is_reusable_after_the_row_resolves() {
    let (_dir, store) = open_temp();
    let a = enqueue(&store, "gh", None, Some("r1"), None, Some("k2"), None, 100).unwrap();
    claim(&store, "ctl-A", None, 200).unwrap();
    resolve(&store, a, "ctl-A", 300).unwrap().unwrap();
    // The live row is pruned, so the key is free again: a NEW pending row is
    // created (not the suppressed-existing-id short-circuit). The minted id may
    // reuse the freed integer — live-max is the pruneable-table contract
    // (ARCHITECTURE §1.4) — so we assert a fresh pending row exists, not a
    // distinct id.
    let b = enqueue(&store, "gh", None, Some("r2"), None, Some("k2"), None, 400).unwrap();
    assert_eq!(
        read_state(&store, b).as_deref(),
        Some("pending"),
        "a new pending row is enqueued once the prior row resolved"
    );
    let conn = store.conn().unwrap();
    let refs: Option<String> = conn
        .query_row("SELECT refs FROM wv_queue WHERE id=?1", params![b], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        refs.as_deref(),
        Some("r2"),
        "the new row carries the second enqueue's payload, not the resolved one's"
    );
}

#[test]
fn dedupe_only_suppresses_while_a_live_row_exists() {
    // A claimed (still live) row also suppresses a duplicate enqueue — in-flight
    // work is not re-queued.
    let (_dir, store) = open_temp();
    let a = enqueue(&store, "gh", None, None, None, Some("k3"), None, 100).unwrap();
    claim(&store, "ctl-A", None, 200).unwrap();
    let b = enqueue(&store, "gh", None, None, None, Some("k3"), None, 300).unwrap();
    assert_eq!(a, b, "a claimed (live) row still suppresses a re-enqueue");
}

// ---------- AC-6: scope is stamped by the enqueuer, immutable across claim ----

#[test]
fn scope_survives_a_claim_unchanged() {
    let (_dir, store) = open_temp();
    let id = enqueue(&store, "gh", Some("my-epic"), None, None, None, None, 100).unwrap();
    assert_eq!(read_scope(&store, id).as_deref(), Some("my-epic"));
    claim(&store, "ctl-A", None, 200).unwrap();
    assert_eq!(
        read_scope(&store, id).as_deref(),
        Some("my-epic"),
        "the claimer must not overwrite scope"
    );
}

#[test]
fn scoped_claim_only_picks_matching_scope() {
    let (_dir, store) = open_temp();
    let _other = enqueue(&store, "gh", Some("epic-a"), None, None, None, None, 100).unwrap();
    let mine = enqueue(&store, "gh", Some("epic-b"), None, None, None, None, 101).unwrap();
    let claimed = claim(&store, "ctl-B", Some("epic-b"), 200).unwrap();
    assert_eq!(claimed, Some(mine), "a scoped claim picks only its scope");
    assert_eq!(read_state(&store, mine).as_deref(), Some("claimed"));
}

// ---------- AC-9: queue_list scoped/unscoped, resolved rows excluded ----------

#[test]
fn queue_list_returns_non_resolved_rows_scoped_and_unscoped() {
    let (_dir, store) = open_temp();
    let a = enqueue(&store, "gh", Some("x"), None, None, None, None, 100).unwrap();
    let b = enqueue(&store, "gh", Some("y"), None, None, None, None, 101).unwrap();
    // Claim one so the list spans multiple states.
    claim(&store, "ctl-A", Some("x"), 200).unwrap();

    let all = queue_list(&store, None).unwrap();
    let ids: Vec<i64> = all.iter().map(|r| r.id).collect();
    assert!(
        ids.contains(&a) && ids.contains(&b),
        "unscoped list has both"
    );

    let scoped = queue_list(&store, Some("x")).unwrap();
    assert_eq!(scoped.len(), 1, "scoped list has only the x-scoped row");
    assert_eq!(scoped[0].id, a);
    assert_eq!(scoped[0].state, "claimed");

    // Resolve the claimed row; it drops out of both lists.
    resolve(&store, a, "ctl-A", 300).unwrap().unwrap();
    let after = queue_list(&store, None).unwrap();
    let after_ids: Vec<i64> = after.iter().map(|r| r.id).collect();
    assert!(
        !after_ids.contains(&a) && after_ids.contains(&b),
        "a resolved row does not appear in the list"
    );
    assert!(
        queue_list(&store, Some("x")).unwrap().is_empty(),
        "the x-scope now has no non-resolved rows"
    );
}

#[test]
fn max_id_tracks_the_live_max() {
    let (_dir, store) = open_temp();
    assert_eq!(max_id(&store).unwrap(), 0, "empty queue → max 0");
    enqueue(&store, "gh", None, None, None, None, None, 100).unwrap();
    enqueue(&store, "gh", None, None, None, None, None, 101).unwrap();
    assert_eq!(max_id(&store).unwrap(), 2);
}

// ---------- AC-7: every queue op completes off-thread, no reactor deadlock ----

#[tokio::test(flavor = "current_thread")]
async fn all_four_queue_ops_run_off_thread_without_deadlock() {
    // The handlers wrap each DB op in `commands::await_blocking` (spawn_blocking)
    // exactly as `signal_done` does (handler.rs:4100). On a single-threaded
    // (`current_thread`) runtime, a synchronous DB call made DIRECTLY on the
    // reactor would monopolize the one worker and this test would hang; routing it
    // through `spawn_blocking` frees the reactor. Driving all four ops through the
    // real `await_blocking` wrapper here proves the off-thread contract the
    // handlers rely on (a direct blocking call would time the test out).
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("weft.sqlite3");
    WeftStore::open(&path).unwrap();

    // enqueue
    let p = path.clone();
    let id = crate::commands::await_blocking("test.wv_enqueue", move || {
        let s = WeftStore::open(&p).map_err(|e| e.to_string())?;
        enqueue(&s, "gh", Some("s"), Some("r"), None, None, None, 100).map_err(|e| e.to_string())
    })
    .await
    .unwrap();
    assert!(id >= 1);

    // claim
    let p = path.clone();
    let claimed = crate::commands::await_blocking("test.wv_claim", move || {
        let s = WeftStore::open(&p).map_err(|e| e.to_string())?;
        claim(&s, "ctl-A", None, 200).map_err(|e| e.to_string())
    })
    .await
    .unwrap();
    assert_eq!(claimed, Some(id));

    // list
    let p = path.clone();
    let rows = crate::commands::await_blocking("test.wv_list", move || {
        let s = WeftStore::open(&p).map_err(|e| e.to_string())?;
        queue_list(&s, None).map_err(|e| e.to_string())
    })
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);

    // resolve
    let p = path.clone();
    let resolved = crate::commands::await_blocking("test.wv_resolve", move || {
        let s = WeftStore::open(&p).map_err(|e| e.to_string())?;
        resolve(&s, id, "ctl-A", 300).map_err(|e| e.to_string())
    })
    .await
    .unwrap();
    assert!(resolved.is_ok(), "resolve completed off-thread");
    let store = WeftStore::open(&path).unwrap();
    assert_eq!(
        queue_list(&store, None).unwrap().len(),
        0,
        "the row was pruned by the off-thread resolve"
    );
}

// ============================================================================
// SW-WV-C2 — dispatch tripwire wiring (claim-time verdict + mid-flight cycle +
// prune-preserves-verdict). The PURE five-row logic is unit-tested in
// `weft/tripwire.rs`; these exercise the store transactions (AC-2/3/4/5/8).
// ============================================================================

use crate::weft::tripwire::{Lifecycle, TouchedPath, TripwireInput};

/// Read the (tripwire_verdict, verify_fix_cycles) pair for a queue row.
fn read_tripwire(store: &WeftStore, id: i64) -> Option<(String, i64)> {
    let conn = store.conn().unwrap();
    conn.query_row(
        "SELECT tripwire_verdict, verify_fix_cycles FROM wv_queue WHERE id=?1",
        params![id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .ok()
}

fn read_claimed_by(store: &WeftStore, id: i64) -> Option<String> {
    let conn = store.conn().unwrap();
    conn.query_row(
        "SELECT claimed_by FROM wv_queue WHERE id=?1",
        params![id],
        |r| r.get(0),
    )
    .ok()
    .flatten()
}

/// Count `wv_write_log` MUTATE audit rows for a given queue row id (AC-8 evidence).
/// Filtered to `operation='mutate'` so `enqueue`'s earlier `append` audit row does
/// not inflate the count — this asserts specifically that each claim/report
/// state-transition wrote its own audit row in the same transaction.
fn audit_mutations_for(store: &WeftStore, id: i64) -> i64 {
    let conn = store.conn().unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM wv_write_log \
         WHERE tbl='wv_queue' AND row_id=?1 AND operation='mutate'",
        params![id],
        |r| r.get(0),
    )
    .unwrap()
}

/// A Keep-shaped tripwire input (single non-code path, no create, few lines).
fn keep_input() -> TripwireInput {
    TripwireInput {
        scope: Some("root".into()),
        touched_paths: vec![TouchedPath {
            path: "docs/x.md".into(),
            is_create: false,
            is_ledger: false,
        }],
        added_lines: 5,
        lifecycle: Lifecycle::Persistent,
        requires_diff_read: false,
        verify_fix_cycles: 0,
    }
}

/// A Delegate-shaped tripwire input (a .rs create → Row 2 fires).
fn delegate_input() -> TripwireInput {
    TripwireInput {
        scope: Some("root".into()),
        touched_paths: vec![TouchedPath {
            path: "src/new.rs".into(),
            is_create: true,
            is_ledger: false,
        }],
        added_lines: 5,
        lifecycle: Lifecycle::Persistent,
        requires_diff_read: false,
        verify_fix_cycles: 0,
    }
}

// AC-3 / AC-2 / AC-8 — a kept claim stamps the verified claimant +
// tripwire_verdict='keep' atomically, and writes exactly one audit row.
#[test]
fn claim_with_tripwire_keep_stamps_claimant() {
    let (_dir, store) = open_temp();
    let id = enqueue(&store, "gh", None, None, None, None, None, 100).unwrap();
    let claim = claim_with_tripwire(&store, &keep_input(), "run-99", None, 200)
        .unwrap()
        .expect("a pending row was claimed");
    assert_eq!(claim.id, id);
    assert_eq!(claim.claimed_by, "run-99");
    assert_eq!(read_claimed_by(&store, id).as_deref(), Some("run-99"));
    let (verdict, _) = read_tripwire(&store, id).unwrap();
    assert_eq!(
        verdict, "keep",
        "verdict stamped in the claim UPDATE (AC-2)"
    );
    assert_eq!(read_state(&store, id).as_deref(), Some("claimed"));
    // A kept claim records no fired row.
    assert_eq!(claim.verdict.fired_row, None);
    // AC-8: exactly one MUTATE audit row for the claim transition (the enqueue's
    // own `append` audit row is excluded by the operation filter).
    assert_eq!(audit_mutations_for(&store, id), 1);
}

// AC-3 / AC-2 — a delegated claim stamps the verified claimant + tripwire_verdict='delegate'
// atomically (no row left claimed with a 'pending' verdict).
#[test]
fn claim_with_tripwire_delegate_stamps_claimant() {
    let (_dir, store) = open_temp();
    let id = enqueue(&store, "gh", None, None, None, None, None, 100).unwrap();
    let claim = claim_with_tripwire(&store, &delegate_input(), "run-77", None, 200)
        .unwrap()
        .expect("a pending row was claimed");
    assert_eq!(claim.id, id);
    assert_eq!(claim.claimed_by, "run-77");
    assert_eq!(claim.verdict.fired_row, Some(2));
    assert_eq!(read_claimed_by(&store, id).as_deref(), Some("run-77"));
    let (verdict, _) = read_tripwire(&store, id).unwrap();
    assert_eq!(verdict, "delegate");
    // AC-2 invariant: no claimed row with a 'pending' verdict.
    let conn = store.conn().unwrap();
    let bad: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wv_queue WHERE state='claimed' AND tripwire_verdict='pending'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(bad, 0);
}

// Empty queue → Ok(None) (same contract as `claim`).
#[test]
fn claim_with_tripwire_empty_queue_returns_none() {
    let (_dir, store) = open_temp();
    let out = claim_with_tripwire(&store, &keep_input(), "run-1", None, 200).unwrap();
    assert!(out.is_none(), "no pending row → None");
}

// AC-4 — report_cycle increments below the threshold returns crossed=false, and
// the call that takes cycles 2→3 crosses and flips the verdict to 'delegate'.
#[test]
fn report_cycle_crosses_threshold_and_flips_verdict() {
    let (_dir, store) = open_temp();
    let id = enqueue(&store, "gh", None, None, None, None, None, 100).unwrap();
    // Claim as Keep so the row starts 'claimed' with verdict 'keep'.
    claim_with_tripwire(&store, &keep_input(), "run-1", None, 200)
        .unwrap()
        .unwrap();

    // Cycle 1 (0→1): below threshold.
    let r1 = report_cycle(&store, id, 210).unwrap().unwrap();
    assert_eq!(r1.cycles, 1);
    assert!(!r1.crossed);
    // Cycle 2 (1→2): below threshold.
    let r2 = report_cycle(&store, id, 220).unwrap().unwrap();
    assert_eq!(r2.cycles, 2);
    assert!(!r2.crossed);
    assert_eq!(
        read_tripwire(&store, id).unwrap().0,
        "keep",
        "still keep at 2"
    );

    // Cycle 3 (2→3): crosses the threshold → verdict flips to 'delegate'.
    let r3 = report_cycle(&store, id, 230).unwrap().unwrap();
    assert_eq!(r3.cycles, 3);
    assert!(r3.crossed, "the 2→3 call crosses the Row-5 threshold");
    let (verdict, cycles) = read_tripwire(&store, id).unwrap();
    assert_eq!(verdict, "delegate", "AC-4: verdict flipped mid-flight");
    assert_eq!(cycles, 3);

    // A 4th report (3→4) is above the threshold but NOT the crossing call —
    // edge-triggered `crossed` is false and the verdict stays `delegate`.
    let r4 = report_cycle(&store, id, 240).unwrap().unwrap();
    assert_eq!(r4.cycles, 4);
    assert!(
        !r4.crossed,
        "already fired — a later report is not a re-cross"
    );
    assert_eq!(read_tripwire(&store, id).unwrap().0, "delegate");

    // AC-8: each state-transition wrote a MUTATE audit row (claim + 4 reports = 5);
    // the enqueue `append` row is excluded by the operation filter.
    assert_eq!(audit_mutations_for(&store, id), 5);
}

// AC-4 refusal — report_cycle on a non-open (pending) row is InvalidState.
#[test]
fn report_cycle_on_pending_row_refuses() {
    let (_dir, store) = open_temp();
    let id = enqueue(&store, "gh", None, None, None, None, None, 100).unwrap();
    let out = report_cycle(&store, id, 210).unwrap();
    assert_eq!(
        out,
        Err(QueueError::InvalidState),
        "pending row has no live cycle"
    );
    // A missing row likewise refuses.
    let missing = report_cycle(&store, 9999, 210).unwrap();
    assert_eq!(missing, Err(QueueError::InvalidState));
}

// AC-5 — resolve preserves tripwire_verdict + verify_fix_cycles in the key-ledger
// `meta` JSON blob after the queue row is pruned.
#[test]
fn resolve_preserves_verdict_in_key_ledger_meta() {
    let (_dir, store) = open_temp();
    let id = enqueue(&store, "gh", None, Some("pr#5"), None, None, None, 100).unwrap();
    claim_with_tripwire(&store, &delegate_input(), "run-5", None, 200)
        .unwrap()
        .unwrap();
    // Report two cycles so verify_fix_cycles is a non-default value in the blob.
    report_cycle(&store, id, 210).unwrap().unwrap();
    report_cycle(&store, id, 220).unwrap().unwrap();

    resolve(&store, id, "run-5", 400).unwrap().unwrap();
    assert_eq!(queue_row_count(&store, id), 0, "row pruned");

    let conn = store.conn().unwrap();
    let meta: String = conn
        .query_row(
            "SELECT meta FROM wv_key_ledger WHERE queue_id=?1",
            params![id],
            |r| r.get(0),
        )
        .expect("key-ledger meta blob present");
    let parsed: serde_json::Value = serde_json::from_str(&meta).unwrap();
    assert_eq!(parsed["tripwireVerdict"], "delegate");
    assert_eq!(parsed["verifyFixCycles"], 2);
    // AC-5: the row that fired (Row 2, the .rs create) survives the prune too.
    assert_eq!(parsed["firedRow"], 2);
}

// ==========================================================================
// SW-WA-D3 — always-on guarantees on the fold + write path.
// ==========================================================================

/// Seed a `wv_done_record` for `dispatch_id` linked to `queue_id`, keyed to the given
/// worker `session`, with all three legs populated (an unverified record — the caller
/// chooses whether to stamp it verified).
fn seed_done_record(store: &WeftStore, dispatch_id: &str, queue_id: i64, session: &str) {
    let record = crate::weft::DoneRecord {
        dispatch_id: dispatch_id.to_string(),
        queue_id,
        session_id: session.to_string(),
        run_id: "run-1".to_string(),
        story_id: "SW-WA-D3".to_string(),
        subclass: "review".to_string(),
        leg1_source: "abc123".to_string(),
        leg2_footprint: vec![serde_json::json!({"kind": "file", "reference": "a.rs"})],
        leg3_packet: serde_json::json!({
            "signalStatus": "success",
            "canonicalRefs": ["https://example.test/pr/1"],
        }),
        created: 5_000,
    };
    store.insert_done_record(&record).unwrap();
}

/// AC-1 (T2): `close_task` on an UNVERIFIED dispatch — with the worker success signal
/// present so `controller_verified` is the SOLE unmet leg — returns `Blocked` with
/// `missing == ["controller_verified"]`, and the queue row is NOT pruned. The T1 stamp
/// gate (which never stamps an unverified verdict) is what keeps the row unclosable.
#[test]
fn close_task_blocks_unverified_dispatch_on_controller_verified_only() {
    let (_dir, store) = open_temp();
    let id = enqueue(&store, "gh", None, Some("pr#1"), None, None, None, 100).unwrap();
    claim(&store, "ctl-A", None, 200).unwrap();
    // A done record keyed to the worker session + a worker SUCCESS signal → the
    // `accepted` leg holds; `controller_verified` is left unset (never verified).
    seed_done_record(&store, "disp-1", id, "worker-sess");
    crate::weft::wv_signal_insert(&store, "w", "t", "worker-sess", "success", None, None).unwrap();

    let outcome = close_task(&store, id, "ctl-A", "disp-1", 400).unwrap();
    match outcome {
        CloseOutcome::Blocked { missing } => {
            assert_eq!(
                missing,
                vec![CloseCondition::NotControllerVerified],
                "controller_verified is the sole unmet leg"
            );
        }
        other => panic!("expected Blocked, got {other:?}"),
    }
    assert_eq!(
        read_state(&store, id).as_deref(),
        Some("claimed"),
        "an unverified row is NOT pruned"
    );
    assert_eq!(queue_row_count(&store, id), 1, "the row survives");
}

/// AC-1: once the dispatch is `controller_verified` (both legs hold), `close_task`
/// prunes the row.
#[test]
fn close_task_resolves_a_verified_dispatch() {
    let (_dir, store) = open_temp();
    let id = enqueue(&store, "gh", None, Some("pr#1"), None, None, None, 100).unwrap();
    claim(&store, "ctl-A", None, 200).unwrap();
    seed_done_record(&store, "disp-1", id, "worker-sess");
    crate::weft::wv_signal_insert(&store, "w", "t", "worker-sess", "success", None, None).unwrap();
    // Stamp controller_verified via the real verify seam (non-impl review subclass →
    // no run-store read needed). A seeded dispatch record satisfies the Done-When leg.
    crate::weft::wv_write_dispatch(&store, "SW-WA-D3", "do it", "", "", "finish", Some("root"))
        .map(|dispatch_row| {
            let conn = store.conn().unwrap();
            conn.execute(
                "UPDATE wv_dispatch_records SET state='done' WHERE id=?1",
                params![dispatch_row],
            )
            .unwrap();
        })
        .unwrap();
    let verdict = store
        .verify_done(
            "disp-1",
            1,
            std::path::Path::new("/nonexistent/run-store.jsonl"),
        )
        .unwrap();
    assert!(verdict.verified, "the dispatch verifies: {verdict:?}");

    assert_eq!(
        close_task(&store, id, "other-session", "disp-1", 390).unwrap(),
        CloseOutcome::ResolveRefused(QueueError::OwnerMismatch)
    );
    assert_eq!(
        queue_row_count(&store, id),
        1,
        "foreign caller cannot prune"
    );
    let outcome = close_task(&store, id, "ctl-A", "disp-1", 400).unwrap();
    assert_eq!(outcome, CloseOutcome::Resolved, "a verified row folds");
    assert_eq!(queue_row_count(&store, id), 0, "the row is pruned");
}

/// AC-3 (T3): G4 own-item-only write parity — for a row claimed by session X, a
/// `resolve` by Y yields the typed `OwnerMismatch` refusal AND
/// `admit_own_item_write("Y","X") == NotOwner`. The named guard tracks the SQL rule so
/// the guarantee can't drift; a blank owner is refused by both.
#[test]
fn g4_own_item_write_classifier_tracks_the_sql_one_writer_rule() {
    let (_dir, store) = open_temp();
    let id = enqueue(&store, "gh", None, Some("pr#9"), None, None, None, 100).unwrap();
    claim(&store, "ctl-X", None, 200).unwrap();

    // The SQL one-writer rule refuses a non-owner resolve.
    let refusal = resolve(&store, id, "ctl-Y", 400).unwrap();
    assert_eq!(refusal, Err(QueueError::OwnerMismatch));
    // The named classifier agrees — same question, same answer.
    assert_eq!(
        crate::weft::admit_own_item_write("ctl-Y", "ctl-X"),
        crate::weft::WriteDecision::NotOwner
    );
    // The owner's own resolve is admitted by both (parity on the permit side too).
    assert_eq!(
        crate::weft::admit_own_item_write("ctl-X", "ctl-X"),
        crate::weft::WriteDecision::Owned
    );
    // A blank owner is fail-closed on both sides.
    assert_eq!(
        crate::weft::admit_own_item_write("ctl-X", ""),
        crate::weft::WriteDecision::NotOwner
    );

    // The row is untouched after the refused resolve (SQL guard held).
    assert_eq!(read_state(&store, id).as_deref(), Some("claimed"));
    assert_eq!(queue_row_count(&store, id), 1);
}

/// SW-WA-A10 (AC-3 no-double-file): the `id > pre_max` novel-append contract that GATES the
/// `queue_enqueued` reactive dispatch wake. A fresh insert mints live-max+1, so its returned id is
/// strictly above the pre-append max — the wake fires. A dedupe short-circuit (a live row already
/// holds the key) returns the EXISTING id, at or below the pre-append max — the wake MUST NOT fire,
/// so a triage re-pulse of an in-flight ask never re-wakes the controller. This is the exact seam
/// the `wv_enqueue` handler reads before emitting `queue_enqueued`.
#[test]
fn a10_novel_append_is_above_pre_max_and_a_dedupe_hit_is_not() {
    let (_dir, store) = open_temp();
    // A fresh enqueue: pre-append max is 0, the minted id is strictly above it → a novel append.
    let pre_max = current_max_id(&store).unwrap();
    let id = enqueue(
        &store,
        "slack:msg-1",
        None,
        None,
        None,
        Some("dedupe-A"),
        None,
        1,
    )
    .unwrap();
    assert!(
        id > pre_max,
        "a novel append returns an id above the pre-append max — the wake fires"
    );
    // A second enqueue with the SAME live dedupe key short-circuits: pre-append max now equals the
    // live row's id, and the returned id is that existing id (NOT above the snapshot) → no re-wake.
    let pre_max2 = current_max_id(&store).unwrap();
    let dup = enqueue(
        &store,
        "slack:msg-1-again",
        None,
        None,
        None,
        Some("dedupe-A"),
        None,
        2,
    )
    .unwrap();
    assert_eq!(dup, id, "the dedupe short-circuit returns the existing id");
    assert!(
        dup <= pre_max2,
        "a dedupe hit is not above the pre-append max — the wake is suppressed (no double-file)"
    );
}

// ---------- role-inbox orphan cleanup ----------

/// Seed a `wv_queue` row directly (bypassing the normal write path) with an
/// arbitrary `source`/`state`/`dedupe_key`. Used only by the cleanup tests to
/// stage pre-fix orphan rows that nothing is still writing. The INSERT runs
/// INSIDE `src/weft/`, so the `only_weft_sqlite_writes_wv_tables` CI test
/// does not flag it.
fn seed_queue_row(
    store: &WeftStore,
    id: i64,
    source: &str,
    state: &str,
    dedupe_key: &str,
    now: i64,
) {
    let conn = store.conn().unwrap();
    conn.execute(
        "INSERT INTO wv_queue \
         (id, source, state, scope, created, updated, refs, note, dedupe_key, dispatch_peers) \
         VALUES (?1, ?2, ?3, NULL, ?4, ?4, NULL, NULL, ?5, NULL)",
        params![id, source, state, now, dedupe_key],
    )
    .unwrap();
}

/// The cleanup prunes exactly `source='role_inbox' AND state='pending'`
/// rows, writes `wv_key_ledger` rows under each dedupe tag, and leaves any
/// other row (claimed role rows, other sources) intact.
#[test]
fn prune_orphan_role_inbox_rows_prunes_only_pending_role_rows() {
    let (_dir, store) = open_temp();
    let now = 1_000_000;
    seed_queue_row(&store, 1, "role_inbox", "pending", "role_delivery:d1", now);
    seed_queue_row(&store, 2, "role_inbox", "pending", "role_delivery:d2", now);
    seed_queue_row(&store, 3, "role_inbox", "claimed", "role_delivery:d3", now);
    seed_queue_row(&store, 4, "triage", "pending", "triage:t1", now);

    let mut conn = store.conn().unwrap();
    let pruned = super::prune_orphan_role_inbox_rows(&mut conn, now + 1).unwrap();
    assert_eq!(pruned, 2, "exactly the two pending role rows are pruned");

    // The two targeted rows are gone; the other two are intact.
    let remaining: Vec<(i64, String, String)> = {
        let mut stmt = conn
            .prepare("SELECT id, source, state FROM wv_queue ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(
        remaining,
        vec![
            (3, "role_inbox".to_string(), "claimed".to_string()),
            (4, "triage".to_string(), "pending".to_string()),
        ],
        "a claimed role row and a non-role row survive the cleanup"
    );

    // Two wv_key_ledger rows, one per pruned id, under the right dedupe tag.
    let ledger: Vec<(i64, Option<String>)> = {
        let mut stmt = conn
            .prepare("SELECT queue_id, ref FROM wv_key_ledger ORDER BY queue_id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(
        ledger,
        vec![
            (1, Some("role_delivery:d1".to_string())),
            (2, Some("role_delivery:d2".to_string())),
        ],
        "the ledger carries the dedupe tag of each pruned row"
    );

    // Each pruned row leaves a `prune` audit row (same shape as `resolve`).
    let audits: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wv_write_log \
             WHERE tbl='wv_queue' AND operation='prune' AND row_id IN (1,2)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(audits, 2, "one `prune` audit row per pruned queue row");
}

/// The cleanup is idempotent. A second call returns 0 and makes no
/// writes; re-opening `WeftStore` twice (which calls it on open) also lands
/// at the same end state.
#[test]
fn prune_orphan_role_inbox_rows_is_idempotent() {
    // Pass 1: seed and prune on a bare store.
    let (dir, store) = open_temp();
    let now = 2_000_000;
    seed_queue_row(&store, 10, "role_inbox", "pending", "role_delivery:x", now);
    seed_queue_row(&store, 11, "role_inbox", "pending", "role_delivery:y", now);
    {
        let mut conn = store.conn().unwrap();
        let pruned = super::prune_orphan_role_inbox_rows(&mut conn, now + 1).unwrap();
        assert_eq!(pruned, 2);
    }

    // Snapshot the post-first-pass state before the second call.
    let snapshot = |store: &WeftStore| -> (i64, i64, i64) {
        let conn = store.conn().unwrap();
        let q: i64 = conn
            .query_row("SELECT COUNT(*) FROM wv_queue", [], |r| r.get(0))
            .unwrap();
        let l: i64 = conn
            .query_row("SELECT COUNT(*) FROM wv_key_ledger", [], |r| r.get(0))
            .unwrap();
        let a: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM wv_write_log WHERE tbl='wv_queue' AND operation='prune'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        (q, l, a)
    };
    let after_first = snapshot(&store);

    // Second call: no matching rows → 0 pruned, no writes, no new audit rows.
    {
        let mut conn = store.conn().unwrap();
        let pruned = super::prune_orphan_role_inbox_rows(&mut conn, now + 2).unwrap();
        assert_eq!(pruned, 0, "a second call finds nothing to prune");
    }
    assert_eq!(snapshot(&store), after_first, "no second-pass writes");

    // Re-opening WeftStore (which calls the cleanup on open) is also a no-op.
    drop(store);
    let path = dir.path().join("weft.sqlite3");
    let store2 = WeftStore::open(&path).unwrap();
    assert_eq!(snapshot(&store2), after_first, "re-open adds nothing");
    drop(store2);
    let store3 = WeftStore::open(&path).unwrap();
    assert_eq!(
        snapshot(&store3),
        after_first,
        "a second re-open adds nothing"
    );
}

/// WeftStore::open itself must prune orphan role-inbox rows a
/// legacy store carries, not just leave the function callable. Seed an orphan
/// into the sqlite file, drop the store, then open a fresh WeftStore and verify
/// the open path drained it + wrote its ledger sibling. Without this test the
/// one-line wire-up at `WeftStore::open` could be deleted and both of the
/// direct-call tests above would still pass.
#[test]
fn weft_store_open_prunes_legacy_orphan_role_inbox_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("weft.sqlite3");

    // Seed rows as an older process would have left them, then give this
    // fixture a fresh process cache scope for the migration open.
    {
        let store = WeftStore::open(&path).unwrap();
        seed_queue_row(
            &store,
            42,
            "role_inbox",
            "pending",
            "role_delivery:legacy",
            1_234_000,
        );
    }

    // The first open in the fresh cache scope MUST drain the orphan.
    crate::weft::sqlite::reset_cached_store_for_test(&path);
    let store = WeftStore::open(&path).unwrap();
    let conn = store.conn().unwrap();
    let remaining: i64 = conn
        .query_row("SELECT COUNT(*) FROM wv_queue WHERE id=42", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        remaining, 0,
        "WeftStore::open did not prune the legacy orphan row — the wire-up at sqlite.rs is missing or broken"
    );
    let ledger_ref: Option<String> = conn
        .query_row("SELECT ref FROM wv_key_ledger WHERE queue_id=42", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        ledger_ref.as_deref(),
        Some("role_delivery:legacy"),
        "ledger ref was not written on open-time prune"
    );
    let audit: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wv_write_log WHERE tbl='wv_queue' AND operation='prune' AND row_id=42",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(audit, 1, "open-time prune did not write the audit row");
}

/// AC-3 conformance: the `resolve` one-writer site references
/// `crate::weft::admit_own_item_write` BY NAME, so the G4 guarantee has a named
/// classifier wired at the SQL guard.
#[test]
fn resolve_site_references_admit_own_item_write_by_name() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/weft/queue.rs"),
    )
    .expect("read queue.rs");
    assert!(
        src.contains("crate::weft::admit_own_item_write("),
        "the resolve one-writer site must reference admit_own_item_write by name"
    );
}

#[test]
fn kept_and_delegated_claims_belong_to_the_verified_claimant() {
    for input in [keep_input(), delegate_input()] {
        let (_dir, store) = open_temp();
        let id = enqueue(&store, "gh", None, None, None, None, None, 100).unwrap();
        let claim = claim_with_tripwire(&store, &input, "session-a", None, 200)
            .unwrap()
            .unwrap();
        assert_eq!(claim.claimed_by, "session-a");
        assert_eq!(
            resolve(&store, id, "session-b", 210).unwrap(),
            Err(QueueError::OwnerMismatch)
        );
        assert_eq!(
            resolve(&store, id, "controller", 210).unwrap(),
            Err(QueueError::OwnerMismatch)
        );
        crate::weft::wv_write_dispatch(&store, "story", "task", "", "", "done", Some("root"))
            .unwrap();
        assert_eq!(
            mark_dispatched(&store, id, "session-b", "story", "worker", 220).unwrap(),
            Err(QueueError::StateConflict)
        );
        mark_dispatched(&store, id, "session-a", "story", "worker", 230)
            .unwrap()
            .unwrap();
        resolve(&store, id, "session-a", 240).unwrap().unwrap();
    }
}

#[test]
fn successive_controller_invocations_can_finish_the_same_queue_work() {
    use crate::commands::dashboard_launch::{DashboardLaunchState, RunProvenance};
    fn invocation(session: &str, routine: Option<&str>) -> QueueCaller {
        let state = DashboardLaunchState::new(4);
        state.register("controller", session, Some("codesense"));
        if let Some(key) = routine {
            state.stamp_provenance(
                "controller",
                RunProvenance {
                    run_id: format!("run-{session}"),
                    story_id: "controller".into(),
                    routine_key: Some(key.into()),
                    ..Default::default()
                },
            );
        }
        crate::mcp::local_identity::queue_caller(session, Some(&state), None)
    }
    let (_dir, store) = open_temp();
    let first = invocation("fresh-1", Some("org/team/controller"));
    let second = invocation("fresh-2", Some("org/team/controller"));
    let foreign = invocation("fresh-3", Some("org/team/other"));
    let plain = invocation("plain", None);
    let id = enqueue(&store, "gh", None, None, None, None, None, 100).unwrap();
    let claimed = claim_with_tripwire(&store, &delegate_input(), first.clone(), None, 200)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.claimed_by, "routine:org/team/controller");
    crate::weft::wv_write_dispatch(&store, "SW-WA-D3", "task", "", "", "finish", Some("root"))
        .unwrap();
    mark_dispatched(&store, id, first, "SW-WA-D3", "worker", 210)
        .unwrap()
        .unwrap();
    seed_done_record(&store, "disp-1", id, "worker-session");
    crate::weft::wv_signal_insert(
        &store,
        "worker",
        "task",
        "worker-session",
        "success",
        None,
        None,
    )
    .unwrap();
    store
        .conn()
        .unwrap()
        .execute("UPDATE wv_dispatch_records SET state='done'", [])
        .unwrap();
    assert!(
        store
            .verify_done(
                "disp-1",
                1,
                std::path::Path::new("/nonexistent/run-store.jsonl")
            )
            .unwrap()
            .verified
    );
    for other in [foreign, plain] {
        assert_eq!(
            close_task(&store, id, other, "disp-1", 300).unwrap(),
            CloseOutcome::ResolveRefused(QueueError::OwnerMismatch)
        );
    }
    assert_eq!(
        close_task(&store, id, second, "disp-1", 310).unwrap(),
        CloseOutcome::Resolved
    );
    let actor: String = store.conn().unwrap().query_row(
        "SELECT caller_session FROM wv_write_log WHERE tbl='wv_queue' AND operation='prune' AND row_id=?1",
        params![id], |r| r.get(0)).unwrap();
    assert_eq!(actor, "fresh-2");
}
