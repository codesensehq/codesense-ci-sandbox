// SPDX-License-Identifier: MIT
use super::*;
use serde_json::json;
use tauri::Manager;

#[test]
fn prefs_legacy_owner_is_initialized_before_the_first_owned_snapshot() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("app-state.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&json!({
            "viewer":{"settings":{"editorLineWrap":"on"},"openedTeams":["acme:dev"]},
            "shortcuts":{"version":2,"shortcuts":{"settings.open":"CmdOrCtrl+Shift+Y"}},
            "dashboardPolicy":{"preset":"team-safe"}
        }))
        .unwrap(),
    )
    .unwrap();
    let mut registry = Registry::default();
    let mut topics = TopicRegistry::default();
    let principal = Principal::from(42u64);
    open(
        &mut registry,
        &mut topics,
        home.path(),
        &principal,
        "prefs",
        Some("global"),
    )
    .unwrap();
    let (owned, _) = open(
        &mut registry,
        &mut topics,
        home.path(),
        &principal,
        "prefs",
        Some("principal"),
    )
    .unwrap();
    let owned = serde_json::to_string(&owned.snapshot).unwrap();
    assert!(owned.contains("team-safe"));
    assert!(owned.contains("openedTeams"));
    assert!(!owned.contains("editorLineWrap"));
    assert!(!owned.contains("settings.open"));
    let (other, _) = open(
        &mut registry,
        &mut topics,
        home.path(),
        &Principal::from(43u64),
        "prefs",
        Some("principal"),
    )
    .unwrap();
    assert!(other.snapshot.is_empty());
}

#[test]
fn prefs_views_keep_global_and_principal_storage_separate() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("app-state.json");
    std::fs::write(&path, serde_json::to_vec(&json!({
        "viewer":{"settings":{"editorLineWrap":"on"}},
        "shortcuts":{"version":4,"shortcuts":{"settings.open":"CmdOrCtrl+Y"}},
        "localProfileWorkspacesV1":{
            "42":{"dashboardPolicy":{"preset":"team-safe"},"viewer":{"activeWorkspace":{"acme:dev":"secret"}}},
            "43":{"dashboardPolicy":{"preset":"open"},"viewer":{"activeWorkspace":{"acme:dev":"other"}}}
        }
    })).unwrap()).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let mut registry = Registry::default();
    let mut topics = TopicRegistry::default();
    for principal in [42u64, 43] {
        let principal = Principal::from(principal);
        let (global, _) = open(
            &mut registry,
            &mut topics,
            home.path(),
            &principal,
            "prefs",
            Some("global"),
        )
        .unwrap();
        let global = serde_json::to_string(&global.snapshot).unwrap();
        assert!(global.contains("editorLineWrap"));
        assert!(global.contains("settings.open"));
        assert!(!global.contains("dashboardPolicy"));
        assert!(!global.contains("activeWorkspace"));
        let (owned, _) = open(
            &mut registry,
            &mut topics,
            home.path(),
            &principal,
            "prefs",
            Some("principal"),
        )
        .unwrap();
        let owned = serde_json::to_string(&owned.snapshot).unwrap();
        assert!(!owned.contains("editorLineWrap"));
        assert!(!owned.contains("settings.open"));
        assert!(owned.contains(if principal.as_str() == "42" {
            "secret"
        } else {
            "other"
        }));
        assert!(!owned.contains(if principal.as_str() == "42" {
            "other"
        } else {
            "secret"
        }));
    }
    assert_eq!(
        std::fs::read(path).unwrap(),
        bytes,
        "view preparation must never migrate global prefs into a profile"
    );
}

