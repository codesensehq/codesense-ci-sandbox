// SPDX-License-Identifier: MIT
//! Native watcher diagnosis: use production boot/activation registration and
//! listen to the same context-file-change event consumed by the files hub.
use super::*;
use std::time::{Duration, Instant};
use tauri::Listener;

#[derive(Clone, Copy, Debug)]
enum Boot {
    ContentOnly,
    ConfigurationFirst,
    TeamFirst,
    Deferred,
    DeferredThenReactivated,
}

fn eventually(mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if predicate() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn exercise(boot: Boot) {
    let temporary = tempfile::tempdir().unwrap();
    // Pass this home explicitly instead of mutating process-wide HOME while
    // other Rust tests may be running.
    let home = temporary.path().join(".codesense");
    let org = home.join("users/1/context/acme/ctx");
    let content = org.join("default/content");
    std::fs::create_dir_all(content.join("nested")).unwrap();
    for relative in ["", "nested"] {
        for name in ["replace.md", "rename-source.md", "delete.md", "edit.md"] {
            std::fs::write(content.join(relative).join(name), "# Original\n").unwrap();
        }
    }
    let scratch = crate::context_repo::scratch_space::scratch_root(
        &home,
        &crate::domain::Principal::from(1u64),
    );
    std::fs::create_dir_all(&scratch).unwrap();
    git2::Repository::init(&org).unwrap();
    git2::Repository::init(&scratch).unwrap();
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(WatcherState::default());
    app.manage(crate::live_hub::LiveHub::default());
    let prepare_prefs = || {
        crate::live_hub::configuration::prepare(
            app.handle(),
            &home,
            &crate::domain::Principal::from(1u64),
            "prefs",
            Some("global"),
        )
        .unwrap();
        crate::live_hub::configuration::watch_prefs(app.handle(), false);
    };
    if !matches!(boot, Boot::TeamFirst | Boot::ContentOnly) {
        prepare_prefs();
    }
    app.manage(crate::commands::local_profile::LocalProfileState::for_test(
        &home,
        1,
        crate::commands::local_profile::MachineOwner::LegacyGithub { id: 1 },
    ));
    let events = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
    let received = events.clone();
    app.listen_any("context-file-change", move |event| {
        let payload: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
        if let Some(path) = payload["file_path"].as_str() {
            received.lock().unwrap().push(path.into());
        }
    });
    let state = app.state::<WatcherState>();
    let activate = |team_first: bool| {
        register_worktree_move_watch(app.handle(), &state, &org, "acme/ctx");
        if team_first {
            register_team_activation_watches(app.handle(), &state, &org, "acme/ctx", "default");
            prepare_prefs();
        }
        crate::commands::scratch_watch::register_scratch_watch(app.handle(), &state, &home, 1);
        crate::commands::context_git_watch::register_context_git_watch(
            app.handle(),
            &state,
            &org,
            crate::commands::context_git_watch::GitDirs::resolve(&org),
        );
        if !team_first {
            register_team_activation_watches(app.handle(), &state, &org, "acme/ctx", "default");
        }
    };
    if matches!(boot, Boot::Deferred | Boot::DeferredThenReactivated) {
        activate(false);
    }
    start_watching_impl(app.handle().clone(), vec![], 0).unwrap();
    match boot {
        Boot::ConfigurationFirst | Boot::DeferredThenReactivated => activate(false),
        Boot::TeamFirst => activate(true),
        Boot::ContentOnly => {
            register_team_activation_watches(app.handle(), &state, &org, "acme/ctx", "default")
        }
        Boot::Deferred => {}
    }

    // Wrap the handler activation actually installed; never rebuild it here,
    // which could accidentally repair the registration defect being diagnosed.
    let handler_paths = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
    if state.team_context_dirs.lock().unwrap().contains(&content) {
        let actual = state
            .dispatcher
            .register_handler(&content, Arc::new(|_| {}))
            .expect("activation must install a context handler");
        let paths = handler_paths.clone();
        state.dispatcher.register_handler(
            &content,
            Arc::new(move |path| {
                paths.lock().unwrap().push(path.into());
                actual(path);
            }),
        );
    }

    // Warm the stream using a separate file. Every asserted operation below
    // happens once, on its own path, so delayed events cannot satisfy it.
    let warm = content.join("warmup.md");
    let mut last_write = Instant::now() - Duration::from_secs(1);
    let ready = eventually(|| {
        if last_write.elapsed() >= Duration::from_millis(500) {
            std::fs::write(&warm, "# Warm\n").unwrap();
            last_write = Instant::now();
        }
        events.lock().unwrap().contains(&warm)
    });
    let mut expected = Vec::new();
    for relative in ["", "nested"] {
        let dir = content.join(relative);
        let created = dir.join("native-created.md");
        std::fs::write(&created, "# Created\n").unwrap();
        expected.push((format!("create {relative}"), created));
        let edited = dir.join("edit.md");
        std::fs::write(&edited, "# Edited\n").unwrap();
        expected.push((format!("edit {relative}"), edited));
        let target = dir.join("replace.md");
        let replacement = dir.join(".native-replacement.tmp");
        std::fs::write(&replacement, "# Replaced\n").unwrap();
        std::fs::rename(&replacement, &target).unwrap();
        expected.push((format!("atomic replacement {relative}"), target));
        let source = dir.join("rename-source.md");
        let renamed = dir.join("native-renamed.md");
        std::fs::rename(&source, &renamed).unwrap();
        expected.push((format!("rename source {relative}"), source));
        expected.push((format!("rename destination {relative}"), renamed));
        let deleted = dir.join("delete.md");
        std::fs::remove_file(&deleted).unwrap();
        expected.push((format!("delete {relative}"), deleted));
    }
    let fresh = content.join("created-after-start/new.md");
    std::fs::create_dir_all(fresh.parent().unwrap()).unwrap();
    std::fs::write(&fresh, "# Newly nested\n").unwrap();
    expected.push(("new directory child".to_string(), fresh));
    eventually(|| {
        let paths = events.lock().unwrap();
        expected.iter().all(|(_, path)| paths.contains(path))
    });
    let mut missing: Vec<_> = expected
        .iter()
        .filter(|(_, path)| !events.lock().unwrap().contains(path))
        .map(|(operation, path)| format!("{operation}: {}", path.display()))
        .collect();
    if !ready {
        missing.push("warmup".into());
    }
    let watched = state.watcher_handle().unwrap().watched_paths().unwrap();
    let native_events = events.lock().unwrap().clone();
    let native_handler_paths = handler_paths.lock().unwrap().clone();
    let direct_probe = content.join("dispatcher-probe.md");
    if !missing.is_empty() {
        crate::content_engine::watcher::route_test_event(
            &state.dispatcher,
            notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
                .add_path(direct_probe.clone()),
        );
    }
    let direct_delivered = events.lock().unwrap().contains(&direct_probe);
    assert!(
        missing.is_empty(),
        "{boot:?}: missing context-file-change: {missing:?}; watched={watched:?}; context handler paths={native_handler_paths:?}; emitted={native_events:?}; dispatcher probe delivered={direct_delivered}",
    );
}

#[test]
#[ignore = "Native diagnostic: machine fseventsd is degraded; deterministic routing covers recovery"]
fn pbf211_native_configuration_before_team() {
    exercise(Boot::ConfigurationFirst);
}

#[test]
#[ignore = "Native diagnostic: machine fseventsd is degraded; deterministic routing covers recovery"]
fn pbf211_native_team_before_configuration() {
    exercise(Boot::TeamFirst);
}

#[test]
#[ignore = "Native diagnostic: machine fseventsd is degraded; deterministic routing covers recovery"]
fn pbf211_native_activation_before_watcher_start() {
    exercise(Boot::Deferred);
}

#[test]
#[ignore = "Native diagnostic: machine fseventsd is degraded; deterministic routing covers recovery"]
fn pbf211_native_deferred_activation_retried_after_start() {
    exercise(Boot::DeferredThenReactivated);
}

#[test]
#[ignore = "Native diagnostic: machine fseventsd is degraded; deterministic routing covers recovery"]
fn pbf211_native_content_only_control() {
    exercise(Boot::ContentOnly);
}
