// SPDX-License-Identifier: MIT
//! What the usage store keeps about a session's stored conversation: one row for its state and
//! size (`session_conversation`) and one row per tool call (`session_tool_call`). The
//! conversation text itself lives in a file beside `run-artifacts/`; these rows are what stays when
//! that file is removed, so a tool-call history outlives the text.
//!
//! Both tables are additive (no schema version bump), like `session_record`. The index holds names,
//! ids and times only: never a tool's input or output, and every string passes `redact_secrets`.

use std::collections::BTreeMap;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::{db_err, has_column, UsageStore};
use crate::shared::secret_scan::redact_secrets;

/// How long a stored conversation is kept after its session ends, unless a policy says otherwise.
pub const DEFAULT_RETENTION_DAYS: i64 = 90;
/// The most one session's stored conversation may hold, in bytes, unless a policy says otherwise.
pub const DEFAULT_MAX_BYTES: usize = 20 * 1024 * 1024;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS session_conversation (
    session_id     TEXT PRIMARY KEY,
    state          TEXT NOT NULL,
    source_format  TEXT,
    bytes          INTEGER NOT NULL DEFAULT 0,
    turn_count     INTEGER NOT NULL DEFAULT 0,
    truncated      TEXT,
    captured_at_ms INTEGER NOT NULL,
    unavailable    TEXT NOT NULL DEFAULT '{}'
);
CREATE TABLE IF NOT EXISTS session_tool_call (
    session_id TEXT NOT NULL,
    turn_index INTEGER NOT NULL,
    seq        INTEGER NOT NULL,
    native_id  TEXT,
    name       TEXT NOT NULL,
    mcp_server TEXT,
    is_error   INTEGER,
    ts_ms      INTEGER,
    skill      TEXT,
    aliases    TEXT,
    PRIMARY KEY (session_id, turn_index, seq)
);
CREATE INDEX IF NOT EXISTS session_tool_call_by_native
    ON session_tool_call (session_id, native_id) WHERE native_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS session_tool_call_by_skill
    ON session_tool_call (session_id, skill) WHERE skill IS NOT NULL;
";

/// Create the conversation tables, and add to an older `session_record` the columns that say where
/// a session's native log is and which skills it started with.
pub(super) fn init(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(SCHEMA).map_err(db_err)?;
    for column in ["config_home", "provided_skills"] {
        if !has_column(conn, "session_record", column)? {
            conn.execute_batch(&format!(
                "ALTER TABLE session_record ADD COLUMN {column} TEXT;"
            ))
            .map_err(db_err)?;
        }
    }
    if !has_column(conn, "session_tool_call", "aliases")? {
        conn.execute_batch("ALTER TABLE session_tool_call ADD COLUMN aliases TEXT;")
            .map_err(db_err)?;
    }
    Ok(())
}

/// Where a stored conversation stands. A session with no row at all is `absent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Capture could not obtain its session record or native ID; the reason is durable.
    Unavailable,
    /// Stored whole.
    Present,
    /// Stored, but a size limit cut it (see [`Truncation`]).
    Truncated,
    /// The text was removed after its retention ran out; the tool-call index stays.
    Expired,
    /// The session's CLI has no reader, so no conversation was ever stored.
    Unsupported,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Unavailable => "unavailable",
            State::Present => "present",
            State::Truncated => "truncated",
            State::Expired => "expired",
            State::Unsupported => "unsupported",
        }
    }

    fn parse(text: &str) -> Option<State> {
        Some(match text {
            "unavailable" => State::Unavailable,
            "present" => State::Present,
            "truncated" => State::Truncated,
            "expired" => State::Expired,
            "unsupported" => State::Unsupported,
            _ => return None,
        })
    }
}

/// What a size limit dropped from a stored conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Truncation {
    /// The first and last turn index dropped (inclusive); `None` when no whole turn was dropped.
    pub dropped_turns: Option<(u32, u32)>,
    pub dropped_turn_count: u32,
    /// Bytes of the CLI's record left out: dropped turns plus any head of the log not read.
    pub dropped_bytes: u64,
    /// Tool inputs, results and texts cut short to fit one turn in the limit.
    #[serde(default)]
    pub trimmed_fields: u32,
}

/// One session's stored conversation, as the store knows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMeta {
    pub session_id: String,
    pub state: State,
    pub source_format: Option<String>,
    pub bytes: u64,
    pub turn_count: u32,
    pub truncated: Option<Truncation>,
    pub captured_at_ms: i64,
    /// `{field: reason}` for each fact the CLI's record does not hold.
    pub unavailable: BTreeMap<String, String>,
}

