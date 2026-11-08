// SPDX-License-Identifier: MIT
use super::*;
use crate::panes::watch::{team_dir_handler, Coalescer};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

fn org_metadata(root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(
        root.join("org.yml"),
        "slug: acme/app\ndisplay_name: Acme\ncreated_at: '2026-01-01T00:00:00Z'\n",
    )
    .unwrap();
    std::fs::write(root.join("people.yml"), "people: []\n").unwrap();
}

#[test]
fn a_panel_id_retry_keeps_the_installed_ref_and_copy_metadata() {
    let home = tempfile::tempdir().unwrap();
    let principal = Principal::from(42u64);
    let root = path_to_org_root(home.path(), &principal, "acme/app").unwrap();
    org_metadata(&root);
    std::fs::create_dir_all(root.join("a/panes")).unwrap();
    std::fs::create_dir_all(root.join("b/content")).unwrap();
    std::fs::write(root.join("teams.yml"), "teams:\n  - name: a\n  - name: b\n").unwrap();
    std::fs::write(root.join("a/panes/people.html"), "<title>People</title>").unwrap();
    let source = panes::list_layer(
        &root.join("a/panes"),
        PaneScope::Team,
        "acme/app",
        Some("a"),
    )
    .remove(0);
    let installed = panes::install::choose(home.path(), &principal, &source, true, true).unwrap();
    let mut registry = Registry::default();
    let mut topics = TopicRegistry::default();
    let (_, before, _) = registry
        .open(
            &mut topics,
            ViewRequest {
                home: home.path(),
                principal: &principal,
                owner: "installed",
                sequence: 1,
                org: "acme/app",
                team: "b",
                selected: vec![installed.clone()],
                retry: None,
            },
        )
        .unwrap();
    let row = before
        .snapshot
        .iter()
        .find(|r| r["id"] == installed.key())
        .unwrap();
    let version = row["entry"]["installSourceVersion"].clone();
    assert!(version.is_string());
    let mut panel_ref = installed.clone();
    panel_ref.installed_from = None;
    let (_, after, _) = registry
        .open(
            &mut topics,
            ViewRequest {
                home: home.path(),
                principal: &principal,
                owner: "installed",
                sequence: 2,
                org: "acme/app",
                team: "b",
                selected: vec![installed.clone()],
                retry: Some(panel_ref),
            },
        )
        .unwrap();
    let row = after
        .snapshot
        .iter()
        .find(|r| r["id"] == installed.key())
        .unwrap();
    assert_eq!(row["entry"]["dashboardRef"], json!(installed));
    assert_eq!(row["entry"]["installSourceVersion"], version);

    let path = PathBuf::from(row["entry"]["path"].as_str().unwrap());
    let key = after.view_key;
    registry.entry_reads = 0;
    registry.layer_scans = 0;
    let change = panes::watch::scratch_pane_change(
        &scratch_space::scratch_root(home.path(), &principal),
        None,
        &path,
    )
    .unwrap();
    assert!(change.installed.is_some());
    std::fs::write(&path, "<title>Edited copy</title>").unwrap();
    registry.changed(&mut topics, &change).unwrap();
    assert_eq!(registry.entry_reads, 1);
    assert_eq!(
        registry.layer_scans, 0,
        "installed entry edits are targeted"
    );
    std::fs::remove_file(&path).unwrap();
    registry.changed(&mut topics, &change).unwrap();
    std::fs::write(&path, "<title>Returned copy</title>").unwrap();
    registry.changed(&mut topics, &change).unwrap();
    let returned = topics.snapshot(TOPIC, &key).unwrap();
    let row = returned
        .snapshot
        .iter()
        .find(|r| r["id"] == installed.key())
        .unwrap();
    assert_eq!(row["entry"]["title"], "Returned copy");
    assert_eq!(row["entry"]["dashboardRef"], json!(installed));
    assert_eq!(row["entry"]["installSourceVersion"], version);
}

