// SPDX-License-Identifier: MIT
//! Contract tests at the `OrchestrationStore` seam. Each proves one case under:
//! concurrency (OS threads, per-caller connections), restart (drop + reopen from
//! the same path), and fault injection (armed abort points inside multi-row ops).

use std::sync::Mutex;

use super::fault::{self, FaultPoint};
use super::sqlite::{default_store_path, SqliteOrchestrationStore};
use super::store::{OrchestrationStore, StoreError};
use super::types::{DeliveryRef, RunEvent, RunState, WorkerId};

/// A GitHub-PAT-shaped marker the shared `secret_scan` seam detects — used to
/// prove redaction at persist.
const SECRET: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123";

/// A lease deadline far in the future — for claims in tests that never exercise
/// reclaim, so the claim is unambiguously live.
const LEASE_NEVER_EXPIRES: i64 = i64::MAX;

fn open(dir: &std::path::Path, name: &str) -> SqliteOrchestrationStore {
    SqliteOrchestrationStore::open(dir.join(name)).expect("store opens + migrates")
}

// ── AC-1: durable path is runtime state under codesense_home, not the repo ────

#[test]
fn default_store_path_lives_under_codesense_home_not_the_context_repo() {
    let home = std::path::Path::new("/home/user/.codesense");
    let path = default_store_path(home);
    assert!(
        path.starts_with(home),
        "coordinator DB is runtime state under codesense_home (ARCH-INV-11)"
    );
    assert_eq!(path.file_name().unwrap(), "coordinator.sqlite3");
    // The path a real store opens must round-trip through open() the same way tests do.
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteOrchestrationStore::open(default_store_path(dir.path())).unwrap();
    store.enqueue_runnable("run-1").unwrap();
    assert_eq!(store.count_runs_in_state(RunState::Runnable).unwrap(), 1);
}

// ── AC-2: single-winner claim under concurrency ──────────────────────────────

#[test]
fn claim_ready_elects_exactly_one_winner_per_item_under_concurrency() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path(), "claim.sqlite3");
    const N: usize = 1000;
    const THREADS: usize = 8;
    for i in 0..N {
        store.enqueue_runnable(&format!("run-{i}")).unwrap();
    }

    let claimed = Mutex::new(Vec::<String>::new());
    std::thread::scope(|s| {
        for t in 0..THREADS {
            let store = store.clone();
            let claimed = &claimed;
            s.spawn(move || {
                let worker = WorkerId::new(format!("w{t}"));
                let mut mine = Vec::new();
                // Each thread drains via its OWN connection: the race resolves in
                // SQLite's CAS, not a Rust mutex.
                while let Some(claim) = store.claim_ready(&worker, LEASE_NEVER_EXPIRES).unwrap() {
                    mine.push(claim.run_id);
                }
                claimed.lock().unwrap().extend(mine);
            });
        }
    });

    let mut all = claimed.into_inner().unwrap();
    assert_eq!(
        all.len(),
        N,
        "no lost item and no double-dispatch (perfect partition)"
    );
    all.sort();
    all.dedup();
    assert_eq!(all.len(), N, "no item claimed by two workers");
    assert_eq!(
        store.count_runs_in_state(RunState::Runnable).unwrap(),
        0,
        "queue fully drained"
    );
}

// ── AC-7: restart durability ─────────────────────────────────────────────────

#[test]
fn runnable_rows_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("restart.sqlite3");
    {
        let store = SqliteOrchestrationStore::open(&path).unwrap();
        store.enqueue_runnable("run-runnable").unwrap();
    } // process teardown

    let store = SqliteOrchestrationStore::open(&path).unwrap();
    assert_eq!(
        store.count_runs_in_state(RunState::Runnable).unwrap(),
        1,
        "runnable survived restart"
    );
    // The reopened store is a working coordinator: it can still claim the runnable.
    let claim = store
        .claim_ready(&WorkerId::new("w"), LEASE_NEVER_EXPIRES)
        .unwrap();
    assert_eq!(claim.unwrap().run_id, "run-runnable");
}

