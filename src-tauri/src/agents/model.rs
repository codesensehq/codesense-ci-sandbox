// SPDX-License-Identifier: MIT
//! The agent definition: serde types for `agent.yaml` and the save-time
//! validation every write runs.
//!
//! Unknown keys at every level are kept in a `#[serde(flatten)]` catch-all so a
//! newer build's fields survive an edit made by an older one. They are carried,
//! never applied: a hand-written `account:` key does nothing, because which
//! provider account a launch uses is each user's own local binding
//! ([`super::binding`]), never part of the shared definition.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::agents::adapter::is_escalating_flag;
use codesense_host_proto::reverse_mcp::{
    is_env_name, is_env_value, MAX_AGENT_ENV_NAME, MAX_AGENT_ENV_VALUE, MAX_AGENT_ENV_VARS,
};

use super::context::{normalize_entries, ContextEntry, ContextError};
use super::paths::{StartupPath, StartupPathError};

/// The MCP server key of the CodeSense bridge, which every agent always has.
pub const BRIDGE_SERVER: &str = "codesense";

use codesense_pty_core::agent_cli::AgentCli;

/// How a launch runs the agent unless the launcher or routine overrides it.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, rmcp::schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LaunchMode {
    #[default]
    Foreground,
    Headless,
}

/// Which MCP servers the agent can reach, by `.mcp.json` server key. The
/// CodeSense bridge is always present and never listed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
pub struct McpAccess {
    #[serde(default)]
    pub servers: Vec<String>,
    /// Codex bridge tool names, unqualified and exact. Omitted/null allows all;
    /// an empty list allows none. A by-name call through `codesense_call_tool`
    /// requires both the wrapper and its target in this list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_tools: Option<Vec<String>>,
    #[serde(flatten)]
    #[schemars(with = "BTreeMap<String, serde_json::Value>")]
    pub extra: BTreeMap<String, serde_yaml::Value>,
}

/// Claude's own `permissions` settings, kept verbatim: CodeSense never parses a
/// rule or a mode, the CLI enforces them. Every key Claude documents for that
/// block is accepted, under its snake_case name or Claude's own camelCase one
/// (`defaultMode`), so a block pasted from a Claude settings file works as is.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
pub struct ClaudePermissions {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ask: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<String>,
    #[serde(
        default,
        alias = "additionalDirectories",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub additional_directories: Vec<String>,
    #[serde(
        default,
        alias = "defaultMode",
        skip_serializing_if = "Option::is_none"
    )]
    pub default_mode: Option<String>,
    #[serde(
        default,
        alias = "disableBypassPermissionsMode",
        skip_serializing_if = "Option::is_none"
    )]
    pub disable_bypass_permissions_mode: Option<String>,
    #[serde(flatten)]
    #[schemars(with = "BTreeMap<String, serde_json::Value>")]
    pub extra: BTreeMap<String, serde_yaml::Value>,
}

/// Codex's own approval and sandbox settings, kept verbatim, under their
/// snake_case `config.toml` names or camelCase spellings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
pub struct CodexPermissions {
    #[serde(
        default,
        alias = "approvalPolicy",
        skip_serializing_if = "Option::is_none"
    )]
    pub approval_policy: Option<String>,
    #[serde(
        default,
        alias = "sandboxMode",
        skip_serializing_if = "Option::is_none"
    )]
    pub sandbox_mode: Option<String>,
    #[serde(flatten)]
    #[schemars(with = "BTreeMap<String, serde_json::Value>")]
    pub extra: BTreeMap<String, serde_yaml::Value>,
}

/// OpenCode's permission rules, keyed by tool pattern and kept verbatim. The launch
/// composer adds a rule only where the agent has set none for that key: a foreground
/// launch falls back to `"*": "ask"`, a headless one to `"*": "deny"` with the read-only
/// tools, CodeSense's tools and `external_directory: deny` allowed or set, and an
/// escalating agent (`--auto` or an all-allow map) gets `"*": "allow"` headless instead.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
pub struct OpenCodePermissions {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub permission: BTreeMap<String, String>,
    #[serde(flatten)]
    #[schemars(with = "BTreeMap<String, serde_json::Value>")]
    pub extra: BTreeMap<String, serde_yaml::Value>,
}

