// SPDX-License-Identifier: MIT
use super::*;
use crate::context_repo::caller_scope::{tests::AppFixture, CallerScope};
use crate::domain::Principal;
use std::path::Path;

fn child(f: &AppFixture) -> CallerScope {
    CallerScope::team(
        &f.fixture.home,
        &Principal::from(1u64),
        &commands::dashboard_launch::LaunchTeam {
            org: "acme/ctx".into(),
            team: "child".into(),
        },
    )
}

#[test]
fn validator_requests_keep_child_authority_when_parent_is_active() {
    let f = AppFixture::new();
    f.session("child-validator", Some("child"));
    f.app
        .state::<super::super::state::ActiveTeamState>()
        .update(crate::mcp::state::ActiveTeam {
            org_slug: "acme/ctx".into(),
            team_name: "parent".into(),
        });
    let hidden = f.fixture.write(
        "parent",
        "content/secret.html",
        "<!doctype html><title>Private</title>",
    );
    let shared = f.fixture.write(
        "parent",
        "content/shared.md",
        "# Shared valid plain document\n",
    );
    let missing = f.fixture.path("parent", "content/missing.html");
    let live = |path: &Path| {
        tauri::async_runtime::block_on(validate_live_data_request(
            child(&f),
            ValidateLiveDataParams {
                path: Some(path.display().to_string()),
                content: None,
            },
        ))
    };
    let a = live(&hidden).unwrap_err();
    let b = live(&missing).unwrap_err();
    assert_eq!((a.code, a.message), (b.code, b.message));
    assert!(live(&shared).is_ok());
    let story = |path: &Path| {
        tauri::async_runtime::block_on(validate_story_request(
            f.app.handle(),
            Some("child-validator"),
            None,
            ValidateStoryParams {
                path: Some(path.display().to_string()),
                content: None,
            },
        ))
    };
    let a = story(&hidden).unwrap_err();
    let b = story(&missing).unwrap_err();
    assert_eq!((a.code, a.message), (b.code, b.message));
    assert!(story(&shared).is_ok());
}

#[test]
fn canvas_request_captures_admitted_scene_before_export_under_reversed_active_team() {
    use tauri::Listener;
    let f = AppFixture::new();
    f.session("canvas-child", Some("child"));
    f.session("canvas-parent", Some("parent"));
    f.app.manage(CanvasRenderRegistry::new());
    let hidden = f.fixture.write(
        "parent",
        "content/secret.excalidraw",
        r#"{"type":"excalidraw","elements":[]}"#,
    );
    let shared = f.fixture.write(
        "parent",
        "content/shared.excalidraw",
        r#"{"type":"excalidraw","elements":[]}"#,
    );
    f.fixture.write(
        "parent",
        "config/sharing.yml",
        "share: [content/shared.excalidraw]\n",
    );
    let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
    let seen = observed.clone();
    let app = f.app.handle().clone();
    f.app.listen("canvas:render-request", move |event| {
        let payload: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
        app.state::<CanvasRenderRegistry>()
            .take(payload["requestId"].as_str().unwrap())
            .unwrap()
            .send(Ok("iVBORw0KGgo=".into()))
            .unwrap();
        seen.lock().unwrap().push(payload);
    });
    let render = |session: &str, path: &Path| {
        tauri::async_runtime::block_on(render_canvas_request(
            f.app.handle(),
            Some(session),
            None,
            RenderCanvasParams {
                path: path.display().to_string(),
            },
        ))
    };
    f.app
        .state::<super::super::state::ActiveTeamState>()
        .update(crate::mcp::state::ActiveTeam {
            org_slug: "acme/ctx".into(),
            team_name: "parent".into(),
        });
    let a = render("canvas-child", &hidden).unwrap_err();
    let b = render(
        "canvas-child",
        &f.fixture.path("parent", "content/missing.excalidraw"),
    )
    .unwrap_err();
    assert_eq!((a.code, a.message), (b.code, b.message));
    assert!(observed.lock().unwrap().is_empty());
    assert!(render("canvas-child", &shared).is_ok());
    assert_eq!(
        observed.lock().unwrap()[0]["sceneJson"],
        std::fs::read_to_string(&shared).unwrap()
    );
    f.app
        .state::<super::super::state::ActiveTeamState>()
        .update(crate::mcp::state::ActiveTeam {
            org_slug: "acme/ctx".into(),
            team_name: "child".into(),
        });
    assert!(render("canvas-parent", &hidden).is_ok());
}

#[test]
fn role_yaml_peer_files_and_http_models_filter_before_content_metadata() {
    let f = AppFixture::new();
    let scope = child(&f);
    let role = f.fixture.write(
        "parent",
        "content/roles/qa.yaml",
        "id: qa\nname: Hidden role\ndefaultHandler: acme/ctx/parent/secret\n",
    );
    let parent = f.fixture.path("parent", "content");
    assert!(crate::queue_roles::list_for(&parent, &scope)
        .unwrap()
        .is_empty());
    std::fs::remove_file(&role).unwrap();
    assert!(crate::queue_roles::list_for(&parent, &scope)
        .unwrap()
        .is_empty());
    f.fixture.write(
        "parent",
        "content/roles/qa.yaml",
        "id: qa\nname: Shared role\n",
    );
    f.fixture.write(
        "parent",
        "config/sharing.yml",
        "share: [content/roles/qa.yaml, content/shared.md, content/status-taxonomy.yml]\n",
    );
    let scope = child(&f);
    assert_eq!(
        crate::queue_roles::list_for(&parent, &scope).unwrap()[0].name,
        "Shared role"
    );
    assert_eq!(
        read_peer_worktree_file_for(&parent, "secret.md", &scope),
        read_peer_worktree_file_for(&parent, "missing.md", &scope)
    );
    assert!(
        read_peer_worktree_file_for(&parent, "shared.md", &scope)["contentUtf8"]
            .as_str()
            .unwrap()
            .contains("parent")
    );
    f.fixture.write(
        "parent",
        "content/status-taxonomy.yml",
        "states:\n  - {id: shipped, label: Shipped, role: done}\n",
    );
    assert!(
        crate::commands::status_taxonomy::load_status_taxonomy_from_for(&parent, &scope)
            .states
            .iter()
            .any(|s| s.id == "shipped")
    );
    let ledger = f.fixture.write(
        "parent",
        "config/sprints.yaml",
        "sprints:\n  - {id: secret-sprint, name: Secret}\n",
    );
    let denied = serde_json::to_value(
        crate::commands::sprint_registry::load_sprint_registry_from_for(
            &f.fixture.path("parent", "config"),
            &scope,
        ),
    )
    .unwrap();
    std::fs::remove_file(ledger).unwrap();
    assert_eq!(
        denied,
        serde_json::to_value(
            crate::commands::sprint_registry::load_sprint_registry_from_for(
                &f.fixture.path("parent", "config"),
                &scope
            )
        )
        .unwrap()
    );
    f.fixture.write(
        "parent",
        "content/shared.md",
        "---\ntype: story\nsource_repo: acme/api\nstory_id: SHARED\nsprint: spring\n---\nShared\n",
    );
    f.fixture.write(
        "parent",
        "content/secret.md",
        "---\ntype: story\nsource_repo: acme/api\nstory_id: SECRET\nsprint: spring\n---\nSecret\n",
    );
    let stories = || {
        serde_json::to_value(crate::commands::sprint_registry::sprint_member_stories_for(
            &f.fixture.path("parent", "config"),
            &parent,
            "spring",
            None,
            &scope,
        ))
        .unwrap()
    };
    let denied = stories();
    assert!(denied.to_string().contains("SHARED"));
    assert!(!denied.to_string().contains("SECRET"));
    std::fs::remove_file(parent.join("secret.md")).unwrap();
    assert_eq!(denied, stories());
}

