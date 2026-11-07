// SPDX-License-Identifier: MIT
//! SW-AGR-2 seam tests (SPEC §9.11): a `mode: wake` routine fire over the
//! real coordinator, admission, claim loop, dispatch, run store and delivery
//! core (SW-AGR-9). Only the edges are fakes: the session roster, and a
//! terminal server ([`Sim`]) that keeps an idle-gated queue the way
//! `SessionWakeV1` does and records every byte it would type.
//!
//! R2: every case runs for a saved agent on this Mac and on a connected
//! machine, idle, busy and absent. R1: no fire ever starts a second instance
//! of an agent that is live in its target.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use codesense_pty_core::domain::PtySessionId;
use codesense_pty_core::session_wake::{
    DeliverRequest, DeliveryStatus, GapPolicy, OutcomeLog, TimerInfo, TimerSpec, UndeliveredReason,
    WakeLimits, WakeOutcome,
};

use crate::delivery::authority::{Caller, SessionPosture, SessionRef};
use crate::delivery::events::{SyncHealth, WakeDeliveredEvent, WakeEventSink, WakeOutcomeEvent};
use crate::delivery::host::{HostLookup, HostResolver, WakeHost};
use crate::delivery::run_status::WakeRunStatus;
use crate::delivery::store::RecordDir;
use crate::delivery::{DeliveryError, DeliveryReceipt, DeliveryService, WakeDelivery};
use crate::terminal::session_server::session_wake::WakeCallError;

use super::admit::RoutinePointer;
use super::dispatch::{LaunchFuture, LaunchRequest, Launcher};
use super::finish;
use super::inbox_idle::{IdleMail, REOFFER_MS, WINDOW_MS};
use super::model::{AgentRef, EventSpec, RoutineDef, RoutineTrigger, ValidationError};
use super::run_seams::{key, manual, routine, run, save, store, ticker};
use super::runs::{phase, RoutineRun};
use super::service::{self, Placement, SaveTarget};
use super::test_support::{alice, drain_claim_loop, stub_agent, StubLauncher, TempHome, ORG, TEAM};
use super::wake::{
    self, Activity, Candidate, LaunchFlights, NoticeKind, Pick, RemoteLaunch, Restore,
    RosterFuture, TabFuture, TabOp, TabReply, TargetMachine, WakeEnv, WakeNotice, WakeNotify,
    WakeQuery,
};

const LOCAL: &str = "local";
const REMOTE: &str = "codesense-test-1";
const BARB: &str = "s-barb";

// ───────────────────────────── a terminal server ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SimState {
    Idle,
    Busy,
    NeedsYou,
    /// Its agent quit: the server answers `session_ended`.
    Gone,
}

/// A terminal server's wake queue: a delivery to an idle session is typed at
/// once (and the session is then busy), any other waits for the session to
/// go idle; `needs-you` is never typed into. A repeated idempotency key is
/// answered with what it already came to.
#[derive(Default)]
struct Sim {
    states: Mutex<HashMap<String, SimState>>,
    queue: Mutex<Vec<(DeliverRequest, String)>>,
    typed: Mutex<Vec<(String, String)>>,
    answered: Mutex<HashMap<String, WakeOutcome>>,
    log: Mutex<Vec<WakeOutcome>>,
    ids: AtomicU64,
    /// The next delivery takes effect but its reply is lost.
    lose_reply: AtomicBool,
    /// Every delivery asked of it (SW-AGR-13: what limits it carried).
    requests: Mutex<Vec<DeliverRequest>>,
    /// Decide every delivery with this status and notice, as the server's
    /// wake limits would (SW-AGR-13).
    decide: Mutex<Option<(DeliveryStatus, Option<String>)>>,
    /// A server before wake limits (proto minor 12): a limited wake is
    /// refused by the client before it is sent, as `SessionServerClient`
    /// does.
    before_limits: AtomicBool,
}

impl Sim {
    fn set(&self, session: &str, state: SimState) {
        self.states.lock().unwrap().insert(session.into(), state);
    }

    fn typed(&self) -> Vec<(String, String)> {
        self.typed.lock().unwrap().clone()
    }

    fn queued(&self) -> usize {
        self.queue.lock().unwrap().len()
    }

    fn outcome(&self, req: &DeliverRequest, id: &str, status: DeliveryStatus) -> WakeOutcome {
        let mut log = self.log.lock().unwrap();
        let outcome = WakeOutcome {
            seq: log.len() as u64 + 1,
            delivery_id: id.into(),
            idempotency_key: req.idempotency_key.clone(),
            session: req.session.clone(),
            marker: req.marker.clone(),
            originator: req.originator.clone(),
            timer_id: None,
            status,
            host_boot_id: "boot-1".into(),
            at_ms: 1_000 + log.len() as u64,
            notice: None,
        };
        log.push(outcome.clone());
        self.answered
            .lock()
            .unwrap()
            .insert(req.idempotency_key.clone(), outcome.clone());
        outcome
    }

    fn type_in(&self, req: &DeliverRequest) {
        let body = codesense_pty_core::session_wake::compose(req.header.as_deref(), &req.text)
            .expect("a well-formed wake");
        self.typed
            .lock()
            .unwrap()
            .push((req.session.as_str().to_string(), body));
        // An agent given a prompt works on it.
        self.set(req.session.as_str(), SimState::Busy);
    }

    /// `session` reaches `state`; an idle one gets its oldest queued wake.
    fn go(&self, session: &str, state: SimState) -> Vec<WakeOutcome> {
        self.set(session, state);
        if state != SimState::Idle {
            return Vec::new();
        }
        let next = {
            let mut queue = self.queue.lock().unwrap();
            let at = queue
                .iter()
                .position(|(r, _)| r.session.as_str() == session);
            at.map(|at| queue.remove(at))
        };
        match next {
            Some((req, id)) => {
                self.type_in(&req);
                vec![self.outcome(&req, &id, DeliveryStatus::Delivered { at_ms: 7_000 })]
            }
            None => Vec::new(),
        }
    }
}

impl WakeHost for Sim {
    fn deliver(&self, req: DeliverRequest) -> Result<WakeOutcome, WakeCallError> {
        if let Some(stored) = self.answered.lock().unwrap().get(&req.idempotency_key) {
            return Ok(stored.clone());
        }
        if req.limits.is_some() && self.before_limits.load(Ordering::SeqCst) {
            return Err(WakeCallError::Refused(format!(
                "The terminal server on test-host is {} (max_wakes, max_wakes_per_day, min_gap); update it to wake with limits there.",
                crate::terminal::session_server::session_wake::WAKE_LIMITS_TOO_OLD
            )));
        }
        let id = format!("d-{}", self.ids.fetch_add(1, Ordering::SeqCst));
        self.requests.lock().unwrap().push(req.clone());
        if let Some((status, notice)) = self.decide.lock().unwrap().clone() {
            let mut outcome = self.outcome(&req, &id, status);
            outcome.notice = notice;
            return Ok(outcome);
        }
        // A session the server has not seen settle yet (one just launched) is busy.
        let state = self
            .states
            .lock()
            .unwrap()
            .get(req.session.as_str())
            .copied()
            .unwrap_or(SimState::Busy);
        if state == SimState::Gone {
            return Err(WakeCallError::Refused(format!(
                "session {} is not running",
                req.session.as_str()
            )));
        }
        let outcome = match state {
            SimState::Gone => unreachable!(),
            SimState::Idle => {
                self.type_in(&req);
                self.outcome(&req, &id, DeliveryStatus::Delivered { at_ms: 5_000 })
            }
            SimState::Busy | SimState::NeedsYou => {
                self.queue.lock().unwrap().push((req.clone(), id.clone()));
                let mut outcome = self.outcome(
                    &req,
                    &id,
                    DeliveryStatus::Queued {
                        deadline_ms: 99_000,
                    },
                );
                // A queued answer is not a log entry.
                self.log.lock().unwrap().pop();
                outcome.seq = 0;
                outcome
            }
        };
        if self.lose_reply.swap(false, Ordering::SeqCst) {
            return Err(WakeCallError::Unknown("the reply was lost".into()));
        }
        Ok(outcome)
    }
    fn cancel(&self, session: &PtySessionId, key: &str) -> Result<WakeOutcome, WakeCallError> {
        let found = {
            let mut queue = self.queue.lock().unwrap();
            let at = queue
                .iter()
                .position(|(r, _)| r.session == *session && r.idempotency_key == key);
            at.map(|at| queue.remove(at))
        };
        match found {
            Some((req, id)) => Ok(self.outcome(
                &req,
                &id,
                DeliveryStatus::Undelivered {
                    reason: UndeliveredReason::Archived,
                },
            )),
            // As the session server: a delivery no longer queued is answered as it
            // stands (typed, unknown, …).
            None => match self.answered.lock().unwrap().get(key) {
                Some(stored) => Ok(stored.clone()),
                None => Err(WakeCallError::Refused("nothing queued".into())),
            },
        }
    }
    fn timer_set(&self, _: TimerSpec) -> Result<TimerInfo, WakeCallError> {
        Err(WakeCallError::Refused("no timers here".into()))
    }
    fn timer_list(&self, _: Option<&PtySessionId>) -> Result<Vec<TimerInfo>, WakeCallError> {
        Ok(Vec::new())
    }
    fn timer_cancel(&self, _: &PtySessionId, _: &str) -> Result<bool, WakeCallError> {
        Ok(false)
    }
    fn outcomes(&self, since: u64) -> Result<OutcomeLog, WakeCallError> {
        let log = self.log.lock().unwrap();
        Ok(OutcomeLog {
            host_boot_id: "boot-1".into(),
            outcomes: log.iter().filter(|o| o.seq > since).cloned().collect(),
            overflow: false,
            last_seq: log.len() as u64,
        })
    }
}

#[derive(Default)]
struct Machines {
    ready: Mutex<HashMap<String, Arc<dyn WakeHost>>>,
    offline: Mutex<Vec<String>>,
}

impl HostResolver for Machines {
    fn resolve(&self, machine: &str) -> HostLookup {
        if self.offline.lock().unwrap().iter().any(|m| m == machine) {
            return HostLookup::Offline {
                detail: "unreachable".into(),
            };
        }
        match self.ready.lock().unwrap().get(machine) {
            Some(sim) => HostLookup::Ready(sim.clone()),
            None => HostLookup::Unknown,
        }
    }
}

/// The app's sink: events go nowhere; inbox notes go to the environment's
/// inbox, as the app's does (SW-AGR-13).
#[derive(Default)]
struct Quiet(std::sync::OnceLock<Arc<Env>>);
impl WakeEventSink for Quiet {
    fn delivered(&self, _: &WakeDeliveredEvent) {}
    fn outcome(&self, _: &WakeOutcomeEvent) {}
    fn sync_health(&self, _: &SyncHealth) {}
    fn notice(&self, event: &crate::delivery::events::WakeNoticeEvent) {
        if let Some(env) = self.0.get() {
            env.post(&wake::delivery_notice(event));
        }
    }
}

// ───────────────────────────── the app around it ─────────────────────────────

/// Something that happens while the machine is asked.
type RemoteHook = Box<dyn Fn(&RemoteLaunch) + Send + Sync>;

struct Env {
    service: Arc<DeliveryService>,
    machines: Arc<Machines>,
    roster: Mutex<Vec<Candidate>>,
    unreadable: Mutex<Vec<String>>,
    ignorable: Mutex<Vec<String>>,
    restore: Mutex<Restore>,
    known: Mutex<Vec<String>>,
    /// Every remote launch call: what was asked, of which launch.
    remote_calls: Mutex<Vec<(TabOp, RemoteLaunch)>>,
    /// What the machine answers next, in order; then [`Env::remote_reply`].
    remote_script: Mutex<std::collections::VecDeque<TabReply>>,
    remote_reply: Mutex<TabReply>,
    /// How long the machine takes to answer, in ms (0: at once).
    remote_delay_ms: AtomicU64,
    /// Runs while the machine is asked (something happening meanwhile).
    on_remote_call: Mutex<Option<RemoteHook>>,
    /// Remote stops the launcher sent: `(run, machine, session)`.
    killed_remote: Mutex<Vec<(String, String, String)>>,
    posts: Mutex<Vec<WakeNotice>>,
    redispatched: Mutex<Vec<RoutinePointer>>,
    cancels: AtomicU64,
    flights: LaunchFlights,
    /// The roster read yields once before it answers, as the app's (an async
    /// layout read) does, so same-tick fires interleave there (SW-AGR-7 S6).
    roster_yields: AtomicBool,
}

impl WakeNotify for Env {
    fn post(&self, notice: &WakeNotice) {
        self.posts.lock().unwrap().push(notice.clone());
    }
    fn redispatch(&self, pointer: RoutinePointer, _after: std::time::Duration) {
        self.redispatched.lock().unwrap().push(pointer);
    }
}

impl WakeEnv for Env {
    fn restore(&self) -> Restore {
        *self.restore.lock().unwrap()
    }
    fn roster(&self) -> RosterFuture<'_> {
        let roster = wake::Roster {
            candidates: self.roster.lock().unwrap().clone(),
            unreadable: self.unreadable.lock().unwrap().clone(),
            ignorable: self.ignorable.lock().unwrap().clone(),
        };
        let yields = self.roster_yields.load(Ordering::SeqCst);
        Box::pin(async move {
            if yields {
                tokio::task::yield_now().await;
            }
            Ok(roster)
        })
    }
    fn machine_known(&self, machine: &str) -> bool {
        self.known.lock().unwrap().iter().any(|m| m == machine)
    }
    fn posture(&self, _: &Candidate, _: &crate::domain::Principal) -> SessionPosture {
        SessionPosture::default()
    }
    fn deliver(&self, delivery: WakeDelivery) -> Result<DeliveryReceipt, DeliveryError> {
        self.service.deliver(&Caller::RoutineHost, delivery)
    }
    fn cancel(&self, target: &SessionRef, key: &str) -> Result<WakeRunStatus, DeliveryError> {
        self.cancels.fetch_add(1, Ordering::SeqCst);
        self.service.cancel(&Caller::RoutineHost, target, key)
    }
    fn remote_tab<'a>(&'a self, launch: &'a RemoteLaunch, op: TabOp) -> TabFuture<'a> {
        self.remote_calls.lock().unwrap().push((op, launch.clone()));
        if let Some(hook) = self.on_remote_call.lock().unwrap().as_ref() {
            hook(launch);
        }
        let reply = self
            .remote_script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| self.remote_reply.lock().unwrap().clone());
        let delay = self.remote_delay_ms.load(Ordering::SeqCst);
        Box::pin(async move {
            if delay > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
            reply
        })
    }
    fn flights(&self) -> &LaunchFlights {
        &self.flights
    }
}

/// The stub agent launcher, with wake delivery through `env`.
struct WakeLauncher {
    stub: Arc<StubLauncher>,
    env: Arc<Env>,
}

impl Launcher for WakeLauncher {
    fn launch(&self, request: LaunchRequest) -> LaunchFuture<'_> {
        self.stub.launch(request)
    }
    fn kill_run(&self, run_id: &str) {
        Launcher::kill_run(self.stub.as_ref(), run_id)
    }
    fn wake_env(&self) -> Option<Arc<dyn WakeEnv>> {
        Some(self.env.clone())
    }
    fn kill_remote(&self, run_id: &str, machine: &str, session: &str) {
        self.env.killed_remote.lock().unwrap().push((
            run_id.to_string(),
            machine.to_string(),
            session.to_string(),
        ));
    }
}

struct World {
    temp: TempHome,
    stub: Arc<StubLauncher>,
    env: Arc<Env>,
    local: Arc<Sim>,
    remote: Arc<Sim>,
}

impl World {
    fn new() -> Self {
        let temp = TempHome::new();
        let home = temp.codesense_home();
        let machines = Arc::new(Machines::default());
        let (local, remote) = (Arc::new(Sim::default()), Arc::new(Sim::default()));
        machines
            .ready
            .lock()
            .unwrap()
            .insert(LOCAL.into(), local.clone());
        machines
            .ready
            .lock()
            .unwrap()
            .insert(REMOTE.into(), remote.clone());
        let sink = Arc::new(Quiet::default());
        let service = Arc::new(DeliveryService::new(
            machines.clone(),
            sink.clone(),
            RecordDir::at(None),
            "this-mac".into(),
        ));
        // The app's run recorder, as `delivery::app` installs it: outcomes
        // routed later (a push, a catch-up), told through home's environment.
        let record_home = home.clone();
        service.subscribe(Arc::new(move |outcome| {
            let store = super::state::RoutineStateStore::open(&record_home).unwrap();
            match wake::env_for(&record_home) {
                Some(env) => wake::record_routed(&store, outcome, env.as_ref()),
                None => wake::record_routed(&store, outcome, &wake::NoNotify),
            }
        }));
        let env = Arc::new(Env {
            service,
            machines,
            roster: Mutex::new(Vec::new()),
            unreadable: Mutex::new(Vec::new()),
            ignorable: Mutex::new(Vec::new()),
            restore: Mutex::new(Restore::Settled),
            known: Mutex::new(vec![REMOTE.into()]),
            remote_calls: Mutex::new(Vec::new()),
            remote_script: Mutex::new(Default::default()),
            remote_reply: Mutex::new(TabReply::Opened {
                session: "r-new".into(),
                handle: "r-handle".into(),
            }),
            remote_delay_ms: AtomicU64::new(0),
            on_remote_call: Mutex::new(None),
            killed_remote: Mutex::new(Vec::new()),
            posts: Mutex::new(Vec::new()),
            redispatched: Mutex::new(Vec::new()),
            cancels: AtomicU64::new(0),
            flights: LaunchFlights::default(),
            roster_yields: AtomicBool::new(false),
        });
        let _ = sink.0.set(env.clone());
        wake::install(&home, env.clone());
        let stub = Arc::new(StubLauncher::new(stub_agent(temp.root())));
        Self {
            temp,
            stub,
            env,
            local,
            remote,
        }
    }

    fn home(&self) -> PathBuf {
        self.temp.codesense_home()
    }

    fn sim(&self, machine: &str) -> &Arc<Sim> {
        if machine == LOCAL {
            &self.local
        } else {
            &self.remote
        }
    }

    fn launcher(&self) -> Arc<dyn Launcher> {
        Arc::new(WakeLauncher {
            stub: self.stub.clone(),
            env: self.env.clone(),
        })
    }

    async fn drain(&self) -> usize {
        drain_claim_loop(&self.home(), self.launcher()).await
    }

    /// The wakes handed back to be dispatched again, taken.
    fn take_redispatched(&self) -> Vec<RoutinePointer> {
        std::mem::take(&mut *self.env.redispatched.lock().unwrap())
    }

    /// Dispatch again what was handed back, as the app's task does.
    async fn redrive(&self) -> usize {
        let pointers = self.take_redispatched();
        let n = pointers.len();
        for pointer in pointers {
            super::dispatch::execute(self.home(), pointer, self.launcher().as_ref())
                .await
                .unwrap();
        }
        n
    }

    /// Barb's live session on `machine`, doing `state`.
    fn barb(&self, machine: &str, session: &str, state: SimState) {
        self.sim(machine).set(session, state);
        self.env.roster.lock().unwrap().push(candidate(
            machine,
            session,
            &barb_key(),
            match state {
                SimState::Idle => Activity::Idle,
                SimState::Busy => Activity::Busy,
                SimState::NeedsYou => Activity::NeedsYou,
                SimState::Gone => Activity::Idle,
            },
        ));
    }

    /// The terminal server on `machine` saw `session` reach `state`; its
    /// pushes reach the desktop.
    fn go(&self, machine: &str, session: &str, state: SimState) {
        for outcome in self.sim(machine).go(session, state) {
            self.env.service.on_pushed(machine, outcome);
        }
    }

    fn typed(&self) -> Vec<(String, String)> {
        let mut all = self.local.typed();
        all.extend(self.remote.typed());
        all
    }

    fn posts(&self) -> Vec<WakeNotice> {
        self.env.posts.lock().unwrap().clone()
    }

    /// Every remote launch call, `(op, request id, machine, prompt)`.
    fn remote_calls(&self) -> Vec<(TabOp, uuid::Uuid, String, String)> {
        self.env
            .remote_calls
            .lock()
            .unwrap()
            .iter()
            .map(|(op, l)| (*op, l.request_id, l.machine.clone(), l.prompt.clone()))
            .collect()
    }

    fn creates(&self) -> usize {
        self.remote_calls()
            .iter()
            .filter(|(op, ..)| *op == TabOp::Create)
            .count()
    }

    /// The machine answers these next.
    fn script(&self, replies: impl IntoIterator<Item = TabReply>) {
        self.env.remote_script.lock().unwrap().extend(replies);
    }
}

fn not_prepared(detail: &str) -> TabReply {
    TabReply::NotStarted {
        reason: UndeliveredReason::NeedsPreparation,
        detail: detail.into(),
    }
}

/// The test team's saved agent, by key.
fn barb_key() -> String {
    AgentRef::from(super::test_support::AGENT)
        .key_for(&key("any"))
        .unwrap()
        .to_string()
}

fn candidate(machine: &str, session: &str, agent: &str, activity: Activity) -> Candidate {
    Candidate {
        machine: machine.into(),
        session: session.into(),
        handle: format!("h-{session}"),
        agent: Some(agent.into()),
        routine_key: Some(key("triage").to_string()),
        activity,
        headless: false,
        last_active_ms: 1,
        request_id: None,
    }
}

/// A visible routine that wakes the team agent on `machine`.
fn wake_routine(id: &str, machine: &str) -> RoutineDef {
    let mut def = routine(id, "0 9 * * *");
    def.mode = "wake".into();
    def.policy.visible = true;
    def.task = "triage new issues for {{date}}; last time: {{last_run_summary}}".into();
    def.target.machine = Some(machine.into());
    def
}

fn runs_of(home: &Path, id: &str) -> Vec<RoutineRun> {
    store(home).runs(&key(id)).unwrap()
}

fn only_run(home: &Path, id: &str) -> RoutineRun {
    let runs = runs_of(home, id);
    assert_eq!(runs.len(), 1, "{runs:?}");
    runs.into_iter().next().unwrap()
}

