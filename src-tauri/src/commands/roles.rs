// SPDX-License-Identifier: MIT
use super::{await_blocking, route_to_context};
use crate::queue_roles::RoleConfig;
use tauri::{AppHandle, Emitter};

pub(crate) const ROLES_CHANGED: &str = "roles:changed";

pub(crate) fn content(
    resolution: &route_to_context::ContextResolution,
) -> Result<std::path::PathBuf, String> {
    let ctx = match resolution {
        route_to_context::ContextResolution::Ok(ctx) => ctx,
        _ => return Err("team content is not available on this machine".into()),
    };
    crate::agents::roots::team_content_dir(
        &ctx.codesense_home,
        &route_to_context::ActiveContext::principal(&ctx),
        &ctx.org_slug,
        ctx.team_name.as_deref().ok_or("no team selected")?,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn roles_list(app: AppHandle, team_key: String) -> Result<Vec<RoleConfig>, String> {
    roles_list_for(app, team_key, Default::default()).await
}

pub(crate) async fn roles_list_for<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    team_key: String,
    scope: crate::context_repo::caller_scope::CallerScope,
) -> Result<Vec<RoleConfig>, String> {
    let (org, team) = team_key.split_once(':').ok_or("expected org:team")?;
    let ctx = route_to_context::derive_context_for(&app, org, team).await?;
    await_blocking("roles_list", move || {
        crate::queue_roles::list_for(&content(&ctx)?, &scope)
    })
    .await
}

#[tauri::command]
pub async fn role_save(
    app: AppHandle,
    team_key: String,
    config: RoleConfig,
) -> Result<RoleConfig, String> {
    let (org, team) = team_key.split_once(':').ok_or("expected org:team")?;
    let ctx = route_to_context::derive_context_for(&app, org, team).await?;
    let result = await_blocking("role_save", move || {
        crate::queue_roles::save(&content(&ctx)?, config)
    })
    .await?;
    let _ = app.emit(ROLES_CHANGED, ());
    super::agent_read_model::agent_state_changed(&app);
    Ok(result)
}
