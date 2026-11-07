// SPDX-License-Identifier: MIT
//! A report of one session reads that session's usage events and no others (SV-CDE-105).
use super::*;
use crate::session_usage::{event::Tokens, tests};
use rusqlite::{params, StatementStatus};

fn plan(store: &UsageStore, by_session: bool) -> String {
    let mut stmt = store
        .conn
        .prepare(&format!("EXPLAIN QUERY PLAN {}", usage_report_sql(by_session)))
        .unwrap();
    let rows = stmt
        .query_map(params![None::<String>, None::<String>, Some("[\"s0\"]"), None::<String>], |r| r.get::<_, String>(3))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    rows.join("\n")
}

/// The VM steps the session statement takes for session `s0` in a store holding `others` more
/// sessions of three events each.
fn steps_for_one_session(others: usize) -> (i32, usize) {
    let (_dir, mut store) = tests::store();
    let mut events = vec![];
    for n in 0..=others {
        for m in 0..3 {
            events.push(tests::event(
                &format!("e-{n}-{m}"),
                &format!("s{n}"),
                "gpt-6-sol",
                1_790_000_000_000 + m,
                Tokens { input: 1, ..Default::default() },
            ));
        }
    }
    store.ingest(&events).unwrap();
    let mut stmt = store.conn.prepare(&usage_report_sql(true)).unwrap();
    let mut rows = stmt
        .query(params![None::<String>, None::<String>, Some("[\"s0\"]"), None::<String>])
        .unwrap();
    let mut read = 0;
    while rows.next().unwrap().is_some() {
        read += 1;
    }
    drop(rows);
    (stmt.get_status(StatementStatus::VmStep), read)
}

#[test]
fn the_session_statement_searches_the_session_index() {
    let (_dir, store) = tests::store();
    let by_session = plan(&store, true);
    assert!(
        by_session.lines().any(|l| l.starts_with("SEARCH") && l.contains("usage_event_by_session")),
        "no index search of usage_event by session:\n{by_session}"
    );
    // The plan prints the alias, not the table name.
    assert!(!by_session.lines().any(|l| l.starts_with("SCAN e")), "{by_session}");
    // The report over every event scans them, as it must.
    assert!(plan(&store, false).lines().any(|l| l.starts_with("SCAN")));
}

#[test]
fn the_session_statement_never_reads_another_sessions_events() {
    let (few, read_few) = steps_for_one_session(2);
    let (many, read_many) = steps_for_one_session(200);
    assert_eq!((read_few, read_many), (3, 3), "only the session's own events come back");
    assert_eq!(few, many, "the work does not grow with the other sessions' events");
}

#[test]
fn a_launch_finds_its_session_ids_through_the_launch_index() {
    let (_dir, store) = tests::store();
    let mut stmt = store
        .conn
        .prepare(&format!("EXPLAIN QUERY PLAN {}", session_read::LAUNCH_SESSIONS_SQL))
        .unwrap();
    let plan = stmt
        .query_map(params!["launch"], |r| r.get::<_, String>(3))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .join("\n");
    assert!(
        plan.lines().any(|l| l.starts_with("SEARCH") && l.contains("USING COVERING INDEX session_attribution_by_launch")
            || l.starts_with("SEARCH") && l.contains("USING INDEX session_attribution_by_launch")),
        "no index search by launch:\n{plan}"
    );
}

/// A report says why its sessions are missing while managed Claude settings send usage
/// elsewhere, even with older sessions listed, and carries no reason key otherwise.
#[test]
fn a_report_says_why_while_managed_settings_redirect_usage() {
    let (_dir, mut store) = tests::store();
    store
        .ingest(&[tests::event("e-1", "older", "gpt-6-sol", 1_790_000_000_000, Tokens { input: 1, ..Default::default() })])
        .unwrap();
    let report = || serde_json::to_value(explained(store.report(&Filter::default()).unwrap())).unwrap();
    let plain = report();
    assert!(plain.get("unavailableReason").is_none(), "{plain}");
    let read_from_its_log = managed_telemetry::Fixture::redirecting();
    assert!(report().get("unavailableReason").is_none(), "Claude's session log is read");
    drop(read_from_its_log);
    let _managed = managed_telemetry::Fixture::redirecting_unread();
    let redirected = report();
    assert_eq!(redirected["unavailableReason"], managed_telemetry::USAGE_REDIRECTED);
    assert_eq!(redirected["sessions"].as_array().unwrap().len(), 1, "older sessions are still listed");
}
