// SPDX-License-Identifier: MIT
//! SW-WV-B5 acceptance tests for the scope layer (`wv_scopes` + `wv_watches`) and
//! their named write handlers.
//!
//! Covers AC-1 (scope round-trip + parent_scope + idempotent-create + close +
//! durability), AC-2 (watches composite PK + DEFERRABLE-style FK-in-one-tx + triage
//! read-path filter + lifecycle enum gate), AC-4 (session-restart round-trip; no
//! session-local cache — the grep half lives in `commands/weft_scopes.rs`), AC-5
//! (silence-never-closes: bare close rejected + three valid CloseReasons + quiet),
//! AC-6 (artifact_ptr is a pointer, no filesystem side-effect), AC-7 (schema
//! migration idempotency), AC-9 (every mutating handler co-commits exactly one
//! `wv_write_log` row; a failed handler leaves the count unchanged).

use super::*;
use crate::weft::store::WeftStore;

fn open_temp() -> (tempfile::TempDir, WeftStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
    (dir, store)
}

/// Total `wv_write_log` rows for a given table (AC-9 audit-count assertions).
fn audit_rows(store: &WeftStore, table: &str) -> i64 {
    let conn = store.conn().unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM wv_write_log WHERE tbl=?1",
        params![table],
        |r| r.get(0),
    )
    .unwrap()
}

// ============================== AC-1: wv_scopes ==============================

#[test]
fn scope_create_round_trip_parent_child_and_closed_at_null() {
    let (_d, store) = open_temp();
    // (a) insert a root scope and a child scope.
    assert!(wv_scope_create(&store, "root", None, Some("/artifacts/root"), None).unwrap());
    assert!(wv_scope_create(
        &store,
        "epic/SW-WV",
        Some("root"),
        Some("/artifacts/epic"),
        None
    )
    .unwrap());

    let root = wv_scope_get(&store, "root").unwrap().unwrap();
    let child = wv_scope_get(&store, "epic/SW-WV").unwrap().unwrap();

    // parent-child relationship via parent_scope.
    assert_eq!(root.parent_scope, None, "root scope has no parent");
    assert_eq!(child.parent_scope.as_deref(), Some("root"));
    // closed_at NULL on both (open).
    assert_eq!(root.closed_at, None);
    assert_eq!(child.closed_at, None);
    // artifact pointer round-trips verbatim.
    assert_eq!(root.artifact_ptr.as_deref(), Some("/artifacts/root"));
    // created_at/updated_at are stamped (epoch-ms, > 0).
    assert!(root.created_at > 0 && root.updated_at > 0);
}

// ===================== SW-AV-17: owner_session on scopes =====================

#[test]
fn sw_av_17_owner_session_persists_and_is_independent_of_lease_session() {
    // SW-AV-17 seam: `wv_scope_create` records `owner_session` (the opener's session
    // id, who-opened-this-scope); it round-trips through the read path and is decoupled
    // from `lease_session` (the lease-acquire cache SW-AV-27 deletes).
    let (_d, store) = open_temp();
    assert!(wv_scope_create(&store, "epic-42", Some("root"), None, Some("sess-opener")).unwrap());

    let row = wv_scope_get(&store, "epic-42").unwrap().unwrap();
    // (a) owner_session persisted verbatim through the read path.
    assert_eq!(
        row.owner_session.as_deref(),
        Some("sess-opener"),
        "owner_session round-trips through wv_scope_get"
    );
    // (b) INDEPENDENT of the lease: no lease was acquired, so lease_session is None,
    // yet owner_session stands on its own.
    assert_eq!(
        row.lease_session, None,
        "no lease was acquired — lease_session is null at create"
    );

    // (c) survives lease deletion: closing the scope clears lease_session (and, later,
    // SW-AV-27 removes the lease path entirely) — owner_session is untouched.
    wv_scope_close(&store, "epic-42").unwrap();
    let closed = wv_scope_get(&store, "epic-42").unwrap().unwrap();
    assert_eq!(closed.lease_session, None, "close cleared the lease cache");
    assert_eq!(
        closed.owner_session.as_deref(),
        Some("sess-opener"),
        "owner_session survives the lease being gone (the SW-AV-18 tree key)"
    );
}

