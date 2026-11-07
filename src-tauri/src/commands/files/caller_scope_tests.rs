// SPDX-License-Identifier: MIT
use super::*;
use crate::context_repo::caller_scope::tests::AppFixture;

#[test]
fn absolute_parent_paths_validate_and_read_only_shared_content() {
    let f = AppFixture::new();
    let denied = f.fixture.path("parent", "content/secret.md");
    let missing = f.fixture.path("parent", "content/missing.md");
    let shared = f.fixture.path("parent", "content/shared.md");
    assert_eq!(
        validate_context_path(&denied, f.app.handle()),
        validate_context_path(&missing, f.app.handle())
    );
    assert!(validate_context_path(&shared, f.app.handle()).is_ok());
    let other = f.fixture.home.join("users/1/orgs/other/ctx");
    std::fs::create_dir_all(other.join("parent/content")).unwrap();
    std::fs::write(
        other.join("org.yml"),
        "slug: other/ctx\ndisplay_name: Other\ncreated_at: '2026-01-01T00:00:00Z'\n",
    )
    .unwrap();
    std::fs::write(other.join("teams.yml"), "teams:\n  - name: parent\n").unwrap();
    let other_file = other.join("parent/content/private.md");
    std::fs::write(&other_file, "other-org-content").unwrap();
    tauri::async_runtime::block_on(async {
        let read = |path: PathBuf| {
            read_file_snapshot_for(
                f.app.handle().clone(),
                path.to_string_lossy().into(),
                None,
                f.fixture.scope.clone(),
            )
        };
        assert_eq!(
            read(denied).await.unwrap_err(),
            read(missing).await.unwrap_err()
        );
        let result = read(shared).await.unwrap();
        assert_eq!(result["text"], "parent-shared.md-bytes");
        assert_eq!(read(other_file).await.unwrap()["text"], "other-org-content");
        assert_eq!(
            read(f.fixture.path("sibling", "content/secret.md"))
                .await
                .unwrap()["text"],
            "sibling-secret.md-bytes"
        );
    });
}

#[test]
fn workspace_reader_uses_host_frame_and_filters_adjacent_parent_files() {
    use tauri::Manager;
    let f = AppFixture::new();
    let page = f
        .fixture
        .write("parent", "content/board.html", "<title>Board</title>");
    let frame = f.frame(&page, "workspace");
    f.app
        .state::<crate::mcp::state::ActiveTeamState>()
        .update(crate::mcp::state::ActiveTeam {
            org_slug: "acme/ctx".into(),
            team_name: "parent".into(),
        });
    let read = |path: &str| {
        tauri::async_runtime::block_on(read_workspace_file(
            f.app.handle().clone(),
            page.display().to_string(),
            path.into(),
            vec!["md".into(), "html".into()],
            Some(frame.frame_id.clone()),
        ))
    };
    assert_eq!(
        read("secret.md").unwrap_err(),
        read("missing.md").unwrap_err()
    );
    assert_eq!(
        read("shared.md").unwrap().content.unwrap(),
        "parent-shared.md-bytes"
    );
    let rows = serde_json::to_value(read("./").unwrap())
        .unwrap()
        .to_string();
    assert!(rows.contains("shared.md"));
    assert!(!rows.contains("secret.md"));
    assert!(!rows.contains("board.html"));
}