#[test]
fn real_writers_and_scratch_ingress_feed_owned_versioned_views() {
    let home = tempfile::tempdir().unwrap();
    let app = tauri::test::mock_builder()
        .manage(crate::commands::watch::WatcherState::default())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(super::super::LiveHub::default());
    let principal = Principal::from(42u64);
    let org = "acme/repo";
    let state = app.state::<crate::commands::watch::WatcherState>();
    let watcher = std::sync::Arc::new(
        crate::content_engine::Watcher::new(
            crate::content_engine::WatchSetup { dirs: vec![] },
            state.dispatcher.clone(),
        )
        .unwrap(),
    );
    *state.watcher.lock().unwrap() = Some(watcher.clone());
    crate::commands::scratch_watch::register_scratch_watch(app.handle(), &state, home.path(), 42);
    // Exercise the actual registered dispatcher handlers without asynchronous kernel echoes.
    watcher.unwatch(home.path()).unwrap();
    watcher
        .unwatch(&crate::context_repo::scratch_space::scratch_root(
            home.path(),
            &principal,
        ))
        .unwrap();

    let initial = prepare(app.handle(), home.path(), &principal, "profile", Some(org)).unwrap();
    assert!(initial.snapshot.is_empty());
    let key = initial.view_key.clone();
    let workspace = prepare(
        app.handle(),
        home.path(),
        &principal,
        "workspaceConfig",
        None,
    )
    .unwrap();
    let initial_views = json!([initial, workspace]);
    let recorded = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    use tauri::Listener;
    for channel in ["codesense:delta", "codesense:subscription-delta"] {
        let capture = recorded.clone();
        app.listen(channel, move |event| capture.lock().unwrap().push(json!({"event":channel,"payload":serde_json::from_str::<Value>(event.payload()).unwrap()})));
    }
    super::super::subscribe(
        app.handle(),
        "profile",
        &key,
        None,
        Some("native:configuration:profile:[\"profile\",\"profile:42:acme/repo\"]"),
        None,
    )
    .unwrap();
    crate::commands::panes::write_profile_patch(
        app.handle(),
        home.path(),
        &principal,
        org,
        json!({"slots":{"dock":"native"}}),
        false,
    )
    .unwrap();
    let after = app
        .state::<super::super::LiveHub>()
        .snapshot(&key, "profile")
        .unwrap();
    assert!(after.rev > initial.rev);
    assert!(after
        .snapshot
        .iter()
        .any(|row| row["field"] == "slots" && row["key"] == "dock" && row["value"] == "native"));
    let path = profile_path(home.path(), &principal, org).unwrap();
    let mut disk: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    disk["name"] = json!("Newer name");
    disk["chrome"] = json!({"newer":true});
    std::fs::write(&path, serde_json::to_vec(&disk).unwrap()).unwrap();
    crate::commands::panes::write_profile_patch(
        app.handle(),
        home.path(),
        &principal,
        org,
        json!({"slots":{"file-nav":"native"}}),
        false,
    )
    .unwrap();
    let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved["name"], "Newer name");
    assert_eq!(saved["chrome"]["newer"], true);
    assert_eq!(saved["slots"]["dock"], "native");
    disk = saved;
    disk["slots"]["dock"] = json!({"scope":"built-in","org":org,"id":"agents"});
    std::fs::write(&path, serde_json::to_vec(&disk).unwrap()).unwrap();
    state.dispatcher.dispatch(&path);
    let changed = app
        .state::<super::super::LiveHub>()
        .snapshot(&key, "profile")
        .unwrap();
    assert!(changed
        .snapshot
        .iter()
        .any(|row| row["key"] == "dock" && row["value"]["id"] == "agents"));
    let reply = super::super::subscribe(
        app.handle(),
        "profile",
        &key,
        Some(after.rev),
        Some("native:configuration:profile:[\"profile\",\"profile:42:acme/repo\"]"),
        None,
    )
    .unwrap();
    assert!(!reply.deltas.unwrap().is_empty());

    super::super::subscribe(app.handle(), "workspaceConfig", &workspace.view_key, None, Some("native:configuration:workspaceConfig:[\"workspaceConfig\",\"workspaceConfig:42:native\"]"), None).unwrap();
    crate::commands::workspace_config::save_patch(
        app.handle(),
        home.path(),
        json!({"repos":{"team:acme:dev":{"mainDoc":"Epic.md"}}}),
    )
    .unwrap();
    let config_path = home.path().join("workspace-config.json");
    let mut disk: Value = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    disk["repos"]["team:acme:dev"]["newerField"] = json!(true);
    disk["repos"]["other"] = json!({"mainDoc":"Newer.md", "futureField":true});
    std::fs::write(&config_path, serde_json::to_vec(&disk).unwrap()).unwrap();
    crate::commands::workspace_config::save_patch(
        app.handle(),
        home.path(),
        json!({"repos":{"team:acme:dev":{"mainDoc":"Team.md"}}}),
    )
    .unwrap();
    let saved: Value = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    assert_eq!(saved["repos"]["team:acme:dev"]["newerField"], true);
    assert_eq!(saved["repos"]["other"]["mainDoc"], "Newer.md");
    assert_eq!(saved["repos"]["other"]["futureField"], true);
    disk = saved;
    disk["repos"]["team:acme:dev"]["mainDoc"] = json!("Outside.md");
    std::fs::write(&config_path, serde_json::to_vec(&disk).unwrap()).unwrap();
    state.dispatcher.dispatch(&config_path);
    let changed = app
        .state::<super::super::LiveHub>()
        .snapshot(&workspace.view_key, "workspaceConfig")
        .unwrap();
    assert!(changed
        .snapshot
        .iter()
        .any(|row| row["id"] == "team:acme:dev" && row["value"]["mainDoc"] == "Outside.md"));

    let trace = json!({"initial":initial_views,"deltas":recorded.lock().unwrap().clone(),"final":[changed, app.state::<super::super::LiveHub>().snapshot(&key, "profile").unwrap()]});
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../src/features/dashboard/lib/configuration-writer-trace.json");
    if std::env::var_os("UPDATE_CONFIGURATION_TRACE").is_some() {
        std::fs::write(
            &fixture,
            serde_json::to_string_pretty(&trace).unwrap() + "\n",
        )
        .unwrap();
    } else {
        let expected: Value = serde_json::from_slice(&std::fs::read(&fixture).unwrap()).unwrap();
        assert_eq!(
            trace, expected,
            "configuration consumer trace must match real writers and file ingress"
        );
    }

    for index in 0..=crate::live_hub::projection::RETAINED_DELTAS {
        crate::commands::workspace_config::save_patch(
            app.handle(),
            home.path(),
            json!({"repos":{"team:acme:dev":{"mainDoc":format!("Gap{index}.md")}}}),
        )
        .unwrap();
    }
    let recovered = super::super::subscribe(app.handle(), "workspaceConfig", &workspace.view_key, Some(0), Some("native:configuration:workspaceConfig:[\"workspaceConfig\",\"workspaceConfig:42:native\"]"), None).unwrap();
    assert!(recovered
        .snapshot
        .unwrap()
        .iter()
        .any(|row| row["id"] == "team:acme:dev" && row["value"]["mainDoc"] == "Gap256.md"));

    let other_org = prepare(
        app.handle(),
        home.path(),
        &principal,
        "profile",
        Some("other/repo"),
    )
    .unwrap();
    assert!(other_org.snapshot.is_empty());
    let other_path = profile_path(home.path(), &principal, "other/repo").unwrap();
    std::fs::create_dir_all(other_path.parent().unwrap()).unwrap();
    let mut outside = crate::panes::profile::empty();
    outside["name"] = json!("Other org");
    std::fs::write(&other_path, serde_json::to_vec(&outside).unwrap()).unwrap();
    state.dispatcher.dispatch(&other_path);
    assert!(
        app.state::<super::super::LiveHub>()
            .snapshot(&other_org.view_key, "profile")
            .unwrap()
            .snapshot
            .is_empty(),
        "an unsubscribed file ingress stays dirty without a source read"
    );
    let resumed = super::super::subscribe(
        app.handle(),
        "profile",
        &other_org.view_key,
        Some(0),
        Some("other-org"),
        None,
    )
    .unwrap();
    assert!(resumed
        .snapshot
        .unwrap()
        .iter()
        .any(|row| row["field"] == "name" && row["value"] == "Other org"));

    assert!(
        prepare(app.handle(), home.path(), &principal, "profile", Some(org))
            .unwrap()
            .rev
            > 0
    );
    let next = prepare(
        app.handle(),
        home.path(),
        &Principal::from(99u64),
        "profile",
        Some(org),
    )
    .unwrap();
    assert!(next.snapshot.is_empty());
    assert!(app
        .state::<super::super::LiveHub>()
        .snapshot(&key, "profile")
        .unwrap()
        .snapshot
        .is_empty());
}

