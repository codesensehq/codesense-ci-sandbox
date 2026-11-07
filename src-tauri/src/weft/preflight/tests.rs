// SPDX-License-Identifier: MIT
//! SW-WV-F2 tests for the dispatch preflight core.
//!
//! The `#[tauri::command]` `preflight_wv_dispatch` needs an `AppHandle` +
//! active-team context that no unit harness provides (DW-WV-E1-1 — no Tauri
//! `MockRuntime` in this crate), so these cover the two pure/store-layer cores it
//! composes plus the off-thread static guard (AC-7):
//! - `WvPreflightResult::from_dimensions` — the verdict + first-failing message fold.
//! - `queue_and_capacity_blocking` — the queue-row + capacity reads against a real
//!   temp `weft.sqlite3`.
//! - `no_raw_blocking_guard` — no `with_blocking(` escape-hatch mint exists in the
//!   module (all blocking work is the safe `await_blocking`/`spawn_blocking` path).
//!
//! RI-2-ALLOW-FILE: this test file references the `with_blocking(` token only as a
//! string literal inside its own backstop assertion + message (the `no_raw_blocking_guard`
//! check above), never as a real blocking call — so the crate-wide RI-2 mint scanner
//! opts this whole file out. (Pre-existing baseline false-positive, greened here.)

use rusqlite::params;

use super::*;
use crate::weft::store::WeftStore;

fn open_temp() -> (tempfile::TempDir, WeftStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
    (dir, store)
}

fn seed_queue_row(store: &WeftStore, id: i64, state: &str) {
    let conn = store.conn().unwrap();
    conn.execute(
        "INSERT INTO wv_queue (id, source, state) VALUES (?1, 'test', ?2)",
        params![id, state],
    )
    .unwrap();
}

fn seed_roster_row(store: &WeftStore, name: &str, status: &str) {
    let conn = store.conn().unwrap();
    conn.execute(
        "INSERT INTO wv_roster (name, status) VALUES (?1, ?2)",
        params![name, status],
    )
    .unwrap();
}

#[test]
fn from_dimensions_ok_when_all_true() {
    let r = WvPreflightResult::from_dimensions(true, true);
    assert!(r.ok);
    assert!(r.message.is_none());
}

#[test]
fn from_dimensions_names_the_first_failing_dimension() {
    // Queue-row first.
    let r = WvPreflightResult::from_dimensions(false, false);
    assert!(!r.ok);
    assert!(r.message.unwrap().to_lowercase().contains("pending"));

    // Then capacity (queue-row ok).
    let r = WvPreflightResult::from_dimensions(true, false);
    assert!(!r.ok);
    assert!(r.message.unwrap().to_lowercase().contains("idle"));
}

#[test]
fn queue_row_ok_true_when_no_queue_id() {
    let (_dir, store) = open_temp();
    // No queue_id supplied (header "+ Dispatch") → the queue-row check is skipped.
    // Empty roster → capacity is allowed (first dispatch).
    let (queue_row_ok, capacity_ok) = queue_and_capacity_blocking(&store, None).unwrap();
    assert!(queue_row_ok);
    assert!(capacity_ok);
}

#[test]
fn queue_row_ok_false_for_absent_queue_id() {
    let (_dir, store) = open_temp();
    let (queue_row_ok, _) = queue_and_capacity_blocking(&store, Some(999)).unwrap();
    assert!(!queue_row_ok);
}

#[test]
fn queue_row_ok_true_for_pending_row_false_for_claimed() {
    let (_dir, store) = open_temp();
    seed_queue_row(&store, 42, "pending");
    seed_queue_row(&store, 43, "claimed");

    let (pending_ok, _) = queue_and_capacity_blocking(&store, Some(42)).unwrap();
    assert!(pending_ok, "a pending row is dispatchable");

    let (claimed_ok, _) = queue_and_capacity_blocking(&store, Some(43)).unwrap();
    assert!(!claimed_ok, "a non-pending row is not dispatchable");
}

#[test]
fn capacity_ok_true_regardless_of_live_workers() {
    // F2 has no worker cap and the roster has no `idle` state, so capacity is always
    // available — a populated roster (live `provision`/`dispatch`/`signal` workers)
    // must NOT block a new dispatch (the pre-fix `status='idle'` query blocked every
    // dispatch the moment any worker existed). B5/scopes will wire a real ceiling.
    let (_dir, store) = open_temp();
    let (_, empty_ok) = queue_and_capacity_blocking(&store, None).unwrap();
    assert!(empty_ok);

    seed_roster_row(&store, "w-1", "provision");
    seed_roster_row(&store, "w-2", "dispatch");
    let (_, populated_ok) = queue_and_capacity_blocking(&store, None).unwrap();
    assert!(populated_ok, "a populated roster must not block dispatch");
}

#[test]
fn no_raw_blocking_guard() {
    // AC-7: no `with_blocking(` escape-hatch mint in the preflight module — all
    // blocking SQLite work goes through the safe `await_blocking`/`spawn_blocking`
    // path (which needs no `RI-2-ALLOW` marker).
    let src = include_str!("../preflight.rs");
    assert!(
        !src.contains("with_blocking("),
        "preflight.rs must not mint a with_blocking( escape hatch"
    );
}
