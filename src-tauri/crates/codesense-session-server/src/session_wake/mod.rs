// SPDX-License-Identifier: MIT
//! `SessionWakeV1` on the host that runs the session: a per-session
//! delivery queue written into the PTY only when the agent is observed idle,
//! the session timers that feed it, idempotency by key, and a capped outcome
//! log for clients that were away. Local and remote sessions take this one
//! path: the local session server outlives the app too.
//!
//! A queue head may type only when all of these hold, checked
//! once per tick and again while the writable attachment is held:
//! - the session is live and still the agent invocation the delivery was
//!   queued for; its activity snapshot is observed, unexited, `Idle`/`Done`,
//!   with no open ask; the agent CLI is the PTY's foreground job (one batched
//!   process probe per tick). OSC-133 never counts;
//! - the session is past the turn of its last delivery: one delivery per turn;
//! - no viewer input in the last 3 s, and no input that can leave text in the
//!   prompt (a draft, an open `/` menu) since the agent's current turn began;
//! - the TUI has bracketed paste on;
//! - the writable attachment is free, or held by the delivery's own desktop,
//!   with no write in flight (never stolen).
//!
//! It is written once, through the session's input queue: the header and
//! text as one bracketed paste, a pause of at least 200 ms for the echo to
//! settle (the repo's agent submit timing), then one Enter. The submit is
//! confirmed from activity (a new turn, or busy) before it counts as
//! `delivered`; unconfirmed, or a write that failed part way, is `unknown`
//! and never written again (R5). No nudges, no retries.
//!
//! Each attempt runs off the queue's lock on a runner of its own, so a slow
//! session never holds up another. Times within a host lifetime are
//! monotonic; only what must outlive it is kept in wall-clock time.
//!
//! No session record is persisted. The store (`store.rs`) keeps the queue,
//! timers and outcomes so a restarted host (a new boot id) can say what it
//! lost: queued deliveries become `undelivered(server_restarted)`, one
//! caught mid-write `unknown`; timers end with their sessions.
//!
//! Who may set a timer or wake a session is enforced on the desktop, which
//! authenticates the caller. This host trusts its
//! admitted connections and stamps each request with the connection's viewer.

pub(crate) mod host_port;
pub(crate) mod schedule;
pub(crate) mod store;
#[cfg(test)]
pub(crate) mod tests;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::{Duration, Instant};

use codesense_pty_core::activity::{Snapshot, State as Activity};
use codesense_pty_core::agent_cli::AgentCli;
use codesense_pty_core::domain::PtySessionId;
use codesense_pty_core::session_wake::{
    compose, paste_payload, timer_header, valid_label, DeliverRequest, DeliveryStatus, GapPolicy,
    Originator, OutcomeLog, TimerInfo, TimerSchedule, TimerSpec, UndeliveredReason, WakeLimits,
    WakeOutcome, BUDGET_RETENTION_MS, ENTER_GAP_MAX_MS, ENTER_GAP_MIN_MS, IDEMPOTENCY_GRACE_MS,
    MACHINE_TIMER_CAP, MAX_MIN_GAP_MS, MAX_WAIT_LIMIT_MS, MIN_TIMER_INTERVAL_MS, OBSERVE_GRACE_MS,
    OUTCOME_LOG_CAP, QUIET_INPUT_MS, SESSION_RATE_LIMIT, SESSION_RATE_WINDOW_MS, SESSION_TIMER_CAP,
    SUBMIT_CONFIRM_MS, SUBMIT_KEY, WAKE_DAY_MS,
};
use serde::{Deserialize, Serialize};

use schedule::CronSchedule;
use store::Store;

/// How often the host evaluates its queue and timers.
pub(crate) const TICK: Duration = Duration::from_secs(1);
/// How long an evicted key is still answered `unknown`.
const EVICTED_RETENTION_MS: u64 = IDEMPOTENCY_GRACE_MS;
/// Poll cadence while waiting on the echo and the submit.
const POLL_MS: u64 = 25;
/// How long a session whose attempt was put back is left alone.
const REQUEUE_COOLDOWN_MS: u64 = 2_000;
/// Below this many deliveries in the backstop's hour a trip is over: the
/// next one is said again.
const BACKSTOP_LOW_WATER: usize = SESSION_RATE_LIMIT / 2;
/// Consecutive failed ticks after which the capability is withdrawn.
const TICK_FAILURES: u32 = 3;

/// The host's clocks: a monotonic one for everything within this host
/// lifetime, the wall clock for what is kept across lifetimes and for cron.
pub trait Clock: Send + Sync {
    fn mono_ms(&self) -> u64;
    fn wall_ms(&self) -> u64;
    fn sleep_ms(&self, ms: u64);
}

/// The system clocks. Monotonic time keeps counting while the machine
/// sleeps (`CLOCK_BOOTTIME` on Linux, `CLOCK_MONOTONIC` — continuous time —
/// on macOS), so a wait counts real time, not awake time. `Instant` does not
/// (it stops across sleep on both).
pub struct SystemClock;

/// Milliseconds on the sleep-counting monotonic clock.
fn continuous_ms() -> u64 {
    #[cfg(target_os = "linux")]
    let id = libc::CLOCK_BOOTTIME;
    #[cfg(not(target_os = "linux"))]
    let id = libc::CLOCK_MONOTONIC;
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes the one timespec it is given.
    if unsafe { libc::clock_gettime(id, &mut now) } != 0 {
        static BASE: OnceLock<Instant> = OnceLock::new();
        return u64::try_from(BASE.get_or_init(Instant::now).elapsed().as_millis())
            .unwrap_or(u64::MAX);
    }
    u64::try_from(now.tv_sec)
        .unwrap_or(0)
        .saturating_mul(1000)
        .saturating_add(u64::try_from(now.tv_nsec).unwrap_or(0) / 1_000_000)
}

impl Clock for SystemClock {
    fn mono_ms(&self) -> u64 {
        continuous_ms()
    }
    fn wall_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0)
    }
    fn sleep_ms(&self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

/// Viewer input that reached a session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InputStamp {
    /// When the latest input arrived, on [`Clock::mono_ms`].
    pub at_ms: u64,
    /// Where the agent was (invocation, turn generation) when input that can
    /// leave text in its prompt last arrived.
    pub draft: Option<(String, u64)>,
}

/// Whether a delivery may use the session's writable attachment.
pub enum Claim {
    /// Held until dropped: no one else can claim it meanwhile.
    Held(Box<dyn Send>),
    /// Held by another connection, or a write is in flight. Never stolen.
    Elsewhere,
}

/// What the queue reads and writes on the host.
pub trait WakePort: Send + Sync {
    /// The session is registered and its output has not ended.
    fn session_live(&self, session: &PtySessionId) -> bool;
    /// The session's activity snapshot (pty-core `ActivityOwner`).
    fn activity(&self, session: &PtySessionId) -> Option<Snapshot>;
    /// For each query, whether the provider's CLI is the PTY's foreground
    /// job: one process probe for them all.
    fn agents_in_foreground(&self, queries: &[(PtySessionId, AgentCli)]) -> Vec<Option<bool>>;
    /// Whether the session's shell runs no job of its own (no foreground job,
    /// no background or stopped job): `None` when it cannot be read. A
    /// stopped agent (Ctrl-Z) is a job, so it is not an idle shell.
    fn shell_idle(&self, session: &PtySessionId) -> Option<bool>;
    fn last_input(&self, session: &PtySessionId) -> Option<InputStamp>;
    /// Whether the TUI has bracketed paste (DECSET 2004) on; `None` when the
    /// terminal cannot be read.
    fn bracketed_paste(&self, session: &PtySessionId) -> Option<bool>;
    fn output_seq(&self, session: &PtySessionId) -> Option<u64>;
    /// Whether [`claim`](Self::claim) would hold it now (taking nothing).
    fn claim_available(&self, session: &PtySessionId, originator: &Originator) -> bool;
    fn claim(&self, session: &PtySessionId, originator: &Originator) -> Claim;
    /// Write through the session's input queue, in order with other input.
    fn write(&self, session: &PtySessionId, bytes: &[u8]) -> Result<(), String>;
    /// Push a final outcome to the clients that asked for them.
    fn emit(&self, outcome: &WakeOutcome);
}

/// Where a delivery attempt runs.
pub trait Runner: Send + Sync {
    fn run(&self, job: Box<dyn FnOnce() + Send>);
}

/// Each attempt on a thread of its own.
pub struct ThreadRunner;

impl Runner for ThreadRunner {
    fn run(&self, job: Box<dyn FnOnce() + Send>) {
        let job = Arc::new(Mutex::new(Some(job)));
        let spare = job.clone();
        if std::thread::Builder::new()
            .name("session-wake-write".into())
            .spawn(move || {
                if let Some(job) = spare.lock().unwrap_or_else(|p| p.into_inner()).take() {
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err() {
                        log::error!("a session wake write panicked");
                    }
                }
            })
            .is_err()
        {
            if let Some(job) = job.lock().unwrap_or_else(|p| p.into_inner()).take() {
                job()
            }
        }
    }
}