/// Drives `Registry::open` the way the hub does: source reads happen between locked steps.
fn open(
    registry: &mut Registry,
    topics: &mut TopicRegistry,
    home: &Path,
    principal: &Principal,
    topic: &str,
    org: Option<&str>,
) -> Result<(Snapshot, Vec<Delta>), String> {
    loop {
        match registry.open(topics, home, principal, topic, org)? {
            Step::Done(done) => return Ok(done),
            Step::Read(read) => registry.deposit(read.fetch())?,
        }
    }
}

thread_local! {
    pub(super) static SOURCE_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Runs on the reading thread once a source read has finished and before its result is used.
    pub(super) static READ_HOOK: std::cell::RefCell<Option<Box<dyn Fn(&Path)>>> = const { std::cell::RefCell::new(None) };
    pub(super) static VIEW_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[test]
fn paused_profile_does_no_native_work_and_reveals_with_one_source_read() {
    paused_configuration("profile");
}
#[test]
fn paused_workspace_config_does_no_native_work_and_reveals_with_one_source_read() {
    paused_configuration("workspaceConfig");
}
fn paused_configuration(topic: &str) {
    let home = tempfile::tempdir().unwrap();
    let app = tauri::test::mock_builder()
        .manage(crate::commands::watch::WatcherState::default())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(super::super::LiveHub::default());
    let principal = Principal::from(42u64);
    let org = "acme/repo";
    let state = app.state::<crate::commands::watch::WatcherState>();
    let watcher = std::sync::Arc::new(
        crate::content_engine::Watcher::new(
            crate::content_engine::WatchSetup { dirs: vec![] },
            state.dispatcher.clone(),
        )
        .unwrap(),
    );
    *state.watcher.lock().unwrap() = Some(watcher.clone());
    crate::commands::scratch_watch::register_scratch_watch(app.handle(), &state, home.path(), 42);
    watcher.unwatch(home.path()).unwrap();
    watcher
        .unwatch(&crate::context_repo::scratch_space::scratch_root(
            home.path(),
            &principal,
        ))
        .unwrap();
    let initial = prepare(app.handle(), home.path(), &principal, topic, Some(org)).unwrap();
    super::super::subscribe(
        app.handle(),
        topic,
        &initial.view_key,
        Some(initial.rev),
        Some("native:configuration:paused"),
        None,
    )
    .unwrap();
    // Renderer pause releases this scoped subscription through the normal native release path.
    super::super::unsubscribe(app.handle(), "native:configuration:paused", false);
    SOURCE_READS.with(|reads| reads.set(0));
    VIEW_BUILDS.with(|builds| builds.set(0));
    let path = if topic == "profile" {
        profile_path(home.path(), &principal, org).unwrap()
    } else {
        home.path().join("workspace-config.json")
    };
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let outside = if topic == "profile" {
        crate::panes::profile::empty()
    } else {
        json!({"repos":{}})
    };
    std::fs::write(&path, serde_json::to_vec(&outside).unwrap()).unwrap();
    state.dispatcher.dispatch(&path);
    assert_eq!(
        SOURCE_READS.with(|reads| reads.get()),
        0,
        "paused file ingress must not read"
    );
    assert_eq!(
        VIEW_BUILDS.with(|builds| builds.get()),
        0,
        "paused file ingress must not parse/project"
    );
    if topic == "profile" {
        crate::commands::panes::write_profile_patch(
            app.handle(),
            home.path(),
            &principal,
            org,
            json!({"slots":{"dock":"native"}}),
            false,
        )
        .unwrap();
    } else {
        crate::commands::workspace_config::save_patch(
            app.handle(),
            home.path(),
            json!({"repos":{"repo":{"mainDoc":"Latest.md"}}}),
        )
        .unwrap();
    }
    assert_eq!(SOURCE_READS.with(|reads| reads.get()), 0);
    assert_eq!(
        VIEW_BUILDS.with(|builds| builds.get()),
        0,
        "a managed write must not project a paused view"
    );
    assert_eq!(
        app.state::<super::super::LiveHub>()
            .snapshot(&initial.view_key, topic)
            .unwrap()
            .rev,
        initial.rev
    );
    let reply = super::super::subscribe(
        app.handle(),
        topic,
        &initial.view_key,
        Some(initial.rev),
        Some("native:configuration:paused"),
        None,
    )
    .unwrap();
    assert!(reply.rev > initial.rev);
    assert_eq!(
        SOURCE_READS.with(|reads| reads.get()),
        1,
        "reveal reads the latest source once"
    );
    assert_eq!(VIEW_BUILDS.with(|builds| builds.get()), 1);
    super::super::subscribe(
        app.handle(),
        topic,
        &initial.view_key,
        Some(reply.rev),
        Some("native:configuration:paused"),
        None,
    )
    .unwrap();
    assert_eq!(SOURCE_READS.with(|reads| reads.get()), 1);
    let current = app
        .state::<super::super::LiveHub>()
        .snapshot(&initial.view_key, topic)
        .unwrap();
    assert!(current.snapshot.iter().any(|row| if topic == "profile" {
        row["key"] == "dock" && row["value"] == "native"
    } else {
        row["id"] == "repo" && row["value"]["mainDoc"] == "Latest.md"
    }));
}