#[test]
fn publish_and_filtered_history_omit_ancestor_names_and_counts() {
    let f = AppFixture::new();
    let scope = child(&f);
    let repo = git2::Repository::init(&f.fixture.root).unwrap();
    let commit = |message: &str| {
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("Alice", "alice@example.com").unwrap();
        let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            message,
            &tree,
            &parent.iter().collect::<Vec<_>>(),
        )
        .unwrap()
    };
    let base = commit("base");
    repo.reference("refs/remotes/origin/main", base, true, "baseline")
        .unwrap();
    f.fixture
        .write("parent", "content/secret.md", "Hidden edit\n");
    f.fixture
        .write("parent", "content/shared.md", "Shared edit\n");
    let publish = || {
        crate::commands::git_sync::publish_set_impl_for(&f.fixture.root, None, None, &scope)
            .unwrap()
    };
    let denied = publish();
    assert!(serde_json::to_string(&denied)
        .unwrap()
        .contains("shared.md"));
    assert!(!serde_json::to_string(&denied)
        .unwrap()
        .contains("secret.md"));
    std::fs::write(
        f.fixture.path("parent", "content/secret.md"),
        "parent-secret.md-bytes",
    )
    .unwrap();
    assert_eq!(denied, publish());
    f.fixture
        .write("parent", "content/secret.md", "Hidden committed\n");
    commit("hidden commit");
    let paths = vec!["parent/content/secret.md".into()];
    let history = crate::commands::context_repo_git::context_repo_history_impl_for(
        &f.fixture.root,
        &crate::commands::context_repo_git::HistoryBranches::All,
        Some(&paths),
        60,
        None,
        &scope,
    )
    .unwrap();
    assert!(history.commits.is_empty());
    let paths = vec!["parent/content/shared.md".into()];
    let history = crate::commands::context_repo_git::context_repo_history_impl_for(
        &f.fixture.root,
        &crate::commands::context_repo_git::HistoryBranches::All,
        Some(&paths),
        60,
        None,
        &scope,
    )
    .unwrap();
    assert!(!history.commits.is_empty());
    assert!(!serde_json::to_string(&history)
        .unwrap()
        .contains("parent/content/secret.md"));
}

#[test]
fn explicit_parent_hydration_want_is_intersected_with_real_child_session() {
    use crate::agents::prepare::{step3, Destination};
    let f = AppFixture::new();
    f.session("hydrate-child", Some("child"));
    let scope = tauri::async_runtime::block_on(crate::context_repo::caller_scope::for_session(
        f.app.handle(),
        Some("hydrate-child"),
        None,
    ));
    let target = crate::agents::ManifestTarget::team("acme/ctx", "parent");
    let paths = vec!["secret.md".into(), "shared.md".into()];
    let want = step3::want_for_selection_for(
        &f.fixture.home,
        &Principal::from(1u64),
        &target,
        &paths,
        &[],
        &scope,
    )
    .unwrap();
    assert_eq!(want.files.len(), 1);
    assert_eq!(want.files[0].dest, "shared.md");
    let output = f.fixture.temp.path().join("parent-destination");
    std::fs::create_dir(&output).unwrap();
    let report = step3::hydrate(step3::Hydrate {
        home: &f.fixture.home,
        principal: &Principal::from(1u64),
        destination: Destination::Local { root: &output },
        want: &want,
        operation: "child-parent-hydrate",
        workspace: "parent".into(),
        agent: "child".into(),
        prune: false,
        overwrite_modified: false,
        progress: &|_| {},
        validate: &|| Ok(()),
        cancel: None,
        legacy: &|| Ok(Vec::new()),
    })
    .unwrap();
    assert!(output.join("shared.md").is_file());
    assert!(!output.join("secret.md").exists());
    assert!(!serde_json::to_string(&report)
        .unwrap()
        .contains("secret.md"));
    let hidden = f.fixture.path("parent", "content/secret.md");
    std::fs::remove_file(hidden).unwrap();
    let missing = step3::want_for_selection_for(
        &f.fixture.home,
        &Principal::from(1u64),
        &target,
        &paths,
        &[],
        &scope,
    )
    .unwrap();
    assert_eq!(want, missing);
}

#[test]
fn shared_parent_html_capture_refuses_parent_viewer_before_dom() {
    let f = AppFixture::new();
    let page = f.fixture.write(
        "parent",
        "content/shared.html",
        "<!doctype html><title>Shared dashboard</title>",
    );
    let parent = f.frame_for_team(&page, "open", "parent");
    let child_frame = f.frame_for_team(&page, "open", "child");
    let registry = crate::mcp::state::RenderedDocumentRegistry::new();
    let (id, rx) = registry.register_for(child(&f));
    assert_eq!(
        crate::mcp::state::admit_rendered_document_for(
            f.app.handle(),
            &registry,
            &id,
            Some(&parent.frame_id)
        )
        .unwrap_err(),
        crate::context_repo::caller_scope::MISSING
    );
    assert_eq!(
        tauri::async_runtime::block_on(rx).unwrap().unwrap_err(),
        crate::context_repo::caller_scope::MISSING
    );
    let (id, _rx) = registry.register_for(child(&f));
    assert!(crate::mcp::state::admit_rendered_document_for(
        f.app.handle(),
        &registry,
        &id,
        Some(&child_frame.frame_id)
    )
    .is_ok());
    let (id, _rx) = registry.register_for(CallerScope::Human);
    assert!(crate::mcp::state::admit_rendered_document_for(
        f.app.handle(),
        &registry,
        &id,
        Some(&parent.frame_id)
    )
    .is_ok());
}

#[test]
fn production_mint_retains_source_identity_but_reads_as_child_host_and_isolates_record_views() {
    let f = AppFixture::new();
    f.app.manage(crate::http::HttpApiPort(9321));
    f.app.manage(crate::live_hub::LiveHub::default());
    f.app.manage(crate::records::projection::Views::default());
    let page = f.fixture.write(
        "parent",
        "content/shared.html",
        "<!doctype html><html><head><title>Shared</title></head><body>Dashboard</body></html>",
    );
    f.fixture.write(
        "parent",
        "content/schema/customers.yaml",
        "collection: customers\nscope: team\nfolder: content/customers\nid: slug\n",
    );
    let hidden = f.fixture.write(
        "parent",
        "content/customers/secret.md",
        "# SECRET-CUSTOMER\nPrivate rows\n",
    );
    f.fixture.write(
        "parent",
        "content/customers/shared.md",
        "# SHARED-CUSTOMER\nAdmitted rows\n",
    );
    f.fixture.write("parent", "config/sharing.yml", "share: [content/shared.md, content/shared.html, content/schema/customers.yaml, content/customers/shared.md]\n");
    let service = f.fixture.service();
    f.app.manage(service);
    let mint = |host: &str| {
        f.app
            .state::<crate::mcp::state::ActiveTeamState>()
            .update(crate::mcp::state::ActiveTeam {
                org_slug: "acme/ctx".into(),
                team_name: host.into(),
            });
        tauri::async_runtime::block_on(commands::frames::mint_frame(
            f.app.handle().clone(),
            commands::frames::MintFrameArgs {
                document_path: page.display().to_string(),
                content_root: f.fixture.path(host, "content").display().to_string(),
                asset_folder: None,
                slot: "right-pane".into(),
                message: None,
                parent_frame: None,
                builtin_team: None,
                policy: Default::default(),
            },
        ))
        .unwrap()
    };
    let child_frame = mint("child");
    let parent_frame = mint("parent");
    assert_eq!(child_frame.grant.team.as_ref().unwrap().team, "parent");
    let scope = crate::context_repo::caller_scope::for_page(
        f.app.handle(),
        None,
        Some(&child_frame.frame_id),
    )
    .unwrap();
    assert!(matches!(&scope, CallerScope::Team(view) if view.reader_team() == "child"));
    let query = tauri::async_runtime::block_on(commands::context_query::query_context(
        f.app.handle().clone(),
        serde_json::from_value(serde_json::json!({"where":{"teams":["parent"]}})).unwrap(),
        page.display().to_string(),
        Some(child_frame.frame_id.clone()),
        None,
        None,
    ))
    .unwrap();
    assert!(!serde_json::to_string(&query).unwrap().contains("secret.md"));
    let read = |path: &Path| {
        tauri::async_runtime::block_on(commands::files::read_file_snapshot_for(
            f.app.handle().clone(),
            path.display().to_string(),
            None,
            scope.clone(),
        ))
    };
    assert_eq!(
        read(&hidden).unwrap_err(),
        read(&f.fixture.path("parent", "content/customers/missing.md")).unwrap_err()
    );
    assert!(read(&f.fixture.path("parent", "content/shared.md")).is_ok());
    let call = |id: &str| {
        tauri::async_runtime::block_on(crate::records::commands::records_call(
            f.app.handle().clone(),
            child_frame.frame_id.clone(),
            "read".into(),
            serde_json::json!({"collection":"customers","id":id}),
        ))
    };
    assert_eq!(call("secret").unwrap_err(), call("missing").unwrap_err());
    assert!(call("shared")
        .unwrap()
        .to_string()
        .contains("SHARED-CUSTOMER"));
    for frames in [[&parent_frame, &child_frame], [&child_frame, &parent_frame]] {
        let mut keys = Vec::new();
        for frame in frames {
            let key =
                tauri::async_runtime::block_on(crate::records::commands::records_prepare_view(
                    f.app.handle().clone(),
                    frame.frame_id.clone(),
                    "customers".into(),
                ))
                .unwrap();
            let reply =
                crate::live_hub::subscribe(f.app.handle(), "records", &key, None, None, None)
                    .unwrap();
            let text = serde_json::to_string(&reply).unwrap();
            assert!(text.contains("SHARED-CUSTOMER"));
            assert_eq!(
                text.contains("SECRET-CUSTOMER"),
                frame.frame_id == parent_frame.frame_id
            );
            keys.push(key);
        }
        assert_ne!(keys[0], keys[1]);
    }
}

