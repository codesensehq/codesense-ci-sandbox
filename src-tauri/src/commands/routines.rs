// SPDX-License-Identifier: MIT
//! The Routines tab's commands: thin wrappers over [`crate::routines::service`].
//!
//! Each command resolves who is asking (the signed-in account, or the local
//! guest) and runs one service fn inside `await_blocking`, so no filesystem or
//! SQLite work happens on the async runtime. Keys travel as the rendered,
//! account-scoped strings the service serializes; times are epoch ms. Every
//! write the service makes emits `routines:changed` itself, so the tab
//! re-reads on that event rather than on a reply.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::agents::launch_check::LaunchWarning;
use crate::commands::await_blocking;
use crate::commands::workspace_open::{
    open_agent, AppOpenWorkspacePort, OpenRequest, OpenStatus, OpenWorkspacePort,
};
use crate::domain::Principal;
use crate::mcp::routine_tools::{
    self, CallerSession, RoutineCreateParams, RoutineSaved, RoutineUpdateParams, ToolEnvelope,
};
use crate::routines::admit::Admission;
use crate::routines::finish::{AppSessions, Sessions};
use crate::routines::service::{
    self, RoutineDetail, RoutineSummary, RunDetail, RunHistory, RunsPage, SavedRoutine,
    SchedulePreview, SchedulerStatus,
};
use crate::routines::{schedule, Author, PersonalOwner, Placement, RoutineError, RoutineKey};

/// Who a command acts for.
#[derive(Debug, Clone)]
pub(crate) struct Caller {
    pub home: PathBuf,
    pub principal: Principal,
}

/// A listed routine plus whether the caller created it, which only decides how
/// the tab credits and groups it: anyone can change any routine.
#[derive(Debug, Clone, Serialize)]
pub struct RoutineRow {
    #[serde(flatten)]
    pub summary: RoutineSummary,
    pub mine: bool,
    /// SW-AGR-5: its wake queued on a busy session (the agent row's `⏰ due`),
    /// in the golden `agent_get.routines[].queued` shape; `null` when nothing
    /// waits or the delivery core is not running.
    pub queued: Option<crate::delivery::read_model::QueuedWake>,
    /// SW-XS-25: its arm on its machine is behind what it is now (its agent
    /// or definition changed since it was prepared there); `null` otherwise.
    /// Refresh on machine prepares it.
    pub content_stale: Option<crate::routines::machine_arm::ContentStale>,
}

/// The routine sentence as the tab's form sends it: *every [when] have [agent]
/// do [task] in [where]*, plus policy and metadata triggers. A create needs name and task.
/// An update changes only fields present, preserving other triggers.
/// A blank `when` removes schedules.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RoutineForm {
    pub action: Option<crate::routines::model::RoutineAction>,
    pub command: Option<Vec<String>>,
    #[serde(deserialize_with = "routine_tools::deserialize_patch")]
    pub after: Option<Option<crate::routines::model::After>>,
    pub name: Option<String>,
    pub task: Option<String>,
    #[serde(deserialize_with = "routine_tools::deserialize_schema_patch")]
    pub result_schema: Option<Option<serde_json::Value>>,
    /// A preset (`every 5m`, `daily 09:00`, …) or a 5-field cron, read in `tz`.
    pub when: Option<String>,
    pub metadata_triggers: Option<Vec<crate::routines::model::MetadataCondition>>,
    pub tz: Option<String>,
    /// The agent that runs it: an agent id in the routine's own space, or an
    /// agent key. A create needs one.
    pub agent: Option<String>,
    #[serde(rename = "where")]
    pub target: Option<String>,
    /// A team routine's `one_mac` or `each_mac`.
    pub runs_on: Option<String>,
    /// `fresh` or `wake`; a create without one gets its agent's default.
    pub mode: Option<String>,
    /// Where a wake looks for the agent: `local`, a machine id, or `any`.
    pub machine: Option<String>,
    pub overlap: Option<String>,
    pub missed: Option<String>,
    pub max_runtime: Option<String>,
    pub visible: Option<bool>,
    /// SW-AGR-13's limits ("Limits" in the form): a number or text sets one,
    /// `null` clears it on an edit, absent keeps it.
    #[serde(deserialize_with = "routine_tools::deserialize_patch")]
    pub max_wakes: Option<Option<u32>>,
    #[serde(deserialize_with = "routine_tools::deserialize_patch")]
    pub budget_usd: Option<Option<crate::session_usage::spend_caps::BudgetUsd>>,
    #[serde(deserialize_with = "routine_tools::deserialize_patch")]
    pub max_wakes_per_day: Option<Option<u32>>,
    #[serde(deserialize_with = "routine_tools::deserialize_patch")]
    pub min_gap: Option<Option<String>>,
}

