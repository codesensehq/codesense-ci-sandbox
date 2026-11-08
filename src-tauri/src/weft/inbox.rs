// SPDX-License-Identifier: MIT
//! The durable inbox agents and the human message each other through (`wv_inbox`).
//!
//! A message goes from one address to another. An address is a session handle or
//! [`HUMAN`]. Nothing here decides who may write to whom: the only refusal is input the
//! store cannot hold (blank text). A reply records the message it answers, which may
//! live in another team's store.
//!
//! Every write co-commits its `wv_write_log` audit row on one IMMEDIATE transaction,
//! like every other `wv_*` ledger. All functions are synchronous SQLite; callers run
//! them on the blocking pool.

use rusqlite::{params, OptionalExtension, TransactionBehavior};

use super::sqlite::{is_blank, write_audit_public, wv_auto_append, WvValidation, WvWriteResult};
use super::store::WeftStore;

const INBOX: &str = "wv_inbox";
const ANNOUNCED: &str = "wv_inbox_notified";

/// The address of the person using CodeSense (also the sender of a message from a
/// caller that has no session handle).
pub const HUMAN: &str = "human";

/// `wv_inbox.via` of a row recording text typed into the recipient's terminal.
pub const VIA_TERMINAL: &str = "terminal";

/// How many of the newest rows [`inbox_list`] returns.
pub const INBOX_LIST_LIMIT: u32 = 500;

/// How a message body should be rendered. Stored as the lowercase variant name
/// on `wv_inbox.content_type`; omitted/`None` means the FE renderer applies its
/// default (`markdown` — see `DESIGN-inbox-ux.md` §3).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum MessageContentType {
    Plain,
    Markdown,
    Html,
}

impl MessageContentType {
    pub(crate) fn as_stored(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Markdown => "markdown",
            Self::Html => "html",
        }
    }

    fn from_stored(raw: Option<&str>) -> Option<Self> {
        match raw.map(str::trim).unwrap_or("") {
            "plain" => Some(Self::Plain),
            "markdown" => Some(Self::Markdown),
            "html" => Some(Self::Html),
            _ => None,
        }
    }
}

/// What a message asks of the human, for agent views and dashboards. The human
/// inbox shows both kinds as conversations and counts all unread human mail. Stored as given on `wv_inbox.kind` (NULL when the sender set none); reads
/// resolve the effective kind with [`InboxMessage::effective_kind`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum MessageKind {
    /// Waits on the human: a question, a decision, something only they can unblock.
    NeedsYou,
    /// Informational: progress, "merged", "done".
    Update,
}

impl MessageKind {
    pub(crate) fn as_stored(self) -> &'static str {
        match self {
            Self::NeedsYou => "needs-you",
            Self::Update => "update",
        }
    }

    pub(crate) fn from_stored(raw: Option<&str>) -> Option<Self> {
        match raw.map(str::trim).unwrap_or("") {
            "needs-you" => Some(Self::NeedsYou),
            "update" => Some(Self::Update),
            _ => None,
        }
    }

    /// The kind a message counts as. A row typed into a terminal (`via`) is always an
    /// update: it is a record, not mail. Otherwise the sender's own kind wins. Without
    /// one, a message whose sender set a Reply-To naming someone other than the human
    /// and itself (`reply_to`: the documented way to ask a question whose answer must
    /// reach someone) waits on the human; anything else is an update. Defaulting to
    /// `update` distinguishes reports from questions for agent views and dashboards.
    /// An agent that needs an answer has `ask_user` and `kind: "needs-you"`.
    pub fn effective(
        stored: Option<Self>,
        reply_to: Option<&str>,
        via: Option<&str>,
        from: &str,
    ) -> Self {
        Self::resolve(stored, reply_to, via, from, &|_| false)
    }

    /// [`MessageKind::effective`], where `holds_role(role)` says whether the sender holds
    /// (or held) role `role`: a Reply-To naming the sender's own role (a manager's digest
    /// with `replyTo: "role:manager"`) asks nothing of anyone else, as one naming the
    /// sender itself does.
    pub fn resolve(
        stored: Option<Self>,
        reply_to: Option<&str>,
        via: Option<&str>,
        from: &str,
        holds_role: &dyn Fn(&str) -> bool,
    ) -> Self {
        if via.is_some_and(|v| !v.trim().is_empty()) {
            return Self::Update;
        }
        if let Some(kind) = stored {
            return kind;
        }
        let Some(r) = reply_to.map(str::trim).filter(|r| asks_elsewhere(r)) else {
            return Self::Update;
        };
        let own = match ReplyTo::parse(r) {
            ReplyTo::Role(role) => holds_role(super::role_inbox::role_name(role)),
            ReplyTo::Address(address) => address == from.trim(),
        };
        if own {
            Self::Update
        } else {
            Self::NeedsYou
        }
    }
}

/// Who holds or held each role in one store, as recorded there: every registration
/// (its instance id and its session), every claim owner and every acceptance receipt.
/// Receipts outlive the queue, so a role holder stays known after it released the role.
/// Ids are instance ids, usually tab session ids.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoleHolders(std::collections::HashMap<String, std::collections::HashSet<String>>);

impl RoleHolders {
    /// Whether any of `ids` (one sender's handle and tab session) holds or held `role`.
    pub fn any_holds(&self, role: &str, ids: &[&str]) -> bool {
        self.0
            .get(role)
            .is_some_and(|held| ids.iter().any(|id| held.contains(id.trim())))
    }
}

/// [`RoleHolders`] of `store`.
pub fn role_holders(store: &WeftStore) -> WvWriteResult<RoleHolders> {
    let conn = store.conn()?;
    let mut holders: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT role, instance_id FROM wv_role_registration \
         UNION SELECT role, session_id FROM wv_role_registration WHERE session_id IS NOT NULL \
         UNION SELECT role, claim_owner_instance FROM wv_role_delivery \
               WHERE claim_owner_instance IS NOT NULL \
         UNION SELECT role, instance_id FROM wv_role_acceptance_receipt",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (role, id) = row?;
        let id = id.trim();
        if !id.is_empty() {
            holders.entry(role).or_default().insert(id.to_string());
        }
    }
    Ok(RoleHolders(holders))
}

/// A Reply-To that sends the answer somewhere: not blank, and not the human (who is the
/// one answering), however it is spelled (`human`, `role:human`).
fn asks_elsewhere(reply_to: &str) -> bool {
    let r = reply_to.trim();
    let bare = r.strip_prefix("role:").unwrap_or(r).trim();
    !bare.is_empty() && !bare.eq_ignore_ascii_case(HUMAN)
}

/// Per-message click action attached by the sender. Stored verbatim as JSON on
/// `wv_inbox.on_click`; omitted/`None` is the ordinary "open the thread" default.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum MessageOnClick {
    /// Click the row → focus the agent's running terminal (`agents-list.tsx`
    /// `onLocate`). The id is the agent definition id (`agents_list` row key —
    /// the sender picks the agent to jump to; the UI resolves to a running
    /// session at click time).
    GotoAgent { agent_id: String },
    /// Click the row → open `path` in the viewer. The host validates the path
    /// against the sender's team content root before opening (`openFileInTeam`).
    OpenFile { path: String },
    /// Explicit default — same as omitting the field. Kept so an agent that
    /// wants the row to open the message view can say so.
    ShowMessage,
}

impl MessageOnClick {
    /// Serialize to the JSON string stored on `wv_inbox.on_click`. `None` →
    /// `None` so SQLite stores NULL.
    pub fn to_stored(this: Option<&Self>) -> Option<String> {
        this.map(|v| serde_json::to_string(v).unwrap_or_default())
            .filter(|s| !s.is_empty())
    }

    /// Parse a stored JSON blob. A legacy row with no column, a null, or a
    /// malformed value all read as `None` — a corrupt click action is never a
    /// listing failure.
    pub fn from_stored(raw: Option<String>) -> Option<Self> {
        raw.filter(|s| !s.trim().is_empty())
            .and_then(|s| serde_json::from_str::<Self>(&s).ok())
    }
}

/// Portable identity of the inbox holding a reply's original message.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct InboxReplyTeam {
    pub org: String,
    pub team: String,
}

/// One `wv_inbox` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxMessage {
    pub id: i64,
    pub from: String,
    pub to: String,
    pub text: String,
    pub created_at_ms: i64,
    pub read_at_ms: Option<i64>,
    pub in_reply_to: Option<i64>,
    pub in_reply_to_team: Option<InboxReplyTeam>,
    /// SW-BSI-2: optional managed-file reference the human previews. The path is a
    /// managed logical path; conversation identity is independent of file identity.
    pub primary_file: Option<String>,
    pub attachments: Vec<crate::media::Attachment>,
    /// SW-BSI-UX: the human archived this message. Distinct from read/work-acceptance;
    /// a human organizational signal. Null = not archived.
    pub archived_at_ms: Option<i64>,
    /// When the sender withdrew it before any agent picked it up. A withdrawn row never
    /// reaches an agent.
    pub withdrawn_at_ms: Option<i64>,
    /// Where a role-addressed (or withdrawn) message stands. Filled by the list reads
    /// only; `None` elsewhere and for a plain message nobody withdrew.
    pub delivery: Option<InboxDelivery>,
    /// Per-message click action chosen by the sender (goto-agent / open-file /
    /// show-message). `None` is the ordinary "open the thread" default.
    pub on_click: Option<MessageOnClick>,
    /// How to render the body (`plain` / `markdown` / `html`). `None` → the FE
    /// renderer applies its default (`markdown`). The FE sanitizes `html`.
    pub content_type: Option<MessageContentType>,
    /// [`VIA_TERMINAL`] when the text was typed into the recipient's terminal rather
    /// than delivered through the inbox; `None` for ordinary mail.
    pub via: Option<String>,
    /// The role queue the message was addressed to. Filled by the list reads only,
    /// from its delivery row; `None` elsewhere and for mail to a session or the human.
    pub role: Option<String>,
    /// Where replies to this message go instead of its sender (email Reply-To): a
    /// session handle, [`HUMAN`], or `role:<id>` (see [`ReplyTo`]). `None` → the sender.
    pub reply_to: Option<String>,
    /// The sender's one-line plain-text preview, shown in list views instead of the
    /// body (raw html/markdown previews badly). Stored normalized by
    /// [`normalize_summary`]; `None` → the list derives a preview from the body.
    pub summary: Option<String>,
    /// SW-AGR-18: the kind the sender set, as stored; `None` when it set none. Lanes use
    /// [`InboxMessage::effective_kind`].
    pub kind: Option<MessageKind>,
}

impl InboxMessage {
    /// The kind this message counts as (see [`MessageKind::effective`]).
    pub fn effective_kind(&self) -> MessageKind {
        MessageKind::effective(
            self.kind,
            self.reply_to.as_deref(),
            self.via.as_deref(),
            &self.from,
        )
    }
}

/// The longest stored [`InboxMessage::summary`], in chars (an ellipsis included).
pub const SUMMARY_MAX_CHARS: usize = 280;

/// Invisible format characters a summary may not carry: zero-width marks and the
/// bidi embeddings, overrides and isolates (U+200B..U+200F, U+202A..U+202E,
/// U+2066..U+2069) and the BOM. They would let a one-line preview visually reorder or
/// spoof the text around it, such as the sender's name on the same row.
fn is_spoofing_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}'
    )
}