fn assert_typed_once(typed: &[(String, String)], session: &str, routine_name: &str) {
    assert_eq!(typed.len(), 1, "typed exactly once: {typed:?}");
    let (into, body) = &typed[0];
    assert_eq!(into, session);
    let header = body.lines().next().unwrap();
    assert!(
        header.starts_with(&format!("[routine: {routine_name} | run ")),
        "the run header comes first: {body:?}"
    );
    assert!(
        body.contains("triage new issues for 20"),
        "date filled in: {body:?}"
    );
    assert!(
        body.contains("{{last_run_summary}}"),
        "a dynamic value is never typed into a live session (§9.5): {body:?}"
    );
}

// CX3: the routine's real delivery core reaches the real host queue. Only
// PTY I/O is fake: qualified observation and Submitted drive ActivityOwner.
#[tokio::test]
async fn cx3_wake_routine_into_a_starting_codex_local_and_remote() {
    use crate::delivery::wakeup::tests::{Link, Server};
    use codesense_pty_core::agent_cli::AgentCli;
    for machine in [LOCAL, REMOTE] {
        let w = World::new();
        let agent = super::test_support::save_team_agent(
            &w.home(),
            &alice(),
            super::test_support::agent("cx3-codex", AgentCli::Codex),
        );
        let server = Server::start();
        server.starting("fresh-codex", AgentCli::Codex);
        w.env
            .machines
            .ready
            .lock()
            .unwrap()
            .insert(machine.into(), Arc::new(Link(server.clone())));
        w.env.roster.lock().unwrap().push(candidate(
            machine,
            "fresh-codex",
            &agent.to_string(),
            Activity::Idle,
        ));
        w.env.roster.lock().unwrap()[0].routine_key = Some(key("cx3-wake").to_string());
        let mut def = wake_routine("cx3-wake", machine);
        def.agent = AgentRef::from("cx3-codex");
        save(&w.home(), def);
        let fired = manual(&w.home(), "cx3-wake");
        w.drain().await;
        assert_eq!(run(&w.home(), &fired.run_id).phase, phase::QUEUED);
        assert!(server.writes("fresh-codex").is_empty());
        assert_eq!(w.stub.calls(), 0);
        server.observe("fresh-codex");
        server.after(1_000);
        w.env.service.sync(machine).unwrap();
        let finished = run(&w.home(), &fired.run_id);
        assert_eq!(finished.phase, phase::DELIVERED, "{machine}");
        assert!(finished.delivered_at.is_some());
        assert_eq!(
            server.generation("fresh-codex"),
            1,
            "real turn-start evidence"
        );
        let writes = server.writes("fresh-codex");
        assert_eq!(writes.len(), 2);
        assert!(writes[0].starts_with("\x1b[200~[routine:"));
        assert_eq!(writes[1], "\r");
    }
}

// ───────────────────────────── R2: {local, remote} × idle ─────────────────────────────

async fn idle_delivers_once(machine: &str) {
    let w = World::new();
    w.barb(machine, BARB, SimState::Idle);
    save(&w.home(), wake_routine("triage", machine));
    manual(&w.home(), "triage");
    w.drain().await;

    assert_eq!(w.stub.calls(), 0, "no new session");
    assert_typed_once(&w.typed(), BARB, "Routine triage");
    let run = only_run(&w.home(), "triage");
    assert_eq!(
        run.phase,
        phase::DELIVERED,
        "§4.1.5: no result schema, so it is done once typed in"
    );
    assert!(run.delivered_at.is_some());
    assert_eq!(run.session_id.as_deref(), Some(BARB));
    assert_eq!(run.wake_machine.as_deref(), Some(machine));
    // A wake into a running session has no launch, yet the run is a task: made at the routine's
    // start, linked to the session it woke, and closed when the delivery ended the run.
    let usage = crate::session_usage::UsageStore::open(&w.home().join("usage.sqlite")).unwrap();
    let task = usage.read_task(&run.run_id).unwrap().expect("the wake run's task");
    assert_eq!(
        (task.task_type.as_str(), task.status.as_str(), task.raw_status.as_deref()),
        ("routine", "done", Some("delivered"))
    );
    assert!(task.goal.starts_with("Routine triage"), "{}", task.goal);
    assert!(usage
        .task_links(&run.run_id)
        .unwrap()
        .iter()
        .any(|l| l.kind == "session" && l.target == BARB));
}

#[tokio::test]
async fn r2_local_idle_is_woken_once_and_nothing_launches() {
    idle_delivers_once(LOCAL).await;
}

#[tokio::test]
async fn r2_remote_idle_is_woken_once_and_nothing_launches() {
    idle_delivers_once(REMOTE).await;
}

// ───────────────────────────── R2: {local, remote} × busy ─────────────────────────────

async fn busy_queues_then_delivers(machine: &str) {
    let w = World::new();
    w.barb(machine, BARB, SimState::Busy);
    save(&w.home(), wake_routine("triage", machine));
    manual(&w.home(), "triage");
    w.drain().await;

    assert_eq!(w.stub.calls(), 0);
    assert!(w.typed().is_empty(), "never typed while busy");
    let queued = only_run(&w.home(), "triage");
    assert_eq!(queued.phase, phase::QUEUED);
    assert_eq!(queued.session_id.as_deref(), Some(BARB));
    let read_model = w.env.service.queued_for_routine(&key("triage").to_string());
    assert_eq!(
        read_model.map(|q| (q.machine_id, q.session_id)),
        Some((machine.to_string(), BARB.to_string())),
        "agent_get.routines[].queued shows it"
    );

    w.go(machine, BARB, SimState::Idle);
    assert_typed_once(&w.typed(), BARB, "Routine triage");
    let run = only_run(&w.home(), "triage");
    assert_eq!(run.phase, phase::DELIVERED);
    assert!(run.delivered_at.is_some());
    assert!(w
        .env
        .service
        .queued_for_routine(&key("triage").to_string())
        .is_none());
}

#[tokio::test]
async fn r2_local_busy_queues_until_idle() {
    busy_queues_then_delivers(LOCAL).await;
}

#[tokio::test]
async fn r2_remote_busy_queues_until_idle() {
    busy_queues_then_delivers(REMOTE).await;
}

// ───────────────────────────── R2: {local, remote} × absent ─────────────────────────────

#[tokio::test]
async fn r2_local_absent_launches_one_with_the_task_as_its_first_prompt() {
    let w = World::new();
    save(&w.home(), wake_routine("triage", LOCAL));
    manual(&w.home(), "triage");
    w.drain().await;

    assert_eq!(w.stub.calls(), 1, "exactly one Barb");
    let launched = w.stub.last();
    assert!(launched.request.visible, "a wake always lands in a tab");
    assert!(
        launched
            .request
            .prompt
            .starts_with("[routine: Routine triage | run "),
        "{}",
        launched.request.prompt
    );
    assert!(launched.request.prompt.contains("triage new issues for 20"));
    assert!(
        w.typed().is_empty(),
        "launched with it, never launched-then-typed"
    );
    let run = only_run(&w.home(), "triage");
    assert_eq!(
        run.phase,
        phase::DELIVERED,
        "its agent started with the task: delivered (no result schema)"
    );
    assert!(run.delivered_at.is_some());
    assert_eq!(
        run.session_id.as_deref(),
        Some(launched.session_id.as_str())
    );
    assert_eq!(
        run.wake_machine.as_deref(),
        Some(LOCAL),
        "the Barb a wake starts is Barb's own, never the run's to kill"
    );
}

#[tokio::test]
async fn reassigning_a_wake_routine_preserves_the_previous_agents_live_workspace() {
    let w = World::new();
    save(&w.home(), wake_routine("triage", LOCAL));
    manual(&w.home(), "triage");
    w.drain().await;
    let a = w.stub.last();
    std::fs::write(
        a.request.cwd.join("active-work.txt"),
        "agent A is still working",
    )
    .unwrap();
    w.local.set(&a.session_id, SimState::Busy);
    let mut live = candidate(LOCAL, &a.session_id, &barb_key(), Activity::Busy);
    live.routine_key = Some(key("triage").to_string());
    w.env.roster.lock().unwrap().push(live);

    let replacement = super::test_support::save_team_agent(
        &w.home(),
        &alice(),
        super::test_support::agent(
            "replacement",
            codesense_pty_core::agent_cli::AgentCli::Claude,
        ),
    );
    let mut def = service::get_current(&w.home(), &alice(), &key("triage"))
        .unwrap()
        .def;
    let revision = def.revision;
    def.agent = AgentRef::from("replacement");
    service::save(
        &w.home(),
        &alice(),
        SaveTarget::Update {
            key: key("triage"),
            base_revision: revision,
        },
        def,
        &super::model::Author::user("alice"),
    )
    .unwrap();
    manual(&w.home(), "triage");
    w.drain().await;
    let b = w.stub.last();
    assert_eq!(w.stub.calls(), 2);
    assert_eq!(b.request.agent.manifest.agent.key, replacement);
    assert!(
        w.stub.is_running(&a.session_id),
        "the previous agent remains alive"
    );
    assert_eq!(
        std::fs::read_to_string(a.request.cwd.join("active-work.txt"))
            .expect("reassignment must not erase the previous agent's work"),
        "agent A is still working",
    );
    assert_ne!(a.request.cwd, b.request.cwd);
    assert!(!b.request.cwd.join("active-work.txt").exists());
}

#[tokio::test]
async fn local_wakes_do_not_share_recent_launches_before_roster_refresh() {
    let w = World::new();
    for id in ["wake-a", "wake-b"] {
        save(&w.home(), wake_routine(id, LOCAL));
        manual(&w.home(), id);
        w.drain().await;
    }
    assert!(
        w.env.roster.lock().unwrap().is_empty(),
        "neither launch is visible yet"
    );
    assert_eq!(
        w.stub.calls(),
        2,
        "each local routine must launch its own session"
    );
    assert!(w.typed().is_empty());
    assert_eq!(
        w.local.queued(),
        0,
        "B must not queue into A's recent launch"
    );
    let launches = w.stub.launched();
    assert_ne!(launches[0].request.cwd, launches[1].request.cwd);
}

#[tokio::test]
async fn wake_cannot_enter_another_routines_fresh_workspace() {
    let w = World::new();
    let mut fresh = routine("fresh-a", "0 9 * * *");
    fresh.mode = "fresh".into();
    fresh.policy.visible = true;
    save(&w.home(), fresh);
    save(&w.home(), wake_routine("wake-b", LOCAL));

    manual(&w.home(), "fresh-a");
    w.drain().await;
    let a = w.stub.last();
    w.stub
        .signal_done(&a.session_id, "success", "finished")
        .await;
    w.local.set(&a.session_id, SimState::Idle);
    let mut live = candidate(LOCAL, &a.session_id, &barb_key(), Activity::Idle);
    live.routine_key = Some(key("fresh-a").to_string());
    w.env.roster.lock().unwrap().push(live);

    let b_run = manual(&w.home(), "wake-b").run_id;
    w.drain().await;
    let b_run = run(&w.home(), &b_run);
    let b_cwd = if b_run.session_id.as_deref() == Some(a.session_id.as_str()) {
        a.request.cwd.clone()
    } else {
        w.stub.last().request.cwd.clone()
    };
    std::fs::write(b_cwd.join("wake-work.txt"), "still working").unwrap();

    manual(&w.home(), "fresh-a");
    w.drain().await;
    assert!(
        b_cwd.join("wake-work.txt").exists(),
        "fresh A erased live wake B's files"
    );
    assert_ne!(b_cwd, a.request.cwd, "wake B must own a separate workspace");
    assert_eq!(w.stub.calls(), 3);
    assert!(w.typed().is_empty(), "B must not deliver into A's session");
}

#[tokio::test]
async fn wake_prepares_context_when_absent_then_reuses_the_actual_workspace() {
    let w = World::new();
    let org = crate::context_repo::path_to_org_root(&w.home(), &alice(), ORG).unwrap();
    let view = crate::context_repo::inheritance::team_view(&org, ORG, TEAM);
    let content = view.own_content().own_path().unwrap().to_path_buf();
    let skill = content.join("skills/judge/SKILL.md");
    std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
    std::fs::write(
        &skill,
        "---\nname: judge\ndescription: Judge agents\n---\nJudge\n",
    )
    .unwrap();
    let mut agent =
        super::test_support::agent("judge", codesense_pty_core::agent_cli::AgentCli::Claude);
    agent.context = Some(vec![serde_yaml::from_str(
        "path: skills/judge\ndest: .claude/skills/judge\n",
    )
    .unwrap()]);
    super::test_support::save_team_agent(&w.home(), &alice(), agent);
    let mut def = wake_routine("triage", LOCAL);
    def.agent = AgentRef::from("judge");
    save(&w.home(), def);
    manual(&w.home(), "triage");
    w.drain().await;
    let launched = w.stub.last();
    let cwd = &launched.request.cwd;
    assert!(!cwd.starts_with(&content));
    assert_eq!(
        std::fs::read(cwd.join(".claude/skills/judge/SKILL.md")).unwrap(),
        std::fs::read(skill).unwrap()
    );
    std::fs::write(cwd.join("keep-while-alive.txt"), "working").unwrap();
    w.local.set(&launched.session_id, SimState::Idle);
    let patch = w
        .stub
        .registry
        .launch_tags_for_session(&launched.session_id)
        .unwrap();
    assert_eq!(
        patch["launchRoutineKey"].as_deref(),
        Some(key("triage").to_string().as_str())
    );
    let mut live = tab(&launched.session_id, &[], false);
    live.tags = patch
        .into_iter()
        .filter_map(|(key, value)| value.map(|value| (key, value)))
        .collect();
    *w.env.roster.lock().unwrap() = super::wake_app::roster_from(
        &[super::wake_app::HostTabs {
            machine: LOCAL.into(),
            online: true,
            tabs: vec![live],
        }],
        &|_, _| Some(true),
        &|_| Some(Activity::Idle),
        &|_| None,
        &super::wake_app::RosterRules::immediate(),
    );
    manual(&w.home(), "triage");
    w.drain().await;
    assert_eq!(
        w.stub.calls(),
        1,
        "the same wake routine must reuse its live agent after roster refresh"
    );
    assert!(
        cwd.join("keep-while-alive.txt").exists(),
        "delivery must not reset a live workspace"
    );
    assert_eq!(w.typed().len(), 1);
    assert!(!content.join(".claude/skills/judge/SKILL.md").exists());
}

// ───────────────────────────── SW-AGR-2b review r1 ─────────────────────────────

/// A ticker over `w`'s home whose clock started a second ago, with `sessions`.
fn maintenance(
    w: &World,
    sessions: Arc<super::test_support::FakeSessions>,
) -> (super::tick::RoutineTicker, Arc<AtomicI64>) {
    let clock = Arc::new(AtomicI64::new(finish::now_ms() - 1_000));
    let ticker = ticker(&w.home(), &clock, sessions);
    (ticker, clock)
}

// MUST 1: a run whose agent runs on a machine is asked of that machine.
// Maintenance on this Mac (which knows nothing of it) never ends it; an
// unreachable machine is unknown, never ended; once the machine says it is
// gone, it ends; past max_runtime its kill goes to that machine.
#[tokio::test]
async fn maintenance_asks_a_remote_runs_machine_and_never_ends_it_for_this_mac() {
    use crate::commands::pty_backend::Liveness;
    let w = World::new();
    let mut fresh = wake_routine("nightly", REMOTE);
    fresh.mode = "fresh".into();
    save(&w.home(), fresh);
    save(&w.home(), with_result(wake_routine("triage", REMOTE)));
    let nightly = manual(&w.home(), "nightly");
    w.drain().await;
    *w.env.remote_reply.lock().unwrap() = TabReply::Opened {
        session: "r-triage".into(),
        handle: "rh-triage".into(),
    };
    let triage = manual(&w.home(), "triage");
    w.drain().await;
    let sessions = Arc::new(super::test_support::FakeSessions::default());
    let (ticker, clock) = maintenance(&w, sessions.clone());
    clock.store(finish::now_ms(), Ordering::SeqCst);

    // This Mac does not know these sessions (its own liveness says gone);
    // the machine cannot be reached: Unknown.
    ticker.tick();
    let phases = |w: &World| {
        (
            run(&w.home(), &nightly.run_id).phase,
            run(&w.home(), &triage.run_id).phase,
        )
    };
    assert_eq!(phases(&w), (phase::RUNNING.into(), phase::RUNNING.into()));
    let triage_run = run(&w.home(), &triage.run_id);
    assert!(
        triage_run.delivered_at.is_some(),
        "delivered, kept open for its result"
    );
    assert_eq!(triage_run.launch_machine.as_deref(), Some(REMOTE));
    assert_eq!(
        run(&w.home(), &nightly.run_id).launch_machine.as_deref(),
        Some(REMOTE)
    );
    assert!(
        finish::reconcile_runs(&store(&w.home()), finish::now_ms(), sessions.as_ref(), None)
            .unwrap()
            .interrupted
            .is_empty()
    );

    // The machine says both run.
    for session in ["r-new", "r-triage"] {
        sessions
            .remote
            .lock()
            .unwrap()
            .insert(format!("{REMOTE}/{session}"), Liveness::Known(true));
    }
    ticker.tick();
    assert_eq!(phases(&w), (phase::RUNNING.into(), phase::RUNNING.into()));

    // The fresh run's agent ended there: its run ends.
    sessions
        .remote
        .lock()
        .unwrap()
        .insert(format!("{REMOTE}/r-new"), Liveness::Known(false));
    ticker.tick();
    assert_eq!(phases(&w).0, phase::ENDED);
    assert_eq!(phases(&w).1, phase::RUNNING);

    // Past max_runtime: timed out; a launched session would be killed on its
    // machine, a woken one (the wake's) never.
    let mut long = wake_routine("long", REMOTE);
    long.mode = "fresh".into();
    save(&w.home(), long);
    *w.env.remote_reply.lock().unwrap() = TabReply::Opened {
        session: "r-long".into(),
        handle: "rh-long".into(),
    };
    let long = manual(&w.home(), "long");
    w.drain().await;
    sessions
        .remote
        .lock()
        .unwrap()
        .insert(format!("{REMOTE}/r-long"), Liveness::Known(true));
    clock.store(finish::now_ms() + 31 * 60_000, Ordering::SeqCst);
    ticker.tick();
    assert_eq!(run(&w.home(), &long.run_id).phase, phase::TIMED_OUT);
    assert_eq!(run(&w.home(), &triage.run_id).phase, phase::TIMED_OUT);
    let killed = sessions.killed_on.lock().unwrap().clone();
    assert_eq!(
        killed,
        vec![(
            long.run_id.clone(),
            REMOTE.to_string(),
            Some("r-long".to_string())
        )],
        "the fresh run's agent is stopped on its machine; the woken one is not"
    );
    assert!(
        sessions.killed.lock().unwrap().is_empty(),
        "nothing killed on this Mac"
    );
}

#[test]
fn a_machines_liveness_answer_is_known_only_when_it_answered() {
    use crate::commands::pty_backend::Liveness;
    let alive: std::collections::HashSet<PtySessionId> = [PtySessionId::new("s")].into();
    assert_eq!(
        finish::remote_liveness(Ok(alive), "s"),
        Liveness::Known(true)
    );
    assert_eq!(
        finish::remote_liveness(Ok(Default::default()), "s"),
        Liveness::Known(false)
    );
    assert_eq!(
        finish::remote_liveness(Err("not connected".into()), "s"),
        Liveness::Unknown,
        "an unreachable machine never ends a run"
    );
}

// MUST 2: on a machine, a launch's single flight is (agent, machine): two
// routines with different targets firing together start one Barb.
#[tokio::test]
async fn two_remote_routines_with_different_targets_firing_together_create_once() {
    let w = World::new();
    w.env.remote_delay_ms.store(50, Ordering::SeqCst);
    let mut repo = wake_routine("repo-a", REMOTE);
    repo.target.kind = "repo:acme/a".into();
    save(&w.home(), repo);
    save(&w.home(), wake_routine("team-ws", REMOTE));
    let a = pointer_of(&w.home(), &manual(&w.home(), "repo-a").run_id);
    let b = pointer_of(&w.home(), &manual(&w.home(), "team-ws").run_id);
    let (la, lb) = (w.launcher(), w.launcher());
    let (ra, rb) = tokio::join!(
        super::dispatch::execute(w.home(), a, la.as_ref()),
        super::dispatch::execute(w.home(), b, lb.as_ref()),
    );
    ra.unwrap();
    rb.unwrap();
    for _ in 0..3 {
        w.redrive().await;
    }
    assert_eq!(w.creates(), 1, "one Barb: {:?}", w.remote_calls());
    for id in ["repo-a", "team-ws"] {
        assert_eq!(
            only_run(&w.home(), id).session_id.as_deref(),
            Some("r-new"),
            "{id}"
        );
    }
}

/// Fire `id` once with the machine answering nothing (Create, then Attach,
/// both unknown): its run is `unknown`, its launch held.
async fn held_launch(w: &World, id: &str) -> String {
    let lost = TabReply::Unknown("lost".into());
    w.script([lost.clone(), lost]);
    save(&w.home(), wake_routine(id, REMOTE));
    let fired = manual(&w.home(), id);
    w.drain().await;
    assert_eq!(only_run(&w.home(), id).phase, phase::UNKNOWN);
    fired.run_id
}

// MUST 3: a held launch whose tab shows in the machine's layout (its
// remoteRequestId) is settled: the wake goes to that tab, nothing is asked.
#[tokio::test]
async fn a_held_launch_whose_tab_shows_is_settled_and_woken() {
    let w = World::new();
    let first = held_launch(&w, "triage").await;
    let mut tab = candidate(REMOTE, "r-shown", &barb_key(), Activity::Idle);
    tab.request_id = Some(wake::launch_request_id(&first).to_string());
    w.remote.set("r-shown", SimState::Idle);
    w.env.roster.lock().unwrap().push(tab);
    let calls = w.remote_calls().len();
    manual(&w.home(), "triage");
    w.drain().await;
    assert_eq!(
        w.remote_calls().len(),
        calls,
        "nothing asked of the machine"
    );
    assert_typed_once(&w.typed(), "r-shown", "Routine triage");
    // And it stays settled: with Barb gone, the next fire Creates afresh.
    w.env.roster.lock().unwrap().clear();
    let third = manual(&w.home(), "triage");
    w.drain().await;
    let last = w.remote_calls().last().cloned().unwrap();
    assert_eq!(
        (last.0, last.1),
        (TabOp::Create, wake::launch_request_id(&third.run_id))
    );
}

