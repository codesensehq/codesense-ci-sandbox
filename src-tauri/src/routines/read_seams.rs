// SPDX-License-Identifier: MIT
//! Seam tests for the routine read service: what a run left on its routine,
//! read back the way the MCP tools and the Routines tab read it.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Utc};

use super::admit::{self, Fire, Outcome};
use super::model::{Author, AuthorKind, OverlapPolicy, RoutineTrigger, ScheduleSpec};
use super::run_seams::{
    files_named, key, manual, ms, routine, run, save, store, ticker, ticker_as,
};
use super::schedule::ScheduleError;
use super::service::{self, LastRun, PersonalOwner, Placement, RoutineError, RunsPage, SaveTarget};
use super::test_support::{
    alice, changes_of, drain_claim_loop, record_changes, stub_agent, write_org, FakeSessions,
    StubLauncher, TempHome,
};
use super::tick::RunSource;
use super::{ValidationError, Variable};

fn at(rfc3339: &str) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms(rfc3339)).unwrap()
}

#[tokio::test]
async fn a_scheduled_runs_result_reads_back_from_the_routine() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, routine("digest", "*/5 * * * *"));
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let clock = Arc::new(AtomicI64::new(ms("2026-10-05T09:01:00Z")));
    let ticker = ticker(&home, &clock, launcher.clone());
    ticker.tick().unwrap();
    clock.store(ms("2026-10-05T09:05:10Z"), Ordering::SeqCst);
    let run_id = ticker.tick().unwrap().changed[0].1.clone();
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    let session = launcher.last().session_id;

    // While it runs, the running run is the last run.
    let now = at("2026-10-05T09:06:00Z");
    let listed = service::list_summaries_at(&home, &alice(), now).unwrap();
    assert_eq!(listed.len(), 1);
    let summary = &listed[0];
    assert_eq!(summary.schedule_in_words, "every 5 minutes");
    assert_eq!(summary.next_run_at, Some(ms("2026-10-05T09:10:00Z")));
    assert!(summary.armed_here);
    assert_eq!(
        summary.created_by,
        Some(Author {
            kind: AuthorKind::User,
            name: "alice".into()
        })
    );
    let last = summary.last_run.as_ref().unwrap();
    assert_eq!(
        (last.run_id.as_str(), last.phase.as_str()),
        (run_id.as_str(), "running")
    );
    assert!(!summary.needs_you);

    launcher.signal_done(&session, "success", "ok").await;
    let ended = run(&home, &run_id);
    let summary = service::list_summaries_at(&home, &alice(), now)
        .unwrap()
        .remove(0);
    assert_eq!(
        summary.last_run,
        Some(LastRun {
            run_id: run_id.clone(),
            phase: "succeeded".into(),
            reason: None,
            at: ended.ended_at.unwrap(),
            result_text: Some("ok".into()),
            detail: None,
            delivered_at: None,
            wake_machine: None,
            launch_machine: None,
            session_id: ended.session_id.clone(),
        })
    );
    assert!(!summary.needs_you);

    let history = service::runs(&home, &alice(), &key("digest"), RunsPage::default()).unwrap();
    assert_eq!(history.runs.len(), 1);
    assert_eq!(history.runs[0], ended);
    assert_eq!(history.next_before, None);

    let detail = service::run_detail(&home, &alice(), &run_id).unwrap();
    assert_eq!(detail.routine_name, "Routine digest");
    assert_eq!(detail.run.result_text.as_deref(), Some("ok"));
    assert_eq!(detail.run.source, "schedule");
    assert_eq!(
        detail.run.occurrence_key.as_deref(),
        Some("acme/api/backend/digest@2026-10-05T09:05:00Z")
    );
    assert_eq!(detail.run.session_id.as_deref(), Some(session.as_str()));
    assert!(detail.run.started_at.is_some());

    launcher.exit(&session).await;
    assert_eq!(files_named(temp.root(), "weft.sqlite3"), 0);
}

#[tokio::test]
async fn an_edited_schedule_reads_its_own_next_run_before_the_scheduler_wakes() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, routine("digest", "0 9 * * *"));
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let clock = Arc::new(AtomicI64::new(ms("2026-10-05T07:00:00Z")));
    ticker(&home, &clock, launcher).tick().unwrap();
    let next = |home: &std::path::Path| {
        service::list_summaries_at(home, &alice(), at("2026-10-05T07:00:00Z"))
            .unwrap()
            .remove(0)
            .next_run_at
    };
    assert_eq!(next(&home), Some(ms("2026-10-05T09:00:00Z")));

    let mut edited = service::get(&home, &alice(), &key("digest")).unwrap().def;
    edited.triggers = vec![RoutineTrigger::Schedule(ScheduleSpec {
        cron: "0 18 * * *".into(),
        tz: "UTC".into(),
    })];
    service::save(
        &home,
        &alice(),
        SaveTarget::Update {
            key: key("digest"),
            base_revision: 1,
        },
        edited,
        &Author::user("alice"),
    )
    .unwrap();
    assert_eq!(
        next(&home),
        Some(ms("2026-10-05T18:00:00Z")),
        "the old schedule's cursor is not the edited routine's next run"
    );
}

