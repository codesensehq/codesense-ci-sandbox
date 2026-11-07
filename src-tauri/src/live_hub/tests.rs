// SPDX-License-Identifier: MIT
use super::projection::*;
use serde_json::json;

#[tokio::test]
async fn an_unused_service_save_has_a_definition_projection_without_a_session_delta() {
    use super::*;
    use crate::agents::{events, service, SaveTarget};
    use crate::routines::test_support::{agent, alice, launch_app, save_team_agent, TempHome};
    use codesense_pty_core::agent_cli::AgentCli;
    let home = TempHome::new();
    events::recorder::record();
    let principal = alice();
    let key = save_team_agent(
        &home.codesense_home(),
        &principal,
        agent("unused-live", AgentCli::Codex),
    );
    let app = launch_app(&home.codesense_home());
    app.manage(LiveHub::default());
    listen_sources(app.handle());
    refresh_definitions(app.handle()).await;
    let sessions_before = app.state::<LiveHub>().snapshot("all", "sessions").unwrap();
    let mut definition = service::read_current(&home.codesense_home(), &principal, &key).unwrap();
    definition.name = "Renamed without a session".into();
    let base_revision = definition.revision;
    service::save(
        &home.codesense_home(),
        &principal,
        SaveTarget::Update {
            key: key.clone(),
            base_revision,
        },
        definition,
        &crate::routines::Author::user("1"),
    )
    .unwrap();
    // Relay the service's real emitted payload through the installed app listener.
    app.emit(
        events::AGENTS_CHANGED,
        events::recorder::last(&key.to_string()),
    )
    .unwrap();
    app.state::<LiveHub>().next_batch().await;
    refresh_definitions(app.handle()).await;
    let snapshot = app
        .state::<LiveHub>()
        .snapshot("agentDefinitions:1:native", "agentDefinitions")
        .unwrap();
    let row = snapshot
        .snapshot
        .iter()
        .find(|r| r["key"] == key.to_string() && r["rowType"] == "definition")
        .expect("saved agent row");
    assert_eq!(row["row"]["name"], "Renamed without a session");
    assert_eq!(
        app.state::<LiveHub>()
            .snapshot("all", "sessions")
            .unwrap()
            .rev,
        sessions_before.rev
    );
}

#[tokio::test]
async fn registry_changes_wake_inbox_footprint_reconciliation() {
    use super::*;
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(LiveHub::default());
    listen_sources(app.handle());
    app.emit(
        crate::commands::registry_local::ORG_REGISTRY_CHANGED_EVENT,
        (),
    )
    .unwrap();
    assert_eq!(
        app.state::<LiveHub>().next_batch().await,
        REBUILD | FOOTPRINT | DEFINITIONS
    );
}

#[test]
fn views_advance_only_for_visible_changes_and_echo_writes() {
    let mut p = SessionsProjection::default();
    p.snapshot("team:a");
    p.snapshot("team:b");
    let row = |team: &str, title: &str| {
        (
            Some(format!("team:{team}")),
            json!({"id":team,"displayName":title}),
        )
    };
    let d = p.replace(
        vec![row("a", "one"), row("b", "two")],
        Some("write-1".into()),
    );
    assert_eq!(d.len(), 2);
    assert_eq!(d[0].request_id.as_deref(), Some("write-1"));
    assert!(p
        .replace(vec![row("a", "one"), row("b", "two")], None)
        .is_empty());
    let d = p.replace(vec![row("a", "one"), row("b", "changed")], None);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].view_key, "team:b");
    assert_eq!(d[0].rev, 2);
    assert_eq!(p.snapshot("team:a").rev, 1);
    assert_eq!(p.replace(vec![], None)[0].removes, vec!["a"]);
}
struct Agent;
impl FieldProvider for Agent {
    fn fields(&self) -> &'static [&'static str] {
        &["agent"]
    }
    fn contribute(
        &self,
        source: &serde_json::Map<String, serde_json::Value>,
        row: &mut serde_json::Map<String, serde_json::Value>,
    ) {
        row.insert("agent".into(), source["agent"].clone());
    }
}
#[test]
fn process_fields_survive_without_modules_and_each_field_has_one_owner() {
    let source = json!({"id":"process","state":"idle","agent":"test"});
    let process = json!({"id":"process","state":"idle"});
    let mut providers = FieldProviders::new(["id", "state"]);
    assert_eq!(
        providers.project(
            source.as_object().unwrap(),
            process.as_object().unwrap().clone()
        ),
        process
    );
    providers.register(Box::new(Agent)).unwrap();
    assert!(providers.register(Box::new(Agent)).is_err());
    assert_eq!(
        providers.project(
            source.as_object().unwrap(),
            process.as_object().unwrap().clone()
        ),
        source
    );
}

#[test]
fn context_index_changes_refresh_agent_definitions() {
    use super::*;
    // A teammate's agent edit arrives through the context index, without a local agent-write event.
    for event in [
        "context-index-changed",
        "context-attachments-changed",
        crate::commands::registry_local::ORG_REGISTRY_CHANGED_EVENT,
        crate::commands::scratch_use::SCRATCH_USE_CHANGED_EVENT,
        crate::commands::agent_accounts::LIFECYCLE_EVENT,
        crate::commands::agent_accounts::LOGIN_EVENT,
    ] {
        assert!(DOMAIN_EVENTS.contains(&event));
        assert_ne!(event_flags(event) & DEFINITIONS, 0);
    }
}

#[tokio::test]
async fn every_domain_source_invalidates_the_projection_and_bursts_coalesce() {
    use super::*;
    let hub = LiveHub::default();
    let started = std::time::Instant::now();
    for event in DOMAIN_EVENTS {
        hub.invalidate(event_flags(event));
    }
    let flags = hub.next_batch().await;
    assert!(started.elapsed() >= COALESCE);
    assert_eq!(
        flags,
        REBUILD | INBOX | HOSTS | FOOTPRINT | DEFINITIONS | MACHINE_STATS
    );
    assert_eq!(hub.dirty.load(Ordering::Acquire), 0);
    for event in DOMAIN_EVENTS {
        assert_ne!(event_flags(event) & REBUILD, 0, "{event}");
    }
}

#[tokio::test]
async fn file_changes_and_machine_stats_wake_independent_projections() {
    use super::*;
    let hub = LiveHub::default();
    hub.invalidate(FILES);
    let flags = hub.next_batch().await;
    assert_ne!(flags & FILES, 0);
    assert_eq!(flags & MACHINE_STATS, 0);
    hub.invalidate(MACHINE_STATS);
    let flags = hub.next_batch().await;
    assert_ne!(flags & MACHINE_STATS, 0);
    assert_eq!(flags & FILES, 0);
}

#[tokio::test]
async fn declared_turn_changes_wake_the_sessions_projection() {
    use super::*;
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(DashboardLaunchState::new(2));
    app.manage(LiveHub::default());
    listen_sources(app.handle());
    crate::mcp::agent_dot::on_signal_done(
        app.handle(),
        &crate::mcp::agent_dot::DotLedger::default(),
        "worker",
        "success",
        |_, _| true,
    );
    assert_eq!(
        app.state::<DashboardLaunchState>().turns().last("worker"),
        Some(crate::agent_state::AgentState::Done)
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), app.state::<LiveHub>().next_batch())
            .await
            .unwrap(),
        REBUILD | HOSTS
    );
}