/// Hermes approval mode and native pre_tool_call deny expressions.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
pub struct HermesPermissions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toolsets: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<String>,
    #[serde(flatten)]
    #[schemars(with = "BTreeMap<String, serde_json::Value>")]
    pub extra: BTreeMap<String, serde_yaml::Value>,
}

/// The runtime's native permission config, keyed by runtime. Only the block
/// for the agent's own runtime applies; the other one is carried untouched so
/// switching runtime loses nothing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
pub struct Permissions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<ClaudePermissions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex: Option<CodexPermissions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opencode: Option<OpenCodePermissions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hermes: Option<HermesPermissions>,
    #[serde(flatten)]
    #[schemars(with = "BTreeMap<String, serde_json::Value>")]
    pub extra: BTreeMap<String, serde_yaml::Value>,
}

/// One agent definition, exactly as stored in `agent.yaml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
pub struct Agent {
    /// Immutable once created.
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// The local user id that created it. Attribution only, never a permission:
    /// any teammate may edit a team agent.
    #[serde(default)]
    pub created_by: String,
    #[schemars(schema_with = "runtime_schema")]
    pub runtime: AgentCli,
    /// Appended to the runtime's argv; never a raw command line.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Environment variables every launch sets on the runtime's process, by
    /// name. CodeSense's own launch variables are always CodeSense's, and any
    /// other variable CodeSense sets for the launch wins over one here of the
    /// same name. Every value is a string. Anyone who can read the agent sees
    /// them: not for secrets.
    #[serde(default, skip_serializing_if = "AgentEnv::is_empty")]
    #[schemars(with = "BTreeMap<String, String>")]
    pub env: AgentEnv,
    #[serde(default)]
    pub launch: LaunchMode,
    #[serde(default)]
    pub mcp: McpAccess,
    /// Ordered content-relative paths, stored normalized by every save. Kept as
    /// written on read so one bad hand edit is a typed validation error, not
    /// an unreadable definition.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub startup: Vec<String>,
    /// The bulk content sets a workspace gets for this agent.
    /// Absent means the runtime's default, resolved at launch and never
    /// written back ([`super::context::default_context`]); `[]` means none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "context_schema")]
    pub context: Option<Vec<ContextEntry>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<Permissions>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub default_prompt: String,
    /// The face its sessions wear when a launch names none: `pack:id`, or a
    /// colour or emoji. Display only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// A routine may wake a running session of this
    /// agent even while it skips permission checks (bypass or unattended).
    /// Off by default; it means nothing for an agent that keeps its checks.
    #[serde(
        default,
        rename = "allowWake",
        alias = "allow_wake",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub allow_wake: bool,
    /// After a terminal server update ends its session, the
    /// session is relaunched as this same Agent, in its workspace and tab,
    /// with a resume brief CodeSense builds from what it knows (never a CLI
    /// session id). Off by default.
    #[serde(
        default,
        rename = "resumeAfterRestart",
        alias = "resume_after_restart",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub resume_after_restart: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_usd: Option<crate::session_usage::spend_caps::Budget>,
    #[serde(default)]
    pub revision: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    #[serde(flatten)]
    #[schemars(with = "BTreeMap<String, serde_json::Value>")]
    pub extra: BTreeMap<String, serde_yaml::Value>,
}

