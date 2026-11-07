// SPDX-License-Identifier: MIT
use super::*;
use crate::agent_cli::orion::Orion;
use std::io::Read;

#[derive(Default)]
struct Outlet {
    told: Mutex<Vec<(String, String)>>,
    released: Mutex<Vec<String>>,
    uncertain: Mutex<Vec<String>>,
    states: Mutex<Vec<InboxWaiterState>>,
    lifecycle: Mutex<Vec<(String, String)>>,
    /// What `has_tools` answers; `Some(true)` unless a test sets it.
    tools: Mutex<Option<Option<bool>>>,
    /// Who hears of the session's notices.
    audience: Mutex<Option<String>>,
}

impl InboxOutlet for Outlet {
    fn fate(&self, session: &str, token: &str, fate: Fate) {
        match fate {
            Fate::Told => lock(&self.told).push((session.to_string(), token.to_string())),
            Fate::Released => lock(&self.released).push(token.to_string()),
            Fate::Uncertain => lock(&self.uncertain).push(token.to_string()),
        }
    }

    fn waiter(&self, state: InboxWaiterState) {
        lock(&self.states).push(state);
    }

    fn waiter_lifecycle(&self, _session: &str, event: &str, reason: &str) {
        lock(&self.lifecycle).push((event.into(), reason.into()));
    }

    fn audience(&self, _session: &str) -> Option<String> {
        lock(&self.audience).clone()
    }

    fn has_tools(&self, _session: &str) -> Option<bool> {
        lock(&self.tools).unwrap_or(Some(true))
    }
}

impl Outlet {
    fn told(&self) -> Vec<String> {
        lock(&self.told).iter().map(|(_, t)| t.clone()).collect()
    }
}

fn me() -> Owner {
    crate::activity::peer::inspect(std::process::id()).expect("this process can be inspected")
}

/// A waiter served on its own thread, and the helper's end of its socket.
fn waiter(
    inbox: &Arc<HostInbox>,
    outlet: &Arc<Outlet>,
    arm: WaitArm,
    check: bool,
    prompt: Option<&'static str>,
) -> (std::thread::JoinHandle<()>, UnixStream) {
    let (host_end, helper_end) = UnixStream::pair().unwrap();
    helper_end
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let (inbox, outlet) = (inbox.clone(), outlet.clone());
    let served = std::thread::spawn(move || {
        inbox.serve(&*outlet, "S1", arm, me(), check, prompt, host_end);
    });
    (served, helper_end)
}

