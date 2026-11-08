// SPDX-License-Identifier: MIT
//! Agents message each other, and the human, through a durable inbox.
//!
//! `send_message` stores a row, `read_messages` hands the caller its unread rows once,
//! and `reply` answers a message to whoever sent it, or to the message's `replyTo` when it
//! names one (email Reply-To: the answer goes only there; `role:<id>` queues it for that
//! role, so it outlives the sender). MCP supplies its verified session to this port;
//! the native app has its own human path. A session is addressed by its launch handle,
//! or, for a hand-opened tab, its session id (as `list_sessions` lists it). A named
//! session that is not running is refused, never recorded as the human. Only the app
//! reads the human's inbox or marks it read. Anyone may message anyone:
//! the only refusals are inputs that cannot be stored (blank text, a sender that is not
//! running, an unknown recipient or message). A sender shows under its tab's current
//! name, so a rename follows it.
//!
//! Every team has its own inbox, in its weft store. A session's mail lives in the team
//! it was launched in (else the UI-active team), so it stays put when the person switches
//! teams in the app. Mail to the human lives in the sender's team. The app reads and
//! answers the human's mail of the team it names, never of whatever team is active in
//! the backend at that moment.
//!
//! Nothing is ever typed into a recipient's terminal by this module. An agent learns
//! about its mail from the host its tab runs on (this Mac's terminal server, or a
//! machine's), through its CLI's inbox port: a Claude's own inbox waiter (an
//! `asyncRewake` hook armed at its boot, every turn end and after each tool call) wakes
//! it when mail arrives, so an idle agent starts a turn and a running one hears at its
//! next model request. The inbox store records which mail each session was told about:
//! a notice is reserved when it is offered ([`inbox::inbox_reserve_announcement`]) and
//! pushed to the session's host (`super::host_inbox`), and marks its mail told only once
//! the host says the agent has it ([`confirm_notice`]), so each batch of new mail is
//! announced once, across app restarts too, an agent that never reads is not told in a
//! loop, and a notice lost before it was told is offered again.
//!
//! The decisions run over the [`InboxHost`] port so they are tested end to end against
//! real stores without a PTY; [`AppInboxHost`] is the production port.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::await_blocking;
use super::dashboard_launch::LaunchTeam;
use super::route_to_context::ContextResolution;
use crate::live_hub::inbox_events::{InboxChange, InboxEvent, INBOX_EVENT};
use crate::weft::inbox::{
    self, InboxDelivery, InboxMessage, InboxReplyTeam, MessageOnClick, ReplyTo, HUMAN,
    REPLY_TO_ROLE_PREFIX,
};
use crate::weft::reactions;
use crate::weft::role_inbox::WithdrawOutcome;
use crate::weft::sqlite::{WvWriteError, WvWriteResult};
use crate::weft::WeftStore;

/// Tells the viewer the inbox changed, so the Agents panel's Messages list refetches.
/// Mirrored in `src/tauri/events.ts`.
pub(crate) const AGENT_INBOX_CHANGED_EVENT: &str = "agent-inbox-changed";

/// The registry and store side of the inbox.
#[async_trait::async_trait]
pub(crate) trait InboxHost: Send + Sync {
    fn media_config(&self, _team: &LaunchTeam) -> crate::media::Config {
        crate::media::Config::default()
    }
    fn present_media(&self, team: &LaunchTeam, attachments: &mut [crate::media::Attachment]) {
        if !attachments.is_empty() {
            self.media_config(team).apply(attachments);
        }
    }
    /// The bytes of a cached image by sha256 (the content-addressed media cache).
    fn cached_media(&self, _sha: &str) -> Result<Vec<u8>, String> {
        Err("no media cache".into())
    }
    async fn own_media_sender(&self, _sender: &str) -> bool {
        false
    }
    /// Read authority of the authenticated sender, independent of the inbox store team.
    async fn caller_scope(&self, _sender: &str) -> crate::context_repo::caller_scope::CallerScope {
        crate::context_repo::caller_scope::CallerScope::Unknown
    }
    async fn caller_media_workspace(&self, _sender: &str) -> Option<std::path::PathBuf> {
        None
    }
    async fn prepare_media(
        &self,
        team: &LaunchTeam,
        input: &crate::media::MediaInput,
        own: bool,
        _scope: &crate::context_repo::caller_scope::CallerScope,
        _workspace_root: Option<&std::path::Path>,
    ) -> Result<(crate::media::Attachment, bool), String> {
        input.validate()?;
        let config = self.media_config(team);
        let attachment = crate::media::Attachment::remote(
            input.url.as_deref().ok_or("local media unavailable")?,
            &input.alt,
            &config,
            own,
        )
        .map_err(|e| e.reason.to_string())?;
        Ok((attachment, false))
    }
    async fn fetch_media(
        &self,
        _team: &LaunchTeam,
        _attachment: crate::media::Attachment,
        _automatic: bool,
    ) -> Result<crate::media::Attachment, String> {
        Err("media fetch unavailable".into())
    }

    async fn admit_automatic_input(&self, _session: &str) -> Result<(), String> {
        Ok(())
    }

    async fn chat_announcement(
        &self,
        _session: &str,
        _handle: &str,
    ) -> Result<Option<inbox::ChatAnnouncement>, String> {
        Ok(None)
    }
    async fn chat_told(
        &self,
        _session: &str,
        _handle: &str,
        _chat: &inbox::ChatAnnouncement,
    ) -> Result<(), String> {
        Ok(())
    }
    /// The inbox store of `team`. `Err` for a team this machine does not hold (unknown,
    /// removed or not synced); nothing is created for it.
    async fn team_store(&self, team: &LaunchTeam) -> Result<PathBuf, String>;
    async fn role_configs(
        &self,
        _team: &LaunchTeam,
    ) -> Result<Vec<crate::queue_roles::RoleConfig>, String> {
        Ok(vec![])
    }

    async fn role_configs_for(
        &self,
        team: &LaunchTeam,
        _scope: &crate::context_repo::caller_scope::CallerScope,
    ) -> Result<Vec<crate::queue_roles::RoleConfig>, String> {
        self.role_configs(team).await
    }

    /// The team active in the app, for a session whose launch team is unknown.
    async fn active_team(&self) -> Result<LaunchTeam, String>;
    /// The team the session under `handle` was launched in, if recorded.
    fn launch_team(&self, handle: &str) -> Option<LaunchTeam>;
    /// The team of the workspace whose tab runs the session under `handle` on this Mac,
    /// read from the terminal layout: a tab opened by hand records no launch team.
    async fn tab_team(&self, _handle: &str) -> Option<LaunchTeam> {
        None
    }
    /// The handle `session_id` is bound to, if any.
    fn handle_for_session(&self, session_id: &str) -> Option<String>;
    /// Every session `list_sessions` lists now, with its handle, its tab session, its
    /// current name and whether it runs. Read once per inbox call.
    async fn sessions(&self) -> Result<KnownSessions, String>;
    /// Whether the launch registry holds `handle`.
    fn has_handle(&self, handle: &str) -> bool;
    /// The name the session under `handle` was launched with, if any.
    fn display_name(&self, handle: &str) -> Option<String>;
    /// Tell the viewer the inbox changed.
    fn projected_inbox(&self, _team: &LaunchTeam) -> Option<Result<InboxView, String>> {
        None
    }
    fn inbox_changed(&self);
    /// The live hub committed a projection pass: `changes` names, per team, the messages it
    /// re-read. Readers that refresh on the notice find those messages committed.
    fn inbox_committed(&self, _changes: &[(LaunchTeam, Vec<i64>)]) {
        self.inbox_changed();
    }
    fn inbox_event(&self, _event: InboxEvent) {
        self.inbox_changed();
    }
    /// A message was stored for the session under `handle`. The app offers its host the
    /// inbox notice and tells routines with an `inbox_idle` trigger.
    fn mail_stored(&self, _handle: &str) {}
    /// Message `message_id` was queued for `role` in `team`'s inbox. The app tells
    /// routines with a `role_pending` trigger (a controller watching the queues), which
    /// decide whether to start a holder; nothing claims it here. `sender` is who the
    /// guard treats as sending it, so a routine never fires on a message its own run
    /// queued: the calling tab's session id for a local MCP call, the authority's handle
    /// for a remote one, otherwise the stored sender (a session handle), or the human.
    /// It is not necessarily the stored sender.
    fn role_message_queued(
        &self,
        _team: &LaunchTeam,
        _role: &str,
        _message_id: i64,
        _sender: &str,
    ) {
    }
    /// Every team synced on this machine, whose inboxes a cross-team read covers.
    async fn synced_teams(&self) -> Result<Vec<LaunchTeam>, String> {
        Ok(Vec::new())
    }
    /// Include a session-linked role store in the existing renewal lifecycle.
    fn note_role_store(&self, _path: PathBuf) {}
    /// A role queue changed (a reply was queued for a role), so role views re-read.
    fn roles_changed(&self) {}
    /// Whether local launch restoration has settled, so inbox notices can use the
    /// restored tab's kept handle. Default true for hosts with no restoration phase.
    fn launch_restore_settled(&self) -> bool {
        true
    }
    /// SW-BSI-3: the story that owns `logical_path` within `team`, if any linked
    /// worktree record's epic subtree is an ancestor. Powers the agent-facing
    /// attachment hint on `read_messages`: a remote agent whose own `read_file`
    /// misses the file can self-recover with `hydrate_story` for `story_id`.
    /// `None` is the honest "owner unknown" case — the UI falls through to its
    /// generic unavailable rendering. Default returns `None` so a `FakeHost` in
    /// tests needs no new fixture.
    async fn owning_story_for_path(
        &self,
        _team: &LaunchTeam,
        _logical_path: &str,
    ) -> Option<String> {
        None
    }
}

/// A session as the inbox addresses and names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KnownSession {
    pub handle: String,
    /// The tab session it runs under (its `$CODESENSE_TAB_SESSION_ID`), when known.
    pub session: Option<String>,
    /// Its name now: its tab's current name, else the name it was launched with.
    pub name: String,
    /// It runs now: not exited, and its machine is connected.
    pub running: bool,
}

/// The sessions one inbox call resolves its addresses and names against.
#[derive(Debug, Clone, Default)]
pub(crate) struct KnownSessions(Vec<KnownSession>);

impl KnownSessions {
    pub(crate) fn new(sessions: Vec<KnownSession>) -> Self {
        Self(sessions)
    }

    /// The session `id` names: by its handle, else by its tab session (a running one
    /// first, should an ended tab still hold the same id).
    pub(crate) fn find(&self, id: &str) -> Option<&KnownSession> {
        let id = id.trim();
        self.0.iter().find(|s| s.handle == id).or_else(|| {
            let mut by_session = self.0.iter().filter(|s| s.session.as_deref() == Some(id));
            let first = by_session.next()?;
            Some(if first.running {
                first
            } else {
                by_session.find(|s| s.running).unwrap_or(first)
            })
        })
    }
}

impl From<&crate::mcp::session_list::SessionListRow> for KnownSession {
    fn from(row: &crate::mcp::session_list::SessionListRow) -> Self {
        KnownSession {
            handle: row.handle.clone(),
            session: row.session.clone(),
            name: row.display_name.clone(),
            running: crate::mcp::identity_tools::is_running(row.state),
        }
    }
}

/// The team an app command acts on, as the viewer names it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct InboxTeam {
    pub org: String,
    pub team: String,
}

impl From<InboxTeam> for LaunchTeam {
    fn from(t: InboxTeam) -> Self {
        LaunchTeam {
            org: t.org,
            team: t.team,
        }
    }
}

/// What a sender of an unmarked question to the human is told.
const QUESTION_HINT: &str = "This reads like a question, but it was sent with no kind, so it defaults to update. Both kinds appear in the human Inbox; kind marks whether it waits on them for agent views and dashboards. If it needs an answer, send it with kind: \"needs-you\" (or ask with ask_user and wait).";

/// The outcome of `send_message`, `reply` and the human's reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub(crate) enum SendResult {
    Sent {
        #[serde(rename = "messageId")]
        message_id: i64,
        /// SW-AGR-18: for a message to the human, the kind it counts as there
        /// (`needs-you` or `update`).
        #[serde(skip_serializing_if = "Option::is_none")]
        kind: Option<inbox::MessageKind>,
        /// A hint for the sender, e.g. an unmarked question that defaulted to update.
        #[serde(skip_serializing_if = "Option::is_none")]
        hint: Option<String>,
    },
    /// An input that cannot be stored; `message` says which, in one sentence.
    Invalid { message: String },
}

impl ReactResult {
    fn invalid(message: impl std::fmt::Display) -> Self {
        ReactResult::Invalid {
            message: message.to_string(),
        }
    }
}

impl SendResult {
    fn sent(message_id: i64) -> Self {
        SendResult::Sent {
            message_id,
            kind: None,
            hint: None,
        }
    }

    /// A message to the human: say which lane it lands in, and when it was sent with no
    /// kind yet reads like a question (`?` at its end), say how to ask instead. The
    /// message is never reclassified by its words.
    fn for_human(
        self,
        set: Option<inbox::MessageKind>,
        effective: inbox::MessageKind,
        text: &str,
    ) -> Self {
        match self {
            SendResult::Sent { message_id, .. } => {
                let asks = set.is_none()
                    && effective == inbox::MessageKind::Update
                    && text.trim_end().ends_with('?');
                SendResult::Sent {
                    message_id,
                    kind: Some(effective),
                    hint: asks.then(|| QUESTION_HINT.to_string()),
                }
            }
            other => other,
        }
    }

    fn invalid(reason: impl std::fmt::Display) -> Self {
        SendResult::Invalid {
            message: format!("Not sent: {reason}."),
        }
    }
}

/// One message as `read_messages` returns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReadMessage {
    pub message_id: i64,
    pub from: String,
    pub from_name: String,
    pub text: String,
    pub in_reply_to: Option<i64>,
    pub in_reply_to_team: Option<InboxReplyTeam>,
    /// Where `reply` sends the answer instead of to `from` (a session handle, `human`
    /// or `role:<id>`), when the sender set one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// The sender's one-line plain-text preview of `text`, when it set one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// SW-BSI-3: the managed-file reference the sender attached, if any. A
    /// remote agent uses this with the matching `attachment_hint` to copy in
    /// the file on its own machine before reading.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_file: Option<String>,
    #[serde(default)]
    pub attachments: Vec<crate::media::Attachment>,
    /// SW-BSI-3: when `primary_file` is set and we can resolve the story it
    /// belongs to, hand the caller the shape of the recovery call so a remote
    /// agent whose own `read_file` misses the attachment can self-serve
    /// `hydrate_story` (with `story_id`) without a round trip through the human.
    /// Absent when there is no attachment or the owning story is unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachment_hint: Option<AttachmentHint>,
}