/// A sender's summary as the store keeps it: secrets redacted like the body, control
/// and bidi/zero-width format characters dropped, whitespace runs (newlines
/// included) collapsed to one space, trimmed, and capped at [`SUMMARY_MAX_CHARS`]
/// chars with a trailing `…` when cut. Blank → `None`.
pub fn normalize_summary(raw: Option<&str>) -> Option<String> {
    let visible: String = crate::shared::secret_scan::redact_secrets(raw?)
        .chars()
        .filter(|&c| (c.is_whitespace() || !c.is_control()) && !is_spoofing_format_char(c))
        .collect();
    let mut words = String::new();
    for word in visible.split_whitespace() {
        if !words.is_empty() {
            words.push(' ');
        }
        words.push_str(word);
    }
    if words.is_empty() {
        return None;
    }
    if words.chars().count() <= SUMMARY_MAX_CHARS {
        return Some(words);
    }
    let mut cut: String = words.chars().take(SUMMARY_MAX_CHARS - 1).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    Some(cut)
}

/// The prefix that marks a [`InboxMessage::reply_to`] as a role queue.
pub const REPLY_TO_ROLE_PREFIX: &str = "role:";

/// A stored `reply_to`, read: a role queue or a plain address (a session handle or
/// [`HUMAN`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyTo<'a> {
    Role(&'a str),
    Address(&'a str),
}

impl<'a> ReplyTo<'a> {
    pub fn parse(stored: &'a str) -> Self {
        match stored.strip_prefix(REPLY_TO_ROLE_PREFIX) {
            Some(role) => ReplyTo::Role(role.trim()),
            None => ReplyTo::Address(stored.trim()),
        }
    }
}

/// How far an agent got with a message sent to its role inbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InboxDeliveryState {
    /// No running instance holds it.
    Queued,
    /// An instance claimed it and is reading it.
    Reading,
    /// An instance accepted it, as work or as no work.
    Accepted,
    /// The accepting instance acknowledged it.
    Done,
    /// The sender cancelled it before anyone picked it up.
    Withdrawn,
}

/// One message's delivery, as the human's Messages list shows it. Times are epoch ms.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxDelivery {
    pub state: InboxDeliveryState,
    /// When an instance first picked it up; kept if that claim later lapsed.
    pub picked_up_at_ms: Option<i64>,
    /// The instance holding or having accepted it, as it registered; `None` while queued.
    pub instance: Option<String>,
    /// The session that instance registered under, when it named one (an instance id
    /// need not be a session id). Not on the wire: the read model shows the holder by it.
    #[serde(skip)]
    pub instance_session: Option<String>,
    pub accepted_at_ms: Option<i64>,
    /// `work` or `no_work`, from the acceptance receipt.
    pub disposition: Option<String>,
    pub note: Option<String>,
    pub withdrawn_at_ms: Option<i64>,
}

/// What `recipient` has not read yet: how many rows, and the newest row's id (so a
/// mail announcement can tell whether anything arrived since the last one).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InboxUnread {
    pub count: u32,
    pub newest_id: Option<i64>,
}

const SELECT_COLUMNS: &str =
    "SELECT id, sender, recipient, text, created_at, read_at, in_reply_to, in_reply_to_org, in_reply_to_team, primary_file, archived_at, withdrawn_at, on_click, content_type, via, reply_to, summary, kind, attachments FROM wv_inbox";

fn row_to_message(r: &rusqlite::Row<'_>) -> rusqlite::Result<InboxMessage> {
    let org: Option<String> = r.get(7)?;
    let team: Option<String> = r.get(8)?;
    let content_type_raw: Option<String> = r.get(13)?;
    Ok(InboxMessage {
        id: r.get(0)?,
        from: r.get(1)?,
        to: r.get(2)?,
        text: r.get(3)?,
        created_at_ms: r.get(4)?,
        read_at_ms: r.get(5)?,
        in_reply_to: r.get(6)?,
        // Legacy or partial provenance is unknown, never inferred from this store.
        in_reply_to_team: org.zip(team).and_then(|(org, team)| {
            (!org.is_empty() && !team.is_empty()).then_some(InboxReplyTeam { org, team })
        }),
        primary_file: r.get::<_, Option<String>>(9)?.filter(|s| !s.is_empty()),
        attachments: r
            .get::<_, Option<String>>(18)?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        archived_at_ms: r.get(10)?,
        withdrawn_at_ms: r.get(11)?,
        delivery: None,
        on_click: MessageOnClick::from_stored(r.get::<_, Option<String>>(12)?),
        content_type: MessageContentType::from_stored(content_type_raw.as_deref()),
        via: r
            .get::<_, Option<String>>(14)?
            .filter(|s| !s.trim().is_empty()),
        role: None,
        reply_to: r
            .get::<_, Option<String>>(15)?
            .filter(|s| !s.trim().is_empty()),
        summary: r
            .get::<_, Option<String>>(16)?
            .filter(|s| !s.trim().is_empty()),
        kind: MessageKind::from_stored(r.get::<_, Option<String>>(17)?.as_deref()),
    })
}

/// The delivery of each listed message: its role delivery row (and acceptance receipt),
/// or its withdrawal. Delivery and receipt times are stored in seconds; the view is ms.
fn with_deliveries(
    conn: &rusqlite::Connection,
    mut rows: Vec<InboxMessage>,
) -> WvWriteResult<Vec<InboxMessage>> {
    let mut stmt = conn.prepare_cached(
        "SELECT d.state, d.claim_owner_instance, d.first_claimed_at, \
                r.instance_id, r.created_at, r.kind, r.note_or_reason, d.role \
         FROM wv_role_delivery d \
         LEFT JOIN wv_role_acceptance_receipt r ON r.delivery_id = d.delivery_id \
         WHERE d.message_id = ?1 ORDER BY d.delivery_id ASC LIMIT 1",
    )?;
    let mut session_stmt = conn.prepare_cached(
        "SELECT session_id FROM wv_role_registration WHERE role = ?1 AND instance_id = ?2",
    )?;
    for m in &mut rows {
        if let Some(at) = m.withdrawn_at_ms {
            m.delivery = Some(InboxDelivery {
                state: InboxDeliveryState::Withdrawn,
                picked_up_at_ms: None,
                instance: None,
                instance_session: None,
                accepted_at_ms: None,
                disposition: None,
                note: None,
                withdrawn_at_ms: Some(at),
            });
            continue;
        }
        let found = stmt
            .query_row(params![m.id], |r| {
                Ok((
                    DeliveryRow {
                        state: r.get(0)?,
                        claim_owner: r.get(1)?,
                        first_claimed_at: r.get(2)?,
                        receipt_instance: r.get(3)?,
                        accepted_at: r.get(4)?,
                        kind: r.get(5)?,
                        note: r.get(6)?,
                    },
                    r.get::<_, String>(7)?,
                ))
            })
            .optional()?;
        if let Some((row, role)) = found {
            let mut delivery = row.view();
            if let Some(instance) = delivery.instance.as_deref() {
                delivery.instance_session = session_stmt
                    .query_row(params![role, instance], |r| r.get::<_, Option<String>>(0))
                    .optional()?
                    .flatten()
                    .filter(|s| !s.trim().is_empty());
            }
            m.delivery = Some(delivery);
            m.role = Some(role);
        }
    }
    Ok(rows)
}

/// A delivery row joined with its receipt, as stored.
pub(crate) struct DeliveryRow {
    pub state: String,
    pub claim_owner: Option<String>,
    pub first_claimed_at: Option<i64>,
    pub receipt_instance: Option<String>,
    pub accepted_at: Option<i64>,
    pub kind: Option<String>,
    pub note: Option<String>,
}

impl DeliveryRow {
    /// Acknowledged is done; a receipt means accepted (also while a reclaim adopts it);
    /// a live claim is reading; anything else waits in the queue.
    pub(crate) fn state(&self) -> InboxDeliveryState {
        if self.state == "acknowledged" {
            InboxDeliveryState::Done
        } else if self.receipt_instance.is_some() {
            InboxDeliveryState::Accepted
        } else if self.state == "claimed" {
            InboxDeliveryState::Reading
        } else {
            InboxDeliveryState::Queued
        }
    }

    fn view(self) -> InboxDelivery {
        let state = self.state();
        let instance = match state {
            InboxDeliveryState::Queued | InboxDeliveryState::Withdrawn => None,
            InboxDeliveryState::Reading => self.claim_owner,
            InboxDeliveryState::Accepted | InboxDeliveryState::Done => {
                self.receipt_instance.or(self.claim_owner)
            }
        };
        InboxDelivery {
            state,
            picked_up_at_ms: self.first_claimed_at.map(|s| s * 1000),
            instance,
            instance_session: None,
            accepted_at_ms: self.accepted_at.map(|s| s * 1000),
            note: self.note.filter(|n| !n.trim().is_empty()),
            disposition: self.kind,
            withdrawn_at_ms: None,
        }
    }
}

pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn invalid(msg: &str) -> super::sqlite::WvWriteError {
    WvValidation(msg.to_string()).into()
}

/// Message text as stored: secrets redacted, like every other prose column in the
/// weft store. Blank text is refused.
fn stored_text(text: &str) -> WvWriteResult<String> {
    if is_blank(text) {
        return Err(invalid("text must not be blank"));
    }
    Ok(crate::shared::secret_scan::redact_secrets(text))
}

/// Store a message from `from` to `to` (answering message `in_reply_to` of another
/// store or this one) and return its id. `primary_file` is the optional managed-file
/// reference the human previews with the message; the path is stored verbatim and
/// never resolved here (the Inbox is a reference store, not a copy store).
#[allow(clippy::too_many_arguments)]
pub fn inbox_insert(
    store: &WeftStore,
    from: &str,
    to: &str,
    text: &str,
    in_reply_to: Option<i64>,
    in_reply_to_team: Option<&InboxReplyTeam>,
    primary_file: Option<&str>,
    on_click: Option<&MessageOnClick>,
    content_type: Option<MessageContentType>,
) -> WvWriteResult<i64> {
    inbox_insert_with_reply_to(
        store,
        from,
        to,
        text,
        in_reply_to,
        in_reply_to_team,
        primary_file,
        on_click,
        content_type,
        None,
        None,
        None,
    )
}

/// [`inbox_insert`] with a `reply_to` (where replies to it go instead of `from`),
/// stored as the caller resolved it, a `summary` (the list preview), stored
/// normalized, and the sender's `kind` (NULL when it set none).
#[allow(clippy::too_many_arguments)]
pub fn inbox_insert_with_reply_to(
    store: &WeftStore,
    from: &str,
    to: &str,
    text: &str,
    in_reply_to: Option<i64>,
    in_reply_to_team: Option<&InboxReplyTeam>,
    primary_file: Option<&str>,
    on_click: Option<&MessageOnClick>,
    content_type: Option<MessageContentType>,
    reply_to: Option<&str>,
    summary: Option<&str>,
    kind: Option<MessageKind>,
) -> WvWriteResult<i64> {
    inbox_insert_request(
        store,
        from,
        to,
        text,
        in_reply_to,
        in_reply_to_team,
        primary_file,
        on_click,
        content_type,
        reply_to,
        summary,
        kind,
        None,
        None,
    )
}

/// A stable digest shared by the durable mail and completion retry receipts.
pub(crate) fn request_payload_hash(payload: &serde_json::Value) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(payload.to_string().as_bytes()))
}

