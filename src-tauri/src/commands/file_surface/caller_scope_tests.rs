// SPDX-License-Identifier: MIT
use super::*;
use crate::context_repo::caller_scope::tests::AppFixture;

#[test]
fn non_team_frame_selecting_parent_keeps_child_authority_even_without_optional_view() {
    let f = AppFixture::new();
    f.app.manage(f.fixture.service());
    let frame = f.frame(&f.fixture.path("parent", "panes/parent.html"), "workspace");
    tauri::async_runtime::block_on(async {
        let args = ListFiles {
            team: Some("parent".into()),
            ..Default::default()
        };
        let (mut access, _) = access(f.app.handle(), &args, Some(&frame.frame_id))
            .await
            .unwrap();
        assert_eq!(access.own, f.fixture.path("parent", "content"));
        let denied = f.fixture.path("parent", "content/secret.md");
        let missing = f.fixture.path("parent", "content/missing.md");
        let shared = f.fixture.path("parent", "content/shared.md");
        assert_eq!(access.admits_path(&denied), access.admits_path(&missing));
        assert!(access.admits_path(&shared).is_ok());
        access.view = None;
        assert_eq!(access.admits_path(&denied), access.admits_path(&missing));
        assert!(access.admits_path(&shared).is_ok());
        let listed = call(
            f.app.handle().clone(),
            "list",
            json!({"team":"parent"}),
            Some(frame.frame_id.clone()),
        )
        .await
        .unwrap();
        assert_eq!(listed["rows"].as_array().unwrap().len(), 1);
        assert_eq!(listed["rows"][0]["name"], "shared.md");
        let read = |path: PathBuf| {
            call(
                f.app.handle().clone(),
                "read",
                json!({"path":path}),
                Some(frame.frame_id.clone()),
            )
        };
        assert_eq!(
            read(denied).await.unwrap_err(),
            read(missing).await.unwrap_err()
        );
        assert_eq!(
            read(shared).await.unwrap()["text"],
            "parent-shared.md-bytes"
        );
        assert_eq!(
            read(f.fixture.path("sibling", "content/secret.md"))
                .await
                .unwrap()["text"],
            "sibling-secret.md-bytes"
        );
        assert_eq!(
            destination(&access, "new-private.md").unwrap_err(),
            NOT_FOUND
        );
    });
}

#[tokio::test]
async fn session_mutations_gate_delivery_and_mask_unreadable_outcomes() {
    let f = AppFixture::new();
    f.app.manage(f.fixture.service());
    f.session("mutation-child", Some("child"));
    f.app
        .state::<crate::mcp::state::ActiveTeamState>()
        .update(crate::mcp::state::ActiveTeam {
            org_slug: "acme/ctx".into(),
            team_name: "parent".into(),
        });
    let caller = crate::context_repo::caller_scope::for_session(
        f.app.handle(),
        Some("mutation-child"),
        None,
    )
    .await;
    let invoke =
        |op: &'static str, args| call_for(f.app.handle().clone(), op, args, None, caller.clone());
    let hidden = f.fixture.path("parent", "content/secret.md");
    let missing = f.fixture.path("parent", "content/absent.md");
    let dest = f.fixture.path("child", "content/copied.md");
    assert_eq!(
        invoke("move", json!({"from":hidden,"to":dest}))
            .await
            .unwrap_err(),
        invoke("move", json!({"from":missing,"to":dest}))
            .await
            .unwrap_err()
    );
    assert!(hidden.exists());
    assert!(!dest.exists());
    let shared = f.fixture.path("parent", "content/shared.md");
    invoke("move", json!({"from":shared,"to":dest}))
        .await
        .unwrap();
    assert_eq!(
        invoke("read", json!({"path":dest})).await.unwrap()["text"],
        "parent-shared.md-bytes"
    );
    for collision in ["fail", "keep-both"] {
        assert_eq!(
            invoke(
                "create",
                json!({"path":hidden,"content":"new","onCollision":collision})
            )
            .await
            .unwrap_err(),
            invoke(
                "create",
                json!({"path":missing,"content":"new","onCollision":collision})
            )
            .await
            .unwrap_err()
        );
    }
    assert_eq!(
        invoke("delete", json!({"path":hidden})).await.unwrap_err(),
        invoke(
            "delete",
            json!({"path":f.fixture.path("parent", "content/never.md")})
        )
        .await
        .unwrap_err()
    );
    assert!(!hidden.exists(), "baseline deletion still executes");
    let own = f.fixture.path("child", "content/new.md");
    invoke("create", json!({"path":own,"content":"own"}))
        .await
        .unwrap();
    invoke("delete", json!({"path":own})).await.unwrap();
    let hidden_dir = f.fixture.path("parent", "content");
    assert_eq!(
        invoke(
            "move",
            json!({"from":hidden_dir,"to":f.fixture.path("child", "content/copied-dir")})
        )
        .await
        .unwrap_err(),
        NOT_FOUND
    );
    assert!(hidden_dir.exists());
}

#[tokio::test]
async fn directory_move_checks_excluded_descendants_before_renaming_an_admitted_root() {
    let f = AppFixture::new();
    f.fixture.write(
        "parent",
        "config/sharing.yml",
        "share: ['content/**', '!content/docs/secret.md']\n",
    );
    let docs = f.fixture.path("parent", "content/docs");
    let secret = f
        .fixture
        .write("parent", "content/docs/secret.md", "hidden bytes");
    f.fixture
        .write("parent", "content/docs/shared.md", "shared bytes");
    let shared_only = f
        .fixture
        .write("parent", "content/public/shared.md", "public bytes");
    f.fixture
        .write("child", "content/own-folder/own.md", "own bytes");
    f.app.manage(f.fixture.service());
    f.session("folder-child", Some("child"));
    let caller =
        crate::context_repo::caller_scope::for_session(f.app.handle(), Some("folder-child"), None)
            .await;
    assert!(caller.allows(&docs));
    assert!(!caller.allows(&secret));
    let invoke = |from: &Path, to: &Path| {
        call_for(
            f.app.handle().clone(),
            "move",
            json!({"from":from,"to":to}),
            None,
            caller.clone(),
        )
    };
    let dest = f.fixture.path("child", "content/copied");
    let denied = invoke(&docs, &dest).await.unwrap_err();
    assert!(docs.is_dir());
    assert!(secret.is_file());
    assert!(!dest.exists());
    std::fs::remove_file(&secret).unwrap();
    // A missing excluded file leaves a wholly shared folder, which can be delivered.
    invoke(&docs, &dest).await.unwrap();
    assert_eq!(denied, NOT_FOUND);
    assert_eq!(
        std::fs::read_to_string(dest.join("shared.md")).unwrap(),
        "shared bytes"
    );
    let missing = f.fixture.path("parent", "content/absent-folder");
    assert_eq!(
        invoke(&missing, &f.fixture.path("child", "content/absent-copy"))
            .await
            .unwrap_err(),
        denied
    );
    assert!(!dest.join("secret.md").exists());
    for (source, target, expected) in [
        (
            shared_only.parent().unwrap().to_path_buf(),
            f.fixture.path("child", "content/public-copy"),
            "shared.md",
        ),
        (
            f.fixture.path("child", "content/own-folder"),
            f.fixture.path("child", "content/own-copy"),
            "own.md",
        ),
    ] {
        invoke(&source, &target).await.unwrap();
        assert!(target.join(expected).is_file());
    }
}
