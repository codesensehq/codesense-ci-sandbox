// SPDX-License-Identifier: MIT
//! SW-AGR-3 tests: session timers end to end through the desktop's delivery
//! core into the real `SessionWakeV1` engine (the session server's own
//! queue and timer store) over a fake PTY, an injected clock and a driven
//! pty-core `ActivityOwner`. Local and remote take the same path; every case
//! runs both, for a saved agent and an unnamed session (R2, §9.1 matrix).

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use codesense_pty_core::activity::{
    ActivityOwner, Event, Exit, Report, Scope, Snapshot, State, Writer,
};
use codesense_pty_core::agent_cli::AgentCli;
use codesense_pty_core::domain::PtySessionId;
use codesense_pty_core::session_wake::{
    DeliverRequest, DeliveryStatus, Originator, OutcomeLog, TimerInfo, TimerSchedule, TimerSpec,
    UndeliveredReason, WakeOutcome, MAX_TEXT_BYTES, MAX_WAIT_LIMIT_MS,
};
use codesense_session_server::session_wake::{
    Claim, Clock, InputStamp, Runner, SessionWake, WakePort,
};

use super::super::authority::{AuthorityError, Caller, SessionRef, TransportIdentity};
use super::super::events::{
    SessionTimersChanged, SyncHealth, WakeDeliveredEvent, WakeEventSink, WakeOrigin,
    WakeOutcomeEvent,
};
use super::super::host::{HostLookup, HostResolver, WakeHost};
use super::super::run_status::WakeRunStatus;
use super::super::store::RecordDir;
use super::super::{DeliveryError, DeliveryService};
use super::*;
use crate::terminal::session_server::session_wake::WakeCallError;

const LOCAL: &str = "local";
const REMOTE: &str = "codesense-test-1";
const MACHINES: [&str; 2] = [LOCAL, REMOTE];
/// A saved agent's tab and an unnamed (starter) session's tab.
const SESSIONS: [&str; 2] = ["s-saved-barb", "s-unnamed-claude"];
const SEC: u64 = 1_000;
const MIN: u64 = 60 * SEC;
/// 2026-10-05T12:00:00Z.
const WALL: u64 = 1_791_201_600_000;

// ───────────────────────────── the session server ─────────────────────────────

struct FakeClock {
    mono: AtomicU64,
    wall: AtomicU64,
}

