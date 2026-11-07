// SPDX-License-Identifier: MIT
//! The routine service: the one API every routine surface (MCP tools, the
//! Routines tab, the run read service) wraps.
//!
//! Every fn takes `(codesense_home, principal, …)`, needs no `AppHandle`, and
//! does blocking filesystem and SQLite work; async callers run it inside
//! `await_blocking`. Nothing here asks who owns a routine: anyone who can see
//! one may change, pause, run, stop, move or archive it, like any other team
//! content. A change is a local context-repo edit, and sharing it is a git push
//! the user's agent makes; `owner` only says who created it. Definition writes
//! are revisioned: each one writes an immutable `revisions/<n>.yml` snapshot
//! (crediting its author) and bumps `revision`, under a per-routine lock so a
//! stale base is a typed [`RoutineError::Conflict`].
//! Pausing is local state and never touches git. Every definition write drops
//! the scheduler's discovery cache, so the next tick sees it.
//!
//! Run control: [`run_now`] admits a manual fire through the same admission as
//! a scheduled one, and [`stop_run`] ends a run `cancelled` before killing its
//! sessions.
//!
//! Reads: [`list_summaries`] (each routine with its next and last run),
//! [`runs`] (paged history, skips and merges included), [`run_detail`],
//! [`needs_you_count`], [`preview_schedule`], [`scheduler_status`] and
//! [`context_for_session`]. A run's result lives on its row; a routine needs
//! you while its latest terminal run is `needs_you`, so the next run that ends
//! any other way clears it with no acknowledgement and no separate store. A run
//! that simply `ended` reported nothing and leaves it as it was.
//!
//! Every write emits `routines:changed`, so whatever shows these reads knows
//! to re-read.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::context_repo::org_slug::validate_org_slug;
use crate::context_repo::scoped_key::BUILTIN_ORG;
use crate::context_repo::scratch_space::{self, is_scratch_team, SCRATCH_TEAM};
use crate::context_repo::{path_to_org_root, path_to_team_config_dir};
use crate::domain::Principal;

use super::admit::{self, Admission, Fire};
use super::agents::{self, AgentDisplay};
use super::defs::{self, RevisionRecord};
use super::discover::{
    agent_is_here, discover_routines, invalidate_discovery_cache, OwnerNames, RoutineScope,
};
use super::dispatch;
use super::events;
use super::finish::{self, Ending, Sessions};
use super::host::{is_armed_anywhere, is_armed_here, local_host_id};
use super::layout;
use super::migrate;
use super::model::{
    is_valid_id, AgentRef, Author, RoutineDef, RoutineKey, RoutineMode, RoutineTrigger, RunsOn,
    ValidationError, EVENT_INBOX_IDLE, EVENT_ROLE_PENDING, PERSONAL_OWNER,
};
use super::runs::RoutineRun;
use super::schedule;
use super::state::{enabled_here, RoutineStateStore, TriggerRow};
use super::tick::{self, RunSource};
use crate::agents::launch_check::LaunchWarning;
use crate::agents::AgentKey;

/// The most enabled, non-archived routines armed on this Mac. A sanity cap against runaway agent authoring,
/// not a policy.
pub const MAX_ACTIVE_ROUTINES: usize = 25;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutineError {
    Validation(ValidationError),
    /// The save's base revision is stale; nothing was written.
    Conflict {
        current_revision: u32,
    },
    /// The save would exceed [`MAX_ACTIVE_ROUTINES`].
    LimitReached {
        limit: usize,
    },
    NotFound(RoutineKey),
    RevisionNotFound {
        key: RoutineKey,
        revision: u32,
    },
    AlreadyExists(RoutineKey),
    /// The routine is archived, so it cannot be armed.
    Archived(RoutineKey),
    /// The org/team/id does not name a place this user can hold routines.
    InvalidLocation(String),
    /// A manual run named a variable the routine does not declare.
    UnknownVariable(String),
    RunNotFound(String),
    /// The session is not running a routine.
    NotARoutineRun(String),
    /// Its agent is nowhere this Mac can see, so it does not run until
    /// someone picks one.
    Unassigned(RoutineKey),
    /// SW-AGR-8: armed on a machine, it fires there only: a fire by hand is
    /// the machine's too.
    OnMachine {
        key: RoutineKey,
        machine: String,
    },
    /// SW-AGR-8: moved off `machine`, which has not yet confirmed it stopped
    /// firing it: nothing fires it elsewhere meanwhile, by hand either.
    HandingOver {
        key: RoutineKey,
        machine: String,
    },
    Io(String),
    Store(String),
}

impl fmt::Display for RoutineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(e) => write!(f, "{e}"),
            Self::Conflict { current_revision } => write!(
                f,
                "routine changed since you loaded it (now at revision {current_revision}); reload and retry"
            ),
            Self::LimitReached { limit } => write!(
                f,
                "you already have {limit} active routines on this machine; pause or archive one first"
            ),
            Self::NotFound(key) => write!(f, "routine {key} not found"),
            Self::RevisionNotFound { key, revision } => {
                write!(f, "routine {key} has no revision {revision}")
            }
            Self::AlreadyExists(key) => write!(f, "routine {key} already exists"),
            Self::Archived(key) => write!(f, "routine {key} is archived"),
            Self::InvalidLocation(why) => write!(f, "{why}"),
            Self::UnknownVariable(name) => {
                write!(f, "the routine declares no variable named {name:?}")
            }
            Self::RunNotFound(run_id) => write!(f, "routine run {run_id} not found"),
            Self::NotARoutineRun(session_id) => {
                write!(f, "session {session_id} is not a routine run")
            }
            Self::Unassigned(key) => write!(
                f,
                "routine {key} has no agent here (it is Unassigned): pick an agent for it first"
            ),
            Self::OnMachine { key, machine } => write!(
                f,
                "routine {key} is armed on {machine}: it runs there, so a run by hand is fired on {machine}"
            ),
            Self::HandingOver { key, machine } => write!(
                f,
                "routine {key} was moved off {machine}, which has not yet confirmed it stopped firing it: it cannot run here until {machine} reconnects and confirms (or, if {machine} is gone for good, release the handover with routine_handover_release)"
            ),
            Self::Io(e) => write!(f, "routine io error: {e}"),
            Self::Store(e) => write!(f, "routine state error: {e}"),
        }
    }
}

impl std::error::Error for RoutineError {}

impl From<ValidationError> for RoutineError {
    fn from(e: ValidationError) -> Self {
        Self::Validation(e)
    }
}

impl From<defs::DefsError> for RoutineError {
    fn from(e: defs::DefsError) -> Self {
        Self::Io(e.to_string())
    }
}

impl From<crate::coordinator::StoreError> for RoutineError {
    fn from(e: crate::coordinator::StoreError) -> Self {
        Self::Store(e.to_string())
    }
}

/// The signed-in profile and the org whose scratch space a personal save
/// lands in (created on first save).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalOwner {
    pub github_user_id: u64,
    pub username: String,
    pub display_name: String,
    pub avatar_url: Option<String>,
    /// The org the item is filed under: it applies in that org only.
    pub org: String,
}

/// Where a new routine is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// The caller's scratch space for the owner's org, created on first save.
    Personal(PersonalOwner),
    /// A locally-synced team.
    Team { org: String, team: String },
}

/// What a save does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveTarget {
    /// Write a new routine (revision 1) at the placement.
    Create(Placement),
    /// Replace an existing routine. `base_revision` is the revision the edit was
    /// made against.
    Update { key: RoutineKey, base_revision: u32 },
}

/// A routine as listed: the definition plus its machine-local facts, what
/// will run next and what ran last.
#[derive(Debug, Clone, Serialize)]
pub struct RoutineSummary {
    pub action: super::model::RoutineAction,
    pub command: Vec<String>,
    pub key: RoutineKey,
    pub scope: RoutineScope,
    pub armed_here: bool,
    pub owner_display: String,
    /// This Mac's switch: a shared job not paused here, or a recipe each Mac
    /// runs that this Mac turned on.
    pub enabled: bool,
    /// The author of revision 1, when its snapshot is present.
    pub created_by: Option<Author>,
    /// Every trigger in plain English ("weekdays at 9:00 MDT"), or "manual
    /// only".
    pub schedule_in_words: String,
    /// Unix epoch milliseconds of the next scheduled fire; `None` when paused
    /// here, when it names no Mac to run on, or when no schedule fires again.
    /// A routine armed on another Mac
    /// shows when its schedule next comes due there.
    pub next_run_at: Option<i64>,
    /// The latest run that started. Skipped and coalesced fires, runs still
    /// waiting and runs that ended before they started are only in [`runs`].
    pub last_run: Option<LastRun>,
    /// The caller's routine whose latest run that started and ended, ended
    /// `needs_you`.
    pub needs_you: bool,
    /// That `needs_you` run, which may be older than [`Self::last_run`] while
    /// a later run is still going.
    pub needs_you_run: Option<LastRun>,
    /// The routine's agent by name, archived or not, or why it does not
    /// resolve.
    pub agent_display: AgentDisplay,
    /// The agent it belongs to, when its reference reads as one.
    pub agent_key: Option<AgentKey>,
    /// Its agent is nowhere this Mac can see: listed under "Unassigned",
    /// never fired.
    pub unassigned: bool,
    /// Another routine of its space owns its id (a sync brought two): never
    /// fired; rename or remove one of the two folders.
    pub duplicate: bool,
    /// What a fire of it does in this build ([`effective_mode`]).
    pub effective_mode: RoutineMode,
    pub def: RoutineDef,
}

/// The last run of a routine, as a list row shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LastRun {
    pub run_id: String,
    pub phase: String,
    pub reason: Option<String>,
    /// Unix epoch milliseconds it ended, or started while still running.
    pub at: i64,
    pub result_text: Option<String>,
    /// CodeSense's explanation of why the run ended ([`RoutineRun::detail`]).
    pub detail: Option<String>,
    /// SW-AGR-5: when a wake run's task was typed into its live session, so
    /// the row reads Delivered while the woken agent still has the run open.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<i64>,
    /// SW-AGR-5: the machine of the session a wake run woke.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wake_machine: Option<String>,
    /// SW-AGR-5: the machine a remote fresh run (or a wake's launch) started
    /// its agent on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch_machine: Option<String>,
    /// SW-AGR-5: the session the run launched or woke, so the row's Details
    /// can show its tab.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

impl From<RoutineRun> for LastRun {
    fn from(run: RoutineRun) -> Self {
        Self {
            at: run.ended_at.or(run.started_at).unwrap_or(run.created_at),
            run_id: run.run_id,
            phase: run.phase,
            reason: run.reason,
            result_text: run.result_text,
            detail: run.detail,
            delivered_at: run.delivered_at,
            wake_machine: run.wake_machine,
            launch_machine: run.launch_machine,
            session_id: run.session_id,
        }
    }
}

/// Which page of a routine's runs to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunsPage {
    /// At most this many runs, capped at [`MAX_RUNS_PAGE`].
    pub limit: usize,
    /// Only runs older than this run: the previous page's `next_before`.
    pub before: Option<String>,
}