#[test]
fn avatar_metadata_is_admitted_before_collection_names_and_default_selection() {
    let f = AppFixture::new();
    let manifest = f.fixture.write(
        "parent",
        "content/avatars/hidden/pack.yaml",
        "name: Secret faces
faces: []
",
    );
    let places = super::super::avatar_tools::AvatarPlaces::for_team(
        &f.fixture.home,
        &Principal::from(1u64),
        "acme/ctx",
        "parent",
    );
    let read = |scope: &CallerScope| {
        serde_json::to_value(super::super::avatar_tools::list_for(
            &places,
            Default::default(),
            scope,
        ))
        .unwrap()
    };
    let denied = read(&child(&f));
    assert!(!denied.to_string().contains("Secret faces"));
    std::fs::remove_file(&manifest).unwrap();
    assert_eq!(denied, read(&child(&f)));
    std::fs::write(
        &manifest,
        "name: Shared faces
faces: []
",
    )
    .unwrap();
    f.fixture.write(
        "parent",
        "config/sharing.yml",
        "share: [content/avatars/hidden/pack.yaml]
",
    );
    assert!(read(&child(&f)).to_string().contains("Shared faces"));
}

#[test]
fn avatar_import_reads_only_admitted_real_source_before_copying_into_own_collection() {
    use crate::avatars::PackScope;
    let f = AppFixture::new();
    f.session("child-avatar", Some("child"));
    let source = f.fixture.path("parent", "content/photo.png");
    let file = std::fs::File::create(&source).unwrap();
    let mut encoder = png::Encoder::new(file, 4, 4);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&[10u8, 100, 200, 255].repeat(16))
        .unwrap();
    let destination = f.fixture.path("child", "content/avatars");
    std::fs::create_dir_all(destination.join("staff")).unwrap();
    let scope = || {
        tauri::async_runtime::block_on(crate::context_repo::caller_scope::for_session(
            f.app.handle(),
            Some("child-avatar"),
            None,
        ))
    };
    let import = |file: &str, id: &str| {
        serde_json::to_value(super::super::avatar_tools::add_for(
            PackScope::Team,
            Ok(destination.clone()),
            serde_json::from_value(serde_json::json!({"pack":"staff", "id":id, "file":file}))
                .unwrap(),
            &scope(),
            || {},
        ))
        .unwrap()
    };
    let denied = import(&source.display().to_string(), "portrait");
    assert_eq!(
        denied,
        import(
            &f.fixture
                .path("parent", "content/missing.png")
                .display()
                .to_string(),
            "portrait"
        )
    );
    assert_eq!(
        denied,
        import("../../../../parent/content/photo.png", "portrait")
    );
    assert!(!destination.join("staff/pack.yaml").exists());
    f.fixture.write(
        "parent",
        "config/sharing.yml",
        "share: [content/photo.png]
",
    );
    assert_eq!(
        import(&source.display().to_string(), "portrait")["ok"],
        true
    );
    assert_eq!(
        import("../../../../parent/content/photo.png", "relative")["ok"],
        true
    );
    assert!(destination.join("staff/pack.yaml").is_file());
}

#[tokio::test]
async fn page_saved_launch_and_remote_tab_admit_definition_and_target_before_preparation() {
    let f = AppFixture::new();
    let principal = Principal::from(1u64);
    let scope = child(&f);
    let hidden = f.fixture.agent("parent", "reviewer");
    let own = f.fixture.agent("child", "reviewer");
    let shared = f.fixture.write("parent", "content/shared.md", "Shared");
    assert!(scope.existing(&shared).is_ok());
    let admit = |reference: &str, team: &str| {
        commands::agents_launch::admit_named_launch(
            &f.fixture.home,
            &principal,
            &scope,
            reference,
            Some(("acme/ctx", team)),
            &f.fixture.home,
        )
    };
    assert_eq!(
        admit(&hidden.to_string(), "child").unwrap_err(),
        crate::context_repo::caller_scope::MISSING
    );
    assert_eq!(
        admit(&hidden.to_string(), "parent").unwrap_err(),
        admit("acme/ctx/parent/absent", "parent").unwrap_err()
    );
    assert_eq!(admit(&own.to_string(), "child").unwrap(), own.to_string());
    use crate::commands::pty_layout::RemoteShellTabRequest;
    let request = |agent: String, team: &str| RemoteShellTabRequest::Create {
        request_id: uuid::Uuid::new_v4(),
        machine_id: "local".into(),
        team_key: format!("acme/ctx:{team}"),
        workspace_id: "workspace".into(),
        cwd: Some("/workspace".into()),
        name: None,
        launch: None,
        permission_mode: Default::default(),
        agent_key: Some(agent),
        prepared_workspace_handle: Some("prepared".into()),
        headed: false,
        initial_prompt: None,
        identity: Default::default(),
    };
    let denied = super::super::machine_tools::admit_remote_tab_request(
        f.app.handle(),
        &request(hidden.to_string(), "child"),
        &scope,
    )
    .await
    .unwrap_err();
    std::fs::remove_file(
        f.fixture
            .path("parent", "config/agents/reviewer/agent.yaml"),
    )
    .unwrap();
    assert_eq!(
        denied,
        super::super::machine_tools::admit_remote_tab_request(
            f.app.handle(),
            &request(hidden.to_string(), "child"),
            &scope
        )
        .await
        .unwrap_err()
    );
    assert!(super::super::machine_tools::admit_remote_tab_request(
        f.app.handle(),
        &request(own.to_string(), "child"),
        &scope
    )
    .await
    .is_ok());
    let mut shell = request(String::new(), "parent");
    if let RemoteShellTabRequest::Create {
        agent_key,
        prepared_workspace_handle,
        ..
    } = &mut shell
    {
        *agent_key = None;
        *prepared_workspace_handle = None;
    }
    assert!(
        super::super::machine_tools::admit_remote_tab_request(f.app.handle(), &shell, &scope)
            .await
            .is_ok()
    );
    let attach = RemoteShellTabRequest::Attach {
        machine_id: "local".into(),
        team_key: "acme/ctx:parent".into(),
        reference: crate::commands::pty_layout::RemoteTabReference::RequestId(uuid::Uuid::new_v4()),
        headed: false,
    };
    assert!(
        super::super::machine_tools::admit_remote_tab_request(f.app.handle(), &attach, &scope)
            .await
            .is_ok()
    );
}