// ── AC-9: one-fact-one-owner (no metadata-mirroring column) ───────────────────

#[test]
fn schema_persists_no_metadata_mirroring_column() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path(), "schema.sqlite3");
    let sql = store.schema_sql().unwrap().to_lowercase();

    // Any of these column tokens would mean the store mirrors a metadata fact.
    for forbidden in [
        "status",
        "rule",
        "definition",
        "config",
        "assignee",
        "title",
        "summary",
        "priority",
        "metadata",
    ] {
        assert!(
            !sql.contains(forbidden),
            "schema must not mirror metadata `{forbidden}`:\n{sql}"
        );
    }
    // Sanity: the coordination tables ARE present — including `deliveries`, whose
    // only columns are the coordination key + state.
    assert!(
        sql.contains("run_events") && sql.contains("run_requests") && sql.contains("deliveries")
    );
}

// ── AC-11: redaction at persist ──────────────────────────────────────────────

#[test]
fn append_scrubs_a_secret_before_it_reaches_a_db_row() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path(), "scrub-append.sqlite3");
    store
        .append(RunEvent {
            run_id: "r".into(),
            kind: "note".into(),
            payload: format!("saw {SECRET} here"),
        })
        .unwrap();
    assert_eq!(
        store.count_event_payloads_containing(SECRET).unwrap(),
        0,
        "append scrubbed the secret"
    );
}

// ── AC-12: coordinator decisions use only the DB, never a loose file ──────────

#[test]
fn coordinator_decisions_use_only_the_db_no_loose_files() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path(), "only.sqlite3");

    // A full cycle: enqueue → claim → append → finish.
    store.enqueue_runnable("run-1").unwrap();
    assert!(store
        .claim_ready(&WorkerId::new("w"), LEASE_NEVER_EXPIRES)
        .unwrap()
        .is_some());
    store
        .append(RunEvent {
            run_id: "run-1".into(),
            kind: "note".into(),
            payload: "p".into(),
        })
        .unwrap();
    assert!(store.finish_run("run-1", &WorkerId::new("w")).unwrap());

    // Only the coordinator DB (+ its WAL/SHM sidecars) exists — no JSONL was ever consulted or written.
    let names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(!names.is_empty());
    assert!(
        names.iter().all(|n| n.starts_with("only.sqlite3")),
        "coordinator touched only its DB, no loose files: {names:?}",
    );
}

// ── SW-AU-80B AC-2: CAS-guarded transitions never clobber a live run ──────────

#[test]
fn enqueue_runnable_never_clobbers_a_claimed_or_done_run() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path(), "cas-enqueue.sqlite3");

    assert!(store.enqueue_runnable("run-1").unwrap(), "fresh enqueue");
    assert!(
        store.enqueue_runnable("run-1").unwrap(),
        "an already-runnable run re-arms idempotently"
    );

    // Claim it — a re-enqueue must NOT reset a claimed run to runnable (the
    // double-dispatch footgun the SW-AU-80A deferral names).
    let claim = store
        .claim_ready(&WorkerId::new("w"), LEASE_NEVER_EXPIRES)
        .unwrap();
    assert_eq!(claim.unwrap().run_id, "run-1");
    assert!(
        !store.enqueue_runnable("run-1").unwrap(),
        "a claimed run is not clobbered back to runnable"
    );
    assert_eq!(store.count_runs_in_state(RunState::Claimed).unwrap(), 1);
    assert_eq!(store.count_runs_in_state(RunState::Runnable).unwrap(), 0);

    // A done run is likewise never resurrected.
    store.set_state_for_test("run-1", "done").unwrap();
    assert!(
        !store.enqueue_runnable("run-1").unwrap(),
        "a done run is not resurrected"
    );
    assert_eq!(store.count_runs_in_state(RunState::Done).unwrap(), 1);
}

