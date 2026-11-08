// SPDX-License-Identifier: MIT
use super::schema::{args_schema, result_schema};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Class {
    Read,
    Metadata,
    Prefs,
    Spawn,
    Send,
    Manage,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerbSpec {
    pub name: String,
    pub class: Class,
    pub args_schema: Value,
    pub result_schema: Value,
    pub grant: String,
    pub inbox_target: bool,
    pub confirmable: bool,
    pub mcp_twin: Option<String>,
    pub presentation_only_reason: Option<String>,
    pub audited: bool,
    pub action: bool,
    pub module: String,
}
pub struct Registry {
    specs: BTreeMap<String, VerbSpec>,
}
impl Registry {
    pub fn register(&mut self, module: &str, spec: VerbSpec) {
        assert!(
            [
                "sessions",
                "inbox",
                "records",
                "processes",
                "agent",
                "commands",
                "files"
            ]
            .contains(&module),
            "unknown resource module"
        );
        assert!(
            module == "agent" || spec.name.starts_with(&format!("{module}.")),
            "new verbs use resource namespaces"
        );
        self.register_existing(module, spec);
    }
    fn register_existing(&mut self, module: &str, mut spec: VerbSpec) {
        spec.module = module.into();
        assert!(
            super::capabilities::known(&spec.grant),
            "unknown capability {:?}",
            spec.grant
        );
        assert!(spec.mcp_twin.is_some() || spec.presentation_only_reason.is_some());
        assert!(!self.specs.contains_key(&spec.name), "duplicate verb");
        self.specs.insert(spec.name.clone(), spec);
    }
    pub fn get(&self, name: &str) -> Option<&VerbSpec> {
        self.specs.get(name)
    }
    pub fn all(&self) -> impl Iterator<Item = &VerbSpec> {
        self.specs.values()
    }
}
impl Default for Registry {
    fn default() -> Self {
        let mut registry = Self {
            specs: BTreeMap::new(),
        };
        register_resources(&mut registry);
        agent::register(&mut registry);
        for operation in [
            "collections",
            "list",
            "read",
            "search",
            "history",
            "readAt",
            "create",
            "write",
            "setFields",
            "delete",
            "restore",
        ] {
            let name = format!("records.{operation}");
            let twin = format!(
                "records_{}",
                match operation {
                    "readAt" => "read_at",
                    "setFields" => "set_fields",
                    other => other,
                }
            );
            let write = crate::records::binding::is_write(operation);
            registry.register(
                "records",
                spec(
                    &name,
                    if write { Class::Metadata } else { Class::Read },
                    operation == "delete",
                    Some(&twin),
                    None,
                    write,
                    write,
                ),
            );
        }
        registry
    }
}
fn spec(
    name: &str,
    class: Class,
    confirmable: bool,
    mcp_twin: Option<&str>,
    reason: Option<&str>,
    action: bool,
    audited: bool,
) -> VerbSpec {
    VerbSpec {
        name: name.into(),
        class,
        args_schema: args_schema(name),
        result_schema: result_schema(name),
        grant: capability_for_verb(name).into(),
        inbox_target: matches!(
            name,
            "inbox"
                | "roles"
                | "openMessage"
                | "embedMessage"
                | "loadMedia"
                | "markRead"
                | "markUnread"
                | "archive"
                | "unarchive"
                | "sendAsHuman"
                | "replyAsHuman"
                | "react"
                | "updates"
                | "humanStatus"
        ),
        confirmable,
        mcp_twin: mcp_twin.map(str::to_owned),
        presentation_only_reason: reason.map(str::to_owned),
        audited,
        action,
        module: String::new(),
    }
}

fn capability_for_verb(name: &str) -> &'static str {
    if let Some(operation) = name.strip_prefix("records.") {
        return crate::records::binding::capability(
            crate::records::Scope::Private,
            crate::records::binding::is_write(operation),
        );
    }
    match name {
        "agent" | "agents" | "gotoAgent" | "sessions" | "sessionRead" | "whyLine" | "storyChanges" | "storyTotals" | "sessionUsage" | "modelRates" | "canPasteToTerminal" | "prefs"
        | "setPrefs" | "setUiState" | "subscribe" | "unsubscribe" | "page" | "moveWorkspace"
        | "openLauncher" | "openRoutineDraft" | "launchUnder" | "dismiss_agent_mark"
        | "showMenu" | "showPopover" | "focusOut" | "focusTerminal" | "commands.register" => {
            "sessions:read"
        }
        "openDiff" => "repo:read",
        "commands.list" | "commands.run" => "commands:run",
        "sendPrompt" | "sendToSession" | "sendToWorkspaceTab" | "runPrompt" => "sessions:send",
        "closeTerminalTab" | "closeWorkspace" | "stopSession" => "sessions:stop",
        "openTab" | "openWorkspace" | "runSkill" => "launch",
        "humanStatus" | "inbox" | "roles" | "updates" | "watchInbox" | "watchAgentInbox"
        | "watchAgentActivity" | "openMessage" | "embedMessage" | "openAgentInbox" => {
            "inbox:read:team"
        }
        "inboxRollup" => "inbox:read:all-teams",
        "loadMedia" | "archive" | "unarchive" | "markRead" | "markUnread" | "postMessage"
        | "replyAsHuman" | "react" | "sendAsHuman" | "setHumanStatus" => "inbox:write",
        "analyze" | "context" | "getDocDir" | "getEpicHandle" | "openFile" | "queryContext"
        | "validateYaml" => "context:read",
        "draft" | "routeToContext" | "setField" | "setStatus" => "context:write",
        "readWorkspace" | "files.list" | "files.read" => "files:read",
        "files.move" | "files.delete" | "files.create" => "files:write",
        "routine" | "routineResult" | "routineRun" | "routineRuns" | "routineStatus"
        | "routines" | "run" | "runEvidence" | "runWorkProduct" | "runs" => "routines:read",
        "routineRefresh" => "routines:run",
        "routineCancel" => "routines:manage",
        "getWorkspaceStatus" | "setup" | "worktrees" => "repo:read",
        "getDependencyStatus" | "machines" => "machines:read",
        "setModelRate" => "accounts:manage",
        _ => panic!("no capability for verb {name}"),
    }
}

