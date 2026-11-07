// SPDX-License-Identifier: MIT
//! SW-WV-C3 acceptance tests for the 3-leg done-state + verify-don't-fold.
//!
//! Covers AC-1 (schema idempotency), AC-2 (atomic insert + audit + idempotent
//! no-op), AC-3 (verify stamps all legs / a failed leg is reported without
//! side-effecting the queue), AC-5 (idle ≠ done), and AC-6 (impl idempotency
//! guard + non-impl unconditional pass with no run-store read). AC-4
//! (`wv_resolve_queue` close-predicate helpers `is_done_verified` /
//! `has_success_signal`) is covered here at the store layer; the handler pre-flight
//! is exercised by the MCP handler tests.

use std::path::{Path, PathBuf};

use rusqlite::params;

use super::*;
use crate::commands::sprint_run::run_store::{
    ArtifactKind, FootprintChange, FootprintEntry, RunEvent, StepArtifact, StepResultEvent,
    StoreRecord,
};
use crate::weft::store::WeftStore;

fn open_temp() -> (tempfile::TempDir, WeftStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
    (dir, store)
}

/// Seed a live `wv_queue` row so the `wv_done_record.queue_id` FK is satisfiable.
fn seed_queue_row(store: &WeftStore) -> i64 {
    let mut conn = store.conn().unwrap();
    crate::weft::wv_append(
        &mut conn,
        "wv_queue",
        "INSERT INTO wv_queue (id, state) VALUES (?1, 'claimed')",
        &[],
    )
    .unwrap()
}

/// A minimal valid `DoneRecord` for a queue row, all three legs populated.
fn sample_record(queue_id: i64, subclass: &str) -> DoneRecord {
    DoneRecord {
        dispatch_id: "disp-1".to_string(),
        queue_id,
        session_id: "sess-1".to_string(),
        run_id: "run-1".to_string(),
        story_id: "SW-WV-C3".to_string(),
        subclass: subclass.to_string(),
        leg1_source: "abc123".to_string(),
        leg2_footprint: vec![serde_json::json!({"kind": "file", "reference": "a.rs"})],
        leg3_packet: serde_json::json!({
            "signalStatus": "success",
            "summary": "done",
            "canonicalRefs": ["https://example.test/pr/1"],
        }),
        created: 5_000,
    }
}

/// Write a run-store JSONL with the given records serialized (`StoreRecord` is
/// `Serialize`; `RunEvent` is `#[serde(tag="kind")]`).
fn write_run_store(records: &[StoreRecord]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run-store.jsonl");
    let mut body = String::new();
    for rec in records {
        body.push_str(&serde_json::to_string(rec).unwrap());
        body.push('\n');
    }
    std::fs::write(&path, body).unwrap();
    (dir, path)
}

fn step_result_record(ts: u128, story_id: &str, run_id: &str, pr: bool) -> StoreRecord {
    let mut artifacts = Vec::new();
    let mut footprint = Vec::new();
    if pr {
        artifacts.push(StepArtifact {
            kind: ArtifactKind::Pr,
            reference: "https://example.test/pr/1".into(),
            label: None,
        });
    }
    footprint.push(FootprintEntry {
        kind: ArtifactKind::File,
        reference: "a.rs".into(),
        change: FootprintChange::Modified,
    });
    StoreRecord {
        schema_version: 1,
        seq: 1,
        ts,
        event: RunEvent::StepResult(StepResultEvent {
            story_id: story_id.into(),
            run_id: run_id.into(),
            node_id: "dev".into(),
            summary: None,
            outputs: Vec::new(),
            artifacts,
            footprint,
        }),
    }
}

fn read_verified(store: &WeftStore, dispatch_id: &str) -> (i64, i64, i64, i64, Option<i64>) {
    let conn = store.conn().unwrap();
    conn.query_row(
        "SELECT leg1_verified, leg2_verified, leg3_verified, controller_verified, verified_at \
         FROM wv_done_record WHERE dispatch_id=?1",
        params![dispatch_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )
    .unwrap()
}

fn done_record_count(store: &WeftStore, dispatch_id: &str) -> i64 {
    let conn = store.conn().unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM wv_done_record WHERE dispatch_id=?1",
        params![dispatch_id],
        |r| r.get(0),
    )
    .unwrap()
}

// ---------- AC-1: schema exists, idempotently created ----------