fn until(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The next line the host sent the helper.
fn next(helper: &UnixStream) -> String {
    let line =
        codesense_host_proto::reverse_mcp::read_message(&mut &*helper, wire::MAX_LINE, |_| Ok(()))
            .unwrap();
    line.trim_end_matches('\n').to_string()
}

/// The helper says it printed the notice reserved under `token`.
fn printed(helper: &UnixStream, token: &str) {
    wire::send_lines(&mut &*helper, &wire::printed_line(token)).unwrap();
}

fn new() -> (Arc<HostInbox>, Arc<Outlet>) {
    (Arc::new(HostInbox::default()), Arc::new(Outlet::default()))
}

#[test]
fn a_held_notice_is_handed_to_the_waiter_and_told_once_it_is_printed() {
    let (inbox, outlet) = new();
    let (served, helper) = waiter(&inbox, &outlet, WaitArm::Replace, false, None);
    until(|| inbox.state("S1").waiting);
    assert_eq!(
        inbox.mail("S1", "You have 1 new message.".into(), "t1".into()),
        Repeat::Held
    );
    assert_eq!(next(&helper), "mail t1");
    assert_eq!(next(&helper), "You have 1 new message.");
    // Nothing is told before the helper says it printed the notice.
    assert!(outlet.told().is_empty());
    printed(&helper, "t1");
    served.join().unwrap();
    assert_eq!(outlet.told(), ["t1"]);
    // The woken agent reads armed through the turn its notice started.
    let state = inbox.state("S1");
    assert!(!state.waiting && state.grace_ms.is_some());
    // The same token pushed again (its report was lost): not handed over twice.
    assert_eq!(
        inbox.mail("S1", "You have 1 new message.".into(), "t1".into()),
        Repeat::Told
    );
}

#[test]
fn mail_pushed_before_the_waiter_arms_is_handed_over_at_once_and_a_repeat_push_is_ignored() {
    let (inbox, outlet) = new();
    inbox.mail("S1", "old".into(), "t1".into());
    inbox.mail("S1", "newer\nsecond line".into(), "t2".into());
    // The same token again changes nothing.
    assert_eq!(inbox.mail("S1", "newer".into(), "t2".into()), Repeat::Held);
    let (served, helper) = waiter(&inbox, &outlet, WaitArm::Replace, false, None);
    assert_eq!(next(&helper), "mail t2");
    // A notice travels as one line.
    assert_eq!(next(&helper), "newer second line");
    printed(&helper, "t2");
    served.join().unwrap();
    assert_eq!(outlet.told(), ["t2"]);
}

#[test]
fn a_notice_waits_for_the_agent_to_have_its_tools_again() {
    let (inbox, outlet) = new();
    *lock(&outlet.tools) = Some(Some(false));
    let (served, helper) = waiter(&inbox, &outlet, WaitArm::Replace, false, None);
    until(|| inbox.state("S1").waiting);
    inbox.mail("S1", "n".into(), "t1".into());
    // Told now, the agent could not read it: nothing is handed over yet.
    helper.set_read_timeout(Some(LIVENESS_POLL * 2)).unwrap();
    let mut byte = [0u8; 1];
    assert!((&helper).read(&mut byte).is_err());
    assert!(inbox.state("S1").waiting);
    *lock(&outlet.tools) = Some(Some(true));
    helper
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    assert_eq!(next(&helper), "mail t1");
    next(&helper);
    printed(&helper, "t1");
    served.join().unwrap();
    assert_eq!(outlet.told(), ["t1"]);
}

#[test]
fn a_notice_for_an_agent_with_no_codesense_stream_waits_only_a_grace() {
    let (inbox, outlet) = new();
    *lock(&outlet.tools) = Some(None);
    let (served, helper) = waiter(&inbox, &outlet, WaitArm::Replace, false, None);
    until(|| inbox.state("S1").waiting);
    let mailed = Instant::now();
    inbox.mail("S1", "n".into(), "t1".into());
    helper
        .set_read_timeout(Some(NO_STREAM_GRACE + Duration::from_secs(5)))
        .unwrap();
    assert_eq!(next(&helper), "mail t1");
    assert!(
        mailed.elapsed() >= NO_STREAM_GRACE,
        "it waited for a stream"
    );
    next(&helper);
    printed(&helper, "t1");
    served.join().unwrap();
}

#[test]
fn a_notice_the_helper_never_printed_stays_held_for_the_next_waiter() {
    let (inbox, outlet) = new();
    inbox.mail("S1", "n".into(), "t1".into());
    let (served, helper) = waiter(&inbox, &outlet, WaitArm::Replace, false, None);
    assert_eq!(next(&helper), "mail t1");
    drop(helper);
    served.join().unwrap();
    assert!(outlet.told().is_empty());
    let (served, helper) = waiter(&inbox, &outlet, WaitArm::Replace, false, None);
    assert_eq!(next(&helper), "mail t1");
    next(&helper);
    printed(&helper, "t1");
    served.join().unwrap();
    assert_eq!(outlet.told(), ["t1"]);
}

#[test]
fn a_newer_waiter_replaces_the_one_before_it_and_a_re_arm_defers() {
    let (inbox, outlet) = new();
    let (first, older) = waiter(&inbox, &outlet, WaitArm::Replace, false, None);
    until(|| inbox.state("S1").waiting);
    let (_second, newer) = waiter(&inbox, &outlet, WaitArm::Replace, false, None);
    assert_eq!(next(&older), "replaced");
    first.join().unwrap();
    // A tool call's re-arm while one waits: it ends, the waiting one keeps its place.
    let (third, re_arm) = waiter(&inbox, &outlet, WaitArm::IfAbsent, false, None);
    assert_eq!(next(&re_arm), "replaced");
    third.join().unwrap();
    assert!(inbox.state("S1").waiting);
    drop(newer);
}

#[test]
fn a_helper_that_goes_away_releases_its_place_and_a_forgotten_session_reads_unarmed() {
    let (inbox, outlet) = new();
    let (served, helper) = waiter(&inbox, &outlet, WaitArm::Replace, false, None);
    until(|| inbox.state("S1").waiting);
    drop(helper);
    served.join().unwrap();
    let state = inbox.state("S1");
    assert!(!state.waiting && state.grace_ms.is_some(), "{state:?}");
    // Arming and ending each reported a change.
    assert!(lock(&outlet.states).len() >= 2);
    inbox.forget("S1", &*outlet);
    assert_eq!(inbox.state("S1").grace_ms, None);
    assert!(inbox.sessions().is_empty());
    // Its desktop heard it is no longer armed.
    assert!(!lock(&outlet.states).last().unwrap().waiting);
    assert_eq!(lock(&outlet.states).last().unwrap().grace_ms, None);
}

#[test]
fn a_check_hands_over_what_is_held_and_otherwise_ends_at_once() {
    let (inbox, outlet) = new();
    inbox.mail("S1", "n".into(), "t1".into());
    let (served, helper) = waiter(&inbox, &outlet, WaitArm::IfAbsent, true, None);
    assert_eq!(next(&helper), "mail t1");
    next(&helper);
    printed(&helper, "t1");
    served.join().unwrap();
    assert_eq!(outlet.told(), ["t1"]);
    let looked = Instant::now();
    let (served, helper) = waiter(&inbox, &outlet, WaitArm::IfAbsent, true, None);
    assert_eq!(next(&helper), "replaced");
    served.join().unwrap();
    assert!(looked.elapsed() < Duration::from_secs(1), "it never waits");
    // A check takes no place and leaves no waiter behind.
    let state = inbox.state("S1");
    assert!(!state.waiting && state.grace_ms.is_none(), "{state:?}");
    // An agent with no CodeSense stream yet is handed it at once (a check cannot wait for
    // one); one known to lack its tools is not.
    *lock(&outlet.tools) = Some(Some(false));
    inbox.mail("S1", "n".into(), "t2".into());
    let (served, helper) = waiter(&inbox, &outlet, WaitArm::IfAbsent, true, None);
    assert_eq!(next(&helper), "replaced");
    served.join().unwrap();
    *lock(&outlet.tools) = Some(None);
    let (served, helper) = waiter(&inbox, &outlet, WaitArm::IfAbsent, true, None);
    assert_eq!(next(&helper), "mail t2");
    next(&helper);
    printed(&helper, "t2");
    served.join().unwrap();
}

#[test]
fn a_session_told_to_another_desktop_is_told_again() {
    let (inbox, outlet) = new();
    inbox.set_submitter("S1", Some(true));
    inbox.sweep(&*outlet);
    inbox.sweep(&*outlet);
    assert_eq!(lock(&outlet.states).len(), 1);
    *lock(&outlet.audience) = Some("other-mac".into());
    inbox.sweep(&*outlet);
    assert_eq!(lock(&outlet.states).len(), 2);
}

#[test]
fn a_sweep_reports_only_what_changed() {
    let (inbox, outlet) = new();
    inbox.set_submitter("S1", Some(false));
    inbox.sweep(&*outlet);
    inbox.sweep(&*outlet);
    assert_eq!(lock(&outlet.states).len(), 1);
    // Working, the host can still submit to it: it reads armed, not waiting.
    let busy = lock(&outlet.states)[0].clone();
    assert!(!busy.waiting && busy.grace_ms.is_some());
    inbox.set_submitter("S1", Some(true));
    inbox.sweep(&*outlet);
    assert!(lock(&outlet.states).last().unwrap().waiting);
    // Told again whatever it was told before (a desktop that named itself again).
    inbox.report_again(&*outlet, "S1");
    assert_eq!(lock(&outlet.states).len(), 3);
}

/// An engine that answers every turn it is asked to start `0`, and records the client ids
/// (`1`) and prompts (`2`) it was handed.
struct Port(SubmitOutcome, Vec<String>, Vec<String>);

impl Port {
    fn answering(outcome: SubmitOutcome) -> Self {
        Self(outcome, Vec::new(), Vec::new())
    }
}

impl Channel for Port {
    fn start_turn(&mut self, text: &str, client_id: &str) -> SubmitOutcome {
        self.1.push(client_id.to_string());
        self.2.push(text.to_string());
        self.0
    }
}

/// A channel lent to one submit.
struct Lent<'a>(&'a mut dyn Channel);

