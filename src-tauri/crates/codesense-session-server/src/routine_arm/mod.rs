// SPDX-License-Identifier: MIT
//! `RoutineArmV1` on the host: persistent
//! routines armed on this machine, fired on its own clock whether or not a
//! desktop is connected.
//!
//! - **Schedules** are 5-field crons in the routine's IANA zone, evaluated on
//!   the wall clock with the tzdb compiled into this binary (the same rules as
//!   session timers and the app's schedules). A fire whose instant passed
//!   more than [`MISSED_GRACE_MS`] ago was missed while the machine was down
//!   or asleep: `missed: skip` records one skipped run, `run_once` fires once.
//! - **Fencing.** The host keeps, per routine, the highest arm epoch it has
//!   seen: the armed one, or a tombstone left by a disarm. An arm or a disarm
//!   older than that is refused, and each fire re-checks the epoch first, so
//!   a desktop holding a stale definition can never bring a routine back.
//!   Every move between a desktop and a machine takes a new epoch; an arm at
//!   a tombstone's own epoch is the same placement coming back (a pause or an
//!   archive undone) and is accepted.
//! - **A fire is claimed before it acts**: its cursor moves and a `firing`
//!   run is written durably first. Its id is derived from the routine, the
//!   epoch and the occurrence, so no occurrence fires twice, across restarts
//!   too. A claim found still `firing` after a restart is `unknown` (R5): it
//!   may have delivered or launched, and is never acted on again.
//! - **Wake** fires go through this host's `SessionWakeV1` queue with the run
//!   id as the idempotency key (idle-gated, never typed into a busy or
//!   needs-you session), into the routine agent's live session in its
//!   workspace, idle first. With none, the agent is started like a fresh
//!   fire, single-flight: a session this engine started for the same agent
//!   and workspace counts as live until it ends (R1).
//! - **Fresh** fires start the agent only from the launch recipe a desktop
//!   pushed with the arm, into the workspace it prepared. No recipe, or a
//!   workspace whose prepared files are gone: `undelivered(needs_preparation)`.
//!   A fresh run is open until its agent finishes it (`routine_done`,
//!   `signal_done`), its session ends (`exited`), or it passes its
//!   `max_runtime` (`timed_out`, its session stopped). Overlap is decided on
//!   that open run, never on whether its session still lives: an agent that
//!   finished its run and idles at its prompt does not hold the next one.
//! - **One flight per agent and workspace.** A wake that finds no session
//!   launches if absent through the host's single flight, which every
//!   launcher shares (a desktop's launch-if-absent too): a session already
//!   running or being started there is woken instead, once it is up.
//! - **Runs** are appended to a capped log (one entry per status change) that
//!   desktops read with a cursor when they (re)connect.
//!
//! The store lives under `<root>/routine-arms/` (`meta.json`, and the run log
//! in `outcomes.jsonl`), beside the wake store.
//!
//! Who may arm is the connection's business: this host trusts its admitted
//! connections, as for wakes, and stamps each arm with the connection's viewer.

pub(crate) mod host_port;
pub mod local_mcp;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use chrono::{SecondsFormat, TimeZone, Utc};
use codesense_pty_core::domain::PtySessionId;
use codesense_pty_core::routine_arm::{
    arm_run_id, render_static, routine_header, ArmAnswer, ArmCompleteAnswer, ArmFireAnswer,
    ArmInfo, ArmLaunch, ArmList, ArmMissed, ArmMode, ArmOverlap, ArmRefusal, ArmRun, ArmRunLog,
    ArmRunStatus, ArmTarget, ArmTombstone, RoutineArm, ARM_RUN_LOG_CAP, ARM_SCHEDULE_CAP,
    DEFAULT_MAX_RUNTIME_MS, MACHINE_ARM_CAP, MAX_TASK_BYTES, MIN_ARM_INTERVAL_MS, MISSED_GRACE_MS,
};
use codesense_pty_core::session_wake::{
    compose, valid_label, DeliverRequest, DeliveryStatus, Originator, UndeliveredReason,
    WakeOutcome, MAX_WAIT_LIMIT_MS,
};
use serde::{Deserialize, Serialize};

use crate::session_wake::schedule::CronSchedule;
use crate::session_wake::store::Store;
use crate::session_wake::Clock;

/// How often the host evaluates its armed routines.
pub(crate) const TICK: Duration = Duration::from_secs(5);

/// How long a wake waits for a session of its agent that is still being
/// started (by this host or a desktop) before giving up on this fire.
pub(crate) const STARTING_WAIT: Duration = Duration::from_secs(90);

/// A live session of an armed routine's agent, as this host sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmCandidate {
    pub session: PtySessionId,
    /// Its agent is observed idle at its prompt (deliverable now).
    pub idle: bool,
    /// Larger is more recently active (the layout order of its tab).
    pub recency: u64,
    /// Reserved and being started (by this host or a desktop): not up yet,
    /// but it counts as the agent's session (never a second launch).
    pub starting: bool,
}

/// Why a launch failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchFailure {
    /// Nothing was started.
    NotStarted(String),
    /// It may have started: never tried again (R5).
    Unknown(String),
    /// A launch-if-absent found the agent running, or being started, in the
    /// workspace (the host's single flight): that session, to wake instead.
    Busy(PtySessionId),
}

/// What the engine reads and does on the host.
pub trait ArmPort: Send + Sync {
    /// Live sessions of `target.agent` in `target`'s workspace, launched by
    /// `target.owner` (or untagged).
    fn candidates(&self, target: &ArmTarget) -> Vec<ArmCandidate>;
    fn session_live(&self, session: &PtySessionId) -> bool;
    /// Whether `launch` can start the agent in `target.cwd` now: the
    /// workspace and everything its preparation wrote are still there.
    fn prepared(&self, target: &ArmTarget, launch: &ArmLaunch) -> Result<(), String>;
    /// Start the agent in a new tab as `session`, with `prompt` first.
    /// `if_absent`: only when no session of the agent runs, or is being
    /// started, in the workspace ([`LaunchFailure::Busy`] otherwise), checked
    /// and reserved in one step against every launcher on the host.
    /// `desktop` is the viewer that manages the routine: its connection, if
    /// connected, relays the agent's CodeSense MCP.
    fn launch(
        &self,
        target: &ArmTarget,
        launch: &ArmLaunch,
        session: &PtySessionId,
        prompt: &str,
        if_absent: bool,
        desktop: &str,
    ) -> Result<(), LaunchFailure>;
    /// Stop `session` (a fresh run past its `max_runtime`).
    fn kill(&self, session: &PtySessionId);
    /// Queue a delivery through this host's wake queue.
    fn deliver(&self, request: DeliverRequest) -> Result<WakeOutcome, String>;
    /// Where a delivery stands now.
    fn delivery(
        &self,
        originator: &Originator,
        session: &PtySessionId,
        key: &str,
    ) -> Option<WakeOutcome>;
    /// Cancel a queued delivery (`undelivered(archived)`).
    fn cancel(&self, originator: &Originator, session: &PtySessionId, key: &str);
}

/// One armed routine and its cursors.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Armed {
    arm: RoutineArm,
    armed_at_ms: u64,
    /// Per schedule, the wall-clock instant of its next occurrence.
    next: Vec<Option<u64>>,
}