// ── SW-AU-80B AC-3 / AC-4: lease reclaim + recover report ─────────────────────

#[test]
fn recover_reclaims_only_expired_leases() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path(), "reclaim.sqlite3");
    store.enqueue_runnable("a").unwrap();
    store.enqueue_runnable("b").unwrap();

    // Two claims: one with a past deadline, one far in the future.
    let expired = store
        .claim_ready(&WorkerId::new("w"), 100)
        .unwrap()
        .unwrap();
    let live = store
        .claim_ready(&WorkerId::new("w"), i64::MAX)
        .unwrap()
        .unwrap();
    assert_eq!(store.count_runs_in_state(RunState::Claimed).unwrap(), 2);

    let report = store.recover(200).unwrap();
    assert_eq!(
        report.reclaimed_claims, 1,
        "only the past-deadline lease is reclaimed"
    );
    assert!(
        report.runnable_runs.contains(&expired.run_id),
        "the expired-lease claim is runnable again"
    );
    assert!(
        !report.runnable_runs.contains(&live.run_id),
        "the live claim is left claimed"
    );
    assert_eq!(store.count_runs_in_state(RunState::Claimed).unwrap(), 1);
}

#[test]
fn recover_reports_durable_work_and_is_idempotent_under_fault() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recover.sqlite3");
    let store = SqliteOrchestrationStore::open(&path).unwrap();

    store.enqueue_runnable("runnable-1").unwrap();
    store.enqueue_runnable("expired-1").unwrap();
    // Claim one of the two runnable runs with an already-past lease (which of the two
    // is irrelevant — after reclaim both are runnable again).
    let _claimed = store
        .claim_ready(&WorkerId::new("w"), 100)
        .unwrap()
        .unwrap();
    store
        .seed_delivery_for_test("reactive", "d1", "pending")
        .unwrap();
    // A consumed delivery must NOT be re-offered.
    store
        .seed_delivery_for_test("reactive", "d2", "consumed")
        .unwrap();

    // Fault mid-recover: the reclaim rolls back and nothing is reported.
    fault::arm(FaultPoint::RecoverPostReclaimPreScan);
    let err = store.recover(200).unwrap_err();
    assert!(matches!(
        err,
        StoreError::InjectedFault(FaultPoint::RecoverPostReclaimPreScan)
    ));
    assert_eq!(
        store.count_runs_in_state(RunState::Claimed).unwrap(),
        1,
        "reclaim rolled back — the run is still claimed"
    );

    // Retry (fault consumed): reclaim + full report.
    let report = store.recover(200).unwrap();
    assert_eq!(report.reclaimed_claims, 1);
    let mut runnable = report.runnable_runs.clone();
    runnable.sort();
    assert_eq!(
        runnable,
        vec!["expired-1".to_string(), "runnable-1".to_string()],
        "the reclaimed run and the already-runnable run are both re-offered"
    );
    assert_eq!(
        report.pending_deliveries,
        vec![DeliveryRef {
            adapter_id: "reactive".into(),
            delivery_key: "d1".into(),
        }],
        "only the pending delivery is re-offered, never the consumed one"
    );
}

// ── SW-AU-80B: schema migration of a pre-lease (80A-shape) coordinator file ────