#[tokio::test]
async fn a_needs_you_run_counts_until_a_later_run_ends_another_way() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, routine("triage", "0 9 * * *"));
    save(&home, routine("quiet", "0 9 * * *"));
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let needs_you = |id: &str| {
        service::list_summaries(&home, &alice())
            .unwrap()
            .into_iter()
            .find(|s| s.key == key(id))
            .unwrap()
    };
    assert_eq!(service::needs_you_count(&home, &alice()).unwrap(), 0);

    let first = manual(&home, "triage").run_id;
    drain_claim_loop(&home, launcher.clone()).await;
    let session = launcher.last().session_id;
    launcher
        .signal_done(&session, "needs-attention", "which repo?")
        .await;
    assert_eq!(run(&home, &first).phase, "needs_you");
    assert_eq!(service::needs_you_count(&home, &alice()).unwrap(), 1);
    let summary = needs_you("triage");
    assert!(summary.needs_you);
    let last = summary.last_run.unwrap();
    assert_eq!(last.phase, "needs_you");
    assert_eq!(last.result_text.as_deref(), Some("which repo?"));
    assert!(!needs_you("quiet").needs_you);
    launcher.exit(&session).await;

    // A fire that does not run leaves it needing you.
    service::set_enabled(&home, &alice(), &key("triage"), false).unwrap();
    assert_eq!(
        manual(&home, "triage").outcome,
        Outcome::Skipped {
            reason: "paused".into()
        }
    );
    service::set_enabled(&home, &alice(), &key("triage"), true).unwrap();
    assert_eq!(service::needs_you_count(&home, &alice()).unwrap(), 1);

    // So does a run stopped before it started: it never ran.
    let stopped = manual(&home, "triage").run_id;
    assert!(service::stop_run(&home, &alice(), &stopped, launcher.as_ref()).unwrap());
    assert_eq!(run(&home, &stopped).phase, "cancelled");
    assert_eq!(service::needs_you_count(&home, &alice()).unwrap(), 1);
    let summary = needs_you("triage");
    assert!(summary.needs_you);
    assert_eq!(summary.last_run.unwrap().run_id, first);

    // The next run that ends another way clears it; nothing is acknowledged.
    let second = manual(&home, "triage").run_id;
    drain_claim_loop(&home, launcher.clone()).await;
    let session = launcher.last().session_id;
    // While it runs, the row still needs you and still carries what was asked.
    let summary = needs_you("triage");
    assert!(summary.needs_you);
    assert_eq!(summary.last_run.unwrap().phase, "running");
    let asked = summary.needs_you_run.unwrap();
    assert_eq!(
        (asked.run_id.as_str(), asked.result_text.as_deref()),
        (first.as_str(), Some("which repo?"))
    );
    launcher.signal_done(&session, "success", "done").await;
    assert_eq!(run(&home, &second).phase, "succeeded");
    assert_eq!(service::needs_you_count(&home, &alice()).unwrap(), 0);
    let summary = needs_you("triage");
    assert!(!summary.needs_you);
    assert_eq!(summary.needs_you_run, None);
    assert_eq!(summary.last_run.unwrap().run_id, second);
    launcher.exit(&session).await;

    assert_eq!(files_named(temp.root(), "weft.sqlite3"), 0);
}