impl FakeClock {
    fn advance(&self, ms: u64) {
        self.mono.fetch_add(ms, Ordering::SeqCst);
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

/// Attempts run where they start: every outcome is there at once.
struct Inline;

impl Runner for Inline {
    fn run(&self, job: Box<dyn FnOnce() + Send>) {
        job()
    }
}

/// The PTY side of a session server: activity from a real `ActivityOwner`
/// the test drives; writes recorded. An Enter makes the agent take the
/// prompt (a new turn), as Claude does.
struct Pty {
    owner: ActivityOwner,
    writers: Mutex<BTreeMap<String, Writer>>,
    reports: AtomicU64,
    live: Mutex<HashSet<String>>,
    writes: Mutex<Vec<(String, String)>>,
}

impl Pty {
    fn hook(&self, session: &str, event: Event, turn: Option<&str>) {
        let writers = self.writers.lock().unwrap();
        let provider = self.owner.snapshot(session).unwrap().provider;
        let n = self.reports.fetch_add(1, Ordering::SeqCst);
        self.owner
            .report_for_test(
                &writers[session],
                std::process::id(),
                Report {
                    provider,
                    event,
                    scope: Scope::Main,
                    report_id: format!("r{n}"),
                    provider_session: Some("provider-1".into()),
                    turn: turn.map(str::to_string),
                    tool: None,
                    tool_name: None,
                    reason: None,
                    agent: None,
                    subagents: None,
                },
            )
            .unwrap();
    }
}

impl WakePort for Pty {
    fn session_live(&self, session: &PtySessionId) -> bool {
        self.live.lock().unwrap().contains(session.as_str())
    }
    fn activity(&self, session: &PtySessionId) -> Option<Snapshot> {
        self.owner.snapshot(session.as_str())
    }
    fn agents_in_foreground(&self, queries: &[(PtySessionId, AgentCli)]) -> Vec<Option<bool>> {
        vec![Some(true); queries.len()]
    }
    fn shell_idle(&self, _: &PtySessionId) -> Option<bool> {
        Some(false)
    }
    fn last_input(&self, _: &PtySessionId) -> Option<InputStamp> {
        None
    }
    fn bracketed_paste(&self, _: &PtySessionId) -> Option<bool> {
        Some(true)
    }
    fn output_seq(&self, _: &PtySessionId) -> Option<u64> {
        Some(0)
    }
    fn claim_available(&self, _: &PtySessionId, _: &Originator) -> bool {
        true
    }
    fn claim(&self, _: &PtySessionId, _: &Originator) -> Claim {
        Claim::Held(Box::new(()))
    }
    fn write(&self, session: &PtySessionId, bytes: &[u8]) -> Result<(), String> {
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        self.writes
            .lock()
            .unwrap()
            .push((session.as_str().into(), text.clone()));
        if text == "\r" {
            let n = self.reports.load(Ordering::SeqCst);
            self.hook(
                session.as_str(),
                Event::Submitted,
                Some(&format!("wake-{n}")),
            );
        }
        Ok(())
    }
    fn emit(&self, _: &WakeOutcome) {}
}

/// A session server's `SessionWakeV1` engine as the desktop reaches it.
pub(crate) struct Server {
    pty: Arc<Pty>,
    clock: Arc<FakeClock>,
    wake: Arc<SessionWake>,
}

impl Server {
    pub(crate) fn start() -> Arc<Self> {
        let clock = Arc::new(FakeClock {
            mono: AtomicU64::new(1_000),
            wall: AtomicU64::new(WALL),
        });
        Self::open(clock, "boot-1", None)
    }

    fn open(clock: Arc<FakeClock>, boot: &str, store: Option<PathBuf>) -> Arc<Self> {
        let pty = Arc::new(Pty {
            owner: ActivityOwner::new(),
            writers: Mutex::default(),
            reports: AtomicU64::new(0),
            live: Mutex::default(),
            writes: Mutex::default(),
        });
        let wake = SessionWake::open(
            pty.clone(),
            clock.clone(),
            Arc::new(Inline),
            boot.into(),
            store,
        );
        Arc::new(Self { pty, clock, wake })
    }

    /// A live interactive Claude session at its prompt.
    fn idle(&self, session: &str) {
        self.starting(session, AgentCli::Claude);
        self.observe(session);
        assert_eq!(self.state(session), State::Idle);
    }

    pub(crate) fn starting(&self, session: &str, provider: AgentCli) {
        let writer = self
            .pty
            .owner
            .prepare_native(session, provider, "unused")
            .unwrap();
        self.pty
            .owner
            .register(session, Some(std::process::id()))
            .unwrap();
        self.pty
            .writers
            .lock()
            .unwrap()
            .insert(session.into(), writer);
        self.pty.live.lock().unwrap().insert(session.into());
    }

    pub(crate) fn observe(&self, session: &str) {
        self.pty.hook(session, Event::Started, None);
    }

    pub(crate) fn generation(&self, session: &str) -> u64 {
        self.pty.owner.snapshot(session).unwrap().turn_generation
    }

    /// A live session CodeSense cannot observe: a plain shell (no agent),
    /// or Codex, whose activity is not qualified yet (`Scope::Unknown`).
    fn unobservable(&self, session: &str, codex: bool) {
        if codex {
            let writer = self
                .pty
                .owner
                .prepare_native(session, AgentCli::Codex, "unused")
                .unwrap();
            self.pty
                .owner
                .register(session, Some(std::process::id()))
                .unwrap();
            self.pty
                .writers
                .lock()
                .unwrap()
                .insert(session.into(), writer);
            assert!(!self.pty.owner.snapshot(session).unwrap().observed);
        }
        self.pty.live.lock().unwrap().insert(session.into());
    }

    /// The agent is working on a turn of its own.
    fn busy(&self, session: &str) {
        self.pty.hook(session, Event::Submitted, Some("long-task"));
        assert_ne!(self.state(session), State::Idle);
    }

    fn finish_turn(&self, session: &str) {
        self.pty.hook(session, Event::Stopped, None);
    }

    /// The agent exits; its tab stays open on the shell.
    fn agent_exits(&self, session: &str) {
        self.pty.hook(session, Event::Ended, None);
        let incarnation = self
            .pty
            .owner
            .snapshot(session)
            .unwrap()
            .identity
            .session_incarnation;
        self.pty.owner.exit(
            session,
            &incarnation,
            Exit {
                code: Some(0),
                signal: None,
                requested: false,
            },
        );
    }

    /// The tab is closed: its PTY is gone.
    fn close(&self, session: &str) {
        self.pty.live.lock().unwrap().remove(session);
    }

    fn state(&self, session: &str) -> State {
        self.pty.owner.snapshot(session).unwrap().state
    }

    /// `ms` pass on the server, which ticks once.
    pub(crate) fn after(&self, ms: u64) {
        self.clock.advance(ms);
        self.wake.tick();
    }

    pub(crate) fn writes(&self, session: &str) -> Vec<String> {
        self.pty
            .writes
            .lock()
            .unwrap()
            .iter()
            .filter(|(s, _)| s == session)
            .map(|(_, text)| text.clone())
            .collect()
    }

    fn now(&self) -> u64 {
        self.clock.wall_ms()
    }
}

/// The desktop's connection to a [`Server`].
pub(crate) struct Link(pub(crate) Arc<Server>);

impl WakeHost for Link {
    fn deliver(&self, request: DeliverRequest) -> Result<WakeOutcome, WakeCallError> {
        self.0.wake.deliver(request).map_err(WakeCallError::Refused)
    }
    fn cancel(&self, session: &PtySessionId, key: &str) -> Result<WakeOutcome, WakeCallError> {
        self.0
            .wake
            .cancel_delivery(&Originator::default(), session, key)
            .ok_or_else(|| WakeCallError::Refused("nothing queued".into()))
    }
    fn timer_set(&self, spec: TimerSpec) -> Result<TimerInfo, WakeCallError> {
        self.0.wake.set_timer(spec).map_err(WakeCallError::Refused)
    }
    fn timer_list(&self, session: Option<&PtySessionId>) -> Result<Vec<TimerInfo>, WakeCallError> {
        Ok(self.0.wake.timers(session))
    }
    fn timer_cancel(&self, session: &PtySessionId, timer_id: &str) -> Result<bool, WakeCallError> {
        Ok(self.0.wake.cancel_timer(session, timer_id))
    }
    fn outcomes(&self, since: u64) -> Result<OutcomeLog, WakeCallError> {
        Ok(self.0.wake.outcomes(since))
    }
}

// ───────────────────────────── the desktop ─────────────────────────────

/// Which machines this desktop reaches now.
#[derive(Default)]
struct Machines {
    ready: Mutex<BTreeMap<String, Arc<Server>>>,
    offline: Mutex<HashSet<String>>,
}

impl HostResolver for Machines {
    fn resolve(&self, machine: &str) -> HostLookup {
        if self.offline.lock().unwrap().contains(machine) {
            return HostLookup::Offline {
                detail: "not connected".into(),
            };
        }
        match self.ready.lock().unwrap().get(machine) {
            Some(server) => HostLookup::Ready(Arc::new(Link(server.clone()))),
            None => HostLookup::Unknown,
        }
    }
}

#[derive(Default)]
struct Window {
    delivered: Mutex<Vec<WakeDeliveredEvent>>,
    outcomes: Mutex<Vec<WakeOutcomeEvent>>,
    timers_changed: Mutex<Vec<SessionTimersChanged>>,
}

impl WakeEventSink for Window {
    fn delivered(&self, event: &WakeDeliveredEvent) {
        self.delivered.lock().unwrap().push(event.clone());
    }
    fn outcome(&self, event: &WakeOutcomeEvent) {
        self.outcomes.lock().unwrap().push(event.clone());
    }
    fn sync_health(&self, _: &SyncHealth) {}
    fn timers_changed(&self, event: &SessionTimersChanged) {
        self.timers_changed.lock().unwrap().push(event.clone());
    }
}

struct Desktop {
    machines: Arc<Machines>,
    window: Arc<Window>,
    service: DeliveryService,
}

impl Desktop {
    fn new() -> Self {
        let machines = Arc::new(Machines::default());
        let window = Arc::new(Window::default());
        let service = DeliveryService::new(
            machines.clone(),
            window.clone(),
            RecordDir::at(None),
            "this-mac".into(),
        );
        Self {
            machines,
            window,
            service,
        }
    }

    fn connect(&self, machine: &str, server: &Arc<Server>) {
        self.machines.offline.lock().unwrap().remove(machine);
        self.machines
            .ready
            .lock()
            .unwrap()
            .insert(machine.into(), server.clone());
        self.service.sync(machine).unwrap();
    }

    fn disconnect(&self, machine: &str) {
        self.machines.offline.lock().unwrap().insert(machine.into());
    }

    /// The agent in `own` calls `schedule_wakeup`.
    fn agent_schedules(
        &self,
        server: &Server,
        own: &SessionRef,
        ask: &WakeupAsk,
    ) -> Result<ScheduledWakeup, DeliveryError> {
        schedule(
            &self.service,
            &Caller::Session(own.clone()),
            own,
            ask,
            server.now(),
            "Barb",
        )
    }

    fn agent_lists(&self, own: &SessionRef) -> Result<Vec<WakeupRow>, DeliveryError> {
        list(&self.service, &Caller::Session(own.clone()), own)
    }
}

fn ask(prompt: &str) -> WakeupAsk {
    WakeupAsk {
        prompt: prompt.into(),
        ..WakeupAsk::default()
    }
}

fn in_(duration: &str, prompt: &str) -> WakeupAsk {
    WakeupAsk {
        in_: Some(duration.into()),
        ..ask(prompt)
    }
}

fn rfc3339(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn paste(header: &str, text: &str) -> String {
    format!("\x1b[200~{header}\n{text}\x1b[201~")
}

// ───────────────────────────── AC1 ─────────────────────────────

// AC1 (+ R2): an interactive session, unnamed or saved, local or remote,
// schedules `in: "2m"`; two minutes later (server clock) the prompt is
// typed once, with its marker, and the desktop shows the delivery.
#[test]
fn a_session_schedules_in_two_minutes_and_is_woken_two_minutes_later() {
    for machine in MACHINES {
        for session in SESSIONS {
            let server = Server::start();
            let desktop = Desktop::new();
            desktop.connect(machine, &server);
            server.idle(session);
            let own = SessionRef::new(machine, session);
            let set = desktop
                .agent_schedules(
                    &server,
                    &own,
                    &WakeupAsk {
                        name: Some("build check".into()),
                        ..in_("2m", "check whether the build finished")
                    },
                )
                .unwrap();
            assert_eq!(set.next_at, rfc3339(WALL + 2 * MIN), "{machine}/{session}");
            assert_eq!(set.machine_id, machine);
            assert_eq!(set.session_id, session);
            server.after(2 * MIN - SEC);
            assert!(server.writes(session).is_empty(), "not before two minutes");
            server.after(SEC);
            assert_eq!(
                server.writes(session),
                [
                    paste(
                        "[timer: build check | fire 1]",
                        "check whether the build finished"
                    ),
                    "\r".to_string()
                ],
                "{machine}/{session}"
            );
            // The desktop catches up: the tab's history and the woken caption
            // name the timer and who set it.
            desktop.service.sync(machine).unwrap();
            let delivered = desktop.window.delivered.lock().unwrap().clone();
            assert_eq!(delivered.len(), 1);
            assert_eq!(delivered[0].marker, "build check (set by Barb)");
            assert_eq!(delivered[0].caption, "Woken by build check (set by Barb)");
            assert!(matches!(
                delivered[0].origin,
                WakeOrigin::Timer {
                    timer_id: Some(ref id)
                } if *id == set.timer_id
            ));
            let history = desktop.service.history(machine, session);
            assert_eq!(history.len(), 1);
            assert!(matches!(history[0].status, WakeRunStatus::Delivered { .. }));
            // A one-shot timer is done.
            assert!(desktop.agent_lists(&own).unwrap().is_empty());
        }
    }
}

// ───────────────────────────── AC2 ─────────────────────────────

// AC2 + §9.6 "while the desktop is offline": a remote session's timer keeps
// firing on the machine; setting, listing and cancelling say clearly that
// the machine is offline; after the reconnect the outcome is there and the
// list is what the server holds.
#[test]
fn a_remote_timer_fires_while_the_desktop_is_away_and_shows_after_reconnect() {
    let server = Server::start();
    let desktop = Desktop::new();
    desktop.connect(REMOTE, &server);
    for session in SESSIONS {
        server.idle(session);
    }
    let own = SessionRef::new(REMOTE, SESSIONS[1]);
    let set = desktop
        .agent_schedules(
            &server,
            &own,
            &WakeupAsk {
                every: Some("5m".into()),
                ..in_("5m", "read your inbox")
            },
        )
        .unwrap();
    desktop.disconnect(REMOTE);
    for refused in [
        desktop
            .agent_schedules(&server, &own, &in_("1m", "x"))
            .map(|_| ())
            .unwrap_err(),
        desktop.agent_lists(&own).map(|_| ()).unwrap_err(),
        cancel(
            &desktop.service,
            &Caller::Session(own.clone()),
            &own,
            &set.timer_id,
        )
        .map(|_| ())
        .unwrap_err(),
    ] {
        assert!(
            matches!(refused, DeliveryError::Offline { .. }),
            "{refused:?}"
        );
        let said = message(&refused);
        assert!(said.contains("codesense-test-1 is offline"), "{said}");
        assert!(said.contains("keep firing"), "{said}");
    }
    // The desktop is away; the machine fires on its own clock.
    server.after(5 * MIN);
    assert_eq!(server.writes(SESSIONS[1]).len(), 2, "pasted and submitted");
    assert!(desktop.service.history(REMOTE, SESSIONS[1]).is_empty());
    // Back: the outcome is caught up, and the list is the server's.
    desktop.connect(REMOTE, &server);
    let history = desktop.service.history(REMOTE, SESSIONS[1]);
    assert_eq!(history.len(), 1);
    assert!(matches!(history[0].status, WakeRunStatus::Delivered { .. }));
    let outcomes = desktop.window.outcomes.lock().unwrap().clone();
    assert!(outcomes.iter().all(|o| o.ours), "this desktop set it");
    let rows = desktop.agent_lists(&own).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].timer_id, set.timer_id);
    assert_eq!(rows[0].fires, 1);
    assert_eq!(rows[0].every.as_deref(), Some("5m"));
    assert_eq!(rows[0].prompt, "read your inbox");
    assert_eq!(rows[0].next_at, rfc3339(WALL + 10 * MIN));
}

// ───────────────────────────── AC3 ─────────────────────────────

// AC3: the session ends (its agent exits, or its tab is closed), and its
// timers are gone on the server that held them, local and remote; nothing
// is typed, and another session's timers are untouched.
#[test]
fn a_sessions_timers_end_with_it_local_and_remote() {
    for machine in MACHINES {
        for close_tab in [false, true] {
            let server = Server::start();
            let desktop = Desktop::new();
            desktop.connect(machine, &server);
            let (gone, other) = (SESSIONS[0], SESSIONS[1]);
            server.idle(gone);
            server.idle(other);
            let own = SessionRef::new(machine, gone);
            let kept = SessionRef::new(machine, other);
            desktop
                .agent_schedules(&server, &own, &in_("10m", "one"))
                .unwrap();
            desktop
                .agent_schedules(
                    &server,
                    &own,
                    &WakeupAsk {
                        every: Some("1m".into()),
                        ..in_("1m", "two")
                    },
                )
                .unwrap();
            desktop
                .agent_schedules(&server, &kept, &in_("10m", "three"))
                .unwrap();
            if close_tab {
                server.close(gone);
            } else {
                server.agent_exits(gone);
            }
            server.after(SEC);
            assert!(
                desktop.agent_lists(&own).unwrap().is_empty(),
                "{machine} close_tab={close_tab}"
            );
            assert!(server
                .wake
                .timers(Some(&PtySessionId::new(gone)))
                .is_empty());
            assert_eq!(desktop.agent_lists(&kept).unwrap().len(), 1);
            for _ in 0..12 {
                server.after(MIN);
            }
            assert!(server.writes(gone).is_empty());
            desktop.service.sync(machine).unwrap();
            let ended: Vec<WakeOutcomeEvent> = desktop
                .window
                .outcomes
                .lock()
                .unwrap()
                .iter()
                .filter(|o| o.session_id == gone)
                .cloned()
                .collect();
            assert_eq!(ended.len(), 2, "each timer's end said once: {ended:?}");
            assert!(ended.iter().all(|o| o.status
                == WakeRunStatus::of(&DeliveryStatus::Undelivered {
                    reason: UndeliveredReason::SessionEnded
                })));
        }
    }
}

// §9.9 "on startup, timers whose session no longer exists are cleaned up":
// a restarted terminal server ran no session over, so it starts with no
// timers, and the desktop learns each one ended.
#[test]
fn a_restarted_server_starts_with_no_orphan_timers() {
    let store = tempfile::tempdir().unwrap();
    let first = Server::open(
        Arc::new(FakeClock {
            mono: AtomicU64::new(1_000),
            wall: AtomicU64::new(WALL),
        }),
        "boot-1",
        Some(store.path().to_path_buf()),
    );
    let desktop = Desktop::new();
    desktop.connect(LOCAL, &first);
    first.idle(SESSIONS[1]);
    let own = SessionRef::new(LOCAL, SESSIONS[1]);
    desktop
        .agent_schedules(&first, &own, &in_("30m", "later"))
        .unwrap();
    let clock = first.clock.clone();
    drop(first);
    let second = Server::open(clock, "boot-2", Some(store.path().to_path_buf()));
    assert!(second.wake.timers(None).is_empty());
    desktop.connect(LOCAL, &second);
    assert!(desktop.agent_lists(&own).unwrap().is_empty());
    let history = desktop.service.history(LOCAL, SESSIONS[1]);
    assert_eq!(history.len(), 1);
    assert_eq!(
        history[0].status,
        WakeRunStatus::of(&DeliveryStatus::Undelivered {
            reason: UndeliveredReason::ServerRestarted
        })
    );
}

// ───────────────────────────── AC4 ─────────────────────────────

// AC4: `every: "1m"` while the session is busy for five minutes: one
// delivery, when it goes idle; the fires in between collapse into it.
#[test]
fn every_minute_while_busy_for_five_minutes_delivers_once_at_idle() {
    for machine in MACHINES {
        for session in SESSIONS {
            let server = Server::start();
            let desktop = Desktop::new();
            desktop.connect(machine, &server);
            server.idle(session);
            server.busy(session);
            let own = SessionRef::new(machine, session);
            desktop
                .agent_schedules(
                    &server,
                    &own,
                    &WakeupAsk {
                        every: Some("1m".into()),
                        ..in_("1m", "status?")
                    },
                )
                .unwrap();
            for _ in 0..5 {
                server.after(MIN);
            }
            assert!(server.writes(session).is_empty(), "never typed while busy");
            server.finish_turn(session);
            server.after(SEC);
            assert_eq!(
                server.writes(session),
                [paste("[timer: status? | fire 1]", "status?"), "\r".into()],
                "{machine}/{session}: exactly one delivery"
            );
            desktop.service.sync(machine).unwrap();
            let statuses: Vec<WakeRunStatus> = desktop
                .window
                .outcomes
                .lock()
                .unwrap()
                .iter()
                .filter(|o| o.status.is_final())
                .map(|o| o.status.clone())
                .collect();
            let delivered = statuses
                .iter()
                .filter(|s| matches!(s, WakeRunStatus::Delivered { .. }))
                .count();
            let coalesced = statuses
                .iter()
                .filter(|s| matches!(s, WakeRunStatus::Coalesced { .. }))
                .count();
            assert_eq!((delivered, coalesced), (1, 4), "{statuses:?}");
            assert_eq!(desktop.agent_lists(&own).unwrap()[0].fires, 1);
        }
    }
}

// ───────────────────────────── AC5 ─────────────────────────────

// AC5: ten active timers per session; the eleventh is refused with the
// limit, and nothing is added.
#[test]
fn an_eleventh_timer_is_refused_with_the_limit() {
    for machine in MACHINES {
        let server = Server::start();
        let desktop = Desktop::new();
        desktop.connect(machine, &server);
        server.idle(SESSIONS[1]);
        let own = SessionRef::new(machine, SESSIONS[1]);
        for n in 0..10 {
            desktop
                .agent_schedules(&server, &own, &in_("1h", &format!("timer {n}")))
                .unwrap();
        }
        let refused = desktop
            .agent_schedules(&server, &own, &in_("1h", "one too many"))
            .unwrap_err();
        let said = message(&refused);
        assert!(
            said.contains("already has 10 active timers"),
            "{machine}: {said}"
        );
        assert_eq!(desktop.agent_lists(&own).unwrap().len(), 10);
    }
}

// ───────────────────────────── AC6 / §9.5 ─────────────────────────────

// AC6 + §9.5: a timer only targets its owner. A caller naming another
// session is refused before anything reaches the server; it cannot list
// that session's timers, and cancelling another session's timer id from
// its own session finds nothing.
#[test]
fn a_forged_session_is_refused_and_nothing_is_set() {
    for machine in MACHINES {
        let server = Server::start();
        let desktop = Desktop::new();
        desktop.connect(machine, &server);
        let (mine, theirs) = (SESSIONS[1], SESSIONS[0]);
        server.idle(mine);
        server.idle(theirs);
        let own = SessionRef::new(machine, mine);
        let other = SessionRef::new(machine, theirs);
        let forged = schedule(
            &desktop.service,
            &Caller::Session(own.clone()),
            &other,
            &in_("2m", "do their work"),
            server.now(),
            "Barb",
        )
        .unwrap_err();
        assert_eq!(
            forged,
            DeliveryError::Authority(AuthorityError::Forged {
                claimed: theirs.into()
            })
        );
        assert!(server.wake.timers(None).is_empty(), "nothing reached it");
        assert!(list(&desktop.service, &Caller::Session(own.clone()), &other).is_err());
        let theirs_timer = desktop
            .agent_schedules(&server, &other, &in_("2m", "theirs"))
            .unwrap();
        assert_eq!(
            cancel(
                &desktop.service,
                &Caller::Session(own.clone()),
                &own,
                &theirs_timer.timer_id
            ),
            Ok(false)
        );
        assert!(matches!(
            cancel(
                &desktop.service,
                &Caller::Session(own.clone()),
                &other,
                &theirs_timer.timer_id
            ),
            Err(DeliveryError::Authority(AuthorityError::Forged { .. }))
        ));
        assert_eq!(server.wake.timers(None).len(), 1, "theirs is untouched");
        // A routine host never owns a timer.
        assert!(matches!(
            schedule(
                &desktop.service,
                &Caller::RoutineHost,
                &own,
                &in_("2m", "x"),
                server.now(),
                "routine"
            ),
            Err(DeliveryError::Authority(AuthorityError::NotOwnSession))
        ));
    }
}

// §5 human use: "Remind this agent…" sets a timer on that tab with the
// person as its author, local or remote.
#[test]
fn a_person_reminds_a_tab_and_is_its_author() {
    for machine in MACHINES {
        let server = Server::start();
        let desktop = Desktop::new();
        desktop.connect(machine, &server);
        server.idle(SESSIONS[1]);
        let tab = SessionRef::new(machine, SESSIONS[1]);
        let set = schedule(
            &desktop.service,
            &Caller::Human(crate::commands::wake::HumanGesture::for_test()),
            &tab,
            &in_("20m", "Check whether the build finished"),
            server.now(),
            HUMAN_AUTHOR,
        )
        .unwrap();
        assert_eq!(set.name, "Check whether the build");
        let rows = list(
            &desktop.service,
            &Caller::Human(crate::commands::wake::HumanGesture::for_test()),
            &tab,
        )
        .unwrap();
        assert_eq!(rows[0].created_by, "you");
        server.after(20 * MIN);
        desktop.service.sync(machine).unwrap();
        assert_eq!(
            desktop.window.delivered.lock().unwrap()[0].marker,
            "Check whether the build (set by you)"
        );
        assert_eq!(
            cancel(
                &desktop.service,
                &Caller::Human(crate::commands::wake::HumanGesture::for_test()),
                &tab,
                "no-such-timer"
            ),
            Ok(false)
        );
    }
}

// SW-AGR-6: the tab's timer chip learns of a timer an agent or the person
// set or cancelled, local and remote, saved agent and unnamed session alike;
// a refused set and a cancel of nothing say nothing.
#[test]
fn setting_and_cancelling_a_timer_tells_the_window() {
    for machine in MACHINES {
        for session in SESSIONS {
            let server = Server::start();
            let desktop = Desktop::new();
            desktop.connect(machine, &server);
            server.idle(session);
            let own = SessionRef::new(machine, session);
            let changed = || desktop.window.timers_changed.lock().unwrap().clone();
            let expected = SessionTimersChanged {
                machine_id: machine.to_string(),
                session_id: session.to_string(),
            };

            let set = desktop
                .agent_schedules(&server, &own, &in_("20m", "check the build"))
                .unwrap();
            assert_eq!(
                changed(),
                vec![expected.clone()],
                "{machine} {session}: set"
            );

            assert!(desktop
                .agent_schedules(&server, &own, &in_("0m", "never"))
                .is_err());
            let person = Caller::Human(crate::commands::wake::HumanGesture::for_test());
            assert_eq!(
                cancel(&desktop.service, &person, &own, "no-such-timer"),
                Ok(false)
            );
            assert_eq!(changed().len(), 1, "{machine} {session}: nothing changed");

            assert_eq!(
                cancel(&desktop.service, &person, &own, &set.timer_id),
                Ok(true)
            );
            assert_eq!(
                changed(),
                vec![expected.clone(), expected],
                "{machine} {session}: cancel"
            );
        }
    }
}

#[test]
fn the_timers_changed_event_is_camel_case() {
    let event = SessionTimersChanged {
        machine_id: "codesense-test-1".into(),
        session_id: "s-1".into(),
    };
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        serde_json::json!({"machineId": "codesense-test-1", "sessionId": "s-1"})
    );
}