#[test]
fn sibling_files_flow_through_ingress_into_versioned_selected_rows() {
    let home = tempfile::tempdir().unwrap();
    let principal = Principal::from(42u64);
    let root = path_to_org_root(home.path(), &principal, "acme/app").unwrap();
    org_metadata(&root);
    std::fs::create_dir_all(root.join("a/panes")).unwrap();
    std::fs::create_dir_all(root.join("b/content")).unwrap();
    std::fs::write(root.join("teams.yml"), "teams:\n  - name: a\n  - name: b\n").unwrap();
    let path = root.join("a/panes/people.html");
    let html = |title: &str, chord: &str| {
        format!(
        "<title>{title}</title><meta name='codesense:slots' content='dock'><meta name='codesense:commands' content='[{{\"id\":\"find\",\"label\":\"Find {title}\",\"defaultChord\":\"{chord}\",\"scope\":\"global\"}}]'>"
    )
    };
    std::fs::write(&path, html("People", "Alt+P")).unwrap();
    std::fs::write(root.join("a/panes/unselected.html"), "<title>Other</title>").unwrap();
    let selected = DashboardRef {
        scope: DashboardScope::Team,
        org: "acme/app".into(),
        team: Some("a".into()),
        id: "people".into(),
        installed_from: None,
    };
    let mut registry = Registry::default();
    let mut topics = super::super::projection::TopicRegistry::default();
    let (_, snapshot, _) = registry
        .open(
            &mut topics,
            ViewRequest {
                home: home.path(),
                principal: &principal,
                owner: "test",
                sequence: 1,
                org: "acme/app",
                team: "b",
                selected: vec![selected.clone()],
                retry: None,
            },
        )
        .unwrap();
    let key = snapshot.view_key.clone();
    let selected_document_revision = snapshot
        .snapshot
        .iter()
        .find(|row| row["id"] == selected.key())
        .unwrap()["documentRevision"]
        .as_u64()
        .unwrap();
    let (_, sibling_snapshot, _) = registry
        .open(
            &mut topics,
            ViewRequest {
                home: home.path(),
                principal: &principal,
                owner: "sibling-reader",
                sequence: 1,
                org: "acme/app",
                team: "a",
                selected: vec![selected.clone()],
                retry: None,
            },
        )
        .unwrap();
    assert_eq!(
        sibling_snapshot
            .snapshot
            .iter()
            .find(|row| row["id"] == selected.key())
            .unwrap()["documentRevision"],
        selected_document_revision,
        "reader team does not change the source document revision"
    );
    assert!(snapshot
        .snapshot
        .iter()
        .any(|row| row["id"] == selected.key()));
    assert!(!snapshot
        .snapshot
        .iter()
        .any(|row| row["entry"]["id"] == "unselected"));
    let before = topics.snapshot(TOPIC, &key).unwrap().rev;
    registry.entry_reads = 0;
    registry.layer_scans = 0;
    let state = Arc::new(Mutex::new((registry, topics)));
    let (tx, rx) = mpsc::channel();
    let sink = state.clone();
    let coalescer = Arc::new(Coalescer::new(
        Duration::ZERO,
        Duration::ZERO,
        Arc::new(move |change| {
            let mut state = sink.lock().unwrap();
            let (registry, topics) = &mut *state;
            let deltas = registry.changed(topics, &change).unwrap();
            tx.send(deltas).unwrap();
        }),
    ));
    let handler = team_dir_handler(
        root.join("a"),
        "acme/app".into(),
        coalescer,
        Arc::new(|_| {}),
    );
    std::fs::write(&path, html("Members", "Alt+M")).unwrap();
    handler(&path);
    let deltas = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let delta = deltas.iter().find(|delta| delta.view_key == key).unwrap();
    assert_eq!(delta.rev, before + 1);
    assert_eq!(delta.upserts.len(), 1);
    assert_eq!(delta.upserts[0]["entry"]["title"], "Members");
    assert_eq!(
        delta.upserts[0]["entry"]["declaration"]["commands"][0]["defaultChord"],
        "Alt+M"
    );
    {
        let state = state.lock().unwrap();
        assert_eq!(state.0.entry_reads, 2);
        assert_eq!(state.0.layer_scans, 0);
    }
    let revision = delta.upserts[0]["sourceRevision"].as_u64().unwrap();
    let document_revision = delta.upserts[0]["documentRevision"].as_u64().unwrap();
    std::fs::write(root.join("a/panes/people.js"), "window.changed=true").unwrap();
    handler(&root.join("a/panes/people.js"));
    let asset = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let original_asset = asset.iter().find(|d| d.view_key == key).unwrap();
    assert!(
        original_asset.upserts[0]["sourceRevision"]
            .as_u64()
            .unwrap()
            > revision
    );
    assert!(
        original_asset.upserts[0]["documentRevision"]
            .as_u64()
            .unwrap()
            > document_revision
    );
    let sibling_asset = asset
        .iter()
        .find(|d| d.view_key == sibling_snapshot.view_key)
        .unwrap();
    assert_eq!(
        sibling_asset.upserts[0]["documentRevision"], original_asset.upserts[0]["documentRevision"],
        "asset-only changes share one revision across reader teams"
    );
    assert_eq!(
        state.lock().unwrap().0.entry_reads,
        2,
        "assets only advance revisions"
    );
    let template = root.join("a/panes/lib/template.html");
    std::fs::create_dir_all(template.parent().unwrap()).unwrap();
    std::fs::write(&template, "<p>Asset</p>").unwrap();
    handler(&template);
    let nested = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(nested[0].upserts[0]["sourceRevision"].as_u64().unwrap() > revision);
    assert_eq!(state.lock().unwrap().0.entry_reads, 2);
    assert_eq!(state.lock().unwrap().0.layer_scans, 0);
    std::fs::remove_file(&path).unwrap();
    handler(&path);
    let deleted = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(
        deleted[0].upserts[0]["entry"].is_null(),
        "selected identity survives deletion"
    );
    std::fs::write(&path, html("Returned", "Alt+R")).unwrap();
    handler(&path);
    let restored = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(restored[0].upserts[0]["entry"]["title"], "Returned");
    let mut locked = state.lock().unwrap();
    let replay = locked.1.subscribe(TOPIC, &key, Some(before)).unwrap();
    assert_eq!(replay.deltas.unwrap().len(), 5);
    assert!(
        locked.1.snapshot(TOPIC, "all").unwrap().snapshot.is_empty(),
        "frame grants cannot inherit a native source view"
    );
    let (registry, topics) = &mut *locked;
    let reads = registry.entry_reads;
    registry
        .open(
            topics,
            ViewRequest {
                home: home.path(),
                principal: &principal,
                owner: "test",
                sequence: 3,
                org: "acme/app",
                team: "b",
                selected: vec![selected.clone()],
                retry: Some(selected.clone()),
            },
        )
        .unwrap();
    assert_eq!(
        registry.entry_reads,
        reads + 1,
        "manual retry reads only its source"
    );
    assert_eq!(registry.layer_scans, 0, "retry reuses the source cache");
    let (listing, _, _) = registry
        .open(
            topics,
            ViewRequest {
                home: home.path(),
                principal: &principal,
                owner: "test",
                sequence: 2,
                org: "acme/app",
                team: "a",
                selected: vec![],
                retry: None,
            },
        )
        .unwrap();
    assert_eq!(
        listing.chain,
        vec!["b"],
        "an older request cannot replace the native view"
    );
    drop(locked);

    std::fs::write(&path, "<title>HTML title</title>").unwrap();
    let manifest = root.join("a/panes/pane-manifest.yaml");
    std::fs::write(&manifest, "version: 1\npanes:\n  - id: people\n    html: people.html\n    title: From manifest\n    slots: [dock]\n    commands:\n      - id: find\n        label: Lookup\n        defaultChord: Alt+L\n        scope: global\n").unwrap();
    handler(&manifest);
    let declared = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(declared[0].upserts[0]["entry"]["title"], "From manifest");
    assert_eq!(
        declared[0].upserts[0]["entry"]["declaration"]["commands"][0]["defaultChord"],
        "Alt+L"
    );
    // The reader this test opened first, as before the sibling reader existed: the sibling's
    // delta also carries its team's listing, which this budget never measured.
    let own = declared.iter().find(|d| d.view_key == key).unwrap();
    assert!(serde_json::to_vec(own).unwrap().len() < 2048);
    for _ in 0..=super::super::projection::RETAINED_DELTAS {
        std::fs::write(&path, "<title>HTML title</title><p>Body edit</p>").unwrap();
        handler(&path);
        rx.recv_timeout(Duration::from_secs(10)).unwrap();
    }
    let reply = state
        .lock()
        .unwrap()
        .1
        .subscribe(TOPIC, &key, Some(before))
        .unwrap();
    assert!(
        reply.snapshot.is_some(),
        "a source consumer beyond retained revisions gets a snapshot"
    );
}

