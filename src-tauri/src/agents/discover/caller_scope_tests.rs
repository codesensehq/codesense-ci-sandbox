// SPDX-License-Identifier: MIT
use crate::context_repo::caller_scope::tests::Fixture;
use crate::domain::Principal;
use crate::mcp::agent_tools::{self, AgentGetParams, AgentReply};

#[test]
fn child_agent_get_and_list_hide_parent_definitions_and_keep_shared_startup() {
    let f = Fixture::new();
    let principal = Principal::from(1u64);
    let parent = f.agent("parent", "hidden-parent");
    let child = f.agent("child", "reader");
    let sibling = f.agent("sibling", "sibling-reader");
    std::fs::remove_file(f.path("child", "content/shared.md")).unwrap();
    let mut definition = crate::agents::service::read_current(&f.home, &principal, &child).unwrap();
    let revision = definition.revision;
    definition.startup = vec!["shared.md".into()];
    crate::agents::service::save(
        &f.home,
        &principal,
        crate::agents::SaveTarget::Update {
            key: child.clone(),
            base_revision: revision,
        },
        definition,
        &crate::routines::Author::user("1"),
    )
    .unwrap();
    let get = |key: &str| {
        agent_tools::get_for(
            &f.home,
            &principal,
            AgentGetParams { id: key.into() },
            &f.scope,
        )
        .unwrap()
    };
    assert_eq!(
        serde_json::to_value(get(&parent.to_string())).unwrap(),
        serde_json::to_value(get("acme/ctx/parent/missing")).unwrap()
    );
    let AgentReply::Ok(got) = get(&child.to_string()) else {
        panic!("own agent missing")
    };
    let manifest = serde_json::to_string(&got.resolved.manifest).unwrap();
    assert!(manifest.contains("shared.md"));
    assert!(manifest.contains("parent"));
    assert!(!manifest.contains("secret.md"));
    let AgentReply::Ok(rows) = agent_tools::list_for(
        &f.home,
        &principal,
        serde_json::from_value(serde_json::json!({})).unwrap(),
        &f.scope,
    )
    .unwrap() else {
        panic!("list refused")
    };
    assert!(rows.iter().any(|a| a.id == child.to_string()));
    assert!(rows.iter().any(|a| a.id == sibling.to_string()));
    assert!(!rows.iter().any(|a| a.id == parent.to_string()));
    let AgentReply::Ok(human) = agent_tools::list(
        &f.home,
        &principal,
        serde_json::from_value(serde_json::json!({})).unwrap(),
    )
    .unwrap() else {
        panic!("human list refused")
    };
    assert!(human.iter().any(|a| a.id == parent.to_string()));
}

#[test]
fn builtin_agent_get_filters_nested_routines_before_summarizing() {
    let f = Fixture::new();
    let me = Principal::from(1u64);
    for team in ["parent", "child", "sibling"] {
        f.write(
            team,
            "config/agents/_builtin/claude/routines/job/routine.yml",
            &format!("id: job\nname: {team}-routine-sentinel\ntask: Read shared.md\nwhere: {{kind: team_workspace}}\nrevision: 1\n"),
        );
    }
    let private = crate::context_repo::scratch_space::ensure_org_space(&f.home, &me, "acme/ctx")
        .unwrap()
        .0;
    let routine = private
        .config_dir()
        .join("agents/_builtin/claude/routines/personal-job");
    std::fs::create_dir_all(&routine).unwrap();
    std::fs::write(routine.join("routine.yml"), "id: personal-job\nname: private-routine-positive\ntask: Read shared.md\nwhere: {kind: team_workspace}\nrevision: 1\n").unwrap();
    let read = |scope| {
        agent_tools::get_for(
            &f.home,
            &me,
            AgentGetParams {
                id: "builtin/claude".into(),
            },
            scope,
        )
        .unwrap()
    };
    let AgentReply::Ok(got) = read(&f.scope) else {
        panic!("builtin missing")
    };
    let rows = serde_json::to_string(&got.routines).unwrap();
    assert!(!rows.contains("parent-routine-sentinel"));
    assert!(rows.contains("child-routine-sentinel"));
    assert!(rows.contains("sibling-routine-sentinel"));
    assert!(rows.contains("private-routine-positive"));
    let AgentReply::Ok(human) = read(&crate::context_repo::caller_scope::CallerScope::Human) else {
        panic!("human builtin missing")
    };
    assert!(serde_json::to_string(&human.routines)
        .unwrap()
        .contains("parent-routine-sentinel"));
    std::fs::remove_dir_all(f.path("parent", "config/agents/_builtin/claude/routines/job"))
        .unwrap();
    let AgentReply::Ok(absent) = read(&f.scope) else {
        panic!("builtin missing")
    };
    assert_eq!(
        serde_json::to_value(got.routines).unwrap(),
        serde_json::to_value(absent.routines).unwrap()
    );
}

