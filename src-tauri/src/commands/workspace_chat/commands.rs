// SPDX-License-Identifier: MIT
//! Tauri `#[command]` surface for workspace group chat in the agent inbox.
//!
//! Four commands:
//!   * `workspace_chat_send` — human compose (stamps `from = HUMAN`).
//!   * `workspace_chat_read` — paginated history + head id + archive flag.
//!   * `workspace_chat_list_workspaces` — picker/sidebar roll-up.
//!   * `workspace_chat_archive` — explicit archive toggle (chat-local stamp;
//!     the admission check reads SW-WC-1's registry).
//!
//! All four register in `lib.rs::invoke_handler![...]` (shared-plumbing
//! touch; full-workspace cargo required per `mq-cargo-queue-rules`).

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use super::index::ChatIndexEntry;
use super::model::{MessageContentType, MessageKind, MessageOnClick, HUMAN};
use super::store::{ChatAppend, ChatStoreError, WorkspaceChatStore};

use crate::commands::workspace_registry::WorkspaceRegistryState;

/// The event the FE listens for to re-fetch chat on an append.
pub const UPDATED_EVENT: &str = "codesense:workspace-chat-updated";

/// Payload for `codesense:workspace-chat-updated`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatUpdatedPayload {
    pub workspace_id: String,
    pub head_id: i64,
}

/// Result returned to the FE on a successful `send`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSendResult {
    pub message_id: i64,
    pub created_at_ms: i64,
    pub head_id: i64,
}

/// Result returned to the FE on `read`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatReadResult {
    pub workspace_id: String,
    pub messages: Vec<super::model::WorkspaceChatMessage>,
    pub head_id: i64,
    pub archived: bool,
    pub human_read_id: i64,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn is_archived(app: &AppHandle, workspace_id: &str) -> bool {
    app.state::<WorkspaceRegistryState>()
        .get(workspace_id)
        .archived_at_ms
        .is_some()
}

/// Resolve the human sender's identity for a Tauri invoke-path send.
///
/// Precedence (per design §6):
/// 1. Active local profile's `display_name` when the user is signed in.
/// 2. The principal id as resolved from the ambient auth state.
/// 3. `USER` / `USERNAME` env var.
/// 4. The literal `"you"`.
async fn resolve_human_from(app: &AppHandle) -> (String, String) {
    let principal_display = super::super::route_to_context::resolve_ambient(app)
        .await
        .ok()
        .map(|(_, user_id)| crate::domain::Principal::from(user_id).as_str().to_string());

    // Prefer the active profile's display name.
    let display = super::super::local_profile::peek_active(app)
        .map(|p| p.display_name)
        .filter(|s| !s.trim().is_empty())
        .or(principal_display)
        .or_else(|| std::env::var("USER").ok())
        .or_else(|| std::env::var("USERNAME").ok())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "you".to_string());
    (HUMAN.to_string(), display)
}

/// A committed post updates every view and offers native notices to its bound recipients.
/// Lifecycle rows update the transcript without notifying agents.
pub fn fire_fanout(
    app: &AppHandle,
    workspace_id: &str,
    head_id: i64,
    kind: MessageKind,
    from: &str,
) {
    let payload = ChatUpdatedPayload {
        workspace_id: workspace_id.to_string(),
        head_id,
    };
    if let Err(e) = app.emit(UPDATED_EVENT, &payload) {
        log::warn!(
            target: "workspace_chat",
            "emit {} for {}: {e}",
            UPDATED_EVENT,
            workspace_id,
        );
    }
    let launch = app.state::<super::super::dashboard_launch::DashboardLaunchState>();
    notify_workspace_members(&launch, workspace_id, kind, from, |handle| {
        super::super::host_inbox::mail_stored(app, handle)
    });
}

pub(crate) fn notify_workspace_members(
    launch: &super::super::dashboard_launch::DashboardLaunchState,
    workspace: &str,
    kind: MessageKind,
    from: &str,
    mut tell: impl FnMut(&str),
) {
    if kind == MessageKind::System {
        return;
    }
    for handle in launch.chat_recipients(workspace, Some(from)) {
        tell(&handle);
    }
}

/// Append synchronously (fs + fsync + index rewrite) and return the committed
/// row. Callers in `async` handlers wrap this in `spawn_blocking` (CR-3).
fn append_blocking(
    store: &WorkspaceChatStore,
    at: ChatAppend,
) -> Result<super::model::WorkspaceChatMessage, ChatStoreError> {
    store.append(at, now_ms())
}

/// Append + emit the FE nudge event on the reactor thread (CR-3: callers
/// are responsible for having run the fs ops under `spawn_blocking`).
async fn append_and_emit(
    app: &AppHandle,
    at: ChatAppend,
) -> Result<super::model::WorkspaceChatMessage, ChatStoreError> {
    let workspace_id = at.workspace_id.clone();
    let app_clone = app.clone();
    let message = tauri::async_runtime::spawn_blocking(move || {
        let store = app_clone.state::<WorkspaceChatStore>();
        append_blocking(&store, at)
    })
    .await
    .map_err(|e| ChatStoreError::Io(std::io::Error::other(format!("join: {e}"))))??;
    fire_fanout(
        app,
        &workspace_id,
        message.message_id,
        message.kind,
        &message.from,
    );
    Ok(message)
}

