// SPDX-License-Identifier: MIT
use super::*;
use crate::live_hub::{projection::TopicRegistry, view_key};
use crate::routines::test_support::ORG;

#[tokio::test]
async fn pbf211_files_hub_tracks_create_atomic_rename_and_delete() {
    let (fixture, key, _) = prepared_fixture("docs", false).await;
    let created = fixture.team_root.join("docs/native-created.md");
    let renamed = fixture.team_root.join("docs/native-renamed.md");
    let contains = |path: &std::path::Path| {
        let hub = fixture.app.state::<super::super::LiveHub>();
        let snapshot = hub.snapshot(&key, "files").unwrap();
        snapshot
            .snapshot
            .iter()
            .any(|row| row["path"] == path.to_str().unwrap())
    };
    std::fs::write(&created, "# New").unwrap();
    saved(&fixture, &created).await;
    assert!(
        contains(&created),
        "new file missing from subscribed hub snapshot"
    );
    let temporary = created.with_extension("tmp");
    std::fs::write(&temporary, "# Replacement").unwrap();
    std::fs::rename(&temporary, &created).unwrap();
    saved(&fixture, &created).await;
    assert!(contains(&created));
    let snapshot = fixture
        .app
        .state::<super::super::LiveHub>()
        .snapshot(&key, "files")
        .unwrap();
    assert_eq!(
        snapshot
            .snapshot
            .iter()
            .find(|row| row["path"] == created.to_str().unwrap())
            .unwrap()["size"],
        "# Replacement".len()
    );
    std::fs::rename(&created, &renamed).unwrap();
    saved(&fixture, &created).await;
    saved(&fixture, &renamed).await;
    assert!(!contains(&created));
    assert!(contains(&renamed));
    std::fs::remove_file(&renamed).unwrap();
    saved(&fixture, &renamed).await;
    assert!(!contains(&renamed));
}

#[tokio::test]
async fn files_view_key_keeps_page_fields_first_and_trusted_fields_last() {
    let (_fixture, key, _) = prepared_fixture("docs", true).await;
    let serialized = key.split_once("|files:").unwrap().1;
    let tuple: Value = serde_json::from_str(serialized).unwrap();
    let tuple = tuple.as_array().unwrap();
    assert_eq!(
        &tuple[..5],
        &[
            json!("team"),
            Value::Null,
            Value::Null,
            Value::Null,
            json!("docs")
        ]
    );
    assert_eq!(tuple.len(), 7);
}

#[test]
fn files_view_key_keeps_distinct_page_fields_in_sdk_order() {
    let directory = tempfile::tempdir().unwrap();
    let access = ContentAccess::new(
        directory.path().to_path_buf(),
        None,
        crate::context_repo::caller_scope::CallerScope::Human,
    );
    let args = ListFiles {
        scope: Some("scope-value".into()),
        team: Some("team-value".into()),
        story_id: Some("story-value".into()),
        agent_id: Some("agent-value".into()),
        dir: Some("directory-value".into()),
    };

    let key = key("base", &args, &access);
    let serialized = key.split_once("|files:").unwrap().1;
    let tuple: Value = serde_json::from_str(serialized).unwrap();

    assert_eq!(
        &tuple.as_array().unwrap()[..5],
        &[
            json!("scope-value"),
            json!("team-value"),
            json!("story-value"),
            json!("agent-value"),
            json!("directory-value")
        ]
    );
}

