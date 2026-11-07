// SPDX-License-Identifier: MIT
//! Turn an agent into exactly what a launch would run and start with: the
//! runtime's launch profile, the account this user would launch it with, the
//! startup manifest (with the generated permission entries) and the names and
//! warnings derived from it.
//!
//! Every surface that starts or shows an agent (launches, routines, the Agents
//! tab, MCP callers, the lifecycle bootstrap) goes through [`resolve_agent`]
//! (or [`preview_agent`] for an unsaved draft), so they all agree. Both are
//! pure over the filesystem: they read, take no `AppHandle` and write nothing.
//!
//! Only "this agent cannot be identified" is an error ([`ResolveError`]).
//! Everything else (a missing startup path, an unknown MCP server, an account
//! that cannot be used, permissions that shut CodeSense out) is data in the
//! result, so a caller can show it. Nothing here refuses a launch.
//!
//! The runtime is one of the built-in CLI adapters (`claude`, `codex`): nothing a
//! team ships changes how the runtime itself is driven.

use crate::shared::config_format::ConfigFormat;
use std::collections::BTreeSet;
use std::fmt;
use std::io::Read;
use std::path::Path;

use super::adapter::{self, LaunchMode, ResolvedLaunchProfile};
use crate::domain::Principal;
use crate::install::agent_account_store::{
    get_active_account, list_profiles, resolve_agent_account, AccountRequirement,
    AccountResolutionAvailability, AccountResolutionFreshness, AccountResolutionReason,
    AccountResolutionSource, ResolvedAgentAccount,
};
use codesense_pty_core::agent_cli::{descriptor, Descriptor, McpScope};

use super::binding;
use super::content_reader::{ContentReader, FsContentReader};
use super::keys::AgentKey;
use super::launch_check::{self, LaunchWarning, LaunchWarningCode};
use super::manifest::{
    content_hash, AgentRevisionRef, ContentLayer, ManifestError, ManifestErrorCode, StartupManifest,
};
use super::manifest_resolve::{content_layers_with, resolve_manifest_with, ManifestTarget};
use super::model::{self, Agent};
use super::paths::StartupPath;
use super::permissions;
use super::provides::{self, AgentProvides};
use super::roots;
use super::service::{self, AgentError, Placement};
use codesense_pty_core::agent_cli::AgentCli;

/// How to resolve.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolveOptions {
    /// The `(org, team)` the launch runs against. `None` resolves against the
    /// agent's own team (a personal agent: the personal space alone).
    pub target_team: Option<(String, String)>,
}

/// Where this user's account choice for the agent came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountSource {
    /// This user bound the agent to an account.
    Binding,
    /// No binding: the runtime's active account (or the CLI's own login).
    RuntimeDefault,
    /// The launcher named an account for this one launch, in place of the binding.
    Chosen,
}

/// Which provider account this user would launch the agent with.
#[derive(Debug, Clone)]
pub struct AgentAccountChoice {
    /// The runtime descriptor's policy; its provider is always the agent's
    /// runtime.
    pub policy: ResolvedAccountPolicy,
    /// This user's bound account, if any. A launch passes it as the explicit
    /// override.
    pub override_account_id: Option<String>,
    pub source: AccountSource,
    /// What the account resolver picks today. A binding that cannot be used
    /// (detached, missing, another provider's) shows here as its reason; it
    /// never falls back to another account.
    pub preview: ResolvedAgentAccount,
    /// Why the account store could not be read, when it could not. The
    /// preview is then unavailable.
    pub store_error: Option<String>,
    /// This user's consent to keep a raw terminal snapshot of an interactive
    /// launch, kept beside the binding. Off when the bindings file cannot be read.
    pub capture_raw_terminal: bool,
}

/// An agent resolved for a launch by one user.
#[derive(Debug, Clone)]
pub struct ResolvedAgent {
    pub agent: AgentRevisionRef,
    pub launch: ResolvedLaunchProfile,
    pub account: AgentAccountChoice,
    /// Exactly [`resolve_manifest`]'s output, with the generated permission
    /// entries and every launch error in it.
    pub manifest: StartupManifest,
    pub provides: AgentProvides,
    /// [`launch_check::warnings`] for the agent's declared launch with its
    /// `default_prompt` (none when empty).
    pub warnings: Vec<LaunchWarning>,
    pub launch_admission: super::adapter::LaunchAdmission,
    /// SW-AN-50: the MCP servers the target team's config offers for this
    /// runtime, the bridge aside, so the editor and starter detail can list
    /// what a copy could grant.
    pub offered_mcp_servers: Vec<String>,
    /// Whether all launch paths apply `manifest`. False during the limited
    /// rollout: named local product launches apply it; other paths retain prior behavior.
    pub manifest_applies_at_launch: bool,
    pub archived: bool,
}

