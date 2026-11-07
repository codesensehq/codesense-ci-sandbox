// SPDX-License-Identifier: MIT
//! Seam tests for `agent_done` routines: the real dashboard-stream producer
//! under a mock app, admission's loop guard over the real coordinator, and the
//! claim loop rendering the event into the run's task.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tauri::Manager;

use crate::agents::session_events::spawn_session_status_producer;
use crate::commands::dashboard_launch::{DashboardLaunchState, RunProvenance, SessionOwner};
use crate::dashboard_stream::{DashboardStreamHub, SessionStatus};
use crate::domain::Principal;

use super::admit::Outcome;
use super::discover::discover_routines;
use super::event::{self, AgentDoneEvent, Scope};
use super::finish;
use super::model::{AgentRef, EventSpec, RoutineDef, RoutineKey, RoutineTrigger};
use super::run_seams::{key, manual, routine, run, save, store, ticker_as};
use super::runs::RoutineRun;
use super::service::{self, PersonalOwner, Placement, SaveTarget};
use super::test_support::{alice, drain_claim_loop, stub_agent, StubLauncher, TempHome, ORG, TEAM};

fn on_agent_done(id: &str, from_agent: Option<&str>) -> RoutineDef {
    let mut def = routine(id, "0 9 * * *");
    def.task = "Follow up on {{trigger.agent}}: {{trigger.summary}}".into();
    def.triggers = vec![RoutineTrigger::Event(EventSpec {
        kind: "agent_done".into(),
        from_agent: from_agent.map(AgentRef::from),
        role: None,
        metadata: None,
    })];
    def
}

/// The revision stamp of a launch of `agent`: a team agent by id, or any
/// agent by key, at revision 1.
fn stamp(agent: &str) -> String {
    if agent.contains('/') {
        format!("{agent}@1")
    } else {
        format!("{ORG}/{TEAM}/{agent}@1")
    }
}

fn runs_of(home: &Path, id: &str) -> Vec<RoutineRun> {
    store(home).runs(&key(id)).unwrap()
}

fn owner(principal: Option<&str>, org: &str, team: Option<&str>) -> SessionOwner {
    SessionOwner {
        principal: principal.map(str::to_string),
        org: org.into(),
        team: team.map(str::to_string),
    }
}

/// A session Alice launched in the routines' own team.
fn in_team() -> SessionOwner {
    owner(Some("1"), ORG, Some(TEAM))
}

fn by_session(runs: &[RoutineRun], key: &RoutineKey, session_id: &str) -> Option<RoutineRun> {
    let occurrence = format!("{key}@event:{session_id}");
    runs.iter()
        .find(|r| r.occurrence_key.as_deref() == Some(occurrence.as_str()))
        .cloned()
}

fn outcome(run: &RoutineRun) -> (&str, Option<&str>, Option<&str>) {
    (
        run.phase.as_str(),
        run.reason.as_deref(),
        run.result_text.as_deref(),
    )
}

/// A mock app running the real producer over its own stream hub and launch
/// registry, with a routine ticker signed in as Alice.
struct App {
    app: tauri::App<tauri::test::MockRuntime>,
}

impl App {
    fn new(home: &Path, clock: &Arc<AtomicI64>, launcher: Arc<StubLauncher>) -> Self {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        app.manage(DashboardStreamHub::new());
        app.manage(DashboardLaunchState::new(8));
        app.manage(ticker_as(home, clock, launcher, alice()));
        spawn_session_status_producer(app.handle().clone());
        Self { app }
    }

    /// Register `session_id` in the app's launch registry, as a launch spine
    /// does: launched in the routines' team as `agent`, as a run of
    /// `routine_key` when set.
    fn launched(&self, session_id: &str, agent: Option<&str>, routine_key: Option<&str>) {
        self.launched_as(session_id, agent, routine_key, Some(in_team()));
    }