fn recovery_fixture() -> (tempfile::TempDir, Principal, PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let principal = Principal::from(42u64);
    let root = path_to_org_root(home.path(), &principal, "acme/app").unwrap();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(
        root.join("org.yml"),
        "slug: acme/app\ndisplay_name: Acme\ncreated_at: 2026-01-01T00:00:00Z\n",
    )
    .unwrap();
    std::fs::write(root.join("people.yml"), "people: []\n").unwrap();
    std::fs::write(
        root.join("teams.yml"),
        "teams:\n  - name: child\n    parent_team: a\n  - name: a\n  - name: b\n",
    )
    .unwrap();
    for team in ["child", "a", "b"] {
        std::fs::create_dir_all(root.join(team).join("panes")).unwrap();
        if team != "child" {
            std::fs::create_dir_all(root.join(team).join("config")).unwrap();
            std::fs::write(
                root.join(team).join("config/sharing.yml"),
                "share: ['panes/**']\n",
            )
            .unwrap();
        }
        std::fs::write(
            root.join(team).join("panes/people.html"),
            format!("<title>{team}</title>"),
        )
        .unwrap();
    }
    (home, principal, root)
}

#[test]
fn unshared_parent_inbox_keeps_the_builtin_available_through_the_hub() {
    let (home, root) = crate::context_repo::inheritance::test_fixture();
    let principal = Principal::from(1u64);
    std::fs::write(
        root.join("parent/panes/inbox.html"),
        "<title>Parent inbox</title>",
    )
    .unwrap();
    let mut registry = Registry::default();
    let mut topics = TopicRegistry::default();
    for team in ["child", "parent", "sibling", scratch_space::SCRATCH_TEAM] {
        let direct = panes::list_panes(home.path(), &principal, "acme/ctx", team).unwrap();
        let recovery = direct.builtin_inbox.as_ref().unwrap();
        assert_eq!(recovery.dashboard_ref.scope, DashboardScope::BuiltIn);
        let (listing, _, _) = registry
            .open(
                &mut topics,
                ViewRequest {
                    home: home.path(),
                    principal: &principal,
                    owner: team,
                    sequence: 1,
                    org: "acme/ctx",
                    team,
                    selected: vec![],
                    retry: None,
                },
            )
            .unwrap();
        assert_eq!(listing.panes, direct.panes, "{team}");
        // By id, not position: other built-ins (story-trail) may list after the inbox.
        let inboxes: Vec<_> = listing
            .panes
            .iter()
            .filter(|pane| pane.id == "inbox")
            .collect();
        assert_eq!(inboxes.len(), 1, "{team}: one inbox listed");
        let inbox = inboxes[0];
        if team == "parent" {
            assert_eq!(inbox.dashboard_ref.scope, DashboardScope::Team);
        } else {
            assert_eq!(inbox.dashboard_ref.scope, DashboardScope::BuiltIn);
            assert_eq!(inbox, recovery, "{team}: the hub lists the built-in inbox");
        }
    }
}

#[test]
fn recovery_restores_a_builtin_shadowed_during_cold_discovery() {
    let (home, principal, root) = recovery_fixture();
    let path = root.join("a/panes/inbox.html");
    std::fs::write(&path, "<title>Custom inbox</title>").unwrap();
    let mut registry = Registry::default();
    let mut topics = TopicRegistry::default();
    let (before, _, _) = registry
        .open(
            &mut topics,
            ViewRequest {
                home: home.path(),
                principal: &principal,
                owner: "shadow",
                sequence: 1,
                org: "acme/app",
                team: "a",
                selected: vec![],
                retry: None,
            },
        )
        .unwrap();
    // The custom inbox shadows the built-in Inbox; other built-ins (e.g. files-tree) still list.
    assert!(!before
        .panes
        .iter()
        .any(|p| p.id == "inbox" && p.dashboard_ref.scope == DashboardScope::BuiltIn));
    std::fs::remove_file(&path).unwrap();
    let change =
        panes::watch::team_pane_change(&root.join("a/panes"), "acme/app", "a", &path).unwrap();
    registry.changed(&mut topics, &change).unwrap();
    let after = registry.listing(registry.views.values().next().unwrap());
    assert!(after
        .panes
        .iter()
        .any(|p| p.id == "inbox" && p.dashboard_ref.scope == DashboardScope::BuiltIn));
}

