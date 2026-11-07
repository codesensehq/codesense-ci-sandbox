// SPDX-License-Identifier: MIT
//! Seam tests for the context query engine: a real on-disk footprint (two
//! orgs, a private space per org, all-orgs content) indexed by `IndexState`
//! and queried through `Snapshot::query` — the exact path the MCP tool and the
//! `query_context` command take.

use std::path::{Path, PathBuf};
use tauri::Manager;

use serde_json::json;

use super::engine::{BodyReader, DiskBodyReader, QueryError, Viewer};
use super::index::{IndexRoot, IndexState, Snapshot};
use super::model::{ContextQuery, ContextQueryResult, Kind, Layer};

fn write(p: &Path, s: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, s).unwrap();
}

struct Footprint {
    _tmp: tempfile::TempDir,
    roots: Vec<IndexRoot>,
    acme_team: PathBuf,
}

fn story(id: &str, status: &str, extra: &str) -> String {
    format!("---\ntype: story\nstory_id: {id}\nstatus: {status}\n{extra}---\n# {id} title\n\nBody of {id}.\n")
}

/// Two orgs (`acme`, `globex`), each with a team repo and a private space,
/// plus all-orgs content.
fn footprint() -> Footprint {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path();
    let acme_team = base.join("orgs/acme/ctx/default/content");
    let globex_team = base.join("orgs/globex/ctx/default/content");
    let acme_private = base.join("scratch/orgs/acme/content");
    let globex_private = base.join("scratch/orgs/globex/content");
    let all_orgs = base.join("scratch/all-orgs");

    let epic = acme_team.join("_bmad-output/13-onboarding");
    write(
        &epic.join("README.md"),
        "---\nstatus: in-progress\nrepos: [acme/app]\n---\n# Onboarding epic\n",
    );
    write(
        &epic.join("OC-1-login.md"),
        &story("OC-1", "todo", "parent_epic: SW-OC\nassigned_to: alice\npriority: p2\npoints: 3\ntags: [ux, auth]\nsupporting_docs:\n  - ./notes.md\n"),
    );
    write(
        &epic.join("OC-2-signup.md"),
        &story(
            "OC-2",
            "in-progress",
            "parent_epic: SW-OC\nassigned_to: alice\npriority: p0\npoints: 5\ndepends_on: [OC-1]\n",
        ),
    );
    write(
        &epic.join("OC-3-done.md"),
        &story(
            "OC-3",
            "done",
            "parent_epic: SW-OC\nassigned_to: alice\npriority: p1\n",
        ),
    );
    write(
        &epic.join("OC-4-bob.md"),
        &story(
            "OC-4",
            "todo",
            "parent_epic: SW-OC\nassigned_to: bob\npriority: p1\n",
        ),
    );
    write(&epic.join("notes.md"), "# Notes\n\nThe needle is here.\n");
    write(
        &epic.join("board.html"),
        "<html><head><meta name=\"cs:type\" content=\"dashboard\"><meta name=\"cs:depends-on\" content=\"OC-1, OC-2\"><title>Board</title></head></html>",
    );
    write(&epic.join("diagram.png"), "png");
    write(
        &globex_team.join("_bmad-output/x/GX-1.md"),
        &story("GX-1", "todo", "assigned_to: alice\n"),
    );
    write(
        &acme_private.join("draft.md"),
        &story("ACME-PRIV", "todo", "assigned_to: alice\n"),
    );
    write(
        &globex_private.join("secret.md"),
        &story("GLOBEX-PRIV", "todo", "assigned_to: alice\n"),
    );
    write(
        &all_orgs.join(".claude/skills/hello/SKILL.md"),
        "---\nname: hello\n---\n# Hello skill\n",
    );

    let root = |dir: PathBuf, org: Option<&str>, team: Option<&str>, layer| IndexRoot {
        dir,
        org: org.map(String::from),
        team: team.map(String::from),
        layer,
    };
    let roots = vec![
        root(
            acme_team.clone(),
            Some("acme"),
            Some("default"),
            Layer::Team,
        ),
        root(globex_team, Some("globex"), Some("default"), Layer::Team),
        root(acme_private, Some("acme"), None, Layer::Private),
        root(globex_private, Some("globex"), None, Layer::Private),
        root(all_orgs, None, None, Layer::AllOrgs),
    ];
    Footprint {
        _tmp: tmp,
        roots,
        acme_team,
    }
}

fn index(fp: &Footprint) -> (IndexState, Snapshot) {
    let mut state = IndexState::default();
    state.reconcile(fp.roots.clone());
    let snap = state.snapshot(1, true);
    (state, snap)
}

fn alice() -> Viewer {
    Viewer {
        identities: vec!["alice".into()],
        ..Default::default()
    }
}

fn run(snap: &Snapshot, q: serde_json::Value) -> ContextQueryResult {
    let q: ContextQuery = serde_json::from_value(q).expect("query parses");
    snap.query(&q, &alice(), &DiskBodyReader)
        .expect("query runs")
}

fn ids(r: &ContextQueryResult) -> Vec<String> {
    r.rows.iter().map(|row| row.id.clone()).collect()
}

#[test]
fn kernel_rescans_reconcile_the_shared_index_without_forward_dispatch() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::Duration;

    use notify::event::{Flag, ModifyKind};
    use notify::{Event, EventKind};

    use super::service::{ContextIndexService, Footprint as ServiceFootprint, IndexChange};
    use crate::content_engine::watcher::route_test_event;
    use crate::content_engine::Dispatcher;

    fn assert_published_row(service: &ContextIndexService, change: &IndexChange, path: &Path) {
        let result = super::run_query(service, &ContextQuery::default(), &Viewer::default())
            .expect("the shared query reads the published snapshot");
        assert!(!result.indexing);
        assert_eq!(result.generation, change.generation);
        assert_eq!(change.orgs, vec!["acme".to_string()]);
        let path = path.to_string_lossy();
        assert!(change.paths.iter().any(|p| p == path.as_ref()));
        assert!(result
            .rows
            .iter()
            .any(|row| { row.path == path.as_ref() && row.org.as_deref() == Some("acme") }));
    }

    for shape in ["subtree", "ancestor", "pathless"] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("acme/default/content");
        write(&root.join("README.md"), "# Initial context\n");
        let service = ContextIndexService::new();
        let dispatcher = Dispatcher::new(0, 100, 60_000, 500);
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(service.clone());
        super::bind_rescan_reconciliation(app.handle(), &dispatcher);

        let full_walks = Arc::new(AtomicUsize::new(0));
        let walks = Arc::clone(&full_walks);
        let indexed_root = root.clone();
        let (tx, rx) = mpsc::channel();
        service.start(
            Arc::new(move || {
                walks.fetch_add(1, Ordering::SeqCst);
                Some(ServiceFootprint {
                    owner: 1,
                    roots: vec![IndexRoot {
                        dir: indexed_root.clone(),
                        org: Some("acme".into()),
                        team: Some("default".into()),
                        layer: Layer::Team,
                    }],
                    summary_headings: Vec::new(),
                })
            }),
            Arc::new(move |change| {
                let _ = tx.send(change);
            }),
        );
        let initial = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(initial.generation, 1);
        assert!(service.snapshot().ready);

        let normal_paths = Arc::new(Mutex::new(Vec::new()));
        let delivered = Arc::clone(&normal_paths);
        let normal_index = service.clone();
        dispatcher.register_handler(
            &root,
            Arc::new(move |path| {
                delivered.lock().unwrap().push(path.to_path_buf());
                normal_index.note_paths([path.to_path_buf()]);
            }),
        );
        let reverse_paths = Arc::new(Mutex::new(Vec::new()));
        let rescanned = Arc::clone(&reverse_paths);
        dispatcher.register_rescan_handler(
            &root,
            Arc::new(move |path| rescanned.lock().unwrap().push(path.to_path_buf())),
        );

        let ordinary = root.join("ordinary.md");
        write(&ordinary, "# Ordinary change\n");
        route_test_event(
            &dispatcher,
            Event::new(EventKind::Modify(ModifyKind::Any)).add_path(ordinary.clone()),
        );
        let normal_change = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_published_row(&service, &normal_change, &ordinary);
        assert_eq!(full_walks.load(Ordering::SeqCst), 1);
        assert_eq!(*normal_paths.lock().unwrap(), vec![ordinary]);
        normal_paths.lock().unwrap().clear();
        assert!(reverse_paths.lock().unwrap().is_empty());

        // No native watcher is attached: only the production rescan route can
        // discover this file before the worker's sixty-second backstop.
        let delta = root.join("updates/rescan-delta.md");
        write(&delta, "# Rescan discovery\n");
        let event = Event::new(EventKind::Other).set_flag(Flag::Rescan);
        let event = match shape {
            "subtree" => event.add_path(root.join("updates")),
            "ancestor" => event.add_path(tmp.path().to_path_buf()),
            "pathless" => event,
            _ => unreachable!(),
        };
        route_test_event(&dispatcher, event);
        let recovered = rx
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|e| panic!("{shape} rescan did not publish the new context row: {e}"));
        assert!(recovered.generation > normal_change.generation);
        assert_published_row(&service, &recovered, &delta);
        assert!(normal_paths.lock().unwrap().is_empty());
        let reverse_count = if shape == "ancestor" { 0 } else { 1 };
        assert_eq!(reverse_paths.lock().unwrap().len(), reverse_count);

        // Closing a directory subscription must not detach footprint-wide
        // metadata recovery from the app's dispatcher.
        dispatcher.unregister_handler(&root);
        let after_close = root.join("after-close.md");
        write(&after_close, "# Still in the synced footprint\n");
        route_test_event(
            &dispatcher,
            Event::new(EventKind::Other).set_flag(Flag::Rescan),
        );
        let after_close_change = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(after_close_change.generation > recovered.generation);
        assert_published_row(&service, &after_close_change, &after_close);
        assert!(normal_paths.lock().unwrap().is_empty());
        assert_eq!(reverse_paths.lock().unwrap().len(), reverse_count);
    }
}

#[test]
fn recovered_frontmatter_stays_queryable_with_relationships() {
    let fp = footprint();
    let dir = fp.acme_team.join("recovery");
    write(
        &dir.join("a.md"),
        &story(
            "REC-A",
            "todo",
            "title: Inbox: first pass\ndepends_on: [REC-B]\n",
        ),
    );
    write(&dir.join("b.md"), &story("REC-B", "done", ""));
    write(
        &dir.join("c.md"),
        &story("REC-C", "todo", "depends_on: [REC-A]\n"),
    );
    let (_, snap) = index(&fp);
    let result = run(&snap, json!({"where": {"orgs": ["acme"]}, "limit": 1000}));
    let row = result
        .rows
        .iter()
        .find(|r| r.fields.get("story_id") == Some(&json!("REC-A")))
        .expect("recovered story returned by query_context");
    assert_eq!(row.fields["status"], "todo");
    assert_eq!(row.fields["depends_on"], json!(["REC-B"]));
    assert_eq!(row.fields["title"], "Inbox: first pass");
    let relations = row.relationships.as_ref().unwrap();
    assert_eq!(relations.depends_on.len(), 1);
    assert!(relations.depends_on[0].resolved);
    assert_eq!(relations.dependents.len(), 1);
    assert!(relations.dependents[0].resolved);
    assert_eq!(
        std::fs::read_to_string(dir.join("a.md")).unwrap(),
        story(
            "REC-A",
            "todo",
            "title: Inbox: first pass\ndepends_on: [REC-B]\n"
        )
    );
}

