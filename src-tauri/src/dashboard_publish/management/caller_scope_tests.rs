// SPDX-License-Identifier: MIT
use super::*;
use crate::context_repo::caller_scope::{tests::AppFixture, CallerScope};

fn dashboard(f: &AppFixture, team: &str, name: &str) -> PathBuf {
    let folder = f.fixture.path(team, &format!("content/{name}"));
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("dashboard.yml"), "slug: shared-publish\ntitle: Shared\npublish:\n  state: draft\n  visibility: private\n  files: [index.html]\n").unwrap();
    std::fs::write(folder.join("index.html"), "<p>Shared bytes</p>").unwrap();
    folder
}

#[tokio::test]
async fn publication_action_references_keep_the_callers_definition_scope() {
    let f = AppFixture::new();
    let hidden = f.fixture.agent("parent", "publisher");
    let own = f.fixture.agent("child", "publisher");
    let caller = f.fixture.scope.clone();
    let scope = state::Scope {
        org: "acme/ctx".into(),
        team: "child".into(),
    };
    let actions = |key: String| {
        serde_json::from_value(serde_json::json!({"launch": {
            "verb":"launchAgent", "who":"owner", "agent":key,
            "prompt":"Read {id}", "mode":"headless", "max_per_hour":1, "max_running":1
        }}))
        .unwrap()
    };
    let denied = super::super::actions::validate_references_for(
        f.app.handle(),
        &actions(hidden.to_string()),
        Some(&scope),
        Some(caller.clone()),
    )
    .await
    .unwrap_err();
    let missing = super::super::actions::validate_references_for(
        f.app.handle(),
        &actions("acme/ctx/parent/absent".into()),
        Some(&scope),
        Some(caller.clone()),
    )
    .await
    .unwrap_err();
    assert_eq!(denied, missing);
    super::super::actions::validate_references_for(
        f.app.handle(),
        &actions(own.to_string()),
        Some(&scope),
        Some(caller.clone()),
    )
    .await
    .unwrap();
    // The persistent consumer derives the same scope from its saved source team.
    assert_eq!(
        super::super::actions::validate_references_for(
            f.app.handle(),
            &actions(hidden.to_string()),
            Some(&scope),
            None
        )
        .await
        .unwrap_err(),
        denied
    );
    super::super::actions::validate_references_for(
        f.app.handle(),
        &actions(hidden.to_string()),
        Some(&scope),
        Some(CallerScope::Human),
    )
    .await
    .unwrap();
    assert!(caller
        .read_admitted(&f.fixture.path("parent", "content/shared.md"))
        .is_ok());
}

#[tokio::test]
async fn publishing_mcp_source_refuses_ancestor_authority_before_manifest_or_persistent_work() {
    use tauri::Manager;
    let f = AppFixture::new();
    f.fixture.write(
        "parent",
        "config/sharing.yml",
        "share: ['content/shared-publish/**']\n",
    );
    let hidden = dashboard(&f, "parent", "hidden-publish");
    let shared = dashboard(&f, "parent", "shared-publish");
    let own = dashboard(&f, "child", "own-publish");
    let sibling = dashboard(&f, "sibling", "sibling-publish");
    f.session("publish-child", Some("child"));
    f.app
        .state::<crate::mcp::state::ActiveTeamState>()
        .update(crate::mcp::state::ActiveTeam {
            org_slug: "acme/ctx".into(),
            team_name: "parent".into(),
        });
    let caller =
        crate::context_repo::caller_scope::for_session(f.app.handle(), Some("publish-child"), None)
            .await;
    let missing = f.fixture.path("parent", "content/missing-publish");
    // These are the shared source preflights used by publish, unpublish and share MCP routes.
    for payload in [true, false] {
        let call = |path: &Path| admit_source(path.to_str().unwrap(), &caller, payload);
        assert_eq!(call(&hidden).unwrap_err(), call(&missing).unwrap_err());
        assert_eq!(call(&shared).unwrap_err(), call(&missing).unwrap_err());
        assert!(call(&own).is_ok());
        assert!(call(&sibling).is_ok());
        assert!(admit_source(hidden.to_str().unwrap(), &CallerScope::Human, payload).is_ok());
    }
    assert_eq!(
        caller.read_admitted(&shared.join("index.html")).unwrap(),
        "<p>Shared bytes</p>"
    );
    // Copying admitted files into an own source is supported; no publication/network action runs here.
    std::fs::copy(shared.join("index.html"), own.join("index.html")).unwrap();
    let admitted = admit_source(own.to_str().unwrap(), &caller, true).unwrap();
    let manifest = manifest::load(&admitted).unwrap();
    let built = payload::build(
        &manifest,
        payload::Mode::Published,
        &payload::FolderReader { folder: &admitted },
    )
    .unwrap();
    assert_eq!(
        built.payload.files[0].bytes,
        b"<p>Shared bytes</p>".to_vec()
    );
    let private = crate::context_repo::scratch_space::ensure_org_space(
        &f.fixture.home,
        &crate::domain::Principal::from(1u64),
        "acme/ctx",
    )
    .unwrap()
    .0
    .content_dir()
    .join("publication");
    std::fs::create_dir_all(&private).unwrap();
    std::fs::copy(own.join("dashboard.yml"), private.join("dashboard.yml")).unwrap();
    std::fs::copy(own.join("index.html"), private.join("index.html")).unwrap();
    assert!(admit_source(private.to_str().unwrap(), &caller, true).is_ok());
    let other_root = f.fixture.home.join("users/1/orgs/other/ctx");
    std::fs::create_dir_all(other_root.join("parent/content/other-publish")).unwrap();
    for name in ["org.yml", "teams.yml", "people.yml"] {
        let text = std::fs::read_to_string(f.fixture.root.join(name))
            .unwrap()
            .replace("acme/ctx", "other/ctx");
        std::fs::write(other_root.join(name), text).unwrap();
    }
    let other = other_root.join("parent/content/other-publish");
    std::fs::copy(own.join("dashboard.yml"), other.join("dashboard.yml")).unwrap();
    std::fs::copy(own.join("index.html"), other.join("index.html")).unwrap();
    assert!(admit_source(other.to_str().unwrap(), &caller, true).is_ok());
    // A real alternate checkout retains the source owner's identity.
    let checkout = f.fixture.temp.path().join("publisher-checkout");
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "test@example.test"],
        vec![
            "add",
            "org.yml",
            "teams.yml",
            "people.yml",
            "parent/config/sharing.yml",
            "parent/content/shared-publish",
        ],
        vec!["commit", "-m", "publication fixture"],
    ] {
        let result = std::process::Command::new("git")
            .args(args)
            .current_dir(&f.fixture.root)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let result = std::process::Command::new("git")
        .args(["worktree", "add", "-b", "publisher-checkout"])
        .arg(&checkout)
        .current_dir(&f.fixture.root)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        admit_source(
            checkout
                .join("parent/content/shared-publish")
                .to_str()
                .unwrap(),
            &caller,
            true
        )
        .unwrap_err(),
        crate::context_repo::caller_scope::MISSING
    );
    // Real-owner aliases cannot turn an ancestor's source into an own dashboard.
    #[cfg(unix)]
    {
        let alias = f.fixture.path("child", "content/parent-publish");
        std::os::unix::fs::symlink(&shared, &alias).unwrap();
        assert_eq!(
            admit_source(alias.to_str().unwrap(), &caller, true).unwrap_err(),
            crate::context_repo::caller_scope::MISSING
        );
        let asset = own.join("index.html");
        std::fs::remove_file(&asset).unwrap();
        std::os::unix::fs::symlink(hidden.join("index.html"), &asset).unwrap();
        assert_eq!(
            admit_source(own.to_str().unwrap(), &caller, true).unwrap_err(),
            crate::context_repo::caller_scope::MISSING
        );
        // Taking down or managing an own dashboard reads its slug, not its payload.
        assert!(admit_source(own.to_str().unwrap(), &caller, false).is_ok());
    }
}