#[test]
fn recovery_rediscovers_an_unselected_installed_entry_without_scanning_its_layer() {
    let (home, principal, root) = recovery_fixture();
    let source = panes::list_layer(
        &root.join("a/panes"),
        PaneScope::Team,
        "acme/app",
        Some("a"),
    )
    .remove(0);
    let installed = panes::install::choose(home.path(), &principal, &source, true, true).unwrap();
    let mut registry = Registry::default();
    let mut topics = TopicRegistry::default();
    let (listing, _, _) = registry
        .open(
            &mut topics,
            ViewRequest {
                home: home.path(),
                principal: &principal,
                owner: "unselected",
                sequence: 1,
                org: "acme/app",
                team: "b",
                selected: vec![],
                retry: None,
            },
        )
        .unwrap();
    let entry = listing
        .panes
        .iter()
        .find(|p| p.dashboard_ref == installed)
        .unwrap();
    let version = entry.install_source_version.clone();
    let path = PathBuf::from(&entry.path);
    let change = panes::watch::scratch_pane_change(
        &scratch_space::scratch_root(home.path(), &principal),
        None,
        &path,
    )
    .unwrap();
    std::fs::remove_file(&path).unwrap();
    registry.changed(&mut topics, &change).unwrap();
    let reader = registry.views.values().next().unwrap().reader_key();
    assert!(!registry.sources.contains_key(&(reader, installed.key())));
    registry.entry_reads = 0;
    registry.layer_scans = 0;
    std::fs::write(&path, "<title>Restored install</title>").unwrap();
    registry.changed(&mut topics, &change).unwrap();
    let listing = registry.listing(registry.views.values().next().unwrap());
    let entry = listing
        .panes
        .iter()
        .find(|p| p.dashboard_ref == installed)
        .expect("unselected install returns");
    assert_eq!(entry.title, "Restored install");
    assert_eq!(entry.install_source_version, version);
    assert_eq!(registry.entry_reads, 1);
    assert_eq!(registry.layer_scans, 0);
}

#[test]
fn recovery_reparents_through_registry_ingress_and_manual_reload_with_only_new_layers_read() {
    use crate::context_repo::{apply_registry_edit, RegistryEdit};
    let (home, principal, root) = recovery_fixture();
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(super::super::LiveHub::default());
    let request = |sequence, retry| ViewRequest {
        home: home.path(),
        principal: &principal,
        owner: "parent",
        sequence,
        org: "acme/app",
        team: "child",
        selected: vec![],
        retry,
    };
    let before = super::open(app.handle(), request(1, None)).unwrap();
    assert_eq!(before["chain"], json!(["child", "a"]));
    let edit = RegistryEdit::SetTeamParent {
        team_name: "child".into(),
        parent_team: Some("b".into()),
    };
    for (path, bytes) in apply_registry_edit(&edit, &root).unwrap().files {
        std::fs::write(root.join(path), bytes).unwrap();
    }
    crate::commands::registry_local::emit_org_registry_changed(
        app.handle(),
        &principal,
        "acme/app",
        &edit,
    );
    {
        let hub = app.state::<super::super::LiveHub>();
        let model = hub.model.lock().unwrap();
        let view = model.pane_sources.views.values().next().unwrap();
        let listing = model.pane_sources.listing(view);
        assert_eq!(listing.chain, ["child", "b"]);
        assert!(listing.panes.iter().any(|p| p.team.as_deref() == Some("b")));
        assert!(!listing.panes.iter().any(|p| p.team.as_deref() == Some("a")));
        assert_eq!(
            model.pane_sources.layer_scans, 2,
            "only the added B layer is discovered"
        );
    }
    let edit = RegistryEdit::SetTeamParent {
        team_name: "child".into(),
        parent_team: Some("a".into()),
    };
    for (path, bytes) in apply_registry_edit(&edit, &root).unwrap().files {
        std::fs::write(root.join(path), bytes).unwrap();
    }
    let retry = DashboardRef {
        scope: DashboardScope::Team,
        org: "acme/app".into(),
        team: Some("child".into()),
        id: "people".into(),
        installed_from: None,
    };
    let reloaded = super::open(app.handle(), request(2, Some(retry))).unwrap();
    assert_eq!(reloaded["chain"], json!(["child", "a"]));
}