    /// [`Self::launched`] by `owner`, or by a launch that recorded none.
    fn launched_as(
        &self,
        session_id: &str,
        agent: Option<&str>,
        routine_key: Option<&str>,
        owner: Option<SessionOwner>,
    ) {
        let launch = self.app.state::<DashboardLaunchState>();
        let handle = format!("handle-{session_id}");
        assert!(launch.begin(&handle).unwrap());
        launch
            .bind_with_account(&handle, session_id, Some("test"), None)
            .unwrap();
        if let Some(agent) = agent {
            launch.set_agent_revision_if_absent(&handle, stamp(agent));
        }
        if let Some(owner) = owner {
            launch.set_owner_if_absent(&handle, owner);
        }
        if let Some(routine_key) = routine_key {
            launch.stamp_provenance(
                &handle,
                RunProvenance {
                    run_id: format!("run-{session_id}"),
                    story_id: handle.clone(),
                    routine_key: Some(routine_key.to_string()),
                    ..Default::default()
                },
            );
        }
    }

    /// The agent's `signal_done` as the MCP handler fans it into the hub.
    fn signal(&self, session_id: &str, summary: &str) {
        self.app.state::<DashboardStreamHub>().on_session_status(
            session_id,
            "success",
            Some(summary.to_string()),
        );
    }
}

/// Wait for `id`'s runs to satisfy `done`, which the producer's detached
/// offers reach asynchronously.
async fn wait_for(home: &Path, id: &str, done: impl Fn(&[RoutineRun]) -> bool) -> Vec<RoutineRun> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let runs = runs_of(home, id);
        if done(&runs) || Instant::now() > deadline {
            return runs;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_reviewer_finishing_fires_its_routine_once_with_the_summary_in_the_task() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, on_agent_done("follow-ups", Some("reviewer")));
    save(&home, on_agent_done("probe", Some("prober")));
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let clock = Arc::new(AtomicI64::new(finish::now_ms()));
    let app = App::new(&home, &clock, launcher.clone());

    app.launched("rev-1", Some("reviewer"), None);
    app.signal("rev-1", "PR 12 needs two fixes");
    let runs = wait_for(&home, "follow-ups", |r| !r.is_empty()).await;
    assert_eq!(runs.len(), 1);
    let fired = &runs[0];
    assert_eq!(
        (
            fired.source.as_str(),
            fired.phase.as_str(),
            fired.reason.as_deref()
        ),
        ("event", "queued", None)
    );
    assert_eq!(
        fired.occurrence_key.as_deref(),
        Some("acme/api/backend/follow-ups@event:rev-1")
    );
    assert!(
        runs_of(&home, "probe").is_empty(),
        "another agent's trigger"
    );

    // The same delta again admits nothing. A later delta for the probe
    // routine shows the producer has moved past it.
    app.signal("rev-1", "PR 12 needs two fixes");
    app.launched("probe-1", Some("prober"), None);
    app.signal("probe-1", "probed");
    assert_eq!(wait_for(&home, "probe", |r| !r.is_empty()).await.len(), 1);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(runs_of(&home, "follow-ups").len(), 1);

    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 2);
    let prompts: Vec<String> = launcher
        .launched()
        .into_iter()
        .map(|l| l.request.prompt)
        .collect();
    assert!(
        prompts
            .iter()
            .any(|p| p.ends_with("Follow up on acme/api/backend/reviewer@1: PR 12 needs two fixes")),
        "{prompts:?}"
    );
    for launched in launcher.launched() {
        launcher.exit(&launched.session_id).await;
    }
}

