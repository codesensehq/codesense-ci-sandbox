// SPDX-License-Identifier: MIT
//! The wake queue and timers over a fake PTY port, an injected
//! clock and a driven pty-core `ActivityOwner`; then the same engine on a real
//! in-process session server over its socket, delivering into a real PTY.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use codesense_pty_core::activity::{
    Action, ActivityOwner, Event, Mutation, Report, Scope, State, Writer,
};
use codesense_pty_core::agent_cli::AgentCli;
use codesense_pty_core::domain::PtySessionId;
use codesense_pty_core::session_wake::{
    DeliverRequest, DeliveryStatus, GapPolicy, Originator, TimerSchedule, TimerSpec,
    UndeliveredReason, WakeLimits, WakeOutcome, ENTER_GAP_MIN_MS, OUTCOME_LOG_CAP,
    SESSION_RATE_LIMIT,
};

use super::{Claim, Clock, InputStamp, Runner, SessionWake, WakePort};

const MIN: u64 = 60_000;
const WALL: u64 = 1_790_000_000_000;

struct FakeClock {
    mono: AtomicU64,
    wall: AtomicU64,
}

impl FakeClock {
    fn new() -> Self {
        Self {
            mono: AtomicU64::new(1_000),
            wall: AtomicU64::new(WALL),
        }
    }
    fn advance(&self, ms: u64) {
        self.mono.fetch_add(ms, Ordering::SeqCst);
        self.wall.fetch_add(ms, Ordering::SeqCst);
    }
    /// The machine sleeps: wall time passes, a non-sleep-counting monotonic
    /// clock does not.
    fn sleep_machine(&self, ms: u64) {
        self.wall.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn mono_ms(&self) -> u64 {
        self.mono.load(Ordering::SeqCst)
    }
    fn wall_ms(&self) -> u64 {
        self.wall.load(Ordering::SeqCst)
    }
    fn sleep_ms(&self, ms: u64) {
        self.advance(ms);
    }
}

/// Attempts run where they are started: the tests see every outcome at once.
struct Inline;

impl Runner for Inline {
    fn run(&self, job: Box<dyn FnOnce() + Send>) {
        job()
    }
}

/// A fake PTY host: what the queue reads comes from a real `ActivityOwner`
/// driven by the test; what it writes is recorded. With `auto_submit`, an
/// Enter makes the agent take the prompt (a new turn), as Claude does.
struct Fake {
    clock: Arc<FakeClock>,
    owner: ActivityOwner,
    writers: Mutex<HashMap<String, Writer>>,
    reports: AtomicU64,
    live: Mutex<HashSet<String>>,
    background: AtomicBool,
    /// The process probe cannot answer (timed out, no pid, older host).
    probe_fails: AtomicBool,
    /// The shell holds a stopped or background job (a Ctrl-Z'd agent).
    stopped_job: AtomicBool,
    elsewhere: AtomicBool,
    bracketed: Mutex<Option<bool>>,
    auto_submit: AtomicBool,
    /// The human types into the prompt the moment a wake claims the
    /// attachment (a race the claim-time re-check must catch).
    type_on_claim: AtomicBool,
    on_claim: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// The human types between the paste and its Enter.
    type_in_gap: AtomicBool,
    inputs: Mutex<HashMap<String, InputStamp>>,
    /// (session, bytes, mono time) of every write.
    writes: Mutex<Vec<(String, String, u64)>>,
    /// Fail the write with this 1-based index.
    fail_write: AtomicUsize,
    /// Copy this file to that one as the first write begins.
    snapshot_store: Mutex<Option<(PathBuf, PathBuf)>>,
    emitted: Mutex<Vec<WakeOutcome>>,
}

impl Fake {
    fn new(clock: Arc<FakeClock>) -> Self {
        Self {
            clock,
            owner: ActivityOwner::new(),
            writers: Mutex::default(),
            reports: AtomicU64::new(0),
            live: Mutex::default(),
            background: AtomicBool::new(false),
            probe_fails: AtomicBool::new(false),
            stopped_job: AtomicBool::new(false),
            elsewhere: AtomicBool::new(false),
            bracketed: Mutex::new(Some(true)),
            auto_submit: AtomicBool::new(true),
            type_on_claim: AtomicBool::new(false),
            on_claim: Mutex::default(),
            type_in_gap: AtomicBool::new(false),
            inputs: Mutex::default(),
            writes: Mutex::default(),
            fail_write: AtomicUsize::new(0),
            snapshot_store: Mutex::default(),
            emitted: Mutex::default(),
        }
    }

    fn hook(
        &self,
        session: &str,
        event: Event,
        turn: Option<&str>,
        scope: Scope,
    ) -> Result<(), &'static str> {
        let writers = self.writers.lock().unwrap();
        let writer = &writers[session];
        let provider = self.owner.snapshot(session).unwrap().provider;
        let n = self.reports.fetch_add(1, Ordering::SeqCst);
        self.owner
            .report_for_test(
                writer,
                std::process::id(),
                Report {
                    provider,
                    event,
                    scope,
                    report_id: format!("r{n}"),
                    provider_session: Some("provider-1".into()),
                    turn: turn.map(str::to_string),
                    tool: matches!(event, Event::Waiting | Event::ToolStarted)
                        .then(|| "Bash".to_string()),
                    tool_name: None,
                    reason: None,
                    agent: None,
                    subagents: None,
                },
            )
            .map(|_| ())
    }

    /// The human typed `draft` input into `session` now.
    fn typed(&self, session: &str, draft: bool) {
        let position = self
            .owner
            .snapshot(session)
            .map(|s| (s.identity.invocation_id, s.turn_generation));
        let mut inputs = self.inputs.lock().unwrap();
        let stamp = inputs.entry(session.into()).or_default();
        stamp.at_ms = self.clock.mono_ms();
        if draft {
            stamp.draft = position;
        }
    }
}

impl WakePort for Fake {
    fn session_live(&self, session: &PtySessionId) -> bool {
        self.live.lock().unwrap().contains(session.as_str())
    }
    fn activity(&self, session: &PtySessionId) -> Option<codesense_pty_core::activity::Snapshot> {
        self.owner.snapshot(session.as_str())
    }
    fn agents_in_foreground(&self, queries: &[(PtySessionId, AgentCli)]) -> Vec<Option<bool>> {
        if self.probe_fails.load(Ordering::SeqCst) {
            return vec![None; queries.len()];
        }
        vec![Some(!self.background.load(Ordering::SeqCst)); queries.len()]
    }
    fn shell_idle(&self, _: &PtySessionId) -> Option<bool> {
        if self.probe_fails.load(Ordering::SeqCst) {
            return None;
        }
        Some(!self.stopped_job.load(Ordering::SeqCst))
    }
    fn last_input(&self, session: &PtySessionId) -> Option<InputStamp> {
        self.inputs.lock().unwrap().get(session.as_str()).cloned()
    }
    fn bracketed_paste(&self, _: &PtySessionId) -> Option<bool> {
        *self.bracketed.lock().unwrap()
    }
    fn output_seq(&self, _: &PtySessionId) -> Option<u64> {
        Some(0)
    }
    fn claim_available(&self, _: &PtySessionId, _: &Originator) -> bool {
        !self.elsewhere.load(Ordering::SeqCst)
    }
    fn claim(&self, session: &PtySessionId, _: &Originator) -> Claim {
        if let Some(action) = self.on_claim.lock().unwrap().take() {
            action();
        }
        if self.type_on_claim.swap(false, Ordering::SeqCst) {
            self.typed(session.as_str(), true);
        }
        if self.elsewhere.load(Ordering::SeqCst) {
            Claim::Elsewhere
        } else {
            Claim::Held(Box::new(()))
        }
    }
    fn write(&self, session: &PtySessionId, bytes: &[u8]) -> Result<(), String> {
        if let Some((from, to)) = self.snapshot_store.lock().unwrap().take() {
            std::fs::copy(from, to).unwrap();
        }
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        {
            let mut writes = self.writes.lock().unwrap();
            if writes.len() + 1 == self.fail_write.load(Ordering::SeqCst) {
                return Err("pty write failed".into());
            }
            writes.push((session.as_str().into(), text.clone(), self.clock.mono_ms()));
        }
        if text.starts_with("\x1b[200~") && self.type_in_gap.swap(false, Ordering::SeqCst) {
            self.typed(session.as_str(), true);
        }
        if text == "\r" && self.auto_submit.load(Ordering::SeqCst) {
            let n = self.reports.load(Ordering::SeqCst);
            self.hook(
                session.as_str(),
                Event::Submitted,
                Some(&format!("wake-{n}")),
                Scope::Main,
            )
            .unwrap();
        }
        Ok(())
    }
    fn emit(&self, outcome: &WakeOutcome) {
        self.emitted.lock().unwrap().push(outcome.clone());
    }
}

pub(crate) struct Rig {
    fake: Arc<Fake>,
    clock: Arc<FakeClock>,
    pub(crate) wake: Arc<SessionWake>,
}

pub(crate) fn rig() -> Rig {
    rig_at(None, "boot-1", Arc::new(FakeClock::new()))
}

fn rig_at(store: Option<PathBuf>, boot: &str, clock: Arc<FakeClock>) -> Rig {
    let fake = Arc::new(Fake::new(clock.clone()));
    let wake = SessionWake::open(
        fake.clone(),
        clock.clone(),
        Arc::new(Inline),
        boot.into(),
        store,
    );
    Rig { fake, clock, wake }
}

impl Rig {
    /// A live session whose agent (`provider`) is launched and committed.
    fn agent(&self, session: &str, provider: AgentCli) {
        let writer = self
            .fake
            .owner
            .prepare_native(session, provider, "unused")
            .unwrap();
        self.fake
            .owner
            .register(session, Some(std::process::id()))
            .unwrap();
        self.fake
            .writers
            .lock()
            .unwrap()
            .insert(session.into(), writer);
        self.fake.live.lock().unwrap().insert(session.into());
    }

    /// A live Claude session observed idle at its prompt.
    pub(crate) fn idle(&self, session: &str) {
        self.agent(session, AgentCli::Claude);
        self.hook(session, Event::Started, None);
        assert_eq!(self.state(session), State::Idle);
    }

    fn hook(&self, session: &str, event: Event, turn: Option<&str>) {
        self.fake.hook(session, event, turn, Scope::Main).unwrap();
    }

    /// The agent finishes whatever turn it is in.
    fn finish_turn(&self, session: &str) {
        self.hook(session, Event::Stopped, None);
    }

    /// The human submits a prompt of their own and the agent answers it.
    fn human_turn(&self, session: &str, turn: &str) {
        self.hook(session, Event::Submitted, Some(turn));
        self.hook(session, Event::Stopped, Some(turn));
    }

    fn mutate(&self, session: &str, action: Action) {
        let snapshot = self.fake.owner.snapshot(session).unwrap();
        let n = self.fake.reports.fetch_add(1, Ordering::SeqCst);
        self.fake
            .owner
            .mutate(Mutation {
                session: session.into(),
                identity: snapshot.identity,
                turn_generation: snapshot.turn_generation,
                operation_id: format!("op{n}"),
                action,
            })
            .unwrap();
    }

    fn state(&self, session: &str) -> State {
        self.fake.owner.snapshot(session).unwrap().state
    }

    fn advance(&self, ms: u64) {
        self.clock.advance(ms);
    }

    fn deliver(&self, session: &str, key: &str) -> WakeOutcome {
        self.wake
            .deliver(request(session, key, "check the inbox"))
            .unwrap()
    }

    pub(crate) fn owner(&self) -> &ActivityOwner {
        &self.fake.owner
    }

