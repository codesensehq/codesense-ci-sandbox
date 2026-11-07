// SPDX-License-Identifier: MIT
use super::*;
use crate::context_repo::caller_scope::tests::AppFixture;
use serde_json::json;
use tauri::Manager;

#[test]
fn child_query_mcp_and_app_bridge_filter_explicit_parent_before_counts_and_bodies() {
    let f = AppFixture::new();
    let service = f.fixture.service();
    f.app.manage(service.clone());
    let frame = f.frame(&f.fixture.path("parent", "panes/parent.html"), "workspace");
    f.app
        .state::<crate::mcp::state::ActiveTeamState>()
        .update(crate::mcp::state::ActiveTeam {
            org_slug: "acme/ctx".into(),
            team_name: "parent".into(),
        });
    let params: QueryContextParams = serde_json::from_value(
        json!({"where":{"teams":["parent"]},"text":"bytes","groupBy":"team"}),
    )
    .unwrap();
    let viewer = Viewer {
        scope: f.fixture.scope.clone(),
        ..Default::default()
    };
    let mcp = run_as(&service, &params.to_query().unwrap(), &viewer).unwrap();
    let page_viewer = Viewer {
        page: Some(f.fixture.path("child", "content").display().to_string()),
        ..Default::default()
    };
    let published = run_as(&service, &params.to_query().unwrap(), &page_viewer).unwrap();
    assert_eq!(
        serde_json::to_value(&mcp).unwrap(),
        serde_json::to_value(published).unwrap()
    );
    assert_eq!(mcp.rows.len(), 1);
    assert_eq!(mcp.rows[0].file_name, "shared.md");
    assert!(!serde_json::to_string(&mcp).unwrap().contains("secret"));
    let bridge = tauri::async_runtime::block_on(crate::commands::context_query::query_context(
        f.app.handle().clone(),
        params.to_query().unwrap(),
        frame.document_path.clone(),
        Some(frame.frame_id.clone()),
        None,
        None,
    ))
    .unwrap();
    assert_eq!(
        serde_json::to_value(&mcp).unwrap(),
        serde_json::to_value(&bridge).unwrap()
    );
    let query = |name| {
        serde_json::from_value::<ContextQuery>(
            json!({"where":{"pathPrefix": f.fixture.path("parent", name)}}),
        )
        .unwrap()
    };
    let denied = run_as(&service, &query("content/secret.md"), &viewer).unwrap();
    let missing = run_as(&service, &query("content/missing.md"), &viewer).unwrap();
    assert_eq!(
        serde_json::to_value(denied).unwrap(),
        serde_json::to_value(missing).unwrap()
    );
    let before = serde_json::to_value(mcp).unwrap();
    std::fs::remove_file(f.fixture.path("parent", "content/secret.md")).unwrap();
    let mut index = crate::context_query::index::IndexState::default();
    index.reconcile(f.fixture.roots());
    let absent = index
        .snapshot(service.snapshot().generation, true)
        .query(
            &params.to_query().unwrap(),
            &viewer,
            &context_query::engine::DiskBodyReader,
        )
        .unwrap();
    assert_eq!(before, serde_json::to_value(absent).unwrap());
}

