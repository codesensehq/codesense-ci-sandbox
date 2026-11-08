// SPDX-License-Identifier: MIT
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;

use super::*;
use codesense_host_proto::inbox_wait::mail_notice;

/// A launch registry in memory, over real per-team stores.
pub(crate) struct FakeHost {
    dir: tempfile::TempDir,
    automatic_refusal: Mutex<Option<String>>,
    automatic_cap: Mutex<
        Option<(
            crate::session_usage::spend_caps::CapStatus,
            Result<(), String>,
        )>,
    >,
    chat: crate::commands::workspace_chat::WorkspaceChatStore,
    launch: crate::commands::dashboard_launch::DashboardLaunchState,
    /// The UI-active team; `None` while no team is selected.
    active_team: Mutex<Option<String>>,
    /// The teams this machine holds.
    known_teams: Mutex<HashSet<String>>,
    /// session id → handle.
    handles: HashMap<String, String>,
    names: HashMap<String, String>,
    /// handle → the team it was launched in.
    teams: Mutex<HashMap<String, String>>,
    /// handle → the team of the workspace its tab is in, as the terminal layout says.
    tab_teams: Mutex<HashMap<String, String>>,
    changed: Mutex<u32>,
    inbox_events: Mutex<Vec<InboxEvent>>,
    /// SW-BSI-3 fixture: `(team, logical_path) → story_id` for the fake
    /// attachment-owner resolver. A map hit on `read_messages` yields the
    /// `AttachmentHint` for that message; a miss stays `None`.
    story_owners: Mutex<HashMap<(String, String), String>>,
    /// How long resolving a team's store takes (a busy app); none by default.
    store_delay: Mutex<Option<std::time::Duration>>,
    /// Every handle a message was stored for, in order.
    stored_for: Mutex<Vec<String>>,
    /// Resolve stores through the app's real path resolution under this CodeSense
    /// home, instead of a flat temp layout.
    real_home: Mutex<Option<PathBuf>>,
    /// The sessions `list_sessions` lists: its tabs, each with its current name.
    sessions: Mutex<Vec<KnownSession>>,
    /// Read scopes used by local-media seam tests; default is the human scope.
    media_scopes: Mutex<HashMap<String, crate::context_repo::caller_scope::CallerScope>>,
    /// Sender session → captured local workspace root for local-media seam tests.
    media_workspaces: Mutex<HashMap<String, PathBuf>>,
    /// Every `role_message_queued` call: `(team, role, message id, sender)`.
    queued: Mutex<Vec<(String, String, i64, String)>>,
}

impl FakeHost {
    /// H1 (session S1, "Manager") and H2 (session S2, "Sprint 1"), both launched in team
    /// `default`, which is also the UI-active team.
    pub(crate) fn new() -> Arc<Self> {
        let pairs = |items: &[(&str, &str)]| -> HashMap<String, String> {
            items
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        };
        let dir = tempfile::tempdir().unwrap();
        let chat = crate::commands::workspace_chat::WorkspaceChatStore::default();
        chat.install(dir.path());
        Arc::new(FakeHost {
            dir,
            automatic_refusal: Mutex::default(),
            automatic_cap: Mutex::default(),
            chat,
            launch: crate::commands::dashboard_launch::DashboardLaunchState::new(8),
            active_team: Mutex::new(Some("default".into())),
            known_teams: Mutex::new(["default", "design"].map(String::from).into()),
            handles: pairs(&[("S1", "H1"), ("S2", "H2")]),
            names: pairs(&[("H1", "Manager"), ("H2", "Sprint 1")]),
            teams: Mutex::new(pairs(&[("H1", "default"), ("H2", "default")])),
            tab_teams: Mutex::default(),
            changed: Mutex::default(),
            inbox_events: Mutex::default(),
            story_owners: Mutex::default(),
            store_delay: Mutex::default(),
            stored_for: Mutex::default(),
            real_home: Mutex::default(),
            sessions: Mutex::new(vec![
                known("H1", "S1", "Manager", true),
                known("H2", "S2", "Sprint 1", true),
            ]),
            media_scopes: Mutex::default(),
            media_workspaces: Mutex::default(),
            queued: Mutex::default(),
        })
    }

    /// A tab `list_sessions` lists: one a person opened by hand has its session id as its
    /// handle; a remote tab's session is its `remote-shell-…` id there.
    pub(crate) fn tab(&self, handle: &str, session: &str, name: &str, running: bool) {
        self.sessions
            .lock()
            .unwrap()
            .push(known(handle, session, name, running));
    }

    /// Rename `handle`'s tab.
    fn rename(&self, handle: &str, name: &str) {
        for s in self.sessions.lock().unwrap().iter_mut() {
            if s.handle == handle {
                s.name = name.to_string();
            }
        }
    }

    /// `handle`'s tab ended, or its machine went offline.
    fn stop(&self, handle: &str) {
        for s in self.sessions.lock().unwrap().iter_mut() {
            if s.handle == handle {
                s.running = false;
            }
        }
    }

    /// Route every team store through `team_store_path` under a CodeSense home in
    /// this fake's temp dir; returns that home.
    fn use_real_store_paths(&self) -> PathBuf {
        let home = self.dir.path().join("codesense-home");
        *self.real_home.lock().unwrap() = Some(home.clone());
        home
    }

    /// SW-BSI-3 fixture: declare that `logical_path` in `team` is owned by `story_id`
    /// so a subsequent `read_messages` returns the corresponding `AttachmentHint`.
    fn claim_owner(&self, team: &str, logical_path: &str, story_id: &str) {
        self.story_owners.lock().unwrap().insert(
            (team.to_string(), logical_path.to_string()),
            story_id.to_string(),
        );
    }

    pub(crate) fn team_store(&self, team: &str) -> PathBuf {
        let real_home = self.real_home.lock().unwrap().clone();
        match real_home {
            Some(home) => team_store_path(
                ContextResolution::Ok(real_context(&home, team)),
                &launch_team(team),
            )
            .unwrap(),
            None => self.dir.path().join(team).join("weft.sqlite3"),
        }
    }

    fn switch_team(&self, team: Option<&str>) {
        *self.active_team.lock().unwrap() = team.map(String::from);
    }

    fn rows_in(&self, team: &str) -> Vec<InboxMessage> {
        inbox::inbox_list(&WeftStore::open(self.team_store(team)).unwrap()).unwrap()
    }

    pub(crate) fn rows(&self) -> Vec<InboxMessage> {
        self.rows_in("default")
    }

    fn stored_event_ids(&self) -> Vec<i64> {
        self.inbox_events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| matches!(event.kind, InboxChange::Stored))
            .flat_map(|event| event.message_ids.iter().copied())
            .collect()
    }
}

#[async_trait::async_trait]
impl InboxHost for FakeHost {
    async fn caller_scope(&self, sender: &str) -> crate::context_repo::caller_scope::CallerScope {
        self.media_scopes
            .lock()
            .unwrap()
            .get(sender)
            .cloned()
            .unwrap_or(crate::context_repo::caller_scope::CallerScope::Human)
    }

    async fn caller_media_workspace(&self, sender: &str) -> Option<PathBuf> {
        self.media_workspaces.lock().unwrap().get(sender).cloned()
    }

    async fn prepare_media(
        &self,
        _team: &LaunchTeam,
        input: &crate::media::MediaInput,
        own: bool,
        scope: &crate::context_repo::caller_scope::CallerScope,
        workspace_root: Option<&std::path::Path>,
    ) -> Result<(crate::media::Attachment, bool), String> {
        input.validate()?;
        if let Some(path) = &input.path {
            let cache = self.dir.path().join("media-cache");
            std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
            let attachment = crate::media::prepare_local_for_scope_or_workspace(
                scope,
                std::path::Path::new(path),
                workspace_root,
                &input.alt,
                &cache,
            )?;
            return Ok((attachment, false));
        }
        Ok((
            crate::media::Attachment::remote(
                input.url.as_deref().unwrap(),
                &input.alt,
                &crate::media::Config::default(),
                own,
            )
            .map_err(|e| e.reason.to_string())?,
            false,
        ))
    }

    async fn synced_teams(&self) -> Result<Vec<LaunchTeam>, String> {
        Ok(self
            .known_teams
            .lock()
            .unwrap()
            .iter()
            .map(|team| launch_team(team))
            .collect())
    }

    async fn admit_automatic_input(&self, _: &str) -> Result<(), String> {
        if let Some((status, capability)) = &*self.automatic_cap.lock().unwrap() {
            crate::session_usage::spend_caps::admit_mail_offer(status, capability.clone())?;
        }
        self.automatic_refusal
            .lock()
            .unwrap()
            .clone()
            .map_or(Ok(()), Err)
    }

    async fn chat_announcement(
        &self,
        session: &str,
        handle: &str,
    ) -> Result<Option<inbox::ChatAnnouncement>, String> {
        let Some(workspace) = self.launch.workspace_for_session(session) else {
            return Ok(None);
        };
        let u = self
            .chat
            .unannounced(&workspace, handle, session)
            .map_err(|e| e.to_string())?;
        Ok((u.count > 0).then_some(inbox::ChatAnnouncement {
            workspace_id: workspace,
            through_id: u.through_id,
            count: u.count,
        }))
    }
    async fn chat_told(
        &self,
        session: &str,
        handle: &str,
        chat: &inbox::ChatAnnouncement,
    ) -> Result<(), String> {
        self.chat
            .note_told(&chat.workspace_id, handle, session, chat.through_id)
            .map_err(|e| e.to_string())
    }

    async fn team_store(&self, team: &LaunchTeam) -> Result<PathBuf, String> {
        let delay = *self.store_delay.lock().unwrap();
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        if team.org != ORG || !self.known_teams.lock().unwrap().contains(&team.team) {
            return Err(format!("unknown team {}:{}", team.org, team.team));
        }
        Ok(self.team_store(&team.team))
    }
    async fn active_team(&self) -> Result<LaunchTeam, String> {
        let team = self.active_team.lock().unwrap().clone();
        team.map(|t| launch_team(&t))
            .ok_or_else(|| "no active team selected".to_string())
    }
    fn launch_team(&self, handle: &str) -> Option<LaunchTeam> {
        self.teams
            .lock()
            .unwrap()
            .get(handle)
            .map(|t| launch_team(t))
    }
    async fn tab_team(&self, handle: &str) -> Option<LaunchTeam> {
        self.tab_teams
            .lock()
            .unwrap()
            .get(handle)
            .map(|t| launch_team(t))
    }
    fn handle_for_session(&self, session_id: &str) -> Option<String> {
        self.handles
            .get(session_id)
            .cloned()
            .or_else(|| self.launch.handle_for_session(session_id))
            .or_else(|| self.launch.remote_handle_for_session(session_id))
    }
    fn has_handle(&self, handle: &str) -> bool {
        self.handles.values().any(|h| h == handle)
    }
    fn display_name(&self, handle: &str) -> Option<String> {
        self.names.get(handle).cloned()
    }
    async fn sessions(&self) -> Result<KnownSessions, String> {
        Ok(KnownSessions::new(self.sessions.lock().unwrap().clone()))
    }
    fn inbox_changed(&self) {
        *self.changed.lock().unwrap() += 1;
    }
    fn inbox_event(&self, event: InboxEvent) {
        self.inbox_events.lock().unwrap().push(event);
        self.inbox_changed();
    }
    fn mail_stored(&self, handle: &str) {
        self.stored_for.lock().unwrap().push(handle.to_string());
    }
    fn role_message_queued(&self, team: &LaunchTeam, role: &str, message_id: i64, sender: &str) {
        self.queued.lock().unwrap().push((
            team.team.clone(),
            role.to_string(),
            message_id,
            sender.to_string(),
        ));
    }
    async fn owning_story_for_path(&self, team: &LaunchTeam, logical_path: &str) -> Option<String> {
        self.story_owners
            .lock()
            .unwrap()
            .get(&(team.team.clone(), logical_path.to_string()))
            .cloned()
    }
}

const ORG: &str = "acme";

fn known(handle: &str, session: &str, name: &str, running: bool) -> KnownSession {
    KnownSession {
        handle: handle.into(),
        session: Some(session.into()),
        name: name.into(),
        running,
    }
}

fn launch_team(team: &str) -> LaunchTeam {
    LaunchTeam {
        org: ORG.to_string(),
        team: team.to_string(),
    }
}

/// The fake as the port the inbox takes.
pub(crate) fn port(host: &Arc<FakeHost>) -> Arc<dyn InboxHost> {
    host.clone()
}

fn sent_id(result: SendResult) -> i64 {
    match result {
        SendResult::Sent { message_id, .. } => message_id,
        other => panic!("expected sent, got {other:?}"),
    }
}

fn is_invalid(result: &SendResult) -> bool {
    matches!(result, SendResult::Invalid { .. })
}

