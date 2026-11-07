// SPDX-License-Identifier: MIT
use std::path::PathBuf;
use std::sync::Mutex;

use super::*;
use crate::commands::agent_inbox::{KnownSession, KnownSessions};
use crate::mcp::session_list::SessionState;
use crate::weft::inbox;
use crate::weft::role_state::RoleDeliveryState;

const ORG: &str = "acme";

/// Two sessions in team `default` (also the active team): H1 (session S1, "Manager")
/// runs saved Agent `acme/default/reviewer`; H2 (session S2, "Sprint 1") runs nothing.
/// Team `alias` resolves to `default`'s store; team `broken`'s store is a directory,
/// which cannot be opened.
struct FakeHost {
    dir: tempfile::TempDir,
    changed: Mutex<u32>,
    events: Mutex<Vec<crate::live_hub::inbox_events::InboxEvent>>,
    /// Every `(team, role, message id)` queued for a role, in order.
    queued: Mutex<Vec<(String, String, i64)>>,
    /// When set, listing the synced teams fails.
    synced_fails: std::sync::atomic::AtomicBool,
    /// When set, resolving the active team fails.
    active_fails: std::sync::atomic::AtomicBool,
    /// When set, the synced teams listed instead of the usual three.
    synced_only: Mutex<Option<Vec<&'static str>>>,
    remote: super::super::dashboard_launch::DashboardLaunchState,
    remote_online: std::sync::atomic::AtomicBool,
    projected: Mutex<Option<InboxView>>,
    role_dirs: Mutex<std::collections::HashMap<String, PathBuf>>,
}

impl FakeHost {
    fn new() -> Arc<Self> {
        Arc::new(FakeHost {
            dir: tempfile::tempdir().unwrap(),
            projected: Mutex::default(),
            role_dirs: Mutex::default(),
            changed: Mutex::default(),
            events: Mutex::default(),
            queued: Mutex::default(),
            synced_fails: Default::default(),
            active_fails: Default::default(),
            synced_only: Mutex::default(),
            remote: super::super::dashboard_launch::DashboardLaunchState::new(4),
            remote_online: std::sync::atomic::AtomicBool::new(true),
        })
    }

    fn store(&self, team: &str) -> WeftStore {
        WeftStore::open(self.path(team)).unwrap()
    }

    fn path(&self, team: &str) -> PathBuf {
        self.dir.path().join(team).join("weft.sqlite3")
    }
}

#[async_trait::async_trait]
impl InboxHost for FakeHost {
    fn projected_inbox(&self, team: &LaunchTeam) -> Option<Result<InboxView, String>> {
        if team.team != "default" {
            return None;
        }
        self.projected.lock().unwrap().clone().map(Ok)
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
        let dir = self
            .role_dirs
            .lock()
            .unwrap()
            .get(&team.team)
            .cloned()
            .unwrap_or_else(|| self.dir.path().join(&team.team));
        crate::queue_roles::list_for(&dir, scope)
    }

    async fn team_store(&self, team: &LaunchTeam) -> Result<PathBuf, String> {
        if team.org != ORG {
            return Err(format!("unknown team {}:{}", team.org, team.team));
        }
        match team.team.as_str() {
            "default" | "design" => Ok(self.path(&team.team)),
            "alias" => Ok(self.path("default")),
            "broken" => {
                let path = self.path("broken");
                std::fs::create_dir_all(&path).unwrap();
                Ok(path)
            }
            _ => Err(format!("unknown team {}:{}", team.org, team.team)),
        }
    }
    async fn active_team(&self) -> Result<LaunchTeam, String> {
        if self.active_fails.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("no active team".into());
        }
        Ok(team("default"))
    }
    fn launch_team(&self, handle: &str) -> Option<LaunchTeam> {
        if let Some(team) = self.remote.launch_team(handle) {
            return Some(team);
        }
        match handle {
            "H1" | "H2" => Some(team("default")),
            "H3" => Some(team("design")),
            // A session launched in another org than the active one.
            "H9" => Some(LaunchTeam {
                org: "beta".into(),
                team: "default".into(),
            }),
            _ => None,
        }
    }
    fn handle_for_session(&self, session_id: &str) -> Option<String> {
        match session_id {
            "S1" => Some("H1".into()),
            "S2" => Some("H2".into()),
            "S3" => Some("H3".into()),
            "S9" => Some("H9".into()),
            _ => self.remote.remote_handle_for_session(session_id),
        }
    }
    fn has_handle(&self, handle: &str) -> bool {
        matches!(handle, "H1" | "H2" | "H3") || self.remote.remote_shell_by_handle(handle).is_some()
    }
    fn display_name(&self, handle: &str) -> Option<String> {
        match handle {
            "H1" => Some("Manager".into()),
            "H2" => Some("Sprint 1".into()),
            _ => None,
        }
    }
    fn inbox_changed(&self) {
        *self.changed.lock().unwrap() += 1;
    }
    fn inbox_event(&self, event: crate::live_hub::inbox_events::InboxEvent) {
        self.events.lock().unwrap().push(event);
        self.inbox_changed();
    }
    fn role_message_queued(&self, team: &LaunchTeam, role: &str, message_id: i64, _sender: &str) {
        self.queued
            .lock()
            .unwrap()
            .push((team.team.clone(), role.to_string(), message_id));
    }
    async fn synced_teams(&self) -> Result<Vec<LaunchTeam>, String> {
        if self.synced_fails.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("contexts unreadable".into());
        }
        if let Some(only) = self.synced_only.lock().unwrap().clone() {
            return Ok(only.into_iter().map(team).collect());
        }
        // `missing` is synced but its inbox was never written: it is skipped, not created.
        Ok(vec![team("default"), team("design"), team("missing")])
    }
    async fn sessions(&self) -> Result<KnownSessions, String> {
        let session = |handle: &str, session: &str, name: &str| KnownSession {
            handle: handle.into(),
            session: Some(session.into()),
            name: name.into(),
            running: true,
        };
        let mut sessions = vec![
            session("H1", "S1", "Manager"),
            session("H2", "S2", "Sprint 1"),
            session("H3", "S3", "H3"),
        ];
        sessions.extend(
            self.remote
                .remote_shells_on_machine("test-box")
                .into_iter()
                .map(|record| KnownSession {
                    handle: record.handle,
                    session: Some(record.target.session.as_str().to_string()),
                    name: "Remote worker".into(),
                    running: self.remote_online.load(std::sync::atomic::Ordering::SeqCst),
                }),
        );
        Ok(KnownSessions::new(sessions))
    }
}

fn team(name: &str) -> LaunchTeam {
    LaunchTeam {
        org: ORG.into(),
        team: name.into(),
    }
}

const REVIEWER: &str = "acme/default/reviewer";

fn row(handle: &str, name: &str, agent: Option<&str>) -> SessionListRow {
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
        state: SessionState::Idle,
        launched_by: None,
        story: None,
        workspace: None,
        role: None,
        avatar: None,
        agent: agent.map(String::from),
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
        session: None,
        failed: false,
    }
}

fn listed() -> Vec<SessionListRow> {
    vec![
        row("H1", "Manager", Some(REVIEWER)),
        row("H2", "Sprint 1", None),
    ]
}

fn insert(store: &WeftStore, from: &str, to: &str, text: &str, reply: Option<i64>) -> i64 {
    inbox::inbox_insert(store, from, to, text, reply, None, None, None, None).unwrap()
}

fn ids(view: &InboxView) -> Vec<i64> {
    view.messages.iter().map(|m| m.message_id).collect()
}

#[tokio::test]
async fn listing_the_inbox_marks_nothing_read() {
    let host = FakeHost::new();
    let store = host.store("default");
    let to_human = insert(&store, "H1", HUMAN, "need you", None);
    let to_h2 = insert(&store, "H1", "H2", "hi sprint", None);
    let default = team("default");

    for _ in 0..2 {
        let all = inbox(
            &*host,
            &default,
            &InboxQuery {
                scope: Some(InboxScope::All),
                ..Default::default()
            },
            &listed(),
        )
        .await
        .unwrap();
        assert_eq!(ids(&all), vec![to_h2, to_human]);
        assert!(all.messages.iter().all(|m| m.unread));
        assert_eq!(all.human_unread, 1);
    }
    // The session's own unread mail is still there for read_messages to drain.
    assert_eq!(inbox::inbox_unread(&store, "H2").unwrap().count, 1);
    assert_eq!(
        *host.changed.lock().unwrap(),
        0,
        "a read announces no change"
    );
}