/// The agent invocation a delivery or timer belongs to. A respawned tab
/// reuses its session id; never its incarnation or invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Binding {
    incarnation: String,
    invocation: String,
}

impl Binding {
    fn of(snapshot: &Snapshot) -> Self {
        Self {
            incarnation: snapshot.identity.session_incarnation.clone(),
            invocation: snapshot.identity.invocation_id.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pending {
    id: String,
    /// The client key scoped by originator and session; `None` for a timer's
    /// fire, which never enters the client key map.
    scope: Option<String>,
    key: String,
    session: PtySessionId,
    binding: Option<Binding>,
    payload: String,
    marker: String,
    originator: Originator,
    timer_id: Option<String>,
    enqueued_ms: u64,
    deadline_ms: u64,
    max_wait_ms: u64,
    /// An attempt holds it; a write may have begun.
    in_flight: bool,
    /// The routine wake budget it is charged to once it goes in.
    #[serde(default)]
    budget: Option<String>,
}

#[derive(Debug, Clone)]
struct Record {
    outcome: WakeOutcome,
    retain_until_ms: u64,
}

/// One appended line of `outcomes.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Line {
    outcome: WakeOutcome,
    scope: Option<String>,
    retain_until_wall: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Timer {
    /// Its text is `info.text`, stored once.
    info: TimerInfo,
    max_wait_ms: u64,
    binding: Option<Binding>,
    next_ms: u64,
    /// A cron timer's next fire on the wall clock: judged against the wall
    /// clock every tick, so a machine that slept fires on time when it wakes.
    #[serde(default)]
    next_wall: Option<u64>,
    /// The delivery this timer has queued and not yet settled.
    pending: Option<String>,
    /// Its last delivery that went in (or may have): id and wall clock, for
    /// its `min_gap`.
    #[serde(default)]
    last: Option<(String, u64)>,
}

/// One routine's wake budget, kept across host lifetimes in wall
/// time: what its `max_wakes`, `max_wakes_per_day` and `min_gap` count.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Budget {
    /// Deliveries that went in (or may have: `unknown`) under it.
    total: u32,
    /// When those of the last rolling day went in.
    day: VecDeque<u64>,
    /// Its last delivery, into whichever session, and when: `min_gap`
    /// belongs to the routine, so a relaunched session is inside it too.
    latest: Option<(String, u64)>,
    /// Its last use (a delivery, or a fire checked against it); one unused
    /// for [`BUDGET_RETENTION_MS`] is forgotten.
    touched: u64,
}

/// The rewritten part of the store.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Meta {
    boot_id: String,
    next_seq: u64,
    dropped_through: u64,
    queue: Vec<Pending>,
    timers: Vec<Timer>,
    /// Per session: the invocation and turn its last delivery was written in.
    held: HashMap<PtySessionId, (String, u64)>,
    /// Evicted key hashes and when (wall clock) they are forgotten.
    evicted: Vec<(u64, u64)>,
    budgets: HashMap<String, Budget>,
    rate: HashMap<PtySessionId, VecDeque<(u64, String)>>,
    tripped: HashSet<PtySessionId>,
}

#[derive(Default)]
struct State {
    boot_id: String,
    next_seq: u64,
    dropped_through: u64,
    log: VecDeque<WakeOutcome>,
    records: HashMap<String, Record>,
    evicted: HashMap<u64, u64>,
    queue: VecDeque<Pending>,
    timers: Vec<Timer>,
    held: HashMap<PtySessionId, (String, u64)>,
    /// Routine wake budgets by their key.
    budgets: HashMap<String, Budget>,
    /// The backstop: per session, every delivery queued for it in the last
    /// [`SESSION_RATE_WINDOW_MS`] (wall clock) and the source that asked.
    rate: HashMap<PtySessionId, VecDeque<(u64, String)>>,
    /// Sessions whose backstop tripped and has not fallen below its low-water
    /// mark since: the trip was said once.
    tripped: HashSet<PtySessionId>,
    /// Sessions whose last attempt was put back, and until when (monotonic)
    /// they are not tried again.
    cooldown: HashMap<PtySessionId, u64>,
    /// Sessions idle at a prompt without their agent CLI in the foreground,
    /// since when (monotonic): the agent quit back to the shell.
    quit_since: HashMap<PtySessionId, u64>,
}

/// How long a session may sit at its prompt without its agent CLI in the
/// foreground before a queued wake for it is `session_ended`.
pub const AGENT_QUIT_GRACE_MS: u64 = 3_000;

/// Whether `facts` show a live, observed session at rest where a probe that
/// worked found no agent CLI and the shell runs no job: its agent quit back
/// to the shell. A probe that could not answer is never a quit.
fn quit_shell(facts: &Facts) -> bool {
    facts.live
        && facts.foreground == Some(false)
        && facts.shell_idle == Some(true)
        && facts.snapshot.as_ref().is_some_and(Snapshot::takes_input)
}

/// What the gathered facts decide for a queue head.
#[derive(Debug, PartialEq)]
enum Verdict {
    Wait,
    Final(UndeliveredReason),
    Go,
}

/// What the host said about one session, read outside the queue's lock.
struct Facts {
    live: bool,
    snapshot: Option<Snapshot>,
    /// `None`: the probe could not tell.
    foreground: Option<bool>,
    /// Asked only when the agent is not in the foreground.
    shell_idle: Option<bool>,
    input: Option<InputStamp>,
}

/// The host's wake queue and timers.
pub struct SessionWake {
    port: Arc<dyn WakePort>,
    clock: Arc<dyn Clock>,
    runner: Arc<dyn Runner>,
    store: Mutex<Option<Store>>,
    state: Mutex<State>,
    me: Weak<SessionWake>,
    failures: AtomicU32,
    failed: AtomicBool,
    /// Make every tick fail (tests of the supervisor).
    #[cfg(test)]
    poisoned: AtomicBool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

fn key_hash(scope: &str) -> u64 {
    let hash = blake3::hash(scope.as_bytes());
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&hash.as_bytes()[..8]);
    u64::from_le_bytes(bytes)
}

/// A client key, scoped by who asked and where.
fn scoped(originator: &Originator, session: &PtySessionId, key: &str) -> String {
    format!("{}\u{0}{}\u{0}{key}", originator.desktop, session)
}

impl SessionWake {
    /// The queue of the host lifetime `boot_id`, keeping its store in `store`
    /// (a directory). A store left by another lifetime is reconciled.
    pub fn open(
        port: Arc<dyn WakePort>,
        clock: Arc<dyn Clock>,
        runner: Arc<dyn Runner>,
        boot_id: String,
        store: Option<PathBuf>,
    ) -> Arc<Self> {
        let mut store = store.map(Store::new);
        let (meta, lines): (Option<Meta>, Vec<Line>) = match store.as_mut() {
            Some(store) => store.load(),
            None => (None, Vec::new()),
        };
        let meta = meta.unwrap_or_default();
        let now = clock.mono_ms();
        let wall = clock.wall_ms();
        let restarted = !meta.boot_id.is_empty() && meta.boot_id != boot_id;
        let mut state = State {
            boot_id,
            next_seq: meta.next_seq,
            dropped_through: meta.dropped_through,
            timers: meta.timers,
            held: meta.held,
            budgets: meta.budgets,
            rate: meta.rate,
            tripped: meta.tripped,
            queue: meta.queue.into(),
            evicted: meta
                .evicted
                .into_iter()
                .filter(|(_, until)| *until > wall)
                .collect(),
            ..State::default()
        };
        for line in lines {
            state.next_seq = state.next_seq.max(line.outcome.seq);
            if let Some(scope) = line.scope {
                if line.retain_until_wall > wall {
                    let retain_until_ms = now.saturating_add(line.retain_until_wall - wall);
                    state.records.insert(
                        scope,
                        Record {
                            outcome: line.outcome.clone(),
                            retain_until_ms,
                        },
                    );
                }
            }
            if !state.log.iter().any(|o| o.seq == line.outcome.seq) {
                state.log.push_back(line.outcome);
            }
        }
        state.log.make_contiguous().sort_by_key(|o| o.seq);
        while state.log.len() > OUTCOME_LOG_CAP {
            if let Some(dropped) = state.log.pop_front() {
                state.dropped_through = state.dropped_through.max(dropped.seq);
            }
        }
        let wake = Arc::new_cyclic(|me| Self {
            port,
            clock,
            runner,
            store: Mutex::new(store),
            state: Mutex::new(state),
            me: me.clone(),
            failures: AtomicU32::new(0),
            failed: AtomicBool::new(false),
            #[cfg(test)]
            poisoned: AtomicBool::new(false),
        });
        {
            let mut state = lock(&wake.state);
            if restarted {
                wake.reconcile_restart(&mut state);
            } else {
                // Queued client records are kept with the queue, not as lines.
                let queued: Vec<Pending> = state.queue.iter().cloned().collect();
                for pending in queued {
                    let outcome = wake.queued_outcome(&state, &pending);
                    wake.remember(&mut state, &pending, outcome);
                }
            }
            let _ = wake.save(&state);
        }
        wake
    }