// MUST 3: an Attach that finds the held launch's tab ended starts Barb in
// the same fire, under this run's own request id.
#[tokio::test]
async fn an_attach_finding_the_held_tab_ended_creates_in_the_same_fire() {
    let w = World::new();
    held_launch(&w, "triage").await;
    w.script([TabReply::Ended {
        session: "r-old".into(),
        handle: "rh-old".into(),
    }]);
    let second = manual(&w.home(), "triage");
    w.drain().await;
    let calls = w.remote_calls();
    let ops: Vec<TabOp> = calls.iter().map(|c| c.0).collect();
    assert_eq!(
        ops,
        vec![TabOp::Create, TabOp::Attach, TabOp::Attach, TabOp::Create],
        "{calls:?}"
    );
    assert_eq!(calls[3].1, wake::launch_request_id(&second.run_id));
    let run = run(&w.home(), &second.run_id);
    assert_eq!(run.phase, phase::DELIVERED);
    assert_eq!(run.session_id.as_deref(), Some("r-new"));
    assert!(w.take_redispatched().is_empty(), "not deferred");
}

// MUST 3: a hold that never settles is dropped after LAUNCH_SETTLEMENT_TTL:
// the fire that finds it raises needs_you (posted once), the next Creates.
#[test]
fn a_hold_that_never_settles_expires_after_the_settlement_ttl() {
    let flights = LaunchFlights::default();
    let key = (
        "acme/api/backend/barb".to_string(),
        String::new(),
        REMOTE.to_string(),
    );
    let id = uuid::Uuid::new_v4();
    flights.hold(&key, id, 1_000);
    assert_eq!(
        flights.begin(&key, 1_000 + wake::HOLD_TTL_MS - 1),
        wake::Begin::Reattach(id)
    );
    // Held again under the same id: it keeps its first time.
    flights.hold(&key, id, 1_000 + wake::HOLD_TTL_MS - 1);
    assert_eq!(
        flights.begin(&key, 1_000 + wake::HOLD_TTL_MS),
        wake::Begin::HoldExpired(wake::Expired {
            request_id: id,
            since: 1_000,
            asked: false,
        })
    );
    assert_eq!(
        flights.begin(&key, 1_000 + wake::HOLD_TTL_MS),
        wake::Begin::Go
    );
    assert_eq!(
        wake::HOLD_TTL_MS,
        crate::commands::pty_layout::LAUNCH_SETTLEMENT_TTL.as_millis() as i64
    );
}

#[tokio::test]
async fn an_expired_hold_raises_needs_you_and_the_next_fire_creates() {
    let w = World::new();
    let first = held_launch(&w, "triage").await;
    let key = ("".to_string(), String::new(), String::new());
    let _ = key;
    // Age the hold past the TTL.
    let flight = (barb_key(), String::new(), REMOTE.to_string());
    let held = w.env.flights.held(&flight).expect("held");
    assert_eq!(held, wake::launch_request_id(&first));
    w.env.flights.finish(&flight, None, 0);
    w.env
        .flights
        .restore_hold(&flight, held, finish::now_ms() - wake::HOLD_TTL_MS - 1);
    // It is attached once more first; still unconfirmed, not running there.
    w.script([TabReply::Unknown("no answer".into())]);
    let second = manual(&w.home(), "triage");
    w.drain().await;
    let run2 = run(&w.home(), &second.run_id);
    assert_eq!(run2.phase, phase::NEEDS_YOU, "{run2:?}");
    let posts = w.posts();
    assert_eq!(posts.last().unwrap().kind, NoticeKind::LaunchUnconfirmed);
    let third = manual(&w.home(), "triage");
    w.drain().await;
    let last = w.remote_calls().last().cloned().unwrap();
    assert_eq!(
        (last.0, last.1),
        (TabOp::Create, wake::launch_request_id(&third.run_id))
    );
    assert_eq!(run(&w.home(), &third.run_id).phase, phase::DELIVERED);
}

// Edge 1: an expired hold whose launch is still running on the machine
// (`pending`) raises needs_you once and stays held: nothing Creates over it;
// once the Attach confirms the tab, the wake goes to it.
#[tokio::test]
async fn an_expired_hold_still_pending_asks_once_and_stays_held() {
    let w = World::new();
    let first = held_launch(&w, "triage").await;
    let flight = (barb_key(), String::new(), REMOTE.to_string());
    let held = wake::launch_request_id(&first);
    w.env.flights.finish(&flight, None, 0);
    w.env
        .flights
        .restore_hold(&flight, held, finish::now_ms() - wake::HOLD_TTL_MS - 1);
    let pending = || TabReply::Pending("still launching".into());
    let asked = |w: &World| {
        w.posts()
            .iter()
            .filter(|p| p.kind == NoticeKind::LaunchUnconfirmed)
            .count()
    };
    w.script([pending()]);
    let second = manual(&w.home(), "triage");
    w.drain().await;
    assert_eq!(run(&w.home(), &second.run_id).phase, phase::NEEDS_YOU);
    assert_eq!(asked(&w), 1);
    assert_eq!(w.env.flights.held(&flight), Some(held), "kept");
    // Still pending at the next fire: its run asks too, the inbox once.
    w.script([pending()]);
    let third = manual(&w.home(), "triage");
    w.drain().await;
    assert_eq!(run(&w.home(), &third.run_id).phase, phase::NEEDS_YOU);
    assert_eq!(asked(&w), 1, "posted once per hold");
    assert_eq!(
        w.creates(),
        1,
        "never a second Create: {:?}",
        w.remote_calls()
    );
    // The machine confirms it: the next wake goes to that Barb.
    w.script([TabReply::Opened {
        session: "r-barb".into(),
        handle: "rh-barb".into(),
    }]);
    let fourth = manual(&w.home(), "triage");
    w.drain().await;
    w.redrive().await;
    assert_eq!(
        run(&w.home(), &fourth.run_id).session_id.as_deref(),
        Some("r-barb")
    );
    assert!(w.env.flights.held(&flight).is_none());
    assert_eq!(w.creates(), 1);
}

// Edge 2: a remote fresh run that times out (or is stopped) while its Create
// is out never keeps the session it then binds: its machine was recorded
// before the Create, and the agent is stopped there.
#[tokio::test]
async fn a_remote_fresh_run_ended_while_launching_stops_its_agent_there() {
    let w = World::new();
    let mut def = wake_routine("nightly", REMOTE);
    def.mode = "fresh".into();
    save(&w.home(), def);
    let home = w.home();
    *w.env.on_remote_call.lock().unwrap() = Some(Box::new(move |launch: &RemoteLaunch| {
        let store = super::state::RoutineStateStore::open(&home).unwrap();
        let before = store.run(&launch.run_id).unwrap().unwrap();
        assert_eq!(
            before.launch_machine.as_deref(),
            Some(REMOTE),
            "recorded before the Create"
        );
        finish::finish_and_announce(
            &store,
            &launch.run_id,
            finish::Ending::TimedOut,
            finish::now_ms(),
        );
    }));
    let fired = manual(&w.home(), "nightly");
    w.drain().await;
    assert_eq!(run(&w.home(), &fired.run_id).phase, phase::TIMED_OUT);
    assert_eq!(
        *w.env.killed_remote.lock().unwrap(),
        vec![(
            fired.run_id.clone(),
            REMOTE.to_string(),
            "r-new".to_string()
        )]
    );
}

// A fresh routine with `machine: any` stays valid with each_mac (each Mac
// starts its own); a wake one is refused.
#[test]
fn each_mac_with_any_stays_valid_for_fresh() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let create = |def: RoutineDef| {
        service::save(
            &home,
            &alice(),
            SaveTarget::Create(Placement::Team {
                org: ORG.into(),
                team: TEAM.into(),
            }),
            def,
            &super::Author::user("alice"),
        )
    };
    let mut fresh = wake_routine("each-fresh-any", "any");
    fresh.mode = "fresh".into();
    fresh.runs_on = "each_mac".into();
    create(fresh).expect("fresh + any + each_mac");
    let mut wake_any = wake_routine("each-wake-any", "any");
    wake_any.runs_on = "each_mac".into();
    assert!(create(wake_any).is_err());
}

// The pending wait: a launch still running on its machine is waited for
// (until launch-settled, at most SETTLE_WAIT), then asked once more.
#[tokio::test(start_paused = true)]
async fn a_pending_answer_waits_for_launch_settled_then_asks_again() {
    use super::wake_app::{through_pending, SETTLE_WAIT};
    use std::time::Duration;
    let pending = |s: &&str| *s == "pending";
    // Not pending: answered as it is, nothing asked again.
    let (out, again) = through_pending(
        "opened",
        pending,
        std::future::pending(),
        SETTLE_WAIT,
        || async { panic!("not asked again") },
    )
    .await;
    assert_eq!((out, again), ("opened", false));
    // Settled after 2 s: asked again then, well before the bound.
    let start = tokio::time::Instant::now();
    let (out, again) = through_pending(
        "pending",
        pending,
        tokio::time::sleep(Duration::from_secs(2)),
        SETTLE_WAIT,
        || async { "opened" },
    )
    .await;
    assert_eq!((out, again), ("opened", true));
    assert_eq!(start.elapsed(), Duration::from_secs(2));
    // Never settles: asked again at the bound; whatever it says stands.
    let start = tokio::time::Instant::now();
    let (out, again) = through_pending(
        "pending",
        pending,
        std::future::pending(),
        SETTLE_WAIT,
        || async { "pending" },
    )
    .await;
    assert_eq!((out, again), ("pending", true));
    assert_eq!(start.elapsed(), SETTLE_WAIT);
    assert_eq!(SETTLE_WAIT, Duration::from_secs(30));
}

// SHOULD 1: after an app restart (flights forgotten) the latest unknown
// launch of this agent on this machine is held again from the store, so the
// next fire only Attaches on it.
#[tokio::test]
async fn after_a_restart_an_unknown_launch_is_held_again_from_the_store() {
    let w = World::new();
    let first = held_launch(&w, "triage").await;
    let flight = (barb_key(), String::new(), REMOTE.to_string());
    w.env.flights.finish(&flight, None, 0); // the app restarted
    assert!(w.env.flights.held(&flight).is_none());
    w.script([TabReply::Unknown("still lost".into())]);
    manual(&w.home(), "triage");
    w.drain().await;
    let calls = w.remote_calls();
    let last = calls.last().unwrap();
    assert_eq!(
        (last.0, last.1),
        (TabOp::Attach, wake::launch_request_id(&first))
    );
    assert_eq!(w.creates(), 1, "never a second Create: {calls:?}");
}

// SHOULD 3: an old absence (no wake asked for a while) says nothing about
// now; tabs that left the layout leave the watch.
#[test]
fn the_quit_watch_restarts_a_stale_streak_and_forgets_closed_tabs() {
    use super::wake::{QuitState, QuitWatch, AGENT_QUIT_GRACE_MS};
    let watch = QuitWatch::default();
    assert_eq!(watch.observe(REMOTE, "s", true, 0), QuitState::Quitting);
    // Sampled again long after: a new streak, not a quit.
    let later = 10 * AGENT_QUIT_GRACE_MS;
    assert_eq!(watch.observe(REMOTE, "s", true, later), QuitState::Quitting);
    assert_eq!(
        watch.observe(REMOTE, "s", true, later + AGENT_QUIT_GRACE_MS),
        QuitState::Quit
    );
    watch.observe(REMOTE, "gone", true, 0);
    assert_eq!(watch.len(), 2);
    watch.keep_only(&[(REMOTE.to_string(), "s".to_string())].into());
    assert_eq!(watch.len(), 1);
}

// SHOULD 6: each_mac with another machine is refused whatever the mode.
#[test]
fn each_mac_with_another_machine_is_refused_for_fresh_too() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mut def = wake_routine("each-fresh", REMOTE);
    def.mode = "fresh".into();
    def.runs_on = "each_mac".into();
    match service::save(
        &home,
        &alice(),
        SaveTarget::Create(Placement::Team {
            org: ORG.into(),
            team: TEAM.into(),
        }),
        def,
        &super::Author::user("alice"),
    ) {
        Err(service::RoutineError::Validation(ValidationError::EachMacWakeNeedsLocal)) => {}
        other => panic!("refused: {other:?}"),
    }
}

// LOW: an Only me routine aimed at a machine says why, plainly.
#[tokio::test]
async fn an_only_me_routine_cannot_start_an_agent_on_a_machine() {
    let w = World::new();
    let launch = RemoteLaunch {
        run_id: "r1".into(),
        routine_key: super::model::RoutineKey::personal(&alice(), ORG, "mine").to_string(),
        routine_name: "Mine".into(),
        org: ORG.into(),
        team: String::new(),
        agent: barb_key(),
        machine: REMOTE.into(),
        target: super::model::RoutineTarget::TeamWorkspace,
        prompt: "p".into(),
        principal: alice(),
        request_id: wake::launch_request_id("r1"),
    };
    match wake::open_remote(w.env.as_ref(), &launch, TabOp::Create).await {
        TabReply::NotStarted { detail, .. } => assert!(detail.contains("Only me"), "{detail}"),
        other => panic!("{other:?}"),
    }
    assert!(w.remote_calls().is_empty());
}

// Deviation 1 (review decision): a wake whose agent is started with its
// task is delivered; a result schema keeps it running, with delivered_at.
#[tokio::test]
async fn a_launched_wake_with_a_result_schema_stays_running_with_delivered_at() {
    for machine in [LOCAL, REMOTE] {
        let w = World::new();
        save(&w.home(), with_result(wake_routine("triage", machine)));
        manual(&w.home(), "triage");
        w.drain().await;
        let run = only_run(&w.home(), "triage");
        assert_eq!(run.phase, phase::RUNNING, "{machine}");
        assert!(run.delivered_at.is_some(), "{machine}");
        assert!(
            run.wake_machine.is_some(),
            "{machine}: never the run's to kill"
        );
    }
}

// SW-AGR-2b AC1: no Barb anywhere, a remote wake → exactly one Create on its
// machine, under the run's request id, with the task as the first prompt.
#[tokio::test]
async fn r2_remote_absent_launches_one_on_its_machine() {
    let w = World::new();
    save(&w.home(), wake_routine("triage", REMOTE));
    let fired = manual(&w.home(), "triage");
    w.drain().await;

    assert_eq!(w.stub.calls(), 0, "nothing on this Mac");
    let calls = w.remote_calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    let (op, request_id, machine, prompt) = &calls[0];
    assert_eq!(*op, TabOp::Create);
    assert_eq!(machine, REMOTE);
    assert_eq!(*request_id, wake::launch_request_id(&fired.run_id));
    assert!(
        prompt.starts_with("[routine: Routine triage | run "),
        "{prompt}"
    );
    assert!(prompt.contains("triage new issues for 20"), "{prompt}");
    assert!(
        w.typed().is_empty(),
        "launched with it, never launched-then-typed"
    );
    let run = only_run(&w.home(), "triage");
    assert_eq!(
        run.phase,
        phase::DELIVERED,
        "its agent started with the task: delivered (no result schema)"
    );
    assert!(run.delivered_at.is_some());
    assert_eq!(run.session_id.as_deref(), Some("r-new"));
    assert_eq!(
        run.wake_machine.as_deref(),
        Some(REMOTE),
        "the Barb a wake starts is Barb's own, never the run's to kill"
    );
    // The launch it asked for: the routine's team and target, resolved there.
    let launch = &w.env.remote_calls.lock().unwrap()[0].1;
    assert_eq!((launch.org.as_str(), launch.team.as_str()), (ORG, TEAM));
    assert_eq!(launch.agent, barb_key());
    assert_eq!(launch.target, super::model::RoutineTarget::TeamWorkspace);
    assert_eq!(run.launch_machine.as_deref(), Some(REMOTE));
}

// AC1 + R1: a second fire right after the launch queues into the new Barb.
#[tokio::test]
async fn a_fire_right_after_a_remote_launch_queues_into_that_barb() {
    let w = World::new();
    save(&w.home(), wake_routine("triage", REMOTE));
    manual(&w.home(), "triage");
    w.drain().await;
    // Its run is still going: a manual fire of the same routine coalesces, so
    // another wake routine on the same Barb shows the single flight.
    save(&w.home(), wake_routine("digest", REMOTE));
    manual(&w.home(), "digest");
    w.drain().await;
    assert_eq!(w.creates(), 1, "never a second Barb");
    let digest = only_run(&w.home(), "digest");
    assert_eq!(digest.session_id.as_deref(), Some("r-new"));
    assert_eq!(
        digest.phase,
        phase::QUEUED,
        "queued until the new Barb is idle"
    );
}

// AC2 (wire): the Create's answer is lost; an Attach on the same request id
// finds the tab. One Create ever, so one Barb.
#[tokio::test]
async fn a_lost_create_reply_attaches_on_the_same_request_never_creates_again() {
    let w = World::new();
    w.script([
        TabReply::Unknown("the launch is still preparing after its budget".into()),
        TabReply::Opened {
            session: "r-barb".into(),
            handle: "rh-barb".into(),
        },
    ]);
    save(&w.home(), wake_routine("triage", REMOTE));
    let fired = manual(&w.home(), "triage");
    w.drain().await;

    let calls = w.remote_calls();
    let ops: Vec<TabOp> = calls.iter().map(|c| c.0).collect();
    assert_eq!(ops, vec![TabOp::Create, TabOp::Attach], "{calls:?}");
    let id = wake::launch_request_id(&fired.run_id);
    assert!(
        calls.iter().all(|c| c.1 == id),
        "the same request id: {calls:?}"
    );
    let run = only_run(&w.home(), "triage");
    assert_eq!(run.phase, phase::DELIVERED);
    assert_eq!(run.session_id.as_deref(), Some("r-barb"));
    assert_eq!(w.stub.calls(), 0);
    assert!(w.posts().is_empty(), "confirmed: nothing to tell");
}

// AC2: never confirmed either way → the run is `unknown` (posted once), the
// flight is held, and every later fire only Attaches on that request id.
#[tokio::test]
async fn an_unconfirmed_remote_launch_is_unknown_and_later_fires_only_attach() {
    let w = World::new();
    let lost = TabReply::Unknown("the machine stopped answering".into());
    w.script([lost.clone(), lost.clone()]);
    save(&w.home(), wake_routine("triage", REMOTE));
    let first = manual(&w.home(), "triage");
    w.drain().await;

    let run = only_run(&w.home(), "triage");
    assert_eq!(run.phase, phase::UNKNOWN, "{run:?}");
    let posts = w.posts();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(posts[0].kind, NoticeKind::Unknown);
    let held = wake::launch_request_id(&first.run_id);
    assert!(
        posts[0].detail.contains(&held.to_string()),
        "{:?}",
        posts[0]
    );

    // The next fire: still not confirmed → it waits, never Creates.
    w.script([lost]);
    let second = manual(&w.home(), "triage");
    w.drain().await;
    // Then the machine answers: the Barb the first Create started.
    w.script([TabReply::Opened {
        session: "r-barb".into(),
        handle: "rh-barb".into(),
    }]);
    w.redrive().await;
    let calls = w.remote_calls();
    assert_eq!(w.creates(), 1, "one Create ever: {calls:?}");
    assert!(
        calls.iter().all(|c| c.1 == held),
        "every call names the first run's request: {calls:?}"
    );
    // This fire's task goes to that Barb as a wake.
    w.redrive().await;
    let second = store(&w.home()).run(&second.run_id).unwrap().unwrap();
    assert_eq!(second.session_id.as_deref(), Some("r-barb"), "{second:?}");
    assert_eq!(
        second.phase,
        phase::QUEUED,
        "queued until that Barb is idle"
    );
    w.go(REMOTE, "r-barb", SimState::Idle);
    assert_typed_once(&w.typed(), "r-barb", "Routine triage");
    assert_eq!(w.stub.calls(), 0);
}

// AC2: an Attach that finds nothing was started frees the flight: the next
// launch Creates under its own run's id.
#[tokio::test]
async fn an_attach_that_finds_nothing_started_lets_the_next_fire_create() {
    let w = World::new();
    w.script([
        TabReply::Unknown("lost".into()),
        TabReply::Unknown("lost".into()),
        not_prepared("nothing was started"),
    ]);
    save(&w.home(), wake_routine("triage", REMOTE));
    manual(&w.home(), "triage");
    w.drain().await;
    let second = manual(&w.home(), "triage");
    w.drain().await;
    w.redrive().await;
    let calls = w.remote_calls();
    let last = calls.last().unwrap();
    assert_eq!(last.0, TabOp::Create, "{calls:?}");
    assert_eq!(last.1, wake::launch_request_id(&second.run_id));
    assert_eq!(w.creates(), 2);
}

// AC3: a Barb another desktop started on the machine (only its layout shows
// it, this desktop never launched it) is woken there, not duplicated.
#[tokio::test]
async fn a_barb_another_desktop_started_on_the_machine_is_woken_not_duplicated() {
    use super::wake_app::{roster_from, HostTabs, RosterRules};
    let w = World::new();
    let hosts = vec![HostTabs {
        machine: REMOTE.into(),
        online: true,
        tabs: vec![tab(
            "r-other-desktop",
            &[("remoteAgentKey", &barb_key()), ("remoteHandle", "rh-9")],
            false,
        )],
    }];
    let roster = roster_from(
        &hosts,
        &|_, _| Some(true),
        &|_| Some(Activity::Idle),
        // Not in this desktop's launch registry.
        &|_| None,
        &RosterRules::immediate(),
    );
    w.remote.set("r-other-desktop", SimState::Idle);
    *w.env.roster.lock().unwrap() = roster;
    for target in [REMOTE, "any"] {
        let id = format!("triage-{}", target.replace('-', ""));
        save(&w.home(), wake_routine(&id, target));
        manual(&w.home(), &id);
        w.drain().await;
        w.go(REMOTE, "r-other-desktop", SimState::Idle);
    }
    assert!(w.remote_calls().is_empty(), "nothing launched there");
    assert_eq!(w.stub.calls(), 0, "nothing launched here");
    let typed = w.typed();
    assert_eq!(typed.len(), 2, "{typed:?}");
    assert!(typed
        .iter()
        .all(|(session, _)| session == "r-other-desktop"));
}