    pub(crate) fn on_claim(&self, action: Box<dyn FnOnce() + Send>) {
        *self.fake.on_claim.lock().unwrap() = Some(action);
    }

    pub(crate) fn writes(&self) -> Vec<(String, String)> {
        self.fake
            .writes
            .lock()
            .unwrap()
            .iter()
            .map(|(s, t, _)| (s.clone(), t.clone()))
            .collect()
    }
}

fn request(session: &str, key: &str, text: &str) -> DeliverRequest {
    DeliverRequest {
        session: PtySessionId::new(session),
        text: text.into(),
        header: Some("[routine: triage | run 7]".into()),
        marker: "Triage (edited by Barb)".into(),
        max_wait_ms: 30 * MIN,
        idempotency_key: key.into(),
        originator: Originator {
            desktop: "desk-a".into(),
            routine_host: Some("desk-a".into()),
        },
        limits: None,
    }
}

fn undelivered(reason: UndeliveredReason) -> DeliveryStatus {
    DeliveryStatus::Undelivered { reason }
}

fn is_queued(outcome: &WakeOutcome) -> bool {
    matches!(outcome.status, DeliveryStatus::Queued { .. })
}

fn is_delivered(status: &DeliveryStatus) -> bool {
    matches!(status, DeliveryStatus::Delivered { .. })
}

fn status_of(rig: &Rig, key: &str) -> DeliveryStatus {
    rig.wake
        .outcomes(0)
        .outcomes
        .into_iter()
        .rev()
        .find(|outcome| outcome.idempotency_key == key)
        .map(|outcome| outcome.status)
        .unwrap_or(DeliveryStatus::Queued { deadline_ms: 0 })
}

const PASTE: &str = "\x1b[200~[routine: triage | run 7]\ncheck the inbox\x1b[201~";

// CX3: a channel-capable CLI may establish native readiness after launch. Waiting
// grants no authority to type: qualified observation and input readiness still gate it.
#[test]
fn cx3_starting_codex_waits_then_delivers_with_turn_start_proof() {
    let rig = rig();
    rig.agent("codex", AgentCli::Codex);
    assert!(is_queued(&rig.deliver("codex", "starting")));
    assert!(rig.writes().is_empty());
    *rig.fake.bracketed.lock().unwrap() = Some(false);
    rig.hook("codex", Event::Started, None);
    rig.wake.tick();
    assert!(rig.writes().is_empty(), "no typing until the TUI is ready");
    *rig.fake.bracketed.lock().unwrap() = Some(true);
    rig.advance(super::REQUEUE_COOLDOWN_MS);
    rig.wake.tick();
    assert!(is_delivered(&status_of(&rig, "starting")));
    assert_eq!(
        rig.writes(),
        [
            ("codex".into(), PASTE.into()),
            ("codex".into(), "\r".into())
        ]
    );
    assert_eq!(rig.fake.owner.snapshot("codex").unwrap().turn_generation, 1);
}

#[test]
fn cx3_qualified_codex_requires_a_turn_and_preserves_human_and_permission_guards() {
    for guard in ["no-submit", "draft", "permission", "background"] {
        let rig = rig();
        rig.agent("codex", AgentCli::Codex);
        rig.hook("codex", Event::Started, None);
        match guard {
            "no-submit" => rig.fake.auto_submit.store(false, Ordering::SeqCst),
            "draft" => rig.fake.typed("codex", true),
            "permission" => {
                rig.hook("codex", Event::Submitted, Some("t1"));
                rig.hook("codex", Event::Waiting, Some("t1"));
            }
            "background" => rig.fake.background.store(true, Ordering::SeqCst),
            _ => unreachable!(),
        }
        let outcome = rig.deliver("codex", guard);
        if guard == "no-submit" {
            assert!(matches!(outcome.status, DeliveryStatus::Unknown { .. }));
            assert_eq!(rig.writes().len(), 2);
            rig.advance(MIN);
            rig.wake.tick();
            assert_eq!(rig.writes().len(), 2, "no duplicate Enter");
        } else {
            assert!(is_queued(&outcome));
            assert!(rig.writes().is_empty());
        }
    }
}

// An idle session gets the header and text once — one bracketed
// paste, at least 200 ms, one Enter — confirmed by the agent taking it.
#[test]
fn idle_session_gets_one_paste_then_one_enter_and_the_submit_is_confirmed() {
    let rig = rig();
    rig.idle("s1");
    let outcome = rig.deliver("s1", "k1");
    assert!(is_delivered(&outcome.status), "{outcome:?}");
    assert_eq!(
        rig.writes(),
        [("s1".into(), PASTE.into()), ("s1".into(), "\r".into())]
    );
    let times: Vec<u64> = rig
        .fake
        .writes
        .lock()
        .unwrap()
        .iter()
        .map(|w| w.2)
        .collect();
    assert!(times[1] - times[0] >= ENTER_GAP_MIN_MS, "{times:?}");
    for _ in 0..5 {
        rig.advance(10_000);
        rig.wake.tick();
    }
    assert_eq!(rig.writes().len(), 2, "no nudges, no retries");
    let emitted = rig.fake.emitted.lock().unwrap().clone();
    assert_eq!(emitted.len(), 1);
    assert_eq!(emitted[0].marker, "Triage (edited by Barb)");
    assert_eq!(emitted[0].originator.desktop, "desk-a");
    assert_eq!(emitted[0].host_boot_id, "boot-1");
}

// An Enter the agent never shows taking is unknown — never re-sent.
#[test]
fn an_unconfirmed_submit_is_unknown_and_never_nudged() {
    let rig = rig();
    rig.idle("s1");
    rig.fake.auto_submit.store(false, Ordering::SeqCst);
    let outcome = rig.deliver("s1", "k1");
    assert!(
        matches!(outcome.status, DeliveryStatus::Unknown { .. }),
        "{outcome:?}"
    );
    for _ in 0..5 {
        rig.advance(MIN);
        rig.wake.tick();
    }
    assert_eq!(rig.writes().len(), 2);
}

#[test]
fn a_finished_turn_is_idle() {
    let rig = rig();
    rig.idle("s1");
    rig.human_turn("s1", "t1");
    assert_eq!(rig.state("s1"), State::Done);
    assert!(is_delivered(&rig.deliver("s1", "k1").status));
}

// One delivery per turn. The next waits for the agent to have taken the
// last one and gone idle again — never back-to-back on a stale Idle.
#[test]
fn one_delivery_per_turn() {
    let rig = rig();
    rig.idle("s1");
    assert!(is_delivered(&rig.deliver("s1", "k1").status));
    assert!(is_queued(&rig.deliver("s1", "k2")));
    assert!(is_queued(&rig.deliver("s1", "k3")));
    for _ in 0..5 {
        rig.advance(10_000);
        rig.wake.tick();
    }
    assert_eq!(rig.writes().len(), 2, "the agent is still on k1's turn");
    rig.finish_turn("s1");
    rig.wake.tick();
    assert!(is_delivered(&status_of(&rig, "k2")));
    assert_eq!(rig.writes().len(), 4);
    rig.wake.tick();
    assert_eq!(rig.writes().len(), 4, "k3 waits for k2's turn to end");
    rig.finish_turn("s1");
    rig.wake.tick();
    assert!(is_delivered(&status_of(&rig, "k3")));
}

// After an unconfirmed write the snapshot still reads Idle; the session
// is held until a new turn, so nothing more lands on it.
#[test]
fn a_stale_idle_after_an_unconfirmed_write_holds_the_session() {
    let rig = rig();
    rig.idle("s1");
    rig.fake.auto_submit.store(false, Ordering::SeqCst);
    rig.deliver("s1", "k1");
    assert!(is_queued(&rig.deliver("s1", "k2")));
    for _ in 0..5 {
        rig.advance(10_000);
        rig.wake.tick();
    }
    assert_eq!(rig.state("s1"), State::Idle);
    assert_eq!(rig.writes().len(), 2, "never back-to-back on a stale Idle");
    rig.fake.auto_submit.store(true, Ordering::SeqCst);
    rig.human_turn("s1", "t1");
    rig.wake.tick();
    assert!(is_delivered(&status_of(&rig, "k2")));
}

// Busy waits, then delivers at the next idle point.
#[test]
fn busy_session_waits_then_delivers_at_idle() {
    let rig = rig();
    rig.idle("s1");
    rig.hook("s1", Event::Submitted, Some("t1"));
    assert_eq!(rig.state("s1"), State::Thinking);
    assert!(is_queued(&rig.deliver("s1", "k1")));
    rig.hook("s1", Event::ToolStarted, Some("t1"));
    for _ in 0..3 {
        rig.advance(MIN);
        rig.wake.tick();
    }
    assert!(rig.writes().is_empty(), "never typed while busy");
    rig.hook("s1", Event::Stopped, Some("t1"));
    rig.wake.tick();
    assert!(is_delivered(&status_of(&rig, "k1")));
    assert_eq!(rig.writes().len(), 2);
}

// Past its wait it is undelivered(busy_timeout), never written late.
#[test]
fn busy_past_max_wait_is_undelivered_busy_timeout() {
    let rig = rig();
    rig.idle("s1");
    rig.hook("s1", Event::Submitted, Some("t1"));
    assert!(is_queued(&rig.deliver("s1", "k1")));
    rig.advance(30 * MIN - 1);
    rig.wake.tick();
    assert!(is_queued(
        &rig.wake.deliver(request("s1", "k1", "x")).unwrap()
    ));
    rig.advance(1);
    rig.wake.tick();
    assert_eq!(
        status_of(&rig, "k1"),
        undelivered(UndeliveredReason::BusyTimeout)
    );
    rig.hook("s1", Event::Stopped, Some("t1"));
    rig.wake.tick();
    assert!(rig.writes().is_empty());
}

// Shells are immediately unobservable; fresh agents get bounded
// startup grace. Unknown evidence never authorizes delivery.
#[test]
fn unobserved_sessions_have_bounded_startup_grace_without_authorizing_delivery() {
    let rig = rig();
    rig.fake.live.lock().unwrap().insert("shell".into());
    assert_eq!(
        rig.deliver("shell", "k-shell").status,
        undelivered(UndeliveredReason::Unobservable)
    );
    rig.agent("codex", AgentCli::Codex);
    assert!(rig
        .fake
        .hook("codex", Event::Started, None, Scope::Unknown)
        .is_err());
    assert!(is_queued(&rig.deliver("codex", "k-codex")));
    assert!(rig.writes().is_empty());
    rig.agent("fresh", AgentCli::Claude);
    assert!(is_queued(&rig.deliver("fresh", "k-fresh")));
    rig.agent("never", AgentCli::Claude);
    assert!(is_queued(&rig.deliver("never", "k-never")));
    rig.advance(60_000);
    rig.hook("fresh", Event::Started, None);
    rig.wake.tick();
    assert!(is_delivered(&status_of(&rig, "k-fresh")));
    rig.advance(60_000);
    rig.wake.tick();
    assert_eq!(
        status_of(&rig, "k-never"),
        undelivered(UndeliveredReason::Unobservable)
    );
    assert_eq!(
        status_of(&rig, "k-codex"),
        undelivered(UndeliveredReason::Unobservable)
    );
    assert_eq!(rig.writes().len(), 2, "only the observed one");
}