// §9.1: a headless run (`claude -p`, a remote task) gets no timers.
#[test]
fn headless_runs_are_refused() {
    for (session, headless) in [("headless-1791201600000-1", false), ("s-run", true)] {
        let refused = require_interactive(session, headless).unwrap_err();
        assert!(
            message(&refused).contains("not supported for headless runs"),
            "{refused:?}"
        );
    }
    assert_eq!(require_interactive("s-unnamed-claude", false), Ok(()));
}

// ───────────────────────────── §9.9 validation ─────────────────────────────

const NOW: u64 = WALL;

fn refused(ask: &WakeupAsk) -> String {
    plan(ask, NOW, "Barb").expect_err("refused")
}

#[test]
fn the_first_fire_is_within_366_days() {
    assert!(plan(&in_("366d", "x"), NOW, "Barb").is_ok());
    assert!(refused(&in_("367d", "x")).contains("366 days"));
}

#[test]
fn exactly_one_of_in_or_at() {
    assert!(refused(&ask("x")).contains("exactly one of `in` or `at`"));
    let both = WakeupAsk {
        at: Some(rfc3339(NOW + 10 * MIN)),
        ..in_("5m", "x")
    };
    assert!(refused(&both).contains("exactly one of `in` or `at`"));
}

#[test]
fn at_is_a_future_time_with_an_offset() {
    let local = WakeupAsk {
        at: Some("2026-10-05T14:30:00".into()),
        ..ask("x")
    };
    assert!(
        refused(&local).contains("time zone offset"),
        "{}",
        refused(&local)
    );
    let past = WakeupAsk {
        at: Some(rfc3339(NOW - MIN)),
        ..ask("x")
    };
    assert!(refused(&past).contains("in the future"));
    let now = WakeupAsk {
        at: Some(rfc3339(NOW)),
        ..ask("x")
    };
    assert!(refused(&now).contains("in the future"));
    // 2026-10-05T07:30:00-07:00 is 14:30Z, two and a half hours from NOW.
    let later = WakeupAsk {
        at: Some("2026-10-05T07:30:00-07:00".into()),
        ..ask("x")
    };
    let timer = plan(&later, NOW, "Barb").unwrap();
    assert_eq!(
        timer.schedule,
        TimerSchedule::After {
            after_ms: 150 * MIN,
            every_ms: None
        }
    );
}

