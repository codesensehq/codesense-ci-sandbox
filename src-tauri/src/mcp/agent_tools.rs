// SPDX-License-Identifier: MIT
//! The agent MCP tools: their parameters, wire shapes and bodies.
//!
//! The `#[tool]` methods in [`super::handler`] only work out who is calling
//! (the local principal, the calling session's launch, the target team) and run
//! one of the bodies here inside `await_blocking`. Every body is a thin adapter
//! over [`crate::agents::service`] and [`crate::agents::resolve`]: it turns the
//! agent's words into a definition or a key, calls the service, and shapes the
//! result as `{ok: true, data}` or `{ok: false, error: {code, message, …}}`,
//! with snake_case keys and RFC3339 times.
//!
//! None of these tools tells anyone anything, and none writes anything but the
//! agent's own definition under `config/agents/`: an edit is a local change to
//! the context repo, and sharing it is the user's own git push.

use std::collections::BTreeMap;
use std::path::Path;

use rmcp::schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};

use crate::agents::adapter::ResolvedLaunchProfile;
use crate::agents::context::{parse_repo, repo_label, ContextEntry, RepoOverride};
use crate::agents::launch_check::LaunchWarning;
use crate::agents::manifest::AgentRevisionRef;
use crate::agents::manifest_resolve::{ContextRequest, ContextResolution, ManifestTarget};
use crate::agents::model::{LaunchMode, McpAccess, Permissions};
use crate::agents::provides::AgentProvides;
use crate::agents::resolve::{AccountSource, AgentAccountChoice};
use crate::agents::roots;
use crate::agents::service::{self, AgentReferrers};
use crate::agents::{
    Agent, AgentError, AgentKey, AgentScope, Placement, ResolveError, ResolveOptions,
    ResolvedAgent, SaveTarget, StartupManifest, ValidationError,
};
use crate::domain::{Principal, SourceRepoSlug};
use crate::install::agent_account_store::{
    AccountResolutionAvailability, AccountResolutionReason, AccountResolutionSource, AgentProvider,
};
use crate::routines::Author;
use codesense_pty_core::agent_cli::AgentCli;

use super::routine_tools::CallerSession;
use super::tool_envelope::ToolEnvelope;

/// What a body returns: a reply the agent branches on, or a hard failure (an
/// unreadable store, a failed write) that surfaces as an MCP error.
pub(crate) type Reply<T> = Result<AgentReply<T>, String>;

/// Why a call changed nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RefusalCode {
    /// An input is wrong; `field` names it when one input is to blame.
    Invalid,
    /// The agent changed since `baseRevision`; re-read and reapply.
    Conflict,
    /// No such agent (or revision) visible to the caller.
    NotFound,
    /// Something still references the agent; archive it instead.
    InUse,
    /// A starter agent the app ships; copy it to change it.
    ReadOnly,
    /// The change takes the agent's routines with it: call again with the
    /// flag the message names once the user agreed (`referrers` lists them).
    NeedsConfirm,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Refusal {
    pub code: RefusalCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_revision: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub referrers: Option<Vec<String>>,
}

impl Refusal {
    fn invalid(field: Option<&str>, message: impl Into<String>) -> Self {
        Self {
            code: RefusalCode::Invalid,
            message: message.into(),
            field: field.map(str::to_string),
            current_revision: None,
            referrers: None,
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            code: RefusalCode::NotFound,
            ..Self::invalid(None, message)
        }
    }
}

/// `{ok: true, data}` or `{ok: false, error}`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AgentReply<T> {
    Ok(T),
    Refused(Refusal),
}

impl<T: Serialize> Serialize for AgentReply<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        match self {
            Self::Ok(data) => {
                map.serialize_entry("ok", &true)?;
                map.serialize_entry("data", data)?;
            }
            Self::Refused(error) => {
                map.serialize_entry("ok", &false)?;
                map.serialize_entry("error", error)?;
            }
        }
        map.end()
    }
}

/// Who is calling: the local principal, and what their revisions are
/// credited to and their new agents default to.
#[derive(Debug, Clone)]
pub(crate) struct AgentCaller {
    pub author: Author,
    /// The calling session's own CLI, when its launch recorded which: what a
    /// new agent runs when the caller names no `runtime`. `None` (no session,
    /// or a launch that recorded no account) makes `runtime` required rather
    /// than guessed.
    pub session_runtime: Option<AgentCli>,
}

impl AgentCaller {
    /// `session` is the calling session's launch record, when the caller
    /// passed a session id; without one the caller is the local user.
    pub(crate) fn new(principal: &Principal, session: Option<&CallerSession>) -> Self {
        Self {
            author: match session {
                Some(session) => session.author(),
                None => Author::user(principal.as_str()),
            },
            session_runtime: session.and_then(|s| s.account_runtime),
        }
    }
}

fn open_object_schema(_: &mut SchemaGenerator) -> Schema {
    rmcp::schemars::json_schema!({
        "type": "object",
        "additionalProperties": true
    })
}

fn nullable_object_schema(_: &mut SchemaGenerator) -> Schema {
    rmcp::schemars::json_schema!({
        "type": ["object", "null"],
        "additionalProperties": true
    })
}

/// Keep an omitted patch field apart from an explicit `null`.
fn nullable_patch<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<serde_json::Value>>, D::Error> {
    Option::<serde_json::Value>::deserialize(deserializer).map(Some)
}

/// One `context` entry as the tools take it: a bare content-relative path
/// (`"docs/architecture"`), or `{path, dest?}` when it lands elsewhere in the
/// workspace. Stored as the object form either way. Any other key is refused
/// by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextEntryInput(pub ContextEntry);

impl<'de> Deserialize<'de> for ContextEntryInput {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer
            .deserialize_any(crate::agents::context::EntryVisitor { strict: true })
            .map(Self)
    }
}

impl JsonSchema for ContextEntryInput {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ContextEntry".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        rmcp::schemars::json_schema!({
            "anyOf": [
                {
                    "type": "string",
                    "description": "A content-relative file or directory, e.g. `docs` or `.claude/skills`."
                },
                {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "A content-relative file or directory."
                        },
                        "dest": {
                            "type": "string",
                            "description": "Where the prefix lands in the workspace instead, e.g. `plans`."
                        }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            ]
        })
    }
}

impl From<ContextEntryInput> for ContextEntry {
    fn from(e: ContextEntryInput) -> Self {
        e.0
    }
}

/// One per-repo override as the tools take it. Naming a repo replaces its
/// override; one with neither `add` nor `remove` removes it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepoOverrideInput {
    /// `host/owner/repo`, or `owner/repo` (always github.com); a clone URL
    /// also works.
    pub repo: String,
    /// Extra context sets this repo's workspace gets.
    #[serde(default)]
    pub add: Vec<ContextEntryInput>,
    /// Globs over content paths this repo's workspace never gets, e.g.
    /// `.claude/skills/legacy-*`.
    #[serde(default)]
    pub remove: Vec<String>,
}

/// A stored per-repo override on the wire: `add` as `{path, dest?}` only, so
/// it can be passed back to agent_update as it is (keys a newer build stored
/// are kept there by themselves).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RepoOverrideItem {
    /// `host/owner/repo`.
    pub repo: String,
    pub add: Vec<ContextEntry>,
    pub remove: Vec<String>,
}

impl RepoOverrideItem {
    /// Every override with an `add` or a `remove`. One holding only keys a
    /// newer build stored is left out: listed as `{repo}` and passed back to
    /// agent_update, it would remove that override.
    fn list(all: crate::agents::defs::RepoOverrides) -> Vec<Self> {
        all.into_iter()
            .filter(|(_, o)| !(o.add.is_empty() && o.remove.is_empty()))
            .map(|(repo, o)| Self {
                repo,
                add: wire_context(&o.add),
                remove: o.remove,
            })
            .collect()
    }
}

/// Keep an omitted `context` apart from an explicit `null` (back to the
/// runtime default).
fn nullable_context<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<Vec<ContextEntryInput>>>, D::Error> {
    Option::<Vec<ContextEntryInput>>::deserialize(deserializer).map(Some)
}

/// Keep an omitted `env` apart from an explicit `null` (clears it, like `{}`).
fn nullable_env<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<BTreeMap<String, serde_json::Value>>>, D::Error> {
    Option::<BTreeMap<String, serde_json::Value>>::deserialize(deserializer).map(Some)
}

/// `env` as given, or the refusal of the first value that is not a string.
fn env_strings(
    env: Option<BTreeMap<String, serde_json::Value>>,
) -> Result<crate::agents::model::AgentEnv, Refusal> {
    env.unwrap_or_default()
        .into_iter()
        .map(|(name, value)| match value {
            serde_json::Value::String(value) => Ok((name, value)),
            _ => Err(Refusal::invalid(
                Some("env"),
                format!("the value of env {name:?} is not a string: pass it as one, e.g. \"1\""),
            )),
        })
        .collect()
}

