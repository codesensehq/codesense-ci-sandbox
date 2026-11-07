// SPDX-License-Identifier: MIT
//! SW-WV-A3 acceptance tests — the worker lifecycle state machine at the STORE
//! SEAM (the load-bearing CAS guard, not the pure `check_transition` alone).
//!
//! Covers AC-1 (illegal transitions refused through the real `WeftStore` methods +
//! a happy-path round-trip via `SELECT`), AC-2 (concurrent CAS: one winner + audit
//! atomicity — roster row and `wv_write_log` row both written or neither), AC-3
//! (four-condition closure check + offered-worker protection), AC-6 (injectable
//! liveness probe warns non-blocking), AC-7 (no reactor block on a current-thread
//! runtime). The pure transition table + the `SignalDoneHooks` registry order test
//! live in `weft/worker_lifecycle.rs` (A3 sole owner of the order assertion).

use std::sync::Arc;

use super::*;
use crate::weft::store::WeftStore;
use crate::weft::worker_lifecycle::{ClosureCondition, LifecycleError};

fn open_temp() -> (tempfile::TempDir, WeftStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
    (dir, store)
}

/// Seed a worker roster row directly in a chosen lifecycle `status` (+ optional
/// scope/lifecycle/session) so a transition can be exercised from any predecessor
/// without driving the whole spawn path. Goes through the store's own connection
/// but is a TEST seed, not a production write.
fn seed_worker(
    store: &WeftStore,
    name: &str,
    status: &str,
    scope: Option<&str>,
    lifecycle: &str,
    session_id: Option<&str>,
) {
    let conn = store.conn().unwrap();
    conn.execute(
        "INSERT INTO wv_roster (name, status, scope, relation, lifecycle, session_id, created_at, updated_at) \
         VALUES (?1, ?2, ?3, 'child:s', ?4, ?5, '', '') \
         ON CONFLICT(name) DO UPDATE SET status=excluded.status, scope=excluded.scope, \
           lifecycle=excluded.lifecycle, session_id=excluded.session_id",
        rusqlite::params![name, status, scope, lifecycle, session_id],
    )
    .unwrap();
}