/// A fresh run whose agent was started and has not finished it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Running {
    /// Its `launched` entry.
    run: ArmRun,
    session: PtySessionId,
    /// Past this (host wall clock), it times out and its session stops.
    deadline_ms: u64,
}

/// What a routine carries between its runs here, armed or not.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Carried {
    /// The last occurrence fired (or skipped) here, or the floor a desktop
    /// pushed: nothing at or before it fires.
    last_fired_ms: Option<u64>,
    /// The last successful run's summary (`{{last_run_summary}}`).
    last_summary: Option<String>,
    /// When the last run fired (`{{last_run_at}}`).
    last_run_at_ms: Option<u64>,
}

/// A run that is not final yet.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Open {
    run: ArmRun,
    /// The session a queued wake waits for.
    session: Option<PtySessionId>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Meta {
    boot_id: String,
    next_seq: u64,
    dropped_through: u64,
    armed: Vec<Armed>,
    /// The highest epoch a disarm left, per routine.
    tombs: BTreeMap<String, u64>,
    open: Vec<Open>,
    running: Vec<Running>,
    carried: BTreeMap<String, Carried>,
}

#[derive(Default)]
struct State {
    boot_id: String,
    next_seq: u64,
    dropped_through: u64,
    armed: Vec<Armed>,
    tombs: BTreeMap<String, u64>,
    open: Vec<Open>,
    running: Vec<Running>,
    carried: BTreeMap<String, Carried>,
    log: VecDeque<ArmRun>,
    /// Sessions this engine started this lifetime, by (agent, workspace):
    /// live until they end, whatever the layout shows yet (single-flight).
    launched: HashMap<(String, String), Vec<PtySessionId>>,
}

/// The host's armed routines.
pub struct RoutineArms {
    port: Arc<dyn ArmPort>,
    clock: Arc<dyn Clock>,
    store: Mutex<Option<Store>>,
    state: Mutex<State>,
    /// One fire at a time: a fire acts outside the state lock.
    firing: Mutex<()>,
    failed: AtomicBool,
    /// Why this host cannot run routines without a desktop (it refuses
    /// every arm with it), if it cannot.
    unsupported: Option<String>,
    /// How long a wake waits for a session still being started.
    starting_wait: Duration,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

fn invalid(detail: impl Into<String>) -> ArmRefusal {
    ArmRefusal::Invalid {
        detail: detail.into(),
    }
}

/// What one claimed fire does, decided under the lock and acted outside it.
struct Claimed {
    run: ArmRun,
    arm: RoutineArm,
    /// What `{{last_run_summary}}` and `{{last_run_at}}` render as.
    carried: Carried,
}

impl RoutineArms {
    /// The armed routines of host lifetime `boot_id`, kept in `store` (a
    /// directory). A store another lifetime left is reconciled: runs it left
    /// `firing` are `unknown`, and the sessions it started are gone.
    pub fn open(
        port: Arc<dyn ArmPort>,
        clock: Arc<dyn Clock>,
        boot_id: String,
        store: Option<PathBuf>,
    ) -> Arc<Self> {
        Self::open_with(port, clock, boot_id, store, None, STARTING_WAIT)
    }

    /// [`Self::open`] on a host that cannot run routines without a desktop
    /// (`unsupported` says why), and with its own wait for a starting
    /// session.
    pub fn open_with(
        port: Arc<dyn ArmPort>,
        clock: Arc<dyn Clock>,
        boot_id: String,
        store: Option<PathBuf>,
        unsupported: Option<String>,
        starting_wait: Duration,
    ) -> Arc<Self> {
        let mut store = store.map(Store::new);
        let (meta, lines): (Option<Meta>, Vec<ArmRun>) = match store.as_mut() {
            Some(store) => store.load(),
            None => (None, Vec::new()),
        };
        let meta = meta.unwrap_or_default();
        let restarted = !meta.boot_id.is_empty() && meta.boot_id != boot_id;
        let mut state = State {
            boot_id,
            next_seq: meta.next_seq,
            dropped_through: meta.dropped_through,
            armed: meta.armed,
            tombs: meta.tombs,
            open: meta.open,
            running: meta.running,
            carried: meta.carried,
            ..State::default()
        };
        for run in lines {
            state.next_seq = state.next_seq.max(run.seq);
            state.log.push_back(run);
        }
        let mut log: Vec<ArmRun> = std::mem::take(&mut state.log).into();
        log.sort_by_key(|run| run.seq);
        log.dedup_by_key(|run| run.seq);
        state.log = log.into();
        while state.log.len() > ARM_RUN_LOG_CAP {
            if let Some(dropped) = state.log.pop_front() {
                state.dropped_through = state.dropped_through.max(dropped.seq);
            }
        }
        let arms = Arc::new(Self {
            port,
            clock,
            store: Mutex::new(store),
            state: Mutex::new(state),
            firing: Mutex::new(()),
            failed: AtomicBool::new(false),
            unsupported,
            starting_wait,
        });
        {
            let mut state = lock(&arms.state);
            if restarted {
                // Sessions do not outlive their host: a fresh run still open
                // ended with it.
                let running = std::mem::take(&mut state.running);
                for open in running {
                    let at_ms = arms.clock.wall_ms();
                    arms.append(&mut state, &open.run, ArmRunStatus::Exited { at_ms });
                }
                let firing: Vec<Open> = state
                    .open
                    .iter()
                    .filter(|open| open.run.status == ArmRunStatus::Firing)
                    .cloned()
                    .collect();
                state
                    .open
                    .retain(|open| open.run.status != ArmRunStatus::Firing);
                for open in firing {
                    arms.append(
                        &mut state,
                        &open.run,
                        ArmRunStatus::Unknown {
                            detail: "the terminal server restarted while this run was firing; it may have reached the agent".into(),
                        },
                    );
                }
            }
            let _ = arms.save(&state);
        }
        arms
    }

    /// Whether the engine still runs.
    pub fn healthy(&self) -> bool {
        !self.failed.load(Ordering::Acquire)
    }