impl ResolvedAgent {
    /// Resolve advisory launch facts for an existing selected local worktree.
    pub(crate) fn at_launch_cwd(mut self, cwd: Option<&Path>) -> Self {
        if let Some(cwd) = cwd {
            std::iter::Extend::extend(
                &mut self.warnings,
                launch_check::root_document_warnings(self.account.policy.runtime, cwd),
            );
        }
        self
    }
}

/// Why an agent cannot be resolved at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// No such agent, or its definition cannot be read.
    NotFound { key: AgentKey, why: String },
    /// Another user's personal agent.
    NotVisible(AgentKey),
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound { why, .. } => write!(f, "{why}"),
            Self::NotVisible(key) => write!(f, "agent {key} is not visible to you"),
        }
    }
}

impl std::error::Error for ResolveError {}

/// Resolve the saved agent `key`, at its current revision, for a launch by
/// `principal`. Archived agents resolve too.
pub fn resolve_agent(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    options: ResolveOptions,
) -> Result<ResolvedAgent, ResolveError> {
    resolve_pinned(home, principal, key, options).map(|(_, resolved)| resolved)
}

/// [`resolve_agent`], also returning the exact definition it resolved, for a
/// launch that regenerates per-mode entries from that same revision.
pub(crate) fn resolve_pinned(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    options: ResolveOptions,
) -> Result<(Agent, ResolvedAgent), ResolveError> {
    resolve_pinned_with(&FsContentReader, home, principal, key, options)
}

pub(crate) fn resolve_agent_with(
    reader: &dyn ContentReader,
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    options: ResolveOptions,
) -> Result<ResolvedAgent, ResolveError> {
    resolve_pinned_with(reader, home, principal, key, options).map(|(_, resolved)| resolved)
}

pub(crate) fn resolve_pinned_with(
    reader: &dyn ContentReader,
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    options: ResolveOptions,
) -> Result<(Agent, ResolvedAgent), ResolveError> {
    if !key.resolves_for(principal) {
        return Err(ResolveError::NotVisible(key.clone()));
    }
    let not_found = |e: AgentError| ResolveError::NotFound {
        key: key.clone(),
        why: e.to_string(),
    };
    let current = service::read_current(home, principal, key).map_err(not_found)?;
    let pinned = service::get_pinned(home, principal, key, current.revision).map_err(not_found)?;
    let resolved = compose(
        reader,
        home,
        principal,
        &pinned.agent,
        pinned.reference,
        options,
    );
    Ok((pinned.agent, resolved))
}

/// [`resolve_agent`] for an unsaved draft that would be saved at `placement`.
/// Its reference carries the draft's own revision and the hash of the draft
/// exactly as given: a save stamps the revision and creator, so the saved
/// agent's reference (and so its manifest hash) differs.
pub fn preview_agent(
    home: &Path,
    principal: &Principal,
    agent: &Agent,
    placement: &Placement,
    options: ResolveOptions,
) -> Result<ResolvedAgent, ResolveError> {
    preview_agent_with(&FsContentReader, home, principal, agent, placement, options)
}

pub(crate) fn preview_agent_with(
    reader: &dyn ContentReader,
    home: &Path,
    principal: &Principal,
    agent: &Agent,
    placement: &Placement,
    options: ResolveOptions,
) -> Result<ResolvedAgent, ResolveError> {
    let key = match placement {
        Placement::Personal(owner) => AgentKey::personal(principal, &owner.org, &agent.id),
        Placement::Team { org, team } => AgentKey::scoped(principal, org, team, &agent.id),
    };
    let reference = AgentRevisionRef {
        key,
        revision: agent.revision,
        definition_hash: content_hash(
            serde_yaml::to_string(agent)
                .expect("an agent definition serializes to YAML")
                .as_bytes(),
        ),
    };
    Ok(compose(reader, home, principal, agent, reference, options))
}

