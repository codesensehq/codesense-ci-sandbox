// SPDX-License-Identifier: MIT
use super::*;
use crate::context_repo::caller_scope::tests::Fixture;
use crate::install::frontmatter_cache::FrontmatterCache;

#[test]
fn dashboard_team_root_and_cross_org_story_id_use_caller_admission() {
    let f = Fixture::new();
    let principal = Principal::from(1u64);
    for (path, id) in [("secret.md", "P-SECRET"), ("shared.md", "P-SHARED")] {
        f.write(
            "parent",
            &format!("content/{path}"),
            &format!("---\ntype: story\nstory_id: {id}\nsource_repo: acme/code\n---\n# {id}\n"),
        );
    }
    let resolver = crate::install::org_story_resolver::OrgStoryResolver::new();
    let shared = FrontmatterCache::new_shared();
    let snapshot = resolver.snapshot_for(
        &shared,
        Some("another/org"),
        &f.home,
        &principal,
        "acme/ctx",
    );
    assert_eq!(
        resolve_story_path_in_for(&snapshot, "P-SECRET", &f.scope).unwrap_err(),
        resolve_story_path_in_for(&snapshot, "NO-SUCH-ID", &f.scope).unwrap_err()
    );
    let resolved = resolve_story_path_in_for(&snapshot, "P-SHARED", &f.scope).unwrap();
    assert_eq!(resolved.team, "parent");
    assert!(std::fs::read_to_string(resolved.story_file_path)
        .unwrap()
        .contains("P-SHARED"));
    assert_eq!(
        resolve_managed_team_content_root_for(&f.home, &principal, "acme/ctx", "parent", &f.scope)
            .unwrap_err(),
        resolve_managed_team_content_root_for(&f.home, &principal, "acme/ctx", "absent", &f.scope)
            .unwrap_err()
    );
    assert!(resolve_managed_team_content_root_for(
        &f.home, &principal, "acme/ctx", "child", &f.scope
    )
    .is_ok());
    assert!(resolve_managed_team_content_root_for(
        &f.home, &principal, "acme/ctx", "sibling", &f.scope
    )
    .is_ok());
    std::fs::remove_file(f.path("parent", "content/secret.md")).unwrap();
    let cache = FrontmatterCache::build_from_scan(&f.root);
    assert_eq!(
        resolve_story_path_in_for(&snapshot, "P-SECRET", &f.scope).unwrap_err(),
        resolve_story_path_in_for(&cache, "P-SECRET", &f.scope).unwrap_err()
    );
}

#[test]
fn cold_story_lookup_filters_hidden_duplicates_before_choosing_a_match() {
    let f = Fixture::new();
    for (team, path) in [
        ("grand", "secret.md"),
        ("parent", "secret.md"),
        ("parent", "shared.md"),
    ] {
        f.write(
            team,
            &format!("content/{path}"),
            "---\ntype: story\nstory_id: P-SHARED\nsource_repo: acme/code\n---\n# Story\n",
        );
    }
    let shared = FrontmatterCache::new_shared();
    let read = |id| resolve_story_path_with_fallback_for(&shared, Some(&f.root), id, &f.scope);
    let resolved = read("P-SHARED").unwrap();
    assert_eq!(resolved.team, "parent");
    assert_eq!(
        std::path::Path::new(&resolved.story_file_path),
        f.path("parent", "content/shared.md")
    );
    let before = read("P-SHARED").unwrap().story_file_path;
    std::fs::remove_file(f.path("grand", "content/secret.md")).unwrap();
    std::fs::remove_file(f.path("parent", "content/secret.md")).unwrap();
    assert_eq!(read("P-SHARED").unwrap().story_file_path, before);
    assert_eq!(
        read("P-UNSHARED").unwrap_err(),
        read("NO-SUCH-ID").unwrap_err()
    );
}
