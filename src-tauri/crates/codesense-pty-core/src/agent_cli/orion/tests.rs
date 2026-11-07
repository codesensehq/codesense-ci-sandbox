// SPDX-License-Identifier: MIT
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::*;
use crate::activity::{ActivityOwner, Scope, State, Writer};
use crate::domain::InboxWaiterState;
use crate::inbox_host::{Fate, InboxOutlet};

/// Orion's hook input for `event`.
fn hook(event: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"hook_event_name": event, "session_id": "orion-1"}))
        .unwrap()
}

#[test]
fn prepare_adds_the_observer_and_check_hooks_ahead_of_the_prompt_and_it_runs_on_its_server() {
    let mut launch = Prepare::new(
        strings(&["--model", "m", "--", "hello"]),
        serde_json::json!({}),
        "/app/helper",
    );
    Orion.prepare(&mut launch).unwrap();
    let args = &launch.arguments;
    let dashdash = args.iter().position(|a| a == "--").unwrap();
    assert_eq!(&args[dashdash..], ["--", "hello"]);
    assert_eq!(&args[..2], ["--model", "m"]);
    let hooks: Vec<&str> = args[..dashdash]
        .windows(2)
        .filter(|pair| pair[0] == "--hook")
        .map(|pair| pair[1].as_str())
        .collect();
    for event in OBSERVER_EVENTS {
        assert!(
            hooks.contains(&format!("{event}='/app/helper' --activity-hook {ORION}").as_str()),
            "{event}"
        );
    }
    for Check { event, .. } in CHECKS {
        let check = format!("{event}={}", inbox_hook_command(ORION, event));
        assert!(hooks.contains(&check.as_str()), "{event}");
    }
    // Its settings are untouched: it takes its hooks as flags.
    assert_eq!(launch.settings, serde_json::json!({}));
    // It runs on its app server, which its TUI attaches to, at the endpoint the host gives.
    let channel = Orion.channel(&strings(&["--model", "m"])).unwrap();
    assert_eq!(channel.engine, ["serve", "--listen", CHANNEL_ENDPOINT]);
    assert_eq!(
        channel.attached,
        ["--model", "m", "--remote", CHANNEL_ENDPOINT]
    );
}

#[test]
fn its_hooks_report_its_main_turn_and_its_other_ports_answer_honestly() {
    let started = Orion.normalize(&hook("Boot"), false).unwrap();
    assert_eq!(
        (started.provider, started.event, started.scope),
        (ORION, Event::Started, Scope::Main)
    );
    assert_eq!(started.provider_session.as_deref(), Some("orion-1"));
    assert!(Orion.normalize(&hook("Unknown"), false).is_none());
    assert!(Orion.session_policy().observed_from_start);
    assert!(!Orion.session_policy().rebind_on_start);
    // No status hooks, no folder settings for them, no usage, no transcript.
    assert!(Orion.status_hooks().is_empty());
    assert!(Orion.descriptor().project_hooks.is_none());
    assert!(matches!(Orion.output().usage, Usage::None));
    assert!(Orion.output().transcript.is_none());
    assert!(!Orion.restore_tags().permission_mode);
    assert!(matches!(Orion.probe().activity, ActivityProbe::Version));
    assert!(Orion.descriptor().release.qualified("orion 1.4.2"));
    assert!(!Orion.descriptor().release.qualified("orion 2.0.0"));
}

#[test]
fn a_control_submit_reaches_it_through_its_native_channel() {
    let endpoint = tempfile::tempdir().unwrap();
    let server = serve(endpoint.path());
    let control = Orion.control();
    assert_eq!(control.exit_line, ":quit");
    assert!(!control.bracketed_paste && control.submit_starts_turn);
    assert_eq!(control.command_prefix, Some(':'));
    let mut channel = (control.channel.unwrap())(endpoint.path(), None, None).unwrap();
    let notice = Notice {
        text: "You have 1 new message.",
        token: "t-1",
    };
    server.answer(&[SubmitOutcome::RefusedNoEffect]);
    assert_eq!(
        Orion.submit_notice(&mut *channel, &notice),
        SubmitOutcome::RefusedNoEffect
    );
    assert_eq!(
        Orion.submit_notice(&mut *channel, &notice),
        SubmitOutcome::Accepted
    );
    assert_eq!(server.handed(), ["t-1", "t-1"]);
    // No server at an endpoint: no connection.
    assert!((control.channel.unwrap())(Path::new("/no/such/endpoint"), None, None).is_err());
}