// Needs-you from each of its three sources is never typed into.
#[test]
fn needs_you_from_each_source_gets_zero_writes() {
    type Source = fn(&Rig, &str);
    let sources: [(&str, Source); 3] = [
        ("permission hook", |rig, s| {
            rig.hook(s, Event::Submitted, Some("t1"));
            rig.hook(s, Event::Waiting, Some("t1"));
        }),
        ("ask_user", |rig, s| {
            rig.mutate(s, Action::AskOpened { id: "ask-1".into() })
        }),
        ("blocked progress", |rig, s| {
            rig.mutate(s, Action::Progress { blocked: true })
        }),
    ];
    for (name, open) in sources {
        let rig = rig();
        rig.idle("s1");
        open(&rig, "s1");
        assert_eq!(rig.state("s1"), State::NeedsPermission, "{name}");
        assert!(is_queued(&rig.deliver("s1", "k1")), "{name}");
        for _ in 0..30 {
            rig.advance(MIN);
            rig.wake.tick();
        }
        assert!(rig.writes().is_empty(), "{name} was typed into");
        assert_eq!(
            status_of(&rig, "k1"),
            undelivered(UndeliveredReason::BusyTimeout),
            "{name}"
        );
    }
}

// A failed agent, a gone session, and a respawned tab that reuses
// the session id (a new incarnation) all end the delivery; nothing written.
#[test]
fn failed_ended_and_respawned_sessions_end_the_delivery() {
    let rig = rig();
    rig.idle("s1");
    rig.hook("s1", Event::Submitted, Some("t1"));
    rig.hook("s1", Event::Failed, Some("t1"));
    assert_eq!(
        rig.deliver("s1", "k1").status,
        undelivered(UndeliveredReason::AgentFailed)
    );
    rig.idle("s2");
    rig.hook("s2", Event::Submitted, Some("t1"));
    assert!(is_queued(&rig.deliver("s2", "k2")));
    rig.fake.live.lock().unwrap().remove("s2");
    rig.wake.tick();
    assert_eq!(
        status_of(&rig, "k2"),
        undelivered(UndeliveredReason::SessionEnded)
    );
    rig.idle("s3");
    rig.hook("s3", Event::Submitted, Some("t1"));
    assert!(is_queued(&rig.deliver("s3", "k3")));
    let old = rig.fake.owner.snapshot("s3").unwrap().identity;
    rig.fake.owner.remove("s3", &old.session_incarnation);
    rig.idle("s3");
    rig.wake.tick();
    assert_eq!(
        status_of(&rig, "k3"),
        undelivered(UndeliveredReason::SessionEnded)
    );
    assert!(rig.writes().is_empty());
}

// Any viewer input within 3 s keeps it queued.
#[test]
fn viewer_input_within_three_seconds_keeps_it_queued() {
    let rig = rig();
    rig.idle("s1");
    rig.advance(1_000);
    rig.fake.typed("s1", false);
    assert!(is_queued(&rig.deliver("s1", "k1")));
    rig.advance(2_999);
    rig.wake.tick();
    assert!(rig.writes().is_empty());
    rig.advance(1);
    rig.wake.tick();
    assert_eq!(rig.writes().len(), 2);
}

// A non-empty prompt (a human draft or an open `/` menu) is never
// pasted onto — the local case, where the originating desktop holds the
// writable attachment itself. Only the human's own submit clears it.
#[test]
fn a_human_draft_is_never_pasted_onto_even_by_the_attachment_holder() {
    let rig = rig();
    rig.idle("s1");
    rig.fake.typed("s1", true);
    assert!(is_queued(&rig.deliver("s1", "k1")));
    for _ in 0..10 {
        rig.advance(MIN);
        rig.wake.tick();
    }
    assert!(rig.writes().is_empty(), "pasted onto a draft");
    rig.human_turn("s1", "t1");
    rig.wake.tick();
    assert!(is_delivered(&status_of(&rig, "k1")));
}

// The verdict is re-run while the attachment is held, so input that
// lands between the check and the claim stops the write.
#[test]
fn input_racing_the_claim_stops_the_write() {
    let rig = rig();
    rig.idle("s1");
    rig.fake.type_on_claim.store(true, Ordering::SeqCst);
    assert!(is_queued(&rig.deliver("s1", "k1")));
    assert!(rig.writes().is_empty());
    rig.advance(10_000);
    rig.wake.tick();
    assert!(
        rig.writes().is_empty(),
        "the draft typed at claim time holds it"
    );
}

// A writable attachment held elsewhere keeps it queued; never stolen.
#[test]
fn writable_attachment_held_elsewhere_keeps_it_queued() {
    let rig = rig();
    rig.idle("s1");
    rig.fake.elsewhere.store(true, Ordering::SeqCst);
    assert!(is_queued(&rig.deliver("s1", "k1")));
    for _ in 0..5 {
        rig.advance(10_000);
        rig.wake.tick();
    }
    assert!(rig.writes().is_empty());
    rig.fake.elsewhere.store(false, Ordering::SeqCst);
    rig.wake.tick();
    assert_eq!(rig.writes().len(), 2);
}

// Idle without the agent CLI in the foreground is not idle; neither is a
// TUI with bracketed paste off.
#[test]
fn no_foreground_agent_or_no_bracketed_paste_waits() {
    let rig = rig();
    rig.idle("s1");
    rig.fake.background.store(true, Ordering::SeqCst);
    assert!(is_queued(&rig.deliver("s1", "k1")));
    rig.wake.tick();
    rig.fake.background.store(false, Ordering::SeqCst);
    *rig.fake.bracketed.lock().unwrap() = Some(false);
    rig.wake.tick();
    assert!(rig.writes().is_empty());
    *rig.fake.bracketed.lock().unwrap() = Some(true);
    rig.advance(2_000);
    rig.wake.tick();
    assert_eq!(rig.writes().len(), 2);
}

// An agent that quit back to its shell (idle, its CLI not
// in the foreground) is `session_ended` once the quit grace passes, never
// waited out to busy_timeout; the desktop then resolves the wake again.
#[test]
fn an_agent_that_quit_to_its_shell_is_session_ended_fast() {
    let rig = rig();
    rig.idle("s1");
    rig.fake.background.store(true, Ordering::SeqCst);
    assert!(is_queued(&rig.deliver("s1", "k1")));
    rig.wake.tick();
    assert!(rig.writes().is_empty());
    rig.advance(super::AGENT_QUIT_GRACE_MS);
    rig.wake.tick();
    let outcome = rig.deliver("s1", "k1");
    assert_eq!(outcome.status, undelivered(UndeliveredReason::SessionEnded));
    assert!(rig.writes().is_empty(), "never typed into the bare shell");
}

// A probe that cannot answer is never an agent that quit:
// the wake keeps waiting, it is never `session_ended`.
#[test]
fn a_failing_probe_is_never_session_ended() {
    let rig = rig();
    rig.idle("s1");
    rig.fake.probe_fails.store(true, Ordering::SeqCst);
    assert!(is_queued(&rig.deliver("s1", "k1")));
    for _ in 0..3 {
        rig.advance(super::AGENT_QUIT_GRACE_MS);
        rig.wake.tick();
    }
    assert!(is_queued(&rig.deliver("s1", "k1")), "still waiting");
    assert!(rig.writes().is_empty(), "and nothing typed");
}

// A stopped agent (Ctrl-Z) is still the agent, never a
// quit: the wake waits for it to come back to the foreground.
#[test]
fn a_stopped_agent_job_is_never_session_ended() {
    let rig = rig();
    rig.idle("s1");
    rig.fake.background.store(true, Ordering::SeqCst);
    rig.fake.stopped_job.store(true, Ordering::SeqCst);
    assert!(is_queued(&rig.deliver("s1", "k1")));
    for _ in 0..3 {
        rig.advance(super::AGENT_QUIT_GRACE_MS);
        rig.wake.tick();
    }
    assert!(is_queued(&rig.deliver("s1", "k1")));
    rig.fake.background.store(false, Ordering::SeqCst);
    rig.fake.stopped_job.store(false, Ordering::SeqCst);
    rig.wake.tick();
    assert_eq!(rig.writes().len(), 2, "typed once it is back");
}

// An agent that is only briefly not foreground (it came back) is delivered.
#[test]
fn an_agent_back_in_the_foreground_within_the_grace_is_delivered() {
    let rig = rig();
    rig.idle("s1");
    rig.fake.background.store(true, Ordering::SeqCst);
    assert!(is_queued(&rig.deliver("s1", "k1")));
    rig.advance(1_000);
    rig.wake.tick();
    rig.fake.background.store(false, Ordering::SeqCst);
    rig.advance(5_000);
    rig.wake.tick();
    assert_eq!(rig.writes().len(), 2);
}

// A blocked session never holds up another, and blocked ticks do not
// rewrite the store.
#[test]
fn a_blocked_session_does_not_stall_others_or_rewrite_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig_at(
        Some(dir.path().join("wake")),
        "boot-1",
        Arc::new(FakeClock::new()),
    );
    rig.idle("a");
    rig.idle("b");
    rig.fake.typed("a", true);
    assert!(is_queued(&rig.deliver("a", "ka")));
    assert!(is_delivered(&rig.deliver("b", "kb").status));
    let meta = dir.path().join("wake").join("meta.json");
    let before = std::fs::read(&meta).unwrap();
    let modified = std::fs::metadata(&meta).unwrap().modified().unwrap();
    for _ in 0..5 {
        rig.advance(10_000);
        rig.wake.tick();
    }
    assert_eq!(std::fs::read(&meta).unwrap(), before);
    assert_eq!(
        std::fs::metadata(&meta).unwrap().modified().unwrap(),
        modified
    );
    assert_eq!(rig.writes().iter().filter(|w| w.0 == "a").count(), 0);
}

// If the in-flight marker cannot be stored, nothing is written.
#[test]
fn nothing_is_written_when_the_in_flight_marker_cannot_be_stored() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig_at(
        Some(dir.path().join("wake")),
        "boot-1",
        Arc::new(FakeClock::new()),
    );
    rig.idle("s1");
    rig.hook("s1", Event::Submitted, Some("t1"));
    assert!(is_queued(&rig.deliver("s1", "k1")));
    rig.wake.fail_store(true);
    rig.hook("s1", Event::Stopped, Some("t1"));
    rig.wake.tick();
    assert!(rig.writes().is_empty());
    // A delivery that cannot be stored is refused whole, never queued.
    assert!(rig.wake.deliver(request("s1", "k2", "x")).is_err());
    rig.wake.fail_store(false);
    rig.wake.tick();
    assert!(is_delivered(&status_of(&rig, "k1")));
    rig.finish_turn("s1");
    rig.wake.tick();
    assert_eq!(rig.writes().len(), 2, "k2 was never queued");
}

// R5: a repeated key is answered from the store and writes
// nothing; keys are scoped by originator and session.
#[test]
fn duplicate_idempotency_key_returns_stored_outcome_without_writing() {
    let rig = rig();
    rig.idle("s1");
    let first = rig.deliver("s1", "k1");
    let again = rig
        .wake
        .deliver(request("s1", "k1", "different text"))
        .unwrap();
    assert_eq!(again, first);
    assert_eq!(rig.writes().len(), 2);
    let queued = rig.deliver("s1", "k2");
    assert!(is_queued(&queued));
    assert_eq!(rig.deliver("s1", "k2"), queued);
    // Another desktop's k1 is its own delivery.
    let mut other = request("s1", "k1", "x");
    other.originator.desktop = "desk-b".into();
    let theirs = rig.wake.deliver(other).unwrap();
    assert_ne!(theirs.delivery_id, first.delivery_id);
    rig.finish_turn("s1");
    rig.wake.tick();
    rig.deliver("s1", "k2");
    assert_eq!(rig.writes().len(), 4);
}

