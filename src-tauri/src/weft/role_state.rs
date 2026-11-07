// SPDX-License-Identifier: MIT
//! A read of the role mailboxes: who holds each role and where every delivery to it
//! stands. A view over `wv_role_registration`, `wv_role_delivery` and
//! `wv_role_acceptance_receipt`; it claims and changes nothing.
//!
//! The role tables store epoch seconds; this view reports epoch milliseconds, like the
//! inbox.

use rusqlite::{params, Connection};

use super::role_inbox::AcceptKind;
use super::sqlite::WvWriteResult;
use super::store::WeftStore;

/// How many acknowledged deliveries per role a read returns by default. Open ones
/// (pending, claimed, accepted) are always all returned.
pub const ROLE_STATE_DONE_DEFAULT_LIMIT: u32 = 20;

/// The most open deliveries one role reports, so a runaway queue cannot blow up a read.
const OPEN_DELIVERY_CAP: u32 = 500;

/// How much of a message's text a delivery carries.
const EXCERPT_CHARS: usize = 160;

/// Where a delivery to a role stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RoleDeliveryState {
    /// Waiting: no instance holds it.
    Pending,
    /// An instance claimed it and is deciding (its claim lapses at `claimExpiresAtMs`).
    Claimed,
    /// An instance accepted it, as work or as no work, and has not acknowledged it yet.
    Accepted,
    /// The accepting instance acknowledged it: finished.
    Acknowledged,
}

impl RoleDeliveryState {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "claimed" => Some(Self::Claimed),
            "accepted" => Some(Self::Accepted),
            "acknowledged" => Some(Self::Acknowledged),
            _ => None,
        }
    }
}

/// One instance registered for a role.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleRegistrationView {
    /// The id the instance registered with (usually its session id).
    pub instance_id: String,
    /// The instance's session display name, when it is a known session.
    pub instance_name: Option<String>,
    /// The fence token the instance passes to every claim, accept and ack.
    pub generation: i64,
    pub registered_at_ms: i64,
    pub last_renewed_at_ms: i64,
    pub expires_at_ms: i64,
    /// Still registered now (`expiresAtMs` is in the future). A lapsed registration
    /// holds nothing until it registers again.
    pub live: bool,
    /// The tab session whose life renews it, when it registered with one (its
    /// `instance_id` can then be any id the agent chose).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// The acceptance receipt of a delivery.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleReceiptView {
    pub receipt_id: String,
    /// The instance that accepted it.
    pub instance_id: String,
    pub generation: i64,
    /// `work` (the acceptor committed to doing the work and acknowledging it) or
    /// `no_work` (accepted with a reason, no action taken).
    pub kind: AcceptKind,
    pub note: Option<String>,
    /// Deprecated: always null for new receipts; role work is not mirrored into
    /// wv_queue. Kept for wire compatibility.
    pub queue_item_id: Option<i64>,
    pub accepted_at_ms: i64,
}

/// One message delivered to a role.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleDeliveryView {
    pub delivery_id: i64,
    /// The inbox message (`inbox_list` row `messageId`).
    pub message_id: i64,
    pub state: RoleDeliveryState,
    /// Who sent the message: a session handle, or `human`.
    pub from: String,
    /// The sender's display name.
    pub from_name: Option<String>,
    /// The start of the message text.
    pub excerpt: String,
    pub created_at_ms: i64,
    /// When an instance first claimed it (kept if that claim later lapsed).
    pub first_claimed_at_ms: Option<i64>,
    /// The instance holding (or having held) its claim.
    pub claim_owner: Option<String>,
    pub claim_owner_name: Option<String>,
    pub claim_owner_generation: Option<i64>,
    pub claim_expires_at_ms: Option<i64>,
    /// Claimed, but the claim lapsed: another instance can claim it again.
    pub claim_lapsed: bool,
    /// Its acceptance, once accepted.
    pub receipt: Option<RoleReceiptView>,
}

/// How many deliveries of a role are in each state (over all of them, not only the
/// listed ones).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleDeliveryCounts {
    pub pending: u32,
    pub claimed: u32,
    pub accepted: u32,
    pub acknowledged: u32,
}