/// The reports of a host's inbox.
#[derive(Default)]
struct Outlet {
    fates: Mutex<Vec<(String, Fate)>>,
    states: Mutex<Vec<InboxWaiterState>>,
}

impl InboxOutlet for Outlet {
    fn fate(&self, _session: &str, token: &str, fate: Fate) {
        self.fates.lock().unwrap().push((token.to_string(), fate));
    }
    fn waiter(&self, state: InboxWaiterState) {
        self.states.lock().unwrap().push(state);
    }
    fn audience(&self, _: &str) -> Option<String> {
        None
    }
    fn has_tools(&self, _: &str) -> Option<bool> {
        Some(true)
    }
}

/// A host running one committed Orion session, `S1`, idle at its prompt.
struct Host {
    owner: Arc<ActivityOwner>,
    outlet: Arc<Outlet>,
    writer: Writer,
    server: Option<Arc<Server>>,
    _endpoint: tempfile::TempDir,
    /// Each sweep runs at a later time than the last.
    sweeps: Mutex<u32>,
}

/// `reachable`: it runs on its app server, which its host reaches it through.
fn rig(reachable: bool) -> Host {
    let endpoint = tempfile::tempdir().unwrap();
    let socket = endpoint.path().join("engine-orion.sock");
    let server = reachable.then(|| serve(&socket));
    let outlet = Arc::new(Outlet::default());
    let owner = Arc::new(ActivityOwner::with_adapters(adapters));
    owner.set_inbox_outlet(outlet.clone());
    let writer = owner.prepare_native("S1", ORION, "unused").unwrap();
    owner.register("S1", Some(std::process::id())).unwrap();
    if reachable {
        owner.bind_engine_for_test(&writer, socket).unwrap();
    }
    let host = Host {
        owner,
        outlet,
        writer,
        server,
        _endpoint: endpoint,
        sweeps: Mutex::new(0),
    };
    host.hook("Boot");
    assert_eq!(host.owner.snapshot("S1").unwrap().state, State::Idle);
    host.sweep();
    host
}

impl Host {
    fn hook(&self, event: &str) {
        self.raw(hook(event));
    }

    /// Its subagent `id`'s `event`.
    fn subagent(&self, event: &str, id: &str) {
        self.raw(
            serde_json::to_vec(
                &serde_json::json!({"hook_event_name": event, "session_id": "orion-1", "agent_id": id}),
            )
            .unwrap(),
        );
    }

    fn raw(&self, input: Vec<u8>) {
        let report = Orion.normalize(&input, true).unwrap();
        self.owner
            .report_for_test(&self.writer, std::process::id(), report)
            .unwrap();
    }

    /// The host's periodic sweep, past its interval since the last one.
    fn sweep(&self) {
        let mut sweeps = self.sweeps.lock().unwrap();
        *sweeps += 1;
        self.owner
            .decay(Instant::now() + Duration::from_secs(6) * *sweeps);
    }

    fn mail(&self, token: &str) {
        self.owner
            .inbox_mail("S1", "You have 1 new message.".into(), token.into())
            .unwrap();
    }

    fn fates(&self) -> Vec<(String, Fate)> {
        self.outlet.fates.lock().unwrap().clone()
    }

    /// The app server answers the next notices `answers`.
    fn answer(&self, answers: &[SubmitOutcome]) {
        self.server.as_ref().unwrap().answer(answers);
    }

    fn handed(&self) -> Vec<String> {
        self.server.as_ref().unwrap().handed()
    }

