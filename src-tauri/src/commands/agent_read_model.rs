// SPDX-License-Identifier: MIT
//! One read model of running agents and their inboxes, shared by dashboards (the page
//! bridge, through the Tauri commands below) and agents (the MCP tools). Both get the
//! same shapes from the same functions, and neither sees how the Weft ledger stores
//! them.
//!
//! - **sessions**: the `list_sessions` row ([`SessionListRow`]).
//! - **agent**: a saved Agent as `agent_get` returns it, with its running `instances`.
//! - **inbox**: messages by scope, filtered by agent, session or role. Listing marks
//!   nothing read.
//! - **roles**: each role's registrations and delivery queue ([`RoleQueueView`]).
//! - **updates**: agent progress notes for dashboards ([`AgentUpdate`]).
//! - **human status** ([`HumanStatusView`]).
//!
//! The human's writes from a dashboard ([`send_as_human`] and the existing reply, mark
//! read and status commands) go through the same inbox operations the app uses.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::agent_inbox::{
    address_name, list_messages_unbounded, send_message_replying, team_inbox, with_store,
    AppInboxHost, InboxHost, InboxScope, InboxTeam, InboxView, KnownSessions, SendResult,
};
use super::await_blocking;
use super::dashboard_launch::LaunchTeam;
use crate::live_hub::inbox_events::{InboxChange, InboxEvent};
use crate::mcp::session_list::SessionListRow;
use crate::weft::agent_updates::AgentUpdate;
use crate::weft::human_status::HumanStatusView;
use crate::weft::inbox::{
    InboxDeliveryState, InboxMessage, InboxReplyTeam, MessageContentType, MessageKind,
    MessageOnClick, HUMAN,
};
use crate::weft::role_state::{RoleQueueView, ROLE_STATE_DONE_DEFAULT_LIMIT};
use crate::weft::WeftStore;

/// Tells the viewer that role registrations or queues, or the human's status changed, so
/// live dashboards re-read (`codesense:agents-changed` in the page). Mirrored in
/// `src/tauri/events.ts`.
pub(crate) const AGENT_STATE_CHANGED_EVENT: &str = "agent-state-changed";

/// Tell the viewer the agents' state changed (see [`AGENT_STATE_CHANGED_EVENT`]).
pub(crate) fn agent_state_changed(app: &tauri::AppHandle) {
    use tauri::Emitter;
    // Fire-and-forget: a page that misses it re-reads on its next change or load.
    let _ = app.emit(AGENT_STATE_CHANGED_EVENT, ());
}

// ── team ────────────────────────────────────────────────────────────────────────

/// The team a read acts on: the one named; else the team `session` (a handle or a
/// session id) was launched in; else the team active in the app.
pub(crate) async fn resolve_team(
    host: &dyn InboxHost,
    team: Option<InboxTeam>,
    session: Option<&str>,
) -> Result<LaunchTeam, String> {
    if let Some(team) = team {
        return Ok(team.into());
    }
    if let Some(handle) = session.and_then(|s| session_handle(host, s)) {
        if let Some(team) = crate::commands::agent_inbox::handle_team(host, &handle).await {
            return Ok(team);
        }
    }
    host.active_team().await
}

/// The org whose context repo a call acts on, for an agent that may work in another org
/// than the one shown in the app: the org named; else the org the session named (a handle
/// or a session id) was launched in, refusing a session CodeSense does not know; else the
/// org of the caller's own tab (`own_session`, else `own_org` for a remote tab). `None`
/// when nothing names one: the org active in the app applies.
pub(crate) fn caller_org(
    host: &dyn InboxHost,
    named_org: Option<&str>,
    named_session: Option<&str>,
    own_session: Option<&str>,
    own_org: Option<&str>,
) -> Result<Option<String>, String> {
    fn given(v: Option<&str>) -> Option<&str> {
        v.map(str::trim).filter(|v| !v.is_empty())
    }
    if let Some(org) = given(named_org) {
        return Ok(Some(org.to_string()));
    }
    if let Some(session) = given(named_session) {
        let team = session_handle(host, session).and_then(|h| host.launch_team(&h));
        return match team {
            Some(team) => Ok(Some(team.org)),
            None => Err(format!(
                "{session} is not a session CodeSense launched; pass your $CODESENSE_TAB_SESSION_ID or {{org}}"
            )),
        };
    }
    if let Some(team) = given(own_session)
        .and_then(|s| session_handle(host, s))
        .and_then(|h| host.launch_team(&h))
    {
        return Ok(Some(team.org));
    }
    Ok(given(own_org).map(str::to_string))
}

/// The handle `session` names: a session id's handle, or the handle itself.
fn session_handle(host: &dyn InboxHost, session: &str) -> Option<String> {
    let session = session.trim();
    if session.is_empty() {
        return None;
    }
    Some(super::agent_inbox::session_handle(host, session, None))
}

// ── sessions ────────────────────────────────────────────────────────────────────

/// Every session, as `list_sessions` lists it.
pub(crate) async fn sessions(app: &tauri::AppHandle) -> Result<Vec<SessionListRow>, String> {
    crate::mcp::session_list::read_session_list(app, None).await
}

/// The sessions running saved Agent `agent`.
pub(crate) fn instances_of(sessions: &[SessionListRow], agent: &str) -> Vec<SessionListRow> {
    sessions
        .iter()
        .filter(|s| s.agent.as_deref() == Some(agent))
        .cloned()
        .collect()
}

// ── inbox ───────────────────────────────────────────────────────────────────────

/// What `inbox_list` (MCP) and `inbox()` (bridge) take. Both spellings of each key are
/// accepted (`agentId` / `agent_id`).
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InboxQuery {
    /// `to-human` (messages to the human), `to-agents` (messages to sessions and
    /// roles) or `all`. Default: `to-human` with no filter below, `all` with one.
    #[serde(default)]
    #[schemars(with = "Option<String>")]
    pub scope: Option<InboxScope>,
    /// A saved Agent's id (`agent_list`): its role inbox, the inboxes of its running
    /// instances, and every reply under them.
    #[serde(default, alias = "agent_id")]
    pub agent_id: Option<String>,
    /// A session (a `list_sessions` handle or a session id): what it sent and received,
    /// and every reply under them.
    #[serde(default, alias = "session_id")]
    pub session_id: Option<String>,
    /// A role (any role name, usually a saved Agent's id): messages to and from it, and replies.
    #[serde(default)]
    pub role: Option<String>,
    /// At most this many messages, newest first: at most 500 (the inbox keeps the newest
    /// 500); an agent's `inbox_list` lists 50 when it names none, a page's `inbox()` 500.
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub conversation: Option<String>,
    /// A messageId: only messages older than it. To page, pass the last listed messageId
    /// while `hasMore`.
    #[serde(default)]
    pub before: Option<i64>,
    /// Only messages of this effective kind: `needs-you` (waits on the human) or
    /// `update` (informational). Applied before `limit`.
    #[serde(default)]
    pub kind: Option<MessageKind>,
}

impl InboxQuery {
    fn has_filter(&self) -> bool {
        [&self.agent_id, &self.session_id, &self.role]
            .iter()
            .any(|v| v.as_deref().is_some_and(|s| !s.trim().is_empty()))
    }
}