/// What a save or a restore did. A stale base revision is an answer, not an
/// error: the tab keeps the user's edit and offers a reload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum SaveReply {
    Saved {
        key: String,
        revision: u32,
        /// What a run would be warned about; the save happened.
        warnings: Vec<LaunchWarning>,
    },
    Conflict {
        current_revision: u32,
    },
}

/// Where a tab save writes.
#[derive(Debug, Clone)]
pub(crate) enum FormTarget {
    /// A new routine, enabled or saved as a draft.
    Create { placement: Placement, enabled: bool },
    /// An edit of `key` made against `base_revision`.
    Update { key: String, base_revision: u32 },
}

/// How many upcoming fires the form previews.
const PREVIEW_FIRES: usize = 5;

/// The agent session "Describe it to an agent" opens: a visible team-workspace
/// agent started with a plain request. The `routine_*` MCP tool descriptions
/// carry everything it needs, so it depends on no skill being in the team.
const AUTHORING_AGENT_NAME: &str = "Routine author";
const AUTHORING_PROMPT: &str = "Help me create a CodeSense routine. Use the `routine_*` MCP tools; `routine_create` explains the fields.";

/// Whether the authoring agent is up, and what to tell the user when it is not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuthoringAgent {
    pub opened: bool,
    pub message: String,
}

pub(crate) async fn caller<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Result<Caller, String> {
    let (home, user_id) = super::route_to_context::resolve_ambient(app).await?;
    Ok(Caller {
        home,
        principal: Principal::from(user_id),
    })
}

/// The signed-in profile, for the revisions a tab action writes and for the
/// scratch space a personal save lands in: the ACTIVE org's.
pub(crate) async fn personal_owner(app: &AppHandle) -> Result<PersonalOwner, String> {
    let identity = super::scratch_identity::resolve_scratch_identity(app).await?;
    let org = super::route_to_context::derive_active_context(app)
        .await
        .map(|ctx| ctx.org_slug)
        .map_err(|_| "pick an org first: an Only me item is kept for the open org".to_string())?;
    Ok(identity.owner_in(org))
}

/// Who a tab write is credited to: the signed-in user by handle, else the
/// account id (a guest has no handle).
pub(crate) async fn author(app: &AppHandle, caller: &Caller) -> Author {
    match super::scratch_identity::resolve_scratch_identity(app).await {
        Ok(identity) => Author::user(identity.handle),
        Err(_) => Author::user(caller.principal.as_str()),
    }
}

fn parse_key(key: &str) -> Result<RoutineKey, String> {
    RoutineKey::parse(key).ok_or_else(|| format!("not a routine key: {key:?}"))
}

pub(crate) fn list(caller: &Caller) -> Result<Vec<RoutineRow>, String> {
    let rows =
        service::list_summaries(&caller.home, &caller.principal).map_err(|e| e.to_string())?;
    let mut stale = crate::routines::machine_arm::stale_arms_at(&caller.home);
    Ok(rows
        .into_iter()
        .map(|summary| RoutineRow {
            mine: summary.def.owner == caller.principal.as_str(),
            // Only for the machine it is armed on now.
            content_stale: crate::routines::machine_arm::stale_for(
                &mut stale,
                &summary.key.to_string(),
                &summary.def,
            ),
            summary,
            queued: None,
        })
        .collect())
}

/// Fill each row's `queued` from the delivery core (`queued_for(routine key)`).
pub(crate) fn attach_queued(
    rows: &mut [RoutineRow],
    queued_for: impl Fn(&str) -> Option<crate::delivery::read_model::QueuedWake>,
) {
    for row in rows {
        row.queued = queued_for(&row.summary.key.to_string());
    }
}

pub(crate) fn get(caller: &Caller, key: &str) -> Result<RoutineDetail, String> {
    service::get(&caller.home, &caller.principal, &parse_key(key)?).map_err(|e| e.to_string())
}

