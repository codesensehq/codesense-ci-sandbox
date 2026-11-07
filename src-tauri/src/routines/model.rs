// SPDX-License-Identifier: MIT
//! The routine definition: serde types for `routine.yml` and the save-time
//! validation every write runs.
//!
//! Choice-like fields that v1 only partly supports (`where.kind`, event
//! `kind`) are kept as strings on the wire and checked here, so an
//! unsupported value is a typed [`ValidationError`] with a useful message rather
//! than an opaque deserialize failure. Unknown keys at every level are kept in a
//! `#[serde(flatten)]` catch-all so a newer build's fields survive an edit made
//! by an older one. The one exception is `deliver`: routines never deliver
//! results, so that key is refused at save rather than silently carried.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize};

#[cfg(test)]
#[cfg(test)]
use crate::domain::Principal;

use super::schedule::{self, ScheduleError};
use super::template;

use crate::agents::AgentKey;
use crate::context_repo::scoped_key::ScopedKey;
pub use crate::context_repo::scoped_key::PERSONAL_OWNER;

/// The `<org>/<team>/<id>` identity of one routine; see [`ScopedKey`]. The
/// run state behind a personal key lives in a coordinator shared by every
/// account on the machine, which is why the key carries its owner.
pub type RoutineKey = ScopedKey;

/// The agent a routine runs, as written in `routine.yml`: a bare agent id,
/// which names that agent in the routine's own space (its team for a team
/// routine, its account's personal space for a personal one), or a rendered
/// agent key (`<org>/<team>/<id>`), which lets a personal routine name a team
/// agent.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentRef(String);

impl AgentRef {
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into().trim().to_string())
    }

    /// The reference to `agent` as a routine at `routine` writes it: the bare
    /// id when the agent lives in the routine's own space, else its key.
    pub fn to_agent(agent: &AgentKey, routine: &RoutineKey) -> Self {
        let same_space = agent.org == routine.org
            && agent.team == routine.team
            && agent.account == routine.account;
        Self(if same_space {
            agent.id.clone()
        } else {
            agent.to_string()
        })
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True when no agent is written: a definition stored under its agent's
    /// directory leaves the agent to its path (`layout`).
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The shape checks every save runs; where the agent may live is
    /// [`Self::key_for`]'s.
    fn validate(&self) -> Result<(), ValidationError> {
        let raw = self.0.as_str();
        if raw.is_empty() {
            return Err(ValidationError::EmptyAgent);
        }
        // No org, team or id segment may hold ':'; key parsing alone checks only the id.
        if raw.contains(':') {
            return Err(ValidationError::InvalidAgent(raw.to_string()));
        }
        let well_formed = if raw.contains('/') {
            AgentKey::parse(raw).is_some_and(|key| crate::agents::model::is_valid_id(&key.id))
        } else {
            crate::agents::model::is_valid_id(raw)
        };
        if !well_formed {
            return Err(ValidationError::InvalidAgent(raw.to_string()));
        }
        Ok(())
    }

    /// The key of the agent this reference names, read in the routine at
    /// `routine`'s own space, wherever that agent lives. What an `agent_done`
    /// filter waits for: a routine may watch any agent finish.
    pub fn named_key(&self, routine: &RoutineKey) -> Result<AgentKey, ValidationError> {
        self.validate()?;
        let raw = self.0.as_str();
        Ok(
            match raw.contains('/').then(|| AgentKey::parse(raw)).flatten() {
                Some(key) => key,
                None => AgentKey {
                    id: raw.to_string(),
                    ..routine.clone()
                },
            },
        )
    }

    /// The key of the agent the routine at `routine` runs. A team routine may
    /// be re-homed onto any teammate's Mac, so it can only run an agent of its
    /// own team or a starter (every Mac has those), never a personal one.
    pub fn key_for(&self, routine: &RoutineKey) -> Result<AgentKey, ValidationError> {
        let key = self.named_key(routine)?;
        let raw = self.0.as_str();
        if key.is_builtin() || !raw.contains('/') {
            return Ok(key);
        }
        if routine.is_personal() {
            // SLW-PT-19: a personal routine is filed under one org; it never reaches
            // another org's agents, personal or team.
            if key.org != routine.org {
                return Err(ValidationError::AgentOutsideOrg(raw.to_string()));
            }
            return Ok(key);
        }
        if key.is_personal() {
            return Err(ValidationError::TeamRoutineNeedsTeamAgent(raw.to_string()));
        }
        if key.org != routine.org || key.team != routine.team {
            return Err(ValidationError::AgentOutsideTeam(raw.to_string()));
        }
        Ok(key)
    }
}

impl fmt::Display for AgentRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for AgentRef {
    fn from(raw: &str) -> Self {
        Self::new(raw)
    }
}

impl Serialize for AgentRef {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for AgentRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::new)
    }
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, rmcp::schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RoutineAction {
    #[default]
    Agent,
    Command,
}

impl RoutineAction {
    pub fn is_agent(&self) -> bool {
        *self == Self::Agent
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct After {
    pub routine: String,
    pub on: AfterOn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AfterOn {
    Succeeded,
    Finished,
}

/// One routine definition, exactly as stored in `routine.yml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
pub struct RoutineDef {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "RoutineAction::is_agent")]
    pub action: RoutineAction,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<After>,
    /// The principal who created this routine, for display and attribution.
    /// It grants nothing: anyone who can see a routine can change it.
    #[serde(default)]
    pub owner: String,
    /// The agent that runs the task; see [`AgentRef`]. A routine stored under
    /// its agent (`config/agents/<agent>/routines/<id>/`) does not write it: the
    /// path names the agent and every read fills it in (`layout`). Only a
    /// legacy `config/routines/<id>/` file or an `_unassigned` one carries it.
    #[serde(default, skip_serializing_if = "AgentRef::is_empty")]
    #[schemars(with = "String")]
    pub agent: AgentRef,
    /// What a fire does: `fresh` starts a new instance of the agent, `wake`
    /// delivers the task into a live one (starting one only when
    /// none is running). Blank when the author picked none: the default is worked out
    /// at fire time (`service::effective_mode`); migration writes `fresh`.
    /// Kept as a string so an unknown value is a
    /// typed [`ValidationError`]; see [`RoutineDef::mode`].
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mode: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub task: String,
    /// Optional bounded inline JSON Schema for a private dashboard snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_schema: Option<serde_json::Value>,
    #[serde(rename = "where")]
    pub target: Where,
    /// Empty means manual-only.
    #[serde(
        default,
        with = "serde_yaml::with::singleton_map_recursive",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[schemars(with = "Vec<RoutineTrigger>")]
    pub triggers: Vec<RoutineTrigger>,
    #[serde(default)]
    pub policy: Policy,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub variables: BTreeMap<String, Variable>,
    #[serde(default)]
    pub revision: u32,
    /// The local host id of the one Mac that arms this routine, so it fires
    /// once however many machines have it. Placement, not permission: anyone
    /// may move it. Unset only in a definition written outside CodeSense; see
    /// `host::is_armed_here`.
    #[serde(default)]
    pub armed_host: String,
    /// The team-shared id of the machine whose session server arms this
    /// routine: it fires there on the machine's own clock, with or
    /// without a desktop. `armed_host` then names the Mac that manages the
    /// arm (pushes it, and records its runs); no Mac fires it itself. Old
    /// clients carry it as an unknown key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub armed_machine: Option<String>,
    /// Bumped on every move of the routine between a desktop and a
    /// machine. A machine refuses an arm or disarm older than the epoch it
    /// holds, so a stale definition can never re-arm it there.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub arm_epoch: u64,
    /// The machine this routine moved off and the epoch of that
    /// move, until that machine acknowledged it stopped firing it. No Mac
    /// fires the routine meanwhile (it may still fire there), whichever Mac
    /// reads this; the Mac that manages it clears it once acknowledged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handover_from: Option<HandoverFrom>,
    /// How a team routine runs: `one_mac` (the default, also when empty) runs
    /// it once for the team on the `armed_host` Mac; `each_mac` runs it on
    /// every Mac whose user turned it on there, each with its own runs. Kept
    /// as a string so an unknown value is a typed [`ValidationError`]; see
    /// [`RoutineDef::runs_on`].
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub runs_on: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    /// Archived because its agent was archived: restoring the agent offers to
    /// restore exactly these (`lifecycle`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived_with_agent: bool,
    /// Where a migrated routine came from (`migrate`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migrated: Option<Migrated>,
    #[serde(flatten)]
    #[schemars(with = "BTreeMap<String, serde_json::Value>")]
    pub extra: BTreeMap<String, serde_yaml::Value>,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// A machine a routine moved off, until it acknowledged the disarm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
pub struct HandoverFrom {
    pub machine: String,
    pub epoch: u64,
}

/// The legacy file a routine was migrated from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
pub struct Migrated {
    /// The legacy dir relative to the space's `config/`: `routines/<id>`.
    /// The legacy content last brought forward (the 3-way merge base for an
    /// older client's edits) is kept beside the routine, in
    /// `migrate::BASE_FILE`, so mirroring it never rewrites this file.
    pub from: String,
    /// Shown with the routine: why it was renamed on the way (an id clash).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The revision the move wrote: the editor says why the routine
    /// starts a new session only until a later revision is saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u32>,
}

/// Where the run happens. `kind` is `team_workspace` or `repo:<owner>/<repo>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
pub struct Where {
    pub kind: String,
    /// Which machine runs it: `local`, `any` or a team-shared machine id;
    /// absent means local.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    #[serde(flatten)]
    #[schemars(with = "BTreeMap<String, serde_json::Value>")]
    pub extra: BTreeMap<String, serde_yaml::Value>,
}

impl Where {
    pub fn team_workspace() -> Self {
        Self {
            kind: WHERE_TEAM_WORKSPACE.to_string(),
            machine: None,
            extra: BTreeMap::new(),
        }
    }

    /// The typed target, or the validation error that rejects `kind`.
    pub fn resolve(&self) -> Result<RoutineTarget, ValidationError> {
        let kind = self.kind.trim();
        if kind == WHERE_TEAM_WORKSPACE {
            return Ok(RoutineTarget::TeamWorkspace);
        }
        if kind.starts_with(WHERE_MACHINE_PREFIX) {
            return Err(ValidationError::RemoteMachineUnsupported(kind.to_string()));
        }
        if let Some(slug) = kind.strip_prefix(WHERE_REPO_PREFIX) {
            return crate::domain::SourceRepoSlug::parse_owner_repo(slug)
                .map(|_| RoutineTarget::Repo(slug.trim().to_string()))
                .ok_or_else(|| ValidationError::UnsupportedWhere(kind.to_string()));
        }
        Err(ValidationError::UnsupportedWhere(kind.to_string()))
    }
}

const WHERE_TEAM_WORKSPACE: &str = "team_workspace";
const WHERE_REPO_PREFIX: &str = "repo:";
const WHERE_MACHINE_PREFIX: &str = "machine:";
pub(crate) const EVENT_METADATA_CHANGE: &str = "metadata_change";
pub(crate) const EVENT_AGENT_DONE: &str = "agent_done";
/// Fires when mail is stored for a session idle at its prompt, or a session goes idle
/// with mail it has not read; see `super::inbox_idle`.
pub(crate) const EVENT_INBOX_IDLE: &str = "inbox_idle";
/// Fires when a message is queued for a role inbox; see `super::role_pending`.
pub(crate) const EVENT_ROLE_PENDING: &str = "role_pending";
const DELIVER_KEY: &str = "deliver";
const MODE_WAKE: &str = "wake";
const MODE_FRESH: &str = "fresh";
const RUNS_ON_ONE_MAC: &str = "one_mac";
const RUNS_ON_EACH_MAC: &str = "each_mac";

/// The validated form of [`RoutineDef::runs_on`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunsOn {
    /// A shared job: it runs once for the team, on the Mac `armed_host` names.
    OneMac,
    /// A shared recipe: each Mac whose user turned it on runs its own
    /// instance. `armed_host` only says which Mac it starts on (see
    /// `host::starts_on_here`).
    EachMac,
}

impl RunsOn {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OneMac => RUNS_ON_ONE_MAC,
            Self::EachMac => RUNS_ON_EACH_MAC,
        }
    }

    /// `one_mac`, `each_mac`, or blank for the default `one_mac`.
    pub fn parse(raw: &str) -> Result<Self, ValidationError> {
        match raw.trim() {
            "" | RUNS_ON_ONE_MAC => Ok(Self::OneMac),
            RUNS_ON_EACH_MAC => Ok(Self::EachMac),
            other => Err(ValidationError::UnknownRunsOn(other.to_string())),
        }
    }
}