/// One tool call in the index: names and ids only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedCall {
    pub turn_index: u32,
    pub seq: u32,
    pub native_id: Option<String>,
    pub name: String,
    pub mcp_server: Option<String>,
    pub is_error: Option<bool>,
    pub ts_ms: Option<i64>,
    pub skill: Option<String>,
    /// Other ids that resolve to this call (Codex file-change item ids); see [`UsageStore::find_tool_call`].
    #[serde(default)]
    pub aliases: Vec<String>,
}

/// Aliases are stored newline-joined in one nullable column; ids never contain a newline.
const ALIAS_SEPARATOR: char = '\n';

const CALL_COLUMNS: &str =
    "turn_index, seq, native_id, name, mcp_server, is_error, ts_ms, skill, aliases";

fn indexed_call_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<IndexedCall> {
    Ok(IndexedCall {
        turn_index: r.get(0)?, seq: r.get(1)?, native_id: r.get(2)?, name: r.get(3)?,
        mcp_server: r.get(4)?, is_error: r.get(5)?, ts_ms: r.get(6)?, skill: r.get(7)?,
        aliases: r
            .get::<_, Option<String>>(8)?
            .map(|joined| Iterator::collect(joined.split(ALIAS_SEPARATOR).map(str::to_owned)))
            .unwrap_or_default(),
    })
}

fn clean(value: &Option<String>) -> Option<String> {
    value.as_deref().map(redact_secrets)
}

impl UsageStore {
    /// Replace a session's conversation row and its whole tool-call index in one transaction. A
    /// capture rebuilds the index from the native log, so the same log written twice, at a
    /// checkpoint and again at the end, leaves exactly the same rows.
    pub fn write_conversation(
        &mut self,
        meta: &ConversationMeta,
        calls: &[IndexedCall],
    ) -> Result<(), String> {
        let tx = self.conn.transaction().map_err(db_err)?;
        let unavailable = serde_json::to_string(
            &meta
                .unavailable
                .iter()
                .map(|(k, v)| (k.clone(), redact_secrets(v)))
                .collect::<BTreeMap<_, _>>(),
        )
        .unwrap_or_else(|_| "{}".into());
        let truncated = meta
            .truncated
            .as_ref()
            .and_then(|t| serde_json::to_string(t).ok());
        tx.execute(
            "INSERT INTO session_conversation
                (session_id, state, source_format, bytes, turn_count, truncated, captured_at_ms, unavailable)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (session_id) DO UPDATE SET
                state = excluded.state, source_format = excluded.source_format, bytes = excluded.bytes,
                turn_count = excluded.turn_count, truncated = excluded.truncated,
                captured_at_ms = excluded.captured_at_ms, unavailable = excluded.unavailable",
            params![
                meta.session_id,
                meta.state.as_str(),
                clean(&meta.source_format),
                meta.bytes as i64,
                meta.turn_count,
                truncated,
                meta.captured_at_ms,
                unavailable,
            ],
        )
        .map_err(db_err)?;
        tx.execute(
            "DELETE FROM session_tool_call WHERE session_id = ?1",
            params![meta.session_id],
        )
        .map_err(db_err)?;
        {
            let mut insert = tx
                .prepare(
                    "INSERT OR REPLACE INTO session_tool_call
                        (session_id, turn_index, seq, native_id, name, mcp_server, is_error, ts_ms, skill, aliases)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                )
                .map_err(db_err)?;
            for c in calls {
                insert
                    .execute(params![
                        meta.session_id,
                        c.turn_index,
                        c.seq,
                        clean(&c.native_id),
                        redact_secrets(&c.name),
                        clean(&c.mcp_server),
                        c.is_error,
                        c.ts_ms,
                        clean(&c.skill),
                        (!Vec::is_empty(&c.aliases)).then(|| {
                            c.aliases
                                .iter()
                                .map(|a| redact_secrets(a))
                                .collect::<Vec<_>>()
                                .join(&ALIAS_SEPARATOR.to_string())
                        }),
                    ])
                    .map_err(db_err)?;
            }
        }
        tx.commit().map_err(db_err)
    }