impl Channel for Lent<'_> {
    fn start_turn(&mut self, text: &str, client_id: &str) -> SubmitOutcome {
        self.0.start_turn(text, client_id)
    }
}

/// Submits S1's held notice over `channel`.
fn submit(inbox: &HostInbox, outlet: &Outlet, channel: &mut dyn Channel) {
    inbox.submit(outlet, "S1", &Orion, || Some(Box::new(Lent(channel))));
}

#[test]
fn a_host_submit_is_told_only_once_its_cli_accepted_it() {
    let (inbox, outlet) = new();
    inbox.mail("S1", "n".into(), "t1".into());
    let mut port = Port::answering(SubmitOutcome::Accepted);
    submit(&inbox, &outlet, &mut port);
    assert_eq!(port.1, ["t1"]);
    // Headed by its wake key, which the CLI's own prompt hook shows back.
    assert_eq!(port.2, [format!("{}\nn", wake_key("t1"))]);
    assert_eq!(outlet.told(), ["t1"]);
    // Nothing is left to hand over or submit again.
    submit(&inbox, &outlet, &mut port);
    assert_eq!(port.1, ["t1"]);
    assert_eq!(inbox.mail("S1", "n".into(), "t1".into()), Repeat::Told);
}

#[test]
fn a_refused_submit_is_released_and_stays_for_a_check_in_the_same_turn() {
    let (inbox, outlet) = new();
    inbox.mail("S1", "n".into(), "t1".into());
    let mut port = Port::answering(SubmitOutcome::RefusedNoEffect);
    submit(&inbox, &outlet, &mut port);
    assert_eq!(*lock(&outlet.released), ["t1"]);
    assert!(outlet.told().is_empty());
    // A working CLI's check hook hears it.
    let (served, helper) = waiter(&inbox, &outlet, WaitArm::IfAbsent, true, None);
    assert_eq!(next(&helper), "mail t1");
    next(&helper);
    printed(&helper, "t1");
    served.join().unwrap();
    assert_eq!(outlet.told(), ["t1"]);
}