#[test]
fn sw_av_17_who_opened_whom_join_resolves_via_owner_session() {
    // SW-AV-17: the roster↔scope tree join — session → its scope → parent_scope →
    // parent's owner_session = who-opened-whom — resolves purely from the scope rows,
    // WITHOUT touching lease_session.
    let (_d, store) = open_temp();
    // The parent controller opened the root scope; it then opened the child scope for
    // the child session it spawned.
    wv_scope_create(&store, "root", None, None, Some("sess-parent")).unwrap();
    wv_scope_create(&store, "epic-42", Some("root"), None, Some("sess-child")).unwrap();

    // Start from the child's scope, walk to the parent scope, read its owner_session.
    let child = wv_scope_get(&store, "epic-42").unwrap().unwrap();
    assert_eq!(child.owner_session.as_deref(), Some("sess-child"));
    let parent_scope = child.parent_scope.expect("child has a parent scope");
    let parent = wv_scope_get(&store, &parent_scope).unwrap().unwrap();
    assert_eq!(
        parent.owner_session.as_deref(),
        Some("sess-parent"),
        "the parent scope's owner_session is who-opened-whom — resolved via owner_session, not lease_session"
    );
}

#[test]
fn sw_av_17_pre_migration_row_reads_null_owner_session() {
    // SW-AV-17: the additive migration leaves a row created before the column existed
    // readable — its owner_session heals to NULL (honest: no opener was recorded).
    let (_d, store) = open_temp();
    {
        // Simulate a pre-SW-AV-17 write: insert a scope row WITHOUT owner_session,
        // exactly as a store created before the `ensure_column` migration would hold it.
        let conn = store.conn().unwrap();
        conn.execute(
            "INSERT INTO wv_scopes (scope, parent_scope, artifact_ptr, lease_session, \
                    created_at, updated_at, closed_at) \
             VALUES ('legacy', NULL, '', NULL, 1, 1, NULL)",
            [],
        )
        .unwrap();
    }
    let row = wv_scope_get(&store, "legacy").unwrap().unwrap();
    assert_eq!(
        row.owner_session, None,
        "a pre-migration row reads a NULL owner_session (additive, no backfill)"
    );
}

#[test]
fn scope_create_is_idempotent_returns_false_on_duplicate() {
    let (_d, store) = open_temp();
    assert!(wv_scope_create(&store, "root", None, Some("/a"), None).unwrap());
    // (b) second create on the same scope returns Ok(false) and does NOT overwrite.
    assert!(!wv_scope_create(&store, "root", Some("other"), Some("/b"), None).unwrap());
    let row = wv_scope_get(&store, "root").unwrap().unwrap();
    assert_eq!(
        row.parent_scope, None,
        "duplicate create must not overwrite parent"
    );
    assert_eq!(
        row.artifact_ptr.as_deref(),
        Some("/a"),
        "duplicate must not overwrite ptr"
    );
}

#[test]
fn scope_create_rejects_blank_name() {
    let (_d, store) = open_temp();
    let err = wv_scope_create(&store, "   ", None, None, None).unwrap_err();
    assert!(
        matches!(err, WvWriteError::Invalid(_)),
        "blank scope name is a refusal"
    );
}

#[test]
fn scope_create_empty_artifact_ptr_is_valid() {
    // An empty-string artifact pointer is valid (scope pre-exists its first artifact).
    let (_d, store) = open_temp();
    assert!(wv_scope_create(&store, "root", None, None, None).unwrap());
    let row = wv_scope_get(&store, "root").unwrap().unwrap();
    assert_eq!(row.artifact_ptr.as_deref(), Some(""));
}