// An evicted key answers unknown and is never written again.
#[test]
fn evicted_key_answers_unknown_and_never_writes() {
    let rig = rig();
    rig.idle("s1");
    let queued_at = rig.clock.mono_ms();
    rig.deliver("s1", "k1");
    let retained_until = queued_at + 30 * MIN + 24 * 60 * MIN;
    rig.advance(retained_until - 1 - rig.clock.mono_ms());
    rig.wake.tick();
    assert!(is_delivered(&rig.deliver("s1", "k1").status));
    rig.advance(1);
    rig.wake.tick();
    let answer = rig.deliver("s1", "k1");
    assert!(
        matches!(answer.status, DeliveryStatus::Unknown { .. }),
        "{answer:?}"
    );
    rig.wake.tick();
    assert_eq!(rig.writes().len(), 2);
}

// 200 one-minute timers churning for three hours never push a client's
// key out of its retention: re-asked, it gets its stored outcome, no write.
#[test]
fn timer_churn_never_evicts_a_client_key() {
    let rig = rig();
    rig.idle("client");
    let first = rig.deliver("client", "kept");
    assert!(is_delivered(&first.status));
    for s in 0..20 {
        let session = format!("busy{s}");
        rig.idle(&session);
        rig.hook(&session, Event::Submitted, Some("t1"));
        for _ in 0..10 {
            let mut spec = timer(&session, MIN, Some(MIN));
            spec.max_wait_ms = 5 * MIN;
            rig.wake.set_timer(spec).unwrap();
        }
    }
    for _ in 0..180 {
        rig.advance(MIN);
        rig.wake.tick();
    }
    assert!(rig.wake.outcomes(0).overflow, "the churn filled the log");
    let writes = rig.writes().len();
    assert_eq!(rig.deliver("client", "kept"), first);
    assert_eq!(rig.writes().len(), writes);
}

// R5: a write that fails after the paste is unknown, and never re-sent.
#[test]
fn a_failed_submit_is_unknown_and_never_retried() {
    let rig = rig();
    rig.idle("s1");
    rig.fake.fail_write.store(2, Ordering::SeqCst);
    let outcome = rig.deliver("s1", "k1");
    assert!(
        matches!(outcome.status, DeliveryStatus::Unknown { .. }),
        "{outcome:?}"
    );
    rig.fake.fail_write.store(0, Ordering::SeqCst);
    rig.wake.tick();
    rig.deliver("s1", "k1");
    assert_eq!(rig.writes().len(), 1, "only the paste ever went out");
}

// C0/C1 controls (ESC first) are stripped, so the paste cannot be
// closed early and the text cannot steer the terminal; 8 KiB cap.
#[test]
fn escape_and_controls_are_stripped_from_the_delivered_text() {
    let rig = rig();
    rig.idle("s1");
    rig.wake
        .deliver(request(
            "s1",
            "k1",
            "a\x1b[201~\x1b]52;c;evil\x07\rb\u{9b}2Jc\n\td",
        ))
        .unwrap();
    let paste = &rig.writes()[0].1;
    assert_eq!(
        paste,
        "\x1b[200~[routine: triage | run 7]\na[201~]52;c;evilb2Jc\n\td\x1b[201~"
    );
    assert_eq!(paste.matches('\x1b').count(), 2);
    assert!(rig
        .wake
        .deliver(request("s1", "k2", &"x".repeat(8 * 1024)))
        .is_err());
}

// A queued delivery can be cancelled (archived); a final one is
// answered as it stands.
#[test]
fn a_queued_delivery_is_cancelled_as_archived() {
    let rig = rig();
    rig.idle("s1");
    rig.hook("s1", Event::Submitted, Some("t1"));
    rig.deliver("s1", "k1");
    let origin = request("s1", "k1", "x").originator;
    let cancelled = rig
        .wake
        .cancel_delivery(&origin, &PtySessionId::new("s1"), "k1")
        .unwrap();
    assert_eq!(cancelled.status, undelivered(UndeliveredReason::Archived));
    rig.finish_turn("s1");
    rig.wake.tick();
    assert!(rig.writes().is_empty());
    assert_eq!(rig.deliver("s1", "k1").status, cancelled.status);
}

// A restart (new boot id) turns queued deliveries into
// server_restarted, and one caught mid-write into unknown; timers end.
#[test]
fn server_restart_turns_queued_into_server_restarted() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("wake");
    let clock = Arc::new(FakeClock::new());
    let first = rig_at(Some(store.clone()), "boot-A", clock.clone());
    first.idle("s1");
    first.hook("s1", Event::Submitted, Some("t1"));
    assert!(is_queued(&first.deliver("s1", "queued")));
    first.wake.set_timer(timer("s1", 5 * MIN, None)).unwrap();
    first.idle("s2");
    let mid = dir.path().join("mid-write.json");
    *first.fake.snapshot_store.lock().unwrap() = Some((store.join("meta.json"), mid.clone()));
    first.deliver("s2", "in-flight");
    drop(first);

    let second = rig_at(Some(store.clone()), "boot-B", clock.clone());
    assert_eq!(
        status_of(&second, "queued"),
        undelivered(UndeliveredReason::ServerRestarted)
    );
    let answer = second.wake.deliver(request("s1", "queued", "x")).unwrap();
    assert_eq!(
        answer.status,
        undelivered(UndeliveredReason::ServerRestarted)
    );
    assert!(
        is_delivered(&status_of(&second, "in-flight")),
        "kept from the log"
    );
    assert!(second.wake.timers(None).is_empty());
    assert!(second.wake.outcomes(0).outcomes.iter().any(
        |o| o.timer_id.is_some() && o.status == undelivered(UndeliveredReason::ServerRestarted)
    ));
    assert!(second.writes().is_empty());
    assert_eq!(second.wake.boot_id(), "boot-B");

    std::fs::copy(&mid, store.join("meta.json")).unwrap();
    let third = rig_at(Some(store), "boot-C", clock);
    let in_flight: Vec<_> = third
        .wake
        .outcomes(0)
        .outcomes
        .into_iter()
        .filter(|o| o.idempotency_key == "in-flight")
        .collect();
    assert!(
        in_flight
            .iter()
            .any(|o| matches!(o.status, DeliveryStatus::Unknown { .. })),
        "{in_flight:?}"
    );
    assert!(third.writes().is_empty());
}

fn timer(session: &str, after_ms: u64, every_ms: Option<u64>) -> TimerSpec {
    TimerSpec {
        session: PtySessionId::new(session),
        schedule: TimerSchedule::After { after_ms, every_ms },
        until_ms: None,
        count: None,
        text: "time to check CI".into(),
        name: "ci-check".into(),
        created_by: "agent:barb".into(),
        max_wait_ms: 30 * MIN,
        originator: Originator {
            desktop: "desk-a".into(),
            routine_host: None,
        },
        require_observed: false,
        min_gap_ms: None,
    }
}

// A timer asked to need observation is refused at once where no
// fire could ever be typed (a plain shell); agents awaiting their first
// observation may register, while delivery still requires real evidence.
#[test]
fn a_timer_that_needs_observation_is_refused_on_an_unobservable_session() {
    let rig = rig();
    rig.fake.live.lock().unwrap().insert("shell".into());
    rig.agent("codex", AgentCli::Codex);
    assert!(rig
        .fake
        .hook("codex", Event::Started, None, Scope::Unknown)
        .is_err());
    rig.agent("fresh", AgentCli::Claude);
    rig.idle("claude");
    let observed = |session: &str| {
        let mut spec = timer(session, MIN, None);
        spec.require_observed = true;
        rig.wake.set_timer(spec)
    };
    let refused = observed("shell").unwrap_err();
    assert!(refused.contains("cannot be observed"), "{refused}");
    assert!(observed("fresh").is_ok());
    assert!(observed("codex").is_ok());
    assert!(observed("claude").is_ok());
    assert_eq!(rig.wake.timers(None).len(), 3);
    // Without the ask, an unobservable session still takes a timer.
    assert!(rig.wake.set_timer(timer("shell", MIN, None)).is_ok());
}

// A timer fires on the host's schedule with no client listening, and
// its outcome is in the log for the client that reconnects.
#[test]
fn timer_fires_on_schedule_and_its_outcome_waits_in_the_log() {
    let rig = rig();
    rig.idle("s1");
    let set = rig.wake.set_timer(timer("s1", 20 * MIN, None)).unwrap();
    assert_eq!(set.next_at_ms, WALL + 20 * MIN);
    rig.advance(20 * MIN - 1);
    rig.wake.tick();
    assert!(rig.writes().is_empty());
    rig.advance(1);
    rig.wake.tick();
    assert_eq!(
        rig.writes(),
        [
            (
                "s1".into(),
                "\x1b[200~[timer: ci-check | fire 1]\ntime to check CI\x1b[201~".into()
            ),
            ("s1".into(), "\r".into())
        ]
    );
    let log = rig.wake.outcomes(0);
    assert_eq!(log.outcomes.len(), 1);
    let outcome = &log.outcomes[0];
    assert_eq!(outcome.timer_id.as_deref(), Some(set.timer_id.as_str()));
    assert!(is_delivered(&outcome.status));
    assert_eq!(outcome.marker, "ci-check (set by agent:barb)");
    assert!(rig.wake.timers(None).is_empty(), "a one-shot timer is done");
    assert!(rig.wake.outcomes(log.last_seq).outcomes.is_empty());
}

// Fires missed while busy collapse into the pending delivery and do
// not count against the timer.
#[test]
fn coalesced_fires_do_not_consume_the_count() {
    let rig = rig();
    rig.idle("s1");
    rig.hook("s1", Event::Submitted, Some("t1"));
    let mut spec = timer("s1", MIN, Some(MIN));
    spec.count = Some(2);
    rig.wake.set_timer(spec).unwrap();
    rig.advance(MIN);
    rig.wake.tick();
    rig.advance(MIN);
    rig.wake.tick();
    let log = rig.wake.outcomes(0).outcomes;
    assert_eq!(log.len(), 1);
    let DeliveryStatus::Coalesced { into } = &log[0].status else {
        panic!("{log:?}");
    };
    assert_eq!(rig.wake.timers(None)[0].fires, 1);
    rig.finish_turn("s1");
    rig.wake.tick();
    let delivered = rig.wake.outcomes(log[0].seq).outcomes;
    assert_eq!(&delivered[0].delivery_id, into);
    rig.finish_turn("s1");
    rig.advance(MIN);
    rig.wake.tick();
    assert_eq!(rig.writes().len(), 4, "two deliveries for count 2");
    assert!(rig.wake.timers(None).is_empty());
}

// Cancelling a timer archives the fire it has queued.
#[test]
fn cancelling_a_timer_archives_its_queued_fire() {
    let rig = rig();
    rig.idle("s1");
    rig.hook("s1", Event::Submitted, Some("t1"));
    let set = rig.wake.set_timer(timer("s1", MIN, Some(MIN))).unwrap();
    rig.advance(MIN);
    rig.wake.tick();
    assert!(rig
        .wake
        .cancel_timer(&PtySessionId::new("s1"), &set.timer_id));
    let log = rig.wake.outcomes(0).outcomes;
    assert_eq!(
        log.last().unwrap().status,
        undelivered(UndeliveredReason::Archived)
    );
    rig.finish_turn("s1");
    rig.wake.tick();
    assert!(rig.writes().is_empty());
}