/// `team`'s inbox as `query` asks, newest first, with the human's unread count. A
/// view: it marks nothing read. `sessions` are the listed sessions, for an agent's
/// running instances.
pub(crate) async fn inbox(
    host: &dyn InboxHost,
    team: &LaunchTeam,
    query: &InboxQuery,
    sessions: &[SessionListRow],
) -> Result<InboxView, String> {
    inbox_matching(host, team, query, sessions, &|_| true).await
}

/// [`inbox`] keeping only the rows `keep` admits, before `limit` and `has_more`.
pub(crate) async fn inbox_matching(
    host: &dyn InboxHost,
    team: &LaunchTeam,
    query: &InboxQuery,
    sessions: &[SessionListRow],
    keep: &(dyn Fn(&super::agent_inbox::InboxRow) -> bool + Sync),
) -> Result<InboxView, String> {
    let trimmed = |v: &Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    // `human` is neither an agent nor a session (its mail is the whole human inbox):
    // as an agent or session filter it names nothing, as a role filter does below.
    let not_human = |v: &String| !v.eq_ignore_ascii_case(HUMAN);
    let mut addresses: Vec<String> = Vec::new();
    if let Some(agent) = trimmed(&query.agent_id).filter(not_human) {
        addresses.extend(instances_of(sessions, &agent).into_iter().map(|s| s.handle));
        addresses.insert(0, agent);
    }
    if let Some(session) = trimmed(&query.session_id).filter(not_human) {
        addresses.extend(session_handle(host, &session).filter(not_human));
    }
    // `role:x` (the Reply-To spelling) reads the queue `x`, as every write keys it.
    if let Some(role) = trimmed(&query.role) {
        let role = crate::weft::role_inbox::role_name(&role);
        // `human` is no role queue (writes and role_state refuse it): like a bare
        // `role:`, it names nothing.
        if !role.is_empty() && !role.eq_ignore_ascii_case(HUMAN) {
            addresses.push(role.to_string());
        }
    }
    let scope = query
        .scope
        .unwrap_or(if query.has_filter() || query.conversation.is_some() {
            InboxScope::All
        } else {
            InboxScope::ToHuman
        });
    let mut view = match addresses.split_first() {
        Some((first, rest)) => {
            list_messages_unbounded(host, team, scope, Some(first), rest).await?
        }
        None => list_messages_unbounded(host, team, scope, None, &[]).await?,
    };
    // A filter that names nothing (a bare `role:`, or only `human`) matches nothing, rather than
    // falling through to every message in scope.
    if query.has_filter() && addresses.is_empty() {
        view.messages.clear();
    }
    view.messages.retain(|m| keep(m));
    if let Some(kind) = query.kind {
        view.messages.retain(|m| m.kind == kind);
    }
    if let Some(conversation) = &query.conversation {
        view.messages.retain(|m| &m.conversation == conversation);
    }
    if let Some(before) = query.before {
        view.messages.retain(|m| m.message_id < before);
    }
    let limit = query.limit.filter(|l| *l > 0).unwrap_or(500).min(500) as usize;
    view.matched = view.messages.len();
    view.has_more = view.messages.len() > limit;
    view.messages.truncate(limit);
    Ok(view)
}

/// What an agent's `inbox_list` asks for beyond the bounded default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(inline)]
pub enum InboxInclude {
    /// Every body in full, however long.
    Text,
    /// The human's conversation summaries (`conversations`).
    Conversations,
}

/// The messages an agent's `inbox_list` lists when it names no `limit`.
pub(crate) const AGENT_INBOX_LIMIT: u32 = 50;
/// Longer bodies reach an agent as a `textPreview` unless it includes `text`.
pub(crate) const AGENT_TEXT_MAX_BYTES: usize = 2048;
/// How much of a cut body its `textPreview` keeps.
pub(crate) const AGENT_PREVIEW_BYTES: usize = 512;
/// The most one agent answer weighs; older messages past it wait for the next page.
pub(crate) const AGENT_ANSWER_BUDGET: usize = 64 * 1024;

/// An agent's `inbox_list`: `read` (the shared inbox read) of `query` with
/// [`AGENT_INBOX_LIMIT`] when it names none, shaped by [`agent_inbox_answer`]. The served
/// handler and its tests run this one path.
pub(crate) async fn agent_inbox<F, Fut>(
    query: &InboxQuery,
    include: &[InboxInclude],
    read: F,
) -> Result<serde_json::Value, String>
where
    F: FnOnce(InboxQuery) -> Fut,
    Fut: std::future::Future<Output = Result<InboxView, String>>,
{
    let mut query = InboxQuery::clone(query);
    query.limit = Some(Option::unwrap_or(
        Option::filter(query.limit, |limit| *limit > 0),
        AGENT_INBOX_LIMIT,
    ));
    agent_inbox_answer(&read(query).await?, include)
}

/// `view` as an agent gets it: bodies over [`AGENT_TEXT_MAX_BYTES`] cut to a `textPreview`
/// with `truncated: true` (unless it includes `text`), conversations only when included,
/// `total` (the messages that matched, older than `before` when given, ahead of `limit`), and the newest messages that fit
/// [`AGENT_ANSWER_BUDGET`]. A cut sets `hasMore`; the next page is `before` the last listed
/// `messageId`.
pub(crate) fn agent_inbox_answer(
    view: &InboxView,
    include: &[InboxInclude],
) -> Result<serde_json::Value, String> {
    let wants = |what: InboxInclude| <[InboxInclude]>::contains(include, &what);
    let mut answer = Result::map_err(serde_json::to_value(view), |e| ToString::to_string(&e))?;
    let fields = Option::ok_or(
        serde_json::Value::as_object_mut(&mut answer),
        "inbox view is not an object",
    )?;
    if !wants(InboxInclude::Conversations) {
        serde_json::Map::remove(fields, "conversations");
    }
    serde_json::Map::insert(
        fields,
        std::string::String::from("total"),
        serde_json::Value::from(view.matched),
    );
    let messages = match serde_json::Map::remove(fields, "messages") {
        Some(serde_json::Value::Array(messages)) => messages,
        _ => std::vec::Vec::new(),
    };
    let size = |value: &serde_json::Value| {
        let bytes = Result::map_err(serde_json::to_vec(value), |e| ToString::to_string(&e))?;
        Ok::<_, String>(std::vec::Vec::len(&bytes))
    };
    // `"messages":[]` and the `hasMore` it may turn true are measured with the rest.
    let mut used = size(&serde_json::Value::Object(serde_json::Map::clone(fields)))?
        + str::len(r#","messages":[],"hasMore":false"#);
    let mut kept = std::vec::Vec::new();
    for mut message in messages {
        if !wants(InboxInclude::Text) {
            preview_long_text(&mut message);
        }
        let weight = size(&message)? + 1;
        if !std::vec::Vec::is_empty(&kept) && used + weight > AGENT_ANSWER_BUDGET {
            serde_json::Map::insert(
                fields,
                std::string::String::from("hasMore"),
                serde_json::Value::Bool(true),
            );
            break;
        }
        used += weight;
        std::vec::Vec::push(&mut kept, message);
    }
    serde_json::Map::insert(
        fields,
        std::string::String::from("messages"),
        serde_json::Value::Array(kept),
    );
    Ok(answer)
}

/// Replace a body longer than [`AGENT_TEXT_MAX_BYTES`] by its first
/// [`AGENT_PREVIEW_BYTES`] (on a char boundary) as `textPreview`, with `truncated: true`.
fn preview_long_text(message: &mut serde_json::Value) {
    let Some(fields) = serde_json::Value::as_object_mut(message) else {
        return;
    };
    let Some(text) = Option::and_then(
        serde_json::Map::get(fields, "text"),
        serde_json::Value::as_str,
    ) else {
        return;
    };
    if str::len(text) <= AGENT_TEXT_MAX_BYTES {
        return;
    }
    let mut end = AGENT_PREVIEW_BYTES;
    while !str::is_char_boundary(text, end) {
        end -= 1;
    }
    let preview = str::to_owned(&text[..end]);
    serde_json::Map::remove(fields, "text");
    serde_json::Map::insert(
        fields,
        std::string::String::from("textPreview"),
        serde_json::Value::String(preview),
    );
    serde_json::Map::insert(
        fields,
        std::string::String::from("truncated"),
        serde_json::Value::Bool(true),
    );
}

// ── roles ───────────────────────────────────────────────────────────────────────

/// What `role_state` returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RoleStateView {
    pub roles: Vec<RoleQueueView>,
}