fn compose(
    reader: &dyn ContentReader,
    home: &Path,
    principal: &Principal,
    agent: &Agent,
    reference: AgentRevisionRef,
    options: ResolveOptions,
) -> ResolvedAgent {
    let account = account_choice(home, principal, &reference.key, agent.runtime, None);
    compose_with_reader(reader, home, principal, agent, reference, options, account)
}

pub(crate) fn compose_with_account(
    home: &Path,
    principal: &Principal,
    agent: &Agent,
    reference: AgentRevisionRef,
    options: ResolveOptions,
    account: AgentAccountChoice,
) -> ResolvedAgent {
    compose_with_reader(
        &FsContentReader,
        home,
        principal,
        agent,
        reference,
        options,
        account,
    )
}

fn compose_with_reader(
    reader: &dyn ContentReader,
    home: &Path,
    principal: &Principal,
    agent: &Agent,
    reference: AgentRevisionRef,
    options: ResolveOptions,
    account: AgentAccountChoice,
) -> ResolvedAgent {
    let descriptor = descriptor(agent.runtime);
    let mut launch = launch_profile(agent, &reference.key);
    launch.capture_raw_terminal = raw_capture(&account, launch.mode);
    let target = ManifestTarget::for_launch(
        &reference.key,
        options
            .target_team
            .map(|(org, team)| ManifestTarget::team(org, team)),
    );

    let generated = permissions::generate(agent, agent.runtime, agent.launch);
    let mut errors = definition_errors(agent);
    let dropped_mcp = dropped_mcp_servers(reader, home, principal, agent, &target, descriptor);
    errors.extend(dropped_mcp_errors(agent, &target, descriptor, &dropped_mcp));
    let offered_mcp_servers = offered_servers_with(reader, home, principal, agent.runtime, &target);
    let mcp_config_error =
        effective_mcp_config_with(reader, home, principal, agent.runtime, &target).err();
    // The runtime's root instructions file is the one optional entry: a team
    // without a `CLAUDE.md` is ordinary, so where no layer has it the agent
    // runs without it instead of being refused. Any other error on it stays.
    let sharing_warnings: Vec<_> = roots::team_view(
        home,
        principal,
        &target.org,
        &target.team,
        target.context_root.as_ref(),
    )
    .map(|view| {
        view.diagnostics
            .iter()
            .map(|message| LaunchWarning {
                code: LaunchWarningCode::Inheritance,
                message: message.clone(),
            })
            .collect()
    })
    .unwrap_or_default();
    let manifest = if names_root_doc(agent) {
        let manifest = resolve_manifest_with(
            reader,
            home,
            principal,
            agent,
            &reference,
            target.clone(),
            generated.clone(),
            errors.clone(),
        );
        match without_missing_root_doc(agent, &manifest) {
            Some(rest) => resolve_manifest_with(
                reader, home, principal, &rest, &reference, target, generated, errors,
            ),
            None => manifest,
        }
    } else {
        resolve_manifest_with(
            reader, home, principal, agent, &reference, target, generated, errors,
        )
    };
    let provides = provides::derive(agent.runtime, &manifest);

    let launch_admission = adapter::launch_admission(
        agent.runtime,
        home,
        principal,
        &manifest,
        &launch.args,
        reader,
        Some(&agent.default_prompt),
    );
    let mut resolved = ResolvedAgent {
        launch_admission,
        agent: reference,
        launch,
        account,
        manifest,
        provides,
        warnings: Vec::new(),
        offered_mcp_servers,
        manifest_applies_at_launch: false,
        archived: agent.is_archived(),
    };
    let prompt = Some(agent.default_prompt.as_str()).filter(|p| !p.trim().is_empty());
    resolved.warnings = launch_check::warnings(&resolved, prompt);
    resolved.warnings.extend(sharing_warnings);
    resolved.warnings.extend(context_warning(agent));
    resolved
        .warnings
        .extend(launch_check::usage_not_recorded_warning(agent));
    resolved.warnings.extend(silent_empty_mcp_warning(
        agent,
        &resolved.offered_mcp_servers,
    ));
    resolved
        .warnings
        .extend(silent_dropped_mcp_warning(&dropped_mcp));
    resolved
        .warnings
        .extend(mcp_config_unresolvable_warning(mcp_config_error.as_deref()));
    resolved
}

/// The warning for a `context` this build would refuse to save.
fn context_warning(agent: &Agent) -> Option<launch_check::LaunchWarning> {
    let context = agent.context.as_ref()?;
    let error = super::context::normalize_entries(context).err()?;
    Some(launch_check::LaunchWarning {
        code: launch_check::LaunchWarningCode::InvalidContext,
        message: format!("{error}; it is ignored until fixed"),
    })
}