    fn reconcile_restart(&self, state: &mut State) {
        let now = self.clock.mono_ms();
        let queue: Vec<Pending> = state.queue.drain(..).collect();
        for pending in queue {
            let status = if pending.in_flight {
                DeliveryStatus::Unknown {
                    detail: "the terminal server restarted while writing".into(),
                }
            } else {
                DeliveryStatus::Undelivered {
                    reason: UndeliveredReason::ServerRestarted,
                }
            };
            let outcome = self.settle(state, &pending, status);
            self.port.emit(&outcome);
        }
        // Sessions do not survive their host, and neither do their timers.
        for timer in std::mem::take(&mut state.timers) {
            let outcome = self.timer_ended(state, &timer, UndeliveredReason::ServerRestarted, now);
            self.port.emit(&outcome);
        }
        // Budgets belong to routines and outlive the host; the backstop's
        // counts belong to sessions, which do not.
        state.held.clear();
        state.rate.clear();
        state.tripped.clear();
    }

    /// This host lifetime.
    pub fn boot_id(&self) -> String {
        lock(&self.state).boot_id.clone()
    }

    /// Whether the queue still runs: false once its ticker kept failing.
    pub fn healthy(&self) -> bool {
        !self.failed.load(Ordering::Acquire)
    }

    /// Queue `request` and try it now. A key this originator already used on
    /// this session is answered with its stored outcome (an evicted one with
    /// `unknown`), and nothing is written. `Err` refuses it: nothing queued.
    pub fn deliver(&self, request: DeliverRequest) -> Result<WakeOutcome, String> {
        if !valid_label(&request.idempotency_key) {
            return Err("idempotency key must be 1-256 bytes without controls".into());
        }
        let scope = scoped(
            &request.originator,
            &request.session,
            &request.idempotency_key,
        );
        if let Some(answer) = self.known(&lock(&self.state), &scope, &request) {
            return Ok(answer);
        }
        if request.max_wait_ms == 0 || request.max_wait_ms > MAX_WAIT_LIMIT_MS {
            return Err(format!(
                "max wait must be between 1 ms and {MAX_WAIT_LIMIT_MS} ms"
            ));
        }
        if request.marker.len() > 256 || request.marker.chars().any(char::is_control) {
            return Err("marker must be at most 256 bytes without controls".into());
        }
        if let Some(limits) = &request.limits {
            limits.validate()?;
        }
        let payload = paste_payload(request.header.as_deref(), &request.text)?;
        if !self.port.session_live(&request.session) {
            return Err(format!("session {} is not running", request.session));
        }
        let binding = self
            .port
            .activity(&request.session)
            .map(|s| Binding::of(&s));
        let now = self.clock.mono_ms();
        let answer = {
            let mut state = lock(&self.state);
            if let Some(answer) = self.known(&state, &scope, &request) {
                return Ok(answer);
            }
            let refusal = self.limit_verdict(
                &mut state,
                &request.session,
                request.limits.as_ref(),
                self.clock.wall_ms(),
            );
            let pending = Pending {
                id: uuid::Uuid::new_v4().to_string(),
                scope: Some(scope.clone()),
                key: request.idempotency_key,
                session: request.session,
                binding,
                payload,
                marker: request.marker,
                originator: request.originator,
                timer_id: None,
                enqueued_ms: now,
                deadline_ms: now.saturating_add(request.max_wait_ms),
                max_wait_ms: request.max_wait_ms,
                in_flight: false,
                budget: request.limits.map(|limits| limits.budget),
            };
            if let Some((status, notice)) = refusal {
                // Decided now, by the limits: logged and answered like any
                // final outcome, and nothing is queued or typed.
                let outcome = self.settle_noted(&mut state, &pending, status, notice);
                let _ = self.save(&state);
                drop(state);
                self.port.emit(&outcome);
                return Ok(outcome);
            }
            let id = pending.id.clone();
            let session = pending.session.clone();
            self.enqueue(&mut state, pending);
            if let Err(error) = self.save(&state) {
                // Not durable, so not queued: undo it whole.
                state.queue.retain(|p| p.id != id);
                state.records.remove(&scope);
                if let Some(rate) = state.rate.get_mut(&session) {
                    rate.pop_back();
                }
                return Err(format!("the wake store cannot be written: {error}"));
            }
            scope
        };
        self.pump();
        let state = lock(&self.state);
        Ok(state
            .records
            .get(&answer)
            .map(|record| record.outcome.clone())
            .unwrap_or_else(|| self.unknown_answer(&state, "", &PtySessionId::new(""))))
    }

    /// One inbox safety-net attempt, driven only by the activity owner's sweep.
    /// Reuses the attachment, draft, foreground and typing gates of normal delivery.
    pub(crate) fn submit_inbox_notice(
        &self,
        session: PtySessionId,
        notice: &str,
        originator: Originator,
        tools_ready: &dyn Fn() -> bool,
    ) -> codesense_pty_core::agent_cli::SubmitOutcome {
        use codesense_pty_core::agent_cli::SubmitOutcome;
        let Some(snapshot) = self.port.activity(&session) else {
            return SubmitOutcome::RefusedNoEffect;
        };
        let adapter = codesense_pty_core::agent_cli::adapter(snapshot.provider);
        if !matches!(
            adapter.inbox(),
            codesense_pty_core::agent_cli::InboxDelivery::Waiter { .. }
        ) || !adapter.allows_pty_recovery()
        {
            return SubmitOutcome::RefusedNoEffect;
        }
        let Ok(payload) = paste_payload(None, notice) else {
            return SubmitOutcome::RefusedNoEffect;
        };
        let now = self.clock.mono_ms();
        let pending = Pending {
            id: String::new(),
            scope: None,
            key: String::new(),
            session: session.clone(),
            binding: Some(Binding::of(&snapshot)),
            payload,
            marker: String::new(),
            originator,
            timer_id: None,
            enqueued_ms: now,
            deadline_ms: now.saturating_add(SUBMIT_CONFIRM_MS),
            max_wait_ms: SUBMIT_CONFIRM_MS,
            in_flight: true,
            budget: None,
        };
        match self.write_if_ready(&pending, tools_ready) {
            Attempt::Settled(_, Some(position)) => {
                lock(&self.state).held.insert(session, position);
                SubmitOutcome::Uncertain
            }
            _ => SubmitOutcome::RefusedNoEffect,
        }
    }

    /// Cancel `originator`'s queued delivery `key` on `session`
    /// (`undelivered(archived)`); a delivery being written or already final
    /// is answered as it stands.
    pub fn cancel_delivery(
        &self,
        originator: &Originator,
        session: &PtySessionId,
        key: &str,
    ) -> Option<WakeOutcome> {
        let scope = scoped(originator, session, key);
        let mut state = lock(&self.state);
        let index = state
            .queue
            .iter()
            .position(|p| p.scope.as_deref() == Some(scope.as_str()) && !p.in_flight);
        let Some(index) = index else {
            return state.records.get(&scope).map(|r| r.outcome.clone());
        };
        let pending = state.queue.remove(index)?;
        let outcome = self.settle(
            &mut state,
            &pending,
            DeliveryStatus::Undelivered {
                reason: UndeliveredReason::Archived,
            },
        );
        let _ = self.save(&state);
        drop(state);
        self.port.emit(&outcome);
        Some(outcome)
    }

    /// The outcome `originator`'s delivery `key` on `session` stands at
    /// (queued or final), while it is retained; an evicted key answers
    /// `unknown`. `None` when this host never took it.
    pub fn outcome_of(
        &self,
        originator: &Originator,
        session: &PtySessionId,
        key: &str,
    ) -> Option<WakeOutcome> {
        let scope = scoped(originator, session, key);
        let state = lock(&self.state);
        if let Some(record) = state.records.get(&scope) {
            return Some(record.outcome.clone());
        }
        state
            .evicted
            .contains_key(&key_hash(&scope))
            .then(|| self.unknown_answer(&state, key, session))
    }

    fn known(&self, state: &State, scope: &str, request: &DeliverRequest) -> Option<WakeOutcome> {
        if let Some(record) = state.records.get(scope) {
            return Some(record.outcome.clone());
        }
        state
            .evicted
            .contains_key(&key_hash(scope))
            .then(|| self.unknown_answer(state, &request.idempotency_key, &request.session))
    }

    fn unknown_answer(&self, state: &State, key: &str, session: &PtySessionId) -> WakeOutcome {
        WakeOutcome {
            seq: 0,
            delivery_id: String::new(),
            idempotency_key: key.to_string(),
            session: session.clone(),
            marker: String::new(),
            originator: Originator::default(),
            timer_id: None,
            status: DeliveryStatus::Unknown {
                detail: "this delivery's outcome is no longer retained".into(),
            },
            host_boot_id: state.boot_id.clone(),
            at_ms: self.clock.wall_ms(),
            notice: None,
        }
    }