    /// Arm `arm` (or re-arm it after an edit), stamped as coming from the
    /// connection whose viewer is `desktop`.
    pub fn arm(&self, mut arm: RoutineArm, desktop: &str) -> ArmAnswer {
        if let Some(why) = &self.unsupported {
            return ArmAnswer::Refused {
                refusal: invalid(why.clone()),
            };
        }
        if let Err(refusal) = validate(&arm) {
            return ArmAnswer::Refused { refusal };
        }
        arm.originator.desktop = desktop.to_string();
        let wall = self.clock.wall_ms();
        let mut state = lock(&self.state);
        let index = state.armed.iter().position(|a| a.arm.key == arm.key);
        if let Some(refusal) = stale(&state, index, &arm.key, arm.epoch, Some(arm.revision)) {
            return ArmAnswer::Refused { refusal };
        }
        if index.is_none() && state.armed.len() >= MACHINE_ARM_CAP {
            return ArmAnswer::Refused {
                refusal: invalid(format!(
                    "this machine already has {MACHINE_ARM_CAP} armed routines"
                )),
            };
        }
        let before = (
            index.map(|i| state.armed[i].clone()),
            state.tombs.get(&arm.key).copied(),
            state.carried.get(&arm.key).cloned(),
        );
        if let Some(old) = before.0.as_ref().filter(|old| {
            arm.epoch > old.arm.epoch
                && arm.launch.is_none()
                && old.arm.launch.is_some()
                && !arm.source_digest.is_empty()
                && arm.source_digest == old.arm.source_digest
        }) {
            // An unchanged prepared-content receipt may move to a new
            // manager. Ownership, attribution and carried seeds do not
            // change prepared content; the other inputs must still match.
            let mut previous = arm.clone();
            previous.epoch = old.arm.epoch;
            previous.revision = old.arm.revision;
            previous.originator = old.arm.originator.clone();
            previous.author = old.arm.author.clone();
            previous.last_fired_ms = old.arm.last_fired_ms;
            previous.last_run_summary = old.arm.last_run_summary.clone();
            previous.last_run_at_ms = old.arm.last_run_at_ms;
            // A wake budget is local to its managing Mac, not prepared content.
            if let (Some(previous), Some(old)) = (&mut previous.limits, &old.arm.limits) {
                previous.budget = old.budget.clone();
            }
            previous.launch = old.arm.launch.clone();
            if previous == old.arm {
                arm.launch = old.arm.launch.clone();
            }
        }
        // Nothing at or before the last occurrence fired where it was armed
        // before (here, or where the desktop says it fired) fires again.
        let floor = wall.max(
            before
                .2
                .as_ref()
                .and_then(|c| c.last_fired_ms)
                .max(arm.last_fired_ms)
                .unwrap_or(0),
        );
        // An edit that keeps a schedule keeps its cursor: no occurrence is
        // fired twice or lost across the edit.
        let next: Vec<Option<u64>> = arm
            .schedules
            .iter()
            .map(|schedule| {
                let kept = before.0.as_ref().and_then(|old| {
                    old.arm
                        .schedules
                        .iter()
                        .position(|s| s == schedule)
                        .and_then(|i| old.next.get(i).copied().flatten())
                });
                kept.or_else(|| {
                    CronSchedule::parse(&schedule.cron, &schedule.tz)
                        .ok()
                        .and_then(|cron| cron.next_after_ms(floor))
                })
            })
            .collect();
        // The desktop's last run seeds what this machine has none of yet.
        let mut carried = before.2.clone().unwrap_or_default();
        carried.last_fired_ms = carried.last_fired_ms.max(arm.last_fired_ms);
        if carried.last_summary.is_none() {
            carried.last_summary = arm.last_run_summary.clone();
        }
        if carried.last_run_at_ms.is_none() {
            carried.last_run_at_ms = arm.last_run_at_ms;
        }
        let armed = Armed {
            armed_at_ms: before.0.as_ref().map_or(wall, |old| old.armed_at_ms),
            arm,
            next,
        };
        let key = armed.arm.key.clone();
        match index {
            Some(i) => state.armed[i] = armed,
            None => state.armed.push(armed),
        }
        state.tombs.remove(&key);
        state.carried.insert(key.clone(), carried);
        if let Err(error) = self.save(&state) {
            // Not durable, so not armed: undo it whole.
            match before.0 {
                Some(old) => {
                    if let Some(slot) = state.armed.iter_mut().find(|a| a.arm.key == key) {
                        *slot = old;
                    }
                }
                None => state.armed.retain(|a| a.arm.key != key),
            }
            if let Some(epoch) = before.1 {
                state.tombs.insert(key.clone(), epoch);
            }
            match before.2 {
                Some(carried) => state.carried.insert(key, carried),
                None => state.carried.remove(&key),
            };
            return ArmAnswer::Refused {
                refusal: ArmRefusal::Unavailable {
                    detail: error.to_string(),
                },
            };
        }
        let info = state
            .armed
            .iter()
            .find(|a| a.arm.key == key)
            .map(info_of)
            .expect("just armed");
        ArmAnswer::Armed { info }
    }

    /// Stop firing `key`, leaving a tombstone at `epoch`; its queued wakes
    /// are cancelled.
    pub fn disarm(&self, key: &str, epoch: u64) -> ArmAnswer {
        if !valid_label(key) || epoch == 0 {
            return ArmAnswer::Refused {
                refusal: invalid("a disarm needs a routine key and an epoch of at least 1"),
            };
        }
        // A fire in flight finishes first: once a disarm is answered, this
        // host fires nothing more of the routine (the handover's fence).
        let _one = lock(&self.firing);
        let mut state = lock(&self.state);
        let index = state.armed.iter().position(|a| a.arm.key == key);
        // A disarm at the armed epoch is the move back off this machine: the
        // epoch moved forward in git, so it can only equal or exceed it.
        if let Some(i) = index {
            let held = &state.armed[i].arm;
            if held.epoch > epoch {
                return ArmAnswer::Refused {
                    refusal: ArmRefusal::Stale {
                        held_epoch: held.epoch,
                        held_revision: held.revision,
                    },
                };
            }
        }
        if let Some(tomb) = state.tombs.get(key) {
            if *tomb > epoch {
                return ArmAnswer::Refused {
                    refusal: ArmRefusal::Stale {
                        held_epoch: *tomb,
                        held_revision: 0,
                    },
                };
            }
        }
        let removed = index.map(|i| state.armed.remove(i));
        let tomb_before = state.tombs.insert(key.to_string(), epoch);
        if let Err(error) = self.save(&state) {
            if let Some(armed) = removed {
                state.armed.push(armed);
            }
            match tomb_before {
                Some(old) => state.tombs.insert(key.to_string(), old),
                None => state.tombs.remove(key),
            };
            return ArmAnswer::Refused {
                refusal: ArmRefusal::Unavailable {
                    detail: error.to_string(),
                },
            };
        }
        let queued: Vec<(Originator, PtySessionId, String)> = state
            .open
            .iter()
            .filter(|open| open.run.key == key)
            .filter_map(|open| {
                Some((
                    open.run.originator.clone(),
                    open.session.clone()?,
                    open.run.run_id.clone(),
                ))
            })
            .collect();
        drop(state);
        for (originator, session, run_id) in queued {
            self.port.cancel(&originator, &session, &run_id);
        }
        self.settle_open();
        ArmAnswer::Disarmed {
            was_armed: removed.is_some(),
            last_fired_ms: lock(&self.state)
                .carried
                .get(key)
                .and_then(|c| c.last_fired_ms),
        }
    }

    /// The armed routines and the tombstones.
    /// The arms stamped with `viewer` (sent before their desktop
    /// named itself) whose originating install is `host_part` are stamped
    /// with that desktop's `identity`; their keys.
    pub fn restamp(&self, viewer: &str, host_part: &str, identity: &str) -> Vec<String> {
        let mut state = lock(&self.state);
        let mut keys = Vec::new();
        for armed in state.armed.iter_mut() {
            let originator = &mut armed.arm.originator;
            if originator.desktop == viewer
                && crate::offline_mcp::same_install(originator.routine_host.as_deref(), host_part)
            {
                originator.desktop = identity.to_string();
                keys.push(armed.arm.key.clone());
            }
        }
        if !keys.is_empty() {
            if let Err(error) = self.save(&state) {
                log::warn!("routine arms not saved after naming their desktop: {error}");
            }
        }
        keys
    }

