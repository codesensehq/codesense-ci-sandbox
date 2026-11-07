// SPDX-License-Identifier: MIT
//! SW-WV-B1 acceptance tests for the sanctioned write path.
//!
//! Covers AC-1 (one write path + grep lint), AC-2 (live-max ids under
//! concurrency), AC-3 (single-writer), AC-5 (idempotent open + path math), AC-6
//! (no `WeftStore` call on the UI thread), AC-7 (atomic audit), and AC-8 (the
//! `wv_*` interactive-verb conformance check). AC-4 (capability profiles) lives
//! in `commands/mcp_scope.rs` alongside the pack fixtures.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::*;
use crate::weft::store::{MutateResult, WeftStore};

fn open_temp() -> (tempfile::TempDir, WeftStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
    (dir, store)
}

// ---------- AC-2: live-max ids, never a cached counter ----------

#[test]
fn duplicate_ids_impossible_under_concurrent_append() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("weft.sqlite3");
    // Materialize the schema once before racing.
    WeftStore::open(&path).unwrap();

    const PER_THREAD: usize = 50;
    let ids: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));

    let mut handles = Vec::new();
    for _ in 0..2 {
        let path = path.clone();
        let ids = Arc::clone(&ids);
        handles.push(std::thread::spawn(move || {
            // Each thread opens its OWN store handle on the same DB file — the
            // race is resolved inside SQLite's IMMEDIATE transaction, not a Rust
            // mutex around the append.
            let store = WeftStore::open(&path).unwrap();
            let mut conn = store.conn().unwrap();
            let mut local = Vec::with_capacity(PER_THREAD);
            for _ in 0..PER_THREAD {
                let id = wv_append(
                    &mut conn,
                    "wv_queue",
                    "INSERT INTO wv_queue (id) VALUES (?1)",
                    &[],
                )
                .unwrap();
                local.push(id);
            }
            ids.lock().unwrap().extend(local);
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    let ids = ids.lock().unwrap();
    let n = 2 * PER_THREAD;
    assert_eq!(ids.len(), n, "every append returned an id");
    let unique: BTreeSet<i64> = ids.iter().copied().collect();
    assert_eq!(
        unique.len(),
        n,
        "no duplicate ids under concurrent append (the cached-counter collision)"
    );
    // The union covers exactly 1..=N — no gaps, no overshoot.
    let expected: BTreeSet<i64> = (1..=n as i64).collect();
    assert_eq!(unique, expected, "ids cover every integer from 1 to N");
}

// ---------- AC-3: single-writer per record ----------

/// Seed a `wv_queue` row that carries an owner column, so the mutate tests have a
/// concrete owner-guarded UPDATE to exercise. `claimed_by` is added via the
/// B2-shaped migration path (`ensure_column`) rather than the B1 stub.
fn seed_owned_queue_row(store: &WeftStore, owner: &str) -> i64 {
    let conn = store.conn().unwrap();
    crate::shared::sqlite::ensure_column(&conn, "wv_queue", "claimed_by", "TEXT").unwrap();
    drop(conn);
    let mut conn = store.conn().unwrap();
    wv_append(
        &mut conn,
        "wv_queue",
        "INSERT INTO wv_queue (id, claimed_by) VALUES (?1, ?2)",
        &[&owner],
    )
    .unwrap()
}

fn read_claimed_by(store: &WeftStore, id: i64) -> Option<String> {
    let conn = store.conn().unwrap();
    conn.query_row(
        "SELECT claimed_by FROM wv_queue WHERE id=?1",
        params![id],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn owner_mutation_succeeds() {
    let (_dir, store) = open_temp();
    let id = seed_owned_queue_row(&store, "worker-A");
    let mut conn = store.conn().unwrap();
    let result = wv_mutate_owned(
        &mut conn,
        "wv_queue",
        id,
        "claimed_by",
        "worker-A",
        "UPDATE wv_queue SET claimed_by=?3 WHERE id=?1 AND claimed_by=?2",
        &[&"worker-A-reassigned"],
    )
    .unwrap();
    assert_eq!(result, MutateResult::Updated);
    assert_eq!(
        read_claimed_by(&store, id).as_deref(),
        Some("worker-A-reassigned"),
        "the owner's mutation was applied"
    );
}

#[test]
fn non_owner_mutation_refused() {
    let (_dir, store) = open_temp();
    let id = seed_owned_queue_row(&store, "worker-A");
    let mut conn = store.conn().unwrap();
    let result = wv_mutate_owned(
        &mut conn,
        "wv_queue",
        id,
        "claimed_by",
        "worker-B",
        "UPDATE wv_queue SET claimed_by=?3 WHERE id=?1 AND claimed_by=?2",
        &[&"worker-B-steal"],
    )
    .unwrap();
    assert_eq!(result, MutateResult::NotOwner, "a non-owner loses the CAS");
    assert_eq!(
        read_claimed_by(&store, id).as_deref(),
        Some("worker-A"),
        "the row is unchanged after a NotOwner mutation"
    );
}

// ---------- AC-5: idempotent open, forward-migration, path math ----------

#[test]
fn open_is_idempotent_and_retains_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("weft.sqlite3");

    let store = WeftStore::open(&path).unwrap();
    let mut conn = store.conn().unwrap();
    let id = wv_append(
        &mut conn,
        "wv_queue",
        "INSERT INTO wv_queue (id) VALUES (?1)",
        &[],
    )
    .unwrap();
    drop(conn);
    drop(store);

    // Re-open the same path — a second open must not error or lose the row.
    let store = WeftStore::open(&path).unwrap();
    let conn = store.conn().unwrap();
    let found: i64 = conn
        .query_row("SELECT id FROM wv_queue WHERE id=?1", params![id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(found, id, "the row survives a drop + re-open");
}

// SW-WV-C2 (AC-2) — the tripwire columns are present after TWO opens on the same
// database (proves `ensure_column` idempotency on an already-migrated store), and
// the key-ledger `meta` column (AC-5) likewise.
#[test]
fn tripwire_columns_present_after_two_opens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("weft.sqlite3");

    // First open creates + migrates.
    let store = WeftStore::open(&path).unwrap();
    drop(store);
    // Second open on the same file must not error (idempotent ensure_column).
    let store = WeftStore::open(&path).unwrap();
    let conn = store.conn().unwrap();

    let queue_cols = column_names(&conn, "wv_queue");
    assert!(
        queue_cols.iter().any(|c| c == "tripwire_verdict"),
        "wv_queue carries tripwire_verdict after two opens: {queue_cols:?}"
    );
    assert!(
        queue_cols.iter().any(|c| c == "verify_fix_cycles"),
        "wv_queue carries verify_fix_cycles after two opens: {queue_cols:?}"
    );
    assert!(
        queue_cols.iter().any(|c| c == "fired_row"),
        "wv_queue carries fired_row after two opens: {queue_cols:?}"
    );
    let ledger_cols = column_names(&conn, "wv_key_ledger");
    assert!(
        ledger_cols.iter().any(|c| c == "meta"),
        "wv_key_ledger carries the AC-5 meta blob column: {ledger_cols:?}"
    );

    // The defaults are legal: a bare-id insert reads 'pending' / 0.
    conn.execute("INSERT INTO wv_queue (id) VALUES (42)", [])
        .unwrap();
    let (verdict, cycles): (String, i64) = conn
        .query_row(
            "SELECT tripwire_verdict, verify_fix_cycles FROM wv_queue WHERE id=42",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(verdict, "pending");
    assert_eq!(cycles, 0);
}

fn column_names(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    let rows = stmt.query_map([], |r| r.get::<_, String>(1)).unwrap();
    rows.collect::<Result<Vec<_>, _>>().unwrap()
}

#[test]
fn ensure_column_migration_fires_on_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("weft.sqlite3");

    let store = WeftStore::open(&path).unwrap();
    let conn = store.conn().unwrap();
    // Simulate a future story adding a column via the forward-migration idiom.
    crate::shared::sqlite::ensure_column(&conn, "wv_queue", "_test_col", "TEXT").unwrap();
    drop(conn);
    drop(store);

    let store = WeftStore::open(&path).unwrap();
    let conn = store.conn().unwrap();
    assert!(
        column_names(&conn, "wv_queue")
            .iter()
            .any(|c| c == "_test_col"),
        "the migrated column persists across re-open"
    );
}

fn store_principal() -> crate::domain::Principal {
    crate::domain::Principal::from(42u64)
}

#[test]
fn weft_store_path_is_outside_the_context_repo_clone() {
    use crate::context_repo::{path_to_install_root, path_to_org_root};
    let home = PathBuf::from("/home/dev/.codesense");
    let principal = store_principal();
    let path = weft_store_path(&home, &principal, "acme/api", "default").unwrap();
    let org_root = path_to_org_root(&home, &principal, "acme/api").unwrap();
    assert!(
        !path.starts_with(&org_root),
        "store {path:?} must not live inside the clone {org_root:?}",
    );
    let install_root = path_to_install_root(&home, &principal);
    assert_eq!(
        path,
        install_root.join("acme/api/default/weft.sqlite3"),
        "store lives at {{install_root}}/{{org}}/{{team}}/weft.sqlite3",
    );
}

#[test]
fn weft_store_path_rejects_names_that_escape_the_install_root() {
    let home = PathBuf::from("/home/dev/.codesense");
    let p = store_principal();
    for (org, team) in [
        ("acme/api", ".."),
        ("acme/api", "a/b"),
        ("acme/api", ""),
        ("acme/api", "_none"),
        ("../api", "default"),
        ("acme/..", "default"),
        ("acme//api", "default"),
    ] {
        assert!(
            weft_store_path(&home, &p, org, team).is_err(),
            "{org:?}/{team:?} must be rejected",
        );
    }
}

fn write_store_files(base: &Path, tag: &str) {
    std::fs::create_dir_all(base.parent().unwrap()).unwrap();
    std::fs::write(base, format!("{tag}-db")).unwrap();
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = base.as_os_str().to_owned();
        sidecar.push(suffix);
        std::fs::write(PathBuf::from(sidecar), format!("{tag}{suffix}")).unwrap();
    }
}

fn read_sidecar(base: &Path, suffix: &str) -> Option<String> {
    let mut sidecar = base.as_os_str().to_owned();
    sidecar.push(suffix);
    std::fs::read_to_string(PathBuf::from(sidecar)).ok()
}

#[test]
fn adopt_moves_the_old_store_and_its_sidecars() {
    let tmp = tempfile::tempdir().unwrap();
    let legacy = tmp.path().join("clone/default/install/weft.sqlite3");
    let dest = tmp.path().join("installs/acme/api/default/weft.sqlite3");
    write_store_files(&legacy, "old");

    assert!(adopt_legacy_weft_store(&legacy, &dest).unwrap());

    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "old-db");
    assert_eq!(read_sidecar(&dest, "-wal").as_deref(), Some("old-wal"));
    assert_eq!(read_sidecar(&dest, "-shm").as_deref(), Some("old-shm"));
    assert!(!legacy.exists());
    assert_eq!(read_sidecar(&legacy, "-wal"), None);
    assert_eq!(read_sidecar(&legacy, "-shm"), None);
    // Nothing left to move: a second call is a no-op.
    assert!(!adopt_legacy_weft_store(&legacy, &dest).unwrap());
}

#[test]
fn adopt_never_replaces_an_existing_store() {
    let tmp = tempfile::tempdir().unwrap();
    let legacy = tmp.path().join("clone/default/install/weft.sqlite3");
    let dest = tmp.path().join("installs/acme/api/default/weft.sqlite3");
    write_store_files(&legacy, "old");
    write_store_files(&dest, "new");

    assert!(!adopt_legacy_weft_store(&legacy, &dest).unwrap());

    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "new-db");
    assert_eq!(read_sidecar(&dest, "-wal").as_deref(), Some("new-wal"));
    assert_eq!(std::fs::read_to_string(&legacy).unwrap(), "old-db");
}

#[test]
fn adopt_finishes_a_move_interrupted_after_the_sidecars() {
    let tmp = tempfile::tempdir().unwrap();
    let legacy = tmp.path().join("clone/default/install/weft.sqlite3");
    let dest = tmp.path().join("installs/acme/api/default/weft.sqlite3");
    write_store_files(&legacy, "old");
    // Simulate a crash after the sidecars moved but before the main file did.
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    for suffix in ["-wal", "-shm"] {
        let mut from = legacy.as_os_str().to_owned();
        from.push(suffix);
        let mut to = dest.as_os_str().to_owned();
        to.push(suffix);
        std::fs::rename(PathBuf::from(from), PathBuf::from(to)).unwrap();
    }

    assert!(adopt_legacy_weft_store(&legacy, &dest).unwrap());

    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "old-db");
    assert_eq!(read_sidecar(&dest, "-wal").as_deref(), Some("old-wal"));
    assert!(!legacy.exists());
}

#[test]
fn resolve_adopts_the_in_clone_store_with_its_rows() {
    use crate::context_repo::path_to_org_root;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let principal = store_principal();
    let org_root = path_to_org_root(home, &principal, "acme/api").unwrap();
    let legacy = legacy_in_clone_weft_store_path(&org_root, "default");
    assert_eq!(
        legacy,
        org_root.join("default/install/weft.sqlite3"),
        "the old store sat inside the clone",
    );
    {
        let store = WeftStore::open(&legacy).unwrap();
        crate::weft::inbox::inbox_insert(&store, "H1", "H2", "kept", None, None, None, None, None)
            .unwrap();
    }

    let path = resolve_weft_store_path(home, &principal, "acme/api", "default").unwrap();

    assert_eq!(
        path,
        weft_store_path(home, &principal, "acme/api", "default").unwrap()
    );
    assert!(
        !legacy.exists(),
        "the old in-clone store is moved, not copied"
    );
    let store = WeftStore::open(&path).unwrap();
    let rows = crate::weft::inbox::inbox_list(&store).unwrap();
    assert_eq!(
        rows.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
        ["kept"]
    );
}

#[test]
fn resolve_without_an_old_store_creates_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let principal = store_principal();
    let path = resolve_weft_store_path(tmp.path(), &principal, "acme/api", "default").unwrap();
    assert!(!path.exists());
}

