// SPDX-License-Identifier: MIT
//! Task 8 stub-grep: no pre-existing wv_close/wv_write_brief/dispatch_handle stubs
//! found on 2026-09-20 (the C1/B2/C3 deps had already landed the canonical seams;
//! C5 extends them, reusing C3's `is_close_predicate_met` / `has_success_signal` /
//! `is_done_verified` verify seam rather than re-implementing the close predicate).
//!
//! SW-WV-C5 acceptance tests — closure + start-and-release.
//!
//! Covers AC-8 (backward-compatible `wv_queue` migration for `dispatched_at` /
//! `dispatch_handle`), AC-2 (`mark_dispatched` `claimed → dispatched` + handle stamp),
//! AC-1 ordering (brief must precede dispatch), AC-4/AC-5 (two-condition close
//! predicate D5 → atomic prune + key-ledger), and the source-grep contract tests
//! (AC-3 no-poll, AC-7 `await_blocking` discipline, AC-9 two-seam gate).
//!
//! RECONCILIATION (as-built vs. the 2026-09-18 story): the deps landed with a more
//! mature shape than the story assumed. `wv_dispatch_records` (C1) is keyed by
//! `story_id` with fields `task`/`acceptance` (D4 names, not `queue_id`-FK /
//! `goal`/`done_when`); the closure predicate already exists as C3's
//! `is_close_predicate_met`, and B2's `wv_resolve_queue` already does the atomic
//! prune + key-ledger and even runs the predicate as a pre-flight. C5 therefore
//! adds ONLY the genuinely net-new surface: the `dispatched_at`/`dispatch_handle`
//! runtime columns, the real `mark_dispatched` release transition (the story's
//! `wv_write_brief` is FOLDED — C1's `wv_write_dispatch` is the sole brief writer,
//! D4), and the ergonomic `close_task` that reuses C3's verify seam.

use std::path::{Path, PathBuf};

use rusqlite::params;

use super::*;
use crate::weft::queue::{self, CloseCondition, CloseOutcome, QueueError};
use crate::weft::store::WeftStore;

fn open_temp() -> (tempfile::TempDir, WeftStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
    (dir, store)
}

/// Enqueue one `pending` row and claim it as `claimed_by` — the state a release
/// starts from. Returns the row id.
fn enqueue_and_claim(store: &WeftStore, claimed_by: &str) -> i64 {
    let id = queue::enqueue(store, "test", None, Some("ref-1"), None, None, None, 100).unwrap();
    let claimed = queue::claim(store, claimed_by, None, 200).unwrap();
    assert_eq!(claimed, Some(id), "claim should return the enqueued row");
    id
}

/// Read a `wv_queue` row's `(state, dispatched_at, dispatch_handle)`.
fn read_dispatch_cols(store: &WeftStore, id: i64) -> (String, Option<i64>, Option<String>) {
    let conn = store.conn().unwrap();
    conn.query_row(
        "SELECT state, dispatched_at, dispatch_handle FROM wv_queue WHERE id=?1",
        params![id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .unwrap()
}

// ─────────────────────────── AC-8: migration ───────────────────────────

/// AC-8 — a pre-existing B2-era `weft.sqlite3` (no `dispatched_at` / `dispatch_handle`
/// on `wv_queue`) forward-migrates on the next `WeftStore::open` without data loss.
/// Simulated by DROPping the two C5 columns from a fresh store, inserting a row, then
/// reopening — both columns must be present again and the row intact.
#[test]
fn ac8_wv_queue_migration_is_backward_compatible() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("weft.sqlite3");
    {
        // Simulate a B2-era DB: hand-build a `wv_queue` that carries the B2 columns
        // but NOT the two C5 runtime columns, and seed a row that predates them. A
        // direct `open_conn` (no `WeftStore::open`, so no C5 migration runs yet).
        let conn = crate::shared::sqlite::open_wal_db(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE wv_queue (\
                 id INTEGER PRIMARY KEY, source TEXT NOT NULL DEFAULT '', \
                 state TEXT NOT NULL DEFAULT 'pending', claimed_by TEXT, scope TEXT, \
                 created INTEGER NOT NULL DEFAULT 0, updated INTEGER NOT NULL DEFAULT 0, \
                 refs TEXT, note TEXT, dedupe_key TEXT);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO wv_queue (id, source, state, created, updated) \
             VALUES (7, 'legacy', 'pending', 1, 1)",
            [],
        )
        .unwrap();
        let cols: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('wv_queue')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            !cols.iter().any(|c| c == "dispatched_at"),
            "precondition: the B2-era table lacks dispatched_at"
        );
    }
    // Reopen: the C5 `ensure_column` migration heals both columns forward.
    let store = WeftStore::open(&path).unwrap();
    let conn = store.conn().unwrap();
    let cols: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_info('wv_queue')")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        cols.iter().any(|c| c == "dispatched_at"),
        "dispatched_at must be forward-migrated on reopen"
    );
    assert!(
        cols.iter().any(|c| c == "dispatch_handle"),
        "dispatch_handle must be forward-migrated on reopen"
    );
    // The legacy row survives, and its new columns read NULL (no backfill needed).
    let (state, at, handle) = read_dispatch_cols(&store, 7);
    assert_eq!(state, "pending");
    assert_eq!(at, None);
    assert_eq!(handle, None);
}