    pub fn list(&self) -> ArmList {
        let state = lock(&self.state);
        let last_fired = |key: &str| state.carried.get(key).and_then(|c| c.last_fired_ms);
        ArmList {
            armed: state
                .armed
                .iter()
                .map(|armed| ArmInfo {
                    last_fired_ms: last_fired(&armed.arm.key),
                    ..info_of(armed)
                })
                .collect(),
            disarmed: state
                .tombs
                .iter()
                .map(|(key, epoch)| ArmTombstone {
                    key: key.clone(),
                    epoch: *epoch,
                    last_fired_ms: last_fired(key),
                })
                .collect(),
            unsupported: self.unsupported.clone(),
        }
    }

    /// The run log after `since`.
    pub fn runs(&self, since: u64) -> ArmRunLog {
        let state = lock(&self.state);
        ArmRunLog {
            host_boot_id: state.boot_id.clone(),
            runs: state
                .log
                .iter()
                .filter(|run| run.seq > since)
                .cloned()
                .collect(),
            overflow: since < state.dropped_through,
            last_seq: state.next_seq,
        }
    }

    /// Finish a run this host fired into `session` (the agent's
    /// `routine_done`, or `signal_done` for its latest run, answered here
    /// while no desktop relays its CodeSense MCP, or relayed by one): `run_id`,
    /// or with none the latest run delivered into or started in `session`
    /// that is not finished. Recorded in the run log, so it reaches the
    /// desktop with the run rows. A fresh run it finishes is no longer open:
    /// the next fire starts the agent again, even while this session idles.
    pub fn complete(
        &self,
        session: &str,
        run_id: Option<&str>,
        outcome: &str,
        summary: &str,
    ) -> Result<ArmRun, String> {
        if !codesense_pty_core::routine_arm::RUN_OUTCOMES.contains(&outcome) {
            return Err(format!(
                "outcome {outcome:?} must be success, failed or needs-attention"
            ));
        }
        let summary: String = summary
            .chars()
            .filter(|c| !c.is_control() || *c == '\n')
            .take(4096)
            .collect();
        let mut state = lock(&self.state);
        // Each run's current status: its newest log entry, or the entry an
        // open fresh run started with (the log is capped).
        let mut current: HashMap<&str, &ArmRun> = HashMap::new();
        for running in &state.running {
            current.insert(running.run.run_id.as_str(), &running.run);
        }
        for run in &state.log {
            current.insert(run.run_id.as_str(), run);
        }
        // A run still being started is bound to its session already.
        let starting: Vec<&str> = state
            .open
            .iter()
            .filter(|o| {
                o.run.status == ArmRunStatus::Firing
                    && o.session.as_ref().is_some_and(|s| s.as_str() == session)
            })
            .map(|o| o.run.run_id.as_str())
            .collect();
        let in_session = |run: &ArmRun| match &run.status {
            ArmRunStatus::Delivered { session: s, .. }
            | ArmRunStatus::Launched { session: s, .. } => s.as_str() == session,
            ArmRunStatus::Firing => starting.contains(&run.run_id.as_str()),
            _ => false,
        };
        let finished = |run: &ArmRun| {
            matches!(
                run.status,
                ArmRunStatus::Done { .. }
                    | ArmRunStatus::TimedOut { .. }
                    | ArmRunStatus::Exited { .. }
            )
        };
        let run = match run_id {
            Some(id) => {
                let run = current
                    .get(id)
                    .ok_or_else(|| format!("no routine run {id} on this machine"))?;
                if finished(run) {
                    return Err(format!("routine run {id} is already finished"));
                }
                if !in_session(run) {
                    return Err(format!(
                        "routine run {id} was not started or woken in this session"
                    ));
                }
                (*run).clone()
            }
            None => current
                .values()
                .filter(|run| in_session(run))
                .max_by_key(|run| run.seq)
                .map(|run| (*run).clone())
                .ok_or("no routine run of this machine is open in this session")?,
        };
        let at_ms = self.clock.wall_ms();
        let entry = self.append(
            &mut state,
            &run,
            ArmRunStatus::Done {
                outcome: outcome.to_string(),
                summary: summary.clone(),
                at_ms,
            },
        );
        state.running.retain(|r| r.run.run_id != run.run_id);
        if outcome == "success" {
            state
                .carried
                .entry(run.key.clone())
                .or_default()
                .last_summary = Some(summary);
        }
        let _ = self.save(&state);
        Ok(entry)
    }

    /// [`Self::complete`] as the wire answers it.
    pub fn complete_answer(
        &self,
        session: &str,
        run_id: Option<&str>,
        outcome: &str,
        summary: &str,
    ) -> ArmCompleteAnswer {
        match self.complete(session, run_id, outcome, summary) {
            Ok(run) => ArmCompleteAnswer::Done { run: Box::new(run) },
            Err(detail) => ArmCompleteAnswer::Refused { detail },
        }
    }

    /// Fire `key` now, by hand (`routine_run_now` on a desktop), through its
    /// overlap policy and this host's single flight, as a scheduled fire
    /// would: `task` (already rendered by the desktop) replaces the arm's for
    /// this run. Its cursors do not move.
    pub fn fire_now(&self, key: &str, epoch: u64, task: Option<String>) -> ArmFireAnswer {
        let refused = |refusal| ArmFireAnswer::Refused { refusal };
        if let Some(task) = &task {
            if task.len() > MAX_TASK_BYTES {
                return refused(invalid(format!(
                    "the task is {} bytes; the limit is {MAX_TASK_BYTES}",
                    task.len()
                )));
            }
        }
        let _one = lock(&self.firing);
        let wall = self.clock.wall_ms();
        let claimed = {
            let mut state = lock(&self.state);
            let Some(index) = state.armed.iter().position(|a| a.arm.key == key) else {
                return refused(match state.tombs.get(key) {
                    Some(tomb) => ArmRefusal::Stale {
                        held_epoch: *tomb,
                        held_revision: 0,
                    },
                    None => invalid(format!("{key} is not armed on this machine")),
                });
            };
            let held = &state.armed[index].arm;
            if held.epoch != epoch {
                return refused(ArmRefusal::Stale {
                    held_epoch: held.epoch,
                    held_revision: held.revision,
                });
            }
            let mut armed = state.armed[index].clone();
            if let Some(task) = task {
                armed.arm.task = task;
            }
            // A fire by hand is its own run every time.
            let run_id = uuid::Uuid::new_v4().to_string();
            let run = self.run_of(&state, &armed, run_id, "manual", wall, wall);
            match self.claim(&mut state, &armed, run) {
                Some(claimed) => claimed,
                None => {
                    return refused(ArmRefusal::Unavailable {
                        detail: "the routine store cannot be written".into(),
                    })
                }
            }
        };
        let status = self.act(&claimed);
        self.finish(&claimed, status);
        let state = lock(&self.state);
        let run = state
            .log
            .iter()
            .rev()
            .find(|run| run.run_id == claimed.run.run_id)
            .cloned()
            .unwrap_or(claimed.run);
        ArmFireAnswer::Fired { run: Box::new(run) }
    }

