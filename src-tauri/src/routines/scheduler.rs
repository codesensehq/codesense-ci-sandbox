// SPDX-License-Identifier: MIT
//! The routines' own scheduler thread.
//!
//! One named OS thread wakes every [`TICK_INTERVAL`] and runs one routine pass
//! ([`super::tick::run_routine_tick`]): bound and settle started runs (the
//! run-timeout watchdog), then claim every due schedule occurrence. The thread
//! shares only with the launch-deadline sweep: both passes run in
//! their own [`guarded_wake`] so a panic in one never stops the other, and
//! each is single-flight by construction (the thread is single-threaded).
//!
//! The pass does blocking file and SQLite IO, which is why it is a plain thread
//! and not an async task.

use std::time::Duration;

use tauri::{AppHandle, Manager, Runtime};

use crate::shared::panic_guard::guarded_wake;

use super::tick::{run_routine_tick, RoutineTicker};

/// How often the thread wakes. Each schedule still fires only once its
/// occurrence is due; this is the resolution at which "is anything due?" is
/// re-checked.
pub const TICK_INTERVAL: Duration = Duration::from_secs(15);

/// Start the routine scheduler for the app: install the change emitter, register
/// the production ticker and spawn the thread. Called once from `lib.rs` setup.
pub fn spawn_scheduler(app: AppHandle) {
    super::events::install_app_emitter(app.clone());
    app.manage(RoutineTicker::for_app(app.clone()));
    super::inbox_idle::sweep_at_boot(app.clone());
    // the launch-deadline sweep is spawned alongside the routines
    // thread on the production runtime only. Shared with the routines pass
    // only through `guarded_wake` so a panic in one never stops the other,
    // and independent of sign-in (the pass runs even for a signed-out boot).
    let sweep_app = app.clone();
    spawn_ticker_thread(
        app,
        TICK_INTERVAL,
        || true,
        move || {
            crate::commands::dashboard_launch::run_deadline_sweep(&sweep_app);
            crate::commands::dashboard_launch::run_startup_sweep(&sweep_app);
        },
    );
}