// ─────────────────── AC-2 / AC-1 ordering: mark_dispatched ───────────────────

/// AC-2 happy path — enqueue → claim → write brief (C1) → mark-dispatched: the row
/// moves to `dispatched` with `dispatched_at` + `dispatch_handle` stamped.
#[test]
fn ac2_mark_dispatched_transitions_and_stamps_handle() {
    let (_dir, store) = open_temp();
    let id = enqueue_and_claim(&store, "ctl-A");
    // The brief must exist before release (AC-1 ordering) — write it via C1.
    crate::weft::wv_write_dispatch(
        &store,
        "wv-queue-1",
        "do the thing",
        "",
        "",
        "PR opened",
        None,
    )
    .unwrap();

    let out = queue::mark_dispatched(&store, id, "ctl-A", "wv-queue-1", "handle-xyz", 300).unwrap();
    assert_eq!(out, Ok(()), "mark_dispatched should succeed");

    let (state, at, handle) = read_dispatch_cols(&store, id);
    assert_eq!(state, "dispatched");
    assert_eq!(at, Some(300));
    assert_eq!(handle.as_deref(), Some("handle-xyz"));
}

/// AC-2 guard — `mark_dispatched` from a `pending` state (never claimed) is refused
/// `StateConflict`, and no runtime columns are stamped.
#[test]
fn ac2_mark_dispatched_from_pending_is_state_conflict() {
    let (_dir, store) = open_temp();
    let id = queue::enqueue(&store, "test", None, None, None, None, None, 100).unwrap();
    // A brief exists, so the refusal is purely the state guard (not BriefRequired).
    crate::weft::wv_write_dispatch(&store, "wv-queue-1", "task", "", "", "done", None).unwrap();

    let out = queue::mark_dispatched(&store, id, "ctl-A", "wv-queue-1", "h", 300).unwrap();
    assert_eq!(out, Err(QueueError::StateConflict));

    let (state, at, handle) = read_dispatch_cols(&store, id);
    assert_eq!(state, "pending", "the row must be untouched");
    assert_eq!(at, None);
    assert_eq!(handle, None);
}

/// AC-1 ordering — `mark_dispatched` before any `wv_dispatch_records` brief exists is
/// refused `BriefRequired`; the row stays `claimed` and unstamped (the finish line is
/// frozen before release, never retro-fitted).
#[test]
fn ac1_mark_dispatched_without_brief_is_refused() {
    let (_dir, store) = open_temp();
    let id = enqueue_and_claim(&store, "ctl-A");
    // No wv_write_dispatch call — the brief is absent.
    let out = queue::mark_dispatched(&store, id, "ctl-A", "wv-queue-1", "h", 300).unwrap();
    assert_eq!(out, Err(QueueError::BriefRequired));

    let (state, at, handle) = read_dispatch_cols(&store, id);
    assert_eq!(state, "claimed", "the row must stay claimed until briefed");
    assert_eq!(at, None);
    assert_eq!(handle, None);
}

/// AC-1 ordering (retro-fit guard) — a STALE brief already marked `done` (from a
/// prior cycle under the same reusable `story_id`) must NOT satisfy the ordering
/// guard: releasing requires a FRESH `open` brief. Proves the guard checks brief
/// STATE, not mere existence — the retro-fit hole the story forbids.
#[test]
fn ac1_mark_dispatched_with_only_a_stale_done_brief_is_refused() {
    let (_dir, store) = open_temp();
    let id = enqueue_and_claim(&store, "ctl-A");
    // A brief was written and completed in a prior cycle → now `done`, not `open`.
    crate::weft::wv_write_dispatch(&store, "wv-queue-1", "old task", "", "", "old done", None)
        .unwrap();
    crate::weft::wv_mark_dispatch_done(&store, "wv-queue-1", "prior-worker").unwrap();

    // No FRESH brief for the current dispatch → the guard must refuse.
    let out = queue::mark_dispatched(&store, id, "ctl-A", "wv-queue-1", "h", 300).unwrap();
    assert_eq!(out, Err(QueueError::BriefRequired));
    let (state, at, handle) = read_dispatch_cols(&store, id);
    assert_eq!(
        state, "claimed",
        "a stale done brief must not release the row"
    );
    assert_eq!(at, None);
    assert_eq!(handle, None);
}