#[test]
fn indexes_every_file_with_org_team_layer_and_kind() {
    let fp = footprint();
    let (_, snap) = index(&fp);
    // Footprint-wide: every team + all-orgs file (private items need one named org).
    let r = run(&snap, json!({ "limit": 1000 }));
    assert_eq!(r.total, 10, "every non-private file: {:?}", ids(&r));
    let by_id = |id: &str| r.rows.iter().find(|row| row.id == id).unwrap();
    let oc1 = by_id("OC-1");
    assert_eq!(oc1.org.as_deref(), Some("acme"));
    assert_eq!(oc1.team.as_deref(), Some("default"));
    assert_eq!(oc1.layer, Layer::Team);
    assert_eq!(oc1.kind, Kind::Story);
    assert_eq!(oc1.title.as_deref(), Some("OC-1 title"));
    assert_eq!(oc1.fields["tags"], json!(["ux", "auth"]));
    assert_eq!(by_id("README.md").kind, Kind::Epic);
    assert_eq!(by_id("notes.md").kind, Kind::Doc);
    assert_eq!(by_id("board.html").kind, Kind::Dashboard);
    assert_eq!(
        by_id("board.html").fields["depends_on"],
        json!(["OC-1", "OC-2"])
    );
    assert_eq!(by_id("diagram.png").kind, Kind::Other);
    assert_eq!(by_id("SKILL.md").kind, Kind::Skill);
    assert_eq!(by_id("SKILL.md").layer, Layer::AllOrgs);
    assert_eq!(by_id("SKILL.md").org, None);
    let acme = run(
        &snap,
        json!({ "where": { "orgs": ["acme"] }, "limit": 1000 }),
    );
    let priv_row = acme.rows.iter().find(|row| row.id == "ACME-PRIV").unwrap();
    assert_eq!(priv_row.layer, Layer::Private);
    assert_eq!(priv_row.team, None);
    assert_eq!(priv_row.org.as_deref(), Some("acme"));
}

#[test]
fn ac2_stories_in_epic_by_status_assigned_to_me_sorted_by_priority_grouped_by_status() {
    let fp = footprint();
    let (_, snap) = index(&fp);
    let r = run(
        &snap,
        json!({
            "where": { "kinds": ["story"] },
            "relationships": { "parentEpic": "SW-OC" },
            "filters": [
                { "key": "status", "op": "in", "value": ["todo", "in-progress"] },
                { "key": "assigned_to", "op": "eq", "value": "@me" }
            ],
            "sort": ["priority"],
            "groupBy": "status"
        }),
    );
    assert_eq!(
        ids(&r),
        vec!["OC-2", "OC-1"],
        "p0 before p2; done + bob excluded"
    );
    let groups: Vec<(Option<String>, usize)> = r
        .groups
        .unwrap()
        .into_iter()
        .map(|g| (g.value, g.count))
        .collect();
    assert_eq!(
        groups,
        vec![(Some("todo".into()), 1), (Some("in-progress".into()), 1)],
        "groups follow the status vocabulary order"
    );
}

#[test]
fn footprint_wide_by_default_and_org_scope_is_explicit() {
    let fp = footprint();
    let (_, snap) = index(&fp);
    let all = run(
        &snap,
        json!({ "match": { "assigned_to": "alice" }, "limit": 100 }),
    );
    let all_ids = ids(&all);
    assert!(
        all_ids.contains(&"GX-1".to_string()),
        "other orgs by default"
    );
    assert!(all_ids.contains(&"OC-1".to_string()));

    let acme = run(
        &snap,
        json!({ "where": { "orgs": ["acme"] }, "match": { "assigned_to": "alice" }, "limit": 100 }),
    );
    assert!(!ids(&acme).contains(&"GX-1".to_string()));
}

#[test]
fn private_items_never_leak_across_orgs_and_all_orgs_items_show_everywhere() {
    let fp = footprint();
    let (_, snap) = index(&fp);
    let acme = run(&snap, json!({ "where": { "orgs": "acme" }, "limit": 100 }));
    let acme_ids = ids(&acme);
    assert!(
        acme_ids.contains(&"ACME-PRIV".to_string()),
        "own private space"
    );
    assert!(
        !acme_ids.contains(&"GLOBEX-PRIV".to_string()),
        "another org's private item must never appear"
    );
    assert!(
        acme_ids.contains(&"SKILL.md".to_string()),
        "all-orgs content"
    );

    let globex = run(
        &snap,
        json!({ "where": { "orgs": ["globex"] }, "limit": 100 }),
    );
    assert!(!ids(&globex).contains(&"ACME-PRIV".to_string()));
    assert!(ids(&globex).contains(&"SKILL.md".to_string()));

    // `sameOrgAs` (the csquery UI default) scopes to the anchor file's org.
    let anchor = fp.acme_team.join("_bmad-output/13-onboarding/README.md");
    let same = run(
        &snap,
        json!({ "where": { "sameOrgAs": anchor.to_string_lossy() }, "limit": 100 }),
    );
    assert!(!ids(&same).contains(&"GLOBEX-PRIV".to_string()));
    assert!(!ids(&same).contains(&"GX-1".to_string()));
    assert!(ids(&same).contains(&"ACME-PRIV".to_string()));
}

#[test]
fn filter_operators() {
    let fp = footprint();
    let (_, snap) = index(&fp);
    let q = |filters: serde_json::Value| {
        ids(&run(
            &snap,
            json!({ "where": { "orgs": ["acme"], "teams": ["default"], "kinds": ["story"] }, "filters": filters }),
        ))
    };
    assert_eq!(
        q(json!([{ "key": "status", "op": "not", "value": ["done", "in-progress"] }])),
        vec!["OC-1", "OC-4"]
    );
    assert_eq!(
        q(json!([{ "key": "depends_on", "op": "exists" }])),
        vec!["OC-2"]
    );
    assert_eq!(
        q(json!([{ "key": "tags", "op": "missing" }])),
        vec!["OC-2", "OC-3", "OC-4"]
    );
    assert_eq!(
        q(json!([{ "key": "tags", "op": "contains", "value": "UX" }])),
        vec!["OC-1"]
    );
    assert_eq!(
        q(json!([{ "key": "title", "op": "contains", "value": "oc-3" }])),
        vec!["OC-3"]
    );
    assert_eq!(
        q(json!([{ "key": "points", "op": "range", "min": 4 }])),
        vec!["OC-2"]
    );
    assert_eq!(
        q(json!([{ "key": "points", "op": "range", "min": 1, "max": 3 }])),
        vec!["OC-1"]
    );
    assert_eq!(
        q(json!([{ "key": "storyId", "op": "prefix", "value": "OC-" }])).len(),
        4
    );
    // OR within a key, AND across keys.
    assert_eq!(
        q(json!([
            { "key": "status", "op": "eq", "value": "done" },
            { "key": "status", "op": "eq", "value": "in-progress" },
            { "key": "assigned-to", "op": "eq", "value": "alice" }
        ])),
        vec!["OC-2", "OC-3"]
    );
    // Dates/text compare lexically in a range.
    let mtime_any = q(json!([{ "key": "mtime", "op": "range", "min": 0 }]));
    assert_eq!(mtime_any.len(), 4);
}

#[test]
fn relationships_resolve_across_the_index() {
    let fp = footprint();
    let (_, snap) = index(&fp);
    let deps_on_oc1 = run(&snap, json!({ "relationships": { "dependsOn": "OC-1" } }));
    // The HTML board's `cs:depends-on` resolves too (any file can depend on a story).
    assert_eq!(ids(&deps_on_oc1), vec!["OC-2", "board.html"]);
    let backlinked = run(
        &snap,
        json!({ "relationships": { "backlinkedBy": "OC-1" } }),
    );
    assert_eq!(ids(&backlinked), vec!["notes.md"]);
    let deps_of_oc2 = run(
        &snap,
        json!({ "relationships": { "dependedOnBy": "OC-2" } }),
    );
    assert_eq!(ids(&deps_of_oc2), vec!["OC-1"]);
    let full = run(
        &snap,
        json!({ "relationships": { "dependedOnBy": "OC-2" }, "select": ["relationships"] }),
    );
    let rel = full.rows[0].relationships.as_ref().unwrap();
    assert_eq!(rel.dependents.len(), 2, "OC-2 and the board depend on OC-1");
    assert_eq!(rel.supporting_docs[0].file_name, "notes.md");
    assert_eq!(rel.parent_epic.as_deref(), Some("SW-OC"));
}

#[test]
fn folder_epic_is_the_nearest_folder_holding_a_main_doc() {
    let fp = footprint();
    let (_, snap) = index(&fp);
    let rows = run(&snap, json!({ "select": ["relationships"], "limit": 1000 })).rows;
    let folder = |id: &str| {
        rows.iter()
            .find(|r| r.id == id)
            .unwrap_or_else(|| panic!("{id} indexed"))
            .relationships
            .as_ref()
            .unwrap()
            .folder_epic
            .clone()
    };
    // The epic folder holds a README: its stories, files and the README itself.
    assert_eq!(folder("OC-1").as_deref(), Some("13-onboarding"));
    assert_eq!(folder("notes.md").as_deref(), Some("13-onboarding"));
    assert_eq!(folder("README.md").as_deref(), Some("13-onboarding"));
    // No main doc on the way up to the content root: no folder epic, whatever
    // the frontmatter says.
    assert_eq!(folder("GX-1"), None);
}

#[test]
fn sort_paging_select_and_text() {
    let fp = footprint();
    let (_, snap) = index(&fp);
    let base = json!({ "where": { "orgs": ["acme"], "teams": ["default"], "kinds": ["story"] }, "sort": [{ "key": "story_id", "dir": "desc" }], "limit": 3, "select": ["status"] });
    let page1 = run(&snap, base.clone());
    assert_eq!(ids(&page1), vec!["OC-4", "OC-3", "OC-2"]);
    assert_eq!(page1.total, 4);
    let row = &page1.rows[0];
    assert_eq!(row.fields.keys().collect::<Vec<_>>(), vec!["status"]);
    assert!(row.relationships.is_none());
    let mut next = base.clone();
    next["cursor"] = json!(page1.next_cursor.clone().unwrap());
    let page2 = run(&snap, next);
    assert_eq!(ids(&page2), vec!["OC-1"]);
    assert!(page2.next_cursor.is_none());

    let text = run(&snap, json!({ "text": "NEEDLE" }));
    assert_eq!(ids(&text), vec!["notes.md"]);
    assert!(!text.text_scan_truncated);

    let bad = serde_json::from_value::<ContextQuery>(
        json!({ "filters": [{ "key": "a", "op": "nope" }] }),
    );
    assert!(bad.is_err(), "unknown operators are refused");
}

#[test]
fn an_explicit_order_ranks_sort_values_and_group_buckets() {
    // A team taxonomy order (`done` first) beats the canonical vocabulary.
    let fp = footprint();
    let (_, snap) = index(&fp);
    let r = run(
        &snap,
        json!({
            "where": { "orgs": ["acme"], "teams": ["default"], "kinds": ["story"] },
            "sort": [{ "key": "status", "order": ["done", "in-progress", "todo"] }, "story_id"],
            "groupBy": "status"
        }),
    );
    assert_eq!(ids(&r), vec!["OC-3", "OC-2", "OC-1", "OC-4"]);
    let buckets: Vec<String> = r
        .groups
        .unwrap()
        .into_iter()
        .filter_map(|g| g.value)
        .collect();
    assert_eq!(buckets, vec!["done", "in-progress", "todo"]);
}

#[test]
fn path_scope_glob_and_layers() {
    let fp = footprint();
    let (_, snap) = index(&fp);
    let glob = run(&snap, json!({ "where": { "pathGlob": "**/OC-*.md" } }));
    assert_eq!(ids(&glob).len(), 4);
    let prefix = run(
        &snap,
        json!({ "where": { "pathPrefix": "_bmad-output/x" } }),
    );
    assert_eq!(ids(&prefix), vec!["GX-1"]);
    let private = run(&snap, json!({ "where": { "layers": ["private"] } }));
    assert_eq!(private.total, 0, "no org named: no private items");
    let acme = run(
        &snap,
        json!({ "where": { "orgs": ["acme"], "layers": ["private"] } }),
    );
    assert_eq!(ids(&acme), vec!["ACME-PRIV"]);
}