#[test]
fn done_record_table_has_all_columns() {
    let (_dir, store) = open_temp();
    let conn = store.conn().unwrap();
    let mut stmt = conn.prepare("PRAGMA table_info(wv_done_record)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    for expected in [
        "dispatch_id",
        "queue_id",
        "session_id",
        "run_id",
        "story_id",
        "subclass",
        "leg1_source",
        "leg1_verified",
        "leg2_footprint",
        "leg2_verified",
        "leg3_packet",
        "leg3_idempotent",
        "leg3_verified",
        "controller_verified",
        "done_when_met",
        "created",
        "verified_at",
    ] {
        assert!(
            cols.iter().any(|c| c == expected),
            "wv_done_record must have column {expected:?}; got {cols:?}"
        );
    }
}

#[test]
fn open_on_preexisting_store_without_table_creates_it_without_data_loss() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("weft.sqlite3");
    // First open builds the whole schema; seed a wv_queue row.
    let store = WeftStore::open(&path).unwrap();
    let qid = seed_queue_row(&store);
    // Simulate a pre-C3 store: drop the done-record table entirely.
    {
        let conn = store.conn().unwrap();
        conn.execute_batch("DROP TABLE wv_done_record;").unwrap();
    }
    drop(store);
    crate::weft::sqlite::reset_cached_store_for_test(&path);
    // Re-open: the table is recreated (SCHEMA `CREATE IF NOT EXISTS`) and the
    // pre-existing queue row survives.
    let store = WeftStore::open(&path).unwrap();
    assert_eq!(done_record_count(&store, "disp-1"), 0, "table is queryable");
    let conn = store.conn().unwrap();
    let survived: i64 = conn
        .query_row("SELECT id FROM wv_queue WHERE id=?1", params![qid], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        survived, qid,
        "pre-existing queue row survives the migration"
    );
}

// ---------- AC-2: atomic insert + audit + idempotent no-op ----------

#[test]
fn insert_done_record_writes_three_legs_and_audit() {
    let (_dir, store) = open_temp();
    let qid = seed_queue_row(&store);
    let record = sample_record(qid, "impl");
    assert!(store.insert_done_record(&record).unwrap(), "first insert");

    let conn = store.conn().unwrap();
    let (source, footprint, packet, run_id, story_id, cv): (
        String,
        String,
        String,
        String,
        String,
        i64,
    ) = conn
        .query_row(
            "SELECT leg1_source, leg2_footprint, leg3_packet, run_id, story_id, controller_verified \
             FROM wv_done_record WHERE dispatch_id='disp-1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .unwrap();
    assert_eq!(source, "abc123");
    assert!(footprint.contains("a.rs"), "leg2 footprint stored as JSON");
    assert!(packet.contains("success"), "leg3 packet stored as JSON");
    assert_eq!(run_id, "run-1");
    assert_eq!(story_id, "SW-WV-C3");
    assert_eq!(cv, 0, "controller_verified defaults to 0");

    // AUDIT-ATOMIC: exactly one wv_write_log row for this record's queue key.
    let audit: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wv_write_log WHERE tbl='wv_done_record' AND row_id=?1",
            params![qid],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        audit, 1,
        "exactly one audit row co-committed with the insert"
    );
}

#[test]
fn insert_done_record_is_idempotent_on_duplicate_dispatch_id() {
    let (_dir, store) = open_temp();
    let qid = seed_queue_row(&store);
    let record = sample_record(qid, "impl");
    assert!(store.insert_done_record(&record).unwrap(), "first insert");
    assert!(
        !store.insert_done_record(&record).unwrap(),
        "a duplicate dispatch_id is an Ok(false) no-op"
    );
    assert_eq!(done_record_count(&store, "disp-1"), 1, "exactly one row");
}

// ---------- AC-3: verify stamps all legs; a failed leg is reported ----------

/// Mark the story's dispatch record done so the done-when predicate holds. The
/// dispatch's `created_at` (job START) is forced to an RFC-3339 stamp derived from
/// `start_ms` so the impl Leg-3 guard's before/after-start comparison is
/// deterministic (the run-store PR footprint `ts` values in the impl tests are
/// small integers).
fn seed_done_dispatch_at(store: &WeftStore, story_id: &str, start_ms: i64) {
    let id = crate::weft::wv_write_dispatch(
        store,
        story_id,
        "do the thing",
        "",
        "",
        "the finish line",
        Some("root"),
    )
    .unwrap();
    let started = chrono::DateTime::from_timestamp_millis(start_ms)
        .unwrap()
        .to_rfc3339();
    let conn = store.conn().unwrap();
    conn.execute(
        "UPDATE wv_dispatch_records SET state='done', created_at=?2 WHERE id=?1",
        params![id, started],
    )
    .unwrap();
}