#[tokio::test]
async fn a_routine_never_fires_itself_and_takes_one_event_per_window() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mut def = on_agent_done("storm", None);
    def.task = "Sweep what changed.".into();
    save(&home, def);
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let clock = Arc::new(AtomicI64::new(finish::now_ms()));
    let app = App::new(&home, &clock, launcher.clone());

    // The routine's own run, launched by the routine dispatcher, finishes.
    // This app's registry never saw it: the run log says whose it was.
    let own = manual(&home, "storm").run_id;
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    let own_session = launcher.last().session_id;
    launcher.signal_done(&own_session, "success", "done").await;
    launcher.exit(&own_session).await;
    app.signal(&own_session, "done");
    let runs = wait_for(&home, "storm", |r| r.len() == 2).await;
    let refused = runs.iter().find(|r| r.run_id != own).expect("event row");
    assert_eq!(
        (
            refused.source.as_str(),
            refused.phase.as_str(),
            refused.reason.as_deref()
        ),
        ("event", "skipped", Some("loop_guard"))
    );
    assert_eq!(
        refused.result_text,
        Some(format!("self-fire (finished session {own_session})"))
    );

    // So is a run of it the registry still holds, by its provenance.
    app.launched("own-2", None, Some("acme/api/backend/storm"));
    app.signal("own-2", "done again");
    let runs = wait_for(&home, "storm", |r| r.len() == 3).await;
    assert!(runs.iter().any(|r| r.occurrence_key.as_deref()
        == Some("acme/api/backend/storm@event:own-2")
        && r.reason.as_deref() == Some("loop_guard")));
    assert_eq!(launcher.calls(), 1, "nothing launched for itself");

    // Two other agents finish within the window: one run, one refusal.
    app.launched("ext-1", Some("dev"), None);
    app.signal("ext-1", "first");
    wait_for(&home, "storm", |r| r.len() == 4).await;
    clock.fetch_add(60_000, Ordering::SeqCst);
    app.launched("ext-2", Some("dev"), None);
    app.signal("ext-2", "second");
    let runs = wait_for(&home, "storm", |r| r.len() == 5).await;
    let by_session = |s: &str| {
        runs.iter()
            .find(|r| {
                r.occurrence_key.as_deref() == Some(&format!("acme/api/backend/storm@event:{s}"))
            })
            .unwrap()
            .clone()
    };
    assert_eq!(
        (
            by_session("ext-1").phase.as_str(),
            by_session("ext-1").reason
        ),
        ("queued", None)
    );
    let second = by_session("ext-2");
    assert_eq!(
        (second.phase.as_str(), second.reason.as_deref()),
        ("skipped", Some("loop_guard"))
    );
    assert_eq!(
        second.result_text.as_deref(),
        Some("loop:rate_budget (finished session ext-2)")
    );
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    assert_eq!(launcher.calls(), 2);
    launcher.exit(&launcher.last().session_id).await;
}

fn event(session_id: &str, agent: Option<&str>) -> AgentDoneEvent {
    AgentDoneEvent {
        session_id: session_id.into(),
        status: SessionStatus::Success,
        summary: None,
        agent: agent.map(stamp),
        routine_key: None,
        owner: Some(in_team()),
    }
}

#[test]
fn an_agent_done_trigger_matches_its_agent_or_any() {
    let named = on_agent_done("a", Some("reviewer"));
    let by_key = on_agent_done("k", Some("acme/api/backend/reviewer"));
    let any = on_agent_done("b", None);
    let scheduled = routine("c", "0 9 * * *");
    let team = key("a");
    let agent = |stamp: &str| crate::agents::AgentRevisionRef::key_of(&format!("{stamp}@1"));
    let reviewer = agent("acme/api/backend/reviewer");
    let reviewer = reviewer.as_ref();
    // A bare id is the routine's own team's agent; a key names it outright.
    assert!(event::matches(&team, &named, reviewer));
    assert!(event::matches(&team, &by_key, reviewer));
    assert!(!event::matches(
        &team,
        &named,
        agent("acme/api/backend/dev").as_ref()
    ));
    assert!(!event::matches(
        &team,
        &named,
        agent("acme/api/frontend/reviewer").as_ref()
    ));
    assert!(
        !event::matches(&team, &named, None),
        "a session launched without an agent"
    );
    // In a personal routine a bare id is the account's own personal agent.
    let mine = RoutineKey::personal(&alice(), "acme/api", "a");
    assert!(!event::matches(&mine, &named, reviewer));
    assert!(event::matches(
        &mine,
        &named,
        agent("_personal/1/acme/api/_scratch/reviewer").as_ref()
    ));
    assert!(event::matches(&mine, &by_key, reviewer));
    // A filter only waits: it may name another team's agent or a personal one.
    let elsewhere = on_agent_done("e", Some("acme/api/frontend/deployer"));
    assert!(event::matches(
        &team,
        &elsewhere,
        agent("acme/api/frontend/deployer").as_ref()
    ));
    let personal = on_agent_done("p", Some("_personal/1/acme/api/_scratch/mine"));
    assert!(event::matches(
        &team,
        &personal,
        agent("_personal/1/acme/api/_scratch/mine").as_ref()
    ));
    // No filter: any finished session, as before.
    assert!(event::matches(
        &team,
        &any,
        agent("acme/api/backend/dev").as_ref()
    ));
    assert!(event::matches(&team, &any, None));
    assert!(!event::matches(&team, &scheduled, reviewer));
}