#[test]
fn a_refused_notice_is_submitted_again_only_once_its_agent_was_seen_working_and_idle() {
    let (inbox, outlet) = new();
    inbox.set_submitter("S1", Some(true));
    inbox.mail("S1", "n".into(), "t1".into());
    let mut refused = Port::answering(SubmitOutcome::RefusedNoEffect);
    submit(&inbox, &outlet, &mut refused);
    // Still idle to its host: its engine would refuse it again, so it is not asked.
    inbox.set_submitter("S1", Some(true));
    submit(&inbox, &outlet, &mut refused);
    assert_eq!(refused.1, ["t1"]);
    assert!(inbox.holds("S1") && !inbox.awaits_submit("S1"));
    // Seen working, then idle again: it is.
    inbox.set_submitter("S1", Some(false));
    inbox.set_submitter("S1", Some(true));
    let mut port = Port::answering(SubmitOutcome::Accepted);
    submit(&inbox, &outlet, &mut port);
    assert_eq!(port.1, ["t1"]);
    assert_eq!(outlet.told(), ["t1"]);
}

#[test]
fn an_engine_that_cant_be_reached_takes_nothing() {
    let (inbox, outlet) = new();
    inbox.mail("S1", "n".into(), "t1".into());
    inbox.submit(&*outlet, "S1", &Orion, || None);
    assert!(lock(&outlet.released).is_empty() && outlet.told().is_empty());
    // It is held, for a check, and not asked again while its agent sits idle.
    assert!(inbox.holds("S1") && !inbox.awaits_submit("S1"));
}