    /// One tick: settle queued wakes and open fresh runs, then fire what is
    /// due. Whether an occurrence was missed is judged against the tick's
    /// start, so a slow fire never makes the next one look missed.
    pub fn tick(&self) {
        let started = self.clock.wall_ms();
        self.settle_open();
        self.settle_running();
        let _one = lock(&self.firing);
        loop {
            let Some(claimed) = self.claim_next(started) else {
                break;
            };
            let status = self.act(&claimed);
            self.finish(&claimed, status);
        }
    }

    /// End each open fresh run whose session ended (`exited`) or that passed
    /// its `max_runtime` (`timed_out`, its session stopped).
    fn settle_running(&self) {
        let wall = self.clock.wall_ms();
        let running: Vec<Running> = lock(&self.state).running.clone();
        if running.is_empty() {
            return;
        }
        let mut ended: Vec<(Running, ArmRunStatus)> = Vec::new();
        for open in running {
            if wall >= open.deadline_ms {
                self.port.kill(&open.session);
                ended.push((open, ArmRunStatus::TimedOut { at_ms: wall }));
            } else if !self.port.session_live(&open.session) {
                ended.push((open, ArmRunStatus::Exited { at_ms: wall }));
            }
        }
        if ended.is_empty() {
            return;
        }
        let mut state = lock(&self.state);
        for (open, status) in ended {
            // The agent may have finished it meanwhile: its verdict stands.
            if !state
                .running
                .iter()
                .any(|r| r.run.run_id == open.run.run_id)
            {
                continue;
            }
            state.running.retain(|r| r.run.run_id != open.run.run_id);
            self.append(&mut state, &open.run, status);
        }
        let _ = self.save(&state);
    }

    /// Move each queued wake to its final status once its delivery has one.
    fn settle_open(&self) {
        let queued: Vec<(String, Originator, PtySessionId)> = lock(&self.state)
            .open
            .iter()
            .filter(|open| matches!(open.run.status, ArmRunStatus::Queued { .. }))
            .filter_map(|open| {
                Some((
                    open.run.run_id.clone(),
                    open.run.originator.clone(),
                    open.session.clone()?,
                ))
            })
            .collect();
        if queued.is_empty() {
            return;
        }
        let answers: Vec<(String, Option<WakeOutcome>)> = queued
            .into_iter()
            .map(|(run, originator, session)| {
                let outcome = self.port.delivery(&originator, &session, &run);
                (run, outcome)
            })
            .collect();
        let mut state = lock(&self.state);
        let mut changed = false;
        for (run_id, outcome) in answers {
            let Some(index) = state.open.iter().position(|o| o.run.run_id == run_id) else {
                continue;
            };
            let session = state.open[index].session.clone();
            let status = match (outcome, session) {
                (Some(outcome), Some(session)) => wake_status(&outcome, &session),
                (None, _) => ArmRunStatus::Unknown {
                    detail: "the terminal server no longer knows this delivery".into(),
                },
                (Some(_), None) => continue,
            };
            if !status.is_final() {
                continue;
            }
            let open = state.open.remove(index);
            self.append(&mut state, &open.run, status);
            changed = true;
        }
        if changed {
            let _ = self.save(&state);
        }
    }

    /// Claim the next due occurrence of any armed routine: move its cursor
    /// and write it `firing`, durably, before anything acts on it. An
    /// occurrence more than [`MISSED_GRACE_MS`] before `started` (the tick's
    /// start) was missed.
    fn claim_next(&self, started: u64) -> Option<Claimed> {
        let wall = self.clock.wall_ms();
        let mut state = lock(&self.state);
        let tombs = state.tombs.clone();
        // A routine whose epoch a disarm overtook is never fired again.
        state.armed.retain(|a| {
            tombs
                .get(&a.arm.key)
                .is_none_or(|tomb| a.arm.epoch >= *tomb)
        });
        let (index, schedule, due) = state.armed.iter().enumerate().find_map(|(i, armed)| {
            armed
                .next
                .iter()
                .enumerate()
                .find_map(|(s, next)| next.filter(|at| *at <= wall).map(|at| (i, s, at)))
        })?;
        let armed = state.armed[index].clone();
        let spec = &armed.arm.schedules[schedule];
        let cron = CronSchedule::parse(&spec.cron, &spec.tz).ok();
        let following = cron.as_ref().and_then(|cron| cron.next_after_ms(wall));
        let missed = started.saturating_sub(due) > MISSED_GRACE_MS;
        let run_id = arm_run_id(&armed.arm.key, armed.arm.epoch, due);
        let previous = state.armed[index].next[schedule];
        state.armed[index].next[schedule] = following;
        let source = if missed { "catch_up" } else { "schedule" };
        let seen = state.open.iter().any(|o| o.run.run_id == run_id)
            || state.log.iter().any(|run| run.run_id == run_id);
        let carried_before = state.carried.get(&armed.arm.key).cloned();
        {
            let carried = state.carried.entry(armed.arm.key.clone()).or_default();
            carried.last_fired_ms = carried.last_fired_ms.max(Some(due));
        }
        let run = self.run_of(&state, &armed, run_id, source, due, wall);
        if seen {
            // Already fired (a cursor restored from an older store): never
            // twice. The cursor moved on; look further.
            let _ = self.save(&state);
            drop(state);
            return self.claim_next(started);
        }
        if missed && armed.arm.missed == ArmMissed::Skip {
            self.append(
                &mut state,
                &run,
                ArmRunStatus::Skipped {
                    why: "missed while the machine was down or asleep (missed: skip)".into(),
                },
            );
            let _ = self.save(&state);
            drop(state);
            return self.claim_next(started);
        }
        match self.claim(&mut state, &armed, run) {
            Some(claimed) => Some(claimed),
            None => {
                // Not claimed: the cursor goes back, so it is tried again.
                state.armed[index].next[schedule] = previous;
                match carried_before {
                    Some(carried) => state.carried.insert(armed.arm.key.clone(), carried),
                    None => state.carried.remove(&armed.arm.key),
                };
                None
            }
        }
    }

    /// The `firing` run of `armed` for the occurrence at `scheduled_at_ms`.
    fn run_of(
        &self,
        state: &State,
        armed: &Armed,
        run_id: String,
        source: &str,
        scheduled_at_ms: u64,
        wall: u64,
    ) -> ArmRun {
        ArmRun {
            seq: 0,
            run_id,
            key: armed.arm.key.clone(),
            epoch: armed.arm.epoch,
            revision: armed.arm.revision,
            mode: armed.arm.mode,
            source: source.into(),
            scheduled_at_ms,
            fired_at_ms: wall,
            status: ArmRunStatus::Firing,
            originator: armed.arm.originator.clone(),
            host_boot_id: state.boot_id.clone(),
            at_ms: wall,
        }
    }