#[test]
fn only_enabled_routines_armed_here_are_offered() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    for id in ["live", "paused", "elsewhere"] {
        save(&home, on_agent_done(id, None));
    }
    service::set_enabled(&home, &alice(), &key("paused"), false).unwrap();
    let mut routines = discover_routines(&home, &alice()).unwrap();
    for r in &mut routines {
        if r.key == key("elsewhere") {
            r.armed_here = false;
        }
    }
    let store = store(&home);
    let offered = event::offer(
        &(home.clone(), alice()),
        &routines,
        &store,
        &event("s-1", None),
        finish::now_ms(),
    )
    .unwrap();
    let keys: Vec<_> = offered.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, vec!["acme/api/backend/live"]);
    assert_eq!(offered[0].1.outcome, Outcome::Queued);
    assert!(runs_of(&home, "paused").is_empty());
    assert!(runs_of(&home, "elsewhere").is_empty());

    // The same session again is a duplicate, not a second run.
    let again = event::offer(
        &(home.clone(), alice()),
        &routines,
        &store,
        &event("s-1", None),
        finish::now_ms(),
    )
    .unwrap();
    assert_eq!(again[0].1.outcome, Outcome::Duplicate);
    assert_eq!(runs_of(&home, "live").len(), 1);
}

#[tokio::test]
async fn the_watchdog_bounds_a_run_by_its_own_max_runtime_whoever_is_signed_in() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mut def = routine("short", "0 9 * * *");
    def.policy.max_runtime = Some("1m".into());
    save(&home, def);
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let clock = Arc::new(AtomicI64::new(finish::now_ms()));
    // Another account is signed in: it cannot see Alice's routine at all.
    let ticker = ticker_as(&home, &clock, launcher.clone(), Principal::from(2u64));
    let run_id = manual(&home, "short").run_id;
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    let session_id = launcher.last().session_id;

    clock.fetch_add(2 * 60_000, Ordering::SeqCst);
    ticker.tick();
    assert_eq!(run(&home, &run_id).phase, "timed_out");
    assert!(!launcher.is_running(&session_id));
}

#[tokio::test]
async fn a_run_dispatched_for_another_account_fails_as_an_account_mismatch() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    // The personal agent its bare `agent` names (SW-AGR-1: one this Mac
    // lacks would leave the routine Unassigned, and nothing fires it).
    super::test_support::save_personal_agent(
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
            org: "acme/api".into(),
        })),
        routine("mine", "0 9 * * *"),
        &super::model::Author::user("alice"),
    )
    .unwrap();
    let run_id = service::run_now(&home, &alice(), &saved.key, BTreeMap::new())
        .unwrap()
        .run_id;
    rusqlite::Connection::open(crate::coordinator::default_store_path(&home))
        .unwrap()
        .execute(
            "UPDATE run_requests SET request = replace(request, '\"principal\":\"1\"', '\"principal\":\"2\"')
             WHERE run_id = ?1",
            [&run_id],
        )
        .unwrap();
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    let row = run(&home, &run_id);
    assert_eq!(
        (row.phase.as_str(), row.reason.as_deref()),
        ("failed", Some("account_mismatch"))
    );
    assert_eq!(launcher.calls(), 0);
}