#[test]
fn history_pages_newest_first_through_skipped_and_coalesced_fires() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    write_org(home, 1);
    let mut def = routine("co", "0 9 * * *");
    def.policy.overlap = OverlapPolicy::Coalesce;
    save(home, def);
    save(home, routine("other", "0 9 * * *"));

    let active = manual(home, "co").run_id;
    let folded = manual(home, "co").run_id;
    service::set_enabled(home, &alice(), &key("co"), false).unwrap();
    let skipped = manual(home, "co").run_id;
    manual(home, "other");

    let first = service::runs(
        home,
        &alice(),
        &key("co"),
        RunsPage {
            limit: 2,
            before: None,
        },
    )
    .unwrap();
    let rows: Vec<_> = first
        .runs
        .iter()
        .map(|r| {
            (
                r.run_id.as_str(),
                r.phase.as_str(),
                r.reason.as_deref(),
                r.linked_run_id.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            (skipped.as_str(), "skipped", Some("paused"), None),
            (folded.as_str(), "coalesced", None, Some(active.as_str())),
        ]
    );
    assert_eq!(first.next_before.as_deref(), Some(folded.as_str()));

    let second = service::runs(
        home,
        &alice(),
        &key("co"),
        RunsPage {
            limit: 2,
            before: first.next_before,
        },
    )
    .unwrap();
    assert_eq!(second.runs.len(), 1);
    assert_eq!(second.runs[0].run_id, active);
    assert_eq!(second.runs[0].phase, "queued");
    assert_eq!(second.next_before, None);

    // Neither a skip nor a merge is the routine's last run.
    let summary = service::list_summaries(home, &alice())
        .unwrap()
        .into_iter()
        .find(|s| s.key == key("co"))
        .unwrap();
    assert_eq!(summary.last_run, None);
    assert!(!summary.enabled);
    assert_eq!(
        summary.next_run_at, None,
        "a paused routine has no next run"
    );

    assert_eq!(
        service::runs(
            home,
            &alice(),
            &key("co"),
            RunsPage {
                limit: 2,
                before: Some("no-such-run".into()),
            },
        )
        .unwrap_err(),
        RoutineError::RunNotFound("no-such-run".into())
    );
    assert_eq!(
        service::run_detail(home, &alice(), "no-such-run").unwrap_err(),
        RoutineError::RunNotFound("no-such-run".into())
    );
    // Runs are read only through a routine the caller can see.
    let bob = crate::domain::Principal::from(2u64);
    assert_eq!(
        service::run_detail(home, &bob, &active).unwrap_err(),
        RoutineError::RunNotFound(active.clone())
    );
    assert_eq!(
        service::runs(home, &bob, &key("co"), RunsPage::default()).unwrap_err(),
        RoutineError::NotFound(key("co"))
    );
}

