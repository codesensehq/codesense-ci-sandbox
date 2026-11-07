// SPDX-License-Identifier: MIT
//! The desktop's side of each agent CLI: what only the app needs on top of the shared
//! adapter in [`codesense_pty_core::agent_cli`] (which login a launch needs, the headless
//! runner recipe), and the launch shapes the app builds from both. Teams customise an
//! agent through its `agent.yaml` (runtime, args, MCP, permissions, startup), never by
//! editing an adapter; which login runs it is each user's machine-local binding.

use codesense_pty_core::agent_cli::{
    adapter, descriptor, AgentCli, AttentionSignal, HeadlessStructured, InteractiveStructured,
    McpScope,
};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use super::launch::AgentLaunch;
use super::manifest::GeneratedEntry;
use super::model::{Agent, Permissions, ValidationError};
use crate::domain::Principal;
use crate::install::agent_account_store::{AccountRequirement, AgentAccountProfile, AgentProvider};
use crate::session_usage::event::UsageEvent;

/// What the app alone needs to drive one agent CLI.
#[derive(Debug, Clone)]
pub struct DesktopCli {
    /// Whether a launch needs one of this CLI's logins.
    pub account_requirement: AccountRequirement,
    /// Resolve a required non-secret model endpoint before a managed launch.
    pub model_endpoint: Option<ModelEndpointPort>,
    /// Whether a live session can receive routine wake deliveries through this CLI's
    /// native integration.
    pub supports_wake_delivery: bool,
    /// Whether a remote machine can launch this CLI as a native tab.
    pub supports_remote_launch: bool,
    /// Whether this CLI is offered as a preview: listed, with some of its integration
    /// still to come. The New agent templates and Settings label it.
    pub preview: bool,
    /// How attribution capture hooks are wired for this CLI.
    pub attribution: Attribution,
    /// The headless runner recipe (`analyze` / `run_skill`).
    pub runner: RunnerRecipe,
    /// How its starter agents read in the agent list.
    pub starter: Starter,
    /// How its unattended starter runs with no prompt ever blocking it: whether it passes
    /// the bypass flag, and its permission block.
    pub unattended: Unattended,
    /// Whether `agent`'s own permission mode (in its args, which the CLI lets win, or its
    /// permissions) has a launch approve all tool use, besides the bypass flag.
    pub escalates: fn(&Agent) -> bool,
    /// The launch settings `agent` runs with in `mode`: its own permissions, verbatim,
    /// with what CodeSense needs.
    pub launch_settings: fn(&Agent, super::model::LaunchMode) -> GeneratedEntry,
    /// Additional environment and private configuration prepared for a managed launch.
    pub managed_launch_env: fn() -> Result<Vec<(String, String)>, String>,
    /// Seed a private scoped home from a prepared runtime before it can be spawned.
    /// Inputs are the session home and exact selected executable; output is launch env.
    pub seed_home: Option<fn(&Path, &Path) -> Result<Vec<(String, String)>, String>>,
    /// Stop private-home background processes before capture or removal.
    pub teardown_home: Option<fn(&Path)>,
    /// Project cwd-dependent managed configuration, without writing the file.
    pub home_config: Option<fn(&AgentLaunch, &Path, &mut serde_json::Value) -> Result<(), String>>,
    /// Entries CodeSense adds to the inline config of a launch of a CLI that takes its
    /// scoped config inline (a plugin to load, say). Empty when it adds none. `qualified`
    /// is whether the CLI version this launch spawns is inside the range CodeSense is
    /// qualified on; a CLI with entries that depend on its version's contract adds none
    /// when it is not.
    pub inline_config:
        fn(qualified: bool) -> Result<serde_json::Map<String, serde_json::Value>, String>,
    /// A shell fragment for a user-typed CLI, when this runtime uses an env-content config.
    pub shell_mcp_config: fn(&str) -> Option<(&'static str, &'static str, serde_json::Value)>,
    /// Pre-trust the folder a managed launch runs in, so no trust prompt blocks a launch
    /// nobody watches. Best effort: a failure is logged, never fatal.
    pub pretrust: fn(&AgentLaunch, &Path, &str) -> Vec<String>,
    /// Its block of a saved Agent's `permissions`.
    pub permissions: PermissionBlock,
    /// Whether the CLI can enforce a saved agent's bridge tool allowlist.
    pub enforces_bridge_tool_allowlist: bool,
    /// Adapter-owned notes for advisory posture that cannot be expressed as a rule.
    pub launch_notes: fn(&[String]) -> Vec<String>,
    /// Describe native settings for the Overview without a runtime branch in the UI.
    pub permission_display: Option<fn(&serde_json::Value, &[String]) -> String>,
    /// The settings a headless profile launch's per-launch settings start from, so a run
    /// nobody watches never parks on an approval prompt.
    pub headless_settings: fn() -> serde_json::Value,
    /// Translates a permission map into this CLI's native ordered rules at the config
    /// boundary; `None` for a CLI whose settings already carry the map verbatim.
    pub native_permission_rules: Option<fn(&serde_json::Value) -> serde_json::Value>,
    /// How an account's CLI is pointed at CodeSense's status-line receiver, for a CLI whose
    /// usage comes from its status line ([`Usage::StatusLine`]); `None` for any other.
    ///
    /// [`Usage::StatusLine`]: codesense_pty_core::agent_cli::Usage::StatusLine
    pub status_line: Option<StatusLineInstall>,
    /// One of its usage records among the OpenTelemetry log records it exports, as a usage
    /// event; `None` for any other record.
    pub usage_record: fn(&crate::session_usage::telemetry::Attrs) -> Option<UsageEvent>,
    /// Where the CLI reads managed settings, in the order it checks them, for a CLI whose
    /// managed settings apply over a launch's environment and so can send its usage logs past
    /// CodeSense's collector; empty for any other. Only one CLI lists paths today, so
    /// `session_usage::managed_telemetry` reads every listed file with that CLI's log keys; a
    /// second CLI here needs its own keys there.
    pub managed_settings: &'static [&'static str],
    /// The usage events in a session's own log, given that log's path, for a CLI whose usage
    /// CodeSense reads there while managed settings send its usage logs elsewhere; `None` for
    /// any other.
    pub session_log_usage: Option<fn(&Path) -> Vec<UsageEvent>>,
}

