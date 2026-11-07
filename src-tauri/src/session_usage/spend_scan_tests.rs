// SPDX-License-Identifier: MIT
//! The spend-cap query is bounded by the cap window, not by an agent's whole history
//! (SV-PBF-169).
use super::*;
use chrono::NaiveDate;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::Instant;

const AGENT: &str = "agent-heavy";
const ROUTINE: &str = "nightly";

fn plan(store: &UsageStore, scope: SpendScope<'_>, window: SpendWindow) -> String {
    fn plan_detail(r: &rusqlite::Row) -> rusqlite::Result<String> {
        r.get::<_, String>(3)
    }
    let (since, until) = window.days().unwrap();
    let (sql, lower, upper) = spend_sql(scope);
    let mut stmt = store
        .conn
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap();
    let rows = if matches!(scope, SpendScope::Agent(_)) {
        stmt.query_map(params![since, until, lower], plan_detail)
    } else {
        stmt.query_map(params![since, until, lower, upper], plan_detail)
    }
    .unwrap()
    .collect::<Result<Vec<_>, _>>()
    .unwrap();
    rows.join("\n")
}

#[test]
fn the_agent_scope_is_searched_by_session_and_day_window() {
    let (_dir, store) = tests::store();
    let day = NaiveDate::from_ymd_opt(2026, 10, 21).unwrap();
    let plan = plan(&store, SpendScope::Agent(AGENT), SpendWindow::Day(day));
    assert!(
        plan.contains("usage_event_by_session_day_cost"),
        "agent cost subquery should use the session/day/cost index:\n{plan}"
    );
    assert!(
        plan.contains("usage_event_unpriced_by_session_day_model"),
        "agent model subquery should use the unpriced partial index:\n{plan}"
    );
    assert!(
        !plan.contains("usage_event_by_session (session_id=?)"),
        "agent scope should not read all-time session events:\n{plan}"
    );
    assert!(
        !plan.contains("GROUP BY"),
        "agent scope should not group by model:\n{plan}"
    );
    assert!(
        !plan.contains("ORDER BY"),
        "agent scope should not order by model:\n{plan}"
    );
}

#[test]
fn the_routine_scope_stays_bounded_by_the_day_window() {
    let (_dir, store) = tests::store();
    let day = NaiveDate::from_ymd_opt(2026, 10, 21).unwrap();
    let plan = plan(&store, SpendScope::Routine(ROUTINE), SpendWindow::Day(day));
    assert!(
        plan.contains("day>? AND day<?"),
        "unbounded by day:\n{plan}"
    );
    assert!(
        plan.contains("usage_event_by_session_day_cost"),
        "routine cost subquery should use the session/day/cost index:\n{plan}"
    );
    assert!(
        plan.contains("usage_event_unpriced_by_session_day_model"),
        "routine model subquery should use the unpriced partial index:\n{plan}"
    );
    assert!(
        !plan.contains("usage_event_by_session (session_id=?)"),
        "reads all-time events of each session:\n{plan}"
    );
    assert!(
        !plan.contains("GROUP BY"),
        "routine scope should not group by model:\n{plan}"
    );
    assert!(
        !plan.contains("ORDER BY"),
        "routine scope should not order by model:\n{plan}"
    );
}