#[test]
fn open_migrates_a_pre_lease_80a_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.sqlite3");
    // Seed the exact SW-AU-80A shape: `runs` without `lease_expiry`, no `deliveries`.
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE runs (run_id TEXT PRIMARY KEY, state TEXT NOT NULL, claimed_by TEXT);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO runs (run_id, state) VALUES ('r1', 'runnable')",
            [],
        )
        .unwrap();
    }

    // Reopen with the current build: the migration adds `lease_expiry`, so a full
    // claim → recover cycle works on the upgraded file rather than hitting
    // "no such column: lease_expiry".
    let store = SqliteOrchestrationStore::open(&path).unwrap();
    let claim = store
        .claim_ready(&WorkerId::new("w"), 100)
        .unwrap()
        .unwrap();
    assert_eq!(claim.run_id, "r1");
    assert_eq!(
        store.recover(200).unwrap().reclaimed_claims,
        1,
        "the migrated claim reclaims on an upgraded DB"
    );
    // The new `deliveries` table was created on the old DB too.
    store
        .seed_delivery_for_test("reactive", "s1", "pending")
        .unwrap();

    // Reopening the already-migrated file is a no-op (the common every-startup path):
    // `ensure_column` sees `lease_expiry` present and does not re-`ALTER`.
    drop(store);
    let reopened = SqliteOrchestrationStore::open(&path).unwrap();
    assert!(
        reopened.enqueue_runnable("r2").unwrap(),
        "upgraded DB is fully usable"
    );
}

// ── SW-AU-80D: migration of an 80B-shape `deliveries` (no `run_id`) ────────────

#[test]
fn open_migrates_an_80b_deliveries_table_and_the_added_fk_is_live() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old-deliveries.sqlite3");
    // Seed the exact SW-AU-80B shape: a `deliveries` table WITHOUT `run_id` (its fired-run link
    // arrives with SW-AU-80D via `ensure_column`'s `ALTER ... ADD COLUMN run_id ... REFERENCES`).
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE runs (run_id TEXT PRIMARY KEY, state TEXT NOT NULL, claimed_by TEXT, lease_expiry INTEGER);
             CREATE TABLE deliveries (
                 adapter_id   TEXT NOT NULL,
                 delivery_key TEXT NOT NULL,
                 state        TEXT NOT NULL DEFAULT 'pending',
                 PRIMARY KEY (adapter_id, delivery_key)
             );",
        )
        .unwrap();
    }

    // Reopen with the current build: `ensure_column` performs the `ALTER TABLE deliveries ADD
    // COLUMN run_id TEXT REFERENCES runs(run_id) ...`, so a full enqueue-on-accept links a real
    // delivery on the upgraded file rather than hitting "no such column: run_id".
    let store = SqliteOrchestrationStore::open(&path).unwrap();
    store
        .seed_delivery_for_test("reactive", "d1", "pending")
        .unwrap();
    assert!(store
        .enqueue_on_accept("run-1", POINTER, Some(&deliv("reactive", "d1")))
        .unwrap());
    assert_eq!(
        store.run_request_json("run-1").unwrap().as_deref(),
        Some(POINTER)
    );
    // The FK on the ADDED column is genuinely enforced (not an inert ALTER): a dangling link is
    // still rejected on the upgraded DB — the property `ensure_column`'s comment asserts.
    let err = store
        .try_link_delivery_to_missing_run_for_test("reactive", "d2", "ghost-run")
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Sqlite(_)),
        "the FK added by ALTER rejects a dangling delivery link on an upgraded 80B DB"
    );
}

#[test]
fn recover_treats_a_lease_exactly_at_the_cutoff_as_live() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path(), "lease-boundary.sqlite3");
    store.enqueue_runnable("r").unwrap();
    store.claim_ready(&WorkerId::new("w"), 200).unwrap();

    // `lease_expiry < now_cutoff` is strict: a lease exactly at the cutoff is still live.
    assert_eq!(
        store.recover(200).unwrap().reclaimed_claims,
        0,
        "a lease exactly at the cutoff is not reclaimed"
    );
    // One tick past the deadline, it reclaims.
    assert_eq!(
        store.recover(201).unwrap().reclaimed_claims,
        1,
        "a lease strictly before the cutoff is reclaimed"
    );
}

// ── SW-AU-80D: durable enqueue-on-accept (producer) ───────────────────────────