    /// Write `run` `firing`, durably, before anything acts on it: a claim
    /// that cannot be saved leaves no trace and fires nothing (`None`).
    fn claim(&self, state: &mut State, armed: &Armed, run: ArmRun) -> Option<Claimed> {
        let carried = state
            .carried
            .get(&armed.arm.key)
            .cloned()
            .unwrap_or_default();
        state
            .carried
            .entry(armed.arm.key.clone())
            .or_default()
            .last_run_at_ms = Some(run.scheduled_at_ms);
        let firing = self.entry(state, &run, ArmRunStatus::Firing);
        state.open.push(Open {
            run: firing.clone(),
            session: None,
        });
        if let Err(error) = self.save(state) {
            log::warn!("routine {}: fire not claimed: {error}", armed.arm.key);
            state.open.retain(|o| o.run.run_id != firing.run_id);
            state.next_seq -= 1;
            state
                .carried
                .entry(armed.arm.key.clone())
                .or_default()
                .last_run_at_ms = carried.last_run_at_ms;
            return None;
        }
        self.record(state, firing.clone());
        Some(Claimed {
            run: firing,
            arm: armed.arm.clone(),
            carried,
        })
    }

    /// What a claimed fire does, outside the state lock.
    fn act(&self, claimed: &Claimed) -> (ArmRunStatus, Option<PtySessionId>) {
        let arm = &claimed.arm;
        let run = &claimed.run;
        let header = routine_header(&arm.name, &run.run_id);
        let text = render_task(arm, run.scheduled_at_ms, &claimed.carried);
        match arm.mode {
            ArmMode::Wake => {
                let mut candidates = self.port.candidates(&arm.target);
                let flight = (arm.target.agent.clone(), arm.target.workspace_id.clone());
                // Sessions this engine started that the layout may not show
                // yet still count (R1).
                let launched: Vec<PtySessionId> = lock(&self.state)
                    .launched
                    .get(&flight)
                    .cloned()
                    .unwrap_or_default();
                for session in launched {
                    if self.port.session_live(&session)
                        && !candidates.iter().any(|c| c.session == session)
                    {
                        candidates.push(ArmCandidate {
                            session,
                            idle: false,
                            recency: u64::MAX,
                            starting: false,
                        });
                    }
                }
                // Idle first, then up before starting, then the most recent.
                candidates.sort_by(|a, b| {
                    b.idle
                        .cmp(&a.idle)
                        .then(a.starting.cmp(&b.starting))
                        .then(b.recency.cmp(&a.recency))
                });
                match candidates.into_iter().next() {
                    Some(candidate) => self.wake_into(arm, run, candidate, header, text),
                    // Launch if absent: the task is the first prompt. A
                    // launcher that got there first (a desktop's own
                    // launch-if-absent) is woken instead.
                    None => match self.start(arm, run, &header, &text, true) {
                        Ok(started) => started,
                        Err(session) => self.wake_into(
                            arm,
                            run,
                            ArmCandidate {
                                session,
                                idle: false,
                                recency: u64::MAX,
                                starting: true,
                            },
                            header,
                            text,
                        ),
                    },
                }
            }
            ArmMode::Fresh => {
                // Overlap is decided on the routine's open run: started and
                // not finished, timed out or exited. A session that idles
                // after its agent finished the run holds nothing.
                let open = lock(&self.state)
                    .running
                    .iter()
                    .find(|r| r.run.key == arm.key && r.run.run_id != run.run_id)
                    .map(|r| r.run.run_id.clone());
                if let Some(into) = open {
                    return (
                        match arm.overlap {
                            ArmOverlap::Coalesce => ArmRunStatus::Coalesced { into },
                            ArmOverlap::Skip => ArmRunStatus::Skipped {
                                why: format!("run {into} is still running (overlap: skip)"),
                            },
                        },
                        None,
                    );
                }
                self.start(arm, run, &header, &text, false)
                    .unwrap_or_else(|session| {
                        (
                            ArmRunStatus::Undelivered {
                                reason: UndeliveredReason::Unavailable,
                                detail: Some(format!(
                                    "session {session} of the agent is being started"
                                )),
                            },
                            None,
                        )
                    })
            }
        }
    }

    /// Deliver a wake into `candidate`; one still being started is waited
    /// for (up to the engine's starting wait), never launched over.
    fn wake_into(
        &self,
        arm: &RoutineArm,
        run: &ArmRun,
        candidate: ArmCandidate,
        header: String,
        text: String,
    ) -> (ArmRunStatus, Option<PtySessionId>) {
        if candidate.starting && !self.wait_live(&candidate.session) {
            return (
                ArmRunStatus::Undelivered {
                    reason: UndeliveredReason::Unavailable,
                    detail: Some(format!(
                        "{} was still starting (session {}); the next fire wakes it",
                        arm.target.agent, candidate.session
                    )),
                },
                None,
            );
        }
        let request = DeliverRequest {
            session: candidate.session.clone(),
            text,
            header: Some(header),
            marker: marker(arm),
            max_wait_ms: arm.max_wait_ms,
            idempotency_key: run.run_id.clone(),
            originator: arm.originator.clone(),
            limits: arm.limits.clone(),
        };
        match self.port.deliver(request) {
            Ok(outcome) => (
                wake_status(&outcome, &candidate.session),
                Some(candidate.session),
            ),
            Err(error) => (
                ArmRunStatus::Undelivered {
                    reason: UndeliveredReason::Unavailable,
                    detail: Some(error),
                },
                None,
            ),
        }
    }

    /// Wait for `session` (being started) to be up.
    fn wait_live(&self, session: &PtySessionId) -> bool {
        let deadline = std::time::Instant::now() + self.starting_wait;
        loop {
            if self.port.session_live(session) {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(100).min(self.starting_wait));
        }
    }

    /// Start the agent from the arm's launch recipe, only into a prepared
    /// workspace. `Err(session)`: a launch-if-absent found `session` of the
    /// agent running or being started there.
    fn start(
        &self,
        arm: &RoutineArm,
        run: &ArmRun,
        header: &str,
        text: &str,
        if_absent: bool,
    ) -> Result<(ArmRunStatus, Option<PtySessionId>), PtySessionId> {
        let Some(launch) = &arm.launch else {
            return Ok((
                ArmRunStatus::Undelivered {
                    reason: UndeliveredReason::NeedsPreparation,
                    detail: Some(
                        "no desktop has prepared this agent on the machine yet; open CodeSense on the routine's desktop to prepare it".into(),
                    ),
                },
                None,
            ));
        };
        if let Err(detail) = self.port.prepared(&arm.target, launch) {
            return Ok((
                ArmRunStatus::Undelivered {
                    reason: UndeliveredReason::NeedsPreparation,
                    detail: Some(detail),
                },
                None,
            ));
        }
        let prompt = match compose(Some(header), text) {
            Ok(prompt) => prompt,
            Err(detail) => {
                return Ok((
                    ArmRunStatus::Undelivered {
                        reason: UndeliveredReason::Unavailable,
                        detail: Some(detail),
                    },
                    None,
                ))
            }
        };
        let session = PtySessionId::new(format!("routine-{}", run.run_id.replace('-', "")));
        let flight = (arm.target.agent.clone(), arm.target.workspace_id.clone());
        {
            let mut state = lock(&self.state);
            // Counted before it starts: a fire racing this one finds it.
            state
                .launched
                .entry(flight.clone())
                .or_default()
                .push(session.clone());
            // Bound to its run before it starts: an agent that finishes the
            // run at once (`routine_done`) finds it.
            if let Some(open) = state.open.iter_mut().find(|o| o.run.run_id == run.run_id) {
                open.session = Some(session.clone());
            }
        }
        let forget = || {
            if let Some(sessions) = lock(&self.state).launched.get_mut(&flight) {
                sessions.retain(|s| s != &session);
            }
        };
        Ok(
            match self.port.launch(
                &arm.target,
                launch,
                &session,
                &prompt,
                if_absent,
                &arm.originator.desktop,
            ) {
                Ok(()) => (
                    ArmRunStatus::Launched {
                        session: session.clone(),
                        at_ms: self.clock.wall_ms(),
                    },
                    Some(session),
                ),
                Err(LaunchFailure::NotStarted(detail)) => {
                    forget();
                    (
                        ArmRunStatus::Undelivered {
                            reason: UndeliveredReason::AgentFailed,
                            detail: Some(detail),
                        },
                        None,
                    )
                }
                Err(LaunchFailure::Busy(running)) => {
                    forget();
                    if let Some(open) = lock(&self.state)
                        .open
                        .iter_mut()
                        .find(|o| o.run.run_id == run.run_id)
                    {
                        open.session = None;
                    }
                    return Err(running);
                }
                // It may have started: the session stays counted, so nothing
                // launches over it (R1), and the run is never retried (R5).
                Err(LaunchFailure::Unknown(detail)) => {
                    (ArmRunStatus::Unknown { detail }, Some(session))
                }
            },
        )
    }