/// Display names for the session handles a role read carries.
#[derive(Debug, Default)]
pub(crate) struct Names {
    pub sessions: HashMap<String, String>,
}

impl Names {
    pub(crate) fn from_sessions(sessions: &[SessionListRow]) -> Self {
        Names {
            sessions: sessions
                .iter()
                .map(|s| (s.handle.clone(), s.display_name.clone()))
                .collect(),
        }
    }

    /// The display name of `who`: a session id, a handle, or `human`.
    fn session(&self, host: &dyn InboxHost, who: &str) -> Option<String> {
        if who == HUMAN {
            return Some(HUMAN.to_string());
        }
        let handle = session_handle(host, who)?;
        self.sessions
            .get(&handle)
            .cloned()
            .or_else(|| host.display_name(&handle))
    }
}

fn apply_role_configs(
    roles: &mut Vec<RoleQueueView>,
    configs: &[crate::queue_roles::RoleConfig],
    role_filter: Option<&str>,
) {
    for config in configs {
        if role_filter.is_some_and(|id| crate::weft::role_inbox::role_name(id) != config.id)
            || roles.iter().any(|q| q.role == config.id)
        {
            continue;
        }
        roles.push(RoleQueueView {
            role: config.id.clone(),
            role_name: None,
            registrations: vec![],
            counts: Default::default(),
            deliveries: vec![],
            deliveries_truncated: false,
        });
    }

    for queue in roles {
        queue.role_name = configs
            .iter()
            .find(|c| c.id == queue.role)
            .map(|c| c.name.clone());
    }
}

/// The role mailboxes of `team` (only `role`, when named): who is registered and where
/// each delivery stands, with names filled from `names`. Claims nothing.
pub(crate) async fn roles(
    host: &dyn InboxHost,
    team: &LaunchTeam,
    role: Option<String>,
    done_limit: Option<u32>,
    names: &Names,
) -> Result<RoleStateView, String> {
    roles_for(host, team, role, done_limit, names, &Default::default()).await
}

pub(crate) async fn roles_for(
    host: &dyn InboxHost,
    team: &LaunchTeam,
    role: Option<String>,
    done_limit: Option<u32>,
    names: &Names,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<RoleStateView, String> {
    let path = host.team_store(team).await?;
    let now = chrono::Utc::now().timestamp();
    let limit = done_limit.unwrap_or(ROLE_STATE_DONE_DEFAULT_LIMIT);
    let role_filter = role.clone();
    let mut roles = await_blocking("role_state", move || {
        // RI-2-ALLOW: inside the await_blocking closure, off the async runtime.
        let store = WeftStore::open(path).map_err(|e| format!("open role store: {e}"))?;
        crate::weft::role_state::role_state_read(&store, role.as_deref(), now, limit)
            .map_err(|e| e.to_string())
    })
    .await?;
    let configs = host.role_configs_for(team, scope).await?;
    apply_role_configs(&mut roles, &configs, role_filter.as_deref());
    for queue in &mut roles {
        for reg in &mut queue.registrations {
            reg.instance_name = names.session(host, &reg.instance_id);
        }
        for d in &mut queue.deliveries {
            d.from_name = names.session(host, &d.from);
            d.claim_owner_name = d
                .claim_owner
                .as_deref()
                .and_then(|owner| names.session(host, owner));
        }
    }
    Ok(RoleStateView { roles })
}

// ── updates and human status ─────────────────────────────────────────────────────

/// What `list_agent_updates` returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct UpdatesView {
    pub updates: Vec<AgentUpdate>,
}

/// The newest agent updates of `team`, newest first.
pub(crate) async fn updates(
    host: &dyn InboxHost,
    team: &LaunchTeam,
    limit: Option<u32>,
) -> Result<UpdatesView, String> {
    let path = host.team_store(team).await?;
    let limit = limit.unwrap_or(crate::weft::agent_updates::AGENT_UPDATES_LIST_DEFAULT_LIMIT);
    let updates = await_blocking("list_agent_updates", move || {
        // RI-2-ALLOW: inside the await_blocking closure, off the async runtime.
        let store = WeftStore::open(path).map_err(|e| format!("open inbox store: {e}"))?;
        crate::weft::agent_updates::agent_updates_list(&store, limit).map_err(|e| e.to_string())
    })
    .await?;
    Ok(UpdatesView { updates })
}

/// The human's status in `team`.
pub(crate) async fn human_status(
    host: &dyn InboxHost,
    team: &LaunchTeam,
) -> Result<HumanStatusView, String> {
    let path = host.team_store(team).await?;
    await_blocking("get_human_status", move || {
        // RI-2-ALLOW: inside the await_blocking closure, off the async runtime.
        let store = WeftStore::open(path).map_err(|e| format!("open inbox store: {e}"))?;
        crate::weft::human_status::human_status_get(&store).map_err(|e| e.to_string())
    })
    .await
}

// ── the human's send ─────────────────────────────────────────────────────────────

/// The outcome of [`send_as_human`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub(crate) enum HumanSendResult {
    Sent {
        #[serde(rename = "messageId")]
        message_id: i64,
        /// The role delivery, when `to` was a role.
        #[serde(rename = "deliveryId", skip_serializing_if = "Option::is_none")]
        delivery_id: Option<i64>,
    },
    /// An input that cannot be stored; `message` says which.
    Invalid { message: String },
}

/// Where the human's send goes, from how `to` is spelled.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HumanTarget {
    /// `@name`: a session (handle or session id), never a role.
    Session(String),
    /// `#name`: a role queue, never a session.
    Role(String),
    /// A bare name: a session when one answers to it, else a role.
    Either(String),
}