// AC4: a fresh routine aimed at a machine launches there, through the same
// remote path (its own run's request id, the rendered task as its prompt),
// without a wake flight; a repo not cloned on this Mac is fine.
#[tokio::test]
async fn a_fresh_routine_on_a_machine_launches_there() {
    let w = World::new();
    let mut def = wake_routine("nightly", REMOTE);
    def.mode = "fresh".into();
    def.target.kind = "repo:acme/not-cloned-here".into();
    save(&w.home(), def);
    let fired = manual(&w.home(), "nightly");
    assert_eq!(fired.outcome, super::admit::Outcome::Queued);
    w.drain().await;

    assert_eq!(w.stub.calls(), 0, "nothing on this Mac");
    let calls = w.remote_calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    let (op, request_id, machine, prompt) = &calls[0];
    assert_eq!(*op, TabOp::Create);
    assert_eq!(machine, REMOTE);
    assert_eq!(*request_id, wake::launch_request_id(&fired.run_id));
    assert!(
        prompt.starts_with("triage new issues for 20") && prompt.ends_with("last time: "),
        "a fresh run's prompt is its rendered task: {prompt:?}"
    );
    let launch = &w.env.remote_calls.lock().unwrap()[0].1;
    assert_eq!(
        launch.target,
        super::model::RoutineTarget::Repo("acme/not-cloned-here".into())
    );
    let run = only_run(&w.home(), "nightly");
    assert_eq!(run.phase, phase::RUNNING);
    assert_eq!(run.session_id.as_deref(), Some("r-new"));
    assert!(
        w.env.flights.launched(finish::now_ms()).is_empty(),
        "a fresh run takes no wake flight"
    );
    finish::finish(&store(&w.home()), &fired.run_id, finish::Ending::Signal {
        status: crate::dashboard_stream::SessionStatus::Success,
        summary: Some("remote work finished".into()),
    }, finish::now_ms()).unwrap().expect("remote run closes");
    let usage = crate::session_usage::UsageStore::open(&w.home().join("usage.sqlite")).unwrap();
    let task = usage.read_task(&fired.run_id).unwrap().expect("remote fresh task");
    assert_eq!((task.status.as_str(), task.raw_status.as_deref()), ("done", Some("succeeded")));
}

// AC4: local fresh behaviour is unchanged.
#[tokio::test]
async fn a_fresh_routine_on_this_mac_still_launches_here() {
    let w = World::new();
    for machine in [LOCAL, "any"] {
        let id = format!("fresh-{machine}");
        let mut def = wake_routine(&id, machine);
        def.mode = "fresh".into();
        save(&w.home(), def);
        manual(&w.home(), &id);
        w.drain().await;
        assert_eq!(only_run(&w.home(), &id).phase, phase::RUNNING);
    }
    assert_eq!(w.stub.calls(), 2);
    assert!(w.remote_calls().is_empty());
}

#[tokio::test]
async fn a_fresh_remote_launch_never_confirmed_is_unknown_and_never_retried() {
    let w = World::new();
    w.script([
        TabReply::Unknown("lost".into()),
        TabReply::Unknown("still lost".into()),
    ]);
    let mut def = wake_routine("nightly", REMOTE);
    def.mode = "fresh".into();
    save(&w.home(), def);
    let fired = manual(&w.home(), "nightly");
    w.drain().await;
    let ops: Vec<TabOp> = w.remote_calls().iter().map(|c| c.0).collect();
    assert_eq!(ops, vec![TabOp::Create, TabOp::Attach]);
    let run = only_run(&w.home(), "nightly");
    assert_eq!(run.phase, phase::UNKNOWN);
    // Dispatched again (a restart's recovery): nothing is asked again.
    super::dispatch::execute(
        w.home(),
        pointer_of(&w.home(), &fired.run_id),
        w.launcher().as_ref(),
    )
    .await
    .unwrap();
    assert_eq!(w.remote_calls().len(), 2);
}

#[tokio::test]
async fn a_fresh_remote_routine_without_a_workspace_there_fails_target_unavailable() {
    let w = World::new();
    *w.env.remote_reply.lock().unwrap() = not_prepared("no workspace for acme/api on it");
    let mut def = wake_routine("nightly", REMOTE);
    def.mode = "fresh".into();
    save(&w.home(), def);
    manual(&w.home(), "nightly");
    w.drain().await;
    let run = only_run(&w.home(), "nightly");
    assert_eq!(
        (run.phase.as_str(), run.reason.as_deref()),
        (phase::FAILED, Some("target_unavailable"))
    );
}

#[test]
fn a_run_request_id_is_uuid_v5_of_its_run_id() {
    // RFC 4122 test vector: the DNS namespace and "python.org".
    assert_eq!(
        wake::uuid_v5(&uuid::Uuid::NAMESPACE_DNS, b"python.org").to_string(),
        "886313e1-3b8a-5372-9b90-0c9aee199e5d"
    );
    let id = wake::launch_request_id("run-1");
    assert_eq!(id.get_version_num(), 5);
    assert_eq!(id, wake::launch_request_id("run-1"), "stable per run");
    assert_ne!(id, wake::launch_request_id("run-2"));
}

// SHOULD: a remote wake needs no clone of its repo on this Mac.
#[tokio::test]
async fn a_remote_wake_on_a_repo_not_cloned_here_still_wakes_there() {
    let w = World::new();
    w.barb(REMOTE, BARB, SimState::Idle);
    let mut def = wake_routine("triage", REMOTE);
    def.target.kind = "repo:acme/not-cloned-here".into();
    save(&w.home(), def);
    let fired = manual(&w.home(), "triage");
    assert_eq!(fired.outcome, super::admit::Outcome::Queued);
    w.drain().await;
    assert_typed_once(&w.typed(), BARB, "Routine triage");
}

#[tokio::test]
async fn a_remote_workspace_not_prepared_is_undelivered_needs_preparation() {
    let w = World::new();
    *w.env.remote_reply.lock().unwrap() = not_prepared("prepare backend on codesense-test-1 first");
    save(&w.home(), wake_routine("triage", REMOTE));
    manual(&w.home(), "triage");
    w.drain().await;
    let run = only_run(&w.home(), "triage");
    assert_eq!(
        (run.phase.as_str(), run.reason.as_deref()),
        (phase::UNDELIVERED, Some("needs_preparation"))
    );
    assert_eq!(w.stub.calls(), 0);
}

#[tokio::test]
async fn any_machine_with_no_instance_launches_here() {
    let w = World::new();
    save(&w.home(), wake_routine("triage", "any"));
    manual(&w.home(), "triage");
    w.drain().await;
    assert_eq!(w.stub.calls(), 1);
    assert!(w.remote_calls().is_empty());
}

#[tokio::test]
async fn any_machine_prefers_an_idle_instance_over_a_busy_one() {
    let w = World::new();
    w.barb(LOCAL, "s-busy", SimState::Busy);
    w.barb(REMOTE, "s-idle", SimState::Idle);
    save(&w.home(), wake_routine("triage", "any"));
    manual(&w.home(), "triage");
    w.drain().await;
    assert_eq!(w.stub.calls(), 0);
    assert_typed_once(&w.typed(), "s-idle", "Routine triage");
}

// ───────────────────────────── collapsed fires ─────────────────────────────

#[tokio::test]
async fn two_fires_while_busy_collapse_into_one_queued_delivery() {
    let w = World::new();
    w.barb(LOCAL, BARB, SimState::Busy);
    let mut def = wake_routine("triage", LOCAL);
    // Even a routine that queues its fresh runs collapses wakes.
    def.policy.overlap = super::model::OverlapPolicy::Queue;
    save(&w.home(), def);
    let first = manual(&w.home(), "triage");
    w.drain().await;
    let second = manual(&w.home(), "triage");
    w.drain().await;

    assert_eq!(
        second.outcome,
        super::admit::Outcome::Coalesced {
            active_run_id: first.run_id.clone()
        }
    );
    assert_eq!(w.local.queued(), 1, "exactly one queued delivery");
    let merged = run(&w.home(), &second.run_id);
    assert_eq!(merged.phase, phase::COALESCED);
    assert_eq!(merged.linked_run_id.as_deref(), Some(first.run_id.as_str()));

    w.go(LOCAL, BARB, SimState::Idle);
    assert_typed_once(&w.typed(), BARB, "Routine triage");
    assert_eq!(w.stub.calls(), 0);
}

// MUST 2: a delivered wake never holds back the next fire.
#[tokio::test]
async fn a_fire_after_a_delivered_wake_is_delivered_again() {
    let w = World::new();
    w.barb(LOCAL, BARB, SimState::Idle);
    save(&w.home(), wake_routine("triage", LOCAL));
    manual(&w.home(), "triage");
    w.drain().await;
    assert_eq!(w.typed().len(), 1);
    // Barb finished that turn and sits idle again.
    w.local.set(BARB, SimState::Idle);
    let second = manual(&w.home(), "triage");
    assert_eq!(second.outcome, super::admit::Outcome::Queued, "not folded");
    w.drain().await;
    assert_eq!(w.typed().len(), 2, "a second delivery");
    assert!(runs_of(&w.home(), "triage")
        .iter()
        .all(|r| r.phase == phase::DELIVERED));
}

// MUST 2 with a result schema: the delivered wake stays open for its result,
// yet the next fire is still typed in (only a waiting wake folds fires).
#[tokio::test]
async fn a_fire_after_a_delivered_wake_still_open_for_its_result_is_delivered() {
    let w = World::new();
    w.barb(LOCAL, BARB, SimState::Idle);
    save(&w.home(), with_result(wake_routine("triage", LOCAL)));
    let first = manual(&w.home(), "triage");
    w.drain().await;
    assert_eq!(run(&w.home(), &first.run_id).phase, phase::RUNNING);
    w.local.set(BARB, SimState::Idle);
    let second = manual(&w.home(), "triage");
    assert_eq!(second.outcome, super::admit::Outcome::Queued);
    w.drain().await;
    assert_eq!(w.typed().len(), 2);
}

// ───────────────────────────── R1 guards ─────────────────────────────

#[tokio::test]
async fn two_local_routines_in_one_tick_launch_separate_sessions() {
    let w = World::new();
    save(&w.home(), wake_routine("triage", LOCAL));
    save(&w.home(), wake_routine("digest", LOCAL));
    manual(&w.home(), "triage");
    manual(&w.home(), "digest");
    w.drain().await;
    w.redrive().await;

    assert_eq!(w.stub.calls(), 2);
    let a = only_run(&w.home(), "triage");
    let b = only_run(&w.home(), "digest");
    assert_eq!(a.phase, phase::DELIVERED);
    assert_eq!(b.phase, phase::DELIVERED);
    assert_ne!(a.session_id, b.session_id);
    assert!(w.typed().is_empty());
}

fn pointer_of(home: &Path, run_id: &str) -> RoutinePointer {
    let coordinator = crate::coordinator::SqliteOrchestrationStore::open(
        crate::coordinator::default_store_path(home),
    )
    .unwrap();
    let json = coordinator.run_request_json(run_id).unwrap().unwrap();
    super::admit::parse_pointer(&json).unwrap().unwrap()
}

// Different local routines dispatched concurrently each own their launch.
#[tokio::test]
async fn concurrent_local_routines_launch_separate_sessions() {
    let w = World::new();
    save(&w.home(), wake_routine("triage", LOCAL));
    save(&w.home(), wake_routine("digest", LOCAL));
    let a = pointer_of(&w.home(), &manual(&w.home(), "triage").run_id);
    let b = pointer_of(&w.home(), &manual(&w.home(), "digest").run_id);
    let (la, lb) = (w.launcher(), w.launcher());
    let (ra, rb) = tokio::join!(
        super::dispatch::execute(w.home(), a, la.as_ref()),
        super::dispatch::execute(w.home(), b, lb.as_ref()),
    );
    ra.unwrap();
    rb.unwrap();
    for _ in 0..3 {
        w.redrive().await;
    }
    assert_eq!(w.stub.calls(), 2);
    assert_ne!(
        only_run(&w.home(), "triage").session_id,
        only_run(&w.home(), "digest").session_id
    );
    assert!(w.typed().is_empty());
}

#[tokio::test]
async fn a_local_inbox_wake_does_not_adopt_a_different_routines_session() {
    let w = World::new();
    w.barb(LOCAL, BARB, SimState::Idle);
    save(&w.home(), wake_routine("triage", LOCAL));
    save(&w.home(), inbox_wake("inbox", LOCAL));
    let clock = Arc::new(AtomicI64::new(finish::now_ms()));
    let ticker = ticker(&w.home(), &clock, w.stub.clone());
    // The same tick: the schedule's fire and the inbox's.
    manual(&w.home(), "triage");
    let offered = ticker.offer_inbox_idle(&[idle_mail(BARB, 5)]).unwrap();
    assert_eq!(offered.admissions.len(), 1);
    w.drain().await;

    assert_eq!(w.stub.calls(), 1, "the inbox routine needs its own session");
    assert_typed_once(&w.typed(), BARB, "Routine triage");
    assert_eq!(w.local.queued(), 0);
}

// MUST 5: a fire before the restore pass is deferred (the claim loop goes
// on), then resolved once the restored Barb is listed.
#[tokio::test]
async fn r1_a_fire_before_the_restore_pass_is_deferred_and_never_duplicates() {
    let w = World::new();
    *w.env.restore.lock().unwrap() = Restore::Pending;
    save(&w.home(), wake_routine("triage", LOCAL));
    manual(&w.home(), "triage");
    w.drain().await;
    let waiting = only_run(&w.home(), "triage");
    assert_eq!(waiting.phase, phase::QUEUED);
    assert_eq!(waiting.wake_machine.as_deref(), Some(wake::DEFERRED));
    assert_eq!(w.env.redispatched.lock().unwrap().len(), 1);
    // Another fire meanwhile folds into the waiting one.
    let again = manual(&w.home(), "triage");
    assert!(matches!(
        again.outcome,
        super::admit::Outcome::Coalesced { .. }
    ));
    // The restore pass brings back Barb's tab.
    w.local.set(BARB, SimState::Idle);
    w.env
        .roster
        .lock()
        .unwrap()
        .push(candidate(LOCAL, BARB, &barb_key(), Activity::Idle));
    *w.env.restore.lock().unwrap() = Restore::Settled;
    w.redrive().await;

    assert_eq!(
        w.stub.calls(),
        0,
        "the restored Barb is woken, not duplicated"
    );
    assert_typed_once(&w.typed(), BARB, "Routine triage");
}

#[tokio::test]
async fn a_restore_that_never_settles_ends_unobservable_and_launches_nothing() {
    let w = World::new();
    *w.env.restore.lock().unwrap() = Restore::Pending;
    save(&w.home(), wake_routine("triage", LOCAL));
    manual(&w.home(), "triage");
    w.drain().await;
    // Ten minutes on, it is still not settled.
    let mut pointer = w.take_redispatched().remove(0);
    pointer.fired_at -= wake::DEFER_CAP_MS;
    super::dispatch::execute(w.home(), pointer, w.launcher().as_ref())
        .await
        .unwrap();
    let run = only_run(&w.home(), "triage");
    assert_eq!(
        (run.phase.as_str(), run.reason.as_deref()),
        (phase::UNDELIVERED, Some("unobservable"))
    );
    assert_eq!(w.stub.calls(), 0);
    // A restore that gave up says so at once.
    *w.env.restore.lock().unwrap() = Restore::GaveUp;
    let gave_up = manual(&w.home(), "triage");
    w.drain().await;
    assert_eq!(
        run_reason(&w.home(), &gave_up.run_id),
        Some("unobservable".into())
    );
    assert!(w.take_redispatched().is_empty());
    assert_eq!(w.stub.calls(), 0);
}

fn run_reason(home: &Path, run_id: &str) -> Option<String> {
    run(home, run_id).reason
}

#[tokio::test]
async fn r1_a_starter_routine_never_types_into_a_hand_opened_tab() {
    let w = World::new();
    // A Claude tab opened by hand (no saved agent), and one started from the
    // built-in starter: both idle.
    w.local.set("s-hand", SimState::Idle);
    w.local.set("s-starter", SimState::Idle);
    {
        let mut roster = w.env.roster.lock().unwrap();
        let mut hand = candidate(LOCAL, "s-hand", "x", Activity::Idle);
        hand.agent = None;
        roster.push(hand);
        roster.push(candidate(
            LOCAL,
            "s-starter",
            "builtin/claude",
            Activity::Idle,
        ));
    }
    let mut def = routine("starter", "0 9 * * *");
    def.agent = AgentRef::from("builtin/claude");
    def.policy.visible = true;
    save(&w.home(), def);
    // Even a definition hand-edited to `wake` runs fresh on a starter.
    let mut edited = Vec::new();
    for entry in walkdir::WalkDir::new(w.temp.root()) {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "yml")
            && path.to_string_lossy().contains("/routines/starter")
        {
            let text = std::fs::read_to_string(path).unwrap();
            if text.contains("mode: fresh") {
                std::fs::write(path, text.replace("mode: fresh", "mode: wake")).unwrap();
                edited.push(path.to_path_buf());
            }
        }
    }
    assert!(!edited.is_empty(), "the routine's files were hand-edited");
    manual(&w.home(), "starter");
    w.drain().await;

    assert!(
        w.typed().is_empty(),
        "never typed into a tab opened by hand"
    );
    assert_eq!(w.stub.calls(), 1, "a starter's routine starts fresh");
    assert!(only_run(&w.home(), "starter").wake_machine.is_none());
}

// MUST 3: a tab whose agent quit back to its shell is not Barb.
#[tokio::test]
async fn a_tab_whose_agent_quit_is_not_woken_and_barb_is_started() {
    let w = World::new();
    // The probe saw no agent CLI in the tab: it is listed as ended.
    w.local.set(BARB, SimState::Idle);
    w.env
        .roster
        .lock()
        .unwrap()
        .push(candidate(LOCAL, BARB, &barb_key(), Activity::Ended));
    save(&w.home(), wake_routine("triage", LOCAL));
    manual(&w.home(), "triage");
    w.drain().await;
    assert!(w.typed().is_empty(), "never typed into the bare shell");
    assert_eq!(w.stub.calls(), 1);
}

// MUST 3: the agent quit between the listing and the delivery: the server
// answers `session_ended`, and the wake is resolved again (launch if absent).
#[tokio::test]
async fn a_wake_whose_agent_quit_meanwhile_is_resolved_again_and_launches() {
    let w = World::new();
    w.barb(LOCAL, BARB, SimState::Gone);
    save(&w.home(), wake_routine("triage", LOCAL));
    manual(&w.home(), "triage");
    w.drain().await;
    let reopened = only_run(&w.home(), "triage");
    assert_eq!(reopened.phase, phase::QUEUED, "not ended: resolved again");
    assert_eq!(reopened.session_id, None);
    assert_eq!(w.env.redispatched.lock().unwrap().len(), 1);
    // Now the probe sees it quit too.
    w.env.roster.lock().unwrap()[0].activity = Activity::Ended;
    w.redrive().await;
    assert_eq!(w.stub.calls(), 1, "Barb started once");
    assert!(w.typed().is_empty());
    assert_eq!(only_run(&w.home(), "triage").phase, phase::DELIVERED);
}

// r3: a session found ended after it was picked is never picked again for
// the run, even while the list still shows it live: here the agent launches.
#[tokio::test]
async fn a_session_found_ended_is_not_picked_again_and_the_wake_launches_here() {
    let w = World::new();
    w.barb(LOCAL, BARB, SimState::Gone);
    save(&w.home(), wake_routine("triage", LOCAL));
    manual(&w.home(), "triage");
    w.drain().await;
    let pointers = w.take_redispatched();
    assert_eq!(pointers.len(), 1);
    assert_eq!(pointers[0].ended_sessions, vec![format!("{LOCAL}/{BARB}")]);
    // The list still shows the quit tab as live Barb.
    for pointer in pointers {
        super::dispatch::execute(w.home(), pointer, w.launcher().as_ref())
            .await
            .unwrap();
    }
    assert_eq!(w.stub.calls(), 1, "launched, not retried into the dead tab");
    assert!(w.take_redispatched().is_empty(), "no retry loop");
    assert_eq!(only_run(&w.home(), "triage").phase, phase::DELIVERED);
}

// r3, remote: a quit Barb is never retried into; with no workspace there
// it ends `undelivered(needs_preparation)`.
#[tokio::test]
async fn a_remote_session_found_ended_ends_needs_preparation_not_retried() {
    let w = World::new();
    *w.env.remote_reply.lock().unwrap() = not_prepared("no workspace for it there");
    w.barb(REMOTE, BARB, SimState::Gone);
    save(&w.home(), wake_routine("triage", REMOTE));
    manual(&w.home(), "triage");
    w.drain().await;
    for _ in 0..3 {
        w.redrive().await;
    }
    let run = only_run(&w.home(), "triage");
    assert_eq!(
        (run.phase.as_str(), run.reason.as_deref()),
        (phase::UNDELIVERED, Some("needs_preparation"))
    );
    assert!(w.take_redispatched().is_empty());
    assert!(w.typed().is_empty());
}

// ───────────────────────────── honest results ─────────────────────────────

#[tokio::test]
async fn an_unknown_wake_is_never_sent_again_and_posts_once_to_the_inbox() {
    let w = World::new();
    w.barb(REMOTE, BARB, SimState::Idle);
    w.remote.lose_reply.store(true, Ordering::SeqCst);
    save(&w.home(), wake_routine("triage", REMOTE));
    let fired = manual(&w.home(), "triage");
    w.drain().await;

    let unknown = only_run(&w.home(), "triage");
    assert_eq!(unknown.phase, phase::UNKNOWN);
    assert_eq!(w.typed().len(), 1, "it did reach Barb");
    let posts = w.posts();
    assert_eq!(posts.len(), 1, "one inbox post: {posts:?}");
    assert_eq!(posts[0].kind, NoticeKind::Unknown);
    assert_eq!(posts[0].run_id, fired.run_id);
    assert_eq!(posts[0].session, BARB);
    assert!(posts[0].backlink().contains(BARB), "Show-tab backlink");
    assert_eq!(
        posts[0].context(),
        "May have been delivered. Check the terminal before re-sending."
    );

    // Dispatched again (a restart's recovery): nothing is sent again.
    let pointer = RoutinePointer {
        run_id: fired.run_id.clone(),
        routine_key: key("triage").to_string(),
        revision: 1,
        principal: alice().as_str().to_string(),
        source: "manual".into(),
        scheduled_at: None,
        fired_at: finish::now_ms(),
        variables: Default::default(),
        trigger_payload: None,
        ended_sessions: Vec::new(),
    };
    super::dispatch::execute(w.home(), pointer, w.launcher().as_ref())
        .await
        .unwrap();
    // The catch-up reports it unknown once more when its wait runs out.
    wake::record_status(
        &store(&w.home()),
        &fired.run_id,
        &SessionRef::new(REMOTE, BARB),
        &WakeRunStatus::Unknown {
            detail: "no outcome arrived within its wait".into(),
        },
        w.env.as_ref(),
    );
    assert_eq!(w.typed().len(), 1, "never re-sent");
    assert_eq!(w.stub.calls(), 0);
    assert_eq!(w.posts().len(), 1, "posted once per run");
}