#[tokio::test]
async fn dashboard_creation_masks_ancestor_collision_and_success_without_restricting_writes() {
    use crate::dashboard_publish::{
        catalog::CATALOG,
        create::{create_for, CreateRequest},
    };
    let f = AppFixture::new();
    f.session("draft-child", Some("child"));
    let caller =
        crate::context_repo::caller_scope::for_session(f.app.handle(), Some("draft-child"), None)
            .await;
    let request = |folder: &Path| CreateRequest {
        folder: folder.to_path_buf(),
        slug: "caller-draft".into(),
        title: "Caller draft".into(),
        context_team: Some(("acme/ctx".into(), "parent".into())),
        ..Default::default()
    };
    let mut denied = None;
    for (name, state) in [("existing", 2), ("absent", 0), ("empty", 1)] {
        let folder = f.fixture.path("parent", &format!("content/{name}-draft"));
        if state > 0 {
            std::fs::create_dir_all(&folder).unwrap();
        }
        if state == 2 {
            std::fs::write(folder.join("private.txt"), "hidden sentinel").unwrap();
        }
        let result = create_for(&request(&folder), CATALOG, &caller).unwrap_err();
        if let Some(before) = &denied {
            assert_eq!(before, &result);
        } else {
            denied = Some(result);
        }
        if state == 2 {
            assert_eq!(
                std::fs::read_to_string(folder.join("private.txt")).unwrap(),
                "hidden sentinel"
            );
            assert!(!folder.join("dashboard.yml").exists());
        } else {
            assert!(
                folder.join("dashboard.yml").is_file(),
                "baseline creation still executes"
            );
        }
    }
    for team in ["child", "sibling"] {
        let folder = f.fixture.path(team, "content/visible-draft");
        assert!(create_for(&request(&folder), CATALOG, &caller).is_ok());
    }
    #[cfg(unix)]
    {
        let alias = f.fixture.path("child", "content/parent-draft-alias");
        std::os::unix::fs::symlink(f.fixture.path("parent", "content"), &alias).unwrap();
        assert_eq!(
            create_for(&request(&alias.join("aliased-draft")), CATALOG, &caller).unwrap_err(),
            denied.clone().unwrap()
        );
        assert!(
            f.fixture
                .path("parent", "content/aliased-draft/dashboard.yml")
                .is_file(),
            "baseline creation still executes through the alias"
        );
    }
    let human = f.fixture.path("parent", "content/human-draft");
    assert!(create_for(&request(&human), CATALOG, &CallerScope::Human).is_ok());
    assert_eq!(
        caller
            .read_admitted(&f.fixture.path("parent", "content/shared.md"))
            .unwrap(),
        "parent-shared.md-bytes"
    );
}