impl HumanTarget {
    fn parse(to: &str) -> Self {
        let to = to.trim();
        // `role:<id>` is the Reply-To spelling of a role queue; it names the role `<id>`.
        let role_name = |r: &str| crate::weft::role_inbox::role_name(r).to_string();
        if let Some(role) = to.strip_prefix('#') {
            HumanTarget::Role(role_name(role))
        } else if let Some(session) = to.strip_prefix('@') {
            let session = session.trim();
            // `@Human` / `@HUMAN` is the human, as the bare spelling is.
            if session.eq_ignore_ascii_case(HUMAN) {
                HumanTarget::Session(HUMAN.to_string())
            } else {
                HumanTarget::Session(session.to_string())
            }
        } else if to.starts_with("role:") {
            HumanTarget::Role(role_name(to))
        } else if to.eq_ignore_ascii_case(HUMAN) {
            // `Human` / `HUMAN` is the human, as the inbox stores it, never a role.
            HumanTarget::Either(HUMAN.to_string())
        } else {
            HumanTarget::Either(to.to_string())
        }
    }
}

/// The human sends `text` to `to`: a session (`@name`, or a bare name some running or
/// launched session answers to; the mail lands in the team it was launched in), or a
/// role (`#name`, or any other bare name, usually a saved Agent's id) in `team`, queued
/// until an instance claims it. Only that recipient gets it. `in_reply_to` is a message
/// of `team`'s inbox the send answers; it is marked read for the human. `reply_to` (a
/// running session, `human` or `role:<id>`) is where replies to it go instead of to the
/// human. `summary` is the optional one-line list preview.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn send_as_human(
    host: &Arc<dyn InboxHost>,
    team: &LaunchTeam,
    to: &str,
    text: &str,
    content_type: Option<MessageContentType>,
    on_click: Option<MessageOnClick>,
    attach_file: Option<String>,
    in_reply_to: Option<i64>,
    reply_to: Option<&str>,
    summary: Option<&str>,
) -> Result<HumanSendResult, String> {
    // Session recipients resolve their own inbox; validate the selected team too.
    let path = host.team_store(team).await?;
    if let Some(id) = in_reply_to {
        let exists = with_store("send_as_human", path, move |store| {
            crate::weft::inbox::inbox_get(store, id)
        })
        .await??
        .is_some();
        if !exists {
            return Ok(HumanSendResult::Invalid {
                message: "Not sent: no such message to reply to.".into(),
            });
        }
    }
    let target = HumanTarget::parse(to);
    let name = match &target {
        HumanTarget::Session(n) | HumanTarget::Role(n) | HumanTarget::Either(n) => n.clone(),
    };
    if name.is_empty() {
        return Ok(HumanSendResult::Invalid {
            message: "Not sent: no recipient.".into(),
        });
    }
    // A role queue named `human` would store a row addressed to the human: the human's
    // own message would come back as mail from itself.
    if matches!(target, HumanTarget::Role(_)) && name.eq_ignore_ascii_case(HUMAN) {
        return Ok(HumanSendResult::Invalid {
            message: "Not sent: human is not a role.".into(),
        });
    }
    let reply_to = match super::agent_inbox::resolve_reply_to(&**host, reply_to).await? {
        Ok(reply_to) => reply_to,
        Err(message) => {
            return Ok(HumanSendResult::Invalid {
                message: format!("Not sent: {message}."),
            })
        }
    };
    let attach_file = attach_file
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let in_reply_to_ref = in_reply_to.map(|id| {
        (
            id,
            InboxReplyTeam {
                org: team.org.clone(),
                team: team.team.clone(),
            },
        )
    });
    let is_session = match &target {
        HumanTarget::Session(_) => true,
        HumanTarget::Role(_) => false,
        HumanTarget::Either(to) => {
            to == HUMAN
                || host.has_handle(to)
                || host
                    .handle_for_session(to)
                    .is_some_and(|h| host.has_handle(&h))
                // A tab someone opened by hand, addressed by its session id.
                || host.sessions().await?.find(to).is_some()
        }
    };
    let result = if is_session {
        match send_message_replying(
            host,
            None.into(),
            &name,
            text,
            attach_file.as_deref(),
            on_click,
            content_type,
            in_reply_to_ref,
            reply_to.as_deref(),
            summary,
            // The human's own send asks nothing of the human: it carries no kind.
            None,
            None,
            None,
        )
        .await?
        {
            SendResult::Sent { message_id, .. } => HumanSendResult::Sent {
                message_id,
                delivery_id: None,
            },
            SendResult::Invalid { message } => HumanSendResult::Invalid { message },
        }
    } else {
        send_role_as_human(
            host,
            team,
            name,
            text,
            content_type,
            on_click,
            attach_file,
            in_reply_to_ref,
            reply_to,
            summary,
        )
        .await?
    };
    if let (Some(id), HumanSendResult::Sent { .. }) = (in_reply_to, &result) {
        mark_answered(host, team, id).await;
    }
    Ok(result)
}

/// Queue the human's `text` for `role` in `team`, and tell the watchers of role queues.
#[allow(clippy::too_many_arguments)]
async fn send_role_as_human(
    host: &Arc<dyn InboxHost>,
    team: &LaunchTeam,
    role: String,
    text: &str,
    content_type: Option<MessageContentType>,
    on_click: Option<MessageOnClick>,
    attach_file: Option<String>,
    in_reply_to: Option<(i64, InboxReplyTeam)>,
    reply_to: Option<String>,
    summary: Option<&str>,
) -> Result<HumanSendResult, String> {
    if text.trim().is_empty() {
        return Ok(HumanSendResult::Invalid {
            message: "Not sent: the text is blank.".into(),
        });
    }
    let path = host.team_store(team).await?;
    let queued_role = role.clone();
    let text = text.to_string();
    let summary = summary.map(str::to_string);
    let now = chrono::Utc::now().timestamp();
    let sent = await_blocking("send_as_human", move || {
        use crate::weft::role_inbox::RoleInboxError;
        use crate::weft::sqlite::WvWriteError;
        // RI-2-ALLOW: inside the await_blocking closure, off the async runtime.
        let store = WeftStore::open(path).map_err(|e| format!("open role store: {e}"))?;
        match crate::weft::role_inbox::send_role_message_commit_with_reply_to(
            &store,
            HUMAN,
            &role,
            &text,
            in_reply_to.as_ref().map(|(id, _)| *id),
            in_reply_to.as_ref().map(|(_, team)| team),
            None,
            "",
            now,
            attach_file.as_deref(),
            on_click.as_ref(),
            content_type,
            reply_to.as_deref(),
            summary.as_deref(),
        ) {
            Ok(ids) => Ok(Ok(ids)),
            Err(RoleInboxError::Store(WvWriteError::Invalid(reason))) => Ok(Err(reason.0)),
            Err(e) => Err(e.to_string()),
        }
    })
    .await?;
    match sent {
        Ok((message_id, delivery_id)) => {
            host.inbox_event(InboxEvent::new(InboxChange::Stored, team, [message_id]));
            host.roles_changed();
            host.role_message_queued(team, &queued_role, message_id, crate::weft::inbox::HUMAN);
            Ok(HumanSendResult::Sent {
                message_id,
                delivery_id: Some(delivery_id),
            })
        }
        Err(reason) => Ok(HumanSendResult::Invalid {
            message: format!("Not sent: {reason}."),
        }),
    }
}

