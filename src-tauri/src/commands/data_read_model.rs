// SPDX-License-Identifier: MIT
//! The page bridge's reads of routines and agent runs. Each returns exactly what its MCP
//! twin returns (the `data` of a routine tool's envelope; a run tool's whole reply), from
//! the same function, so a dashboard and an agent see the same data:
//!
//! - `routines()` → `routine_list`, `routine({key})` → `routine_get`,
//!   `routineRuns({key, limit?, before?})` → `routine_runs`, `routineRun({runId})` →
//!   `routine_run`.
//! - `runs({limit?})` → `list_runs`, `run({runId, maxArtifactBytes?})` → `read_run`,
//!   `runEvidence({runId, storyId?})` → `read_run_evidence`, `runWorkProduct({runId})` →
//!   `read_run_work_product`.
//!
//! A routine tool's refusal (`validation`, `conflict`, ...) is an error naming it.

use tauri::AppHandle;

use super::await_blocking;
use super::routines::caller;
use crate::commands::sprint_run::run_evidence::RunEvidence;
use crate::commands::sprint_run::run_store::{RunOutcomeRead, RunWorkProductRead};
use crate::mcp::routine_tools::{
    self, RoutineGetParams, RoutineGot, RoutineListItem, RoutineRunGot, RoutineRunParams,
    RoutineRunsParams, RunsPageItem, ToolEnvelope,
};
use crate::mcp::run_tools::{
    self, ListRunsParams, ReadRunEvidenceParams, ReadRunParams, ReadRunWorkProductParams, RunRead,
};
use crate::session_usage::{self, Filter, UsageReport};

/// A routine tool's reply as a bridge result: its `data`, or an error naming the refusal.
pub(crate) fn envelope_data<T>(reply: routine_tools::Reply<T>) -> Result<T, String> {
    match reply? {
        ToolEnvelope::Ok(data) => Ok(data),
        ToolEnvelope::Validation { message } => Err(format!("validation: {message}")),
        ToolEnvelope::Conflict { message, .. } => Err(format!("conflict: {message}")),
        ToolEnvelope::NotSyncedLocally => Err("not_synced_locally".into()),
        ToolEnvelope::UnauthorizedTeam => Err("unauthorized_team".into()),
    }
}

/// Every routine this user can see (the bridge's `routines()`; `routine_list`).
#[tauri::command]
pub async fn routine_read_list(
    app: AppHandle,
    frame_id: Option<String>,
    page: Option<String>,
) -> Result<Vec<RoutineListItem>, String> {
    let caller = caller(&app).await?;
    let scope = crate::context_repo::caller_scope::for_management_view(
        &app,
        page.as_deref(),
        frame_id.as_deref(),
    )?;
    await_blocking("routine_read_list", move || {
        envelope_data(routine_tools::list_for(
            &caller.home,
            &caller.principal,
            Default::default(),
            chrono::Utc::now(),
            &scope,
        ))
    })
    .await
}

/// One routine in full (the bridge's `routine({key})`; `routine_get`).
#[tauri::command]
pub async fn routine_read_get(
    app: AppHandle,
    key: String,
    frame_id: Option<String>,
    page: Option<String>,
) -> Result<RoutineGot, String> {
    let caller = caller(&app).await?;
    let scope = crate::context_repo::caller_scope::for_management_view(
        &app,
        page.as_deref(),
        frame_id.as_deref(),
    )?;
    await_blocking("routine_read_get", move || {
        envelope_data(routine_tools::get_for(
            &caller.home,
            &caller.principal,
            RoutineGetParams {
                id: key,
                agent: None,
            },
            chrono::Utc::now(),
            &scope,
        ))
    })
    .await
}

