// SPDX-License-Identifier: MIT
//! Who a session is, as CodeSense shows it: `who_am_i`, and the running-session check
//! the tools that let a caller name a session (as an inbox sender, as a launcher) share.
//!
//! A caller may name any running session, its own or another's, on this Mac or on a
//! machine: CodeSense does not decide who may speak as whom. It only refuses a session
//! that is not running, so nothing is recorded under an id no tab holds.

use serde::Serialize;

use super::session_list::{SessionListRow, SessionState};
use crate::commands::agent_inbox::{KnownSession, KnownSessions};

/// A session in this state runs now. An exited tab does not, nor does one on a machine
/// that is not connected (it may still run there, but nothing here can reach it).
pub(crate) fn is_running(state: SessionState) -> bool {
    !matches!(state, SessionState::Exited | SessionState::Offline)
}

/// The running session `id` (a handle or a tab session id) names among `rows`.
pub(crate) fn running_in<'a>(
    rows: &'a [SessionListRow],
    id: &str,
) -> Result<&'a SessionListRow, String> {
    let id = id.trim();
    if id.is_empty() {
        return Err("give a session id: your $CODESENSE_TAB_SESSION_ID".into());
    }
    let sessions = KnownSessions::new(rows.iter().map(KnownSession::from).collect());
    let found = sessions
        .find(id)
        .and_then(|s| rows.iter().find(|row| row.handle == s.handle));
    match found {
        Some(row) if is_running(row.state) => Ok(row),
        Some(row) if row.state == SessionState::Offline => Err(format!(
            "session '{id}' (\"{}\") is on a machine that is not connected",
            row.display_name
        )),
        Some(row) => Err(format!(
            "session '{id}' (\"{}\") has exited",
            row.display_name
        )),
        None => Err(format!(
            "no session '{id}' is running; pass a running session's id, normally your own $CODESENSE_TAB_SESSION_ID"
        )),
    }
}

/// The running session `id` names, read now.
pub(crate) async fn running_session(
    app: &tauri::AppHandle,
    id: &str,
) -> Result<SessionListRow, String> {
    let rows = super::session_list::read_session_list(app, None).await?;
    running_in(&rows, id).cloned()
}

/// What `who_am_i` returns: the caller's own `list_sessions` row, with the session id it
/// passed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WhoAmI {
    pub handle: String,
    pub session_id: Option<String>,
    /// Its tab's name now: what its messages show as.
    pub display_name: String,
    pub machine: Option<String>,
    /// A remote session's machine by its id (`machine` is the label), for the
    /// calls that name a machine.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    /// A remote saved-Agent session's prepared workspace, by its handle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_handle: Option<String>,
    pub workspace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub story: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    pub launched_by: Option<String>,
    pub state: SessionState,
}

/// `who_am_i` over the sessions listed now.
pub(crate) fn who_am_i_in(rows: &[SessionListRow], session_id: &str) -> Result<WhoAmI, String> {
    let row = running_in(rows, session_id)?;
    Ok(WhoAmI {
        handle: row.handle.clone(),
        session_id: row
            .session
            .clone()
            .or_else(|| Some(session_id.trim().to_string())),
        display_name: row.display_name.clone(),
        machine: row.machine.clone(),
        machine_id: row.machine_id.clone(),
        workspace_handle: row.workspace_handle.clone(),
        workspace: row.workspace.clone(),
        story: row.story.clone(),
        role: row.role.clone(),
        avatar: row.avatar.clone(),
        agent: row.agent.clone(),
        launched_by: row.launched_by.clone(),
        state: row.state,
    })
}

/// `who_am_i`: the session `session_id` names, as CodeSense shows it now.
pub(crate) async fn who_am_i(app: &tauri::AppHandle, session_id: &str) -> Result<WhoAmI, String> {
    let rows = super::session_list::read_session_list(app, None).await?;
    who_am_i_in(&rows, session_id)
}

pub(crate) const WHO_AM_I_DESCRIPTION: &str = "Who you are in CodeSense. {session_id} is optional: omitted, it is the tab this MCP connection runs in; pass your $CODESENSE_TAB_SESSION_ID to name a session (on a remote machine it is your remote tab's session id). Returns {handle, sessionId, displayName, machine, machineId?, workspaceHandle?, workspace, story?, role?, avatar?, agent?, launchedBy, state}: your own list_sessions row. `displayName` is your tab's current name — your inbox messages (send_message, reply) show as that name; rename yourself with rename_terminal_tab and the new name shows from then on. `handle` is how others address you (send_message {to}). `machine` is \"This Mac\" for a tab on this Mac. `machineId` is the id of a remote tab's machine (what machine tools address it by); it is absent on this Mac. Errors when the session is not running (exited, or its machine is not connected).";

#[cfg(test)]
mod tests {
    #[path = "engine_identity_tests.rs"]
    mod engine_identity_tests;