/// The validated form of [`RoutineDef::mode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutineMode {
    /// Deliver the task into a live instance of the agent (SW-AGR-2).
    Wake,
    /// Start a new instance on every fire: what every routine did before.
    Fresh,
}

impl RoutineMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wake => MODE_WAKE,
            Self::Fresh => MODE_FRESH,
        }
    }

    /// `wake`, `fresh`, or blank for a definition written before modes, which
    /// runs `fresh` as it always did.
    pub fn parse(raw: &str) -> Result<Self, ValidationError> {
        match raw.trim() {
            "" | MODE_FRESH => Ok(Self::Fresh),
            MODE_WAKE => Ok(Self::Wake),
            other => Err(ValidationError::UnknownMode(other.to_string())),
        }
    }
}

/// The validated form of [`Where::kind`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutineTarget {
    TeamWorkspace,
    /// An `owner/repo` source repo.
    Repo(String),
}

/// One trigger. On the wire it is a single-key map: `- schedule: {…}` or
/// `- event: {…}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoutineTrigger {
    Schedule(ScheduleSpec),
    Event(EventSpec),
}

/// A wall-clock schedule: a 5-field cron in an IANA zone (see
/// [`super::schedule`]). A missing `tz` is filled with this machine's zone at
/// save and persisted, so it never changes with where the app later runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
pub struct ScheduleSpec {
    pub cron: String,
    #[serde(default)]
    pub tz: String,
}

/// An event trigger: `agent_done` (an agent finished), `inbox_idle` (an agent sits
/// idle at its prompt with unread inbox mail) or `role_pending` (a message is queued
/// for a role inbox in reach of the routine).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
pub struct EventSpec {
    pub kind: String,
    /// `agent_done` only: only an agent this names finishing fires it; read like
    /// [`RoutineDef::agent`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub from_agent: Option<AgentRef>,
    /// `role_pending` only: watch this queue; omit to watch every queue in reach.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<MetadataCondition>,
}

/// Selection and predicates share the context query wire model and comparator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MetadataCondition {
    #[serde(default)]
    pub selection: crate::context_query::model::ContextQuery,
    pub target: crate::context_query::model::FilterClause,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<crate::context_query::model::FilterClause>,
    /// An authoring hint for list wording; matching always uses the query engine.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub list: bool,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, rmcp::schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum OverlapPolicy {
    #[default]
    Coalesce,
    Skip,
    Queue,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, rmcp::schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum MissedPolicy {
    #[default]
    Skip,
    RunOnce,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
pub struct Policy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_usd: Option<crate::session_usage::spend_caps::Budget>,
    #[serde(default)]
    pub overlap: OverlapPolicy,
    #[serde(default)]
    pub missed: MissedPolicy,
    /// Required. `30m` / `2h`, between one minute and a day. Optional on the wire
    /// only so a missing value is a typed validation error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_runtime: Option<String>,
    #[serde(default)]
    pub visible: bool,
    /// Optional, set by the routine's author (usually an
    /// agent): how many deliveries its wakes may make in all. Reached, the
    /// routine pauses itself with one inbox note; resuming starts a new count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wakes: Option<u32>,
    /// Deliveries in any rolling 24 h. Over it a fire is
    /// `undelivered(limit_reached)`; it resumes on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wakes_per_day: Option<u32>,
    /// The least time between two wakes of this routine, into any session (`30m`,
    /// `2h`, `1d`; at most `7d`), for wakes that start heavy work. A fire
    /// inside it is coalesced or skipped per `overlap`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_gap: Option<String>,
    #[serde(flatten)]
    #[schemars(with = "BTreeMap<String, serde_json::Value>")]
    pub extra: BTreeMap<String, serde_yaml::Value>,
}

/// The longest `policy.min_gap`.
const MAX_MIN_GAP: Duration = Duration::from_secs(7 * 24 * 60 * 60);

impl Policy {
    /// The parsed `min_gap` (`None` when unset), or the error that rejects it.
    pub fn min_gap_duration(&self) -> Result<Option<Duration>, ValidationError> {
        let Some(raw) = self.min_gap.as_deref().map(str::trim) else {
            return Ok(None);
        };
        let gap = parse_gap(raw).ok_or_else(|| {
            ValidationError::WakeLimit(format!(
                "policy.min_gap {raw:?} must be written like 30m, 2h or 1d"
            ))
        })?;
        if gap < MIN_RUNTIME || gap > MAX_MIN_GAP {
            return Err(ValidationError::WakeLimit(format!(
                "policy.min_gap {raw:?} must be between 1m and 7d"
            )));
        }
        Ok(Some(gap))
    }

    /// Every check on the wake limits (SW-AGR-13).
    pub fn validate_wake_limits(&self) -> Result<(), ValidationError> {
        for (name, value) in [
            ("max_wakes", self.max_wakes),
            ("max_wakes_per_day", self.max_wakes_per_day),
        ] {
            if value == Some(0) {
                return Err(ValidationError::WakeLimit(format!(
                    "policy.{name} must be at least 1 (omit it for no limit)"
                )));
            }
        }
        self.min_gap_duration().map(|_| ())
    }

    /// Whether any wake limit is set.
    pub fn has_wake_limits(&self) -> bool {
        self.max_wakes.is_some() || self.max_wakes_per_day.is_some() || self.min_gap.is_some()
    }

    /// The parsed `max_runtime`, or the error that rejects it.
    pub fn max_runtime_duration(&self) -> Result<Duration, ValidationError> {
        let raw = self
            .max_runtime
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or(ValidationError::MaxRuntimeRequired)?;
        parse_max_runtime(raw)
    }
}

/// Parse a trimmed, non-empty `max_runtime` string and apply the same range
/// check (1m–24h) a routine policy applies. Shared with the
/// `open_workspace` launch budget so the two surfaces behave
/// alike: a caller asking for 48h gets an error, not a silent 24h.
pub(crate) fn parse_max_runtime(raw: &str) -> Result<Duration, ValidationError> {
    let duration =
        parse_runtime(raw).ok_or_else(|| ValidationError::MaxRuntimeInvalid(raw.to_string()))?;
    if duration < MIN_RUNTIME || duration > MAX_RUNTIME {
        return Err(ValidationError::MaxRuntimeOutOfRange(raw.to_string()));
    }
    Ok(duration)
}

const MIN_RUNTIME: Duration = Duration::from_secs(60);
const MAX_RUNTIME: Duration = Duration::from_secs(24 * 60 * 60);

/// `<digits>m`, `<digits>h` or `<digits>d`.
fn parse_gap(raw: &str) -> Option<Duration> {
    match raw.strip_suffix('d') {
        Some(days) if !days.is_empty() && days.bytes().all(|b| b.is_ascii_digit()) => Some(
            Duration::from_secs(days.parse::<u64>().ok()?.checked_mul(86_400)?),
        ),
        Some(_) => None,
        None => parse_runtime(raw),
    }
}

/// `<digits>m` or `<digits>h`. Seconds are deliberately not accepted: the
/// shortest allowed runtime is a minute.
fn parse_runtime(raw: &str) -> Option<Duration> {
    let (digits, unit_secs) = if let Some(d) = raw.strip_suffix('m') {
        (d, 60)
    } else if let Some(d) = raw.strip_suffix('h') {
        (d, 3600)
    } else {
        return None;
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u64 = digits.parse().ok()?;
    Some(Duration::from_secs(n.checked_mul(unit_secs)?))
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, rmcp::schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum VariableType {
    #[default]
    Text,
    Number,
    Boolean,
}

/// A declared task variable. `default` is required when any trigger is a
/// schedule, because nobody is there to supply a value.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
pub struct Variable {
    #[serde(rename = "type", default)]
    pub var_type: VariableType,
    #[serde(
        default,
        deserialize_with = "de_scalar_string",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(schema_with = "scalar_default_schema")]
    pub default: Option<String>,
    #[serde(flatten)]
    #[schemars(with = "BTreeMap<String, serde_json::Value>")]
    pub extra: BTreeMap<String, serde_yaml::Value>,
}

/// Accept a string, number or boolean default and keep it as its text form, so
/// `default: 5` and `default: "5"` mean the same thing.
fn de_scalar_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Scalar {
        Text(String),
        Int(i64),
        Float(f64),
        Bool(bool),
    }
    Ok(
        Option::<Scalar>::deserialize(deserializer)?.map(|s| match s {
            Scalar::Text(s) => s,
            Scalar::Int(n) => n.to_string(),
            Scalar::Float(n) => n.to_string(),
            Scalar::Bool(b) => b.to_string(),
        }),
    )
}

/// Who wrote a revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorKind {
    User,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Author {
    pub kind: AuthorKind,
    pub name: String,
}

impl Author {
    pub fn user(name: impl Into<String>) -> Self {
        Self {
            kind: AuthorKind::User,
            name: name.into(),
        }
    }

    pub fn agent(name: impl Into<String>) -> Self {
        Self {
            kind: AuthorKind::Agent,
            name: name.into(),
        }
    }
}