#[test]
fn refresh_paths_updates_moves_and_deletes() {
    let fp = footprint();
    let (mut state, _) = index(&fp);
    let epic = fp.acme_team.join("_bmad-output/13-onboarding");
    std::thread::sleep(std::time::Duration::from_millis(5));
    write(
        &epic.join("OC-1-login.md"),
        &story("OC-1", "done", "parent_epic: SW-OC\n"),
    );
    std::fs::remove_file(epic.join("OC-4-bob.md")).unwrap();
    let changed = state.refresh_paths(&[epic.join("OC-1-login.md"), epic.join("OC-4-bob.md")]);
    assert_eq!(changed.len(), 2);
    let snap = state.snapshot(2, true);
    let r = run(
        &snap,
        json!({ "where": { "kinds": ["story"], "orgs": ["acme"], "teams": ["default"] }, "match": { "status": "done" } }),
    );
    assert_eq!(ids(&r), vec!["OC-1", "OC-3"]);
    // A removed folder drops everything under it.
    std::fs::remove_dir_all(&epic).unwrap();
    state.refresh_paths(std::slice::from_ref(&epic));
    let snap = state.snapshot(3, true);
    let r = run(
        &snap,
        json!({ "where": { "orgs": ["acme"], "teams": ["default"] } }),
    );
    assert_eq!(r.total, 0);
}

/// Perf sanity: index 5k files and query them. Prints timings; the bounds are
/// loose enough for a debug build on a loaded machine.
#[test]
fn perf_5k_files_index_and_query() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("content");
    let statuses = ["todo", "in-progress", "review", "done"];
    for i in 0..5_000 {
        let epic = i % 50;
        write(
            &root.join(format!("epic-{epic}/S-{i}.md")),
            &story(
                &format!("S-{i}"),
                statuses[i % 4],
                &format!("parent_epic: E-{epic}\nassigned_to: user{}\npriority: p{}\ndepends_on: [S-{}]\n", i % 7, i % 4, i.saturating_sub(1)),
            ),
        );
    }
    let roots = vec![IndexRoot {
        dir: root,
        org: Some("acme".into()),
        team: Some("default".into()),
        layer: Layer::Team,
    }];
    let t = std::time::Instant::now();
    let mut state = IndexState::default();
    state.reconcile(roots.clone());
    let snap = state.snapshot(1, true);
    let index_time = t.elapsed();

    let q: ContextQuery = serde_json::from_value(json!({
        "where": { "kinds": ["story"] },
        "relationships": { "parentEpic": "E-7" },
        "filters": [{ "key": "status", "op": "in", "value": ["todo", "in-progress"] }],
        "sort": ["priority"],
        "groupBy": "status"
    }))
    .unwrap();
    let t = std::time::Instant::now();
    const REPS: u32 = 20;
    for _ in 0..REPS {
        snap.query(&q, &Viewer::default(), &DiskBodyReader).unwrap();
    }
    let query_time = t.elapsed() / REPS;

    let t = std::time::Instant::now();
    let unchanged = state.reconcile(roots);
    let reconcile_time = t.elapsed();
    assert!(unchanged.is_empty());

    println!(
        "SW-OC-26 perf: index 5000 files {index_time:?}; query {query_time:?}; idle reconcile {reconcile_time:?}"
    );
    assert!(
        index_time.as_secs() < 30,
        "index build too slow: {index_time:?}"
    );
    assert!(
        query_time.as_millis() < 500,
        "query too slow: {query_time:?}"
    );
}

/// A reader that never touches disk, proving `text` stays bounded.
struct CountingReader(std::sync::atomic::AtomicUsize);
impl BodyReader for CountingReader {
    fn read(&self, _path: &str, _max: usize) -> Option<String> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Some(String::new())
    }
}

#[test]
fn text_scan_is_bounded() {
    use super::engine::MAX_TEXT_SCAN_FILES;
    use super::record::ContextRecord;
    use std::sync::Arc;
    let records: Vec<Arc<ContextRecord>> = (0..MAX_TEXT_SCAN_FILES + 10)
        .map(|i| {
            Arc::new(ContextRecord {
                headings: Vec::new(),
                path: format!("/c/f{i:06}.md"),
                rel_path: format!("f{i:06}.md"),
                file_name: format!("f{i:06}.md"),
                org: None,
                team: None,
                layer: Layer::AllOrgs,
                kind: Kind::Doc,
                mtime_ms: None,
                title: None,
                fields: Default::default(),
                is_pipeline_doc: false,
                metadata_truncated: false,
                metadata_valid: true,
                summary: None,
            })
        })
        .collect();
    let snap = Snapshot::build(records, Vec::new(), 1, true);
    let reader = CountingReader(Default::default());
    let q: ContextQuery = serde_json::from_value(json!({ "text": "x" })).unwrap();
    let r = snap.query(&q, &Viewer::default(), &reader).unwrap();
    assert!(r.text_scan_truncated);
    assert_eq!(
        reader.0.load(std::sync::atomic::Ordering::SeqCst),
        MAX_TEXT_SCAN_FILES
    );
}

#[test]
fn same_org_as_fails_closed_for_all_orgs_or_unknown_anchors() {
    let fp = footprint();
    let (_, snap) = index(&fp);
    let only_all_orgs = |anchor: &str| {
        let r = run(
            &snap,
            json!({ "where": { "sameOrgAs": anchor }, "limit": 100 }),
        );
        assert!(
            r.rows.iter().all(|row| row.org.is_none()),
            "anchor {anchor} must leave only all-orgs content, got {:?}",
            ids(&r)
        );
    };
    let all_orgs_skill = snap
        .records
        .iter()
        .find(|r| r.layer == Layer::AllOrgs)
        .unwrap()
        .path
        .clone();
    only_all_orgs(&all_orgs_skill);
    only_all_orgs("/nowhere/at/all.md");
    // A not-yet-indexed file under acme's team root still resolves to acme.
    let fresh = fp.acme_team.join("_bmad-output/new-doc.md");
    let r = run(
        &snap,
        json!({ "where": { "sameOrgAs": fresh.to_string_lossy() }, "limit": 100 }),
    );
    assert!(ids(&r).contains(&"ACME-PRIV".to_string()));
    assert!(!ids(&r).contains(&"GLOBEX-PRIV".to_string()));
}

#[test]
fn links_never_cross_orgs() {
    let fp = footprint();
    // An acme story pointing at globex's private file by relative path.
    let epic = fp.acme_team.join("_bmad-output/13-onboarding");
    let rel = pathdiff(&fp.roots[3].dir.join("secret.md"), &epic);
    write(
        &epic.join("OC-9-cross.md"),
        &story(
            "OC-9",
            "todo",
            &format!("supporting_docs:\n  - {rel}\ndepends_on: [GLOBEX-PRIV]\n"),
        ),
    );
    let (_, snap) = index(&fp);
    let r = run(
        &snap,
        json!({ "match": { "story_id": "OC-9" }, "select": ["relationships"] }),
    );
    let rel = r.rows[0].relationships.as_ref().unwrap();
    assert!(rel.supporting_docs.iter().all(|x| !x.resolved));
    assert!(rel.depends_on.iter().all(|x| !x.resolved));
    // …so nothing can probe for the other org's file through a relationship.
    let probe = run(
        &snap,
        json!({ "relationships": { "hasSupportingDoc": "GLOBEX-PRIV" } }),
    );
    assert_eq!(probe.total, 0);
}