    fn queued_outcome(&self, state: &State, pending: &Pending) -> WakeOutcome {
        WakeOutcome {
            seq: 0,
            delivery_id: pending.id.clone(),
            idempotency_key: pending.key.clone(),
            session: pending.session.clone(),
            marker: pending.marker.clone(),
            originator: pending.originator.clone(),
            timer_id: pending.timer_id.clone(),
            status: DeliveryStatus::Queued {
                deadline_ms: self.wall_of(pending.deadline_ms),
            },
            host_boot_id: state.boot_id.clone(),
            at_ms: self.clock.wall_ms(),
            notice: None,
        }
    }

    /// The wall-clock estimate of monotonic `ms`.
    fn wall_of(&self, ms: u64) -> u64 {
        let (mono, wall) = (self.clock.mono_ms(), self.clock.wall_ms());
        (i128::from(wall) + i128::from(ms) - i128::from(mono)).clamp(0, i128::from(u64::MAX)) as u64
    }

    fn enqueue(&self, state: &mut State, pending: Pending) {
        // The backstop counts every delivery queued, whatever asked for it.
        state
            .rate
            .entry(pending.session.clone())
            .or_default()
            .push_back((self.clock.wall_ms(), pending.marker.clone()));
        let outcome = self.queued_outcome(state, &pending);
        self.remember(state, &pending, outcome);
        state.queue.push_back(pending);
    }

    /// Keep a client delivery's outcome as its key's answer until its wait
    /// plus the grace, counted from when it was queued.
    fn remember(&self, state: &mut State, pending: &Pending, outcome: WakeOutcome) {
        if let Some(scope) = &pending.scope {
            let retain_until_ms = pending
                .enqueued_ms
                .saturating_add(pending.max_wait_ms)
                .saturating_add(IDEMPOTENCY_GRACE_MS);
            state.records.insert(
                scope.clone(),
                Record {
                    outcome,
                    retain_until_ms,
                },
            );
        }
    }

    /// Record a final `status` for `pending`: its key's answer and the log.
    fn settle(&self, state: &mut State, pending: &Pending, status: DeliveryStatus) -> WakeOutcome {
        self.settle_noted(state, pending, status, None)
    }

    /// [`settle`](Self::settle), with a notice for a person on the outcome.
    fn settle_noted(
        &self,
        state: &mut State,
        pending: &Pending,
        status: DeliveryStatus,
        notice: Option<String>,
    ) -> WakeOutcome {
        state.next_seq += 1;
        let wall = self.clock.wall_ms();
        // Went in, or may have: it counts against its budget and its timer's
        // gap.
        let went_in = matches!(
            status,
            DeliveryStatus::Delivered { .. } | DeliveryStatus::Unknown { .. }
        ) && !pending.payload.is_empty();
        if went_in {
            if let Some(budget) = &pending.budget {
                let budget = state.budgets.entry(budget.clone()).or_default();
                budget.total = budget.total.saturating_add(1);
                budget.day.push_back(wall);
                while budget
                    .day
                    .front()
                    .is_some_and(|at| wall.saturating_sub(*at) >= WAKE_DAY_MS)
                {
                    budget.day.pop_front();
                }
                budget.latest = Some((pending.id.clone(), wall));
                budget.touched = wall;
            }
        }
        let outcome = WakeOutcome {
            seq: state.next_seq,
            delivery_id: pending.id.clone(),
            idempotency_key: pending.key.clone(),
            session: pending.session.clone(),
            marker: pending.marker.clone(),
            originator: pending.originator.clone(),
            timer_id: pending.timer_id.clone(),
            status,
            host_boot_id: state.boot_id.clone(),
            at_ms: wall,
            notice,
        };
        if let Some(timer_id) = &pending.timer_id {
            if let Some(timer) = state
                .timers
                .iter_mut()
                .find(|timer| &timer.info.timer_id == timer_id)
            {
                if timer.pending.as_deref() == Some(pending.id.as_str()) {
                    timer.pending = None;
                    if went_in {
                        timer.last = Some((pending.id.clone(), wall));
                    }
                }
            }
        }
        self.remember(state, pending, outcome.clone());
        let retain_until_wall = state
            .records
            .get(pending.scope.as_deref().unwrap_or_default())
            .map(|record| self.wall_of(record.retain_until_ms))
            .unwrap_or(0);
        self.append(
            state,
            Line {
                outcome: outcome.clone(),
                scope: pending.scope.clone(),
                retain_until_wall,
            },
        );
        state.log.push_back(outcome.clone());
        while state.log.len() > OUTCOME_LOG_CAP {
            if let Some(dropped) = state.log.pop_front() {
                state.dropped_through = state.dropped_through.max(dropped.seq);
            }
        }
        outcome
    }

    /// A timer ended without a delivery: surfaced once in the log.
    fn timer_ended(
        &self,
        state: &mut State,
        timer: &Timer,
        reason: UndeliveredReason,
        now: u64,
    ) -> WakeOutcome {
        let pending = Pending {
            id: uuid::Uuid::new_v4().to_string(),
            scope: None,
            key: format!("timer:{}:ended", timer.info.timer_id),
            session: timer.info.session.clone(),
            binding: None,
            payload: String::new(),
            marker: timer_marker(&timer.info),
            originator: timer.info.originator.clone(),
            timer_id: Some(timer.info.timer_id.clone()),
            enqueued_ms: now,
            deadline_ms: now,
            max_wait_ms: 0,
            in_flight: false,
            budget: None,
        };
        self.settle(state, &pending, DeliveryStatus::Undelivered { reason })
    }

    fn append(&self, state: &State, line: Line) {
        let mut store = lock(&self.store);
        let Some(store) = store.as_mut() else {
            return;
        };
        if let Err(error) = store.append(&line) {
            log::warn!("session wake outcome not stored: {error}");
        }
        // Compact once the file holds well over what is still retained.
        let live = state.records.len() + state.log.len();
        if store.lines > 4096 && store.lines > 2 * live {
            let now = self.clock.mono_ms();
            let mut lines: Vec<Line> = state
                .records
                .iter()
                .filter(|(_, record)| record.outcome.status.is_final())
                .map(|(scope, record)| Line {
                    outcome: record.outcome.clone(),
                    scope: Some(scope.clone()),
                    retain_until_wall: self.wall_of(record.retain_until_ms.max(now)),
                })
                .collect();
            let kept: HashSet<u64> = lines.iter().map(|line| line.outcome.seq).collect();
            lines.extend(
                state
                    .log
                    .iter()
                    .filter(|outcome| !kept.contains(&outcome.seq))
                    .map(|outcome| Line {
                        outcome: outcome.clone(),
                        scope: None,
                        retain_until_wall: 0,
                    }),
            );
            lines.sort_by_key(|line| line.outcome.seq);
            if let Err(error) = store.compact(&lines) {
                log::warn!("session wake outcomes not compacted: {error}");
            }
        }
    }

    /// Write the meta document. `Err` when it could not be written durably.
    fn save(&self, state: &State) -> std::io::Result<()> {
        let mut store = lock(&self.store);
        let Some(store) = store.as_mut() else {
            return Ok(());
        };
        let meta = Meta {
            boot_id: state.boot_id.clone(),
            next_seq: state.next_seq,
            dropped_through: state.dropped_through,
            queue: state.queue.iter().cloned().collect(),
            timers: state.timers.clone(),
            held: state.held.clone(),
            evicted: state.evicted.iter().map(|(k, v)| (*k, *v)).collect(),
            budgets: state.budgets.clone(),
            rate: state.rate.clone(),
            tripped: state.tripped.clone(),
        };
        let result = store.save_meta(&meta);
        if let Err(error) = &result {
            log::warn!("session wake store not written: {error}");
        }
        result
    }

    /// The outcomes logged after `since`, and whether any of those were
    /// dropped by the cap before being read.
    pub fn outcomes(&self, since: u64) -> OutcomeLog {
        let state = lock(&self.state);
        OutcomeLog {
            host_boot_id: state.boot_id.clone(),
            outcomes: state
                .log
                .iter()
                .filter(|outcome| outcome.seq > since)
                .cloned()
                .collect(),
            overflow: since < state.dropped_through,
            last_seq: state.next_seq,
        }
    }

    /// One tick: fire due timers, forget expired keys, move the queue.
    pub fn tick(&self) {
        #[cfg(test)]
        if self.poisoned.load(Ordering::Acquire) {
            panic!("poisoned tick (test)");
        }
        self.fire_timers();
        self.expire();
        self.pump();
    }

    fn expire(&self) {
        let now = self.clock.mono_ms();
        let wall = self.clock.wall_ms();
        let mut state = lock(&self.state);
        let expired: Vec<String> = state
            .records
            .iter()
            .filter(|(_, r)| r.retain_until_ms <= now && r.outcome.status.is_final())
            .map(|(scope, _)| scope.clone())
            .collect();
        let forgotten = state.evicted.len();
        state.evicted.retain(|_, until| *until > wall);
        let aged = self.age_limits(&mut state, wall);
        let gone: Vec<PtySessionId> = state
            .held
            .keys()
            .chain(state.rate.keys())
            .filter(|session| !self.port.session_live(session))
            .cloned()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        if expired.is_empty() && forgotten == state.evicted.len() && gone.is_empty() && !aged {
            return;
        }
        for session in gone {
            // Gone sessions: their holds and backstop counts go too.
            if !state.queue.iter().any(|p| p.session == session) {
                state.held.remove(&session);
                state.rate.remove(&session);
                state.tripped.remove(&session);
            }
        }
        for scope in expired {
            state.records.remove(&scope);
            state
                .evicted
                .insert(key_hash(&scope), wall.saturating_add(EVICTED_RETENTION_MS));
        }
        let _ = self.save(&state);
    }