/// Why a definition cannot be saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    Command(String),
    Chain(String),
    InvalidId(String),
    EmptyName,
    EmptyTask,
    EmptyOwner,
    EmptyAgent,
    InvalidAgent(String),
    /// A team routine naming a personal agent: a teammate's Mac may run it.
    TeamRoutineNeedsTeamAgent(String),
    /// A team routine naming another team's agent.
    AgentOutsideTeam(String),
    /// A personal routine naming an agent of another org: a personal routine
    /// applies and runs in its own org only (SLW-PT-19).
    AgentOutsideOrg(String),
    InvalidRevision,
    RemoteMachineUnsupported(String),
    UnsupportedWhere(String),
    MaxRuntimeRequired,
    MaxRuntimeInvalid(String),
    MaxRuntimeOutOfRange(String),
    /// A `policy` wake limit that cannot be used (SW-AGR-13); the message
    /// says which and why.
    WakeLimit(String),
    InvalidSchedule(ScheduleError),
    UnsupportedEvent(String),
    InvalidMetadata(String),
    /// `from_agent` on an event kind that takes no agent filter.
    FromAgentNeedsAgentDone(String),
    RoleNeedsRolePending,
    EmptyEventRole,
    InvalidVariableName(String),
    VariableNeedsDefault(String),
    VariableDefaultMismatch(String),
    UnknownToken(String),
    /// Routines never deliver results; the agent running one sends or files
    /// whatever it needs itself. Saving a `deliver` key would read as a promise
    /// the app does not keep, so it is refused rather than carried along.
    DeliverUnsupported,
    ResultSchema(String),
    UnknownRunsOn(String),
    /// A personal routine runs on its own Mac; only a team routine can be a
    /// recipe each Mac runs.
    EachMacNeedsTeam,
    UnknownMode(String),
    /// `mode: wake` before this build can deliver one (SW-AGR-2).
    WakeNotAvailable,
    /// `mode: wake` on a starter: a starter's sessions are the ones people
    /// open by hand, so a routine on one only ever starts a fresh instance.
    WakeNeedsSavedAgent(String),
    /// `runs_on: each_mac` with a wake aimed at a machine other than each
    /// Mac's own (§9.9): every Mac would wake the same remote session.
    EachMacWakeNeedsLocal,
    /// `where.machine` is not `local`, `any` or a machine id.
    InvalidMachine(String),
    /// SW-AGR-8: what a routine armed on a machine cannot be.
    MachineArm(String),
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Command(message) | Self::Chain(message) => f.write_str(message),
            Self::InvalidId(id) => write!(
                f,
                "routine id {id:?} must be 1-64 lowercase letters, digits, '-' or '_', starting with a letter or digit"
            ),
            Self::EmptyName => f.write_str("routine name is required"),
            Self::EmptyTask => f.write_str("routine task is required"),
            Self::EmptyOwner => f.write_str("routine owner is required"),
            Self::EmptyAgent => f.write_str("routine agent is required: name an agent (see agent_list)"),
            Self::InvalidAgent(raw) => write!(
                f,
                "agent {raw:?} is not an agent id or key; see agent_list"
            ),
            Self::TeamRoutineNeedsTeamAgent(raw) => write!(
                f,
                "agent {raw:?} is a personal agent, which only its owner's Mac has: \
                 a team routine needs a team agent; any teammate may run it"
            ),
            Self::AgentOutsideTeam(raw) => {
                let team = AgentKey::parse(raw)
                    .map(|key| format!("{}/{}", key.org, key.team))
                    .unwrap_or_else(|| raw.clone());
                write!(
                    f,
                    "agent {raw:?} lives in team {team}, which the Mac of a teammate running \
                     this team routine may not have: a team routine runs one of its own team's agents"
                )
            }
            Self::AgentOutsideOrg(raw) => write!(
                f,
                "agent {raw:?} belongs to another org: an Only me routine runs agents of its own org only"
            ),
            Self::InvalidRevision => f.write_str("routine revision must be 1 or more"),
            Self::RemoteMachineUnsupported(kind) => write!(
                f,
                "where.kind {kind:?} is not supported yet: remote machines arrive in a later release"
            ),
            Self::UnsupportedWhere(kind) => write!(
                f,
                "where.kind {kind:?} must be team_workspace or repo:<owner>/<repo>"
            ),
            Self::MaxRuntimeRequired => f.write_str("policy.max_runtime is required (e.g. 30m)"),
            Self::MaxRuntimeInvalid(v) => write!(
                f,
                "policy.max_runtime {v:?} must be written like 30m or 2h"
            ),
            Self::MaxRuntimeOutOfRange(v) => write!(
                f,
                "policy.max_runtime {v:?} must be between 1m and 24h"
            ),
            Self::WakeLimit(why) => f.write_str(why),
            Self::InvalidSchedule(why) => write!(f, "invalid schedule trigger: {why}"),
            Self::InvalidMetadata(why) => f.write_str(why),
            Self::UnsupportedEvent(kind) => write!(
                f,
                "event trigger kind {kind:?} is not supported: use agent_done (an agent finished), inbox_idle (an agent sits idle with unread mail), role_pending (a message is queued for a role inbox) or metadata_change"
            ),
            Self::FromAgentNeedsAgentDone(kind) => write!(
                f,
                "event trigger kind {kind:?} takes no from_agent: only agent_done filters by agent"
            ),
            Self::RoleNeedsRolePending => f.write_str("role is only valid on role_pending events; omit it for other kinds"),
            Self::EmptyEventRole => f.write_str("role must not be empty; omit role to watch every queue in reach"),
            Self::InvalidVariableName(name) => write!(
                f,
                "variable name {name:?} must be letters, digits or '_' and must not shadow a built-in"
            ),
            Self::VariableNeedsDefault(name) => write!(
                f,
                "variable {name:?} needs a default because the routine has a schedule trigger"
            ),
            Self::VariableDefaultMismatch(name) => write!(
                f,
                "variable {name:?} has a default that does not match its type"
            ),
            Self::UnknownToken(token) => write!(
                f,
                "task token {{{{{token}}}}} is not a built-in, a declared variable, or (with an event trigger) a trigger field"
            ),
            Self::ResultSchema(message) => f.write_str(message),
            Self::DeliverUnsupported => f.write_str(
                "routines do not deliver results: remove `deliver` and have the task tell the agent where to post or file its output",
            ),
            Self::UnknownRunsOn(value) => write!(
                f,
                "runs_on {value:?} must be one_mac (runs once for the team) or each_mac (runs on each Mac that turns it on)"
            ),
            Self::EachMacNeedsTeam => f.write_str(
                "runs_on each_mac is for team routines: an Only me routine runs on its own Mac",
            ),
            Self::UnknownMode(value) => write!(
                f,
                "mode {value:?} must be wake (deliver into the agent's live session) or fresh (start a new one)"
            ),
            Self::WakeNotAvailable => f.write_str(
                "mode wake is not available yet: routines start a fresh session on every run until wake delivery ships",
            ),
            Self::EachMacWakeNeedsLocal => f.write_str(
                "runs_on each_mac only works with machine local: each teammate's Mac wakes or starts its own agent, never all of them the same one on another machine",
            ),
            Self::MachineArm(why) => write!(f, "a routine armed on a machine {why}"),
            Self::InvalidMachine(raw) => write!(
                f,
                "where.machine {raw:?} must be local, any, or a machine id"
            ),
            Self::WakeNeedsSavedAgent(agent) => write!(
                f,
                "mode wake needs a saved agent: {agent} is a built-in starter, whose routines always start fresh"
            ),
        }
    }
}

impl std::error::Error for ValidationError {}

/// Routine ids name a directory, so they are restricted to a path-safe slug.
pub(crate) fn is_valid_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    (1..=64).contains(&bytes.len())
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-' || *b == b'_')
}

impl RoutineDef {
    pub fn has_schedule(&self) -> bool {
        self.triggers
            .iter()
            .any(|t| matches!(t, RoutineTrigger::Schedule(_)))
    }

    pub fn is_archived(&self) -> bool {
        self.archived_at.is_some()
    }

    /// How this routine runs, or the error that rejects its `runs_on`.
    pub fn runs_on(&self) -> Result<RunsOn, ValidationError> {
        RunsOn::parse(&self.runs_on)
    }

    /// What a fire does, or the error that rejects its `mode`.
    pub fn mode(&self) -> Result<RoutineMode, ValidationError> {
        RoutineMode::parse(&self.mode)
    }

    /// Whether the routine at `key` may run the agent it names. Checked on
    /// every save, where the key is known; [`Self::validate`] checks only the
    /// shape of every agent reference. A starter's routines start fresh only.
    pub fn validate_agents_for(&self, key: &RoutineKey) -> Result<(), ValidationError> {
        if self.is_command() {
            if key.is_personal() {
                return Err(ValidationError::Command(
                    "Commands run in a team's workspace for now. Pick a team under Who can use it."
                        .into(),
                ));
            }
            return Ok(());
        }
        let agent = self.agent.key_for(key)?;
        if agent.is_builtin() && self.mode()? == RoutineMode::Wake {
            return Err(ValidationError::WakeNeedsSavedAgent(agent.to_string()));
        }
        Ok(())
    }