/// SW-BSI-3: a structured hint pointing at the recovery call for a remote
/// agent whose workspace lacks an attachment. Shaped so the agent can
/// act on it mechanically without parsing free-text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttachmentHint {
    /// The story whose closure (epic subtree) contains the attachment. Pass as
    /// `storyId` to `hydrate_story`.
    pub owning_story_id: String,
    /// The MCP verb the caller invokes to copy the attachment into their
    /// own machine. Fixed string so an agent script can match on it directly.
    pub suggested_call: &'static str,
    /// A one-sentence human-readable explanation for logs and transcripts. No
    /// agent should branch on this text — it is advisory, not a contract.
    pub note: String,
}

/// One row of the human's Messages list: every message in the inbox, whoever it is for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InboxRow {
    pub conversation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_role: Option<String>,
    pub message_id: i64,
    pub from: String,
    pub from_name: String,
    pub to: String,
    pub to_name: String,
    pub text: String,
    pub created_at_ms: i64,
    pub in_reply_to: Option<i64>,
    pub in_reply_to_team: Option<InboxReplyTeam>,
    /// Addressed to the human, who can reply to it from the list.
    pub to_human: bool,
    pub unread: bool,
    /// SW-BSI-2: optional managed-file reference the human previews (Markdown/HTML).
    /// `None` is a standalone message; the UI opens no document tab for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_file: Option<String>,
    pub attachments: Vec<crate::media::Attachment>,
    /// SW-BSI-UX: when `Some`, the human has archived this row. The UI drops archived
    /// threads out of the default "needs you" lane into "done".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_at_ms: Option<i64>,
    /// Where a message to an agent's role inbox stands, or its withdrawal; `None` for a
    /// plain message nobody withdrew.
    pub delivery: Option<InboxDelivery>,
    /// Per-message click action chosen by the sender (goto-agent / open-file /
    /// show-message). `None` → the Inbox UI opens the conversation view.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_click: Option<MessageOnClick>,
    /// How the body should render: `plain`, `markdown` (default), or `html`
    /// (strictly sanitized FE-side). `None` → the FE renderer applies its
    /// default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<inbox::MessageContentType>,
    /// Where replies to this message go instead of its sender: a session handle,
    /// `human`, or `role:<id>`. Absent → replies go to `from`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// How `reply_to` shows: a session's current tab name, or the role id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to_name: Option<String>,
    /// The sender's one-line plain-text preview, for list views (the body may be
    /// html or long markdown). Absent → derive the preview from `text`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// A reply to this message would go to a session that is no longer running (the
    /// `reply_to` session, else the sender): the reply waits in its inbox unread.
    pub reply_target_ended: bool,
    /// How the row got here when it is not mail: `terminal` for text typed into the
    /// recipient's terminal (`send_to_session`), stored read since the recipient already
    /// has it. Absent for mail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// SW-AGR-18: what it asks of the human — `needs-you` (waits on them) or `update`
    /// (informational). The sender's kind, else `needs-you` when it set a `replyTo`
    /// naming someone other than the human, else `update`.
    pub kind: inbox::MessageKind,
    /// The emoji reactions on it, grouped by emoji with their distinct reactors. A
    /// reaction is not mail: it leaves `unread` and every count alone.
    pub reactions: Vec<crate::weft::reactions::ReactionGroup>,
}

/// SW-BSI-UX: which recipient class `list_inbox_messages` returns. `ToHuman` is the
/// original Messages list (human-addressed only); `ToAgents` is the observability
/// lens (role or session recipients — everything not `human`); `All` returns both.
#[derive(Debug, Clone, Copy, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum InboxScope {
    #[default]
    #[serde(rename = "to-human")]
    ToHuman,
    #[serde(rename = "to-agents")]
    ToAgents,
    #[serde(rename = "all")]
    All,
}

impl InboxScope {
    pub(crate) fn admits(&self, to: &str) -> bool {
        let is_human = to == HUMAN;
        match self {
            InboxScope::ToHuman => is_human,
            InboxScope::ToAgents => !is_human,
            InboxScope::All => true,
        }
    }
}

/// The human's Messages list: the newest rows, and how many messages to the human are
/// unread in the whole inbox (not only in the listed rows).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InboxView {
    pub conversations: Vec<super::agent_read_model::ConversationSummary>,
    pub has_more: bool,
    /// How many messages matched before the limit cut `messages`; 0 when the view was not limited.
    /// Not part of the serialized view.
    #[serde(skip)]
    pub matched: usize,
    pub messages: Vec<InboxRow>,
    pub human_unread: u32,
}

/// How long one inbox check may take. Past it, or on a store error, the check is tried
/// again after a pause: the arm that prompted it is not lost to a slow store.
const CHECK_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// What one inbox check found for a session's agent.
#[derive(Debug)]
pub(crate) enum Check {
    /// A notice to hand the agent, and the token that confirms it was told.
    Offer { notice: String, token: String },
    /// Nothing it was not told about.
    Nothing,
    /// A notice offered earlier awaits its confirm: check again after this long.
    PendingFor(std::time::Duration),
    /// Whether the agent has the notice offered earlier is unknown: it is not offered
    /// again until it is settled, or newer mail comes.
    Held,
}

/// One check of `session_id`'s inbox for mail it was not told about, reserving a notice
/// for it (with `reoffer`, an unconfirmed one is offered again at once): `Err` when the
/// check failed or ran past its budget (to be tried again; a reservation that committed
/// late is offered again once it lapses).
pub(crate) async fn check_for_mail(
    host: &dyn InboxHost,
    session_id: &str,
    reoffer: bool,
) -> Result<Check, ()> {
    let check = reserved_notice(host, session_id, reoffer);
    match tokio::time::timeout(CHECK_BUDGET, check).await {
        Ok(Ok(check)) => Ok(check),
        Ok(Err(())) | Err(_) => {
            log::warn!(target: "inbox", "inbox check for {session_id} failed or took over {CHECK_BUDGET:?}; trying again");
            Err(())
        }
    }
}

/// Who an inbox call speaks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Caller<'a> {
    /// A verified MCP session or an internal native selection (`None`: the person).
    /// A named session that is not running is refused.
    Named(Option<&'a str>),
    /// The tab handle an authenticated connection runs in (a remote agent's reverse
    /// MCP authority). It is the caller whatever the arguments say, and its liveness is
    /// the authority's to prove, not the session list's.
    Authenticated(&'a str),
}

impl<'a> From<Option<&'a str>> for Caller<'a> {
    fn from(session_id: Option<&'a str>) -> Self {
        Caller::Named(session_id)
    }
}

/// Resolve a verified MCP session or an internal native-app caller to its live handle.
/// Unknown or ended named sessions are refused, never reinterpreted as the person.
fn caller_handle<'a>(
    sessions: &KnownSessions,
    caller: impl Into<Caller<'a>>,
) -> Result<Option<String>, String> {
    let session_id = match caller.into() {
        Caller::Authenticated(handle) => {
            let handle = handle.trim();
            if handle.is_empty() {
                return Err("the authenticated tab has no handle".into());
            }
            return Ok(Some(
                sessions
                    .find(handle)
                    .map_or_else(|| handle.to_string(), |s| s.handle.clone()),
            ));
        }
        Caller::Named(session_id) => session_id,
    };
    let Some(sid) = session_id.map(str::trim).filter(|sid| !sid.is_empty()) else {
        return Ok(None);
    };
    match sessions.find(sid) {
        Some(session) if session.running => Ok(Some(session.handle.clone())),
        Some(session) => Err(format!(
            "session '{sid}' (\"{}\") is not running; pass a running session's id, normally your own $CODESENSE_TAB_SESSION_ID",
            session.name
        )),
        None => Err(format!(
            "no session '{sid}' is running; pass a running session's id, normally your own $CODESENSE_TAB_SESSION_ID (who_am_i shows who that is)"
        )),
    }
}

/// The handle `session_id` speaks as, resolved like [`caller_handle`]: `Ok(Ok(None))` for
/// the human, `Ok(Err(reason))` for a session that is not running. The outer `Err` is a
/// failure to list the sessions.
pub(crate) async fn resolve_caller<'a>(
    host: &dyn InboxHost,
    caller: impl Into<Caller<'a>>,
) -> Result<Result<Option<String>, String>, String> {
    let caller = caller.into();
    let sessions = host.sessions().await?;
    Ok(caller_handle(&sessions, caller))
}

/// A recipient as the inbox stores it: `human`, or a session's handle (its session id is
/// accepted for it). A session that is not running may still be written to: its mail
/// waits. `None` for an address nobody holds.
fn recipient_address(host: &dyn InboxHost, sessions: &KnownSessions, to: &str) -> Option<String> {
    let to = to.trim();
    // `Human` / `HUMAN` is the human, as a bare name is everywhere else.
    if to.eq_ignore_ascii_case(HUMAN) {
        return Some(HUMAN.to_string());
    }
    let handle = session_handle(host, to, Some(sessions));
    (sessions.find(to).is_some() || host.has_handle(&handle)).then_some(handle)
}

/// The address a session names. A captured session list takes precedence over the
/// launch registry; a hand-started session without a binding keeps its own id.
pub(super) fn session_handle(
    host: &dyn InboxHost,
    session: &str,
    sessions: Option<&KnownSessions>,
) -> String {
    sessions
        .and_then(|sessions| sessions.find(session))
        .map(|session| session.handle.clone())
        .or_else(|| host.handle_for_session(session))
        .unwrap_or_else(|| session.to_string())
}

/// A `replyTo` as the inbox stores it: `role:<id>`, `human`, or the handle of a running
/// session (its session id is accepted for it). `Ok(None)` when none is given; `Err`
/// says, in one sentence, why one cannot be used.
fn reply_to_address(
    host: &dyn InboxHost,
    sessions: &KnownSessions,
    reply_to: Option<&str>,
) -> Result<Option<String>, String> {
    let Some(raw) = reply_to.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    match ReplyTo::parse(raw) {
        ReplyTo::Role("") => {
            Err("replyTo \"role:\" names no role; pass role:<agent id>".to_string())
        }
        // `role:human` (any case) is no role queue (a reply to it could never be
        // queued); it means the human, and is stored as the plain address.
        ReplyTo::Role(role) | ReplyTo::Address(role) if role.eq_ignore_ascii_case(HUMAN) => {
            Ok(Some(HUMAN.to_string()))
        }
        ReplyTo::Role(role) => Ok(Some(format!("{REPLY_TO_ROLE_PREFIX}{role}"))),
        ReplyTo::Address(address) => {
            let found = sessions.find(address).or_else(|| {
                host.handle_for_session(address)
                    .and_then(|handle| sessions.find(&handle))
            });
            match found {
                Some(session) if session.running => Ok(Some(session.handle.clone())),
                Some(session) => Err(format!(
                    "replyTo session '{address}' (\"{}\") is not running; name a running session, or role:<agent id> so answers wait in that role's queue",
                    session.name
                )),
                None => Err(format!(
                    "replyTo '{address}' is not a running session; pass a list_sessions handle, \"human\", or role:<agent id>"
                )),
            }
        }
    }
}

/// [`reply_to_address`] for a send outside this module (a role message): the outer
/// `Err` is a failure to list the sessions, the inner one an unusable `replyTo`.
pub(crate) async fn resolve_reply_to(
    host: &dyn InboxHost,
    reply_to: Option<&str>,
) -> Result<Result<Option<String>, String>, String> {
    if reply_to.map(str::trim).is_none_or(str::is_empty) {
        return Ok(Ok(None));
    }
    let sessions = host.sessions().await?;
    Ok(reply_to_address(host, &sessions, reply_to))
}

/// How an address shows: `human`, a session's current tab name (else the name it was
/// launched with), or the address itself.
pub(crate) fn address_name(
    host: &dyn InboxHost,
    sessions: &KnownSessions,
    address: &str,
) -> String {
    if address == HUMAN {
        return HUMAN.to_string();
    }
    sessions
        .find(address)
        .map(|s| s.name.clone())
        .or_else(|| host.display_name(address))
        .unwrap_or_else(|| address.to_string())
}

/// A resolved inbox keeps portable ownership beside its local storage path.
#[derive(Clone)]
pub(crate) struct InboxLocation {
    pub team: LaunchTeam,
    pub path: PathBuf,
}

pub(crate) async fn team_inbox(
    host: &dyn InboxHost,
    team: LaunchTeam,
) -> Result<InboxLocation, String> {
    let path = host.team_store(&team).await?;
    Ok(InboxLocation { team, path })
}

async fn active_store(host: &dyn InboxHost) -> Result<InboxLocation, String> {
    team_inbox(host, host.active_team().await?).await
}

/// The team whose inbox holds the mail of the session under `handle`: the team it was
/// launched in, else the team of the workspace its tab is in. `None` only for a session
/// with neither, whose mail follows the team active in the app.
pub(crate) async fn handle_team(host: &dyn InboxHost, handle: &str) -> Option<LaunchTeam> {
    if let Some(team) = crate::mcp::remote_authority::caller_team(handle) {
        return Some(team);
    }
    match host.launch_team(handle) {
        Some(team) => Some(team),
        None => host.tab_team(handle).await,
    }
}

/// A session's inbox stays in one team's store when the UI switches teams.
pub(crate) async fn session_store(
    host: &dyn InboxHost,
    handle: &str,
) -> Result<InboxLocation, String> {
    match handle_team(host, handle).await {
        Some(team) => team_inbox(host, team).await,
        None => active_store(host).await,
    }
}

/// Run `op` against the store at `path` off the async runtime. The outer `Err` is a
/// storage failure; the inner one an input the store refused.
pub(crate) async fn with_store<T, F>(
    label: &'static str,
    path: PathBuf,
    op: F,
) -> Result<Result<T, String>, String>
where
    T: Send + 'static,
    F: FnOnce(&WeftStore) -> WvWriteResult<T> + Send + 'static,
{
    await_blocking(label, move || {
        // RI-2-ALLOW: inside the await_blocking closure, off the async runtime.
        let store = WeftStore::open(path).map_err(|e| format!("open inbox store: {e}"))?;
        match op(&store) {
            Ok(value) => Ok(Ok(value)),
            Err(WvWriteError::Invalid(reason)) => Ok(Err(reason.0)),
            Err(WvWriteError::Store(e)) => Err(format!("inbox store: {e}")),
        }
    })
    .await
}