/// A run's question is for the Mac that ran it, from any personal space there,
/// creator or not, and stays there when the routine moves to another Mac.
#[test]
fn a_team_routines_question_is_for_the_mac_that_ran_it() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let alice_org = write_org(home, 1);
    let bob_org = write_org(home, 2);
    save(home, routine("triage", "0 9 * * *"));
    // The git pull that brings Alice's team routine to Bob.
    let routine_dir = |org: &std::path::Path| {
        super::defs::routine_dir(
            &crate::context_repo::path_to_team_config_dir(org, super::test_support::TEAM),
            "triage",
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
    let asked = manual(home, "triage").run_id;
    store(home)
        .transaction(|tx| {
            tx.execute(
                "UPDATE routine_runs SET phase = 'needs_you', started_at = 1, ended_at = 2,
                     result_text = 'which repo?' WHERE run_id = ?1",
                [&asked],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(service::needs_you_count(home, &alice()).unwrap(), 1);

    let bob = crate::domain::Principal::from(2u64);
    let seen = service::list_summaries(home, &bob).unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].last_run.as_ref().unwrap().run_id, asked);
    assert!(seen[0].needs_you, "asked on the Mac Bob uses");
    assert_eq!(
        seen[0].needs_you_run.as_ref().map(|r| r.run_id.as_str()),
        Some(asked.as_str())
    );
    assert_eq!(service::needs_you_count(home, &bob).unwrap(), 1);
    let history = service::runs(home, &bob, &key("triage"), RunsPage::default()).unwrap();
    assert_eq!(history.runs.len(), 1);
    assert_eq!(
        service::run_detail(home, &bob, &asked)
            .unwrap()
            .routine_name,
        "Routine triage"
    );

    // Moved to another Mac: the question its run here asked stays here.
    let mut moved = service::get(home, &alice(), &key("triage")).unwrap().def;
    moved.armed_host = "another-mac".into();
    moved.revision += 1;
    for org in [&alice_org, &bob_org] {
        super::defs::write_revision(
            &routine_dir(org),
            &moved,
            &Author::user("bob"),
            "2026-10-05T10:00:00Z",
        )
        .unwrap();
    }
    for who in [alice(), bob.clone()] {
        let seen = service::list_summaries(home, &who).unwrap();
        assert!(!seen[0].armed_here);
        assert!(seen[0].needs_you);
        assert_eq!(service::needs_you_count(home, &who).unwrap(), 1);
    }
}

#[test]
fn missed_occurrences_count_until_acknowledged_and_stay_in_history() {
    record_changes();
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    write_org(home, 1);
    save(home, routine("ack-poll", "*/5 * * * *"));
    let sessions: Arc<dyn super::finish::Sessions> = Arc::new(FakeSessions::default());

    // One app session seeds 08:05, then the app quits until 08:17.
    let clock = Arc::new(AtomicI64::new(ms("2026-10-05T08:00:30Z")));
    ticker(home, &clock, sessions.clone()).tick().unwrap();
    clock.store(ms("2026-10-05T08:17:00Z"), Ordering::SeqCst);
    let relaunched = ticker(home, &clock, sessions);
    clock.store(ms("2026-10-05T08:17:00Z") + 5_000, Ordering::SeqCst);
    relaunched.tick().unwrap();

    assert_eq!(
        service::scheduler_status(home, &alice())
            .unwrap()
            .missed_count,
        3
    );
    let history = service::runs(home, &alice(), &key("ack-poll"), RunsPage::default()).unwrap();
    assert_eq!(history.runs.len(), 1);
    assert_eq!(history.runs[0].phase, "skipped");
    assert_eq!(history.runs[0].reason.as_deref(), Some("missed:3"));

    let before = changes_of("acme/api/backend/ack-poll");
    service::acknowledge_missed(home, &alice()).unwrap();
    assert_eq!(
        service::scheduler_status(home, &alice())
            .unwrap()
            .missed_count,
        0
    );
    assert_eq!(changes_of("acme/api/backend/ack-poll"), before + 1);
    assert_eq!(
        service::runs(home, &alice(), &key("ack-poll"), RunsPage::default())
            .unwrap()
            .runs
            .len(),
        1,
        "acknowledging keeps the skipped run"
    );
    // Nothing left to acknowledge: no change to announce.
    service::acknowledge_missed(home, &alice()).unwrap();
    assert_eq!(changes_of("acme/api/backend/ack-poll"), before + 1);
}

#[tokio::test]
async fn a_routine_run_reads_its_own_pinned_context() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mut def = routine("ctx", "0 9 * * *");
    for (name, default) in [("channel", "#ops"), ("topic", "deploys")] {
        def.variables.insert(
            name.into(),
            Variable {
                var_type: Default::default(),
                default: Some(default.into()),
                extra: Default::default(),
            },
        );
    }
    save(&home, def);
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));

    let first = manual(&home, "ctx").run_id;
    drain_claim_loop(&home, launcher.clone()).await;
    let session = launcher.last().session_id;
    // An edit while it runs does not change what it is running.
    let mut edited = service::get(&home, &alice(), &key("ctx")).unwrap().def;
    edited.task = "Do the new thing.".into();
    service::save(
        &home,
        &alice(),
        SaveTarget::Update {
            key: key("ctx"),
            base_revision: 1,
        },
        edited,
        &Author::user("alice"),
    )
    .unwrap();

    let context = service::context_for_session(&home, &session).unwrap();
    assert_eq!(context.run_id, first);
    assert_eq!(context.routine_key, key("ctx"));
    assert_eq!(context.routine_name, "Routine ctx");
    assert_eq!(context.revision, 1);
    assert_eq!(context.routine.revision, 1);
    assert_eq!(context.routine.task, routine("ctx", "0 9 * * *").task);
    assert_eq!(context.source, "manual");
    assert!(context.fired_at > 0);
    assert_eq!(context.trigger_payload, None);
    assert_eq!(context.last_run_summary, None);
    assert_eq!(context.last_run_id, None);
    assert!(
        context
            .content_dir
            .as_ref()
            .is_some_and(|dir| dir.is_absolute()),
        "a team routine is told its team's content folder"
    );
    let values = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    };
    assert_eq!(
        context.variables,
        values(&[("channel", "#ops"), ("topic", "deploys")]),
        "the declared defaults the task rendered with"
    );
    launcher.signal_done(&session, "success", "ok").await;
    launcher.exit(&session).await;

    // A fired run carries its payload, and sees the previous run's result.
    let def = service::get(&home, &alice(), &key("ctx")).unwrap().def;
    let payload = serde_json::json!({"agent": "triage", "run_id": "r-1"});
    let admitted = store(&home)
        .transaction(|tx| {
            admit::admit(
                tx,
                &home,
                &Fire {
                    key: &key("ctx"),
                    principal: &alice(),
                    def: &def,
                    source: RunSource::Manual,
                    occurrence_key: None,
                    scheduled_at: None,
                    now: super::finish::now_ms(),
                    variables: values(&[("channel", "#dev"), ("undeclared", "x")]),
                    trigger_payload: Some(payload.clone()),
                    cause: None,
                },
            )
        })
        .unwrap();
    drain_claim_loop(&home, launcher.clone()).await;
    let session = launcher.last().session_id;
    let context = service::context_for_session(&home, &session).unwrap();
    assert_eq!(context.run_id, admitted.run_id);
    assert_eq!(context.revision, 2);
    assert_eq!(context.routine.task, "Do the new thing.");
    assert_eq!(context.trigger_payload, Some(payload));
    assert_eq!(context.last_run_summary.as_deref(), Some("ok"));
    assert_eq!(context.last_run_id.as_deref(), Some(first.as_str()));
    assert_eq!(
        context.variables,
        values(&[("channel", "#dev"), ("topic", "deploys")])
    );
    launcher.signal_done(&session, "failed", "broke").await;
    launcher.exit(&session).await;
    assert_eq!(run(&home, &admitted.run_id).phase, "failed");

    // After a failed run, the previous run is still the one whose result the
    // summary is, not the failed run.
    let third = manual(&home, "ctx").run_id;
    drain_claim_loop(&home, launcher.clone()).await;
    let session = launcher.last().session_id;
    let context = service::context_for_session(&home, &session).unwrap();
    assert_eq!(context.run_id, third);
    assert_eq!(context.last_run_summary.as_deref(), Some("ok"));
    assert_eq!(context.last_run_id.as_deref(), Some(first.as_str()));
    launcher.signal_done(&session, "success", "fresh").await;
    // What this run was told does not change once it has a result of its own.
    let context = service::context_for_session(&home, &session).unwrap();
    assert_eq!(context.last_run_id.as_deref(), Some(first.as_str()));
    launcher.exit(&session).await;

    assert_eq!(
        service::context_for_session(&home, "shell-not-a-routine").unwrap_err(),
        RoutineError::NotARoutineRun("shell-not-a-routine".into())
    );
    assert_eq!(files_named(temp.root(), "weft.sqlite3"), 0);
}