#[test]
fn routine_preview_and_run_admission_hide_ancestor_definitions_before_warnings_or_dispatch() {
    use crate::mcp::routine_tools::*;
    use crate::routines::service::Placement;
    let f = AppFixture::new();
    let principal = Principal::from(1u64);
    let scope = child(&f);
    let hidden = f.fixture.agent("parent", "preview-agent");
    let own = f.fixture.agent("child", "preview-agent");
    let preview = |agent: &str| {
        let params = serde_json::from_value(serde_json::json!({
            "name":"Preview", "task":"Inspect", "when":"manual", "agent":agent,
            "enabled":false, "dryRun":true
        }))
        .unwrap();
        create_for(
            &f.fixture.home,
            &principal,
            Placement::Personal(crate::routines::PersonalOwner {
                github_user_id: 1,
                username: "me".into(),
                display_name: "Me".into(),
                avatar_url: None,
                org: "acme/ctx".into(),
            }),
            &CallerSession::default(),
            &crate::routines::Author::agent("test"),
            params,
            chrono::Utc::now(),
            &scope,
        )
    };
    let denied = serde_json::to_value(preview(&hidden.to_string()).unwrap()).unwrap();
    assert_eq!(denied["status"], "validation");
    assert_eq!(
        denied["data"]["message"],
        crate::context_repo::caller_scope::MISSING
    );
    std::fs::remove_file(
        f.fixture
            .path("parent", "config/agents/preview-agent/agent.yaml"),
    )
    .unwrap();
    assert_eq!(
        denied,
        serde_json::to_value(preview(&hidden.to_string()).unwrap()).unwrap()
    );
    assert!(matches!(
        preview(&own.to_string()).unwrap(),
        ToolEnvelope::Ok(_)
    ));
    let parent = f.fixture.routine("parent", "manual");
    let own = f.fixture.routine("child", "manual");
    let run = |key: String| {
        run_now_with_for(
            &f.fixture.home,
            &principal,
            RoutineRunNowParams {
                id: key,
                variables: None,
                session_id: None,
            },
            &|_, _| panic!("no machine dispatch expected"),
            &scope,
        )
    };
    let denied = run(parent).unwrap_err();
    assert_eq!(denied, run("acme/ctx/parent/absent".into()).unwrap_err());
    assert!(run(own).is_ok());
    assert!(scope
        .existing(&f.fixture.path("parent", "content/shared.md"))
        .is_ok());
}

#[tokio::test]
async fn page_agent_scope_is_recorded_before_bridge_queries_with_parent_active() {
    use crate::commands::dashboard_launch::DashboardLaunchState;
    let f = AppFixture::new();
    f.app.manage(DashboardLaunchState::new(16));
    f.app.manage(f.fixture.service());
    let principal = Principal::from(1u64);
    let page = f
        .fixture
        .write("parent", "content/shared.html", "<title>Shared</title>");
    let frame = f.frame(&page, "workspace");
    let scope = crate::context_repo::caller_scope::for_page(
        f.app.handle(),
        Some(page.to_str().unwrap()),
        Some(&frame.frame_id),
    )
    .unwrap();
    f.app
        .state::<super::super::state::ActiveTeamState>()
        .update(crate::mcp::state::ActiveTeam {
            org_slug: "acme/ctx".into(),
            team_name: "parent".into(),
        });
    let cwd = f.fixture.temp.path().join("generic-workspace");
    std::fs::create_dir_all(&cwd).unwrap();
    scope
        .record_page_launch(
            f.app.handle(),
            &f.fixture.home,
            &principal,
            "page-agent",
            &cwd,
            Some("acme/ctx:child"),
            page.to_str(),
        )
        .unwrap();
    let caller =
        crate::context_repo::caller_scope::for_session(f.app.handle(), Some("page-agent"), None)
            .await;
    assert!(matches!(caller, CallerScope::Team(_)));
    let read = |path: &str| {
        super::super::context_query_tool::request(
            f.app.handle().clone(),
            Some("page-agent".into()),
            None,
            serde_json::from_value(
                serde_json::json!({"where":{"pathPrefix":f.fixture.path("parent", path)}}),
            )
            .unwrap(),
        )
    };
    assert_eq!(
        serde_json::to_value(read("content/secret.md").await.unwrap()).unwrap(),
        serde_json::to_value(read("content/absent.md").await.unwrap()).unwrap()
    );
    let shared = serde_json::to_string(&read("content/shared.md").await.unwrap()).unwrap();
    assert!(shared.contains("shared.md"));
    assert_eq!(
        scope
            .record_page_launch(
                f.app.handle(),
                &f.fixture.home,
                &principal,
                "blocked-page-agent",
                &cwd,
                Some("acme/ctx:parent"),
                page.to_str()
            )
            .unwrap_err(),
        crate::context_repo::caller_scope::MISSING
    );
    assert!(!f
        .app
        .state::<DashboardLaunchState>()
        .has_handle("blocked-page-agent"));
}

#[test]
fn routine_preparation_controls_and_new_schedules_gate_before_side_effects() {
    use crate::mcp::routine_tools::*;
    use std::cell::Cell;
    let f = AppFixture::new();
    let principal = Principal::from(1u64);
    let scope = child(&f);
    let parent = f.fixture.routine("parent", "prepare");
    let own = f.fixture.routine("child", "prepare");
    let sibling = f.fixture.routine("sibling", "prepare");
    let calls = Cell::new(0);
    let refresh = |_: &crate::routines::RoutineKey| {
        calls.set(calls.get() + 1);
        Ok("machine".into())
    };
    let request = |id: String| RoutineRefreshOnMachineParams {
        id,
        session_id: None,
    };
    let denied = refresh_on_machine_for(
        &f.fixture.home,
        &principal,
        request(parent.clone()),
        &refresh,
        &scope,
    )
    .unwrap_err();
    assert_eq!(
        denied,
        refresh_on_machine_for(
            &f.fixture.home,
            &principal,
            request("acme/ctx/parent/absent".into()),
            &refresh,
            &scope
        )
        .unwrap_err()
    );
    assert_eq!(calls.get(), 0);
    for key in [own, sibling] {
        assert!(refresh_on_machine_for(
            &f.fixture.home,
            &principal,
            request(key),
            &refresh,
            &scope
        )
        .is_ok());
    }
    assert_eq!(calls.get(), 2);
    let enable: RoutineSetEnabledParams =
        serde_json::from_value(serde_json::json!({"id":parent,"enabled":true})).unwrap();
    assert_eq!(
        set_enabled_and_refresh_for(
            &f.fixture.home,
            &principal,
            enable,
            &|_| panic!("must not prepare"),
            &scope
        )
        .unwrap_err(),
        denied
    );
    let preview = |team: &str, authority: &CallerScope| {
        create_for(&f.fixture.home, &principal,
        crate::routines::service::Placement::Team { org:"acme/ctx".into(), team:team.into() },
        &CallerSession::default(), &crate::routines::Author::user("1"),
        serde_json::from_value(serde_json::json!({"id":"scheduled", "name":"Scheduled", "task":"Inspect", "agent":"builtin/claude-unattended", "when":"daily 09:00", "enabled":true,"dryRun":true})).unwrap(),
        chrono::Utc::now(), authority)
    };
    assert_eq!(preview("parent", &scope).unwrap_err(), denied);
    for team in ["child", "sibling"] {
        assert!(matches!(
            preview(team, &scope).unwrap(),
            ToolEnvelope::Ok(_)
        ));
    }
    assert!(matches!(
        preview("parent", &CallerScope::Human).unwrap(),
        ToolEnvelope::Ok(_)
    ));
    assert!(
        crate::context_repo::scratch_space::org_space(&f.fixture.home, &principal, "acme/ctx")
            .is_none()
    );
    for dry_run in [true, false] {
        let private = create_for(&f.fixture.home, &principal,
            crate::routines::service::Placement::Personal(crate::routines::service::PersonalOwner {
                github_user_id:1, username:"me".into(), display_name:"Me".into(), avatar_url:None, org:"acme/ctx".into(),
            }), &CallerSession::default(), &crate::routines::Author::user("1"),
            serde_json::from_value(serde_json::json!({"id":"private-preview", "name":"Private", "task":"Inspect", "agent":"builtin/claude-unattended", "enabled":false, "dryRun":dry_run})).unwrap(),
            chrono::Utc::now(), &scope).unwrap();
        assert!(matches!(private, ToolEnvelope::Ok(_)), "{private:?}");
    }
    assert!(scope
        .existing(&f.fixture.path("parent", "content/shared.md"))
        .is_ok());
}