/// Store a message to the already-resolved `to` in the store at `path`.
#[allow(clippy::too_many_arguments)]
async fn store_message(
    host: &Arc<dyn InboxHost>,
    location: InboxLocation,
    from: String,
    to: String,
    text: &str,
    reply_to: Option<(i64, InboxReplyTeam)>,
    attach_file: Option<String>,
    on_click: Option<MessageOnClick>,
    content_type: Option<inbox::MessageContentType>,
    replies_go_to: Option<String>,
    summary: Option<String>,
    kind: Option<inbox::MessageKind>,

    request_id: Option<&str>,
    request_hash: Option<String>,
    media: Option<&crate::media::MediaInput>,
) -> Result<SendResult, String> {
    let request_hash = request_hash.or_else(|| {
        media?;
        request_id.map(|_| {
            inbox::request_payload_hash(&serde_json::json!([
                from,
                to,
                text,
                reply_to,
                attach_file,
                on_click,
                content_type,
                replies_go_to,
                summary,
                kind,
                media
            ]))
        })
    });
    if let (Some(id), Some(hash)) = (request_id, request_hash.as_deref()) {
        if let Some(result) = replay_send(&location.path, &from, id, hash, kind, text).await? {
            return Ok(result);
        }
    }
    let prepared = if let Some(input) = media {
        let own = input.url.is_some() && host.own_media_sender(&from).await;
        let scope = host.caller_scope(&from).await;
        let workspace_root = host.caller_media_workspace(&from).await;
        match host
            .prepare_media(
                &location.team,
                input,
                own,
                &scope,
                workspace_root.as_deref(),
            )
            .await
        {
            Ok(prepared) => Some(prepared),
            Err(e) => return Ok(SendResult::invalid(e)),
        }
    } else {
        None
    };
    let attachments = prepared.as_ref().map(|(a, _)| vec![a.clone()]);
    let fetch_location = location.clone();
    let text = text.to_string();
    let recipient = to.clone();
    let request_id = request_id.map(str::to_string);
    let stored = with_store("send_message", location.path, move |store| {
        inbox::inbox_insert_request_outcome(
            store,
            &from,
            &to,
            &text,
            reply_to.as_ref().map(|(id, _)| *id),
            reply_to.as_ref().map(|(_, team)| team),
            attach_file.as_deref(),
            on_click.as_ref(),
            content_type,
            replies_go_to.as_deref(),
            summary.as_deref(),
            kind,
            request_id.as_deref(),
            request_hash.as_deref(),
            attachments.as_deref(),
        )
    })
    .await?;
    let (message_id, inserted) = match stored {
        Ok(outcome) => outcome,
        Err(reason) => return Ok(SendResult::invalid(reason)),
    };
    if inserted {
        if let Some((attachment, true)) = prepared {
            start_media_fetch(host.clone(), fetch_location, message_id, attachment);
        }
        host.inbox_event(InboxEvent::new(
            InboxChange::Stored,
            &location.team,
            [message_id],
        ));
        if recipient != HUMAN {
            host.mail_stored(&recipient);
        }
    }
    Ok(SendResult::sent(message_id))
}

/// CodeSense's own note to the human in a specific team's inbox,
/// from `codesense`. Used by the deadline sweep when the launcher is gone (or
/// the launch was human-launched) so the kill still reaches somebody. When
/// `team` is `None` the active team's inbox is used instead — same degradation
/// as [`note_to_session`] when the launched handle has no team.
pub(crate) async fn note_to_human(
    host: &Arc<dyn InboxHost>,
    team: Option<&(String, String)>,
    text: &str,
    summary: &str,
) -> Result<i64, String> {
    note_to_human_with(host, team, text, summary, NoteExtras::default()).await
}

/// What a note carries beyond its text: the click action its row honours, its kind,
/// and a request id that makes the note idempotent (the same id stores one message).
#[derive(Default)]
pub(crate) struct NoteExtras<'a> {
    pub on_click: Option<MessageOnClick>,
    pub kind: Option<inbox::MessageKind>,
    pub request_id: Option<&'a str>,
}

impl NoteExtras<'_> {
    fn request_hash(&self, from: &str, to: &str, text: &str) -> Option<String> {
        self.request_id.map(|_| {
            inbox::request_payload_hash(&serde_json::json!([from, to, text, self.on_click]))
        })
    }
}

pub(crate) async fn note_to_human_with(
    host: &Arc<dyn InboxHost>,
    team: Option<&(String, String)>,
    text: &str,
    summary: &str,
    extras: NoteExtras<'_>,
) -> Result<i64, String> {
    let location = match team {
        Some((org, team)) => {
            use crate::commands::dashboard_launch::LaunchTeam;
            team_inbox(
                &**host,
                LaunchTeam {
                    org: org.clone(),
                    team: team.clone(),
                },
            )
            .await?
        }
        None => active_store(&**host).await?,
    };
    let request_hash = extras.request_hash("codesense", HUMAN, text);
    match store_message(
        host,
        location,
        "codesense".to_string(),
        HUMAN.to_string(),
        text,
        None,
        None,
        extras.on_click,
        Some(inbox::MessageContentType::Markdown),
        None,
        Some(summary.to_string()),
        Some(extras.kind.unwrap_or(inbox::MessageKind::Update)),
        extras.request_id,
        request_hash,
        None,
    )
    .await?
    {
        SendResult::Sent { message_id, .. } => Ok(message_id),
        SendResult::Invalid { message } => Err(message),
    }
}

/// SW-AGR-19 PR-8: CodeSense's own note to a session (an asynchronous
/// operation it asked for has ended), in the session's team inbox, from
/// `codesense`. The session's mail notice offers it as any other mail.
pub(crate) async fn note_to_session(
    host: &Arc<dyn InboxHost>,
    session_id: &str,
    text: &str,
    summary: &str,
) -> Result<i64, String> {
    note_to_session_with(host, session_id, text, summary, NoteExtras::default()).await
}

pub(crate) async fn note_to_session_with(
    host: &Arc<dyn InboxHost>,
    session_id: &str,
    text: &str,
    summary: &str,
    extras: NoteExtras<'_>,
) -> Result<i64, String> {
    let handle = session_handle(&**host, session_id, None);
    let location = session_store(&**host, &handle).await?;
    let request_hash = extras.request_hash("codesense", &handle, text);
    match store_message(
        host,
        location,
        "codesense".to_string(),
        handle,
        text,
        None,
        None,
        extras.on_click,
        None,
        None,
        Some(summary.to_string()),
        extras.kind,
        extras.request_id,
        request_hash,
        None,
    )
    .await?
    {
        SendResult::Sent { message_id, .. } => Ok(message_id),
        SendResult::Invalid { message } => Err(message),
    }
}

/// `send_message`: the caller (by its self-reported session id) messages `to`. Mail to
/// a session goes to its team; mail to the human goes to the sender's team. An optional
/// `attach_file` records a managed-file reference for the human preview — stored
/// verbatim as a logical path; `None` is a standalone message. `on_click` is the
/// per-message action the human-inbox UI honours on row click. `reply_to` (a running
/// session, `human` or `role:<id>`) is where replies to it go instead of the sender.
/// `summary` is the sender's one-line list preview. (Tests only: the MCP tool sends
/// through [`send_message_with_kind`].)
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn send_message<'a>(
    host: &Arc<dyn InboxHost>,
    session_id: impl Into<Caller<'a>>,
    to: &str,
    text: &str,
    attach_file: Option<&str>,
    on_click: Option<MessageOnClick>,
    content_type: Option<inbox::MessageContentType>,
    reply_to: Option<&str>,
    summary: Option<&str>,
) -> Result<SendResult, String> {
    send_message_with_kind(
        host,
        session_id,
        to,
        text,
        attach_file,
        on_click,
        content_type,
        reply_to,
        summary,
        None,
    )
    .await
}

/// `send_message` with the sender's `kind` (SW-AGR-18): what the message asks of the
/// human, for agent views and dashboards. `None` stores no kind.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn send_message_with_kind<'a>(
    host: &Arc<dyn InboxHost>,
    session_id: impl Into<Caller<'a>>,
    to: &str,
    text: &str,
    attach_file: Option<&str>,
    on_click: Option<MessageOnClick>,
    content_type: Option<inbox::MessageContentType>,
    reply_to: Option<&str>,
    summary: Option<&str>,
    kind: Option<inbox::MessageKind>,
) -> Result<SendResult, String> {
    send_message_request(
        host,
        session_id,
        to,
        text,
        attach_file,
        on_click,
        content_type,
        reply_to,
        summary,
        kind,
        None,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn send_message_request<'a>(
    host: &Arc<dyn InboxHost>,
    session_id: impl Into<Caller<'a>>,
    to: &str,
    text: &str,
    attach_file: Option<&str>,
    on_click: Option<MessageOnClick>,
    content_type: Option<inbox::MessageContentType>,
    reply_to: Option<&str>,
    summary: Option<&str>,
    kind: Option<inbox::MessageKind>,
    request_id: Option<&str>,
    media: Option<&crate::media::MediaInput>,
) -> Result<SendResult, String> {
    send_message_replying(
        host,
        session_id.into(),
        to,
        text,
        attach_file,
        on_click,
        content_type,
        None,
        reply_to,
        summary,
        kind,
        request_id,
        media,
    )
    .await
}

/// [`send_message`], recording the message it answers (`in_reply_to`: its id and the
/// team whose inbox holds it). `reply_to` is the new message's own Reply-To address.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn send_message_replying(
    host: &Arc<dyn InboxHost>,
    session_id: Caller<'_>,
    to: &str,
    text: &str,
    attach_file: Option<&str>,
    on_click: Option<MessageOnClick>,
    content_type: Option<inbox::MessageContentType>,
    in_reply_to: Option<(i64, InboxReplyTeam)>,
    reply_to: Option<&str>,
    summary: Option<&str>,
    kind: Option<inbox::MessageKind>,

    request_id: Option<&str>,
    media: Option<&crate::media::MediaInput>,
) -> Result<SendResult, String> {
    if text.trim().is_empty() && media.is_none() {
        return Ok(SendResult::invalid("the text is blank"));
    }
    let request_hash = request_id.filter(|id| !id.trim().is_empty()).map(|_| {
        let mut payload = serde_json::json!([
            to,
            text,
            attach_file,
            on_click,
            content_type,
            in_reply_to,
            reply_to,
            summary,
            kind
        ]);
        if let Some(media) = media {
            payload
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!(media));
        }
        inbox::request_payload_hash(&payload)
    });
    let sessions = host.sessions().await?;
    let Some(to) = recipient_address(&**host, &sessions, to) else {
        return Ok(SendResult::invalid(format_args!(
            "no session '{}'; pass a handle from list_sessions, or \"human\"",
            to.trim()
        )));
    };
    let sender = match caller_handle(&sessions, session_id) {
        Ok(sender) => sender,
        Err(message) => return Ok(SendResult::invalid(message)),
    };
    let path = match (to.as_str(), sender.as_deref()) {
        (HUMAN, Some(sender)) => session_store(&**host, sender).await?,
        (HUMAN, None) => active_store(&**host).await?,
        (session, _) => session_store(&**host, session).await?,
    };
    let from = sender.clone().unwrap_or_else(|| HUMAN.to_string());
    if let (Some(id), Some(hash)) = (request_id, request_hash.as_deref()) {
        if let Some(result) = replay_send(&path.path, &from, id, hash, kind, text).await? {
            return Ok(result);
        }
    }
    let replies_go_to = match reply_to_address(&**host, &sessions, reply_to) {
        Ok(address) => address,
        Err(message) => return Ok(SendResult::invalid(message)),
    };
    let to_human = to == HUMAN && sender.is_some();
    // A Reply-To naming the sender's own role (a manager's digest, `role:manager`) asks
    // nothing of anyone else: stored as an update, so it stays one after the role is
    // released and the store no longer says who held it.
    let stored_kind = match (kind, sender.as_deref(), replies_go_to.as_deref()) {
        (None, Some(sender), Some(reply)) if to_human => match ReplyTo::parse(reply) {
            ReplyTo::Role(role) => {
                let role = crate::weft::role_inbox::role_name(role).to_string();
                let ids: Vec<String> = std::iter::once(sender.to_string())
                    .chain(sessions.find(sender).and_then(|s| s.session.clone()))
                    .collect();
                let holds = with_store("send_message", path.path.clone(), move |store| {
                    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
                    Ok(inbox::role_holders(store)?.any_holds(&role, &ids))
                })
                .await?
                .unwrap_or(false);
                holds.then_some(inbox::MessageKind::Update)
            }
            ReplyTo::Address(_) => None,
        },
        _ => kind,
    };
    let from = sender.unwrap_or_else(|| HUMAN.to_string());
    let effective =
        inbox::MessageKind::effective(stored_kind, replies_go_to.as_deref(), None, &from);
    let result = store_message(
        host,
        path.clone(),
        from.clone(),
        to,
        text,
        in_reply_to,
        attach_file.map(|s| s.to_string()),
        on_click,
        content_type,
        replies_go_to,
        summary.map(str::to_string),
        stored_kind,
        request_id,
        request_hash.clone(),
        media,
    )
    .await?;
    if matches!(result, SendResult::Sent { .. }) {
        if let (Some(id), Some(hash)) = (request_id, request_hash.as_deref()) {
            if let Some(replayed) = replay_send(&path.path, &from, id, hash, kind, text).await? {
                return Ok(replayed);
            }
        }
    }
    Ok(if to_human {
        result.for_human(kind, effective, text)
    } else {
        result
    })
}

/// Return the original stored lane and hint before any live-session or role checks.
async fn replay_send(
    path: &std::path::Path,
    from: &str,
    id: &str,
    hash: &str,
    requested_kind: Option<inbox::MessageKind>,
    text: &str,
) -> Result<Option<SendResult>, String> {
    let (from, id, hash) = (from.to_string(), id.to_string(), hash.to_string());
    let replay = with_store("send_message", path.to_path_buf(), move |store| {
        inbox::inbox_replay_request(store, &from, &id, &hash)
    })
    .await?;
    Ok(match replay {
        Err(error) => Some(SendResult::invalid(error)),
        Ok(None) => None,
        Ok(Some(message)) => {
            let result = SendResult::sent(message.id);
            Some(if message.to == HUMAN && message.from != HUMAN {
                result.for_human(requested_kind, message.effective_kind(), text)
            } else {
                result
            })
        }
    })
}

