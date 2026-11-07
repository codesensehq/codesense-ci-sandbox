// SPDX-License-Identifier: MIT
//! SW-DP-26: the hosted inbox uses the app's read model, with a fixed team and
//! an explicit projection. Message text never enters diagnostic errors or logs.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde_json::{json, Value};

use super::agents::{self, Team};
use super::catalog::{self, Call, LiveCtx, QueryError};
use crate::commands::agent_inbox::{InboxHost, InboxScope};
use crate::commands::agent_read_model::{self, InboxQuery};
use crate::commands::dashboard_launch::LaunchTeam;

pub(crate) mod faces;
pub(crate) mod media;

/// The declaration hook in the shared action schema; execution stays here.
pub(crate) fn valid_declaration(a: &super::actions::Action) -> bool {
    !a.to.is_empty()
        && a.to.len() <= 32
        && a.to.iter().all(|to| recipient_syntax(to))
        && a.fields.is_empty()
        && a.choices.is_empty()
        && a.match_fields.is_empty()
        && a.agent.is_none()
        && a.prompt.is_none()
        && a.mode.is_none()
        && a.routine.is_none()
        && a.vars.is_empty()
}

// Serialize inbox reservations across dashboards. The shared log retains pending
// reservations, so interrupted sends cannot reopen the owner's hourly allowance.
static SEND_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) async fn dispatch(
    ctx: &LiveCtx,
    folder: &std::path::Path,
    block: &super::state::LiveApproved,
    data_dir: &std::path::Path,
    frame: &Value,
    line: &mut Value,
) -> Result<Value, &'static str> {
    let _turn = SEND_LOCK.lock().await;
    line["kind"] = json!("inbox");
    let mut args = MessageArgs::parse(frame["args"].clone())?;
    let dir = data_dir.to_path_buf();
    let rows = crate::commands::await_blocking("dashboard.inbox.history", move || {
        super::actions::read_all_history(&dir)
    })
    .await
    .map_err(|_| "failed")?;
    if let Some(action) = frame["action"].as_str().and_then(|n| block.actions.get(n)) {
        if !action.to.contains(&args.to) {
            return Err("invalid_args");
        }
        args.allowed_recipients = Some(action.to.clone());
        let slug_rows: Vec<_> = rows
            .iter()
            .filter(|r| r["slug"] == frame["dashboard"])
            .cloned()
            .collect();
        super::actions::cap(
            action,
            frame["action"].as_str().unwrap_or(""),
            &slug_rows,
            &[],
            chrono::Utc::now(),
        )?;
    }
    let owner_rows: Vec<_> = rows
        .into_iter()
        .filter(|r| r["by"]["account"] == frame["by"]["account"])
        .collect();
    cap(&owner_rows, chrono::Utc::now())?;
    let dir = data_dir.to_path_buf();
    let reservation = line.clone();
    crate::commands::await_blocking("dashboard.inbox.reserve", move || {
        super::live::save_action_log(&dir, &reservation)
    })
    .await
    .map_err(|_| "failed")?;
    let result = deliver(ctx, folder, block.scope.as_ref(), args).await?;
    for (key, source) in [
        ("to", "to"),
        ("length", "length"),
        ("message_id", "messageId"),
    ] {
        line[key] = result[source].clone();
    }
    Ok(result)
}

/// The hosted inbox's args: the read model's, plus `scope: "human"` (messages to or from
/// the human), which is filtered here, on the Mac, because the read model has no such scope.
#[derive(Clone, Debug)]
pub(crate) struct HostedQuery {
    query: InboxQuery,
    human: bool,
}

pub(crate) fn args(mut value: Value) -> Result<HostedQuery, QueryError> {
    let human = value["scope"] == "human";
    if human {
        if let Some(map) = value.as_object_mut() {
            map.remove("scope");
        }
    }
    let mut query: InboxQuery =
        catalog::strict(value, catalog::query_args("inbox"))?;
    // Hosted inboxes show all recipient classes by default; args only narrow.
    query.scope.get_or_insert(InboxScope::All);
    query.limit = Some(
        query
            .limit
            .unwrap_or(catalog::MAX_ROWS as u32)
            .clamp(1, catalog::MAX_ROWS as u32),
    );
    Ok(HostedQuery { query, human })
}