    /// Trim what the limits keep by age, never by count: backstop
    /// entries past their hour, budget days past their day, budgets unused
    /// for [`BUDGET_RETENTION_MS`]. True when anything went.
    fn age_limits(&self, state: &mut State, wall: u64) -> bool {
        let mut changed = false;
        for entries in state.rate.values_mut() {
            while entries
                .front()
                .is_some_and(|(at, _)| wall.saturating_sub(*at) >= SESSION_RATE_WINDOW_MS)
            {
                entries.pop_front();
                changed = true;
            }
        }
        let before = state.rate.len();
        state.rate.retain(|_, entries| !entries.is_empty());
        changed |= before != state.rate.len();
        let queued: HashSet<String> = state
            .queue
            .iter()
            .filter_map(|p| p.budget.clone())
            .collect();
        let before = state.budgets.len();
        state.budgets.retain(|key, budget| {
            queued.contains(key) || wall.saturating_sub(budget.touched) < BUDGET_RETENTION_MS
        });
        changed |= before != state.budgets.len();
        for budget in state.budgets.values_mut() {
            while budget
                .day
                .front()
                .is_some_and(|at| wall.saturating_sub(*at) >= WAKE_DAY_MS)
            {
                budget.day.pop_front();
                changed = true;
            }
        }
        changed
    }

    /// What `limits` and the host's backstop decide, now, for a delivery
    /// into `session`, before it is queued: `None` lets it in.
    /// Inside the routine's `min_gap` (since its last delivery into any
    /// session, or while that one still waits) it is `coalesced` into that
    /// one, or `undelivered(min_gap)` with `overlap: skip`. Its `max_wakes`
    /// spent is `undelivered(max_wakes)`; its rolling day spent,
    /// `undelivered(limit_reached)`. Deliveries still queued hold their place.
    /// Past the backstop, `undelivered(rate_limited)`, with a notice naming
    /// the top source the first time.
    fn limit_verdict(
        &self,
        state: &mut State,
        session: &PtySessionId,
        limits: Option<&WakeLimits>,
        wall: u64,
    ) -> Option<(DeliveryStatus, Option<String>)> {
        if let Some(limits) = limits {
            let queued: Vec<&Pending> = state
                .queue
                .iter()
                .filter(|p| p.budget.as_deref() == Some(limits.budget.as_str()))
                .collect();
            // Checked: in use, so kept (a monthly routine's count lasts).
            if let Some(budget) = state.budgets.get_mut(&limits.budget) {
                budget.touched = wall;
            }
            let budget = state.budgets.get(&limits.budget);
            if let Some(gap) = limits.min_gap_ms {
                let waiting = queued.first().map(|p| p.id.clone());
                let recent = budget
                    .and_then(|b| b.latest.as_ref())
                    .filter(|(_, at)| wall.saturating_sub(*at) < gap)
                    .map(|(id, _)| id.clone());
                if let Some(into) = waiting.or(recent) {
                    return Some((
                        match limits.gap {
                            GapPolicy::Coalesce => DeliveryStatus::Coalesced { into },
                            GapPolicy::Skip => DeliveryStatus::Undelivered {
                                reason: UndeliveredReason::MinGap,
                            },
                        },
                        None,
                    ));
                }
            }
            let reserved = u32::try_from(queued.len()).unwrap_or(u32::MAX);
            if let Some(max) = limits.max_wakes {
                if budget.map_or(0, |b| b.total).saturating_add(reserved) >= max {
                    return Some((
                        DeliveryStatus::Undelivered {
                            reason: UndeliveredReason::MaxWakes,
                        },
                        None,
                    ));
                }
            }
            if let Some(max) = limits.max_wakes_per_day {
                let today = budget.map_or(0, |b| {
                    b.day
                        .iter()
                        .filter(|at| wall.saturating_sub(**at) < WAKE_DAY_MS)
                        .count()
                });
                if u32::try_from(today)
                    .unwrap_or(u32::MAX)
                    .saturating_add(reserved)
                    >= max
                {
                    return Some((
                        DeliveryStatus::Undelivered {
                            reason: UndeliveredReason::LimitReached,
                        },
                        None,
                    ));
                }
            }
        }
        self.backstop(state, session, wall).map(|notice| {
            (
                DeliveryStatus::Undelivered {
                    reason: UndeliveredReason::RateLimited,
                },
                notice,
            )
        })
    }

