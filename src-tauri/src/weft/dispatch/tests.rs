// SPDX-License-Identifier: MIT
//! SW-WV-C1 acceptance tests for the 4-field dispatch record + done-when contract.
//!
//! Covers AC-1 (four-field structural brief + acceptance/task required), AC-2
//! (written before start, externally checkable read-back), AC-3 (done-when
//! evaluation `open → done` CAS + typed refusals), AC-4 (`dispatch_done_by_session`
//! resolves the roster key and advances — the hook's core; the wiring into
//! `build_signal_done_hooks` is code-review-verified per DW-WV-A3-1/3), AC-6 (audit
//! co-commit). The two-seam registry cross-check + `EXPECTED_MCP_TOOLS` live in
//! `mcp/handler.rs` + `commands/interactive_gate.rs` alongside the verb.

use rusqlite::params;

use super::*;
use crate::weft::store::WeftStore;

fn open_temp() -> (tempfile::TempDir, WeftStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
    (dir, store)
}

/// Seed a live dispatched worker roster row (A2/A3 shape) so
/// `dispatch_done_by_session` can resolve `session_id → story_id`. Uses the
/// sanctioned A2 provision→dispatch handlers so the row matches production.
fn seed_dispatched_worker(store: &WeftStore, story_id: &str, session_id: &str) {
    crate::weft::wv_provision_worker_row(store, story_id, "impl", "root").unwrap();
    let r = crate::weft::wv_dispatch_worker_row(store, story_id, session_id, None).unwrap();
    assert_eq!(r, crate::weft::MutateResult::Updated);
}

fn read_state(store: &WeftStore, id: i64) -> Option<String> {
    let conn = store.conn().unwrap();
    conn.query_row(
        "SELECT state FROM wv_dispatch_records WHERE id=?1",
        params![id],
        |r| r.get(0),
    )
    .ok()
}

fn audit_ops_for(store: &WeftStore) -> Vec<(String, String)> {
    let conn = store.conn().unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT operation, tbl FROM wv_write_log WHERE tbl='wv_dispatch_records' ORDER BY id",
        )
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

// ---- AC-1: the four-field structural brief, task + acceptance required. ----

#[test]
fn write_dispatch_persists_the_four_fields() {
    let (_d, store) = open_temp();
    let id = wv_write_dispatch(
        &store,
        "SW-X-1",
        "Do the thing",
        "here is context",
        "do not touch prod",
        "the tests pass and the PR is green",
        Some("root"),
    )
    .unwrap();
    let recs = dispatch_state(&store, "SW-X-1").unwrap();
    assert_eq!(recs.len(), 1);
    let r = &recs[0];
    assert_eq!(r.id, id);
    assert_eq!(r.task, "Do the thing");
    assert_eq!(r.context, "here is context");
    assert_eq!(r.constraints, "do not touch prod");
    assert_eq!(r.acceptance, "the tests pass and the PR is green");
    assert_eq!(r.state, "open");
    assert_eq!(r.scope.as_deref(), Some("root"));
    assert!(r.session_id.is_none());
    assert!(r.done_at.is_none());
    assert!(!r.created_at.is_empty());
}

#[test]
fn empty_context_and_constraints_are_allowed() {
    let (_d, store) = open_temp();
    // Not every job constrains — only task + acceptance are required.
    let id = wv_write_dispatch(&store, "SW-X-2", "task", "", "", "done when merged", None).unwrap();
    let recs = dispatch_state(&store, "SW-X-2").unwrap();
    assert_eq!(recs[0].id, id);
    assert_eq!(recs[0].context, "");
    assert_eq!(recs[0].constraints, "");
}

#[test]
fn blank_acceptance_is_refused_no_checkable_finish_line() {
    let (_d, store) = open_temp();
    let err = wv_write_dispatch(&store, "SW-X-3", "task", "", "", "   ", None).unwrap_err();
    assert!(
        matches!(
            err,
            WeftStoreError::Dispatch(DispatchError::MissingRequiredField {
                field: "acceptance"
            })
        ),
        "a blank done-when must be refused, got {err:?}"
    );
    // Nothing was written.
    assert!(dispatch_state(&store, "SW-X-3").unwrap().is_empty());
}

