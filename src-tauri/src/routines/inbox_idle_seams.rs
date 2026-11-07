// SPDX-License-Identifier: MIT
//! Seam tests for `inbox_idle` routines: the trigger's window, batching, waiting for a
//! run still going, once-per-mail marks and their single re-offer, reach, the restart-seeded window, and the claim loop rendering the sessions into the
//! run's task, all over the real coordinator and admission.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use crate::commands::agent_inbox::SessionUnread;
use crate::commands::dashboard_launch::{LaunchTeam, SessionOwner};
use crate::mcp::session_list::{SessionListRow, SessionState};

use super::admit::Outcome;
use super::finish;
use super::inbox_idle::{self, IdleMail, InboxIdleState, RECHECKS, REOFFER_MS, WINDOW_MS};
use super::model::{EventSpec, RoutineDef, RoutineKey, RoutineTrigger, Where};
use super::run_seams::{key, routine, save, store, ticker};
use super::runs::RoutineRun;
use super::test_support::{drain_claim_loop, stub_agent, StubLauncher, TempHome, ORG, TEAM};
use super::tick::RoutineTicker;

fn on_inbox_idle(id: &str) -> RoutineDef {
    let mut def = routine(id, "0 9 * * *");
    def.task = "Wake {{trigger.count}}: {{trigger.sessions}}".into();
    def.triggers = vec![RoutineTrigger::Event(EventSpec {
        kind: "inbox_idle".into(),
        from_agent: None,
        role: None,
        metadata: None,
    })];
    def
}

fn owner(principal: Option<&str>, team: Option<&str>) -> SessionOwner {
    SessionOwner {
        principal: principal.map(str::to_string),
        org: ORG.into(),
        team: team.map(str::to_string),
    }
}

/// A session Alice launched in the routines' team with mail through `newest_id`.
fn idle(handle: &str, newest_id: i64) -> IdleMail {
    IdleMail {
        handle: handle.into(),
        name: format!("tab {handle}"),
        unread: 1,
        oldest_unread_at: Some("2026-10-04T10:00:00.000Z".into()),
        newest_id,
        inbox_org: ORG.into(),
        inbox_team: TEAM.into(),
        owner: Some(owner(Some("1"), Some(TEAM))),
        agent: None,
        announced: None,
    }
}

fn runs_of(home: &Path, id: &str) -> Vec<RoutineRun> {
    store(home).runs(&key(id)).unwrap()
}

fn outcomes(offered: &inbox_idle::Offered) -> Vec<Outcome> {
    offered
        .admissions
        .iter()
        .map(|(_, a)| a.outcome.clone())
        .collect()
}

struct World {
    temp: TempHome,
    launcher: Arc<StubLauncher>,
    clock: Arc<AtomicI64>,
    ticker: RoutineTicker,
}

impl World {
    fn new(defs: Vec<RoutineDef>) -> Self {
        let temp = TempHome::new();
        let home = temp.codesense_home();
        for def in defs {
            save(&home, def);
        }
        let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
        let clock = Arc::new(AtomicI64::new(finish::now_ms()));
        let ticker = ticker(&home, &clock, launcher.clone());
        Self {
            temp,
            launcher,
            clock,
            ticker,
        }
    }

    fn home(&self) -> std::path::PathBuf {
        self.temp.codesense_home()
    }

    fn now(&self) -> i64 {
        self.clock.load(Ordering::SeqCst)
    }

    fn advance(&self, ms: i64) {
        self.clock.fetch_add(ms, Ordering::SeqCst);
    }

    fn offer(&self, sessions: &[IdleMail]) -> inbox_idle::Offered {
        self.ticker.offer_inbox_idle(sessions).unwrap()
    }

    /// Run every queued run to its end; returns the prompts launched.
    async fn finish_runs(&self) -> Vec<String> {
        let before = self.launcher.calls();
        drain_claim_loop(&self.home(), self.launcher.clone()).await;
        let launched = self.launcher.launched();
        for l in &launched[before..] {
            self.launcher.exit(&l.session_id).await;
        }
        launched[before..]
            .iter()
            .map(|l| l.request.prompt.clone())
            .collect()
    }
}