#[test]
fn a_schedule_preview_is_the_words_and_the_next_fires() {
    let now = at("2026-10-05T09:01:00Z");
    let preview = service::preview_schedule_at("*/5 * * * *", "UTC", 3, now).unwrap();
    assert_eq!(preview.in_words, "every 5 minutes");
    assert_eq!(preview.tz, "UTC");
    assert_eq!(
        preview.next_fires,
        vec![
            ms("2026-10-05T09:05:00Z"),
            ms("2026-10-05T09:10:00Z"),
            ms("2026-10-05T09:15:00Z"),
        ]
    );
    assert_eq!(
        service::preview_schedule_at("0 9 * * *", "UTC", 500, now)
            .unwrap()
            .next_fires
            .len(),
        service::MAX_PREVIEW
    );
    assert_eq!(
        service::preview_schedule_at("0 9 * * *", "", 1, now)
            .unwrap()
            .tz,
        super::schedule::system_zone()
    );
    assert!(matches!(
        service::preview_schedule_at("* * * * *", "UTC", 3, now),
        Err(RoutineError::Validation(ValidationError::InvalidSchedule(
            ScheduleError::IntervalTooShort { .. }
        )))
    ));
}

#[test]
fn every_kind_of_trigger_reads_in_words() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    write_org(home, 1);
    let mut manual_only = routine("by-hand", "0 9 * * *");
    manual_only.triggers.clear();
    save(home, manual_only);
    let mut two = routine("twice", "0 9 * * 1-5");
    two.triggers.push(RoutineTrigger::Schedule(ScheduleSpec {
        cron: "0 18 * * *".into(),
        tz: "UTC".into(),
    }));
    save(home, two);

    let now = at("2026-10-05T12:00:00Z");
    let listed = service::list_summaries_at(home, &alice(), now).unwrap();
    let find = |id: &str| listed.iter().find(|s| s.key == key(id)).unwrap();
    assert_eq!(find("by-hand").schedule_in_words, "manual only");
    assert_eq!(find("by-hand").next_run_at, None);
    let twice = find("twice");
    assert!(
        twice.schedule_in_words.contains("; "),
        "{}",
        twice.schedule_in_words
    );
    assert_eq!(
        twice.next_run_at,
        Some(ms("2026-10-05T18:00:00Z")),
        "the earliest of its schedules"
    );
}

#[test]
fn every_definition_and_local_state_write_announces_a_change() {
    record_changes();
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    write_org(home, 1);
    let changes = || changes_of("acme/api/backend/evt-write");

    save(home, routine("evt-write", "0 9 * * *"));
    assert_eq!(changes(), 1, "create");
    service::set_enabled(home, &alice(), &key("evt-write"), false).unwrap();
    assert_eq!(changes(), 2, "pause");
    let def = service::get(home, &alice(), &key("evt-write")).unwrap().def;
    service::save(
        home,
        &alice(),
        SaveTarget::Update {
            key: key("evt-write"),
            base_revision: 1,
        },
        def,
        &Author::user("alice"),
    )
    .unwrap();
    assert_eq!(changes(), 3, "update");
    service::restore(
        home,
        &alice(),
        &key("evt-write"),
        1,
        2,
        &Author::user("alice"),
    )
    .unwrap();
    assert_eq!(changes(), 4, "restore");
    service::archive(home, &alice(), &key("evt-write"), &Author::user("alice")).unwrap();
    assert_eq!(changes(), 5, "archive");
}