    /// `Some` when `session` has had [`SESSION_RATE_LIMIT`] deliveries queued
    /// in the last [`SESSION_RATE_WINDOW_MS`]; inside it, the notice naming
    /// the top source the first time it trips. A trip lasts until the count
    /// falls below [`BACKSTOP_LOW_WATER`]: a loop that keeps running frees a
    /// slot now and then, and that is not a new trip to tell anyone about.
    fn backstop(
        &self,
        state: &mut State,
        session: &PtySessionId,
        wall: u64,
    ) -> Option<Option<String>> {
        let Some(entries) = state.rate.get(session) else {
            state.tripped.remove(session);
            return None;
        };
        let recent: Vec<&String> = entries
            .iter()
            .filter(|(at, _)| wall.saturating_sub(*at) < SESSION_RATE_WINDOW_MS)
            .map(|(_, source)| source)
            .collect();
        if recent.len() < BACKSTOP_LOW_WATER {
            state.tripped.remove(session);
        }
        if recent.len() < SESSION_RATE_LIMIT {
            return None;
        }
        if !state.tripped.insert(session.clone()) {
            return Some(None);
        }
        let mut counts: HashMap<&String, usize> = HashMap::new();
        for source in &recent {
            *counts.entry(source).or_default() += 1;
        }
        let (top, n) = counts
            .into_iter()
            .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(a.0)))
            .unwrap_or((recent[0], 0));
        Some(Some(format!(
            "Session {session} was asked for {} wakes in the last hour, more than the {SESSION_RATE_LIMIT} a session takes, so further wakes are refused for now. Most came from {top} ({n}). Wakes resume on their own once that slows down.",
            recent.len()
        )))
    }

    /// Move every session's queue head that can move, without letting one
    /// session hold up another: facts are gathered outside the lock, with one
    /// process probe, and each write runs on the runner.
    pub fn pump(&self) {
        let heads: Vec<Pending> = {
            let mut state = lock(&self.state);
            let now = self.clock.mono_ms();
            state.cooldown.retain(|_, until| *until > now);
            let busy: HashSet<&PtySessionId> = state
                .queue
                .iter()
                .filter(|p| p.in_flight)
                .map(|p| &p.session)
                .collect();
            let mut seen = HashSet::new();
            state
                .queue
                .iter()
                .filter(|p| {
                    !busy.contains(&p.session)
                        && !state.cooldown.contains_key(&p.session)
                        && seen.insert(p.session.clone())
                })
                .cloned()
                .collect()
        };
        if heads.is_empty() {
            return;
        }
        let facts = self.gather(&heads);
        let now = self.clock.mono_ms();
        let mut emitted = Vec::new();
        let mut attempts = Vec::new();
        let mut again = false;
        {
            let mut state = lock(&self.state);
            let mut changed = false;
            for (head, facts) in heads.iter().zip(facts) {
                let Some(index) = state
                    .queue
                    .iter()
                    .position(|p| p.id == head.id && !p.in_flight)
                else {
                    continue;
                };
                let held = state.held.get(&head.session).cloned();
                // A session at its prompt whose agent CLI is
                // not in the foreground for AGENT_QUIT_GRACE_MS has no agent
                // left to wake: `session_ended` now, so the desktop resolves
                // the wake again, instead of waiting it out to busy_timeout.
                let quit = quit_shell(&facts);
                let verdict = if quit {
                    let since = *state.quit_since.entry(head.session.clone()).or_insert(now);
                    if now.saturating_sub(since) >= AGENT_QUIT_GRACE_MS {
                        Verdict::Final(UndeliveredReason::SessionEnded)
                    } else {
                        self.verdict(head, &facts, held.as_ref(), now)
                    }
                } else {
                    state.quit_since.remove(&head.session);
                    self.verdict(head, &facts, held.as_ref(), now)
                };
                match verdict {
                    Verdict::Wait => {}
                    Verdict::Final(reason) => {
                        state.queue.remove(index);
                        emitted.push(self.settle(
                            &mut state,
                            head,
                            DeliveryStatus::Undelivered { reason },
                        ));
                        changed = true;
                        // The next in this session's queue may move now.
                        again = true;
                    }
                    // Held elsewhere: stays queued without touching the store.
                    Verdict::Go if !self.port.claim_available(&head.session, &head.originator) => {}
                    Verdict::Go => {
                        state.queue[index].in_flight = true;
                        attempts.push(head.clone());
                    }
                }
            }
            if !attempts.is_empty() {
                // In flight is on disk before a byte is written; if it cannot
                // be, nothing is written.
                if self.save(&state).is_err() {
                    for attempt in attempts.drain(..) {
                        if let Some(p) = state.queue.iter_mut().find(|p| p.id == attempt.id) {
                            p.in_flight = false;
                        }
                    }
                }
            } else if changed {
                let _ = self.save(&state);
            }
        }
        for outcome in &emitted {
            self.port.emit(outcome);
        }
        for pending in attempts {
            let Some(me) = self.me.upgrade() else {
                return;
            };
            self.runner.run(Box::new(move || me.attempt(pending)));
        }
        if again {
            self.pump();
        }
    }

    /// The facts for each head, one process probe for all of them.
    fn gather(&self, heads: &[Pending]) -> Vec<Facts> {
        let mut facts: Vec<Facts> = heads
            .iter()
            .map(|head| {
                let live = self.port.session_live(&head.session);
                Facts {
                    live,
                    snapshot: live.then(|| self.port.activity(&head.session)).flatten(),
                    foreground: None,
                    shell_idle: None,
                    input: None,
                }
            })
            .collect();
        let idle = |f: &Facts| f.snapshot.as_ref().is_some_and(Snapshot::takes_input);
        let queries: Vec<(usize, (PtySessionId, AgentCli))> = facts
            .iter()
            .enumerate()
            .filter(|(_, f)| idle(f))
            .filter_map(|(i, f)| {
                let provider = f.snapshot.as_ref()?.provider;
                Some((i, (heads[i].session.clone(), provider)))
            })
            .collect();
        if !queries.is_empty() {
            let probe: Vec<_> = queries.iter().map(|(_, q)| q.clone()).collect();
            let alive = self.port.agents_in_foreground(&probe);
            for ((i, (session, _)), alive) in queries.iter().zip(alive) {
                facts[*i].foreground = alive;
                if alive == Some(false) {
                    facts[*i].shell_idle = self.port.shell_idle(session);
                }
            }
        }
        for (i, f) in facts.iter_mut().enumerate() {
            if idle(f) {
                f.input = self.port.last_input(&heads[i].session);
            }
        }
        facts
    }

    /// Whether `pending` (its session's queue head) moves now.
    fn verdict(
        &self,
        pending: &Pending,
        facts: &Facts,
        held: Option<&(String, u64)>,
        now: u64,
    ) -> Verdict {
        if !facts.live {
            return Verdict::Final(UndeliveredReason::SessionEnded);
        }
        let Some(snapshot) = &facts.snapshot else {
            return Verdict::Final(UndeliveredReason::Unobservable);
        };
        if snapshot.exit.is_some()
            || pending
                .binding
                .as_ref()
                .is_some_and(|binding| *binding != Binding::of(snapshot))
        {
            return Verdict::Final(UndeliveredReason::SessionEnded);
        }
        if matches!(snapshot.state, Activity::Errored | Activity::Crashed) {
            return Verdict::Final(UndeliveredReason::AgentFailed);
        }
        if !snapshot.observed {
            // Waiting is not authority to type. A first hook or verified native
            // startup observation must arrive within the bounded grace.
            let grace = pending
                .enqueued_ms
                .saturating_add(OBSERVE_GRACE_MS)
                .min(pending.deadline_ms);
            return if awaits_observation(snapshot) && now < grace {
                Verdict::Wait
            } else {
                Verdict::Final(UndeliveredReason::Unobservable)
            };
        }
        let idle =
            matches!(snapshot.state, Activity::Idle | Activity::Done) && snapshot.asks.is_empty();
        let position = (
            snapshot.identity.invocation_id.clone(),
            snapshot.turn_generation,
        );
        let draft = idle
            && facts
                .input
                .as_ref()
                .is_some_and(|input| input.draft.as_ref() == Some(&position));
        if now >= pending.deadline_ms {
            return Verdict::Final(if draft {
                UndeliveredReason::DraftPending
            } else {
                UndeliveredReason::BusyTimeout
            });
        }
        if !idle {
            // Thinking, Working, and needs-you: never typed into.
            return Verdict::Wait;
        }
        if held.is_some_and(|(invocation, generation)| {
            *invocation == position.0 && position.1 <= *generation
        }) {
            // One delivery per turn: wait for the agent to take the last one.
            return Verdict::Wait;
        }
        if let Some(input) = &facts.input {
            if now < input.at_ms.saturating_add(QUIET_INPUT_MS) {
                return Verdict::Wait;
            }
        }
        if draft {
            // A draft (or an open menu) since this turn began: never paste
            // onto it.
            return Verdict::Wait;
        }
        if facts.foreground != Some(true) {
            return Verdict::Wait;
        }
        Verdict::Go
    }

    /// One attempt, off the queue's lock: claim, re-check, write, confirm.
    fn attempt(&self, pending: Pending) {
        let status =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.write(&pending)))
                .unwrap_or_else(|_| {
                    Attempt::Settled(
                        DeliveryStatus::Unknown {
                            detail: "the write failed unexpectedly".into(),
                        },
                        None,
                    )
                });
        let outcome = {
            let mut state = lock(&self.state);
            match status {
                // Not written: back in the queue.
                Attempt::Requeue => {
                    if let Some(p) = state.queue.iter_mut().find(|p| p.id == pending.id) {
                        p.in_flight = false;
                    }
                    let until = self.clock.mono_ms().saturating_add(REQUEUE_COOLDOWN_MS);
                    state.cooldown.insert(pending.session.clone(), until);
                    let _ = self.save(&state);
                    None
                }
                Attempt::Settled(status, written) => {
                    state.queue.retain(|p| p.id != pending.id);
                    if let Some(position) = written {
                        state.held.insert(pending.session.clone(), position);
                    }
                    let outcome = self.settle(&mut state, &pending, status);
                    let _ = self.save(&state);
                    Some(outcome)
                }
            }
        };
        if let Some(outcome) = outcome {
            self.port.emit(&outcome);
            self.pump();
        }
    }

    fn write(&self, pending: &Pending) -> Attempt {
        self.write_if_ready(pending, &|| true)
    }

    fn write_if_ready(&self, pending: &Pending, tools_ready: &dyn Fn() -> bool) -> Attempt {
        let session = &pending.session;
        let _claim = match self.port.claim(session, &pending.originator) {
            Claim::Held(claim) => claim,
            Claim::Elsewhere => return Attempt::Requeue,
        };
        // Re-run the verdict while the attachment is held: nothing changed
        // between the check and the write.
        let facts = self.gather(std::slice::from_ref(pending));
        let Some(facts) = facts.into_iter().next() else {
            return Attempt::Requeue;
        };
        let held = lock(&self.state).held.get(session).cloned();
        match self.verdict(pending, &facts, held.as_ref(), self.clock.mono_ms()) {
            Verdict::Go => {}
            Verdict::Wait => return Attempt::Requeue,
            Verdict::Final(reason) => {
                return Attempt::Settled(DeliveryStatus::Undelivered { reason }, None)
            }
        }
        let Some(snapshot) = facts.snapshot else {
            return Attempt::Requeue;
        };
        // Read last of all (it asks the emulator), and only here, off any
        // request thread.
        if self.port.bracketed_paste(session) == Some(false) {
            return Attempt::Requeue;
        }
        if !tools_ready() {
            return Attempt::Requeue;
        }
        let position = (
            snapshot.identity.invocation_id.clone(),
            snapshot.turn_generation,
        );
        let pasted_at = self.clock.mono_ms();
        if let Err(error) = self.port.write(session, pending.payload.as_bytes()) {
            return Attempt::Settled(
                if self.port.session_live(session) {
                    DeliveryStatus::Unknown { detail: error }
                } else {
                    DeliveryStatus::Undelivered {
                        reason: UndeliveredReason::SessionEnded,
                    }
                },
                Some(position),
            );
        }
        self.settle_echo(session);
        // A human key in the gap: their Enter must not submit our text, nor
        // ours theirs. No Enter; the session stays held.
        if self
            .port
            .last_input(session)
            .is_some_and(|input| input.at_ms >= pasted_at)
        {
            return Attempt::Settled(
                DeliveryStatus::Unknown {
                    detail: "viewer input arrived between the paste and its Enter; Enter not sent"
                        .into(),
                },
                Some(position),
            );
        }
        if !tools_ready() {
            return Attempt::Settled(
                DeliveryStatus::Unknown {
                    detail: "CodeSense tools disconnected between the paste and its Enter; Enter not sent".into(),
                },
                Some(position),
            );
        }
        if let Err(error) = self.port.write(session, SUBMIT_KEY.as_bytes()) {
            return Attempt::Settled(
                DeliveryStatus::Unknown {
                    detail: format!("the text was written but not submitted: {error}"),
                },
                Some(position),
            );
        }
        drop(_claim);
        // Confirm from activity: a new turn, or the agent busy.
        let deadline = self.clock.mono_ms().saturating_add(SUBMIT_CONFIRM_MS);
        loop {
            if let Some(now) = self.port.activity(session) {
                if now.identity.invocation_id == position.0
                    && (now.turn_generation > position.1
                        || matches!(now.state, Activity::Thinking | Activity::Working))
                {
                    return Attempt::Settled(
                        DeliveryStatus::Delivered {
                            at_ms: self.clock.wall_ms(),
                        },
                        Some(position),
                    );
                }
            }
            if self.clock.mono_ms() >= deadline {
                return Attempt::Settled(
                    DeliveryStatus::Unknown {
                        detail: "written, but the agent did not show the submit".into(),
                    },
                    Some(position),
                );
            }
            self.clock.sleep_ms(POLL_MS * 2);
        }
    }

    /// Between the paste and its Enter: until the echo has been quiet for the
    /// minimum gap (at most the maximum), and never less than the minimum.
    fn settle_echo(&self, session: &PtySessionId) {
        let start = self.clock.mono_ms();
        let max = start.saturating_add(ENTER_GAP_MAX_MS);
        let mut last = self.port.output_seq(session);
        let mut quiet_since = start;
        loop {
            let now = self.clock.mono_ms();
            if now >= quiet_since.saturating_add(ENTER_GAP_MIN_MS) || now >= max {
                break;
            }
            self.clock.sleep_ms(POLL_MS);
            let seq = self.port.output_seq(session);
            if seq != last {
                last = seq;
                quiet_since = self.clock.mono_ms();
            }
        }
        let waited = self.clock.mono_ms().saturating_sub(start);
        if waited < ENTER_GAP_MIN_MS {
            self.clock.sleep_ms(ENTER_GAP_MIN_MS - waited);
        }
    }

    /// Set a timer on `spec.session`.
    pub fn set_timer(&self, spec: TimerSpec) -> Result<TimerInfo, String> {
        let now = self.clock.mono_ms();
        let wall = self.clock.wall_ms();
        if !valid_label(&spec.name) {
            return Err("timer name must be 1-256 bytes without controls".into());
        }
        if !valid_label(&spec.created_by) {
            return Err("timer author must be 1-256 bytes without controls".into());
        }
        if spec.max_wait_ms == 0 || spec.max_wait_ms > MAX_WAIT_LIMIT_MS {
            return Err(format!(
                "max wait must be between 1 ms and {MAX_WAIT_LIMIT_MS} ms"
            ));
        }
        if spec.count == Some(0) {
            return Err("count must be at least 1".into());
        }
        if spec
            .min_gap_ms
            .is_some_and(|gap| !(MIN_TIMER_INTERVAL_MS..=MAX_MIN_GAP_MS).contains(&gap))
        {
            return Err("min_gap must be between 1 minute and 7 days".into());
        }
        compose(Some(&timer_header(&spec.name, u32::MAX)), &spec.text)?;
        let (first, first_wall) = first_fire(&spec.schedule, now, wall)?;
        let until_ms = spec.until_ms.map(|until| now.saturating_add(until));
        if until_ms.is_some_and(|until| until <= first) {
            return Err("until must be after the first fire".into());
        }
        if !self.port.session_live(&spec.session) {
            return Err(format!("session {} is not running", spec.session));
        }
        let snapshot = self.port.activity(&spec.session);
        if spec.require_observed && !observable(snapshot.as_ref()) {
            return Err(format!(
                "session {} cannot be observed (CodeSense cannot tell when its agent is idle: a plain shell, or an agent whose activity is not reported yet, like Codex), so a timer there could never be delivered",
                spec.session
            ));
        }
        let binding = snapshot.as_ref().map(Binding::of);
        let mut state = lock(&self.state);
        if state.timers.len() >= MACHINE_TIMER_CAP {
            return Err(format!(
                "this machine already has {MACHINE_TIMER_CAP} active timers"
            ));
        }
        if state
            .timers
            .iter()
            .filter(|timer| timer.info.session == spec.session)
            .count()
            >= SESSION_TIMER_CAP
        {
            return Err(format!(
                "this session already has {SESSION_TIMER_CAP} active timers"
            ));
        }
        let info = TimerInfo {
            timer_id: uuid::Uuid::new_v4().to_string(),
            session: spec.session,
            name: spec.name,
            created_by: spec.created_by,
            originator: spec.originator,
            schedule: spec.schedule,
            text: spec.text,
            next_at_ms: first_wall.unwrap_or_else(|| self.wall_of(first)),
            fires: 0,
            count: spec.count,
            until_at_ms: until_ms.map(|until| self.wall_of(until)),
            created_at_ms: wall,
            min_gap_ms: spec.min_gap_ms,
        };
        state.timers.push(Timer {
            info: info.clone(),
            max_wait_ms: spec.max_wait_ms,
            binding,
            next_ms: first,
            next_wall: first_wall,
            pending: None,
            last: None,
        });
        self.save(&state)
            .map_err(|error| format!("the wake store cannot be written: {error}"))?;
        Ok(info)
    }

    /// The active timers, of `session` or of every session.
    pub fn timers(&self, session: Option<&PtySessionId>) -> Vec<TimerInfo> {
        lock(&self.state)
            .timers
            .iter()
            .filter(|timer| session.is_none_or(|session| &timer.info.session == session))
            .map(|timer| timer.info.clone())
            .collect()
    }

    /// The timers stamped with `viewer` (set before their desktop
    /// named itself) whose originating install is `host_part` are stamped
    /// with that desktop's `identity`; how many.
    pub fn restamp(&self, viewer: &str, host_part: &str, identity: &str) -> usize {
        let mut state = lock(&self.state);
        let mut count = 0;
        for timer in state.timers.iter_mut() {
            let originator = &mut timer.info.originator;
            if originator.desktop == viewer
                && crate::offline_mcp::same_install(originator.routine_host.as_deref(), host_part)
            {
                originator.desktop = identity.to_string();
                count += 1;
            }
        }
        if count > 0 {
            let _ = self.save(&state);
        }
        count
    }

    /// Cancel `session`'s timer `timer_id` and the fire it has queued
    /// (`undelivered(archived)`); whether it was active.
    pub fn cancel_timer(&self, session: &PtySessionId, timer_id: &str) -> bool {
        let mut state = lock(&self.state);
        let Some(index) = state
            .timers
            .iter()
            .position(|timer| timer.info.timer_id == timer_id && &timer.info.session == session)
        else {
            return false;
        };
        let timer = state.timers.remove(index);
        let mut emitted = Vec::new();
        if let Some(fire) = timer.pending {
            if let Some(i) = state
                .queue
                .iter()
                .position(|p| p.id == fire && !p.in_flight)
            {
                if let Some(pending) = state.queue.remove(i) {
                    emitted.push(self.settle(
                        &mut state,
                        &pending,
                        DeliveryStatus::Undelivered {
                            reason: UndeliveredReason::Archived,
                        },
                    ));
                }
            }
        }
        let _ = self.save(&state);
        drop(state);
        for outcome in &emitted {
            self.port.emit(outcome);
        }
        true
    }

    fn fire_timers(&self) {
        let now = self.clock.mono_ms();
        let wall = self.clock.wall_ms();
        let sessions: Vec<PtySessionId> = lock(&self.state)
            .timers
            .iter()
            .map(|timer| timer.info.session.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        if sessions.is_empty() {
            return;
        }
        let alive: HashMap<PtySessionId, (bool, Option<Snapshot>)> = sessions
            .into_iter()
            .map(|session| {
                let live = self.port.session_live(&session);
                let snapshot = live.then(|| self.port.activity(&session)).flatten();
                (session, (live, snapshot))
            })
            .collect();
        let mut state = lock(&self.state);
        let mut emitted = Vec::new();
        let mut changed = false;
        let timers = std::mem::take(&mut state.timers);
        let mut kept = Vec::with_capacity(timers.len());
        for mut timer in timers {
            let (live, snapshot) = alive
                .get(&timer.info.session)
                .cloned()
                .unwrap_or((false, None));
            let ended = !live
                || snapshot.as_ref().is_some_and(|s| {
                    s.exit.is_some()
                        || timer
                            .binding
                            .as_ref()
                            .is_some_and(|binding| *binding != Binding::of(s))
                });
            if ended {
                // Timers die with their agent: said once, never re-fired.
                changed = true;
                emitted.push(self.timer_ended(
                    &mut state,
                    &timer,
                    UndeliveredReason::SessionEnded,
                    now,
                ));
                continue;
            }
            // A timer set before its agent's first activity binds to it now.
            if timer.binding.is_none() {
                if let Some(snapshot) = &snapshot {
                    timer.binding = Some(Binding::of(snapshot));
                    changed = true;
                }
            }
            let due = match timer.next_wall {
                Some(at) => wall >= at,
                None => timer.next_ms <= now,
            };
            if !due {
                kept.push(timer);
                continue;
            }
            changed = true;
            let marker = timer_marker(&timer.info);
            // A fire inside the timer's `min_gap` collapses into
            // its last delivery, like one while that still waits.
            let in_gap = timer.info.min_gap_ms.and_then(|gap| {
                timer
                    .last
                    .as_ref()
                    .filter(|(_, at)| wall.saturating_sub(*at) < gap)
                    .map(|(id, _)| id.clone())
            });
            let refused = if timer.pending.is_none() && in_gap.is_none() {
                self.backstop(&mut state, &timer.info.session, wall)
            } else {
                None
            };
            match timer.pending.clone().or(in_gap) {
                // A fire while the last one still waits collapses into it,
                // and does not count against the timer.
                Some(into) => {
                    let pending = Pending {
                        id: uuid::Uuid::new_v4().to_string(),
                        scope: None,
                        key: format!("timer:{}:coalesced:{now}", timer.info.timer_id),
                        session: timer.info.session.clone(),
                        binding: None,
                        payload: String::new(),
                        marker,
                        originator: timer.info.originator.clone(),
                        timer_id: Some(timer.info.timer_id.clone()),
                        enqueued_ms: now,
                        deadline_ms: now,
                        max_wait_ms: 0,
                        in_flight: false,
                        budget: None,
                    };
                    emitted.push(self.settle(
                        &mut state,
                        &pending,
                        DeliveryStatus::Coalesced { into },
                    ));
                }
                // The backstop: this fire is refused, the timer goes on.
                None if refused.is_some() => {
                    let pending = Pending {
                        id: uuid::Uuid::new_v4().to_string(),
                        scope: None,
                        key: format!("timer:{}:rate_limited:{now}", timer.info.timer_id),
                        session: timer.info.session.clone(),
                        binding: None,
                        payload: String::new(),
                        marker,
                        originator: timer.info.originator.clone(),
                        timer_id: Some(timer.info.timer_id.clone()),
                        enqueued_ms: now,
                        deadline_ms: now,
                        max_wait_ms: 0,
                        in_flight: false,
                        budget: None,
                    };
                    emitted.push(self.settle_noted(
                        &mut state,
                        &pending,
                        DeliveryStatus::Undelivered {
                            reason: UndeliveredReason::RateLimited,
                        },
                        refused.flatten(),
                    ));
                }
                None => {
                    timer.info.fires += 1;
                    let fire = timer.info.fires;
                    let header = timer_header(&timer.info.name, fire);
                    match paste_payload(Some(&header), &timer.info.text) {
                        Ok(payload) => {
                            let pending = Pending {
                                id: uuid::Uuid::new_v4().to_string(),
                                scope: None,
                                key: format!("timer:{}:{fire}", timer.info.timer_id),
                                session: timer.info.session.clone(),
                                binding: timer.binding.clone(),
                                payload,
                                marker,
                                originator: timer.info.originator.clone(),
                                timer_id: Some(timer.info.timer_id.clone()),
                                enqueued_ms: now,
                                deadline_ms: now.saturating_add(timer.max_wait_ms),
                                max_wait_ms: timer.max_wait_ms,
                                in_flight: false,
                                budget: None,
                            };
                            timer.pending = Some(pending.id.clone());
                            self.enqueue(&mut state, pending);
                        }
                        Err(error) => log::warn!("timer {}: {error}", timer.info.timer_id),
                    }
                }
            }
            let Some((next, next_wall)) = next_fire(&timer, now, wall) else {
                continue;
            };
            let next_at = next_wall.unwrap_or_else(|| self.wall_of(next));
            let exhausted = timer
                .info
                .count
                .is_some_and(|count| timer.info.fires >= count)
                || timer.info.until_at_ms.is_some_and(|until| next_at > until);
            if exhausted {
                continue;
            }
            timer.next_ms = next;
            timer.next_wall = next_wall;
            timer.info.next_at_ms = next_at;
            kept.push(timer);
        }
        state.timers = kept;
        if changed {
            let _ = self.save(&state);
        }
        drop(state);
        for outcome in &emitted {
            self.port.emit(outcome);
        }
    }

    /// One supervised tick: a panic is caught and counted; past
    /// [`TICK_FAILURES`] in a row the queue stops and the capability is
    /// withdrawn.
    pub(crate) fn supervised_tick(&self) {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.tick())) {
            Ok(()) => self.failures.store(0, Ordering::Release),
            Err(_) => {
                let failures = self.failures.fetch_add(1, Ordering::AcqRel) + 1;
                log::error!("session wake tick failed ({failures} in a row)");
                if failures >= TICK_FAILURES {
                    self.failed.store(true, Ordering::Release);
                    self.fail_queue();
                }
            }
        }
    }

    /// The queue stopped: every delivery still waiting is
    /// `undelivered(unavailable)`, said once.
    fn fail_queue(&self) {
        let mut state = lock(&self.state);
        let waiting: Vec<Pending> = state
            .queue
            .iter()
            .filter(|p| !p.in_flight)
            .cloned()
            .collect();
        state.queue.retain(|p| p.in_flight);
        let outcomes: Vec<WakeOutcome> = waiting
            .iter()
            .map(|pending| {
                self.settle(
                    &mut state,
                    pending,
                    DeliveryStatus::Undelivered {
                        reason: UndeliveredReason::Unavailable,
                    },
                )
            })
            .collect();
        let _ = self.save(&state);
        drop(state);
        for outcome in &outcomes {
            self.port.emit(outcome);
        }
    }

    #[cfg(test)]
    fn fail_store(&self, fail: bool) {
        if let Some(store) = lock(&self.store).as_mut() {
            store.fail_writes = fail;
        }
    }
}