/// On first use of a workspace's chat, emit a `Chat started.` system row so
/// the thread opens with a dateable "day one" marker. Idempotent — subsequent
/// calls see an existing head and no-op. Best-effort; a failure logs and
/// doesn't block the caller's own append (the subsequent user/agent row is
/// still durable). Offloads the fs ops via `spawn_blocking` (CR-3).
///
/// The atomicity of "check + append message_id=1" lives in the store's
/// `append_if_first` method: the exists-check happens under the same mutex
/// as the write, so two concurrent `ensure_chat_started` calls cannot both
/// commit a duplicate anchor row (CR2-5).
pub async fn ensure_chat_started(app: &AppHandle, workspace_id: &str) {
    {
        let store = app.state::<WorkspaceChatStore>();
        if !store.is_installed() {
            return;
        }
        // Cheap short-circuit for the common path (chat already exists). The
        // authoritative check still happens inside `append_if_first` below.
        if store.exists(workspace_id) {
            return;
        }
    }
    let at = ChatAppend {
        workspace_id: workspace_id.to_string(),
        from: MessageKind::SYSTEM_FROM.to_string(),
        from_name: MessageKind::SYSTEM_FROM_NAME.to_string(),
        text: "Chat started.".to_string(),
        kind: MessageKind::System,
        content_type: Some(MessageContentType::Plain),
        on_click: None,
        primary_file: None,
    };
    let app_clone = app.clone();
    let now = now_ms();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let store = app_clone.state::<WorkspaceChatStore>();
        store.append_if_first(at, now)
    })
    .await;
    match outcome {
        Ok(Ok(Some(message))) => {
            fire_fanout(
                app,
                workspace_id,
                message.message_id,
                message.kind,
                &message.from,
            );
        }
        Ok(Ok(None)) => {
            // Another writer committed the anchor row; nothing to do.
        }
        Ok(Err(e)) => log::warn!(
            target: "workspace_chat",
            "could not emit chat_started for {workspace_id}: {e}"
        ),
        Err(e) => log::warn!(
            target: "workspace_chat",
            "join emit chat_started for {workspace_id}: {e}"
        ),
    }
}

/// Append a system row — used from here (archive) and from external seams
/// (session_joined). Keeps system-emit discipline (always `MessageKind::System`,
/// never with on_click/primary_file). Callers that want fire-and-forget
/// semantics wrap this in their own `tauri::async_runtime::spawn` and chain
/// it with any sibling writes they want strictly ordered (CR2-2).
pub async fn emit_system_message(
    app: &AppHandle,
    workspace_id: &str,
    text: impl Into<String>,
) -> Result<super::model::WorkspaceChatMessage, ChatStoreError> {
    let at = ChatAppend {
        workspace_id: workspace_id.to_string(),
        from: MessageKind::SYSTEM_FROM.to_string(),
        from_name: MessageKind::SYSTEM_FROM_NAME.to_string(),
        text: text.into(),
        kind: MessageKind::System,
        content_type: Some(MessageContentType::Plain),
        on_click: None,
        primary_file: None,
    };
    append_and_emit(app, at).await
}

/// Human compose. Dashboards call this via `window.codesense.invoke`.
#[tauri::command]
pub async fn workspace_chat_send(
    app: AppHandle,
    workspace_id: String,
    text: String,
    content_type: Option<MessageContentType>,
    on_click: Option<MessageOnClick>,
    primary_file: Option<String>,
) -> Result<ChatSendResult, String> {
    if is_archived(&app, &workspace_id) {
        return Err("workspace archived".to_string());
    }
    let (from, from_name) = resolve_human_from(&app).await;
    {
        let store = app.state::<WorkspaceChatStore>();
        if !store.is_installed() {
            return Err("workspace chat engine not installed".to_string());
        }
    }
    // First-use: emit `Chat started.` as message id=1 so the thread opens
    // with a lifecycle anchor.
    ensure_chat_started(&app, &workspace_id).await;
    let at = ChatAppend {
        workspace_id,
        from,
        from_name,
        text,
        kind: MessageKind::Chat,
        content_type,
        on_click,
        primary_file,
    };
    let msg = append_and_emit(&app, at).await.map_err(|e| e.to_string())?;
    Ok(ChatSendResult {
        message_id: msg.message_id,
        created_at_ms: msg.created_at_ms,
        head_id: msg.message_id,
    })
}

