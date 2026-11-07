// SPDX-License-Identifier: MIT
//! Strict tool contracts remain discoverable on unsupported desktop platforms.
#[path = "remote_agent_task/types.rs"]
mod types;
use super::remote_authority::RemoteAuthority;
use crate::commands::route_to_context::ActiveContext;
pub(crate) use types::{CancelParams, Outcome, ReadParams, StartParams};
pub(crate) async fn start(
    _app: tauri::AppHandle,
    _ctx: ActiveContext,
    _params: StartParams,
    _caller: Option<RemoteAuthority>,
) -> Result<Outcome, String> {
    Err("capabilityUnavailable: local-account remote tasks require a Unix desktop".into())
}
pub(crate) async fn read(
    _app: tauri::AppHandle,
    _ctx: ActiveContext,
    _params: ReadParams,
    _caller: Option<RemoteAuthority>,
) -> Result<Outcome, String> {
    Err("capabilityUnavailable: local-account remote tasks require a Unix desktop".into())
}
pub(crate) async fn cancel(
    _app: tauri::AppHandle,
    _ctx: ActiveContext,
    _params: CancelParams,
    _caller: Option<RemoteAuthority>,
) -> Result<Outcome, String> {
    Err("capabilityUnavailable: local-account remote tasks require a Unix desktop".into())
}
