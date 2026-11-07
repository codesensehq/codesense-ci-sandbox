// SPDX-License-Identifier: MIT
//! Spend-cap evaluation and the LiveHub model lock never wait on each other: the usage
//! store is read on a connection of its own, and no hub reader waits for it (SV-PBF-169).
use super::*;
use crate::routines::test_support::{agent, alice, launch_app, save_team_agent, TempHome};
use crate::session_usage::{
    event::Tokens,
    spend_caps::{self, Budget, Evaluation},
    tests::event,
    UsageStore,
};
use codesense_pty_core::agent_cli::AgentCli;
use std::sync::mpsc;
use std::time::Duration;

/// A bound on how long either side may take; only reached when the guard fails.
const BOUND: Duration = Duration::from_secs(5);

fn populated_app() -> (
    TempHome,
    tauri::App<tauri::test::MockRuntime>,
    crate::agents::AgentKey,
) {
    let home = TempHome::new();
    let app = launch_app(&home.codesense_home());
    app.manage(LiveHub::default());

    let user = alice();
    let mut definition = agent("budgeted", AgentCli::Claude);
    definition.budget_usd = Some(Budget {
        per_day: Some(50.0),
        per_week: Some(100.0),
    });
    let key = save_team_agent(&home.codesense_home(), &user, definition);

    let mut store = UsageStore::open(&home.codesense_home().join("usage.sqlite")).unwrap();
    store
        .attribute(
            "s1",
            "acme/api",
            Some("backend"),
            &key.to_string(),
            "handle1",
            None,
        )
        .unwrap();
    let mut ev = event(
        "e1",
        "s1",
        "claude-sonnet",
        chrono::Local::now().timestamp_millis(),
        Tokens {
            output: 1,
            ..Tokens::default()
        },
    );
    ev.reported_cost_usd = Some(1.23);
    store.ingest(&[ev]).unwrap();

    (home, app, key)
}

fn agent_status<'a>(evaluated: &'a Evaluation, key: &str) -> &'a spend_caps::CapStatus {
    evaluated
        .agents
        .get(key)
        .expect("agent status present in evaluation")
}

fn find_spend_row<'a>(snapshot: &'a [serde_json::Value], key: &str) -> &'a serde_json::Value {
    snapshot
        .iter()
        .find(|r| r["id"] == format!("spend:agent:{key}"))
        .expect("spend row present in agentList snapshot")
}

#[test]
fn spend_evaluation_completes_while_the_hub_lock_is_held() {
    let (home, app, key) = populated_app();

    // Hold the real app hub lock while evaluation reads usage.sqlite.
    let app_handle = app.handle().clone();
    let (held_tx, held) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let hub = app_handle.state::<LiveHub>();
        let _guard = hub.model.lock().unwrap();
        held_tx.send(()).unwrap();
        let _ = release_rx.recv();
    });
    let held_res = held.recv_timeout(BOUND);

    let home_path = home.codesense_home();
    let (done_tx, done) = mpsc::channel();
    let evaluator = std::thread::spawn(move || {
        let principal = alice();
        let evaluated = spend_caps::evaluate(&home_path, &principal);
        let _ = done_tx.send(evaluated);
    });
    let evaluated = done.recv_timeout(BOUND);

    // Release and join before asserting or publishing.
    drop(release_tx);
    let holder_join = holder.join();
    let evaluator_join = evaluator.join();

    held_res.expect("holder never acquired the hub lock");
    holder_join.expect("holder thread panicked");
    evaluator_join.expect("evaluator thread panicked");
    let evaluated = evaluated
        .expect("evaluation timed out waiting on the hub lock")
        .expect("evaluation failed");
    let status = agent_status(&evaluated, &key.to_string());
    assert!((status.spend.today - 1.23).abs() < 0.001);
    assert!((status.spend.week - 1.23).abs() < 0.001);
    assert_eq!(status.effective.per_day, Some(50.0));
    assert_eq!(status.effective.per_week, Some(100.0));

    // Publication legitimately needs the hub lock.
    crate::live_hub::spend_changed(app.handle(), &evaluated).unwrap();
    let snapshot = app
        .state::<LiveHub>()
        .snapshot("all", "agentList")
        .unwrap()
        .snapshot;
    let row = find_spend_row(&snapshot, &key.to_string());
    assert_eq!(row["rowType"], "spend");
    assert!((row["spend"]["today"].as_f64().unwrap() - 1.23).abs() < 0.001);
    assert!((row["spend"]["week"].as_f64().unwrap() - 1.23).abs() < 0.001);
    assert_eq!(row["spend"]["cap"]["perDay"], 50.0);
    assert_eq!(row["spend"]["cap"]["perWeek"], 100.0);
}

#[test]
fn hub_readers_proceed_while_spend_query_is_blocked() {
    let (home, app, key) = populated_app();

    let app_handle = app.handle().clone();
    let (entered_tx, entered) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let hook = move || {
        let _ = entered_tx.send(());
        let _ = release_rx.recv();
    };

    let (done_tx, done) = mpsc::channel();
    let evaluator = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(spend_caps::evaluate_and_publish(
            &app_handle,
            &std::collections::BTreeMap::new(),
            Some(Box::new(hook)),
        ));
        let _ = done_tx.send(result);
    });
    let entry_res = entered.recv_timeout(BOUND);

    // Hub readers must proceed while the spend query is parked.
    let app_handle2 = app.handle().clone();
    let (reader_done_tx, reader_done_rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let hub = app_handle2.state::<LiveHub>();
        let _ = hub.rows(None);
        let _ = hub.inbox_view(&crate::commands::dashboard_launch::LaunchTeam {
            org: "acme".into(),
            team: "backend".into(),
        });
        let snapshot_result = hub
            .snapshot("all", "agentList")
            .map(|snapshot| snapshot.snapshot)
            .map_err(|e| e);
        let _ = reader_done_tx.send(snapshot_result);
    });
    let reader_res = reader_done_rx.recv_timeout(BOUND);

    // Release and join all workers before asserting or inspecting published state.
    drop(release_tx);
    let eval_result = done.recv_timeout(BOUND);
    let eval_join = evaluator.join();
    let reader_join = reader.join();

    entry_res.expect("spend query never started");
    reader_join.expect("reader thread panicked");
    eval_join.expect("evaluator thread panicked");

    let reader_snapshot = reader_res.expect("hub reader timed out while spend query was blocked");
    let (_caller, evaluated) = eval_result
        .expect("evaluator timed out")
        .expect("caller resolution or publish failed");
    let evaluated = evaluated.expect("evaluation failed");
    let status = agent_status(&evaluated, &key.to_string());
    assert!((status.spend.today - 1.23).abs() < 0.001);

    reader_snapshot.expect("agentList snapshot read failed");

    // The production refresh-to-publish path produced the exact row.
    let snapshot = app
        .state::<LiveHub>()
        .snapshot("all", "agentList")
        .unwrap()
        .snapshot;
    let row = find_spend_row(&snapshot, &key.to_string());
    assert!((row["spend"]["today"].as_f64().unwrap() - 1.23).abs() < 0.001);
    assert!((row["spend"]["week"].as_f64().unwrap() - 1.23).abs() < 0.001);
}