#[tokio::test]
async fn cancel_wake_on_an_unreachable_host_errors_and_keeps_it_queued() {
    let w = World::new();
    w.barb(REMOTE, BARB, SimState::Busy);
    save(&w.home(), wake_routine("triage", REMOTE));
    let fired = manual(&w.home(), "triage");
    w.drain().await;
    w.env.machines.offline.lock().unwrap().push(REMOTE.into());

    let sessions = super::test_support::FakeSessions::default();
    let err = service::stop_run(&w.home(), &alice(), &fired.run_id, &sessions).unwrap_err();
    assert!(err.to_string().contains("still queued"), "{err}");
    let err = crate::mcp::routine_tools::cancel(
        &w.home(),
        &alice(),
        serde_json::from_value(serde_json::json!({"runId": fired.run_id})).unwrap(),
        &sessions,
    )
    .unwrap_err();
    assert!(err.contains("still queued"), "{err}");
    assert_eq!(
        run(&w.home(), &fired.run_id).phase,
        phase::QUEUED,
        "never ended locally"
    );
    assert_eq!(w.remote.queued(), 1, "the host still holds it");
    assert!(w.typed().is_empty(), "nothing typed");
}

#[tokio::test]
async fn cancel_wake_after_it_went_in_reads_delivered() {
    let w = World::new();
    w.barb(REMOTE, BARB, SimState::Busy);
    save(&w.home(), with_result(wake_routine("triage", REMOTE)));
    let fired = manual(&w.home(), "triage");
    w.drain().await;
    // The host types it, but its outcome has not reached this desktop yet.
    let _unsent = w.remote.go(BARB, SimState::Idle);
    assert_eq!(w.typed().len(), 1);

    let sessions = super::test_support::FakeSessions::default();
    let reply = crate::mcp::routine_tools::cancel(
        &w.home(),
        &alice(),
        serde_json::from_value(serde_json::json!({"runId": fired.run_id})).unwrap(),
        &sessions,
    )
    .unwrap();
    let wire = serde_json::to_value(reply).unwrap();
    assert_eq!(wire["status"], "validation");
    let message = wire["data"]["message"].as_str().unwrap();
    assert!(message.contains("could not be withdrawn"), "{message}");
    assert!(message.contains("running"), "{message}");
    assert!(!message.contains("already finished"), "{message}");
    assert!(sessions.killed.lock().unwrap().is_empty());
    assert!(sessions.killed_on.lock().unwrap().is_empty());
    let run = run(&w.home(), &fired.run_id);
    assert!(run.delivered_at.is_some(), "recorded as delivered: {run:?}");
    assert_ne!(run.phase, phase::CANCELLED);
}

#[tokio::test]
async fn cancel_wake_without_the_wake_env_errors_instead_of_ending_locally() {
    let w = World::new();
    w.barb(REMOTE, BARB, SimState::Busy);
    save(&w.home(), wake_routine("triage", REMOTE));
    let fired = manual(&w.home(), "triage");
    w.drain().await;
    wake::uninstall(&w.home());

    let sessions = super::test_support::FakeSessions::default();
    assert!(service::stop_run(&w.home(), &alice(), &fired.run_id, &sessions).is_err());
    assert_eq!(run(&w.home(), &fired.run_id).phase, phase::QUEUED);
    assert_eq!(w.remote.queued(), 1);
}

/// The phase and reason of `id`'s last run as the routine list reads it.
fn listed_last(home: &std::path::Path, id: &str) -> (String, Option<String>, String) {
    let summary = service::list_summaries(home, &alice())
        .unwrap()
        .into_iter()
        .find(|s| s.key == key(id))
        .expect("listed");
    let last = summary.last_run.expect("a last run");
    (last.phase, last.reason, last.run_id)
}

/// SW-AGR-5 r3: an undelivered wake is the last run whichever path ended it,
/// including those that never recorded a machine on the run.
#[tokio::test]
async fn a_machine_unknown_wake_is_the_routines_last_run() {
    let w = World::new();
    save(&w.home(), wake_routine("triage", "m-gone"));
    let fired = manual(&w.home(), "triage");
    w.drain().await;
    assert_eq!(
        listed_last(&w.home(), "triage"),
        (
            "undelivered".into(),
            Some("machine_unknown".into()),
            fired.run_id
        )
    );
}

#[tokio::test]
async fn a_machine_offline_wake_is_the_routines_last_run() {
    let w = World::new();
    w.env.machines.offline.lock().unwrap().push(REMOTE.into());
    w.env
        .roster
        .lock()
        .unwrap()
        .push(candidate(REMOTE, BARB, &barb_key(), Activity::Offline));
    save(&w.home(), wake_routine("triage", "any"));
    let fired = manual(&w.home(), "triage");
    w.drain().await;
    assert_eq!(
        listed_last(&w.home(), "triage"),
        (
            "undelivered".into(),
            Some("machine_offline".into()),
            fired.run_id
        )
    );
}

#[tokio::test]
async fn a_needs_preparation_launch_is_the_routines_last_run() {
    let w = World::new();
    *w.env.remote_reply.lock().unwrap() = not_prepared("prepare backend on codesense-test-1 first");
    save(&w.home(), wake_routine("triage", REMOTE));
    let fired = manual(&w.home(), "triage");
    w.drain().await;
    assert_eq!(
        listed_last(&w.home(), "triage"),
        (
            "undelivered".into(),
            Some("needs_preparation".into()),
            fired.run_id
        )
    );
}

#[tokio::test]
async fn cancel_wake_while_its_remote_launch_is_in_flight_keeps_it_queued() {
    let w = World::new();
    w.barb(REMOTE, BARB, SimState::Busy);
    save(&w.home(), wake_routine("triage", REMOTE));
    let fired = manual(&w.home(), "triage");
    w.drain().await;
    // A launch-if-absent spawn on the machine is out and unanswered.
    store(&w.home())
        .transaction(|tx| {
            tx.execute(
                "UPDATE routine_runs SET launch_request = 'req-1', launch_machine = ?2
                  WHERE run_id = ?1",
                rusqlite::params![fired.run_id, REMOTE],
            )?;
            Ok(())
        })
        .unwrap();
    let sessions = super::test_support::FakeSessions::default();
    let err = service::stop_run(&w.home(), &alice(), &fired.run_id, &sessions).unwrap_err();
    assert!(err.to_string().contains("still starting"), "{err}");
    assert_eq!(
        run(&w.home(), &fired.run_id).phase,
        phase::QUEUED,
        "never ended locally"
    );
    assert_eq!(
        w.env.cancels.load(Ordering::SeqCst),
        0,
        "nothing withdrawn mid-spawn"
    );
}

/// SW-AGR-5 r2: a wake that ends `unknown` or `undelivered` straight from
/// `queued` (it never got a `started_at`) is the routine's last run, so the
/// list, the attention count and the Run-now guard see it.
#[tokio::test]
async fn a_wake_ending_from_queued_is_the_routines_last_run() {
    let w = World::new();
    w.barb(REMOTE, BARB, SimState::Busy);
    save(&w.home(), wake_routine("triage", REMOTE));
    let fired = manual(&w.home(), "triage");
    w.drain().await;
    assert_eq!(only_run(&w.home(), "triage").phase, phase::QUEUED);
    assert!(only_run(&w.home(), "triage").started_at.is_none());

    wake::record_status(
        &store(&w.home()),
        &fired.run_id,
        &SessionRef::new(REMOTE, BARB),
        &WakeRunStatus::Unknown {
            detail: "the machine didn't confirm the paste".into(),
        },
        w.env.as_ref(),
    );
    let listed = service::list_summaries(&w.home(), &alice()).unwrap();
    let last = listed
        .iter()
        .find(|s| s.key == key("triage"))
        .and_then(|s| s.last_run.clone())
        .expect("the unknown wake is the last run");
    assert_eq!(
        (last.run_id.as_str(), last.phase.as_str()),
        (fired.run_id.as_str(), phase::UNKNOWN)
    );
    assert_eq!(last.wake_machine.as_deref(), Some(REMOTE));

    // An undelivered one, too.
    let again = manual(&w.home(), "triage");
    w.drain().await;
    wake::record_status(
        &store(&w.home()),
        &again.run_id,
        &SessionRef::new(REMOTE, BARB),
        &WakeRunStatus::Undelivered {
            reason: UndeliveredReason::BusyTimeout,
        },
        w.env.as_ref(),
    );
    let listed = service::list_summaries(&w.home(), &alice()).unwrap();
    let last = listed
        .iter()
        .find(|s| s.key == key("triage"))
        .and_then(|s| s.last_run.clone())
        .unwrap();
    assert_eq!(
        (
            last.run_id.as_str(),
            last.phase.as_str(),
            last.reason.as_deref()
        ),
        (
            again.run_id.as_str(),
            phase::UNDELIVERED,
            Some("busy_timeout")
        )
    );
}

#[tokio::test]
async fn needs_you_is_never_typed_into_until_it_is_answered() {
    for machine in [LOCAL, REMOTE] {
        let w = World::new();
        w.barb(machine, BARB, SimState::NeedsYou);
        save(&w.home(), wake_routine("triage", machine));
        manual(&w.home(), "triage");
        w.drain().await;
        assert!(w.typed().is_empty());
        assert_eq!(only_run(&w.home(), "triage").phase, phase::QUEUED);
        // Still waiting on the human.
        w.go(machine, BARB, SimState::NeedsYou);
        assert!(w.typed().is_empty(), "nothing typed while a prompt waits");
        w.go(machine, BARB, SimState::Idle);
        assert_typed_once(&w.typed(), BARB, "Routine triage");
    }
}

#[tokio::test]
async fn a_machine_this_mac_does_not_know_is_machine_unknown() {
    let w = World::new();
    save(&w.home(), wake_routine("triage", "m-gone"));
    manual(&w.home(), "triage");
    w.drain().await;
    let run = only_run(&w.home(), "triage");
    assert_eq!(
        (run.phase.as_str(), run.reason.as_deref()),
        (phase::UNDELIVERED, Some("machine_unknown"))
    );
    assert_eq!(w.stub.calls(), 0);
    assert!(w.remote_calls().is_empty());
}

// Decision (review r1): never duplicate wins. An agent last seen running on a
// machine that is offline is never started again here, however long it stays
// offline: `undelivered(machine_offline)`, posted once per streak.
#[tokio::test]
async fn an_agent_on_an_offline_machine_is_never_started_again_here() {
    let w = World::new();
    w.env.machines.offline.lock().unwrap().push(REMOTE.into());
    w.env
        .roster
        .lock()
        .unwrap()
        .push(candidate(REMOTE, BARB, &barb_key(), Activity::Offline));
    save(&w.home(), wake_routine("triage", "any"));
    for _ in 0..2 {
        manual(&w.home(), "triage");
        w.drain().await;
    }
    let runs = runs_of(&w.home(), "triage");
    assert_eq!(runs.len(), 2);
    assert!(runs
        .iter()
        .all(|r| r.phase == phase::UNDELIVERED && r.reason.as_deref() == Some("machine_offline")));
    assert_eq!(w.stub.calls(), 0, "R1: a disconnected Barb is still live");
    let posts = w.posts();
    assert_eq!(posts.len(), 1, "posted once for the streak: {posts:?}");
    assert_eq!(posts[0].kind, NoticeKind::Offline);
}

// ───────────────────────────── validation, archive ─────────────────────────────

#[test]
fn each_mac_refuses_a_wake_on_another_machine_and_fresh_may_target_one() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let create = |def: RoutineDef| {
        service::save(
            &home,
            &alice(),
            SaveTarget::Create(Placement::Team {
                org: ORG.into(),
                team: TEAM.into(),
            }),
            def,
            &super::Author::user("alice"),
        )
    };
    let mut remote = wake_routine("each-remote", REMOTE);
    remote.runs_on = "each_mac".into();
    match create(remote) {
        Err(service::RoutineError::Validation(ValidationError::EachMacWakeNeedsLocal)) => {}
        other => panic!("refused: {other:?}"),
    }
    let mut any = wake_routine("each-any", "any");
    any.runs_on = "each_mac".into();
    assert!(create(any).is_err());
    let mut here = wake_routine("each-here", LOCAL);
    here.runs_on = "each_mac".into();
    create(here).expect("each Mac waking its own agent is fine");
    // SW-AGR-2b: a fresh run on another machine launches there.
    let mut fresh = wake_routine("fresh-remote", REMOTE);
    fresh.mode = "fresh".into();
    create(fresh).expect("a fresh routine may target a machine");
}

#[tokio::test]
async fn archiving_a_routine_cancels_its_queued_wake() {
    let w = World::new();
    w.barb(REMOTE, BARB, SimState::Busy);
    save(&w.home(), wake_routine("triage", REMOTE));
    manual(&w.home(), "triage");
    w.drain().await;
    assert_eq!(w.remote.queued(), 1);

    service::archive(
        &w.home(),
        &alice(),
        &key("triage"),
        &super::Author::user("alice"),
    )
    .unwrap();
    assert_eq!(w.env.cancels.load(Ordering::SeqCst), 1);
    assert_eq!(w.remote.queued(), 0);
    let run = only_run(&w.home(), "triage");
    assert_eq!(
        (run.phase.as_str(), run.reason.as_deref()),
        (phase::UNDELIVERED, Some("archived"))
    );
    w.go(REMOTE, BARB, SimState::Idle);
    assert!(w.typed().is_empty(), "never typed after the archive");
}

#[tokio::test]
async fn cancel_wake_withdraws_the_queued_wake_from_its_host() {
    let w = World::new();
    w.barb(REMOTE, BARB, SimState::Busy);
    save(&w.home(), wake_routine("triage", REMOTE));
    let fired = manual(&w.home(), "triage");
    w.drain().await;
    assert_eq!(w.remote.queued(), 1);

    let sessions = super::test_support::FakeSessions::default();
    let reply = crate::mcp::routine_tools::cancel(
        &w.home(),
        &alice(),
        serde_json::from_value(serde_json::json!({"runId": fired.run_id})).unwrap(),
        &sessions,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(reply).unwrap()["data"]["phase"],
        "cancelled"
    );
    assert_eq!(
        w.env.cancels.load(Ordering::SeqCst),
        1,
        "the host was asked"
    );
    assert_eq!(w.remote.queued(), 0, "nothing waits on the host any more");
    let run = run(&w.home(), &fired.run_id);
    assert_eq!(run.phase, phase::CANCELLED);
    w.go(REMOTE, BARB, SimState::Idle);
    assert!(w.typed().is_empty(), "never typed after Cancel wake");
}

/// A wake routine whose result keeps its run open after delivery (§9.7).
fn with_result(mut def: RoutineDef) -> RoutineDef {
    def.result_schema = Some(serde_json::json!({
        "type": "object",
        "properties": {"ok": {"type": "boolean"}}
    }));
    def
}

#[tokio::test]
async fn stopping_a_wake_run_never_kills_the_woken_session() {
    let w = World::new();
    w.barb(LOCAL, BARB, SimState::Idle);
    save(&w.home(), with_result(wake_routine("triage", LOCAL)));
    let fired = manual(&w.home(), "triage");
    w.drain().await;
    assert_eq!(
        run(&w.home(), &fired.run_id).phase,
        phase::RUNNING,
        "a result schema keeps it open"
    );
    let sessions = super::test_support::FakeSessions::default();
    let reply = crate::mcp::routine_tools::cancel(
        &w.home(),
        &alice(),
        serde_json::from_value(serde_json::json!({"runId": fired.run_id})).unwrap(),
        &sessions,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(reply).unwrap()["data"]["phase"],
        "cancelled"
    );
    assert!(
        sessions.killed.lock().unwrap().is_empty(),
        "Barb's own session outlives the run"
    );
}

// MUST 1: the Barb a wake started outlives the run that started it.
#[tokio::test]
async fn the_launching_run_timing_out_never_kills_the_barb_it_started() {
    let w = World::new();
    // Its result schema keeps the launching run open (delivered, running).
    save(&w.home(), with_result(wake_routine("triage", LOCAL)));
    save(&w.home(), wake_routine("digest", LOCAL));
    let first = manual(&w.home(), "triage");
    w.drain().await;
    let barb = w.stub.last().session_id;
    // A different local routine owns a separate session.
    manual(&w.home(), "digest");
    w.drain().await;
    assert_ne!(
        only_run(&w.home(), "digest").session_id.as_deref(),
        Some(barb.as_str())
    );
    // The first run passes its max runtime.
    let late = finish::now_ms() + 24 * 60 * 60 * 1000;
    let sessions = super::test_support::FakeSessions::default();
    assert_eq!(
        finish::watchdog(&store(&w.home()), late, &sessions).unwrap(),
        1
    );
    assert_eq!(run(&w.home(), &first.run_id).phase, phase::TIMED_OUT);
    assert!(sessions.killed.lock().unwrap().is_empty(), "nothing killed");
    assert!(w.stub.is_running(&barb), "Barb is still alive");
    // And a stop of it kills nothing either.
    assert_eq!(only_run(&w.home(), "digest").phase, phase::DELIVERED);
}

// ───────────────────────────── Start-or-show (MUST 6) ─────────────────────────────

/// The flight key a wake on the test team's workspace launches under.
fn team_flight(home: &Path, id: &str) -> wake::FlightKey {
    let _ = home;
    (barb_key(), key(id).to_string(), LOCAL.into())
}

#[tokio::test]
async fn a_local_wakes_flight_is_separate_from_an_attended_start() {
    let w = World::new();
    save(&w.home(), wake_routine("triage", LOCAL));
    manual(&w.home(), "triage");
    w.drain().await;
    let barb = w.stub.last().session_id;
    let cwd = &w.stub.last().request.cwd;
    let human = wake::flight_key(&barb_key(), cwd, LOCAL);
    assert_eq!(w.env.flights.claim_start(&human, finish::now_ms()), Ok(()));
    assert!(w.stub.is_running(&barb));
}

#[tokio::test]
async fn an_attended_start_does_not_reserve_a_local_routines_flight() {
    let w = World::new();
    save(&w.home(), wake_routine("triage", LOCAL));
    let content = super::target::resolve(
        &w.home(),
        &alice(),
        &key("triage"),
        &service::get_current(&w.home(), &alice(), &key("triage"))
            .unwrap()
            .def,
    )
    .unwrap();
    let human = wake::flight_key(&barb_key(), &content, LOCAL);
    w.env.flights.claim_start(&human, finish::now_ms()).unwrap();
    manual(&w.home(), "triage");
    w.drain().await;
    assert_eq!(
        w.stub.calls(),
        1,
        "an untagged Start does not own this routine's flight"
    );
    assert!(w.typed().is_empty());
}

// ───────────────────────────── inbox_idle ─────────────────────────────

fn inbox_wake(id: &str, machine: &str) -> RoutineDef {
    let mut def = wake_routine(id, machine);
    def.task = "read your inbox and answer".into();
    def.triggers = vec![RoutineTrigger::Event(EventSpec {
        kind: "inbox_idle".into(),
        from_agent: None,
        role: None,
        metadata: None,
    })];
    def
}

fn idle_mail(session: &str, newest_id: i64) -> IdleMail {
    IdleMail {
        handle: format!("h-{session}"),
        name: "Barb".into(),
        unread: 1,
        oldest_unread_at: None,
        newest_id,
        inbox_org: ORG.into(),
        inbox_team: TEAM.into(),
        owner: Some(crate::commands::dashboard_launch::SessionOwner {
            principal: Some("1".into()),
            org: ORG.into(),
            team: Some(TEAM.into()),
        }),
        agent: Some(barb_key()),
        announced: None,
    }
}

#[tokio::test]
async fn mail_the_inbox_notice_told_barb_about_is_her_wake_routines_first_offer() {
    let w = World::new();
    w.barb(LOCAL, BARB, SimState::Idle);
    save(&w.home(), inbox_wake("inbox", LOCAL));
    for candidate in w.env.roster.lock().unwrap().iter_mut() {
        candidate.routine_key = Some(key("inbox").to_string());
    }
    let clock = Arc::new(AtomicI64::new(finish::now_ms()));
    let ticker = ticker(&w.home(), &clock, w.stub.clone());
    // Her inbox waiter just told her about mail 5 and she left it unread: the routine
    // does not type the same news into her seconds later.
    let mut told = idle_mail(BARB, 5);
    told.announced = Some(crate::weft::inbox::Announced {
        through_id: 5,
        at_ms: clock.load(Ordering::SeqCst),
        session: "s-told".into(),
    });
    let offered = ticker.offer_inbox_idle(&[told.clone()]).unwrap();
    w.drain().await;
    assert!(offered.admissions.is_empty(), "no duplicate notice");
    assert!(w.typed().is_empty());

    // Still unread a re-offer later: the routine nudges her, as it always has, and the
    // notice counted as one of its three wakes for that mail.
    let mut fired = 0;
    for _ in 0..4 {
        clock.fetch_add(REOFFER_MS.max(WINDOW_MS) + 1, Ordering::SeqCst);
        let offered = ticker.offer_inbox_idle(&[told.clone()]).unwrap();
        fired += offered
            .admissions
            .iter()
            .filter(|(_, a)| a.is_queued())
            .count();
        w.drain().await;
        w.local.set(BARB, SimState::Idle);
    }
    assert_eq!(fired, 2, "the notice and two wakes, then it stops");

    // Mail the notice has not told her about yet is news to the routine at once.
    let mut newer = idle_mail(BARB, 6);
    newer.announced = told.announced.clone();
    let offered = ticker.offer_inbox_idle(&[newer]).unwrap();
    assert_eq!(offered.admissions.len(), 1);
}