/// Why a definition cannot be saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    RuntimeUnavailable(AgentCli),
    InvalidId(String),
    EmptyName,
    InvalidArg(String),
    /// More `env` variables than a launch carries.
    TooManyEnvVars(usize),
    InvalidEnvName(String),
    /// The value of the named `env` variable is too long or holds a NUL.
    InvalidEnvValue(String),
    EngineTelemetryEnv(String),
    /// The value of the named `env` variable was written as something other than a string.
    EnvValueNotString(String),
    InvalidMcpServer(String),
    DuplicateMcpServer(String),
    /// The bridge is implicit; listing it is redundant.
    BridgeServerListed,
    InvalidBridgeTool(String),
    DuplicateBridgeTool(String),
    BridgeToolsUnsupported,
    InvalidStartupPath(StartupPathError),
    DuplicateStartupPath(String),
    /// `path` lies inside (or contains) the earlier entry `covered_by`, so its
    /// files would belong to two entries.
    OverlappingStartupPath {
        path: String,
        covered_by: String,
    },
    /// The agent's own runtime block carries a key that runtime does not have.
    UnknownPermissionKey {
        runtime: AgentCli,
        key: String,
    },
    InvalidPermission(String),
    DuplicatePermission(String),
    InvalidRevision,
    InvalidBudget(String),
    /// A `context` entry is not valid.
    InvalidContext(ContextError),
    /// A per-repo context override is not valid.
    InvalidRepoOverride(ContextError),
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RuntimeUnavailable(runtime) => write!(f, "{runtime} is not available yet"),
            Self::InvalidBudget(why) => f.write_str(why),
            Self::InvalidId(id) => write!(
                f,
                "agent id {id:?} must be 1-64 lowercase letters, digits or '-', starting with a letter or digit"
            ),
            Self::EmptyName => write!(f, "an agent needs a name"),
            Self::InvalidArg(a) => write!(
                f,
                "argument {a:?} is not allowed: args are separate argv words, not a command line"
            ),
            Self::TooManyEnvVars(n) => write!(
                f,
                "env has {n} variables; an agent sets at most {MAX_AGENT_ENV_VARS}"
            ),
            Self::InvalidEnvName(name) => write!(
                f,
                "env name {name:?} is not valid: letters, digits and '_', not starting with a digit, at most {MAX_AGENT_ENV_NAME} bytes"
            ),
            Self::EngineTelemetryEnv(name) => write!(f, "env.{name} is engine-owned usage telemetry; remove this variable so CodeSense can measure spend"),
            Self::InvalidEnvValue(name) => write!(
                f,
                "the value of env {name:?} is not valid: at most {MAX_AGENT_ENV_VALUE} bytes, with no NUL"
            ),
            Self::EnvValueNotString(name) => write!(
                f,
                "the value of env {name:?} is not a string: quote it, e.g. {name}: \"1\""
            ),
            Self::InvalidMcpServer(s) => write!(f, "MCP server key {s:?} is not valid"),
            Self::DuplicateMcpServer(s) => write!(f, "MCP server {s:?} is listed twice"),
            Self::BridgeServerListed => write!(
                f,
                "the {BRIDGE_SERVER:?} bridge is always available; remove it from mcp.servers"
            ),
            Self::InvalidBridgeTool(name) => write!(f, "bridge tool {name:?} must be an unqualified exact name of 1-128 ASCII letters, digits, '_' or '-'"),
            Self::DuplicateBridgeTool(name) => write!(f, "bridge tool {name:?} is listed twice"),
            Self::BridgeToolsUnsupported => write!(
                f,
                "mcp.bridge_tools is supported only for {} agents",
                bridge_tool_runtimes()
            ),
            Self::InvalidStartupPath(e) => write!(f, "{e}"),
            Self::DuplicateStartupPath(p) => write!(f, "startup path {p:?} is listed twice"),
            Self::OverlappingStartupPath { path, covered_by } => write!(
                f,
                "startup path {path:?} overlaps the earlier entry {covered_by:?}; list one of them"
            ),
            Self::UnknownPermissionKey { runtime, key } => {
                write!(f, "permissions.{runtime} has no key {key:?}")
            }
            Self::InvalidPermission(v) => write!(
                f,
                "permission value {v:?} is not valid: values must be non-empty with no NUL"
            ),
            Self::DuplicatePermission(v) => write!(f, "permission rule {v:?} is listed twice"),
            Self::InvalidRevision => write!(f, "a saved agent has a revision of at least 1"),
            Self::InvalidContext(e) | Self::InvalidRepoOverride(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ValidationError {}

/// `[a-z0-9][a-z0-9-]{0,63}`: ids become directory names.
pub fn is_valid_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    (1..=64).contains(&bytes.len())
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// The runtimes whose adapters enforce `mcp.bridge_tools`, by display label.
pub(crate) fn bridge_tool_runtimes() -> String {
    AgentCli::join_names(
        AgentCli::ALL
            .into_iter()
            .map(crate::agents::adapter::desktop)
            .filter(|desktop| desktop.enforces_bridge_tool_allowlist)
            .map(|desktop| desktop.starter.name),
    )
}

/// A word made only of shell syntax (`|`, `&&`, `;`, `>` …) or whitespace:
/// a sign someone pasted a command line into `args`.
fn is_shell_only(arg: &str) -> bool {
    arg.chars()
        .all(|c| c.is_whitespace() || "|&;<>()$`\\\"'!*".contains(c))
}

/// The value `args` gives one of `names`, as `--flag value` or `--flag=value`.
/// The last occurrence wins, as it does for the CLIs' own parsers.
pub(crate) fn flag_value<'a>(args: &'a [String], names: &[&str]) -> Option<&'a str> {
    let mut found = None;
    for (i, arg) in args.iter().enumerate() {
        for name in names {
            if arg == name {
                if let Some(value) = args.get(i + 1) {
                    found = Some(value.as_str());
                }
            } else if let Some(value) = arg
                .strip_prefix(name)
                .and_then(|rest| rest.strip_prefix('='))
            {
                found = Some(value);
            }
        }
    }
    found
}

