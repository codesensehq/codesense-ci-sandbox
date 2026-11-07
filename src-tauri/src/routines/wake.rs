// SPDX-License-Identifier: MIT
//! SW-AGR-2: a `wake` routine fire. Instead of starting another instance of
//! its agent, the fire finds the instance that is already running and hands
//! its task to the delivery core (SW-AGR-9), which types it in when that
//! session is idle. Only when no instance is live anywhere in the routine's
//! target does it start one, with the task as its first prompt (SPEC §4.1,
//! §9.1, §9.8).
//!
//! - **Who can be woken** (§9.1). Sessions whose saved agent is the routine's
//!   agent, still running in their tab (a tab whose agent quit back to its
//!   shell is not the agent), never a built-in starter's (a starter routine
//!   is fresh only, so a tab someone opened by hand is never typed into),
//!   never a headless run.
//! - **Where** (§4): `target.machine` is `local` (the default), a machine id,
//!   or `any`. Candidates come from every machine's terminal layout, so a
//!   remote session counts whoever launched it. A session on a disconnected
//!   machine that was last seen with its agent running still counts as live
//!   (R1): the wake is `undelivered(machine_offline)` and nothing launches
//!   (never duplicate). A machine this desktop does not know is
//!   `undelivered(machine_unknown)`.
//! - **Which** session: one `trigger.sessions` names first (an `inbox_idle`
//!   wake), then idle over busy, then the most recently active. A local
//!   session must carry the matching routine's launch tag; a hand-started
//!   session or another routine's session is never adopted.
//! - **No duplicates** (R1). Resolution waits for the launch registry's
//!   restore pass after an app restart, and a launch goes through one
//!   single-flight per local routine, or per agent and remote machine.
//!   A pending local launch waits for its tagged roster entry. Remote
//!   launches retain their recent-launch fallback.
//! - **Never parks the claim loop.** A fire that has to wait (the restore
//!   pass, a launch in flight, a session still starting) is deferred: it is
//!   handed back to be dispatched again shortly ([`WakeNotify::redispatch`]),
//!   for up to [`DEFER_CAP_MS`], then ends `undelivered(unobservable)`.
//! - **Honest results** (R5, R7): what the delivery core answers is the run's
//!   status. `unknown` is never sent again, and posts once per run to the
//!   human inbox. A wake that finds its agent quit (`session_ended` before it
//!   was typed) is resolved again, so it launches the agent if none runs.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use codesense_pty_core::session_wake::UndeliveredReason;
use rusqlite::{params, OptionalExtension};

use crate::coordinator::StoreResult;
use crate::delivery::authority::{SessionPosture, SessionRef};
use crate::delivery::events::WakeOrigin;
use crate::delivery::run_status::WakeRunStatus;
use crate::delivery::{DeliveryError, DeliveryReceipt, WakeDelivery};
use crate::domain::Principal;

use super::admit::RoutinePointer;
use super::finish::{self, Ending};
use super::runs::phase;
use super::state::RoutineStateStore;

/// This Mac, as a wake target names it.
pub(crate) const LOCAL: &str = crate::commands::machines::LOCAL_MACHINE_ID;
/// Any connected machine (`target.machine: any`).
pub(crate) const ANY: &str = "any";

/// How long a wake waits for its session to be idle when the routine says
/// nothing (`policy.max_wait`).
pub(crate) const DEFAULT_MAX_WAIT_MS: u64 = 30 * 60 * 1000;
/// How long a session a wake (or a person's Start) just launched counts as
/// live and busy before the session list shows it.
pub(crate) const LAUNCH_GRACE_MS: i64 = 2 * 60 * 1000;
/// A launch in flight longer than this never settled: it no longer blocks.
pub(crate) const IN_FLIGHT_STALE_MS: i64 = 2 * 60 * 1000;
/// How long after it fired a wake may keep being deferred (the restore
/// pass, a launch in flight) before it ends `undelivered(unobservable)`.
pub(crate) const DEFER_CAP_MS: i64 = 10 * 60 * 1000;
/// How soon a deferred wake is dispatched again.
pub(crate) const DEFER_STEP: Duration = Duration::from_secs(2);
/// How many times a wake whose agent quit is resolved again.
pub(crate) const MAX_REOPENS: i64 = 2;
/// `routine_runs.wake_machine` of a wake that is deferred: it is the
/// routine's waiting wake (later fires fold into it) and owns no session.
pub(crate) const DEFERRED: &str = "deferred";

/// Why a wake run was refused by the delivery core's authority (a session
/// that skips permission checks without `allowWake`, the gate): its
/// `undelivered` reason.
pub(crate) const REASON_WAKE_REFUSED: &str = "wake_refused";

// ───────────────────────────── candidates ─────────────────────────────

/// What a session is doing, as wake resolution reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Activity {
    /// At its prompt.
    Idle,
    /// Mid-turn.
    Busy,
    /// A permission prompt waits: never typed into, treated as busy (§9.3).
    NeedsYou,
    /// Its owner has no current observation; the session server judges.
    Unknown,
    /// Its machine is not connected; its agent was running when last seen.
    Offline,
    /// A launch reserved but not bound to a session yet.
    Starting,
    /// Its probe found no agent CLI, for less than [`AGENT_QUIT_GRACE_MS`]:
    /// a re-exec or `/exit` + `claude --resume` looks like this for a moment.
    Quitting,
    /// It ended, or its agent quit back to the shell.
    Ended,
}

/// One session that might be woken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    /// `local` for this Mac, else the machine id.
    pub machine: String,
    /// Empty while [`Activity::Starting`].
    pub session: String,
    pub handle: String,
    /// The saved agent it runs, by key (no revision).
    pub agent: Option<String>,
    /// The routine that launched this local session.
    pub routine_key: Option<String>,
    pub activity: Activity,
    /// A headless run (`claude -p`): never a wake candidate (§9.1).
    pub headless: bool,
    /// When it was last active (started, or last output), epoch ms.
    pub last_active_ms: i64,
    /// The remote launch request its tab was opened under (`remoteRequestId`),
    /// when it has one: a held unconfirmed launch it answers is settled.
    pub request_id: Option<String>,
}

/// `target.machine`, typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TargetMachine {
    Local,
    Machine(String),
    Any,
}

impl TargetMachine {
    /// Blank or `local` is this Mac.
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::trim).unwrap_or_default() {
            "" | LOCAL => Self::Local,
            ANY => Self::Any,
            id => Self::Machine(id.to_string()),
        }
    }

    /// A named machine other than this Mac.
    pub fn is_remote(&self) -> bool {
        matches!(self, Self::Machine(_))
    }

    fn includes(&self, machine: &str) -> bool {
        match self {
            Self::Local => machine == LOCAL,
            Self::Machine(id) => machine == id,
            Self::Any => true,
        }
    }

    /// Where an absent agent is launched: `any` launches on the routine's
    /// armed host, this Mac (§9.8).
    pub fn launch_machine(&self) -> &str {
        match self {
            Self::Machine(id) => id,
            Self::Local | Self::Any => LOCAL,
        }
    }
}

/// What a wake looks for.
#[derive(Debug, Clone)]
pub(crate) struct WakeQuery<'a> {
    /// The routine's agent key.
    pub agent: &'a str,
    pub routine_key: &'a str,
    pub target: &'a TargetMachine,
    /// Handles to prefer (an `inbox_idle` fire's `trigger.sessions`).
    pub preferred: &'a [String],
}

/// What resolution found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pick {
    /// A live session: hand it the wake.
    Deliver(Candidate),
    /// Only a session on a machine that is not connected, last seen with its
    /// agent running: `undelivered(machine_offline)`, nothing launches (R1).
    Offline(Candidate),
    /// Only a launch of the agent that is not bound yet: wait for it.
    Starting,
    /// Only a session whose agent left its prompt a moment ago: look again
    /// once [`AGENT_QUIT_GRACE_MS`] has passed.
    Quitting,
    /// Nothing live in the target: launch one.
    Absent,
}

/// Whether `c` is an instance of the query's agent within its target.
fn in_target(c: &Candidate, q: &WakeQuery<'_>) -> bool {
    if c.headless || c.agent.as_deref() != Some(q.agent) || !q.target.includes(&c.machine) {
        return false;
    }
    // A remote session's dir is that machine's: only its machine is compared.
    if c.machine != LOCAL {
        return true;
    }
    c.routine_key.as_deref() == Some(q.routine_key)
}

/// Resolve the session a wake goes to (§4.1 step 1, §9.8).
pub(crate) fn pick(candidates: &[Candidate], q: &WakeQuery<'_>) -> Pick {
    let mine: Vec<&Candidate> = candidates.iter().filter(|c| in_target(c, q)).collect();
    let rank = |c: &Candidate| {
        let preferred = q.preferred.contains(&c.handle);
        let idle = c.activity == Activity::Idle;
        // Preferred first, then idle, then the most recent.
        (!preferred, !idle, std::cmp::Reverse(c.last_active_ms))
    };
    let of = |wanted: &[Activity]| {
        let mut found: Vec<&Candidate> = mine
            .iter()
            .copied()
            .filter(|c| wanted.contains(&c.activity))
            .collect();
        found.sort_by_key(|c| rank(c));
        found.first().map(|c| (*c).clone())
    };
    if let Some(live) = of(&[
        Activity::Idle,
        Activity::Busy,
        Activity::NeedsYou,
        Activity::Unknown,
    ]) {
        return Pick::Deliver(live);
    }
    if of(&[Activity::Starting]).is_some() {
        return Pick::Starting;
    }
    if of(&[Activity::Quitting]).is_some() {
        return Pick::Quitting;
    }
    match of(&[Activity::Offline]) {
        Some(offline) => Pick::Offline(offline),
        None => Pick::Absent,
    }
}

// ───────────────────────────── single flight ─────────────────────────────