#[test]
fn every_is_at_least_a_minute_and_durations_are_strict() {
    let fast = WakeupAsk {
        every: Some("30s".into()),
        ..in_("1m", "x")
    };
    assert!(refused(&fast).contains("at least 1m"));
    for bad in [
        "",
        "5",
        "5 minutes",
        "0m",
        "m",
        "-5m",
        "1.5h",
        "5x",
        "99999999999999999999d",
    ] {
        let message = refused(&in_(bad, "x"));
        assert!(message.contains("`in`"), "{bad:?}: {message}");
    }
    let timer = plan(
        &WakeupAsk {
            every: Some("15m".into()),
            count: Some(3),
            ..in_("1h30m", "x")
        },
        NOW,
        "Barb",
    )
    .unwrap();
    assert_eq!(
        timer.schedule,
        TimerSchedule::After {
            after_ms: 90 * MIN,
            every_ms: Some(15 * MIN)
        }
    );
    assert_eq!(timer.count, Some(3));
    assert_eq!(
        timer.max_wait_ms, MAX_WAIT_LIMIT_MS,
        "a fire waits up to 24 h"
    );
    assert!(timer.require_observed);
    assert_eq!(timer.created_by, "Barb");
    let seconds = plan(&in_("90s", "x"), NOW, "Barb").unwrap();
    assert_eq!(
        seconds.schedule,
        TimerSchedule::After {
            after_ms: 90 * SEC,
            every_ms: None
        }
    );
}

