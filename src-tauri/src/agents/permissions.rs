// SPDX-License-Identifier: MIT
//! The agent's native CLI permissions turned into the per-launch settings the
//! runtime reads, merged with what CodeSense itself needs.
//!
//! CodeSense never interprets, removes or rewrites a rule, and never enforces
//! a mode: the agent's rules pass through verbatim and the CLI enforces them.
//! CodeSense only adds the allows it needs ([`CLAUDE_REQUIRED_ALLOW`]). A rule
//! that blocks one of those still applies as written; the launch warnings
//! ([`super::launch_check`]) say so, using [`blocks_required`].
//!
//! Each runtime gets one [`GeneratedTarget::LaunchSettings`] entry, which its
//! descriptor's `launch_settings` delivers:
//!
//! - **Claude** gets the
//!   whole body of the per-launch file passed with `claude --settings <file>`,
//!   holding this agent's rules and the required allows and nothing else, so
//!   two agents launched into one folder never see each other's rules. A
//!   headless launch with no `default_mode` gets `dontAsk`, so a tool outside
//!   the allow list is refused instead of waiting on a prompt nobody can see.
//!   A headless launch of an agent that skips permission checks (its args or
//!   its `defaultMode` say so) also gets `skipDangerousModePermissionPrompt`,
//!   so Claude's one-time bypass-mode acknowledgement never blocks a session
//!   nobody sees; a foreground launch still shows it.
//! - **Codex** gets the `approval_policy` and `sandbox_mode` keys, each passed as
//!   its own `-c` override. A headless launch gets `approval_policy =
//!   "never"` and `sandbox_mode = "workspace-write"` for whichever of the two
//!   the agent leaves unset: it never waits on an approval nobody can see, and
//!   it can write only inside its workspace. A foreground launch gets no
//!   default keys (the CLI's own interactive default). A value the agent did
//!   set is used verbatim.
//!
//! The entries are generated launch files: never published back and never
//! written into the published context or a shared settings file.

use serde_json::{json, Map, Value};

use super::manifest::{json_hash, GeneratedEntry, GeneratedTarget};
use super::model::{Agent, LaunchMode, BRIDGE_SERVER};
use codesense_pty_core::agent_cli::AgentCli;

/// The tools every Claude agent is allowed so it can talk to CodeSense through
/// the bridge: post into the UI, ask the user, signal that it is done, and read and
/// answer and react to its inbox when its hooks tell it it has mail (so a headless agent
/// that asks nothing can still read, answer and react to it).
pub const CLAUDE_REQUIRED_ALLOW: &[&str] = &[
    "mcp__codesense__post_agent_message",
    "mcp__codesense__ask_user",
    "mcp__codesense__signal_done",
    "mcp__codesense__read_messages",
    "mcp__codesense__reply",
    "mcp__codesense__react",
];

/// SW-AGR-9 (SPEC §9.7): the routine-run tools a woken session calls,
/// pre-allowed for every Claude agent launch, since any saved agent's session
/// may be woken by one of its routines after it started: read what woke it,
/// publish the run's result, and finish that run without ending the session.
/// Allowed, not required: a rule that blocks one is the agent's choice.
pub const CLAUDE_ROUTINE_ALLOW: &[&str] = &[
    "mcp__codesense__routine_context",
    "mcp__codesense__routine_publish_result",
    "mcp__codesense__routine_done",
];

/// Every allow a Claude launch is generated with before the agent's own:
/// [`CLAUDE_REQUIRED_ALLOW`] then [`CLAUDE_ROUTINE_ALLOW`].
#[cfg(test)]
pub fn claude_generated_allow() -> Vec<&'static str> {
    CLAUDE_REQUIRED_ALLOW
        .iter()
        .chain(CLAUDE_ROUTINE_ALLOW)
        .copied()
        .collect()
}

/// The id of the generated Claude settings entry.
pub const CLAUDE_SETTINGS_ID: &str = "claude-launch-settings";
/// The id of the generated Codex config entry.
pub const CODEX_CONFIG_ID: &str = "codex-scoped-config";
/// The id of the generated OpenCode environment permission entry.
pub const OPENCODE_SETTINGS_ID: &str = "opencode-launch-permissions";

/// The Claude `defaultMode` of a headless launch whose agent sets none.
pub const CLAUDE_HEADLESS_DEFAULT_MODE: &str = "dontAsk";
/// The Codex `approval_policy` of a headless launch whose agent sets none.
pub const CODEX_HEADLESS_APPROVAL_POLICY: &str = "never";
/// The Codex `sandbox_mode` of a headless launch whose agent sets none.
pub const CODEX_HEADLESS_SANDBOX_MODE: &str = "workspace-write";