/// Two accounts on one Mac share the coordinator; a personal routine of the
/// same id in each is still two routines with nothing in common.
#[tokio::test]
async fn same_named_personal_routines_of_two_accounts_share_nothing() {
    record_changes();
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let accounts = [
        (alice(), "alice"),
        (crate::domain::Principal::from(2u64), "bob"),
    ];
    let mut keys = Vec::new();
    for (who, name) in &accounts {
        // Personal routines are filed under an org each account has on this machine
        // (the temp home already holds Alice's).
        if *who != alice() {
            super::test_support::write_org(&home, who.as_str().parse().unwrap());
        }
        // Each account's own personal `default` agent runs its routine.
        super::test_support::save_personal_agent(
            &home,
            who,
            super::test_support::agent(
                super::test_support::AGENT,
                codesense_pty_core::agent_cli::AgentCli::Claude,
            ),
        );
        let saved = service::save(
            &home,
            who,
            SaveTarget::Create(Placement::Personal(PersonalOwner {
                github_user_id: who.as_str().parse().unwrap(),
                username: (*name).into(),
                display_name: (*name).into(),
                avatar_url: None,
                org: "acme/api".into(),
            })),
            routine("standup", "0 9 * * *"),
            &Author::user(*name),
        )
        .unwrap();
        keys.push(saved.key);
    }
    assert_eq!(keys[0].to_string(), "_personal/1/acme/api/_scratch/standup");
    assert_eq!(keys[1].to_string(), "_personal/2/acme/api/_scratch/standup");
    // No team org can render like a personal key.
    assert!(matches!(
        service::save(
            &home,
            &alice(),
            SaveTarget::Create(Placement::Team {
                org: "_personal/2".into(),
                team: "personal".into(),
            }),
            routine("standup", "0 9 * * *"),
            &Author::user("alice"),
        ),
        Err(RoutineError::InvalidLocation(_))
    ));

    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let clock = Arc::new(AtomicI64::new(ms("2026-01-05T08:00:00Z")));
    let tickers: Vec<_> = accounts
        .iter()
        .map(|(who, _)| ticker_as(&home, &clock, launcher.clone(), who.clone()))
        .collect();
    for ticker in &tickers {
        assert_eq!(ticker.tick().unwrap().seeded, 1);
    }
    let mut cursors: Vec<_> = store(&home)
        .triggers()
        .unwrap()
        .into_iter()
        .map(|t| t.trigger_id)
        .collect();
    cursors.sort();
    assert_eq!(
        cursors,
        vec![format!("{}#0", keys[0]), format!("{}#0", keys[1])]
    );

    // Each account's scheduled fire succeeds with its own summary, then a run
    // by hand (stamped with the real clock, after the fake one) is told that
    // summary and ends needing its owner.
    clock.store(ms("2026-01-05T09:00:10Z"), Ordering::SeqCst);
    let mut asked = Vec::new();
    for ((who, name), ticker) in accounts.iter().zip(&tickers) {
        let changed = ticker.tick().unwrap().changed;
        assert_eq!(changed.len(), 1, "{name}'s fire");
        let scheduled = changed[0].1.clone();
        assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
        let session = launcher.last().session_id;
        launcher
            .signal_done(&session, "success", &format!("{name} done"))
            .await;
        launcher.exit(&session).await;
        assert_eq!(
            run(&home, &scheduled).occurrence_key,
            Some(format!("{}@2026-01-05T09:00:00Z", changed[0].0))
        );

        let key = &keys[asked.len()];
        let by_hand = service::run_now(&home, who, key, BTreeMap::new())
            .unwrap()
            .run_id;
        drain_claim_loop(&home, launcher.clone()).await;
        let session = launcher.last().session_id;
        let context = service::context_for_session(&home, &session).unwrap();
        assert_eq!(&context.routine_key, key);
        assert_eq!(context.last_run_summary, Some(format!("{name} done")));
        assert_eq!(context.last_run_id.as_deref(), Some(scheduled.as_str()));
        launcher
            .signal_done(&session, "needs-attention", &format!("{name} asks"))
            .await;
        launcher.exit(&session).await;
        asked.push((scheduled, by_hand, session));
    }

    // A run reads its own context whoever is signed in now: Alice's run, after
    // Bob's ran and with the same routine id in his personal space.
    let alices = service::context_for_session(&home, &asked[0].2).unwrap();
    assert_eq!(alices.routine_key, keys[0]);
    assert_eq!(alices.last_run_summary.as_deref(), Some("alice done"));
    // A dispatch request naming another account than the routine's never
    // reads that account's personal space.
    let coordinator =
        rusqlite::Connection::open(crate::coordinator::default_store_path(&home)).unwrap();
    coordinator
        .execute(
            "UPDATE run_requests SET request = replace(request, '\"principal\":\"1\"', '\"principal\":\"2\"')
             WHERE run_id = ?1",
            [&asked[0].1],
        )
        .unwrap();
    assert_eq!(
        service::context_for_session(&home, &asked[0].2).unwrap_err(),
        RoutineError::NotFound(keys[0].clone())
    );

    for (i, (who, name)) in accounts.iter().enumerate() {
        let (mine, theirs) = (&keys[i], &keys[1 - i]);
        let (scheduled, by_hand, _) = &asked[i];
        let listed = service::list_summaries(&home, who).unwrap();
        assert_eq!(listed.len(), 1, "{name}");
        assert_eq!(&listed[0].key, mine);
        assert_eq!(listed[0].last_run.as_ref().unwrap().run_id, *by_hand);
        assert_eq!(
            listed[0].needs_you_run.as_ref().unwrap().result_text,
            Some(format!("{name} asks"))
        );
        assert_eq!(service::needs_you_count(&home, who).unwrap(), 1);
        let history = service::runs(&home, who, mine, RunsPage::default()).unwrap();
        let ids: Vec<_> = history.runs.iter().map(|r| r.run_id.clone()).collect();
        assert_eq!(ids, vec![by_hand.clone(), scheduled.clone()], "{name}");
        assert_eq!(
            service::run_detail(&home, who, scheduled)
                .unwrap()
                .run
                .result_text,
            Some(format!("{name} done"))
        );
        // The other account's routine is not found from here, by key or run.
        assert_eq!(
            service::runs(&home, who, theirs, RunsPage::default()).unwrap_err(),
            RoutineError::NotFound(theirs.clone())
        );
        assert_eq!(
            service::get(&home, who, theirs).unwrap_err(),
            RoutineError::NotFound(theirs.clone())
        );
        let (_, their_run, _) = &asked[1 - i];
        assert_eq!(
            service::run_detail(&home, who, their_run).unwrap_err(),
            RoutineError::RunNotFound(their_run.clone())
        );
        assert_eq!(
            service::stop_run(&home, who, their_run, launcher.as_ref()).unwrap_err(),
            RoutineError::RunNotFound(their_run.clone())
        );
        assert!(changes_of(&mine.to_string()) > 0);
    }
    assert_eq!(changes_of("_personal/_personal/personal/standup"), 0);

    // Pausing one leaves the other running.
    service::set_enabled(&home, &accounts[0].0, &keys[0], false).unwrap();
    assert!(!service::list_summaries(&home, &accounts[0].0).unwrap()[0].enabled);
    assert!(service::list_summaries(&home, &accounts[1].0).unwrap()[0].enabled);

    // Archiving one drops only its own cursor.
    service::archive(&home, &accounts[0].0, &keys[0], &Author::user("alice")).unwrap();
    tickers[0].tick().unwrap();
    let cursors: Vec<_> = store(&home)
        .triggers()
        .unwrap()
        .into_iter()
        .map(|t| t.trigger_id)
        .collect();
    assert_eq!(cursors, vec![format!("{}#0", keys[1])]);
    assert_eq!(files_named(temp.root(), "weft.sqlite3"), 0);
}