#[test]
fn until_is_after_the_first_fire_and_count_is_at_least_one() {
    let early = WakeupAsk {
        every: Some("5m".into()),
        until: Some(rfc3339(NOW + 10 * MIN)),
        ..in_("10m", "x")
    };
    assert!(refused(&early).contains("after the first fire"));
    let no_offset = WakeupAsk {
        every: Some("5m".into()),
        until: Some("2026-10-05T23:00:00".into()),
        ..in_("10m", "x")
    };
    assert!(refused(&no_offset).contains("time zone offset"));
    let fine = plan(
        &WakeupAsk {
            every: Some("5m".into()),
            until: Some(rfc3339(NOW + 60 * MIN)),
            ..in_("10m", "x")
        },
        NOW,
        "Barb",
    )
    .unwrap();
    assert_eq!(fine.until_ms, Some(60 * MIN), "a duration, not a timestamp");
    let zero = WakeupAsk {
        every: Some("5m".into()),
        count: Some(0),
        ..in_("10m", "x")
    };
    assert!(refused(&zero).contains("`count`"));
    // SW-AGR-13: no 500 ceiling: a week of 15-minute checks is fine.
    let week = WakeupAsk {
        every: Some("15m".into()),
        count: Some(672),
        ..in_("15m", "x")
    };
    assert_eq!(plan(&week, NOW, "Barb").unwrap().count, Some(672));
    let both = WakeupAsk {
        every: Some("5m".into()),
        count: Some(2),
        until: Some(rfc3339(NOW + 60 * MIN)),
        ..in_("10m", "x")
    };
    assert!(refused(&both).contains("at most one of `until` or `count`"));
    for one_shot in [
        WakeupAsk {
            count: Some(2),
            ..in_("10m", "x")
        },
        WakeupAsk {
            until: Some(rfc3339(NOW + 60 * MIN)),
            ..in_("10m", "x")
        },
    ] {
        assert!(refused(&one_shot).contains("need `every`"));
    }
}