impl Default for RunsPage {
    fn default() -> Self {
        Self {
            limit: DEFAULT_RUNS_PAGE,
            before: None,
        }
    }
}

pub const DEFAULT_RUNS_PAGE: usize = 50;
pub const MAX_RUNS_PAGE: usize = 200;

/// One page of a routine's runs, newest first.
#[derive(Debug, Clone, Serialize)]
pub struct RunHistory {
    pub runs: Vec<RoutineRun>,
    /// Pass as [`RunsPage::before`] for the next page; `None` on the last.
    pub next_before: Option<String>,
}

/// One run with the routine it belongs to.
#[derive(Debug, Clone, Serialize)]
pub struct RunDetail {
    #[serde(flatten)]
    pub run: RoutineRun,
    pub routine_name: String,
}

/// A schedule in words with its upcoming fires.
#[derive(Debug, Clone, Serialize)]
pub struct SchedulePreview {
    pub cron: String,
    /// The zone it was evaluated in: the one given, else this machine's.
    pub tz: String,
    pub in_words: String,
    /// Unix epoch milliseconds.
    pub next_fires: Vec<i64>,
}

/// The most fires [`preview_schedule`] returns.
pub const MAX_PREVIEW: usize = 20;

/// The scheduler's standing as the Routines view reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SchedulerStatus {
    /// Occurrences skipped because the app was closed or asleep, since the
    /// last [`acknowledge_missed`].
    pub missed_count: u64,
}

/// What a running routine's agent may ask about its own run.
#[derive(Debug, Clone, Serialize)]
pub struct RunContext {
    pub run_id: String,
    pub routine_key: RoutineKey,
    pub routine_name: String,
    /// The revision the run is pinned to, and that revision's definition.
    pub revision: u32,
    pub routine: RoutineDef,
    pub source: String,
    /// Unix epoch milliseconds.
    pub scheduled_at: Option<i64>,
    pub fired_at: i64,
    pub variables: BTreeMap<String, String>,
    /// The event that fired the run, for an event trigger.
    pub trigger_payload: Option<serde_json::Value>,
    pub coalesced_payloads: Vec<serde_json::Value>,
    /// The result of the routine's last successful run before this one, the
    /// `{{last_run_summary}}` its task rendered with.
    pub last_run_summary: Option<String>,
    /// The run [`Self::last_run_summary`] came from.
    pub last_run_id: Option<String>,
    /// The agent this run launched, as `<key>@<revision>`.
    pub agent: Option<String>,
    /// The absolute path of the folder the routine's content lives in: its
    /// team's content, or for a personal routine the `content/` folder of the
    /// user's private space for its org (its run folder starts empty, so
    /// relative paths do not reach it).
    pub content_dir: Option<PathBuf>,
}

/// One routine in full, including every revision (oldest first).
#[derive(Debug, Clone, Serialize)]
pub struct RoutineDetail {
    pub key: RoutineKey,
    pub scope: RoutineScope,
    pub armed_here: bool,
    pub owner_display: String,
    pub enabled: bool,
    pub agent_display: AgentDisplay,
    pub agent_key: Option<AgentKey>,
    pub unassigned: bool,
    /// What a fire of it does in this build ([`effective_mode`]).
    pub effective_mode: RoutineMode,
    pub def: RoutineDef,
    pub revisions: Vec<RevisionRecord>,
    /// SW-AGR-8: where a machine arm of it stands, for the UI: the machine
    /// it is waiting on to confirm it stopped firing it, and runs a
    /// machine dropped before this Mac read them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_arm: Option<MachineArmView>,
}

/// SW-AGR-8: what the UI shows of a routine's machine arm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineArmView {
    /// The machine that fires it, if one does.
    pub armed_machine: Option<String>,
    /// The machine it moved off that has not confirmed it stopped firing
    /// it: nothing fires it elsewhere meanwhile.
    pub waiting_on: Option<String>,
    /// `waiting for <machine> to confirm it stopped firing it`.
    pub waiting: Option<String>,
    /// Since when (ms), as this Mac recorded the move.
    pub waiting_since_ms: Option<i64>,
    /// When the machine's run log last dropped runs before this Mac read
    /// them (ms), if it did.
    pub runs_lost_at_ms: Option<i64>,
}

/// The routine's machine arm view, when it has one.
fn machine_arm_view(
    store: &RoutineStateStore,
    key: &RoutineKey,
    def: &RoutineDef,
) -> Option<MachineArmView> {
    let local = super::machine_arm::handovers(store)
        .ok()
        .and_then(|rows| rows.into_iter().find(|h| h.routine_key == key.to_string()));
    let waiting_on = def
        .handover_from
        .as_ref()
        .map(|from| from.machine.clone())
        .or_else(|| local.as_ref().map(|h| h.machine.clone()));
    let armed_machine = def.armed_machine().map(str::to_string);
    if armed_machine.is_none() && waiting_on.is_none() {
        return None;
    }
    let runs_lost_at_ms = armed_machine.as_deref().and_then(|machine| {
        super::machine_arm::runs_lost_at(store, machine)
            .ok()
            .flatten()
    });
    Some(MachineArmView {
        waiting: waiting_on
            .as_ref()
            .map(|machine| format!("waiting for {machine} to confirm it stopped firing it")),
        waiting_since_ms: local.map(|h| h.since_ms),
        waiting_on,
        armed_machine,
        runs_lost_at_ms,
    })
}

/// The result of a write.
#[derive(Debug, Clone, Serialize)]
pub struct SavedRoutine {
    pub key: RoutineKey,
    pub def: RoutineDef,
    /// What a run of the saved routine would be warned about (its task's
    /// leading `/command` missing from its agent, among others). Shown, never
    /// a refusal.
    pub warnings: Vec<LaunchWarning>,
    /// Where it lives now.
    #[serde(skip)]
    pub dir: PathBuf,
}