const POINTER: &str = r#"{"target_selector":"epic:SE-7","cause":{"cause":"manual"},"trigger_source":"other","fired_at":"t"}"#;

fn deliv(adapter: &str, key: &str) -> DeliveryRef {
    DeliveryRef {
        adapter_id: adapter.into(),
        delivery_key: key.into(),
    }
}

#[test]
fn enqueue_on_accept_writes_runnable_row_pointer_and_consumes_delivery_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("enqueue.sqlite3");
    {
        let store = SqliteOrchestrationStore::open(&path).unwrap();
        // A reactive fire first lands its delivery pending (SW-AU-80B), then this story
        // consumes it while enqueuing the runnable run — one atomic transaction.
        store
            .seed_delivery_for_test("reactive", "d1", "pending")
            .unwrap();
        assert!(store
            .enqueue_on_accept("run-1", POINTER, Some(&deliv("reactive", "d1")))
            .unwrap());

        assert_eq!(store.count_runs_in_state(RunState::Runnable).unwrap(), 1);
        assert_eq!(
            store.run_request_json("run-1").unwrap().as_deref(),
            Some(POINTER),
            "the durable run-request pointer is persisted"
        );
        // The consumed delivery is no longer pending, so `recover` never re-offers it.
        assert!(
            store.recover(0).unwrap().pending_deliveries.is_empty(),
            "the fired delivery is consumed, not left pending"
        );
    } // teardown

    // A fresh process over the same DB still sees the durable runnable row + pointer — the
    // ready work a claim-driven dispatcher (SW-AU-80E) recovers after a crash.
    let store = SqliteOrchestrationStore::open(&path).unwrap();
    let report = store.recover(0).unwrap();
    assert_eq!(report.runnable_runs, vec!["run-1".to_string()]);
    assert_eq!(
        store.run_request_json("run-1").unwrap().as_deref(),
        Some(POINTER)
    );
}

#[test]
fn enqueue_on_accept_with_no_delivery_still_commits_runnable_and_pointer() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path(), "enqueue-neutral.sqlite3");
    // A dedup-neutral fire (manual/connector, empty key) records no delivery — the runnable
    // row + pointer still commit atomically.
    assert!(store.enqueue_on_accept("run-1", POINTER, None).unwrap());
    assert_eq!(store.count_runs_in_state(RunState::Runnable).unwrap(), 1);
    assert!(store.run_request_json("run-1").unwrap().is_some());
}

#[test]
fn enqueue_on_accept_is_atomic_on_injected_fault_and_retries_idempotently() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path(), "enqueue-fault.sqlite3");
    store
        .seed_delivery_for_test("reactive", "d1", "pending")
        .unwrap();

    fault::arm(FaultPoint::EnqueuePostRunnablePrePointer);
    let err = store
        .enqueue_on_accept("run-1", POINTER, Some(&deliv("reactive", "d1")))
        .unwrap_err();
    assert!(matches!(
        err,
        StoreError::InjectedFault(FaultPoint::EnqueuePostRunnablePrePointer)
    ));
    // All-or-nothing: the runnable row, the pointer, and the delivery-consume all rolled back.
    assert_eq!(store.count_runs_in_state(RunState::Runnable).unwrap(), 0);
    assert!(store.run_request_json("run-1").unwrap().is_none());
    assert_eq!(
        store.recover(0).unwrap().pending_deliveries,
        vec![deliv("reactive", "d1")],
        "the delivery is still pending — its consume rolled back with the run"
    );

    // The one-shot fault is spent; a retry now commits cleanly and idempotently.
    assert!(store
        .enqueue_on_accept("run-1", POINTER, Some(&deliv("reactive", "d1")))
        .unwrap());
    assert_eq!(store.count_runs_in_state(RunState::Runnable).unwrap(), 1);
    assert!(store.run_request_json("run-1").unwrap().is_some());
}