/// One launch's identity: `(agent, scope, machine)`. A local wake's scope
/// is its routine key; an attended Start uses its canonical directory.
pub(crate) type FlightKey = (String, String, String);

/// The flight key of `agent` launched in `dir` on `machine`.
pub(crate) fn flight_key(agent: &str, dir: &Path, machine: &str) -> FlightKey {
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    (
        agent.to_string(),
        dir.to_string_lossy().into_owned(),
        machine.to_string(),
    )
}

#[derive(Debug, Clone)]
enum Flight {
    /// Claimed by a fire or a Start, not settled yet.
    Claimed { since: i64 },
    Launched {
        session: String,
        handle: String,
        at: i64,
    },
    /// A remote launch whose Create was never confirmed either way (SW-AGR-2b):
    /// the tab may exist. Held (since `since`) until an Attach on the same
    /// request id settles it, its tab shows in the machine's layout, or
    /// [`HOLD_TTL_MS`] passes; nothing ever Creates over it meanwhile.
    Unconfirmed {
        request_id: uuid::Uuid,
        since: i64,
        /// A person was already asked about it (it outlived the TTL while
        /// its launch was still running on the machine).
        asked: bool,
    },
}

/// How long an unconfirmed remote launch is held before a person is asked
/// (`needs_you`): as long as the launch's settlement stays answerable.
pub(crate) const HOLD_TTL_MS: i64 =
    crate::commands::pty_layout::LAUNCH_SETTLEMENT_TTL.as_millis() as i64;

/// Launches in flight or just made, keyed by their scope. Local wakes use
/// their routine key; attended Starts use their directory; remote wakes
/// coordinate per agent and machine.
#[derive(Debug, Default)]
pub(crate) struct LaunchFlights {
    flights: Mutex<HashMap<FlightKey, Flight>>,
}

/// What [`LaunchFlights::begin`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Begin {
    /// This fire launches; it must [`LaunchFlights::finish`].
    Go,
    /// Another launch is in flight.
    Wait,
    /// An earlier remote launch was never confirmed: this fire may only
    /// Attach on its request id, never Create (SW-AGR-2b). It must
    /// [`LaunchFlights::finish`] or [`LaunchFlights::hold`] again.
    Reattach(uuid::Uuid),
    /// That launch stayed unconfirmed for [`HOLD_TTL_MS`]: the hold is
    /// dropped and a person is asked; the next fire may launch.
    /// It may only Attach; still `pending` there, the hold is kept.
    HoldExpired(Expired),
}

/// A hold that outlived [`HOLD_TTL_MS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Expired {
    pub request_id: uuid::Uuid,
    pub since: i64,
    /// A person was already asked about this hold.
    pub asked: bool,
}

impl LaunchFlights {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<FlightKey, Flight>> {
        self.flights.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A wake that found nothing live claims the launch. A launch in flight,
    /// or one made within [`LAUNCH_GRACE_MS`] (it may have settled after this
    /// fire read its candidates), is waited for: never launched over.
    pub fn begin(&self, key: &FlightKey, now: i64) -> Begin {
        let mut flights = self.lock();
        match flights.get(key) {
            Some(Flight::Claimed { since }) if now - since < IN_FLIGHT_STALE_MS => Begin::Wait,
            Some(Flight::Launched { at, .. }) if now - at < LAUNCH_GRACE_MS => Begin::Wait,
            Some(Flight::Unconfirmed {
                request_id,
                since,
                asked,
            }) if now - since >= HOLD_TTL_MS => {
                let expired = Expired {
                    request_id: *request_id,
                    since: *since,
                    asked: *asked,
                };
                flights.remove(key);
                Begin::HoldExpired(expired)
            }
            Some(Flight::Unconfirmed { request_id, .. }) => Begin::Reattach(*request_id),
            _ => {
                flights.insert(key.clone(), Flight::Claimed { since: now });
                Begin::Go
            }
        }
    }

    /// A remote launch of `key` came back unconfirmed: hold it under its
    /// request id, so every later launch of `key` only Attaches on it. Held
    /// again under the same id, it keeps the time it was first held.
    pub fn hold(&self, key: &FlightKey, request_id: uuid::Uuid, now: i64) {
        let mut flights = self.lock();
        let (since, asked) = match flights.get(key) {
            Some(Flight::Unconfirmed {
                request_id: held,
                since,
                asked,
            }) if *held == request_id => (*since, *asked),
            _ => (now, false),
        };
        flights.insert(
            key.clone(),
            Flight::Unconfirmed {
                request_id,
                since,
                asked,
            },
        );
    }

    /// Keep an expired hold whose launch is still running on its machine:
    /// held as it was, its person already asked.
    pub fn keep_expired(&self, key: &FlightKey, expired: Expired) {
        self.lock().insert(
            key.clone(),
            Flight::Unconfirmed {
                request_id: expired.request_id,
                since: expired.since,
                asked: true,
            },
        );
    }

    /// A hold the store recorded before an app restart (`since` the launch
    /// was declared unknown), when `key` has no flight now.
    pub fn restore_hold(&self, key: &FlightKey, request_id: uuid::Uuid, since: i64) {
        self.lock()
            .entry(key.clone())
            .or_insert(Flight::Unconfirmed {
                request_id,
                since,
                asked: false,
            });
    }

    /// The request id `key` is held under, if it is.
    pub fn held(&self, key: &FlightKey) -> Option<uuid::Uuid> {
        match self.lock().get(key) {
            Some(Flight::Unconfirmed { request_id, .. }) => Some(*request_id),
            _ => None,
        }
    }

    /// Tabs opened under these request ids are on their machine's layout:
    /// every launch held under one of them is settled (the tab is there, and
    /// the roster finds it). The settled ids.
    pub fn release_seen(&self, seen: &[String]) -> Vec<uuid::Uuid> {
        let mut released = Vec::new();
        self.lock().retain(|_, flight| match flight {
            Flight::Unconfirmed { request_id, .. }
                if seen.iter().any(|id| id == &request_id.to_string()) =>
            {
                released.push(*request_id);
                false
            }
            _ => true,
        });
        released
    }

    /// A person's Start of the agent: `Err` with the session to show when a
    /// launch is in flight (`None` while unbound) or was just made; else it
    /// claims the launch, and must [`Self::finish`].
    pub fn claim_start(&self, key: &FlightKey, now: i64) -> Result<(), Option<String>> {
        let mut flights = self.lock();
        match flights.get(key) {
            Some(Flight::Claimed { since }) if now - since < IN_FLIGHT_STALE_MS => Err(None),
            Some(Flight::Launched { session, at, .. }) if now - at < LAUNCH_GRACE_MS => {
                Err(Some(session.clone()))
            }
            Some(Flight::Unconfirmed { .. }) => Err(None),
            _ => {
                flights.insert(key.clone(), Flight::Claimed { since: now });
                Ok(())
            }
        }
    }

    /// The launch of `key` settled: what it started, or nothing.
    pub fn finish(&self, key: &FlightKey, launched: Option<(String, String)>, now: i64) {
        let mut flights = self.lock();
        match launched {
            Some((session, handle)) => {
                flights.insert(
                    key.clone(),
                    Flight::Launched {
                        session,
                        handle,
                        at: now,
                    },
                );
            }
            None => {
                flights.remove(key);
            }
        }
    }

    /// Sessions launched within [`LAUNCH_GRACE_MS`], as busy candidates: live
    /// before the session list shows them.
    pub fn launched(&self, now: i64) -> Vec<Candidate> {
        let mut flights = self.lock();
        flights.retain(|_, flight| match flight {
            Flight::Launched { at, .. } => now - *at < LAUNCH_GRACE_MS,
            Flight::Claimed { since } => now - *since < IN_FLIGHT_STALE_MS,
            Flight::Unconfirmed { .. } => true,
        });
        flights
            .iter()
            .filter_map(|((agent, _, machine), flight)| match flight {
                Flight::Launched {
                    session,
                    handle,
                    at,
                } => Some(Candidate {
                    machine: machine.clone(),
                    session: session.clone(),
                    handle: handle.clone(),
                    agent: Some(agent.clone()),
                    routine_key: None,
                    activity: Activity::Busy,
                    headless: false,
                    last_active_ms: *at,
                    request_id: None,
                }),
                Flight::Claimed { .. } | Flight::Unconfirmed { .. } => None,
            })
            .collect()
    }
}

// ───────────────────────────── a quit agent ─────────────────────────────

/// How long a tab must show no agent CLI before a wake reads its agent as
/// quit: the session server's own grace (§9.3), so a Claude re-exec or an
/// `/exit` + `claude --resume` never reads as absent on either side.
pub(crate) const AGENT_QUIT_GRACE_MS: i64 =
    codesense_session_server::session_wake::AGENT_QUIT_GRACE_MS as i64;

/// What the quit check says of a live tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuitState {
    /// Its agent runs, or its probe could not say.
    Present,
    /// No agent CLI, for less than [`AGENT_QUIT_GRACE_MS`].
    Quitting,
    /// No agent CLI for [`AGENT_QUIT_GRACE_MS`] or more: it quit.
    Quit,
}

/// Since when each tab's probe has found no agent CLI, by `(machine,
/// session)`: the desktop half of the quit grace.
#[derive(Debug, Default)]
pub(crate) struct QuitWatch {
    /// `(first seen absent, last seen absent)`.
    since: Mutex<HashMap<(String, String), (i64, i64)>>,
}

impl QuitWatch {
    /// `absent`: the tab's probe worked and found no agent CLI at `now`. A
    /// streak whose last sample is older than twice the grace (no wake asked
    /// for a while) starts over: an old absence says nothing about now.
    pub fn observe(&self, machine: &str, session: &str, absent: bool, now: i64) -> QuitState {
        let mut since = self.since.lock().unwrap_or_else(|e| e.into_inner());
        let key = (machine.to_string(), session.to_string());
        if !absent {
            since.remove(&key);
            return QuitState::Present;
        }
        let entry = since.entry(key).or_insert((now, now));
        if now - entry.1 > 2 * AGENT_QUIT_GRACE_MS {
            entry.0 = now;
        }
        entry.1 = now;
        if now - entry.0 >= AGENT_QUIT_GRACE_MS {
            QuitState::Quit
        } else {
            QuitState::Quitting
        }
    }

