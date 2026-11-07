// SPDX-License-Identifier: MIT
//! Slot dashboards' commands: the active set a page registers, the registry the
//! frontend pushes, and the person's side of list and run. The MCP tools
//! `command_list` / `command_run` call the same registry functions as an agent.

use tauri::{Emitter, Manager};

use crate::dashboard_stream::frame::FrameStore;
use crate::panes::commands::{Actor, CommandRegistry, CommandRow, SlotCommand, COMMAND_RUN_EVENT};

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisteredCommands {
    /// The calling frame's dashboard key (`DashboardRef::key`).
    pub dashboard: String,
    pub active: Vec<String>,
}

/// The host side of the `commands.register` verb, run after admission.
#[tauri::command]
pub fn commands_register(
    app: tauri::AppHandle,
    origin: String,
    token: String,
    ids: Vec<String>,
) -> Result<RegisteredCommands, String> {
    let frames = app.state::<std::sync::Arc<FrameStore>>();
    let grant = frames
        .resolve(&origin, &token)
        .ok_or("denied: frame identity")?;
    let frame = frames.by_origin(&origin).ok_or("denied: frame identity")?;
    let declared = crate::panes::declaration_at(std::path::Path::new(&frame.document_path))
        .map(|d| d.commands)
        .unwrap_or_default();
    let active = app
        .state::<CommandRegistry>()
        .register(&grant.dashboard, &declared, &ids)?;
    Ok(RegisteredCommands {
        dashboard: grant.dashboard,
        active,
    })
}

/// The frontend registry's slot-dashboard commands with their effective chords.
#[tauri::command]
pub fn commands_sync(app: tauri::AppHandle, commands: Vec<SlotCommand>) -> Result<(), String> {
    app.state::<CommandRegistry>().set_commands(commands)
}

/// The local profile's dashboard policy, which decides humans-only and grants.
pub(crate) async fn current_policy(app: &tauri::AppHandle) -> Result<crate::verbs::Policy, String> {
    let (_, principal) = crate::commands::route_to_context::resolve_ambient(app).await?;
    crate::verbs::commands::read_policy(app.clone(), &principal.to_string()).await
}

/// List the slot dashboards' commands for `actor`.
pub(crate) async fn list(app: &tauri::AppHandle, actor: Actor) -> Result<Vec<CommandRow>, String> {
    let policy = current_policy(app).await?;
    app.state::<CommandRegistry>().list(&policy, actor)
}

/// Run a slot dashboard's command for `actor`: one event the frontend dispatches
/// like a chord (reveal, then deliver once after the page is ready).
pub(crate) async fn run(
    app: &tauri::AppHandle,
    actor: Actor,
    id: &str,
    args: Option<serde_json::Value>,
) -> Result<(), String> {
    // Only an agent's run is checked against the policy; a person's always may.
    let policy = match actor {
        Actor::Agent => current_policy(app).await?,
        Actor::Human => crate::verbs::Policy::default(),
    };
    app.state::<CommandRegistry>()
        .run(&policy, actor, id, args, |event| {
            app.emit(COMMAND_RUN_EVENT, event)
                .map_err(|e| e.to_string())
        })
}

/// The `commands.list` page verb: the person's view of every slot command.
#[tauri::command]
pub async fn commands_list(app: tauri::AppHandle) -> Result<Vec<CommandRow>, String> {
    list(&app, Actor::Human).await
}

/// The `commands.run` page verb. A page acts for the person, who may run any
/// active command; the frame's own grant was checked at admission.
#[tauri::command]
pub async fn commands_run(
    app: tauri::AppHandle,
    id: String,
    args: Option<serde_json::Value>,
) -> Result<(), String> {
    run(&app, Actor::Human, &id, args).await
}