#[tokio::test]
async fn nested_files_projection_streams_own_and_inherited_deltas() {
    use crate::routines::test_support::TEAM;
    let (fixture, _, _) = prepared_fixture("docs", true).await;
    for content in [
        fixture.team_root.clone(),
        fixture.org_root.join("platform/content"),
    ] {
        for dir in [
            "docs",
            "docs/level2",
            "docs/level2/level3",
            "docs/level2/level3/level4",
        ] {
            std::fs::create_dir_all(content.join(dir)).unwrap();
            let name = if content == fixture.team_root {
                "own.md"
            } else {
                "inherited.md"
            };
            std::fs::write(content.join(dir).join(name), "deep content").unwrap();
        }
    }
    let frame = fixture.frame("file-nav", Some(TEAM), "open");
    let mut keys = BTreeSet::new();
    let mut opened = Vec::new();
    for (level, dir) in ["docs", "docs/level2", "docs/level2/level3"]
        .iter()
        .enumerate()
    {
        let args = ListFiles {
            dir: Some(fixture.team_root.join(dir).to_string_lossy().into_owned()),
            ..Default::default()
        };
        let key =
            file_surface_prepare_view(fixture.app.handle().clone(), frame.frame_id.clone(), args)
                .await
                .unwrap();
        assert!(
            keys.insert(key.clone()),
            "each directory needs its own view"
        );
        let hub = fixture.app.state::<super::super::LiveHub>();
        let mut model = hub.model.lock().unwrap();
        let reply = model
            .projections
            .subscribe_window(&format!("level-{level}"), "files", &key, None, None)
            .unwrap();
        let rows = reply.snapshot.unwrap();
        assert!(
            rows.iter().any(|row| row["name"] == "own.md"),
            "missing own children of {dir}: {rows:?}"
        );
        assert!(
            rows.iter()
                .any(|row| row["name"] == "inherited.md" && row["inheritedFrom"] == "platform"),
            "missing inherited children of {dir}: {rows:?}"
        );
        assert!(
            rows.iter().any(|row| file_surface::is_folder(row)),
            "missing next level of {dir}"
        );
        opened.push((dir.to_string(), key));
    }
    let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let capture = sent.clone();
    fixture
        .app
        .listen_any("codesense:subscription-delta", move |event| {
            capture
                .lock()
                .unwrap()
                .push(serde_json::from_str(event.payload()).unwrap());
        });
    for (dir, key) in opened {
        for (content, name) in [
            (fixture.team_root.clone(), "own.md"),
            (fixture.org_root.join("platform/content"), "inherited.md"),
        ] {
            sent.lock().unwrap().clear();
            let path = content.join(&dir).join(name);
            std::fs::write(&path, format!("streamed change in {dir}/{name}")).unwrap();
            saved(&fixture, &path).await;
            let messages = sent.lock().unwrap();
            assert!(
                messages.iter().any(|message| {
                    message["delta"]["viewKey"] == key
                        && message["delta"]["upserts"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|row| row["name"] == name)
                }),
                "nested view must stream {dir}/{name}: {messages:?}"
            );
        }
    }
}
// The browser cache regression consumes this exact projection row: the folder
// belongs to the child, but its displayed epic metadata belongs to the parent.
#[tokio::test]
async fn own_folder_inherited_epic_matches_first_paint_fixture() {
    use crate::routines::test_support::TEAM;
    let (fixture, old_key, _) = prepared_fixture("docs", true).await;
    let own = fixture.team_root.join("docs/folder");
    let parent_root = fixture.org_root.join("platform/content");
    let parent = parent_root.join("docs/folder");
    std::fs::create_dir_all(&own).unwrap();
    std::fs::create_dir_all(&parent).unwrap();
    std::fs::write(own.join("notes.md"), "own notes").unwrap();
    std::fs::write(
        parent.join("README.md"),
        "---\ntype: epic\nstory_id: P-1\ntitle: Parent roadmap\nstatus: review\n---\n",
    )
    .unwrap();
    std::fs::write(
        parent.join("story.md"),
        "---\nstory_id: P-2\n---\n# Parent story",
    )
    .unwrap();
    let frame = fixture.frame("file-nav", Some(TEAM), "open");
    for shared in [true, false] {
        if !shared {
            std::fs::write(
                fixture.org_root.join("platform/config/sharing.yml"),
                "share: []\n",
            )
            .unwrap();
        }
        fixture
            .app
            .state::<super::super::LiveHub>()
            .model
            .lock()
            .unwrap()
            .files
            .views
            .remove(&old_key);
        let key = file_surface_prepare_view(
            fixture.app.handle().clone(),
            frame.frame_id.clone(),
            ListFiles {
                dir: Some("docs".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let hub = fixture.app.state::<super::super::LiveHub>();
        let model = hub.model.lock().unwrap();
        let folder = model.files.views[&key]
            .rows
            .values()
            .find(|row| row["name"] == "folder")
            .unwrap();
        assert!(
            folder.get("inheritedFrom").is_none(),
            "this is an own folder"
        );
        if shared {
            let normalized = serde_json::to_string(folder)
                .unwrap()
                .replace(fixture.team_root.to_str().unwrap(), "/content")
                .replace(parent_root.to_str().unwrap(), "/parent/content");
            let expected: Value = serde_json::from_str(include_str!(
                "../../../tests/support/fixtures/files-tree-inherited-epic.json"
            ))
            .unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&normalized).unwrap(),
                expected
            );
        } else {
            assert_eq!(folder["kind"], "dir");
            assert_eq!(folder["storyCount"], 0);
            for field in ["storyId", "title", "status", "epicFilePath"] {
                assert!(
                    folder.get(field).is_none(),
                    "withdrawn field {field}: {folder:?}"
                );
            }
        }
    }
}
fn content_fixture(
    inherited: bool,
) -> (
    crate::commands::file_surface::tests::SurfaceFixture,
    PathBuf,
) {
    use crate::routines::test_support::TEAM;
    let fixture = crate::commands::file_surface::tests::SurfaceFixture::new();
    fixture.app.manage(super::super::LiveHub::default());
    std::fs::write(
        fixture.org_root.join("teams.yml"),
        format!("teams:\n  - name: {TEAM}\n    parent_team: platform\n  - name: platform\n"),
    )
    .unwrap();
    let own = fixture.team_root.join("docs");
    let parent = fixture.org_root.join("platform/content/docs");
    std::fs::create_dir_all(&own).unwrap();
    std::fs::create_dir_all(&parent).unwrap();
    std::fs::create_dir_all(fixture.org_root.join("platform/config")).unwrap();
    std::fs::write(
        fixture.org_root.join("platform/config/sharing.yml"),
        "share: ['content/docs/**']\n",
    )
    .unwrap();
    let changed = if inherited {
        parent.join("a.md")
    } else {
        own.join("a.md")
    };
    std::fs::write(&changed, "first").unwrap();
    start(fixture.app.handle());
    (fixture, changed)
}

async fn prepared_fixture(
    dir: &str,
    inherited: bool,
) -> (
    crate::commands::file_surface::tests::SurfaceFixture,
    String,
    PathBuf,
) {
    use crate::routines::test_support::TEAM;
    let (fixture, changed) = content_fixture(inherited);
    let parent = fixture.org_root.join("platform/content/docs");
    let frame = fixture.frame("file-nav", Some(TEAM), "open");
    let args = ListFiles {
        dir: Some(if dir == "inherited" {
            parent.to_string_lossy().into_owned()
        } else {
            dir.into()
        }),
        ..Default::default()
    };
    let key = file_surface_prepare_view(fixture.app.handle().clone(), frame.frame_id, args)
        .await
        .unwrap();
    fixture
        .app
        .state::<super::super::LiveHub>()
        .model
        .lock()
        .unwrap()
        .projections
        .subscribe_window("regression", "files", &key, None, None)
        .unwrap();
    (fixture, key, changed)
}
async fn saved(
    fixture: &crate::commands::file_surface::tests::SurfaceFixture,
    path: &std::path::Path,
) {
    use tauri::Emitter;
    let payload = if path.starts_with(&fixture.team_root) {
        json!({"file_path":path})
    } else {
        json!({"file_path":path,"inherited_by":fixture.team_root})
    };
    fixture.app.emit("context-file-change", payload).unwrap();
    assert_eq!(
        fixture
            .app
            .state::<super::super::LiveHub>()
            .next_batch()
            .await,
        super::super::FILES
    );
    refresh_with(fixture.app.handle(), |_| async { unreachable!() })
        .await
        .unwrap();
}
#[tokio::test]
async fn relative_directory_preparation_receives_absolute_watcher_events() {
    let (fixture, key, changed) = prepared_fixture("docs", false).await;
    std::fs::write(&changed, "saved through production wiring").unwrap();
    saved(&fixture, &changed).await;
    let hub = fixture.app.state::<super::super::LiveHub>();
    let model = hub.model.lock().unwrap();
    assert_eq!(
        model.files.views[&key]
            .rows
            .values()
            .find(|row| row["path"] == changed.to_str().unwrap())
            .unwrap()["size"],
        "saved through production wiring".len()
    );
}
#[tokio::test]
async fn absolute_inherited_directory_receives_saves() {
    let (fixture, key, changed) = prepared_fixture("inherited", true).await;
    std::fs::write(&changed, "saved inherited file").unwrap();
    saved(&fixture, &changed).await;
    let hub = fixture.app.state::<super::super::LiveHub>();
    let model = hub.model.lock().unwrap();
    assert_eq!(
        model.files.views[&key]
            .rows
            .values()
            .find(|row| row["path"] == changed.to_str().unwrap())
            .unwrap()["size"],
        "saved inherited file".len()
    );
}
#[tokio::test]
async fn absolute_inherited_directory_keeps_merged_summaries_equal_to_snapshot() {
    use crate::routines::test_support::TEAM;
    let (fixture, old_key, _) = prepared_fixture("inherited", true).await;
    let own = fixture.team_root.join("docs/epic");
    let parent = fixture.org_root.join("platform/content/docs/epic");
    std::fs::create_dir_all(&own).unwrap();
    std::fs::create_dir_all(&parent).unwrap();
    let changed = own.join("a.md");
    std::fs::write(
        own.join("README.md"),
        "---\ntype: epic\nstory_id: E-1\ntitle: Roadmap\n---\n",
    )
    .unwrap();
    std::fs::write(&changed, "---\nstory_id: A-1\n---\n# First").unwrap();
    std::fs::write(parent.join("b.md"), "---\nstory_id: B-1\n---\n# Second").unwrap();
    // Forget the prepared view so the command reads the new tree.
    fixture
        .app
        .state::<super::super::LiveHub>()
        .model
        .lock()
        .unwrap()
        .files
        .views
        .remove(&old_key);
    let frame = fixture.frame("file-nav", Some(TEAM), "open");
    let args = ListFiles {
        dir: Some(parent.parent().unwrap().to_string_lossy().into_owned()),
        ..Default::default()
    };
    let key = file_surface_prepare_view(fixture.app.handle().clone(), frame.frame_id, args)
        .await
        .unwrap();
    let before = fixture
        .app
        .state::<super::super::LiveHub>()
        .model
        .lock()
        .unwrap()
        .files
        .views[&key]
        .rows
        .values()
        .find(|row| row["name"] == "epic")
        .unwrap()
        .clone();
    let changed = parent.join("b.md");
    std::fs::write(&changed, "---\nstory_id: B-1\n---\n# Saved").unwrap();
    saved(&fixture, &changed).await;
    let hub = fixture.app.state::<super::super::LiveHub>();
    let model = hub.model.lock().unwrap();
    let after = model.files.views[&key]
        .rows
        .values()
        .find(|row| row["name"] == "epic")
        .unwrap();
    assert_eq!(
        after, &before,
        "a save must use exactly the snapshot summary derivation"
    );
}
/// Exercise preparation, watcher batching and publication, including inherited roots.
async fn save_preserves_siblings(nested: bool, child_override: bool) {
    use crate::routines::test_support::TEAM;
    use tauri::Emitter;
    let (fixture, old_key, inherited) = prepared_fixture("inherited", true).await;
    let relative = if nested { "docs/stories" } else { "docs" };
    let own = fixture.team_root.join(relative);
    let parent = inherited
        .parent()
        .unwrap()
        .join(if nested { "stories" } else { "" });
    std::fs::create_dir_all(&own).unwrap();
    std::fs::create_dir_all(&parent).unwrap();
    let inherited = parent.join("a.md");
    std::fs::write(&inherited, "---\nstory_id: A-1\n---\n# First").unwrap();
    let sibling = own.join("b.md");
    std::fs::write(&sibling, "original sibling").unwrap();
    let hub = fixture.app.state::<super::super::LiveHub>();
    hub.model.lock().unwrap().files.views.remove(&old_key);
    let frame = fixture.frame("file-nav", Some(TEAM), "open");
    let args = ListFiles {
        dir: Some(
            inherited
                .ancestors()
                .nth(if nested { 2 } else { 1 })
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        ),
        ..Default::default()
    };
    let key = file_surface_prepare_view(fixture.app.handle().clone(), frame.frame_id, args)
        .await
        .unwrap();
    hub.model
        .lock()
        .unwrap()
        .projections
        .subscribe_window("regression", "files", &key, None, None)
        .unwrap();
    let logical = format!("{relative}/a.md");
    let sibling_key = format!("{relative}/b.md");
    let child_rows = |view: &FileView| {
        if nested {
            view.folder_rows[relative].clone()
        } else {
            view.rows.clone()
        }
    };
    let before = child_rows(&hub.model.lock().unwrap().files.views[&key]);
    assert_eq!(before[&logical]["inheritedFrom"], "platform");
    let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let capture = sent.clone();
    fixture
        .app
        .listen_any("codesense:subscription-delta", move |event| {
            capture
                .lock()
                .unwrap()
                .push(serde_json::from_str(event.payload()).unwrap());
        });
    std::fs::write(&sibling, "sibling awaiting its own event").unwrap();
    let changed = if child_override {
        own.join("a.md")
    } else {
        inherited.clone()
    };
    std::fs::write(&changed, "ordinary file now").unwrap();
    if !child_override {
        // A valid save from another team shares the same hub batch.
        let unrelated = fixture.org_root.join("other/content/saved.md");
        std::fs::create_dir_all(unrelated.parent().unwrap()).unwrap();
        std::fs::write(&unrelated, "other team's save").unwrap();
        fixture
            .app
            .emit("context-file-change", json!({"file_path":unrelated}))
            .unwrap();
    }
    saved(&fixture, &changed).await;
    let after = child_rows(&hub.model.lock().unwrap().files.views[&key]);
    assert_eq!(after[&logical]["size"], "ordinary file now".len());
    assert_eq!(
        after[&sibling_key], before[&sibling_key],
        "must not relist untouched siblings in a mixed-team batch or child override"
    );
    let messages = sent.lock().unwrap().clone();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["delta"]["upserts"].as_array().unwrap().len(), 1);
    if nested {
        assert_eq!(
            hub.model.lock().unwrap().files.views[&key].rows[relative]["storyCount"],
            0
        );
    }
    if child_override {
        assert_eq!(after[&logical]["id"], json!(changed));
        assert!(after[&logical].get("inheritedFrom").is_none());
        // Later saves in the farther layer must leave the child winner untouched.
        sent.lock().unwrap().clear();
        std::fs::write(&inherited, "farther layer saved").unwrap();
        saved(&fixture, &inherited).await;
        assert_eq!(
            child_rows(&hub.model.lock().unwrap().files.views[&key]),
            after
        );
        assert!(sent.lock().unwrap().is_empty());
    } else {
        assert_eq!(messages[0]["delta"]["removes"], json!([]));
    }
}
#[tokio::test]
async fn one_save_refreshes_only_the_changed_sibling() {
    save_preserves_siblings(false, false).await;
}
#[tokio::test]
async fn file_replaced_by_folder_rebuilds_parent_summary() {
    use tauri::Emitter;
    for inherited in [false, true] {
        let (fixture, key, original) = prepared_fixture("docs", inherited).await;
        let folder = fixture.team_root.join("docs/a.md");
        if !inherited {
            std::fs::remove_file(&original).unwrap();
        }
        std::fs::create_dir(&folder).unwrap();
        let readme = folder.join("README.md");
        let story = folder.join("story.md");
        std::fs::write(
            &readme,
            "---\ntype: epic\nstory_id: E-1\ntitle: Welcome\n---\n",
        )
        .unwrap();
        std::fs::write(&story, "---\nstory_id: S-1\n---\n# First").unwrap();
        // The filesystem is final before this one directory-and-children batch.
        for path in [&readme, &story] {
            fixture
                .app
                .emit("context-file-change", json!({"file_path":path}))
                .unwrap();
        }
        saved(&fixture, &folder).await;
        {
            let hub = fixture.app.state::<super::super::LiveHub>();
            let model = hub.model.lock().unwrap();
            let view = &model.files.views[&key];
            let summary = &view.rows["docs/a.md"];
            assert_eq!(
                summary["kind"], "epic",
                "a file-to-folder transition must rebuild its summary (inherited={inherited})"
            );
            assert_eq!(summary["title"], "Welcome");
            assert_eq!(summary["storyCount"], 1);
            assert_eq!(summary["epicFilePath"], json!(readme));
            assert!(view.folder_rows["docs/a.md"].contains_key("docs/a.md/story.md"));
        }
        std::fs::write(&story, "ordinary file now").unwrap();
        saved(&fixture, &story).await;
        assert_eq!(
            fixture
                .app
                .state::<super::super::LiveHub>()
                .model
                .lock()
                .unwrap()
                .files
                .views[&key]
                .rows["docs/a.md"]["storyCount"],
            0
        );
    }
}
#[tokio::test]
async fn nested_save_refreshes_cached_folder_aggregate_without_rereading_siblings() {
    save_preserves_siblings(true, false).await;
}
#[tokio::test]
async fn nearer_layer_creation_replaces_inherited_row() {
    save_preserves_siblings(false, true).await;
}
#[tokio::test]
async fn nested_nearer_layer_creation_replaces_inherited_row() {
    save_preserves_siblings(true, true).await;
}
#[tokio::test]
async fn inherited_by_refreshes_the_childs_gated_rows() {
    use tauri::Emitter;
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(super::super::LiveHub::default());
    let (_tmp, root) = crate::context_repo::inheritance::test_fixture();
    let view = crate::context_repo::inheritance::team_view(&root, "acme/ctx", "child");
    let own = view.own_content().own_path().unwrap().to_path_buf();
    std::fs::remove_file(own.join("shared.md")).unwrap();
    let access = ContentAccess::new(
        own.clone(),
        Some(view),
        crate::context_repo::caller_scope::CallerScope::Human,
    );
    let args = ListFiles::default();
    let key = key("team:acme/ctx/child", &args, &access);
    let initial = rows(json!({"rows":access.list_rows(None).unwrap()}));
    let inherited = initial.values().find(|r| r["name"] == "shared.md").unwrap()["path"]
        .as_str()
        .unwrap()
        .to_owned();
    {
        let hub = app.state::<super::super::LiveHub>();
        let mut model = hub.model.lock().unwrap();
        model
            .projections
            .replace_view(
                "files",
                &key,
                initial
                    .values()
                    .cloned()
                    .map(|r| (Some("team:acme/ctx/child".into()), r))
                    .collect(),
            )
            .unwrap();
        model
            .projections
            .subscribe_window("child", "files", &key, None, None)
            .unwrap();
        model.files.views.insert(
            key.clone(),
            FileView {
                access,
                args,
                team_key: "team:acme/ctx/child".into(),
                root: PathBuf::new(),
                rows: initial,
                folder_rows: BTreeMap::new(),
            },
        );
    }
    start(app.handle());
    std::fs::write(&inherited, "changed shared bytes").unwrap();
    app.emit(
        "context-file-change",
        json!({"file_path":inherited,"inherited_by":own}),
    )
    .unwrap();
    assert_eq!(
        app.state::<super::super::LiveHub>().next_batch().await,
        super::super::FILES
    );
    refresh_with(app.handle(), |_| async { unreachable!() })
        .await
        .unwrap();
    let hub = app.state::<super::super::LiveHub>();
    let model = hub.model.lock().unwrap();
    let row = &model.files.views[&key].rows["shared.md"];
    assert_eq!(row["inheritedFrom"], "parent");
    assert_eq!(row["size"], "changed shared bytes".len());
}
#[tokio::test]
async fn watcher_batch_publishes_rename_and_clears_deleted_roots_without_cross_team_delivery() {
    use tauri::Emitter;
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(super::super::LiveHub::default());
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap().join("folder");
    std::fs::create_dir(&root).unwrap();
    let old = root.join("old.md");
    let new = root.join("new.md");
    std::fs::write(&old, "hello").unwrap();
    let view = FileView {
        access: ContentAccess::new(
            temporary.path().canonicalize().unwrap(),
            None,
            crate::context_repo::caller_scope::CallerScope::Human,
        ),
        args: ListFiles::default(),
        team_key: "team:acme/a".into(),
        root: PathBuf::from("folder"),
        folder_rows: BTreeMap::new(),
        rows: rows(
            json!({"rows":ContentAccess::new(temporary.path().canonicalize().unwrap(), None, crate::context_repo::caller_scope::CallerScope::Human).list_rows(Some("folder")).unwrap()}),
        ),
    };
    let a = key("team:acme/a", &view.args, &view.access);
    let b = key("team:acme/b", &view.args, &view.access);
    let scoped = format!("{a}-story");
    {
        let hub = app.state::<super::super::LiveHub>();
        let mut model = hub.model.lock().unwrap();
        for key in [&a, &scoped] {
            model
                .projections
                .replace_view(
                    "files",
                    key,
                    view.rows
                        .values()
                        .cloned()
                        .map(|r| (Some("team:acme/a".into()), r))
                        .collect(),
                )
                .unwrap();
            model.files.views.insert(key.clone(), view.clone());
        }
        model.files.views.get_mut(&scoped).unwrap().args.scope = Some("story".into());
        for (id, key) in [("a", &a), ("b", &b), ("story", &scoped)] {
            model
                .projections
                .subscribe_window(id, "files", key, None, None)
                .unwrap();
        }
    }
    let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let capture = sent.clone();
    app.listen_any("codesense:subscription-delta", move |event| {
        capture
            .lock()
            .unwrap()
            .push(serde_json::from_str(event.payload()).unwrap())
    });
    start(app.handle());
    std::fs::rename(&old, &new).unwrap();
    let hidden = root.join(".DS_Store");
    std::fs::write(&hidden, "noise").unwrap();
    for path in [&old, &new, &hidden] {
        app.emit("context-file-change", json!({"file_path":path}))
            .unwrap();
    }
    assert_eq!(
        app.state::<super::super::LiveHub>().next_batch().await,
        super::super::FILES
    );
    // A disappearing story must not abort the same batch's directory update.
    refresh_with(app.handle(), |_| async { Err("File not found".into()) })
        .await
        .unwrap();
    let messages = sent.lock().unwrap().clone();
    assert_eq!(messages.len(), 2);
    assert!(messages.iter().all(|m| m["subscriptionId"] != "b"));
    let delta = &messages
        .iter()
        .find(|m| m["subscriptionId"] == "a")
        .unwrap()["delta"];
    assert_eq!(delta["removes"], json!([old]));
    assert_eq!(delta["upserts"].as_array().unwrap().len(), 1);
    assert_eq!(delta["upserts"][0]["path"], json!(new));
    sent.lock().unwrap().clear();
    app.emit("context-file-change", json!({"file_path":root}))
        .unwrap();
    assert_eq!(
        app.state::<super::super::LiveHub>().next_batch().await,
        super::super::FILES
    );
    refresh_with(app.handle(), |_| async { Err("File not found".into()) })
        .await
        .unwrap();
    assert!(
        sent.lock().unwrap().is_empty(),
        "root refresh preserves listing exclusions"
    );
    std::fs::remove_dir_all(&root).unwrap();
    app.emit(
        "context-file-change",
        json!({"file_path":temporary.path().canonicalize().unwrap()}),
    )
    .unwrap();
    assert_eq!(
        app.state::<super::super::LiveHub>().next_batch().await,
        super::super::FILES
    );
    refresh_with(app.handle(), |_| async { Err("File not found".into()) })
        .await
        .unwrap();
    let messages = sent.lock().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["delta"]["removes"], json!([new]));
    assert_eq!(messages[0]["delta"]["upserts"], json!([]));
}
#[tokio::test]
async fn agent_definition_event_refreshes_want_set_without_a_content_event() {
    use tauri::Emitter;
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(super::super::LiveHub::default());
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let key = "team:acme/a|files:agent";
    {
        let hub = app.state::<super::super::LiveHub>();
        let mut model = hub.model.lock().unwrap();
        model
            .projections
            .replace_view(
                "files",
                key,
                vec![(
                    Some("team:acme/a".into()),
                    json!({"id":"old","relativePath":"old"}),
                )],
            )
            .unwrap();
        model
            .projections
            .subscribe_window("agent-files", "files", key, None, None)
            .unwrap();
        model.files.views.insert(
            key.into(),
            FileView {
                access: ContentAccess::new(
                    root.clone(),
                    None,
                    crate::context_repo::caller_scope::CallerScope::Human,
                ),
                args: ListFiles {
                    scope: Some("agent".into()),
                    ..Default::default()
                },
                team_key: "team:acme/a".into(),
                root: PathBuf::new(),
                folder_rows: BTreeMap::new(),
                rows: [("old".into(), json!({"id":"old","relativePath":"old"}))].into(),
            },
        );
    }
    let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let capture = sent.clone();
    app.listen_any("codesense:subscription-delta", move |event| {
        capture
            .lock()
            .unwrap()
            .push(serde_json::from_str(event.payload()).unwrap())
    });
    start(app.handle());
    app.emit("agents:changed", ()).unwrap();
    assert_eq!(
        app.state::<super::super::LiveHub>().next_batch().await,
        super::super::FILES
    );
    refresh_with(app.handle(), |_| async {
        Ok(json!({"rows":[{"id":"new","relativePath":"new.md","name":"new.md"}]}))
    })
    .await
    .unwrap();
    let sent = sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["subscriptionId"], "agent-files");
    assert_eq!(sent[0]["delta"]["removes"], json!(["old"]));
    assert_eq!(sent[0]["delta"]["upserts"][0]["id"], "new");
}
#[test]
fn files_views_page_beyond_the_initial_window() {
    let mut topics = TopicRegistry::default();
    let key = "team:acme/a|files:team";
    topics
        .replace_view(
            "files",
            key,
            (0..405)
                .map(|n| {
                    (
                        Some("team:acme/a".into()),
                        json!({"id":format!("f{n:04}"),"name":format!("{n}.md"),"kind":"file"}),
                    )
                })
                .collect(),
        )
        .unwrap();
    let first = topics
        .subscribe_window("files", "files", key, None, None)
        .unwrap();
    assert_eq!(first.snapshot.unwrap().len(), 200);
    assert_eq!(first.total, Some(405));
    let second = topics.page("files", &first.cursor.unwrap()).unwrap();
    assert_eq!(second.snapshot.len(), 200);
    let third = topics.page("files", &second.cursor.unwrap()).unwrap();
    assert_eq!(third.snapshot.len(), 5);
    assert!(third.cursor.is_none());
}