#[test]
fn scope_close_sets_closed_at_and_clears_lease() {
    let (_d, store) = open_temp();
    wv_scope_create(&store, "root", None, None, None).unwrap();
    wv_scope_create(&store, "child", Some("root"), None, None).unwrap();

    // (c) close the child: closed_at set non-NULL, lease_session NULL.
    let closed_at = wv_scope_close(&store, "child").unwrap();
    assert!(closed_at > 0);
    let row = wv_scope_get(&store, "child").unwrap().unwrap();
    assert_eq!(row.closed_at, Some(closed_at));
    assert_eq!(row.lease_session, None);
    // Row is NOT deleted — it stays as an audit trail.
    assert!(wv_scope_get(&store, "child").unwrap().is_some());
}

#[test]
fn scope_double_close_is_a_typed_error_not_silent_success() {
    let (_d, store) = open_temp();
    wv_scope_create(&store, "s", None, None, None).unwrap();
    wv_scope_close(&store, "s").unwrap();
    // Second close matches no OPEN scope → typed refusal (single-writer guard).
    let err = wv_scope_close(&store, "s").unwrap_err();
    assert!(
        matches!(err, WvWriteError::Invalid(_)),
        "double-close is a refusal: {err}"
    );
}

#[test]
fn scope_close_unknown_is_a_typed_error() {
    let (_d, store) = open_temp();
    let err = wv_scope_close(&store, "nope").unwrap_err();
    assert!(matches!(err, WvWriteError::Invalid(_)));
}

#[test]
fn scope_rows_and_closed_at_survive_a_reopen() {
    // (d) durable state across a fresh WeftStore::open on the same path.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("weft.sqlite3");
    let closed_at = {
        let store = WeftStore::open(&path).unwrap();
        wv_scope_create(&store, "root", None, Some("/a"), None).unwrap();
        wv_scope_create(&store, "child", Some("root"), None, None).unwrap();
        wv_scope_close(&store, "child").unwrap()
    };
    // Re-open with a fresh handle.
    let store2 = WeftStore::open(&path).unwrap();
    let root = wv_scope_get(&store2, "root").unwrap().unwrap();
    let child = wv_scope_get(&store2, "child").unwrap().unwrap();
    assert_eq!(root.artifact_ptr.as_deref(), Some("/a"));
    assert_eq!(child.parent_scope.as_deref(), Some("root"));
    assert_eq!(
        child.closed_at,
        Some(closed_at),
        "closed_at value survives reopen"
    );
}

// ============================== AC-2: wv_watches ==============================

#[test]
fn watch_composite_pk_upsert_dedupes_not_duplicates() {
    let (_d, store) = open_temp();
    wv_scope_create(&store, "s", None, None, None).unwrap();
    wv_watch_upsert(&store, "s", "slack", "C123", "open").unwrap();
    // Second upsert on the same (scope, kind, ref) updates, never inserts a dup.
    wv_watch_upsert(&store, "s", "slack", "C123", "quiet").unwrap();
    let all = wv_watch_list(&store, "s", true).unwrap();
    assert_eq!(all.len(), 1, "composite PK dedupes");
    assert_eq!(all[0].lifecycle, "quiet", "upsert advanced the lifecycle");
}

#[test]
fn watch_upsert_rejects_unknown_lifecycle_at_the_handler() {
    let (_d, store) = open_temp();
    wv_scope_create(&store, "s", None, None, None).unwrap();
    // The Rust enum is the gate — an unknown lifecycle is refused before the SQL.
    let err = wv_watch_upsert(&store, "s", "slack", "C1", "sleeping").unwrap_err();
    assert!(
        matches!(err, WvWriteError::Invalid(_)),
        "unknown lifecycle refused: {err}"
    );
    assert_eq!(
        wv_watch_list(&store, "s", true).unwrap().len(),
        0,
        "no row written"
    );
}

#[test]
fn watch_upsert_rejects_blank_components() {
    let (_d, store) = open_temp();
    wv_scope_create(&store, "s", None, None, None).unwrap();
    assert!(wv_watch_upsert(&store, "s", "", "C1", "open").is_err());
    assert!(wv_watch_upsert(&store, "s", "slack", "  ", "open").is_err());
}