#[test]
fn mcp_preview_request_gates_saved_and_draft_ancestor_targets() {
    use crate::context_repo::caller_scope::tests::AppFixture;
    let f = AppFixture::new();
    f.session("child-session", Some("child"));
    let me = Principal::from(1u64);
    let parent = f.fixture.agent("parent", "hidden-preview");
    std::fs::remove_file(f.fixture.path("child", "content/shared.md")).unwrap();
    let read = |value: serde_json::Value, team: &str| {
        let caller = crate::commands::routines::Caller {
            home: f.fixture.home.clone(),
            principal: me.clone(),
        };
        let agent = agent_tools::AgentCaller::new(&me, None);
        let placement = crate::agents::Placement::Team {
            org: "acme/ctx".into(),
            team: team.into(),
        };
        serde_json::to_value(
            tauri::async_runtime::block_on(agent_tools::preview_request(
                f.app.handle(),
                Some("child-session"),
                None,
                caller,
                agent,
                placement,
                serde_json::from_value(value).unwrap(),
            ))
            .unwrap(),
        )
        .unwrap()
    };
    assert_eq!(
        read(serde_json::json!({"agent":parent.to_string()}), "child"),
        read(
            serde_json::json!({"agent":"acme/ctx/parent/missing"}),
            "child"
        )
    );
    let draft = serde_json::json!({"name":"Draft", "runtime":"claude", "startup":["shared.md"]});
    let denied = read(draft.clone(), "parent");
    assert_eq!(denied["ok"], false);
    assert_eq!(
        denied,
        read(
            serde_json::json!({"name":"Draft", "runtime":"claude", "startup":["missing.md"]}),
            "parent"
        )
    );
    let positive = read(draft.clone(), "child");
    assert_eq!(positive["ok"], true, "{positive}");
    assert!(positive.to_string().contains("shared.md"));
    assert!(!positive.to_string().contains("secret.md"));
    assert_eq!(read(draft.clone(), "sibling")["ok"], true);
    f.fixture
        .write("parent", "config/sharing.yml", "share:\n  - content/**\n");
    assert_eq!(read(draft, "parent")["ok"], false);
}

#[test]
fn create_dry_run_reuses_preview_admission_without_restricting_writes() {
    let f = Fixture::new();
    let me = Principal::from(1u64);
    let agent = agent_tools::AgentCaller::new(&me, None);
    let read = |team: &str,
                startup: &str,
                scope: &crate::context_repo::caller_scope::CallerScope| {
        agent_tools::create_for(&f.home, &me, crate::agents::Placement::Team { org: "acme/ctx".into(), team: team.into() }, &agent,
            serde_json::from_value(serde_json::json!({"name":"Dry run", "runtime":"claude", "dryRun":true, "startup":[startup]})).unwrap(), scope).unwrap()
    };
    assert_eq!(
        serde_json::to_value(read("parent", "secret.md", &f.scope)).unwrap(),
        serde_json::to_value(read("parent", "missing.md", &f.scope)).unwrap()
    );
    assert!(matches!(
        read("child", "shared.md", &f.scope),
        AgentReply::Ok(_)
    ));
    assert!(matches!(
        read("sibling", "secret.md", &f.scope),
        AgentReply::Ok(_)
    ));
    assert!(matches!(
        read(
            "parent",
            "secret.md",
            &crate::context_repo::caller_scope::CallerScope::Human
        ),
        AgentReply::Ok(_)
    ));
}