#[cfg(unix)]
#[test]
fn inherited_sources_keep_reader_cache_authority_and_denied_missing_responses() {
    for parent_first in [true, false] {
        let (home, root) = crate::context_repo::inheritance::test_fixture();
        let principal = Principal::from(1u64);
        let shared = DashboardRef {
            scope: DashboardScope::Team,
            org: "acme/ctx".into(),
            team: Some("parent".into()),
            id: "parent".into(),
            installed_from: None,
        };
        let secret = DashboardRef {
            id: "secret".into(),
            ..shared.clone()
        };
        let sibling = DashboardRef {
            team: Some("sibling".into()),
            id: "sibling".into(),
            ..shared.clone()
        };
        let private_alias = DashboardRef {
            scope: DashboardScope::Private,
            team: None,
            id: "private-alias".into(),
            ..shared.clone()
        };
        let secret_path = root.join("parent/panes/secret.html");
        std::fs::write(&secret_path, "<title>PRIVATE_PARENT_SENTINEL</title>").unwrap();
        std::fs::write(root.join("parent/config/private-pane.yaml"), "version: 1\npanes:\n  - id: parent\n    html: parent.html\n    title: FORBIDDEN_MANIFEST_ALIAS_METADATA\n").unwrap();
        let manifest = root.join("parent/panes/pane-manifest.yaml");
        std::os::unix::fs::symlink("../config/private-pane.yaml", &manifest).unwrap();
        let (private, _) =
            scratch_space::ensure_org_space(home.path(), &principal, "acme/ctx").unwrap();
        std::fs::create_dir_all(private.panes_dir()).unwrap();
        std::fs::write(
            private.panes_dir().join("personal.html"),
            "<title>Personal</title>",
        )
        .unwrap();
        std::os::unix::fs::symlink(&secret_path, private.panes_dir().join("private-alias.html"))
            .unwrap();
        let mut registry = Registry::default();
        let mut topics = TopicRegistry::default();
        let open = |registry: &mut Registry,
                    topics: &mut TopicRegistry,
                    team: &str,
                    owner: &str,
                    sequence: u64,
                    retry| {
            registry
                .open(
                    topics,
                    ViewRequest {
                        home: home.path(),
                        principal: &principal,
                        owner,
                        sequence,
                        org: "acme/ctx",
                        team,
                        selected: if team == "child" {
                            vec![
                                shared.clone(),
                                secret.clone(),
                                sibling.clone(),
                                private_alias.clone(),
                            ]
                        } else {
                            vec![]
                        },
                        retry,
                    },
                )
                .unwrap()
        };
        if parent_first {
            open(&mut registry, &mut topics, "parent", "parent", 1, None);
        }
        let (_, child, _) = open(&mut registry, &mut topics, "child", "child", 1, None);
        if !parent_first {
            open(&mut registry, &mut topics, "parent", "parent", 1, None);
        }
        let child_key = child.view_key;
        let check_child = |registry: &Registry, topics: &mut TopicRegistry| {
            let listing = registry.listing(&registry.views[&child_key]);
            let snapshot = topics.snapshot(TOPIC, &child_key).unwrap();
            let serialized = serde_json::to_string(&snapshot.snapshot).unwrap();
            assert!(
                !serialized.contains("PRIVATE_PARENT_SENTINEL"),
                "parent first: {parent_first}"
            );
            assert!(
                !serialized.contains("FORBIDDEN_MANIFEST_ALIAS_METADATA"),
                "parent first: {parent_first}"
            );
            assert!(listing
                .panes
                .iter()
                .any(|p| p.id == "personal" && p.scope == PaneScope::Private));
            assert!(!listing
                .panes
                .iter()
                .any(|p| p.id == "secret" || p.id == "private-alias"));
            let row = |id: String| snapshot.snapshot.iter().find(|r| r["id"] == id).unwrap();
            assert_eq!(row(shared.key())["entry"]["title"], "parent");
            assert_eq!(
                row(shared.key())["contentRoot"],
                root.join("child/content").to_string_lossy().as_ref()
            );
            assert_eq!(
                row(sibling.key())["contentRoot"],
                root.join("sibling/content").to_string_lossy().as_ref()
            );
            assert!(row(secret.key())["entry"].is_null());
            assert!(row(private_alias.key())["entry"].is_null());
            let reader = registry.views[&child_key].reader_key();
            assert!(registry
                .sources
                .iter()
                .filter(|((key, _), _)| key == &reader)
                .all(|(_, source)| source
                    .entry
                    .as_ref()
                    .is_none_or(|entry| entry.id != "secret" && entry.id != "private-alias")));
            (
                serde_json::to_value(listing).unwrap(),
                serde_json::to_value(snapshot).unwrap(),
            )
        };
        check_child(&registry, &mut topics);
        let parent = topics
            .snapshot(TOPIC, &format!("paneSources:{}:parent", principal.as_str()))
            .unwrap();
        assert!(serde_json::to_string(&parent.snapshot)
            .unwrap()
            .contains("FORBIDDEN_MANIFEST_ALIAS_METADATA"));
        std::fs::write(
            &secret_path,
            "<title>PRIVATE_PARENT_SENTINEL edited</title>",
        )
        .unwrap();
        let secret_change = panes::watch::team_pane_change(
            &root.join("parent/panes"),
            "acme/ctx",
            "parent",
            &secret_path,
        )
        .unwrap();
        let deltas = registry.changed(&mut topics, &secret_change).unwrap();
        assert!(
            !deltas.iter().any(|d| d.view_key == child_key),
            "unshared entry edits do not change child revisions"
        );
        let manifest_change = panes::watch::team_pane_change(
            &root.join("parent/panes"),
            "acme/ctx",
            "parent",
            &manifest,
        )
        .unwrap();
        registry.changed(&mut topics, &manifest_change).unwrap();
        open(
            &mut registry,
            &mut topics,
            "child",
            "child",
            2,
            Some(shared.clone()),
        );
        let denied = check_child(&registry, &mut topics);
        std::fs::remove_file(&secret_path).unwrap();
        registry.changed(&mut topics, &secret_change).unwrap();
        open(
            &mut registry,
            &mut topics,
            "child",
            "child",
            3,
            Some(secret.clone()),
        );
        assert_eq!(
            check_child(&registry, &mut topics),
            denied,
            "denied and missing include identical entries, roots, overlays and revisions"
        );
    }
}

/// One reader that opens org `acme/app` (a team-owned pane `people`), leaves for `acme/other`
/// (which prunes the pane's source), and returns.
struct AwayAndBack {
    home: tempfile::TempDir,
    principal: Principal,
    registry: Registry,
    topics: super::super::projection::TopicRegistry,
    sequence: u64,
    pane: DashboardRef,
}