#[tokio::test]
async fn usage_enrichment_admits_current_source_without_filtering_runtime_rows() {
    use crate::session_usage::{self, Filter, StoryLink, UsageStore};
    let f = AppFixture::new();
    let hidden = f.fixture.write("parent", "content/usage-secret.md", "---\ntype: story\nstory_id: P-USAGE\ntitle: Hidden usage title\nstatus: in-progress\nepic: Parent epic\n---\n");
    let shared = f.fixture.write(
        "parent",
        "content/shared.md",
        "---\ntype: story\nstory_id: S-USAGE\ntitle: Shared usage title\nstatus: ready\n---\n",
    );
    let mut store = UsageStore::open(&f.fixture.home.join("usage.sqlite")).unwrap();
    for (session, id, source) in [
        ("parent-usage", "P-USAGE", &hidden),
        ("shared-usage", "S-USAGE", &shared),
    ] {
        let event = session_usage::tests::event(
            session,
            session,
            "claude-opus-4-8",
            session_usage::tests::local_noon(2026, 10, 15),
            session_usage::event::Tokens {
                input: 1,
                ..Default::default()
            },
        );
        store.ingest(&[event]).unwrap();
        store
            .link_story(
                session,
                &StoryLink {
                    story_id: id.into(),
                    source: source.clone(),
                    org: "acme/ctx".into(),
                },
            )
            .unwrap();
    }
    let page = f
        .fixture
        .write("parent", "content/shared.html", "<title>Shared</title>");
    let frame = f.frame(&page, "workspace");
    let read = || {
        commands::data_read_model::session_read_usage(
            f.app.handle().clone(),
            Filter::default(),
            Some(frame.frame_id.clone()),
            Some(page.display().to_string()),
        )
    };
    let denied = read().await.unwrap();
    let denied_session_count = match &denied {
        session_usage::UsageReport { sessions, .. } => sessions.len(),
    };
    assert_eq!(denied_session_count, 2, "runtime rows remain visible");
    let story = |report: &session_usage::UsageReport, session: &str| {
        report
            .sessions
            .iter()
            .find(|row| row.session_id == session)
            .unwrap()
            .story
            .clone()
            .unwrap()
    };
    assert_eq!(story(&denied, "parent-usage").title, None);
    assert_eq!(story(&denied, "parent-usage").status, None);
    assert_eq!(story(&denied, "parent-usage").epic, None);
    assert_eq!(
        story(&denied, "shared-usage").title.as_deref(),
        Some("Shared usage title")
    );
    let human = commands::data_read_model::session_read_usage(
        f.app.handle().clone(),
        Filter::default(),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        story(&human, "parent-usage").title.as_deref(),
        Some("Hidden usage title")
    );
    std::fs::remove_file(hidden).unwrap();
    let missing = read().await.unwrap();
    assert_eq!(
        serde_json::to_value(denied).unwrap(),
        serde_json::to_value(missing).unwrap()
    );
}

#[test]
fn selected_registered_checkout_hydrates_shared_parent_content_through_real_child_scope() {
    use crate::agents::prepare::{step3, Destination};
    use crate::install::worktree_index::{self, ContextWorktreeRecord};
    let f = AppFixture::new();
    f.session("checkout-child", Some("child"));
    let me = Principal::from(1u64);
    std::fs::remove_file(f.fixture.path("child", "content/shared.md")).unwrap();
    f.fixture.write(
        "parent",
        "content/parent-only.md",
        "PRIVATE-CHECKOUT-SENTINEL",
    );
    let repo = git2::Repository::init(&f.fixture.root).unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = git2::Signature::now("Test", "test@example.com").unwrap();
    let commit = repo
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "context source",
            &tree,
            &[],
        )
        .unwrap();
    let commit = repo.find_commit(commit).unwrap();
    repo.branch("source-checkout", &commit, false).unwrap();
    let reference = repo.find_reference("refs/heads/source-checkout").unwrap();
    let mut options = git2::WorktreeAddOptions::new();
    options.reference(Some(&reference));
    let checkout = f.fixture.temp.path().join("selected-context-checkout");
    repo.worktree("source-checkout", &checkout, Some(&options))
        .unwrap();
    worktree_index::write_context_record(
        &f.fixture.home,
        &me,
        &ContextWorktreeRecord {
            owner: None,
            extras: Default::default(),
            schema_version: worktree_index::SCHEMA_VERSION,
            kind: "context".into(),
            id: "selected-source".into(),
            org: "acme/ctx".into(),
            team: "parent".into(),
            name: "Selected source".into(),
            branch: "source-checkout".into(),
            base_sha: commit.id().to_string(),
            root: checkout.clone(),
            launch_cwd: checkout.join("parent/content"),
            created_at: 1,
            last_used_at: 1,
        },
    )
    .unwrap();
    let scope = tauri::async_runtime::block_on(crate::context_repo::caller_scope::for_session(
        f.app.handle(),
        Some("checkout-child"),
        None,
    ));
    let mut ctx = crate::content_engine::ResolverContext {
        context_root: None,
        codesense_home: f.fixture.home.clone(),
        user_id: 1,
        org_slug: "acme/ctx".into(),
        team_name: Some("child".into()),
    };
    crate::mcp::context_operations::bind_context(
        &mut ctx,
        Some(
            serde_json::from_value(serde_json::json!({"kind":"worktree","id":"selected-source"}))
                .unwrap(),
        ),
        None,
    )
    .unwrap();
    let mut target = crate::agents::ManifestTarget::team("acme/ctx", "child");
    target.context_root = ctx.context_root;
    let paths = vec!["shared.md".into(), "parent-only.md".into()];
    let want = || {
        step3::want_for_selection_for(&f.fixture.home, &me, &target, &paths, &[], &scope).unwrap()
    };
    let denied = want();
    assert_eq!(denied.files.len(), 1);
    assert_eq!(denied.files[0].dest, "shared.md");
    let output = f.fixture.temp.path().join("own-code-workspace");
    std::fs::create_dir(&output).unwrap();
    git2::Repository::init(&output).unwrap();
    step3::hydrate(step3::Hydrate {
        home: &f.fixture.home,
        principal: &me,
        destination: Destination::Local { root: &output },
        want: &denied,
        operation: "selected-checkout",
        workspace: "own".into(),
        agent: "checkout-child".into(),
        prune: false,
        overwrite_modified: false,
        progress: &|_| {},
        validate: &|| Ok(()),
        cancel: None,
        legacy: &|| Ok(Vec::new()),
    })
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(output.join("shared.md")).unwrap(),
        "parent-shared.md-bytes"
    );
    assert!(!output.join("parent-only.md").exists());
    std::fs::remove_file(checkout.join("parent/content/parent-only.md")).unwrap();
    assert_eq!(denied, want());
}