pub(crate) fn query(ctx: LiveCtx, call: Call) -> BoxFuture<'static, Result<Value, QueryError>> {
    Box::pin(async move {
        let query = args(call.args)?;
        let handle = tokio::runtime::Handle::current();
        // Keep K20 charged until the read model's blocking work finishes, even
        // if the caller times out and drops this future (as for sessions).
        catalog::blocking(call.hold, "live.inbox", move || {
            Ok(handle.block_on(async move {
                let agents = agents::host_of(&ctx)?;
                let team = agents::scoped_team(&agents, &call.folder, call.scope.as_ref()).await?;
                read(&ctx, &team, &query, &call.fields).await
            }))
        })
        .await
        .map_err(|_| failed())?
    })
}

pub(crate) async fn read(
    ctx: &LiveCtx,
    team: &Team,
    hosted: &HostedQuery,
    fields: &[String],
) -> Result<Value, QueryError> {
    let HostedQuery { query, human } = hosted;
    let is_human = |row: &crate::commands::agent_inbox::InboxRow| {
        !*human || row.from == crate::weft::inbox::HUMAN || row.to == crate::weft::inbox::HUMAN
    };
    let agent_team = team.clone();
    let team = LaunchTeam {
        org: team.org.clone(),
        team: team.team.clone(),
    };
    let view = if let Some(host) = &ctx.inbox {
        let sessions = agents::host_of(ctx)?
            .sessions()
            .await
            .map_err(|_| failed())?;
        agent_read_model::inbox_matching(host.as_ref(), &team, query, &sessions, &is_human).await
    } else {
        let app = ctx.app.as_ref().ok_or_else(failed)?;
        agent_read_model::app_inbox_matching(app, &team, query, &is_human).await
    }
    .map_err(|_| failed())?;
    let matched = view.matched;
    let has = |name: &str| fields.iter().any(|f| f == name);
    let (include_text, include_meta, include_media, include_reactions) =
        (has("text"), has("meta"), has("media"), has("reactions"));
    // Faces only for senders of the rows that go out: no roster, nobody else's.
    let sender_faces = if has("faces") {
        let agents = agents::host_of(ctx)?;
        let sessions = agents.sessions().await.map_err(|_| failed())?;
        let senders: Vec<(String, String)> = view.messages.iter().take(catalog::MAX_ROWS).map(|r| (r.from.clone(), r.from_name.clone())).collect();
        crate::commands::await_blocking("live.inbox.faces", move || {
            Ok(faces::Faces::new(agents.as_ref(), &agent_team, &sessions).faces_for_rows(senders.iter().map(|(from, name)| (from.as_str(), name.as_str()))))
        })
        .await
        .map_err(|_| failed())?
    } else {
        Default::default()
    };
    let messages: Vec<Value> = view.messages.into_iter().take(catalog::MAX_ROWS).map(|row| {
        let mut out = json!({
            "messageId": row.message_id, "from": row.from, "fromName": row.from_name,
            "summary": row.summary, "kind": row.kind, "createdAt": row.created_at_ms,
            "unread": row.unread, "inReplyTo": row.in_reply_to.filter(|_| row.in_reply_to_team.as_ref().is_none_or(|origin| origin.org == team.org && origin.team == team.team)),
        });
        // Only the delivery state is public; notes and instance details can contain
        // free text or identities beyond the declared projection.
        if let Some(delivery) = row.delivery {
            out["delivery"] = json!({"state": delivery.state});
        }
        if let Some(face) = sender_faces.get(&row.from).and_then(Option::as_ref) {
            out["sender"] = json!({"face": face.projected()});
        }
        if include_text { out["text"] = Value::String(row.text); }
        if include_meta {
            out["to"] = json!(row.to);
            out["toName"] = json!(row.to_name);
            out["contentType"] = json!(row.content_type);
            if let Some(reply_to) = row.reply_to { out["replyTo"] = json!(reply_to); }
            if let Some(name) = row.reply_to_name { out["replyToName"] = json!(name); }
        }
        // Image attachments by hash and size only: never the source URL, host or failure reason.
        if include_media {
            out["attachments"] = row.attachments.iter().map(media::projected).collect();
        }
        // Counts and the human's own mark only: the reactors' names never leave the Mac.
        if include_reactions {
            out["reactions"] = row.reactions.iter().map(|g| json!({
                "emoji": g.emoji, "count": g.count,
                "mine": g.reactors.iter().any(|r| r == crate::weft::inbox::HUMAN),
            })).collect();
        }
        Ok(out)
    }).collect::<Result<_, QueryError>>()?;
    // Keep newest complete rows inside the live answer budget. One individually
    // oversized message still uses the dispatcher's too_large error.
    let mut answer = json!({"messages": [], "humanUnread": view.human_unread});
    catalog::bounded_rows(messages, matched, catalog::MAX_ROWS, &answer)
        .put_in(&mut answer, "messages");
    Ok(answer)
}