/// An agent as `agent_create` and `agent_preview` take it.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentDraft {
    /// What the agent is called, e.g. "Slack triage".
    #[serde(default)]
    pub name: String,
    /// The agent id within its team: 1-64 of `a-z`, `0-9`, `-`. Default:
    /// derived from `name`. It never changes once created.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// `claude`, `codex` or `opencode`. Required unless CodeSense launched you and you
    /// pass your `sessionId`, when it defaults to your own CLI.
    #[serde(default)]
    pub runtime: Option<String>,
    /// Extra argv words for the CLI, each its own string (never a command line).
    #[serde(default)]
    pub args: Option<Vec<String>>,
    /// Non-secret native settings saved with this agent; no global model or credential store.
    #[serde(default, alias = "startup_settings")]
    pub startup_settings: Option<BTreeMap<String, serde_json::Value>>,
    /// Environment variables every launch sets on the CLI's process, by name:
    /// `{"CLAUDE_CODE_DISABLE_AUTO_MEMORY": "1"}`. Use it for what the CLI only
    /// reads from its environment, e.g. to run the agent on its own context
    /// alone. Values are strings; names are letters, digits and `_`, not
    /// starting with a digit; at most 64 variables, each value at most 4096
    /// bytes. CodeSense's own launch variables (its session id, API port, MCP
    /// config, activity bindings, `PATH` and `SHELL`) are always CodeSense's,
    /// and any other variable CodeSense sets for the launch (account, MCP
    /// scope, inbox) wins over one here of the same name. Visible to anyone
    /// who can read the agent (stored in agent.yaml); not for secrets. Default
    /// none.
    #[serde(default)]
    #[schemars(with = "Option<BTreeMap<String, String>>")]
    pub env: Option<BTreeMap<String, serde_json::Value>>,
    /// `foreground` (default) or `headless`. A headless Codex agent with no
    /// policy gets approval_policy never + sandbox_mode workspace-write;
    /// no bypass flag is ever added for you.
    #[serde(default)]
    pub launch: Option<String>,
    /// MCP server keys from the team's MCP config (`.mcp.json` /
    /// `.codex/config.toml`). Default none beyond the CodeSense bridge, which is
    /// always there.
    #[serde(default, alias = "mcp_servers")]
    pub mcp_servers: Option<Vec<String>>,
    /// Codex bridge tool names (unqualified, exact); null/omitted grants all,
    /// [] grants none. Local Codex only; remote preparation refuses explicit lists.
    /// The generated CodeSense alias always auto-approves, including on-request
    /// and untrusted agents. Shell/filesystem sandbox settings and other MCP
    /// servers are unchanged. Native enabled_tools and the captured relay grant
    /// both enforce this list; codesense_call_tool needs wrapper and target enabled.
    /// Unknown names warn and grant nothing; excluded signal_done/read_messages/reply
    /// warn. The bridge runs in CodeSense outside the CLI shell/filesystem sandbox;
    /// permitted bridge tools can mutate CodeSense data.
    #[serde(
        default,
        alias = "mcp_bridge_tools",
        deserialize_with = "crate::mcp::routine_tools::deserialize_patch"
    )]
    pub mcp_bridge_tools: Option<Option<Vec<String>>>,
    /// Ordered plain content-relative paths the agent starts with (default none: only
    /// what CodeSense itself needs): each a file, or a directory that brings
    /// everything under it, e.g. `CLAUDE.md`, `.claude/skills/bmad-build/`,
    /// `.claude/commands/bmad-help.md`; never `skill:`/`command:` keys, absolute
    /// paths, `..`, globs, reserved top-level dirs such as `.git`, duplicates or
    /// an entry inside another. A file may list more content paths it needs in
    /// `requires:` frontmatter, and they come along. A path no content layer has
    /// is a manifest error, except the runtime's root instructions file
    /// (`CLAUDE.md` / `AGENTS.md`), skipped where the team has none.
    #[serde(default)]
    pub startup: Option<Vec<String>>,
    /// The bulk content sets a workspace gets for the agent: `[{path,
    /// dest?}]` (or bare paths), at most 32 relative prefixes, `dest` remapping
    /// one. Omitted or `null`: the runtime's default (claude: `CLAUDE.md` +
    /// `.claude/{commands,hooks,agents,skills}`; codex: `AGENTS.md` +
    /// `.agents/skills`); `[]`: none. A missing prefix is a warning, never an
    /// error; anything else is fetched at run time with hydrate_worktree.
    #[serde(default)]
    pub context: Option<Vec<ContextEntryInput>>,
    /// Per-repo overrides of `context`: `[{repo: "owner/repo", add?: [path or
    /// {path, dest?}], remove?: [globs]}]`, stored beside the agent as
    /// `config/agents/<id>/repos/<host>/<owner>/<repo>.yaml` and part of its
    /// revision. A `dest` may not be VCS data, a managed file such as
    /// `.claude/settings.json`, or collide with another entry's.
    #[serde(default, alias = "repo_overrides")]
    pub repo_overrides: Option<Vec<RepoOverrideInput>>,
    /// The runtime's own permission config, passed to the CLI verbatim (default
    /// none: the CLI's own defaults): `{claude: {allow, ask, deny,
    /// additionalDirectories, defaultMode, disableBypassPermissionsMode}}` with
    /// Claude's own rule strings, or `{codex: {approvalPolicy, sandboxMode}}`.
    #[serde(default)]
    #[schemars(schema_with = "open_object_schema")]
    pub permissions: Option<serde_json::Value>,
    /// The prompt a launch starts with when the launcher gives none.
    #[serde(default, alias = "default_prompt")]
    pub default_prompt: Option<String>,
    /// The face its sessions wear in the agents list when a launch names none:
    /// `pack:id` from `avatar_list` (e.g. `builtin-funny:hardhat-raccoon`), or
    /// a colour or emoji. Display only.
    #[serde(default)]
    pub avatar: Option<String>,
    /// Default team queue to join on launch; null means no role.
    #[serde(default)]
    pub role: Option<String>,
    /// `true` lets a routine wake a running session of this agent even while it
    /// skips permission checks (bypass or unattended). Default false.
    #[serde(default, alias = "allow_wake")]
    pub allow_wake: Option<bool>,
    /// `true` relaunches its sessions as this Agent, with a resume brief,
    /// after a terminal server update ends them. Default false.
    #[serde(default, alias = "resume_after_restart")]
    pub resume_after_restart: Option<bool>,
    #[serde(
        default,
        deserialize_with = "crate::mcp::routine_tools::deserialize_patch"
    )]
    pub budget_usd: Option<Option<crate::session_usage::spend_caps::BudgetUsd>>,
    /// Anything else, refused by name so a misspelt or nested key (such as
    /// `mcp: {servers}` copied from a definition) is never silently dropped.
    #[serde(flatten)]
    #[schemars(skip)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

