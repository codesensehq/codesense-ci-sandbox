// SPDX-License-Identifier: MIT
use super::*;
use crate::attribution::{
    catch_up, checkpoints,
    git_ai::tests::git,
    turns::tests::{replay_log, replay_repo},
};
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture {
    home: tempfile::TempDir,
    repo: tempfile::TempDir,
    clock: Arc<AtomicU64>,
    owner: Arc<CheckpointWatcher>,
}
impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let repo = replay_repo(true);
        Self::setup(home.path(), repo.path());
        let clock = Arc::new(AtomicU64::new(0));
        let tick = clock.clone();
        let owner = CheckpointWatcher::new(
            home.path().to_owned(),
            None,
            Arc::new(move || tick.load(Ordering::SeqCst)),
            false,
        );
        OWNERS
            .lock()
            .unwrap()
            .insert(home.path().to_owned(), owner.clone());
        let f = Self {
            home,
            repo,
            clock,
            owner,
        };
        f.owner.drive_capture();
        f.advance(DEBOUNCE_MS);
        f
    }
    fn setup(home: &Path, repo: &Path) {
        crate::attribution::why_line::tests::attach(home, repo);
        crate::attribution::prefs::set_for_checkout(
            home,
            &crate::domain::Principal::from(1),
            repo.to_str().unwrap(),
            Some(crate::attribution::prefs::AttributionChoice::On),
            None,
        )
        .unwrap();
        std::fs::create_dir_all(home.join("bin")).unwrap();
        std::fs::write(home.join("bin/git-ai"), "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(
            home.join("bin/git-ai"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::fs::create_dir_all(repo.join(".git/ai/working_logs")).unwrap();
    }
    fn advance(&self, ms: u64) {
        self.clock.fetch_add(ms, Ordering::SeqCst);
        while self.owner.drive_capture() {}
    }
    fn log(&self) -> PathBuf {
        self.archive(self.repo.path()).join("checkpoints.jsonl")
    }
    fn event(&self, path: &Path) {
        crate::content_engine::watcher::route_test_event(
            &self.owner.watches.dispatcher,
            notify::Event::new(notify::EventKind::Create(notify::event::CreateKind::Folder))
                .add_path(path.to_owned()),
        );
    }
    /// A second attached clone of the same remote, registered and swept once.
    fn add_clone(&self) -> tempfile::TempDir {
        let repo = replay_repo(true);
        git(
            repo.path(),
            &["remote", "add", "origin", "https://github.com/acme/api.git"],
        );
        let path = crate::context_repo::path_to_user_attachments_file(
            self.home.path(),
            &crate::domain::Principal::from(1),
        );
        let mut attachments = crate::context_repo::attachments_io::read_attachments(&path).unwrap();
        attachments
            .attachments
            .push(crate::commands::attachments::existing_checkout_attachment(repo.path()).unwrap());
        crate::context_repo::attachments_io::write_attachments_atomic(&path, &attachments).unwrap();
        std::fs::create_dir_all(repo.path().join(".git/ai/working_logs")).unwrap();
        self.owner.schedule_refresh();
        self.advance(0);
        self.advance(DEBOUNCE_MS);
        repo
    }
    fn archive(&self, repo: &Path) -> PathBuf {
        let path = replay_log(repo);
        std::fs::write(path.parent().unwrap().join(".archived_at"), "1").unwrap();
        path.canonicalize().unwrap().parent().unwrap().to_owned()
    }
    fn attachments_file(&self) -> PathBuf {
        crate::context_repo::path_to_user_attachments_file(
            self.home.path(),
            &crate::domain::Principal::from(1),
        )
    }
    fn write_attachments(&self, attachments: Vec<crate::domain::Attachment>) {
        crate::context_repo::attachments_io::write_attachments_atomic(
            &self.attachments_file(),
            &crate::domain::Attachments { attachments },
        )
        .unwrap();
    }
    /// Abort every cursor insert for logs under `dir`, so that clone's import fails.
    fn fail_imports_under(&self, dir: &Path) {
        let db = self.home.path().join("usage.sqlite");
        drop(crate::session_usage::UsageStore::open(&db).unwrap());
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute_batch(&format!(
                "CREATE TRIGGER fail_imports BEFORE INSERT ON checkpoint_log_cursor \
                 WHEN NEW.path LIKE '{}%' BEGIN SELECT RAISE(ABORT, 'import failed'); END;",
                dir.to_string_lossy().replace('\'', "''")
            ))
            .unwrap();
    }
    fn passes(&self, repo: &Path) -> usize {
        self.owner
            .passes
            .lock()
            .unwrap()
            .get(&repo.canonicalize().unwrap())
            .copied()
            .unwrap_or(0)
    }
    fn rows(&self) -> i64 {
        let _store =
            crate::session_usage::UsageStore::open(&self.home.path().join("usage.sqlite")).unwrap();
        rusqlite::Connection::open(self.home.path().join("usage.sqlite"))
            .unwrap()
            .query_row("SELECT COUNT(*) FROM checkpoint_trace", [], |r| r.get(0))
            .unwrap()
    }
    fn on(&self, on: bool) {
        crate::attribution::prefs::set_for_checkout(
            self.home.path(),
            &crate::domain::Principal::from(1),
            self.repo.path().to_str().unwrap(),
            Some(if on {
                crate::attribution::prefs::AttributionChoice::On
            } else {
                crate::attribution::prefs::AttributionChoice::Off
            }),
            None,
        )
        .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.owner.stop_capture();
        OWNERS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(self.home.path());
    }
}

fn row(case: usize) {
    let f = Fixture::new();
    match case {
        1 => {
            let log = f.log();
            f.event(log.parent().unwrap());
            f.advance(DEBOUNCE_MS - 1);
            assert_eq!(f.rows(), 0);
            f.advance(1);
            assert_eq!(f.rows(), 2, "archive event must capture before why_line");
            let snapshot = catch_up::capture(f.home.path());
            assert_eq!(snapshot.trigger, Some("watcher"));
            assert_eq!(snapshot.trace_rows_stored, 2);
            assert_eq!(snapshot.session_mapping_rows_stored, 2);
        }
        2 => {
            let log = f.log();
            for _ in 0..20 {
                f.event(log.parent().unwrap());
            }
            f.advance(DEBOUNCE_MS);
            assert_eq!(f.rows(), 2);
            assert!(!f.owner.drive_capture());
            assert_eq!(
                catch_up::capture(f.home.path()).logs_new,
                1,
                "burst must produce one import pass"
            );
        }
        3 => {
            let log = f.log();
            f.event(log.parent().unwrap());
            f.on(false);
            f.advance(DEBOUNCE_MS);
            assert_eq!(f.rows(), 0);
            assert!(f.owner.owned.lock().unwrap().is_empty());
            f.on(true);
            f.advance(0);
            f.advance(DEBOUNCE_MS);
            assert_eq!(f.rows(), 2, "reenabling capture registers and reconciles");
            crate::attribution::prefs::set_for_checkout(
                f.home.path(),
                &crate::domain::Principal::from(1),
                f.repo.path().to_str().unwrap(),
                Some(crate::attribution::prefs::AttributionChoice::Auto),
                None,
            )
            .unwrap();
            f.advance(0);
            // `auto` captures (SV-CDE-40: whether a repo is public no longer matters), so the
            // watcher keeps the clone; only `off` above releases it.
            assert!(!f.owner.owned.lock().unwrap().is_empty());
        }
        4 => {
            // The git-ai namespace is deliberately unrelated to the linked admin name.
            let linked = f.home.path().join("linked");
            git(
                f.repo.path(),
                &[
                    "worktree",
                    "add",
                    "-b",
                    "new-linked",
                    linked.to_str().unwrap(),
                ],
            );
            let root = f
                .repo
                .path()
                .join(".git/ai/worktrees/arbitrary-engine-namespace/working_logs");
            let log = f.log();
            let archive = root.join(log.parent().unwrap().file_name().unwrap());
            std::fs::create_dir_all(&archive).unwrap();
            std::fs::copy(&log, archive.join("checkpoints.jsonl")).unwrap();
            std::fs::write(archive.join(".archived_at"), "1").unwrap();
            std::fs::remove_file(log).unwrap();
            // Root sentinel sees lazy namespace creation before working_logs existed.
            f.event(&f.repo.path().join(".git/ai/worktrees"));
            f.advance(0);
            f.advance(DEBOUNCE_MS);
            assert!(f
                .owner
                .owned
                .lock()
                .unwrap()
                .contains(&root.canonicalize().unwrap()));
            assert_eq!(f.rows(), 2, "first lazy archive must not be lost");
            let watched_root = root.canonicalize().unwrap();
            std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
            f.event(&f.repo.path().join(".git/ai/worktrees"));
            f.advance(0);
            assert!(!f.owner.owned.lock().unwrap().contains(&watched_root));
        }
        5 => {
            let before = catch_up::capture(f.home.path());
            assert_eq!(before.watch_errors, 0, "lazy absent directories are normal");
            let dir = f
                .repo
                .path()
                .join(".git/ai/working_logs")
                .canonicalize()
                .unwrap();
            f.owner.denied.lock().unwrap().insert(dir.clone());
            f.owner.schedule_refresh();
            f.advance(0);
            let status = catch_up::capture(f.home.path());
            assert!(
                status.watch_errors > 0,
                "registration failures must be counted"
            );
            assert_eq!(
                status.last_error.as_deref(),
                Some("checkpoint watch registration failed")
            );
            assert!(
                f.owner
                    .owned
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|p| p.ends_with("ai")),
                "other sentinel watches survive one failure"
            );
            f.owner.denied.lock().unwrap().clear();
            f.owner.metadata_denied.lock().unwrap().insert(dir.clone());
            f.owner.schedule_refresh();
            f.advance(0);
            assert!(
                catch_up::capture(f.home.path()).watch_errors > 0,
                "metadata PermissionDenied must be logged and counted"
            );
            assert_eq!(
                catch_up::capture(f.home.path()).last_error.as_deref(),
                Some("checkpoint watch registration failed")
            );
            f.owner.metadata_denied.lock().unwrap().clear();
            let namespace = f.repo.path().join(".git/ai/worktrees/denied-enumerated");
            std::fs::create_dir_all(namespace.join("working_logs")).unwrap();
            let namespace = namespace.canonicalize().unwrap();
            f.owner
                .metadata_denied
                .lock()
                .unwrap()
                .insert(namespace.clone());
            f.owner.schedule_refresh();
            f.advance(0);
            assert!(
                catch_up::capture(f.home.path()).watch_errors > 0,
                "enumeration must not filter away metadata errors"
            );
            assert!(!f.owner.owned.lock().unwrap().contains(&namespace));
            f.owner.metadata_denied.lock().unwrap().clear();
            f.owner.denied.lock().unwrap().insert(dir.clone());
            f.owner.schedule_refresh();
            f.advance(0);
            assert_eq!(
                catch_up::Capture {
                    watch_errors: 0,
                    last_error: None,
                    ..catch_up::capture(f.home.path())
                },
                before,
                "watch errors never reset the last pass's counters, lastSweepAt or trigger"
            );
            f.owner.denied.lock().unwrap().clear();
            f.owner.schedule_refresh();
            f.advance(0);
            assert_eq!(
                catch_up::capture(f.home.path()),
                before,
                "a clean reconciliation clears only the watch error"
            );
            let log = f.log();
            f.event(log.parent().unwrap());
            f.advance(DEBOUNCE_MS);
            assert_eq!(f.rows(), 2);
        }
        6 => {
            let log = f.log();
            catch_up::run_at(f.home.path(), &f.home.path().join("usage.sqlite"));
            f.event(log.parent().unwrap());
            f.advance(DEBOUNCE_MS);
            assert_eq!(f.rows(), 2);
            assert_eq!(catch_up::capture(f.home.path()).trace_rows_stored, 0);
            let mut store =
                crate::session_usage::UsageStore::open(&f.home.path().join("usage.sqlite"))
                    .unwrap();
            let probe = checkpoints::tests::Probe::new(f.repo.path());
            let stats =
                checkpoints::harvest_all(f.repo.path(), "github.com/acme/api", &mut store).unwrap();
            assert_eq!(stats.bytes_read, 0);
            assert!(probe.take().len() <= 1);
        }
        7 => {
            let log = f.log();
            f.event(log.parent().unwrap());
            f.clock.fetch_add(DEBOUNCE_MS, Ordering::SeqCst);
            let repo = f
                .owner
                .begin_capture()
                .expect("event starts one clone flight");
            for _ in 0..20 {
                f.event(log.parent().unwrap());
            }
            assert!(f.owner.begin_capture().is_none());
            assert!(f.owner.queue.lock().unwrap().slots[&repo].pending);
            f.owner.capture_clone(&repo);
            f.owner.finish_capture(&repo);
            f.advance(DEBOUNCE_MS);
            assert_eq!(f.rows(), 2);
            assert_eq!(
                catch_up::capture(f.home.path()).trace_rows_stored,
                0,
                "exactly one pending replay after in-flight events"
            );
            assert!(!f.owner.drive_capture());
        }
        8 => {
            f.owner.stop_capture();
            let log = f.log();
            f.event(log.parent().unwrap());
            f.advance(DEBOUNCE_MS);
            assert_eq!(f.rows(), 0);
            catch_up::run_at(f.home.path(), &f.home.path().join("usage.sqlite"));
            assert_eq!(f.rows(), 2);
            assert_eq!(catch_up::capture(f.home.path()).trigger, Some("startup"));
        }
        9 => {
            let log = f.log();
            let live = log
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join(git(f.repo.path(), &["rev-parse", "HEAD"]).trim());
            std::fs::create_dir_all(&live).unwrap();
            std::fs::rename(&log, live.join("checkpoints.jsonl")).unwrap();
            f.event(&live);
            f.advance(DEBOUNCE_MS);
            assert_eq!(f.rows(), 0, "live label alone is not the archive wake");
            std::fs::rename(live.join("checkpoints.jsonl"), &log).unwrap();
            f.event(log.parent().unwrap());
            f.advance(DEBOUNCE_MS);
            assert_eq!(f.rows(), 2);
        }
        10 => {
            std::fs::remove_file(f.home.path().join("bin/git-ai")).unwrap();
            f.owner.schedule_refresh();
            f.advance(0);
            assert!(f.owner.owned.lock().unwrap().is_empty());
            let log = f.log();
            f.event(log.parent().unwrap());
            f.advance(DEBOUNCE_MS);
            assert_eq!(f.rows(), 0);
            f.on(false);
            f.advance(0);
            assert!(f.owner.owned.lock().unwrap().is_empty());
        }
        11 => {
            // Clone A's import fails; a later clean pass on clone B keeps A's error.
            let b = f.add_clone();
            let a_archive = f.archive(f.repo.path());
            let b_archive = f.archive(b.path());
            let a_key = a_archive.to_string_lossy().into_owned();
            let db = f.home.path().join("usage.sqlite");
            let _store = crate::session_usage::UsageStore::open(&db).unwrap();
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute(
                &format!(
                    "CREATE TRIGGER fail_a BEFORE INSERT ON checkpoint_log_cursor \
                     WHEN NEW.path LIKE '{}%' BEGIN SELECT RAISE(ABORT, 'a failed'); END;",
                    a_key.replace('\'', "''")
                ),
                [],
            )
            .unwrap();
            f.event(&a_archive);
            f.advance(DEBOUNCE_MS);
            assert_eq!(
                catch_up::capture(f.home.path()).last_error.as_deref(),
                Some("checkpoint import failed")
            );
            f.event(&b_archive);
            f.advance(DEBOUNCE_MS);
            let status = catch_up::capture(f.home.path());
            assert!(
                status.trace_rows_stored > 0,
                "clone B's pass stored its rows"
            );
            assert_eq!(
                status.last_error.as_deref(),
                Some("checkpoint import failed"),
                "clone B's clean pass keeps clone A's error"
            );
            conn.execute_batch("DROP TRIGGER fail_a").unwrap();
            f.event(&a_archive);
            f.advance(DEBOUNCE_MS);
            assert_eq!(
                catch_up::capture(f.home.path()).last_error,
                None,
                "clone A's own success clears it"
            );
        }
        12 => {
            // Registration errors keep the startup sweep's numbers.
            f.log();
            catch_up::run_at(f.home.path(), &f.home.path().join("usage.sqlite"));
            let startup = catch_up::capture(f.home.path());
            assert_eq!(startup.trigger, Some("startup"));
            assert_eq!(startup.trace_rows_stored, 2);
            let dir = f
                .repo
                .path()
                .join(".git/ai/working_logs")
                .canonicalize()
                .unwrap();
            f.owner.denied.lock().unwrap().insert(dir);
            f.owner.schedule_refresh();
            f.advance(0);
            let status = catch_up::capture(f.home.path());
            assert!(status.watch_errors > 0);
            assert_eq!(
                status.last_error.as_deref(),
                Some("checkpoint watch registration failed")
            );
            assert_eq!(
                catch_up::Capture {
                    watch_errors: 0,
                    last_error: None,
                    ..status
                },
                startup,
                "watch errors keep the startup sweep numbers"
            );
        }
        13 => {
            // A burst of archive events across two clones: no reconcile, one pass each.
            let b = f.add_clone();
            let archives = [f.archive(f.repo.path()), f.archive(b.path())];
            let reconciles = *f.owner.reconciles.lock().unwrap();
            let passes = [f.passes(f.repo.path()), f.passes(b.path())];
            for _ in 0..20 {
                for archive in &archives {
                    f.event(archive);
                }
            }
            f.advance(DEBOUNCE_MS);
            assert_eq!(
                *f.owner.reconciles.lock().unwrap(),
                reconciles,
                "archive events only queue their clone"
            );
            assert_eq!(
                [f.passes(f.repo.path()), f.passes(b.path())],
                [passes[0] + 1, passes[1] + 1],
                "one pass per clone"
            );
        }
        14 | 15 => {
            // A clone's import error belongs to an eligible clone: turning capture off
            // (row 14) or detaching the clone (row 15) clears it without another pass.
            let archive = f.archive(f.repo.path());
            f.fail_imports_under(&archive);
            f.event(&archive);
            f.advance(DEBOUNCE_MS);
            assert_eq!(
                catch_up::capture(f.home.path()).last_error.as_deref(),
                Some("checkpoint import failed")
            );
            if case == 14 {
                f.on(false);
            } else {
                f.write_attachments(Vec::new());
                f.owner.schedule_refresh();
            }
            f.advance(0);
            assert_eq!(
                catch_up::capture(f.home.path()).last_error,
                None,
                "an ineligible clone's import error is not published"
            );
        }
        16 => {
            // A catalog error found while reconciling is published, and clears on recovery.
            let path = f.attachments_file();
            let saved = std::fs::read(&path).unwrap();
            std::fs::write(&path, "{not json").unwrap();
            f.owner.schedule_refresh();
            f.advance(0);
            assert_eq!(
                catch_up::capture(f.home.path()).last_error.as_deref(),
                Some("attached clone enumeration unavailable")
            );
            std::fs::write(&path, saved).unwrap();
            f.owner.schedule_refresh();
            f.advance(0);
            assert_eq!(catch_up::capture(f.home.path()).last_error, None);
        }
        _ => unreachable!(),
    }
}
#[test]
fn watcher_case_table_real_handler() {
    let mut failed = Vec::new();
    for case in 1..=16 {
        let result = std::panic::catch_unwind(|| row(case));
        eprintln!(
            "WATCHER ROW {case}: {}",
            if result.is_ok() { "PASS" } else { "FAIL" }
        );
        if result.is_err() {
            failed.push(case);
        }
    }
    assert!(failed.is_empty(), "watcher case rows failed: {failed:?}");
}