#[test]
fn watcher_rename_is_one_remove_and_one_upsert_and_does_not_reach_another_team() {
    let temporary = tempfile::tempdir().unwrap();
    let dir = temporary.path().canonicalize().unwrap();
    let old = dir.as_path().join("before.md");
    let new = dir.as_path().join("after.md");
    let unchanged = dir.as_path().join("stays.md");
    std::fs::write(&old, "hello").unwrap();
    std::fs::write(&unchanged, "same").unwrap();
    let access = ContentAccess::new(
        dir.clone(),
        None,
        crate::context_repo::caller_scope::CallerScope::Human,
    );
    let mut view = FileView {
        access,
        args: ListFiles::default(),
        team_key: "team:acme/a".into(),
        root: PathBuf::new(),
        folder_rows: BTreeMap::new(),
        rows: ContentAccess::new(
            dir.clone(),
            None,
            crate::context_repo::caller_scope::CallerScope::Human,
        )
        .list_rows(None)
        .unwrap()
        .into_iter()
        .map(|row| (row["relativePath"].as_str().unwrap().to_owned(), row))
        .collect(),
    };
    let key_a = key("team:acme/a", &view.args, &view.access);
    let key_b = key("team:acme/b", &view.args, &view.access);
    let mut topics = TopicRegistry::default();
    topics
        .replace_view(
            "files",
            &key_a,
            view.rows
                .values()
                .cloned()
                .map(|r| (Some("team:acme/a".into()), r))
                .collect(),
        )
        .unwrap();
    topics
        .subscribe_window("a", "files", &key_a, None, Some([0, 200]))
        .unwrap();
    topics
        .subscribe_window("b", "files", &key_b, None, Some([0, 200]))
        .unwrap();
    std::fs::rename(&old, &new).unwrap();
    let changed = [old.clone(), new.clone()]
        .into_iter()
        .map(|physical| {
            let logical = view.access.logical_path(&physical).unwrap();
            (physical, logical)
        })
        .collect();
    update(&mut view, &changed);
    let deltas = topics
        .replace_view(
            "files",
            &key_a,
            view.rows
                .values()
                .cloned()
                .map(|r| (Some("team:acme/a".into()), r))
                .collect(),
        )
        .unwrap();
    let sent = topics.deliver(&deltas);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "a");
    assert_eq!(sent[0].1.removes, vec![old.to_string_lossy()]);
    assert_eq!(sent[0].1.upserts.len(), 1);
    assert_eq!(sent[0].1.upserts[0]["path"], new.to_string_lossy().as_ref());
    assert!(view_key::admits(&key_a, Some("team:acme/a")));
    assert!(!view_key::admits(&key_b, Some("team:acme/a")));
}