// ---------- AC-7: audit row written atomically with every mutation ----------

fn audit_count(store: &WeftStore, table: &str, row_id: i64) -> i64 {
    let conn = store.conn().unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM wv_write_log WHERE tbl=?1 AND row_id=?2",
        params![table, row_id],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn wv_audit_atomic_append() {
    let (_dir, store) = open_temp();
    let mut conn = store.conn().unwrap();
    let id = wv_append(
        &mut conn,
        "wv_queue",
        "INSERT INTO wv_queue (id) VALUES (?1)",
        &[],
    )
    .unwrap();
    assert_eq!(
        audit_count(&store, "wv_queue", id),
        1,
        "exactly one audit row per append, in the same transaction"
    );
}

#[test]
fn wv_auto_append_writes_audit() {
    let (_dir, store) = open_temp();
    let mut conn = store.conn().unwrap();
    let id = wv_auto_append(
        &mut conn,
        "INSERT INTO wv_decisions DEFAULT VALUES",
        &[],
        "wv_decisions",
    )
    .unwrap();
    assert_eq!(
        audit_count(&store, "wv_decisions", id),
        1,
        "an AUTOINCREMENT append also writes its audit row atomically"
    );
}

#[test]
fn wv_mutate_owned_audits_only_on_update() {
    let (_dir, store) = open_temp();
    let id = seed_owned_queue_row(&store, "worker-A");

    // A NotOwner mutation writes no audit row.
    let mut conn = store.conn().unwrap();
    let miss = wv_mutate_owned(
        &mut conn,
        "wv_queue",
        id,
        "claimed_by",
        "worker-B",
        "UPDATE wv_queue SET claimed_by=?3 WHERE id=?1 AND claimed_by=?2",
        &[&"nope"],
    )
    .unwrap();
    assert_eq!(miss, MutateResult::NotOwner);
    // Only the append's audit row exists so far (operation="append").
    let conn = store.conn().unwrap();
    let mutate_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wv_write_log WHERE tbl='wv_queue' AND row_id=?1 AND operation='mutate'",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(mutate_rows, 0, "NotOwner writes no audit row");
    drop(conn);

    // An owner mutation writes exactly one mutate audit row.
    let mut conn = store.conn().unwrap();
    let hit = wv_mutate_owned(
        &mut conn,
        "wv_queue",
        id,
        "claimed_by",
        "worker-A",
        "UPDATE wv_queue SET claimed_by=?3 WHERE id=?1 AND claimed_by=?2",
        &[&"worker-A2"],
    )
    .unwrap();
    assert_eq!(hit, MutateResult::Updated);
    let conn = store.conn().unwrap();
    let mutate_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wv_write_log WHERE tbl='wv_queue' AND row_id=?1 AND operation='mutate'",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        mutate_rows, 1,
        "Updated writes exactly one mutate audit row"
    );
}

