// SPDX-License-Identifier: MIT
use super::*;
use crate::context_repo::caller_scope::tests::AppFixture;
use tauri::Listener;

#[test]
fn rendered_parent_document_denies_like_missing_before_dispatch_and_shared_dispatches() {
    let f = AppFixture::new();
    let main = tauri::WebviewWindowBuilder::new(&f.app, "main", tauri::WebviewUrl::default())
        .build()
        .unwrap();
    let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
    let seen = observed.clone();
    main.listen("document:read-rendered-request", move |event| {
        seen.lock()
            .unwrap()
            .push(serde_json::from_str(event.payload()).unwrap())
    });
    let hidden = f.fixture.write(
        "parent",
        "content/secret.html",
        "<title>Private sentinel</title>",
    );
    let missing = f.fixture.path("parent", "content/missing.html");
    let shared = f
        .fixture
        .write("parent", "content/shared.html", "<title>Shared</title>");
    f.fixture.write(
        "parent",
        "config/sharing.yml",
        "share:\n  - content/shared.html\n",
    );
    let scope = crate::context_repo::caller_scope::CallerScope::Team(
        crate::context_repo::inheritance::team_view(&f.fixture.root, "acme/ctx", "child"),
    );
    let dispatch = |path: &std::path::Path| {
        dispatch_rendered_document_request(
            f.app.handle(),
            &path.to_string_lossy(),
            &RenderedDocumentFormat::Text,
            "read",
            &scope,
        )
    };
    let denied = dispatch(&hidden).unwrap_err();
    let absent = dispatch(&missing).unwrap_err();
    assert_eq!(denied.code, absent.code);
    assert_eq!(denied.message, absent.message);
    assert!(observed.lock().unwrap().is_empty());
    dispatch(&shared).unwrap();
    assert_eq!(observed.lock().unwrap().len(), 1);
    assert_eq!(
        observed.lock().unwrap()[0]["path"],
        shared.to_string_lossy().as_ref()
    );
}

#[test]
fn epic_repos_request_uses_child_session_while_parent_is_active() {
    let f = AppFixture::new();
    f.session("child-session", Some("child"));
    f.app
        .state::<crate::mcp::state::ActiveTeamState>()
        .update(crate::mcp::state::ActiveTeam {
            org_slug: "acme/ctx".into(),
            team_name: "parent".into(),
        });
    for name in ["secret.md", "shared.md"] {
        f.fixture.write(
            "parent",
            &format!("content/{name}"),
            "---\ntype: epic\nrepos: [acme/code]\n---\n# Epic\n",
        );
    }
    let read = |name| {
        tauri::async_runtime::block_on(read_epic_repos_request(
            f.app.handle().clone(),
            Some("child-session".into()),
            None,
            f.fixture.path("parent", name).display().to_string(),
        ))
    };
    assert_eq!(
        read("content/secret.md").unwrap_err(),
        read("content/missing.md").unwrap_err()
    );
    assert_eq!(read("content/shared.md").unwrap(), vec!["acme/code"]);
}
