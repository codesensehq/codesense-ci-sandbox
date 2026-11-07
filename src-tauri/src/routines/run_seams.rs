// SPDX-License-Identifier: MIT
//! Seam tests for a routine run's life: admission policy over a real
//! coordinator, the claim loop's routine dispatch, the outcome funnel that ends
//! a run, the watchdog and the restart reconcile.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use chrono::DateTime;

use crate::commands::pty_backend::Liveness;
use crate::coordinator::{
    default_store_path, OrchestrationStore, RunState, SqliteOrchestrationStore,
};
use crate::dashboard_stream::SessionStatus;

use super::admit::{parse_pointer, Outcome};
use super::finish::{self, Ending};
use super::model::{
    AgentRef, Author, OverlapPolicy, Policy, RoutineDef, RoutineKey, RoutineTrigger, ScheduleSpec,
    Where,
};
use super::runs::RoutineRun;
use super::service::{self, Placement, SaveTarget};
use super::state::RoutineStateStore;
use super::test_support::{
    alice, drain_claim_loop, exit_code, killed_by, signal_done_via, stub_agent, write_org,
    FakeSessions, StubLauncher, TempHome, ORG, TEAM,
};
use super::tick::{Clock, RoutineAdmitter, RoutineTicker};

pub(super) fn key(id: &str) -> RoutineKey {
    RoutineKey::new(ORG, TEAM, id)
}

pub(super) fn routine(id: &str, cron: &str) -> RoutineDef {
    RoutineDef {
        action: Default::default(),
        command: Vec::new(),
        after: None,
        id: id.into(),
        name: format!("Routine {id}"),
        owner: String::new(),
        agent: AgentRef::from("default"),
        mode: String::new(),
        task: "Summarize what changed. Last time: {{last_run_summary}}".into(),
        target: Where::team_workspace(),
        triggers: vec![RoutineTrigger::Schedule(ScheduleSpec {
            cron: cron.into(),
            tz: "UTC".into(),
        })],
        policy: Policy {
            max_runtime: Some("30m".into()),
            ..Policy::default()
        },
        variables: Default::default(),
        revision: 0,
        armed_host: String::new(),
        result_schema: None,
        runs_on: String::new(),
        archived_at: None,
        armed_machine: None,
        arm_epoch: 0,
        handover_from: None,
        archived_with_agent: false,
        migrated: None,
        extra: Default::default(),
    }
}

pub(super) fn save(home: &Path, def: RoutineDef) {
    service::save(
        home,
        &alice(),
        SaveTarget::Create(Placement::Team {
            org: ORG.into(),
            team: TEAM.into(),
        }),
        def,
        &Author::user("alice"),
    )
    .unwrap();
}

pub(super) fn manual(home: &Path, id: &str) -> super::admit::Admission {
    service::run_now(home, &alice(), &key(id), BTreeMap::new()).unwrap()
}

pub(super) fn store(home: &Path) -> RoutineStateStore {
    RoutineStateStore::open(home).unwrap()
}

pub(super) fn run(home: &Path, run_id: &str) -> RoutineRun {
    store(home).run(run_id).unwrap().expect("run row")
}

fn coordinator(home: &Path) -> SqliteOrchestrationStore {
    SqliteOrchestrationStore::open(default_store_path(home)).unwrap()
}

pub(super) fn ms(rfc3339: &str) -> i64 {
    DateTime::parse_from_rfc3339(rfc3339)
        .unwrap()
        .timestamp_millis()
}

const MIN: i64 = 60_000;

pub(super) fn ticker(
    home: &Path,
    clock: &Arc<AtomicI64>,
    sessions: Arc<dyn finish::Sessions>,
) -> RoutineTicker {
    ticker_as(home, clock, sessions, alice())
}

/// A ticker for the app signed in as `principal`.
pub(super) fn ticker_as(
    home: &Path,
    clock: &Arc<AtomicI64>,
    sessions: Arc<dyn finish::Sessions>,
    principal: crate::domain::Principal,
) -> RoutineTicker {
    let c = Arc::clone(clock);
    let clock: Clock = Arc::new(move || c.load(Ordering::SeqCst));
    let home = home.to_path_buf();
    RoutineTicker::new(
        clock,
        Arc::new(RoutineAdmitter),
        Arc::new(move || Some((home.clone(), principal.clone()))),
        sessions,
    )
}

/// Routine deliveries `recover` would re-offer.
fn pending_routine_deliveries(home: &Path) -> usize {
    coordinator(home)
        .recover(0)
        .unwrap()
        .pending_deliveries
        .iter()
        .filter(|d| d.adapter_id == "routine")
        .count()
}

pub(super) fn files_named(root: &Path, name: &str) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|e| {
            let path = e.path();
            if path.is_dir() {
                files_named(&path, name)
            } else {
                usize::from(path.file_name().is_some_and(|n| n == name))
            }
        })
        .sum()
}

#[tokio::test]
async fn a_scheduled_fire_runs_to_succeeded_and_feeds_the_next_task() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, routine("digest", "*/5 * * * *"));
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let clock = Arc::new(AtomicI64::new(ms("2026-10-05T09:01:00Z")));
    let ticker = ticker(&home, &clock, launcher.clone());

    // The first wake seeds the cursor; the wake after 09:05 admits it.
    ticker.tick().unwrap();
    clock.store(ms("2026-10-05T09:05:10Z"), Ordering::SeqCst);
    let report = ticker.tick().unwrap();
    assert_eq!(report.admitted, 1);
    let run_id = report.changed[0].1.clone();
    assert_eq!(run(&home, &run_id).phase, "queued");
    assert_eq!(
        run(&home, &run_id).occurrence_key.as_deref(),
        Some("acme/api/backend/digest@2026-10-05T09:05:00Z")
    );

    // The real claim loop drains it through the routine branch.
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    assert_eq!(launcher.calls(), 1);
    let launched = launcher.last();
    let row = run(&home, &run_id);
    assert_eq!(row.phase, "running");
    assert_eq!(
        row.session_id.as_deref(),
        Some(launched.session_id.as_str())
    );
    assert!(!launched.request.visible);
    assert_eq!(
        launched.request.handle,
        format!("routine:acme/api/backend/digest@{run_id}")
    );
    assert_eq!(
        launched.request.provenance.routine_key.as_deref(),
        Some("acme/api/backend/digest")
    );
    assert_eq!(
        launched.request.provenance.trigger_source.as_deref(),
        Some("routine")
    );
    // The rendered task is the agent's whole prompt, and reaches it unchanged.
    assert_eq!(
        launched.request.prompt,
        "Summarize what changed. Last time: "
    );
    assert_eq!(
        launcher.received_prompt(&launched.session_id),
        b"Summarize what changed. Last time: "
    );
    assert_eq!(
        coordinator(&home)
            .count_runs_in_state(RunState::Done)
            .unwrap(),
        1,
        "the claim is retired at dispatch, not held for the agent's runtime"
    );

    // The agent signals; the run ends through the outcome funnel.
    launcher
        .signal_done(&launched.session_id, "success", "ok")
        .await;
    let row = run(&home, &run_id);
    assert_eq!(row.phase, "succeeded");
    assert_eq!(row.result_text.as_deref(), Some("ok"));
    let state = store(&home).state(&key("digest")).unwrap().unwrap();
    assert_eq!(state.last_result_text.as_deref(), Some("ok"));
    assert_eq!(state.last_run_id.as_deref(), Some(run_id.as_str()));
    // Its process exit afterwards changes nothing.
    launcher.exit(&launched.session_id).await;
    assert_eq!(run(&home, &run_id).phase, "succeeded");

    // The next fire's task carries the last summary.
    clock.store(ms("2026-10-05T09:10:10Z"), Ordering::SeqCst);
    assert_eq!(ticker.tick().unwrap().admitted, 1);
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    assert_eq!(launcher.calls(), 2);
    assert_eq!(
        launcher.received_prompt(&launcher.last().session_id),
        b"Summarize what changed. Last time: ok"
    );

    assert_eq!(pending_routine_deliveries(&home), 0);
    assert_eq!(
        files_named(temp.root(), "weft.sqlite3"),
        0,
        "routines never open the Weft ledger"
    );
    launcher.exit(&launcher.last().session_id).await;
}

#[tokio::test]
async fn a_killed_app_leaves_an_interrupted_run_that_is_never_relaunched() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, routine("digest", "*/5 * * * *"));
    let first = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let run_id = manual(&home, "digest").run_id;
    assert_eq!(drain_claim_loop(&home, first.clone()).await, 1);
    let session_id = first.last().session_id;
    // The app dies: its process and its launch registry go with it.
    first.kill_child(&session_id);
    drop(first);

    let fresh = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let report =
        finish::reconcile_runs(&store(&home), finish::now_ms(), fresh.as_ref(), None).unwrap();
    assert_eq!(report.interrupted, vec![run_id.clone()]);
    let row = run(&home, &run_id);
    assert_eq!(
        (row.phase.as_str(), row.reason.as_deref()),
        ("failed", Some("interrupted"))
    );

    // Even a coordinator claim the crash left behind re-drives to nothing.
    let conn = rusqlite::Connection::open(default_store_path(&home)).unwrap();
    conn.execute(
        "UPDATE runs SET state = 'claimed', claimed_by = 'dead', lease_expiry = 0 WHERE run_id = ?1",
        [&run_id],
    )
    .unwrap();
    assert_eq!(coordinator(&home).recover(1).unwrap().reclaimed_claims, 1);
    assert_eq!(drain_claim_loop(&home, fresh.clone()).await, 1);
    assert_eq!(fresh.calls(), 0, "a run is never relaunched");
    assert_eq!(run(&home, &run_id).phase, "failed");
}

#[tokio::test]
async fn the_watchdog_times_out_a_run_and_the_late_exit_does_not_overwrite_it() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mut def = routine("digest", "*/5 * * * *");
    def.policy.max_runtime = Some("1m".into());
    save(&home, def);
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let clock = Arc::new(AtomicI64::new(finish::now_ms()));
    let ticker = ticker(&home, &clock, launcher.clone());
    let run_id = manual(&home, "digest").run_id;
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    let session_id = launcher.last().session_id;
    assert!(launcher.is_running(&session_id));
    // A bound under the generic headless ceiling is the headless process's too.
    assert_eq!(
        launcher.last().request.max_runtime,
        std::time::Duration::from_secs(60)
    );

    clock.fetch_add(30_000, Ordering::SeqCst);
    ticker.tick().unwrap();
    assert_eq!(run(&home, &run_id).phase, "running", "within max_runtime");

    clock.fetch_add(MIN, Ordering::SeqCst);
    ticker.tick().unwrap();
    assert_eq!(run(&home, &run_id).phase, "timed_out");
    assert!(!launcher.is_running(&session_id), "the agent was killed");

    // The killed process's exit, and even a late signal, lose the race.
    launcher.exit_with(&session_id, killed_by(9), "", "").await;
    signal_done_via(&launcher.registry, &session_id, "success", "late").await;
    let row = run(&home, &run_id);
    assert_eq!(row.phase, "timed_out");
    assert_eq!(row.result_text, None);
}

#[tokio::test]
async fn a_long_max_runtime_governs_its_headless_run_past_the_generic_ceiling() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mut def = routine("nightly", "*/5 * * * *");
    def.policy.max_runtime = Some("2h".into());
    save(&home, def);
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let clock = Arc::new(AtomicI64::new(finish::now_ms()));
    let ticker = ticker(&home, &clock, launcher.clone());
    let run_id = manual(&home, "nightly").run_id;
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    let launched = launcher.last();
    assert!(!launched.request.visible);
    // The headless process is given the routine's own bound, not the
    // generic 30-minute one.
    assert_eq!(
        launched.request.max_runtime,
        std::time::Duration::from_secs(2 * 60 * 60)
    );

    clock.fetch_add(45 * MIN, Ordering::SeqCst);
    ticker.tick().unwrap();
    assert_eq!(run(&home, &run_id).phase, "running");

    clock.fetch_add(2 * 60 * MIN, Ordering::SeqCst);
    ticker.tick().unwrap();
    assert_eq!(run(&home, &run_id).phase, "timed_out");
    assert!(!launcher.is_running(&launched.session_id));
}