/// An action's answer when the dashboard's team could not be worked out: `failed` while the
/// context index is not ready (try again), else `not_found`.
fn team_refusal(error: QueryError) -> &'static str {
    match error {
        QueryError::Failed(_) => "failed",
        _ => "not_found",
    }
}

pub(crate) fn failed() -> QueryError {
    QueryError::Failed("inbox unavailable".into())
}

// The fixture uses the app's InboxHost port and real per-team stores.
pub(crate) type Host = Arc<dyn InboxHost>;

pub const MAX_TEXT_CHARS: usize = 2000;
pub const OWNER_PER_HOUR: u32 = 60;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct MessageArgs {
    pub to: String,
    pub text: String,
    pub reply_to: Option<i64>,
    #[serde(skip)]
    pub allowed_recipients: Option<Vec<String>>,
}

impl MessageArgs {
    pub(crate) fn parse(value: Value) -> Result<Self, &'static str> {
        let args: Self = serde_json::from_value(value).map_err(|_| "invalid_args")?;
        if args.text.trim().is_empty()
            || args.text.chars().count() > MAX_TEXT_CHARS
            || args.text.contains('\0')
            || !recipient_syntax(&args.to)
            || args
                .reply_to
                .is_some_and(|id| id <= 0 || id > 9_007_199_254_740_991)
        {
            return Err("invalid_args");
        }
        Ok(args)
    }
}

pub(crate) fn recipient_syntax(to: &str) -> bool {
    if to == "human" {
        return true;
    }
    if let Some(role) = to.strip_prefix("role:") {
        return role.len() <= 128
            && !role.chars().any(char::is_control)
            && crate::weft::role_inbox::stored_role(role).is_ok();
    }
    !to.is_empty() && to.len() <= agents::MAX_HANDLE && !to.chars().any(char::is_control)
}

/// Shares the dispatcher's durable metadata history; all message action names
/// count toward the owner's combined cap, so aliases cannot bypass K49.
pub(crate) fn cap(rows: &[Value], now: chrono::DateTime<chrono::Utc>) -> Result<(), &'static str> {
    if rows
        .iter()
        .filter(|r| {
            r["kind"] == "inbox"
                && (r["outcome"] == "done" || r["outcome"] == "pending")
                && r["at"]
                    .as_str()
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .is_some_and(|at| at > now - chrono::Duration::hours(1))
        })
        .count()
        >= OWNER_PER_HOUR as usize
    {
        Err("rate_limited")
    } else {
        Ok(())
    }
}