/// Resolve one stored message directly, including messages outside recent list windows.
/// Both the approval preview and the actual reply use this lookup and selection rule.
async fn stored_reply_recipient(path: PathBuf, message_id: i64) -> Result<Option<String>, String> {
    let original = with_store("reply", path, move |store| {
        inbox::inbox_get(store, message_id)
    })
    .await??;
    Ok(original.map(|message| message.reply_to.unwrap_or(message.from)))
}

/// Answer message `message_id` of the inbox at `own`. The reply goes to the message's
/// `reply_to` when it names one, else to its sender: a session's own team, the human in
/// the same team, or (for `role:<id>`) a pending delivery in that role's queue in the
/// same team. When `marks_read`, the answered message is marked read for `from`.
#[allow(clippy::too_many_arguments)]
async fn reply_as(
    host: &Arc<dyn InboxHost>,
    from: String,
    own: InboxLocation,
    marks_read: bool,
    message_id: i64,
    text: &str,
    content_type: Option<inbox::MessageContentType>,
    summary: Option<&str>,
    kind: Option<inbox::MessageKind>,

    request_id: Option<&str>,
    media: Option<&crate::media::MediaInput>,
) -> Result<SendResult, String> {
    if text.trim().is_empty() && media.is_none() {
        return Ok(SendResult::invalid("the text is blank"));
    }
    let Some(target) = stored_reply_recipient(own.path.clone(), message_id).await? else {
        return Ok(SendResult::invalid(format_args!("no message {message_id}")));
    };
    let origin = InboxReplyTeam {
        org: own.team.org.clone(),
        team: own.team.team.clone(),
    };
    let to_human = from != HUMAN
        && match ReplyTo::parse(&target) {
            ReplyTo::Role(role) => role.eq_ignore_ascii_case(HUMAN),
            ReplyTo::Address(address) => address == HUMAN,
        };
    let result = match ReplyTo::parse(&target) {
        // A `role:human` stored before replyTo was normalized still means the human.
        ReplyTo::Role(role) if role.eq_ignore_ascii_case(HUMAN) => {
            store_message(
                host,
                own.clone(),
                from.clone(),
                HUMAN.to_string(),
                text,
                Some((message_id, origin)),
                None,
                None,
                content_type,
                None,
                summary.map(str::to_string),
                kind,
                request_id,
                None,
                media,
            )
            .await?
        }
        ReplyTo::Role(role) => {
            queue_role_reply(
                host,
                &own,
                &from,
                role,
                text,
                message_id,
                origin,
                content_type,
                summary,
                request_id,
                media,
            )
            .await?
        }
        ReplyTo::Address(to) => {
            let to_path = if to == HUMAN {
                own.clone()
            } else {
                session_store(&**host, to).await?
            };
            store_message(
                host,
                to_path,
                from.clone(),
                to.to_string(),
                text,
                Some((message_id, origin)),
                // Replies never attach a managed file (SW-BSI-2 Phase 4): the initial
                // message carries the primary file; subsequent turns reference it in
                // text, not by rebinding a new reference.
                None,
                // Replies don't carry a click action — the originating message's
                // onClick governs the thread row.
                None,
                content_type,
                None,
                summary.map(str::to_string),
                kind,
                request_id,
                None,
                media,
            )
            .await?
        }
    };
    // A reply carries no Reply-To: its kind is the replier's, else an update.
    let result = if to_human {
        result.for_human(kind, kind.unwrap_or(inbox::MessageKind::Update), text)
    } else {
        result
    };
    // Only once the reply is stored, so a failed reply leaves the message unread. The
    // reply stands either way: a failed mark is logged, not reported, so a retry does
    // not store the reply twice.
    if marks_read && matches!(result, SendResult::Sent { .. }) {
        let marked = with_store("reply", own.path, move |store| {
            inbox::inbox_mark_read(store, message_id, &from)
        })
        .await;
        match marked {
            Ok(Ok(true)) => {
                host.inbox_event(InboxEvent::new(InboxChange::Read, &own.team, [message_id]))
            }
            Ok(Ok(false)) => {}
            Ok(Err(e)) | Err(e) => {
                log::warn!(target: "inbox", "answered message {message_id} not marked read: {e}")
            }
        }
    }
    Ok(result)
}

/// Queue a reply to message `message_id` for `role` in the store at `path`: an inbox
/// row to the role plus its pending delivery, committed together, so whichever
/// instance of the role claims it next gets it, however long that takes.
#[allow(clippy::too_many_arguments)]
async fn queue_role_reply(
    host: &Arc<dyn InboxHost>,
    own: &InboxLocation,
    from: &str,
    role: &str,
    text: &str,
    message_id: i64,
    origin: InboxReplyTeam,
    content_type: Option<inbox::MessageContentType>,
    summary: Option<&str>,

    request_id: Option<&str>,
    media: Option<&crate::media::MediaInput>,
) -> Result<SendResult, String> {
    let queued_role = role.to_string();
    let queued_sender = from.to_string();
    let (from, role, text) = (from.to_string(), role.to_string(), text.to_string());
    let summary = summary.map(str::to_string);
    let now = chrono::Utc::now().timestamp();
    let request_id = request_id
        .filter(|id| !id.trim().is_empty())
        .map(|id| format!("reply:{from}:{id}"));
    let mut payload =
        serde_json::json!([from, role, text, message_id, origin, content_type, summary]);
    if let Some(media) = media {
        payload
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!(media));
    }
    let payload_hash = inbox::request_payload_hash(&payload);
    if let Some(key) = &request_id {
        let (key, hash) = (key.clone(), payload_hash.clone());
        let replay = with_store("reply.replay", own.path.clone(), move |store| {
            // RI-2-ALLOW: with_store executes this closure on the blocking pool.
            let conn = store.conn()?;
            crate::weft::role_inbox::replay_message_request(&conn, &key, &hash).map_err(|e| match e
            {
                crate::weft::role_inbox::RoleInboxError::Store(error) => error,
                other => crate::weft::sqlite::wv_refusal(other.to_string()),
            })
        })
        .await?;
        match replay {
            Ok(Some((message_id, _))) => return Ok(SendResult::sent(message_id)),
            Err(reason) => return Ok(SendResult::invalid(reason)),
            Ok(None) => {}
        }
    }
    let prepared = if let Some(input) = media {
        let sender_owns_machine = input.url.is_some() && host.own_media_sender(&from).await;
        let scope = host.caller_scope(&from).await;
        let workspace_root = host.caller_media_workspace(&from).await;
        Some(
            host.prepare_media(
                &own.team,
                input,
                sender_owns_machine,
                &scope,
                workspace_root.as_deref(),
            )
            .await?,
        )
    } else {
        None
    };
    let attachments = prepared.as_ref().map(|(a, _)| vec![a.clone()]);
    let stored = with_store("reply", own.path.clone(), move |store| {
        crate::weft::role_inbox::send_role_message_commit_outcome(
            store,
            &from,
            &role,
            &text,
            Some(message_id),
            Some(&origin),
            request_id.as_deref(),
            &payload_hash,
            now,
            None,
            None,
            content_type,
            None,
            summary.as_deref(),
            attachments.as_deref(),
        )
        .map(|(message_id, _delivery_id, inserted)| (message_id, inserted))
        .map_err(|e| match e {
            crate::weft::role_inbox::RoleInboxError::Store(err) => err,
            other => crate::weft::sqlite::wv_refusal(other.to_string()),
        })
    })
    .await?;
    match stored {
        Ok((message_id, inserted)) => {
            if inserted {
                if let Some((attachment, true)) = prepared {
                    start_media_fetch(host.clone(), own.clone(), message_id, attachment);
                }
                host.inbox_event(InboxEvent::new(
                    InboxChange::Stored,
                    &own.team,
                    [message_id],
                ));
                host.roles_changed();
                host.role_message_queued(&own.team, &queued_role, message_id, &queued_sender);
            }
            Ok(SendResult::sent(message_id))
        }
        Err(reason) => Ok(SendResult::invalid(reason)),
    }
}

/// `reply`: the caller answers message `message_id`. A caller with no handle answers as
/// the human from the UI-active team, but marks nothing read: only the app reads the
/// human's mail.
#[cfg(test)]
pub(crate) async fn reply<'a>(
    host: &Arc<dyn InboxHost>,
    session_id: impl Into<Caller<'a>>,
    message_id: i64,
    text: &str,
    summary: Option<&str>,
) -> Result<SendResult, String> {
    reply_with_kind(host, session_id, message_id, text, summary, None).await
}

/// `reply` with the replier's `kind` (SW-AGR-18), stored on the reply when it goes to
/// a session or the human. A reply queued for a role keeps none: role queues have no lanes.
pub(crate) async fn reply_with_kind<'a>(
    host: &Arc<dyn InboxHost>,
    session_id: impl Into<Caller<'a>>,
    message_id: i64,
    text: &str,
    summary: Option<&str>,
    kind: Option<inbox::MessageKind>,
) -> Result<SendResult, String> {
    reply_with_kind_and_content(host, session_id, message_id, text, summary, kind, None).await
}

/// The same reply path with an explicit body format (hosted inboxes force plain).
pub(crate) async fn reply_with_kind_and_content<'a>(
    host: &Arc<dyn InboxHost>,
    session_id: impl Into<Caller<'a>>,
    message_id: i64,
    text: &str,
    summary: Option<&str>,
    kind: Option<inbox::MessageKind>,
    content_type: Option<inbox::MessageContentType>,
) -> Result<SendResult, String> {
    reply_request(
        host,
        session_id,
        message_id,
        text,
        summary,
        kind,
        content_type,
        None,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn reply_request<'a>(
    host: &Arc<dyn InboxHost>,
    session_id: impl Into<Caller<'a>>,
    message_id: i64,
    text: &str,
    summary: Option<&str>,
    kind: Option<inbox::MessageKind>,
    content_type: Option<inbox::MessageContentType>,
    request_id: Option<&str>,
    media: Option<&crate::media::MediaInput>,
) -> Result<SendResult, String> {
    let session_id = session_id.into();
    let sessions = host.sessions().await?;
    let caller = match caller_handle(&sessions, session_id) {
        Ok(caller) => caller,
        Err(message) => return Ok(SendResult::invalid(message)),
    };
    match caller {
        Some(handle) => {
            let own = session_store(&**host, &handle).await?;
            reply_as(
                host,
                handle,
                own,
                true,
                message_id,
                text,
                content_type,
                summary,
                kind,
                request_id,
                media,
            )
            .await
        }
        None => {
            let own = active_store(&**host).await?;
            reply_as(
                host,
                HUMAN.to_string(),
                own,
                false,
                message_id,
                text,
                content_type,
                summary,
                kind,
                request_id,
                media,
            )
            .await
        }
    }
}

/// The human answers message `message_id` of `team`'s inbox from the Agents panel: the
/// reply goes only where an agent's `reply` would (the message's `reply_to`, else its
/// sender) and the original is marked read for the human. Nobody else (no foreman or
/// controller) is copied. The reply's provenance is the displayed conversation's owning
/// team, never a later UI-active team.
pub(crate) async fn reply_from_human(
    host: &Arc<dyn InboxHost>,
    team: &LaunchTeam,
    message_id: i64,
    text: &str,
    content_type: Option<inbox::MessageContentType>,
    summary: Option<&str>,
) -> Result<SendResult, String> {
    let own = team_inbox(&**host, team.clone()).await?;
    reply_as(
        host,
        HUMAN.to_string(),
        own,
        true,
        message_id,
        text,
        content_type,
        summary,
        None,
        None,
        None,
    )
    .await
}

/// Record that `text` was typed into the terminal of the session under `target` (a
/// `send_to_session` that went through), as a read row with `via = terminal` in that
/// session's inbox, so its conversation shows it. `caller` is the sending session (its
/// id or handle); `None` is the human. A caller that no longer resolves is recorded as
/// the id it gave, never as the human. Returns the row id.
pub(crate) async fn log_terminal_send(
    host: &dyn InboxHost,
    caller: Option<&str>,
    target: &str,
    text: &str,
) -> Result<Option<i64>, String> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    let sessions = host.sessions().await?;
    // Resolved as a named caller, not an implied one: a tab the session list does not
    // know (an unregistered local tab, a remote authenticated handle) is logged under
    // the id it gave rather than as the human, since this records who typed the text.
    let from = match caller_handle(&sessions, caller) {
        Ok(Some(handle)) => handle,
        Ok(None) => HUMAN.to_string(),
        Err(_) => caller.map(str::trim).unwrap_or(HUMAN).to_string(),
    };
    let to = sessions
        .find(target)
        .map(|s| s.handle.clone())
        .or_else(|| host.handle_for_session(target))
        .unwrap_or_else(|| target.trim().to_string());
    let location = session_store(host, &to).await?;
    let text = text.to_string();
    let stored = with_store("send_to_session", location.path, move |store| {
        inbox::inbox_insert_terminal(store, &from, &to, &text)
    })
    .await?;
    match stored {
        Ok(id) => {
            host.inbox_event(InboxEvent::new(InboxChange::Stored, &location.team, [id]));
            Ok(Some(id))
        }
        Err(reason) => {
            log::warn!(target: "inbox", "terminal send not logged: {reason}");
            Ok(None)
        }
    }
}

/// The outcome of a react call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub(crate) enum ReactResult {
    Reacted {
        #[serde(rename = "messageId")]
        message_id: i64,
        emoji: String,
        outcome: crate::weft::reactions::ReactOutcome,
    },
    /// An input that cannot be stored; `message` says which, in one sentence.
    Invalid { message: String },
}