#[test]
fn stories_topic_has_team_scoped_windows_and_replay() {
    let hub = super::super::LiveHub::default();
    let mut model = hub.model.lock().unwrap();
    let key = "team:acme/a";
    model
        .projections
        .replace_view(
            "stories",
            key,
            (0..405)
                .map(|n| {
                    (
                        Some(key.to_owned()),
                        json!({"id":format!("s{n:04}"),"kind":"story","status":"todo"}),
                    )
                })
                .collect(),
        )
        .unwrap();
    let first = model
        .projections
        .subscribe_window("stories-a", "stories", key, None, None)
        .unwrap();
    assert_eq!(first.snapshot.unwrap().len(), 200);
    assert_eq!(first.total, Some(405));
    let page = model
        .projections
        .page("stories-a", &first.cursor.unwrap())
        .unwrap();
    assert_eq!(page.snapshot.len(), 200);
    assert!(model
        .projections
        .snapshot("stories", "team:acme/b")
        .unwrap()
        .snapshot
        .is_empty());
}

// Drive the real activation-installed handler through the native callback seam.
async fn registered_recovery_route(shape: &str) {
    use crate::commands::watch::{
        register_team_activation_watches, start_watching_impl, WatcherState,
    };
    use crate::content_engine::watcher::route_test_event;
    use crate::routines::test_support::TEAM;
    use notify::{
        event::{Flag, ModifyKind},
        Event, EventKind,
    };
    for configuration_first in [false, true] {
        let (fixture, key, _) = prepared_fixture("docs", false).await;
        let state = fixture.app.state::<WatcherState>();
        let home = fixture.org_root.ancestors().nth(5).unwrap();
        let configure = || {
            super::super::configuration::prepare(
                fixture.app.handle(),
                home,
                &crate::domain::Principal::from(1u64),
                "prefs",
                Some("global"),
            )
            .unwrap();
            super::super::configuration::watch_prefs(fixture.app.handle(), false);
            let scratch = crate::context_repo::scratch_space::scratch_root(
                home,
                &crate::domain::Principal::from(1u64),
            );
            std::fs::create_dir_all(&scratch).unwrap();
            crate::commands::scratch_watch::register_scratch_watch(
                fixture.app.handle(),
                &state,
                home,
                1,
            );
        };
        start_watching_impl(fixture.app.handle().clone(), vec![], 0).unwrap();
        if configuration_first {
            configure();
        }
        register_team_activation_watches(
            fixture.app.handle(),
            &state,
            &fixture.org_root,
            ORG,
            TEAM,
        );
        if !configuration_first {
            configure();
        }
        for restart in [false, true] {
            if restart {
                start_watching_impl(fixture.app.handle().clone(), vec![], 0).unwrap();
                configure();
                register_team_activation_watches(
                    fixture.app.handle(),
                    &state,
                    &fixture.org_root,
                    ORG,
                    TEAM,
                );
            }
            // Keep the real registrations, but exclude live kernel delivery from deterministic tests.
            let watcher = state.watcher_handle().unwrap();
            for watched in watcher.watched_paths().unwrap() {
                watcher.unwatch(&watched).unwrap();
            }
            let dir = fixture.team_root.join("docs");
            let path = dir.join(format!("{configuration_first}-{restart}-{shape}.md"));
            std::fs::write(&path, shape).unwrap();
            // A git-like batch changes several children behind one parent event.
            let sibling = path.with_extension("txt");
            std::fs::write(&sibling, "batch sibling").unwrap();
            let temporary = dir.join(".atomic.tmp");
            std::fs::write(&temporary, "atomic replacement").unwrap();
            std::fs::rename(&temporary, &path).unwrap();
            let deleted = dir.join("a.md");
            let _ = std::fs::remove_file(&deleted);
            let hub = fixture.app.state::<super::super::LiveHub>();
            hub.model.lock().unwrap().files.pending.clear();
            let event_path = match shape {
                "file" => path.clone(),
                "directory" | "scoped-rescan" => dir.clone(),
                "ancestor-rescan" => fixture.org_root.clone(),
                _ => fixture.team_root.clone(),
            };
            // The dispatcher deliberately retains its debounce map across restart.
            if matches!(shape, "directory" | "root") {
                std::thread::sleep(std::time::Duration::from_millis(60));
            }
            if shape == "activation" {
                register_team_activation_watches(
                    fixture.app.handle(),
                    &state,
                    &fixture.org_root,
                    ORG,
                    TEAM,
                );
                // Activation reasserts the actual native watch; detach it again before checks.
                let watcher = state.watcher_handle().unwrap();
                for watched in watcher.watched_paths().unwrap() {
                    watcher.unwatch(&watched).unwrap();
                }
            } else {
                let mut event = Event::new(EventKind::Modify(ModifyKind::Any));
                if shape != "global-rescan" {
                    event = event.add_path(event_path);
                }
                if shape == "file" {
                    event = event.add_path(sibling.clone()).add_path(deleted.clone());
                }
                if shape.ends_with("rescan") {
                    event = event.set_flag(Flag::Rescan);
                }
                route_test_event(&state.dispatcher, event);
            }
            assert!(!hub.model.lock().unwrap().files.pending.is_empty(), "{shape} configuration_first={configuration_first} restart={restart}: no hub invalidation");
            refresh_with(fixture.app.handle(), |_| async { unreachable!() })
                .await
                .unwrap();
            let snapshot = hub.snapshot(&key, "files").unwrap();
            for expected in [&path, &sibling] {
                assert!(
                    snapshot
                        .snapshot
                        .iter()
                        .any(|r| r["path"] == expected.to_str().unwrap()),
                    "{shape}: listing did not recover {}",
                    expected.display()
                );
            }
            assert!(
                !snapshot
                    .snapshot
                    .iter()
                    .any(|r| r["path"] == deleted.to_str().unwrap()),
                "{shape}: deleted row remained"
            );
            assert_eq!(
                snapshot
                    .snapshot
                    .iter()
                    .find(|r| r["path"] == path.to_str().unwrap())
                    .unwrap()["size"],
                "atomic replacement".len()
            );
            route_test_event(
                &state.dispatcher,
                Event::new(EventKind::Modify(ModifyKind::Any))
                    .add_path(fixture.org_root.join("unrelated/content"))
                    .set_flag(Flag::Rescan),
            );
            assert!(
                hub.model.lock().unwrap().files.pending.is_empty(),
                "unrelated team invalidated this listing"
            );
        }
    }
}
#[tokio::test]
async fn pbf211_registered_file_batch() {
    registered_recovery_route("file").await;
}
#[tokio::test]
async fn pbf211_registered_directory_batch() {
    registered_recovery_route("directory").await;
}
#[tokio::test]
async fn pbf211_registered_root_batch() {
    registered_recovery_route("root").await;
}
#[tokio::test]
async fn pbf211_registered_scoped_rescan_batch() {
    registered_recovery_route("scoped-rescan").await;
}
#[tokio::test]
async fn pbf211_registered_ancestor_rescan_batch() {
    registered_recovery_route("ancestor-rescan").await;
}
#[tokio::test]
async fn pbf211_registered_global_rescan_batch() {
    registered_recovery_route("global-rescan").await;
}
#[tokio::test]
async fn pbf211_registered_activation_batch() {
    registered_recovery_route("activation").await;
}