/// AC-2 ownership — `mark_dispatched` by a session that does not own the claimed row
/// is refused `StateConflict` (the CAS `claimed_by=?` guard matches nothing).
#[test]
fn ac2_mark_dispatched_by_non_owner_is_refused() {
    let (_dir, store) = open_temp();
    let id = enqueue_and_claim(&store, "ctl-A");
    crate::weft::wv_write_dispatch(&store, "wv-queue-1", "task", "", "", "done", None).unwrap();

    let out = queue::mark_dispatched(&store, id, "ctl-B", "wv-queue-1", "h", 300).unwrap();
    assert_eq!(out, Err(QueueError::StateConflict));
    let (state, _, _) = read_dispatch_cols(&store, id);
    assert_eq!(state, "claimed");
}

// ─────────────── AC-4 / AC-5: two-condition closure (D5) ───────────────

/// Build a done record for a dispatch and stamp it `controller_verified=1` through the
/// REAL C3 verify seam (`verify_done`), plus insert the worker's success signal — the
/// state in which BOTH close conditions hold. Returns the `dispatch_id`.
fn make_closeable(store: &WeftStore, run_store_path: &Path, queue_id: i64) -> String {
    let dispatch_id = "disp-close-1".to_string();
    let worker_session = "worker-sess-1";
    let story_id = "SW-WV-C5";
    let run_id = "run-1";

    // C1 dispatch record in `done` state with a non-blank acceptance — the Done-When
    // predicate `verify_done` reads.
    crate::weft::wv_write_dispatch(store, story_id, "task", "", "", "PR opened", None).unwrap();
    crate::weft::wv_mark_dispatch_done(store, story_id, worker_session).unwrap();

    // C3 done record (3 legs the verify seam re-checks) keyed by the worker session.
    let record = crate::weft::DoneRecord {
        dispatch_id: dispatch_id.clone(),
        queue_id,
        session_id: worker_session.to_string(),
        run_id: run_id.to_string(),
        story_id: story_id.to_string(),
        subclass: "review".to_string(), // content-addressed → Leg-3 idempotent, no run-store read
        leg1_source: "base@abc".to_string(),
        leg2_footprint: vec![serde_json::json!({"kind": "file", "reference": "a.rs"})],
        leg3_packet: serde_json::json!({
            "signalStatus": "success",
            "canonicalRefs": ["https://example.test/pr/1"],
        }),
        created: 5_000,
    };
    assert!(store.insert_done_record(&record).unwrap());

    // Controller verification (leg (b)) via the real seam.
    let verdict = store
        .verify_done(&dispatch_id, 900, run_store_path)
        .unwrap();
    assert!(verdict.verified, "fixture must verify: {verdict:?}");
    assert!(store.is_done_verified(&dispatch_id).unwrap());

    // Success signal (leg (a)) for the worker session.
    crate::weft::wv_signal_insert(
        store,
        "worker",
        story_id,
        worker_session,
        "success",
        None,
        None,
    )
    .unwrap();
    assert!(store.has_success_signal(worker_session).unwrap());

    dispatch_id
}