#[tokio::test]
async fn two_routines_never_fire_each_other_round() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    for id in ["ping", "pong"] {
        let mut def = on_agent_done(id, None);
        def.task = "Sweep what changed.".into();
        save(&home, def);
    }
    let routines = discover_routines(&home, &alice()).unwrap();
    let store = store(&home);
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let now = finish::now_ms();
    let outcome_of = |offered: &[(String, super::admit::Admission)], id: &str| {
        let key = key(id).to_string();
        let run_id = &offered.iter().find(|(k, _)| *k == key).unwrap().1.run_id;
        let row = run(&home, run_id);
        (row.phase, row.result_text)
    };

    // Ping runs by hand; its agent finishing fires pong, not ping.
    manual(&home, "ping");
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    let ping_session = launcher.last().session_id;
    let offered = event::offer(
        &(home.clone(), alice()),
        &routines,
        &store,
        &event(&ping_session, None),
        now,
    )
    .unwrap();
    assert_eq!(
        outcome_of(&offered, "ping"),
        (
            "skipped".into(),
            Some(format!("self-fire (finished session {ping_session})"))
        )
    );
    assert_eq!(outcome_of(&offered, "pong").0, "queued");

    // Pong's agent finishing a minute later: ping is behind it, so ping is a
    // cycle, and pong's own run is named a self-fire though its rate is spent.
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    let pong_session = launcher.last().session_id;
    let offered = event::offer(
        &(home.clone(), alice()),
        &routines,
        &store,
        &event(&pong_session, None),
        now + 60_000,
    )
    .unwrap();
    assert_eq!(
        outcome_of(&offered, "ping"),
        (
            "skipped".into(),
            Some(format!("cycle (finished session {pong_session})"))
        )
    );
    assert_eq!(
        outcome_of(&offered, "pong"),
        (
            "skipped".into(),
            Some(format!("self-fire (finished session {pong_session})"))
        )
    );
    assert_eq!(launcher.calls(), 2);
    for launched in launcher.launched() {
        launcher.exit(&launched.session_id).await;
    }
}

#[test]
fn a_session_is_in_reach_of_its_own_team_or_its_launching_accounts_routines() {
    let team = key("t");
    let personal = RoutineKey {
        org: "acme/api".into(),
        team: crate::context_repo::scratch_space::SCRATCH_TEAM.into(),
        id: "p".into(),
        account: Some("1".into()),
    };
    let cases = [
        (Some(in_team()), Scope::In, Scope::In),
        // Another account's agent in the same team.
        (
            Some(owner(Some("2"), ORG, Some(TEAM))),
            Scope::In,
            Scope::Out,
        ),
        (
            Some(owner(Some("1"), ORG, Some("frontend"))),
            Scope::Out,
            Scope::In,
        ),
        // The account's own agent in ANOTHER org: never in a personal routine's reach.
        (
            Some(owner(Some("1"), "globex", Some(TEAM))),
            Scope::Out,
            Scope::Out,
        ),
        (Some(owner(Some("1"), ORG, None)), Scope::Unknown, Scope::In),
        (
            Some(owner(None, ORG, Some(TEAM))),
            Scope::In,
            Scope::Unknown,
        ),
        (
            Some(owner(None, "globex", None)),
            Scope::Out,
            Scope::Unknown,
        ),
        (None, Scope::Unknown, Scope::Unknown),
    ];
    for (owner, of_team, of_personal) in cases {
        assert_eq!(
            (
                event::scope(&team, owner.as_ref()),
                event::scope(&personal, owner.as_ref())
            ),
            (of_team, of_personal),
            "{owner:?}"
        );
    }
}

