// SPDX-License-Identifier: MIT
//! SW-WV-B4 acceptance tests for coord (durable, append-only, one owner per
//! contract, peers read-only).
//!
//! Covers AC-6 (b) coord owner enforcement, (c) coord peer-lane bound, (d) coord
//! non-deletability (a compile-time grep). Every store test
//! uses a fresh on-disk store via `WeftStore::open` (WAL needs a real path; the
//! store is process-local, no network, no Tauri `AppHandle`, no spawned process —
//! AC-6).

use std::path::{Path, PathBuf};

// `super::*` brings the B4 `pub` handlers (`wv_signal_insert`,
// `wv_coord_write`, `wv_coord_read`), the `pub` types (`CoordRefusal`,
// `WvWriteError`, `WvWriteResult`), and the `params!` macro re-export into scope
// from `sqlite.rs`. `WeftStore` is imported into `sqlite.rs` via a private `use`
// (not re-exported by the glob), so it is named explicitly — matching `b5_tests`.
use super::*;
use crate::weft::store::WeftStore;

fn open_temp() -> (tempfile::TempDir, WeftStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
    (dir, store)
}

/// Count of live `wv_signals` rows.
fn signal_count(store: &WeftStore) -> i64 {
    let conn = store.conn().unwrap();
    conn.query_row("SELECT COUNT(*) FROM wv_signals", [], |r| r.get(0))
        .unwrap()
}

/// The current `body` stored for a coord contract (test-only direct read).
fn coord_body(store: &WeftStore, contract: &str) -> Option<String> {
    let conn = store.conn().unwrap();
    conn.query_row(
        "SELECT body FROM wv_coord WHERE contract=?1",
        params![contract],
        |r| r.get::<_, String>(0),
    )
    .ok()
}

/// Assert a write result is the given [`CoordRefusal`] — mapped through
/// [`WvWriteError::Invalid`] with the refusal's canonical message.
fn assert_refusal<T: std::fmt::Debug>(res: WvWriteResult<T>, refusal: CoordRefusal) {
    match res {
        Err(WvWriteError::Invalid(v)) => {
            assert_eq!(
                v.0,
                WvWriteError::from(refusal).to_string(),
                "refusal message"
            );
        }
        other => panic!("expected {refusal:?}, got {other:?}"),
    }
}

// ======================= AC-6 (b): coord owner enforcement =======================

#[test]
fn coord_write_non_owner_rejected() {
    let (_d, store) = open_temp();
    // Worker A creates the contract.
    wv_coord_write(&store, "relay/handoff", "dispatch-a", "alice", "A's body").unwrap();
    assert_eq!(
        coord_body(&store, "relay/handoff").as_deref(),
        Some("A's body")
    );

    // Worker B attempts to write the same contract — refused with NotOwner.
    let res = wv_coord_write(&store, "relay/handoff", "dispatch-b", "bob", "B's body");
    assert_refusal(res, CoordRefusal::NotOwner);

    // The row body is unchanged (a peer never overwrites a contract it does not own).
    assert_eq!(
        coord_body(&store, "relay/handoff").as_deref(),
        Some("A's body"),
        "a rejected non-owner write leaves the row untouched"
    );

    // The owner CAN update its own row (append-only-by-owner).
    wv_coord_write(
        &store,
        "relay/handoff",
        "dispatch-a",
        "alice",
        "A's new body",
    )
    .unwrap();
    assert_eq!(
        coord_body(&store, "relay/handoff").as_deref(),
        Some("A's new body")
    );
}

// ======================= AC-6 (c): coord peer-lane bound =======================

