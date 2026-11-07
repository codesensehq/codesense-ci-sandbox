// SPDX-License-Identifier: MIT
//! Durable checkpoint identities, independent of session-record arrival and text retention.
use super::{db_err, UsageStore};
use rusqlite::{params, OptionalExtension};
pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS checkpoint_trace (
 cli TEXT NOT NULL, native_session_id TEXT NOT NULL, trace_id TEXT NOT NULL,
 git_ai_session_id TEXT NOT NULL DEFAULT '',
 tool_use_id TEXT NOT NULL, ambiguous INTEGER NOT NULL DEFAULT 0,
 PRIMARY KEY(cli,native_session_id,trace_id));
CREATE TABLE IF NOT EXISTS checkpoint_log_base (
 repo_key TEXT NOT NULL, base_sha TEXT NOT NULL, PRIMARY KEY(repo_key,base_sha));
CREATE TABLE IF NOT EXISTS checkpoint_log_cursor (
 repo_key TEXT NOT NULL,path TEXT NOT NULL,length INTEGER NOT NULL,modified TEXT NOT NULL,offset INTEGER NOT NULL,
 identity TEXT NOT NULL DEFAULT '',window_hash TEXT NOT NULL DEFAULT '',
 PRIMARY KEY(repo_key,path));";
#[derive(Debug, Clone)]
pub(crate) struct CheckpointRow {
    pub git_ai_session_id: String,
    pub cli: String,
    pub native_session_id: String,
    pub trace_id: String,
    pub tool_use_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckpointLink {
    pub tool_use_id: String,
    pub ambiguous: bool,
}
#[derive(Debug)]
pub(crate) struct CheckpointCursor {
    pub length: u64,
    pub modified: String,
    pub offset: u64,
    pub identity: String,
    pub window_hash: String,
}

pub(super) fn init(tx: &rusqlite::Transaction<'_>) -> Result<(), String> {
    tx.execute_batch(SCHEMA).map_err(db_err)?;
    if !super::has_column(tx, "checkpoint_trace", "git_ai_session_id")? {
        tx.execute_batch("ALTER TABLE checkpoint_trace ADD COLUMN git_ai_session_id TEXT NOT NULL DEFAULT ''; DELETE FROM checkpoint_log_cursor;").map_err(db_err)?;
    }
    tx.execute_batch(
        "CREATE INDEX IF NOT EXISTS checkpoint_session_key ON checkpoint_trace(git_ai_session_id,cli);",
    )
    .map_err(db_err)?;
    for column in ["identity", "window_hash"] {
        if !super::has_column(tx, "checkpoint_log_cursor", column)? {
            tx.execute_batch(&format!(
                "ALTER TABLE checkpoint_log_cursor ADD COLUMN {column} TEXT NOT NULL DEFAULT '';"
            ))
            .map_err(db_err)?;
        }
    }
    Ok(())
}

impl UsageStore {
    /// Preserve coverage even when the source log has already been pruned. Absolute cursor paths
    /// remain separate for different clones of the same remote; an existing destination wins.
    pub(crate) fn migrate_checkpoint_keys(
        &mut self,
        previous: &std::collections::BTreeSet<String>,
        key: &str,
    ) -> Result<(), String> {
        let tx = self.conn.transaction().map_err(db_err)?;
        for old in previous
            .iter()
            .filter(|old| std::string::String::as_str(old) != key)
        {
            tx.execute("INSERT OR IGNORE INTO checkpoint_log_base(repo_key,base_sha) SELECT ?2,base_sha FROM checkpoint_log_base WHERE repo_key=?1", params![old,key]).map_err(db_err)?;
            tx.execute("INSERT OR IGNORE INTO checkpoint_log_cursor(repo_key,path,length,modified,offset,identity,window_hash) SELECT ?2,path,length,modified,offset,identity,window_hash FROM checkpoint_log_cursor WHERE repo_key=?1", params![old,key]).map_err(db_err)?;
            tx.execute("DELETE FROM checkpoint_log_base WHERE repo_key=?1", [old])
                .map_err(db_err)?;
            tx.execute("DELETE FROM checkpoint_log_cursor WHERE repo_key=?1", [old])
                .map_err(db_err)?;
        }
        rusqlite::Transaction::commit(tx).map_err(db_err)
    }
    pub(crate) fn checkpoint_native_session(
        &self,
        cli: Option<&str>,
        id: &str,
    ) -> Result<Option<String>, String> {
        self.conn.query_row("SELECT MIN(native_session_id) FROM checkpoint_trace WHERE (?1 IS NULL OR cli=?1) AND git_ai_session_id=?2 AND native_session_id<>git_ai_session_id HAVING COUNT(DISTINCT native_session_id)=1", params![cli,id], |r|r.get(0)).optional().map_err(db_err)
    }
    pub(crate) fn checkpoint_cursor(
        &self,
        repo: &str,
        path: &str,
    ) -> Result<Option<CheckpointCursor>, String> {
        self.conn.query_row("SELECT length,modified,offset,identity,window_hash FROM checkpoint_log_cursor WHERE repo_key=?1 AND path=?2",params![repo,path],|r|Ok(CheckpointCursor{length:r.get(0)?,modified:r.get(1)?,offset:r.get(2)?,identity:r.get(3)?,window_hash:r.get(4)?})).optional().map_err(db_err)
    }
    #[cfg(test)]
    pub(crate) fn store_checkpoint_rows(
        &mut self,
        repo: &str,
        base: Option<&str>,
        rows: &[CheckpointRow],
    ) -> Result<(), String> {
        self.checkpoint_batch(repo, base, rows, None)
    }
    pub(crate) fn store_checkpoint_file(
        &mut self,
        repo: &str,
        base: Option<&str>,
        rows: &[CheckpointRow],
        path: &str,
        cursor: &CheckpointCursor,
    ) -> Result<(), String> {
        self.checkpoint_batch(repo, base, rows, Some((path, cursor)))
    }
    /// Cursor and identities commit together; replay is idempotent, ambiguity and coverage never reset.
    fn checkpoint_batch(
        &mut self,
        repo: &str,
        base: Option<&str>,
        rows: &[CheckpointRow],
        cursor: Option<(&str, &CheckpointCursor)>,
    ) -> Result<(), String> {
        let tx = self.conn.transaction().map_err(db_err)?;
        for row in rows {
            tx.execute("INSERT INTO checkpoint_trace(cli,native_session_id,trace_id,tool_use_id,git_ai_session_id) VALUES(?1,?2,?3,?4,?5)
                ON CONFLICT(cli,native_session_id,trace_id) DO UPDATE SET git_ai_session_id=excluded.git_ai_session_id, ambiguous=checkpoint_trace.ambiguous OR checkpoint_trace.tool_use_id<>excluded.tool_use_id",
                params![row.cli,row.native_session_id,row.trace_id,row.tool_use_id,row.git_ai_session_id]).map_err(db_err)?;
        }
        if let Some(base) = base {
            tx.execute(
                "INSERT OR IGNORE INTO checkpoint_log_base(repo_key,base_sha) VALUES(?1,?2)",
                params![repo, base],
            )
            .map_err(db_err)?;
        }
        if let Some((path, cursor)) = cursor {
            tx.execute("INSERT INTO checkpoint_log_cursor(repo_key,path,length,modified,offset,identity,window_hash) VALUES(?1,?2,?3,?4,?5,?6,?7)
                ON CONFLICT(repo_key,path) DO UPDATE SET length=excluded.length,modified=excluded.modified,offset=excluded.offset,identity=excluded.identity,window_hash=excluded.window_hash",
                params![repo,path,cursor.length,cursor.modified,cursor.offset,cursor.identity,cursor.window_hash]).map_err(db_err)?;
        }
        tx.commit().map_err(db_err)
    }
    pub(crate) fn checkpoint_link(
        &self,
        cli: &str,
        native: &str,
        trace: &str,
    ) -> Result<Option<CheckpointLink>, String> {
        self.conn.query_row("SELECT tool_use_id,ambiguous FROM checkpoint_trace WHERE cli=?1 AND native_session_id=?2 AND trace_id=?3",
            params![cli,native,trace],|r| Ok(CheckpointLink {tool_use_id:r.get(0)?,ambiguous:r.get(1)?})).optional().map_err(db_err)
    }
    pub(crate) fn checkpoint_base_seen(&self, repo: &str, base: &str) -> Result<bool, String> {
        self.conn.query_row("SELECT EXISTS(SELECT 1 FROM checkpoint_log_base WHERE repo_key=?1 AND base_sha=?2)",params![repo,base],|r|r.get(0)).map_err(db_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attribution::git_ai::tests::{git, rebuild};
    const KEY: &str = "github.com/acme/api";
    fn harvest(
        repo: &std::path::Path,
        key: &str,
        store: &mut UsageStore,
    ) -> Result<crate::attribution::checkpoints::Stats, String> {
        let head = git(repo, &["rev-parse", "HEAD"]);
        crate::attribution::checkpoints::harvest(repo, key, store, &[head.trim().to_owned()].into())
    }
    #[test]
    fn old_checkpoint_store_replays_cursor_and_backfills_session_mapping_once() {
        let repo = rebuild();
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("usage.sqlite");
        let mut store = UsageStore::open(&path).unwrap();
        let head = git(repo.path(), &["rev-parse", "HEAD"]);
        let captured =
            include_str!("../attribution/testdata/git_ai_1_7_5_claude_session/checkpoints.jsonl");
        let log = repo
            .path()
            .join(".git/ai/working_logs")
            .join(head.trim())
            .join("checkpoints.jsonl");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::write(log, captured).unwrap();
        harvest(repo.path(), KEY, &mut store).unwrap();
        // Recreate the previous schema and its already-complete cursor.
        store.conn.execute_batch("CREATE TABLE checkpoint_trace_old (cli TEXT NOT NULL,native_session_id TEXT NOT NULL,trace_id TEXT NOT NULL,tool_use_id TEXT NOT NULL,ambiguous INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(cli,native_session_id,trace_id)); INSERT INTO checkpoint_trace_old SELECT cli,native_session_id,trace_id,tool_use_id,ambiguous FROM checkpoint_trace; DROP TABLE checkpoint_trace; ALTER TABLE checkpoint_trace_old RENAME TO checkpoint_trace;").unwrap();
        drop(store);
        let mut store = UsageStore::open(&path).unwrap();
        let count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM checkpoint_trace", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
        assert_eq!(harvest(repo.path(), KEY, &mut store).unwrap().records, 2);
        let count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM checkpoint_trace WHERE git_ai_session_id='s_5cef34e3292d7d'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
        drop(store);
        let mut store = UsageStore::open(&path).unwrap();
        assert_eq!(harvest(repo.path(), KEY, &mut store).unwrap().bytes_read, 0);
        assert_eq!(
            store
                .checkpoint_link(
                    "claude",
                    "c1a0de00-0000-4000-a000-000000000101",
                    "t_68582e0b58f78b"
                )
                .unwrap()
                .unwrap()
                .tool_use_id,
            "toolu_01CDE55Write"
        );
    }
    #[test]
    fn sweep_transaction_rolls_back_cursor_rows_and_retries() {
        let repo = rebuild();
        let home = tempfile::tempdir().unwrap();
        let mut store = UsageStore::open(&home.path().join("usage.sqlite")).unwrap();
        let head = git(repo.path(), &["rev-parse", "HEAD"]);
        let log = repo
            .path()
            .join(".git/ai/working_logs")
            .join(head.trim())
            .join("checkpoints.jsonl");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::write(
            &log,
            include_str!("../attribution/testdata/git_ai_1_7_5_checkpoints/live-claude.jsonl"),
        )
        .unwrap();
        store.conn.execute_batch("CREATE TRIGGER interrupt_cursor BEFORE INSERT ON checkpoint_log_cursor BEGIN SELECT RAISE(ABORT, 'interrupted'); END;").unwrap();
        assert!(
            crate::attribution::checkpoints::harvest_all(repo.path(), KEY, &mut store).is_err()
        );
        for table in [
            "checkpoint_trace",
            "checkpoint_log_base",
            "checkpoint_log_cursor",
        ] {
            let count: i64 = store
                .conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0);
        }
        store
            .conn
            .execute_batch("DROP TRIGGER interrupt_cursor")
            .unwrap();
        assert_eq!(
            crate::attribution::checkpoints::harvest_all(repo.path(), KEY, &mut store)
                .unwrap()
                .records,
            2
        );
    }
}