    /// Where a wake looks (§4, §9.9): `where.machine` is `local`, `any` or a
    /// machine id, and a recipe each Mac runs wakes only that Mac's own agent:
    /// aimed at another machine (or any), every Mac would wake the same one.
    pub fn validate_wake_target(&self) -> Result<(), ValidationError> {
        let machine = self.target.machine.as_deref().map(str::trim);
        if let Some(raw) = machine {
            if raw.is_empty() || raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(ValidationError::InvalidMachine(raw.to_string()));
            }
        }
        let local = matches!(machine, None | Some("local"));
        // A recipe each Mac runs, aimed at one other machine (or any), would
        // have every Mac wake, or start, the same agent there (SW-AGR-2b:
        // whatever the mode). A fresh run on another machine launches there.
        // A fresh run with `any` starts on each Mac itself, as before.
        let fresh_any = machine == Some("any") && self.mode.trim() != MODE_WAKE;
        if !local && !fresh_any && self.runs_on() == Ok(RunsOn::EachMac) {
            return Err(ValidationError::EachMacWakeNeedsLocal);
        }
        Ok(())
    }

    pub fn is_command(&self) -> bool {
        self.action == RoutineAction::Command
    }

    /// Every check a save runs. Returns the first problem found.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if let Some(cap) = self.policy.budget_usd {
            cap.validate().map_err(ValidationError::WakeLimit)?;
            if self.is_command() {
                return Err(ValidationError::WakeLimit(
                    "command routines cannot have a spend cap".into(),
                ));
            }
            if self.mode.trim() == "wake" {
                return Err(ValidationError::WakeLimit(
                    "wake routines cannot have a spend cap; cap the agent".into(),
                ));
            }
            if self.armed_machine().is_some() {
                return Err(ValidationError::WakeLimit(
                    "machine-armed routines cannot have a spend cap yet".into(),
                ));
            }
        }
        if !is_valid_id(&self.id) {
            return Err(ValidationError::InvalidId(self.id.clone()));
        }
        if self.name.trim().is_empty() {
            return Err(ValidationError::EmptyName);
        }
        if !self.is_command() && self.task.trim().is_empty() {
            return Err(ValidationError::EmptyTask);
        }
        if self.owner.trim().is_empty() {
            return Err(ValidationError::EmptyOwner);
        }
        if self.is_command() {
            if !self.agent.is_empty() || !self.task.is_empty() || !self.mode.is_empty() {
                return Err(ValidationError::Command(
                    "A command routine has no agent, task or mode; wake is not supported.".into(),
                ));
            }
            super::command::validate_program(
                self.command.first().map(String::as_str).unwrap_or_default(),
            )
            .map_err(ValidationError::Command)?;
            if self.armed_machine().is_some()
                || self.target.machine.as_deref().is_some_and(|m| m != "local")
            {
                return Err(ValidationError::Command(
                    "Command routines can't be armed on a machine yet; use This Mac.".into(),
                ));
            }
        } else {
            self.agent.validate()?;
            if !self.command.is_empty() {
                return Err(ValidationError::Command(
                    "An agent routine has no command.".into(),
                ));
            }
        }
        if let Some(after) = &self.after {
            if !is_valid_id(&after.routine) {
                return Err(ValidationError::Chain(
                    "After must name a routine id in the same space.".into(),
                ));
            }
            if self.runs_on()? == RunsOn::EachMac || self.armed_machine().is_some() {
                return Err(ValidationError::Chain("After a routine runs once for the team or personally; each Mac and armed machines support schedules only.".into()));
            }
        }
        if self.revision == 0 {
            return Err(ValidationError::InvalidRevision);
        }
        if self.extra.contains_key(DELIVER_KEY) {
            return Err(ValidationError::DeliverUnsupported);
        }
        if let Some(schema) = &self.result_schema {
            super::results::validate_schema(schema).map_err(ValidationError::ResultSchema)?;
        }
        self.target.resolve()?;
        self.runs_on()?;
        self.mode()?;
        self.policy.max_runtime_duration()?;
        self.policy.validate_wake_limits()?;
        for trigger in &self.triggers {
            validate_trigger(trigger)?;
        }
        if self.runs_on()? == RunsOn::EachMac
            && self
                .triggers
                .iter()
                .any(|t| matches!(t, RoutineTrigger::Event(e) if e.kind == EVENT_METADATA_CHANGE))
        {
            return Err(ValidationError::InvalidMetadata("Metadata triggers run once for the team or personally; each_mac supports schedules only for metadata routines".into()));
        }
        self.validate_machine_arm()?;
        self.validate_variables()?;
        if self.is_command() {
            Ok(())
        } else {
            self.validate_template()
        }
    }

    /// The machine whose session server arms this routine, if one does.
    pub fn armed_machine(&self) -> Option<&str> {
        self.armed_machine
            .as_deref()
            .map(str::trim)
            .filter(|machine| !machine.is_empty())
    }

    /// SW-AGR-8: a routine armed on a machine runs there on its schedule, as
    /// a shared job, in a session on that machine.
    pub fn validate_machine_arm(&self) -> Result<(), ValidationError> {
        if self.armed_machine().is_some() && self.policy.budget_usd.is_some() {
            return Err(ValidationError::MachineArm(
                "a capped routine cannot be armed on a machine yet".into(),
            ));
        }
        let Some(machine) = self.armed_machine() else {
            return Ok(());
        };
        let refuse = |why: &str| Err(ValidationError::MachineArm(why.to_string()));
        if matches!(machine, "local" | "any")
            || machine.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return refuse("names one machine by its id (not local or any)");
        }
        if self.runs_on() == Ok(RunsOn::EachMac) {
            return refuse("runs once for the team: it cannot be each_mac");
        }
        if !self.has_schedule() {
            return refuse("needs a schedule: the machine fires it on its own clock");
        }
        if self
            .triggers
            .iter()
            .any(|t| !matches!(t, RoutineTrigger::Schedule(_)))
        {
            return refuse(
                "runs on its schedule only: agent_done, inbox_idle and role_pending need a desktop",
            );
        }
        if self
            .target
            .machine
            .as_deref()
            .map(str::trim)
            .is_some_and(|target| target != machine)
        {
            return refuse("runs its agent on that machine: where.machine must name it too");
        }
        if self.arm_epoch == 0 {
            return refuse("needs an arm epoch (move it with routine_arm_on)");
        }
        // The machine publishes no structured result yet: that needs
        // routine_publish_result, which only a desktop answers.
        if self.result_schema.is_some() {
            return refuse("cannot have a result schema yet: publishing a result needs a desktop");
        }
        // Everything the task names must render: the machine renders the
        // built-ins, the desktop the declared variables (from their defaults).
        let declared: BTreeSet<&str> = self
            .variables
            .iter()
            .filter(|(_, v)| v.default.is_some())
            .map(|(name, _)| name.as_str())
            .collect();
        for token in template::tokens(&self.task) {
            let renders = match &token {
                template::Token::Builtin(_) => true,
                template::Token::Name(name) => declared.contains(name.as_str()),
                template::Token::Trigger(_) | template::Token::Malformed(_) => false,
            };
            if !renders {
                return Err(ValidationError::MachineArm(format!(
                    "cannot render {} on the machine: only the built-ins and variables with defaults render there",
                    token.text()
                )));
            }
        }
        Ok(())
    }

    fn validate_variables(&self) -> Result<(), ValidationError> {
        let needs_defaults = self.has_schedule();
        for (name, var) in &self.variables {
            if !template::is_identifier(name) || template::is_reserved_name(name) {
                return Err(ValidationError::InvalidVariableName(name.clone()));
            }
            if needs_defaults && var.default.is_none() {
                return Err(ValidationError::VariableNeedsDefault(name.clone()));
            }
            if let Some(default) = &var.default {
                let fits = match var.var_type {
                    VariableType::Text => true,
                    VariableType::Number => default.trim().parse::<f64>().is_ok_and(f64::is_finite),
                    VariableType::Boolean => matches!(default.trim(), "true" | "false"),
                };
                if !fits {
                    return Err(ValidationError::VariableDefaultMismatch(name.clone()));
                }
            }
        }
        Ok(())
    }

    fn validate_template(&self) -> Result<(), ValidationError> {
        let declared: BTreeSet<&str> = self.variables.keys().map(String::as_str).collect();
        // A trigger payload only exists when an event fired the run.
        let has_event = self.after.is_some()
            || self
                .triggers
                .iter()
                .any(|t| matches!(t, RoutineTrigger::Event(_)));
        for token in template::tokens(&self.task) {
            let known = match &token {
                template::Token::Builtin(_) => true,
                template::Token::Trigger(_) => has_event,
                template::Token::Name(name) => declared.contains(name.as_str()),
                template::Token::Malformed(_) => false,
            };
            if !known {
                return Err(ValidationError::UnknownToken(token.text()));
            }
        }
        Ok(())
    }
}