/// React as `reactor` to message `message_id` in the store at `own`. A caller that is a
/// session (`party`) reacts only to mail it sent, received or is the reply target of, or
/// to a role queue; the human's pane reacts to anything it shows.
async fn react_in(
    host: &dyn InboxHost,
    own: InboxLocation,
    reactor: String,
    party: bool,
    message_id: i64,
    emoji: &str,
    remove: Option<bool>,
) -> Result<ReactResult, String> {
    let InboxLocation { team, path } = own;
    let emoji = emoji.to_string();
    let stored = with_store("react", path, move |store| {
        if party {
            let held = inbox::inbox_get(store, message_id)?.is_some_and(|m| {
                m.role.is_some()
                    || m.from == reactor
                    || m.to == reactor
                    || m.reply_to.as_deref() == Some(reactor.as_str())
            });
            if !held {
                return Ok(Err(format!("no message {message_id}")));
            }
        }
        match reactions::reaction_react(store, message_id, &reactor, &emoji, remove) {
            Ok(outcome) => Ok(Ok((outcome, emoji))),
            Err(WvWriteError::Invalid(refusal)) => Ok(Err(refusal.0)),
            Err(other) => Err(other),
        }
    })
    .await?;
    Ok(match stored {
        Ok(Ok((outcome, emoji))) => {
            if outcome != reactions::ReactOutcome::Unchanged {
                host.inbox_event(InboxEvent::new(InboxChange::Reacted, &team, [message_id]));
            }
            ReactResult::Reacted {
                message_id,
                emoji,
                outcome,
            }
        }
        Ok(Err(message)) | Err(message) => ReactResult::Invalid { message },
    })
}

/// An agent reacts to message `message_id` with `emoji` (see
/// [`crate::weft::reactions::reaction_react`]), in the store of the team it was launched
/// in, as `reply` resolves it: never the team the UI has selected.
pub(crate) async fn react_request<'a>(
    host: &dyn InboxHost,
    session_id: impl Into<Caller<'a>>,
    message_id: i64,
    emoji: &str,
    remove: Option<bool>,
) -> Result<ReactResult, String> {
    let sessions = host.sessions().await?;
    let handle = match caller_handle(&sessions, session_id) {
        Ok(Some(handle)) => handle,
        Ok(None) => {
            return Ok(ReactResult::invalid(
                "an agent reaction needs a running session",
            ))
        }
        Err(message) => return Ok(ReactResult::Invalid { message }),
    };
    let own = session_store(host, &handle).await?;
    react_in(host, own, handle, true, message_id, emoji, remove).await
}

/// The human reacts to message `message_id` of `team`'s inbox.
pub(crate) async fn react_from_human(
    host: &dyn InboxHost,
    team: &LaunchTeam,
    message_id: i64,
    emoji: &str,
    remove: Option<bool>,
) -> Result<ReactResult, String> {
    let own = team_inbox(host, team.clone()).await?;
    react_in(
        host,
        own,
        HUMAN.to_string(),
        false,
        message_id,
        emoji,
        remove,
    )
    .await
}

/// New reactions on messages the caller sent, each reported once. Not mail: nothing here
/// is unread, announced or woken for. Mail to an agent in another team lives in that team's
/// store, so every store the caller can see is drained, each notice naming its team.
pub(crate) async fn take_reaction_notices<'a>(
    host: &dyn InboxHost,
    session_id: impl Into<Caller<'a>>,
) -> Result<Vec<reactions::ReactionNotice>, String> {
    let sessions = host.sessions().await?;
    let Ok(Some(me)) = caller_handle(&sessions, session_id) else {
        return Ok(Vec::new());
    };
    let own = session_store(host, &me).await?;
    let stores = super::agent_read_model::team_stores(host, Some(own.team.clone()), false)
        .await
        .unwrap_or_else(|_| vec![(own.team.clone(), own.path.clone())]);
    let mut found = Vec::new();
    for (team, path) in stores {
        let sender = me.clone();
        let taken = with_store("reaction_notices", path, move |store| {
            reactions::reactions_take_notices(store, &[sender.as_str()])
        })
        .await
        .and_then(|inner| inner);
        match taken {
            Ok(notices) => found.extend(notices.into_iter().map(|mut n| {
                n.team = Some(InboxReplyTeam {
                    org: team.org.clone(),
                    team: team.team.clone(),
                });
                n
            })),
            Err(error) => {
                log::warn!(target: "inbox", "reaction notices of team {}:{} failed: {error}", team.org, team.team)
            }
        }
    }
    Ok(found)
}

/// `read_messages`: the caller's unread messages, oldest first, each returned once. A
/// caller with no handle has no inbox and reads nothing (the human reads in the app).
pub(crate) async fn read_messages<'a>(
    host: &dyn InboxHost,
    session_id: impl Into<Caller<'a>>,
) -> Result<Vec<ReadMessage>, String> {
    let session_id = session_id.into();
    let sessions = host.sessions().await?;
    let me = match caller_handle(&sessions, session_id) {
        Ok(Some(me)) => me,
        // The human, or a session that is not running: nothing to read.
        Ok(None) | Err(_) => return Ok(Vec::new()),
    };
    let InboxLocation { team, path } = session_store(host, &me).await?;
    let taken = with_store("read_messages", path, move |store| {
        inbox::inbox_take_unread(store, &me)
    })
    .await??;
    if !taken.is_empty() {
        host.inbox_event(InboxEvent::new(
            InboxChange::Read,
            &team,
            taken.iter().map(|m| m.id),
        ));
    }
    // SW-BSI-3: resolve the attachment hint once per message. Serialized because
    // the host's resolver is async; the lookup itself is a cheap index read, so
    // sequential-await adds no meaningful latency. An attachment with no linked
    // owner stays `None` without any error.
    let mut results: Vec<ReadMessage> = Vec::with_capacity(taken.len());
    for m in taken {
        let attachment_hint = if let Some(logical_path) = m.primary_file.as_ref() {
            host.owning_story_for_path(&team, logical_path).await.map(|story_id| AttachmentHint {
                    note: format!(
                        "The attached file is in story {story_id}'s subtree. If reading it fails on your machine, call hydrate_story with this storyId and your workspace as target to copy it in."
                    ),
                    owning_story_id: story_id,
                    suggested_call: "hydrate_story",
                })
        } else {
            None
        };
        results.push(ReadMessage {
            message_id: m.id,
            from_name: address_name(host, &sessions, &m.from),
            from: m.from,
            text: m.text,
            in_reply_to: m.in_reply_to,
            in_reply_to_team: m.in_reply_to_team,
            reply_to: m.reply_to,
            summary: m.summary,
            primary_file: m.primary_file,
            attachments: {
                let mut a = m.attachments;
                host.present_media(&team, &mut a);
                a
            },
            attachment_hint,
        });
    }
    Ok(results)
}

/// When a session's agent was last told about its mail (see [`SessionUnread::announced`]).
pub(crate) use crate::weft::inbox::Announced;

/// A session's unread mail, as `list_sessions` reports it and the `inbox_idle` routine
/// trigger reads it: the mail addressed to its own handle in its team's inbox, exactly what
/// `read_messages` would hand it. Mail to an agent definition's role inbox is not counted:
/// that is a queue shared by every instance, claimed with `claim_role_messages`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionUnread {
    pub count: u32,
    /// When the oldest unread message was stored, epoch ms.
    pub oldest_at_ms: Option<i64>,
    /// The newest unread message's id, comparable only within `team`'s inbox.
    pub newest_id: Option<i64>,
    /// The team whose inbox holds the session's mail.
    pub team: LaunchTeam,
    /// When its agent was last told about its mail, in `team`'s inbox.
    pub announced: Option<Announced>,
}

/// Each handle's cached unread mail (absent with none), and the
/// handles whose inbox could not be resolved or read.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct UnreadByHandle {
    pub found: HashMap<String, SessionUnread>,
    pub failed: std::collections::HashSet<String>,
}

/// The unread mail of each of `handles`. One read per team inbox; nothing is marked read.
/// A handle whose inbox cannot be resolved (a team not synced here) or read is in
/// `failed`, so a listing never fails on one team's store.
#[cfg(test)]
pub(crate) async fn unread_by_handle(host: &dyn InboxHost, handles: &[String]) -> UnreadByHandle {
    let mut teams = HashMap::new();
    for handle in handles {
        teams.insert(handle.clone(), handle_team(host, handle).await);
    }
    unread_by_handle_with_teams(host, handles, &teams).await
}

#[cfg(test)]
pub(crate) async fn unread_by_handle_with_teams(
    host: &dyn InboxHost,
    handles: &[String],
    teams: &HashMap<String, Option<LaunchTeam>>,
) -> UnreadByHandle {
    let mut out = UnreadByHandle::default();
    let mut by_store: HashMap<PathBuf, (LaunchTeam, Vec<String>)> = HashMap::new();
    // Each team's store is resolved once, however many of its sessions are listed.
    let mut resolved: HashMap<Option<(String, String)>, Option<InboxLocation>> = HashMap::new();
    for handle in handles {
        let team = teams.get(handle).cloned().flatten();
        let slot = team.as_ref().map(|t| (t.org.clone(), t.team.clone()));
        let location = match resolved.get(&slot) {
            Some(location) => location.clone(),
            None => {
                let resolved_store = match team {
                    Some(team) => team_inbox(host, team).await,
                    None => active_store(host).await,
                };
                // Debug only: a team not synced here fails on every listing.
                let location = resolved_store
                    .map_err(|e| log::debug!(target: "inbox", "no inbox for {handle}: {e}"))
                    .ok();
                resolved.insert(slot, location.clone());
                location
            }
        };
        let Some(InboxLocation { team, path }) = location else {
            out.failed.insert(handle.clone());
            continue;
        };
        by_store
            .entry(path)
            .or_insert_with(|| (team, Vec::new()))
            .1
            .push(handle.clone());
    }
    for (path, (team, recipients)) in by_store {
        let asked = recipients.clone();
        let read = with_store("inbox_unread_for", path, move |store| {
            inbox::inbox_unread_for(store, &recipients)
        })
        .await;
        match read {
            Ok(Ok(found)) => {
                for (handle, summary) in found {
                    out.found.insert(
                        handle,
                        SessionUnread {
                            count: summary.count,
                            oldest_at_ms: summary.oldest_at_ms,
                            newest_id: summary.newest_id,
                            team: team.clone(),
                            announced: summary.announced,
                        },
                    );
                }
            }
            Ok(Err(e)) | Err(e) => {
                log::warn!(target: "inbox", "unread counts for team {}:{} failed: {e}", team.org, team.team);
                out.failed.extend(asked);
            }
        }
    }
    out
}

/// The human's Messages list for `team`: the newest rows, newest first, and the human's
/// unread count. A view: it marks nothing read. `scope` filters by recipient class
/// (`to-human` default, `to-agents` for the observability lens, `all` for both); with
/// `recipient_agent_id` (an agent definition id) only that agent's conversation: its role
/// inbox, shared by every running instance, the inboxes of `sessions` (its instances'
/// handles), and the replies under them.
pub(crate) async fn list_messages(
    host: &dyn InboxHost,
    team: &LaunchTeam,
    scope: InboxScope,
    recipient_agent_id: Option<&str>,
    sessions: &[String],
) -> Result<InboxView, String> {
    let mut view = list_messages_unbounded(host, team, scope, recipient_agent_id, sessions).await?;
    view.matched = view.messages.len();
    view.has_more = view.messages.len() > 500;
    view.messages.truncate(500);
    Ok(view)
}

pub(crate) async fn list_messages_unbounded(
    host: &dyn InboxHost,
    team: &LaunchTeam,
    scope: InboxScope,
    recipient_agent_id: Option<&str>,
    sessions: &[String],
) -> Result<InboxView, String> {
    let mut view = match host.projected_inbox(team) {
        Some(view) => view?,
        None => {
            let batch =
                crate::live_hub::inbox::read_batch(host.team_store(team).await?, true, vec![])
                    .await?;
            let mut projection = crate::live_hub::inbox::InboxProjection::default();
            projection.apply_batch(host, &host.sessions().await?, team, batch, &[]);
            projection.view_parts(team).build()
        }
    };
    for row in &mut view.messages {
        host.present_media(team, &mut row.attachments);
    }
    if let Some(agent) = recipient_agent_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        let addresses: std::collections::HashSet<_> = std::iter::once(agent)
            .chain(sessions.iter().map(|s| s.trim()).filter(|s| !s.is_empty()))
            .collect();
        let mut ids: std::collections::HashSet<_> = view
            .messages
            .iter()
            .filter(|m| addresses.contains(m.from.as_str()) || addresses.contains(m.to.as_str()))
            .map(|m| m.message_id)
            .collect();
        loop {
            let old = ids.len();
            for m in &view.messages {
                if m.in_reply_to.is_some_and(|id| ids.contains(&id)) {
                    ids.insert(m.message_id);
                }
            }
            if ids.len() == old {
                break;
            }
        }
        view.messages.retain(|m| ids.contains(&m.message_id));
    }
    view.messages.retain(|m| scope.admits(&m.to));

    Ok(view)
}

/// The human read message `message_id` of `team`'s inbox in the app. Only a message
/// addressed to the human changes.
pub(crate) async fn mark_read_by_human(
    host: &dyn InboxHost,
    team: &LaunchTeam,
    message_id: i64,
) -> Result<(), String> {
    let path = host.team_store(team).await?;
    let changed = with_store("mark_inbox_message_read", path, move |store| {
        inbox::inbox_mark_read(store, message_id, HUMAN)
    })
    .await??;
    if changed {
        host.inbox_event(InboxEvent::new(InboxChange::Read, team, [message_id]));
    }
    Ok(())
}

/// Whether every id of `message_ids` is a message to the human in `team`'s inbox store. A
/// published dashboard's mark-read refuses a whole call when one is not, so it cannot
/// tell another conversation's ids from missing ones (SV-INP-46).
pub(crate) async fn all_to_human(
    host: &dyn InboxHost,
    team: &LaunchTeam,
    message_ids: &[i64],
) -> Result<bool, String> {
    let path = host.team_store(team).await?;
    let ids = message_ids.to_vec();
    Ok(with_store("dashboard_mark_read_check", path, move |store| {
        for id in &ids {
            if !inbox::inbox_get(store, *id)?.is_some_and(|m| m.to == HUMAN) {
                return Ok(false);
            }
        }
        Ok(true)
    })
    .await??)
}

/// SW-AGR-18: the human read every message of `message_ids` in `team`'s inbox at once
/// (Updates' "Mark all read"). One store transaction, and one inbox-changed signal however
/// many changed, so every inbox view re-reads once. Returns how many changed.
pub(crate) async fn mark_read_many_by_human(
    host: &dyn InboxHost,
    team: &LaunchTeam,
    message_ids: Vec<i64>,
) -> Result<u32, String> {
    if message_ids.is_empty() {
        return Ok(0);
    }
    let path = host.team_store(team).await?;
    let changed_ids = message_ids.clone();
    let changed = with_store("mark_inbox_messages_read", path, move |store| {
        inbox::inbox_mark_read_many(store, &message_ids, HUMAN)
    })
    .await??;
    if changed > 0 {
        host.inbox_event(InboxEvent::new(InboxChange::Read, team, changed_ids));
    }
    Ok(changed)
}