/// The Claude setting that acknowledges bypass-permissions mode up front, so its one-time
/// confirmation dialog is not shown.
pub const CLAUDE_SKIP_BYPASS_PROMPT: &str = "skipDangerousModePermissionPrompt";

/// The generated launch entries for `agent` running on `runtime` in `mode`: its
/// runtime's launch settings.
pub fn generate(agent: &Agent, runtime: AgentCli, mode: LaunchMode) -> Vec<GeneratedEntry> {
    vec![(super::adapter::desktop(runtime).launch_settings)(
        agent, mode,
    )]
}

pub(super) fn entry(
    id: &str,
    target: GeneratedTarget,
    content: Value,
    required: Vec<String>,
) -> GeneratedEntry {
    GeneratedEntry {
        id: id.into(),
        target,
        hash: json_hash(&content),
        content,
        required,
        path: None,
        merge_keys: Default::default(),
    }
}

/// A Claude agent's launch settings: the whole body of its per-launch settings file.
pub(super) fn claude(agent: &Agent, mode: LaunchMode) -> GeneratedEntry {
    let mut entry = claude_permissions(
        agent.permissions.as_ref().and_then(|p| p.claude.as_ref()),
        mode,
    );
    if mode == LaunchMode::Headless && agent.is_escalating() {
        entry.content[CLAUDE_SKIP_BYPASS_PROMPT] = json!(true);
        entry.hash = json_hash(&entry.content);
    }
    entry
}

fn claude_permissions(
    own: Option<&super::model::ClaudePermissions>,
    mode: LaunchMode,
) -> GeneratedEntry {
    let required: Vec<String> = CLAUDE_REQUIRED_ALLOW
        .iter()
        .map(|s| s.to_string())
        .collect();

    let mut allow = required.clone();
    allow.extend(CLAUDE_ROUTINE_ALLOW.iter().map(|s| s.to_string()));
    let mut deny = Vec::new();
    let mut body = Map::new();
    if let Some(own) = own {
        let preset = allow.clone();
        allow.extend(own.allow.iter().filter(|r| !preset.contains(r)).cloned());
        deny.clone_from(&own.deny);
        if !own.ask.is_empty() {
            body.insert("ask".into(), json!(own.ask));
        }
        if !own.additional_directories.is_empty() {
            body.insert(
                "additionalDirectories".into(),
                json!(own.additional_directories),
            );
        }
        if let Some(v) = &own.disable_bypass_permissions_mode {
            body.insert("disableBypassPermissionsMode".into(), json!(v));
        }
    }
    body.insert("allow".into(), json!(allow));
    body.insert("deny".into(), json!(deny));
    let default_mode = own
        .and_then(|p| p.default_mode.clone())
        .or_else(|| (mode == LaunchMode::Headless).then(|| CLAUDE_HEADLESS_DEFAULT_MODE.into()));
    if let Some(m) = default_mode {
        body.insert("defaultMode".into(), json!(m));
    }

    entry(
        CLAUDE_SETTINGS_ID,
        GeneratedTarget::LaunchSettings,
        json!({ "permissions": Value::Object(body) }),
        required,
    )
}

/// Whether a Claude `deny` or `ask` rule would block a required tool: the
/// tool itself, the bridge server as a whole (`mcp__codesense`,
/// `mcp__codesense__*`), or every tool (`*`). Only ever used to warn.
pub fn blocks_required(rule: &str) -> bool {
    let server = format!("mcp__{BRIDGE_SERVER}");
    let rule = rule.trim();
    CLAUDE_REQUIRED_ALLOW.contains(&rule)
        || rule == "*"
        || rule == server
        || rule == format!("{server}__*")
}

/// A Codex agent's launch settings: its approval and sandbox keys, each passed as its own
/// config override.
pub(super) fn codex(agent: &Agent, mode: LaunchMode) -> GeneratedEntry {
    let own = agent.permissions.as_ref().and_then(|p| p.codex.as_ref());
    let headless = |default: &str| (mode == LaunchMode::Headless).then(|| default.to_string());
    let approval = own
        .and_then(|p| p.approval_policy.clone())
        .or_else(|| headless(CODEX_HEADLESS_APPROVAL_POLICY));
    let sandbox = own
        .and_then(|p| p.sandbox_mode.clone())
        .or_else(|| headless(CODEX_HEADLESS_SANDBOX_MODE));
    let mut body = Map::new();
    if let Some(v) = approval {
        body.insert("approval_policy".into(), json!(v));
    }
    if let Some(v) = sandbox {
        body.insert("sandbox_mode".into(), json!(v));
    }
    entry(
        CODEX_CONFIG_ID,
        GeneratedTarget::LaunchSettings,
        Value::Object(body),
        Vec::new(),
    )
}

