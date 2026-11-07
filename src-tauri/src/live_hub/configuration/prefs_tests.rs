// SPDX-License-Identifier: MIT
use super::*;
use serde_json::json;
use tauri::{Listener, Manager};

#[test]
fn managed_prefs_writers_file_ingress_and_principal_boundaries_feed_live_consumers() {
    let home = tempfile::tempdir().unwrap();
    for principal in ["42", "43"] {
        std::fs::create_dir_all(home.path().join("users").join(principal)).unwrap();
    }
    let profile = crate::commands::local_profile::LocalProfileState::load(home.path()).unwrap();
    profile.select("42", None).unwrap();
    let app = tauri::test::mock_builder()
        .manage(profile)
        .manage(crate::commands::watch::WatcherState::default())
        .manage(super::super::LiveHub::default())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let data = home.path().join("app-data");
    std::fs::create_dir_all(&data).unwrap();
    let path = data.join("app-state.json");
    let write = |principal, patch| {
        crate::commands::system::write_app_state(
            app.handle(),
            &path,
            Some(principal),
            &serde_json::to_string(&patch).unwrap(),
            true,
        )
        .unwrap()
    };
    write(
        "42",
        json!({"viewer":{"settings":{"editorLineWrap":"off"},"activeWorkspace":{"acme:dev":"first"},"workspaces":{"acme:dev":[{"id":"first","kind":"story","cwd":"/first","status":"ready"}]}},"shortcuts":{"version":2,"shortcuts":{}},"dashboardPolicy":{"preset":"open"}}),
    );
    let state = app.state::<crate::commands::watch::WatcherState>();
    let watcher = std::sync::Arc::new(
        crate::content_engine::Watcher::new(
            crate::content_engine::WatchSetup { dirs: vec![] },
            state.dispatcher.clone(),
        )
        .unwrap(),
    );
    *state.watcher.lock().unwrap() = Some(watcher.clone());
    crate::commands::scratch_watch::register_configuration_file_watch(app.handle(), &state, &path);
    watcher.unwatch(&data).unwrap();
    let principal = Principal::from(42u64);
    let global = prepare(app.handle(), &data, &principal, "prefs", Some("global")).unwrap();
    let owned = prepare(app.handle(), &data, &principal, "prefs", Some("principal")).unwrap();
    let initial = json!([global, owned]);
    let recorded = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    for channel in ["codesense:delta", "codesense:subscription-delta"] {
        let capture = recorded.clone();
        app.listen(channel, move |event| capture.lock().unwrap().push(json!({"event":channel,"payload":serde_json::from_str::<Value>(event.payload()).unwrap()})));
    }
    let global_id = "native:prefs:global:[\"prefs\",\"prefs:global\"]";
    let owned_id = "native:prefs:principal:[\"prefs\",\"prefs:42:native\"]";
    super::super::subscribe(
        app.handle(),
        "prefs",
        &global.view_key,
        None,
        Some(global_id),
        None,
    )
    .unwrap();
    super::super::subscribe(
        app.handle(),
        "prefs",
        &owned.view_key,
        None,
        Some(owned_id),
        None,
    )
    .unwrap();
    write("42", json!({"viewer":{"settings":{"editorLineWrap":"on"}}}));
    let mut outside: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    outside["viewer"]["settings"]["sidebarWidth"] = json!(333);
    outside["shortcuts"]["shortcuts"]["settings.open"] = json!("CmdOrCtrl+Shift+Y");
    outside["localProfileWorkspacesV1"]["42"]["dashboardPolicy"] =
        json!({"preset":"team-safe","switches":{"confirmTeamSendAsYou":true}});
    std::fs::write(&path, serde_json::to_vec(&outside).unwrap()).unwrap();
    state.dispatcher.dispatch(&path);
    write(
        "42",
        json!({"viewer":{"settings":{"editorLineWrap":"on"},"activeWorkspace":{"acme:dev":"second"},"workspaces":{"acme:dev":[{"id":"second","kind":"story","cwd":"/second","status":"ready"}]}},"dashboardPolicy":{"switches":{"confirmMessageDestructive":true}}}),
    );
    let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved["viewer"]["settings"]["sidebarWidth"], 333);
    assert_eq!(
        saved["localProfileWorkspacesV1"]["42"]["dashboardPolicy"]["switches"]
            ["confirmTeamSendAsYou"],
        true
    );
    for principal in ["42", "43"] {
        assert!(saved["localProfileWorkspacesV1"][principal]["viewer"]
            .get("settings")
            .is_none());
        assert!(saved["localProfileWorkspacesV1"][principal]
            .get("shortcuts")
            .is_none());
    }
    let hub = app.state::<super::super::LiveHub>();
    let final_views = json!([
        hub.snapshot(&global.view_key, "prefs").unwrap(),
        hub.snapshot(&owned.view_key, "prefs").unwrap()
    ]);
    let events = recorded.lock().unwrap().clone();
    for index in 0..=super::super::projection::RETAINED_DELTAS {
        write(
            "42",
            json!({"viewer":{"settings":{"editorLineWrap":if index % 2 == 0 {"off"} else {"on"}}}}),
        );
    }
    let recovered = super::super::subscribe(
        app.handle(),
        "prefs",
        &global.view_key,
        Some(0),
        Some(global_id),
        None,
    )
    .unwrap();
    assert!(
        recovered.snapshot.is_some(),
        "revision gap must recover from a snapshot"
    );
    let gap = json!({"view":hub.snapshot(&global.view_key,"prefs").unwrap(),"events":recorded.lock().unwrap().iter().rev().take(2).cloned().collect::<Vec<_>>()});
    super::super::unsubscribe(app.handle(), global_id, false);
    super::super::unsubscribe(app.handle(), owned_id, false);
    super::tests::SOURCE_READS.with(|reads| reads.set(0));
    super::tests::VIEW_BUILDS.with(|builds| builds.set(0));
    write("42", json!({"viewer":{"settings":{"editorLineWrap":"on"}}}));
    state.dispatcher.dispatch(&path);
    assert_eq!(super::tests::SOURCE_READS.with(|reads| reads.get()), 0);
    assert_eq!(super::tests::VIEW_BUILDS.with(|builds| builds.get()), 0);
    super::super::subscribe(
        app.handle(),
        "prefs",
        &global.view_key,
        Some(0),
        Some(global_id),
        None,
    )
    .unwrap();
    super::super::subscribe(
        app.handle(),
        "prefs",
        &owned.view_key,
        Some(0),
        Some(owned_id),
        None,
    )
    .unwrap();
    assert_eq!(
        super::tests::SOURCE_READS.with(|reads| reads.get()),
        1,
        "one file read fans out to both prefs scopes on reveal"
    );
    app.state::<crate::commands::local_profile::LocalProfileState>()
        .select("43", None)
        .unwrap();
    write(
        "43",
        json!({"dashboardPolicy":{"preset":"open"},"viewer":{"activeWorkspace":{"acme:dev":"other"},"workspaces":{"acme:dev":[{"id":"other","kind":"story","cwd":"/other","status":"ready"}]}}}),
    );
    assert!(crate::commands::system::write_app_state(
        app.handle(),
        &path,
        Some("42"),
        r#"{"viewer":{"settings":{"editorLineWrap":"off"}}}"#,
        true
    )
    .is_err());
    let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for principal in ["42", "43"] {
        assert!(saved["localProfileWorkspacesV1"][principal]["viewer"]
            .get("settings")
            .is_none());
        assert!(saved["localProfileWorkspacesV1"][principal]
            .get("shortcuts")
            .is_none());
    }
    let other = prepare(
        app.handle(),
        &data,
        &Principal::from(43u64),
        "prefs",
        Some("principal"),
    )
    .unwrap();
    assert!(!serde_json::to_string(&other.snapshot)
        .unwrap()
        .contains("second"));
    assert!(hub
        .snapshot(&owned.view_key, "prefs")
        .unwrap()
        .snapshot
        .is_empty());
    let trace =
        json!({"initial":initial,"deltas":events,"final":final_views,"gap":gap,"other":other});
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../src/features/dashboard/lib/prefs-writer-trace.json");
    if std::env::var_os("UPDATE_PREFS_TRACE").is_some() {
        std::fs::write(
            &fixture,
            serde_json::to_string_pretty(&trace).unwrap() + "\n",
        )
        .unwrap();
    } else {
        let expected: Value = serde_json::from_slice(&std::fs::read(fixture).unwrap()).unwrap();
        assert_eq!(
            trace, expected,
            "mounted consumers must use real writer/ingress frames"
        );
    }
}