// Cron in a non-UTC zone fires at local wall time across DST.
#[test]
fn cron_timer_in_denver_fires_at_nine_local_across_the_fall_back() {
    let rig = rig();
    let set_wall = 1_793_376_000_000; // 2026-10-30T16:00Z, 10:00 MDT
    rig.clock.wall.store(set_wall, Ordering::SeqCst);
    rig.idle("s1");
    let mut spec = timer("s1", 0, None);
    spec.schedule = TimerSchedule::Cron {
        cron: "0 9 * * *".into(),
        tz: "America/Denver".into(),
    };
    let set = rig.wake.set_timer(spec).unwrap();
    let oct31_9_mdt = 1_793_458_800_000; // 2026-10-31T15:00Z
    let nov1_9_mst = 1_793_548_800_000; // 2026-11-01T16:00Z
    assert_eq!(set.next_at_ms, oct31_9_mdt);
    rig.advance(oct31_9_mdt - set_wall);
    rig.wake.tick();
    assert_eq!(rig.writes().len(), 2);
    assert_eq!(rig.wake.timers(None)[0].next_at_ms, nov1_9_mst);
    rig.finish_turn("s1");
    rig.advance(nov1_9_mst - 3_600_000 - oct31_9_mdt);
    rig.wake.tick();
    assert_eq!(rig.writes().len(), 2, "08:00 MST is not 09:00");
    rig.advance(3_600_000);
    rig.wake.tick();
    assert_eq!(rig.writes().len(), 4);
}

// Timer validation.
#[test]
fn invalid_timers_are_refused() {
    let rig = rig();
    rig.idle("s1");
    assert!(rig.wake.set_timer(timer("s1", MIN, Some(MIN - 1))).is_err());
    let mut until = timer("s1", 10 * MIN, None);
    until.until_ms = Some(10 * MIN);
    assert!(rig.wake.set_timer(until).is_err());
    let mut count = timer("s1", MIN, None);
    count.count = Some(0);
    assert!(rig.wake.set_timer(count).is_err());
    assert!(rig.wake.set_timer(timer("gone", MIN, None)).is_err());
    let mut cron = timer("s1", 0, None);
    cron.schedule = TimerSchedule::Cron {
        cron: "0 9 * * *".into(),
        tz: "Not/AZone".into(),
    };
    assert!(rig.wake.set_timer(cron).is_err());
}

// Timers end with their agent — said once in the log, never again.
#[test]
fn timers_end_with_their_agent_once() {
    let rig = rig();
    rig.idle("s1");
    rig.wake.set_timer(timer("s1", MIN, Some(MIN))).unwrap();
    rig.hook("s1", Event::Ended, None);
    let incarnation = rig
        .fake
        .owner
        .snapshot("s1")
        .unwrap()
        .identity
        .session_incarnation;
    rig.fake.owner.exit(
        "s1",
        &incarnation,
        codesense_pty_core::activity::Exit {
            code: Some(0),
            signal: None,
            requested: false,
        },
    );
    for _ in 0..5 {
        rig.advance(MIN);
        rig.wake.tick();
    }
    assert!(rig.wake.timers(None).is_empty());
    let ended: Vec<_> = rig.wake.outcomes(0).outcomes;
    assert_eq!(ended.len(), 1, "{ended:?}");
    assert_eq!(
        ended[0].status,
        undelivered(UndeliveredReason::SessionEnded)
    );
    assert!(rig.writes().is_empty());
}

// ───────────── Per-routine wake limits and the backstop ─────────────

fn budget(name: &str) -> WakeLimits {
    WakeLimits {
        budget: format!("routine:o/t/{name}#1"),
        max_wakes: None,
        max_wakes_per_day: None,
        min_gap_ms: None,
        gap: GapPolicy::Coalesce,
    }
}

fn limited(session: &str, key: &str, limits: &WakeLimits) -> DeliverRequest {
    let mut request = request(session, key, "check the inbox");
    request.limits = Some(limits.clone());
    request
}

const HOUR: u64 = 60 * MIN;

/// Deliver `key` under `limits` into idle `s1`, then let the agent finish
/// the turn it took (so the next one is not held by one-per-turn).
fn fire(rig: &Rig, key: &str, limits: &WakeLimits) -> DeliveryStatus {
    let outcome = rig.wake.deliver(limited("s1", key, limits)).unwrap();
    if is_delivered(&outcome.status) {
        rig.finish_turn("s1");
    }
    outcome.status
}

fn pastes(rig: &Rig) -> usize {
    rig.writes()
        .iter()
        .filter(|(_, text)| text.starts_with("\x1b[200~"))
        .count()
}

// `max_wakes: 3` delivers three times, then answers `max_wakes` (the
// desktop pauses the routine on it); a resume is a new budget and continues.
#[test]
fn max_wakes_three_delivers_three_then_answers_max_wakes_and_a_resume_continues() {
    let rig = rig();
    rig.idle("s1");
    let limits = WakeLimits {
        max_wakes: Some(3),
        ..budget("light")
    };
    for n in 1..=3 {
        assert!(is_delivered(&fire(&rig, &format!("k{n}"), &limits)), "{n}");
        rig.advance(MIN);
    }
    let fourth = fire(&rig, "k4", &limits);
    assert_eq!(fourth, undelivered(UndeliveredReason::MaxWakes));
    assert_eq!(pastes(&rig), 3);
    assert_eq!(status_of(&rig, "k4"), fourth, "logged for the desktop");
    assert_eq!(fire(&rig, "k4", &limits), fourth, "its key answers again");
    // Another routine's budget is its own.
    assert!(is_delivered(&fire(&rig, "other", &budget("other"))));
    // Resumed: the desktop sends the next budget generation.
    let resumed = WakeLimits {
        budget: "routine:o/t/light#2".into(),
        ..limits
    };
    assert!(is_delivered(&fire(&rig, "k5", &resumed)));
}

// A wake still queued holds its place in the budget (no overshoot); one
// that never went in gives it back.
#[test]
fn a_queued_wake_holds_its_budget_and_one_that_never_went_in_gives_it_back() {
    let rig = rig();
    rig.idle("s1");
    rig.hook("s1", Event::Submitted, Some("t1"));
    let limits = WakeLimits {
        max_wakes: Some(1),
        ..budget("heavy")
    };
    let mut first = limited("s1", "k1", &limits);
    first.max_wait_ms = MIN;
    assert!(is_queued(&rig.wake.deliver(first).unwrap()));
    assert_eq!(
        rig.wake
            .deliver(limited("s1", "k2", &limits))
            .unwrap()
            .status,
        undelivered(UndeliveredReason::MaxWakes)
    );
    rig.advance(MIN);
    rig.wake.tick();
    assert_eq!(
        status_of(&rig, "k1"),
        undelivered(UndeliveredReason::BusyTimeout)
    );
    rig.finish_turn("s1");
    assert!(is_delivered(&fire(&rig, "k3", &limits)));
}

// `max_wakes_per_day: 2` over a day of five fires: two delivered, three
// `undelivered(limit_reached)`, and it delivers again the next day.
#[test]
fn max_wakes_per_day_two_of_five_then_again_the_next_day() {
    let rig = rig();
    rig.idle("s1");
    let limits = WakeLimits {
        max_wakes_per_day: Some(2),
        ..budget("digest")
    };
    let mut statuses = Vec::new();
    for n in 0..5 {
        statuses.push(fire(&rig, &format!("d{n}"), &limits));
        rig.advance(4 * HOUR);
    }
    let limit = undelivered(UndeliveredReason::LimitReached);
    assert!(
        is_delivered(&statuses[0]) && is_delivered(&statuses[1]),
        "{statuses:?}"
    );
    assert_eq!(statuses[2..], [limit.clone(), limit.clone(), limit]);
    assert_eq!(pastes(&rig), 2);
    // 20 h in; the first delivery leaves the rolling day at 24 h.
    rig.advance(4 * HOUR);
    assert!(is_delivered(&fire(&rig, "next-day", &limits)));
}

// `min_gap: 2h` with hourly fires: every other fire is coalesced (or,
// with `overlap: skip`, skipped), and the session is never woken inside the
// gap.
#[test]
fn min_gap_two_hours_with_hourly_fires_alternates_and_never_rewakes_inside_it() {
    for gap in [GapPolicy::Coalesce, GapPolicy::Skip] {
        let rig = rig();
        rig.idle("s1");
        let limits = WakeLimits {
            min_gap_ms: Some(2 * HOUR),
            gap,
            ..budget("heavy")
        };
        let mut outcomes = Vec::new();
        for n in 0..5 {
            let outcome = rig
                .wake
                .deliver(limited("s1", &format!("h{n}"), &limits))
                .unwrap();
            if is_delivered(&outcome.status) {
                rig.finish_turn("s1");
            }
            outcomes.push(outcome);
            rig.advance(HOUR);
        }
        for n in [0, 2, 4] {
            assert!(
                is_delivered(&outcomes[n].status),
                "{gap:?} {n}: {outcomes:?}"
            );
        }
        for n in [1, 3] {
            let expected = match gap {
                GapPolicy::Coalesce => DeliveryStatus::Coalesced {
                    into: outcomes[n - 1].delivery_id.clone(),
                },
                GapPolicy::Skip => undelivered(UndeliveredReason::MinGap),
            };
            assert_eq!(outcomes[n].status, expected, "{gap:?} {n}");
        }
        let times: Vec<u64> = rig
            .fake
            .writes
            .lock()
            .unwrap()
            .iter()
            .filter(|w| w.1.starts_with("\x1b[200~"))
            .map(|w| w.2)
            .collect();
        assert_eq!(times.len(), 3, "{gap:?}");
        assert!(
            times.windows(2).all(|w| w[1] - w[0] >= 2 * HOUR),
            "{times:?}"
        );
    }
}

// A fire while the last one of the same routine still waits in the
// queue is inside the gap too: it never queues a second copy.
#[test]
fn a_fire_while_the_last_one_still_waits_is_inside_the_gap() {
    let rig = rig();
    rig.idle("s1");
    rig.hook("s1", Event::Submitted, Some("t1"));
    let limits = WakeLimits {
        min_gap_ms: Some(2 * HOUR),
        ..budget("heavy")
    };
    let first = rig.wake.deliver(limited("s1", "q1", &limits)).unwrap();
    assert!(is_queued(&first));
    let second = rig.wake.deliver(limited("s1", "q2", &limits)).unwrap();
    assert_eq!(
        second.status,
        DeliveryStatus::Coalesced {
            into: first.delivery_id.clone()
        }
    );
    rig.finish_turn("s1");
    rig.wake.tick();
    assert_eq!(pastes(&rig), 1);
}

// A runaway loop (two routines and a timer waking one session every
// minute) trips the backstop: `undelivered(rate_limited)`, one notice naming
// the top source, and it recovers on its own once the loop stops.
#[test]
fn a_runaway_loop_trips_the_backstop_once_and_recovers_when_it_stops() {
    let rig = rig();
    rig.idle("s1");
    let mut spec = timer("s1", MIN, Some(MIN));
    spec.name = "loop-timer".into();
    rig.wake.set_timer(spec).unwrap();
    let routine = |marker: &str, key: String| {
        let mut request = request("s1", &key, "again");
        request.marker = marker.into();
        request.limits = Some(budget(marker));
        let outcome = rig.wake.deliver(request).unwrap();
        if is_delivered(&outcome.status) {
            rig.finish_turn("s1");
        }
        outcome
    };
    let mut refused = Vec::new();
    for minute in 0..50 {
        for n in 0..2 {
            let outcome = routine("Loop A", format!("a{minute}-{n}"));
            if outcome.status == undelivered(UndeliveredReason::RateLimited) {
                refused.push(outcome);
            }
        }
        let outcome = routine("Loop B", format!("b{minute}"));
        if outcome.status == undelivered(UndeliveredReason::RateLimited) {
            refused.push(outcome);
        }
        rig.advance(MIN);
        rig.wake.tick();
        rig.finish_turn("s1");
    }
    assert_eq!(
        pastes(&rig),
        SESSION_RATE_LIMIT,
        "the backstop holds the hour"
    );
    let timer_refused: Vec<_> = rig
        .wake
        .outcomes(0)
        .outcomes
        .into_iter()
        .filter(|o| o.timer_id.is_some() && o.status == undelivered(UndeliveredReason::RateLimited))
        .collect();
    assert!(
        !refused.is_empty() && !timer_refused.is_empty(),
        "every source is counted"
    );
    let notices: Vec<&str> = refused
        .iter()
        .chain(&timer_refused)
        .filter_map(|o| o.notice.as_deref())
        .collect();
    assert_eq!(notices.len(), 1, "said once: {notices:?}");
    assert!(
        notices[0].contains("Loop A"),
        "names the top source: {}",
        notices[0]
    );
    // The loop stops; an hour later the session takes wakes again.
    rig.wake
        .cancel_timer(&PtySessionId::new("s1"), &rig.wake.timers(None)[0].timer_id);
    rig.advance(HOUR);
    rig.wake.tick();
    let back = routine("Loop A", "after".into());
    assert!(is_delivered(&back.status), "{back:?}");
    assert_eq!(back.notice, None);
}