/// OpenCode receives its native ordered permission rules in `OPENCODE_CONFIG_CONTENT`. Defaults
/// are inserted only when absent, so an agent's explicit tool decisions win. A headless
/// launch starts from the adapter's `headless_settings` (`"*":"deny"` with the read-only
/// tools allowed), because `opencode run` auto-rejects the first `ask` and stops with no
/// answer; a foreground launch starts from `"*":"ask"`. An escalating agent (`--auto` or an
/// all-allow map) starts from the same map with only `"*"` set to `allow`: its own denies
/// still win key by key.
pub(super) fn opencode(agent: &Agent, mode: LaunchMode) -> GeneratedEntry {
    let mut permissions = agent
        .permissions
        .as_ref()
        .and_then(|p| p.opencode.as_ref())
        .map(|p| p.permission.clone())
        .unwrap_or_default();
    if mode == LaunchMode::Headless {
        let base = (super::adapter::desktop(agent.runtime).headless_settings)();
        let escalated = agent.is_escalating();
        for (pattern, decision) in base.as_object().into_iter().flatten() {
            // `--auto` does not override a `"*": "deny"`, so an escalating agent's base
            // is the one adapter map with only its catch-all opened.
            let decision = if escalated && pattern == "*" {
                Some("allow")
            } else {
                decision.as_str()
            };
            if let Some(decision) = decision {
                permissions
                    .entry(pattern.clone())
                    .or_insert_with(|| decision.into());
            }
        }
    }
    permissions
        .entry("*".into())
        .or_insert_with(|| "ask".into());
    // Omitted/null `bridge_tools` leaves the bridge unrestricted, so its tools follow the
    // base rule; only a declared allowlist closes the namespace (the launch's MCP scope
    // then writes the per-tool grants).
    if agent.mcp.bridge_tools.is_some() {
        permissions
            .entry("codesense_*".into())
            .or_insert_with(|| "deny".into());
    }
    for pattern in bridge_tool_allow_patterns() {
        permissions.entry(pattern).or_insert_with(|| "allow".into());
    }
    entry(
        OPENCODE_SETTINGS_ID,
        GeneratedTarget::LaunchSettings,
        super::adapter::native_permission_rules(&json!(permissions)),
        Vec::new(),
    )
}