pub type ModelEndpointResolver = fn(
    &Path,
    &Principal,
    &super::manifest::StartupManifest,
    &[String],
    &dyn super::content_reader::ContentReader,
) -> Result<serde_json::Value, String>;

#[derive(Debug, Clone, Copy)]
pub struct ModelEndpointPort {
    pub resolve: ModelEndpointResolver,
    pub missing_message: fn() -> String,
    pub model_arguments: fn(&[String]) -> ModelArguments,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelArguments {
    pub input_args: Vec<String>,
    pub model: Option<String>,
    pub edit: Option<ModelArgumentEdit>,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelArgumentEdit {
    pub args: Vec<String>,
    pub clear_args: Vec<String>,
    pub value_index: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LaunchAdmission {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_display: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_arguments: Option<ModelArguments>,
    pub allowed: bool,
    pub reason: Option<String>,
    pub model_endpoint: bool,
    pub model: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_prompt_max_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_prompt_too_long: Option<String>,
}

/// The adapter's task budget, shared by preview and final composed startup.
pub(crate) fn initial_prompt_refusal(runtime: AgentCli, prompt: Option<&str>) -> Option<String> {
    let (max, reason) = descriptor(runtime).initial_prompt_limit.as_ref()?;
    prompt
        .filter(|prompt| prompt.len() > *max)
        .map(|_| reason.clone())
}

pub fn launch_admission(
    runtime: AgentCli,
    home: &Path,
    principal: &Principal,
    manifest: &super::manifest::StartupManifest,
    args: &[String],
    reader: &dyn super::content_reader::ContentReader,
    prompt: Option<&str>,
) -> LaunchAdmission {
    let limit = descriptor(runtime).initial_prompt_limit.as_ref();
    let desktop = desktop(runtime);
    let (model_endpoint, model, reason) = match desktop.model_endpoint {
        None => (false, None, None),
        Some(port) => match (port.resolve)(home, principal, manifest, args, reader) {
            Ok(model) => (true, Some(model), None),
            Err(reason) => (true, None, Some(reason)),
        },
    };
    let reason = reason.or_else(|| initial_prompt_refusal(runtime, prompt));
    LaunchAdmission {
        model_arguments: desktop.model_endpoint.map(|port| (port.model_arguments)(args)),
        permission_display: desktop.permission_display.map(|describe| {
            let empty = serde_json::Value::Null;
            let settings = std::iter::Iterator::find(&mut manifest.generated.iter(), |entry| {
                entry.target == super::manifest::GeneratedTarget::LaunchSettings
            })
            .map(|entry| &entry.content)
            .unwrap_or(&empty);
            describe(settings, args)
        }),
        allowed: reason.is_none(),
        reason,
        model_endpoint,
        model,
        initial_prompt_max_bytes: limit.map(|(max, _)| *max),
        initial_prompt_too_long: limit.map(|(_, reason)| reason.clone()),
    }
}

/// How a desktop CLI captures line attribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribution {
    /// Capture is unavailable, with the reason reported on the worktree.
    Unsupported(&'static str),
    /// Checkpoint hooks live in the launch settings file.
    SettingsHooks,
    /// Checkpoint hooks are passed as argv configuration overrides.
    ArgvHooks,
}

/// A CLI's block of a saved Agent's `permissions` (keyed by its id), which applies only to
/// an agent that runs on it.
#[derive(Debug, Clone, Copy)]
pub struct PermissionBlock {
    /// Refuses a block that names a key or value the CLI cannot take.
    pub validate: fn(&Permissions) -> Result<(), ValidationError>,
    /// Replaces the block with `block` (`null` removes it).
    pub set: fn(&mut Permissions, serde_json::Value) -> Result<(), serde_json::Error>,
}

/// How an account's managed home has its CLI's status line pass through CodeSense's
/// receiver, and how that is undone.
#[derive(Debug, Clone, Copy)]
pub struct StatusLineInstall {
    /// Wraps the account's status line command so each redraw also reaches the receiver at
    /// `socket` with `capability`, recording what it owns on the account.
    pub install: fn(
        root: &Path,
        principal: &Principal,
        profile: &AgentAccountProfile,
        socket: &Path,
        capability: &str,
    ) -> Result<AgentAccountProfile, String>,
    /// Puts back the status line the install found, when CodeSense's is still in place.
    pub restore: fn(root: &Path, principal: &Principal, account_id: &str) -> Result<(), String>,
}

/// The runtime a launch that names no agent runs on.
pub const PREFERRED_RUNTIME: AgentCli = AgentCli::Claude;

/// How a CLI's unattended starter runs.
#[derive(Debug, Clone)]
pub struct Unattended {
    /// It passes the descriptor's bypass flag.
    pub bypass_flag: bool,
    pub permissions: super::model::Permissions,
}

/// The names and descriptions of a CLI's starter and its unattended twin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Starter {
    pub name: &'static str,
    pub description: &'static str,
    pub unattended_name: &'static str,
    pub unattended_description: &'static str,
}

/// The app's side of `cli`.
pub fn desktop(cli: AgentCli) -> &'static DesktopCli {
    static CLAUDE: LazyLock<DesktopCli> = LazyLock::new(claude::desktop);
    static CODEX: LazyLock<DesktopCli> = LazyLock::new(codex::desktop);
    static OPENCODE: LazyLock<DesktopCli> = LazyLock::new(opencode::desktop);
    static HERMES: LazyLock<DesktopCli> = LazyLock::new(hermes::desktop);
    match cli {
        AgentCli::Claude => &CLAUDE,
        AgentCli::Codex => &CODEX,
        AgentCli::OpenCode => &OPENCODE,
        AgentCli::Hermes => &HERMES,
    }
}

/// The provider whose logins run `cli`.
pub fn provider_of(cli: AgentCli) -> Option<AgentProvider> {
    match cli {
        AgentCli::Claude => Some(AgentProvider::Claude),
        AgentCli::Codex => Some(AgentProvider::Codex),
        AgentCli::OpenCode | AgentCli::Hermes => None,
    }
}

/// The CLI whose logins `provider` holds.
pub fn cli_of_provider(provider: AgentProvider) -> AgentCli {
    match provider {
        AgentProvider::Claude => AgentCli::Claude,
        AgentProvider::Codex => AgentCli::Codex,
    }
}

mod claude;
pub(crate) use claude::MANAGED_SETTINGS_PATHS as CLAUDE_MANAGED_SETTINGS_PATHS;
mod codex;
pub(crate) mod hermes;
mod opencode;

fn strings(tokens: &[&str]) -> Vec<String> {
    tokens.iter().map(|t| t.to_string()).collect()
}

/// Expand an interactive session-id argv template, refusing anything but exactly one
/// standalone `{session_id}` token among plain ones.
pub(crate) fn expand_session_id_argv(
    template: &[String],
    session_id: &str,
) -> Result<Vec<String>, String> {
    if template.is_empty() {
        return Err("structured_transcript.interactive.session_id_argv is empty".to_string());
    }
    let mut placeholder_count = 0;
    let mut expanded = Vec::with_capacity(template.len());
    for token in template {
        if token.is_empty() {
            return Err(
                "structured_transcript.interactive.session_id_argv contains an empty token"
                    .to_string(),
            );
        }
        if token == "{session_id}" {
            placeholder_count += 1;
            expanded.push(session_id.to_string());
        } else {
            if token.contains('{') || token.contains('}') {
                return Err(format!(
                    "structured_transcript.interactive.session_id_argv token {token:?} contains an unsupported or embedded placeholder"
                ));
            }
            expanded.push(token.clone());
        }
    }
    if placeholder_count != 1 {
        return Err(format!(
            "structured_transcript.interactive.session_id_argv must contain exactly one standalone {{session_id}} token (found {placeholder_count})"
        ));
    }
    Ok(expanded)
}

/// The quiet window the terminal dot floor uses for an idle-output CLI.
pub const DEFAULT_ATTENTION_QUIET_SECS: u32 = 10;

/// Whether `arg` on its own marks a launch as auto-approving all tool use: it is a
/// runtime's blanket bypass flag.
pub fn is_escalating_flag(arg: &str) -> bool {
    AgentCli::ALL
        .into_iter()
        .any(|cli| descriptor(cli).bypass_flag == arg)
}

/// The execution model of a launch: a terminal tab, or headless (no tab; observed in the
/// agents list).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LaunchMode {
    #[default]
    Interactive,
    Headless,
}