#[tokio::test]
async fn a_fresh_inbox_idle_routine_hears_mail_the_notice_already_told() {
    let w = World::new();
    let mut def = inbox_wake("sweeper", LOCAL);
    def.mode = "fresh".into();
    save(&w.home(), def);
    let clock = Arc::new(AtomicI64::new(finish::now_ms()));
    let ticker = ticker(&w.home(), &clock, w.stub.clone());
    // A fresh routine starts its own agent about idle sessions' mail: the session's
    // notice is not that agent's, so it still fires.
    let mut told = idle_mail("s-worker", 5);
    told.agent = Some(format!("{ORG}/{TEAM}/worker"));
    told.announced = Some(crate::weft::inbox::Announced {
        through_id: 5,
        at_ms: clock.load(Ordering::SeqCst),
        session: "s-told".into(),
    });
    let offered = ticker.offer_inbox_idle(&[told]).unwrap();
    assert_eq!(offered.admissions.len(), 1);
}

#[tokio::test]
async fn inbox_idle_on_barbs_own_wake_routine_wakes_her_directly() {
    let w = World::new();
    // Two idle Barbs: the one with the mail is woken, even if not the newest.
    w.barb(LOCAL, "s-mail", SimState::Idle);
    w.barb(LOCAL, "s-other", SimState::Idle);
    w.env.roster.lock().unwrap()[1].last_active_ms = 99;
    save(&w.home(), inbox_wake("inbox", LOCAL));
    for candidate in w.env.roster.lock().unwrap().iter_mut() {
        candidate.routine_key = Some(key("inbox").to_string());
    }
    let clock = Arc::new(AtomicI64::new(finish::now_ms()));
    let ticker = ticker(&w.home(), &clock, w.stub.clone());
    ticker.offer_inbox_idle(&[idle_mail("s-mail", 5)]).unwrap();
    w.drain().await;

    assert_eq!(w.stub.calls(), 0, "no middleman agent");
    let typed = w.typed();
    assert_eq!(typed.len(), 1, "{typed:?}");
    assert_eq!(typed[0].0, "s-mail");
}

#[tokio::test]
async fn inbox_idle_stops_after_three_ignored_wakes_and_raises_needs_you() {
    let w = World::new();
    w.barb(LOCAL, BARB, SimState::Idle);
    save(&w.home(), inbox_wake("inbox", LOCAL));
    for candidate in w.env.roster.lock().unwrap().iter_mut() {
        candidate.routine_key = Some(key("inbox").to_string());
    }
    let clock = Arc::new(AtomicI64::new(finish::now_ms()));
    let ticker = ticker(&w.home(), &clock, w.stub.clone());
    let mut fired = 0;
    for _ in 0..6 {
        let offered = ticker.offer_inbox_idle(&[idle_mail(BARB, 5)]).unwrap();
        fired += offered
            .admissions
            .iter()
            .filter(|(_, a)| a.is_queued())
            .count();
        w.drain().await;
        // Barb was woken and ignored the mail: it stays unread, she is idle.
        w.local.set(BARB, SimState::Idle);
        clock.fetch_add(REOFFER_MS.max(WINDOW_MS) + 1, Ordering::SeqCst);
    }
    assert_eq!(
        fired, 3,
        "three wakes for the same unread mail, then it stops"
    );
    assert_eq!(w.typed().len(), 3);
    let needs_you: Vec<RoutineRun> = runs_of(&w.home(), "inbox")
        .into_iter()
        .filter(|r| r.phase == phase::NEEDS_YOU)
        .collect();
    assert_eq!(needs_you.len(), 1, "raised once");
    // Newer mail is news again.
    let offered = ticker.offer_inbox_idle(&[idle_mail(BARB, 6)]).unwrap();
    assert_eq!(offered.admissions.len(), 1);
}

#[tokio::test]
async fn a_workers_mail_never_fires_barbs_wake_routine() {
    let w = World::new();
    w.barb(LOCAL, BARB, SimState::Idle);
    save(&w.home(), inbox_wake("inbox", LOCAL));
    for candidate in w.env.roster.lock().unwrap().iter_mut() {
        candidate.routine_key = Some(key("inbox").to_string());
    }
    let clock = Arc::new(AtomicI64::new(finish::now_ms()));
    let ticker = ticker(&w.home(), &clock, w.stub.clone());
    // A worker (another agent) and an ad-hoc tab (no saved agent) sit idle
    // with mail: neither is Barb's, so Barb's wake routine does not fire.
    let mut worker = idle_mail("s-worker", 5);
    worker.agent = Some(format!("{ORG}/{TEAM}/worker"));
    let mut ad_hoc = idle_mail("s-adhoc", 5);
    ad_hoc.agent = None;
    let offered = ticker.offer_inbox_idle(&[worker, ad_hoc]).unwrap();
    w.drain().await;
    assert!(offered.admissions.is_empty(), "no run admitted");
    assert!(runs_of(&w.home(), "inbox").is_empty(), "no run row");
    assert!(w.typed().is_empty(), "nothing typed into Barb");
    assert_eq!(w.stub.calls(), 0, "nothing launched");
    // Barb's own mail still wakes her.
    ticker.offer_inbox_idle(&[idle_mail(BARB, 6)]).unwrap();
    w.drain().await;
    let typed = w.typed();
    assert_eq!(typed.len(), 1, "{typed:?}");
    assert_eq!(typed[0].0, BARB);
}

#[tokio::test]
async fn a_fresh_inbox_idle_routine_still_hears_every_agents_mail() {
    let w = World::new();
    let mut def = inbox_wake("sweeper", LOCAL);
    def.mode = "fresh".into();
    save(&w.home(), def);
    let clock = Arc::new(AtomicI64::new(finish::now_ms()));
    let ticker = ticker(&w.home(), &clock, w.stub.clone());
    let mut worker = idle_mail("s-worker", 5);
    worker.agent = Some(format!("{ORG}/{TEAM}/worker"));
    let offered = ticker.offer_inbox_idle(&[worker]).unwrap();
    assert_eq!(offered.admissions.len(), 1, "team-wide, as before");
}

// ───────────────────────────── the default mode ─────────────────────────────

#[test]
fn a_new_routine_without_a_mode_gets_its_agents_default_written() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mut bypass =
        super::test_support::agent("bypass", codesense_pty_core::agent_cli::AgentCli::Claude);
    bypass.args = vec!["--dangerously-skip-permissions".into()];
    super::test_support::save_team_agent(&home, &alice(), bypass.clone());
    let mut opted =
        super::test_support::agent("opted", codesense_pty_core::agent_cli::AgentCli::Claude);
    opted.args = vec!["--dangerously-skip-permissions".into()];
    opted.allow_wake = true;
    super::test_support::save_team_agent(&home, &alice(), opted);
    let create = |id: &str, visible: bool, agent: &str, mode: &str| {
        let mut def = routine(id, "0 9 * * *");
        def.policy.visible = visible;
        def.agent = AgentRef::from(agent);
        def.mode = mode.into();
        service::save(
            &home,
            &alice(),
            SaveTarget::Create(Placement::Team {
                org: ORG.into(),
                team: TEAM.into(),
            }),
            def,
            &super::Author::user("alice"),
        )
        .unwrap()
    };
    let mode = |id: &str, visible: bool, agent: &str| create(id, visible, agent, "").def.mode;
    assert_eq!(mode("visible", true, super::test_support::AGENT), "wake");
    assert_eq!(mode("headless", false, super::test_support::AGENT), "fresh");
    assert_eq!(mode("starter", true, "builtin/claude"), "fresh");
    assert_eq!(
        mode("bypassed", true, "bypass"),
        "fresh",
        "no allowWake: fresh"
    );
    assert_eq!(mode("opted-in", true, "opted"), "wake");
    // Picking wake on it anyway saves, with a warning that its wakes are refused.
    let warned = create("forced", true, "bypass", "wake");
    assert!(warned
        .warnings
        .iter()
        .any(|w| w.code == crate::agents::launch_check::LaunchWarningCode::WakeNotAllowed));
}

#[test]
fn a_routine_stored_without_a_mode_runs_fresh() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mut def = routine("bare", "0 9 * * *");
    def.policy.visible = true;
    def.mode = String::new();
    assert_eq!(
        service::effective_mode(&home, &alice(), &key("bare"), &def),
        super::model::RoutineMode::Fresh,
        "only a create writes the agent's default"
    );
}

// ───────────────────────────── the inbox a notice goes to ─────────────────────────────

#[test]
fn a_notice_goes_to_the_routines_own_team_without_an_open_team() {
    let team_key = key("triage").to_string();
    assert_eq!(
        super::wake_app::notice_inbox(&team_key, None),
        Some((ORG.to_string(), TEAM.to_string()))
    );
    assert_eq!(
        super::wake_app::notice_inbox(&team_key, Some(("other".into(), "team".into()))),
        Some((ORG.to_string(), TEAM.to_string())),
        "the routine's team, not the one in view"
    );
    let personal = super::model::RoutineKey::personal(&alice(), ORG, "mine").to_string();
    assert_eq!(super::wake_app::notice_inbox(&personal, None), None);
}

// ───────────────────────────── candidates from the layouts ─────────────────────────────

fn tab(session: &str, tags: &[(&str, &str)], exited: bool) -> codesense_pty_core::layout::TabEntry {
    codesense_pty_core::layout::TabEntry {
        session: PtySessionId::new(session),
        name: session.into(),
        role: None,
        placement: codesense_pty_core::layout::Placement::Tab,
        state: if exited {
            codesense_pty_core::layout::EntryState::Exited {
                exit_code: Some(0),
                signal: None,
            }
        } else {
            codesense_pty_core::layout::EntryState::Live
        },
        tags: tags
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }
}

// MUST 4: a remote Barb this desktop never launched (not in its registry)
// is found from the machine's layout; a quit agent and an exited tab are not.
#[test]
fn candidates_come_from_every_machines_layout_not_this_desktops_registry() {
    use super::wake_app::{roster_from, HostTabs};
    let barb = barb_key();
    let revision = format!("{barb}@3");
    let hosts = vec![
        HostTabs {
            machine: REMOTE.into(),
            online: true,
            tabs: vec![
                tab(
                    "r-barb",
                    &[("remoteAgentKey", &barb), ("remoteHandle", "rh-1")],
                    false,
                ),
                tab("r-quit", &[("remoteAgentKey", &barb)], false),
                tab("r-done", &[("remoteAgentKey", &barb)], true),
                tab("r-shell", &[], false),
            ],
        },
        HostTabs {
            machine: LOCAL.into(),
            online: true,
            tabs: vec![tab(
                "l-barb",
                &[
                    ("launchAgentRevision", &revision),
                    ("launchHandle", "lh-1"),
                    ("launchRoutineKey", "acme/api/backend/triage"),
                ],
                false,
            )],
        },
        HostTabs {
            machine: "m-asleep".into(),
            online: false,
            tabs: vec![tab("o-barb", &[("remoteAgentKey", &barb)], false)],
        },
    ];
    let probe = |_: &str, session: &str| Some(session != "r-quit");
    let roster = roster_from(
        &hosts,
        &probe,
        &|session| (session == "l-barb").then_some(Activity::Idle),
        &|_| None,
        &super::wake_app::RosterRules::immediate(),
    );
    let got: Vec<(&str, &str, Activity, Option<&str>)> = roster
        .iter()
        .map(|c| {
            (
                c.machine.as_str(),
                c.session.as_str(),
                c.activity,
                c.routine_key.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            (REMOTE, "r-barb", Activity::Unknown, None),
            (REMOTE, "r-quit", Activity::Ended, None),
            (
                LOCAL,
                "l-barb",
                Activity::Idle,
                Some("acme/api/backend/triage")
            ),
            ("m-asleep", "o-barb", Activity::Offline, None),
        ]
    );
    assert!(roster
        .iter()
        .all(|c| c.agent.as_deref() == Some(barb.as_str())));
    assert_eq!(roster[0].handle, "rh-1");
    // It is the one a remote wake picks.
    let picked = pick(
        &roster,
        &WakeQuery {
            agent: &barb,
            routine_key: "acme/api/backend/triage",
            target: &TargetMachine::Machine(REMOTE.into()),
            preferred: &[],
        },
    );
    assert_eq!(picked, Pick::Deliver(roster[0].clone()));
}

// ───────────────────────────── review r2 ─────────────────────────────

// A: a probe that cannot answer (timed out, no pid, an older host without
// the process probe) is never "the agent quit": the tab stays a candidate.
#[test]
fn a_probe_that_cannot_answer_never_reads_as_a_quit_agent() {
    use super::wake_app::{roster_from, HostTabs};
    let barb = barb_key();
    let revision = format!("{barb}@3");
    let hosts = vec![
        HostTabs {
            machine: LOCAL.into(),
            online: true,
            tabs: vec![tab("l-barb", &[("launchAgentRevision", &revision)], false)],
        },
        HostTabs {
            machine: REMOTE.into(),
            online: true,
            tabs: vec![tab("r-barb", &[("remoteAgentKey", &barb)], false)],
        },
    ];
    let unknown = |_: &str, _: &str| None;
    let roster = roster_from(
        &hosts,
        &unknown,
        &|session| (session == "l-barb").then_some(Activity::Idle),
        &|_| None,
        &super::wake_app::RosterRules::immediate(),
    );
    let activity: Vec<Activity> = roster.iter().map(|c| c.activity).collect();
    assert_eq!(activity, vec![Activity::Idle, Activity::Unknown]);
}

// ───────────────────────────── SW-AGR-2b: carried from the r3 review ─────────────────────────────

// The desktop's quit check takes the server's grace: a tab whose agent left
// its prompt a moment ago (a re-exec, `/exit` + `claude --resume`) is not
// absent until it has been gone for AGENT_QUIT_GRACE_MS.
#[test]
fn a_quit_agent_reads_as_quit_only_after_the_servers_grace() {
    use super::wake::{QuitState, QuitWatch, AGENT_QUIT_GRACE_MS};
    use super::wake_app::{roster_from, HostTabs, RosterRules};
    assert_eq!(
        AGENT_QUIT_GRACE_MS,
        codesense_session_server::session_wake::AGENT_QUIT_GRACE_MS as i64
    );
    let watch = QuitWatch::default();
    let now = AtomicI64::new(10_000);
    let quit = |machine: &str, session: &str, absent: bool| {
        watch.observe(machine, session, absent, now.load(Ordering::SeqCst))
    };
    let rules = RosterRules::of(None, &quit);
    let barb = barb_key();
    let hosts = vec![HostTabs {
        machine: REMOTE.into(),
        online: true,
        tabs: vec![tab("r-barb", &[("remoteAgentKey", &barb)], false)],
    }];
    let gone = Mutex::new(true);
    let probe = |_: &str, _: &str| Some(!*gone.lock().unwrap());
    let read =
        || roster_from(&hosts, &probe, &|_| Some(Activity::Idle), &|_| None, &rules)[0].activity;
    assert_eq!(read(), Activity::Quitting, "just left its prompt");
    let query_target = TargetMachine::Machine(REMOTE.into());
    let only = roster_from(&hosts, &probe, &|_| Some(Activity::Idle), &|_| None, &rules);
    assert_eq!(pick(&only, &query(&barb, &query_target)), Pick::Quitting);
    now.fetch_add(AGENT_QUIT_GRACE_MS - 1, Ordering::SeqCst);
    assert_eq!(read(), Activity::Quitting);
    // Back at its prompt (`claude --resume`): present again, grace reset.
    *gone.lock().unwrap() = false;
    assert_eq!(read(), Activity::Idle);
    *gone.lock().unwrap() = true;
    now.fetch_add(1, Ordering::SeqCst);
    assert_eq!(read(), Activity::Quitting, "the grace starts over");
    now.fetch_add(AGENT_QUIT_GRACE_MS, Ordering::SeqCst);
    assert_eq!(read(), Activity::Ended, "gone for the whole grace: quit");
    assert_eq!(watch.observe(REMOTE, "other", false, 0), QuitState::Present);
}

// A wake that finds only an agent in its quit grace looks again shortly; it
// neither launches nor types.
#[tokio::test]
async fn a_wake_on_an_agent_in_its_quit_grace_defers() {
    let w = World::new();
    w.env
        .roster
        .lock()
        .unwrap()
        .push(candidate(REMOTE, BARB, &barb_key(), Activity::Quitting));
    save(&w.home(), wake_routine("triage", REMOTE));
    manual(&w.home(), "triage");
    w.drain().await;
    assert!(w.remote_calls().is_empty());
    assert!(w.typed().is_empty());
    assert_eq!(w.take_redispatched().len(), 1, "looked at again shortly");
}

// Shared machines: a remote tab tagged as launched under another account is
// never woken; an untagged one (an older desktop) is this account's, since a
// machine's private terminal server admits only its owner.
#[test]
fn a_remote_tab_another_account_launched_is_never_a_candidate() {
    use super::wake_app::{roster_from, HostTabs, RosterRules, TAG_REMOTE_OWNER};
    let barb = barb_key();
    let hosts = vec![HostTabs {
        machine: REMOTE.into(),
        online: true,
        tabs: vec![
            tab(
                "r-mine",
                &[("remoteAgentKey", &barb), (TAG_REMOTE_OWNER, "41")],
                false,
            ),
            tab(
                "r-teammate",
                &[("remoteAgentKey", &barb), (TAG_REMOTE_OWNER, "77")],
                false,
            ),
            tab("r-untagged", &[("remoteAgentKey", &barb)], false),
        ],
    }];
    let present = |_: &str, _: &str, _: bool| super::wake::QuitState::Present;
    let rules = RosterRules::of(Some("41"), &present);
    let roster = roster_from(&hosts, &|_, _| Some(true), &|_| None, &|_| None, &rules);
    let sessions: Vec<&str> = roster.iter().map(|c| c.session.as_str()).collect();
    assert_eq!(sessions, vec!["r-mine", "r-untagged"]);
}

// `machine: any` and a machine the person hid or never set up: its unread
// layout does not hold an absent agent's launch back; a wake naming that
// machine still waits for it.
#[tokio::test]
async fn a_hidden_machine_does_not_hold_back_an_any_wake() {
    let w = World::new();
    *w.env.unreadable.lock().unwrap() = vec!["m-hidden".to_string()];
    *w.env.ignorable.lock().unwrap() = vec!["m-hidden".to_string()];
    save(&w.home(), wake_routine("triage", "any"));
    manual(&w.home(), "triage");
    w.drain().await;
    assert_eq!(w.stub.calls(), 1, "launched here");
    assert!(w.take_redispatched().is_empty(), "never deferred for it");
    drop(w);

    let w = World::new();
    w.env.known.lock().unwrap().push("m-hidden".into());
    *w.env.unreadable.lock().unwrap() = vec!["m-hidden".to_string()];
    *w.env.ignorable.lock().unwrap() = vec!["m-hidden".to_string()];
    save(&w.home(), wake_routine("triage", "m-hidden"));
    manual(&w.home(), "triage");
    w.drain().await;
    assert!(w.remote_calls().is_empty(), "named: waits to read it");
    assert_eq!(w.take_redispatched().len(), 1);
}

// SW-AGR-12: the routine's workspace on the machine is chosen from the
// durable preparation receipts (`remote-workspace-intents-v1`), never from the
// viewer: a workspace closed in the UI, or a fire before the frontend has
// published anything, still finds it. Never a path on this Mac.
struct ReceiptWorld {
    _dir: tempfile::TempDir,
    places: Vec<crate::mcp::remote_workspace_target::PreparedPlace>,
    handles: std::collections::BTreeMap<String, String>,
}

const RECEIPT_TEAM: &str = "acme/api:backend";

/// (workspace, team, machine, server, owner, repo, worktree, recorded kind)
type ReceiptRow<'a> = (
    &'a str,
    &'a str,
    &'a str,
    &'a str,
    u64,
    (&'a str, &'a str),
    String,
    Option<&'a str>,
);

fn receipt_world(rows: &[ReceiptRow]) -> ReceiptWorld {
    use crate::mcp::remote_workspace_target::{prepared_places, write_test_receipt};
    let dir = tempfile::tempdir().unwrap();
    let principal = crate::domain::Principal::from(41);
    let root = crate::context_repo::path_to_user_home(dir.path(), &principal)
        .join("remote-workspace-intents-v1");
    std::fs::create_dir_all(&root).unwrap();
    let mut handles = std::collections::BTreeMap::new();
    for (i, (id, team_key, machine, server, owner, (o, r), cwd, kind)) in rows.iter().enumerate() {
        let handle = write_test_receipt(
            &root,
            &format!("{i:08x}-0000-4000-8000-000000000000"),
            machine,
            team_key,
            id,
            crate::domain::SourceRepoSlug::new(*o, *r).unwrap(),
            cwd,
            *owner,
            server,
            *kind,
        );
        handles.insert(id.to_string(), handle);
    }
    // No viewer anywhere: what is on disk is enough (a fire right after start).
    let places = prepared_places(dir.path(), &principal, 41, REMOTE).unwrap();
    ReceiptWorld {
        _dir: dir,
        places,
        handles,
    }
}

fn story_dir() -> String {
    use sha2::{Digest, Sha256};
    let digest = format!("{:x}", Sha256::digest(RECEIPT_TEAM.as_bytes()));
    format!(
        "/home/dev/codesense/worktrees/acme-api-backend-{}",
        &digest[..6]
    )
}

fn any_repo(_: &crate::domain::SourceRepoSlug) -> bool {
    true
}