    /// Forget every tab not in `live`: it left the layout.
    pub fn keep_only(&self, live: &std::collections::HashSet<(String, String)>) {
        self.since
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|key, _| live.contains(key));
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.since.lock().unwrap().len()
    }
}

// ───────────────────────────── remote launch (SW-AGR-2b) ─────────────────────────────

/// The namespace a routine run's remote launch request id is made in.
const REMOTE_LAUNCH_NAMESPACE: uuid::Uuid =
    uuid::Uuid::from_u128(0x6f1d_2c6e_7b0a_4c58_9a3e_51c4_ad27_e90b);

/// RFC 4122 name-based (SHA-1) UUID: version 5.
pub(crate) fn uuid_v5(namespace: &uuid::Uuid, name: &[u8]) -> uuid::Uuid {
    use sha1::{Digest, Sha1};
    let mut hasher = Sha1::new();
    hasher.update(namespace.as_bytes());
    hasher.update(name);
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    uuid::Builder::from_sha1_bytes(bytes).into_uuid()
}

/// The request id a run's remote launch is created with, the same on every
/// dispatch of that run: `uuid_v5(run_id)`. A repeated Create of it is the
/// same launch, never a second one.
pub(crate) fn launch_request_id(run_id: &str) -> uuid::Uuid {
    uuid_v5(&REMOTE_LAUNCH_NAMESPACE, run_id.as_bytes())
}

/// One launch of a routine run's agent on a connected machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteLaunch {
    pub run_id: String,
    pub routine_key: String,
    pub routine_name: String,
    /// The routine's team (a personal routine's org has none: its scratch).
    pub org: String,
    pub team: String,
    /// The agent, by key.
    pub agent: String,
    pub machine: String,
    /// Where on the machine: resolved there, never as a path on this Mac.
    pub target: super::model::RoutineTarget,
    /// What the agent starts with.
    pub prompt: String,
    pub principal: Principal,
    pub request_id: uuid::Uuid,
}

/// What a remote launch asks of the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TabOp {
    /// Prepare the workspace (SW-XS-7) and start the agent, once per request id.
    Create,
    /// Find what a Create of the same request id did. Never starts anything
    /// a Create did not ask for.
    Attach,
}

/// What the machine answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TabReply {
    /// The agent's tab: its session and handle.
    Opened { session: String, handle: String },
    /// That request's tab was opened, and has since ended: its agent is not
    /// running there any more.
    Ended { session: String, handle: String },
    /// Not confirmed either way: the agent may have started.
    Unknown(String),
    /// The launch is still running on the machine (`pending`): not
    /// confirmed either way yet.
    Pending(String),
    /// Nothing was started, and why (`needs_preparation` when the machine
    /// has no prepared workspace for the target).
    NotStarted {
        reason: UndeliveredReason,
        detail: String,
    },
    /// SW-AGR-8: nothing was started because the agent already runs there,
    /// or is being started (the machine's single flight, which its own
    /// routines share): wake that session instead.
    Running(String),
}

pub(crate) type TabFuture<'a> = Pin<Box<dyn Future<Output = TabReply> + Send + 'a>>;

/// Ask the machine to start `launch`'s agent, `op` first: a Create whose
/// answer is lost is followed by one Attach on the same request id, never by
/// a second Create.
pub(crate) async fn open_remote(env: &dyn WakeEnv, launch: &RemoteLaunch, op: TabOp) -> TabReply {
    if super::model::RoutineKey::parse(&launch.routine_key).is_some_and(|key| key.is_personal()) {
        return TabReply::NotStarted {
            reason: UndeliveredReason::Unavailable,
            detail: format!(
                "an Only me routine cannot start an agent on {}: machines run a team's agents, so make it a team routine",
                launch.machine
            ),
        };
    }
    match (op, env.remote_tab(launch, op).await) {
        (TabOp::Create, TabReply::Unknown(_) | TabReply::Pending(_)) => {
            env.remote_tab(launch, TabOp::Attach).await
        }
        (_, reply) => reply,
    }
}

// ───────────────────────────── the environment ─────────────────────────────

/// What the session list shows on every machine, and the machines whose
/// layout could not be read (each one could run the agent unseen).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Roster {
    pub candidates: Vec<Candidate>,
    /// Machines (`local` for this Mac) whose sessions could not be read.
    pub unreadable: Vec<String>,
    /// Machines the person hid or never set up: a `machine: any` wake does
    /// not wait to read them (a named one still does).
    pub ignorable: Vec<String>,
}

impl From<Vec<Candidate>> for Roster {
    fn from(candidates: Vec<Candidate>) -> Self {
        Self {
            candidates,
            unreadable: Vec::new(),
            ignorable: Vec::new(),
        }
    }
}

pub(crate) type RosterFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Roster, String>> + Send + 'a>>;

/// Where the launch registry's restore of this Mac's sessions stands after
/// an app restart (§9.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Restore {
    /// Still restoring: a wake waits (deferred).
    Pending,
    /// Every live launch is in the registry.
    Settled,
    /// It stopped short: whether the agent runs cannot be told.
    GaveUp,
}

/// What a wake tells the human inbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoticeKind {
    SpendWarn,
    SpendReached,
    SpendPending,
    /// It may have been typed (once per run).
    Unknown,
    /// Its agent runs on a machine that is offline (once per streak).
    Offline,
    /// A launch on a machine stayed unconfirmed for [`HOLD_TTL_MS`].
    LaunchUnconfirmed,
    /// SW-AGR-13: the routine made its `max_wakes` deliveries and paused
    /// itself (once per pause).
    MaxWakes,
    /// SW-AGR-13: the terminal server's backstop refused its wake (once per
    /// session per hour); `detail` names the top source.
    RateLimited,
    /// SW-AGR-13: a wake with limits was not sent: the machine's terminal
    /// server is too old to enforce them (once per machine per day).
    ServerOutdated,
}

/// One post to the human inbox: `needs-attention`, with a Show-tab backlink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WakeNotice {
    pub kind: NoticeKind,
    pub run_id: String,
    pub routine_key: String,
    pub machine: String,
    pub session: String,
    pub detail: String,
}

impl WakeNotice {
    fn routine(&self) -> &str {
        if self.routine_key.is_empty() {
            // A timer's, or a person's, wake.
            return "A wake";
        }
        self.routine_key
            .rsplit('/')
            .next()
            .unwrap_or(&self.routine_key)
    }

    /// A routine's note with no run: the arm sync's (SW-AGR-13).
    fn is_arm_note(&self) -> bool {
        self.run_id.is_empty() && !self.routine_key.is_empty()
    }

    /// The inbox item's headline.
    pub fn title(&self) -> String {
        match self.kind {
            NoticeKind::SpendWarn | NoticeKind::SpendReached | NoticeKind::SpendPending => {
                self.detail.clone()
            }
            NoticeKind::Unknown => format!("{}'s wake may have been delivered", self.routine()),
            NoticeKind::Offline => format!(
                "{} could not wake its agent: {} is offline",
                self.routine(),
                self.machine
            ),
            NoticeKind::LaunchUnconfirmed => format!(
                "{} could not tell whether its agent started on {}",
                self.routine(),
                self.machine
            ),
            NoticeKind::MaxWakes => format!(
                "{} paused itself: it made the wakes its max_wakes allows",
                self.routine()
            ),
            NoticeKind::RateLimited => format!(
                "{} could not wake its agent: too many wakes in the last hour. {}",
                self.routine(),
                self.detail
            ),
            // A machine-armed routine the sync could not arm (no run).
            NoticeKind::ServerOutdated if self.is_arm_note() => format!(
                "{} isn't running: {}'s terminal server is too old to enforce its limits.",
                self.routine(),
                self.machine
            ),
            NoticeKind::ServerOutdated => format!(
                "{} could not wake its agent on {}: {}",
                self.routine(),
                self.machine,
                self.detail
            ),
        }
    }

    /// Its sentence.
    pub fn context(&self) -> &'static str {
        match self.kind {
            NoticeKind::SpendWarn | NoticeKind::SpendReached | NoticeKind::SpendPending => "Open spend caps in the app to review the cap or pending change.",
            NoticeKind::Unknown => "May have been delivered. Check the terminal before re-sending.",
            NoticeKind::Offline => {
                "The agent still runs there, so nothing new was started. The next fire tries again."
            }
            NoticeKind::LaunchUnconfirmed => {
                "Check that machine's terminals: if the agent is not there, the next fire starts it."
            }
            NoticeKind::MaxWakes => {
                "Resume it to start a new count (raise policy.max_wakes first if it needs more)."
            }
            NoticeKind::RateLimited => {
                "A safety limit for loops, not a setting: wakes go in again on their own once they slow down."
            }
            NoticeKind::ServerOutdated if self.is_arm_note() => {
                "Update the terminal server, or remove the limits."
            }
            NoticeKind::ServerOutdated => {
                "Its wake limits would not be enforced there, so nothing was sent. Update that machine's terminal server."
            }
        }
    }

    /// The Show-tab backlink.
    pub fn backlink(&self) -> String {
        format!(
            "codesense://show-tab?machine={}&session={}&run={}",
            self.machine, self.session, self.run_id
        )
    }
}

/// What recording a wake's outcome may need from the app: the human inbox,
/// and dispatching a wake again.
pub(crate) trait WakeNotify: Send + Sync {
    fn post(&self, notice: &WakeNotice);
    /// Spend notices are deduplicated only after durable delivery.
    fn post_confirmed(&self, notice: &WakeNotice) -> bool {
        self.post(notice);
        true
    }
    /// Dispatch the run `pointer` names again after `after` (a deferred wake,
    /// or one whose agent quit, resolved again).
    fn redispatch(&self, pointer: RoutinePointer, after: Duration);
}

