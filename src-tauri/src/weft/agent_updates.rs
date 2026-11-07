// SPDX-License-Identifier: MIT
//! SW-BSI-UX: agent → UI progress notifications surfaced in the Inbox pane's
//! Updates lane. Non-actionable: no reply, no attachment, no archive-ness. One
//! row per `post_agent_message` emit, persisted so the lane survives app
//! restart. The Inbox pane reads the newest N rows (newest first); older rows
//! are implicitly accessible by widening the fetch limit.

use rusqlite::params;

use super::sqlite::{is_blank, write_audit_public, WvValidation, WvWriteResult};
use super::store::WeftStore;

const AGENT_UPDATES: &str = "wv_agent_updates";

/// How many of the newest rows [`agent_updates_list`] returns by default.
pub const AGENT_UPDATES_LIST_DEFAULT_LIMIT: u32 = 50;

/// One `wv_agent_updates` row — a persisted agent → UI notification.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUpdate {
    pub id: i64,
    pub session_id: String,
    pub agent_name: String,
    pub role: String,
    pub text: String,
    pub created_at_ms: i64,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn invalid(msg: &str) -> super::sqlite::WvWriteError {
    WvValidation(msg.to_string()).into()
}

/// Store an agent update. Blank text is refused (matches wv_inbox discipline);
/// everything else is stored verbatim after a secret-redact pass.
pub fn agent_updates_insert(
    store: &WeftStore,
    session_id: &str,
    agent_name: &str,
    role: &str,
    text: &str,
) -> WvWriteResult<i64> {
    if is_blank(text) {
        return Err(invalid("text must not be blank"));
    }
    let text = crate::shared::secret_scan::redact_secrets(text);
    let agent_name = agent_name.trim();
    let role = role.trim();
    let role = if role.is_empty() { "agent" } else { role };
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute(
        "INSERT INTO wv_agent_updates (session_id, agent_name, role, text, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![session_id, agent_name, role, text, now_ms()],
    )?;
    let id = tx.last_insert_rowid();
    write_audit_public(&tx, AGENT_UPDATES, "mutate", id)?;
    tx.commit()?;
    Ok(id)
}

/// The newest [`limit`] updates (clamped), newest first. A view: it marks
/// nothing read. Reading older updates is a widen-limit call, not pagination
/// state.
pub fn agent_updates_list(store: &WeftStore, limit: u32) -> WvWriteResult<Vec<AgentUpdate>> {
    let limit = limit.clamp(1, 500);
    let conn = store.conn()?;
    let mut stmt = conn.prepare(
        "SELECT id, session_id, agent_name, role, text, created_at \
         FROM wv_agent_updates ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit], |r| {
        Ok(AgentUpdate {
            id: r.get(0)?,
            session_id: r.get(1)?,
            agent_name: r.get(2)?,
            role: r.get(3)?,
            text: r.get(4)?,
            created_at_ms: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_temp() -> (tempfile::TempDir, WeftStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
        (dir, store)
    }

    #[test]
    fn insert_and_list_newest_first() {
        let (_dir, store) = open_temp();
        agent_updates_insert(&store, "s1", "code-reviewer", "agent", "started review").unwrap();
        agent_updates_insert(&store, "s1", "code-reviewer", "agent", "finished review").unwrap();
        let rows = agent_updates_list(&store, 10).unwrap();
        assert_eq!(rows.len(), 2);
        // Newest first.
        assert_eq!(rows[0].text, "finished review");
        assert_eq!(rows[1].text, "started review");
    }

    #[test]
    fn blank_text_refused() {
        let (_dir, store) = open_temp();
        assert!(agent_updates_insert(&store, "s1", "a", "agent", "").is_err());
        assert!(agent_updates_insert(&store, "s1", "a", "agent", "   ").is_err());
    }

    #[test]
    fn role_defaults_when_blank() {
        let (_dir, store) = open_temp();
        agent_updates_insert(&store, "s1", "a", "", "hello").unwrap();
        let rows = agent_updates_list(&store, 10).unwrap();
        assert_eq!(rows[0].role, "agent");
    }

    #[test]
    fn updates_survive_reopen() {
        let (dir, store) = open_temp();
        agent_updates_insert(&store, "s1", "a", "agent", "hello").unwrap();
        drop(store);
        let reopened = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
        let rows = agent_updates_list(&reopened, 10).unwrap();
        assert_eq!(rows.len(), 1);
    }
}