/// SW-AN-50: the warning when the agent names no MCP servers but the team's
/// MCP config offers some (so a launch reaches only the CodeSense bridge, yet
/// Slack, GitHub, … are a copy-and-edit away). The loud case (an unknown
/// server) stays an error on `manifest`.
fn silent_empty_mcp_warning(
    agent: &Agent,
    offered: &[String],
) -> Option<launch_check::LaunchWarning> {
    if !agent.mcp.servers.is_empty() || offered.is_empty() {
        return None;
    }
    Some(launch_check::LaunchWarning {
        code: launch_check::LaunchWarningCode::NoMcpServersOffered,
        message: format!(
            "This agent has no MCP servers beyond the CodeSense bridge; the team offers: {}. \
             Edit the agent (or copy a starter) and check the servers it should reach.",
            offered.join(", ")
        ),
    })
}

/// Whether `raw` (a startup entry as written) names `runtime`'s root doc.
fn is_root_doc(runtime: AgentCli, raw: &str) -> bool {
    StartupPath::parse(raw).is_ok_and(|p| p.as_str() == descriptor(runtime).root_doc.as_str())
}

/// Whether `agent` names its runtime's root instructions file in `startup`.
fn names_root_doc(agent: &Agent) -> bool {
    agent
        .startup
        .iter()
        .any(|raw| is_root_doc(agent.runtime, raw))
}

/// `agent` without its runtime's root instructions file, when `manifest`
/// found that file in no layer. `None` otherwise.
fn without_missing_root_doc(agent: &Agent, manifest: &StartupManifest) -> Option<Agent> {
    let doc = descriptor(agent.runtime).root_doc.as_str();
    let missing = manifest.errors.iter().any(|e| {
        e.code == ManifestErrorCode::Missing && e.path.as_ref().is_some_and(|p| p.as_str() == doc)
    });
    missing.then(|| Agent {
        startup: agent
            .startup
            .iter()
            .filter(|raw| !is_root_doc(agent.runtime, raw))
            .cloned()
            .collect(),
        ..agent.clone()
    })
}

/// The account policy of a launch on `runtime`: its provider's login, and whether the
/// runtime's adapter requires one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAccountPolicy {
    pub runtime: AgentCli,
    pub requirement: AccountRequirement,
}

impl ResolvedAccountPolicy {
    /// The provider whose accounts the launch may run with.
    pub fn provider(&self) -> Option<crate::install::agent_account_store::AgentProvider> {
        adapter::provider_of(self.runtime)
    }
}

/// [`ResolvedAccountPolicy`] of `runtime`, read from its built-in adapter.
pub(crate) fn account_policy(runtime: AgentCli) -> ResolvedAccountPolicy {
    ResolvedAccountPolicy {
        runtime,
        requirement: adapter::desktop(runtime).account_requirement,
    }
}

/// The adapter's launch with the agent's identity, args, env, escalation, MCP scope and
/// mode in place. An agent is always scoped, so it reaches the bridge through its
/// projected MCP config and its allowlist is never `None`.
fn launch_profile(agent: &Agent, key: &AgentKey) -> ResolvedLaunchProfile {
    let desktop = adapter::desktop(agent.runtime);
    let projects_bridge_tool_allowlist =
        desktop.enforces_bridge_tool_allowlist || desktop.home_config.is_some();
    let profile = ResolvedLaunchProfile::for_agent(
        agent.runtime,
        key.to_string(),
        agent.args.clone(),
        agent.is_escalating(),
        agent.mcp.servers.clone(),
        match agent.launch {
            model::LaunchMode::Foreground => LaunchMode::Interactive,
            model::LaunchMode::Headless => LaunchMode::Headless,
        },
    );
    ResolvedLaunchProfile {
        env: agent.env.vars().clone(),
        bridge_tools: projects_bridge_tool_allowlist
            .then(|| agent.mcp.bridge_tools.clone())
            .flatten(),
        effective_bridge_tools: if projects_bridge_tool_allowlist {
            static NAMES: std::sync::OnceLock<BTreeSet<String>> = std::sync::OnceLock::new();
            let names = NAMES.get_or_init(|| {
                crate::mcp::handler::CodesenseHandler::served_tool_router()
                    .list_all()
                    .iter()
                    .map(|tool| tool.name.to_string())
                    .collect()
            });
            names
                .iter()
                .filter(|name| {
                    agent
                        .mcp
                        .bridge_tools
                        .as_ref()
                        .is_none_or(|tools| tools.contains(name))
                })
                .cloned()
                .collect()
        } else {
            Vec::new()
        },
        ..profile
    }
}

