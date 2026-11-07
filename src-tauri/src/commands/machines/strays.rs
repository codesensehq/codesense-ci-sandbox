// SPDX-License-Identifier: MIT
//! SW-AGR-19 PR-7: This Mac's stray terminal servers — every
//! `codesense-session-server` the user runs besides the one this app is
//! attached to — listed for Machines, and each stopped only after its own
//! confirmation (see [`crate::terminal::session_server::strays`]).

use tauri::Manager;

use crate::terminal::session_server::strays::{self, StrayServer, StrayStop, StrayTarget};
use crate::terminal::session_server::{SessionServerState, SIDECAR_BIN};

/// The pid of the server this app is attached to, if any.
fn attached_pid(app: &tauri::AppHandle) -> Option<u32> {
    app.try_state::<SessionServerState>()
        .and_then(|state| state.connected())
        .map(|client| client.server_pid())
}

/// SW-AGR-19: This Mac's stray terminal servers.
#[tauri::command]
pub async fn list_stray_terminal_servers(
    app: tauri::AppHandle,
) -> Result<Vec<StrayServer>, String> {
    crate::commands::await_blocking("list_stray_terminal_servers", move || {
        let own_root = app
            .try_state::<SessionServerState>()
            .and_then(|state| state.root().map(|root| root.to_path_buf()));
        let bundled = crate::commands::deps::resolve_sidecar_source(SIDECAR_BIN).ok();
        Ok(strays::strays(
            attached_pid(&app),
            bundled.as_deref(),
            own_root.as_deref(),
        ))
    })
    .await
}

/// SW-AGR-19: stop the stray the person confirmed — only if the same process
/// still serves the same root, and never the server this app is attached to.
#[tauri::command]
pub async fn stop_stray_terminal_server(
    app: tauri::AppHandle,
    target: StrayTarget,
) -> Result<StrayStop, String> {
    crate::commands::await_blocking("stop_stray_terminal_server", move || {
        let own_root = app
            .try_state::<SessionServerState>()
            .and_then(|state| state.root().map(|root| root.to_path_buf()));
        let stopped = strays::stop(&target, attached_pid(&app), own_root.as_deref());
        log::info!(
            target: "machines",
            "stray terminal server {} at {}: {stopped:?}",
            target.pid,
            target.root
        );
        Ok(stopped)
    })
    .await
}