#[tokio::test]
async fn a_headless_agent_that_exits_cleanly_without_a_signal_simply_ends() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, routine("quiet", "*/5 * * * *"));
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));

    // A signalled run first: its summary and its question are the routine's.
    let asked = manual(&home, "quiet").run_id;
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    let session = launcher.last().session_id;
    launcher
        .signal_done(&session, "needs-attention", "which repo?")
        .await;
    launcher.exit(&session).await;
    assert_eq!(run(&home, &asked).phase, "needs_you");

    // Then a run whose agent exits 0, printing something, without signalling.
    let run_id = manual(&home, "quiet").run_id;
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    let session = launcher.last().session_id;
    launcher
        .exit_with(&session, exit_code(0), "all done here", "")
        .await;
    let row = run(&home, &run_id);
    assert_eq!(
        (
            row.phase.as_str(),
            row.reason.as_deref(),
            row.result_text.as_deref()
        ),
        ("ended", None, None),
        "stdout is not a result: only signal_done gives one"
    );
    assert!(row.ended_at.is_some());

    // The routine's last run is the ended one, but its summary, and the
    // question it is waiting on, still come from the run that signalled.
    let state = store(&home).state(&key("quiet")).unwrap().unwrap();
    assert_eq!(state.last_run_id.as_deref(), Some(run_id.as_str()));
    assert_eq!(state.last_result_text, None);
    assert_eq!(service::needs_you_count(&home, &alice()).unwrap(), 1);
    let needing = store(&home).needing_you().unwrap();
    assert_eq!(needing[&key("quiet").to_string()].run_id, asked);
}

#[tokio::test]
async fn a_headless_agent_that_exits_non_zero_or_crashes_fails_with_the_exit_reason() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, routine("crashy", "*/5 * * * *"));
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));

    let run_id = manual(&home, "crashy").run_id;
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    launcher
        .exit_with(
            &launcher.last().session_id,
            exit_code(2),
            "partial",
            "error: no credentials\nmore",
        )
        .await;
    let row = run(&home, &run_id);
    assert_eq!(
        (
            row.phase.as_str(),
            row.reason.as_deref(),
            row.result_text.as_deref()
        ),
        (
            "failed",
            Some("exited_with_error"),
            Some("error: no credentials")
        )
    );

    let run_id = manual(&home, "crashy").run_id;
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    launcher
        .exit_with(&launcher.last().session_id, killed_by(9), "", "")
        .await;
    let row = run(&home, &run_id);
    assert_eq!(
        (
            row.phase.as_str(),
            row.reason.as_deref(),
            row.result_text.as_deref()
        ),
        (
            "failed",
            Some("exited_with_error"),
            Some("agent exited non-zero")
        )
    );
    assert_eq!(service::needs_you_count(&home, &alice()).unwrap(), 0);

    // What it reported is read back from run history, so a secret is scrubbed.
    let run_id = manual(&home, "crashy").run_id;
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    launcher
        .exit_with(
            &launcher.last().session_id,
            exit_code(1),
            "",
            "error: bad token ghp_abcdefghijklmnopqrstuvwxyz0123",
        )
        .await;
    let text = run(&home, &run_id).result_text.unwrap();
    assert!(
        !text.contains("ghp_abcdefghijklmnopqrstuvwxyz0123"),
        "{text}"
    );

    // A stop from outside the routine is a cancel, not the agent's error.
    let run_id = manual(&home, "crashy").run_id;
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    launcher.capture_stopped(&launcher.last().session_id).await;
    let row = run(&home, &run_id);
    assert_eq!(
        (row.phase.as_str(), row.reason.as_deref()),
        ("cancelled", None)
    );

    // The capture's own time limit is a timeout, not an agent error.
    let run_id = manual(&home, "crashy").run_id;
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    launcher
        .capture_timed_out(&launcher.last().session_id)
        .await;
    let row = run(&home, &run_id);
    assert_eq!(
        (row.phase.as_str(), row.reason.as_deref()),
        ("timed_out", None)
    );
}

#[tokio::test]
async fn a_task_that_fills_in_empty_never_launches() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mut def = routine("blank", "*/5 * * * *");
    def.task = "{{last_run_summary}}".into();
    save(&home, def);
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let run_id = manual(&home, "blank").run_id;
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    assert_eq!(launcher.calls(), 0);
    let row = run(&home, &run_id);
    assert_eq!(
        (
            row.phase.as_str(),
            row.reason.as_deref(),
            row.result_text.as_deref(),
            row.detail.as_deref()
        ),
        (
            "failed",
            Some("task_unrenderable"),
            None,
            Some("the task is empty once filled in")
        )
    );
}

#[tokio::test]
async fn the_wake_leaves_an_exited_headless_run_to_its_own_completion() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, routine("racer", "*/5 * * * *"));
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let clock = Arc::new(AtomicI64::new(finish::now_ms() - MIN));
    let ticker = ticker(&home, &clock, launcher.clone());
    let run_id = manual(&home, "racer").run_id;
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    clock.store(finish::now_ms(), Ordering::SeqCst);
    let session = launcher.last().session_id;

    // The process is gone, but its completion has not recorded how it exited.
    launcher.kill_child(&session);
    ticker.tick().unwrap();
    assert_eq!(run(&home, &run_id).phase, "running");

    // The completion then reports a crash, and the run fails.
    launcher.exit_with(&session, killed_by(9), "", "").await;
    let row = run(&home, &run_id);
    assert_eq!(
        (row.phase.as_str(), row.reason.as_deref()),
        ("failed", Some("exited_with_error"))
    );
}

#[tokio::test]
async fn a_visible_run_launches_as_a_tab_and_its_signal_succeeds() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mut def = routine("digest", "*/5 * * * *");
    def.policy.visible = true;
    // A fresh run in a tab (a visible routine now defaults to wake).
    def.mode = "fresh".into();
    save(&home, def);
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let run_id = manual(&home, "digest").run_id;
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    let launched = launcher.last();
    assert!(launched.request.visible);
    assert_eq!(launched.request.provenance.run_id, run_id);

    launcher
        .signal_done(&launched.session_id, "success", "tab done")
        .await;
    let row = run(&home, &run_id);
    assert_eq!(row.phase, "succeeded");
    assert_eq!(row.result_text.as_deref(), Some("tab done"));
    launcher.kill_child(&launched.session_id);
}

#[tokio::test]
async fn a_run_surviving_a_restart_ends_through_its_session_id() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mut def = routine("digest", "*/5 * * * *");
    def.policy.visible = true;
    // A fresh run in a tab (a visible routine now defaults to wake).
    def.mode = "fresh".into();
    save(&home, def);
    save(&home, routine("other", "*/5 * * * *"));
    let first = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let survivor = manual(&home, "digest").run_id;
    let unknown = manual(&home, "other").run_id;
    assert_eq!(drain_claim_loop(&home, first.clone()).await, 2);
    let survivor_session = run(&home, &survivor).session_id.unwrap();
    let unknown_session = run(&home, &unknown).session_id.unwrap();

    // After a restart the session server still has the tab; the other
    // session's host cannot be asked yet.
    let sessions = FakeSessions::default();
    sessions.set(&survivor_session, Liveness::Known(true));
    sessions.set(&unknown_session, Liveness::Unknown);
    let report = finish::reconcile_runs(&store(&home), finish::now_ms(), &sessions, None).unwrap();
    assert_eq!(report.alive, vec![survivor.clone()]);
    assert_eq!(report.unknown, vec![unknown.clone()]);
    assert_eq!(run(&home, &survivor).phase, "running");
    assert_eq!(run(&home, &unknown).phase, "running");

    // Its signal reaches a fresh app with no run context for the session.
    let fresh = crate::commands::dashboard_launch::DashboardLaunchState::new(4);
    signal_done_via(&fresh, &survivor_session, "needs-attention", "need a hand").await;
    let row = run(&home, &survivor);
    assert_eq!(row.phase, "needs_you");
    assert_eq!(row.result_text.as_deref(), Some("need a hand"));

    // Once the host answers, the unknown run settles.
    sessions.set(&unknown_session, Liveness::Known(false));
    finish::reconcile_runs(
        &store(&home),
        finish::now_ms(),
        &sessions,
        Some(std::slice::from_ref(&unknown)),
    )
    .unwrap();
    assert_eq!(run(&home, &unknown).reason.as_deref(), Some("interrupted"));
    first.kill_child(&survivor_session);
    first.kill_child(&unknown_session);
}

#[test]
fn reconcile_truth_table() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    write_org(home, 1);
    save(home, routine("digest", "*/5 * * * *"));
    let store = store(home);
    let cases = [
        ("live", Some(Liveness::Known(true)), "running", None),
        (
            "dead",
            Some(Liveness::Known(false)),
            "failed",
            Some("interrupted"),
        ),
        ("unknown", Some(Liveness::Unknown), "running", None),
        ("never-bound", None, "failed", Some("interrupted")),
    ];
    let sessions = FakeSessions::default();
    let mut ids = Vec::new();
    for (name, liveness, _, _) in &cases {
        let run_id = format!("run-{name}");
        let conn = rusqlite::Connection::open(default_store_path(home)).unwrap();
        conn.execute(
            "INSERT INTO routine_runs (run_id, routine_key, revision, source, occurrence_key,
                                       phase, session_id, created_at, started_at)
             VALUES (?1, 'acme/api/backend/digest', 1, 'manual', ?1, 'running', ?2, 0, 0)",
            rusqlite::params![run_id, liveness.as_ref().map(|_| format!("s-{name}"))],
        )
        .unwrap();
        if let Some(l) = liveness {
            sessions.set(&format!("s-{name}"), l.clone());
        }
        ids.push(run_id);
    }
    finish::reconcile_runs(&store, 1, &sessions, None).unwrap();
    for ((_, _, phase, reason), run_id) in cases.iter().zip(&ids) {
        let row = store.run(run_id).unwrap().unwrap();
        assert_eq!(
            (row.phase.as_str(), row.reason.as_deref()),
            (*phase, *reason),
            "{run_id}"
        );
    }
}

#[test]
fn the_first_ending_wins() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    write_org(home, 1);
    save(home, routine("digest", "*/5 * * * *"));
    let store = store(home);
    let start = |run_id: &str| {
        let conn = rusqlite::Connection::open(default_store_path(home)).unwrap();
        conn.execute(
            "INSERT INTO routine_runs (run_id, routine_key, revision, source, phase, created_at, started_at)
             VALUES (?1, 'acme/api/backend/digest', 1, 'manual', 'running', 0, 0)",
            [run_id],
        )
        .unwrap();
    };
    let signal = |summary: &str| Ending::Signal {
        status: SessionStatus::Success,
        summary: Some(summary.into()),
    };
    // Signal, then timeout, then exit.
    start("a");
    assert!(finish::finish(&store, "a", signal("done"), 5)
        .unwrap()
        .is_some());
    assert!(finish::finish(&store, "a", Ending::TimedOut, 6)
        .unwrap()
        .is_none());
    assert!(
        finish::finish(&store, "a", Ending::ENDED, 7)
            .unwrap()
            .is_none()
    );
    assert_eq!(store.run("a").unwrap().unwrap().phase, "succeeded");
    // Timeout, then signal, then exit.
    start("b");
    assert!(finish::finish(&store, "b", Ending::TimedOut, 5)
        .unwrap()
        .is_some());
    assert!(finish::finish(&store, "b", signal("late"), 6)
        .unwrap()
        .is_none());
    assert_eq!(store.run("b").unwrap().unwrap().phase, "timed_out");
    // A clean exit without a signal simply ends the run, with no result,
    // and keeps the last good summary.
    start("c");
    finish::finish(&store, "c", Ending::ENDED, 9).unwrap();
    let c = store.run("c").unwrap().unwrap();
    assert_eq!(
        (
            c.phase.as_str(),
            c.reason.as_deref(),
            c.result_text.as_deref()
        ),
        ("ended", None, None)
    );
    let state = store.state(&key("digest")).unwrap().unwrap();
    assert_eq!(state.last_run_id.as_deref(), Some("c"));
    assert_eq!(state.last_result_text.as_deref(), Some("done"));
}

