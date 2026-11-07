// SPDX-License-Identifier: MIT
//! SW-WV-D4 store-level acceptance tests for the bounded worker-to-worker peer read
//! lane: the `wv_queue.dispatch_peers` migration (AC-1), the peer-authorisation
//! lookup, the peer-cwd + scope reads that back `wv_peer_read_file` / `wv_escalate`,
//! and the escalate insert (AC-4). The MCP-handler layer (`require_live_session`,
//! `session_id → worker` resolution, path-traversal guard) is covered by the
//! `mcp/handler.rs` unit tests; these exercise the store contract every verb calls.
//!
//! Every test uses a fresh on-disk store via `WeftStore::open` (WAL needs a real
//! path; the store is process-local — no network, no Tauri `AppHandle`, no spawned
//! process) plus `tempfile::tempdir()` for the peer worktree.

use super::*;
use crate::weft::store::WeftStore;

fn open_temp() -> (tempfile::TempDir, WeftStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
    (dir, store)
}

fn column_names(store: &WeftStore, table: &str) -> Vec<String> {
    let conn = store.conn().unwrap();
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    let rows = stmt.query_map([], |r| r.get::<_, String>(1)).unwrap();
    rows.collect::<Result<Vec<_>, _>>().unwrap()
}

// ============================ AC-1: dispatch_peers column ============================

#[test]
fn fresh_db_has_dispatch_peers_column() {
    let (_dir, store) = open_temp();
    assert!(
        column_names(&store, "wv_queue")
            .iter()
            .any(|c| c == "dispatch_peers"),
        "a fresh DB carries the dispatch_peers column"
    );
    // The column accepts both NULL and a JSON array value.
    let mut conn = store.conn().unwrap();
    let peers_json = serde_json::to_string(&vec!["impl-1", "review-1"]).unwrap();
    let id_with = wv_append(
        &mut conn,
        "wv_queue",
        "INSERT INTO wv_queue (id, dispatch_peers) VALUES (?1, ?2)",
        &[&peers_json],
    )
    .unwrap();
    let id_null = wv_append(
        &mut conn,
        "wv_queue",
        "INSERT INTO wv_queue (id, dispatch_peers) VALUES (?1, NULL)",
        &[],
    )
    .unwrap();
    let read_json: Option<String> = conn
        .query_row(
            "SELECT dispatch_peers FROM wv_queue WHERE id=?1",
            params![id_with],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(read_json.as_deref(), Some(peers_json.as_str()));
    let read_null: Option<String> = conn
        .query_row(
            "SELECT dispatch_peers FROM wv_queue WHERE id=?1",
            params![id_null],
            |r| r.get(0),
        )
        .unwrap();
    assert!(read_null.is_none(), "NULL peers = closed lane");
}

#[test]
fn upgraded_db_gains_dispatch_peers_without_dropping_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("weft.sqlite3");

    // Simulate a pre-D4 DB: a `wv_queue` shaped WITHOUT the dispatch_peers column,
    // carrying a live row.
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE wv_queue (\
                id INTEGER PRIMARY KEY, source TEXT, state TEXT DEFAULT 'pending', \
                claimed_by TEXT, scope TEXT, created INTEGER DEFAULT 0, \
                updated INTEGER DEFAULT 0, refs TEXT, note TEXT, dedupe_key TEXT);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO wv_queue (id, source, state, note) VALUES (7, 'triage', 'pending', 'legacy row')",
            [],
        )
        .unwrap();
    }

    // Open through the store's migration path — the column is added, the row survives.
    let store = WeftStore::open(&path).unwrap();
    assert!(
        column_names(&store, "wv_queue")
            .iter()
            .any(|c| c == "dispatch_peers"),
        "the migration adds dispatch_peers to a pre-existing DB"
    );
    let conn = store.conn().unwrap();
    let note: String = conn
        .query_row("SELECT note FROM wv_queue WHERE id=7", [], |r| r.get(0))
        .unwrap();
    assert_eq!(note, "legacy row", "existing rows are not dropped");
    let peers: Option<String> = conn
        .query_row("SELECT dispatch_peers FROM wv_queue WHERE id=7", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(
        peers.is_none(),
        "the migrated column reads NULL for old rows"
    );
}

// ============================ dispatch_peers round-trip ============================