#[tokio::test]
async fn a_fire_names_whom_to_wake_and_the_same_mail_is_not_offered_twice() {
    let w = World::new(vec![
        on_inbox_idle("wake"),
        routine("scheduled-only", "0 9 * * *"),
    ]);
    assert!(w.ticker.wants_inbox_idle());

    let first = w.offer(&[idle("SW-1#a", 5)]);
    assert_eq!(outcomes(&first), vec![Outcome::Queued]);
    assert!(runs_of(&w.home(), "scheduled-only").is_empty());
    // It will be looked at again once the re-offer time has passed.
    assert_eq!(
        first.rechecks,
        vec![(w.now() + REOFFER_MS, vec!["SW-1#a".to_string()])]
    );
    let prompts = w.finish_runs().await;
    assert_eq!(prompts.len(), 1);
    assert!(
        prompts[0].ends_with(
            "Wake 1: [{\"handle\":\"SW-1#a\",\"name\":\"tab SW-1#a\",\"oldest_unread_at\":\"2026-10-04T10:00:00.000Z\",\"unread\":1}]"
        ),
        "{}",
        prompts[0]
    );

    // The same mail again (its turn ended, still unread): not offered again yet.
    w.advance(WINDOW_MS);
    assert!(w.offer(&[idle("SW-1#a", 5)]).admissions.is_empty());
    assert_eq!(runs_of(&w.home(), "wake").len(), 1);

    // Still unread after the re-offer time: offered once more, then never for it.
    w.advance(REOFFER_MS);
    assert_eq!(
        outcomes(&w.offer(&[idle("SW-1#a", 5)])),
        vec![Outcome::Queued]
    );
    w.finish_runs().await;
    w.advance(REOFFER_MS);
    assert!(w.offer(&[idle("SW-1#a", 5)]).admissions.is_empty());
    // Newer mail is news.
    assert_eq!(
        outcomes(&w.offer(&[idle("SW-1#a", 6)])),
        vec![Outcome::Queued]
    );
    assert_eq!(runs_of(&w.home(), "wake").len(), 3);
    w.finish_runs().await;
}

#[tokio::test]
async fn mail_inside_the_window_or_during_a_run_waits_writes_no_row_and_is_batched() {
    let w = World::new(vec![on_inbox_idle("wake")]);
    let fired_at = w.now();
    assert_eq!(
        outcomes(&w.offer(&[idle("SW-1#a", 1)])),
        vec![Outcome::Queued]
    );

    // Inside the window: deferred to its end, no row.
    w.advance(5_000);
    let held = w.offer(&[idle("SW-1#b", 2), idle("SW-1#c", 3)]);
    assert!(held.admissions.is_empty());
    assert_eq!(
        held.rechecks,
        vec![(
            fired_at + WINDOW_MS,
            vec!["SW-1#b".to_string(), "SW-1#c".to_string()]
        )]
    );

    // Past the window but the first run has not run yet: still no row, wait again.
    w.clock.store(fired_at + WINDOW_MS, Ordering::SeqCst);
    let busy = w.offer(&[idle("SW-1#b", 2), idle("SW-1#c", 3)]);
    assert!(busy.admissions.is_empty(), "{busy:?}");
    assert_eq!(busy.rechecks[0].0, w.now() + WINDOW_MS);
    assert_eq!(runs_of(&w.home(), "wake").len(), 1, "no coalesced rows");

    // Once it ended, one run for both.
    w.finish_runs().await;
    let batched = w.offer(&[idle("SW-1#b", 2), idle("SW-1#c", 3)]);
    assert_eq!(outcomes(&batched), vec![Outcome::Queued]);
    let prompts = w.finish_runs().await;
    assert!(prompts[0].contains("Wake 2: "), "{}", prompts[0]);
    assert!(
        prompts[0].contains("SW-1#b") && prompts[0].contains("SW-1#c"),
        "{}",
        prompts[0]
    );
}