/// Mark message `message_id` of `team`'s inbox read for the human once it is answered.
/// Only a message addressed to the human changes; a failure is logged, not reported,
/// since the answer already went out.
async fn mark_answered(host: &Arc<dyn InboxHost>, team: &LaunchTeam, message_id: i64) {
    let Ok(location) = team_inbox(&**host, team.clone()).await else {
        return;
    };
    let marked = with_store("send_as_human", location.path, move |store| {
        crate::weft::inbox::inbox_mark_read(store, message_id, HUMAN)
    })
    .await;
    match marked {
        Ok(Ok(true)) => host.inbox_event(InboxEvent::new(InboxChange::Read, team, [message_id])),
        Ok(Ok(false)) => {}
        Ok(Err(e)) | Err(e) => {
            log::warn!(target: "inbox", "answered message {message_id} not marked read: {e}")
        }
    }
}

// ── one session's conversation, and the rollup ───────────────────────────────────

/// The team whose inbox holds a [`ConversationMessage`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ConversationTeam {
    pub org: String,
    pub team: String,
}

/// Where a role-addressed (or withdrawn) message stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ConversationDeliveryState {
    /// Queued: no instance has claimed it.
    Pending,
    Claimed,
    Accepted,
    Acknowledged,
    Withdrawn,
}

impl From<InboxDeliveryState> for ConversationDeliveryState {
    fn from(state: InboxDeliveryState) -> Self {
        match state {
            InboxDeliveryState::Queued => Self::Pending,
            InboxDeliveryState::Reading => Self::Claimed,
            InboxDeliveryState::Accepted => Self::Accepted,
            InboxDeliveryState::Done => Self::Acknowledged,
            InboxDeliveryState::Withdrawn => Self::Withdrawn,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConversationDelivery {
    pub state: ConversationDeliveryState,
    /// The instance holding or having accepted it; `None` while pending.
    pub claimed_by: Option<String>,
}

/// One message as a session's conversation and the rollup show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConversationMessage {
    pub conversation: String,
    pub id: i64,
    pub team: ConversationTeam,
    /// A session handle, or `human`.
    pub from: String,
    pub from_name: String,
    /// A session handle or `human`; `None` when addressed to a role queue.
    pub to: Option<String>,
    pub to_name: Option<String>,
    /// The role queue it was addressed to.
    pub to_role: Option<String>,
    pub text: String,
    pub content_type: Option<MessageContentType>,
    pub created_at_ms: i64,
    /// The human archived this message; conversation history remains available.
    pub archived_at_ms: Option<i64>,
    pub in_reply_to: Option<i64>,
    /// `terminal` when the text was typed into the recipient's terminal.
    pub via: Option<String>,
    /// Where replies to it go instead of its sender (email Reply-To): a session handle,
    /// `human`, or `role:<id>`. `None`: replies go to the sender.
    pub reply_to: Option<String>,
    /// The sender's one-line plain-text list preview, when it set one.
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_file: Option<String>,
    pub attachments: Vec<crate::media::Attachment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_click: Option<MessageOnClick>,
    /// Unread for its recipient: a session or the human has not read it; a role
    /// message no instance has claimed yet.
    pub unread: bool,
    pub delivery: Option<ConversationDelivery>,
    /// SW-AGR-18: what it asks of the human: `needs-you` or `update` (the effective
    /// kind, see [`MessageKind::effective`]).
    pub kind: MessageKind,
    /// The emoji reactions on it, grouped by emoji with their distinct reactors.
    pub reactions: Vec<crate::weft::reactions::ReactionGroup>,
}

/// `row`'s reactions among `all` (one store's reactions).
fn attach_reactions(
    mut row: ConversationMessage,
    all: &[crate::weft::reactions::Reaction],
) -> ConversationMessage {
    let on: Vec<_> = all.iter().filter(|r| r.message_id == row.id).collect();
    row.reactions = crate::weft::reactions::group_reactions(&on);
    row
}

/// `list_session_inbox`: everything one session sent and received, and what waits in
/// the role queues it holds, across every team synced here, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionInboxDto {
    pub handle: String,
    pub roles: Vec<String>,
    pub messages: Vec<ConversationMessage>,
}

/// One role queue in the rollup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RollupRole {
    pub role: String,
    /// The queue's team, or none when this summary combines several teams.
    pub team: Option<LaunchTeam>,
    pub holder_count: usize,
    /// The session holding it now, by handle; `None` with no live holder.
    pub holder: Option<String>,
    /// Messages queued for it that no instance has claimed.
    pub pending: u32,
    pub note: String,
}

/// `list_inbox_rollup`: every team's messages synced here, oldest first, and the role
/// queues.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RollupDto {
    pub conversations: Vec<ConversationSummary>,
    pub messages: Vec<ConversationMessage>,
    pub roles: Vec<RollupRole>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub human_unread: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConversationSummary {
    pub with: String,
    pub team: LaunchTeam,
    pub unread: u32,
    pub message_count: u32,
    pub last_at: i64,
    pub last_message_id: i64,
    pub last_preview: String,
}

pub(crate) fn conversation_key(
    message: &InboxMessage,
    messages: &std::collections::BTreeMap<i64, InboxMessage>,
) -> String {
    let mut root = message;
    let mut path = Vec::new();
    loop {
        if let Some(start) = path.iter().position(|id| *id == root.id) {
            // All entries into a cycle choose the same representative.
            root = &messages[path[start..].iter().min().unwrap()];
            break;
        }
        path.push(root.id);
        let Some(parent) = root.in_reply_to.and_then(|id| messages.get(&id)) else {
            break;
        };
        root = parent;
    }
    if root.from != HUMAN {
        root.from.clone()
    } else if let Some(role) = &root.role {
        format!("#{role}")
    } else if let Some(role) = root.to.strip_prefix("role:") {
        format!("#{role}")
    } else {
        root.to.clone()
    }
}

pub(crate) fn conversation_summaries(
    team: &LaunchTeam,
    messages: &std::collections::BTreeMap<i64, InboxMessage>,
) -> Vec<ConversationSummary> {
    let mut summaries = std::collections::BTreeMap::<String, ConversationSummary>::new();
    for m in messages
        .values()
        .filter(|m| m.archived_at_ms.is_none() && (m.to == HUMAN || m.from == HUMAN))
    {
        let key = conversation_key(m, messages);
        let summary = summaries
            .entry(key.clone())
            .or_insert_with(|| ConversationSummary {
                with: key,
                team: team.clone(),
                unread: 0,
                message_count: 0,
                last_at: i64::MIN,
                last_message_id: 0,
                last_preview: String::new(),
            });
        summary.message_count += 1;
        if m.to == HUMAN && m.from != HUMAN && m.read_at_ms.is_none() && m.withdrawn_at_ms.is_none()
        {
            summary.unread += 1;
        }
        if (m.created_at_ms, m.id) >= (summary.last_at, summary.last_message_id) {
            summary.last_at = m.created_at_ms;
            summary.last_message_id = m.id;
            let media_preview = m
                .attachments
                .first()
                .filter(|_| m.text.trim().is_empty())
                .map(|a| {
                    let kind = if matches!(a.mime.as_deref(), Some("image/png" | "image/jpeg")) {
                        "Image"
                    } else {
                        "GIF"
                    };
                    format!("{kind} · {}", a.alt)
                });
            summary.last_preview = m
                .summary
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(
                    if m.content_type == Some(MessageContentType::Html) && !m.text.trim().is_empty()
                    {
                        "HTML message"
                    } else {
                        media_preview.as_deref().unwrap_or(&m.text)
                    },
                )
                .chars()
                .take(120)
                .collect();
        }
    }
    summaries.into_values().collect()
}