/// Restrict all store and recipient resolution in the existing delivery functions.
/// Even a reply's stored Reply-To cannot escape this dashboard's team.
struct ScopedHost {
    inner: Host,
    team: LaunchTeam,
    known_sessions: Vec<crate::commands::agent_inbox::KnownSession>,
}
#[async_trait::async_trait]
impl InboxHost for ScopedHost {
    async fn team_store(&self, team: &LaunchTeam) -> Result<std::path::PathBuf, String> {
        if team != &self.team {
            return Err("not_found".into());
        }
        self.inner.team_store(team).await
    }
    async fn active_team(&self) -> Result<LaunchTeam, String> {
        Ok(self.team.clone())
    }
    fn launch_team(&self, handle: &str) -> Option<LaunchTeam> {
        self.has_handle(handle).then(|| self.team.clone())
    }
    fn handle_for_session(&self, _: &str) -> Option<String> {
        None
    }
    async fn sessions(&self) -> Result<crate::commands::agent_inbox::KnownSessions, String> {
        Ok(crate::commands::agent_inbox::KnownSessions::new(
            self.known_sessions.clone(),
        ))
    }
    fn has_handle(&self, handle: &str) -> bool {
        self.known_sessions.iter().any(|s| s.handle == handle)
    }
    fn display_name(&self, handle: &str) -> Option<String> {
        self.inner.display_name(handle)
    }
    fn projected_inbox(&self, team: &LaunchTeam) -> Option<Result<crate::commands::agent_inbox::InboxView, String>> {
        self.inner.projected_inbox(team)
    }
    fn inbox_changed(&self) {
        self.inner.inbox_changed();
    }
    fn inbox_event(&self, event: crate::live_hub::inbox_events::InboxEvent) {
        self.inner.inbox_event(event);
    }
    fn mail_stored(&self, handle: &str) {
        self.inner.mail_stored(handle);
    }
    fn role_message_queued(&self, team: &LaunchTeam, role: &str, id: i64, sender: &str) {
        self.inner.role_message_queued(team, role, id, sender);
    }
    fn roles_changed(&self) {
        self.inner.roles_changed();
    }
}