#[tokio::test]
async fn context_transfer_mcp_masks_hidden_destination_collisions_and_completed_writes() {
    use commands::context_mobility::MobilityOp;
    let f = AppFixture::new();
    f.session("transfer-child", Some("child"));
    let scope = crate::context_repo::caller_scope::for_session(
        f.app.handle(),
        Some("transfer-child"),
        None,
    )
    .await;
    let ctx = scope.context(f.app.handle()).await.unwrap();
    let source = f.fixture.path("child", "content/docs/plan.md");
    let target = f.fixture.path("parent", "content/docs/plan.md");
    let mut denied = None;
    for copy in [true, false] {
        for existing in [None, Some("hidden bytes"), Some("owned bytes")] {
            f.fixture
                .write("child", "content/docs/plan.md", "owned bytes");
            if let Some(text) = existing {
                f.fixture.write("parent", "content/docs/plan.md", text);
            } else if target.exists() {
                std::fs::remove_file(&target).unwrap();
            }
            let request = serde_json::from_value(serde_json::json!({
                "from":"team:acme/ctx:child", "to":"team:acme/ctx:parent", "logicalName":"docs/plan.md"
            })).unwrap();
            let error = run_context_mobility_for(
                ctx.clone(),
                if copy {
                    MobilityOp::Copy
                } else {
                    MobilityOp::Move
                },
                request,
                None,
                scope.clone(),
            )
            .await
            .unwrap_err();
            let error = serde_json::to_value(error).unwrap();
            if let Some(before) = &denied {
                assert_eq!(before, &error);
            } else {
                denied = Some(error);
            }
            assert_eq!(
                std::fs::read_to_string(&target).unwrap(),
                if existing == Some("hidden bytes") {
                    "hidden bytes"
                } else {
                    "owned bytes"
                }
            );
            assert_eq!(source.exists(), copy || existing == Some("hidden bytes"));
        }
    }
    for (from, to, logical, policy) in [
        ("parent", "child", "shared.md", "replace"),
        ("child", "parent", "shared.md", "replace"),
        ("child", "sibling", "docs/plan.md", "replace"),
    ] {
        if from == "child" && logical == "docs/plan.md" {
            f.fixture
                .write("child", "content/docs/plan.md", "owned bytes");
        }
        let result = run_context_mobility_for(ctx.clone(), MobilityOp::Copy,
            serde_json::from_value(serde_json::json!({"from":format!("team:acme/ctx:{from}"),"to":format!("team:acme/ctx:{to}"),"logicalName":logical,"policy":policy})).unwrap(),
            None, scope.clone()).await.unwrap();
        assert!(serde_json::to_value(result)
            .unwrap()
            .to_string()
            .contains("copied"));
    }
    // A visible collision may choose a suffix that is not shared. Its entire readback is masked too.
    f.fixture
        .write("child", "content/shared.md", "new shared bytes");
    f.fixture
        .write("parent", "content/shared.md", "old shared bytes");
    let result = run_context_mobility_for(ctx, MobilityOp::Copy,
        serde_json::from_value(serde_json::json!({"from":"team:acme/ctx:child","to":"team:acme/ctx:parent","logicalName":"shared.md","policy":"keep_both"})).unwrap(),
        None, scope).await.unwrap_err();
    assert_eq!(denied.unwrap(), serde_json::to_value(result).unwrap());
}

#[tokio::test]
async fn session_read_mcp_and_tauri_return_identical_json_for_each_launch_kind() {
    use crate::session_usage::{record::{hand_started,SessionRecord}, UsageStore};
    let f=AppFixture::new();
    let mut store=UsageStore::open(&f.fixture.home.join("usage.sqlite")).unwrap();
    let outcome=serde_json::from_value(serde_json::json!({
        "story_id":"SV-example","run_id":"cde14-run","status":"success","ended_at":"2026-10-27T00:00:00Z",
        "source":"signal","org":"acme/ctx","team":"child","session_ids":["linked"]
    })).unwrap();
    commands::sprint_run::run_store::record_run_outcome(outcome);
    for id in ["linked","unlinked","hand"] {
        let mut record=if id=="hand" {hand_started(id,Some("native-hand"),Some("acme/ctx"),Some("child"))} else {SessionRecord::new(id)};
        if id=="linked" {record.run_id=Some("cde14-run".into());}
        store.write_session_record(&record).unwrap();
        store.ingest(&[crate::session_usage::tests::event(id,id,"gpt-6-sol",1_790_000_000_000,crate::session_usage::event::Tokens{input:10,..Default::default()})]).unwrap();
        if id != "unlinked" {
            store.insert_task_if_absent(&crate::session_usage::task::TaskRecord::new(id,format!("Work {id}"))).unwrap();
        }
        let expected_tasks=serde_json::json!(store.tasks_for_session(id).unwrap());
        let params=|| serde_json::from_value(serde_json::json!({"sessionId":id,"include":["conversation"],"cursor":null,"limit":null,"turn":3,"toolUseId":null})).unwrap();
        let mcp=session_read_request(params(),&CallerScope::Human).await.unwrap();
        let json:serde_json::Value=serde_json::from_str(&mcp.content[0].as_text().unwrap().text).unwrap();
        let page=commands::data_read_model::session_read(f.app.handle().clone(),params(),None,None).await.unwrap();
        assert_eq!(json,page,"{id}");
        assert_eq!(page["tasks"],expected_tasks,"{id}");
        if id=="hand" {
            let native=|| serde_json::from_value(serde_json::json!({"sessionId":"native-hand","include":["conversation"],"cursor":null,"limit":null,"turn":null,"toolUseId":null})).unwrap();
            let native_mcp=session_read_request(native(),&CallerScope::Human).await.unwrap();
            let native_json:serde_json::Value=serde_json::from_str(&native_mcp.content[0].as_text().unwrap().text).unwrap();
            let native_page=commands::data_read_model::session_read(f.app.handle().clone(),native(),None,None).await.unwrap();
            assert_eq!(native_json,page); assert_eq!(native_page,page);
        }
        assert_eq!(page["run"]["state"],if id=="linked" {"present"} else {"unlinked"});
        if id=="linked" {assert_eq!(page["run"]["outcome"]["outcome"]["status"],"success");}
        assert_eq!(page["conversation"]["state"],"absent");
    }
    let schema=serde_json::to_value(rmcp::schemars::schema_for!(crate::session_usage::session_read::Params)).unwrap();
    super::super::identity_guard::check_tool("session_read",&schema).unwrap();
}

#[test]
fn why_line_is_in_inventory_with_committed_span_schema_and_read_only_description() {
    let router = CodesenseHandler::served_tool_router();
    let tool = router.get("why_line").expect("why_line is served");
    let schema = serde_json::to_value(&tool.input_schema).unwrap();
    for field in ["repo", "path", "start", "end"] {
        assert!(schema["required"].as_array().unwrap().contains(&serde_json::json!(field)));
    }
    assert!(!schema["required"].as_array().unwrap().contains(&serde_json::json!("rev")));
    assert!(schema["properties"]["rev"]["default"].is_null());
    assert!(schema["properties"].get("story").is_some());
    assert!(schema["properties"].get("sessionId").is_none());
    let description = tool.description.as_deref().unwrap();
    for phrase in ["Read-only", "local copy", "no network", "Every local agent"] {
        assert!(description.contains(phrase), "missing {phrase}");
    }
    assert!(description.chars().count() <= 4096);
}