#[test]
fn allowed_uncle_agent_sources_intersect_the_childs_ancestor_admission() {
    use crate::commands::dashboard_launch::LaunchTeam;
    use crate::context_repo::caller_scope::CallerScope;
    let f = Fixture::new();
    let me = Principal::from(1u64);
    let mut teams = std::fs::read_to_string(f.root.join("teams.yml")).unwrap();
    teams.push_str("  - name: uncle\n    parent_team: grand\n");
    std::fs::write(f.root.join("teams.yml"), teams).unwrap();
    f.write(
        "grand",
        "config/sharing.yml",
        "share:\n  - content/grand-only.md\n  - content/.mcp.json\n",
    );
    let source = f.write("grand", "content/grand-only.md", "GRAND-SOURCE-SENTINEL");
    f.write("uncle", "content/uncle.md", "UNCLE-OWN-POSITIVE");
    f.write("grand", "config/projections.yml", "projections:\n  - {key: hidden-grand, source_subdir: grand-only.md, dest: grand-secret-dest}\n");
    f.write(
        "uncle",
        "config/projections.yml",
        "projections:\n  - {key: own, source_subdir: uncle.md, dest: uncle-positive-dest}\n",
    );
    let key = f.agent("uncle", "reader");
    let mut def = crate::agents::service::read_current(&f.home, &me, &key).unwrap();
    let revision = def.revision;
    def.startup = vec!["grand-only.md".into(), "uncle.md".into()];
    crate::agents::service::save(
        &f.home,
        &me,
        crate::agents::SaveTarget::Update {
            key: key.clone(),
            base_revision: revision,
        },
        def,
        &crate::routines::Author::user("1"),
    )
    .unwrap();
    let scope = || {
        CallerScope::team(
            &f.home,
            &me,
            &LaunchTeam {
                org: "acme/ctx".into(),
                team: "child".into(),
            },
        )
    };
    let actor = agent_tools::AgentCaller::new(&me, None);
    let created = agent_tools::create_for(&f.home, &me, crate::agents::Placement::Team { org: "acme/ctx".into(), team: "uncle".into() }, &actor,
        serde_json::from_value(serde_json::json!({"id":"saved-source", "name":"Saved source", "runtime":"claude", "startup":["grand-only.md", "uncle.md"]})).unwrap(), &scope()).unwrap();
    let created = serde_json::to_value(created).unwrap();
    assert_eq!(created["ok"], true);
    assert!(!created["data"]["preview"]["manifest"]["entries"]
        .to_string()
        .contains("grand-only.md"));
    let saved_key = created["data"]["id"].as_str().unwrap().to_string();
    let saved_preview = |revision| {
        let result = agent_tools::update_for(
            &f.home,
            &me,
            &actor,
            serde_json::from_value(
                serde_json::json!({"id":saved_key, "baseRevision":revision, "name":"Saved source"}),
            )
            .unwrap(),
            &scope(),
        )
        .unwrap();
        serde_json::to_value(result).unwrap()["data"]["preview"].clone()
    };
    let saved_denied = saved_preview(1);
    assert!(!saved_denied["manifest"]["entries"]
        .to_string()
        .contains("grand-only.md"));
    assert!(saved_denied["manifest"]["entries"]
        .to_string()
        .contains("uncle.md"));
    let read = || {
        let scope = scope();
        let get = agent_tools::get_for(
            &f.home,
            &me,
            AgentGetParams {
                id: key.to_string(),
            },
            &scope,
        )
        .unwrap();
        let preview = agent_tools::preview_for(
            &f.home,
            &me,
            crate::agents::Placement::Team {
                org: "acme/ctx".into(),
                team: "uncle".into(),
            },
            &agent_tools::AgentCaller::new(&me, None),
            serde_json::from_value(serde_json::json!({"agent":key.to_string()})).unwrap(),
            &scope,
        )
        .unwrap();
        let list = agent_tools::list_for(
            &f.home,
            &me,
            serde_json::from_value(serde_json::json!({"includeManifest":true})).unwrap(),
            &scope,
        )
        .unwrap();
        [
            serde_json::to_value(get).unwrap(),
            serde_json::to_value(preview).unwrap(),
            serde_json::to_value(list).unwrap(),
        ]
    };
    assert!(!scope().allows(&source));
    let denied = read();
    assert_eq!(denied[0]["ok"], true);
    assert!(denied[0]["data"]["resolved"]["manifest"]["entries"]
        .to_string()
        .contains("uncle.md"));
    assert!(!denied[0]["data"]["resolved"]["manifest"]["entries"]
        .to_string()
        .contains("grand-only.md"));
    assert!(!denied[0].to_string().contains("grand-srv"));
    assert!(!denied[0]["data"]["context_suggestion"]
        .to_string()
        .contains("grand-secret-dest"));
    assert!(denied[0]["data"]["context_suggestion"]
        .to_string()
        .contains("uncle-positive-dest"));
    std::fs::remove_file(&source).unwrap();
    assert_eq!(denied, read());
    let saved_missing = saved_preview(2);
    for field in ["entries", "errors", "totals", "generated"] {
        assert_eq!(
            saved_denied["manifest"][field],
            saved_missing["manifest"][field]
        );
    }
    assert_eq!(saved_denied["warnings"], saved_missing["warnings"]);
    std::fs::write(&source, "GRAND-SOURCE-SENTINEL").unwrap();
    f.write(
        "parent",
        "config/sharing.yml",
        "share:\n  - content/grand-only.md\n",
    );
    assert!(scope().allows(&source));
    let shared = read();
    assert!(shared[0]["data"]["context_suggestion"]
        .to_string()
        .contains("grand-secret-dest"));
    assert!(shared[0]["data"]["resolved"]["manifest"]["entries"]
        .to_string()
        .contains("grand-only.md"));
}