/// Observational history read. Only the explicit human-view command moves the human
/// position; agent acknowledgements come from their own MCP reads.
#[tauri::command]
pub async fn workspace_chat_read(
    app: AppHandle,
    workspace_id: String,
    since: Option<i64>,
    limit: Option<u32>,
) -> Result<ChatReadResult, String> {
    {
        let store = app.state::<WorkspaceChatStore>();
        if !store.is_installed() {
            return Err("workspace chat engine not installed".to_string());
        }
    }
    let archived = is_archived(&app, &workspace_id);
    let app_clone = app.clone();
    let ws = workspace_id.clone();
    let out = tauri::async_runtime::spawn_blocking(move || {
        let store = app_clone.state::<WorkspaceChatStore>();
        Ok::<_, ChatStoreError>((store.read(&ws, since, limit)?, store.human_read_id(&ws)?))
    })
    .await
    .map_err(|e| format!("join: {e}"))?
    .map_err(|e| e.to_string())?;
    let (out, human_read_id) = out;
    Ok(ChatReadResult {
        human_read_id,
        workspace_id: out.workspace_id,
        messages: out.messages,
        head_id: out.head_id,
        archived,
    })
}

#[tauri::command]
pub async fn workspace_chat_mark_viewed(
    app: AppHandle,
    workspace_id: String,
    through_id: i64,
) -> Result<(), String> {
    let app_task = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        app_task
            .state::<WorkspaceChatStore>()
            .note_human_read(&workspace_id, through_id)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    let _ = app.emit("codesense:workspace-chat-viewed", ());
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HumanChatRoom {
    pub workspace_id: String,
    pub unread: u32,
    pub agents: usize,
}

#[tauri::command]
pub async fn workspace_chat_human_rooms(app: AppHandle) -> Result<Vec<HumanChatRoom>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let store = app.state::<WorkspaceChatStore>();
        let launch = app.state::<super::super::dashboard_launch::DashboardLaunchState>();
        let mut ids: std::collections::BTreeSet<_> = WorkspaceChatStore::list(&store)
            .into_iter()
            .map(|w| w.workspace_id)
            .collect();
        ids.extend(launch.chat_workspaces());
        ids.into_iter()
            .map(|workspace_id| {
                let unread = store
                    .human_unread(&workspace_id)
                    .map_err(|e| e.to_string())?
                    .count;
                let agents = launch.chat_recipients(&workspace_id, None).len();
                Ok(HumanChatRoom {
                    workspace_id,
                    unread,
                    agents,
                })
            })
            .collect()
    })
    .await
    .map_err(|e| e.to_string())?
}

/// List every workspace with a chat log for the sidebar.
#[tauri::command]
pub async fn workspace_chat_list_workspaces(app: AppHandle) -> Result<Vec<ChatIndexEntry>, String> {
    let store = app.state::<WorkspaceChatStore>();
    if !store.is_installed() {
        return Ok(Vec::new());
    }
    // `list()` is a short in-memory copy of the cached index; safe on the
    // reactor thread.
    let registry = app.state::<WorkspaceRegistryState>();
    let mut rows = store.list();
    // Overlay the SW-WC-1 archive flag (source of truth) onto each row so a
    // picker that filters on `archivedAtMs` sees the current state. Overlay
    // unconditionally: if the registry says the workspace is active, a
    // stale chat-local archive stamp from a prior `workspace_chat_archive`
    // call MUST be cleared — otherwise an unarchive via SW-WC-1 leaves the
    // sidebar reporting the workspace as archived forever (CR2-4).
    for row in rows.iter_mut() {
        row.archived_at_ms = registry.get(&row.workspace_id).archived_at_ms;
    }
    rows.sort_by(|a, b| b.last_message_at_ms.cmp(&a.last_message_at_ms));
    Ok(rows)
}

/// Archive a workspace through the chat engine's surface. CR3-9: delegates
/// to SW-WC-1's `archive_workspace` so a FE caller sees a single consistent
/// archive semantic — registry flag flipped, chat-local stamp written,
/// `Workspace archived.` lifecycle row persisted, archive-changed event
/// emitted. Previously this command stamped only the chat-local flag and
/// left `is_archived` false, so a FE that invoked it believed the workspace
/// was archived while writes to it still succeeded.
#[tauri::command]
pub async fn workspace_chat_archive(app: AppHandle, workspace_id: String) -> Result<(), String> {
    super::super::workspace_registry::archive_workspace(app, workspace_id)
        .await
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::workspace_chat::store::WorkspaceChatStore;
    use tempfile::TempDir;

    #[test]
    fn system_message_matches_system_sentinel_shape() {
        let tmp = TempDir::new().unwrap();
        let store = WorkspaceChatStore::default();
        store.install(tmp.path());
        let at = ChatAppend {
            workspace_id: "story:a".to_string(),
            from: MessageKind::SYSTEM_FROM.to_string(),
            from_name: MessageKind::SYSTEM_FROM_NAME.to_string(),
            text: "Chat started.".to_string(),
            kind: MessageKind::System,
            content_type: Some(MessageContentType::Plain),
            on_click: None,
            primary_file: None,
        };
        let m = store.append(at, 1).unwrap();
        assert_eq!(m.from, "system");
        assert_eq!(m.from_name, "System");
        assert!(matches!(m.kind, MessageKind::System));
    }
}
