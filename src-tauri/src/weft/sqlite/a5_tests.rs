// SPDX-License-Identifier: MIT
//! SW-WA-A5 acceptance tests — durable roster `session_id` / `run_id` correlation
//! columns at the STORE SEAM.
//!
//! Grounds the fix for DW-B3-3 / DW-WV-A4-1: the durable roster carried a live
//! `session_id` but no `run_id`, yet `resolve_done_record_context` already SELECTs
//! `run_id FROM wv_roster` (a latent `no such column: run_id` bug on any real store).
//! These tests assert (AC-1) the column exists and forward-migrates, (AC-2) the DTO
//! round-trips both correlation columns through `wv_upsert_roster` / `wv_list_roster`,
//! (AC-3) the spawn `provision → dispatch` transition stamps the live `run_id`
//! alongside `session_id`, and (AC-4) `resolve_done_record_context` reads the stamped
//! `run_id` back through the real column.

use super::*;
use crate::weft::store::WeftStore;

fn open_temp() -> (tempfile::TempDir, WeftStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
    (dir, store)
}

fn roster_columns(store: &WeftStore) -> Vec<String> {
    let conn = store.conn().unwrap();
    let mut stmt = conn.prepare("PRAGMA table_info(wv_roster)").unwrap();
    stmt.query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

// ---------- AC-1: run_id column exists + forward-migrates ----------

#[test]
fn roster_has_session_id_and_run_id_columns() {
    let (_dir, store) = open_temp();
    let cols = roster_columns(&store);
    for expected in ["session_id", "run_id"] {
        assert!(
            cols.iter().any(|c| c == expected),
            "wv_roster must carry the {expected:?} correlation column; got {cols:?}"
        );
    }
}

#[test]
fn run_id_column_forward_migrates_onto_a_preexisting_roster() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("weft.sqlite3");
    // Simulate a pre-A5 store: a roster with a session_id column but no run_id.
    {
        let conn = crate::shared::sqlite::open_wal_db(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE wv_roster (name TEXT PRIMARY KEY, session_id TEXT);\
             INSERT INTO wv_roster (name, session_id) VALUES ('SW-OLD', 'sess-old');",
        )
        .unwrap();
    }
    // Opening the real store forward-migrates: run_id is healed on, the old row survives.
    let store = WeftStore::open(&path).unwrap();
    let cols = roster_columns(&store);
    assert!(
        cols.iter().any(|c| c == "run_id"),
        "run_id must be healed onto a pre-A5 roster; got {cols:?}"
    );
    let conn = store.conn().unwrap();
    let (session_id, run_id): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT session_id, run_id FROM wv_roster WHERE name='SW-OLD'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(session_id.as_deref(), Some("sess-old"), "old row survives");
    assert_eq!(run_id, None, "a forward-migrated row reads a NULL run_id");
}

// ---------- AC-2: DTO threads session_id + run_id ----------

#[test]
fn upsert_and_list_round_trip_session_id_and_run_id() {
    let (_dir, store) = open_temp();
    let row = WvRosterRow {
        name: "SW-WA-A5".to_string(),
        workspace: None,
        pane: None,
        kind: None,
        subclass: Some("impl".to_string()),
        lifecycle: "task".to_string(),
        owned: None,
        orch_owner: None,
        relation: "child:root".to_string(),
        parent: Some("root".to_string()),
        current_task: None,
        cwd: None,
        status: Some("dispatch".to_string()),
        session_id: Some("sess-live-1".to_string()),
        run_id: Some("run-live-1".to_string()),
    };
    wv_upsert_roster(&store, &row).unwrap();

    let rows = wv_list_roster(&store).unwrap();
    let got = rows.iter().find(|r| r.name == "SW-WA-A5").unwrap();
    assert_eq!(
        got.session_id.as_deref(),
        Some("sess-live-1"),
        "session_id survives the upsert → list round-trip"
    );
    assert_eq!(
        got.run_id.as_deref(),
        Some("run-live-1"),
        "run_id survives the upsert → list round-trip"
    );
}