#[tokio::test]
async fn pbf211_parent_ancestor_rescan_recovers_an_inherited_listing() {
    use crate::commands::watch::{
        register_team_activation_watches, start_watching_impl, WatcherState,
    };
    use crate::content_engine::watcher::route_test_event;
    use crate::routines::test_support::TEAM;
    use notify::{
        event::{Flag, ModifyKind},
        Event, EventKind,
    };
    let (fixture, key, changed) = prepared_fixture("docs", true).await;
    start_watching_impl(fixture.app.handle().clone(), vec![], 0).unwrap();
    let state = fixture.app.state::<WatcherState>();
    register_team_activation_watches(fixture.app.handle(), &state, &fixture.org_root, ORG, TEAM);
    let watcher = state.watcher_handle().unwrap();
    for root in watcher.watched_paths().unwrap() {
        watcher.unwatch(&root).unwrap();
    }
    let hub = fixture.app.state::<super::super::LiveHub>();
    hub.model.lock().unwrap().files.pending.clear();
    std::fs::write(&changed, "inherited edit while no event was delivered").unwrap();
    // The parent has no workspace subscriber; only the child view knows this inherited root.
    route_test_event(
        &state.dispatcher,
        Event::new(EventKind::Modify(ModifyKind::Any))
            .add_path(fixture.org_root.join("platform").canonicalize().unwrap())
            .set_flag(Flag::Rescan),
    );
    assert!(
        !hub.model.lock().unwrap().files.pending.is_empty(),
        "inherited view was not invalidated"
    );
    refresh_with(fixture.app.handle(), |_| async { unreachable!() })
        .await
        .unwrap();
    let snapshot = hub.snapshot(&key, "files").unwrap();
    assert_eq!(
        snapshot
            .snapshot
            .iter()
            .find(|row| row["path"] == changed.to_str().unwrap())
            .unwrap()["size"],
        "inherited edit while no event was delivered".len()
    );
    route_test_event(
        &state.dispatcher,
        Event::new(EventKind::Modify(ModifyKind::Any))
            .add_path(fixture.org_root.join("unrelated"))
            .set_flag(Flag::Rescan),
    );
    assert!(
        hub.model.lock().unwrap().files.pending.is_empty(),
        "unrelated team refreshed inherited listing"
    );
}