#[tokio::test]
async fn a_routine_with_nowhere_to_run_writes_one_skipped_row_per_window() {
    let mut def = on_inbox_idle("wake");
    def.target = Where {
        machine: None,
        kind: "repo:acme/not-attached".into(),
        extra: Default::default(),
    };
    let w = World::new(vec![def]);
    let skipped = w.offer(&[idle("SW-1#a", 1)]);
    assert!(matches!(
        outcomes(&skipped)[..],
        [Outcome::Skipped { ref reason }] if reason == "target_unavailable"
    ));
    w.advance(5_000);
    assert!(w.offer(&[idle("SW-1#a", 1)]).admissions.is_empty());
    assert_eq!(runs_of(&w.home(), "wake").len(), 1);
    w.advance(WINDOW_MS);
    assert_eq!(w.offer(&[idle("SW-1#a", 1)]).admissions.len(), 1);
    assert_eq!(runs_of(&w.home(), "wake").len(), 2);
}

#[tokio::test]
async fn a_restart_keeps_the_window_from_the_run_log() {
    let w = World::new(vec![on_inbox_idle("wake")]);
    w.offer(&[idle("SW-1#a", 1)]);
    w.finish_runs().await;
    // A new app: fresh in-memory state over the same run log.
    let restarted = ticker(&w.home(), &w.clock, w.launcher.clone());
    w.advance(5_000);
    let held = restarted.offer_inbox_idle(&[idle("SW-1#b", 2)]).unwrap();
    assert!(held.admissions.is_empty(), "{held:?}");
    assert_eq!(runs_of(&w.home(), "wake").len(), 1);
}

#[tokio::test]
async fn a_paused_routine_an_unknown_launch_or_another_teams_session_fires_nothing() {
    let w = World::new(vec![on_inbox_idle("wake")]);
    let mut other_team = idle("SW-9#x", 3);
    other_team.owner = Some(owner(Some("1"), Some("frontend")));
    let mut by_hand = idle("term-tab-1", 4);
    by_hand.owner = None;
    assert!(w.offer(&[other_team, by_hand]).admissions.is_empty());
    assert!(runs_of(&w.home(), "wake").is_empty());

    super::service::set_enabled(
        &w.home(),
        &super::test_support::alice(),
        &key("wake"),
        false,
    )
    .unwrap();
    assert!(w.offer(&[idle("SW-1#a", 6)]).admissions.is_empty());
    assert!(runs_of(&w.home(), "wake").is_empty());
}

#[test]
fn reach_is_agent_dones_rule() {
    let team = key("wake");
    let with = |o: Option<SessionOwner>| IdleMail {
        owner: o,
        ..idle("h", 1)
    };
    assert!(inbox_idle::in_reach(
        &team,
        &with(Some(owner(Some("1"), Some(TEAM))))
    ));
    assert!(inbox_idle::in_reach(
        &team,
        &with(Some(owner(None, Some(TEAM))))
    ));
    assert!(!inbox_idle::in_reach(
        &team,
        &with(Some(owner(Some("1"), Some("frontend"))))
    ));
    assert!(!inbox_idle::in_reach(
        &team,
        &with(Some(owner(Some("1"), None)))
    ));
    assert!(!inbox_idle::in_reach(&team, &with(None)));

    let personal = RoutineKey {
        account: Some("1".into()),
        ..key("wake")
    };
    assert!(inbox_idle::in_reach(
        &personal,
        &with(Some(owner(Some("1"), Some("frontend"))))
    ));
    assert!(!inbox_idle::in_reach(
        &personal,
        &with(Some(owner(Some("2"), Some(TEAM))))
    ));
    assert!(!inbox_idle::in_reach(
        &personal,
        &with(Some(owner(None, Some(TEAM))))
    ));
}

#[test]
fn offers_collect_until_taken_and_schedule_one_check_at_a_time() {
    let mut state = InboxIdleState::default();
    assert!(state.note([("a", 0)]), "the first offer schedules a check");
    assert!(
        !state.note([("b", 1), ("a", 2)]),
        "one is already scheduled"
    );
    assert_eq!(
        state.take(),
        vec![("a".to_string(), 0), ("b".to_string(), 1)],
        "a fresh offer resets a recheck count"
    );
    assert!(state.take().is_empty());
    assert!(!state.note([]), "nothing offered, nothing to check");
    assert!(
        state.note([("c", 0)]),
        "after a take the next offer schedules again"
    );
}