    fn until(&self, mut done: impl FnMut(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out: {:?}", self.fates());
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn fate(token: &str, fate: Fate) -> (String, Fate) {
    (token.to_string(), fate)
}

fn told(token: &str) -> (String, Fate) {
    fate(token, Fate::Told)
}

#[test]
fn a_channel_without_native_readiness_is_not_contacted_at_start() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("engine-orion.sock");
    let server = serve(&socket);
    let owner = ActivityOwner::with_adapters(adapters);
    let writer = owner.prepare_native("fresh", ORION, "unused").unwrap();
    owner.register("fresh", Some(std::process::id())).unwrap();
    owner.bind_engine_for_test(&writer, socket).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        server.sessions().is_empty(),
        "delivery channels do not imply startup probing"
    );
    assert!(!owner.snapshot("fresh").unwrap().observed);
}

#[test]
fn the_host_submits_a_notice_to_an_idle_orion_and_tells_it_once_accepted() {
    let host = rig(true);
    // While the host can submit to it, it reads as waiting for mail.
    assert!(host.owner.inbox().state("S1").waiting);
    assert!(host.outlet.states.lock().unwrap().last().unwrap().waiting);
    host.mail("t1");
    host.until(|h| h.fates() == [told("t1")]);
    // One notice for the batch, handed once, on the session its hooks named.
    assert_eq!(host.handed(), ["t1"]);
    assert_eq!(
        host.server.as_ref().unwrap().sessions(),
        [Some("orion-1".to_string())]
    );
    assert!(!host.owner.inbox().holds("S1"));
    // Pushed again (its report was lost on the way): told again, never handed again.
    host.mail("t1");
    assert_eq!(host.fates(), [told("t1"), told("t1")]);
    host.sweep();
    assert_eq!(host.handed(), ["t1"]);
}

#[test]
fn a_notice_orion_refused_is_released_and_offered_again_once_it_was_seen_working_and_idle() {
    let host = rig(true);
    host.answer(&[SubmitOutcome::RefusedNoEffect]);
    host.mail("t1");
    host.until(|h| h.fates() == [fate("t1", Fate::Released)]);
    // Still held for it: a check hook in its turn hands it over. Idle as before, it is not
    // asked again (its engine would refuse it again).
    assert!(host.owner.inbox().holds("S1"));
    host.sweep();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(host.handed(), ["t1"]);
    // Seen working and idle again, it is.
    host.hook("Prompt");
    host.sweep();
    host.hook("TurnEnd");
    host.sweep();
    host.until(|h| h.fates().last() == Some(&told("t1")));
    assert_eq!(host.handed(), ["t1", "t1"]);
}

#[test]
fn a_newer_batch_replaces_a_held_notice_and_is_the_one_submitted() {
    let host = rig(true);
    host.answer(&[SubmitOutcome::RefusedNoEffect]);
    host.mail("t1");
    host.until(|h| h.fates() == [fate("t1", Fate::Released)]);
    // More mail arrives: the desktop pushes one notice for the whole batch, which takes
    // the held one's place.
    host.mail("t2");
    host.until(|h| h.fates().last() == Some(&told("t2")));
    host.sweep();
    assert_eq!(host.handed(), ["t1", "t2"]);
    assert!(!host.fates().contains(&told("t1")));
}

#[test]
fn an_uncertain_submit_is_held_and_never_sent_again_blind() {
    let host = rig(true);
    host.answer(&[SubmitOutcome::Uncertain]);
    host.mail("t1");
    host.until(|h| h.fates() == [fate("t1", Fate::Uncertain)]);
    // Pushed again, and swept while idle: reported uncertain again, never resent.
    host.mail("t1");
    host.sweep();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(host.handed(), ["t1"]);
    assert!(host
        .fates()
        .iter()
        .all(|(_, fate)| *fate == Fate::Uncertain));
}

#[test]
fn a_working_orion_still_counts_as_reachable_and_a_host_without_a_way_to_it_reads_no_waiter() {
    let host = rig(true);
    host.hook("Prompt");
    host.sweep();
    // Working, the host can still submit to it: armed, with a grace, not waiting.
    let busy = host.owner.inbox().state("S1");
    assert!(!busy.waiting && busy.grace_ms.is_some());
    host.hook("TurnEnd");
    host.sweep();
    assert!(host.owner.inbox().state("S1").waiting);

    // One its host reaches no engine of (its engine never came up, or it runs standalone)
    // is never submitted to: no waiter, and its mail waits, held, for a check hook or the
    // next turn.
    let unreachable = rig(false);
    let state = unreachable.owner.inbox().state("S1");
    assert!(!state.waiting && state.grace_ms.is_none());
    // With no engine, nothing is spawned to submit: the mail is held at once.
    unreachable.mail("t1");
    assert!(unreachable.fates().is_empty());
    assert!(unreachable.owner.inbox().holds("S1"));
}

#[test]
fn a_cli_that_never_lists_its_subagents_drops_one_unheard_from_for_hours() {
    let host = rig(true);
    host.hook("Prompt");
    host.subagent("Spawn", "a1");
    host.hook("TurnEnd");
    let quiet = Instant::now() + Duration::from_secs(6 * 60 * 60);
    host.owner.decay(quiet - Duration::from_secs(60));
    assert_eq!(host.owner.snapshot("S1").unwrap().subagents_open, 1);
    host.owner.decay(quiet + Duration::from_secs(1));
    assert_eq!(host.owner.snapshot("S1").unwrap().subagents_open, 0);
}

#[test]
fn its_subagents_are_counted_through_the_shared_port_and_leave_its_state_and_mail_alone() {
    let host = rig(true);
    host.hook("Prompt");
    host.subagent("Spawn", "a1");
    host.subagent("Spawn", "a2");
    host.hook("TurnEnd");
    let snapshot = host.owner.snapshot("S1").unwrap();
    assert_eq!((snapshot.state, snapshot.subagents_open), (State::Done, 2));
    // Its own turn ended: it is idle at its prompt for its mail, its subagents running.
    host.sweep();
    assert!(host.owner.inbox().state("S1").waiting);
    host.mail("t1");
    host.until(|host| host.fates() == [told("t1")]);
    host.subagent("Join", "a1");
    assert_eq!(host.owner.snapshot("S1").unwrap().subagents_open, 1);
    host.subagent("Join", "a2");
    let snapshot = host.owner.snapshot("S1").unwrap();
    assert_eq!((snapshot.state, snapshot.subagents_open), (State::Done, 0));
}

fn mutate_turn(host: &Host, op: &str, action: crate::activity::Action) {
    let snap = host.owner.snapshot("S1").unwrap();
    host.owner
        .mutate(crate::activity::Mutation {
            session: "S1".into(),
            identity: snap.identity.clone(),
            turn_generation: snap.turn_generation,
            operation_id: op.into(),
            action,
        })
        .unwrap();
}

fn assert_submitted_once(host: &Host) {
    host.sweep();
    host.mail("t-new");
    for _ in 0..5 {
        host.sweep();
        host.mail("t-new");
    }
    host.until(|h| h.fates().contains(&told("t-new")));
    assert_eq!(host.owner.snapshot("S1").unwrap().state, State::Done);
    assert!(host.owner.snapshot("S1").unwrap().asks.is_empty());
    assert_eq!(host.handed(), ["t-new"]);
    assert!(!host.owner.inbox().holds("S1"));
}

#[test]
fn needs_attention_finish_allows_one_mail_notice() {
    let host = rig(true);
    host.hook("Prompt");
    mutate_turn(
        &host,
        "done-1",
        crate::activity::Action::Done {
            outcome: crate::activity::Outcome::NeedsAttention,
        },
    );
    host.hook("TurnEnd");
    assert_submitted_once(&host);
}

#[test]
fn turn_end_clears_open_ask_and_allows_one_mail_notice() {
    let host = rig(true);
    host.hook("Prompt");
    mutate_turn(
        &host,
        "ask-1",
        crate::activity::Action::AskOpened { id: "q1".into() },
    );
    host.hook("TurnEnd");
    assert_submitted_once(&host);
}

#[test]
fn a_permission_blocked_submitter_has_no_grace() {
    let host = rig(true);
    host.hook("Prompt");
    mutate_turn(
        &host,
        "blocked",
        crate::activity::Action::Progress { blocked: true },
    );
    host.sweep();
    let state = host.owner.inbox().state("S1");
    assert!(!state.waiting && state.grace_ms.is_none());
    host.mail("held");
    host.sweep();
    assert!(host.handed().is_empty());
}