/// Called only by the action dispatcher after its owner, cap and declaration
/// checks. The result contains metadata only, safe for the action audit.
pub(crate) async fn deliver(
    ctx: &LiveCtx,
    folder: &std::path::Path,
    scope: Option<&super::state::Scope>,
    args: MessageArgs,
) -> Result<Value, &'static str> {
    use crate::commands::agent_inbox::{self, AppInboxHost, Caller, KnownSession, SendResult};
    use crate::weft::inbox::MessageContentType;
    let agents = agents::host_of(ctx).map_err(|_| "failed")?;
    let team = agents::scoped_team(&agents, folder, scope)
        .await
        .map_err(team_refusal)?;
    let listed = agents.sessions().await.map_err(|_| "failed")?;
    let sessions: Vec<KnownSession> = listed
        .iter()
        .filter(|row| {
            row.machine_id.is_none()
                && crate::mcp::identity_tools::is_running(row.state)
                && agents.launch_team(&row.handle).as_ref() == Some(&team)
                && agents.resolve(&row.handle).is_some()
        })
        .map(KnownSession::from)
        .collect();
    let valid_target = |to: &str| {
        recipient_syntax(to)
            && (to == "human" || to.starts_with("role:") || sessions.iter().any(|s| s.handle == to))
    };
    if args.reply_to.is_none() && !valid_target(&args.to) {
        return Err("not_found");
    }
    let inner = match &ctx.inbox {
        Some(host) => host.clone(),
        None => Arc::new(AppInboxHost {
            app: ctx.app.clone().ok_or("failed")?,
        }) as Host,
    };
    let launch_team = LaunchTeam {
        org: team.org,
        team: team.team,
    };
    let mut to = args.to;
    if let Some(id) = args.reply_to {
        let path = inner.team_store(&launch_team).await.map_err(|_| "failed")?;
        let original = crate::commands::await_blocking("dashboard inbox reply", move || {
            // RI-2-ALLOW: SQLite work runs on the blocking pool.
            let store = crate::weft::WeftStore::open(path).map_err(|_| "failed".to_string())?;
            crate::weft::inbox::inbox_get(&store, id).map_err(|_| "failed".to_string())
        })
        .await
        .map_err(|_| "failed")?
        .ok_or("not_found")?;
        to = agent_inbox::shown_reply_to(Some(
            original.reply_to.as_ref().unwrap_or(&original.from).clone(),
        ))
        .ok_or("not_found")?;
        if !valid_target(&to) {
            return Err("not_found");
        }
    }
    if args
        .allowed_recipients
        .as_ref()
        .is_some_and(|allowed| !allowed.contains(&to))
    {
        return Err("invalid_args");
    }
    let host: Host = Arc::new(ScopedHost {
        inner,
        team: launch_team.clone(),
        known_sessions: sessions,
    });
    let summary: String = args.text.chars().take(80).collect();
    let id = if let Some(id) = args.reply_to {
        match agent_inbox::reply_with_kind_and_content(
            &host,
            Caller::Named(None),
            id,
            &args.text,
            Some(&summary),
            None,
            Some(MessageContentType::Plain),
        )
        .await
        .map_err(|_| "failed")?
        {
            SendResult::Sent { message_id, .. } => message_id,
            SendResult::Invalid { .. } => return Err("not_found"),
        }
    } else if let Some(role) = to.strip_prefix("role:") {
        // This is the existing MCP role-send commit, including its queue state.
        let path = host.team_store(&launch_team).await.map_err(|_| "failed")?;
        let send = crate::commands::role_inbox::RoleSend {
            sender: "human".into(),
            role: role.into(),
            text: args.text.clone(),
            request_id: None,
            attach_file: None,
            on_click: None,
            content_type: Some(MessageContentType::Plain),
            reply_to: None,
            summary: Some(summary),
        };
        let result = crate::commands::await_blocking("dashboard inbox", move || {
            // RI-2-ALLOW: SQLite work runs on the blocking pool.
            let store = crate::weft::WeftStore::open(path).map_err(|_| "failed".to_string())?;
            crate::commands::role_inbox::commit_role_send(
                &store,
                &send,
                chrono::Utc::now().timestamp(),
            )
        })
        .await
        .map_err(|_| "failed")?;
        let id = result
            .get("messageId")
            .and_then(Value::as_i64)
            .ok_or("failed")?;
        host.inbox_event(crate::live_hub::inbox_events::InboxEvent::new(
            crate::live_hub::inbox_events::InboxChange::Stored,
            &launch_team,
            [id],
        ));
        host.roles_changed();
        host.role_message_queued(&launch_team, role, id, "human");
        id
    } else {
        match agent_inbox::send_message_with_kind(
            &host,
            Caller::Named(None),
            &to,
            &args.text,
            None,
            None,
            Some(MessageContentType::Plain),
            None,
            Some(&summary),
            None,
        )
        .await
        .map_err(|_| "failed")?
        {
            SendResult::Sent { message_id, .. } => message_id,
            SendResult::Invalid { .. } => return Err("not_found"),
        }
    };
    Ok(json!({"status":"sent", "messageId":id, "to":to, "length":args.text.chars().count()}))
}

// ---- SV-INP-28: the owner's reactions (tapbacks) ----

pub const REACT_PER_HOUR: u32 = 120;
/// The quick six the phone page offers; free emoji text is never accepted.
pub const QUICK_REACTIONS: [&str; 6] = ["😂", "\u{2764}\u{FE0F}", "👍", "😮", "🎉", "🔥"];

// Serializes reaction reservations, apart from sends, so a slow send never holds a tapback.
static REACT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct ReactArgs {
    pub message_id: i64,
    pub emoji: String,
    pub remove: Option<bool>,
}

impl ReactArgs {
    pub(crate) fn parse(value: Value) -> Result<Self, &'static str> {
        let args: Self = serde_json::from_value(value).map_err(|_| "invalid_args")?;
        if args.message_id <= 0
            || args.message_id > 9_007_199_254_740_991
            || crate::weft::reactions::validate_emoji(&args.emoji).is_err()
            || !QUICK_REACTIONS.contains(&args.emoji.as_str())
        {
            return Err("invalid_args");
        }
        Ok(args)
    }
}