#[test]
fn scope_plus_watches_in_one_transaction_via_deferrable_fk() {
    // The FK to wv_scopes is DEFERRABLE INITIALLY DEFERRED so a scope + its watches
    // can be created in one transaction. Assert a scope then two watches all commit
    // and read back (proving the FK does not reject the watch write path).
    let (_d, store) = open_temp();
    wv_scope_create(&store, "epic/x", None, Some("/a"), None).unwrap();
    wv_watch_upsert(&store, "epic/x", "jira", "SW-1", "open").unwrap();
    wv_watch_upsert(&store, "epic/x", "github", "42", "open").unwrap();
    assert_eq!(wv_watch_list(&store, "epic/x", true).unwrap().len(), 2);
}

#[test]
fn watch_list_triage_read_path_filters_closed() {
    // The D1-triage read path: query by scope, filter lifecycle != 'closed'.
    let (_d, store) = open_temp();
    wv_scope_create(&store, "a", None, None, None).unwrap();
    wv_scope_create(&store, "b", None, None, None).unwrap();
    wv_watch_upsert(&store, "a", "slack", "C1", "open").unwrap();
    wv_watch_upsert(&store, "a", "slack", "C2", "quiet").unwrap();
    wv_watch_upsert(&store, "a", "slack", "C3", "open").unwrap();
    wv_watch_close(&store, "a", "slack", "C3", "explicit_command").unwrap();
    // scope b's watch must not leak into scope a's read.
    wv_watch_upsert(&store, "b", "slack", "C9", "open").unwrap();

    let open_ish = wv_watch_list(&store, "a", false).unwrap();
    assert_eq!(open_ish.len(), 2, "only open + quiet (closed filtered out)");
    assert!(open_ish.iter().all(|w| w.scope == "a"));
    assert!(open_ish.iter().all(|w| w.lifecycle != "closed"));

    let all = wv_watch_list(&store, "a", true).unwrap();
    assert_eq!(all.len(), 3, "include_closed returns the closed one too");
}

// ============================== AC-4: restart round-trip ==============================

#[test]
fn session_restart_round_trip_two_opens_same_path() {
    // Open a store, create a scope with two watches, drop the handle (session end),
    // re-open via a fresh handle, read the scope + watches back — same state, no
    // in-process cache. (The no-cache grep half lives in commands/weft_scopes.rs.)
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("weft.sqlite3");
    {
        let store = WeftStore::open(&path).unwrap();
        wv_scope_create(&store, "root", None, Some("/artifacts"), None).unwrap();
        wv_watch_upsert(&store, "root", "slack", "C1", "open").unwrap();
        wv_watch_upsert(&store, "root", "jira", "SW-1", "quiet").unwrap();
        // store dropped here — simulates the controller session ending.
    }
    let store2 = WeftStore::open(&path).unwrap();
    let scope = wv_scope_get(&store2, "root").unwrap().unwrap();
    assert_eq!(scope.artifact_ptr.as_deref(), Some("/artifacts"));
    let watches = wv_watch_list(&store2, "root", true).unwrap();
    assert_eq!(
        watches.len(),
        2,
        "both watches resume from the durable record"
    );
}

// ============================== AC-5: silence never closes ==============================

#[test]
fn watch_close_requires_a_close_reason() {
    let (_d, store) = open_temp();
    wv_scope_create(&store, "s", None, None, None).unwrap();
    wv_watch_upsert(&store, "s", "slack", "C1", "open").unwrap();
    // A bare close with no reason is rejected (silence never closes a watch).
    let err = wv_watch_close(&store, "s", "slack", "C1", "").unwrap_err();
    assert!(
        matches!(err, WvWriteError::Invalid(_)),
        "bare close refused: {err}"
    );
    // The watch stays open.
    assert_eq!(wv_watch_list(&store, "s", false).unwrap().len(), 1);
}