#[tokio::test]
async fn the_inbox_filters_by_scope_agent_session_and_role() {
    let host = FakeHost::new();
    let store = host.store("default");
    let to_human = insert(&store, "H1", HUMAN, "from the reviewer", None);
    let to_h2 = insert(&store, HUMAN, "H2", "for sprint 1", None);
    let (to_role, _) = crate::weft::role_inbox::send_role_message_commit(
        &store,
        HUMAN,
        REVIEWER,
        "please review",
        None,
        None,
        None,
        "",
        1,
        None,
        None,
        None,
    )
    .unwrap();
    let to_foreman = insert(&store, "H2", "foreman", "status", None);
    let reply = insert(&store, "H1", HUMAN, "on it", Some(to_role));
    let default = team("default");
    let list = |query: InboxQuery| {
        let host = host.clone();
        let default = default.clone();
        async move { inbox(&*host, &default, &query, &listed()).await.unwrap() }
    };

    // No filter: the human's inbox.
    assert_eq!(
        ids(&list(InboxQuery::default()).await),
        vec![reply, to_human]
    );
    // to-agents: everything not to the human.
    let agents = list(InboxQuery {
        scope: Some(InboxScope::ToAgents),
        ..Default::default()
    })
    .await;
    assert_eq!(ids(&agents), vec![to_foreman, to_role, to_h2]);
    // An agent: its role inbox, its running instance H1, and replies under them; the
    // default scope widens to all.
    let reviewer = list(InboxQuery {
        agent_id: Some(REVIEWER.into()),
        ..Default::default()
    })
    .await;
    assert_eq!(ids(&reviewer), vec![reply, to_role, to_human]);
    // A session, by its session id.
    let h2 = list(InboxQuery {
        session_id: Some("S2".into()),
        ..Default::default()
    })
    .await;
    assert_eq!(ids(&h2), vec![to_foreman, to_h2]);
    // A session no launch handle maps (a hand-opened tab, by its session id) and a
    // remote tab's handle: both directions are listed under the id itself.
    for unmapped in ["shell-hand-1", "remote-tab-xyz"] {
        let sent = insert(&store, unmapped, HUMAN, "from the tab", None);
        let received = insert(&store, HUMAN, unmapped, "to the tab", None);
        let view = list(InboxQuery {
            session_id: Some(unmapped.into()),
            ..Default::default()
        })
        .await;
        assert_eq!(ids(&view), vec![received, sent], "{unmapped}");
    }
    // A role.
    let foreman = list(InboxQuery {
        role: Some("foreman".into()),
        ..Default::default()
    })
    .await;
    assert_eq!(ids(&foreman), vec![to_foreman]);
    // The Reply-To spelling of a role reads the same queue.
    let prefixed = list(InboxQuery {
        role: Some(" role:foreman ".into()),
        ..Default::default()
    })
    .await;
    assert_eq!(ids(&prefixed), vec![to_foreman]);
    // A role that is only the prefix names no queue: nothing, not every message.
    for empty in ["role:", " role: "] {
        let view = list(InboxQuery {
            role: Some(empty.into()),
            ..Default::default()
        })
        .await;
        assert_eq!(ids(&view), Vec::<i64>::new(), "{empty:?}");
    }
    // The role message carries its delivery; a limit keeps the newest.
    let newest = list(InboxQuery {
        agent_id: Some(REVIEWER.into()),
        limit: Some(2),
        ..Default::default()
    })
    .await;
    assert_eq!(ids(&newest), vec![reply, to_role]);
    let queued = newest.messages[1].delivery.as_ref().unwrap();
    assert_eq!(queued.state, inbox::InboxDeliveryState::Queued);
}

#[tokio::test]
async fn role_state_names_the_role_its_instances_and_senders() {
    let host = FakeHost::new();
    let store = host.store("default");
    crate::weft::role_inbox::send_role_message_commit(
        &store,
        "H2",
        REVIEWER,
        "look at this",
        None,
        None,
        None,
        "",
        1,
        None,
        None,
        None,
    )
    .unwrap();
    let now = chrono::Utc::now().timestamp();
    let reg = crate::weft::role_inbox::register_role(&store, REVIEWER, "S1", now).unwrap();
    crate::weft::role_inbox::claim_role_messages(&store, REVIEWER, "S1", reg.generation, 1, now)
        .unwrap();

    let names = Names::from_sessions(&listed());
    crate::queue_roles::save(
        &host.dir.path().join("default"),
        crate::queue_roles::RoleConfig {
            id: REVIEWER.into(),
            name: "Reviewer".into(),
            icon: None,
            default_handler: None,
        },
    )
    .unwrap();
    let view = roles(&*host, &team("default"), None, None, &names)
        .await
        .unwrap();
    assert_eq!(view.roles.len(), 1);
    let q = &view.roles[0];
    assert_eq!(q.role, REVIEWER);
    assert_eq!(q.role_name.as_deref(), Some("Reviewer"));
    assert_eq!(q.registrations[0].instance_name.as_deref(), Some("Manager"));
    assert!(q.registrations[0].live);
    let d = &q.deliveries[0];
    assert_eq!(d.state, RoleDeliveryState::Claimed);
    assert_eq!(d.from_name.as_deref(), Some("Sprint 1"));
    assert_eq!(d.claim_owner.as_deref(), Some("S1"));
    assert_eq!(d.claim_owner_name.as_deref(), Some("Manager"));
    assert_eq!(q.counts.claimed, 1);

    // The serialized shape is what the bridge and the MCP tool both return.
    let json = serde_json::to_value(&view).unwrap();
    let role = &json["roles"][0];
    assert_eq!(role["roleName"], "Reviewer");
    assert_eq!(role["registrations"][0]["instanceName"], "Manager");
    assert_eq!(role["deliveries"][0]["state"], "claimed");
    assert!(role["deliveries"][0]["claimExpiresAtMs"].is_i64());
}