/// `to` relative to `from_dir`, `/`-joined (test helper).
fn pathdiff(to: &Path, from_dir: &Path) -> String {
    let to: Vec<_> = to.components().collect();
    let from: Vec<_> = from_dir.components().collect();
    let common = to.iter().zip(&from).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec!["..".into(); from.len() - common];
    parts.extend(
        to[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    parts.join("/")
}

#[test]
fn refreshing_a_parent_folder_keeps_a_nested_roots_files() {
    let tmp = tempfile::tempdir().unwrap();
    let outer = tmp.path().join("outer");
    let inner = outer.join("nested");
    write(&outer.join("a.md"), "# a\n");
    write(&inner.join("b.md"), "# b\n");
    let roots = vec![
        IndexRoot {
            dir: outer.clone(),
            org: Some("o".into()),
            team: Some("t".into()),
            layer: Layer::Team,
        },
        IndexRoot {
            dir: inner.clone(),
            org: Some("o".into()),
            team: None,
            layer: Layer::Private,
        },
    ];
    let mut state = IndexState::default();
    state.reconcile(roots);
    assert_eq!(state.len(), 2);
    state.refresh_paths(std::slice::from_ref(&outer));
    assert_eq!(
        state.len(),
        2,
        "the nested root's file must survive a parent refresh"
    );
}

#[test]
fn frontmatter_wins_for_descriptive_builtins_and_bad_shapes_reject() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("c");
    write(
        &root.join("x.md"),
        "---\ntype: story\nstory_id: X-1\nkind: feature\n---\n",
    );
    let mut state = IndexState::default();
    state.reconcile(vec![IndexRoot {
        dir: root,
        org: Some("o".into()),
        team: Some("t".into()),
        layer: Layer::Team,
    }]);
    let snap = state.snapshot(1, true);
    assert_eq!(
        ids(&run(&snap, json!({ "match": { "kind": "feature" } }))),
        vec!["X-1"]
    );
    let clause = serde_json::from_value(json!({"key":"kind","op":"eq","value":"feature"})).unwrap();
    assert!(super::engine::clause_matches(
        &snap.records[0],
        &clause,
        &alice()
    ));
    assert_eq!(
        super::engine::field_value(&snap.records[0], "kind"),
        Some(json!("feature"))
    );
    // `where.kinds` still uses the classification.
    assert_eq!(
        ids(&run(&snap, json!({ "where": { "kinds": ["story"] } }))),
        vec!["X-1"]
    );
    // `limit: 0` counts without a cursor that would loop.
    let counted = run(&snap, json!({ "limit": 0, "groupBy": "type" }));
    assert_eq!(counted.total, 1);
    assert!(counted.next_cursor.is_none());
    // A typo in the sort object rejects instead of silently sorting ascending.
    assert!(serde_json::from_value::<ContextQuery>(
        json!({ "sort": [{ "key": "a", "direction": "desc" }] })
    )
    .is_err());
}

/// Run `q` as a page at `page` (a dashboard / csquery block / pane).
fn run_page(snap: &Snapshot, page: &str, q: serde_json::Value) -> ContextQueryResult {
    let q: ContextQuery = serde_json::from_value(q).expect("query parses");
    let viewer = Viewer {
        page: Some(page.to_string()),
        ..alice()
    };
    snap.query(&q, &viewer, &DiskBodyReader)
        .expect("query runs")
}

#[test]
fn a_page_sees_team_content_everywhere_but_private_items_only_of_its_own_org() {
    let fp = footprint();
    let (_, snap) = index(&fp);
    let acme_page = fp.acme_team.join("_bmad-output/13-onboarding/board.html");
    let acme_page = acme_page.to_string_lossy();
    // Footprint-wide team content (the control plane) …
    let all = run_page(&snap, &acme_page, json!({ "limit": 100 }));
    let ids_all = ids(&all);
    assert!(
        ids_all.contains(&"GX-1".to_string()),
        "other orgs' team content stays visible"
    );
    assert!(
        ids_all.contains(&"ACME-PRIV".to_string()),
        "own org's private items"
    );
    assert!(ids_all.contains(&"SKILL.md".to_string()), "all-orgs items");
    // … but never another org's private items, even asked for by name.
    assert!(!ids_all.contains(&"GLOBEX-PRIV".to_string()));
    let explicit = run_page(
        &snap,
        &acme_page,
        json!({ "where": { "orgs": ["globex"], "layers": ["private"] } }),
    );
    assert_eq!(
        explicit.total, 0,
        "explicit org list can't reach globex's private items"
    );
    // An unknown page path sees no private items at all.
    let unknown = run_page(&snap, "", json!({ "where": { "layers": ["private"] } }));
    assert_eq!(unknown.total, 0);
    // An agent (no page): private items only for ONE org it names explicitly.
    let all = run(
        &snap,
        json!({ "where": { "orgs": "all", "layers": ["private"] } }),
    );
    assert_eq!(all.total, 0, "orgs: all never returns private items");
    let two = run(
        &snap,
        json!({ "where": { "orgs": ["acme", "globex"], "layers": ["private"] } }),
    );
    assert_eq!(two.total, 0, "two orgs is not one anchor org");
    let one = run(
        &snap,
        json!({ "where": { "orgs": ["globex"], "layers": ["private"] } }),
    );
    assert_eq!(ids(&one), vec!["GLOBEX-PRIV"]);
}

#[test]
fn a_page_never_learns_of_another_orgs_private_items_through_relationships() {
    let fp = footprint();
    // A globex team story linking globex's own private file (a legit same-org link).
    let gx = fp.roots[1].dir.join("_bmad-output/x");
    let rel = pathdiff(&fp.roots[3].dir.join("secret.md"), &gx);
    write(
        &gx.join("GX-2.md"),
        &story("GX-2", "todo", &format!("supporting_docs:\n  - {rel}\n")),
    );
    let (_, snap) = index(&fp);
    // An agent naming globex sees the same-org link …
    let agent = run(
        &snap,
        json!({ "where": { "orgs": ["globex"] }, "match": { "story_id": "GX-2" }, "select": ["relationships"] }),
    );
    assert!(
        agent.rows[0]
            .relationships
            .as_ref()
            .unwrap()
            .supporting_docs[0]
            .resolved
    );
    let acme_page = fp.acme_team.join("README.md");
    let acme_page = acme_page.to_string_lossy();
    let page = run_page(
        &snap,
        &acme_page,
        json!({ "match": { "story_id": "GX-2" }, "select": ["relationships"] }),
    );
    assert!(page.rows[0]
        .relationships
        .as_ref()
        .unwrap()
        .supporting_docs
        .is_empty());
    let probe = run_page(
        &snap,
        &acme_page,
        json!({ "relationships": { "hasSupportingDoc": "secret.md" } }),
    );
    assert_eq!(
        probe.total, 0,
        "no probing another org's private files by link"
    );
}

#[test]
fn at_me_matches_the_profile_roster_id_in_that_org_only() {
    let fp = footprint();
    let epic = fp.acme_team.join("_bmad-output/13-onboarding");
    write(
        &epic.join("OC-7.md"),
        &story("OC-7", "todo", "assigned_to: person:p-alice\n"),
    );
    write(
        &fp.roots[1].dir.join("_bmad-output/x/GX-7.md"),
        &story("GX-7", "todo", "assigned_to: person:p-alice\n"),
    );
    let (_, snap) = index(&fp);
    let q: ContextQuery =
        serde_json::from_value(json!({ "match": { "assigned_to": "@me" }, "limit": 100 })).unwrap();
    let viewer = Viewer {
        identities: vec!["alice".into()],
        org_identities: [("acme".to_string(), vec!["person:p-alice".to_string()])].into(),
        page: None,
        ..Default::default()
    };
    let r = snap.query(&q, &viewer, &DiskBodyReader).unwrap();
    let got = ids(&r);
    assert!(
        got.contains(&"OC-7".to_string()),
        "roster id matches in its org"
    );
    assert!(got.contains(&"OC-1".to_string()), "handle still matches");
    assert!(
        !got.contains(&"GX-7".to_string()),
        "acme's roster id means nothing in globex"
    );
    assert!(super::uses_me(&q));
}

#[test]
fn roster_me_finds_the_profile_by_github_id_then_login() {
    use crate::domain::{Identity, Org, OrgConfigFlags, Person, PersonId};
    let gh = |id: u64, login: &str| {
        Person::from_identities(
            PersonId::from(id),
            vec![Identity {
                provider: "github".to_string(),
                handle: Some(login.to_string()),
                provider_user_id: Some(id.to_string()),
            }],
            login,
            None,
        )
    };
    let org = Org::new(
        "acme",
        "Acme",
        chrono::Utc::now(),
        None,
        OrgConfigFlags::default(),
        vec![gh(7, "alice"), gh(9, "bob")],
        vec![],
    )
    .unwrap();
    let bob = PersonId::from(9u64);
    assert_eq!(
        super::roster_me(&org, 9, "whatever"),
        Some(format!("person:{}", bob.as_str()))
    );
    let alice = PersonId::from(7u64);
    assert_eq!(
        super::roster_me(&org, 12345, "@Alice"),
        Some(format!("person:{}", alice.as_str()))
    );
    assert_eq!(super::roster_me(&org, 12345, "nobody"), None);
}

#[test]
fn cursors_are_bound_to_the_query_and_the_index_generation() {
    let fp = footprint();
    let (mut state, _) = index(&fp);
    let snap = state.snapshot(1, true);
    let q = json!({ "where": { "kinds": ["story"] }, "limit": 2, "sort": ["story_id"] });
    let first = run(&snap, q.clone());
    let cursor = first.next_cursor.clone().unwrap();
    let mut next = q.clone();
    next["cursor"] = json!(cursor);
    assert_eq!(run(&snap, next.clone()).rows.len(), 2);
    // Another query with the same cursor: refused.
    let mut other = json!({ "where": { "kinds": ["doc"] }, "limit": 2 });
    other["cursor"] = json!(cursor);
    let parsed: ContextQuery = serde_json::from_value(other).unwrap();
    let err = snap.query(&parsed, &alice(), &DiskBodyReader).unwrap_err();
    assert!(
        matches!(&err, QueryError::InvalidCursor(m) if m.contains("different query")),
        "{err}"
    );
    // Another snapshot: stale, with a restart hint.
    let later = state.snapshot(2, true);
    let parsed: ContextQuery = serde_json::from_value(next).unwrap();
    let err = later.query(&parsed, &alice(), &DiskBodyReader).unwrap_err();
    assert_eq!(err, QueryError::StaleCursor);
    assert!(err.to_string().contains("restart paging"), "{err}");
    let _ = &mut state;
}

/// SV-ENG-63: a caller that sent only the first `kept` rows of a page gets the cursor that
/// resumes at the next row, on the first page and on a cursor page; no rows follow, no cursor.
#[test]
fn a_cursor_after_the_rows_kept_resumes_at_the_next_row() {
    let fp = footprint();
    let (_, snap) = index(&fp);
    let base = json!({ "where": { "kinds": ["story"] }, "limit": 3, "sort": ["story_id"] });
    let parse = |v: &serde_json::Value| serde_json::from_value::<ContextQuery>(v.clone()).unwrap();
    let all = run(&snap, json!({ "where": { "kinds": ["story"] }, "sort": ["story_id"] }));
    let first = run(&snap, base.clone());
    let cut = super::cursor_after_kept(&parse(&base), &alice(), &first, 1).unwrap();
    let mut resume = base.clone();
    resume["cursor"] = json!(cut);
    assert_eq!(ids(&run(&snap, resume.clone()))[0], ids(&all)[1]);
    // A cursor page resumes from its own start, not from zero.
    let second = run(&snap, resume.clone());
    let cut = super::cursor_after_kept(&parse(&resume), &alice(), &second, 1);
    let mut again = base.clone();
    again["cursor"] = json!(cut.unwrap());
    assert_eq!(ids(&run(&snap, again))[0], ids(&all)[2]);
    // Nothing follows the last row, and nothing was kept means no cursor.
    assert_eq!(super::cursor_after_kept(&parse(&base), &alice(), &first, 0), None);
    let whole = run(&snap, json!({ "where": { "kinds": ["story"] }, "sort": ["story_id"] }));
    let q = parse(&json!({ "where": { "kinds": ["story"] }, "sort": ["story_id"] }));
    assert_eq!(super::cursor_after_kept(&q, &alice(), &whole, whole.rows.len()), None);
}

#[test]
fn eq_compares_text_exactly_unless_the_operand_is_a_number() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("c");
    write(
        &root.join("a.md"),
        "---\nstory_id: \"01\"\npoints: 1.0\n---\n",
    );
    let mut state = IndexState::default();
    state.reconcile(vec![IndexRoot {
        dir: root,
        org: Some("o".into()),
        team: Some("t".into()),
        layer: Layer::Team,
    }]);
    let snap = state.snapshot(1, true);
    assert_eq!(run(&snap, json!({ "match": { "story_id": "1" } })).total, 0);
    assert_eq!(
        run(&snap, json!({ "match": { "story_id": "01" } })).total,
        1
    );
    assert_eq!(
        run(&snap, json!({ "match": { "points": 1 } })).total,
        1,
        "numeric operand"
    );
    assert_eq!(
        run(&snap, json!({ "match": { "points": "1" } })).total,
        0,
        "text operand"
    );
    for (operand, expected) in [(json!(1), true), (json!("1"), false), (json!("1.0"), true)] {
        let clause =
            serde_json::from_value(json!({"key":"points","op":"eq","value":operand})).unwrap();
        assert_eq!(
            super::engine::clause_matches(&snap.records[0], &clause, &alice()),
            expected
        );
    }
    // Mixed numeric/text values sort without panicking.
    let sorted = run(&snap, json!({ "sort": ["story_id", "points"] }));
    assert_eq!(sorted.total, 1);
}

#[test]
fn index_edge_cases_skipped_dirs_symlinks_and_truncated_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("c");
    write(&root.join("keep.md"), "# keep\n");
    let mut state = IndexState::default();
    state.reconcile(vec![IndexRoot {
        dir: root.clone(),
        org: Some("o".into()),
        team: Some("t".into()),
        layer: Layer::Team,
    }]);
    assert_eq!(state.len(), 1);
    // A single-file event inside .git / node_modules is ignored.
    write(&root.join(".git/HEAD.md"), "x");
    write(&root.join("node_modules/pkg/README.md"), "x");
    state.refresh_paths(&[
        root.join(".git/HEAD.md"),
        root.join("node_modules/pkg/README.md"),
    ]);
    assert_eq!(state.len(), 1);
    // A file replaced by a symlink drops its record.
    #[cfg(unix)]
    {
        let target = tmp.path().join("elsewhere.md");
        write(&target, "# elsewhere\n");
        std::fs::remove_file(root.join("keep.md")).unwrap();
        std::os::unix::fs::symlink(&target, root.join("keep.md")).unwrap();
        state.refresh_paths(&[root.join("keep.md")]);
        assert_eq!(state.len(), 0, "a symlink is never followed or kept");
    }
    // Frontmatter longer than the read cap is marked truncated.
    let long = format!("---\ntype: story\nnotes: \"{}\"\n", "x".repeat(20_000));
    write(&root.join("long.md"), &long);
    write(&root.join("short.md"), "---\ntype: story\n---\nbody\n");
    state.refresh_paths(&[root.join("long.md"), root.join("short.md")]);
    let snap = state.snapshot(1, true);
    let r = run(&snap, json!({ "limit": 10 }));
    let flag = |name: &str| {
        r.rows
            .iter()
            .find(|row| row.file_name == name)
            .unwrap()
            .metadata_truncated
    };
    assert!(flag("long.md"));
    assert!(!flag("short.md"));
}

/// A reader that returns large bodies, counting bytes.
struct BigReader(std::sync::atomic::AtomicUsize);
impl BodyReader for BigReader {
    fn read(&self, _path: &str, max: usize) -> Option<String> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Some("y".repeat(max))
    }
}

#[test]
fn text_scan_stops_at_the_total_byte_budget_and_says_total_is_approximate() {
    use super::engine::{MAX_TEXT_SCAN_BYTES, MAX_TEXT_SCAN_TOTAL_BYTES};
    use super::record::ContextRecord;
    use std::sync::Arc;
    let files = MAX_TEXT_SCAN_TOTAL_BYTES / MAX_TEXT_SCAN_BYTES + 20;
    let records: Vec<Arc<ContextRecord>> = (0..files)
        .map(|i| {
            Arc::new(ContextRecord {
                headings: Vec::new(),
                path: format!("/c/f{i:06}.md"),
                rel_path: format!("f{i:06}.md"),
                file_name: format!("f{i:06}.md"),
                org: None,
                team: None,
                layer: Layer::AllOrgs,
                kind: Kind::Doc,
                mtime_ms: None,
                title: None,
                fields: Default::default(),
                is_pipeline_doc: false,
                metadata_truncated: false,
                metadata_valid: true,
                summary: None,
            })
        })
        .collect();
    let snap = Snapshot::build(records, Vec::new(), 1, true);
    let reader = BigReader(Default::default());
    let q: ContextQuery = serde_json::from_value(json!({ "text": "nomatch" })).unwrap();
    let r = snap.query(&q, &Viewer::default(), &reader).unwrap();
    assert!(r.text_scan_truncated && r.total_approximate);
    assert_eq!(
        reader.0.load(std::sync::atomic::Ordering::SeqCst),
        MAX_TEXT_SCAN_TOTAL_BYTES / MAX_TEXT_SCAN_BYTES
    );
}