#[tokio::test]
async fn runtime_admission_events_refresh_cached_hosts() {
    use super::*;
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(LiveHub::default());
    listen_sources(app.handle());
    app.emit("terminal-layout-admission-changed", ()).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), app.state::<LiveHub>().next_batch())
            .await
            .unwrap(),
        REBUILD | HOSTS | MACHINE_STATS
    );
}
#[test]
fn store_open_reuses_the_migrated_pool_across_reads() {
    use crate::weft::WeftStore;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inbox.sqlite3");
    let first = WeftStore::open(&path).unwrap();
    for _ in 0..20 {
        let next = WeftStore::open(&path).unwrap();
        assert!(first.shares_pool(&next));
        assert_eq!(
            next.conn()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM wv_inbox", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
#[tokio::test]
async fn cached_read_never_waits_for_a_hung_host() {
    use super::*;
    let hub = LiveHub::default();
    hub.model.lock().unwrap().begin_refresh("hung-host");
    let (rows, _) = tokio::time::timeout(Duration::from_millis(10), async { hub.rows(None) })
        .await
        .unwrap();
    assert!(rows.is_empty());
    assert!(hub.model.lock().unwrap().reading.contains("hung-host"));
}

#[test]
fn cached_remote_rows_keep_identity_offline_state_and_closed_tab_behavior() {
    use super::*;
    use crate::mcp::terminal_tabs::LiveTab;
    let mut record: crate::commands::pty_layout::RemoteShellRecord=serde_json::from_value(json!({
      "handle":"remote-agent","owner_id":41,"request_id":"00000000-0000-4000-8000-000000000001",
      "machine_id":"droplet","team_key":"acme:design","workspace_id":"w","workspace":"acme:design|w","cwd":"/home/dev","name":null,
      "target":{"member":{"machineId":"droplet","slot":"remote"},"admissionRevision":"1","namespace":{"serverId":"s","epoch":"e","generation":"1"},"session":"remote-session"},
      "agent":{"workspace_handle":"ws-1","name":"Worker","manifest":{"agent":{"key":"acme/design/worker","revision":1,"definition_hash":"h"},"runtime":"claude","entries":[],"essentials":[],"generated":[],"totals":{"files":0,"bytes":0},"errors":[],"manifest_hash":""},"runtime":"claude","startup":{"args":[],"settings":{},"prompt":null},"mcp_allow":[]},
      "identity":{"launched_by":"lead"}
    })).unwrap();
    record.status = "opened";
    let target = record.target.clone();
    let launch = DashboardLaunchState::new(2);
    launch.reserve_remote_shell(record).unwrap();
    let mut live = LiveTabs::of(
        &[],
        BTreeMap::from([("droplet".into(), "Build machine".into())]),
    );
    live.live.push("droplet".into());
    live.tabs.push(LiveTab {
        machine_id: "droplet".into(),
        machine: "Build machine".into(),
        workspace_key: "acme:design|w".into(),
        workspace: "w".into(),
        session: "remote-session".into(),
        name: "Renamed worker".into(),
        handle: Some("remote-agent".into()),
        exited: false,
        target,
        agent: Some("acme/design/worker".into()),
    });
    let rows = project_registry(&launch, &live, cached_registry(&launch, &live).unwrap());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].handle, "remote-agent");
    assert_eq!(rows[0].workspace_handle.as_deref(), Some("ws-1"));
    assert_eq!(rows[0].machine.as_deref(), Some("Build machine"));
    assert_eq!(rows[0].display_name, "Renamed worker");
    let hub = LiveHub::default();
    hub.model.lock().unwrap().projections.replace("sessions", vec![(None, json!({"id":"remote-session","sessionId":"remote-session","handle":"remote-agent","effectiveStatus":"crashed","crashId":"exit-mark"}))], None).unwrap();
    let tool_rows = hub.with_effective_status(&rows);
    assert_eq!(tool_rows[0]["sessionId"], "remote-session");
    assert_eq!(tool_rows[0]["crashId"], "exit-mark");

    live.tabs.clear();
    live.live.clear();
    live.offline.push("droplet".into());
    let offline = project_registry(&launch, &live, cached_registry(&launch, &live).unwrap());
    assert_eq!(offline[0].state, session_list::SessionState::Offline);
    assert_eq!(offline[0].workspace_handle.as_deref(), Some("ws-1"));
    live.offline.clear();
    live.live.push("droplet".into());
    assert!(cached_registry(&launch, &live).unwrap().is_empty());
}

#[test]
fn invalidation_during_host_read_retries_only_that_host() {
    let mut model = super::Model::default();
    assert!(model.begin_refresh("slow"));
    assert!(model.begin_refresh("fast"));
    assert!(!model.begin_refresh("slow"));
    assert!(!model.finish_refresh("fast"));
    assert!(model.finish_refresh("slow"));
    assert!(model.reading.contains("slow"));
    assert!(!model.reading.contains("fast"));
    assert!(!model.finish_refresh("slow"));
    assert!(model.reading.is_empty());
}

#[test]
fn failed_cached_activity_does_not_invent_a_running_session() {
    use super::*;
    use crate::commands::machines::{
        RuntimeCounter, RuntimeLayoutReply, RuntimeMemberAddress, RuntimeMemberSlot,
        RuntimeMemberTarget, RuntimeNamespace,
    };
    use codesense_pty_core::domain::{PtySessionId, ServerId, WorkspaceKey};
    use codesense_pty_core::layout::{
        EntryState, LayoutSnapshot, Placement, TabEntry, Tags, ViewerLayout, WorkspaceLayout,
    };
    let mut workspace = WorkspaceLayout::default();
    workspace.tabs.push(TabEntry {
        session: PtySessionId::new("worker"),
        name: "Worker".into(),
        role: None,
        placement: Placement::Tab,
        state: EntryState::Live,
        tags: Tags::new(),
    });
    let reply = RuntimeLayoutReply::Live {
        private_authority_epoch: None,
        target: RuntimeMemberTarget {
            member: RuntimeMemberAddress {
                machine_id: crate::commands::machines::LOCAL_MACHINE_ID.into(),
                slot: RuntimeMemberSlot::InProcess,
            },
            admission_revision: RuntimeCounter::new(1),
            namespace: RuntimeNamespace {
                server_id: "server".into(),
                epoch: "epoch".into(),
                generation: RuntimeCounter::new(1),
            },
        },
        snapshot: Box::new(LayoutSnapshot {
            schema: 1,
            server_id: ServerId::new("server"),
            server_name: "server".into(),
            epoch: "epoch".into(),
            rev: 1,
            workspaces: BTreeMap::from([(WorkspaceKey::new("acme:design|work"), workspace)]),
            viewer: ViewerLayout::default(),
        }),
    };
    let mut model = Model::default();
    model.hosts.insert(
        "local".into(),
        (reply, Err("activity owner unavailable".into())),
    );
    let launch = DashboardLaunchState::new(2);
    let live = model.live_tabs();
    let rows = project_registry(&launch, &live, cached_registry(&launch, &live).unwrap());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, session_list::SessionState::Unknown);
}

#[test]
fn registry_retains_view_deltas_and_falls_back_after_retention() {
    let mut registry = TopicRegistry::default();
    registry.snapshot("sessions", "team:a").unwrap();
    for i in 0..300 {
        registry
            .replace(
                "sessions",
                vec![(Some("team:a".into()), json!({"id":"a", "n":i}))],
                None,
            )
            .unwrap();
    }
    let replay = registry.subscribe("sessions", "team:a", Some(298)).unwrap();
    assert_eq!(replay.deltas.as_ref().unwrap().len(), 2);
    assert!(replay.snapshot.is_none());
    let old = registry.subscribe("sessions", "team:a", Some(1)).unwrap();
    assert_eq!(old.snapshot.unwrap().len(), 1);
    assert!(registry.snapshot("unknown", "all").is_err());
    assert_eq!(
        registry.snapshot("ui:dock", "team:a").unwrap().topic,
        "ui:dock"
    );
}

#[test]
fn inbox_subscription_windows_and_pages_remain_bounded() {
    let mut registry = TopicRegistry::default();
    let rows = (1..=5000)
        .map(|id| {
            (
                Some("team:acme/a".into()),
                json!({"id":id.to_string(), "messageId":id}),
            )
        })
        .collect();
    registry.replace("inbox", rows, None).unwrap();
    let first = registry
        .subscribe_window("one", "inbox", "team:acme/a", None, None)
        .unwrap();
    assert_eq!(first.snapshot.as_ref().unwrap().len(), 200);
    assert_eq!(first.total, Some(5000));
    let mut seen = first.snapshot.unwrap();
    let mut cursor = first.cursor;
    while let Some(before) = cursor {
        let page = registry.page("one", &before).unwrap();
        seen.extend(page.snapshot);
        cursor = page.cursor;
    }
    assert_eq!(seen.len(), 5000);
    assert_eq!(
        seen.iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        5000
    );
    registry
        .subscribe_window("two", "inbox", "team:acme/a", None, Some([0, 50]))
        .unwrap();
    let moved = registry.move_window("two", [25, 75]).unwrap();
    assert_eq!(moved.upserts.len(), 25);
    assert_eq!(moved.removes.len(), 25);
    let changes = registry
        .patch(
            "inbox",
            vec![(
                Some("team:acme/a".into()),
                json!({"id":"1","messageId":1,"unread":false}),
            )],
            vec![],
            None,
        )
        .unwrap();
    assert!(changes.iter().all(|(id, _)| id != "two"));
    let again = registry
        .subscribe_window("two", "inbox", "team:acme/a", None, Some([25, 75]))
        .unwrap();
    assert_eq!(again.snapshot.unwrap().len(), 50);
    assert_eq!(
        registry
            .snapshot("inbox", "team:acme/b")
            .unwrap()
            .snapshot
            .len(),
        0
    );
}

#[test]
fn paged_subscriptions_do_not_send_unloaded_old_changes() {
    let mut registry = TopicRegistry::default();
    registry
        .replace(
            "inbox",
            (1..=5000)
                .map(|n| {
                    (
                        Some("team:a".into()),
                        json!({"id":n.to_string(),"messageId":n}),
                    )
                })
                .collect(),
            None,
        )
        .unwrap();
    registry
        .subscribe_window("paged", "inbox", "team:a", None, None)
        .unwrap();
    let sent = registry
        .patch(
            "inbox",
            vec![(
                Some("team:a".into()),
                json!({"id":"1","messageId":1,"read":true}),
            )],
            vec![],
            None,
        )
        .unwrap();
    assert!(sent.is_empty());
    let sent = registry
        .patch(
            "inbox",
            vec![(Some("team:a".into()), json!({"id":"5001","messageId":5001}))],
            vec![],
            None,
        )
        .unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1.upserts.len(), 1);
}