pub(crate) fn validate_trigger(trigger: &RoutineTrigger) -> Result<(), ValidationError> {
    use crate::context_query::{engine::normalize_key, model::FilterOp};
    if let RoutineTrigger::Event(spec) = trigger {
        if let Some(role) = &spec.role {
            if spec.kind.trim() != EVENT_ROLE_PENDING {
                return Err(ValidationError::RoleNeedsRolePending);
            }
            if crate::weft::role_inbox::role_name(role).is_empty() {
                return Err(ValidationError::EmptyEventRole);
            }
        }
        if spec.kind.trim() == EVENT_METADATA_CHANGE {
            if spec.from_agent.is_some() {
                return Err(ValidationError::FromAgentNeedsAgentDone(spec.kind.clone()));
            }
            let invalid = |message: &str| ValidationError::InvalidMetadata(message.into());
            let condition = spec.metadata.as_ref().ok_or_else(|| {
                invalid(
                    "metadata_change needs metadata with selection, target and optional previous",
                )
            })?;
            let valid = |clause: &crate::context_query::model::FilterClause| {
                clause.op == FilterOp::Eq
                    && !clause.key.trim().is_empty()
                    && clause
                        .value
                        .as_ref()
                        .is_some_and(|v| v.is_string() || v.is_number() || v.is_boolean())
                    && clause.min.is_none()
                    && clause.max.is_none()
            };
            if !valid(&condition.target) {
                return Err(invalid(
                    "Metadata target needs a field and an eq scalar value",
                ));
            }
            if let Some(previous) = &condition.previous {
                if !valid(previous)
                    || normalize_key(&previous.key) != normalize_key(&condition.target.key)
                {
                    return Err(invalid(
                        "Metadata previous must be an eq clause on the target field",
                    ));
                }
            }
            if condition.selection.text.is_some() {
                return Err(invalid("Metadata selection does not support body text"));
            }
            return Ok(());
        }
        if spec.metadata.is_some() {
            return Err(ValidationError::InvalidMetadata(
                "metadata is only valid on metadata_change events".into(),
            ));
        }
    }
    match trigger {
        RoutineTrigger::Schedule(spec) => {
            schedule::validate(&spec.cron, &spec.tz).map_err(ValidationError::InvalidSchedule)
        }
        RoutineTrigger::Event(spec) if spec.kind.trim() == EVENT_AGENT_DONE => {
            spec.from_agent.as_ref().map_or(Ok(()), AgentRef::validate)
        }
        RoutineTrigger::Event(spec)
            if spec.kind.trim() == EVENT_INBOX_IDLE || spec.kind.trim() == EVENT_ROLE_PENDING =>
        {
            match spec.from_agent {
                Some(_) => Err(ValidationError::FromAgentNeedsAgentDone(spec.kind.clone())),
                None => Ok(()),
            }
        }
        RoutineTrigger::Event(spec) => Err(ValidationError::UnsupportedEvent(spec.kind.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_definition_needs_no_agent_task_or_mode() {
        let def: RoutineDef = serde_yaml::from_str(
            r#"id: sync
name: Sync
owner: alice
action: command
command: [python3, scripts/sync.py, '{{itemId}}']
where: {kind: team_workspace}
policy: {max_runtime: 5m}
revision: 1
"#,
        )
        .unwrap();
        def.validate().unwrap();
        let encoded = serde_yaml::to_string(&def).unwrap();
        assert!(!encoded.contains("agent:"));
        assert!(!encoded.contains("task:"));
        assert!(!encoded.contains("mode:"));
    }

    fn sample_yaml() -> &'static str {
        r#"id: slack-triage
name: Slack triage
owner: "42"
agent: triage
task: |
  Sweep #{{channel}} since {{last_run_at}}.
  Previous: {{last_run_summary}} on {{date}} by {{trigger.from}}
where:
  kind: team_workspace
  future_where_key: kept
triggers:
  - schedule:
      cron: 0 9 * * 1-5
      tz: America/Denver
  - event:
      kind: agent_done
      from_agent: reviewer
policy:
  max_runtime: 30m
  future_policy_key: 3
variables:
  channel:
    type: text
    default: all-codesense
revision: 3
armed_host: host-a
future_top_key:
  nested: true
"#
    }

    fn parse(yaml: &str) -> RoutineDef {
        serde_yaml::from_str(yaml).expect("parses")
    }

    #[test]
    fn budgets_refuse_wakes_commands_and_machine_arms() {
        use crate::session_usage::spend_caps::Budget;
        let mut def = valid();
        def.policy.budget_usd = Some(Budget {
            per_day: Some(1.0),
            per_week: None,
        });
        def.mode = "wake".into();
        assert!(def
            .validate()
            .unwrap_err()
            .to_string()
            .contains("cap the agent"));
        def.mode = "fresh".into();
        def.action = RoutineAction::Command;
        assert!(def
            .validate()
            .unwrap_err()
            .to_string()
            .contains("command routines"));
        def.action = RoutineAction::Agent;
        def.armed_machine = Some("test-host".into());
        assert!(def
            .validate()
            .unwrap_err()
            .to_string()
            .contains("cannot have a spend cap yet"));
        assert!(def
            .validate_machine_arm()
            .unwrap_err()
            .to_string()
            .contains("cannot be armed on a machine yet"));
    }

    fn valid() -> RoutineDef {
        parse(sample_yaml())
    }

    #[test]
    fn schema_round_trips_with_unknown_keys_and_defaults() {
        let def = valid();
        assert_eq!(def.agent, AgentRef::from("triage"));
        assert_eq!(def.policy.overlap, OverlapPolicy::Coalesce);
        assert_eq!(def.policy.missed, MissedPolicy::Skip);
        assert!(!def.policy.visible);
        assert_eq!(def.triggers.len(), 2);
        assert!(matches!(
            &def.triggers[0],
            RoutineTrigger::Schedule(s) if s.cron == "0 9 * * 1-5" && s.tz == "America/Denver"
        ));
        assert!(def.extra.contains_key("future_top_key"));
        assert!(def.target.extra.contains_key("future_where_key"));
        assert!(def.policy.extra.contains_key("future_policy_key"));
        def.validate().expect("sample is valid");

        let yaml = serde_yaml::to_string(&def).unwrap();
        // Triggers stay in the single-key map form, not YAML tags.
        assert!(yaml.contains("- schedule:"), "{yaml}");
        assert!(!yaml.contains("!schedule"), "{yaml}");
        let back: RoutineDef = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back, def);
    }

    /// SW-AGR-1's shape, exactly as a save writes it: `mode`, `where.machine`
    /// and `armed_machine` beside the fields every older client knows, no
    /// `agent:` (the path names it), and the migration record.
    const GOLDEN_AGENT_ROUTINE: &str = r#"id: triage
name: Triage
owner: '1'
mode: wake
task: Sweep the inbox
where:
  kind: team_workspace
  machine: m-7f3a
triggers:
- schedule:
    cron: 0 9 * * 1-5
    tz: America/Denver
policy:
  overlap: coalesce
  missed: skip
  max_runtime: 30m
  visible: false
revision: 4
armed_host: host-a
armed_machine: m-7f3a
arm_epoch: 3
handover_from:
  machine: m-old
  epoch: 2
runs_on: one_mac
archived_at: 2026-10-05T09:00:00Z
archived_with_agent: true
migrated:
  from: routines/triage
  note: Renamed from triage
future_top_key: kept
"#;

    #[test]
    fn golden_agent_routine_round_trips_byte_for_byte() {
        let def = parse(GOLDEN_AGENT_ROUTINE);
        assert!(def.agent.is_empty(), "the path names the agent");
        assert_eq!(def.mode().unwrap(), RoutineMode::Wake);
        assert_eq!(def.target.machine.as_deref(), Some("m-7f3a"));
        assert_eq!(def.armed_machine.as_deref(), Some("m-7f3a"));
        assert_eq!(def.arm_epoch, 3, "SW-AGR-8: the arm epoch is typed");
        assert_eq!(
            def.handover_from,
            Some(HandoverFrom {
                machine: "m-old".into(),
                epoch: 2
            }),
            "SW-AGR-8: a pending handover is typed"
        );
        assert!(def.archived_with_agent);
        assert_eq!(def.migrated.as_ref().unwrap().from, "routines/triage");
        assert!(def.extra.contains_key("future_top_key"));
        assert!(def.target.extra.is_empty(), "machine is typed, not extra");
        assert_eq!(serde_yaml::to_string(&def).unwrap(), GOLDEN_AGENT_ROUTINE);
    }

    #[test]
    fn a_legacy_file_still_parses_with_its_agent_and_no_mode() {
        let def = valid();
        assert_eq!(def.agent, AgentRef::from("triage"));
        assert!(def.mode.is_empty());
        assert_eq!(
            def.mode().unwrap(),
            RoutineMode::Fresh,
            "it runs as it always did"
        );
        assert_eq!(def.target.machine, None);
        assert_eq!(def.armed_machine, None);
        let yaml = serde_yaml::to_string(&def).unwrap();
        assert!(yaml.contains("agent: triage"), "{yaml}");
        for absent in [
            "mode:",
            "machine:",
            "armed_machine:",
            "arm_epoch:",
            "handover_from:",
            "migrated:",
            "archived_with_agent:",
        ] {
            assert!(!yaml.contains(absent), "{absent} in {yaml}");
        }
        assert_eq!(parse(&yaml), def);
    }

    #[test]
    fn mode_is_wake_or_fresh_and_a_starter_only_starts_fresh() {
        let mut def = valid();
        def.mode = "nudge".into();
        assert_eq!(
            def.validate(),
            Err(ValidationError::UnknownMode("nudge".into()))
        );
        def.mode = "wake".into();
        def.validate().unwrap();
        let key = RoutineKey::new("acme/api", "backend", "slack-triage");
        def.validate_agents_for(&key).unwrap();
        def.agent = AgentRef::from("builtin/claude");
        assert_eq!(
            def.validate_agents_for(&key),
            Err(ValidationError::WakeNeedsSavedAgent(
                "builtin/claude".into()
            ))
        );
        def.mode = "fresh".into();
        def.validate_agents_for(&key).unwrap();
    }

    #[test]
    fn a_deliver_key_parses_but_is_refused_at_save() {
        let yaml = format!("{}deliver:\n  - inbox\n", sample_yaml());
        let def = parse(&yaml);
        assert!(def.extra.contains_key("deliver"));
        assert_eq!(def.validate(), Err(ValidationError::DeliverUnsupported));
    }

    #[test]
    fn json_wire_form_matches_yaml_shape() {
        let def = valid();
        let json = serde_json::to_value(&def).unwrap();
        assert_eq!(json["where"]["kind"], "team_workspace");
        assert_eq!(json["triggers"][0]["schedule"]["tz"], "America/Denver");
        let back: RoutineDef = serde_json::from_value(json).unwrap();
        assert_eq!(back.triggers, def.triggers);
    }

    #[test]
    fn omitted_optionals_take_defaults() {
        let def = parse(
            "id: a\nname: A\nowner: '1'\nagent: x\ntask: t\nwhere: {kind: team_workspace}\npolicy: {max_runtime: 1m, overlap: queue, missed: run_once}\nrevision: 1\narmed_host: h\n",
        );
        assert!(def.triggers.is_empty(), "no triggers = manual only");
        assert_eq!(def.policy.overlap, OverlapPolicy::Queue);
        assert_eq!(def.policy.missed, MissedPolicy::RunOnce);
        def.validate().unwrap();
    }

    #[test]
    fn profile_style_agent_forms_are_ordinary_invalid_agents() {
        let yaml = sample_yaml().replace("agent: triage", "agent: {pack: codex, id: default}");
        assert!(serde_yaml::from_str::<RoutineDef>(&yaml).is_err());
        let def = parse(&sample_yaml().replace("agent: triage", "agent: claude:default"));
        assert_eq!(
            def.validate(),
            Err(ValidationError::InvalidAgent("claude:default".into()))
        );
        let def = parse(&sample_yaml().replace("agent: triage", "agent: acme:x/core/triage"));
        assert_eq!(
            def.validate(),
            Err(ValidationError::InvalidAgent("acme:x/core/triage".into()))
        );
        let mut def = parse(sample_yaml());
        def.triggers = vec![RoutineTrigger::Event(EventSpec {
            kind: EVENT_AGENT_DONE.into(),
            from_agent: Some("codex:reviewer".into()),
            role: None,
            metadata: None,
        })];
        assert_eq!(
            def.validate(),
            Err(ValidationError::InvalidAgent("codex:reviewer".into()))
        );
    }

    #[test]
    fn an_agent_ref_names_an_agent_in_the_routines_own_space_or_by_key() {
        let team = RoutineKey::new("acme/api", "backend", "sweep");
        let mine = RoutineKey::personal(&Principal::from(1u64), "acme/api", "sweep");
        let team_agent = AgentKey::new("acme/api", "backend", "triage");
        let my_agent = AgentKey::personal(&Principal::from(1u64), "acme/api", "triage");
        let bobs_agent = AgentKey::personal(&Principal::from(2u64), "acme/api", "triage");
        let other_team = AgentKey::new("acme/api", "frontend", "triage");

        // A bare id is the agent in the routine's own space.
        assert_eq!(
            AgentRef::from("triage").key_for(&team),
            Ok(team_agent.clone())
        );
        assert_eq!(
            AgentRef::from("triage").key_for(&mine),
            Ok(my_agent.clone())
        );
        // A key names a team agent from a personal routine, and the routine's
        // own team agent from a team one.
        for routine in [&team, &mine] {
            assert_eq!(
                AgentRef::from("acme/api/backend/triage").key_for(routine),
                Ok(team_agent.clone())
            );
        }
        assert_eq!(
            AgentRef::from(" _personal/1/acme/api/_scratch/triage ").key_for(&mine),
            Ok(my_agent.clone())
        );
        // A team routine may run on any teammate's Mac, so never a personal agent,
        // and it names only its own team's.
        assert_eq!(
            AgentRef::from("_personal/1/acme/api/_scratch/triage").key_for(&team),
            Err(ValidationError::TeamRoutineNeedsTeamAgent(
                "_personal/1/acme/api/_scratch/triage".into()
            ))
        );
        assert!(AgentRef::from("_personal/1/acme/api/_scratch/triage")
            .key_for(&team)
            .unwrap_err()
            .to_string()
            .contains("a team routine needs a team agent; any teammate may run it"));
        assert_eq!(
            AgentRef::from("acme/api/frontend/triage").key_for(&team),
            Err(ValidationError::AgentOutsideTeam(
                "acme/api/frontend/triage".into()
            ))
        );
        // Said as why, not as a refusal: the agent may not be on the Mac that
        // runs the routine.
        assert!(AgentRef::from("acme/api/frontend/triage")
            .key_for(&team)
            .unwrap_err()
            .to_string()
            .contains("lives in team acme/api/frontend, which the Mac of a teammate running this team routine may not have"));
        assert_eq!(
            AgentRef::from("acme/api/frontend/triage").key_for(&mine),
            Ok(other_team.clone())
        );
        // Only to run one: a filter that waits for an agent may name any.
        assert_eq!(
            AgentRef::from("acme/api/frontend/triage").named_key(&team),
            Ok(other_team.clone())
        );
        assert_eq!(
            AgentRef::from("_personal/1/acme/api/_scratch/triage").named_key(&team),
            Ok(my_agent.clone())
        );
        // Malformed references.
        for bad in [
            "Triage",
            "a b",
            "x/y",
            "acme/api/backend/Bad",
            "_personal/x/acme/api/_scratch/t",
        ] {
            assert_eq!(
                AgentRef::from(bad).key_for(&team),
                Err(ValidationError::InvalidAgent(bad.into())),
                "{bad}"
            );
        }
        assert_eq!(
            AgentRef::from("  ").key_for(&team),
            Err(ValidationError::EmptyAgent)
        );
        // Written back the shortest way that names the same agent.
        assert_eq!(AgentRef::to_agent(&team_agent, &team).as_str(), "triage");
        assert_eq!(AgentRef::to_agent(&my_agent, &mine).as_str(), "triage");
        assert_eq!(
            AgentRef::to_agent(&team_agent, &mine).as_str(),
            "acme/api/backend/triage"
        );
        assert_eq!(
            AgentRef::to_agent(&my_agent, &team).as_str(),
            "_personal/1/acme/api/_scratch/triage"
        );
        assert_eq!(
            AgentRef::to_agent(&bobs_agent, &mine).as_str(),
            "_personal/2/acme/api/_scratch/triage"
        );
        // On the wire it is the string as written.
        assert_eq!(
            serde_json::to_value(AgentRef::from("acme/api/backend/triage")).unwrap(),
            serde_json::json!("acme/api/backend/triage")
        );
    }

    #[test]
    fn where_accepts_repo_and_rejects_machine_and_unknown() {
        let mut def = valid();
        def.target.kind = "repo:alderco/codesense".into();
        assert_eq!(
            def.target.resolve(),
            Ok(RoutineTarget::Repo("alderco/codesense".into()))
        );
        def.validate().unwrap();

        def.target.kind = "machine:studio".into();
        let err = def.validate().unwrap_err();
        assert!(matches!(err, ValidationError::RemoteMachineUnsupported(_)));
        assert!(err
            .to_string()
            .contains("remote machines arrive in a later release"));

        def.target.kind = "repo:not-a-slug".into();
        assert!(matches!(
            def.validate(),
            Err(ValidationError::UnsupportedWhere(_))
        ));
        def.target.kind = "somewhere".into();
        assert!(matches!(
            def.validate(),
            Err(ValidationError::UnsupportedWhere(_))
        ));
    }

    #[test]
    fn parse_max_runtime_accepts_valid_minute_and_hour_strings() {
        assert_eq!(parse_max_runtime("1m"), Ok(Duration::from_secs(60)));
        assert_eq!(parse_max_runtime("30m"), Ok(Duration::from_secs(1800)));
        assert_eq!(parse_max_runtime("24h"), Ok(Duration::from_secs(86_400)));
    }

    #[test]
    fn parse_max_runtime_refuses_out_of_range_minutes_and_hours() {
        for out in ["0m", "25h", "1441m"] {
            assert_eq!(
                parse_max_runtime(out),
                Err(ValidationError::MaxRuntimeOutOfRange(out.into())),
                "{out}"
            );
        }
    }

    #[test]
    fn parse_max_runtime_refuses_unparseable_strings() {
        for bad in ["30s", "abc", "", "m", "1.5h", "30"] {
            assert_eq!(
                parse_max_runtime(bad),
                Err(ValidationError::MaxRuntimeInvalid(bad.into())),
                "{bad}"
            );
        }
    }

    #[test]
    fn max_runtime_is_required_and_bounded() {
        let mut def = valid();
        def.policy.max_runtime = None;
        assert_eq!(def.validate(), Err(ValidationError::MaxRuntimeRequired));
        for bad in ["30", "m", "30s", "1.5h", "abc", "-5m", "5µ", "30分"] {
            def.policy.max_runtime = Some(bad.into());
            assert_eq!(
                def.validate(),
                Err(ValidationError::MaxRuntimeInvalid(bad.into())),
                "{bad}"
            );
        }
        for out in ["0m", "25h", "1441m"] {
            def.policy.max_runtime = Some(out.into());
            assert_eq!(
                def.validate(),
                Err(ValidationError::MaxRuntimeOutOfRange(out.into())),
                "{out}"
            );
        }
        for ok in ["1m", "30m", "2h", "24h", "1440m"] {
            def.policy.max_runtime = Some(ok.into());
            def.validate().unwrap();
        }
        def.policy.max_runtime = Some("2h".into());
        assert_eq!(
            def.policy.max_runtime_duration(),
            Ok(Duration::from_secs(7200))
        );
    }

    // SW-AGR-13: the wake limits are optional policy keys, round-trip as
    // written (absent stays absent), and are validated on save.
    #[test]
    fn wake_limits_are_optional_policy_keys_and_validated() {
        let def = valid();
        assert!(!def.policy.has_wake_limits());
        assert!(!serde_yaml::to_string(&def).unwrap().contains("max_wakes"));
        let yaml = sample_yaml().replace(
            "max_runtime: 30m",
            "max_runtime: 30m\n  max_wakes: 40\n  max_wakes_per_day: 6\n  min_gap: 2h",
        );
        let limited: RoutineDef = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(limited.policy.max_wakes, Some(40));
        assert_eq!(limited.policy.max_wakes_per_day, Some(6));
        assert_eq!(limited.policy.min_gap.as_deref(), Some("2h"));
        assert!(
            ["max_wakes", "max_wakes_per_day", "min_gap"]
                .iter()
                .all(|k| !limited.policy.extra.contains_key(*k)),
            "known keys, not extra"
        );
        assert!(limited.policy.has_wake_limits());
        let again: RoutineDef =
            serde_yaml::from_str(&serde_yaml::to_string(&limited).unwrap()).unwrap();
        assert_eq!(again, limited);
        assert_eq!(
            limited.policy.min_gap_duration(),
            Ok(Some(Duration::from_secs(7200)))
        );
        let mut def = valid();
        def.policy.max_wakes = Some(0);
        assert!(
            matches!(def.validate(), Err(ValidationError::WakeLimit(m)) if m.contains("max_wakes"))
        );
        def.policy.max_wakes = None;
        def.policy.max_wakes_per_day = Some(0);
        assert!(
            matches!(def.validate(), Err(ValidationError::WakeLimit(m)) if m.contains("max_wakes_per_day"))
        );
        def.policy.max_wakes_per_day = Some(500);
        for bad in ["2", "30s", "8d", "0m", "1.5h", "d"] {
            def.policy.min_gap = Some(bad.into());
            assert!(
                matches!(def.validate(), Err(ValidationError::WakeLimit(m)) if m.contains("min_gap")),
                "{bad}"
            );
        }
        for ok in ["1m", "45m", "2h", "1d", "7d"] {
            def.policy.min_gap = Some(ok.into());
            def.validate().unwrap();
        }
    }

    #[test]
    fn runs_on_defaults_to_one_mac_and_rejects_unknown_values() {
        let mut def = valid();
        assert_eq!(def.runs_on(), Ok(RunsOn::OneMac), "absent is one_mac");
        assert!(!serde_yaml::to_string(&def).unwrap().contains("runs_on"));
        for (raw, mode) in [("one_mac", RunsOn::OneMac), ("each_mac", RunsOn::EachMac)] {
            def.runs_on = raw.into();
            assert_eq!(def.runs_on(), Ok(mode));
            def.validate().unwrap();
            let yaml = serde_yaml::to_string(&def).unwrap();
            assert!(yaml.contains(&format!("runs_on: {raw}")), "{yaml}");
            assert_eq!(serde_yaml::from_str::<RoutineDef>(&yaml).unwrap(), def);
        }
        for bad in ["every_mac", "EachMac", "all"] {
            def.runs_on = bad.into();
            assert_eq!(
                def.validate(),
                Err(ValidationError::UnknownRunsOn(bad.into())),
                "{bad}"
            );
        }
    }

    #[test]
    fn bad_overlap_value_fails_to_parse() {
        let yaml = sample_yaml().replace("max_runtime: 30m", "max_runtime: 30m\n  overlap: pile");
        assert!(serde_yaml::from_str::<RoutineDef>(&yaml).is_err());
    }

    #[test]
    fn triggers_reject_bad_cron_missing_tz_and_other_events() {
        let mut def = valid();
        def.triggers = vec![RoutineTrigger::Schedule(ScheduleSpec {
            cron: "0 9 * *".into(),
            tz: "UTC".into(),
        })];
        assert!(matches!(
            def.validate(),
            Err(ValidationError::InvalidSchedule(
                ScheduleError::InvalidCron { .. }
            ))
        ));
        let sched = |cron: &str, tz: &str| {
            vec![RoutineTrigger::Schedule(ScheduleSpec {
                cron: cron.into(),
                tz: tz.into(),
            })]
        };
        def.triggers = sched("0 9 * * *", " ");
        assert_eq!(
            def.validate(),
            Err(ValidationError::InvalidSchedule(ScheduleError::MissingZone))
        );
        def.triggers = sched("0 9 * * *", "America/Denvr");
        assert_eq!(
            def.validate(),
            Err(ValidationError::InvalidSchedule(
                ScheduleError::UnknownZone("America/Denvr".into())
            ))
        );
        def.triggers = sched("*/2 * * * *", "UTC");
        assert_eq!(
            def.validate(),
            Err(ValidationError::InvalidSchedule(
                ScheduleError::IntervalTooShort {
                    min: schedule::MIN_INTERVAL
                }
            ))
        );
        def.triggers = sched("0 9 1 * 1-5", "America/Denver");
        assert_eq!(
            def.validate(),
            Err(ValidationError::InvalidSchedule(
                ScheduleError::BothDayFields("0 9 1 * 1-5".into())
            ))
        );
        def.triggers = sched("0 9 * * 1-5", "America/Denver");
        assert!(!matches!(
            def.validate(),
            Err(ValidationError::InvalidSchedule(_))
        ));
        def.triggers = vec![RoutineTrigger::Event(EventSpec {
            kind: "queue_enqueued".into(),
            from_agent: None,
            role: None,
            metadata: None,
        })];
        assert_eq!(
            def.validate(),
            Err(ValidationError::UnsupportedEvent("queue_enqueued".into()))
        );
    }

    #[test]
    fn an_inbox_idle_trigger_parses_from_yaml_and_takes_no_agent_filter() {
        let yaml = r#"id: wake-idle-agents
name: Wake idle agents
owner: "42"
agent: builtin/claude-unattended
task: |
  Wake these sessions: {{trigger.sessions}} ({{trigger.count}})
where:
  kind: team_workspace
triggers:
  - event:
      kind: inbox_idle
policy:
  max_runtime: 5m
runs_on: each_mac
revision: 1
"#;
        let mut def = parse(yaml);
        assert_eq!(
            def.triggers,
            vec![RoutineTrigger::Event(EventSpec {
                kind: EVENT_INBOX_IDLE.into(),
                from_agent: None,
                role: None,
                metadata: None,
            })]
        );
        def.validate()
            .expect("an inbox_idle routine with trigger tokens is valid");
        let back: RoutineDef = serde_yaml::from_str(&serde_yaml::to_string(&def).unwrap()).unwrap();
        assert_eq!(back, def);

        def.triggers = vec![RoutineTrigger::Event(EventSpec {
            kind: EVENT_INBOX_IDLE.into(),
            from_agent: Some(AgentRef::from("reviewer")),
            role: None,
            metadata: None,
        })];
        assert_eq!(
            def.validate(),
            Err(ValidationError::FromAgentNeedsAgentDone(
                "inbox_idle".into()
            ))
        );
    }

    /// Role queues allow an optional role filter and never an agent filter.
    #[test]
    fn a_role_pending_trigger_parses_from_yaml_and_takes_no_agent_filter() {
        let yaml = r#"id: review-queue-sweep
name: Review queue sweep
owner: "1234567"
agent: builtin/claude-unattended
task: |
  The reviewer role queue has pending work.
where:
  kind: team_workspace
triggers:
  - event:
      kind: role_pending
  - schedule:
      cron: "0 */4 * * *"
      tz: UTC
mode: wake
policy:
  max_runtime: 10m
runs_on: each_mac
revision: 3
"#;
        let mut def = parse(yaml);
        assert!(
            def.triggers.contains(&RoutineTrigger::Event(EventSpec {
                kind: EVENT_ROLE_PENDING.into(),
                from_agent: None,
                role: None,
                metadata: None,
            })),
            "the role_pending trigger parses with no from_agent"
        );
        def.validate()
            .expect("a role_pending routine with trigger tokens is valid");
        let back: RoutineDef = serde_yaml::from_str(&serde_yaml::to_string(&def).unwrap()).unwrap();
        assert_eq!(back, def, "the role_pending trigger round-trips");

        def.triggers = vec![RoutineTrigger::Event(EventSpec {
            kind: EVENT_ROLE_PENDING.into(),
            from_agent: Some(AgentRef::from("ricky")),
            role: None,
            metadata: None,
        })];
        assert_eq!(
            def.validate(),
            Err(ValidationError::FromAgentNeedsAgentDone(
                "role_pending".into()
            )),
            "role_pending refuses a from_agent filter — only agent_done filters by agent"
        );
    }

    #[test]
    fn role_filters_share_yaml_validation_and_round_trip() {
        for (yaml, expected) in [
            ("event: {kind: role_pending, role: reviewer}", Ok(())),
            ("event: {kind: role_pending, role: 'role:reviewer'}", Ok(())),
            ("event: {kind: role_pending}", Ok(())),
            (
                "event: {kind: role_pending, role: ''}",
                Err(ValidationError::EmptyEventRole),
            ),
            (
                "event: {kind: role_pending, role: 'role:  '}",
                Err(ValidationError::EmptyEventRole),
            ),
            (
                "event: {kind: role_pending, role: '  '}",
                Err(ValidationError::EmptyEventRole),
            ),
            (
                "event: {kind: inbox_idle, role: reviewer}",
                Err(ValidationError::RoleNeedsRolePending),
            ),
            (
                "event: {kind: agent_done, role: reviewer}",
                Err(ValidationError::RoleNeedsRolePending),
            ),
            (
                "event: {kind: metadata_change, role: reviewer}",
                Err(ValidationError::RoleNeedsRolePending),
            ),
        ] {
            let mut document = serde_yaml::to_value(valid()).unwrap();
            document["triggers"] = serde_yaml::from_str(&format!("- {yaml}")).unwrap();
            let def: RoutineDef = serde_yaml::from_value(document).unwrap();
            assert_eq!(def.validate(), expected, "{yaml}");
            let back: RoutineDef =
                serde_yaml::from_str(&serde_yaml::to_string(&def).unwrap()).unwrap();
            assert_eq!(back, def);
        }
    }

    /// SW-AGR-14 AC-9: riverside's juniper/sasha/buck/… role-wake routines are
    /// bare `role_pending`-only — no schedule backstop. The engine must accept
    /// them: `has_schedule` only governs variable defaults, not whether an event
    /// trigger alone is enough to arm.
    #[test]
    fn a_bare_role_pending_routine_with_no_schedule_validates() {
        let yaml = r#"id: junipers-planner-wake
name: Juniper's planner wake
owner: "1234567"
agent: juniper
task: |
  The planner queue has a message.
where:
  kind: team_workspace
triggers:
  - event:
      kind: role_pending
mode: wake
policy:
  max_runtime: 15m
runs_on: each_mac
revision: 1
"#;
        let def = parse(yaml);
        def.validate()
            .expect("a bare role_pending-only routine is valid");
        assert!(
            !def.has_schedule(),
            "no schedule — the event alone fires it"
        );
    }

    /// SW-AGR-14 AC-5: role_pending can't fire on a remote session server (role
    /// queues live only on the desktop that holds the context repo). A routine
    /// armed on a machine is refused at save time if it has any desktop-only
    /// trigger: agent_done, inbox_idle or role_pending.
    #[test]
    fn a_machine_armed_routine_refuses_a_role_pending_trigger() {
        let mut def = valid();
        def.armed_machine = Some("m-7f3a".into());
        def.target.machine = Some("m-7f3a".into());
        def.arm_epoch = 1;
        def.runs_on = "one_mac".into();
        def.triggers = vec![
            RoutineTrigger::Schedule(ScheduleSpec {
                cron: "0 9 * * 1-5".into(),
                tz: "America/Denver".into(),
            }),
            RoutineTrigger::Event(EventSpec {
                kind: EVENT_ROLE_PENDING.into(),
                from_agent: None,
                role: None,
                metadata: None,
            }),
        ];
        let err = def
            .validate()
            .expect_err("machine-armed routines may only hold schedule triggers");
        match err {
            ValidationError::MachineArm(reason) => {
                assert!(reason.contains("schedule only"), "{reason}");
                assert!(reason.contains("role_pending"), "{reason}");
            }
            other => panic!("expected MachineArm, got {other:?}"),
        }
    }

    #[test]
    fn identity_fields_are_required() {
        let mut def = valid();
        def.id = "Bad Id".into();
        assert!(matches!(def.validate(), Err(ValidationError::InvalidId(_))));
        def.id = "../x".into();
        assert!(matches!(def.validate(), Err(ValidationError::InvalidId(_))));
        let mut def = valid();
        def.name = " ".into();
        assert_eq!(def.validate(), Err(ValidationError::EmptyName));
        let mut def = valid();
        def.task = String::new();
        assert_eq!(def.validate(), Err(ValidationError::EmptyTask));
        let mut def = valid();
        def.owner = String::new();
        assert_eq!(def.validate(), Err(ValidationError::EmptyOwner));
        let mut def = valid();
        def.agent = AgentRef::from("");
        assert_eq!(def.validate(), Err(ValidationError::EmptyAgent));
        // An unset armed_host is a valid placement: its creator's Mac arms it.
        let mut def = valid();
        def.armed_host = String::new();
        assert_eq!(def.validate(), Ok(()));
        let mut def = valid();
        def.revision = 0;
        assert_eq!(def.validate(), Err(ValidationError::InvalidRevision));
    }

    #[test]
    fn task_tokens_must_be_builtin_or_declared() {
        let mut def = valid();
        def.task = "hello {{nope}}".into();
        assert_eq!(
            def.validate(),
            Err(ValidationError::UnknownToken("nope".into()))
        );
        def.task = "hello {{ date }}".into();
        assert_eq!(
            def.validate(),
            Err(ValidationError::UnknownToken(" date ".into()))
        );
        def.task = "{{timestamp}} {{trigger.payload_key}} {{channel}}".into();
        def.validate().unwrap();
        // Without an event trigger there is no payload to fill a trigger field.
        def.triggers
            .retain(|t| matches!(t, RoutineTrigger::Schedule(_)));
        assert_eq!(
            def.validate(),
            Err(ValidationError::UnknownToken("trigger.payload_key".into()))
        );
    }

    #[test]
    fn typed_variable_defaults_must_parse() {
        let mut def = valid();
        let var = def.variables.get_mut("channel").unwrap();
        var.var_type = VariableType::Number;
        var.default = Some("lots".into());
        assert_eq!(
            def.validate(),
            Err(ValidationError::VariableDefaultMismatch("channel".into()))
        );
        for not_finite in ["NaN", "inf", "-infinity"] {
            def.variables.get_mut("channel").unwrap().default = Some(not_finite.into());
            assert_eq!(
                def.validate(),
                Err(ValidationError::VariableDefaultMismatch("channel".into())),
                "{not_finite}"
            );
        }
        def.variables.get_mut("channel").unwrap().default = Some("2.5".into());
        def.validate().unwrap();
        let var = def.variables.get_mut("channel").unwrap();
        var.var_type = VariableType::Boolean;
        var.default = Some("maybe".into());
        assert_eq!(
            def.validate(),
            Err(ValidationError::VariableDefaultMismatch("channel".into()))
        );
        def.variables.get_mut("channel").unwrap().default = Some("true".into());
        def.validate().unwrap();
    }

    #[test]
    fn schedules_require_variable_defaults() {
        let mut def = valid();
        def.variables.get_mut("channel").unwrap().default = None;
        assert_eq!(
            def.validate(),
            Err(ValidationError::VariableNeedsDefault("channel".into()))
        );
        // Without a schedule (here: event-only) a variable may lack a default.
        def.triggers
            .retain(|t| matches!(t, RoutineTrigger::Event(_)));
        def.validate().unwrap();
    }

    #[test]
    fn variable_names_cannot_shadow_builtins() {
        let mut def = valid();
        def.variables.insert("date".into(), Variable::default());
        def.triggers.clear();
        assert_eq!(
            def.validate(),
            Err(ValidationError::InvalidVariableName("date".into()))
        );
        def.variables.remove("date");
        def.variables.insert("bad-name".into(), Variable::default());
        assert_eq!(
            def.validate(),
            Err(ValidationError::InvalidVariableName("bad-name".into()))
        );
    }

    #[test]
    fn numeric_variable_default_is_kept_as_text() {
        let yaml = sample_yaml().replace("default: all-codesense", "default: 5");
        let def = parse(&yaml);
        assert_eq!(def.variables["channel"].default.as_deref(), Some("5"));
    }

    #[test]
    fn routine_key_renders_and_parses() {
        let key = RoutineKey::new("acme/api", "backend", "triage");
        assert_eq!(key.to_string(), "acme/api/backend/triage");
        assert_eq!(RoutineKey::parse("acme/api/backend/triage"), Some(key));
        assert_eq!(RoutineKey::parse("backend/triage"), None);
        assert_eq!(RoutineKey::parse("acme/api//triage"), None);
    }

    #[test]
    fn a_personal_routine_never_names_another_orgs_agent() {
        let me = Principal::from(1u64);
        let mine = RoutineKey::personal(&me, "acme/a", "sweep");
        assert_eq!(
            AgentRef::from("_personal/1/acme/b/_scratch/triage").key_for(&mine),
            Err(ValidationError::AgentOutsideOrg(
                "_personal/1/acme/b/_scratch/triage".into()
            ))
        );
        assert_eq!(
            AgentRef::from("acme/b/backend/triage").key_for(&mine),
            Err(ValidationError::AgentOutsideOrg(
                "acme/b/backend/triage".into()
            ))
        );
        assert!(AgentRef::from("_personal/1/acme/a/_scratch/triage")
            .key_for(&mine)
            .is_ok());
        assert!(AgentRef::from("acme/a/backend/triage")
            .key_for(&mine)
            .is_ok());
        assert!(AgentRef::from("builtin/claude").key_for(&mine).is_ok());
    }

    #[test]
    fn a_personal_key_is_qualified_by_its_account() {
        let alice = RoutineKey::personal(&Principal::from(1u64), "acme/api", "standup");
        let bob = RoutineKey::personal(&Principal::from(2u64), "acme/api", "standup");
        assert_ne!(alice, bob);
        assert_eq!(alice.to_string(), "_personal/1/acme/api/_scratch/standup");
        assert_eq!(
            RoutineKey::parse("_personal/1/acme/api/_scratch/standup"),
            Some(alice.clone())
        );
        assert!(alice.resolves_for(&Principal::from(1u64)));
        assert!(!alice.resolves_for(&Principal::from(2u64)));
        // No unqualified personal key.
        assert_eq!(
            RoutineKey::parse("_personal/_personal/personal/standup"),
            None
        );
        assert_eq!(
            RoutineKey::parse("_personal/bob/acme/api/_scratch/standup"),
            None
        );
        assert!(!RoutineKey::new("acme/api", "_scratch", "standup")
            .resolves_for(&Principal::from(1u64)));
        // A team key is the same whoever sees it.
        let team = RoutineKey::scoped(&Principal::from(1u64), "acme/api", "backend", "triage");
        assert_eq!(team, RoutineKey::new("acme/api", "backend", "triage"));
        assert!(team.resolves_for(&Principal::from(2u64)));
        // On the wire a key is its rendered form, and only a well-formed one
        // reads back.
        assert_eq!(
            serde_json::to_value(&alice).unwrap(),
            serde_json::json!("_personal/1/acme/api/_scratch/standup")
        );
        assert_eq!(
            serde_json::from_value::<RoutineKey>(serde_json::json!(
                "_personal/1/acme/api/_scratch/standup"
            ))
            .unwrap(),
            alice
        );
        assert!(serde_json::from_value::<RoutineKey>(serde_json::json!(
            "_personal/_personal/personal/standup"
        ))
        .is_err());
    }

    /// SW-AGR-10: every routine the CodeSense docs ship is an agent-owned
    /// definition: it sits at `<agent>/routines/<id>/routine.yml` under an
    /// agent that ships beside it, names no `agent:` (the path does), writes
    /// its mode, and none teaches the old middleman (a second agent that
    /// wakes the first with `send_to_session`). The examples are read the way
    /// a user installs them: copied under a team's `config/`.
    #[test]
    fn every_routine_yml_in_the_codesense_docs_is_an_agent_owned_routine() {
        use super::super::{defs, layout};
        fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    collect(&path, out);
                } else if path.file_name().is_some_and(|n| n == "routine.yml") {
                    out.push(path);
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../codesense-docs");
        let mut files = Vec::new();
        collect(&root, &mut files);
        files.sort();
        // (routine id, agent, mode) of every routine that ships.
        let expected = [
            ("update-codesense-docs", "docs-helper", RoutineMode::Fresh),
            ("manager-ensure", "manager", RoutineMode::Wake),
            ("manager-wake", "manager", RoutineMode::Wake),
            (
                "solo-founder-queue-wake",
                "solo-founder-planner",
                RoutineMode::Wake,
            ),
            ("while-you-were-away", "reviewer", RoutineMode::Fresh),
        ];
        let mut found = Vec::new();
        for file in &files {
            let raw = std::fs::read_to_string(file).unwrap();
            let rel = file.strip_prefix(&root).unwrap().display().to_string();
            assert!(
                !raw.contains("send_to_session"),
                "{rel}: a routine wakes its own agent with mode: wake, never through send_to_session"
            );
            let def: RoutineDef =
                serde_yaml::from_str(&raw).unwrap_or_else(|e| panic!("{rel} does not parse: {e}"));
            assert!(
                def.extra.is_empty() && def.policy.extra.is_empty() && def.target.extra.is_empty(),
                "{rel} has unknown fields"
            );
            assert!(
                def.agent.is_empty(),
                "{rel}: the path names the agent, not `agent:`"
            );
            // Install the owning agent under config/agents; example groupings
            // above that directory are not part of the runtime layout.
            let agent_dir = file.parent().unwrap().parent().unwrap().parent().unwrap();
            assert!(
                agent_dir.join("agent.yaml").is_file(),
                "{rel}: its agent ships beside it"
            );
            let team = tempfile::tempdir().unwrap();
            let installed = team
                .path()
                .join("config")
                .join(layout::AGENTS_DIR)
                .join(agent_dir.file_name().unwrap())
                .join(file.strip_prefix(agent_dir).unwrap());
            if agent_dir
                .parent()
                .and_then(std::path::Path::file_name)
                .is_some_and(|name| name == layout::AGENTS_DIR)
            {
                let parts: Vec<&str> = rel.split('/').collect();
                let at = parts
                    .iter()
                    .rposition(|p| *p == layout::AGENTS_DIR)
                    .unwrap();
                assert_eq!(
                    installed,
                    team.path().join("config").join(parts[at..].join("/")),
                    "{rel}: ungrouped examples keep their existing installation path"
                );
            }
            std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
            std::fs::write(&installed, &raw).unwrap();
            let dir = installed.parent().unwrap();
            let home = layout::home_of_dir(dir)
                .unwrap_or_else(|| panic!("{rel} is not in an agent-owned layout"));
            let layout::Home::Agent(agent) = home else {
                panic!("{rel} must live under a saved agent, not {home:?}");
            };
            let loaded = defs::read_def(dir).unwrap().unwrap();
            loaded
                .validate()
                .unwrap_or_else(|e| panic!("{rel} is invalid: {e}"));
            loaded
                .validate_wake_target()
                .unwrap_or_else(|e| panic!("{rel} has an invalid wake target: {e}"));
            assert_eq!(
                loaded.agent.as_str(),
                agent,
                "{rel}: the read fills in its agent"
            );
            assert_eq!(
                Some(loaded.id.as_str()),
                dir.file_name().and_then(|n| n.to_str()),
                "{rel}: id must match its folder"
            );
            let mode = loaded
                .mode()
                .unwrap_or_else(|e| panic!("{rel} must write its mode: {e}"));
            found.push((loaded.id.clone(), agent, mode.as_str()));
        }
        found.sort();
        let mut expected: Vec<_> = expected
            .iter()
            .map(|(id, agent, mode)| (id.to_string(), agent.to_string(), mode.as_str()))
            .collect();
        expected.sort();
        assert_eq!(found, expected);
    }

    #[test]
    fn reviewer_result_requires_citations_and_accepts_a_quiet_day() {
        use serde_json::json;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let raw = std::fs::read_to_string(root.join(
            "../codesense-docs/getting-started/content/examples/agents/reviewer/routines/while-you-were-away/routine.yml",
        )).unwrap();
        let def = parse(&raw);
        let schema = def.result_schema.as_ref().unwrap();
        super::super::results::validate_schema(schema).unwrap();
        assert_eq!(def.runs_on().unwrap(), RunsOn::OneMac);
        assert_eq!(def.variables["person"].default.as_deref(), Some("sam"));
        let mut payload = json!({
            "person":"sam", "review_path":"reviews/sam/2026-10-20.md",
            "window":{"since":"2026-10-19T07:00:00Z","until":"2026-10-20T07:00:00Z"},
            "generated_at":"2026-10-20T07:01:00Z", "event_count":0,
            "from_sha":"a".repeat(40), "to_sha":"b".repeat(40), "sections":[]
        });
        let validate = |p: &serde_json::Value| super::super::results::validate_payload(schema, p);
        validate(&payload).unwrap();
        payload["event_count"] = json!(1);
        assert!(validate(&payload).is_err());
        payload["sections"] =
            json!([{"title":"What moved","claims":[{"text":"Shipped","citations":[]}]}]);
        assert!(validate(&payload).is_err(), "uncited claims cannot publish");
        payload["sections"][0]["claims"][0]["citations"] = json!([{
            "source":"commit", "org":"acme/team-context", "team":"web",
            "at":"2026-10-20T06:00:00Z", "sha":"a".repeat(40)
        }]);
        validate(&payload).unwrap();
        payload["sections"][0]["claims"][0]["citations"][0]
            .as_object_mut()
            .unwrap()
            .remove("sha");
        assert!(
            validate(&payload).is_err(),
            "a commit citation needs its SHA"
        );
        payload["sections"][0]["claims"][0]["citations"] = json!([{
            "source":"message", "org":"acme/team-context", "team":"web",
            "at":"2026-10-20T06:00:00Z", "id":42, "record":"agent-update"
        }]);
        validate(&payload).unwrap();
        payload["sections"][0]["claims"][0]["citations"][0]["team"] = json!("family");
        assert!(
            validate(&payload).is_err(),
            "citations stay in the reviewed team"
        );
    }
}

fn scalar_default_schema(_: &mut rmcp::schemars::SchemaGenerator) -> rmcp::schemars::Schema {
    rmcp::schemars::json_schema!({"type":["string","number","boolean","null"]})
}