#[test]
fn results_report_the_orgs_they_are_scoped_to() {
    let fp = footprint();
    let (_, snap) = index(&fp);
    assert_eq!(run(&snap, json!({})).scope_orgs, None);
    assert_eq!(
        run(&snap, json!({ "where": { "orgs": ["globex", "acme"] } })).scope_orgs,
        Some(vec!["acme".to_string(), "globex".to_string()])
    );
}

#[test]
fn same_root_as_limits_to_the_anchor_files_content_root() {
    let fp = footprint();
    let (_, snap) = index(&fp);
    let anchor = fp.acme_team.join("_bmad-output/13-onboarding/board.html");
    let r = run(
        &snap,
        json!({ "where": { "sameRootAs": anchor.to_string_lossy(), "kinds": ["story"] } }),
    );
    assert_eq!(ids(&r), vec!["OC-1", "OC-2", "OC-3", "OC-4"]);
    let none = run(
        &snap,
        json!({ "where": { "sameRootAs": "/nowhere/x.html" } }),
    );
    assert_eq!(none.total, 0);
}

#[test]
fn range_matches_only_values_of_the_bounds_kind_and_contains_takes_numbers() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("c");
    write(
        &root.join("n.md"),
        "---\nstory_id: N\nwhen: 5\nsizes: [3, 8]\n---\n",
    );
    write(
        &root.join("d.md"),
        "---\nstory_id: D\nwhen: 2026-10-02\nsizes: [\"Large\"]\n---\n",
    );
    write(&root.join("t.md"), "---\nstory_id: T\nwhen: soon\n---\n");
    let mut state = IndexState::default();
    state.reconcile(vec![IndexRoot {
        dir: root,
        org: Some("o".into()),
        team: Some("t".into()),
        layer: Layer::Team,
    }]);
    let snap = state.snapshot(1, true);
    let q =
        |f: serde_json::Value| ids(&run(&snap, json!({ "filters": [f], "sort": ["story_id"] })));
    assert_eq!(
        q(json!({ "key": "when", "op": "range", "min": 0 })),
        vec!["N"]
    );
    assert_eq!(
        q(json!({ "key": "when", "op": "range", "min": "2026-01-01" })),
        vec!["D"]
    );
    assert_eq!(
        q(json!({ "key": "when", "op": "range", "max": "zzz", "min": "a" })),
        vec!["T"]
    );
    assert_eq!(
        q(json!({ "key": "sizes", "op": "contains", "value": 8 })),
        vec!["N"]
    );
    assert_eq!(
        q(json!({ "key": "sizes", "op": "contains", "value": "large" })),
        vec!["D"]
    );
}

/// SW-DNS-2: `select: ["summary"]` returns each pipeline doc's first configured
/// summary section, parsed once at index time from the head the index already
/// reads; a query without it carries no `summary` key at all.
#[test]
fn summary_is_the_first_configured_section_and_only_when_selected() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("orgs/acme/ctx/default/content");
    let epic = root.join("e/billing");
    write(
        &epic.join("README.md"),
        "---\ntype: epic\nepic_id: BIL\n---\n# Billing\n\n## Purpose\n\nGet   paid\non time.\n",
    );
    write(
        &epic.join("BIL-1.md"),
        &format!(
            "{}\n## Purpose\n\nInvoices go out.\n\n## Story\n\nAs a user.\n\n## Overview\n\nThe overview.\n",
            story("BIL-1", "todo", "")
        ),
    );
    write(
        &epic.join("BIL-2.md"),
        &format!(
            "{}\n## Story\n\nOnly a story.\n",
            story("BIL-2", "todo", "")
        ),
    );
    write(&epic.join("BIL-3.md"), &story("BIL-3", "todo", ""));
    write(
        &epic.join("notes.md"),
        "# Notes\n\n## Purpose\n\nNot a pipeline doc.\n",
    );
    let roots = vec![IndexRoot {
        dir: root.clone(),
        org: Some("acme".into()),
        team: Some("default".into()),
        layer: Layer::Team,
    }];
    let mut state = IndexState::default();
    state.reconcile(roots.clone());
    let snap = state.snapshot(1, true);
    let rows = run(&snap, json!({ "select": ["summary"], "limit": 100 })).rows;
    let summary = |rows: &[super::model::ContextRow], id: &str| {
        rows.iter()
            .find(|r| r.id == id)
            .unwrap_or_else(|| panic!("{id} indexed"))
            .summary
            .clone()
    };
    assert_eq!(
        summary(&rows, "BIL-1"),
        Some(Some("Invoices go out.".into()))
    );
    assert_eq!(summary(&rows, "BIL-2"), Some(Some("Only a story.".into())));
    assert_eq!(summary(&rows, "BIL-3"), Some(None), "no heading: null");
    assert_eq!(
        summary(&rows, "README.md"),
        Some(Some("Get paid on time.".into())),
        "epic main docs carry it too, whitespace collapsed"
    );
    assert_eq!(
        summary(&rows, "notes.md"),
        Some(None),
        "plain docs carry none"
    );

    // Not selected: no `summary` key on the wire.
    let plain = run(&snap, json!({ "select": ["status"], "limit": 100 })).rows;
    assert_eq!(summary(&plain, "BIL-1"), None);
    let wire = serde_json::to_value(plain.iter().find(|r| r.id == "BIL-1").unwrap()).unwrap();
    assert!(wire.get("summary").is_none(), "{wire}");
    let wire = serde_json::to_value(rows.iter().find(|r| r.id == "BIL-3").unwrap()).unwrap();
    assert_eq!(wire.get("summary"), Some(&serde_json::Value::Null));

    // The configured headings decide; a change re-derives every summary.
    assert!(state.set_summary_headings(vec!["Overview".into()]));
    assert!(
        !state.set_summary_headings(vec![" Overview ".into()]),
        "same list"
    );
    state.reconcile(roots);
    let snap = state.snapshot(2, true);
    let rows = run(&snap, json!({ "select": ["summary"], "limit": 100 })).rows;
    assert_eq!(summary(&rows, "BIL-1"), Some(Some("The overview.".into())));
    assert_eq!(summary(&rows, "BIL-2"), Some(None));
}

/// SW-DNS-2: a long section is capped (on a char boundary) so the index and the
/// wire stay small.
#[test]
fn summary_is_capped() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("c");
    let long = "é".repeat(900);
    write(
        &root.join("e/S-1.md"),
        &format!("{}\n## Purpose\n\n{long}\n", story("S-1", "todo", "")),
    );
    let mut state = IndexState::default();
    state.reconcile(vec![IndexRoot {
        dir: root,
        org: Some("acme".into()),
        team: Some("default".into()),
        layer: Layer::Team,
    }]);
    let snap = state.snapshot(1, true);
    let rows = run(&snap, json!({ "select": ["summary"] })).rows;
    let s = rows[0].summary.clone().flatten().expect("a summary");
    assert!(
        s.chars().count() <= super::index::SUMMARY_CAP,
        "{}",
        s.chars().count()
    );
    assert!(s.ends_with('…'));
}

/// SW-DNS-2: a summary is plain text — inline Markdown markup dropped and
/// character references decoded — for Markdown and HTML stories alike.
#[test]
fn summary_is_plain_text_for_markdown_and_html_stories() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("c");
    write(
        &root.join("e/M-1.md"),
        &format!(
            "{}\n## Purpose\n\n- Ship **bold** and _soft_ [links](https://x.test) with `code` &amp; ~~old~~ news &#x2014; ![chart](c.png) done.\n",
            story("M-1", "todo", "")
        ),
    );
    write(
        &root.join("e/H-1.html"),
        "<html><head><meta name=\"cs:type\" content=\"story\"><meta name=\"cs:story-id\" content=\"H-1\"><meta name=\"cs:status\" content=\"todo\"><title>H-1</title></head><body><h1>H-1</h1><section data-cs=\"Purpose\"><p>Ship &amp; <b>bill</b> &lt;fast&gt; &#8212; on time.</p></section></body></html>",
    );
    let mut state = IndexState::default();
    state.reconcile(vec![IndexRoot {
        dir: root,
        org: Some("acme".into()),
        team: Some("default".into()),
        layer: Layer::Team,
    }]);
    let snap = state.snapshot(1, true);
    let rows = run(&snap, json!({ "select": ["summary"] })).rows;
    let summary = |name: &str| {
        rows.iter()
            .find(|r| r.file_name == name)
            .unwrap_or_else(|| panic!("{name} indexed"))
            .summary
            .clone()
            .flatten()
    };
    assert_eq!(
        summary("M-1.md").as_deref(),
        Some("Ship bold and soft links with code & old news — chart done.")
    );
    assert_eq!(
        summary("H-1.html").as_deref(),
        Some("Ship & bill <fast> — on time.")
    );
}

/// SW-DNS-2: the summary comes from the head the index already reads (16 KB of a
/// Markdown file); a `## Purpose` past it reads as no summary rather than costing a
/// second read.
#[test]
fn summary_past_the_index_head_is_null() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("c");
    let filler = "Background paragraph.\n\n".repeat(super::index::META_READ_CAP / 20);
    write(
        &root.join("e/L-1.md"),
        &format!(
            "{}\n## Context\n\n{filler}\n## Purpose\n\nToo far down.\n",
            story("L-1", "todo", "")
        ),
    );
    let mut state = IndexState::default();
    state.reconcile(vec![IndexRoot {
        dir: root,
        org: Some("acme".into()),
        team: Some("default".into()),
        layer: Layer::Team,
    }]);
    let snap = state.snapshot(1, true);
    let rows = run(&snap, json!({ "select": ["summary"] })).rows;
    assert_eq!(rows[0].id, "L-1");
    assert_eq!(rows[0].summary, Some(None));
}

/// Real teaching pages plus early-path body decoys and generated reference noise.
fn docs_footprint() -> Footprint {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("docs/content");
    let pages = [
        ("arc-e-agents/agent-design.html", include_str!("../../../codesense-docs/getting-started/content/arc-e-agents/agent-design.html")),
        ("arc-g-routines/trigger-cookbook.html", include_str!("../../../codesense-docs/getting-started/content/arc-g-routines/trigger-cookbook.html")),
        ("arc-g-routines/command-routines.html", include_str!("../../../codesense-docs/getting-started/content/arc-g-routines/command-routines.html")),
        ("arc-i-dashboard-primitives/customize-built-in-panes.html", include_str!("../../../codesense-docs/getting-started/content/arc-i-dashboard-primitives/customize-built-in-panes.html")),
        ("arc-i-dashboard-primitives/pane-shapes.html", include_str!("../../../codesense-docs/getting-started/content/arc-i-dashboard-primitives/pane-shapes.html")),
        ("arc-b-model/teams.html", include_str!("../../../codesense-docs/getting-started/content/arc-b-model/teams.html")),
        ("arc-f-inboxes-and-triage/roles.html", include_str!("../../../codesense-docs/getting-started/content/arc-f-inboxes-and-triage/roles.html")),
    ];
    for (path, html) in pages {
        let metadata = if path.starts_with("arc-g-") {
            "<meta name=\"cs:topic\" content=\"routines\"><meta name=\"cs:task\" content=\"automate, chain\"><meta name=\"cs:tools\" content=\"routine_create, routine_runs\"><meta name=\"cs:audience\" content=\"agents, humans\">"
        } else {
            ""
        };
        write(
            &dir.join(path),
            &html.replace("</head>", &format!("{metadata}</head>")),
        );
    }
    write(&dir.join("aaa.html"), "<html><head><title>Noise</title><meta name=\"cs:type\" content=\"content\"></head><body>agent design metadata trigger Inbox pane pane manifest set_team_parent command routine role_save</body></html>");
    write(
        &dir.join("concepts/README.md"),
        "---\ntype: content\n---\n# Concepts\n",
    );
    write(&dir.join("ordinary/README.md"), "# Untyped README\n");
    write(
        &dir.join("examples/skills/example/SKILL.md"),
        "---\ntype: skill\n---\n# A skill\n",
    );
    write(
        &dir.join("ordinary/board.html"),
        "<html><head><title>Board</title></head></html>",
    );
    write(
        &dir.join("generated/noise.json"),
        r#"{"tools":["agent design", "metadata trigger", "Inbox pane", "pane manifest", "set_team_parent", "command routine", "role_save"]}"#,
    );
    // This real sidecar is > 1 MB: metadata's 64 KB cap must not truncate it.
    let sidecar =
        include_str!("../../../codesense-docs/getting-started/content/generated/search-index.json");
    assert!(sidecar.len() > 1_000_000);
    write(&dir.join("generated/search-index.json"), sidecar);
    let other = tmp.path().join("other/content");
    write(
        &other.join("private.html"),
        "<html><head><title>Agent design</title></head><body>otherorgterm</body></html>",
    );
    write(
        &other.join("generated/search-index.json"),
        r##"{"schema":1,"entries":[{"title":"otherorgterm","href":"./private.html#hidden","text":""}]}"##,
    );
    let roots = vec![
        IndexRoot {
            dir: dir.clone(),
            org: Some("codesense/docs".into()),
            team: Some("getting-started".into()),
            layer: Layer::Team,
        },
        IndexRoot {
            dir: other,
            org: Some("other".into()),
            team: None,
            layer: Layer::Private,
        },
    ];
    Footprint {
        _tmp: tmp,
        roots,
        acme_team: dir,
    }
}

