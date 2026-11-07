// SPDX-License-Identifier: MIT
//! The starter agents CodeSense ships: "Claude" and "Codex", one per built-in
//! CLI adapter, so a launch works before anyone has written an agent; their
//! unattended twins, "Claude (unattended)" and "Codex (unattended)", which run
//! without asking for permission and are what a background launch that names
//! no agent runs.
//!
//! Each is an ordinary [`Agent`] that lives in the app, never on disk. It runs
//! its runtime's defaults (no extra args, no MCP server beyond the bridge, the
//! CLI's own permission config) and has no `default_prompt`, so the user types
//! the task. Its `startup` names only the runtime's root instructions file
//! (`CLAUDE.md` for Claude, `AGENTS.md` for Codex), which the resolver applies
//! where the launch target's content layers have it (the usual precedence: the
//! personal space first, then the team) and leaves out elsewhere, as it does
//! for any agent. So a copy behaves exactly like the starter until it is
//! edited.
//!
//! The app keeps no history of a starter: any revision resolves to this
//! build's definition, so a session stamped by an earlier version still
//! verifies after an upgrade.
//!
//! A starter cannot be edited, archived or deleted. "Copy to Mine" makes an
//! ordinary editable agent from it.

use codesense_pty_core::agent_cli::descriptor;
use std::sync::OnceLock;

use super::keys::AgentKey;
use super::manifest::content_hash;
use super::model::{Agent, LaunchMode, McpAccess};
use codesense_pty_core::agent_cli::AgentCli;

/// Who a starter is credited to.
pub const CREATOR: &str = "CodeSense";

/// The refusal for any change to a starter.
pub const READ_ONLY_MESSAGE: &str = "built-in agent — Copy to Mine to customize";

/// The starter that runs on `runtime`.
pub fn key(runtime: AgentCli) -> AgentKey {
    AgentKey::builtin(runtime.as_str())
}

/// The suffix of an unattended starter's id: `claude-unattended`, `codex-unattended`.
const UNATTENDED_SUFFIX: &str = "-unattended";

/// The unattended starter that runs on `runtime`: no permission prompt ever blocks it.
pub fn unattended_key(runtime: AgentCli) -> AgentKey {
    AgentKey::builtin(format!("{}{UNATTENDED_SUFFIX}", runtime.as_str()))
}

/// The ordinary starter an unattended starter is the twin of (`builtin/claude` for
/// `builtin/claude-unattended`); `None` for any other key.
pub fn attended_twin(key: &AgentKey) -> Option<AgentKey> {
    if !key.is_builtin() {
        return None;
    }
    let runtime = key.id.strip_suffix(UNATTENDED_SUFFIX)?;
    AgentCli::ALL
        .into_iter()
        .find(|r| r.as_str() == runtime)
        .map(self::key)
}

use super::adapter::PREFERRED_RUNTIME;

/// The binary of the preferred runtime's CLI: what a launch that names no agent runs.
pub fn preferred_binary() -> &'static str {
    &descriptor(PREFERRED_RUNTIME).binary
}

/// The preferred runtime's headless runner recipe.
pub fn preferred_runner() -> &'static super::adapter::RunnerRecipe {
    &super::adapter::desktop(PREFERRED_RUNTIME).runner
}

/// The starter a launch that names no agent launches: the preferred runtime's.
pub fn preferred_key() -> AgentKey {
    key(PREFERRED_RUNTIME)
}

/// The starter a launch that names no agent runs: the preferred runtime's unattended
/// starter for a background launch nobody watches (it must never sit on a prompt), else
/// its ordinary starter.
pub fn default_key(headless: bool) -> AgentKey {
    default_key_for(PREFERRED_RUNTIME, headless)
}

/// [`default_key`] on `runtime`.
pub fn default_key_for(runtime: AgentCli, headless: bool) -> AgentKey {
    if headless {
        unattended_key(runtime)
    } else {
        key(runtime)
    }
}