/// Nothing to tell: no app around (a routine store used on its own).
pub(crate) struct NoNotify;

impl WakeNotify for NoNotify {
    fn post(&self, _: &WakeNotice) {}
    fn redispatch(&self, _: RoutinePointer, _: Duration) {}
}

/// What a wake fire needs from the app around it. The app's is
/// [`super::wake_app::AppWakeEnv`]; tests pass fakes.
pub(crate) trait WakeEnv: WakeNotify {
    fn restore(&self) -> Restore;
    /// Every session that might be woken, on every machine.
    fn roster(&self) -> RosterFuture<'_>;
    /// Whether `machine` is one this desktop knows (shared with it).
    fn machine_known(&self, machine: &str) -> bool;
    /// The posture of the picked session (bypass, `allowWake`) as its
    /// launch recorded it, read for the routine's `principal`. Blocking.
    fn posture(&self, candidate: &Candidate, principal: &Principal) -> SessionPosture;
    /// Hand a wake to the delivery core. Blocking.
    fn deliver(&self, delivery: WakeDelivery) -> Result<DeliveryReceipt, DeliveryError>;
    /// Cancel a queued wake (archive). Blocking.
    fn cancel(
        &self,
        target: &SessionRef,
        idempotency_key: &str,
    ) -> Result<WakeRunStatus, DeliveryError>;
    /// Ask `launch.machine` to `op` the launch `launch` names (SW-AGR-2b):
    /// a Create prepares the routine's workspace there (SW-XS-7, resolved on
    /// the machine) and starts the agent with its prompt first, under
    /// `launch.request_id`; an Attach finds what that request did.
    fn remote_tab<'a>(&'a self, launch: &'a RemoteLaunch, op: TabOp) -> TabFuture<'a>;
    fn flights(&self) -> &LaunchFlights;
}

// ───────────────────────────── recording ─────────────────────────────

/// Keep the run's dispatch request, so a deferred wake (or one whose agent
/// quit) can be dispatched again; ending the run clears it.
fn keep_pointer(store: &RoutineStateStore, pointer: &RoutinePointer) -> StoreResult<()> {
    let json = serde_json::to_string(pointer)
        .map_err(|e| crate::coordinator::StoreError::Effect(e.to_string()))?;
    store.transaction(|tx| {
        tx.execute(
            "UPDATE routine_runs SET request = ?2
              WHERE run_id = ?1 AND phase = 'queued' AND reason IS NULL",
            params![
                pointer.run_id,
                crate::shared::secret_scan::redact_secrets(&json)
            ],
        )?;
        Ok(())
    })
}

/// `pointer` with `target` recorded as ended for its run.
fn with_ended(mut pointer: RoutinePointer, target: &SessionRef) -> RoutinePointer {
    let ended = ended_id(&target.machine, &target.session);
    if !pointer.ended_sessions.contains(&ended) {
        pointer.ended_sessions.push(ended);
    }
    pointer
}

fn ended_id(machine: &str, session: &str) -> String {
    format!("{machine}/{session}")
}

/// Keep `pointer` on its run whatever phase it is in (a reopened wake).
fn keep_pointer_any(store: &RoutineStateStore, pointer: &RoutinePointer) -> StoreResult<()> {
    let json = serde_json::to_string(pointer)
        .map_err(|e| crate::coordinator::StoreError::Effect(e.to_string()))?;
    store.transaction(|tx| {
        tx.execute(
            "UPDATE routine_runs SET request = ?2 WHERE run_id = ?1 AND phase = 'queued'",
            params![
                pointer.run_id,
                crate::shared::secret_scan::redact_secrets(&json)
            ],
        )?;
        Ok(())
    })
}

/// The dispatch request a wake run kept.
pub(super) fn kept_pointer(store: &RoutineStateStore, run_id: &str) -> Option<RoutinePointer> {
    let json: Option<String> = store
        .transaction(|tx| {
            Ok(tx
                .query_row(
                    "SELECT COALESCE(request, (SELECT request FROM run_requests WHERE run_requests.run_id = routine_runs.run_id)) FROM routine_runs WHERE run_id = ?1",
                    params![run_id],
                    |r| r.get(0),
                )
                .optional()?
                .flatten())
        })
        .ok()
        .flatten();
    serde_json::from_str(&json?).ok()
}