#[test]
fn wv_audit_atomic_rollback() {
    let (_dir, store) = open_temp();
    let mut conn = store.conn().unwrap();
    // Bypass the sanctioned write path: open an IMMEDIATE transaction directly,
    // insert into wv_queue, then roll back by dropping the tx.
    {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        tx.execute("INSERT INTO wv_queue (id) VALUES (999)", [])
            .unwrap();
        // Drop without commit → rollback.
    }
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wv_write_log WHERE tbl='wv_queue'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 0,
        "bypassing wv_append leaves no audit trail — the raw back door the AC-1 grep catches"
    );
}

// ---------- SW-WV-B6: audit-trail query + prune surface ----------
//
// B6 reads/prunes the `wv_write_log` trail B1's sanctioned write path produces.
// Rows are seeded via `wv_append` / `wv_auto_append` (real audit rows), then —
// where a synthetic age is needed — their `written_at` is overwritten by a raw
// UPDATE. That raw write is legal ONLY inside this sanctioned weft file (the
// `only_weft_sqlite_writes_wv_tables` grep excludes it); a production caller
// would never touch `wv_write_log` directly.

/// Force a `wv_write_log` row's `written_at` to a synthetic epoch-seconds value,
/// so age-sensitive filters (`after_at`, prune cutoff) can be exercised
/// deterministically. Sanctioned-file-only (see module note).
fn set_written_at(store: &WeftStore, audit_id: i64, secs: i64) {
    let conn = store.conn().unwrap();
    let changed = conn
        .execute(
            "UPDATE wv_write_log SET written_at=?2 WHERE id=?1",
            params![audit_id, secs],
        )
        .unwrap();
    assert_eq!(changed, 1, "the audit row exists");
}