pub(crate) fn set_enabled(caller: &Caller, key: &str, enabled: bool) -> Result<(), String> {
    service::set_enabled(&caller.home, &caller.principal, &parse_key(key)?, enabled)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
pub(crate) fn run_now(
    caller: &Caller,
    key: &str,
    variables: BTreeMap<String, String>,
) -> Result<Admission, String> {
    service::run_now(&caller.home, &caller.principal, &parse_key(key)?, variables)
        .map_err(|e| e.to_string())
}

pub(crate) fn runs(
    caller: &Caller,
    key: &str,
    limit: Option<usize>,
    before: Option<String>,
) -> Result<RunHistory, String> {
    let page = RunsPage {
        limit: limit.unwrap_or(service::DEFAULT_RUNS_PAGE),
        before,
    };
    service::runs(&caller.home, &caller.principal, &parse_key(key)?, page)
        .map_err(|e| e.to_string())
}

pub(crate) fn stop_run(
    caller: &Caller,
    run_id: &str,
    sessions: &dyn Sessions,
) -> Result<bool, String> {
    service::stop_run(&caller.home, &caller.principal, run_id, sessions).map_err(|e| e.to_string())
}

pub(crate) fn archive(caller: &Caller, key: &str, author: &Author) -> Result<SavedRoutine, String> {
    service::archive(&caller.home, &caller.principal, &parse_key(key)?, author)
        .map_err(|e| e.to_string())
}

pub(crate) fn copy_to_mine(
    caller: &Caller,
    key: &str,
    owner: PersonalOwner,
    author: &Author,
) -> Result<SavedRoutine, String> {
    service::copy_to_mine(
        &caller.home,
        &caller.principal,
        &parse_key(key)?,
        Placement::Personal(owner),
        author,
    )
    .map_err(|e| e.to_string())
}

pub(crate) fn arm_here(
    caller: &Caller,
    key: &str,
    author: &Author,
) -> Result<SavedRoutine, String> {
    service::arm_here(&caller.home, &caller.principal, &parse_key(key)?, author)
        .map_err(|e| e.to_string())
}

pub(crate) fn run_detail(caller: &Caller, run_id: &str) -> Result<RunDetail, String> {
    service::run_detail(&caller.home, &caller.principal, run_id).map_err(|e| e.to_string())
}

pub(crate) fn needs_you_count(caller: &Caller) -> Result<usize, String> {
    service::needs_you_count(&caller.home, &caller.principal).map_err(|e| e.to_string())
}

pub(crate) fn scheduler_status(caller: &Caller) -> Result<SchedulerStatus, String> {
    service::scheduler_status(&caller.home, &caller.principal).map_err(|e| e.to_string())
}

pub(crate) fn acknowledge_missed(caller: &Caller) -> Result<(), String> {
    service::acknowledge_missed(&caller.home, &caller.principal).map_err(|e| e.to_string())
}

/// A tab save through the same parse the routine MCP tools use, so the form and
/// an agent read `when`, `agent` and `where` identically.
pub(crate) fn save(
    caller: &Caller,
    target: FormTarget,
    form: RoutineForm,
    author: &Author,
    limits_note: &dyn Fn(&str) -> Option<String>,
) -> Result<SaveReply, String> {
    let now = chrono::Utc::now();
    let reply = match target {
        FormTarget::Create { placement, enabled } => {
            let command = form.action == Some(crate::routines::model::RoutineAction::Command);
            let missing = if command {
                "a routine needs a name"
            } else {
                "a routine needs a name and a task"
            };
            let name = form.name.ok_or_else(|| missing.to_string())?;
            let task = if command {
                form.task.unwrap_or_default()
            } else {
                form.task.ok_or_else(|| missing.to_string())?
            };
            let params = RoutineCreateParams {
                action: form.action,
                command: form.command,
                after: form.after.flatten(),
                name,
                task,
                when: form.when,
                metadata_triggers: form.metadata_triggers,
                events: None,
                tz: form.tz,
                agent: form.agent,
                mode: form.mode,
                machine: form.machine,
                target: form.target,
                scope: None,
                org: None,
                team: None,
                runs_on: form.runs_on,
                max_runtime: form.max_runtime,
                overlap: form.overlap,
                missed: form.missed,
                visible: form.visible,
                max_wakes: form.max_wakes.flatten(),
                budget_usd: form.budget_usd.flatten(),
                max_wakes_per_day: form.max_wakes_per_day.flatten(),
                min_gap: form.min_gap.flatten(),
                variables: None,
                result_schema: form.result_schema.flatten(),
                id: None,
                enabled: Some(enabled),
                dry_run: false,
                session_id: None,
            };
            routine_tools::create(
                &caller.home,
                &caller.principal,
                placement,
                &CallerSession::default(),
                author,
                params,
                now,
            )
        }
        FormTarget::Update { key, base_revision } => {
            let params = RoutineUpdateParams {
                action: form.action,
                command: form.command,
                after: form.after,
                id: key,
                base_revision,
                name: form.name,
                task: form.task,
                when: form.when,
                metadata_triggers: form.metadata_triggers,
                events: None,
                tz: form.tz,
                agent: form.agent,
                mode: form.mode,
                machine: form.machine,
                target: form.target,
                runs_on: form.runs_on,
                max_runtime: form.max_runtime,
                overlap: form.overlap,
                missed: form.missed,
                visible: form.visible,
                max_wakes: form.max_wakes,
                budget_usd: form.budget_usd,
                max_wakes_per_day: form.max_wakes_per_day,
                min_gap: form.min_gap,
                variables: None,
                result_schema: form.result_schema,
                session_id: None,
            };
            routine_tools::update(
                &caller.home,
                &caller.principal,
                author,
                params,
                now,
                limits_note,
            )
        }
    };
    save_reply(reply?)
}

/// SW-XS-25: the refresh an explicit change of a machine-armed routine runs
/// on its machine: what to tell the person when it could not prepare it
/// there (`None`: prepared, or not armed on a machine this Mac manages).
pub(crate) type RefreshAfter<'a> = &'a dyn Fn(&str) -> Option<String>;

/// A refresh's warning as a save warning.
pub(crate) fn machine_warning(message: String) -> LaunchWarning {
    LaunchWarning {
        code: crate::agents::launch_check::LaunchWarningCode::MachineNotPrepared,
        message,
    }
}

/// A saved edit (or restore) of `reply`'s routine, refreshed on its machine.
pub(crate) fn with_refresh(reply: SaveReply, refresh: RefreshAfter<'_>) -> SaveReply {
    match reply {
        SaveReply::Saved {
            key,
            revision,
            mut warnings,
        } => {
            warnings.extend(refresh(&key).map(machine_warning));
            SaveReply::Saved {
                key,
                revision,
                warnings,
            }
        }
        other => other,
    }
}

/// Pause or resume; a resume refreshes it on its machine. The warning, if
/// any, says it was not prepared there.
pub(crate) fn set_enabled_and_refresh(
    caller: &Caller,
    key: &str,
    enabled: bool,
    refresh: RefreshAfter<'_>,
) -> Result<Option<String>, String> {
    set_enabled(caller, key, enabled)?;
    Ok(if enabled { refresh(key) } else { None })
}

/// Whether a machine can take an arm (`check_arm_target`).
pub(crate) type ArmCheck<'a> = &'a dyn Fn(&str, bool) -> Result<Option<String>, String>;

/// Move where `key` fires (`machine` absent: this Mac), then refresh it on
/// the machine it is armed on now.
pub(crate) fn arm_on_and_refresh(
    caller: &Caller,
    key: &str,
    machine: Option<String>,
    author: &Author,
    check: ArmCheck<'_>,
    refresh: RefreshAfter<'_>,
) -> Result<SavedRoutine, String> {
    let routine = parse_key(key)?;
    let placement = match machine
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
    {
        Some(machine) => {
            // In the roster, and able to run routines without a desktop.
            let limited = service::get_current(&caller.home, &caller.principal, &routine)
                .is_ok_and(|current| {
                    routine_tools::limited_wake(
                        &caller.home,
                        &caller.principal,
                        &routine,
                        &current.def,
                    )
                });
            if let Some(warning) = check(&machine, limited)? {
                log::warn!(target: "routines", "routine {routine} armed on {machine}: {warning}");
            }
            service::ArmPlacement::Machine(machine)
        }
        None => service::ArmPlacement::ThisMac,
    };
    let mut saved = service::arm_on(&caller.home, &caller.principal, &routine, placement, author)
        .map_err(|e| e.to_string())?;
    // SW-XS-25: arming it on a machine prepares it there.
    saved
        .warnings
        .extend(refresh(&saved.key.to_string()).map(machine_warning));
    Ok(saved)
}

/// Release a handover from a machine that is gone, then refresh the routine
/// on the machine it moved to (its arm waited on that handover).
pub(crate) fn release_and_refresh(
    caller: &Caller,
    key: &str,
    machine: &str,
    refresh: RefreshAfter<'_>,
) -> Result<Option<String>, String> {
    service::release_handover(&caller.home, &caller.principal, &parse_key(key)?, machine)
        .map_err(|e| e.to_string())?;
    Ok(refresh(key))
}

fn save_reply(envelope: ToolEnvelope<RoutineSaved>) -> Result<SaveReply, String> {
    match envelope {
        ToolEnvelope::Ok(saved) => Ok(SaveReply::Saved {
            key: saved.id,
            revision: saved.revision,
            warnings: saved.warnings,
        }),
        ToolEnvelope::Conflict {
            current_revision: Some(current),
            ..
        } => Ok(SaveReply::Conflict {
            current_revision: u32::try_from(current).unwrap_or(u32::MAX),
        }),
        ToolEnvelope::Conflict { message, .. } | ToolEnvelope::Validation { message } => {
            Err(message)
        }
        ToolEnvelope::NotSyncedLocally => Err("that team is not synced on this Mac".into()),
        ToolEnvelope::UnauthorizedTeam => Err("you are not a member of that team".into()),
    }
}

/// `when` in words and its next fires, read exactly as a save would read it.
pub(crate) fn preview_schedule(when: &str, tz: Option<&str>) -> Result<SchedulePreview, String> {
    let (cron, tz) = schedule::parse_when(when, tz).map_err(|e| e.to_string())?;
    service::preview_schedule(&cron, &tz, PREVIEW_FIRES).map_err(|e| e.to_string())
}

/// Write revision `revision`'s body as a new revision, made against
/// `base_revision`.
pub(crate) fn restore_revision(
    caller: &Caller,
    key: &str,
    revision: u32,
    base_revision: u32,
    author: &Author,
) -> Result<SaveReply, String> {
    match service::restore(
        &caller.home,
        &caller.principal,
        &parse_key(key)?,
        revision,
        base_revision,
        author,
    ) {
        Ok(saved) => Ok(SaveReply::Saved {
            key: saved.key.to_string(),
            revision: saved.def.revision,
            warnings: saved.warnings,
        }),
        Err(RoutineError::Conflict { current_revision }) => {
            Ok(SaveReply::Conflict { current_revision })
        }
        Err(e) => Err(e.to_string()),
    }
}

/// The open "Describe it to an agent" asks for: the visible team-workspace
/// launch agents use, primed with the authoring request.
pub(crate) fn authoring_request() -> OpenRequest {
    OpenRequest {
        target: "team-workspace".to_string(),
        name: Some(AUTHORING_AGENT_NAME.to_string()),
        initial_prompt: Some(AUTHORING_PROMPT.to_string()),
        mode: Some("headed".to_string()),
        ..OpenRequest::default()
    }
}

/// Every routine the caller can see, with its next and last run.
#[tauri::command]
pub async fn routine_list(app: AppHandle) -> Result<Vec<RoutineRow>, String> {
    let caller = caller(&app).await?;
    let delivery = tauri::Manager::try_state::<crate::delivery::app::DeliveryState>(&app)
        .map(|state| state.inner().clone());
    await_blocking("routine_list", move || {
        let mut rows = list(&caller)?;
        if let Some(delivery) = delivery {
            attach_queued(&mut rows, |key| delivery.queued_for_routine(key));
        }
        Ok(rows)
    })
    .await
}

/// One routine with its revision history.
#[tauri::command]
pub async fn routine_get(app: AppHandle, key: String) -> Result<RoutineDetail, String> {
    let caller = caller(&app).await?;
    await_blocking("routine_get", move || get(&caller, &key)).await
}

/// Flip this Mac's switch for a routine: pause or resume a shared job, or
/// turn a recipe each Mac runs on or off here. SW-XS-25: a resume of a
/// machine-armed routine prepares it there; the reply is what to tell the
/// person when it could not (else `null`).
#[tauri::command]
pub async fn routine_set_enabled(
    app: AppHandle,
    key: String,
    enabled: bool,
) -> Result<Option<String>, String> {
    let caller = caller(&app).await?;
    await_blocking("routine_set_enabled", move || {
        set_enabled_and_refresh(&caller, &key, enabled, &|key| {
            refresh_after_change(&app, &caller, key)
        })
    })
    .await
}

/// [`crate::routines::machine_arm_app::refresh_after_change`], as `caller`.
fn refresh_after_change(app: &AppHandle, caller: &Caller, key: &str) -> Option<String> {
    crate::routines::machine_arm_app::refresh_after_change(
        app,
        &caller.home,
        &caller.principal,
        key,
    )
}

/// Fire one of the caller's routines now, through its policy like any fire.
#[tauri::command]
pub async fn routine_run_now(
    app: AppHandle,
    key: String,
    variables: Option<BTreeMap<String, String>>,
    frame_id: Option<String>,
    page_path: Option<String>,
) -> Result<Admission, String> {
    let scope = crate::context_repo::caller_scope::for_management_view(
        &app,
        page_path.as_deref(),
        frame_id.as_deref(),
    )?;
    let caller = caller(&app).await?;
    await_blocking("routine_run_now", move || {
        let variables = variables.unwrap_or_default();
        let parsed = parse_key(&key)?;
        scope.require_routine(&caller.home, &caller.principal, &parsed)?;
        match service::run_now(&caller.home, &caller.principal, &parsed, variables.clone()) {
            // SW-AGR-8: armed on a machine, it fires there.
            Err(service::RoutineError::OnMachine { .. }) => {
                crate::routines::machine_arm_app::fire_now(
                    &app,
                    &caller.home,
                    &caller.principal,
                    &parsed,
                    variables,
                )
            }
            other => other.map_err(|e| e.to_string()),
        }
    })
    .await
}

/// The most runs one Recent read returns per routine; its merged and skipped
/// fires are capped the same, separately.
pub const MAX_RECENT_PER_ROUTINE: usize = 50;

/// Every routine's runs since `since_ms`, newest first, at most `per_routine`
/// of each and as many of its merged or skipped fires (the Schedule's Recent,
/// SW-AGR-5).
pub(crate) fn recent(
    caller: &Caller,
    since_ms: i64,
    per_routine: Option<usize>,
) -> Result<Vec<crate::routines::runs::RoutineRun>, String> {
    let cap = per_routine
        .unwrap_or(MAX_RECENT_PER_ROUTINE)
        .clamp(1, MAX_RECENT_PER_ROUTINE);
    service::recent_runs(&caller.home, &caller.principal, since_ms, cap).map_err(|e| e.to_string())
}

/// Every routine's runs since `sinceMs` in one bounded read, at most
/// `perRoutine` of each (and as many of its merged or skipped fires).
#[tauri::command]
pub async fn routine_runs_since(
    app: AppHandle,
    since_ms: i64,
    per_routine: Option<usize>,
) -> Result<Vec<crate::routines::runs::RoutineRun>, String> {
    let caller = caller(&app).await?;
    await_blocking("routine_runs_since", move || {
        recent(&caller, since_ms, per_routine)
    })
    .await
}

/// A page of a routine's runs, newest first, skips and merges included.
#[tauri::command]
pub async fn routine_runs(
    app: AppHandle,
    key: String,
    limit: Option<usize>,
    before: Option<String>,
) -> Result<RunHistory, String> {
    let caller = caller(&app).await?;
    await_blocking("routine_runs", move || runs(&caller, &key, limit, before)).await
}

/// End a run `cancelled`, then kill its sessions. False when it had already
/// ended.
#[tauri::command]
pub async fn routine_stop_run(app: AppHandle, run_id: String) -> Result<bool, String> {
    let caller = caller(&app).await?;
    let sessions = AppSessions::new(app);
    await_blocking("routine_stop_run", move || {
        stop_run(&caller, &run_id, &sessions)
    })
    .await
}

/// The team a new routine is for ("Who can use it: ‹Team›").
#[derive(Debug, Clone, Deserialize)]
pub struct RoutineTeamTarget {
    pub org: String,
    pub team: String,
}

/// Create a routine from the sentence form (no `key`), or edit one the caller
/// owns against `base_revision`. A create is enabled unless `enabled` is false,
/// which saves a draft. A create goes to `team` when given ("Who can use it:
/// ‹Team›"), else to the caller's scratch space for the open org ("Only me").
#[tauri::command]
pub async fn routine_save(
    app: AppHandle,
    key: Option<String>,
    base_revision: Option<u32>,
    form: RoutineForm,
    enabled: Option<bool>,
    team: Option<RoutineTeamTarget>,
) -> Result<SaveReply, String> {
    if key.is_some() && team.is_some() {
        return Err("a team is chosen for a new routine only; an edit stays where it is".into());
    }
    let caller = caller(&app).await?;
    let author = author(&app, &caller).await;
    let target = match key {
        Some(key) => FormTarget::Update {
            key,
            base_revision: base_revision
                .ok_or_else(|| "an edit needs the revision it was made against".to_string())?,
        },
        None => FormTarget::Create {
            placement: match team {
                Some(RoutineTeamTarget { org, team }) => Placement::Team { org, team },
                None => Placement::Personal(personal_owner(&app).await?),
            },
            enabled: enabled.unwrap_or(true),
        },
    };
    await_blocking("routine_save", move || {
        let reply = save(&caller, target, form, &author, &|machine| {
            crate::routines::machine_arm_app::wake_limits_note(&app, machine)
        })?;
        // SW-XS-25: an edit of a machine-armed routine prepares it there.
        Ok(with_refresh(reply, &|key| {
            refresh_after_change(&app, &caller, key)
        }))
    })
    .await
}

/// A schedule in words with its next five fires, for the form's live preview.
#[tauri::command]
pub async fn routine_preview_schedule(
    when: String,
    tz: Option<String>,
) -> Result<SchedulePreview, String> {
    await_blocking("routine_preview_schedule", move || {
        preview_schedule(&when, tz.as_deref())
    })
    .await
}

/// Restore an older revision of one of the caller's routines as a new
/// revision.
#[tauri::command]
pub async fn routine_restore_revision(
    app: AppHandle,
    key: String,
    revision: u32,
    base_revision: u32,
) -> Result<SaveReply, String> {
    let caller = caller(&app).await?;
    let author = author(&app, &caller).await;
    await_blocking("routine_restore_revision", move || {
        let reply = restore_revision(&caller, &key, revision, base_revision, &author)?;
        Ok(with_refresh(reply, &|key| {
            refresh_after_change(&app, &caller, key)
        }))
    })
    .await
}

/// Open the authoring agent in the UI-active team's workspace through `port`.
pub(crate) async fn open_authoring_agent(
    port: &dyn OpenWorkspacePort,
) -> Result<AuthoringAgent, String> {
    let (active, host) = port.targeted().await?;
    let result = open_agent(host.as_ref(), &active, &authoring_request()).await?;
    Ok(AuthoringAgent {
        opened: matches!(result.status, OpenStatus::Opened | OpenStatus::AlreadyLive),
        message: result.message,
    })
}

/// Open a visible agent in the active team's workspace, asked to help the user
/// create a routine, so the user can describe one in words.
#[tauri::command]
pub async fn routine_open_authoring_agent(app: AppHandle) -> Result<AuthoringAgent, String> {
    open_authoring_agent(&AppOpenWorkspacePort { app }).await
}

/// Archive one of the caller's routines; its history stays readable.
#[tauri::command]
pub async fn routine_archive(app: AppHandle, key: String) -> Result<SavedRoutine, String> {
    let caller = caller(&app).await?;
    let author = author(&app, &caller).await;
    await_blocking("routine_archive", move || archive(&caller, &key, &author)).await
}

/// Copy a visible routine (typically a teammate's) into the caller's personal
/// routines, armed on this machine.
#[tauri::command]
pub async fn routine_copy_to_mine(app: AppHandle, key: String) -> Result<SavedRoutine, String> {
    let caller = caller(&app).await?;
    let owner = personal_owner(&app).await?;
    let author = Author::user(owner.username.clone());
    await_blocking("routine_copy_to_mine", move || {
        copy_to_mine(&caller, &key, owner, &author)
    })
    .await
}

/// Make this machine the one that fires one of the caller's routines.
#[tauri::command]
pub async fn routine_arm_here(app: AppHandle, key: String) -> Result<SavedRoutine, String> {
    let caller = caller(&app).await?;
    let author = author(&app, &caller).await;
    await_blocking("routine_arm_here", move || arm_here(&caller, &key, &author)).await
}

/// SW-AGR-8: move where one of the caller's routines fires: this Mac
/// (`machine` absent) or a machine's terminal server. The editor's control.
#[tauri::command]
pub async fn routine_arm_on(
    app: AppHandle,
    key: String,
    machine: Option<String>,
) -> Result<SavedRoutine, String> {
    let caller = caller(&app).await?;
    let author = author(&app, &caller).await;
    await_blocking("routine_arm_on", move || {
        arm_on_and_refresh(
            &caller,
            &key,
            machine,
            &author,
            &|machine, limited| {
                crate::routines::machine_arm_app::check_arm_target(&app, machine, limited)
            },
            &|key| refresh_after_change(&app, &caller, key),
        )
    })
    .await
}

/// SW-XS-25: "Refresh on machine": prepare a machine-armed routine's agent on
/// its machine from what it is now, and push its arm with the launch recipe.
/// The only way content that changed since the last preparation reaches the
/// machine. Returns the machine.
#[tauri::command]
pub async fn routine_refresh_on_machine(app: AppHandle, key: String) -> Result<String, String> {
    parse_key(&key)?;
    let caller = caller(&app).await?;
    await_blocking("routine_refresh_on_machine", move || {
        crate::routines::machine_arm_app::refresh_explicit(
            &app,
            &caller.home,
            &caller.principal,
            &key,
        )
    })
    .await
}

/// SW-AGR-8: stop waiting for `machine`, which is gone, to confirm it
/// stopped firing a routine moved off it.
#[tauri::command]
pub async fn routine_handover_release(
    app: AppHandle,
    key: String,
    machine: String,
) -> Result<Option<String>, String> {
    let caller = caller(&app).await?;
    await_blocking("routine_handover_release", move || {
        release_and_refresh(&caller, &key, &machine, &|key| {
            refresh_after_change(&app, &caller, key)
        })
    })
    .await
}

/// One run row with its routine's name.
#[tauri::command]
pub async fn routine_run_detail(app: AppHandle, run_id: String) -> Result<RunDetail, String> {
    let caller = caller(&app).await?;
    await_blocking("routine_run_detail", move || run_detail(&caller, &run_id)).await
}

/// How many of the caller's routines ended their latest run needing them.
#[tauri::command]
pub async fn routine_needs_you_count(app: AppHandle) -> Result<usize, String> {
    let caller = caller(&app).await?;
    await_blocking("routine_needs_you_count", move || needs_you_count(&caller)).await
}

/// Occurrences missed while the app was closed or asleep, since last
/// acknowledged.
#[tauri::command]
pub async fn routine_scheduler_status(app: AppHandle) -> Result<SchedulerStatus, String> {
    let caller = caller(&app).await?;
    await_blocking("routine_scheduler_status", move || {
        scheduler_status(&caller)
    })
    .await
}

/// Clear the missed count; the skipped runs stay in each routine's history.
#[tauri::command]
pub async fn routine_acknowledge_missed(app: AppHandle) -> Result<(), String> {
    let caller = caller(&app).await?;
    await_blocking("routine_acknowledge_missed", move || {
        acknowledge_missed(&caller)
    })
    .await
}

#[cfg(test)]
mod tests;

/// Read private cached data without launching any work.
#[tauri::command]
pub async fn routine_result<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    key: String,
    frame_id: Option<String>,
    page: Option<String>,
) -> Result<crate::routines::results::ResultStatus, String> {
    let scope = crate::context_repo::caller_scope::for_management_view(
        &app,
        page.as_deref(),
        frame_id.as_deref(),
    )?;
    let caller = caller(&app).await?;
    await_blocking("routine_result", move || {
        let key = crate::routines::RoutineKey::parse(&key).ok_or("Invalid routine key")?;
        crate::routines::results::read_for(&caller.home, &caller.principal, &key, &scope)
    })
    .await
}
/// Cancel only this principal's live result runs for an authorized routine key.
#[tauri::command]
pub async fn routine_cancel_result(app: AppHandle, key: String) -> Result<usize, String> {
    let caller = caller(&app).await?;
    let sessions = AppSessions::new(app);
    await_blocking("routine_cancel_result", move || {
        let key = crate::routines::RoutineKey::parse(&key).ok_or("Invalid routine key")?;
        crate::routines::results::cancel(&caller.home, &caller.principal, &key, &sessions)
    })
    .await
}

#[tauri::command]
pub async fn routine_preview_command(
    app: AppHandle,
    command: Vec<String>,
    key: Option<String>,
    org: String,
    team: String,
    after: Option<crate::routines::model::After>,
) -> Result<crate::routines::command::Preview, String> {
    let caller = caller(&app).await?;
    await_blocking("routine_preview_command", move || {
        let key = key
            .as_deref()
            .map(parse_key)
            .transpose()?
            .unwrap_or_else(|| RoutineKey::new(&org, &team, "preview"));
        let def: crate::routines::RoutineDef = serde_json::from_value(serde_json::json!({
            "id": key.id, "name":"Preview", "action":"command", "command":command,
            "where":{"kind":"team_workspace"}, "after": after
        }))
        .map_err(|e| e.to_string())?;
        Ok(crate::routines::command::preview(
            &caller.home,
            &caller.principal,
            &key,
            &def,
            crate::routines::finish::now_ms(),
        ))
    })
    .await
}
