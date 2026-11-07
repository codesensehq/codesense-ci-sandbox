// SPDX-License-Identifier: MIT
//! SV-INP-15: emoji reactions on inbox messages. A reaction is a row of its own, never
//! mail: it is not unread, wakes nobody, and one reactor holds at most one of each emoji
//! on a message. The `UNIQUE(message_id, reactor, emoji)` key is the whole dedupe; a
//! toggle is a delete-or-insert in one transaction.

use rusqlite::{params, OptionalExtension, TransactionBehavior};
use unicode_segmentation::UnicodeSegmentation;

use super::sqlite::{write_audit_public, WvValidation, WvWriteResult};
use super::store::WeftStore;

const REACTIONS: &str = "wv_reactions";

/// The longest accepted emoji, in UTF-8 bytes (a ZWJ family with skin tones fits).
pub const EMOJI_MAX_BYTES: usize = 64;

/// One `wv_reactions` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reaction {
    pub message_id: i64,
    pub reactor: String,
    pub emoji: String,
    pub created_at_ms: i64,
}

/// What a react call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReactOutcome {
    Added,
    Removed,
    /// `remove` asked for a state the row already had.
    Unchanged,
}

/// A new reaction on a message the reader sent, for its next `read_messages`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReactionNotice {
    /// The team whose store holds the message; set by the reader that gathers notices
    /// across teams.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<super::inbox::InboxReplyTeam>,
    pub message_id: i64,
    pub emoji: String,
    pub reactor: String,
    pub created_at_ms: i64,
}

/// One emoji on a message with the distinct reactors that hold it, as a row carries it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReactionGroup {
    pub emoji: String,
    pub count: u32,
    pub reactors: Vec<String>,
}

/// `rows` (one message's reactions, oldest first) grouped by emoji in first-seen order.
pub fn group_reactions(rows: &[&Reaction]) -> Vec<ReactionGroup> {
    let mut groups: Vec<ReactionGroup> = Vec::new();
    for row in rows {
        let group = match groups.iter().position(|g| g.emoji == row.emoji) {
            Some(at) => &mut groups[at],
            None => {
                groups.push(ReactionGroup {
                    emoji: row.emoji.clone(),
                    count: 0,
                    reactors: Vec::new(),
                });
                groups.last_mut().expect("just pushed")
            }
        };
        if !group.reactors.contains(&row.reactor) {
            group.reactors.push(row.reactor.clone());
            group.count += 1;
        }
    }
    groups
}

fn invalid(msg: &str) -> super::sqlite::WvWriteError {
    WvValidation(msg.to_string()).into()
}

/// `emoji` when it is exactly one extended grapheme cluster of at most
/// [`EMOJI_MAX_BYTES`] bytes, with no control or whitespace characters.
pub fn validate_emoji(emoji: &str) -> WvWriteResult<&str> {
    let one_cluster = emoji.graphemes(true).count() == 1;
    let plain = !emoji.chars().any(|c| c.is_control() || c.is_whitespace());
    // A lone ASCII letter or digit is one cluster but not an emoji; keycaps carry U+20E3.
    let pictographic = emoji.chars().any(|c| !c.is_ascii());
    if !(one_cluster && plain && pictographic && emoji.len() <= EMOJI_MAX_BYTES) {
        return Err(invalid(
            "emoji must be exactly one emoji (one character cluster, at most 64 bytes)",
        ));
    }
    Ok(emoji)
}