/// A routine's run history (the bridge's `routineRuns(...)`; `routine_runs`).
#[tauri::command]
pub async fn routine_read_runs<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    key: String,
    limit: Option<usize>,
    before: Option<String>,
    frame_id: Option<String>,
    page: Option<String>,
) -> Result<RunsPageItem, String> {
    let caller = caller(&app).await?;
    let scope = crate::context_repo::caller_scope::for_management_view(
        &app,
        page.as_deref(),
        frame_id.as_deref(),
    )?;
    await_blocking("routine_read_runs", move || {
        envelope_data(routine_tools::runs_for(
            &caller.home,
            &caller.principal,
            RoutineRunsParams {
                id: key,
                limit,
                before,
            },
            &scope,
        ))
    })
    .await
}

/// One routine run (the bridge's `routineRun({runId})`; `routine_run`).
#[tauri::command]
pub async fn routine_read_run<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    run_id: String,
    frame_id: Option<String>,
    page: Option<String>,
) -> Result<RoutineRunGot, String> {
    let caller = caller(&app).await?;
    let scope = crate::context_repo::caller_scope::for_management_view(
        &app,
        page.as_deref(),
        frame_id.as_deref(),
    )?;
    await_blocking("routine_read_run", move || {
        envelope_data(routine_tools::run_for(
            &caller.home,
            &caller.principal,
            RoutineRunParams { run_id },
            &scope,
        ))
    })
    .await
}

/// The latest outcome of every agent run, newest first (the bridge's `runs(...)`;
/// `list_runs`).
#[tauri::command]
pub async fn run_read_list(limit: Option<u32>) -> Result<Vec<RunOutcomeRead>, String> {
    run_tools::list_runs(ListRunsParams { limit }).await
}

/// One agent run in full (the bridge's `run(...)`; `read_run`).
#[tauri::command]
pub async fn run_read(run_id: String, max_artifact_bytes: Option<u32>) -> Result<RunRead, String> {
    run_tools::read_run(ReadRunParams {
        run_id,
        max_artifact_bytes,
    })
    .await
}

/// One agent run's evidence (the bridge's `runEvidence(...)`; `read_run_evidence`).
#[tauri::command]
pub async fn run_read_evidence(
    run_id: String,
    story_id: Option<String>,
) -> Result<Option<RunEvidence>, String> {
    run_tools::read_run_evidence(ReadRunEvidenceParams { run_id, story_id }).await
}

/// The branch an agent run produced (the bridge's `runWorkProduct(...)`;
/// `read_run_work_product`).
#[tauri::command]
pub async fn run_read_work_product(run_id: String) -> Result<Option<RunWorkProductRead>, String> {
    run_tools::read_run_work_product(ReadRunWorkProductParams { run_id }).await
}

/// Story sessions, tasks and totals, shared with the MCP story_changes verb.
#[tauri::command]
pub async fn story_changes<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    params: session_usage::story_changes::Params,
    frame_id: Option<String>,
    page: Option<String>,
) -> Result<serde_json::Value, String> {
    let scope = crate::context_repo::caller_scope::for_management_view(&app, page.as_deref(), frame_id.as_deref())?;
    session_usage::story_changes::current(&params, &scope).await
}

/// Many stories' cached totals, shared with the MCP story_totals verb.
#[tauri::command]
pub async fn story_totals<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    params: session_usage::story_totals::Params,
    frame_id: Option<String>,
    page: Option<String>,
) -> Result<serde_json::Value, String> {
    let scope = crate::context_repo::caller_scope::for_management_view(&app, page.as_deref(), frame_id.as_deref())?;
    session_usage::story_totals::current(&params, &scope).await
}

/// Read-only source diff, revalidated for every page or native viewer request.
#[tauri::command]
pub async fn open_diff_read<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    params: crate::attribution::diff::Params,
    frame_id: Option<String>,
    page: Option<String>,
) -> Result<serde_json::Value, String> {
    let scope = crate::context_repo::caller_scope::for_management_view(&app, page.as_deref(), frame_id.as_deref())?;
    let (home, user_id) = crate::commands::route_to_context::resolve_ambient(&app).await?;
    await_blocking("open_diff_read", move || Ok(crate::attribution::diff::read(&home, &crate::domain::Principal::from(user_id), &scope, &params))).await
}

