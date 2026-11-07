// SPDX-License-Identifier: MIT
//! The routine MCP tools: their parameters, wire shapes and bodies.
//!
//! The `#[tool]` methods in [`super::handler`] only resolve who is calling (the
//! signed-in principal, the calling session's launch profile, the target team)
//! and run one of the bodies here inside `await_blocking`. Every body is a thin
//! adapter over [`crate::routines::service`]: it parses the agent's words into
//! a definition or a key, calls the service, and shapes the result into the
//! shared [`ToolEnvelope`], with every time as RFC3339.
//!
//! None of these tools tells anyone anything: a routine an agent creates,
//! changes or deletes shows up in the Routines tab and nowhere else.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use rmcp::schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

use crate::agents::launch_check::LaunchWarning;
use crate::agents::AgentKey;
use crate::context_repo::scratch_space::SCRATCH_TEAM;
use crate::domain::Principal;
use crate::routines::admit::Outcome;
use crate::routines::model::{
    is_valid_id, validate_trigger, EventSpec, MetadataCondition, RoutineMode, RunsOn, Variable,
    EVENT_AGENT_DONE, EVENT_INBOX_IDLE, EVENT_METADATA_CHANGE, EVENT_ROLE_PENDING,
};
use crate::routines::runs::RoutineRun;
use crate::routines::service::{self, Placement, RoutineError, RunsPage, SaveTarget};
use crate::routines::state::{enabled_here, RoutineStateStore};
use crate::routines::{
    agents, layout, local_host_id, schedule, AgentRef, Author, MissedPolicy, OverlapPolicy, Policy,
    RoutineDef, RoutineKey, RoutineScope, RoutineTrigger, ScheduleSpec, Where,
};

pub(crate) use super::tool_envelope::ToolEnvelope;

/// What a body returns: a typed envelope, or a hard failure (an unreadable
/// store, a failed write) that surfaces as an MCP error.
pub(crate) type Reply<T> = Result<ToolEnvelope<T>, String>;

/// The message `routine_context` answers with outside a routine run.
pub(crate) const NOT_A_ROUTINE_RUN: &str = "not a routine run";

/// How long a run may take when the caller does not say.
const DEFAULT_MAX_RUNTIME: &str = "30m";

/// How many upcoming fires a create or update reports.
const NEXT_FIRES: usize = 3;

/// How many recent runs `routine_get` includes.
const GET_RUNS: usize = 5;

/// The calling session's launch facts, as the launch registry recorded them.
#[derive(Debug, Clone, Default)]
pub(crate) struct CallerSession {
    /// The runtime an account-bound launch resolved its account for.
    pub account_runtime: Option<codesense_pty_core::agent_cli::AgentCli>,
    /// The launch profile id the session's launch stamped.
    pub profile_id: Option<String>,
    /// The named agent the session was launched as: what a routine it creates
    /// runs when it names none.
    pub agent: Option<AgentKey>,
}

impl CallerSession {
    /// Who a revision this caller writes is credited to.
    pub(crate) fn author(&self) -> Author {
        let name = self
            .profile_id
            .clone()
            .or_else(|| self.account_runtime.map(|r| r.as_str().to_string()))
            .unwrap_or_else(|| "mcp".to_string());
        Author::agent(name)
    }
}

fn open_object_schema(_: &mut SchemaGenerator) -> Schema {
    rmcp::schemars::json_schema!({
        "type": "object",
        "additionalProperties": true
    })
}

fn string_map_schema(_: &mut SchemaGenerator) -> Schema {
    rmcp::schemars::json_schema!({
        "type": "object",
        "additionalProperties": { "type": "string" }
    })
}