#[test]
fn team_inbox_counts_change_incrementally_and_human_self_mail_is_excluded() {
    let mut inbox = super::inbox::InboxProjection::default();
    let team = crate::commands::dashboard_launch::LaunchTeam {
        org: "acme".into(),
        team: "a".into(),
    };
    let dir = tempfile::tempdir().unwrap();
    let store = crate::weft::WeftStore::open(dir.path().join("inbox.sqlite3")).unwrap();
    for (from, to) in [
        ("human", "human"),
        ("worker", "human"),
        ("worker", "worker"),
    ] {
        crate::weft::inbox::inbox_insert(&store, from, to, "note", None, None, None, None, None)
            .unwrap();
    }
    let rows = crate::weft::inbox::inbox_list(&store).unwrap();
    for row in rows {
        let kind = row.effective_kind();
        inbox.put(&team, row, kind);
    }
    assert_eq!(inbox.counts(&team)["humanUnread"], 1);
    assert_eq!(inbox.counts(&team)["recipients"]["worker"], 1);
    let mut row = crate::weft::inbox::inbox_get(&store, 2).unwrap().unwrap();
    row.read_at_ms = Some(123);
    inbox.put(&team, row, crate::weft::inbox::MessageKind::Update);
    assert_eq!(inbox.counts(&team)["humanUnread"], 0);
}

#[tokio::test]
async fn typed_inbox_events_wake_only_the_inbox_projection() {
    use super::*;
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(LiveHub::default());
    listen_sources(app.handle());
    let team = crate::commands::dashboard_launch::LaunchTeam {
        org: "acme".into(),
        team: "a".into(),
    };
    app.emit(
        inbox_events::INBOX_EVENT,
        inbox_events::InboxEvent::new(inbox_events::InboxChange::Read, &team, [2, 4]),
    )
    .unwrap();
    assert_eq!(app.state::<LiveHub>().next_batch().await, INBOX);
    assert_eq!(
        app.state::<LiveHub>().model.lock().unwrap().inbox_pending["team:acme/a"].1,
        BTreeSet::from([2, 4])
    );
}

#[test]
fn retained_window_replay_preserves_boundary_membership_after_hidden_arrivals() {
    let mut registry = TopicRegistry::default();
    registry
        .replace(
            "inbox",
            (1..=100)
                .map(|n| {
                    (
                        Some("team:a".into()),
                        json!({"id":n.to_string(),"messageId":n}),
                    )
                })
                .collect(),
            None,
        )
        .unwrap();
    let first = registry
        .subscribe_window("hidden", "inbox", "team:a", None, Some([10, 30]))
        .unwrap();
    let mut materialized = first
        .snapshot
        .unwrap()
        .into_iter()
        .map(|r| (r["id"].as_str().unwrap().to_owned(), r))
        .collect::<std::collections::BTreeMap<_, _>>();
    registry
        .patch(
            "inbox",
            vec![(Some("team:a".into()), json!({"id":"101","messageId":101}))],
            vec![],
            None,
        )
        .unwrap();
    let replay = registry
        .subscribe_window("hidden", "inbox", "team:a", Some(first.rev), Some([10, 30]))
        .unwrap();
    for delta in replay.deltas.unwrap() {
        for id in delta.removes {
            materialized.remove(&id);
        }
        for row in delta.upserts {
            materialized.insert(row["id"].as_str().unwrap().to_owned(), row);
        }
    }
    let expected = registry
        .subscribe_window("fresh", "inbox", "team:a", None, Some([10, 30]))
        .unwrap()
        .snapshot
        .unwrap()
        .into_iter()
        .map(|r| (r["id"].as_str().unwrap().to_owned(), r))
        .collect();
    assert_eq!(materialized, expected);
}
#[test]
fn cross_team_arrivals_and_newest_order_do_not_compare_team_local_ids() {
    let mut registry = TopicRegistry::default();
    let row = |team: &str, id: i64, at: i64| {
        (
            Some(format!("team:{team}")),
            json!({"id":format!("{team}/{id}"),"messageId":id,"createdAt":at,"team":{"org":"acme","team":team}}),
        )
    };
    registry
        .replace("inbox", vec![row("a", 5000, 1), row("b", 10, 2)], None)
        .unwrap();
    let first = registry
        .subscribe_window("all", "inbox", "all", None, None)
        .unwrap();
    assert_eq!(first.snapshot.unwrap()[0]["id"], "b/10");
    let sent = registry
        .patch("inbox", vec![row("b", 11, 3)], vec![], None)
        .unwrap();
    assert!(sent[0].1.upserts.iter().any(|r| r["id"] == "b/11"));
}

#[test]
fn cross_team_paging_walks_independent_id_sequences_without_gaps() {
    let mut registry = TopicRegistry::default();
    let mut rows = Vec::new();
    for (team, size) in [("busy", 501), ("quiet", 102)] {
        for id in 1..=size {
            rows.push((Some(format!("team:acme/{team}")),json!({"id":format!("{team}/{id}"),"team":{"org":"acme","team":team},"messageId":id,"createdAt":id/3*2+if team=="quiet"{1}else{0}})));
        }
    }
    registry.replace("inbox", rows, None).unwrap();
    let first = registry
        .subscribe_window("mixed", "inbox", "all", None, None)
        .unwrap();
    let mut seen = first.snapshot.unwrap();
    let mut cursor = first.cursor;
    while let Some(before) = cursor {
        let page = registry.page("mixed", &before).unwrap();
        assert!(page.snapshot.len() <= 200);
        seen.extend(page.snapshot);
        cursor = page.cursor;
    }
    assert_eq!(seen.len(), 603);
    assert_eq!(
        seen.iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        603
    );
    assert!(seen
        .windows(2)
        .all(|pair| pair[0]["createdAt"].as_i64() >= pair[1]["createdAt"].as_i64()));
}

#[test]
fn cross_team_paging_excludes_live_arrivals_before_a_teams_first_page() {
    let mut registry = TopicRegistry::default();
    let row = |team: &str, id: i64, at: i64| {
        (
            Some(format!("team:acme/{team}")),
            json!({"id":format!("message:{team}/{id}"),"rowType":"message","team":{"org":"acme","team":team},"messageId":id,"createdAt":at}),
        )
    };
    let mut rows: Vec<_> = (1..=601).map(|id| row("busy", id, 1000 + id)).collect();
    rows.extend((1..=3).map(|id| row("quiet", id, id)));
    for team in ["busy", "quiet"] {
        rows.push((
            Some(format!("team:acme/{team}")),
            json!({"id":format!("counts:{team}"),"rowType":"counts","team":{"org":"acme","team":team}}),
        ));
    }
    registry.replace("inbox", rows, None).unwrap();
    let first = registry
        .subscribe_window("mixed-live", "inbox", "all", None, None)
        .unwrap();
    let first_rows = first.snapshot.unwrap();
    assert_eq!(first_rows.len(), 200);
    assert!(!first_rows
        .iter()
        .any(|row| row["rowType"] == "message" && row["team"]["team"] == "quiet"));
    let mut seen: std::collections::BTreeSet<_> = first_rows
        .iter()
        .map(|row| row["id"].as_str().unwrap().to_string())
        .collect();

    // The quiet team's cursor is still MAX when its next message arrives live.
    let deliveries = registry
        .patch("inbox", vec![row("quiet", 4, 2000)], vec![], None)
        .unwrap();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].1.upserts[0]["id"], "message:quiet/4");
    for row in &deliveries[0].1.upserts {
        assert!(seen.insert(row["id"].as_str().unwrap().to_string()));
    }
    let mut cursor = first.cursor;
    while let Some(before) = cursor {
        let page = registry.page("mixed-live", &before).unwrap();
        assert!(page.snapshot.len() <= PAGE_SIZE);
        for row in page.snapshot {
            assert!(
                seen.insert(row["id"].as_str().unwrap().to_string()),
                "page repeated a loaded row"
            );
        }
        cursor = page.cursor;
    }
    let expected: std::collections::BTreeSet<_> = registry
        .snapshot("inbox", "all")
        .unwrap()
        .snapshot
        .iter()
        .map(|row| row["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(seen, expected, "paging must retain every older message");
}

struct FootprintHost {
    dir: tempfile::TempDir,
    teams: std::sync::Mutex<Vec<crate::commands::dashboard_launch::LaunchTeam>>,
    discovery_failed: std::sync::atomic::AtomicBool,
    store_failed: std::sync::atomic::AtomicBool,
    /// Every committed-pass report, in order.
    committed:
        std::sync::Mutex<Vec<Vec<(crate::commands::dashboard_launch::LaunchTeam, Vec<i64>)>>>,
    presentation_gate: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
}
impl FootprintHost {
    fn team(name: &str) -> crate::commands::dashboard_launch::LaunchTeam {
        crate::commands::dashboard_launch::LaunchTeam {
            org: "acme".into(),
            team: name.into(),
        }
    }
    fn path(&self, name: &str) -> std::path::PathBuf {
        self.dir.path().join(name).join("inbox.sqlite3")
    }
}
#[async_trait::async_trait]
impl crate::commands::agent_inbox::InboxHost for FootprintHost {
    fn present_media(
        &self,
        team: &crate::commands::dashboard_launch::LaunchTeam,
        attachments: &mut [crate::media::Attachment],
    ) {
        let gate = self.presentation_gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate();
        }
        if !attachments.is_empty() {
            self.media_config(team).apply(attachments);
        }
    }
    async fn team_store(
        &self,
        team: &crate::commands::dashboard_launch::LaunchTeam,
    ) -> Result<std::path::PathBuf, String> {
        if team.team == "attached" && self.store_failed.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("temporarily unavailable".into());
        }
        Ok(self.path(&team.team))
    }
    async fn active_team(&self) -> Result<crate::commands::dashboard_launch::LaunchTeam, String> {
        Ok(Self::team("active"))
    }
    async fn synced_teams(
        &self,
    ) -> Result<Vec<crate::commands::dashboard_launch::LaunchTeam>, String> {
        if self
            .discovery_failed
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err("temporarily unavailable".into());
        }
        Ok(self.teams.lock().unwrap().clone())
    }
    fn launch_team(&self, _: &str) -> Option<crate::commands::dashboard_launch::LaunchTeam> {
        None
    }
    fn handle_for_session(&self, _: &str) -> Option<String> {
        None
    }
    async fn sessions(&self) -> Result<crate::commands::agent_inbox::KnownSessions, String> {
        Ok(crate::commands::agent_inbox::KnownSessions::new(vec![]))
    }
    fn has_handle(&self, _: &str) -> bool {
        false
    }
    fn display_name(&self, _: &str) -> Option<String> {
        None
    }
    fn inbox_changed(&self) {}
    fn inbox_committed(
        &self,
        changes: &[(crate::commands::dashboard_launch::LaunchTeam, Vec<i64>)],
    ) {
        self.committed.lock().unwrap().push(changes.to_vec());
    }
}