/// Mark human-directed messages unread with one read-state projection invalidation.
pub(crate) async fn mark_unread_many_by_human(
    host: &dyn InboxHost,
    team: &LaunchTeam,
    message_ids: Vec<i64>,
) -> Result<u32, String> {
    if message_ids.is_empty()
        || message_ids
            .iter()
            .any(|id| *id <= 0 || *id > 9_007_199_254_740_991)
    {
        return Err(
            "markUnread: messageIds must be a non-empty array of positive safe integer ids".into(),
        );
    }
    let path = host.team_store(team).await?;
    let changed_ids = message_ids.clone();
    let changed = with_store("mark_inbox_messages_unread", path, move |store| {
        inbox::inbox_mark_unread_many(store, &message_ids, HUMAN)
    })
    .await??;
    if changed > 0 {
        host.inbox_event(InboxEvent::new(InboxChange::Read, team, changed_ids));
    }
    Ok(changed)
}

/// Archive and read a batch of human messages with one change notification.
pub(crate) async fn mark_archived_many_by_human(
    host: &dyn InboxHost,
    team: &LaunchTeam,
    message_ids: Vec<i64>,
) -> Result<u32, String> {
    let path = host.team_store(team).await?;
    let changed_ids = message_ids.clone();
    let changed = with_store("mark_inbox_messages_archived", path, move |store| {
        inbox::inbox_mark_archived_many(store, &message_ids)
    })
    .await??;
    if changed > 0 {
        host.inbox_event(InboxEvent::new(InboxChange::Archived, team, changed_ids));
    }
    Ok(changed)
}

/// Undo an archive: unarchive a batch of human messages, restoring unread on those in
/// `restore_unread`, with one change notification.
pub(crate) async fn unarchive_many_by_human(
    host: &dyn InboxHost,
    team: &LaunchTeam,
    message_ids: Vec<i64>,
    restore_unread: Vec<i64>,
) -> Result<u32, String> {
    let path = host.team_store(team).await?;
    let changed_ids = message_ids.clone();
    let changed = with_store("unarchive_inbox_messages", path, move |store| {
        inbox::inbox_unarchive_many(store, &message_ids, &restore_unread)
    })
    .await??;
    if changed > 0 {
        host.inbox_event(InboxEvent::new(InboxChange::Archived, team, changed_ids));
    }
    Ok(changed)
}

/// A stored `reply_to` as a row shows it: a `role:human` (any case) stored before
/// replyTo was normalized means the human, and shows as the plain address, where a
/// reply to it goes.
pub(crate) fn shown_reply_to(reply_to: Option<String>) -> Option<String> {
    match reply_to.as_deref().map(ReplyTo::parse) {
        Some(ReplyTo::Role(role)) if role.eq_ignore_ascii_case(HUMAN) => Some(HUMAN.to_string()),
        _ => reply_to,
    }
}

/// The kind `m` counts as (see [`inbox::MessageKind::resolve`]), where a Reply-To naming a
/// role the sender holds or held in this store (`holders`, by its handle or tab session)
/// is its own.
pub(crate) fn message_kind(
    m: &InboxMessage,
    holders: &inbox::RoleHolders,
    sessions: &KnownSessions,
) -> inbox::MessageKind {
    let session = sessions.find(&m.from).and_then(|s| s.session.as_deref());
    let ids: Vec<&str> = std::iter::once(m.from.as_str()).chain(session).collect();
    inbox::MessageKind::resolve(
        m.kind,
        m.reply_to.as_deref(),
        m.via.as_deref(),
        &m.from,
        &|role| holders.any_holds(role, &ids),
    )
}

pub(crate) fn inbox_row(
    host: &dyn InboxHost,
    sessions: &KnownSessions,
    holders: &inbox::RoleHolders,
    mut m: InboxMessage,
) -> InboxRow {
    let kind = message_kind(&m, holders, sessions);
    m.reply_to = shown_reply_to(m.reply_to);
    let reply_to_name = m.reply_to.as_deref().map(|r| match ReplyTo::parse(r) {
        ReplyTo::Role(role) => role.to_string(),
        ReplyTo::Address(address) => address_name(host, sessions, address),
    });
    let reply_target_ended = match ReplyTo::parse(m.reply_to.as_deref().unwrap_or(&m.from)) {
        ReplyTo::Role(_) | ReplyTo::Address(HUMAN) => false,
        ReplyTo::Address(address) => !sessions.find(address).is_some_and(|s| s.running),
    };
    InboxRow {
        conversation: super::agent_read_model::conversation_key(&m, &Default::default()),
        to_role: m.role,
        reply_to_name,
        reply_target_ended,
        message_id: m.id,
        from_name: address_name(host, sessions, &m.from),
        to_name: address_name(host, sessions, &m.to),
        to_human: m.to == HUMAN,
        unread: m.read_at_ms.is_none() && m.withdrawn_at_ms.is_none(),
        from: m.from,
        to: m.to,
        text: m.text,
        created_at_ms: m.created_at_ms,
        in_reply_to: m.in_reply_to,
        in_reply_to_team: m.in_reply_to_team,
        primary_file: m.primary_file,
        attachments: m.attachments,
        archived_at_ms: m.archived_at_ms,
        delivery: m.delivery,
        on_click: m.on_click,
        content_type: m.content_type,
        reply_to: m.reply_to,
        summary: m.summary,
        via: m.via,
        kind,
        reactions: Vec::new(),
    }
}

/// Archive and read human-directed messages in one transaction.
#[tauri::command]
pub async fn mark_inbox_messages_archived(
    app: tauri::AppHandle,
    team: InboxTeam,
    message_ids: Vec<i64>,
) -> Result<u32, String> {
    mark_archived_many_by_human(&AppInboxHost { app }, &team.into(), message_ids).await
}

/// Unarchive human-directed messages, making `restore_unread` unread again, in one transaction.
#[tauri::command]
pub async fn unarchive_inbox_messages(
    app: tauri::AppHandle,
    team: InboxTeam,
    message_ids: Vec<i64>,
    restore_unread: Vec<i64>,
) -> Result<u32, String> {
    unarchive_many_by_human(
        &AppInboxHost { app },
        &team.into(),
        message_ids,
        restore_unread,
    )
    .await
}

/// Restore unread state for human-directed messages in the named team.
#[tauri::command]
pub async fn mark_inbox_messages_unread(
    app: tauri::AppHandle,
    team: InboxTeam,
    message_ids: Vec<i64>,
) -> Result<u32, String> {
    mark_unread_many_by_human(&AppInboxHost { app }, &team.into(), message_ids).await
}

/// Withdraw message `message_id` of `team`'s inbox before any agent picks it up. Anyone
/// may withdraw any message still waiting; one an agent already took is too late.
pub(crate) async fn withdraw_by_human(
    host: &dyn InboxHost,
    team: &LaunchTeam,
    message_id: i64,
) -> Result<WithdrawOutcome, String> {
    let path = host.team_store(team).await?;
    let now = chrono::Utc::now().timestamp_millis();
    let (outcome, changed) = with_store("withdraw_inbox_message", path, move |store| {
        crate::weft::role_inbox::withdraw_message_changed(store, message_id, now).map_err(|e| {
            match e {
                crate::weft::role_inbox::RoleInboxError::Store(err) => err,
                other => crate::weft::sqlite::wv_refusal(other.to_string()),
            }
        })
    })
    .await??;
    if changed {
        host.inbox_event(InboxEvent::new(InboxChange::Withdrawn, team, [message_id]));
    }
    Ok(outcome)
}

/// Reserves a notice of `session_id`'s mail it was not told about (see
/// [`inbox::inbox_reserve_announcement`]): `Nothing` with none, or with no inbox here,
/// and `Err` when the store failed (logged). A tab no launch bound to a handle (one a
/// person opened and started an agent in by hand) is addressed by its session id.
async fn reserved_notice(
    host: &dyn InboxHost,
    session_id: &str,
    reoffer: bool,
) -> Result<Check, ()> {
    let handle = session_handle(host, session_id, None);
    // A session with no inbox here (no launch team and no active team) has no mail to
    // tell, and asking again will not give it one.
    let path = match session_store(host, &handle).await {
        Ok(path) => path,
        Err(e) => {
            log::debug!(target: "inbox", "no inbox for {handle}: {e}");
            return Ok(Check::Nothing);
        }
    };
    let chat = host
        .chat_announcement(session_id, &handle)
        .await
        .map_err(|e| {
            log::warn!(target: "inbox", "chat notice: {e}");
        })?;
    let chat_text = chat.as_ref().map(|c| (c.workspace_id.clone(), c.count));
    let (recipient, session) = (handle.clone(), session_id.to_string());
    match with_store("inbox_wait", path.path, move |store| {
        inbox::inbox_reserve_announcement(store, &recipient, &session, reoffer, chat)
    })
    .await
    {
        Ok(Ok(inbox::Reservation::Offer { token, count })) => Ok(Check::Offer {
            notice: codesense_host_proto::inbox_wait::mail_and_chat_notice(
                count,
                session_id,
                chat_text.as_ref().map(|(w, n)| (w.as_str(), *n)),
            ),
            token,
        }),
        Ok(Ok(inbox::Reservation::Nothing)) => Ok(Check::Nothing),
        Ok(Ok(inbox::Reservation::Held)) => Ok(Check::Held),
        Ok(Ok(inbox::Reservation::PendingUntil(at_ms))) => {
            let left = at_ms.saturating_sub(inbox::now_ms());
            Ok(Check::PendingFor(std::time::Duration::from_millis(
                u64::try_from(left).unwrap_or(0) + RESERVE_RECHECK_MARGIN_MS,
            )))
        }
        Ok(Err(e)) | Err(e) => {
            log::warn!(target: "inbox", "announcing mail for {handle} failed: {e}");
            Err(())
        }
    }
}

/// How long past a pending offer's lapse its waiter checks again, so the check finds it
/// lapsed.
pub(crate) const RESERVE_RECHECK_MARGIN_MS: u64 = 50;

/// The notice reserved under `token` for `session_id` was handed to its agent's hook:
/// what it told about is marked announced (see [`inbox::inbox_confirm_announcement`]).
/// The reservation is looked for in the store of the handle the session is bound to,
/// then (if the session was bound since it was offered) in its own. Idempotent; a stale or
/// unknown token, or a session with no inbox here, confirms nothing.
pub(crate) async fn confirm_notice(
    host: &dyn InboxHost,
    session_id: &str,
    token: &str,
) -> Result<bool, String> {
    let handle = session_handle(host, session_id, None);
    let mut tried = None;
    for recipient in [handle.as_str(), session_id] {
        let path = match session_store(host, recipient).await {
            Ok(location) => location,
            Err(e) => {
                log::debug!(target: "inbox", "no inbox for {recipient}: {e}");
                continue;
            }
        };
        let team = path.team;
        let path = path.path;
        if tried.as_ref() == Some(&path) {
            continue;
        }
        tried = Some(path.clone());
        let (sid, tok) = (session_id.to_string(), token.to_string());
        let chat = with_store("inbox_ack_chat", path.clone(), move |store| {
            inbox::inbox_reserved_chat(store, &sid, &tok)
        })
        .await??;
        if let Some((recipient, chat)) = chat {
            host.chat_told(session_id, &recipient, &chat).await?;
        }
        let (session, token) = (session_id.to_string(), token.to_string());
        let confirmed = with_store("inbox_ack", path, move |store| {
            inbox::inbox_confirm_announcement(store, &session, &token)
        })
        .await??;
        if confirmed {
            let recipient = recipient.to_string();
            let ids = with_store(
                "inbox_confirmed_ids",
                tried.clone().unwrap(),
                move |store| {
                    Ok(inbox::inbox_announced(store)?
                        .get(&recipient)
                        .map(|notice| notice.through_id)
                        .into_iter()
                        .collect::<Vec<_>>())
                },
            )
            .await??;
            host.inbox_event(InboxEvent::new(InboxChange::NoticeConfirmed, &team, ids));
            return Ok(true);
        }
    }
    log::debug!(target: "inbox", "no notice of {session_id} awaits confirm {token}");
    Ok(false)
}

/// The notice reserved under `token` will not reach `session_id`'s agent: it is offered
/// again at the session's next check rather than after the reservation's TTL. Best effort:
/// a failure only delays it until the TTL ends.
pub(crate) async fn release_notice(host: &dyn InboxHost, session_id: &str, token: &str) {
    let settle = inbox::inbox_release_announcement;
    settle_notice(
        host,
        session_id,
        token,
        "releasing",
        "inbox_release",
        settle,
    )
    .await
}

/// Whether the notice reserved under `token` reached `session_id`'s agent is unknown: it
/// is held, never offered again, until a confirm settles it (see
/// [`inbox::inbox_hold_announcement`]). Best effort: a failure leaves it to lapse and be
/// offered again after its TTL.
pub(crate) async fn hold_notice(host: &dyn InboxHost, session_id: &str, token: &str) {
    let settle = inbox::inbox_hold_announcement;
    settle_notice(host, session_id, token, "holding", "inbox_hold", settle).await
}

/// Applies `settle` to the notice reserved under `token` in `session_id`'s inbox, logging
/// (as `doing` it) any failure.
async fn settle_notice(
    host: &dyn InboxHost,
    session_id: &str,
    token: &str,
    doing: &str,
    label: &'static str,
    settle: fn(&WeftStore, &str) -> WvWriteResult<()>,
) {
    let path = match session_store(host, &session_handle(host, session_id, None)).await {
        Ok(location) => location.path,
        Err(e) => {
            log::warn!(target: "inbox", "{doing} a notice for {session_id} failed: {e}");
            return;
        }
    };
    let token = token.to_string();
    if let Ok(Err(e)) | Err(e) = with_store(label, path, move |store| settle(store, &token)).await {
        log::warn!(target: "inbox", "{doing} a notice for {session_id} failed: {e}");
    }
}

