// SPDX-License-Identifier: MIT
//! Queue registration and read-only startup facts for a newly bound session.
use super::agent_inbox::{AppInboxHost, InboxHost};
use super::dashboard_launch::{DashboardLaunchState, LaunchTeam};
use crate::weft::{role_inbox, WeftStore};
use tauri::{AppHandle, Manager};

pub(crate) fn environment(team: &LaunchTeam, role: Option<&str>) -> Vec<(String, String)> {
    vec![
        ("CODESENSE_ORG".into(), team.org.clone()),
        ("CODESENSE_TEAM".into(), team.team.clone()),
        ("CODESENSE_ROLE".into(), role.unwrap_or_default().into()),
    ]
}

pub(crate) fn join(
    store: &WeftStore,
    team: &LaunchTeam,
    role: &str,
    session: &str,
    remote: bool,
    now: i64,
) -> Result<String, String> {
    let options = role_inbox::RegisterOptions {
        session_id: Some(session.into()),
        exclusive: false,
        ttl_secs: if remote {
            role_inbox::REMOTE_REGISTRATION_TTL_SECS
        } else {
            role_inbox::DEFAULT_REGISTRATION_TTL_SECS
        },
    };
    let registration = role_inbox::register_role_as(store, role, session, &options, now)
        .map_err(|e| e.to_string())?;
    // This inspection leaves every delivery pending and available for an explicit claim.
    let queues = crate::weft::role_state::role_state_read(store, Some(role), now, 1)
        .map_err(|e| e.to_string())?;
    let pending = queues
        .first()
        .map(|q| i64::from(q.counts.pending))
        .unwrap_or(registration.pending_count);
    Ok(format!("You hold role {role} in team {}/{}; {pending} messages are pending. Role inbox inspection is available through role_state; claim_role_messages is a separate action. Registration instanceId={session}, generation={}.", team.org, team.team, registration.generation))
}

/// Register and inspect before handing startup to the provider. The provider carries
/// this prompt through its native startup arguments, rather than typing into a cold TUI.
pub(crate) async fn start<T, F>(
    host: &dyn InboxHost,
    team: Option<&LaunchTeam>,
    role: Option<&str>,
    session: &str,
    remote: bool,
    prompt: Option<&str>,
    spawn: impl FnOnce(Option<String>) -> F,
) -> Result<T, String>
where
    F: std::future::Future<Output = Result<T, String>>,
{
    let mut store_path = None;
    let facts = match role {
        Some(role) => {
            let team = team.ok_or("the launch has no team for this queue")?;
            let path = host.team_store(team).await?;
            store_path = Some(path.clone());
            host.note_role_store(path.clone());
            let (team, role, session) = (team.clone(), role.to_string(), session.to_string());
            let facts = super::await_blocking("launch_role_startup", move || {
                // RI-2-ALLOW: inside await_blocking, off the async runtime.
                let store = WeftStore::open(path).map_err(|e| e.to_string())?;
                join(
                    &store,
                    &team,
                    &role,
                    &session,
                    remote,
                    chrono::Utc::now().timestamp(),
                )
            })
            .await?;
            host.roles_changed();
            Some(facts)
        }
        None => None,
    };
    let prompt = match (prompt, facts) {
        (Some(prompt), Some(facts)) => Some(format!("{prompt}\n\n{facts}")),
        (Some(prompt), None) => Some(prompt.to_string()),
        (None, facts) => facts,
    };
    let result = spawn(prompt).await;
    if result.is_err() {
        if let Some(path) = store_path {
            let session = session.to_string();
            let _ = super::await_blocking("failed_launch_role_release", move || {
                // RI-2-ALLOW: inside await_blocking, off the async runtime.
                let store = WeftStore::open(path).map_err(|e| e.to_string())?;
                role_inbox::renew_session_registrations(
                    &store,
                    chrono::Utc::now().timestamp(),
                    |id| {
                        if id == session {
                            role_inbox::SessionLiveness::Ended
                        } else {
                            role_inbox::SessionLiveness::Unknown
                        }
                    },
                )
                .map_err(|e| e.to_string())
            })
            .await;
            host.roles_changed();
        }
    }
    result
}

/// Start an interactive CLI using its native initial-prompt operand.
pub(crate) async fn start_argv<T, F>(
    host: &dyn InboxHost,
    team: Option<&LaunchTeam>,
    role: Option<&str>,
    session: &str,
    runtime: codesense_pty_core::agent_cli::AgentCli,
    args: Vec<String>,
    prompt: Option<&str>,
    spawn: impl FnOnce(super::mcp_workspace::InitialPromptPlan) -> F,
) -> Result<T, String>
where
    F: std::future::Future<Output = Result<T, String>>,
{
    start(host, team, role, session, false, prompt, |prompt| async move {
        if let Some(reason) = crate::agents::adapter::initial_prompt_refusal(runtime, prompt.as_deref()) {
            return Err(reason);
        }
        let plan = super::mcp_workspace::plan_initial_prompt(runtime, args, prompt.as_deref());
        if role.is_some() && !plan.in_argv {
            return Err("This role-enabled startup cannot carry its initial task reliably in the CLI arguments".into());
        }
        spawn(plan).await
    }).await
}

pub(crate) async fn register(app: &AppHandle, handle: &str, session: &str) -> Option<String> {
    let state = app.state::<DashboardLaunchState>();
    let entry = DashboardLaunchState::snapshot(&state)
        .into_iter()
        .find(|e| e.handle == handle && e.session_id.as_deref() == Some(session))?;
    let role = entry.meta.queue_role?;
    let team = state.launch_team(handle);
    let result = start(
        &AppInboxHost { app: app.clone() },
        team.as_ref(),
        Some(&role),
        session,
        entry.machine_id.is_some(),
        None,
        |facts| async { Ok(facts) },
    )
    .await;
    match result {
        Ok(facts) => facts,
        Err(error) => {
            state.set_role_join_error(handle, error.clone());
            Some(format!("Not joined: {role}. {error}"))
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