/// AC-4 + AC-5 happy path — BOTH conditions hold → `close_task` prunes the queue row
/// AND writes exactly one `wv_key_ledger` row for it, atomically. The two assertions
/// are made in a single post-call read (proving no window where one table changed
/// without the other).
#[test]
fn ac4_ac5_close_task_resolves_atomically_when_both_conditions_hold() {
    let (_dir, store) = open_temp();
    let run_dir = tempfile::tempdir().unwrap();
    let run_store_path = run_dir.path().join("run-store.jsonl");

    let id = enqueue_and_claim(&store, "ctl-A");
    let dispatch_id = make_closeable(&store, &run_store_path, id);

    let out = queue::close_task(&store, id, "ctl-A", &dispatch_id, 400).unwrap();
    assert_eq!(out, CloseOutcome::Resolved);

    let conn = store.conn().unwrap();
    let queue_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wv_queue WHERE id=?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    let (ledger_rows, ledger_ref): (i64, Option<String>) = conn
        .query_row(
            "SELECT COUNT(*), MAX(ref) FROM wv_key_ledger WHERE queue_id=?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(queue_rows, 0, "the queue row must be pruned");
    assert_eq!(ledger_rows, 1, "exactly one key-ledger row must be written");
    assert_eq!(
        ledger_ref.as_deref(),
        Some("ref-1"),
        "ledger carries the row's ref"
    );
}

/// AC-4 — success signal present but NOT controller-verified → `closureBlocked` with
/// `missing == [controller_verified]`; the queue row is untouched, no ledger row.
#[test]
fn ac4_close_task_blocked_when_not_controller_verified() {
    let (_dir, store) = open_temp();
    let id = enqueue_and_claim(&store, "ctl-A");

    // A done record + success signal, but NO verify_done → controller_verified stays 0.
    let dispatch_id = "disp-2".to_string();
    let worker_session = "w-2";
    let record = crate::weft::DoneRecord {
        dispatch_id: dispatch_id.clone(),
        queue_id: id,
        session_id: worker_session.to_string(),
        run_id: "run-2".to_string(),
        story_id: "SW-WV-C5".to_string(),
        subclass: "review".to_string(),
        leg1_source: "base".to_string(),
        leg2_footprint: vec![serde_json::json!({"kind": "file", "reference": "a.rs"})],
        leg3_packet: serde_json::json!({"signalStatus": "success", "canonicalRefs": ["r"]}),
        created: 5_000,
    };
    store.insert_done_record(&record).unwrap();
    crate::weft::wv_signal_insert(&store, "w", "t", worker_session, "success", None, None).unwrap();

    let out = queue::close_task(&store, id, "ctl-A", &dispatch_id, 400).unwrap();
    assert_eq!(
        out,
        CloseOutcome::Blocked {
            missing: vec![CloseCondition::NotControllerVerified]
        }
    );
    assert_queue_present_no_ledger(&store, id);
}

/// AC-4 — controller-verified but NO success signal → `closureBlocked` with
/// `missing == [accepted]`; the queue row is untouched, no ledger row.
#[test]
fn ac4_close_task_blocked_when_not_accepted() {
    let (_dir, store) = open_temp();
    let run_dir = tempfile::tempdir().unwrap();
    let run_store_path = run_dir.path().join("run-store.jsonl");
    let id = enqueue_and_claim(&store, "ctl-A");

    // Full verified done record but DELETE the success signal path: build the closeable
    // fixture, then prove the `accepted` leg is the sole miss by NOT inserting a signal.
    let dispatch_id = "disp-3".to_string();
    let worker_session = "w-3";
    let story_id = "SW-WV-C5";
    crate::weft::wv_write_dispatch(&store, story_id, "task", "", "", "PR opened", None).unwrap();
    crate::weft::wv_mark_dispatch_done(&store, story_id, worker_session).unwrap();
    let record = crate::weft::DoneRecord {
        dispatch_id: dispatch_id.clone(),
        queue_id: id,
        session_id: worker_session.to_string(),
        run_id: "run-3".to_string(),
        story_id: story_id.to_string(),
        subclass: "review".to_string(),
        leg1_source: "base".to_string(),
        leg2_footprint: vec![serde_json::json!({"kind": "file", "reference": "a.rs"})],
        leg3_packet: serde_json::json!({"signalStatus": "success", "canonicalRefs": ["r"]}),
        created: 5_000,
    };
    store.insert_done_record(&record).unwrap();
    assert!(
        store
            .verify_done(&dispatch_id, 900, &run_store_path)
            .unwrap()
            .verified
    );
    // No wv_signal_insert → the `accepted` leg is unmet.

    let out = queue::close_task(&store, id, "ctl-A", &dispatch_id, 400).unwrap();
    assert_eq!(
        out,
        CloseOutcome::Blocked {
            missing: vec![CloseCondition::NotAccepted]
        }
    );
    assert_queue_present_no_ledger(&store, id);
}

/// AC-4 — neither condition met (no done record at all) → both legs reported missing;
/// row untouched.
#[test]
fn ac4_close_task_blocked_reports_both_when_nothing_done() {
    let (_dir, store) = open_temp();
    let id = enqueue_and_claim(&store, "ctl-A");
    let out = queue::close_task(&store, id, "ctl-A", "no-such-dispatch", 400).unwrap();
    assert_eq!(
        out,
        CloseOutcome::Blocked {
            missing: vec![
                CloseCondition::NotAccepted,
                CloseCondition::NotControllerVerified
            ]
        }
    );
    assert_queue_present_no_ledger(&store, id);
}

fn assert_queue_present_no_ledger(store: &WeftStore, id: i64) {
    let conn = store.conn().unwrap();
    let queue_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wv_queue WHERE id=?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    let ledger_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wv_key_ledger WHERE queue_id=?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        queue_rows, 1,
        "a blocked close must leave the queue row in place"
    );
    assert_eq!(
        ledger_rows, 0,
        "a blocked close must write no key-ledger row"
    );
}