/// Whether a launch in `mode` keeps a raw terminal snapshot: only an
/// interactive one, and only with this user's consent.
pub(crate) fn raw_capture(account: &AgentAccountChoice, mode: LaunchMode) -> bool {
    account.capture_raw_terminal && mode == LaunchMode::Interactive
}

/// The account `principal` would launch the agent `key` (on `runtime`) with,
/// without resolving its manifest: what a list row shows.
pub fn account_preview(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    runtime: AgentCli,
) -> AgentAccountChoice {
    account_choice(home, principal, key, runtime, None)
}

/// An account the launcher names for one launch, at the generation it saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChosenAccount {
    pub id: String,
    pub generation: Option<u64>,
}

/// [`account_preview`] for a launch that names its own account: the binding is
/// left alone, and the account must still be the one the launcher saw.
pub(crate) fn chosen_account(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    runtime: AgentCli,
    chosen: &ChosenAccount,
) -> AgentAccountChoice {
    account_choice(home, principal, key, runtime, Some(chosen))
}

/// This user's account for the agent: the launcher's chosen account, else
/// their binding, else the runtime's active account, as the launch-time
/// resolver would pick it now.
fn account_choice(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    runtime: AgentCli,
    chosen: Option<&ChosenAccount>,
) -> AgentAccountChoice {
    let policy = account_policy(runtime);
    let provider = adapter::provider_of(runtime);
    let stored = binding::stored(home, principal, key).map_err(|e| e.to_string());
    // Consent lives in the bindings file alone, so a broken account store never hides it.
    let capture_raw_terminal = stored.as_ref().is_ok_and(|s| s.capture_raw_terminal);
    let Some(provider) = provider else {
        return AgentAccountChoice {
            policy,
            override_account_id: None,
            source: if chosen.is_some() {
                AccountSource::Chosen
            } else {
                AccountSource::RuntimeDefault
            },
            preview: ResolvedAgentAccount {
                spawn_eligible: chosen.is_none(),
                ..crate::install::agent_account_store::resolve_without_managed_account()
            },
            store_error: None,
            capture_raw_terminal,
        };
    };
    let read = || -> Result<_, String> {
        // A launch that names its own account never reads the binding.
        // An unattended starter with no account of its own runs on the account its
        // ordinary twin is bound to, so a background launch that names no agent keeps
        // the login the user chose for the Claude or Codex starter.
        let bound = match chosen {
            Some(_) => None,
            None => match stored.clone()?.account_id {
                Some(id) => Some(id),
                None => match super::builtin::attended_twin(key) {
                    Some(twin) => {
                        binding::stored(home, principal, &twin)
                            .map_err(|e| e.to_string())?
                            .account_id
                    }
                    None => None,
                },
            },
        };
        let profiles = list_profiles(home, principal).map_err(|e| e.to_string())?;
        let active = get_active_account(home, principal, provider).map_err(|e| e.to_string())?;
        Ok((bound, profiles, active))
    };
    match read() {
        Ok((bound, profiles, active)) => {
            // A binding follows its account, not one generation of it, so the
            // current generation is the expected one. A chosen account must be
            // the generation the launcher saw.
            let (override_account_id, generation, source) = match chosen {
                Some(chosen) => (
                    Some(chosen.id.clone()),
                    chosen.generation,
                    AccountSource::Chosen,
                ),
                None => {
                    let generation = bound
                        .as_deref()
                        .and_then(|id| profiles.iter().find(|p| p.id == id))
                        .map(|p| p.generation);
                    let source = if bound.is_some() {
                        AccountSource::Binding
                    } else {
                        AccountSource::RuntimeDefault
                    };
                    (bound, generation, source)
                }
            };
            let preview = resolve_agent_account(
                policy.requirement,
                provider,
                &profiles,
                active.as_ref(),
                override_account_id.as_deref(),
                generation,
            );
            AgentAccountChoice {
                policy,
                source,
                override_account_id,
                preview,
                store_error: None,
                capture_raw_terminal,
            }
        }
        Err(error) => AgentAccountChoice {
            policy,
            override_account_id: None,
            source: AccountSource::RuntimeDefault,
            preview: ResolvedAgentAccount {
                source: AccountResolutionSource::None,
                account_id: None,
                provider: Some(provider),
                label: None,
                generation: None,
                reason: AccountResolutionReason::RequiredMissing,
                availability: AccountResolutionAvailability::Error,
                freshness: AccountResolutionFreshness::Unknown,
                spawn_eligible: false,
            },
            store_error: Some(error),
            capture_raw_terminal,
        },
    }
}