/// One role's mailbox.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleQueueView {
    /// The role id: any role name, usually a saved Agent's id.
    pub role: String,
    /// The team-configured display name; absent for an unconfigured queue.
    pub role_name: Option<String>,
    pub registrations: Vec<RoleRegistrationView>,
    pub counts: RoleDeliveryCounts,
    /// Every open delivery (pending, claimed, accepted), oldest first, then the newest
    /// acknowledged ones, newest first.
    pub deliveries: Vec<RoleDeliveryView>,
    /// Some acknowledged (or, past 500, open) deliveries were left out.
    pub deliveries_truncated: bool,
}

/// The mailbox of every role that has a registration or a delivery (or only `role`),
/// as of `now_secs`, with at most `done_limit` acknowledged deliveries each. Names are
/// left empty for the caller to fill.
pub fn role_state_read(
    store: &WeftStore,
    role: Option<&str>,
    now_secs: i64,
    done_limit: u32,
) -> WvWriteResult<Vec<RoleQueueView>> {
    let conn = store.conn()?;
    // `role:x` (the Reply-To spelling) reads the queue `x`, as every write keys it.
    let roles: Vec<String> = match role
        .map(super::role_inbox::role_name)
        .filter(|r| !r.is_empty())
    {
        Some(role) => vec![role.to_string()],
        None => {
            let mut stmt = conn.prepare(
                "SELECT role FROM wv_role_registration UNION SELECT role FROM wv_role_delivery \
                 ORDER BY role",
            )?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<Vec<String>>>()?
        }
    };
    // A queue no write can reach (`human`, or `role:human` an older build stored
    // before `human` was refused) can never be claimed or cleared, so it is not
    // listed; its rows stay put, untouched.
    roles
        .into_iter()
        .filter(|role| super::role_inbox::stored_role(role).is_ok())
        .map(|role| role_queue(&conn, role, now_secs, done_limit))
        .collect()
}