/// The headless runner recipe consumed by `agent_runner`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerRecipe {
    /// Default model substituted into the `{model}` placeholder. `None` drops the
    /// `--model {model}` pair (a CLI that takes no model flag).
    pub default_model: Option<String>,
    /// Single-turn, tool-less `analyze` argv template.
    pub analyze_args: Vec<String>,
    /// Write-capable `run_skill` argv template (multi-turn, edit-capable).
    pub skill_args: Vec<String>,
}

/// An agent's launch on its runtime's adapter: the shape a launch runs with (binary,
/// args, escalation, MCP scope, mode), so the FE never re-implements it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedLaunchProfile {
    /// The agent key.
    pub id: String,
    /// The command basename (e.g. `"claude"`).
    pub binary: String,
    /// Extra argv the launcher appends after the resolved agent path.
    pub args: Vec<String>,
    /// Whether the launch auto-approves all tool use.
    pub escalating: bool,
    /// The agent's MCP-server allowlist: the launch is scoped to
    /// `(allow ∪ {bridge}) ∩ present` via an ephemeral config. `None` = unfiltered.
    #[serde(default)]
    pub mcp_allow: Option<Vec<String>>,
    /// Codex's declared bridge restriction: null allows all, [] allows none.
    pub bridge_tools: Option<Vec<String>>,
    /// Registered bridge tools this launch can call, after the restriction.
    pub effective_bridge_tools: Vec<String>,
    /// The effective execution model.
    #[serde(default)]
    pub mode: LaunchMode,
    /// The adapter's MCP-scope mechanism, so a launch site projects the launched CLI's
    /// shape. Never crosses to the webview.
    #[serde(skip)]
    pub mcp_scope: Option<McpScope>,
    /// The adapter's env-strip set; the FE-composed interactive spawn strips it.
    pub env_suppress: Vec<String>,
    /// The agent's own environment variables, set on the CLI under every variable
    /// CodeSense sets for the launch.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    pub account_requirement: AccountRequirement,
    /// The adapter's interactive structured-transcript mechanism, read at session end.
    /// Never crosses to the webview.
    #[serde(skip)]
    pub structured_interactive: Option<InteractiveStructured>,
    /// The launching user's raw-terminal consent, from their agent binding, for an
    /// interactive agent launch. Always off for headless.
    #[serde(skip)]
    pub capture_raw_terminal: bool,
}