// A loop that keeps running for hours holds the backstop tripped
// (a freed slot is not a new trip): exactly one notice. Once the loop falls
// below half the limit, a later trip is said again.
#[test]
fn a_loop_running_for_hours_is_said_once_and_a_later_trip_again() {
    let rig = rig();
    rig.idle("s1");
    let notices = std::cell::RefCell::new(Vec::new());
    let fire = |key: String| {
        let mut request = request("s1", &key, "again");
        request.marker = "Loop A".into();
        let outcome = rig.wake.deliver(request).unwrap();
        if is_delivered(&outcome.status) {
            rig.finish_turn("s1");
        }
        notices.borrow_mut().extend(outcome.notice);
    };
    // Four a minute for two and a half hours: always over the limit.
    for minute in 0..150 {
        for n in 0..4 {
            fire(format!("l{minute}-{n}"));
        }
        rig.advance(MIN);
        rig.wake.tick();
    }
    assert_eq!(notices.borrow().len(), 1, "{notices:?}");
    // A quiet hour, then a second loop: a new trip, said again.
    rig.advance(HOUR);
    rig.wake.tick();
    for minute in 0..40 {
        for n in 0..4 {
            fire(format!("m{minute}-{n}"));
        }
        rig.advance(MIN);
    }
    assert_eq!(notices.borrow().len(), 2, "{notices:?}");
}

// A budget outlives a long idle (a monthly routine with
// `max_wakes` keeps its count).
#[test]
fn a_budget_survives_months_of_idle() {
    let rig = rig();
    rig.idle("s1");
    let limits = WakeLimits {
        max_wakes: Some(1),
        ..budget("monthly")
    };
    assert!(is_delivered(&fire(&rig, "jan", &limits)));
    for _ in 0..3 {
        rig.advance(31 * 24 * HOUR);
        rig.wake.tick();
    }
    assert_eq!(
        fire(&rig, "apr", &limits),
        undelivered(UndeliveredReason::MaxWakes)
    );
}

// `min_gap` belongs to the routine, not the session: a session
// relaunched inside the gap is not woken again.
#[test]
fn min_gap_holds_across_a_relaunched_session() {
    let rig = rig();
    rig.idle("s1");
    let limits = WakeLimits {
        min_gap_ms: Some(2 * HOUR),
        ..budget("heavy")
    };
    let first = rig.wake.deliver(limited("s1", "h1", &limits)).unwrap();
    assert!(is_delivered(&first.status));
    rig.advance(HOUR);
    rig.idle("s2");
    let second = rig.wake.deliver(limited("s2", "h2", &limits)).unwrap();
    assert_eq!(
        second.status,
        DeliveryStatus::Coalesced {
            into: first.delivery_id
        }
    );
    assert_eq!(pastes(&rig), 1);
}

// r1 LOW: a timer's `min_gap` is at least a minute, like its interval.
#[test]
fn a_timer_min_gap_is_at_least_a_minute() {
    let rig = rig();
    rig.idle("s1");
    let mut spec = timer("s1", MIN, Some(MIN));
    spec.min_gap_ms = Some(MIN - 1);
    assert!(rig.wake.set_timer(spec.clone()).is_err());
    spec.min_gap_ms = Some(MIN);
    assert!(rig.wake.set_timer(spec).is_ok());
}

// The lifetime cap is gone: a session past 500 deliveries is still
// woken.
#[test]
fn a_session_past_500_lifetime_deliveries_is_still_woken() {
    let rig = rig();
    rig.idle("s1");
    for n in 0..501 {
        let outcome = rig.deliver("s1", &format!("k{n}"));
        assert!(is_delivered(&outcome.status), "{n}: {outcome:?}");
        rig.finish_turn("s1");
        rig.advance(31_000);
    }
    assert_eq!(pastes(&rig), 501);
}

// A timer's `min_gap` coalesces a fire inside it into the last delivery,
// which does not count against the timer.
#[test]
fn a_timer_min_gap_coalesces_fires_inside_it() {
    let rig = rig();
    rig.idle("s1");
    let mut spec = timer("s1", MIN, Some(MIN));
    spec.min_gap_ms = Some(2 * MIN);
    let set = rig.wake.set_timer(spec).unwrap();
    assert_eq!(set.min_gap_ms, Some(2 * MIN));
    for _ in 0..5 {
        rig.advance(MIN);
        rig.wake.tick();
        rig.finish_turn("s1");
    }
    let log = rig.wake.outcomes(0).outcomes;
    let kinds: Vec<&str> = log
        .iter()
        .map(|o| match &o.status {
            DeliveryStatus::Delivered { .. } => "d",
            DeliveryStatus::Coalesced { .. } => "c",
            _ => "?",
        })
        .collect();
    assert_eq!(kinds, ["d", "c", "d", "c", "d"], "{log:?}");
    let DeliveryStatus::Coalesced { into } = &log[1].status else {
        unreachable!()
    };
    assert_eq!(into, &log[0].delivery_id);
    assert_eq!(rig.wake.timers(None)[0].fires, 3);
}

// Limits are validated before anything is queued.
#[test]
fn invalid_limits_are_refused() {
    let rig = rig();
    rig.idle("s1");
    let zero = WakeLimits {
        max_wakes: Some(0),
        ..budget("x")
    };
    assert!(rig.wake.deliver(limited("s1", "z", &zero)).is_err());
    let mut spec = timer("s1", MIN, None);
    spec.min_gap_ms = Some(0);
    assert!(rig.wake.set_timer(spec).is_err());
    assert!(rig.writes().is_empty());
}

// A routine's budget outlives a terminal-server restart (its sessions do
// not): `max_wakes` counts across it.
#[test]
fn a_wake_budget_survives_a_server_restart() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("wake");
    let clock = Arc::new(FakeClock::new());
    let limits = WakeLimits {
        max_wakes: Some(1),
        ..budget("heavy")
    };
    let first = rig_at(Some(store.clone()), "boot-A", clock.clone());
    first.idle("s1");
    assert!(is_delivered(&fire(&first, "k1", &limits)));
    drop(first);
    let second = rig_at(Some(store), "boot-B", clock);
    second.idle("s1");
    assert_eq!(
        fire(&second, "k2", &limits),
        undelivered(UndeliveredReason::MaxWakes)
    );
}

// Caps: 200 active timers per machine, 10 per session.
#[test]
fn a_machine_holds_at_most_200_timers_and_a_session_10() {
    let rig = rig();
    for s in 0..21 {
        rig.fake.live.lock().unwrap().insert(format!("s{s}"));
    }
    for s in 0..20 {
        for _ in 0..10 {
            rig.wake
                .set_timer(timer(&format!("s{s}"), MIN, None))
                .unwrap();
        }
    }
    let machine = rig.wake.set_timer(timer("s20", MIN, None)).unwrap_err();
    assert!(machine.contains("200"), "{machine}");
    assert!(rig.wake.cancel_timer(
        &PtySessionId::new("s0"),
        &rig.wake.timers(Some(&PtySessionId::new("s0")))[0].timer_id
    ));
    let session = rig.wake.set_timer(timer("s1", MIN, None)).unwrap_err();
    assert!(session.contains("10"), "{session}");
    rig.wake.set_timer(timer("s20", MIN, None)).unwrap();
}

// Caps: the log keeps 1,000 outcomes and says when a reader missed some.
#[test]
fn the_outcome_log_keeps_1000_and_marks_overflow() {
    let rig = rig();
    for s in 0..3 {
        rig.fake.live.lock().unwrap().insert(format!("shell{s}"));
    }
    for n in 0..1005 {
        let session = format!("shell{}", n / 400);
        rig.wake
            .deliver(request(&session, &format!("k{n}"), "x"))
            .unwrap();
    }
    let all = rig.wake.outcomes(0);
    assert_eq!(all.outcomes.len(), OUTCOME_LOG_CAP);
    assert!(all.overflow);
    assert_eq!(all.last_seq, 1005);
    assert_eq!(all.outcomes[0].seq, 6);
    let caught_up = rig.wake.outcomes(5);
    assert!(!caught_up.overflow);
    assert_eq!(caught_up.outcomes.len(), 1000);
}

// A human key between the paste and its Enter: no Enter is sent, the
// delivery is unknown, and the session stays held.
#[test]
fn input_in_the_paste_enter_gap_withholds_the_enter() {
    let rig = rig();
    rig.idle("s1");
    rig.fake.type_in_gap.store(true, Ordering::SeqCst);
    let outcome = rig.deliver("s1", "k1");
    assert!(
        matches!(outcome.status, DeliveryStatus::Unknown { .. }),
        "{outcome:?}"
    );
    assert_eq!(rig.writes(), [("s1".into(), PASTE.into())], "no Enter");
    assert!(is_queued(&rig.deliver("s1", "k2")));
    for _ in 0..5 {
        rig.advance(MIN);
        rig.wake.tick();
    }
    assert_eq!(rig.writes().len(), 1, "held");
}

// A wait that runs out on a human draft says so.
#[test]
fn a_draft_that_outlasts_the_wait_is_draft_pending() {
    let rig = rig();
    rig.idle("s1");
    rig.fake.typed("s1", true);
    rig.deliver("s1", "k1");
    rig.idle("s2");
    rig.hook("s2", Event::Submitted, Some("t1"));
    rig.deliver("s2", "k2");
    rig.advance(30 * MIN);
    rig.wake.tick();
    assert_eq!(
        status_of(&rig, "k1"),
        undelivered(UndeliveredReason::DraftPending)
    );
    assert_eq!(
        status_of(&rig, "k2"),
        undelivered(UndeliveredReason::BusyTimeout)
    );
}

// Cron is judged against the wall clock, so a machine that slept
// through 09:00 fires when it wakes, though its monotonic clock stood still.
#[test]
fn a_cron_timer_fires_after_the_machine_slept_through_it() {
    let rig = rig();
    rig.clock.wall.store(1_793_376_000_000, Ordering::SeqCst); // 2026-10-30T16:00Z
    rig.idle("s1");
    let mut spec = timer("s1", 0, None);
    spec.schedule = TimerSchedule::Cron {
        cron: "0 9 * * *".into(),
        tz: "America/Denver".into(),
    };
    rig.wake.set_timer(spec).unwrap();
    let monotonic = rig.clock.mono_ms();
    rig.clock.sleep_machine(24 * 60 * MIN);
    rig.wake.tick();
    assert!(
        rig.clock.mono_ms() - monotonic < 2_000,
        "the monotonic clock stood still but for the write's own pauses"
    );
    assert_eq!(rig.writes().len(), 2, "fired on waking");
}

