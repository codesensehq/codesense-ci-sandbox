// SPDX-License-Identifier: MIT
//! The two MCP tools agents call to post to and read from a
//! workspace chat.
//!
//! The verified connection supplies the caller session. The tool
//! resolves the session's bound workspace (via
//! `DashboardLaunchState::workspace_for_session`), rejects unbound sessions,
//! rejects a `workspace` override that doesn't match, rejects writes to an
//! archived workspace (reads `WorkspaceRegistryState`), then
//! append-or-reads through the engine.

use rmcp::model::ErrorData;
use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::commands::dashboard_launch::DashboardLaunchState;
use crate::commands::workspace_chat::{
    fire_fanout, ChatAppend, MessageKind, WorkspaceChatMessage, WorkspaceChatStore,
};
use crate::commands::workspace_registry::WorkspaceRegistryState;
use crate::weft::inbox::{MessageContentType, MessageOnClick};

/// `workspace_chat_send` params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceChatSendParams {
    /// Omit to use the verified connection. A supplied id must name the same caller.
    #[serde(default)]
    pub session_id: String,
    pub text: String,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub content_type: Option<MessageContentType>,
    #[serde(default)]
    pub on_click: Option<MessageOnClick>,
    #[serde(default)]
    pub primary_file: Option<String>,
}

/// `workspace_chat_send` result.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceChatSendResult {
    pub status: &'static str, // always "sent"
    pub message_id: i64,
    pub workspace_id: String,
}

/// `workspace_chat_read` params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceChatReadParams {
    /// Omit to use the verified connection. A supplied id must name the same caller.
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub since: Option<i64>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub workspace: Option<String>,
}

/// `workspace_chat_read` result.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceChatReadResult {
    pub workspace_id: String,
    pub messages: Vec<WorkspaceChatMessage>,
    pub head_id: i64,
    pub archived: bool,
    pub unread_remaining: u32,
}

fn resolve_bound_workspace(
    app: &AppHandle,
    session_id: &str,
) -> Result<(String, String), ErrorData> {
    let launch = app
        .try_state::<DashboardLaunchState>()
        .ok_or_else(|| ErrorData::internal_error("dashboard launch state unavailable", None))?;
    let handle = launch
        .handle_for_session(session_id)
        .or_else(|| launch.remote_handle_for_session(session_id))
        .ok_or_else(|| ErrorData::invalid_params("no session binding".to_string(), None))?;
    let bound = launch.workspace_for_session(session_id).ok_or_else(|| {
        ErrorData::invalid_params("session not bound to a workspace".to_string(), None)
    })?;
    Ok((handle, bound))
}

fn check_override(override_ws: Option<&str>, bound: &str) -> Result<(), ErrorData> {
    if let Some(w) = override_ws {
        let trimmed = w.trim();
        // Treat a whitespace-only / empty override as absent so the
        // agent's error message blames the real problem (blank parameter,
        // Jinja default rendering an unset variable) rather than falsely
        // claiming a mismatch against the bound workspace.
        if trimmed.is_empty() {
            return Ok(());
        }
        if trimmed != bound {
            return Err(ErrorData::invalid_params(
                "workspace override does not match session's bound workspace".to_string(),
                None,
            ));
        }
    }
    Ok(())
}