pub(crate) fn conversation_message(
    host: &dyn InboxHost,
    sessions: &KnownSessions,
    holders: &crate::weft::inbox::RoleHolders,
    team: &LaunchTeam,
    m: InboxMessage,
) -> ConversationMessage {
    conversation_message_from_row(
        host,
        sessions,
        team,
        super::agent_inbox::inbox_row(host, sessions, holders, m),
    )
}

fn conversation_message_from_row(
    host: &dyn InboxHost,
    sessions: &KnownSessions,
    team: &LaunchTeam,
    m: super::agent_inbox::InboxRow,
) -> ConversationMessage {
    let role = m.to_role;
    ConversationMessage {
        conversation: m.conversation,
        id: m.message_id,
        team: ConversationTeam {
            org: team.org.clone(),
            team: team.team.clone(),
        },
        from: m.from,
        from_name: m.from_name,
        to: role.is_none().then_some(m.to),
        to_name: role.is_none().then_some(m.to_name),
        to_role: role,
        text: m.text,
        content_type: m.content_type,
        created_at_ms: m.created_at_ms,
        archived_at_ms: m.archived_at_ms,
        in_reply_to: m.in_reply_to,
        via: m.via,
        reply_to: m.reply_to,
        summary: m.summary,
        primary_file: m.primary_file,
        attachments: {
            let mut a = m.attachments;
            host.present_media(team, &mut a);
            a
        },
        on_click: m.on_click,
        unread: m
            .delivery
            .as_ref()
            .map_or(m.unread, |d| d.state == InboxDeliveryState::Queued),
        delivery: m.delivery.map(|d| ConversationDelivery {
            state: d.state.into(),
            claimed_by: d
                .instance_session
                .as_deref()
                .or(d.instance.as_deref())
                .map(|id| instance_handle(host, sessions, id)),
        }),
        kind: m.kind,
        reactions: m.reactions,
    }
}

/// The handle of the session a role instance id names (a known session, else the
/// host's handle for a tab session), else the id itself.
fn instance_handle(host: &dyn InboxHost, sessions: &KnownSessions, id: &str) -> String {
    sessions
        .find(id)
        .map(|s| s.handle.clone())
        .or_else(|| host.handle_for_session(id))
        .unwrap_or_else(|| id.to_string())
}

/// Oldest first across stores; ids break ties within one store.
fn oldest_first(messages: &mut [ConversationMessage]) {
    messages.sort_by(|a, b| {
        (a.created_at_ms, &a.team.org, &a.team.team, a.id).cmp(&(
            b.created_at_ms,
            &b.team.org,
            &b.team.team,
            b.id,
        ))
    });
}

/// Every synced team's inbox, each store once (two teams resolving to one store list
/// its rows once). Include unwritten paths only to read their queue configuration,
/// leaving the stores unopened. A team whose store cannot be resolved is skipped (one team never
/// fails the whole read). A session's own launch/tab team is included even when the
/// synced list omits it. If that list fails, fall back to the active and session teams.
pub(crate) async fn team_stores(
    host: &dyn InboxHost,
    also: Option<LaunchTeam>,
    include_unwritten: bool,
) -> Result<Vec<(LaunchTeam, std::path::PathBuf)>, String> {
    let mut teams = match host.synced_teams().await {
        Ok(teams) => teams,
        Err(synced) => {
            log::warn!(target: "inbox", "synced teams unreadable, reading active/session teams: {synced}");
            match host.active_team().await {
                Ok(active) => vec![active],
                Err(_) if also.is_some() => Vec::new(),
                Err(active) => return Err(format!(
                    "no inbox to read: synced teams unreadable ({synced}); active team unreadable ({active})"
                )),
            }
        }
    };
    if let Some(team) = also {
        if !teams.contains(&team) {
            teams.push(team);
        }
    }
    let mut out: Vec<(LaunchTeam, std::path::PathBuf)> = Vec::new();
    for team in teams {
        match host.team_store(&team).await {
            Ok(path) if out.iter().any(|(_, seen)| *seen == path) => {}
            Ok(path) if path.exists() || include_unwritten => out.push((team, path)),
            Ok(_) => {}
            Err(e) => log::debug!(target: "inbox", "no inbox for {}:{}: {e}", team.org, team.team),
        }
    }
    Ok(out)
}

/// The per-team store reads of a cross-team view: one failing team is skipped, but
/// when every store read fails the view fails with the last error rather than showing
/// an empty inbox.
#[derive(Default)]
struct StoreReads {
    attempted: u32,
    succeeded: u32,
    last_error: String,
}

impl StoreReads {
    /// The value of one team's read, or `None` (recording the error) when it failed.
    fn take<T>(&mut self, read: Result<Result<T, String>, String>) -> Option<T> {
        self.attempted += 1;
        match read {
            Ok(Ok(found)) => {
                self.succeeded += 1;
                Some(found)
            }
            Ok(Err(e)) | Err(e) => {
                self.last_error = e;
                None
            }
        }
    }

    fn all_failed(&self) -> Result<(), String> {
        if self.attempted > 0 && self.succeeded == 0 {
            return Err(self.last_error.clone());
        }
        Ok(())
    }
}

/// [`SessionInboxDto`] for `handle` (a handle or a tab session id). A view: it marks
/// nothing read and claims nothing. `human` is not a session: its mail is the human's
/// inbox, not one session's conversation.
pub(crate) async fn session_inbox(
    host: &dyn InboxHost,
    handle: &str,
) -> Result<SessionInboxDto, String> {
    let asked = handle.trim().to_string();
    if asked.is_empty() {
        return Err("no session named: pass a session handle or tab session id".into());
    }
    if asked.eq_ignore_ascii_case(HUMAN) {
        return Err("human is not a session: pass a session handle or tab session id".into());
    }
    let sessions = host.sessions().await?;
    let known = sessions.find(&asked).cloned();
    let handle = super::agent_inbox::session_handle(host, &asked, Some(&sessions));
    // Mail and registrations may name the session by its handle or by its tab session.
    let mut addresses = vec![handle.clone()];
    for alias in [Some(asked.clone()), known.and_then(|s| s.session)]
        .into_iter()
        .flatten()
    {
        if !alias.is_empty() && !addresses.contains(&alias) {
            addresses.push(alias);
        }
    }
    let now = chrono::Utc::now().timestamp();
    let mut roles: Vec<String> = Vec::new();
    let mut messages = Vec::new();
    let mut reads = StoreReads::default();
    for (team, path) in team_stores(
        host,
        crate::commands::agent_inbox::handle_team(host, &handle).await,
        false,
    )
    .await?
    {
        let addresses = addresses.clone();
        let read = with_store("list_session_inbox", path, move |store| {
            let held = crate::weft::role_inbox::roles_held_by(store, &addresses, now)?;
            let rows = crate::weft::inbox::inbox_list_for_session(store, &addresses, &held)?;
            Ok((
                held,
                rows,
                crate::weft::inbox::role_holders(store)?,
                crate::weft::reactions::reactions_all(store)?,
            ))
        })
        .await;
        let Some((held, rows, holders, reactions)) = reads.take(read) else {
            log::warn!(target: "inbox", "session inbox of team {}:{} failed: {}", team.org, team.team, reads.last_error);
            continue;
        };
        for role in held {
            if !roles.contains(&role) {
                roles.push(role);
            }
        }
        messages.extend(rows.into_iter().map(|m| {
            attach_reactions(
                conversation_message(host, &sessions, &holders, &team, m),
                &reactions,
            )
        }));
    }
    reads.all_failed()?;
    roles.sort();
    oldest_first(&mut messages);
    Ok(SessionInboxDto {
        handle,
        roles,
        messages,
    })
}