/// `name` is required to create (the draft type leaves it optional only so
/// `agent_preview {agent}` can omit it).
fn require_name(schema: &mut Schema) {
    if let Some(object) = schema.as_object_mut() {
        let required = object
            .entry("required")
            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
        if let Some(list) = required.as_array_mut() {
            if !list.iter().any(|v| v == "name") {
                list.push(serde_json::Value::from("name"));
            }
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(transform = require_name)]
pub struct AgentCreateParams {
    #[serde(flatten)]
    pub draft: AgentDraft,
    /// `team` (default: in `org`+`team`, else the active team, where any
    /// teammate can use and edit it) or `personal` (Only me, only when the user
    /// asks for it: filed in your private space for `org`, which is then
    /// required and must be an org synced on this Mac).
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub org: Option<String>,
    #[serde(default)]
    pub team: Option<String>,
    /// Check everything and return the reply a create would, writing nothing.
    #[serde(default, alias = "dry_run")]
    pub dry_run: bool,
    /// Omit to use the verified connection identity. A supplied id must name that caller.
    #[serde(default, alias = "session_id")]
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentPreviewParams {
    /// The agent, in exactly the fields `agent_create` takes.
    #[serde(flatten)]
    pub draft: AgentDraft,
    /// Preview this saved agent (its `id` from agent_list) instead of a draft.
    #[serde(default)]
    pub agent: Option<String>,
    /// The repo whose workspace to resolve `context` for (`owner/repo` or
    /// `host/owner/repo`): its per-repo tier and override apply.
    #[serde(default)]
    pub repo: Option<String>,
    /// Existing local directory selected for launch, to preview project-document warnings.
    /// Omit when no local target exists yet or the launch runs on another machine.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Resolve the team-workspace default instead of the runtime's.
    #[serde(default, alias = "team_workspace")]
    pub team_workspace: bool,
    /// Where it would be saved: `team` (default) or `personal` (Only me; needs `org`).
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub org: Option<String>,
    #[serde(default)]
    pub team: Option<String>,
    /// Omit to use the verified connection identity. A supplied id must name that caller.
    #[serde(default, alias = "session_id")]
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentUpdateParams {
    /// The agent's `id` as `agent_list` returns it.
    pub id: String,
    /// The `revision` you last read.
    #[serde(alias = "base_revision")]
    pub base_revision: u32,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(
        default,
        alias = "startup_settings",
        deserialize_with = "crate::mcp::routine_tools::deserialize_patch"
    )]
    pub startup_settings: Option<Option<BTreeMap<String, serde_json::Value>>>,
    /// Replaces the launch environment variables whole; `{}` or `null` clears
    /// them. Values are strings. Visible to anyone who can read the agent
    /// (stored in agent.yaml); not for secrets.
    #[serde(default, deserialize_with = "nullable_env")]
    #[schemars(with = "Option<BTreeMap<String, String>>")]
    pub env: Option<Option<BTreeMap<String, serde_json::Value>>>,
    #[serde(default)]
    pub launch: Option<String>,
    #[serde(default, alias = "mcp_servers")]
    pub mcp_servers: Option<Vec<String>>,
    /// Replaces Codex's bridge tool list. Omit to keep it, null to allow all,
    /// [] to allow none. Local Codex only. Native enabled_tools and the relay grant
    /// enforce it; by-name dispatch needs wrapper and target enabled. Generated
    /// bridge aliases always auto-approve without changing the CLI shell/filesystem
    /// sandbox or other MCP servers. Unknown or excluded required tools warn.
    #[serde(
        default,
        alias = "mcp_bridge_tools",
        deserialize_with = "crate::mcp::routine_tools::deserialize_patch"
    )]
    pub mcp_bridge_tools: Option<Option<Vec<String>>>,
    #[serde(default)]
    pub startup: Option<Vec<String>>,
    /// Replaces `context`; `null` returns it to the runtime default.
    #[serde(default, deserialize_with = "nullable_context")]
    pub context: Option<Option<Vec<ContextEntryInput>>>,
    /// Replaces the override of each repo it names; `{repo}` alone removes it.
    #[serde(default, alias = "repo_overrides")]
    pub repo_overrides: Option<Vec<RepoOverrideInput>>,
    /// Replaces each runtime block it names (`{claude: …}` replaces only the
    /// claude block, `{codex: null}` removes the codex one); `null` removes
    /// every permission.
    #[serde(default, deserialize_with = "nullable_patch")]
    #[schemars(schema_with = "nullable_object_schema")]
    pub permissions: Option<Option<serde_json::Value>>,
    #[serde(default, alias = "default_prompt")]
    pub default_prompt: Option<String>,
    /// The default face (`pack:id`, a colour or an emoji); `""` removes it.
    #[serde(default)]
    pub avatar: Option<String>,
    /// Omit to keep the default queue, null to clear, or pass its stable id.
    #[serde(default, deserialize_with = "crate::queue_roles::nullable_role")]
    pub role: Option<Option<String>>,
    /// `true` lets a routine wake a running session of this agent even while it
    /// skips permission checks; `false` turns that off.
    #[serde(default, alias = "allow_wake")]
    pub allow_wake: Option<bool>,
    /// `true` relaunches its sessions with a resume brief after a terminal
    /// server update; `false` turns that off.
    #[serde(default, alias = "resume_after_restart")]
    pub resume_after_restart: Option<bool>,
    #[serde(
        default,
        deserialize_with = "crate::mcp::routine_tools::deserialize_patch"
    )]
    pub budget_usd: Option<Option<crate::session_usage::spend_caps::BudgetUsd>>,
    /// Omit to use the verified connection identity. A supplied id must name that caller.
    #[serde(default, alias = "session_id")]
    pub session_id: Option<String>,
    /// Anything else, refused by name.
    #[serde(flatten)]
    #[schemars(skip)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentListParams {
    /// `all` (default), `mine` (your Only me agents and the team agents you
    /// created), `team` (team agents only) or `builtin` (the read-only starter
    /// agents CodeSense ships: `builtin/claude`, `builtin/codex`, their unattended twins
    /// `builtin/claude-unattended` and `builtin/codex-unattended`).
    #[serde(default)]
    pub scope: Option<String>,
    /// Include archived agents. Default false.
    #[serde(default, alias = "include_archived")]
    pub include_archived: bool,
    /// Resolve each agent's startup manifest and add its `{files, bytes,
    /// error_count}`. Default false: it reads every startup file.
    #[serde(default, alias = "include_manifest")]
    pub include_manifest: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentGetParams {
    /// The agent's `id` as `agent_list` returns it.
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentDeleteParams {
    /// The agent's `id` as `agent_list` returns it.
    pub id: String,
    /// Remove it and its history for good instead of archiving it. Refused
    /// while anything references it.
    #[serde(default)]
    pub hard: bool,
    /// Archive its routines with it. Required (after the user agreed) when it
    /// has active routines: without it the archive is refused, listing them.
    #[serde(default, alias = "archive_routines")]
    pub archive_routines: bool,
    /// Omit to use the verified connection identity. A supplied id must name that caller.
    #[serde(default, alias = "session_id")]
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentRestoreParams {
    /// The agent's `id` as `agent_list` returns it.
    pub id: String,
    /// The old revision whose definition becomes current again.
    pub revision: u32,
    /// The `revision` you last read.
    #[serde(alias = "base_revision")]
    pub base_revision: u32,
    /// Omit to use the verified connection identity. A supplied id must name that caller.
    #[serde(default, alias = "session_id")]
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentUnarchiveParams {
    /// The agent's `id` as `agent_list` returns it.
    pub id: String,
    /// The `revision` you last read.
    #[serde(alias = "base_revision")]
    pub base_revision: u32,
    /// Also unarchive the routines archiving it archived (listed as
    /// `archived_routines` when this is false). Default false.
    #[serde(default, alias = "restore_routines")]
    pub restore_routines: bool,
    /// Omit to use the verified connection identity. A supplied id must name that caller.
    #[serde(default, alias = "session_id")]
    pub session_id: Option<String>,
}

/// A manifest in three numbers.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ManifestSummary {
    pub files: u64,
    pub bytes: u64,
    pub error_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct AgentListItem {
    #[serde(flatten)]
    pub cap: crate::session_usage::spend_caps::CapStatus,
    pub id: String,
    pub name: String,
    pub description: String,
    pub runtime: AgentCli,
    pub launch: LaunchMode,
    pub mcp_servers: Vec<String>,
    pub startup_count: usize,
    /// The creator's display name. Attribution only.
    pub created_by: String,
    pub scope: AgentScope,
    pub org: String,
    pub team: String,
    pub archived: bool,
    pub revision: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest: Option<ManifestSummary>,
    /// Why the manifest could not be resolved, when it was asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_error: Option<String>,
}

/// Which provider login this user would launch the agent with.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AccountItem {
    /// `binding` (this user bound the agent to an account) or `runtime_default`
    /// (the runtime's active account, or the CLI's own login).
    pub source: &'static str,
    pub bound_account_id: Option<String>,
    pub resolved_source: AccountResolutionSource,
    pub account_id: Option<String>,
    pub provider: Option<AgentProvider>,
    pub label: Option<String>,
    pub reason: AccountResolutionReason,
    pub availability: AccountResolutionAvailability,
    pub spawn_eligible: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store_error: Option<String>,
    /// This user's consent to keep a raw terminal snapshot of interactive launches.
    pub capture_raw_terminal: bool,
}

impl From<&AgentAccountChoice> for AccountItem {
    fn from(choice: &AgentAccountChoice) -> Self {
        let preview = &choice.preview;
        Self {
            source: match choice.source {
                AccountSource::Binding => "binding",
                AccountSource::RuntimeDefault => "runtime_default",
                AccountSource::Chosen => "chosen",
            },
            bound_account_id: choice.override_account_id.clone(),
            resolved_source: preview.source,
            account_id: preview.account_id.clone(),
            provider: preview.provider,
            label: preview.label.clone(),
            reason: preview.reason,
            availability: preview.availability,
            spawn_eligible: preview.spawn_eligible,
            store_error: choice.store_error.clone(),
            capture_raw_terminal: choice.capture_raw_terminal,
        }
    }
}

/// [`ResolvedAgent`] on the wire.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResolvedItem {
    pub agent: AgentRevisionRef,
    pub launch: ResolvedLaunchProfile,
    pub account: AccountItem,
    pub manifest: StartupManifest,
    pub provides: AgentProvides,
    pub warnings: Vec<LaunchWarning>,
    pub launch_admission: crate::agents::adapter::LaunchAdmission,
    /// Every MCP server the target team's config offers (the bridge
    /// aside), so the editor and starter detail can list what a copy could
    /// grant without a separate call.
    pub offered_mcp_servers: Vec<String>,
    /// False until selective bootstrap ships: a launch still gets the team's
    /// whole projected content, and `manifest` is what it will get.
    pub manifest_applies_at_launch: bool,
    pub archived: bool,
}

impl From<ResolvedAgent> for ResolvedItem {
    fn from(resolved: ResolvedAgent) -> Self {
        Self {
            account: AccountItem::from(&resolved.account),
            agent: resolved.agent,
            launch: resolved.launch,
            manifest: resolved.manifest,
            provides: resolved.provides,
            warnings: resolved.warnings,
            launch_admission: resolved.launch_admission,
            offered_mcp_servers: resolved.offered_mcp_servers,
            manifest_applies_at_launch: resolved.manifest_applies_at_launch,
            archived: resolved.archived,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct RevisionItem {
    pub n: u32,
    pub author: Author,
    pub at: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AgentGot {
    #[serde(flatten)]
    pub cap: crate::session_usage::spend_caps::CapStatus,
    pub id: String,
    pub scope: AgentScope,
    pub created_by: String,
    pub revision: u32,
    pub archived: bool,
    /// The `context` as stored, as `{path, dest?}` only (any key a newer build
    /// stored is in `definition` and kept by agent_update); `null` while the
    /// runtime default applies. Passing it back to agent_update leaves the
    /// context unchanged (it still saves a revision).
    pub context: Option<Vec<ContextEntry>>,
    /// What a launch into a repo's workspace gets after `startup`: `context`, or
    /// the runtime's default while it is `null` (`[]` is none). Per-repo
    /// overrides apply on top.
    pub effective_context: Vec<ContextEntry>,
    /// What a launch into the team workspace gets after `startup`: `context`, or
    /// the team-workspace default while it is `null`.
    pub effective_context_team_workspace: Vec<ContextEntry>,
    /// True while `context` is `null` and both effective lists are defaults.
    pub context_defaulted: bool,
    /// The definition exactly as stored.
    pub definition: Agent,
    /// Its per-repo context overrides.
    pub repo_overrides: Vec<RepoOverrideItem>,
    /// Override files that could not be read (each skipped).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub repo_override_errors: Vec<String>,
    /// Every revision, oldest first.
    pub revisions: Vec<RevisionItem>,
    pub resolved: ResolvedItem,
    /// The sessions running it now, each as `list_sessions` lists it.
    pub instances: Vec<super::session_list::SessionListRow>,
    /// Its routines: every non-archived routine that belongs to it,
    /// in every space you can see, with this Mac's switch and next fire.
    pub routines: Vec<AgentRoutineItem>,
    /// Why `routines` could not be read (it is then empty); the agent itself
    /// still reads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routines_error: Option<String>,
    /// The team's `projections.yml` entries this agent's `context`
    /// lacks, offered once ("Add these to its context?"); never applied
    /// unless the user accepts through `agent_update`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_suggestion: Option<crate::agents::context_suggestion::ContextSuggestion>,
}

/// One routine of an agent, as `agent_get` lists it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentRoutineItem {
    /// The routine's `id`, as every routine tool takes it.
    pub id: String,
    pub name: String,
    /// `fresh` or `wake`.
    pub mode: String,
    /// RFC3339, or null when it has no scheduled fire from here.
    pub next_run_at: Option<String>,
    /// This Mac's switch.
    pub enabled: bool,
    /// Its wake queued on a busy session (`⏰ due`), else `null`.
    /// Filled by the app from the delivery core.
    pub queued: Option<crate::delivery::read_model::QueuedWake>,
}

/// A routine an agent's archive or unarchive took with it, or offers.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct OwnedRoutineItem {
    pub id: String,
    pub name: String,
}

impl From<crate::routines::lifecycle::OwnedRoutine> for OwnedRoutineItem {
    fn from(r: crate::routines::lifecycle::OwnedRoutine) -> Self {
        Self {
            id: r.key.to_string(),
            name: r.name,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct AgentSaved {
    pub id: String,
    pub revision: u32,
    /// The `context` as stored, `{path, dest?}` only, as agent_get reports it;
    /// `null` while the runtime default applies.
    pub context: Option<Vec<ContextEntry>>,
    /// Every per-repo override it has now.
    pub repo_overrides: Vec<RepoOverrideItem>,
    /// What a launch would run, as agent_get resolves it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<ResolvedItem>,
    /// Why the saved agent could not be resolved, when it could not. The save
    /// itself stands.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_error: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub dry_run: bool,
}

impl AgentSaved {
    fn new(
        key: &AgentKey,
        agent: &Agent,
        repo_overrides: Vec<RepoOverrideItem>,
        preview: Result<ResolvedItem, String>,
        dry_run: bool,
    ) -> Self {
        let (preview, preview_error) = match preview {
            Ok(preview) => (Some(preview), None),
            Err(e) => (None, Some(e)),
        };
        Self {
            id: key.to_string(),
            revision: agent.revision,
            context: agent.context.as_deref().map(wire_context),
            repo_overrides,
            preview,
            preview_error,
            dry_run,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct AgentArchived {
    pub id: String,
    pub archived: bool,
    pub revision: u32,
    /// What still references it (routine keys); they keep working.
    pub used_by: Vec<String>,
    /// Its routines, archived with it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub archived_routines: Vec<OwnedRoutineItem>,
}

/// What `agent_unarchive` did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct AgentUnarchived {
    pub id: String,
    pub revision: u32,
    pub archived: bool,
    /// Routines brought back with it (`restoreRoutines: true`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub restored_routines: Vec<OwnedRoutineItem>,
    /// Routines its archive archived that are still archived: offer them,
    /// then call again with `restoreRoutines: true`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub archived_routines: Vec<OwnedRoutineItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct AgentDeleted {
    pub id: String,
    pub deleted: bool,
}

/// What `agent_delete` did.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum AgentRemoved {
    Archived(AgentArchived),
    Deleted(AgentDeleted),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct AgentChanged {
    pub id: String,
    pub revision: u32,
    pub archived: bool,
}

/// The input a validation error is about, by the name the tools take it.
fn field_of(e: &ValidationError) -> Option<String> {
    Some(match e {
        ValidationError::RuntimeUnavailable(_) => "runtime".into(),
        ValidationError::InvalidId(_) => "id".into(),
        ValidationError::InvalidBudget(_) => "budgetUsd".into(),
        ValidationError::EmptyName => "name".into(),
        ValidationError::InvalidArg(_) => "args".into(),
        ValidationError::TooManyEnvVars(_)
        | ValidationError::InvalidEnvName(_)
        | ValidationError::InvalidEnvValue(_)
        | ValidationError::EngineTelemetryEnv(_)
        | ValidationError::EnvValueNotString(_) => "env".into(),
        ValidationError::InvalidMcpServer(_)
        | ValidationError::DuplicateMcpServer(_)
        | ValidationError::BridgeServerListed => "mcpServers".into(),
        ValidationError::InvalidBridgeTool(_)
        | ValidationError::DuplicateBridgeTool(_)
        | ValidationError::BridgeToolsUnsupported => "mcpBridgeTools".into(),
        ValidationError::InvalidStartupPath(_)
        | ValidationError::DuplicateStartupPath(_)
        | ValidationError::OverlappingStartupPath { .. } => "startup".into(),
        ValidationError::UnknownPermissionKey { runtime, key } => {
            format!("permissions.{runtime}.{key}")
        }
        ValidationError::InvalidPermission(_) | ValidationError::DuplicatePermission(_) => {
            "permissions".into()
        }
        ValidationError::InvalidContext(_) => "context".into(),
        ValidationError::InvalidRepoOverride(_) => "repoOverrides".into(),
        ValidationError::InvalidRevision => return None,
    })
}

/// A service refusal as the reply the agent branches on. A store or disk
/// failure is not the caller's to fix, so it stays an error.
fn refused<T>(e: AgentError) -> Reply<T> {
    let message = e.to_string();
    let refusal = match e {
        AgentError::Validation(v) => Refusal::invalid(field_of(&v).as_deref(), message),
        AgentError::Conflict { current_revision } => Refusal {
            code: RefusalCode::Conflict,
            current_revision: Some(current_revision),
            ..Refusal::invalid(None, message)
        },
        AgentError::NotFound(_) | AgentError::RevisionNotFound { .. } => {
            Refusal::not_found(message)
        }
        AgentError::AlreadyExists(_) => Refusal::invalid(Some("id"), message),
        AgentError::InUse { referrers, .. } => Refusal {
            code: RefusalCode::InUse,
            referrers: Some(referrers),
            ..Refusal::invalid(None, message)
        },
        AgentError::InvalidLocation(_) => Refusal::invalid(None, message),
        AgentError::ReadOnly(_) => Refusal {
            code: RefusalCode::ReadOnly,
            ..Refusal::invalid(None, message)
        },
        AgentError::Io(_) => return Err(message),
    };
    Ok(AgentReply::Refused(refusal))
}

/// A routine-store failure in this tool family's reply shape.
fn routine_refused<T>(e: crate::routines::RoutineError) -> Reply<T> {
    use crate::routines::RoutineError;
    match e {
        RoutineError::Io(m) | RoutineError::Store(m) => Err(m),
        other => Ok(AgentReply::Refused(Refusal::invalid(
            None,
            other.to_string(),
        ))),
    }
}

macro_rules! routine_try {
    ($call:expr) => {
        match $call {
            Ok(value) => value,
            Err(e) => return routine_refused(e),
        }
    };
}

fn unresolved<T>(e: ResolveError) -> Reply<T> {
    Ok(AgentReply::Refused(Refusal::not_found(e.to_string())))
}

/// A refused placement (a bad scope, an unknown or unsynced team) in this
/// tool family's reply shape, naming the input to fix.
pub(crate) fn placement_refused<T>(field: Option<&str>, message: String) -> AgentReply<T> {
    AgentReply::Refused(Refusal::invalid(field, message))
}

/// The message a refused team resolution carries.
pub(crate) fn team_refusal_message(envelope: ToolEnvelope<()>) -> String {
    match envelope {
        ToolEnvelope::Validation { message } => message,
        ToolEnvelope::NotSyncedLocally => "that team is not synced on this machine".to_string(),
        ToolEnvelope::UnauthorizedTeam => "that team is not one you have synced".to_string(),
        other => {
            serde_json::to_string(&other).unwrap_or_else(|_| "that team cannot be used".into())
        }
    }
}

/// `?` for a service call inside a body: a refusal returns its reply.
macro_rules! service_try {
    ($call:expr) => {
        match $call {
            Ok(value) => value,
            Err(e) => return refused(e),
        }
    };
}

/// `?` for a parse step inside a body: a bad input returns `invalid`.
macro_rules! input_try {
    ($call:expr) => {
        match $call {
            Ok(value) => value,
            Err(refusal) => return Ok(AgentReply::Refused(refusal)),
        }
    };
}

/// Refuse input keys the tool does not take, naming every one (the first as
/// `field`) and saying what to send instead. Only tool input is this strict:
/// keys a stored `agent.yaml` carries that this build does not know are kept
/// as they are on every save.
fn no_unknown(unknown: &BTreeMap<String, serde_json::Value>) -> Result<(), Refusal> {
    let Some(first) = unknown.keys().next() else {
        return Ok(());
    };
    let hints: Vec<String> = unknown
        .keys()
        .map(|key| match key.as_str() {
            "mcp" => "`mcp`: pass MCP servers as `mcpServers: [..]`, not `mcp: {servers}`".into(),
            "created_by" | "createdBy" | "revision" | "archived_at" | "archivedAt" => {
                format!("`{key}`: set by CodeSense, leave it out")
            }
            _ => format!("`{key}`: not an input of this tool"),
        })
        .collect();
    Err(Refusal::invalid(
        Some(first),
        format!("unknown input: {}", hints.join("; ")),
    ))
}

fn parse_key(id: &str) -> Result<AgentKey, Refusal> {
    AgentKey::parse(id.trim()).ok_or_else(|| {
        Refusal::invalid(
            Some("id"),
            format!("{id:?} is not an agent id; pass the `id` agent_list returns"),
        )
    })
}

fn parse_runtime(raw: &str) -> Result<AgentCli, Refusal> {
    raw.trim()
        .parse()
        .map_err(|e: codesense_host_proto::agent_cli::UnknownAgentCli| {
            Refusal::invalid(Some("runtime"), e.to_string())
        })
}

fn parse_launch(raw: &str) -> Result<LaunchMode, Refusal> {
    match raw.trim() {
        "foreground" => Ok(LaunchMode::Foreground),
        "headless" => Ok(LaunchMode::Headless),
        other => Err(Refusal::invalid(
            Some("launch"),
            format!("launch {other:?} must be foreground or headless"),
        )),
    }
}

/// Apply a `permissions` input over `current`: each runtime block it names is
/// replaced whole (`null` removes it), the others are kept.
fn apply_permissions(
    current: Option<Permissions>,
    value: serde_json::Value,
) -> Result<Option<Permissions>, Refusal> {
    let serde_json::Value::Object(blocks) = value else {
        return Err(Refusal::invalid(
            Some("permissions"),
            "permissions must be an object keyed by runtime: {claude: {...}} or {codex: {...}}",
        ));
    };
    let mut permissions = current.unwrap_or_default();
    for (runtime, block) in blocks {
        let field = format!("permissions.{runtime}");
        let bad = |e: serde_json::Error| Refusal::invalid(Some(&field), format!("{field}: {e}"));
        let cli: AgentCli = runtime.parse().map_err(|_| {
            let known: Vec<&str> = AgentCli::ALL.iter().map(|cli| cli.as_str()).collect();
            Refusal::invalid(
                Some(&field),
                format!(
                    "permissions has no runtime {runtime:?}; use {}",
                    known.join(" or ")
                ),
            )
        })?;
        (crate::agents::adapter::desktop(cli).permissions.set)(&mut permissions, block)
            .map_err(bad)?;
    }
    Ok((permissions != Permissions::default()).then_some(permissions))
}

/// A new agent's id from its name, with room for a `-N` suffix.
fn id_from_name(name: &str) -> String {
    if !name.chars().any(|c| c.is_ascii_alphanumeric()) {
        return "agent".to_string();
    }
    let slug = crate::shared::slug::slugify(name);
    let id: String = slug.chars().take(55).collect();
    id.trim_end_matches('-').to_string()
}

/// The `agents/` dir a placement writes into, when it exists yet.
fn placement_dir(
    home: &Path,
    principal: &Principal,
    placement: &Placement,
) -> Result<Option<std::path::PathBuf>, AgentError> {
    match placement {
        Placement::Personal(owner) => roots::personal_agents_dir(home, principal, &owner.org),
        Placement::Team { org, team } => roots::team_agents_dir(home, principal, org, team),
    }
}

/// The definition a draft describes, with every default filled in.
fn draft_agent(
    draft: AgentDraft,
    id: String,
    session_runtime: Option<AgentCli>,
) -> Result<Agent, Refusal> {
    no_unknown(&draft.unknown)?;
    let runtime = match draft.runtime.as_deref() {
        Some(raw) => parse_runtime(raw)?,
        None => session_runtime.ok_or_else(|| {
            Refusal::invalid(
                Some("runtime"),
                // Hermes is a preview runtime: parse_runtime accepts it, but this
                // hint doesn't offer it.
                "pass `runtime`: `claude`, `codex` or `opencode`. It defaults to your own CLI \
                 only when CodeSense launched you (pass your `sessionId`) and recorded which",
            )
        })?,
    };
    let launch = match draft.launch.as_deref() {
        Some(raw) => parse_launch(raw)?,
        None => LaunchMode::default(),
    };
    let permissions = match draft.permissions {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => apply_permissions(None, value)?,
    };
    Ok(Agent {
        id,
        name: draft.name.trim().to_string(),
        description: draft.description.unwrap_or_default().trim().to_string(),
        created_by: String::new(),
        runtime,
        args: draft.args.unwrap_or_default(),
        launch,
        mcp: McpAccess {
            servers: draft.mcp_servers.unwrap_or_default(),
            bridge_tools: draft.mcp_bridge_tools.flatten(),
            ..McpAccess::default()
        },
        startup: draft.startup.unwrap_or_default(),
        context: draft
            .context
            .map(|entries| entries.into_iter().map(ContextEntry::from).collect()),
        permissions,
        default_prompt: draft.default_prompt.unwrap_or_default(),
        avatar: crate::commands::dashboard_launch::launch_label(draft.avatar.as_deref()),
        role: draft
            .role
            .as_deref()
            .map(crate::queue_roles::normalize_role)
            .transpose()
            .map_err(|e| Refusal::invalid(Some("role"), e))?,
        allow_wake: draft.allow_wake.unwrap_or(false),
        resume_after_restart: draft.resume_after_restart.unwrap_or(false),
        budget_usd: draft.budget_usd.flatten().map(Into::into),
        revision: 0,
        archived_at: None,
        extra: draft
            .startup_settings
            .map(|settings| {
                BTreeMap::from([(
                    "startup_settings".into(),
                    serde_yaml::to_value(settings).expect("JSON settings serialize to YAML"),
                )])
            })
            .unwrap_or_default(),
        env: env_strings(draft.env)?,
    })
}

/// The overrides an input names, each normalized on its own, or the refusal
/// of the first bad one (naming `repoOverrides`). The save checks them
/// against the agent's context too. Writes nothing.
fn parse_overrides(
    inputs: Option<Vec<RepoOverrideInput>>,
) -> Result<Vec<(SourceRepoSlug, RepoOverride)>, Refusal> {
    let bad = |e: &dyn std::fmt::Display| Refusal::invalid(Some("repoOverrides"), e.to_string());
    let mut out: Vec<(SourceRepoSlug, RepoOverride)> = Vec::new();
    for input in inputs.unwrap_or_default() {
        let slug = parse_repo(&input.repo).map_err(|e| bad(&e))?;
        let o = RepoOverride {
            add: input.add.into_iter().map(ContextEntry::from).collect(),
            remove: input.remove,
            ..RepoOverride::default()
        };
        let o = o.normalized().map_err(|e| bad(&e))?;
        out.push((slug, o));
    }
    Ok(out)
}

/// `context` entries as the tools report them: `{path, dest?}`, without the
/// keys a newer build stored (those stay in `definition`, and agent_update
/// keeps them), so a reply's list is valid agent_update input as it is.
fn wire_context(entries: &[ContextEntry]) -> Vec<ContextEntry> {
    entries
        .iter()
        .map(|e| ContextEntry {
            path: e.path.clone(),
            dest: e.dest.clone(),
            extra: Default::default(),
        })
        .collect()
}

/// `entries` with the keys a newer build stored on the matching stored entry
/// carried over, so a whole-list replace keeps them as the editor's in-place
/// edits do. The match is the stored entry with the same path and `dest`, else
/// the first with the same path. Tool input never carries such keys itself.
fn with_stored_keys(stored: &[ContextEntry], entries: Vec<ContextEntry>) -> Vec<ContextEntry> {
    use crate::agents::context_edit::normalize_path;
    let dest_of = |e: &ContextEntry| e.dest.as_deref().and_then(normalize_path);
    entries
        .into_iter()
        .map(|mut entry| {
            let path = normalize_path(&entry.path);
            let dest = dest_of(&entry);
            let same_path =
                |old: &&ContextEntry| path.is_some() && normalize_path(&old.path) == path;
            if let Some(old) = stored
                .iter()
                .filter(same_path)
                .find(|old| dest_of(old) == dest)
                .or_else(|| stored.iter().find(same_path))
            {
                for (key, value) in &old.extra {
                    entry
                        .extra
                        .entry(key.clone())
                        .or_insert_with(|| value.clone());
                }
            }
            entry
        })
        .collect()
}

/// Each replaced override (not a removal) keeps the keys a newer build stored on
/// the override and on its `add` entries. An unreadable stored override has none
/// to keep: the replace overwrites it.
fn keep_stored_override_keys(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    overrides: &mut [(SourceRepoSlug, RepoOverride)],
) {
    for (slug, o) in overrides.iter_mut() {
        if o.add.is_empty() && o.remove.is_empty() {
            continue;
        }
        let Ok(Some(stored)) = service::repo_override(home, principal, key, slug) else {
            continue;
        };
        o.add = with_stored_keys(&stored.add, std::mem::take(&mut o.add));
        for (k, v) in stored.extra {
            o.extra.entry(k).or_insert(v);
        }
    }
}

/// Overrides as listed on the wire, sorted by repo.
fn override_items(overrides: &[(SourceRepoSlug, RepoOverride)]) -> Vec<RepoOverrideItem> {
    RepoOverrideItem::list(
        overrides
            .iter()
            .filter(|(_, o)| !o.is_empty())
            .filter_map(|(slug, o)| Some((repo_label(slug).ok()?, o.clone())))
            .collect(),
    )
}

/// The saved agent's overrides after a save, read after the commit: a read
/// that fails never fails the save, it only lands in `preview_error`.
fn overrides_after_save(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
) -> (Vec<RepoOverrideItem>, Option<String>) {
    match service::repo_overrides(home, principal, key) {
        Ok((all, problems)) => (
            RepoOverrideItem::list(all),
            (!problems.is_empty()).then(|| problems.join("; ")),
        ),
        Err(e) => (Vec::new(), Some(format!("cannot list repo overrides: {e}"))),
    }
}

/// Answer a committed save: its overrides and what a launch would run. Only
/// reads; any failure is reported in `preview_error`, never as a refusal.
fn saved_reply(
    home: &Path,
    principal: &Principal,
    saved: &service::SavedAgent,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> AgentSaved {
    let (repo_overrides, listing_error) = overrides_after_save(home, principal, &saved.key);
    let preview = crate::agents::resolve::resolve_agent_with(
        scope,
        home,
        principal,
        &saved.key,
        ResolveOptions::default(),
    )
    .map(ResolvedItem::from)
    .map_err(|e| e.to_string());
    let mut reply = AgentSaved::new(&saved.key, &saved.agent, repo_overrides, preview, false);
    if let Some(e) = listing_error {
        reply.preview_error = Some(match reply.preview_error.take() {
            Some(earlier) => format!("{earlier}; {e}"),
            None => e,
        });
    }
    reply
}

/// The id a draft gets at `placement`: its own, else the first free one
/// derived from its name.
fn draft_id(
    home: &Path,
    principal: &Principal,
    placement: &Placement,
    draft: &AgentDraft,
) -> Result<String, AgentError> {
    if let Some(id) = draft
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        return Ok(id.to_string());
    }
    let base = id_from_name(&draft.name);
    Ok(match placement_dir(home, principal, placement)? {
        Some(dir) => service::first_free_id(&dir, &base),
        None => base,
    })
}

/// `agent_create`: write a new agent, credited to the caller, and return what
/// a launch of it would run. A dry run checks the same things and writes
/// nothing.
pub(crate) fn create(
    home: &Path,
    principal: &Principal,
    placement: Placement,
    caller: &AgentCaller,
    params: AgentCreateParams,
) -> Reply<AgentSaved> {
    create_for(
        home,
        principal,
        placement,
        caller,
        params,
        &Default::default(),
    )
}

pub(crate) fn create_for(
    home: &Path,
    principal: &Principal,
    placement: Placement,
    caller: &AgentCaller,
    params: AgentCreateParams,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Reply<AgentSaved> {
    let admitted = match &placement {
        Placement::Team { org, team } => scope.target_team(home, principal, org, team),
        Placement::Personal(_) => true,
    };
    if params.dry_run && !admitted {
        return Ok(AgentReply::Refused(Refusal::not_found("Agent not found")));
    }
    // Architecture: preserve the write, but mask every result, including duplicate
    // and ID allocation errors, when its definition namespace is not readable.
    let result = create_in(home, principal, placement, caller, params, scope);
    if !admitted {
        return Ok(AgentReply::Refused(Refusal::not_found("Agent not found")));
    }
    result
}

fn create_in(
    home: &Path,
    principal: &Principal,
    placement: Placement,
    caller: &AgentCaller,
    params: AgentCreateParams,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Reply<AgentSaved> {
    let mut params = params;
    let id = service_try!(draft_id(home, principal, &placement, &params.draft));
    let overrides = input_try!(parse_overrides(params.draft.repo_overrides.take()));
    let agent = input_try!(draft_agent(params.draft, id, caller.session_runtime));
    if params.dry_run {
        let (key, checked) =
            service_try!(service::check_create(home, principal, &placement, agent));
        let overrides = service_try!(service::check_repo_overrides(&checked, &overrides));
        let preview = match crate::agents::resolve::preview_agent_with(
            scope,
            home,
            principal,
            &checked,
            &placement,
            ResolveOptions::default(),
        ) {
            Ok(preview) => Ok(preview.into()),
            Err(e) => Err(e.to_string()),
        };
        return Ok(AgentReply::Ok(AgentSaved::new(
            &key,
            &checked,
            override_items(&overrides),
            preview,
            true,
        )));
    }
    let saved = service_try!(service::save_with_overrides(
        home,
        principal,
        SaveTarget::Create(placement),
        agent,
        &overrides,
        &caller.author,
    ));
    if !scope.definition(home, principal, &saved.key) {
        return Ok(AgentReply::Refused(Refusal::not_found("Agent not found")));
    }
    Ok(AgentReply::Ok(saved_reply(home, principal, &saved, scope)))
}

/// What `agent_preview` answers: what a launch would run, plus the context
/// want set for the repo it names.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct AgentPreviewed {
    #[serde(flatten)]
    pub resolved: ResolvedItem,
    /// The resolved want set: files, bytes, each file's tier, and
    /// warnings.
    pub context: ContextResolution,
    /// The per-repo overrides the agent (or draft) has.
    pub repo_overrides: Vec<RepoOverrideItem>,
}

/// The most files `agent_preview` lists; `totals` still count every one.
pub(crate) const PREVIEW_MAX_FILES: usize = 500;

/// `agent_preview`: what an unsaved draft, or a saved agent (`agent`), would
/// resolve to at `placement`, with its context for `repo`. Writes nothing.
pub(crate) fn preview(
    home: &Path,
    principal: &Principal,
    placement: Placement,
    caller: &AgentCaller,
    params: AgentPreviewParams,
) -> Reply<AgentPreviewed> {
    preview_for(
        home,
        principal,
        placement,
        caller,
        params,
        &Default::default(),
    )
}

pub(crate) fn preview_for(
    home: &Path,
    principal: &Principal,
    placement: Placement,
    caller: &AgentCaller,
    params: AgentPreviewParams,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Reply<AgentPreviewed> {
    Ok(
        match preview_detail(
            home,
            principal,
            placement,
            caller,
            params,
            ResolveOptions::default(),
            true,
            scope,
        )? {
            AgentReply::Ok((resolved, context, repo_overrides)) => AgentReply::Ok(AgentPreviewed {
                resolved,
                context: context
                    .expect("context is resolved when asked for")
                    .capped(PREVIEW_MAX_FILES),
                repo_overrides,
            }),
            AgentReply::Refused(refusal) => AgentReply::Refused(refusal),
        },
    )
}

/// [`preview`] resolved against `options` (such as the team a personal
/// agent would be launched into), for the Agents tab's live preview. It does
/// not walk the context: the launch preview is all it answers.
pub(crate) fn preview_with(
    home: &Path,
    principal: &Principal,
    placement: Placement,
    caller: &AgentCaller,
    params: AgentPreviewParams,
    options: ResolveOptions,
) -> Reply<ResolvedItem> {
    Ok(
        match preview_detail(
            home,
            principal,
            placement,
            caller,
            params,
            options,
            false,
            &Default::default(),
        )? {
            AgentReply::Ok((resolved, ..)) => AgentReply::Ok(resolved),
            AgentReply::Refused(refusal) => AgentReply::Refused(refusal),
        },
    )
}

/// The draft fields given alongside `agent`, which then means nothing.
fn draft_fields(draft: &AgentDraft) -> Vec<&'static str> {
    let mut out = Vec::new();
    let mut given = |set: bool, name: &'static str| {
        if set {
            out.push(name);
        }
    };
    given(!draft.name.is_empty(), "name");
    given(draft.id.is_some(), "id");
    given(draft.description.is_some(), "description");
    given(draft.runtime.is_some(), "runtime");
    given(draft.args.is_some(), "args");
    given(draft.startup_settings.is_some(), "startupSettings");
    given(draft.env.is_some(), "env");
    given(draft.launch.is_some(), "launch");
    given(draft.mcp_servers.is_some(), "mcpServers");
    given(draft.mcp_bridge_tools.is_some(), "mcpBridgeTools");
    given(draft.startup.is_some(), "startup");
    given(draft.context.is_some(), "context");
    given(draft.repo_overrides.is_some(), "repoOverrides");
    given(draft.permissions.is_some(), "permissions");
    given(draft.default_prompt.is_some(), "defaultPrompt");
    given(draft.avatar.is_some(), "avatar");
    given(draft.allow_wake.is_some(), "allowWake");
    given(draft.resume_after_restart.is_some(), "resumeAfterRestart");
    out
}

type Previewed = (
    ResolvedItem,
    Option<ContextResolution>,
    Vec<RepoOverrideItem>,
);

fn preview_detail(
    home: &Path,
    principal: &Principal,
    placement: Placement,
    caller: &AgentCaller,
    mut params: AgentPreviewParams,
    options: ResolveOptions,
    with_context: bool,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Reply<Previewed> {
    let target = params
        .org
        .as_ref()
        .zip(params.team.as_ref())
        .or(options.target_team.as_ref().map(|(org, team)| (org, team)));
    if target.is_some_and(|(org, team)| !scope.target_team(home, principal, org, team)) {
        return Ok(AgentReply::Refused(Refusal::not_found("Agent not found")));
    }
    if params.agent.is_none() {
        if let Placement::Team { org, team } = &placement {
            if !scope.target_team(home, principal, org, team) {
                return Ok(AgentReply::Refused(Refusal::not_found("Agent not found")));
            }
        }
    }
    let repo = match params
        .repo
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
    {
        Some(raw) => Some(input_try!(
            parse_repo(raw).map_err(|e| Refusal::invalid(Some("repo"), e.to_string()))
        )),
        None => None,
    };
    if let Some(id) = params.agent.as_deref() {
        input_try!(no_unknown(&params.draft.unknown));
        if let Some(first) = draft_fields(&params.draft).first() {
            return Ok(AgentReply::Refused(Refusal::invalid(
                Some(first),
                format!(
                    "`{first}` is a draft field; with `agent` the saved agent is previewed as it \
                     is. Pass either `agent` or a draft, not both"
                ),
            )));
        }
        let key = input_try!(parse_key(id));
        if !scope.definition(home, principal, &key) {
            return Ok(AgentReply::Refused(Refusal::not_found("Agent not found")));
        }
        // `org`+`team` name the team the agent would be launched into.
        let options = match (params.org.clone(), params.team.clone()) {
            (Some(org), Some(team)) => ResolveOptions {
                target_team: Some((org, team)),
            },
            _ => options,
        };
        let launch_target = options
            .target_team
            .clone()
            .map(|(org, team)| ManifestTarget::team(org, team));
        let resolved =
            match crate::agents::resolve::resolve_agent_with(scope, home, principal, &key, options)
            {
                Ok(resolved) => {
                    ResolvedItem::from(resolved.at_launch_cwd(params.cwd.as_deref().map(Path::new)))
                }
                Err(e) => return unresolved(e),
            };
        if !with_context {
            return Ok(AgentReply::Ok((resolved, None, Vec::new())));
        }
        let target = ManifestTarget {
            repo,
            ..ManifestTarget::for_launch(&key, launch_target)
        };
        let context = service_try!(service::resolve_agent_context_with(
            scope,
            home,
            principal,
            &key,
            &target,
            params.team_workspace,
        ));
        let (all, _) = service_try!(service::repo_overrides(home, principal, &key));
        return Ok(AgentReply::Ok((
            resolved,
            Some(context),
            RepoOverrideItem::list(all),
        )));
    }
    let launch_target = options
        .target_team
        .clone()
        .map(|(org, team)| ManifestTarget::team(org, team));
    let overrides = input_try!(parse_overrides(params.draft.repo_overrides.take()));
    let id = service_try!(draft_id(home, principal, &placement, &params.draft));
    let mut agent = input_try!(draft_agent(params.draft, id, caller.session_runtime));
    agent.created_by = principal.to_string();
    agent.revision = 1;
    // The checks a save runs on the body, so a preview that passes is a draft
    // a create accepts (where it goes and whether its id is free aside).
    service_try!(agent.normalize_startup().map_err(AgentError::from));
    service_try!(agent.validate().map_err(AgentError::from));
    let overrides = service_try!(service::check_repo_overrides(&agent, &overrides));
    let resolved = match crate::agents::resolve::preview_agent_with(
        scope, home, principal, &agent, &placement, options,
    ) {
        Ok(preview) => {
            ResolvedItem::from(preview.at_launch_cwd(params.cwd.as_deref().map(Path::new)))
        }
        Err(e) => return Ok(AgentReply::Refused(Refusal::invalid(None, e.to_string()))),
    };
    if !with_context {
        return Ok(AgentReply::Ok((resolved, None, Vec::new())));
    }
    let target = ManifestTarget {
        repo: repo.clone(),
        ..ManifestTarget::for_launch(&resolved.agent.key, launch_target)
    };
    let label = repo.as_ref().and_then(|slug| repo_label(slug).ok());
    let repo_override = overrides
        .iter()
        .find(|(slug, _)| label.is_some() && repo_label(slug).ok() == label)
        .map(|(_, o)| o);
    let context = crate::agents::manifest_resolve::context::resolve_context_with(
        scope,
        home,
        principal,
        ContextRequest {
            agent: &agent,
            target: &target,
            repo_override,
            team_workspace: params.team_workspace,
        },
    );
    Ok(AgentReply::Ok((
        resolved,
        Some(context),
        override_items(&overrides),
    )))
}

/// `agent_update`: apply a patch on top of `base_revision`. A stale base is a
/// `conflict` that changes nothing. Any agent the caller can see is theirs to
/// change.
pub(crate) fn update(
    home: &Path,
    principal: &Principal,
    caller: &AgentCaller,
    params: AgentUpdateParams,
) -> Reply<AgentSaved> {
    update_for(home, principal, caller, params, &Default::default())
}

pub(crate) fn update_for(
    home: &Path,
    principal: &Principal,
    caller: &AgentCaller,
    params: AgentUpdateParams,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Reply<AgentSaved> {
    input_try!(no_unknown(&params.unknown));
    let key = input_try!(parse_key(&params.id));
    // A starter is refused before any input is checked: copying it is the fix.
    if crate::agents::builtin::is_starter(&key) {
        return refused(AgentError::ReadOnly(key));
    }
    let agent = service_try!(service::read_current(home, principal, &key));
    let target = SaveTarget::Update {
        key,
        base_revision: params.base_revision,
    };
    save_patched(home, principal, caller, target, agent, params, scope)
}

/// Apply an update's fields to `agent` and save it to `target`.
fn save_patched(
    home: &Path,
    principal: &Principal,
    caller: &AgentCaller,
    target: SaveTarget,
    agent: Agent,
    mut params: AgentUpdateParams,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Reply<AgentSaved> {
    let mut overrides = input_try!(parse_overrides(params.repo_overrides.take()));
    if let SaveTarget::Update { key, .. } = &target {
        keep_stored_override_keys(home, principal, key, &mut overrides);
    }
    let agent = input_try!(patched(agent, params));
    commit_patched(home, principal, caller, target, agent, &overrides, scope)
}

/// `agent` with an update's fields applied, or the refusal of the first bad one.
fn patched(mut agent: Agent, params: AgentUpdateParams) -> Result<Agent, Refusal> {
    if let Some(name) = params.name {
        agent.name = name.trim().to_string();
    }
    if let Some(description) = params.description {
        agent.description = description.trim().to_string();
    }
    if let Some(raw) = params.runtime.as_deref() {
        agent.runtime = parse_runtime(raw)?;
    }
    if let Some(settings) = params.startup_settings {
        agent.set_startup_settings(settings);
    }
    if let Some(args) = params.args {
        agent.args = args;
    }
    if let Some(env) = params.env {
        agent.env = env_strings(env)?;
    }
    if let Some(raw) = params.launch.as_deref() {
        agent.launch = parse_launch(raw)?;
    }
    if let Some(servers) = params.mcp_servers {
        agent.mcp.servers = servers;
    }
    if let Some(tools) = params.mcp_bridge_tools {
        agent.mcp.bridge_tools = tools;
    }
    if let Some(startup) = params.startup {
        agent.startup = startup;
    }
    if let Some(context) = params.context {
        let stored = agent.context.take().unwrap_or_default();
        agent.context = context.map(|entries| {
            with_stored_keys(
                &stored,
                entries.into_iter().map(ContextEntry::from).collect(),
            )
        });
    }
    match params.permissions {
        None => {}
        Some(None) => agent.permissions = None,
        Some(Some(value)) => {
            agent.permissions = apply_permissions(agent.permissions.take(), value)?;
        }
    }
    if let Some(prompt) = params.default_prompt {
        agent.default_prompt = prompt;
    }
    if let Some(avatar) = params.avatar {
        agent.avatar = crate::commands::dashboard_launch::launch_label(Some(&avatar));
    }
    if let Some(role) = params.role {
        agent.role = role
            .as_deref()
            .map(crate::queue_roles::normalize_role)
            .transpose()
            .map_err(|e| Refusal::invalid(Some("role"), e))?;
    }
    if let Some(allow_wake) = params.allow_wake {
        agent.allow_wake = allow_wake;
    }
    if let Some(cap) = params.budget_usd {
        agent.budget_usd = cap.map(Into::into);
    }
    if let Some(resume) = params.resume_after_restart {
        agent.resume_after_restart = resume;
    }
    Ok(agent)
}

/// Save a patched `agent` to `target` and answer with what a launch would run.
fn commit_patched(
    home: &Path,
    principal: &Principal,
    caller: &AgentCaller,
    target: SaveTarget,
    agent: Agent,
    overrides: &[(SourceRepoSlug, RepoOverride)],
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Reply<AgentSaved> {
    let saved = service_try!(service::save_with_overrides(
        home,
        principal,
        target,
        agent,
        overrides,
        &caller.author,
    ));
    if !scope.definition(home, principal, &saved.key) {
        return Ok(AgentReply::Refused(Refusal::not_found("Agent not found")));
    }
    Ok(AgentReply::Ok(saved_reply(home, principal, &saved, scope)))
}

/// Which agents `agent_list` keeps for a `scope`.
fn in_scope(scope: &str, principal: &Principal, found: &crate::agents::DiscoveredAgent) -> bool {
    match scope {
        "mine" => {
            found.scope == AgentScope::Personal || found.agent.created_by == principal.as_str()
        }
        "team" => found.scope == AgentScope::Team,
        "builtin" => found.scope == AgentScope::Builtin,
        _ => true,
    }
}

/// `agent_list`: every agent the caller can see, personal and team, then the
/// starters.
pub(crate) fn list(
    home: &Path,
    principal: &Principal,
    params: AgentListParams,
) -> Reply<Vec<AgentListItem>> {
    list_for(home, principal, params, &Default::default())
}

pub(crate) fn list_for(
    home: &Path,
    principal: &Principal,
    params: AgentListParams,
    caller: &crate::context_repo::caller_scope::CallerScope,
) -> Reply<Vec<AgentListItem>> {
    let scope = params.scope.as_deref().map(str::trim).unwrap_or("all");
    if !matches!(scope, "all" | "mine" | "team" | "builtin") {
        return Ok(AgentReply::Refused(Refusal::invalid(
            Some("scope"),
            format!("scope {scope:?} must be all, mine, team or builtin"),
        )));
    }
    let found = service_try!(service::list_for(
        home,
        principal,
        params.include_archived,
        caller
    ));
    let mut items = Vec::new();
    for agent in found.into_iter().filter(|a| in_scope(scope, principal, a)) {
        let (manifest, manifest_error) = if params.include_manifest {
            match crate::agents::resolve::resolve_agent_with(
                caller,
                home,
                principal,
                &agent.key,
                ResolveOptions::default(),
            ) {
                Ok(r) => (
                    Some(ManifestSummary {
                        files: r.manifest.totals.files,
                        bytes: r.manifest.totals.bytes,
                        error_count: r.manifest.errors.len(),
                    }),
                    None,
                ),
                Err(e) => (None, Some(e.to_string())),
            }
        } else {
            (None, None)
        };
        items.push(AgentListItem {
            cap: crate::session_usage::spend_caps::agent_status(
                home,
                &agent.key.to_string(),
                &agent.agent,
            )?,
            id: agent.key.to_string(),
            name: agent.agent.name.clone(),
            description: agent.agent.description.clone(),
            runtime: agent.agent.runtime,
            launch: agent.agent.launch,
            mcp_servers: agent.agent.mcp.servers.clone(),
            startup_count: agent.agent.startup.len(),
            created_by: agent.created_by_display,
            scope: agent.scope,
            org: agent.org,
            team: agent.team,
            archived: agent.agent.is_archived(),
            revision: agent.agent.revision,
            manifest,
            manifest_error,
        });
    }
    Ok(AgentReply::Ok(items))
}

/// `agent_get`: the definition, its history and what a launch of it would
/// run. Archived agents resolve too.
pub(crate) fn get(home: &Path, principal: &Principal, params: AgentGetParams) -> Reply<AgentGot> {
    get_for(home, principal, params, &Default::default())
}

pub(crate) fn get_for(
    home: &Path,
    principal: &Principal,
    params: AgentGetParams,
    caller: &crate::context_repo::caller_scope::CallerScope,
) -> Reply<AgentGot> {
    let key = input_try!(parse_key(&params.id));
    if !caller.definition(home, principal, &key) {
        return Ok(AgentReply::Refused(Refusal::not_found("Agent not found")));
    }
    let detail = match service::get(home, principal, &key) {
        Err(AgentError::NotFound(_))
            if !matches!(
                caller,
                crate::context_repo::caller_scope::CallerScope::Human
            ) =>
        {
            return Ok(AgentReply::Refused(Refusal::not_found("Agent not found")));
        }
        result => service_try!(result),
    };
    let (all, repo_override_errors) = service_try!(service::repo_overrides(home, principal, &key));
    let repo_overrides = RepoOverrideItem::list(all);
    let resolved = match crate::agents::resolve::resolve_agent_with(
        caller,
        home,
        principal,
        &key,
        ResolveOptions::default(),
    ) {
        Ok(resolved) => resolved.into(),
        Err(e) => return unresolved(e),
    };
    // Its routines never fail the read of the agent itself. Reading them may
    // bring a legacy routine forward first (`routines::migrate`): idempotent,
    // and a no-op once a space is migrated.
    let (routines, routines_error) =
        match agent_routines_for(home, principal, &key, chrono::Utc::now(), caller) {
            Ok(routines) => (routines, None),
            Err(e) => {
                log::warn!(target: "agents", "agent {key}: routines unreadable: {e}");
                (Vec::new(), Some(e.to_string()))
            }
        };
    let context_suggestion = crate::agents::context_suggestion::suggestion_for_caller(
        home,
        principal,
        &key,
        &detail.agent,
        caller,
    );
    let context = detail.agent.context.as_deref().map(wire_context);
    let context_defaulted = context.is_none();
    // The same default the launch path resolves (manifest_resolve::context).
    let effective = |team_workspace: bool| {
        context.clone().unwrap_or_else(|| {
            crate::agents::context::default_context(detail.agent.runtime, team_workspace)
        })
    };
    let effective_context = effective(false);
    let effective_context_team_workspace = effective(true);
    Ok(AgentReply::Ok(AgentGot {
        cap: crate::session_usage::spend_caps::agent_status(
            home,
            &detail.key.to_string(),
            &detail.agent,
        )?,
        id: detail.key.to_string(),
        scope: detail.scope,
        created_by: detail.created_by_display,
        revision: detail.agent.revision,
        archived: detail.agent.is_archived(),
        revisions: detail
            .revisions
            .into_iter()
            .map(|r| RevisionItem {
                n: r.revision,
                author: r.author,
                at: r.at,
            })
            .collect(),
        context,
        effective_context,
        effective_context_team_workspace,
        context_defaulted,
        definition: detail.agent,
        repo_overrides,
        repo_override_errors,
        resolved,
        instances: Vec::new(),
        routines,
        routines_error,
        context_suggestion,
    }))
}

/// `agent`'s routines as `agent_get` lists them.

fn agent_routines_for(
    home: &Path,
    principal: &Principal,
    agent: &AgentKey,
    now: chrono::DateTime<chrono::Utc>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<Vec<AgentRoutineItem>, crate::routines::RoutineError> {
    Ok(
        crate::routines::service::summaries_for_agent_scoped(home, principal, agent, now, scope)?
            .into_iter()
            .map(|summary| AgentRoutineItem {
                id: summary.key.to_string(),
                name: summary.def.name.clone(),
                mode: summary.effective_mode.as_str().to_string(),
                next_run_at: summary.next_run_at.map(|ms| {
                    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
                        .unwrap_or_default()
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                }),
                enabled: summary.enabled,
                queued: None,
            })
            .collect(),
    )
}

/// `agent_delete`: archive the agent (what references it keeps working), or
/// with `hard` remove it for good, refused while anything references it.
pub(crate) fn delete(
    home: &Path,
    principal: &Principal,
    caller: &AgentCaller,
    referrers: &dyn AgentReferrers,
    params: AgentDeleteParams,
) -> Reply<AgentRemoved> {
    let key = input_try!(parse_key(&params.id));
    if params.hard {
        service_try!(service::delete(home, principal, &key, None, referrers));
        return Ok(AgentReply::Ok(AgentRemoved::Deleted(AgentDeleted {
            id: key.to_string(),
            deleted: true,
        })));
    }
    // Archiving an agent archives its routines, behind a confirm
    // that lists them.
    let owned = routine_try!(crate::routines::lifecycle::active_owned(
        home, principal, &key
    ));
    if !owned.is_empty() && !params.archive_routines && !crate::agents::builtin::is_starter(&key) {
        let names: Vec<String> = owned.iter().map(|r| r.name.clone()).collect();
        return Ok(AgentReply::Refused(Refusal {
            code: RefusalCode::NeedsConfirm,
            referrers: Some(owned.iter().map(|r| r.key.to_string()).collect()),
            ..Refusal::invalid(
                Some("archiveRoutines"),
                format!(
                    "archiving {key} also archives its {} routine(s): {}. Confirm with the user, then call again with archiveRoutines: true",
                    owned.len(),
                    names.join(", ")
                ),
            )
        }));
    }
    let saved = service_try!(service::archive(home, principal, &key, &caller.author));
    let archived_routines = routine_try!(crate::routines::lifecycle::archive_owned(
        home,
        principal,
        &saved.key,
        &caller.author
    ));
    let used_by = service_try!(service::used_by(home, principal, &saved.key, referrers));
    Ok(AgentReply::Ok(AgentRemoved::Archived(AgentArchived {
        id: saved.key.to_string(),
        archived: true,
        revision: saved.agent.revision,
        used_by,
        archived_routines: archived_routines.into_iter().map(Into::into).collect(),
    })))
}

/// `agent_restore`: make an old revision's definition current again, as a
/// new revision. Archived stays archived.
pub(crate) fn restore(
    home: &Path,
    principal: &Principal,
    caller: &AgentCaller,
    params: AgentRestoreParams,
) -> Reply<AgentChanged> {
    let key = input_try!(parse_key(&params.id));
    let saved = service_try!(service::restore(
        home,
        principal,
        &key,
        params.revision,
        params.base_revision,
        &caller.author,
    ));
    Ok(AgentReply::Ok(AgentChanged {
        id: saved.key.to_string(),
        revision: saved.agent.revision,
        archived: saved.agent.is_archived(),
    }))
}

/// `agent_unarchive`: bring an archived agent back into lists and pickers,
/// offering (or with `restoreRoutines`, restoring) the routines its archive
/// archived. Hooked here, never on `agent_restore`.
pub(crate) fn unarchive(
    home: &Path,
    principal: &Principal,
    caller: &AgentCaller,
    params: AgentUnarchiveParams,
) -> Reply<AgentUnarchived> {
    use crate::routines::lifecycle;
    let key = input_try!(parse_key(&params.id));
    let saved = service_try!(service::unarchive(
        home,
        principal,
        &key,
        params.base_revision,
        &caller.author,
    ));
    let restored = if params.restore_routines {
        routine_try!(lifecycle::restore_owned(
            home,
            principal,
            &saved.key,
            &caller.author
        ))
    } else {
        Vec::new()
    };
    let offered = routine_try!(lifecycle::archived_with(home, principal, &saved.key));
    Ok(AgentReply::Ok(AgentUnarchived {
        id: saved.key.to_string(),
        revision: saved.agent.revision,
        archived: saved.agent.is_archived(),
        restored_routines: restored.into_iter().map(Into::into).collect(),
        archived_routines: offered.into_iter().map(Into::into).collect(),
    }))
}

#[cfg(test)]
#[path = "agent_tools_tests.rs"]
mod tests;

pub(crate) async fn preview_request<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    session: Option<&str>,
    remote: Option<crate::commands::dashboard_launch::LaunchTeam>,
    caller: crate::commands::routines::Caller,
    agent: AgentCaller,
    placement: Placement,
    params: AgentPreviewParams,
) -> Reply<AgentPreviewed> {
    let scope = crate::context_repo::caller_scope::for_session(app, session, remote).await;
    let mut reply = crate::commands::await_blocking("mcp.agent_preview", move || {
        preview_for(
            &caller.home,
            &caller.principal,
            placement,
            &agent,
            params,
            &scope,
        )
    })
    .await;
    if let Ok(AgentReply::Ok(previewed)) = &mut reply {
        crate::commands::deps::note_cli_version(app, &mut previewed.resolved);
    }
    reply
}

/// Architecture: scope response reads without changing the existing write authorization.
pub(crate) fn readback_for<T>(
    result: Reply<T>,
    home: &Path,
    principal: &Principal,
    id: &str,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Reply<T> {
    if AgentKey::parse(id).is_some_and(|key| !scope.definition(home, principal, &key)) {
        return Ok(AgentReply::Refused(Refusal::not_found("Agent not found")));
    }
    result
}