/// Claim a completion before its side effects run, or replay its durable receipt.
/// The lookup and insert share one IMMEDIATE transaction so retries cannot both win.
pub(crate) fn inbox_claim_completion_request(
    store: &WeftStore,
    key: &str,
    hash: &str,
) -> WvWriteResult<Option<String>> {
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let prior: Option<(String, Option<String>)> = tx
        .query_row(
            "SELECT payload_hash, result FROM wv_mcp_request WHERE request_id=?1",
            [key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((previous, result)) = prior {
        if previous != hash {
            return Err(invalid("requestId reused with a different payload"));
        }
        return result.map(Some).ok_or_else(|| invalid(
            "CodeSense restarted or the earlier call is still running; its outcome is uncertain. Check list_runs before sending a new completion."
        ));
    }
    tx.execute(
        "INSERT INTO wv_mcp_request (request_id, payload_hash) VALUES (?1, ?2)",
        params![key, hash],
    )?;
    tx.commit()?;
    Ok(None)
}

/// Record the result that subsequent retries of the claimed completion replay.
pub(crate) fn inbox_finish_completion_request(
    store: &WeftStore,
    key: &str,
    result: &str,
) -> WvWriteResult<()> {
    store.conn()?.execute(
        "UPDATE wv_mcp_request SET result=?2 WHERE request_id=?1",
        params![key, result],
    )?;
    Ok(())
}

/// One authoritative replay check, shared by the early command lookup and the
/// insert transaction. The hash describes caller inputs, never derived classification.
fn request_replay(
    conn: &rusqlite::Connection,
    key: &str,
    hash: &str,
) -> WvWriteResult<Option<i64>> {
    let prior: Option<(String, i64)> = conn
        .query_row(
            "SELECT payload_hash, message_id FROM wv_send_message_request WHERE request_id=?1",
            [key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    match prior {
        Some((prior_hash, _)) if prior_hash != hash => {
            Err(invalid("requestId reused with a different payload"))
        }
        Some((_, id)) => Ok(Some(id)),
        None => Ok(None),
    }
}

pub(crate) fn inbox_replay_request(
    store: &WeftStore,
    from: &str,
    request_id: &str,
    hash: &str,
) -> WvWriteResult<Option<InboxMessage>> {
    let conn = store.conn()?;
    let Some(id) = request_replay(&conn, &format!("mail:{from}:{request_id}"), hash)? else {
        return Ok(None);
    };
    Ok(conn
        .query_row(
            &format!("{SELECT_COLUMNS} WHERE id=?1"),
            [id],
            row_to_message,
        )
        .optional()?)
}

/// A retry key commits with its message, so losing the response cannot duplicate mail.
#[allow(clippy::too_many_arguments)]
pub(crate) fn inbox_insert_request(
    store: &WeftStore,
    from: &str,
    to: &str,
    text: &str,
    in_reply_to: Option<i64>,
    in_reply_to_team: Option<&InboxReplyTeam>,
    primary_file: Option<&str>,
    on_click: Option<&MessageOnClick>,
    content_type: Option<MessageContentType>,
    reply_to: Option<&str>,
    summary: Option<&str>,
    kind: Option<MessageKind>,
    request_id: Option<&str>,
    request_hash: Option<&str>,
) -> WvWriteResult<i64> {
    inbox_insert_request_outcome(
        store,
        from,
        to,
        text,
        in_reply_to,
        in_reply_to_team,
        primary_file,
        on_click,
        content_type,
        reply_to,
        summary,
        kind,
        request_id,
        request_hash,
        None,
    )
    .map(|(id, _)| id)
}

/// The insert decision is made in the same transaction as the retry receipt.
#[allow(clippy::too_many_arguments)]
pub(crate) fn inbox_insert_request_outcome(
    store: &WeftStore,
    from: &str,
    to: &str,
    text: &str,
    in_reply_to: Option<i64>,
    in_reply_to_team: Option<&InboxReplyTeam>,
    primary_file: Option<&str>,
    on_click: Option<&MessageOnClick>,
    content_type: Option<MessageContentType>,
    reply_to: Option<&str>,
    summary: Option<&str>,
    kind: Option<MessageKind>,
    request_id: Option<&str>,
    request_hash: Option<&str>,
    attachments: Option<&[crate::media::Attachment]>,
) -> WvWriteResult<(i64, bool)> {
    let text = if text.trim().is_empty() && attachments.is_some_and(|a| !a.is_empty()) {
        String::new()
    } else {
        stored_text(text)?
    };
    let attachments_json = serde_json::to_string(&attachments.unwrap_or_default()).unwrap();
    let kind = kind.map(MessageKind::as_stored);
    let summary = normalize_summary(summary);
    let reply_to = reply_to.map(str::trim).filter(|s| !s.is_empty());
    let origin = in_reply_to.and(in_reply_to_team);
    let reply_org = origin.map(|t| t.org.as_str());
    let reply_team = origin.map(|t| t.team.as_str());
    let primary_file = primary_file.map(|s| s.trim()).filter(|s| !s.is_empty());
    let on_click_json = MessageOnClick::to_stored(on_click);
    let content_type_str = content_type.map(|c| c.as_stored());
    let mut payload = serde_json::json!([
        from,
        to,
        text,
        in_reply_to,
        reply_org,
        reply_team,
        primary_file,
        on_click_json,
        content_type_str,
        reply_to,
        summary,
        kind
    ]);
    if attachments.is_some_and(|a| !a.is_empty()) {
        payload
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!(attachments_json));
    }
    let hash = request_payload_hash(&payload);
    let hash = request_hash.unwrap_or(&hash);
    let key = request_id
        .filter(|id| !id.trim().is_empty())
        .map(|id| format!("mail:{from}:{id}"));
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(key) = &key {
        if let Some(id) = request_replay(&tx, key, hash)? {
            return Ok((id, false));
        }
    }
    tx.execute(
        "INSERT INTO wv_inbox (sender, recipient, text, created_at, in_reply_to, in_reply_to_org, in_reply_to_team, primary_file, on_click, content_type, reply_to, summary, kind, attachments) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![from, to, text, now_ms(), in_reply_to, reply_org, reply_team, primary_file, on_click_json, content_type_str, reply_to, summary, kind, attachments_json],
    )?;
    let id = tx.last_insert_rowid();
    write_audit_public(&tx, INBOX, "append", id)?;
    if let Some(key) = key {
        tx.execute("INSERT INTO wv_send_message_request (request_id, payload_hash, message_id, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![key, hash, id, now_ms()])?;
        write_audit_public(&tx, "wv_send_message_request", "append", 0)?;
    }
    tx.commit()?;
    Ok((id, true))
}

/// Record text `from` typed into `to`'s terminal, so the conversation shows it. The row
/// is stored read: the recipient already has the text in front of it, and an unread
/// count or mail announcement for it would make the agent read it twice.
pub fn inbox_insert_terminal(
    store: &WeftStore,
    from: &str,
    to: &str,
    text: &str,
) -> WvWriteResult<i64> {
    let text = stored_text(text)?;
    let now = now_ms();
    let mut conn = store.conn()?;
    Ok(wv_auto_append(
        &mut conn,
        "INSERT INTO wv_inbox (sender, recipient, text, created_at, read_at, via, content_type) \
         VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6)",
        &[
            &from,
            &to,
            &text,
            &now,
            &VIA_TERMINAL,
            &MessageContentType::Plain.as_stored(),
        ],
        INBOX,
    )?)
}

/// Message `id`, if this store holds it.
pub fn inbox_get(store: &WeftStore, id: i64) -> WvWriteResult<Option<InboxMessage>> {
    let conn = store.conn()?;
    Ok(conn
        .query_row(
            &format!("{SELECT_COLUMNS} WHERE id = ?1"),
            params![id],
            row_to_message,
        )
        .optional()?)
}

/// Mark message `id` read when it is addressed to `reader` and still unread. Returns
/// whether it changed.
pub fn inbox_mark_read(store: &WeftStore, id: i64, reader: &str) -> WvWriteResult<bool> {
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute(
        "UPDATE wv_inbox SET read_at = ?3 WHERE id = ?1 AND recipient = ?2 AND read_at IS NULL",
        params![id, reader, now_ms()],
    )?;
    if changed == 1 {
        write_audit_public(&tx, INBOX, "mutate", id)?;
    }
    tx.commit()?;
    Ok(changed == 1)
}

/// SW-AGR-18: read every message of `ids` addressed to `reader` that it has not read, in
/// one transaction (one audit row each). Returns how many changed; other ids, someone
/// else's mail and mail already read are left alone.
pub fn inbox_mark_read_many(store: &WeftStore, ids: &[i64], reader: &str) -> WvWriteResult<u32> {
    inbox_set_read_many(store, ids, reader, Some(now_ms()))
}

/// Clear a recipient's read state in one transaction, leaving delivery and archive state alone.
pub fn inbox_mark_unread_many(store: &WeftStore, ids: &[i64], reader: &str) -> WvWriteResult<u32> {
    inbox_set_read_many(store, ids, reader, None)
}

// Architecture: both read-state transitions share recipient scoping, idempotency and auditing.
fn inbox_set_read_many(
    store: &WeftStore,
    ids: &[i64],
    reader: &str,
    read_at: Option<i64>,
) -> WvWriteResult<u32> {
    if ids.is_empty() {
        return Ok(0);
    }
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut changed = 0u32;
    {
        let predicate = if read_at.is_some() {
            "IS NULL"
        } else {
            "IS NOT NULL"
        };
        let mut stmt = tx.prepare_cached(&format!(
            "UPDATE wv_inbox SET read_at = ?3 WHERE id = ?1 AND recipient = ?2 AND read_at {predicate}",
        ))?;
        for id in ids {
            if stmt.execute(params![id, reader, read_at])? == 1 {
                write_audit_public(&tx, INBOX, "mutate", *id)?;
                changed += 1;
            }
        }
    }
    tx.commit()?;
    Ok(changed)
}

/// Archive and read the human's messages together, counting each newly archived row once.
pub fn inbox_mark_archived_many(store: &WeftStore, ids: &[i64]) -> WvWriteResult<u32> {
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let now = now_ms();
    let mut changed = 0;
    {
        let mut stmt = tx.prepare_cached(
            "UPDATE wv_inbox SET archived_at = ?2, read_at = COALESCE(read_at, ?2) WHERE id = ?1 AND recipient = 'human' AND archived_at IS NULL",
        )?;
        for id in ids {
            if stmt.execute(params![id, now])? == 1 {
                write_audit_public(&tx, INBOX, "mutate", *id)?;
                changed += 1;
            }
        }
    }
    tx.commit()?;
    Ok(changed)
}

/// Undo [`inbox_mark_archived_many`]: unarchive the human's archived messages of `ids`, and
/// make the ones also in `restore_unread` unread again, in one transaction (one audit row
/// per changed message). Returns how many were unarchived; other ids are left alone.
pub fn inbox_unarchive_many(
    store: &WeftStore,
    ids: &[i64],
    restore_unread: &[i64],
) -> WvWriteResult<u32> {
    let restore_unread: std::collections::HashSet<i64> = restore_unread.iter().copied().collect();
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut changed = 0;
    {
        let mut stmt = tx.prepare_cached(
            "UPDATE wv_inbox SET archived_at = NULL, read_at = CASE WHEN ?2 THEN NULL ELSE read_at END WHERE id = ?1 AND recipient = 'human' AND archived_at IS NOT NULL",
        )?;
        for id in ids {
            if stmt.execute(params![id, restore_unread.contains(id)])? == 1 {
                write_audit_public(&tx, INBOX, "mutate", *id)?;
                changed += 1;
            }
        }
    }
    tx.commit()?;
    Ok(changed)
}

/// `recipient`'s unread messages, oldest first, marked read in the same transaction so
/// each is returned once.
pub fn inbox_take_unread(store: &WeftStore, recipient: &str) -> WvWriteResult<Vec<InboxMessage>> {
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let unread = {
        let mut stmt = tx.prepare(&format!(
            "{SELECT_COLUMNS} WHERE recipient = ?1 AND read_at IS NULL AND withdrawn_at IS NULL ORDER BY id ASC"
        ))?;
        let rows = stmt.query_map(params![recipient], row_to_message)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let now = now_ms();
    for message in &unread {
        tx.execute(
            "UPDATE wv_inbox SET read_at = ?2 WHERE id = ?1",
            params![message.id, now],
        )?;
        write_audit_public(&tx, INBOX, "mutate", message.id)?;
    }
    tx.commit()?;
    Ok(unread
        .into_iter()
        .map(|m| InboxMessage {
            read_at_ms: Some(now),
            ..m
        })
        .collect())
}

/// How many messages `recipient` has not read, and the newest one's id.
pub fn inbox_unread(store: &WeftStore, recipient: &str) -> WvWriteResult<InboxUnread> {
    let conn = store.conn()?;
    let (count, newest_id): (u32, Option<i64>) = conn.query_row(
        "SELECT COUNT(*), MAX(id) FROM wv_inbox WHERE recipient = ?1 AND read_at IS NULL AND withdrawn_at IS NULL",
        params![recipient],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(InboxUnread { count, newest_id })
}

/// How long a notice offered to an agent waits for its delivery to be confirmed before
/// it is offered again: a lost confirm costs a repeated notice, never untold mail.
pub const RESERVE_TTL_MS: i64 = 60_000;

/// How long an offer made by another app run (one before a restart, or another app on
/// this store) may still be confirmed: a host reports a notice told within seconds of
/// being handed it, so past this its confirm is not coming.
const OTHER_RUN_GRACE_MS: i64 = 5_000;

/// This app run, recorded with each notice it offers (see [`OTHER_RUN_GRACE_MS`]).
pub(crate) fn this_run() -> &'static str {
    static RUN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    RUN.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

/// What one inbox check found for a session's agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reservation {
    /// Tell the agent about `count` unread messages, then confirm `token` once it was
    /// told. The same token comes back when an unconfirmed notice is offered again.
    Offer { token: String, count: u32 },
    /// Nothing it was not told (or offered) about.
    Nothing,
    /// A notice offered at most [`RESERVE_TTL_MS`] ago awaits its confirm; it is offered
    /// again from this time (epoch ms).
    PendingUntil(i64),
    /// Whether the notice offered under the pending token reached the agent is unknown
    /// ([`inbox_hold_announcement`]): it is never offered again, only confirmed (or
    /// replaced by newer mail).
    Held,
}

/// What a reservation's `reserved_by` holds while its delivery is uncertain.
const UNCERTAIN: &str = "uncertain";

/// Shared chat referenced by an inbox notice; the transcript stays in its workspace log.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChatAnnouncement {
    pub workspace_id: String,
    pub through_id: i64,
    pub count: u32,
}

pub(crate) fn inbox_reserved_chat(
    store: &WeftStore,
    session: &str,
    token: &str,
) -> WvWriteResult<Option<(String, ChatAnnouncement)>> {
    let raw: Option<(String, Option<String>)> = store.conn()?.query_row(
        "SELECT recipient, reserved_chat FROM wv_inbox_notified WHERE session = ?1 AND reserve_token = ?2",
        params![session, token], |r| Ok((r.get(0)?, r.get(1)?)),
    ).optional()?;
    raw.and_then(|(handle, chat)| chat.map(|c| (handle, c)))
        .map(|(h, s)| {
            serde_json::from_str(&s)
                .map(|c| (h, c))
                .map_err(|e| crate::weft::sqlite::wv_refusal(e.to_string()))
        })
        .transpose()
}

/// Direct-mail test fixture over the shared reservation engine.
#[cfg(test)]
pub(crate) fn inbox_reserve_announcement_at(
    store: &WeftStore,
    recipient: &str,
    session: &str,
    now: i64,
    run: &str,
    reoffer: bool,
) -> WvWriteResult<Reservation> {
    inbox_reserve_with_chat_at(store, recipient, session, now, run, reoffer, None)
}

/// Reserve one host notice referring to direct mail and any untold shared posts.
pub(crate) fn inbox_reserve_announcement(
    store: &WeftStore,
    recipient: &str,
    session: &str,
    reoffer: bool,
    chat: Option<ChatAnnouncement>,
) -> WvWriteResult<Reservation> {
    inbox_reserve_with_chat_at(
        store,
        recipient,
        session,
        now_ms(),
        this_run(),
        reoffer,
        chat,
    )
}

#[allow(clippy::too_many_arguments)]
fn inbox_reserve_with_chat_at(
    store: &WeftStore,
    recipient: &str,
    session: &str,
    now: i64,
    run: &str,
    reoffer: bool,
    chat: Option<ChatAnnouncement>,
) -> WvWriteResult<Reservation> {
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (count, newest): (u32, Option<i64>) = tx.query_row(
        "SELECT COUNT(*), MAX(id) FROM wv_inbox WHERE recipient = ?1 AND read_at IS NULL AND withdrawn_at IS NULL",
        params![recipient],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let newest = newest.unwrap_or(0);
    if count == 0 && chat.is_none() {
        return Ok(Reservation::Nothing);
    }
    // This session's marks; another session's under the handle tell it nothing.
    type Mark = (
        i64,
        Option<i64>,
        Option<String>,
        Option<i64>,
        Option<String>,
        Option<String>,
    );
    let mark: Option<Mark> = tx
        .query_row(
            "SELECT through_id, reserved_through, reserve_token, reserved_at, reserved_by, reserved_chat \
             FROM wv_inbox_notified WHERE recipient = ?1 AND session = ?2",
            params![recipient, session],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .optional()?;
    let told = mark.as_ref().map_or(0, |m| m.0);
    if newest <= told && chat.is_none() {
        return Ok(Reservation::Nothing);
    }
    if let Some((_, Some(through), Some(token), Some(at), by, reserved_chat)) = mark {
        let previous_chat: Option<ChatAnnouncement> =
            reserved_chat.and_then(|s| serde_json::from_str(&s).ok());
        let chat_covered = match (&chat, &previous_chat) {
            (None, _) => true,
            (Some(c), Some(p)) => c.workspace_id == p.workspace_id && c.through_id <= p.through_id,
            _ => false,
        };
        if newest <= through && chat_covered && by.as_deref() == Some(UNCERTAIN) {
            return Ok(Reservation::Held);
        }
        if newest <= through && chat_covered {
            // Its confirm may still arrive after the offer (a clock that stepped back
            // reads as lapsed) and before its TTL ends; a released offer has no run.
            let ttl = match by.as_deref() {
                Some(by) if by == run => Some(RESERVE_TTL_MS),
                Some(_) => Some(OTHER_RUN_GRACE_MS),
                None => None,
            };
            if let Some(until) = ttl
                .filter(|_| !reoffer)
                .map(|ttl| at + ttl)
                .filter(|until| (at..*until).contains(&now))
            {
                return Ok(Reservation::PendingUntil(until));
            }
            let row: i64 = tx.query_row(
                "UPDATE wv_inbox_notified SET reserved_at = ?3, reserved_by = ?4 \
                 WHERE recipient = ?1 AND reserve_token = ?2 RETURNING id",
                params![recipient, token, now, run],
                |r| r.get(0),
            )?;
            write_audit_public(&tx, ANNOUNCED, "mutate", row)?;
            tx.commit()?;
            return Ok(Reservation::Offer { token, count });
        }
    }
    let token = uuid::Uuid::new_v4().to_string();
    let reserved_chat = chat.map(|c| serde_json::to_string(&c).expect("chat notice serializes"));
    let row: i64 = tx.query_row(
        "INSERT INTO wv_inbox_notified \
         (recipient, session, through_id, announced_at, reserved_through, reserve_token, \
          reserved_at, reserved_by, reserved_chat) \
         VALUES (?1, ?2, 0, 0, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT(recipient) DO UPDATE SET \
         through_id = CASE WHEN session = excluded.session THEN through_id ELSE 0 END, \
         announced_at = CASE WHEN session = excluded.session THEN announced_at ELSE 0 END, \
         session = excluded.session, reserved_through = excluded.reserved_through, \
         reserve_token = excluded.reserve_token, reserved_at = excluded.reserved_at, \
         reserved_by = excluded.reserved_by, reserved_chat = excluded.reserved_chat \
         RETURNING id",
        params![recipient, session, newest, token, now, run, reserved_chat],
        |r| r.get(0),
    )?;
    write_audit_public(&tx, ANNOUNCED, "mutate", row)?;
    tx.commit()?;
    Ok(Reservation::Offer { token, count })
}

/// The notice reserved under `token` for `session` was handed to its agent's hook: what
/// it told about is marked announced, never moving the mark back. The token names the
/// reservation whatever handle the session is bound to by now. Idempotent: a token
/// confirmed already, replaced by a newer offer (whose notice counts its mail too), or of
/// another session, confirms nothing. Returns whether a reservation was confirmed.
pub fn inbox_confirm_announcement(
    store: &WeftStore,
    session: &str,
    token: &str,
) -> WvWriteResult<bool> {
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let row: Option<i64> = tx
        .query_row(
            "UPDATE wv_inbox_notified SET through_id = MAX(through_id, reserved_through), \
             announced_at = ?2, reserved_through = NULL, reserve_token = NULL, \
             reserved_at = NULL, reserved_by = NULL, reserved_chat = NULL \
             WHERE reserve_token = ?1 AND session = ?3 RETURNING id",
            params![token, now_ms(), session],
            |r| r.get(0),
        )
        .optional()?;
    let Some(row) = row else {
        return Ok(false);
    };
    write_audit_public(&tx, ANNOUNCED, "mutate", row)?;
    tx.commit()?;
    Ok(true)
}

/// The notice reserved under `token` will not reach its agent (its host could not take
/// it, or its CLI refused it): it lapses now, so the session's next check offers it at
/// once.
pub fn inbox_release_announcement(store: &WeftStore, token: &str) -> WvWriteResult<()> {
    set_reserved_by(store, token, None)
}

/// Whether the notice reserved under `token` reached its agent is unknown (its CLI may
/// have taken it): it is never offered again, so the agent is never told twice blind. A
/// confirm of `token` still marks it told, and newer mail is offered under a new token.
pub fn inbox_hold_announcement(store: &WeftStore, token: &str) -> WvWriteResult<()> {
    set_reserved_by(store, token, Some(UNCERTAIN))
}

/// Sets who holds the notice reserved under `token` (`None`: no one, so it lapses).
fn set_reserved_by(store: &WeftStore, token: &str, by: Option<&str>) -> WvWriteResult<()> {
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let row: Option<i64> = tx
        .query_row(
            "UPDATE wv_inbox_notified SET reserved_by = ?2 WHERE reserve_token = ?1 RETURNING id",
            params![token, by],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(row) = row {
        write_audit_public(&tx, ANNOUNCED, "mutate", row)?;
        tx.commit()?;
    }
    Ok(())
}

/// When a recipient's agent was last told about its mail (a confirmed notice, see
/// [`inbox_confirm_announcement`]): through which message, when (epoch ms), and the
/// session it was told in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Announced {
    pub through_id: i64,
    pub at_ms: i64,
    pub session: String,
}

/// One recipient's unread mail as a session listing shows it: how many rows, when the
/// oldest was stored (epoch ms), the newest row's id, and when its agent was last told
/// about its mail.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnreadSummary {
    pub count: u32,
    pub oldest_at_ms: Option<i64>,
    pub newest_id: Option<i64>,
    pub announced: Option<Announced>,
}

/// The unread mail of each of `recipients` that has any, counted exactly as
/// [`inbox_take_unread`] would hand it over (unread, not withdrawn). A view: it marks
/// nothing read. A recipient with no unread mail is absent from the map.
pub fn inbox_unread_for(
    store: &WeftStore,
    recipients: &[String],
) -> WvWriteResult<std::collections::HashMap<String, UnreadSummary>> {
    let mut out = std::collections::HashMap::new();
    if recipients.is_empty() {
        return Ok(out);
    }
    let conn = store.conn()?;
    let marks = (1..=recipients.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut stmt = conn.prepare(&format!(
        "SELECT i.recipient, COUNT(*), MIN(i.created_at), MAX(i.id), a.through_id, a.announced_at, a.session \
         FROM wv_inbox i LEFT JOIN wv_inbox_notified a ON a.recipient = i.recipient \
         WHERE i.recipient IN ({marks}) AND i.read_at IS NULL AND i.withdrawn_at IS NULL \
         GROUP BY i.recipient"
    ))?;
    let values: Vec<&dyn rusqlite::ToSql> = recipients
        .iter()
        .map(|r| r as &dyn rusqlite::ToSql)
        .collect();
    let rows = stmt.query_map(values.as_slice(), |r| {
        Ok((
            r.get::<_, String>(0)?,
            UnreadSummary {
                count: r.get(1)?,
                oldest_at_ms: r.get(2)?,
                newest_id: r.get(3)?,
                announced: match (
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                ) {
                    // A row whose session has only offers pending was told nothing.
                    (Some(through_id), Some(at_ms), Some(session)) if through_id > 0 => {
                        Some(Announced {
                            through_id,
                            at_ms,
                            session,
                        })
                    }
                    _ => None,
                },
            },
        ))
    })?;
    for row in rows {
        let (recipient, summary) = row?;
        out.insert(recipient, summary);
    }
    Ok(out)
}

/// The newest [`INBOX_LIST_LIMIT`] rows of the whole inbox, newest first. A view:
/// it marks nothing read.
pub fn inbox_list(store: &WeftStore) -> WvWriteResult<Vec<InboxMessage>> {
    let conn = store.conn()?;
    let mut stmt = conn.prepare(&format!("{SELECT_COLUMNS} ORDER BY id DESC LIMIT ?1"))?;
    let rows = stmt.query_map(params![INBOX_LIST_LIMIT], row_to_message)?;
    let rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    with_deliveries(&conn, rows)
}

/// A stable descending-id page; views never read beyond the requested page.
pub fn inbox_page(
    store: &WeftStore,
    before: Option<i64>,
    limit: usize,
) -> WvWriteResult<Vec<InboxMessage>> {
    let conn = store.conn()?;
    let mut stmt = conn.prepare(&format!(
        "{SELECT_COLUMNS} WHERE (?1 IS NULL OR id < ?1) ORDER BY id DESC LIMIT ?2"
    ))?;
    let rows = stmt
        .query_map(params![before, limit.min(200)], row_to_message)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    with_deliveries(&conn, rows)
}
pub fn inbox_rows_by_id(store: &WeftStore, ids: &[i64]) -> WvWriteResult<Vec<InboxMessage>> {
    let conn = store.conn()?;
    let mut rows = Vec::new();
    for id in ids {
        if let Some(row) = inbox_get(store, *id)? {
            rows.push(row);
        }
    }
    with_deliveries(&conn, rows)
}
pub fn inbox_announced(
    store: &WeftStore,
) -> WvWriteResult<std::collections::BTreeMap<String, Announced>> {
    let conn = store.conn()?;
    let mut stmt=conn.prepare("SELECT recipient,through_id,announced_at,session FROM wv_inbox_notified WHERE through_id>0 AND announced_at IS NOT NULL AND session IS NOT NULL")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get(0)?,
            Announced {
                through_id: r.get(1)?,
                at_ms: r.get(2)?,
                session: r.get(3)?,
            },
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The newest [`INBOX_LIST_LIMIT`] rows of one agent's conversation, newest first: rows
/// addressed to (or sent as) `agent_id`, its role inbox, or one of `sessions` (the handles
/// of its running instances, whose own inboxes are part of its conversation), and every
/// reply under them at any depth. A view: it marks nothing read.
pub fn inbox_list_for_agent(
    store: &WeftStore,
    agent_id: &str,
    sessions: &[String],
) -> WvWriteResult<Vec<InboxMessage>> {
    let conn = store.conn()?;
    let mut addresses: Vec<&str> = vec![agent_id];
    addresses.extend(sessions.iter().map(String::as_str));
    let marks = (0..addresses.len())
        .map(|i| format!("?{}", i + 2))
        .collect::<Vec<_>>()
        .join(", ");
    let mut stmt = conn.prepare(&format!(
        "WITH RECURSIVE thread(id) AS ( \
            SELECT id FROM wv_inbox WHERE recipient IN ({marks}) OR sender IN ({marks}) \
            UNION SELECT m.id FROM wv_inbox m JOIN thread t ON m.in_reply_to = t.id \
         ) \
         {SELECT_COLUMNS} WHERE id IN (SELECT id FROM thread) ORDER BY id DESC LIMIT ?1"
    ))?;
    let mut values: Vec<&dyn rusqlite::ToSql> = vec![&INBOX_LIST_LIMIT];
    values.extend(addresses.iter().map(|a| a as &dyn rusqlite::ToSql));
    let rows = stmt.query_map(values.as_slice(), row_to_message)?;
    let rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    with_deliveries(&conn, rows)
}

/// The newest [`INBOX_LIST_LIMIT`] rows of one session's conversation, newest first:
/// rows sent by or addressed to any of `addresses` (the session's handle and its tab
/// session id), rows addressed to one of `roles` (the role queues it holds), and role
/// rows it claimed or accepted. Replies are not followed: a session sees what was said
/// to it and by it. A view: it marks nothing read and claims nothing.
pub fn inbox_list_for_session(
    store: &WeftStore,
    addresses: &[String],
    roles: &[String],
) -> WvWriteResult<Vec<InboxMessage>> {
    if addresses.is_empty() && roles.is_empty() {
        return Ok(Vec::new());
    }
    let conn = store.conn()?;
    let marks = |from: usize, n: usize| {
        let list = (from..from + n)
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        if list.is_empty() {
            "NULL".to_string()
        } else {
            list
        }
    };
    let addr = marks(2, addresses.len());
    let role = marks(2 + addresses.len(), roles.len());
    let mut stmt = conn.prepare(&format!(
        "{SELECT_COLUMNS} WHERE sender IN ({addr}) OR recipient IN ({addr}) OR recipient IN ({role}) \
            OR id IN (SELECT message_id FROM wv_role_delivery \
                      WHERE role IN ({role}) OR claim_owner_instance IN ({addr})) \
            OR id IN (SELECT message_id FROM wv_role_acceptance_receipt WHERE instance_id IN ({addr})) \
         ORDER BY id DESC LIMIT ?1"
    ))?;
    let mut values: Vec<&dyn rusqlite::ToSql> = vec![&INBOX_LIST_LIMIT];
    values.extend(addresses.iter().map(|a| a as &dyn rusqlite::ToSql));
    values.extend(roles.iter().map(|r| r as &dyn rusqlite::ToSql));
    let rows = stmt.query_map(values.as_slice(), row_to_message)?;
    let rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    with_deliveries(&conn, rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::weft::sqlite::WvWriteError;

    fn open_temp() -> (tempfile::TempDir, WeftStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
        (dir, store)
    }

    fn mail(store: &WeftStore, to: &str) -> i64 {
        inbox_insert(store, "H1", to, "body", None, None, None, None, None).unwrap()
    }

    /// Reserves for `session` at `now`, and confirms an offer at once (its agent was told).
    fn tell(store: &WeftStore, recipient: &str, session: &str, now: i64) -> Option<u32> {
        match inbox_reserve_announcement_at(store, recipient, session, now, RUN, false).unwrap() {
            Reservation::Offer { token, count } => {
                assert!(inbox_confirm_announcement(store, session, &token).unwrap());
                Some(count)
            }
            Reservation::Nothing => None,
            other => panic!("not told: {other:?}"),
        }
    }

    fn offer(store: &WeftStore, recipient: &str, session: &str, now: i64) -> (String, u32) {
        match inbox_reserve_announcement_at(store, recipient, session, now, RUN, false).unwrap() {
            Reservation::Offer { token, count } => (token, count),
            other => panic!("no offer: {other:?}"),
        }
    }

    const T0: i64 = 1_000_000;
    const RUN: &str = "run-1";

    #[test]
    fn an_offer_of_another_app_run_or_a_clock_set_back_is_offered_again_soon() {
        let (_dir, store) = open_temp();
        mail(&store, "H2");
        let (token, _) = offer(&store, "H2", "S2", T0);
        // Another run (the app restarted, or a second app shares the store) waits only
        // as long as a hook takes to confirm.
        assert_eq!(
            inbox_reserve_announcement_at(&store, "H2", "S2", T0 + 1, "run-2", false).unwrap(),
            Reservation::PendingUntil(T0 + OTHER_RUN_GRACE_MS)
        );
        assert_eq!(
            inbox_reserve_announcement_at(
                &store,
                "H2",
                "S2",
                T0 + OTHER_RUN_GRACE_MS,
                "run-2",
                false
            )
            .unwrap(),
            Reservation::Offer {
                token: token.clone(),
                count: 1
            }
        );
        // A clock set back before the offer does not hold it for longer than its TTL.
        assert!(matches!(
            inbox_reserve_announcement_at(&store, "H2", "S2", T0 - 10_000, "run-2", false).unwrap(),
            Reservation::Offer { .. }
        ));
    }

    #[test]
    fn an_unconfirmed_offer_is_offered_again_at_once_when_asked_to_reoffer() {
        let (_dir, store) = open_temp();
        mail(&store, "H2");
        let (token, _) = offer(&store, "H2", "S2", T0);
        assert_eq!(
            inbox_reserve_announcement_at(&store, "H2", "S2", T0 + 1, RUN, true).unwrap(),
            Reservation::Offer { token, count: 1 }
        );
    }

    #[test]
    fn a_released_offer_is_offered_again_at_once() {
        let (_dir, store) = open_temp();
        mail(&store, "H2");
        let (token, _) = offer(&store, "H2", "S2", T0);
        inbox_release_announcement(&store, &token).unwrap();
        assert_eq!(offer(&store, "H2", "S2", T0 + 1), (token, 1));
    }

    #[test]
    fn an_uncertain_offer_is_never_offered_again_but_is_confirmed_or_replaced_by_new_mail() {
        let (_dir, store) = open_temp();
        mail(&store, "H2");
        let (token, _) = offer(&store, "H2", "S2", T0);
        inbox_hold_announcement(&store, &token).unwrap();
        // Not after its TTL, not on a re-offer, not in another run.
        let later = T0 + RESERVE_TTL_MS * 10;
        for (run, reoffer) in [(RUN, false), (RUN, true), ("run-2", false)] {
            assert_eq!(
                inbox_reserve_announcement_at(&store, "H2", "S2", later, run, reoffer).unwrap(),
                Reservation::Held
            );
        }
        // Settled: told.
        assert!(inbox_confirm_announcement(&store, "S2", &token).unwrap());
        assert_eq!(tell(&store, "H2", "S2", later), None);
        // Newer mail is a new notice, under a new token, covering all of it.
        mail(&store, "H2");
        let (held, _) = offer(&store, "H2", "S2", later);
        inbox_hold_announcement(&store, &held).unwrap();
        mail(&store, "H2");
        let (newer, count) = offer(&store, "H2", "S2", later);
        assert_ne!(newer, held);
        assert_eq!(count, 3);
    }

    #[test]
    fn each_batch_of_new_mail_is_announced_once_and_the_mark_outlives_the_app() {
        let (dir, store) = open_temp();
        assert_eq!(tell(&store, "H2", "S2", T0), None);
        mail(&store, "H2");
        assert_eq!(tell(&store, "H2", "S2", T0), Some(1));
        assert_eq!(tell(&store, "H2", "S2", T0), None);

        // The app restarts: the mark is in the store, so the agent is not told again
        // about mail it already heard about and left unread.
        drop(store);
        let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
        assert_eq!(tell(&store, "H2", "S2", T0), None);

        // New mail is a new batch, counted with the unread mail before it.
        mail(&store, "H2");
        assert_eq!(tell(&store, "H2", "S2", T0), Some(2));
        // Marks are per recipient; nothing was marked read.
        mail(&store, "H3");
        assert_eq!(tell(&store, "H3", "S3", T0), Some(1));
        assert_eq!(inbox_unread(&store, "H2").unwrap().count, 2);
    }

    #[test]
    fn a_new_session_under_the_handle_starts_unannounced() {
        let (_dir, store) = open_temp();
        mail(&store, "H2");
        assert_eq!(tell(&store, "H2", "S2", T0), Some(1));
        // The handle is relaunched as another session: it was never told.
        assert_eq!(tell(&store, "H2", "S9", T0), Some(1));
        assert_eq!(tell(&store, "H2", "S9", T0), None);
    }

    #[test]
    fn read_mail_is_never_announced() {
        let (_dir, store) = open_temp();
        mail(&store, "H2");
        inbox_take_unread(&store, "H2").unwrap();
        assert_eq!(tell(&store, "H2", "S2", T0), None);
    }

    #[test]
    fn an_offer_marks_nothing_told_until_it_is_confirmed() {
        let (_dir, store) = open_temp();
        mail(&store, "H2");
        let wanted = ["H2".to_string()];
        let (token, count) = offer(&store, "H2", "S2", T0);
        assert_eq!(count, 1);
        assert_eq!(
            inbox_unread_for(&store, &wanted).unwrap()["H2"].announced,
            None
        );
        assert!(inbox_confirm_announcement(&store, "S2", &token).unwrap());
        assert!(inbox_unread_for(&store, &wanted).unwrap()["H2"]
            .announced
            .is_some());
    }

    #[test]
    fn an_unconfirmed_offer_waits_out_its_ttl_then_is_offered_again_under_its_token() {
        let (dir, store) = open_temp();
        mail(&store, "H2");
        let (token, _) = offer(&store, "H2", "S2", T0);
        // Its confirm may still be on the way: no second notice yet.
        assert_eq!(
            inbox_reserve_announcement_at(&store, "H2", "S2", T0 + RESERVE_TTL_MS - 1, RUN, false)
                .unwrap(),
            Reservation::PendingUntil(T0 + RESERVE_TTL_MS)
        );
        // Past it, the same notice is offered again (the reservation is in the store).
        drop(store);
        let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
        let later = T0 + RESERVE_TTL_MS;
        assert_eq!(offer(&store, "H2", "S2", later), (token.clone(), 1));
        // Offering it again restarted its wait.
        assert_eq!(
            inbox_reserve_announcement_at(&store, "H2", "S2", later + 1, RUN, false).unwrap(),
            Reservation::PendingUntil(later + RESERVE_TTL_MS)
        );
        assert!(inbox_confirm_announcement(&store, "S2", &token).unwrap());
        assert_eq!(tell(&store, "H2", "S2", later + 1), None);
    }

    #[test]
    fn new_mail_while_an_offer_is_pending_replaces_it_with_one_covering_all_the_mail() {
        let (_dir, store) = open_temp();
        mail(&store, "H2");
        let (first, _) = offer(&store, "H2", "S2", T0);
        mail(&store, "H2");
        let (second, count) = offer(&store, "H2", "S2", T0 + 1);
        assert_ne!(first, second);
        assert_eq!(count, 2);
        // The replaced offer's confirm is stale: it confirms nothing.
        assert!(!inbox_confirm_announcement(&store, "S2", &first).unwrap());
        assert!(inbox_confirm_announcement(&store, "S2", &second).unwrap());
        assert_eq!(tell(&store, "H2", "S2", T0 + 2), None);
    }

    #[test]
    fn a_confirm_is_idempotent_and_never_moves_the_mark_back() {
        let (_dir, store) = open_temp();
        let wanted = ["H2".to_string()];
        let first = mail(&store, "H2");
        let (token, _) = offer(&store, "H2", "S2", T0);
        assert!(inbox_confirm_announcement(&store, "S2", &token).unwrap());
        assert!(!inbox_confirm_announcement(&store, "S2", &token).unwrap());
        let second = mail(&store, "H2");
        assert_eq!(tell(&store, "H2", "S2", T0), Some(2));
        // The first token, replayed, leaves the newer mark where it is.
        assert!(!inbox_confirm_announcement(&store, "S2", &token).unwrap());
        let told = inbox_unread_for(&store, &wanted).unwrap()["H2"]
            .announced
            .clone()
            .unwrap();
        assert!(told.through_id == second && second > first);
        // Another session's ack confirms nothing; nor does a token never issued.
        mail(&store, "H2");
        let (third, _) = offer(&store, "H2", "S2", T0);
        assert!(!inbox_confirm_announcement(&store, "S9", &third).unwrap());
        assert!(!inbox_confirm_announcement(&store, "S2", "nope").unwrap());
    }

    #[test]
    fn unread_mail_reports_when_its_agent_was_last_told_about_it() {
        let (_dir, store) = open_temp();
        let first = mail(&store, "H2");
        let wanted = ["H2".to_string()];
        assert_eq!(
            inbox_unread_for(&store, &wanted).unwrap()["H2"].announced,
            None
        );
        let before = now_ms();
        tell(&store, "H2", "S2", T0);
        let told = inbox_unread_for(&store, &wanted).unwrap()["H2"]
            .announced
            .clone()
            .unwrap();
        assert_eq!((told.through_id, told.session.as_str()), (first, "S2"));
        assert!(told.at_ms >= before);
        // Newer mail is unread past what it was told about.
        let second = mail(&store, "H2");
        let summary = inbox_unread_for(&store, &wanted).unwrap()["H2"].clone();
        assert_eq!(
            (summary.newest_id, summary.announced.unwrap().through_id),
            (Some(second), first)
        );
    }

    #[test]
    fn unread_for_counts_each_recipient_without_marking_anything_read() {
        let (_dir, store) = open_temp();
        let first = inbox_insert(&store, "H1", "H2", "one", None, None, None, None, None).unwrap();
        let second =
            inbox_insert(&store, HUMAN, "H2", "two", None, None, None, None, None).unwrap();
        let read = inbox_insert(&store, "H2", "H3", "read", None, None, None, None, None).unwrap();
        assert!(inbox_mark_read(&store, read, "H3").unwrap());
        inbox_insert(&store, "H2", "H1", "for H1", None, None, None, None, None).unwrap();
        let oldest = inbox_get(&store, first).unwrap().unwrap().created_at_ms;

        let wanted = ["H2", "H3", "nobody"].map(String::from);
        let unread = inbox_unread_for(&store, &wanted).unwrap();
        assert_eq!(
            unread.get("H2"),
            Some(&UnreadSummary {
                count: 2,
                oldest_at_ms: Some(oldest),
                newest_id: Some(second),
                announced: None,
            })
        );
        assert!(!unread.contains_key("H3"), "read mail is not unread");
        assert!(!unread.contains_key("nobody"));
        assert!(!unread.contains_key("H1"), "only the recipients asked for");
        // A view: the mail is still there to read.
        assert_eq!(inbox_take_unread(&store, "H2").unwrap().len(), 2);
        assert!(inbox_unread_for(&store, &wanted).unwrap().is_empty());
        assert!(inbox_unread_for(&store, &[]).unwrap().is_empty());
    }

    #[test]
    fn inbox_insert_take_unread_marks_read_once() {
        let (dir, store) = open_temp();
        let first =
            inbox_insert(&store, "H1", "H2", "status?", None, None, None, None, None).unwrap();
        let second = inbox_insert(
            &store,
            HUMAN,
            "H2",
            "and tests?",
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        inbox_insert(
            &store,
            "H2",
            "H1",
            "not for H2",
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            inbox_unread(&store, "H2").unwrap(),
            InboxUnread {
                count: 2,
                newest_id: Some(second)
            }
        );

        let taken = inbox_take_unread(&store, "H2").unwrap();
        assert_eq!(
            taken.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![first, second]
        );
        assert_eq!((taken[0].from.as_str(), taken[0].to.as_str()), ("H1", "H2"));
        assert_eq!(taken[1].from, HUMAN);
        assert!(taken.iter().all(|m| m.read_at_ms.is_some()));

        assert!(inbox_take_unread(&store, "H2").unwrap().is_empty());
        assert_eq!(inbox_unread(&store, "H2").unwrap(), InboxUnread::default());
        // The other recipient's row is untouched.
        assert_eq!(inbox_unread(&store, "H1").unwrap().count, 1);

        // Rows survive a reopen of the store file (an app restart).
        drop(store);
        let reopened = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
        assert_eq!(inbox_list(&reopened).unwrap().len(), 3);
    }

    #[test]
    fn mark_unread_batch_changes_only_the_recipient_and_preserves_archive() {
        let (_dir, store) = open_temp();
        let human =
            inbox_insert(&store, "H1", HUMAN, "hello", None, None, None, None, None).unwrap();
        let agent =
            inbox_insert(&store, HUMAN, "H1", "reply", None, None, None, None, None).unwrap();
        inbox_mark_read(&store, agent, "H1").unwrap();
        inbox_mark_archived_many(&store, &[human]).unwrap();
        let archived = inbox_get(&store, human).unwrap().unwrap().archived_at_ms;
        assert_eq!(
            inbox_mark_unread_many(&store, &[human, human, agent], HUMAN).unwrap(),
            1
        );
        let message = inbox_get(&store, human).unwrap().unwrap();
        assert!(message.read_at_ms.is_none());
        assert_eq!(message.archived_at_ms, archived);
        assert!(inbox_get(&store, agent)
            .unwrap()
            .unwrap()
            .read_at_ms
            .is_some());
        assert_eq!(inbox_mark_unread_many(&store, &[human], HUMAN).unwrap(), 0);
    }

    #[test]
    fn inbox_get_and_mark_read_only_for_the_recipient() {
        let (_dir, store) = open_temp();
        let asked =
            inbox_insert(&store, "H1", HUMAN, "status?", None, None, None, None, None).unwrap();
        let reply = inbox_insert(
            &store,
            HUMAN,
            "H1",
            "done",
            Some(asked),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            inbox_get(&store, reply).unwrap().unwrap().in_reply_to,
            Some(asked)
        );
        assert_eq!(inbox_get(&store, 999).unwrap(), None);

        // Only the recipient reads a message, once.
        assert!(!inbox_mark_read(&store, asked, "H1").unwrap());
        assert_eq!(inbox_unread(&store, HUMAN).unwrap().count, 1);
        assert!(inbox_mark_read(&store, asked, HUMAN).unwrap());
        assert!(!inbox_mark_read(&store, asked, HUMAN).unwrap());
        assert_eq!(inbox_unread(&store, HUMAN).unwrap(), InboxUnread::default());
    }

    #[test]
    fn inbox_refuses_blank_text() {
        let (_dir, store) = open_temp();
        assert!(matches!(
            inbox_insert(&store, "H1", "H2", "  \n", None, None, None, None, None),
            Err(WvWriteError::Invalid(_))
        ));
        assert!(matches!(
            inbox_insert(&store, "H1", "H2", "", Some(1), None, None, None, None),
            Err(WvWriteError::Invalid(_))
        ));
        assert!(inbox_list(&store).unwrap().is_empty());
    }

    #[test]
    fn inbox_list_is_newest_first_and_marks_nothing_read() {
        let (_dir, store) = open_temp();
        let a = inbox_insert(&store, "H1", "H2", "one", None, None, None, None, None).unwrap();
        let b = inbox_insert(&store, "H2", HUMAN, "two", None, None, None, None, None).unwrap();
        let rows = inbox_list(&store).unwrap();
        assert_eq!(rows.iter().map(|m| m.id).collect::<Vec<_>>(), vec![b, a]);
        assert!(rows.iter().all(|m| m.read_at_ms.is_none()));
        assert_eq!(inbox_unread(&store, "H2").unwrap().count, 1);
        assert_eq!(inbox_unread(&store, HUMAN).unwrap().count, 1);
    }

    #[test]
    fn summary_is_stored_normalized_and_read_back_everywhere() {
        let (_dir, store) = open_temp();
        let id = inbox_insert_with_reply_to(
            &store,
            "H1",
            HUMAN,
            "<h1>Build</h1><p>green</p>",
            None,
            None,
            None,
            None,
            Some(MessageContentType::Html),
            None,
            Some("  Build\n\tis   green\u{7} "),
            None,
        )
        .unwrap();
        let plain = inbox_insert(&store, "H1", HUMAN, "hi", None, None, None, None, None).unwrap();
        let blank = inbox_insert_with_reply_to(
            &store,
            "H1",
            HUMAN,
            "hi",
            None,
            None,
            None,
            None,
            None,
            None,
            Some(" \n "),
            None,
        )
        .unwrap();
        let got = inbox_get(&store, id).unwrap().unwrap();
        assert_eq!(got.summary.as_deref(), Some("Build is green"));
        assert_eq!(inbox_get(&store, plain).unwrap().unwrap().summary, None);
        assert_eq!(inbox_get(&store, blank).unwrap().unwrap().summary, None);
        let listed = inbox_list(&store).unwrap();
        let row = listed.iter().find(|m| m.id == id).unwrap();
        assert_eq!(row.summary.as_deref(), Some("Build is green"));
        let taken = inbox_take_unread(&store, HUMAN).unwrap();
        assert_eq!(taken[0].summary.as_deref(), Some("Build is green"));
    }

    /// SW-AGR-18: the sender's kind is stored as given (NULL when unset), and every
    /// read resolves the effective kind: the set kind, else `needs-you` when the sender
    /// asked for the answer somewhere (`reply_to`), else `update`.
    #[test]
    fn kind_is_stored_as_sent_and_resolves_to_an_effective_kind() {
        let (_dir, store) = open_temp();
        let insert = |kind: Option<MessageKind>, reply_to: Option<&str>| {
            inbox_insert_with_reply_to(
                &store, "H1", HUMAN, "text", None, None, None, None, None, reply_to, None, kind,
            )
            .unwrap()
        };
        let asked = insert(Some(MessageKind::NeedsYou), None);
        let told = insert(Some(MessageKind::Update), Some("role:manager"));
        let plain = insert(None, None);
        let question = insert(None, Some("role:manager"));
        let to_self = insert(None, Some("role:Human"));
        let own = insert(None, Some("H1"));
        let get = |id| inbox_get(&store, id).unwrap().unwrap();
        assert_eq!(get(asked).kind, Some(MessageKind::NeedsYou));
        assert_eq!(get(told).kind, Some(MessageKind::Update));
        assert_eq!(get(plain).kind, None, "an unset kind is stored as NULL");
        assert_eq!(get(asked).effective_kind(), MessageKind::NeedsYou);
        assert_eq!(
            get(told).effective_kind(),
            MessageKind::Update,
            "the sender's kind wins over replyTo"
        );
        assert_eq!(get(plain).effective_kind(), MessageKind::Update);
        assert_eq!(get(question).effective_kind(), MessageKind::NeedsYou);
        assert_eq!(
            get(to_self).effective_kind(),
            MessageKind::Update,
            "a Reply-To naming the human asks nothing of anyone else"
        );
        assert_eq!(
            get(own).effective_kind(),
            MessageKind::Update,
            "a Reply-To naming the sender itself changes nothing"
        );
        // Every read carries it.
        let listed = inbox_list(&store).unwrap();
        assert_eq!(
            listed.iter().find(|m| m.id == asked).unwrap().kind,
            Some(MessageKind::NeedsYou)
        );
        let taken = inbox_take_unread(&store, HUMAN).unwrap();
        assert_eq!(taken[0].kind, Some(MessageKind::NeedsYou));
        // A typed terminal row never waits on anyone.
        let typed = inbox_insert_terminal(&store, "H1", HUMAN, "ls").unwrap();
        assert_eq!(get(typed).effective_kind(), MessageKind::Update);
    }

    /// SW-AGR-18: a Reply-To naming the sender's own role (a manager's digest with
    /// `replyTo: "role:manager"`) is no question; another role's is.
    #[test]
    fn a_reply_to_the_senders_own_role_is_an_update() {
        use super::super::role_inbox::{register_role, release_role};
        let (_dir, store) = open_temp();
        let now = chrono::Utc::now().timestamp();
        let reg = register_role(&store, "manager", "sess-mgr", now).unwrap();
        let holders = role_holders(&store).unwrap();
        assert!(holders.any_holds("manager", &["H1", "sess-mgr"]));
        assert!(!holders.any_holds("manager", &["H2", "sess-worker"]));
        assert!(!holders.any_holds("reviewer", &["sess-mgr"]));
        let holds = |ids: &'static [&'static str]| {
            let holders = holders.clone();
            move |role: &str| holders.any_holds(role, ids)
        };
        let resolve = |reply_to, ids: &'static [&'static str]| {
            MessageKind::resolve(None, Some(reply_to), None, "H1", &holds(ids))
        };
        // The manager's digest: its own role.
        assert_eq!(
            resolve("role:manager", &["H1", "sess-mgr"]),
            MessageKind::Update
        );
        assert_eq!(
            resolve(" role: manager ", &["H1", "sess-mgr"]),
            MessageKind::Update
        );
        // A worker asking for the manager: someone else's role.
        assert_eq!(
            resolve("role:manager", &["H2", "sess-worker"]),
            MessageKind::NeedsYou
        );
        // The manager asking for another role.
        assert_eq!(
            resolve("role:reviewer", &["H1", "sess-mgr"]),
            MessageKind::NeedsYou
        );
        // The sender's own kind still wins.
        assert_eq!(
            MessageKind::resolve(
                Some(MessageKind::NeedsYou),
                Some("role:manager"),
                None,
                "H1",
                &holds(&["H1", "sess-mgr"]),
            ),
            MessageKind::NeedsYou
        );
        // A released role stays known while a receipt or claim records it; with nothing
        // left, the registration's own row is gone.
        release_role(&store, "manager", "sess-mgr", reg.generation).unwrap();
        assert!(!role_holders(&store)
            .unwrap()
            .any_holds("manager", &["sess-mgr"]));
    }

    #[test]
    fn archive_many_changes_only_unarchived_human_mail_and_reads_it() {
        let (_dir, store) = open_temp();
        let insert =
            |to| inbox_insert(&store, "H1", to, "hello", None, None, None, None, None).unwrap();
        let unread = insert(HUMAN);
        let read = insert(HUMAN);
        inbox_mark_read(&store, read, HUMAN).unwrap();
        let read_at = inbox_get(&store, read).unwrap().unwrap().read_at_ms;
        let other = insert("H2");
        assert_eq!(
            inbox_mark_archived_many(&store, &[unread, read, unread, other, 99999]).unwrap(),
            2
        );
        let row = inbox_get(&store, unread).unwrap().unwrap();
        assert!(row.archived_at_ms.is_some());
        assert_eq!(row.archived_at_ms, row.read_at_ms);
        assert_eq!(
            inbox_get(&store, read).unwrap().unwrap().read_at_ms,
            read_at
        );
        let other_row = inbox_get(&store, other).unwrap().unwrap();
        assert!(other_row.archived_at_ms.is_none() && other_row.read_at_ms.is_none());
        assert_eq!(
            inbox_mark_archived_many(&store, &[unread, read]).unwrap(),
            0
        );
    }

    #[test]
    fn unarchive_many_round_trips_archive_and_restores_only_the_named_unread() {
        let (_dir, store) = open_temp();
        let insert =
            |to| inbox_insert(&store, "H1", to, "hello", None, None, None, None, None).unwrap();
        let (first, second, read, live) =
            (insert(HUMAN), insert(HUMAN), insert(HUMAN), insert(HUMAN));
        inbox_mark_read(&store, read, HUMAN).unwrap();
        let read_at = inbox_get(&store, read).unwrap().unwrap().read_at_ms;
        let other = insert("H2");
        inbox_mark_archived_many(&store, &[first, second, read]).unwrap();
        // Archive only ever touches human mail; set an agent's row archived directly so the
        // recipient guard is what keeps unarchive off it.
        store
            .conn()
            .unwrap()
            .execute(
                "UPDATE wv_inbox SET archived_at = 1, read_at = 1 WHERE id = ?1",
                params![other],
            )
            .unwrap();
        let mutations = || -> i64 {
            store
                .conn()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM wv_write_log WHERE tbl = ?1 AND operation = 'mutate'",
                    params![INBOX],
                    |r| r.get(0),
                )
                .unwrap()
        };
        let before = mutations();
        // A live row, someone else's mail and an unknown id are unchanged, even if named unread.
        assert_eq!(
            inbox_unarchive_many(
                &store,
                &[first, second, read, first, live, other, 99999],
                &[first, second, live, other],
            )
            .unwrap(),
            3
        );
        assert_eq!(mutations() - before, 3, "one audit row per changed message");
        for id in [first, second] {
            let row = inbox_get(&store, id).unwrap().unwrap();
            assert!(row.archived_at_ms.is_none() && row.read_at_ms.is_none());
        }
        let row = inbox_get(&store, read).unwrap().unwrap();
        assert!(row.archived_at_ms.is_none());
        assert_eq!(
            row.read_at_ms, read_at,
            "a row read before archiving stays read"
        );
        assert!(inbox_get(&store, live)
            .unwrap()
            .unwrap()
            .read_at_ms
            .is_none());
        let other_row = inbox_get(&store, other).unwrap().unwrap();
        assert_eq!((other_row.archived_at_ms, other_row.read_at_ms), (Some(1), Some(1)));
        assert_eq!(inbox_unread(&store, HUMAN).unwrap().count, 3);
        assert_eq!(
            inbox_unarchive_many(&store, &[first, second, read], &[]).unwrap(),
            0
        );
        assert_eq!(mutations() - before, 3);
    }

    #[test]
    fn mark_read_many_reads_only_the_readers_unread_mail_in_one_go() {
        let (_dir, store) = open_temp();
        let mine: Vec<i64> = (0..500)
            .map(|i| {
                inbox_insert(
                    &store,
                    "H1",
                    HUMAN,
                    &format!("r{i}"),
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap()
            })
            .collect();
        let other = inbox_insert(
            &store,
            "H1",
            "H2",
            "not yours",
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let mut ids = mine.clone();
        ids.push(other);
        ids.push(99_999);
        assert_eq!(inbox_mark_read_many(&store, &ids, HUMAN).unwrap(), 500);
        assert_eq!(inbox_unread(&store, HUMAN).unwrap().count, 0);
        assert_eq!(
            inbox_unread(&store, "H2").unwrap().count,
            1,
            "someone else's mail"
        );
        assert_eq!(
            inbox_mark_read_many(&store, &ids, HUMAN).unwrap(),
            0,
            "already read"
        );
        assert_eq!(inbox_mark_read_many(&store, &[], HUMAN).unwrap(), 0);
    }

    #[test]
    fn message_kind_reads_only_its_two_spellings() {
        assert_eq!(
            MessageKind::from_stored(Some("needs-you")),
            Some(MessageKind::NeedsYou)
        );
        assert_eq!(
            MessageKind::from_stored(Some(" update ")),
            Some(MessageKind::Update)
        );
        assert_eq!(MessageKind::from_stored(Some("urgent")), None);
        assert_eq!(MessageKind::from_stored(None), None);
        assert_eq!(
            serde_json::to_value(MessageKind::NeedsYou).unwrap(),
            serde_json::json!("needs-you")
        );
        assert!(serde_json::from_value::<MessageKind>(serde_json::json!("urgent")).is_err());
    }

    #[test]
    fn normalize_summary_drops_bidi_and_zero_width_format_chars() {
        // A right-to-left override would visually reorder the rest of the row.
        assert_eq!(
            normalize_summary(Some("\u{202E}evil\u{202C} from \u{2066}human\u{2069}")).as_deref(),
            Some("evil from human")
        );
        for c in ('\u{200B}'..='\u{200F}')
            .chain('\u{202A}'..='\u{202E}')
            .chain('\u{2066}'..='\u{2069}')
            .chain(['\u{FEFF}'])
        {
            assert_eq!(
                normalize_summary(Some(&format!("a{c}b"))).as_deref(),
                Some("ab"),
                "U+{:04X} is dropped",
                c as u32
            );
            assert_eq!(
                normalize_summary(Some(&c.to_string())),
                None,
                "U+{:04X}",
                c as u32
            );
        }
        // Ordinary non-ASCII text is kept.
        assert_eq!(
            normalize_summary(Some("café — 日本語 ✓")).as_deref(),
            Some("café — 日本語 ✓")
        );
    }

    #[test]
    fn normalize_summary_redacts_secrets_like_the_body() {
        let got =
            normalize_summary(Some("deploy with ghp_abcdefghijklmnopqrstuvwxyz0123 now")).unwrap();
        assert!(!got.contains("ghp_abcdefghijklmnopqrstuvwxyz0123"), "{got}");
        assert!(
            got.starts_with("deploy with ") && got.ends_with(" now"),
            "{got}"
        );
        assert_eq!(
            normalize_summary(Some("token: hunter2secret")).as_deref(),
            Some("token: [redacted]")
        );
    }

    #[test]
    fn normalize_summary_collapses_caps_and_drops_blank() {
        assert_eq!(normalize_summary(None), None);
        assert_eq!(normalize_summary(Some("")), None);
        assert_eq!(normalize_summary(Some(" \r\n\t ")), None);
        assert_eq!(normalize_summary(Some("\u{0}\u{1b}")), None);
        assert_eq!(
            normalize_summary(Some(" one\n\ntwo \t three ")).as_deref(),
            Some("one two three")
        );
        assert_eq!(
            normalize_summary(Some("bel\u{7}l")).as_deref(),
            Some("bell"),
            "a control char is dropped, not spaced"
        );
        let exact = "a".repeat(SUMMARY_MAX_CHARS);
        assert_eq!(normalize_summary(Some(&exact)), Some(exact.clone()));
        // Cut on a char boundary (multi-byte chars), ellipsis included in the cap.
        let long = "é".repeat(SUMMARY_MAX_CHARS + 50);
        let cut = normalize_summary(Some(&long)).unwrap();
        assert_eq!(cut.chars().count(), SUMMARY_MAX_CHARS);
        assert!(cut.ends_with('…'));
        assert!(cut.starts_with("éé"));
        // A cut that lands after a space does not leave "word …".
        let words = format!("{} tail", "x".repeat(SUMMARY_MAX_CHARS - 2));
        let cut = normalize_summary(Some(&words)).unwrap();
        assert!(cut.ends_with("x…"), "{cut}");
        assert!(cut.chars().count() <= SUMMARY_MAX_CHARS);
    }

    #[test]
    fn legacy_inbox_migrates_without_guessing_reply_origin() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.sqlite3");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE wv_inbox (id INTEGER PRIMARY KEY AUTOINCREMENT, sender TEXT NOT NULL,
             recipient TEXT NOT NULL, text TEXT NOT NULL, created_at INTEGER NOT NULL,
             read_at INTEGER, in_reply_to INTEGER);
             INSERT INTO wv_inbox VALUES (1,'H1','H2','original',10,77,NULL);
             INSERT INTO wv_inbox VALUES (2,'H2','H1','legacy reply',20,NULL,1);",
        )
        .unwrap();
        drop(conn);
        let store = WeftStore::open(&path).unwrap();
        let original = inbox_get(&store, 1).unwrap().unwrap();
        let reply = inbox_get(&store, 2).unwrap().unwrap();
        assert_eq!(
            (original.text.as_str(), original.read_at_ms),
            ("original", Some(77))
        );
        assert_eq!(
            (reply.text.as_str(), reply.in_reply_to, reply.read_at_ms),
            ("legacy reply", Some(1), None)
        );
        assert_eq!(reply.in_reply_to_team, None);
        // The summary column is added on open; a legacy row has none.
        assert_eq!((original.summary, reply.summary.as_deref()), (None, None));
        // So is the kind column (SW-AGR-18): a legacy row has none and reads as an update.
        assert_eq!((original.kind, reply.kind), (None, None));
        assert_eq!(reply.effective_kind(), MessageKind::Update);
        // A partially populated migration cannot establish provenance either.
        store
            .conn()
            .unwrap()
            .execute("UPDATE wv_inbox SET in_reply_to_org='acme' WHERE id=2", [])
            .unwrap();
        assert_eq!(
            inbox_get(&store, 2).unwrap().unwrap().in_reply_to_team,
            None
        );
        let team = InboxReplyTeam {
            org: "acme".into(),
            team: "design".into(),
        };
        let fresh = inbox_insert(
            &store,
            "H1",
            "H2",
            "new reply",
            Some(2),
            Some(&team),
            None,
            None,
            None,
        )
        .unwrap();
        drop(store);
        let reopened = WeftStore::open(path).unwrap();
        assert_eq!(
            inbox_get(&reopened, fresh)
                .unwrap()
                .unwrap()
                .in_reply_to_team,
            Some(team)
        );
        assert_eq!(inbox_list(&reopened).unwrap().len(), 3);
    }

    #[test]
    fn a_terminal_row_is_stored_read_and_marked_via_terminal() {
        let (_dir, store) = open_temp();
        let id = inbox_insert_terminal(&store, "H1", "H2", "run the tests").unwrap();
        let row = inbox_get(&store, id).unwrap().unwrap();
        assert_eq!(row.via.as_deref(), Some(VIA_TERMINAL));
        assert_eq!(row.read_at_ms, Some(row.created_at_ms));
        assert_eq!(row.content_type, Some(MessageContentType::Plain));
        assert_eq!(inbox_unread(&store, "H2").unwrap(), InboxUnread::default());
        assert!(inbox_take_unread(&store, "H2").unwrap().is_empty());
        // Ordinary mail carries no via.
        let mail = inbox_insert(&store, "H1", "H2", "hi", None, None, None, None, None).unwrap();
        assert_eq!(inbox_get(&store, mail).unwrap().unwrap().via, None);
        assert!(matches!(
            inbox_insert_terminal(&store, "H1", "H2", " "),
            Err(WvWriteError::Invalid(_))
        ));
    }

    #[test]
    fn a_legacy_inbox_gains_the_via_column() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.sqlite3");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE wv_inbox (id INTEGER PRIMARY KEY AUTOINCREMENT, sender TEXT NOT NULL,
             recipient TEXT NOT NULL, text TEXT NOT NULL, created_at INTEGER NOT NULL,
             read_at INTEGER, in_reply_to INTEGER);
             INSERT INTO wv_inbox VALUES (1,'H1','H2','old',10,NULL,NULL);",
        )
        .unwrap();
        drop(conn);
        let store = WeftStore::open(&path).unwrap();
        assert_eq!(inbox_get(&store, 1).unwrap().unwrap().via, None);
        let id = inbox_insert_terminal(&store, "H1", "H2", "typed").unwrap();
        assert_eq!(
            inbox_get(&store, id).unwrap().unwrap().via.as_deref(),
            Some(VIA_TERMINAL)
        );
    }

    #[test]
    fn a_session_list_covers_its_own_mail_and_its_role_queues() {
        let (_dir, store) = open_temp();
        let sent = inbox_insert(&store, "H1", HUMAN, "out", None, None, None, None, None).unwrap();
        let got = inbox_insert(&store, HUMAN, "S1", "in", None, None, None, None, None).unwrap();
        let other = inbox_insert(
            &store, "H2", HUMAN, "not mine", None, None, None, None, None,
        )
        .unwrap();
        let (queued, _) = crate::weft::role_inbox::send_role_message_commit(
            &store, HUMAN, "reviewer", "review", None, None, None, "", 0, None, None, None,
        )
        .unwrap();
        let addresses = ["H1".to_string(), "S1".to_string()];
        let ids = |roles: &[String]| {
            inbox_list_for_session(&store, &addresses, roles)
                .unwrap()
                .into_iter()
                .map(|m| m.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&[]), vec![got, sent]);
        assert_eq!(ids(&["reviewer".into()]), vec![queued, got, sent]);
        assert!(!ids(&["reviewer".into()]).contains(&other));
        assert!(inbox_list_for_session(&store, &[], &[]).unwrap().is_empty());
    }
}

/// Replace a message's image metadata after the host fetch completes.
pub(crate) fn inbox_set_attachments(
    store: &WeftStore,
    id: i64,
    attachments: &[crate::media::Attachment],
) -> WvWriteResult<()> {
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let json = serde_json::to_string(attachments).unwrap();
    tx.execute(
        "UPDATE wv_inbox SET attachments=?2 WHERE id=?1",
        params![id, json],
    )?;
    write_audit_public(&tx, INBOX, "mutate", id)?;
    tx.commit()?;
    Ok(())
}