/// A saved agent's `env`: each variable's value by name. A value written by hand as anything but
/// a string (a number, a boolean, a null) is kept aside by name as written, never read as its text,
/// so the definition reads, fails validation naming the variable, and a revision written without
/// validation (archiving) carries it unchanged rather than dropping it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentEnv {
    vars: BTreeMap<String, String>,
    not_strings: BTreeMap<String, serde_yaml::Value>,
}

impl AgentEnv {
    /// No variables, and none written as a non-string.
    pub fn is_empty(&self) -> bool {
        self.vars.is_empty() && self.not_strings.is_empty()
    }

    /// The variables whose values are strings.
    pub fn vars(&self) -> &BTreeMap<String, String> {
        &self.vars
    }

    /// The first variable whose value was written as something other than a string.
    pub fn first_not_string(&self) -> Option<&str> {
        self.not_strings.keys().next().map(String::as_str)
    }
}

impl std::ops::Deref for AgentEnv {
    type Target = BTreeMap<String, String>;
    fn deref(&self) -> &Self::Target {
        &self.vars
    }
}

impl From<BTreeMap<String, String>> for AgentEnv {
    fn from(vars: BTreeMap<String, String>) -> Self {
        Self {
            vars,
            not_strings: BTreeMap::new(),
        }
    }
}

impl FromIterator<(String, String)> for AgentEnv {
    fn from_iter<I: IntoIterator<Item = (String, String)>>(iter: I) -> Self {
        BTreeMap::from_iter(iter).into()
    }
}

impl PartialEq<BTreeMap<String, String>> for AgentEnv {
    fn eq(&self, other: &BTreeMap<String, String>) -> bool {
        self.not_strings.is_empty() && self.vars == *other
    }
}

impl PartialEq<AgentEnv> for BTreeMap<String, String> {
    fn eq(&self, other: &AgentEnv) -> bool {
        other == self
    }
}

impl Serialize for AgentEnv {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.vars.len() + self.not_strings.len()))?;
        for (name, value) in &self.vars {
            map.serialize_entry(name, value)?;
        }
        for (name, value) in &self.not_strings {
            map.serialize_entry(name, value)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for AgentEnv {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Value {
            Text(String),
            Other(serde_yaml::Value),
        }
        let mut env = Self::default();
        for (name, value) in BTreeMap::<String, Value>::deserialize(deserializer)? {
            match value {
                Value::Text(text) => {
                    env.vars.insert(name, text);
                }
                Value::Other(value) => {
                    env.not_strings.insert(name, value);
                }
            }
        }
        Ok(env)
    }
}

