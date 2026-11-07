// SPDX-License-Identifier: MIT
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager};
use tokio_util::sync::CancellationToken;

use crate::agents::adapter::{cli_of_provider, desktop};
use crate::domain::Principal;
use crate::install::agent_account_store::{
    self, AccountResolutionSource, AgentAccountAuthState, AgentAccountProfile, AgentProvider,
    IdentityVerification, StoreMutationError,
};
use codesense_pty_core::agent_cli::descriptor;

use super::async_helpers::await_blocking;
use super::paths::codesense_home_dir;

pub const LIFECYCLE_EVENT: &str = "agent-account-lifecycle-changed";
pub const LOGIN_EVENT: &str = "agent-account-login-state-changed";

#[derive(Default)]
pub struct AgentAccountRuntimeState {
    logins: Mutex<HashMap<(u64, String), LoginTask>>,
    pending_provenance: Mutex<
        HashMap<
            String,
            (
                LaunchAccountProvenance,
                Option<super::pty_host::LocalEffectClaim>,
            ),
        >,
    >,
    environment_bindings: Mutex<HashMap<String, EnvironmentBinding>>,
    auth_rechecks: Mutex<AuthRecheckThrottle>,
}

struct EnvironmentBinding {
    launch: LaunchEnvironment,
    minted_at: std::time::Instant,
}

/// What a one-use environment binding carries into a spawn.
#[derive(Default)]
pub(crate) struct LaunchEnvironment {
    /// Set over everything else: the MCP scope's variables and the launch's usage export.
    pub values: Vec<(String, String)>,
    pub scope: Option<super::mcp_scope::ScopeCreation>,
    /// A saved agent's own variables, set under every variable CodeSense sets.
    pub agent_env: Vec<(String, String)>,
    pub requires_spend_holds: bool,
}

struct LoginTask {
    token: CancellationToken,
    nonce: u128,
    generation: u64,
    terminal_claimed: bool,
}

impl LaunchEnvironment {
    /// The binding of a saved agent's launch `profile`: `values` and `scope` over the
    /// agent's own variables.
    pub(crate) fn for_agent(
        profile: &crate::agents::adapter::ResolvedLaunchProfile,
        values: Vec<(String, String)>,
        scope: Option<super::mcp_scope::ScopeCreation>,
        requires_spend_holds: bool,
    ) -> Self {
        Self {
            values,
            scope,
            agent_env: profile.env.clone().into_iter().collect(),
            requires_spend_holds,
        }
    }
}

/// Mint the one-use binding a spawn consumes for `launch`; `None` when it carries nothing.
pub(crate) fn mint_launch_environment<R: tauri::Runtime>(
    app: &AppHandle<R>,
    launch: LaunchEnvironment,
) -> Option<String> {
    if launch.values.is_empty() && launch.scope.is_none() && launch.agent_env.is_empty() {
        return None;
    }
    let token = crate::http::random_hex_secret();
    let state = app.state::<AgentAccountRuntimeState>();
    let mut bindings = state.environment_bindings.lock().unwrap();
    bindings.retain(|_, binding| binding.minted_at.elapsed() < std::time::Duration::from_secs(60));
    bindings.insert(
        token.clone(),
        EnvironmentBinding {
            launch,
            minted_at: std::time::Instant::now(),
        },
    );
    Some(token)
}

pub(crate) fn consume_environment_binding<R: tauri::Runtime>(
    app: &AppHandle<R>,
    token: Option<&str>,
) -> Result<LaunchEnvironment, String> {
    let Some(token) = token else {
        return Ok(LaunchEnvironment::default());
    };
    let state = app.state::<AgentAccountRuntimeState>();
    let mut bindings = state.environment_bindings.lock().unwrap();
    bindings.retain(|_, binding| binding.minted_at.elapsed() < std::time::Duration::from_secs(60));
    bindings
        .remove(token)
        .map(|binding| binding.launch)
        .ok_or_else(|| "invalid, expired, or already-consumed launch environment binding".into())
}

pub fn validate_environment_binding(app: &AppHandle, token: Option<&str>) -> Result<(), String> {
    let Some(token) = token else {
        return Ok(());
    };
    let state = app.state::<AgentAccountRuntimeState>();
    let mut bindings = state.environment_bindings.lock().unwrap();
    bindings.retain(|_, binding| binding.minted_at.elapsed() < std::time::Duration::from_secs(60));
    bindings
        .contains_key(token)
        .then_some(())
        .ok_or_else(|| "invalid, expired, or already-consumed launch environment binding".into())
}

#[derive(Clone)]
pub(crate) struct LaunchBinding {
    github_user_id: u64,
    provenance: LaunchAccountProvenance,
    managed_home: Option<std::path::PathBuf>,
    minted_at: std::time::Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchAccountProvenance {
    pub runtime: codesense_pty_core::agent_cli::AgentCli,
    pub provider: Option<AgentProvider>,
    pub source: AccountResolutionSource,
    pub reason: agent_account_store::AccountResolutionReason,
    pub account_id: Option<String>,
    pub label: Option<String>,
    pub generation: Option<u64>,
    /// The account's managed CLI home, where that CLI keeps its session log. `None` on a launch
    /// that runs as the system default.
    pub home: Option<std::path::PathBuf>,
}

pub struct ConsumedLaunchBinding {
    pub environment: Vec<(String, String)>,
    /// Variables the launch drops because a managed account is injected: credentials its CLI
    /// ranks above that account's login. Empty on a system-default launch, which keeps them.
    pub env_remove: Vec<String>,
    pub provenance: Option<LaunchAccountProvenance>,
}

impl AgentAccountRuntimeState {
    pub fn cancel(&self, github_user_id: u64, account_id: &str) -> bool {
        let key = (github_user_id, account_id.to_owned());
        let mut logins = self.logins.lock().unwrap();
        if logins.get(&key).is_some_and(|task| task.terminal_claimed) {
            return false;
        }
        logins.remove(&key).is_some_and(|task| {
            task.token.cancel();
            true
        })
    }

    /// Stage the launch-time account provenance for a session, keyed by session
    /// id, so the later `bind` can stamp it once. `None` stages nothing.
    #[cfg(test)]
    pub fn stage_provenance(
        &self,
        session_id: &str,
        provenance: Option<LaunchAccountProvenance>,
    ) -> Result<(), String> {
        self.stage_owned_provenance(session_id, provenance, None)
    }

    pub(crate) fn stage_owned_provenance(
        &self,
        session_id: &str,
        provenance: Option<LaunchAccountProvenance>,
        owner: Option<super::pty_host::LocalEffectClaim>,
    ) -> Result<(), String> {
        if let Some(provenance) = provenance {
            let mut pending = self
                .pending_provenance
                .lock()
                .map_err(|_| "account provenance state unavailable")?;
            if pending
                .get(session_id)
                .is_some_and(|(_, prior)| prior != &owner)
            {
                return Err("account provenance belongs to another runtime claim".into());
            }
            pending.insert(session_id.to_owned(), (provenance, owner));
        }
        Ok(())
    }

    /// Read the staged provenance for a session id without consuming it. A
    /// session id with no staged entry (including a key mismatch) reads `None`.
    #[cfg(test)]
    pub fn provenance(&self, session_id: &str) -> Option<LaunchAccountProvenance> {
        self.pending_provenance
            .lock()
            .ok()?
            .get(session_id)
            .map(|(provenance, _)| provenance.clone())
    }

    /// Drop the staged provenance for a session id (after bind or on teardown).
    #[cfg(test)]
    pub fn clear_provenance(&self, session_id: &str) {
        if let Ok(mut pending) = self.pending_provenance.lock() {
            if pending
                .get(session_id)
                .is_some_and(|(_, owner)| owner.is_none())
            {
                pending.remove(session_id);
            }
        }
    }

    pub(crate) fn take_owned_provenance(
        &self,
        owner: &super::pty_host::LocalEffectClaim,
    ) -> Option<LaunchAccountProvenance> {
        let mut pending = self.pending_provenance.lock().ok()?;
        if pending
            .get(&owner.session)
            .is_none_or(|(_, current)| current.as_ref() != Some(owner))
        {
            return None;
        }
        pending
            .remove(&owner.session)
            .map(|(provenance, _)| provenance)
    }

    pub(crate) fn owned_provenance(
        &self,
        owner: &super::pty_host::LocalEffectClaim,
    ) -> Option<LaunchAccountProvenance> {
        self.pending_provenance
            .lock()
            .ok()?
            .get(&owner.session)
            .filter(|(_, current)| current.as_ref() == Some(owner))
            .map(|(provenance, _)| provenance.clone())
    }