/// A cap of its own per kind of owner action (K49 covers sends): the owner's done or pending
/// calls of `kind` in the last hour, from the shared durable history.
fn hourly_cap(
    rows: &[Value],
    now: chrono::DateTime<chrono::Utc>,
    kind: &str,
    limit: u32,
) -> Result<(), &'static str> {
    let recent = rows.iter().filter(|r| {
        r["kind"] == kind
            && (r["outcome"] == "done" || r["outcome"] == "pending")
            && r["at"]
                .as_str()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .is_some_and(|at| at > now - chrono::Duration::hours(1))
    });
    if recent.count() >= limit as usize {
        Err("rate_limited")
    } else {
        Ok(())
    }
}

/// Reactions: the owner's done or pending reactions in the last hour.
pub(crate) fn react_cap(
    rows: &[Value],
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), &'static str> {
    hourly_cap(rows, now, "react", REACT_PER_HOUR)
}

/// `ownerReact`, after the dispatcher's owner check: validate, charge the owner's cap, then
/// react as the human in the dashboard's own team. The log line holds ids only.
pub(crate) async fn react_dispatch(
    ctx: &LiveCtx,
    folder: &std::path::Path,
    block: &super::state::LiveApproved,
    data_dir: &std::path::Path,
    frame: &Value,
    line: &mut Value,
) -> Result<Value, &'static str> {
    let _turn = REACT_LOCK.lock().await;
    line["kind"] = json!("react");
    let args = ReactArgs::parse(frame["args"].clone())?;
    let dir = data_dir.to_path_buf();
    let rows = crate::commands::await_blocking("dashboard.react.history", move || {
        super::actions::read_all_history(&dir)
    })
    .await
    .map_err(|_| "failed")?;
    let owner_rows: Vec<_> = rows
        .into_iter()
        .filter(|r| r["by"]["account"] == frame["by"]["account"])
        .collect();
    react_cap(&owner_rows, chrono::Utc::now())?;
    let dir = data_dir.to_path_buf();
    let reservation = line.clone();
    crate::commands::await_blocking("dashboard.react.reserve", move || {
        super::live::save_action_log(&dir, &reservation)
    })
    .await
    .map_err(|_| "failed")?;
    let result = react(ctx, folder, block.scope.as_ref(), args).await?;
    line["message_id"] = result["messageId"].clone();
    Ok(result)
}

async fn react(
    ctx: &LiveCtx,
    folder: &std::path::Path,
    scope: Option<&super::state::Scope>,
    args: ReactArgs,
) -> Result<Value, &'static str> {
    use crate::commands::agent_inbox::{self, AppInboxHost, ReactResult};
    use crate::weft::reactions::ReactOutcome;
    let agents = agents::host_of(ctx).map_err(|_| "failed")?;
    let team = agents::scoped_team(&agents, folder, scope)
        .await
        .map_err(team_refusal)?;
    let host: Host = match &ctx.inbox {
        Some(host) => host.clone(),
        None => Arc::new(AppInboxHost {
            app: ctx.app.clone().ok_or("failed")?,
        }),
    };
    let team = LaunchTeam {
        org: team.org,
        team: team.team,
    };
    // The team's own store holds only this team's mail, so another team's id is not found.
    match agent_inbox::react_from_human(host.as_ref(), &team, args.message_id, &args.emoji, args.remove)
        .await
        .map_err(|_| "failed")?
    {
        ReactResult::Reacted { message_id, emoji, outcome } => Ok(json!({
            "status": "reacted", "messageId": message_id, "emoji": emoji,
            // `on` is the state now; an unchanged explicit add or remove is the state asked for.
            "on": match outcome {
                ReactOutcome::Added => true,
                ReactOutcome::Removed => false,
                ReactOutcome::Unchanged => args.remove != Some(true),
            },
        })),
        ReactResult::Invalid { .. } => Err("not_found"),
    }
}

// ---- SV-INP-46: the owner marks the human's mail read ----