#[tokio::test]
async fn context_and_registry_changes_reconcile_inbox_rows_counts_and_reloads() {
    use super::*;
    use crate::weft::{inbox as ledger, WeftStore};
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(LiveHub::default());
    listen_sources(app.handle());
    let host = FootprintHost {
        dir: tempfile::tempdir().unwrap(),
        teams: Mutex::new(vec![FootprintHost::team("active")]),
        discovery_failed: Default::default(),
        store_failed: Default::default(),
        committed: Default::default(),
        presentation_gate: Default::default(),
    };
    for team in ["active", "attached"] {
        let store = WeftStore::open(host.path(team)).unwrap();
        ledger::inbox_insert(
            &store, "worker", "human", "mail", None, None, None, None, None,
        )
        .unwrap();
    }
    let hub = app.state::<LiveHub>();
    let resubscribe = || {
        hub.model
            .lock()
            .unwrap()
            .projections
            .subscribe_window("native", "inbox", "all", None, None)
            .unwrap();
    };
    resubscribe();
    let sent = std::sync::Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let received = sent.clone();
    app.listen("codesense:subscription-delta", move |event| {
        received
            .lock()
            .unwrap()
            .push(serde_json::from_str(event.payload()).unwrap());
    });
    refresh_inbox_with_host(app.handle(), &host, false)
        .await
        .unwrap();
    resubscribe();
    assert!(!hub
        .model
        .lock()
        .unwrap()
        .inbox
        .contains_team(&FootprintHost::team("attached")));

    // Attaching wakes the real refresh path without requiring session activity.
    host.teams
        .lock()
        .unwrap()
        .push(FootprintHost::team("attached"));
    app.emit("context-attachments-changed", ()).unwrap();
    let flags = hub.next_batch().await;
    assert_eq!(flags, REBUILD | FOOTPRINT | DEFINITIONS);
    refresh_inbox_with_host(app.handle(), &host, flags & FOOTPRINT != 0)
        .await
        .unwrap();
    resubscribe();
    assert_eq!(
        hub.model
            .lock()
            .unwrap()
            .inbox
            .counts(&FootprintHost::team("attached"))["humanUnread"],
        1
    );

    // Neither discovery nor per-store resolution failures mean a team was disconnected.
    host.discovery_failed.store(true, Ordering::Relaxed);
    refresh_inbox_with_host(app.handle(), &host, true)
        .await
        .unwrap();
    assert!(hub
        .model
        .lock()
        .unwrap()
        .inbox
        .contains_team(&FootprintHost::team("attached")));
    host.discovery_failed.store(false, Ordering::Relaxed);
    host.store_failed.store(true, Ordering::Relaxed);
    refresh_inbox_with_host(app.handle(), &host, true)
        .await
        .unwrap();
    assert!(hub
        .model
        .lock()
        .unwrap()
        .inbox
        .contains_team(&FootprintHost::team("attached")));
    host.store_failed.store(false, Ordering::Relaxed);

    host.teams.lock().unwrap().clear();
    app.emit("context-attachments-changed", ()).unwrap();
    let flags = hub.next_batch().await;
    refresh_inbox_with_host(app.handle(), &host, flags & FOOTPRINT != 0)
        .await
        .unwrap();
    let mut model = hub.model.lock().unwrap();
    assert!(!model.inbox.contains_team(&FootprintHost::team("attached")));
    assert!(!model.inbox.bridge_rows.contains_key("team:acme/attached"));
    let snapshot = model.projections.snapshot("inbox", "all").unwrap();
    assert_eq!(
        snapshot.snapshot.len(),
        2,
        "active team and its mail remain"
    );
    assert_eq!(
        snapshot
            .snapshot
            .iter()
            .filter_map(|row| row["humanUnread"].as_u64())
            .sum::<u64>(),
        1
    );
    drop(model);
    assert!(sent.lock().unwrap().iter().any(|event| {
        let removes = event["delta"]["removes"].as_array().unwrap();
        removes.contains(&json!("counts:team:acme/attached"))
            && removes.contains(&json!("message:acme/attached/1"))
    }));

    // Reattachment must reload the ledger, including mail absent from the retired cache.
    let store = WeftStore::open(host.path("attached")).unwrap();
    ledger::inbox_insert(
        &store, "worker", "human", "new mail", None, None, None, None, None,
    )
    .unwrap();
    host.teams
        .lock()
        .unwrap()
        .push(FootprintHost::team("attached"));
    app.emit("context-attachments-changed", ()).unwrap();
    let flags = hub.next_batch().await;
    refresh_inbox_with_host(app.handle(), &host, flags & FOOTPRINT != 0)
        .await
        .unwrap();
    let model = hub.model.lock().unwrap();
    assert_eq!(
        model.inbox.counts(&FootprintHost::team("attached"))["humanUnread"],
        2
    );
    assert_eq!(model.inbox.bridge_rows["team:acme/attached"].len(), 2);
    drop(model);
    resubscribe();

    // Archiving an inactive team changes the registry rather than its attachment.
    host.teams.lock().unwrap().clear();
    app.emit(
        crate::commands::registry_local::ORG_REGISTRY_CHANGED_EVENT,
        json!({"orgSlug":"acme","edit":"ArchiveTeam","team":"attached"}),
    )
    .unwrap();
    let flags = hub.next_batch().await;
    assert_eq!(flags, REBUILD | FOOTPRINT | DEFINITIONS);
    refresh_inbox_with_host(app.handle(), &host, flags & FOOTPRINT != 0)
        .await
        .unwrap();
    let mut model = hub.model.lock().unwrap();
    assert!(!model.inbox.contains_team(&FootprintHost::team("attached")));
    let snapshot = model.projections.snapshot("inbox", "all").unwrap();
    assert_eq!(snapshot.snapshot.len(), 2);
    assert_eq!(
        snapshot
            .snapshot
            .iter()
            .filter_map(|row| row["humanUnread"].as_u64())
            .sum::<u64>(),
        1
    );
}

#[tokio::test]
async fn initial_inbox_hydration_resets_subscribers_before_bounded_resync() {
    use super::*;
    use crate::weft::{inbox as ledger, WeftStore};
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(LiveHub::default());
    let host = FootprintHost {
        dir: tempfile::tempdir().unwrap(),
        teams: Mutex::new(vec![FootprintHost::team("active")]),
        discovery_failed: Default::default(),
        store_failed: Default::default(),
        committed: Default::default(),
        presentation_gate: Default::default(),
    };
    let store = WeftStore::open(host.path("active")).unwrap();
    for _ in 0..5000 {
        ledger::inbox_insert(
            &store, "worker", "human", "mail", None, None, None, None, None,
        )
        .unwrap();
    }
    let hub = app.state::<LiveHub>();
    let (paged, windowed, raw) = {
        let mut model = hub.model.lock().unwrap();
        (
            model
                .projections
                .subscribe_window("paged", "inbox", "all", None, None)
                .unwrap(),
            model
                .projections
                .subscribe_window("windowed", "inbox", "all", None, Some([100, 150]))
                .unwrap(),
            model
                .projections
                .subscribe("inbox", "team:acme/active", None)
                .unwrap(),
        )
    };
    let sent = std::sync::Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let received = sent.clone();
    app.listen("codesense:subscription-delta", move |event| {
        received
            .lock()
            .unwrap()
            .push(serde_json::from_str(event.payload()).unwrap());
    });
    refresh_inbox_with_host(app.handle(), &host, false)
        .await
        .unwrap();
    let sent = sent.lock().unwrap();
    assert_eq!(sent.len(), 2);
    for event in sent.iter() {
        assert!(event["baseRev"].is_null());
        assert_eq!(event["delta"]["upserts"], json!([]));
        assert_eq!(event["delta"]["removes"], json!([]));
        assert_eq!(event["delta"]["total"], 5001);
    }
    drop(sent);
    let mut model = hub.model.lock().unwrap();
    assert!(model
        .projections
        .move_window("windowed", [100, 150])
        .is_err());
    let raw = model
        .projections
        .subscribe("inbox", "team:acme/active", Some(raw.rev))
        .unwrap();
    assert!(raw.deltas.is_none());
    assert_eq!(raw.snapshot.unwrap().len(), PAGE_SIZE);
    let windowed = model
        .projections
        .subscribe_window(
            "windowed",
            "inbox",
            "all",
            Some(windowed.rev),
            Some([100, 150]),
        )
        .unwrap();
    let full = model.projections.snapshot("inbox", "all").unwrap();
    assert_eq!(windowed.snapshot.unwrap(), full.snapshot[100..150]);
    let paged = model
        .projections
        .subscribe_window("paged", "inbox", "all", Some(paged.rev), None)
        .unwrap();
    let mut seen = paged.snapshot.unwrap();
    assert_eq!(seen.len(), PAGE_SIZE);
    let mut cursor = paged.cursor;
    while let Some(before) = cursor {
        let page = model.projections.page("paged", &before).unwrap();
        assert!(page.snapshot.len() <= PAGE_SIZE);
        seen.extend(page.snapshot);
        cursor = page.cursor;
    }
    assert_eq!(seen.len(), 5001);
    assert_eq!(
        seen.iter()
            .map(|row| row["id"].as_str().unwrap())
            .collect::<BTreeSet<_>>()
            .len(),
        5001
    );
}

