// SPDX-License-Identifier: MIT
use super::{await_blocking, route_to_context::resolve_ambient};
use crate::context_repo::scratch_space;
use crate::domain::Principal;
use serde::Serialize;
use tauri::Manager;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivateNavSource {
    pub repo_root: String,
    pub content_path: String,
    pub prefix: String,
    pub baseline_started: bool,
    pub baseline_error: Option<String>,
}

pub(crate) fn resolve_at(
    home: &std::path::Path,
    principal: &Principal,
    org: &str,
) -> Result<PrivateNavSource, String> {
    let (space, _) = scratch_space::ensure_org_space(home, principal, org)?;
    let root = scratch_space::scratch_root(home, principal);
    let content = space.content_dir();
    let prefix = content
        .strip_prefix(&root)
        .map_err(|e| e.to_string())?
        .to_string_lossy()
        .into_owned();
    let baseline = scratch_space::ensure_content_baseline(home, principal, org);
    Ok(PrivateNavSource {
        repo_root: root.to_string_lossy().into_owned(),
        content_path: content.to_string_lossy().into_owned(),
        prefix,
        baseline_started: baseline.as_ref().copied().unwrap_or(false),
        baseline_error: baseline.err(),
    })
}

#[tauri::command]
pub async fn resolve_private_nav_source(
    app: tauri::AppHandle,
    org_slug: String,
) -> Result<PrivateNavSource, String> {
    let (home, user) = resolve_ambient(&app).await?;
    let watch_home = home.clone();
    let source = await_blocking("resolve_private_nav_source", move || {
        let principal = Principal::from(user);
        if !crate::mcp::setup_tools::synced_org_slugs(&home, user)?.contains(&org_slug) {
            return Err(format!("{org_slug} is no longer attached"));
        }
        resolve_at(&home, &principal, &org_slug)
    })
    .await?;
    if let Some(watch) = app.try_state::<super::watch::WatcherState>() {
        super::scratch_watch::register_scratch_watch(&app, &watch, &watch_home, user);
    }
    Ok(source)
}

/// Open the same Panels menu used by the app's View menu.
#[tauri::command]
pub fn open_panels_menu(app: tauri::AppHandle, window: tauri::Window) -> Result<(), String> {
    use tauri::menu::ContextMenu;
    let menu = app.menu().ok_or("The app menu is not available")?;
    for item in menu.items().map_err(|e| e.to_string())? {
        let Some(view) = item.as_submenu() else {
            continue;
        };
        if view.text().map_err(|e| e.to_string())? != "View" {
            continue;
        }
        for item in view.items().map_err(|e| e.to_string())? {
            if let Some(panels) = item.as_submenu() {
                if panels.text().map_err(|e| e.to_string())? == "Panels" {
                    return panels.popup(window).map_err(|e| e.to_string());
                }
            }
        }
    }
    Err("No panes are available yet. An agent can create a private pane for you.".into())
}