/// [`RollupDto`]: every synced team's newest messages and its role queues. A view.
pub(crate) async fn inbox_rollup(host: &dyn InboxHost) -> Result<RollupDto, String> {
    inbox_rollup_for(host, &Default::default()).await
}

pub(crate) async fn inbox_rollup_for(
    host: &dyn InboxHost,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<RollupDto, String> {
    let sessions = host.sessions().await?;
    let now = chrono::Utc::now().timestamp();
    let mut messages = Vec::new();
    // Equal role ids retain a combined count; mixed-team identity stays unspecified.
    let mut queues: Vec<RollupRole> = Vec::new();
    let mut conversations = Vec::new();
    let mut reads = StoreReads::default();
    for (team, path) in team_stores(host, None, true).await? {
        let projected = if path.exists() {
            host.projected_inbox(&team).transpose()?
        } else {
            None
        };
        let cached = projected.is_some();
        let (rows, mut roles, holders, reactions) = if path.exists() {
            let read = with_store("list_inbox_rollup", path, move |store| {
                let mut rows = Vec::new();
                if !cached {
                    let mut before = None;
                    loop {
                        let page = crate::weft::inbox::inbox_page(store, before, 200)?;
                        let done = page.len() < 200;
                        before = page.last().map(|m| m.id);
                        rows.extend(page);
                        if done {
                            break;
                        }
                    }
                }
                let roles = crate::weft::role_state::role_state_read(store, None, now, 0)?;
                let holders = crate::weft::inbox::role_holders(store)?;
                let reactions = if cached {
                    Vec::new()
                } else {
                    crate::weft::reactions::reactions_all(store)?
                };
                Ok((rows, roles, holders, reactions))
            })
            .await;
            let Some((rows, roles, holders, reactions)) = reads.take(read) else {
                log::warn!(target: "inbox", "rollup of team {}:{} failed: {}", team.org, team.team, reads.last_error);
                continue;
            };
            (rows, roles, holders, reactions)
        } else {
            (
                Vec::new(),
                Vec::new(),
                crate::weft::inbox::RoleHolders::default(),
                Vec::new(),
            )
        };
        match host.role_configs_for(&team, scope).await {
            Ok(configs) => apply_role_configs(&mut roles, &configs, None),
            Err(error) => {
                log::warn!(target: "inbox", "rollup roles of team {}:{} failed: {error}", team.org, team.team)
            }
        }
        let all: std::collections::BTreeMap<_, _> =
            rows.iter().cloned().map(|m| (m.id, m)).collect();
        conversations.extend(
            projected
                .as_ref()
                .map(|v| v.conversations.clone())
                .unwrap_or_else(|| conversation_summaries(&team, &all)),
        );
        if let Some(view) = projected {
            // The full cached rows retain keys even when their roots are outside the payload.
            for row in view.messages.into_iter().take(500) {
                messages.push(conversation_message_from_row(host, &sessions, &team, row));
            }
        } else {
            messages.extend(rows.into_iter().take(500).map(|m| {
                let key = conversation_key(&m, &all);
                let mut row = attach_reactions(
                    conversation_message(host, &sessions, &holders, &team, m),
                    &reactions,
                );
                row.conversation = key;
                row
            }));
        }
        for queue in roles {
            let holder = queue
                .registrations
                .iter()
                .filter(|r| r.live)
                .max_by_key(|r| r.last_renewed_at_ms)
                .map(|r| {
                    instance_handle(
                        host,
                        &sessions,
                        r.session_id.as_deref().unwrap_or(&r.instance_id),
                    )
                });
            let holder_count = queue.registrations.iter().filter(|r| r.live).count();
            match queues.iter_mut().find(|r| r.role == queue.role) {
                Some(existing) => {
                    existing.pending += queue.counts.pending;
                    existing.holder_count += holder_count;
                    if existing.team.as_ref() != Some(&team) {
                        existing.team = None;
                    }
                    if existing.holder.is_none() {
                        existing.holder = holder;
                    }
                }
                None => queues.push(RollupRole {
                    role: queue.role,
                    team: Some(team.clone()),
                    holder_count,
                    holder,
                    pending: queue.counts.pending,
                    note: String::new(),
                }),
            }
        }
    }
    reads.all_failed()?;
    oldest_first(&mut messages);
    queues.sort_by(|a, b| a.role.cmp(&b.role));
    let roles = queues
        .into_iter()
        .map(|mut queue| {
            queue.note = role_note(
                queue
                    .holder
                    .as_deref()
                    .map(|h| address_name(host, &sessions, h)),
                queue.pending,
            );
            queue
        })
        .collect();
    Ok(RollupDto {
        conversations,
        messages,
        roles,
        human_unread: None,
    })
}

/// One line on where a role queue stands, for the rollup's queue list.
fn role_note(holder_name: Option<String>, pending: u32) -> String {
    match (holder_name, pending) {
        (Some(name), 0) => format!("held by {name}"),
        (Some(name), n) => format!("held by {name} · {n} queued until it claims"),
        (None, 0) => "no live holder".to_string(),
        (None, n) => format!("no live holder · {n} queued"),
    }
}

// ── app wiring ───────────────────────────────────────────────────────────────────

/// [`roles`] for the app: session names from the live session list.

pub(crate) async fn app_roles_for(
    app: &tauri::AppHandle,
    team: &LaunchTeam,
    role: Option<String>,
    done_limit: Option<u32>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<RoleStateView, String> {
    let host = AppInboxHost { app: app.clone() };
    let names = Names::from_sessions(&sessions(app).await.unwrap_or_default());
    roles_for(&host, team, role, done_limit, &names, scope).await
}

/// [`inbox`] for the app: an agent filter reads the live sessions for its instances.
pub(crate) async fn app_inbox(
    app: &tauri::AppHandle,
    team: &LaunchTeam,
    query: &InboxQuery,
) -> Result<InboxView, String> {
    app_inbox_matching(app, team, query, &|_| true).await
}

pub(crate) async fn app_inbox_matching(
    app: &tauri::AppHandle,
    team: &LaunchTeam,
    query: &InboxQuery,
    keep: &(dyn Fn(&super::agent_inbox::InboxRow) -> bool + Sync),
) -> Result<InboxView, String> {
    let host = AppInboxHost { app: app.clone() };
    let listed = if query
        .agent_id
        .as_deref()
        .is_some_and(|a| !a.trim().is_empty())
    {
        sessions(app).await.unwrap_or_default()
    } else {
        Vec::new()
    };
    inbox_matching(&host, team, query, &listed, keep).await
}

/// A saved Agent as `agent_get` returns it, with its running instances filled in.
pub(crate) async fn app_agent_for(
    app: &tauri::AppHandle,
    id: String,
    scope: crate::context_repo::caller_scope::CallerScope,
) -> Result<crate::mcp::agent_tools::Reply<crate::mcp::agent_tools::AgentGot>, String> {
    use crate::mcp::agent_tools::{self, AgentGetParams, AgentReply};
    let (home, user_id) = super::route_to_context::resolve_ambient(app).await?;
    let delivery = tauri::Manager::try_state::<crate::delivery::app::DeliveryState>(app)
        .map(|state| state.inner().clone());
    let mut reply = await_blocking("agent_get", move || {
        let principal = crate::domain::Principal::from(user_id);
        let mut reply = agent_tools::get_for(&home, &principal, AgentGetParams { id }, &scope);
        if let Ok(AgentReply::Ok(got)) = &mut reply {
            crate::delivery::read_model::attach_queued(got, delivery.as_deref());
        }
        Ok::<_, String>(reply)
    })
    .await?;
    if let Ok(AgentReply::Ok(got)) = &mut reply {
        got.instances = instances_of(&sessions(app).await.unwrap_or_default(), &got.id);
        super::deps::note_cli_version(app, &mut got.resolved);
    }
    Ok(reply)
}

// ── Tauri commands (the page bridge) ─────────────────────────────────────────────

/// Every session, as `list_sessions` lists it (the bridge's `sessions()`).
#[tauri::command]
pub async fn agent_read_sessions(app: tauri::AppHandle) -> Result<Vec<SessionListRow>, String> {
    sessions(&app).await
}

/// A saved Agent with its running instances (the bridge's `agent({key})`). A refusal
/// (`not_found`, `invalid`) is an error naming its code.
#[tauri::command]
pub async fn agent_read_agent(
    app: tauri::AppHandle,
    key: String,
    frame_id: Option<String>,
    page: Option<String>,
) -> Result<crate::mcp::agent_tools::AgentGot, String> {
    use crate::mcp::agent_tools::AgentReply;
    let scope = crate::context_repo::caller_scope::for_management_view(
        &app,
        page.as_deref(),
        frame_id.as_deref(),
    )?;
    match app_agent_for(&app, key, scope).await?? {
        AgentReply::Ok(got) => Ok(got),
        AgentReply::Refused(refusal) => Err(format!(
            "{}: {}",
            serde_json::to_value(refusal.code)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| "refused".into()),
            refusal.message
        )),
    }
}

/// SW-XS-14: dismiss the one-time `projections.yml` context suggestion that
/// `agent_read_agent` (and `agent_get`) returned for agent `key`, by its
/// `fingerprint`. Records the dismissal for this user only; the agent is not
/// touched. A changed suggestion (a new fingerprint) shows again.
#[tauri::command]
pub async fn agent_dismiss_context_suggestion(
    app: tauri::AppHandle,
    key: String,
    fingerprint: String,
) -> Result<(), String> {
    let (home, user_id) = super::route_to_context::resolve_ambient(&app).await?;
    await_blocking("agent_dismiss_context_suggestion", move || {
        let key = crate::agents::AgentKey::parse(&key)
            .ok_or_else(|| format!("invalid agent key {key:?}"))?;
        crate::agents::context_suggestion::dismiss(
            &home,
            &crate::domain::Principal::from(user_id),
            &key,
            &fingerprint,
        )
    })
    .await
}

/// `team`'s inbox as `query` asks (the bridge's `inbox(...)`). Marks nothing read.
#[tauri::command]
pub async fn agent_read_inbox(
    app: tauri::AppHandle,
    team: InboxTeam,
    query: Option<InboxQuery>,
) -> Result<InboxView, String> {
    app_inbox(&app, &team.into(), &query.unwrap_or_default()).await
}

/// `team`'s role mailboxes (the bridge's `roles(...)`).
#[tauri::command]
pub async fn agent_read_roles(
    app: tauri::AppHandle,
    team: InboxTeam,
    role: Option<String>,
    limit: Option<u32>,
    frame_id: Option<String>,
    page: Option<String>,
) -> Result<RoleStateView, String> {
    let scope = crate::context_repo::caller_scope::for_management_view(
        &app,
        page.as_deref(),
        frame_id.as_deref(),
    )?;
    app_roles_for(&app, &team.into(), role, limit, &scope).await
}

/// The human sends a message from a dashboard or the inbox (the bridge's
/// `sendAsHuman`): `to` is `@session`, `#role`, or a bare name; `in_reply_to` is the
/// message of `team`'s inbox it answers.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn send_inbox_as_human(
    app: tauri::AppHandle,
    team: InboxTeam,
    to: String,
    text: String,
    content_type: Option<MessageContentType>,
    on_click: Option<MessageOnClick>,
    attach_file: Option<String>,
    in_reply_to: Option<i64>,
    reply_to: Option<String>,
    summary: Option<String>,
) -> Result<HumanSendResult, String> {
    let host = AppInboxHost::port(app.clone());
    let result = send_as_human(
        &host,
        &team.into(),
        &to,
        &text,
        content_type,
        on_click,
        attach_file,
        in_reply_to,
        reply_to.as_deref(),
        summary.as_deref(),
    )
    .await?;
    if matches!(
        result,
        HumanSendResult::Sent {
            delivery_id: Some(_),
            ..
        }
    ) {
        agent_state_changed(&app);
    }
    Ok(result)
}

/// One session's conversation across every synced team (the inbox rail of its tab):
/// what it sent, what it received, and what waits in the role queues it holds. Works
/// for any session, a hand-started tab included. Marks nothing read, claims nothing.
#[tauri::command]
pub async fn list_session_inbox(
    app: tauri::AppHandle,
    handle: String,
) -> Result<SessionInboxDto, String> {
    session_inbox(&AppInboxHost { app }, &handle).await
}

/// Every synced team's messages and role queues (the full-screen inbox rollup).
#[tauri::command]
pub async fn list_inbox_rollup(
    app: tauri::AppHandle,
    frame_id: Option<String>,
    page: Option<String>,
) -> Result<RollupDto, String> {
    use tauri::Manager;
    let scope = crate::context_repo::caller_scope::for_management_view(
        &app,
        page.as_deref(),
        frame_id.as_deref(),
    )?;
    let mut rollup = inbox_rollup_for(&AppInboxHost { app: app.clone() }, &scope).await?;
    rollup.human_unread = Some(crate::live_hub::LiveHub::human_unread_total(
        &app.state::<crate::live_hub::LiveHub>(),
    ));
    Ok(rollup)
}

#[cfg(test)]
mod tests;