#[test]
fn prefs_ingress_waits_for_the_managed_writer_gate_before_observing_disk() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("users/42")).unwrap();
    let profile = crate::commands::local_profile::LocalProfileState::load(home.path()).unwrap();
    profile.select("42", None).unwrap();
    let app = tauri::test::mock_builder()
        .manage(profile)
        .manage(super::super::LiveHub::default())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let path = home.path().join("app-state.json");
    std::fs::write(&path, r#"{"viewer":{"settings":{"editorLineWrap":"on"}}}"#).unwrap();
    let global = prepare(
        app.handle(),
        home.path(),
        &Principal::from(42u64),
        "prefs",
        Some("global"),
    )
    .unwrap();
    super::super::subscribe(
        app.handle(),
        "prefs",
        &global.view_key,
        None,
        Some("native:ingress-gate"),
        None,
    )
    .unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let handle = app.handle().clone();
    let task = app
        .state::<crate::commands::local_profile::LocalProfileState>()
        .with_expected(Some("42"), || {
            let task = std::thread::spawn(move || {
                started_tx.send(()).unwrap();
                let result = path_changed(&handle, &path);
                done_tx.send(result).unwrap();
            });
            started_rx.recv().unwrap();
            let completed = done_rx.recv_timeout(std::time::Duration::from_millis(250));
            Ok((task, completed))
        })
        .unwrap();
    task.0.join().unwrap();
    assert!(
        matches!(task.1, Err(std::sync::mpsc::RecvTimeoutError::Timeout)),
        "file ingress must wait until the managed writer has published its committed value"
    );
    done_rx.recv().unwrap().unwrap();
}