#[tokio::test]
async fn a_team_routine_fires_only_on_agents_that_finished_in_its_team() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, on_agent_done("triage", None));
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let clock = Arc::new(AtomicI64::new(finish::now_ms()));
    let app = App::new(&home, &clock, launcher);

    let elsewhere = [
        ("other-team", owner(Some("1"), ORG, Some("frontend"))),
        ("other-org", owner(Some("1"), "globex", Some(TEAM))),
    ];
    for (session, launched_by) in elsewhere.clone() {
        app.launched_as(session, Some("dev"), None, Some(launched_by));
        app.signal(session, "done");
    }
    app.launched_as(
        "no-team",
        Some("dev"),
        None,
        Some(owner(Some("1"), ORG, None)),
    );
    app.signal("no-team", "done");
    app.launched_as("unrecorded", Some("dev"), None, None);
    app.signal("unrecorded", "done");
    // A teammate's agent in this team is in reach whoever launched it.
    app.launched_as(
        "teammate",
        Some("dev"),
        None,
        Some(owner(Some("2"), ORG, Some(TEAM))),
    );
    app.signal("teammate", "done");

    wait_for(&home, "triage", |r| !r.is_empty()).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let runs = runs_of(&home, "triage");
    assert_eq!(runs.len(), 1, "{runs:?}");
    let triage = key("triage");
    // A session whose team is not known leaves no row, like one out of reach:
    // a skip for every hand-started agent would bury the routine's history.
    for session in ["other-team", "other-org", "no-team", "unrecorded"] {
        assert!(by_session(&runs, &triage, session).is_none(), "{session}");
    }
    let fired = by_session(&runs, &triage, "teammate").expect("teammate");
    assert_eq!(outcome(&fired), ("queued", None, None));
}

#[tokio::test]
async fn a_personal_routine_fires_on_its_accounts_agents_in_any_team_of_its_own_org() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    // The personal agent its bare `agent` names (SW-AGR-1: one this Mac
    // lacks would leave the routine Unassigned, and nothing fires it).
    super::test_support::save_personal_agent(
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
            org: "acme/api".into(),
        })),
        on_agent_done("mine", None),
        &super::model::Author::user("alice"),
    )
    .unwrap();
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let clock = Arc::new(AtomicI64::new(finish::now_ms()));
    let app = App::new(&home, &clock, launcher);

    app.launched_as(
        "bob",
        Some("dev"),
        None,
        Some(owner(Some("2"), ORG, Some(TEAM))),
    );
    app.signal("bob", "done");
    app.launched_as(
        "signed-out",
        Some("dev"),
        None,
        Some(owner(None, ORG, Some(TEAM))),
    );
    app.signal("signed-out", "done");
    app.launched_as(
        "alice-elsewhere",
        Some("dev"),
        None,
        Some(owner(Some("1"), "globex", Some("ops"))),
    );
    app.signal("alice-elsewhere", "done");
    // Same org, another team: a personal routine is filed under the ORG, so it
    // reaches its account's sessions in every team of that org.
    app.launched_as(
        "alice-other-team",
        Some("dev"),
        None,
        Some(owner(Some("1"), ORG, Some("platform"))),
    );
    app.signal("alice-other-team", "done");

    let personal = store(&home);
    let read = || personal.runs(&saved.key).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while read().is_empty() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let runs = read();
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert!(by_session(&runs, &saved.key, "bob").is_none());
    assert!(
        by_session(&runs, &saved.key, "signed-out").is_none(),
        "no launching account: not known to be this account's agent"
    );
    assert!(
        by_session(&runs, &saved.key, "alice-elsewhere").is_none(),
        "SLW-PT-19: another org's session never fires (or leaks into) org A's personal routine"
    );
    assert_eq!(
        outcome(&by_session(&runs, &saved.key, "alice-other-team").unwrap()),
        ("queued", None, None)
    );
}

// SLW-PT-20 AC-2: "Use my scratch items in ‹Team›" off — a personal routine no longer
// acts on its account's sessions in THAT team (the session's own team, from the work),
// while another team of the same org still fires it.
#[tokio::test]
async fn a_personal_routine_skips_sessions_of_a_team_with_scratch_items_off() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    // The personal agent its bare `agent` names (SW-AGR-1: one this Mac
    // lacks would leave the routine Unassigned, and nothing fires it).
    super::test_support::save_personal_agent(
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
            org: "acme/api".into(),
        })),
        on_agent_done("mine", None),
        &super::model::Author::user("alice"),
    )
    .unwrap();
    crate::context_repo::scratch_space::set_team_uses_scratch(&home, &alice(), ORG, TEAM, false)
        .unwrap();
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let clock = Arc::new(AtomicI64::new(finish::now_ms()));
    let app = App::new(&home, &clock, launcher);

    app.launched_as(
        "alice-off-team",
        Some("dev"),
        None,
        Some(owner(Some("1"), ORG, Some(TEAM))),
    );
    app.signal("alice-off-team", "done");
    app.launched_as(
        "alice-other-team",
        Some("dev"),
        None,
        Some(owner(Some("1"), ORG, Some("platform"))),
    );
    app.signal("alice-other-team", "done");

    let personal = store(&home);
    let read = || personal.runs(&saved.key).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while read().is_empty() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let runs = read();
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert!(
        by_session(&runs, &saved.key, "alice-off-team").is_none(),
        "the team switched scratch items off"
    );
    assert!(by_session(&runs, &saved.key, "alice-other-team").is_some());
}