/// The audit id of the row `wv_append` just wrote for `(tbl, row_id)`. Each
/// append writes exactly one `operation='append'` audit row (AC-7).
fn last_audit_id(store: &WeftStore, table: &str, row_id: i64) -> i64 {
    let conn = store.conn().unwrap();
    conn.query_row(
        "SELECT id FROM wv_write_log WHERE tbl=?1 AND row_id=?2 AND operation='append'",
        params![table, row_id],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn query_audit_table_filter_returns_only_that_table() {
    let (_dir, store) = open_temp();
    let mut conn = store.conn().unwrap();
    // Seed across two tables: six wv_queue appends, four wv_decisions appends —
    // each append writes one audit row via the sanctioned path.
    for _ in 0..6 {
        wv_append(
            &mut conn,
            "wv_queue",
            "INSERT INTO wv_queue (id) VALUES (?1)",
            &[],
        )
        .unwrap();
    }
    for _ in 0..4 {
        wv_auto_append(
            &mut conn,
            "INSERT INTO wv_decisions DEFAULT VALUES",
            &[],
            "wv_decisions",
        )
        .unwrap();
    }

    let rows = store
        .query_audit(AuditQueryParams {
            table_name: Some("wv_queue".to_string()),
            limit: 100,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(rows.len(), 6, "only the six wv_queue audit rows");
    assert!(
        rows.iter().all(|r| r.table_name == "wv_queue"),
        "no other table leaks into a table-filtered query"
    );
    // Descending id order (AC-1 wire contract).
    assert!(
        rows.windows(2).all(|w| w[0].id > w[1].id),
        "rows are returned in descending id order"
    );
}

#[test]
fn query_audit_operation_filter_discriminates_append_vs_mutate() {
    let (_dir, store) = open_temp();
    // One append + one owner mutation on the same row → one 'append' + one
    // 'mutate' audit row.
    let id = seed_owned_queue_row(&store, "worker-A");
    let mut conn = store.conn().unwrap();
    wv_mutate_owned(
        &mut conn,
        "wv_queue",
        id,
        "claimed_by",
        "worker-A",
        "UPDATE wv_queue SET claimed_by=?3 WHERE id=?1 AND claimed_by=?2",
        &[&"worker-A2"],
    )
    .unwrap();

    let mutates = store
        .query_audit(AuditQueryParams {
            operation: Some("mutate".to_string()),
            limit: 100,
            ..Default::default()
        })
        .unwrap();
    assert!(
        mutates.iter().all(|r| r.operation == "mutate"),
        "operation filter returns only mutate rows"
    );
    assert!(
        mutates.iter().any(|r| r.row_id == id),
        "the seeded row's mutate audit is present"
    );
}

#[test]
fn query_audit_paginates_all_rows_exactly_once_via_since_id() {
    let (_dir, store) = open_temp();
    let mut conn = store.conn().unwrap();
    // Twenty appends → twenty audit rows (ids 1..=20 contiguous, AUTOINCREMENT).
    for _ in 0..20 {
        wv_append(
            &mut conn,
            "wv_queue",
            "INSERT INTO wv_queue (id) VALUES (?1)",
            &[],
        )
        .unwrap();
    }

    // Cursor pagination in pages of five. Rows come back DESC by id, so the
    // natural cursor is `before_id` (exclusive UPPER bound): each page returns
    // the five largest ids strictly below the previous page's smallest, yielding
    // disjoint descending windows that together cover the whole log.
    let mut seen: BTreeSet<i64> = BTreeSet::new();
    let mut before: Option<i64> = None;
    let mut pages = 0;
    loop {
        let page = store
            .query_audit(AuditQueryParams {
                before_id: before,
                limit: 5,
                ..Default::default()
            })
            .unwrap();
        if page.is_empty() {
            break;
        }
        pages += 1;
        assert!(page.len() <= 5, "a page never exceeds the requested limit");
        for row in &page {
            assert!(
                seen.insert(row.id),
                "row id {} returned in more than one page",
                row.id
            );
        }
        // Next page: everything strictly below the smallest id on this page.
        before = Some(page.iter().map(|r| r.id).min().unwrap());
    }
    assert_eq!(
        pages, 4,
        "twenty rows over pages of five span exactly four pages"
    );
    assert_eq!(seen.len(), 20, "every audit row returned exactly once");
    assert_eq!(
        seen,
        (1..=20).collect::<BTreeSet<i64>>(),
        "the union covers audit ids 1..=20 with no gaps or repeats"
    );

    // `since_id` composes as the exclusive LOWER bound: a window bounded by both
    // cursors returns exactly the ids in the open interval, newest-first.
    let window = store
        .query_audit(AuditQueryParams {
            since_id: Some(5),
            before_id: Some(11),
            limit: 100,
            ..Default::default()
        })
        .unwrap();
    let window_ids: Vec<i64> = window.iter().map(|r| r.id).collect();
    assert_eq!(
        window_ids,
        vec![10, 9, 8, 7, 6],
        "since_id/before_id bound an open interval, returned DESC"
    );
}

#[test]
fn query_audit_after_at_filters_on_written_at_seconds() {
    let (_dir, store) = open_temp();
    let mut conn = store.conn().unwrap();
    let old = wv_append(
        &mut conn,
        "wv_queue",
        "INSERT INTO wv_queue (id) VALUES (?1)",
        &[],
    )
    .unwrap();
    let recent = wv_append(
        &mut conn,
        "wv_queue",
        "INSERT INTO wv_queue (id) VALUES (?1)",
        &[],
    )
    .unwrap();
    let old_audit = last_audit_id(&store, "wv_queue", old);
    let recent_audit = last_audit_id(&store, "wv_queue", recent);
    set_written_at(&store, old_audit, 1_000);
    set_written_at(&store, recent_audit, 5_000);

    let rows = store
        .query_audit(AuditQueryParams {
            after_at: Some(4_000),
            limit: 100,
            ..Default::default()
        })
        .unwrap();
    let ids: BTreeSet<i64> = rows.iter().map(|r| r.id).collect();
    assert!(ids.contains(&recent_audit), "the recent row is included");
    assert!(
        !ids.contains(&old_audit),
        "a row older than after_at is excluded"
    );
}

#[test]
fn query_audit_limit_clamps_to_max() {
    let (_dir, store) = open_temp();
    // A request for more than the hard cap resolves to the cap via the shared
    // clamp (nothing is seeded, so the count is 0 either way — the clamp
    // arithmetic itself is unit-tested in `clamp_audit_limit_defaults_and_caps`).
    let over = store
        .query_audit(AuditQueryParams {
            limit: AUDIT_QUERY_MAX_LIMIT + 1_000,
            ..Default::default()
        })
        .unwrap();
    assert!(over.len() as u32 <= AUDIT_QUERY_MAX_LIMIT);
}

#[test]
fn clamp_audit_limit_defaults_and_caps() {
    assert_eq!(super::clamp_audit_limit(0), AUDIT_QUERY_DEFAULT_LIMIT);
    assert_eq!(super::clamp_audit_limit(10), 10);
    assert_eq!(
        super::clamp_audit_limit(AUDIT_QUERY_MAX_LIMIT + 1),
        AUDIT_QUERY_MAX_LIMIT
    );
}

#[test]
fn prune_audit_removes_old_rows_and_keeps_recent() {
    let (_dir, store) = open_temp();
    let mut conn = store.conn().unwrap();
    // Three appends; force two into the "old" epoch and one "recent".
    let a = wv_append(
        &mut conn,
        "wv_queue",
        "INSERT INTO wv_queue (id) VALUES (?1)",
        &[],
    )
    .unwrap();
    let b = wv_append(
        &mut conn,
        "wv_queue",
        "INSERT INTO wv_queue (id) VALUES (?1)",
        &[],
    )
    .unwrap();
    let c = wv_append(
        &mut conn,
        "wv_queue",
        "INSERT INTO wv_queue (id) VALUES (?1)",
        &[],
    )
    .unwrap();
    let a_audit = last_audit_id(&store, "wv_queue", a);
    let b_audit = last_audit_id(&store, "wv_queue", b);
    let c_audit = last_audit_id(&store, "wv_queue", c);
    set_written_at(&store, a_audit, 1_000);
    set_written_at(&store, b_audit, 1_500);
    set_written_at(&store, c_audit, 9_000);

    // Cutoff at 2000s → the two <2000 rows go, the 9000 row stays.
    let deleted = store.prune_audit(2_000).unwrap();
    assert_eq!(deleted, 2, "exactly the two old rows were pruned");

    let remaining = store
        .query_audit(AuditQueryParams {
            limit: 100,
            ..Default::default()
        })
        .unwrap();
    let ids: BTreeSet<i64> = remaining.iter().map(|r| r.id).collect();
    assert!(ids.contains(&c_audit), "the recent row survives");
    assert!(!ids.contains(&a_audit) && !ids.contains(&b_audit));
}

#[test]
fn prune_audit_on_empty_log_deletes_nothing() {
    let (_dir, store) = open_temp();
    assert_eq!(store.prune_audit(i64::MAX).unwrap(), 0);
}

// ---------- SW-WV-B6 AC-4: off-thread dispatch of the read + prune methods ----------

/// B6 AC-4: the query and prune methods run on the blocking pool. This exercises
/// the exact `spawn_blocking` wrapping the MCP handler uses and asserts both
/// return `Ok(_)`, proving off-thread dispatch compiles and runs. (B1 owns the
/// single shared CI grep-guard per D7; this is B6's per-story conformance
/// evidence, not a second guard.)
#[tokio::test]
async fn b6_query_and_prune_run_on_blocking_pool() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("weft.sqlite3");
    WeftStore::open(&path).unwrap();

    let store = WeftStore::open(&path).unwrap();
    let q = tokio::task::spawn_blocking(move || {
        store.query_audit(AuditQueryParams {
            limit: 10,
            ..Default::default()
        })
    })
    .await
    .expect("join");
    assert!(q.is_ok(), "query_audit returns Ok on the blocking pool");

    let store = WeftStore::open(&path).unwrap();
    let p = tokio::task::spawn_blocking(move || store.prune_audit(0))
        .await
        .expect("join");
    assert!(p.is_ok(), "prune_audit returns Ok on the blocking pool");
}

// ---------- AC-8: wv_* interactive-verb conformance ----------

#[test]
fn every_wv_interactive_verb_has_both_seams() {
    let main = crate::commands::interactive_gate::INTERACTIVE_VERBS;
    for verb in WV_INTERACTIVE_VERBS {
        assert!(
            main.contains(verb),
            "wv_* interactive verb {verb:?} must also appear in \
             commands::interactive_gate::INTERACTIVE_VERBS (both seams required)"
        );
    }
    // No starter agent the app ships may reach a wv_* verb: the weft write path is Rust-only.
    for (key, agent) in crate::agents::builtin::all() {
        let servers = &agent.mcp.servers;
        assert!(
            !servers.iter().any(|s| s.starts_with("wv_")),
            "starter {key} must not carry a wv_* MCP verb: {servers:?}"
        );
    }
}

// ---------- AC-1 + AC-6: source-tree grep lints ----------
//
// These read the crate source with `std::fs` (no walkdir needed for a
// recurse-and-collect over `.rs` files) and assert the sanctioned-write-path and
// blocking-pool disciplines hold across the whole tree. They are the ONE shared
// CI enforcement harness for the SW-WV epic (D7); other stories reference them.

fn scan_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if !dir.exists() {
        return;
    }
    for entry in std::fs::read_dir(dir).expect("read scan dir") {
        let entry = entry.expect("dir entry");
        // Skip symlinks (no cycle/escape risk; `file_type` does not follow).
        if entry.file_type().map(|t| t.is_symlink()).unwrap_or(true) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn is_comment(trimmed: &str) -> bool {
    trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*')
}

/// Whitespace-squished, comment-stripped code portion of a line, so a token
/// mentioned only in a trailing comment or a doc line can't false-positive.
fn strip_line_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_str = false;
    let mut i = 0;
    while i + 1 < bytes.len() {
        match bytes[i] {
            b'"' => in_str = !in_str,
            b'/' if !in_str && bytes[i + 1] == b'/' => return &line[..i],
            _ => {}
        }
        i += 1;
    }
    line
}

/// The sanctioned home for raw `wv_*` SQL is the whole `src/weft/` module tree —
/// ARCHITECTURE §1.2 sanctions "dedicated handler functions in `src-tauri/src/weft/`",
/// which since SW-WV-B2 spans more than `sqlite.rs` alone (the `queue.rs` lifecycle
/// handlers own the `wv_queue`/`wv_key_ledger` transitions). The scan excludes the
/// whole module so its own SQL string literals don't self-trip, while still catching
/// a raw `wv_*` write anywhere else in the crate.
fn is_sanctioned_weft_file(path: &Path) -> bool {
    let s = path.to_string_lossy().replace('\\', "/");
    s.contains("/src/weft/") || s.ends_with("src/weft.rs")
}

#[test]
fn only_weft_sqlite_writes_wv_tables() {
    let mut files = Vec::new();
    rust_files(&scan_root(), &mut files);

    let mut violations = Vec::new();
    for file in files {
        if is_sanctioned_weft_file(&file) {
            continue;
        }
        let src = std::fs::read_to_string(&file).expect("read source");
        for (i, line) in src.lines().enumerate() {
            let trimmed = line.trim_start();
            if is_comment(trimmed) || trimmed.starts_with("use ") {
                continue;
            }
            // Squish whitespace so `INSERT   INTO   wv_` still matches, then look
            // for a raw write against a `wv_` table.
            let code: String = strip_line_comment(line)
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            let hits_write = code.contains("INSERTINTOwv_")
                || code.contains("UPDATEwv_")
                || code.contains("DELETEFROMwv_");
            if hits_write {
                violations.push(format!("{}:{}: {}", file.display(), i + 1, line.trim()));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "SW-WV-B1 ONE-WRITER: raw SQL against a `wv_*` table outside \
         `src/weft/sqlite.rs`. Route the write through a `weft::` handler:\n{}",
        violations.join("\n")
    );
}

/// SW-WV-E1 AC-5 / AC-6 boundary: neither the ratify nor the revoke verb may exist
/// anywhere under `src/mcp/`. Ratification is a Tauri command (the human UI
/// boundary), never an MCP verb — so no agent can ratify an agent, or its own
/// assertion, via the MCP path (ARCHITECTURE §4.1 ENDORSEMENT-WALL). The forbidden
/// tokens are CONSTRUCTED at runtime (`"wv_" + "ratify"`) so this assertion file
/// does not itself contain the literal it forbids and cannot self-trip.
#[test]
fn ratify_revoke_are_not_mcp_verbs() {
    let ratify = format!("wv_{}", "ratify");
    let revoke = format!("wv_{}", "revoke");
    let mcp_dir = scan_root().join("mcp");
    let mut files = Vec::new();
    rust_files(&mcp_dir, &mut files);
    assert!(!files.is_empty(), "mcp source files found to scan");
    let mut violations = Vec::new();
    for file in files {
        let src = std::fs::read_to_string(&file).expect("read mcp source");
        if src.contains(&ratify) || src.contains(&revoke) {
            violations.push(file.display().to_string());
        }
    }
    assert!(
        violations.is_empty(),
        "SW-WV-E1 ENDORSEMENT-WALL: ratify/revoke tokens appear under src/mcp/ — \
         ratification must stay a Tauri command, never an MCP verb:\n{}",
        violations.join("\n")
    );
}

#[test]
fn no_weft_store_call_on_ui_thread() {
    const STORE_CALLS: &[&str] = &[
        "WeftStore::open",
        ".conn(",
        "wv_append",
        "wv_auto_append",
        "wv_mutate_owned",
    ];
    const ALLOW_MARKER: &str = "RI-2-ALLOW";

    let mut files = Vec::new();
    rust_files(&scan_root(), &mut files);

    let mut violations = Vec::new();
    for file in files {
        // The weft module + its tests legitimately call the store off any command
        // plane; skip them.
        if is_sanctioned_weft_file(&file) {
            continue;
        }
        let src = std::fs::read_to_string(&file).expect("read source");
        // Two cheap pre-filters, BOTH required. A UI-thread violation needs a
        // Tauri command AND a reference to the weft store — a file that never
        // names `WeftStore`/`weft::` cannot call it, so `.conn(` there is some
        // *other* type's connection, not ours. Gating on the weft mention keeps
        // the generic `.conn(` token from false-positiving across the tree while
        // still catching a real `use crate::weft::WeftStore; …store.conn()` site.
        if !src.contains("#[tauri::command]") {
            continue;
        }
        if !(src.contains("WeftStore") || src.contains("weft::")) {
            continue;
        }
        let lines: Vec<&str> = src.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            if is_comment(trimmed) || trimmed.starts_with("use ") {
                continue;
            }
            let code = strip_line_comment(line);
            let calls_store = STORE_CALLS.iter().any(|c| code.contains(c));
            if !calls_store {
                continue;
            }
            // Allowed if this line, or the line directly above, mints a
            // `spawn_blocking` or carries an explicit RI-2-ALLOW marker. The
            // marker/offload must be adjacent — a code line in between breaks it
            // (feedback rule).
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
        "SW-WV-B1 BLOCKING-POOL: a `WeftStore` call reached a Tauri command file \
         without an adjacent `spawn_blocking` offload or a `// RI-2-ALLOW` marker \
         (the marker must sit on the offload line or the line directly above):\n{}",
        violations.join("\n")
    );
}

// ---------- `role:x` re-key migration on open ----------

/// A read-only connection to `path`: any write through it fails.
fn read_only(path: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

#[test]
fn the_role_rekey_writes_nothing_when_no_row_needs_it() {
    let (dir, store) = open_temp();
    let path = dir.path().join("weft.sqlite3");
    // Rows the migration leaves alone: bare roles, `role:human`, a session row.
    let conn = store.conn().unwrap();
    conn.execute_batch(
        "INSERT INTO wv_inbox (id, sender, recipient, text, created_at) \
           VALUES (1, 'H1', 'builder', 'x', 1), (2, 'H1', 'role:human', 'x', 1), \
                  (3, 'H1', 'role:tab', 'x', 1); \
         INSERT INTO wv_role_delivery (message_id, role, state, created_at) \
           VALUES (1, 'builder', 'pending', 1), (2, 'role:human', 'pending', 1); \
         INSERT INTO wv_role_registration \
           (role, instance_id, generation, registered_at, last_renewed_at, expires_at) \
           VALUES ('builder', 'H1', 1, 1, 1, 99), ('role: ', 'H1', 1, 1, 1, 99);",
    )
    .unwrap();
    drop(conn);
    // Only the read-only check runs: on a read-only connection it succeeds.
    super::rekey_prefixed_role_rows(&read_only(&path)).unwrap();
    // A row that does need re-keying makes it write (and so fail read-only).
    store
        .conn()
        .unwrap()
        .execute(
            "INSERT INTO wv_role_acceptance_receipt \
             (receipt_id, delivery_id, role, message_id, instance_id, generation, kind, created_at) \
             VALUES ('r1', 9, 'role:builder', 9, 'H1', 1, 'work', 1)",
            [],
        )
        .unwrap();
    assert!(super::rekey_prefixed_role_rows(&read_only(&path)).is_err());
    super::reset_cached_store_for_test(&path);
    WeftStore::open(&path).unwrap();
    super::rekey_prefixed_role_rows(&read_only(&path)).unwrap();
}

#[test]
fn a_prefixed_registration_whose_instance_holds_the_bare_role_is_not_stranded() {
    let (dir, store) = open_temp();
    let path = dir.path().join("weft.sqlite3");
    store
        .conn()
        .unwrap()
        .execute_batch(
            "INSERT INTO wv_role_registration \
               (role, instance_id, generation, registered_at, last_renewed_at, expires_at) \
             VALUES ('legacy', 'H1', 2, 1, 1, 99), ('role:legacy', 'H1', 1, 1, 1, 99), \
                    ('role:legacy', 'H2', 1, 1, 1, 99), ('role:human', 'H3', 1, 1, 1, 99);",
        )
        .unwrap();
    drop(store);
    super::reset_cached_store_for_test(&path);
    let store = WeftStore::open(&path).unwrap();
    let rows: Vec<(String, String, i64)> = {
        let conn = store.conn().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT role, instance_id, generation FROM wv_role_registration \
                 ORDER BY role, instance_id",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        rows.collect::<rusqlite::Result<_>>().unwrap()
    };
    assert_eq!(
        rows,
        vec![
            ("legacy".to_string(), "H1".to_string(), 2),
            ("legacy".to_string(), "H2".to_string(), 1),
            ("role:human".to_string(), "H3".to_string(), 1),
        ]
    );
    // Nothing is left for the next open to re-key.
    super::rekey_prefixed_role_rows(&read_only(&path)).unwrap();
}

#[test]
fn a_prefixed_delivery_with_a_bare_twin_does_not_keep_every_open_writing() {
    let (dir, store) = open_temp();
    let path = dir.path().join("weft.sqlite3");
    // One message delivered to both `role:legacy` and `legacy`: the prefixed delivery
    // cannot be re-keyed (`UNIQUE(message_id, role)`).
    store
        .conn()
        .unwrap()
        .execute_batch(
            "INSERT INTO wv_inbox (id, sender, recipient, text, created_at) \
               VALUES (1, 'H1', 'role:legacy', 'x', 1); \
             INSERT INTO wv_role_delivery (message_id, role, state, created_at) \
               VALUES (1, 'role:legacy', 'pending', 1), (1, 'legacy', 'pending', 1);",
        )
        .unwrap();
    drop(store);
    // The inbox row still needs re-keying: the first open writes.
    assert!(super::rekey_prefixed_role_rows(&read_only(&path)).is_err());
    super::reset_cached_store_for_test(&path);
    let store = WeftStore::open(&path).unwrap();
    // The next open only reads.
    super::rekey_prefixed_role_rows(&read_only(&path)).unwrap();
    // Nothing was deleted: both deliveries stand.
    let roles: Vec<String> = {
        let conn = store.conn().unwrap();
        let mut stmt = conn
            .prepare("SELECT role FROM wv_role_delivery ORDER BY role")
            .unwrap();
        let rows = stmt.query_map([], |r| r.get(0)).unwrap();
        rows.collect::<rusqlite::Result<_>>().unwrap()
    };
    assert_eq!(roles, vec!["legacy".to_string(), "role:legacy".to_string()]);
}
