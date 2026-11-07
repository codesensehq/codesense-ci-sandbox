// SPDX-License-Identifier: MIT
use serde_json::Value;
use tauri::Manager;
fn frame<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    id: &str,
) -> Result<crate::dashboard_stream::frame::FrameRecord, String> {
    app.state::<std::sync::Arc<crate::dashboard_stream::frame::FrameStore>>()
        .by_id(id)
        .ok_or_else(|| "denied: frame released".into())
}
#[tauri::command]
pub async fn records_call<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    frame_id: String,
    operation: String,
    args: Value,
) -> Result<Value, String> {
    let scope = crate::context_repo::caller_scope::for_page(&app, None, Some(&frame_id))?;
    let frame = frame(&app, &frame_id)?;
    let index = app
        .try_state::<crate::context_query::ContextIndexService>()
        .map(|i| i.inner().clone());
    crate::commands::await_blocking("records.call", move || {
        execute_frame_for(&frame, &operation, &args, index, scope)
    })
    .await
}
#[tauri::command]
pub async fn records_prepare_view<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    frame_id: String,
    collection: String,
) -> Result<String, String> {
    let scope = crate::context_repo::caller_scope::for_page(&app, None, Some(&frame_id))?;
    let frame = frame(&app, &frame_id)?;
    crate::verbs::check_frame_capability(
        &frame,
        "records.list",
        &serde_json::json!({"collection":collection}),
    )
    .map_err(str::to_owned)?;
    let binding = frame
        .records_binding
        .ok_or("unavailable: frame has no collection roots")?
        .for_caller(scope);
    crate::commands::await_blocking("records.prepare", move || {
        super::projection::prepare(&app, &binding, &collection)
    })
    .await
}
/// The host's own view of one of the person's private collections in `org_slug`
/// (the native Notes list), for `liveHub.watch("records", viewKey)`. It is the
/// same projection a pane subscribes to, prepared from the ambient profile.
#[tauri::command]
pub async fn records_prepare_native_view(
    app: tauri::AppHandle,
    org_slug: String,
    collection: String,
) -> Result<String, String> {
    let (home, principal) = crate::commands::route_to_context::resolve_ambient(&app).await?;
    crate::commands::await_blocking("records.prepare_native", move || {
        let binding =
            super::binding::Binding::from_context(&home, &principal.into(), &org_slug, None)?;
        super::projection::prepare(&app, &binding, &collection)
    })
    .await
}

pub(crate) fn execute_frame(
    frame: &crate::dashboard_stream::frame::FrameRecord,
    operation: &str,
    args: &Value,
    index: Option<crate::context_query::ContextIndexService>,
) -> Result<Value, String> {
    execute_frame_for(frame, operation, args, index, Default::default())
}

fn execute_frame_for(
    frame: &crate::dashboard_stream::frame::FrameRecord,
    operation: &str,
    args: &Value,
    index: Option<crate::context_query::ContextIndexService>,
    scope: crate::context_repo::caller_scope::CallerScope,
) -> Result<Value, String> {
    crate::verbs::check_frame_capability(frame, &format!("records.{operation}"), args)
        .map_err(str::to_owned)?;
    let binding = frame
        .records_binding
        .as_ref()
        .ok_or("unavailable: frame has no collection roots")?;
    super::binding::execute(
        &binding.clone().for_caller(scope),
        operation,
        args,
        &super::Actor::human(),
        index,
        Some(&frame.effective_grant),
    )
}