    /// A session's conversation row; `None` when nothing was ever stored for it.
    pub fn read_conversation_meta(
        &self,
        session_id: &str,
    ) -> Result<Option<ConversationMeta>, String> {
        self.conn
            .query_row(
                "SELECT session_id, state, source_format, bytes, turn_count, truncated, captured_at_ms, unavailable
                 FROM session_conversation WHERE session_id = ?1",
                params![session_id],
                |r| {
                    let state: String = r.get(1)?;
                    let truncated: Option<String> = r.get(5)?;
                    let unavailable: String = r.get(7)?;
                    Ok(ConversationMeta {
                        session_id: r.get(0)?,
                        state: State::parse(&state).unwrap_or(State::Present),
                        source_format: r.get(2)?,
                        bytes: r.get::<_, i64>(3)?.max(0) as u64,
                        turn_count: r.get(4)?,
                        truncated: truncated.and_then(|t| serde_json::from_str(&t).ok()),
                        captured_at_ms: r.get(6)?,
                        unavailable: serde_json::from_str(&unavailable).unwrap_or_default(),
                    })
                },
            )
            .optional()
            .map_err(db_err)
    }

    /// The sessions whose stored conversation is past its retention at `cutoff_ms`: it ended before
    /// then, or (with no recorded end) was last captured before then.
    pub fn expirable_conversations(&self, cutoff_ms: i64) -> Result<Vec<String>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT c.session_id FROM session_conversation c
                 LEFT JOIN session_end e ON e.session_id = c.session_id
                 WHERE c.state IN ('present', 'truncated')
                   AND COALESCE(e.ended_at_ms, c.captured_at_ms) < ?1",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![cutoff_ms], |r| r.get(0))
            .map_err(db_err)?;
        rows.collect::<Result<Vec<String>, _>>().map_err(db_err)
    }

    /// Mark a session's conversation expired. Its tool-call index stays.
    pub fn mark_conversation_expired(&self, session_id: &str) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE session_conversation SET state = 'expired', bytes = 0 WHERE session_id = ?1",
                params![session_id],
            )
            .map(|_| ())
            .map_err(db_err)
    }

    /// How many tool calls a session's index holds.
    pub fn count_tool_calls(&self, session_id: &str) -> Result<u32, String> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM session_tool_call WHERE session_id = ?1",
                params![session_id],
                |r| r.get(0),
            )
            .map_err(db_err)
    }

    /// The index row of the tool call the CLI knows as `native_id`, else the call that lists it as
    /// an alias (a Codex `exec-…` file-change item id resolves to its enclosing `call_…`).
    pub fn find_tool_call(
        &self,
        session_id: &str,
        native_id: &str,
    ) -> Result<Option<IndexedCall>, String> {
        let exact = self.conn.query_row(
            &format!("SELECT {CALL_COLUMNS} FROM session_tool_call WHERE session_id = ?1 AND native_id = ?2
             ORDER BY turn_index, seq LIMIT 1"),
            params![session_id, native_id], indexed_call_row,
        ).optional().map_err(db_err)?;
        if exact.is_some() {
            return Ok(exact);
        }
        self.conn.query_row(
            &format!("SELECT {CALL_COLUMNS} FROM session_tool_call
             WHERE session_id = ?1 AND aliases IS NOT NULL
               AND instr(char(10) || aliases || char(10), char(10) || ?2 || char(10)) > 0
             ORDER BY turn_index, seq LIMIT 1"),
            params![session_id, native_id], indexed_call_row,
        ).optional().map_err(db_err)
    }

    /// A session's tool calls, in conversation order.
    pub fn read_tool_calls(&self, session_id: &str) -> Result<Vec<IndexedCall>, String> {
        let mut stmt = self
            .conn
            .prepare(
                &format!("SELECT {CALL_COLUMNS}
                 FROM session_tool_call WHERE session_id = ?1 ORDER BY turn_index, seq"),
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![session_id], indexed_call_row)
            .map_err(db_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_usage::tests::store;

    fn meta(session: &str) -> ConversationMeta {
        ConversationMeta {
            session_id: session.into(),
            state: State::Present,
            source_format: Some("anthropic-session-jsonl-v1".into()),
            bytes: 2048,
            turn_count: 3,
            truncated: None,
            captured_at_ms: 1_000,
            unavailable: [("turn.cost".to_owned(), "none in the record".to_owned())].into(),
        }
    }

    fn call(turn: u32, seq: u32, name: &str) -> IndexedCall {
        IndexedCall {
            turn_index: turn,
            seq,
            native_id: Some(format!("toolu_{turn}_{seq}")),
            name: name.into(),
            mcp_server: None,
            is_error: Some(false),
            ts_ms: Some(1_700_000_000_000),
            skill: None,
            aliases: Vec::new(),
        }
    }

    #[test]
    fn a_conversation_and_its_index_survive_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        let mut m = meta("s1");
        m.state = State::Truncated;
        m.truncated = Some(Truncation {
            dropped_turns: Some((1, 4)),
            dropped_turn_count: 4,
            dropped_bytes: 900,
            trimmed_fields: 0,
        });
        let calls = vec![call(0, 0, "Read"), call(2, 0, "Bash"), call(2, 1, "Skill")];
        UsageStore::open(&path)
            .unwrap()
            .write_conversation(&m, &calls)
            .unwrap();
        let store = UsageStore::open(&path).unwrap();
        assert_eq!(store.read_conversation_meta("s1").unwrap(), Some(m));
        assert_eq!(store.read_tool_calls("s1").unwrap(), calls);
        assert_eq!(store.read_conversation_meta("other").unwrap(), None);
    }

    #[test]
    fn writing_the_same_capture_twice_leaves_one_row_per_call() {
        let (_d, mut store) = store();
        let calls = vec![call(0, 0, "Read"), call(1, 0, "Bash")];
        store.write_conversation(&meta("s1"), &calls).unwrap();
        store.write_conversation(&meta("s1"), &calls).unwrap();
        assert_eq!(store.read_tool_calls("s1").unwrap(), calls);
        // A later checkpoint that sees more of the log replaces the index, not appends to it.
        let more = vec![call(0, 0, "Read"), call(1, 0, "Bash"), call(2, 0, "Edit")];
        store.write_conversation(&meta("s1"), &more).unwrap();
        assert_eq!(store.read_tool_calls("s1").unwrap(), more);
    }

    /// Fails on main: `find_tool_call` matches `native_id` only, so an alias finds no call.
    #[test]
    fn an_alias_resolves_to_its_enclosing_call_and_never_over_an_exact_id() {
        let (_d, mut store) = store();
        let mut enclosing = call(4, 1, "exec");
        enclosing.native_id = Some("call_a".into());
        enclosing.aliases = vec!["exec-1".into(), "exec-2".into()];
        let mut other = call(6, 0, "exec");
        other.native_id = Some("exec-2".into());
        store
            .write_conversation(&meta("s1"), &[enclosing.clone(), other.clone()])
            .unwrap();
        assert_eq!(store.read_tool_calls("s1").unwrap(), [enclosing.clone(), other.clone()]);
        assert_eq!(store.find_tool_call("s1", "exec-1").unwrap(), Some(enclosing));
        assert_eq!(store.find_tool_call("s1", "exec-2").unwrap(), Some(other));
        assert_eq!(store.find_tool_call("s1", "exec").unwrap(), None);
        assert_eq!(store.find_tool_call("other", "exec-1").unwrap(), None);
    }

    /// Fails on main (no `aliases` column to add): a store from before aliases opens, gains the
    /// column, and its old rows still resolve by `native_id`.
    #[test]
    fn a_pre_alias_store_gains_the_aliases_column_and_old_rows_still_resolve() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE session_tool_call (
                    session_id TEXT NOT NULL, turn_index INTEGER NOT NULL, seq INTEGER NOT NULL,
                    native_id TEXT, name TEXT NOT NULL, mcp_server TEXT, is_error INTEGER,
                    ts_ms INTEGER, skill TEXT,
                    PRIMARY KEY (session_id, turn_index, seq)
                );
                INSERT INTO session_tool_call (session_id, turn_index, seq, native_id, name, ts_ms)
                    VALUES ('old', 2, 0, 'toolu_old', 'Bash', 1700000000000);",
            )
            .unwrap();
            assert!(!has_column(&conn, "session_tool_call", "aliases").unwrap());
        }
        let store = UsageStore::open(&path).unwrap();
        assert!(has_column(&store.conn, "session_tool_call", "aliases").unwrap());
        let found = store.find_tool_call("old", "toolu_old").unwrap().unwrap();
        assert_eq!((found.turn_index, found.name.as_str()), (2, "Bash"));
        assert!(found.aliases.is_empty());
        assert_eq!(store.find_tool_call("old", "exec-1").unwrap(), None);
    }

    #[test]
    fn another_sessions_index_is_untouched_by_a_rewrite() {
        let (_d, mut store) = store();
        store
            .write_conversation(&meta("a"), &[call(0, 0, "Read")])
            .unwrap();
        store
            .write_conversation(&meta("b"), &[call(0, 0, "Bash")])
            .unwrap();
        store.write_conversation(&meta("b"), &[]).unwrap();
        assert_eq!(store.read_tool_calls("a").unwrap().len(), 1);
        assert!(store.read_tool_calls("b").unwrap().is_empty());
    }

    #[test]
    fn the_index_holds_names_and_ids_and_no_credential() {
        let (_d, mut store) = store();
        let secret = format!("ghp_{}", "a1B2c3D4e5".repeat(4));
        let mut c = call(0, 0, &format!("tool {secret}"));
        c.native_id = Some(format!("id-{secret}"));
        c.skill = Some(format!("skill {secret}"));
        let mut m = meta("s1");
        m.unavailable.insert("x".into(), format!("failed {secret}"));
        store.write_conversation(&m, &[c]).unwrap();
        let dump: String = store
            .conn
            .query_row(
                "SELECT json_object('t', (SELECT json_group_array(json_array(native_id, name, skill)) FROM session_tool_call),
                                    'c', (SELECT unavailable FROM session_conversation))",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!dump.contains(&secret), "{dump}");
        let columns: Vec<String> = store
            .conn
            .prepare("SELECT name FROM pragma_table_info('session_tool_call')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        for forbidden in ["input", "output", "result", "arguments", "text"] {
            assert!(!columns.iter().any(|c| c == forbidden), "{forbidden}");
        }
    }

    #[test]
    fn the_tables_are_added_without_changing_the_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        {
            let store = UsageStore::open(&path).unwrap();
            store
                .conn
                .execute_batch("DROP TABLE session_conversation; DROP TABLE session_tool_call;")
                .unwrap();
        }
        let store = UsageStore::open(&path).unwrap();
        assert!(has_column(&store.conn, "session_conversation", "state").unwrap());
        assert!(has_column(&store.conn, "session_tool_call", "native_id").unwrap());
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 5, "an older build must still open this store");
    }

    #[test]
    fn a_store_from_before_the_record_columns_gets_them_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        {
            let store = UsageStore::open(&path).unwrap();
            // The record table as the previous build created it.
            store
                .conn
                .execute_batch(
                    "DROP TABLE session_record;
                     CREATE TABLE session_record (
                        session_id TEXT PRIMARY KEY, native_session_id TEXT, org TEXT, team TEXT,
                        handle TEXT, launched_by TEXT, agent TEXT, manifest_hash TEXT, cli TEXT,
                        cli_version TEXT, model TEXT, mcp_allow TEXT, bridge_tools TEXT, account TEXT,
                        story_id TEXT, workspace TEXT, worktree TEXT, branch TEXT, run_id TEXT,
                        unavailable TEXT NOT NULL DEFAULT '{}', recorded_at_ms INTEGER NOT NULL);
                     INSERT INTO session_record (session_id, recorded_at_ms) VALUES ('old', 1);",
                )
                .unwrap();
        }
        let store = UsageStore::open(&path).unwrap();
        assert!(has_column(&store.conn, "session_record", "config_home").unwrap());
        assert!(has_column(&store.conn, "session_record", "provided_skills").unwrap());
        let old = store.read_session_record("old").unwrap().unwrap();
        assert_eq!((old.config_home, old.provided_skills), (None, None));
    }

    #[test]
    fn a_record_keeps_where_its_cli_home_is_and_which_skills_it_started_with() {
        let (_d, store) = store();
        let mut r = crate::session_usage::record::SessionRecord::new("s1");
        r.config_home = Some("/accounts/work/.claude".into());
        r.provided_skills = Some(vec!["bmad-help".into(), "review".into()]);
        store.write_session_record(&r).unwrap();
        let back = store.read_session_record("s1").unwrap().unwrap();
        assert_eq!(back.config_home.as_deref(), Some("/accounts/work/.claude"));
        assert_eq!(
            back.provided_skills,
            Some(vec!["bmad-help".to_owned(), "review".to_owned()])
        );
        assert!(!back.unavailable.contains_key("configHome"));
        let bare = crate::session_usage::record::SessionRecord::new("s2");
        store.write_session_record(&bare).unwrap();
        let bare = store.read_session_record("s2").unwrap().unwrap();
        assert_eq!(bare.config_home, None);
    }
}