#[test]
fn watcher_real_emit_prefs_and_describe_setup_entries_and_shutdown() {
    let app = crate::context_repo::caller_scope::tests::AppFixture::new();
    let repo = replay_repo(true);
    let home = &app.fixture.home;
    Fixture::setup(home, repo.path());
    let time = Arc::new(AtomicU64::new(0));
    let tick = time.clone();
    let owner = CheckpointWatcher::new(
        home.clone(),
        None,
        Arc::new(move || tick.load(Ordering::SeqCst)),
        false,
    );
    OWNERS.lock().unwrap().insert(home.clone(), owner.clone());
    bind(app.app.handle(), &owner);
    owner.drive_capture();
    time.fetch_add(DEBOUNCE_MS, Ordering::SeqCst);
    owner.drive_capture();
    let log = replay_log(repo.path());
    std::fs::write(log.parent().unwrap().join(".archived_at"), "1").unwrap();
    crate::commands::worktree::emit_worktree_changed(app.app.handle());
    assert!(
        owner.queue.lock().unwrap().refresh,
        "real worktree emit must refresh registrations"
    );
    let fresh = repo.path().join(".git/ai/worktrees/second/working_logs");
    std::fs::create_dir_all(&fresh).unwrap();
    owner.drive_capture();
    time.fetch_add(DEBOUNCE_MS, Ordering::SeqCst);
    owner.drive_capture();
    let snapshot = serde_json::to_value(
        tauri::async_runtime::block_on(crate::mcp::setup_tools::describe_setup(app.app.handle()))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(snapshot["attribution"]["capture"]["trigger"], "watcher");
    assert_eq!(snapshot["attribution"]["capture"]["traceRowsStored"], 2);
    // Shutdown during a flight permits its existing transaction to finish, cancels pending.
    owner.queue_clone(&repo.path().canonicalize().unwrap());
    time.fetch_add(DEBOUNCE_MS, Ordering::SeqCst);
    let clone = owner.begin_capture().unwrap();
    owner.queue_clone(&clone);
    owner.stop_capture();
    owner.capture_clone(&clone);
    owner.finish_capture(&clone);
    assert!(owner.begin_capture().is_none());
    assert!(owner.owned.lock().unwrap().is_empty());
    // Replay after a pass completed during shutdown never duplicates committed rows.
    catch_up::run_at(home, &home.join("usage.sqlite"));
    assert_eq!(catch_up::capture(home).trace_rows_stored, 0);
    assert_eq!(catch_up::capture(home).logs_new, 0);
    let conn = rusqlite::Connection::open(home.join("usage.sqlite")).unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM checkpoint_trace", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM checkpoint_log_cursor", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    OWNERS.lock().unwrap().remove(home);
}

#[test]
fn watcher_installer_completion_registers_without_another_user_event() {
    let f = Fixture::new();
    std::fs::remove_file(f.home.path().join("bin/git-ai")).unwrap();
    f.owner.schedule_refresh();
    f.advance(0);
    assert!(f.owner.owned.lock().unwrap().is_empty());
    let log = f.log();
    crate::attribution::install::tests::complete_controlled_install(f.home.path(), || {
        assert!(f.owner.owned.lock().unwrap().is_empty());
        assert_eq!(f.rows(), 0);
    });
    // No manual refresh, preferences write or worktree emit after completion.
    assert!(
        f.owner.queue.lock().unwrap().refresh,
        "installer completion must notify the already-registered watcher owner"
    );
    f.advance(0);
    assert!(!f.owner.owned.lock().unwrap().is_empty());
    f.event(log.parent().unwrap());
    f.advance(DEBOUNCE_MS);
    assert_eq!(f.rows(), 2);
}

/// The app-lifetime watcher for a CodeSense home, registered as the app registers it and
/// driven by hand: each [`AppOwner::drive`] is one pass of its loop.
pub(crate) struct AppOwner(Arc<CheckpointWatcher>);

impl AppOwner {
    /// A watcher as app startup makes one, with its first refresh still pending.
    pub(crate) fn start(home: &Path) -> Self {
        let owner = CheckpointWatcher::new(home.to_owned(), None, Arc::new(|| 0), false);
        OWNERS
            .lock()
            .unwrap()
            .insert(home.to_owned(), owner.clone());
        Self(owner)
    }

    /// One pass: a pending refresh (startup, or an attachment written) runs.
    pub(crate) fn drive(&self) {
        self.0.drive_capture();
    }

    pub(crate) fn refresh_pending(&self) -> bool {
        self.0.queue.lock().unwrap().refresh
    }
}

impl Drop for AppOwner {
    fn drop(&mut self) {
        OWNERS.lock().unwrap().remove(&self.0.home);
        self.0.stop_capture();
    }
}