#[tokio::test]
async fn team_hydration_preserves_other_views_loaded_pages_and_replay() {
    use super::*;
    use crate::weft::{inbox as ledger, WeftStore};
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(LiveHub::default());
    listen_sources(app.handle());
    let host = FootprintHost {
        dir: tempfile::tempdir().unwrap(),
        teams: Mutex::new(vec![FootprintHost::team("active")]),
        discovery_failed: Default::default(),
        store_failed: Default::default(),
        committed: Default::default(),
        presentation_gate: Default::default(),
    };
    let store = WeftStore::open(host.path("active")).unwrap();
    for _ in 0..401 {
        ledger::inbox_insert(
            &store, "worker", "human", "mail", None, None, None, None, None,
        )
        .unwrap();
    }
    refresh_inbox_with_host(app.handle(), &host, false)
        .await
        .unwrap();
    let hub = app.state::<LiveHub>();
    let first_rev = {
        let mut model = hub.model.lock().unwrap();
        let first = model
            .projections
            .subscribe_window("team-a", "inbox", "team:acme/active", None, None)
            .unwrap();
        let mut cursor = first.cursor;
        while let Some(before) = cursor {
            cursor = model.projections.page("team-a", &before).unwrap().cursor;
        }
        model
            .projections
            .subscribe_window("all", "inbox", "all", None, None)
            .unwrap();
        first.rev
    };
    let sent = std::sync::Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let received = sent.clone();
    app.listen("codesense:subscription-delta", move |event| {
        received
            .lock()
            .unwrap()
            .push(serde_json::from_str(event.payload()).unwrap());
    });
    // Give team A a retained delta before another team's initial history arrives.
    ledger::inbox_mark_read(&store, 1, "human").unwrap();
    app.emit(
        inbox_events::INBOX_EVENT,
        inbox_events::InboxEvent::new(
            inbox_events::InboxChange::Read,
            &FootprintHost::team("active"),
            [1],
        ),
    )
    .unwrap();
    hub.next_batch().await;
    refresh_inbox_with_host(app.handle(), &host, false)
        .await
        .unwrap();
    sent.lock().unwrap().clear();

    let attached = WeftStore::open(host.path("attached")).unwrap();
    ledger::inbox_insert(
        &attached,
        "worker",
        "human",
        "other team's history",
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    host.teams
        .lock()
        .unwrap()
        .push(FootprintHost::team("attached"));
    app.emit("context-attachments-changed", ()).unwrap();
    let flags = hub.next_batch().await;
    refresh_inbox_with_host(app.handle(), &host, flags & FOOTPRINT != 0)
        .await
        .unwrap();
    let resets = std::mem::take(&mut *sent.lock().unwrap());
    assert_eq!(resets.len(), 1);
    assert_eq!(resets[0]["subscriptionId"], "all");
    assert!(resets[0]["baseRev"].is_null());
    let replay = hub
        .model
        .lock()
        .unwrap()
        .projections
        .subscribe_window("team-a", "inbox", "team:acme/active", Some(first_rev), None)
        .unwrap();
    assert!(
        replay.snapshot.is_none(),
        "unaffected view retains its log and subscription"
    );
    assert_eq!(replay.deltas.unwrap().len(), 1);

    // The unchanged subscription still owns and receives updates to its oldest page.
    ledger::inbox_mark_read(&store, 2, "human").unwrap();
    app.emit(
        inbox_events::INBOX_EVENT,
        inbox_events::InboxEvent::new(
            inbox_events::InboxChange::Read,
            &FootprintHost::team("active"),
            [2],
        ),
    )
    .unwrap();
    hub.next_batch().await;
    refresh_inbox_with_host(app.handle(), &host, false)
        .await
        .unwrap();
    let updates = sent.lock().unwrap();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0]["subscriptionId"], "team-a");
    assert!(updates[0]["delta"]["upserts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["id"] == "message:acme/active/2" && row["message"]["unread"] == false));
}

#[tokio::test(start_paused = true)]
async fn stall_deadline_wakes_once_and_activity_rearms_the_single_sleep() {
    use super::*;
    use std::sync::Arc;
    let hub = Arc::new(LiveHub::default());
    let now = tokio::time::Instant::now();
    hub.model.lock().unwrap().stall_deadline = Some(now + Duration::from_millis(600_000));
    let waiting = hub.clone();
    let task = tokio::spawn(async move { waiting.next_batch().await });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(599_999)).await;
    assert!(!task.is_finished());
    // Actual activity wakes the same loop and moves its next deadline.
    hub.model.lock().unwrap().stall_deadline =
        Some(tokio::time::Instant::now() + Duration::from_millis(600_000));
    hub.invalidate(REBUILD);
    assert_eq!(task.await.unwrap(), REBUILD);
    let waiting = hub.clone();
    let task = tokio::spawn(async move { waiting.next_batch().await });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(1)).await;
    assert!(!task.is_finished(), "the former deadline was cancelled");
    assert_eq!(task.await.unwrap(), REBUILD | HOSTS);
    assert!(hub.model.lock().unwrap().stall_deadline.is_none());
    assert!(
        tokio::time::timeout(Duration::from_secs(1), hub.next_batch())
            .await
            .is_err(),
        "no periodic wake follows a stall"
    );
}

#[tokio::test(start_paused = true)]
async fn headless_pending_asks_clear_at_the_registry_owner_on_every_exit() {
    use super::effective_status::*;
    use crate::mcp::state::AskUserRegistry;
    use std::time::Duration;
    for exit in ["answer", "cancel", "timeout"] {
        let registry = AskUserRegistry::default();
        let (id, _receiver) = registry.register("headless");
        registry.changed().await;
        let row = serde_json::json!({"sessionId":"headless","alive":false});
        let signals = select_live_signals(
            None,
            &LegacySignals::default(),
            None,
            None,
            Some("running"),
            registry.pending_sessions().contains("headless"),
            false,
            None,
            None,
            None,
            None,
            false,
            0,
        );
        assert_eq!(
            effective_status(&row, Some("running"), Some(&signals)),
            AgentStatus::AwaitingAnswer
        );
        match exit {
            "answer" => {
                registry.take(&id);
            }
            "cancel" => {
                registry.cancel_by_session("headless");
            }
            "timeout" => {
                tokio::time::sleep(Duration::from_secs(600)).await;
                registry.take(&id);
            }
            _ => unreachable!(),
        }
        registry.changed().await;
        let signals = select_live_signals(
            None,
            &LegacySignals::default(),
            None,
            None,
            Some("running"),
            registry.pending_sessions().contains("headless"),
            false,
            None,
            None,
            None,
            None,
            false,
            0,
        );
        assert_eq!(
            effective_status(&row, Some("running"), Some(&signals)),
            AgentStatus::Unknown,
            "{exit}"
        );
    }
}

#[tokio::test]
async fn done_events_retain_the_completion_owner_and_a_new_turn_releases_the_result() {
    use super::*;
    use crate::mcp::session_status::SessionOutcomes;
    use codesense_pty_core::activity::Snapshot as ActivitySnapshot;
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(LiveHub::default());
    app.manage(SessionOutcomes::default());
    listen_sources(app.handle());
    let identity =
        serde_json::json!({"hostBootId":"b","sessionIncarnation":"p","invocationId":"i"});
    app.emit("mcp:agent-done", serde_json::json!({"sessionId":"s","status":"failed","activityCompletion":{"identity":identity,"turnGeneration":1,"revision":5}})).unwrap();
    let owner = app.state::<SessionOutcomes>();
    assert_eq!(owner.snapshot()["s"].outcome, "failed");
    let mut snapshot: ActivitySnapshot = serde_json::from_value(serde_json::json!({"session":"s","identity":identity,"turnGeneration":2,"revision":6,"provider":"claude","state":"thinking","observed":true,"limited":false,"exit":null,"asks":[]})).unwrap();
    owner.observe(&snapshot);
    assert!(owner.snapshot()["s"].new_turn_seen);
    snapshot.state = codesense_pty_core::activity::State::Done;
    owner.observe(&snapshot);
    assert!(
        owner.snapshot()["s"].new_turn_seen,
        "a later Stop cannot revive the previous result"
    );
    let completed = owner.snapshot()["s"].completion.clone();
    owner.record("s", "failed", completed.clone(), 0);
    assert!(
        owner.snapshot()["s"].new_turn_seen,
        "a repeated completion cannot reclaim the new turn"
    );
    let late = SessionOutcomes::default();
    snapshot.state = codesense_pty_core::activity::State::Thinking;
    late.observe(&snapshot);
    snapshot.state = codesense_pty_core::activity::State::Done;
    late.observe(&snapshot);
    late.record("s", "failed", completed, 0);
    assert!(
        late.snapshot()["s"].new_turn_seen,
        "owner turn order beats event receipt order"
    );

    assert_eq!(app.state::<LiveHub>().next_batch().await, REBUILD | HOSTS);
}