/// What the definition itself breaks, beyond its startup paths (which the
/// manifest resolver reports, so they are left out here and cannot hide a
/// later problem). A draft need not have a revision yet.
fn definition_errors(agent: &Agent) -> Vec<ManifestError> {
    // SW-XS-13: `context` does not block a launch yet; an invalid one is a
    // warning (see `context_warning`) until launches apply it (SW-XS-7).
    let rest = Agent {
        revision: agent.revision.max(1),
        startup: Vec::new(),
        context: None,
        ..agent.clone()
    };
    match rest.validate() {
        Ok(()) => Vec::new(),
        Err(e) => vec![ManifestError {
            path: None,
            code: ManifestErrorCode::Invalid,
            message: e.to_string(),
        }],
    }
}

/// SW-AN-52: MCP servers the agent names that the target team's content-layer
/// MCP config does not define. The entries are still passed through to the
/// launch; the launch drops each as it projects the config, so the launcher
/// sees exactly the set the content-layer source of truth offers.
fn dropped_mcp_servers(
    reader: &dyn ContentReader,
    home: &Path,
    principal: &Principal,
    agent: &Agent,
    target: &ManifestTarget,
    descriptor: &Descriptor,
) -> Vec<String> {
    let Some(scope) = descriptor.mcp_scope.as_ref() else {
        return Vec::new();
    };
    if agent.mcp.servers.is_empty() {
        return Vec::new();
    }
    let present = present_servers(reader, home, principal, target, scope);
    agent
        .mcp
        .servers
        .iter()
        .filter(|s| !present.contains(s.trim()))
        .cloned()
        .collect()
}

/// One `invalid` error per dropped MCP server, naming the content-layer file
/// the entry should have appeared in.
fn dropped_mcp_errors(
    agent: &Agent,
    target: &ManifestTarget,
    descriptor: &Descriptor,
    dropped: &[String],
) -> Vec<ManifestError> {
    let Some(scope) = descriptor.mcp_scope.as_ref() else {
        return Vec::new();
    };
    if agent.mcp.servers.is_empty() || dropped.is_empty() {
        return Vec::new();
    }
    dropped
        .iter()
        .map(|server| ManifestError {
            path: None,
            code: ManifestErrorCode::Invalid,
            message: format!(
                "MCP server {server:?} is not in the {} of {}/{}; the launch will not have it",
                scope.config_filename, target.org, target.team
            ),
        })
        .collect()
}

/// SW-AN-52: the aggregated warning named after the AC-2 wording when the
/// agent names MCP servers the content-layer config does not define. Named
/// alongside the per-field manifest errors (`dropped_mcp_errors`) so UIs that
/// surface warnings prominently see one line, not N.
fn silent_dropped_mcp_warning(dropped: &[String]) -> Option<launch_check::LaunchWarning> {
    if dropped.is_empty() {
        return None;
    }
    Some(launch_check::LaunchWarning {
        code: launch_check::LaunchWarningCode::McpServersDropped,
        message: format!(
            "{} of this agent's MCP servers were not found at launch and were dropped: {}.",
            dropped.len(),
            dropped.join(", ")
        ),
    })
}

/// SW-AN-52: the warning when the content-layer MCP config cannot be resolved
/// (bytes-limit, parse error, I/O failure). Launch falls back to the cwd
/// `.mcp.json` alone — never refuses — so the user sees why servers defined in
/// the content layer may silently drop.
fn mcp_config_unresolvable_warning(reason: Option<&str>) -> Option<launch_check::LaunchWarning> {
    let reason = reason?;
    Some(launch_check::LaunchWarning {
        code: launch_check::LaunchWarningCode::McpConfigUnresolvable,
        message: format!(
            "This team's MCP config could not be read ({reason}). The launch falls back to the \
             worktree's own .mcp.json alone; any server defined only in the team's config will \
             not reach the agent."
        ),
    })
}