#[test]
fn upsert_updates_the_correlation_columns_on_conflict() {
    let (_dir, store) = open_temp();
    let mut row = WvRosterRow {
        name: "SW-WA-A5".to_string(),
        workspace: None,
        pane: None,
        kind: None,
        subclass: None,
        lifecycle: "task".to_string(),
        owned: None,
        orch_owner: None,
        relation: "peer".to_string(),
        parent: None,
        current_task: None,
        cwd: None,
        status: None,
        session_id: Some("sess-old".to_string()),
        run_id: Some("run-old".to_string()),
    };
    wv_upsert_roster(&store, &row).unwrap();
    row.session_id = Some("sess-new".to_string());
    row.run_id = Some("run-new".to_string());
    wv_upsert_roster(&store, &row).unwrap();

    let rows = wv_list_roster(&store).unwrap();
    assert_eq!(
        rows.len(),
        1,
        "still one row (PK conflict updated, not inserted)"
    );
    assert_eq!(rows[0].session_id.as_deref(), Some("sess-new"));
    assert_eq!(rows[0].run_id.as_deref(), Some("run-new"));
}

// ---------- AC-3: spawn dispatch transition stamps run_id ----------

#[test]
fn dispatch_transition_stamps_session_id_and_run_id() {
    let (_dir, store) = open_temp();
    wv_provision_worker_row(&store, "SW-X", "impl", "root").unwrap();
    let outcome = wv_dispatch_worker_row(&store, "SW-X", "sess-x", Some("run-x")).unwrap();
    assert_eq!(outcome, MutateResult::Updated);

    let conn = store.conn().unwrap();
    let (session_id, run_id, status): (Option<String>, Option<String>, Option<String>) = conn
        .query_row(
            "SELECT session_id, run_id, status FROM wv_roster WHERE name='SW-X'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(session_id.as_deref(), Some("sess-x"));
    assert_eq!(
        run_id.as_deref(),
        Some("run-x"),
        "run_id stamped at dispatch"
    );
    assert_eq!(status.as_deref(), Some("dispatch"));
}

#[test]
fn dispatch_transition_with_no_run_id_leaves_it_null() {
    let (_dir, store) = open_temp();
    wv_provision_worker_row(&store, "SW-Y", "impl", "root").unwrap();
    wv_dispatch_worker_row(&store, "SW-Y", "sess-y", None).unwrap();

    let conn = store.conn().unwrap();
    let run_id: Option<String> = conn
        .query_row("SELECT run_id FROM wv_roster WHERE name='SW-Y'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        run_id, None,
        "a dispatch with no run_id leaves the column NULL"
    );
}

// ---------- AC-4: resolve_done_record_context reads the stamped run_id ----------

#[test]
fn resolve_done_record_context_returns_the_stamped_run_id() {
    let (_dir, store) = open_temp();
    // A live worker on a scope with a claimed queue row (the done-record context).
    wv_provision_worker_row(&store, "SW-Z", "impl", "root").unwrap();
    wv_dispatch_worker_row(&store, "SW-Z", "sess-z", Some("run-z")).unwrap();
    {
        // A claimed queue row for scope root so resolve() finds a queue_id.
        let conn = store.conn().unwrap();
        conn.execute(
            "INSERT INTO wv_queue (id, scope, state) VALUES (1, NULL, 'claimed')",
            [],
        )
        .unwrap();
    }
    let ctx = store
        .resolve_done_record_context("sess-z")
        .unwrap()
        .expect("a live dispatched worker resolves a done-record context");
    let (_queue_id, run_id, story_id, _subclass) = ctx;
    assert_eq!(
        run_id, "run-z",
        "the stamped roster run_id is returned verbatim"
    );
    assert_eq!(story_id, "SW-Z");
}

#[test]
fn resolve_done_record_context_falls_back_to_story_id_without_run_id() {
    let (_dir, store) = open_temp();
    wv_provision_worker_row(&store, "SW-NR", "impl", "root").unwrap();
    wv_dispatch_worker_row(&store, "SW-NR", "sess-nr", None).unwrap();
    {
        let conn = store.conn().unwrap();
        conn.execute(
            "INSERT INTO wv_queue (id, scope, state) VALUES (1, NULL, 'claimed')",
            [],
        )
        .unwrap();
    }
    let (_queue_id, run_id, story_id, _subclass) = store
        .resolve_done_record_context("sess-nr")
        .unwrap()
        .expect("context resolves even without a run_id");
    assert_eq!(
        run_id, story_id,
        "run_id falls back to story_id when the roster carries none"
    );
}