impl AwayAndBack {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let principal = Principal::from(7u64);
        let root = path_to_org_root(home.path(), &principal, "acme/app").unwrap();
        org_metadata(&root);
        std::fs::create_dir_all(root.join("a/panes")).unwrap();
        std::fs::write(root.join("teams.yml"), "teams:\n  - name: a\n").unwrap();
        std::fs::write(root.join("a/panes/people.html"), "<title>People</title>").unwrap();
        let other = path_to_org_root(home.path(), &principal, "acme/other").unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(
            other.join("org.yml"),
            "slug: acme/other\ndisplay_name: Other\ncreated_at: '2026-01-01T00:00:00Z'\n",
        )
        .unwrap();
        std::fs::write(other.join("people.yml"), "people: []\n").unwrap();
        std::fs::create_dir_all(other.join("c")).unwrap();
        std::fs::write(other.join("teams.yml"), "teams:\n  - name: c\n").unwrap();
        let pane = DashboardRef {
            scope: DashboardScope::Team,
            org: "acme/app".into(),
            team: Some("a".into()),
            id: "people".into(),
            installed_from: None,
        };
        Self {
            home,
            principal,
            registry: Registry::default(),
            topics: super::super::projection::TopicRegistry::default(),
            sequence: 0,
            pane,
        }
    }

    fn open(&mut self, org: &str, team: &str) -> Snapshot {
        self.sequence += 1;
        let selected = if org == "acme/app" {
            vec![self.pane.clone()]
        } else {
            Vec::new()
        };
        self.registry
            .open(
                &mut self.topics,
                ViewRequest {
                    home: self.home.path(),
                    principal: &self.principal,
                    owner: "reader",
                    sequence: self.sequence,
                    org,
                    team,
                    selected,
                    retry: None,
                },
            )
            .unwrap()
            .1
    }

    fn pane_html(&self) -> PathBuf {
        path_to_org_root(self.home.path(), &self.principal, "acme/app")
            .unwrap()
            .join("a/panes/people.html")
    }

    fn revision(&self, snapshot: &Snapshot) -> u64 {
        snapshot
            .snapshot
            .iter()
            .find(|row| row["id"] == self.pane.key())
            .unwrap()["documentRevision"]
            .as_u64()
            .unwrap()
    }

    fn away(&mut self) {
        self.open("acme/other", "c");
    }
}

#[test]
fn a_pruned_document_returning_unchanged_keeps_its_document_revision() {
    let mut f = AwayAndBack::new();
    let first = f.open("acme/app", "a");
    let before = f.revision(&first);
    f.away();
    let back = f.open("acme/app", "a");
    assert_eq!(f.revision(&back), before);
}

#[test]
fn a_pruned_document_returning_changed_advances_its_document_revision() {
    let mut f = AwayAndBack::new();
    let first = f.open("acme/app", "a");
    let before = f.revision(&first);
    f.away();
    std::fs::write(f.pane_html(), "<title>Members</title>").unwrap();
    let back = f.open("acme/app", "a");
    assert!(f.revision(&back) > before);
}

#[test]
fn an_asset_change_while_a_document_is_pruned_advances_its_document_revision() {
    let mut f = AwayAndBack::new();
    let first = f.open("acme/app", "a");
    let before = f.revision(&first);
    f.away();
    f.registry
        .changed(
            &mut f.topics,
            &PaneChange {
                org: "acme/app".into(),
                scope: PaneScope::Team,
                team: Some("a".into()),
                id: None,
                installed: None,
                declarations: false,
            },
        )
        .unwrap();
    let back = f.open("acme/app", "a");
    assert!(f.revision(&back) > before);
}

#[test]
fn an_installed_edit_preserves_unrelated_pruned_private_documents() {
    let mut f = AwayAndBack::new();
    let root = path_to_org_root(f.home.path(), &f.principal, "acme/app").unwrap();
    let source = panes::list_layer(
        &root.join("a/panes"),
        PaneScope::Team,
        "acme/app",
        Some("a"),
    )
    .remove(0);
    let installed =
        panes::install::choose(f.home.path(), &f.principal, &source, true, true).unwrap();
    let panes_dir = scratch_space::org_space_at(f.home.path(), &f.principal, "acme/app")
        .unwrap()
        .panes_dir();
    std::fs::write(panes_dir.join("inbox.html"), "<title>Inbox</title>").unwrap();
    let inbox = DashboardRef {
        scope: DashboardScope::Private,
        org: "acme/app".into(),
        team: None,
        id: "inbox".into(),
        installed_from: None,
    };
    let row = |snapshot: &Snapshot, reference: &DashboardRef| {
        snapshot
            .snapshot
            .iter()
            .find(|row| row["id"] == reference.key())
            .unwrap()
            .clone()
    };
    let revision = |snapshot: &Snapshot, reference: &DashboardRef| {
        row(snapshot, reference)["documentRevision"]
            .as_u64()
            .unwrap()
    };
    let first = f.open("acme/app", "a");
    let path = PathBuf::from(row(&first, &installed)["entry"]["path"].as_str().unwrap());
    f.away();
    for reference in [&installed, &inbox] {
        assert!(f.registry.pruned.iter().any(|p| p.key == reference.key()));
    }

    std::fs::write(&path, "<title>Edited installed pane</title>").unwrap();
    let scratch = scratch_space::scratch_root(f.home.path(), &f.principal);
    let change = panes::watch::scratch_pane_change(&scratch, None, &path).unwrap();
    assert_eq!(change.installed, Some(("a".into(), "people".into())));
    assert!(change.id.is_none());
    f.registry.changed(&mut f.topics, &change).unwrap();
    let back = f.open("acme/app", "a");
    assert!(revision(&back, &installed) > revision(&first, &installed));
    assert_eq!(revision(&back, &inbox), revision(&first, &inbox));

    // An asset in the private layer has no installed identity and still invalidates both panes.
    f.away();
    let asset = panes_dir.join("shared.js");
    std::fs::write(&asset, "export const value = 1;").unwrap();
    let change = panes::watch::scratch_pane_change(&scratch, None, &asset).unwrap();
    assert!(change.installed.is_none() && change.id.is_none());
    f.registry.changed(&mut f.topics, &change).unwrap();
    let refreshed = f.open("acme/app", "a");
    for reference in [&installed, &inbox] {
        assert!(revision(&refreshed, reference) > revision(&back, reference));
    }
}