#[test]
fn admission_policy_matrix() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    write_org(home, 1);
    let with = |id: &str, overlap: OverlapPolicy| {
        let mut def = routine(id, "*/5 * * * *");
        def.policy.overlap = overlap;
        save(home, def);
    };
    with("co", OverlapPolicy::Coalesce);
    with("sk", OverlapPolicy::Skip);
    with("qu", OverlapPolicy::Queue);
    with("pa", OverlapPolicy::Coalesce);

    // Paused: a manual fire records why it did not run.
    service::set_enabled(home, &alice(), &key("pa"), false).unwrap();
    assert_eq!(
        manual(home, "pa").outcome,
        Outcome::Skipped {
            reason: "paused".into()
        }
    );

    // Coalesce folds into the active run and launches nothing.
    let active = manual(home, "co");
    assert_eq!(active.outcome, Outcome::Queued);
    let folded = manual(home, "co");
    assert_eq!(
        folded.outcome,
        Outcome::Coalesced {
            active_run_id: active.run_id.clone()
        }
    );
    assert_eq!(
        run(home, &folded.run_id).linked_run_id.as_deref(),
        Some(active.run_id.as_str())
    );

    // Skip records already_running.
    assert_eq!(manual(home, "sk").outcome, Outcome::Queued);
    assert_eq!(
        manual(home, "sk").outcome,
        Outcome::Skipped {
            reason: "already_running".into()
        }
    );

    // Queue holds three, refuses the fourth, and releases them in order.
    let first = manual(home, "qu");
    let held: Vec<_> = (0..3).map(|_| manual(home, "qu")).collect();
    assert!(held.iter().all(|a| a.outcome == Outcome::Held));
    assert_eq!(
        manual(home, "qu").outcome,
        Outcome::Skipped {
            reason: "queue_full".into()
        }
    );
    let coordinator = coordinator(home);
    assert_eq!(coordinator.run_request_json(&held[0].run_id).unwrap(), None);
    let finished = finish::finish(&store(home), &first.run_id, Ending::Cancelled, 1)
        .unwrap()
        .unwrap();
    assert_eq!(finished.released.as_deref(), Some(held[0].run_id.as_str()));
    let released = run(home, &held[0].run_id);
    assert_eq!((released.phase.as_str(), released.reason), ("queued", None));
    let pointer = coordinator
        .run_request_json(&held[0].run_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        parse_pointer(&pointer).unwrap().unwrap().run_id,
        held[0].run_id
    );
    assert_eq!(run(home, &held[1].run_id).reason.as_deref(), Some("held"));

    // Only admitted runs are coordinator work, and none is a pending delivery.
    assert_eq!(
        coordinator.count_runs_in_state(RunState::Runnable).unwrap(),
        4,
        "co, sk, qu's first and its released successor"
    );
    assert_eq!(pending_routine_deliveries(home), 0);
}

#[test]
fn an_unavailable_target_is_skipped_with_no_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let org_root = write_org(home, 1);
    let mut repo = routine("repo", "*/5 * * * *");
    repo.target.kind = "repo:acme/not-cloned".into();
    save(home, repo);
    save(home, routine("team", "*/5 * * * *"));
    let skipped = Outcome::Skipped {
        reason: "target_unavailable".into(),
    };
    assert_eq!(manual(home, "repo").outcome, skipped);

    // A team workspace that is gone from disk is unavailable too.
    std::fs::remove_dir_all(crate::context_repo::path_to_team_content_dir(
        &org_root, TEAM,
    ))
    .unwrap();
    assert_eq!(manual(home, "team").outcome, skipped);
    assert_eq!(
        coordinator(home)
            .count_runs_in_state(RunState::Runnable)
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn a_run_that_cannot_start_fails_with_its_reason_and_launches_nothing() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    // The agent is there when the run is admitted (an Unassigned routine
    // never fires, SW-AGR-1) and gone by the time it dispatches.
    super::test_support::save_team_agent(
        &home,
        &alice(),
        super::test_support::agent(
            "no-such-profile",
            codesense_pty_core::agent_cli::AgentCli::Claude,
        ),
    );
    let mut def = routine("digest", "*/5 * * * *");
    def.agent = AgentRef::from("no-such-profile");
    save(&home, def);
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let run_id = manual(&home, "digest").run_id;
    let agent_dir = crate::context_repo::path_to_team_config_dir(
        &crate::context_repo::path_to_org_root(&home, &alice(), ORG).unwrap(),
        TEAM,
    )
    .join("agents/no-such-profile");
    std::fs::remove_file(agent_dir.join("agent.yaml")).unwrap();
    std::fs::remove_dir_all(agent_dir.join("revisions")).unwrap();
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    assert_eq!(launcher.calls(), 0);
    let row = run(&home, &run_id);
    assert_eq!(
        (row.phase.as_str(), row.reason.as_deref()),
        ("failed", Some("agent_unavailable"))
    );
    assert_eq!(row.result_text, None);
    assert!(row.detail.unwrap().contains("no-such-profile"));
}

#[tokio::test]
async fn stopping_a_run_cancels_it_and_kills_its_agent() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, routine("digest", "*/5 * * * *"));
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let run_id = manual(&home, "digest").run_id;
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    let session_id = launcher.last().session_id;

    assert!(service::stop_run(&home, &alice(), &run_id, launcher.as_ref()).unwrap());
    assert_eq!(run(&home, &run_id).phase, "cancelled");
    assert!(!launcher.is_running(&session_id));
    assert!(!service::stop_run(&home, &alice(), &run_id, launcher.as_ref()).unwrap());

    // A queued run stopped before dispatch never launches.
    let queued = manual(&home, "digest").run_id;
    service::stop_run(&home, &alice(), &queued, launcher.as_ref()).unwrap();
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    assert_eq!(launcher.calls(), 1);
    assert_eq!(run(&home, &queued).phase, "cancelled");
}

#[test]
fn a_routine_pointer_is_told_apart_from_an_automation_pointer() {
    assert!(parse_pointer(
        r#"{"cause":{"kind":"manual"},"trigger_source":"manual","fired_at":"x"}"#
    )
    .is_none());
    assert!(parse_pointer("not json").is_none());
    assert!(matches!(
        parse_pointer(r#"{"kind":"routine","run_id":"r"}"#),
        Some(Err(_))
    ));
}

#[test]
fn a_missed_instant_does_not_suppress_another_triggers_run_there() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    write_org(home, 1);
    let mut def = routine("digest", "0,30 9 * * *");
    def.policy.overlap = OverlapPolicy::Queue;
    def.triggers.push(RoutineTrigger::Schedule(ScheduleSpec {
        cron: "0 9 * * *".into(),
        tz: "UTC".into(),
    }));
    save(home, def);
    let store = store(home);
    let routines = super::discover::discover_routines(home, &alice()).unwrap();
    let account = (home.to_path_buf(), alice());
    let launch = ms("2026-10-05T08:00:00Z");
    super::tick::run_routine_tick_with(
        launch,
        launch,
        &account,
        &routines,
        &store,
        &RoutineAdmitter,
    )
    .unwrap();
    // The app slept past 09:00 and 09:30. The first trigger runs 09:30 and
    // records 09:00 as missed; the second trigger's own 09:00 still runs.
    let now = ms("2026-10-05T09:31:00Z");
    super::tick::run_routine_tick_with(now, launch, &account, &routines, &store, &RoutineAdmitter)
        .unwrap();
    let keys: Vec<(String, String)> = store
        .runs(&key("digest"))
        .unwrap()
        .into_iter()
        .map(|r| (r.occurrence_key.unwrap(), r.phase))
        .collect();
    let at = |t: &str| format!("acme/api/backend/digest@2026-10-05T{t}Z");
    assert!(
        keys.contains(&(at("09:30:00"), "queued".into())),
        "{keys:?}"
    );
    assert!(
        keys.contains(&(format!("{}#missed", at("09:00:00")), "skipped".into())),
        "{keys:?}"
    );
    assert!(
        keys.contains(&(at("09:00:00"), "queued".into())),
        "{keys:?}"
    );
}

#[test]
fn an_archived_routines_cursor_is_pruned() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    write_org(home, 1);
    save(home, routine("digest", "*/5 * * * *"));
    let store = store(home);
    let account = (home.to_path_buf(), alice());
    let now = ms("2026-10-05T08:00:00Z");
    let routines = super::discover::discover_routines(home, &alice()).unwrap();
    super::tick::run_routine_tick_with(now, now, &account, &routines, &store, &RoutineAdmitter)
        .unwrap();
    assert_eq!(store.triggers().unwrap().len(), 1);

    service::archive(home, &alice(), &key("digest"), &Author::user("alice")).unwrap();
    let routines = super::discover::discover_routines(home, &alice()).unwrap();
    super::tick::run_routine_tick_with(now, now, &account, &routines, &store, &RoutineAdmitter)
        .unwrap();
    assert!(store.triggers().unwrap().is_empty());
}

/// Two personal spaces on one Mac share the coordinator, and a schedule's
/// cursor is the Mac's. A wake from a clone that has not synced the routine yet
/// leaves the cursor alone; once it has, a switch of personal space carries
/// the schedule on, so no occurrence is lost or fired twice. Each run is
/// dispatched from the personal space of the wake that fired it, and either
/// one can stop it.
#[test]
fn a_team_routines_schedule_is_its_macs_across_personal_spaces() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    write_org(home, 1);
    let bob = crate::domain::Principal::from(2u64);
    let bob_org = write_org(home, 2);
    save(home, routine("digest", "0 9 * * *"));
    let store = store(home);
    let wake = |who: &crate::domain::Principal, now: i64| {
        let routines = super::discover::discover_routines(home, who).unwrap();
        super::tick::run_routine_tick_with(
            now,
            ms("2026-10-05T08:00:00Z"),
            &(home.to_path_buf(), who.clone()),
            &routines,
            &store,
            &RoutineAdmitter,
        )
        .unwrap()
    };
    let cursors = || -> Vec<(String, Option<i64>)> {
        store
            .triggers()
            .unwrap()
            .into_iter()
            .map(|t| (t.trigger_id, t.next_run_at))
            .collect()
    };
    let trigger = format!("{}#0", key("digest"));
    let seeded = vec![(trigger.clone(), Some(ms("2026-10-05T09:00:00Z")))];

    assert_eq!(wake(&alice(), ms("2026-10-05T08:00:00Z")).seeded, 1);
    assert_eq!(cursors(), seeded);

    // Bob's clone has not synced the routine yet.
    wake(&bob, ms("2026-10-05T08:10:00Z"));
    assert_eq!(cursors(), seeded, "a stale clone");

    // Bob's clone has it, armed on this Mac: his wake carries the cursor on.
    let def = service::get(home, &alice(), &key("digest")).unwrap().def;
    let bobs_copy = super::defs::sync_target(
        &super::service::routine_dir_of(home, &alice(), &key("digest")).unwrap(),
        &super::defs::routine_dir(
            &crate::context_repo::path_to_team_config_dir(&bob_org, TEAM),
            "digest",
        ),
    );
    super::defs::write_revision(
        &bobs_copy,
        &def,
        &Author::user("alice"),
        "2026-10-05T08:00:00Z",
    )
    .unwrap();
    let seen = super::discover::discover_routines(home, &bob).unwrap();
    assert!(seen.iter().any(|r| r.key == key("digest") && r.armed_here));
    assert_eq!(wake(&bob, ms("2026-10-05T08:20:00Z")).seeded, 0);
    assert_eq!(cursors(), seeded);

    // Alice is back for the 09:00 fire: it runs once, and Bob's next wake
    // finds nothing more to run.
    let fired = wake(&alice(), ms("2026-10-05T09:30:00Z"));
    assert_eq!((fired.seeded, fired.admitted), (0, 1));
    let first = fired.changed[0].1.clone();
    assert_eq!(
        run(home, &first).occurrence_key,
        Some(format!("{}@2026-10-05T09:00:00Z", key("digest")))
    );
    assert_eq!(pointer_principal(home, &first), "1");
    assert_eq!(wake(&bob, ms("2026-10-05T09:31:00Z")).admitted, 0);
    let launcher = FakeSessions::default();
    assert!(service::stop_run(home, &bob, &first, &launcher).unwrap());

    // Tuesday's fire comes on Bob's wake and is dispatched from his space.
    let fired = wake(&bob, ms("2026-10-06T09:00:10Z"));
    assert_eq!((fired.seeded, fired.admitted), (0, 1));
    let run_id = fired.changed[0].1.clone();
    assert_eq!(
        run(home, &run_id).occurrence_key,
        Some(format!("{}@2026-10-06T09:00:00Z", key("digest")))
    );
    assert_eq!(pointer_principal(home, &run_id), "2");

    // Either space stops the other's run.
    assert!(service::stop_run(home, &alice(), &run_id, &launcher).unwrap());
    assert_eq!(run(home, &run_id).phase, "cancelled");
}