#[test]
fn old_session_day_index_is_upgraded_to_covering_index_on_reopen() {
    let (dir, mut store) = tests::store();
    let today = NaiveDate::from_ymd_opt(2026, 10, 21).unwrap();

    // Attribute the same session under AGENT and a routine handle so both scopes match it.
    store
        .attribute("s1", "org", Some("team"), AGENT, "routine:nightly@1", None)
        .unwrap();
    let mut priced = tests::event(
        "e1",
        "s1",
        "claude-sonnet",
        tests::local_noon(2026, 10, 21),
        super::event::Tokens { input: 1, ..Default::default() },
    );
    priced.reported_cost_usd = Some(1.23);
    let unpriced = tests::event(
        "e2",
        "s1",
        "unknown-variant3",
        tests::local_noon(2026, 10, 21),
        super::event::Tokens { input: 1, ..Default::default() },
    );
    store.ingest(&[priced, unpriced]).unwrap();

    // Simulate a pre-variant3 store: both old indexes present, both new indexes absent, version 5.
    store
        .conn
        .execute_batch(
            "DROP INDEX IF EXISTS usage_event_by_session_day_cost;
             DROP INDEX IF EXISTS usage_event_unpriced_by_session_day_model;
             DROP INDEX IF EXISTS usage_event_by_session_day_covering;
             CREATE INDEX IF NOT EXISTS usage_event_by_session_day ON usage_event (session_id, day);
             CREATE INDEX IF NOT EXISTS usage_event_by_session_day_covering ON usage_event (session_id, day, model, cost_usd);
             PRAGMA user_version = 5;",
        )
        .unwrap();
    drop(store);

    let mut store = UsageStore::open(&dir.path().join("usage.sqlite")).unwrap();
    let plan = plan(&store, SpendScope::Agent(AGENT), SpendWindow::Day(today));
    assert!(
        plan.contains("usage_event_by_session_day_cost") || plan.contains("usage_event_unpriced_by_session_day_model"),
        "plan did not switch to a variant3 index:\n{plan}"
    );
    let old_count: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name IN ('usage_event_by_session_day','usage_event_by_session_day_covering')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(old_count, 0, "old indexes still exist");
    let new_count: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name IN ('usage_event_by_session_day_cost','usage_event_unpriced_by_session_day_model')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(new_count, 2, "both variant3 indexes not present");

    for scope in [SpendScope::Agent(AGENT), SpendScope::Routine(ROUTINE)] {
        let got = store.spend_for(scope, SpendWindow::Day(today)).unwrap();
        assert!(
            (got.cost_usd - 1.23).abs() < 0.001,
            "priced data lost for {:?}: got {}",
            scope, got.cost_usd
        );
        assert_eq!(
            got.unpriced_models,
            vec!["unknown-variant3"],
            "unpriced models mismatch for {:?}",
            scope
        );
    }

    // Reopen again: the index set must remain stable.
    drop(store);
    let store = UsageStore::open(&dir.path().join("usage.sqlite")).unwrap();
    let count: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name IN ('usage_event_by_session_day_cost','usage_event_unpriced_by_session_day_model')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 2, "index set changed on a second reopen");
}