#[test]
fn the_pruned_document_memory_is_bounded() {
    let mut f = AwayAndBack::new();
    let reference = f.pane.clone();
    let entry = {
        let first = f.open("acme/app", "a");
        let _ = first;
        f.registry
            .sources
            .values()
            .find_map(|s| s.entry.clone())
            .unwrap()
    };
    for n in 0..(PRUNED_DOCUMENT_CAP * 2) {
        let mut reference = reference.clone();
        reference.id = format!("pane-{n}");
        f.registry
            .remember_pruned(reference.key(), entry.clone(), n as u64);
    }
    assert_eq!(f.registry.pruned.len(), PRUNED_DOCUMENT_CAP);
}

#[test]
fn an_evicted_document_returning_never_reuses_a_revision() {
    for edit_while_away in [false, true] {
        let mut f = AwayAndBack::new();
        let first = f.open("acme/app", "a");
        let before = f.revision(&first);
        let entry = f
            .registry
            .sources
            .values()
            .find_map(|s| s.entry.clone())
            .unwrap();
        f.away();
        assert!(f.registry.pruned.iter().any(|p| p.key == f.pane.key()));
        for n in 0..PRUNED_DOCUMENT_CAP {
            let mut other = f.pane.clone();
            other.id = format!("filler-{n}");
            f.registry
                .remember_pruned(other.key(), entry.clone(), n as u64);
        }
        assert!(!f.registry.pruned.iter().any(|p| p.key == f.pane.key()));
        if edit_while_away {
            std::fs::write(f.pane_html(), "<title>Members</title>").unwrap();
        }
        let back = f.open("acme/app", "a");
        assert!(
            f.revision(&back) > before,
            "an evicted document renumbers (edited: {edit_while_away})"
        );
    }
}

#[test]
fn a_registry_edit_drops_the_remembered_documents_of_its_org_only() {
    let mut f = AwayAndBack::new();
    f.open("acme/app", "a");
    let entry = f
        .registry
        .sources
        .values()
        .find_map(|s| s.entry.clone())
        .unwrap();
    f.away();
    let mut elsewhere = f.pane.clone();
    elsewhere.org = "acme/other".into();
    let mut elsewhere_entry = entry;
    elsewhere_entry.dashboard_ref = elsewhere.clone();
    f.registry
        .remember_pruned(elsewhere.key(), elsewhere_entry, 1);
    assert!(f.registry.pruned.iter().any(|p| p.key == f.pane.key()));
    f.registry
        .registry_changed(&mut f.topics, &f.principal, "acme/app")
        .unwrap();
    assert!(!f
        .registry
        .pruned
        .iter()
        .any(|p| p.reference.org == "acme/app"));
    assert!(f.registry.pruned.iter().any(|p| p.key == elsewhere.key()));
}