/// The personal space a run's dispatch pointer names.
fn pointer_principal(home: &Path, run_id: &str) -> String {
    let json = coordinator(home)
        .run_request_json(run_id)
        .unwrap()
        .expect("a dispatch pointer");
    super::admit::parse_pointer(&json)
        .expect("a routine pointer")
        .unwrap()
        .principal
}

#[tokio::test]
async fn a_tab_closed_without_a_signal_ends_its_run_on_the_next_wake() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mut def = routine("digest", "*/5 * * * *");
    def.policy.visible = true;
    // A fresh run in a tab (a visible routine now defaults to wake).
    def.mode = "fresh".into();
    save(&home, def);
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let clock = Arc::new(AtomicI64::new(finish::now_ms() - MIN));
    let ticker = ticker(&home, &clock, launcher.clone());
    let run_id = manual(&home, "digest").run_id;
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    clock.store(finish::now_ms(), Ordering::SeqCst);
    ticker.tick().unwrap();
    assert_eq!(run(&home, &run_id).phase, "running");

    launcher.kill_child(&launcher.last().session_id);
    ticker.tick().unwrap();
    let row = run(&home, &run_id);
    assert_eq!(
        (
            row.phase.as_str(),
            row.reason.as_deref(),
            row.result_text.as_deref()
        ),
        ("ended", None, None)
    );
    // The next run is not refused behind the old session.
    assert_eq!(manual(&home, "digest").outcome, Outcome::Queued);
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    assert_eq!(launcher.calls(), 2);
    launcher.kill_child(&launcher.last().session_id);
}

#[tokio::test]
async fn a_restart_survivor_closed_without_a_signal_ends_on_the_next_wake() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mut def = routine("digest", "*/5 * * * *");
    def.policy.visible = true;
    // A fresh run in a tab (a visible routine now defaults to wake).
    def.mode = "fresh".into();
    save(&home, def);
    let first = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let run_id = manual(&home, "digest").run_id;
    assert_eq!(drain_claim_loop(&home, first.clone()).await, 1);
    let session_id = run(&home, &run_id).session_id.unwrap();

    // A fresh app, launched after the run started, finds its tab still open.
    let sessions = Arc::new(FakeSessions::default());
    sessions.set(&session_id, Liveness::Known(true));
    let clock = Arc::new(AtomicI64::new(finish::now_ms() + MIN));
    let ticker = ticker(&home, &clock, sessions.clone());
    let report =
        finish::reconcile_runs(&store(&home), finish::now_ms(), sessions.as_ref(), None).unwrap();
    assert_eq!(report.alive, vec![run_id.clone()]);
    ticker.watch_survivors(report.alive);
    ticker.tick().unwrap();
    assert_eq!(run(&home, &run_id).phase, "running");

    // The tab is closed without a signal: the next wake ends the run.
    sessions.set(&session_id, Liveness::Known(false));
    ticker.tick().unwrap();
    let row = run(&home, &run_id);
    assert_eq!(
        (
            row.phase.as_str(),
            row.reason.as_deref(),
            row.result_text.as_deref()
        ),
        ("ended", None, None)
    );
    first.kill_child(&session_id);
}

#[test]
fn another_accounts_cursors_survive_this_accounts_wake() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    write_org(home, 1);
    save(home, routine("digest", "*/5 * * * *"));
    let store = store(home);
    let now = ms("2026-10-05T08:00:00Z");
    let alice_account = (home.to_path_buf(), alice());
    let routines = super::discover::discover_routines(home, &alice()).unwrap();
    super::tick::run_routine_tick_with(
        now,
        now,
        &alice_account,
        &routines,
        &store,
        &RoutineAdmitter,
    )
    .unwrap();
    assert_eq!(store.triggers().unwrap().len(), 1);

    // Bob signs in on the same machine; he has not cloned Alice's org.
    let bob = crate::domain::Principal::from(2u64);
    let bob_routines = super::discover::discover_routines(home, &bob).unwrap();
    super::tick::run_routine_tick_with(
        now,
        now,
        &(home.to_path_buf(), bob),
        &bob_routines,
        &store,
        &RoutineAdmitter,
    )
    .unwrap();
    assert_eq!(store.triggers().unwrap().len(), 1);
}

/// Launches through the stub, but the run times out before the launch
/// returns, as when the watchdog wakes mid-launch.
struct TimedOutMidLaunch(Arc<StubLauncher>, std::path::PathBuf);

impl super::dispatch::Launcher for TimedOutMidLaunch {
    fn launch(&self, request: super::dispatch::LaunchRequest) -> super::dispatch::LaunchFuture<'_> {
        Box::pin(async move {
            let run_id = request.provenance.run_id.clone();
            let session_id = self.0.launch(request).await?;
            finish::finish(&store(&self.1), &run_id, Ending::TimedOut, finish::now_ms()).unwrap();
            Ok(session_id)
        })
    }

    fn kill_run(&self, run_id: &str) {
        super::dispatch::Launcher::kill_run(self.0.as_ref(), run_id);
    }
}

#[tokio::test]
async fn a_run_timed_out_while_it_launched_has_its_agent_killed() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, routine("digest", "*/5 * * * *"));
    let stub = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let run_id = manual(&home, "digest").run_id;
    let launcher = Arc::new(TimedOutMidLaunch(stub.clone(), home.clone()));
    assert_eq!(drain_claim_loop(&home, launcher).await, 1);
    let session_id = stub.last().session_id;
    assert_eq!(run(&home, &run_id).phase, "timed_out");
    assert!(!stub.is_running(&session_id), "no orphaned agent");
}