/// Every deferred wake: the ones an app that quit was still waiting on.
pub(crate) fn deferred(store: &RoutineStateStore) -> Vec<RoutinePointer> {
    let rows: StoreResult<Vec<String>> = store.transaction(|tx| {
        let mut stmt = tx.prepare(
            "SELECT request FROM routine_runs
              WHERE phase = 'queued' AND reason IS NULL AND wake_machine = ?1
                AND request IS NOT NULL",
        )?;
        let rows = stmt.query_map(params![DEFERRED], |r| r.get(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    });
    rows.unwrap_or_default()
        .iter()
        .filter_map(|json| serde_json::from_str(json).ok())
        .collect()
}

/// Bind a wake run to the session it asks to wake, before the delivery is
/// sent: its outcome then finds the run, an archive can cancel it, and the
/// run never kills a session it did not launch.
fn bind_target(store: &RoutineStateStore, run_id: &str, target: &SessionRef) -> StoreResult<()> {
    store.transaction(|tx| {
        tx.execute(
            "UPDATE routine_runs SET wake_machine = ?2, session_id = ?3
              WHERE run_id = ?1 AND phase = 'queued'",
            params![run_id, target.machine, target.session],
        )?;
        Ok(())
    })
}

/// A wake whose agent quit before it was typed (`session_ended`): unbind it
/// and resolve it again (up to [`MAX_REOPENS`] times), so it launches the
/// agent when none runs. True when it was reopened.
fn reopen(store: &RoutineStateStore, run_id: &str, target: &SessionRef) -> StoreResult<bool> {
    store.transaction(|tx| {
        Ok(tx.execute(
            "UPDATE routine_runs
                SET wake_machine = ?3, session_id = NULL,
                    wake_retries = COALESCE(wake_retries, 0) + 1
              WHERE run_id = ?1 AND phase = 'queued' AND reason IS NULL
                AND session_id = ?2 AND delivered_at IS NULL
                AND COALESCE(wake_retries, 0) < ?4 AND request IS NOT NULL",
            params![run_id, target.session, DEFERRED, MAX_REOPENS],
        )? == 1)
    })
}

/// Record `status` on wake run `run_id` (R7). An outcome about a session the
/// run is no longer bound to changes nothing; a `session_ended` before the
/// wake was typed resolves it again; an `unknown` posts once per run to the
/// human inbox.
pub(crate) fn record_status(
    store: &RoutineStateStore,
    run_id: &str,
    target: &SessionRef,
    status: &WakeRunStatus,
    notify: &dyn WakeNotify,
) {
    let run = match store.run(run_id) {
        Ok(Some(run)) => run,
        Ok(None) => return,
        Err(error) => {
            log::warn!(target: "routines", "wake run {run_id}: {error}");
            return;
        }
    };
    if run.phase == phase::QUEUED && run.session_id.as_deref() != Some(target.session.as_str()) {
        // Rebound, or resolved again: this outcome is about an earlier target.
        return;
    }
    if *status
        == (WakeRunStatus::Undelivered {
            reason: UndeliveredReason::SessionEnded,
        })
    {
        match reopen(store, run_id, target) {
            Ok(true) => {
                if let Some(pointer) = kept_pointer(store, run_id) {
                    // That session is ended for this run: never picked again,
                    // so it launches if absent (or, remote, ends honestly).
                    let pointer = with_ended(pointer, target);
                    let _ = keep_pointer_any(store, &pointer);
                    log::info!(target: "routines", "wake run {run_id}: {} ended before the wake went in; resolving again", target.session);
                    super::events::changed(&run.routine_key, run_id);
                    notify.redispatch(pointer, Duration::ZERO);
                    return;
                }
            }
            Ok(false) => {}
            Err(error) => log::warn!(target: "routines", "wake run {run_id}: {error}"),
        }
    }
    if let Err(error) = finish::apply_wake_status(store, run_id, target, status, finish::now_ms()) {
        log::warn!(target: "routines", "wake run {run_id}: {error}");
        return;
    }
    if let WakeRunStatus::Undelivered { reason } = status {
        on_limit(
            store,
            &LimitHit {
                run_id,
                fired_at: run.created_at,
                routine_key: &run.routine_key,
                target,
                reason: *reason,
                notice: None,
            },
            notify,
        );
    }
    let WakeRunStatus::Unknown { detail } = status else {
        return;
    };
    let first: StoreResult<bool> = store.transaction(|tx| {
        Ok(tx.execute(
            "UPDATE routine_runs SET wake_notified = 1
              WHERE run_id = ?1 AND phase = 'unknown' AND wake_notified IS NULL",
            params![run_id],
        )? == 1)
    });
    match first {
        Ok(true) => notify.post(&WakeNotice {
            kind: NoticeKind::Unknown,
            run_id: run_id.to_string(),
            routine_key: run.routine_key,
            machine: target.machine.clone(),
            session: target.session.clone(),
            detail: detail.clone(),
        }),
        Ok(false) => {}
        Err(error) => log::warn!(target: "routines", "wake run {run_id}: {error}"),
    }
}

/// Keep `detail` as an undelivered run's text, unless it has one.
fn keep_detail(store: &RoutineStateStore, run_id: &str, detail: &str) -> StoreResult<()> {
    store.transaction(|tx| {
        tx.execute(
            "UPDATE routine_runs SET result_text = ?2
              WHERE run_id = ?1 AND phase = 'undelivered' AND result_text IS NULL",
            params![run_id, crate::shared::secret_scan::redact_secrets(detail)],
        )?;
        Ok(())
    })
}

/// A run that ended on one of its routine's wake limits (SW-AGR-13).
pub(crate) struct LimitHit<'a> {
    pub run_id: &'a str,
    /// When the run fired (epoch ms): one fired before the routine's last
    /// resume never pauses it.
    pub fired_at: i64,
    pub routine_key: &'a str,
    pub target: &'a SessionRef,
    pub reason: UndeliveredReason,
    /// What the terminal server said about it, to tell a person once.
    pub notice: Option<&'a str>,
}

/// Whether no backstop note about `about` went out in the last hour.
fn first_note_in_hour(about: &str) -> bool {
    first_note_within(about, 60 * 60 * 1000)
}

/// Whether no note about `about` went out in the last `quiet_ms` (this app
/// run); marks it as gone out now when not.
pub(crate) fn first_note_within(about: &str, quiet_ms: i64) -> bool {
    static NOTED: std::sync::OnceLock<Mutex<HashMap<String, i64>>> = std::sync::OnceLock::new();
    let now = finish::now_ms();
    let mut noted = NOTED
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if noted.get(about).is_some_and(|at| now - *at < quiet_ms) {
        return false;
    }
    noted.insert(about.to_string(), now);
    true
}

/// The inbox note for a delivery core notice (SW-AGR-13), whatever asked for
/// the wake.
pub(crate) fn delivery_notice(event: &crate::delivery::events::WakeNoticeEvent) -> WakeNotice {
    use crate::delivery::events::WakeNoticeKind;
    let (run_id, routine_key) = match &event.origin {
        WakeOrigin::Routine {
            routine_key,
            run_id,
        } => (run_id.clone(), routine_key.clone()),
        _ => (String::new(), String::new()),
    };
    WakeNotice {
        kind: match event.kind {
            WakeNoticeKind::Backstop => NoticeKind::RateLimited,
            WakeNoticeKind::ServerOutdated => NoticeKind::ServerOutdated,
        },
        run_id,
        routine_key,
        machine: event.machine_id.clone(),
        session: event.session_id.clone(),
        detail: event.text.clone(),
    }
}

/// What a wake limit asks of this Mac (SW-AGR-13), wherever the run fired
/// (here, or on a machine with the desktop closed):
/// - `max_wakes`: pause the routine here (its switch, which also disarms it
///   on a machine), and post one inbox note when this paused it. Resuming
///   starts a new budget, so it continues.
/// - `rate_limited` with the server's notice (the first refusal of a trip):
///   post it once.
pub(crate) fn on_limit(store: &RoutineStateStore, hit: &LimitHit<'_>, notify: &dyn WakeNotify) {
    let notice = |kind, detail: String| WakeNotice {
        kind,
        run_id: hit.run_id.to_string(),
        routine_key: hit.routine_key.to_string(),
        machine: hit.target.machine.clone(),
        session: hit.target.session.clone(),
        detail,
    };
    match hit.reason {
        UndeliveredReason::MaxWakes => {
            match store.pause_for_wake_limit(hit.routine_key, hit.fired_at) {
                Ok(true) => {
                    log::info!(target: "routines", "routine {} reached its max_wakes and paused itself", hit.routine_key);
                    super::events::routine_changed(hit.routine_key);
                    notify.post(&notice(NoticeKind::MaxWakes, String::new()));
                }
                Ok(false) => {}
                Err(error) => {
                    log::warn!(target: "routines", "routine {}: not paused at its max_wakes: {error}", hit.routine_key)
                }
            }
        }
        // A desktop-fired wake's note is the delivery core's (any source);
        // this is a machine-fired run's, read from its log: once per routine
        // and machine per hour.
        UndeliveredReason::RateLimited => {
            if let Some(detail) = hit.notice.filter(|n| !n.trim().is_empty()) {
                if first_note_in_hour(&format!("{}\0{}", hit.target.machine, hit.routine_key)) {
                    notify.post(&notice(NoticeKind::RateLimited, detail.to_string()));
                }
            }
        }
        _ => {}
    }
}

/// A routine's own wake limits (SW-AGR-13) as the terminal server enforces
/// them: its budget is the routine key, the install that fires it (two Macs
/// running one routine never share a count) and that install's budget
/// generation for it (a resume starts a new one). `None` when it sets none.
pub(crate) fn wake_limits_of(
    def: &super::model::RoutineDef,
    routine_key: &str,
    install: &str,
    generation: u64,
) -> Option<codesense_pty_core::session_wake::WakeLimits> {
    use codesense_pty_core::session_wake::{GapPolicy, WakeLimits};
    if !def.policy.has_wake_limits() {
        return None;
    }
    if install.trim().is_empty() {
        // Never one budget shared by every install that has no id.
        log::warn!(target: "routines", "routine {routine_key}: this Mac's install id is unreadable; its wake limits are not sent");
        return None;
    }
    Some(WakeLimits {
        budget: format!("routine:{routine_key}#{install}:{generation}"),
        max_wakes: def.policy.max_wakes,
        max_wakes_per_day: def.policy.max_wakes_per_day,
        min_gap_ms: def
            .policy
            .min_gap_duration()
            .ok()
            .flatten()
            .map(|gap| u64::try_from(gap.as_millis()).unwrap_or(u64::MAX)),
        // `queue` has no meaning inside a gap: it coalesces.
        gap: match def.policy.overlap {
            super::model::OverlapPolicy::Skip => GapPolicy::Skip,
            super::model::OverlapPolicy::Coalesce | super::model::OverlapPolicy::Queue => {
                GapPolicy::Coalesce
            }
        },
    })
}

/// Record a routed delivery outcome (a push, a catch-up) on its routine run.
pub(crate) fn record_routed(
    store: &RoutineStateStore,
    outcome: &crate::delivery::RoutedOutcome,
    notify: &dyn WakeNotify,
) {
    let WakeOrigin::Routine { run_id, .. } = &outcome.origin else {
        return;
    };
    if !outcome.ours {
        return;
    }
    record_status(store, run_id, &outcome.target, &outcome.status, notify);
}

/// Cancel every queued wake of `routine_key` (its routine was archived):
/// `undelivered(archived)`. A wake whose machine cannot be reached now stays
/// queued on its run, so a delivery that still happens is recorded (§9.9).
/// A deferred one, not sent anywhere yet, ends here.
pub(crate) fn cancel_queued(store: &RoutineStateStore, routine_key: &str, env: &dyn WakeEnv) {
    let queued: StoreResult<Vec<(String, String, Option<String>)>> = store.transaction(|tx| {
        let mut stmt = tx.prepare(
            "SELECT run_id, wake_machine, session_id FROM routine_runs
              WHERE routine_key = ?1 AND phase = 'queued' AND reason IS NULL
                AND wake_machine IS NOT NULL",
        )?;
        let rows = stmt.query_map(params![routine_key], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    });
    let queued = match queued {
        Ok(queued) => queued,
        Err(error) => {
            log::warn!(target: "routines", "routine {routine_key}: queued wakes unreadable: {error}");
            return;
        }
    };
    for (run_id, machine, session) in queued {
        let Some(session) = session.filter(|_| machine != DEFERRED) else {
            finish::finish_and_announce(
                store,
                &run_id,
                undelivered(UndeliveredReason::Archived, "the routine was archived"),
                finish::now_ms(),
            );
            continue;
        };
        let target = SessionRef::new(machine, session);
        match env.cancel(&target, &run_id) {
            Ok(status) => record_status(store, &run_id, &target, &status, env),
            Err(error) => {
                log::warn!(target: "routines", "routine {routine_key}: wake {run_id} not cancelled: {error}")
            }
        }
    }
}

/// Withdraw `run`'s wake from the host it is queued on, for a user's Stop
/// ("Cancel wake"). `None` when the caller should end the run `cancelled`
/// itself: it has no queued wake on a host (fresh, or deferred and never
/// sent), or the host withdrew it and nothing recorded that yet.
/// `Some(true)` when the withdrawal is recorded as `cancelled` here.
/// `Some(false)` when the wake had already reached its end there (typed, or
/// unknown): that outcome is recorded honestly instead of a cancel. An
/// unreachable host keeps the wake queued and says so: ending the run here
/// could hide a paste that still happens (R5).
pub(crate) fn cancel_queued_run(
    home: &Path,
    store: &RoutineStateStore,
    run: &super::runs::RoutineRun,
) -> Result<Option<bool>, super::service::RoutineError> {
    let Some(machine) = run.wake_machine.as_deref() else {
        return Ok(None);
    };
    // Deferred: never handed to a host, so nothing waits anywhere.
    if machine == DEFERRED {
        return Ok(None);
    }
    // A wake that was sent somewhere is withdrawn there or not at all: ending it
    // only here could hide a paste that still happens (R5).
    let Some(session) = run.session_id.as_deref() else {
        return Err(super::service::RoutineError::Io(format!(
            "the wake's session on {machine} isn't known here, so it can't be withdrawn; it is still queued"
        )));
    };
    let Some(env) = env_for(home) else {
        return Err(super::service::RoutineError::Io(
            "wake delivery isn't running, so the wake can't be withdrawn; it is still queued"
                .into(),
        ));
    };
    let target = SessionRef::new(machine.to_string(), session.to_string());
    match env.cancel(&target, &run.run_id) {
        Ok(WakeRunStatus::Undelivered {
            reason: UndeliveredReason::Archived,
        }) => {
            // Withdrawn. The host reports a withdrawal as `archived`, and that
            // outcome may already be recorded on the run: a user's stop reads
            // as `cancelled`.
            let id = run.run_id.clone();
            let rewritten: StoreResult<usize> = store.transaction(|tx| {
                Ok(tx.execute(
                    "UPDATE routine_runs SET phase = 'cancelled', reason = NULL
                      WHERE run_id = ?1 AND phase = 'undelivered' AND reason = 'archived'",
                    params![id],
                )?)
            });
            match rewritten {
                Ok(0) => Ok(None),
                Ok(_) => {
                    super::events::changed(&run.routine_key, &run.run_id);
                    Ok(Some(true))
                }
                Err(error) => Err(super::service::RoutineError::Store(error.to_string())),
            }
        }
        Ok(status) => {
            record_status(store, &run.run_id, &target, &status, env.as_ref());
            Ok(Some(false))
        }
        Err(error) => Err(super::service::RoutineError::Io(format!(
            "couldn't reach {machine} to cancel the wake, so it is still queued: {error}"
        ))),
    }
}

// ───────────────────────────── archive hook ─────────────────────────────

/// The wake environment per CodeSense home, for the archive hook and the
/// outcome recorder: the app registers its own at startup, a test its fake.
static ENVS: Mutex<Vec<(PathBuf, Arc<dyn WakeEnv>)>> = Mutex::new(Vec::new());

/// Forget `home`'s environment (tests: the app keeps one for its lifetime).
#[cfg(test)]
pub(crate) fn uninstall(home: &Path) {
    ENVS.lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(h, _)| h != home);
}

/// Register `env` as `home`'s.
pub(crate) fn install(home: &Path, env: Arc<dyn WakeEnv>) {
    let mut envs = ENVS.lock().unwrap_or_else(|e| e.into_inner());
    envs.retain(|(h, _)| h != home);
    envs.push((home.to_path_buf(), env));
}

/// `home`'s wake environment, if one is installed.
pub(crate) fn env_for(home: &Path) -> Option<Arc<dyn WakeEnv>> {
    ENVS.lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(h, _)| h == home)
        .map(|(_, env)| env.clone())
}