// SW-AGR-13: `minGap` travels to the terminal server as a duration; bad
// ones are refused here.
#[test]
fn min_gap_is_a_bounded_duration() {
    let gapped = WakeupAsk {
        every: Some("1h".into()),
        min_gap: Some("3h".into()),
        ..in_("1h", "rerun the suite")
    };
    assert_eq!(
        plan(&gapped, NOW, "Barb").unwrap().min_gap_ms,
        Some(180 * MIN)
    );
    assert_eq!(plan(&in_("5m", "x"), NOW, "Barb").unwrap().min_gap_ms, None);
    for bad in ["0s", "30s", "8d", "soon"] {
        let ask = WakeupAsk {
            min_gap: Some(bad.into()),
            ..in_("5m", "x")
        };
        assert!(refused(&ask).contains("`minGap`"), "{bad}");
    }
    let parsed: WakeupAsk =
        serde_json::from_value(serde_json::json!({"in":"5m","prompt":"x","minGap":"2h"})).unwrap();
    assert_eq!(parsed.min_gap.as_deref(), Some("2h"));
}

#[test]
fn when_idle_only_is_removed_and_unknown_fields_are_refused() {
    let parsed: WakeupAsk =
        serde_json::from_value(serde_json::json!({"in":"5m","prompt":"x","whenIdleOnly":false}))
            .unwrap();
    assert!(refused(&parsed).contains("whenIdleOnly was removed"));
    assert!(serde_json::from_value::<WakeupAsk>(
        serde_json::json!({"in":"5m","prompt":"x","evrey":"5m"})
    )
    .is_err());
    let schema = serde_json::to_value(schemars::schema_for!(WakeupAsk)).unwrap();
    let properties = schema["properties"].as_object().unwrap();
    assert!(properties.contains_key("in"));
    assert!(!properties.contains_key("whenIdleOnly"), "not advertised");
}