/// Production admission/dispatch/outcome/storage with a real stub child; no provider dependency.
#[tokio::test]
async fn structured_result_launch_publish_finish_cached_read_and_failure_isolation() {
    use super::results;
    use serde_json::json;
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mut def = routine("dashboard", "*/5 * * * *");
    def.task = "Collect the count, call routine_publish_result with your sessionId and JSON payload, then signal_done success.".into();
    let authored_task = def.task.clone();
    def.triggers.clear();
    def.result_schema = Some(
        json!({"type":"object","properties":{"count":{"type":"integer"}},"required":["count"],"additionalProperties":false}),
    );
    save(&home, def);
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    for _ in 0..3 {
        assert!(results::read(&home, &alice(), &key("dashboard"))
            .unwrap()
            .snapshot
            .is_none());
    }
    assert_eq!(launcher.calls(), 0);
    let first = manual(&home, "dashboard");
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    let session = launcher.last().session_id;
    assert_eq!(launcher.last().request.prompt, authored_task);
    assert!(results::publish(&home, &alice(), "other-session", json!({"count":1})).is_err());
    assert!(results::publish(
        &home,
        &crate::domain::Principal::from(2u64),
        &session,
        json!({"count":1})
    )
    .is_err());
    assert!(results::publish(&home, &alice(), &session, json!({"count":"bad"})).is_err());
    results::publish(&home, &alice(), &session, json!({"count":7})).unwrap();
    assert!(results::read(&home, &alice(), &key("dashboard"))
        .unwrap()
        .snapshot
        .is_none());
    launcher.signal_done(&session, "success", "Collected").await;
    assert_eq!(run(&home, &first.run_id).phase, "succeeded");
    let cached = results::read(&home, &alice(), &key("dashboard")).unwrap();
    assert!(cached.schema_compatible);
    assert_eq!(cached.snapshot.unwrap().payload, json!({"count":7}));
    // Shared routine definitions do not share private results across principals.
    let alice_org = crate::context_repo::path_to_org_root(&home, &alice(), ORG).unwrap();
    let bob_org = write_org(&home, 2);
    let routine_dir = |org: &std::path::Path| {
        super::defs::routine_dir(
            &crate::context_repo::path_to_team_config_dir(org, TEAM),
            "dashboard",
        )
    };
    let from = routine_dir(&alice_org);
    let to = super::defs::sync_target(&from, &routine_dir(&bob_org));
    std::fs::create_dir_all(&to).unwrap();
    for entry in std::fs::read_dir(&from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
    let foreign = results::read(
        &home,
        &crate::domain::Principal::from(2u64),
        &key("dashboard"),
    )
    .unwrap();
    assert!(foreign.snapshot.is_none());
    assert!(foreign.latest_attempt.is_none());
    assert!(results::publish(&home, &alice(), &session, json!({"count":9})).is_err());
    launcher.exit(&session).await;
    // A teammate may run the shared definition and cancel their own private
    // attempt; its creator cannot cancel that attempt or read its candidate.
    let bob = crate::domain::Principal::from(2u64);
    let bob_run = service::run_now(&home, &bob, &key("dashboard"), BTreeMap::new()).unwrap();
    assert_eq!(bob_run.outcome, Outcome::Queued);
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    let bob_session = launcher.last().session_id;
    results::publish(&home, &bob, &bob_session, json!({"count":99})).unwrap();
    assert!(results::publish(&home, &alice(), &bob_session, json!({"count":100})).is_err());
    assert_eq!(
        results::cancel(&home, &alice(), &key("dashboard"), launcher.as_ref()).unwrap(),
        0
    );
    assert_eq!(run(&home, &bob_run.run_id).phase, "running");
    assert_eq!(
        results::cancel(&home, &bob, &key("dashboard"), launcher.as_ref()).unwrap(),
        1
    );
    assert_eq!(run(&home, &bob_run.run_id).phase, "cancelled");
    assert!(results::read(&home, &bob, &key("dashboard"))
        .unwrap()
        .snapshot
        .is_none());
    assert_eq!(
        results::read(&home, &alice(), &key("dashboard"))
            .unwrap()
            .snapshot
            .unwrap()
            .payload,
        json!({"count":7})
    );
    launcher.exit(&bob_session).await;
    // Fresh store handles use persisted last-good data; cancellation and missing output cannot replace it.
    for ending in [
        None,
        Some(Ending::Cancelled),
        Some(Ending::TimedOut),
        Some(Ending::Interrupted),
        Some(Ending::Signal {
            status: SessionStatus::Failed,
            summary: None,
        }),
    ] {
        let next = manual(&home, "dashboard");
        drain_claim_loop(&home, launcher.clone()).await;
        let session = launcher.last().session_id;
        if let Some(ending) = ending {
            results::publish(&home, &alice(), &session, json!({"count":8})).unwrap();
            finish::finish(&store(&home), &next.run_id, ending, finish::now_ms()).unwrap();
            assert!(results::publish(&home, &alice(), &session, json!({"count":9})).is_err());
        } else {
            results::publish(&home, &alice(), &session, json!({"count":8})).unwrap();
            assert!(results::publish(
                &home,
                &alice(),
                &session,
                json!({"count":"invalid replacement"})
            )
            .is_err());
            launcher
                .signal_done(&session, "success", "Forgot output")
                .await;
            assert_eq!(
                run(&home, &next.run_id).reason.as_deref(),
                Some("result_missing")
            );
        }
        assert_eq!(
            results::read(&home, &alice(), &key("dashboard"))
                .unwrap()
                .snapshot
                .unwrap()
                .run_id,
            first.run_id
        );
        launcher.exit(&session).await;
    }
    let pinned = manual(&home, "dashboard");
    let mut changed = service::get_current(&home, &alice(), &key("dashboard"))
        .unwrap()
        .def;
    changed.result_schema = Some(json!({"type":"array"}));
    changed.runs_on = "each_mac".into();
    service::save(
        &home,
        &alice(),
        SaveTarget::Update {
            key: key("dashboard"),
            base_revision: 1,
        },
        changed,
        &Author::user("alice"),
    )
    .unwrap();
    let cached = results::read(&home, &alice(), &key("dashboard")).unwrap();
    assert!(!cached.schema_compatible);
    assert_eq!(cached.snapshot.unwrap().revision, 1);
    drain_claim_loop(&home, launcher.clone()).await;
    let session = launcher.last().session_id;
    results::publish(&home, &alice(), &session, json!({"count":11})).unwrap();
    launcher
        .signal_done(&session, "success", "Pinned schema")
        .await;
    let cached = results::read(&home, &alice(), &key("dashboard")).unwrap();
    assert!(!cached.schema_compatible);
    assert_eq!(cached.snapshot.unwrap().run_id, pinned.run_id);
    launcher.exit(&session).await;
}

#[tokio::test]
async fn result_cancel_atomically_stops_active_and_held_without_launching_them() {
    use super::results;
    use serde_json::json;
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mut def = routine("cancel-board", "*/5 * * * *");
    def.result_schema = Some(json!(true));
    def.policy.overlap = OverlapPolicy::Queue;
    save(&home, def);
    save(&home, routine("unrelated", "*/5 * * * *"));
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let initial = manual(&home, "cancel-board");
    drain_claim_loop(&home, launcher.clone()).await;
    let session = launcher.last().session_id;
    results::publish(&home, &alice(), &session, json!({"kept":true})).unwrap();
    launcher.signal_done(&session, "success", "Initial").await;
    launcher.exit(&session).await;
    let active = manual(&home, "cancel-board");
    drain_claim_loop(&home, launcher.clone()).await;
    let active_session = launcher.last().session_id;
    let held = manual(&home, "cancel-board");
    assert_eq!(held.outcome, Outcome::Held);
    let unrelated = manual(&home, "unrelated");
    let status = results::read(&home, &alice(), &key("cancel-board")).unwrap();
    assert_eq!(status.latest_attempt.unwrap().run_id, held.run_id);
    assert_eq!(status.active_attempt.unwrap().run_id, active.run_id);
    struct CancelObserver<'a> {
        home: &'a Path,
        ids: Vec<String>,
        launcher: &'a StubLauncher,
    }
    impl finish::Sessions for CancelObserver<'_> {
        fn kill_run(&self, run_id: &str, session_id: Option<&str>) {
            // At the very first external effect all selected rows are already terminal.
            for id in &self.ids {
                assert_eq!(run(self.home, id).phase, "cancelled");
            }
            finish::Sessions::kill_run(self.launcher, run_id, session_id);
        }
        fn liveness(&self, session_id: &str) -> Liveness<bool> {
            finish::Sessions::liveness(self.launcher, session_id)
        }
    }
    let sessions = CancelObserver {
        home: &home,
        ids: vec![active.run_id.clone(), held.run_id.clone()],
        launcher: &launcher,
    };
    // Detached capture owns these facts before cancellation releases the live registry entry.
    let launched = launcher.last();
    let write = crate::commands::dashboard_launch::compose_launch_write(
        &launcher.registry, &active_session, &launched.request.handle, Default::default(),
    ).unwrap();
    let usage = crate::session_usage::UsageStore::open(&home.join("usage.sqlite")).unwrap();
    assert_eq!(
        results::cancel(&home, &alice(), &key("cancel-board"), &sessions).unwrap(),
        2
    );
    // A cancellation closes the run's task through the same funnel as every other ending.
    let cancelled = usage.read_task(&active.run_id).unwrap().unwrap();
    assert_eq!(
        (cancelled.status.as_str(), cancelled.raw_status.as_deref()),
        ("abandoned", Some("cancelled"))
    );
    // Launch capture arrives after cancellation; it enriches without reopening the row.
    let capture = || crate::session_usage::task_capture::capture_launch_task(
        &usage,
        &crate::session_usage::task_capture::Capture {
            session_id: &active_session, record: &write.record, inputs: &write.task,
            routine: write.routine.as_ref(), launcher_task: write.launcher.as_deref(),
            story_file: None, now_ms: finish::now_ms(),
        },
    ).unwrap();
    let enriched = capture();
    assert_eq!(enriched.status, "abandoned");
    assert_eq!(enriched.closed_at_ms, cancelled.closed_at_ms);
    assert_eq!(capture(), enriched, "duplicate capture is inert");
    let reopened = crate::session_usage::UsageStore::open(&home.join("usage.sqlite")).unwrap();
    assert_eq!(reopened.read_task(&active.run_id).unwrap().unwrap(), enriched);
    assert!(reopened.read_task(&held.run_id).unwrap().is_none(), "unstarted run has no task");
    assert_eq!(
        results::cancel(&home, &alice(), &key("cancel-board"), &sessions).unwrap(),
        0
    );
    assert!(results::publish(&home, &alice(), &active_session, json!({"late":true})).is_err());
    assert_eq!(run(&home, &unrelated.run_id).phase, "queued");
    let calls = launcher.calls();
    drain_claim_loop(&home, launcher.clone()).await;
    assert_eq!(
        launcher.calls(),
        calls + 1,
        "only unrelated routine launches"
    );
    let status = results::read(&home, &alice(), &key("cancel-board")).unwrap();
    assert!(status.active_attempt.is_none());
    assert_eq!(status.snapshot.unwrap().run_id, initial.run_id);
    launcher.exit(&launcher.last().session_id).await;
}

#[tokio::test]
async fn result_active_attempt_survives_coalesced_and_skipped_latest_fires() {
    use super::results;
    use serde_json::json;
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    for (id, policy) in [
        ("coalesced-board", OverlapPolicy::Coalesce),
        ("skipped-board", OverlapPolicy::Skip),
    ] {
        let mut def = routine(id, "*/5 * * * *");
        def.result_schema = Some(json!(true));
        def.policy.overlap = policy;
        save(&home, def);
        let active = manual(&home, id);
        drain_claim_loop(&home, launcher.clone()).await;
        let latest = manual(&home, id);
        let status = results::read(&home, &alice(), &key(id)).unwrap();
        assert_eq!(status.active_attempt.unwrap().run_id, active.run_id);
        assert_eq!(status.latest_attempt.unwrap().run_id, latest.run_id);
        results::cancel(&home, &alice(), &key(id), launcher.as_ref()).unwrap();
    }
}

/// Resolves the account at the same boundary as both desktop launch spines,
/// then starts the existing stub process and real completion funnel.
struct AccountResolvingLauncher {
    app: tauri::AppHandle<tauri::test::MockRuntime>,
    stub: Arc<StubLauncher>,
}

impl super::dispatch::Launcher for AccountResolvingLauncher {
    fn launch(&self, request: super::dispatch::LaunchRequest) -> super::dispatch::LaunchFuture<'_> {
        Box::pin(async move {
            let (resolved, _) = crate::commands::agent_accounts::resolve_for_launch(
                &self.app,
                request.agent.account_input(),
            )
            .await?;
            assert!(resolved.spawn_eligible);
            self.stub.launch(request).await
        })
    }

    fn kill_run(&self, run_id: &str) {
        super::dispatch::Launcher::kill_run(self.stub.as_ref(), run_id);
    }
}

#[tokio::test]
async fn scheduled_dispatch_resolves_launch_account_without_selected_team() {
    use crate::mcp::state::{ActiveTeam, ActiveTeamState};
    use tauri::Manager;

    let temp = TempHome::new();
    let home = temp.codesense_home();
    let app = super::test_support::launch_app(&home);
    let selected = app.state::<ActiveTeamState>();
    let stub = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let launcher = Arc::new(AccountResolvingLauncher {
        app: app.handle().clone(),
        stub: stub.clone(),
    });
    for visible in [false, true] {
        let mut def = routine(if visible { "visible" } else { "headless" }, "*/5 * * * *");
        def.policy.visible = visible;
        def.mode = super::model::RoutineMode::Fresh.as_str().into();
        save(&home, def);
    }
    let clock = Arc::new(AtomicI64::new(ms("2026-10-05T09:01:00Z")));
    let ticker = ticker(&home, &clock, stub.clone());
    ticker.tick().unwrap();
    for (at, selection) in [
        ("2026-10-05T09:05:10Z", None),
        ("2026-10-05T09:10:10Z", Some((ORG, "other-team"))),
        ("2026-10-05T09:15:10Z", Some(("other/org", "elsewhere"))),
    ] {
        match selection {
            Some((org, team)) => selected.update(ActiveTeam {
                org_slug: org.into(),
                team_name: team.into(),
            }),
            None => selected.clear(),
        }
        clock.store(ms(at), Ordering::SeqCst);
        let report = ticker.tick().unwrap();
        assert_eq!(report.admitted, 2);
        assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 2);
        for (_, run_id) in report.changed {
            let row = run(&home, &run_id);
            assert_ne!(row.reason.as_deref(), Some("launch_failed"), "{row:?}");
            assert_eq!(row.phase, "running", "{row:?}");
            let session = row.session_id.unwrap();
            let launched = stub
                .launched()
                .into_iter()
                .find(|launch| launch.session_id == session)
                .unwrap();
            assert_eq!(launched.request.org, ORG);
            assert_eq!(launched.request.team, TEAM);
            assert_eq!(
                launched.request.provenance.routine_key.as_deref(),
                Some(row.routine_key.as_str())
            );
            // The run's task is made from the routine the dispatch carried, visible or not.
            let brief = launched
                .request
                .provenance
                .routine
                .clone()
                .expect("the routine rides the run");
            assert!(brief.name.starts_with("Routine "), "{brief:?}");
            let write = crate::commands::dashboard_launch::compose_launch_write(
                &stub.registry,
                &session,
                &launched.request.handle,
                Default::default(),
            )
            .expect("the stub registered the launch");
            let usage = crate::session_usage::UsageStore::open(&home.join("usage.sqlite")).unwrap();
            let task = crate::session_usage::task_capture::capture_launch_task(
                &usage,
                &crate::session_usage::task_capture::Capture {
                    session_id: &session,
                    record: &write.record,
                    inputs: &write.task,
                    routine: write.routine.as_ref(),
                    launcher_task: write.launcher.as_deref(),
                    story_file: None,
                    now_ms: 1,
                },
            )
            .unwrap();
            assert_eq!(
                task.task_id, run_id,
                "visible: {}",
                launched.request.visible
            );
            assert_eq!(
                (task.task_type.as_str(), task.parent_id.as_deref()),
                ("routine", None)
            );
            assert!(task.goal.starts_with(&brief.name), "{}", task.goal);
            selected.clear();
            assert_eq!(ticker.tick().unwrap().admitted, 0);
            assert_eq!(run(&home, &run_id).phase, "running");
            stub.signal_done(&session, "success", "completed in routine team")
                .await;
            stub.exit(&session).await;
            assert_eq!(run(&home, &run_id).phase, "succeeded");
            // The run's ending closed its task, in the usage store of the same home.
            let closed = usage.read_task(&run_id).unwrap().unwrap();
            assert_eq!(
                (closed.status.as_str(), closed.raw_status.as_deref()),
                ("done", Some("succeeded"))
            );
        }
    }
}