// A timer set before its agent's first activity binds to the agent once
// it appears, so a respawn still ends it.
#[test]
fn a_timer_set_before_the_first_snapshot_binds_when_it_arrives() {
    let rig = rig();
    rig.fake.live.lock().unwrap().insert("s1".into());
    rig.wake
        .set_timer(timer("s1", 10 * MIN, Some(10 * MIN)))
        .unwrap();
    rig.idle("s1");
    rig.wake.tick();
    let old = rig.fake.owner.snapshot("s1").unwrap().identity;
    rig.fake.owner.remove("s1", &old.session_incarnation);
    rig.idle("s1");
    rig.wake.tick();
    assert!(rig.wake.timers(None).is_empty(), "the respawn ended it");
    rig.advance(10 * MIN);
    rig.wake.tick();
    assert!(rig.writes().is_empty());
}

// A queue whose ticks keep failing stops, withdraws itself, and says
// so for every delivery still waiting.
#[test]
fn a_failing_ticker_settles_the_queue_as_unavailable() {
    let rig = rig();
    rig.idle("s1");
    rig.hook("s1", Event::Submitted, Some("t1"));
    rig.deliver("s1", "k1");
    rig.wake.poisoned.store(true, Ordering::SeqCst);
    for _ in 0..3 {
        rig.wake.supervised_tick();
    }
    assert!(!rig.wake.healthy());
    assert_eq!(
        status_of(&rig, "k1"),
        undelivered(UndeliveredReason::Unavailable)
    );
}

// ---- The same engine on a real in-process session server, over its socket.

pub(crate) mod wire {
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use codesense_host_proto::{greet, read_frame, write_frame, Capability, Frame, Hello};
    use codesense_pty_core::domain::{TerminalDomainEvent, ViewerId};
    use codesense_pty_core::protocol::{
        client_hello, instance_id, socket_path, Inbound, Ingress, Message, Reply, Request,
    };

    use super::*;
    use crate::testing::{registration, sh, wait_until, TestServer, WAIT};

    pub(crate) struct Client {
        socket: UnixStream,
        frames: mpsc::Receiver<Frame>,
        ingress: Ingress,
        pub(super) events: Vec<TerminalDomainEvent>,
        rid: u64,
        pub(crate) hello: Hello,
    }

    impl Drop for Client {
        fn drop(&mut self) {
            let _ = self.socket.shutdown(std::net::Shutdown::Both);
        }
    }

    impl Client {
        pub(crate) fn open(root: &Path, viewer: &str) -> Self {
            let socket = UnixStream::connect(socket_path(root)).unwrap();
            let mut reader = socket.try_clone().unwrap();
            reader.set_read_timeout(Some(WAIT)).unwrap();
            let hello = client_hello(instance_id(root), "test", &ViewerId::new(viewer));
            let greeted = greet(&mut reader, &mut &socket, &hello).unwrap();
            let _ = reader.set_read_timeout(None);
            let (tx, frames) = mpsc::channel();
            std::thread::spawn(move || {
                while let Ok(Some(frame)) = read_frame(&mut reader) {
                    if tx.send(frame).is_err() {
                        break;
                    }
                }
            });
            Self {
                socket,
                frames,
                ingress: Ingress::default(),
                events: Vec::new(),
                rid: 0,
                hello: greeted,
            }
        }

        pub(crate) fn call(&mut self, request: Request) -> Result<Reply, String> {
            self.rid += 1;
            let rid = self.rid;
            let frame = Frame::control(&Message::Call { rid, request }).unwrap();
            write_frame(&mut self.socket, &frame).unwrap();
            let deadline = Instant::now() + WAIT;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                let frame = self.frames.recv_timeout(left).expect("reply");
                match self.ingress.accept(&frame).unwrap() {
                    Some(Inbound::Reply { rid: got, result }) if got == rid => return result,
                    Some(Inbound::Event(event)) => self.events.push(event),
                    _ => {}
                }
            }
        }

        /// Wait for a pushed wake outcome.
        pub(super) fn next_wake(&mut self) -> WakeOutcome {
            let deadline = Instant::now() + WAIT;
            loop {
                if let Some(index) = self
                    .events
                    .iter()
                    .position(|event| matches!(event, TerminalDomainEvent::Wake { .. }))
                {
                    let TerminalDomainEvent::Wake { outcome } = self.events.remove(index) else {
                        unreachable!()
                    };
                    return *outcome;
                }
                let left = deadline.saturating_duration_since(Instant::now());
                let frame = self.frames.recv_timeout(left).expect("wake outcome");
                if let Some(Inbound::Event(event)) = self.ingress.accept(&frame).unwrap() {
                    self.events.push(event);
                }
            }
        }

        pub(super) fn spawn_script(
            &mut self,
            server: &TestServer,
            id: &str,
            script: &str,
        ) -> Option<u32> {
            server.track(id);
            let reply = self
                .call(Request::Spawn {
                    intent: sh(script),
                    registration: registration(id, None, false),
                })
                .unwrap();
            let Reply::Spawned { pid } = reply else {
                panic!("{reply:?}")
            };
            pid
        }

        pub(super) fn spawn(&mut self, server: &TestServer, id: &str) {
            self.spawn_script(server, id, "cat");
        }
    }

    #[test]
    fn session_wake_v1_is_advertised_only_where_it_runs() {
        let plain = TestServer::start();
        assert!(!Client::open(plain.root(), "v")
            .hello
            .supports(&Capability::SessionWakeV1));
        let server = TestServer::start();
        server.enable_wake();
        assert!(Client::open(server.root(), "v")
            .hello
            .supports(&Capability::SessionWakeV1));
    }

    // The originator is the connection's viewer, never the request's claim;
    // an unobserved shell is never typed into; a repeated key is answered.
    #[test]
    fn deliver_over_the_socket_stamps_the_originator_and_never_types_unobserved() {
        let server = TestServer::start();
        server.enable_wake();
        let mut client = Client::open(server.root(), "desk-a");
        client.spawn(&server, "w1");
        let mut forged = request("w1", "wire-1", "hello");
        forged.originator.desktop = "desk-forged".into();
        let Reply::WakeDelivery { outcome } = client
            .call(Request::WakeDeliver {
                request: Box::new(forged.clone()),
            })
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(outcome.originator.desktop, "desk-a");
        assert_eq!(outcome.status, undelivered(UndeliveredReason::Unobservable));
        let Reply::WakeDelivery { outcome: again } = client
            .call(Request::WakeDeliver {
                request: Box::new(forged),
            })
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(again, outcome);
        let text = server.text("w1");
        assert!(!text.contains("hello"), "{text:?}");
        let refused = client.call(Request::WakeDeliver {
            request: Box::new(request("nope", "wire-2", "x")),
        });
        assert!(refused.unwrap_err().contains("not running"));
    }

    // Over the wire: a timer fires while no client is connected; the
    // client that reconnects reads its outcome from the log, then gets later
    // ones pushed.
    #[test]
    fn a_timer_fires_while_disconnected_and_the_reconnecting_client_reads_it() {
        let server = TestServer::start();
        server.enable_wake();
        let mut client = Client::open(server.root(), "desk-a");
        client.spawn(&server, "w1");
        let mut spec = timer("w1", 200, None);
        spec.max_wait_ms = 60_000;
        let Reply::WakeTimer { timer: set } = client
            .call(Request::WakeTimerSet {
                spec: Box::new(spec.clone()),
            })
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(set.originator.desktop, "desk-a");
        drop(client);
        let wake = server.host().wake().unwrap();
        assert!(wait_until(|| wake.timers(None).is_empty()));
        let mut back = Client::open(server.root(), "desk-a");
        assert_eq!(back.call(Request::Listen), Ok(Reply::Done));
        let Reply::WakeOutcomes { log } = back.call(Request::WakeOutcomes { since: 0 }).unwrap()
        else {
            panic!()
        };
        assert_eq!(log.outcomes.len(), 1);
        assert_eq!(
            log.outcomes[0].timer_id.as_deref(),
            Some(set.timer_id.as_str())
        );
        assert_eq!(
            log.outcomes[0].status,
            undelivered(UndeliveredReason::Unobservable)
        );
        let mut repeat = spec;
        repeat.schedule = TimerSchedule::After {
            after_ms: 200,
            every_ms: Some(60_000),
        };
        let Reply::WakeTimer { timer: second } = back
            .call(Request::WakeTimerSet {
                spec: Box::new(repeat),
            })
            .unwrap()
        else {
            panic!()
        };
        // An outcome already in the log may still be pushed (pushes race the
        // read; its seq tells them apart).
        let mut pushed = back.next_wake();
        while pushed.seq <= log.last_seq {
            pushed = back.next_wake();
        }
        assert_eq!(pushed.timer_id.as_deref(), Some(second.timer_id.as_str()));
        assert_eq!(pushed.seq, log.last_seq + 1);
        assert_eq!(
            back.call(Request::WakeTimerCancel {
                session: PtySessionId::new("w1"),
                timer_id: second.timer_id.clone(),
            }),
            Ok(Reply::Flag { value: true })
        );
    }

    // On the real host: another desktop's attachment is never taken;
    // the originating desktop's own is used; never alongside a write in
    // flight; input is stamped, drafts with the agent's position.
    #[test]
    fn the_host_claim_never_steals_and_never_overlaps_a_write() {
        let server = TestServer::start();
        server.enable_wake();
        let mut other = Client::open(server.root(), "desk-b");
        other.spawn(&server, "w1");
        let session = PtySessionId::new("w1");
        assert!(server.host().wake_claim(&session, "desk-a").is_some());
        assert_eq!(
            other.call(Request::ClaimWritable {
                session: session.clone()
            }),
            Ok(Reply::Done)
        );
        assert!(server.host().wake_claim(&session, "desk-a").is_none());
        assert!(!server.host().wake_claim_available(&session, "desk-a"));
        assert!(server.host().wake_claim_available(&session, "desk-b"));
        assert!(server.host().wake_claim(&session, "").is_none());
        let held = server.host().wake_claim(&session, "desk-b");
        assert!(held.is_some());
        assert!(
            server.host().wake_claim(&session, "desk-b").is_none(),
            "never alongside another write"
        );
        let mut third = Client::open(server.root(), "desk-c");
        assert_eq!(
            other.call(Request::ReleaseWritable {
                session: session.clone()
            }),
            Ok(Reply::Done)
        );
        assert_eq!(
            third.call(Request::ClaimWritable {
                session: session.clone()
            }),
            Ok(Reply::WritableBusy)
        );
        drop(held);
        assert_eq!(
            third.call(Request::ClaimWritable {
                session: session.clone()
            }),
            Ok(Reply::Done)
        );
        assert!(server.host().last_input(&session).is_none());
        let mut input = codesense_pty_core::protocol::Inputs::default();
        for frame in input.write(&session, 900, b"\x1b[I", || 901).unwrap() {
            write_frame(&mut third.socket, &frame).unwrap();
        }
        assert!(wait_until(|| server.host().last_input(&session).is_some()));
        assert_eq!(server.host().last_input(&session).unwrap().draft, None);
    }

    /// Prepare `id` as a Claude session and spawn `script` in it; its writer
    /// and pid.
    fn agent_session(
        client: &mut Client,
        server: &TestServer,
        id: &str,
        script: &str,
    ) -> (codesense_pty_core::activity::Writer, u32) {
        let Reply::ActivityWriter { writer } = client
            .call(Request::PrepareActivity {
                session: PtySessionId::new(id),
                provider: AgentCli::Claude,
            })
            .unwrap()
        else {
            panic!()
        };
        let pid = client.spawn_script(server, id, script).expect("pid");
        (*writer, pid)
    }

    fn started(server: &TestServer, writer: &codesense_pty_core::activity::Writer, pid: u32) {
        server
            .host()
            .terminal()
            .activity()
            .report_for_test(
                writer,
                pid,
                Report {
                    provider: AgentCli::Claude,
                    event: Event::Started,
                    scope: Scope::Main,
                    report_id: "start".into(),
                    provider_session: Some("p1".into()),
                    turn: None,
                    tool: None,
                    tool_name: None,
                    reason: None,
                    agent: None,
                    subagents: None,
                },
            )
            .unwrap();
    }

    // On the real host, typed text stamps a draft at the agent's position;
    // a lone Ctrl-C clears it.
    #[test]
    fn typed_text_is_a_draft_and_a_lone_ctrl_c_clears_it() {
        let server = TestServer::start();
        server.host().start_activity(server.root()).unwrap();
        server.enable_wake();
        let mut client = Client::open(server.root(), "desk-a");
        let (writer, pid) = agent_session(&mut client, &server, "d1", "stty -isig; exec cat");
        started(&server, &writer, pid);
        let session = PtySessionId::new("d1");
        let mut input = codesense_pty_core::protocol::Inputs::default();
        let mut rid = 1000;
        let mut send = |client: &mut Client, bytes: &[u8]| {
            rid += 2;
            for frame in input.write(&session, rid, bytes, || rid + 1).unwrap() {
                write_frame(&mut client.socket, &frame).unwrap();
            }
        };
        send(&mut client, b"half a thought");
        assert!(wait_until(|| server
            .host()
            .last_input(&session)
            .is_some_and(|input| input.draft.is_some())));
        send(&mut client, b"\x03");
        assert!(wait_until(|| server
            .host()
            .last_input(&session)
            .is_some_and(|input| input.draft.is_none())));
    }

    // The whole path on a real PTY: an observed, idle claude-like process in
    // the foreground, a delivery through `HostWakePort` and the input queue,
    // its paste on screen, the agent's submit confirming it, and the outcome
    // pushed. A second delivery waits for the next turn.
    #[test]
    fn a_real_session_gets_the_delivery_through_the_host_port() {
        let server = TestServer::start();
        server.host().start_activity(server.root()).unwrap();
        server.enable_wake();
        // The agent CLI: bash under the name `claude` (argv[0] names the
        // program for the foreground probe), at a prompt with bracketed
        // paste on, reading lines.
        let bin = tempfile::tempdir().unwrap();
        let claude = bin.path().join("claude");
        std::os::unix::fs::symlink("/bin/bash", &claude).unwrap();
        let script = bin.path().join("agent.sh");
        std::fs::write(
            &script,
            "printf '\\033[?2004h> '\nwhile IFS= read -r line; do printf 'got:%s\\n' \"$line\"; done\n",
        )
        .unwrap();
        let mut client = Client::open(server.root(), "desk-a");
        assert_eq!(client.call(Request::Listen), Ok(Reply::Done));
        assert!(client.call(Request::WakeOutcomes { since: 0 }).is_ok());
        let Reply::ActivityWriter { writer } = client
            .call(Request::PrepareActivity {
                session: PtySessionId::new("real"),
                provider: AgentCli::Claude,
            })
            .unwrap()
        else {
            panic!()
        };
        let pid = client
            .spawn_script(
                &server,
                "real",
                &format!("exec {} {}", claude.display(), script.display()),
            )
            .expect("pid");
        let owner = server.host().terminal().activity().clone();
        let report = |event: Event, turn: Option<&str>, id: &str| {
            owner
                .report_for_test(
                    &writer,
                    pid,
                    Report {
                        provider: AgentCli::Claude,
                        event,
                        scope: Scope::Main,
                        report_id: id.into(),
                        provider_session: Some("p1".into()),
                        turn: turn.map(str::to_string),
                        tool: None,
                        tool_name: None,
                        reason: None,
                        agent: None,
                        subagents: None,
                    },
                )
                .unwrap()
        };
        assert!(wait_until(|| server.text("real").contains(">")));
        report(Event::Started, None, "r1");
        let Reply::WakeDelivery { outcome } = client
            .call(Request::WakeDeliver {
                request: Box::new(DeliverRequest {
                    header: None,
                    ..request("real", "k1", "check the inbox")
                }),
            })
            .unwrap()
        else {
            panic!()
        };
        assert!(
            !outcome.status.is_final() || is_delivered(&outcome.status),
            "{outcome:?}"
        );
        let Reply::WakeDelivery { outcome: second } = client
            .call(Request::WakeDeliver {
                request: Box::new(request("real", "k2", "and then this")),
            })
            .unwrap()
        else {
            panic!()
        };
        assert!(is_queued(&second));
        // The paste reaches the PTY; the agent takes it (its submit hook).
        assert!(
            wait_until(|| server.text("real").contains("check the inbox")),
            "{:?}\n{:?}\nforeground {:?} bracketed {:?} snapshot {:?}",
            server.text("real"),
            server.host().wake().unwrap().outcomes(0),
            server
                .host()
                .wake_agents_in_foreground(&[(PtySessionId::new("real"), "claude".into())]),
            server
                .host()
                .wake_bracketed_paste(&PtySessionId::new("real")),
            server.host().wake_activity(&PtySessionId::new("real")),
        );
        // The Enter is a write of its own, after the gap: the agent has the
        // text but has not read a line yet.
        assert!(
            !server.text("real").contains("got:"),
            "{:?}",
            server.text("real")
        );
        assert!(wait_until(|| server.text("real").contains("got:")));
        report(Event::Submitted, Some("t1"), "r2");
        let pushed = client.next_wake();
        assert_eq!(pushed.idempotency_key, "k1");
        assert!(is_delivered(&pushed.status), "{pushed:?}");
        let screen = server.text("real");
        assert_eq!(
            screen.matches("got:").count(),
            1,
            "the paste arrived once, as one line: {screen:?}"
        );
        assert_eq!(screen.matches("check the inbox").count(), 2, "{screen:?}");
        // One per turn: k2 waits until the agent's turn ends.
        std::thread::sleep(Duration::from_millis(1500));
        assert!(!server.text("real").contains("and then this"));
        report(Event::Stopped, Some("t1"), "r3");
        assert!(wait_until(|| server.text("real").contains("and then this")));
        report(Event::Submitted, Some("t2"), "r4");
        let pushed = client.next_wake();
        assert_eq!(pushed.idempotency_key, "k2");
        assert!(is_delivered(&pushed.status), "{pushed:?}");
    }
}