    use super::*;

    fn row(handle: &str, session: &str, name: &str, state: SessionState) -> SessionListRow {
        SessionListRow {
            id: handle.to_string(),
            team: None,
            queue_role: None,
            role_join_error: None,
            chat_workspace_id: None,
            group_unread: 0,
            group_read_id: 0,
            handle: handle.into(),
            display_name: name.into(),
            state,
            launched_by: None,
            story: None,
            workspace: Some("root".into()),
            role: None,
            avatar: None,
            agent: None,
            machine_id: None,
            workspace_handle: None,
            machine: Some("This Mac".into()),
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

    fn rows() -> Vec<SessionListRow> {
        let mut remote = row(
            "remote-tab-7",
            "remote-shell-7",
            "build box",
            SessionState::Idle,
        );
        remote.machine = Some("droplet".into());
        remote.launched_by = Some("SW-1".into());
        remote.role = Some("worker".into());
        let mut launched = row("SW-1", "pty-launched", "Lead", SessionState::Running);
        launched.story = Some("SW-1".into());
        vec![
            // A tab a person opened by hand: its handle is its session id.
            row("pty-hand", "pty-hand", "Terminal 3", SessionState::Running),
            launched,
            remote,
            row("pty-dead", "pty-dead", "Old tab", SessionState::Exited),
            row(
                "remote-tab-gone",
                "remote-shell-gone",
                "Away",
                SessionState::Offline,
            ),
        ]
    }

    #[test]
    fn who_am_i_answers_for_a_hand_opened_local_tab_by_its_session() {
        let me = who_am_i_in(&rows(), " pty-hand ").unwrap();
        assert_eq!(me.handle, "pty-hand");
        assert_eq!(me.session_id.as_deref(), Some("pty-hand"));
        assert_eq!(me.display_name, "Terminal 3");
        assert_eq!(me.machine.as_deref(), Some("This Mac"));
        assert_eq!(me.state, SessionState::Running);
        let launched = who_am_i_in(&rows(), "pty-launched").unwrap();
        assert_eq!(
            (launched.handle.as_str(), launched.story.as_deref()),
            ("SW-1", Some("SW-1"))
        );
    }

    #[test]
    fn who_am_i_answers_for_a_remote_tab_by_its_remote_shell_session_or_handle() {
        for id in ["remote-shell-7", "remote-tab-7"] {
            let me = who_am_i_in(&rows(), id).unwrap();
            assert_eq!(me.handle, "remote-tab-7");
            assert_eq!(me.session_id.as_deref(), Some("remote-shell-7"));
            assert_eq!(me.display_name, "build box");
            assert_eq!(me.machine.as_deref(), Some("droplet"));
            assert_eq!(me.launched_by.as_deref(), Some("SW-1"));
            assert_eq!(me.role.as_deref(), Some("worker"));
        }
        let json = serde_json::to_value(who_am_i_in(&rows(), "remote-shell-7").unwrap()).unwrap();
        assert_eq!(json["displayName"], "build box");
        assert_eq!(json["sessionId"], "remote-shell-7");
        assert_eq!(json["state"], "idle");
        assert!(json.get("story").is_none());
    }

    #[test]
    fn a_remote_session_names_its_machine_id_only_when_it_is_known() {
        let mut rows = rows();
        let remote = rows
            .iter_mut()
            .find(|row| row.handle == "remote-tab-7")
            .unwrap();
        remote.machine_id = Some("aws-dev-box-447c4b".into());
        remote.workspace_handle = Some("remote-workspace-abc".into());
        let json = serde_json::to_value(who_am_i_in(&rows, "remote-shell-7").unwrap()).unwrap();
        assert_eq!(json["machineId"], "aws-dev-box-447c4b");
        assert_eq!(json["workspaceHandle"], "remote-workspace-abc");
        assert_eq!(json["machine"], "droplet");
        let json = serde_json::to_value(who_am_i_in(&rows, "pty-hand").unwrap()).unwrap();
        assert!(json.get("machineId").is_none());
    }

    #[test]
    fn a_session_that_is_not_running_is_refused() {
        assert!(who_am_i_in(&rows(), "pty-dead")
            .unwrap_err()
            .contains("has exited"));
        assert!(who_am_i_in(&rows(), "remote-shell-gone")
            .unwrap_err()
            .contains("not connected"));
        assert!(who_am_i_in(&rows(), "made-up")
            .unwrap_err()
            .contains("no session 'made-up' is running"));
        assert!(who_am_i_in(&rows(), "  ").is_err());
    }

    #[test]
    fn any_running_session_may_be_named_not_only_the_callers() {
        for id in ["pty-hand", "SW-1", "pty-launched", "remote-tab-7"] {
            assert!(running_in(&rows(), id).is_ok(), "{id}");
        }
    }
}
