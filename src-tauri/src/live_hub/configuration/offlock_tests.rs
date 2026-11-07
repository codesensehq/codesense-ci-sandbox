// SPDX-License-Identifier: MIT
//! Source reads must never run while the LiveHub model lock is held.
use super::*;
use std::sync::mpsc;
use tauri::{test::MockRuntime, AppHandle, Manager};

const BOUND: std::time::Duration = std::time::Duration::from_secs(5);

struct Stalled {
    thread: std::thread::JoinHandle<()>,
    release: mpsc::Sender<()>,
}
impl Stalled {
    /// Runs `work` on its own thread; its first source read parks until `release`.
    fn start(
        app: &AppHandle<MockRuntime>,
        work: impl FnOnce(&AppHandle<MockRuntime>) + Send + 'static,
    ) -> Self {
        let (entered_tx, entered) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let app = app.clone();
        let thread = std::thread::spawn(move || {
            let gate = std::cell::RefCell::new(Some((entered_tx, release_rx)));
            tests::READ_HOOK.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move |_| {
                    if let Some((entered, release)) = gate.borrow_mut().take() {
                        entered.send(()).unwrap();
                        let _ = release.recv();
                    }
                }));
            });
            work(&app);
        });
        entered
            .recv_timeout(BOUND)
            .expect("the work never reached its source read");
        Self { thread, release }
    }
    fn finish(self) {
        self.release.send(()).unwrap();
        self.thread.join().unwrap();
    }
}

fn mock_app() -> tauri::App<MockRuntime> {
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(super::super::LiveHub::default());
    app
}
fn assert_model_free(app: &AppHandle<MockRuntime>, stalled: Stalled, what: &str) {
    let free = app
        .state::<super::super::LiveHub>()
        .model
        .try_lock()
        .is_ok();
    stalled.finish();
    assert!(free, "{what} held the model lock across its source read");
}
fn config_path(home: &Path) -> PathBuf {
    home.join("workspace-config.json")
}
fn write_repos(home: &Path, repos: Value) {
    std::fs::write(
        config_path(home),
        serde_json::to_vec(&json!({"repos": repos})).unwrap(),
    )
    .unwrap();
}
fn subscribed(app: &AppHandle<MockRuntime>, home: &Path, principal: &Principal) -> Snapshot {
    let initial = prepare(app, home, principal, "workspaceConfig", None).unwrap();
    super::super::subscribe(
        app,
        "workspaceConfig",
        &initial.view_key,
        Some(initial.rev),
        Some("native:configuration:offlock"),
        None,
    )
    .unwrap();
    initial
}

#[test]
fn opening_a_view_reads_its_source_without_the_model_lock() {
    let home = tempfile::tempdir().unwrap();
    write_repos(home.path(), json!({"a": {"mainDoc": "A.md"}}));
    let app = mock_app();
    let dir = home.path().to_path_buf();
    let stalled = Stalled::start(app.handle(), move |app| {
        prepare(app, &dir, &Principal::from(42u64), "workspaceConfig", None).unwrap();
    });
    assert_model_free(app.handle(), stalled, "prepare");
}

#[test]
fn refreshing_a_dirty_source_on_subscribe_reads_without_the_model_lock() {
    let home = tempfile::tempdir().unwrap();
    let principal = Principal::from(42u64);
    write_repos(home.path(), json!({"a": {"mainDoc": "A.md"}}));
    let app = mock_app();
    let initial = subscribed(app.handle(), home.path(), &principal);
    super::super::unsubscribe(app.handle(), "native:configuration:offlock", false);
    write_repos(home.path(), json!({"b": {"mainDoc": "B.md"}}));
    changed(app.handle(), &config_path(home.path()), None).unwrap();
    let key = initial.view_key.clone();
    let stalled = Stalled::start(app.handle(), move |app| {
        super::super::subscribe(
            app,
            "workspaceConfig",
            &key,
            Some(initial.rev),
            Some("native:configuration:offlock"),
            None,
        )
        .unwrap();
    });
    assert_model_free(app.handle(), stalled, "subscribe");
}

#[test]
fn the_watcher_read_runs_without_the_model_lock() {
    let home = tempfile::tempdir().unwrap();
    write_repos(home.path(), json!({"a": {"mainDoc": "A.md"}}));
    let app = mock_app();
    subscribed(app.handle(), home.path(), &Principal::from(42u64));
    write_repos(home.path(), json!({"b": {"mainDoc": "B.md"}}));
    let path = config_path(home.path());
    let stalled = Stalled::start(app.handle(), move |app| {
        changed(app, &path, None).unwrap();
    });
    assert_model_free(app.handle(), stalled, "the watcher path");
}

#[test]
fn a_newer_change_beats_a_read_that_was_already_in_flight() {
    let home = tempfile::tempdir().unwrap();
    write_repos(home.path(), json!({"old": {"mainDoc": "Old.md"}}));
    let app = mock_app();
    let initial = subscribed(app.handle(), home.path(), &Principal::from(42u64));
    write_repos(home.path(), json!({"stale": {"mainDoc": "Stale.md"}}));
    let path = config_path(home.path());
    let watcher_path = path.clone();
    let stalled = Stalled::start(app.handle(), move |app| {
        changed(app, &watcher_path, None).unwrap();
    });
    // The in-flight read holds the stale file. A newer value lands before it is applied.
    let (done_tx, done) = mpsc::channel();
    let handle = app.handle().clone();
    let ingress = std::thread::spawn(move || {
        value_changed(
            &handle,
            &path,
            json!({"repos": {"newer": {"mainDoc": "Newer.md"}}}),
        )
        .unwrap();
        done_tx.send(()).unwrap();
    });
    let landed = done.recv_timeout(BOUND).is_ok();
    stalled.finish();
    ingress.join().unwrap();
    assert!(landed, "a newer change waited behind an in-flight read");
    let rows = app
        .state::<super::super::LiveHub>()
        .snapshot(&initial.view_key, "workspaceConfig")
        .unwrap()
        .snapshot;
    let ids: Vec<_> = rows.iter().map(|row| row["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["newer"], "the stale read overwrote the newer change");
}