#[test]
fn prompt_and_name_are_checked_and_the_name_defaults_to_the_prompt() {
    assert!(refused(&in_("5m", "  \n")).contains("`prompt`"));
    assert!(refused(&in_("5m", &"a".repeat(9 * 1024))).contains("`prompt`"));
    let bad_name = WakeupAsk {
        name: Some("bad\u{7}name".into()),
        ..in_("5m", "x")
    };
    assert!(refused(&bad_name).contains("`name`"));
    // The header a fire adds counts against the 8 KiB a delivery carries.
    let header = codesense_pty_core::session_wake::timer_header("x", u32::MAX).len() + 1;
    let named_x = |prompt: String| WakeupAsk {
        name: Some("x".into()),
        ..in_("5m", &prompt)
    };
    assert!(plan(&named_x("a".repeat(MAX_TEXT_BYTES - header)), NOW, "Barb").is_ok());
    assert!(refused(&named_x("a".repeat(MAX_TEXT_BYTES - header + 1))).contains("`prompt`"));
    let named = plan(
        &in_("5m", "  Read your inbox and answer\nanything"),
        NOW,
        "Barb",
    )
    .unwrap();
    assert_eq!(named.name, "Read your inbox and");
    assert_eq!(named.text, "  Read your inbox and answer\nanything");
}

// ───────────────────────────── human gesture guard ─────────────────────────────

/// `Caller::Human(crate::commands::wake::HumanGesture::for_test())` skips the bypass consent and owns any tab, so it is
/// minted only where a person acted: the app window's commands. Nothing an
/// agent reaches (the MCP tools, the dashboard bridge) may produce it.
#[test]
fn only_the_app_windows_commands_act_as_the_person() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut minted = Vec::new();
    let mut stack = vec![src.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if !name.ends_with(".rs") || name == "tests.rs" || name.ends_with("_tests.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            if text.contains("Caller::Human") {
                minted.push(
                    path.strip_prefix(&src)
                        .unwrap()
                        .to_string_lossy()
                        .to_string(),
                );
            }
        }
    }
    minted.sort();
    assert_eq!(
        minted,
        ["commands/wake.rs", "delivery/authority.rs"],
        "only the window's commands construct it; the delivery core only matches it"
    );
}

// ───────────────────────────── review r1 ─────────────────────────────