    /// Record what a fire did.
    fn finish(&self, claimed: &Claimed, (status, session): (ArmRunStatus, Option<PtySessionId>)) {
        let mut state = lock(&self.state);
        let run_id = &claimed.run.run_id;
        state.open.retain(|open| &open.run.run_id != run_id);
        // The agent may already have finished it (it called routine_done the
        // moment it started): its verdict stays the run's last word.
        let finished = state
            .log
            .iter()
            .any(|r| &r.run_id == run_id && matches!(r.status, ArmRunStatus::Done { .. }));
        if !finished {
            let run = self.append(&mut state, &claimed.run, status.clone());
            if let (ArmRunStatus::Launched { session, at_ms }, ArmMode::Fresh) =
                (&status, claimed.arm.mode)
            {
                // A fresh run is open until its agent finishes it, its
                // session ends, or it passes its max_runtime.
                let max_runtime = match claimed.arm.max_runtime_ms {
                    0 => DEFAULT_MAX_RUNTIME_MS,
                    ms => ms,
                };
                state.running.push(Running {
                    run: run.clone(),
                    session: session.clone(),
                    deadline_ms: at_ms.saturating_add(max_runtime),
                });
            }
            if !status.is_final() {
                state.open.push(Open { run, session });
            }
        }
        let _ = self.save(&state);
        // Forget sessions this engine started once they ended.
        let launched = std::mem::take(&mut state.launched);
        drop(state);
        let kept: HashMap<(String, String), Vec<PtySessionId>> = launched
            .into_iter()
            .map(|(flight, sessions)| {
                let live = sessions
                    .into_iter()
                    .filter(|session| self.port.session_live(session))
                    .collect::<Vec<_>>();
                (flight, live)
            })
            .filter(|(_, sessions)| !sessions.is_empty())
            .collect();
        let mut state = lock(&self.state);
        for (flight, sessions) in kept {
            state.launched.entry(flight).or_default().extend(sessions);
        }
    }

    /// Append `run` at `status` to the log; the entry as written.
    fn append(&self, state: &mut State, run: &ArmRun, status: ArmRunStatus) -> ArmRun {
        let entry = self.entry(state, run, status);
        self.record(state, entry.clone());
        entry
    }

    /// The next log entry for `run` at `status` (its position taken).
    fn entry(&self, state: &mut State, run: &ArmRun, status: ArmRunStatus) -> ArmRun {
        state.next_seq += 1;
        ArmRun {
            seq: state.next_seq,
            status,
            host_boot_id: state.boot_id.clone(),
            at_ms: self.clock.wall_ms(),
            ..run.clone()
        }
    }

    /// Write `entry` to the log, on disk and in memory.
    fn record(&self, state: &mut State, entry: ArmRun) {
        {
            let mut store = lock(&self.store);
            if let Some(store) = store.as_mut() {
                if let Err(error) = store.append(&entry) {
                    log::warn!("routine run not stored: {error}");
                }
                if store.lines > 4 * ARM_RUN_LOG_CAP {
                    let kept: Vec<ArmRun> = state
                        .log
                        .iter()
                        .cloned()
                        .chain(std::iter::once(entry.clone()))
                        .collect();
                    let skip = kept.len().saturating_sub(ARM_RUN_LOG_CAP);
                    if let Err(error) = store.compact(&kept[skip..]) {
                        log::warn!("routine runs not compacted: {error}");
                    }
                }
            }
        }
        state.log.push_back(entry);
        while state.log.len() > ARM_RUN_LOG_CAP {
            if let Some(dropped) = state.log.pop_front() {
                state.dropped_through = state.dropped_through.max(dropped.seq);
            }
        }
    }

    fn save(&self, state: &State) -> std::io::Result<()> {
        let store = lock(&self.store);
        let Some(store) = store.as_ref() else {
            return Ok(());
        };
        let meta = Meta {
            boot_id: state.boot_id.clone(),
            next_seq: state.next_seq,
            dropped_through: state.dropped_through,
            armed: state.armed.clone(),
            tombs: state.tombs.clone(),
            open: state.open.clone(),
            running: state.running.clone(),
            carried: state.carried.clone(),
        };
        let result = store.save_meta(&meta);
        if let Err(error) = &result {
            log::warn!("routine arm store not written: {error}");
        }
        result
    }