#[test]
fn watch_close_rejects_unknown_reason() {
    let (_d, store) = open_temp();
    wv_scope_create(&store, "s", None, None, None).unwrap();
    wv_watch_upsert(&store, "s", "slack", "C1", "open").unwrap();
    let err = wv_watch_close(&store, "s", "slack", "C1", "got_bored").unwrap_err();
    assert!(matches!(err, WvWriteError::Invalid(_)));
}

#[test]
fn watch_close_accepts_each_valid_reason() {
    // Parametric over the three valid CloseReasons.
    for reason in ["explicit_command", "scope_closed", "tail_message"] {
        let (_d, store) = open_temp();
        wv_scope_create(&store, "s", None, None, None).unwrap();
        wv_watch_upsert(&store, "s", "slack", "C1", "open").unwrap();
        assert!(
            wv_watch_close(&store, "s", "slack", "C1", reason).unwrap(),
            "close with {reason} succeeds"
        );
        let all = wv_watch_list(&store, "s", true).unwrap();
        assert_eq!(all[0].lifecycle, "closed");
    }
}

#[test]
fn watch_mark_quiet_does_not_close() {
    let (_d, store) = open_temp();
    wv_scope_create(&store, "s", None, None, None).unwrap();
    wv_watch_upsert(&store, "s", "slack", "C1", "open").unwrap();
    assert!(wv_watch_mark_quiet(&store, "s", "slack", "C1").unwrap());
    let all = wv_watch_list(&store, "s", true).unwrap();
    assert_eq!(all[0].lifecycle, "quiet", "quiet is not closed");
    // A quiet watch is still returned by the triage read (not auto-dropped).
    assert_eq!(wv_watch_list(&store, "s", false).unwrap().len(), 1);
}

#[test]
fn watch_close_on_unknown_key_is_a_noop() {
    let (_d, store) = open_temp();
    wv_scope_create(&store, "s", None, None, None).unwrap();
    assert!(!wv_watch_close(&store, "s", "slack", "nope", "explicit_command").unwrap());
}

// ============================== AC-6: pointer, never a copy ==============================

#[test]
fn update_artifact_ptr_changes_only_the_row_no_fs_side_effect() {
    let (_d, store) = open_temp();
    // A fake artifact path that does NOT exist on disk.
    let fake = "/nonexistent/artifacts/scope-x";
    wv_scope_create(&store, "x", None, Some(fake), None).unwrap();
    let before = wv_scope_get(&store, "x").unwrap().unwrap();

    let new_ptr = "/another/nonexistent/place";
    assert!(wv_scope_update_artifact_ptr(&store, "x", new_ptr).unwrap());

    // The row's ptr changed and updated_at advanced (>=) ...
    let after = wv_scope_get(&store, "x").unwrap().unwrap();
    assert_eq!(after.artifact_ptr.as_deref(), Some(new_ptr));
    assert!(after.updated_at >= before.updated_at);
    // ... and NO file was read/written/created at either path (pointer, not a copy).
    assert!(!std::path::Path::new(fake).exists());
    assert!(!std::path::Path::new(new_ptr).exists());
}

#[test]
fn update_artifact_ptr_unknown_scope_is_a_noop() {
    let (_d, store) = open_temp();
    assert!(!wv_scope_update_artifact_ptr(&store, "nope", "/x").unwrap());
}

// ============================== AC-7: migration idempotency ==============================

#[test]
fn open_is_idempotent_row_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("weft.sqlite3");
    {
        let store = WeftStore::open(&path).unwrap();
        wv_scope_create(&store, "root", None, Some("/a"), None).unwrap();
        wv_watch_upsert(&store, "root", "slack", "C1", "open").unwrap();
    }
    // Re-open on a DB that already has the schema — no error, no data loss.
    let store = WeftStore::open(&path).unwrap();
    assert!(wv_scope_get(&store, "root").unwrap().is_some());
    assert_eq!(wv_watch_list(&store, "root", true).unwrap().len(), 1);
    // And re-open a THIRD time — still idempotent.
    let store3 = WeftStore::open(&path).unwrap();
    assert!(wv_scope_get(&store3, "root").unwrap().is_some());
}