/// The inbox store of a team, from its context resolution. A team this machine does not
/// hold is a typed error, never a path (so nothing is created for it).
pub(crate) fn team_store_path(
    resolution: ContextResolution,
    team: &LaunchTeam,
) -> Result<PathBuf, String> {
    match resolution {
        ContextResolution::Ok(ctx) => crate::weft::resolve_weft_store_path(
            &ctx.codesense_home,
            &ctx.principal(),
            &team.org,
            &team.team,
        ),
        ContextResolution::Unauthorized => Err(format!("unknown team {}:{}", team.org, team.team)),
        ContextResolution::NotSynced => Err(format!(
            "team {}:{} is not synced on this machine",
            team.org, team.team
        )),
    }
}

/// The app-backed [`InboxHost`].
#[derive(Clone)]
pub(crate) struct AppInboxHost {
    pub app: tauri::AppHandle,
}

impl AppInboxHost {
    /// The team of the workspace whose local tab runs the session under `handle`, read
    /// from this Mac's terminal layout and kept, so its inbox stays in one team's store.
    /// The same read keeps the team of every tab opened by hand it lists, so listing many
    /// sessions reads the layout once.
    async fn read_tab_team(&self, handle: &str) -> Option<LaunchTeam> {
        use crate::commands::pty_layout::WORKSPACE_KEY_SEPARATOR;
        let launch = self.launch();
        let session = launch.as_ref().and_then(|l| l.resolve(handle));
        let tabs = crate::mcp::terminal_tabs::LiveTabs::read_local(&self.app)
            .await
            .tabs;
        let team_of = |tab: &crate::mcp::terminal_tabs::LiveTab| {
            let (team_key, _) = tab.workspace_key.split_once(WORKSPACE_KEY_SEPARATOR)?;
            crate::commands::dashboard_launch::launch_team_from_key(team_key)
        };
        let found = crate::mcp::terminal_tabs::by_handle(
            &tabs,
            handle,
            Some(session.as_deref().unwrap_or(handle)),
        )
        .and_then(team_of);
        let Some(launch) = launch else {
            return found;
        };
        // A tab opened by hand has no handle: it is addressed by its own session.
        for tab in tabs.iter().filter(|tab| tab.handle.is_none()) {
            if let Some(team) = team_of(tab) {
                launch.keep_tab_team(&tab.session, team);
            }
        }
        match found {
            Some(team) => Some(launch.keep_tab_team(handle, team)),
            None => {
                launch.tab_team_missed(handle);
                None
            }
        }
    }

    fn launch(&self) -> Option<tauri::State<'_, super::dashboard_launch::DashboardLaunchState>> {
        use tauri::Manager;
        self.app
            .try_state::<super::dashboard_launch::DashboardLaunchState>()
    }

    /// The host, behind the port the inbox operations take.
    pub(crate) fn port(app: tauri::AppHandle) -> Arc<dyn InboxHost> {
        Arc::new(AppInboxHost { app })
    }
}

struct CallerMediaWorkspaceRow {
    handle: String,
    session_id: Option<String>,
    worktree_dir: Option<std::path::PathBuf>,
    is_remote: bool,
}

fn caller_media_workspace_from_snapshot(
    sender: &str,
    rows: impl IntoIterator<Item = CallerMediaWorkspaceRow>,
    handle_for_session: Option<&str>,
    is_remote_session: bool,
) -> Option<std::path::PathBuf> {
    if is_remote_session {
        return None;
    }
    let rows: Vec<_> = rows.into_iter().collect();
    if let Some(row) = rows.iter().find(|row| row.handle == sender) {
        return (!row.is_remote).then(|| row.worktree_dir.clone()).flatten();
    }
    let handle = handle_for_session?;
    rows.into_iter()
        .find(|row| {
            row.handle == handle && row.session_id.as_deref() == Some(sender) && !row.is_remote
        })
        .and_then(|row| row.worktree_dir)
}

#[async_trait::async_trait]
impl InboxHost for AppInboxHost {
    fn media_config(&self, team: &LaunchTeam) -> crate::media::Config {
        crate::media::config_sync(&self.app, team)
    }
    fn present_media(&self, team: &LaunchTeam, attachments: &mut [crate::media::Attachment]) {
        if attachments.is_empty() {
            return;
        }
        self.media_config(team).apply(attachments);
        crate::media::present_cached(&self.app, attachments);
    }
    fn cached_media(&self, sha: &str) -> Result<Vec<u8>, String> {
        crate::media::cached(&self.app, sha)
    }
    async fn own_media_sender(&self, sender: &str) -> bool {
        let tabs = crate::mcp::terminal_tabs::LiveTabs::read(&self.app).await;
        let session = self.launch().and_then(|l| l.resolve(sender));
        crate::mcp::terminal_tabs::by_handle(
            &tabs.tabs,
            sender,
            session.as_deref().or(Some(sender)),
        )
        .is_some_and(|t| !t.exited)
    }
    async fn caller_scope(&self, sender: &str) -> crate::context_repo::caller_scope::CallerScope {
        let remote = crate::mcp::remote_authority::caller_team(sender);
        crate::context_repo::caller_scope::for_session(&self.app, Some(sender), remote).await
    }
    async fn caller_media_workspace(&self, sender: &str) -> Option<std::path::PathBuf> {
        if crate::mcp::remote_authority::caller_team(sender).is_some() {
            return None;
        }
        let launch = self.launch()?;
        caller_media_workspace_from_snapshot(
            sender,
            launch
                .snapshot()
                .into_iter()
                .map(|row| CallerMediaWorkspaceRow {
                    handle: row.handle,
                    session_id: row.session_id,
                    worktree_dir: row.worktree_dir,
                    is_remote: row.machine_id.is_some(),
                }),
            launch.handle_for_session(sender).as_deref(),
            launch.remote_handle_for_session(sender).is_some(),
        )
    }
    async fn prepare_media(
        &self,
        team: &LaunchTeam,
        input: &crate::media::MediaInput,
        own: bool,
        scope: &crate::context_repo::caller_scope::CallerScope,
        workspace_root: Option<&std::path::Path>,
    ) -> Result<(crate::media::Attachment, bool), String> {
        crate::media::prepare(&self.app, team, input, own, scope, workspace_root).await
    }
    async fn fetch_media(
        &self,
        team: &LaunchTeam,
        attachment: crate::media::Attachment,
        automatic: bool,
    ) -> Result<crate::media::Attachment, String> {
        crate::media::fetch(&self.app, team, attachment, automatic).await
    }

    async fn admit_automatic_input(&self, session: &str) -> Result<(), String> {
        crate::session_usage::spend_caps::refuse_mail_offer(&self.app, session).await
    }

    async fn role_configs(
        &self,
        team: &LaunchTeam,
    ) -> Result<Vec<crate::queue_roles::RoleConfig>, String> {
        self.role_configs_for(team, &Default::default()).await
    }
    async fn role_configs_for(
        &self,
        team: &LaunchTeam,
        scope: &crate::context_repo::caller_scope::CallerScope,
    ) -> Result<Vec<crate::queue_roles::RoleConfig>, String> {
        let scope = scope.clone();
        let ctx =
            super::route_to_context::derive_context_for(&self.app, &team.org, &team.team).await?;
        super::await_blocking("role_configs", move || {
            crate::queue_roles::list_for(&super::roles::content(&ctx)?, &scope)
        })
        .await
    }

    async fn chat_announcement(
        &self,
        session: &str,
        handle: &str,
    ) -> Result<Option<inbox::ChatAnnouncement>, String> {
        let Some(workspace) = self.launch().and_then(|l| l.workspace_for_session(session)) else {
            return Ok(None);
        };
        let (app, handle, session) = (self.app.clone(), handle.to_string(), session.to_string());
        await_blocking("inbox.chat_announcement", move || {
            use tauri::Manager;
            let Some(store) = app
                .try_state::<super::workspace_chat::WorkspaceChatStore>()
                .filter(|s| crate::commands::workspace_chat::WorkspaceChatStore::is_installed(&s))
            else {
                return Ok(None);
            };
            let unread = store
                .unannounced(&workspace, &handle, &session)
                .map_err(|e| e.to_string())?;
            Ok((unread.count > 0).then_some(inbox::ChatAnnouncement {
                workspace_id: workspace,
                through_id: unread.through_id,
                count: unread.count,
            }))
        })
        .await
    }
    async fn chat_told(
        &self,
        session: &str,
        handle: &str,
        chat: &inbox::ChatAnnouncement,
    ) -> Result<(), String> {
        let (app, handle, session, chat) = (
            self.app.clone(),
            handle.to_string(),
            session.to_string(),
            chat.clone(),
        );
        await_blocking("inbox.chat_told", move || {
            use tauri::Manager;
            app.state::<super::workspace_chat::WorkspaceChatStore>()
                .note_told(&chat.workspace_id, &handle, &session, chat.through_id)
                .map_err(|e| e.to_string())
        })
        .await
    }
    async fn team_store(&self, team: &LaunchTeam) -> Result<PathBuf, String> {
        let resolution =
            super::route_to_context::derive_context_for(&self.app, &team.org, &team.team).await?;
        team_store_path(resolution, team)
    }

    async fn active_team(&self) -> Result<LaunchTeam, String> {
        let ctx = super::route_to_context::derive_active_context(&self.app).await?;
        let team = ctx.team_name.ok_or("no active team selected")?;
        Ok(LaunchTeam {
            org: ctx.org_slug,
            team,
        })
    }

    fn launch_team(&self, handle: &str) -> Option<LaunchTeam> {
        self.launch()?.launch_team(handle)
    }

    async fn tab_team(&self, handle: &str) -> Option<LaunchTeam> {
        // The layout cannot be read until the webview boots; until then the tab has none.
        if let Some(known) = self.launch().and_then(|l| l.known_tab_team(handle)) {
            return known;
        }
        self.read_tab_team(handle).await
    }

    fn handle_for_session(&self, session_id: &str) -> Option<String> {
        let launch = self.launch()?;
        // An agent on a machine runs under its tab's session there.
        launch
            .handle_for_session(session_id)
            .or_else(|| launch.remote_handle_for_session(session_id))
    }

    fn has_handle(&self, handle: &str) -> bool {
        self.launch().is_some_and(|l| l.has_handle(handle))
    }

    async fn sessions(&self) -> Result<KnownSessions, String> {
        let rows = crate::mcp::session_list::read_session_list(&self.app, None).await?;
        Ok(KnownSessions::new(
            rows.iter().map(KnownSession::from).collect(),
        ))
    }

    fn display_name(&self, handle: &str) -> Option<String> {
        self.launch()?.display_name(handle)
    }

    fn inbox_changed(&self) {
        use tauri::Emitter;
        // Fire-and-forget: a missing renderer refetches when the panel next mounts.
        let _ = self.app.emit(AGENT_INBOX_CHANGED_EVENT, ());
    }

    fn inbox_committed(&self, changes: &[(LaunchTeam, Vec<i64>)]) {
        use tauri::Emitter;
        let changes: Vec<_> = changes
            .iter()
            .map(|(team, ids)| serde_json::json!({"team": team, "messageIds": ids}))
            .collect();
        let _ = self.app.emit(
            AGENT_INBOX_CHANGED_EVENT,
            serde_json::json!({"changes": changes}),
        );
    }

    fn projected_inbox(&self, team: &LaunchTeam) -> Option<Result<InboxView, String>> {
        use tauri::Manager;
        Some(
            self.app
                .try_state::<crate::live_hub::LiveHub>()
                .ok_or_else(|| "live hub unavailable".to_string())
                .and_then(|hub| hub.inbox_view(team)),
        )
    }
    fn inbox_event(&self, event: InboxEvent) {
        use tauri::Emitter;
        let _ = self.app.emit(INBOX_EVENT, event);
    }

    fn mail_stored(&self, handle: &str) {
        // Its notice goes to the host its tab runs on, this Mac's or a machine's.
        super::host_inbox::mail_stored(&self.app, handle);
        crate::routines::inbox_idle::note(&self.app, handle);
    }

    fn role_message_queued(&self, team: &LaunchTeam, role: &str, message_id: i64, sender: &str) {
        crate::routines::role_pending::offer_role_pending(
            &self.app, team, role, message_id, sender,
        );
    }

    async fn synced_teams(&self) -> Result<Vec<LaunchTeam>, String> {
        let (home, user_id) = super::route_to_context::resolve_ambient(&self.app).await?;
        let contexts = super::async_helpers::await_blocking("inbox.synced_teams", move || {
            super::route_to_context::synced_contexts_in(&home, user_id)
        })
        .await?;
        Ok(contexts
            .into_iter()
            .filter_map(|ctx| {
                ctx.team_name.map(|team| LaunchTeam {
                    org: ctx.org_slug,
                    team,
                })
            })
            .collect())
    }

    fn note_role_store(&self, path: PathBuf) {
        super::role_inbox::note_role_store(&self.app, path);
    }

    fn roles_changed(&self) {
        super::agent_read_model::agent_state_changed(&self.app);
    }

    fn launch_restore_settled(&self) -> bool {
        // The notice host waits for a kept launch handle while restoration runs.
        self.launch()
            .is_some_and(|l| l.local_restore_caller_gate_settled())
    }

    /// SW-BSI-3: look up the story whose epic subtree owns `logical_path`.
    /// Needs the signed-in principal + the `codesense_home` the install index
    /// lives under; both come from the active resolver context. A failure to
    /// derive those is a silent `None` (same posture as other best-effort
    /// read-only resolutions on this host).
    async fn owning_story_for_path(&self, team: &LaunchTeam, logical_path: &str) -> Option<String> {
        let ctx = crate::commands::route_to_context::derive_active_context(&self.app)
            .await
            .ok()?;
        crate::install::attachment_resolver::owning_story_for_path(
            &ctx.codesense_home,
            &ctx.principal(),
            &team.org,
            &team.team,
            logical_path,
        )
    }
}

/// The launch spine's agent under `handle` is at its prompt with no turn to run (it was
/// launched with no prompt): it reads idle from now on, not unknown. Nothing is typed
/// into it.
pub(crate) fn on_session_ready(app: &tauri::AppHandle, handle: &str) {
    use tauri::Manager;
    let Some(launch) = app.try_state::<super::dashboard_launch::DashboardLaunchState>() else {
        return;
    };
    if let Some(session_id) = launch.resolve(handle) {
        launch.turns().record_ready(&session_id);
    }
}