#[tokio::test]
async fn why_line_seeded_mcp_call_returns_joined_session_facts_without_conversation() {
    use crate::session_usage::{record::SessionRecord, UsageStore};
    let f = AppFixture::new();
    let repo = crate::attribution::git_ai::tests::rebuild();
    crate::attribution::why_line::tests::attach(&f.fixture.home, repo.path());
    // An omitted rev reads origin's default branch, never the checkout's HEAD.
    crate::attribution::git_ai::tests::git(repo.path(), &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let store = UsageStore::open(&f.fixture.home.join("usage.sqlite")).unwrap();
    let mut record = SessionRecord::new("why-line-tab");
    record.native_session_id = Some(crate::attribution::git_ai::tests::CLAUDE.into());
    record.cli = Some("claude".into()); record.story_id = Some("SV-CDE-13".into());
    store.write_session_record(&record).unwrap();
    drop(store);
    let p = serde_json::from_value(serde_json::json!({"repo":"acme/api","path":"a.txt","start":1,"end":3})).unwrap();
    let response = why_line_request(&f.fixture.home, &Principal::from(1), p, &CallerScope::Human, Default::default()).await.unwrap();
    let result: serde_json::Value = serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(result["ranges"][0]["link"], "linked");
    assert_eq!(result["ranges"][0]["sessionId"], "why-line-tab");
    assert_eq!(result["sessions"].as_object().unwrap().len(), 1);
    assert_eq!(result["sessions"]["why-line-tab"]["session"]["sessionId"], "why-line-tab");
    assert!(result["sessions"]["why-line-tab"].get("conversation").is_none());
    assert!(result.get("unavailable").is_none());
    assert_eq!(result["ranges"][0]["unavailable"]["turn"],crate::attribution::turns::LOG_UNAVAILABLE);
}

#[tokio::test]
async fn story_changes_mcp_tauri_and_published_catalog_read_durable_stories() {
    use crate::session_usage::{UsageStore, StoryLink, record::SessionRecord, tests::event, event::Tokens};
    let router = CodesenseHandler::served_tool_router();
    let tool = router.get("story_changes").expect("story_changes inventory");
    let description = tool.description.as_deref().unwrap();
    assert!(description.chars().count() <= 4096, "story_changes served description length");
    for field in ["linesReason", "urlReason", "mergedAtReason", "author"] {
        assert!(description.contains(field), "story_changes documents {field}");
    }
    let f = AppFixture::new();
    let home = &f.fixture.home;
    let mut store = UsageStore::open(&home.join("usage.sqlite")).unwrap();
    for (index, state) in ["in-progress", "done", "removed"].iter().enumerate() {
        let id = format!("story-tab-{index}");
        let story = format!("SV-29-{index}");
        let path = home.join(format!("story-{index}.md"));
        std::fs::write(&path, format!("---\nstatus: {state}\n---\n")).unwrap();
        let worktree = home.join(format!("worktree-{index}"));
        std::fs::create_dir(&worktree).unwrap();
        let mut record = SessionRecord::new(&id);
        record.worktree = Some(worktree.to_string_lossy().into());
        store.write_session_record(&record).unwrap();
        store.link_story(&id, &StoryLink{story_id:story.clone(),source:path,org:"acme/ctx".into()}).unwrap();
        let mut e = event(&id,&id,"gpt-6-sol",1_790_000_000_000,Tokens{input:10,..Default::default()});
        e.reported_cost_usd = Some(3.0);
        store.ingest(&[e]).unwrap();
        if *state == "removed" {std::fs::remove_dir(&worktree).unwrap();}
        let params = || serde_json::from_value(serde_json::json!({"storyId":story})).unwrap();
        let mcp = story_changes_request(params(), &CallerScope::Human).await.unwrap();
        let json:serde_json::Value = serde_json::from_str(&mcp.content[0].as_text().unwrap().text).unwrap();
        // One backend: MCP is the agent projection of the bridge read of the agent's params.
        let agent = crate::session_usage::story_changes::agent_read_params(&params());
        let page = commands::data_read_model::story_changes(f.app.handle().clone(),agent,None,None).await.unwrap();
        assert_eq!(json,crate::session_usage::story_changes::agent_projection(page.clone(),&params()).unwrap());
        assert_eq!(json["totals"],page["totals"]);
        assert_eq!(page["totals"]["sessions"],1);
        assert_eq!(page["totals"]["costUsd"],3.0);
        #[cfg(feature = "dashboard-publish")]
        {
        let catalog = crate::dashboard_publish::catalog::find(crate::dashboard_publish::catalog::CATALOG,"storyChanges").expect("published storyChanges catalog");
        let published = (catalog.run)(Default::default(), crate::dashboard_publish::catalog::Call{args:serde_json::json!({"storyId":story}),..Default::default()}).await.unwrap();
        assert_eq!(published["totals"],page["totals"]);
        assert!(published["sessions"][0]["session"].is_null(),"record paths are not published");
        }
    }
}

#[cfg(feature = "dashboard-publish")]
#[tokio::test]
async fn story_changes_published_catalog_dispatches_without_default_usage_cap() {
    use crate::session_usage::{UsageStore,StoryLink,record::SessionRecord,tests::event,event::Tokens};
    let f=AppFixture::new();
    let home=&f.fixture.home;
    let mut store=UsageStore::open(&home.join("usage.sqlite")).unwrap();
    for index in 0..51 {
        let id=format!("catalog-tab-{index}");
        let mut r=SessionRecord::new(&id);
        r.worktree=Some("/private/worktree".into());
        store.write_session_record(&r).unwrap();
        store.link_story(&id,&StoryLink{story_id:"SV-catalog".into(),source:home.join("story.md"),org:"acme/ctx".into()}).unwrap();
        let mut e=event(&id,&id,"gpt-6-sol",1_790_000_000_000,Tokens{input:10,..Default::default()});
        e.reported_cost_usd=Some(1.0);
        store.ingest(&[e]).unwrap();
    }
    use crate::session_usage::{task::TaskRecord, timing::{TimingState,Stamped,Signal}};
    use codesense_pty_core::activity::Event;
    let mut timing = TimingState::default();
    let private_note = "Private progress about the unreleased customer contract";
    for (at_ms, signal) in [
        (100, Signal::Activity(Event::Submitted, "fixture")),
        (150, Signal::Progress {phase:"blocked".into(),note:Some(private_note.into())}),
        (200, Signal::Activity(Event::Stopped, "fixture")),
        (300, Signal::Activity(Event::Ended, "fixture")),
    ] {
        timing.apply(&store,&Stamped {session_id:"catalog-tab-0".into(),at_ms,signal}).unwrap();
    }
    assert_eq!(store.read_session_timing("catalog-tab-0").unwrap().unwrap().idle_gaps[0].note.as_deref(),Some(private_note));
    for (index,id,parent) in [(1,"catalog-parent",None),(2,"catalog-dev",Some("catalog-parent")),(3,"catalog-review",Some("catalog-dev"))] {
        let mut task = TaskRecord::new(id,"Private task goal for the customer contract");
        task.story_id=Some("SV-catalog".into());
        task.worktree=Some("/private/task-worktree".into());
        task.parent_id=parent.map(str::to_owned);
        task.created_at_ms=index;
        if id=="catalog-review" { task.task_type="review".into(); }
        store.insert_task_if_absent(&task).unwrap();
    }
    store.add_task_link("catalog-review","relates_to:reviews","catalog-dev").unwrap();
    let entry=crate::dashboard_publish::catalog::find(crate::dashboard_publish::catalog::CATALOG,"storyChanges").expect("published storyChanges entry");
    let answer=(entry.run)(Default::default(),crate::dashboard_publish::catalog::Call{args:serde_json::json!({"storyId":"SV-catalog"}),..Default::default()}).await.unwrap();
    assert_eq!(answer["totals"]["sessions"],51);
    assert_eq!(answer["sessions"].as_array().unwrap().len(),51);
    assert_eq!(answer["totals"]["costUsd"],51.0);
    let session=&answer["sessions"][0];
    assert_eq!(session["sessionId"],"catalog-tab-0");
    assert_eq!(session["basis"],"exact");
    assert_eq!(session["models"][0]["model"],"gpt-6-sol");
    assert_eq!(session["cost"]["totalUsd"],1.0);
    assert_eq!(session["timing"],serde_json::json!({"startedAtMs":100,"endedAtMs":300,"wallMs":200,"activeMs":100,"idleMs":100,"turnCount":1,"toolCallCount":0,"timingSource":"fixture"}));
    let dev=answer["tasks"].as_array().unwrap().iter().find(|t|t["taskId"]=="catalog-dev").unwrap();
    assert_eq!(dev["parentId"],"catalog-parent");
    assert_eq!(dev["reviews"],serde_json::json!(["catalog-review"]));
    assert_eq!(answer["totals"]["reviewTasks"],1);
    let json=answer.to_string();
    for private in ["/private/worktree","/private/task-worktree",private_note,"Private task goal","idleGaps","note"] {
        assert!(!json.contains(private),"published private field: {private}");
    }
}

#[tokio::test]
async fn why_line_mcp_page_parity_keeps_session_joins_live_after_warming_cache() {
    use crate::attribution::git_ai::tests::{git, rebuild, CLAUDE};
    use crate::session_usage::{
        record::{RecordAccount, SessionRecord},
        UsageStore,
    };
    let f = AppFixture::new();
    let repo = rebuild();
    crate::attribution::why_line::tests::attach(&f.fixture.home, repo.path());
    // An omitted rev reads origin's default branch, never the checkout's HEAD.
    git(repo.path(), &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let p = || {
        serde_json::from_value(
            serde_json::json!({"repo":"acme/api","path":"a.txt","start":1,"end":3}),
        )
        .unwrap()
    };
    async fn pair(f: &AppFixture, p: crate::attribution::why_line::Params) -> serde_json::Value {
        let mcp = why_line_request(
            &f.fixture.home,
            &Principal::from(1),
            p.clone(),
            &CallerScope::Human,
            Default::default(),
        )
        .await
        .unwrap();
        let expected: serde_json::Value =
            serde_json::from_str(&mcp.content[0].as_text().unwrap().text).unwrap();
        let page = commands::data_read_model::why_line(f.app.handle().clone(), p, None, None)
            .await
            .unwrap();
        assert_eq!(page, expected);
        page
    }
    let warm = pair(&f, p()).await;
    assert_eq!(warm["ranges"][0]["link"], "unlinked");
    assert!(f.fixture.home.join("caches/why-line.sqlite3").exists());
    let store = UsageStore::open(&f.fixture.home.join("usage.sqlite")).unwrap();
    let mut record = SessionRecord::new("cached-page-session");
    record.native_session_id = Some(CLAUDE.into());
    record.cli = Some("claude".into());
    record.worktree = Some("/private/local/worktree".into());
    record.story_id = Some("SV-CDE-13".into());
    record.account = Some(RecordAccount {
        provider: Some("fixture".into()),
        account_id: Some("private-account".into()),
        label: Some("owner@example.com".into()),
        source: Some("fixture".into()),
    });
    store.write_session_record(&record).unwrap();
    drop(store);
    let linked = pair(&f, p()).await;
    assert_eq!(linked["ranges"][0]["link"], "linked");
    assert_eq!(linked["ranges"][0]["sessionId"], "cached-page-session");
    assert_eq!(linked["sessions"].as_object().unwrap().len(), 1);
    assert_eq!(
        linked["sessions"]["cached-page-session"]["session"]["account"]["accountId"],
        "private-account"
    );
    crate::attribution::turns::tests::checkpoint_log(repo.path(),"a.txt",&["toolu_01CCC"]);
    crate::attribution::turns::tests::capture(&f.fixture.home,&f.fixture.home.join("session-conversations"),"cached-page-session",None,&Default::default());
    let pointed=pair(&f,p()).await;
    assert_eq!(pointed["ranges"][0]["turn"],serde_json::json!({"sessionId":"cached-page-session","toolUseId":"toolu_01CCC","turn":2,"basis":"edit","taskId":"cached-page-session","taskBasis":"session"}));
    let mut codex = p();
    codex.path = "b.txt".into();
    assert_eq!(pair(&f, codex).await["ranges"][0]["link"], "unlinked");
    std::fs::write(repo.path().join("associated.txt"), "story change\n").unwrap();
    git(repo.path(), &["add", "associated.txt"]);
    git(
        repo.path(),
        &["commit", "-qm", "SV-CDE-13: associated change (#2708)"],
    );
    git(repo.path(), &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let mut associated = p();
    associated.path = "associated.txt".into();
    associated.end = 1;
    assert_eq!(
        pair(&f, associated).await["ranges"][0]["basis"],
        "associated"
    );
    let mut unavailable = p();
    unavailable.rev = Some("not-a-local-revision".into());
    assert_eq!(pair(&f, unavailable).await["status"], "unavailable");
}

#[test]
fn why_line_page_verb_is_read_only_and_has_its_mcp_twin_and_schema() {
    let registry = crate::verbs::builtin_registry();
    let spec = registry.get("whyLine").expect("whyLine is registered");
    assert_eq!(spec.mcp_twin.as_deref(), Some("why_line"));
    assert_eq!(spec.grant, "sessions:read");
    assert!(!spec.action);
    assert!(spec.args_schema["required"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("repo")));
    assert!(spec.args_schema["properties"]["rev"]["default"].is_null());
    assert!(spec.args_schema["properties"].get("story").is_some());
}

/// `setup` entry paths: describe_setup, the in-app page command and the published entry read
/// the same Doctor; the published answer is the allow-listed subset.
#[cfg(feature = "dashboard-publish")]
#[tokio::test]
async fn setup_in_app_published_and_describe_setup_agree() {
    use crate::dashboard_publish::catalog::{find, Call, CATALOG};
    let f = AppFixture::new();
    let handle = f.app.handle().clone();
    let described = serde_json::to_value(setup_tools::describe_setup(&handle).await.unwrap()).unwrap();
    let page = commands::data_read_model::setup(handle.clone()).await.unwrap();
    assert_eq!(page["attribution"], described["attribution"]);
    assert_eq!(page.as_object().unwrap().len(), 1, "nothing but attribution");
    let entry = find(CATALOG, "setup").expect("published setup entry");
    let ctx = crate::dashboard_publish::catalog::LiveCtx::default();
    assert!((entry.run)(ctx, Call::default()).await.is_err(), "no app, no answer");
    let shared = crate::dashboard_publish::catalog::setup_answer_for_test(handle.clone(), serde_json::Value::Null)
        .await
        .unwrap();
    let a = &shared["attribution"];
    let full = &described["attribution"];
    for key in crate::attribution::setup::SHARED_DOCTOR_FIELDS {
        assert!(a.get(*key).is_some(), "{key}");
    }
    assert_eq!(a["capture"], full["capture"]);
    assert_eq!(a["status"]["kind"], full["status"]["kind"]);
    assert_eq!(a["gitAiVersion"], full["gitAiVersion"]);
    for key in crate::attribution::setup::EXCLUDED_DOCTOR_FIELDS {
        assert!(a.get(*key).is_none(), "{key} is not shared");
    }
    let bad = crate::dashboard_publish::catalog::setup_answer_for_test(handle, serde_json::json!({"path": "/x"})).await;
    assert!(matches!(bad, Err(crate::dashboard_publish::catalog::QueryError::InvalidArgs(_))));
}

/// The dashboard `setup` reads (in-app and published) leave a pending replacement notice alone;
/// `describe_setup` still announces it exactly once.
#[cfg(feature = "dashboard-publish")]
#[tokio::test]
async fn setup_queries_do_not_consume_the_replacement_notice() {
    let f = AppFixture::new();
    let handle = f.app.handle().clone();
    let receipt = crate::attribution::setup::tests::plant_pending_notice(&f.fixture.home);
    let before = std::fs::read(&receipt).unwrap();
    for _ in 0..2 {
        let page = commands::data_read_model::setup(handle.clone()).await.unwrap();
        assert!(page.to_string().contains("replaced git-ai 1.7.4"), "in-app shows the notice");
        let shared = crate::dashboard_publish::catalog::setup_answer_for_test(handle.clone(), serde_json::Value::Null)
            .await
            .unwrap();
        assert!(!shared.to_string().contains("replaced git-ai"));
        assert_eq!(std::fs::read(&receipt).unwrap(), before, "receipt byte-identical");
    }
    let announced = |state: &setup_tools::SetupState| state.attribution.warnings.iter().any(|w| w.starts_with("replaced git-ai 1.7.4"));
    assert!(announced(&setup_tools::describe_setup(&handle).await.unwrap()));
    assert!(!announced(&setup_tools::describe_setup(&handle).await.unwrap()), "exactly once");
}