#[test]
fn single_window_aggregate_counts_unpriced_models_once_and_keeps_priced_costs() {
    let (_dir, mut store) = tests::store();
    let today = NaiveDate::from_ymd_opt(2026, 10, 21).unwrap();
    let yesterday = NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
    let monday = NaiveDate::from_ymd_opt(2026, 10, 19).unwrap();
    let prior_week = NaiveDate::from_ymd_opt(2026, 10, 13).unwrap();
    let absent_week = NaiveDate::from_ymd_opt(2026, 10, 5).unwrap();

    store
        .attribute("sa", "org", Some("team"), AGENT, "handle-a", None)
        .unwrap();
    store
        .attribute("sr", "org", Some("team"), "agent-routine", "routine:nightly@1", None)
        .unwrap();

    let rows = [
        // Agent scope
        ("a1", "sa", "unknown-z", today, Some(2.0)),
        ("a2", "sa", "unknown-z", today, None),
        ("a3", "sa", "unknown-z", today, None),
        ("a4", "sa", "unknown-a", today, None),
        ("a5", "sa", "unknown-week", yesterday, None),
        ("a6", "sa", "unknown-paid", monday, Some(0.75)),
        ("a7", "sa", "unknown-outside", prior_week, None),
        // Routine scope
        ("r1", "sr", "unknown-z", today, Some(1.5)),
        ("r2", "sr", "unknown-z", today, None),
        ("r3", "sr", "unknown-z", today, None),
        ("r4", "sr", "unknown-a", today, None),
        ("r5", "sr", "unknown-week", yesterday, None),
        ("r6", "sr", "unknown-paid", monday, Some(0.75)),
        ("r7", "sr", "unknown-outside", prior_week, None),
    ];

    let mut events = Vec::new();
    for (id, session, model, day, cost) in rows {
        let mut ev = tests::event(
            id,
            session,
            model,
            tests::local_noon(day.year(), day.month(), day.day()),
            super::event::Tokens { input: 1, ..Default::default() },
        );
        ev.reported_cost_usd = cost;
        events.push(ev);
    }
    store.ingest(&events).unwrap();

    let cases = [
        (SpendScope::Agent(AGENT), SpendWindow::Day(today), 2.0, vec!["unknown-a", "unknown-z"]),
        (SpendScope::Agent(AGENT), SpendWindow::Week(today), 2.75, vec!["unknown-a", "unknown-week", "unknown-z"]),
        (SpendScope::Agent(AGENT), SpendWindow::Day(yesterday), 0.0, vec!["unknown-week"]),
        (SpendScope::Agent(AGENT), SpendWindow::Day(monday), 0.75, vec![]),
        (SpendScope::Agent(AGENT), SpendWindow::Day(absent_week), 0.0, vec![]),
        (SpendScope::Agent(AGENT), SpendWindow::Week(absent_week), 0.0, vec![]),
        (SpendScope::Agent(AGENT), SpendWindow::Week(prior_week), 0.0, vec!["unknown-outside"]),
        (SpendScope::Routine(ROUTINE), SpendWindow::Day(today), 1.5, vec!["unknown-a", "unknown-z"]),
        (SpendScope::Routine(ROUTINE), SpendWindow::Week(today), 2.25, vec!["unknown-a", "unknown-week", "unknown-z"]),
        (SpendScope::Routine(ROUTINE), SpendWindow::Day(yesterday), 0.0, vec!["unknown-week"]),
        (SpendScope::Routine(ROUTINE), SpendWindow::Day(monday), 0.75, vec![]),
        (SpendScope::Routine(ROUTINE), SpendWindow::Day(absent_week), 0.0, vec![]),
        (SpendScope::Routine(ROUTINE), SpendWindow::Week(absent_week), 0.0, vec![]),
        (SpendScope::Routine(ROUTINE), SpendWindow::Week(prior_week), 0.0, vec!["unknown-outside"]),
    ];

    for (scope, window, want_cost, want_models) in cases {
        let got = store.spend_for(scope, window).unwrap();
        assert!(
            (got.cost_usd - want_cost).abs() < 0.001,
            "cost mismatch for {:?} {:?}: got {}",
            scope, window, got.cost_usd
        );
        assert_eq!(
            got.unpriced_models, want_models,
            "unpriced models mismatch for {:?} {:?}",
            scope, window
        );
    }
}
/// Heavy-day reference: 2026-10-21 = 54,479 events in the real usage.sqlite.
/// Seed 10x that (544,800) on the heavy day, proportional reference counts on
/// 2026-10-19/20, and modest proportional counts for the remaining ~3 weeks of
/// history so ordinary cap_status reads stay bounded by the day/week window.
fn seed(store: &UsageStore, today: NaiveDate) {
    store.conn.execute_batch("BEGIN").unwrap();
    let mut attribute = store
        .conn
        .prepare("INSERT INTO session_attribution (session_id, org, team, agent, handle, cwd, attributed_at_ms) VALUES (?1,'org','team',?2,?3,NULL,0)")
        .unwrap();
    let mut event = store
        .conn
        .prepare("INSERT INTO usage_event (machine_id, cli, event_id, session_id, day, at_ms, cwd, model, input, output, cache_read, cache_write, cost_usd, cost_from) VALUES ('','claude',?1,?2,?3,0,NULL,'claude-sonnet',1,1,1,1,0.01,'seed')")
        .unwrap();

    let agent_sessions: Vec<_> = (0..400).map(|s| format!("a-{s}")).collect();
    let routine_sessions: Vec<_> = (0..100).map(|s| format!("r-{s}")).collect();
    let other_sessions: Vec<_> = (0..100).map(|s| format!("o-{s}")).collect();
    for (i, sid) in agent_sessions.iter().enumerate() {
        attribute
            .execute(params![sid, AGENT, format!("agent-{i}")])
            .unwrap();
    }
    for (i, sid) in routine_sessions.iter().enumerate() {
        attribute
            .execute(params![
                sid,
                format!("agent-routine-{i}"),
                format!("routine:{ROUTINE}@{i}")
            ])
            .unwrap();
    }
    for (i, sid) in other_sessions.iter().enumerate() {
        attribute
            .execute(params![sid, format!("other-{i}"), format!("other-{i}")])
            .unwrap();
    }

    let oct = |d: i64| NaiveDate::from_ymd_opt(2026, 10, d as u32).unwrap();
    let mut n = 0u64;
    for back in 0..=20 {
        let day = today - chrono::Duration::days(back);
        let day_total: u64 = if day == today {
            544_800
        } else if day == oct(20) {
            242_400
        } else if day == oct(19) {
            8_040
        } else {
            6_000
        };
        let agent_events = day_total * 2 / 3;
        let routine_events = day_total / 6;
        let other_events = day_total / 6;
        let day_str = day.format("%Y-%m-%d").to_string();
        let mut emit = |count: u64, sessions: &[String]| {
            for _ in 0..count {
                let sid = &sessions[(n as usize) % sessions.len()];
                n += 1;
                event
                    .execute(params![format!("e{n}"), sid, &day_str])
                    .unwrap();
            }
        };
        emit(agent_events, &agent_sessions);
        emit(routine_events, &routine_sessions);
        emit(other_events, &other_sessions);
    }
    store.conn.execute_batch("COMMIT").unwrap();
}