#[test]
fn an_uncertain_submit_is_never_sent_again_until_the_prompt_shows_its_wake_key() {
    let (inbox, outlet) = new();
    inbox.mail("S1", "n".into(), "t1".into());
    let mut port = Port::answering(SubmitOutcome::Uncertain);
    submit(&inbox, &outlet, &mut port);
    assert_eq!(*lock(&outlet.uncertain), ["t1"]);
    // Pushed again (a re-offer): never submitted or handed over blind.
    assert_eq!(inbox.mail("S1", "n".into(), "t1".into()), Repeat::Uncertain);
    submit(&inbox, &outlet, &mut port);
    assert_eq!(port.1, ["t1"]);
    // A prompt without its wake key settles nothing (the bare token is not the key);
    // one with it is told.
    let (served, helper) = waiter(&inbox, &outlet, WaitArm::IfAbsent, true, Some("about t1"));
    assert_eq!(next(&helper), "replaced");
    served.join().unwrap();
    assert!(outlet.told().is_empty());
    let (served, helper) = waiter(
        &inbox,
        &outlet,
        WaitArm::IfAbsent,
        true,
        Some("[inbox t1]\nYou have 1 new message."),
    );
    assert_eq!(next(&helper), "replaced");
    served.join().unwrap();
    assert_eq!(outlet.told(), ["t1"]);
    assert_eq!(inbox.mail("S1", "n".into(), "t1".into()), Repeat::Told);
}

#[test]
fn a_notice_a_waiter_is_handing_over_is_not_submitted_too() {
    let (inbox, outlet) = new();
    let (served, helper) = waiter(&inbox, &outlet, WaitArm::Replace, false, None);
    until(|| inbox.state("S1").waiting);
    inbox.mail("S1", "n".into(), "t1".into());
    assert_eq!(next(&helper), "mail t1");
    let mut port = Port::answering(SubmitOutcome::Accepted);
    submit(&inbox, &outlet, &mut port);
    assert!(port.1.is_empty(), "the waiter has it");
    next(&helper);
    printed(&helper, "t1");
    served.join().unwrap();
    assert_eq!(outlet.told(), ["t1"]);
}

/// An engine whose first turn started sees a newer notice pushed meanwhile.
struct Busy<'a> {
    inbox: &'a HostInbox,
    outlet: &'a Outlet,
    handed: Vec<String>,
}

impl Channel for Busy<'_> {
    fn start_turn(&mut self, _text: &str, client_id: &str) -> SubmitOutcome {
        if self.handed.is_empty() {
            assert_eq!(
                self.inbox.mail("S1", "newer".into(), "t2".into()),
                Repeat::Held
            );
            // A second submit while this one runs hands nothing over.
            let mut second = Port::answering(SubmitOutcome::Accepted);
            submit(self.inbox, self.outlet, &mut second);
            assert!(second.1.is_empty());
        }
        self.handed.push(client_id.to_string());
        SubmitOutcome::Accepted
    }
}