#[test]
fn coord_read_unlisted_peer_rejected() {
    let (_d, store) = open_temp();
    wv_coord_write(
        &store,
        "relay/handoff",
        "dispatch-a",
        "alice",
        "shared state",
    )
    .unwrap();

    // A listed peer reads the body.
    let listed = wv_coord_read(
        &store,
        "relay/handoff",
        "bob",
        &["bob".to_string(), "carol".to_string()],
    )
    .unwrap();
    assert_eq!(listed.as_deref(), Some("shared state"));

    // A worker NOT listed in its dispatch's peers is refused with NotAuthorized —
    // and no data is returned.
    let res = wv_coord_read(
        &store,
        "relay/handoff",
        "mallory",
        &["bob".to_string(), "carol".to_string()],
    );
    assert_refusal(res, CoordRefusal::NotAuthorized);

    // A listed peer reading a non-existent contract gets None (no delivery semantics).
    let missing = wv_coord_read(&store, "no/such", "bob", &["bob".to_string()]).unwrap();
    assert!(missing.is_none());
}

// ==================== AC-6 (d): coord rows are never deleted ====================

/// The whole `src/weft/` module tree scanned for a raw `DELETE FROM wv_coord`.
/// `WeftStore` exposes no public method that issues one; this is the compile-time
/// grep that proves no call site under `src/weft/` deletes a coord row (AC-2/AC-6).
/// The forbidden token is CONSTRUCTED at runtime so this test file does not itself
/// contain the literal it forbids (mirrors the `ratify_revoke_are_not_mcp_verbs`
/// technique in `tests.rs`).
#[test]
fn coord_rows_are_never_deleted() {
    let forbidden = format!("DELETE FROM wv_{}", "coord");
    // Also match the whitespace-squished form so `DELETE  FROM  wv_coord` still trips.
    let forbidden_squished: String = forbidden.chars().filter(|c| !c.is_whitespace()).collect();

    let weft_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/weft");
    let mut files = Vec::new();
    collect_rs(&weft_dir, &mut files);
    assert!(!files.is_empty(), "weft source files found to scan");

    let this_file = Path::new(file!())
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("signals_coord_tests.rs")
        .to_string();

    let mut violations = Vec::new();
    for file in &files {
        // Skip this test file — it constructs the token at runtime but a defensive
        // name-skip keeps the intent obvious.
        if file
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n == this_file)
        {
            continue;
        }
        let src = std::fs::read_to_string(file).expect("read weft source");
        for line in src.lines() {
            let trimmed = line.trim_start();
            // Skip comment/doc lines and `use` lines — a mention there is not a call.
            if trimmed.starts_with("//") || trimmed.starts_with('*') || trimmed.starts_with("/*") {
                continue;
            }
            let code = strip_trailing_comment(line);
            let squished: String = code.chars().filter(|c| !c.is_whitespace()).collect();
            if code.contains(&forbidden) || squished.contains(&forbidden_squished) {
                violations.push(format!("{}: {}", file.display(), line.trim()));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "SW-WV-B4: coord rows are append-only and never deleted — a raw \
         `DELETE FROM wv_coord` appears under src/weft/:\n{}",
        violations.join("\n")
    );
}

/// Recursively collect `.rs` files under `dir` (mirrors `tests::rust_files`; kept
/// local so this module has no cross-test-module dependency).
fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
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
            collect_rs(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Return the code portion of a line, dropping any `// …` trailing comment (string
/// literals aside). Mirrors `tests::strip_line_comment`.
fn strip_trailing_comment(line: &str) -> &str {
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

// ============ Extra: insert validation belt + audit-atomicity coverage ============

#[test]
fn signal_insert_rejects_out_of_enum_status() {
    let (_d, store) = open_temp();
    let res = wv_signal_insert(&store, "alice", "t1", "sess", "done", None, None);
    match res {
        Err(WvWriteError::Invalid(_)) => {}
        other => panic!("expected an invalid-status refusal, got {other:?}"),
    }
    assert_eq!(signal_count(&store), 0, "a rejected insert writes no row");
}

#[test]
fn coord_write_co_commits_an_audit_row() {
    let (_d, store) = open_temp();
    wv_coord_write(&store, "c1", "d1", "alice", "body").unwrap();

    let conn = store.conn().unwrap();
    let coord_audits: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wv_write_log WHERE tbl='wv_coord' AND operation='coord_write'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        coord_audits, 1,
        "coord_write co-commits exactly one audit row"
    );
}