#[test]
fn enqueue_on_accept_is_idempotent_for_the_same_run_id() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path(), "enqueue-idem.sqlite3");
    // A deterministic run_id means a replayed accept enqueues exactly one runnable row and
    // one pointer — the second call re-arms idempotently, never a duplicate.
    assert!(store.enqueue_on_accept("run-1", POINTER, None).unwrap());
    assert!(
        store.enqueue_on_accept("run-1", POINTER, None).unwrap(),
        "a replay of an already-runnable run re-arms"
    );
    assert_eq!(store.count_runs_in_state(RunState::Runnable).unwrap(), 1);
}

#[test]
fn enqueue_on_accept_never_clobbers_an_occupied_run() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path(), "enqueue-occupied.sqlite3");
    store.enqueue_on_accept("run-1", POINTER, None).unwrap();
    store
        .claim_ready(&WorkerId::new("w"), LEASE_NEVER_EXPIRES)
        .unwrap();

    // A claimed run is occupied: a second accept carrying a fresh delivery is a duplicate
    // (`false`) and neither resets the run nor overwrites the pointer — but it STILL consumes
    // its delivery against the in-flight run, so `recover` never re-offers it forever.
    store
        .seed_delivery_for_test("reactive", "d2", "pending")
        .unwrap();
    assert!(!store
        .enqueue_on_accept("run-1", "{\"other\":true}", Some(&deliv("reactive", "d2")))
        .unwrap());
    assert_eq!(store.count_runs_in_state(RunState::Claimed).unwrap(), 1);
    assert_eq!(store.count_runs_in_state(RunState::Runnable).unwrap(), 0);
    assert_eq!(
        store.run_request_json("run-1").unwrap().as_deref(),
        Some(POINTER),
        "the original pointer is not overwritten by an occupied re-accept"
    );
    assert!(
        store.recover(0).unwrap().pending_deliveries.is_empty(),
        "the delivery is consumed against the occupied run, not stranded pending"
    );
}

#[test]
fn deliveries_foreign_key_rejects_a_link_to_a_missing_run() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path(), "deliv-fk.sqlite3");
    let err = store
        .try_link_delivery_to_missing_run_for_test("reactive", "d1", "ghost-run")
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Sqlite(_)),
        "the deferred FK rejects a delivery linked to a non-existent run"
    );
    // The normal path (run written first, in the same transaction) links cleanly.
    store.enqueue_runnable("run-ok").unwrap();
    store
        .seed_delivery_for_test("reactive", "d2", "pending")
        .unwrap();
    assert!(store
        .enqueue_on_accept("run-ok", POINTER, Some(&deliv("reactive", "d2")))
        .unwrap());
}

#[test]
fn enqueue_on_accept_scrubs_a_secret_in_the_pointer() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path(), "enqueue-scrub.sqlite3");
    let with_secret = format!(r#"{{"cause":{{"cause":"manual"}},"blob":"{SECRET}"}}"#);
    store
        .enqueue_on_accept("run-1", &with_secret, None)
        .unwrap();
    let stored = store.run_request_json("run-1").unwrap().unwrap();
    assert!(
        !stored.contains(SECRET),
        "the run-request pointer is scrubbed through the one redaction owner:\n{stored}"
    );
}

// ── SW-AU-80E: `finish_run` — the terminal CAS the claim loop calls ──────────

