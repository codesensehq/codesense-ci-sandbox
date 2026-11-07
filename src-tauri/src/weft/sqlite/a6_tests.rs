// SPDX-License-Identifier: MIT
//! SW-WA-A6 — the fleet live-count read at the STORE SEAM. The concurrency cap's
//! spare-slot input is `wv_count_live_workers(scope)`: it must count exactly the LIVE
//! child workers (`provision`/`dispatch`/`signal`) for a scope and exclude terminal
//! (`handoff`/`deprovision`), parked (`needs-attention`), peer, and other-scope rows —
//! so the cap decision the controller applies is grounded in the real roster, not the
//! trigger payload (a dedup miss must read the true fleet size).

use super::*;
use crate::weft::concurrency_cap::{dispatch_admittance, CapDecision, DEFAULT_CONCURRENCY_CAP};
use crate::weft::store::WeftStore;

fn open_temp() -> (tempfile::TempDir, WeftStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
    (dir, store)
}

/// Seed a roster row with a chosen `status` + `relation` (`child:<scope>` or `peer`),
/// so the fleet count can be exercised without the whole spawn path. A TEST seed.
fn seed_row(store: &WeftStore, name: &str, status: &str, relation: &str) {
    let conn = store.conn().unwrap();
    conn.execute(
        "INSERT INTO wv_roster (name, status, relation, lifecycle, created_at, updated_at) \
         VALUES (?1, ?2, ?3, 'task', '', '') \
         ON CONFLICT(name) DO UPDATE SET status=excluded.status, relation=excluded.relation",
        rusqlite::params![name, status, relation],
    )
    .unwrap();
}

#[test]
fn counts_only_live_child_workers_for_the_scope() {
    let (_dir, store) = open_temp();
    // Three live workers in scope `root`.
    seed_row(&store, "w-provision", "provision", "child:root");
    seed_row(&store, "w-dispatch", "dispatch", "child:root");
    seed_row(&store, "w-signal", "signal", "child:root");
    // Terminal + parked rows in scope `root` — never counted as live.
    seed_row(&store, "w-handoff", "handoff", "child:root");
    seed_row(&store, "w-deprovision", "deprovision", "child:root");
    seed_row(&store, "w-parked", "needs-attention", "child:root");
    // A live worker in a DIFFERENT scope — must not leak into `root`'s count.
    seed_row(&store, "w-other", "dispatch", "child:other");
    // A peer (observe-only) row — never a managed live worker.
    seed_row(&store, "peer-a", "dispatch", "peer");

    let live = wv_count_live_workers(&store, "root").unwrap();
    assert_eq!(
        live, 3,
        "exactly the three live child workers in scope root"
    );

    let other = wv_count_live_workers(&store, "other").unwrap();
    assert_eq!(other, 1, "the other scope has its own independent count");

    // An empty scope reads zero (never an error).
    let empty = wv_count_live_workers(&store, "no-such-scope").unwrap();
    assert_eq!(empty, 0);
}

#[test]
fn cap_admits_below_capacity_and_parks_at_capacity_through_the_real_count() {
    let (_dir, store) = open_temp();
    // Seed up to (cap - 1) live workers → one spare slot → admit.
    for i in 0..(DEFAULT_CONCURRENCY_CAP - 1) {
        seed_row(&store, &format!("w{i}"), "dispatch", "child:root");
    }
    let live = wv_count_live_workers(&store, "root").unwrap();
    let decision = dispatch_admittance(DEFAULT_CONCURRENCY_CAP, live);
    assert!(
        decision.admits(),
        "with a spare slot the cap admits the next auto-dispatch"
    );

    // Fill the last slot → at capacity → the next auto-dispatch parks.
    seed_row(&store, "w-last", "dispatch", "child:root");
    let live_full = wv_count_live_workers(&store, "root").unwrap();
    assert_eq!(live_full, DEFAULT_CONCURRENCY_CAP);
    let parked = dispatch_admittance(DEFAULT_CONCURRENCY_CAP, live_full);
    assert_eq!(
        parked,
        CapDecision::Park {
            cap: DEFAULT_CONCURRENCY_CAP,
            live: DEFAULT_CONCURRENCY_CAP
        },
        "a full fleet parks the overflow instead of fanning out another worker"
    );
}
