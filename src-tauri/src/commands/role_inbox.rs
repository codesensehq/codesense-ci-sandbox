// SPDX-License-Identifier: MIT
//! SW-BSI-2: Tauri commands + helpers that wrap the `weft::role_inbox` role-keyed
//! delivery operations. Each command resolves the team's weft store path via
//! [`AppInboxHost`] and runs the sync weft fn on the blocking pool.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::agent_inbox::{AppInboxHost, InboxHost, InboxTeam};
use super::await_blocking;
use crate::live_hub::inbox_events::{InboxChange, InboxEvent};
use crate::weft::inbox::MessageOnClick;
use crate::weft::role_inbox::{
    self, AcceptKind, AcceptedWork, ClaimedDelivery, RegistrationOutcome, RoleInboxError,
    SendRequestOutcome,
};
use crate::weft::sqlite::{WvWriteError, WvWriteResult};
use crate::weft::WeftStore;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationOutcomeDto {
    pub generation: i64,
    pub expires_at: i64,
    pub pending_count: i64,
    pub accepted_unacked: Vec<AcceptedWork>,
}

impl From<RegistrationOutcome> for RegistrationOutcomeDto {
    fn from(v: RegistrationOutcome) -> Self {
        Self {
            generation: v.generation,
            expires_at: v.expires_at,
            pending_count: v.pending_count,
            accepted_unacked: v.accepted_unacked,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptDisposition {
    pub kind: String,
    #[serde(default)]
    pub note_or_reason: String,
}

/// Open `path` as a `WeftStore` off the async runtime and run `op` against it.
async fn with_store<T, F>(label: &'static str, path: PathBuf, op: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&WeftStore) -> Result<T, RoleInboxError> + Send + 'static,
{
    await_blocking(label, move || {
        // RI-2-ALLOW: inside await_blocking closure, off the async runtime.
        let store = WeftStore::open(path).map_err(|e| format!("open role store: {e}"))?;
        op(&store).map_err(|e| e.to_string())
    })
    .await
}

async fn team_store_path(host: &AppInboxHost, team: InboxTeam) -> Result<PathBuf, String> {
    host.team_store(&team.into()).await
}

/// `register_role` — the caller declares a live instance for `role` (shared, and
/// renewed by the caller: this command names no session).
#[tauri::command]
pub async fn register_role_cmd(
    app: tauri::AppHandle,
    team: InboxTeam,
    role: String,
    instance_id: String,
) -> Result<RegistrationOutcomeDto, String> {
    let host = AppInboxHost { app };
    let path = team_store_path(&host, team.clone()).await?;
    let changed_role = role.clone();
    let outcome = with_store("register_role", path, move |store| {
        role_inbox::register_role(store, &role, &instance_id, now_secs())
    })
    .await?;
    super::agent_inbox::role_delivery_changed(&host, &team.into(), None, Some(&changed_role))
        .await?;
    super::agent_read_model::agent_state_changed(&host.app);
    Ok(outcome.into())
}

/// `claim_role_messages` — atomic claim of up to `limit` pending deliveries.
#[tauri::command]
pub async fn claim_role_messages_cmd(
    app: tauri::AppHandle,
    team: InboxTeam,
    role: String,
    instance_id: String,
    generation: i64,
    limit: u32,
) -> Result<Vec<ClaimedDelivery>, String> {
    let host = AppInboxHost { app };
    let path = team_store_path(&host, team.clone()).await?;
    let claimed = with_store("claim_role_messages", path, move |store| {
        role_inbox::claim_role_messages(store, &role, &instance_id, generation, limit, now_secs())
    })
    .await?;
    // A pickup moves the message's state in the human's Messages list.
    if !claimed.is_empty() {
        host.inbox_event(InboxEvent::new(
            InboxChange::RoleClaim,
            &team.clone().into(),
            claimed.iter().map(|d| d.message_id),
        ));
        super::agent_read_model::agent_state_changed(&host.app);
    }
    Ok(claimed)
}

/// `accept_role_message` — commit the acceptance receipt. Role work is tracked
/// by the receipt and `ack_role_message`; it is not mirrored into `wv_queue`.
#[tauri::command]
pub async fn accept_role_message_cmd(
    app: tauri::AppHandle,
    team: InboxTeam,
    delivery_id: i64,
    claim_token: String,
    instance_id: String,
    generation: i64,
    disposition: AcceptDisposition,
) -> Result<String, String> {
    let host = AppInboxHost { app };
    let path = team_store_path(&host, team.clone()).await?;
    let kind = match disposition.kind.as_str() {
        "work" => AcceptKind::Work,
        "no_work" | "noWork" => AcceptKind::NoWork,
        other => {
            return Err(format!(
                "disposition.kind must be 'work' or 'no_work' (got '{other}')"
            ))
        }
    };
    let receipt = with_store("accept_role_message", path, move |store| {
        role_inbox::accept_role_message(
            store,
            delivery_id,
            &claim_token,
            &instance_id,
            generation,
            kind,
            &disposition.note_or_reason,
            now_secs(),
        )
    })
    .await?;
    super::agent_inbox::role_delivery_changed(&host, &team.into(), Some(delivery_id), None).await?;
    super::agent_read_model::agent_state_changed(&host.app);
    Ok(receipt)
}

/// `ack_role_message` — acknowledge an accepted delivery.
#[tauri::command]
pub async fn ack_role_message_cmd(
    app: tauri::AppHandle,
    team: InboxTeam,
    delivery_id: i64,
    claim_token: String,
    instance_id: String,
    generation: i64,
    receipt_id: String,
) -> Result<(), String> {
    let host = AppInboxHost { app };
    let path = team_store_path(&host, team.clone()).await?;
    with_store("ack_role_message", path, move |store| {
        role_inbox::ack_role_message(
            store,
            delivery_id,
            &claim_token,
            &instance_id,
            generation,
            &receipt_id,
            now_secs(),
        )
    })
    .await?;
    super::agent_inbox::role_delivery_changed(&host, &team.into(), Some(delivery_id), None).await?;
    super::agent_read_model::agent_state_changed(&host.app);
    Ok(())
}

/// `release_role` — release this instance's registration + any un-accepted
/// claims it still owns. Accepted/acknowledged rows are not touched.
#[tauri::command]
pub async fn release_role_cmd(
    app: tauri::AppHandle,
    team: InboxTeam,
    role: String,
    instance_id: String,
    generation: i64,
) -> Result<u64, String> {
    let host = AppInboxHost { app };
    let released = release_registration(&host, &team.into(), role, instance_id, generation).await?;
    super::agent_read_model::agent_state_changed(&host.app);
    Ok(released)
}

/// Release the registration and refresh inferred message kinds even when it held no claims.
pub(crate) async fn release_registration(
    host: &dyn InboxHost,
    team: &super::dashboard_launch::LaunchTeam,
    role: String,
    instance_id: String,
    generation: i64,
) -> Result<u64, String> {
    let path = host.team_store(team).await?;
    let changed_role = role.clone();
    let released = with_store("release_role", path, move |store| {
        role_inbox::release_role(store, &role, &instance_id, generation)
    })
    .await?;
    super::agent_inbox::role_delivery_changed(host, team, None, Some(&changed_role)).await?;
    Ok(released)
}

/// `send_role_message` — write an inbox row addressed to `role` + a pending
/// `wv_role_delivery` row in one IMMEDIATE transaction. Request-id idempotency:
/// same `request_id` + same `payload_hash` returns the same identities;
/// different payload under the same `request_id` conflicts. `attach_file` is an
/// optional managed-file reference (logical path) stored verbatim on the inbox
/// row; it participates in the payload hash so a retry with a different
/// attachment under the same `request_id` is a conflict, not a replay. `summary`
/// (the optional one-line list preview) is part of the payload hash too.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn send_role_message_cmd(
    app: tauri::AppHandle,
    team: InboxTeam,
    sender: String,
    role: String,
    text: String,
    request_id: Option<String>,
    attach_file: Option<String>,
    on_click: Option<MessageOnClick>,
    content_type: Option<crate::weft::inbox::MessageContentType>,
    summary: Option<String>,
) -> Result<SendRoleMessageResult, String> {
    let host = AppInboxHost { app };
    let launch_team: super::dashboard_launch::LaunchTeam = team.clone().into();
    let queued_role = role_inbox::role_name(&role).to_string();
    let queued_sender = sender.clone();
    let path = team_store_path(&host, team.clone()).await?;
    let attach_file = attach_file
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let payload_hash = role_send_payload_hash(
        &sender,
        &role,
        &text,
        attach_file.as_deref(),
        content_type,
        None,
        summary.as_deref(),
    );
    let outcome = with_store("send_role_message", path, move |store| {
        role_inbox::send_role_message_commit_with_reply_to(
            store,
            &sender,
            &role,
            &text,
            None,
            None,
            request_id.as_deref(),
            &payload_hash,
            now_secs(),
            attach_file.as_deref(),
            on_click.as_ref(),
            content_type,
            None,
            summary.as_deref(),
        )
    })
    .await;
    match outcome {
        Ok((message_id, delivery_id)) => {
            host.inbox_event(InboxEvent::new(
                InboxChange::Stored,
                &launch_team,
                [message_id],
            ));
            // A dashboard send queues like any other: `role_pending` routines are offered it.
            host.role_message_queued(&launch_team, &queued_role, message_id, &queued_sender);
            super::agent_read_model::agent_state_changed(&host.app);
            Ok(SendRoleMessageResult::Sent {
                message_id,
                delivery_id,
            })
        }
        Err(err) if err.contains("request_id reused") => Ok(SendRoleMessageResult::Conflict),
        Err(err) if err.contains("must not be blank") || err.contains("is not a role queue") => {
            Ok(SendRoleMessageResult::Invalid { message: err })
        }
        Err(err) => Err(err),
    }
}

/// `send_role_message` outcome. `Sent` carries both identities; `Conflict` means
/// the request_id was reused with a different payload (the committed outcome is
/// authoritative, not what the retrying caller just offered); `Invalid` carries
/// a blank-text or similar refusal reason.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum SendRoleMessageResult {
    Sent {
        #[serde(rename = "messageId")]
        message_id: i64,
        #[serde(rename = "deliveryId")]
        delivery_id: i64,
    },
    Conflict,
    Invalid {
        message: String,
    },
}

/// One role send as an MCP caller asks for it, `reply_to` already resolved.
pub(crate) struct RoleSend {
    pub sender: String,
    pub role: String,
    pub text: String,
    pub request_id: Option<String>,
    pub attach_file: Option<String>,
    pub on_click: Option<MessageOnClick>,
    pub content_type: Option<crate::weft::inbox::MessageContentType>,
    pub reply_to: Option<String>,
    pub summary: Option<String>,
}

/// MCP's queue/notify boundary: only a committed send is offered to routines.
pub(crate) async fn send_role_and_notify(
    path: PathBuf,
    send: RoleSend,
    notify: impl FnOnce(&str, i64),
) -> Result<serde_json::Value, String> {
    let role = role_inbox::role_name(&send.role).to_string();
    // RI-2-ALLOW: commit role mail off the async runtime before notifying.
    let outcome = await_blocking("send_role_message", move || {
        // RI-2-ALLOW: inside await_blocking closure, off the async runtime.
        let store = WeftStore::open(path).map_err(|e| format!("open role store: {e}"))?;
        commit_role_send(&store, &send, chrono::Utc::now().timestamp())
    })
    .await?;
    if outcome["status"] == "sent" {
        if let Some(message_id) = outcome["messageId"].as_i64() {
            notify(&role, message_id);
        }
    }
    Ok(outcome)
}

/// Commit `send` (hashing its payload for `requestId` idempotency) and answer as
/// `send_role_message` does: `{status:"sent",messageId,deliveryId}`,
/// `{status:"conflict"}` (the request id was used for another payload) or
/// `{status:"invalid",message}` (blank text). Blocking SQLite.
pub(crate) fn commit_role_send(
    store: &WeftStore,
    send: &RoleSend,
    now: i64,
) -> Result<serde_json::Value, String> {
    let payload_hash = role_send_payload_hash(
        &send.sender,
        &send.role,
        &send.text,
        send.attach_file.as_deref(),
        send.content_type,
        send.reply_to.as_deref(),
        send.summary.as_deref(),
    );
    match role_inbox::send_role_message_commit_with_reply_to(
        store,
        &send.sender,
        &send.role,
        &send.text,
        None,
        None,
        send.request_id.as_deref(),
        &payload_hash,
        now,
        send.attach_file.as_deref(),
        send.on_click.as_ref(),
        send.content_type,
        send.reply_to.as_deref(),
        send.summary.as_deref(),
    ) {
        Ok((message_id, delivery_id)) => Ok(serde_json::json!({
            "status": "sent",
            "messageId": message_id,
            "deliveryId": delivery_id,
        })),
        Err(role_inbox::RoleInboxError::RequestConflict) => {
            Ok(serde_json::json!({ "status": "conflict" }))
        }
        Err(role_inbox::RoleInboxError::Store(e @ crate::weft::WvWriteError::Invalid(_))) => {
            Ok(serde_json::json!({ "status": "invalid", "message": e.to_string() }))
        }
        Err(e) => Err(e.to_string()),
    }
}

/// The `requestId` idempotency hash of a role send: every field that shapes the stored
/// message, so a retry that changes any of them under the same key is a conflict, not a
/// replay. `summary` is hashed as the store keeps it (normalized), so a retry that only
/// differs in its whitespace still replays.
#[allow(clippy::too_many_arguments)]
pub(crate) fn role_send_payload_hash(
    sender: &str,
    role: &str,
    text: &str,
    attach_file: Option<&str>,
    content_type: Option<crate::weft::inbox::MessageContentType>,
    reply_to: Option<&str>,
    summary: Option<&str>,
) -> String {
    let attach_bytes = attach_file.unwrap_or("");
    let ct_bytes = content_type.map(|c| c.as_stored()).unwrap_or("");
    let reply_bytes = reply_to.unwrap_or("");
    let summary = crate::weft::inbox::normalize_summary(summary).unwrap_or_default();
    let bytes =
        format!("{sender}\n{role}\n{text}\n{attach_bytes}\n{ct_bytes}\n{reply_bytes}\n{summary}");
    blake3::hash(bytes.as_bytes()).to_hex().to_string()
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// How often the app renews session-held role registrations. Well inside the 60s
/// local TTL, so one slow tick never lapses a live session's role.
pub(crate) const ROLE_RENEW_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

/// The team stores holding role registrations that name a session, which the
/// renewer visits each tick. A store is noted when a session registers (or claims)
/// in it and dropped once a tick finds no live session registration there.
#[derive(Default)]
pub(crate) struct RoleRenewalStores(std::sync::Mutex<std::collections::BTreeSet<PathBuf>>);

impl RoleRenewalStores {
    pub(crate) fn note(&self, path: PathBuf) {
        if let Ok(mut stores) = self.0.lock() {
            stores.insert(path);
        }
    }

    fn snapshot(&self) -> Vec<PathBuf> {
        self.0
            .lock()
            .map(|stores| stores.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn forget(&self, path: &PathBuf) {
        if let Ok(mut stores) = self.0.lock() {
            stores.remove(path);
        }
    }
}

/// Note `path` for the renewer, when the app runs one.
pub(crate) fn note_role_store(app: &tauri::AppHandle, path: PathBuf) {
    use tauri::Manager;
    if let Some(stores) = app.try_state::<RoleRenewalStores>() {
        stores.note(path);
    }
}

/// Whether session `id` runs, read from the same session list `list_sessions` and the
/// inbox use. A running session is live. A session on a machine that is not connected
/// is unknown (it may still run there; its registration rides out the remote TTL). A
/// listed session that exited is ended. An id no listed session holds is unknown.
pub(crate) fn session_liveness(
    rows: &[crate::mcp::session_list::SessionListRow],
    id: &str,
) -> role_inbox::SessionLiveness {
    use super::agent_inbox::{KnownSession, KnownSessions};
    use crate::mcp::session_list::SessionState;
    use role_inbox::SessionLiveness;
    let sessions = KnownSessions::new(rows.iter().map(KnownSession::from).collect());
    let row = sessions
        .find(id)
        .and_then(|s| rows.iter().find(|row| row.handle == s.handle));
    match row {
        Some(row) if crate::mcp::identity_tools::is_running(row.state) => SessionLiveness::Live,
        Some(row) if row.state == SessionState::Exited => SessionLiveness::Ended,
        _ => SessionLiveness::Unknown,
    }
}

pub(crate) fn renew_store(
    store: &WeftStore,
    now: i64,
    rows: &[crate::mcp::session_list::SessionListRow],
) -> role_inbox::RoleInboxResult<role_inbox::RenewReport> {
    role_inbox::renew_session_registrations(store, now, |session| session_liveness(rows, session))
}

/// One renewer tick: read the session list once, then renew or release every noted
/// store's session-held registrations.
async fn renew_roles_once(app: &tauri::AppHandle) {
    use tauri::Manager;
    let Some(stores) = app.try_state::<RoleRenewalStores>() else {
        return;
    };
    let paths = stores.snapshot();
    if paths.is_empty() {
        return;
    }
    let rows = match crate::mcp::session_list::read_session_list(app, None).await {
        Ok(rows) => std::sync::Arc::new(rows),
        Err(error) => {
            // Liveness unknown this tick: change nothing; the TTL covers a missed tick.
            log::warn!(target: "agents::roles", "role renewal skipped: {error}");
            return;
        }
    };
    let mut changed = false;
    for path in paths {
        let (rows, store_path) = (rows.clone(), path.clone());
        let report = await_blocking("role_renewal", move || {
            // RI-2-ALLOW: inside await_blocking closure, off the async runtime.
            let store = WeftStore::open(store_path).map_err(|e| format!("open role store: {e}"))?;
            renew_store(&store, now_secs(), &rows).map_err(|e| e.to_string())
        })
        .await;
        match report {
            Ok(report) => {
                if report.released > 0 {
                    changed = true;
                    let host = AppInboxHost { app: app.clone() };
                    if let Ok(teams) = host.synced_teams().await {
                        for team in teams {
                            if host.team_store(&team).await.ok().as_ref() == Some(&path) {
                                let _ = super::agent_inbox::role_delivery_changed(
                                    &host, &team, None, None,
                                )
                                .await;
                                break;
                            }
                        }
                    }
                }
                if report.remaining == 0 {
                    stores.forget(&path);
                }
            }
            Err(error) => {
                log::warn!(target: "agents::roles", "role renewal failed for {}: {error}", path.display());
            }
        }
    }
    if changed {
        super::agent_read_model::agent_state_changed(app);
    }
}

/// Start the app's role renewer: every [`ROLE_RENEW_INTERVAL`], keep each running
/// session's role registrations alive and release those of sessions that ended.
pub(crate) fn spawn_role_renewer(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(ROLE_RENEW_INTERVAL).await;
            renew_roles_once(&app).await;
        }
    });
}

// Silence the dead-code warning on helper types referenced from the dto builder
// paths and tests only.
#[allow(dead_code)]
fn _types_used() {
    let _ = WvWriteError::Invalid;
    let _: WvWriteResult<()> = Ok(());
    let _: Option<SendRequestOutcome> = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::session_list::{SessionListRow, SessionState};
    use crate::weft::role_inbox::SessionLiveness;

    fn row(handle: &str, session: &str, state: SessionState) -> SessionListRow {
        SessionListRow {
            id: handle.to_string(),
            team: None,
            queue_role: None,
            role_join_error: None,
            chat_workspace_id: None,
            group_unread: 0,
            group_read_id: 0,
            handle: handle.into(),
            display_name: handle.into(),
            state,
            launched_by: None,
            story: None,
            workspace: None,
            role: None,
            avatar: None,
            agent: None,
            machine_id: None,
            workspace_handle: None,
            machine: None,
            unread: Some(0),
            oldest_unread_at: None,
            mail_notice: None,
            notified_at: None,
            subagents: 0,
            inbox_waiter: Default::default(),
            progress: None,
            launch_telemetry: None,
            release_qualified: None,
            release_note: None,
            deadline_at_ms: None,
            expiring: false,
            session: Some(session.into()),
            failed: false,
        }
    }

    #[test]
    fn liveness_follows_the_session_list() {
        let rows = vec![
            row("h-run", "s-run", SessionState::Running),
            row("h-idle", "s-idle", SessionState::Idle),
            row("h-gone", "s-gone", SessionState::Exited),
            row("h-off", "s-off", SessionState::Offline),
        ];
        assert_eq!(session_liveness(&rows, "s-run"), SessionLiveness::Live);
        assert_eq!(session_liveness(&rows, "h-idle"), SessionLiveness::Live);
        assert_eq!(session_liveness(&rows, "s-gone"), SessionLiveness::Ended);
        assert_eq!(session_liveness(&rows, "s-off"), SessionLiveness::Unknown);
        assert_eq!(session_liveness(&rows, "nobody"), SessionLiveness::Unknown);
    }

    /// The seam: a store noted for a session, renewed by the session list's truth.
    #[test]
    fn a_killed_session_listed_as_exited_frees_its_role_on_the_next_tick() {
        let td = tempfile::TempDir::new().unwrap();
        // RI-2-ALLOW: a test, no UI thread.
        let store = WeftStore::open(td.path().join("weft.sqlite3")).unwrap();
        let options = role_inbox::RegisterOptions {
            session_id: Some("s-a".into()),
            exclusive: true,
            ttl_secs: role_inbox::DEFAULT_REGISTRATION_TTL_SECS,
        };
        role_inbox::register_role_as(&store, "triage", "s-a", &options, 1_000).unwrap();

        let running = vec![row("h-a", "s-a", SessionState::Running)];
        let report = role_inbox::renew_session_registrations(&store, 1_015, |s| {
            session_liveness(&running, s)
        })
        .unwrap();
        assert_eq!(report.renewed, 1);

        let killed = vec![row("h-a", "s-a", SessionState::Exited)];
        let report = role_inbox::renew_session_registrations(&store, 1_030, |s| {
            session_liveness(&killed, s)
        })
        .unwrap();
        assert_eq!((report.released, report.remaining), (1, 0));
        let other = role_inbox::RegisterOptions {
            session_id: Some("s-b".into()),
            ..options
        };
        role_inbox::register_role_as(&store, "triage", "s-b", &other, 1_031).unwrap();
    }

    #[test]
    fn a_different_summary_is_a_different_role_payload() {
        use crate::weft::inbox::MessageContentType::Html;
        let hash = |summary: Option<&str>| {
            role_send_payload_hash("h1", "triage", "<p>hi</p>", None, Some(Html), None, summary)
        };
        assert_ne!(hash(Some("Build is green")), hash(Some("Build is red")));
        assert_ne!(hash(Some("Build is green")), hash(None));
        // Hashed as stored: blank is no summary, whitespace differences are the same one.
        assert_eq!(hash(Some("  ")), hash(None));
        assert_eq!(
            hash(Some(" Build\n is  green ")),
            hash(Some("Build is green"))
        );

        // The seam: a retry under the same request id with another summary conflicts.
        let td = tempfile::TempDir::new().unwrap();
        // RI-2-ALLOW: a test, no UI thread.
        let store = WeftStore::open(td.path().join("weft.sqlite3")).unwrap();
        let send = |summary: Option<&str>| {
            role_inbox::send_role_message_commit_with_reply_to(
                &store,
                "h1",
                "triage",
                "<p>hi</p>",
                None,
                None,
                Some("req-1"),
                &hash(summary),
                1_000,
                None,
                None,
                Some(Html),
                None,
                summary,
            )
        };
        let (message_id, _) = send(Some("Build is green")).unwrap();
        assert_eq!(send(Some("Build  is green")).unwrap().0, message_id);
        assert!(matches!(
            send(Some("Build is red")),
            Err(role_inbox::RoleInboxError::RequestConflict)
        ));
        let stored = crate::weft::inbox::inbox_get(&store, message_id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.summary.as_deref(), Some("Build is green"));
    }

    /// The `send_role_message` tool's seam: a retry under the same requestId replays,
    /// unless the summary changed, which is a conflict.
    #[test]
    fn a_role_send_retry_with_another_summary_conflicts() {
        let td = tempfile::TempDir::new().unwrap();
        // RI-2-ALLOW: a test, no UI thread.
        let store = WeftStore::open(td.path().join("weft.sqlite3")).unwrap();
        let send = |summary: Option<&str>| RoleSend {
            sender: "h1".into(),
            role: "triage".into(),
            text: "<p>hi</p>".into(),
            request_id: Some("req-1".into()),
            attach_file: None,
            on_click: None,
            content_type: Some(crate::weft::inbox::MessageContentType::Html),
            reply_to: None,
            summary: summary.map(String::from),
        };
        let first = commit_role_send(&store, &send(Some("Build is green")), 1_000).unwrap();
        assert_eq!(first["status"], "sent");
        let replay = commit_role_send(&store, &send(Some(" Build is\ngreen ")), 1_001).unwrap();
        assert_eq!(replay["status"], "sent");
        assert_eq!(replay["messageId"], first["messageId"]);
        let changed = commit_role_send(&store, &send(Some("Build is red")), 1_002).unwrap();
        assert_eq!(changed, serde_json::json!({ "status": "conflict" }));
        let dropped = commit_role_send(&store, &send(None), 1_003).unwrap();
        assert_eq!(dropped["status"], "conflict");
        let blank = RoleSend {
            text: "  ".into(),
            request_id: None,
            ..send(None)
        };
        assert_eq!(
            commit_role_send(&store, &blank, 1_004).unwrap()["status"],
            "invalid"
        );
    }

    #[test]
    fn the_renewer_ticks_well_inside_the_local_ttl() {
        assert!(
            ROLE_RENEW_INTERVAL.as_secs() * 3 <= role_inbox::DEFAULT_REGISTRATION_TTL_SECS as u64
        );
    }
}