/// `routine_key` was archived: cancel its queued wakes.
pub(crate) fn on_archived(home: &Path, routine_key: &str) {
    let Some(env) = env_for(home) else {
        return;
    };
    match RoutineStateStore::open(home) {
        Ok(store) => cancel_queued(&store, routine_key, env.as_ref()),
        Err(error) => log::warn!(target: "routines", "routine {routine_key}: {error}"),
    }
}

// ───────────────────────────── a fire ─────────────────────────────

/// A wake fire, resolved from its pinned revision.
#[derive(Debug, Clone)]
pub(crate) struct WakePlan {
    pub pointer: RoutinePointer,
    pub routine_key: String,
    /// What the scrollback annotation names: the routine and who last edited
    /// it (§9.5).
    pub marker: String,
    pub agent: String,
    /// Static values only (§9.5).
    pub text: String,
    pub header: String,
    pub max_wait_ms: u64,
    pub target: TargetMachine,
    pub preferred: Vec<String>,
    /// Who the run is dispatched as.
    pub principal: Principal,
    /// The routine's name and `where`, for a launch on another machine.
    pub routine_name: String,
    pub where_target: super::model::RoutineTarget,
    /// Its wake limits (SW-AGR-13), carried on every delivery.
    pub limits: Option<codesense_pty_core::session_wake::WakeLimits>,
}

impl WakePlan {
    /// What a launch-if-absent starts with: the header and the task.
    pub fn prompt(&self) -> String {
        format!("{}\n{}", self.header, self.text)
    }

    /// Its launch-if-absent on `machine`, under `request_id`.
    pub fn remote_launch(&self, machine: &str, request_id: uuid::Uuid) -> RemoteLaunch {
        let (org, team) = super::model::RoutineKey::parse(&self.routine_key)
            .map(|key| (key.org.clone(), key.team.clone()))
            .unwrap_or_default();
        RemoteLaunch {
            run_id: self.pointer.run_id.clone(),
            routine_key: self.routine_key.clone(),
            routine_name: self.routine_name.clone(),
            org,
            team,
            agent: self.agent.clone(),
            machine: machine.to_string(),
            target: self.where_target.clone(),
            prompt: self.prompt(),
            principal: self.principal.clone(),
            request_id,
        }
    }

    /// Its launch's single flight: on another machine, `(agent, "",
    /// machine)`, since a remote instance is matched by machine alone (this
    /// Mac's dir says nothing there); here, `(agent, routine, local)`.
    pub fn flight_key(&self) -> FlightKey {
        match &self.target {
            TargetMachine::Machine(machine) => (self.agent.clone(), String::new(), machine.clone()),
            _ => (
                self.agent.clone(),
                self.routine_key.clone(),
                self.target.launch_machine().to_string(),
            ),
        }
    }

    fn query(&self) -> WakeQuery<'_> {
        WakeQuery {
            agent: &self.agent,
            routine_key: &self.routine_key,
            target: &self.target,
            preferred: &self.preferred,
        }
    }
}

/// How a wake fire came out, for the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Fired {
    /// Handed to a live session; the run follows its delivery.
    Delivered(WakeRunStatus),
    /// Nothing live: launch this Mac's instance through the fresh path with
    /// the wake's prompt, then [`launched`].
    LaunchHere,
    /// Started on another machine; the run follows it as a fresh run.
    Launched,
    /// It has to wait: dispatched again shortly.
    Deferred,
    /// Ended here (the run says why).
    Ended,
}

/// The wake's launch-if-absent settled: `Some((session, handle))` once it
/// launched.
pub(crate) fn launched(env: &dyn WakeEnv, plan: &WakePlan, launched: Option<(String, String)>) {
    env.flights()
        .finish(&plan.flight_key(), launched, finish::now_ms());
}

async fn blocking<T: Send + 'static>(
    what: &'static str,
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    crate::commands::await_blocking(what, f).await
}

/// Run `f` on the routine store, off the async reactor.
async fn with_store<T: Send + 'static>(
    home: &Path,
    what: &'static str,
    f: impl FnOnce(&RoutineStateStore) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let home = home.to_path_buf();
    blocking(what, move || {
        let store = RoutineStateStore::open(&home).map_err(|e| e.to_string())?;
        f(&store)
    })
    .await
}

async fn end(home: &Path, run_id: &str, ending: Ending) {
    let run_id = run_id.to_string();
    let _ = with_store(home, "routine_wake_end", move |store| {
        finish::finish_and_announce(store, &run_id, ending, finish::now_ms());
        Ok(())
    })
    .await;
}

fn undelivered(reason: UndeliveredReason, detail: impl Into<String>) -> Ending {
    let status = WakeRunStatus::Undelivered { reason };
    Ending::Wake {
        phase: status.run_phase(),
        reason: status.run_reason(),
        detail: Some(detail.into()),
    }
}

/// The wake has to wait: hand it back to be dispatched again, unless it has
/// waited [`DEFER_CAP_MS`] since it fired, then `undelivered(unobservable)`.
async fn defer(home: &Path, plan: &WakePlan, env: &Arc<dyn WakeEnv>, why: String) -> Fired {
    let run_id = plan.pointer.run_id.clone();
    if finish::now_ms() - plan.pointer.fired_at >= DEFER_CAP_MS {
        end(
            home,
            &run_id,
            undelivered(UndeliveredReason::Unobservable, why),
        )
        .await;
        return Fired::Ended;
    }
    let id = run_id.clone();
    let _ = with_store(home, "routine_wake_defer", move |store| {
        store
            .transaction(|tx| {
                tx.execute(
                    "UPDATE routine_runs SET wake_machine = ?2
                      WHERE run_id = ?1 AND phase = 'queued' AND wake_machine IS NULL",
                    params![id, DEFERRED],
                )?;
                Ok(())
            })
            .map_err(|e| e.to_string())
    })
    .await;
    log::info!(target: "routines", "wake run {run_id} deferred: {why}");
    env.redispatch(plan.pointer.clone(), DEFER_STEP);
    Fired::Deferred
}

/// Fire `plan` as a wake. The run stays `queued` until its delivery says
/// otherwise; [`Fired::LaunchHere`] hands a launch back to the caller. It
/// never waits: whatever has to, is deferred.
pub(crate) async fn fire(home: &Path, plan: &WakePlan, env: Arc<dyn WakeEnv>) -> Fired {
    let run_id = plan.pointer.run_id.clone();
    let pointer = plan.pointer.clone();
    let _ = with_store(home, "routine_wake_keep", move |store| {
        keep_pointer(store, &pointer).map_err(|e| e.to_string())
    })
    .await;
    if let TargetMachine::Machine(id) = &plan.target {
        if !env.machine_known(id) {
            end(
                home,
                &run_id,
                undelivered(
                    UndeliveredReason::MachineUnknown,
                    format!("{id} isn't a machine this Mac knows or shares with this team"),
                ),
            )
            .await;
            return Fired::Ended;
        }
    }
    match env.restore() {
        Restore::Settled => {}
        Restore::Pending => {
            return defer(
                home,
                plan,
                &env,
                "CodeSense was still restoring this Mac's sessions, so it could not tell whether the agent runs".into(),
            )
            .await
        }
        Restore::GaveUp => {
            end(
                home,
                &run_id,
                undelivered(
                    UndeliveredReason::Unobservable,
                    "CodeSense could not restore this Mac's sessions after a restart, so it could not tell whether the agent runs",
                ),
            )
            .await;
            return Fired::Ended;
        }
    }
    let now = finish::now_ms();
    let Roster {
        mut candidates,
        unreadable,
        ignorable,
    } = match env.roster().await {
        Ok(roster) => roster,
        Err(error) => {
            return defer(
                home,
                plan,
                &env,
                format!("the session list could not be read: {error}"),
            )
            .await
        }
    };
    // SW-AGR-2b: an unconfirmed launch held before an app restart is held
    // again; one whose tab now shows on its machine is settled.
    if let TargetMachine::Machine(_) = &plan.target {
        restore_hold(home, plan, env.as_ref()).await;
    }
    let seen: Vec<String> = candidates
        .iter()
        .filter_map(|c| c.request_id.clone())
        .collect();
    for settled in env.flights().release_seen(&seen) {
        settle_request(home, settled).await;
    }
    // Sessions this run found ended after picking them count as ended, even
    // where the list still shows them live.
    for c in candidates.iter_mut() {
        if plan
            .pointer
            .ended_sessions
            .contains(&ended_id(&c.machine, &c.session))
        {
            c.activity = Activity::Ended;
        }
    }
    // A session just launched (by a wake or a person), until the list shows it.
    let listed: Vec<String> = candidates.iter().map(|c| c.session.clone()).collect();
    candidates.extend(env.flights().launched(now).into_iter().filter(|c| {
        c.machine != LOCAL
            && !listed.contains(&c.session)
            && !plan
                .pointer
                .ended_sessions
                .contains(&ended_id(&c.machine, &c.session))
    }));
    match pick(&candidates, &plan.query()) {
        Pick::Deliver(c) => deliver(home, plan, env, c).await,
        Pick::Offline(c) => offline(home, plan, env.as_ref(), c).await,
        Pick::Starting => {
            defer(
                home,
                plan,
                &env,
                format!("{} was still starting", plan.agent),
            )
            .await
        }
        Pick::Quitting => {
            defer(
                home,
                plan,
                &env,
                format!(
                    "{} had just left its prompt; looking again before starting another",
                    plan.agent
                ),
            )
            .await
        }
        // A machine in the target whose sessions could not be read may run the
        // agent unseen: never launch over it.
        Pick::Absent if !unseen(&plan.target, &unreadable, &ignorable).is_empty() => {
            defer(
                home,
                plan,
                &env,
                format!(
                    "the sessions on {} could not be read",
                    unseen(&plan.target, &unreadable, &ignorable).join(", ")
                ),
            )
            .await
        }
        Pick::Absent => match env.flights().begin(&plan.flight_key(), now) {
            Begin::Wait => {
                defer(
                    home,
                    plan,
                    &env,
                    format!("a launch of {} was still in flight", plan.agent),
                )
                .await
            }
            begun => launch_absent(home, plan, env, begun).await,
        },
    }
}