#[test]
fn enqueue_round_trips_dispatch_peers() {
    let (_dir, store) = open_temp();
    let peers_json = serde_json::to_string(&vec!["impl-1"]).unwrap();
    let id = crate::weft::queue::enqueue(
        &store,
        "controller",
        Some("root"),
        None,
        Some("dispatched work"),
        None,
        Some(peers_json.as_str()),
        1,
    )
    .unwrap();
    let conn = store.conn().unwrap();
    let stored: Option<String> = conn
        .query_row(
            "SELECT dispatch_peers FROM wv_queue WHERE id=?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored.as_deref(), Some(peers_json.as_str()));
}

// ============================ peer-authorisation lookup ============================

/// Seed a dispatched row owned by the controller's verified session. The worker
/// is correlated through dispatch_handle, independently of claim ownership.
fn seed_claimed_row(store: &WeftStore, worker: &str, scope: &str, peers: Option<&[&str]>) {
    let peers_json = peers.map(|p| serde_json::to_string(&p).unwrap());
    let mut conn = store.conn().unwrap();
    wv_append(
        &mut conn,
        "wv_queue",
        "INSERT INTO wv_queue (id, source, state, claimed_by, dispatch_handle, scope, dispatch_peers) \
         VALUES (?1, 'controller', 'dispatched', 'controller-session', ?2, ?3, ?4)",
        &[&worker, &scope, &peers_json],
    )
    .unwrap();
}

#[test]
fn dispatch_peers_follow_dispatched_worker_not_claim_owner() {
    let (_dir, store) = open_temp();
    seed_claimed_row(&store, "worker-B", "root", Some(&["worker-A"]));
    let peers = wv_dispatch_peers_for_worker(&store, "worker-B");
    assert_eq!(peers, vec!["worker-A".to_string()]);
    assert!(wv_dispatch_peers_for_worker(&store, "controller-session").is_empty());
}

#[test]
fn dispatch_peers_empty_when_no_claimed_row_or_null() {
    let (_dir, store) = open_temp();
    // No row at all.
    assert!(wv_dispatch_peers_for_worker(&store, "ghost").is_empty());
    // A claimed row with NULL peers — the lane is closed.
    seed_claimed_row(&store, "worker-C", "root", None);
    assert!(
        wv_dispatch_peers_for_worker(&store, "worker-C").is_empty(),
        "NULL dispatch_peers = closed lane, never an error"
    );
}

// ============================ AC-2: wv_coord_read peer bound ============================

#[test]
fn coord_read_authorised_by_dispatch_peers() {
    let (_dir, store) = open_temp();
    // worker-A owns a contract.
    wv_coord_write(
        &store,
        "plan/api",
        "dispatch-1",
        "worker-A",
        "the shared plan",
    )
    .unwrap();
    // B4's read bound checks the calling worker appears in the supplied peers list
    // (the MCP verb passes the caller's own dispatch_peers). worker-B present →
    // authorised; the body is returned.
    let peers = vec!["worker-B".to_string(), "worker-A".to_string()];
    let body = wv_coord_read(&store, "plan/api", "worker-B", &peers).unwrap();
    assert_eq!(body.as_deref(), Some("the shared plan"));
}

#[test]
fn coord_read_refused_when_caller_not_listed() {
    let (_dir, store) = open_temp();
    wv_coord_write(
        &store,
        "plan/api",
        "dispatch-1",
        "worker-A",
        "the shared plan",
    )
    .unwrap();
    // worker-C is not in the supplied peers list — refused before any SQL.
    let peers = vec!["worker-A".to_string(), "worker-B".to_string()];
    let res = wv_coord_read(&store, "plan/api", "worker-C", &peers);
    assert!(
        matches!(res, Err(crate::weft::WvWriteError::Invalid(_))),
        "a caller not in dispatch_peers is refused (not-authorized)"
    );
}

#[test]
fn coord_read_authorised_caller_missing_contract_returns_none() {
    let (_dir, store) = open_temp();
    let peers = vec!["worker-B".to_string()];
    let missing = wv_coord_read(&store, "no/such", "worker-B", &peers).unwrap();
    assert!(
        missing.is_none(),
        "a missing contract reads None (not-found)"
    );
}

// ============================ AC-3 backing: peer cwd read ============================

#[test]
fn roster_cwd_for_worker_reads_and_misses() {
    let (dir, store) = open_temp();
    let cwd = dir.path().join("peer-worktree");
    std::fs::create_dir_all(&cwd).unwrap();
    crate::weft::wv_upsert_roster(
        &store,
        &crate::weft::WvRosterRow {
            name: "worker-A".to_string(),
            workspace: None,
            pane: None,
            kind: None,
            subclass: Some("impl".to_string()),
            lifecycle: "task".to_string(),
            owned: None,
            orch_owner: None,
            relation: "peer".to_string(),
            parent: None,
            current_task: None,
            cwd: Some(cwd.to_string_lossy().into_owned()),
            status: Some("live".to_string()),
            session_id: None,
            run_id: None,
        },
    )
    .unwrap();
    let found = wv_roster_cwd_for_worker(&store, "worker-A").unwrap();
    assert_eq!(found, Some(cwd));
    let missing = wv_roster_cwd_for_worker(&store, "nobody").unwrap();
    assert!(
        missing.is_none(),
        "an unknown peer reads None (peer-not-found)"
    );
}

// ============================ AC-4: scope read + escalate insert ============================

#[test]
fn queue_scope_follows_dispatched_worker_not_claim_owner() {
    let (_dir, store) = open_temp();
    seed_claimed_row(&store, "worker-B", "epic-x", Some(&["worker-A"]));
    let scope = wv_queue_scope_for_worker(&store, "worker-B").unwrap();
    assert_eq!(scope, "epic-x");
    // No claimed row → an Err the handler maps to the default "root".
    assert!(wv_queue_scope_for_worker(&store, "ghost").is_err());
}

#[test]
fn escalate_insert_appends_pending_agent_row() {
    let (_dir, store) = open_temp();
    // Seed the caller's own claimed row — the escalate must leave it untouched.
    seed_claimed_row(&store, "worker-B", "epic-x", Some(&["worker-A"]));

    let refs = serde_json::json!({
        "invokedBy": "worker-B",
        "involvedWorkers": ["worker-A"],
    })
    .to_string();
    let new_id = wv_escalate_insert(&store, "epic-x", "conflicting file edits", &refs, 42).unwrap();

    let conn = store.conn().unwrap();
    let (source, state, claimed_by, scope, note): (String, String, String, String, String) = conn
        .query_row(
            "SELECT source, state, claimed_by, scope, note FROM wv_queue WHERE id=?1",
            params![new_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(source, "agent");
    assert_eq!(state, "pending");
    assert_eq!(claimed_by, "", "escalation row is unclaimed");
    assert_eq!(scope, "epic-x");
    assert_eq!(note, "conflicting file edits");

    // The caller's own claimed row is unchanged.
    let (b_state, b_scope): (String, String) = conn
        .query_row(
            "SELECT state, scope FROM wv_queue WHERE dispatch_handle='worker-B'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(b_state, "dispatched", "the caller's row is untouched");
    assert_eq!(b_scope, "epic-x");
}

// ============================ AC-6: full peer-lane round trip ============================

#[test]
fn peer_lane_full_round_trip() {
    let (dir, store) = open_temp();

    // Two workers: A (owner) with a temp worktree, B (peer/caller).
    let a_cwd = dir.path().join("worker-a");
    std::fs::create_dir_all(&a_cwd).unwrap();
    let roster = |name: &str, cwd: Option<&std::path::Path>| crate::weft::WvRosterRow {
        name: name.to_string(),
        workspace: None,
        pane: None,
        kind: None,
        subclass: Some("impl".to_string()),
        lifecycle: "task".to_string(),
        owned: None,
        orch_owner: None,
        relation: "peer".to_string(),
        parent: None,
        current_task: None,
        cwd: cwd.map(|p| p.to_string_lossy().into_owned()),
        status: Some("live".to_string()),
        session_id: None,
        run_id: None,
    };
    crate::weft::wv_upsert_roster(&store, &roster("worker-A", Some(&a_cwd))).unwrap();
    crate::weft::wv_upsert_roster(&store, &roster("worker-B", None)).unwrap();

    // A publishes a coord contract.
    wv_coord_write(
        &store,
        "plan/api",
        "dispatch-A",
        "worker-A",
        "the plan body",
    )
    .unwrap();

    // B is dispatched with A listed as a peer.
    seed_claimed_row(&store, "worker-B", "root", Some(&["worker-A"]));

    // wv_coord_read as B (its dispatch lists A, and B itself is in the passed list).
    let peers = wv_dispatch_peers_for_worker(&store, "worker-B");
    assert_eq!(peers, vec!["worker-A".to_string()]);
    // The MCP verb passes the caller's own dispatch_peers as the B4 bound list; model
    // the handler's call shape (the caller resolves its own peers + itself is the
    // calling_worker). B4's read bound checks calling_worker ∈ peers, so include B.
    let read_peers = vec!["worker-A".to_string(), "worker-B".to_string()];
    let body = wv_coord_read(&store, "plan/api", "worker-B", &read_peers).unwrap();
    assert_eq!(body.as_deref(), Some("the plan body"));

    // A file in A's worktree, readable through the peer cwd resolution.
    std::fs::write(a_cwd.join("notes.md"), "hello from A").unwrap();
    let a_root = wv_roster_cwd_for_worker(&store, "worker-A")
        .unwrap()
        .unwrap();
    let content = std::fs::read_to_string(a_root.join("notes.md")).unwrap();
    assert_eq!(content, "hello from A");

    // B escalates a cross-worker decision — a new pending agent row appears.
    let refs =
        serde_json::json!({ "invokedBy": "worker-B", "involvedWorkers": ["worker-A"] }).to_string();
    let esc_id = wv_escalate_insert(&store, "root", "who owns api.rs?", &refs, 7).unwrap();
    let conn = store.conn().unwrap();
    let (src, st): (String, String) = conn
        .query_row(
            "SELECT source, state FROM wv_queue WHERE id=?1",
            params![esc_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((src.as_str(), st.as_str()), ("agent", "pending"));
}