/// The MCP servers an agent on `runtime` can name when launched into
/// `target`: every server its workspace's MCP config has, the always-present
/// bridge aside, by key.

fn offered_servers_with(
    reader: &dyn ContentReader,
    home: &Path,
    principal: &Principal,
    runtime: AgentCli,
    target: &ManifestTarget,
) -> Vec<String> {
    let Some(scope) = descriptor(runtime).mcp_scope.as_ref() else {
        return Vec::new();
    };
    let mut present = present_servers(reader, home, principal, target, scope);
    present.remove(&scope.bridge_id);
    present.into_iter().collect()
}

/// Resolve the non-generated MCP definitions from the same authoritative
/// content layers used by Agent validation. Nearest content wins over
/// inherited content. No workspace projection is required.
pub(crate) fn effective_mcp_config(
    home: &Path,
    principal: &Principal,
    runtime: AgentCli,
    target: &ManifestTarget,
) -> Result<serde_json::Value, String> {
    effective_mcp_config_with(&FsContentReader, home, principal, runtime, target)
}

fn effective_mcp_config_with(
    reader: &dyn ContentReader,
    home: &Path,
    principal: &Principal,
    runtime: AgentCli,
    target: &ManifestTarget,
) -> Result<serde_json::Value, String> {
    use crate::shared::json_io::deep_merge;
    let scope = descriptor(runtime)
        .mcp_scope
        .as_ref()
        .ok_or("runtime MCP scope unavailable")?;
    let mut merged = serde_json::json!({ scope.server_map_key.clone(): {} });
    let mut total_bytes = 0usize;
    for (layer, root) in content_layers_with(reader, home, principal, target)
        .into_iter()
        .rev()
    {
        if matches!(layer, ContentLayer::Personal { org: None, .. }) {
            continue;
        }
        let mut current = serde_json::json!({});
        match root.open(reader, &scope.config_filename) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|_| "Agent MCP config read failed")?;
                total_bytes = total_bytes.saturating_add(bytes.len());
                if bytes.len() > 1024 * 1024 || total_bytes > 4 * 1024 * 1024 {
                    return Err("Agent MCP configuration exceeds limit".into());
                }
                current = ConfigFormat::from(scope.format)
                    .parse(&bytes)
                    .map_err(|_| "invalid Agent MCP configuration")?;
                if !current.is_object() {
                    return Err("Agent MCP configuration must be an object".into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("Agent MCP configuration unavailable or unsafe".into()),
        }
        // Only the server map is launch input; provider auth-home/global config
        // keys are never copied into the remote startup payload.
        if let Some(servers) = current.get(&scope.server_map_key) {
            if !servers.is_object() {
                return Err("Agent MCP server map must be an object".into());
            }
            deep_merge(&mut merged[&scope.server_map_key], servers);
        }
        if serde_json::to_vec(&merged)
            .map_err(|e| e.to_string())?
            .len()
            > 1024 * 1024
        {
            return Err("merged Agent MCP configuration exceeds limit".into());
        }
    }
    Ok(merged)
}

/// The server keys a workspace of `target` gets in the runtime's MCP config:
/// the bridge, every server in that config file across the target's content
/// layers (it is a merged file).
fn present_servers(
    reader: &dyn ContentReader,
    home: &Path,
    principal: &Principal,
    target: &ManifestTarget,
    scope: &McpScope,
) -> BTreeSet<String> {
    let mut present = BTreeSet::from([scope.bridge_id.clone()]);
    let mut add = |value: Option<&serde_json::Value>| {
        if let Some(servers) = value.and_then(serde_json::Value::as_object) {
            present.extend(servers.keys().map(|k| k.trim().to_string()));
        }
    };
    for (layer, root) in content_layers_with(reader, home, principal, target) {
        // SLW-PT-19: all-orgs is inert text only — never an MCP config.
        if matches!(layer, ContentLayer::Personal { org: None, .. }) {
            continue;
        }
        // Read like any content file: never through a symlink.
        let mut bytes = Vec::new();
        let read = root
            .open(reader, &scope.config_filename)
            .and_then(|mut f| f.read_to_end(&mut bytes));
        if read.is_ok() {
            if let Ok(config) = ConfigFormat::from(scope.format).parse(&bytes) {
                add(config.get(&scope.server_map_key));
            }
        }
    }
    present
}

#[cfg(test)]
pub(crate) mod tests;