/// The machines of `target` whose sessions could not be read: this Mac for a
/// local wake (and every launch here), the named machine, or any for `any`
/// but the ones the person hid or never set up (`ignorable`).
fn unseen(target: &TargetMachine, unreadable: &[String], ignorable: &[String]) -> Vec<String> {
    unreadable
        .iter()
        .filter(|machine| match target {
            TargetMachine::Any => machine.as_str() == LOCAL || !ignorable.contains(machine),
            TargetMachine::Local => machine.as_str() == LOCAL,
            TargetMachine::Machine(id) => machine == &id,
        })
        .cloned()
        .collect()
}

async fn launch_absent(home: &Path, plan: &WakePlan, env: Arc<dyn WakeEnv>, begun: Begin) -> Fired {
    let run_id = plan.pointer.run_id.clone();
    let TargetMachine::Machine(machine) = &plan.target else {
        return Fired::LaunchHere;
    };
    // SW-AGR-2b: one request id per run, so a Create repeated after a lost
    // answer is the same launch; an earlier unconfirmed one is only attached.
    // A hold past its TTL is attached once more before a person is asked.
    let mut expired = None;
    let (mut request_id, mut op) = match begun {
        Begin::Reattach(request_id) => (request_id, TabOp::Attach),
        Begin::HoldExpired(hold) => {
            expired = Some(hold);
            (hold.request_id, TabOp::Attach)
        }
        Begin::Go | Begin::Wait => (launch_request_id(&run_id), TabOp::Create),
    };
    loop {
        let launch = plan.remote_launch(machine, request_id);
        let reply = open_remote(env.as_ref(), &launch, op).await;
        if op == TabOp::Attach && !matches!(reply, TabReply::Unknown(_) | TabReply::Pending(_)) {
            // A definitive answer about the held launch: no longer held.
            settle_request(home, request_id).await;
        }
        match (op, reply) {
            (TabOp::Create, TabReply::Opened { session, handle })
            | (TabOp::Create, TabReply::Ended { session, handle }) => {
                // Its agent started with this run's task: delivered (§9.6).
                launched(env.as_ref(), plan, Some((session.clone(), handle)));
                record_remote_launch(home, &run_id, machine, &session, request_id).await;
                return Fired::Launched;
            }
            (TabOp::Attach, TabReply::Opened { session, handle }) => {
                // An earlier run started it: this run's task goes to it as a
                // wake, once it shows (it is live and busy until then).
                launched(env.as_ref(), plan, Some((session, handle)));
                return defer(
                    home,
                    plan,
                    &env,
                    format!("{} was still starting on {machine}", plan.agent),
                )
                .await;
            }
            (TabOp::Attach, TabReply::Ended { .. }) => {
                // The earlier launch's agent is gone: start one now, under
                // this run's own request id, in this fire.
                launched(env.as_ref(), plan, None);
                if env.flights().begin(&plan.flight_key(), finish::now_ms()) != Begin::Go {
                    return defer(
                        home,
                        plan,
                        &env,
                        format!("a launch of {} was still in flight", plan.agent),
                    )
                    .await;
                }
                request_id = launch_request_id(&run_id);
                op = TabOp::Create;
            }
            (TabOp::Attach, reply @ (TabReply::Unknown(_) | TabReply::Pending(_)))
                if expired.is_some() =>
            {
                let hold = expired.expect("expired");
                let pending = matches!(reply, TabReply::Pending(_));
                return hold_expired(home, plan, env.as_ref(), machine, hold, pending).await;
            }
            (op, TabReply::Unknown(detail) | TabReply::Pending(detail)) => {
                // It may have started: every later launch only attaches (R1).
                env.flights()
                    .hold(&plan.flight_key(), request_id, finish::now_ms());
                if op == TabOp::Attach {
                    return defer(
                        home,
                        plan,
                        &env,
                        format!(
                            "the launch of {} on {machine} was not confirmed yet ({detail})",
                            plan.agent
                        ),
                    )
                    .await;
                }
                unconfirmed(
                    home,
                    &plan.routine_key,
                    &run_id,
                    &plan.agent,
                    machine,
                    request_id,
                    &detail,
                    env.as_ref(),
                )
                .await;
                return Fired::Ended;
            }
            (_, TabReply::Running(detail)) => {
                // Another launcher (the machine's own routine) got there
                // first: this run's task goes to that session, once it shows.
                launched(env.as_ref(), plan, None);
                return defer(home, plan, &env, detail).await;
            }
            (op, TabReply::NotStarted { reason, detail }) => {
                launched(env.as_ref(), plan, None);
                if op == TabOp::Attach {
                    // The earlier launch started nothing: this run may start one.
                    return defer(home, plan, &env, detail).await;
                }
                end(home, &run_id, undelivered(reason, detail)).await;
                return Fired::Ended;
            }
        }
    }
}

