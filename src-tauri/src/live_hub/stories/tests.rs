// SPDX-License-Identifier: MIT
use super::*;
use crate::commands::pipeline_scanner::{scan_pipeline_tree_impl, DerivedPipelineTree};
use crate::live_hub::LiveHub;
use std::{
    io::Write,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};
use tauri::{Emitter, Listener};

fn native_rows(tree: &DerivedPipelineTree, root: &Path) -> Vec<Value> {
    let mut child = Command::new("pnpm")
        .args(["exec", "tsx", "tests/support/project-stories-fixture.ts"])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let input = json!({"tree":tree,"states":crate::commands::status_taxonomy::load_status_taxonomy_from(root).states});
    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(&input).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn files(tree: Value) -> NativeFiles {
    serde_json::from_value(json!({"tree":tree,"starterPage":null,"mergePaths":[],"repos":{}}))
        .unwrap()
}
#[tokio::test]
async fn real_scan_native_adapter_and_real_hub_stream_one_status_delta() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let story = root.join("story.md");
    let write = |status| {
        std::fs::write(&story, format!("---\ntype: story\nstory_id: S-1\ntitle: Declared title\nstatus: {status}\nsprint: '1'\n---\n")).unwrap()
    };
    write("todo");
    std::fs::write(root.join("README.md"), "---\ntype: epic\nstory_id: E-1\ntitle: Epic title\nsource_repo: invalid\nstatus: in-progress\nsprint: '1'\n---\n").unwrap();
    let scan = || scan_pipeline_tree_impl(&root, "", "", "README.md").unwrap();
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(LiveHub::default());
    let tree = scan();
    publish_native(
        app.handle(),
        "acme:a",
        native_rows(&tree, &root),
        files(json!(tree)),
    )
    .unwrap();
    let first = app
        .state::<LiveHub>()
        .model
        .lock()
        .unwrap()
        .projections
        .subscribe_window("a", "stories", "team:acme/a", None, None)
        .unwrap();
    assert!(first
        .snapshot
        .as_ref()
        .unwrap()
        .iter()
        .any(|r| r["kind"] == "epic"
            && r["title"] == "Epic title"
            && r["storyId"] == "E-1"
            && r["metadataValid"] == false));
    assert!(first
        .snapshot
        .unwrap()
        .iter()
        .any(|r| r["storyId"] == "S-1" && r["ready"] == true && r["title"] == "Declared title"));
    app.state::<LiveHub>()
        .model
        .lock()
        .unwrap()
        .projections
        .subscribe_window("b", "stories", "team:acme/b", None, None)
        .unwrap();
    let sent = Arc::new(Mutex::new(Vec::<Value>::new()));
    let capture = sent.clone();
    app.listen_any("codesense:subscription-delta", move |event| {
        capture
            .lock()
            .unwrap()
            .push(serde_json::from_str(event.payload()).unwrap())
    });
    // Use the existing watcher ingress and coalescer; no new watcher or polling loop.
    crate::live_hub::files::start(app.handle());
    write("in-progress");
    app.emit("context-file-change", json!({"file_path":story}))
        .unwrap();
    assert_eq!(
        app.state::<LiveHub>().next_batch().await,
        crate::live_hub::FILES
    );
    let tree = scan();
    let rows = native_rows(&tree, &root);
    publish_native(app.handle(), "acme:a", rows.clone(), files(json!(tree))).unwrap();
    publish_native(app.handle(), "acme:a", rows, files(json!(tree))).unwrap();
    let sent = sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["subscriptionId"], "a");
    let delta = &sent[0]["delta"];
    assert_eq!(delta["topic"], "stories");
    assert_eq!(delta["upserts"].as_array().unwrap().len(), 1);
    assert_eq!(delta["upserts"][0]["status"], "in-progress");
    assert_eq!(delta["upserts"][0]["role"], "active");
    assert_eq!(delta["upserts"][0]["ready"], false);
    assert_eq!(delta["removes"], json!([]));
}