/// A resolved routine location.
struct Located {
    key: RoutineKey,
    org_root: PathBuf,
    /// The `config/` dir of the routine's space.
    config_dir: PathBuf,
    /// Where it is (under its agent, or a not yet migrated legacy dir), or,
    /// for a routine that does not exist, where its legacy dir would be.
    dir: PathBuf,
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Team names and routine ids become path segments: lowercase letters, digits,
/// `-` and `_` only.
fn check_segment(what: &str, value: &str) -> Result<(), RoutineError> {
    if is_valid_id(value) {
        Ok(())
    } else {
        Err(RoutineError::InvalidLocation(format!(
            "{what} {value:?} is not a valid name"
        )))
    }
}

fn check_org(org: &str) -> Result<(), RoutineError> {
    // SW-CR-1: parallel to `agents::service::check_org` — these two guards are
    // the ACTIVE rejection path for the two reserved owners, matching both the
    // bare form (`builtin`) and the two-segment form (`builtin/<x>`) so a
    // routine key never aliases onto the starter or personal namespace.
    // `validate_org_slug` also rejects the bare reservations, but as
    // defense-in-depth: the first-segment match here fires first and gives a
    // service-shaped error.
    // SW-CR-5: the first-segment match is case-insensitive (ASCII) so a
    // user-authored `org.yml` carrying slug `"Builtin/..."` can't alias on a
    // case-folding filesystem.
    let first = org.split('/').next().unwrap_or_default();
    if first.eq_ignore_ascii_case(PERSONAL_OWNER) {
        return Err(RoutineError::InvalidLocation(format!(
            "org {org:?} is reserved for private spaces"
        )));
    }
    if first.eq_ignore_ascii_case(BUILTIN_ORG) {
        return Err(RoutineError::InvalidLocation(format!(
            "org {org:?} is reserved for starter definitions"
        )));
    }
    match validate_org_slug(org) {
        None => Ok(()),
        Some(why) => Err(RoutineError::InvalidLocation(format!(
            "org {org:?} is not valid: {why}"
        ))),
    }
}

/// The `config/` dir of `org/team` as `principal` sees it: the org's scratch
/// space for the scratch team (whether or not it exists yet), else the team's.
pub(crate) fn config_dir_of(
    codesense_home: &Path,
    principal: &Principal,
    org: &str,
    org_root: &Path,
    team: &str,
) -> Result<PathBuf, RoutineError> {
    if is_scratch_team(team) {
        return scratch_space::org_space_at(codesense_home, principal, org)
            .map(|space| space.config_dir())
            .map_err(|e| RoutineError::InvalidLocation(e.to_string()));
    }
    Ok(path_to_team_config_dir(org_root, team))
}

/// The `config/` dir of the space `key` lives in (its team's, or its org's
/// scratch space for a personal key).
pub(crate) fn key_config_dir(
    codesense_home: &Path,
    principal: &Principal,
    key: &RoutineKey,
) -> Result<PathBuf, RoutineError> {
    space_config_dir(codesense_home, principal, &key.org, &key.team)
}

/// The `config/` dir of `org/team` (the org's scratch space for the scratch team).
pub(crate) fn space_config_dir(
    codesense_home: &Path,
    principal: &Principal,
    org: &str,
    team: &str,
) -> Result<PathBuf, RoutineError> {
    let org_root = path_to_org_root(codesense_home, principal, org).map_err(RoutineError::Io)?;
    config_dir_of(codesense_home, principal, org, &org_root, team)
}

/// The `content/` dir (the team workspace) of the space `key` lives in: its
/// team's, or its org's scratch space for a personal key.
pub(crate) fn key_content_dir(
    codesense_home: &Path,
    principal: &Principal,
    key: &RoutineKey,
) -> Result<PathBuf, RoutineError> {
    if is_scratch_team(&key.team) {
        return scratch_space::org_space_at(codesense_home, principal, &key.org)
            .map(|space| space.content_dir())
            .map_err(|e| RoutineError::InvalidLocation(e.to_string()));
    }
    let org_root =
        path_to_org_root(codesense_home, principal, &key.org).map_err(RoutineError::Io)?;
    Ok(crate::context_repo::path_to_team_content_dir(
        &org_root, &key.team,
    ))
}

/// Map a key to its directory, rejecting anything that could escape the orgs
/// tree. Another account's personal routine is not found: its path would be
/// the caller's own scratch space.
fn locate(
    codesense_home: &Path,
    principal: &Principal,
    key: &RoutineKey,
) -> Result<Located, RoutineError> {
    if !key.resolves_for(principal) {
        return Err(RoutineError::NotFound(key.clone()));
    }
    check_org(&key.org)?;
    if !is_scratch_team(&key.team) {
        check_segment("team", &key.team)?;
    }
    check_segment("routine id", &key.id)?;
    let org_root =
        path_to_org_root(codesense_home, principal, &key.org).map_err(RoutineError::Io)?;
    let config_dir = config_dir_of(codesense_home, principal, &key.org, &org_root, &key.team)?;
    // A space an older client still writes legacy routines into is brought
    // forward before any read or write of one of them.
    migrate::migrate_space(codesense_home, principal, &key.org, &key.team, &config_dir);
    let dir = find_dir(&config_dir, &key.id)
        .unwrap_or_else(|| layout::legacy_routine_dir(&config_dir, &key.id));
    Ok(Located {
        key: key.clone(),
        org_root,
        config_dir,
        dir,
    })
}

/// The dir routine `id` lives in within the space at `config_dir`: under its
/// agent, else a legacy dir that is not a tombstone (a read-only docs
/// context's, which is never migrated).
pub(crate) fn find_dir(config_dir: &Path, id: &str) -> Option<PathBuf> {
    if let Some(dir) = layout::find_in_agents(config_dir, id) {
        return Some(dir);
    }
    let legacy = layout::legacy_routine_dir(config_dir, id);
    match defs::read_def(&legacy) {
        Ok(Some(def)) if !def.extra.contains_key(migrate::MOVED_TO) => Some(legacy),
        _ => None,
    }
}

/// The dir routine `key` lives in, as the caller resolves it: what a run
/// reads its pinned revision from.
pub(crate) fn routine_dir_of(
    codesense_home: &Path,
    principal: &Principal,
    key: &RoutineKey,
) -> Result<PathBuf, RoutineError> {
    Ok(locate(codesense_home, principal, key)?.dir)
}

/// Whether this build delivers `wake` fires into a live session (SW-AGR-2).
/// Every routine saved before it shipped has `fresh` written explicitly, so
/// turning it on flips no stored routine to `wake`.
pub const WAKE_DELIVERY: bool = true;

/// The mode a routine that names none would run in once wake ships: `wake`
/// for a visible routine on a saved agent that launches in the foreground
/// (its people work with a live session), else `fresh`. A headless routine
/// (`visible: false`), a starter's (§9.1) and one whose agent cannot be read
/// are fresh.
pub fn default_mode(
    codesense_home: &Path,
    principal: &Principal,
    routine: &RoutineKey,
    def: &RoutineDef,
) -> RoutineMode {
    if !def.policy.visible {
        return RoutineMode::Fresh;
    }
    let Ok(key) = def.agent.key_for(routine) else {
        return RoutineMode::Fresh;
    };
    if key.is_builtin() {
        return RoutineMode::Fresh;
    }
    match crate::agents::service::read_current(codesense_home, principal, &key) {
        // An agent that runs without permission checks can be woken only
        // when it opted in (§9.5): without `allowWake` its routines start fresh.
        Ok(agent)
            if agent.launch == crate::agents::model::LaunchMode::Foreground
                && crate::agents::adapter::desktop(agent.runtime).supports_wake_delivery
                && (!agent.is_escalating() || agent.allow_wake) =>
        {
            RoutineMode::Wake
        }
        _ => RoutineMode::Fresh,
    }
}

/// What a fire of the routine does: `fresh` while [`WAKE_DELIVERY`] is off,
/// whatever is stored, and always for a starter's routine; else the stored
/// mode. A routine that names none (written before modes, or by hand) runs
/// fresh as it always did: only a create writes its agent's
/// [`default_mode`].
pub fn effective_mode(
    codesense_home: &Path,
    principal: &Principal,
    routine: &RoutineKey,
    def: &RoutineDef,
) -> RoutineMode {
    if !WAKE_DELIVERY {
        return RoutineMode::Fresh;
    }
    // A starter's routines are fresh only, whatever a hand edit wrote: its
    // sessions are the ones people open by hand (§9.1).
    if def
        .agent
        .key_for(routine)
        .map_or(true, |agent| agent.is_builtin())
    {
        return RoutineMode::Fresh;
    }
    let mode = def.mode().unwrap_or(RoutineMode::Fresh);
    if mode == RoutineMode::Wake
        && !agent_supports_wake_delivery(codesense_home, principal, routine, def)
    {
        RoutineMode::Fresh
    } else {
        mode
    }
}

fn agent_supports_wake_delivery(
    codesense_home: &Path,
    principal: &Principal,
    routine: &RoutineKey,
    def: &RoutineDef,
) -> bool {
    let Ok(key) = def.agent.key_for(routine) else {
        return false;
    };
    if key.is_builtin() {
        return false;
    }
    crate::agents::service::read_current(codesense_home, principal, &key)
        .is_ok_and(|agent| crate::agents::adapter::desktop(agent.runtime).supports_wake_delivery)
}

/// The mode an edit writes: what the author picked, else `fresh`
/// explicitly, so a routine stored before modes (which ran fresh) never flips
/// to `wake` by being edited. A create writes its agent's default instead.
fn written_mode(picked: &str) -> String {
    match picked.trim() {
        "" => RoutineMode::Fresh.as_str().to_string(),
        picked => picked.to_string(),
    }
}

/// Refuse storing `wake` (newly, on `current` → `next`) while this build
/// cannot deliver it.
fn check_wake(
    codesense_home: &Path,
    principal: &Principal,
    routine: &RoutineKey,
    current: Option<&RoutineDef>,
    next: &RoutineDef,
) -> Result<(), RoutineError> {
    let wake = |def: &RoutineDef| def.mode().is_ok_and(|m| m == RoutineMode::Wake);
    let has_saved_agent = next
        .agent
        .key_for(routine)
        .is_ok_and(|agent| !agent.is_builtin());
    if wake(next)
        && ((has_saved_agent
            && !agent_supports_wake_delivery(codesense_home, principal, routine, next))
            || (!WAKE_DELIVERY && !current.is_some_and(wake)))
    {
        return Err(ValidationError::WakeNotAvailable.into());
    }
    Ok(())
}

/// Resolve a placement to `(org, team, config_dir)`, creating the org's
/// scratch space if this is its first personal save.
fn resolve_placement(
    codesense_home: &Path,
    principal: &Principal,
    placement: &Placement,
) -> Result<(String, String, PathBuf), RoutineError> {
    match placement {
        Placement::Personal(owner) => {
            if Principal::from(owner.github_user_id) != *principal {
                return Err(RoutineError::InvalidLocation(
                    "an Only me routine can only be created in your own private space".into(),
                ));
            }
            check_org(&owner.org)?;
            let (space, _created) =
                scratch_space::ensure_org_space(codesense_home, principal, &owner.org)
                    .map_err(|e| RoutineError::InvalidLocation(e.to_string()))?;
            Ok((
                owner.org.clone(),
                SCRATCH_TEAM.to_string(),
                space.config_dir(),
            ))
        }
        Placement::Team { org, team } => {
            check_org(org)?;
            check_segment("team", team)?;
            // Only a team discovery scans may hold a routine, or it would be
            // written where the list, the cap and arming never look.
            let synced = match principal.as_str().parse::<u64>() {
                Ok(user_id) => {
                    crate::commands::route_to_context::synced_contexts_in(codesense_home, user_id)
                        .map_err(RoutineError::Io)?
                        .iter()
                        .any(|c| {
                            c.org_slug == *org && c.team_name.as_deref() == Some(team.as_str())
                        })
                }
                Err(_) => false,
            };
            if !synced {
                return Err(RoutineError::InvalidLocation(format!(
                    "team {org}/{team} is not synced on this machine"
                )));
            }
            let org_root =
                path_to_org_root(codesense_home, principal, org).map_err(RoutineError::Io)?;
            Ok((
                org.clone(),
                team.clone(),
                path_to_team_config_dir(&org_root, team),
            ))
        }
    }
}

fn read_existing(loc: &Located) -> Result<RoutineDef, RoutineError> {
    defs::read_def(&loc.dir)?.ok_or_else(|| RoutineError::NotFound(loc.key.clone()))
}

/// Refuse a change that would leave the caller seeing more than
/// [`MAX_ACTIVE_ROUTINES`] enabled routines armed on this machine. `def` and `enabled`
/// describe the routine as it would be after the change; it counts only if it
/// would itself be active.
fn ensure_under_cap(
    codesense_home: &Path,
    principal: &Principal,
    host: &str,
    store: &RoutineStateStore,
    key: &RoutineKey,
    def: &RoutineDef,
    enabled: bool,
) -> Result<(), RoutineError> {
    // §9.9: a routine this Mac arms on a machine counts too.
    let counts =
        |def: &RoutineDef| is_armed_here(def, host) || super::host::manages_machine_arm(def, host);
    if !enabled || !counts(def) {
        return Ok(());
    }
    let mut others = 0usize;
    for r in discover_routines(codesense_home, principal)? {
        if counts(&r.def) && r.key != *key && store.is_enabled(&r.key, &r.def, host)? {
            others += 1;
        }
    }
    if others + 1 > MAX_ACTIVE_ROUTINES {
        return Err(RoutineError::LimitReached {
            limit: MAX_ACTIVE_ROUTINES,
        });
    }
    Ok(())
}

/// Serializes every cap check with the write that follows it, so two
/// concurrent saves or resumes cannot both pass a count of `MAX - 1`. Taken
/// after a routine lock, never before one.
static CAP_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn cap_guard() -> std::sync::MutexGuard<'static, ()> {
    CAP_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Validate, cap-check and write `def` as its next revision. Callers hold the
/// routine lock and have already set `def.revision`.
fn commit(
    codesense_home: &Path,
    principal: &Principal,
    host: &str,
    store: &RoutineStateStore,
    loc: &Located,
    def: RoutineDef,
    author: &Author,
) -> Result<SavedRoutine, RoutineError> {
    // Restored and re-armed definitions use the same trigger spelling as new saves.
    let def = normalize_triggers(def);
    def.validate()?;
    // A requested removal keeps the accepted cap's mode/arming restrictions until accepted.
    if def.policy.budget_usd.is_none() {
        let caps = crate::session_usage::spend_caps::status(
            codesense_home,
            "routine",
            &loc.key.to_string(),
            None,
        )
        .map_err(RoutineError::Io)?;
        crate::session_usage::spend_caps::validate_routine_cap(&def, caps.effective)?;
    }
    def.validate_agents_for(&loc.key)?;
    super::command::validate_definition(codesense_home, principal, &loc.key, &def)
        .map_err(ValidationError::Command)?;
    let _cap = cap_guard();
    super::chain::validate(codesense_home, principal, &loc.key, &def)?;
    let enabled = store.is_enabled(&loc.key, &def, host)?;
    ensure_under_cap(
        codesense_home,
        principal,
        host,
        store,
        &loc.key,
        &def,
        enabled,
    )?;
    let loc = rehome(loc, &def)?;
    defs::write_revision(&loc.dir, &def, author, &now_rfc3339())?;
    // This Mac now has the routine in its new mode, so a switch flipped here
    // under the other one is settled at once, as the next wake would. The
    // revision is already written, so a failure here only leaves that to the
    // next wake (the switch reads by its mode meanwhile) and never fails the
    // save.
    if let Err(e) = store.settle_switches(&[(loc.key.to_string(), &def)], host) {
        log::warn!(target: "routines", "routine {}: switch not settled: {e}", loc.key);
    }
    crate::session_usage::spend_caps::status(
        codesense_home,
        "routine",
        &loc.key.to_string(),
        def.policy.budget_usd,
    )
    .map_err(RoutineError::Io)?;
    crate::session_usage::spend_caps::changed();
    invalidate_discovery_cache(codesense_home);
    Ok(SavedRoutine {
        dir: loc.dir.clone(),
        key: loc.key.clone(),
        def,
        warnings: Vec::new(),
    })
}

/// Announce a definition write once every lock is released, so a listener
/// that reads or writes routines in response never runs inside (or blocks
/// on) the routine and cap locks.
fn announced(saved: Result<SavedRoutine, RoutineError>) -> Result<SavedRoutine, RoutineError> {
    if let Ok(saved) = &saved {
        // An older client still fires by the legacy tombstone: keep where it
        // runs, whether it is archived and which agent it runs in step.
        migrate::mirror_to_tombstone(&saved.dir);
        events::routine_changed(&saved.key.to_string());
    }
    saved
}

/// [`announced`], with what a run of the written routine would be warned
/// about. Worked out once every lock is released: it resolves the agent and
/// reads its startup content.
fn announced_with_warnings(
    codesense_home: &Path,
    principal: &Principal,
    saved: Result<SavedRoutine, RoutineError>,
) -> Result<SavedRoutine, RoutineError> {
    announced_with_warnings_for(codesense_home, principal, &Default::default(), saved)
}

fn announced_with_warnings_for(
    codesense_home: &Path,
    principal: &Principal,
    caller_scope: &crate::context_repo::caller_scope::CallerScope,
    saved: Result<SavedRoutine, RoutineError>,
) -> Result<SavedRoutine, RoutineError> {
    announced(saved).map(|mut saved| {
        saved.warnings = agents::save_warnings_for(
            codesense_home,
            principal,
            &saved.key,
            &saved.def,
            caller_scope,
        );
        saved
    })
}

/// Pin missing schedule zones and store the canonical name of each role queue.
fn normalize_triggers(mut routine: RoutineDef) -> RoutineDef {
    for trigger in &mut routine.triggers {
        match trigger {
            RoutineTrigger::Schedule(spec) if spec.tz.trim().is_empty() => {
                spec.tz = schedule::system_zone();
            }
            RoutineTrigger::Event(spec) => {
                if let Some(role) = &mut spec.role {
                    *role = crate::weft::role_inbox::role_name(role).to_string();
                }
            }
            _ => {}
        }
    }
    routine
}

/// Every non-archived routine visible to the caller, across all of their
/// teams and their personal space: local enabled state, who created it, its
/// schedule in words, its next fire and its last run.
pub fn list_summaries(
    codesense_home: &Path,
    principal: &Principal,
) -> Result<Vec<RoutineSummary>, RoutineError> {
    list_summaries_at(codesense_home, principal, Utc::now())
}

pub(crate) fn list_summaries_at(
    codesense_home: &Path,
    principal: &Principal,
    now: DateTime<Utc>,
) -> Result<Vec<RoutineSummary>, RoutineError> {
    list_summaries_for(codesense_home, principal, now, &Default::default())
}

pub(crate) fn list_summaries_for(
    codesense_home: &Path,
    principal: &Principal,
    now: DateTime<Utc>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<Vec<RoutineSummary>, RoutineError> {
    super::discover::migrate_all_for(codesense_home, principal, scope)?;
    let active = scratch_space::active_org(codesense_home);
    let routines = super::discover::discover_routines_for(codesense_home, principal, false, scope)?
        .into_iter()
        .filter(|r| !r.key.is_personal() || active.as_deref() == Some(r.key.org.as_str()))
        .collect();
    summarize(codesense_home, principal, routines, now, scope)
}

/// Every non-archived routine of `agent` the caller can see, in every space
/// (not only the org on screen), summarized as [`list_summaries`] does: what
/// `agent_get` lists as the agent's routines.
pub fn summaries_for_agent(
    codesense_home: &Path,
    principal: &Principal,
    agent: &AgentKey,
    now: DateTime<Utc>,
) -> Result<Vec<RoutineSummary>, RoutineError> {
    summaries_for_agent_scoped(codesense_home, principal, agent, now, &Default::default())
}

pub(crate) fn summaries_for_agent_scoped(
    codesense_home: &Path,
    principal: &Principal,
    agent: &AgentKey,
    now: DateTime<Utc>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<Vec<RoutineSummary>, RoutineError> {
    super::discover::migrate_all_for(codesense_home, principal, scope)?;
    let routines = super::discover::discover_routines_for(codesense_home, principal, false, scope)?
        .into_iter()
        .filter(|r| r.agent_key.as_ref() == Some(agent))
        .collect::<Vec<_>>();
    if routines.is_empty() {
        // Nothing to summarize: a read never creates the run store.
        return Ok(Vec::new());
    }
    summarize(codesense_home, principal, routines, now, scope)
}

fn summarize(
    codesense_home: &Path,
    principal: &Principal,
    routines: Vec<super::discover::DiscoveredRoutine>,
    now: DateTime<Utc>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<Vec<RoutineSummary>, RoutineError> {
    let host = local_host_id(codesense_home).map_err(|e| RoutineError::Io(e.to_string()))?;
    let store = RoutineStateStore::open(codesense_home)?;
    let switches = store.switches()?;
    let mut last_runs = store.last_runs()?;
    let mut needing = store.needing_you()?;
    let cursors: HashMap<String, TriggerRow> = store
        .triggers()?
        .into_iter()
        .map(|row| (row.trigger_id.clone(), row))
        .collect();
    let mut agent_cache = agents::DisplayCache::new();
    Ok(routines
        .into_iter()
        .map(|r| {
            let rendered = r.key.to_string();
            let created_by = defs::read_revision(&r.dir, 1)
                .ok()
                .flatten()
                .map(|rec| rec.author);
            let enabled = enabled_here(switches.get(&rendered).copied(), &r.def, &host);
            // Only runs this Mac made are in its store, so a question here is
            // for this Mac, wherever the routine is armed now.
            let needs_you_run = needing.remove(&rendered).map(LastRun::from);
            let next_run_at = if r.held() {
                // Nothing fires a routine with no agent here.
                None
            } else if !is_armed_anywhere(&r.def) {
                // It names no Mac, so it fires nowhere until one is picked.
                None
            } else if !r.armed_here {
                // A shared job that fires on another Mac, whose pause state
                // this machine cannot see.
                next_fire(&r.def, now)
            } else if !enabled {
                None
            } else {
                next_due(&rendered, &r.def, &cursors, now)
            };
            RoutineSummary {
                action: r.def.action,
                command: r.def.command.clone(),
                agent_display: agents::display_cached_for(
                    codesense_home,
                    principal,
                    &r.key,
                    &r.def,
                    &mut agent_cache,
                    scope,
                ),
                schedule_in_words: schedule_in_words_in(
                    codesense_home,
                    principal,
                    &r.key,
                    &r.def,
                    now,
                ),
                next_run_at,
                last_run: last_runs.remove(&rendered).map(LastRun::from),
                needs_you: needs_you_run.is_some(),
                needs_you_run,
                enabled,
                effective_mode: effective_mode(codesense_home, principal, &r.key, &r.def),
                agent_key: r.agent_key,
                unassigned: r.unassigned,
                duplicate: r.duplicate,
                key: r.key,
                scope: r.scope,
                armed_here: r.armed_here,
                owner_display: r.owner_display,
                created_by,
                def: r.def,
            }
        })
        .collect())
}

/// Each trigger in words, joined; "manual only" without triggers.
pub(crate) fn schedule_in_words(def: &RoutineDef, now: DateTime<Utc>) -> String {
    if def.triggers.is_empty() && def.after.is_none() {
        return "manual only".to_string();
    }
    let mut words = def
        .triggers
        .iter()
        .map(|trigger| match trigger {
            RoutineTrigger::Schedule(spec) => schedule::describe_at(&spec.cron, &spec.tz, now),
            RoutineTrigger::Event(spec) if spec.kind.trim() == EVENT_INBOX_IDLE => {
                "when an agent sits idle with unread mail".to_string()
            }
            RoutineTrigger::Event(spec) if spec.kind.trim() == EVENT_ROLE_PENDING => {
                "when a message is queued for a role".to_string()
            }
            RoutineTrigger::Event(spec)
                if spec.kind.trim() == super::model::EVENT_METADATA_CHANGE =>
            {
                spec.metadata
                    .as_ref()
                    .map(super::metadata::words)
                    .unwrap_or_default()
            }
            RoutineTrigger::Event(spec) => match &spec.from_agent {
                Some(agent) => format!("when {agent} finishes"),
                None => "when an agent finishes".to_string(),
            },
        })
        .collect::<Vec<_>>()
        .join("; ");
    if let Some(after) = &def.after {
        if !words.is_empty() {
            words.push_str("; ");
        }
        words.push_str(&format!(
            "after {} (gone) {}",
            after.routine,
            if after.on == super::model::AfterOn::Succeeded {
                "succeeds"
            } else {
                "finishes"
            }
        ));
    }
    words
}

pub(crate) fn schedule_in_words_in(
    home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    def: &RoutineDef,
    now: DateTime<Utc>,
) -> String {
    let words = schedule_in_words(def, now);
    let Some(after) = &def.after else {
        return words;
    };
    let upstream = RoutineKey {
        id: after.routine.clone(),
        ..key.clone()
    };
    let name = routine_dir_of(home, principal, &upstream)
        .ok()
        .and_then(|dir| defs::read_def(&dir).ok().flatten())
        .map(|d| d.name);
    match name {
        Some(name) => words.replace(
            &format!("after {} (gone)", after.routine),
            &format!("after {name}"),
        ),
        None => words,
    }
}

/// The earliest fire of any of the routine's schedules after `now`.
fn next_fire(def: &RoutineDef, now: DateTime<Utc>) -> Option<i64> {
    def.triggers
        .iter()
        .filter_map(|trigger| match trigger {
            RoutineTrigger::Schedule(spec) => schedule::next_after(&spec.cron, &spec.tz, now),
            RoutineTrigger::Event(_) => None,
        })
        .min()
        .map(|at| at.timestamp_millis())
}

/// When a routine armed here next fires: each schedule's scheduler cursor
/// (none while parked) while the cursor was computed from that schedule, else
/// the schedule's own next fire, as for a schedule not yet seeded or edited
/// since the scheduler's last wake.
fn next_due(
    rendered: &str,
    def: &RoutineDef,
    cursors: &HashMap<String, TriggerRow>,
    now: DateTime<Utc>,
) -> Option<i64> {
    def.triggers
        .iter()
        .enumerate()
        .filter_map(|(index, trigger)| {
            let RoutineTrigger::Schedule(spec) = trigger else {
                return None;
            };
            match cursors.get(&format!("{rendered}#{index}")) {
                Some(row) if row.spec_hash == tick::spec_hash(&spec.cron, &spec.tz) => {
                    row.next_run_at
                }
                _ => {
                    schedule::next_after(&spec.cron, &spec.tz, now).map(|at| at.timestamp_millis())
                }
            }
        })
        .min()
}

/// The rendered keys of every routine the caller can see.
fn visible_keys(
    codesense_home: &Path,
    principal: &Principal,
) -> Result<HashSet<String>, RoutineError> {
    Ok(discover_routines(codesense_home, principal)?
        .into_iter()
        .map(|r| r.key.to_string())
        .collect())
}

/// `key`'s definition, archived or not, as the caller can read it: runs are
/// only read through a routine the caller can see.

pub(crate) fn readable_def_for(
    codesense_home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<RoutineDef, RoutineError> {
    readable_location_for(codesense_home, principal, key, scope).map(|(_, def)| def)
}

fn readable_location_for(
    codesense_home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<(Located, RoutineDef), RoutineError> {
    if !matches!(scope, crate::context_repo::caller_scope::CallerScope::Human) {
        let config = key_config_dir(codesense_home, principal, key)?;
        if !scope.allows(&config) {
            return Err(RoutineError::NotFound(key.clone()));
        }
    }
    let loc = locate(codesense_home, principal, key)?;
    if !scope.allows(&loc.dir.join("routine.yml")) {
        return Err(RoutineError::NotFound(key.clone()));
    }
    let def = read_existing(&loc)?;
    Ok((loc, def))
}

/// A page of `key`'s runs, newest first, including the fires that did not run
/// (`skipped` with their `reason`, `coalesced` with their `linked_run_id`).
/// Runs outlive their routine, so an archived routine's history still reads.
pub fn runs(
    codesense_home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    page: RunsPage,
) -> Result<RunHistory, RoutineError> {
    runs_for(codesense_home, principal, key, page, &Default::default())
}

pub(crate) fn runs_for(
    codesense_home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    page: RunsPage,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<RunHistory, RoutineError> {
    readable_def_for(codesense_home, principal, key, scope)?;
    let limit = page.limit.clamp(1, MAX_RUNS_PAGE);
    let store = RoutineStateStore::open(codesense_home)?;
    let Some(mut runs) = store.runs_newest_first(key, page.before.as_deref(), limit + 1)? else {
        return Err(RoutineError::RunNotFound(page.before.unwrap_or_default()));
    };
    let next_before = if runs.len() > limit {
        runs.truncate(limit);
        runs.last().map(|r| r.run_id.clone())
    } else {
        None
    };
    Ok(RunHistory { runs, next_before })
}

/// The runs since `since_ms` of every routine the caller can read (paused ones
/// too), newest first, at most `per_routine` of each routine's runs and as many
/// of its merged and skipped fires, counted apart.
pub fn recent_runs(
    codesense_home: &Path,
    principal: &Principal,
    since_ms: i64,
    per_routine: usize,
) -> Result<Vec<RoutineRun>, RoutineError> {
    let keys: Vec<String> = list_summaries(codesense_home, principal)?
        .into_iter()
        .map(|s| s.key.to_string())
        .collect();
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let store = RoutineStateStore::open(codesense_home)?;
    Ok(store.runs_since(&keys, since_ms, per_routine)?)
}

/// One run and its routine's definition. A run of a routine the caller cannot
/// read is not found.
fn readable_run(
    codesense_home: &Path,
    principal: &Principal,
    store: &RoutineStateStore,
    run_id: &str,
) -> Result<(RoutineRun, RoutineDef), RoutineError> {
    readable_run_for(
        codesense_home,
        principal,
        store,
        run_id,
        &Default::default(),
    )
}

fn readable_run_for(
    codesense_home: &Path,
    principal: &Principal,
    store: &RoutineStateStore,
    run_id: &str,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<(RoutineRun, RoutineDef), RoutineError> {
    let not_found = || RoutineError::RunNotFound(run_id.to_string());
    let run = store.run(run_id)?.ok_or_else(not_found)?;
    let key = RoutineKey::parse(&run.routine_key).ok_or_else(not_found)?;
    match readable_def_for(codesense_home, principal, &key, scope) {
        Ok(def) => Ok((run, def)),
        Err(RoutineError::NotFound(_) | RoutineError::InvalidLocation(_)) => Err(not_found()),
        Err(e) => Err(e),
    }
}

/// One run row with its routine's name. A run of a routine the caller cannot
/// read is not found.
pub fn run_detail(
    codesense_home: &Path,
    principal: &Principal,
    run_id: &str,
) -> Result<RunDetail, RoutineError> {
    run_detail_for(codesense_home, principal, run_id, &Default::default())
}

pub(crate) fn run_detail_for(
    codesense_home: &Path,
    principal: &Principal,
    run_id: &str,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<RunDetail, RoutineError> {
    let store = RoutineStateStore::open(codesense_home)?;
    let (run, def) = readable_run_for(codesense_home, principal, &store, run_id, scope)?;
    Ok(RunDetail {
        run,
        routine_name: def.name,
    })
}

/// How many visible routines need an answer on this Mac: the latest of their
/// runs here that started and ended, ended `needs_you`.
pub fn needs_you_count(
    codesense_home: &Path,
    principal: &Principal,
) -> Result<usize, RoutineError> {
    let needing = RoutineStateStore::open(codesense_home)?.needing_you()?;
    if needing.is_empty() {
        return Ok(0);
    }
    let visible = visible_keys(codesense_home, principal)?;
    Ok(needing.keys().filter(|key| visible.contains(*key)).count())
}

/// `cron` in words and its next `n` fires (at most [`MAX_PREVIEW`]), checked
/// as a save would check it. An empty `tz` is this machine's zone.
pub fn preview_schedule(cron: &str, tz: &str, n: usize) -> Result<SchedulePreview, RoutineError> {
    preview_schedule_at(cron, tz, n, Utc::now())
}

pub(crate) fn preview_schedule_at(
    cron: &str,
    tz: &str,
    n: usize,
    now: DateTime<Utc>,
) -> Result<SchedulePreview, RoutineError> {
    let tz = match tz.trim() {
        "" => schedule::system_zone(),
        tz => tz.to_string(),
    };
    schedule::validate_at(cron, &tz, now).map_err(ValidationError::InvalidSchedule)?;
    Ok(SchedulePreview {
        in_words: schedule::describe_at(cron, &tz, now),
        next_fires: schedule::next_n(cron, &tz, now, n.min(MAX_PREVIEW))
            .into_iter()
            .map(|at| at.timestamp_millis())
            .collect(),
        cron: cron.to_string(),
        tz,
    })
}

/// How many of the caller's routines' occurrences were missed while the app
/// was closed or asleep, since they were last acknowledged.
pub fn scheduler_status(
    codesense_home: &Path,
    principal: &Principal,
) -> Result<SchedulerStatus, RoutineError> {
    let keys = visible_keys(codesense_home, principal)?;
    Ok(SchedulerStatus {
        missed_count: RoutineStateStore::open(codesense_home)?.missed_count(&keys)?,
    })
}

/// Clear the missed count [`scheduler_status`] reports. The skipped runs that
/// recorded the misses stay in each routine's history.
pub fn acknowledge_missed(
    codesense_home: &Path,
    principal: &Principal,
) -> Result<(), RoutineError> {
    let keys = visible_keys(codesense_home, principal)?;
    for key in RoutineStateStore::open(codesense_home)?.acknowledge_missed(&keys)? {
        events::routine_changed(&key);
    }
    Ok(())
}

/// Everything the agent running a routine may ask about its run: the routine,
/// the revision the run is pinned to, how it was fired, the values its task
/// was rendered with, and the routine's last successful run before it. A session that
/// is not a routine run is a typed [`RoutineError::NotARoutineRun`].
///
/// The session is the run's own agent, so the routine is read as the account
/// the run was dispatched for, whoever is signed in now: a run keeps its
/// context across an account switch. A run whose dispatch request does not
/// name an account that can read its routine reads nothing.
pub fn context_for_session(
    codesense_home: &Path,
    session_id: &str,
) -> Result<RunContext, RoutineError> {
    context_for_session_run(codesense_home, session_id, None)
}

pub fn context_for_session_run(
    codesense_home: &Path,
    session_id: &str,
    run_id: Option<&str>,
) -> Result<RunContext, RoutineError> {
    let not_a_run = || RoutineError::NotARoutineRun(session_id.to_string());
    let store = RoutineStateStore::open(codesense_home)?;
    let run = match run_id {
        Some(id) => store
            .run(id)?
            .filter(|r| r.session_id.as_deref() == Some(session_id)),
        None => store.run_by_session(session_id)?,
    }
    .ok_or_else(not_a_run)?;
    let key = RoutineKey::parse(&run.routine_key).ok_or_else(not_a_run)?;
    let request = crate::coordinator::SqliteOrchestrationStore::open(
        crate::coordinator::default_store_path(codesense_home),
    )?
    .run_request_json(&run.run_id)?;
    let pointer = match request.as_deref().and_then(admit::parse_pointer) {
        Some(Ok(pointer)) if pointer.routine_key == run.routine_key => pointer,
        Some(Ok(_)) | None => return Err(not_a_run()),
        Some(Err(e)) => {
            return Err(RoutineError::Store(format!(
                "routine run {}: unreadable dispatch request: {e}",
                run.run_id
            )))
        }
    };
    let principal = Principal::from(pointer.principal.as_str());
    let loc = locate(codesense_home, &principal, &key)?;
    let Some(routine) = defs::read_pinned(&loc.dir, run.revision)? else {
        return Err(RoutineError::RevisionNotFound {
            key,
            revision: run.revision,
        });
    };
    // The summary the task rendered with, and the run it came from: a later
    // run's ending must not change what this run was told.
    let previous = store.last_success_by(&key, run.started_at.unwrap_or(run.created_at))?;
    // The declared values the task rendered with.
    let variables = dispatch::declared_values(&routine, &pointer.variables);
    let coalesced_payloads = store.coalesced_payloads(&run.run_id)?;
    Ok(RunContext {
        coalesced_payloads,
        run_id: run.run_id,
        routine_name: routine.name.clone(),
        routine_key: key.clone(),
        revision: run.revision,
        routine,
        source: run.source,
        scheduled_at: pointer.scheduled_at,
        fired_at: pointer.fired_at,
        variables,
        trigger_payload: pointer.trigger_payload,
        // As `finish` stores it: a success without a summary is told "".
        last_run_summary: previous
            .as_ref()
            .map(|p| p.result_text.clone().unwrap_or_default()),
        last_run_id: previous.map(|p| p.run_id),
        agent: run.agent,
        content_dir: key_content_dir(codesense_home, &principal, &key).ok(),
    })
}

/// One routine with its full revision history. Archived routines stay
/// readable here.
pub fn get(
    codesense_home: &Path,
    principal: &Principal,
    key: &RoutineKey,
) -> Result<RoutineDetail, RoutineError> {
    detail(codesense_home, principal, key, true)
}

/// [`get`] without the revision history (`revisions` is empty): what a
/// single-routine read or an edit needs, without reading every past revision.
pub fn get_current(
    codesense_home: &Path,
    principal: &Principal,
    key: &RoutineKey,
) -> Result<RoutineDetail, RoutineError> {
    detail(codesense_home, principal, key, false)
}

pub(crate) fn get_current_for(
    home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<RoutineDetail, RoutineError> {
    detail_for(home, principal, key, false, scope)
}

fn detail(
    codesense_home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    with_history: bool,
) -> Result<RoutineDetail, RoutineError> {
    detail_for(
        codesense_home,
        principal,
        key,
        with_history,
        &Default::default(),
    )
}

fn detail_for(
    codesense_home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    with_history: bool,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<RoutineDetail, RoutineError> {
    let (loc, def) = readable_location_for(codesense_home, principal, key, scope)?;
    crate::session_usage::spend_caps::status(
        codesense_home,
        "routine",
        &key.to_string(),
        def.policy.budget_usd,
    )
    .map_err(RoutineError::Io)?;
    let host = local_host_id(codesense_home).map_err(|e| RoutineError::Io(e.to_string()))?;
    let store = RoutineStateStore::open(codesense_home)?;
    let revisions = if with_history {
        defs::read_revisions(&loc.dir)?
    } else {
        Vec::new()
    };
    let agent_key = def.agent.key_for(key).ok();
    let unassigned = !def.is_command()
        && !agent_key.as_ref().is_some_and(|agent| {
            scope.definition(codesense_home, principal, agent)
                && agent_is_here(codesense_home, principal, agent)
        });
    Ok(RoutineDetail {
        effective_mode: effective_mode(codesense_home, principal, key, &def),
        agent_key,
        unassigned,
        key: loc.key.clone(),
        scope: RoutineScope::of_team(&key.team),
        armed_here: is_armed_here(&def, &host),
        owner_display: OwnerNames::default().display(&key.org, &loc.org_root, &def.owner),
        enabled: store.is_enabled(key, &def, &host)?,
        agent_display: agents::display(codesense_home, principal, &loc.key, &def),
        revisions,
        machine_arm: machine_arm_view(&store, key, &def),
        def,
    })
}

/// Create or update a routine.
///
/// - **Create** writes revision 1 created by the caller (`owner`, whatever
///   `routine.owner` says). A shared job is armed on this host; a recipe each
///   Mac runs names no Mac, so it starts off everywhere.
/// - **Update** replaces the body of any routine the caller can see. The
///   owner, `armed_host` and `archived_at` are carried over from the current
///   revision; [`arm_here`] and [`archive`] are the only ways to change them,
///   except where [`placed`] says otherwise (a change of `runs_on`, or a
///   hand-written shared job naming no Mac).
///
/// `runs_on: each_mac` is refused for a personal routine.
pub fn save(
    codesense_home: &Path,
    principal: &Principal,
    target: SaveTarget,
    routine: RoutineDef,
    author: &Author,
) -> Result<SavedRoutine, RoutineError> {
    save_for(
        codesense_home,
        principal,
        target,
        routine,
        author,
        &Default::default(),
    )
}

pub(crate) fn save_for(
    codesense_home: &Path,
    principal: &Principal,
    target: SaveTarget,
    routine: RoutineDef,
    author: &Author,
    caller_scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<SavedRoutine, RoutineError> {
    let host = local_host_id(codesense_home).map_err(|e| RoutineError::Io(e.to_string()))?;
    let store = RoutineStateStore::open(codesense_home)?;
    let routine = normalize_triggers(routine);
    match target {
        SaveTarget::Create(placement) => {
            if !is_valid_id(&routine.id) {
                return Err(ValidationError::InvalidId(routine.id).into());
            }
            let scope = match placement {
                Placement::Personal(_) => RoutineScope::Personal,
                Placement::Team { .. } => RoutineScope::Team,
            };
            let runs_on = scoped_runs_on(scope, &routine)?;
            let mode = routine.runs_on()?;
            let def = RoutineDef {
                runs_on,
                owner: principal.to_string(),
                armed_host: match mode {
                    RunsOn::OneMac => host.clone(),
                    RunsOn::EachMac => String::new(),
                },
                revision: 1,
                archived_at: None,
                ..routine
            };
            // Before the placement: a first personal save creates the personal
            // space, which an invalid body must not leave behind.
            def.validate()?;
            let (org, team, _) = resolve_placement(codesense_home, principal, &placement)?;
            let key = RoutineKey::scoped(principal, org, team, &def.id);
            let mut loc = locate(codesense_home, principal, &key)?;
            // It lives under the agent it runs, with the mode its author
            // picked, else `fresh` (`written_mode`).
            loc.dir =
                layout::routine_dir_in(&loc.config_dir, &layout::home_for_def(&key, &def), &def.id);
            check_wake(codesense_home, principal, &key, None, &def)?;
            // A new routine gets the mode the form showed: its author's
            // pick, else its agent's default, written down so a later change
            // to the agent never flips it (§9.2).
            let mode = if def.is_command() {
                String::new()
            } else {
                match def.mode.trim() {
                    "" => default_mode(codesense_home, principal, &key, &def)
                        .as_str()
                        .to_string(),
                    picked => picked.to_string(),
                }
            };
            let def = RoutineDef { mode, ..def };
            def.validate_wake_target()?;
            announced_with_warnings_for(
                codesense_home,
                principal,
                caller_scope,
                with_space_lock(&loc.config_dir, || {
                    defs::with_routine_lock(&loc.dir, || {
                        // Ids are unique across the space, whichever agent
                        // holds one, so a key always names one routine.
                        if layout::id_taken(&loc.config_dir, &def.id) {
                            return Err(RoutineError::AlreadyExists(loc.key.clone()));
                        }
                        commit(codesense_home, principal, &host, &store, &loc, def, author)
                    })
                }),
            )
        }
        SaveTarget::Update { key, base_revision } => {
            let loc = locate(codesense_home, principal, &key)?;
            if routine.id != key.id {
                return Err(RoutineError::InvalidLocation(format!(
                    "routine id {:?} cannot be changed to {:?}",
                    key.id, routine.id
                )));
            }
            let runs_on = scoped_runs_on(RoutineScope::of_team(&key.team), &routine)?;
            announced_with_warnings_for(
                codesense_home,
                principal,
                caller_scope,
                defs::with_routine_lock(&loc.dir, || {
                    let current = read_existing(&loc)?;
                    crate::session_usage::spend_caps::status(
                        codesense_home,
                        "routine",
                        &loc.key.to_string(),
                        current.policy.budget_usd,
                    )
                    .map_err(RoutineError::Io)?;
                    if current.revision != base_revision {
                        return Err(RoutineError::Conflict {
                            current_revision: current.revision,
                        });
                    }
                    check_wake(codesense_home, principal, &key, Some(&current), &routine)?;
                    let def = carried(
                        &current,
                        RoutineDef {
                            runs_on,
                            owner: current.owner.clone(),
                            armed_host: placed(&current, &routine, &host),
                            archived_at: current.archived_at.clone(),
                            revision: current.revision + 1,
                            ..routine
                        },
                    );
                    // Another agent moves the routine to that agent's dir: same
                    // id, key and runs, its own revisions with it.
                    def.validate_wake_target()?;
                    commit(codesense_home, principal, &host, &store, &loc, def, author)
                }),
            )
        }
    }
}

/// Serializes a space's id check with the create that follows it, so two
/// creates of one id under different agents cannot both pass. Taken before a
/// routine lock, never after one.
fn with_space_lock<T>(config_dir: &Path, f: impl FnOnce() -> T) -> T {
    defs::with_routine_lock(&layout::agents_dir(config_dir), f)
}

/// The body `next` with what an edit that does not know it leaves alone
/// carried over from `current`: the agent (a blank one keeps the current), the
/// mode, the machine fields, the migration record and why it was archived.
fn carried(current: &RoutineDef, mut next: RoutineDef) -> RoutineDef {
    if !next.is_command() && !current.is_command() && next.agent.is_empty() {
        next.agent = current.agent.clone();
    }
    if !next.is_command() && next.mode.trim().is_empty() {
        next.mode = current.mode.clone();
    }
    if !next.is_command() {
        next.mode = written_mode(&next.mode);
    }
    if next.target.machine.is_none() {
        next.target.machine = current.target.machine.clone();
    }
    next.armed_machine = current.armed_machine.clone();
    next.arm_epoch = current.arm_epoch;
    next.handover_from = current.handover_from.clone();
    next.migrated = current.migrated.clone();
    next.archived_with_agent = current.archived_with_agent && next.archived_at.is_some();
    next
}

/// `loc`, moved to the dir `def`'s agent gives it when that is not where it
/// is: the routine dir is renamed there, revisions and all. A legacy dir (a
/// read-only docs routine) never moves.
fn rehome(loc: &Located, def: &RoutineDef) -> Result<Located, RoutineError> {
    if layout::home_of_dir(&loc.dir) == Some(layout::Home::Legacy) {
        return Ok(Located {
            key: loc.key.clone(),
            org_root: loc.org_root.clone(),
            config_dir: loc.config_dir.clone(),
            dir: loc.dir.clone(),
        });
    }
    let home = layout::home_for_def(&loc.key, def);
    let dir = layout::routine_dir_in(&loc.config_dir, &home, &def.id);
    if dir != loc.dir {
        if dir.exists() {
            return Err(RoutineError::AlreadyExists(loc.key.clone()));
        }
        // A command home cannot imply the former agent for pinned revisions.
        let revisions = if def.is_command() && layout::implied_agent(&loc.dir).is_some() {
            defs::read_revisions(&loc.dir)?
        } else {
            Vec::new()
        };
        migrate::move_dir(&loc.dir, &dir)?;
        for revision in revisions {
            defs::write_revision_record(&dir, &revision)?;
        }
    }
    Ok(Located {
        key: loc.key.clone(),
        org_root: loc.org_root.clone(),
        config_dir: loc.config_dir.clone(),
        dir,
    })
}

/// The Mac a saved revision names, going from `current` to the body `next`:
///
/// - A shared job keeps the Mac it names, or takes this one when it names none
///   (only a hand-written definition can), so every shared job CodeSense
///   writes runs on exactly one Mac. An edit never moves one already placed;
///   [`arm_here`] moves it.
/// - A recipe each Mac runs that becomes a shared job is stamped with this
///   Mac, the one that changed it: it then runs here only, and every other
///   Mac stops.
/// - A routine that becomes, or stays, a recipe each Mac runs keeps the Mac it
///   names, so the Mac that ran it as a shared job starts on
///   (`host::starts_on_here`) and the change never silently stops it there.
///
/// A body whose `runs_on` this build cannot read keeps what it names; its save
/// is refused by validation anyway.
fn placed(current: &RoutineDef, next: &RoutineDef, this_host: &str) -> String {
    let (Ok(was), Ok(now)) = (current.runs_on(), next.runs_on()) else {
        return current.armed_host.clone();
    };
    match (was, now) {
        (RunsOn::EachMac, RunsOn::OneMac) => this_host.to_string(),
        (_, RunsOn::EachMac) => current.armed_host.clone(),
        (RunsOn::OneMac, RunsOn::OneMac) if current.armed_host.trim().is_empty() => {
            this_host.to_string()
        }
        (RunsOn::OneMac, RunsOn::OneMac) => current.armed_host.clone(),
    }
}

/// The `runs_on` a save in `scope` writes for `def`: a personal routine runs
/// on its own Mac, so it has no mode, and a recipe each Mac runs is refused
/// there.
fn scoped_runs_on(scope: RoutineScope, def: &RoutineDef) -> Result<String, RoutineError> {
    let mode = def.runs_on()?;
    match scope {
        RoutineScope::Personal if mode == RunsOn::EachMac => {
            Err(ValidationError::EachMacNeedsTeam.into())
        }
        RoutineScope::Personal => Ok(String::new()),
        RoutineScope::Team => Ok(def.runs_on.clone()),
    }
}

/// Archive a routine as a new revision. Its run history is kept, it drops out
/// of discovery and arming, and [`get`] still reads it. Archiving an archived
/// routine is a no-op.
pub fn archive(
    codesense_home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    author: &Author,
) -> Result<SavedRoutine, RoutineError> {
    archive_with(codesense_home, principal, key, author, false)
}

/// [`archive`], marked `archived_with_agent` when its agent's archive is what
/// archives it, so restoring the agent can offer it back.
pub(crate) fn archive_with(
    codesense_home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    author: &Author,
    with_agent: bool,
) -> Result<SavedRoutine, RoutineError> {
    let loc = locate(codesense_home, principal, key)?;
    announced(defs::with_routine_lock(&loc.dir, || {
        let current = read_existing(&loc)?;
        if current.is_archived() {
            return Ok(SavedRoutine {
                dir: loc.dir.clone(),
                key: loc.key.clone(),
                def: current,
                warnings: Vec::new(),
            });
        }
        let def = RoutineDef {
            archived_at: Some(now_rfc3339()),
            archived_with_agent: with_agent,
            revision: current.revision + 1,
            ..current
        };
        // No body validation or cap: retiring a routine must work even when its
        // stored body no longer passes this build's checks (a newer build's
        // fields, a hand edit), and archiving only ever lowers the active count.
        defs::write_revision(&loc.dir, &def, author, &now_rfc3339())?;
        invalidate_discovery_cache(codesense_home);
        Ok(SavedRoutine {
            dir: loc.dir.clone(),
            key: loc.key.clone(),
            def,
            warnings: Vec::new(),
        })
    }))
    .inspect(|saved| {
        super::target::remove_personal_run_dir(codesense_home, principal, &saved.key);
        // Its queued wakes are never typed now (§9.9).
        super::wake::on_archived(codesense_home, &saved.key.to_string());
        // SW-XS-25: nothing of it on a machine is stale or prepared now.
        if let Ok(store) = RoutineStateStore::open(codesense_home) {
            let _ = super::machine_arm::forget_machine_arms(&store, &saved.key.to_string(), None);
        }
    })
}

/// Bring an archived routine back, as a new revision. It counts against the
/// cap again and passes this build's checks like any save. Unarchiving one
/// that is not archived is a no-op.
pub fn unarchive(
    codesense_home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    author: &Author,
) -> Result<SavedRoutine, RoutineError> {
    let host = local_host_id(codesense_home).map_err(|e| RoutineError::Io(e.to_string()))?;
    let store = RoutineStateStore::open(codesense_home)?;
    let loc = locate(codesense_home, principal, key)?;
    announced_with_warnings(
        codesense_home,
        principal,
        defs::with_routine_lock(&loc.dir, || {
            let current = read_existing(&loc)?;
            if !current.is_archived() {
                return Ok(SavedRoutine {
                    dir: loc.dir.clone(),
                    key: loc.key.clone(),
                    def: current,
                    warnings: Vec::new(),
                });
            }
            let def = RoutineDef {
                archived_at: None,
                archived_with_agent: false,
                revision: current.revision + 1,
                ..current
            };
            commit(codesense_home, principal, &host, &store, &loc, def, author)
        }),
    )
}

/// Write a new revision whose body is revision `revision`'s. The owner,
/// `armed_host` and `archived_at` stay as they are now, so restoring never
/// moves the routine to another machine or un-archives it, except as a change
/// of `runs_on` or a shared job naming no Mac places it (see [`placed`]).
pub fn restore(
    codesense_home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    revision: u32,
    base_revision: u32,
    author: &Author,
) -> Result<SavedRoutine, RoutineError> {
    let host = local_host_id(codesense_home).map_err(|e| RoutineError::Io(e.to_string()))?;
    let store = RoutineStateStore::open(codesense_home)?;
    let loc = locate(codesense_home, principal, key)?;
    announced_with_warnings(
        codesense_home,
        principal,
        defs::with_routine_lock(&loc.dir, || {
            let current = read_existing(&loc)?;
            if current.revision != base_revision {
                return Err(RoutineError::Conflict {
                    current_revision: current.revision,
                });
            }
            let old = defs::read_revision(&loc.dir, revision)?.ok_or_else(|| {
                RoutineError::RevisionNotFound {
                    key: loc.key.clone(),
                    revision,
                }
            })?;
            let runs_on = scoped_runs_on(RoutineScope::of_team(&loc.key.team), &old.routine)?;
            let def = carried(
                &current,
                RoutineDef {
                    runs_on,
                    id: current.id.clone(),
                    owner: current.owner.clone(),
                    // Keep ownership on an agent-only restore; an action change
                    // restores the revision's own agent fields and home.
                    agent: if old.routine.action == current.action {
                        current.agent.clone()
                    } else {
                        old.routine.agent.clone()
                    },
                    armed_host: placed(&current, &old.routine, &host),
                    archived_at: current.archived_at.clone(),
                    revision: current.revision + 1,
                    ..old.routine
                },
            );
            commit(codesense_home, principal, &host, &store, &loc, def, author)
        }),
    )
}

/// Flip this Mac's switch for a routine: pause or resume a shared job, or
/// turn a recipe each Mac runs on or off for this Mac ("Run on my Mac").
/// Local state only; the definition and git are untouched. Turning one on
/// counts against the cap.
pub fn set_enabled(
    codesense_home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    enabled: bool,
) -> Result<(), RoutineError> {
    let loc = locate(codesense_home, principal, key)?;
    let store = RoutineStateStore::open(codesense_home)?;
    let host = local_host_id(codesense_home).map_err(|e| RoutineError::Io(e.to_string()))?;
    // The definition is read under the routine lock so a concurrent `arm_here`
    // cannot move it onto this host between the read and the cap check.
    defs::with_routine_lock(&loc.dir, || {
        let def = read_existing(&loc)?;
        switch_here(codesense_home, principal, &host, &store, key, &def, enabled)
    })?;
    events::routine_changed(&key.to_string());
    Ok(())
}

/// Write this Mac's switch for `key` under `def`'s mode, cap-checked when it
/// turns on. Callers hold the routine lock.
fn switch_here(
    codesense_home: &Path,
    principal: &Principal,
    host: &str,
    store: &RoutineStateStore,
    key: &RoutineKey,
    def: &RoutineDef,
    enabled: bool,
) -> Result<(), RoutineError> {
    let mode = def.runs_on()?;
    let _cap = cap_guard();
    if enabled && !store.is_enabled(key, def, host)? {
        ensure_under_cap(codesense_home, principal, host, store, key, def, true)?;
    }
    Ok(store.set_enabled(key, mode, enabled)?)
}

/// Make this machine the one that fires the routine, as a new revision. Anyone
/// may move a routine: where it runs is placement, not permission. A routine
/// already armed here is returned unchanged. A recipe each Mac runs is not
/// moved: this Mac's switch for it is turned on instead, which is local state
/// and writes no revision.
pub fn arm_here(
    codesense_home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    author: &Author,
) -> Result<SavedRoutine, RoutineError> {
    let host = local_host_id(codesense_home).map_err(|e| RoutineError::Io(e.to_string()))?;
    let store = RoutineStateStore::open(codesense_home)?;
    let loc = locate(codesense_home, principal, key)?;
    // SW-AGR-8: one armed on a machine moves back as a move does (a new
    // epoch, and a handover until the machine lets go).
    if read_existing(&loc)?.armed_machine().is_some() {
        return arm_on(
            codesense_home,
            principal,
            key,
            ArmPlacement::ThisMac,
            author,
        );
    }
    announced(defs::with_routine_lock(&loc.dir, || {
        let current = read_existing(&loc)?;
        if current.is_archived() {
            return Err(RoutineError::Archived(loc.key.clone()));
        }
        if current.runs_on()? == RunsOn::EachMac {
            switch_here(
                codesense_home,
                principal,
                &host,
                &store,
                &loc.key,
                &current,
                true,
            )?;
            return Ok(SavedRoutine {
                dir: loc.dir.clone(),
                key: loc.key.clone(),
                def: current,
                warnings: Vec::new(),
            });
        }
        if current.armed_host == host {
            return Ok(SavedRoutine {
                dir: loc.dir.clone(),
                key: loc.key.clone(),
                def: current,
                warnings: Vec::new(),
            });
        }
        let def = RoutineDef {
            armed_host: host.clone(),
            revision: current.revision + 1,
            arm_epoch: current.arm_epoch + 1,
            ..current
        };
        commit(codesense_home, principal, &host, &store, &loc, def, author)
    }))
}

/// SW-AGR-8: where a routine is armed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArmPlacement {
    /// This Mac fires it (`armed_host`).
    ThisMac,
    /// That machine's session server fires it on its own clock; this Mac
    /// manages the arm.
    Machine(String),
}

/// SW-AGR-8: move a routine between this Mac and a machine, as a new
/// revision under a new arm epoch (the machine fences every arm by it). This
/// Mac becomes the one that manages it. Moving it off a machine records a
/// pending handover: nothing else fires it until that machine acknowledges
/// the disarm (`machine_arm`), so the move never doubles a fire. A routine
/// already where it is asked to be (and managed here) is returned unchanged.
pub fn arm_on(
    codesense_home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    placement: ArmPlacement,
    author: &Author,
) -> Result<SavedRoutine, RoutineError> {
    // Discovery, import and its cursor save must finish before ownership
    // changes and rewinds that cursor.
    let _sync = super::machine_arm_app::SYNCING
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let host = local_host_id(codesense_home).map_err(|e| RoutineError::Io(e.to_string()))?;
    let store = RoutineStateStore::open(codesense_home)?;
    let loc = locate(codesense_home, principal, key)?;
    announced(defs::with_routine_lock(&loc.dir, || {
        let current = read_existing(&loc)?;
        if current.is_archived() {
            return Err(RoutineError::Archived(loc.key.clone()));
        }
        let from = current.armed_machine().map(str::to_string);
        let to = match &placement {
            ArmPlacement::ThisMac => None,
            ArmPlacement::Machine(machine) => Some(machine.trim().to_string()),
        };
        if from == to && current.armed_host == host {
            return Ok(SavedRoutine {
                dir: loc.dir.clone(),
                key: loc.key.clone(),
                def: current,
                warnings: Vec::new(),
            });
        }
        let mut def = current.clone();
        def.armed_host = host.clone();
        def.revision = current.revision + 1;
        def.arm_epoch = current.arm_epoch + 1;
        if from != to {
            def.armed_machine = to.clone();
            if let Some(machine) = &to {
                // Its agent runs on that machine too.
                def.target.machine = Some(machine.clone());
            }
            // Moved off a machine: every Mac holds it until that machine
            // acknowledges the disarm (the file says so to each of them).
            if let Some(machine) = &from {
                def.handover_from = Some(super::model::HandoverFrom {
                    machine: machine.clone(),
                    epoch: def.arm_epoch,
                });
            }
        }
        let saved = commit(codesense_home, principal, &host, &store, &loc, def, author)?;
        if current.armed_host != host {
            super::machine_arm::rewind_run_cursors(&store, from.as_deref(), to.as_deref())?;
        }
        // SW-XS-25: what was stale or prepared on the machine it left is moot.
        super::machine_arm::forget_machine_arms(&store, &loc.key.to_string(), to.as_deref())?;
        if let Some(machine) = from.filter(|from| Some(from) != to.as_ref()) {
            super::machine_arm::begin_handover(
                &store,
                &loc.key.to_string(),
                &machine,
                saved.def.arm_epoch,
                chrono::Utc::now().timestamp_millis(),
            )?;
        }
        Ok(saved)
    }))
}

/// SW-AGR-8: `machine` acknowledged it stopped firing `key` (at `epoch` or
/// later): the routine's file no longer holds every Mac back. The revision
/// that clears it keeps the last editor as its author (it changes nothing
/// anyone wrote). A file that names another handover is left as it is.
pub fn settle_handover(
    codesense_home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    machine: &str,
    epoch: u64,
) -> Result<(), RoutineError> {
    let host = local_host_id(codesense_home).map_err(|e| RoutineError::Io(e.to_string()))?;
    let store = RoutineStateStore::open(codesense_home)?;
    let loc = locate(codesense_home, principal, key)?;
    announced(defs::with_routine_lock(&loc.dir, || {
        let current = read_existing(&loc)?;
        let clears = current
            .handover_from
            .as_ref()
            .is_some_and(|from| from.machine == machine && from.epoch <= epoch);
        if !clears {
            return Ok(SavedRoutine {
                dir: loc.dir.clone(),
                key: loc.key.clone(),
                def: current,
                warnings: Vec::new(),
            });
        }
        let author = defs::read_revision(&loc.dir, current.revision)
            .ok()
            .flatten()
            .map(|record| record.author)
            .unwrap_or_else(|| Author::agent("CodeSense"));
        let mut def = current.clone();
        def.handover_from = None;
        def.revision = current.revision + 1;
        commit(codesense_home, principal, &host, &store, &loc, def, &author)
    }))
    .map(|_| ())
}

/// SW-AGR-8: give up waiting for `machine` to acknowledge it stopped firing
/// `key`, because that machine is gone (the person says so): its pending
/// handover ends here and in the routine's file. If the machine is in fact
/// still running and still holds the arm, it may fire the routine once more
/// alongside its new home; only release a machine that is gone.
pub fn release_handover(
    codesense_home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    machine: &str,
) -> Result<(), RoutineError> {
    let store = RoutineStateStore::open(codesense_home)?;
    super::machine_arm::release_handover(&store, &key.to_string(), machine)?;
    settle_handover(codesense_home, principal, key, machine, u64::MAX)
}

/// Copy any visible routine (typically a teammate's) into a new routine owned
/// by the caller: a fresh id, armed on this machine, revision 1.
pub fn copy_to_mine(
    codesense_home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    placement: Placement,
    author: &Author,
) -> Result<SavedRoutine, RoutineError> {
    let source = read_existing(&locate(codesense_home, principal, key)?)?;
    let (org, team, config_dir) = resolve_placement(codesense_home, principal, &placement)?;
    let id = fresh_copy_id(&config_dir, &source.id);
    // The copy names the same agents: a bare id means the agent of the
    // routine's own space, so each is rewritten for the copy's.
    let copy_key = RoutineKey::scoped(principal, org, team, &id);
    let same_agent = |agent: &AgentRef| {
        agent
            .named_key(key)
            .map(|named| AgentRef::to_agent(&named, &copy_key))
            .unwrap_or_else(|_| agent.clone())
    };
    // A runner the copy's space may not run (another team's agent, in a team
    // copy) becomes the agent of the same id there.
    let agent = Some(same_agent(&source.agent))
        .filter(|agent| agent.key_for(&copy_key).is_ok())
        .unwrap_or_else(|| match source.agent.named_key(key) {
            Ok(named) => AgentRef::new(named.id),
            Err(_) => source.agent.clone(),
        });
    let mut triggers = source.triggers.clone();
    for trigger in &mut triggers {
        if let RoutineTrigger::Event(spec) = trigger {
            spec.from_agent = spec.from_agent.as_ref().map(same_agent);
        }
    }
    // A personal copy runs on its own Mac, whichever way the original runs.
    let runs_on = match placement {
        Placement::Personal(_) => String::new(),
        Placement::Team { .. } => source.runs_on.clone(),
    };
    let routine = RoutineDef {
        id,
        owner: String::new(),
        runs_on,
        agent,
        triggers,
        migrated: None,
        archived_with_agent: false,
        ..source
    };
    save(
        codesense_home,
        principal,
        SaveTarget::Create(placement),
        routine,
        author,
    )
}

/// `<id>-copy`, then `<id>-copy-2`, … : the first that is free in `config_dir`.
/// The base is shortened so the result stays a valid id.
fn fresh_copy_id(config_dir: &Path, id: &str) -> String {
    let stem: String = id.chars().take(50).collect();
    first_free_id(config_dir, &format!("{stem}-copy"))
}

/// `base`, then `base-2`, `base-3`, … : the first routine id not taken in
/// `config_dir`.
pub(crate) fn first_free_id(config_dir: &Path, base: &str) -> String {
    layout::first_free_id(config_dir, base)
}

/// Fire `key` now, by hand. It goes through the routine's policy like any
/// fire: a paused routine records a `paused` skip, and an active run makes it
/// coalesce, skip or wait. It runs on this Mac, wherever the routine is
/// armed, and a pause counts only on the Mac it is armed on; an archived one
/// cannot run. `variables` override the declared
/// defaults.
pub fn run_now(
    codesense_home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    variables: std::collections::BTreeMap<String, String>,
) -> Result<Admission, RoutineError> {
    let loc = locate(codesense_home, principal, key)?;
    let def = read_existing(&loc)?;
    if def.is_archived() {
        return Err(RoutineError::Archived(key.clone()));
    }
    // Armed on a machine, it fires there, through that machine's policy and
    // single flight: never here.
    if let Some(machine) = def.armed_machine() {
        return Err(RoutineError::OnMachine {
            key: key.clone(),
            machine: machine.to_string(),
        });
    }
    // Moved off a machine that has not confirmed the disarm: it may still be
    // firing there, so nothing fires it here, by hand either. Handovers that
    // cannot be read hold it (fails closed).
    let handing_over = def
        .handover_from
        .as_ref()
        .map(|from| from.machine.clone())
        .or_else(|| {
            let store = RoutineStateStore::open(codesense_home).ok();
            match store.map(|store| super::machine_arm::handovers(&store)) {
                Some(Ok(rows)) => rows
                    .into_iter()
                    .find(|h| h.routine_key == key.to_string())
                    .map(|h| h.machine),
                _ => (def.arm_epoch > 0).then(|| "a machine".to_string()),
            }
        });
    if let Some(machine) = handing_over {
        return Err(RoutineError::HandingOver {
            key: key.clone(),
            machine,
        });
    }
    // A routine whose agent is gone is Unassigned and never fires, by hand
    // either: it waits for someone to pick an agent.
    let assigned = def.is_command()
        || def
            .agent
            .key_for(key)
            .is_ok_and(|agent| agent_is_here(codesense_home, principal, &agent));
    if !assigned {
        return Err(RoutineError::Unassigned(key.clone()));
    }
    if let Some(name) = variables
        .keys()
        .find(|n| !def.is_command() && !def.variables.contains_key(*n))
    {
        return Err(RoutineError::UnknownVariable(name.clone()));
    }
    let store = RoutineStateStore::open(codesense_home)?;
    let admission = store.transaction(|tx| {
        admit::admit(
            tx,
            codesense_home,
            &Fire {
                key,
                principal,
                def: &def,
                source: RunSource::Manual,
                occurrence_key: None,
                scheduled_at: None,
                now: finish::now_ms(),
                variables,
                trigger_payload: None,
                cause: None,
            },
        )
    })?;
    events::changed(&key.to_string(), &admission.run_id);
    if admission.is_queued() {
        crate::commands::sprint_run::claim_loop::wake();
    }
    Ok(admission)
}

/// Stop a run: it is ended `cancelled` first, so the exit the kill causes
/// changes nothing, then its sessions are killed. A held or not yet dispatched
/// run is cancelled before it ever launches. False when it had already ended.
/// A run of a routine the caller cannot read is not found.
pub fn stop_run(
    codesense_home: &Path,
    principal: &Principal,
    run_id: &str,
    sessions: &dyn Sessions,
) -> Result<bool, RoutineError> {
    stop_run_for(
        codesense_home,
        principal,
        run_id,
        sessions,
        &Default::default(),
    )
}

pub(crate) fn stop_run_for(
    codesense_home: &Path,
    principal: &Principal,
    run_id: &str,
    sessions: &dyn Sessions,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<bool, RoutineError> {
    let store = RoutineStateStore::open(codesense_home)?;
    let (run, _) = readable_run_for(codesense_home, principal, &store, run_id, scope)?;
    // A wake still queued on a busy session is withdrawn from its host first
    // (SW-AGR-5 "Cancel wake"), as an archive does: ending only the run here
    // would leave the text to be typed when the session goes idle.
    if run.phase == super::runs::phase::QUEUED {
        // A remote spawn for this run is out and unanswered (SW-AGR-2b): its
        // agent may be starting with the task. Ending the run here would lose
        // what it does; it settles when the launch answers.
        use rusqlite::OptionalExtension;
        let launching: Option<String> = store.transaction(|tx| {
            Ok(tx
                .query_row(
                    "SELECT launch_request FROM routine_runs WHERE run_id = ?1",
                    rusqlite::params![run_id],
                    |r| r.get(0),
                )
                .optional()?
                .flatten())
        })?;
        if launching.is_some() {
            let machine = run.launch_machine.as_deref().unwrap_or("its machine");
            return Err(RoutineError::Io(format!(
                "{} is still starting on {machine}, so this run can't be stopped yet; it is still queued",
                run.agent.as_deref().unwrap_or("the agent")
            )));
        }
        if let Some(stopped) = super::wake::cancel_queued_run(codesense_home, &store, &run)? {
            return Ok(stopped);
        }
    }
    let Some(finished) = finish::finish(&store, run_id, Ending::Cancelled, finish::now_ms())?
    else {
        return Ok(false);
    };
    // A wake run woke a live session it does not own: nothing to kill.
    if run.wake_machine.is_none() {
        sessions.kill_on(run_id, run.session_machine(), run.launched_session());
    }
    finished.announce();
    Ok(true)
}
