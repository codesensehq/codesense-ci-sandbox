// SPDX-License-Identifier: MIT
use super::super::agent_inbox::{InboxHost, KnownSessions};
use super::*;
use std::path::PathBuf;
use std::sync::Mutex;

/// Only the external store location is substituted; registration and startup run in production code.
pub(crate) struct StartupHost {
    pub path: PathBuf,
    pub noted: Mutex<Vec<PathBuf>>,
}
impl StartupHost {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            noted: Mutex::default(),
        }
    }
    pub fn store(&self) -> WeftStore {
        WeftStore::open(&self.path).unwrap()
    }
}
#[async_trait::async_trait]
impl InboxHost for StartupHost {
    async fn team_store(&self, _: &LaunchTeam) -> Result<PathBuf, String> {
        Ok(self.path.clone())
    }
    fn note_role_store(&self, path: PathBuf) {
        self.noted.lock().unwrap().push(path);
    }
    async fn active_team(&self) -> Result<LaunchTeam, String> {
        Err("no active team".into())
    }
    fn launch_team(&self, _: &str) -> Option<LaunchTeam> {
        None
    }
    fn handle_for_session(&self, _: &str) -> Option<String> {
        None
    }
    async fn sessions(&self) -> Result<KnownSessions, String> {
        Ok(KnownSessions::default())
    }
    fn has_handle(&self, _: &str) -> bool {
        false
    }
    fn display_name(&self, _: &str) -> Option<String> {
        None
    }
    fn inbox_changed(&self) {}
}

#[tokio::test]
async fn a_cold_provider_receives_task_and_pending_facts_on_argv_after_registration() {
    use codesense_pty_core::agent_cli::AgentCli;
    let tmp = tempfile::tempdir().unwrap();
    let host = StartupHost::new(tmp.path().join("inbox.sqlite3"));
    let now = chrono::Utc::now().timestamp();
    role_inbox::send_role_message_commit(
        &host.store(),
        "human",
        "reviewer",
        "already waiting",
        None,
        None,
        None,
        "",
        now,
        None,
        None,
        None,
    )
    .unwrap();
    let team = LaunchTeam {
        org: "acme".into(),
        team: "backend".into(),
    };
    for runtime in [AgentCli::Claude, AgentCli::Codex] {
        let session = format!("cold-{runtime:?}");
        let session = session.as_str();
        let host = &host;
        start_argv(
            host,
            Some(&team),
            Some("reviewer"),
            session,
            runtime,
            vec![],
            Some("/review work"),
            |plan| async move {
                // Simulate a provider that accepts no TUI input during cold startup.
                assert!(plan.in_argv);
                assert!(plan.pty_prompt.is_none());
                assert_eq!(plan.launch_args[0], "--");
                let task = &plan.launch_args[1];
                assert!(task.starts_with("/review work\n\n"));
                assert!(task.contains("1 messages are pending"));
                let queue = crate::weft::role_state::role_state_read(
                    &host.store(),
                    Some("reviewer"),
                    now,
                    0,
                )
                .unwrap();
                assert!(queue[0]
                    .registrations
                    .iter()
                    .any(|r| r.instance_id == session));
                assert_eq!(queue[0].counts.pending, 1);
                assert_eq!(queue[0].counts.claimed, 0);
                Ok(())
            },
        )
        .await
        .unwrap();
    }
    assert_eq!(host.noted.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn unsupported_startup_or_provider_failure_is_reported_and_releases_membership() {
    let tmp = tempfile::tempdir().unwrap();
    let host = StartupHost::new(tmp.path().join("inbox.sqlite3"));
    let team = LaunchTeam {
        org: "acme".into(),
        team: "backend".into(),
    };
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let result = start_argv(
        &host,
        Some(&team),
        Some("reviewer"),
        "blocked",
        codesense_pty_core::agent_cli::AgentCli::Claude,
        vec!["--".into()],
        Some("work"),
        |_| async {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        },
    )
    .await;
    assert!(result.unwrap_err().contains("reliably"));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    let result: Result<(), _> = start(
        &host,
        Some(&team),
        Some("reviewer"),
        "failed",
        true,
        Some("work"),
        |_| async { Err("provider refused startup".into()) },
    )
    .await;
    assert_eq!(result.unwrap_err(), "provider refused startup");
    assert!(crate::weft::role_state::role_state_read(
        &host.store(),
        Some("reviewer"),
        chrono::Utc::now().timestamp(),
        0
    )
    .unwrap()[0]
        .registrations
        .iter()
        .all(|r| !r.live));
}

pub(crate) fn session_row(
    session: &str,
    state: crate::mcp::session_list::SessionState,
) -> crate::mcp::session_list::SessionListRow {
    crate::mcp::session_list::SessionListRow {
        id: session.to_string(),
        team: None,
        chat_workspace_id: None,
        group_unread: 0,
        group_read_id: 0,
        queue_role: None,
        role_join_error: None,
        handle: session.into(),
        display_name: session.into(),
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