/// Refuses an `env` map a launch cannot carry: too many variables, a name
/// that is not a portable variable name, or a value too long or holding a NUL.
pub(crate) fn check_env(env: &BTreeMap<String, String>) -> Result<(), ValidationError> {
    if env.len() > MAX_AGENT_ENV_VARS {
        return Err(ValidationError::TooManyEnvVars(env.len()));
    }
    for (name, value) in env {
        if codesense_pty_core::spawn::telemetry_owned_env(name) {
            return Err(ValidationError::EngineTelemetryEnv(name.clone()));
        }
        if !is_env_name(name) {
            return Err(ValidationError::InvalidEnvName(name.clone()));
        }
        if !is_env_value(value) {
            return Err(ValidationError::InvalidEnvValue(name.clone()));
        }
    }
    Ok(())
}

fn valid_value(value: &str) -> bool {
    !value.is_empty() && !value.contains('\0')
}

/// Refuses an empty permission value, or one holding a NUL.
pub(crate) fn check_permission(value: &str) -> Result<(), ValidationError> {
    if valid_value(value) {
        Ok(())
    } else {
        Err(ValidationError::InvalidPermission(value.to_string()))
    }
}

/// Replaces a CLI's permission block with `block` (`null` removes it).
pub(crate) fn set_permission_block<T: serde::de::DeserializeOwned>(
    slot: &mut Option<T>,
    block: serde_json::Value,
) -> Result<(), serde_json::Error> {
    *slot = match block {
        serde_json::Value::Null => None,
        block => Some(serde_json::from_value(block)?),
    };
    Ok(())
}

/// Refuses a `runtime` permission block holding a key it does not know (`extra`).
pub(crate) fn known_permission_keys(
    runtime: AgentCli,
    extra: &BTreeMap<String, serde_yaml::Value>,
) -> Result<(), ValidationError> {
    extra.keys().next().map_or(Ok(()), |key| {
        Err(ValidationError::UnknownPermissionKey {
            runtime,
            key: key.clone(),
        })
    })
}

impl Agent {
    /// Optional native non-secret settings, persisted by the existing per-agent extension map.
    pub fn startup_settings(&self) -> Option<serde_json::Value> {
        self.extra
            .get("startup_settings")
            .and_then(|settings| std::result::Result::ok(serde_json::to_value(settings)))
    }

    pub fn set_startup_settings(&mut self, settings: Option<BTreeMap<String, serde_json::Value>>) {
        match settings {
            Some(settings) => {
                self.extra.insert(
                    "startup_settings".into(),
                    serde_yaml::to_value(settings).expect("JSON settings serialize to YAML"),
                );
            }
            None => {
                self.extra.remove("startup_settings");
            }
        }
    }

    pub fn is_archived(&self) -> bool {
        self.archived_at.is_some()
    }

    /// The startup entries, parsed and normalized, in order.
    pub fn startup_paths(&self) -> Result<Vec<StartupPath>, ValidationError> {
        let mut out: Vec<StartupPath> = Vec::with_capacity(self.startup.len());
        for raw in &self.startup {
            let path = StartupPath::parse(raw).map_err(ValidationError::InvalidStartupPath)?;
            for earlier in &out {
                if *earlier == path {
                    return Err(ValidationError::DuplicateStartupPath(path.to_string()));
                }
                if earlier.contains(&path) || path.contains(earlier) {
                    return Err(ValidationError::OverlappingStartupPath {
                        path: path.to_string(),
                        covered_by: earlier.to_string(),
                    });
                }
            }
            out.push(path);
        }
        Ok(out)
    }

    /// Rewrite `startup` and `context` in normalized form, the identity every
    /// consumer keys on. Refuses exactly what [`Self::validate`] refuses about
    /// either.
    pub fn normalize_startup(&mut self) -> Result<(), ValidationError> {
        self.startup = self
            .startup_paths()?
            .into_iter()
            .map(|p| p.to_string())
            .collect();
        if let Some(context) = &self.context {
            self.context =
                Some(normalize_entries(context).map_err(ValidationError::InvalidContext)?);
        }
        Ok(())
    }