    #[cfg(test)]
    fn fail_store(&self, fail: bool) {
        if let Some(store) = lock(&self.store).as_mut() {
            store.fail_writes = fail;
        }
    }
}

/// Whether an arm at `epoch` (and `revision`) is older than what the host
/// holds for `key`.
fn stale(
    state: &State,
    index: Option<usize>,
    key: &str,
    epoch: u64,
    revision: Option<u32>,
) -> Option<ArmRefusal> {
    if let Some(i) = index {
        let held = &state.armed[i].arm;
        let older = epoch < held.epoch
            || (epoch == held.epoch && revision.is_some_and(|r| r < held.revision));
        if older {
            return Some(ArmRefusal::Stale {
                held_epoch: held.epoch,
                held_revision: held.revision,
            });
        }
    }
    // A disarm at epoch N refuses every older arm. Every move of a routine
    // between a desktop and a machine takes a new epoch, so an arm at N
    // itself can only come from the same placement: the routine paused or
    // archived and back (a disarm without a move), never a stale definition.
    state
        .tombs
        .get(key)
        .filter(|tomb| epoch < **tomb)
        .map(|tomb| ArmRefusal::Stale {
            held_epoch: *tomb,
            held_revision: 0,
        })
}

fn validate(arm: &RoutineArm) -> Result<(), ArmRefusal> {
    if !valid_label(&arm.key) {
        return Err(invalid("routine key must be 1-256 bytes without controls"));
    }
    if arm.epoch == 0 {
        return Err(invalid("an arm needs an epoch of at least 1"));
    }
    if !valid_label(&arm.name) {
        return Err(invalid("routine name must be 1-256 bytes without controls"));
    }
    if arm.schedules.is_empty() || arm.schedules.len() > ARM_SCHEDULE_CAP {
        return Err(invalid(format!(
            "a routine armed on a machine needs 1-{ARM_SCHEDULE_CAP} schedules"
        )));
    }
    for spec in &arm.schedules {
        let cron = CronSchedule::parse(&spec.cron, &spec.tz).map_err(invalid)?;
        let first = cron
            .next_after_ms(0)
            .ok_or_else(|| invalid(format!("cron {:?} never fires", spec.cron)))?;
        if cron
            .next_after_ms(first)
            .is_some_and(|second| second - first < MIN_ARM_INTERVAL_MS)
        {
            return Err(invalid("a routine fires at most once a minute"));
        }
    }
    if arm.task.len() > MAX_TASK_BYTES {
        return Err(invalid(format!(
            "the task is {} bytes; the limit is {MAX_TASK_BYTES}",
            arm.task.len()
        )));
    }
    compose(
        Some(&routine_header(&arm.name, &arm_run_id("", 0, 0))),
        &arm.task,
    )
    .map_err(invalid)?;
    if arm.max_wait_ms == 0 || arm.max_wait_ms > MAX_WAIT_LIMIT_MS {
        return Err(invalid(format!(
            "max wait must be between 1 ms and {MAX_WAIT_LIMIT_MS} ms"
        )));
    }
    if !valid_label(&arm.target.agent) || !valid_label(&arm.target.workspace_id) {
        return Err(invalid("the target needs an agent and a workspace"));
    }
    if arm.max_runtime_ms != 0
        && !(MIN_ARM_INTERVAL_MS..=DEFAULT_MAX_RUNTIME_MS).contains(&arm.max_runtime_ms)
    {
        return Err(invalid("max runtime must be between a minute and a day"));
    }
    if arm
        .last_run_summary
        .as_ref()
        .is_some_and(|summary| summary.len() > MAX_TASK_BYTES)
    {
        return Err(invalid("the last run summary is too long"));
    }
    if !arm.target.cwd.starts_with('/') {
        return Err(invalid("the workspace directory must be absolute"));
    }
    if let Some(launch) = &arm.launch {
        codesense_pty_core::agent_cli::validate_startup(&launch.agent_startup, launch.launch)
            .map_err(invalid)?;
        if launch.prepared_paths.iter().any(|path| {
            path.starts_with('/') || path.split('/').any(|part| part == ".." || part.is_empty())
        }) {
            return Err(invalid("prepared paths must stay inside the workspace"));
        }
    }
    Ok(())
}

fn info_of(armed: &Armed) -> ArmInfo {
    ArmInfo {
        key: armed.arm.key.clone(),
        epoch: armed.arm.epoch,
        revision: armed.arm.revision,
        mode: armed.arm.mode,
        next_at_ms: armed.next.iter().flatten().min().copied(),
        armed_at_ms: armed.armed_at_ms,
        has_launch: armed.arm.launch.is_some(),
        prepared_digest: armed
            .arm
            .launch
            .as_ref()
            .map(|launch| launch.prepared_digest.clone())
            .unwrap_or_default(),
        workspace_id: armed.arm.target.workspace_id.clone(),
        originator: armed.arm.originator.clone(),
        source_digest: armed.arm.source_digest.clone(),
        last_fired_ms: None,
    }
}

/// The run status a delivery outcome stands for.
fn wake_status(outcome: &WakeOutcome, session: &PtySessionId) -> ArmRunStatus {
    match &outcome.status {
        DeliveryStatus::Queued { deadline_ms } => ArmRunStatus::Queued {
            session: session.clone(),
            deadline_ms: *deadline_ms,
        },
        DeliveryStatus::Delivered { at_ms } => ArmRunStatus::Delivered {
            session: session.clone(),
            at_ms: *at_ms,
        },
        DeliveryStatus::Coalesced { into } => ArmRunStatus::Coalesced { into: into.clone() },
        // Inside the routine's `min_gap` with `overlap: skip`.
        DeliveryStatus::Undelivered {
            reason: UndeliveredReason::MinGap,
        } => ArmRunStatus::Skipped {
            why: "inside the routine's min_gap since its last wake (overlap: skip)".into(),
        },
        DeliveryStatus::Undelivered { reason } => ArmRunStatus::Undelivered {
            reason: *reason,
            detail: outcome.notice.clone(),
        },
        DeliveryStatus::Unknown { detail } => ArmRunStatus::Unknown {
            detail: detail.clone(),
        },
    }
}

/// The delivery marker: the routine and who last edited it.
fn marker(arm: &RoutineArm) -> String {
    // The same shape a desktop-fired wake's marker has.
    let marker = if arm.author.trim().is_empty() {
        arm.name.clone()
    } else {
        format!("{} (last edited by {})", arm.name, arm.author.trim())
    };
    marker
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect()
}

/// The task with `{{date}}` (in the first schedule's zone), `{{timestamp}}`
/// of the occurrence, and `{{last_run_summary}}` / `{{last_run_at}}` from
/// what the routine carries filled in. The desktop rendered its declared
/// variables when it pushed the arm.
fn render_task(arm: &RoutineArm, at_ms: u64, carried: &Carried) -> String {
    let rfc3339 = |ms: u64| {
        i64::try_from(ms)
            .ok()
            .and_then(|ms| Utc.timestamp_millis_opt(ms).single())
            .map(|at| at.to_rfc3339_opts(SecondsFormat::Secs, true))
    };
    let instant = i64::try_from(at_ms)
        .ok()
        .and_then(|ms| Utc.timestamp_millis_opt(ms).single())
        .unwrap_or_else(Utc::now);
    let date = arm
        .schedules
        .first()
        .and_then(|s| s.tz.trim().parse::<chrono_tz::Tz>().ok())
        .map(|tz| instant.with_timezone(&tz).format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| instant.format("%Y-%m-%d").to_string());
    let values = BTreeMap::from([
        ("date", date),
        (
            "timestamp",
            instant.to_rfc3339_opts(SecondsFormat::Secs, true),
        ),
        (
            "last_run_summary",
            carried.last_summary.clone().unwrap_or_default(),
        ),
        (
            "last_run_at",
            carried.last_run_at_ms.and_then(rfc3339).unwrap_or_default(),
        ),
    ]);
    render_static(&arm.task, &values)
}

/// Run `arms`' tick every [`TICK`] for as long as it lives. A tick that
/// panics is logged; three in a row stop the engine.
pub(crate) fn start_ticker(arms: Weak<RoutineArms>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("routine-arm".into())
        .spawn(move || {
            let mut failures = 0u32;
            loop {
                std::thread::sleep(TICK);
                let Some(arms) = arms.upgrade() else { return };
                if !arms.healthy() {
                    return;
                }
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| arms.tick())) {
                    Ok(()) => failures = 0,
                    Err(_) => {
                        failures += 1;
                        log::error!("routine arm tick failed ({failures} in a row)");
                        if failures >= 3 {
                            arms.failed.store(true, Ordering::Release);
                            return;
                        }
                    }
                }
            }
        })
        .map(|_| ())
}