#[test]
fn mcp_query_request_resolves_recorded_session_before_consuming_the_index() {
    use crate::commands::dashboard_launch::{DashboardLaunchState, LaunchTeam};
    use crate::install::worktree_index::{self, ContextWorktreeRecord};
    let f = AppFixture::new();
    let private = crate::context_repo::scratch_space::ensure_org_space(
        &f.fixture.home,
        &crate::domain::Principal::from(1u64),
        "acme/ctx",
    )
    .unwrap()
    .0;
    std::fs::create_dir_all(private.content_dir()).unwrap();
    std::fs::write(private.content_dir().join("private-positive.md"), "private").unwrap();
    let mut roots = f.fixture.roots();
    roots.push(crate::context_query::index::IndexRoot {
        dir: private.content_dir(),
        org: Some("acme/ctx".into()),
        team: None,
        layer: crate::context_query::model::Layer::Private,
    });
    f.app.manage(f.fixture.service_with(roots));
    f.app.manage(DashboardLaunchState::new(8));
    f.app
        .state::<crate::mcp::state::ActiveTeamState>()
        .update(crate::mcp::state::ActiveTeam {
            org_slug: "acme/ctx".into(),
            team_name: "parent".into(),
        });
    let state = f.app.state::<DashboardLaunchState>();
    for (handle, session) in [
        ("launched", "launch-session"),
        ("retained", "worktree-session"),
        ("unknown", "unknown-session"),
    ] {
        state.register(handle, session, Some("codesense"));
    }
    state.set_launch_team_if_absent(
        "launched",
        LaunchTeam {
            org: "acme/ctx".into(),
            team: "child".into(),
        },
    );
    let root = f.fixture.temp.path().join("retained");
    let cwd = root.join("child/content");
    std::fs::create_dir_all(&cwd).unwrap();
    worktree_index::write_context_record(
        &f.fixture.home,
        &crate::domain::Principal::from(1u64),
        &ContextWorktreeRecord {
            owner: None,
            extras: Default::default(),
            schema_version: worktree_index::SCHEMA_VERSION,
            kind: "context".into(),
            id: "retained-child".into(),
            org: "acme/ctx".into(),
            team: "child".into(),
            name: "Retained".into(),
            branch: "work/child".into(),
            base_sha: "0".repeat(40),
            root,
            launch_cwd: cwd.clone(),
            created_at: 1,
            last_used_at: 1,
        },
    )
    .unwrap();
    state.set_worktree_dir_if_absent("retained", cwd);
    let read = |session: &str, path: Option<&str>| {
        let query = match path {
            Some(path) => {
                serde_json::json!({"where":{"pathPrefix": f.fixture.path("parent", path)}})
            }
            None => {
                serde_json::json!({"where":{"teams":["parent"]},"text":"bytes","groupBy":"team"})
            }
        };
        let params = serde_json::from_value(query).unwrap();
        let reply = tauri::async_runtime::block_on(request(
            f.app.handle().clone(),
            Some(session.into()),
            match session {
                "launch-session" | "worktree-session" => Some(LaunchTeam {
                    org: "acme/ctx".into(),
                    team: "parent".into(),
                }),
                "remote-only" => Some(LaunchTeam {
                    org: "acme/ctx".into(),
                    team: "child".into(),
                }),
                _ => None,
            },
            params,
        ))
        .unwrap();
        let rmcp::model::ContentBlock::Text(text) = &reply.content[0] else {
            panic!("not text")
        };
        serde_json::from_str::<serde_json::Value>(&text.text).unwrap()
    };
    for session in ["launch-session", "worktree-session", "remote-only"] {
        let result = read(session, None);
        assert_eq!(result["rows"].as_array().unwrap().len(), 1);
        assert!(result.to_string().contains("shared.md"));
        assert!(!result.to_string().contains("secret.md"));
        assert_eq!(
            read(session, Some("content/secret.md")),
            read(session, Some("content/missing.md"))
        );
    }
    assert!(read("unknown-session", None)["rows"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(read("hand-started", None).to_string().contains("secret.md"));
    for session in ["launch-session", "worktree-session", "unknown-session"] {
        let params = serde_json::from_value(
            serde_json::json!({"where":{"orgs":["acme/ctx"],"layers":["private"]}}),
        )
        .unwrap();
        let reply = tauri::async_runtime::block_on(request(
            f.app.handle().clone(),
            Some(session.into()),
            None,
            params,
        ))
        .unwrap();
        let text = serde_json::to_string(&reply).unwrap();
        assert!(text.contains("private-positive.md"));
    }
}

#[test]
fn inherited_native_markdown_query_uses_viewer_host_but_retains_parent_source_path() {
    let f = AppFixture::new();
    let page = f.fixture.write(
        "parent",
        "content/shared.md",
        "```csquery\nwhere:\n  teams: [parent]\n```\n",
    );
    f.app.manage(f.fixture.service());
    let query: ContextQuery =
        serde_json::from_value(serde_json::json!({"where":{"teams":["parent"]},"groupBy":"team"}))
            .unwrap();
    let read = |host: &str, query: ContextQuery| {
        tauri::async_runtime::block_on(crate::commands::context_query::query_context(
            f.app.handle().clone(),
            query,
            page.display().to_string(),
            None,
            Some(f.fixture.path(host, "content").display().to_string()),
            None,
        ))
        .unwrap()
    };
    let child = read("child", query.clone());
    assert_eq!(child.rows.len(), 1);
    assert_eq!(child.rows[0].file_name, "shared.md");
    assert!(!serde_json::to_string(&child).unwrap().contains("secret.md"));
    let selected = |name| {
        serde_json::from_value(
            serde_json::json!({"where":{"pathPrefix":f.fixture.path("parent", name)}}),
        )
        .unwrap()
    };
    assert_eq!(
        serde_json::to_value(read("child", selected("content/secret.md"))).unwrap(),
        serde_json::to_value(read("child", selected("content/missing.md"))).unwrap()
    );
    assert!(read("parent", query)
        .rows
        .iter()
        .any(|r| r.file_name == "secret.md"));
}