#[test]
fn shortcut_reset_removes_the_disk_key_and_committed_row_without_losing_another_binding() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("users/42")).unwrap();
    let profile = crate::commands::local_profile::LocalProfileState::load(home.path()).unwrap();
    profile.select("42", None).unwrap();
    let app = tauri::test::mock_builder()
        .manage(profile)
        .manage(super::super::LiveHub::default())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let path = home.path().join("app-state.json");
    let write = |value: Value| {
        crate::commands::system::write_app_state(
            app.handle(),
            &path,
            Some("42"),
            &value.to_string(),
            true,
        )
        .unwrap()
    };
    write(
        json!({"shortcuts":{"version":2,"shortcuts":{"settings.open":"CmdOrCtrl+Shift+Y","nav.open-workspace":"CmdOrCtrl+Alt+9"}},"viewer":{"activeWorkspace":{"acme:dev":"first"}}}),
    );
    let global = prepare(
        app.handle(),
        home.path(),
        &Principal::from(42u64),
        "prefs",
        Some("global"),
    )
    .unwrap();
    super::super::subscribe(
        app.handle(),
        "prefs",
        &global.view_key,
        None,
        Some("native:reset"),
        None,
    )
    .unwrap();
    write(json!({"shortcuts":{"shortcuts":{"settings.open":null}}}));
    let saved: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(saved["shortcuts"]["shortcuts"]
        .get("settings.open")
        .is_none());
    assert_eq!(
        saved["shortcuts"]["shortcuts"]["nav.open-workspace"],
        "CmdOrCtrl+Alt+9"
    );
    assert_eq!(
        saved["localProfileWorkspacesV1"]["42"]["viewer"]["activeWorkspace"]["acme:dev"],
        "first"
    );
    let snapshot = app
        .state::<super::super::LiveHub>()
        .snapshot(&global.view_key, "prefs")
        .unwrap();
    assert!(!snapshot
        .snapshot
        .iter()
        .any(|row| row["path"] == json!(["shortcuts", "shortcuts", "settings.open"])));
    assert!(snapshot
        .snapshot
        .iter()
        .any(|row| row["value"] == "CmdOrCtrl+Alt+9"));
}