#[test]
fn a_budget_refusal_records_a_skipped_run_instead_of_dispatching_a_wake() {
    use crate::session_usage::{event::Tokens, spend_caps::Budget, tests::event, UsageStore};
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mut def = routine("budget", "0 * * * *");
    def.policy.budget_usd = Some(Budget {
        per_day: Some(1.0),
        per_week: None,
    });
    save(&home, def);
    let mut usage = UsageStore::open(&home.join("usage.sqlite")).unwrap();
    usage
        .attribute(
            "spent",
            ORG,
            Some(TEAM),
            "agent",
            &format!("routine:{}@one", key("budget")),
            None,
        )
        .unwrap();
    let mut call = event(
        "e",
        "spent",
        "claude-opus-5-5",
        chrono::Utc::now().timestamp_millis(),
        Tokens::default(),
    );
    call.reported_cost_usd = Some(1.0);
    usage.ingest(&[call]).unwrap();
    let result = manual(&home, "budget");
    assert_eq!(
        result.outcome,
        Outcome::Skipped {
            reason: "budget_reached".into()
        }
    );
    let saved = run(&home, &result.run_id);
    assert_eq!(saved.phase, "skipped");
    assert_eq!(saved.reason.as_deref(), Some("budget_reached"));
    assert!(saved
        .result_text
        .as_deref()
        .unwrap()
        .contains("cap $1.0000"));
}

#[tokio::test]
async fn held_runs_recheck_the_current_routine_and_agent_cap_before_dispatch() {
    use crate::session_usage::{event::Tokens, spend_caps::Budget, tests::event, UsageStore};
    for agent_cap in [false, true] {
        let temp = TempHome::new();
        let home = temp.codesense_home();
        let mut def = routine("queued-budget", "0 * * * *");
        def.policy.overlap = OverlapPolicy::Queue;
        if !agent_cap {
            def.policy.budget_usd = Some(Budget {
                per_day: Some(1.0),
                per_week: None,
            });
        }
        save(&home, def);
        let agent_key = crate::agents::AgentKey::new(ORG, TEAM, "default");
        if agent_cap {
            let mut agent = crate::agents::service::get(&home, &alice(), &agent_key)
                .unwrap()
                .agent;
            agent.budget_usd = Some(Budget {
                per_day: Some(1.0),
                per_week: None,
            });
            crate::agents::service::save(
                &home,
                &alice(),
                crate::agents::service::SaveTarget::Update {
                    key: agent_key.clone(),
                    base_revision: agent.revision,
                },
                agent,
                &Author::user("alice"),
            )
            .unwrap();
        }
        let first = manual(&home, "queued-budget");
        let held = manual(&home, "queued-budget");
        assert_eq!(held.outcome, Outcome::Held);
        let mut usage = UsageStore::open(&home.join("usage.sqlite")).unwrap();
        usage
            .attribute(
                "spent",
                ORG,
                Some(TEAM),
                &agent_key.to_string(),
                &format!("routine:{}@one", key("queued-budget")),
                None,
            )
            .unwrap();
        let mut call = event(
            "past-cap",
            "spent",
            "claude-opus-5-5",
            chrono::Utc::now().timestamp_millis(),
            Tokens::default(),
        );
        call.reported_cost_usd = Some(1.0);
        usage.ingest(&[call]).unwrap();
        finish::finish(
            &store(&home),
            &first.run_id,
            Ending::Cancelled,
            finish::now_ms(),
        )
        .unwrap();
        let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
        drain_claim_loop(&home, launcher.clone()).await;
        let result = run(&home, &held.run_id);
        assert_eq!(result.phase, "skipped");
        assert_eq!(result.reason.as_deref(), Some("budget_reached"));
        assert_eq!(launcher.calls(), 0);
    }
}

#[tokio::test]
async fn queued_caps_follow_the_resolved_agent_after_the_routine_moves() {
    use crate::session_usage::{event::Tokens, spend_caps::Budget, tests::event, UsageStore};
    for capped_original in [false, true] {
        let temp = TempHome::new();
        let home = temp.codesense_home();
        let mut def = routine("changed-agent", "0 * * * *");
        def.policy.overlap = OverlapPolicy::Queue;
        save(&home, def);
        let first = manual(&home, "changed-agent");
        let held = manual(&home, "changed-agent");
        let original_key = crate::agents::AgentKey::new(ORG, TEAM, "default");
        let mut replacement = crate::agents::service::get(&home, &alice(), &original_key)
            .unwrap()
            .agent;
        replacement.id = "replacement".into();
        replacement.name = "Replacement".into();
        replacement.budget_usd = (!capped_original).then_some(Budget {
            per_day: Some(1.0),
            per_week: None,
        });
        let replacement = crate::agents::service::save(
            &home,
            &alice(),
            crate::agents::service::SaveTarget::Create(crate::agents::service::Placement::Team {
                org: ORG.into(),
                team: TEAM.into(),
            }),
            replacement,
            &Author::user("alice"),
        )
        .unwrap();
        if capped_original {
            let mut original = crate::agents::service::get(&home, &alice(), &original_key)
                .unwrap()
                .agent;
            original.budget_usd = Some(Budget {
                per_day: Some(1.0),
                per_week: None,
            });
            crate::agents::service::save(
                &home,
                &alice(),
                crate::agents::service::SaveTarget::Update {
                    key: original_key.clone(),
                    base_revision: original.revision,
                },
                original,
                &Author::user("alice"),
            )
            .unwrap();
        }
        let mut current = service::get_current(&home, &alice(), &key("changed-agent"))
            .unwrap()
            .def;
        let revision = current.revision;
        current.agent = AgentRef::from("replacement");
        service::save(
            &home,
            &alice(),
            SaveTarget::Update {
                key: key("changed-agent"),
                base_revision: revision,
            },
            current,
            &Author::user("alice"),
        )
        .unwrap();
        let spent_key = if capped_original {
            original_key.to_string()
        } else {
            replacement.key.to_string()
        };
        let mut usage = UsageStore::open(&home.join("usage.sqlite")).unwrap();
        usage
            .attribute("spent", ORG, Some(TEAM), &spent_key, "h", None)
            .unwrap();
        let mut call = event(
            "spent",
            "spent",
            "claude-opus-5-5",
            chrono::Utc::now().timestamp_millis(),
            Tokens::default(),
        );
        call.reported_cost_usd = Some(1.0);
        usage.ingest(&[call]).unwrap();
        finish::finish(
            &store(&home),
            &first.run_id,
            Ending::Cancelled,
            finish::now_ms(),
        )
        .unwrap();
        let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
        drain_claim_loop(&home, launcher.clone()).await;
        // Agent-owned revision files inherit the new home after a routine moves.
        // The gate must govern the same resolved agent the queued run will launch.
        let result = run(&home, &held.run_id);
        assert_eq!(
            result.phase,
            if !capped_original {
                "skipped"
            } else {
                "running"
            }
        );
        assert_eq!(launcher.calls(), usize::from(capped_original));
        if !capped_original {
            assert_eq!(result.reason.as_deref(), Some("budget_reached"));
        }
    }
}

#[test]
fn pending_cap_removal_keeps_wake_and_machine_restrictions_until_native_acceptance() {
    use crate::session_usage::spend_caps::{self, Budget};
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mut def = routine("pending-arm", "0 * * * *");
    def.policy.budget_usd = Some(Budget {
        per_day: Some(1.0),
        per_week: None,
    });
    save(&home, def);
    let key = key("pending-arm");
    let mut def = service::get_current(&home, &alice(), &key).unwrap().def;
    let revision = def.revision;
    def.policy.budget_usd = None;
    service::save(
        &home,
        &alice(),
        SaveTarget::Update {
            key: key.clone(),
            base_revision: revision,
        },
        def,
        &Author::user("alice"),
    )
    .unwrap();
    let mut wake = service::get_current(&home, &alice(), &key).unwrap().def;
    let revision = wake.revision;
    wake.mode = "wake".into();
    assert!(spend_caps::refuse_routine(&home, &alice(), &key, &wake)
        .unwrap_err()
        .contains("cap the agent"));
    let error = service::save(
        &home,
        &alice(),
        SaveTarget::Update {
            key: key.clone(),
            base_revision: revision,
        },
        wake,
        &Author::user("alice"),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("cap the agent"), "{error}");
    let arm = || {
        service::arm_on(
            &home,
            &alice(),
            &key,
            service::ArmPlacement::Machine("remote-test".into()),
            &Author::user("alice"),
        )
    };
    let error = arm().unwrap_err().to_string();
    assert!(error.contains("cannot have a spend cap yet"), "{error}");
    spend_caps::accept_current(&home, &alice(), "routine", &key.to_string(), None).unwrap();
    assert!(arm().is_ok());
}

#[test]
fn a_routine_only_cap_exposes_pending_rates_and_admits_after_native_acceptance() {
    use crate::session_usage::{
        event::Tokens,
        spend_caps::{self, Budget},
        tests::event,
        SetModelRateParams, UsageStore, SET_BY_AGENT,
    };
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mut def = routine("rate-cap", "0 * * * *");
    def.policy.budget_usd = Some(Budget {
        per_day: Some(1.0),
        per_week: None,
    });
    save(&home, def);
    let agent = crate::agents::AgentKey::new(ORG, TEAM, "default").to_string();
    let routine_key = key("rate-cap").to_string();
    let mut usage = UsageStore::open(&home.join("usage.sqlite")).unwrap();
    usage
        .attribute(
            "routine-rate",
            ORG,
            Some(TEAM),
            &agent,
            &format!("routine:{routine_key}@one"),
            None,
        )
        .unwrap();
    usage
        .ingest(&[event(
            "routine-unknown",
            "routine-rate",
            "routine-unknown-model",
            chrono::Utc::now().timestamp_millis(),
            Tokens {
                input: 1000,
                ..Default::default()
            },
        )])
        .unwrap();
    assert!(
        matches!(manual(&home, "rate-cap").outcome, Outcome::Skipped { reason } if reason == "budget_reached")
    );
    let request = SetModelRateParams {
        model_prefix: "routine-unknown-model".into(),
        effective_from: chrono::Local::now().date_naive().to_string(),
        input_per_mtok: 1.25,
        output_per_mtok: 7.5,
        cache_read_per_mtok: Some(0.125),
        long: None,
    };
    assert_eq!(
        usage.request_rate(request.clone(), SET_BY_AGENT).unwrap(),
        0
    );
    let value = spend_caps::evaluate(&home, &alice()).unwrap();
    assert!(value.agents[&agent].pending_rates.is_empty());
    assert_eq!(
        value.routines[&routine_key].pending_rates,
        vec![request.clone()]
    );
    #[derive(Default)]
    struct Notices(std::sync::Mutex<Vec<super::wake::WakeNotice>>);
    impl super::wake::WakeNotify for Notices {
        fn post(&self, notice: &super::wake::WakeNotice) {
            self.0.lock().unwrap().push(notice.clone());
        }
        fn redispatch(&self, _: super::admit::RoutinePointer, _: std::time::Duration) {}
    }
    let notices = Notices::default();
    spend_caps::notify_evaluation(&usage, &value, &notices).unwrap();
    assert!(notices.0.lock().unwrap().iter().any(|n| n.routine_key
        == format!("{agent}#routine:{routine_key}")
        && n.detail.contains("input $1.25/M")));

    assert!(
        matches!(manual(&home, "rate-cap").outcome, Outcome::Skipped { reason } if reason == "budget_reached")
    );
    usage
        .accept_rate(&request.model_prefix, &request.effective_from, &request)
        .unwrap();
    assert!(
        spend_caps::evaluate(&home, &alice()).unwrap().routines[&routine_key]
            .pending_rates
            .is_empty()
    );
    assert_eq!(manual(&home, "rate-cap").outcome, Outcome::Queued);
}

/// A saved OpenCode agent's headless run: its stdout is the `run --format json` stream.
mod opencode_headless {
    use super::*;
    use crate::routines::test_support::{OPENCODE_ERROR_RUN, OPENCODE_TEXT_RUN};
    use codesense_pty_core::agent_cli::TranscriptFormat;

    const FORMAT: Option<TranscriptFormat> = Some(TranscriptFormat::OpenCodeRunJson);