#[test]
fn files_use_native_starter_merge_override_and_installed_group_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    for file in ["start.md", "settings.json", "guide.md"] {
        std::fs::write(root.join(file), "{}").unwrap();
    }
    let access = ContentAccess::new(
        root.clone(),
        None,
        crate::context_repo::caller_scope::CallerScope::Human,
    );
    let tree = json!({"absoluteDirPath":root,"dirPath":"","dirName":"team","docs":[
        {"fileName":"guide.md","filePath":root.join("guide.md"),"overridesTeam":"platform"}
    ],"subEpics":[{"absoluteDirPath":"installed://owner/repo/repo content","dirPath":"repo content","dirName":"repo content","docs":[
        {"fileName":"source.md","filePath":"/native/source.md","installedSourceRepo":"owner/repo","shadowsTeamFile":true}
    ],"subEpics":[]}]});
    let mut model = files(tree);
    model.starter_page = Some(StarterPage {
        path: root.join("start.md").to_string_lossy().into(),
        root: root.to_string_lossy().into(),
    });
    model.merge_paths = vec!["settings.json".into()];
    model.repos.insert(
        "owner/repo".into(),
        json!({"glyph":"R","tint":"native-tint"}),
    );
    let rows = model.listing(
        &access,
        &ListFiles::default(),
        access.list_rows(None).unwrap(),
    );
    let row = |name| rows.iter().find(|r| r["name"] == name).unwrap();
    assert_eq!(row("start.md")["starterPage"], true);
    assert_eq!(row("settings.json")["mergeMarked"], true);
    assert_eq!(row("guide.md")["overrides"], "platform");
    assert_eq!(row("repo content")["repoGlyph"], "R");
    assert_eq!(row("repo content")["repoTint"], "native-tint");
    let args = ListFiles {
        dir: Some("installed://owner/repo/repo content".into()),
        ..Default::default()
    };
    assert!(model.installed_dir(args.dir.as_deref().unwrap()));
    let children = model.listing(&access, &args, vec![]);
    assert_eq!(children.len(), 1);
    assert_eq!(children[0]["shadows"], true);
    assert_eq!(children[0]["repoGlyph"], "R");
}

#[test]
fn listed_installed_rows_open_and_read_in_the_requesting_team() {
    use crate::commands::file_surface::{call, tests::SurfaceFixture};
    use crate::routines::test_support::{ORG, TEAM};
    let fixture = SurfaceFixture::new();
    fixture.app.manage(LiveHub::default());
    let source = fixture.org_root.join("installed-repo/source.md");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, "installed source").unwrap();
    let tree = json!({"absoluteDirPath":fixture.team_root,"dirPath":"","docs":[],"subEpics":[
        {"absoluteDirPath":"installed://owner/repo/docs","dirPath":"docs","dirName":"docs","docs":[
            {"fileName":"source.md","filePath":source,"installedSourceRepo":"owner/repo"}
        ],"subEpics":[]}
    ]});
    publish_native(
        fixture.app.handle(),
        &format!("{ORG}:{TEAM}"),
        vec![],
        files(tree),
    )
    .unwrap();
    let frame = fixture.frame("file-nav", Some(TEAM), "open");
    // The viewer remains on the requesting team after another team becomes active.
    std::fs::write(
        fixture.org_root.join("teams.yml"),
        format!("teams:\n  - name: {TEAM}\n  - name: other\n"),
    )
    .unwrap();
    std::fs::create_dir_all(crate::context_repo::path_to_team_content_dir(
        &fixture.org_root,
        "other",
    ))
    .unwrap();
    fixture
        .app
        .state::<crate::mcp::state::ActiveTeamState>()
        .update(crate::mcp::state::ActiveTeam {
            org_slug: ORG.into(),
            team_name: "other".into(),
        });
    tauri::async_runtime::block_on(async {
        let view_key = crate::live_hub::files::file_surface_prepare_view(
            fixture.app.handle().clone(),
            frame.frame_id.clone(),
            ListFiles {
                dir: Some("installed://owner/repo/docs".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let model = fixture.app.state::<LiveHub>();
        let rows = model
            .model
            .lock()
            .unwrap()
            .projections
            .subscribe_window("installed-open", "files", &view_key, None, None)
            .unwrap()
            .snapshot
            .unwrap();
        assert_eq!(rows.len(), 1);
        let args = json!({"path":rows[0]["path"]});
        let opened = call(
            fixture.app.handle().clone(),
            "resolve-open",
            args.clone(),
            Some(frame.frame_id.clone()),
        )
        .await;
        let read = call(
            fixture.app.handle().clone(),
            "read",
            args.clone(),
            Some(frame.frame_id.clone()),
        )
        .await;
        let deleted = call(
            fixture.app.handle().clone(),
            "delete",
            args,
            Some(frame.frame_id),
        )
        .await;
        assert!(deleted.is_err(), "installed rows remain browse-only");
        assert!(
            opened.is_ok() && read.is_ok(),
            "installed row: open={opened:?}, read={read:?}"
        );
        assert_eq!(
            opened.unwrap(),
            json!({"path":source,"org":ORG,"team":TEAM})
        );
        assert_eq!(read.unwrap()["text"], "installed source");
    });
}

#[test]
fn installed_admission_keeps_host_scope_when_the_selected_team_is_an_ancestor() {
    use crate::commands::file_surface::{access, call};
    use crate::context_repo::caller_scope::tests::AppFixture;
    use crate::dashboard_stream::frame::FrameStore;
    let f = AppFixture::new();
    f.app.manage(f.fixture.service());
    f.app.manage(LiveHub::default());
    let child = f.frame(&f.fixture.path("parent", "panes/parent.html"), "workspace");
    let mut grant = child.grant.clone();
    grant.team.as_mut().unwrap().team = "parent".into();
    let frame = f.app.state::<Arc<FrameStore>>().mint(
        9321,
        grant,
        child.content_root.clone(),
        child.document_path.clone(),
        child.asset_folder.clone(),
        child.policy.clone(),
        None,
    );
    let shared = f.fixture.path("parent", "content/shared.md");
    let hidden = f.fixture.path("parent", "content/secret.md");
    publish_native(f.app.handle(), "acme/ctx:parent", vec![], files(json!({
        "absoluteDirPath":f.fixture.path("parent", "content"),"dirPath":"","docs":[],
        "subEpics":[{"absoluteDirPath":"installed://owner/repo/docs","dirPath":"docs","dirName":"docs",
            "docs":[
                {"fileName":"shared.md","filePath":shared,"installedSourceRepo":"owner/repo"},
                {"fileName":"secret.md","filePath":hidden,"installedSourceRepo":"owner/repo"}
            ],"subEpics":[]}]
    }))).unwrap();
    tauri::async_runtime::block_on(async {
        let args = ListFiles {
            dir: Some("installed://owner/repo/docs".into()),
            ..Default::default()
        };
        let (access, key) = access(f.app.handle(), &args, Some(&frame.frame_id))
            .await
            .unwrap();
        assert!(
            matches!(&access.caller, crate::context_repo::caller_scope::CallerScope::Team(view) if view.reader_team() == "child")
        );
        assert_eq!(key, "team:acme/ctx/parent");
        let listed = f.app.state::<LiveHub>().model.lock().unwrap().nav_files[&key].listing(
            &access,
            &args,
            vec![],
        );
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0]["path"], json!(shared));
        for op in ["resolve-open", "read"] {
            let invoke = |path: &Path| {
                call(
                    f.app.handle().clone(),
                    op,
                    json!({"path":path}),
                    Some(frame.frame_id.clone()),
                )
            };
            assert!(invoke(&shared).await.is_ok(), "shared {op}");
            assert_eq!(
                invoke(&hidden).await.unwrap_err(),
                invoke(&f.fixture.path("parent", "content/missing.md"))
                    .await
                    .unwrap_err()
            );
        }
    });
}