/// Convenience: mark done with a job-start at epoch millis 3_000 (before the impl
/// happy-path PR footprint at ts=9_000, after the duplicate PR footprint at ts=1_000).
fn seed_done_dispatch(store: &WeftStore, story_id: &str) {
    seed_done_dispatch_at(store, story_id, 3_000);
}

#[test]
fn verify_done_happy_path_stamps_controller_verified() {
    let (_dir, store) = open_temp();
    let qid = seed_queue_row(&store);
    seed_done_dispatch(&store, "SW-WV-C3");
    let record = sample_record(qid, "impl");
    store.insert_done_record(&record).unwrap();
    // A run store whose PR footprint was recorded AFTER the dispatch's `created`.
    let (_rs_dir, rs_path) =
        write_run_store(&[step_result_record(9_000, "SW-WV-C3", "run-1", true)]);

    let result = store.verify_done("disp-1", 12_345, &rs_path).unwrap();
    assert!(result.verified, "all legs pass: {result:?}");
    let (l1, l2, l3, cv, at) = read_verified(&store, "disp-1");
    assert_eq!((l1, l2, l3, cv), (1, 1, 1, 1));
    assert_eq!(at, Some(12_345), "verified_at stamped");

    // AUDIT-ATOMIC: the mutate audit row co-committed with the stamp.
    let conn = store.conn().unwrap();
    let mutate: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wv_write_log WHERE tbl='wv_done_record' AND operation='mutate'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        mutate, 1,
        "verify stamp writes exactly one mutate audit row"
    );
}

#[test]
fn verify_done_empty_footprint_fails_leg2_without_touching_queue() {
    let (_dir, store) = open_temp();
    let qid = seed_queue_row(&store);
    seed_done_dispatch(&store, "SW-WV-C3");
    let mut record = sample_record(qid, "impl");
    record.leg2_footprint = Vec::new(); // empty → Leg 2 fails
    store.insert_done_record(&record).unwrap();
    let (_rs_dir, rs_path) = write_run_store(&[]);

    let result = store.verify_done("disp-1", 1, &rs_path).unwrap();
    assert!(!result.verified);
    assert_eq!(result.failed_legs, vec![2]);
    let (_, _, _, cv, at) = read_verified(&store, "disp-1");
    assert_eq!(cv, 0, "controller_verified stays 0 on a failed leg");
    assert_eq!(at, None, "verified_at not set on failure");
    // The queue row is untouched (still claimed).
    let conn = store.conn().unwrap();
    let state: String = conn
        .query_row(
            "SELECT state FROM wv_queue WHERE id=?1",
            params![qid],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "claimed", "the queue row is not resolved");
}

// ---------- AC-5: idle ≠ done ----------

#[test]
fn verify_done_no_record_reports_all_legs_failed() {
    let (_dir, store) = open_temp();
    let (_rs_dir, rs_path) = write_run_store(&[]);
    let result = store.verify_done("no-such-dispatch", 1, &rs_path).unwrap();
    assert!(!result.verified);
    assert_eq!(result.failed_legs, vec![1, 2, 3]);
    assert!(
        result.reason.contains("idle"),
        "reason names the idle case: {}",
        result.reason
    );
}

// ---------- AC-6: impl idempotency guard; non-impl unconditional pass ----------

