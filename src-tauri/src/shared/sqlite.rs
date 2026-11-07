// SPDX-License-Identifier: MIT
//! Shared SQLite scaffolding for the app's embedded stores.
//!
//! Both the automation coordinator ([`crate::coordinator::sqlite`])
//! and the install-state cache ([`crate::content_engine::sync_state_store`]) are
//! WAL-mode, one-connection-per-operation SQLite stores with the same open and
//! pragma discipline. This module owns that once-reviewed plumbing so the two
//! stores cannot drift on the settings the concurrency contract depends on (WAL,
//! busy-timeout, `synchronous=NORMAL`, foreign keys).
//!
//! The split into two functions is deliberate and load-bearing:
//!
//! - [`open_conn`] applies the per-connection pragmas and is called on EVERY
//!   operation (the conn-per-caller model — each op opens its own connection).
//! - [`open_wal_db`] runs ONCE at store open: it creates the parent dir, opens a
//!   configured connection, and sets + asserts WAL. It returns a live connection
//!   and nothing more. It deliberately owns no schema, so a store's
//!   `execute_batch(SCHEMA)` and any post-open column reconciliation stay the
//!   store's own responsibility on the returned connection — a shared helper
//!   cannot accidentally swallow a store's migration step it never sees.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::Connection;

/// How long a connection waits on a contended write lock before erroring. Kept
/// long enough that a burst of concurrent writers resolves through the lock
/// rather than surfacing spurious `SQLITE_BUSY`, and short enough to still
/// surface a genuine deadlock. Shared by every store so the one reviewed value
/// is set in a single place; safe because all store work runs on the blocking
/// pool, so this wait never blocks the async runtime.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(10);

/// Opening a WAL store failed either while creating the database's parent
/// directory or while configuring the connection. Both embedded stores convert
/// this into their own error type via `From`.
#[derive(Debug)]
pub enum OpenError {
    /// Creating the parent directory, or the filesystem refusing WAL mode.
    Io(std::io::Error),
    /// A SQLite-level failure opening or configuring the connection.
    Sqlite(rusqlite::Error),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Io(e) => write!(f, "sqlite store io error: {e}"),
            OpenError::Sqlite(e) => write!(f, "sqlite store error: {e}"),
        }
    }
}

impl std::error::Error for OpenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            OpenError::Io(e) => Some(e),
            OpenError::Sqlite(e) => Some(e),
        }
    }
}

impl From<std::io::Error> for OpenError {
    fn from(e: std::io::Error) -> Self {
        OpenError::Io(e)
    }
}

impl From<rusqlite::Error> for OpenError {
    fn from(e: rusqlite::Error) -> Self {
        OpenError::Sqlite(e)
    }
}

/// Open a fresh connection to `path` with the standard per-connection pragmas: a
/// busy-timeout so a contended write *waits* for the lock rather than erroring,
/// WAL-safe `synchronous=NORMAL`, and `foreign_keys=ON` (per-connection, off by
/// default in SQLite, so every connection must opt in).
///
/// This does **not** touch schema and does **not** issue `journal_mode`: WAL is a
/// durable file property set once by [`open_wal_db`], and re-issuing the pragma
/// per connection would trigger a checkpoint check on every call.
pub fn open_conn(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.execute_batch("PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;")?;
    Ok(conn)
}

/// One-time store open: create the database's parent directory, open a configured
/// connection, then set and assert WAL journal mode. Returns the live connection;
/// the caller applies its own schema and any column migrations on it.
///
/// WAL is confirmed by reading the mode back: a filesystem that refuses WAL would
/// otherwise silently leave the store in rollback-journal mode and weaken the
/// concurrent-reader guarantee. `memory` is the expected answer for an in-memory
/// database.
pub fn open_wal_db(path: &Path) -> Result<Connection, OpenError> {
    // `Connection::open` creates the file but not its parent dirs, so a first
    // open under a fresh home would fail. Create the parent up front; skip when
    // there is no real parent (a bare filename or `:memory:`).
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let conn = open_conn(path)?;
    let mode: String = conn
        .query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0))?
        .to_ascii_lowercase();
    if mode != "wal" && mode != "memory" {
        return Err(OpenError::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!("sqlite store requires WAL journal mode, filesystem gave '{mode}'"),
        )));
    }
    Ok(conn)
}