fn role_queue(
    conn: &Connection,
    role: String,
    now_secs: i64,
    done_limit: u32,
) -> WvWriteResult<RoleQueueView> {
    let registrations = {
        let mut stmt = conn.prepare_cached(
            "SELECT instance_id, generation, registered_at, last_renewed_at, expires_at, session_id \
             FROM wv_role_registration WHERE role=?1 ORDER BY registered_at, instance_id",
        )?;
        let rows = stmt.query_map(params![role], |r| {
            let expires_at: i64 = r.get(4)?;
            Ok(RoleRegistrationView {
                instance_id: r.get(0)?,
                instance_name: None,
                generation: r.get(1)?,
                registered_at_ms: r.get::<_, i64>(2)? * 1000,
                last_renewed_at_ms: r.get::<_, i64>(3)? * 1000,
                expires_at_ms: expires_at * 1000,
                live: expires_at > now_secs,
                session_id: r
                    .get::<_, Option<String>>(5)?
                    .filter(|s| !s.trim().is_empty()),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    let mut counts = RoleDeliveryCounts::default();
    {
        let mut stmt = conn.prepare_cached(
            "SELECT state, COUNT(*) FROM wv_role_delivery WHERE role=?1 GROUP BY state",
        )?;
        let rows = stmt.query_map(params![role], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?))
        })?;
        for row in rows {
            let (state, n) = row?;
            match RoleDeliveryState::parse(&state) {
                Some(RoleDeliveryState::Pending) => counts.pending = n,
                Some(RoleDeliveryState::Claimed) => counts.claimed = n,
                Some(RoleDeliveryState::Accepted) => counts.accepted = n,
                Some(RoleDeliveryState::Acknowledged) => counts.acknowledged = n,
                None => {}
            }
        }
    }

    let mut deliveries = deliveries_where(
        conn,
        &role,
        "d.state != 'acknowledged' ORDER BY d.delivery_id ASC",
        OPEN_DELIVERY_CAP,
        now_secs,
    )?;
    let done = deliveries_where(
        conn,
        &role,
        "d.state = 'acknowledged' ORDER BY d.delivery_id DESC",
        done_limit,
        now_secs,
    )?;
    deliveries.extend(done);
    let listed = deliveries.len() as u64;
    let total = u64::from(counts.pending)
        + u64::from(counts.claimed)
        + u64::from(counts.accepted)
        + u64::from(counts.acknowledged);

    Ok(RoleQueueView {
        role,
        role_name: None,
        registrations,
        counts,
        deliveries,
        deliveries_truncated: listed < total,
    })
}

fn deliveries_where(
    conn: &Connection,
    role: &str,
    filter_and_order: &str,
    limit: u32,
    now_secs: i64,
) -> WvWriteResult<Vec<RoleDeliveryView>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT d.delivery_id, d.message_id, d.state, d.created_at, d.first_claimed_at, \
                d.claim_owner_instance, d.claim_owner_generation, d.claim_expires_at, \
                m.sender, m.text, \
                r.receipt_id, r.instance_id, r.generation, r.kind, r.note_or_reason, \
                r.queue_item_id, r.created_at \
         FROM wv_role_delivery d \
         LEFT JOIN wv_inbox m ON m.id = d.message_id \
         LEFT JOIN wv_role_acceptance_receipt r ON r.delivery_id = d.delivery_id \
         WHERE d.role = ?1 AND {filter_and_order} LIMIT ?2"
    ))?;
    let rows = stmt.query_map(params![role, limit], |r| {
        let state_raw: String = r.get(2)?;
        let state = RoleDeliveryState::parse(&state_raw).unwrap_or(RoleDeliveryState::Pending);
        let claim_expires_at: Option<i64> = r.get(7)?;
        let text: Option<String> = r.get(9)?;
        let receipt_id: Option<String> = r.get(10)?;
        let receipt = match receipt_id {
            Some(receipt_id) => Some(RoleReceiptView {
                receipt_id,
                instance_id: r.get(11)?,
                generation: r.get(12)?,
                kind: r
                    .get::<_, String>(13)
                    .ok()
                    .and_then(|k| match k.as_str() {
                        "no_work" => Some(AcceptKind::NoWork),
                        "work" => Some(AcceptKind::Work),
                        _ => None,
                    })
                    .unwrap_or(AcceptKind::Work),
                note: r.get::<_, Option<String>>(14)?.filter(|n| !n.is_empty()),
                queue_item_id: r.get(15)?,
                accepted_at_ms: r.get::<_, i64>(16)? * 1000,
            }),
            None => None,
        };
        Ok(RoleDeliveryView {
            delivery_id: r.get(0)?,
            message_id: r.get(1)?,
            state,
            from: r.get::<_, Option<String>>(8)?.unwrap_or_default(),
            from_name: None,
            excerpt: text
                .map(|t| t.chars().take(EXCERPT_CHARS).collect())
                .unwrap_or_default(),
            created_at_ms: r.get::<_, i64>(3)? * 1000,
            first_claimed_at_ms: r.get::<_, Option<i64>>(4)?.map(|s| s * 1000),
            claim_owner: r.get(5)?,
            claim_owner_name: None,
            claim_owner_generation: r.get(6)?,
            claim_expires_at_ms: claim_expires_at.map(|s| s * 1000),
            claim_lapsed: state == RoleDeliveryState::Claimed
                && claim_expires_at.is_some_and(|at| at <= now_secs),
            receipt,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::weft::role_inbox::{
        accept_role_message, ack_role_message, claim_role_messages, register_role,
        send_role_message_commit,
    };

    fn open() -> (tempfile::TempDir, WeftStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
        (dir, store)
    }

    fn send(store: &WeftStore, role: &str, text: &str, now: i64) -> (i64, i64) {
        send_role_message_commit(
            store, "human", role, text, None, None, None, "hash", now, None, None, None,
        )
        .unwrap()
    }

    #[test]
    fn shows_every_delivery_state_with_its_claim_owner_and_receipt() {
        let (_dir, store) = open();
        let now = 1_000_000;
        let (m_pending, _) = send(&store, "reviewer", "pending one", now);
        let (m_claimed, d_claimed) = send(&store, "reviewer", "claimed one", now + 1);
        let (_m_accepted, d_accepted) = send(&store, "reviewer", "accepted one", now + 2);
        let (_m_done, d_done) = send(&store, "reviewer", "done one", now + 3);
        let reg = register_role(&store, "reviewer", "inst-a", now + 10).unwrap();
        // Claim all four, then move each to its state.
        let claimed =
            claim_role_messages(&store, "reviewer", "inst-a", reg.generation, 4, now + 11).unwrap();
        assert_eq!(claimed.len(), 4);
        let token = |d: i64| {
            claimed
                .iter()
                .find(|c| c.delivery_id == d)
                .unwrap()
                .claim_token
                .clone()
        };
        let r_acc = accept_role_message(
            &store,
            d_accepted,
            &token(d_accepted),
            "inst-a",
            reg.generation,
            AcceptKind::NoWork,
            "not mine",
            now + 12,
        )
        .unwrap();
        let r_done = accept_role_message(
            &store,
            d_done,
            &token(d_done),
            "inst-a",
            reg.generation,
            AcceptKind::Work,
            "",
            now + 12,
        )
        .unwrap();
        ack_role_message(
            &store,
            d_done,
            &token(d_done),
            "inst-a",
            reg.generation,
            &r_done,
            now + 13,
        )
        .unwrap();
        // Put the first one back to pending, as a lapsed-and-released claim would be.
        store
            .conn()
            .unwrap()
            .execute(
                "UPDATE wv_role_delivery SET state='pending', claim_token=NULL, \
                 claim_owner_instance=NULL, claim_owner_generation=NULL, claim_expires_at=NULL \
                 WHERE message_id=?1",
                params![m_pending],
            )
            .unwrap();

        let roles = role_state_read(&store, None, now + 14, 20).unwrap();
        assert_eq!(roles.len(), 1);
        let q = &roles[0];
        assert_eq!(q.role, "reviewer");
        assert_eq!(
            q.counts,
            RoleDeliveryCounts {
                pending: 1,
                claimed: 1,
                accepted: 1,
                acknowledged: 1
            }
        );
        assert!(!q.deliveries_truncated);
        assert_eq!(q.registrations.len(), 1);
        assert!(q.registrations[0].live);
        assert_eq!(q.registrations[0].instance_id, "inst-a");
        assert_eq!(q.registrations[0].expires_at_ms % 1000, 0);

        let states: Vec<_> = q.deliveries.iter().map(|d| d.state).collect();
        assert_eq!(
            states,
            vec![
                RoleDeliveryState::Pending,
                RoleDeliveryState::Claimed,
                RoleDeliveryState::Accepted,
                RoleDeliveryState::Acknowledged
            ]
        );
        let pending = &q.deliveries[0];
        assert_eq!(pending.claim_owner, None);
        assert_eq!(pending.excerpt, "pending one");
        assert_eq!(pending.from, "human");
        let claimed_row = q
            .deliveries
            .iter()
            .find(|d| d.delivery_id == d_claimed)
            .unwrap();
        assert_eq!(claimed_row.message_id, m_claimed);
        assert_eq!(claimed_row.claim_owner.as_deref(), Some("inst-a"));
        assert!(!claimed_row.claim_lapsed);
        assert!(claimed_row.receipt.is_none());
        let accepted = q
            .deliveries
            .iter()
            .find(|d| d.delivery_id == d_accepted)
            .unwrap();
        let receipt = accepted.receipt.as_ref().unwrap();
        assert_eq!(receipt.receipt_id, r_acc);
        assert_eq!(receipt.kind, AcceptKind::NoWork);
        assert_eq!(receipt.note.as_deref(), Some("not mine"));

        // Far in the future the claim and the registration have lapsed.
        let later = role_state_read(&store, Some("reviewer"), now + 100_000, 20).unwrap();
        let claimed_later = later[0]
            .deliveries
            .iter()
            .find(|d| d.delivery_id == d_claimed)
            .unwrap();
        assert!(claimed_later.claim_lapsed);
        assert!(!later[0].registrations[0].live);
    }

    #[test]
    fn reading_claims_nothing_and_limits_only_finished_deliveries() {
        let (_dir, store) = open();
        let now = 2_000_000;
        for i in 0..3 {
            send(&store, "builder", &format!("msg {i}"), now + i);
        }
        let first = role_state_read(&store, None, now + 10, 0).unwrap();
        let second = role_state_read(&store, None, now + 10, 0).unwrap();
        assert_eq!(first, second, "a read changes nothing");
        assert_eq!(first[0].counts.pending, 3);
        assert_eq!(
            first[0].deliveries.len(),
            3,
            "open deliveries are never cut"
        );
        assert!(!first[0].deliveries_truncated);
        // A role with nothing stored reads empty, not an error.
        let none = role_state_read(&store, Some("nobody"), now, 5).unwrap();
        assert_eq!(none[0].counts, RoleDeliveryCounts::default());
        assert!(none[0].deliveries.is_empty());
    }

    #[test]
    fn the_reply_to_spelling_reads_the_same_queue() {
        let (_dir, store) = open();
        let now = 2_000_000;
        send(&store, "role:builder", "queued under the prefix", now);
        send(&store, "builder", "queued bare", now + 1);
        let bare = role_state_read(&store, Some("builder"), now + 2, 0).unwrap();
        let prefixed = role_state_read(&store, Some(" role:builder "), now + 2, 0).unwrap();
        assert_eq!(bare, prefixed);
        assert_eq!(bare[0].role, "builder");
        assert_eq!(bare[0].counts.pending, 2);
    }

    #[test]
    fn human_is_not_a_role_queue() {
        let (_dir, store) = open();
        for role in ["human", "role:human", "role:HUMAN", " Human "] {
            assert!(
                send_role_message_commit(
                    &store, "H1", role, "hi", None, None, None, "hash", 1, None, None, None,
                )
                .is_err(),
                "{role}"
            );
        }
        assert!(role_state_read(&store, None, 1, 0).unwrap().is_empty());
    }

    #[test]
    fn rows_an_older_build_queued_under_the_prefix_are_rekeyed_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("weft.sqlite3");
        let store = WeftStore::open(&path).unwrap();
        // What an older build stored for `send_role_message {role:"role:legacy"}`
        // (and `role:human`, which must not turn into mail to the human).
        let conn = store.conn().unwrap();
        for (id, role) in [(1, "role:legacy"), (2, "role:human")] {
            conn.execute(
                "INSERT INTO wv_inbox (id, sender, recipient, text, created_at) \
                 VALUES (?1, 'H1', ?2, 'old', 1)",
                rusqlite::params![id, role],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO wv_role_delivery (message_id, role, state, created_at) \
                 VALUES (?1, ?2, 'pending', 1)",
                rusqlite::params![id, role],
            )
            .unwrap();
        }
        // A session-addressed row is never touched, whatever its name.
        conn.execute(
            "INSERT INTO wv_inbox (id, sender, recipient, text, created_at) \
             VALUES (3, 'H1', 'role:tab', 'to a tab', 1)",
            [],
        )
        .unwrap();
        drop(conn);
        drop(store);
        crate::weft::sqlite::reset_cached_store_for_test(&path);
        let store = WeftStore::open(&path).unwrap();
        let legacy = role_state_read(&store, Some("legacy"), 2, 0).unwrap();
        assert_eq!(legacy[0].counts.pending, 1);
        let recipients: Vec<String> = {
            let conn = store.conn().unwrap();
            let mut stmt = conn
                .prepare("SELECT recipient FROM wv_inbox ORDER BY id")
                .unwrap();
            let rows = stmt.query_map([], |r| r.get(0)).unwrap();
            rows.collect::<rusqlite::Result<_>>().unwrap()
        };
        assert_eq!(recipients, vec!["legacy", "role:human", "role:tab"]);
        // The `role:human` queue (and a bare `human` one) no write can reach is
        // not listed, so it cannot sit in role_state or the rollup forever.
        store
            .conn()
            .unwrap()
            .execute(
                "INSERT INTO wv_role_registration \
                 (role, instance_id, generation, registered_at, last_renewed_at, expires_at) \
                 VALUES ('human', 'H9', 1, 1, 1, 99)",
                [],
            )
            .unwrap();
        let listed: Vec<String> = role_state_read(&store, None, 2, 0)
            .unwrap()
            .into_iter()
            .map(|q| q.role)
            .collect();
        assert_eq!(listed, vec!["legacy"]);
        assert!(role_state_read(&store, Some("role:human"), 2, 0)
            .unwrap()
            .is_empty());
    }
}