#[tokio::test]
async fn a_followup_ask_replaces_a_retained_result_and_closure_returns_to_running() {
    use super::*;
    use crate::mcp::{session_status::SessionOutcomes, state::AskUserRegistry};
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(LiveHub::default());
    app.manage(SessionOutcomes::default());
    app.manage(AskUserRegistry::default());
    listen_sources(app.handle());
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("effective_status/parity.json")).unwrap();
    for case in fixture["managedAsk"].as_array().unwrap() {
        let finish = case["finish"].as_str().unwrap();
        app.emit("mcp:agent-done", json!({"sessionId":"s","status":finish}))
            .unwrap();
        let registry = app.state::<AskUserRegistry>();
        let (id, _reply) = registry.register("s");
        app.emit("mcp:ask-user", json!({"sessionId":"s","correlationId":id}))
            .unwrap();
        let owner = app.state::<SessionOutcomes>();
        owner.reconcile_asks(&registry.pending_sessions());
        assert_eq!(owner.snapshot()["s"].outcome, "awaiting-answer");
        registry.take(&id);
        owner.reconcile_asks(&registry.pending_sessions());
        assert_eq!(
            owner.snapshot()["s"].outcome,
            case["afterClose"].as_str().unwrap(),
            "{finish}"
        );
    }
}

fn project_managed_outcome(
    hub: &super::LiveHub,
    owner: &crate::mcp::session_status::SessionOutcomes,
    snapshot: Option<&codesense_pty_core::activity::Snapshot>,
    legacy: &super::effective_status::LegacySignals,
) -> Vec<Delta> {
    use super::effective_status::*;
    let outcome = owner.get("s").unwrap();
    let signals = select_live_signals(
        snapshot,
        legacy,
        legacy.state.as_deref().and_then(|s| match s {
            "working" => Some("running"),
            "thinking" => Some("thinking"),
            "done" => Some("done"),
            "needs-permission" | "blocked" => Some("needs-permission"),
            "errored" => Some("error"),
            _ => None,
        }),
        None,
        Some(&outcome.outcome),
        false,
        false,
        None,
        None,
        outcome.completion.as_ref(),
        None,
        false,
        0,
    );
    let runtime = json!({"id":"s","sessionId":"s","handle":"worker","alive":true,"status":"running",
        "_managedOutcome":outcome.outcome,"_statusSignals":signals});
    let wire = super::agent_list::sessions::project(&[], &[runtime], &mut [], &hub.providers);
    hub.model
        .lock()
        .unwrap()
        .projections
        .replace("sessions", wire, None)
        .unwrap()
}

#[tokio::test(start_paused = true)]
async fn a_managed_stop_without_signal_done_publishes_dropped_after_the_quiet_window() {
    use super::*;
    use crate::mcp::session_status::SessionOutcomes;
    let hub = LiveHub::default();
    let owner = SessionOutcomes::default();
    let launch = DashboardLaunchState::new(1);
    assert!(crate::commands::dashboard_launch::bind_supervised(
        &launch,
        Some(&owner),
        "worker",
        "s",
        None,
        None,
        None
    )
    .is_err());
    assert!(
        owner.get("s").is_none(),
        "a refused bind never arms supervision"
    );
    launch.begin("worker").unwrap();
    crate::commands::dashboard_launch::bind_supervised(
        &launch,
        Some(&owner),
        "worker",
        "s",
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(launch.resolve("worker").as_deref(), Some("s"));
    let turns = launch.turns();
    turns.record_hook("s", "UserPromptSubmit");
    turns.record_hook("s", "Stop");
    let observation = turns.observation("s").unwrap();
    let legacy = effective_status::LegacySignals {
        state: Some(observation.state.as_wire().into()),
        observed: true,
        ..Default::default()
    };
    owner.legacy_status("s", Some("done"));
    let deadline = owner
        .reconcile_launch("s", None, observation.generation, false)
        .unwrap();
    assert_eq!(
        deadline - tokio::time::Instant::now(),
        Duration::from_secs(45)
    );
    project_managed_outcome(&hub, &owner, None, &legacy);
    assert_eq!(
        hub.snapshot("all", "sessions").unwrap().snapshot[0]["effectiveStatus"],
        "idle"
    );
    hub.model.lock().unwrap().stall_deadline = Some(deadline);
    assert_eq!(hub.next_batch().await, REBUILD | HOSTS);
    assert!(owner
        .reconcile_launch("s", None, observation.generation, false)
        .is_none());
    let deltas = project_managed_outcome(&hub, &owner, None, &legacy);
    assert_eq!(deltas.len(), 1);
    assert_eq!(deltas[0].upserts[0]["effectiveStatus"], "dropped");
    assert_eq!(deltas[0].upserts[0]["managedState"], "needs-attention");
    assert!(hub.model.lock().unwrap().stall_deadline.is_none());
}

#[tokio::test]
async fn a_normal_managed_exit_projects_attention_without_overwriting_signal_done() {
    use super::*;
    use crate::mcp::session_status::SessionOutcomes;
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(LiveHub::default());
    app.manage(SessionOutcomes::default());
    listen_sources(app.handle());
    let owner = app.state::<SessionOutcomes>();
    owner.arm_launch("s", 0, None);
    owner.reconcile_launch("s", None, 0, false);
    app.emit(
        "pty-exit",
        json!({"session_id":"s","exit_code":0,"signal":null}),
    )
    .unwrap();
    let hub = app.state::<LiveHub>();
    let legacy = effective_status::LegacySignals {
        ended: true,
        ..Default::default()
    };
    project_managed_outcome(&hub, &owner, None, &legacy);
    assert_eq!(
        hub.snapshot("all", "sessions").unwrap().snapshot[0]["effectiveStatus"],
        "dropped"
    );
    app.emit("mcp:agent-done", json!({"sessionId":"s","status":"failed"}))
        .unwrap();
    app.emit("pty-exit", json!({"session_id":"s","exit_code":0}))
        .unwrap();
    assert_eq!(owner.get("s").unwrap().outcome, "failed");
    app.emit("pty-exit", json!({"session_id":"unmanaged","exit_code":0}))
        .unwrap();
    assert!(owner.get("unmanaged").is_none());
}

#[tokio::test(start_paused = true)]
async fn reading_a_restored_parked_launch_does_not_arm_a_new_task() {
    use crate::commands::dashboard_launch::DashboardLaunchState;
    use crate::mcp::session_status::SessionOutcomes;
    let owner = SessionOutcomes::default();
    let restored = DashboardLaunchState::new(1);
    restored.begin("parked").unwrap();
    restored.bind("parked", "restored", None).unwrap();
    assert!(owner.reconcile_launch("restored", None, 0, false).is_none());
    tokio::time::advance(std::time::Duration::from_secs(600)).await;
    assert!(owner.reconcile_launch("restored", None, 0, false).is_none());
    assert!(owner.get("restored").is_none());
}

#[tokio::test(start_paused = true)]
async fn shared_launch_event_sequences_match_the_frontend_producer_and_projected_status() {
    use super::*;
    use crate::mcp::{session_status::SessionOutcomes, state::AskUserRegistry};
    use codesense_pty_core::activity::Snapshot as ActivitySnapshot;
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("effective_status/parity.json")).unwrap();
    for case in fixture["supervision"].as_array().unwrap() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(LiveHub::default());
        app.manage(SessionOutcomes::default());
        app.manage(DashboardLaunchState::new(1));
        app.manage(AskUserRegistry::default());
        listen_sources(app.handle());
        let owner = app.state::<SessionOutcomes>();
        let launch = app.state::<DashboardLaunchState>();
        let hub = app.state::<LiveHub>();
        let asks = app.state::<AskUserRegistry>();
        let mut question = None;
        let mut snapshot: Option<ActivitySnapshot> = None;
        let start = tokio::time::Instant::now();
        let mut deadline = None;
        let mut revision = 0;
        let reconcile = |snapshot: Option<&ActivitySnapshot>| {
            owner.reconcile_asks(&asks.pending_sessions());
            if let Some(snapshot) = snapshot {
                owner.observe(snapshot);
            }
            owner.reconcile_launch(
                "s",
                snapshot,
                launch.turns().observation("s").map_or(0, |p| p.generation),
                asks.pending_sessions().contains("s"),
            )
        };
        for event in case["events"].as_array().unwrap() {
            let at = start + Duration::from_millis(event["at"].as_u64().unwrap());
            while let Some(next) = deadline.filter(|next| *next <= at) {
                tokio::time::advance(next - tokio::time::Instant::now()).await;
                deadline = reconcile(snapshot.as_ref());
            }
            tokio::time::advance(at - tokio::time::Instant::now()).await;
            match event["event"].as_str().unwrap() {
                "bind" => {
                    launch.begin("worker").unwrap();
                    crate::commands::dashboard_launch::bind_supervised(
                        &launch,
                        Some(&owner),
                        "worker",
                        "s",
                        None,
                        None,
                        None,
                    )
                    .unwrap();
                }
                "legacy" => {
                    if !snapshot
                        .as_ref()
                        .is_some_and(|s| s.observed || s.exit.is_some())
                    {
                        if let Some(hook) = event["hook"].as_str() {
                            launch.turns().record_hook("s", hook);
                        }
                    }
                    app.emit(crate::mcp::agent_dot::TERMINAL_STATUS_EVENT, json!({"tab_session_id":"s","status":event["status"],"hook_event":event["hook"]})).unwrap();
                }
                "snapshot" => {
                    revision += 1;
                    let mut value = json!({"session":"s","identity":{"hostBootId":"b","sessionIncarnation":"p","invocationId":"i"},"turnGeneration":1,"revision":revision,"provider":"claude","state":"idle","observed":false,"limited":false,"exit":null,"asks":[]});
                    value
                        .as_object_mut()
                        .unwrap()
                        .extend(event["snapshot"].as_object().unwrap().clone());
                    snapshot = Some(serde_json::from_value(value).unwrap());
                    let reply = crate::commands::machines::RuntimeLayoutReply::Live {
                        private_authority_epoch: None,
                        target: serde_json::from_value(json!({"member":{"machineId":"local","slot":"sidecar"},"admissionRevision":"1","namespace":{"serverId":"server","epoch":"epoch","generation":"1"}})).unwrap(),
                        snapshot: Box::new(codesense_pty_core::layout::LayoutSnapshot {
                            schema: 1, server_id: codesense_pty_core::domain::ServerId::new("server"), server_name: "server".into(), epoch:"epoch".into(), rev:1, workspaces:Default::default(), viewer:Default::default(),
                        }),
                    };
                    hub.model
                        .lock()
                        .unwrap()
                        .hosts
                        .insert("host".into(), (reply, Ok(vec![snapshot.clone().unwrap()])));
                }
                "message" => {
                    app.emit(
                        "mcp:agent-message",
                        json!({"sessionId":"s","text":"working"}),
                    )
                    .unwrap();
                }
                "done" => {
                    app.emit(
                        "mcp:agent-done",
                        json!({"sessionId":"s","status":event["status"]}),
                    )
                    .unwrap();
                }
                "ask-open" => {
                    let (id, _reply) = asks.register("s");
                    question = Some(id);
                    app.emit(
                        "mcp:ask-user",
                        json!({"sessionId":"s","correlationId":question}),
                    )
                    .unwrap();
                }
                "ask-close" => {
                    asks.take(question.as_ref().unwrap());
                }
                "pty-exit" => {
                    app.emit(
                        "pty-exit",
                        json!({"session_id":"s","exit_code":0,"signal":null}),
                    )
                    .unwrap();
                }
                "check" => {
                    let outcome = owner.get("s").unwrap();
                    assert_eq!(
                        outcome.outcome,
                        event["outcome"].as_str().unwrap(),
                        "{} at {}",
                        case["name"],
                        event["at"]
                    );
                    let observation = launch.turns().observation("s");
                    let authority = snapshot.as_ref().filter(|s| s.observed || s.exit.is_some());
                    let legacy = effective_status::LegacySignals {
                        state: authority
                            .map(|s| {
                                serde_json::to_value(s.state)
                                    .unwrap()
                                    .as_str()
                                    .unwrap()
                                    .to_owned()
                            })
                            .or_else(|| observation.map(|p| p.state.as_wire().into())),
                        observed: authority.is_some_and(|s| s.observed) || observation.is_some(),
                        ended: authority.is_some_and(|s| s.exit.is_some()),
                        new_turn: outcome.new_turn_seen
                            || observation
                                .is_some_and(|p| p.generation > outcome.legacy_generation),
                        ..Default::default()
                    };
                    project_managed_outcome(&hub, &owner, snapshot.as_ref(), &legacy);
                    assert_eq!(
                        hub.snapshot("all", "sessions").unwrap().snapshot[0]["effectiveStatus"],
                        event["status"],
                        "{} at {}",
                        case["name"],
                        event["at"]
                    );
                }
                _ => unreachable!(),
            }
            deadline = reconcile(snapshot.as_ref());
        }
    }
}
#[test]
fn machine_stats_stream_a_snapshot_then_deltas_to_subscription_owners() {
    use super::projection::TopicRegistry;
    let mut topics = TopicRegistry::default();
    assert!(!topics.has_subscribers("machineStats"));
    topics
        .subscribe_window("frame:a", "machineStats", "all", None, None)
        .unwrap();
    topics.set_owner("frame:a", "window-a");
    topics
        .subscribe_window("frame:b", "machineStats", "all", None, None)
        .unwrap();
    topics.set_owner("frame:b", "window-b");
    assert!(topics.has_subscribers("machineStats"));
    let deltas = topics
        .replace(
            "machineStats",
            vec![(None, serde_json::json!({"id":"local", "load1":3.04}))],
            None,
        )
        .unwrap();
    let delivered = topics.deliver(&deltas);
    assert_eq!(delivered.len(), 2);
    assert_eq!(delivered[0].1.upserts[0]["load1"], 3.04);
    let snapshot = topics.snapshot("machineStats", "all").unwrap();
    assert_eq!(snapshot.snapshot[0]["id"], "local");
    topics.drop_owner("window-a");
    assert!(topics.has_subscribers("machineStats"));
    topics.unsubscribe("frame:b");
    assert!(!topics.has_subscribers("machineStats"));
}