fn read_status(store: &WeftStore, name: &str) -> Option<String> {
    let conn = store.conn().unwrap();
    conn.query_row(
        "SELECT status FROM wv_roster WHERE name=?1",
        rusqlite::params![name],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

fn audit_count(store: &WeftStore, op: &str, name: &str) -> i64 {
    // `write_audit_named` keys the audit row_id on a stable hash of the name.
    let conn = store.conn().unwrap();
    let row_id = super::stable_key_hash(name);
    conn.query_row(
        "SELECT COUNT(*) FROM wv_write_log WHERE tbl='wv_roster' AND operation=?1 AND row_id=?2",
        rusqlite::params![op, row_id],
        |r| r.get(0),
    )
    .unwrap()
}

// ---------------------------------------------------------------------------
// AC-1 — illegal transitions refused at the store seam; happy-path round-trip.
// ---------------------------------------------------------------------------

#[test]
fn ac1_illegal_transitions_refused_through_store_methods() {
    let (_d, store) = open_temp();

    // signal requires `dispatch`. From `provision` it's illegal.
    seed_worker(&store, "w1", "provision", None, "task", None);
    let err = wv_worker_signal(&store, "w1").unwrap_err();
    assert!(matches!(
        err,
        WvWriteError::Invalid(_) // IllegalTransition maps to a validation refusal
    ));
    // The row was NOT advanced (no silent no-op the OTHER way either).
    assert_eq!(read_status(&store, "w1").as_deref(), Some("provision"));

    // handoff requires `signal`. From `dispatch` it's illegal.
    seed_worker(&store, "w2", "dispatch", None, "task", None);
    assert!(wv_worker_handoff(&store, "w2").is_err());
    assert_eq!(read_status(&store, "w2").as_deref(), Some("dispatch"));

    // handoff from `provision` — illegal.
    seed_worker(&store, "w3", "provision", None, "task", None);
    assert!(wv_worker_handoff(&store, "w3").is_err());

    // signal from `signal` (already signalled) — illegal (no re-signal).
    seed_worker(&store, "w4", "signal", None, "task", None);
    assert!(wv_worker_signal(&store, "w4").is_err());

    // signal from `deprovision` (terminal) — illegal.
    seed_worker(&store, "w5", "deprovision", None, "task", None);
    assert!(wv_worker_signal(&store, "w5").is_err());

    // handoff from `handoff` — illegal.
    seed_worker(&store, "w6", "handoff", None, "task", None);
    assert!(wv_worker_handoff(&store, "w6").is_err());

    // signal on an ABSENT row — illegal (nothing to advance, not a no-op).
    assert!(wv_worker_signal(&store, "ghost").is_err());

    // handoff on an ABSENT row — illegal.
    assert!(wv_worker_handoff(&store, "ghost2").is_err());

    // deprovision from `provision` (skip signal) — closure check refuses it.
    seed_worker(&store, "w7", "provision", None, "task", None);
    let err = wv_worker_deprovision(
        &store,
        "w7",
        DeprovisionInputs {
            packet_delivered: true,
            force: false,
        },
        |_| true,
    )
    .unwrap_err();
    match err {
        WvWriteError::Invalid(v) => assert!(
            v.0.contains("signal-received"),
            "provision→deprovision must fail the signal-received closure gate: {}",
            v.0
        ),
        other => panic!("expected closure refusal, got {other:?}"),
    }
}

#[test]
fn ac1_happy_path_round_trip_provision_dispatch_signal_deprovision() {
    let (_d, store) = open_temp();
    // Drive the full sequence through the real store methods.
    wv_provision_worker_row(&store, "SW-X", "developer", "scope-x").unwrap();
    assert_eq!(read_status(&store, "SW-X").as_deref(), Some("provision"));

    let r = wv_dispatch_worker_row(&store, "SW-X", "sess-x", None).unwrap();
    assert_eq!(r, MutateResult::Updated);
    assert_eq!(read_status(&store, "SW-X").as_deref(), Some("dispatch"));

    wv_worker_signal(&store, "SW-X").unwrap();
    assert_eq!(read_status(&store, "SW-X").as_deref(), Some("signal"));

    // No open follow-ups in scope-x, packet delivered → deprovision succeeds.
    let out = wv_worker_deprovision(
        &store,
        "SW-X",
        DeprovisionInputs {
            packet_delivered: true,
            force: false,
        },
        |_| true,
    )
    .unwrap();
    assert!(!out.liveness_warned);
    // Final state asserted via SELECT (AC-1).
    assert_eq!(read_status(&store, "SW-X").as_deref(), Some("deprovision"));
}

#[test]
fn ac1_happy_path_with_optional_handoff() {
    let (_d, store) = open_temp();
    seed_worker(&store, "SW-H", "signal", Some("scope-h"), "task", None);
    wv_worker_handoff(&store, "SW-H").unwrap();
    assert_eq!(read_status(&store, "SW-H").as_deref(), Some("handoff"));
    let out = wv_worker_deprovision(
        &store,
        "SW-H",
        DeprovisionInputs {
            packet_delivered: true,
            force: false,
        },
        |_| true,
    )
    .unwrap();
    assert!(!out.liveness_warned);
    assert_eq!(read_status(&store, "SW-H").as_deref(), Some("deprovision"));
}

// ---------------------------------------------------------------------------
// AC-2 — concurrent CAS: exactly one winner; audit co-committed atomically.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ac2_concurrent_dispatch_to_signal_one_winner() {
    let (_d, store) = open_temp();
    seed_worker(&store, "race", "dispatch", None, "task", None);

    // Two tasks race the same `dispatch → signal` transition. Exactly one wins.
    let s1 = store.clone();
    let s2 = store.clone();
    let t1 = tokio::task::spawn_blocking(move || wv_worker_signal(&s1, "race"));
    let t2 = tokio::task::spawn_blocking(move || wv_worker_signal(&s2, "race"));
    let (r1, r2) = (t1.await.unwrap(), t2.await.unwrap());

    let wins = [r1.is_ok(), r2.is_ok()].iter().filter(|b| **b).count();
    let losses = [&r1, &r2].iter().filter(|r| r.is_err()).count();
    assert_eq!(wins, 1, "exactly one dispatch→signal wins");
    assert_eq!(
        losses, 1,
        "the loser gets a typed refusal, not a silent no-op"
    );
    assert_eq!(read_status(&store, "race").as_deref(), Some("signal"));
    // The loser's error is the IllegalTransition refusal.
    let loser = if r1.is_err() { r1 } else { r2 };
    match loser.unwrap_err() {
        WvWriteError::Invalid(v) => assert!(v.0.contains("dispatch"), "loser names the transition"),
        other => panic!("expected IllegalTransition refusal, got {other:?}"),
    }
    // Exactly ONE audit row for the winning signal transition (the loser wrote none).
    assert_eq!(audit_count(&store, "signal", "race"), 1);
}

#[test]
fn ac2_audit_row_co_committed_with_each_transition() {
    let (_d, store) = open_temp();
    seed_worker(&store, "aud", "dispatch", Some("sc"), "task", None);

    // Before: no signal/deprovision audit rows.
    assert_eq!(audit_count(&store, "signal", "aud"), 0);
    wv_worker_signal(&store, "aud").unwrap();
    // After the signal transition: roster row AND the audit row are both present.
    assert_eq!(read_status(&store, "aud").as_deref(), Some("signal"));
    assert_eq!(audit_count(&store, "signal", "aud"), 1);

    wv_worker_deprovision(
        &store,
        "aud",
        DeprovisionInputs {
            packet_delivered: true,
            force: false,
        },
        |_| true,
    )
    .unwrap();
    assert_eq!(read_status(&store, "aud").as_deref(), Some("deprovision"));
    assert_eq!(audit_count(&store, "deprovision", "aud"), 1);
}

#[test]
fn ac2_failed_transition_writes_no_audit_row() {
    let (_d, store) = open_temp();
    seed_worker(&store, "noaud", "provision", None, "task", None);
    // Illegal signal from provision → no roster change, no audit row.
    assert!(wv_worker_signal(&store, "noaud").is_err());
    assert_eq!(read_status(&store, "noaud").as_deref(), Some("provision"));
    assert_eq!(audit_count(&store, "signal", "noaud"), 0);
}

// ---------------------------------------------------------------------------
// AC-3 — the four-condition closure check + offered-worker protection.
// ---------------------------------------------------------------------------

fn deprovision(
    store: &WeftStore,
    name: &str,
    packet_delivered: bool,
    force: bool,
) -> WvWriteResult<DeprovisionOutcome> {
    wv_worker_deprovision(
        store,
        name,
        DeprovisionInputs {
            packet_delivered,
            force,
        },
        |_| true,
    )
}

fn assert_closure(err: WvWriteError, expect: ClosureCondition) {
    match err {
        WvWriteError::Invalid(v) => assert!(
            v.0.contains(expect.as_wire()),
            "expected closure condition {:?} ({}), got: {}",
            expect,
            expect.as_wire(),
            v.0
        ),
        other => panic!("expected closure refusal, got {other:?}"),
    }
}

#[test]
fn ac3_condition1_signal_not_received() {
    let (_d, store) = open_temp();
    seed_worker(&store, "c1", "dispatch", Some("sc"), "task", None);
    assert_closure(
        deprovision(&store, "c1", true, false).unwrap_err(),
        ClosureCondition::SignalReceived,
    );
    assert_eq!(read_status(&store, "c1").as_deref(), Some("dispatch"));
}

#[test]
fn ac3_condition2_packet_not_delivered() {
    let (_d, store) = open_temp();
    seed_worker(&store, "c2", "signal", Some("sc"), "task", None);
    assert_closure(
        deprovision(&store, "c2", false, false).unwrap_err(),
        ClosureCondition::PacketDelivered,
    );
}

#[test]
fn ac3_condition3_open_followups_in_scope() {
    let (_d, store) = open_temp();
    seed_worker(&store, "c3", "signal", Some("scope-c3"), "task", None);
    // An open (non-resolved) wv_queue row in the worker's scope blocks closure.
    {
        let conn = store.conn().unwrap();
        conn.execute(
            "INSERT INTO wv_queue (id, source, state, scope, created, updated) \
             VALUES (1, 'agent', 'pending', 'scope-c3', 0, 0)",
            [],
        )
        .unwrap();
    }
    assert_closure(
        deprovision(&store, "c3", true, false).unwrap_err(),
        ClosureCondition::NoOpenFollowups,
    );
    // Once the queue row resolves, closure succeeds.
    {
        let conn = store.conn().unwrap();
        conn.execute("UPDATE wv_queue SET state='resolved' WHERE id=1", [])
            .unwrap();
    }
    deprovision(&store, "c3", true, false).unwrap();
    assert_eq!(read_status(&store, "c3").as_deref(), Some("deprovision"));
}

#[test]
fn ac3_condition4_offered_worker_protected_without_force() {
    let (_d, store) = open_temp();
    seed_worker(&store, "c4", "signal", Some("sc"), "offered", None);
    let err = deprovision(&store, "c4", true, false).unwrap_err();
    match err {
        WvWriteError::Invalid(v) => assert!(
            v.0.contains("offered worker is protected"),
            "offered worker must give OfferedWorkerProtected: {}",
            v.0
        ),
        other => panic!("expected OfferedWorkerProtected, got {other:?}"),
    }
    // With force=true it deprovisions.
    deprovision(&store, "c4", true, true).unwrap();
    assert_eq!(read_status(&store, "c4").as_deref(), Some("deprovision"));
}

#[test]
fn ac3_all_conditions_met_succeeds() {
    let (_d, store) = open_temp();
    seed_worker(&store, "ok", "signal", Some("scope-ok"), "task", None);
    let out = deprovision(&store, "ok", true, false).unwrap();
    assert!(!out.liveness_warned);
    assert_eq!(read_status(&store, "ok").as_deref(), Some("deprovision"));
}

// ---------------------------------------------------------------------------
// AC-6 — injectable liveness probe warns (non-blocking); Ok either way.
// ---------------------------------------------------------------------------

#[test]
fn ac6_liveness_probe_alive_warns_but_returns_ok() {
    let (_d, store) = open_temp();
    seed_worker(
        &store,
        "live",
        "signal",
        Some("sc"),
        "task",
        Some("sess-live"),
    );
    // Probe reports NOT idle (alive) → warn flag set, still Ok, row deprovisioned.
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls2 = calls.clone();
    let out = wv_worker_deprovision(
        &store,
        "live",
        DeprovisionInputs {
            packet_delivered: true,
            force: false,
        },
        move |sid| {
            calls2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            assert_eq!(sid, "sess-live");
            false // NOT idle → still alive
        },
    )
    .unwrap();
    assert!(out.liveness_warned, "an alive PTY at teardown warns");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(read_status(&store, "live").as_deref(), Some("deprovision"));
}

#[test]
fn ac6_liveness_probe_idle_no_warning() {
    let (_d, store) = open_temp();
    seed_worker(
        &store,
        "idle",
        "signal",
        Some("sc"),
        "task",
        Some("sess-idle"),
    );
    let out = wv_worker_deprovision(
        &store,
        "idle",
        DeprovisionInputs {
            packet_delivered: true,
            force: false,
        },
        |_| true, // idle → no warning
    )
    .unwrap();
    assert!(!out.liveness_warned);
    assert_eq!(read_status(&store, "idle").as_deref(), Some("deprovision"));
}

// ---------------------------------------------------------------------------
// AC-4 (iii) — the roster-update hook's store binding: the `signal_done` hook
// resolves the completing session's worker by `session_id` and advances it
// `dispatch → signal`. This is the exact fn the registered `roster-update` hook
// calls (`wv_worker_signal_by_session`); asserting it proves the binding without a
// full Tauri AppHandle harness (which does not exist — see DEFERRALS DW-WV-E1-1).
// ---------------------------------------------------------------------------

#[test]
fn ac4_signal_by_session_advances_dispatch_to_signal() {
    let (_d, store) = open_temp();
    seed_worker(
        &store,
        "SW-S",
        "dispatch",
        Some("sc"),
        "task",
        Some("sess-42"),
    );
    // The hook advances by session id.
    let advanced = wv_worker_signal_by_session(&store, "sess-42").unwrap();
    assert!(advanced, "a dispatched worker for the session advances");
    assert_eq!(read_status(&store, "SW-S").as_deref(), Some("signal"));
    // Audit row co-committed for the signal transition (keyed on the roster name).
    assert_eq!(audit_count(&store, "signal", "SW-S"), 1);
}

#[test]
fn ac4_signal_by_session_is_graceful_noop_for_non_worker_session() {
    let (_d, store) = open_temp();
    // No dispatched worker carries this session — the completion hot path never fails.
    assert!(!wv_worker_signal_by_session(&store, "unknown-sess").unwrap());
    // A blank session id is also a graceful no-op.
    assert!(!wv_worker_signal_by_session(&store, "").unwrap());
    // A worker NOT in `dispatch` (already signalled) is not re-advanced.
    seed_worker(
        &store,
        "SW-D",
        "signal",
        Some("sc"),
        "task",
        Some("sess-done"),
    );
    assert!(!wv_worker_signal_by_session(&store, "sess-done").unwrap());
    assert_eq!(read_status(&store, "SW-D").as_deref(), Some("signal"));
}

// ---------------------------------------------------------------------------
// AC-7 — no reactor block: all four transitions run under a current-thread
// runtime without deadlocking (a blocking SQLite call on the reactor would hang).
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn ac7_all_transitions_run_without_blocking_the_reactor() {
    let (_d, store) = open_temp();
    // The store methods are synchronous SQLite; drive them off the reactor via
    // spawn_blocking exactly as the MCP/Tauri boundary does. Completing this test
    // proves the calls are off-thread (a reactor-blocking call would deadlock).
    let s = store.clone();
    tokio::task::spawn_blocking(move || {
        wv_provision_worker_row(&s, "SW-R", "developer", "scope-r").unwrap();
        wv_dispatch_worker_row(&s, "SW-R", "sess-r", None).unwrap();
        wv_worker_signal(&s, "SW-R").unwrap();
        wv_worker_handoff(&s, "SW-R").unwrap();
        wv_worker_deprovision(
            &s,
            "SW-R",
            DeprovisionInputs {
                packet_delivered: true,
                force: false,
            },
            |_| true,
        )
        .unwrap();
    })
    .await
    .unwrap();
    assert_eq!(read_status(&store, "SW-R").as_deref(), Some("deprovision"));
}