/// Spawn the thread that runs the app's registered [`RoutineTicker`] every
/// `interval` for as long as `keep_running` holds (the app's lifetime in
/// production). A panicking pass is logged and swallowed, so one bad wake
/// never stops later ones. `after_tick` runs after each pass in its own
/// `guarded_wake` — production wires the launch-deadline sweep
/// through it; tests can observe the callback or supply a no-op.
pub(crate) fn spawn_ticker_thread<R: Runtime, F: Fn() + Send + 'static>(
    app: AppHandle<R>,
    interval: Duration,
    keep_running: impl Fn() -> bool + Send + 'static,
    after_tick: F,
) -> Option<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("routine-scheduler".into())
        .spawn(move || loop {
            std::thread::sleep(interval);
            if !keep_running() {
                break;
            }
            guarded_wake("routine-tick", || run_routine_tick(&app));
            guarded_wake("launch-deadline", &after_tick);
        })
        .map_err(|e| log::warn!(target: "routines", "routine scheduler not started: {e}"))
        .ok()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    use super::*;
    use crate::commands::pty_backend::Liveness;
    use crate::routines::finish;
    use crate::routines::run_seams::{key, manual, ms, routine, run, save, store, ticker};
    use crate::routines::test_support::{
        drain_claim_loop, stub_agent, FakeSessions, StubLauncher, TempHome,
    };

    struct StopOnDrop(Arc<AtomicBool>);

    impl Drop for StopOnDrop {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }

    /// Poll `done` until it holds, failing after a few seconds.
    fn wait_for(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn after_tick_runs_without_an_account_and_recovers_from_a_panic() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let clock_reads = Arc::new(AtomicUsize::new(0));
        let reads = Arc::clone(&clock_reads);
        let account_reads = Arc::new(AtomicUsize::new(0));
        let accounts = Arc::clone(&account_reads);
        app.manage(RoutineTicker::new(
            Arc::new(move || {
                reads.fetch_add(1, Ordering::SeqCst);
                1_000
            }),
            Arc::new(crate::routines::tick::RoutineAdmitter),
            Arc::new(move || {
                accounts.fetch_add(1, Ordering::SeqCst);
                None
            }),
            Arc::new(FakeSessions::default()),
        ));

        let running = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&running);
        let callbacks = Arc::new(AtomicUsize::new(0));
        let calls = Arc::clone(&callbacks);
        let thread = spawn_ticker_thread(
            app.handle().clone(),
            Duration::from_millis(5),
            move || flag.load(Ordering::Acquire),
            move || {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    panic!("injected deadline callback panic");
                }
            },
        )
        .expect("thread");
        let stop = StopOnDrop(Arc::clone(&running));

        wait_for("callbacks after the injected panic", || {
            callbacks.load(Ordering::SeqCst) >= 3
        });
        drop(stop);
        thread
            .join()
            .expect("callback panic stays inside its guard");

        assert!(account_reads.load(Ordering::SeqCst) > 0);
        assert_eq!(
            clock_reads.load(Ordering::SeqCst),
            callbacks.load(Ordering::SeqCst) + 1,
            "each callback follows a routine tick, plus the ticker's initial clock read"
        );
    }

    /// Nothing here starts the automation loop scheduler: the routine thread
    /// alone seeds the cursor, claims the due occurrence, and the claim loop
    /// launches its agent.
    #[tokio::test]
    async fn a_cron_routine_fires_through_the_routine_scheduler_thread_alone() {
        let temp = TempHome::new();
        let home = temp.codesense_home();
        save(&home, routine("standalone", "0 9 * * *"));
        let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
        let clock = Arc::new(AtomicI64::new(ms("2026-10-05T08:00:00Z")));
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        app.manage(ticker(&home, &clock, launcher.clone()));
        let running = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&running);
        let thread = spawn_ticker_thread(
            app.handle().clone(),
            Duration::from_millis(5),
            move || flag.load(Ordering::Acquire),
            || {},
        )
        .expect("thread");
        // Stops the thread on every exit, a failed wait included, so it never
        // outlives this test's home.
        let stop = StopOnDrop(Arc::clone(&running));

        wait_for("the cursor to be seeded", || {
            !store(&home).triggers().unwrap().is_empty()
        });
        clock.store(ms("2026-10-05T09:01:00Z"), Ordering::SeqCst);
        wait_for("the occurrence to be queued", || {
            !store(&home).runs(&key("standalone")).unwrap().is_empty()
        });
        drop(stop);
        thread.join().unwrap();

        let runs = store(&home).runs(&key("standalone")).unwrap();
        assert_eq!(runs.len(), 1, "one run per occurrence: {runs:?}");
        assert_eq!(runs[0].source, "schedule");
        assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
        let launched = launcher.last();
        assert_eq!(launched.request.provenance.run_id, runs[0].run_id);
        launcher.kill_child(&launched.session_id);
    }

    /// Runs the claim loop launched before a restart are settled at startup:
    /// a gone session's run is interrupted, and a surviving one is handed to
    /// the routine ticker, which ends it once its session does.
    #[tokio::test]
    async fn runs_launched_through_the_claim_loop_are_reconciled_at_startup() {
        let temp = TempHome::new();
        let home = temp.codesense_home();
        save(&home, routine("gone", "*/5 * * * *"));
        // A tab: its ending is swept by the tick, unlike a headless run's.
        let mut tab = routine("survivor", "*/5 * * * *");
        tab.policy.visible = true;
        // A fresh run in a tab (a visible routine now defaults to wake).
        tab.mode = "fresh".into();
        save(&home, tab);
        let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
        let gone = manual(&home, "gone").run_id;
        let survivor = manual(&home, "survivor").run_id;
        assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 2);
        let gone_session = run(&home, &gone).session_id.unwrap();
        let survivor_session = run(&home, &survivor).session_id.unwrap();

        // The restarted app: only the survivor's session is still running.
        let sessions = Arc::new(FakeSessions::default());
        sessions.set(&survivor_session, Liveness::Known(true));
        let clock = Arc::new(AtomicI64::new(finish::now_ms()));
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        app.manage(ticker(&home, &clock, sessions.clone()));
        let report =
            finish::reconcile_at_startup(app.handle(), &store(&home), finish::now_ms(), &*sessions)
                .unwrap();
        assert_eq!(report.interrupted, vec![gone.clone()]);
        assert_eq!(report.alive, vec![survivor.clone()]);
        let row = run(&home, &gone);
        assert_eq!(
            (row.phase.as_str(), row.reason.as_deref()),
            ("failed", Some("interrupted"))
        );
        assert_eq!(run(&home, &survivor).phase, "running");

        // Its session ends; the next routine pass settles it.
        sessions.set(&survivor_session, Liveness::Known(false));
        app.state::<RoutineTicker>().tick();
        let row = run(&home, &survivor);
        assert_eq!((row.phase.as_str(), row.reason.as_deref()), ("ended", None));
        launcher.kill_child(&gone_session);
        launcher.kill_child(&survivor_session);
    }
}