/// Hermes native settings. Saved denies and bridge restrictions remain under yolo.
pub(super) fn hermes(agent: &Agent, mode: LaunchMode) -> GeneratedEntry {
    let desktop = super::adapter::desktop(agent.runtime);
    let own = agent.permissions.as_ref().and_then(|p| p.hermes.as_ref());
    let mut content = if mode == LaunchMode::Headless && !agent.is_escalating() {
        (desktop.headless_settings)()
    } else {
        super::adapter::hermes::settings(false)
    };
    // Only the admitted model selectors from startup_settings are projected by home_config.
    if let Some(startup) = agent.startup_settings() {
        let mut model = Map::new();
        super::adapter::hermes::merge_model(&startup, &mut model);
        if !serde_json::Map::is_empty(&model) {
            content["model"] = json!(model);
        }
    }
    if own.and_then(|p| p.approval.as_deref()) == Some("yolo") {
        content["approvals"]["mode"] = json!("off");
    }
    if let Some(toolsets) = own.and_then(|p| p.toolsets.as_ref()) {
        content["platform_toolsets"]["cli"] = json!(toolsets);
    }
    let mut blocks = content["hooks"]["pre_tool_call"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for pattern in own.into_iter().flat_map(|p| &p.deny) {
        let block = super::adapter::hermes::block_hook(pattern);
        if !blocks.contains(&block) {
            blocks.push(block);
        }
    }
    if let Some(tools) = &agent.mcp.bridge_tools {
        let names = tools
            .iter()
            .map(|name| regex::escape(name))
            .collect::<Vec<_>>()
            .join("|");
        let pattern = if std::string::String::is_empty(&names) {
            "^mcp__codesense__.*".into()
        } else {
            format!("^mcp__codesense__(?!({names})$).*")
        };
        blocks.push(super::adapter::hermes::block_hook(&pattern));
    }
    if !std::vec::Vec::is_empty(&blocks) {
        content["hooks"] = json!({"pre_tool_call": blocks});
    }
    entry(
        "hermes-launch-settings",
        GeneratedTarget::LaunchSettings,
        content,
        Vec::new(),
    )
}

/// The OpenCode permission patterns of the bridge tools every agent is allowed, and of the
/// routine-run tools a woken session calls, which are the ones a Claude launch is allowed too.
pub(super) fn bridge_tool_allow_patterns() -> Vec<String> {
    let routine_tools = CLAUDE_ROUTINE_ALLOW
        .iter()
        .filter_map(|tool| tool.strip_prefix("mcp__codesense__"));
    [
        "who_am_i",
        "list_sessions",
        "signal_progress",
        "signal_done",
        "ask_user",
        "post_agent_message",
        "read_messages",
        "reply",
    ]
    .into_iter()
    .chain(routine_tools)
    .map(|name| format!("codesense_{name}"))
    .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::agents::model::{ClaudePermissions, CodexPermissions, McpAccess, Permissions};

    #[test]
    fn hermes_modes_share_defaults_and_only_headless_denies_gated_tools() {
        let agent = agent(AgentCli::Hermes, None);
        let foreground = hermes(&agent, LaunchMode::Foreground).content;
        let mut headless = hermes(&agent, LaunchMode::Headless).content;
        assert_eq!(
            headless,
            (super::super::adapter::desktop(AgentCli::Hermes).headless_settings)()
        );
        assert_eq!(
            headless["hooks"]["pre_tool_call"][1]["matcher"],
            "^(terminal|write_file|patch|computer_use)$"
        );
        headless["hooks"]["pre_tool_call"]
            .as_array_mut()
            .unwrap()
            .remove(1);
        assert_eq!(headless, foreground);
    }

    #[test]
    fn hermes_bridge_native_fullmatch_blocks_each_excluded_inner_name_and_allows_control() {
        let mut agent = agent(AgentCli::Hermes, None);
        for allowed in [vec!["allowed".into()], Vec::new()] {
            agent.mcp.bridge_tools = Some(allowed.clone());
            let settings = hermes(&agent, LaunchMode::Foreground).content;
            let pattern = settings["hooks"]["pre_tool_call"]
                .as_array()
                .unwrap()
                .iter()
                .find(|block| {
                    block["matcher"]
                        .as_str()
                        .unwrap()
                        .starts_with("^mcp__codesense__")
                })
                .unwrap()["matcher"]
                .as_str()
                .unwrap();
            // Hermes uses Python re.fullmatch, including after deferred tool_call unwrapping.
            let result = std::process::Command::new("python3").args(["-I", "-c",
                "import re,sys,json; p=re.compile(sys.argv[1]); allowed=json.loads(sys.argv[2]); assert all(p.fullmatch('mcp__codesense__'+n) for n in ['excluded_read','excluded_write']); assert bool(p.fullmatch('mcp__codesense__allowed')) == (not allowed); assert not p.fullmatch('tool_call'); assert not p.fullmatch('mcp__other__read')",
                pattern, &serde_json::to_string(&allowed).unwrap()]).output().unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
    }

    #[test]
    fn hermes_saved_permissions_keep_denies_and_bridge_floor_under_yolo() {
        let mut agent = agent(
            AgentCli::Hermes,
            Some(Permissions {
                hermes: Some(super::super::model::HermesPermissions {
                    approval: Some("yolo".into()),
                    toolsets: Some(vec!["file".into()]),
                    deny: vec!["^write_file$".into()],
                    ..Default::default()
                }),
                ..Default::default()
            }),
        );
        agent.mcp.bridge_tools = Some(vec!["who_am_i".into()]);
        for mode in [LaunchMode::Foreground, LaunchMode::Headless] {
            let settings = hermes(&agent, mode).content;
            assert_eq!(settings["approvals"]["mode"], "off");
            assert_eq!(settings["platform_toolsets"]["cli"], json!(["file"]));
            let blocks = settings["hooks"]["pre_tool_call"].as_array().unwrap();
            assert_eq!(blocks.len(), 3);
            assert_eq!(
                blocks[0]["matcher"],
                "^(memory|skill_manage|cronjob_manage)$"
            );
            assert_eq!(blocks[1]["matcher"], "^write_file$");
            assert_eq!(blocks[2]["matcher"], "^mcp__codesense__(?!(who_am_i)$).*");
            assert!(blocks.iter().all(|block| block["fail_closed"] == true));
        }
        agent.permissions = None;
        agent.args = vec!["--yolo".into()];
        assert_eq!(
            hermes(&agent, LaunchMode::Headless).content["hooks"]["pre_tool_call"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        agent.mcp.bridge_tools = Some(Vec::new());
        assert_eq!(
            hermes(&agent, LaunchMode::Headless).content["hooks"]["pre_tool_call"][1]["matcher"],
            "^mcp__codesense__.*"
        );
    }

    fn agent(runtime: AgentCli, permissions: Option<Permissions>) -> Agent {
        Agent {
            role: None,
            id: "a".into(),
            name: "A".into(),
            description: String::new(),
            created_by: "1".into(),
            runtime,
            args: Vec::new(),
            launch: LaunchMode::Foreground,
            mcp: McpAccess::default(),
            startup: Vec::new(),
            context: None,
            permissions,
            default_prompt: String::new(),
            avatar: None,
            allow_wake: false,
            resume_after_restart: false,
            budget_usd: None,
            revision: 1,
            archived_at: None,
            extra: BTreeMap::new(),
            env: Default::default(),
        }
    }

    fn claude_perms(p: ClaudePermissions) -> Option<Permissions> {
        Some(Permissions {
            claude: Some(p),
            ..Permissions::default()
        })
    }

    fn codex_perms(p: CodexPermissions) -> Option<Permissions> {
        Some(Permissions {
            codex: Some(p),
            ..Permissions::default()
        })
    }

    fn native_map(config: &Value) -> Value {
        let rules = config["permissions"]
            .as_array()
            .expect("native ordered rules");
        assert!(rules.iter().all(|rule| rule["resource"] == "*"));
        json!(rules
            .iter()
            .map(|rule| (
                rule["action"].as_str().unwrap().to_owned(),
                rule["effect"].clone()
            ))
            .collect::<serde_json::Map<_, _>>())
    }

    #[test]
    fn opencode_permission_defaults_are_inserted_only_when_absent() {
        let generated = one(&agent(AgentCli::OpenCode, None), LaunchMode::Foreground);
        let permissions = native_map(&generated.content);
        assert_eq!(permissions["*"], "ask");
        assert!(permissions.get("codesense_*").is_none());
        assert_eq!(permissions["codesense_who_am_i"], "allow");
        for tool in ["routine_context", "routine_publish_result", "routine_done"] {
            assert_eq!(permissions[format!("codesense_{tool}")], "allow", "{tool}");
        }
        let parsed: serde_json::Value =
            native_map(&serde_json::from_str::<Value>(&generated.content.to_string()).unwrap());
        assert_eq!(parsed["*"], "ask");
        assert!(parsed.get("permission").is_none());

        let mut permission = BTreeMap::new();
        permission.insert("*".into(), "deny".into());
        permission.insert("codesense_ask_user".into(), "deny".into());
        let custom = agent(
            AgentCli::OpenCode,
            Some(Permissions {
                opencode: Some(crate::agents::model::OpenCodePermissions {
                    permission,
                    ..Default::default()
                }),
                ..Default::default()
            }),
        );
        let generated = one(&custom, LaunchMode::Foreground);
        assert_eq!(native_map(&generated.content)["*"], "deny");
        assert_eq!(native_map(&generated.content)["codesense_ask_user"], "deny");
        assert_eq!(
            native_map(&generated.content)["codesense_who_am_i"],
            "allow"
        );
    }

    #[test]
    fn opencode_routine_allows_cover_what_claude_gets_and_an_explicit_decision_wins() {
        let mut permission = BTreeMap::new();
        permission.insert("codesense_routine_done".into(), "deny".into());
        let custom = agent(
            AgentCli::OpenCode,
            Some(Permissions {
                opencode: Some(crate::agents::model::OpenCodePermissions {
                    permission,
                    ..Default::default()
                }),
                ..Default::default()
            }),
        );
        for mode in [LaunchMode::Foreground, LaunchMode::Headless] {
            let generated = one(&custom, mode);
            assert_eq!(
                native_map(&generated.content)["codesense_routine_done"],
                "deny"
            );
            for tool in CLAUDE_ROUTINE_ALLOW {
                let name = tool.replace("mcp__codesense__", "codesense_");
                if name != "codesense_routine_done" {
                    assert_eq!(
                        native_map(&generated.content)[name.as_str()],
                        "allow",
                        "{tool}"
                    );
                }
            }
            let base = if mode == LaunchMode::Headless {
                "deny"
            } else {
                "ask"
            };
            assert_eq!(native_map(&generated.content)["*"], base);
        }
    }

    #[test]
    fn opencode_headless_default_denies_but_allows_read_only_and_codesense_tools() {
        let generated = one(&agent(AgentCli::OpenCode, None), LaunchMode::Headless);
        let map = native_map(&generated.content);
        assert_eq!(map["*"], "deny");
        assert_eq!(map["external_directory"], "deny");
        for tool in ["read", "glob", "grep", "list", "todowrite"] {
            assert_eq!(map[tool], "allow", "{tool}");
        }
        assert_eq!(map["codesense_signal_done"], "allow");
        for tool in ["routine_context", "routine_publish_result", "routine_done"] {
            assert_eq!(map[format!("codesense_{tool}")], "allow", "{tool}");
        }
        // The interactive launch is unchanged.
        let interactive = one(&agent(AgentCli::OpenCode, None), LaunchMode::Foreground);
        assert_eq!(native_map(&interactive.content)["*"], "ask");
        assert!(native_map(&interactive.content).get("read").is_none());
        assert!(native_map(&interactive.content)
            .get("external_directory")
            .is_none());
    }

    #[test]
    fn opencode_headless_agent_map_wins_key_by_key() {
        let mut permission = BTreeMap::new();
        permission.insert("*".into(), "allow".into());
        permission.insert("read".into(), "deny".into());
        permission.insert("external_directory".into(), "ask".into());
        let custom = agent(
            AgentCli::OpenCode,
            Some(Permissions {
                opencode: Some(crate::agents::model::OpenCodePermissions {
                    permission,
                    ..Default::default()
                }),
                ..Default::default()
            }),
        );
        let map = native_map(&one(&custom, LaunchMode::Headless).content);
        assert_eq!(map["*"], "allow");
        assert_eq!(map["read"], "deny");
        assert_eq!(map["external_directory"], "ask");
        assert_eq!(map["grep"], "allow");
    }

    fn opencode_with_map(entries: &[(&str, &str)]) -> Agent {
        let permission = entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        agent(
            AgentCli::OpenCode,
            Some(Permissions {
                opencode: Some(crate::agents::model::OpenCodePermissions {
                    permission,
                    ..Default::default()
                }),
                ..Default::default()
            }),
        )
    }

    #[test]
    fn opencode_headless_auto_agent_gets_an_allow_base_with_the_codesense_allows() {
        let mut auto = agent(AgentCli::OpenCode, None);
        auto.args = vec!["--auto".into()];
        let map = native_map(&one(&auto, LaunchMode::Headless).content);
        assert_eq!(map["*"], "allow");
        assert_eq!(map["external_directory"], "deny");
        assert_eq!(map["codesense_routine_done"], "allow");
        // Interactive is unchanged.
        assert_eq!(
            native_map(&one(&auto, LaunchMode::Foreground).content)["*"],
            "ask"
        );
    }

    #[test]
    fn opencode_headless_all_allow_map_agent_gets_an_allow_base() {
        let map =
            native_map(&one(&opencode_with_map(&[("*", "allow")]), LaunchMode::Headless).content);
        assert_eq!(map["*"], "allow");
        assert_eq!(map["codesense_signal_done"], "allow");
    }

    #[test]
    fn opencode_headless_escalated_agent_keeps_its_own_denies() {
        let mut auto = opencode_with_map(&[("shell", "deny")]);
        auto.args = vec!["--auto".into()];
        let map = native_map(&one(&auto, LaunchMode::Headless).content);
        assert_eq!(map["*"], "allow");
        assert_eq!(map["shell"], "deny");
    }

    #[test]
    fn opencode_headless_plain_agent_keeps_the_deny_base() {
        let map = native_map(
            &one(
                &opencode_with_map(&[("shell", "allow")]),
                LaunchMode::Headless,
            )
            .content,
        );
        assert_eq!(map["*"], "deny");
        assert_eq!(map["shell"], "allow");
    }

    #[test]
    fn opencode_headless_unattended_starter_gets_the_allow_base() {
        let starter = crate::agents::builtin::get(&crate::agents::builtin::unattended_key(
            AgentCli::OpenCode,
        ))
        .unwrap();
        let map = native_map(&one(&starter, LaunchMode::Headless).content);
        assert_eq!(map["*"], "allow");
        assert_eq!(map["codesense_signal_done"], "allow");
        assert_eq!(
            native_map(&one(&starter, LaunchMode::Foreground).content)["*"],
            "ask"
        );
    }

    #[test]
    fn opencode_headless_default_is_the_adapters_headless_settings() {
        let port = (crate::agents::adapter::desktop(AgentCli::OpenCode).headless_settings)();
        let generated = one(&agent(AgentCli::OpenCode, None), LaunchMode::Headless);
        for (key, value) in port.as_object().unwrap() {
            assert_eq!(&native_map(&generated.content)[key], value, "{key}");
        }
    }

    #[test]
    fn opencode_denies_the_bridge_namespace_only_for_a_declared_allowlist() {
        let mut restricted = agent(AgentCli::OpenCode, None);
        restricted.mcp.bridge_tools = Some(vec!["notes_read".into()]);
        let generated = one(&restricted, LaunchMode::Foreground);
        assert_eq!(native_map(&generated.content)["codesense_*"], "deny");
        assert_eq!(
            native_map(&generated.content)["codesense_who_am_i"],
            "allow"
        );
        assert!(generated.content.get("codesense_notes_read").is_none());

        let mut empty = agent(AgentCli::OpenCode, None);
        empty.mcp.bridge_tools = Some(Vec::new());
        assert_eq!(
            native_map(&one(&empty, LaunchMode::Foreground).content)["codesense_*"],
            "deny"
        );
    }

    #[test]
    fn a_headless_launch_that_skips_permissions_acknowledges_bypass_mode_up_front() {
        let by_mode = claude_perms(ClaudePermissions {
            default_mode: Some("bypassPermissions".into()),
            ..ClaudePermissions::default()
        });
        let by_flag = Agent {
            args: vec!["--dangerously-skip-permissions".into()],
            ..agent(AgentCli::Claude, None)
        };
        for a in [agent(AgentCli::Claude, by_mode), by_flag] {
            let headless = one(&a, LaunchMode::Headless);
            assert_eq!(headless.content[CLAUDE_SKIP_BYPASS_PROMPT], json!(true));
            assert_eq!(headless.hash, json_hash(&headless.content));
            // A foreground launch still shows the acknowledgement to the person.
            let foreground = one(&a, LaunchMode::Foreground);
            assert!(foreground.content.get(CLAUDE_SKIP_BYPASS_PROMPT).is_none());
        }
        // An agent that asks for permission never gets it.
        let asking = one(&agent(AgentCli::Claude, None), LaunchMode::Headless);
        assert!(asking.content.get(CLAUDE_SKIP_BYPASS_PROMPT).is_none());
    }

    fn one(agent: &Agent, mode: LaunchMode) -> GeneratedEntry {
        let entries = generate(agent, agent.runtime, mode);
        assert_eq!(entries.len(), 1);
        entries.into_iter().next().unwrap()
    }

    fn golden(entry: &GeneratedEntry, content: &str, hash: &str) {
        assert_eq!(
            serde_json::to_string(&entry.content).unwrap(),
            content,
            "content"
        );
        assert_eq!(entry.hash, json_hash(&entry.content));
        assert_eq!(entry.hash, hash, "hash");
    }

    #[test]
    fn claude_foreground_without_permissions_is_the_required_allows_only() {
        let e = one(&agent(AgentCli::Claude, None), LaunchMode::Foreground);
        assert_eq!(e.id, CLAUDE_SETTINGS_ID);
        assert_eq!(e.target, GeneratedTarget::LaunchSettings);
        assert_eq!(e.required, CLAUDE_REQUIRED_ALLOW);
        golden(
            &e,
            r#"{"permissions":{"allow":["mcp__codesense__post_agent_message","mcp__codesense__ask_user","mcp__codesense__signal_done","mcp__codesense__read_messages","mcp__codesense__reply","mcp__codesense__react","mcp__codesense__routine_context","mcp__codesense__routine_publish_result","mcp__codesense__routine_done"],"deny":[]}}"#,
            "blake3:99172fa30ac98ba285c6ddfd6904124b990e1cafd5e05860c80dafc2e1d7db8d",
        );
    }

    #[test]
    fn claude_headless_merges_the_agents_rules_and_its_mode() {
        let a = agent(
            AgentCli::Claude,
            claude_perms(ClaudePermissions {
                allow: vec![
                    "Bash(git status)".into(),
                    "mcp__codesense__signal_done".into(),
                ],
                deny: vec!["WebFetch".into()],
                default_mode: Some("acceptEdits".into()),
                ..ClaudePermissions::default()
            }),
        );
        golden(
            &one(&a, LaunchMode::Headless),
            r#"{"permissions":{"allow":["mcp__codesense__post_agent_message","mcp__codesense__ask_user","mcp__codesense__signal_done","mcp__codesense__read_messages","mcp__codesense__reply","mcp__codesense__react","mcp__codesense__routine_context","mcp__codesense__routine_publish_result","mcp__codesense__routine_done","Bash(git status)"],"defaultMode":"acceptEdits","deny":["WebFetch"]}}"#,
            "blake3:52d68834a15fbb323b36bfc6843f2a7c64037db8bdd53be225e60eaee3161c05",
        );
    }

    #[test]
    fn claude_headless_without_a_mode_does_not_ask_and_foreground_sets_none() {
        let a = agent(AgentCli::Claude, None);
        let headless = one(&a, LaunchMode::Headless);
        assert_eq!(
            headless.content["permissions"]["defaultMode"],
            CLAUDE_HEADLESS_DEFAULT_MODE
        );
        let foreground = one(&a, LaunchMode::Foreground);
        assert!(foreground.content["permissions"]
            .get("defaultMode")
            .is_none());
    }

    #[test]
    fn claude_passes_the_other_documented_keys_through_verbatim() {
        let a = agent(
            AgentCli::Claude,
            claude_perms(ClaudePermissions {
                ask: vec!["Bash(rm:*)".into()],
                additional_directories: vec!["../shared".into()],
                disable_bypass_permissions_mode: Some("disable".into()),
                ..ClaudePermissions::default()
            }),
        );
        let p = one(&a, LaunchMode::Foreground).content["permissions"].clone();
        assert_eq!(p["ask"], json!(["Bash(rm:*)"]));
        assert_eq!(p["additionalDirectories"], json!(["../shared"]));
        assert_eq!(p["disableBypassPermissionsMode"], "disable");
    }

    #[test]
    fn rules_over_required_tools_pass_through_verbatim() {
        let deny = vec![
            "mcp__codesense__signal_done".to_string(),
            "WebFetch".into(),
            "mcp__codesense".into(),
            "mcp__codesense__*".into(),
            "*".into(),
            "mcp__codesense__gs_publish".into(),
        ];
        let ask = vec!["mcp__codesense__ask_user".to_string(), "Bash".into()];
        let a = agent(
            AgentCli::Claude,
            claude_perms(ClaudePermissions {
                deny: deny.clone(),
                ask: ask.clone(),
                ..ClaudePermissions::default()
            }),
        );
        let p = one(&a, LaunchMode::Headless).content["permissions"].clone();
        assert_eq!(p["deny"], json!(deny));
        assert_eq!(p["ask"], json!(ask));
        // The required allows are still added; the CLI decides between them.
        assert_eq!(p["allow"], json!(claude_generated_allow()));
    }

    #[test]
    fn a_rule_blocks_a_required_tool_only_when_it_covers_one() {
        for rule in [
            "mcp__codesense__signal_done",
            "mcp__codesense__ask_user",
            "mcp__codesense__post_agent_message",
            "mcp__codesense",
            "mcp__codesense__*",
            "*",
            " mcp__codesense ",
        ] {
            assert!(blocks_required(rule), "{rule}");
        }
        for rule in [
            "mcp__codesense__gs_publish",
            "mcp__codesense_other",
            "mcp__slack__*",
            "Bash",
            "WebFetch",
        ] {
            assert!(!blocks_required(rule), "{rule}");
        }
    }

    #[test]
    fn codex_headless_without_a_policy_never_asks_and_stays_in_its_workspace() {
        let e = one(&agent(AgentCli::Codex, None), LaunchMode::Headless);
        assert_eq!(e.id, CODEX_CONFIG_ID);
        assert_eq!(e.target, GeneratedTarget::LaunchSettings);
        assert_eq!(e.required, Vec::<String>::new());
        golden(
            &e,
            r#"{"approval_policy":"never","sandbox_mode":"workspace-write"}"#,
            "blake3:8a85e7faeed8c17296ff4753fd15cb6c83747c8ad791cef4de5e7855d363674d",
        );
        let foreground = one(&agent(AgentCli::Codex, None), LaunchMode::Foreground);
        assert_eq!(foreground.content, json!({}));
    }

    #[test]
    fn codex_uses_an_explicit_policy_verbatim() {
        let a = agent(
            AgentCli::Codex,
            codex_perms(CodexPermissions {
                approval_policy: Some("on-request".into()),
                sandbox_mode: None,
                ..CodexPermissions::default()
            }),
        );
        golden(
            &one(&a, LaunchMode::Foreground),
            r#"{"approval_policy":"on-request"}"#,
            "blake3:04d45efdd8aabd0e698d6a055adf95e99de2f1a81c893fe9625b13dd2aedffe9",
        );
        // Headless fills only the key the agent left unset.
        assert_eq!(
            one(&a, LaunchMode::Headless).content,
            json!({"approval_policy": "on-request", "sandbox_mode": CODEX_HEADLESS_SANDBOX_MODE})
        );
        let sandbox_only = agent(
            AgentCli::Codex,
            codex_perms(CodexPermissions {
                sandbox_mode: Some("read-only".into()),
                ..CodexPermissions::default()
            }),
        );
        assert_eq!(
            one(&sandbox_only, LaunchMode::Headless).content,
            json!({"approval_policy": CODEX_HEADLESS_APPROVAL_POLICY, "sandbox_mode": "read-only"})
        );
    }

    #[test]
    fn only_the_runtimes_own_block_applies() {
        let a = agent(
            AgentCli::Codex,
            Some(Permissions {
                claude: Some(ClaudePermissions {
                    deny: vec!["mcp__codesense__signal_done".into()],
                    ..ClaudePermissions::default()
                }),
                ..Permissions::default()
            }),
        );
        let entries = generate(&a, AgentCli::Codex, LaunchMode::Foreground);
        assert_eq!(entries[0].target, GeneratedTarget::LaunchSettings);
    }
}