#[test]
fn direct_files_list_of_an_installed_folder_matches_its_subscription() {
    use crate::commands::file_surface::{call, tests::SurfaceFixture};
    use crate::routines::test_support::{ORG, TEAM};
    let fixture = SurfaceFixture::new();
    fixture.app.manage(LiveHub::default());
    let source = fixture.org_root.join("installed-repo/source.md");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, "installed source").unwrap();
    let tree = json!({"absoluteDirPath":fixture.team_root,"dirPath":"","docs":[],"subEpics":[
        {"absoluteDirPath":"installed://owner/repo/docs","dirPath":"docs","dirName":"docs","docs":[
            {"fileName":"source.md","filePath":source,"installedSourceRepo":"owner/repo","shadowsTeamFile":true}
        ],"subEpics":[]}
    ]});
    let mut native = files(tree);
    native.repos.insert(
        "owner/repo".into(),
        json!({"glyph":"R","tint":"native-tint"}),
    );
    publish_native(
        fixture.app.handle(),
        &format!("{ORG}:{TEAM}"),
        vec![],
        native,
    )
    .unwrap();
    let frame = fixture.frame("file-nav", Some(TEAM), "list");
    tauri::async_runtime::block_on(async {
        for dir in [Some("installed://owner/repo/docs"), None] {
            let view_key = crate::live_hub::files::file_surface_prepare_view(
                fixture.app.handle().clone(),
                frame.frame_id.clone(),
                ListFiles {
                    dir: dir.map(Into::into),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            let subscribed = fixture
                .app
                .state::<LiveHub>()
                .model
                .lock()
                .unwrap()
                .projections
                .subscribe_window("installed-list", "files", &view_key, None, None)
                .unwrap()
                .snapshot
                .unwrap();
            let listed = call(
                fixture.app.handle().clone(),
                "list",
                json!({"dir":dir}),
                Some(frame.frame_id.clone()),
            )
            .await
            .unwrap();
            let by_path = |rows: &[Value]| {
                let mut rows = rows.to_vec();
                rows.sort_by_key(|r| r["relativePath"].to_string());
                rows
            };
            let listed = listed["rows"].as_array().unwrap();
            assert!(!subscribed.is_empty(), "subscription rows for {dir:?}");
            assert_eq!(by_path(listed), by_path(&subscribed), "dir {dir:?}");
        }
    });
}