async fn recovery_payload_contract(kind: &str, route: &str, with_files_view: bool) {
    let delete_org = route.starts_with("deleted-org-");
    let deleting = route.starts_with("deleted-");
    let route = route
        .strip_prefix(if delete_org {
            "deleted-org-"
        } else {
            "deleted-"
        })
        .unwrap_or(route);
    use crate::commands::watch::{
        register_team_activation_watches, start_watching_impl, WatcherState,
    };
    use crate::content_engine::watcher::route_test_event;
    use crate::routines::test_support::{ORG, TEAM};
    use notify::{
        event::{Flag, ModifyKind},
        Event, EventKind,
    };
    use std::sync::{Arc, Mutex};
    use tauri::Listener;
    let fixture = if with_files_view {
        prepared_fixture("docs", true).await.0
    } else {
        content_fixture(true).0
    };
    if !with_files_view {
        assert!(fixture
            .app
            .state::<super::super::LiveHub>()
            .model
            .lock()
            .unwrap()
            .files
            .views
            .is_empty());
    }
    let home = fixture.org_root.ancestors().nth(5).unwrap();
    let private = crate::context_repo::scratch_space::org_space_at(
        home,
        &crate::domain::Principal::from(1u64),
        ORG,
    )
    .unwrap()
    .content_dir();
    if kind == "private" {
        std::fs::create_dir_all(&private).unwrap();
        std::fs::write(private.join("a.md"), "private before").unwrap();
        if with_files_view {
            let frame = if route.ends_with("activation") {
                fixture.frame("file-nav", Some(TEAM), "open")
            } else {
                fixture.frame("private-nav", None, "private")
            };
            file_surface_prepare_view(
                fixture.app.handle().clone(),
                frame.frame_id,
                ListFiles {
                    scope: Some("private".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        }
    }
    let state = fixture.app.state::<WatcherState>();
    if route == "deferred-activation" {
        crate::commands::scratch_watch::register_scratch_watch(
            fixture.app.handle(),
            &state,
            home,
            1,
        );
    } else {
        start_watching_impl(fixture.app.handle().clone(), vec![], 0).unwrap();
    }
    register_team_activation_watches(fixture.app.handle(), &state, &fixture.org_root, ORG, TEAM);
    if kind == "private" && route != "deferred-activation" {
        crate::commands::scratch_watch::register_scratch_watch(
            fixture.app.handle(),
            &state,
            home,
            1,
        );
    }
    let watcher = state.watcher_handle();
    if let Some(watcher) = &watcher {
        for root in watcher.watched_paths().unwrap() {
            watcher.unwatch(&root).unwrap();
        }
    }
    let emitted = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = emitted.clone();
    fixture.app.listen_any("context-file-change", move |event| {
        captured
            .lock()
            .unwrap()
            .push(serde_json::from_str(event.payload()).unwrap());
    });
    let physical = if kind == "private" {
        private.clone()
    } else {
        fixture.org_root.join("platform/content")
    };
    std::fs::write(
        physical.join(if kind == "private" {
            "a.md"
        } else {
            "docs/a.md"
        }),
        "after recovery gap",
    )
    .unwrap();
    if kind == "inherited" && route.ends_with("activation") {
        std::fs::write(
            fixture.team_root.join("docs/own.md"),
            "own write while inactive",
        )
        .unwrap();
    }
    if deleting {
        std::fs::remove_dir_all(if delete_org {
            physical.parent().unwrap()
        } else {
            &physical
        })
        .unwrap();
    }
    if route.ends_with("activation") {
        // Watches register before the ambient active-team snapshot necessarily catches up.
        fixture
            .app
            .state::<crate::mcp::state::ActiveTeamState>()
            .clear();
    }
    if kind == "private" && route.ends_with("activation") {
        let other = crate::context_repo::scratch_space::org_space_at(
            home,
            &crate::domain::Principal::from(1u64),
            "acme/unrelated",
        )
        .unwrap()
        .content_dir();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("a.md"), "unrelated private content").unwrap();
        register_team_activation_watches(
            fixture.app.handle(),
            &state,
            &fixture.org_root.parent().unwrap().join("unrelated"),
            "acme/unrelated",
            TEAM,
        );
        assert!(
            emitted
                .lock()
                .unwrap()
                .iter()
                .all(|payload| payload["file_path"] != private.to_str().unwrap()),
            "another org activated private files"
        );
        emitted.lock().unwrap().clear();
        if let Some(watcher) = &watcher {
            for root in watcher.watched_paths().unwrap() {
                watcher.unwatch(&root).unwrap();
            }
        }
    }
    match route {
        "activation" | "deferred-activation" => register_team_activation_watches(
            fixture.app.handle(),
            &state,
            &fixture.org_root,
            ORG,
            TEAM,
        ),
        "global" => route_test_event(
            &state.dispatcher,
            Event::new(EventKind::Modify(ModifyKind::Any)).set_flag(Flag::Rescan),
        ),
        _ => route_test_event(
            &state.dispatcher,
            Event::new(EventKind::Modify(ModifyKind::Any))
                .add_path(if kind == "private" {
                    crate::context_repo::scratch_space::scratch_root(
                        home,
                        &crate::domain::Principal::from(1u64),
                    )
                } else {
                    physical
                })
                .set_flag(Flag::Rescan),
        ),
    }
    let contracts: Value = serde_json::from_str(include_str!(
        "../../../src/shared/lib/__test-fixtures__/recovery-payloads.json"
    ))
    .unwrap();
    let actual: Vec<Value> = emitted
        .lock()
        .unwrap()
        .iter()
        .map(|payload| {
            let mut normalized = payload.clone();
            for field in [
                "file_path",
                "repo_path",
                "context_repo_path",
                "inherited_by",
            ] {
                if let Some(value) = payload[field].as_str() {
                    normalized[field] = json!(value
                        .replace(
                            private.to_str().unwrap(),
                            "/fixture/users/1/private/orgs/acme/app/content"
                        )
                        .replace(fixture.org_root.to_str().unwrap(), "/fixture/org"));
                }
            }
            normalized
        })
        .collect();
    assert!(
        actual.contains(&contracts[kind]),
        "{kind} {route} payload missing: expected {}, emitted {actual:?}",
        contracts[kind]
    );
    if kind == "inherited" && route.ends_with("activation") {
        let own = json!({"repo_path":"", "context_repo_path":"", "file_path":contracts["inherited"]["inherited_by"]});
        assert!(
            actual.contains(&own),
            "{route} own root missing: expected {own}, emitted {actual:?}"
        );
    }
}

#[tokio::test]
async fn pbf211_inherited_recovery_payload_scoped() {
    recovery_payload_contract("inherited", "scoped", true).await;
}
#[tokio::test]
async fn pbf211_inherited_recovery_payload_global() {
    recovery_payload_contract("inherited", "global", true).await;
}
#[tokio::test]
async fn pbf211_inherited_recovery_payload_activation() {
    recovery_payload_contract("inherited", "activation", true).await;
}
#[tokio::test]
async fn pbf211_private_recovery_payload_scoped() {
    recovery_payload_contract("private", "scoped", true).await;
}
#[tokio::test]
async fn pbf211_private_recovery_payload_global() {
    recovery_payload_contract("private", "global", true).await;
}

#[tokio::test]
async fn pbf211_private_recovery_payload_activation() {
    recovery_payload_contract("private", "activation", true).await;
}

#[tokio::test]
async fn pbf211_private_recovery_payload_native_activation_without_file_view() {
    recovery_payload_contract("private", "activation", false).await;
}
#[tokio::test]
async fn pbf211_private_recovery_payload_native_scoped_without_file_view() {
    recovery_payload_contract("private", "scoped", false).await;
}
#[tokio::test]
async fn pbf211_private_recovery_payload_native_global_without_file_view() {
    recovery_payload_contract("private", "global", false).await;
}

#[tokio::test]
async fn pbf211_private_recovery_payload_native_deferred_activation_without_file_view() {
    recovery_payload_contract("private", "deferred-activation", false).await;
}

#[tokio::test]
async fn pbf211_private_recovery_payload_deleted_content_activation_without_file_view() {
    recovery_payload_contract("private", "deleted-activation", false).await;
}
#[tokio::test]
async fn pbf211_private_recovery_payload_deleted_content_scoped_without_file_view() {
    recovery_payload_contract("private", "deleted-scoped", false).await;
}
#[tokio::test]
async fn pbf211_private_recovery_payload_deleted_content_global_without_file_view() {
    recovery_payload_contract("private", "deleted-global", false).await;
}
#[tokio::test]
async fn pbf211_private_recovery_payload_deleted_org_activation_without_file_view() {
    recovery_payload_contract("private", "deleted-org-activation", false).await;
}

#[tokio::test]
async fn pbf211_inherited_recovery_payload_native_scoped_without_file_view() {
    recovery_payload_contract("inherited", "scoped", false).await;
}
#[tokio::test]
async fn pbf211_inherited_recovery_payload_native_global_without_file_view() {
    recovery_payload_contract("inherited", "global", false).await;
}
#[tokio::test]
async fn pbf211_inherited_recovery_payload_native_activation_without_file_view() {
    recovery_payload_contract("inherited", "activation", false).await;
}
#[tokio::test]
async fn pbf211_inherited_recovery_payload_native_deferred_activation_without_file_view() {
    recovery_payload_contract("inherited", "deferred-activation", false).await;
}

#[tokio::test]
async fn pbf211_private_recovery_payload_deleted_org_scoped_without_file_view() {
    recovery_payload_contract("private", "deleted-org-scoped", false).await;
}
#[tokio::test]
async fn pbf211_private_recovery_payload_deleted_org_global_without_file_view() {
    recovery_payload_contract("private", "deleted-org-global", false).await;
}