    async fn run_to_exit(
        id: &str,
        signal: Option<(&str, &str)>,
        code: i32,
        stdout: &str,
    ) -> (TempHome, std::path::PathBuf, String) {
        let temp = TempHome::new();
        let home = temp.codesense_home();
        save(&home, routine(id, "*/5 * * * *"));
        let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
        let run_id = manual(&home, id).run_id;
        assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
        let session = launcher.last().session_id;
        if let Some((status, text)) = signal {
            launcher.signal_done(&session, status, text).await;
        }
        launcher
            .exit_structured(&session, FORMAT, exit_code(code), stdout, "")
            .await;
        (temp, home, run_id)
    }

    #[tokio::test]
    async fn a_clean_exit_without_a_signal_ends_and_its_final_text_is_not_a_result() {
        let (_temp, home, run_id) = run_to_exit("oc-quiet", None, 0, OPENCODE_TEXT_RUN).await;
        let row = run(&home, &run_id);
        assert_eq!(row.phase, "ended");
        assert_eq!(row.result_text, None, "stdout is not a result");
    }

    #[tokio::test]
    async fn signal_done_gives_the_result_whatever_the_stream_says() {
        let (_temp, home, run_id) =
            run_to_exit("oc-signal", Some(("success", "X")), 0, OPENCODE_TEXT_RUN).await;
        let row = run(&home, &run_id);
        assert_eq!(row.phase, "succeeded");
        assert_eq!(row.result_text.as_deref(), Some("X"));
    }

    #[tokio::test]
    async fn a_non_zero_exit_fails_with_the_error_message() {
        let (_temp, home, run_id) = run_to_exit("oc-bad", None, 1, OPENCODE_ERROR_RUN).await;
        let row = run(&home, &run_id);
        assert_eq!(
            (row.phase.as_str(), row.result_text.as_deref()),
            ("failed", Some("fake bad request"))
        );
    }

    #[tokio::test]
    async fn an_error_event_fails_the_run_even_when_the_process_exited_zero() {
        let (_temp, home, run_id) = run_to_exit("oc-zero", None, 0, OPENCODE_ERROR_RUN).await;
        let row = run(&home, &run_id);
        assert_eq!(
            (
                row.phase.as_str(),
                row.reason.as_deref(),
                row.result_text.as_deref()
            ),
            (
                "failed",
                Some("exited_with_error"),
                Some("fake bad request")
            )
        );
    }

    #[tokio::test]
    async fn stopping_a_run_that_already_printed_its_final_text_cancels_it() {
        let temp = TempHome::new();
        let home = temp.codesense_home();
        save(&home, routine("oc-stop", "*/5 * * * *"));
        let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
        let run_id = manual(&home, "oc-stop").run_id;
        assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
        launcher
            .capture_stopped_structured(&launcher.last().session_id, FORMAT, OPENCODE_TEXT_RUN)
            .await;
        let row = run(&home, &run_id);
        assert_eq!(
            (row.phase.as_str(), row.reason.as_deref()),
            ("cancelled", None)
        );
    }
}

/// A saved Hermes agent's headless run: its stdout is the `chat -Q --format stream-json`
/// stream, and its process output is never the routine's result text. One table, every
/// input row against every state column.
mod hermes_headless {
    use super::*;
    use crate::commands::dashboard_launch::headless_run_outcome;
    use crate::shared::proc_capture::CaptureOutput;
    use crate::routines::test_support::{
        HERMES_DEADLINE_RUN, HERMES_DENY_RUN, HERMES_ERROR_RUN, HERMES_RECOVERED_RUN,
        HERMES_TEXT_RUN,
    };
    use codesense_pty_core::agent_cli::TranscriptFormat;

    const FORMAT: Option<TranscriptFormat> = Some(TranscriptFormat::HermesStreamJson);

    #[derive(Clone, Copy)]
    enum How {
        Exit(i32),
        TimedOut,
        Stopped,
    }

    /// What a process-derived ending leaves: the capture's wire status, the routine row.
    type Ended = (&'static str, &'static str, Option<&'static str>);
    const SUCCESS: Ended = ("success", "ended", None);
    const FAILED: Ended = ("failed", "failed", Some("exited_with_error"));
    const TIMED_OUT: Ended = ("needs-attention", "timed_out", None);
    const STOPPED: Ended = ("failed", "cancelled", None);

    struct Row {
        input: &'static str,
        stdout: String,
        how: How,
        ended: Ended,
        /// The agent's own `signal_done`, which owns the verdict when it comes first.
        signal: Option<(&'static str, &'static str)>,
    }

    fn without_result(raw: &str) -> String {
        raw.lines()
            .filter(|line| !line.contains("\"type\": \"result\""))
            .map(|line| format!("{line}\n"))
            .collect()
    }

    fn row(input: &'static str, stdout: &str, how: How, ended: Ended) -> Row {
        Row { input, stdout: stdout.into(), how, ended, signal: None }
    }

    fn table() -> Vec<Row> {
        let signalled = |input, status, text| Row {
            signal: Some((status, text)),
            ..row(input, HERMES_TEXT_RUN, How::Exit(0), SUCCESS)
        };
        vec![
            row("result 0, process 0", HERMES_TEXT_RUN, How::Exit(0), SUCCESS),
            row("tool denied, then result 0", HERMES_DENY_RUN, How::Exit(0), SUCCESS),
            row("missing result, exit 0", &without_result(HERMES_TEXT_RUN), How::Exit(0), FAILED),
            row("missing result, exit 1", &without_result(HERMES_TEXT_RUN), How::Exit(1), FAILED),
            row("non-zero process, clean result", HERMES_TEXT_RUN, How::Exit(1), FAILED),
            row(
                "non-zero result exit, process 0",
                &HERMES_TEXT_RUN.replace("\"exit_code\": 0", "\"exit_code\": 2"),
                How::Exit(0),
                FAILED,
            ),
            row("terminal result.error, process 1", HERMES_ERROR_RUN, How::Exit(1), FAILED),
            row("terminal result.error, process 0", HERMES_ERROR_RUN, How::Exit(0), FAILED),
            row("recovered API error, clean result", HERMES_RECOVERED_RUN, How::Exit(0), SUCCESS),
            row("deadline", HERMES_DEADLINE_RUN, How::TimedOut, TIMED_OUT),
            row("explicit stop", HERMES_TEXT_RUN, How::Stopped, STOPPED),
            signalled("signal_done success", "success", "signalled text"),
            signalled("signal_done failed", "failed", "signalled failure"),
        ]
    }

    fn capture(row: &Row) -> CaptureOutput {
        let (status, timed_out, cancelled) = match row.how {
            How::Exit(code) => (Some(exit_code(code)), false, false),
            How::TimedOut => (None, true, false),
            How::Stopped => (Some(killed_by(15)), false, true),
        };
        CaptureOutput {
            status,
            stdout: row.stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
            timed_out,
            cancelled,
        }
    }

    struct Played {
        _temp: TempHome,
        home: std::path::PathBuf,
        launcher: Arc<StubLauncher>,
        run_id: String,
        session: String,
    }

    async fn start(id: &str) -> Played {
        let temp = TempHome::new();
        let home = temp.codesense_home();
        save(&home, routine(id, "*/5 * * * *"));
        let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
        let run_id = manual(&home, id).run_id;
        assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
        let session = launcher.last().session_id;
        Played { _temp: temp, home, launcher, run_id, session }
    }

    impl Played {
        async fn end(&self, row: &Row) {
            let stdout = row.stdout.as_str();
            match row.how {
                How::Exit(code) => {
                    self.launcher
                        .exit_structured(&self.session, FORMAT, exit_code(code), stdout, "")
                        .await
                }
                How::TimedOut => {
                    self.launcher.capture_timed_out_structured(&self.session, FORMAT, stdout).await
                }
                How::Stopped => {
                    self.launcher.capture_stopped_structured(&self.session, FORMAT, stdout).await
                }
            }
        }

        fn seen(&self) -> (String, Option<String>, Option<String>) {
            let row = run(&self.home, &self.run_id);
            (row.phase, row.reason, row.result_text)
        }
    }

    fn assert_row(played: &Played, phase: &str, reason: Option<&str>, text: Option<&str>, at: &str) {
        let (p, r, t) = played.seen();
        assert_eq!((p.as_str(), r.as_deref(), t.as_deref()), (phase, reason, text), "{at}");
    }

    #[tokio::test]
    async fn every_input_has_its_status_and_provenance_in_every_state() {
        for row in table() {
            let at = |column: &str| format!("{} / {column}", row.input);
            let (wire, phase, reason) = row.ended;
            // The capture's own verdict, before any routine row.
            assert_eq!(headless_run_outcome(FORMAT, &Ok(capture(&row))).0, wire, "{}", at("wire"));

            // Pending: nothing else has ended the run.
            {
                let played = start("hm-pending").await;
                if let Some((status, text)) = row.signal {
                    played.launcher.signal_done(&played.session, status, text).await;
                }
                played.end(&row).await;
                match row.signal {
                    Some(("success", text)) => assert_row(&played, "succeeded", None, Some(text), &at("pending")),
                    Some((_, text)) => assert_row(&played, "failed", None, Some(text), &at("pending")),
                    None => assert_row(&played, phase, reason, None, &at("pending")),
                }
            }

            // Concurrent signal, process first: the first ending keeps the row.
            {
                let played = start("hm-process-first").await;
                played.end(&row).await;
                played.launcher.signal_done(&played.session, "success", "late").await;
                assert_row(&played, phase, reason, None, &at("process first"));
            }

            // Concurrent signal, signal first: the signal's verdict and text stand.
            if row.signal.is_none() {
                let played = start("hm-signal-first").await;
                played.launcher.signal_done(&played.session, "success", "early").await;
                played.end(&row).await;
                assert_row(&played, "succeeded", None, Some("early"), &at("signal first"));
            }

            // Terminal success / error: a later process input changes nothing.
            for (ending, phase, text) in [
                (Ending::Signal { status: SessionStatus::Success, summary: Some("T".into()) }, "succeeded", "T"),
                (Ending::Signal { status: SessionStatus::Failed, summary: Some("E".into()) }, "failed", "E"),
            ] {
                let played = start("hm-terminal").await;
                finish::finish(&store(&played.home), &played.run_id, ending, finish::now_ms()).unwrap();
                played.end(&row).await;
                assert_row(&played, phase, None, Some(text), &at("terminal"));
            }

            // Restart / replay: the persisted ending reloads, and neither the same input
            // again nor another ending changes it.
            {
                let played = start("hm-restart").await;
                played.end(&row).await;
                let reopened = store(&played.home);
                for again in [
                    finish::exit_ending(SessionStatus::Failed, Some("replay".into()), true),
                    finish::exit_ending(SessionStatus::Success, None, true),
                    Ending::Interrupted,
                ] {
                    assert!(finish::finish(&reopened, &played.run_id, again, finish::now_ms()).unwrap().is_none());
                }
                if row.signal.is_none() {
                    assert_row(&played, phase, reason, None, &at("restart"));
                }
            }
        }
    }

    #[tokio::test]
    async fn an_old_runs_late_input_cannot_alter_a_newer_run() {
        for row in table().into_iter().filter(|row| matches!(row.how, How::Exit(_))) {
            let played = start("hm-old").await;
            let older = played.launcher.run_context(&played.session);
            played
                .launcher
                .capture_stopped_structured(&played.session, FORMAT, HERMES_TEXT_RUN)
                .await;
            let newer_run = manual(&played.home, "hm-old").run_id;
            assert_eq!(drain_claim_loop(&played.home, played.launcher.clone()).await, 1);
            let newer_session = played.launcher.last().session_id;
            let How::Exit(code) = row.how else { unreachable!() };
            played
                .launcher
                .exit_captured(older, &played.session, FORMAT, exit_code(code), &row.stdout)
                .await;
            let at = row.input;
            assert_row(&played, "cancelled", None, None, at);
            let newer = run(&played.home, &newer_run);
            assert_eq!((newer.phase.as_str(), newer.result_text.as_deref()), ("running", None), "{at}");
            assert_ne!(newer_session, played.session);
        }
    }
}

#[test]
fn cancelling_after_start_before_initial_capture_keeps_the_first_close_after_reopen() {
    use crate::session_usage::{UsageStore, task_capture::{RoutineBrief, RoutineStart, capture_routine_start}};
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mut def = routine("capture-race", "*/5 * * * *");
    def.result_schema = Some(serde_json::json!(true));
    def.policy.overlap = OverlapPolicy::Queue;
    let brief = RoutineBrief { name: def.name.clone(), task: def.task.clone() };
    save(&home, def);
    let active = manual(&home, "capture-race");
    let started_at = finish::now_ms() - 1_000;
    // This is the durable start attestation dispatch writes before scheduling task capture.
    store(&home).transaction(|tx| {
        assert_eq!(tx.execute(
            "UPDATE routine_runs SET phase = 'running', started_at = ?2 WHERE run_id = ?1 AND phase = 'queued'",
            rusqlite::params![active.run_id, started_at],
        )?, 1);
        Ok(())
    }).unwrap();
    let held = manual(&home, "capture-race");
    assert_eq!(held.outcome, Outcome::Held);
    let path = home.join("usage.sqlite");
    let open = || UsageStore::open(&path).unwrap();
    assert!(open().read_task(&active.run_id).unwrap().is_none(), "capture is delayed");
    let launcher = StubLauncher::new(stub_agent(temp.root()));
    assert_eq!(super::results::cancel(&home, &alice(), &key("capture-race"), &launcher).unwrap(), 2);
    let pending = open().read_task(&active.run_id).unwrap().expect("started run retains an early close");
    assert_eq!((pending.status.as_str(), pending.raw_status.as_deref()), ("abandoned", Some("cancelled")));
    assert!(pending.closed_at_ms.is_some());
    assert!(open().read_task(&held.run_id).unwrap().is_none(), "unstarted run creates nothing");
    let start = RoutineStart {
        org: ORG, team: TEAM, run_id: &active.run_id, routine: &brief,
        policy_dir: None, cwd: None, session_id: None, now_ms: started_at,
    };
    capture_routine_start(&open(), &start).unwrap();
    let captured = open().read_task(&active.run_id).unwrap().unwrap();
    assert_eq!(captured.goal, format!("{}: {}", brief.name, brief.task));
    assert_eq!(captured.created_at_ms, started_at);
    assert_eq!((captured.status.as_str(), captured.raw_status.as_deref(), captured.closed_at_ms),
        ("abandoned", Some("cancelled"), pending.closed_at_ms));
    crate::session_usage::task_outcome::close_run_task(&default_store_path(&home), &active.run_id, "succeeded");
    capture_routine_start(&open(), &start).unwrap();
    assert_eq!(open().read_task(&active.run_id).unwrap().unwrap(), captured, "later events and capture are inert after reopen");
}

/// Every ending keeps two facts apart: `result_text` is what the run's agent produced, and
/// `detail` is CodeSense's explanation of why the run ended. One table over every CLI.
mod ending_explanations {
    use super::*;
    use crate::agents::service::{save_typed_for_test, Placement as AgentPlacement};
    use crate::routines::test_support::agent;
    use codesense_pty_core::agent_cli::AgentCli;