/// Committed attribution; the page and MCP share one resolver.
#[tauri::command]
pub async fn why_line<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    params: crate::attribution::why_line::Params,
    frame_id: Option<String>,
    page: Option<String>,
) -> Result<serde_json::Value, String> {
    why_line_holding(app, params, frame_id, page, None).await
}

pub(crate) async fn why_line_holding<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    params: crate::attribution::why_line::Params,
    frame_id: Option<String>,
    page: Option<String>,
    hold: crate::attribution::why_line::ReadHold,
) -> Result<serde_json::Value, String> {
    let scope = crate::context_repo::caller_scope::for_management_view(
        &app,
        page.as_deref(),
        frame_id.as_deref(),
    )?;
    let (home, user_id) = crate::commands::route_to_context::resolve_ambient(&app).await?;
    crate::attribution::why_line::resolve_holding(
        &home,
        &crate::domain::Principal::from(user_id),
        &params,
        &scope,
        session_usage::launch_stories(&app),
        hold,
    )
    .await
}

/// This machine's attribution and capture status, as `describe_setup` serves it.
#[tauri::command]
pub async fn setup<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Result<serde_json::Value, String> {
    let doctor = crate::mcp::setup_tools::read_local_doctor(&app).await?;
    Ok(crate::attribution::setup::local(&doctor))
}

/// One joined session, from the same backend as the MCP session_read verb.
#[tauri::command]
pub async fn session_read<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    params: session_usage::session_read::Params,
    frame_id: Option<String>,
    page: Option<String>,
) -> Result<serde_json::Value, String> {
    let scope = crate::context_repo::caller_scope::for_management_view(&app, page.as_deref(), frame_id.as_deref())?;
    session_usage::session_read::current(&params, &scope).await
}

/// Token use and spend per agent session (the bridge's `sessionUsage({sinceMs?, untilMs?,
/// sessionId?, cwd?, story?, limit?})`; `session_read_usage`), each session tied to its story. Read from each agent CLI's own reports, so it
/// covers every session the CLI reported. Claude and Codex report only the sessions CodeSense launched
/// (their telemetry is switched on at launch). This is what the MCP `session_usage` tool returns too.
/// Times are epoch milliseconds, and count by whole local day.
#[tauri::command]
pub async fn session_read_usage<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    filter: Filter,
    frame_id: Option<String>,
    page: Option<String>,
) -> Result<UsageReport, String> {
    let scope = crate::context_repo::caller_scope::for_management_view(
        &app,
        page.as_deref(),
        frame_id.as_deref(),
    )?;
    let launch_stories = session_usage::launch_stories(&app);
    await_blocking("session_read_usage", move || {
        session_usage::read_usage_for(&filter, launch_stories, &scope)
    })
    .await
}

/// Set a model's rate from a local day, and price the unpriced usage it covers (the page's
/// `setModelRate`; `session_set_model_rate`). Its optional cache-read price is used where it is not
/// 0.1x input, and its `long` prices for a long prompt. Returns how many costs it changed.
#[tauri::command]
pub async fn session_set_model_rate(
    params: session_usage::SetModelRateParams,
) -> Result<usize, String> {
    await_blocking("session_set_model_rate", move || {
        session_usage::set_model_rate(params)
    })
    .await
}