#[test]
fn only_fires_that_ran_count_against_the_event_rate() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, on_agent_done("r", None));
    let routines = discover_routines(&home, &alice()).unwrap();
    let store = store(&home);
    let now = finish::now_ms();
    let offer = |event: AgentDoneEvent, at: i64| {
        let offered =
            event::offer(&(home.clone(), alice()), &routines, &store, &event, at).unwrap();
        assert_eq!(offered.len(), 1);
        run(&home, &offered[0].1.run_id)
    };

    let unknown = AgentDoneEvent {
        owner: None,
        ..event("s-1", None)
    };
    assert!(
        event::offer(&(home.clone(), alice()), &routines, &store, &unknown, now)
            .unwrap()
            .is_empty(),
        "a session of unknown scope is not offered"
    );

    let fired = offer(event("s-2", None), now + 1_000);
    assert_eq!(outcome(&fired), ("queued", None, None));
    assert!(fired.causation.is_some(), "a run keeps its chain");
    let limited = offer(event("s-3", None), now + 2_000);
    assert_eq!(limited.reason.as_deref(), Some("loop_guard"));
    assert_eq!(limited.causation, None, "a refused fire keeps no chain");
    let next_window = offer(event("s-4", None), now + 1_000 + 5 * 60 * 1000 + 1);
    assert_eq!(
        next_window.phase, "coalesced",
        "the first run is still active"
    );
}

#[tokio::test]
async fn an_interactive_launch_bound_with_its_team_fires_routines_without_a_named_agent() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save(&home, on_agent_done("any-agent", None));
    save(&home, on_agent_done("dev-agent", Some("dev")));
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let clock = Arc::new(AtomicI64::new(finish::now_ms()));
    let app = App::new(&home, &clock, launcher);
    // An interactive dashboard launch reserves its handle and binds the spawned
    // terminal from the frontend, passing the story's team key.
    let bind = |handle: &str, session_id: &str, team_key: Option<&str>| {
        let launch = app.app.state::<DashboardLaunchState>();
        let fresh = launch.resolve(handle).as_deref() != Some(session_id);
        if launch.resolve(handle).is_none() {
            assert!(launch.begin(handle).unwrap());
        }
        launch
            .bind_with_account(handle, session_id, Some("doc.md"), None)
            .unwrap();
        crate::commands::dashboard_launch::stamp_bound_launch(
            &launch,
            handle,
            fresh,
            Some("1".into()),
            team_key,
        );
    };
    let team_key = format!("{ORG}:{TEAM}");
    bind("SW-1", "story-dev", Some(&team_key));
    // A bind that says nothing about where it ran, as before.
    bind("SW-2", "unscoped", None);
    app.signal("unscoped", "done");
    app.signal("story-dev", "done");

    let any = wait_for(&home, "any-agent", |r| !r.is_empty()).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(any.len(), 1, "{any:?}");
    let fired = by_session(&any, &key("any-agent"), "story-dev").expect("any-agent");
    assert_eq!(outcome(&fired), ("queued", None, None));
    // A trigger naming agent `dev` waits for a session launched as that agent.
    assert!(
        runs_of(&home, "dev-agent").is_empty(),
        "a launch that recorded no agent matches no from_agent"
    );
    assert_eq!(
        runs_of(&home, "any-agent").len(),
        1,
        "the unscoped bind left no row"
    );
}