    const SECRET: &str = "ghp_abcdefghijklmnopqrstuvwxyz1234567890";

    /// A saved team agent of `cli` that prepares for a routine run, through the same
    /// neutral facts the launch tests use.
    fn save_cli_agent(home: &Path, cli: AgentCli) -> crate::agents::AgentKey {
        let mut saved = agent(&format!("{cli}-agent"), cli);
        if crate::agents::adapter::desktop(cli).model_endpoint.is_some() {
            saved.set_startup_settings(Some(BTreeMap::from([(
                "model".into(),
                serde_json::json!({"provider": "custom", "base_url": "http://127.0.0.1:11434/v1", "default": "qwen2.5:3b-instruct"}),
            )])));
            saved.context = Some(Vec::new());
        }
        if AgentCli::ALL.contains(&cli) {
            return super::super::test_support::save_team_agent(home, &alice(), saved);
        }
        saved.revision = 1;
        let team = AgentPlacement::Team { org: ORG.into(), team: TEAM.into() };
        save_typed_for_test(home, &alice(), &saved, &team).unwrap()
    }

    fn cli_routine(home: &Path, id: &str, cli: AgentCli) -> crate::agents::AgentKey {
        let saved = save_cli_agent(home, cli);
        let mut def = routine(id, "*/5 * * * *");
        def.agent = AgentRef::from(saved.id.as_str());
        save(home, def);
        saved
    }

    /// `(phase, reason, result_text, detail)` of a run's row.
    fn seen(home: &Path, run_id: &str) -> (String, Option<String>, Option<String>, Option<String>) {
        let row = run(home, run_id);
        (row.phase, row.reason, row.result_text, row.detail)
    }

    /// The `detail` column as stored, not through any reader that might scrub it.
    fn raw_detail(home: &Path, run_id: &str) -> Option<String> {
        let conn = rusqlite::Connection::open(store(home).path()).unwrap();
        conn.query_row(
            "SELECT detail FROM routine_runs WHERE run_id = ?1",
            [run_id],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn assert_unstarted(home: &Path, run_id: &str, reason: &str, says: &str, at: &str) {
        let (phase, why, result, detail) = seen(home, run_id);
        assert_eq!((phase.as_str(), why.as_deref(), result.as_deref()), ("failed", Some(reason), None), "{at}");
        let detail = detail.unwrap_or_default();
        assert!(detail.contains(says), "{at}: detail {detail:?} names {says:?}");
        assert!(run(home, run_id).started_at.is_none(), "{at}: never started");
    }

    #[tokio::test]
    async fn a_refused_launch_keeps_its_scrubbed_reason_as_detail_for_every_cli() {
        // The refusal the app's final fold gives a saved `--safe-mode` Hermes agent.
        let refusal = codesense_pty_core::agent_cli::hermes::native_arguments(&["--safe-mode".to_owned()])
            .check_managed()
            .unwrap_err();
        for cli in AgentCli::every_cli() {
            let temp = TempHome::new();
            let home = temp.codesense_home();
            cli_routine(&home, "refused", cli);
            let launcher = Arc::new(
                StubLauncher::new(stub_agent(temp.root())).refusing(format!("{refusal} ({SECRET})")),
            );
            let run_id = manual(&home, "refused").run_id;
            // The real claim loop reaches dispatch, whose launch is refused. A CLI that is
            // registered but not listed yet is refused before the launcher, with its own reason.
            let listed = AgentCli::ALL.contains(&cli);
            let says = if listed { refusal.to_owned() } else { format!("{cli} is not available yet") };
            assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
            assert_eq!(launcher.calls(), usize::from(listed), "{cli}");
            let (phase, reason, result, _) = seen(&home, &run_id);
            assert_eq!((phase.as_str(), reason.as_deref(), result.as_deref()), ("failed", Some("launch_failed"), None), "{cli}");
            let stored = raw_detail(&home, &run_id).unwrap_or_default();
            assert!(stored.contains(&says) && !stored.contains(SECRET), "{cli}: {stored}");
        }
    }

    #[tokio::test]
    async fn a_workspace_that_cannot_be_prepared_is_detail_for_every_cli() {
        for cli in AgentCli::every_cli() {
            let temp = TempHome::new();
            let home = temp.codesense_home();
            let saved = cli_routine(&home, "no-room", cli);
            let routine_dir = super::super::workspace::team_root(&home, &alice(), &key("no-room")).join("no-room");
            let dir = super::super::workspace::agent_dir(&routine_dir, &saved);
            std::fs::create_dir_all(&routine_dir).unwrap();
            let elsewhere = temp.root().join("elsewhere");
            std::fs::create_dir_all(&elsewhere).unwrap();
            std::os::unix::fs::symlink(&elsewhere, &dir).unwrap();
            let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
            let run_id = manual(&home, "no-room").run_id;
            assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
            assert_eq!(launcher.calls(), 0, "{cli}: nothing launched");
            assert_unstarted(&home, &run_id, "launch_failed", "changed location", &format!("{cli}"));
        }
    }

    #[tokio::test]
    async fn a_run_that_could_not_be_prepared_is_detail() {
        // A Hermes agent with no model, refused by its own adapter.
        let temp = TempHome::new();
        let home = temp.codesense_home();
        let mut saved = agent("hm", AgentCli::Hermes);
        saved.revision = 1;
        let team = AgentPlacement::Team { org: ORG.into(), team: TEAM.into() };
        save_typed_for_test(&home, &alice(), &saved, &team).unwrap();
        let mut def = routine("no-model", "*/5 * * * *");
        def.agent = AgentRef::from("hm");
        save(&home, def);
        // A task that cannot be filled in.
        let mut def = routine("no-value", "*/5 * * * *");
        def.task = "{{last_run_summary}}".into();
        save(&home, def);
        let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
        for (id, reason, says) in [
            ("no-model", "agent_unavailable", "Hermes needs a model"),
            ("no-value", "task_unrenderable", "empty once filled in"),
        ] {
            let run_id = manual(&home, id).run_id;
            assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
            assert_unstarted(&home, &run_id, reason, says, id);
        }
        assert_eq!(launcher.calls(), 0);
    }

    #[tokio::test]
    async fn a_failed_exit_is_the_agents_text_or_detail_by_the_clis_own_fact() {
        use codesense_pty_core::agent_cli::TranscriptFormat;
        for cli in AgentCli::every_cli() {
            let temp = TempHome::new();
            let home = temp.codesense_home();
            // A listed CLI runs as a saved agent of that CLI. One registered but not listed
            // yet cannot launch as a saved agent, so its run ends in its format directly.
            let listed = AgentCli::ALL.contains(&cli);
            if listed {
                cli_routine(&home, "exits", cli);
            } else {
                save(&home, routine("exits", "*/5 * * * *"));
            }
            let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
            let run_id = manual(&home, "exits").run_id;
            assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
            let launched = launcher.last().request.agent.runtime;
            if listed {
                assert_eq!(launched, cli, "the run launched its saved agent's CLI");
            }
            // The capture ends in the headless format of the CLI that ran.
            let format = Option::and_then(
                codesense_pty_core::agent_cli::adapter(if listed { launched } else { cli })
                    .output()
                    .transcript,
                |t| t.headless.as_ref(),
            )
            .map(|h| h.format);
            launcher
                .exit_structured(&launcher.last().session_id, format, exit_code(1), "", "it broke")
                .await;
            let (phase, reason, result, detail) = seen(&home, &run_id);
            assert_eq!((phase.as_str(), reason.as_deref()), ("failed", Some("exited_with_error")), "{cli}");
            if Option::is_some_and(format, TranscriptFormat::result_is_signal_only) {
                assert_eq!(result, None, "{cli}: process output is never the result");
                assert!(detail.is_some_and(|d| !d.is_empty()), "{cli}: the exit is explained");
            } else {
                assert!(result.is_some_and(|r| !r.is_empty()), "{cli}: unchanged, the exit text is the result");
                assert_eq!(detail, None, "{cli}");
            }
        }
    }

    #[tokio::test]
    async fn a_signal_owns_the_row_and_a_later_refusal_adds_no_detail() {
        let temp = TempHome::new();
        let home = temp.codesense_home();
        save(&home, routine("owned", "*/5 * * * *"));
        let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
        let run_id = manual(&home, "owned").run_id;
        assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
        launcher.signal_done(&launcher.last().session_id, "success", "T").await;
        let late = Ending::Failed { reason: "launch_failed", detail: "late refusal".into() };
        assert!(finish::finish(&store(&home), &run_id, late, finish::now_ms()).unwrap().is_none());
        let (phase, _, result, detail) = seen(&home, &run_id);
        assert_eq!((phase.as_str(), result.as_deref(), detail), ("succeeded", Some("T"), None));
    }
}