// SHOULD 2: a plain shell is refused at set time. A native-capable starting
// session may await observation, but its fire can never type without evidence.
#[test]
fn timer_admission_refuses_shells_and_bounds_native_startup_observation() {
    for machine in MACHINES {
        for (session, codex) in [("s-shell", false), ("s-codex", true)] {
            let server = Server::start();
            let desktop = Desktop::new();
            desktop.connect(machine, &server);
            server.unobservable(session, codex);
            let own = SessionRef::new(machine, session);
            let set = desktop.agent_schedules(&server, &own, &in_("2m", "x"));
            if codex {
                assert!(set.is_ok());
                server.after(2 * MIN);
                server.after(2 * MIN);
                assert!(server.writes(session).is_empty());
                assert!(server
                    .wake
                    .outcomes(0)
                    .outcomes
                    .iter()
                    .any(|outcome| outcome.status
                        == DeliveryStatus::Undelivered {
                            reason: UndeliveredReason::Unobservable
                        }));
                continue;
            }
            let refused = set.unwrap_err();
            assert!(
                message(&refused).contains("cannot be observed"),
                "{machine}/{session}: {refused:?}"
            );
            assert!(server.wake.timers(None).is_empty());
        }
    }
}

// SHOULD 1: a one-shot reminder waits through a long turn (beyond the old
// 30-minute wait) and is delivered when the session goes idle.
#[test]
fn a_one_shot_waits_through_a_two_hour_turn() {
    let server = Server::start();
    let desktop = Desktop::new();
    desktop.connect(LOCAL, &server);
    server.idle(SESSIONS[1]);
    server.busy(SESSIONS[1]);
    let own = SessionRef::new(LOCAL, SESSIONS[1]);
    desktop
        .agent_schedules(&server, &own, &in_("1m", "still there?"))
        .unwrap();
    for _ in 0..120 {
        server.after(MIN);
    }
    assert!(server.writes(SESSIONS[1]).is_empty());
    server.finish_turn(SESSIONS[1]);
    server.after(SEC);
    assert_eq!(server.writes(SESSIONS[1]).len(), 2, "delivered at idle");
}

// SHOULD 4: the owner an MCP caller gets. A remote task (no tab) and a local
// headless run are refused as headless; a terminal is bound to its own
// session; a forged id and an anonymous caller are refused.
#[test]
fn the_timer_owner_is_the_authenticated_interactive_session() {
    let remote_tab = TransportIdentity::Reverse {
        machine: REMOTE.into(),
        session: Some("s-tab".into()),
        task: None,
    };
    assert_eq!(
        timer_owner(&remote_tab, "s-tab", |_| None, |_| false),
        Ok(SessionRef::new(REMOTE, "s-tab"))
    );
    assert!(matches!(
        timer_owner(&remote_tab, "s-other", |_| None, |_| false),
        Err(DeliveryError::Authority(AuthorityError::Forged { .. }))
    ));
    let remote_task = TransportIdentity::Reverse {
        machine: REMOTE.into(),
        session: None,
        task: None,
    };
    let task = timer_owner(&remote_task, "s-tab", |_| None, |_| false).unwrap_err();
    assert!(
        message(&task).contains("not supported for headless runs"),
        "{task:?}"
    );
    // An authenticated task is still headless: no timers (SW-AGR-8).
    let named_task = TransportIdentity::Reverse {
        machine: REMOTE.into(),
        session: None,
        task: Some("remote-agent-task-7".into()),
    };
    let refused = timer_owner(&named_task, "remote-agent-task-7", |_| None, |_| false).unwrap_err();
    assert!(message(&refused).contains("not supported for headless runs"));
    let (ours, _theirs) = std::os::unix::net::UnixStream::pair().unwrap();
    let me = codesense_pty_core::activity::peer::of_socket(&ours).unwrap();
    let parent =
        codesense_pty_core::activity::peer::inspect(std::os::unix::process::parent_id()).unwrap();
    let local = TransportIdentity::LocalPeer(me);
    assert_eq!(
        timer_owner(&local, "s-term", |_| Some(parent), |_| false),
        Ok(SessionRef::new(LOCAL, "s-term"))
    );
    let headless = timer_owner(&local, "s-run", |_| Some(parent), |s| s == "s-run").unwrap_err();
    assert!(message(&headless).contains("not supported for headless runs"));
    assert!(matches!(
        timer_owner(&TransportIdentity::Anonymous, "s-term", |_| None, |_| false),
        Err(DeliveryError::Authority(AuthorityError::Unauthenticated))
    ));
}

// LOW: the person reminds only an interactive tab; on another machine, a
// terminal tab this desktop opened there, never a remote task.
#[test]
fn a_person_reminds_only_a_terminal_tab() {
    assert_eq!(require_tab("s-1", LOCAL, false, None), Ok(()));
    assert_eq!(require_tab("s-1", REMOTE, false, Some(REMOTE)), Ok(()));
    for record in [None, Some("another-box")] {
        let refused = require_tab("s-task", REMOTE, false, record).unwrap_err();
        assert!(
            message(&refused).contains("not a terminal tab"),
            "{refused:?}"
        );
    }
    assert!(require_tab("s-1", LOCAL, true, None).is_err());
}

// LOW: "you" is the person's alone; an agent is credited by its tab name.
#[test]
fn agents_are_credited_apart_from_the_person() {
    assert_eq!(agent_author(Some("Barb")), "agent:Barb");
    assert_eq!(agent_author(Some("you")), "agent:you");
    assert_eq!(agent_author(Some(" \u{7} ")), "agent");
    assert_eq!(agent_author(None), "agent");
}

// LOW: an offline machine is named as people know it.
#[test]
fn the_offline_message_names_the_machine_by_its_label() {
    let offline = DeliveryError::Offline {
        machine: "m-7f3a".into(),
    };
    assert!(message(&offline).starts_with("m-7f3a is offline"));
    assert!(message_on(&offline, |_| Some("build-box".into())).starts_with("build-box is offline"));
}