/// The Agents panel's Messages list for `team` (the team the viewer shows): the newest
/// messages, and the human's unread count. Reading the list marks nothing read.
/// `scope` filters by recipient class — defaults to `to-human` for backward compat.
/// `recipient_agent_id` (an agent definition id) narrows the list to that agent's
/// conversation: messages to its role inbox, to its running instances' own inboxes
/// (`sessions`, their handles), and the replies under them.
#[tauri::command]
pub async fn list_inbox_messages(
    app: tauri::AppHandle,
    team: InboxTeam,
    scope: Option<InboxScope>,
    recipient_agent_id: Option<String>,
    sessions: Option<Vec<String>>,
) -> Result<InboxView, String> {
    list_messages(
        &AppInboxHost { app },
        &team.into(),
        scope.unwrap_or_default(),
        recipient_agent_id.as_deref(),
        &sessions.unwrap_or_default(),
    )
    .await
}

/// Preview a reply's destination by ID without marking the original message read.
#[tauri::command]
pub async fn inbox_reply_recipient(
    app: tauri::AppHandle,
    team: InboxTeam,
    message_id: i64,
) -> Result<Option<String>, String> {
    let own = team_inbox(&AppInboxHost { app }, team.into()).await?;
    stored_reply_recipient(own.path, message_id).await
}

/// The human replies to message `message_id` of `team`'s inbox from the Agents panel
/// or a dashboard. `content_type` is how the reply renders (markdown by default);
/// `summary` is its optional one-line list preview.
#[tauri::command]
pub async fn reply_as_human(
    app: tauri::AppHandle,
    team: InboxTeam,
    message_id: i64,
    text: String,
    content_type: Option<inbox::MessageContentType>,
    summary: Option<String>,
) -> Result<SendResult, String> {
    reply_from_human(
        &AppInboxHost::port(app),
        &team.into(),
        message_id,
        &text,
        content_type,
        summary.as_deref(),
    )
    .await
}

/// The human reacts to message `message_id` of `team`'s inbox with one emoji: `remove`
/// omitted toggles, `true` takes it back, `false` makes sure it is there.
#[tauri::command]
pub async fn react_as_human(
    app: tauri::AppHandle,
    team: InboxTeam,
    message_id: i64,
    emoji: String,
    remove: Option<bool>,
) -> Result<ReactResult, String> {
    react_from_human(
        &AppInboxHost { app },
        &team.into(),
        message_id,
        &emoji,
        remove,
    )
    .await
}

/// The human read message `message_id` of `team`'s inbox in the Agents panel.
#[tauri::command]
pub async fn mark_inbox_message_read(
    app: tauri::AppHandle,
    team: InboxTeam,
    message_id: i64,
) -> Result<(), String> {
    mark_read_by_human(&AppInboxHost { app }, &team.into(), message_id).await
}

/// SW-AGR-18: the human read every message of `message_ids` in `team`'s inbox at once.
/// Returns how many changed.
#[tauri::command]
pub async fn mark_inbox_messages_read(
    app: tauri::AppHandle,
    team: InboxTeam,
    message_ids: Vec<i64>,
) -> Result<u32, String> {
    mark_read_many_by_human(&AppInboxHost { app }, &team.into(), message_ids).await
}

/// Withdraw message `message_id` of `team`'s inbox before any agent picks it up.
#[tauri::command]
pub async fn withdraw_inbox_message(
    app: tauri::AppHandle,
    team: InboxTeam,
    message_id: i64,
) -> Result<WithdrawOutcome, String> {
    withdraw_by_human(&AppInboxHost { app }, &team.into(), message_id).await
}

// SW-BSI-UX agent updates and human status Tauri commands.

/// The newest agent updates (progress notifications) for `team`'s inbox.
#[tauri::command]
pub async fn list_agent_updates(
    app: tauri::AppHandle,
    team: InboxTeam,
    limit: Option<u32>,
) -> Result<Vec<crate::weft::agent_updates::AgentUpdate>, String> {
    super::agent_read_model::updates(&AppInboxHost { app }, &team.into(), limit)
        .await
        .map(|view| view.updates)
}

/// The human's current online/away status for `team`'s inbox. Default is `online`
/// when no row has been set yet.
#[tauri::command]
pub async fn get_human_status(
    app: tauri::AppHandle,
    team: InboxTeam,
) -> Result<crate::weft::human_status::HumanStatusView, String> {
    super::agent_read_model::human_status(&AppInboxHost { app }, &team.into()).await
}

/// Set the human's status for `team`'s inbox. `note` is an optional free-text
/// hint (<=200 chars). Writing `online` with no note is the default "I'm back"
/// reset.
#[tauri::command]
pub async fn set_human_status(
    app: tauri::AppHandle,
    team: InboxTeam,
    status: crate::weft::human_status::HumanStatus,
    note: Option<String>,
) -> Result<crate::weft::human_status::HumanStatusView, String> {
    let host = AppInboxHost { app };
    let path = host.team_store(&team.into()).await?;
    let note_moved = note;
    let view = with_store("set_human_status", path, move |store| {
        crate::weft::human_status::human_status_set(store, status, note_moved.as_deref())
    })
    .await?
    .map_err(|e| e.to_string())?;
    // Reuse the inbox-changed event so the viewer refetches both the status chip
    // and anything else keyed to inbox lifecycle (cheap — no new event wire).
    host.inbox_changed();
    super::agent_read_model::agent_state_changed(&host.app);
    Ok(view)
}

/// SW-BSI-3 Phase 2: what the inbox knows about an attached file ahead of
/// actually trying to open it. The UI uses this to render a precise
/// unavailable chip — a known owning story on a remote workspace that hasn't
/// hydrated it offers a Hydrate button; an owner we cannot find at all shows
/// generic unavailable; a tombstoned or missing attachment names its state
/// honestly. A remote agent uses the matching `AttachmentHint` on
/// `read_messages` instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[allow(dead_code)] // Tombstoned/Missing reserved; written by follow-on content-engine hooks.
pub(crate) enum AttachmentStatus {
    /// The owning story is linked and the attachment should resolve on open.
    /// The viewer's own missing-file state still governs the final rendering.
    Ready { owning_story_id: String },
    /// The owning story is linked, but the targeted remote workspace has NOT
    /// hydrated it yet. UI offers a `[Hydrate story: <id>]` button that calls
    /// `trigger_hydrate_from_inbox` with these exact args.
    #[serde(rename_all = "camelCase")]
    NotHydratedOnRemote {
        machine_id: String,
        workspace_handle: String,
        owning_story_id: String,
    },
    /// No linked worktree record claims this path's subtree. The viewer falls
    /// through to its generic unavailable chip; no hydration call is offered.
    OwningStoryUnknown,
    /// A later observation marks the attachment removed (reserved — no store
    /// writes a tombstone today; a follow-on content-engine hook will).
    Tombstoned,
    /// Observed-missing (reserved — a transient read failure the viewer owns
    /// today; promoted here when the resolver confirms the path is gone).
    Missing,
}

/// A remote workspace to resolve `inbox_attachment_status` against. Absent =
/// local resolution; present = ask the named remote machine whether it has
/// hydrated the owning story.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InboxRemoteTarget {
    pub machine_id: String,
    pub workspace_handle: String,
}

/// `inbox_attachment_status`: resolve what we know about `primary_file` for
/// `team` without opening it. With no `remote_target`, local resolution
/// returns `ready | owningStoryUnknown`. With a `remote_target`, we also ask
/// the named machine's hydration index: if the owning story is NOT in the
/// remote's hydrated epic set, return `notHydratedOnRemote` with the args a
/// subsequent `trigger_hydrate_from_inbox` call needs.
#[tauri::command]
pub async fn inbox_attachment_status(
    app: tauri::AppHandle,
    team: InboxTeam,
    primary_file: String,
    remote_target: Option<InboxRemoteTarget>,
) -> Result<AttachmentStatus, String> {
    let host = AppInboxHost { app: app.clone() };
    let team: LaunchTeam = team.into();
    let Some(owning_story_id) = host.owning_story_for_path(&team, &primary_file).await else {
        return Ok(AttachmentStatus::OwningStoryUnknown);
    };
    let Some(target) = remote_target else {
        return Ok(AttachmentStatus::Ready { owning_story_id });
    };
    // Remote-side check: resolve the remote identity + read its hydration facts.
    // Any failure degrades to Ready — a transient resolve failure should not
    // fabricate an unavailable state. The caller still gets accurate data if
    // the real remote probe runs.
    match remote_hydration_facts(&app, &target, &owning_story_id).await {
        Ok(Some(false)) => Ok(AttachmentStatus::NotHydratedOnRemote {
            machine_id: target.machine_id,
            workspace_handle: target.workspace_handle,
            owning_story_id,
        }),
        _ => Ok(AttachmentStatus::Ready { owning_story_id }),
    }
}

/// Query `machine_id`'s `workspace_handle` for its hydrated-stories set and
/// report whether `owning_story_id` is in it. `Ok(Some(true))` = hydrated,
/// `Ok(Some(false))` = known not hydrated, `Ok(None)` = cannot determine
/// (missing team context, resolver fault, etc.). The caller degrades None to
/// Ready so the UI never fabricates an unavailable state.
async fn remote_hydration_facts(
    app: &tauri::AppHandle,
    target: &InboxRemoteTarget,
    owning_story_id: &str,
) -> Result<Option<bool>, String> {
    let ctx = match crate::commands::route_to_context::derive_active_context(app).await {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };
    let resolver_ctx = ctx.to_minimal_resolver_context();
    let resolved = match crate::mcp::remote_destination::resolve(
        app.clone(),
        ctx.clone(),
        &target.machine_id,
        &target.workspace_handle,
        None,
    )
    .await
    {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };
    // SW-XS-8: what a workspace was hydrated with lives in its managed record.
    let identity = resolved.io.identity().clone();
    let home = resolver_ctx.codesense_home.clone();
    let principal = crate::domain::Principal::from(resolver_ctx.user_id);
    let record = tokio::task::spawn_blocking(move || {
        crate::agents::prepare::ManagedRecord::load(&home, &principal, &identity)
    })
    .await
    .map_err(|e| e.to_string())?;
    // Story hydration is tracked at the epic-dir level; a story's subtree is
    // materialised when its epic dir sits in `hydrated_epic_dirs`. A
    // conservative check: match by story_id substring (epic dirs often name
    // the story, e.g. `08-dashboards-and-agents/25-builder-skills-integration`
    // owns SW-BSI-3). This stays correct for the current layout; a stricter
    // check belongs in `attachment_resolver` alongside the owning-story map.
    let any_hit = record
        .hydrated_epics
        .iter()
        .any(|d| d.contains(owning_story_id));
    Ok(Some(any_hit))
}

/// `trigger_hydrate_from_inbox`: the UI's `[Hydrate story: X]` button — an
/// explicit human action that runs the same forward `hydrate_story` on the
/// engine (SW-XS-8): never a reverse sync. Returns the operation's result
/// verbatim so the UI can surface errors precisely.
#[tauri::command]
pub async fn trigger_hydrate_from_inbox(
    app: tauri::AppHandle,
    machine_id: String,
    workspace_handle: String,
    story_id: String,
) -> Result<serde_json::Value, String> {
    crate::mcp::context_operations::explicit::run(
        app,
        crate::mcp::context_operations::WorkspaceTarget::Remote {
            machine_id,
            workspace_handle,
        },
        None,
        crate::mcp::context_operations::explicit::Op::Story {
            story_id,
            policy: None,
        },
        None,
        /* caller */ None,
        /* connection session */ None,
    )
    .await
    .map_err(|e| e.message.to_string())
}

#[cfg(test)]
pub(crate) mod tests;

/// Refresh role delivery rows through the same typed inbox event as direct mail.
pub(crate) async fn role_delivery_changed(
    host: &dyn InboxHost,
    team: &LaunchTeam,
    delivery_id: Option<i64>,
    role: Option<&str>,
) -> Result<(), String> {
    let path = host.team_store(team).await?;
    let role = role.map(str::to_string);
    let ids=with_store("role_delivery_changed",path,move |store| {
        // RI-2-ALLOW: with_store executes this closure on the blocking pool.
        let conn=crate::weft::WeftStore::conn(&store)?;
        let mut stmt=conn.prepare("SELECT message_id FROM wv_role_delivery WHERE (?1 IS NULL OR delivery_id=?1) AND (?2 IS NULL OR role=?2)")?;
        let rows=stmt.query_map(rusqlite::params![delivery_id,role],|r|r.get::<_,i64>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }).await??;
    host.inbox_event(InboxEvent::new(InboxChange::RoleClaim, team, ids));
    Ok(())
}

fn start_media_fetch(
    host: Arc<dyn InboxHost>,
    location: InboxLocation,
    message_id: i64,
    attachment: crate::media::Attachment,
) {
    tauri::async_runtime::spawn(async move {
        if let Err(e) = finish_media_fetch(&host, location, message_id, attachment, true).await {
            log::warn!("inbox media: {e}");
        }
    });
}
async fn finish_media_fetch(
    host: &Arc<dyn InboxHost>,
    location: InboxLocation,
    message_id: i64,
    attachment: crate::media::Attachment,
    automatic: bool,
) -> Result<(), String> {
    let attachment = host
        .fetch_media(&location.team, attachment, automatic)
        .await?;
    with_store("media.loaded", location.path, move |store| {
        inbox::inbox_set_attachments(store, message_id, &[attachment])
    })
    .await?
    .map_err(|e| e.to_string())?;
    host.inbox_event(InboxEvent::new(
        InboxChange::Stored,
        &location.team,
        [message_id],
    ));
    Ok(())
}
#[tauri::command]
pub async fn load_inbox_media(
    app: tauri::AppHandle,
    team: InboxTeam,
    message_id: i64,
) -> Result<(), String> {
    let host = AppInboxHost::port(app);
    let location = team_inbox(&*host, team.into()).await?;
    if host.media_config(&location.team).remote == crate::media::Remote::Off {
        return Err("media is off in team config".into());
    }
    let mut row = with_store("media.load", location.path.clone(), move |s| {
        inbox::inbox_get(s, message_id)
    })
    .await?
    .map_err(|e| e.to_string())?
    .ok_or("message unavailable")?;
    host.present_media(&location.team, &mut row.attachments);
    let a = row
        .attachments
        .into_iter()
        .next()
        .ok_or("message has no media")?;
    if a.state != "pending" && !(a.state == "failed" && a.retryable) && a.state != "blocked" {
        return Err("media cannot be loaded".into());
    }
    finish_media_fetch(&host, location, message_id, a, false).await
}