/// Drive the exact commit/notify body used by send_role_message, the app's
/// production offer, ticker admission and real claim loop; no fabricated event.
#[tokio::test]
async fn mcp_role_filters_run_only_matching_queues_without_claiming_mail() {
    use crate::commands::dashboard_launch::LaunchTeam;
    use crate::commands::role_inbox::{send_role_and_notify, RoleSend};
    use crate::mcp::routine_tools::{create, CallerSession};
    use serde_json::json;

    let temp = TempHome::new();
    let home = temp.codesense_home();
    let now = chrono::Utc::now();
    for (id, filter) in [
        ("reviewer-mail", Some(" role: reviewer ")),
        ("builder-mail", Some("role:builder")),
        ("other-mail", Some("role:other")),
        ("all-mail", None),
    ] {
        let mut event = json!({"kind":"role_pending"});
        if let Some(role) = filter {
            event["role"] = json!(role);
        }
        let result = create(&home, &alice(), Placement::Team {org:ORG.into(), team:TEAM.into()}, &CallerSession::default(), &super::model::Author::agent("test"),
            serde_json::from_value(json!({"id":id,"name":id,"agent":"default","mode":"fresh","task":"Inspect {{trigger.role}} message {{trigger.message_id}}","events":[event],"overlap":"queue","maxRuntime":"5m"})).unwrap(), now).unwrap();
        let wire = serde_json::to_value(result).unwrap();
        assert_eq!(wire["status"], "ok", "{wire}");
    }
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let clock = Arc::new(AtomicI64::new(finish::now_ms()));
    let app = App::new(&home, &clock, launcher.clone());
    let team = LaunchTeam {
        org: ORG.into(),
        team: TEAM.into(),
    };
    let path = temp.root().join("role-mail.sqlite3");
    let mut ids = Vec::new();
    for (index, (target, role)) in [("role:reviewer", "reviewer"), (" role:builder ", "builder")]
        .into_iter()
        .enumerate()
    {
        let outcome = send_role_and_notify(
            path.clone(),
            RoleSend {
                sender: "sender-session".into(),
                role: target.into(),
                text: format!("Work for {role}"),
                request_id: None,
                attach_file: None,
                on_click: None,
                content_type: None,
                reply_to: None,
                summary: None,
            },
            |role, message_id| {
                super::role_pending::offer_role_pending(
                    app.app.handle(),
                    &team,
                    role,
                    message_id,
                    "sender-session",
                );
            },
        )
        .await
        .unwrap();
        assert_eq!(outcome["status"], "sent", "{outcome}");
        ids.push(outcome["messageId"].as_i64().unwrap());
        assert_eq!(
            wait_for(&home, "all-mail", |r| r.len() == index + 1)
                .await
                .len(),
            index + 1
        );
        assert_eq!(
            wait_for(&home, &format!("{role}-mail"), |r| !r.is_empty())
                .await
                .len(),
            1
        );
    }
    assert!(runs_of(&home, "other-mail").is_empty());
    assert_eq!(runs_of(&home, "reviewer-mail").len(), 1);
    assert_eq!(runs_of(&home, "builder-mail").len(), 1);
    app.app.state::<super::tick::RoutineTicker>().tick();
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 3);
    for (index, role) in ["reviewer", "builder"].into_iter().enumerate() {
        let runs = runs_of(&home, &format!("{role}-mail"));
        assert_eq!(runs[0].source, "event");
        assert_eq!(runs[0].phase, "running");
        let payload = runs[0].trigger_payload.as_ref().unwrap();
        assert_eq!(payload["role"], role);
        assert_eq!(payload["message_id"], ids[index]);
        assert!(launcher.launched().iter().any(|l| l
            .request
            .prompt
            .ends_with(&format!("Inspect {role} message {}", ids[index]))));
    }
    for launched in launcher.launched() {
        launcher.exit(&launched.session_id).await;
    }
    // The wildcard's second message follows its first run through overlap queue.
    app.app.state::<super::tick::RoutineTicker>().tick();
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    assert_eq!(launcher.calls(), 4);
    launcher.exit(&launcher.last().session_id).await;
    let mail = crate::weft::WeftStore::open(path).unwrap();
    let pending: i64 = mail
        .conn()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM wv_role_delivery WHERE state='pending'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pending, 2, "routine notification never claims role mail");
}