/// Toggle (`remove` None), add (`Some(false)`) or remove (`Some(true)`) `reactor`'s
/// `emoji` on message `message_id`. Refuses a message this store does not hold.
pub fn reaction_react(
    store: &WeftStore,
    message_id: i64,
    reactor: &str,
    emoji: &str,
    remove: Option<bool>,
) -> WvWriteResult<ReactOutcome> {
    let emoji = validate_emoji(emoji)?;
    let reactor = reactor.trim();
    if reactor.is_empty() {
        return Err(invalid("reactor is blank"));
    }
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let held = tx
        .query_row(
            "SELECT 1 FROM wv_inbox WHERE id = ?1",
            params![message_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !held {
        return Err(invalid(&format!("no message {message_id}")));
    }
    let existing: Option<i64> = tx
        .query_row(
            "SELECT id FROM wv_reactions WHERE message_id = ?1 AND reactor = ?2 AND emoji = ?3",
            params![message_id, reactor, emoji],
            |r| r.get(0),
        )
        .optional()?;
    let outcome = match (existing, remove) {
        (Some(id), None | Some(true)) => {
            tx.execute("DELETE FROM wv_reactions WHERE id = ?1", params![id])?;
            write_audit_public(&tx, REACTIONS, "delete", id)?;
            ReactOutcome::Removed
        }
        (None, None | Some(false)) => {
            tx.execute(
                "INSERT INTO wv_reactions (message_id, reactor, emoji, created_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    message_id,
                    reactor,
                    emoji,
                    chrono::Utc::now().timestamp_millis()
                ],
            )?;
            write_audit_public(&tx, REACTIONS, "append", tx.last_insert_rowid())?;
            ReactOutcome::Added
        }
        _ => ReactOutcome::Unchanged,
    };
    tx.commit()?;
    Ok(outcome)
}

fn row_to_reaction(r: &rusqlite::Row) -> rusqlite::Result<Reaction> {
    Ok(Reaction {
        message_id: r.get(0)?,
        reactor: r.get(1)?,
        emoji: r.get(2)?,
        created_at_ms: r.get(3)?,
    })
}

/// Every reaction on `message_ids`, oldest first.
pub fn reactions_for(store: &WeftStore, message_ids: &[i64]) -> WvWriteResult<Vec<Reaction>> {
    // Well under SQLite's bound-variable limit on any build.
    const CHUNK: usize = 500;
    let conn = store.conn()?;
    let mut found = Vec::new();
    for ids in message_ids.chunks(CHUNK) {
        let marks = vec!["?"; ids.len()].join(",");
        let mut stmt = conn.prepare(&format!(
            "SELECT message_id, reactor, emoji, created_at FROM wv_reactions \
             WHERE message_id IN ({marks}) ORDER BY id"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(ids), row_to_reaction)?;
        for row in rows {
            found.push(row?);
        }
    }
    Ok(found)
}

/// Every reaction in the store, oldest first.
pub fn reactions_all(store: &WeftStore) -> WvWriteResult<Vec<Reaction>> {
    let conn = store.conn()?;
    let mut stmt = conn.prepare_cached(
        "SELECT message_id, reactor, emoji, created_at FROM wv_reactions ORDER BY id",
    )?;
    let rows = stmt
        .query_map([], row_to_reaction)?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

/// Reactions by someone else on messages sent by any of `senders` that were not yet
/// reported, marked reported. Not mail: nothing here is unread or wakes anyone.
pub fn reactions_take_notices(
    store: &WeftStore,
    senders: &[&str],
) -> WvWriteResult<Vec<ReactionNotice>> {
    if senders.is_empty() {
        return Ok(Vec::new());
    }
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut notices = Vec::new();
    {
        let mut stmt = tx.prepare(
            "SELECT r.id, r.message_id, r.emoji, r.reactor, r.created_at, m.sender \
             FROM wv_reactions r JOIN wv_inbox m ON m.id = r.message_id \
             WHERE r.notified_at IS NULL AND r.reactor <> m.sender ORDER BY r.id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    ReactionNotice {
                        team: None,
                        message_id: r.get(1)?,
                        emoji: r.get(2)?,
                        reactor: r.get(3)?,
                        created_at_ms: r.get(4)?,
                    },
                    r.get::<_, String>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let now = chrono::Utc::now().timestamp_millis();
        for (id, notice, sender) in rows {
            if senders.contains(&sender.as_str()) {
                tx.execute(
                    "UPDATE wv_reactions SET notified_at = ?2 WHERE id = ?1",
                    params![id, now],
                )?;
                write_audit_public(&tx, REACTIONS, "mutate", id)?;
                notices.push(notice);
            }
        }
    }
    tx.commit()?;
    Ok(notices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::weft::inbox::{inbox_get, inbox_insert, inbox_unread};

    fn open_temp() -> (tempfile::TempDir, WeftStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
        (dir, store)
    }

    fn message(store: &WeftStore, from: &str, to: &str) -> i64 {
        inbox_insert(store, from, to, "hello", None, None, None, None, None).unwrap()
    }

    #[test]
    fn validate_accepts_one_cluster_of_any_emoji_kind() {
        for e in ["👍", "👍🏽", "👨‍👩‍👧‍👦", "🇳🇴", "1️⃣", "❤️", "🧑🏽‍💻"]
        {
            assert_eq!(validate_emoji(e).unwrap(), e, "{e}");
        }
    }

    #[test]
    fn validate_refuses_text_pairs_blank_controls_and_oversize() {
        for bad in [
            "",
            " ",
            "a",
            "ab",
            "👍👍",
            "👍 ",
            " 👍",
            "👍\n",
            "\u{7}",
            "👍\u{0}",
        ] {
            assert!(validate_emoji(bad).is_err(), "{bad:?}");
        }
        let long = "👨\u{200d}".repeat(40) + "👨";
        assert!(long.len() > EMOJI_MAX_BYTES);
        assert!(validate_emoji(&long).is_err());
    }

    #[test]
    fn groups_count_distinct_reactors_in_first_seen_order() {
        let (_d, store) = open_temp();
        let id = message(&store, "alice", "human");
        for (who, emoji) in [("human", "👍"), ("bob", "❤️"), ("bob", "👍")] {
            reaction_react(&store, id, who, emoji, Some(false)).unwrap();
        }
        let rows = reactions_for(&store, &[id]).unwrap();
        let refs: Vec<&Reaction> = rows.iter().collect();
        let groups = group_reactions(&refs);
        assert_eq!(
            groups,
            vec![
                ReactionGroup {
                    emoji: "👍".into(),
                    count: 2,
                    reactors: vec!["human".into(), "bob".into()]
                },
                ReactionGroup {
                    emoji: "❤️".into(),
                    count: 1,
                    reactors: vec!["bob".into()]
                },
            ]
        );
    }

    #[test]
    fn reactions_for_reads_only_the_asked_messages() {
        let (_d, store) = open_temp();
        let first = message(&store, "alice", "human");
        let second = message(&store, "alice", "human");
        reaction_react(&store, first, "human", "👍", None).unwrap();
        reaction_react(&store, second, "human", "🎉", None).unwrap();
        assert!(reactions_for(&store, &[]).unwrap().is_empty());
        let only_second = reactions_for(&store, &[second, 999]).unwrap();
        assert_eq!(only_second.len(), 1);
        assert_eq!(only_second[0].emoji, "🎉");
        assert_eq!(reactions_for(&store, &[first, second]).unwrap().len(), 2);
        let many: Vec<i64> = (1..=1200).collect();
        assert_eq!(reactions_for(&store, &many).unwrap().len(), 2);
    }

    #[test]
    fn toggle_adds_then_removes() {
        let (_d, store) = open_temp();
        let id = message(&store, "alice", "human");
        assert_eq!(
            reaction_react(&store, id, "human", "👍", None).unwrap(),
            ReactOutcome::Added
        );
        assert_eq!(reactions_for(&store, &[id]).unwrap().len(), 1);
        assert_eq!(
            reaction_react(&store, id, "human", "👍", None).unwrap(),
            ReactOutcome::Removed
        );
        assert!(reactions_for(&store, &[id]).unwrap().is_empty());
    }

    #[test]
    fn one_row_per_reactor_message_and_emoji() {
        let (_d, store) = open_temp();
        let id = message(&store, "alice", "human");
        reaction_react(&store, id, "human", "👍", Some(false)).unwrap();
        assert_eq!(
            reaction_react(&store, id, "human", "👍", Some(false)).unwrap(),
            ReactOutcome::Unchanged
        );
        reaction_react(&store, id, "bob", "👍", Some(false)).unwrap();
        reaction_react(&store, id, "human", "❤️", Some(false)).unwrap();
        let rows = reactions_for(&store, &[id]).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows.iter().filter(|r| r.emoji == "👍").count(), 2);
    }

    #[test]
    fn remove_of_absent_reaction_is_unchanged() {
        let (_d, store) = open_temp();
        let id = message(&store, "alice", "human");
        assert_eq!(
            reaction_react(&store, id, "human", "👍", Some(true)).unwrap(),
            ReactOutcome::Unchanged
        );
    }

    #[test]
    fn unknown_message_and_bad_emoji_are_refused() {
        let (_d, store) = open_temp();
        assert!(reaction_react(&store, 999, "human", "👍", None).is_err());
        let id = message(&store, "alice", "human");
        assert!(reaction_react(&store, id, "human", "ab", None).is_err());
        assert!(reaction_react(&store, id, " ", "👍", None).is_err());
    }

    #[test]
    fn a_reaction_is_not_unread_mail_and_leaves_the_message_alone() {
        let (_d, store) = open_temp();
        let id = message(&store, "alice", "human");
        let before = inbox_get(&store, id).unwrap().unwrap();
        let unread_before = inbox_unread(&store, "human").unwrap();
        reaction_react(&store, id, "human", "👍", None).unwrap();
        reaction_react(&store, id, "alice", "🎉", None).unwrap();
        assert_eq!(inbox_get(&store, id).unwrap().unwrap(), before);
        assert_eq!(inbox_unread(&store, "human").unwrap(), unread_before);
    }

    #[test]
    fn notices_are_reported_once_per_reaction_and_skip_the_senders_own() {
        let (_d, store) = open_temp();
        let id = message(&store, "alice", "human");
        reaction_react(&store, id, "human", "👍", None).unwrap();
        reaction_react(&store, id, "alice", "🎉", None).unwrap();
        let notices = reactions_take_notices(&store, &["alice"]).unwrap();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].reactor, "human");
        assert_eq!(notices[0].emoji, "👍");
        assert_eq!(notices[0].message_id, id);
        assert!(reactions_take_notices(&store, &["alice"])
            .unwrap()
            .is_empty());
        assert!(reactions_take_notices(&store, &["bob"]).unwrap().is_empty());
    }

    #[test]
    fn un_react_withdraws_an_unreported_notice_and_re_react_notifies_again() {
        let (_d, store) = open_temp();
        let id = message(&store, "alice", "human");
        reaction_react(&store, id, "human", "👍", None).unwrap();
        reaction_react(&store, id, "human", "👍", None).unwrap();
        assert!(reactions_take_notices(&store, &["alice"])
            .unwrap()
            .is_empty());
        reaction_react(&store, id, "human", "👍", None).unwrap();
        assert_eq!(reactions_take_notices(&store, &["alice"]).unwrap().len(), 1);
        reaction_react(&store, id, "human", "👍", None).unwrap();
        reaction_react(&store, id, "human", "👍", None).unwrap();
        assert_eq!(reactions_take_notices(&store, &["alice"]).unwrap().len(), 1);
    }
}