#[test]
fn docs_queries_rank_the_real_pages_and_return_heading_anchors() {
    let fp = docs_footprint();
    let (_, snap) = index(&fp);
    // AC1's six cases and DOC-7's three HTML-scoped cases: nine cases, seven terms.
    for (term, page, anchor, html_only) in [
        ("agent design", "agent-design.html", "#what", false),
        ("metadata trigger", "trigger-cookbook.html", "#hero", false),
        (
            "Inbox pane",
            "customize-built-in-panes.html",
            "#inbox",
            false,
        ),
        ("pane manifest", "pane-shapes.html", "#pane-manifest", false),
        ("set_team_parent", "teams.html", "#set-team-parent", false),
        ("command routine", "command-routines.html", "#hero", false),
        (
            "metadata trigger",
            "trigger-cookbook.html",
            "#metadata_change",
            true,
        ),
        ("command routine", "command-routines.html", "#hero", true),
        ("role_save", "roles.html", "#configuration", true),
    ] {
        let mut q = json!({"where":{"orgs":["codesense/docs"]},"text":term});
        if html_only {
            q["where"]["pathGlob"] = json!("**/*.html");
        }
        let out = run(&snap, q);
        assert_eq!(out.rows[0].file_name, page, "{term}: {:?}", ids(&out));
        assert!(
            out.rows[0].text_matches.iter().any(|m| m.anchor == anchor),
            "{term}: {:?}",
            out.rows[0].text_matches
        );
        assert_eq!(out.excluded, ["generated/"]);
        assert!(out
            .rows
            .iter()
            .all(|r| !r.rel_path.starts_with("generated/")));
        assert!(out.rows[0].path.ends_with(&out.rows[0].rel_path));
        assert_eq!(
            out.rows
                .iter()
                .map(|r| &r.path)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            out.total
        );
    }
}

#[test]
fn docs_topics_select_canonical_lists_and_content_kind_precedes_heuristics() {
    let fp = docs_footprint();
    let (_, snap) = index(&fp);
    let out = run(
        &snap,
        json!({"where":{"orgs":["codesense/docs"]},"filters":[{"key":"cs:topic","op":"eq","value":"routines"}],"select":["cs:type","cs:topic","cs:task","cs:tools","cs:audience"],"sort":["cs:fileName"],"groupBy":"cs:topic"}),
    );
    assert_eq!(
        ids(&out),
        ["command-routines.html", "trigger-cookbook.html"]
    );
    for row in &out.rows {
        assert_eq!(row.kind, Kind::Doc);
        assert_eq!(row.fields["topic"], json!(["routines"]));
        assert_eq!(
            row.fields["tools"],
            json!(["routine_create", "routine_runs"])
        );
        assert_eq!(row.fields["audience"], json!(["agents", "humans"]));
        assert_eq!(row.fields.len(), 5);
    }
    assert_eq!(out.groups.unwrap()[0].count, 2);
    let all = run(&snap, json!({"where":{"orgs":["codesense/docs"]}}));
    assert!(all.excluded.is_empty());
    let kind = |path| all.rows.iter().find(|r| r.rel_path == path).unwrap().kind;
    assert_eq!(kind("concepts/README.md"), Kind::Doc);
    assert_eq!(kind("ordinary/README.md"), Kind::Epic);
    assert_eq!(kind("ordinary/board.html"), Kind::Dashboard);
    assert_eq!(kind("examples/skills/example/SKILL.md"), Kind::Skill);
    assert!(all
        .rows
        .iter()
        .any(|r| r.rel_path == "generated/noise.json"));
}

#[test]
fn sidecar_search_is_generic_visible_refreshable_and_pages_stably() {
    let fp = docs_footprint();
    let (mut state, _) = index(&fp);
    let path = fp.acme_team.join("generated/search-index.json");
    let mut sidecar: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    sidecar["entries"].as_array_mut().unwrap().push(
        json!({"title":"sidecaronlyterm", "href":"./arc-b-model/teams.html#only", "text":""}),
    );
    write(&path, &serde_json::to_string(&sidecar).unwrap());
    assert_eq!(state.refresh_paths(&[path.clone()]), [path.clone()]);
    let snap = state.snapshot(2, true);
    let out = run(
        &snap,
        json!({"where":{"orgs":["codesense/docs"]},"text":"sidecaronlyterm"}),
    );
    assert_eq!(ids(&out), ["teams.html"]);
    assert_eq!(out.rows[0].text_matches[0].anchor, "#only");
    assert_eq!(
        run(
            &snap,
            json!({"where":{"orgs":["codesense/docs"]},"text":"otherorgterm"})
        )
        .total,
        0
    );
    assert_eq!(
        run(&snap, json!({"text":"otherorgterm"})).total,
        0,
        "private page and sidecar remain hidden"
    );
    let other = run(
        &snap,
        json!({"where":{"orgs":["other"]},"text":"otherorgterm"}),
    );
    assert_eq!(other.rows[0].text_matches[0].anchor, "#hidden");
    assert!(other.excluded.is_empty());
    let sorted = run(
        &snap,
        json!({"where":{"orgs":["codesense/docs"]},"text":"pane manifest","sort":["path"]}),
    );
    assert_eq!(sorted.rows[0].file_name, "aaa.html", "explicit sort wins");
    for scope in [
        json!({"orgs":["codesense/docs"],"pathPrefix":"generated"}),
        json!({"orgs":["codesense/docs"],"pathPrefix":"generated/"}),
        json!({"orgs":["codesense/docs"],"pathPrefix":fp.acme_team.join("generated")}),
        json!({"orgs":["codesense/docs"],"pathGlob":"generated/*.json"}),
    ] {
        let refs = run(&snap, json!({"where":scope,"text":"pane manifest"}));
        assert!(refs.excluded.is_empty());
        assert!(refs.rows.iter().any(|r| r.file_name == "noise.json"));
    }
    let mut query: ContextQuery = serde_json::from_value(
        json!({"where":{"orgs":["codesense/docs"]},"text":"pane","limit":1,"groupBy":"kind"}),
    )
    .unwrap();
    let first = snap.query(&query, &alice(), &DiskBodyReader).unwrap();
    assert_eq!(
        first
            .groups
            .as_ref()
            .unwrap()
            .iter()
            .map(|g| g.count)
            .sum::<usize>(),
        first.total
    );
    let cursor = first.next_cursor.clone().unwrap();
    let mut rows = first.rows;
    let mut next = Some(cursor.clone());
    while let Some(c) = next {
        query.cursor = Some(c);
        let page = snap.query(&query, &alice(), &DiskBodyReader).unwrap();
        assert_eq!(page.total, first.total);
        rows.extend(page.rows);
        next = page.next_cursor;
    }
    assert_eq!(rows.len(), first.total);
    assert_eq!(
        rows.iter()
            .map(|r| &r.path)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        first.total
    );
    sidecar["entries"]
        .as_array_mut()
        .unwrap()
        .last_mut()
        .unwrap()["href"] = json!("./arc-b-model/teams.html#changed");
    write(&path, &serde_json::to_string(&sidecar).unwrap());
    assert!(!state.refresh_paths(&[path]).is_empty());
    let fresh = state.snapshot(3, true);
    query.cursor = Some(cursor);
    assert_eq!(
        fresh.query(&query, &alice(), &DiskBodyReader).unwrap_err(),
        QueryError::StaleCursor
    );
    let updated = run(
        &fresh,
        json!({"where":{"orgs":["codesense/docs"]},"text":"sidecaronlyterm"}),
    );
    assert_eq!(updated.rows[0].text_matches[0].anchor, "#changed");
}

/// SV-PBF-287: a running index over one team root holding `n` stories `P-000`…,
/// and the root.
fn paging_index(n: usize) -> (tempfile::TempDir, PathBuf, super::ContextIndexService) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("acme/default/content");
    for i in 0..n {
        write(
            &root.join(format!("stories/P-{i:03}.md")),
            &story(&format!("P-{i:03}"), "todo", ""),
        );
    }
    let svc = super::ContextIndexService::started(vec![IndexRoot {
        dir: root.clone(),
        org: Some("acme".into()),
        team: Some("default".into()),
        layer: Layer::Team,
    }]);
    (tmp, root, svc)
}

/// The stories query a paged dashboard runs, `limit` rows a page, at `cursor`.
fn story_page(limit: usize, cursor: Option<&str>) -> ContextQuery {
    let mut q = json!({ "where": { "kinds": ["story"] }, "sort": ["story_id"], "limit": limit });
    if let Some(c) = cursor {
        q["cursor"] = json!(c);
    }
    serde_json::from_value(q).unwrap()
}

/// Move the index on: write one new story and edit another, then wait for the publish.
fn churn(svc: &super::ContextIndexService, root: &Path, step: usize) {
    let added = root.join(format!("stories/NEW-{step:03}.md"));
    let edited = root.join(format!("stories/P-{:03}.md", step % 10));
    write(&added, &story(&format!("NEW-{step:03}"), "todo", ""));
    write(
        &edited,
        &story(
            &format!("P-{:03}", step % 10),
            &format!("review-{step}"),
            "",
        ),
    );
    let generation = svc.snapshot().generation;
    svc.settle(vec![added, edited], generation);
}

/// SV-PBF-287 case 7: a paged walk while files are written between its pages
/// reads the generation its first page came from: every row exactly once, the
/// same total on every page, and no error.
#[test]
fn a_cursor_walk_under_churn_gets_every_row_once() {
    let (_tmp, root, svc) = paging_index(120);
    let first = super::run_query(&svc, &story_page(25, None), &alice()).unwrap();
    assert_eq!(first.total, 120);
    let generation = first.generation;
    let mut seen: Vec<String> = ids(&first);
    let mut cursor = first.next_cursor.clone();
    let mut step = 0;
    while let Some(c) = cursor {
        churn(&svc, &root, step);
        step += 1;
        assert!(svc.snapshot().generation > generation, "the index moved");
        let page = super::run_query(&svc, &story_page(25, Some(&c)), &alice())
            .unwrap_or_else(|e| panic!("page {step} under churn: {e}"));
        assert_eq!(page.generation, generation);
        assert_eq!(page.total, 120);
        seen.extend(ids(&page));
        cursor = page.next_cursor;
    }
    assert_eq!(step, 4);
    let expected: Vec<String> = (0..120).map(|i| format!("P-{i:03}")).collect();
    assert_eq!(
        seen, expected,
        "every row once, in order, none of the new ones"
    );
    // A new walk reads the newest generation.
    let fresh = super::run_query(&svc, &story_page(500, None), &alice()).unwrap();
    assert_eq!(fresh.total, 124);
}