fn startup_snapshot(observed: bool) -> codesense_pty_core::activity::Snapshot {
    serde_json::from_value(json!({
        "session":"s","identity":{"hostBootId":"b","sessionIncarnation":"p","invocationId":"i"},
        "revision":1,"turnGeneration":0,"provider":"codex","state":"idle","observed":observed,
        "limited":false,"exit":null,"asks":[]
    }))
    .unwrap()
}

fn project_startup(
    hub: &super::LiveHub,
    owner: &crate::mcp::session_status::SessionOutcomes,
    launch: &crate::commands::dashboard_launch::DashboardLaunchState,
    snapshot: &codesense_pty_core::activity::Snapshot,
    elapsed_ms: u64,
) -> serde_json::Value {
    use super::effective_status::*;
    let (bound, ready) = launch.readiness("s").unwrap();
    let stuck = stuck_on_startup(Some(snapshot), ready, bound, bound.unwrap() + elapsed_ms);
    let outcome = owner.get("s").unwrap();
    let signals = select_live_signals(
        Some(snapshot),
        &LegacySignals {
            observed: snapshot.observed,
            ..Default::default()
        },
        None,
        None,
        Some(&outcome.outcome),
        false,
        false,
        None,
        None,
        outcome.completion.as_ref(),
        None,
        stuck,
        0,
    );
    let runtime = json!({"id":"s","sessionId":"s","handle":"worker","alive":true,"status":"running",
        "_managedOutcome":outcome.outcome,"_statusSignals":signals});
    let wire = super::agent_list::sessions::project(&[], &[runtime], &mut [], &hub.providers);
    hub.model
        .lock()
        .unwrap()
        .projections
        .replace("sessions", wire, None)
        .unwrap();
    hub.snapshot("all", "sessions").unwrap().snapshot[0].clone()
}

#[tokio::test(start_paused = true)]
async fn a_never_ready_session_reads_startup_not_dropped_and_clears_once_observed() {
    use super::effective_status::UNREADY_BOUND_MS;
    use crate::commands::dashboard_launch::{bind_supervised, DashboardLaunchState};
    use crate::mcp::session_status::SessionOutcomes;
    let hub = super::LiveHub::default();
    let owner = SessionOutcomes::default();
    let launch = DashboardLaunchState::new(1);
    launch.begin("worker").unwrap();
    bind_supervised(&launch, Some(&owner), "worker", "s", None, None, None).unwrap();
    let waiting = startup_snapshot(false);

    // Inside the bound the session is unknown, not marked.
    let early = project_startup(&hub, &owner, &launch, &waiting, UNREADY_BOUND_MS - 1);
    assert_eq!(early["needsYouKind"], serde_json::Value::Null);
    assert_eq!(early["effectiveStatus"], "unknown");

    // The launch net re-arms past its liveness window without dropping it.
    for _ in 0..3 {
        tokio::time::advance(std::time::Duration::from_secs(601)).await;
        assert!(owner
            .reconcile_launch("s", Some(&waiting), 0, false)
            .is_some());
        assert_eq!(owner.get("s").unwrap().outcome, "running");
    }

    // At the bound the mark appears, as needs-you and not as a managed drop.
    let stuck = project_startup(&hub, &owner, &launch, &waiting, UNREADY_BOUND_MS);
    assert_eq!(stuck["needsYouKind"], "startup");
    assert_eq!(stuck["effectiveStatus"], "dropped");
    assert_eq!(stuck["managedState"], "running");

    // Observed: the registry stamps readiness and the mark clears.
    let ready = startup_snapshot(true);
    launch.mark_ready("s");
    let cleared = project_startup(&hub, &owner, &launch, &ready, UNREADY_BOUND_MS * 2);
    assert_eq!(cleared["needsYouKind"], serde_json::Value::Null);
    assert_ne!(cleared["effectiveStatus"], "dropped");
    assert!(launch.readiness("s").unwrap().1.is_some());
    // The first stamp stays.
    let first = launch.readiness("s").unwrap().1;
    launch.mark_ready("s");
    assert_eq!(launch.readiness("s").unwrap().1, first);
}