#[test]
fn blank_task_is_refused() {
    let (_d, store) = open_temp();
    let err = wv_write_dispatch(&store, "SW-X-4", "  ", "", "", "done when x", None).unwrap_err();
    assert!(matches!(
        err,
        WeftStoreError::Dispatch(DispatchError::MissingRequiredField { field: "task" })
    ));
}

#[test]
fn blank_story_id_is_refused() {
    let (_d, store) = open_temp();
    let err = wv_write_dispatch(&store, "", "task", "", "", "done when x", None).unwrap_err();
    assert!(matches!(
        err,
        WeftStoreError::Dispatch(DispatchError::MissingRequiredField { field: "story_id" })
    ));
}

// ---- AC-2: written before start; externally checkable; acceptance never rewritten. ----

#[test]
fn dispatch_is_open_before_any_signal_and_acceptance_is_fixed() {
    let (_d, store) = open_temp();
    let id = wv_write_dispatch(&store, "SW-X-5", "task", "", "", "GREEN", None).unwrap();
    // An external reader can verify the finish line without asking the worker.
    let recs = dispatch_state(&store, "SW-X-5").unwrap();
    assert_eq!(recs[0].state, "open");
    assert_eq!(recs[0].acceptance, "GREEN");
    assert_eq!(read_state(&store, id).as_deref(), Some("open"));
}

// ---- AC-3: done-when evaluation, single-writer CAS + typed refusals. ----

#[test]
fn mark_done_advances_open_to_done_and_stamps_session() {
    let (_d, store) = open_temp();
    let id = wv_write_dispatch(&store, "SW-X-6", "task", "", "", "done", None).unwrap();
    let done_id = wv_mark_dispatch_done(&store, "SW-X-6", "sess-9").unwrap();
    assert_eq!(done_id, id);
    let r = &dispatch_state(&store, "SW-X-6").unwrap()[0];
    assert_eq!(r.state, "done");
    assert_eq!(r.session_id.as_deref(), Some("sess-9"));
    assert!(r.done_at.is_some());
}

#[test]
fn mark_done_twice_is_refused_not_a_silent_noop() {
    let (_d, store) = open_temp();
    wv_write_dispatch(&store, "SW-X-7", "task", "", "", "done", None).unwrap();
    wv_mark_dispatch_done(&store, "SW-X-7", "sess-1").unwrap();
    // A second mark finds no open record — a typed refusal, never a silent no-op.
    let err = wv_mark_dispatch_done(&store, "SW-X-7", "sess-1").unwrap_err();
    assert!(matches!(
        err,
        WeftStoreError::Dispatch(DispatchError::NotOpen)
    ));
}

#[test]
fn mark_done_absent_record_is_refused() {
    let (_d, store) = open_temp();
    let err = wv_mark_dispatch_done(&store, "nope", "sess").unwrap_err();
    assert!(matches!(
        err,
        WeftStoreError::Dispatch(DispatchError::NotOpen)
    ));
}

// ---- AC-4: dispatch_done_by_session — the hook's core (resolve roster key). ----

#[test]
fn done_by_session_resolves_roster_key_and_marks_done() {
    let (_d, store) = open_temp();
    seed_dispatched_worker(&store, "SW-X-8", "sess-live");
    wv_write_dispatch(&store, "SW-X-8", "task", "", "", "done", Some("root")).unwrap();

    let advanced = dispatch_done_by_session(&store, "sess-live").unwrap();
    assert!(advanced, "a live dispatched worker's record must advance");
    let r = &dispatch_state(&store, "SW-X-8").unwrap()[0];
    assert_eq!(r.state, "done");
    assert_eq!(r.session_id.as_deref(), Some("sess-live"));
}