#[test]
fn a_routines_workspace_on_a_machine_is_chosen_from_its_receipts() {
    use super::model::RoutineTarget;
    use super::wake_app::{host_workspace, HostWorkspace};
    let team = RECEIPT_TEAM;
    let story = story_dir();
    let w = receipt_world(&[
        // On another machine.
        (
            "elsewhere",
            team,
            "m-other",
            "srv",
            41,
            ("acme", "api"),
            "/home/dev/other".into(),
            None,
        ),
        // A story's worktree there (an older receipt, no kind): that story's
        // work, never a routine's.
        (
            "story-12",
            team,
            REMOTE,
            "srv",
            41,
            ("acme", "api"),
            format!("{story}/SW-12"),
            None,
        ),
        // A newer receipt says it is a story's, wherever it lives.
        (
            "story-13",
            team,
            REMOTE,
            "srv",
            41,
            ("acme", "api"),
            "/home/dev/work/s13".into(),
            Some("story"),
        ),
        (
            "api-main",
            team,
            REMOTE,
            "srv",
            41,
            ("Acme", "API"),
            "/home/dev/work/api".into(),
            Some("generic"),
        ),
        // Prepared by a terminal server the machine no longer runs.
        (
            "api-old",
            team,
            REMOTE,
            "srv-gone",
            41,
            ("acme", "api"),
            "/home/dev/work/old".into(),
            None,
        ),
        // Another account's on a shared machine.
        (
            "api-theirs",
            team,
            REMOTE,
            "srv",
            7,
            ("acme", "api"),
            "/home/dev/work/theirs".into(),
            None,
        ),
        (
            "web",
            team,
            REMOTE,
            "srv",
            41,
            ("acme", "web"),
            "/home/dev/work/web".into(),
            None,
        ),
        (
            "other-team",
            "acme/api:other",
            REMOTE,
            "srv",
            41,
            ("acme", "ops"),
            "/home/dev/work/ops".into(),
            None,
        ),
    ]);
    let places = &w.places;
    assert!(places.iter().all(|p| p.machine_id == REMOTE));
    assert!(
        places.iter().all(|p| p.workspace_id != "api-theirs"),
        "only this account's receipts"
    );
    let api = RoutineTarget::Repo("acme/api".into());
    assert_eq!(
        host_workspace(places, team, REMOTE, "srv", &any_repo, &api),
        Ok(vec![HostWorkspace {
            workspace_id: "api-main".into(),
            handle: w.handles["api-main"].clone(),
            cwd: "/home/dev/work/api".into(),
        }]),
        "the repo's own workspace on the live server, never a story's"
    );
    // Only stories' workspaces there: refused, never launched into; the copy
    // says what to do.
    let stories: Vec<_> = places
        .iter()
        .filter(|p| p.workspace_id.starts_with("story-"))
        .cloned()
        .collect();
    let refused = host_workspace(&stories, team, REMOTE, "srv", &any_repo, &api).unwrap_err();
    assert!(
        refused.contains("prepare_remote_workspace") && refused.contains("story"),
        "{refused}"
    );
    assert_eq!(
        host_workspace(
            places,
            team,
            REMOTE,
            "srv",
            &any_repo,
            &RoutineTarget::Repo("acme/web".into())
        )
        .map(|w| w[0].cwd.clone()),
        Ok("/home/dev/work/web".into())
    );
    // `host/owner/repo` names it on that host only.
    assert_eq!(
        host_workspace(
            places,
            team,
            REMOTE,
            "srv",
            &any_repo,
            &RoutineTarget::Repo("github.com/acme/web".into())
        )
        .map(|w| w[0].workspace_id.clone()),
        Ok("web".into())
    );
    assert!(host_workspace(
        places,
        team,
        REMOTE,
        "srv",
        &any_repo,
        &RoutineTarget::Repo("gitlab.com/acme/web".into())
    )
    .is_err());
    let missing = host_workspace(
        places,
        team,
        REMOTE,
        "srv",
        &any_repo,
        &RoutineTarget::Repo("acme/ops".into()),
    )
    .unwrap_err();
    assert!(
        missing.contains("acme/ops") && missing.contains(REMOTE),
        "{missing}"
    );
    let none = host_workspace(places, team, "m-bare", "srv", &any_repo, &api).unwrap_err();
    assert!(none.contains("m-bare"), "{none}");
    // The stale server's receipt is never a candidate.
    let stale_only: Vec<_> = places
        .iter()
        .filter(|p| p.workspace_id == "api-old")
        .cloned()
        .collect();
    assert!(host_workspace(&stale_only, team, REMOTE, "srv", &any_repo, &api).is_err());
    // A team-workspace routine: the team's workspace there, when there is
    // exactly one choice of (configured) repo; else it must name its repo.
    let team_ws = host_workspace(
        places,
        team,
        REMOTE,
        "srv",
        &any_repo,
        &RoutineTarget::TeamWorkspace,
    )
    .unwrap_err();
    assert!(team_ws.contains("repo:"), "{team_ws}");
    // A repo the team no longer configures does not count (nor block).
    let only_api = |repo: &crate::domain::SourceRepoSlug| repo.repo.eq_ignore_ascii_case("api");
    assert_eq!(
        host_workspace(
            places,
            team,
            REMOTE,
            "srv",
            &only_api,
            &RoutineTarget::TeamWorkspace
        )
        .map(|w| w.into_iter().map(|w| w.workspace_id).collect::<Vec<_>>()),
        Ok(vec!["api-main".to_string()])
    );
    assert!(host_workspace(
        places,
        team,
        REMOTE,
        "srv",
        &only_api,
        &RoutineTarget::Repo("acme/web".into())
    )
    .is_err());
    // No receipts at all (nothing prepared on this Mac yet): an empty list.
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(
        crate::mcp::remote_workspace_target::prepared_places(
            empty.path(),
            &crate::domain::Principal::from(41),
            41,
            REMOTE
        ),
        Ok(vec![])
    );
}