    /// Whether a launch of this agent auto-approves all tool use: an escalating
    /// flag in `args`, or a permission mode that bypasses approval, set either
    /// by a CLI flag in `args` (which the CLI lets win) or by the agent's own
    /// permissions. Shown, never refused.
    pub fn is_escalating(&self) -> bool {
        let args = self.args.as_slice();
        if args.iter().any(|a| is_escalating_flag(a)) {
            return true;
        }
        (super::adapter::desktop(self.runtime).escalates)(self)
    }

    /// Everything a save checks.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if !<[AgentCli]>::contains(&AgentCli::ALL, &self.runtime) {
            return Err(ValidationError::RuntimeUnavailable(self.runtime));
        }
        if let Some(cap) = self.budget_usd {
            cap.validate().map_err(ValidationError::InvalidBudget)?;
        }
        if !is_valid_id(&self.id) {
            return Err(ValidationError::InvalidId(self.id.clone()));
        }
        if self.name.trim().is_empty() {
            return Err(ValidationError::EmptyName);
        }
        if self.revision == 0 {
            return Err(ValidationError::InvalidRevision);
        }
        if let Some(arg) = self
            .args
            .iter()
            .find(|a| a.is_empty() || a.contains('\0') || is_shell_only(a))
        {
            return Err(ValidationError::InvalidArg(arg.clone()));
        }
        if let Some(name) = self.env.first_not_string() {
            return Err(ValidationError::EnvValueNotString(name.to_string()));
        }
        check_env(&self.env)?;
        let mut servers = BTreeSet::new();
        for server in &self.mcp.servers {
            if server.trim().is_empty() || server.contains('\0') {
                return Err(ValidationError::InvalidMcpServer(server.clone()));
            }
            if server == BRIDGE_SERVER {
                return Err(ValidationError::BridgeServerListed);
            }
            if !servers.insert(server.as_str()) {
                return Err(ValidationError::DuplicateMcpServer(server.clone()));
            }
        }
        self.startup_paths()?;
        if let Some(tools) = &self.mcp.bridge_tools {
            let desktop = crate::agents::adapter::desktop(self.runtime);
            if !desktop.enforces_bridge_tool_allowlist && desktop.home_config.is_none() {
                return Err(ValidationError::BridgeToolsUnsupported);
            }
            let mut seen = BTreeSet::new();
            for name in tools {
                if name.is_empty()
                    || name.len() > 128
                    || name.starts_with("mcp__")
                    || !name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
                {
                    return Err(ValidationError::InvalidBridgeTool(name.clone()));
                }
                if !seen.insert(name) {
                    return Err(ValidationError::DuplicateBridgeTool(name.clone()));
                }
            }
        }
        if let Some(context) = &self.context {
            normalize_entries(context).map_err(ValidationError::InvalidContext)?;
        }
        self.validate_permissions()
    }

    /// Only the block of the agent's own runtime applies; any other is carried untouched.
    fn validate_permissions(&self) -> Result<(), ValidationError> {
        self.permissions.as_ref().map_or(Ok(()), |permissions| {
            (crate::agents::adapter::desktop(self.runtime)
                .permissions
                .validate)(permissions)
        })
    }
}

// These types use custom serde implementations; describe their YAML input shapes.
fn runtime_schema(_: &mut rmcp::schemars::SchemaGenerator) -> rmcp::schemars::Schema {
    rmcp::schemars::json_schema!({"type":"string", "enum": AgentCli::ALL.map(|runtime| runtime.as_str())})
}
fn context_schema(_: &mut rmcp::schemars::SchemaGenerator) -> rmcp::schemars::Schema {
    rmcp::schemars::json_schema!({"anyOf":[{"type":"null"},{"type":"array","items":{"anyOf":[
        {"type":"string"}, {"type":"object","required":["path"],"properties":{
            "path":{"type":"string"},"dest":{"type":["string","null"]}},"additionalProperties":true}
    ]}}]})
}