/// SV-PBF-287 cases 8 and 9: a cursor whose pin lapsed (30 s idle, or 120 s
/// however busy) or from an earlier app run is `StaleCursor`, whose message
/// starts with `stale_cursor` (the in-app bridge passes it on); a malformed
/// cursor, or another query's, stays `InvalidCursor`.
#[test]
fn a_lapsed_or_earlier_run_cursor_is_stale_and_a_malformed_one_is_invalid() {
    use super::service::{PIN_IDLE, PIN_MAX};
    use std::time::Duration;
    let (_tmp, root, svc) = paging_index(60);
    let page = |cursor: &str| super::run_query(&svc, &story_page(5, Some(cursor)), &alice());

    // Idle past PIN_IDLE once the index moved on: stale.
    let first = super::run_query(&svc, &story_page(5, None), &alice()).unwrap();
    churn(&svc, &root, 0);
    let second = page(first.next_cursor.as_deref().unwrap()).unwrap();
    svc.advance_clock(PIN_IDLE + Duration::from_secs(1));
    let err = page(second.next_cursor.as_deref().unwrap()).unwrap_err();
    assert_eq!(err, QueryError::StaleCursor);
    assert!(err.to_string().starts_with(super::STALE_CURSOR), "{err}");

    // Paged every 25 s: kept until PIN_MAX after it was pinned, then stale.
    let walk = super::run_query(&svc, &story_page(5, None), &alice()).unwrap();
    churn(&svc, &root, 1);
    let mut cursor = walk.next_cursor.unwrap();
    let mut elapsed = Duration::ZERO;
    let step = Duration::from_secs(25);
    while elapsed + step < PIN_MAX {
        svc.advance_clock(step);
        elapsed += step;
        cursor = page(cursor.as_str())
            .unwrap_or_else(|e| panic!("paged at {elapsed:?}: {e}"))
            .next_cursor
            .unwrap();
    }
    svc.advance_clock(step);
    assert_eq!(page(cursor.as_str()).unwrap_err(), QueryError::StaleCursor);

    // From an earlier app run, even at the current generation: stale.
    let now = super::run_query(&svc, &story_page(5, None), &alice()).unwrap();
    let earlier = super::engine::from_an_earlier_run(now.next_cursor.as_deref().unwrap());
    assert_eq!(page(earlier.as_str()).unwrap_err(), QueryError::StaleCursor);

    // Malformed, or another query's: invalid, never stale.
    for bad in ["garbage", "o:5:1:x", "o:5:x:abc:1"] {
        assert!(
            matches!(page(bad), Err(QueryError::InvalidCursor(_))),
            "{bad}"
        );
    }
    let mut other: ContextQuery = serde_json::from_value(
        json!({ "where": { "kinds": ["doc"] }, "limit": 5, "cursor": now.next_cursor }),
    )
    .unwrap();
    assert!(matches!(
        super::run_query(&svc, &other, &alice()),
        Err(QueryError::InvalidCursor(_))
    ));
    // Even once its generation is gone, another query's cursor is invalid, not stale.
    other.cursor = Some(cursor);
    assert!(matches!(
        super::run_query(&svc, &other, &alice()),
        Err(QueryError::InvalidCursor(_))
    ));
}

/// SV-PBF-287 (I4): at most `MAX_PINS` generations stay pinned, the least
/// recently paged one going first, and a reset drops every pin, even one a
/// query that raced the reset tries to add.
#[test]
fn at_most_four_generations_stay_pinned_and_a_reset_drops_them() {
    use super::service::MAX_PINS;
    use std::time::Duration;
    let (_tmp, root, svc) = paging_index(30);
    let mut walks = Vec::new();
    for step in 0..=MAX_PINS {
        let first = super::run_query(&svc, &story_page(5, None), &alice()).unwrap();
        walks.push(first.next_cursor.unwrap());
        churn(&svc, &root, step);
    }
    assert_eq!(svc.pinned().len(), MAX_PINS);
    let page = |cursor: &str| super::run_query(&svc, &story_page(5, Some(cursor)), &alice());
    assert_eq!(
        page(walks[0].as_str()).unwrap_err(),
        QueryError::StaleCursor
    );
    for cursor in &walks[1..] {
        page(cursor.as_str()).unwrap();
    }

    svc.reset();
    assert!(svc.pinned().is_empty());
    assert_eq!(
        page(walks[MAX_PINS].as_str()).unwrap_err(),
        QueryError::StaleCursor
    );

    // A query that ran on the old index while the reset landed keeps nothing pinned.
    assert!(svc.wait_ready(Duration::from_secs(10)).is_some());
    let raced = svc
        .query_with(&story_page(5, None), &alice(), |snapshot| {
            let result = snapshot.query(&story_page(5, None), &alice(), &DiskBodyReader);
            svc.reset();
            result
        })
        .unwrap();
    assert!(raced.next_cursor.is_some());
    assert!(svc.pinned().is_empty());
    assert_eq!(
        page(raced.next_cursor.as_deref().unwrap()).unwrap_err(),
        QueryError::StaleCursor
    );
}

/// SV-PBF-287 (review r1 M1): a `text` walk matches file bodies as they are now,
/// so it pages only the current generation: it is never pinned, and a page
/// after the index moved is `StaleCursor`, never a skipped or repeated row.
#[test]
fn a_text_walk_pages_only_the_current_generation() {
    let (_tmp, root, svc) = paging_index(30);
    let text_page = |cursor: Option<&str>| {
        let mut q = story_page(5, cursor);
        q.text = Some("body".into());
        q
    };
    let first = super::run_query(&svc, &text_page(None), &alice()).unwrap();
    assert_eq!(first.total, 30);
    assert!(svc.pinned().is_empty(), "a text walk is never pinned");
    let second =
        super::run_query(&svc, &text_page(first.next_cursor.as_deref()), &alice()).unwrap();
    assert_eq!(ids(&second)[0], "P-005");
    // A body edit moves the index on: the walk restarts instead of reading
    // today's bodies through yesterday's records.
    let edited = root.join("stories/P-012.md");
    write(
        &edited,
        "---\ntype: story\nstory_id: P-012\nstatus: todo\n---\n# P-012\n\nNo match here.\n",
    );
    let generation = svc.snapshot().generation;
    svc.settle(vec![edited], generation);
    assert_eq!(
        super::run_query(&svc, &text_page(second.next_cursor.as_deref()), &alice()).unwrap_err(),
        QueryError::StaleCursor
    );
    assert!(svc.pinned().is_empty());
}

/// SV-PBF-287 (review r1 L1): only a first page pins. A walk paged on the
/// current generation past `PIN_MAX` keeps working while the index is still,
/// but its lapsed pin is never made again, so once the index moves it is stale.
#[test]
fn a_walk_is_never_pinned_again_after_its_pin_lapsed() {
    use std::time::Duration;
    let (_tmp, root, svc) = paging_index(60);
    let page = |cursor: &str| super::run_query(&svc, &story_page(5, Some(cursor)), &alice());
    let first = super::run_query(&svc, &story_page(5, None), &alice()).unwrap();
    assert_eq!(svc.pinned().len(), 1);
    let mut cursor = first.next_cursor.unwrap();
    for _ in 0..5 {
        svc.advance_clock(Duration::from_secs(25));
        cursor = page(cursor.as_str()).unwrap().next_cursor.unwrap();
    }
    assert!(svc.pinned().is_empty(), "the lapsed pin was not made again");
    churn(&svc, &root, 0);
    assert_eq!(page(cursor.as_str()).unwrap_err(), QueryError::StaleCursor);
}

/// SV-PBF-287 (PBF-272 AC4, the S3 churn gate): walks over about 1,000 stories
/// at limit 200, while a writer thread keeps the index publishing, read every
/// row exactly once from their own generation, with at most `MAX_PINS`
/// generations pinned throughout. Prints the peak pins, their bytes and the
/// time per page (`--nocapture` shows it).
#[test]
fn walks_under_concurrent_writes_read_every_row_once_within_the_pin_cap() {
    use super::service::MAX_PINS;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    let (_tmp, root, svc) = paging_index(1000);
    let start_generation = svc.snapshot().generation;
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let (svc, root, stop) = (svc.clone(), root.clone(), Arc::clone(&stop));
        std::thread::spawn(move || {
            let mut step = 0usize;
            while !stop.load(Ordering::SeqCst) {
                let added = root.join(format!("stories/W-{step:05}.md"));
                let edited = root.join(format!("stories/P-{:03}.md", step % 1000));
                write(&added, &story(&format!("W-{step:05}"), "todo", ""));
                write(
                    &edited,
                    &story(&format!("P-{:03}", step % 1000), &format!("s{step}"), ""),
                );
                let mut paths = vec![added, edited];
                if step >= 3 {
                    let gone = root.join(format!("stories/W-{:05}.md", step - 3));
                    let _ = std::fs::remove_file(&gone);
                    paths.push(gone);
                }
                svc.note_paths(paths);
                step += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
        })
    };
    // A failed assert below must not leave the writer spinning.
    struct StopOnDrop(Arc<AtomicBool>);
    impl Drop for StopOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let _stop_writer = StopOnDrop(Arc::clone(&stop));
    let (mut pages, mut page_time) = (0u32, Duration::ZERO);
    let (mut peak_pins, mut peak_bytes) = (0usize, 0usize);
    let mut timed = |cursor: Option<&str>| {
        let started = Instant::now();
        let page = super::run_query(&svc, &story_page(200, cursor), &alice());
        page_time += started.elapsed();
        pages += 1;
        page
    };
    for walk in 0..5 {
        let first = timed(None).unwrap_or_else(|e| panic!("walk {walk}, first page: {e}"));
        let mut seen = ids(&first);
        let mut cursor = first.next_cursor.clone();
        let mut moved = false;
        while let Some(c) = cursor {
            std::thread::sleep(Duration::from_millis(250));
            moved |= svc.snapshot().generation > first.generation;
            let page = timed(Some(c.as_str())).unwrap_or_else(|e| panic!("walk {walk}: {e}"));
            assert_eq!(page.generation, first.generation, "walk {walk}");
            assert_eq!(page.total, first.total, "walk {walk}");
            seen.extend(ids(&page));
            let pinned = svc.pinned();
            assert!(pinned.len() <= MAX_PINS, "walk {walk}: {pinned:?}");
            peak_pins = peak_pins.max(pinned.len());
            peak_bytes = peak_bytes.max(svc.pinned_bytes());
            cursor = page.next_cursor;
        }
        assert!(moved, "walk {walk}: the index published between its pages");
        let unique: std::collections::HashSet<&String> = seen.iter().collect();
        assert_eq!(unique.len(), seen.len(), "walk {walk}: a row came twice");
        assert_eq!(seen.len(), first.total, "walk {walk}: a row was missed");
    }
    stop.store(true, Ordering::SeqCst);
    writer.join().unwrap();
    assert!(svc.snapshot().generation > start_generation + 5);
    eprintln!(
        "SV-PBF-287 churn: {pages} pages, {:?} per page, peak {peak_pins} pinned generations, about {peak_bytes} bytes pinned",
        page_time / pages
    );
}

// ---- SV-PBF-267 PR 1: per-query ScopeMemo (CallerScope semantics unchanged) ----

mod scope_memo {
    use super::*;
    use crate::context_repo::caller_scope::CallerScope;
    use crate::context_repo::inheritance::{team_view, CANONICALIZE_CALLS};
    use std::sync::Arc;

    const SLUG: &str = "acme/ctx";

    fn scoped(scope: CallerScope) -> Viewer {
        Viewer { scope, ..alice() }
    }

    fn query_all(snap: &Snapshot, viewer: &Viewer, text: Option<&str>) -> ContextQueryResult {
        let mut q = json!({ "limit": 10000 });
        if let Some(t) = text {
            q["text"] = json!(t);
        }
        let q: ContextQuery = serde_json::from_value(q).unwrap();
        snap.query(&q, viewer, &DiskBodyReader).unwrap()
    }