/// A wake's agent started on `machine` with its task as its first prompt:
/// bind the run to that session (the agent's own, never the run's to kill)
/// and record it delivered (a result schema keeps it running).
async fn record_remote_launch(
    home: &Path,
    run_id: &str,
    machine: &str,
    session: &str,
    request_id: uuid::Uuid,
) {
    let (id, machine, session) = (run_id.to_string(), machine.to_string(), session.to_string());
    let _ = with_store(home, "routine_wake_launched", move |store| {
        let now = finish::now_ms();
        store
            .transaction(|tx| {
                tx.execute(
                    "UPDATE routine_runs
                        SET started_at = COALESCE(started_at, ?3), session_id = ?2,
                            wake_machine = ?4, launch_machine = ?4, launch_request = ?5
                      WHERE run_id = ?1 AND phase = 'queued'",
                    params![id, session, now, machine, request_id.to_string()],
                )?;
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        finish::apply_wake_status(
            store,
            &id,
            &SessionRef::new(&machine, &session),
            &WakeRunStatus::Delivered {
                delivered_at_ms: u64::try_from(now).unwrap_or_default(),
            },
            now,
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    })
    .await;
}

/// A held launch got a definitive answer (or its tab showed): no run's
/// `unknown` keeps it held across a restart any more.
async fn settle_request(home: &Path, request_id: uuid::Uuid) {
    let _ = with_store(home, "routine_launch_settled", move |store| {
        store
            .transaction(|tx| {
                tx.execute(
                    "UPDATE routine_runs SET launch_request = NULL
                      WHERE launch_request = ?1 AND phase = 'unknown'",
                    params![request_id.to_string()],
                )?;
                Ok(())
            })
            .map_err(|e| e.to_string())
    })
    .await;
}

/// After an app restart: hold again the latest launch of this agent on this
/// machine that a run declared `unknown` within [`HOLD_TTL_MS`] (SPEC R1).
async fn restore_hold(home: &Path, plan: &WakePlan, env: &dyn WakeEnv) {
    let TargetMachine::Machine(machine) = &plan.target else {
        return;
    };
    let key = plan.flight_key();
    if env.flights().held(&key).is_some() {
        return;
    }
    let (agent, machine) = (plan.agent.clone(), machine.clone());
    let since = finish::now_ms() - HOLD_TTL_MS;
    let held: Option<(String, i64)> = with_store(home, "routine_launch_restore", move |store| {
        store
            .transaction(|tx| {
                Ok(tx
                    .query_row(
                        "SELECT launch_request, ended_at FROM routine_runs
                          WHERE phase = 'unknown' AND launch_request IS NOT NULL
                            AND launch_machine = ?1 AND agent_ref = ?2 AND ended_at >= ?3
                          ORDER BY ended_at DESC LIMIT 1",
                        params![machine, agent, since],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?)
            })
            .map_err(|e| e.to_string())
    })
    .await
    .ok()
    .flatten();
    if let Some((request, at)) = held {
        if let Ok(request_id) = uuid::Uuid::parse_str(&request) {
            env.flights().restore_hold(&key, request_id, at);
        }
    }
}

/// A launch held unconfirmed for [`HOLD_TTL_MS`], whose Attach still did not
/// confirm it either way: a person is asked (`needs_you`, posted once per
/// hold). While its launch is still running on the machine (`pending`) the
/// hold is kept, so nothing Creates over it; otherwise it is dropped and the
/// next fire may launch (a tab it did start would show on the machine).
async fn hold_expired(
    home: &Path,
    plan: &WakePlan,
    env: &dyn WakeEnv,
    machine: &str,
    hold: Expired,
    pending: bool,
) -> Fired {
    let request_id = hold.request_id;
    if pending {
        env.flights().keep_expired(&plan.flight_key(), hold);
    } else {
        settle_request(home, request_id).await;
    }
    let detail = if pending {
        format!(
            "{} is still being started on {machine} (launch request {request_id}) after {} minutes; nothing new is started while it is",
            plan.agent,
            HOLD_TTL_MS / 60_000
        )
    } else {
        format!(
            "{} may have started on {machine} (launch request {request_id}), but it never showed there within {} minutes",
            plan.agent,
            HOLD_TTL_MS / 60_000
        )
    };
    end(
        home,
        &plan.pointer.run_id,
        Ending::Wake {
            phase: phase::NEEDS_YOU,
            reason: None,
            detail: Some(detail.clone()),
        },
    )
    .await;
    if !hold.asked {
        env.post(&WakeNotice {
            kind: NoticeKind::LaunchUnconfirmed,
            run_id: plan.pointer.run_id.clone(),
            routine_key: plan.routine_key.clone(),
            machine: machine.to_string(),
            session: String::new(),
            detail,
        });
    }
    Fired::Ended
}

/// A launch whose start was never confirmed: the run is `unknown` (R5),
/// posted once to the human inbox, never started again. The run keeps the
/// launch's request id, so a restart holds it again.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn unconfirmed(
    home: &Path,
    routine_key: &str,
    run_id: &str,
    agent: &str,
    machine: &str,
    request_id: uuid::Uuid,
    detail: &str,
    notify: &dyn WakeNotify,
) {
    let detail =
        format!("the agent may have started on {machine} (launch request {request_id}): {detail}");
    let (id, agent_ref, on) = (run_id.to_string(), agent.to_string(), machine.to_string());
    let _ = with_store(home, "routine_launch_unknown", move |store| {
        store
            .transaction(|tx| {
                tx.execute(
                    "UPDATE routine_runs
                        SET launch_machine = ?2, launch_request = ?3,
                            agent_ref = COALESCE(agent_ref, ?4)
                      WHERE run_id = ?1",
                    params![id, on, request_id.to_string(), agent_ref],
                )?;
                Ok(())
            })
            .map_err(|e| e.to_string())
    })
    .await;
    end(
        home,
        run_id,
        Ending::Wake {
            phase: phase::UNKNOWN,
            reason: None,
            detail: Some(detail.clone()),
        },
    )
    .await;
    notify.post(&WakeNotice {
        kind: NoticeKind::Unknown,
        run_id: run_id.to_string(),
        routine_key: routine_key.to_string(),
        machine: machine.to_string(),
        session: String::new(),
        detail,
    });
}

/// The agent runs only on a machine that is offline: never start another
/// (R1). `undelivered(machine_offline)`, posted to the inbox once per streak.
async fn offline(home: &Path, plan: &WakePlan, env: &dyn WakeEnv, c: Candidate) -> Fired {
    let (run_id, key) = (plan.pointer.run_id.clone(), plan.routine_key.clone());
    let detail = format!(
        "{} runs on {}, which is offline; nothing new was started",
        plan.agent, c.machine
    );
    let ending = undelivered(UndeliveredReason::MachineOffline, detail.clone());
    let (id, k) = (run_id.clone(), key.clone());
    let first = with_store(home, "routine_wake_offline", move |store| {
        let before: Option<Option<String>> = store
            .transaction(|tx| {
                Ok(tx
                    .query_row(
                        "SELECT reason FROM routine_runs
                          WHERE routine_key = ?1 AND run_id <> ?2
                            AND phase NOT IN ('coalesced', 'skipped')
                          ORDER BY created_at DESC, rowid DESC LIMIT 1",
                        params![k, id],
                        |r| r.get(0),
                    )
                    .optional()?)
            })
            .map_err(|e| e.to_string())?;
        finish::finish_and_announce(store, &id, ending, finish::now_ms());
        Ok(before.flatten().as_deref() != Some("machine_offline"))
    })
    .await
    .unwrap_or(false);
    if first {
        env.post(&WakeNotice {
            kind: NoticeKind::Offline,
            run_id,
            routine_key: key,
            machine: c.machine,
            session: c.session,
            detail,
        });
    }
    Fired::Ended
}

async fn deliver(home: &Path, plan: &WakePlan, env: Arc<dyn WakeEnv>, c: Candidate) -> Fired {
    let run_id = plan.pointer.run_id.clone();
    let (root, principal, candidate_agent) =
        (home.to_path_buf(), plan.principal.clone(), c.agent.clone());
    let refusal = blocking("routine_wake_budget", move || {
        let key = candidate_agent
            .as_deref()
            .and_then(crate::agents::AgentKey::parse)
            .ok_or_else(|| "cannot identify the saved agent for this wake".to_string())?;
        crate::session_usage::spend_caps::refuse_agent(&root, &principal, &key)
    })
    .await;
    if let Err(detail) = refusal {
        end(
            home,
            &run_id,
            Ending::Wake {
                phase: super::runs::phase::SKIPPED,
                reason: Some(super::runs::reason::BUDGET_REACHED),
                detail: Some(detail),
            },
        )
        .await;
        return Fired::Ended;
    }
    let target = SessionRef::new(&c.machine, &c.session);
    // Right before typing: a routine archived meanwhile sends nothing.
    let (home_buf, principal, routine) = (
        home.to_path_buf(),
        plan.principal.clone(),
        super::model::RoutineKey::parse(&plan.routine_key),
    );
    let archived = blocking("routine_wake_archived", move || {
        Ok(routine.is_some_and(|key| {
            super::service::get_current(&home_buf, &principal, &key)
                .is_ok_and(|d| d.def.is_archived())
        }))
    })
    .await
    .unwrap_or(false);
    if archived {
        end(
            home,
            &run_id,
            undelivered(
                UndeliveredReason::Archived,
                "the routine was archived before its wake was sent",
            ),
        )
        .await;
        return Fired::Ended;
    }
    let (id, bound) = (run_id.clone(), target.clone());
    let _ = with_store(home, "routine_wake_bind", move |store| {
        bind_target(store, &id, &bound).map_err(|e| e.to_string())
    })
    .await;
    let delivery = WakeDelivery {
        target: target.clone(),
        text: plan.text.clone(),
        header: Some(plan.header.clone()),
        marker: plan.marker.clone(),
        max_wait_ms: plan.max_wait_ms,
        idempotency_key: run_id.clone(),
        origin: WakeOrigin::Routine {
            routine_key: plan.routine_key.clone(),
            run_id: run_id.clone(),
        },
        posture: SessionPosture::default(),
        limits: plan.limits.clone(),
    };
    let (sent, principal) = (env.clone(), plan.principal.clone());
    let receipt = blocking("routine_wake_deliver", move || {
        let posture = sent.posture(&c, &principal);
        Ok(sent.deliver(WakeDelivery {
            posture,
            ..delivery
        }))
    })
    .await;
    let mut why_undelivered = None;
    let status = match receipt {
        Ok(Ok(receipt)) => {
            // What the delivery core said about a wake that was not typed (an
            // outdated server, an offline machine): the run's detail.
            if matches!(receipt.status, WakeRunStatus::Undelivered { .. }) {
                why_undelivered = receipt.notice;
            }
            receipt.status
        }
        Ok(Err(DeliveryError::Unknown(detail))) => WakeRunStatus::Unknown { detail },
        Ok(Err(error)) => {
            // Refused (authority, the gate): nothing was sent.
            end(
                home,
                &run_id,
                Ending::Wake {
                    phase: phase::UNDELIVERED,
                    reason: Some(REASON_WAKE_REFUSED),
                    detail: Some(error.to_string()),
                },
            )
            .await;
            return Fired::Ended;
        }
        Err(error) => WakeRunStatus::Unknown { detail: error },
    };
    let (id, recorded, notify, outcome) = (run_id, target, env, status.clone());
    let _ = with_store(home, "routine_wake_record", move |store| {
        record_status(store, &id, &recorded, &outcome, notify.as_ref());
        if let Some(detail) = why_undelivered {
            keep_detail(store, &id, &detail).map_err(|e| e.to_string())?;
        }
        Ok(())
    })
    .await;
    Fired::Delivered(status)
}

/// The handles an `inbox_idle` fire names (`trigger.sessions[].handle`).
pub(crate) fn preferred_handles(pointer: &RoutinePointer) -> Vec<String> {
    pointer
        .trigger_payload
        .as_ref()
        .and_then(|p| p.get("sessions"))
        .and_then(|s| s.as_array())
        .map(|sessions| {
            sessions
                .iter()
                .filter_map(|s| s.get("handle").and_then(|h| h.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// `policy.max_wait` (`30m`, `2h`), else [`DEFAULT_MAX_WAIT_MS`].
pub(crate) fn max_wait_ms(def: &super::model::RoutineDef) -> u64 {
    def.policy
        .extra
        .get("max_wait")
        .and_then(|v| v.as_str())
        .and_then(|raw| {
            let raw = raw.trim();
            let (digits, unit) = match raw.strip_suffix('m') {
                Some(d) => (d, 60_000u64),
                None => (raw.strip_suffix('h')?, 3_600_000u64),
            };
            digits.parse::<u64>().ok().map(|n| n * unit)
        })
        .filter(|ms| *ms > 0)
        .unwrap_or(DEFAULT_MAX_WAIT_MS)
}