impl VerbSpec {
    pub fn required_capability(&self, caller: &super::Caller, args: &Value) -> &str {
        if matches!(self.name.as_str(), "subscribe" | "unsubscribe" | "page")
            && args["topic"] == "files"
        {
            return "files:read";
        }
        if self.name.starts_with("records.")
            || (matches!(self.name.as_str(), "subscribe" | "unsubscribe" | "page")
                && args["topic"] == "records")
        {
            let scope = caller
                .records_binding
                .as_ref()
                .and_then(|b| {
                    b.resolve(args["collection"].as_str().unwrap_or("notes"))
                        .ok()
                })
                .map(|s| s.collection.scope)
                .unwrap_or(crate::records::Scope::Private);
            return crate::records::binding::capability(scope, self.class == Class::Metadata);
        }
        if matches!(self.name.as_str(), "subscribe" | "unsubscribe" | "page")
            && args["topic"] == "inbox"
        {
            if caller.scope == "team" {
                "inbox:read:team"
            } else {
                "inbox:read:all-teams"
            }
        } else {
            &self.grant
        }
    }
}
pub fn builtin_registry() -> &'static Registry {
    static REGISTRY: std::sync::OnceLock<Registry> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(Registry::default)
}
fn register_resources(registry: &mut Registry) {
    for (verb, twin, class) in [
        ("files.list", "list_context_content", Class::Read),
        ("files.read", "read_content_file", Class::Read),
        ("files.move", "move_content_file", Class::Metadata),
        ("files.delete", "delete_content_file", Class::Metadata),
        ("files.create", "create_content_file", Class::Metadata),
    ] {
        registry.register(
            "files",
            spec(
                verb,
                class,
                matches!(verb, "files.move" | "files.delete"),
                Some(twin),
                None,
                true,
                class != Class::Read,
            ),
        );
    }
    registry.register(
        "commands",
        spec(
            "commands.register",
            Class::Read,
            false,
            None,
            Some("Frame-scoped declaration: the calling dashboard names which of its own declared commands are active"),
            false,
            false,
        ),
    );
    registry.register(
        "commands",
        spec(
            "commands.list",
            Class::Read,
            false,
            Some("command_list"),
            None,
            false,
            false,
        ),
    );
    registry.register(
        "commands",
        spec(
            "commands.run",
            Class::Read,
            false,
            Some("command_run"),
            None,
            true,
            true,
        ),
    );
    for name in ["showMenu", "showPopover", "focusOut", "focusTerminal"] {
        registry.register_existing(
            "records",
            spec(
                name,
                Class::Read,
                false,
                None,
                Some("Host-only frame menus, popovers and focus; requires a rendered frame"),
                false,
                false,
            ),
        );
    }

    for name in ["subscribe", "unsubscribe", "page"] {
        registry.register_existing(
            "sessions",
            spec(
                name,
                Class::Read,
                false,
                None,
                Some("Host projection subscription, inbox cursor paging or shared slot UI topics"),
                false,
                false,
            ),
        );
    }
    registry.register_existing(
        "records",
        spec(
            "setUiState",
            Class::Prefs,
            false,
            None,
            Some("Host-owned shared slot UI state"),
            true,
            false,
        ),
    );

    registry.register_existing(
        "records",
        spec(
            "analyze",
            Class::Read,
            false,
            None,
            Some("Host presentation or dashboard-local operation"),
            false,
            false,
        ),
    );
    registry.register_existing(
        "inbox",
        spec(
            "archive",
            Class::Metadata,
            false,
            None,
            Some("Host operation bound to the rendered dashboard; no identical MCP binding"),
            true,
            true,
        ),
    );
    registry.register_existing(
        "inbox",
        spec(
            "unarchive",
            Class::Metadata,
            false,
            None,
            Some("Host operation bound to the rendered dashboard; no identical MCP binding"),
            true,
            true,
        ),
    );
    registry.register_existing(
        "processes",
        spec(
            "closeTerminalTab",
            Class::Manage,
            true,
            Some("close_terminal_tab"),
            None,
            true,
            true,
        ),
    );
    registry.register_existing(
        "processes",
        spec(
            "closeWorkspace",
            Class::Manage,
            true,
            Some("close_workspace"),
            None,
            true,
            true,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "context",
            Class::Read,
            false,
            None,
            Some("Host presentation or dashboard-local operation"),
            false,
            false,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "draft",
            Class::Metadata,
            false,
            None,
            Some("Host presentation or dashboard-local operation"),
            true,
            true,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "getDependencyStatus",
            Class::Read,
            false,
            None,
            Some("Host presentation or dashboard-local operation"),
            false,
            false,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "getDocDir",
            Class::Read,
            false,
            None,
            Some("Host presentation or dashboard-local operation"),
            false,
            false,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "getWorkspaceStatus",
            Class::Read,
            false,
            None,
            Some("Host operation bound to the rendered dashboard; no identical MCP binding"),
            true,
            true,
        ),
    );
    registry.register_existing(
        "inbox",
        spec(
            "humanStatus",
            Class::Read,
            false,
            Some("get_human_status"),
            None,
            false,
            false,
        ),
    );
    registry.register_existing(
        "inbox",
        spec(
            "inbox",
            Class::Read,
            false,
            Some("inbox_list"),
            None,
            false,
            false,
        ),
    );
    registry.register_existing(
        "inbox",
        spec(
            "inboxRollup",
            Class::Read,
            false,
            None,
            Some("Host operation bound to the rendered dashboard; no identical MCP binding"),
            false,
            false,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "machines",
            Class::Read,
            false,
            Some("list_machines"),
            None,
            false,
            false,
        ),
    );
    registry.register_existing(
        "inbox",
        spec(
            "loadMedia",
            Class::Metadata,
            true,
            None,
            Some("Fetch one inbox image after the user requests it"),
            true,
            true,
        ),
    );
    registry.register_existing(
        "inbox",
        spec(
            "markRead",
            Class::Metadata,
            false,
            None,
            Some("Host operation bound to the rendered dashboard; no identical MCP binding"),
            true,
            true,
        ),
    );
    registry.register_existing(
        "inbox",
        spec(
            "markUnread",
            Class::Metadata,
            false,
            Some("mark_unread"),
            None,
            true,
            true,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "modelRates",
            Class::Read,
            false,
            Some("model_rates"),
            None,
            false,
            false,
        ),
    );
    registry.register_existing(
        "processes",
        spec(
            "openFile",
            Class::Spawn,
            false,
            None,
            Some("Host presentation or dashboard-local operation"),
            true,
            true,
        ),
    );
    registry.register_existing(
        "processes",
        spec(
            "embedMessage",
            Class::Spawn,
            false,
            None,
            Some("Host-minted inline inbox message frame"),
            true,
            true,
        ),
    );
    registry.register_existing(
        "processes",
        spec(
            "openMessage",
            Class::Spawn,
            false,
            None,
            Some("Host presentation or dashboard-local operation"),
            true,
            true,
        ),
    );
    registry.register_existing(
        "processes",
        spec(
            "openTab",
            Class::Spawn,
            false,
            Some("open_terminal_tab"),
            None,
            true,
            true,
        ),
    );
    registry.register_existing(
        "processes",
        spec(
            "openWorkspace",
            Class::Spawn,
            true,
            Some("open_workspace"),
            None,
            true,
            true,
        ),
    );
    registry.register_existing(
        "inbox",
        spec(
            "postMessage",
            Class::Send,
            true,
            Some("send_message"),
            None,
            true,
            true,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "prefs",
            Class::Read,
            false,
            None,
            Some("Host presentation or dashboard-local operation"),
            false,
            false,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "queryContext",
            Class::Read,
            false,
            Some("query_context"),
            None,
            false,
            false,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "readWorkspace",
            Class::Read,
            false,
            None,
            Some("Host operation bound to the rendered dashboard; no identical MCP binding"),
            true,
            true,
        ),
    );
    registry.register_existing(
        "inbox",
        spec(
            "replyAsHuman",
            Class::Send,
            true,
            Some("reply"),
            None,
            true,
            true,
        ),
    );
    registry.register_existing(
        "inbox",
        spec(
            "react",
            Class::Metadata,
            false,
            Some("react"),
            None,
            true,
            true,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "roles",
            Class::Read,
            false,
            Some("role_state"),
            None,
            false,
            false,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "routeToContext",
            Class::Metadata,
            false,
            None,
            Some("Host presentation or dashboard-local operation"),
            true,
            true,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "routine",
            Class::Read,
            false,
            Some("routine_get"),
            None,
            false,
            false,
        ),
    );
    registry.register_existing(
        "processes",
        spec(
            "routineCancel",
            Class::Manage,
            true,
            None,
            Some("Host operation bound to the rendered dashboard; no identical MCP binding"),
            true,
            true,
        ),
    );
    registry.register_existing(
        "processes",
        spec(
            "routineRefresh",
            Class::Spawn,
            false,
            None,
            Some("Host operation bound to the rendered dashboard; no identical MCP binding"),
            true,
            true,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "routineResult",
            Class::Read,
            false,
            Some("routine_result"),
            None,
            true,
            true,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "routineRun",
            Class::Read,
            false,
            Some("routine_run"),
            None,
            false,
            false,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "routineRuns",
            Class::Read,
            false,
            Some("routine_runs"),
            None,
            false,
            false,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "routineStatus",
            Class::Read,
            false,
            None,
            Some("Host operation bound to the rendered dashboard; no identical MCP binding"),
            true,
            true,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "routines",
            Class::Read,
            false,
            Some("routine_list"),
            None,
            false,
            false,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "run",
            Class::Read,
            false,
            Some("read_run"),
            None,
            false,
            false,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "runEvidence",
            Class::Read,
            false,
            Some("read_run_evidence"),
            None,
            false,
            false,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "runWorkProduct",
            Class::Read,
            false,
            Some("read_run_work_product"),
            None,
            false,
            false,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "runs",
            Class::Read,
            false,
            Some("list_runs"),
            None,
            false,
            false,
        ),
    );
    registry.register_existing(
        "inbox",
        spec(
            "sendAsHuman",
            Class::Send,
            true,
            Some("send_message"),
            None,
            true,
            true,
        ),
    );
    registry.register_existing(
        "processes",
        spec(
            "sendToWorkspaceTab",
            Class::Send,
            true,
            None,
            Some("Host operation bound to the rendered dashboard; no identical MCP binding"),
            true,
            true,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "setField",
            Class::Metadata,
            false,
            None,
            Some("Host presentation or dashboard-local operation"),
            true,
            true,
        ),
    );
    registry.register_existing(
        "inbox",
        spec(
            "setHumanStatus",
            Class::Metadata,
            false,
            None,
            Some("Host operation bound to the rendered dashboard; no identical MCP binding"),
            true,
            true,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "setModelRate",
            Class::Metadata,
            false,
            Some("set_model_rate"),
            None,
            true,
            true,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "setPrefs",
            Class::Prefs,
            false,
            None,
            Some("Host presentation or dashboard-local operation"),
            true,
            false,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "setStatus",
            Class::Metadata,
            false,
            None,
            Some("Host presentation or dashboard-local operation"),
            true,
            true,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "updates",
            Class::Read,
            false,
            Some("list_agent_updates"),
            None,
            false,
            false,
        ),
    );
    registry.register_existing(
        "records",
        spec(
            "validateYaml",
            Class::Read,
            false,
            None,
            Some("Host operation bound to the rendered dashboard; no identical MCP binding"),
            false,
            false,
        ),
    );
    registry.register_existing(
        "inbox",
        spec(
            "watchAgentActivity",
            Class::Read,
            false,
            None,
            Some("Host presentation or dashboard-local operation"),
            false,
            false,
        ),
    );
    registry.register_existing(
        "inbox",
        spec(
            "watchAgentInbox",
            Class::Read,
            false,
            None,
            Some("Host presentation or dashboard-local operation"),
            false,
            false,
        ),
    );
    registry.register_existing(
        "inbox",
        spec(
            "watchInbox",
            Class::Read,
            false,
            None,
            Some("Host presentation or dashboard-local operation"),
            false,
            false,
        ),
    );
    registry.register_existing(
        "records",
        spec("setup", Class::Read, false, Some("describe_setup"), None, false, false),
    );
    registry.register_existing(
        "records",
        spec(
            "worktrees",
            Class::Read,
            false,
            Some("list_worktrees"),
            None,
            false,
            false,
        ),
    );
}
mod agent {
    use super::*;
    pub fn register(registry: &mut Registry) {
        registry.register(
            "agent",
            spec(
                "moveWorkspace",
                Class::Prefs,
                false,
                Some("reorder_workspace"),
                None,
                true,
                false,
            ),
        );
        // Architecture: presentation entry points use the single registry grant check.
        // Policy (open): reuse sessions:read, without confirmation or human-only gates.
        for (name, reason) in [
            (
                "openLauncher",
                "Opens a filled native launch sheet; agents launch with open_workspace",
            ),
            (
                "openRoutineDraft",
                "Opens a filled private routine draft; agents create with routine_create",
            ),
        ] {
            registry.register(
                "agent",
                spec(name, Class::Spawn, false, None, Some(reason), true, true),
            );
        }
        registry.register(
            "agent",
            spec(
                "launchUnder",
                Class::Spawn,
                false,
                None,
                Some("Opens the native launch sheet; agents launch with open_workspace"),
                true,
                true,
            ),
        );
        registry.register(
            "agent",
            spec(
                "openAgentInbox",
                Class::Spawn,
                false,
                None,
                Some("Reveals the agent and opens its native inbox rail"),
                true,
                true,
            ),
        );
        registry.register(
            "agent",
            spec(
                "agent",
                Class::Read,
                false,
                Some("agent_get"),
                None,
                false,
                false,
            ),
        );
        registry.register(
            "agent",
            spec(
                "agents",
                Class::Read,
                false,
                Some("agent_list"),
                None,
                false,
                false,
            ),
        );
        registry.register(
            "agent",
            spec(
                "getEpicHandle",
                Class::Read,
                false,
                None,
                Some("Host presentation or dashboard-local operation"),
                false,
                false,
            ),
        );
        registry.register(
            "agent",
            spec(
                "gotoAgent",
                Class::Spawn,
                false,
                None,
                Some("Host presentation or dashboard-local operation"),
                true,
                true,
            ),
        );
        registry.register(
            "agent",
            spec(
                "runPrompt",
                Class::Send,
                true,
                None,
                Some("Host operation bound to the rendered dashboard; no identical MCP binding"),
                true,
                true,
            ),
        );
        registry.register(
            "agent",
            spec(
                "runSkill",
                Class::Spawn,
                false,
                None,
                Some("Host operation bound to the rendered dashboard; no identical MCP binding"),
                true,
                true,
            ),
        );
        registry.register(
            "agent",
            spec(
                "sendToSession",
                Class::Send,
                true,
                Some("send_to_session"),
                None,
                true,
                true,
            ),
        );
        registry.register(
            "agent",
            spec(
                "sendPrompt",
                Class::Send,
                true,
                None,
                Some("Agents prompt running agents with send_to_session; this is the person's typed send from a page."),
                true,
                true,
            ),
        );
        registry.register(
            "agent",
            spec(
                "canPasteToTerminal",
                Class::Read,
                false,
                None,
                Some("Pages ask it to enable a paste control; agents address sessions by handle with send_to_session."),
                false,
                false,
            ),
        );
        registry.register(
            "agent",
            spec("storyChanges", Class::Read, false, Some("story_changes"), None, false, false),
        );
        Registry::register(
            registry,
            "agent",
            spec("storyTotals", Class::Read, false, Some("story_totals"), None, false, false),
        );
        registry.register("agent", spec("openDiff", Class::Spawn, false, None, Some("Opens a read-only source diff tab in the Mac viewer."), true, true));
        registry.register(
            "agent",
            spec("whyLine", Class::Read, false, Some("why_line"), None, false, false),
        );
        registry.register(
            "agent",
            spec(
                "sessionRead",
                Class::Read,
                false,
                Some("session_read"),
                None,
                false,
                false,
            ),
        );
        registry.register(
            "agent",
            spec(
                "sessionUsage",
                Class::Read,
                false,
                Some("session_usage"),
                None,
                false,
                false,
            ),
        );
        registry.register(
            "agent",
            spec(
                "sessions",
                Class::Read,
                false,
                Some("list_sessions"),
                None,
                false,
                false,
            ),
        );
        registry.register(
            "agent",
            spec(
                "dismiss_agent_mark",
                Class::Metadata,
                false,
                Some("dismiss_agent_mark"),
                None,
                true,
                true,
            ),
        );
        registry.register(
            "agent",
            spec(
                "stopSession",
                Class::Manage,
                true,
                None,
                Some("Host operation bound to the rendered dashboard; no identical MCP binding"),
                true,
                true,
            ),
        );
    }
}