/// MCP: `workspace_chat_send`.
pub async fn workspace_chat_send(
    app: &AppHandle,
    params: WorkspaceChatSendParams,
) -> Result<WorkspaceChatSendResult, ErrorData> {
    // Admission order: unbound session → archived workspace
    // → override mismatch. Checking override before archived would mask the
    // real refusal ("workspace archived") behind "override does not match"
    // when the caller passes a stale workspace id.
    let (handle, bound_ws) = resolve_bound_workspace(app, &params.session_id)?;

    // The workspace registry is the single source of truth for archived state.
    let archived = app
        .state::<WorkspaceRegistryState>()
        .get(&bound_ws)
        .archived_at_ms
        .is_some();
    if archived {
        return Err(ErrorData::invalid_params(
            "workspace archived".to_string(),
            None,
        ));
    }

    check_override(params.workspace.as_deref(), &bound_ws)?;

    let store = app.state::<WorkspaceChatStore>();
    if !store.is_installed() {
        return Err(ErrorData::internal_error(
            "workspace chat engine not installed",
            None,
        ));
    }

    // Resolve the agent's display name through the launch registry (same
    // seam the role inbox uses). Fall back to the handle itself.
    let from_name = app
        .state::<DashboardLaunchState>()
        .display_name(&handle)
        .unwrap_or_else(|| handle.clone());

    // First-use: emit `Chat started.` so an agent's post into a fresh
    // workspace opens the thread with the lifecycle anchor.
    crate::commands::workspace_chat::ensure_chat_started(app, &bound_ws).await;

    let app_clone = app.clone();
    let bound_clone = bound_ws.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let store = app_clone.state::<WorkspaceChatStore>();
        let now = chrono::Utc::now().timestamp_millis();
        let at = ChatAppend {
            workspace_id: bound_clone,
            from: handle,
            from_name,
            text: params.text,
            kind: MessageKind::Chat,
            content_type: params.content_type,
            on_click: params.on_click,
            primary_file: params.primary_file,
        };
        store.append(at, now)
    })
    .await
    .map_err(|e| ErrorData::internal_error(format!("join: {e}"), None))?;
    let message = outcome.map_err(|e| match e {
        crate::commands::workspace_chat::ChatStoreError::EmptyText
        | crate::commands::workspace_chat::ChatStoreError::TooLong
        | crate::commands::workspace_chat::ChatStoreError::InvalidWorkspaceId(_) => {
            ErrorData::invalid_params(e.to_string(), None)
        }
        other => ErrorData::internal_error(other.to_string(), None),
    })?;

    // Share the Tauri `#[command]` path's fan-out helper so a future tweak
    // to the fan-out contract (payload field, additional subscribers) does
    // not drift between the two surfaces. The helper also gates the
    // routine note on MessageKind so the agent is not woken for engine
    // lifecycle rows — this path writes a regular chat row.
    fire_fanout(
        app,
        &bound_ws,
        message.message_id,
        message.kind,
        &message.from,
    );

    Ok(WorkspaceChatSendResult {
        status: "sent",
        message_id: message.message_id,
        workspace_id: bound_ws,
    })
}

/// MCP: `workspace_chat_read`.
pub async fn workspace_chat_read(
    app: &AppHandle,
    params: WorkspaceChatReadParams,
) -> Result<WorkspaceChatReadResult, ErrorData> {
    let (handle, bound_ws) = resolve_bound_workspace(app, &params.session_id)?;
    check_override(params.workspace.as_deref(), &bound_ws)?;

    let archived = app
        .state::<WorkspaceRegistryState>()
        .get(&bound_ws)
        .archived_at_ms
        .is_some();

    let store = app.state::<WorkspaceChatStore>();
    if !store.is_installed() {
        return Err(ErrorData::internal_error(
            "workspace chat engine not installed",
            None,
        ));
    }
    let app_clone = app.clone();
    let bound_clone = bound_ws.clone();
    let out = tauri::async_runtime::spawn_blocking(move || {
        let store = app_clone.state::<WorkspaceChatStore>();
        store.read_for_session(
            &bound_clone,
            &handle,
            &params.session_id,
            params.since,
            params.limit,
        )
    })
    .await
    .map_err(|e| ErrorData::internal_error(format!("join: {e}"), None))?
    .map_err(|e| match e {
        crate::commands::workspace_chat::ChatStoreError::InvalidWorkspaceId(_) => {
            ErrorData::invalid_params(e.to_string(), None)
        }
        other => ErrorData::internal_error(other.to_string(), None),
    })?;

    let _ = app.emit("codesense:workspace-chat-updated", ());
    Ok(WorkspaceChatReadResult {
        workspace_id: out.page.workspace_id,
        messages: out.page.messages,
        head_id: out.page.head_id,
        unread_remaining: out.unread_remaining,
        archived,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_override_rejects_mismatch() {
        let err = check_override(Some("story:other"), "story:self").unwrap_err();
        assert!(
            err.to_string()
                .contains("workspace override does not match"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn check_override_accepts_matching_or_absent() {
        assert!(check_override(Some("story:self"), "story:self").is_ok());
        assert!(check_override(None, "story:self").is_ok());
    }

    // The admission order is enforced by the implementation code order in
    // `workspace_chat_send`: unbound session (`resolve_bound_workspace`) →
    // archived check (reads `WorkspaceRegistryState`) → override check
    // (`check_override`). The admission-order contract is a reading-level
    // guarantee — an integration test would need a live Tauri app with
    // every state registered, which the project's test harness does not
    // support for this layer. The unit tests below cover the individual
    // predicates; the order itself has no seam or e2e test yet.

    #[test]
    fn tool_signatures_round_trip_through_json() {
        let send = serde_json::json!({
            "sessionId": "sess-1",
            "text": "hi",
        });
        let send: WorkspaceChatSendParams = serde_json::from_value(send).unwrap();
        assert_eq!(send.session_id, "sess-1");
        assert_eq!(send.text, "hi");
        assert!(send.workspace.is_none());

        let read = serde_json::json!({
            "sessionId": "sess-1",
            "since": 7,
        });
        let read: WorkspaceChatReadParams = serde_json::from_value(read).unwrap();
        assert_eq!(read.session_id, "sess-1");
        assert_eq!(read.since, Some(7));
    }
}