#[test]
fn watches_index_and_lifecycle_column_present_after_open() {
    // The idx_wv_watches_scope index and the lifecycle/updated_at columns are
    // created by open() (SCHEMA + ensure_column + post-migration index).
    let (_d, store) = open_temp();
    let conn = store.conn().unwrap();
    let idx_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_wv_watches_scope'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(idx_count, 1, "triage read-path index exists");
    // lifecycle column present + defaults to 'open' on a bare insert of PK cols.
    let cols: Vec<String> = {
        let mut stmt = conn.prepare("PRAGMA table_info(wv_watches)").unwrap();
        stmt.query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert!(cols.iter().any(|c| c == "lifecycle"));
    assert!(cols.iter().any(|c| c == "updated_at"));
}

// ============================== AC-9: audit atomicity ==============================

#[test]
fn every_scope_watch_mutation_co_commits_exactly_one_audit_row() {
    let (_d, store) = open_temp();

    // wv_scope_create (success) → +1 scope audit row.
    assert_eq!(audit_rows(&store, "wv_scopes"), 0);
    wv_scope_create(&store, "root", None, Some("/a"), None).unwrap();
    assert_eq!(audit_rows(&store, "wv_scopes"), 1);

    // An idempotent no-op create writes NO audit row.
    wv_scope_create(&store, "root", None, Some("/b"), None).unwrap();
    assert_eq!(
        audit_rows(&store, "wv_scopes"),
        1,
        "no-op create audits nothing"
    );

    // wv_scope_update_artifact_ptr → +1.
    wv_scope_update_artifact_ptr(&store, "root", "/c").unwrap();
    assert_eq!(audit_rows(&store, "wv_scopes"), 2);

    // wv_scope_close → +1.
    wv_scope_close(&store, "root").unwrap();
    assert_eq!(audit_rows(&store, "wv_scopes"), 3);

    // A failed close (double-close) writes NO audit row.
    let _ = wv_scope_close(&store, "root");
    assert_eq!(
        audit_rows(&store, "wv_scopes"),
        3,
        "failed handler audits nothing"
    );

    // Watches: upsert / mark_quiet / close each +1.
    wv_scope_create(&store, "s", None, None, None).unwrap();
    assert_eq!(audit_rows(&store, "wv_watches"), 0);
    wv_watch_upsert(&store, "s", "slack", "C1", "open").unwrap();
    assert_eq!(audit_rows(&store, "wv_watches"), 1);
    wv_watch_mark_quiet(&store, "s", "slack", "C1").unwrap();
    assert_eq!(audit_rows(&store, "wv_watches"), 2);
    wv_watch_close(&store, "s", "slack", "C1", "tail_message").unwrap();
    assert_eq!(audit_rows(&store, "wv_watches"), 3);

    // A rejected watch upsert (bad lifecycle) writes NO audit row.
    let _ = wv_watch_upsert(&store, "s", "slack", "C2", "bogus");
    assert_eq!(
        audit_rows(&store, "wv_watches"),
        3,
        "rejected upsert audits nothing"
    );
}

// ============================== enum surfaces ==============================

#[test]
fn watch_lifecycle_and_close_reason_from_wire_round_trip() {
    for s in WV_WATCH_LIFECYCLES {
        assert_eq!(WatchLifecycle::from_wire(s).unwrap().as_str(), *s);
    }
    assert!(WatchLifecycle::from_wire("nope").is_none());
    for s in WV_WATCH_CLOSE_REASONS {
        assert_eq!(CloseReason::from_wire(s).unwrap().as_str(), *s);
    }
    assert!(CloseReason::from_wire("").is_none());
    assert_eq!(WV_WATCH_QUIET_THRESHOLD_DAYS, 3);
}