#[test]
fn a_notice_pushed_while_another_is_submitted_waits_for_the_next_submit() {
    let (inbox, outlet) = new();
    inbox.mail("S1", "n".into(), "t1".into());
    let mut port = Busy {
        inbox: &inbox,
        outlet: &outlet,
        handed: vec![],
    };
    submit(&inbox, &outlet, &mut port);
    assert_eq!(port.handed, ["t1"]);
    assert!(inbox.holds("S1"));
    submit(&inbox, &outlet, &mut port);
    assert_eq!(port.handed, ["t1", "t2"]);
    assert_eq!(outlet.told(), ["t1", "t2"]);
    // A refused one is not submitted again at once.
    inbox.mail("S1", "n".into(), "t3".into());
    let mut refused = Port::answering(SubmitOutcome::RefusedNoEffect);
    submit(&inbox, &outlet, &mut refused);
    assert_eq!(refused.1, ["t3"]);
}

#[test]
fn a_session_its_host_tracks_works_on_what_it_was_handed_and_its_newer_notice_waits() {
    let (inbox, outlet) = new();
    // The host submits to it, and sees it idle.
    inbox.set_submitter("S1", Some(true));
    inbox.mail("S1", "n".into(), "t1".into());
    let mut port = Busy {
        inbox: &inbox,
        outlet: &outlet,
        handed: vec![],
    };
    submit(&inbox, &outlet, &mut port);
    // Only the first starts a turn: the agent now works on it, so the newer one stays held,
    // untaken, for its checks, until a sweep sees it idle again.
    assert_eq!(port.handed, ["t1"]);
    assert_eq!(outlet.told(), ["t1"]);
    assert!(inbox.holds("S1"));
    assert!(!inbox.state("S1").waiting);
}

#[test]
fn each_fate_is_reported_by_its_own_event() {
    for fate in [Fate::Told, Fate::Released, Fate::Uncertain] {
        assert_eq!(
            Fate::of(fate.event("S1", "t")),
            Some((PtySessionId::new("S1"), "t".to_string(), fate))
        );
    }
    let state = InboxWaiterState {
        session: PtySessionId::new("S1"),
        waiting: true,
        grace_ms: None,
    };
    assert_eq!(Fate::of(TerminalDomainEvent::InboxWaiter { state }), None);
}

#[test]
fn waiter_logs_start_and_exit_for_delivery_and_a_disconnected_helper() {
    let (inbox, outlet) = new();
    let (served, helper) = waiter(&inbox, &outlet, WaitArm::Replace, false, None);
    until(|| inbox.state("S1").waiting);
    drop(helper);
    served.join().unwrap();
    let events = lock(&outlet.lifecycle).clone();
    assert_eq!(events[0], ("start".into(), "armed".into()));
    assert_eq!(events[1].0, "exit");
    assert!(events[1].1.contains("timeout/killed"));
    let (served, helper) = waiter(&inbox, &outlet, WaitArm::Replace, false, None);
    until(|| inbox.state("S1").waiting);
    inbox.mail("S1", "Mail".into(), "logged".into());
    assert_eq!(next(&helper), "mail logged");
    next(&helper);
    printed(&helper, "logged");
    served.join().unwrap();
    assert_eq!(lock(&outlet.lifecycle).last().unwrap().1, "mail-delivered");
}

