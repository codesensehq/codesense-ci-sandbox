// SPDX-License-Identifier: MIT
use super::*;
use crate::context_repo::caller_scope::tests::AppFixture;
use tauri::Manager;

#[test]
fn shared_parent_pane_open_file_retains_child_frame_scope_when_active_team_changes() {
    let f = AppFixture::new();
    f.app.manage(f.fixture.service());
    let frame = f.frame(&f.fixture.path("parent", "panes/parent.html"), "workspace");
    f.app
        .state::<crate::mcp::state::ActiveTeamState>()
        .update(crate::mcp::state::ActiveTeam {
            org_slug: "acme/ctx".into(),
            team_name: "parent".into(),
        });
    tauri::async_runtime::block_on(async {
        let open = |name: &str| {
            bridge_resolve_open_target(
                f.app.handle().clone(),
                f.fixture
                    .path("child", "content/page.html")
                    .to_string_lossy()
                    .into(),
                name.into(),
                Some("parent".into()),
                Some(frame.frame_id.clone()),
            )
        };
        assert_eq!(
            open("secret.md").await.unwrap_err(),
            open("missing.md").await.unwrap_err()
        );
        let target = open("shared.md").await.unwrap();
        assert_eq!(
            std::fs::read_to_string(target.path).unwrap(),
            "parent-shared.md-bytes"
        );
        let roots = f.fixture.roots();
        let page = f
            .fixture
            .path("child", "content/page.html")
            .to_string_lossy()
            .into_owned();
        assert!(resolve_open_target_for(
            &roots,
            &page,
            "secret.md",
            Some("sibling"),
            &f.fixture.scope
        )
        .is_ok());
    });
}