fn registered_pane_recovery(shape: &str) {
    let mut f = AwayAndBack::new();
    let pane_path = if matches!(shape, "private-root" | "activation-private") {
        f.pane.scope = DashboardScope::Private;
        f.pane.team = None;
        let dir = scratch_space::org_space_at(f.home.path(), &f.principal, "acme/app")
            .unwrap()
            .panes_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("people.html");
        std::fs::write(&path, "<title>Private people</title>").unwrap();
        path
    } else if shape == "activation-inherited" {
        let root = path_to_org_root(f.home.path(), &f.principal, "acme/app").unwrap();
        std::fs::write(
            root.join("teams.yml"),
            "teams:\n  - name: a\n    parent_team: b\n  - name: b\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("b/panes")).unwrap();
        std::fs::create_dir_all(root.join("b/config")).unwrap();
        std::fs::write(root.join("b/config/sharing.yml"), "share: ['panes/**']\n").unwrap();
        f.pane.team = Some("b".into());
        let path = root.join("b/panes/people.html");
        std::fs::write(&path, "<title>Inherited people</title>").unwrap();
        path
    } else {
        f.pane_html()
    };
    let before = f.open("acme/app", "a");
    let revision = f.revision(&before);
    let root = path_to_org_root(f.home.path(), &f.principal, "acme/app").unwrap();
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(crate::commands::watch::WatcherState::default());
    let hub = super::super::LiveHub::default();
    {
        let mut model = hub.model.lock().unwrap();
        model.pane_sources = std::mem::take(&mut f.registry);
        model.projections = std::mem::take(&mut f.topics);
    }
    app.manage(hub);
    let state = app.state::<crate::commands::watch::WatcherState>();
    if shape != "activation-before-start" {
        crate::commands::watch::start_watching_impl(app.handle().clone(), vec![], 0).unwrap();
        crate::commands::watch::register_worktree_move_watch(
            app.handle(),
            &state,
            &root,
            "acme/app",
        );
        if shape == "private-root" {
            crate::commands::scratch_watch::register_scratch_watch(
                app.handle(),
                &state,
                f.home.path(),
                7,
            );
        }
    }
    // Keep the real registrations, but exclude live kernel delivery from deterministic tests.
    if let Some(watcher) = state.watcher_handle() {
        for watched in watcher.watched_paths().unwrap() {
            watcher.unwatch(&watched).unwrap();
        }
    }
    let hub = app.state::<super::super::LiveHub>();
    let current = || {
        let model = hub.model.lock().unwrap();
        let source = model
            .pane_sources
            .sources
            .values()
            .find(|s| s.reference == f.pane)
            .unwrap();
        (source.revision, source.document_revision)
    };
    let layer_scans = || hub.model.lock().unwrap().pane_sources.layer_scans;
    let before_recovery = current();
    let route = || {
        if shape.starts_with("activation") {
            crate::commands::watch::register_team_activation_watches(
                app.handle(),
                &state,
                &root,
                "acme/app",
                "a",
            );
        } else {
            use notify::{
                event::{Flag, ModifyKind},
                Event, EventKind,
            };
            let mut event = Event::new(EventKind::Modify(ModifyKind::Any));
            if shape != "global-rescan" {
                let path = if shape == "private-root" {
                    scratch_space::scratch_root(f.home.path(), &f.principal)
                } else {
                    let path = root.join(if shape == "directory" { "a/panes" } else { "a" });
                    if shape == "canonical-rescan" {
                        path.canonicalize().unwrap()
                    } else {
                        path
                    }
                };
                event = event.add_path(path);
            }
            if shape.ends_with("rescan") {
                event = event.set_flag(Flag::Rescan);
            }
            let scans = layer_scans();
            crate::content_engine::watcher::route_test_event(&state.dispatcher, event);
            if matches!(shape, "root" | "directory") {
                // A team-dir event reaches the source on the pane coalescer's own thread after
                // its quiet period. Wait for that layer scan, or it can land after a later snapshot.
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                while layer_scans() == scans {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "{shape}: the pane layer scan never ran"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
            } else {
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    };
    route();
    assert_eq!(
        current().1,
        revision,
        "{shape}: unchanged recovery reminted the document"
    );
    if shape != "directory" && shape != "root" {
        assert_eq!(
            current(),
            before_recovery,
            "{shape}: unchanged recovery reminted the source"
        );
    }
    std::fs::write(&pane_path, "<title>Changed while away</title>").unwrap();
    route();
    assert!(
        current().1 > revision,
        "{shape}: did not reread the retained source"
    );
    let changed = current();
    route();
    assert_eq!(
        current().1,
        changed.1,
        "{shape}: duplicate recovery reminted an unchanged document"
    );
    if shape != "directory" && shape != "root" {
        assert_eq!(
            current(),
            changed,
            "{shape}: duplicate recovery reminted an unchanged source"
        );
    }
    let before_unrelated = current();
    crate::commands::watch::register_team_activation_watches(
        app.handle(),
        &state,
        &root,
        "acme/app",
        "unrelated",
    );
    assert_eq!(
        current(),
        before_unrelated,
        "unrelated team activation refreshed this source"
    );
    if shape == "private-root" {
        let sibling = pane_path
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("content/sibling.md");
        crate::content_engine::watcher::route_test_event(
            &state.dispatcher,
            notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
                .add_path(sibling),
        );
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(
            current(),
            before_unrelated,
            "private content sibling refreshed a pane"
        );
    }
}
#[test]
fn pbf211_activation_recovers_retained_panes() {
    registered_pane_recovery("activation");
}
#[test]
fn pbf211_directory_recovers_retained_panes() {
    registered_pane_recovery("directory");
}
#[test]
fn pbf211_team_root_recovers_retained_panes() {
    registered_pane_recovery("root");
}
#[test]
fn pbf211_scoped_rescan_recovers_retained_panes() {
    registered_pane_recovery("scoped-rescan");
}
#[test]
fn pbf211_global_rescan_recovers_retained_panes() {
    registered_pane_recovery("global-rescan");
}

#[test]
fn pbf211_activation_before_start_recovers_retained_panes() {
    registered_pane_recovery("activation-before-start");
}

#[test]
fn pbf211_private_root_recovers_retained_panes() {
    registered_pane_recovery("private-root");
}

#[test]
fn pbf211_activation_recovers_selected_private_panes() {
    registered_pane_recovery("activation-private");
}
#[test]
fn pbf211_activation_recovers_selected_inherited_panes() {
    registered_pane_recovery("activation-inherited");
}

#[test]
fn pbf211_shared_pane_rescan_keeps_document_revisions_equal_across_readers() {
    let (home, principal, root) = recovery_fixture();
    let reference = DashboardRef {
        scope: DashboardScope::Team,
        org: "acme/app".into(),
        team: Some("a".into()),
        id: "people".into(),
        installed_from: None,
    };
    let mut registry = Registry::default();
    let mut topics = TopicRegistry::default();
    for team in ["child", "a"] {
        registry
            .open(
                &mut topics,
                ViewRequest {
                    home: home.path(),
                    principal: &principal,
                    // Iterate the child first, opposite the reader-key/source ordering.
                    owner: if team == "child" { "first" } else { "second" },
                    sequence: 1,
                    org: "acme/app",
                    team,
                    selected: vec![reference.clone()],
                    retry: None,
                },
            )
            .unwrap();
    }
    let before = registry.document_revisions[&reference.key()];
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(crate::commands::watch::WatcherState::default());
    let hub = super::super::LiveHub::default();
    {
        let mut model = hub.model.lock().unwrap();
        model.pane_sources = registry;
        model.projections = topics;
    }
    app.manage(hub);
    crate::commands::watch::start_watching_impl(app.handle().clone(), vec![], 0).unwrap();
    std::fs::write(
        root.join("a/panes/people.html"),
        "<title>Changed shared pane</title>",
    )
    .unwrap();
    let state = app.state::<crate::commands::watch::WatcherState>();
    crate::content_engine::watcher::route_test_event(
        &state.dispatcher,
        notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
            .add_path(root.join("a/panes"))
            .set_flag(notify::event::Flag::Rescan),
    );
    let hub = app.state::<super::super::LiveHub>();
    let model = hub.model.lock().unwrap();
    let revisions: Vec<_> = model
        .pane_sources
        .sources
        .values()
        .filter(|source| source.reference.key() == reference.key())
        .map(|source| source.document_revision)
        .collect();
    assert_eq!(revisions.len(), 2);
    assert!(
        revisions[0] > before,
        "rescan did not recover the changed shared pane"
    );
    assert_eq!(
        revisions[0], revisions[1],
        "readers disagree on their shared document revision"
    );
}

#[test]
fn pbf211_canonical_rescan_recovers_retained_panes() {
    registered_pane_recovery("canonical-rescan");
}