#[test]
fn agent_delete_referrers_admit_secondary_routine_sources_before_constraints() {
    use crate::agents::AgentReferrers;
    use crate::routines::agents::{CallerRoutineReferrers, RoutineReferrers};
    let f = Fixture::new();
    let me = Principal::from(1u64);
    let key = f.agent("child", "delete-target");
    let watch = format!("id: watch\nname: Parent watch\ntask: Watch completion\nwhere: {{kind: team_workspace}}\nrevision: 1\ntriggers:\n  - event: {{kind: agent_done, from_agent: {key}}}\n");
    let hidden = f.write(
        "parent",
        "config/agents/_builtin/claude/routines/watch/routine.yml",
        &watch,
    );
    assert!(RoutineReferrers
        .referrers(&f.home, &me, &key)
        .iter()
        .any(|name| name.contains("parent")));
    assert!(CallerRoutineReferrers(&f.scope)
        .referrers(&f.home, &me, &key)
        .is_empty());
    let remove = || {
        agent_tools::delete(
            &f.home,
            &me,
            &agent_tools::AgentCaller::new(&me, None),
            &CallerRoutineReferrers(&f.scope),
            serde_json::from_value(serde_json::json!({"id":key.to_string(), "hard":true})).unwrap(),
        )
        .unwrap()
    };
    let denied = serde_json::to_value(remove()).unwrap();
    assert_eq!(denied["ok"], true);
    std::fs::remove_file(hidden).unwrap();
    f.agent("child", "delete-target");
    assert_eq!(denied, serde_json::to_value(remove()).unwrap());
    f.agent("child", "delete-target");
    f.write(
        "child",
        "config/agents/_builtin/claude/routines/watch/routine.yml",
        &watch.replace("Parent watch", "Own watch"),
    );
    let own = serde_json::to_value(remove()).unwrap();
    assert_eq!(own["ok"], false);
    assert!(own.to_string().contains("child/watch"));
}

