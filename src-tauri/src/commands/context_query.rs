// SPDX-License-Identifier: MIT
//! SW-OC-26 — the `query_context` command: the app-side door to the one
//! context query engine (`crate::context_query`). Used by `csquery` blocks,
//! right panes, the in-app stream hub and the dashboard bridge's
//! `queryContext`/`subscribeContext`. Runs on the blocking pool so a `text`
//! query's bounded body scan never touches the async executor.

use crate::context_query::{self, ContextIndexService, ContextQuery, ContextQueryResult};

/// `page`: the path of the page the app answers for (a dashboard, a `csquery`
/// block, a pane) — private items then come only from that page's own org
/// (an empty or unknown path sees none). Required: every app-side caller is a
/// page, so no app path gets the agent rules. The page itself never sets it:
/// the trusted caller does.
#[tauri::command]
pub async fn query_context<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    query: ContextQuery,
    page: String,
    frame_id: Option<String>,
    caller_page: Option<String>,
    caller_team: Option<String>,
) -> Result<ContextQueryResult, String> {
    use tauri::Manager;
    let service = app
        .try_state::<ContextIndexService>()
        .ok_or("the context index is not running")?
        .inner()
        .clone();
    let mut viewer =
        context_query::viewer_for(&app, &service, Some(page), context_query::uses_me(&query));
    viewer.scope = crate::context_repo::caller_scope::for_host(
        &app,
        caller_page.as_deref().or(viewer.page.as_deref()),
        frame_id.as_deref(),
        caller_team.as_deref(),
    )?;
    super::await_blocking("query_context", move || {
        context_query::run_query(&service, &query, &viewer).map_err(|e| e.to_string())
    })
    .await
}