struct InboxRecovery {
    wake: std::sync::Weak<SessionWake>,
    owner: std::sync::Weak<Fake>,
    fates: Mutex<Vec<codesense_pty_core::inbox_host::Fate>>,
    tools: AtomicBool,
}

impl codesense_pty_core::inbox_host::InboxOutlet for InboxRecovery {
    fn fate(&self, _: &str, _: &str, fate: codesense_pty_core::inbox_host::Fate) {
        self.fates.lock().unwrap().push(fate);
    }
    fn waiter(&self, _: codesense_pty_core::domain::InboxWaiterState) {}
    fn audience(&self, _: &str) -> Option<String> {
        None
    }
    fn has_tools(&self, _: &str) -> Option<bool> {
        Some(self.tools.load(Ordering::SeqCst))
    }
    fn recover(
        &self,
        session: &str,
        notice: &codesense_pty_core::agent_cli::Notice<'_>,
    ) -> codesense_pty_core::agent_cli::SubmitOutcome {
        let result = self.wake.upgrade().unwrap().submit_inbox_notice(
            PtySessionId::new(session),
            notice.text,
            Originator::default(),
            &|| self.tools.load(Ordering::SeqCst),
        );
        if result == codesense_pty_core::agent_cli::SubmitOutcome::Uncertain {
            // The real prompt check carries the submitted wake key, even before the
            // submit call has returned to the inbox owner.
            let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
            self.owner.upgrade().unwrap().owner.inbox().serve(
                self,
                session,
                codesense_pty_core::inbox_waiters::WaitArm::IfAbsent,
                codesense_pty_core::activity::peer::inspect(std::process::id()).unwrap(),
                true,
                Some(notice.text),
                socket,
            );
        }
        result
    }
}

fn recovery(rig: &Rig) -> Arc<InboxRecovery> {
    let outlet = Arc::new(InboxRecovery {
        wake: Arc::downgrade(&rig.wake),
        owner: Arc::downgrade(&rig.fake),
        fates: Mutex::default(),
        tools: AtomicBool::new(true),
    });
    rig.fake.owner.set_inbox_outlet(outlet.clone());
    outlet
}

fn inbox_sweep(rig: &Rig, n: u32) {
    rig.fake
        .owner
        .decay(std::time::Instant::now() + std::time::Duration::from_secs(6) * n);
}

#[test]
fn dead_waiter_mail_wakes_once_and_prompt_confirmation_tells_it() {
    use codesense_pty_core::inbox_host::Fate;
    let rig = rig();
    rig.idle("s");
    let outlet = recovery(&rig);
    rig.fake
        .owner
        .inbox_mail("s", "You have 1 new message.".into(), "notice-1".into())
        .unwrap();
    inbox_sweep(&rig, 1);
    let until = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !outlet.fates.lock().unwrap().contains(&Fate::Told) {
        assert!(std::time::Instant::now() < until);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    rig.finish_turn("s");
    for n in 2..6 {
        inbox_sweep(&rig, n);
    }
    assert_eq!(
        rig.writes().len(),
        2,
        "one paste and one Enter across sweeps"
    );
    assert_eq!(*outlet.fates.lock().unwrap(), [Fate::Told]);
}

#[test]
fn dead_waiter_recovery_never_types_into_a_draft_or_recent_human_input() {
    for draft in [false, true] {
        let rig = rig();
        rig.idle("s");
        let outlet = recovery(&rig);
        let snapshot = rig.fake.owner.snapshot("s").unwrap();
        rig.fake.inputs.lock().unwrap().insert(
            "s".into(),
            InputStamp {
                at_ms: if draft { 0 } else { rig.clock.mono_ms() },
                draft: draft.then_some((snapshot.identity.invocation_id, snapshot.turn_generation)),
            },
        );
        rig.fake
            .owner
            .inbox_mail("s", "You have mail.".into(), "held".into())
            .unwrap();
        inbox_sweep(&rig, 1);
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(rig.writes().is_empty(), "draft={draft}");
        assert!(rig.fake.owner.inbox().holds("s"));
        assert!(!outlet
            .fates
            .lock()
            .unwrap()
            .contains(&codesense_pty_core::inbox_host::Fate::Told));
    }
}

#[test]
fn dead_waiter_recovery_holds_mail_while_the_agents_tools_are_disconnected() {
    let rig = rig();
    rig.idle("s");
    let outlet = recovery(&rig);
    outlet.tools.store(false, Ordering::SeqCst);
    rig.fake
        .owner
        .inbox_mail("s", "You have mail.".into(), "held".into())
        .unwrap();
    inbox_sweep(&rig, 1);
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(rig.writes().is_empty());
    assert!(rig.fake.owner.inbox().holds("s"));
    assert!(outlet.fates.lock().unwrap().is_empty());
}

#[test]
fn a_waiter_cli_that_re_arms_itself_is_never_recovered_through_the_terminal() {
    let rig = rig();
    rig.agent("oc", AgentCli::OpenCode);
    rig.hook("oc", Event::Started, None);
    assert_eq!(rig.state("oc"), State::Idle);
    let outlet = recovery(&rig);
    rig.fake
        .owner
        .inbox_mail("oc", "You have 1 new message.".into(), "held".into())
        .unwrap();
    for n in 1..4 {
        inbox_sweep(&rig, n);
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(
        rig.writes().is_empty(),
        "nothing is pasted into its terminal"
    );
    assert!(
        rig.fake.owner.inbox().holds("oc"),
        "the notice waits for its waiter"
    );
    assert!(outlet.fates.lock().unwrap().is_empty());
}