    fn team_root(dir: &Path, team: &str) -> IndexRoot {
        IndexRoot {
            dir: dir.join(team).join("content"),
            org: Some("acme".into()),
            team: Some(team.into()),
            layer: Layer::Team,
        }
    }

    /// Two orgs, 10 acme teams in a parent chain (every other ancestor shares a
    /// subset), 5k files over nested folders, plus another org's team.
    fn generated() -> (tempfile::TempDir, PathBuf, Vec<IndexRoot>) {
        let tmp = tempfile::tempdir().unwrap();
        let acme = tmp.path().join("users/1/orgs/acme/ctx");
        let globex = tmp.path().join("users/1/orgs/globex/ctx");
        write(
            &acme.join("org.yml"),
            "slug: acme/ctx\ndisplay_name: Acme\ncreated_at: \"2026-01-01T00:00:00Z\"\n",
        );
        write(
            &globex.join("org.yml"),
            "slug: globex/ctx\ndisplay_name: Globex\ncreated_at: \"2026-01-01T00:00:00Z\"\n",
        );
        write(&globex.join("teams.yml"), "teams:\n  - name: gx\n");
        let mut teams = String::from("teams:\n  - name: t0\n");
        for i in 1..10 {
            teams.push_str(&format!("  - name: t{i}\n    parent_team: t{}\n", i - 1));
        }
        teams.push_str("  - name: loner\n");
        write(&acme.join("teams.yml"), &teams);
        for i in 0..9 {
            let share = if i % 2 == 0 {
                "share:\n  - 'content/d0/**'\n  - '!content/d0/d1/**'\n"
            } else {
                "share:\n  - 'content/shared-*.md'\n"
            };
            write(&acme.join(format!("t{i}/config/sharing.yml")), share);
        }
        let mut roots = Vec::new();
        for t in (0..10)
            .map(|i| format!("t{i}"))
            .chain(["loner".to_string()])
        {
            for d in 0..10 {
                for sub in 0..5 {
                    for f in 0..10 {
                        write(
                            &acme.join(format!("{t}/content/d{d}/d{sub}/f{f}.md")),
                            &format!("# {t} {d} {sub} {f}\n"),
                        );
                    }
                }
                write(&acme.join(format!("{t}/content/shared-{d}.md")), "shared\n");
            }
            roots.push(team_root(&acme, &t));
        }
        write(&globex.join("gx/content/g.md"), "# g\n");
        roots.push(IndexRoot {
            dir: globex.join("gx/content"),
            org: Some("globex".into()),
            team: Some("gx".into()),
            layer: Layer::Team,
        });
        (tmp, acme, roots)
    }

    fn indexed(roots: Vec<IndexRoot>) -> Snapshot {
        let mut state = IndexState::default();
        state.reconcile(roots);
        state.snapshot(1, true)
    }

    /// The old behaviour: filter every record with `allows`, then query.
    fn expected_paths(snap: &Snapshot, scope: &CallerScope) -> Vec<String> {
        let kept = Snapshot::build(
            snap.records
                .iter()
                .filter(|r| scope.allows(Path::new(&r.path)))
                .cloned()
                .collect(),
            snap.roots.clone(),
            snap.generation,
            true,
        );
        query_all(&kept, &scoped(CallerScope::Human), None)
            .rows
            .into_iter()
            .map(|r| r.path)
            .collect()
    }

    #[test]
    fn memoised_verdict_equals_resolve_read_for_every_record_and_scope_kind() {
        let (_tmp, acme, roots) = generated();
        let snap = indexed(roots);
        assert!(snap.records.len() >= 5_000, "{}", snap.records.len());
        let mut scopes: Vec<(String, CallerScope)> = ["t0", "t3", "t9", "loner", "nobody"]
            .iter()
            .map(|t| (t.to_string(), CallerScope::Team(team_view(&acme, SLUG, t))))
            .collect();
        scopes.push(("unknown".into(), CallerScope::Unknown));
        for (name, scope) in scopes {
            let got: Vec<String> = query_all(&snap, &scoped(scope.clone()), None)
                .rows
                .into_iter()
                .map(|r| r.path)
                .collect();
            let want = expected_paths(&snap, &scope);
            assert_eq!(got, want, "scope {name}");
            // The fixture must exercise both outcomes.
            if name == "t0" {
                assert!(!got.is_empty() && got.len() < snap.records.len());
            }
        }
    }

    /// Canonicalize calls made by one scoped query over the generated 5k fixture.
    fn canonicalize_calls_for(text: Option<&str>) -> (usize, usize, usize) {
        let (_tmp, acme, roots) = generated();
        let root_count = roots.len();
        let snap = indexed(roots);
        let scope = CallerScope::Team(team_view(&acme, SLUG, "t5"));
        let viewer = scoped(scope);
        let mut q = json!({ "limit": 50 });
        if let Some(t) = text {
            q["text"] = json!(t);
        }
        let q: ContextQuery = serde_json::from_value(q).unwrap();
        CANONICALIZE_CALLS.with(|c| c.set(0));
        snap.query(&q, &viewer, &DiskBodyReader).unwrap();
        let calls = CANONICALIZE_CALLS.with(|c| c.get());
        let dirs: std::collections::HashSet<_> = snap
            .records
            .iter()
            .filter_map(|r| Path::new(&r.path).parent().map(Path::to_path_buf))
            .collect();
        assert!(snap.records.len() >= 5_000);
        (calls, root_count + dirs.len() + 50, snap.records.len())
    }

    #[test]
    fn a_scoped_query_canonicalizes_per_dir_not_per_record() {
        let (calls, budget, records) = canonicalize_calls_for(None);
        assert!(calls <= budget, "{calls} > {budget} for {records} records");
        assert!(calls * 4 < records, "{calls} calls for {records} records");
    }

    #[test]
    fn a_scoped_text_query_stays_within_the_same_budget() {
        // Nothing matches, so every candidate body is scanned (and rechecked).
        let (calls, budget, records) = canonicalize_calls_for(Some("zzzneverpresentzzz"));
        assert!(calls <= budget, "{calls} > {budget} for {records} records");
        assert!(calls * 4 < records, "{calls} calls for {records} records");
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_swapped_for_a_symlink_before_reindex_is_denied() {
        // refresh_paths follows intermediate symlinks, so the debounced batch can index
        // child/content/a/secret.md through a -> parent/content. It must stay Denied.
        let (_tmp, root) = crate::context_repo::inheritance::test_fixture();
        let child_root = team_root(&root, "child");
        std::fs::create_dir_all(root.join("child/content/a")).unwrap();
        std::fs::write(root.join("child/content/a/secret.md"), "# mine\n").unwrap();
        let mut state = IndexState::default();
        state.reconcile(vec![child_root.clone()]);
        std::fs::remove_dir_all(root.join("child/content/a")).unwrap();
        std::os::unix::fs::symlink(root.join("parent/content"), root.join("child/content/a"))
            .unwrap();
        state.refresh_paths(&[
            root.join("child/content/a"),
            root.join("child/content/a/secret.md"),
        ]);
        let snap = state.snapshot(2, true);
        // Precondition: the file really was re-indexed through the symlink. If
        // `refresh_paths` is hardened later this must fail loudly, not pass vacuously.
        assert!(
            snap.records
                .iter()
                .any(|r| r.path.ends_with("child/content/a/secret.md")),
            "refresh_paths no longer indexes through a swapped directory symlink: {:?}",
            snap.records.iter().map(|r| &r.path).collect::<Vec<_>>()
        );
        let scope = CallerScope::Team(team_view(&root, SLUG, "child"));
        let got: Vec<String> = query_all(&snap, &scoped(scope.clone()), None)
            .rows
            .into_iter()
            .map(|r| r.path)
            .collect();
        assert_eq!(got, expected_paths(&snap, &scope));
        assert!(
            !got.iter().any(|p| p.ends_with("child/content/a/secret.md")),
            "{got:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_fixture_holds_through_snapshot_query() {
        let fx = crate::context_repo::inheritance::alias_fixture();
        let roots: Vec<IndexRoot> = ["child", "parent", "sibling", "grand"]
            .iter()
            .map(|t| team_root(&fx.root, t))
            .collect();
        let snap = indexed(roots);
        for team in ["child", "sibling"] {
            let scope = CallerScope::Team(team_view(&fx.root, SLUG, team));
            let got = query_all(&snap, &scoped(scope.clone()), None);
            let paths: Vec<&str> = got.rows.iter().map(|r| r.path.as_str()).collect();
            // Symlinks are never indexed, so no alias row can carry a target's verdict.
            assert!(paths.iter().all(|p| !p.contains("alias-")), "{paths:?}");
            // An ancestor's unshared file stays Denied for the descendant (a
            // sibling is "other team" content and is admitted, as before).
            if team == "child" {
                assert!(
                    !paths
                        .iter()
                        .any(|p| p.ends_with("parent/content/secret.md")),
                    "{team}: {paths:?}"
                );
                assert!(!paths.iter().any(|p| p.ends_with("grand/content/secret.md")));
            }
            assert_eq!(
                got.rows.iter().map(|r| r.path.clone()).collect::<Vec<_>>(),
                expected_paths(&snap, &scope),
                "{team}"
            );
        }
        let child = CallerScope::Team(team_view(&fx.root, SLUG, "child"));
        let got = query_all(&snap, &scoped(child), None);
        assert!(got
            .rows
            .iter()
            .any(|r| r.path.ends_with("parent/content/shared.md")));
        assert!(got
            .rows
            .iter()
            .any(|r| r.path.ends_with("child/content/secret.md")));
    }

    fn record_for(root: &IndexRoot, file: &str) -> Arc<super::super::record::ContextRecord> {
        let path = root.dir.join(file);
        let meta = std::fs::metadata(&path).unwrap();
        Arc::new(super::super::index::build_record(root, &path, &meta, &[]).unwrap())
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_root_dir_cannot_expose_the_unshared_parent_files() {
        let (_tmp, root) = crate::context_repo::inheritance::test_fixture();
        // child/content -> parent/content: a lexical prefix check would admit these as "own".
        std::fs::remove_dir_all(root.join("child/content")).unwrap();
        std::os::unix::fs::symlink(root.join("parent/content"), root.join("child/content"))
            .unwrap();
        let child_root = team_root(&root, "child");
        let records = vec![
            record_for(&child_root, "secret.md"),
            record_for(&child_root, "shared.md"),
        ];
        let snap = Snapshot::build(records, vec![child_root], 1, true);
        let scope = CallerScope::Team(team_view(&root, SLUG, "child"));
        let got = query_all(&snap, &scoped(scope.clone()), None);
        let names: Vec<&str> = got.rows.iter().map(|r| r.file_name.as_str()).collect();
        assert_eq!(
            names,
            vec!["shared.md"],
            "parent's unshared secret.md is Denied"
        );
        assert_eq!(
            got.rows.iter().map(|r| r.path.clone()).collect::<Vec<_>>(),
            expected_paths(&snap, &scope)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_file_swapped_for_a_symlink_after_indexing_is_not_body_read() {
        let (_tmp, root) = crate::context_repo::inheritance::test_fixture();
        let child_root = team_root(&root, "child");
        let snap = Snapshot::build(
            vec![record_for(&child_root, "shared.md")],
            vec![child_root.clone()],
            1,
            true,
        );
        let scope = CallerScope::Team(team_view(&root, SLUG, "child"));
        let viewer = scoped(scope);
        let before = query_all(&snap, &viewer, Some("child"));
        assert_eq!(before.rows.len(), 1, "own body is readable before the swap");
        // Inside the watcher debounce: the indexed file becomes a link to an unshared file.
        let file = root.join("child/content/shared.md");
        std::fs::remove_file(&file).unwrap();
        std::os::unix::fs::symlink(root.join("parent/content/secret.md"), &file).unwrap();
        let after = query_all(&snap, &viewer, Some("parent-secret"));
        assert!(after.rows.is_empty(), "{:?}", after.rows);
    }
}
