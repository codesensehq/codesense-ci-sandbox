// SPDX-License-Identifier: MIT
//! Captured saved-Agent MCP grants. Desktop endpoints stay local; remote stdio uses a typed projection.
pub(crate) mod config;
mod http;
pub(crate) mod identity;
pub(crate) mod local_grant;
pub(crate) mod native;
mod transport;
use config::Grant;
use std::sync::Arc;
use tauri::Manager;

pub(crate) async fn capture(
    app: &tauri::AppHandle,
    record: &crate::commands::pty_layout::RemoteShellRecord,
    parent: Option<&super::remote_authority::RemoteAuthority>,
) -> Result<(Arc<Grant>, Option<String>), String> {
    let Some(agent) = record.agent.as_ref() else {
        return Ok((Arc::new(Grant::new()), None));
    };
    let (owner_id, team_key, workspace_id) =
        (record.owner_id, &record.team_key, &record.workspace_id);
    let (org, team) = team_key.split_once(':').ok_or("invalid bound MCP team")?;
    let context =
        match crate::commands::route_to_context::derive_context_for(app, org, team).await? {
            crate::commands::route_to_context::ContextResolution::Ok(ctx)
                if ctx.user_id == owner_id =>
            {
                ctx
            }
            _ => return Err("bound MCP context unavailable".into()),
        };
    if let Some(parent) = parent {
        parent
            .validate()
            .await
            .map_err(|_| "original MCP caller retired")?;
    }
    let scope = codesense_pty_core::agent_cli::descriptor(agent.runtime)
        .mcp_scope
        .clone()
        .ok_or("runtime MCP scope unavailable")?;
    let allow = Some(agent.mcp_allow.clone());
    let seed = serde_json::to_vec(agent).map_err(|e| e.to_string())?;
    use sha2::{Digest, Sha256};
    let mut revision = Sha256::new();
    revision.update(seed);
    if allow.as_ref().is_some_and(Vec::is_empty) {
        return Ok((
            Arc::new(Grant::new()),
            Some(format!("{:x}", revision.finalize())),
        ));
    }
    let viewer = app.state::<super::state::McpViewerState>().get();
    let row = viewer
        .get("workspaces")
        .and_then(|v| v.get(team_key))
        .and_then(|v| v.as_array())
        .and_then(|rows| {
            rows.iter()
                .find(|row| row.get("id").and_then(|v| v.as_str()) == Some(workspace_id))
        })
        .ok_or("bound desktop MCP workspace unavailable")?;
    let cwd = desktop_config_root(row, context.team_content_dir())
        .ok_or("bound desktop MCP workspace unavailable")?;
    if !cwd.is_absolute() {
        return Err("desktop MCP workspace path must be absolute".into());
    }
    let home = context.codesense_home.clone();
    let principal = context.principal();
    let runtime = agent.runtime;
    let target = crate::agents::manifest_resolve::ManifestTarget {
        context_root: None,
        org: context.org_slug.clone(),
        team: context.team_name.clone().unwrap_or_default(),
        repo: None,
    };
    let (grant, revision) = crate::commands::await_blocking("capture_local_mcp", move || {
        let value =
            crate::agents::resolve::effective_mcp_config(&home, &principal, runtime, &target)?;
        revision.update(serde_json::to_vec(&value).map_err(|e| e.to_string())?);
        let revision = format!("{:x}", revision.finalize());
        let grant = config::parse(
            &value,
            &scope.server_map_key,
            &scope.bridge_id,
            allow.as_deref(),
            &cwd,
            |key| std::env::var(key).ok(),
        )?;
        require_selected_servers(
            &grant,
            allow.as_deref().unwrap_or_default(),
            &scope.bridge_id,
        )?;
        Ok((grant, revision))
    })
    .await?;
    if let Some(parent) = parent {
        parent
            .validate()
            .await
            .map_err(|_| "original MCP caller retired")?;
    }
    Ok((Arc::new(grant), Some(revision)))
}

fn workspace_on_machine(row: &serde_json::Value) -> bool {
    row.get("location")
        .and_then(|location| location.get("kind"))
        .and_then(|kind| kind.as_str())
        == Some("machine")
}

/// Where a workspace's desktop MCP configuration is read: its worktree on
/// This Mac, or, for a workspace that lives on a machine (no local worktree),
/// the team content its worktree's configuration is projected from.
fn desktop_config_root(
    row: &serde_json::Value,
    team_content: Option<std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    if workspace_on_machine(row) {
        return team_content;
    }
    row.get("cwd")
        .and_then(|v| v.as_str())
        .filter(|cwd| !cwd.is_empty())
        .map(std::path::PathBuf::from)
}

fn require_selected_servers(grant: &Grant, allow: &[String], bridge: &str) -> Result<(), String> {
    for name in allow
        .iter()
        .map(|name| name.trim())
        .filter(|name| *name != bridge && *name != "codesense")
    {
        if !grant.contains_key(name) && !grant.values().any(|server| matches!(&server.endpoint, config::Endpoint::Remote(entry) if entry.logical_name == name)) {
            return Err(format!("selected Agent MCP server {name:?} is missing or disabled"));
        }
    }
    Ok(())
}

pub(crate) type Validate = Arc<
    dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>
        + Send
        + Sync,
>;
pub(crate) async fn serve(
    server: config::Server,
    stream: tokio::io::DuplexStream,
    valid: Validate,
) {
    // Transport errors are intentionally not formatted: endpoint URLs, server diagnostics,
    // and process arguments may contain credentials. EOF truthfully leaves calls uncertain.
    let _ = transport::serve(server, stream, valid).await;
}

#[cfg(test)]
#[test]
fn a_machine_workspace_reads_its_team_content_configuration() {
    let content = std::path::PathBuf::from("/team/content");
    let local = serde_json::json!({"id": "w", "cwd": "/Users/me/wt/SW-1"});
    assert_eq!(
        desktop_config_root(&local, Some(content.clone())),
        Some("/Users/me/wt/SW-1".into())
    );
    let remote = serde_json::json!({"id": "w", "cwd": "", "location": {"kind": "machine", "machineId": "box"}});
    assert_eq!(
        desktop_config_root(&remote, Some(content.clone())),
        Some(content)
    );
    assert_eq!(desktop_config_root(&remote, None), None);
    assert_eq!(
        desktop_config_root(&serde_json::json!({"cwd": ""}), None),
        None
    );
}

#[cfg(test)]
pub(crate) mod http_fixture;
#[cfg(test)]
mod tests;
