// SPDX-License-Identifier: MIT

use super::*;
#[test]
fn unknown_pane_list_hides_team_manifests_but_keeps_private_content() {
    use crate::context_repo::caller_scope::{tests::Fixture, CallerScope};
    let f = Fixture::new();
    f.write(
            "parent",
            "panes/pane-manifest.yaml",
            "version: 1\npanes:\n  - id: parent\n    html: parent.html\n    title: Hidden manifest sentinel\n",
        );
    let me = Principal::from(1u64);
    let space = crate::context_repo::scratch_space::ensure_org_space(&f.home, &me, "acme/ctx")
        .unwrap()
        .0;
    std::fs::create_dir_all(space.panes_dir()).unwrap();
    std::fs::write(
        space.panes_dir().join("personal.html"),
        "<title>Private positive</title>",
    )
    .unwrap();
    std::fs::write(
            space.panes_dir().join(panes::manifest::MANIFEST_FILE),
            "version: 1\npanes:\n  - id: personal\n    html: personal.html\n    title: Private manifest positive\n",
        )
        .unwrap();
    let read = |scope: &CallerScope| {
        serde_json::to_value(list_for(&f.home, &me, "acme/ctx", "parent", scope).unwrap()).unwrap()
    };
    let unknown = read(&CallerScope::Unknown).to_string();
    assert!(!unknown.contains("Hidden manifest sentinel"));
    assert!(unknown.contains("personal.html"));
    assert!(unknown.contains("Private manifest positive"));
    assert!(read(&CallerScope::Human)
        .to_string()
        .contains("Hidden manifest sentinel"));
    std::fs::remove_file(f.path("parent", "panes/pane-manifest.yaml")).unwrap();
    assert_eq!(unknown, read(&CallerScope::Unknown).to_string());
}
#[test]
fn pane_fallback_precedence_runs_after_caller_filtering() {
    use crate::context_repo::caller_scope::{tests::Fixture, CallerScope};
    let f = Fixture::new();
    let me = Principal::from(1u64);
    let read = |scope: &CallerScope| {
        let ToolEnvelope::Ok(listing) =
            list_for(&f.home, &me, "acme/ctx", "parent", scope).unwrap()
        else {
            panic!("listing")
        };
        listing
            .panes
            .into_iter()
            .filter(|pane| pane.id == "inbox")
            .collect::<Vec<_>>()
    };
    assert_eq!(
        read(&f.scope)[0].dashboard_ref.scope,
        panes::DashboardScope::BuiltIn
    );
    f.write(
        "parent",
        "panes/inbox.html",
        "<html><head><title>Hidden Inbox</title></head><body>Private</body></html>",
    );
    let denied = read(&CallerScope::Unknown);
    std::fs::remove_file(f.path("parent", "panes/inbox.html")).unwrap();
    assert_eq!(denied, read(&CallerScope::Unknown));
    let private = crate::context_repo::scratch_space::ensure_org_space(&f.home, &me, "acme/ctx")
        .unwrap()
        .0;
    std::fs::create_dir_all(private.panes_dir()).unwrap();
    std::fs::write(
        private.panes_dir().join("inbox.html"),
        "<html><head><title>Private Inbox</title></head><body>Own</body></html>",
    )
    .unwrap();
    let own = read(&f.scope);
    assert_eq!(own.len(), 1);
    assert_eq!(own[0].title, "Private Inbox");
}
#[test]
fn other_org_same_named_team_keeps_manifest_and_still_gates_real_owner_aliases() {
    use crate::context_repo::caller_scope::{tests::Fixture, CallerScope};
    let f = Fixture::new();
    let me = Principal::from(1u64);
    let org = "other/ctx";
    let root = crate::context_repo::path_to_org_root(&f.home, &me, org).unwrap();
    std::fs::create_dir_all(root.join("parent/panes")).unwrap();
    std::fs::create_dir_all(root.join("parent/content")).unwrap();
    for name in ["org.yml", "people.yml", "teams.yml"] {
        let text = std::fs::read_to_string(f.root.join(name))
            .unwrap()
            .replace("acme/ctx", org);
        std::fs::write(root.join(name), text).unwrap();
    }
    std::fs::write(
        root.join("parent/panes/report.html"),
        "<title>HTML fallback</title>",
    )
    .unwrap();
    let manifest = root.join("parent/panes/pane-manifest.yaml");
    std::fs::write(&manifest, "version: 1\npanes:\n  - id: report\n    html: report.html\n    title: Other org report\n    icon: chart-bar\n").unwrap();
    let read = |scope: &CallerScope| {
        let ToolEnvelope::Ok(listing) = list_for(&f.home, &me, org, "parent", scope).unwrap()
        else {
            panic!("listing refused");
        };
        listing
            .panes
            .into_iter()
            .find(|pane| pane.id == "report")
            .unwrap()
    };
    let expected = read(&CallerScope::Human);
    assert_eq!(expected.title, "Other org report");
    assert_eq!(read(&f.scope), expected);
    #[cfg(unix)]
    {
        let hidden = f.write("parent", "config/private-manifest.yaml", "version: 1\npanes:\n  - id: report\n    html: report.html\n    title: Hidden parent title\n");
        std::fs::remove_file(&manifest).unwrap();
        std::os::unix::fs::symlink(&hidden, &manifest).unwrap();
        let denied = read(&f.scope);
        assert_eq!(denied.title, "HTML fallback");
        std::fs::remove_file(&manifest).unwrap();
        assert_eq!(denied, read(&f.scope));
    }
    assert!(f
        .scope
        .existing(&f.path("parent", "content/shared.md"))
        .is_ok());
}