    pub(crate) fn transfer_owned(
        &self,
        old: &super::pty_host::LocalEffectClaim,
        new: &super::pty_host::LocalEffectClaim,
    ) {
        let mut pending = self
            .pending_provenance
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some((_, owner)) = pending.get_mut(&old.session) {
            if owner.as_ref() == Some(old) {
                *owner = Some(new.clone());
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAccountView {
    pub id: String,
    pub provider: AgentProvider,
    pub label: String,
    pub auth_state: AgentAccountAuthState,
    pub auth_checked_at: Option<u64>,
    pub auth_error: Option<String>,
    pub identity_display_hint: Option<String>,
    pub identity_verification: IdentityVerification,
    pub generation: u64,
    pub created_at: u64,
    pub updated_at: u64,
    pub detached_at: Option<u64>,
}

impl From<AgentAccountProfile> for AgentAccountView {
    fn from(profile: AgentAccountProfile) -> Self {
        let (identity_display_hint, identity_verification) = profile
            .identity
            .as_ref()
            .map(|identity| (identity.display_hint.clone(), identity.verification))
            .unwrap_or((None, IdentityVerification::Unverified));
        Self {
            id: profile.id,
            provider: profile.provider,
            label: profile.label,
            auth_state: profile.auth_state,
            auth_checked_at: profile.auth_checked_at,
            auth_error: profile.auth_error,
            identity_display_hint,
            identity_verification,
            generation: profile.generation,
            created_at: profile.created_at,
            updated_at: profile.updated_at,
            detached_at: profile.detached_at,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateAgentAccountInput {
    pub provider: AgentProvider,
    pub label: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolveAgentAccountInput {
    /// Internal launch policy already resolved for the launch; never accepted over IPC.
    #[serde(skip)]
    pub trusted_policy: Option<crate::agents::resolve::ResolvedAccountPolicy>,
    #[serde(default)]
    pub override_account_id: Option<String>,
    #[serde(default)]
    pub override_account_generation: Option<u64>,
    /// Explicit one-run selection of the unmanaged provider login. This is a
    /// resolver sentinel, never an account record, and is valid only for an
    /// optional profile.
    #[serde(default)]
    pub use_system_default: bool,
    /// The runtime the launch runs, whose accounts it may use; absent is the preferred
    /// built-in runtime.
    #[serde(default)]
    pub runtime: Option<codesense_pty_core::agent_cli::AgentCli>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAccountResolutionView {
    pub runtime: codesense_pty_core::agent_cli::AgentCli,
    pub source: AccountResolutionSource,
    pub account_id: Option<String>,
    pub provider: Option<AgentProvider>,
    pub label: Option<String>,
    pub generation: Option<u64>,
    pub reason: agent_account_store::AccountResolutionReason,
    pub availability: agent_account_store::AccountResolutionAvailability,
    pub freshness: agent_account_store::AccountResolutionFreshness,
    pub spawn_eligible: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveAgentAccountView {
    pub provider: AgentProvider,
    pub account_id: Option<String>,
    pub account_generation: Option<u64>,
    pub revision: u64,
    pub updated_at: u64,
}

impl From<agent_account_store::ActiveAgentAccount> for ActiveAgentAccountView {
    fn from(value: agent_account_store::ActiveAgentAccount) -> Self {
        Self {
            provider: value.provider,
            account_id: value.account_id,
            account_generation: value.account_generation,
            revision: value.revision,
            updated_at: value.updated_at,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveAgentAccountChangeView {
    pub previous: Option<ActiveAgentAccountView>,
    pub current: Option<ActiveAgentAccountView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveAgentAccountMutationView {
    pub status: &'static str,
    pub previous: Option<ActiveAgentAccountView>,
    pub current: Option<ActiveAgentAccountView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAccountLifecycleChanged {
    pub event_id: String,
    pub account_id: String,
    pub provider: AgentProvider,
    pub generation: u64,
    pub state: String,
}

fn mutation_error(error: StoreMutationError) -> String {
    error.to_string()
}

async fn replay_lifecycle_events(app: &AppHandle, github_user_id: u64) -> Result<(), String> {
    let principal = Principal::from(github_user_id);
    let events = await_blocking("read_agent_account_lifecycle_journal", move || {
        let home = codesense_home_dir().ok_or("cannot resolve home directory")?;
        agent_account_store::pending_lifecycle_events(&home, &principal).map_err(mutation_error)
    })
    .await?;

    for event in events {
        if event.state == "detached" {
            reconcile_detached_active(
                app.clone(),
                github_user_id,
                event.provider,
                event.account_id.as_str(),
                event.generation,
            )
            .await;
        }
        let payload = AgentAccountLifecycleChanged {
            event_id: event.id.clone(),
            account_id: event.account_id.clone(),
            provider: event.provider,
            generation: event.generation,
            state: event.state,
        };
        if app.emit(LIFECYCLE_EVENT, payload).is_err() {
            break;
        }
    }
    Ok(())
}

/// Retire a durable lifecycle event only after a renderer listener has
/// reconciled it. `emit` success is not a delivery acknowledgement.
#[tauri::command]
pub async fn acknowledge_agent_account_lifecycle_event(
    github_user_id: u64,
    account_id: String,
    event_id: String,
) -> Result<bool, String> {
    let principal = Principal::from(github_user_id);
    await_blocking("ack_agent_account_lifecycle_event", move || {
        let home = codesense_home_dir().ok_or("cannot resolve home directory")?;
        agent_account_store::acknowledge_lifecycle_event(&home, &principal, &account_id, &event_id)
            .map_err(mutation_error)
    })
    .await
}

async fn mutate_account<F>(
    app: AppHandle,
    github_user_id: u64,
    label: &'static str,
    operation: F,
) -> Result<AgentAccountView, String>
where
    F: FnOnce(&std::path::Path, &Principal) -> Result<AgentAccountProfile, StoreMutationError>
        + Send
        + 'static,
{
    let principal = Principal::from(github_user_id);
    let profile = await_blocking(label, move || {
        let home = codesense_home_dir().ok_or("cannot resolve home directory")?;
        operation(&home, &principal).map_err(mutation_error)
    })
    .await?;
    replay_lifecycle_events(&app, github_user_id).await?;
    Ok(profile.into())
}

#[tauri::command]
pub async fn list_agent_accounts(
    app: AppHandle,
    github_user_id: u64,
) -> Result<Vec<AgentAccountView>, String> {
    list_accounts(app, github_user_id, false, true).await
}

#[tauri::command]
pub async fn list_detached_agent_accounts(
    app: AppHandle,
    github_user_id: u64,
) -> Result<Vec<AgentAccountView>, String> {
    // The paired attached-list read owns journal replay. Avoid concurrently
    // replaying and emitting the same durable lifecycle event twice.
    list_accounts(app, github_user_id, true, false).await
}

async fn list_accounts(
    app: AppHandle,
    github_user_id: u64,
    detached: bool,
    replay: bool,
) -> Result<Vec<AgentAccountView>, String> {
    if replay {
        replay_lifecycle_events(&app, github_user_id).await?;
    }
    let principal = Principal::from(github_user_id);
    let profiles = await_blocking("list_agent_accounts", move || {
        let home = codesense_home_dir().ok_or("cannot resolve home directory")?;
        agent_account_store::list_profiles(&home, &principal)
            .map_err(mutation_error)
            .map(|profiles| {
                profiles
                    .into_iter()
                    .filter(|profile| profile.detached_at.is_some() == detached)
                    .collect::<Vec<_>>()
            })
    })
    .await?;
    if !detached {
        spawn_auth_rechecks(&app, github_user_id, &profiles);
    }
    Ok(profiles.into_iter().map(AgentAccountView::from).collect())
}

#[tauri::command]
pub async fn get_active_agent_account(
    app: AppHandle,
    provider: AgentProvider,
) -> Result<Option<ActiveAgentAccountView>, String> {
    // Resolve the principal from trusted context rather than trusting a
    // caller-supplied id, matching set_active/resolve_for_launch.
    let context = crate::commands::route_to_context::derive_active_context(&app).await?;
    let principal = context.principal();
    await_blocking("get_active_agent_account", move || {
        let home = codesense_home_dir().ok_or("cannot resolve home directory")?;
        agent_account_store::get_active_account(&home, &principal, provider)
            .map(|value| value.map(ActiveAgentAccountView::from))
            .map_err(mutation_error)
    })
    .await
}

pub async fn set_active_agent_account_inner<R: tauri::Runtime>(
    app: AppHandle<R>,
    github_user_id: u64,
    provider: AgentProvider,
    expected_revision: u64,
    account_id: Option<String>,
    expected_account_generation: Option<u64>,
) -> Result<ActiveAgentAccountChangeView, String> {
    let principal = Principal::from(github_user_id);
    let changed = await_blocking("set_active_agent_account", move || {
        let home = codesense_home_dir().ok_or("cannot resolve home directory")?;
        agent_account_store::compare_and_set_active_account(
            &home,
            &principal,
            provider,
            expected_revision,
            account_id.as_deref(),
            expected_account_generation,
        )
        .map(|change| ActiveAgentAccountChangeView {
            previous: change.previous.map(ActiveAgentAccountView::from),
            current: change.current.map(ActiveAgentAccountView::from),
        })
        .map_err(mutation_error)
    })
    .await;
    let changed = match changed {
        Ok(changed) => changed,
        // A CAS conflict is routine optimistic concurrency (a concurrent
        // selection, or the best-effort detach reconcile losing its race), not a
        // security-relevant rejection — the caller surfaces it as `conflict`. Only
        // audit genuine rejections so the security log is not flooded with them.
        Err(error) if error.starts_with(agent_account_store::CONFLICT_ERROR_PREFIX) => {
            return Err(error);
        }
        Err(error) => {
            log::warn!(
                target: "security_audit",
                "active_agent_account_change_rejected principal={} provider={} outcome=rejected",
                github_user_id,
                provider.as_str(),
            );
            return Err(error);
        }
    };
    log::info!(
        target: "security_audit",
        "active_agent_account_changed principal={} provider={} previous_account={:?} previous_revision={:?} current_account={:?} current_revision={:?}",
        github_user_id,
        provider.as_str(),
        changed.previous.as_ref().and_then(|record| record.account_id.as_deref()),
        changed.previous.as_ref().map(|record| record.revision),
        changed.current.as_ref().and_then(|record| record.account_id.as_deref()),
        changed.current.as_ref().map(|record| record.revision),
    );
    let owner_app = app.clone();
    if let Err(error) = await_blocking("agent_definitions.account", move || {
        crate::live_hub::agent_definitions::account_changed(
            &owner_app,
            &Principal::from(github_user_id),
            provider,
        )
    })
    .await
    {
        log::warn!("agent account projection: {error}");
    }
    let _ = app.emit("active-agent-account-changed", &changed);
    Ok(changed)
}

async fn reconcile_detached_active(
    app: AppHandle,
    github_user_id: u64,
    provider: AgentProvider,
    detached_account_id: &str,
    detached_generation: u64,
) {
    let principal = Principal::from(github_user_id);
    let active = await_blocking("read_active_after_detach", move || {
        let home = codesense_home_dir().ok_or("cannot resolve home directory")?;
        agent_account_store::get_active_account(&home, &principal, provider).map_err(mutation_error)
    })
    .await;
    let Ok(Some(active)) = active else {
        return;
    };
    if active.account_id.as_deref() != Some(detached_account_id)
        || active.account_generation != Some(detached_generation)
    {
        return;
    }

    // Best effort by design: a concurrent user/MCP selection wins its CAS and
    // resolver correctness does not depend on this historical-pointer cleanup.
    let _ =
        set_active_agent_account_inner(app, github_user_id, provider, active.revision, None, None)
            .await;
}

#[tauri::command]
pub async fn set_active_agent_account(
    app: AppHandle,
    window: tauri::WebviewWindow,
    provider: AgentProvider,
    expected_revision: u64,
    account_id: Option<String>,
    expected_account_generation: Option<u64>,
) -> Result<ActiveAgentAccountMutationView, String> {
    if !window.is_visible().unwrap_or(false) {
        return Err("interactive_authorization_required".into());
    }
    let context = crate::commands::route_to_context::derive_active_context(&app).await?;
    super::interactive_gate::authorize_scoped_capability(
        context.user_id,
        super::interactive_gate::MANAGE_AGENT_ACCOUNTS,
    )
    .map_err(|error| error.to_string())?;
    match set_active_agent_account_inner(
        app,
        context.user_id,
        provider,
        expected_revision,
        account_id,
        expected_account_generation,
    )
    .await
    {
        Ok(change) => Ok(ActiveAgentAccountMutationView {
            status: "changed",
            previous: change.previous,
            current: change.current,
        }),
        Err(error) if error.starts_with(agent_account_store::CONFLICT_ERROR_PREFIX) => {
            let principal = Principal::from(context.user_id);
            let current = await_blocking("read_active_after_conflict", move || {
                let home = codesense_home_dir().ok_or("cannot resolve home directory")?;
                agent_account_store::get_active_account(&home, &principal, provider)
                    .map(|value| value.map(ActiveAgentAccountView::from))
                    .map_err(mutation_error)
            })
            .await?;
            Ok(ActiveAgentAccountMutationView {
                status: "conflict",
                previous: None,
                current,
            })
        }
        Err(error) => Err(error),
    }
}

pub(crate) async fn resolve_for_launch<R: tauri::Runtime>(
    app: &AppHandle<R>,
    input: ResolveAgentAccountInput,
) -> Result<(agent_account_store::ResolvedAgentAccount, LaunchBinding), String> {
    if input.use_system_default
        && (input.override_account_id.is_some() || input.override_account_generation.is_some())
    {
        return Err("System Default cannot be combined with a managed account override".into());
    }
    let (home, github_user_id) = crate::commands::route_to_context::resolve_ambient(app).await?;
    let policy = input.trusted_policy.clone().unwrap_or_else(|| {
        crate::agents::resolve::account_policy(
            input
                .runtime
                .unwrap_or(crate::agents::adapter::PREFERRED_RUNTIME),
        )
    });
    let principal = Principal::from(github_user_id);
    await_blocking("resolve_agent_account_launch", move || {
        let provider = policy.provider();
        let mut resolved = if let Some(provider) = provider {
            let profiles =
                agent_account_store::list_profiles(&home, &principal).map_err(mutation_error)?;
            let active = agent_account_store::get_active_account(&home, &principal, provider)
                .map_err(mutation_error)?;
            if input.use_system_default {
                use crate::install::agent_account_store::AccountRequirement;
                let eligible = policy.requirement == AccountRequirement::Optional;
                agent_account_store::ResolvedAgentAccount {
                    source: AccountResolutionSource::SystemDefault,
                    account_id: None,
                    provider: Some(provider),
                    label: None,
                    generation: None,
                    reason: if eligible {
                        agent_account_store::AccountResolutionReason::OptionalSystemDefault
                    } else {
                        agent_account_store::AccountResolutionReason::RequiredMissing
                    },
                    availability: agent_account_store::AccountResolutionAvailability::NotApplicable,
                    freshness: agent_account_store::AccountResolutionFreshness::NotApplicable,
                    spawn_eligible: eligible,
                }
            } else {
                agent_account_store::resolve_agent_account(
                    policy.requirement,
                    provider,
                    &profiles,
                    active.as_ref(),
                    input.override_account_id.as_deref(),
                    input.override_account_generation,
                )
            }
        } else {
            if input.use_system_default
                || input.override_account_id.is_some()
                || input.override_account_generation.is_some()
            {
                return Err(
                    "this runtime uses its own login and has no managed account override".into(),
                );
            }
            agent_account_store::resolve_without_managed_account()
        };
        if let Some(account_id) = resolved.account_id.as_deref() {
            // Managed eligibility is independent from advisory quota telemetry.
            // Missing/malformed telemetry is explicitly unknown, never "available".
            resolved.availability = agent_account_store::AccountResolutionAvailability::Unknown;
            resolved.freshness = agent_account_store::AccountResolutionFreshness::Unknown;
            if let Some(usage) =
                super::agent_usage::read_persisted_snapshot(&home, &principal, account_id)
            {
                resolved.availability = match usage.availability {
                    super::agent_usage::AgentAvailability::Unknown => {
                        agent_account_store::AccountResolutionAvailability::Unknown
                    }
                    super::agent_usage::AgentAvailability::Available => {
                        agent_account_store::AccountResolutionAvailability::Available
                    }
                    super::agent_usage::AgentAvailability::Exhausted => {
                        agent_account_store::AccountResolutionAvailability::Exhausted
                    }
                    super::agent_usage::AgentAvailability::AuthRequired => {
                        agent_account_store::AccountResolutionAvailability::AuthRequired
                    }
                    super::agent_usage::AgentAvailability::Error => {
                        agent_account_store::AccountResolutionAvailability::Error
                    }
                };
                resolved.freshness = match usage.usage_freshness {
                    super::agent_usage::UsageFreshness::AwaitingFirstUse => {
                        agent_account_store::AccountResolutionFreshness::AwaitingFirstUse
                    }
                    super::agent_usage::UsageFreshness::Live => {
                        agent_account_store::AccountResolutionFreshness::Live
                    }
                    super::agent_usage::UsageFreshness::Stale => {
                        agent_account_store::AccountResolutionFreshness::Stale
                    }
                    super::agent_usage::UsageFreshness::Unsupported => {
                        agent_account_store::AccountResolutionFreshness::Unsupported
                    }
                };
            }
        }
        let managed_home = resolved
            .account_id
            .as_ref()
            .map(|account_id| agent_account_store::managed_home(&home, &principal, account_id))
            .transpose()
            .map_err(mutation_error)?;
        let binding = LaunchBinding {
            github_user_id,
            provenance: LaunchAccountProvenance {
                runtime: policy.runtime,
                provider: resolved.provider,
                source: resolved.source,
                reason: resolved.reason,
                account_id: resolved.account_id.clone(),
                label: resolved.label.clone(),
                generation: resolved.generation,
                home: managed_home.clone(),
            },
            managed_home,
            minted_at: std::time::Instant::now(),
        };
        Ok((resolved, binding))
    })
    .await
}

fn resolution_view(
    resolved: agent_account_store::ResolvedAgentAccount,
    runtime: codesense_pty_core::agent_cli::AgentCli,
) -> AgentAccountResolutionView {
    AgentAccountResolutionView {
        runtime,
        source: resolved.source,
        account_id: resolved.account_id,
        provider: resolved.provider,
        label: resolved.label,
        generation: resolved.generation,
        reason: resolved.reason,
        availability: resolved.availability,
        freshness: resolved.freshness,
        spawn_eligible: resolved.spawn_eligible,
    }
}

/// Pure/redacted preview: no capability is minted and no path/env value can
/// cross IPC.
#[tauri::command]
pub async fn preview_agent_account_launch(
    app: AppHandle,
    input: ResolveAgentAccountInput,
) -> Result<AgentAccountResolutionView, String> {
    let (resolved, binding) = resolve_for_launch(&app, input).await?;
    Ok(resolution_view(resolved, binding.provenance.runtime))
}

#[tauri::command]
pub async fn create_agent_account(
    app: AppHandle,
    github_user_id: u64,
    input: CreateAgentAccountInput,
) -> Result<AgentAccountView, String> {
    mutate_account(
        app,
        github_user_id,
        "create_agent_account",
        move |home, principal| {
            agent_account_store::create_profile(home, principal, input.provider, &input.label)
        },
    )
    .await
}

#[tauri::command]
pub async fn rename_agent_account(
    app: AppHandle,
    github_user_id: u64,
    account_id: String,
    expected_generation: u64,
    label: String,
) -> Result<AgentAccountView, String> {
    mutate_account(
        app,
        github_user_id,
        "rename_agent_account",
        move |home, principal| {
            agent_account_store::rename_profile(
                home,
                principal,
                &account_id,
                expected_generation,
                &label,
            )
        },
    )
    .await
}

#[tauri::command]
pub async fn detach_agent_account(
    app: AppHandle,
    runtime: tauri::State<'_, AgentAccountRuntimeState>,
    github_user_id: u64,
    account_id: String,
    expected_generation: u64,
) -> Result<AgentAccountView, String> {
    let detached = mutate_account(
        app.clone(),
        github_user_id,
        "detach_agent_account",
        move |home, principal| {
            agent_account_store::detach_profile(home, principal, &account_id, expected_generation)
        },
    )
    .await?;
    // Side effects follow the generation-checked lifecycle commit. A stale
    // request must not disrupt valid work belonging to the current generation.
    runtime.cancel(github_user_id, &detached.id);
    super::agent_usage::cancel_account_work(&app, &detached.id);
    if let Some(status_line) = desktop(cli_of_provider(detached.provider)).status_line {
        let principal = Principal::from(github_user_id);
        if let Some(root) = codesense_home_dir() {
            let _ = (status_line.restore)(&root, &principal, &detached.id);
        }
    }
    Ok(detached)
}

/// Consume exactly once at the process-spawn boundary. The caller receives only
/// backend-owned environment composition; renderer inputs never contain a path
/// or environment key/value.
pub(super) fn consume_resolved_launch_binding(
    binding: Option<LaunchBinding>,
) -> Result<ConsumedLaunchBinding, String> {
    let Some(binding) = binding else {
        return Ok(ConsumedLaunchBinding {
            environment: Vec::new(),
            env_remove: Vec::new(),
            provenance: None,
        });
    };
    if binding.minted_at.elapsed() >= std::time::Duration::from_secs(60) {
        return Err("expired agent account launch resolution".into());
    }
    let root = codesense_home_dir().ok_or("cannot resolve home directory")?;
    // Re-read the persisted record at the last possible moment. A detach or
    // auth/generation mutation after mint therefore refuses before child spawn.
    let (environment, env_remove) =
        if let (Some(account_id), Some(generation), Some(provider), Some(home)) = (
            binding.provenance.account_id.as_ref(),
            binding.provenance.generation,
            binding.provenance.provider,
            binding.managed_home.as_ref(),
        ) {
            let current =
                agent_account_store::list_profiles(&root, &Principal::from(binding.github_user_id))
                    .map_err(mutation_error)?
                    .into_iter()
                    .find(|profile| profile.id == *account_id)
                    .ok_or("agent account no longer exists")?;
            if !current.is_attached()
                || current.auth_state != AgentAccountAuthState::Ready
                || current.generation != generation
                || current.provider != provider
                || !home.is_dir()
            {
                return Err("account home is no longer available".into());
            }
            let cli = descriptor(cli_of_provider(provider));
            (
                vec![(cli.home_env.clone(), home.to_string_lossy().into_owned())],
                cli.account_env_scrub.clone(),
            )
        } else {
            (Vec::new(), Vec::new())
        };
    Ok(ConsumedLaunchBinding {
        environment,
        env_remove,
        provenance: Some(binding.provenance),
    })
}

pub(super) fn resolved_launch_binding_home(
    binding: Option<&LaunchBinding>,
    expected_provider: Option<AgentProvider>,
) -> Result<Option<std::path::PathBuf>, String> {
    let Some(binding) = binding else {
        return Ok(None);
    };
    if binding.provenance.provider != expected_provider {
        return Err("agent account provider does not match the agent's runtime".into());
    }
    Ok(binding.managed_home.clone())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginStateChanged {
    pub account_id: String,
    pub generation: u64,
    pub state: &'static str,
}

#[tauri::command]
pub fn list_agent_account_login_states(
    runtime: tauri::State<'_, AgentAccountRuntimeState>,
    github_user_id: u64,
) -> Vec<LoginStateChanged> {
    runtime
        .logins
        .lock()
        .unwrap()
        .iter()
        .filter(|((user_id, _), _)| *user_id == github_user_id)
        .map(|((_, account_id), task)| LoginStateChanged {
            account_id: account_id.clone(),
            generation: task.generation,
            // A remounted webview cannot know which provider sub-step is in
            // flight, so replay the honest non-terminal state.
            state: "waiting",
        })
        .collect()
}

fn emit_login<R: tauri::Runtime>(
    app: &AppHandle<R>,
    profile: &AgentAccountProfile,
    state: &'static str,
) {
    let _ = app.emit(
        LOGIN_EVENT,
        LoginStateChanged {
            account_id: profile.id.clone(),
            generation: profile.generation,
            state,
        },
    );
}

#[tauri::command]
pub async fn start_agent_account_login(
    app: AppHandle,
    runtime: tauri::State<'_, AgentAccountRuntimeState>,
    github_user_id: u64,
    account_id: String,
    expected_generation: u64,
) -> Result<&'static str, String> {
    let principal = Principal::from(github_user_id);
    let validate_id = account_id.clone();
    let profile = await_blocking("start_agent_account_login.validate", move || {
        let home = codesense_home_dir().ok_or("cannot resolve home directory")?;
        agent_account_store::list_profiles(&home, &principal)
            .map_err(mutation_error)?
            .into_iter()
            .find(|profile| profile.id == validate_id)
            .ok_or_else(|| "not-found: agent account".to_string())
    })
    .await?;
    if !profile.is_attached() || profile.generation != expected_generation {
        return Err("conflict: account changed or disconnected".into());
    }
    let token = CancellationToken::new();
    let login_nonce = rand::random::<u128>();
    {
        let mut logins = runtime.logins.lock().unwrap();
        let login_key = (github_user_id, account_id.clone());
        if logins.contains_key(&login_key) {
            return Ok("coalesced");
        }
        logins.insert(
            login_key,
            LoginTask {
                token: token.clone(),
                nonce: login_nonce,
                generation: profile.generation,
                terminal_claimed: false,
            },
        );
    }
    // Make the credential authority non-launchable before the provider starts
    // mutating it. The generation bump invalidates already-minted capabilities.
    let login_profile = {
        let transition = profile.clone();
        match await_blocking("start_agent_account_login.transition", move || {
            let home = codesense_home_dir().ok_or("cannot resolve home directory")?;
            agent_account_store::set_auth_state(
                &home,
                &Principal::from(github_user_id),
                &transition.id,
                transition.generation,
                AgentAccountAuthState::AuthenticationRequired,
                None,
            )
            .map_err(mutation_error)
        })
        .await
        {
            Ok(profile) => profile,
            Err(error) => {
                runtime
                    .logins
                    .lock()
                    .unwrap()
                    .remove(&(github_user_id, account_id));
                return Err(error);
            }
        }
    };
    if let Some(task) = runtime
        .logins
        .lock()
        .unwrap()
        .get_mut(&(github_user_id, account_id.clone()))
    {
        task.generation = login_profile.generation;
    }
    super::agent_usage::cancel_account_work(&app, &login_profile.id);
    emit_login(&app, &login_profile, "starting");
    let task_app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = run_provider_login(&task_app, github_user_id, &login_profile, &token).await;
        let still_owner = {
            let state = task_app.state::<AgentAccountRuntimeState>();
            let login_key = (github_user_id, account_id.clone());
            let mut logins = state.logins.lock().unwrap();
            logins.get_mut(&login_key).is_some_and(|current| {
                if current.nonce != login_nonce || current.terminal_claimed {
                    return false;
                }
                current.terminal_claimed = true;
                true
            })
        };
        // Cancellation removes ownership immediately so a retry can start.
        // A superseded task must never mutate the newer attempt's generation.
        if !still_owner {
            emit_login(&task_app, &login_profile, "cancelled");
            return;
        }
        let root = match codesense_home_dir() {
            Some(root) => root,
            None => return,
        };
        let principal = Principal::from(github_user_id);
        let update = match result {
            Ok(identity) if !token.is_cancelled() => agent_account_store::commit_login_identity(
                &root,
                &principal,
                &login_profile,
                identity,
            ),
            result => agent_account_store::set_auth_state(
                &root,
                &principal,
                &login_profile.id,
                login_profile.generation,
                if token.is_cancelled() {
                    AgentAccountAuthState::AuthenticationRequired
                } else {
                    AgentAccountAuthState::Error
                },
                result.err().map(|message| redact_provider_error(&message)),
            ),
        };
        if let Ok(mut updated) = update {
            let status_line = desktop(cli_of_provider(updated.provider)).status_line;
            let ready = updated.auth_state == AgentAccountAuthState::Ready;
            if let Some(status_line) = status_line.filter(|_| ready) {
                if let Ok((socket, capability)) = super::agent_usage::register_status_line(
                    &task_app,
                    github_user_id,
                    &updated.id,
                    updated.provider,
                ) {
                    if let Ok(next) =
                        (status_line.install)(&root, &principal, &updated, &socket, &capability)
                    {
                        updated = next;
                    }
                }
            }
            emit_login(
                &task_app,
                &updated,
                if token.is_cancelled() {
                    "cancelled"
                } else if updated.auth_state == AgentAccountAuthState::Ready {
                    "ready"
                } else {
                    "error"
                },
            );
        } else {
            // Every started login has a terminal lifecycle event, including a
            // persistence conflict, so clients never remain stuck in a busy UI.
            emit_login(
                &task_app,
                &login_profile,
                if token.is_cancelled() {
                    "cancelled"
                } else {
                    "error"
                },
            );
        }
        let state = task_app.state::<AgentAccountRuntimeState>();
        let mut logins = state.logins.lock().unwrap();
        let key = (github_user_id, account_id);
        if logins
            .get(&key)
            .is_some_and(|current| current.nonce == login_nonce)
        {
            logins.remove(&key);
        }
    });
    Ok("started")
}

#[cfg(unix)]
struct ClaudeSettingsLock(std::fs::File);

#[cfg(unix)]
impl ClaudeSettingsLock {
    fn acquire(home: &std::path::Path) -> Result<Self, String> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(home.join(".codesense-settings.lock"))
            .map_err(|error| error.to_string())?;
        // SAFETY: the file descriptor remains owned by this guard, and flock
        // neither retains pointers nor accesses Rust-managed memory.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(Self(file))
    }
}

#[cfg(unix)]
impl Drop for ClaudeSettingsLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        // SAFETY: this is the same live descriptor locked by the guard.
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

pub(crate) fn install_claude_status_line(
    root: &std::path::Path,
    principal: &Principal,
    profile: &AgentAccountProfile,
    socket: &std::path::Path,
    capability: &str,
) -> Result<AgentAccountProfile, String> {
    use crate::content_engine::write_tmp_fsync_rename;
    use std::os::unix::fs::PermissionsExt;

    let home =
        agent_account_store::managed_home(root, principal, &profile.id).map_err(mutation_error)?;
    let _settings_lock = ClaudeSettingsLock::acquire(&home)?;
    let settings_path = home.join("settings.json");
    let wrapper_path = home.join("codesense-status-line.sh");
    let backup_path = home.join(".codesense-status-line-backup.json");
    for _ in 0..3 {
        let observed = match std::fs::read(&settings_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => b"{}".to_vec(),
            Err(error) => return Err(error.to_string()),
        };
        if observed.len() > 1024 * 1024 {
            return Err("Claude settings are too large to compose safely".into());
        }
        let mut settings: serde_json::Value =
            serde_json::from_slice(&observed).map_err(|_| "Claude settings are malformed")?;
        let object = settings
            .as_object_mut()
            .ok_or("Claude settings root is not an object")?;
        let original = object
            .get("statusLine")
            .and_then(|value| value.get("command"))
            .and_then(|value| value.as_str())
            .filter(|command| !command.contains("codesense-status-line.sh"))
            .unwrap_or("printf 'Claude'");
        let wrapper = format!(
            "#!/bin/zsh\nset -u\ntmp=$(mktemp -t codesense-status.XXXXXX) || exit 1\ntrap 'rm -f \"$tmp\"' EXIT\ncat >\"$tmp\"\nif [ $(wc -c <\"$tmp\") -le 65536 ]; then\n  ( {{ printf '%s\\n' {}; cat \"$tmp\"; }} | nc -uU {} >/dev/null 2>&1 ) &\n  sender=$!\n  ( sleep 0.05; kill \"$sender\" 2>/dev/null ) &\nfi\n/bin/zsh -lc {} <\"$tmp\"\nexit $?\n",
            codesense_pty_core::shell_quote(capability),
            codesense_pty_core::shell_quote(&socket.to_string_lossy()),
            codesense_pty_core::shell_quote(original),
        );
        write_tmp_fsync_rename(&wrapper_path, wrapper.as_bytes()).map_err(|e| e.to_string())?;
        std::fs::set_permissions(&wrapper_path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
        if !backup_path.exists() {
            write_tmp_fsync_rename(&backup_path, &observed).map_err(|e| e.to_string())?;
            std::fs::set_permissions(&backup_path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| e.to_string())?;
        }
        object.insert(
            "statusLine".into(),
            serde_json::json!({"type":"command","command":wrapper_path.to_string_lossy()}),
        );
        let current = std::fs::read(&settings_path).unwrap_or_else(|_| b"{}".to_vec());
        if current != observed {
            continue;
        }
        let bytes = serde_json::to_vec_pretty(&settings).map_err(|e| e.to_string())?;
        write_tmp_fsync_rename(&settings_path, &bytes).map_err(|e| e.to_string())?;
        std::fs::set_permissions(&settings_path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        return agent_account_store::set_status_line(
            root,
            principal,
            &profile.id,
            profile.generation,
            agent_account_store::ManagedStatusLine {
                wrapper_hash: blake3::hash(wrapper.as_bytes()).to_hex().to_string(),
                prior_settings_hash: Some(blake3::hash(&observed).to_hex().to_string()),
                installed_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            },
        )
        .map_err(mutation_error);
    }
    Err("Claude settings changed repeatedly; external edit was preserved".into())
}

pub(crate) fn restore_owned_claude_status_line(
    root: &std::path::Path,
    principal: &Principal,
    account_id: &str,
) -> Result<(), String> {
    let profile = agent_account_store::list_profiles(root, principal)
        .map_err(mutation_error)?
        .into_iter()
        .find(|profile| profile.id == account_id)
        .ok_or_else(|| "not-found: agent account".to_string())?;
    let Some(ownership) = profile.status_line.as_ref() else {
        return Ok(());
    };
    let home =
        agent_account_store::managed_home(root, principal, &profile.id).map_err(mutation_error)?;
    let _settings_lock = ClaudeSettingsLock::acquire(&home)?;
    let settings_path = home.join("settings.json");
    let wrapper_path = home.join("codesense-status-line.sh");
    let backup_path = home.join(".codesense-status-line-backup.json");
    let wrapper = std::fs::read(&wrapper_path).map_err(|error| error.to_string())?;
    if blake3::hash(&wrapper).to_hex().as_str() != ownership.wrapper_hash {
        return Ok(());
    }
    let current = std::fs::read(&settings_path).map_err(|error| error.to_string())?;
    let settings: serde_json::Value =
        serde_json::from_slice(&current).map_err(|_| "Claude settings are malformed")?;
    let owned = settings
        .pointer("/statusLine/command")
        .and_then(|value| value.as_str())
        .is_some_and(|command| command == wrapper_path.to_string_lossy());
    if !owned {
        return Ok(());
    }
    let backup = std::fs::read(&backup_path).map_err(|error| error.to_string())?;
    if ownership
        .prior_settings_hash
        .as_deref()
        .is_some_and(|expected| blake3::hash(&backup).to_hex().as_str() != expected)
    {
        return Ok(());
    }
    crate::content_engine::write_tmp_fsync_rename(&settings_path, &backup)
        .map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&settings_path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| error.to_string())?;
    }
    let _ = std::fs::remove_file(wrapper_path);
    let _ = std::fs::remove_file(backup_path);
    Ok(())
}

#[tauri::command]
pub fn cancel_agent_account_login(
    runtime: tauri::State<'_, AgentAccountRuntimeState>,
    github_user_id: u64,
    account_id: String,
) -> bool {
    runtime.cancel(github_user_id, &account_id)
}

fn redact_provider_error(message: &str) -> String {
    let first = message.lines().next().unwrap_or("Provider login failed");
    first
        .split_whitespace()
        .take(24)
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(280)
        .collect()
}

async fn run_provider_login(
    app: &AppHandle,
    github_user_id: u64,
    profile: &AgentAccountProfile,
    cancel: &CancellationToken,
) -> Result<agent_account_store::StoredProviderIdentity, String> {
    match profile.provider {
        AgentProvider::Claude => run_claude_login(app, github_user_id, profile, cancel).await,
        AgentProvider::Codex => run_codex_login(app, github_user_id, profile, cancel).await,
    }
}

async fn run_claude_login(
    app: &AppHandle,
    github_user_id: u64,
    profile: &AgentAccountProfile,
    cancel: &CancellationToken,
) -> Result<agent_account_store::StoredProviderIdentity, String> {
    let root = codesense_home_dir().ok_or("cannot resolve home directory")?;
    let home =
        agent_account_store::managed_home(&root, &Principal::from(github_user_id), &profile.id)
            .map_err(mutation_error)?;
    let binary = &descriptor(cli_of_provider(profile.provider)).binary;
    let identity = claude_login_in(app, binary, &home, profile, cancel).await?;
    provider_identity(
        &root,
        github_user_id,
        profile.provider,
        identity.stable.as_deref(),
        identity.display,
    )
}

/// Signs `profile` in to Claude through `binary`, in the account's `home`, then verifies it.
async fn claude_login_in<R: tauri::Runtime>(
    app: &AppHandle<R>,
    binary: &str,
    home: &std::path::Path,
    profile: &AgentAccountProfile,
    cancel: &CancellationToken,
) -> Result<LoginIdentity, String> {
    use tokio::process::Command;
    let cli = descriptor(cli_of_provider(profile.provider));
    let mut login = Command::new(binary);
    scrub_account_env(&mut login, cli);
    let mut child = login
        .args(["auth", "login"])
        .env(&cli.home_env, home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("Claude CLI unavailable: {error}"))?;
    emit_login(app, profile, "waiting");
    tokio::select! {
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err("Login cancelled".into())
        }
        result = tokio::time::timeout(std::time::Duration::from_secs(300), child.wait()) => {
            let status = result.map_err(|_| "Claude login timed out")?.map_err(|e| e.to_string())?;
            if !status.success() { return Err("Claude login did not complete".into()); }
            emit_login(app, profile, "verifying");
            verify_claude_login(binary, home).await?
                .ok_or_else(|| "Claude authentication is required".to_string())
        }
    }
}

async fn run_codex_login(
    app: &AppHandle,
    github_user_id: u64,
    profile: &AgentAccountProfile,
    cancel: &CancellationToken,
) -> Result<agent_account_store::StoredProviderIdentity, String> {
    let root = codesense_home_dir().ok_or("cannot resolve home directory")?;
    let home =
        agent_account_store::managed_home(&root, &Principal::from(github_user_id), &profile.id)
            .map_err(mutation_error)?;
    let binary = &descriptor(cli_of_provider(profile.provider)).binary;
    let identity = codex_login_in(app, binary, &home, profile, cancel).await?;
    provider_identity(
        &root,
        github_user_id,
        profile.provider,
        identity.stable.as_deref(),
        identity.display,
    )
}

/// Signs `profile` in to Codex's App Server through `binary`, in the account's `home`.
async fn codex_login_in<R: tauri::Runtime>(
    app: &AppHandle<R>,
    binary: &str,
    home: &std::path::Path,
    profile: &AgentAccountProfile,
    cancel: &CancellationToken,
) -> Result<LoginIdentity, String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::process::Command;
    let cli = descriptor(cli_of_provider(profile.provider));
    let mut server = Command::new(binary);
    scrub_account_env(&mut server, cli);
    let mut child = server
        .arg("app-server")
        .env(&cli.home_env, home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("Codex CLI unavailable: {error}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or("Codex App Server stdin unavailable")?;
    let stdout = child
        .stdout
        .take()
        .ok_or("Codex App Server stdout unavailable")?;
    stdin
        .write_all(CODEX_HANDSHAKE.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    stdin.write_all(concat!(
        "{\"id\":2,\"method\":\"account/login/start\",\"params\":{\"type\":\"chatgpt\",\"useHostedLoginSuccessPage\":true,\"appBrand\":\"codex\"}}\n"
    ).as_bytes()).await.map_err(|e| e.to_string())?;
    stdin.flush().await.map_err(|e| e.to_string())?;
    let mut lines = BufReader::new(stdout).lines();
    let login = tokio::time::timeout(std::time::Duration::from_secs(300), async {
        let mut browser_opened = false;
        let mut account_requested = false;
        let mut login_id = None::<String>;
        loop {
            let line = tokio::select! {
                _ = cancel.cancelled() => {
                    if let Some(login_id) = login_id.as_deref() {
                        let request = serde_json::json!({
                            "id": 4,
                            "method": "account/login/cancel",
                            "params": { "loginId": login_id }
                        });
                        let _ = stdin.write_all(format!("{request}\n").as_bytes()).await;
                        let _ = stdin.flush().await;
                    }
                    return Err("Login cancelled".to_string());
                }
                line = lines.next_line() => line.map_err(|e| e.to_string())?,
            };
            let Some(line) = line else {
                break;
            };
            if line.len() > 64 * 1024 {
                return Err("Codex App Server message exceeds 64 KiB".into());
            }
            let value: serde_json::Value =
                serde_json::from_str(&line).map_err(|e| e.to_string())?;
            if !browser_opened && value.get("id").and_then(|v| v.as_u64()) == Some(2) {
                let url = value
                    .pointer("/result/authUrl")
                    .and_then(|v| v.as_str())
                    .ok_or("Codex login response has no auth URL")?;
                tauri_plugin_opener::open_url(url, None::<&str>).map_err(|e| e.to_string())?;
                emit_login(app, profile, "browser_opened");
                login_id = value
                    .pointer("/result/loginId")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned);
                browser_opened = true;
                emit_login(app, profile, "waiting");
            }
            if value.get("method").and_then(|v| v.as_str()) == Some("account/login/completed") {
                emit_login(app, profile, "verifying");
                stdin
                    .write_all(CODEX_ACCOUNT_READ.as_bytes())
                    .await
                    .map_err(|e| e.to_string())?;
                stdin.flush().await.map_err(|e| e.to_string())?;
                account_requested = true;
            }
            if account_requested && value.get("id").and_then(|v| v.as_u64()) == Some(3) {
                let result = value
                    .get("result")
                    .cloned()
                    .ok_or("Codex account response has no result")?;
                return Ok(codex_identity(&result));
            }
        }
        Err("Codex App Server closed during login".into())
    });
    let outcome = login
        .await
        .map_err(|_| "Codex login timed out".to_string())?;
    let _ = child.kill().await;
    let _ = child.wait().await;
    outcome
}

/// Drops from `command` what would bill, or report, another identity than the login in the
/// account home it runs under: the CLI's guarded variables and its credential list.
pub(crate) fn scrub_account_env(
    command: &mut tokio::process::Command,
    cli: &codesense_pty_core::agent_cli::Descriptor,
) {
    for name in cli.env_suppress.iter().chain(&cli.account_env_scrub) {
        command.env_remove(name);
    }
}

/// What Codex's App Server needs first on a connection: `initialize`, then `initialized`.
const CODEX_HANDSHAKE: &str = concat!(
    "{\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"codesense\",\"version\":\"1\"}}}\n",
    "{\"method\":\"initialized\",\"params\":{}}\n"
);

/// The App Server's account read, answered under id 3, which never refreshes the token.
const CODEX_ACCOUNT_READ: &str =
    "{\"id\":3,\"method\":\"account/read\",\"params\":{\"refreshToken\":false}}\n";

/// Who a login is signed in as, as far as the CLI says.
struct LoginIdentity {
    stable: Option<String>,
    display: Option<String>,
}

/// Asks `provider`'s CLI, without any interaction, whether the login in `home` is still signed
/// in: `Some` when it is, `None` when it needs signing in, `Err` when the CLI could not say.
async fn verify_login(
    provider: AgentProvider,
    home: &std::path::Path,
) -> Result<Option<LoginIdentity>, String> {
    match provider {
        AgentProvider::Claude => {
            verify_claude_login(&descriptor(cli_of_provider(provider)).binary, home).await
        }
        AgentProvider::Codex => {
            verify_codex_login(&descriptor(cli_of_provider(provider)).binary, home).await
        }
    }
}

async fn verify_claude_login(
    binary: &str,
    home: &std::path::Path,
) -> Result<Option<LoginIdentity>, String> {
    use tokio::process::Command;
    let cli = descriptor(cli_of_provider(AgentProvider::Claude));
    let mut status = Command::new(binary);
    scrub_account_env(&mut status, cli);
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        status
            .args(["auth", "status", "--json"])
            .env(&cli.home_env, home)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| "Claude authentication check timed out")?
    .map_err(|e| format!("Claude CLI unavailable: {e}"))?;
    if output.stdout.len() > 64 * 1024 {
        return Err("Claude authentication response exceeds 64 KiB".into());
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| "Claude authentication response was malformed")?;
    if value.get("loggedIn").and_then(|v| v.as_bool()) != Some(true) {
        return Ok(None);
    }
    Ok(Some(LoginIdentity {
        stable: value
            .get("accountId")
            .and_then(|v| v.as_str())
            .or_else(|| value.get("email").and_then(|v| v.as_str()))
            .map(str::to_string),
        display: value
            .get("email")
            .and_then(|v| v.as_str())
            .map(redact_identity_hint),
    }))
}

/// The identity in a Codex `account/read` result.
fn codex_identity(result: &serde_json::Value) -> LoginIdentity {
    LoginIdentity {
        stable: result
            .pointer("/account/id")
            .and_then(|v| v.as_str())
            .or_else(|| result.pointer("/account/email").and_then(|v| v.as_str()))
            .map(str::to_string),
        display: result
            .pointer("/account/email")
            .and_then(|v| v.as_str())
            .map(redact_identity_hint),
    }
}

async fn verify_codex_login(
    binary: &str,
    home: &std::path::Path,
) -> Result<Option<LoginIdentity>, String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::process::Command;
    let cli = descriptor(cli_of_provider(AgentProvider::Codex));
    let mut server = Command::new(binary);
    scrub_account_env(&mut server, cli);
    let mut child = server
        .arg("app-server")
        .env(&cli.home_env, home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("Codex CLI unavailable: {error}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or("Codex App Server stdin unavailable")?;
    let stdout = child
        .stdout
        .take()
        .ok_or("Codex App Server stdout unavailable")?;
    let reply = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        stdin
            .write_all(CODEX_HANDSHAKE.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        stdin
            .write_all(CODEX_ACCOUNT_READ.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        stdin.flush().await.map_err(|e| e.to_string())?;
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
            if line.len() > 64 * 1024 {
                return Err("Codex App Server message exceeds 64 KiB".to_string());
            }
            let value: serde_json::Value =
                serde_json::from_str(&line).map_err(|e| e.to_string())?;
            if value.get("id").and_then(|v| v.as_u64()) == Some(3) {
                return value
                    .get("result")
                    .cloned()
                    .ok_or_else(|| "Codex account response has no result".to_string());
            }
        }
        Err("Codex App Server closed before replying".to_string())
    })
    .await;
    let _ = child.kill().await;
    let _ = child.wait().await;
    let result = reply.map_err(|_| "Codex authentication check timed out".to_string())??;
    Ok(result
        .get("account")
        .is_some_and(|account| !account.is_null())
        .then(|| codex_identity(&result)))
}

/// How often one account's login is re-checked.
const AUTH_RECHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// When each account's login was last re-checked, so listing accounts often does not spawn
/// a CLI each time.
#[derive(Default)]
struct AuthRecheckThrottle(HashMap<(u64, String), std::time::Instant>);

impl AuthRecheckThrottle {
    /// Whether `key` is due a re-check at `now`; when it is, it is counted as checked.
    fn claim(&mut self, key: (u64, String), now: std::time::Instant) -> bool {
        if self
            .0
            .get(&key)
            .is_some_and(|last| now.saturating_duration_since(*last) < AUTH_RECHECK_INTERVAL)
        {
            return false;
        }
        self.0.insert(key, now);
        true
    }
}

/// The auth state a login check means for an account in `current`, and its error. `None`
/// leaves the account alone: a CLI that could not answer says nothing about a Ready login.
fn auth_check_outcome(
    current: AgentAccountAuthState,
    check: Result<Option<LoginIdentity>, String>,
) -> Option<(AgentAccountAuthState, Option<String>)> {
    match check {
        Ok(Some(_)) => Some((AgentAccountAuthState::Ready, None)),
        Ok(None) => Some((AgentAccountAuthState::AuthenticationRequired, None)),
        Err(_) if current == AgentAccountAuthState::Ready => None,
        Err(message) => Some((
            AgentAccountAuthState::Error,
            Some(redact_provider_error(&message)),
        )),
    }
}

/// Whether a login check applies to an account: one that has been signed in or has failed.
fn auth_recheckable(profile: &AgentAccountProfile) -> bool {
    profile.is_attached() && profile.auth_state != AgentAccountAuthState::NotConfigured
}

/// Writes what `check` found for `profile`, when it moved the account's state or error.
fn apply_auth_check(
    root: &std::path::Path,
    principal: &Principal,
    profile: &AgentAccountProfile,
    check: Result<Option<LoginIdentity>, String>,
) -> Result<Option<AgentAccountProfile>, StoreMutationError> {
    let Some((state, error)) = auth_check_outcome(profile.auth_state, check) else {
        return Ok(None);
    };
    agent_account_store::record_auth_check(
        root,
        principal,
        &profile.id,
        profile.generation,
        state,
        error,
    )
    .map(Some)
}

/// Re-checks, in the background, the login of each of `profiles` that is due one. Accounts in a
/// login are left to it; a check that races another write loses to it.
pub(crate) fn spawn_auth_rechecks(
    app: &AppHandle,
    github_user_id: u64,
    profiles: &[AgentAccountProfile],
) {
    let state = app.state::<AgentAccountRuntimeState>();
    for profile in profiles.iter().filter(|profile| auth_recheckable(profile)) {
        let key = (github_user_id, profile.id.clone());
        if state.logins.lock().unwrap().contains_key(&key)
            || !state
                .auth_rechecks
                .lock()
                .unwrap()
                .claim(key, std::time::Instant::now())
        {
            continue;
        }
        let task_app = app.clone();
        let profile = profile.clone();
        tauri::async_runtime::spawn(async move {
            let _ = recheck_auth(&task_app, github_user_id, &profile).await;
        });
    }
}

/// Re-checks `account_id`'s login after its usage poll failed, if it is due one.
pub(crate) fn spawn_auth_recheck_after_poll_failure(
    app: &AppHandle,
    github_user_id: u64,
    account_id: &str,
) {
    let app = app.clone();
    let account_id = account_id.to_owned();
    tauri::async_runtime::spawn(async move {
        let principal = Principal::from(github_user_id);
        let profiles = await_blocking("recheck_auth.read", move || {
            let home = codesense_home_dir().ok_or("cannot resolve home directory")?;
            agent_account_store::list_profiles(&home, &principal)
                .map_err(mutation_error)
                .map(|profiles| {
                    profiles
                        .into_iter()
                        .filter(|profile| profile.id == account_id)
                        .collect::<Vec<_>>()
                })
        })
        .await;
        if let Ok(profiles) = profiles {
            spawn_auth_rechecks(&app, github_user_id, &profiles);
        }
    });
}

async fn recheck_auth(
    app: &AppHandle,
    github_user_id: u64,
    profile: &AgentAccountProfile,
) -> Result<(), String> {
    let root = codesense_home_dir().ok_or("cannot resolve home directory")?;
    let principal = Principal::from(github_user_id);
    let home = agent_account_store::managed_home(&root, &principal, &profile.id)
        .map_err(mutation_error)?;
    let check = verify_login(profile.provider, &home).await;
    // A login that began while the CLI answered owns the account now.
    let key = (github_user_id, profile.id.clone());
    if app
        .state::<AgentAccountRuntimeState>()
        .logins
        .lock()
        .unwrap()
        .contains_key(&key)
    {
        return Ok(());
    }
    let before = profile.clone();
    let after = await_blocking("recheck_auth.write", move || {
        apply_auth_check(&root, &principal, &before, check).map_err(mutation_error)
    })
    .await?;
    publish_auth_change(app, profile, after.as_ref());
    Ok(())
}

/// Tells the live views, through the event the login flow uses, that a re-check moved
/// `before` to `after`. A check that changed nothing says nothing.
fn publish_auth_change<R: tauri::Runtime>(
    app: &AppHandle<R>,
    before: &AgentAccountProfile,
    after: Option<&AgentAccountProfile>,
) {
    let Some(after) = after.filter(|after| after.generation != before.generation) else {
        return;
    };
    emit_login(
        app,
        after,
        if after.auth_state == AgentAccountAuthState::Ready {
            "ready"
        } else {
            "error"
        },
    );
}

fn redact_identity_hint(value: &str) -> String {
    let Some((local, domain)) = value.split_once('@') else {
        return value
            .chars()
            .next()
            .map_or_else(|| "Signed in".into(), |c| format!("{c}•••"));
    };
    let first = local.chars().next().unwrap_or('•');
    format!("{first}•••@{domain}")
}

fn provider_identity(
    root: &std::path::Path,
    github_user_id: u64,
    provider: AgentProvider,
    stable_identifier: Option<&str>,
    display_hint: Option<String>,
) -> Result<agent_account_store::StoredProviderIdentity, String> {
    let duplicate_key = stable_identifier
        .map(|value| {
            agent_account_store::duplicate_key(
                root,
                &Principal::from(github_user_id),
                provider,
                value,
            )
            .map_err(mutation_error)
        })
        .transpose()?;
    Ok(agent_account_store::StoredProviderIdentity {
        duplicate_key,
        display_hint,
        verification: if stable_identifier.is_some() {
            IdentityVerification::Verified
        } else {
            IdentityVerification::Unverified
        },
        verified_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    })
}

#[tauri::command]
pub async fn reattach_agent_account(
    app: AppHandle,
    github_user_id: u64,
    account_id: String,
    expected_generation: u64,
) -> Result<AgentAccountView, String> {
    mutate_account(
        app,
        github_user_id,
        "reattach_agent_account",
        move |home, principal| {
            agent_account_store::reattach_profile(home, principal, &account_id, expected_generation)
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn launch_account_resolution_ignores_selected_team() {
        use crate::agents::resolve::tests::{make_active, ready_account};
        use crate::mcp::state::{ActiveTeam, ActiveTeamState};
        use crate::routines::test_support::{alice, launch_app, TempHome, ORG, TEAM};
        use codesense_pty_core::agent_cli::AgentCli;
        use tauri::Manager;

        let temp = TempHome::new();
        let home = temp.codesense_home();
        let app = launch_app(&home);
        let selected = app.state::<ActiveTeamState>();
        for (runtime, provider) in [
            (AgentCli::Claude, AgentProvider::Claude),
            (AgentCli::Codex, AgentProvider::Codex),
        ] {
            let active = ready_account(&home, &alice(), provider);
            make_active(&home, &alice(), &active);
            let alternate = ready_account(&home, &alice(), provider);
            for team in [
                None,
                Some((ORG, TEAM)),
                Some((ORG, "other")),
                Some(("other/org", "elsewhere")),
                None,
            ] {
                match team {
                    Some((org, team)) => selected.update(ActiveTeam {
                        org_slug: org.into(),
                        team_name: team.into(),
                    }),
                    None => selected.clear(),
                }
                for override_account in [None, Some(&alternate)] {
                    let (resolved, binding) = resolve_for_launch(
                        app.handle(),
                        ResolveAgentAccountInput {
                            trusted_policy: None,
                            runtime: Some(runtime),
                            override_account_id: override_account.map(|account| account.id.clone()),
                            override_account_generation: override_account
                                .map(|account| account.generation),
                            use_system_default: false,
                        },
                    )
                    .await
                    .unwrap();
                    let expected = override_account.unwrap_or(&active);
                    assert!(resolved.spawn_eligible);
                    assert_eq!(resolved.account_id.as_deref(), Some(expected.id.as_str()));
                    assert_eq!(binding.github_user_id, 1);
                    assert_eq!(binding.provenance.account_id, resolved.account_id);
                    assert_eq!(
                        binding.managed_home,
                        Some(
                            agent_account_store::managed_home(&home, &alice(), &expected.id)
                                .unwrap()
                        )
                    );
                }
            }
        }
    }

    fn signed_in() -> Option<LoginIdentity> {
        Some(LoginIdentity {
            stable: Some("id".into()),
            display: None,
        })
    }

    fn profile_in(
        temp: &TempDir,
        state: AgentAccountAuthState,
        error: Option<&str>,
    ) -> AgentAccountProfile {
        let principal = Principal::from(7u64);
        let created = agent_account_store::create_profile(
            temp.path(),
            &principal,
            AgentProvider::Claude,
            "Work",
        )
        .unwrap();
        agent_account_store::set_auth_state(
            temp.path(),
            &principal,
            &created.id,
            created.generation,
            state,
            error.map(str::to_owned),
        )
        .unwrap()
    }

    #[test]
    fn an_auth_check_moves_state_only_when_something_changed() {
        let temp = TempDir::new().unwrap();
        let principal = Principal::from(7u64);

        // Still signed in: nothing is written, so a binding minted a moment ago survives.
        let ready = profile_in(&temp, AgentAccountAuthState::Ready, None);
        let unchanged = apply_auth_check(temp.path(), &principal, &ready, Ok(signed_in()))
            .unwrap()
            .unwrap();
        assert_eq!(unchanged.generation, ready.generation);

        // The login went: Ready becomes needing sign-in.
        let expired = apply_auth_check(temp.path(), &principal, &ready, Ok(None))
            .unwrap()
            .unwrap();
        assert_eq!(
            expired.auth_state,
            AgentAccountAuthState::AuthenticationRequired
        );
        assert_eq!(expired.generation, ready.generation + 1);

        // A stale Error that now verifies goes back to Ready.
        let errored = profile_in(&temp, AgentAccountAuthState::Error, Some("old"));
        let healed = apply_auth_check(temp.path(), &principal, &errored, Ok(signed_in()))
            .unwrap()
            .unwrap();
        assert_eq!(healed.auth_state, AgentAccountAuthState::Ready);
        assert_eq!(healed.auth_error, None);
        assert_eq!(healed.generation, errored.generation + 1);
    }

    #[test]
    fn a_changed_auth_check_reaches_the_live_views_and_an_unchanged_one_stays_quiet() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use tauri::Listener;

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let heard = Arc::new(AtomicUsize::new(0));
        let counter = heard.clone();
        app.listen(LOGIN_EVENT, move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        let temp = TempDir::new().unwrap();
        let principal = Principal::from(7u64);
        let ready = profile_in(&temp, AgentAccountAuthState::Ready, None);

        let unchanged = apply_auth_check(temp.path(), &principal, &ready, Ok(signed_in())).unwrap();
        publish_auth_change(app.handle(), &ready, unchanged.as_ref());
        let skipped = apply_auth_check(
            temp.path(),
            &principal,
            &ready,
            Err("timed out".to_string()),
        )
        .unwrap();
        publish_auth_change(app.handle(), &ready, skipped.as_ref());
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(heard.load(Ordering::SeqCst), 0);

        let expired = apply_auth_check(temp.path(), &principal, &ready, Ok(None)).unwrap();
        publish_auth_change(app.handle(), &ready, expired.as_ref());
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(heard.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_cli_that_cannot_answer_leaves_a_ready_login_alone() {
        assert_eq!(
            auth_check_outcome(AgentAccountAuthState::Ready, Err("timed out".into())),
            None
        );
        assert_eq!(
            auth_check_outcome(AgentAccountAuthState::Error, Err("timed out".into())),
            Some((AgentAccountAuthState::Error, Some("timed out".into())))
        );
    }

    #[test]
    fn auth_rechecks_are_throttled_per_account() {
        let mut throttle = AuthRecheckThrottle::default();
        let now = std::time::Instant::now();
        let key = (1, "a".to_string());
        assert!(throttle.claim(key.clone(), now));
        assert!(!throttle.claim(key.clone(), now + AUTH_RECHECK_INTERVAL / 2));
        assert!(throttle.claim((1, "b".into()), now));
        assert!(throttle.claim(key, now + AUTH_RECHECK_INTERVAL));
    }

    #[test]
    fn only_attached_signed_in_or_failed_accounts_are_rechecked() {
        let temp = TempDir::new().unwrap();
        assert!(!auth_recheckable(&profile_in(
            &temp,
            AgentAccountAuthState::NotConfigured,
            None
        )));
        for state in [
            AgentAccountAuthState::Ready,
            AgentAccountAuthState::AuthenticationRequired,
            AgentAccountAuthState::Error,
        ] {
            let mut profile = profile_in(&temp, state, None);
            assert!(auth_recheckable(&profile));
            profile.detached_at = Some(1);
            assert!(!auth_recheckable(&profile));
        }
    }

    #[test]
    fn a_refused_launch_is_spelled_as_its_wire_reason() {
        use agent_account_store::AccountResolutionReason::*;
        for (reason, wire) in [
            (ProviderMismatch, "provider-mismatch"),
            (GenerationMismatch, "generation-mismatch"),
            (AccountMissing, "account-missing"),
            (AuthRequired, "auth-required"),
            (Detached, "detached"),
        ] {
            assert_eq!(
                crate::agents::launch::account_refusal(reason),
                format!("agent account is unavailable: {wire}")
            );
        }
    }

    /// A stand-in CLI that records the credential variable and the home it was started with
    /// into `record`, then runs `body`.
    #[cfg(unix)]
    fn fake_cli(dir: &std::path::Path, record: &std::path::Path, vars: &str, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("fake-cli.sh");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\nprintf '%s' \"{vars}\" > '{}'\n{body}\n",
                record.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin.to_string_lossy().into_owned()
    }

    /// The verify step runs Claude in the account's home without the credentials Claude would
    /// rank above that home's login, so it reports the account's own login.
    #[cfg(unix)]
    #[tokio::test]
    async fn claude_verify_runs_in_the_account_home_without_exported_credentials() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let record = dir.path().join("record");
        for name in ["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_AUTH_TOKEN"] {
            std::env::set_var(name, "exported-secret");
        }
        let claude = fake_cli(
            dir.path(),
            &record,
            "$CLAUDE_CODE_OAUTH_TOKEN|$ANTHROPIC_AUTH_TOKEN|$CLAUDE_CONFIG_DIR",
            "echo '{\"loggedIn\":true,\"email\":\"a@b.c\"}'",
        );
        let found = verify_claude_login(&claude, &home).await.unwrap();
        for name in ["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_AUTH_TOKEN"] {
            std::env::remove_var(name);
        }
        assert!(found.is_some());
        assert_eq!(
            std::fs::read_to_string(&record).unwrap(),
            format!("||{}", home.display())
        );
    }

    /// The same for Codex's App Server.
    #[cfg(unix)]
    #[tokio::test]
    async fn codex_verify_runs_in_the_account_home_without_exported_credentials() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let record = dir.path().join("record");
        std::env::set_var("CODEX_API_KEY", "exported-secret");
        let codex = fake_cli(
            dir.path(),
            &record,
            "$CODEX_API_KEY|$CODEX_HOME",
            "while read line; do case \"$line\" in *account/read*) echo '{\"id\":3,\"result\":{\"account\":{\"id\":\"x\",\"email\":\"a@b.c\"}}}'; exit 0;; esac; done",
        );
        let found = verify_codex_login(&codex, &home).await.unwrap();
        std::env::remove_var("CODEX_API_KEY");
        assert!(found.is_some());
        assert_eq!(
            std::fs::read_to_string(&record).unwrap(),
            format!("|{}", home.display())
        );
    }

    /// The login spawn and its verify step each run Claude in the account's home without the
    /// credentials Claude would rank above that home's login.
    #[cfg(unix)]
    #[tokio::test]
    async fn claude_login_and_its_verify_run_without_exported_credentials() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let record = dir.path().join("record");
        let bin = dir.path().join("fake-claude.sh");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\nprintf '%s:%s|%s|%s\\n' \"$2\" \"$CLAUDE_CODE_OAUTH_TOKEN\" \"$ANTHROPIC_AUTH_TOKEN\" \"$CLAUDE_CONFIG_DIR\" >> '{}'\ncase \"$2\" in status) echo '{{\"loggedIn\":true,\"email\":\"a@b.c\"}}';; esac\n",
                record.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        for name in ["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_AUTH_TOKEN"] {
            std::env::set_var(name, "exported-secret");
        }
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let profile = profile_in(&dir, AgentAccountAuthState::AuthenticationRequired, None);
        let signed_in = claude_login_in(
            app.handle(),
            &bin.to_string_lossy(),
            &home,
            &profile,
            &CancellationToken::new(),
        )
        .await;
        for name in ["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_AUTH_TOKEN"] {
            std::env::remove_var(name);
        }
        assert!(signed_in.is_ok());
        let home = home.display();
        assert_eq!(
            std::fs::read_to_string(&record).unwrap(),
            format!("login:||{home}\nstatus:||{home}\n")
        );
    }

    /// The same for the Codex login's App Server spawn.
    #[cfg(unix)]
    #[tokio::test]
    async fn codex_login_runs_without_exported_credentials() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let record = dir.path().join("record");
        // Answers the login start without a URL, so the flow ends at once after the spawn.
        let codex = fake_cli(
            dir.path(),
            &record,
            "$CODEX_API_KEY|$CODEX_HOME",
            "while read line; do case \"$line\" in *login/start*) echo '{\"id\":2,\"result\":{}}'; exit 0;; esac; done",
        );
        std::env::set_var("CODEX_API_KEY", "exported-secret");
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let profile = AgentAccountProfile {
            provider: AgentProvider::Codex,
            ..profile_in(&dir, AgentAccountAuthState::AuthenticationRequired, None)
        };
        let outcome = codex_login_in(
            app.handle(),
            &codex,
            &home,
            &profile,
            &CancellationToken::new(),
        )
        .await;
        std::env::remove_var("CODEX_API_KEY");
        assert!(outcome.is_err(), "the fake sends no sign-in URL");
        assert_eq!(
            std::fs::read_to_string(&record).unwrap(),
            format!("|{}", home.display())
        );
    }

    #[tokio::test]
    async fn a_managed_account_launch_scrubs_exported_credentials_and_a_default_keeps_them() {
        use crate::agents::resolve::tests::{make_active, ready_account};
        use crate::routines::test_support::{alice, launch_app, TempHome};
        use codesense_pty_core::agent_cli::AgentCli;

        let temp = TempHome::new();
        let home = temp.codesense_home();
        let app = launch_app(&home);
        let input = |runtime| ResolveAgentAccountInput {
            trusted_policy: None,
            runtime: Some(runtime),
            override_account_id: None,
            override_account_generation: None,
            use_system_default: false,
        };

        // No Active account: the system default keeps the person's own token.
        let (resolved, binding) = resolve_for_launch(app.handle(), input(AgentCli::Claude))
            .await
            .unwrap();
        assert_eq!(resolved.source, AccountResolutionSource::SystemDefault);
        let consumed = consume_resolved_launch_binding(Some(binding)).unwrap();
        assert!(consumed.env_remove.is_empty());
        assert!(consumed.environment.is_empty());

        // An Active account: the credentials its CLI ranks above the login are dropped.
        let active = ready_account(&home, &alice(), AgentProvider::Claude);
        make_active(&home, &alice(), &active);
        let (_, binding) = resolve_for_launch(app.handle(), input(AgentCli::Claude))
            .await
            .unwrap();
        let consumed = consume_resolved_launch_binding(Some(binding)).unwrap();
        for name in ["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_AUTH_TOKEN"] {
            assert!(consumed.env_remove.iter().any(|n| n == name), "{name}");
        }
        assert_eq!(consumed.environment.len(), 1);
    }

    #[test]
    fn redacted_view_never_projects_duplicate_digest() {
        let profile = AgentAccountProfile {
            schema_version: 1,
            id: "opaque".into(),
            provider: AgentProvider::Codex,
            label: "Work".into(),
            auth_state: AgentAccountAuthState::Ready,
            auth_checked_at: Some(10),
            auth_error: None,
            identity: Some(agent_account_store::StoredProviderIdentity {
                duplicate_key: Some("secret-digest".into()),
                display_hint: Some("u•••@example.test".into()),
                verification: IdentityVerification::Verified,
                verified_at: 10,
            }),
            status_line: None,
            generation: 2,
            created_at: 1,
            updated_at: 10,
            detached_at: None,
            pending_lifecycle_events: Vec::new(),
        };
        let json = serde_json::to_value(AgentAccountView::from(profile)).unwrap();
        assert_eq!(json["identityDisplayHint"], "u•••@example.test");
        assert!(!json.to_string().contains("secret-digest"));
    }

    #[test]
    fn login_cancellation_is_principal_scoped() {
        let runtime = AgentAccountRuntimeState::default();
        let token = CancellationToken::new();
        runtime.logins.lock().unwrap().insert(
            (7, "same-account-id".into()),
            LoginTask {
                token: token.clone(),
                nonce: 1,
                generation: 1,
                terminal_claimed: false,
            },
        );
        assert!(!runtime.cancel(8, "same-account-id"));
        assert!(!token.is_cancelled());
        assert!(runtime.cancel(7, "same-account-id"));
        assert!(token.is_cancelled());
    }

    #[test]
    fn session_provenance_round_trips_by_session_id_and_clears() {
        let runtime = AgentAccountRuntimeState::default();
        let provenance = LaunchAccountProvenance {
            runtime: codesense_pty_core::agent_cli::AgentCli::Claude,
            provider: Some(AgentProvider::Claude),
            source: AccountResolutionSource::Active,
            reason: agent_account_store::AccountResolutionReason::ActiveSelection,
            account_id: Some("acct-a".into()),
            label: Some("Primary".into()),
            generation: Some(3),
            home: None,
        };
        runtime
            .stage_provenance("session-a", Some(provenance.clone()))
            .unwrap();
        // Read back by the SAME session id the spawn staged under.
        assert_eq!(runtime.provenance("session-a"), Some(provenance));
        // A different session id (key mismatch) never sees another session's stamp.
        assert_eq!(runtime.provenance("session-b"), None);
        // Staging `None` is a no-op, never clobbering an existing entry.
        runtime.stage_provenance("session-a", None).unwrap();
        assert!(runtime.provenance("session-a").is_some());
        // Clear drops it so a reused session id cannot inherit a stale stamp.
        runtime.clear_provenance("session-a");
        assert_eq!(runtime.provenance("session-a"), None);
    }

    #[test]
    fn store_conflict_display_keeps_the_matched_prefix() {
        // The set-active command / MCP handler classify a CAS conflict by this
        // prefix; a reword here would silently drop the conflict-recovery path.
        let conflict = agent_account_store::StoreMutationError::Conflict {
            current_generation: 7,
        };
        assert!(conflict
            .to_string()
            .starts_with(agent_account_store::CONFLICT_ERROR_PREFIX));
    }

    #[test]
    fn terminal_claim_prevents_cancel_and_retry_supersession() {
        let runtime = AgentAccountRuntimeState::default();
        let token = CancellationToken::new();
        runtime.logins.lock().unwrap().insert(
            (7, "account".into()),
            LoginTask {
                token: token.clone(),
                nonce: 9,
                generation: 2,
                terminal_claimed: true,
            },
        );
        assert!(!runtime.cancel(7, "account"));
        assert!(!token.is_cancelled());
        assert!(runtime
            .logins
            .lock()
            .unwrap()
            .contains_key(&(7, "account".into())));
    }

    #[test]
    fn claude_status_line_composes_existing_command_and_records_ownership() {
        let temp = TempDir::new().unwrap();
        let principal = Principal::from(42u64);
        let created = agent_account_store::create_profile(
            temp.path(),
            &principal,
            AgentProvider::Claude,
            "Primary",
        )
        .unwrap();
        let ready = agent_account_store::commit_identity(
            temp.path(),
            &principal,
            &created.id,
            created.generation,
            agent_account_store::StoredProviderIdentity {
                duplicate_key: None,
                display_hint: None,
                verification: IdentityVerification::Unverified,
                verified_at: 1,
            },
        )
        .unwrap();
        let home = agent_account_store::managed_home(temp.path(), &principal, &ready.id).unwrap();
        std::fs::write(
            home.join("settings.json"),
            br#"{"statusLine":{"type":"command","command":"printf existing"},"theme":"dark"}"#,
        )
        .unwrap();

        let updated = install_claude_status_line(
            temp.path(),
            &principal,
            &ready,
            std::path::Path::new("/tmp/codesense-test.sock"),
            "route-capability",
        )
        .unwrap();

        let wrapper = std::fs::read_to_string(home.join("codesense-status-line.sh")).unwrap();
        assert!(wrapper.contains("printf existing"));
        assert!(wrapper.contains("route-capability"));
        assert!(wrapper.contains("cat >\"$tmp\""));
        assert!(wrapper.contains("-le 65536"));
        assert!(wrapper.contains("sleep 0.05"));
        let settings: serde_json::Value =
            serde_json::from_slice(&std::fs::read(home.join("settings.json")).unwrap()).unwrap();
        assert_eq!(settings["theme"], "dark");
        assert!(settings["statusLine"]["command"]
            .as_str()
            .unwrap()
            .ends_with("codesense-status-line.sh"));
        assert!(updated.status_line.is_some());

        restore_owned_claude_status_line(temp.path(), &principal, &updated.id).unwrap();
        let restored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(home.join("settings.json")).unwrap()).unwrap();
        assert_eq!(restored["statusLine"]["command"], "printf existing");
        assert!(!home.join("codesense-status-line.sh").exists());
    }
}