#[test]
fn versioned_local_hold_fences_queued_new_recovery_and_waiter_notices_until_lift() {
    let (inbox, outlet) = new();
    inbox.require_wake_holds();
    inbox.mail("S1", "waiting mail".into(), "queued".into());
    let mut port = Port::answering(SubmitOutcome::Accepted);
    submit(&inbox, &outlet, &mut port);
    assert!(port.2.is_empty(), "startup waits for the desktop decision");
    inbox.set_wake_holds(
        2,
        vec![WakeHold {
            session: "S1".into(),
            reason: Some("day cap reached".into()),
        }],
    );
    inbox.set_wake_holds(
        1,
        vec![WakeHold {
            session: "S1".into(),
            reason: None,
        }],
    );
    submit(&inbox, &outlet, &mut port);
    inbox.recover(&*outlet, "S1");
    assert!(inbox.take("S1", |_| true).is_none());
    assert!(port.2.is_empty());
    assert!(outlet.told().is_empty());
    assert!(lock(&inbox.held).contains_key("S1"), "mail remains held");
    inbox.set_wake_holds(
        3,
        vec![WakeHold {
            session: "S1".into(),
            reason: None,
        }],
    );
    submit(&inbox, &outlet, &mut port);
    assert_eq!(port.2.len(), 1);
    assert!(port.2[0].contains("waiting mail"));
    assert_eq!(outlet.told(), vec!["queued"]);
}

#[test]
fn recovery_can_recheck_the_host_wake_permit_without_deadlocking() {
    struct Recovery(Arc<HostInbox>);
    impl InboxOutlet for Recovery {
        fn fate(&self, _: &str, _: &str, _: Fate) {}
        fn waiter(&self, _: InboxWaiterState) {}
        fn audience(&self, _: &str) -> Option<String> {
            None
        }
        fn has_tools(&self, _: &str) -> Option<bool> {
            Some(true)
        }
        fn recover(&self, session: &str, _: &Notice<'_>) -> SubmitOutcome {
            assert!(self.0.wake_allowed(session));
            SubmitOutcome::Accepted
        }
    }
    let inbox = Arc::new(HostInbox::default());
    inbox.mail("S1", "pending".into(), "token".into());
    let (done, received) = std::sync::mpsc::channel();
    let worker = inbox.clone();
    std::thread::spawn(move || {
        worker.recover(&Recovery(worker.clone()), "S1");
        done.send(()).unwrap();
    });
    received
        .recv_timeout(Duration::from_secs(2))
        .expect("recovery must release its permit before the host rechecks");
    assert!(!inbox.holds("S1"));
}

#[test]
fn a_queued_receipt_is_pending_until_its_hook_including_when_the_hook_beats_the_reply() {
    struct Queued<'a> {
        inbox: &'a HostInbox,
        outlet: &'a Outlet,
        hook_first: bool,
    }
    impl Channel for Queued<'_> {
        fn start_turn(&mut self, _text: &str, token: &str) -> SubmitOutcome {
            assert_eq!(token, "reservation");
            if self.hook_first {
                self.inbox
                    .confirm_receipts(self.outlet, "s", &["provider-admission".into()]);
                assert!(
                    self.outlet.told().is_empty(),
                    "receipt association is not known yet"
                );
            }
            SubmitOutcome::Uncertain
        }
        fn receipt_id(&self) -> Option<&str> {
            Some("provider-admission")
        }
    }
    for hook_first in [false, true] {
        let inbox = HostInbox::default();
        let outlet = Outlet::default();
        inbox.mail("s", "notice".into(), "reservation".into());
        inbox.submit(&outlet, "s", &Orion, || {
            Some(Box::new(Queued {
                inbox: &inbox,
                outlet: &outlet,
                hook_first,
            }))
        });
        if !hook_first {
            assert!(outlet.told().is_empty(), "queued does not mean delivered");
            inbox.confirm_receipts(&outlet, "s", &["wrong-admission".into()]);
            assert!(outlet.told().is_empty());
            inbox.confirm_receipts(&outlet, "s", &["provider-admission".into()]);
        }
        assert_eq!(outlet.told(), ["reservation"]);
        inbox.confirm_receipts(&outlet, "s", &["provider-admission".into()]);
        assert_eq!(
            outlet.told(),
            ["reservation"],
            "one reservation is acknowledged once"
        );
    }
}