#[tokio::test(start_paused = true)]
async fn a_supervised_session_that_was_ready_keeps_the_launch_net() {
    use crate::commands::dashboard_launch::{bind_supervised, DashboardLaunchState};
    use crate::mcp::session_status::SessionOutcomes;
    let owner = SessionOutcomes::default();
    let launch = DashboardLaunchState::new(1);
    launch.begin("worker").unwrap();
    bind_supervised(&launch, Some(&owner), "worker", "s", None, None, None).unwrap();
    let ready = startup_snapshot(true);
    assert!(owner
        .reconcile_launch("s", Some(&ready), 0, false)
        .is_some());
    tokio::time::advance(std::time::Duration::from_secs(601)).await;
    assert!(owner
        .reconcile_launch("s", Some(&ready), 0, false)
        .is_none());
    assert_eq!(owner.get("s").unwrap().outcome, "needs-attention");
}

#[tokio::test(start_paused = true)]
async fn a_remote_native_readiness_session_is_marked_past_the_bound_clears_when_ready_and_is_not_marked_offline(
) {
    use super::effective_status::{stuck_on_startup, UNREADY_BOUND_MS};
    use crate::commands::dashboard_launch::DashboardLaunchState;
    let record: crate::commands::pty_layout::RemoteShellRecord = serde_json::from_value(json!({
        "handle": "remote-worker", "owner_id": 41,
        "request_id": "00000000-0000-4000-8000-000000000001",
        "machine_id": "droplet", "team_key": "acme:design", "workspace_id": "w",
        "workspace": "acme:design|w", "cwd": "/home/dev", "name": null, "launch": "codex",
        "target": {"member":{"machineId":"droplet","slot":"remote"},"admissionRevision":"1",
            "namespace":{"serverId":"s","epoch":"e","generation":"1"}, "session": "s"}
    }))
    .unwrap();
    let launch = DashboardLaunchState::new(1);
    launch.reserve_remote_shell(record).unwrap();
    let waiting = startup_snapshot(false);
    // Past the bound, with the machine's observation showing it unready: marked.
    let (clock, ready) = launch.readiness("s").unwrap();
    let past = clock.unwrap() + UNREADY_BOUND_MS;
    assert!(stuck_on_startup(Some(&waiting), ready, clock, past));
    // A disconnected machine reports no observation: never marked, however late.
    assert!(!stuck_on_startup(None, ready, clock, past * 10));
    // Observed: the stamp lands and the mark clears.
    launch.mark_ready("s");
    let (clock, ready) = launch.readiness("s").unwrap();
    assert!(ready.is_some());
    assert!(!stuck_on_startup(Some(&waiting), ready, clock, past));
}

#[tokio::test]
async fn a_committed_pass_reports_the_message_ids_it_read_after_the_cache_holds_them() {
    use super::*;
    use crate::weft::{inbox as ledger, reactions, WeftStore};
    use tauri::Emitter;
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(LiveHub::default());
    listen_sources(app.handle());
    let host = FootprintHost {
        dir: tempfile::tempdir().unwrap(),
        teams: Mutex::new(vec![FootprintHost::team("active")]),
        discovery_failed: Default::default(),
        store_failed: Default::default(),
        committed: Default::default(),
        presentation_gate: Default::default(),
    };
    let team = FootprintHost::team("active");
    let store = WeftStore::open(host.path("active")).unwrap();
    let id = ledger::inbox_insert(
        &store, "worker", "human", "old mail", None, None, None, None, None,
    )
    .unwrap();
    refresh_inbox_with_host(app.handle(), &host, false)
        .await
        .unwrap();
    host.committed.lock().unwrap().clear();

    // The reaction is written, and the domain event arrives before the pass commits.
    reactions::reaction_react(&store, id, "worker", "🎉", None).unwrap();
    app.emit(
        inbox_events::INBOX_EVENT,
        inbox_events::InboxEvent::new(inbox_events::InboxChange::Reacted, &team, [id]),
    )
    .unwrap();
    tokio::task::yield_now().await;
    assert!(
        host.committed.lock().unwrap().is_empty(),
        "nothing is reported before the cache commits"
    );
    refresh_inbox_with_host(app.handle(), &host, false)
        .await
        .unwrap();
    let reports = host.committed.lock().unwrap().clone();
    assert_eq!(reports, vec![vec![(team.clone(), vec![id])]]);
    let hub = app.state::<LiveHub>();
    let cached = hub.inbox_view(&team).unwrap();
    assert_eq!(
        cached
            .messages
            .iter()
            .find(|m| m.message_id == id)
            .unwrap()
            .reactions[0]
            .emoji,
        "🎉",
        "the cache already holds the reaction the report names"
    );
}

#[tokio::test]
async fn hub_readers_proceed_while_inbox_prep_runs_off_lock() {
    use super::*;
    use crate::weft::{inbox as ledger, WeftStore};
    use std::sync::mpsc;
    use std::time::Duration;
    use tauri::{Emitter, Manager};

    const BOUND: Duration = Duration::from_secs(5);

    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(LiveHub::default());
    listen_sources(app.handle());
    let host = FootprintHost {
        dir: tempfile::tempdir().unwrap(),
        teams: Mutex::new(vec![FootprintHost::team("active")]),
        discovery_failed: Default::default(),
        store_failed: Default::default(),
        committed: Default::default(),
        presentation_gate: Default::default(),
    };
    let team = FootprintHost::team("active");

    // Populate the old published snapshot with one message through the real seam.
    let store = WeftStore::open(host.path("active")).unwrap();
    let old_id = ledger::inbox_insert(
        &store, "worker", "human", "old", None, None, None, None, None,
    )
    .unwrap();
    refresh_inbox_with_host(app.handle(), &host, false)
        .await
        .unwrap();

    let hub = app.state::<LiveHub>();
    let old_arc = {
        let model = hub.model.lock().unwrap();
        std::sync::Arc::clone(&model.inbox)
    };
    let old_arc_for_during = std::sync::Arc::clone(&old_arc);
    let old_pair = (old_id, "old".to_string());

    // Queue a real incoming update through the production event/pending seam.
    let new_id = ledger::inbox_insert(
        &store, "worker", "human", "new", None, None, None, None, None,
    )
    .unwrap();
    app.emit(
        inbox_events::INBOX_EVENT,
        inbox_events::InboxEvent::new(inbox_events::InboxChange::Stored, &team, [new_id]),
    )
    .unwrap();
    tokio::task::yield_now().await;

    let (entered_tx, entered) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    *host.presentation_gate.lock().unwrap() = Some(Box::new(move || {
        let _ = entered_tx.send(());
        let _ = release_rx.recv();
    }));

    let app_handle = app.handle().clone();
    let team_for_reader = team.clone();
    let (reader_done_tx, reader_done_rx) = mpsc::channel();
    let (entry_res, reader_res, refresh_join, reader_join, during_pair) = std::thread::scope(|s| {
        let refresh = s.spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(refresh_inbox_with_host(&app_handle, &host, false))
                .unwrap();
        });
        let entry_res = entered.recv_timeout(BOUND);

        let during_pair = old_arc_for_during
            .view_parts(&team)
            .build()
            .messages
            .iter()
            .next()
            .map(|m| (m.message_id, m.text.clone()));

        let app_handle2 = app.handle().clone();
        let reader = s.spawn(move || {
            let hub = app_handle2.state::<LiveHub>();
            let view = hub.inbox_view(&team_for_reader).unwrap();
            let pair = view
                .messages
                .iter()
                .next()
                .map(|m| (m.message_id, m.text.clone()));
            let _ = reader_done_tx.send(pair);
        });
        let reader_res = reader_done_rx.recv_timeout(BOUND);

        // Release even on timeout so scoped workers always finish.
        drop(release_tx);
        let refresh_join = refresh.join();
        let reader_join = reader.join();
        (
            entry_res,
            reader_res,
            refresh_join,
            reader_join,
            during_pair,
        )
    });
    entry_res.expect("refresh did not enter batch presentation");
    let reader_pair = reader_res.expect("hub reader timed out during inbox batch presentation");
    refresh_join.expect("refresh thread panicked");
    reader_join.expect("reader thread panicked");

    assert_eq!(during_pair, Some(old_pair.clone()));
    assert_eq!(reader_pair, Some(old_pair.clone()));

    // The retained old Arc must still see the pre-update snapshot after publication.
    let retained_pair = old_arc
        .view_parts(&team)
        .build()
        .messages
        .iter()
        .next()
        .map(|m| (m.message_id, m.text.clone()));
    assert_eq!(retained_pair, Some(old_pair));

    // After publication the new snapshot must contain both messages.
    let new_view = hub.inbox_view(&team).unwrap();
    assert_eq!(new_view.messages.len(), 2, "new snapshot has both messages");
    assert!(new_view
        .messages
        .iter()
        .any(|m| m.message_id == old_id && m.text == "old"));
    assert!(new_view
        .messages
        .iter()
        .any(|m| m.message_id == new_id && m.text == "new"));
}