#[test]
fn finish_run_retires_a_claimed_run_and_will_not_resurrect() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path(), "finish.sqlite3");
    let w = WorkerId::new("w");
    store.enqueue_runnable("run-1").unwrap();

    // A runnable (un-claimed) run is not this worker's to finish → false, no change.
    assert!(
        !store.finish_run("run-1", &w).unwrap(),
        "finish_run only retires a claimed run — a runnable one is left untouched"
    );
    assert_eq!(store.count_runs_in_state(RunState::Runnable).unwrap(), 1);

    // Claim it, then finish: claimed → done exactly once.
    let claim = store
        .claim_ready(&w, LEASE_NEVER_EXPIRES)
        .unwrap()
        .expect("a runnable run is claimable");
    assert_eq!(claim.run_id, "run-1");
    assert!(
        store.finish_run("run-1", &w).unwrap(),
        "claimed → done returns true"
    );
    assert_eq!(store.count_runs_in_state(RunState::Done).unwrap(), 1);
    assert_eq!(store.count_runs_in_state(RunState::Claimed).unwrap(), 0);

    // A second finish is a no-op: a done run is never resurrected or re-retired.
    assert!(
        !store.finish_run("run-1", &w).unwrap(),
        "finishing an already-done run returns false (idempotent, no resurrection)"
    );
    // An unknown run id likewise changes nothing.
    assert!(!store.finish_run("ghost", &w).unwrap());
}

// SW-AU-80E: a stale worker whose run was reclaimed (lease expiry) and re-claimed by a DIFFERENT
// worker must NOT be able to finish it — the CAS guards on `claimed_by`, not just `state='claimed'`.
#[test]
fn finish_run_will_not_clobber_a_run_reclaimed_by_another_worker() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path(), "finish-race.sqlite3");
    let stale = WorkerId::new("stale");
    let fresh = WorkerId::new("fresh");
    store.enqueue_runnable("run-1").unwrap();

    // `stale` claims with a lease in the past; it then "hangs" (never finishes).
    store
        .claim_ready(&stale, 100)
        .unwrap()
        .expect("claimable by stale");
    // Restart recovery reclaims the lease-expired claim, and `fresh` re-claims the run.
    assert_eq!(store.recover(200).unwrap().reclaimed_claims, 1);
    let reclaimed = store
        .claim_ready(&fresh, i64::MAX)
        .unwrap()
        .expect("re-offered run is re-claimable by fresh");
    assert_eq!(reclaimed.run_id, "run-1");

    // The stale worker finally returns and tries to finish — it must NOT clobber `fresh`'s claim.
    assert!(
        !store.finish_run("run-1", &stale).unwrap(),
        "a stale worker cannot finish a run now claimed by another worker"
    );
    assert_eq!(
        store.count_runs_in_state(RunState::Claimed).unwrap(),
        1,
        "the run stays claimed by the fresh worker"
    );
    // The fresh (real) owner finishes it exactly once.
    assert!(store.finish_run("run-1", &fresh).unwrap());
    assert_eq!(store.count_runs_in_state(RunState::Done).unwrap(), 1);
}

// ── AC-7: the shared-helper extraction preserves the post-WAL `ensure_column`
//         heal (a fresh-DB test would miss it) ─────────────────────────────────

#[test]
fn ensure_column_heal_survives_helper_extraction() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("coordinator.sqlite3");
    // Fresh open builds the schema (with lease_expiry) and stamps WAL.
    let _store = open(dir.path(), "coordinator.sqlite3");

    // Simulate an upgraded-from-older DB: drop a column that `open`'s
    // `ensure_column` reconciliation re-adds. If the extraction had folded the
    // post-WAL step into the shared helper, the reopen below would not heal.
    {
        let conn = crate::shared::sqlite::open_conn(&path).unwrap();
        conn.execute_batch("ALTER TABLE runs DROP COLUMN lease_expiry;")
            .unwrap();
    }

    // Reopen: the store's own `ensure_column` must re-add lease_expiry.
    let _reopened = SqliteOrchestrationStore::open(&path).expect("reopen heals");

    let conn = crate::shared::sqlite::open_conn(&path).unwrap();
    let cols: Vec<String> = {
        let mut stmt = conn.prepare("PRAGMA table_info(runs)").unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(1)).unwrap();
        rows.collect::<Result<_, _>>().unwrap()
    };
    assert!(
        cols.iter().any(|c| c == "lease_expiry"),
        "lease_expiry heal lost after extraction; columns = {cols:?}"
    );
}