impl ResolvedLaunchProfile {
    /// `cli`'s launch of the agent `id` with its own `args`, allowlist and mode.
    /// Nothing is injected: a flag is in the argv only if the agent put it there.
    pub(crate) fn for_agent(
        cli: AgentCli,
        id: String,
        args: Vec<String>,
        escalating: bool,
        mcp_allow: Vec<String>,
        mode: LaunchMode,
    ) -> Self {
        let escalating = escalating || args.iter().any(|a| is_escalating_flag(a));
        let descriptor = descriptor(cli);
        Self {
            id,
            binary: descriptor.binary.clone(),
            args,
            escalating,
            mcp_allow: Some(mcp_allow),
            bridge_tools: None,
            effective_bridge_tools: Vec::new(),
            mode,
            mcp_scope: descriptor.mcp_scope.clone(),
            env_suppress: descriptor.env_suppress.clone(),
            env: BTreeMap::new(),
            account_requirement: desktop(cli).account_requirement,
            structured_interactive: adapter(cli)
                .output()
                .transcript
                .and_then(|s| s.interactive.clone()),
            capture_raw_terminal: false,
        }
    }
}

/// How to recognize one agent CLI in a terminal and how it raises needs-you — what the
/// frontend's per-tab dot floor probes for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSignalInfo {
    /// The process basename the liveness probe matches.
    pub basename: String,
    /// The CLI's attention channel; `None` = liveness only.
    pub attention: Option<AttentionSignal>,
    /// Quiet window for `idle-output` attention.
    pub quiet_secs: u32,
}

