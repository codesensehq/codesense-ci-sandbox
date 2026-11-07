// SPDX-License-Identifier: MIT
use crate::context_repo::caller_scope::tests::Fixture;
use crate::domain::Principal;
use crate::mcp::routine_tools::{self, RoutineGetParams, ToolEnvelope};

#[test]
fn child_routine_lists_hide_parent_definitions_without_changing_human_management() {
    let f = Fixture::new();
    let parent = f.routine("parent", "parent-job");
    let child = f.routine("child", "child-job");
    let sibling = f.routine("sibling", "sibling-job");
    let principal = Principal::from(1u64);
    let now = chrono::Utc::now();
    let ToolEnvelope::Ok(rows) =
        routine_tools::list_for(&f.home, &principal, Default::default(), now, &f.scope).unwrap()
    else {
        panic!("list refused")
    };
    assert!(!rows.iter().any(|r| r.id == parent));
    assert!(rows.iter().any(|r| r.id == child));
    assert!(rows.iter().any(|r| r.id == sibling));
    let get = |id| {
        routine_tools::get_for(
            &f.home,
            &principal,
            RoutineGetParams { id, agent: None },
            now,
            &f.scope,
        )
        .unwrap()
    };
    assert_eq!(
        serde_json::to_value(get(parent.clone())).unwrap(),
        serde_json::to_value(get("acme/ctx/parent/missing".into())).unwrap()
    );
    assert!(matches!(get(child), ToolEnvelope::Ok(_)));
    let ToolEnvelope::Ok(human) =
        routine_tools::list(&f.home, &principal, Default::default(), now).unwrap()
    else {
        panic!("human list refused")
    };
    assert!(human.iter().any(|r| r.id == parent));
    let shared = f.path("parent", "content/shared.md");
    assert!(f.scope.existing(&shared).is_ok());
    assert_eq!(
        std::fs::read_to_string(shared).unwrap(),
        "parent-shared.md-bytes"
    );
}

#[test]
fn run_detail_and_history_hide_parent_definitions_but_keep_other_readers() {
    use crate::context_repo::caller_scope::tests::AppFixture;
    use tauri::Manager;
    let f = AppFixture::new();
    f.session("child-session", Some("child"));
    let me = Principal::from(1u64);
    let store = crate::routines::state::RoutineStateStore::open(&f.fixture.home).unwrap();
    let db = rusqlite::Connection::open(store.path()).unwrap();
    for team in ["parent", "child", "sibling"] {
        let key = f.fixture.routine(team, "history-job");
        db.execute("INSERT INTO routine_runs (run_id,routine_key,revision,source,phase,created_at) VALUES (?1,?2,1,'manual','succeeded',0)", rusqlite::params![format!("{team}-run"), key]).unwrap();
    }
    let frame = f.frame(&f.fixture.path("parent", "panes/parent.html"), "workspace");
    let scope = tauri::async_runtime::block_on(crate::context_repo::caller_scope::for_session(
        f.app.handle(),
        Some("child-session"),
        None,
    ));
    let read = |id: &str| {
        routine_tools::run_for(
            &f.fixture.home,
            &me,
            routine_tools::RoutineRunParams { run_id: id.into() },
            &scope,
        )
        .unwrap()
    };
    assert_eq!(
        serde_json::to_value(read("parent-run")).unwrap(),
        serde_json::to_value(read("missing-run")).unwrap()
    );
    assert!(matches!(read("child-run"), ToolEnvelope::Ok(_)));
    assert!(matches!(read("sibling-run"), ToolEnvelope::Ok(_)));
    let list = |id: &str| {
        routine_tools::runs_for(
            &f.fixture.home,
            &me,
            routine_tools::RoutineRunsParams {
                id: id.into(),
                limit: None,
                before: None,
            },
            &scope,
        )
        .unwrap()
    };
    assert_eq!(
        serde_json::to_value(list("acme/ctx/parent/history-job")).unwrap(),
        serde_json::to_value(list("acme/ctx/parent/missing")).unwrap()
    );
    let page_read = |id: &str| {
        tauri::async_runtime::block_on(crate::commands::data_read_model::routine_read_run(
            f.app.handle().clone(),
            id.into(),
            Some(frame.frame_id.clone()),
            Some(frame.document_path.clone()),
        ))
    };
    assert_eq!(
        page_read("parent-run").unwrap_err(),
        page_read("missing-run").unwrap_err()
    );
    assert!(page_read("child-run").is_ok());
    assert!(
        tauri::async_runtime::block_on(crate::commands::data_read_model::routine_read_run(
            f.app.handle().clone(),
            "parent-run".into(),
            None,
            None
        ))
        .is_ok()
    );
    f.app
        .state::<crate::mcp::state::ActiveTeamState>()
        .update(crate::mcp::state::ActiveTeam {
            org_slug: "acme/ctx".into(),
            team_name: "parent".into(),
        });
    assert!(page_read("parent-run").is_err());
}

