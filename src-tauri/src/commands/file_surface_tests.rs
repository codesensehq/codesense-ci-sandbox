// SPDX-License-Identifier: MIT
use super::*;
#[test]
fn inherited_listing_and_reads_use_the_sharing_gate() {
    use crate::context_repo::inheritance::team_view;
    let (_tmp, root) = crate::context_repo::inheritance::test_fixture();
    std::fs::remove_file(root.join("child/content/shared.md")).unwrap();
    let view = team_view(&root, "acme/ctx", "child");
    let access = ContentAccess::new(
        view.own_content().own_path().unwrap().into(),
        Some(view),
        crate::context_repo::caller_scope::CallerScope::Human,
    );
    let rows = access.list_rows(None).unwrap();
    let shared = rows.iter().find(|r| r["name"] == "shared.md").unwrap();
    assert_eq!(shared["inheritedFrom"], "parent");
    assert!(access
        .admits_path(Path::new(shared["path"].as_str().unwrap()))
        .is_ok());
    for path in [
        root.join("parent/content/secret.md"),
        root.join("sibling/content/shared.md"),
        root.join("parent/content/missing.md"),
    ] {
        assert_eq!(access.admits_path(&path).unwrap_err(), NOT_FOUND);
    }
}

#[test]
fn selecting_an_ancestor_keeps_the_callers_view_at_the_same_boundary() {
    let (_tmp, root) = crate::context_repo::inheritance::test_fixture();
    let view = crate::context_repo::inheritance::team_view(&root, "acme/ctx", "child");
    let parent = crate::context_repo::inheritance::team_view(&root, "acme/ctx", "parent");
    let access = ContentAccess::new(
        parent.own_content().own_path().unwrap().into(),
        Some(view),
        crate::context_repo::caller_scope::CallerScope::Human,
    );
    let rows = access.list_rows(None).unwrap();
    assert!(rows.iter().any(|r| r["name"] == "shared.md"));
    assert!(!rows.iter().any(|r| r["name"] == "secret.md"));
    assert_eq!(
        access
            .admits_path(&access.own.join("secret.md"))
            .unwrap_err(),
        NOT_FOUND
    );
}

#[test]
fn missing_own_reads_have_the_same_not_found_response_as_denied_paths() {
    let (_tmp, root) = crate::context_repo::inheritance::test_fixture();
    let view = crate::context_repo::inheritance::team_view(&root, "acme/ctx", "child");
    let own = view.own_content().own_path().unwrap().to_path_buf();
    let access = ContentAccess::new(
        own.clone(),
        Some(view),
        crate::context_repo::caller_scope::CallerScope::Human,
    );
    assert_eq!(
        access.admits_path(&own.join("absent.md")).unwrap_err(),
        NOT_FOUND
    );
    assert_eq!(
        access
            .admits_path(&root.join("parent/content/secret.md"))
            .unwrap_err(),
        NOT_FOUND
    );
}

#[test]
fn selected_parent_lists_filter_directories_before_summaries_and_winners() {
    let (_tmp, root) = crate::context_repo::inheritance::test_fixture();
    for (relative, text) in [
        ("parent/content/hidden-dir/hidden.md", "hidden"),
        ("parent/content/shared-dir/shared.md", "shared"),
    ] {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    std::fs::write(
        root.join("parent/config/sharing.yml"),
        "share:\n  - content/shared.md\n  - content/shared-dir/shared.md\n",
    )
    .unwrap();
    let child = crate::context_repo::inheritance::team_view(&root, "acme/ctx", "child");
    let parent = crate::context_repo::inheritance::team_view(&root, "acme/ctx", "parent");
    let access = ContentAccess::new(
        parent.own_content().own_path().unwrap().into(),
        Some(parent),
        crate::context_repo::caller_scope::CallerScope::Team(child),
    );
    let rows = access.list_rows(None).unwrap();
    assert!(rows.iter().any(|r| r["name"] == "shared-dir"));
    assert!(!rows.iter().any(|r| r["name"] == "hidden-dir"));
    assert_eq!(
        access.list_rows(Some("hidden-dir")).unwrap(),
        access.list_rows(Some("absent-dir")).unwrap()
    );
    assert_eq!(
        access.list_rows(Some("shared-dir")).unwrap()[0]["name"],
        "shared.md"
    );
}

#[test]
fn rename_tints_admit_sources_before_exposing_old_names() {
    for old_shared in [false, true] {
        let (_tmp, root) = crate::context_repo::inheritance::test_fixture();
        let old = if old_shared {
            "old-shared.md"
        } else {
            "secret.md"
        };
        std::fs::write(root.join("parent/content").join(old), "identical bytes").unwrap();
        std::fs::write(root.join("parent/content/extra.md"), "shared control").unwrap();
        std::fs::write(
            root.join("parent/config/sharing.yml"),
            "share:\n  - content/shared.md\n  - content/old-shared.md\n  - content/extra.md\n",
        )
        .unwrap();
        let repo = git2::Repository::init(&root).unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_path(&PathBuf::from("parent/content").join(old))
            .unwrap();
        index.write().unwrap();
        let tree = index.write_tree().unwrap();
        let tree = repo.find_tree(tree).unwrap();
        let sig = git2::Signature::now("Test", "test@example.test").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        std::fs::remove_file(root.join("parent/content/shared.md")).unwrap();
        std::fs::rename(
            root.join("parent/content").join(old),
            root.join("parent/content/shared.md"),
        )
        .unwrap();
        index
            .remove_path(&PathBuf::from("parent/content").join(old))
            .unwrap();
        index
            .add_path(Path::new("parent/content/shared.md"))
            .unwrap();
        index.write().unwrap();
        let child = crate::context_repo::inheritance::team_view(&root, "acme/ctx", "child");
        let parent = crate::context_repo::inheritance::team_view(&root, "acme/ctx", "parent");
        let access = ContentAccess::new(
            parent.own_content().own_path().unwrap().into(),
            Some(parent),
            crate::context_repo::caller_scope::CallerScope::Team(child),
        );
        let mut rows = access.list_rows(None).unwrap();
        let shared = rows.iter_mut().find(|r| r["name"] == "shared.md").unwrap();
        assert_eq!(
            shared["gitState"],
            if old_shared { "renamed" } else { "added" }
        );
        assert_eq!(
            shared.get("oldName").and_then(Value::as_str),
            old_shared.then_some(old)
        );
        assert_eq!(
            access.admits_path(&root.join("parent/content/secret.md")),
            access.admits_path(&root.join("parent/content/absent.md"))
        );
    }
}