/// SLW-PT-19 seam (control plane): a personal routine filed under org A fires on
/// schedule and by hand in A's own scratch space while org B is on screen. Only the
/// Routines tab (a viewport) narrows to the active org.
#[tokio::test]
async fn a_personal_routine_runs_in_its_own_org_whatever_org_is_active() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let agent_key = super::test_support::save_personal_agent(
        &home,
        &alice(),
        super::test_support::agent(
            super::test_support::AGENT,
            codesense_pty_core::agent_cli::AgentCli::Claude,
        ),
    );
    let saved = service::save(
        &home,
        &alice(),
        SaveTarget::Create(Placement::Personal(PersonalOwner {
            github_user_id: 1,
            username: "alice".into(),
            display_name: "alice".into(),
            avatar_url: None,
            org: super::test_support::ORG.into(),
        })),
        routine("mine", "0 9 * * *"),
        &Author::user("alice"),
    )
    .unwrap();
    let space =
        crate::context_repo::scratch_space::org_space(&home, &alice(), super::test_support::ORG)
            .unwrap();
    // A run works in its own folder of the org's scratch space, never in its
    // `content/`: whatever the launch writes there (`.mcp.json`, CLI settings)
    // would otherwise be overlaid into every worktree of the org.
    let scratch = super::workspace::agent_dir(&space.routine_run_dir(&saved.key.id), &agent_key);
    assert!(!scratch.starts_with(space.content_dir()));

    // Org B is on screen from here on.
    crate::context_repo::scratch_space::set_active_org(&home, Some("other/org"));
    super::discover::invalidate_discovery_cache(&home);
    assert!(
        !service::list_summaries(&home, &alice())
            .unwrap()
            .iter()
            .any(|r| r.key == saved.key),
        "the Routines tab (viewport) shows only the active org's personal routines"
    );

    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let clock = Arc::new(AtomicI64::new(ms("2026-01-05T08:00:00Z")));
    let ticker = ticker_as(&home, &clock, launcher.clone(), alice());
    assert_eq!(
        ticker.tick().unwrap().seeded,
        1,
        "the scheduler arms it anyway"
    );
    clock.store(ms("2026-01-05T09:00:10Z"), Ordering::SeqCst);
    let fired = ticker.tick().unwrap().changed;
    assert_eq!(
        fired.len(),
        1,
        "it fires on schedule with another org active"
    );
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    assert_eq!(
        launcher.last().request.cwd,
        scratch,
        "in its own org's scratch space"
    );
    assert_eq!(launcher.last().request.org, super::test_support::ORG);
    assert!(scratch.is_dir(), "the run folder is made for the run");
    // The agent is told where the org's private content is, as an absolute path.
    let context = service::context_for_session(&home, &launcher.last().session_id).unwrap();
    assert_eq!(context.content_dir, Some(space.content_dir()));
    launcher.exit(&launcher.last().session_id).await;

    service::run_now(&home, &alice(), &saved.key, BTreeMap::new()).unwrap();
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    assert_eq!(launcher.last().request.cwd, scratch);
    launcher.exit(&launcher.last().session_id).await;
}