/// The starter's revision: the app version as one number (`major·10⁶ +
/// minor·10³ + patch`), so a launch's `agent@revision` says which build's
/// starter it ran. A pre-release or build suffix is not in the number; the
/// definition hash, which covers the whole version, tells those apart.
pub fn revision() -> u32 {
    static REVISION: OnceLock<u32> = OnceLock::new();
    *REVISION.get_or_init(|| revision_of(env!("CARGO_PKG_VERSION")))
}

/// [`revision`] for the version `version`. It rises with every release while
/// minor and patch stay under 1000 (a larger one is held at 999 rather than
/// spilling into the next field), and is never 0, which no agent revision is.
fn revision_of(version: &str) -> u32 {
    let core = version.split(['-', '+']).next().unwrap_or_default();
    let mut parts = core.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let mut next = || parts.next().unwrap_or(0);
    let (major, minor, patch) = (next(), next().min(999), next().min(999));
    major
        .saturating_mul(1_000_000)
        .saturating_add(minor * 1_000 + patch)
        .max(1)
}

/// Whether `key` names a starter, without building it.
pub fn is_starter(key: &AgentKey) -> bool {
    key.is_builtin()
        && AgentCli::ALL.iter().any(|r| {
            r.as_str() == key.id || key.id.strip_suffix(UNATTENDED_SUFFIX) == Some(r.as_str())
        })
}

/// The starter's definition, exactly as an agent file would hold it.
pub(crate) fn definition(runtime: AgentCli) -> Agent {
    let starter = &super::adapter::desktop(runtime).starter;
    Agent {
        id: runtime.as_str().to_string(),
        name: starter.name.to_string(),
        description: starter.description.to_string(),
        created_by: CREATOR.to_string(),
        runtime,
        args: Vec::new(),
        launch: LaunchMode::Foreground,
        mcp: McpAccess::default(),
        startup: vec![descriptor(runtime).root_doc.as_str().to_string()],
        // SW-XS §12.2: no explicit context, so the runtime's (or the team
        // workspace's) default applies where it is launched.
        context: None,
        permissions: None,
        default_prompt: String::new(),
        avatar: None,
        role: None,
        allow_wake: false,
        resume_after_restart: false,
        budget_usd: None,
        revision: revision(),
        archived_at: None,
        extra: Default::default(),
        env: Default::default(),
    }
}

/// The unattended starter's definition: the runtime's starter with every permission
/// check off, for work nobody is watching.
pub(crate) fn unattended_definition(runtime: AgentCli) -> Agent {
    let desktop = super::adapter::desktop(runtime);
    let unattended = &desktop.unattended;
    let args = if unattended.bypass_flag {
        vec![descriptor(runtime).bypass_flag.clone()]
    } else {
        Vec::new()
    };
    Agent {
        id: unattended_key(runtime).id,
        name: desktop.starter.unattended_name.to_string(),
        description: desktop.starter.unattended_description.to_string(),
        args,
        permissions: Some(unattended.permissions.clone()),
        ..definition(runtime)
    }
}

/// The starter `key` names, if it names one.
pub fn get(key: &AgentKey) -> Option<Agent> {
    if !is_starter(key) {
        return None;
    }
    AgentCli::ALL.into_iter().find_map(|runtime| {
        if runtime.as_str() == key.id {
            Some(definition(runtime))
        } else if key.id.strip_suffix(UNATTENDED_SUFFIX) == Some(runtime.as_str()) {
            Some(unattended_definition(runtime))
        } else {
            None
        }
    })
}

/// Every starter, with its key: the runtime starters, then their unattended twins.
pub fn all() -> Vec<(AgentKey, Agent)> {
    AgentCli::ALL
        .into_iter()
        .map(|runtime| (key(runtime), definition(runtime)))
        .chain(
            AgentCli::ALL
                .into_iter()
                .map(|runtime| (unattended_key(runtime), unattended_definition(runtime))),
        )
        .collect()
}

/// The hash a starter's `agent@revision` carries: its definition's YAML, as
/// for any saved agent, followed by the full app version that ships it.
pub fn definition_hash(agent: &Agent) -> String {
    let yaml = serde_yaml::to_string(agent).expect("an agent definition serializes to YAML");
    content_hash(format!("{yaml}# codesense {}\n", env!("CARGO_PKG_VERSION")).as_bytes())
}

#[cfg(test)]
mod tests;