/// At most this many ids in one `ownerMarkRead`.
pub const MARK_READ_MAX_IDS: usize = 200;
/// Calls an hour, on a cap of their own (opening a conversation marks it; sends and
/// reactions keep theirs).
pub const MARK_READ_PER_HOUR: u32 = 600;

static MARK_READ_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct MarkReadArgs {
    pub message_ids: Vec<i64>,
}

impl MarkReadArgs {
    /// 1 to 200 positive safe-integer ids; repeats collapse.
    pub(crate) fn parse(value: Value) -> Result<Self, &'static str> {
        let mut args: Self = serde_json::from_value(value).map_err(|_| "invalid_args")?;
        if args.message_ids.is_empty()
            || args.message_ids.len() > MARK_READ_MAX_IDS
            || args
                .message_ids
                .iter()
                .any(|id| *id <= 0 || *id > 9_007_199_254_740_991)
        {
            return Err("invalid_args");
        }
        args.message_ids.sort_unstable();
        args.message_ids.dedup();
        Ok(args)
    }
}

/// `ownerMarkRead`, after the dispatcher's owner check: validate, charge the owner's cap,
/// then mark the human's mail read in the dashboard's own team through the app's own path.
/// The log line holds a count and ids only.
pub(crate) async fn mark_read_dispatch(
    ctx: &LiveCtx,
    folder: &std::path::Path,
    block: &super::state::LiveApproved,
    data_dir: &std::path::Path,
    frame: &Value,
    line: &mut Value,
) -> Result<Value, &'static str> {
    let _turn = MARK_READ_LOCK.lock().await;
    line["kind"] = json!("mark_read");
    let args = MarkReadArgs::parse(frame["args"].clone())?;
    line["count"] = json!(args.message_ids.len());
    let dir = data_dir.to_path_buf();
    let rows = crate::commands::await_blocking("dashboard.mark_read.history", move || {
        super::actions::read_all_history(&dir)
    })
    .await
    .map_err(|_| "failed")?;
    let owner_rows: Vec<_> = rows
        .into_iter()
        .filter(|r| r["by"]["account"] == frame["by"]["account"])
        .collect();
    hourly_cap(&owner_rows, chrono::Utc::now(), "mark_read", MARK_READ_PER_HOUR)?;
    let dir = data_dir.to_path_buf();
    let reservation = line.clone();
    crate::commands::await_blocking("dashboard.mark_read.reserve", move || {
        super::live::save_action_log(&dir, &reservation)
    })
    .await
    .map_err(|_| "failed")?;
    let result = mark_read(ctx, folder, block.scope.as_ref(), args).await?;
    line["marked"] = result["marked"].clone();
    Ok(result)
}

async fn mark_read(
    ctx: &LiveCtx,
    folder: &std::path::Path,
    scope: Option<&super::state::Scope>,
    args: MarkReadArgs,
) -> Result<Value, &'static str> {
    use crate::commands::agent_inbox::{self, AppInboxHost};
    let agents = agents::host_of(ctx).map_err(|_| "failed")?;
    let team = agents::scoped_team(&agents, folder, scope)
        .await
        .map_err(team_refusal)?;
    let host: Host = match &ctx.inbox {
        Some(host) => host.clone(),
        None => Arc::new(AppInboxHost {
            app: ctx.app.clone().ok_or("failed")?,
        }),
    };
    let team = LaunchTeam {
        org: team.org,
        team: team.team,
    };
    // Every id must be a message to the human in this team's own store; one that is not
    // (another team's, an agent's, a missing one) refuses the whole call, marking nothing.
    if !agent_inbox::all_to_human(host.as_ref(), &team, &args.message_ids)
        .await
        .map_err(|_| "failed")?
    {
        return Err("not_found");
    }
    // The same function the app's Inbox uses, so read state, the badge and the stream
    // events stay in one place. Already-read mail is left alone: marking is idempotent.
    let marked = agent_inbox::mark_read_many_by_human(host.as_ref(), &team, args.message_ids)
        .await
        .map_err(|_| "failed")?;
    Ok(json!({"status": "marked", "marked": marked}))
}

#[cfg(test)]
pub(crate) mod tests;