// ─────────────── Source-grep contract tests (AC-3, AC-7, AC-9) ───────────────

/// Read a source file relative to this crate's manifest dir (`CARGO_MANIFEST_DIR`
/// == the `src-tauri` crate root, so a source path is `src/...`), panicking with the
/// attempted path if it cannot be read.
fn read_src(rel: &str) -> String {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read {} for the C5 contract test: {e}",
            path.display()
        )
    })
}

/// Slice the source of one async handler arm: from `async fn <name>(` to the matching
/// closing brace (brace-balanced from the `{` that opens the body). Panics if the
/// handler is not found — a rename must update this test, not silently pass.
fn handler_body<'a>(source: &'a str, name: &str) -> &'a str {
    let needle = format!("async fn {name}(");
    let start = source
        .find(&needle)
        .unwrap_or_else(|| panic!("handler `{name}` not found in handler.rs"));
    let rest = &source[start..];
    let open = rest
        .find('{')
        .unwrap_or_else(|| panic!("no opening brace for handler `{name}`"));
    let mut depth = 0i32;
    for (i, ch) in rest[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &rest[open..open + i + 1];
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced braces scanning handler `{name}`");
}

const C5_HANDLERS: &[&str] = &["wv_mark_dispatched", "wv_close_task"];

/// AC-3 — no-poll: none of the C5 handler bodies contains a polling / waiting
/// construct. Start-and-release means the controller returns immediately after the
/// transition — no `wv_wait`, no sleep, no channel recv, no busy loop.
#[test]
fn ac3_c5_handlers_have_no_poll_or_wait() {
    let source = read_src("src/mcp/handler.rs");
    const FORBIDDEN: &[&str] = &[
        "wv_wait",
        "tokio::sleep",
        ".recv()",
        "loop {",
        "while !done",
    ];
    for handler in C5_HANDLERS {
        let body = handler_body(&source, handler);
        for pat in FORBIDDEN {
            assert!(
                !body.contains(pat),
                "AC-3 violated: handler `{handler}` contains a poll/wait construct `{pat}` — \
                 start-and-release must not block past the transition"
            );
        }
    }
}

/// AC-7 — `await_blocking` discipline: each C5 handler dispatches its SQLite work via
/// the safe `await_blocking(` wrapper and never calls bare `spawn_blocking(` (which
/// would bypass the wrapper).
#[test]
fn ac7_c5_handlers_use_await_blocking_not_bare_spawn_blocking() {
    let source = read_src("src/mcp/handler.rs");
    for handler in C5_HANDLERS {
        let body = handler_body(&source, handler);
        assert!(
            body.contains("await_blocking("),
            "AC-7 violated: handler `{handler}` must dispatch DB work via await_blocking("
        );
        assert!(
            !body.contains("spawn_blocking("),
            "AC-7 violated: handler `{handler}` must not call bare spawn_blocking( \
             (use the await_blocking wrapper)"
        );
    }
}

/// AC-9 — two-seam gate: each C5 verb appears in the `INTERACTIVE_VERBS` array
/// (`commands/interactive_gate.rs`) AND is admitted via `InteractiveGate::admit("<verb>")`
/// in `handler.rs`. A lease check is a belt, not a substitute for either seam.
#[test]
fn ac9_c5_verbs_have_both_gate_seams() {
    let gate_src = read_src("src/commands/interactive_gate.rs");
    let handler_src = read_src("src/mcp/handler.rs");
    for verb in C5_HANDLERS {
        assert!(
            gate_src.contains(&format!("\"{verb}\"")),
            "AC-9 violated: `{verb}` is missing from INTERACTIVE_VERBS (interactive_gate.rs)"
        );
        assert!(
            handler_src.contains(&format!("InteractiveGate::admit(\"{verb}\")")),
            "AC-9 violated: handler.rs is missing InteractiveGate::admit(\"{verb}\")"
        );
    }
}