// SW-AGR-12 (review r1 MUST 1): a stale receipt (its worktree gone on the
// machine) never blocks the routine, even when it sorts first: each
// candidate goes through the receipt gate in order and the first the
// machine confirms is the one. When none is confirmed, the detail names each
// workspace and worktree that failed.
#[tokio::test]
async fn a_stale_receipt_sorting_first_never_blocks_the_live_one() {
    use super::model::RoutineTarget;
    use super::wake_app::{choose_place, PlaceError};
    let team = RECEIPT_TEAM;
    let w = receipt_world(&[
        (
            "aaa-gone",
            team,
            REMOTE,
            "srv",
            41,
            ("acme", "api"),
            "/home/dev/work/gone".into(),
            None,
        ),
        // Re-prepared under the same id at a new path.
        (
            "api-main",
            team,
            REMOTE,
            "srv",
            41,
            ("acme", "api"),
            "/home/dev/work/api-old".into(),
            None,
        ),
        (
            "api-main",
            team,
            REMOTE,
            "srv",
            41,
            ("acme", "api"),
            "/home/dev/work/api".into(),
            None,
        ),
    ]);
    let api = RoutineTarget::Repo("acme/api".into());
    let live = ["/home/dev/work/api"];
    let asked = std::sync::Mutex::new(Vec::new());
    let place = choose_place(&w.places, team, REMOTE, Some("srv"), &any_repo, &api, |c| {
        asked.lock().unwrap().push(c.workspace_id.clone());
        let ok = live.contains(&c.cwd.as_str());
        async move {
            if ok {
                Ok(())
            } else {
                Err("remote workspace is not authoritatively prepared".to_string())
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(place.cwd, "/home/dev/work/api");
    assert_eq!(
        asked.lock().unwrap()[0],
        "aaa-gone",
        "the stale one sorts first and is passed over"
    );
    // None confirmed: needs preparation, naming what failed.
    let none = choose_place(
        &w.places,
        team,
        REMOTE,
        Some("srv"),
        &any_repo,
        &api,
        |_| async {
            Err::<(), _>(
                "no current prepared workspace matches this owner, team, machine and handle"
                    .to_string(),
            )
        },
    )
    .await
    .unwrap_err();
    let PlaceError::NeedsPreparation(detail) = &none else {
        panic!("{none:?}")
    };
    assert!(
        detail.contains("aaa-gone at /home/dev/work/gone")
            && detail.contains("api-main at /home/dev/work/api")
            && detail.contains("prepare_remote_workspace"),
        "{detail}"
    );
    assert_eq!(
        none.reason(),
        codesense_pty_core::session_wake::UndeliveredReason::NeedsPreparation
    );
    // Any other failure (a timeout, a retired lease) says nothing about the
    // receipt: it stops there, unavailable, and asks no further candidate.
    let asked = std::sync::Mutex::new(0);
    let stopped = choose_place(
        &w.places,
        team,
        REMOTE,
        Some("srv"),
        &any_repo,
        &api,
        |_| {
            *asked.lock().unwrap() += 1;
            async { Err::<(), _>("private remote authority retired".to_string()) }
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(stopped, PlaceError::Unavailable(_)), "{stopped:?}");
    assert!(stopped.detail().contains("aaa-gone"), "{stopped:?}");
    assert_eq!(*asked.lock().unwrap(), 1);
    assert_eq!(
        stopped.reason(),
        codesense_pty_core::session_wake::UndeliveredReason::Unavailable
    );
}

// SW-AGR-12 (review r1 MUST 2): right after start the machine's terminal
// server is not known yet: the launch is machine-offline (tried again at the
// next fire), never a guess at a dead server's receipt nor needs-preparation.
#[tokio::test]
async fn a_machine_not_connected_yet_is_offline_not_unprepared() {
    use super::model::RoutineTarget;
    use super::wake_app::{choose_place, PlaceError};
    let team = RECEIPT_TEAM;
    let w = receipt_world(&[(
        "api-main",
        team,
        REMOTE,
        "srv",
        41,
        ("acme", "api"),
        "/home/dev/work/api".into(),
        None,
    )]);
    let asked = std::cell::Cell::new(false);
    let offline = choose_place(
        &w.places,
        team,
        REMOTE,
        None,
        &any_repo,
        &RoutineTarget::Repo("acme/api".into()),
        |_| {
            asked.set(true);
            async { Ok::<(), String>(()) }
        },
    )
    .await
    .unwrap_err();
    assert!(
        !asked.get(),
        "nothing is asked of a machine that is not connected"
    );
    assert!(matches!(offline, PlaceError::Offline(_)), "{offline:?}");
    assert_eq!(
        offline.reason(),
        codesense_pty_core::session_wake::UndeliveredReason::MachineOffline
    );
}

// What the machine's tab open answers, as a routine launch reads it: an
// unconfirmed answer is never "nothing started", and a found tab is the
// agent's (attached elsewhere included).
#[test]
fn a_remote_tab_answer_reads_as_opened_unknown_or_not_started() {
    use super::wake_app::{tab_reply, TabAnswer};
    let ask = |status: &'static str,
               code: Option<&'static str>,
               tab: bool,
               retryable: bool,
               attach: bool| {
        tab_reply(&TabAnswer {
            status,
            code,
            message: Some("why"),
            session: tab.then_some("s"),
            handle: tab.then_some("h"),
            retryable,
            attach,
        })
    };
    let answer = |status: &'static str, code: Option<&'static str>, tab: bool, retryable: bool| {
        ask(status, code, tab, retryable, false)
    };
    // An Attach refused for who asked or how says nothing about the launch.
    for code in ["accessDenied", "invalid"] {
        assert!(
            matches!(
                ask("refused", Some(code), false, false, true),
                TabReply::Unknown(_)
            ),
            "{code}"
        );
        assert!(
            matches!(
                ask("refused", Some(code), false, false, false),
                TabReply::NotStarted { .. }
            ),
            "{code} on a Create"
        );
    }
    assert!(matches!(
        ask("refused", Some("workspaceUnavailable"), false, false, true),
        TabReply::NotStarted { .. }
    ));
    assert!(matches!(
        answer("pending", Some("launching"), false, false),
        TabReply::Pending(_)
    ));
    let opened = TabReply::Opened {
        session: "s".into(),
        handle: "h".into(),
    };
    for status in ["opened", "alreadyLive"] {
        assert_eq!(answer(status, None, true, false), opened, "{status}");
    }
    assert_eq!(
        answer("refused", Some("attachedElsewhere"), true, false),
        opened
    );
    assert_eq!(
        answer("ended", None, true, false),
        TabReply::Ended {
            session: "s".into(),
            handle: "h".into(),
        }
    );
    assert!(matches!(
        answer("unknown", Some("timedOut"), false, false),
        TabReply::Unknown(_)
    ));
    assert!(matches!(
        answer("opened", None, false, false),
        TabReply::Unknown(_)
    ));
    // Only a definitive refusal frees anything: a retryable one may still
    // spawn, unless its launch can never spawn now.
    assert!(matches!(
        answer("refused", Some("spawnUnconfirmed"), false, true),
        TabReply::Unknown(_)
    ));
    let not =
        |code: &'static str, retryable: bool| match answer("refused", Some(code), false, retryable)
        {
            TabReply::NotStarted { reason, detail } => {
                assert!(detail.contains(code), "{detail}");
                reason
            }
            other => panic!("{code}: {other:?}"),
        };
    assert_eq!(
        not("workspaceUnavailable", false),
        UndeliveredReason::NeedsPreparation
    );
    assert_eq!(
        not("preparationIncomplete", true),
        UndeliveredReason::NeedsPreparation
    );
    assert_eq!(
        not("newIntentRequired", true),
        UndeliveredReason::NeedsPreparation
    );
    assert_eq!(not("unavailable", false), UndeliveredReason::MachineOffline);
    assert_eq!(
        not("agentCliMissing", false),
        UndeliveredReason::Unavailable
    );
}

// A: an older remote server (no process probe) with `machine: any`: its Barb
// is woken there, never launched again here.
#[tokio::test]
async fn an_unprobeable_remote_barb_with_any_is_woken_not_duplicated() {
    let w = World::new();
    w.remote.set(BARB, SimState::Busy);
    w.env
        .roster
        .lock()
        .unwrap()
        .push(candidate(REMOTE, BARB, &barb_key(), Activity::Unknown));
    save(&w.home(), wake_routine("triage", "any"));
    manual(&w.home(), "triage");
    w.drain().await;
    assert_eq!(w.stub.calls(), 0, "no launch here");
    assert_eq!(w.remote.queued(), 1, "queued for the remote Barb");
}

// B: a machine in the target whose sessions could not be read may run the
// agent unseen: the wake defers, never launches.
#[tokio::test]
async fn an_unreadable_machine_in_the_target_defers_instead_of_launching() {
    for (target, unreadable) in [(LOCAL, LOCAL), ("any", REMOTE), ("any", LOCAL)] {
        let w = World::new();
        *w.env.unreadable.lock().unwrap() = vec![unreadable.to_string()];
        save(&w.home(), wake_routine("triage", target));
        manual(&w.home(), "triage");
        w.drain().await;
        assert_eq!(w.stub.calls(), 0, "{target}/{unreadable}: no launch");
        assert_eq!(
            only_run(&w.home(), "triage").wake_machine.as_deref(),
            Some(wake::DEFERRED)
        );
        assert_eq!(w.take_redispatched().len(), 1);
    }
    // A machine outside the target does not hold a local wake back.
    let w = World::new();
    *w.env.unreadable.lock().unwrap() = vec![REMOTE.to_string()];
    save(&w.home(), wake_routine("triage", LOCAL));
    manual(&w.home(), "triage");
    w.drain().await;
    assert_eq!(w.stub.calls(), 1);
}

#[test]
fn unreadable_machines_are_the_unavailable_the_missing_and_a_dead_local_sidecar() {
    use super::wake_app::{unreadable_machines, LayoutRead};
    let read = vec![
        (LOCAL.to_string(), true, LayoutRead::Live),
        ("m-up".to_string(), false, LayoutRead::Live),
        ("m-asleep".to_string(), false, LayoutRead::Historical),
        ("m-broken".to_string(), false, LayoutRead::Unavailable),
    ];
    let expected = vec![
        LOCAL.to_string(),
        "m-up".into(),
        "m-asleep".into(),
        "m-broken".into(),
        "m-silent".into(),
    ];
    assert_eq!(
        unreadable_machines(&read, &expected),
        vec!["m-broken".to_string(), "m-silent".to_string()]
    );
    // This Mac with only an in-process host (its session server not live).
    let local_dead = vec![(LOCAL.to_string(), false, LayoutRead::Live)];
    assert_eq!(
        unreadable_machines(&local_dead, &[LOCAL.to_string()]),
        vec![LOCAL.to_string()]
    );
    assert_eq!(
        unreadable_machines(&[], &[LOCAL.to_string()]),
        vec![LOCAL.to_string()]
    );
}

// C: a launch that settled after a fire read its candidates is never
// launched over: the fire waits, then sees the session.
#[test]
fn a_fresh_launch_makes_the_next_fire_wait_never_launch_again() {
    let flights = LaunchFlights::default();
    let key = wake::flight_key("acme/api/backend/barb", Path::new("/no/such/dir"), LOCAL);
    assert_eq!(flights.begin(&key, 0), wake::Begin::Go);
    // A second fire that read its candidates before the first settled.
    assert_eq!(flights.begin(&key, 10), wake::Begin::Wait);
    flights.finish(&key, Some(("s-new".into(), "h-new".into())), 20);
    assert_eq!(
        flights.begin(&key, 30),
        wake::Begin::Wait,
        "a just-made launch is waited for"
    );
    assert_eq!(flights.launched(30).len(), 1, "and offered as live");
    // A person's Start made it: the same.
    let human = wake::flight_key("acme/api/backend/bubbles", Path::new("/no/such/dir"), LOCAL);
    flights.claim_start(&human, 0).unwrap();
    flights.finish(&human, Some(("s-human".into(), "s-human".into())), 5);
    assert_eq!(flights.begin(&human, 6), wake::Begin::Wait);
    // Past the grace it blocks nothing.
    assert_eq!(
        flights.begin(&key, 20 + wake::LAUNCH_GRACE_MS),
        wake::Begin::Go
    );
}

// D: a reservation that never bound stops blocking after the stale window.
#[test]
fn a_stale_reservation_is_not_a_starting_agent() {
    use super::wake_app::starting;
    let revision = format!("{}@1", barb_key());
    let fresh = starting("h", Some(&revision), None, false, Some(1_000), 2_000);
    assert_eq!(fresh.map(|c| c.activity), Some(Activity::Starting));
    assert!(starting(
        "h",
        Some(&revision),
        None,
        false,
        Some(1_000),
        1_000 + wake::IN_FLIGHT_STALE_MS
    )
    .is_none());
    assert!(starting("h", None, None, false, Some(1_000), 2_000).is_none());
    assert!(starting("h", Some(&revision), None, false, None, 2_000).is_none());
}

// Attended Starts retain canonical directory coordination independently of wakes.
#[tokio::test]
async fn an_attended_start_keeps_its_canonical_directory_flight() {
    let w = World::new();
    save(&w.home(), wake_routine("triage", LOCAL));
    let def = service::get_current(&w.home(), &alice(), &key("triage"))
        .unwrap()
        .def;
    let dir = super::target::resolve(&w.home(), &alice(), &key("triage"), &def).unwrap();
    let spelled = PathBuf::from(format!("{}/.", dir.display()));
    let human = wake::flight_key(&barb_key(), &spelled, LOCAL);
    assert_ne!(human, team_flight(&w.home(), "triage"));
    assert_eq!(
        human.1,
        std::fs::canonicalize(&dir).unwrap().to_string_lossy(),
        "the canonical team workspace dir"
    );
    w.env.flights.claim_start(&human, finish::now_ms()).unwrap();
    w.env.flights.finish(
        &human,
        Some(("s-human".into(), "s-human".into())),
        finish::now_ms(),
    );
    manual(&w.home(), "triage");
    w.drain().await;
    assert_eq!(w.stub.calls(), 1);
    assert_ne!(
        only_run(&w.home(), "triage").session_id.as_deref(),
        Some("s-human")
    );
}

// Exercise the actual Agents-tab preparation and roster conversion. An
// untagged session stays separate even when its saved agent and directory match.
#[tokio::test]
async fn an_untagged_agents_tab_start_is_not_adopted_by_a_local_wake() {
    let w = World::new();
    save(&w.home(), wake_routine("triage", LOCAL));
    let def = service::get_current(&w.home(), &alice(), &key("triage"))
        .unwrap()
        .def;
    let routine_dir = super::target::resolve(&w.home(), &alice(), &key("triage"), &def).unwrap();
    let barb = crate::agents::AgentKey::parse(&barb_key()).unwrap();
    let launch = crate::agents::launch::prepare(
        &w.home(),
        &alice(),
        &barb,
        crate::agents::launch::PrepareOptions {
            target_team: Some((ORG.into(), TEAM.into())),
            mode_override: Some(crate::agents::adapter::LaunchMode::Interactive),
            ..Default::default()
        },
    )
    .unwrap();
    let session = format!("agents-tab-start-{}", uuid::Uuid::new_v4());
    // The dir as the tab hands it over: not canonical.
    let asked = format!("{}/.", routine_dir.display());
    let (prepared, _scope, _settings) = crate::commands::agents_launch::prepare_product_session(
        launch,
        "/fake/claude".into(),
        session.clone(),
        asked.clone(),
        None,
        Some(crate::agents::product_bootstrap::BootstrapResources {
            sink: Arc::new(crate::commands::worktree::NullEventSink),
        }),
        true,
    )
    .await
    .unwrap();
    let recorded = crate::commands::agents_launch::recorded_launch_dir(&prepared, &asked);
    crate::commands::mcp_scope::remove_scope_config(&session);
    assert!(
        recorded.canonicalize().unwrap() == routine_dir.canonicalize().unwrap(),
        "{} vs {}",
        recorded.display(),
        routine_dir.display()
    );
    use super::wake_app::{roster_from, HostTabs, RosterRules};
    let revision = format!("{}@1", barb_key());
    let hosts = vec![HostTabs {
        machine: LOCAL.into(),
        online: true,
        tabs: vec![tab(
            &session,
            &[
                ("launchAgentRevision", &revision),
                ("launchHandle", "lh-start"),
            ],
            false,
        )],
    }];
    *w.env.roster.lock().unwrap() = roster_from(
        &hosts,
        &|_, _| Some(true),
        &|_| Some(Activity::Idle),
        &|handle| (handle == "lh-start").then(|| (1, false)),
        &RosterRules::immediate(),
    );
    w.local.set(&session, SimState::Idle);
    manual(&w.home(), "triage");
    w.drain().await;
    assert_eq!(
        w.stub.calls(),
        1,
        "an untagged Start is not this routine's session"
    );
    assert!(w.typed().is_empty());
    assert_eq!(
        w.stub.last().request.provenance.routine_key.as_deref(),
        Some(key("triage").to_string().as_str())
    );
}

// ───────────────────────────── resolution ─────────────────────────────

fn query<'a>(agent: &'a str, target: &'a TargetMachine) -> WakeQuery<'a> {
    WakeQuery {
        agent,
        routine_key: "acme/api/backend/triage",
        target,
        preferred: &[],
    }
}

#[test]
fn resolution_never_picks_headless_runs_other_agents_or_other_machines() {
    let agent = "acme/api/backend/barb";
    let mut headless = candidate(LOCAL, "s-headless", agent, Activity::Idle);
    headless.headless = true;
    let other_agent = candidate(
        LOCAL,
        "s-bubbles",
        "acme/api/backend/bubbles",
        Activity::Idle,
    );
    let elsewhere = candidate(REMOTE, "s-remote", agent, Activity::Idle);
    let ended = candidate(LOCAL, "s-ended", agent, Activity::Ended);
    let all = vec![headless, other_agent, elsewhere.clone(), ended];
    let q = |target: &TargetMachine| pick(&all, &query(agent, target));
    assert_eq!(q(&TargetMachine::Local), Pick::Absent);
    assert_eq!(q(&TargetMachine::Any), Pick::Deliver(elsewhere.clone()));
    assert_eq!(
        q(&TargetMachine::Machine(REMOTE.into())),
        Pick::Deliver(elsewhere)
    );
}

#[test]
fn resolution_prefers_idle_then_the_most_recent() {
    let agent = "acme/api/backend/barb";
    let mut busy_new = candidate(LOCAL, "busy-new", agent, Activity::Busy);
    busy_new.last_active_ms = 50;
    let mut idle_old = candidate(LOCAL, "idle-old", agent, Activity::Idle);
    idle_old.last_active_ms = 10;
    let mut idle_new = candidate(LOCAL, "idle-new", agent, Activity::Idle);
    idle_new.last_active_ms = 20;
    let picked = pick(
        &[busy_new, idle_old, idle_new.clone()],
        &query(agent, &TargetMachine::Local),
    );
    assert_eq!(picked, Pick::Deliver(idle_new));
}

#[test]
fn local_resolution_requires_the_routines_launch_tag() {
    let agent = "acme/api/backend/barb";
    let target = TargetMachine::Local;
    let query = query(agent, &target);
    let mut live = candidate(LOCAL, "s", agent, Activity::Idle);
    live.routine_key = None;
    assert_eq!(pick(&[live.clone()], &query), Pick::Absent);
    live.routine_key = Some(key("other").to_string());
    assert_eq!(pick(&[live.clone()], &query), Pick::Absent);
    live.routine_key = Some(key("triage").to_string());
    assert_eq!(pick(&[live.clone()], &query), Pick::Deliver(live));
}

// A launch reserved but not bound yet is waited for, never doubled.
#[test]
fn a_starting_launch_is_waited_for() {
    let agent = "acme/api/backend/barb";
    let mut starting = candidate(LOCAL, "", agent, Activity::Starting);
    starting.handle = "h-start".into();
    assert_eq!(
        pick(&[starting], &query(agent, &TargetMachine::Local)),
        Pick::Starting
    );
}

use super::wake::pick;
mod invariants;

// ───────────────────── SW-AGR-13: per-routine wake limits ─────────────────────

/// A routine for heavy work: three wakes in all, two hours apart, skipped
/// inside the gap.
fn heavy_routine(machine: &str) -> RoutineDef {
    let mut def = wake_routine("heavy", machine);
    def.policy.max_wakes = Some(3);
    def.policy.min_gap = Some("2h".into());
    def.policy.overlap = super::model::OverlapPolicy::Skip;
    def
}

fn install(w: &World) -> String {
    super::host::local_host_id(&w.home()).unwrap()
}

fn decide(w: &World, machine: &str, reason: UndeliveredReason, notice: Option<&str>) {
    *w.sim(machine).decide.lock().unwrap() = Some((
        DeliveryStatus::Undelivered { reason },
        notice.map(str::to_string),
    ));
}

fn limits_sent(w: &World, machine: &str) -> Vec<Option<WakeLimits>> {
    w.sim(machine)
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|r| r.limits.clone())
        .collect()
}

// AC6 + AC3: a routine's limits ride on every wake to the terminal server
// that runs the session, local or remote, which enforces them; a routine
// without limits sends none (an older server is never asked to ignore them).
async fn limits_ride_on_the_wake(machine: &str) {
    let w = World::new();
    w.barb(machine, BARB, SimState::Idle);
    w.env.roster.lock().unwrap()[0].routine_key = Some(key("heavy").to_string());
    if machine == LOCAL {
        w.barb(machine, "s-light", SimState::Idle);
        w.env.roster.lock().unwrap()[1].routine_key = Some(key("light").to_string());
    }
    save(&w.home(), heavy_routine(machine));
    save(&w.home(), wake_routine("light", machine));
    manual(&w.home(), "heavy");
    w.drain().await;
    w.go(machine, BARB, SimState::Idle);
    manual(&w.home(), "light");
    w.drain().await;
    assert_eq!(
        limits_sent(&w, machine),
        [
            Some(WakeLimits {
                budget: format!("routine:{}#{}:0", key("heavy"), install(&w)),
                max_wakes: Some(3),
                max_wakes_per_day: None,
                min_gap_ms: Some(2 * 60 * 60 * 1000),
                gap: GapPolicy::Skip,
            }),
            None
        ]
    );
}

#[tokio::test]
async fn agr13_local_limits_ride_on_the_wake() {
    limits_ride_on_the_wake(LOCAL).await;
}

#[tokio::test]
async fn agr13_remote_limits_ride_on_the_wake() {
    limits_ride_on_the_wake(REMOTE).await;
}

// AC1: the wake past `max_wakes` pauses the routine here with one inbox
// note (said once); resuming starts a new budget and it continues.
async fn max_wakes_pauses_once_and_a_resume_continues(machine: &str) {
    let w = World::new();
    w.barb(machine, BARB, SimState::Idle);
    w.env.roster.lock().unwrap()[0].routine_key = Some(key("heavy").to_string());
    save(&w.home(), heavy_routine(machine));
    decide(&w, machine, UndeliveredReason::MaxWakes, None);
    manual(&w.home(), "heavy");
    w.drain().await;
    let run = only_run(&w.home(), "heavy");
    assert_eq!(run.phase, phase::UNDELIVERED);
    assert_eq!(run.reason.as_deref(), Some("max_wakes"));
    let heavy = key("heavy").to_string();
    assert!(!store(&w.home()).switch_on(&heavy).unwrap(), "paused");
    let posts = w.posts();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(posts[0].kind, NoticeKind::MaxWakes);
    assert!(
        posts[0].title().contains("paused itself"),
        "{}",
        posts[0].title()
    );
    // The same outcome read again (a push, a catch-up) says nothing more.
    wake::record_status(
        &store(&w.home()),
        &run.run_id,
        &SessionRef::new(machine, BARB),
        &WakeRunStatus::Undelivered {
            reason: UndeliveredReason::MaxWakes,
        },
        w.env.as_ref(),
    );
    assert_eq!(w.posts().len(), 1);
    // Resumed: a new budget generation, and the next wake goes in.
    service::set_enabled(
        &w.home(),
        &super::test_support::alice(),
        &key("heavy"),
        true,
    )
    .unwrap();
    *w.sim(machine).decide.lock().unwrap() = None;
    manual(&w.home(), "heavy");
    w.drain().await;
    let budgets: Vec<String> = limits_sent(&w, machine)
        .into_iter()
        .map(|l| l.unwrap().budget)
        .collect();
    let install = install(&w);
    assert_eq!(
        budgets,
        [
            format!("routine:{heavy}#{install}:0"),
            format!("routine:{heavy}#{install}:1")
        ]
    );
    assert_typed_once(&w.typed(), BARB, "Routine heavy");
}

#[tokio::test]
async fn agr13_local_max_wakes_pauses_once_and_a_resume_continues() {
    max_wakes_pauses_once_and_a_resume_continues(LOCAL).await;
}

#[tokio::test]
async fn agr13_remote_max_wakes_pauses_once_and_a_resume_continues() {
    max_wakes_pauses_once_and_a_resume_continues(REMOTE).await;
}

// AC2 + AC3: over the day's budget is `undelivered(limit_reached)` and
// inside the gap with `overlap: skip` a skipped run; neither pauses the
// routine or posts anything (both resume on their own).
async fn day_and_gap_limits_end_the_run_quietly(machine: &str) {
    for (reason, phase_, wire) in [
        (
            UndeliveredReason::LimitReached,
            phase::UNDELIVERED,
            "limit_reached",
        ),
        (UndeliveredReason::MinGap, phase::SKIPPED, "min_gap"),
    ] {
        let w = World::new();
        w.barb(machine, BARB, SimState::Idle);
        w.env.roster.lock().unwrap()[0].routine_key = Some(key("heavy").to_string());
        save(&w.home(), heavy_routine(machine));
        decide(&w, machine, reason, None);
        manual(&w.home(), "heavy");
        w.drain().await;
        let run = only_run(&w.home(), "heavy");
        assert_eq!(
            (run.phase.as_str(), run.reason.as_deref()),
            (phase_, Some(wire))
        );
        assert!(store(&w.home())
            .switch_on(&key("heavy").to_string())
            .unwrap());
        assert!(w.posts().is_empty(), "{:?}", w.posts());
        assert!(w.typed().is_empty());
    }
}

#[tokio::test]
async fn agr13_local_day_and_gap_limits_end_the_run_quietly() {
    day_and_gap_limits_end_the_run_quietly(LOCAL).await;
}

#[tokio::test]
async fn agr13_remote_day_and_gap_limits_end_the_run_quietly() {
    day_and_gap_limits_end_the_run_quietly(REMOTE).await;
}

// AC4: the terminal server's backstop refusal posts its notice (naming the
// top source) once, and never pauses the routine.
async fn the_backstop_notice_is_posted_and_nothing_pauses(machine: &str) {
    let w = World::new();
    w.barb(machine, BARB, SimState::Idle);
    w.env.roster.lock().unwrap()[0].routine_key = Some(key("light").to_string());
    save(&w.home(), wake_routine("light", machine));
    let notice =
        "Session s-barb was asked for 120 wakes in the last hour. Most came from Loop A (80).";
    decide(&w, machine, UndeliveredReason::RateLimited, Some(notice));
    manual(&w.home(), "light");
    w.drain().await;
    let run = only_run(&w.home(), "light");
    assert_eq!(run.reason.as_deref(), Some("rate_limited"));
    let posts = w.posts();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(posts[0].kind, NoticeKind::RateLimited);
    assert!(posts[0].title().contains("Loop A"), "{}", posts[0].title());
    assert!(store(&w.home())
        .switch_on(&key("light").to_string())
        .unwrap());
    // Later refusals of the same trip carry no notice: nothing more is said.
    decide(&w, machine, UndeliveredReason::RateLimited, None);
    manual(&w.home(), "light");
    w.drain().await;
    assert_eq!(w.posts().len(), 1);
}

#[tokio::test]
async fn agr13_local_the_backstop_notice_is_posted_and_nothing_pauses() {
    the_backstop_notice_is_posted_and_nothing_pauses(LOCAL).await;
}

#[tokio::test]
async fn agr13_remote_the_backstop_notice_is_posted_and_nothing_pauses() {
    the_backstop_notice_is_posted_and_nothing_pauses(REMOTE).await;
}

// r1 SHOULD 4: a limited wake to a terminal server too old to enforce the
// limits is never sent: the run is `undelivered(unavailable)` with the
// refusal as its detail, and the inbox is told once per machine.
async fn an_outdated_server_is_sent_no_limited_wake_and_said_once(machine: &str) {
    let w = World::new();
    w.barb(machine, BARB, SimState::Idle);
    w.env.roster.lock().unwrap()[0].routine_key = Some(key("heavy").to_string());
    save(&w.home(), heavy_routine(machine));
    w.sim(machine).before_limits.store(true, Ordering::SeqCst);
    manual(&w.home(), "heavy");
    w.drain().await;
    let run = only_run(&w.home(), "heavy");
    assert_eq!(run.phase, phase::UNDELIVERED);
    assert_eq!(run.reason.as_deref(), Some("unavailable"));
    assert!(
        run.result_text
            .as_deref()
            .is_some_and(|t| t.contains("too old to enforce wake limits")),
        "{run:?}"
    );
    assert!(w.typed().is_empty());
    assert!(
        w.sim(machine).requests.lock().unwrap().is_empty(),
        "nothing sent"
    );
    manual(&w.home(), "heavy");
    w.drain().await;
    let posts = w.posts();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(posts[0].kind, NoticeKind::ServerOutdated);
    assert!(store(&w.home())
        .switch_on(&key("heavy").to_string())
        .unwrap());
}

#[tokio::test]
async fn agr13_local_an_outdated_server_is_sent_no_limited_wake_and_said_once() {
    an_outdated_server_is_sent_no_limited_wake_and_said_once(LOCAL).await;
}

#[tokio::test]
async fn agr13_remote_an_outdated_server_is_sent_no_limited_wake_and_said_once() {
    an_outdated_server_is_sent_no_limited_wake_and_said_once(REMOTE).await;
}

// SW-AGR-8 (MUST 2): a wake's launch-if-absent on a machine that already
// runs the agent, or is starting it (its own routine got there first: the
// machine's single flight), starts nothing and is not ended: it is deferred,
// and delivered to that session once it shows.
#[tokio::test]
async fn a_remote_launch_refused_as_already_running_defers_the_wake() {
    use super::wake_app::{tab_reply, TabAnswer};
    let refused = tab_reply(&TabAnswer {
        status: "refused",
        code: Some(codesense_pty_core::routine_arm::AGENT_RUNNING),
        message: Some("acme:core/barb is already running or starting in this workspace"),
        session: None,
        handle: None,
        retryable: false,
        attach: false,
    });
    assert!(matches!(refused, TabReply::Running(_)), "{refused:?}");
    let w = World::new();
    *w.env.remote_reply.lock().unwrap() = refused;
    save(&w.home(), wake_routine("triage", REMOTE));
    manual(&w.home(), "triage");
    w.drain().await;
    let run = only_run(&w.home(), "triage");
    assert_eq!(run.phase, phase::QUEUED, "deferred, not ended: {run:?}");
    assert!(!w.take_redispatched().is_empty(), "looked at again");
    assert!(w.typed().is_empty());
}

// r1 SHOULD 7: limits on a routine that starts a new agent each fire save,
// with a warning that they never apply; on a wake routine, none.
#[test]
fn wake_limits_on_a_fresh_routine_save_with_a_warning() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let save_with = |id: &str, mode: &str| {
        let mut def = routine(id, "0 9 * * *");
        def.mode = mode.into();
        def.policy.visible = true;
        def.policy.min_gap = Some("2h".into());
        service::save(
            &home,
            &alice(),
            SaveTarget::Create(Placement::Team {
                org: ORG.into(),
                team: TEAM.into(),
            }),
            def,
            &super::Author::user("alice"),
        )
        .unwrap()
        .warnings
        .iter()
        .any(|w| w.code == crate::agents::launch_check::LaunchWarningCode::WakeLimitsIgnored)
    };
    assert!(save_with("fresh-limited", "fresh"));
    assert!(!save_with("wake-limited", "wake"));
}

// SW-AGR-13 r2 LOW: a note with no routine goes to the team of the session's
// workspace; a workspace of no team has none.
#[test]
fn a_workspace_key_names_its_team() {
    assert_eq!(
        super::wake_app::team_of_workspace_key("acme:backend|objects"),
        Some(("acme".to_string(), "backend".to_string()))
    );
    assert_eq!(
        super::wake_app::team_of_workspace_key("acme:_none|objects"),
        None
    );
}

// SW-AGR-13 r2 LOW: an unreadable install id never yields a shared budget.
#[test]
fn no_install_id_sends_no_limits() {
    let mut def = routine("heavy", "0 9 * * *");
    def.policy.min_gap = Some("2h".into());
    assert!(wake::wake_limits_of(&def, "acme/core/heavy", "", 0).is_none());
    assert!(wake::wake_limits_of(&def, "acme/core/heavy", "mac-1", 0).is_some());
}

#[tokio::test]
async fn a_capped_agents_routine_wake_is_skipped_without_typing() {
    use crate::session_usage::{event::Tokens, spend_caps::Budget, tests::event, UsageStore};
    let w = World::new();
    let home = w.home();
    let agent_key = crate::agents::AgentKey::parse(&barb_key()).unwrap();
    let mut agent = crate::agents::service::get(&home, &alice(), &agent_key)
        .unwrap()
        .agent;
    agent.budget_usd = Some(Budget {
        per_day: Some(1.0),
        per_week: None,
    });
    crate::agents::service::save(
        &home,
        &alice(),
        crate::agents::service::SaveTarget::Update {
            key: agent_key.clone(),
            base_revision: agent.revision,
        },
        agent,
        &super::model::Author::user("alice"),
    )
    .unwrap();
    save(&home, inbox_wake("budget-wake", LOCAL));
    w.barb(LOCAL, BARB, SimState::Idle);
    let mut usage = UsageStore::open(&home.join("usage.sqlite")).unwrap();
    usage
        .attribute(BARB, ORG, Some(TEAM), &agent_key.to_string(), "h", None)
        .unwrap();
    let mut call = event(
        "spent",
        BARB,
        "claude-opus-5-5",
        chrono::Utc::now().timestamp_millis(),
        Tokens::default(),
    );
    call.reported_cost_usd = Some(1.0);
    usage.ingest(&[call]).unwrap();
    let admission = manual(&home, "budget-wake");
    assert!(
        matches!(admission.outcome,super::admit::Outcome::Skipped {ref reason} if reason=="budget_reached")
    );
    assert!(w.typed().is_empty());
}

async fn captured_wake_into_existing_session() -> (crate::session_usage::task::TaskRecord, PathBuf) {
    use crate::session_usage::{UsageStore, record::SessionRecord, task::{TaskRecord, SkillVersion, LINK_SESSION}};
    let w = World::new();
    let root = service::key_content_dir(&w.home(), &alice(), &key("triage")).unwrap();
    std::fs::create_dir_all(root.join("tasks")).unwrap();
    std::fs::write(root.join("tasks/task-types.yml"),
        "types:\n  routine:\n    targets: {wall_minutes: 10, cost_usd: 1}\n  ops:\n    targets: {wall_minutes: 20, cost_usd: 2}\n").unwrap();
    let usage = UsageStore::open(&w.home().join("usage.sqlite")).unwrap();
    let mut session = SessionRecord::new(BARB);
    session.org = Some(ORG.into());
    session.team = Some(TEAM.into());
    session.worktree = Some(root.to_string_lossy().into_owned());
    session.provided_skills = Some(vec!["bmad-build".into()]);
    usage.write_session_record(&session).unwrap();
    let mut launch_task = TaskRecord::new(BARB, "the session launch");
    launch_task.worktree = session.worktree.clone();
    launch_task.layers.skills = vec![SkillVersion {
        name: "bmad-build".into(), hash: Some("immutable-launch-hash".into()), reason: None,
    }];
    usage.insert_task_if_absent(&launch_task).unwrap();
    usage.add_task_link(BARB, LINK_SESSION, BARB).unwrap();
    w.barb(LOCAL, BARB, SimState::Idle);
    let mut def = wake_routine("triage", LOCAL);
    def.task = "Task-Type: ops\nSubject: research\nTargets: wall_minutes=60 cost_usd=5\nRun $bmad-build to triage.".into();
    save(&w.home(), def);
    let fired = manual(&w.home(), "triage");
    w.drain().await;
    assert_eq!(w.stub.calls(), 0);
    assert_eq!(run(&w.home(), &fired.run_id).phase, phase::DELIVERED);
    let task = usage.read_task(&fired.run_id).unwrap().unwrap();
    assert_eq!(usage.read_outcome(&task).unwrap().scope, "interval", "a reused session's run is not the session root");
    (task, root)
}

#[tokio::test]
async fn a_wake_into_an_existing_session_honors_directives_over_policy_defaults() {
    let (task, _) = captured_wake_into_existing_session().await;
    assert_eq!((task.task_type.as_str(), task.subject.as_deref()), ("ops", Some("research")));
    assert_eq!(task.type_source.as_deref(), Some("brief"));
    let targets = task.targets.unwrap();
    assert_eq!(targets.values["wall_minutes"], 60.0);
    assert_eq!(targets.values["cost_usd"], 5.0);
    assert_eq!(targets.source, "brief");
}

#[tokio::test]
async fn a_wake_into_an_existing_session_reuses_its_cwd_and_immutable_launch_skill_hash() {
    let (task, root) = captured_wake_into_existing_session().await;
    assert_eq!(task.worktree.as_deref(), root.to_str());
    assert_eq!(task.main_skill.as_deref(), Some("bmad-build"));
    assert_eq!(task.layers.skills[0].hash.as_deref(), Some("immutable-launch-hash"));
}

#[tokio::test]
async fn a_wake_that_launches_honors_directives_over_policy_defaults_after_enrichment() {
    use crate::session_usage::{UsageStore, task_capture::{Capture, TaskInputs, capture_launch_task}};
    let w = World::new();
    let root = service::key_content_dir(&w.home(), &alice(), &key("triage")).unwrap();
    std::fs::create_dir_all(root.join("tasks")).unwrap();
    std::fs::write(root.join("tasks/task-types.yml"),
        "types:\n  routine:\n    targets: {wall_minutes: 10, cost_usd: 1}\n  ops:\n    targets: {wall_minutes: 20, cost_usd: 2}\n").unwrap();
    let mut def = wake_routine("triage", LOCAL);
    def.task = "Task-Type: ops\nSubject: research\nTargets: wall_minutes=60 cost_usd=5\nTriage the issues.".into();
    save(&w.home(), def);
    let fired = manual(&w.home(), "triage");
    w.drain().await;
    assert_eq!(w.stub.calls(), 1);
    let launched = w.stub.last();
    let write = crate::commands::dashboard_launch::compose_launch_write(
        &w.stub.registry, &launched.session_id, &launched.request.handle, Default::default(),
    ).unwrap();
    let inputs = TaskInputs::from_launch(&launched.request.agent, Some(&launched.request.prompt));
    let usage = UsageStore::open(&w.home().join("usage.sqlite")).unwrap();
    let before = usage.read_task(&fired.run_id).unwrap().expect("start precedes launch capture");
    let task = capture_launch_task(&usage, &Capture {
        session_id: &launched.session_id, record: &write.record, inputs: &inputs,
        routine: write.routine.as_ref(), launcher_task: write.launcher.as_deref(),
        story_file: None, now_ms: finish::now_ms(),
    }).unwrap();
    assert_eq!((task.task_type.as_str(), task.subject.as_deref()), ("ops", Some("research")));
    assert_eq!(task.type_source.as_deref(), Some("brief"));
    let targets = task.targets.unwrap();
    assert_eq!(targets.values["wall_minutes"], 60.0);
    assert_eq!(targets.values["cost_usd"], 5.0);
    assert_eq!(targets.source, "brief");
    assert_eq!(task.status, "done");
    assert_eq!(task.closed_at_ms, before.closed_at_ms, "enrichment never reopens a delivery");
}
