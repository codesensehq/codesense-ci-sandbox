// SPDX-License-Identifier: MIT
//! SW-BSI-UX: the human's online/away status for the team's inbox. One row per
//! team (team is implicit in the store the row lives in). Agents read this via
//! MCP to adjust behavior — "don't ping at 3am", "batch for morning", "escalate
//! differently when out for the night". Default (no row) is `online` with no
//! note; writes replace the single row.

use rusqlite::{params, OptionalExtension};

use super::sqlite::{write_audit_public, WvValidation, WvWriteResult};
use super::store::WeftStore;

const HUMAN_STATUS: &str = "wv_human_status";

/// Closed set of statuses. Any string outside this is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HumanStatus {
    Online,
    Away,
    Busy,
    Offline,
}

impl HumanStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            HumanStatus::Online => "online",
            HumanStatus::Away => "away",
            HumanStatus::Busy => "busy",
            HumanStatus::Offline => "offline",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "online" => Some(HumanStatus::Online),
            "away" => Some(HumanStatus::Away),
            "busy" => Some(HumanStatus::Busy),
            "offline" => Some(HumanStatus::Offline),
            _ => None,
        }
    }
}

/// The team's current human-status row.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HumanStatusView {
    pub status: HumanStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub updated_at_ms: i64,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn invalid(msg: &str) -> super::sqlite::WvWriteError {
    WvValidation(msg.to_string()).into()
}

/// Default status when no row exists: `online` with no note and `updated_at = 0`
/// so a caller can distinguish "never set" (0) from "set to online recently".
pub fn human_status_default() -> HumanStatusView {
    HumanStatusView {
        status: HumanStatus::Online,
        note: None,
        updated_at_ms: 0,
    }
}

/// Replace the singleton status row. A note longer than 200 chars is refused —
/// the chip surface is tight.
pub fn human_status_set(
    store: &WeftStore,
    status: HumanStatus,
    note: Option<&str>,
) -> WvWriteResult<HumanStatusView> {
    let note = note.map(|s| s.trim()).filter(|s| !s.is_empty());
    if let Some(n) = note.as_ref() {
        if n.chars().count() > 200 {
            return Err(invalid("note must be at most 200 characters"));
        }
    }
    let note_stored = note.map(crate::shared::secret_scan::redact_secrets);
    let now = now_ms();
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute(
        "INSERT INTO wv_human_status (id, status, note, updated_at) VALUES (1, ?1, ?2, ?3) \
         ON CONFLICT(id) DO UPDATE SET status = excluded.status, note = excluded.note, updated_at = excluded.updated_at",
        params![status.as_str(), note_stored.as_deref(), now],
    )?;
    write_audit_public(&tx, HUMAN_STATUS, "mutate", 1)?;
    tx.commit()?;
    Ok(HumanStatusView {
        status,
        note: note_stored,
        updated_at_ms: now,
    })
}

/// Read the current status; default when no row exists.
pub fn human_status_get(store: &WeftStore) -> WvWriteResult<HumanStatusView> {
    let conn = store.conn()?;
    let row: Option<(String, Option<String>, i64)> = conn
        .query_row(
            "SELECT status, note, updated_at FROM wv_human_status WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    match row {
        None => Ok(human_status_default()),
        Some((status, note, updated_at_ms)) => Ok(HumanStatusView {
            status: HumanStatus::parse(&status).unwrap_or(HumanStatus::Online),
            note: note.filter(|s| !s.is_empty()),
            updated_at_ms,
        }),
    }
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
    fn default_is_online_when_no_row() {
        let (_dir, store) = open_temp();
        let view = human_status_get(&store).unwrap();
        assert_eq!(view.status, HumanStatus::Online);
        assert_eq!(view.note, None);
        assert_eq!(view.updated_at_ms, 0);
    }

    #[test]
    fn set_then_get_roundtrips() {
        let (_dir, store) = open_temp();
        human_status_set(&store, HumanStatus::Away, Some("back in a bit")).unwrap();
        let view = human_status_get(&store).unwrap();
        assert_eq!(view.status, HumanStatus::Away);
        assert_eq!(view.note.as_deref(), Some("back in a bit"));
        assert!(view.updated_at_ms > 0);
    }

    #[test]
    fn set_replaces_prior_row() {
        let (_dir, store) = open_temp();
        human_status_set(&store, HumanStatus::Busy, Some("heads down")).unwrap();
        human_status_set(&store, HumanStatus::Online, None).unwrap();
        let view = human_status_get(&store).unwrap();
        assert_eq!(view.status, HumanStatus::Online);
        assert_eq!(view.note, None);
    }

    #[test]
    fn status_survives_reopen() {
        let (dir, store) = open_temp();
        human_status_set(&store, HumanStatus::Offline, Some("out for the night")).unwrap();
        drop(store);
        let reopened = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
        let view = human_status_get(&reopened).unwrap();
        assert_eq!(view.status, HumanStatus::Offline);
    }

    #[test]
    fn empty_note_normalizes_to_none() {
        let (_dir, store) = open_temp();
        human_status_set(&store, HumanStatus::Online, Some("")).unwrap();
        let view = human_status_get(&store).unwrap();
        assert_eq!(view.note, None);
    }

    #[test]
    fn long_note_refused() {
        let (_dir, store) = open_temp();
        let long = "x".repeat(201);
        assert!(human_status_set(&store, HumanStatus::Online, Some(&long)).is_err());
    }
}