#[tokio::test]
async fn the_human_sends_to_a_session_or_to_a_role() {
    let host = FakeHost::new();
    let port: Arc<dyn InboxHost> = host.clone();
    let default = team("default");

    // A stale selected team must fail before delivery, even to a live session.
    for to in ["@S2", REVIEWER] {
        let error = send_as_human(
            &port,
            &team("removed"),
            to,
            "hello",
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(error, "unknown team acme:removed");
        assert!(!host.path("default").exists());
        assert!(!host.path("removed").exists());
        assert_eq!(*host.changed.lock().unwrap(), 0);
        assert!(host.queued.lock().unwrap().is_empty());
    }

    // A session handle (or its session id): plain mail to that session, no delivery.
    let to_session = send_as_human(
        &port,
        &default,
        "S2",
        "hello",
        Some(MessageContentType::Html),
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let HumanSendResult::Sent {
        message_id,
        delivery_id: None,
    } = to_session
    else {
        panic!("expected a plain send, got {to_session:?}");
    };
    let stored = inbox::inbox_get(&host.store("default"), message_id)
        .unwrap()
        .unwrap();
    assert_eq!((stored.from.as_str(), stored.to.as_str()), (HUMAN, "H2"));
    assert_eq!(stored.content_type, Some(MessageContentType::Html));

    // Anything else is a role: queued as a pending delivery in the named team.
    let to_role = send_as_human(
        &port,
        &default,
        REVIEWER,
        "review please",
        None,
        Some(MessageOnClick::ShowMessage),
        Some(" docs/plan.md ".into()),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let HumanSendResult::Sent {
        delivery_id: Some(_),
        message_id,
    } = to_role
    else {
        panic!("expected a role delivery, got {to_role:?}");
    };
    let stored = inbox::inbox_get(&host.store("default"), message_id)
        .unwrap()
        .unwrap();
    assert_eq!(stored.from, HUMAN);
    assert_eq!(stored.primary_file.as_deref(), Some("docs/plan.md"));
    let queues =
        crate::weft::role_state::role_state_read(&host.store("default"), Some(REVIEWER), 0, 5)
            .unwrap();
    assert_eq!(queues[0].counts.pending, 1);
    // The watchers of role queues hear of it; a plain send to a session is not queued.
    assert_eq!(
        *host.queued.lock().unwrap(),
        vec![("default".to_string(), REVIEWER.to_string(), message_id)]
    );

    // A summary rides along to a role and shows in the read model.
    let summarized = send_as_human(
        &port,
        &default,
        REVIEWER,
        "<h1>Plan</h1>",
        Some(MessageContentType::Html),
        None,
        None,
        None,
        None,
        Some("Review the plan"),
    )
    .await
    .unwrap();
    let HumanSendResult::Sent {
        delivery_id: Some(_),
        message_id,
    } = summarized
    else {
        panic!("expected a role delivery, got {summarized:?}");
    };
    let stored = inbox::inbox_get(&host.store("default"), message_id)
        .unwrap()
        .unwrap();
    assert_eq!(stored.summary.as_deref(), Some("Review the plan"));
    let view = inbox(
        &*host,
        &default,
        &InboxQuery {
            role: Some(REVIEWER.into()),
            ..Default::default()
        },
        &listed(),
    )
    .await
    .unwrap();
    let row = view
        .messages
        .iter()
        .find(|m| m.message_id == message_id)
        .unwrap();
    assert_eq!(row.summary.as_deref(), Some("Review the plan"));

    // Blank text and no recipient are refused, never stored.
    for (to, text) in [(REVIEWER, "  "), ("H1", ""), ("  ", "hi")] {
        let refused = send_as_human(
            &port, &default, to, text, None, None, None, None, None, None,
        )
        .await
        .unwrap();
        assert!(
            matches!(refused, HumanSendResult::Invalid { .. }),
            "{to:?}/{text:?} → {refused:?}"
        );
    }
}

#[tokio::test]
async fn the_humans_send_carries_a_reply_to_and_the_read_model_shows_it() {
    let host = FakeHost::new();
    let port: Arc<dyn InboxHost> = host.clone();
    let default = team("default");
    // To a session, replies to the role queue; to a role, replies to a running session.
    let mut sent = Vec::new();
    for (to, reply_to) in [("H2", "role:manager"), (REVIEWER, "S1")] {
        let result = send_as_human(
            &port,
            &default,
            to,
            "brief",
            None,
            None,
            None,
            None,
            Some(reply_to),
            None,
        )
        .await
        .unwrap();
        let HumanSendResult::Sent { message_id, .. } = result else {
            panic!("expected sent, got {result:?}");
        };
        sent.push(message_id);
    }
    let view = inbox(
        &*host,
        &default,
        &InboxQuery {
            scope: Some(InboxScope::All),
            ..Default::default()
        },
        &listed(),
    )
    .await
    .unwrap();
    let row = |id: i64| view.messages.iter().find(|m| m.message_id == id).unwrap();
    assert_eq!(row(sent[0]).reply_to.as_deref(), Some("role:manager"));
    assert_eq!(row(sent[1]).reply_to.as_deref(), Some("H1"));
    assert_eq!(row(sent[1]).reply_to_name.as_deref(), Some("Manager"));
    let wire = serde_json::to_value(&view).unwrap();
    assert!(wire["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["replyTo"] == "role:manager"));

    // A replyTo nobody runs is refused, never stored.
    let refused = send_as_human(
        &port,
        &default,
        "H2",
        "x",
        None,
        None,
        None,
        None,
        Some("ghost"),
        None,
    )
    .await
    .unwrap();
    assert!(
        matches!(refused, HumanSendResult::Invalid { .. }),
        "{refused:?}"
    );
}

#[tokio::test]
async fn a_read_acts_on_the_named_team_then_the_sessions_then_the_active_one() {
    let host = FakeHost::new();
    let named = resolve_team(
        &*host,
        Some(InboxTeam {
            org: ORG.into(),
            team: "design".into(),
        }),
        Some("S1"),
    )
    .await
    .unwrap();
    assert_eq!(named, team("design"));
    assert_eq!(
        resolve_team(&*host, None, Some("S3")).await.unwrap(),
        team("design")
    );
    assert_eq!(
        resolve_team(&*host, None, Some("H3")).await.unwrap(),
        team("design")
    );
    assert_eq!(
        resolve_team(&*host, None, Some("unknown")).await.unwrap(),
        team("default")
    );
    assert_eq!(
        resolve_team(&*host, None, None).await.unwrap(),
        team("default")
    );
}

#[test]
fn a_context_repo_call_acts_on_the_callers_org_not_the_one_on_screen() {
    let host = FakeHost::new();
    // The app shows `acme`; the agent in session S9 works in `beta`.
    assert_eq!(
        caller_org(&*host, None, Some("S9"), None, None).unwrap(),
        Some("beta".to_string())
    );
    assert_eq!(
        caller_org(&*host, None, None, Some("S9"), None).unwrap(),
        Some("beta".to_string())
    );
    // A named org wins; a remote tab's own org applies when nothing else names one.
    assert_eq!(
        caller_org(&*host, Some("gamma"), Some("S9"), None, None).unwrap(),
        Some("gamma".to_string())
    );
    assert_eq!(
        caller_org(&*host, None, None, Some("unknown"), Some("beta")).unwrap(),
        Some("beta".to_string())
    );
    // A session named but unknown is refused rather than written to the org on screen.
    assert!(caller_org(&*host, None, Some("nope"), None, None).is_err());
    // Nothing names an org: the active one applies.
    assert_eq!(
        caller_org(&*host, None, Some("  "), None, None).unwrap(),
        None
    );
}

#[test]
fn an_inbox_query_takes_both_spellings() {
    let camel: InboxQuery = serde_json::from_value(serde_json::json!({
        "scope": "to-agents", "agentId": "a", "sessionId": "s", "role": "r", "limit": 3
    }))
    .unwrap();
    let snake: InboxQuery = serde_json::from_value(serde_json::json!({
        "agent_id": "a", "session_id": "s"
    }))
    .unwrap();
    assert!(matches!(camel.scope, Some(InboxScope::ToAgents)));
    assert_eq!(camel.agent_id.as_deref(), Some("a"));
    assert_eq!(camel.limit, Some(3));
    assert_eq!(snake.agent_id.as_deref(), Some("a"));
    assert_eq!(snake.session_id.as_deref(), Some("s"));
}

#[test]
fn instances_are_the_sessions_running_the_agent() {
    let found = instances_of(&listed(), REVIEWER);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].handle, "H1");
    assert!(instances_of(&listed(), "builtin/claude").is_empty());
}

#[tokio::test]
async fn a_hash_names_a_role_and_an_at_names_a_session() {
    let host = FakeHost::new();
    let port: Arc<dyn InboxHost> = host.clone();
    let default = team("default");
    let store = host.store("default");
    let asked = insert(&store, "H1", HUMAN, "which way?", None);

    // `#H2` is the role queue named H2, even though a session answers to that name.
    let to_role = send_as_human(
        &port,
        &default,
        "#reviewer",
        "left",
        None,
        None,
        None,
        Some(asked),
        None,
        None,
    )
    .await
    .unwrap();
    let HumanSendResult::Sent {
        message_id: role_msg,
        delivery_id: Some(_),
    } = to_role
    else {
        panic!("expected a role delivery, got {to_role:?}");
    };
    let stored = inbox::inbox_get(&store, role_msg).unwrap().unwrap();
    assert_eq!(stored.to, "reviewer");
    assert_eq!(stored.in_reply_to, Some(asked));
    assert_eq!(
        stored.in_reply_to_team,
        Some(inbox::InboxReplyTeam {
            org: ORG.into(),
            team: "default".into()
        })
    );
    // Answering marks the question read for the human.
    assert!(inbox::inbox_get(&store, asked)
        .unwrap()
        .unwrap()
        .read_at_ms
        .is_some());

    // Only the reviewer queue holds it: no foreman (or any other) delivery.
    let queues = crate::weft::role_state::role_state_read(&store, None, 0, 5).unwrap();
    assert_eq!(
        queues.iter().map(|q| q.role.as_str()).collect::<Vec<_>>(),
        vec!["reviewer"]
    );

    let to_h2 = send_as_human(
        &port,
        &default,
        "#H2",
        "role, not session",
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(matches!(
        to_h2,
        HumanSendResult::Sent {
            delivery_id: Some(_),
            ..
        }
    ));

    // `@S2` is the session, never a role.
    let to_session = send_as_human(
        &port, &default, "@S2", "hi", None, None, None, None, None, None,
    )
    .await
    .unwrap();
    let HumanSendResult::Sent {
        message_id,
        delivery_id: None,
    } = to_session
    else {
        panic!("expected a plain send, got {to_session:?}");
    };
    assert_eq!(
        inbox::inbox_get(&store, message_id).unwrap().unwrap().to,
        "H2"
    );
    // An `@` naming nobody is refused rather than queued for a role.
    let nobody = send_as_human(
        &port, &default, "@ghost", "hi", None, None, None, None, None, None,
    )
    .await
    .unwrap();
    assert!(matches!(nobody, HumanSendResult::Invalid { .. }));

    // `#role:x` and a bare `role:x` (the Reply-To spelling) name the role `x`.
    for to in ["#role:reviewer", "role:reviewer"] {
        let sent = send_as_human(
            &port, &default, to, "spelled", None, None, None, None, None, None,
        )
        .await
        .unwrap();
        let HumanSendResult::Sent {
            message_id,
            delivery_id: Some(_),
        } = sent
        else {
            panic!("expected a role delivery for {to}, got {sent:?}");
        };
        assert_eq!(
            inbox::inbox_get(&store, message_id).unwrap().unwrap().to,
            "reviewer"
        );
    }
    // `#human` is not a role queue: the human's own send would come back as its mail.
    for spelled in ["#human", "#Human", "role:HUMAN"] {
        let to_human = send_as_human(
            &port, &default, spelled, "me", None, None, None, None, None, None,
        )
        .await
        .unwrap();
        assert!(
            matches!(to_human, HumanSendResult::Invalid { .. }),
            "{spelled}"
        );
    }
}

#[tokio::test]
async fn a_session_inbox_has_what_it_sent_received_and_its_roles_hold() {
    use crate::weft::role_inbox::{claim_role_messages, register_role, send_role_message_commit};
    let host = FakeHost::new();
    let default = host.store("default");
    let design = host.store("design");
    let now = chrono::Utc::now().timestamp();

    let sent = insert(&default, "H1", HUMAN, "status: green", None);
    let received = insert(&default, HUMAN, "H1", "thanks", Some(sent));
    let typed = inbox::inbox_insert_terminal(&default, "H2", "H1", "run the tests").unwrap();
    let unrelated = insert(&default, "H2", HUMAN, "not about H1", None);
    // Mail for H1 that lives in another team's inbox.
    let elsewhere = insert(&design, "H3", "H1", "from design", None);
    // H1 holds the reviewer role in design: its queue is part of H1's conversation,
    // and reading the conversation claims nothing.
    register_role(&design, "reviewer", "S1", now).unwrap();
    let (queued, _) = send_role_message_commit(
        &design,
        HUMAN,
        "reviewer",
        "review #4",
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
    let (other_role, _) = send_role_message_commit(
        &design, HUMAN, "qa", "qa only", None, None, None, "", now, None, None, None,
    )
    .unwrap();

    // Asked by its tab session id, answered for its handle.
    let view = session_inbox(&*host, "S1").await.unwrap();
    assert_eq!(view.handle, "H1");
    assert_eq!(view.roles, vec!["reviewer".to_string()]);
    // Ids are only unique within one team's inbox.
    let got: Vec<(String, i64)> = view
        .messages
        .iter()
        .map(|m| (m.team.team.clone(), m.id))
        .collect();
    let at = |t: &str, id: i64| (t.to_string(), id);
    for key in [
        at("default", sent),
        at("default", received),
        at("default", typed),
        at("design", elsewhere),
        at("design", queued),
    ] {
        assert!(got.contains(&key), "message {key:?} missing from {got:?}");
    }
    assert!(!got.contains(&at("default", unrelated)));
    assert!(!got.contains(&at("design", other_role)));
    assert_eq!(got.len(), 5);
    assert!(
        view.messages
            .windows(2)
            .all(|w| w[0].created_at_ms <= w[1].created_at_ms),
        "oldest first"
    );

    let by = |t: &str, id: i64| {
        view.messages
            .iter()
            .find(|m| m.team.team == t && m.id == id)
            .unwrap()
    };
    let m = by("default", sent);
    assert_eq!((m.from.as_str(), m.from_name.as_str()), ("H1", "Manager"));
    assert_eq!(
        (m.to.as_deref(), m.to_name.as_deref()),
        (Some(HUMAN), Some(HUMAN))
    );
    assert_eq!(m.team.team, "default");
    assert!(m.unread && m.via.is_none() && m.delivery.is_none());
    let m = by("default", typed);
    assert_eq!(m.via.as_deref(), Some("terminal"));
    assert!(!m.unread, "typed text is already in front of its recipient");
    assert_eq!(by("design", elsewhere).from, "H3");
    let m = by("design", queued);
    assert_eq!(
        (m.to.as_deref(), m.to_role.as_deref()),
        (None, Some("reviewer"))
    );
    assert!(m.unread);
    assert_eq!(
        m.delivery,
        Some(ConversationDelivery {
            state: ConversationDeliveryState::Pending,
            claimed_by: None
        })
    );
    let reg = register_role(&design, "reviewer", "S1", now + 1).unwrap();
    assert_eq!(reg.pending_count, 1, "listing claimed nothing");

    // Once H1 claims it (as its tab session S1), the queue row shows who holds it, by
    // handle, and is no longer unread.
    claim_role_messages(&design, "reviewer", "S1", reg.generation, 5, now + 1).unwrap();
    let view = session_inbox(&*host, "H1").await.unwrap();
    let m = view
        .messages
        .iter()
        .find(|m| m.team.team == "design" && m.id == queued)
        .unwrap();
    assert!(!m.unread);
    assert_eq!(
        m.delivery,
        Some(ConversationDelivery {
            state: ConversationDeliveryState::Claimed,
            claimed_by: Some("H1".into())
        })
    );

    // The wire shape the inbox rail reads.
    let json = serde_json::to_value(&view).unwrap();
    let row = json["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == queued && m["team"]["team"] == "design")
        .unwrap()
        .clone();
    for key in [
        "id",
        "team",
        "from",
        "fromName",
        "to",
        "toName",
        "toRole",
        "text",
        "contentType",
        "createdAtMs",
        "inReplyTo",
        "via",
        "unread",
        "delivery",
    ] {
        assert!(row.get(key).is_some(), "missing {key}");
    }
    assert_eq!(
        row["team"],
        serde_json::json!({"org": ORG, "team": "design"})
    );
    assert_eq!(
        row["delivery"],
        serde_json::json!({"state": "claimed", "claimedBy": "H1"})
    );
    assert!(row["to"].is_null() && row["via"].is_null());
    assert!(
        !host.path("missing").exists(),
        "a team never written is not created"
    );
}

#[tokio::test]
async fn a_session_inbox_carries_each_messages_reply_to_and_summary() {
    let host = FakeHost::new();
    let asked = inbox::inbox_insert_with_reply_to(
        &host.store("default"),
        "H1",
        HUMAN,
        "<h1>Which branch?</h1>",
        None,
        None,
        None,
        None,
        Some(MessageContentType::Html),
        Some("role:manager"),
        Some("Which branch?"),
        None,
    )
    .unwrap();
    let view = session_inbox(&*host, "H1").await.unwrap();
    let m = view.messages.iter().find(|m| m.id == asked).unwrap();
    assert_eq!(m.reply_to.as_deref(), Some("role:manager"));
    assert_eq!(m.summary.as_deref(), Some("Which branch?"));
    let wire = serde_json::to_value(m).unwrap();
    assert_eq!(wire["replyTo"], "role:manager");
    assert_eq!(wire["summary"], "Which branch?");
    // A plain message carries neither, as nulls on the wire.
    let plain = insert(&host.store("default"), "H1", HUMAN, "hi", None);
    let view = session_inbox(&*host, "H1").await.unwrap();
    let wire = serde_json::to_value(view.messages.iter().find(|m| m.id == plain).unwrap()).unwrap();
    assert!(wire["replyTo"].is_null() && wire["summary"].is_null());
}

#[tokio::test]
async fn a_hand_started_tab_has_an_empty_inbox_not_an_error() {
    let host = FakeHost::new();
    insert(&host.store("default"), "H1", "H2", "hi", None);
    let view = session_inbox(&*host, "tab-by-hand").await.unwrap();
    assert_eq!(view.handle, "tab-by-hand");
    assert!(view.roles.is_empty() && view.messages.is_empty());
}

#[tokio::test]
async fn the_rollup_covers_every_team_and_each_role_queue() {
    use crate::weft::role_inbox::{register_role, send_role_message_commit};
    let host = FakeHost::new();
    let default = host.store("default");
    let design = host.store("design");
    let now = chrono::Utc::now().timestamp();
    let a = insert(&default, "H1", HUMAN, "one", None);
    let click = MessageOnClick::GotoAgent {
        agent_id: "reviewer".into(),
    };
    let b = inbox::inbox_insert(
        &design,
        "H3",
        HUMAN,
        "two",
        None,
        None,
        Some("docs/review.md"),
        Some(&click),
        None,
    )
    .unwrap();
    register_role(&design, "reviewer", "S1", now).unwrap();
    for text in ["r1", "r2"] {
        send_role_message_commit(
            &design, HUMAN, "reviewer", text, None, None, None, "", now, None, None, None,
        )
        .unwrap();
    }
    send_role_message_commit(
        &default,
        "H1",
        "architect",
        "plan?",
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

    let rollup = inbox_rollup(&*host).await.unwrap();
    let keys: Vec<(&str, i64)> = rollup
        .messages
        .iter()
        .map(|m| (m.team.team.as_str(), m.id))
        .collect();
    assert!(keys.contains(&("default", a)) && keys.contains(&("design", b)));
    assert_eq!(rollup.messages.len(), 5);
    let attached = rollup
        .messages
        .iter()
        .find(|m| m.team.team == "design" && m.id == b)
        .unwrap();
    assert_eq!(attached.primary_file.as_deref(), Some("docs/review.md"));
    assert_eq!(attached.on_click.as_ref(), Some(&click));
    let wire = serde_json::to_value(attached).unwrap();
    assert_eq!(wire["primaryFile"], "docs/review.md");
    assert_eq!(
        wire["onClick"],
        serde_json::json!({ "type": "goto-agent", "agent_id": "reviewer" })
    );
    let plain = rollup
        .messages
        .iter()
        .find(|m| m.team.team == "default" && m.id == a)
        .unwrap();
    let wire = serde_json::to_value(plain).unwrap();
    assert!(wire.get("primaryFile").is_none() && wire.get("onClick").is_none());
    assert_eq!(
        rollup.roles,
        vec![
            RollupRole {
                role: "architect".into(),
                team: Some(team("default")),
                holder_count: 0,
                holder: None,
                pending: 1,
                note: "no live holder · 1 queued".into(),
            },
            RollupRole {
                role: "reviewer".into(),
                team: Some(team("design")),
                holder_count: 1,
                holder: Some("H1".into()),
                pending: 2,
                note: "held by Manager · 2 queued until it claims".into(),
            },
        ]
    );
}

#[tokio::test]
async fn unreadable_synced_teams_fall_back_to_the_active_team() {
    let host = FakeHost::new();
    let default = host.store("default");
    let design = host.store("design");
    let here = insert(&default, HUMAN, "H1", "in the active team", None);
    insert(&design, HUMAN, "H1", "in another team", None);
    host.synced_fails
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let view = session_inbox(&*host, "H1").await.unwrap();
    let got: Vec<(String, i64)> = view
        .messages
        .iter()
        .map(|m| (m.team.team.clone(), m.id))
        .collect();
    assert_eq!(got, vec![("default".to_string(), here)]);
}

#[tokio::test]
async fn inbox_rollup_exposes_archive_writes_without_losing_conversation_history() {
    let host = FakeHost::new();
    let store = host.store("default");
    let id = insert(&store, "H1", HUMAN, "report", None);
    let team = LaunchTeam {
        org: ORG.into(),
        team: "default".into(),
    };
    assert_eq!(
        crate::commands::agent_inbox::mark_archived_many_by_human(&*host, &team, vec![id])
            .await
            .unwrap(),
        1
    );
    let dto = inbox_rollup(&*host).await.unwrap();
    let message = dto
        .messages
        .iter()
        .find(|m| m.id == id && m.team.team == "default")
        .unwrap();
    assert!(message.archived_at_ms.is_some());
    assert!(!message.unread);
    let json = serde_json::to_value(message).unwrap();
    assert!(json["archivedAtMs"].is_i64());
    assert_eq!(*host.changed.lock().unwrap(), 1);
}

#[tokio::test]
async fn with_no_team_resolvable_the_inbox_read_fails_rather_than_reads_empty() {
    let host = FakeHost::new();
    let design = host.store("design");
    let mine = insert(&design, HUMAN, "H3", "to the design session", None);
    host.synced_fails
        .store(true, std::sync::atomic::Ordering::SeqCst);
    host.active_fails
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let err = inbox_rollup(&*host).await.unwrap_err();
    assert!(err.contains("contexts unreadable"), "{err}");
    assert!(err.contains("no active team"), "{err}");
    // A session still reads its own launch team's inbox.
    let view = session_inbox(&*host, "H3").await.unwrap();
    let got: Vec<i64> = view.messages.iter().map(|m| m.id).collect();
    assert_eq!(got, vec![mine]);
    assert!(session_inbox(&*host, "H1").await.is_ok());
}

#[tokio::test]
async fn two_teams_resolving_to_one_store_list_its_rows_once() {
    let host = FakeHost::new();
    let mine = insert(&host.store("default"), HUMAN, "H1", "once", None);
    *host.synced_only.lock().unwrap() = Some(vec!["default", "alias"]);

    let view = session_inbox(&*host, "H1").await.unwrap();
    let got: Vec<i64> = view.messages.iter().map(|m| m.id).collect();
    assert_eq!(got, vec![mine]);
    let rollup = inbox_rollup(&*host).await.unwrap();
    let got: Vec<i64> = rollup.messages.iter().map(|m| m.id).collect();
    assert_eq!(got, vec![mine]);
}

#[tokio::test]
async fn a_session_reads_its_own_team_even_when_the_synced_list_omits_it() {
    let host = FakeHost::new();
    let port: Arc<dyn InboxHost> = host.clone();
    *host.synced_only.lock().unwrap() = Some(vec!["default"]);
    let SendResult::Sent { message_id, .. } = super::super::agent_inbox::send_message(
        &port,
        Some("S1"),
        "S3",
        "launch team",
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap() else {
        panic!("send");
    };
    let view = session_inbox(&*host, "S3").await.unwrap();
    assert_eq!(view.handle, "H3");
    assert_eq!(view.messages.len(), 1);
    assert_eq!(view.messages[0].id, message_id);
    assert_eq!(view.messages[0].team.team, "design");
    assert!(view.messages[0].unread);
    let tool_view = inbox(
        &*host,
        &team("design"),
        &InboxQuery {
            session_id: Some("S3".into()),
            ..Default::default()
        },
        &[],
    )
    .await
    .unwrap();
    assert_eq!(tool_view.messages.len(), view.messages.len());
    let rollup = inbox_rollup(&*host).await.unwrap();
    assert!(
        rollup.messages.is_empty(),
        "rollups keep the synced-team rule"
    );
}

#[tokio::test]
async fn when_every_team_store_fails_the_read_fails_rather_than_reads_empty() {
    let host = FakeHost::new();
    *host.synced_only.lock().unwrap() = Some(vec!["broken"]);
    let err = session_inbox(&*host, "H1").await.unwrap_err();
    assert!(err.contains("open inbox store"), "{err}");
    let err = inbox_rollup(&*host).await.unwrap_err();
    assert!(err.contains("open inbox store"), "{err}");

    // One failing team among readable ones is skipped.
    let mine = insert(&host.store("default"), HUMAN, "H1", "readable", None);
    *host.synced_only.lock().unwrap() = Some(vec!["broken", "default"]);
    let view = session_inbox(&*host, "H1").await.unwrap();
    let got: Vec<i64> = view.messages.iter().map(|m| m.id).collect();
    assert_eq!(got, vec![mine]);
    let rollup = inbox_rollup(&*host).await.unwrap();
    let got: Vec<i64> = rollup.messages.iter().map(|m| m.id).collect();
    assert_eq!(got, vec![mine]);
}

#[tokio::test]
async fn human_is_not_a_session_inbox() {
    let host = FakeHost::new();
    insert(&host.store("default"), "H1", HUMAN, "to the human", None);
    for handle in ["human", "Human", " HUMAN "] {
        let err = session_inbox(&*host, handle).await.unwrap_err();
        assert!(err.contains("human is not a session"), "{handle}: {err}");
    }
}

#[tokio::test]
async fn a_human_session_or_agent_filter_names_nothing_and_matches_nothing() {
    let host = FakeHost::new();
    let default = team("default");
    insert(&host.store("default"), "H1", HUMAN, "to the human", None);
    insert(&host.store("default"), HUMAN, "H1", "from the human", None);
    for who in ["human", "Human", " HUMAN "] {
        for query in [
            InboxQuery {
                session_id: Some(who.into()),
                ..Default::default()
            },
            InboxQuery {
                agent_id: Some(who.into()),
                ..Default::default()
            },
        ] {
            let view = inbox(&*host, &default, &query, &listed()).await.unwrap();
            assert!(view.messages.is_empty(), "{who}: {:?}", view.messages);
        }
    }
    // Beside a real filter, `human` is skipped and the real one still matches.
    let view = inbox(
        &*host,
        &default,
        &InboxQuery {
            session_id: Some("human".into()),
            role: Some("nobody-yet".into()),
            agent_id: Some(REVIEWER.into()),
            ..Default::default()
        },
        &listed(),
    )
    .await
    .unwrap();
    assert_eq!(view.messages.len(), 2, "{:?}", view.messages);
}

#[tokio::test]
async fn an_at_human_in_any_case_is_the_human() {
    let host = FakeHost::new();
    let port: Arc<dyn InboxHost> = host.clone();
    let default = team("default");
    for to in ["@human", "@Human", "@HUMAN", " @ Human "] {
        let sent = send_as_human(
            &port,
            &default,
            to,
            "note to self",
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        let HumanSendResult::Sent {
            message_id,
            delivery_id: None,
        } = sent
        else {
            panic!("{to}: expected plain mail to the human, got {sent:?}");
        };
        let stored = inbox::inbox_get(&host.store("default"), message_id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.to, HUMAN, "{to}");
    }
}

#[tokio::test]
async fn a_human_role_filter_names_nothing_and_matches_nothing() {
    let host = FakeHost::new();
    let default = team("default");
    insert(&host.store("default"), "H1", HUMAN, "to the human", None);
    insert(&host.store("default"), HUMAN, "H1", "from the human", None);
    for role in ["human", "Human", "role:human", " role:HUMAN "] {
        let view = inbox(
            &*host,
            &default,
            &InboxQuery {
                role: Some(role.into()),
                ..Default::default()
            },
            &listed(),
        )
        .await
        .unwrap();
        assert!(view.messages.is_empty(), "{role}: {:?}", view.messages);
    }
}

#[tokio::test]
async fn a_bare_human_in_any_case_is_the_human_not_a_role() {
    let host = FakeHost::new();
    let port: Arc<dyn InboxHost> = host.clone();
    let default = team("default");
    for to in ["human", "Human", " HUMAN "] {
        let sent = send_as_human(
            &port,
            &default,
            to,
            "note to self",
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        let HumanSendResult::Sent {
            message_id,
            delivery_id: None,
        } = sent
        else {
            panic!("{to}: expected plain mail to the human, got {sent:?}");
        };
        let stored = inbox::inbox_get(&host.store("default"), message_id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.to, HUMAN, "{to}");
    }
    assert!(host.queued.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_humans_reply_to_a_session_records_what_it_answers() {
    let host = FakeHost::new();
    let port: Arc<dyn InboxHost> = host.clone();
    let default = team("default");
    let asked = insert(&host.store("default"), "H2", HUMAN, "which branch?", None);
    let sent = send_as_human(
        &port,
        &default,
        "S2",
        "main",
        None,
        None,
        None,
        Some(asked),
        None,
        None,
    )
    .await
    .unwrap();
    let HumanSendResult::Sent { message_id, .. } = sent else {
        panic!("expected a send, got {sent:?}");
    };
    let stored = inbox::inbox_get(&host.store("default"), message_id)
        .unwrap()
        .unwrap();
    assert_eq!((stored.from.as_str(), stored.to.as_str()), (HUMAN, "H2"));
    assert_eq!(stored.in_reply_to, Some(asked));
    let reply_team = stored.in_reply_to_team.expect("in_reply_to_team");
    assert_eq!(
        (reply_team.org.as_str(), reply_team.team.as_str()),
        (ORG, "default")
    );
}

#[tokio::test]
async fn an_instance_registered_under_a_session_shows_as_that_session() {
    use crate::weft::role_inbox::{
        claim_role_messages, register_role_as, send_role_message_commit, RegisterOptions,
    };
    let host = FakeHost::new();
    let design = host.store("design");
    let now = chrono::Utc::now().timestamp();
    // A custom instance id, kept alive by session S1 (handle H1).
    let options = RegisterOptions {
        session_id: Some("S1".into()),
        ..RegisterOptions::default()
    };
    let reg = register_role_as(&design, "reviewer", "worker-7", &options, now).unwrap();
    let (queued, _) = send_role_message_commit(
        &design, HUMAN, "reviewer", "review", None, None, None, "", now, None, None, None,
    )
    .unwrap();
    claim_role_messages(&design, "reviewer", "worker-7", reg.generation, 5, now).unwrap();

    let rollup = inbox_rollup(&*host).await.unwrap();
    let role = rollup.roles.iter().find(|r| r.role == "reviewer").unwrap();
    assert_eq!(role.holder.as_deref(), Some("H1"));
    let m = rollup
        .messages
        .iter()
        .find(|m| m.team.team == "design" && m.id == queued)
        .unwrap();
    assert_eq!(
        m.delivery,
        Some(ConversationDelivery {
            state: ConversationDeliveryState::Claimed,
            claimed_by: Some("H1".into())
        })
    );
}

/// SW-AGR-18: a message to the human with the given kind and Reply-To.
fn insert_kind(
    store: &WeftStore,
    text: &str,
    kind: Option<inbox::MessageKind>,
    reply_to: Option<&str>,
) -> i64 {
    inbox::inbox_insert_with_reply_to(
        store, "H1", HUMAN, text, None, None, None, None, None, reply_to, None, kind,
    )
    .unwrap()
}

#[tokio::test]
async fn inbox_rows_carry_the_effective_kind_and_filter_by_it_before_the_limit() {
    use inbox::MessageKind::{NeedsYou, Update};
    let host = FakeHost::new();
    let store = host.store("default");
    let asked = insert_kind(&store, "decide?", Some(NeedsYou), None);
    let routed = insert_kind(&store, "which branch?", None, Some("role:manager"));
    let report = insert_kind(&store, "merged", None, None);
    let told = insert_kind(&store, "done", Some(Update), None);
    let list = |kind| {
        let host = host.clone();
        async move {
            inbox(
                &*host,
                &team("default"),
                &InboxQuery {
                    kind,
                    limit: Some(2),
                    ..Default::default()
                },
                &[],
            )
            .await
            .unwrap()
        }
    };
    let all = list(None).await;
    assert_eq!(ids(&all), vec![told, report]);
    assert_eq!(
        all.messages.iter().map(|m| m.kind).collect::<Vec<_>>(),
        vec![Update, Update]
    );
    let wire = serde_json::to_value(&all.messages[0]).unwrap();
    assert_eq!(wire["kind"], "update");
    // The filter runs before the limit: both needs-you rows, though they are older.
    assert_eq!(ids(&list(Some(NeedsYou)).await), vec![routed, asked]);
    assert_eq!(ids(&list(Some(Update)).await), vec![told, report]);
    // humanUnread keeps counting every unread message to the human.
    assert_eq!(all.human_unread, 4);
}

#[tokio::test]
async fn the_rollup_carries_both_mail_kinds_without_agent_progress_notes() {
    use inbox::MessageKind::{NeedsYou, Update};
    let host = FakeHost::new();
    let default = host.store("default");
    let design = host.store("design");
    let asked = insert_kind(&default, "decide?", Some(NeedsYou), None);
    let report = insert_kind(&design, "merged", None, None);
    crate::weft::agent_updates::agent_updates_insert(&default, "S1", "Manager", "agent", "on it")
        .unwrap();
    crate::weft::agent_updates::agent_updates_insert(&design, "S7", "", "", "tests green").unwrap();

    let rollup = inbox_rollup(&*host).await.unwrap();
    // Ids are unique only within one team's inbox.
    let kind_of = |team: &str, id| {
        rollup
            .messages
            .iter()
            .find(|m| m.team.team == team && m.id == id)
            .unwrap()
            .kind
    };
    assert_eq!(
        (kind_of("default", asked), kind_of("design", report)),
        (NeedsYou, Update)
    );
    let wire = serde_json::to_value(&rollup).unwrap();
    assert_eq!(
        wire["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["kind"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["needs-you", "update"]
    );
    assert!(wire.get("updates").is_none());
    // Notes remain accessible to dashboards through the independent updates read model.
    let default_notes = updates(&*host, &team("default"), None).await.unwrap();
    assert_eq!(default_notes.updates.len(), 1);
    let design_notes = updates(&*host, &team("design"), None).await.unwrap();
    assert_eq!(design_notes.updates.len(), 1);
}

/// SW-AGR-18: a manager's digest (`replyTo: "role:manager"` from the manager role's
/// holder, known here by its tab session) is an update; the same Reply-To from a
/// worker is a question for the human.
#[tokio::test]
async fn a_reply_to_the_senders_own_role_reads_as_an_update() {
    use crate::weft::role_inbox::register_role;
    use inbox::MessageKind::{NeedsYou, Update};
    let host = FakeHost::new();
    let store = host.store("default");
    let now = chrono::Utc::now().timestamp();
    // H1 runs under tab session S1, which holds `manager`.
    register_role(&store, "manager", "S1", now).unwrap();
    let digest = inbox::inbox_insert_with_reply_to(
        &store,
        "H1",
        HUMAN,
        "3 merged",
        None,
        None,
        None,
        None,
        None,
        Some("role:manager"),
        None,
        None,
    )
    .unwrap();
    let question = inbox::inbox_insert_with_reply_to(
        &store,
        "H2",
        HUMAN,
        "which branch?",
        None,
        None,
        None,
        None,
        None,
        Some("role:manager"),
        None,
        None,
    )
    .unwrap();
    let rollup = inbox_rollup(&*host).await.unwrap();
    let kind = |id| {
        rollup
            .messages
            .iter()
            .find(|m| m.team.team == "default" && m.id == id)
            .unwrap()
            .kind
    };
    assert_eq!((kind(digest), kind(question)), (Update, NeedsYou));
    let view = inbox(&*host, &team("default"), &InboxQuery::default(), &[])
        .await
        .unwrap();
    let row = |id| {
        view.messages
            .iter()
            .find(|m| m.message_id == id)
            .unwrap()
            .kind
    };
    assert_eq!((row(digest), row(question)), (Update, NeedsYou));
    let session = session_inbox(&*host, "H1").await.unwrap();
    assert_eq!(
        session
            .messages
            .iter()
            .find(|m| m.id == digest)
            .unwrap()
            .kind,
        Update
    );
}

#[tokio::test]
async fn a_human_reply_requires_a_message_in_its_team() {
    let host = FakeHost::new();
    let port: Arc<dyn InboxHost> = host.clone();
    let result = send_as_human(
        &port,
        &team("default"),
        "S2",
        "reply",
        None,
        None,
        None,
        Some(99999),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(
        matches!(result, HumanSendResult::Invalid { message } if message == "Not sent: no such message to reply to.")
    );
    assert!(inbox::inbox_get(&host.store("default"), 1)
        .unwrap()
        .is_none());
    assert!(host.queued.lock().unwrap().is_empty());
    let asked = insert(&host.store("default"), "H1", "H2", "agent mail", None);
    let sent = send_as_human(
        &port,
        &team("default"),
        "S2",
        "reply",
        None,
        None,
        None,
        Some(asked),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(matches!(sent, HumanSendResult::Sent { .. }));
    assert!(inbox::inbox_get(&host.store("default"), asked)
        .unwrap()
        .unwrap()
        .read_at_ms
        .is_none());
}

#[tokio::test]
async fn a_remote_session_reads_the_same_sent_and_replied_history_when_its_machine_goes_offline() {
    let host = FakeHost::new();
    let record = serde_json::from_value(serde_json::json!({
        "handle": "work#remote", "owner_id": 41, "request_id": uuid::Uuid::new_v4(),
        "machine_id": "test-box", "team_key": "acme:default", "workspace_id": "work",
        "workspace": "acme:default|work", "cwd": "/home/dev", "name": "Remote worker", "launch": "codex", "hooks": true,
        "target": {"member": {"machineId": "test-box", "slot": "remote"}, "admissionRevision": "1",
            "namespace": {"serverId": "s", "epoch": "e", "generation": "1"}, "session": "remote-S4"}
    })).unwrap();
    host.remote.reserve_remote_shell(record).unwrap();
    let port: Arc<dyn InboxHost> = host.clone();
    let SendResult::Sent { message_id, .. } = super::super::agent_inbox::send_message(
        &port,
        Some("S1"),
        "remote-S4",
        "remote question",
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap() else {
        panic!("send");
    };
    let received = super::super::agent_inbox::read_messages(&*port, Some("remote-S4"))
        .await
        .unwrap();
    assert_eq!(received[0].message_id, message_id);
    let SendResult::Sent {
        message_id: reply_id,
        ..
    } = super::super::agent_inbox::reply(
        &port,
        Some("remote-S4"),
        message_id,
        "remote answer",
        None,
    )
    .await
    .unwrap()
    else {
        panic!("reply");
    };
    for entry in ["remote-S4", "work#remote", "S1"] {
        let view = session_inbox(&*host, entry).await.unwrap();
        let got: Vec<_> = view
            .messages
            .iter()
            .map(|m| (m.id, m.text.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![(message_id, "remote question"), (reply_id, "remote answer")]
        );
        assert_eq!(view.messages[0].to.as_deref(), Some("work#remote"));
        assert!(!view.messages[0].unread);
        assert_eq!(view.messages[1].from, "work#remote");
        assert_eq!(view.messages[1].in_reply_to, Some(message_id));
    }
    host.remote_online
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let offline = session_inbox(&*host, "remote-S4").await.unwrap();
    assert_eq!(offline.handle, "work#remote");
    assert_eq!(offline.messages.len(), 2);
}

#[tokio::test]
async fn the_rollup_counts_shared_live_registrations_and_retains_queue_identity() {
    use crate::weft::role_inbox::{register_role_as, RegisterOptions};
    let host = FakeHost::new();
    let now = chrono::Utc::now().timestamp();
    for id in ["S1", "S2"] {
        register_role_as(
            &host.store("design"),
            "reviewer",
            id,
            &RegisterOptions {
                session_id: Some(id.into()),
                exclusive: false,
                ..Default::default()
            },
            now,
        )
        .unwrap();
    }
    let role = inbox_rollup(&*host)
        .await
        .unwrap()
        .roles
        .into_iter()
        .find(|r| r.role == "reviewer")
        .unwrap();
    assert_eq!(role.team, Some(team("design")));
    assert_eq!(role.holder_count, 2);
    let wire = serde_json::to_value(role).unwrap();
    assert_eq!(wire["holderCount"], 2);
    assert_eq!(wire["team"]["team"], "design");
    register_role_as(
        &host.store("default"),
        "reviewer",
        "S3",
        &RegisterOptions {
            exclusive: false,
            ..Default::default()
        },
        now,
    )
    .unwrap();
    let combined = inbox_rollup(&*host)
        .await
        .unwrap()
        .roles
        .into_iter()
        .find(|r| r.role == "reviewer")
        .unwrap();
    assert_eq!(combined.holder_count, 3);
    assert!(combined.team.is_none());
}

#[tokio::test]
async fn the_rollup_includes_a_configured_empty_queue_before_its_store_or_first_message_exists() {
    let host = FakeHost::new();
    insert(
        &host.store("default"),
        "H1",
        HUMAN,
        "an existing conversation",
        None,
    );
    crate::queue_roles::save(
        &host.dir.path().join("design"),
        crate::queue_roles::RoleConfig {
            id: "fresh".into(),
            name: "Fresh verification".into(),
            icon: Some("builtin-chess:rook".into()),
            default_handler: None,
        },
    )
    .unwrap();
    assert!(!host.path("design").exists());
    let rollup = inbox_rollup(&*host).await.unwrap();
    let queue = rollup.roles.iter().find(|r| r.role == "fresh").unwrap();
    assert_eq!(queue.team, Some(team("design")));
    assert_eq!(queue.holder_count, 0);
    assert_eq!(queue.pending, 0);
    assert!(queue.holder.is_none());
    assert!(
        !host.path("design").exists(),
        "reading configured targets leaves an unwritten inbox unopened"
    );
    assert_eq!(rollup.messages.len(), 1);
}

#[tokio::test]
async fn baseline_catalog_counts_self_mail_while_native_chrome_excludes_it() {
    let host = FakeHost::new();
    insert(&host.store("default"), HUMAN, HUMAN, "note to self", None);
    insert(&host.store("default"), "H1", HUMAN, "update", None);
    let view = inbox(&*host, &team("default"), &InboxQuery::default(), &listed())
        .await
        .unwrap();
    // Preserve the baseline disagreement: the ledger includes self-mail; chrome excludes it.
    assert_eq!(
        inbox::inbox_unread(&host.store("default"), HUMAN)
            .unwrap()
            .count,
        2
    );
    assert_eq!(
        view.messages
            .iter()
            .filter(|m| m.unread && m.from != HUMAN)
            .count(),
        1
    );
}

#[test]
fn baseline_session_unread_includes_deliverable_self_mail() {
    let host = FakeHost::new();
    insert(&host.store("default"), "H1", "H1", "note to myself", None);
    assert_eq!(
        inbox::inbox_unread(&host.store("default"), "H1")
            .unwrap()
            .count,
        1
    );
    assert_eq!(
        inbox::inbox_take_unread(&host.store("default"), "H1")
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn inbox_writes_emit_team_and_message_identity() {
    let host = FakeHost::new();
    let port: Arc<dyn InboxHost> = host.clone();
    let HumanSendResult::Sent { message_id, .. } = send_as_human(
        &port,
        &team("default"),
        "human",
        "self note",
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap() else {
        panic!("send")
    };
    super::super::agent_inbox::mark_read_by_human(&*host, &team("default"), message_id)
        .await
        .unwrap();
    let events = host.events.lock().unwrap();
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[0].kind,
        crate::live_hub::inbox_events::InboxChange::Stored
    ));
    assert!(matches!(
        events[1].kind,
        crate::live_hub::inbox_events::InboxChange::Read
    ));
    for event in events.iter() {
        assert_eq!(event.team, team("default"));
        assert_eq!(event.message_ids, vec![message_id]);
    }
}

#[tokio::test]
async fn typed_write_read_and_archive_reach_the_shared_inbox_projection() {
    use crate::live_hub::{
        inbox::{read_batch, InboxProjection},
        projection::TopicRegistry,
    };
    let host = FakeHost::new();
    let port: Arc<dyn InboxHost> = host.clone();
    let team = team("default");
    let mut projection = InboxProjection::default();
    let mut registry = TopicRegistry::default();
    let human = super::super::agent_inbox::note_to_human(&port, None, "review ready", "ready")
        .await
        .unwrap();
    let HumanSendResult::Sent { .. } = send_as_human(
        &port,
        &team,
        "H1",
        "please review",
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap() else {
        panic!("send")
    };
    let mut listed = vec![row("H1", "Manager", None)];
    listed[0].team = Some(team.clone());
    listed[0].session = Some("S1".into());
    let sessions = KnownSessions::new(listed.iter().map(KnownSession::from).collect());
    for step in 0..3 {
        if step == 1 {
            super::super::agent_inbox::mark_read_by_human(&*host, &team, human)
                .await
                .unwrap();
            super::super::agent_inbox::read_messages(&*host, Some("S1"))
                .await
                .unwrap();
        }
        if step == 2 {
            super::super::agent_inbox::mark_archived_many_by_human(&*host, &team, vec![human])
                .await
                .unwrap();
        }
        let events = std::mem::take(&mut *host.events.lock().unwrap());
        assert!(!events.is_empty());
        assert!(events.iter().all(|event| event.team == team));
        let ids = events
            .into_iter()
            .flat_map(|event| event.message_ids)
            .collect::<Vec<_>>();
        let batch = read_batch(host.path("default"), step == 0, ids.clone())
            .await
            .unwrap();
        let (mut changed, removed) = projection.apply_batch(&*host, &sessions, &team, batch, &ids);
        projection.fill_session_fields(&listed);
        changed.extend(projection.count_rows());
        let sent = registry.patch("inbox", changed, removed, None).unwrap();
        if step == 0 {
            registry
                .subscribe_window("native-and-html", "inbox", "all", None, None)
                .unwrap();
        } else {
            assert_eq!(sent.len(), 1);
        }
        let summary = projection.counts(&team);
        assert_eq!(summary["humanUnread"], if step == 0 { 1 } else { 0 });
        assert_eq!(
            summary["sessions"]["H1"]["unread"],
            if step == 0 { 1 } else { 0 }
        );
        let mut native = listed.clone();
        projection.join(&mut native);
        assert_eq!(native[0].unread, Some(if step == 0 { 1 } else { 0 }));
        let bridge = &projection.bridge_rows["team:acme/default"][&human];
        assert_eq!(bridge.unread, step == 0);
        assert_eq!(bridge.archived_at_ms.is_some(), step == 2);
    }
    // Session changes update cached names and ended reply affordances without SQL reads.
    listed[0].display_name = "Renamed".into();
    listed[0].state = SessionState::Exited;
    assert!(!projection.refresh_sessions(&*host, &listed).is_empty());
}

#[tokio::test]
async fn role_registration_and_zero_claim_release_reproject_inferred_kind() {
    use crate::live_hub::inbox::{read_batch, InboxProjection};
    let host = FakeHost::new();
    let port: Arc<dyn InboxHost> = host.clone();
    let team = team("default");
    let store = host.store("default");
    let super::super::agent_inbox::SendResult::Sent { message_id: id, .. } =
        super::super::agent_inbox::send_message(
            &port,
            Some("S1"),
            HUMAN,
            "please decide",
            None,
            None,
            None,
            Some("role:reviewer"),
            None,
        )
        .await
        .unwrap()
    else {
        panic!("send")
    };
    let sessions = KnownSessions::new(vec![KnownSession {
        handle: "H1".into(),
        session: Some("S1".into()),
        name: "Manager".into(),
        running: true,
    }]);
    let mut projection = InboxProjection::default();
    let batch = read_batch(host.path("default"), true, vec![])
        .await
        .unwrap();
    projection.apply_batch(&*host, &sessions, &team, batch, &[]);
    assert_eq!(projection.counts(&team)["needsYouBySender"]["H1"], 1);
    let registration = crate::weft::role_inbox::register_role(
        &store,
        "reviewer",
        "S1",
        chrono::Utc::now().timestamp(),
    )
    .unwrap();
    super::super::agent_inbox::role_delivery_changed(&*host, &team, None, Some("reviewer"))
        .await
        .unwrap();
    let ids = host
        .events
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .message_ids
        .clone();
    let batch = read_batch(host.path("default"), false, ids.clone())
        .await
        .unwrap();
    projection.apply_batch(&*host, &sessions, &team, batch, &ids);
    assert_eq!(projection.counts(&team)["needsYouBySender"]["H1"], 0);
    assert_eq!(
        projection.bridge_rows["team:acme/default"][&id].kind,
        inbox::MessageKind::Update
    );
    host.events.lock().unwrap().clear();
    let released = super::super::role_inbox::release_registration(
        &*host,
        &team,
        "reviewer".into(),
        "S1".into(),
        registration.generation,
    )
    .await
    .unwrap();
    assert_eq!(released, 0, "the registration held no delivery claims");
    let events = std::mem::take(&mut *host.events.lock().unwrap());
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].team, team);
    let ids = events
        .into_iter()
        .flat_map(|event| event.message_ids)
        .collect::<Vec<_>>();
    let batch = read_batch(host.path("default"), false, ids.clone())
        .await
        .unwrap();
    projection.apply_batch(&*host, &sessions, &team, batch, &ids);
    assert_eq!(projection.counts(&team)["needsYouBySender"]["H1"], 1);
    assert_eq!(
        projection.bridge_rows["team:acme/default"][&id].kind,
        inbox::MessageKind::NeedsYou
    );
}

#[tokio::test]
async fn full_history_summary_and_pages_cover_older_unread_mail() {
    let host = FakeHost::new();
    let store = host.store("default");
    for n in 0..600 {
        let sender = if n == 49 { "old-worker" } else { "H1" };
        let id = inbox::inbox_insert(
            &store, sender, HUMAN, "history", None, None, None, None, None,
        )
        .unwrap();
        if n != 49 {
            inbox::inbox_mark_read(&store, id, HUMAN).unwrap();
        }
    }
    let mut projection = crate::live_hub::inbox::InboxProjection::default();
    let batch = crate::live_hub::inbox::read_batch(host.path("default"), true, vec![])
        .await
        .unwrap();
    projection.apply_batch(
        &*host,
        &host.sessions().await.unwrap(),
        &team("default"),
        batch,
        &[],
    );
    *host.projected.lock().unwrap() = Some(projection.view_parts(&team("default")).build());
    let view = super::inbox(&*host, &team("default"), &InboxQuery::default(), &[])
        .await
        .unwrap();
    assert_eq!(view.messages.len(), 500);
    assert_eq!(
        view.conversations
            .iter()
            .find(|c| c.with == "old-worker")
            .unwrap()
            .unread,
        1
    );
    let rollup = inbox_rollup(&*host).await.unwrap();
    assert_eq!(
        rollup
            .conversations
            .iter()
            .find(|c| c.with == "old-worker")
            .unwrap()
            .unread,
        1
    );
    let mut before = None;
    let mut ids = Vec::new();
    loop {
        let page = super::inbox(
            &*host,
            &team("default"),
            &InboxQuery {
                conversation: Some("H1".into()),
                before,
                limit: Some(50),
                ..Default::default()
            },
            &[],
        )
        .await
        .unwrap();
        assert!(page.messages.len() <= 50);
        assert!(page.messages.iter().all(|m| m.conversation == "H1"));
        ids.extend(page.messages.iter().map(|m| m.message_id));
        before = page.messages.last().map(|m| m.message_id);
        if !page.has_more {
            break;
        }
    }
    assert_eq!(ids.len(), 599);
    assert!(ids.windows(2).all(|w| w[0] > w[1]));
    *host.projected.lock().unwrap() = None;
    let fallback = super::inbox(
        &*host,
        &team("default"),
        &InboxQuery {
            conversation: Some("old-worker".into()),
            limit: Some(50),
            ..Default::default()
        },
        &[],
    )
    .await
    .unwrap();
    assert_eq!(fallback.messages.len(), 1);
    assert!(!fallback.has_more);
    assert_eq!(
        fallback
            .conversations
            .iter()
            .find(|c| c.with == "old-worker")
            .unwrap()
            .unread,
        1
    );
}

#[test]
fn conversation_keys_follow_full_team_roots_and_break_cycles_stably() {
    let host = FakeHost::new();
    let store = host.store("default");
    let a = insert(&store, HUMAN, "H1", "human root", None);
    let b = insert(&store, "H1", HUMAN, "reply", Some(a));
    let c = insert(&store, "H2", HUMAN, "agent root", None);
    let d = insert(&store, HUMAN, "H2", "reply", Some(c));
    let mut rows: std::collections::BTreeMap<_, _> = inbox::inbox_page(&store, None, 200)
        .unwrap()
        .into_iter()
        .map(|m| (m.id, m))
        .collect();
    assert_eq!(conversation_key(&rows[&a], &rows), "H1");
    assert_eq!(conversation_key(&rows[&b], &rows), "H1");
    assert_eq!(conversation_key(&rows[&c], &rows), "H2");
    assert_eq!(conversation_key(&rows[&d], &rows), "H2");
    rows.get_mut(&a).unwrap().role = Some("reviewer".into());
    assert_eq!(conversation_key(&rows[&b], &rows), "#reviewer");
    rows.get_mut(&a).unwrap().in_reply_to = Some(b);
    assert_eq!(
        conversation_key(&rows[&a], &rows),
        conversation_key(&rows[&b], &rows)
    );
}

#[tokio::test]
async fn projected_rollup_uses_the_same_message_presentation_as_the_store() {
    let host = FakeHost::new();
    let store = host.store("default");
    let direct = insert(&store, HUMAN, "H1", "withdrawn direct mail", None);
    crate::weft::role_inbox::withdraw_message(
        &store,
        direct,
        chrono::Utc::now().timestamp_millis(),
    )
    .unwrap();
    let as_host: Arc<dyn InboxHost> = host.clone();
    send_as_human(
        &as_host,
        &team("default"),
        "#reviewer",
        "queued role mail",
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let root = insert(&store, HUMAN, "H1", "root", None);
    let reply = insert(&store, "H2", HUMAN, "reply from another worker", Some(root));
    let stored = inbox_rollup(&*host).await.unwrap();
    let mut projection = crate::live_hub::inbox::InboxProjection::default();
    let batch = crate::live_hub::inbox::read_batch(host.path("default"), true, vec![])
        .await
        .unwrap();
    projection.apply_batch(
        &*host,
        &host.sessions().await.unwrap(),
        &team("default"),
        batch,
        &[],
    );
    *host.projected.lock().unwrap() = Some(projection.view_parts(&team("default")).build());
    let projected = inbox_rollup(&*host).await.unwrap();
    assert_eq!(projected, stored);
    let mut renamed = listed();
    renamed[1].display_name = "Renamed worker".into();
    let updates = projection.refresh_sessions(&*host, &renamed);
    assert_eq!(
        projection
            .view_parts(&team("default"))
            .build()
            .messages
            .iter()
            .find(|m| m.message_id == reply)
            .unwrap()
            .conversation,
        "H1"
    );
    assert_eq!(
        updates
            .iter()
            .find(|(_, row)| row["messageId"] == reply)
            .unwrap()
            .1["conversation"]["conversation"],
        "H1"
    );
    let withdrawn = projected.messages.iter().find(|m| m.id == direct).unwrap();
    assert_eq!(withdrawn.to.as_deref(), Some("H1"));
    assert!(withdrawn.to_role.is_none() && !withdrawn.unread);
}

#[tokio::test]
async fn role_state_and_rollup_gate_configuration_enrichment_before_names_and_empty_queues() {
    use crate::context_repo::caller_scope::{tests::Fixture, CallerScope};
    let f = Fixture::new();
    let host = FakeHost::new();
    *host.synced_only.lock().unwrap() = Some(vec!["default"]);
    host.role_dirs
        .lock()
        .unwrap()
        .insert("default".into(), f.path("parent", "content"));
    let file = f.write(
        "parent",
        "content/roles/qa.yaml",
        "id: qa
name: Private QA
",
    );
    let read = |scope: CallerScope| {
        let host = host.clone();
        async move {
            let roles = roles_for(
                &*host,
                &team("default"),
                None,
                None,
                &Names::default(),
                &scope,
            )
            .await
            .unwrap();
            let rollup = inbox_rollup_for(&*host, &scope).await.unwrap();
            (
                serde_json::to_value(roles).unwrap(),
                serde_json::to_value(rollup.roles).unwrap(),
            )
        }
    };
    let denied = read(f.scope.clone()).await;
    assert!(!denied.0.to_string().contains("Private QA"));
    assert!(denied.0["roles"].as_array().unwrap().is_empty());
    std::fs::remove_file(&file).unwrap();
    assert_eq!(denied, read(f.scope.clone()).await);
    std::fs::write(
        &file,
        "id: qa
name: Shared QA
",
    )
    .unwrap();
    f.write(
        "parent",
        "config/sharing.yml",
        "share: [content/roles/qa.yaml]
",
    );
    let scope = CallerScope::team(
        &f.home,
        &1u64.into(),
        &LaunchTeam {
            org: "acme/ctx".into(),
            team: "child".into(),
        },
    );
    let shared = read(scope).await;
    assert!(shared.0.to_string().contains("Shared QA"));
    assert!(shared.1.to_string().contains("qa"));
}

#[tokio::test]
async fn the_rollup_and_a_session_inbox_carry_each_messages_reactions() {
    let host = FakeHost::new();
    let store = host.store("default");
    let id =
        inbox::inbox_insert(&store, "H1", HUMAN, "ready", None, None, None, None, None).unwrap();
    crate::weft::reactions::reaction_react(&store, id, HUMAN, "👍", None).unwrap();
    crate::weft::reactions::reaction_react(&store, id, "H1", "🎉", None).unwrap();
    let quiet =
        inbox::inbox_insert(&store, "H1", HUMAN, "later", None, None, None, None, None).unwrap();

    let rollup = inbox_rollup(&*host).await.unwrap();
    let row = rollup
        .messages
        .iter()
        .find(|m| m.team.team == "default" && m.id == id)
        .unwrap();
    let emoji: Vec<_> = row.reactions.iter().map(|g| g.emoji.as_str()).collect();
    assert_eq!(emoji, ["👍", "🎉"]);
    let wire = serde_json::to_value(row).unwrap();
    assert_eq!(wire["reactions"][0]["count"], 1);
    assert_eq!(wire["reactions"][0]["reactors"], serde_json::json!([HUMAN]));
    let none = rollup.messages.iter().find(|m| m.id == quiet).unwrap();
    assert!(none.reactions.is_empty());

    let view = session_inbox(&*host, "H1").await.unwrap();
    let row = view.messages.iter().find(|m| m.id == id).unwrap();
    assert_eq!(row.reactions.len(), 2);
}

/// What an agent's `inbox_list` answers for its served arguments: they decode as the served
/// `InboxListParams` (an unknown `include` is refused), then run the one path the handler
/// runs, on the fake host instead of the app's.
async fn agent_answer(host: &FakeHost, args: serde_json::Value) -> serde_json::Value {
    let params: crate::mcp::handler::InboxListParams =
        serde_json::from_value(args).expect("served inbox_list arguments");
    agent_inbox(&params.query, &params.include, |query| async move {
        inbox(host, &team("default"), &query, &listed()).await
    })
    .await
    .unwrap()
}

/// SV-CDE-82 case 8: 500 messages of 10 KB. The default answer lists at most 50, each body
/// over 2 KB as a 512-byte `textPreview` with `truncated`, a short body in full, no
/// conversations, and fits 64 KB. Red on the base: there was no agent shaping, and the
/// default limit of 500 returned every 10 KB body (about 5 MB).
#[tokio::test]
async fn an_agents_default_inbox_is_50_previews_under_64_kb() {
    let host = FakeHost::new();
    let store = host.store("default");
    let body = "é".repeat(5 * 1024); // 10 KB, two bytes per char: previews cut on a boundary.
    for _ in 0..499 {
        insert(&store, "H1", HUMAN, &body, None);
    }
    let short = insert(&store, "H1", HUMAN, "short note", None);

    let answer = agent_answer(&host, serde_json::json!({})).await;
    let messages = answer["messages"].as_array().unwrap();
    assert!(!messages.is_empty() && messages.len() <= AGENT_INBOX_LIMIT as usize);
    assert_eq!(messages[0]["messageId"], short, "newest first");
    assert_eq!(messages[0]["text"], "short note");
    assert!(messages[0].get("truncated").is_none());
    for message in &messages[1..] {
        assert!(message.get("text").is_none(), "{message}");
        assert_eq!(message["truncated"], true);
        let preview = message["textPreview"].as_str().unwrap();
        assert!(preview.len() <= 512 && body.starts_with(preview));
    }
    assert_eq!(answer["hasMore"], true);
    assert_eq!(
        answer["total"], 500,
        "every message matched before the limit"
    );
    assert!(answer.get("conversations").is_none());
    assert!(serde_json::to_vec(&answer).unwrap().len() <= AGENT_ANSWER_BUDGET);
    // The bridge's read of the same query is untouched: full bodies, up to 500.
    let bridge = inbox(&*host, &team("default"), &InboxQuery::default(), &listed())
        .await
        .unwrap();
    assert_eq!(bridge.messages.len(), 500);
    assert_eq!(bridge.messages[1].text, body);
}

/// SV-CDE-82 case 9: `include:["text"]` returns whole bodies, and paging with `before` the
/// last listed `messageId` while `hasMore` visits every message once, newest first, even
/// when the 64 KB budget ends a page before `limit`. Red on the base: no agent answer, and
/// no budget, so a page of 50 × 3 KB bodies was 150 KB.
#[tokio::test]
async fn an_agent_pages_full_bodies_backwards_without_gaps_or_duplicates() {
    let host = FakeHost::new();
    let store = host.store("default");
    let mut all = Vec::new();
    for n in 0..120 {
        all.push(insert(
            &store,
            "H1",
            HUMAN,
            &format!("{n:03}{}", "x".repeat(3 * 1024)),
            None,
        ));
    }
    all.reverse();

    let (mut seen, mut before, mut pages) = (Vec::new(), None::<i64>, 0);
    loop {
        let mut args = serde_json::json!({"include": ["text", "conversations"]});
        if let Some(before) = before {
            args["before"] = before.into();
        }
        let answer = agent_answer(&host, args).await;
        assert!(serde_json::to_vec(&answer).unwrap().len() <= AGENT_ANSWER_BUDGET);
        assert!(answer["conversations"].is_array());
        let messages = answer["messages"].as_array().unwrap();
        for message in messages {
            assert_eq!(message["text"].as_str().unwrap().len(), 3 + 3 * 1024);
            seen.push(message["messageId"].as_i64().unwrap());
        }
        pages += 1;
        if answer["hasMore"] != true {
            break;
        }
        before = messages.last().and_then(|m| m["messageId"].as_i64());
    }
    assert_eq!(seen, all, "every message once, in order");
    assert!(
        serde_json::from_value::<crate::mcp::handler::InboxListParams>(
            serde_json::json!({"include": ["bodies"]})
        )
        .is_err(),
        "an unknown include is refused, not ignored"
    );
    assert!(
        pages > 120 / AGENT_INBOX_LIMIT as usize + 1,
        "the budget ended pages early"
    );
}