#[test]
fn done_by_session_still_marks_done_after_roster_update_hook_advanced_to_signal() {
    // The REAL signal_done hook order is roster-update -> dispatch-done-stamp. The
    // FIRST hook advances the roster row `dispatch -> signal` BEFORE this one runs, so
    // the done-stamp must resolve a `signal`-state roster row, not only `dispatch`.
    // This exercises the integration seam the isolated AC-4 test bypasses.
    let (_d, store) = open_temp();
    seed_dispatched_worker(&store, "SW-X-8b", "sess-seam");
    wv_write_dispatch(&store, "SW-X-8b", "task", "", "", "done", Some("root")).unwrap();

    // Simulate hook #1 (roster-update): dispatch -> signal.
    let advanced_roster = crate::weft::wv_worker_signal_by_session(&store, "sess-seam").unwrap();
    assert!(
        advanced_roster,
        "roster-update hook advances dispatch -> signal"
    );

    // Now hook #2 (dispatch-done-stamp) must STILL mark the dispatch record done.
    let advanced = dispatch_done_by_session(&store, "sess-seam").unwrap();
    assert!(
        advanced,
        "dispatch-done-stamp must mark done even after roster is 'signal'"
    );
    assert_eq!(dispatch_state(&store, "SW-X-8b").unwrap()[0].state, "done");
}

#[test]
fn done_by_session_is_graceful_skip_for_a_non_worker_completion() {
    let (_d, store) = open_temp();
    // No roster row for this session at all — a non-worker signal_done.
    let advanced = dispatch_done_by_session(&store, "unknown-sess").unwrap();
    assert!(!advanced, "a non-worker completion is a graceful skip");
}

#[test]
fn done_by_session_is_graceful_skip_when_worker_has_no_dispatch_record() {
    let (_d, store) = open_temp();
    // A live worker but the controller never wrote a brief for it (edge case).
    seed_dispatched_worker(&store, "SW-X-9", "sess-nobrief");
    let advanced = dispatch_done_by_session(&store, "sess-nobrief").unwrap();
    assert!(
        !advanced,
        "a worker with no open dispatch record is a graceful skip, not a failure"
    );
}

#[test]
fn done_by_session_blank_session_is_skip() {
    let (_d, store) = open_temp();
    assert!(!dispatch_done_by_session(&store, "").unwrap());
    assert!(!dispatch_done_by_session(&store, "   ").unwrap());
}

// ---- AC-6: audit co-commit — one append on write, one mutate on done. ----

#[test]
fn every_mutation_co_commits_its_audit_row() {
    let (_d, store) = open_temp();
    wv_write_dispatch(&store, "SW-X-10", "task", "", "", "done", None).unwrap();
    assert_eq!(
        audit_ops_for(&store),
        vec![("append".to_string(), "wv_dispatch_records".to_string())],
        "write co-commits one append audit row"
    );
    wv_mark_dispatch_done(&store, "SW-X-10", "s").unwrap();
    assert_eq!(
        audit_ops_for(&store),
        vec![
            ("append".to_string(), "wv_dispatch_records".to_string()),
            ("mutate".to_string(), "wv_dispatch_records".to_string()),
        ],
        "mark-done co-commits one mutate audit row"
    );
}

#[test]
fn a_refused_mark_done_writes_no_audit_row() {
    let (_d, store) = open_temp();
    // No record → refused → no ledger churn.
    let _ = wv_mark_dispatch_done(&store, "ghost", "s");
    assert!(
        audit_ops_for(&store).is_empty(),
        "a refused mark-done leaves no audit row"
    );
}

// ---- Forward-migration: a fresh store opens the dispatch table idempotently. ----

#[test]
fn open_is_idempotent_for_the_dispatch_table() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("weft.sqlite3");
    let s1 = WeftStore::open(&path).unwrap();
    wv_write_dispatch(&s1, "SW-X-11", "task", "", "", "done", None).unwrap();
    // Re-open the same file — no data loss, no schema error.
    let s2 = WeftStore::open(&path).unwrap();
    assert_eq!(dispatch_state(&s2, "SW-X-11").unwrap().len(), 1);
}
