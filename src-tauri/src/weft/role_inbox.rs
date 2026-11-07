// SPDX-License-Identifier: MIT
//! SW-BSI-2: role-keyed delivery on top of the G-MSG mailbox.
//!
//! A role mailbox is addressed as `(user, org, team, role)` — role is an agent id
//! (`config/agents/<id>/agent.yaml`). Any role is shared (many live instances, one
//! atomic owner per delivery) unless its first live registration asks for
//! `exclusive` (one live holder at a time). No role name is special. A registration
//! made for a running session is kept alive by the app while that session runs
//! ([`renew_session_registrations`]) and released when it ends. See
//! `_bmad-output/08-dashboards-and-agents/25-builder-skills-integration/GROOMING-PLAN.md`.
//!
//! Every write co-commits its `wv_write_log` audit row on one IMMEDIATE
//! transaction, like every other `wv_*` ledger. All functions are synchronous
//! SQLite; callers run them on the blocking pool.

use rusqlite::{params, OptionalExtension, TransactionBehavior};
use uuid::Uuid;

use super::sqlite::{write_audit_public, WvValidation, WvWriteError, WvWriteResult};
use super::store::WeftStore;

/// Default registration TTL. A registration with no session renews before this
/// elapses; one with a session is renewed by the app while the session runs.
pub const DEFAULT_REGISTRATION_TTL_SECS: i64 = 60;

/// Default claim TTL. A claim stale past this is eligible for reclaim.
pub const DEFAULT_CLAIM_TTL_SECS: i64 = 60;

/// Registration TTL for remote callers. One transient 20s renew miss over the
/// relay no longer orphans the registration — the extra headroom absorbs it.
pub const REMOTE_REGISTRATION_TTL_SECS: i64 = 180;

/// Claim TTL for remote callers. Same rationale as the registration widening:
/// one transient relay stall during accept/ack no longer flips the claim stale.
pub const REMOTE_CLAIM_TTL_SECS: i64 = 180;

/// Hard cap on one claim batch. Groomed at 50.
pub const MAX_CLAIM_LIMIT: u32 = 50;

const REG: &str = "wv_role_registration";
const DEL: &str = "wv_role_delivery";
const RCPT: &str = "wv_role_acceptance_receipt";
const REQ: &str = "wv_send_message_request";

/// The disposition the accepting instance recorded for a delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceptKind {
    Work,
    NoWork,
}

impl AcceptKind {
    fn as_str(self) -> &'static str {
        match self {
            AcceptKind::Work => "work",
            AcceptKind::NoWork => "no_work",
        }
    }

    fn parse(s: &str) -> Option<AcceptKind> {
        match s {
            "work" => Some(AcceptKind::Work),
            "no_work" => Some(AcceptKind::NoWork),
            _ => None,
        }
    }
}

/// The outcome `register_role` returns.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationOutcome {
    /// The instance id the registration is held under (pass it to every later verb).
    pub instance_id: String,
    /// The session that keeps the registration alive, when one was given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Whether the role is held one-holder-only.
    pub exclusive: bool,
    pub generation: i64,
    pub expires_at: i64,
    pub pending_count: i64,
    /// Deliveries whose work was accepted by any prior instance of this role and
    /// which have NOT yet been acknowledged. A re-registering instance may adopt
    /// (resume) these.
    pub accepted_unacked: Vec<AcceptedWork>,
}

/// A delivery that was accepted and is not yet acknowledged. On re-registration
/// after a crash the caller inspects these to resume work in flight.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptedWork {
    pub delivery_id: i64,
    pub role: String,
    pub receipt_id: String,
    pub message_id: i64,
    pub kind: AcceptKind,
    /// Deprecated: always null for new receipts; role work is not mirrored into
    /// wv_queue. Kept for wire compatibility.
    pub queue_item_id: Option<i64>,
}

/// One row `claim_role_messages` returns to the caller.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimedDelivery {
    pub delivery_id: i64,
    pub message_id: i64,
    pub role: String,
    pub claim_token: String,
    pub claim_expires_at: i64,
    /// The message itself, so a claimant can act on it without a second read.
    pub text: String,
    /// Who sent it: a session handle, or `human`.
    pub from: String,
    /// When it was sent, epoch ms.
    pub created_at_ms: i64,
    /// Present when a prior instance of this role accepted this delivery and
    /// the current claim is a reclaim-after-expiry. The caller resumes under
    /// the existing receipt rather than re-accepting.
    pub existing_receipt: Option<ReceiptSummary>,
}

/// Lightweight view of a prior acceptance receipt returned alongside a reclaim.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptSummary {
    pub receipt_id: String,
    pub kind: AcceptKind,
    /// Deprecated: always null for new receipts; role work is not mirrored into
    /// wv_queue. Kept for wire compatibility.
    pub queue_item_id: Option<i64>,
}

/// The outcome `send_message` returns for its request-id idempotency path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendRequestOutcome {
    /// First-time call: caller proceeds with inserting the message and (role form
    /// only) the delivery; then calls [`record_send_request_insert`] to pin the
    /// idempotency row.
    Fresh,
    /// Replay of a committed request: the stored (message_id, delivery_id).
    Replay {
        message_id: i64,
        delivery_id: Option<i64>,
    },
}

/// Errors specific to the role-keyed delivery layer. Each maps to a distinct
/// `WvWriteError::Invalid` reason so an app/MCP caller can distinguish them by
/// substring. Keep the strings stable — the FE and tests match on them.
#[derive(Debug)]
pub enum RoleInboxError {
    AlreadyHeld {
        current_instance: String,
    },
    /// The role's live holders registered in the other mode (`held_exclusive`).
    ModeConflict {
        held_exclusive: bool,
    },
    StaleRegistration,
    StaleClaim,
    StaleAck,
    AcceptanceConflict {
        existing_kind: AcceptKind,
    },
    DeliveryNotFound,
    RequestConflict,
    Store(WvWriteError),
}

impl std::fmt::Display for RoleInboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RoleInboxError::AlreadyHeld { current_instance } => {
                write!(f, "role already held by instance '{current_instance}'")
            }
            RoleInboxError::ModeConflict { held_exclusive } => write!(
                f,
                "role is held {} by its live holders",
                if *held_exclusive {
                    "exclusive"
                } else {
                    "shared"
                }
            ),
            RoleInboxError::StaleRegistration => write!(f, "registration is stale or expired"),
            RoleInboxError::StaleClaim => write!(f, "claim token is stale or expired"),
            RoleInboxError::StaleAck => write!(f, "acknowledgement does not match the receipt"),
            RoleInboxError::AcceptanceConflict { existing_kind } => write!(
                f,
                "delivery already accepted with disposition '{}'",
                existing_kind.as_str()
            ),
            RoleInboxError::DeliveryNotFound => write!(f, "delivery not found"),
            RoleInboxError::RequestConflict => {
                write!(f, "request_id reused with a different payload")
            }
            RoleInboxError::Store(e) => write!(f, "{e}"),
        }
    }
}

impl From<WvWriteError> for RoleInboxError {
    fn from(e: WvWriteError) -> Self {
        RoleInboxError::Store(e)
    }
}

impl From<rusqlite::Error> for RoleInboxError {
    fn from(e: rusqlite::Error) -> Self {
        RoleInboxError::Store(e.into())
    }
}

impl From<super::store::WeftStoreError> for RoleInboxError {
    fn from(e: super::store::WeftStoreError) -> Self {
        RoleInboxError::Store(e.into())
    }
}

pub type RoleInboxResult<T> = Result<T, RoleInboxError>;

fn invalid(msg: &str) -> WvWriteError {
    WvValidation(msg.to_string()).into()
}

/// `role` without a `role:` prefix (the Reply-To spelling of a role queue), trimmed:
/// `role:reviewer` and `reviewer` name one queue.
pub fn role_name(role: &str) -> &str {
    let r = role.trim();
    r.strip_prefix("role:").unwrap_or(r).trim()
}

/// `role` is any non-blank ascii-ish identifier. We don't validate it against
/// the Agents registry — that is a UX concern, not a transport invariant.
/// `human` (any case, with or without `role:`) is refused: a role message's
/// inbox row is addressed to its role, so it would read as mail to the human
/// while its delivery sat in a queue no agent holds.
pub fn stored_role(role: &str) -> WvWriteResult<&str> {
    let r = role_name(role);
    if r.is_empty() {
        return Err(invalid("role must not be blank"));
    }
    if r.eq_ignore_ascii_case("human") {
        return Err(invalid(
            "`human` is not a role queue: send to the human with send_message {to:\"human\"}",
        ));
    }
    Ok(r)
}

fn stored_instance(instance: &str) -> WvWriteResult<&str> {
    let i = instance.trim();
    if i.is_empty() {
        return Err(invalid("instance_id must not be blank"));
    }
    Ok(i)
}

/// How a registration is held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterOptions {
    /// The session that keeps the registration alive. `None`: the caller renews it.
    pub session_id: Option<String>,
    /// One live holder at a time.
    pub exclusive: bool,
    /// Registration (and claim) lifetime, renewed in full on each renewal.
    pub ttl_secs: i64,
}

impl Default for RegisterOptions {
    fn default() -> Self {
        Self {
            session_id: None,
            exclusive: false,
            ttl_secs: DEFAULT_REGISTRATION_TTL_SECS,
        }
    }
}

/// Register `instance_id` under `role` as a shared, caller-renewed registration.
/// See [`register_role_as`].
pub fn register_role(
    store: &WeftStore,
    role: &str,
    instance_id: &str,
    now: i64,
) -> RoleInboxResult<RegistrationOutcome> {
    register_role_as(store, role, instance_id, &RegisterOptions::default(), now)
}

/// Same as [`register_role`] but with an explicit TTL so the receiving handler
/// can widen the window for remote callers (see [`REMOTE_REGISTRATION_TTL_SECS`]).
pub fn register_role_with_ttl(
    store: &WeftStore,
    role: &str,
    instance_id: &str,
    now: i64,
    ttl_secs: i64,
) -> RoleInboxResult<RegistrationOutcome> {
    let options = RegisterOptions {
        ttl_secs,
        ..RegisterOptions::default()
    };
    register_role_as(store, role, instance_id, &options, now)
}