#[tokio::test]
async fn send_message_and_reply_route_by_handle() {
    let fake = FakeHost::new();
    let host = port(&fake);

    let asked = sent_id(
        send_message(
            &host,
            Some("S1"),
            "H2",
            "status?",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let answered = sent_id(reply(&host, Some("S2"), asked, "done", None).await.unwrap());

    let rows = fake.rows();
    let row = |id: i64| rows.iter().find(|m| m.id == id).unwrap();
    assert_eq!(
        (row(asked).from.as_str(), row(asked).to.as_str()),
        ("H1", "H2")
    );
    assert!(row(asked).read_at_ms.is_some(), "answering reads it");
    assert_eq!(
        (
            row(answered).from.as_str(),
            row(answered).to.as_str(),
            row(answered).in_reply_to
        ),
        ("H2", "H1", Some(asked))
    );

    assert_eq!(
        row(answered).in_reply_to_team,
        Some(InboxReplyTeam {
            org: ORG.into(),
            team: "default".into()
        })
    );

    // A sender that names no session is the human; a reply to it lands in the human's
    // inbox. A session id works as a recipient too.
    let from_tab = sent_id(
        send_message(&host, None, "S2", "hi", None, None, None, None, None)
            .await
            .unwrap(),
    );
    let to_human = sent_id(
        reply(&host, Some("S2"), from_tab, "hello", None)
            .await
            .unwrap(),
    );
    let rows = fake.rows();
    let row = |id: i64| rows.iter().find(|m| m.id == id).unwrap();
    assert_eq!(
        (row(from_tab).from.as_str(), row(from_tab).to.as_str()),
        (HUMAN, "H2")
    );
    assert_eq!(row(to_human).to, HUMAN);
    let anonymous = sent_id(
        send_message(&host, None, "human", "note", None, None, None, None, None)
            .await
            .unwrap(),
    );
    assert_eq!(fake.rows()[0].id, anonymous);
    assert_eq!(fake.rows()[0].from, HUMAN);

    // The human replies from the panel: the reply goes to the sender and reads the
    // message it answers.
    let human_reply = sent_id(
        reply_from_human(
            &host,
            &launch_team("default"),
            to_human,
            "thanks",
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let rows = fake.rows();
    assert_eq!(rows[0].id, human_reply);
    assert_eq!(rows[0].to, "H2");
    assert!(rows
        .iter()
        .find(|m| m.id == to_human)
        .unwrap()
        .read_at_ms
        .is_some());

    // The only refusals are inputs that cannot be stored.
    let before = fake.rows().len();
    for result in [
        send_message(&host, Some("S1"), "H2", "  ", None, None, None, None, None)
            .await
            .unwrap(),
        send_message(
            &host,
            Some("S1"),
            "nobody",
            "hi",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
        reply(&host, Some("S1"), 9999, "hi", None).await.unwrap(),
        reply(&host, Some("S1"), asked, "", None).await.unwrap(),
    ] {
        assert!(is_invalid(&result), "{result:?}");
    }
    assert_eq!(fake.rows().len(), before);
    let json = serde_json::to_value(
        send_message(
            &host,
            Some("S1"),
            "nobody",
            "hi",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(json["status"], "invalid");
    assert!(!json["message"].as_str().unwrap().contains("Weft"));
    let json = serde_json::to_value(SendResult::sent(7)).unwrap();
    assert_eq!(json, serde_json::json!({"status": "sent", "messageId": 7}));
}

#[tokio::test]
async fn read_messages_returns_oldest_first_then_empty() {
    let fake = FakeHost::new();
    let host = port(&fake);
    let first = sent_id(
        send_message(&host, Some("S1"), "H2", "one", None, None, None, None, None)
            .await
            .unwrap(),
    );
    let second = sent_id(
        send_message(&host, None, "H2", "two", None, None, None, None, None)
            .await
            .unwrap(),
    );
    send_message(
        &host,
        Some("S2"),
        "H1",
        "for H1",
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    let read = read_messages(&*host, Some("S2")).await.unwrap();
    assert_eq!(
        read,
        vec![
            ReadMessage {
                message_id: first,
                from: "H1".into(),
                from_name: "Manager".into(),
                text: "one".into(),
                in_reply_to: None,
                in_reply_to_team: None,
                reply_to: None,
                summary: None,
                primary_file: None,
                attachments: Vec::new(),
                attachment_hint: None,
            },
            ReadMessage {
                message_id: second,
                from: HUMAN.into(),
                from_name: HUMAN.into(),
                text: "two".into(),
                in_reply_to: None,
                in_reply_to_team: None,
                reply_to: None,
                summary: None,
                primary_file: None,
                attachments: Vec::new(),
                attachment_hint: None,
            },
        ]
    );
    assert!(read_messages(&*host, Some("S2")).await.unwrap().is_empty());
    // H1's message is still waiting for H1.
    assert_eq!(read_messages(&*host, Some("S1")).await.unwrap().len(), 1);
}

#[tokio::test]
async fn read_without_handle_never_returns_human_mail() {
    let fake = FakeHost::new();
    let host = port(&fake);
    send_message(
        &host,
        Some("S2"),
        "human",
        "for you",
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    assert!(read_messages(&*host, None).await.unwrap().is_empty());
    assert!(read_messages(&*host, Some("plain-tab"))
        .await
        .unwrap()
        .is_empty());
    assert!(read_messages(&*host, Some("  ")).await.unwrap().is_empty());
    // The human's message is still unread for the human.
    let view = list_messages(
        &*host,
        &launch_team("default"),
        super::InboxScope::ToHuman,
        None,
        &[],
    )
    .await
    .unwrap();
    assert_eq!(view.human_unread, 1);
    assert!(view.messages[0].unread);
}

#[tokio::test]
async fn mail_follows_the_recipients_launch_team() {
    let fake = FakeHost::new();
    let host = port(&fake);
    let asked = sent_id(
        send_message(&host, None, "H2", "status?", None, None, None, None, None)
            .await
            .unwrap(),
    );
    // The person switches the app to another team: H2's mail stays readable, and its
    // reply to the human stays in H2's team, where the human's list of that team shows it.
    fake.switch_team(Some("design"));
    let read = read_messages(&*host, Some("S2")).await.unwrap();
    assert_eq!(read.len(), 1);
    reply(&host, Some("S2"), asked, "done", None).await.unwrap();
    assert!(fake.rows_in("design").is_empty());
    let default_view = list_messages(
        &*host,
        &launch_team("default"),
        super::InboxScope::ToHuman,
        None,
        &[],
    )
    .await
    .unwrap();
    assert_eq!(default_view.human_unread, 1);
    assert_eq!(default_view.messages[0].text, "done");
    assert_eq!(
        list_messages(
            &*host,
            &launch_team("design"),
            super::InboxScope::ToHuman,
            None,
            &[]
        )
        .await
        .unwrap()
        .human_unread,
        0
    );
}

#[tokio::test]
async fn a_hand_opened_tabs_mail_stays_in_its_workspace_team_across_team_switches() {
    let fake = FakeHost::new();
    let host = port(&fake);
    // A tab opened by hand in a `design` workspace: no launch team, its handle is its
    // session, and the terminal layout places it in `design`.
    fake.tab("shell-hand", "shell-hand", "Hand tab", true);
    fake.tab_teams
        .lock()
        .unwrap()
        .insert("shell-hand".into(), "design".into());
    send_message(
        &host,
        None,
        "shell-hand",
        "first",
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(fake.rows_in("design").len(), 1);
    assert!(fake.rows().is_empty());
    // Told about it while the app shows `default`...
    assert!(hook_notice(&*host, "shell-hand").await.is_some());
    // ...it reads it after the app switched teams, and later mail joins it there.
    fake.switch_team(Some("design"));
    fake.switch_team(Some("default"));
    let read = read_messages(&*host, Some("shell-hand")).await.unwrap();
    assert_eq!(read.len(), 1);
    send_message(
        &host,
        None,
        "shell-hand",
        "second",
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let unread = unread_by_handle(&*host, &["shell-hand".to_string()]).await;
    assert_eq!(unread.found["shell-hand"].count, 1);
    assert_eq!(unread.found["shell-hand"].team, launch_team("design"));
    assert!(fake.rows().is_empty());
}

#[tokio::test]
async fn restore_summary_is_unread_update_mail_from_codesense_in_the_resolved_team() {
    let fake = FakeHost::new();
    let host = port(&fake);
    fake.switch_team(Some("design"));
    let team = (ORG.to_string(), "default".to_string());
    let text = "Terminal server updated; 2 tabs restored";
    let id = crate::commands::machines::restore::post_summary_note(&host, &team, text)
        .await
        .unwrap();
    let rows = fake.rows_in("default");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.id, id);
    assert_eq!(row.from, "codesense");
    assert_eq!(row.to, HUMAN);
    assert_eq!(row.text, text);
    assert_eq!(row.summary.as_deref(), Some(text));
    assert_eq!(row.kind, Some(inbox::MessageKind::Update));
    assert!(row.read_at_ms.is_none());
    assert!(fake.rows_in("design").is_empty());
    assert_eq!(*fake.changed.lock().unwrap(), 1);
}

#[tokio::test]
async fn deadline_note_to_human_uses_the_launch_team_when_the_active_team_differs() {
    let fake = FakeHost::new();
    let host = port(&fake);
    fake.switch_team(Some("design"));
    let launch_team = (ORG.to_string(), "default".to_string());
    let text = "Worker exceeded maxRuntime 30m and was stopped";
    let summary = "Worker reached its runtime limit";

    let id = note_to_human(&host, Some(&launch_team), text, summary)
        .await
        .unwrap();

    let rows = fake.rows_in("default");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.id, id);
    assert_eq!(row.from, "codesense");
    assert_eq!(row.to, HUMAN);
    assert_eq!(row.text, text);
    assert_eq!(row.summary.as_deref(), Some(summary));
    assert_eq!(row.kind, Some(inbox::MessageKind::Update));
    assert_eq!(row.content_type, Some(inbox::MessageContentType::Markdown));
    assert!(fake.rows_in("design").is_empty());
}

#[tokio::test]
async fn session_mail_works_with_no_active_team() {
    let fake = FakeHost::new();
    let host = port(&fake);
    fake.switch_team(None);
    sent_id(
        send_message(
            &host,
            Some("S1"),
            "H2",
            "status?",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    assert_eq!(read_messages(&*host, Some("S2")).await.unwrap().len(), 1);
    // Only mail that needs the active team (a sender with no handle) has nowhere to go.
    assert!(
        send_message(&host, None, "human", "hi", None, None, None, None, None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_removed_team_is_a_typed_error_and_creates_nothing() {
    let fake = FakeHost::new();
    let host = port(&fake);
    fake.teams
        .lock()
        .unwrap()
        .insert("H2".into(), "gone".into());
    let err = send_message(
        &host,
        Some("S1"),
        "H2",
        "status?",
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap_err();
    assert!(err.contains("unknown team acme:gone"), "{err}");
    assert!(read_messages(&*host, Some("S2")).await.is_err());
    assert!(!fake.dir.path().join("gone").exists());
    assert!(list_messages(
        &*host,
        &launch_team("gone"),
        super::InboxScope::ToHuman,
        None,
        &[]
    )
    .await
    .is_err());
}

use crate::commands::route_to_context::{ActiveContext, ContextResolution};

fn real_context(home: &std::path::Path, team: &str) -> ActiveContext {
    ActiveContext {
        codesense_home: home.to_path_buf(),
        user_id: 7,
        org_slug: ORG.to_string(),
        team_name: Some(team.to_string()),
    }
}

#[test]
fn team_store_path_resolves_only_a_held_team() {
    let team = launch_team("default");
    let tmp = tempfile::tempdir().unwrap();
    let ctx = real_context(tmp.path(), "default");
    let path = team_store_path(ContextResolution::Ok(ctx.clone()), &team).unwrap();
    assert_eq!(
        path,
        crate::weft::weft_store_path(&ctx.codesense_home, &ctx.principal(), ORG, "default")
            .unwrap()
    );
    let org_root =
        crate::context_repo::path_to_org_root(&ctx.codesense_home, &ctx.principal(), ORG).unwrap();
    assert!(
        !path.starts_with(&org_root),
        "the inbox store {path:?} must not live inside the context repo clone {org_root:?}"
    );
    assert!(path.starts_with(crate::context_repo::path_to_install_root(
        &ctx.codesense_home,
        &ctx.principal()
    )));
    assert!(team_store_path(ContextResolution::Unauthorized, &team)
        .unwrap_err()
        .contains("unknown team"));
    assert!(team_store_path(ContextResolution::NotSynced, &team)
        .unwrap_err()
        .contains("not synced"));
}

#[tokio::test]
async fn inbox_keeps_its_history_when_the_store_leaves_the_context_repo() {
    let fake = FakeHost::new();
    let home = fake.use_real_store_paths();
    let ctx = real_context(&home, "default");
    let org_root = crate::context_repo::path_to_org_root(&home, &ctx.principal(), ORG).unwrap();
    // A store written by an earlier build, inside the context-repo clone.
    let legacy = crate::context_repo::path_to_team_install_dir(&org_root, "default")
        .join(crate::weft::sqlite::WEFT_STORE_FILE);
    let earlier = {
        let store = WeftStore::open(&legacy).unwrap();
        inbox::inbox_insert(
            &store,
            "H2",
            "H1",
            "from before",
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap()
    };
    let host = port(&fake);

    let asked = sent_id(
        send_message(
            &host,
            Some("S1"),
            "H2",
            "status?",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let answered = sent_id(reply(&host, Some("S2"), asked, "done", None).await.unwrap());

    let store_path = fake.team_store("default");
    assert!(!store_path.starts_with(&org_root), "{store_path:?}");
    assert!(store_path.is_file());
    assert!(!legacy.exists(), "the in-clone store is no longer used");
    let rows = fake.rows();
    let text = |id: i64| rows.iter().find(|m| m.id == id).unwrap().text.clone();
    assert_eq!(text(earlier), "from before");
    assert_eq!(text(asked), "status?");
    assert_eq!(text(answered), "done");
}

#[tokio::test]
async fn identity_unbound_mcp_cannot_answer_the_persons_needs_you_mail() {
    let fake = FakeHost::new();
    let host = port(&fake);
    let question = sent_id(
        send_message_with_kind(
            &host,
            Some("S2"),
            "human",
            "Approve this action?",
            None,
            None,
            None,
            None,
            None,
            Some(inbox::MessageKind::NeedsYou),
        )
        .await
        .unwrap(),
    );
    let before = fake.rows().len();
    assert!(crate::mcp::handler::local_inbox_caller(None, None).is_err());
    assert!(crate::mcp::handler::local_inbox_caller(Some("human"), Some("S1")).is_err());
    assert_eq!(fake.rows().len(), before);
    let caller = crate::mcp::handler::local_inbox_caller(None, Some("S1")).unwrap();
    let answered = sent_id(
        reply(&host, caller, question, "Agent's answer", None)
            .await
            .unwrap(),
    );
    let rows = fake.rows();
    assert_eq!(
        rows.iter().find(|row| row.id == answered).unwrap().from,
        "H1"
    );
    let human = sent_id(
        reply_from_human(
            &host,
            &launch_team("default"),
            question,
            "Person's answer",
            None,
            None,
        )
        .await
        .unwrap(),
    );
    assert_eq!(
        fake.rows().iter().find(|row| row.id == human).unwrap().from,
        "human"
    );
}

#[tokio::test]
async fn a_summary_reaches_every_read_model() {
    let fake = FakeHost::new();
    let host = port(&fake);
    let html = sent_id(
        send_message(
            &host,
            Some("S1"),
            "H2",
            "<h1>Build</h1>",
            None,
            None,
            Some(inbox::MessageContentType::Html),
            None,
            Some(" Build\n is green "),
        )
        .await
        .unwrap(),
    );
    let read = read_messages(&*host, Some("S2")).await.unwrap();
    assert_eq!(read[0].summary.as_deref(), Some("Build is green"));
    let json = serde_json::to_value(&read[0]).unwrap();
    assert_eq!(json["summary"], "Build is green");

    let to_human = sent_id(
        send_message(
            &host,
            Some("S1"),
            "human",
            "plain",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let answer = sent_id(
        reply_from_human(
            &host,
            &launch_team("default"),
            to_human,
            "thanks",
            None,
            Some("Thanks"),
        )
        .await
        .unwrap(),
    );
    let view = list_messages(
        &*host,
        &launch_team("default"),
        super::InboxScope::All,
        None,
        &[],
    )
    .await
    .unwrap();
    let row = |id: i64| view.messages.iter().find(|m| m.message_id == id).unwrap();
    assert_eq!(row(html).summary.as_deref(), Some("Build is green"));
    assert_eq!(row(answer).summary.as_deref(), Some("Thanks"));
    let plain = serde_json::to_value(row(to_human)).unwrap();
    assert!(plain.get("summary").is_none(), "absent, not null: {plain}");
    assert_eq!(
        serde_json::to_value(row(html)).unwrap()["summary"],
        "Build is green"
    );
}

/// SW-AGR-18: send_message and reply store the sender's kind; inbox rows carry the
/// effective kind; the human's reply and an unmarked report are updates.
#[tokio::test]
async fn a_kind_is_stored_by_send_and_reply_and_listed_as_the_effective_kind() {
    use inbox::MessageKind::{NeedsYou, Update};
    let fake = FakeHost::new();
    let host = port(&fake);
    let send = |text: &'static str, reply_to: Option<&'static str>, kind| {
        let host = host.clone();
        async move {
            sent_id(
                send_message_with_kind(
                    &host,
                    Some("S1"),
                    "human",
                    text,
                    None,
                    None,
                    None,
                    reply_to,
                    None,
                    kind,
                )
                .await
                .unwrap(),
            )
        }
    };
    let decide = send("ship it?", None, Some(NeedsYou)).await;
    let report = send("merged", None, None).await;
    let routed = send("which branch?", Some("H2"), None).await;
    // The human asks; the agent's answer is an update unless it asks back.
    let asked = sent_id(
        send_message(&host, None, "H1", "status?", None, None, None, None, None)
            .await
            .unwrap(),
    );
    let answer = sent_id(
        reply_with_kind(&host, Some("S1"), asked, "green", None, None)
            .await
            .unwrap(),
    );
    let asks_back = sent_id(
        reply_with_kind(&host, Some("S1"), asked, "merge too?", None, Some(NeedsYou))
            .await
            .unwrap(),
    );
    let stored = |id: i64| fake.rows().into_iter().find(|m| m.id == id).unwrap().kind;
    assert_eq!(stored(decide), Some(NeedsYou));
    assert_eq!(stored(report), None, "an unset kind is stored as none");
    assert_eq!(stored(asks_back), Some(NeedsYou));
    let view = list_messages(
        &*host,
        &launch_team("default"),
        super::InboxScope::ToHuman,
        None,
        &[],
    )
    .await
    .unwrap();
    let kind = |id: i64| {
        view.messages
            .iter()
            .find(|m| m.message_id == id)
            .unwrap()
            .kind
    };
    assert_eq!(
        [decide, report, routed, answer, asks_back].map(kind),
        [NeedsYou, Update, NeedsYou, Update, NeedsYou]
    );
    let wire = serde_json::to_value(view.messages.iter().find(|m| m.message_id == decide)).unwrap();
    assert_eq!(wire["kind"], "needs-you");
}

/// SW-AGR-18: a manager's digest is stored as an update (it stays one after the role is
/// released); the send result names the lane, and an unmarked question gets a hint.
#[tokio::test]
async fn a_send_to_the_human_says_its_lane_and_a_digest_stays_an_update() {
    use crate::weft::role_inbox::{register_role, release_role};
    use inbox::MessageKind::{NeedsYou, Update};
    let fake = FakeHost::new();
    let host = port(&fake);
    let store = WeftStore::open(fake.team_store("default")).unwrap();
    let now = chrono::Utc::now().timestamp();
    let reg = register_role(&store, "manager", "S1", now).unwrap();
    let send = |text: &'static str, reply_to: Option<&'static str>, kind| {
        let host = host.clone();
        async move {
            send_message_with_kind(
                &host,
                Some("S1"),
                "human",
                text,
                None,
                None,
                None,
                reply_to,
                None,
                kind,
            )
            .await
            .unwrap()
        }
    };
    let digest = send("3 merged, 1 blocked", Some("role:manager"), None).await;
    let SendResult::Sent {
        message_id: digest_id,
        kind,
        hint,
    } = digest.clone()
    else {
        panic!("not sent: {digest:?}")
    };
    assert_eq!((kind, hint), (Some(Update), None));
    let json = serde_json::to_value(&digest).unwrap();
    assert_eq!(json["kind"], "update");
    assert!(json.get("hint").is_none());
    release_role(&store, "manager", "S1", reg.generation).unwrap();
    let stored = |id: i64| fake.rows().into_iter().find(|m| m.id == id).unwrap();
    assert_eq!(
        stored(digest_id).kind,
        Some(Update),
        "stored, not inferred later"
    );
    let view = list_messages(
        &*host,
        &launch_team("default"),
        super::InboxScope::ToHuman,
        None,
        &[],
    )
    .await
    .unwrap();
    assert_eq!(
        view.messages
            .iter()
            .find(|m| m.message_id == digest_id)
            .unwrap()
            .kind,
        Update
    );
    // An unmarked question: an update, with a hint; never reclassified by its words.
    let SendResult::Sent {
        message_id,
        kind,
        hint,
    } = send("Ship it?", None, None).await
    else {
        panic!("not sent")
    };
    assert_eq!(kind, Some(Update));
    assert!(hint
        .as_deref()
        .is_some_and(|h| h.contains("kind: \"needs-you\"")));
    assert_eq!(stored(message_id).kind, None);
    // Marked, it waits on the human and needs no hint.
    let SendResult::Sent { kind, hint, .. } = send("Ship it?", None, Some(NeedsYou)).await else {
        panic!("not sent")
    };
    assert_eq!((kind, hint), (Some(NeedsYou), None));
    // A reply to the human echoes its lane too.
    let asked = sent_id(
        send_message(&host, None, "H1", "status?", None, None, None, None, None)
            .await
            .unwrap(),
    );
    let SendResult::Sent { kind, hint, .. } =
        reply_with_kind(&host, Some("S1"), asked, "Merge it too?", None, None)
            .await
            .unwrap()
    else {
        panic!("not sent")
    };
    assert_eq!(kind, Some(Update));
    assert!(hint.is_some());
    // Mail to another session says nothing of lanes.
    let SendResult::Sent { kind, hint, .. } =
        send_message(&host, Some("S1"), "H2", "hi?", None, None, None, None, None)
            .await
            .unwrap()
    else {
        panic!("not sent")
    };
    assert_eq!((kind, hint), (None, None));
}

/// SW-AGR-18: "Mark all read" reads 500 updates in one call, and says the inbox changed
/// once, not 500 times.
#[tokio::test]
async fn the_human_reads_many_messages_at_once_with_one_change_signal() {
    let fake = FakeHost::new();
    let host = port(&fake);
    let store = WeftStore::open(fake.team_store("default")).unwrap();
    let ids: Vec<i64> = (0..500)
        .map(|i| {
            inbox::inbox_insert(
                &store,
                "H1",
                HUMAN,
                &format!("report {i}"),
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap()
        })
        .collect();
    let before = *fake.changed.lock().unwrap();
    let changed = mark_read_many_by_human(&*host, &launch_team("default"), ids.clone())
        .await
        .unwrap();
    assert_eq!(changed, 500);
    assert_eq!(
        *fake.changed.lock().unwrap(),
        before + 1,
        "one inbox-changed signal"
    );
    assert_eq!(inbox::inbox_unread(&store, HUMAN).unwrap().count, 0);
    // Nothing left to read: no signal at all.
    assert_eq!(
        mark_read_many_by_human(&*host, &launch_team("default"), ids)
            .await
            .unwrap(),
        0
    );
    assert_eq!(*fake.changed.lock().unwrap(), before + 1);
}

#[tokio::test]
async fn the_app_acts_on_the_team_it_names() {
    let fake = FakeHost::new();
    let host = port(&fake);
    fake.teams
        .lock()
        .unwrap()
        .insert("H1".into(), "design".into());
    send_message(
        &host,
        Some("S2"),
        "human",
        "from default",
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let in_design = sent_id(
        send_message(
            &host,
            Some("S1"),
            "human",
            "from design",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    // The backend's active team does not decide which inbox the app sees.
    fake.switch_team(Some("default"));
    let design = launch_team("design");
    let view = list_messages(&*host, &design, super::InboxScope::ToHuman, None, &[])
        .await
        .unwrap();
    assert_eq!(view.messages.len(), 1);
    assert_eq!(view.messages[0].text, "from design");

    mark_read_by_human(&*host, &design, in_design)
        .await
        .unwrap();
    assert_eq!(
        list_messages(&*host, &design, super::InboxScope::ToHuman, None, &[])
            .await
            .unwrap()
            .human_unread,
        0
    );
    assert_eq!(
        list_messages(
            &*host,
            &launch_team("default"),
            super::InboxScope::ToHuman,
            None,
            &[]
        )
        .await
        .unwrap()
        .human_unread,
        1
    );

    // A reply from the design team's list goes to H1, in design's store.
    let reply_id = sent_id(
        reply_from_human(&host, &design, in_design, "thanks", None, None)
            .await
            .unwrap(),
    );
    let row = fake
        .rows_in("design")
        .into_iter()
        .find(|m| m.id == reply_id);
    let row = row.unwrap();
    assert_eq!(row.to, "H1");
    assert_eq!(
        row.in_reply_to_team,
        Some(InboxReplyTeam {
            org: ORG.into(),
            team: "design".into()
        })
    );
}

#[tokio::test]
async fn list_inbox_messages_returns_all_rows_without_marking_read() {
    let fake = FakeHost::new();
    let host = port(&fake);
    let asked = sent_id(
        send_message(
            &host,
            Some("S1"),
            "H2",
            "status?",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let to_human = sent_id(
        send_message(
            &host,
            Some("S2"),
            "human",
            "need you",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    assert_eq!(*fake.changed.lock().unwrap(), 2);

    // SW-BSI-UX: this test specifically verifies the "all recipients" axis, so it
    // reads with `InboxScope::All` to see both the human-addressed row and the
    // session-addressed row. The pane's default `ToHuman` scope is covered by the
    // scope-filter tests below.
    let view = list_messages(
        &*host,
        &launch_team("default"),
        super::InboxScope::All,
        None,
        &[],
    )
    .await
    .unwrap();
    let rows = &view.messages;
    assert_eq!(view.human_unread, 1);
    assert_eq!(
        rows.iter().map(|r| r.message_id).collect::<Vec<_>>(),
        vec![to_human, asked],
        "newest first"
    );
    let agent_row = &rows[1];
    assert_eq!(
        (
            agent_row.from_name.as_str(),
            agent_row.to_name.as_str(),
            agent_row.text.as_str()
        ),
        ("Manager", "Sprint 1", "status?")
    );
    assert!(!agent_row.to_human);
    assert!(agent_row.unread);
    assert!(rows[0].to_human && rows[0].unread);
    assert_eq!(rows[0].from_name, "Sprint 1");

    // Listing twice marks nothing read: H2 still gets its message, the human's stays
    // unread until the human reads it.
    list_messages(
        &*host,
        &launch_team("default"),
        super::InboxScope::ToHuman,
        None,
        &[],
    )
    .await
    .unwrap();
    assert_eq!(read_messages(&*host, Some("S2")).await.unwrap().len(), 1);
    assert_eq!(
        list_messages(
            &*host,
            &launch_team("default"),
            super::InboxScope::ToHuman,
            None,
            &[]
        )
        .await
        .unwrap()
        .human_unread,
        1
    );
}

#[tokio::test]
async fn human_marks_only_their_own_messages_read() {
    let fake = FakeHost::new();
    let host = port(&fake);
    let for_human = sent_id(
        send_message(
            &host,
            Some("S2"),
            "human",
            "need you",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let for_h2 = sent_id(
        send_message(
            &host,
            Some("S1"),
            "H2",
            "status?",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let changed = *fake.changed.lock().unwrap();

    mark_read_by_human(&*host, &launch_team("default"), for_h2)
        .await
        .unwrap();
    assert_eq!(*fake.changed.lock().unwrap(), changed, "nothing changed");
    mark_read_by_human(&*host, &launch_team("default"), for_human)
        .await
        .unwrap();
    assert_eq!(*fake.changed.lock().unwrap(), changed + 1);

    let view = list_messages(
        &*host,
        &launch_team("default"),
        super::InboxScope::ToHuman,
        None,
        &[],
    )
    .await
    .unwrap();
    assert_eq!(view.human_unread, 0);
    // H2's message is still unread for H2.
    assert_eq!(read_messages(&*host, Some("S2")).await.unwrap().len(), 1);
}

#[tokio::test]
async fn cross_team_reply_qualifies_colliding_and_self_parent_ids() {
    for seed_collision in [false, true] {
        let fake = FakeHost::new();
        let host = port(&fake);
        fake.teams
            .lock()
            .unwrap()
            .insert("H2".into(), "design".into());
        if seed_collision {
            let unrelated = sent_id(
                send_message(
                    &host,
                    None,
                    "H1",
                    "unrelated local row",
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .await
                .unwrap(),
            );
            assert_eq!(unrelated, 1);
        }
        let original = sent_id(
            send_message(
                &host,
                Some("S1"),
                "H2",
                "in design",
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap(),
        );
        assert_eq!(original, 1);
        // The selected UI team differs from the original message's owning team.
        fake.switch_team(Some("default"));
        let answer = sent_id(
            reply(&host, Some("S2"), original, "back to default", None)
                .await
                .unwrap(),
        );
        assert_eq!(answer, if seed_collision { 2 } else { 1 });
        let rows = read_messages(&*host, Some("S1")).await.unwrap();
        let row = rows.iter().find(|m| m.message_id == answer).unwrap();
        assert_eq!(row.in_reply_to, Some(original));
        assert_eq!(
            row.in_reply_to_team,
            Some(InboxReplyTeam {
                org: ORG.into(),
                team: "design".into()
            })
        );
        let wire = serde_json::to_value(row).unwrap();
        assert_eq!(wire["inReplyTo"], original);
        assert_eq!(
            wire["inReplyToTeam"],
            serde_json::json!({"org": ORG, "team": "design"})
        );
        // SW-BSI-UX: scope `All` because the reply here lives in the agent-recipient
        // axis (sender S2 → recipient S1); the ToHuman filter would hide it.
        let view = list_messages(
            &*host,
            &launch_team("default"),
            super::InboxScope::All,
            None,
            &[],
        )
        .await
        .unwrap();
        assert_eq!(
            view.messages
                .iter()
                .find(|m| m.message_id == answer)
                .unwrap()
                .in_reply_to_team,
            row.in_reply_to_team
        );
        assert!(fake.rows_in("design")[0].read_at_ms.is_some());
    }
}

#[tokio::test]
async fn failed_cross_team_reply_does_not_mark_original_read() {
    let fake = FakeHost::new();
    let host = port(&fake);
    fake.teams
        .lock()
        .unwrap()
        .insert("H2".into(), "design".into());
    let original = sent_id(
        send_message(
            &host,
            Some("S1"),
            "H2",
            "keep unread until delivered",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    // A file blocks the destination directory; this is a real storage error.
    let blocker = fake.dir.path().join("default");
    std::fs::write(&blocker, "not a directory").unwrap();
    assert!(reply(&host, Some("S2"), original, "cannot land yet", None)
        .await
        .is_err());
    assert_eq!(fake.rows_in("design")[0].read_at_ms, None);
    std::fs::remove_file(blocker).unwrap();
    sent_id(
        reply(&host, Some("S2"), original, "delivered now", None)
            .await
            .unwrap(),
    );
    assert!(fake.rows_in("design")[0].read_at_ms.is_some());
    assert_eq!(fake.rows().len(), 1);
}

/// SW-BSI-3: a message with an attached file whose owning story is known
/// surfaces an [`AttachmentHint`] on `read_messages`. A remote agent reads
/// `suggested_call: "hydrate_story"` + `owning_story_id` and can
/// self-recover without a round-trip through the human. A message without
/// an attachment, or one whose path has no linked owner, keeps the hint
/// `None` — the hint is advisory, never a false positive.
#[tokio::test]
async fn read_messages_surfaces_the_attachment_hint_for_a_known_owner() {
    let fake = FakeHost::new();
    let host = port(&fake);

    // One message with an attachment under story SPEC-7's subtree, one plain.
    fake.claim_owner("default", "docs/spec.md", "SPEC-7");
    send_message(
        &host,
        None,
        "H2",
        "please review the spec",
        Some("docs/spec.md"),
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    send_message(&host, None, "H2", "ping", None, None, None, None, None)
        .await
        .unwrap();

    let read = read_messages(&*host, Some("S2")).await.unwrap();
    assert_eq!(read.len(), 2);
    assert_eq!(read[0].primary_file.as_deref(), Some("docs/spec.md"));
    let hint = read[0].attachment_hint.as_ref().expect("hint attached");
    assert_eq!(hint.owning_story_id, "SPEC-7");
    assert_eq!(hint.suggested_call, "hydrate_story");
    assert!(hint.note.contains("SPEC-7"));
    // The plain message has neither field.
    assert!(read[1].primary_file.is_none());
    assert!(read[1].attachment_hint.is_none());
}

/// SW-BSI-3: a message whose attachment path is not under any linked
/// worktree record leaves `attachment_hint` empty — a UI falls through to
/// its generic unavailable rendering, and a remote agent gets no false
/// hydration call to try.
#[tokio::test]
async fn read_messages_omits_the_hint_when_the_owner_is_unknown() {
    let fake = FakeHost::new();
    let host = port(&fake);

    send_message(
        &host,
        None,
        "H2",
        "look at this",
        Some("stray/file.md"),
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    let read = read_messages(&*host, Some("S2")).await.unwrap();
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].primary_file.as_deref(), Some("stray/file.md"));
    assert!(read[0].attachment_hint.is_none());
}

#[tokio::test]
async fn list_inbox_messages_narrows_to_one_agents_conversation() {
    let fake = FakeHost::new();
    let host = port(&fake);
    let path = fake.team_store("default");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let store = crate::weft::WeftStore::open(path).unwrap();
    let insert = |from: &str, to: &str, text: &str, reply: Option<i64>| {
        inbox::inbox_insert(&store, from, to, text, reply, None, None, None, None).unwrap()
    };
    let asked = insert(HUMAN, "triage", "Look at the digest bug", None);
    let answer = insert("SW-1#triage", HUMAN, "On it", Some(asked));
    let follow_up = insert(HUMAN, "SW-1#triage", "Thanks", Some(answer));
    let other = insert(HUMAN, "scribe", "Write the standup", None);
    let as_role = insert("triage", HUMAN, "Filed BUG-9", None);
    insert("SW-2#scribe", HUMAN, "Done", Some(other));

    let view = list_messages(
        &*host,
        &launch_team("default"),
        super::InboxScope::All,
        Some("triage"),
        &[],
    )
    .await
    .unwrap();
    assert_eq!(
        view.messages
            .iter()
            .map(|r| r.message_id)
            .collect::<Vec<_>>(),
        vec![as_role, follow_up, answer, asked],
        "the role's own rows and every reply under them, newest first"
    );

    // The scope still applies on top of the agent lens.
    let to_agent = list_messages(
        &*host,
        &launch_team("default"),
        super::InboxScope::ToAgents,
        Some("triage"),
        &[],
    )
    .await
    .unwrap();
    assert_eq!(
        to_agent
            .messages
            .iter()
            .map(|r| r.message_id)
            .collect::<Vec<_>>(),
        vec![follow_up, asked]
    );

    // Blank or absent: the whole inbox, as before.
    let all = list_messages(
        &*host,
        &launch_team("default"),
        super::InboxScope::All,
        Some(" "),
        &[],
    )
    .await
    .unwrap();
    assert_eq!(all.messages.len(), 6);
}

#[tokio::test]
async fn the_human_sees_delivery_state_and_withdraws_a_waiting_message() {
    use crate::weft::inbox::InboxDeliveryState;
    use crate::weft::role_inbox::{
        claim_role_messages, register_role, send_role_message_commit, WithdrawOutcome,
    };
    let fake = FakeHost::new();
    let host = port(&fake);
    let team = launch_team("default");
    let path = fake.team_store("default");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let store = crate::weft::WeftStore::open(path).unwrap();
    let send = |text: &str| {
        send_role_message_commit(
            &store, HUMAN, "triage", text, None, None, None, text, 1, None, None, None,
        )
        .unwrap()
        .0
    };
    let waiting = send("Look at the digest bug");
    let taken = send("And the flaky test");
    let reg = register_role(&store, "triage", "sess-1", 1).unwrap();
    // Oldest first: the claim takes `waiting`; claim again only after withdrawing.
    let first = claim_role_messages(&store, "triage", "sess-1", reg.generation, 1, 1).unwrap();
    assert_eq!(first[0].message_id, waiting);

    let state_of = |view: &InboxView, id: i64| {
        view.messages
            .iter()
            .find(|r| r.message_id == id)
            .and_then(|r| r.delivery.as_ref())
            .map(|d| d.state)
    };
    let view = list_messages(&*host, &team, InboxScope::All, Some("triage"), &[])
        .await
        .unwrap();
    assert_eq!(state_of(&view, waiting), Some(InboxDeliveryState::Reading));
    assert_eq!(state_of(&view, taken), Some(InboxDeliveryState::Queued));

    let before = *fake.changed.lock().unwrap();
    assert!(matches!(
        withdraw_by_human(&*host, &team, taken).await.unwrap(),
        WithdrawOutcome::Withdrawn { .. }
    ));
    assert_eq!(
        *fake.changed.lock().unwrap(),
        before + 1,
        "a withdrawal refreshes the list"
    );
    assert_eq!(
        withdraw_by_human(&*host, &team, waiting).await.unwrap(),
        WithdrawOutcome::TooLate {
            state: InboxDeliveryState::Reading
        }
    );
    // Repeating a withdrawal changes nothing and announces nothing.
    assert!(matches!(
        withdraw_by_human(&*host, &team, taken).await.unwrap(),
        WithdrawOutcome::Withdrawn { .. }
    ));
    assert_eq!(*fake.changed.lock().unwrap(), before + 1);

    let view = list_messages(&*host, &team, InboxScope::All, Some("triage"), &[])
        .await
        .unwrap();
    assert_eq!(state_of(&view, taken), Some(InboxDeliveryState::Withdrawn));
    // The withdrawn message is not claimable.
    assert!(
        claim_role_messages(&store, "triage", "sess-1", reg.generation, 10, 1)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn an_agents_conversation_shows_its_instances_own_mail() {
    let fake = FakeHost::new();
    let host = port(&fake);
    let first = sent_id(
        send_message(
            &host,
            None,
            "H2",
            "Fix the flaky test",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let team = launch_team("default");
    // The panel of the agent H2 runs lists its instances' handles beside its role.
    let view = list_messages(
        &*host,
        &team,
        InboxScope::All,
        Some("triage"),
        &["H2".into()],
    )
    .await
    .unwrap();
    assert_eq!(
        view.messages
            .iter()
            .map(|m| m.message_id)
            .collect::<Vec<_>>(),
        vec![first]
    );
    assert_eq!(view.messages[0].to, "H2");
    // Another agent's conversation does not show it.
    let other = list_messages(&*host, &team, InboxScope::All, Some("triage"), &[])
        .await
        .unwrap();
    assert!(other.messages.is_empty());
}

#[tokio::test]
async fn a_turn_end_announces_each_batch_of_new_mail_once() {
    let fake = FakeHost::new();
    let host = port(&fake);
    let stop = |session: &'static str| {
        let host = host.clone();
        async move { hook_notice(&*host, session).await }
    };
    // No mail: the agent stops.
    assert_eq!(stop("S2").await, None);

    let body = "the secret body";
    send_message(&host, Some("S1"), "H2", body, None, None, None, None, None)
        .await
        .unwrap();
    let notice = stop("S2").await.unwrap();
    assert_eq!(
        notice,
        "You have 1 new message. Call read_messages with session_id S2."
    );
    assert!(!notice.contains(body), "never the body");
    // It stops again without reading (Claude's stop_hook_active turn): not told again.
    assert_eq!(stop("S2").await, None);

    // More mail: told once more, with every unread message counted.
    send_message(&host, None, "H2", body, None, None, None, None, None)
        .await
        .unwrap();
    assert_eq!(
        stop("S2").await.as_deref(),
        Some("You have 2 new messages. Call read_messages with session_id S2.")
    );
    // Once it reads them, nothing is left to tell.
    assert_eq!(read_messages(&*host, Some("S2")).await.unwrap().len(), 2);
    assert_eq!(stop("S2").await, None);

    // A session with no handle has no inbox; nothing is marked read by announcing.
    assert_eq!(stop("plain-tab").await, None);
    send_message(&host, Some("S2"), "H1", body, None, None, None, None, None)
        .await
        .unwrap();
    assert!(stop("S1").await.is_some());
    assert_eq!(read_messages(&*host, Some("S1")).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_notice_is_told_only_once_its_host_confirms_it() {
    let fake = FakeHost::new();
    let host = port(&fake);
    send_hello(&host).await;
    // The notice is offered, but its confirm never arrives (its host never handed it
    // over): the mail is not marked told.
    let Ok(Check::Offer { notice, token }) = check_for_mail(&*host, "S2", false).await else {
        panic!("no notice offered");
    };
    assert!(notice.starts_with("You have 1 new message"));
    let unread = |fake: &FakeHost| {
        let store = WeftStore::open(fake.team_store("default")).unwrap();
        inbox::inbox_unread_for(&store, &["H2".to_string()]).unwrap()["H2"].clone()
    };
    assert_eq!(unread(&fake).announced, None);
    // While its confirm may still be on the way, the next check offers nothing new.
    assert!(matches!(
        check_for_mail(&*host, "S2", false).await,
        Ok(Check::PendingFor(wait)) if wait > std::time::Duration::from_secs(50)
    ));
    // Its confirm marks it told, once.
    assert!(confirm_notice(&*host, "S2", &token).await.unwrap());
    assert!(!confirm_notice(&*host, "S2", &token).await.unwrap());
    assert!(unread(&fake).announced.is_some());
    assert!(matches!(
        check_for_mail(&*host, "S2", false).await,
        Ok(Check::Nothing)
    ));
}

#[tokio::test]
async fn an_offer_awaiting_its_confirm_is_offered_again_once_it_lapses() {
    let fake = FakeHost::new();
    let host = port(&fake);
    send_hello(&host).await;
    // A notice was offered almost a reservation's TTL ago and never confirmed.
    let store = WeftStore::open(fake.team_store("default")).unwrap();
    let offered = inbox::now_ms() - inbox::RESERVE_TTL_MS + 300;
    let inbox::Reservation::Offer { token, .. } =
        inbox::inbox_reserve_announcement_at(&store, "H2", "S2", offered, inbox::this_run(), false)
            .unwrap()
    else {
        panic!("no offer");
    };
    // A check is told when to look again, and then is offered the same notice.
    let Ok(Check::PendingFor(wait)) = check_for_mail(&*host, "S2", false).await else {
        panic!("pending");
    };
    assert!(wait < std::time::Duration::from_secs(1));
    tokio::time::sleep(wait).await;
    assert!(matches!(
        check_for_mail(&*host, "S2", false).await,
        Ok(Check::Offer { token: again, .. }) if again == token
    ));
}

#[tokio::test]
async fn a_released_notice_is_offered_again_at_once() {
    let fake = FakeHost::new();
    let host = port(&fake);
    send_hello(&host).await;
    let store = WeftStore::open(fake.team_store("default")).unwrap();
    let inbox::Reservation::Offer { token, .. } =
        inbox::inbox_reserve_announcement(&store, "H2", "S2", false, None).unwrap()
    else {
        panic!("no offer");
    };
    release_notice(&*host, "S2", &token).await;
    assert!(matches!(
        check_for_mail(&*host, "S2", false).await,
        Ok(Check::Offer { token: again, .. }) if again == token
    ));
}

#[tokio::test]
async fn an_uncertain_notice_is_held_until_its_confirm_and_never_offered_again_blind() {
    let fake = FakeHost::new();
    let host = port(&fake);
    send_hello(&host).await;
    let Ok(Check::Offer { token, .. }) = check_for_mail(&*host, "S2", false).await else {
        panic!("no notice offered");
    };
    hold_notice(&*host, "S2", &token).await;
    // Not offered again, not even when its host asks for what it held.
    for reoffer in [false, true] {
        assert!(matches!(
            check_for_mail(&*host, "S2", reoffer).await,
            Ok(Check::Held)
        ));
    }
    // Settled: told.
    assert!(confirm_notice(&*host, "S2", &token).await.unwrap());
    assert!(matches!(
        check_for_mail(&*host, "S2", false).await,
        Ok(Check::Nothing)
    ));
}

#[tokio::test]
async fn each_fate_a_host_reports_settles_the_reservation_through_the_shared_store() {
    use crate::commands::host_inbox::{settle, Fate};
    let fake = FakeHost::new();
    let host = port(&fake);
    send_hello(&host).await;
    let offer = || async {
        match check_for_mail(&*host, "S2", false).await {
            Ok(Check::Offer { token, .. }) => Some(token),
            _ => None,
        }
    };
    let token = offer().await.expect("offered");
    // Refused with no effect: offered again at once, under its own token.
    settle(&*host, "S2", &token, Fate::Released).await;
    assert_eq!(offer().await.as_deref(), Some(token.as_str()));
    // Uncertain: held, never offered again blind.
    settle(&*host, "S2", &token, Fate::Uncertain).await;
    assert!(matches!(
        check_for_mail(&*host, "S2", true).await,
        Ok(Check::Held)
    ));
    // Told: marked told, nothing left to offer.
    settle(&*host, "S2", &token, Fate::Told).await;
    assert!(matches!(
        check_for_mail(&*host, "S2", false).await,
        Ok(Check::Nothing)
    ));
}

#[tokio::test]
async fn mail_for_an_agent_that_never_rearms_stays_visible_unread_and_pending() {
    use crate::commands::dashboard_launch::{session_row, DashboardLaunchState};
    use crate::mcp::session_list::{session_list_rows_with_tabs, with_unread};
    let fake = FakeHost::new();
    let host = port(&fake);
    let id = sent_id(
        send_message(
            &host,
            Some("S1"),
            "H2",
            "waiting for your next arm",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let launch = DashboardLaunchState::new(2);
    launch.register("H2", "S2", Some("codex"));
    // No arm or read follows the send. Repeated human views remain observational.
    for _ in 0..2 {
        let unread = unread_by_handle(&*host, &["H2".to_string()]).await;
        let rows = launch
            .snapshot()
            .into_iter()
            .map(|entry| session_row(entry, true))
            .collect();
        let mut listed = session_list_rows_with_tabs(rows, |_| None, None, |_| None);
        with_unread(&mut listed, &unread);
        let row = serde_json::to_value(&listed[0]).unwrap();
        assert_eq!(row["unread"], 1);
        assert_eq!(row["mailNotice"], "pending");
        assert_eq!(row["inboxWaiter"], false);
        assert!(row["notifiedAt"].is_null());
        let view = crate::commands::agent_read_model::session_inbox(&*host, "S2")
            .await
            .unwrap();
        assert_eq!(view.messages.len(), 1);
        assert_eq!(view.messages[0].id, id);
        assert!(view.messages[0].unread);
    }
}

#[tokio::test]
async fn mail_waiting_when_an_agent_arms_is_announced_once() {
    let fake = FakeHost::new();
    let host = port(&fake);
    send_message(&host, Some("S1"), "H2", "one", None, None, None, None, None)
        .await
        .unwrap();
    send_message(&host, Some("S1"), "H2", "two", None, None, None, None, None)
        .await
        .unwrap();
    // The arm at its boot hands over the mail it was never told about.
    assert_eq!(
        hook_notice(&*host, "S2").await.as_deref(),
        Some("You have 2 new messages. Call read_messages with session_id S2.")
    );
    // A context restart (resume, clear, compact) arms again: mail it was told about and
    // left unread does not wake it.
    assert_eq!(hook_notice(&*host, "S2").await, None);
    // Announcing marks nothing read.
    assert_eq!(read_messages(&*host, Some("S2")).await.unwrap().len(), 2);
}

#[tokio::test]
async fn mail_stored_for_a_session_is_offered_on_but_mail_to_the_human_is_not() {
    let host = FakeHost::new();
    let port = port(&host);
    let asked = sent_id(
        send_message(
            &port,
            Some("S1"),
            "H2",
            "status?",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    sent_id(
        send_message(
            &port,
            Some("S1"),
            "human",
            "fyi",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    sent_id(reply(&port, Some("S2"), asked, "done", None).await.unwrap());
    assert_eq!(
        *host.stored_for.lock().unwrap(),
        vec!["H2".to_string(), "H1".to_string()]
    );
    // A refused send stores nothing and offers nothing.
    send_message(
        &port,
        Some("S1"),
        "nobody",
        "hi",
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(host.stored_for.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn unread_by_handle_counts_each_sessions_own_mail_in_its_team_and_marks_nothing_read() {
    let host = FakeHost::new();
    let port = port(&host);
    send_message(&port, Some("S1"), "H2", "one", None, None, None, None, None)
        .await
        .unwrap();
    let second = sent_id(
        send_message(&port, None, "H2", "two", None, None, None, None, None)
            .await
            .unwrap(),
    );
    // H1 lives in team design: its mail is counted there.
    host.teams
        .lock()
        .unwrap()
        .insert("H1".into(), "design".into());
    let to_h1 = sent_id(
        send_message(
            &port,
            Some("S2"),
            "H1",
            "over there",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    // Role-inbox mail (to an agent definition id) is not a session's.
    sent_id(
        store_message(
            &port,
            InboxLocation {
                team: LaunchTeam {
                    org: "acme".into(),
                    team: "default".into(),
                },
                path: host.team_store("default"),
            },
            "human".into(),
            "triage".into(),
            "role mail",
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );

    let handles = ["H1", "H2", "H3"].map(String::from);
    let read = unread_by_handle(&*port, &handles).await;
    assert!(read.failed.is_empty(), "{:?}", read.failed);
    let unread = read.found;
    let h2 = &unread["H2"];
    assert_eq!((h2.count, h2.newest_id), (2, Some(second)));
    assert_eq!(h2.team, launch_team("default"));
    assert!(h2.oldest_at_ms.is_some());
    let h1 = &unread["H1"];
    assert_eq!((h1.count, h1.newest_id), (1, Some(to_h1)));
    assert_eq!(h1.team, launch_team("design"));
    assert!(!unread.contains_key("H3"));
    assert!(!unread.contains_key("triage"));

    // Counting is a view: the mail is still there to read.
    assert_eq!(read_messages(&*port, Some("S2")).await.unwrap().len(), 2);
    assert!(!unread_by_handle(&*port, &handles)
        .await
        .found
        .contains_key("H2"));

    // A session whose team is not synced here is unknown, not empty.
    host.teams
        .lock()
        .unwrap()
        .insert("H2".into(), "gone".into());
    let read = unread_by_handle(&*port, &handles).await;
    assert!(read.failed.contains("H2"));
    assert!(!read.failed.contains("H1"));
}

#[tokio::test]
async fn a_tab_opened_by_hand_sends_as_itself_under_its_current_tab_name() {
    let fake = FakeHost::new();
    let host = port(&fake);
    // A person opened a tab and started Claude in it: no launch bound it to a handle,
    // so it is addressed by its session id, as list_sessions lists it.
    fake.tab("pty-hand", "pty-hand", "Terminal 3", true);
    let sent = sent_id(
        send_message(
            &host,
            Some("pty-hand"),
            "H1",
            "hello",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let row = fake.rows().into_iter().find(|m| m.id == sent).unwrap();
    assert_eq!(
        row.from, "pty-hand",
        "recorded as its session, not the human"
    );
    let read = read_messages(&*host, Some("S1")).await.unwrap();
    assert_eq!(
        (read[0].from.as_str(), read[0].from_name.as_str()),
        ("pty-hand", "Terminal 3")
    );
    // A rename follows it, in the human's list too.
    fake.rename("pty-hand", "Shipper");
    let view = list_messages(
        &*host,
        &launch_team("default"),
        super::InboxScope::All,
        None,
        &[],
    )
    .await
    .unwrap();
    let listed = view.messages.iter().find(|m| m.message_id == sent).unwrap();
    assert_eq!(
        (listed.from_name.as_str(), listed.to_name.as_str()),
        ("Shipper", "Manager")
    );
    // It reads and answers its own mail, and is told about it by its hooks.
    let asked = sent_id(
        send_message(
            &host,
            Some("S1"),
            "pty-hand",
            "status?",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    assert!(hook_notice(&*host, "pty-hand").await.is_some());
    let mine = read_messages(&*host, Some("pty-hand")).await.unwrap();
    assert_eq!((mine.len(), mine[0].message_id), (1, asked));
    let answered = sent_id(
        reply(&host, Some("pty-hand"), asked, "done", None)
            .await
            .unwrap(),
    );
    let row = fake.rows().into_iter().find(|m| m.id == answered).unwrap();
    assert_eq!((row.from.as_str(), row.to.as_str()), ("pty-hand", "H1"));
}

#[tokio::test]
async fn a_launched_sessions_name_is_its_tabs_current_name() {
    let fake = FakeHost::new();
    let host = port(&fake);
    send_message(&host, Some("S1"), "H2", "one", None, None, None, None, None)
        .await
        .unwrap();
    fake.rename("H1", "Lead");
    let read = read_messages(&*host, Some("S2")).await.unwrap();
    assert_eq!(read[0].from_name, "Lead");
}

#[tokio::test]
async fn any_running_session_may_be_named_as_the_sender() {
    let fake = FakeHost::new();
    let host = port(&fake);
    fake.tab("pty-hand", "pty-hand", "Terminal 3", true);
    // Whoever calls may send as another running session, by its session id or handle.
    for (as_session, recorded) in [("S2", "H2"), ("H2", "H2"), ("pty-hand", "pty-hand")] {
        let sent = sent_id(
            send_message(
                &host,
                Some(as_session),
                "H1",
                "hi",
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap(),
        );
        let row = fake.rows().into_iter().find(|m| m.id == sent).unwrap();
        assert_eq!(row.from, recorded, "{as_session}");
    }
}

#[tokio::test]
async fn a_sender_that_is_not_running_is_refused_never_recorded_as_the_human() {
    let fake = FakeHost::new();
    let host = port(&fake);
    fake.tab("pty-dead", "pty-dead", "Old tab", false);
    let before = fake.rows().len();
    for sender in ["pty-dead", "made-up-id"] {
        let result = send_message(
            &host,
            Some(sender),
            "H1",
            "hi",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        let SendResult::Invalid { message } = result else {
            panic!("{sender}: expected invalid");
        };
        assert!(message.contains(sender), "{message}");
        assert!(message.contains("not running") || message.contains("is running"));
    }
    assert_eq!(fake.rows().len(), before, "nothing stored");
    // A launched session whose tab ended is refused too.
    fake.stop("H2");
    assert!(is_invalid(
        &send_message(&host, Some("S2"), "H1", "hi", None, None, None, None, None)
            .await
            .unwrap()
    ));
    // Naming no session sends as the human.
    let sent = sent_id(
        send_message(&host, Some("  "), "H1", "hi", None, None, None, None, None)
            .await
            .unwrap(),
    );
    let row = fake.rows().into_iter().find(|m| m.id == sent).unwrap();
    assert_eq!(row.from, HUMAN);
}

#[tokio::test]
async fn a_remote_tab_sends_reads_and_is_addressed_by_its_handle_or_remote_session() {
    let fake = FakeHost::new();
    let host = port(&fake);
    fake.tab("remote-tab-7", "remote-shell-7", "build box", true);
    // It sends as itself, by its remote session id ($CODESENSE_TAB_SESSION_ID there).
    let sent = sent_id(
        send_message(
            &host,
            Some("remote-shell-7"),
            "H1",
            "built",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let read = read_messages(&*host, Some("S1")).await.unwrap();
    assert_eq!(
        (
            read[0].message_id,
            read[0].from.as_str(),
            read[0].from_name.as_str()
        ),
        (sent, "remote-tab-7", "build box")
    );
    // It may send as another running session, local or remote.
    fake.tab("remote-tab-8", "remote-shell-8", "test box", true);
    for (as_session, recorded) in [("S2", "H2"), ("remote-shell-8", "remote-tab-8")] {
        let sent = sent_id(
            send_message(
                &host,
                Some(as_session),
                "H1",
                "hi",
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap(),
        );
        let row = fake.rows().into_iter().find(|m| m.id == sent).unwrap();
        assert_eq!(row.from, recorded);
    }
    // It is a recipient by its handle or its remote session, and reads its mail.
    for to in ["remote-tab-7", "remote-shell-7"] {
        send_message(&host, Some("S1"), to, "next", None, None, None, None, None)
            .await
            .unwrap();
    }
    let mine = read_messages(&*host, Some("remote-shell-7")).await.unwrap();
    assert_eq!(mine.len(), 2);
    assert!(mine.iter().all(|m| m.from_name == "Manager"));
    let answered = sent_id(
        reply(&host, Some("remote-tab-7"), mine[0].message_id, "ok", None)
            .await
            .unwrap(),
    );
    let row = fake.rows().into_iter().find(|m| m.id == answered).unwrap();
    assert_eq!((row.from.as_str(), row.to.as_str()), ("remote-tab-7", "H1"));
    // Its machine went offline: it no longer sends, but its past mail keeps its name.
    fake.stop("remote-tab-7");
    assert!(is_invalid(
        &send_message(
            &host,
            Some("remote-shell-7"),
            "H1",
            "hi",
            None,
            None,
            None,
            None,
            None
        )
        .await
        .unwrap()
    ));
    let view = list_messages(
        &*host,
        &launch_team("default"),
        super::InboxScope::All,
        None,
        &[],
    )
    .await
    .unwrap();
    let listed = view.messages.iter().find(|m| m.message_id == sent).unwrap();
    assert_eq!(listed.from_name, "build box");
}

/// Every role delivery in `team`'s store, as `(message_id, role, state)`.
fn role_deliveries(fake: &FakeHost, team: &str) -> Vec<(i64, String, String)> {
    let store = WeftStore::open(fake.team_store(team)).unwrap();
    let conn = store.conn().unwrap();
    let mut stmt = conn
        .prepare("SELECT message_id, role, state FROM wv_role_delivery ORDER BY delivery_id")
        .unwrap();
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap();
    rows.collect::<rusqlite::Result<Vec<_>>>().unwrap()
}

#[tokio::test]
async fn a_human_reply_goes_only_to_the_sender_and_queues_no_role_delivery() {
    let fake = FakeHost::new();
    let host = port(&fake);
    let asked = sent_id(
        send_message(
            &host,
            Some("S2"),
            "human",
            "ok to merge?",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let answer = sent_id(
        reply_from_human(&host, &launch_team("default"), asked, "yes", None, None)
            .await
            .unwrap(),
    );
    let row = fake.rows().into_iter().find(|m| m.id == answer).unwrap();
    assert_eq!((row.from.as_str(), row.to.as_str()), (HUMAN, "H2"));
    assert_eq!(row.in_reply_to, Some(asked));
    assert!(
        role_deliveries(&fake, "default").is_empty(),
        "a human reply is not copied to any role"
    );
}

#[tokio::test]
async fn reply_preview_and_delivery_share_by_id_lookup_beyond_recent_500() {
    let fake = FakeHost::new();
    let host = port(&fake);
    let asked = sent_id(
        send_message(
            &host,
            Some("S2"),
            "human",
            "An older request",
            None,
            None,
            None,
            Some("role:manager"),
            None,
        )
        .await
        .unwrap(),
    );
    for _ in 0..500 {
        send_message(
            &host,
            Some("S2"),
            "human",
            "Newer mail",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
    }
    let recent = fake.rows();
    assert_eq!(recent.len(), 500);
    assert!(recent.iter().all(|message| message.id != asked));
    let path = fake.team_store("default");
    assert_eq!(
        stored_reply_recipient(path.clone(), asked)
            .await
            .unwrap()
            .as_deref(),
        Some("role:manager"),
    );
    let store = WeftStore::open(&path).unwrap();
    assert!(inbox::inbox_get(&store, asked)
        .unwrap()
        .unwrap()
        .read_at_ms
        .is_none());
    let answer = sent_id(
        reply_from_human(&host, &launch_team("default"), asked, "Answer", None, None)
            .await
            .unwrap(),
    );
    let delivered = inbox::inbox_get(&store, answer).unwrap().unwrap();
    assert_eq!(
        (delivered.from.as_str(), delivered.to.as_str()),
        (HUMAN, "manager")
    );
    assert_eq!(delivered.in_reply_to, Some(asked));
    assert_eq!(
        role_deliveries(&fake, "default"),
        vec![(answer, "manager".to_string(), "pending".to_string())],
    );
    assert!(inbox::inbox_get(&store, asked)
        .unwrap()
        .unwrap()
        .read_at_ms
        .is_some());
    assert_eq!(
        stored_reply_recipient(path, answer + 1).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn a_reply_carries_its_summary_to_a_session_and_to_a_role_queue() {
    let fake = FakeHost::new();
    let host = port(&fake);
    // An agent's reply (MCP `reply`) to a session.
    let asked = sent_id(
        send_message(
            &host,
            Some("S1"),
            "H2",
            "status?",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let answered = sent_id(
        reply(
            &host,
            Some("S2"),
            asked,
            "## Long\n\nreport",
            Some("All green"),
        )
        .await
        .unwrap(),
    );
    let row = fake.rows().into_iter().find(|m| m.id == answered).unwrap();
    assert_eq!(row.summary.as_deref(), Some("All green"));
    let read = read_messages(&*host, Some("S1")).await.unwrap();
    let got = read.iter().find(|m| m.message_id == answered).unwrap();
    assert_eq!(got.summary.as_deref(), Some("All green"));

    // The human's reply to a message whose replies go to a role is queued for that
    // role with its summary.
    let to_human = sent_id(
        send_message(
            &host,
            Some("S2"),
            "human",
            "which branch?",
            None,
            None,
            None,
            Some("role:manager"),
            None,
        )
        .await
        .unwrap(),
    );
    let queued = sent_id(
        reply_from_human(
            &host,
            &launch_team("default"),
            to_human,
            "<p>main</p>",
            Some(inbox::MessageContentType::Html),
            Some(" Use\nmain "),
        )
        .await
        .unwrap(),
    );
    let row = fake.rows().into_iter().find(|m| m.id == queued).unwrap();
    assert_eq!((row.from.as_str(), row.to.as_str()), (HUMAN, "manager"));
    assert_eq!(row.summary.as_deref(), Some("Use main"));
    assert_eq!(
        role_deliveries(&fake, "default"),
        vec![(queued, "manager".to_string(), "pending".to_string())]
    );
}

#[tokio::test]
async fn reply_to_a_role_queues_the_answer_for_that_role_after_the_sender_exited() {
    let fake = FakeHost::new();
    let host = port(&fake);
    // A worker asks the human, with answers going to the manager role, and exits.
    let asked = sent_id(
        send_message(
            &host,
            Some("S2"),
            "human",
            "which branch?",
            None,
            None,
            None,
            Some(" role:manager "),
            None,
        )
        .await
        .unwrap(),
    );
    fake.stop("H2");
    assert_eq!(
        fake.rows()
            .into_iter()
            .find(|m| m.id == asked)
            .unwrap()
            .reply_to
            .as_deref(),
        Some("role:manager")
    );

    let answer = sent_id(
        reply_from_human(&host, &launch_team("default"), asked, "main", None, None)
            .await
            .unwrap(),
    );
    let row = fake.rows().into_iter().find(|m| m.id == answer).unwrap();
    assert_eq!((row.from.as_str(), row.to.as_str()), (HUMAN, "manager"));
    assert_eq!(row.in_reply_to, Some(asked));
    assert_eq!(
        role_deliveries(&fake, "default"),
        vec![(answer, "manager".to_string(), "pending".to_string())],
        "the answer waits in the manager's queue, not the dead worker's inbox"
    );
    assert!(
        read_messages(&*host, Some("S2")).await.unwrap().is_empty(),
        "nothing goes to the sender"
    );
    // The manager's next instance claims it.
    let store = WeftStore::open(fake.team_store("default")).unwrap();
    let reg = crate::weft::role_inbox::register_role(&store, "manager", "H1", 1_000).unwrap();
    let claimed = crate::weft::role_inbox::claim_role_messages(
        &store,
        "manager",
        "H1",
        reg.generation,
        10,
        1_000,
    )
    .unwrap();
    assert_eq!(
        claimed.iter().map(|c| c.message_id).collect::<Vec<_>>(),
        vec![answer]
    );

    // An agent's reply honours it too.
    let to_h1 = sent_id(
        send_message(
            &host,
            None,
            "H1",
            "status?",
            None,
            None,
            None,
            Some("role:triage"),
            None,
        )
        .await
        .unwrap(),
    );
    let agent_answer = sent_id(
        reply(&host, Some("S1"), to_h1, "green", None)
            .await
            .unwrap(),
    );
    assert!(role_deliveries(&fake, "default").contains(&(
        agent_answer,
        "triage".to_string(),
        "pending".to_string()
    )));
    assert_eq!(fake.rows_in("default")[0].from, "H1");
}

#[tokio::test]
async fn reply_to_a_session_sends_the_answer_to_that_session_only() {
    let fake = FakeHost::new();
    let host = port(&fake);
    // H2 asks the human; answers go to H1 (named by its session id).
    let asked = sent_id(
        send_message(
            &host,
            Some("S2"),
            "human",
            "pick one",
            None,
            None,
            None,
            Some("S1"),
            None,
        )
        .await
        .unwrap(),
    );
    assert_eq!(fake.rows()[0].reply_to.as_deref(), Some("H1"));
    let answer = sent_id(
        reply_from_human(
            &host,
            &launch_team("default"),
            asked,
            "the first",
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let got = read_messages(&*host, Some("S1")).await.unwrap();
    assert_eq!(
        got.iter().map(|m| m.message_id).collect::<Vec<_>>(),
        vec![answer]
    );
    assert!(read_messages(&*host, Some("S2")).await.unwrap().is_empty());
    assert!(role_deliveries(&fake, "default").is_empty());

    // An agent reads the replyTo of a message it gets.
    let to_h2 = sent_id(
        send_message(
            &host,
            None,
            "H2",
            "do it",
            None,
            None,
            None,
            Some("human"),
            None,
        )
        .await
        .unwrap(),
    );
    let read = read_messages(&*host, Some("S2")).await.unwrap();
    let wire = serde_json::to_value(read.iter().find(|m| m.message_id == to_h2).unwrap()).unwrap();
    assert_eq!(wire["replyTo"], "human");
}

#[tokio::test]
async fn a_reply_to_that_names_no_running_session_or_role_is_refused() {
    let fake = FakeHost::new();
    let host = port(&fake);
    fake.stop("H1");
    for reply_to in ["H1", "nobody", "role:", "role:  "] {
        let result = send_message(
            &host,
            Some("S2"),
            "human",
            "q",
            None,
            None,
            None,
            Some(reply_to),
            None,
        )
        .await
        .unwrap();
        assert!(is_invalid(&result), "{reply_to}: {result:?}");
    }
    assert!(fake.rows().is_empty());
    // A blank replyTo is no replyTo.
    sent_id(
        send_message(
            &host,
            Some("S2"),
            "human",
            "q",
            None,
            None,
            None,
            Some(" "),
            None,
        )
        .await
        .unwrap(),
    );
    assert_eq!(fake.rows()[0].reply_to, None);
}

#[tokio::test]
async fn the_inbox_list_shows_where_replies_go_and_whether_that_session_ended() {
    let fake = FakeHost::new();
    let host = port(&fake);
    let to_role = sent_id(
        send_message(
            &host,
            Some("S2"),
            "human",
            "a",
            None,
            None,
            None,
            Some("role:manager"),
            None,
        )
        .await
        .unwrap(),
    );
    let to_h1 = sent_id(
        send_message(
            &host,
            Some("S2"),
            "human",
            "b",
            None,
            None,
            None,
            Some("H1"),
            None,
        )
        .await
        .unwrap(),
    );
    let plain = sent_id(
        send_message(
            &host,
            Some("S2"),
            "human",
            "c",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    fake.stop("H1");
    fake.stop("H2");
    let view = list_messages(
        &*host,
        &launch_team("default"),
        super::InboxScope::ToHuman,
        None,
        &[],
    )
    .await
    .unwrap();
    let row = |id: i64| view.messages.iter().find(|m| m.message_id == id).unwrap();
    assert_eq!(row(to_role).reply_to.as_deref(), Some("role:manager"));
    assert_eq!(row(to_role).reply_to_name.as_deref(), Some("manager"));
    assert!(!row(to_role).reply_target_ended, "a role queue never ends");
    assert_eq!(row(to_h1).reply_to_name.as_deref(), Some("Manager"));
    assert!(row(to_h1).reply_target_ended, "H1 ended");
    assert_eq!(row(plain).reply_to, None);
    assert!(row(plain).reply_target_ended, "its sender H2 ended");

    let wire = serde_json::to_value(row(to_role)).unwrap();
    assert_eq!(wire["replyTo"], "role:manager");
    assert_eq!(wire["replyToName"], "manager");
    assert_eq!(wire["replyTargetEnded"], false);
    let wire = serde_json::to_value(row(plain)).unwrap();
    assert!(wire.get("replyTo").is_none());
    assert_eq!(wire["replyTargetEnded"], true);
}

#[tokio::test]
async fn an_agent_that_names_no_session_sends_as_its_own_tab() {
    let fake = FakeHost::new();
    let host = port(&fake);
    // The local bridge named tab S1 at initialize; the call itself names nobody.
    let sent = sent_id(
        send_message(
            &host,
            crate::mcp::handler::local_inbox_caller(None, Some("S1")).unwrap(),
            "human",
            "done",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let row = fake.rows().into_iter().find(|m| m.id == sent).unwrap();
    assert_eq!(row.from, "H1", "never recorded as the human");

    // A remote agent's connection is its authenticated tab handle.
    fake.tab("remote-h", "remote-shell-1", "Buildbox", true);
    fake.teams
        .lock()
        .unwrap()
        .insert("remote-h".into(), "default".into());
    let remote = sent_id(
        send_message(
            &host,
            crate::mcp::handler::local_inbox_caller(None, Some("remote-h")).unwrap(),
            "human",
            "from afar",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let row = fake.rows().into_iter().find(|m| m.id == remote).unwrap();
    assert_eq!(row.from, "remote-h");

    assert!(crate::mcp::handler::local_inbox_caller(Some("S2"), Some("S1")).is_err());

    // Its reply goes out under its tab too, and its reads drain its own inbox.
    let asked = sent_id(
        send_message(&host, None, "H1", "status?", None, None, None, None, None)
            .await
            .unwrap(),
    );
    let read = read_messages(
        &*host,
        crate::mcp::handler::local_inbox_caller(None, Some("S1")).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        read.iter().map(|m| m.message_id).collect::<Vec<_>>(),
        vec![asked]
    );
    let answered = sent_id(
        reply(
            &host,
            crate::mcp::handler::local_inbox_caller(None, Some("S1")).unwrap(),
            asked,
            "green",
            None,
        )
        .await
        .unwrap(),
    );
    let row = fake.rows().into_iter().find(|m| m.id == answered).unwrap();
    assert_eq!((row.from.as_str(), row.to.as_str()), ("H1", HUMAN));

    assert!(crate::mcp::handler::local_inbox_caller(None, None).is_err());
}

#[tokio::test]
async fn a_connection_whose_tab_ended_is_refused_not_sent_as_the_human() {
    let fake = FakeHost::new();
    let host = port(&fake);
    fake.stop("H1");
    let result = send_message(
        &host,
        crate::mcp::handler::local_inbox_caller(None, Some("S1")).unwrap(),
        "human",
        "hi",
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(is_invalid(&result));
    assert!(fake.rows().is_empty());
    assert!(resolve_caller(&*host, Some("S1")).await.unwrap().is_err());
    assert_eq!(
        resolve_caller(&*host, Some("S2")).await.unwrap(),
        Ok(Some("H2".into()))
    );
    assert_eq!(resolve_caller(&*host, None).await.unwrap(), Ok(None));
}

#[tokio::test]
async fn a_verified_tab_missing_from_the_session_snapshot_is_never_the_human() {
    let fake = FakeHost::new();
    let host = port(&fake);
    let caller = crate::mcp::handler::local_inbox_caller(None, Some("not-yet-listed")).unwrap();
    let sent = send_message(
        &host, caller, "human", "hello", None, None, None, None, None,
    )
    .await
    .unwrap();
    assert!(is_invalid(&sent));
    assert!(fake.rows().is_empty());
}

#[tokio::test]
async fn text_typed_into_a_terminal_joins_the_targets_conversation_read() {
    let fake = FakeHost::new();
    let host = port(&fake);
    let before = *fake.changed.lock().unwrap();
    let id = log_terminal_send(&*host, Some("S1"), "H2", "run the tests")
        .await
        .unwrap()
        .expect("logged");
    let row = fake.rows().into_iter().find(|m| m.id == id).unwrap();
    assert_eq!((row.from.as_str(), row.to.as_str()), ("H1", "H2"));
    assert_eq!(row.via.as_deref(), Some(inbox::VIA_TERMINAL));
    assert!(row.read_at_ms.is_some(), "stored read");
    assert_eq!(
        *fake.changed.lock().unwrap(),
        before + 1,
        "the viewer refetches"
    );
    // It is never handed over again by read_messages, nor announced.
    assert!(read_messages(&*host, Some("S2")).await.unwrap().is_empty());
    assert!(fake.stored_for.lock().unwrap().is_empty());
    // inbox_list tells it from mail by `via`; mail carries none on the wire.
    let mail = sent_id(
        send_message(
            &host,
            Some("S1"),
            "H2",
            "mail",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let view = list_messages(
        &*host,
        &launch_team("default"),
        super::InboxScope::All,
        None,
        &[],
    )
    .await
    .unwrap();
    let typed = view.messages.iter().find(|r| r.message_id == id).unwrap();
    assert_eq!(typed.via.as_deref(), Some(inbox::VIA_TERMINAL));
    assert!(!typed.unread);
    assert_eq!(serde_json::to_value(typed).unwrap()["via"], "terminal");
    let mail = view.messages.iter().find(|r| r.message_id == mail).unwrap();
    assert_eq!(mail.via, None);
    assert!(serde_json::to_value(mail).unwrap().get("via").is_none());
    // Draining the mail returns only the mail, never the typed text.
    let drained = read_messages(&*host, Some("S2")).await.unwrap();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].text, "mail");

    // From the UI (no caller) it is the human's; a target named by session id is
    // stored under its handle.
    let id = log_terminal_send(&*host, None, "S2", "continue")
        .await
        .unwrap()
        .unwrap();
    let row = fake.rows().into_iter().find(|m| m.id == id).unwrap();
    assert_eq!((row.from.as_str(), row.to.as_str()), (HUMAN, "H2"));

    // A hand-started tab is addressed by its session id, which is its handle.
    fake.tab("tab-x", "tab-x", "scratch", true);
    let id = log_terminal_send(&*host, Some("S2"), "tab-x", "hi")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        fake.rows().into_iter().find(|m| m.id == id).unwrap().to,
        "tab-x"
    );
    // Blank text logs nothing.
    assert_eq!(
        log_terminal_send(&*host, None, "H2", "  ").await.unwrap(),
        None
    );
}

#[tokio::test]
async fn the_humans_reply_reaches_only_the_sender_and_no_role_queue() {
    use crate::weft::role_inbox::register_role;
    let fake = FakeHost::new();
    let host = port(&fake);
    let asked = sent_id(
        send_message(
            &host,
            Some("S2"),
            "human",
            "ok to merge?",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let reply_id = sent_id(
        reply_from_human(
            &host,
            &launch_team("default"),
            asked,
            "yes",
            Some(inbox::MessageContentType::Plain),
            None,
        )
        .await
        .unwrap(),
    );
    let row = fake.rows().into_iter().find(|m| m.id == reply_id).unwrap();
    assert_eq!((row.from.as_str(), row.to.as_str()), (HUMAN, "H2"));
    assert_eq!(row.content_type, Some(inbox::MessageContentType::Plain));
    assert_eq!(row.delivery, None, "a plain reply, not a role delivery");
    // The foreman (controller) queue never sees it.
    let store = WeftStore::open(fake.team_store("default")).unwrap();
    let reg = register_role(
        &store,
        "foreman",
        "foreman-1",
        chrono::Utc::now().timestamp(),
    )
    .unwrap();
    assert_eq!(reg.pending_count, 0);
    // The sender is told, like for any other mail.
    assert!(fake.stored_for.lock().unwrap().contains(&"H2".to_string()));
    // Blank and unknown are refused.
    assert!(is_invalid(
        &reply_from_human(&host, &launch_team("default"), asked, " ", None, None)
            .await
            .unwrap()
    ));
    assert!(is_invalid(
        &reply_from_human(&host, &launch_team("default"), 999, "hi", None, None)
            .await
            .unwrap()
    ));
}

/// A remote agent's calls speak as its authenticated tab: never the human, never the
/// session its arguments name, and not refused while its machine's layout lags (its
/// authority proved it live).
#[tokio::test]
async fn an_authenticated_remote_caller_speaks_as_its_own_tab() {
    let fake = FakeHost::new();
    let host = port(&fake);
    fake.tab("remote-h", "remote-shell-1", "Buildbox", false);
    fake.teams
        .lock()
        .unwrap()
        .insert("remote-h".into(), "default".into());
    let me = Caller::Authenticated("remote-h");
    let sent = sent_id(
        send_message(&host, me, "H1", "built", None, None, None, None, None)
            .await
            .unwrap(),
    );
    let row = fake.rows().into_iter().find(|m| m.id == sent).unwrap();
    assert_eq!((row.from.as_str(), row.to.as_str()), ("remote-h", "H1"));
    assert_eq!(
        resolve_caller(&*host, me).await.unwrap(),
        Ok(Some("remote-h".into()))
    );

    // H1 answers; the remote agent reads it and replies, and its reply reaches H1.
    let answer = sent_id(
        reply(&host, Some("S1"), sent, "thanks", None)
            .await
            .unwrap(),
    );
    let read = read_messages(&*host, me).await.unwrap();
    assert_eq!(
        read.iter().map(|m| m.message_id).collect::<Vec<_>>(),
        vec![answer]
    );
    assert_eq!(read[0].from, "H1");
    let back = sent_id(reply(&host, me, answer, "ack", None).await.unwrap());
    let row = fake.rows().into_iter().find(|m| m.id == back).unwrap();
    assert_eq!((row.from.as_str(), row.to.as_str()), ("remote-h", "H1"));
    let mine = read_messages(&*host, Some("S1")).await.unwrap();
    assert!(mine
        .iter()
        .any(|m| m.message_id == back && m.from == "remote-h"));
}

/// A local agent (or the human) messages a remote tab by its session id there: the mail
/// is stored for its handle, offered to the idle check, and its remote hook (keyed by
/// that session) announces it.
#[tokio::test]
async fn mail_to_a_remote_tab_is_offered_under_its_remote_session() {
    let mut fake = FakeHost::new();
    Arc::get_mut(&mut fake)
        .unwrap()
        .handles
        .insert("remote-shell-1".into(), "remote-h".into());
    let host = port(&fake);
    fake.tab("remote-h", "remote-shell-1", "Buildbox", true);
    fake.teams
        .lock()
        .unwrap()
        .insert("remote-h".into(), "default".into());
    let sent = sent_id(
        send_message(
            &host,
            Some("S1"),
            "remote-shell-1",
            "deploy?",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let row = fake.rows().into_iter().find(|m| m.id == sent).unwrap();
    assert_eq!((row.from.as_str(), row.to.as_str()), ("H1", "remote-h"));
    assert!(fake
        .stored_for
        .lock()
        .unwrap()
        .contains(&"remote-h".to_string()));
    let human = sent_id(
        send_message(
            &host,
            None,
            "remote-h",
            "and status",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let notice = hook_notice(&*host, "remote-shell-1")
        .await
        .expect("announced");
    assert_eq!(notice, mail_notice(2, "remote-shell-1"));
    let read = read_messages(&*host, Caller::Authenticated("remote-h"))
        .await
        .unwrap();
    assert_eq!(
        read.iter().map(|m| m.message_id).collect::<Vec<_>>(),
        vec![sent, human]
    );
}

#[tokio::test]
async fn mail_from_a_remote_agent_wakes_only_its_recipient_local_or_on_another_machine() {
    let mut fake = FakeHost::new();
    {
        let handles = &mut Arc::get_mut(&mut fake).unwrap().handles;
        handles.insert("remote-shell-a".into(), "remote-a".into());
        handles.insert("remote-shell-b".into(), "remote-b".into());
    }
    let host = port(&fake);
    for (handle, session, name) in [
        ("remote-a", "remote-shell-a", "machine A"),
        ("remote-b", "remote-shell-b", "machine B"),
    ] {
        fake.tab(handle, session, name, true);
        fake.teams
            .lock()
            .unwrap()
            .insert(handle.into(), "default".into());
    }
    // Remote A to this Mac's agent H2: offered to it alone, the mail names A.
    send_message(
        &host,
        Some("remote-shell-a"),
        "H2",
        "from A",
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(hook_notice(&*host, "S2").await.is_some());
    // Another agent of this Mac's is never told about mail that is not its own.
    assert_eq!(hook_notice(&*host, "S1").await, None);
    // Remote A to remote B: stored for B (whose machine is pushed its notice), offered
    // to B's session alone, attributed to A.
    send_message(
        &host,
        Some("remote-shell-a"),
        "remote-b",
        "to B",
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        *fake.stored_for.lock().unwrap(),
        vec!["H2".to_string(), "remote-b".to_string()]
    );
    assert!(matches!(
        check_for_mail(&*host, "remote-shell-b", false).await,
        Ok(Check::Offer { .. })
    ));
    assert!(matches!(
        check_for_mail(&*host, "remote-shell-a", false).await,
        Ok(Check::Nothing)
    ));
    let read = read_messages(&*host, Caller::Authenticated("remote-b"))
        .await
        .unwrap();
    assert_eq!(
        (read[0].from.as_str(), read[0].from_name.as_str()),
        ("remote-a", "machine A")
    );
}

#[tokio::test]
async fn an_agent_reply_queued_for_a_role_tells_the_role_pending_watchers_once() {
    let fake = FakeHost::new();
    let host = port(&fake);
    let to_h1 = sent_id(
        send_message(
            &host,
            None,
            "H1",
            "status?",
            None,
            None,
            None,
            Some("role:manager"),
            None,
        )
        .await
        .unwrap(),
    );
    assert!(fake.queued.lock().unwrap().is_empty());
    let answer = sent_id(
        reply(&host, Some("S1"), to_h1, "green", None)
            .await
            .unwrap(),
    );
    assert_eq!(
        *fake.queued.lock().unwrap(),
        vec![(
            "default".to_string(),
            "manager".to_string(),
            answer,
            "H1".to_string()
        )]
    );
}

#[tokio::test]
async fn a_role_human_reply_to_is_stored_as_the_human_and_can_be_answered() {
    let fake = FakeHost::new();
    let host = port(&fake);
    for spelling in ["role:human", " role:Human ", "HUMAN"] {
        let to_h1 = sent_id(
            send_message(
                &host,
                Some("S2"),
                "H1",
                "do it",
                None,
                None,
                None,
                Some(spelling),
                None,
            )
            .await
            .unwrap(),
        );
        let row = fake.rows().into_iter().find(|m| m.id == to_h1).unwrap();
        assert_eq!(row.reply_to.as_deref(), Some(HUMAN), "{spelling}");
        // The answer reaches the human rather than being refused as a role reply.
        let answer = sent_id(reply(&host, Some("S1"), to_h1, "done", None).await.unwrap());
        let row = fake.rows().into_iter().find(|m| m.id == answer).unwrap();
        assert_eq!((row.from.as_str(), row.to.as_str()), ("H1", HUMAN));
    }
    assert!(role_deliveries(&fake, "default").is_empty());
}

#[tokio::test]
async fn a_legacy_stored_role_human_reply_to_still_answers_the_human() {
    let fake = FakeHost::new();
    let host = port(&fake);
    // A row stored before replyTo was normalized.
    let to_h1 = inbox::inbox_insert_with_reply_to(
        &WeftStore::open(fake.team_store("default")).unwrap(),
        "H2",
        "H1",
        "do it",
        None,
        None,
        None,
        None,
        None,
        Some("role:human"),
        None,
        None,
    )
    .unwrap();
    let answer = sent_id(reply(&host, Some("S1"), to_h1, "done", None).await.unwrap());
    let row = fake.rows().into_iter().find(|m| m.id == answer).unwrap();
    assert_eq!(row.to, HUMAN);
}

#[tokio::test]
async fn a_legacy_stored_role_human_reply_to_shows_as_the_human() {
    let fake = FakeHost::new();
    let host = port(&fake);
    let store = WeftStore::open(fake.team_store("default")).unwrap();
    let mut ids = Vec::new();
    for legacy in ["role:human", "role:Human", "role:HUMAN"] {
        ids.push(
            inbox::inbox_insert_with_reply_to(
                &store,
                "H2",
                HUMAN,
                "legacy",
                None,
                None,
                None,
                None,
                None,
                Some(legacy),
                None,
                None,
            )
            .unwrap(),
        );
    }
    let view = list_messages(
        &*host,
        &launch_team("default"),
        super::InboxScope::ToHuman,
        None,
        &[],
    )
    .await
    .unwrap();
    for id in ids {
        let row = view.messages.iter().find(|m| m.message_id == id).unwrap();
        assert_eq!(row.reply_to.as_deref(), Some(HUMAN), "{id}");
        assert_eq!(row.reply_to_name.as_deref(), Some(HUMAN), "{id}");
        assert!(!row.reply_target_ended, "{id}: the human never ends");
    }
}

#[tokio::test]
async fn the_human_is_a_recipient_in_any_case() {
    let fake = FakeHost::new();
    let host = port(&fake);
    for to in ["Human", " HUMAN "] {
        let id = sent_id(
            send_message(&host, Some("S2"), to, "hi", None, None, None, None, None)
                .await
                .unwrap(),
        );
        let row = fake.rows().into_iter().find(|m| m.id == id).unwrap();
        assert_eq!(row.to, HUMAN, "{to}");
    }
}

/// `S1` mails `H2` (`S2`'s handle).
async fn send_hello(host: &Arc<dyn InboxHost>) {
    send_message(
        host,
        Some("S1"),
        "H2",
        "hello",
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
}

/// What one check tells `session` about its mail now, marking it announced as its host
/// would once its agent has the notice.
async fn hook_notice(host: &dyn InboxHost, session: &str) -> Option<String> {
    match check_for_mail(host, session, false).await {
        Ok(Check::Offer { notice, token }) => {
            assert!(confirm_notice(host, session, &token).await.unwrap());
            Some(notice)
        }
        _ => None,
    }
}

#[tokio::test]
async fn after_an_app_restart_an_agent_is_not_told_again_about_mail_it_was_told_about() {
    let fake = FakeHost::new();
    let host = port(&fake);
    send_hello(&host).await;
    assert!(hook_notice(&*host, "S2").await.is_some());

    // The app restarts: the inbox store is the same. The agent's waiter re-arms and
    // finds nothing new: it is not woken about mail it left unread.
    assert_eq!(hook_notice(&*host, "S2").await, None);
    // Mail that came while the app was down is owed: the re-armed waiter hands it over.
    send_message(
        &host,
        Some("S1"),
        "H2",
        "while down",
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(hook_notice(&*host, "S2").await.is_some());
}

/// A launch record for a remote tab `handle` on `machine` under `session`, launched with
/// CodeSense's hooks when `hooks`.
fn remote_tab_record(
    handle: &str,
    machine: &str,
    session: &str,
    hooks: bool,
) -> crate::commands::pty_layout::RemoteShellRecord {
    serde_json::from_value(serde_json::json!({
        "handle": handle, "owner_id": 41, "request_id": uuid::Uuid::new_v4(),
        "machine_id": machine, "team_key": "acme:default", "workspace_id": "w",
        "workspace": "acme:default|w", "cwd": "/home/dev", "name": null, "launch": "claude",
        "hooks": hooks,
        "target": {"member":{"machineId":machine,"slot":"remote"},"admissionRevision":"1",
            "namespace":{"serverId":"s","epoch":"e","generation":"1"}, "session": session}
    }))
    .unwrap()
}

/// Mail stored for a remote tab while its machine is disconnected is not lost: the push
/// finds the machine unreachable and releases its reservation, so the next offer pushes
/// it again; and when the machine reconnects its tab (and only the tabs whose notices go
/// there) is offered the mail at once and pushed, still unread for the agent's next read.
#[tokio::test]
async fn mail_for_a_disconnected_machines_tab_is_pushed_again_when_it_reconnects() {
    use crate::commands::dashboard_launch::DashboardLaunchState;
    use crate::commands::host_inbox::{offer_once, reconnect_offers, ttl, Offer, Push};
    let fake = FakeHost::new();
    let host = port(&fake);
    // H2's tab runs on "box" under session S2, launched with CodeSense's hooks; H3's on
    // "other" under S3.
    let launch = DashboardLaunchState::new(4);
    launch
        .reserve_remote_shell(remote_tab_record("H2", "box", "S2", true))
        .unwrap();
    launch
        .reserve_remote_shell(remote_tab_record("H3", "other", "S3", true))
        .unwrap();
    let unread = |fake: &FakeHost| {
        let store = WeftStore::open(fake.team_store("default")).unwrap();
        inbox::inbox_unread_for(&store, &["H2".to_string()]).unwrap()["H2"].clone()
    };

    // Mail is stored while "box" is disconnected: the push can't reach it.
    send_hello(&host).await;
    let again = offer_once(&*host, "S2", Offer::New, 0, |_, _| async {
        Err(Push::Unreachable)
    })
    .await;
    assert_eq!(again, None, "nothing is retried until the machine connects");
    assert_eq!(unread(&fake).announced, None, "the agent was not told");

    // Its reservation was released: the next offer, even a new one, pushes it again
    // rather than waiting out the reservation. "box" is still disconnected.
    let pushed = Mutex::new(None);
    let again = offer_once(&*host, "S2", Offer::New, 0, |notice, _| {
        *pushed.lock().unwrap() = Some(notice);
        async { Err(Push::Unreachable) }
    })
    .await;
    assert_eq!(again, None);
    let notice = pushed
        .into_inner()
        .unwrap()
        .expect("pushed again after the release");
    assert!(notice.starts_with("You have 1 new message"), "{notice}");

    // "box" reconnects: of the tabs this app launched, only those on "box" are offered
    // their mail at once.
    assert_eq!(reconnect_offers(&launch, "box"), vec!["S2".to_string()]);
    assert_eq!(reconnect_offers(&launch, "other"), vec!["S3".to_string()]);
    let pushed = Mutex::new(None);
    let pushed_at = std::time::Instant::now();
    let again = offer_once(&*host, "S2", Offer::Restored, 0, |notice, token| {
        *pushed.lock().unwrap() = Some((notice, token));
        async { Ok(()) }
    })
    .await;
    assert_eq!(
        again,
        Some((ttl(), 0)),
        "the push is checked again once its reservation lapses"
    );
    let (notice, token) = pushed.into_inner().unwrap().expect("pushed on reconnect");
    assert!(notice.starts_with("You have 1 new message"), "{notice}");

    // Pushed but not yet confirmed: the agent is not marked told, and a new offer leaves
    // the notice to its reservation.
    assert_eq!(unread(&fake).announced, None);
    let pushed_again = Mutex::new(false);
    let again = offer_once(&*host, "S2", Offer::New, 0, |_, _| {
        *pushed_again.lock().unwrap() = true;
        async { Ok(()) }
    })
    .await;
    assert!(!pushed_again.into_inner().unwrap(), "not pushed again");
    assert_waits_out_the_reservation(again, pushed_at);

    // Its host hands the notice over and confirms it: the agent is marked told, and the
    // message is still unread for its next read.
    assert!(confirm_notice(&*host, "S2", &token).await.unwrap());
    let told = unread(&fake);
    assert!(told.announced.is_some(), "marked told once confirmed");
    assert_eq!(told.count, 1, "still unread");
    let read = read_messages(&*host, Some("S2")).await.unwrap();
    assert_eq!(read.len(), 1);
}

/// Failed handoffs can retry, but a notice already held by its host is not
/// re-pushed by the desktop. The host owns re-offering a refused CLI submission.
#[tokio::test]
async fn a_failed_handoff_retries_but_a_host_held_notice_does_not() {
    use crate::commands::host_inbox::{offer_once, Offer, Push};
    let fake = FakeHost::new();
    let host = port(&fake);
    send_hello(&host).await;
    let again = offer_once(&*host, "S2", Offer::New, 0, |_, _| async {
        Err(Push::NotAccepted("transport failed".into()))
    })
    .await;
    assert!(matches!(again, Some((_, 1))), "bounded pre-handoff retry");
    let held = Mutex::new(None);
    let again = offer_once(&*host, "S2", Offer::New, 1, |_, token| {
        *held.lock().unwrap() = Some(token);
        async { Ok(()) }
    })
    .await;
    assert!(again.is_some(), "confirmation recheck after acceptance");
    let token = held.into_inner().unwrap().unwrap();
    hold_notice(&*host, "S2", &token).await;
    let again = offer_once(&*host, "S2", Offer::New, 0, |_, _| async {
        panic!("host-held notices are never re-pushed by the desktop")
    })
    .await;
    assert_eq!(again, None);
    let store = WeftStore::open(fake.team_store("default")).unwrap();
    let unread = inbox::inbox_unread_for(&store, &["H2".to_string()]).unwrap();
    assert_eq!(unread["H2"].count, 1);
    assert_eq!(unread["H2"].announced, None);
}

/// `again` checks once more when the reservation made at `pushed_at` (or just after) lapses:
/// what is left of [`ttl`] after the time gone since, to the millisecond the store keeps.
fn assert_waits_out_the_reservation(
    again: Option<(std::time::Duration, u32)>,
    pushed_at: std::time::Instant,
) {
    use crate::commands::host_inbox::ttl;
    let gone = pushed_at.elapsed();
    let Some((wait, 0)) = again else {
        panic!("expected a check when the reservation lapses, got {again:?}");
    };
    assert!(wait <= ttl(), "{wait:?} is within the reservation");
    assert!(
        wait + gone + std::time::Duration::from_millis(1) >= ttl(),
        "{wait:?} is the rest of the reservation, {gone:?} after its push"
    );
}

/// A tab brought back, or whose host reconnected, is pushed the notice it was pushed but
/// never confirmed, at once and under the same token: its host lost what it held. A new
/// offer meanwhile leaves the notice to its reservation.
#[tokio::test]
async fn a_restored_offer_pushes_an_unconfirmed_notice_again() {
    use crate::commands::host_inbox::{offer_once, ttl, Offer};
    let fake = FakeHost::new();
    let host = port(&fake);
    send_hello(&host).await;

    // The notice is pushed and lands, but its confirm never comes.
    let first = Mutex::new(None);
    let pushed_at = std::time::Instant::now();
    let again = offer_once(&*host, "S2", Offer::New, 0, |notice, token| {
        *first.lock().unwrap() = Some((notice, token));
        async { Ok(()) }
    })
    .await;
    assert_eq!(again, Some((ttl(), 0)));
    let (_, token) = first.into_inner().unwrap().expect("pushed");

    // Before its reservation lapses, a new offer does not push it again.
    let pushed_again = Mutex::new(false);
    let again = offer_once(&*host, "S2", Offer::New, 0, |_, _| {
        *pushed_again.lock().unwrap() = true;
        async { Ok(()) }
    })
    .await;
    assert!(!pushed_again.into_inner().unwrap(), "a new offer waits");
    assert_waits_out_the_reservation(again, pushed_at);

    // A restored offer pushes it again at once, under the same token.
    let restored = Mutex::new(None);
    let again = offer_once(&*host, "S2", Offer::Restored, 0, |notice, token| {
        *restored.lock().unwrap() = Some((notice, token));
        async { Ok(()) }
    })
    .await;
    assert_eq!(again, Some((ttl(), 0)));
    let (notice, again_token) = restored.into_inner().unwrap().expect("pushed again");
    assert!(notice.starts_with("You have 1 new message"), "{notice}");
    assert_eq!(again_token, token, "the same notice, offered again");
}

/// A tab on the reconnecting machine whose agent has no CodeSense hooks has nowhere to
/// hear a notice: it is not offered its mail when the machine reconnects.
#[test]
fn a_hookless_tab_is_not_offered_its_mail_on_reconnect() {
    use crate::commands::dashboard_launch::DashboardLaunchState;
    use crate::commands::host_inbox::reconnect_offers;
    let launch = DashboardLaunchState::new(4);
    launch
        .reserve_remote_shell(remote_tab_record("H2", "box", "S2", true))
        .unwrap();
    launch
        .reserve_remote_shell(remote_tab_record("H4", "box", "S4", false))
        .unwrap();
    assert_eq!(reconnect_offers(&launch, "box"), vec!["S2".to_string()]);
}

#[tokio::test]
async fn the_human_archives_many_messages_with_one_change_signal() {
    let fake = FakeHost::new();
    let host = port(&fake);
    let store = WeftStore::open(fake.team_store("default")).unwrap();
    let insert =
        |to| inbox::inbox_insert(&store, "H1", to, "hello", None, None, None, None, None).unwrap();
    let ids = vec![insert(HUMAN), insert(HUMAN), insert("H2")];
    let before = *fake.changed.lock().unwrap();
    assert_eq!(
        mark_archived_many_by_human(&*host, &launch_team("default"), ids.clone())
            .await
            .unwrap(),
        2
    );
    assert_eq!(*fake.changed.lock().unwrap(), before + 1);
    assert_eq!(inbox::inbox_unread(&store, HUMAN).unwrap().count, 0);
    assert_eq!(inbox::inbox_unread(&store, "H2").unwrap().count, 1);
    assert_eq!(
        mark_archived_many_by_human(&*host, &launch_team("default"), ids)
            .await
            .unwrap(),
        0
    );
    assert_eq!(*fake.changed.lock().unwrap(), before + 1);
}

#[tokio::test]
async fn the_human_unarchives_many_messages_with_one_change_signal() {
    let fake = FakeHost::new();
    let host = port(&fake);
    let team = launch_team("default");
    let store = WeftStore::open(fake.team_store("default")).unwrap();
    let insert =
        |to| inbox::inbox_insert(&store, "H1", to, "hello", None, None, None, None, None).unwrap();
    let ids = vec![insert(HUMAN), insert(HUMAN), insert("H2")];
    mark_archived_many_by_human(&*host, &team, ids.clone())
        .await
        .unwrap();
    let before = *fake.changed.lock().unwrap();
    assert_eq!(
        unarchive_many_by_human(&*host, &team, ids.clone(), vec![ids[0]])
            .await
            .unwrap(),
        2
    );
    assert_eq!(*fake.changed.lock().unwrap(), before + 1);
    assert_eq!(inbox::inbox_unread(&store, HUMAN).unwrap().count, 1);
    assert_eq!(
        unarchive_many_by_human(&*host, &team, ids, vec![])
            .await
            .unwrap(),
        0
    );
    assert_eq!(*fake.changed.lock().unwrap(), before + 1);
}

#[tokio::test]
async fn workspace_chat_append_to_native_notice_and_late_bind() {
    use crate::commands::dashboard_launch::{LaunchMeta, LaunchRecord};
    use crate::commands::host_inbox::{inbox_target, offer_once, settle, Fate, Offer, Push};
    use crate::commands::workspace_chat::commands::notify_workspace_members;
    use crate::commands::workspace_chat::{ChatAppend, MessageKind};
    let fake = FakeHost::new();
    let host = port(&fake);
    let bind = |handle: &str, session: &str, story: &str| {
        fake.launch
            .begin_with_provenance(
                handle,
                None,
                LaunchRecord {
                    meta: LaunchMeta {
                        story: Some(story.into()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        fake.launch.bind(handle, session, None).unwrap();
    };
    bind("H1", "S1", "SV-X");
    bind("H2", "S2", "SV-X");
    bind("elsewhere", "other-session", "SV-Y");
    send_hello(&host).await;
    let mut remote = remote_tab_record("remote", "box", "remote-session", true);
    remote.workspace_id = "sv-x-fix-login".into();
    remote.workspace_story_id = Some("SV-X".into());
    fake.launch.reserve_remote_shell(remote).unwrap();
    assert_eq!(
        inbox_target(&fake.launch, "remote"),
        Some(("box".into(), "remote-session".into()))
    );
    let post = fake
        .chat
        .append(
            ChatAppend {
                workspace_id: "story:SV-X".into(),
                from: "H1".into(),
                from_name: "Manager".into(),
                text: "to everyone".into(),
                kind: MessageKind::Chat,
                content_type: None,
                on_click: None,
                primary_file: None,
            },
            1,
        )
        .unwrap();
    let mut notified = Vec::new();
    notify_workspace_members(
        &fake.launch,
        &post.workspace_id,
        post.kind,
        &post.from,
        |h| notified.push(h.to_string()),
    );
    notified.sort();
    assert_eq!(notified, vec!["H2", "remote"]);
    assert!(matches!(
        check_for_mail(&*host, "S1", false).await,
        Ok(Check::Nothing)
    ));
    assert!(matches!(
        check_for_mail(&*host, "other-session", false).await,
        Ok(Check::Nothing)
    ));
    // A busy host accepts and holds a notice until its safe turn; no second push.
    let token = Mutex::new(None);
    offer_once(&*host, "S2", Offer::New, 0, |notice, t| {
        assert!(notice.contains("workspace_chat_read") && notice.contains("read_messages"));
        *token.lock().unwrap() = Some(t);
        async { Ok(()) }
    })
    .await;
    offer_once(&*host, "S2", Offer::New, 0, |_, _| async {
        panic!("duplicate push")
    })
    .await;
    assert_eq!(fake.chat.unread("story:SV-X", "H2", "S2").unwrap().count, 1);
    settle(
        &*host,
        "S2",
        &token.into_inner().unwrap().unwrap(),
        Fate::Told,
    )
    .await;
    assert!(matches!(
        check_for_mail(&*host, "S2", false).await,
        Ok(Check::Nothing)
    ));
    assert_eq!(
        fake.chat.unread("story:SV-X", "H2", "S2").unwrap().count,
        1,
        "told is not read"
    );
    // Disconnected remote releases; reconnect offers the same unread post.
    offer_once(&*host, "remote-session", Offer::New, 0, |_, _| async {
        Err(Push::Unreachable)
    })
    .await;
    let Ok(Check::Offer { token, .. }) = check_for_mail(&*host, "remote-session", true).await
    else {
        panic!("remote backlog missing")
    };
    settle(&*host, "remote-session", &token, Fate::Told).await;
    fake.chat
        .read_for_session("story:SV-X", "H2", "S2", None, None)
        .unwrap();
    assert_eq!(
        fake.chat
            .unread("story:SV-X", "remote", "remote-session")
            .unwrap()
            .count,
        1
    );
    // Clearing and relaunching the same story handle is a new agent, even
    // without an intervening roster-pruning pass or another post.
    fake.launch.release_for_session("S2").unwrap();
    bind("H2", "S3", "SV-X");
    let notice = hook_notice(&*host, "S3").await.unwrap();
    assert!(notice.contains("1 new group-chat message"));
    bind("late", "late-session", "SV-X");
    let notice = hook_notice(&*host, "late-session").await.unwrap();
    assert!(notice.contains("1 new group-chat message"));
    assert_eq!(
        fake.chat.head_id("story:SV-X"),
        1,
        "late join needs no append"
    );
}

#[tokio::test]
async fn request_id_retries_store_one_message_and_one_reply_across_store_reopens() {
    let host = FakeHost::new();
    let port = port(&host);
    let send = || {
        send_message_request(
            &port,
            Some("S1"),
            "H2",
            "review this",
            None,
            None,
            None,
            None,
            None,
            None,
            Some("send-1"),
            None,
        )
    };
    let first = sent_id(send().await.unwrap());
    let stored_hash = |key: &str| -> String {
        WeftStore::open(host.team_store("default"))
            .unwrap()
            .conn()
            .unwrap()
            .query_row(
                "SELECT payload_hash FROM wv_send_message_request WHERE request_id=?1",
                [key],
                |r| r.get(0),
            )
            .unwrap()
    };
    // Ordinary messages retain the receipt format from before image attachments.
    assert_eq!(
        stored_hash("mail:H1:send-1"),
        "eff7d0076e3480f60178bce8510c2f5b4288e8f1443a6951ec4dcc01d30c5929"
    );
    assert_eq!(sent_id(send().await.unwrap()), first);
    assert_eq!(host.stored_event_ids(), vec![first]);
    let messages = read_messages(&*port, Some("S2")).await.unwrap();
    assert_eq!(messages.len(), 1);
    let answer = || {
        reply_request(
            &port,
            Some("S2"),
            first,
            "reviewed",
            None,
            None,
            None,
            Some("reply-1"),
            None,
        )
    };
    let reply = sent_id(answer().await.unwrap());
    assert_eq!(
        stored_hash("mail:H2:reply-1"),
        inbox::request_payload_hash(&serde_json::json!([
            "H2", "H1", "reviewed", first, ORG, "default", null, null, null, null, null, null
        ]))
    );
    assert_eq!(sent_id(answer().await.unwrap()), reply);
    assert_eq!(host.stored_event_ids(), vec![first, reply]);
    assert_eq!(*host.stored_for.lock().unwrap(), vec!["H2", "H1"]);
    let messages = read_messages(&*port, Some("S1")).await.unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].text, "reviewed");
    let changed = send_message_request(
        &port,
        Some("S1"),
        "H2",
        "different",
        None,
        None,
        None,
        None,
        None,
        None,
        Some("send-1"),
        None,
    )
    .await
    .unwrap();
    assert!(matches!(changed, SendResult::Invalid { .. }));
    assert_eq!(host.stored_event_ids(), vec![first, reply]);
}

#[tokio::test]
async fn overlapping_reply_retries_publish_one_stored_event() {
    let fake = FakeHost::new();
    let host = port(&fake);
    let original = sent_id(
        send_message(
            &host,
            Some("S1"),
            "H2",
            "review this",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let answer = || {
        reply_request(
            &host,
            Some("S2"),
            original,
            "reviewed",
            None,
            None,
            None,
            Some("reply-overlap"),
            None,
        )
    };
    let (first, retry) = tokio::join!(answer(), answer());
    let reply = sent_id(first.unwrap());
    assert_eq!(sent_id(retry.unwrap()), reply);
    assert_eq!(fake.stored_event_ids(), vec![original, reply]);
    assert_eq!(*fake.stored_for.lock().unwrap(), vec!["H2", "H1"]);
    assert_eq!(fake.rows().len(), 2);
    assert_eq!(read_messages(&*host, Some("S1")).await.unwrap().len(), 1);
}

#[tokio::test]
async fn role_reply_retries_publish_one_stored_event_and_queue_notice() {
    let fake = FakeHost::new();
    let host = port(&fake);
    let original = sent_id(
        send_message(
            &host,
            Some("S1"),
            "H2",
            "review this",
            None,
            None,
            None,
            Some("role:manager"),
            None,
        )
        .await
        .unwrap(),
    );
    let answer = || {
        reply_request(
            &host,
            Some("S2"),
            original,
            "reviewed",
            None,
            None,
            None,
            Some("role-reply"),
            None,
        )
    };
    let reply = sent_id(answer().await.unwrap());
    assert_eq!(sent_id(answer().await.unwrap()), reply);
    assert_eq!(fake.stored_event_ids(), vec![original, reply]);
    assert_eq!(fake.queued.lock().unwrap().len(), 1);
    assert_eq!(fake.rows().len(), 2);
    let row = fake.rows().into_iter().find(|row| row.id == reply).unwrap();
    assert_eq!(row.to, "manager");
    assert!(row.read_at_ms.is_none());
}

#[tokio::test]
async fn request_id_retry_replays_after_reply_to_exits_but_new_sends_still_validate() {
    let fake = FakeHost::new();
    let host = port(&fake);
    let send = |text: &'static str, key: &'static str| {
        send_message_request(
            &host,
            Some("S2"),
            "human",
            text,
            None,
            None,
            None,
            Some("H1"),
            None,
            None,
            Some(key),
            None,
        )
    };
    let original = sent_id(send("review this", "exit-retry").await.unwrap());
    fake.stop("H1");
    assert_eq!(
        sent_id(send("review this", "exit-retry").await.unwrap()),
        original
    );
    let conflict = send("changed payload", "exit-retry").await.unwrap();
    assert!(
        matches!(conflict, SendResult::Invalid { message } if message.contains("different payload"))
    );
    let new = send("review this", "new-send").await.unwrap();
    assert!(matches!(new, SendResult::Invalid { message } if message.contains("not running")));
}

#[tokio::test]
async fn request_id_replays_original_result_after_a_registration_only_role_handoff() {
    use crate::weft::role_inbox::{register_role, release_role};
    let fake = FakeHost::new();
    let host = port(&fake);
    let store = WeftStore::open(fake.team_store("default")).unwrap();
    let now = chrono::Utc::now().timestamp();
    let reg = register_role(&store, "manager", "S1", now).unwrap();
    let send = |text: &'static str, key: &'static str| {
        send_message_request(
            &host,
            Some("S1"),
            "human",
            text,
            None,
            None,
            None,
            Some("role:manager"),
            None,
            None,
            Some(key),
            None,
        )
    };
    let original = send("Digest ready?", "role-digest").await.unwrap();
    assert!(matches!(
        &original,
        SendResult::Sent {
            kind: Some(inbox::MessageKind::Update),
            hint: Some(_),
            ..
        }
    ));
    release_role(&store, "manager", "S1", reg.generation).unwrap();
    assert!(!inbox::role_holders(&store)
        .unwrap()
        .any_holds("manager", &["S1"]));
    register_role(&store, "manager", "S2", now).unwrap();
    assert_eq!(
        send("Digest ready?", "role-digest").await.unwrap(),
        original
    );
    assert_eq!(fake.rows().len(), 1);
    assert!(
        matches!(send("Different digest", "role-digest").await.unwrap(), SendResult::Invalid { message } if message.contains("different payload"))
    );
    assert!(matches!(
        send("Digest ready?", "new-digest").await.unwrap(),
        SendResult::Sent {
            kind: Some(inbox::MessageKind::NeedsYou),
            ..
        }
    ));
}

#[tokio::test]
async fn sidecar_reconnect_retells_each_local_agents_untold_mail_once_even_without_a_waiter() {
    use crate::commands::host_inbox::{offer_once, settle, Offer};
    use codesense_host_proto::inbox_wait as wire;
    use codesense_pty_core::domain::InboxWaiterState;
    use codesense_pty_core::inbox_host::{Fate, HostInbox, InboxOutlet};
    use codesense_pty_core::inbox_waiters::WaitArm;
    #[derive(Default)]
    struct Outlet(Mutex<Vec<(String, String, Fate)>>);
    impl InboxOutlet for Outlet {
        fn fate(&self, session: &str, token: &str, fate: Fate) {
            self.0
                .lock()
                .unwrap()
                .push((session.into(), token.into(), fate));
        }
        fn waiter(&self, _: InboxWaiterState) {}
        fn audience(&self, _: &str) -> Option<String> {
            None
        }
        fn has_tools(&self, _: &str) -> Option<bool> {
            Some(true)
        }
    }
    let fake = FakeHost::new();
    let host = port(&fake);
    send_hello(&host).await;
    send_message(
        &host,
        Some("S2"),
        "H1",
        "mail for lead",
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    // The old sidecar accepted both notices but stopped before confirming either.
    for session in ["S1", "S2"] {
        offer_once(&*host, session, Offer::New, 0, |_, _| async { Ok(()) }).await;
    }
    let inbox = Arc::new(HostInbox::default());
    let outlet = Arc::new(Outlet::default());
    for session in ["S1", "S2"] {
        assert!(!inbox.state(session).waiting);
        offer_once(&*host, session, Offer::Restored, 0, |notice, token| {
            inbox.mail(session, notice, token);
            async { Ok(()) }
        })
        .await;
        let (server, mut helper) = std::os::unix::net::UnixStream::pair().unwrap();
        helper
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        let (inbox, outlet) = (inbox.clone(), outlet.clone());
        let served = std::thread::spawn(move || {
            inbox.serve(
                &*outlet,
                session,
                WaitArm::IfAbsent,
                codesense_pty_core::activity::peer::inspect(std::process::id()).unwrap(),
                true,
                None,
                server,
            )
        });
        let first = codesense_host_proto::reverse_mcp::read_message(
            &mut helper,
            wire::MAX_LINE,
            |_| Ok(()),
        );
        // Read the same lines the hook helper reads, then acknowledge printing.
        let first = match first {
            Ok(line) => line,
            Err(e) => panic!("{e}"),
        };
        let token = match wire::parse_answer(first.trim_end()) {
            Some(Ok(token)) => token.to_string(),
            _ => panic!("mail expected"),
        };
        let notice = codesense_host_proto::reverse_mcp::read_message(
            &mut helper,
            wire::MAX_LINE,
            |_| Ok(()),
        )
        .unwrap();
        assert!(notice.contains("You have 1 new message"));
        wire::send_lines(&mut helper, &wire::printed_line(&token)).unwrap();
        served.join().unwrap();
    }
    let fates = outlet.0.lock().unwrap().clone();
    assert_eq!(fates.len(), 2);
    for (session, token, fate) in fates {
        settle(&*host, &session, &token, fate).await;
    }
    for session in ["S1", "S2"] {
        offer_once(&*host, session, Offer::Restored, 0, |_, _| async {
            panic!("already told; no duplicate push")
        })
        .await;
    }
}

#[tokio::test]
async fn mark_unread_by_human_is_one_scoped_batch_and_one_event() {
    let fake = FakeHost::new();
    let host = port(&fake);
    let id = sent_id(
        send_message(
            &host,
            Some("S2"),
            "human",
            "hello",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let team = launch_team("default");
    mark_read_many_by_human(&*host, &team, vec![id])
        .await
        .unwrap();
    let before = *fake.changed.lock().unwrap();
    assert_eq!(
        mark_unread_many_by_human(&*host, &team, vec![id, id])
            .await
            .unwrap(),
        1
    );
    assert_eq!(*fake.changed.lock().unwrap(), before + 1);
    assert_eq!(
        mark_unread_many_by_human(&*host, &team, vec![id])
            .await
            .unwrap(),
        0
    );
    assert_eq!(*fake.changed.lock().unwrap(), before + 1);
    let view = list_messages(&*host, &team, super::InboxScope::ToHuman, None, &[])
        .await
        .unwrap();
    assert_eq!(view.human_unread, 1);
}

#[tokio::test]
async fn implicit_unread_write_uses_the_calling_session_team_not_the_ui_team() {
    let fake = FakeHost::new();
    let host = port(&fake);
    let mut ids = Vec::new();
    for team in ["default", "design"] {
        fake.teams.lock().unwrap().insert("H2".into(), team.into());
        let id = sent_id(
            send_message(
                &host,
                Some("S2"),
                "human",
                team,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap(),
        );
        mark_read_many_by_human(&*host, &launch_team(team), vec![id])
            .await
            .unwrap();
        ids.push(id);
    }
    assert_eq!(ids[0], ids[1], "message ids overlap between team stores");
    fake.teams
        .lock()
        .unwrap()
        .insert("H2".into(), "default".into());
    fake.switch_team(Some("design"));
    assert_eq!(
        crate::mcp::handler::mark_unread_for_caller(&*host, None, Some("S2"), vec![ids[0]])
            .await
            .unwrap(),
        1
    );
    for (team, unread) in [("default", 1), ("design", 0)] {
        let view = list_messages(
            &*host,
            &launch_team(team),
            super::InboxScope::ToHuman,
            None,
            &[],
        )
        .await
        .unwrap();
        assert_eq!(view.human_unread, unread, "{team}");
    }
    // The explicit / authenticated remote team takes precedence over the local tab.
    assert_eq!(
        crate::mcp::handler::mark_unread_for_caller(
            &*host,
            Some(InboxTeam {
                org: launch_team("design").org,
                team: "design".into()
            }),
            Some("S2"),
            vec![ids[1]]
        )
        .await
        .unwrap(),
        1
    );
}

#[test]
fn mail_waiting_during_a_turn_starts_no_new_turn_until_native_cap_acceptance() {
    exercise_cap_hold_lift(false);
}

#[test]
fn unknown_usage_stays_held_through_pending_rate_until_native_exact_acceptance() {
    exercise_cap_hold_lift(true);
}

fn exercise_cap_hold_lift(price_unknown: bool) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    use crate::session_usage::{
        event::Tokens,
        spend_caps::{self, Budget},
        tests::event,
        UsageStore,
    };
    use codesense_pty_core::{
        agent_cli::{orion::Orion, Channel, SubmitOutcome},
        domain::InboxWaiterState,
        domain::TerminalDomain,
        inbox_host::{Fate, InboxOutlet},
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    struct Outlet;
    impl InboxOutlet for Outlet {
        fn fate(&self, _: &str, _: &str, _: Fate) {}
        fn waiter(&self, _: InboxWaiterState) {}
        fn audience(&self, _: &str) -> Option<String> {
            None
        }
        fn has_tools(&self, _: &str) -> Option<bool> {
            Some(true)
        }
    }
    struct Turns(Arc<AtomicUsize>);
    impl Channel for Turns {
        fn start_turn(&mut self, _: &str, _: &str) -> SubmitOutcome {
            self.0.fetch_add(1, Ordering::SeqCst);
            SubmitOutcome::Accepted
        }
    }
    let fake = FakeHost::new();
    let host = port(&fake);
    let home = fake.dir.path().join("engine");
    crate::routines::test_support::write_org(&home, 1);
    let principal = crate::domain::Principal::from(1u64);
    let key = crate::agents::AgentKey::new(
        crate::routines::test_support::ORG,
        crate::routines::test_support::TEAM,
        "default",
    );
    let mut agent = crate::agents::service::get(&home, &principal, &key)
        .unwrap()
        .agent;
    agent.budget_usd = Some(Budget {
        per_day: Some(1.0),
        per_week: None,
    });
    let saved = crate::agents::service::save(
        &home,
        &principal,
        crate::agents::service::SaveTarget::Update {
            key: key.clone(),
            base_revision: agent.revision,
        },
        agent,
        &crate::routines::Author::user("person"),
    )
    .unwrap();
    runtime
        .block_on(send_message(
            &host,
            Some("S2"),
            "H1",
            "waiting while working",
            None,
            None,
            None,
            None,
            None,
        ))
        .unwrap();
    let server_root = codesense_session_server::testing::short_root();
    let sidecar = codesense_session_server::testing::TestServer::start_at(server_root.path());
    sidecar.host().start_activity(server_root.path()).unwrap();
    sidecar.host().require_wake_holds();
    let client = crate::terminal::session_server::SessionServerClient::connect(
        server_root.path(),
        Arc::new(codesense_session_server::testing::RecordingSink::default()),
        Arc::default(),
        None,
    )
    .unwrap();
    sidecar.track("S1");
    client
        .spawn(
            codesense_session_server::testing::sh("sleep 60"),
            codesense_session_server::testing::registration("S1", None, false),
        )
        .unwrap();
    let inbox = sidecar.host().terminal().activity().inbox().clone();
    inbox.set_submitter("S1", Some(false));
    let hosts: Vec<Arc<dyn TerminalDomain>> = vec![client];
    let push = || {
        spend_caps::push_session_holds(
            &spend_caps::evaluate(&home, &principal),
            [("S1".into(), Some(format!("{key}@1")))],
            &hosts,
        )
        .unwrap()
    };
    push();
    runtime.block_on(crate::commands::host_inbox::offer_once(
        &*host,
        "S1",
        crate::commands::host_inbox::Offer::New,
        0,
        |notice, token| {
            inbox.mail("S1", notice, token);
            async { Ok(()) }
        },
    ));
    let mut usage = UsageStore::open(&home.join("usage.sqlite")).unwrap();
    usage
        .attribute("S1", "org", None, &key.to_string(), "H1", None)
        .unwrap();
    let mut call = event(
        "mid-turn",
        "S1",
        if price_unknown {
            "unknown-review-model"
        } else {
            "claude-opus-5-5"
        },
        chrono::Utc::now().timestamp_millis(),
        Tokens {
            input: 1_000,
            ..Default::default()
        },
    );
    call.reported_cost_usd = if price_unknown { None } else { Some(2.0) };
    usage.ingest(&[call]).unwrap();
    push();
    // The turn ends only after usage has reached the host over its real protocol.
    inbox.set_submitter("S1", Some(true));
    assert!(!inbox.wake_allowed("S1"));
    let turns = Arc::new(AtomicUsize::new(0));
    let submit = || {
        inbox.submit(&Outlet, "S1", &Orion, || {
            Some(Box::new(Turns(turns.clone())))
        })
    };
    submit();
    inbox.recover(&Outlet, "S1");
    assert_eq!(turns.load(Ordering::SeqCst), 0);
    assert!(
        fake.rows()
            .iter()
            .any(|m| m.text == "waiting while working" && m.read_at_ms.is_none()),
        "mail remains durably unread"
    );
    if price_unknown {
        let request = crate::session_usage::SetModelRateParams {
            model_prefix: "unknown-review-model".into(),
            effective_from: chrono::Local::now().date_naive().to_string(),
            input_per_mtok: 1.25,
            output_per_mtok: 7.5,
            cache_read_per_mtok: Some(0.125),
            long: None,
        };
        assert_eq!(
            usage
                .request_rate(request.clone(), crate::session_usage::SET_BY_AGENT)
                .unwrap(),
            0
        );
        let value = spend_caps::evaluate(&home, &principal).unwrap();
        assert_eq!(
            value.agents[&key.to_string()].pending_rates,
            vec![request.clone()]
        );
        push();
        submit();
        assert_eq!(turns.load(Ordering::SeqCst), 0);
        usage
            .accept_rate(&request.model_prefix, &request.effective_from, &request)
            .unwrap();
        assert!(
            spend_caps::evaluate(&home, &principal).unwrap().agents[&key.to_string()]
                .pending_rates
                .is_empty()
        );
        push();
        submit();
        assert!(inbox.wake_allowed("S1"));
        assert_eq!(turns.load(Ordering::SeqCst), 1);
        return;
    }
    let mut raised = saved.agent;
    raised.budget_usd = Some(Budget {
        per_day: Some(10.0),
        per_week: None,
    });
    let raised = crate::agents::service::save(
        &home,
        &principal,
        crate::agents::service::SaveTarget::Update {
            key: key.clone(),
            base_revision: raised.revision,
        },
        raised,
        &crate::routines::Author::user("person"),
    )
    .unwrap();
    assert!(
        spend_caps::status(&home, "agent", &key.to_string(), raised.agent.budget_usd)
            .unwrap()
            .refusal
            .is_some()
    );
    spend_caps::accept_current(
        &home,
        &principal,
        "agent",
        &key.to_string(),
        raised.agent.budget_usd,
    )
    .unwrap();
    let allowed =
        spend_caps::status(&home, "agent", &key.to_string(), raised.agent.budget_usd).unwrap();
    assert!(allowed.refusal.is_none());
    push();
    submit();
    assert_eq!(turns.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_desktop_hold_keeps_mail_stored_without_offering_it_to_an_older_host() {
    use crate::commands::host_inbox::{offer_once, Offer};
    let fake = FakeHost::new();
    let host = port(&fake);
    send_message(
        &host,
        Some("S2"),
        "H1",
        "keep this mail",
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    *fake.automatic_refusal.lock().unwrap() =
        Some("agent worker: day spend $1, cap $1; terminal-server update needed".into());
    let pushed = std::cell::Cell::new(false);
    assert!(offer_once(&*host, "S1", Offer::New, 0, |_, _| {
        pushed.set(true);
        async { Ok(()) }
    })
    .await
    .is_some());
    assert!(!pushed.get());
    assert!(fake
        .rows()
        .iter()
        .any(|m| m.text == "keep this mail" && m.read_at_ms.is_none()));
    *fake.automatic_refusal.lock().unwrap() = None;
    offer_once(&*host, "S1", Offer::New, 0, |_, _| {
        pushed.set(true);
        async { Ok(()) }
    })
    .await;
    assert!(pushed.get());
}

#[tokio::test]
async fn capped_sessions_on_older_hosts_get_no_mail_offers_even_below_the_cap() {
    use crate::commands::host_inbox::{offer_once, Offer};
    use crate::session_usage::{spend_caps::Budget, UsageStore};
    use codesense_host_proto::{Capability, Hello};
    let fake = FakeHost::new();
    let host = port(&fake);
    let mut hello = Hello::default();
    hello.capabilities = vec![Capability::ActivityV2, Capability::InboxWaitV1];
    let old_host = crate::terminal::session_server::require_spend_hold_capability(&hello);
    assert!(old_host.is_err());
    send_message(
        &host,
        Some("S2"),
        "H1",
        "never queue this on an old host",
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let mut usage = UsageStore::open(&fake.dir.path().join("usage.sqlite")).unwrap();
    usage
        .attribute("old-host", "org", None, "worker", "H1", None)
        .unwrap();
    for spent in [0.25, 2.0] {
        let mut row = crate::session_usage::tests::event(
            "old-usage",
            "old-host",
            "claude-opus-5-5",
            chrono::Utc::now().timestamp_millis(),
            Default::default(),
        );
        row.reported_cost_usd = Some(spent);
        usage.ingest(&[row]).unwrap();
        let status = usage
            .cap_status(
                "agent",
                "worker",
                Budget {
                    per_day: Some(1.0),
                    per_week: None,
                },
                chrono::Local::now().date_naive(),
                false,
            )
            .unwrap();
        *fake.automatic_cap.lock().unwrap() = Some((status, old_host.clone()));
        let pushed = std::cell::Cell::new(false);
        offer_once(&*host, "S1", Offer::New, 0, |_, _| {
            pushed.set(true);
            async { Ok(()) }
        })
        .await;
        assert!(
            !pushed.get(),
            "spend={spent}: an old host must never queue capped mail"
        );
    }
    assert!(fake
        .rows()
        .iter()
        .any(|m| m.text == "never queue this on an old host" && m.read_at_ms.is_none()));
}

#[tokio::test]
async fn remote_routine_and_handleless_outbox_keep_the_captured_team() {
    for session in ["routine-on-machine", "finished-outbox"] {
        let fake = FakeHost::new();
        fake.switch_team(Some("design"));
        fake.teams
            .lock()
            .unwrap()
            .insert("H2".into(), "design".into());
        let host = port(&fake);
        crate::mcp::remote_authority::as_remote_context(
            Some((session.into(), session.into())),
            Some(launch_team("default")),
            async {
                sent_id(
                    send_message(
                        &host,
                        Caller::Authenticated(session),
                        "human",
                        "team A result",
                        None,
                        None,
                        None,
                        None,
                        None,
                    )
                    .await
                    .unwrap(),
                );
                assert_eq!(
                    session_store(host.as_ref(), session)
                        .await
                        .unwrap()
                        .team
                        .team,
                    "default"
                );
                assert_eq!(
                    session_store(host.as_ref(), "H2").await.unwrap().team.team,
                    "design"
                );
            },
        )
        .await;
        let rows = fake.rows_in("default");
        assert_eq!(rows.len(), 1, "{session}: captured team's inbox");
        assert_eq!(rows[0].from, session);
        assert!(
            fake.rows_in("design").is_empty(),
            "UI team must receive nothing"
        );
    }
}

#[tokio::test]
async fn media_only_send_and_reply_reach_both_page_read_models_and_retry_receipts() {
    let fake = FakeHost::new();
    let host = port(&fake);
    let media = crate::media::MediaInput {
        url: Some("https://i.giphy.com/cat.gif".into()),
        path: None,
        alt: "Typing cat".into(),
    };
    let sent = send_message_request(
        &host,
        Some("S1"),
        "H2",
        "",
        None,
        None,
        None,
        None,
        None,
        None,
        Some("media-send"),
        Some(&media),
    )
    .await
    .unwrap();
    let id = sent_id(sent);
    assert_eq!(
        sent_id(
            send_message_request(
                &host,
                Some("S1"),
                "H2",
                "",
                None,
                None,
                None,
                None,
                None,
                None,
                Some("media-send"),
                Some(&media)
            )
            .await
            .unwrap()
        ),
        id
    );
    let stored = fake.rows();
    assert_eq!(stored[0].attachments[0].alt, "Typing cat");
    assert_eq!(stored[0].attachments[0].state, "pending");
    let rows = list_messages_unbounded(&*host, &launch_team("default"), InboxScope::All, None, &[])
        .await
        .unwrap();
    assert_eq!(rows.messages[0].attachments, stored[0].attachments);
    let rollup = super::super::agent_read_model::inbox_rollup(&*host)
        .await
        .unwrap();
    assert_eq!(
        rollup
            .messages
            .iter()
            .find(|m| m.id == id)
            .unwrap()
            .attachments[0]
            .alt,
        "Typing cat"
    );
    let reply = sent_id(
        reply_request(
            &host,
            Some("S2"),
            id,
            "",
            None,
            None,
            None,
            Some("media-reply"),
            Some(&media),
        )
        .await
        .unwrap(),
    );
    assert_eq!(
        fake.rows()
            .iter()
            .find(|m| m.id == reply)
            .unwrap()
            .attachments[0]
            .alt,
        "Typing cat"
    );
    send_message_request(
        &host,
        Some("S1"),
        "human",
        "",
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(&media),
    )
    .await
    .unwrap();
    let rollup = super::super::agent_read_model::inbox_rollup(&*host)
        .await
        .unwrap();
    assert!(rollup
        .conversations
        .iter()
        .any(|c| c.last_preview == "GIF · Typing cat"));
    let changed = crate::media::MediaInput {
        alt: "Different cat".into(),
        ..media
    };
    assert!(is_invalid(
        &reply_request(
            &host,
            Some("S2"),
            id,
            "",
            None,
            None,
            None,
            Some("media-reply"),
            Some(&changed)
        )
        .await
        .unwrap()
    ));
}

#[test]
fn caller_media_workspace_resolves_authenticated_handles_before_session_ids() {
    let rows = || {
        vec![
            CallerMediaWorkspaceRow {
                handle: "agent-a".into(),
                session_id: Some("pty-a".into()),
                worktree_dir: Some(PathBuf::from("/work/a")),
                is_remote: false,
            },
            CallerMediaWorkspaceRow {
                handle: "agent-b".into(),
                session_id: Some("pty-b".into()),
                worktree_dir: Some(PathBuf::from("/work/b")),
                is_remote: false,
            },
            CallerMediaWorkspaceRow {
                handle: "remote-agent".into(),
                session_id: Some("remote-pty".into()),
                worktree_dir: Some(PathBuf::from("/remote/work")),
                is_remote: true,
            },
        ]
    };

    assert_eq!(
        caller_media_workspace_from_snapshot("agent-a", rows(), None, false),
        Some(PathBuf::from("/work/a")),
        "the authenticated handle resolves directly even when it differs from the PTY id"
    );
    assert_eq!(
        caller_media_workspace_from_snapshot("agent-b", rows(), None, false),
        Some(PathBuf::from("/work/b")),
        "a different authenticated handle resolves only its own worktree"
    );
    assert_eq!(
        caller_media_workspace_from_snapshot("pty-a", rows(), Some("agent-a"), false),
        Some(PathBuf::from("/work/a")),
        "a raw session id resolves only through its registered handle"
    );
    assert_eq!(
        caller_media_workspace_from_snapshot("unbound-pty", rows(), None, false),
        None,
        "an unbound session id cannot borrow a workspace"
    );
    assert_eq!(
        caller_media_workspace_from_snapshot("remote-agent", rows(), None, false),
        None,
        "remote rows never expose a local worktree"
    );
}

#[tokio::test]
async fn local_media_send_uses_sender_scope_when_ui_team_is_parent() {
    use crate::agents::content_reader::ContentReader;
    use crate::context_repo::{caller_scope::CallerScope, inheritance};

    let (_temp, root) = inheritance::test_fixture();
    let parent_only = root.join("parent/content/parent.gif");
    let child_image = root.join("child/content/child.gif");
    let workspace = _temp.path().join("child-session-worktree");
    std::fs::create_dir_all(&workspace).unwrap();
    let workspace_image = workspace.join("own-meme.gif");
    let gif = b"GIF89a\x02\x00\x03\x00";
    std::fs::write(&parent_only, gif).unwrap();
    std::fs::write(&child_image, gif).unwrap();
    std::fs::write(&workspace_image, gif).unwrap();

    let caller = CallerScope::Team(inheritance::team_view(&root, "acme/ctx", "child"));
    let ui = CallerScope::Team(inheritance::team_view(&root, "acme/ctx", "parent"));
    assert!(ui.open(&parent_only).is_ok());
    assert!(caller.open(&parent_only).is_err());
    assert!(caller.open(&child_image).is_ok());

    // The UI sees the parent team, while the authenticated sender belongs to child.
    let fake = FakeHost::new();
    fake.switch_team(Some("parent"));
    fake.known_teams
        .lock()
        .unwrap()
        .extend(["parent".into(), "child".into()]);
    fake.teams
        .lock()
        .unwrap()
        .insert("H1".into(), "child".into());
    fake.media_scopes
        .lock()
        .unwrap()
        .insert("H1".into(), caller);
    fake.media_workspaces
        .lock()
        .unwrap()
        .insert("H1".into(), workspace.clone());
    let host = port(&fake);
    let send = |path: &std::path::Path| crate::media::MediaInput {
        url: None,
        path: Some(path.to_string_lossy().into_owned()),
        alt: "Local meme".into(),
    };

    let parent = send_message_request(
        &host,
        Some("S1"),
        "human",
        "",
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(&send(&parent_only)),
    )
    .await
    .unwrap();
    assert!(
        is_invalid(&parent),
        "sender scope must refuse the parent-only image"
    );

    let child = send_message_request(
        &host,
        Some("S1"),
        "human",
        "",
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(&send(&child_image)),
    )
    .await
    .unwrap();
    assert!(matches!(child, SendResult::Sent { .. }));

    let own_workspace = send_message_request(
        &host,
        Some("S1"),
        "human",
        "",
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(&send(&workspace_image)),
    )
    .await
    .unwrap();
    assert!(matches!(own_workspace, SendResult::Sent { .. }));

    let incoming = sent_id(
        send_message(
            &host,
            Some("S2"),
            "H1",
            "Reply with a meme",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    assert!(is_invalid(
        &reply_request(
            &host,
            Some("S1"),
            incoming,
            "",
            None,
            None,
            None,
            None,
            Some(&send(&parent_only)),
        )
        .await
        .unwrap()
    ));

    let queued = sent_id(
        send_message(
            &host,
            Some("S2"),
            "H1",
            "Reply through the role queue",
            None,
            None,
            None,
            Some("role:manager"),
            None,
        )
        .await
        .unwrap(),
    );
    let role_reply = reply_request(
        &host,
        Some("S1"),
        queued,
        "",
        None,
        None,
        None,
        None,
        Some(&send(&parent_only)),
    )
    .await;
    assert!(match role_reply {
        Err(message) => message == "local media is outside sender workspace",
        Ok(SendResult::Invalid { .. }) => true,
        Ok(SendResult::Sent { .. }) => false,
    });
}

async fn review_reply_after_local_source_removed(role: bool) {
    let fake = FakeHost::new();
    let host = port(&fake);
    let original = sent_id(
        send_message(
            &host,
            Some("S1"),
            "H2",
            "Question",
            None,
            None,
            None,
            role.then_some("role:manager"),
            None,
        )
        .await
        .unwrap(),
    );
    let image = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(image.path(), b"GIF89a\x02\x00\x03\x00").unwrap();
    let media = crate::media::MediaInput {
        url: None,
        path: Some(image.path().to_string_lossy().into_owned()),
        alt: "Local meme".into(),
    };
    let answer = || {
        reply_request(
            &host,
            Some("S2"),
            original,
            "",
            None,
            None,
            None,
            Some("local-reply"),
            Some(&media),
        )
    };
    let first = sent_id(answer().await.unwrap());
    std::fs::remove_file(image.path()).unwrap();
    assert_eq!(sent_id(answer().await.unwrap()), first);
    assert_eq!(fake.rows().len(), 2);
}
#[tokio::test]
async fn review_09_session_reply_replays_without_source_file() {
    review_reply_after_local_source_removed(false).await;
}
#[tokio::test]
async fn review_09_role_reply_replays_without_source_file() {
    review_reply_after_local_source_removed(true).await;
}

#[tokio::test]
async fn a_note_carries_its_click_action_and_kind_and_a_request_id_stores_it_once() {
    let fake = FakeHost::new();
    let host = port(&fake);
    let team = (ORG.to_string(), "default".to_string());
    let click = || MessageOnClick::GotoAgent {
        agent_id: "worker".into(),
    };
    let send = || async {
        note_to_human_with(
            &host,
            Some(&team),
            "worker is waiting on a startup screen",
            "worker is waiting on a startup screen",
            NoteExtras {
                on_click: Some(click()),
                kind: Some(inbox::MessageKind::NeedsYou),
                request_id: Some("s-startup"),
            },
        )
        .await
        .unwrap()
    };
    let first = send().await;
    assert_eq!(send().await, first);
    let rows = fake.rows_in("default");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, Some(inbox::MessageKind::NeedsYou));
    assert_eq!(rows[0].on_click, Some(click()));
    // Without extras a note is the plain update it always was.
    let plain = note_to_human(&host, Some(&team), "done", "done")
        .await
        .unwrap();
    let rows = fake.rows_in("default");
    let plain = rows.iter().find(|row| row.id == plain).unwrap();
    assert_eq!(plain.kind, Some(inbox::MessageKind::Update));
    assert_eq!(plain.on_click, None);
}

// SV-INP-15: reactions.

fn reacted(result: ReactResult) -> crate::weft::reactions::ReactOutcome {
    match result {
        ReactResult::Reacted { outcome, .. } => outcome,
        other => panic!("expected a reaction, got {other:?}"),
    }
}

async fn human_view(host: &Arc<FakeHost>) -> InboxView {
    list_messages(
        &**host,
        &launch_team("default"),
        InboxScope::ToHuman,
        None,
        &[],
    )
    .await
    .unwrap()
}

async fn mail_to_human(host: &Arc<FakeHost>, text: &str) -> i64 {
    sent_id(
        send_message(
            &port(host),
            Some("S1"),
            "human",
            text,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    )
}

#[tokio::test]
async fn a_human_reaction_toggles_shows_on_the_row_and_is_not_mail() {
    let fake = FakeHost::new();
    let id = mail_to_human(&fake, "ready for review").await;
    let before = human_view(&fake).await;
    assert_eq!(before.human_unread, 1);

    let added = react_from_human(&*fake, &launch_team("default"), id, "👍", None)
        .await
        .unwrap();
    assert_eq!(reacted(added), crate::weft::reactions::ReactOutcome::Added);
    let after = human_view(&fake).await;
    let row = after.messages.iter().find(|m| m.message_id == id).unwrap();
    assert_eq!(row.reactions.len(), 1);
    assert_eq!(row.reactions[0].emoji, "👍");
    assert_eq!(row.reactions[0].reactors, vec!["human".to_string()]);
    assert!(row.unread, "a reaction does not read the message");
    assert_eq!(after.human_unread, before.human_unread);
    assert_eq!(
        after.messages.len(),
        before.messages.len(),
        "no row of its own"
    );

    let events = fake.inbox_events.lock().unwrap().clone();
    assert!(
        events
            .iter()
            .any(|e| matches!(e.kind, InboxChange::Reacted) && e.message_ids == vec![id]),
        "the change reaches the pane through the inbox event: {events:?}"
    );

    let removed = react_from_human(&*fake, &launch_team("default"), id, "👍", None)
        .await
        .unwrap();
    assert_eq!(
        reacted(removed),
        crate::weft::reactions::ReactOutcome::Removed
    );
    let gone = human_view(&fake).await;
    assert!(gone
        .messages
        .iter()
        .find(|m| m.message_id == id)
        .unwrap()
        .reactions
        .is_empty());
}

#[tokio::test]
async fn reactions_count_distinct_reactors_and_an_agent_reacts_with_any_emoji() {
    let fake = FakeHost::new();
    let id = mail_to_human(&fake, "shipped").await;
    react_from_human(&*fake, &launch_team("default"), id, "🧑🏽‍💻", None)
        .await
        .unwrap();
    // H2 may react to mail it can see; here the message is to the human, so make H2 a
    // party by mailing it and reacting to that instead.
    let to_h2 = sent_id(
        send_message(
            &port(&fake),
            Some("S1"),
            "H2",
            "your turn",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let first = react_request(&*fake, Some("S2"), to_h2, "🧑🏽‍💻", None)
        .await
        .unwrap();
    assert_eq!(reacted(first), crate::weft::reactions::ReactOutcome::Added);
    let again = react_request(&*fake, Some("S2"), to_h2, "🧑🏽‍💻", Some(false))
        .await
        .unwrap();
    assert_eq!(
        reacted(again),
        crate::weft::reactions::ReactOutcome::Unchanged
    );
    let view = list_messages(&*fake, &launch_team("default"), InboxScope::All, None, &[])
        .await
        .unwrap();
    let row = view
        .messages
        .iter()
        .find(|m| m.message_id == to_h2)
        .unwrap();
    assert_eq!(row.reactions.len(), 1);
    assert_eq!(row.reactions[0].count, 1);
    assert_eq!(row.reactions[0].reactors, vec!["H2".to_string()]);
}

#[tokio::test]
async fn an_agent_reacts_only_to_mail_it_is_party_to_and_with_one_emoji() {
    let fake = FakeHost::new();
    fake.tab("H3", "S3", "Third", true);
    let private = sent_id(
        send_message(
            &port(&fake),
            Some("S1"),
            "H2",
            "between us",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let refused = react_request(&*fake, Some("S3"), private, "👍", None)
        .await
        .unwrap();
    assert!(
        matches!(&refused, ReactResult::Invalid { message } if message.contains("no message")),
        "{refused:?}"
    );
    for bad in ["ab", "👍👍", "", "hello"] {
        let refused = react_request(&*fake, Some("S2"), private, bad, None)
            .await
            .unwrap();
        assert!(matches!(refused, ReactResult::Invalid { .. }), "{bad:?}");
    }
    let sender = react_request(&*fake, Some("S1"), private, "👍", None)
        .await
        .unwrap();
    assert_eq!(reacted(sender), crate::weft::reactions::ReactOutcome::Added);
}

#[tokio::test]
async fn an_agent_reaction_lands_in_the_callers_team_not_the_ui_selected_team() {
    let fake = FakeHost::new();
    fake.teams
        .lock()
        .unwrap()
        .insert("H2".into(), "design".into());
    let in_design = sent_id(
        send_message(
            &port(&fake),
            Some("S1"),
            "H2",
            "in design",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    fake.switch_team(Some("default"));
    let result = react_request(&*fake, Some("S2"), in_design, "🎉", None)
        .await
        .unwrap();
    assert_eq!(reacted(result), crate::weft::reactions::ReactOutcome::Added);
    let design = WeftStore::open(fake.team_store("design")).unwrap();
    assert_eq!(
        crate::weft::reactions::reactions_all(&design)
            .unwrap()
            .len(),
        1
    );
    let default = WeftStore::open(fake.team_store("default")).unwrap();
    assert!(crate::weft::reactions::reactions_all(&default)
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn the_sender_hears_of_a_reaction_once_without_mail_and_not_after_an_un_react() {
    let fake = FakeHost::new();
    let kept = mail_to_human(&fake, "first").await;
    let withdrawn = mail_to_human(&fake, "second").await;
    let team = launch_team("default");
    react_from_human(&*fake, &team, kept, "👍", None)
        .await
        .unwrap();
    react_from_human(&*fake, &team, withdrawn, "🙏", None)
        .await
        .unwrap();
    react_from_human(&*fake, &team, withdrawn, "🙏", None)
        .await
        .unwrap();

    assert!(
        read_messages(&*fake, Some("S1")).await.unwrap().is_empty(),
        "a reaction is not mail"
    );
    let notices = take_reaction_notices(&*fake, Some("S1")).await.unwrap();
    assert_eq!(notices.len(), 1);
    assert_eq!(
        (
            notices[0].message_id,
            notices[0].emoji.as_str(),
            notices[0].reactor.as_str()
        ),
        (kept, "👍", "human")
    );
    assert!(take_reaction_notices(&*fake, Some("S1"))
        .await
        .unwrap()
        .is_empty());
    assert!(take_reaction_notices(&*fake, Some("S2"))
        .await
        .unwrap()
        .is_empty());
    assert_eq!(human_view(&fake).await.human_unread, 2);
}

#[tokio::test]
async fn a_sender_in_another_team_hears_of_a_reaction_through_read_messages() {
    use crate::mcp::handler::{ReactParams, ReadMessagesParams};
    let fake = FakeHost::new();
    fake.teams
        .lock()
        .unwrap()
        .insert("H2".into(), "design".into());
    // A (default) mails B (design): the row lives in design's store.
    let sent = sent_id(
        send_message(
            &port(&fake),
            Some("S1"),
            "H2",
            "for design",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    fake.switch_team(Some("default"));
    let react: ReactParams = serde_json::from_value(
        serde_json::json!({"messageId": sent, "emoji": "🎉", "sessionId": "S2"}),
    )
    .unwrap();
    crate::mcp::caller_tools::react(&*fake, Caller::Named(Some("S2")), react)
        .await
        .unwrap();

    let read = crate::mcp::caller_tools::read_messages(
        &*fake,
        Caller::Named(Some("S1")),
        ReadMessagesParams { session_id: None },
    )
    .await
    .unwrap();
    let body: serde_json::Value =
        serde_json::from_str(&read.content[0].as_text().unwrap().text).unwrap();
    let reactions = body["reactions"].as_array().expect("a reactions list");
    assert_eq!(reactions.len(), 1, "{body}");
    assert_eq!(reactions[0]["messageId"], sent);
    assert_eq!(reactions[0]["reactor"], "H2");
    assert_eq!(reactions[0]["emoji"], "🎉");
    assert_eq!(
        reactions[0]["team"],
        serde_json::json!({"org": ORG, "team": "design"})
    );
    // Once only, and not for the reactor.
    let again = crate::mcp::caller_tools::read_messages(
        &*fake,
        Caller::Named(Some("S1")),
        ReadMessagesParams { session_id: None },
    )
    .await
    .unwrap();
    let again: serde_json::Value =
        serde_json::from_str(&again.content[0].as_text().unwrap().text).unwrap();
    assert!(again.get("reactions").is_none(), "{again}");
}