/// Every stored rate, the seeded list prices and the ones the user set, by model prefix and then by
/// day (the page's `modelRates`; `session_read_model_rates`).
#[tauri::command]
pub async fn session_read_model_rates() -> Result<Vec<session_usage::StoredRate>, String> {
    await_blocking("session_read_model_rates", session_usage::model_rates).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_is_an_error_naming_it_and_ok_is_the_data() {
        assert_eq!(envelope_data(Ok(ToolEnvelope::Ok(3))), Ok(3));
        assert_eq!(
            envelope_data::<()>(Ok(ToolEnvelope::Validation {
                message: "routine run x not found".into()
            })),
            Err("validation: routine run x not found".into())
        );
        assert_eq!(
            envelope_data::<()>(Ok(ToolEnvelope::UnauthorizedTeam)),
            Err("unauthorized_team".into())
        );
        assert_eq!(envelope_data::<()>(Err("disk".into())), Err("disk".into()));
    }

    /// The bridge reads a routine run through the same body as `routine_run`, so the
    /// page gets exactly the agent's `data`.
    #[test]
    fn the_bridge_routine_reads_are_the_mcp_data() {
        use crate::routines::test_support::{alice, TempHome};
        let temp = TempHome::new();
        let home = temp.codesense_home();
        let now = chrono::Utc::now();
        let bridge = envelope_data(routine_tools::list(
            &home,
            &alice(),
            Default::default(),
            now,
        ))
        .unwrap();
        let mcp = serde_json::to_value(
            routine_tools::list(&home, &alice(), Default::default(), now).unwrap(),
        )
        .unwrap();
        assert_eq!(mcp["status"], "ok");
        assert_eq!(serde_json::to_value(bridge).unwrap(), mcp["data"]);
        let missing = envelope_data(routine_tools::run(
            &home,
            &alice(),
            RoutineRunParams {
                run_id: "nope".into(),
            },
        ));
        assert!(missing.unwrap_err().starts_with("validation: "));
    }
}

fn require_native_spend_window(label: &str) -> Result<(), String> {
    if label != "main" {
        return Err("Spend caps and rates are accepted from the CodeSense window".into());
    }
    Ok(())
}

/// A person's native Save. No embedded-page or MCP verb exposes this acceptance.
#[tauri::command]
pub async fn spend_cap_set(
    app: AppHandle,
    webview: tauri::Webview,
    scope_kind: String,
    key: String,
    budget_usd: Option<session_usage::spend_caps::BudgetUsd>,
) -> Result<(), String> {
    require_native_spend_window(webview.label())?;
    let caller = caller(&app).await?;
    await_blocking("spend_cap_set", move || {
        session_usage::spend_caps::set_current(
            &caller.home,
            &caller.principal,
            &scope_kind,
            &key,
            budget_usd.map(Into::into),
        )
    })
    .await
}

/// Native-only approval. Embedded pages have no bridge verb for this command.
#[tauri::command]
pub async fn spend_cap_accept(
    app: AppHandle,
    webview: tauri::Webview,
    scope_kind: String,
    key: String,
    budget_usd: Option<session_usage::spend_caps::BudgetUsd>,
    keep: Option<bool>,
) -> Result<(), String> {
    require_native_spend_window(webview.label())?;
    let caller = caller(&app).await?;
    await_blocking("spend_cap_accept", move || {
        let apply = if keep.unwrap_or(false) {
            session_usage::spend_caps::keep_current
        } else {
            session_usage::spend_caps::accept_current
        };
        apply(
            &caller.home,
            &caller.principal,
            &scope_kind,
            &key,
            budget_usd.map(Into::into),
        )
    })
    .await
}

#[tauri::command]
pub async fn spend_rate_accept(
    webview: tauri::Webview,
    request: session_usage::SetModelRateParams,
) -> Result<usize, String> {
    require_native_spend_window(webview.label())?;
    await_blocking("spend_rate_accept", move || {
        session_usage::UsageStore::open(&session_usage::store_path()?)?.accept_rate(
            &request.model_prefix,
            &request.effective_from,
            &request,
        )
    })
    .await
}

#[cfg(test)]
mod native_spend_window_tests {
    #[test]
    fn cap_and_rate_acceptance_share_the_native_window_boundary() {
        assert!(super::require_native_spend_window("main").is_ok());
        assert!(super::require_native_spend_window("dashboard")
            .unwrap_err()
            .contains("CodeSense window"));
    }
}