/// Register `instance_id` under `role` and return its outcome.
///
/// The first live registration sets the role's mode. While a role has live holders,
/// a request in the other mode gets [`RoleInboxError::ModeConflict`], and an
/// `exclusive` request from a second instance gets [`RoleInboxError::AlreadyHeld`].
/// With no live holders the mode resets. A repeat by an instance whose registration
/// is still live keeps its generation (claim tokens stay valid) and refreshes its
/// expiry; a registration that lapsed comes back with the next generation.
pub fn register_role_as(
    store: &WeftStore,
    role: &str,
    instance_id: &str,
    options: &RegisterOptions,
    now: i64,
) -> RoleInboxResult<RegistrationOutcome> {
    let role = stored_role(role)?;
    let instance = stored_instance(instance_id)?;
    let session = options
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let ttl = options.ttl_secs;
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    // The other live holders and their mode.
    let others: Vec<(String, bool)> = {
        let mut stmt = tx.prepare(
            "SELECT instance_id, exclusive FROM wv_role_registration \
             WHERE role=?1 AND instance_id != ?2 AND expires_at > ?3 \
             ORDER BY registered_at, instance_id",
        )?;
        let rows = stmt
            .query_map(params![role, instance, now], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? != 0))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        rows
    };
    if let Some((holder, held_exclusive)) = others.first() {
        if *held_exclusive != options.exclusive {
            return Err(RoleInboxError::ModeConflict {
                held_exclusive: *held_exclusive,
            });
        }
        if options.exclusive {
            return Err(RoleInboxError::AlreadyHeld {
                current_instance: holder.clone(),
            });
        }
    }
    if options.exclusive {
        // An expired holder is replaced; delete its row so an exclusive role keeps
        // at most one row.
        tx.execute(
            "DELETE FROM wv_role_registration \
             WHERE role=?1 AND instance_id != ?2 AND expires_at <= ?3",
            params![role, instance, now],
        )?;
    }

    // Upsert this instance's own registration. A live repeat keeps its generation;
    // a lapsed one bumps it, which fences claim tokens minted before the lapse.
    let existing: Option<(i64, i64)> = tx
        .query_row(
            "SELECT generation, expires_at FROM wv_role_registration \
             WHERE role=?1 AND instance_id=?2",
            params![role, instance],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (generation, is_new) = match existing {
        Some((prev, expires_at)) if expires_at > now => (prev, false),
        Some((prev, _)) => (prev + 1, false),
        None => (1, true),
    };
    let expires_at = now + ttl;
    if is_new {
        tx.execute(
            "INSERT INTO wv_role_registration \
             (role, instance_id, generation, registered_at, expires_at, last_renewed_at, \
              session_id, exclusive, ttl_secs) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?4, ?6, ?7, ?8)",
            params![
                role,
                instance,
                generation,
                now,
                expires_at,
                session,
                options.exclusive as i64,
                ttl
            ],
        )?;
        write_audit_public(&tx, REG, "append", 0)?;
    } else {
        tx.execute(
            "UPDATE wv_role_registration \
             SET generation=?3, expires_at=?4, last_renewed_at=?5, \
                 session_id=?6, exclusive=?7, ttl_secs=?8 \
             WHERE role=?1 AND instance_id=?2",
            params![
                role,
                instance,
                generation,
                expires_at,
                now,
                session,
                options.exclusive as i64,
                ttl
            ],
        )?;
        write_audit_public(&tx, REG, "mutate", 0)?;
    }

    let pending_count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM wv_role_delivery WHERE role=?1 AND state='pending'",
        params![role],
        |r| r.get(0),
    )?;

    // Collect accepted-but-unacknowledged deliveries for this role so the caller
    // can adopt them. A fresh register with no prior work returns an empty vec.
    let mut stmt = tx.prepare(
        "SELECT d.delivery_id, d.role, r.receipt_id, d.message_id, r.kind, r.queue_item_id \
         FROM wv_role_delivery d \
         JOIN wv_role_acceptance_receipt r ON r.delivery_id = d.delivery_id \
         WHERE d.role=?1 AND d.state='accepted'",
    )?;
    let rows = stmt
        .query_map(params![role], |r| {
            let kind_str: String = r.get(4)?;
            Ok(AcceptedWork {
                delivery_id: r.get(0)?,
                role: r.get(1)?,
                receipt_id: r.get(2)?,
                message_id: r.get(3)?,
                kind: AcceptKind::parse(&kind_str).unwrap_or(AcceptKind::Work),
                queue_item_id: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    tx.commit()?;

    Ok(RegistrationOutcome {
        instance_id: instance.to_string(),
        session_id: session.map(str::to_string),
        exclusive: options.exclusive,
        generation,
        expires_at,
        pending_count,
        accepted_unacked: rows,
    })
}

/// Claim up to `limit` pending deliveries for `role` under this registration.
/// Returns the claimed rows oldest first. Each row's `claim_token` is unique
/// per claim and must be presented to `accept` and `ack`.
pub fn claim_role_messages(
    store: &WeftStore,
    role: &str,
    instance_id: &str,
    generation: i64,
    limit: u32,
    now: i64,
) -> RoleInboxResult<Vec<ClaimedDelivery>> {
    claim_role_messages_with_ttl(
        store,
        role,
        instance_id,
        generation,
        limit,
        now,
        DEFAULT_CLAIM_TTL_SECS,
    )
}

/// Same as [`claim_role_messages`] but with an explicit per-claim TTL so the
/// receiving handler can widen the window for remote callers (see
/// [`REMOTE_CLAIM_TTL_SECS`]).
pub fn claim_role_messages_with_ttl(
    store: &WeftStore,
    role: &str,
    instance_id: &str,
    generation: i64,
    limit: u32,
    now: i64,
    claim_ttl_secs: i64,
) -> RoleInboxResult<Vec<ClaimedDelivery>> {
    let role = stored_role(role)?;
    let instance = stored_instance(instance_id)?;
    let limit = limit.clamp(1, MAX_CLAIM_LIMIT);
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    verify_registration(&tx, role, instance, generation, now)?;

    // Also release claims whose own claim_expires_at has passed, so they can be
    // re-claimed by this (or any) instance. This mirrors the groomed semantics
    // of expiry → eligible-for-reclaim. Reset covers BOTH `claimed` and
    // `accepted` rows: accepted-but-unacknowledged work whose instance died
    // becomes reclaimable; the acceptance receipt row stays authoritative and
    // the next claimant adopts it via the ClaimedDelivery.existing_receipt.
    tx.execute(
        "UPDATE wv_role_delivery \
         SET state='pending', claim_token=NULL, claim_owner_instance=NULL, \
             claim_owner_generation=NULL, claim_expires_at=NULL \
         WHERE role=?1 AND state IN ('claimed','accepted') \
           AND claim_expires_at IS NOT NULL AND claim_expires_at <= ?2",
        params![role, now],
    )?;

    // Retire orphan deliveries (their message row is gone) before selecting, so an
    // orphan at the head of the queue can't use up the LIMIT batch and make a claim
    // come back empty while live messages wait behind it. An orphan can never be
    // acted on, and must not reach an agent as an empty message that looks real.
    let orphans = {
        let mut stmt = tx.prepare(
            "SELECT delivery_id, message_id FROM wv_role_delivery \
             WHERE role=?1 AND state='pending' \
               AND NOT EXISTS (SELECT 1 FROM wv_inbox i WHERE i.id = wv_role_delivery.message_id)",
        )?;
        let rows = stmt
            .query_map(params![role], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for (delivery_id, message_id) in orphans {
        let retired = tx.execute(
            "UPDATE wv_role_delivery SET state='acknowledged', claim_token=NULL, \
                 claim_owner_instance=NULL, claim_owner_generation=NULL, claim_expires_at=NULL \
             WHERE delivery_id=?1 AND state='pending'",
            params![delivery_id],
        )?;
        if retired == 1 {
            write_audit_public(&tx, DEL, "mutate", delivery_id)?;
            log::warn!(
                target: "inbox",
                "role {role}: retired delivery {delivery_id}, whose message {message_id} is gone"
            );
        }
    }

    // Collect candidate ids under row-level selection (ORDER BY + LIMIT), then
    // UPDATE each. SQLite does not accept LIMIT in UPDATE here. The whole thing
    // runs inside one IMMEDIATE tx, so no other writer can slip rows out from
    // under us between SELECT and UPDATE.
    let mut candidate_stmt = tx.prepare(
        "SELECT delivery_id, message_id FROM wv_role_delivery \
         WHERE role=?1 AND state='pending' \
           AND EXISTS (SELECT 1 FROM wv_inbox i \
                       WHERE i.id = wv_role_delivery.message_id AND i.withdrawn_at IS NULL) \
         ORDER BY created_at ASC, delivery_id ASC LIMIT ?2",
    )?;
    let candidates = candidate_stmt
        .query_map(params![role, limit as i64], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(candidate_stmt);

    let expires = now + claim_ttl_secs;
    let mut claimed = Vec::with_capacity(candidates.len());
    for (delivery_id, message_id) in candidates {
        // Orphans were retired above and the SELECT requires the message row, so
        // this lookup always finds it inside the IMMEDIATE tx; skip defensively.
        let message: Option<(String, String, i64)> = tx
            .query_row(
                "SELECT sender, text, created_at FROM wv_inbox WHERE id=?1",
                params![message_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((from, text, created_at_ms)) = message else {
            continue;
        };
        let token = Uuid::new_v4().to_string();
        let updated = tx.execute(
            "UPDATE wv_role_delivery \
             SET state='claimed', claim_token=?2, claim_owner_instance=?3, \
                 claim_owner_generation=?4, claim_expires_at=?5, \
                 first_claimed_at=COALESCE(first_claimed_at, ?6) \
             WHERE delivery_id=?1 AND state='pending'",
            params![delivery_id, token, instance, generation, expires, now],
        )?;
        if updated != 1 {
            // Another path flipped the state between SELECT and this UPDATE,
            // which cannot happen inside IMMEDIATE but we handle it defensively.
            continue;
        }
        write_audit_public(&tx, DEL, "mutate", delivery_id)?;

        // Carry any existing acceptance receipt back to the caller so a reclaim
        // after expiry can adopt the accepted work rather than re-accepting.
        let existing: Option<(String, String, Option<i64>)> = tx
            .query_row(
                "SELECT receipt_id, kind, queue_item_id FROM wv_role_acceptance_receipt \
                 WHERE delivery_id=?1",
                params![delivery_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let existing_receipt = existing.map(|(receipt_id, kind, queue_item_id)| ReceiptSummary {
            receipt_id,
            kind: AcceptKind::parse(&kind).unwrap_or(AcceptKind::Work),
            queue_item_id,
        });

        claimed.push(ClaimedDelivery {
            delivery_id,
            message_id,
            role: role.to_string(),
            claim_token: token,
            claim_expires_at: expires,
            text,
            from,
            created_at_ms,
            existing_receipt,
        });
    }

    tx.commit()?;
    Ok(claimed)
}

/// Commit an acceptance receipt (`work` or `no_work`). Role work is tracked by
/// the receipt and `ack_role_message`; it is never mirrored into `wv_queue`.
/// Returns the receipt id.
#[allow(clippy::too_many_arguments)]
pub fn accept_role_message(
    store: &WeftStore,
    delivery_id: i64,
    claim_token: &str,
    instance_id: &str,
    generation: i64,
    kind: AcceptKind,
    note_or_reason: &str,
    now: i64,
) -> RoleInboxResult<String> {
    let instance = stored_instance(instance_id)?;
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    verify_claim(&tx, delivery_id, claim_token, instance, generation, now)?;

    // Idempotent replay / disposition conflict.
    let existing: Option<(String, String, String, i64, Option<i64>)> = tx
        .query_row(
            "SELECT receipt_id, kind, instance_id, generation, queue_item_id \
             FROM wv_role_acceptance_receipt WHERE delivery_id=?1",
            params![delivery_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    if let Some((prev_receipt, prev_kind, prev_instance, prev_gen, _)) = existing {
        if prev_kind == kind.as_str() && prev_instance == instance && prev_gen == generation {
            // Pure replay — tx had no writes, so commit is a no-op.
            tx.commit()?;
            return Ok(prev_receipt);
        }
        if prev_kind != kind.as_str() {
            return Err(RoleInboxError::AcceptanceConflict {
                existing_kind: AcceptKind::parse(&prev_kind).unwrap_or(AcceptKind::Work),
            });
        }
        // Same disposition but different instance/generation (another instance
        // accepted first in a reclaim-race). Return the ORIGINAL receipt — it
        // is the authoritative acceptance. The current claim's work can adopt it.
        tx.commit()?;
        return Ok(prev_receipt);
    }

    // Fetch the delivery's role + message_id to pin them in the receipt.
    let (role, message_id): (String, i64) = tx.query_row(
        "SELECT role, message_id FROM wv_role_delivery WHERE delivery_id=?1",
        params![delivery_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;

    let receipt_id = Uuid::new_v4().to_string();
    // Role work is tracked by this receipt and `ack_role_message`; it is never
    // mirrored into `wv_queue`. The column stays for wire compatibility and is
    // always NULL for new receipts.
    tx.execute(
        "INSERT INTO wv_role_acceptance_receipt \
         (receipt_id, delivery_id, role, message_id, instance_id, generation, kind, note_or_reason, queue_item_id, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            receipt_id,
            delivery_id,
            role,
            message_id,
            instance,
            generation,
            kind.as_str(),
            note_or_reason,
            None::<i64>,
            now,
        ],
    )?;
    write_audit_public(&tx, RCPT, "append", 0)?;

    tx.execute(
        "UPDATE wv_role_delivery SET state='accepted' WHERE delivery_id=?1",
        params![delivery_id],
    )?;
    write_audit_public(&tx, DEL, "mutate", delivery_id)?;

    tx.commit()?;
    Ok(receipt_id)
}

/// Acknowledge: the accepting instance says "I'm done with this delivery". The
/// receipt is a permanent record; ack flips the delivery row to `acknowledged`.
pub fn ack_role_message(
    store: &WeftStore,
    delivery_id: i64,
    claim_token: &str,
    instance_id: &str,
    generation: i64,
    receipt_id: &str,
    now: i64,
) -> RoleInboxResult<()> {
    let instance = stored_instance(instance_id)?;
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    verify_claim(&tx, delivery_id, claim_token, instance, generation, now)?;

    let actual: Option<(String, String)> = tx
        .query_row(
            "SELECT receipt_id, kind FROM wv_role_acceptance_receipt WHERE delivery_id=?1",
            params![delivery_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (actual_receipt, _kind) = match actual {
        Some(r) => r,
        None => return Err(RoleInboxError::StaleAck),
    };
    if actual_receipt != receipt_id {
        return Err(RoleInboxError::StaleAck);
    }

    let state: String = tx.query_row(
        "SELECT state FROM wv_role_delivery WHERE delivery_id=?1",
        params![delivery_id],
        |r| r.get(0),
    )?;
    if state == "acknowledged" {
        // Idempotent re-ack of the same receipt.
        tx.commit()?;
        return Ok(());
    }
    if state != "accepted" {
        return Err(RoleInboxError::StaleAck);
    }

    tx.execute(
        "UPDATE wv_role_delivery SET state='acknowledged' WHERE delivery_id=?1",
        params![delivery_id],
    )?;
    write_audit_public(&tx, DEL, "mutate", delivery_id)?;

    let _ = now; // reserved for future touched_at columns.
    tx.commit()?;
    Ok(())
}

/// Release this instance's registration + any still-claimed-but-unaccepted
/// deliveries. Already-accepted or already-acknowledged rows stay put — their
/// receipts are permanent.
pub fn release_role(
    store: &WeftStore,
    role: &str,
    instance_id: &str,
    generation: i64,
) -> RoleInboxResult<u64> {
    let role = stored_role(role)?;
    let instance = stored_instance(instance_id)?;
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let released = release_in(&tx, role, instance, generation)?;
    tx.commit()?;
    Ok(released)
}

/// [`release_role`] inside the caller's transaction.
fn release_in(
    tx: &rusqlite::Transaction,
    role: &str,
    instance: &str,
    generation: i64,
) -> RoleInboxResult<u64> {
    // Return claimed rows (not accepted/acknowledged) to pending.
    let released = tx.execute(
        "UPDATE wv_role_delivery \
         SET state='pending', claim_token=NULL, claim_owner_instance=NULL, \
             claim_owner_generation=NULL, claim_expires_at=NULL \
         WHERE role=?1 AND state='claimed' \
           AND claim_owner_instance=?2 AND claim_owner_generation=?3",
        params![role, instance, generation],
    )? as u64;
    if released > 0 {
        write_audit_public(tx, DEL, "mutate", 0)?;
    }

    let deleted = tx.execute(
        "DELETE FROM wv_role_registration \
         WHERE role=?1 AND instance_id=?2 AND generation=?3",
        params![role, instance, generation],
    )?;
    if deleted > 0 {
        write_audit_public(tx, REG, "mutate", 0)?;
    }
    Ok(released)
}

/// Whether the session behind a registration runs, as the app sees it now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionLiveness {
    /// It runs: its registrations are renewed.
    Live,
    /// It ended (exited, stopped): its registrations are released now.
    Ended,
    /// Not confirmed either way (its machine is not connected, or no session by that
    /// id is listed): nothing changes and the registration lapses at its expiry.
    Unknown,
}

/// What one renewal pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenewReport {
    /// Registrations whose expiry moved forward.
    pub renewed: usize,
    /// Registrations released because their session ended.
    pub released: usize,
    /// Claimed deliveries those releases returned to pending.
    pub released_claims: u64,
    /// Session-held registrations still live after the pass (renewed or waiting on
    /// an unknown session).
    pub remaining: usize,
}

/// One renewal pass over every live registration that names a session: a live
/// session's registration gets `expires_at = now + ttl` with its generation unchanged,
/// and every claim it still owns at that generation is extended the same way (its
/// claim token unchanged); an ended session's registration is released, its claimed
/// deliveries return to pending, and its accepted-but-unacknowledged deliveries
/// become adoptable now; an unknown one is left to lapse. A registration with no
/// session, or one already expired, is never touched.
pub fn renew_session_registrations(
    store: &WeftStore,
    now: i64,
    liveness: impl Fn(&str) -> SessionLiveness,
) -> RoleInboxResult<RenewReport> {
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    type Held = (String, String, i64, String, i64);
    let held: Vec<Held> = {
        let mut stmt = tx.prepare(
            "SELECT role, instance_id, generation, session_id, ttl_secs \
             FROM wv_role_registration \
             WHERE session_id IS NOT NULL AND expires_at > ?1",
        )?;
        let rows = stmt
            .query_map(params![now], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        rows
    };
    let mut report = RenewReport::default();
    for (role, instance, generation, session, ttl) in held {
        match liveness(&session) {
            SessionLiveness::Live => {
                let expires_at = now + ttl;
                tx.execute(
                    "UPDATE wv_role_registration SET expires_at=?3, last_renewed_at=?4 \
                     WHERE role=?1 AND instance_id=?2",
                    params![role, instance, expires_at, now],
                )?;
                let extended = tx.execute(
                    "UPDATE wv_role_delivery SET claim_expires_at=?4 \
                     WHERE role=?1 AND claim_owner_instance=?2 AND claim_owner_generation=?3 \
                       AND state IN ('claimed','accepted') AND claim_expires_at > ?5",
                    params![role, instance, generation, expires_at, now],
                )?;
                write_audit_public(&tx, REG, "mutate", 0)?;
                if extended > 0 {
                    write_audit_public(&tx, DEL, "mutate", 0)?;
                }
                report.renewed += 1;
                report.remaining += 1;
            }
            SessionLiveness::Ended => {
                report.released_claims += release_in(&tx, &role, &instance, generation)?;
                // Its accepted work is adoptable by the next claim now, not at expiry.
                let adoptable = tx.execute(
                    "UPDATE wv_role_delivery SET claim_expires_at=?4 \
                     WHERE role=?1 AND claim_owner_instance=?2 AND claim_owner_generation=?3 \
                       AND state='accepted' AND claim_expires_at > ?4",
                    params![role, instance, generation, now],
                )?;
                if adoptable > 0 {
                    write_audit_public(&tx, DEL, "mutate", 0)?;
                }
                report.released += 1;
            }
            SessionLiveness::Unknown => report.remaining += 1,
        }
    }
    tx.commit()?;
    Ok(report)
}

/// What [`withdraw_message`] found.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum WithdrawOutcome {
    /// Withdrawn now, or earlier (the stored time): no agent will see it.
    #[serde(rename_all = "camelCase")]
    Withdrawn { withdrawn_at_ms: i64 },
    /// An agent already picked it up; it stays delivered.
    TooLate {
        state: super::inbox::InboxDeliveryState,
    },
    /// No such message, or one addressed to the human (nothing to withdraw from).
    NotFound,
}

/// Withdraw message `message_id` before any agent picks it up, in one IMMEDIATE
/// transaction, so it is atomic against a concurrent claim: either the claim took the
/// delivery first (too late) or the delivery is gone before the claim looks.
///
/// A role delivery is withdrawn while it is pending with no acceptance receipt (its
/// delivery row is deleted). A plain message to an agent session is withdrawn while
/// unread. Repeating a withdrawal returns the first one's time.
pub fn withdraw_message(
    store: &WeftStore,
    message_id: i64,
    now_ms: i64,
) -> RoleInboxResult<WithdrawOutcome> {
    withdraw_message_changed(store, message_id, now_ms).map(|(outcome, _)| outcome)
}

/// [`withdraw_message`], and whether this call is the one that withdrew it.
pub fn withdraw_message_changed(
    store: &WeftStore,
    message_id: i64,
    now_ms: i64,
) -> RoleInboxResult<(WithdrawOutcome, bool)> {
    use super::inbox::{DeliveryRow, InboxDeliveryState, HUMAN};
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    let found: Option<(String, Option<i64>, Option<i64>)> = tx
        .query_row(
            "SELECT recipient, read_at, withdrawn_at FROM wv_inbox WHERE id=?1",
            params![message_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((recipient, read_at, withdrawn_at)) = found else {
        return Ok((WithdrawOutcome::NotFound, false));
    };
    if let Some(at) = withdrawn_at {
        return Ok((
            WithdrawOutcome::Withdrawn {
                withdrawn_at_ms: at,
            },
            false,
        ));
    }
    if recipient == HUMAN {
        return Ok((WithdrawOutcome::NotFound, false));
    }

    let deliveries = {
        let mut stmt = tx.prepare(
            "SELECT d.state, d.claim_owner_instance, d.first_claimed_at, \
                    r.instance_id, r.created_at, r.kind, r.note_or_reason \
             FROM wv_role_delivery d \
             LEFT JOIN wv_role_acceptance_receipt r ON r.delivery_id = d.delivery_id \
             WHERE d.message_id = ?1 ORDER BY d.delivery_id ASC",
        )?;
        let rows = stmt.query_map(params![message_id], |r| {
            Ok(DeliveryRow {
                state: r.get(0)?,
                claim_owner: r.get(1)?,
                first_claimed_at: r.get(2)?,
                receipt_instance: r.get(3)?,
                accepted_at: r.get(4)?,
                kind: r.get(5)?,
                note: r.get(6)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    if deliveries.is_empty() {
        // A plain message to a session: withdrawable until that session reads it.
        if read_at.is_some() {
            return Ok((
                WithdrawOutcome::TooLate {
                    state: InboxDeliveryState::Done,
                },
                false,
            ));
        }
    } else {
        // The furthest any delivery got decides; queued ones alone can be withdrawn.
        let furthest = deliveries
            .iter()
            .map(DeliveryRow::state)
            .max_by_key(|s| match s {
                InboxDeliveryState::Queued | InboxDeliveryState::Withdrawn => 0,
                InboxDeliveryState::Reading => 1,
                InboxDeliveryState::Accepted => 2,
                InboxDeliveryState::Done => 3,
            })
            .unwrap_or(InboxDeliveryState::Queued);
        if furthest != InboxDeliveryState::Queued {
            return Ok((WithdrawOutcome::TooLate { state: furthest }, false));
        }
        // A session whose handle is the role name takes the row through `read_messages`
        // too; once read that way it reached an agent, whatever its delivery says.
        if read_at.is_some() {
            return Ok((
                WithdrawOutcome::TooLate {
                    state: InboxDeliveryState::Reading,
                },
                false,
            ));
        }
        let removed = tx.execute(
            "DELETE FROM wv_role_delivery WHERE message_id=?1 AND state='pending' \
               AND NOT EXISTS (SELECT 1 FROM wv_role_acceptance_receipt r \
                               WHERE r.delivery_id = wv_role_delivery.delivery_id)",
            params![message_id],
        )?;
        if removed > 0 {
            write_audit_public(&tx, DEL, "mutate", 0)?;
        }
    }

    tx.execute(
        "UPDATE wv_inbox SET withdrawn_at=?2 WHERE id=?1 AND withdrawn_at IS NULL",
        params![message_id, now_ms],
    )?;
    write_audit_public(&tx, "wv_inbox", "mutate", message_id)?;
    tx.commit()?;
    Ok((
        WithdrawOutcome::Withdrawn {
            withdrawn_at_ms: now_ms,
        },
        true,
    ))
}

/// Insert a message into `wv_inbox` and (role form only) a matching
/// `wv_role_delivery` row in one IMMEDIATE transaction, with request-id
/// idempotency.
///
/// The caller supplies the full inbox payload (sender/recipient/text/
/// optional reply origin) and the role recipient. For role form, `recipient`
/// is the role name (same string used in `wv_role_delivery.role`) so human
/// readers of `wv_inbox` can still see who it was addressed to.
///
/// Returns the stored (message_id, delivery_id?). On a request-id replay the
/// stored values are returned without a new INSERT. On a request-id reused
/// with a different payload, [`RoleInboxError::RequestConflict`] is returned.
#[allow(clippy::too_many_arguments)]
pub fn send_role_message_commit(
    store: &WeftStore,
    sender: &str,
    role: &str,
    text: &str,
    in_reply_to: Option<i64>,
    in_reply_to_team: Option<&super::inbox::InboxReplyTeam>,
    request_id: Option<&str>,
    payload_hash: &str,
    now: i64,
    primary_file: Option<&str>,
    on_click: Option<&super::inbox::MessageOnClick>,
    content_type: Option<super::inbox::MessageContentType>,
) -> RoleInboxResult<(i64, i64)> {
    send_role_message_commit_with_reply_to(
        store,
        sender,
        role,
        text,
        in_reply_to,
        in_reply_to_team,
        request_id,
        payload_hash,
        now,
        primary_file,
        on_click,
        content_type,
        None,
        None,
    )
}

/// [`send_role_message_commit`] with a `reply_to` stored on the inbox row (where
/// replies to it go instead of `sender`), as the caller resolved it, and a `summary`
/// (the list preview, stored normalized). Both belong in `payload_hash` when a
/// request id is used.
#[allow(clippy::too_many_arguments)]
pub fn send_role_message_commit_with_reply_to(
    store: &WeftStore,
    sender: &str,
    role: &str,
    text: &str,
    in_reply_to: Option<i64>,
    in_reply_to_team: Option<&super::inbox::InboxReplyTeam>,
    request_id: Option<&str>,
    payload_hash: &str,
    now: i64,
    primary_file: Option<&str>,
    on_click: Option<&super::inbox::MessageOnClick>,
    content_type: Option<super::inbox::MessageContentType>,
    reply_to: Option<&str>,
    summary: Option<&str>,
) -> RoleInboxResult<(i64, i64)> {
    send_role_message_commit_outcome(
        store,
        sender,
        role,
        text,
        in_reply_to,
        in_reply_to_team,
        request_id,
        payload_hash,
        now,
        primary_file,
        on_click,
        content_type,
        reply_to,
        summary,
        None,
    )
    .map(|(message_id, delivery_id, _)| (message_id, delivery_id))
}

/// Includes whether this transaction inserted the message rather than replaying it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn send_role_message_commit_outcome(
    store: &WeftStore,
    sender: &str,
    role: &str,
    text: &str,
    in_reply_to: Option<i64>,
    in_reply_to_team: Option<&super::inbox::InboxReplyTeam>,
    request_id: Option<&str>,
    payload_hash: &str,
    now: i64,
    primary_file: Option<&str>,
    on_click: Option<&super::inbox::MessageOnClick>,
    content_type: Option<super::inbox::MessageContentType>,
    reply_to: Option<&str>,
    summary: Option<&str>,
    attachments: Option<&[crate::media::Attachment]>,
) -> RoleInboxResult<(i64, i64, bool)> {
    let role = stored_role(role)?;
    let reply_to = reply_to.map(str::trim).filter(|s| !s.is_empty());
    let summary = super::inbox::normalize_summary(summary);
    if text.trim().is_empty() && !attachments.is_some_and(|a| !a.is_empty()) {
        return Err(invalid("text must not be blank").into());
    }
    let sender = sender.trim();
    if sender.is_empty() {
        return Err(invalid("sender must not be blank").into());
    }
    let primary_file = primary_file.map(|s| s.trim()).filter(|s| !s.is_empty());
    let on_click_json = super::inbox::MessageOnClick::to_stored(on_click);
    let content_type_str = content_type.map(|c| match c {
        super::inbox::MessageContentType::Plain => "plain",
        super::inbox::MessageContentType::Markdown => "markdown",
        super::inbox::MessageContentType::Html => "html",
    });

    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    if let Some(req) = request_id {
        if !req.trim().is_empty() {
            if let Some((prev_msg, delivery_id)) = replay_message_request(&tx, req, payload_hash)? {
                tx.commit()?;
                return Ok((prev_msg, delivery_id, false));
            }
        }
    }

    // Insert the inbox row directly (we cannot call `inbox_insert` here — it
    // opens its own transaction). Mirror its secret-scan + text validation.
    let attachments_json = serde_json::to_string(attachments.unwrap_or_default()).unwrap();
    let stored_text = crate::shared::secret_scan::redact_secrets(text);
    let reply_org = in_reply_to_team.map(|t| t.org.as_str());
    let reply_team = in_reply_to_team.map(|t| t.team.as_str());
    let now_ms = chrono::Utc::now().timestamp_millis();
    tx.execute(
        "INSERT INTO wv_inbox \
         (sender, recipient, text, created_at, in_reply_to, in_reply_to_org, in_reply_to_team, primary_file, on_click, content_type, reply_to, summary, attachments) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![sender, role, stored_text, now_ms, in_reply_to, reply_org, reply_team, primary_file, on_click_json, content_type_str, reply_to, summary, attachments_json],
    )?;
    let message_id = tx.last_insert_rowid();
    write_audit_public(&tx, "wv_inbox", "append", message_id)?;

    tx.execute(
        "INSERT INTO wv_role_delivery (message_id, role, state, created_at) \
         VALUES (?1, ?2, 'pending', ?3)",
        params![message_id, role, now],
    )?;
    let delivery_id = tx.last_insert_rowid();
    write_audit_public(&tx, DEL, "append", delivery_id)?;

    if let Some(req) = request_id {
        if !req.trim().is_empty() {
            tx.execute(
                "INSERT INTO wv_send_message_request \
                 (request_id, payload_hash, message_id, delivery_id, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![req, payload_hash, message_id, delivery_id, now],
            )?;
            write_audit_public(&tx, REQ, "append", 0)?;
        }
    }

    tx.commit()?;
    Ok((message_id, delivery_id, true))
}

/// The roles any of `instances` holds a live registration for at `now` (epoch
/// seconds), sorted: by instance id, or by the tab session a registration with its
/// own instance id renews under. Reading this claims nothing.
pub fn roles_held_by(
    store: &WeftStore,
    instances: &[String],
    now: i64,
) -> WvWriteResult<Vec<String>> {
    if instances.is_empty() {
        return Ok(Vec::new());
    }
    let conn = store.conn()?;
    let marks = (2..2 + instances.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut stmt = conn.prepare(&format!(
        "SELECT DISTINCT role FROM wv_role_registration \
         WHERE (instance_id IN ({marks}) OR session_id IN ({marks})) AND expires_at > ?1 \
         ORDER BY role"
    ))?;
    let mut values: Vec<&dyn rusqlite::ToSql> = vec![&now];
    values.extend(instances.iter().map(|i| i as &dyn rusqlite::ToSql));
    let rows = stmt.query_map(values.as_slice(), |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn verify_registration(
    tx: &rusqlite::Transaction,
    role: &str,
    instance_id: &str,
    generation: i64,
    now: i64,
) -> RoleInboxResult<()> {
    let row: Option<(i64, i64)> = tx
        .query_row(
            "SELECT generation, expires_at FROM wv_role_registration \
             WHERE role=?1 AND instance_id=?2",
            params![role, instance_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match row {
        Some((g, exp)) if g == generation && exp > now => Ok(()),
        _ => Err(RoleInboxError::StaleRegistration),
    }
}

fn verify_claim(
    tx: &rusqlite::Transaction,
    delivery_id: i64,
    claim_token: &str,
    instance_id: &str,
    generation: i64,
    now: i64,
) -> RoleInboxResult<()> {
    type ClaimRow = (
        String,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<i64>,
    );
    let row: Option<ClaimRow> = tx
        .query_row(
            "SELECT state, claim_token, claim_owner_instance, claim_owner_generation, claim_expires_at \
             FROM wv_role_delivery WHERE delivery_id=?1",
            params![delivery_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                ))
            },
        )
        .optional()?;
    let (state, token, owner, owner_gen, expires_at) = match row {
        Some(r) => r,
        None => return Err(RoleInboxError::DeliveryNotFound),
    };
    // `claimed`, `accepted`, and `acknowledged` all preserve the claim fields,
    // so verifying under any of them is valid. `pending` is a fresh row with
    // NULL claim — the caller lost its claim through release/expiry.
    if state != "claimed" && state != "accepted" && state != "acknowledged" {
        return Err(RoleInboxError::StaleClaim);
    }
    let token = token.ok_or(RoleInboxError::StaleClaim)?;
    let owner = owner.ok_or(RoleInboxError::StaleClaim)?;
    let owner_gen = owner_gen.ok_or(RoleInboxError::StaleClaim)?;
    let expires_at = expires_at.ok_or(RoleInboxError::StaleClaim)?;
    if token != claim_token || owner != instance_id || owner_gen != generation {
        return Err(RoleInboxError::StaleClaim);
    }
    if expires_at <= now {
        return Err(RoleInboxError::StaleClaim);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::weft::store::WeftStore;
    use tempfile::TempDir;

    fn open() -> (WeftStore, TempDir) {
        let td = TempDir::new().unwrap();
        let path = td.path().join("weft.sqlite3");
        let store = WeftStore::open(path).unwrap();
        (store, td)
    }

    fn insert_pending(store: &WeftStore, role: &str, text: &str, now: i64) -> (i64, i64) {
        send_role_message_commit(
            store,
            "sender-agent",
            role,
            text,
            None,
            None,
            None,
            "nohash",
            now,
            None,
            None,
            None,
        )
        .unwrap()
    }

    #[test]
    fn a_role_prefix_names_the_same_queue() {
        assert_eq!(role_name(" role: reviewer "), "reviewer");
        assert_eq!(role_name("reviewer"), "reviewer");
        let (store, _td) = open();
        let now = 1_000_000;
        insert_pending(&store, "role:reviewer", "spelled as a Reply-To", now);
        let reg = register_role(&store, "reviewer", "inst-a", now).unwrap();
        assert_eq!(
            reg.pending_count, 1,
            "queued for `reviewer`, not `role:reviewer`"
        );
    }

    // One-holder roles: any role registered `exclusive`; no role name is special.

    fn exclusive() -> RegisterOptions {
        RegisterOptions {
            exclusive: true,
            ..RegisterOptions::default()
        }
    }

    fn held_by(session: &str, exclusive: bool) -> RegisterOptions {
        RegisterOptions {
            session_id: Some(session.to_string()),
            exclusive,
            ttl_secs: DEFAULT_REGISTRATION_TTL_SECS,
        }
    }

    #[test]
    fn an_exclusive_role_refuses_a_second_instance_while_the_first_is_live() {
        let (store, _td) = open();
        let now = 1_000_000;
        let a = register_role_as(&store, "triage", "inst-a", &exclusive(), now).unwrap();
        assert_eq!(a.generation, 1);
        assert!(a.exclusive);
        let b = register_role_as(&store, "triage", "inst-b", &exclusive(), now + 1);
        assert!(
            matches!(b, Err(RoleInboxError::AlreadyHeld { ref current_instance }) if current_instance == "inst-a"),
            "{b:?}"
        );
    }

    #[test]
    fn a_request_in_the_other_mode_conflicts_while_holders_are_live() {
        let (store, _td) = open();
        let now = 1_000_000;
        register_role_as(&store, "triage", "inst-a", &exclusive(), now).unwrap();
        let shared = register_role(&store, "triage", "inst-b", now + 1);
        assert!(matches!(
            shared,
            Err(RoleInboxError::ModeConflict {
                held_exclusive: true
            })
        ));

        register_role(&store, "review", "inst-a", now).unwrap();
        let one = register_role_as(&store, "review", "inst-b", &exclusive(), now + 1);
        assert!(matches!(
            one,
            Err(RoleInboxError::ModeConflict {
                held_exclusive: false
            })
        ));
    }

    #[test]
    fn the_mode_resets_once_no_holder_is_live() {
        let (store, _td) = open();
        let now = 1_000_000;
        let a = register_role(&store, "triage", "inst-a", now).unwrap();
        let later = a.expires_at + 1;
        let b = register_role_as(&store, "triage", "inst-b", &exclusive(), later).unwrap();
        assert!(b.exclusive);
    }

    #[test]
    fn foreman_is_an_ordinary_shared_role_unless_registered_exclusive() {
        let (store, _td) = open();
        let now = 1_000_000;
        register_role(&store, "foreman", "inst-a", now).unwrap();
        let b = register_role(&store, "foreman", "inst-b", now + 1).unwrap();
        assert!(!b.exclusive, "two shared foreman holders");
    }

    #[test]
    fn a_live_repeat_keeps_its_generation_and_a_lapsed_one_bumps_it() {
        let (store, _td) = open();
        let now = 1_000_000;
        let a = register_role_as(&store, "triage", "inst-a", &exclusive(), now).unwrap();
        let b = register_role_as(&store, "triage", "inst-a", &exclusive(), now + 5).unwrap();
        assert_eq!(a.generation, b.generation);
        assert!(b.expires_at > a.expires_at);
        let c = register_role_as(&store, "triage", "inst-a", &exclusive(), b.expires_at).unwrap();
        assert_eq!(c.generation, b.generation + 1);
    }

    #[test]
    fn an_exclusive_holder_is_replaced_after_expiry() {
        let (store, _td) = open();
        let now = 1_000_000;
        let a = register_role_as(&store, "triage", "inst-a", &exclusive(), now).unwrap();
        let later = a.expires_at + 1;
        let b = register_role_as(&store, "triage", "inst-b", &exclusive(), later).unwrap();
        assert!(b.generation >= 1);
    }

    #[test]
    fn two_sessions_racing_for_an_exclusive_role_give_one_holder() {
        let (store, td) = open();
        let path = td.path().join("weft.sqlite3");
        drop(store);
        let now = 1_000_000;
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let store = WeftStore::open(path).unwrap();
                    register_role_as(
                        &store,
                        "triage",
                        &format!("sess-{i}"),
                        &held_by(&format!("sess-{i}"), true),
                        now,
                    )
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let winners = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(winners, 1, "{results:?}");
        assert!(results
            .iter()
            .all(|r| r.is_ok() || matches!(r, Err(RoleInboxError::AlreadyHeld { .. }))));
    }

    // Session-held registrations: renewed while the session runs, released when it ends.

    fn live(_: &str) -> SessionLiveness {
        SessionLiveness::Live
    }

    fn registration(store: &WeftStore, role: &str, instance: &str) -> Option<(i64, i64)> {
        store
            .conn()
            .unwrap()
            .query_row(
                "SELECT generation, expires_at FROM wv_role_registration \
                 WHERE role=?1 AND instance_id=?2",
                params![role, instance],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .unwrap()
    }

    #[test]
    fn a_live_session_holds_its_role_for_five_minutes_with_no_agent_calls() {
        let (store, _td) = open();
        let t0 = 1_000_000;
        let (_m, delivery_id) = insert_pending(&store, "triage", "x", t0);
        let reg =
            register_role_as(&store, "triage", "sess-a", &held_by("sess-a", true), t0).unwrap();
        let claimed =
            claim_role_messages(&store, "triage", "sess-a", reg.generation, 10, t0).unwrap();
        let token = claimed[0].claim_token.clone();

        // Five minutes of 15s app ticks; the agent makes no call.
        let mut now = t0;
        while now < t0 + 300 {
            now += 15;
            let report = renew_session_registrations(&store, now, live).unwrap();
            assert_eq!(report.renewed, 1);
        }

        // Still the holder, same generation; a rival is refused.
        assert_eq!(
            registration(&store, "triage", "sess-a").unwrap().0,
            reg.generation
        );
        assert!(matches!(
            register_role_as(&store, "triage", "sess-b", &held_by("sess-b", true), now),
            Err(RoleInboxError::AlreadyHeld { .. })
        ));
        // Its claim and token stay valid: accept + ack with the original token.
        let receipt = accept_role_message(
            &store,
            delivery_id,
            &token,
            "sess-a",
            reg.generation,
            AcceptKind::Work,
            "n",
            now,
        )
        .unwrap();
        ack_role_message(
            &store,
            delivery_id,
            &token,
            "sess-a",
            reg.generation,
            &receipt,
            now,
        )
        .unwrap();
        // And it can keep claiming under the generation it registered with.
        claim_role_messages(&store, "triage", "sess-a", reg.generation, 10, now).unwrap();
    }

    #[test]
    fn renewal_keeps_generation_and_claim_tokens() {
        let (store, _td) = open();
        let now = 1_000_000;
        let (_m, delivery_id) = insert_pending(&store, "triage", "x", now);
        let reg =
            register_role_as(&store, "triage", "sess-a", &held_by("sess-a", false), now).unwrap();
        let claimed =
            claim_role_messages(&store, "triage", "sess-a", reg.generation, 10, now).unwrap();
        renew_session_registrations(&store, now + 15, live).unwrap();
        let row: (String, i64, i64) = store
            .conn()
            .unwrap()
            .query_row(
                "SELECT claim_token, claim_owner_generation, claim_expires_at \
                 FROM wv_role_delivery WHERE delivery_id=?1",
                params![delivery_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(row.0, claimed[0].claim_token);
        assert_eq!(row.1, reg.generation);
        assert_eq!(row.2, now + 15 + DEFAULT_REGISTRATION_TTL_SECS);
        assert_eq!(
            registration(&store, "triage", "sess-a"),
            Some((reg.generation, now + 15 + DEFAULT_REGISTRATION_TTL_SECS))
        );
    }

    #[test]
    fn an_ended_session_frees_its_role_in_one_tick_and_its_claims_go_pending() {
        let (store, _td) = open();
        let now = 1_000_000;
        let (_m, claimed_id) = insert_pending(&store, "triage", "claimed", now);
        let reg =
            register_role_as(&store, "triage", "sess-a", &held_by("sess-a", true), now).unwrap();
        let claimed =
            claim_role_messages(&store, "triage", "sess-a", reg.generation, 10, now).unwrap();
        assert_eq!(claimed.len(), 1);

        let report =
            renew_session_registrations(&store, now + 15, |_| SessionLiveness::Ended).unwrap();
        assert_eq!(report.released, 1);
        assert_eq!(report.released_claims, 1);
        assert_eq!(registration(&store, "triage", "sess-a"), None);
        let state: String = store
            .conn()
            .unwrap()
            .query_row(
                "SELECT state FROM wv_role_delivery WHERE delivery_id=?1",
                params![claimed_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "pending");

        // Another session takes the role and the work right away.
        let b = register_role_as(
            &store,
            "triage",
            "sess-b",
            &held_by("sess-b", true),
            now + 16,
        )
        .unwrap();
        let got =
            claim_role_messages(&store, "triage", "sess-b", b.generation, 10, now + 16).unwrap();
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn an_ended_sessions_accepted_work_is_adoptable_at_once() {
        let (store, _td) = open();
        let now = 1_000_000;
        let (_m, delivery_id) = insert_pending(&store, "triage", "x", now);
        let reg =
            register_role_as(&store, "triage", "sess-a", &held_by("sess-a", false), now).unwrap();
        let c = claim_role_messages(&store, "triage", "sess-a", reg.generation, 10, now).unwrap();
        let receipt = accept_role_message(
            &store,
            delivery_id,
            &c[0].claim_token,
            "sess-a",
            reg.generation,
            AcceptKind::Work,
            "n",
            now,
        )
        .unwrap();
        renew_session_registrations(&store, now + 15, |_| SessionLiveness::Ended).unwrap();
        let b = register_role_as(
            &store,
            "triage",
            "sess-b",
            &held_by("sess-b", false),
            now + 16,
        )
        .unwrap();
        assert_eq!(b.accepted_unacked.len(), 1);
        let got =
            claim_role_messages(&store, "triage", "sess-b", b.generation, 10, now + 16).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].existing_receipt.as_ref().unwrap().receipt_id,
            receipt
        );
    }

    #[test]
    fn a_remote_session_survives_a_relay_stall_and_lapses_after_180s_offline() {
        let (store, _td) = open();
        let t0 = 1_000_000;
        let remote = RegisterOptions {
            session_id: Some("remote-sess".into()),
            exclusive: true,
            ttl_secs: REMOTE_REGISTRATION_TTL_SECS,
        };
        let reg = register_role_as(&store, "triage", "remote-sess", &remote, t0).unwrap();
        // Live for a minute, then a 60s relay stall (machine unreachable), then back.
        let mut now = t0;
        for _ in 0..4 {
            now += 15;
            renew_session_registrations(&store, now, live).unwrap();
        }
        for _ in 0..4 {
            now += 15;
            renew_session_registrations(&store, now, |_| SessionLiveness::Unknown).unwrap();
        }
        assert!(
            claim_role_messages(&store, "triage", "remote-sess", reg.generation, 10, now).is_ok()
        );
        now += 15;
        assert_eq!(
            renew_session_registrations(&store, now, live)
                .unwrap()
                .renewed,
            1
        );

        // Now its machine goes away for good: still held just under 180s, gone after.
        let last_renewal = now;
        while now < last_renewal + REMOTE_REGISTRATION_TTL_SECS - 15 {
            now += 15;
            renew_session_registrations(&store, now, |_| SessionLiveness::Unknown).unwrap();
        }
        assert!(
            claim_role_messages(&store, "triage", "remote-sess", reg.generation, 10, now).is_ok()
        );
        now = last_renewal + REMOTE_REGISTRATION_TTL_SECS;
        let report =
            renew_session_registrations(&store, now, |_| SessionLiveness::Unknown).unwrap();
        assert_eq!(report.remaining, 0);
        assert!(matches!(
            claim_role_messages(&store, "triage", "remote-sess", reg.generation, 10, now),
            Err(RoleInboxError::StaleRegistration)
        ));
        // A lapsed registration is not revived when its machine returns.
        assert_eq!(
            renew_session_registrations(&store, now, live)
                .unwrap()
                .renewed,
            0
        );
        // Another session may take the role.
        register_role_as(&store, "triage", "other", &held_by("other", true), now).unwrap();
        assert_eq!(registration(&store, "triage", "remote-sess"), None);
    }

    #[test]
    fn a_registration_with_no_session_still_expires_at_60s() {
        let (store, _td) = open();
        let now = 1_000_000;
        let reg = register_role(&store, "triage", "script", now).unwrap();
        assert_eq!(reg.expires_at, now + DEFAULT_REGISTRATION_TTL_SECS);
        let mut t = now;
        while t < now + DEFAULT_REGISTRATION_TTL_SECS {
            t += 15;
            let report = renew_session_registrations(&store, t, live).unwrap();
            assert_eq!(report.renewed, 0);
        }
        assert!(matches!(
            claim_role_messages(&store, "triage", "script", reg.generation, 10, t),
            Err(RoleInboxError::StaleRegistration)
        ));
    }

    #[test]
    fn an_unknown_session_is_neither_renewed_nor_released() {
        let (store, _td) = open();
        let now = 1_000_000;
        let reg =
            register_role_as(&store, "triage", "sess-a", &held_by("sess-a", false), now).unwrap();
        let report =
            renew_session_registrations(&store, now + 15, |_| SessionLiveness::Unknown).unwrap();
        assert_eq!(
            (report.renewed, report.released, report.remaining),
            (0, 0, 1)
        );
        assert_eq!(
            registration(&store, "triage", "sess-a"),
            Some((reg.generation, reg.expires_at))
        );
    }

    // AC7: acceptance+ack idempotency, crash-recovery.

    #[test]
    fn accepting_work_writes_no_queue_row() {
        // After accept{kind=Work}, wv_queue is empty, the receipt's
        // queue_item_id is NULL, and the delivery is `accepted`. The
        // AcceptedWork + ReceiptSummary surfaces also carry None.
        let (store, _td) = open();
        let now = 1_000_000;
        let (_msg, delivery_id) = insert_pending(&store, "triage", "do stuff", now);
        let reg = register_role(&store, "triage", "inst-a", now).unwrap();
        let claimed =
            claim_role_messages(&store, "triage", "inst-a", reg.generation, 10, now).unwrap();
        let token = claimed[0].claim_token.clone();
        accept_role_message(
            &store,
            delivery_id,
            &token,
            "inst-a",
            reg.generation,
            AcceptKind::Work,
            "picked up",
            now + 1,
        )
        .unwrap();

        let conn = store.conn().unwrap();
        let queue_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM wv_queue", [], |r| r.get(0))
            .unwrap();
        assert_eq!(queue_rows, 0, "no wv_queue row is written for accept=work");
        let receipt_qid: Option<i64> = conn
            .query_row(
                "SELECT queue_item_id FROM wv_role_acceptance_receipt WHERE delivery_id=?1",
                params![delivery_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(receipt_qid, None, "receipt.queue_item_id is NULL");
        let state: String = conn
            .query_row(
                "SELECT state FROM wv_role_delivery WHERE delivery_id=?1",
                params![delivery_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "accepted");

        // A sibling instance's register_role surfaces the accepted
        // delivery in accepted_unacked with queue_item_id=None.
        let sibling = register_role(&store, "triage", "inst-b", now + 2).unwrap();
        let adopted = sibling
            .accepted_unacked
            .iter()
            .find(|w| w.delivery_id == delivery_id)
            .expect("register_role surfaces the accepted delivery");
        assert_eq!(adopted.queue_item_id, None);

        // existingReceipt (on reclaim-after-expiry) also carries None.
        let later = now + DEFAULT_CLAIM_TTL_SECS + 1;
        let c = register_role(&store, "triage", "inst-c", later).unwrap();
        let cc = claim_role_messages(&store, "triage", "inst-c", c.generation, 10, later).unwrap();
        assert_eq!(cc.len(), 1);
        let existing = cc[0].existing_receipt.as_ref().unwrap();
        assert_eq!(existing.queue_item_id, None);
    }

    #[test]
    fn accepted_work_is_never_claimable_from_wv_queue() {
        // Accept as work, ack, then an unscoped wv_claim_queue finds
        // nothing — there is no role-inbox row to claim.
        let (store, _td) = open();
        let now = 1_000_000;
        let (_msg, delivery_id) = insert_pending(&store, "triage", "x", now);
        let reg = register_role(&store, "triage", "inst-a", now).unwrap();
        let claimed =
            claim_role_messages(&store, "triage", "inst-a", reg.generation, 10, now).unwrap();
        let token = claimed[0].claim_token.clone();
        let receipt = accept_role_message(
            &store,
            delivery_id,
            &token,
            "inst-a",
            reg.generation,
            AcceptKind::Work,
            "n",
            now + 1,
        )
        .unwrap();
        ack_role_message(
            &store,
            delivery_id,
            &token,
            "inst-a",
            reg.generation,
            &receipt,
            now + 2,
        )
        .unwrap();

        let claimed = crate::weft::queue::claim(&store, "controller", None, now + 3).unwrap();
        assert_eq!(
            claimed, None,
            "an unscoped controller claim finds nothing — role work is not mirrored"
        );
    }

    #[test]
    fn accept_replay_is_idempotent_same_disposition() {
        let (store, _td) = open();
        let now = 1_000_000;
        let (_msg, delivery_id) = insert_pending(&store, "triage", "do stuff", now);
        let reg = register_role(&store, "triage", "inst-a", now).unwrap();
        let claimed =
            claim_role_messages(&store, "triage", "inst-a", reg.generation, 10, now).unwrap();
        assert_eq!(claimed.len(), 1);
        let token = claimed[0].claim_token.clone();
        let r1 = accept_role_message(
            &store,
            delivery_id,
            &token,
            "inst-a",
            reg.generation,
            AcceptKind::Work,
            "picked up",
            now + 1,
        )
        .unwrap();
        let r2 = accept_role_message(
            &store,
            delivery_id,
            &token,
            "inst-a",
            reg.generation,
            AcceptKind::Work,
            "picked up",
            now + 2,
        )
        .unwrap();
        assert_eq!(r1, r2);
    }

    #[test]
    fn accept_different_disposition_conflicts() {
        let (store, _td) = open();
        let now = 1_000_000;
        let (_msg, delivery_id) = insert_pending(&store, "triage", "do stuff", now);
        let reg = register_role(&store, "triage", "inst-a", now).unwrap();
        let claimed =
            claim_role_messages(&store, "triage", "inst-a", reg.generation, 10, now).unwrap();
        let token = claimed[0].claim_token.clone();
        let _ok = accept_role_message(
            &store,
            delivery_id,
            &token,
            "inst-a",
            reg.generation,
            AcceptKind::Work,
            "n",
            now + 1,
        )
        .unwrap();
        let r = accept_role_message(
            &store,
            delivery_id,
            &token,
            "inst-a",
            reg.generation,
            AcceptKind::NoWork,
            "n",
            now + 2,
        );
        assert!(matches!(
            r,
            Err(RoleInboxError::AcceptanceConflict {
                existing_kind: AcceptKind::Work
            })
        ));
    }

    #[test]
    fn ack_idempotent_and_rejects_wrong_receipt() {
        let (store, _td) = open();
        let now = 1_000_000;
        let (_msg, delivery_id) = insert_pending(&store, "triage", "x", now);
        let reg = register_role(&store, "triage", "inst-a", now).unwrap();
        let claimed =
            claim_role_messages(&store, "triage", "inst-a", reg.generation, 10, now).unwrap();
        let token = claimed[0].claim_token.clone();
        let receipt = accept_role_message(
            &store,
            delivery_id,
            &token,
            "inst-a",
            reg.generation,
            AcceptKind::Work,
            "n",
            now,
        )
        .unwrap();

        ack_role_message(
            &store,
            delivery_id,
            &token,
            "inst-a",
            reg.generation,
            &receipt,
            now + 1,
        )
        .unwrap();
        // Re-ack same receipt: idempotent.
        ack_role_message(
            &store,
            delivery_id,
            &token,
            "inst-a",
            reg.generation,
            &receipt,
            now + 2,
        )
        .unwrap();

        // Wrong receipt_id fails StaleAck.
        let r = ack_role_message(
            &store,
            delivery_id,
            &token,
            "inst-a",
            reg.generation,
            "not-the-receipt",
            now + 3,
        );
        assert!(matches!(r, Err(RoleInboxError::StaleAck)));
    }

    #[test]
    fn claim_with_wrong_generation_is_stale() {
        let (store, _td) = open();
        let now = 1_000_000;
        let _ = insert_pending(&store, "triage", "x", now);
        let reg = register_role(&store, "triage", "inst-a", now).unwrap();
        let r = claim_role_messages(&store, "triage", "inst-a", reg.generation + 99, 10, now);
        assert!(matches!(r, Err(RoleInboxError::StaleRegistration)));
    }

    #[test]
    fn accept_with_wrong_claim_token_is_stale() {
        let (store, _td) = open();
        let now = 1_000_000;
        let (_msg, delivery_id) = insert_pending(&store, "triage", "x", now);
        let reg = register_role(&store, "triage", "inst-a", now).unwrap();
        let _ = claim_role_messages(&store, "triage", "inst-a", reg.generation, 10, now).unwrap();
        let r = accept_role_message(
            &store,
            delivery_id,
            "not-the-token",
            "inst-a",
            reg.generation,
            AcceptKind::Work,
            "n",
            now,
        );
        assert!(matches!(r, Err(RoleInboxError::StaleClaim)));
    }

    // AC11: shared-role two instances race → one owner; release non-destructive;
    // work adoption on reclaim.

    #[test]
    fn shared_role_concurrent_claim_yields_one_owner_per_delivery() {
        let (store, _td) = open();
        let now = 1_000_000;
        let (_m, _d) = insert_pending(&store, "triage", "x", now);
        let a = register_role(&store, "triage", "inst-a", now).unwrap();
        let b = register_role(&store, "triage", "inst-b", now).unwrap();
        let ca = claim_role_messages(&store, "triage", "inst-a", a.generation, 10, now).unwrap();
        let cb = claim_role_messages(&store, "triage", "inst-b", b.generation, 10, now).unwrap();
        // One of them got it, the other got none.
        assert_eq!(ca.len() + cb.len(), 1);
    }

    #[test]
    fn release_returns_claims_to_pending_and_leaves_peers_intact() {
        let (store, _td) = open();
        let now = 1_000_000;
        let (_m, _d) = insert_pending(&store, "triage", "x", now);
        let a = register_role(&store, "triage", "inst-a", now).unwrap();
        let b = register_role(&store, "triage", "inst-b", now).unwrap();
        let claimed_a =
            claim_role_messages(&store, "triage", "inst-a", a.generation, 10, now).unwrap();
        assert_eq!(claimed_a.len(), 1);
        // Release inst-a — its claim becomes pending again, inst-b registration stays.
        let n = release_role(&store, "triage", "inst-a", a.generation).unwrap();
        assert_eq!(n, 1);
        let claimed_b =
            claim_role_messages(&store, "triage", "inst-b", b.generation, 10, now).unwrap();
        assert_eq!(claimed_b.len(), 1);
    }

    #[test]
    fn reclaim_after_expiry_adopts_existing_acceptance() {
        let (store, _td) = open();
        let now = 1_000_000;
        let (_m, delivery_id) = insert_pending(&store, "triage", "x", now);
        let a = register_role(&store, "triage", "inst-a", now).unwrap();
        let ca = claim_role_messages(&store, "triage", "inst-a", a.generation, 10, now).unwrap();
        let token_a = ca[0].claim_token.clone();
        let receipt = accept_role_message(
            &store,
            delivery_id,
            &token_a,
            "inst-a",
            a.generation,
            AcceptKind::Work,
            "n",
            now,
        )
        .unwrap();
        // inst-a crashes without ack; its claim expires. inst-b arrives and
        // reclaims — the existing receipt comes back on the claim row.
        let later = now + DEFAULT_CLAIM_TTL_SECS + 1;
        let b = register_role(&store, "triage", "inst-b", later).unwrap();
        let cb = claim_role_messages(&store, "triage", "inst-b", b.generation, 10, later).unwrap();
        assert_eq!(cb.len(), 1);
        let existing = cb[0].existing_receipt.as_ref().unwrap();
        assert_eq!(existing.receipt_id, receipt);
        assert_eq!(existing.kind, AcceptKind::Work);
    }

    // send_message role form.

    #[test]
    fn send_role_pending_delivery_recorded() {
        let (store, _td) = open();
        let now = 1_000_000;
        let (_msg, delivery_id) = insert_pending(&store, "triage", "hi", now);
        assert!(delivery_id > 0);
        let reg = register_role(&store, "triage", "inst-a", now).unwrap();
        assert_eq!(reg.pending_count, 1);
    }

    #[test]
    fn send_same_request_id_same_payload_replays() {
        let (store, _td) = open();
        let now = 1_000_000;
        let (m1, d1) = send_role_message_commit(
            &store,
            "sender-x",
            "triage",
            "hi",
            None,
            None,
            Some("req-1"),
            "hash-abc",
            now,
            None,
            None,
            None,
        )
        .unwrap();
        let (m2, d2) = send_role_message_commit(
            &store,
            "sender-x",
            "triage",
            "hi",
            None,
            None,
            Some("req-1"),
            "hash-abc",
            now + 1,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(m1, m2);
        assert_eq!(d1, d2);
    }

    #[test]
    fn send_same_request_id_different_payload_conflicts() {
        let (store, _td) = open();
        let now = 1_000_000;
        let _ = send_role_message_commit(
            &store,
            "sender-x",
            "triage",
            "hi",
            None,
            None,
            Some("req-1"),
            "hash-abc",
            now,
            None,
            None,
            None,
        )
        .unwrap();
        let r = send_role_message_commit(
            &store,
            "sender-x",
            "triage",
            "goodbye",
            None,
            None,
            Some("req-1"),
            "hash-xyz",
            now + 1,
            None,
            None,
            None,
        );
        assert!(matches!(r, Err(RoleInboxError::RequestConflict)));
    }

    // Phase 4: attach_file wiring tests.

    #[test]
    fn send_role_message_with_attach_file_sets_primary_file() {
        use crate::weft::inbox::inbox_get;
        let (store, _td) = open();
        let now = 1_000_000;
        let (msg, _d) = send_role_message_commit(
            &store,
            "sender-x",
            "triage",
            "take a look",
            None,
            None,
            None,
            "nohash",
            now,
            Some("docs/foo.md"),
            None,
            None,
        )
        .unwrap();
        let row = inbox_get(&store, msg).unwrap().unwrap();
        assert_eq!(row.primary_file.as_deref(), Some("docs/foo.md"));
    }

    #[test]
    fn send_role_message_without_attach_file_leaves_primary_file_null() {
        use crate::weft::inbox::inbox_get;
        let (store, _td) = open();
        let now = 1_000_000;
        let (msg, _d) = send_role_message_commit(
            &store,
            "sender-x",
            "triage",
            "no attachment",
            None,
            None,
            None,
            "nohash",
            now,
            None,
            None,
            None,
        )
        .unwrap();
        let row = inbox_get(&store, msg).unwrap().unwrap();
        assert_eq!(row.primary_file, None);
    }

    #[test]
    fn send_role_message_blank_attach_file_is_null() {
        use crate::weft::inbox::inbox_get;
        let (store, _td) = open();
        let now = 1_000_000;
        let (msg, _d) = send_role_message_commit(
            &store,
            "sender-x",
            "triage",
            "whitespace path",
            None,
            None,
            None,
            "nohash",
            now,
            Some("   "),
            None,
            None,
        )
        .unwrap();
        let row = inbox_get(&store, msg).unwrap().unwrap();
        assert_eq!(row.primary_file, None);
    }

    #[test]
    fn send_role_message_same_request_id_same_attach_replays() {
        let (store, _td) = open();
        let now = 1_000_000;
        let (m1, d1) = send_role_message_commit(
            &store,
            "sender-x",
            "triage",
            "look",
            None,
            None,
            Some("req-attach-1"),
            "hash-with-attach",
            now,
            Some("docs/foo.md"),
            None,
            None,
        )
        .unwrap();
        let (m2, d2) = send_role_message_commit(
            &store,
            "sender-x",
            "triage",
            "look",
            None,
            None,
            Some("req-attach-1"),
            "hash-with-attach",
            now + 1,
            Some("docs/foo.md"),
            None,
            None,
        )
        .unwrap();
        assert_eq!(m1, m2);
        assert_eq!(d1, d2);
    }

    #[test]
    fn a_claim_carries_the_message_text_sender_and_time() {
        let (store, _td) = open();
        let now = 1_000_000;
        let (msg, _d) = insert_pending(&store, "triage", "please review #12", now);
        let sent_at = crate::weft::inbox::inbox_get(&store, msg)
            .unwrap()
            .unwrap()
            .created_at_ms;
        let reg = register_role(&store, "triage", "inst-a", now).unwrap();
        let claimed =
            claim_role_messages(&store, "triage", "inst-a", reg.generation, 10, now).unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].text, "please review #12");
        assert_eq!(claimed[0].from, "sender-agent");
        assert_eq!(claimed[0].created_at_ms, sent_at);
        let wire = serde_json::to_value(&claimed[0]).unwrap();
        assert_eq!(wire["text"], "please review #12");
        assert_eq!(wire["from"], "sender-agent");
        assert_eq!(wire["createdAtMs"], sent_at);
    }

    /// An orphan delivery (its message row gone) never blocks the queue and never
    /// reaches an agent: the claim retires it and hands out the next message.
    #[test]
    fn an_orphan_delivery_does_not_block_the_claim() {
        let (store, _td) = open();
        let now = 1_000_000;
        let (orphan, orphan_delivery) = insert_pending(&store, "triage", "gone", now);
        let (_msg, _d2) = insert_pending(&store, "triage", "still here", now + 1);
        store
            .conn()
            .unwrap()
            .execute("DELETE FROM wv_inbox WHERE id=?1", params![orphan])
            .unwrap();
        let reg = register_role(&store, "triage", "inst-a", now).unwrap();
        let claimed =
            claim_role_messages(&store, "triage", "inst-a", reg.generation, 10, now).unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].text, "still here");
        let state: String = store
            .conn()
            .unwrap()
            .query_row(
                "SELECT state FROM wv_role_delivery WHERE delivery_id=?1",
                params![orphan_delivery],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "acknowledged");
        // A later claim finds nothing left behind it.
        assert!(
            claim_role_messages(&store, "triage", "inst-a", reg.generation, 10, now)
                .unwrap()
                .is_empty()
        );
    }

    /// With `limit: 1`, an orphan at the head of the queue must not use up the
    /// batch: the claim still returns the live message behind it.
    #[test]
    fn an_orphan_at_the_head_does_not_empty_a_limit_one_claim() {
        let (store, _td) = open();
        let now = 1_000_000;
        let (orphan, _orphan_delivery) = insert_pending(&store, "triage", "gone", now);
        let (_msg, _d2) = insert_pending(&store, "triage", "still here", now + 1);
        store
            .conn()
            .unwrap()
            .execute("DELETE FROM wv_inbox WHERE id=?1", params![orphan])
            .unwrap();
        let reg = register_role(&store, "triage", "inst-a", now).unwrap();
        let claimed =
            claim_role_messages(&store, "triage", "inst-a", reg.generation, 1, now).unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].text, "still here");
    }

    #[test]
    fn roles_held_by_lists_live_registrations_only() {
        let (store, _td) = open();
        let now = 1_000_000;
        register_role(&store, "reviewer", "H1", now).unwrap();
        register_role(&store, "qa", "S1", now).unwrap();
        register_role(&store, "architect", "H2", now).unwrap();
        let old = register_role(&store, "implementer", "H1", now - 1_000).unwrap();
        assert!(old.expires_at <= now);
        let held = roles_held_by(&store, &["H1".into(), "S1".into()], now).unwrap();
        assert_eq!(held, vec!["qa".to_string(), "reviewer".to_string()]);
        // A registration under its own instance id is held by the tab session it names.
        let options = RegisterOptions {
            session_id: Some("S1".into()),
            ..RegisterOptions::default()
        };
        register_role_as(&store, "triage", "custom-instance", &options, now).unwrap();
        let held = roles_held_by(&store, &["S1".into()], now).unwrap();
        assert_eq!(held, vec!["qa".to_string(), "triage".to_string()]);
        assert!(roles_held_by(&store, &[], now).unwrap().is_empty());
    }

    #[test]
    fn a_role_message_stays_pending_until_claimed() {
        let (store, _td) = open();
        let now = 1_000_000;
        let (msg, _d) = insert_pending(&store, "reviewer", "look", now);
        register_role(&store, "reviewer", "inst-a", now).unwrap();
        // Listing reads the queue without claiming it.
        let listed = crate::weft::inbox::inbox_list(&store).unwrap();
        assert_eq!(listed.len(), 1);
        let listed = crate::weft::inbox::inbox_list_for_session(
            &store,
            &["inst-a".into()],
            &["reviewer".into()],
        )
        .unwrap();
        assert_eq!(listed[0].id, msg);
        assert_eq!(listed[0].role.as_deref(), Some("reviewer"));
        let reg = register_role(&store, "reviewer", "inst-a", now + 1).unwrap();
        assert_eq!(reg.pending_count, 1, "reading claimed nothing");
    }

    // --- SW-BSI-2 Phase 4b: remote-tolerant TTLs --------------------------

    #[test]
    fn remote_registration_ttl_widens_the_expiry_window() {
        // Local registration uses the groomed 60s expiry; a relay-origin
        // registration uses the 180s expiry so one transient 20s renew miss
        // over an SSH stall does not orphan the registration.
        let (store, _td) = open();
        let now = 2_000_000;

        let local = register_role_with_ttl(
            &store,
            "triage",
            "local-worker",
            now,
            DEFAULT_REGISTRATION_TTL_SECS,
        )
        .unwrap();
        assert_eq!(local.expires_at, now + 60);

        let remote = register_role_with_ttl(
            &store,
            "triage",
            "remote-worker",
            now,
            REMOTE_REGISTRATION_TTL_SECS,
        )
        .unwrap();
        assert_eq!(remote.expires_at, now + 180);

        // The convenience `register_role` keeps the default 60s TTL for
        // backward compat with existing callers that cannot distinguish origin.
        let legacy = register_role(&store, "triage", "legacy-worker", now).unwrap();
        assert_eq!(legacy.expires_at, now + 60);
    }

    #[test]
    fn remote_claim_ttl_widens_the_expiry_window() {
        // Local claim uses the groomed 60s claim TTL; a relay-origin claim
        // uses the 180s claim TTL so one transient relay stall during
        // accept/ack does not flip the claim stale.
        let (store, _td) = open();
        let now = 3_000_000;
        let (_msg, _d) = insert_pending(&store, "triage", "please review", now);

        let reg = register_role_with_ttl(
            &store,
            "triage",
            "remote-worker",
            now,
            REMOTE_REGISTRATION_TTL_SECS,
        )
        .unwrap();

        let claimed = claim_role_messages_with_ttl(
            &store,
            "triage",
            "remote-worker",
            reg.generation,
            10,
            now,
            REMOTE_CLAIM_TTL_SECS,
        )
        .unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].claim_expires_at, now + 180);
    }

    #[test]
    fn local_claim_ttl_stays_at_the_groomed_default() {
        // Regression guard: shortening behavior for local callers would
        // silently widen the valid-claim window for an entire fleet.
        let (store, _td) = open();
        let now = 3_100_000;
        let (_msg, _d) = insert_pending(&store, "triage", "please review", now);

        let reg = register_role(&store, "triage", "local-worker", now).unwrap();
        let claimed =
            claim_role_messages(&store, "triage", "local-worker", reg.generation, 10, now).unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].claim_expires_at, now + DEFAULT_CLAIM_TTL_SECS,);
    }
}

#[cfg(test)]
mod delivery_tests {
    use super::*;
    use crate::weft::inbox::{
        inbox_insert, inbox_list, inbox_list_for_agent, inbox_take_unread, inbox_unread,
        InboxDelivery, InboxDeliveryState, HUMAN,
    };
    use tempfile::TempDir;

    const NOW: i64 = 1_000_000;

    fn open() -> (WeftStore, TempDir) {
        let td = TempDir::new().unwrap();
        let store = WeftStore::open(td.path().join("weft.sqlite3")).unwrap();
        (store, td)
    }

    fn send(store: &WeftStore, role: &str, text: &str) -> (i64, i64) {
        send_role_message_commit(
            store, HUMAN, role, text, None, None, None, "h", NOW, None, None, None,
        )
        .unwrap()
    }

    fn delivery_of(store: &WeftStore, message_id: i64) -> Option<InboxDelivery> {
        inbox_list(store)
            .unwrap()
            .into_iter()
            .find(|m| m.id == message_id)
            .unwrap()
            .delivery
    }

    fn claim(store: &WeftStore, role: &str, instance: &str, now: i64) -> Vec<ClaimedDelivery> {
        let reg = register_role(store, role, instance, now).unwrap();
        claim_role_messages(store, role, instance, reg.generation, 10, now).unwrap()
    }

    #[test]
    fn a_role_message_is_queued_until_picked_up() {
        let (store, _td) = open();
        let (msg, _) = send(&store, "triage", "look at this");
        let d = delivery_of(&store, msg).unwrap();
        assert_eq!(d.state, InboxDeliveryState::Queued);
        assert_eq!(d.instance, None);
        assert_eq!(d.picked_up_at_ms, None);
        // The agent's own list read sees the same state.
        let own = inbox_list_for_agent(&store, "triage", &[]).unwrap();
        assert_eq!(
            own[0].delivery.as_ref().unwrap().state,
            InboxDeliveryState::Queued
        );
    }

    #[test]
    fn a_claimed_message_is_being_read_by_its_instance() {
        let (store, _td) = open();
        let (msg, _) = send(&store, "triage", "look at this");
        assert_eq!(claim(&store, "triage", "sess-1", NOW).len(), 1);
        let d = delivery_of(&store, msg).unwrap();
        assert_eq!(d.state, InboxDeliveryState::Reading);
        assert_eq!(d.instance.as_deref(), Some("sess-1"));
        assert_eq!(d.picked_up_at_ms, Some(NOW * 1000));
    }

    #[test]
    fn accepted_then_acknowledged_carries_the_receipt() {
        let (store, _td) = open();
        let (msg, delivery) = send(&store, "triage", "look at this");
        let claimed = claim(&store, "triage", "sess-1", NOW);
        let token = claimed[0].claim_token.clone();
        let gen = 1;
        let receipt = accept_role_message(
            &store,
            delivery,
            &token,
            "sess-1",
            gen,
            AcceptKind::NoWork,
            "already fixed",
            NOW + 5,
        )
        .unwrap();
        let d = delivery_of(&store, msg).unwrap();
        assert_eq!(d.state, InboxDeliveryState::Accepted);
        assert_eq!(d.accepted_at_ms, Some((NOW + 5) * 1000));
        assert_eq!(d.disposition.as_deref(), Some("no_work"));
        assert_eq!(d.note.as_deref(), Some("already fixed"));
        assert_eq!(d.instance.as_deref(), Some("sess-1"));

        ack_role_message(&store, delivery, &token, "sess-1", gen, &receipt, NOW + 6).unwrap();
        let d = delivery_of(&store, msg).unwrap();
        assert_eq!(d.state, InboxDeliveryState::Done);
        assert_eq!(d.instance.as_deref(), Some("sess-1"));
        assert_eq!(d.picked_up_at_ms, Some(NOW * 1000));
    }

    #[test]
    fn a_reclaim_after_expiry_stays_accepted_and_keeps_the_first_pickup() {
        let (store, _td) = open();
        let (msg, delivery) = send(&store, "triage", "look at this");
        let claimed = claim(&store, "triage", "sess-1", NOW);
        accept_role_message(
            &store,
            delivery,
            &claimed[0].claim_token,
            "sess-1",
            1,
            AcceptKind::Work,
            "",
            NOW + 1,
        )
        .unwrap();
        // The first instance dies; another adopts the accepted work after expiry.
        let later = NOW + DEFAULT_CLAIM_TTL_SECS + DEFAULT_REGISTRATION_TTL_SECS + 10;
        let re = claim(&store, "triage", "sess-2", later);
        assert_eq!(re.len(), 1);
        assert!(re[0].existing_receipt.is_some());
        let d = delivery_of(&store, msg).unwrap();
        assert_eq!(d.state, InboxDeliveryState::Accepted);
        assert_eq!(d.instance.as_deref(), Some("sess-1"));
        assert_eq!(d.picked_up_at_ms, Some(NOW * 1000));
        assert_eq!(d.note, None);
    }

    #[test]
    fn a_released_claim_is_queued_again_but_remembers_the_pickup() {
        let (store, _td) = open();
        let (msg, _) = send(&store, "triage", "look at this");
        claim(&store, "triage", "sess-1", NOW);
        assert_eq!(release_role(&store, "triage", "sess-1", 1).unwrap(), 1);
        let d = delivery_of(&store, msg).unwrap();
        assert_eq!(d.state, InboxDeliveryState::Queued);
        assert_eq!(d.instance, None);
        assert_eq!(d.picked_up_at_ms, Some(NOW * 1000));
    }

    #[test]
    fn a_plain_message_has_no_delivery() {
        let (store, _td) = open();
        let id = inbox_insert(
            &store, "sess-1", HUMAN, "done", None, None, None, None, None,
        )
        .unwrap();
        assert_eq!(delivery_of(&store, id), None);
    }

    #[test]
    fn withdrawing_a_queued_message_hides_it_from_every_agent_path() {
        let (store, _td) = open();
        let (msg, _) = send(&store, "triage", "never mind");
        let out = withdraw_message(&store, msg, 42_000).unwrap();
        assert_eq!(
            out,
            WithdrawOutcome::Withdrawn {
                withdrawn_at_ms: 42_000
            }
        );

        let d = delivery_of(&store, msg).unwrap();
        assert_eq!(d.state, InboxDeliveryState::Withdrawn);
        assert_eq!(d.withdrawn_at_ms, Some(42_000));
        // No claim, no unread, no take returns it.
        assert!(claim(&store, "triage", "sess-1", NOW).is_empty());
        assert_eq!(inbox_unread(&store, "triage").unwrap().count, 0);
        assert!(inbox_take_unread(&store, "triage").unwrap().is_empty());
        // A later claim far in the future (any expiry reset) still finds nothing.
        assert!(claim(&store, "triage", "sess-2", NOW + 10_000).is_empty());
        // Registration reports nothing pending.
        let reg = register_role(&store, "triage", "sess-3", NOW + 20_000).unwrap();
        assert_eq!(reg.pending_count, 0);
    }

    #[test]
    fn withdrawing_twice_returns_the_first_time() {
        let (store, _td) = open();
        let (msg, _) = send(&store, "triage", "never mind");
        withdraw_message(&store, msg, 42_000).unwrap();
        assert_eq!(
            withdraw_message(&store, msg, 99_000).unwrap(),
            WithdrawOutcome::Withdrawn {
                withdrawn_at_ms: 42_000
            }
        );
    }

    #[test]
    fn withdrawing_after_pickup_is_too_late_at_each_stage() {
        let (store, _td) = open();
        let (msg, delivery) = send(&store, "triage", "look");
        let claimed = claim(&store, "triage", "sess-1", NOW);
        assert_eq!(
            withdraw_message(&store, msg, 1).unwrap(),
            WithdrawOutcome::TooLate {
                state: InboxDeliveryState::Reading
            }
        );
        let token = claimed[0].claim_token.clone();
        let receipt = accept_role_message(
            &store,
            delivery,
            &token,
            "sess-1",
            1,
            AcceptKind::Work,
            "",
            NOW,
        )
        .unwrap();
        assert_eq!(
            withdraw_message(&store, msg, 1).unwrap(),
            WithdrawOutcome::TooLate {
                state: InboxDeliveryState::Accepted
            }
        );
        ack_role_message(&store, delivery, &token, "sess-1", 1, &receipt, NOW).unwrap();
        assert_eq!(
            withdraw_message(&store, msg, 1).unwrap(),
            WithdrawOutcome::TooLate {
                state: InboxDeliveryState::Done
            }
        );
        assert_eq!(
            delivery_of(&store, msg).unwrap().state,
            InboxDeliveryState::Done
        );
    }

    #[test]
    fn a_role_message_read_by_a_session_under_the_role_name_is_too_late() {
        let (store, _td) = open();
        let (msg, _) = send(&store, "triage", "look");
        // A session whose handle is the role name reads its unread mail, role rows included.
        assert_eq!(inbox_take_unread(&store, "triage").unwrap().len(), 1);
        assert_eq!(
            withdraw_message(&store, msg, 1).unwrap(),
            WithdrawOutcome::TooLate {
                state: InboxDeliveryState::Reading
            }
        );
    }

    #[test]
    fn a_released_message_can_be_withdrawn() {
        let (store, _td) = open();
        let (msg, _) = send(&store, "triage", "look");
        claim(&store, "triage", "sess-1", NOW);
        release_role(&store, "triage", "sess-1", 1).unwrap();
        assert!(matches!(
            withdraw_message(&store, msg, 7).unwrap(),
            WithdrawOutcome::Withdrawn { .. }
        ));
    }

    #[test]
    fn a_plain_message_to_a_session_is_withdrawable_until_read() {
        let (store, _td) = open();
        let unread = inbox_insert(
            &store, HUMAN, "sess-1", "hold on", None, None, None, None, None,
        )
        .unwrap();
        assert!(matches!(
            withdraw_message(&store, unread, 5).unwrap(),
            WithdrawOutcome::Withdrawn { .. }
        ));
        assert!(inbox_take_unread(&store, "sess-1").unwrap().is_empty());

        let read =
            inbox_insert(&store, HUMAN, "sess-1", "go", None, None, None, None, None).unwrap();
        assert_eq!(inbox_take_unread(&store, "sess-1").unwrap().len(), 1);
        assert_eq!(
            withdraw_message(&store, read, 5).unwrap(),
            WithdrawOutcome::TooLate {
                state: InboxDeliveryState::Done
            }
        );
    }

    #[test]
    fn nothing_to_withdraw_for_the_human_or_an_unknown_id() {
        let (store, _td) = open();
        let to_human = inbox_insert(
            &store, "sess-1", HUMAN, "question", None, None, None, None, None,
        )
        .unwrap();
        assert_eq!(
            withdraw_message(&store, to_human, 1).unwrap(),
            WithdrawOutcome::NotFound
        );
        assert_eq!(
            withdraw_message(&store, 9_999, 1).unwrap(),
            WithdrawOutcome::NotFound
        );
    }

    #[test]
    fn the_outcomes_serialize_as_the_app_contract() {
        let json = |o: WithdrawOutcome| serde_json::to_value(o).unwrap();
        assert_eq!(
            json(WithdrawOutcome::Withdrawn { withdrawn_at_ms: 3 }),
            serde_json::json!({ "status": "withdrawn", "withdrawnAtMs": 3 })
        );
        assert_eq!(
            json(WithdrawOutcome::TooLate {
                state: InboxDeliveryState::Reading
            }),
            serde_json::json!({ "status": "tooLate", "state": "reading" })
        );
        assert_eq!(
            json(WithdrawOutcome::NotFound),
            serde_json::json!({ "status": "notFound" })
        );
        let d = InboxDelivery {
            state: InboxDeliveryState::Accepted,
            picked_up_at_ms: Some(1),
            instance: Some("s".into()),
            instance_session: Some("sess".into()),
            accepted_at_ms: Some(2),
            disposition: Some("work".into()),
            note: None,
            withdrawn_at_ms: None,
        };
        assert_eq!(
            serde_json::to_value(d).unwrap(),
            serde_json::json!({
                "state": "accepted", "pickedUpAtMs": 1, "instance": "s", "acceptedAtMs": 2,
                "disposition": "work", "note": null, "withdrawnAtMs": null
            })
        );
    }

    /// The sender withdraws while an instance claims, on two connections to one store:
    /// exactly one wins, and the store agrees with the winner.
    #[test]
    fn withdraw_and_claim_race_has_exactly_one_winner() {
        use std::sync::{Arc, Barrier};
        let td = TempDir::new().unwrap();
        let path = td.path().join("weft.sqlite3");
        for round in 0..50 {
            let setup = WeftStore::open(&path).unwrap();
            let role = format!("race-{round}");
            let (msg, _) = send(&setup, &role, "contested");
            let reg = register_role(&setup, &role, "sess-1", NOW).unwrap();
            let barrier = Arc::new(Barrier::new(2));

            let (p1, r1, b1) = (path.clone(), role.clone(), barrier.clone());
            let claimer = std::thread::spawn(move || {
                let store = WeftStore::open(p1).unwrap();
                b1.wait();
                claim_role_messages(&store, &r1, "sess-1", reg.generation, 10, NOW).unwrap()
            });
            let (p2, b2) = (path.clone(), barrier.clone());
            let withdrawer = std::thread::spawn(move || {
                let store = WeftStore::open(p2).unwrap();
                b2.wait();
                withdraw_message(&store, msg, 5).unwrap()
            });
            let claimed = claimer.join().unwrap();
            let withdrawn = withdrawer.join().unwrap();

            let state = delivery_of(&setup, msg).unwrap().state;
            match withdrawn {
                WithdrawOutcome::Withdrawn { .. } => {
                    assert!(
                        claimed.is_empty(),
                        "round {round}: claimed a withdrawn message"
                    );
                    assert_eq!(state, InboxDeliveryState::Withdrawn);
                }
                WithdrawOutcome::TooLate { state: seen } => {
                    assert_eq!(
                        claimed.len(),
                        1,
                        "round {round}: too late but nobody claimed"
                    );
                    assert_eq!(seen, InboxDeliveryState::Reading);
                    assert_eq!(state, InboxDeliveryState::Reading);
                }
                WithdrawOutcome::NotFound => panic!("round {round}: message vanished"),
            }
        }
    }
}

pub(crate) fn replay_message_request(
    conn: &rusqlite::Connection,
    request_id: &str,
    payload_hash: &str,
) -> RoleInboxResult<Option<(i64, i64)>> {
    let found: Option<(String,i64,Option<i64>)> = conn.query_row("SELECT payload_hash, message_id, delivery_id FROM wv_send_message_request WHERE request_id=?1", params![request_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    match found {
        None => Ok(None),
        Some((hash, id, Some(delivery))) if hash == payload_hash => Ok(Some((id, delivery))),
        _ => Err(RoleInboxError::RequestConflict),
    }
}