/// The built-in CLIs' liveness and attention signals, Claude first.
pub fn builtin_agent_signals() -> Vec<AgentSignalInfo> {
    AgentCli::ALL
        .into_iter()
        .map(descriptor)
        .map(|descriptor| AgentSignalInfo {
            basename: descriptor.binary.clone(),
            attention: descriptor.attention,
            quiet_secs: DEFAULT_ATTENTION_QUIET_SECS,
        })
        .collect()
}

/// The launch shape of the runtime an agent runs on — everything a launch site needs to
/// project that runtime (binary, headless entry, MCP scope, env-strip). Owned so no
/// descriptor reference is held across an await.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchDescriptor {
    /// The binary basename (Claude `claude`, Codex `codex`).
    pub binary_basename: String,
    /// The headless-mode entry tokens (Claude `["-p"]`, Codex `["exec"]`).
    pub headless_entry: Vec<String>,
    /// The MCP-scope mechanism, or `None` when the CLI declares none.
    pub mcp_scope: Option<McpScope>,
    /// The env-strip set.
    pub env_suppress: Vec<String>,
    /// The headless structured-transcript mechanism, or `None` (the run stays on the text
    /// floor). The interactive arm is read at session end, not at launch.
    pub structured_headless: Option<HeadlessStructured>,
    /// The argv that gives a freshly spawned interactive CLI a CodeSense-minted session id, or `None`.
    pub session_id_argv: Option<Vec<String>>,
    /// How the CLI's own telemetry is pointed at CodeSense's local receiver, or `None`.
    pub telemetry: Option<codesense_pty_core::agent_cli::TelemetryExport>,
}

impl LaunchDescriptor {
    pub(crate) fn of(cli: AgentCli) -> Self {
        let descriptor = descriptor(cli);
        Self {
            binary_basename: descriptor.binary.clone(),
            headless_entry: descriptor.headless_entry.clone(),
            mcp_scope: descriptor.mcp_scope.clone(),
            env_suppress: descriptor.env_suppress.clone(),
            structured_headless: adapter(cli)
                .output()
                .transcript
                .and_then(|s| s.headless.clone()),
            session_id_argv: adapter(cli)
                .output()
                .transcript
                .and_then(|s| s.interactive.as_ref())
                .and_then(|i| i.session_id_argv.clone()),
            telemetry: descriptor.telemetry.clone(),
        }
    }
}

#[cfg(test)]
mod tests;

/// Translate a declared permission map into a CLI's native ordered permission rules at the
/// config boundary: each `(action, effect)` becomes a `{ action, resource: "*", effect }`
/// rule, and the shell action is renamed to the host's word for it.
pub(crate) fn native_permission_rules(map: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({"permissions": map.as_object().into_iter().flatten().map(|(action, effect)| {
        let action = if action == "bash" { "shell" } else { action.as_str() };
        serde_json::json!({"action": action, "resource": "*", "effect": effect})
    }).collect::<Vec<_>>()})
}

/// Internal lifecycle dispatch includes registered adapters while discovery stays listed-only.
pub(crate) fn teardown_managed_home(home: &Path) {
    if std::fs::symlink_metadata(
        home.join(codesense_pty_core::agent_cli::version_cache::RUNTIME_LINK),
    )
    .is_ok()
    {
        if let Some(stop) = desktop(AgentCli::Hermes).teardown_home {
            stop(home);
        }
    }
}

pub(crate) fn sweep_managed_runtimes(scopes: &Path) {
    hermes::runtime::sweep(scopes);
}

#[cfg(test)]
pub(crate) use hermes::runtime::tests::Fixture as ManagedHomeFixture;