/// Event inputs share the YAML model's spelling and validation.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RoutineEventParams {
    /// inbox_idle, agent_done or role_pending.
    pub kind: String,
    /// agent_done only: agent id or full key; omitted watches all agents in reach.
    #[serde(default)]
    pub from_agent: Option<String>,
    /// role_pending only: nonempty queue name; omitted watches every queue in reach.
    #[serde(default)]
    pub role: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoutineCreateParams {
    /// agent (default) or command. action:command takes command:[program,arg,...]
    /// instead of agent/task/mode; it runs as you, with no agent or model, in a
    /// team workspace or managed repo clone. Defaults: overlap queue, maxRuntime 5m.
    /// Only me and machine-armed command routines are refused.
    #[serde(default)]
    pub action: Option<crate::routines::model::RoutineAction>,
    /// Program and arguments, one entry each. A bare program uses PATH; a relative
    /// executable path must be executable inside the team content root; absolute
    /// executable paths are refused. Args render individually, never through a shell.
    /// Exit 0 succeeds; routine_run exposes the secret-redacted last 4 KB of output.
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// Fire after a routine in the same space succeeds or finishes. `finished`
    /// excludes cancelled/skipped/coalesced. Cycles and missing upstream routines
    /// are refused. Chained fires carry trigger.{run_id,routine,phase,result_text,detail}.
    #[serde(default)]
    pub after: Option<crate::routines::model::After>,

    /// What the routine is called, e.g. "Morning triage".
    pub name: String,
    /// What the agent should do on each run, in plain words. Example:
    /// "Summarize acme/web issues since {{last_run_at}} to #web-triage with Slack
    /// MCP tools, then signal_done with it."
    #[serde(default)]
    pub task: String,
    /// Optional bounded inline JSON Schema for the routine's private dashboard
    /// result; omit when no structured result is required.
    #[serde(default)]
    pub result_schema: Option<serde_json::Value>,
    /// When it runs, read in `tz`: a preset (`every 5m|10m|15m|20m|30m`,
    /// `every 1h|2h|3h|4h|6h|8h|12h`, `hourly`, `daily 09:00`, `weekdays 08:30`,
    /// `weekly mon 17:00`, on a 24-hour clock) or a 5-field cron such as
    /// `0 9 * * 1-5`. Nothing fires more often than every 5 minutes. Omit it for a
    /// routine that runs only when started by hand. Empty when removes schedules;
    /// other event triggers are kept.
    #[serde(default)]
    pub when: Option<String>,
    /// Metadata conditions using query_context's selection and eq clauses. Example:
    /// [{"selection":{"where":{"kinds":["story"]}},"target":{"key":"status","op":"eq","value":"in-progress"}}].
    /// Add previous:{"key":"status","op":"eq","value":"planning"} for an exact transition.
    /// Matches at first discovery or restart are a baseline; only later entry fires.
    /// Omit to preserve; [] removes metadata triggers on update, keeping other events.
    /// Not with runsOn:each_mac. Recipients read changes with routine_context and
    /// the wake header runId (incl. coalesced_payloads).
    #[serde(default)]
    #[schemars(with = "Option<Vec<super::AnyJson>>")]
    pub metadata_triggers: Option<Vec<MetadataCondition>>,
    /// Events: [{"kind":"inbox_idle"}], [{"kind":"agent_done","from_agent":"reviewer"}],
    /// or [{"kind":"role_pending","role":"reviewer"}]. Omit role for every queue in reach.
    #[serde(default)]
    pub events: Option<Vec<RoutineEventParams>>,
    /// The IANA zone `when` is read in. Default: this machine's zone.
    #[serde(default)]
    pub tz: Option<String>,
    /// The agent the routine belongs to and runs: its `id` from agent_list in the
    /// routine's own space (a bare `triage` is the team's agent for a team
    /// routine and your private agent for an Only me one), or its full key, which
    /// lets an Only me routine run a team agent. A team routine needs an agent of
    /// its own team or a starter (`builtin/claude`, `builtin/codex`), since any
    /// teammate's Mac may run it. Required, unless `sessionId` names a session
    /// launched as a saved agent (then that one; a session launched as a starter
    /// still defaults to that starter, with a `deprecations` note). The routine
    /// lives under it, in `config/agents/<agent>/routines/<id>/`; each run
    /// launches it with the running user's own account at its current revision,
    /// archived or not.
    /// The reply names the one chosen; routine_update changes it.
    #[serde(default)]
    pub agent: Option<String>,
    /// `wake` (deliver the task into the agent's live session when it is
    /// idle, starting one with `task` as its first prompt only when none runs;
    /// never a second instance) or `fresh` ("start a new" in the app: a new
    /// instance in `where` on each fire, headless unless `visible`). Omitted: `wake` for a visible routine on a
    /// saved agent that launches in a tab, else `fresh`; the reply's
    /// `effective_mode` says what a fire does. A built-in starter's routines
    /// are fresh only.
    #[serde(default)]
    pub mode: Option<String>,
    /// Where a wake looks for (and starts) the agent: `local` (default, this
    /// Mac), a machine id from list_machines, or `any` connected machine (wakes
    /// the agent wherever it runs, starts it here when none does). With
    /// `runsOn: each_mac` it must be `local`, except that a `fresh` routine may
    /// use `any`. A session on an offline machine still counts as running, so
    /// that wake is undelivered (machine_offline) and nothing starts.
    #[serde(default)]
    pub machine: Option<String>,
    /// Where it runs: `team_workspace` (default) or `repo:<owner>/<repo>`.
    #[serde(default, rename = "where")]
    pub target: Option<String>,
    /// `team` (default: teammates can see it) or `personal` (Only me, only when
    /// the user asks for it: your private space for `org`, which is then
    /// required and must be an org synced on this Mac; it runs in that org
    /// only). A team routine goes in `org`/`team`, or the active team when both
    /// are omitted. Anyone who sees it can change it; `owner` grants nothing.
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub org: Option<String>,
    #[serde(default)]
    pub team: Option<String>,
    /// A team routine only: `one_mac` (default) runs it once for the team, on
    /// this Mac until moved (a shared job, e.g. a bug bot or team triage); `each_mac` makes
    /// it a recipe each teammate turns on for their own Mac (e.g. a digest or a
    /// reminder for oneself), off everywhere until they do, with its own runs
    /// on each.
    #[serde(default, alias = "runs_on")]
    pub runs_on: Option<String>,
    /// How long one run may take, like `30m` or `2h`. Default `30m`.
    #[serde(default, alias = "max_runtime")]
    pub max_runtime: Option<String>,
    /// `coalesce` (default), `skip` or `queue`: what a fire does while a run is
    /// still going.
    #[serde(default)]
    pub overlap: Option<String>,
    /// `skip` (default) or `run_once`: what happens to fires missed while the
    /// app was closed or asleep.
    #[serde(default)]
    pub missed: Option<String>,
    /// Optional: how many wakes this routine may deliver in all. Reached, it
    /// pauses itself and posts one inbox note; resuming starts a new count.
    /// Light checks need no wake limits; heavy work may use
    /// "minGap":"4h","maxWakesPerDay":3,"overlap":"skip".
    /// Past 120 wakes/hour per session, wakes return rate_limited (one inbox note).
    /// Omit for no limit. Wake limits count wakes typed into a live session, so
    /// they apply to `mode: wake` only (a `fresh` routine with limits saves with
    /// a `wake_limits_ignored` warning; a start when none runs is not counted).
    /// The terminal server running the session enforces them, so they hold
    /// with the desktop closed; each Mac and machine keeps its own count, and
    /// a machine whose terminal server is too old is sent no limited wake
    /// (undelivered (unavailable), with one inbox note to update it).
    #[serde(default, alias = "max_wakes")]
    pub max_wakes: Option<u32>,
    /// budgetUsd {perDay?,perWeek?}: positive finite USD; local calendar day and
    /// Monday-start week, not rolling windows. Fresh only: wakes cap the agent;
    /// commands cannot. Machine-armed caps are refused. Raising/removing a cap
    /// needs native Accept; lowering ratchets the local accepted ceiling.
    /// Unrestricted shells are trusted; sandbox untrusted agents.
    #[serde(default)]
    pub budget_usd: Option<crate::session_usage::spend_caps::BudgetUsd>,
    /// Optional: wakes in any rolling 24 h. Fires over it are
    /// `undelivered(limit_reached)` and it resumes on its own. A wake limit
    /// (see `maxWakes`): `mode: wake` only.
    #[serde(default, alias = "max_wakes_per_day")]
    pub max_wakes_per_day: Option<u32>,
    /// Optional: the least time between two wakes of this routine (into any
    /// session) (`30m`, `2h`, `1d`; at most `7d`), for wakes that start heavy
    /// work. A fire inside it is coalesced or skipped per `overlap`. A wake
    /// limit (see `maxWakes`): `mode: wake` only.
    #[serde(default, alias = "min_gap")]
    pub min_gap: Option<String>,
    /// Run in a visible terminal tab instead of headless. Default false.
    #[serde(default)]
    pub visible: Option<bool>,
    /// Task variables, `{ name: { type, default } }`, referenced as `{{name}}`:
    /// e.g. `{"channel": {"type": "text", "default": "#web"}}` used as
    /// `{{channel}}`.
    #[serde(default)]
    #[schemars(schema_with = "open_object_schema")]
    pub variables: Option<serde_json::Value>,
    /// The routine id within its team. Default: derived from `name`; a taken id is validation.
    #[serde(default)]
    pub id: Option<String>,
    /// Whether it fires from this Mac. Default true (false for a draft that
    /// never fires until routine_set_enabled resumes it); for `runsOn:
    /// each_mac`, default false, and true turns it on for this Mac.
    /// The 25 enabled-routine cap is checked when enabling.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Check everything and report what would be created, writing nothing;
    /// reports rendered argv for commands. Keep other defaults unless asked;
    /// report `schedule_in_words`, the first `next_fires` and the `agent`.
    /// Validation includes a bad `when`, under 5 minutes, the 25 enabled-routine
    /// cap, no visible `agent`, an Only me agent for a team routine, or a taken `id`.
    #[serde(default, alias = "dry_run")]
    pub dry_run: bool,
    /// Omit to use the verified connection identity. A supplied id must name that caller.
    #[serde(default, alias = "session_id")]
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoutineUpdateParams {
    /// agent (default) or command. action:command takes command:[program,arg,...]
    /// instead of agent/task/mode; it runs as you, with no agent or model, in a
    /// team workspace or managed repo clone. Defaults: overlap queue, maxRuntime 5m.
    /// Only me and machine-armed command routines are refused.
    #[serde(default)]
    pub action: Option<crate::routines::model::RoutineAction>,
    /// Program and arguments, one entry each. A bare program uses PATH; a relative
    /// executable path must be executable inside the team content root; absolute
    /// executable paths are refused. Args render individually, never through a shell.
    /// Exit 0 succeeds; routine_run exposes the secret-redacted last 4 KB of output.
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// Fire after a routine in the same space succeeds or finishes. `finished`
    /// excludes cancelled/skipped/coalesced. Cycles and missing upstream routines
    /// are refused. Chained fires carry trigger.{run_id,routine,phase,result_text,detail}.
    /// after:null removes it; omission preserves it.
    #[serde(default, deserialize_with = "deserialize_patch")]
    pub after: Option<Option<crate::routines::model::After>>,

    /// The routine's `id` as `routine_list` returns it.
    pub id: String,
    /// The `revision` the change was made against.
    #[serde(alias = "base_revision")]
    pub base_revision: u32,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub task: Option<String>,
    /// Replaces the output schema; explicit null clears it, omission preserves it.
    #[serde(default, deserialize_with = "deserialize_schema_patch")]
    pub result_schema: Option<Option<serde_json::Value>>,
    /// A new schedule (as for `routine_create`); replaces every schedule.
    /// when:"" removes schedules and keeps events; omitted preserves schedules.
    #[serde(default)]
    pub when: Option<String>,
    /// Metadata conditions using query_context's selection and eq clauses. Example:
    /// [{"selection":{"where":{"kinds":["story"]}},"target":{"key":"status","op":"eq","value":"in-progress"}}].
    /// Add previous:{"key":"status","op":"eq","value":"planning"} for an exact transition.
    /// Matches at first discovery or restart are a baseline; only later entry fires.
    /// Omit to preserve; [] removes metadata triggers on update, keeping other events.
    /// Not with runsOn:each_mac. Recipients read changes with routine_context and
    /// the wake header runId (incl. coalesced_payloads).
    #[serde(default)]
    #[schemars(with = "Option<Vec<super::AnyJson>>")]
    pub metadata_triggers: Option<Vec<MetadataCondition>>,
    /// Replaces inbox_idle, agent_done and role_pending triggers only; schedules and
    /// metadata triggers stay. Omitted or null preserves; [] clears these events.
    #[serde(default)]
    pub events: Option<Vec<RoutineEventParams>>,
    /// An IANA zone. Alone, it moves every schedule into it; with `when`, it is
    /// the zone `when` is read in. Default: the routine's current zone.
    #[serde(default)]
    pub tz: Option<String>,
    /// Another agent, as for `routine_create`: the routine moves under it
    /// (same id and history): how an Unassigned routine gets an agent.
    #[serde(default)]
    pub agent: Option<String>,
    /// `fresh` or `wake`, as for `routine_create`.
    #[serde(default)]
    pub mode: Option<String>,
    /// `local`, a machine id or `any`, as for `routine_create`.
    #[serde(default)]
    pub machine: Option<String>,
    #[serde(default, rename = "where")]
    pub target: Option<String>,
    /// A team routine only: `one_mac` or `each_mac`, as for `routine_create`.
    /// To `each_mac`: stays on the Mac that ran it as a shared job, off
    /// elsewhere until turned on there. To `one_mac`: this editing Mac runs it
    /// and others stop.
    #[serde(default, alias = "runs_on")]
    pub runs_on: Option<String>,
    #[serde(default, alias = "max_runtime")]
    pub max_runtime: Option<String>,
    #[serde(default)]
    pub overlap: Option<String>,
    #[serde(default)]
    pub missed: Option<String>,
    #[serde(default)]
    pub visible: Option<bool>,
    /// As for routine_create; `null` removes the limit, omitted keeps it.
    #[serde(default, alias = "max_wakes", deserialize_with = "deserialize_patch")]
    pub max_wakes: Option<Option<u32>>,
    /// budgetUsd {perDay?,perWeek?}: positive finite USD for the local calendar
    /// day and Monday-start week, not rolling windows. Lowering is immediate
    /// and ratchets the local accepted ceiling; raises/null removal need native
    /// Accept. Omitted keeps the cap. Fresh only: wakes cap the agent; commands
    /// cannot. Machine-armed caps are refused. Unrestricted shells are trusted;
    /// sandbox untrusted agents.
    #[serde(default, deserialize_with = "deserialize_patch")]
    pub budget_usd: Option<Option<crate::session_usage::spend_caps::BudgetUsd>>,
    /// As for routine_create; `null` removes it, omitted keeps it.
    #[serde(
        default,
        alias = "max_wakes_per_day",
        deserialize_with = "deserialize_patch"
    )]
    pub max_wakes_per_day: Option<Option<u32>>,
    /// As for routine_create; `null` removes it, omitted keeps it.
    #[serde(default, alias = "min_gap", deserialize_with = "deserialize_patch")]
    pub min_gap: Option<Option<String>>,
    /// Replaces every declared variable.
    #[serde(default)]
    #[schemars(schema_with = "open_object_schema")]
    pub variables: Option<serde_json::Value>,
    /// Omit to use the verified connection identity. A supplied id must name that caller.
    #[serde(default, alias = "session_id")]
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoutineGetParams {
    /// The routine's `id` as `routine_list` returns it.
    pub id: String,
    /// Only when it belongs to this agent (its `id` or key from agent_list):
    /// a routine of another agent is `validation`.
    #[serde(default)]
    pub agent: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoutineDeleteParams {
    /// The routine's `id` as `routine_list` returns it.
    pub id: String,
    /// Omit to use the verified connection identity. A supplied id must name that caller.
    #[serde(default, alias = "session_id")]
    pub session_id: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoutineListParams {
    /// Only the routines of this agent: its `id` or key from agent_list.
    #[serde(default)]
    pub agent: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoutineSetEnabledParams {
    pub id: String,
    /// false pauses it on this machine (turns an `each_mac` routine off for
    /// this Mac); true resumes it (turns it on).
    pub enabled: bool,
    /// Omit to use the verified connection identity. A supplied id must name that caller.
    #[serde(default, alias = "session_id")]
    pub session_id: Option<String>,
}

/// Where a routine is armed.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoutineArmOnParams {
    pub id: String,
    /// `this_mac`: this Mac fires it. A machine id (from list_machines): that
    /// machine's terminal server fires it on its own clock, with or without
    /// this Mac.
    pub machine: String,
    /// Omit to use the verified connection identity. A supplied id must name that caller.
    #[serde(default, alias = "session_id")]
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoutineRunNowParams {
    pub id: String,
    /// Values for the routine's declared variables.
    #[serde(default)]
    #[schemars(schema_with = "string_map_schema")]
    pub variables: Option<BTreeMap<String, String>>,
    /// Omit to use the verified connection identity. A supplied id must name that caller.
    #[serde(default, alias = "session_id")]
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoutineRunsParams {
    pub id: String,
    /// At most this many runs (default 50, max 200).
    #[serde(default)]
    pub limit: Option<usize>,
    /// The previous page's `next_before`.
    #[serde(default)]
    pub before: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoutineRunParams {
    /// A routine run's `run_id` (from routine_runs, routine_get's `runs`, or
    /// routine_list's `last_run`).
    #[serde(alias = "run_id")]
    pub run_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoutineCancelParams {
    /// A run_id from routine_runs or routine_run.
    #[serde(alias = "run_id")]
    pub run_id: String,
    /// Omit to use the verified connection identity. A supplied id must name that caller.
    #[serde(default, alias = "session_id")]
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoutineContextParams {
    /// The id in the wake header when the session holds several runs.
    #[serde(default, alias = "run_id")]
    pub run_id: Option<String>,
    /// Your `$CODESENSE_TAB_SESSION_ID`.
    #[serde(alias = "session_id")]
    pub session_id: String,
}

/// `routine_done`: finish one routine run, not the session.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoutineDoneParams {
    /// Your `$CODESENSE_TAB_SESSION_ID`: the session running the run.
    #[serde(alias = "session_id")]
    pub session_id: String,
    /// The run to finish (`run_id` from routine_context, or the id in the
    /// `[routine: … | run <id>]` header of a wake).
    #[serde(alias = "run_id")]
    pub run_id: String,
    /// `success` | `failed` | `needs-attention`.
    pub outcome: String,
    /// A short result line: the run's result, and the next run's
    /// `{{last_run_summary}}` when it succeeded.
    #[serde(default)]
    pub summary: Option<String>,
}

/// A run `routine_done` finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RoutineDoneItem {
    pub run_id: String,
    pub routine_id: String,
    /// The phase it ended in: `succeeded`, `failed` or `needs_you`.
    pub phase: &'static str,
}

/// Finish `run_id` for `session_id` with `outcome`, leaving the session and
/// any other run alone.
#[cfg(test)]
pub(crate) fn done(
    store: &RoutineStateStore,
    session_id: &str,
    params: RoutineDoneParams,
    now: i64,
) -> Reply<RoutineDoneItem> {
    done_with(store, session_id, params, now, &|_, _, _| Ok(None))
}

/// What finishing a run on the machine that fired it answered: the run it
/// finished there (now recorded here), or `None` when it fired no such run
/// in that session.
pub(crate) type OnMachineDone<'a> =
    &'a dyn Fn(&str, &str, &str) -> Result<Option<codesense_pty_core::routine_arm::ArmRun>, String>;

/// [`done`], for a session on a machine: a run that machine fired (one this
/// Mac has no row for yet, or a row it recorded from the machine's log) is
/// finished on the machine itself (`on_machine(run_id, outcome, summary)`),
/// so its open run there ends too, and recorded here.
pub(crate) fn done_with(
    store: &RoutineStateStore,
    session_id: &str,
    params: RoutineDoneParams,
    now: i64,
    on_machine: OnMachineDone<'_>,
) -> Reply<RoutineDoneItem> {
    let machine_run: Option<bool> = store
        .transaction(|tx| {
            use rusqlite::OptionalExtension;
            Ok(tx
                .query_row(
                    "SELECT occurrence_key FROM routine_runs WHERE run_id = ?1",
                    rusqlite::params![params.run_id],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()?
                .map(|occurrence| occurrence.is_some_and(|o| o.contains("@machine:"))))
        })
        .map_err(|e| e.to_string())?;
    if machine_run.is_none_or(|machine| machine) {
        let outcome = params.outcome.trim();
        let summary = params.summary.as_deref().unwrap_or("").trim();
        if let Some(run) = on_machine(&params.run_id, outcome, summary)? {
            use crate::routines::runs::phase as run_phase;
            let phase = match outcome {
                "success" => run_phase::SUCCEEDED,
                "needs-attention" => run_phase::NEEDS_YOU,
                _ => run_phase::FAILED,
            };
            return Ok(ToolEnvelope::Ok(RoutineDoneItem {
                run_id: run.run_id,
                routine_id: run.key,
                phase,
            }));
        }
    }
    let Some(status) = crate::dashboard_stream::SessionStatus::from_wire(params.outcome.trim())
    else {
        return Ok(ToolEnvelope::Validation {
            message: "outcome must be one of: success | failed | needs-attention".into(),
        });
    };
    let summary = params
        .summary
        .map(|summary| summary.trim().to_string())
        .filter(|summary| !summary.is_empty());
    match crate::routines::finish::finish_for_session(
        store,
        &params.run_id,
        session_id,
        status,
        summary,
        now,
    )
    .map_err(|e| e.to_string())?
    {
        Ok(finished) => {
            finished.announce();
            Ok(ToolEnvelope::Ok(RoutineDoneItem {
                run_id: finished.run_id,
                routine_id: finished.routine_key,
                phase: finished.phase,
            }))
        }
        Err(refusal) => Ok(ToolEnvelope::Validation {
            message: refusal.to_string(),
        }),
    }
}

/// A created or updated routine.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct RoutineSaved {
    pub argv: Option<Vec<String>>,
    pub action: crate::routines::model::RoutineAction,
    pub command: Vec<String>,
    pub triggers: Vec<RoutineTrigger>,
    pub id: String,
    pub agent: String,
    /// The key of the agent it belongs to, when the reference reads as one.
    pub agent_key: Option<String>,
    /// The mode as stored (`fresh` / `wake`), or null when none was picked.
    pub mode: Option<String>,
    /// What a fire does in this build: `fresh` until wake ships.
    pub effective_mode: &'static str,
    pub schedule_in_words: String,
    pub next_fires: Vec<String>,
    pub enabled: bool,
    pub revision: u32,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub dry_run: bool,
    /// What a run would be warned about, such as a task starting with a
    /// `/command` the agent does not start with. The save still happened.
    pub warnings: Vec<LaunchWarning>,
    /// Inputs this release still accepts but the next will refuse.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deprecations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct LastRunItem {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct RoutineListItem {
    pub action: crate::routines::model::RoutineAction,
    pub command: Vec<String>,
    pub id: String,
    pub scope: RoutineScope,
    pub org: String,
    pub team: String,
    pub name: String,
    pub schedule_in_words: String,
    pub agent: String,
    /// The key of the agent it belongs to, when the reference reads as one.
    pub agent_key: Option<String>,
    /// Its agent is nowhere on this Mac: it never fires until an agent is
    /// picked (routine_update `agent`).
    pub unassigned: bool,
    /// Another routine of its space owns this id: never fired.
    pub duplicate: bool,
    /// The mode as stored, or null when none was picked.
    pub mode: Option<String>,
    /// What a fire does in this build.
    pub effective_mode: &'static str,
    #[serde(rename = "where")]
    pub target: String,
    /// `one_mac` or `each_mac`.
    pub runs_on: &'static str,
    /// This Mac's switch: not paused here, or, for `each_mac`, turned on here.
    pub enabled: bool,
    pub next_run_at: Option<String>,
    pub last_run: Option<LastRunItem>,
    pub owner: String,
    pub owner_name: String,
    pub created_by: Option<Author>,
    pub armed_here: bool,
    pub needs_you: bool,
    pub revision: u32,
    /// Armed on a machine whose prepared content is behind it
    /// (`machine`, `since_ms`, `why`); routine_refresh_on_machine prepares
    /// it. Absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_stale: Option<crate::routines::machine_arm::ContentStale>,
}

/// One run, without the session id that would let a caller read its context.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct RunItem {
    pub action: crate::routines::model::RoutineAction,
    #[serde(flatten)]
    pub command: crate::routines::command::Evidence,
    pub causation: Option<crate::routines::runs::RunCausation>,
    pub run_id: String,
    pub revision: u32,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_payload: Option<serde_json::Value>,
    pub phase: String,
    pub reason: Option<String>,
    pub linked_run_id: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub result_text: Option<String>,
    pub detail: Option<String>,
    /// The agent the run launched, as `<key>@<revision>`.
    pub agent: Option<String>,
}

impl From<RoutineRun> for RunItem {
    fn from(run: RoutineRun) -> Self {
        Self {
            action: run.action,
            command: run.command,
            causation: run.upstream,
            created_at: rfc3339(run.created_at),
            started_at: run.started_at.map(rfc3339),
            ended_at: run.ended_at.map(rfc3339),
            run_id: run.run_id,
            revision: run.revision,
            source: run.source,
            trigger_payload: run.trigger_payload,
            phase: run.phase,
            reason: run.reason,
            linked_run_id: run.linked_run_id,
            result_text: run.result_text,
            detail: run.detail,
            agent: run.agent,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RoutineGot {
    #[serde(flatten)]
    pub cap: crate::session_usage::spend_caps::CapStatus,
    pub action: crate::routines::model::RoutineAction,
    pub command: Vec<String>,
    pub id: String,
    pub scope: RoutineScope,
    pub agent_key: Option<String>,
    pub unassigned: bool,
    /// The mode as stored, or null when none was picked.
    pub mode: Option<String>,
    /// What a fire does in this build.
    pub effective_mode: &'static str,
    pub owner: String,
    pub owner_name: String,
    pub armed_here: bool,
    /// `one_mac` or `each_mac`.
    pub runs_on: &'static str,
    pub enabled: bool,
    pub revision: u32,
    /// As in routine_list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_stale: Option<crate::routines::machine_arm::ContentStale>,
    pub schedule_in_words: String,
    pub routine: RoutineDef,
    pub runs: Vec<RunItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct RunsPageItem {
    pub runs: Vec<RunItem>,
    pub next_before: Option<String>,
}

/// One run with the routine it belongs to (`routine_run`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct RoutineRunGot {
    /// The routine's id, as routine_get takes it.
    pub routine_id: String,
    pub routine_name: String,
    #[serde(flatten)]
    pub run: RunItem,
}

impl From<service::RunDetail> for RoutineRunGot {
    fn from(detail: service::RunDetail) -> Self {
        Self {
            routine_id: detail.run.routine_key.clone(),
            routine_name: detail.routine_name,
            run: RunItem::from(detail.run),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct EnabledSet {
    pub id: String,
    pub enabled: bool,
    /// Resumed, but not prepared on its machine (why).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ArmedOn {
    pub id: String,
    /// The machine that fires it, or none (this Mac).
    pub armed_machine: Option<String>,
    pub arm_epoch: u64,
    pub revision: u32,
    /// A machine it moved off has not acknowledged the disarm yet: nothing
    /// fires it elsewhere until it does.
    pub handover_pending: bool,
    /// `waiting for <machine> to confirm it stopped firing it`, while the
    /// handover is pending.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting: Option<String>,
    /// Something to know about the target (it is offline: armed once it
    /// connects).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    /// Armed, but not prepared on its machine (why).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_warning: Option<String>,
}

/// Give up waiting on a machine that is gone.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoutineHandoverReleaseParams {
    pub id: String,
    /// The machine the routine is waiting on.
    pub machine: String,
    /// Must be true: you confirm the machine is gone (shut down, deleted).
    /// A machine that still runs may fire the routine once more.
    pub machine_gone: bool,
    /// Omit to use the verified connection identity. A supplied id must name that caller.
    #[serde(default, alias = "session_id")]
    pub session_id: Option<String>,
}

/// `routine_refresh_on_machine`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoutineRefreshOnMachineParams {
    pub id: String,
    /// Omit to use the verified connection identity. A supplied id must name that caller.
    #[serde(default, alias = "session_id")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct RefreshedOnMachine {
    pub id: String,
    pub machine: String,
    /// Always false on success: what fires there is prepared from now.
    pub content_stale: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct HandoverReleased {
    pub id: String,
    pub machine: String,
    /// Released, but the routine is not prepared on the machine it
    /// moved to (why).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct RunStarted {
    pub run_id: String,
    /// `queued`, `coalesced`, `skipped:<reason>`, or `duplicate` when this fire
    /// already has a run.
    pub outcome: String,
    /// `held` when it is queued behind the run still going.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The run a coalesced fire was folded into.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_run_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Archived {
    pub id: String,
    pub archived: bool,
    pub revision: u32,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RunContextItem {
    pub run_id: String,
    pub routine_id: String,
    pub routine_name: String,
    pub revision: u32,
    pub routine: RoutineDef,
    pub source: String,
    pub scheduled_at: Option<String>,
    pub fired_at: String,
    pub variables: BTreeMap<String, String>,
    pub trigger_payload: Option<serde_json::Value>,
    pub coalesced_payloads: Vec<serde_json::Value>,
    pub last_run_summary: Option<String>,
    pub last_run_id: Option<String>,
    /// The agent this run launched, as `<key>@<revision>`.
    pub agent: Option<String>,
    /// Absolute path of the routine's content folder.
    pub content_dir: Option<String>,
}

/// Epoch milliseconds as RFC3339 UTC.
fn rfc3339(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// A service refusal as the envelope the agent branches on. A store or disk
/// failure is not the caller's to fix, so it stays an error.
fn refused<T>(e: RoutineError) -> Reply<T> {
    let message = e.to_string();
    match e {
        RoutineError::Conflict { current_revision } => Ok(ToolEnvelope::Conflict {
            message,
            current_revision: Some(current_revision.into()),
        }),
        RoutineError::NotARoutineRun(_) => Ok(ToolEnvelope::Validation {
            message: NOT_A_ROUTINE_RUN.to_string(),
        }),
        RoutineError::Io(_) | RoutineError::Store(_) => Err(message),
        _ => Ok(ToolEnvelope::Validation { message }),
    }
}

fn invalid<T>(message: impl Into<String>) -> Reply<T> {
    Ok(ToolEnvelope::Validation {
        message: message.into(),
    })
}

/// `?` for a service call inside a body: a refusal returns its envelope.
macro_rules! service_try {
    ($call:expr) => {
        match $call {
            Ok(value) => value,
            Err(e) => return refused(e),
        }
    };
}

/// `?` for a parse step inside a body: a bad input returns `validation`.
macro_rules! input_try {
    ($call:expr) => {
        match $call {
            Ok(value) => value,
            Err(message) => return invalid(message),
        }
    };
}

fn parse_key(id: &str) -> Result<RoutineKey, String> {
    // A starter agent's `builtin/<id>` key parses too, but names no routine.
    RoutineKey::parse(id.trim())
        .filter(|key| !key.is_builtin())
        .ok_or_else(|| format!("{id:?} is not a routine id; pass the `id` routine_list returns"))
}

/// A blank optional input is the same as leaving it out.
fn non_blank(raw: Option<&str>) -> Option<&str> {
    raw.map(str::trim).filter(|raw| !raw.is_empty())
}

fn parse_overlap(raw: &str) -> Result<OverlapPolicy, String> {
    serde_json::from_value(serde_json::Value::String(raw.trim().to_string()))
        .map_err(|_| format!("overlap {raw:?} must be coalesce, skip or queue"))
}

fn parse_missed(raw: &str) -> Result<MissedPolicy, String> {
    serde_json::from_value(serde_json::Value::String(raw.trim().to_string()))
        .map_err(|_| format!("missed {raw:?} must be skip or run_once"))
}

/// `one_mac` or `each_mac`.
fn parse_runs_on(raw: &str) -> Result<RunsOn, String> {
    RunsOn::parse(raw).map_err(|e| e.to_string())
}

/// How a routine runs, as the tools report it; `unknown` for a mode this
/// build cannot read (the definition in `routine_get` shows it as written).
fn mode_of(def: &RoutineDef) -> &'static str {
    def.runs_on().map(RunsOn::as_str).unwrap_or("unknown")
}

fn parse_variables(value: serde_json::Value) -> Result<BTreeMap<String, Variable>, String> {
    serde_json::from_value(value).map_err(|e| format!("variables are not valid: {e}"))
}

fn parse_where(raw: &str) -> Where {
    Where {
        kind: raw.trim().to_string(),
        machine: None,
        extra: BTreeMap::new(),
    }
}

/// `fresh` or `wake`.
fn parse_mode(raw: &str) -> Result<RoutineMode, String> {
    let mode = RoutineMode::parse(raw).map_err(|e| e.to_string())?;
    if mode == RoutineMode::Wake && !service::WAKE_DELIVERY {
        return Err(crate::routines::ValidationError::WakeNotAvailable.to_string());
    }
    Ok(mode)
}

/// The mode as stored: `None` when the routine names none.
fn stored_mode(def: &RoutineDef) -> Option<String> {
    Some(def.mode.trim().to_string()).filter(|m| !m.is_empty())
}

/// Whether the agent filter `raw` names `agent`: its full key, or its id.
fn agent_matches(raw: &str, agent: Option<&AgentKey>) -> bool {
    let raw = raw.trim();
    agent.is_some_and(|agent| agent.to_string() == raw || (!raw.contains('/') && agent.id == raw))
}

/// The deprecation note for a create that names no agent from a session
/// launched as a starter.
pub(crate) const STARTER_AGENT_DEPRECATED: &str = "routine_create without `agent` from a session launched as a built-in starter is deprecated and will be refused in the next release: routines belong to an agent, so pass `agent` (see agent_list)";

/// The agent a routine at `key` runs: the one `raw` names, else the saved
/// agent the calling session was launched as. Either must be an agent the
/// caller can see; there is no fallback to a launch profile. A session
/// launched as a starter (what every ad-hoc session is) still defaults to it
/// for one release, with a deprecation note.
fn routine_agent(
    home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    raw: Option<&str>,
    caller: &CallerSession,
    deprecations: &mut Vec<String>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<AgentRef, String> {
    let agent = match (raw, &caller.agent) {
        (Some(raw), _) => AgentRef::new(raw),
        (None, Some(own)) => {
            if own.is_builtin() {
                deprecations.push(STARTER_AGENT_DEPRECATED.to_string());
            }
            AgentRef::to_agent(own, key)
        }
        (None, None) => {
            return Err(
                "agent is required: a routine belongs to an agent; name one (see agent_list)"
                    .to_string(),
            )
        }
    };
    if let Ok(agent_key) = agent.key_for(key) {
        scope.require_agent(home, principal, &agent_key)?;
    }
    agents::visible_agent(home, principal, key, &agent)?;
    Ok(agent)
}

/// The org and team a placement writes into.
fn placed_in(placement: &Placement) -> (String, String) {
    match placement {
        Placement::Personal(owner) => (owner.org.clone(), SCRATCH_TEAM.to_string()),
        Placement::Team { org, team } => (org.clone(), team.clone()),
    }
}

/// A routine id from its name, short enough to leave room for a `-N` suffix.
fn id_from_name(name: &str) -> String {
    let slug = crate::shared::slug::slugify(name);
    let id: String = slug.chars().take(48).collect();
    id.trim_end_matches('-').to_string()
}

/// The routine's schedules in words and their next few fires, soonest first.
fn upcoming(def: &RoutineDef, now: DateTime<Utc>) -> (String, Vec<String>) {
    let mut fires: Vec<DateTime<Utc>> = def
        .triggers
        .iter()
        .filter_map(|trigger| match trigger {
            RoutineTrigger::Schedule(spec) => {
                Some(schedule::next_n(&spec.cron, &spec.tz, now, NEXT_FIRES))
            }
            RoutineTrigger::Event(_) => None,
        })
        .flatten()
        .collect();
    fires.sort();
    fires.dedup();
    fires.truncate(NEXT_FIRES);
    (
        service::schedule_in_words(def, now),
        fires
            .into_iter()
            .map(|at| rfc3339(at.timestamp_millis()))
            .collect(),
    )
}

/// Whether `org/team` is a team this machine has synced, the only place a
/// team routine can be written.
fn team_is_synced(
    home: &Path,
    principal: &Principal,
    org: &str,
    team: &str,
) -> Result<bool, String> {
    let Ok(user_id) = principal.as_str().parse::<u64>() else {
        return Ok(false);
    };
    Ok(
        crate::commands::route_to_context::synced_contexts_in(home, user_id)?
            .iter()
            .any(|c| c.org_slug == org && c.team_name.as_deref() == Some(team)),
    )
}

/// How many of the caller's routines are enabled and fire from this machine.
/// Reads only: a machine with no run coordinator yet has every routine
/// enabled, and none is created to find that out.
fn active_routines(home: &Path, principal: &Principal) -> Result<usize, String> {
    let host = local_host_id(home).map_err(|e| e.to_string())?;
    let armed: Vec<_> = crate::routines::discover_routines(home, principal)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|r| r.armed_here)
        .collect();
    if !crate::coordinator::default_store_path(home).exists() {
        return Ok(armed
            .iter()
            .filter(|r| enabled_here(None, &r.def, &host))
            .count());
    }
    let store = RoutineStateStore::open(home).map_err(|e| e.to_string())?;
    let mut active = 0;
    for routine in armed {
        if store
            .is_enabled(&routine.key, &routine.def, &host)
            .map_err(|e| e.to_string())?
        {
            active += 1;
        }
    }
    Ok(active)
}

fn saved(
    home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    def: &RoutineDef,
    enabled: bool,
    dry_run: bool,
    warnings: Vec<LaunchWarning>,
    effective: RoutineMode,
    now: DateTime<Utc>,
) -> RoutineSaved {
    let (_, mut next_fires) = upcoming(def, now);
    let schedule_in_words = service::schedule_in_words_in(home, principal, key, def, now);
    // A paused routine has no upcoming fires, as `routine_list` reports it.
    if !enabled {
        next_fires.clear();
    }
    RoutineSaved {
        argv: def.is_command().then(|| {
            crate::routines::command::preview(home, principal, key, def, now.timestamp_millis())
                .argv
        }),
        action: def.action,
        command: def.command.clone(),
        triggers: def.triggers.clone(),
        id: key.to_string(),
        agent: def.agent.to_string(),
        agent_key: def.agent.key_for(key).ok().map(|k| k.to_string()),
        mode: stored_mode(def),
        effective_mode: effective.as_str(),
        schedule_in_words,
        next_fires,
        enabled,
        revision: def.revision,
        dry_run,
        warnings,
        deprecations: Vec::new(),
    }
}

fn metadata_trigger(condition: MetadataCondition) -> RoutineTrigger {
    RoutineTrigger::Event(EventSpec {
        kind: EVENT_METADATA_CHANGE.into(),
        from_agent: None,
        role: None,
        metadata: Some(condition),
    })
}

fn event_triggers(events: Vec<RoutineEventParams>) -> Result<Vec<RoutineTrigger>, String> {
    events
        .into_iter()
        .enumerate()
        .map(|(index, event)| {
            if !matches!(event.kind.trim(), EVENT_INBOX_IDLE | EVENT_AGENT_DONE | EVENT_ROLE_PENDING) {
                return Err(format!("events[{index}].kind: use inbox_idle, agent_done or role_pending; use metadataTriggers for metadata changes"));
            }
            let trigger = RoutineTrigger::Event(EventSpec {
                kind: event.kind,
                from_agent: event.from_agent.map(AgentRef::new),
                role: event.role.map(|role| crate::weft::role_inbox::role_name(&role).to_string()),
                metadata: None,
            });
            validate_trigger(&trigger).map_err(|error| {
                use crate::routines::ValidationError;
                let field = match error {
                    ValidationError::RoleNeedsRolePending | ValidationError::EmptyEventRole => "role",
                    ValidationError::FromAgentNeedsAgentDone(_) | ValidationError::InvalidAgent(_) | ValidationError::EmptyAgent => "from_agent",
                    _ => "kind",
                };
                format!("events[{index}].{field}: {error}")
            })?;
            Ok(trigger)
        })
        .collect()
}

/// Replace one family in place, retaining the order of unrelated triggers.
fn replace_triggers(
    triggers: &mut Vec<RoutineTrigger>,
    replacement: impl IntoIterator<Item = RoutineTrigger>,
    belongs: impl Fn(&RoutineTrigger) -> bool,
) {
    let mut replacement = replacement.into_iter();
    *triggers = std::mem::take(triggers)
        .into_iter()
        .filter_map(|trigger| {
            if belongs(&trigger) {
                replacement.next()
            } else {
                Some(trigger)
            }
        })
        .collect();
    triggers.extend(replacement);
}

/// `routine_create`: write a new routine owned by the caller, enabled unless
/// it is a draft. A dry run checks the same things and writes nothing.
pub(crate) fn create(
    home: &Path,
    principal: &Principal,
    placement: Placement,
    caller: &CallerSession,
    author: &Author,
    params: RoutineCreateParams,
    now: DateTime<Utc>,
) -> Reply<RoutineSaved> {
    create_for(
        home,
        principal,
        placement,
        caller,
        author,
        params,
        now,
        &Default::default(),
    )
}

pub(crate) fn create_for(
    home: &Path,
    principal: &Principal,
    placement: Placement,
    caller: &CallerSession,
    author: &Author,
    params: RoutineCreateParams,
    now: DateTime<Utc>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Reply<RoutineSaved> {
    let mut triggers = match params
        .when
        .as_deref()
        .filter(|when| !when.trim().is_empty() && *when != "manual")
    {
        Some(when) => {
            let (cron, tz) = input_try!(
                schedule::parse_when(when, params.tz.as_deref()).map_err(|e| e.to_string())
            );
            vec![RoutineTrigger::Schedule(ScheduleSpec { cron, tz })]
        }
        None => vec![],
    };
    if let Some(conditions) = params.metadata_triggers {
        triggers.extend(conditions.into_iter().map(metadata_trigger));
    }
    if let Some(events) = params.events {
        triggers.extend(input_try!(event_triggers(events)));
    }
    let (org, team) = placed_in(&placement);
    // The space the routine is written into: the team's, or the org's scratch space.
    scope.require_launch_team(home, principal, &org, &team)?;
    let config_dir =
        service::space_config_dir(home, principal, &org, &team).map_err(|e| e.to_string())?;
    let id = match params.id.as_deref().map(str::trim) {
        Some(id) => id.to_string(),
        None => service::first_free_id(&config_dir, &id_from_name(&params.name)),
    };
    let key = RoutineKey::scoped(principal, &org, &team, &id);
    let mut deprecations = Vec::new();
    let action = params.action.unwrap_or_default();
    let command_action = action == crate::routines::model::RoutineAction::Command;
    let agent = if command_action {
        AgentRef::new(params.agent.clone().unwrap_or_default())
    } else {
        input_try!(routine_agent(
            home,
            principal,
            &key,
            non_blank(params.agent.as_deref()),
            caller,
            &mut deprecations,
            scope,
        ))
    };
    if let Ok(key) = agent.key_for(&key) {
        scope.require_agent(home, principal, &key)?;
    }
    let routine_mode = match non_blank(params.mode.as_deref()) {
        Some(raw) => input_try!(parse_mode(raw)).as_str().to_string(),
        None => String::new(),
    };
    let overlap = match params.overlap.as_deref() {
        Some(raw) => input_try!(parse_overlap(raw)),
        None if command_action => OverlapPolicy::Queue,
        None => OverlapPolicy::default(),
    };
    let missed = match params.missed.as_deref() {
        Some(raw) => input_try!(parse_missed(raw)),
        None => MissedPolicy::default(),
    };
    let variables = match params.variables {
        Some(value) => input_try!(parse_variables(value)),
        None => BTreeMap::new(),
    };
    // Written only when given.
    let (mode, runs_on) = match non_blank(params.runs_on.as_deref()) {
        Some(raw) => {
            let mode = input_try!(parse_runs_on(raw));
            (mode, mode.as_str().to_string())
        }
        None => (RunsOn::OneMac, String::new()),
    };
    // Refused here too, so a dry run approves only what the create accepts.
    if mode == RunsOn::EachMac && matches!(placement, Placement::Personal(_)) {
        return invalid(crate::routines::ValidationError::EachMacNeedsTeam.to_string());
    }
    let mut def = RoutineDef {
        action,
        command: params.command.unwrap_or_default(),
        after: params.after,
        id,
        name: params.name.trim().to_string(),
        owner: String::new(),
        agent,
        mode: routine_mode,
        task: params.task.trim().to_string(),
        target: Where {
            machine: non_blank(params.machine.as_deref()).map(str::to_string),
            ..params
                .target
                .as_deref()
                .map(parse_where)
                .unwrap_or_else(Where::team_workspace)
        },
        triggers,
        policy: Policy {
            overlap,
            missed,
            max_runtime: Some(
                non_blank(params.max_runtime.as_deref())
                    .unwrap_or(if command_action {
                        "5m"
                    } else {
                        DEFAULT_MAX_RUNTIME
                    })
                    .to_string(),
            ),
            visible: params.visible.unwrap_or(false),
            max_wakes: params.max_wakes,
            budget_usd: params.budget_usd.map(Into::into),
            max_wakes_per_day: params.max_wakes_per_day,
            min_gap: non_blank(params.min_gap.as_deref()).map(str::to_string),
            ..Policy::default()
        },
        variables,
        revision: 0,
        armed_host: String::new(),
        result_schema: params.result_schema,
        runs_on,
        archived_at: None,
        armed_machine: None,
        arm_epoch: 0,
        handover_from: None,
        archived_with_agent: false,
        migrated: None,
        extra: BTreeMap::new(),
    };
    // A recipe each Mac runs is off on every Mac until its user turns it on.
    let enabled = params.enabled.unwrap_or(mode == RunsOn::OneMac);

    if !is_valid_id(&def.id) {
        return invalid(crate::routines::ValidationError::InvalidId(def.id).to_string());
    }
    // Ids are unique across the space, whichever agent holds one.
    if layout::id_taken(&config_dir, &def.id) {
        return refused(RoutineError::AlreadyExists(key));
    }

    if params.dry_run {
        if let Placement::Team { org, team } = &placement {
            if !team_is_synced(home, principal, org, team)? {
                return invalid(format!("team {org}/{team} is not synced on this machine"));
            }
        }
        if enabled && active_routines(home, principal)? >= service::MAX_ACTIVE_ROUTINES {
            return refused(RoutineError::LimitReached {
                limit: service::MAX_ACTIVE_ROUTINES,
            });
        }
        def.owner = principal.to_string();
        if mode == RunsOn::OneMac {
            def.armed_host = local_host_id(home).map_err(|e| e.to_string())?;
        }
        def.revision = 1;
        if let Err(e) = def
            .validate()
            .and_then(|()| def.validate_agents_for(&key))
            .and_then(|()| def.validate_wake_target())
        {
            return invalid(e.to_string());
        }
        if let Err(e) = crate::routines::command::validate_definition(home, principal, &key, &def)
            .and_then(|_| {
                crate::routines::chain::validate(home, principal, &key, &def)
                    .map_err(|e| e.to_string())
            })
        {
            return invalid(e);
        }
        let warnings = agents::save_warnings_for(home, principal, &key, &def, scope);
        return Ok(ToolEnvelope::Ok(RoutineSaved {
            deprecations,
            ..saved(
                home,
                principal,
                &key,
                &def,
                enabled,
                true,
                warnings,
                service::effective_mode(home, principal, &key, &def),
                now,
            )
        }));
    }

    // The new routine's local state is written before its definition, so a
    // draft neither counts toward the cap nor fires in between, and no state an
    // earlier routine of this id left behind carries over.
    let store = RoutineStateStore::open(home).map_err(|e| e.to_string())?;
    store
        .set_enabled(&key, mode, enabled)
        .map_err(|e| e.to_string())?;
    let written = match service::save_for(
        home,
        principal,
        SaveTarget::Create(placement),
        def,
        author,
        scope,
    ) {
        Ok(written) => written,
        Err(e) => {
            // Nothing was created: leave no switch behind for this id, unless
            // a concurrent create took the id and the state is now its own.
            if !matches!(e, RoutineError::AlreadyExists(_)) {
                store.clear_switch(&key).map_err(|e| e.to_string())?;
            }
            return refused(e);
        }
    };
    Ok(ToolEnvelope::Ok(RoutineSaved {
        deprecations,
        ..saved(
            home,
            principal,
            &written.key,
            &written.def,
            enabled,
            false,
            written.warnings,
            service::effective_mode(home, principal, &written.key, &written.def),
            now,
        )
    }))
}

/// `routine_update`: apply a patch on top of `base_revision`. A stale base is
/// a `conflict` that changes nothing.
/// Whether `def` is a wake with wake limits: a machine whose
/// terminal server is too old to enforce them never runs it.
pub(crate) fn limited_wake(
    home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    def: &RoutineDef,
) -> bool {
    def.policy.has_wake_limits()
        && service::effective_mode(home, principal, key, def) == RoutineMode::Wake
}

/// Whether a machine can take an arm (`machine`, and whether the routine is a
/// wake with wake limits): `Err` refuses, `Ok(Some(warning))` arms with one.
pub(crate) type ArmTargetCheck<'a> = dyn Fn(&str, bool) -> Result<Option<String>, String> + 'a;

/// `routine_update`. `limits_note(machine)`: why a routine with wake limits
/// would fire nowhere on `machine`, added as a warning when the
/// saved routine is armed there.
pub(crate) fn update(
    home: &Path,
    principal: &Principal,
    author: &Author,
    params: RoutineUpdateParams,
    now: DateTime<Utc>,
    limits_note: &dyn Fn(&str) -> Option<String>,
) -> Reply<RoutineSaved> {
    update_for(
        home,
        principal,
        author,
        params,
        now,
        limits_note,
        &Default::default(),
    )
}

pub(crate) fn update_for(
    home: &Path,
    principal: &Principal,
    author: &Author,
    params: RoutineUpdateParams,
    now: DateTime<Utc>,
    limits_note: &dyn Fn(&str) -> Option<String>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Reply<RoutineSaved> {
    let key = input_try!(parse_key(&params.id));
    scope.require_routine(home, principal, &key)?;
    let current = service_try!(service::get_current_for(home, principal, &key, scope));
    let mut def = current.def;
    if let Ok(agent) = def.agent.key_for(&key) {
        scope.require_agent(home, principal, &agent)?;
    }

    if let Some(action) = params.action {
        if action != def.action {
            def.action = action;
            if def.is_command() {
                def.agent = AgentRef::default();
                def.task.clear();
                def.mode.clear();
            } else {
                def.command.clear();
            }
        }
    }
    if let Some(command) = params.command {
        def.command = command;
    }
    if let Some(after) = params.after {
        def.after = after;
    }
    if let Some(name) = params.name {
        def.name = name.trim().to_string();
    }
    if let Some(task) = params.task {
        def.task = task.trim().to_string();
    }
    if let Some(conditions) = params.metadata_triggers {
        replace_triggers(
            &mut def.triggers,
            conditions.into_iter().map(metadata_trigger),
            |trigger| matches!(trigger, RoutineTrigger::Event(event) if event.kind == EVENT_METADATA_CHANGE),
        );
    }
    if let Some(events) = params.events {
        replace_triggers(
            &mut def.triggers,
            input_try!(event_triggers(events)),
            |trigger| {
                matches!(trigger, RoutineTrigger::Event(event)
                if matches!(event.kind.trim(), EVENT_INBOX_IDLE | EVENT_AGENT_DONE | EVENT_ROLE_PENDING))
            },
        );
    }
    let new_tz = params
        .tz
        .as_deref()
        .map(str::trim)
        .filter(|tz| !tz.is_empty());
    if let Some(schema) = params.result_schema {
        def.result_schema = schema;
    }
    if params
        .when
        .as_deref()
        .is_some_and(|when| when.trim().is_empty() || when == "manual")
    {
        def.triggers
            .retain(|t| !matches!(t, RoutineTrigger::Schedule(_)));
    } else if let Some(when) = params.when.as_deref() {
        // A new `when` replaces every schedule, read in the new zone or else
        // the routine's current one.
        let tz = new_tz.map(str::to_string).or_else(|| {
            def.triggers.iter().find_map(|trigger| match trigger {
                RoutineTrigger::Schedule(spec) => Some(spec.tz.clone()),
                RoutineTrigger::Event(_) => None,
            })
        });
        let tz = tz.filter(|tz| !tz.trim().is_empty());
        let (cron, tz) =
            input_try!(schedule::parse_when(when, tz.as_deref()).map_err(|e| e.to_string()));
        def.triggers
            .retain(|trigger| !matches!(trigger, RoutineTrigger::Schedule(_)));
        def.triggers
            .insert(0, RoutineTrigger::Schedule(ScheduleSpec { cron, tz }));
    } else if let Some(tz) = new_tz {
        // A new zone alone moves every schedule into it, keeping each one.
        let mut any = false;
        for trigger in &mut def.triggers {
            if let RoutineTrigger::Schedule(spec) = trigger {
                let (cron, tz) = input_try!(
                    schedule::parse_when(&spec.cron, Some(tz)).map_err(|e| e.to_string())
                );
                *spec = ScheduleSpec { cron, tz };
                any = true;
            }
        }
        if !any {
            return invalid("tz needs a when: this routine has no schedule");
        }
    }
    // Only a new agent is checked: the tab resends the current one with every
    // edit, and a routine whose agent went away must stay editable.
    if let Some(raw) =
        non_blank(params.agent.as_deref()).filter(|raw| AgentRef::new(*raw) != def.agent)
    {
        def.agent = input_try!(routine_agent(
            home,
            principal,
            &key,
            Some(raw),
            &CallerSession::default(),
            &mut Vec::new(),
            scope,
        ));
    }
    if let Some(raw) = non_blank(params.mode.as_deref()) {
        def.mode = input_try!(parse_mode(raw)).as_str().to_string();
    }
    if let Some(raw) = params.target.as_deref() {
        def.target = Where {
            machine: def.target.machine.clone(),
            ..parse_where(raw)
        };
    }
    if let Some(raw) = non_blank(params.machine.as_deref()) {
        def.target.machine = Some(raw.to_string());
    }
    if let Some(raw) = non_blank(params.runs_on.as_deref()) {
        def.runs_on = input_try!(parse_runs_on(raw)).as_str().to_string();
    }
    if let Some(max_runtime) = non_blank(params.max_runtime.as_deref()) {
        def.policy.max_runtime = Some(max_runtime.to_string());
    }
    if let Some(raw) = params.overlap.as_deref() {
        def.policy.overlap = input_try!(parse_overlap(raw));
    }
    if let Some(raw) = params.missed.as_deref() {
        def.policy.missed = input_try!(parse_missed(raw));
    }
    if let Some(visible) = params.visible {
        def.policy.visible = visible;
    }
    if let Some(cap) = params.budget_usd {
        def.policy.budget_usd = cap.map(Into::into);
    }
    if let Some(max_wakes) = params.max_wakes {
        def.policy.max_wakes = max_wakes;
    }
    if let Some(per_day) = params.max_wakes_per_day {
        def.policy.max_wakes_per_day = per_day;
    }
    if let Some(min_gap) = params.min_gap {
        def.policy.min_gap = min_gap
            .as_deref()
            .and_then(|gap| non_blank(Some(gap)))
            .map(str::to_string);
    }
    if let Some(value) = params.variables {
        def.variables = input_try!(parse_variables(value));
    }
    let written = service_try!(service::save_for(
        home,
        principal,
        SaveTarget::Update {
            key,
            base_revision: params.base_revision,
        },
        def,
        author,
        scope,
    ));
    // Only a routine that fires from this Mac has upcoming fires to report
    // here, as `routine_list` shows it; a change of `runs_on` can change that.
    let host = local_host_id(home).map_err(|e| e.to_string())?;
    let enabled = RoutineStateStore::open(home)
        .and_then(|store| store.is_enabled(&written.key, &written.def, &host))
        .map_err(|e| e.to_string())?;
    let mut warnings = written.warnings;
    if let Some(machine) = written.def.armed_machine() {
        if limited_wake(home, principal, &written.key, &written.def) {
            if let Some(message) = limits_note(machine) {
                warnings.push(LaunchWarning {
                    code: crate::agents::launch_check::LaunchWarningCode::WakeLimitsUnenforced,
                    message,
                });
            }
        }
    }
    Ok(ToolEnvelope::Ok(saved(
        home,
        principal,
        &written.key,
        &written.def,
        enabled && crate::routines::is_armed_here(&written.def, &host),
        false,
        warnings,
        service::effective_mode(home, principal, &written.key, &written.def),
        now,
    )))
}

/// `routine_list`: every routine the caller can see, across all their teams
/// and their personal space.
pub(crate) fn list(
    home: &Path,
    principal: &Principal,
    params: RoutineListParams,
    now: DateTime<Utc>,
) -> Reply<Vec<RoutineListItem>> {
    list_for(home, principal, params, now, &Default::default())
}

pub(crate) fn list_for(
    home: &Path,
    principal: &Principal,
    params: RoutineListParams,
    now: DateTime<Utc>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Reply<Vec<RoutineListItem>> {
    let summaries = service_try!(service::list_summaries_for(home, principal, now, scope));
    let filter = non_blank(params.agent.as_deref()).map(str::to_string);
    let mut stale = crate::routines::machine_arm::stale_arms_at(home);
    Ok(ToolEnvelope::Ok(
        summaries
            .into_iter()
            .filter(|summary| {
                filter
                    .as_deref()
                    .is_none_or(|raw| agent_matches(raw, summary.agent_key.as_ref()))
            })
            .map(|summary| RoutineListItem {
                action: summary.def.action,
                command: summary.def.command.clone(),
                id: summary.key.to_string(),
                scope: summary.scope,
                org: summary.key.org.clone(),
                team: summary.key.team.clone(),
                name: summary.def.name.clone(),
                schedule_in_words: summary.schedule_in_words,
                agent: summary.def.agent.to_string(),
                agent_key: summary.agent_key.as_ref().map(|k| k.to_string()),
                unassigned: summary.unassigned,
                duplicate: summary.duplicate,
                mode: stored_mode(&summary.def),
                effective_mode: summary.effective_mode.as_str(),
                target: summary.def.target.kind.clone(),
                runs_on: mode_of(&summary.def),
                enabled: summary.enabled,
                next_run_at: summary.next_run_at.map(rfc3339),
                last_run: summary.last_run.map(|run| LastRunItem {
                    status: run.phase,
                    reason: run.reason,
                    at: rfc3339(run.at),
                }),
                owner: summary.def.owner.clone(),
                owner_name: summary.owner_display,
                created_by: summary.created_by,
                armed_here: summary.armed_here,
                needs_you: summary.needs_you,
                revision: summary.def.revision,
                content_stale: crate::routines::machine_arm::stale_for(
                    &mut stale,
                    &summary.key.to_string(),
                    &summary.def,
                ),
            })
            .collect(),
    ))
}

/// `routine_get`: the definition, its revision and its last few runs.
pub(crate) fn get(
    home: &Path,
    principal: &Principal,
    params: RoutineGetParams,
    now: DateTime<Utc>,
) -> Reply<RoutineGot> {
    get_for(home, principal, params, now, &Default::default())
}

pub(crate) fn get_for(
    home: &Path,
    principal: &Principal,
    params: RoutineGetParams,
    now: DateTime<Utc>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Reply<RoutineGot> {
    let key = input_try!(parse_key(&params.id));
    if !matches!(scope, crate::context_repo::caller_scope::CallerScope::Human) {
        let admitted = service::key_config_dir(home, principal, &key)
            .ok()
            .filter(|dir| scope.allows(dir))
            .and_then(|_| service::routine_dir_of(home, principal, &key).ok())
            .is_some_and(|dir| scope.allows(&dir.join("routine.yml")));
        if !admitted {
            return invalid("Routine not found");
        }
    }
    let detail = match service::get_current_for(home, principal, &key, scope) {
        Err(crate::routines::RoutineError::NotFound(_))
            if !matches!(scope, crate::context_repo::caller_scope::CallerScope::Human) =>
        {
            return invalid("Routine not found")
        }
        result => service_try!(result),
    };
    if let Some(raw) = non_blank(params.agent.as_deref()) {
        if !agent_matches(raw, detail.agent_key.as_ref()) {
            return invalid(format!("routine {key} does not belong to agent {raw:?}"));
        }
    }
    let runs = service_try!(service::runs_for(
        home,
        principal,
        &key,
        RunsPage {
            limit: GET_RUNS,
            before: None,
        },
        scope,
    ));
    Ok(ToolEnvelope::Ok(RoutineGot {
        cap: crate::session_usage::spend_caps::status_with(
            home,
            "routine",
            &key.to_string(),
            detail.def.policy.budget_usd,
            !detail.def.is_command()
                && crate::session_usage::spend_caps::routine_unrecorded(
                    home,
                    principal,
                    detail.agent_key.as_ref(),
                ),
        )?,
        action: detail.def.action,
        command: detail.def.command.clone(),
        id: detail.key.to_string(),
        scope: detail.scope,
        agent_key: detail.agent_key.as_ref().map(|k| k.to_string()),
        unassigned: detail.unassigned,
        mode: stored_mode(&detail.def),
        effective_mode: detail.effective_mode.as_str(),
        owner: detail.def.owner.clone(),
        owner_name: detail.owner_display,
        armed_here: detail.armed_here,
        runs_on: mode_of(&detail.def),
        enabled: detail.enabled,
        revision: detail.def.revision,
        content_stale: crate::routines::machine_arm::stale_for(
            &mut crate::routines::machine_arm::stale_arms_at(home),
            &detail.key.to_string(),
            &detail.def,
        ),
        schedule_in_words: service::schedule_in_words_in(home, principal, &key, &detail.def, now),
        routine: detail.def,
        runs: runs.runs.into_iter().map(RunItem::from).collect(),
    }))
}

/// `routine_set_enabled`: pause or resume on this machine.
pub(crate) fn set_enabled(
    home: &Path,
    principal: &Principal,
    params: RoutineSetEnabledParams,
) -> Reply<EnabledSet> {
    let key = input_try!(parse_key(&params.id));
    service_try!(service::set_enabled(home, principal, &key, params.enabled));
    Ok(ToolEnvelope::Ok(EnabledSet {
        id: key.to_string(),
        enabled: params.enabled,
        machine_warning: None,
    }))
}

/// `routine_arm_on`: move where a routine is armed.
///
/// `check` says whether a machine can take it (in the roster, and its
/// terminal server runs routines without a desktop): `Err` refuses with its
/// reason, `Ok(Some(warning))` arms it with a warning.
pub(crate) fn arm_on(
    home: &Path,
    principal: &Principal,
    author: &Author,
    params: RoutineArmOnParams,
    check: &ArmTargetCheck<'_>,
) -> Reply<ArmedOn> {
    let key = input_try!(parse_key(&params.id));
    // A wake with limits is armed with a warning where they can't
    // be enforced (it fires nowhere there until the server is updated).
    let limited = service::get_current(home, principal, &key)
        .is_ok_and(|current| limited_wake(home, principal, &key, &current.def));
    let machine = params.machine.trim();
    let mut warning = None;
    let placement = match machine {
        "" => {
            return Ok(ToolEnvelope::Validation {
                message: "machine is required: this_mac or a machine id".into(),
            })
        }
        "this_mac" | "local" => service::ArmPlacement::ThisMac,
        id => {
            warning = input_try!(check(id, limited));
            service::ArmPlacement::Machine(id.to_string())
        }
    };
    let saved = service_try!(service::arm_on(home, principal, &key, placement, author));
    let pending = crate::routines::state::RoutineStateStore::open(home)
        .ok()
        .and_then(|store| crate::routines::machine_arm::pending_handovers(&store).ok())
        .is_none_or(|pending| pending.contains(&key.to_string()))
        || saved.def.handover_from.is_some();
    Ok(ToolEnvelope::Ok(ArmedOn {
        id: key.to_string(),
        armed_machine: saved.def.armed_machine.clone(),
        arm_epoch: saved.def.arm_epoch,
        revision: saved.def.revision,
        handover_pending: pending,
        waiting: saved.def.handover_from.as_ref().map(|from| {
            format!(
                "waiting for {} to confirm it stopped firing it",
                from.machine
            )
        }),
        warning,
        machine_warning: None,
    }))
}

/// The refresh an explicit change of a machine-armed routine runs
/// on its machine ([`crate::commands::routines::RefreshAfter`]).
pub(crate) use crate::commands::routines::RefreshAfter;

/// [`update`], then the refresh on its machine; a refresh that could not
/// prepare it there is a `machine_not_prepared` warning.
pub(crate) fn update_and_refresh(
    home: &Path,
    principal: &Principal,
    author: &Author,
    params: RoutineUpdateParams,
    now: DateTime<Utc>,
    limits_note: &dyn Fn(&str) -> Option<String>,
    refresh: RefreshAfter<'_>,
) -> Reply<RoutineSaved> {
    update_and_refresh_for(
        home,
        principal,
        author,
        params,
        now,
        limits_note,
        refresh,
        &Default::default(),
    )
}

pub(crate) fn update_and_refresh_for(
    home: &Path,
    principal: &Principal,
    author: &Author,
    params: RoutineUpdateParams,
    now: DateTime<Utc>,
    limits_note: &dyn Fn(&str) -> Option<String>,
    refresh: RefreshAfter<'_>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Reply<RoutineSaved> {
    let mut reply = update_for(home, principal, author, params, now, limits_note, scope);
    if let Ok(ToolEnvelope::Ok(saved)) = &mut reply {
        saved
            .warnings
            .extend(refresh(&saved.id).map(crate::commands::routines::machine_warning));
    }
    reply
}

fn require_preparation(
    home: &Path,
    principal: &Principal,
    id: &str,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<(), String> {
    scope.require_routine(home, principal, &parse_key(id)?)
}

/// [`set_enabled`], then (a resume) the refresh on its machine.
pub(crate) fn set_enabled_and_refresh(
    home: &Path,
    principal: &Principal,
    params: RoutineSetEnabledParams,
    refresh: RefreshAfter<'_>,
) -> Reply<EnabledSet> {
    set_enabled_and_refresh_for(home, principal, params, refresh, &Default::default())
}

pub(crate) fn set_enabled_and_refresh_for(
    home: &Path,
    principal: &Principal,
    params: RoutineSetEnabledParams,
    refresh: RefreshAfter<'_>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Reply<EnabledSet> {
    require_preparation(home, principal, &params.id, scope)?;
    let mut reply = set_enabled(home, principal, params);
    if let Ok(ToolEnvelope::Ok(set)) = &mut reply {
        if set.enabled {
            set.machine_warning = refresh(&set.id);
        }
    }
    reply
}

/// [`arm_on`], then the refresh on the machine it is armed on now.
pub(crate) fn arm_on_and_refresh(
    home: &Path,
    principal: &Principal,
    author: &Author,
    params: RoutineArmOnParams,
    check: &ArmTargetCheck<'_>,
    refresh: RefreshAfter<'_>,
) -> Reply<ArmedOn> {
    arm_on_and_refresh_for(
        home,
        principal,
        author,
        params,
        check,
        refresh,
        &Default::default(),
    )
}

pub(crate) fn arm_on_and_refresh_for(
    home: &Path,
    principal: &Principal,
    author: &Author,
    params: RoutineArmOnParams,
    check: &ArmTargetCheck<'_>,
    refresh: RefreshAfter<'_>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Reply<ArmedOn> {
    require_preparation(home, principal, &params.id, scope)?;
    let mut reply = arm_on(home, principal, author, params, check);
    if let Ok(ToolEnvelope::Ok(armed)) = &mut reply {
        armed.machine_warning = refresh(&armed.id);
    }
    reply
}

/// [`handover_release`], then the refresh on the machine it moved to.
pub(crate) fn handover_release_and_refresh(
    home: &Path,
    principal: &Principal,
    params: RoutineHandoverReleaseParams,
    refresh: RefreshAfter<'_>,
) -> Reply<HandoverReleased> {
    handover_release_and_refresh_for(home, principal, params, refresh, &Default::default())
}

pub(crate) fn handover_release_and_refresh_for(
    home: &Path,
    principal: &Principal,
    params: RoutineHandoverReleaseParams,
    refresh: RefreshAfter<'_>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Reply<HandoverReleased> {
    require_preparation(home, principal, &params.id, scope)?;
    let mut reply = handover_release(home, principal, params);
    if let Ok(ToolEnvelope::Ok(released)) = &mut reply {
        released.machine_warning = refresh(&released.id);
    }
    reply
}

/// `routine_refresh_on_machine`: prepare a machine-armed
/// routine's agent on its machine and push its arm. `refresh` does it (the
/// app's [`crate::routines::machine_arm_app::refresh`]); its refusal is
/// `validation` with the reason.
pub(crate) fn refresh_on_machine_for(
    home: &Path,
    principal: &Principal,
    params: RoutineRefreshOnMachineParams,
    refresh: &dyn Fn(&RoutineKey) -> Result<String, String>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Reply<RefreshedOnMachine> {
    require_preparation(home, principal, &params.id, scope)?;
    refresh_on_machine(params, refresh)
}

pub(crate) fn refresh_on_machine(
    params: RoutineRefreshOnMachineParams,
    refresh: &dyn Fn(&RoutineKey) -> Result<String, String>,
) -> Reply<RefreshedOnMachine> {
    let key = input_try!(parse_key(&params.id));
    let machine = input_try!(refresh(&key));
    Ok(ToolEnvelope::Ok(RefreshedOnMachine {
        id: key.to_string(),
        machine,
        content_stale: false,
    }))
}

/// `routine_handover_release`: stop waiting on a machine that is gone.
pub(crate) fn handover_release(
    home: &Path,
    principal: &Principal,
    params: RoutineHandoverReleaseParams,
) -> Reply<HandoverReleased> {
    let key = input_try!(parse_key(&params.id));
    let machine = params.machine.trim();
    if machine.is_empty() {
        return invalid("machine is required: the machine the routine waits on");
    }
    if !params.machine_gone {
        return invalid(
            "machineGone must be true: release only a machine that is gone (a machine that still runs may fire the routine once more); otherwise connect it so it can confirm",
        );
    }
    service_try!(service::release_handover(home, principal, &key, machine));
    Ok(ToolEnvelope::Ok(HandoverReleased {
        id: key.to_string(),
        machine: machine.to_string(),
        machine_warning: None,
    }))
}

/// `routine_run_now`: fire it by hand, through the routine's own policy.
#[cfg(test)]
pub(crate) fn run_now(
    home: &Path,
    principal: &Principal,
    params: RoutineRunNowParams,
) -> Reply<RunStarted> {
    run_now_with(home, principal, params, &|key, _| {
        Err(format!(
            "routine {key} is armed on a machine this app cannot reach"
        ))
    })
}

/// Fires a machine-armed routine on its machine (`routine_run_now`).
pub(crate) type FireOnMachine<'a> = &'a dyn Fn(
    &RoutineKey,
    std::collections::BTreeMap<String, String>,
)
    -> Result<crate::routines::admit::Admission, String>;

/// [`run_now`], with a routine armed on a machine fired there
/// (`on_machine`), through the machine's policy and single flight.
#[cfg(test)]
pub(crate) fn run_now_with(
    home: &Path,
    principal: &Principal,
    params: RoutineRunNowParams,
    on_machine: FireOnMachine<'_>,
) -> Reply<RunStarted> {
    run_now_with_for(home, principal, params, on_machine, &Default::default())
}

pub(crate) fn run_now_with_for(
    home: &Path,
    principal: &Principal,
    params: RoutineRunNowParams,
    on_machine: FireOnMachine<'_>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Reply<RunStarted> {
    let key = input_try!(parse_key(&params.id));
    scope.require_routine(home, principal, &key)?;
    let variables = params.variables.unwrap_or_default();
    let admission = match service::run_now(home, principal, &key, variables.clone()) {
        Ok(admission) => admission,
        Err(RoutineError::OnMachine { .. }) => input_try!(on_machine(&key, variables)),
        Err(e) => return refused(e),
    };
    let (outcome, reason, active_run_id) = match admission.outcome {
        Outcome::Queued => ("queued".to_string(), None, None),
        Outcome::Held => ("queued".to_string(), Some("held".to_string()), None),
        Outcome::Coalesced { active_run_id } => {
            ("coalesced".to_string(), None, Some(active_run_id))
        }
        Outcome::Skipped { reason } => (format!("skipped:{reason}"), None, None),
        Outcome::Duplicate => ("duplicate".to_string(), None, None),
    };
    Ok(ToolEnvelope::Ok(RunStarted {
        run_id: admission.run_id,
        outcome,
        reason,
        active_run_id,
    }))
}

/// `routine_runs`: one page of history, newest first, skips and merges
/// included.
pub(crate) fn runs(
    home: &Path,
    principal: &Principal,
    params: RoutineRunsParams,
) -> Reply<RunsPageItem> {
    runs_for(home, principal, params, &Default::default())
}

pub(crate) fn runs_for(
    home: &Path,
    principal: &Principal,
    params: RoutineRunsParams,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Reply<RunsPageItem> {
    let key = input_try!(parse_key(&params.id));
    let page = RunsPage {
        limit: params.limit.unwrap_or(service::DEFAULT_RUNS_PAGE),
        before: params.before,
    };
    let history = match service::runs_for(home, principal, &key, page, scope) {
        Err(crate::routines::RoutineError::NotFound(_))
            if !matches!(scope, crate::context_repo::caller_scope::CallerScope::Human) =>
        {
            return invalid("Routine not found")
        }
        result => service_try!(result),
    };
    Ok(ToolEnvelope::Ok(RunsPageItem {
        runs: history.runs.into_iter().map(RunItem::from).collect(),
        next_before: history.next_before,
    }))
}

/// `routine_run`: one run, by id, with its routine's id and name. A run of a
/// routine the caller cannot see is `validation` (not found).
pub(crate) fn run(
    home: &Path,
    principal: &Principal,
    params: RoutineRunParams,
) -> Reply<RoutineRunGot> {
    run_for(home, principal, params, &Default::default())
}

pub(crate) fn run_for(
    home: &Path,
    principal: &Principal,
    params: RoutineRunParams,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Reply<RoutineRunGot> {
    let run_id = params.run_id.trim();
    if run_id.is_empty() {
        return invalid("runId is required");
    }
    let detail = match service::run_detail_for(home, principal, run_id, scope) {
        Err(crate::routines::RoutineError::RunNotFound(_))
            if !matches!(scope, crate::context_repo::caller_scope::CallerScope::Human) =>
        {
            return invalid("Run not found")
        }
        result => service_try!(result),
    };
    Ok(ToolEnvelope::Ok(RoutineRunGot {
        routine_id: detail.run.routine_key.clone(),
        routine_name: detail.routine_name,
        run: RunItem::from(detail.run),
    }))
}

/// Stop one visible run through the same lifecycle path as the native app.
#[cfg(test)]
pub(crate) fn cancel(
    home: &Path,
    principal: &Principal,
    params: RoutineCancelParams,
    sessions: &dyn crate::routines::finish::Sessions,
) -> Reply<RoutineRunGot> {
    cancel_for(home, principal, params, sessions, &Default::default())
}

pub(crate) fn cancel_for(
    home: &Path,
    principal: &Principal,
    params: RoutineCancelParams,
    sessions: &dyn crate::routines::finish::Sessions,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Reply<RoutineRunGot> {
    let run_id = params.run_id.trim();
    if run_id.is_empty() {
        return invalid("runId is required");
    }
    let stopped = match service::stop_run_for(home, principal, run_id, sessions, scope) {
        Err(RoutineError::RunNotFound(_))
            if !matches!(scope, crate::context_repo::caller_scope::CallerScope::Human) =>
        {
            return invalid("Run not found")
        }
        result => service_try!(result),
    };
    let detail = service_try!(service::run_detail_for(home, principal, run_id, scope));
    if !stopped {
        let phase = &detail.run.phase;
        return invalid(
            if matches!(phase.as_str(), "queued" | "running" | "unknown") {
                format!("routine run {run_id} could not be withdrawn (current phase: {phase}); read routine_run for its latest state")
            } else {
                format!("routine run {run_id} already finished ({phase}); nothing was changed")
            },
        );
    }
    Ok(ToolEnvelope::Ok(detail.into()))
}

/// `routine_delete`: archive it. Its runs stay readable.
pub(crate) fn delete(
    home: &Path,
    principal: &Principal,
    caller: &CallerSession,
    params: RoutineDeleteParams,
) -> Reply<Archived> {
    let key = input_try!(parse_key(&params.id));
    let written = service_try!(service::archive(home, principal, &key, &caller.author()));
    Ok(ToolEnvelope::Ok(Archived {
        id: written.key.to_string(),
        archived: true,
        revision: written.def.revision,
    }))
}

/// `routine_context`: what the routine run `session_id` is running.
pub(crate) fn context(
    home: &Path,
    session_id: &str,
    run_id: Option<&str>,
) -> Reply<RunContextItem> {
    let context = service_try!(service::context_for_session_run(home, session_id, run_id));
    Ok(ToolEnvelope::Ok(RunContextItem {
        run_id: context.run_id,
        routine_id: context.routine_key.to_string(),
        routine_name: context.routine_name,
        revision: context.revision,
        routine: context.routine,
        source: context.source,
        scheduled_at: context.scheduled_at.map(rfc3339),
        fired_at: rfc3339(context.fired_at),
        variables: context.variables,
        trigger_payload: context.trigger_payload,
        coalesced_payloads: context.coalesced_payloads,
        last_run_summary: context.last_run_summary,
        last_run_id: context.last_run_id,
        agent: context.agent,
        content_dir: context
            .content_dir
            .map(|dir| dir.to_string_lossy().into_owned()),
    }))
}

#[cfg(test)]
#[path = "routine_tools_tests.rs"]
mod tests;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoutinePublishResultParams {
    /// Your `$CODESENSE_TAB_SESSION_ID`. Omit it to use the tab this MCP connection
    /// runs in.
    #[serde(default)]
    pub session_id: Option<String>,
    #[schemars(schema_with = "any_json_schema")]
    pub payload: serde_json::Value,
}

fn any_json_schema(_: &mut SchemaGenerator) -> Schema {
    // Claude's MCP client rejects boolean property schemas and drops the entire
    // tool list. An empty schema object accepts the same JSON values as `true`.
    rmcp::schemars::json_schema!({})
}

/// A field an update may set, clear (`null`) or leave alone (omitted).
pub(crate) fn deserialize_patch<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// Preserve the distinction between an omitted patch and explicit JSON null.
pub(crate) fn deserialize_schema_patch<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<serde_json::Value>>, D::Error> {
    Option::<serde_json::Value>::deserialize(deserializer).map(Some)
}

/// Architecture: a write still runs under existing policy; its readback carries caller authority.
pub(crate) fn readback_for<T>(
    result: Reply<T>,
    home: &Path,
    principal: &Principal,
    id: &str,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Reply<T> {
    if RoutineKey::parse(id).is_some_and(|key| !scope.routine_definition(home, principal, &key)) {
        return invalid("Routine not found");
    }
    result
}
