// SPDX-License-Identifier: MIT
use super::*;
use crate::context_repo::caller_scope::tests::Fixture;

#[test]
fn team_all_parent_listing_and_copy_share_the_caller_gate() {
    let f = Fixture::new();
    let endpoint = |team: &str| ContextEndpoint::TeamAll {
        org_slug: "acme/ctx".into(),
        team_name: team.into(),
    };
    let root = resolve_endpoint_root(&f.home, 1, Some("acme/ctx"), &endpoint("parent")).unwrap();
    let list =
        || crate::commands::context_mobility::list_context_items_for(&root, &f.scope).unwrap();
    let before = serde_json::to_value(list()).unwrap();
    assert_eq!(
        before,
        serde_json::json!([{"logicalName":"shared.md","path":"shared.md"}])
    );
    let params = |name: &str| MoveBetweenContextsParams {
        from: endpoint("parent"),
        to: endpoint("child"),
        logical_name: name.into(),
        to_logical_name: Some("received.md".into()),
        user_id: 1,
        force: false,
        retain_source: true,
    };
    let copy =
        |name| move_between_contexts_for(&f.home, Some("acme/ctx"), &params(name), None, &f.scope);
    assert_eq!(copy("secret.md").unwrap(), copy("missing.md").unwrap());
    assert!(!source_admitted(
        &f.home,
        Some("acme/ctx"),
        &params("secret.md"),
        &f.scope
    ));
    assert!(!f.path("child", "content/received.md").exists());
    assert_eq!(
        copy("shared.md").unwrap(),
        MoveBetweenContextsOutcome::Copied
    );
    assert_eq!(
        std::fs::read_to_string(f.path("child", "content/received.md")).unwrap(),
        "parent-shared.md-bytes"
    );
    std::fs::remove_file(f.path("parent", "content/secret.md")).unwrap();
    assert_eq!(before, serde_json::to_value(list()).unwrap());
}