#[test]
fn committed_ancestor_create_masks_duplicates_and_allocated_ids_but_keeps_writes() {
    let f = Fixture::new();
    let me = Principal::from(1u64);
    let actor = agent_tools::AgentCaller::new(&me, None);
    f.agent("parent", "existing");
    let create = |team: &str, id: Option<&str>| {
        let mut params = serde_json::json!({"name":"Automatic", "runtime":"claude"});
        if let Some(id) = id {
            params["id"] = id.into();
        }
        agent_tools::create_for(
            &f.home,
            &me,
            crate::agents::Placement::Team {
                org: "acme/ctx".into(),
                team: team.into(),
            },
            &actor,
            serde_json::from_value(params).unwrap(),
            &f.scope,
        )
        .unwrap()
    };
    let duplicate = serde_json::to_value(create("parent", Some("existing"))).unwrap();
    let missing = serde_json::to_value(create("parent", Some("new-agent"))).unwrap();
    assert_eq!(duplicate, missing);
    let key = crate::agents::AgentKey::parse("acme/ctx/parent/new-agent").unwrap();
    assert!(
        crate::agents::service::read_current(&f.home, &me, &key).is_ok(),
        "baseline write still happens"
    );
    assert_eq!(
        serde_json::to_value(create("parent", None)).unwrap(),
        serde_json::to_value(create("parent", None)).unwrap()
    );
    assert!(matches!(
        create("child", Some("own-created")),
        AgentReply::Ok(_)
    ));
    assert!(matches!(
        create("sibling", Some("sibling-created")),
        AgentReply::Ok(_)
    ));
    assert!(f
        .scope
        .existing(&f.path("parent", "content/shared.md"))
        .is_ok());
}

#[test]
fn mcp_preview_warns_when_the_cached_cli_version_is_outside_its_range() {
    use crate::commands::deps::DependencyManager;
    use crate::context_repo::caller_scope::tests::AppFixture;
    use tauri::Manager;
    let f = AppFixture::new();
    let me = Principal::from(1u64);
    let draft = serde_json::json!({"name":"Draft", "runtime":"claude"});
    let preview = |draft: serde_json::Value| {
        let caller = crate::commands::routines::Caller {
            home: f.fixture.home.clone(),
            principal: me.clone(),
        };
        serde_json::to_value(
            tauri::async_runtime::block_on(agent_tools::preview_request(
                f.app.handle(),
                None,
                None,
                caller,
                agent_tools::AgentCaller::new(&me, None),
                crate::agents::Placement::Team {
                    org: "acme/ctx".into(),
                    team: "parent".into(),
                },
                serde_json::from_value(draft).unwrap(),
            ))
            .unwrap(),
        )
        .unwrap()
    };
    let codes = |reply: &serde_json::Value| {
        reply["data"]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["code"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    // No dependency state yet (or a version nobody has read): nothing to say.
    assert!(!codes(&preview(draft.clone())).contains(&"cli_version_unqualified".to_owned()));
    f.app.manage(DependencyManager::cached_for_test(&[(
        "claude",
        Some("2.0.1"),
    )]));
    let warned = preview(draft.clone());
    assert!(
        codes(&warned).contains(&"cli_version_unqualified".to_owned()),
        "{warned}"
    );
    let message = warned["data"]["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "cli_version_unqualified")
        .unwrap()["message"]
        .as_str()
        .unwrap();
    assert!(message.contains("2.0.1") && message.contains("Claude Code 2.1.141 or newer"));
}