#[test]
fn result_definition_and_mutation_readbacks_keep_page_scope() {
    use crate::context_repo::caller_scope::tests::AppFixture;
    let f = AppFixture::new();
    let me = Principal::from(1u64);
    let key = f.fixture.routine("parent", "result");
    let parsed = crate::routines::RoutineKey::parse(&key).unwrap();
    let missing = crate::routines::RoutineKey::parse("acme/ctx/parent/missing").unwrap();
    assert_eq!(
        crate::routines::results::read_for(&f.fixture.home, &me, &parsed, &f.fixture.scope)
            .unwrap_err(),
        crate::routines::results::read_for(&f.fixture.home, &me, &missing, &f.fixture.scope)
            .unwrap_err()
    );
    let shared = f.fixture.routine("child", "result");
    assert!(crate::routines::results::read_for(
        &f.fixture.home,
        &me,
        &crate::routines::RoutineKey::parse(&shared).unwrap(),
        &f.fixture.scope
    )
    .is_ok());
    let frame = f.frame(&f.fixture.path("parent", "panes/parent.html"), "workspace");
    assert_eq!(
        tauri::async_runtime::block_on(crate::commands::routines::routine_result(
            f.app.handle().clone(),
            key.clone(),
            Some(frame.frame_id.clone()),
            Some(frame.document_path.clone())
        ))
        .unwrap_err(),
        tauri::async_runtime::block_on(crate::commands::routines::routine_result(
            f.app.handle().clone(),
            missing.to_string(),
            Some(frame.frame_id),
            Some(frame.document_path)
        ))
        .unwrap_err()
    );
    let update = |id: &str| {
        let result = routine_tools::update(
            &f.fixture.home,
            &me,
            &crate::routines::Author::user("1"),
            serde_json::from_value(serde_json::json!({"id":id,"baseRevision":1,"name":"Renamed"}))
                .unwrap(),
            chrono::Utc::now(),
            &|_| None,
        );
        routine_tools::readback_for(result, &f.fixture.home, &me, id, &f.fixture.scope).unwrap()
    };
    assert_eq!(
        serde_json::to_value(update(&key)).unwrap(),
        serde_json::to_value(update(&missing.to_string())).unwrap()
    );
    assert_eq!(
        crate::routines::service::get_current(&f.fixture.home, &me, &parsed)
            .unwrap()
            .def
            .name,
        "Renamed"
    );
}

#[test]
fn private_routine_secondary_agent_probe_cannot_distinguish_hidden_from_missing() {
    let f = Fixture::new();
    let me = Principal::from(1u64);
    let agent = f.agent("parent", "hidden-agent");
    let private = crate::context_repo::scratch_space::ensure_org_space(&f.home, &me, "acme/ctx")
        .unwrap()
        .0;
    let key = crate::routines::RoutineKey::scoped(
        &me,
        "acme/ctx",
        crate::context_repo::scratch_space::SCRATCH_TEAM,
        "personal-probe",
    );
    let reference = crate::routines::AgentRef::new(agent.to_string());
    let dir = crate::routines::layout::routine_dir_in(
        &private.config_dir(),
        &crate::routines::layout::home_for_ref(&key, &reference),
        "personal-probe",
    );
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("routine.yml"),
        format!(
            "id: personal-probe
name: Personal probe
agent: {agent}
task: Check
where: {{kind: team_workspace}}
revision: 1
"
        ),
    )
    .unwrap();
    let read = || {
        serde_json::to_value(
            routine_tools::get_for(
                &f.home,
                &me,
                RoutineGetParams {
                    id: key.to_string(),
                    agent: None,
                },
                chrono::Utc::now(),
                &f.scope,
            )
            .unwrap(),
        )
        .unwrap()
    };
    let denied = read();
    assert_eq!(denied["status"], "ok");
    assert_eq!(denied["data"]["unassigned"], true);
    std::fs::remove_file(f.path("parent", "config/agents/hidden-agent/agent.yaml")).unwrap();
    assert_eq!(denied, read());
}

#[test]
fn routine_advisory_manifest_intersects_permitted_uncle_with_child_source_admission() {
    let f = Fixture::new();
    let me = Principal::from(1u64);
    let mut teams = std::fs::read_to_string(f.root.join("teams.yml")).unwrap();
    teams.push_str("  - name: uncle\n    parent_team: grand\n");
    std::fs::write(f.root.join("teams.yml"), teams).unwrap();
    f.write(
        "grand",
        "config/sharing.yml",
        "share: [content/.claude/commands/grand-only.md]\n",
    );
    let hidden = f.write(
        "grand",
        "content/.claude/commands/grand-only.md",
        "# Grand command",
    );
    f.write("uncle", "content/.claude/commands/own.md", "# Own command");
    let key = f.agent("uncle", "reader");
    let mut agent = crate::agents::service::read_current(&f.home, &me, &key).unwrap();
    let revision = agent.revision;
    agent.startup = vec![
        ".claude/commands/grand-only.md".into(),
        ".claude/commands/own.md".into(),
    ];
    crate::agents::service::save(
        &f.home,
        &me,
        crate::agents::SaveTarget::Update {
            key: key.clone(),
            base_revision: revision,
        },
        agent,
        &crate::routines::Author::user("1"),
    )
    .unwrap();
    let preview = |task: &str| {
        let reply = routine_tools::create_for(&f.home, &me,
            crate::routines::service::Placement::Team { org:"acme/ctx".into(), team:"uncle".into() },
            &routine_tools::CallerSession::default(), &crate::routines::Author::user("1"),
            serde_json::from_value(serde_json::json!({"name":"Manifest preview", "task":task, "agent":key.to_string(), "when":"manual", "enabled":false, "dryRun":true})).unwrap(),
            chrono::Utc::now(), &f.scope).unwrap();
        let ToolEnvelope::Ok(data) = reply else {
            panic!("preview refused");
        };
        data.warnings
    };
    let denied = serde_json::to_value(preview("/grand-only")).unwrap();
    std::fs::remove_file(hidden).unwrap();
    assert_eq!(
        denied,
        serde_json::to_value(preview("/grand-only")).unwrap()
    );
    assert!(!preview("/own").iter().any(|warning| warning.code
        == crate::agents::launch_check::LaunchWarningCode::CommandNotInManifest));
    assert!(f
        .scope
        .existing(&f.path("parent", "content/shared.md"))
        .is_ok());
}