/// SLW-PT-28 AC-3: deleting (archiving) a personal routine removes its run
/// folder, and a run of another routine sweeps away run folders that belong to
/// no live routine, so `.run/` never grows without bound.
#[test]
fn deleting_a_personal_routine_removes_its_run_folder() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let save = |id: &str| {
        service::save(
            &home,
            &alice(),
            SaveTarget::Create(Placement::Personal(PersonalOwner {
                github_user_id: 1,
                username: "alice".into(),
                display_name: "alice".into(),
                avatar_url: None,
                org: super::test_support::ORG.into(),
            })),
            routine(id, "0 9 * * *"),
            &Author::user("alice"),
        )
        .unwrap()
    };
    let (gone, kept) = (save("gone"), save("kept"));
    let run_dir = |saved: &service::SavedRoutine| {
        super::target::resolve(&home, &alice(), &saved.key, &saved.def).unwrap()
    };
    let gone_dir = run_dir(&gone);
    let agent_key = gone.def.agent.named_key(&gone.key).unwrap();
    let gone_leaf = super::workspace::agent_dir(&gone_dir, &agent_key);
    std::fs::create_dir_all(&gone_leaf).unwrap();
    std::fs::write(gone_leaf.join(".mcp.json"), "{}").unwrap();
    assert!(gone_dir.is_dir());

    service::archive(&home, &alice(), &gone.key, &Author::user("alice")).unwrap();
    assert!(
        !gone_dir.exists(),
        "the deleted routine's run folder is removed"
    );

    // A folder left by a routine deleted elsewhere is swept by the next run.
    let ghost = gone_dir.with_file_name("ghost");
    let ghost_leaf = super::workspace::agent_dir(&ghost, &agent_key);
    std::fs::create_dir_all(&ghost_leaf).unwrap();
    std::fs::write(ghost_leaf.join(".mcp.json"), "{}").unwrap();
    let kept_dir = run_dir(&kept);
    assert!(kept_dir.is_dir());
    assert!(!ghost.exists(), "an orphan run folder is swept");
}

/// A routine deleted mid-run keeps its run folder while the agent works in it;
/// the first sweep after that run ends removes it.
#[tokio::test]
async fn a_routine_deleted_mid_run_keeps_its_run_folder_until_the_run_ends() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    super::test_support::save_personal_agent(
        &home,
        &alice(),
        super::test_support::agent(
            super::test_support::AGENT,
            codesense_pty_core::agent_cli::AgentCli::Claude,
        ),
    );
    let save = |id: &str| {
        service::save(
            &home,
            &alice(),
            SaveTarget::Create(Placement::Personal(PersonalOwner {
                github_user_id: 1,
                username: "alice".into(),
                display_name: "alice".into(),
                avatar_url: None,
                org: super::test_support::ORG.into(),
            })),
            routine(id, "0 9 * * *"),
            &Author::user("alice"),
        )
        .unwrap()
    };
    let (busy, other) = (save("busy"), save("other"));
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    service::run_now(&home, &alice(), &busy.key, BTreeMap::new()).unwrap();
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    let busy_dir = launcher.last().request.cwd.clone();
    assert!(busy_dir.is_dir());

    service::archive(&home, &alice(), &busy.key, &Author::user("alice")).unwrap();
    assert!(busy_dir.is_dir(), "kept while its run is live");
    super::target::resolve(&home, &alice(), &other.key, &other.def).unwrap();
    assert!(
        busy_dir.is_dir(),
        "the sweep skips it while its run is live"
    );

    launcher.exit(&launcher.last().session_id).await;
    super::target::resolve(&home, &alice(), &other.key, &other.def).unwrap();
    assert!(
        !busy_dir.exists(),
        "the next sweep after the run ends removes it"
    );
}
