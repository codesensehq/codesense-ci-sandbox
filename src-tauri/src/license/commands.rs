// SPDX-License-Identifier: MIT
//! The update Tauri commands (SW-PB-9 AC-6). SV-PBF-278: the `license_` names stay, but there is
//! no license. Each one is a thin call into the managed `StaticChannel` or the updater;
//! `license_state` and the `license://state` event carry the update view.

use std::sync::Arc;

use tauri::State;

use super::static_channel::StaticChannel;
use super::updates::Updates;
use super::view::View;

#[tauri::command]
pub fn license_state(channel: State<'_, Arc<StaticChannel>>) -> View {
    channel.view()
}

/// Checks the channel's manifest now. A newer build starts a download, straight away even after
/// recent failures, because the user asked; this command returns without waiting for it, and
/// `license://state` carries `update` once the build is staged.
#[tauri::command]
pub fn license_check_now(app: tauri::AppHandle, channel: State<'_, Arc<StaticChannel>>) -> View {
    let checking = channel.inner().clone();
    tauri::async_runtime::spawn(async move {
        checking.check(&app, true).await;
    });
    channel.view()
}

/// SW-PB-23 AC-1: Restart to update, the user's own action. Asks the same gate as the install
/// at quit, then exits through the usual path; the exit installs the staged build and
/// relaunches. Returns an error, and changes nothing, when the gate refuses.
#[tauri::command]
pub fn license_restart_to_update(
    app: tauri::AppHandle,
    channel: State<'_, Arc<StaticChannel>>,
    updater: State<'_, Arc<Updates>>,
) -> Result<(), String> {
    let refresh = || channel.refresh();
    super::restart_to_update(&app, &refresh, &updater)
        .map(|_| ())
        .map_err(str::to_string)
}

/// SW-PB-23: how many headless (no-terminal) agent runs are going. Restart to update asks
/// before it ends them.
#[tauri::command]
pub fn license_headless_runs(app: tauri::AppHandle) -> usize {
    super::headless_runs(&app)
}

/// SW-PB-23: the Restart to update button gave up waiting for the exit; the restart is no
/// longer armed, so a later plain quit doesn't relaunch.
#[tauri::command]
pub fn license_cancel_restart(
    channel: State<'_, Arc<StaticChannel>>,
    updater: State<'_, Arc<Updates>>,
) {
    updater.disarm_restart();
    channel.refresh();
}