enum Attempt {
    Requeue,
    /// The final status, and the agent position it was written at (when a
    /// write began).
    Settled(DeliveryStatus, Option<(String, u64)>),
}

/// Whether a delivery into a session with `snapshot` could ever be typed:
/// an observed agent, or a fresh one whose first hook is still due.
fn observable(snapshot: Option<&Snapshot>) -> bool {
    snapshot.is_some_and(|s| s.observed || (awaits_observation(s) && s.exit.is_none()))
}

/// Whether a first hook or a bound native channel could establish observation.
/// An unqualified launch still expires unobservable; this only grants waiting.
fn awaits_observation(snapshot: &Snapshot) -> bool {
    let adapter = codesense_pty_core::agent_cli::adapter(snapshot.provider);
    adapter.session_policy().observed_from_start || adapter.control().native_readiness
}

fn timer_marker(info: &TimerInfo) -> String {
    format!("{} (set by {})", info.name, info.created_by)
}

/// The monotonic time `wall_target` falls at.
fn mono_at(now: u64, wall: u64, wall_target: u64) -> u64 {
    now.saturating_add(wall_target.saturating_sub(wall))
}

/// The first fire of `schedule` set now.
fn first_fire(schedule: &TimerSchedule, now: u64, wall: u64) -> Result<(u64, Option<u64>), String> {
    match schedule {
        TimerSchedule::After { after_ms, every_ms } => {
            if every_ms.is_some_and(|every| every < MIN_TIMER_INTERVAL_MS) {
                return Err(format!(
                    "a repeating timer fires at most every {} s",
                    MIN_TIMER_INTERVAL_MS / 1000
                ));
            }
            if *after_ms > MAX_WAIT_LIMIT_MS * 366 {
                return Err("the first fire is too far away".into());
            }
            Ok((now.saturating_add(*after_ms), None))
        }
        TimerSchedule::Cron { cron, tz } => {
            let schedule = CronSchedule::parse(cron, tz)?;
            let first = schedule
                .next_after_ms(wall)
                .ok_or_else(|| format!("cron {cron:?} never fires"))?;
            if let Some(second) = schedule.next_after_ms(first) {
                if second - first < MIN_TIMER_INTERVAL_MS {
                    return Err("a repeating timer fires at most every 60 s".into());
                }
            }
            Ok((mono_at(now, wall, first), Some(first)))
        }
    }
}

/// The fire after the one due at `timer.next_ms` (and, for cron, its wall
/// time). Fires missed meanwhile are skipped: they collapsed into this one.
fn next_fire(timer: &Timer, now: u64, wall: u64) -> Option<(u64, Option<u64>)> {
    match &timer.info.schedule {
        TimerSchedule::After { every_ms, .. } => {
            let every = (*every_ms)?;
            let mut next = timer.next_ms.saturating_add(every);
            if next <= now {
                let missed = (now - next) / every + 1;
                next = next.saturating_add(missed.saturating_mul(every));
            }
            Some((next, None))
        }
        TimerSchedule::Cron { cron, tz } => {
            let at = CronSchedule::parse(cron, tz).ok()?.next_after_ms(wall)?;
            Some((mono_at(now, wall, at), Some(at)))
        }
    }
}

/// Run `wake`'s supervised tick every [`TICK`] for as long as it lives.
pub(crate) fn start_ticker(wake: Weak<SessionWake>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("session-wake".into())
        .spawn(move || loop {
            std::thread::sleep(TICK);
            match wake.upgrade() {
                Some(wake) if wake.healthy() => wake.supervised_tick(),
                _ => return,
            }
        })
        .map(|_| ())
}