#[test]
fn verify_done_impl_duplicate_pr_fails_leg3() {
    let (_dir, store) = open_temp();
    let qid = seed_queue_row(&store);
    seed_done_dispatch(&store, "SW-WV-C3");
    let record = sample_record(qid, "impl"); // created=5_000
    store.insert_done_record(&record).unwrap();
    // A PR footprint recorded BEFORE the dispatch's `created` (ts 1_000 < 5_000) →
    // the packet reopened an already-open PR: not idempotent.
    let (_rs_dir, rs_path) =
        write_run_store(&[step_result_record(1_000, "SW-WV-C3", "run-1", true)]);

    let result = store.verify_done("disp-1", 1, &rs_path).unwrap();
    assert!(!result.verified);
    assert_eq!(result.failed_legs, vec![3]);
    assert!(
        result.reason.contains("duplicate PR"),
        "reason names the duplicate PR: {}",
        result.reason
    );
    let conn = store.conn().unwrap();
    let idem: i64 = conn
        .query_row(
            "SELECT leg3_idempotent FROM wv_done_record WHERE dispatch_id='disp-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(idem, 0, "leg3_idempotent stays 0 on a duplicate PR");
}

#[test]
fn verify_done_non_impl_passes_without_run_store_read() {
    let (_dir, store) = open_temp();
    let qid = seed_queue_row(&store);
    seed_done_dispatch(&store, "SW-WV-C3");
    let record = sample_record(qid, "review");
    store.insert_done_record(&record).unwrap();
    // A run_store_path pointing at a NON-EXISTENT file — the non-impl Leg-3 path must
    // not read it and must still succeed.
    let missing = Path::new("/nonexistent/definitely-not-here/run-store.jsonl");

    let result = store.verify_done("disp-1", 7, missing).unwrap();
    assert!(
        result.verified,
        "non-impl verifies without a run-store read: {result:?}"
    );
    let conn = store.conn().unwrap();
    let idem: i64 = conn
        .query_row(
            "SELECT leg3_idempotent FROM wv_done_record WHERE dispatch_id='disp-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(idem, 1, "non-impl leg3_idempotent set unconditionally");
}

// ---------- AC-4 helpers: close-predicate reads ----------

#[test]
fn is_done_verified_reflects_controller_verified() {
    let (_dir, store) = open_temp();
    let qid = seed_queue_row(&store);
    store
        .insert_done_record(&sample_record(qid, "review"))
        .unwrap();
    assert!(
        !store.is_done_verified("disp-1").unwrap(),
        "unverified record is not done-verified"
    );
    assert!(
        !store.is_done_verified("absent").unwrap(),
        "a missing record is not done-verified"
    );
    // Stamp it verified.
    seed_done_dispatch(&store, "SW-WV-C3");
    let missing = Path::new("/nonexistent/run-store.jsonl");
    store.verify_done("disp-1", 1, missing).unwrap();
    assert!(
        store.is_done_verified("disp-1").unwrap(),
        "a controller-verified record reads true"
    );
}

#[test]
fn close_predicate_needs_both_worker_success_signal_and_controller_verified() {
    let (_dir, store) = open_temp();
    let qid = seed_queue_row(&store);
    seed_done_dispatch(&store, "SW-WV-C3");
    // The done record is keyed to the WORKER session "sess-1".
    store
        .insert_done_record(&sample_record(qid, "review"))
        .unwrap();

    // Neither leg yet.
    assert!(!store.is_close_predicate_met("disp-1").unwrap());

    // (b) controller_verified but (a) no success signal → still not met.
    store
        .verify_done("disp-1", 1, Path::new("/nonexistent/run-store.jsonl"))
        .unwrap();
    assert!(
        !store.is_close_predicate_met("disp-1").unwrap(),
        "controller_verified alone is insufficient — needs the worker success signal"
    );

    // A success signal for a DIFFERENT session must NOT satisfy the predicate (the
    // controller's session, say) — the check is keyed to the record's WORKER session.
    crate::weft::wv_signal_insert(&store, "w", "t", "controller-sess", "success", None, None)
        .unwrap();
    assert!(
        !store.is_close_predicate_met("disp-1").unwrap(),
        "a success signal for a non-worker session does not satisfy the predicate"
    );

    // The worker's own success signal ("sess-1") → BOTH legs hold → met.
    crate::weft::wv_signal_insert(&store, "w", "t", "sess-1", "success", None, None).unwrap();
    assert!(
        store.is_close_predicate_met("disp-1").unwrap(),
        "worker success signal + controller_verified → close predicate met"
    );
}

#[test]
fn has_success_signal_matches_only_success_rows_for_the_session() {
    let (_dir, store) = open_temp();
    assert!(
        !store.has_success_signal("sess-1").unwrap(),
        "no signal → false"
    );
    // Insert a success signal for the session via B4's insert handler.
    crate::weft::wv_signal_insert(
        &store, "worker-1", "task-1", "sess-1", "success", None, None,
    )
    .unwrap();
    assert!(
        store.has_success_signal("sess-1").unwrap(),
        "a success signal for the session reads true"
    );
    assert!(
        !store.has_success_signal("other").unwrap(),
        "a different session has no success signal"
    );
}

// ---------- SW-WA-D3 (AC-1, AC-2): G3 verify-don't-fold is the fold gate ----------

/// AC-2 invariant: the `controller_verified` stamp is written iff `may_fold` admits
/// the verdict. For every leg-combination fixture, `is_done_verified(dispatch) == true`
/// implies `may_fold(&verify_done(dispatch)) == true` — the classifier is the gate, not
/// a parallel bool. All-pass ⇒ both true; any-fail ⇒ both false.
#[test]
fn stamp_is_written_iff_may_fold_admits() {
    // ---- all-pass fixture ⇒ verified verdict ⇒ may_fold true ⇒ stamped ----
    {
        let (_dir, store) = open_temp();
        let qid = seed_queue_row(&store);
        seed_done_dispatch(&store, "SW-WV-C3");
        store
            .insert_done_record(&sample_record(qid, "review"))
            .unwrap();
        let missing = Path::new("/nonexistent/run-store.jsonl");
        let verdict = store.verify_done("disp-1", 1, missing).unwrap();
        assert!(verdict.verified, "all-pass verdict is verified");
        assert!(
            crate::weft::may_fold(&verdict),
            "may_fold admits a verified verdict"
        );
        assert!(
            store.is_done_verified("disp-1").unwrap(),
            "the stamp is written when may_fold admits"
        );
    }

    // ---- a failing leg ⇒ unverified verdict ⇒ may_fold false ⇒ NOT stamped ----
    {
        let (_dir, store) = open_temp();
        let qid = seed_queue_row(&store);
        seed_done_dispatch(&store, "SW-WV-C3");
        let mut record = sample_record(qid, "review");
        record.leg2_footprint = Vec::new(); // Leg 2 fails
        store.insert_done_record(&record).unwrap();
        let (_rs_dir, rs_path) = write_run_store(&[]);
        let verdict = store.verify_done("disp-1", 1, &rs_path).unwrap();
        assert!(!verdict.verified, "a failing leg is unverified");
        assert!(
            !crate::weft::may_fold(&verdict),
            "may_fold refuses an unverified verdict"
        );
        assert!(
            !store.is_done_verified("disp-1").unwrap(),
            "the stamp is NOT written when may_fold refuses"
        );
    }

    // ---- no record (idle ≠ done) ⇒ unverified ⇒ may_fold false ⇒ nothing stamped ----
    {
        let (_dir, store) = open_temp();
        let (_rs_dir, rs_path) = write_run_store(&[]);
        let verdict = store.verify_done("absent", 1, &rs_path).unwrap();
        assert!(!verdict.verified);
        assert!(!crate::weft::may_fold(&verdict), "idle verdict never folds");
        assert!(!store.is_done_verified("absent").unwrap());
    }
}

/// AC-2 conformance: the `verify_done` stamp site references `may_fold` BY NAME (the
/// `crate::weft::may_fold` call wired directly in front of the `controller_verified=1`
/// tx — not merely the `VerifyDoneResult` type, which `verify_done` references
/// regardless). Proves the classifier is the literal gate the stamp consults.
#[test]
fn verify_done_stamp_site_references_may_fold_by_name() {
    let src = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/weft/done_record.rs"),
    )
    .expect("read done_record.rs");
    // The stamp tx is `controller_verified=1` inside the UPDATE; the gate call is
    // `crate::weft::may_fold(` and must precede it in the source.
    let gate_idx = src
        .find("crate::weft::may_fold(")
        .expect("verify_done gates the stamp on crate::weft::may_fold by name");
    // Match the SQL stamp fragment (unique to the UPDATE), not the doc-comment mentions.
    let stamp_idx = src
        .find("done_when_met=1, controller_verified=1")
        .expect("the controller_verified=1 stamp exists");
    assert!(
        gate_idx < stamp_idx,
        "the may_fold gate must be wired IN FRONT OF the controller_verified stamp"
    );
}

// ---------- AC-7 / Task 6: every done-record call site carries an RI-2-ALLOW fence ----------
//
// Enforces the threading invariant for ALL future additions to the done-record call
// graph — not just the two sites this story introduced: any caller of
// `insert_done_record` / `verify_done` outside `src/weft/` must be in a blocking
// context (RI-2-ALLOW). This grep catches a missing fence before it janks the Tauri
// reactor.

fn c3_scan_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn c3_rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if !dir.exists() {
        return;
    }
    for entry in std::fs::read_dir(dir).expect("read scan dir") {
        let entry = entry.expect("dir entry");
        if entry.file_type().map(|t| t.is_symlink()).unwrap_or(true) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            c3_rust_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[test]
fn every_done_record_call_site_has_a_blocking_fence() {
    // The method call tokens C3 introduced. `insert_done_record` / `.verify_done(`
    // are the two, matching the story's Task-6 grep. The definitions live in
    // `src/weft/done_record.rs` (excluded below), so a hit elsewhere is a caller.
    const CALLS: &[&str] = &["insert_done_record", ".verify_done("];
    const ALLOW_MARKER: &str = "RI-2-ALLOW";

    let mut files = Vec::new();
    c3_rust_files(&c3_scan_root(), &mut files);

    let mut violations = Vec::new();
    for file in files {
        let s = file.to_string_lossy().replace('\\', "/");
        // Definitions + this story's tests live in the weft module tree — exempt.
        if s.contains("/src/weft/") {
            continue;
        }
        let src = std::fs::read_to_string(&file).expect("read source");
        let lines: Vec<&str> = src.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with("*") || trimmed.starts_with("use ")
            {
                continue;
            }
            // Only the code portion (drop a trailing `//` comment) so a doc mention
            // can't false-positive.
            let code = match line.find("//") {
                Some(idx) => &line[..idx],
                None => line,
            };
            let is_call = CALLS.iter().any(|c| code.contains(c));
            if !is_call {
                continue;
            }
            let self_ok = code.contains("spawn_blocking") || line.contains(ALLOW_MARKER);
            let prev_ok = i > 0 && {
                let prev = lines[i - 1];
                prev.contains("spawn_blocking") || prev.contains(ALLOW_MARKER)
            };
            if !self_ok && !prev_ok {
                violations.push(format!("{}:{}: {}", file.display(), i + 1, line.trim()));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "SW-WV-C3 BLOCKING-POOL: a done-record call reached a site without an adjacent \
         `spawn_blocking`/`await_blocking` offload or a `// RI-2-ALLOW` marker (the marker \
         must sit on the call line or the line directly above):\n{}",
        violations.join("\n")
    );
}

// ---------- AC-8: DoneRecord/VerifyDoneResult live in weft, no stray stubs ----------

#[test]
fn no_stub_definitions_outside_weft() {
    // AC-8: no `DoneRecord`/`VerifyDoneResult` DEFINITION outside `src/weft/`, and no
    // stray experimental stub tokens. A `struct DoneRecord`/`struct VerifyDoneResult`
    // definition anywhere but the weft module tree is a MODULE-HOME violation.
    let mut files = Vec::new();
    c3_rust_files(&c3_scan_root(), &mut files);
    let mut violations = Vec::new();
    for file in files {
        let s = file.to_string_lossy().replace('\\', "/");
        if s.contains("/src/weft/") {
            continue;
        }
        let src = std::fs::read_to_string(&file).expect("read source");
        for (i, line) in src.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") || code.starts_with('*') {
                continue;
            }
            if code.contains("struct DoneRecord") || code.contains("struct VerifyDoneResult") {
                violations.push(format!("{}:{}: {}", file.display(), i + 1, line.trim()));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "SW-WV-C3 MODULE-HOME: DoneRecord/VerifyDoneResult must be defined only in \
         src/weft/; stray definition:\n{}",
        violations.join("\n")
    );
}

// ---------- AC-7: off-thread dispatch of the two methods ----------

#[tokio::test]
async fn insert_and_verify_run_on_blocking_pool() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("weft.sqlite3");
    let store = WeftStore::open(&path).unwrap();
    let qid = seed_queue_row(&store);
    seed_done_dispatch(&store, "SW-WV-C3");
    let record = sample_record(qid, "review");

    let store_i = WeftStore::open(&path).unwrap();
    let inserted = tokio::task::spawn_blocking(move || store_i.insert_done_record(&record))
        .await
        .expect("join");
    assert!(inserted.unwrap(), "insert runs on the blocking pool");

    let store_v = WeftStore::open(&path).unwrap();
    let verified = tokio::task::spawn_blocking(move || {
        store_v.verify_done("disp-1", 1, Path::new("/nonexistent/run-store.jsonl"))
    })
    .await
    .expect("join");
    assert!(
        verified.unwrap().verified,
        "verify runs on the blocking pool"
    );
}