#[test]
fn cap_status_over_heavy_history_stays_fast() {
    let (dir, seed_store) = tests::store();
    // Fixed date so the Monday-window and exact totals are deterministic.
    let today = NaiveDate::from_ymd_opt(2026, 10, 21).unwrap();
    seed(&seed_store, today);
    drop(seed_store);

    let budget = spend_caps::Budget {
        per_day: Some(100_000.0),
        per_week: Some(1_000_000.0),
    };
    // Week window is Monday 2026-10-19 through Sunday 2026-10-25.
    let agent_day = 544_800.0 * 2.0 / 3.0 * 0.01; // 3,632.00
    let routine_day = 544_800.0 / 6.0 * 0.01; // 908.00
    let week_agent_total = (8_040 + 242_400 + 544_800) as f64 * 2.0 / 3.0 * 0.01; // 5,301.60
    let week_routine_total = (8_040 + 242_400 + 544_800) as f64 / 6.0 * 0.01; // 1,325.40
    let mut baseline_work = Vec::new();
    for expanded_history in [false, true] {
        // Measure the first call after reopening as well as subsequent calls.
        let mut store = UsageStore::open(&dir.path().join("usage.sqlite")).unwrap();
        for (scope_index, (kind, key, expected_day, expected_week)) in [
            ("agent", AGENT, agent_day, week_agent_total),
            ("routine", ROUTINE, routine_day, week_routine_total),
        ]
        .into_iter()
        .enumerate()
        {
            for run in 0..5 {
                // Each tick represents 1,000 SQLite VM instructions, independent of
                // scheduling, I/O latency and CPU contention on the shared machine.
                let work = Arc::new(AtomicU64::new(0));
                let counter = Arc::clone(&work);
                store.conn.progress_handler(
                    1_000,
                    Some(move || {
                        counter.fetch_add(1, Ordering::Relaxed);
                        false
                    }),
                );
                let start = Instant::now();
                let result = store.cap_status(kind, key, budget, today, false);
                let elapsed = start.elapsed();
                store.conn.progress_handler(0, None::<fn() -> bool>);
                let status = result.unwrap();
                let work = work.load(Ordering::Relaxed);
                eprintln!("{kind} cap_status run {run}, expanded={expanded_history}: {elapsed:?}, {work} VM ticks");
                assert!(
                    (status.spend.today - expected_day).abs() < 0.001,
                    "{kind} day spend {:.4} != {expected_day:.4}",
                    status.spend.today
                );
                assert!(
                    (status.spend.week - expected_week).abs() < 0.001,
                    "{kind} week spend {:.4} != {expected_week:.4}",
                    status.spend.week
                );
                assert!(work > 0, "progress hook must observe the production query");
                if expanded_history {
                    let baseline = baseline_work[scope_index * 5 + run];
                    // Allow 1% plus two ticks for hook granularity and fixed cap
                    // bookkeeping; scanning the added history costs far more.
                    assert!(work <= baseline + baseline / 100 + 2,
                        "{kind} cap_status work grew with old history: {baseline} -> {work} VM ticks");
                } else {
                    baseline_work.push(work);
                }
            }
        }
        if !expanded_history {
            // Double the event history without changing attribution or any current
            // day/week inputs. These old rows belong to the very same sessions;
            // include unpriced rows so the model lookup is also exercised.
            let added = store.conn.execute(
                "INSERT INTO usage_event (machine_id, cli, event_id, session_id, day, at_ms, cwd, model, input, output, cache_read, cache_write, cost_usd, cost_from)
                 SELECT machine_id, cli, event_id || '-old', session_id, '2026-09-01', at_ms, cwd, model, input, output, cache_read, cache_write, CASE WHEN rowid % 2 = 0 THEN NULL ELSE cost_usd END, cost_from FROM usage_event", []
            ).unwrap();
            assert!(
                added >= 900_000,
                "history-growth fixture must stay substantial"
            );
        }
    }
}