#[tokio::test]
async fn vanished_sessions_are_forgotten() {
    let w = World::new(vec![on_inbox_idle("wake")]);
    w.offer(&[idle("SW-1#gone", 1)]);
    w.finish_runs().await;
    w.advance(WINDOW_MS);
    assert!(w.offer(&[idle("SW-1#gone", 1)]).admissions.is_empty());
    // Its handle left the session list: a session that comes back under it is new.
    w.ticker
        .prune_inbox_idle(&HashSet::from(["SW-1#other".to_string()]));
    assert_eq!(
        outcomes(&w.offer(&[idle("SW-1#gone", 1)])),
        vec![Outcome::Queued]
    );
    w.finish_runs().await;
}

fn row(handle: &str, state: SessionState, failed: bool) -> SessionListRow {
    SessionListRow {
        id: handle.to_string(),
        team: None,
        queue_role: None,
        role_join_error: None,
        chat_workspace_id: None,
        group_unread: 0,
        group_read_id: 0,
        handle: handle.into(),
        display_name: format!("tab {handle}"),
        state,
        launched_by: None,
        story: None,
        workspace: None,
        role: None,
        avatar: None,
        agent: None,
        machine_id: None,
        workspace_handle: None,
        machine: None,
        unread: Some(1),
        oldest_unread_at: None,
        mail_notice: None,
        notified_at: None,
        subagents: 0,
        inbox_waiter: Default::default(),
        progress: None,
        launch_telemetry: None,
        release_qualified: None,
        release_note: None,
        deadline_at_ms: None,
        expiring: false,
        session: None,
        failed,
    }
}

#[test]
fn only_clean_idle_sessions_with_mail_go_on_and_busy_ones_are_looked_at_again() {
    let team = LaunchTeam {
        org: ORG.into(),
        team: TEAM.into(),
    };
    let mail = |count, newest| SessionUnread {
        count,
        oldest_at_ms: Some(1),
        newest_id: Some(newest),
        team: team.clone(),
        announced: None,
    };
    let unread = HashMap::from([
        ("idle".to_string(), mail(2, 9)),
        ("busy".to_string(), mail(1, 3)),
        ("busy-long".to_string(), mail(1, 3)),
        ("crashed".to_string(), mail(1, 4)),
        ("exited".to_string(), mail(1, 4)),
        ("unoffered".to_string(), mail(1, 5)),
    ]);
    let rows = vec![
        row("idle", SessionState::Idle, false),
        row("busy", SessionState::Running, false),
        row("busy-long", SessionState::Running, false),
        row("crashed", SessionState::Idle, true),
        row("exited", SessionState::Exited, false),
        row("quiet", SessionState::Idle, false),
        row("unoffered", SessionState::Idle, false),
    ];
    let offered: Vec<(String, u8)> = [
        ("idle", 0),
        ("busy", 0),
        ("busy-long", RECHECKS),
        ("crashed", 0),
        ("exited", 0),
        ("quiet", 0),
    ]
    .into_iter()
    .map(|(h, n)| (h.to_string(), n))
    .collect();
    // The session list's agent (registry or remote agent-key tag) rides along:
    // a wake routine matches it against its own agent.
    let mut rows = rows;
    rows[0].agent = Some(format!("{ORG}/{TEAM}/barb"));
    let found = inbox_idle::idle_with_mail(&offered, rows, &unread, |h| {
        (h == "idle").then(|| owner(Some("1"), Some(TEAM)))
    });
    assert_eq!(found.idle.len(), 1, "{:?}", found.idle);
    let s = &found.idle[0];
    assert_eq!(
        (
            s.handle.as_str(),
            s.unread,
            s.newest_id,
            s.inbox_team.as_str()
        ),
        ("idle", 2, 9, TEAM)
    );
    assert_eq!(s.owner, Some(owner(Some("1"), Some(TEAM))));
    assert_eq!(
        s.agent.as_deref(),
        Some(format!("{ORG}/{TEAM}/barb").as_str())
    );
    assert_eq!(found.not_yet, vec![("busy".to_string(), 1)]);
}