/// Add `column` to `table` if it is not already present, an idempotent in-field
/// schema heal for a DB created by an earlier build where `CREATE TABLE IF NOT
/// EXISTS` cannot add a column to the existing table. `table`, `column`, and
/// `decl` are compile-time literals (never user input), so the unavoidable string
/// interpolation into DDL carries no injection risk.
///
/// Shared by every embedded store so the one reviewed heal is defined once. A
/// second process may add the same column between the probe and the `ALTER`;
/// SQLite then errors "duplicate column name", which is benign here (the column
/// now exists) and is swallowed. Any other failure propagates.
pub fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    decl: &str,
) -> rusqlite::Result<()> {
    let existing: Vec<String> = {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    if existing.iter().any(|c| c == column) {
        return Ok(());
    }
    match conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl};")) {
        Ok(()) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(_, Some(msg)))
            if msg.contains("duplicate column name") =>
        {
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Move a corrupt DB aside — never delete it, so the bytes stay forensically
/// recoverable — and clear its stale WAL sidecars so a freshly created store
/// cannot replay a partial write-ahead log left by the corrupt file.
pub(crate) fn quarantine_corrupt_db(path: &Path) -> std::io::Result<()> {
    let quarantine = next_quarantine_path(path);
    std::fs::rename(path, &quarantine)?;
    // Move the `-wal` / `-shm` sidecars alongside the quarantined main file: out of
    // the way so a leftover WAL can't be checkpointed into the new empty DB, and
    // preserved (moved, not deleted) so a partial write-ahead log stays forensically
    // recoverable too. Absent is fine.
    for suffix in ["-wal", "-shm"] {
        match std::fs::rename(
            append_suffix(path, suffix),
            append_suffix(&quarantine, suffix),
        ) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    log::warn!(
        target: "shared::sqlite",
        "quarantined corrupt SQLite DB to {}",
        quarantine.display(),
    );
    Ok(())
}

/// First free `<path>.corrupt-<n>` sibling, probing from 0. A monotonic counter,
/// not a timestamp, so the name is deterministic and testable (and stable in
/// environments where wall-clock time is unavailable).
fn next_quarantine_path(path: &Path) -> PathBuf {
    (0..)
        .map(|n| append_suffix(path, &format!(".corrupt-{n}")))
        .find(|candidate| !candidate.exists())
        .expect("an unbounded counter always finds a free quarantine name")
}

/// Append a raw suffix to a path's full filename (`install.sqlite3` + `-wal` →
/// `install.sqlite3-wal`), the way SQLite names its WAL sidecars — distinct from
/// [`Path::with_extension`], which would replace `.sqlite3`.
pub(crate) fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_conn_enables_foreign_keys() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open_conn(&dir.path().join("t.sqlite3")).unwrap();
        let fk: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1, "foreign_keys must be ON per connection");
    }

    #[test]
    fn open_wal_db_creates_parents_and_asserts_wal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/dir/t.sqlite3");
        let conn = open_wal_db(&path).unwrap();
        assert!(path.exists(), "parent dirs created and file opened");
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_ascii_lowercase(), "wal");
    }

    #[test]
    fn open_wal_db_accepts_in_memory() {
        // `:memory:` has no parent and reports `memory` mode — the branch the
        // file-backed store tests never exercise.
        let conn = open_wal_db(Path::new(":memory:")).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_ascii_lowercase(), "memory");
    }

    fn column_names(conn: &Connection, table: &str) -> Vec<String> {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(1)).unwrap();
        rows.collect::<Result<Vec<_>, _>>().unwrap()
    }

    #[test]
    fn ensure_column_adds_a_missing_column_in_place() {
        let conn = open_wal_db(Path::new(":memory:")).unwrap();
        conn.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY);")
            .unwrap();
        assert!(!column_names(&conn, "t").iter().any(|c| c == "added"));

        ensure_column(&conn, "t", "added", "INTEGER").unwrap();

        assert!(
            column_names(&conn, "t").iter().any(|c| c == "added"),
            "column added to the existing table via ALTER, not only fresh CREATE"
        );
    }

    #[test]
    fn ensure_column_is_idempotent_when_the_column_exists() {
        let conn = open_wal_db(Path::new(":memory:")).unwrap();
        conn.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY, added INTEGER);")
            .unwrap();

        // Two calls, neither errors; the column count is unchanged.
        ensure_column(&conn, "t", "added", "INTEGER").unwrap();
        ensure_column(&conn, "t", "added", "INTEGER").unwrap();

        let added = column_names(&conn, "t")
            .into_iter()
            .filter(|c| c == "added")
            .count();
        assert_eq!(added, 1, "no duplicate column, no error on re-run");
    }
}
