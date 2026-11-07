// SPDX-License-Identifier: MIT
//! OpenAI Codex.

use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Instant;

use serde::Deserialize;
use serde_json::{json, Value};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Message, WebSocket};

use super::hooks::HookInput;
use super::*;

pub(super) struct Codex;

static DESCRIPTOR: LazyLock<Descriptor> = LazyLock::new(|| {
    Descriptor {
        name: "OpenAI Codex".into(),
        binary: "codex".into(),
        // The oldest CLI whose flags this recipe relies on; the dependency check refuses older.
        min_version: (0, 140),
        machine_check: Some(MachineCheck {
            label: "Codex command sandbox",
            path: "/proc/sys/kernel/apparmor_restrict_unprivileged_userns",
            restricted: "1",
            remedy: "Codex cannot run sandboxed commands: this machine restricts unprivileged user namespaces. To allow them, run: echo 'kernel.apparmor_restrict_unprivileged_userns = 0' | sudo tee /etc/sysctl.d/60-codex-sandbox-userns.conf && sudo sysctl --system",
        }),
        // A ChatGPT-login session must never bill an inherited API key.
        env_suppress: strings(&["OPENAI_API_KEY"]),
        // `codex exec` reads this key ahead of the login in `CODEX_HOME`; no other
        // credential variable is known to outrank it.
        account_env_scrub: strings(&["CODEX_API_KEY"]),
        root_doc: "AGENTS.md".into(),
        shadowing_root_docs: Vec::new(),
        shadowing_root_docs_up_to_git_root: false,
        headless_entry: strings(&["exec"]),
        // Qualified root PermissionRequest hooks report needs-you through normalize().
        attention: Some(AttentionSignal::Hooks),
        mcp_scope: Some(McpScope {
            config_filename: ".codex/config.toml".into(),
            server_map_key: "mcp_servers".into(),
            bridge_id: "codesense".into(),
            bridge_entry: McpBridgeEntry::Command,
            format: ConfigFragmentFormat::Toml,
            injection: ScopeInjection::EnvDir {
                carry_home: true,
                env_var: "CODEX_HOME".into(),
                passthrough: vec![Passthrough {
                    name: "auth.json".into(),
                    mode: PassthroughMode::Copy,
                }],
                default_home: "~/.codex".into(),
            },
            bridge_override: Some(BridgeOverride {
                flag_template: strings(&["-c", "mcp_servers.codesense.command={bridge}"]),
            }),
            // Codex validates the merged config on every write, so a switched-off server
            // keeps a transport of its own type (the override merges over its project
            // layer, and a stdio server cannot take a url): a no-op command, or a dead
            // local URL. The built-in apps server is switched off too.
            inventory: Some(McpInventory {
                argv: strings(&["--no-daemon", "mcp", "list", "--json"]),
                builtin: strings(&["codex_apps"]),
                stdio_fields: "command=\"/usr/bin/false\"".into(),
                http_fields: "url=\"http://127.0.0.1:9\"".into(),
                override_args: strings(&["--no-daemon", "-c"]),
                selectors: strings(CONFIG_SELECTORS),
                refused: vec![
                    RefusedFlag {
                        flags: strings(&["--remote"]),
                        reason: "a local scoped launch cannot use a remote app server".into(),
                    },
                    RefusedFlag {
                        flags: strings(&["-C", "--cd"]),
                        reason: concat!(
                            "a scoped launch runs in the launch workspace; ",
                            "remove the extra --cd override"
                        )
                        .into(),
                    },
                ],
            }),
        }),
        add_dir_flag: Some("--add-dir".into()),
        prompt_operand: true,
        prompt_flag: None,
        initial_prompt_limit: None,
        headless_aliases: Vec::new(),
        bypass_flag: "--dangerously-bypass-approvals-and-sandbox".into(),
        home_env: "CODEX_HOME".into(),
        home_default: Some("~/.codex".into()),
        launch_settings: LaunchSettings::Overrides("-c".into()),
        // It has neither of Claude's isolation switches, so a startup naming them is refused.
        startup: StartupRules {
            name: "Codex",
            refused: &["--strict-mcp-config", "--setting-sources"],
            settings_keys: &["approval_policy", "sandbox_mode"],
            string_settings: true,
        },
        default_context: strings(&["AGENTS.md", ".agents/skills"]),
        skills_dir: ".agents/skills/".into(),
        // It reads no project command directory.
        commands_dir: None,
        telemetry: Some(TelemetryExport {
            env: Vec::new(),
            argv: strings(&[
                "-c",
                "otel.exporter={otlp-http={endpoint=\"{endpoint}/v1/logs\",protocol=\"json\"}}",
            ]),
            // Codex's own default export goes to OpenAI: its metrics exporter (statsig) and its
            // analytics events. A managed launch turns both off. A saved agent opts back in with
            // its own `-c` for the same key in its launch args, which this leaves alone.
            default_overrides: vec![
                (
                    "otel.metrics_exporter".into(),
                    strings(&["-c", "otel.metrics_exporter=\"none\""]),
                ),
                (
                    "analytics.enabled".into(),
                    strings(&["-c", "analytics.enabled=false"]),
                ),
            ],
            // Each streamed response's completion; its other SSE events carry no usage.
            usage_event_name: "codex.sse_event".into(),
            usage_event_kind: Some("response.completed".into()),
            usage_attributes: strings(&[
                "event.name",
                "event.kind",
                "event.timestamp",
                "conversation.id",
                "model",
                "input_token_count",
                "output_token_count",
                "cached_token_count",
                "cache_write_token_count",
                "reasoning_token_count",
            ]),
        }),
        usage_snapshot_timing: true,
        attribution_hook_trust: true,
        // Its status comes from its activity observer and the bridge, not status hooks.
        project_hooks: None,
        // Every role runs on one install in this range: its root hooks report its main
        // turn, and a remote task runs on it both as the local app-server and as the
        // machine's exec-server (a Claude task's executor too). The oldest release
        // qualified for all of them, and the first major not yet checked.
        release: Release {
            prefix: "codex-cli ",
            suffix: "",
            min: (0, 159, 3),
            before_major: Some(1),
        before: None,
        first_line: false,
            supported: "Codex 0.159.3 or a newer 0.x release",
        },
    }
});

static PROBE: Probe = Probe {
    cache_version: false,
    version_env: &[],
    version_budget: super::DEFAULT_VERSION_BUDGET,
    launch_flags: &["--no-daemon", "--config"],
    agent_flags: &[],
    activity: ActivityProbe::Version,
    help_subcommand: &[],
};

/// Every hook the activity observer folds.
const OBSERVER_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "Stop",
    "Interrupt",
    "SessionEnd",
    "SubagentStart",
    "SubagentStop",
];

/// Its inbox checks, each handing a notice over as context: a `PostToolUse` hook that
/// exits 2 blocks the tool's result instead. There is no turn-end check: a `Stop` hook
/// that exits 2 continues the turn after its activity already reads idle, so its host
/// could start the next notice's turn into one that is still running. Mail after its last
/// tool call goes as a turn of its own once it is idle.
const CHECKS: &[Check] = &[
    Check {
        event: "UserPromptSubmit",
        hand_over: HandOver::AdditionalContext,
    },
    Check {
        event: "PostToolUse",
        hand_over: HandOver::AdditionalContext,
    },
];

/// How long a check may take, in seconds. It looks once and is answered at once; only a
/// check that ran before its invocation was committed waits (Codex runs none before its
/// first prompt), and Codex then ends it rather than hold the turn.
const CHECK_TIMEOUT: u32 = 10;

/// One `-c` override per event, each running `command(event)` with `timeout` seconds.
/// Definitions contain no invocation secrets and stay the same across runs, for native
/// hook trust (kept per event and handler); user and project layers stay in place, and
/// hook-disabling policy is respected.
fn hook_flags(
    events: impl IntoIterator<Item = &'static str>,
    timeout: u32,
    command: impl Fn(&str) -> String,
) -> Result<Vec<String>, &'static str> {
    let mut flags = Vec::new();
    for event in events {
        let quoted = serde_json::to_string(&command(event)).map_err(|_| "invalid hook command")?;
        flags.extend([
            "-c".into(),
            format!(
                "hooks.{event}=[{{hooks=[{{type=\"command\",command={quoted},timeout={timeout}}}]}}]"
            ),
        ]);
    }
    Ok(flags)
}

/// The stable observer command used in launch overrides and native readiness proof.
fn observer_command() -> String {
    format!(
        "[ -x \"${}/{}\" ] || exit 0; exec \"${}/{}\" --activity-hook codex",
        crate::activity::HOST_ENV,
        crate::activity::HELPER_NAME,
        crate::activity::HOST_ENV,
        crate::activity::HELPER_NAME
    )
}

/// The observer's `-c` overrides, one per event.
fn observer_flags(_helper: &str) -> Result<Vec<String>, &'static str> {
    hook_flags(OBSERVER_EVENTS.iter().copied(), 1, |_| observer_command())
}

/// The inbox checks' `-c` overrides, one per event.
fn check_flags() -> Result<Vec<String>, &'static str> {
    hook_flags(Codex.inbox().events(), CHECK_TIMEOUT, |event| {
        inbox_hook_command(AgentCli::Codex, event)
    })
}

/// Appends the observer to `arguments`' options (see [`compose`]).
fn compose_observer(arguments: &[String], helper: &str) -> Result<Vec<String>, &'static str> {
    compose(arguments, &observer_flags(helper)?)
}

fn config_override(arguments: &[String], index: usize) -> Option<&str> {
    match arguments[index].as_str() {
        "-c" | "--config" => arguments.get(index + 1).map(String::as_str),
        argument => argument
            .strip_prefix("--config=")
            .or_else(|| argument.strip_prefix("-c=")),
    }
}

/// Appends the hook groups `owned` sets (`-c hooks.<event>=[…]` pairs) to `arguments`'
/// options, after every handler a session's own flags set, each once; Codex still
/// discovers user, project and plugin layers and applies trust.
fn compose(arguments: &[String], owned: &[String]) -> Result<Vec<String>, &'static str> {
    // Only the options are composed: everything after a bare `--` is an operand (the
    // starting prompt), and the owned `-c` flags go before it.
    let (options, operands) = arguments.split_at(operands_at(arguments));
    let mut output = options.to_vec();
    for pair in owned.chunks_exact(2) {
        let (key, owned_value) = pair[1].split_once('=').ok_or("invalid generated hook")?;
        let owned: toml::Table = format!("value={owned_value}")
            .parse()
            .map_err(|_| "invalid generated hook")?;
        let group = owned["value"].as_array().ok_or("invalid generated hooks")?[0].clone();
        let mut matching = Vec::new();
        for (index, argument) in output.iter().enumerate() {
            if argument.starts_with("-chooks") {
                return Err("compact launch hook override is ambiguous");
            }
            let value = config_override(&output, index);
            if let Some(value) = value {
                let Some((candidate, _)) = value.split_once('=') else {
                    continue;
                };
                if candidate.trim() == "hooks" {
                    return Err("whole-table launch hooks require explicit composition");
                }
                if candidate.trim().starts_with(&format!("{key}.")) {
                    return Err("nested launch hook override is ambiguous");
                }
                if candidate.trim() == key {
                    matching.push((
                        index,
                        value.to_string(),
                        matches!(argument.as_str(), "-c" | "--config"),
                    ));
                }
            }
        }
        if matching.len() > 1 {
            return Err("repeated launch hook override is ambiguous");
        }
        if let Some((index, definition, separate)) = matching.pop() {
            let (_, value) = definition.split_once('=').ok_or("invalid launch hooks")?;
            let mut table: toml::Table = format!("value={value}")
                .parse()
                .map_err(|_| "invalid launch hooks")?;
            let groups = table
                .get_mut("value")
                .and_then(toml::Value::as_array_mut)
                .ok_or("launch hook groups must be arrays")?;
            if groups.contains(&group) {
                continue;
            }
            groups.push(group);
            let definition = format!("{key}={}", toml::Value::Array(groups.clone()));
            if separate {
                output[index + 1] = definition;
            } else if output[index].starts_with("--config=") {
                output[index] = format!("--config={definition}");
            } else {
                output[index] = format!("-c={definition}");
            }
        } else {
            output.extend(["-c".into(), pair[1].clone()]);
        }
    }
    if !output.iter().any(|argument| argument == "--no-daemon") {
        // This is a root CLI option: after `exec` Codex rejects it.
        output.insert(0, "--no-daemon".into());
    }
    output.extend_from_slice(operands);
    Ok(output)
}

/// Marks a hook command as the git-ai checkpoint CodeSense runs for line attribution
/// (a trailing shell comment). Its groups are trusted like the observer's.
pub const ATTRIBUTION_HOOK_MARKER: &str = "codesense-git-ai";

/// One `-c` override per event, each running git-ai's checkpoint `command`. Refused when
/// the launch's own `arguments` already set hooks for one of the events (or the whole
/// table): Codex hook composition keeps one override per event, so the launch would not
/// start.
pub fn attribution_hook_flags(
    command: &str,
    events: &'static [&'static str],
    timeout: u32,
    arguments: &[String],
) -> Result<Vec<String>, &'static str> {
    if !command.contains(ATTRIBUTION_HOOK_MARKER) {
        return Err("attribution hook command lacks its marker");
    }
    for index in 0..operands_at(arguments) {
        let Some((key, _)) = config_override(arguments, index).and_then(|v| v.split_once('='))
        else {
            continue;
        };
        let key = key.trim();
        if key == "hooks"
            || events
                .iter()
                .any(|event| key.starts_with(&format!("hooks.{event}")))
        {
            return Err("the agent already sets Codex hooks for a tool event");
        }
    }
    hook_flags(events.iter().copied(), timeout, |_| command.to_owned())
}

/// A renderer-safe edit: only CodeSense hook overrides, never the user's argv.
#[derive(Debug)]
pub struct AttributionHookEdit {
    pub remove_args: Vec<String>,
    pub extra_args: Vec<String>,
}

/// Reuse native hook composition and trust, returning only the owned capture delta.
/// Other CLI arguments remain solely in the caller's recorded launch recipe.
pub fn attribution_hook_edit(
    arguments: &[String],
    command: Option<&str>,
    events: &'static [&'static str],
    timeout: u32,
) -> Result<AttributionHookEdit, &'static str> {
    let refreshed = refresh_attribution_hook_args(arguments, command, events, timeout)?;
    fn options(arguments: &[String]) -> Vec<(&str, &[String])> {
        let mut options = Vec::new();
        let mut at = 0;
        while at < operands_at(arguments) {
            let width = if matches!(arguments[at].as_str(), "-c" | "--config") {
                2
            } else {
                1
            };
            if let Some((key, _)) = config_override(arguments, at).and_then(|s| s.split_once('=')) {
                options.push((key.trim(), &arguments[at..at + width]));
            }
            at += width;
        }
        options
    }
    let before = options(arguments);
    let after = options(&refreshed);
    // Initial launch refuses user groups on capture events. These marked overrides
    // therefore contain only our checkpoint and the native observer/check groups.
    let changed: Vec<_> = before
        .iter()
        .chain(&after)
        .filter_map(|(key, tokens)| {
            events
                .iter()
                .any(|event| *key == format!("hooks.{event}"))
                .then_some((*key, tokens))
        })
        .filter(|(_, tokens)| {
            tokens
                .iter()
                .any(|token| token.contains(ATTRIBUTION_HOOK_MARKER))
        })
        .map(|(key, _)| key)
        .collect();
    let selected =
        |key: &str| changed.contains(&key) || (!changed.is_empty() && key == "hooks.state");
    let select = |options: Vec<(&str, &[String])>| {
        options
            .into_iter()
            .filter(|(key, _)| selected(key))
            .flat_map(|(_, tokens)| tokens.iter().cloned())
            .collect()
    };
    Ok(AttributionHookEdit {
        remove_args: select(before),
        extra_args: select(after),
    })
}

/// Reconcile only CodeSense's checkpoint groups and their session trust; keep user hooks.
/// The result has one canonical hooks.state override, using the existing trust composer.
pub fn refresh_attribution_hook_args(
    arguments: &[String],
    command: Option<&str>,
    events: &'static [&'static str],
    timeout: u32,
) -> Result<Vec<String>, &'static str> {
    let old_grants = attribution_hook_trust(arguments)?;
    let end = operands_at(arguments);
    let mut output = Vec::new();
    let mut index = 0;
    while index < end {
        let width = if matches!(arguments[index].as_str(), "-c" | "--config") {
            2
        } else {
            1
        };
        let mut replacement = None;
        if let Some((key, raw)) = config_override(arguments, index).and_then(|v| v.split_once('='))
        {
            let key = key.trim();
            if key == "hooks.state" && !old_grants.is_empty() {
                let mut parsed: toml::Table = format!("value={raw}")
                    .parse()
                    .map_err(|_| "invalid hook state")?;
                let state = parsed
                    .get_mut("value")
                    .and_then(toml::Value::as_table_mut)
                    .ok_or("invalid hook state")?;
                for (grant, _) in &old_grants {
                    state.remove(grant);
                }
                replacement = Some(
                    (!state.is_empty())
                        .then(|| format!("{key}={}", toml::Value::Table(state.clone()))),
                );
            } else if key
                .strip_prefix("hooks.")
                .is_some_and(|event| !event.contains('.') && event != "state")
            {
                let mut parsed: toml::Table = format!("value={raw}")
                    .parse()
                    .map_err(|_| "invalid hook groups")?;
                if let Some(groups) = parsed.get_mut("value").and_then(toml::Value::as_array_mut) {
                    let before = groups.len();
                    groups.retain(|group| {
                        !group["hooks"][0]["command"]
                            .as_str()
                            .is_some_and(|c| c.contains(ATTRIBUTION_HOOK_MARKER))
                    });
                    if groups.len() != before {
                        replacement = Some(
                            (!groups.is_empty())
                                .then(|| format!("{key}={}", toml::Value::Array(groups.clone()))),
                        );
                    }
                }
            }
        }
        match replacement {
            Some(Some(value)) => output.extend(["-c".into(), value]),
            Some(None) => {}
            None => output.extend_from_slice(&arguments[index..index + width]),
        }
        index += width;
    }
    if let Some(command) = command {
        let flags = attribution_hook_flags(command, events, timeout, &[])?;
        // A recorded recipe may already contain the native observer/check groups.
        // Compose with those, while retaining the initial launch's refusal of user tool hooks.
        let owned: Vec<_> = observer_flags("")?
            .into_iter()
            .chain(check_flags()?)
            .collect();
        for pair in flags.chunks_exact(2) {
            let key = pair[1].split_once('=').unwrap().0;
            let allowed: Vec<toml::Value> = owned
                .chunks_exact(2)
                .filter_map(|pair| {
                    let (candidate, raw) = pair[1].split_once('=')?;
                    (candidate == key).then(|| {
                        format!("value={raw}").parse::<toml::Table>().unwrap()["value"][0].clone()
                    })
                })
                .collect();
            for at in 0..operands_at(&output) {
                if let Some((candidate, raw)) =
                    config_override(&output, at).and_then(|v| v.split_once('='))
                {
                    if candidate.trim() == key {
                        let table: toml::Table = format!("value={raw}")
                            .parse()
                            .map_err(|_| "invalid hook groups")?;
                        if !table["value"]
                            .as_array()
                            .ok_or("invalid hook groups")?
                            .iter()
                            .all(|group| allowed.contains(group))
                        {
                            return Err("the agent already sets Codex hooks for a tool event");
                        }
                    }
                }
            }
        }
        output = compose(&output, &flags)?;
    }
    output.extend_from_slice(&arguments[end..]);
    // The normal native preparation recomposes observer/check hooks and recomputes all
    // owned trust positions after checkpoint groups were removed or added.
    let mut native = Prepare::new(output, json!({}), "");
    Codex.prepare(&mut native)?;
    let mut output = native.arguments;
    {
        // The trust composer merges existing user state. Emit its combined result once,
        // rather than depending on repeated -c override precedence on relaunch.
        let sites: Vec<_> = (0..operands_at(&output))
            .filter(|&at| {
                config_override(&output, at)
                    .and_then(|v| v.split_once('='))
                    .is_some_and(|(key, _)| {
                        key.trim() == "hooks.state" || key.trim().starts_with("hooks.state.")
                    })
            })
            .collect();
        let keep = sites
            .iter()
            .rev()
            .find(|&&at| {
                config_override(&output, at)
                    .unwrap()
                    .starts_with("hooks.state=")
            })
            .copied();
        for at in sites.into_iter().rev().filter(|at| Some(*at) != keep) {
            let width = if matches!(output[at].as_str(), "-c" | "--config") {
                2
            } else {
                1
            };
            output.drain(at..at + width);
        }
    }
    Ok(output)
}

/// `PreToolUse` as the snake-case label Codex keys hook state by.
fn event_label(event: &str) -> String {
    event
        .chars()
        .enumerate()
        .fold(String::new(), |mut label, (i, c)| {
            if i > 0 && c.is_ascii_uppercase() {
                label.push('_');
            }
            label.push(c.to_ascii_lowercase());
            label
        })
}

/// The state key and trusted hash of the `index`th group of `event`, whose first handler is
/// `handler`. Codex's normalized handler fingerprint (rust-v0.159.3/rust-v0.162.1):
/// hooks/src/engine/discovery.rs and config/src/fingerprint.rs in openai/codex.
fn handler_grant(
    event: &str,
    index: usize,
    handler: &toml::Value,
) -> Result<(String, String), &'static str> {
    use sha2::{Digest, Sha256};
    let label = event_label(event);
    // TOML omits absent Option fields. Normalization supplies async=false.
    let mut identity = json!({"event_name":label,"hooks":[{
        "type":"command", "command":handler["command"].as_str().ok_or("invalid owned command")?,
        "timeout":handler["timeout"].as_integer().ok_or("invalid owned timeout")?, "async":false
    }]});
    identity.sort_all_objects();
    let hash = format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&identity).map_err(|_| "invalid hook identity")?)
    );
    Ok((
        format!("/<session-flags>/config.toml:{label}:{index}:0"),
        hash,
    ))
}

/// Fingerprint an emitted hook through the production normalization in seam tests.
#[cfg(any(test, feature = "test-support"))]
pub fn handler_grant_for_test(
    event: &str,
    index: usize,
    handler: &toml::Value,
) -> Result<(String, String), &'static str> {
    handler_grant(event, index, handler)
}

/// Codex's normalized handler fingerprint (rust-v0.159.3/rust-v0.162.1):
/// hooks/src/engine/discovery.rs and config/src/fingerprint.rs in openai/codex.
/// Only generated handlers are granted trust in the selected native home; user hooks stay untrusted.
fn owned_hook_trust(arguments: &[String]) -> Result<Vec<(String, String)>, &'static str> {
    let owned = observer_flags("")?
        .into_iter()
        .chain(check_flags()?)
        .collect::<Vec<_>>();
    let mut grants = Vec::new();
    for pair in owned.chunks_exact(2) {
        let (key, raw) = pair[1].split_once('=').ok_or("invalid owned hook")?;
        let owned: toml::Table = format!("value={raw}")
            .parse()
            .map_err(|_| "invalid owned hook")?;
        let group = &owned["value"][0];
        let event = key.strip_prefix("hooks.").ok_or("invalid hook event")?;
        let current = (0..operands_at(arguments))
            .filter_map(|index| config_override(arguments, index))
            .filter_map(|arg| arg.split_once('='))
            .find(|(candidate, _)| candidate.trim() == key)
            .map(|(_, value)| value)
            .ok_or("owned hook was not composed")?;
        let current: toml::Table = format!("value={current}")
            .parse()
            .map_err(|_| "invalid composed hooks")?;
        for (index, candidate) in current["value"]
            .as_array()
            .ok_or("invalid composed hooks")?
            .iter()
            .enumerate()
        {
            if candidate == group {
                grants.push(handler_grant(event, index, &group["hooks"][0])?);
            }
        }
    }
    grants.extend(attribution_hook_trust(arguments)?);
    Ok(grants)
}

/// Trust for the git-ai checkpoint groups the launch carries (see
/// [`attribution_hook_flags`]), wherever composition put them. A group Codex does not
/// trust is skipped silently, and every line would then read as human.
fn attribution_hook_trust(arguments: &[String]) -> Result<Vec<(String, String)>, &'static str> {
    let mut grants = Vec::new();
    for index in 0..operands_at(arguments) {
        let Some((key, raw)) = config_override(arguments, index).and_then(|v| v.split_once('='))
        else {
            continue;
        };
        let Some(event) = key
            .trim()
            .strip_prefix("hooks.")
            .filter(|e| !e.contains('.'))
        else {
            continue;
        };
        let Ok(table) = format!("value={raw}").parse::<toml::Table>() else {
            continue;
        };
        let Some(groups) = table["value"].as_array() else {
            continue;
        };
        for (position, group) in groups.iter().enumerate() {
            let handler = &group["hooks"][0];
            if handler["command"]
                .as_str()
                .is_some_and(|c| c.contains(ATTRIBUTION_HOOK_MARKER))
            {
                grants.push(handler_grant(event, position, handler)?);
            }
        }
    }
    Ok(grants)
}

/// A correctly encoded session state table makes concurrent launches independent, while
/// the user layer still owns enabled=false. Preserve explicitly supplied user state;
/// only the current launch's generated handlers receive hashes from CodeSense.
fn session_hook_trust(
    arguments: &mut Vec<String>,
    grants: &[(String, String)],
) -> Result<(), &'static str> {
    fn set(
        table: &mut toml::value::Table,
        path: &[&str],
        value: toml::Value,
    ) -> Result<(), &'static str> {
        let Some((last, parents)) = path.split_last() else {
            return Err("invalid hook state key");
        };
        let mut table = table;
        for key in parents {
            table = table
                .entry((*key).to_owned())
                .or_insert_with(|| toml::Value::Table(Default::default()))
                .as_table_mut()
                .ok_or("invalid hook state table")?;
        }
        table.insert((*last).into(), value);
        Ok(())
    }
    let end = operands_at(arguments);
    let mut state = toml::value::Table::new();
    for index in 0..end {
        let Some(value) = config_override(arguments, index) else {
            continue;
        };
        let Some((key, raw)) = value.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key != "hooks.state" && !key.starts_with("hooks.state.") {
            continue;
        }
        let parsed: toml::Table = format!("value={raw}")
            .parse()
            .map_err(|_| "invalid hook state")?;
        if key == "hooks.state" {
            state = parsed["value"]
                .as_table()
                .ok_or("invalid hook state table")?
                .clone();
        } else {
            set(
                &mut state,
                &key["hooks.state.".len()..].split('.').collect::<Vec<_>>(),
                parsed["value"].clone(),
            )?;
        }
    }
    for (key, hash) in grants {
        let handler = state
            .entry(key.clone())
            .or_insert_with(|| toml::Value::Table(Default::default()))
            .as_table_mut()
            .ok_or("invalid hook handler state")?;
        handler.insert("trusted_hash".into(), toml::Value::String(hash.clone()));
    }
    let assignment = format!("hooks.state={}", toml::Value::Table(state));
    if !arguments[..end].contains(&assignment) {
        arguments.splice(end..end, ["-c".into(), assignment]);
    }
    Ok(())
}

/// Preserve the user's TOML, including comments and disabled hooks. CodeSense writers
/// serialize on an owner-only lock; a native config rewrite is detected and retried.
/// Never follow a replaced/symlinked config or truncate a malformed file.
fn save_hook_trust(home: &Path, grants: &[(String, String)]) -> io::Result<()> {
    use std::fs::{self, OpenOptions};
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let invalid = || io::Error::other("Codex hook trust config is not an owned regular TOML file");
    fs::create_dir_all(home)?;
    let directory = fs::metadata(home)?;
    // Native homes commonly inherit umask 002 (0775). Group writability alone
    // does not invalidate the account-owned config; each file is checked below.
    if !directory.is_dir()
        || directory.uid() != unsafe { libc::geteuid() }
        || directory.mode() & 0o002 != 0
    {
        return Err(invalid());
    }
    let config = home.join("config.toml");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(home.join(".codesense-hook-trust.lock"))?;
    let meta = lock.metadata()?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.nlink() != 1 {
        return Err(invalid());
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        // SAFETY: flock borrows this live descriptor; closing it releases our lock.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            break;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::WouldBlock || Instant::now() >= deadline {
            return Err(error);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let stamp = |metadata: &fs::Metadata| {
        (
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.modified().ok(),
        )
    };
    for _ in 0..3 {
        let (text, previous) = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&config)
        {
            Ok(file) => {
                let metadata = file.metadata()?;
                if !metadata.is_file()
                    || metadata.uid() != unsafe { libc::geteuid() }
                    || metadata.nlink() != 1
                    || metadata.len() > 1024 * 1024
                {
                    return Err(invalid());
                }
                let mut text = String::new();
                file.take(1024 * 1024 + 1).read_to_string(&mut text)?;
                if text.len() > 1024 * 1024 {
                    return Err(invalid());
                }
                (text, Some(stamp(&metadata)))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => (String::new(), None),
            Err(error) => return Err(error),
        };
        let mut document: toml_edit::Document = text.parse().map_err(|_| invalid())?;
        for (key, hash) in grants {
            let mut table: &mut dyn toml_edit::TableLike = document.as_table_mut();
            for part in ["hooks", "state", key.as_str()] {
                table = table
                    .entry(part)
                    .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
                    .as_table_like_mut()
                    .ok_or_else(invalid)?;
            }
            // Existing hashes may belong to a user's handler at this positional key.
            // Leave them untouched; the launch's own state grants its exact definition.
            if table.get("trusted_hash").is_none() {
                table.insert("trusted_hash", toml_edit::value(hash));
            }
        }
        let next = document.to_string();
        if next == text {
            return Ok(());
        }
        let temporary = home.join(format!(".codesense-hooks-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(next.as_bytes())?;
            file.sync_all()?;
            let current = match fs::symlink_metadata(&config) {
                Ok(metadata) => Some(stamp(&metadata)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(error),
            };
            if current != previous {
                return Ok(false);
            }
            fs::rename(&temporary, &config)?;
            Ok(true)
        })();
        let _ = fs::remove_file(&temporary);
        if result? {
            return Ok(());
        }
    }
    Err(io::Error::other(
        "Codex hook trust config kept changing; retry launch",
    ))
}

/// The rollout file of the Codex thread `session_id` under the native home `home`: exactly one
/// `rollout-*-<id>.jsonl` below `sessions/`, found by the id alone. The id comes from accepted
/// activity/native-engine evidence, never a directory name, cwd or recency guess.
pub(super) fn locate_rollout(home: &Path, session_id: &str) -> Option<std::path::PathBuf> {
    use std::fs;
    uuid::Uuid::parse_str(session_id)
        .ok()
        .filter(|id| id.to_string() == session_id)?;
    if !home.is_absolute() {
        return None;
    }
    let root = home.join("sessions");
    if !fs::symlink_metadata(&root).ok()?.is_dir() {
        return None;
    }
    let root = root.canonicalize().ok()?;
    let suffix = format!("-{session_id}.jsonl");
    let mut pending = vec![(root.clone(), 0)];
    let mut matches = Vec::new();
    let mut budget = 20_000usize;
    while let Some((dir, depth)) = pending.pop() {
        for entry in fs::read_dir(dir).ok()? {
            budget = budget.checked_sub(1)?;
            let entry = entry.ok()?;
            let kind = entry.file_type().ok()?;
            if kind.is_dir() && depth < 3 {
                pending.push((entry.path(), depth + 1));
            } else if kind.is_file()
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(&suffix))
            {
                matches.push(entry.path());
                if matches.len() > 1 {
                    return None;
                }
            }
        }
    }
    let [path] = matches.as_slice() else {
        return None;
    };
    let canon = path.canonicalize().ok()?;
    canon.starts_with(&root).then_some(canon)
}

/// Native records live under the selected native home.
fn session_record(home: &Path, session_id: &str) -> Option<output::SessionRecord> {
    use std::fs;
    use std::io::{BufRead, Read, Seek, SeekFrom};
    use std::os::unix::fs::OpenOptionsExt;
    let path = locate_rollout(home, session_id)?;
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut first = Vec::new();
    std::io::BufReader::new((&mut file).take(output::MAX_RECORD_BYTES as u64))
        .read_until(b'\n', &mut first)
        .ok()?;
    let header: Value = serde_json::from_slice(&first).ok()?;
    if header["type"] != "session_meta" || header.pointer("/payload/id")?.as_str()? != session_id {
        return None;
    }
    let length = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(
        length.saturating_sub(output::MAX_RECORD_BYTES as u64),
    ))
    .ok()?;
    let mut raw = Vec::new();
    file.take(output::MAX_RECORD_BYTES as u64)
        .read_to_end(&mut raw)
        .ok()?;
    Some(output::SessionRecord {
        session_id: session_id.into(),
        format: TranscriptFormat::CodexSessionJsonl,
        raw: String::from_utf8_lossy(&raw).into_owned(),
    })
}

/// Parse the recorded public messages, tool I/O, reasoning marker, model and times of a rollout.
/// Instructions and synthetic item-completed echoes are not a second copy of the conversation.
pub(super) fn parse_session_record(raw: &str) -> Option<output::SessionTranscript> {
    parse_rollout_lines(raw.lines())
}

/// [`parse_session_record`] over a rollout's lines one at a time, so a long rollout is never held
/// whole in memory.
pub(super) fn parse_rollout_lines<I>(lines: I) -> Option<output::SessionTranscript>
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    use output::{Reasoning, ReasoningKind, SessionTranscript, ToolCall, Turn, Usage};
    let mut turns: Vec<Turn> = Vec::new();
    let mut pending = std::collections::HashMap::<String, (usize, usize)>::new();
    // The call whose output has not arrived yet: an `item_completed` inside it is its alias.
    let mut open_call: Option<(usize, usize)> = None;
    // The model of the latest `turn_context`, and the reasoning step that precedes the next output.
    let mut model: Option<String> = None;
    let mut reasoning: Option<Reasoning> = None;
    for line in lines {
        let Ok(row) = serde_json::from_str::<Value>(line.as_ref()) else {
            continue;
        };
        let p = &row["payload"];
        let ts_ms = row["timestamp"].as_str().and_then(output::rfc3339_ms);
        if row["type"] == "turn_context" {
            model = p["model"].as_str().map(str::to_owned).or(model);
            continue;
        }
        if row["type"] == "event_msg" && p["type"] == "token_count" {
            if let Some(t) = turns.last_mut().filter(|t| t.role == "assistant") {
                let usage = &p["info"]["last_token_usage"];
                let input_tokens = usage["input_tokens"].as_u64();
                let output_tokens = usage["output_tokens"].as_u64();
                if input_tokens.is_some() || output_tokens.is_some() {
                    t.usage = Some(Usage {
                        input_tokens,
                        output_tokens,
                    });
                }
            }
            continue;
        }
        if row["type"] == "event_msg" && p["type"] == "item_completed" {
            // Codex runs a file change inside the open call and names it by its own item id
            // (`exec-<uuid>`), which is what git-ai records as the tool use id.
            if let (Some((ti, ci)), Some(id)) = (open_call, p["item"]["id"].as_str()) {
                if !id.is_empty() && !pending.contains_key(id) {
                    let aliases = &mut turns[ti].tool_calls[ci].aliases;
                    if !aliases.iter().any(|a| a == id) {
                        aliases.push(id.into());
                    }
                }
            }
            continue;
        }
        if row["type"] != "response_item" {
            continue;
        }
        match p["type"].as_str() {
            Some("reasoning") => {
                let summary = record_text(&p["summary"]);
                reasoning = Some(match (summary, p["encrypted_content"].is_string()) {
                    (Some(text), _) => Reasoning {
                        kind: ReasoningKind::Summary,
                        text: Some(text),
                    },
                    (None, true) => Reasoning {
                        kind: ReasoningKind::Encrypted,
                        text: None,
                    },
                    (None, false) => Reasoning {
                        kind: ReasoningKind::None,
                        text: None,
                    },
                });
            }
            Some("message") => {
                let Some(role) = p["role"]
                    .as_str()
                    .filter(|r| matches!(*r, "user" | "assistant"))
                else {
                    continue;
                };
                let content = if role == "user" {
                    without_injected_context(&p["content"])
                } else {
                    p["content"].clone()
                };
                if let Some(text) = record_text(&content) {
                    let assistant = role == "assistant";
                    turns.push(Turn {
                        role: role.into(),
                        text: Some(text),
                        ts_ms,
                        model: model.clone().filter(|_| assistant),
                        reasoning: reasoning.take().filter(|_| assistant),
                        ..Turn::default()
                    });
                }
            }
            Some("function_call" | "custom_tool_call") => {
                let Some(id) = p["call_id"].as_str() else {
                    continue;
                };
                if pending.contains_key(id) {
                    continue;
                }
                if turns.last().is_none_or(|t| t.role != "assistant") {
                    turns.push(Turn {
                        role: "assistant".into(),
                        ts_ms,
                        model: model.clone(),
                        ..Turn::default()
                    });
                }
                let ti = turns.len() - 1;
                if turns[ti].reasoning.is_none() {
                    turns[ti].reasoning = reasoning.take();
                }
                let calls = &mut turns[ti].tool_calls;
                pending.insert(id.into(), (ti, calls.len()));
                open_call = Some((ti, calls.len()));
                let name = p["name"].as_str().unwrap_or("tool");
                calls.push(ToolCall {
                    input: p
                        .get("arguments")
                        .or_else(|| p.get("input"))
                        .and_then(record_text),
                    native_id: Some(id.into()),
                    // An MCP call is namespaced `mcp__<server>__`.
                    mcp_server: output::mcp_server_of(p["namespace"].as_str().unwrap_or(""))
                        .map(str::to_owned),
                    ts_ms,
                    ..ToolCall::named(name)
                });
            }
            Some("function_call_output" | "custom_tool_call_output") => {
                if let Some(&(ti, ci)) = p["call_id"].as_str().and_then(|id| pending.get(id)) {
                    if open_call == Some((ti, ci)) {
                        open_call = None;
                    }
                    let call = &mut turns[ti].tool_calls[ci];
                    if call.result.is_none() {
                        call.result = record_text(&p["output"]);
                    }
                }
            }
            _ => {}
        }
    }
    (!turns.is_empty()).then(|| {
        SessionTranscript::new(
            None,
            turns,
            // The rollout's tool outputs carry no error flag (codex-cli 0.164.0); a failure is not
            // read out of the output text. Nor does it price a turn.
            [
                (
                    "toolCall.isError".to_owned(),
                    "the Codex rollout records no error flag for a tool call".to_owned(),
                ),
                (
                    "turn.cost".to_owned(),
                    "the Codex rollout holds token counts but no cost".to_owned(),
                ),
            ]
            .into(),
        )
    })
}

/// A user message's blocks without what Codex injects ahead of the person's words: the project
/// instructions and the environment context ride in as user-role blocks.
fn without_injected_context(content: &Value) -> Value {
    const INJECTED: [&str; 2] = ["# AGENTS.md instructions for", "<environment_context>"];
    let Value::Array(blocks) = content else {
        return content.clone();
    };
    Value::Array(
        blocks
            .iter()
            .filter(|b| {
                let text = b["text"].as_str().unwrap_or("").trim_start();
                !INJECTED.iter().any(|marker| text.starts_with(marker))
            })
            .cloned()
            .collect(),
    )
}

fn record_text(value: &Value) -> Option<String> {
    let text = match value {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| {
                matches!(
                    b["type"].as_str(),
                    Some("input_text" | "output_text" | "text" | "summary_text")
                )
            })
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    (!text.trim().is_empty()).then_some(text)
}

static TRANSCRIPT: LazyLock<StructuredTranscriptCapability> =
    LazyLock::new(|| StructuredTranscriptCapability {
        headless: None,
        interactive: Some(InteractiveStructured {
            session_log_glob: "~/.codex/sessions/{year}/{month}/{day}/rollout-*-{session_id}.jsonl"
                .into(),
            session_id_argv: None,
            format: TranscriptFormat::CodexSessionJsonl,
        }),
    });

/// Its app server's rate limits, one window per bucket's primary and secondary limit.
fn rate_limit_usage(result: &serde_json::Value) -> Result<Vec<UsageWindow>, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Window {
        #[serde(default)]
        used_percent: Option<f64>,
        #[serde(default)]
        resets_at: Option<i64>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Bucket {
        #[serde(default)]
        limit_id: Option<String>,
        #[serde(default)]
        limit_name: Option<String>,
        #[serde(default)]
        primary: Option<Window>,
        #[serde(default)]
        secondary: Option<Window>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Response {
        #[serde(default)]
        rate_limits_by_limit_id: Option<std::collections::BTreeMap<String, Bucket>>,
        #[serde(default)]
        rate_limits: Option<Bucket>,
    }
    let response = Response::deserialize(result).map_err(|error| error.to_string())?;
    let mut buckets = response.rate_limits_by_limit_id.unwrap_or_default();
    if buckets.is_empty() {
        if let Some(bucket) = response.rate_limits {
            let id = bucket.limit_id.clone().unwrap_or_else(|| "codex".into());
            buckets.insert(id, bucket);
        }
    }
    let mut windows = Vec::new();
    for (limit_id, bucket) in buckets {
        let label = bucket.limit_name.unwrap_or_else(|| limit_id.clone());
        for (kind, window) in [("primary", bucket.primary), ("secondary", bucket.secondary)] {
            let (used_percent, resets_at) = window
                .map(|w| (w.used_percent, w.resets_at))
                .unwrap_or((None, None));
            if used_percent.is_some_and(|n| !n.is_finite() || !(0.0..=100.0).contains(&n)) {
                return Err("Codex quota percentage is outside 0..=100".into());
            }
            windows.push(UsageWindow {
                id: format!("{limit_id}:{kind}"),
                label: format!("{label} {kind}"),
                used_percent,
                resets_at,
            });
        }
    }
    Ok(windows)
}

static USAGE: UsagePoll = UsagePoll {
    source: "codex_app_server",
    argv: &["app-server"],
    requests: concat!(
        "{\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"codesense\",\"version\":\"1\"}}}\n",
        "{\"method\":\"initialized\",\"params\":{}}\n",
        "{\"id\":2,\"method\":\"account/rateLimits/read\",\"params\":{}}\n"
    ),
    reply_id: 2,
    parse: rate_limit_usage,
    visible: Duration::from_secs(60),
    hidden: Duration::from_secs(5 * 60),
};

/// The flags that select what config a launch runs with: an override (hooks and MCP servers
/// among them), or a profile.
const CONFIG_SELECTORS: &[&str] = &["-c", "--config", "-p", "--profile"];
/// The flags that turn features on or off.
const FEATURE_FLAGS: &[&str] = &["--enable", "--disable"];
/// The flags among a launch's options that configure Codex's engine rather than its TUI's
/// session: its [`CONFIG_SELECTORS`], and its [`FEATURE_FLAGS`].
const ENGINE_FLAGS: &[&[&str]] = &[CONFIG_SELECTORS, FEATURE_FLAGS];

/// The flag that names an extra workspace root.
const ADD_DIR: &str = "--add-dir";
/// The flags that override one config value.
const OVERRIDES: &[&str] = &["-c", "--config"];
/// The config of the extra roots its sandbox may write to.
const WRITABLE_ROOTS: &str = "sandbox_workspace_write.writable_roots";
/// The config only its engine takes, and what holds it. Its TUI attached beside `--remote`
/// refuses extra roots ("configure additional workspace roots on the server"), and it reads
/// no project config, so an MCP server an override names only in part (an inherited
/// project server switched off) has no transport there.
const ENGINE_ONLY: &[&str] = &[WRITABLE_ROOTS, "mcp_servers"];

/// Whether `key` is `config` or names a value inside it.
fn within(key: &str, config: &str) -> bool {
    key.strip_prefix(config)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}

/// `options` split between Codex's engine and its attached TUI, each flag with its value
/// (next, or joined: [`flag_value`](super::descriptor::flag_value)). The engine takes its
/// [`ENGINE_FLAGS`], with every extra root ([`ADD_DIR`], or one a [`WRITABLE_ROOTS`]
/// override gives) merged into one override after them, each root once. The TUI takes the
/// rest but `--no-daemon` (it runs on no daemon, and refuses that beside `--remote`) and
/// [`ENGINE_ONLY`] config.
fn split(options: &[String]) -> (Vec<String>, Vec<String>) {
    let (mut engine, mut attached, mut roots) = (Vec::new(), Vec::new(), Vec::new());
    let mut rest = options.iter();
    while let Some(option) = rest.next() {
        let (words, value) = match flag_of(option, VALUE_FLAGS) {
            Some(None) => {
                let value = rest.next();
                let words: Vec<String> = std::iter::once(option).chain(value).cloned().collect();
                (words, value.map(String::as_str))
            }
            joined => (vec![option.clone()], joined.flatten()),
        };
        if option == "--no-daemon" {
            continue;
        }
        if flag_of(option, &[&[ADD_DIR]]).is_some() {
            roots.extend(value.map(str::to_owned));
            continue;
        }
        let setting = value
            .filter(|_| flag_of(option, &[OVERRIDES]).is_some())
            .and_then(|value| value.split_once('='))
            .map(|(key, value)| (key.trim(), value));
        if let Some((key, given)) =
            setting.and_then(|(key, value)| Some((key, given_roots(key, value)?)))
        {
            roots.extend(given);
            // An override of the roots alone is replaced by the merged one; one of the
            // table that holds them keeps the rest of the table.
            if key == WRITABLE_ROOTS {
                continue;
            }
        }
        // Config inside an engine-only one, or a table that holds one.
        let engine_only = setting.is_some_and(|(key, _)| {
            ENGINE_ONLY
                .iter()
                .any(|only| within(key, only) || within(only, key))
        });
        if flag_of(option, ENGINE_FLAGS).is_some() {
            engine.extend(words.iter().cloned());
        }
        if !engine_only {
            attached.extend(words);
        }
    }
    let mut merged: Vec<toml::Value> = Vec::new();
    for root in roots.into_iter().map(toml::Value::String) {
        if !merged.contains(&root) {
            merged.push(root);
        }
    }
    if !merged.is_empty() {
        // The roots are quoted inside the value, as `workspace_trust` quotes its paths.
        let roots = toml::Value::Array(merged);
        engine.extend(["-c".into(), format!("{WRITABLE_ROOTS}={roots}")]);
    }
    (engine, attached)
}

/// The [`WRITABLE_ROOTS`] an override of `key` to `value` gives, if it names an array of
/// paths there.
fn given_roots(key: &str, value: &str) -> Option<Vec<String>> {
    let config: toml::Table = format!("{key}={value}").parse().ok()?;
    named_roots(&config)?.ok()
}

/// The [`WRITABLE_ROOTS`] `config` names: `None` for none, `Some(Err)` for a value that is
/// not an array of paths.
fn named_roots(config: &toml::Table) -> Option<Result<Vec<String>, String>> {
    let roots = config
        .get("sandbox_workspace_write")?
        .get("writable_roots")?;
    Some(paths(roots).ok_or_else(|| format!("{WRITABLE_ROOTS} is not a list of paths")))
}

/// The flags that select a profile.
const PROFILE: &[&str] = &["-p", "--profile"];

/// The profile `options` select (before any `--`), the last one given.
fn profile_of(options: &[String]) -> Option<&str> {
    let mut profile = None;
    let mut rest = options.iter().take_while(|option| *option != "--");
    while let Some(option) = rest.next() {
        let value = match flag_of(option, VALUE_FLAGS) {
            Some(None) => rest.next().map(String::as_str),
            joined => joined.flatten(),
        };
        if flag_of(option, &[PROFILE]).is_some() {
            profile = value;
        }
    }
    profile
}

/// `roots`, if it is an array of paths.
fn paths(roots: &toml::Value) -> Option<Vec<String>> {
    roots
        .as_array()?
        .iter()
        .map(|root| root.as_str().map(str::to_owned))
        .collect()
}

/// `option` as one of `flags`: its value next (`Some(None)`) or joined, or `None`.
fn flag_of<'a>(option: &'a str, flags: &[&[&str]]) -> Option<Option<&'a str>> {
    flags
        .iter()
        .flat_map(|flags| flags.iter())
        .find_map(|flag| super::descriptor::flag_value(option, flag))
}

/// How long the engine has to answer a request.
const ENGINE_DEADLINE: Duration = Duration::from_secs(10);
/// The largest message read from the engine.
const MAX_MESSAGE: usize = 4 * 1024 * 1024;

/// The subcommands that run its TUI, besides none: one that resumes or forks a session.
const TUI_SUBCOMMANDS: &[&str] = &["resume", "fork"];
/// Its options that take a value: [`ENGINE_FLAGS`] and [`SESSION_VALUE_FLAGS`].
const VALUE_FLAGS: &[&[&str]] = &[CONFIG_SELECTORS, FEATURE_FLAGS, SESSION_VALUE_FLAGS];
/// Its options besides [`ENGINE_FLAGS`] that take a value.
const SESSION_VALUE_FLAGS: &[&str] = &[
    "-m",
    "--model",
    "-s",
    "--sandbox",
    "-a",
    "--ask-for-approval",
    "-i",
    "--image",
    "-C",
    "--cd",
    "--add-dir",
    "--local-provider",
    "--remote-auth-token-env",
];
/// Its options that only print, running no TUI.
const PRINT_FLAGS: &[&str] = &["-h", "--help", "-V", "--version"];

/// Whether `options` run Codex's TUI on an engine of CodeSense's: none attached to a remote
/// of its own (`--remote`), and their first argument that is neither an option nor an
/// option's value is none, a subcommand that runs the TUI, or a prompt. A single word
/// there may be a subcommand (of a release newer than this list knows), which refuses
/// `--remote`, so only a prompt of several words counts as one.
fn runs_tui(options: &[String]) -> bool {
    let mut options = options.iter();
    while let Some(option) = options.next() {
        if PRINT_FLAGS.contains(&option.as_str()) || flag_of(option, &[&["--remote"]]).is_some() {
            return false;
        }
        if flag_of(option, VALUE_FLAGS) == Some(None) {
            options.next();
        } else if !option.starts_with('-') {
            return TUI_SUBCOMMANDS.contains(&option.as_str())
                || option.contains(char::is_whitespace);
        }
    }
    true
}

/// Codex's engine, its app-server, spoken to in JSON-RPC over the WebSocket it serves on a
/// Unix socket: one connection, and the thread its session's TUI shows.
struct AppServer {
    socket: WebSocket<UnixStream>,
    endpoint: PathBuf,
    /// The thread its TUI shows, and whether it was idle (no turn running) as connected;
    /// `None` when which one it shows is unclear.
    shown: Option<(String, bool)>,
    next_id: u64,
}

/// Why a request got no result.
#[derive(Debug, PartialEq, Eq)]
enum RequestError {
    /// The engine answered it with an error: it had no effect.
    Refused,
    /// The connection failed, or no answer came, once it may have been sent.
    Lost,
}

impl AppServer {
    /// A connection to the engine at `endpoint`, on the thread its TUI shows for
    /// `session` (see [`Self::shown_thread`]).
    fn connect(endpoint: &Path, session: Option<&str>) -> io::Result<Self> {
        let mut server = Self::open(endpoint)?;
        server.shown = server.shown_thread(session)?;
        Ok(server)
    }

    /// A connection to the engine at `endpoint`, initialized, on no thread yet.
    fn open(endpoint: &Path) -> io::Result<Self> {
        let stream = UnixStream::connect(endpoint)?;
        stream.set_read_timeout(Some(ENGINE_DEADLINE))?;
        stream.set_write_timeout(Some(ENGINE_DEADLINE))?;
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE))
            .max_frame_size(Some(MAX_MESSAGE));
        let (socket, _) =
            tungstenite::client::client_with_config("ws://localhost/", stream, Some(config))
                .map_err(|_| io::Error::other("the engine refused the connection"))?;
        let mut server = Self {
            socket,
            endpoint: endpoint.to_path_buf(),
            shown: None,
            next_id: 0,
        };
        let refused = |_| io::Error::other("the engine refused the session");
        server
            .request(
                "initialize",
                json!({"clientInfo": {"name": "codesense", "version": env!("CARGO_PKG_VERSION")}}),
            )
            .map_err(refused)?;
        server.notify("initialized")?;
        Ok(server)
    }

    /// The thread the session's TUI shows: the one its hooks last named (`session`, a
    /// thread id), or with no session named the only root, and in either case the one
    /// a person was latest in. After `/new` the TUI shows a newer, unnamed thread, while the old one
    /// stays loaded: that reads as unclear, and nothing is started where no one sees it.
    /// The engine's own threads (ephemeral ones, such as its reviewer's) and subagents'
    /// are never a TUI's. With it, whether it is idle; `Ok(None)` when it is unclear, an
    /// error when the engine could not be read.
    fn shown_thread(&mut self, session: Option<&str>) -> io::Result<Option<(String, bool)>> {
        let lost = |_| io::Error::other("the engine's threads could not be read");
        let loaded = self
            .request("thread/loaded/list", json!({}))
            .map_err(lost)?;
        // A list that goes on past this page leaves the latest unknown.
        let (Some(ids), true) = (loaded["data"].as_array(), loaded["nextCursor"].is_null()) else {
            return Ok(None);
        };
        let mut threads = Vec::new();
        for id in ids.iter().filter_map(Value::as_str) {
            let read = self
                .request("thread/read", json!({"threadId": id}))
                .map_err(lost)?;
            let thread = &read["thread"];
            if thread["ephemeral"] == true || !thread["parentThreadId"].is_null() {
                continue;
            }
            let idle = thread.pointer("/status/type") == Some(&json!("idle"));
            threads.push((
                id.to_owned(),
                thread["recencyAt"].as_i64().unwrap_or(0),
                idle,
            ));
        }
        let shown = match session {
            Some(session) => threads.iter().find(|(id, ..)| id == session),
            None => match threads.as_slice() {
                [only] => Some(only),
                _ => None,
            },
        };
        Ok(shown
            .filter(|(shown, at, _)| {
                threads
                    .iter()
                    .all(|(id, other, _)| id == shown || other < at)
            })
            .map(|(id, _, idle)| (id.clone(), *idle)))
    }

    /// The request seam: sends `method` with `params` and returns its result, passing
    /// over the notifications, and the requests, the engine sends meanwhile (its TUI
    /// answers those).
    fn request(&mut self, method: &str, params: Value) -> Result<Value, RequestError> {
        self.next_id += 1;
        let id = self.next_id;
        let request = json!({"id": id, "method": method, "params": params});
        self.socket
            .send(Message::text(request.to_string()))
            .map_err(|_| RequestError::Lost)?;
        let deadline = Instant::now() + ENGINE_DEADLINE;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || self.socket.get_ref().set_read_timeout(Some(left)).is_err() {
                return Err(RequestError::Lost);
            }
            // A ping is answered by the socket itself; only text carries replies.
            let text = match self.socket.read() {
                Ok(Message::Text(text)) => text,
                Ok(Message::Close(_)) | Err(_) => return Err(RequestError::Lost),
                Ok(_) => continue,
            };
            let Ok(reply) = serde_json::from_str::<Value>(text.as_str()) else {
                continue;
            };
            if reply.get("method").is_some() || reply.get("id").and_then(Value::as_u64) != Some(id)
            {
                continue;
            }
            return reply.get("result").cloned().ok_or(RequestError::Refused);
        }
    }

    fn notify(&mut self, method: &str) -> io::Result<()> {
        self.socket
            .send(Message::text(json!({"method": method}).to_string()))
            .map_err(io::Error::other)
    }

    /// Whether `thread` holds a user message sent under `client_id`, among its latest
    /// items (where one just sent is); `None` when that can't be read.
    fn holds_message(&mut self, thread: &str, client_id: &str) -> Option<bool> {
        let params = json!({"threadId": thread, "limit": 100, "sortDirection": "desc"});
        let page = self.request("thread/items/list", params).ok()?;
        Some(page["data"].as_array()?.iter().any(|entry| {
            entry["item"]["type"] == "userMessage" && entry["item"]["clientId"] == client_id
        }))
    }
}

impl Channel for AppServer {
    fn ready_session(&mut self) -> Option<String> {
        let (thread, true) = self.shown.clone()? else {
            return None;
        };
        let read = self
            .request("thread/read", json!({"threadId": thread}))
            .ok()?;
        let cwd = read["thread"]["cwd"].as_str()?;
        if !Path::new(cwd).is_absolute() {
            return None;
        }
        let list = self.request("hooks/list", json!({"cwds": [cwd]})).ok()?;
        let entries = list["data"].as_array()?;
        let [entry] = entries.as_slice() else {
            return None;
        };
        if entry["cwd"] != cwd || !entry["errors"].as_array()?.is_empty() {
            return None;
        }
        let hooks = entry["hooks"].as_array()?;
        // A custom untrusted hook can still put a review menu in front of the TUI.
        // Managed hooks are trusted by policy; unknown flags/statuses fail closed.
        if hooks.iter().any(|hook| match hook["enabled"].as_bool() {
            Some(false) => false,
            Some(true) => !matches!(hook["trustStatus"].as_str(), Some("trusted" | "managed")),
            None => true,
        }) {
            return None;
        }
        let command = observer_command();
        for event in OBSERVER_EVENTS {
            let mut name = event.to_string();
            name[..1].make_ascii_lowercase();
            if !hooks.iter().any(|hook| {
                hook["eventName"] == name
                    && hook["command"] == command
                    && hook["handlerType"] == "command"
                    && hook["enabled"] == true
                    && hook["source"] == "sessionFlags"
                    && hook["matcher"].is_null()
                    && hook["async"].as_bool() != Some(true)
            }) {
                return None;
            }
        }
        Some(thread)
    }

    /// Only the thread its TUI shows is started a turn, and only while idle: one started
    /// while a turn runs would join it. A person pressing Enter at that moment shares the
    /// new turn; neither prompt is lost.
    fn start_turn(&mut self, text: &str, client_id: &str) -> SubmitOutcome {
        let Some((thread, true)) = self.shown.clone() else {
            return SubmitOutcome::RefusedNoEffect;
        };
        let params = json!({
            "threadId": thread,
            "clientUserMessageId": client_id,
            "input": [{"type": "text", "text": text}],
        });
        match self.request("turn/start", params) {
            Ok(started) if started.pointer("/turn/id").is_some() => SubmitOutcome::Accepted,
            Err(RequestError::Refused) => SubmitOutcome::RefusedNoEffect,
            // Its answer was lost or unreadable: whether the turn started is read back, by
            // its client id, on a connection of its own.
            _ => {
                let held = Self::open(&self.endpoint)
                    .ok()
                    .and_then(|mut fresh| fresh.holds_message(&thread, client_id));
                match held {
                    Some(true) => SubmitOutcome::Accepted,
                    _ => SubmitOutcome::Uncertain,
                }
            }
        }
    }
}

/// [`Control::channel`]: its engine at `endpoint`, on the thread its TUI shows for
/// `session`.
fn connect(
    endpoint: &Path,
    session: Option<&str>,
    _home: Option<&Path>,
) -> io::Result<Box<dyn Channel>> {
    Ok(Box::new(AppServer::connect(endpoint, session)?))
}

impl AgentCliAdapter for Codex {
    fn descriptor(&self) -> &'static Descriptor {
        &DESCRIPTOR
    }

    fn probe(&self) -> &'static Probe {
        &PROBE
    }

    /// The observer and then the inbox checks ride `-c` flags, ahead of any prompt
    /// operand: the observer's groups keep their place, and so their trust.
    fn prepare(&self, launch: &mut Prepare<'_>) -> Result<(), &'static str> {
        launch.arguments = compose_observer(&launch.arguments, launch.helper)?;
        self.inbox_hooks(launch)?;
        let grants = owned_hook_trust(&launch.arguments)?;
        session_hook_trust(&mut launch.arguments, &grants)?;
        if let Some(home) = &launch.home {
            save_hook_trust(home, &grants)
                .map_err(|_| "Codex hook trust could not be prepared in its native home")?;
        }
        Ok(())
    }

    fn workspace_trust(&self, paths: &[PathBuf]) -> Vec<String> {
        let projects: toml::value::Table = paths
            .iter()
            .filter_map(|path| path.to_str())
            .map(|path| {
                (
                    path.to_owned(),
                    toml::Value::Table(toml::value::Table::from_iter([(
                        "trust_level".into(),
                        toml::Value::String("trusted".into()),
                    )])),
                )
            })
            .collect();
        if projects.is_empty() {
            return Vec::new();
        }
        // Codex splits override *keys* on dots without TOML quoting. Quote path keys
        // inside the value instead, including dots, spaces and non-ASCII characters.
        vec![
            "-c".into(),
            format!("projects={}", toml::Value::Table(projects)),
        ]
    }

    /// Its extra roots reach its engine as one [`WRITABLE_ROOTS`] override, which replaces
    /// the roots its config gives, as the roots of a profile `options` select (its
    /// `<name>.config.toml`) replace those of its `config.toml` (checked against Codex
    /// 0.162.1), so a launch carries the ones it would run with. A config that names none,
    /// or no config, gives none.
    fn configured_roots(
        &self,
        home: Option<&Path>,
        options: &[String],
    ) -> Result<Vec<String>, String> {
        let home = home.ok_or("its Codex home is unknown")?;
        let profile = profile_of(options).map(|profile| format!("{profile}.config.toml"));
        for file in profile.into_iter().chain(["config.toml".to_owned()]) {
            let path = home.join(file);
            let read = |reason: &dyn std::fmt::Display| format!("{}: {reason}", path.display());
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(read(&error)),
            };
            let config: toml::Table = text
                .parse()
                .map_err(|error: toml::de::Error| read(&error.message().replace('\n', ", ")))?;
            if let Some(roots) = named_roots(&config) {
                return roots.map_err(|reason| read(&reason));
            }
        }
        Ok(Vec::new())
    }

    /// Its app-server, which takes its engine config (hooks and MCP servers run there),
    /// and its TUI attached to it with its session's options. A run with no TUI (one-shot,
    /// or another subcommand) takes none.
    fn channel(&self, options: &[String]) -> Option<NativeChannel> {
        if DESCRIPTOR.one_shot(options) || !runs_tui(options) {
            return None;
        }
        let endpoint = format!("unix://{CHANNEL_ENDPOINT}");
        let (mut engine, mut attached) = split(options);
        engine.extend(["app-server".into(), "--listen".into(), endpoint.clone()]);
        attached.extend(["--remote".into(), endpoint]);
        Some(NativeChannel {
            engine,
            attached,
            serves_at: None,
        })
    }

    /// An async hook's late exit is dropped, so nothing it runs can wake it: its host
    /// starts a turn through its engine while it is idle, and its checks hand the notice
    /// over while it works. Nothing is typed into its terminal.
    fn inbox(&self) -> InboxDelivery {
        InboxDelivery::HostSubmit { checks: CHECKS }
    }

    fn inbox_hooks(&self, launch: &mut Prepare<'_>) -> Result<(), &'static str> {
        launch.arguments = compose(&launch.arguments, &check_flags()?)?;
        Ok(())
    }

    fn submit_notice(&self, channel: &mut dyn Channel, notice: &Notice<'_>) -> SubmitOutcome {
        channel.start_turn(notice.text, notice.token)
    }

    /// A qualified Codex routes child starts and stops separately and names the child on
    /// the tool and submit callbacks it inherits, so its root hooks are its main turn's;
    /// an unqualified one's are not known to be.
    fn normalize(&self, input: &[u8], qualified: bool) -> Option<Report> {
        let hook = HookInput::parse(input)?;
        let event = hook.shared_event()?;
        let main = qualified || event.0 == Event::Interrupted;
        hook.report(AgentCli::Codex, event, main)
    }

    /// Whether its hooks report its main turn is known only once a release qualified.
    fn session_policy(&self) -> SessionPolicy {
        SessionPolicy {
            bind_on_submit: false,
            rebind_on_start: true,
            rebind_to_retired: false,
            observed_from_start: false,
            subagents: Subagents::Reported,
        }
    }

    /// Its qualified activity hooks report attention through normalize().
    fn status_hooks(&self) -> &'static [StatusHook] {
        &[]
    }

    /// Ctrl-C uses the normal terminal key path; its Interrupt hook ends only the
    /// current turn. The canonical slash command shuts down the interactive CLI.
    fn control(&self) -> Control {
        Control {
            native_readiness: true,
            unready_bound_ms: None,
            exit_line: "/quit",
            bracketed_paste: true,
            submit_starts_turn: true,
            command_prefix: Some('/'),
            channel: Some(connect),
        }
    }

    fn output(&self) -> Output {
        Output {
            session_log: Some(locate_rollout),
            session_record: Some(session_record),
            transcript: Some(&TRANSCRIPT),
            usage: Usage::Poll(&USAGE),
        }
    }

    fn restore_tags(&self) -> RestoreTags {
        RestoreTags {
            permission_mode: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::Scope;

    #[test]
    fn attribution_edit_transports_only_owned_hooks_and_preserves_user_arguments() {
        let events: &'static [&'static str] = &["PreToolUse", "PostToolUse", "Stop"];
        let secret = ["ghp_", "abcdefghijklmnopqrstuvwxyz0123"].concat();
        let user = vec![
            "-c".to_owned(),
            format!("mcp_servers.x.env.TOKEN=\"{secret}\""),
            "token=example1234".into(),
        ];
        let command = "git-ai checkpoint # codesense-git-ai";
        let apply = |arguments: &[String], edit: &AttributionHookEdit| {
            let mut arguments = arguments.to_vec();
            for pair in edit.remove_args.chunks_exact(2) {
                let at = arguments
                    .windows(2)
                    .position(|tokens| tokens == pair)
                    .unwrap();
                arguments.drain(at..at + 2);
            }
            arguments.extend(edit.extra_args.clone());
            arguments
        };
        let mut args = user.clone();
        for command in [Some(command), None, Some(command), Some(command)] {
            let edit = attribution_hook_edit(&args, command, events, 10).unwrap();
            for tokens in [&edit.remove_args, &edit.extra_args] {
                for pair in tokens.chunks_exact(2) {
                    assert_eq!(pair[0], "-c");
                    assert!(pair[1].starts_with("hooks."));
                    assert!(!pair[1].contains(&secret));
                    assert!(!pair[1].contains("token=example1234"));
                    assert!(!pair[1].contains("mcp_servers.x"));
                }
            }
            args = apply(&args, &edit);
            assert_eq!(&args[..user.len()], user);
            assert_eq!(
                args.join(" ").contains(ATTRIBUTION_HOOK_MARKER),
                command.is_some()
            );
            assert!(
                args.iter()
                    .filter(|arg| arg.starts_with("hooks.state="))
                    .count()
                    <= 1
            );
        }
    }

    #[test]
    fn attribution_refresh_removes_checkpoint_trust_and_keeps_other_hooks() {
        let events: &'static [&'static str] = &["PreToolUse", "PostToolUse", "Stop"];
        let original = vec![
            "-c".into(),
            r#"hooks.SubagentStop=[{hooks=[{type="command",command="user-stop"}]}]"#.into(),
        ];
        let command = "git-ai checkpoint # codesense-git-ai-checkpoint";
        let on = refresh_attribution_hook_args(&original, Some(command), events, 10).unwrap();
        let grants = attribution_hook_trust(&on).unwrap();
        assert_eq!(grants.len(), 3);
        let off = refresh_attribution_hook_args(&on, None, events, 10).unwrap();
        assert!(!off.join(" ").contains(ATTRIBUTION_HOOK_MARKER));
        assert!(off.join(" ").contains("user-stop"));
        // Observer groups can reuse checkpoint positions, but never checkpoint hashes.
        for (_, hash) in grants {
            assert!(!off.join(" ").contains(&hash));
        }
        let again = refresh_attribution_hook_args(&off, Some(command), events, 10).unwrap();
        let repeated = refresh_attribution_hook_args(&again, Some(command), events, 10).unwrap();
        assert_eq!(
            attribution_hook_trust(&again).unwrap(),
            attribution_hook_trust(&repeated).unwrap()
        );
        assert_eq!(
            repeated
                .iter()
                .filter(|arg| arg.starts_with("hooks.state="))
                .count(),
            1
        );
    }

    const CX4_ID: &str = "01a15600-0001-7000-8000-000000000002";
    const CX4_OTHER: &str = "01a15600-0000-7000-b000-000000000001";
    const CX4_RECORD: &str = r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}}
{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"checking"}]}}
{"type":"response_item","payload":{"type":"function_call","call_id":"call-1","name":"shell","arguments":"{\"command\":\"echo ok\"}"}}
{"type":"response_item","payload":{"type":"function_call_output","call_id":"call-1","output":"ok"}}
{"type":"response_item","payload":{"type":"custom_tool_call","call_id":"call-2","name":"exec","input":"sleep 5"}}
{"type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"call-2","output":[{"type":"input_text","text":"done"}]}}
{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":12,"output_tokens":3}}}}
{"type":"response_item","payload":{"type":"reasoning","encrypted_content":"never render reasoning"}}
{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"never render instructions"}]}}
{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"finished"}]}}
"#;

    fn cx4_write(home: &Path, id: &str, header_id: &str, body: &str) -> PathBuf {
        let dir = home.join("sessions/2026/10/19");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("rollout-2026-10-19T22-14-17-{id}.jsonl"));
        std::fs::write(
            &file,
            format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{header_id}\"}}}}\n{body}"),
        )
        .unwrap();
        file
    }

    const ROLLOUT_0_164: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/codex/rollout-0.164.0.jsonl"
    ));

    #[test]
    fn a_real_shaped_rollout_keeps_the_request_and_never_invents_an_error_flag() {
        let t = parse_session_record(ROLLOUT_0_164).expect("parses");
        let roles: Vec<&str> = t.turns.iter().map(|t| t.role.as_str()).collect();
        assert_eq!(
            roles,
            ["user", "assistant", "assistant", "user"],
            "{roles:?}"
        );
        assert_eq!(
            t.turns[0].text.as_deref(),
            Some("Please read auth.py and fix the bug"),
            "injected AGENTS.md and environment context are not the person's request"
        );
        let calls: Vec<_> = t.turns[1].tool_calls.iter().collect();
        assert_eq!(
            calls.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["exec_command", "apply_patch", "read_messages"]
        );
        assert!(
            calls.iter().all(|c| c.is_error.is_none()),
            "the rollout has no error flag, so none is reported"
        );
        assert!(calls.iter().all(|c| c.result.is_some()));
        assert!(
            t.unavailable.contains_key("toolCall.isError"),
            "the gap is stated: {:?}",
            t.unavailable
        );
    }

    /// Fails on main: only `call_id` is indexed, so a file-change item id (`exec-…`) names no call.
    #[test]
    fn an_item_completed_inside_an_open_call_is_an_alias_of_that_call() {
        let rows = [
            r#"{"timestamp":"2026-10-28T14:43:18Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"go"}]}}"#,
            // Before any call: no enclosing call, so no alias.
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"FileChange","id":"exec-early"}}}"#,
            r#"{"type":"response_item","payload":{"type":"custom_tool_call","call_id":"call_a","name":"exec","input":"x"}}"#,
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"FileChange","id":"exec-1"}}}"#,
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"FileChange","id":"exec-1"}}}"#,
            r#"{"type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"call_a","output":"ok"}}"#,
            // After the output: the call is closed.
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"FileChange","id":"exec-late"}}}"#,
            r#"{"type":"response_item","payload":{"type":"custom_tool_call","call_id":"call_b","name":"exec","input":"y"}}"#,
            r#"{"type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"call_b","output":"ok"}}"#,
        ];
        let t = parse_rollout_lines(rows).expect("parses");
        let calls: Vec<_> = t.turns.iter().flat_map(|t| &t.tool_calls).collect();
        assert_eq!(calls.len(), 2, "an alias is not a tool call");
        assert_eq!(calls[0].native_id.as_deref(), Some("call_a"));
        assert_eq!(calls[0].aliases, ["exec-1"]);
        assert!(calls[1].aliases.is_empty());
    }

    #[test]
    fn a_real_shaped_rollout_fills_the_neutral_fields_it_records() {
        let t = parse_session_record(ROLLOUT_0_164).expect("parses");
        let indexes: Vec<_> = t.turns.iter().map(|t| t.index).collect();
        assert_eq!(indexes, [Some(0), Some(1), Some(2), Some(3)]);
        let first = &t.turns[1];
        assert_eq!(first.model.as_deref(), Some("gpt-6.1-sol"));
        assert_eq!(
            first
                .reasoning
                .as_ref()
                .map(|r| (r.kind, r.text.as_deref())),
            Some((output::ReasoningKind::Encrypted, None)),
            "the rollout holds only an encrypted reasoning blob"
        );
        assert!(first.ts_ms.is_some());
        let calls = &first.tool_calls;
        assert_eq!(calls[0].native_id.as_deref(), Some("exec-0001"));
        assert_eq!(calls[2].mcp_server.as_deref(), Some("codesense"));
        assert_eq!(calls[0].mcp_server, None);
        assert_eq!(t.turns[0].model, None, "a request has no model");
    }

    #[test]
    fn cx4_native_record_is_id_bound_and_parses_neutral_turns() {
        let home = tempfile::tempdir().unwrap();
        cx4_write(home.path(), CX4_ID, CX4_ID, CX4_RECORD);
        cx4_write(home.path(), CX4_OTHER, CX4_OTHER, "unrelated newer file");
        let read = Codex.output().session_record.expect("native record reader");
        let record =
            read(home.path(), CX4_ID).expect("captured id resolves even with a newer sibling");
        assert_eq!(record.session_id, CX4_ID);
        assert!(!record.raw.contains("unrelated"));
        let transcript = record.format.parse_native(&record.raw).unwrap();
        assert_eq!(transcript.turns.len(), 3);
        assert_eq!(transcript.turns[0].text.as_deref(), Some("hello"));
        let assistant = &transcript.turns[1];
        assert_eq!(assistant.tool_calls.len(), 2);
        assert_eq!(assistant.tool_calls[0].result.as_deref(), Some("ok"));
        assert_eq!(assistant.tool_calls[1].result.as_deref(), Some("done"));
        assert_eq!(assistant.usage.as_ref().unwrap().input_tokens, Some(12));
        assert_eq!(transcript.turns[2].text.as_deref(), Some("finished"));
        let json = serde_json::to_string(&transcript).unwrap();
        assert!(!json.contains("never render"));
    }

    #[test]
    fn cx4_native_record_accepts_normal_metadata_with_full_base_instructions() {
        let home = tempfile::tempdir().unwrap();
        let file = cx4_write(home.path(), CX4_ID, CX4_ID, "");
        let header = serde_json::json!({
            "type": "session_meta",
            "payload": {"id": CX4_ID, "base_instructions": {"text": "instructions ".repeat(3000)}}
        });
        std::fs::write(file, format!("{header}\n{CX4_RECORD}")).unwrap();
        let record = Codex.output().session_record.unwrap()(home.path(), CX4_ID)
            .expect("normal native metadata may exceed 16 KiB");
        let transcript = record.format.parse_native(&record.raw).unwrap();
        assert_eq!(
            transcript.turns.last().unwrap().text.as_deref(),
            Some("finished")
        );
    }

    #[test]
    fn cx4_native_record_refuses_missing_unsafe_mismatched_and_ambiguous_ids() {
        let home = tempfile::tempdir().unwrap();
        let read = Codex.output().session_record.unwrap();
        assert!(read(home.path(), CX4_ID).is_none());
        assert!(read(home.path(), "../../outside").is_none());
        cx4_write(home.path(), CX4_ID, CX4_OTHER, CX4_RECORD);
        assert!(
            read(home.path(), CX4_ID).is_none(),
            "filename cannot override native metadata identity"
        );
        let file = cx4_write(home.path(), CX4_ID, CX4_ID, CX4_RECORD);
        let other_dir = home.path().join("sessions/2026/10/20");
        std::fs::create_dir_all(&other_dir).unwrap();
        std::fs::copy(&file, other_dir.join(file.file_name().unwrap())).unwrap();
        assert!(
            read(home.path(), CX4_ID).is_none(),
            "ambiguous native records cannot be selected arbitrarily"
        );
    }

    #[test]
    fn cx4_native_record_refuses_symlinks_and_bounds_large_or_malformed_records() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let read = Codex.output().session_record.unwrap();
        let file = cx4_write(outside.path(), CX4_ID, CX4_ID, CX4_RECORD);
        std::os::unix::fs::symlink(
            outside.path().join("sessions"),
            home.path().join("sessions"),
        )
        .unwrap();
        assert!(read(home.path(), CX4_ID).is_none());
        std::fs::remove_file(home.path().join("sessions")).unwrap();
        let dest = cx4_write(home.path(), CX4_ID, CX4_ID, "");
        std::fs::remove_file(&dest).unwrap();
        std::os::unix::fs::symlink(&file, &dest).unwrap();
        assert!(read(home.path(), CX4_ID).is_none());
        std::fs::remove_file(&dest).unwrap();
        let large = format!("{}\n{CX4_RECORD}", "x".repeat(2 * 1024 * 1024));
        cx4_write(home.path(), CX4_ID, CX4_ID, &large);
        let record = read(home.path(), CX4_ID).unwrap();
        assert!(record.raw.len() <= 1024 * 1024 + 4);
        assert!(record.format.parse_native(&record.raw).is_some());
        cx4_write(home.path(), CX4_ID, CX4_ID, "malformed\n{}\n");
        let record = read(home.path(), CX4_ID).unwrap();
        assert!(
            record.format.parse_native(&record.raw).is_none(),
            "bounded text fallback remains available"
        );
    }

    #[test]
    fn cx4_output_declares_a_captured_session_record() {
        let output = Codex.output();
        assert!(output
            .transcript
            .and_then(|t| t.interactive.as_ref())
            .is_some());
        assert!(output.session_record.is_some());
    }

    #[test]
    fn cx4_missing_quota_windows_are_unknown_not_zero_or_tokens() {
        let windows = rate_limit_usage(&json!({"rateLimitsByLimitId":{"codex":{
            "primary":{"usedPercent":0,"resetsAt":200},"secondary":null
        }}, "total_tokens":99999}))
        .unwrap();
        assert_eq!(
            windows
                .iter()
                .find(|w| w.id == "codex:primary")
                .unwrap()
                .used_percent,
            Some(0.0)
        );
        assert_eq!(
            windows
                .iter()
                .find(|w| w.id == "codex:secondary")
                .expect("explicit unknown window")
                .used_percent,
            None
        );
        assert!(rate_limit_usage(&json!({})).unwrap().is_empty());
        assert!(
            rate_limit_usage(&json!({"rateLimits":null,"rateLimitsByLimitId":null}))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn cx4_usage_accepts_the_real_legacy_rate_limits_bucket() {
        let windows = rate_limit_usage(&serde_json::json!({"rateLimits": {
            "limitId":"codex", "limitName":"Codex", "primary":{"usedPercent":43,"resetsAt":200}, "secondary":null
        }})).unwrap();
        assert!(windows.iter().any(|w| w.id == "codex:primary"));
    }

    #[test]
    fn cx3_fresh_engine_wakes_before_any_prompt_hook() {
        let dir = tempfile::tempdir().unwrap();
        let (endpoint, seen) = app_server(dir.path(), Script::default());
        let outlet = std::sync::Arc::new(Outlet::default());
        let owner = crate::activity::ActivityOwner::new();
        owner.set_inbox_outlet(outlet.clone());
        let writer = owner
            .prepare_native("fresh", AgentCli::Codex, "unused")
            .unwrap();
        owner.register("fresh", Some(std::process::id())).unwrap();
        owner.bind_engine_for_test(&writer, endpoint).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !owner.snapshot("fresh").unwrap().observed && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            owner.snapshot("fresh").unwrap().takes_input(),
            "native engine must establish initial readiness"
        );
        owner
            .inbox_mail(
                "fresh",
                "mail before first prompt".into(),
                "fresh-mail".into(),
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while outlet.0.lock().unwrap().is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(sent(&seen, "turn/start")[0]["threadId"], "thread-1");
        assert_eq!(
            *outlet.0.lock().unwrap(),
            [("fresh-mail".into(), crate::inbox_host::Fate::Told)]
        );
    }

    #[test]
    fn cx3_native_readiness_refuses_review_menus_busy_or_ambiguous_threads() {
        for script in [
            Script {
                hook_trust: "untrusted",
                ..Script::default()
            },
            Script {
                hook_trust: "modified",
                ..Script::default()
            },
            Script {
                status: "active",
                ..Script::default()
            },
            Script {
                threads: vec![json!({"id":"one"}), json!({"id":"two"})],
                ..Script::default()
            },
            Script {
                threads: vec![json!({"id":"child", "parentThreadId":"parent"})],
                ..Script::default()
            },
            Script {
                hooks: Some(json!({"data":[{"cwd":"/workspace", "hooks":[], "errors":[]}]})),
                ..Script::default()
            },
            Script {
                hooks: Some(
                    json!({"data":[{"cwd":"/workspace", "hooks":[], "errors":[{"message":"bad config"}]}]}),
                ),
                ..Script::default()
            },
        ] {
            let dir = tempfile::tempdir().unwrap();
            let (endpoint, seen) = app_server(dir.path(), script);
            assert!(AppServer::connect(&endpoint, None)
                .unwrap()
                .ready_session()
                .is_none());
            assert!(sent(&seen, "turn/start").is_empty());
        }
    }

    #[test]
    fn cx3_native_readiness_requires_every_owned_observer_and_all_enabled_hook_trust() {
        let dir = tempfile::tempdir().unwrap();
        let (endpoint, _) = app_server(dir.path(), Script::default());
        let original = AppServer::connect(&endpoint, None)
            .unwrap()
            .request("hooks/list", json!({"cwds":["/workspace"]}))
            .unwrap();
        for change in [
            "disabled",
            "async",
            "foreign",
            "matcher",
            "command",
            "missing",
            "custom-review",
        ] {
            let mut hooks = original.clone();
            let own = &mut hooks["data"][0]["hooks"][0];
            match change {
                "disabled" => own["enabled"] = json!(false),
                "async" => own["async"] = json!(true),
                "foreign" => own["source"] = json!("user"),
                "matcher" => own["matcher"] = json!("Bash"),
                "command" => own["command"] = json!("echo not-an-observer"),
                "missing" => {
                    hooks["data"][0]["hooks"].as_array_mut().unwrap().pop();
                }
                "custom-review" => {
                    hooks["data"][0]["hooks"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"enabled":true,"trustStatus":"untrusted"}));
                }
                _ => unreachable!(),
            }
            let other = tempfile::tempdir().unwrap();
            let (endpoint, _) = app_server(
                other.path(),
                Script {
                    hooks: Some(hooks),
                    ..Script::default()
                },
            );
            assert!(
                AppServer::connect(&endpoint, None)
                    .unwrap()
                    .ready_session()
                    .is_none(),
                "{change}"
            );
        }
    }

    #[test]
    fn cx3_native_probe_cannot_replace_hook_evidence_or_revive_a_retired_launch() {
        let dir = tempfile::tempdir().unwrap();
        let (endpoint, _) = app_server(dir.path(), Script::default());
        let owner = crate::activity::ActivityOwner::new();
        let writer = owner
            .prepare_native("fresh", AgentCli::Codex, "unused")
            .unwrap();
        owner.register("fresh", Some(std::process::id())).unwrap();
        for event in ["SessionStart", "UserPromptSubmit", "PermissionRequest"] {
            let input = json!({"hook_event_name":event,"session_id":"thread-1"});
            let report = Codex
                .normalize(&serde_json::to_vec(&input).unwrap(), true)
                .unwrap();
            owner
                .report_for_test(&writer, std::process::id(), report)
                .unwrap();
        }
        owner.bind_engine_for_test(&writer, endpoint).unwrap();
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            owner.snapshot("fresh").unwrap().state,
            crate::activity::State::NeedsPermission
        );
        owner.remove("fresh", &writer.identity.session_incarnation);
        assert!(owner.snapshot("fresh").is_none());
    }

    #[test]
    fn cx3_interrupt_finishes_the_turn_without_retiring_the_session() {
        let owner = crate::activity::ActivityOwner::new();
        let writer = owner
            .prepare_native("interrupt", AgentCli::Codex, "unused")
            .unwrap();
        owner
            .register("interrupt", Some(std::process::id()))
            .unwrap();
        for event in ["SessionStart", "UserPromptSubmit", "Interrupt"] {
            let input = json!({"hook_event_name":event,"session_id":"thread-1"});
            owner
                .report_for_test(
                    &writer,
                    std::process::id(),
                    Codex
                        .normalize(&serde_json::to_vec(&input).unwrap(), true)
                        .unwrap(),
                )
                .unwrap();
        }
        let snapshot = owner.snapshot("interrupt").unwrap();
        assert!(snapshot.takes_input());
        assert!(snapshot.exit.is_none());
        assert_eq!(snapshot.turn_generation, 1);
    }

    #[test]
    fn cx3_control_uses_its_canonical_shutdown_command() {
        assert_eq!(Codex.control().exit_line, "/quit");
    }

    #[test]
    fn cx3_new_thread_rebinds_only_on_a_qualified_root_start() {
        let owner = crate::activity::ActivityOwner::new();
        let writer = owner
            .prepare_native("new-thread", AgentCli::Codex, "unused")
            .unwrap();
        owner
            .register("new-thread", Some(std::process::id()))
            .unwrap();
        let report = |session: &str, qualified: bool, child: bool| {
            let mut input = json!({"hook_event_name":"SessionStart", "session_id":session});
            if child {
                input["agent_id"] = json!("child");
            }
            let report = Codex
                .normalize(&serde_json::to_vec(&input).unwrap(), qualified)
                .unwrap();
            owner.report_for_test(&writer, std::process::id(), report)
        };
        report("before-new", true, false).unwrap();
        assert!(report("after-new", true, true).is_err());
        assert!(report("after-new", false, false).is_err());
        let next = report("after-new", true, false).unwrap();
        assert_eq!(next.identity, writer.identity);
        assert_eq!(next.turn_generation, 1);
        assert!(
            report("before-new", true, false).is_err(),
            "retired thread stays retired"
        );
    }

    #[test]
    fn cx3_control_requires_prompt_readiness_and_a_started_turn() {
        let control = Codex.control();
        assert!(control.native_readiness);
        assert!(
            control.bracketed_paste,
            "wait for the TUI input and delimit a pasted prompt"
        );
        assert!(
            control.submit_starts_turn,
            "terminal redraws are not submission proof"
        );
    }

    #[test]
    fn cx2_live_group_writable_owned_home_can_prepare_generated_trust() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o775)).unwrap();
        let mut launch = Prepare::new(vec![], json!({}), "/server/host");
        launch.home = Some(home.path().to_owned());
        Codex.prepare(&mut launch).unwrap();
        let config = home.path().join("config.toml");
        assert_eq!(
            std::fs::metadata(config).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            Codex.prepare(&mut launch).is_err(),
            "world-writable homes remain refused"
        );
    }

    #[test]
    fn cx2_review_joined_user_hooks_are_prepared_without_trusting_them() {
        let value = "hooks.Stop=[{hooks=[{type=\"command\",command=\"user-stop\",timeout=9}]}]";
        for arguments in [
            vec!["-c".into(), value.into()],
            vec![format!("--config={value}")],
            vec![format!("-c={value}")],
        ] {
            let mut launch = Prepare::new(arguments, json!({}), "/app/helper");
            Codex.prepare(&mut launch).unwrap();
            let value = launch
                .arguments
                .iter()
                .filter_map(|arg| arg.strip_prefix("hooks.state="))
                .next_back()
                .unwrap();
            let table: toml::Table = format!("state={value}").parse().unwrap();
            let state = table["state"].as_table().unwrap();
            assert!(!state.contains_key("/<session-flags>/config.toml:stop:0:0"));
            assert!(state.contains_key("/<session-flags>/config.toml:stop:1:0"));
        }
    }

    #[test]
    fn cx2_invalid_launch_state_leaves_the_native_home_unchanged() {
        let home = tempfile::tempdir().unwrap();
        let mut launch = Prepare::new(
            vec!["-c".into(), "hooks.state=false".into()],
            json!({}),
            "/server/v1/host",
        );
        launch.home = Some(home.path().to_owned());
        assert!(Codex.prepare(&mut launch).is_err());
        assert!(!home.path().join("config.toml").exists());
    }

    #[test]
    fn cx2_each_launch_keeps_its_trust_and_does_not_replace_foreign_hashes() {
        let home = tempfile::tempdir().unwrap();
        let config = home.path().join("config.toml");
        let key = "/<session-flags>/config.toml:session_start:0:0";
        std::fs::write(
            &config,
            format!(
                "[hooks.state.{}]\ntrusted_hash=\"user-reviewed\"\nenabled=false\n",
                serde_json::to_string(key).unwrap()
            ),
        )
        .unwrap();
        let mut launch = Prepare::new(vec![], json!({}), "/server/v1/host");
        launch.home = Some(home.path().to_owned());
        Codex.prepare(&mut launch).unwrap();
        let persisted: toml::Value = std::fs::read_to_string(config).unwrap().parse().unwrap();
        assert_eq!(
            persisted["hooks"]["state"][key]["trusted_hash"].as_str(),
            Some("user-reviewed")
        );
        let state = launch
            .arguments
            .iter()
            .filter_map(|arg| arg.strip_prefix("hooks.state="))
            .next_back()
            .expect("per-launch grants survive concurrent home updates");
        let state: toml::Table = format!("state={state}").parse().unwrap();
        assert_eq!(
            state["state"][key]["trusted_hash"].as_str(),
            Some("sha256:565cf7db41609ffd35be847981451331d89e4995feee0df06a189d0ac68c320f")
        );
        assert!(
            state["state"][key].get("enabled").is_none(),
            "never re-enable a disabled handler"
        );
    }

    #[test]
    fn cx2_trust_refuses_malformed_and_symlinked_configs_without_damage() {
        let home = tempfile::tempdir().unwrap();
        let config = home.path().join("config.toml");
        let grants = vec![(
            "/<session-flags>/config.toml:stop:0:0".into(),
            "sha256:owned".into(),
        )];
        for text in ["broken = [", "hooks = false", "[hooks]\nstate=false"] {
            std::fs::write(&config, text).unwrap();
            assert!(save_hook_trust(home.path(), &grants).is_err());
            assert_eq!(std::fs::read_to_string(&config).unwrap(), text);
        }
        std::fs::remove_file(&config).unwrap();
        let target = home.path().join("personal.toml");
        std::fs::write(&target, "model=\"personal\"\n").unwrap();
        std::os::unix::fs::symlink(&target, &config).unwrap();
        assert!(save_hook_trust(home.path(), &grants).is_err());
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "model=\"personal\"\n"
        );
        assert!(std::fs::symlink_metadata(&config)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn cx2_concurrent_owned_trust_updates_preserve_every_handler() {
        let home = tempfile::tempdir().unwrap();
        std::thread::scope(|threads| {
            for index in 0..8 {
                let home = home.path();
                threads.spawn(move || {
                    save_hook_trust(home, &[(format!("owned:{index}"), format!("hash-{index}"))])
                        .unwrap()
                });
            }
        });
        let value: toml::Value = std::fs::read_to_string(home.path().join("config.toml"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(value["hooks"]["state"].as_table().unwrap().len(), 8);
    }

    #[test]
    fn cx2_machine_prerequisite_is_read_before_runtime_start() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("setting");
        let mut check = Codex.descriptor().machine_check.unwrap();
        check.path = Box::leak(path.to_str().unwrap().to_owned().into_boxed_str());
        std::fs::write(&path, "1\n").unwrap();
        assert!(check.verify().unwrap_err().contains("sudo sysctl --system"));
        std::fs::write(&path, "0\n").unwrap();
        check.verify().unwrap();
        std::fs::remove_file(&path).unwrap();
        check.verify().unwrap();
    }

    #[test]
    fn cx2_trust_is_written_in_the_selected_home_and_user_state_is_preserved() {
        let home = tempfile::tempdir().unwrap();
        let config = home.path().join("config.toml");
        std::fs::write(&config, "# keep my comment\nmodel=\"my-model\"\n[hooks.state.user]\ntrusted_hash=\"do-not-touch\"\nenabled=false\n").unwrap();
        let mut launch = Prepare::new(vec![], json!({}), "/server/v1/host");
        launch.home = Some(home.path().to_owned());
        Codex.prepare(&mut launch).unwrap();
        let text = std::fs::read_to_string(&config).unwrap();
        let value: toml::Value = text.parse().unwrap();
        assert!(value["hooks"]["state"]
            .get("/<session-flags>/config.toml:session_start:0:0")
            .is_some());
        assert_eq!(
            value["hooks"]["state"]["user"]["trusted_hash"].as_str(),
            Some("do-not-touch")
        );
        assert_eq!(
            value["hooks"]["state"]["user"]["enabled"].as_bool(),
            Some(false)
        );
        assert!(text.starts_with("# keep my comment"));
    }

    #[test]
    fn cx2_restricted_ubuntu_sandbox_names_the_explicit_fix() {
        let check = Codex
            .descriptor()
            .machine_check
            .expect("runtime prerequisite");
        assert_eq!(
            check.path,
            "/proc/sys/kernel/apparmor_restrict_unprivileged_userns"
        );
        for value in ["", "0", "not available"] {
            assert!(check.warning(value).is_none());
        }
        let warning = check.warning("1\n").unwrap();
        assert!(warning.contains(
            "sudo tee /etc/sysctl.d/60-codex-sandbox-userns.conf && sudo sysctl --system"
        ));
        assert!(warning.contains("kernel.apparmor_restrict_unprivileged_userns = 0"));
    }

    #[test]
    fn cx2_folder_trust_is_a_launch_override_for_the_real_home() {
        let paths = [PathBuf::from("/managed/a.b \"work\"")];
        let flags = Codex.workspace_trust(&paths);
        assert_eq!(flags.len(), 2);
        assert_eq!(flags[0], "-c");
        let value: toml::Table = flags[1].parse().unwrap();
        assert_eq!(
            value["projects"]["/managed/a.b \"work\""]["trust_level"].as_str(),
            Some("trusted")
        );
    }

    #[test]
    fn cx2_owned_hooks_have_launch_trust_without_trusting_user_hooks() {
        let home = tempfile::tempdir().unwrap();
        let mut launch = Prepare::new(
            vec![
                "-c".into(),
                "hooks.Stop=[{hooks=[{type=\"command\",command=\"user-stop\",timeout=9}]}]".into(),
            ],
            json!({}),
            "/server/v1/host",
        );
        launch.home = Some(home.path().to_owned());
        Codex.prepare(&mut launch).unwrap();
        let value: toml::Value = std::fs::read_to_string(home.path().join("config.toml"))
            .unwrap()
            .parse()
            .unwrap();
        let trusts = value["hooks"]["state"].as_table().unwrap();
        assert_eq!(trusts.len(), OBSERVER_EVENTS.len() + CHECKS.len());
        assert!(!trusts.contains_key("/<session-flags>/config.toml:stop:0:0"));
        assert!(trusts.contains_key("/<session-flags>/config.toml:stop:1:0"));
        assert_eq!(
            trusts["/<session-flags>/config.toml:session_start:0:0"]["trusted_hash"].as_str(),
            Some("sha256:565cf7db41609ffd35be847981451331d89e4995feee0df06a189d0ac68c320f")
        );
        let before = std::fs::read(home.path().join("config.toml")).unwrap();
        Codex.prepare(&mut launch).unwrap();
        assert_eq!(
            std::fs::read(home.path().join("config.toml")).unwrap(),
            before
        );
    }

    /// Git-ai's checkpoint hooks are trusted like the observer's. A hook Codex does
    /// not trust is skipped silently, and every line would then read as human.
    #[test]
    fn git_ai_checkpoint_hooks_are_trusted_at_their_composed_place_and_user_hooks_are_not() {
        use sha2::{Digest, Sha256};
        let command =
            "env HOME='/m' '/bin/git-ai' checkpoint codex --hook-input stdin # codesense-git-ai";
        let user = vec![
            "-c".to_owned(),
            "hooks.SubagentStop=[{hooks=[{type=\"command\",command=\"user-stop\",timeout=9}]}]"
                .into(),
        ];
        let mut arguments = user.clone();
        arguments.extend(
            attribution_hook_flags(command, &["PreToolUse", "PostToolUse", "Stop"], 5, &user)
                .unwrap(),
        );
        let home = tempfile::tempdir().unwrap();
        let mut launch = Prepare::new(arguments, json!({}), "/server/v1/host");
        launch.home = Some(home.path().to_owned());
        Codex.prepare(&mut launch).unwrap();
        let value: toml::Value = std::fs::read_to_string(home.path().join("config.toml"))
            .unwrap()
            .parse()
            .unwrap();
        let trusts = value["hooks"]["state"].as_table().unwrap();
        // The user's hook is index 0 and untrusted; git-ai's comes first among the groups of
        // its own events, ahead of the observer's.
        assert!(!trusts.contains_key("/<session-flags>/config.toml:subagent_stop:0:0"));
        assert!(trusts.contains_key("/<session-flags>/config.toml:subagent_stop:1:0"));
        let expected = |event: &str| {
            let identity = json!({"event_name":event,"hooks":[{
                "type":"command","command":command,"timeout":5,"async":false}]});
            format!(
                "sha256:{:x}",
                Sha256::digest(serde_json::to_vec(&identity).unwrap())
            )
        };
        for event in ["pre_tool_use", "post_tool_use"] {
            let grant = &trusts[&format!("/<session-flags>/config.toml:{event}:0:0")];
            assert_eq!(
                grant["trusted_hash"].as_str(),
                Some(expected(event).as_str()),
                "{event}"
            );
        }
        let stop = trusts
            .iter()
            .filter(|(key, _)| key.contains(":stop:"))
            .any(|(_, grant)| grant["trusted_hash"].as_str() == Some(expected("stop").as_str()));
        assert!(stop, "{trusts:?}");
        // Without the git-ai flags nothing of it is trusted.
        let plain = tempfile::tempdir().unwrap();
        let mut launch = Prepare::new(vec![], json!({}), "/server/v1/host");
        launch.home = Some(plain.path().to_owned());
        Codex.prepare(&mut launch).unwrap();
        let value: toml::Value = std::fs::read_to_string(plain.path().join("config.toml"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            value["hooks"]["state"].as_table().unwrap().len(),
            OBSERVER_EVENTS.len() + CHECKS.len()
        );
    }

    #[test]
    fn an_attribution_hook_without_its_marker_or_beside_the_agents_own_hooks_is_refused() {
        let command = "git-ai checkpoint codex # codesense-git-ai";
        assert!(attribution_hook_flags("git-ai checkpoint codex", &["Stop"], 5, &[]).is_err());
        assert!(attribution_hook_flags(command, &["Stop"], 5, &[]).is_ok());
        for own in [
            "hooks.Stop=[]",
            "hooks.Stop.extra=1",
            "hooks={Stop=[]}",
            "hooks.PreToolUse=[]",
        ] {
            let args = vec!["-c".to_owned(), own.to_owned()];
            let events: &'static [&'static str] = &["Stop", "PreToolUse"];
            assert!(
                attribution_hook_flags(command, events, 5, &args).is_err(),
                "{own}"
            );
            let compact = vec![format!("--config={own}")];
            assert!(
                attribution_hook_flags(command, events, 5, &compact).is_err(),
                "{own}"
            );
        }
        // Hooks of other events, and operands after `--`, do not conflict.
        let args = vec![
            "-c".to_owned(),
            "hooks.SessionStart=[]".to_owned(),
            "--".to_owned(),
            "-c".to_owned(),
            "hooks.Stop=[]".to_owned(),
        ];
        assert!(attribution_hook_flags(command, &["Stop"], 5, &args).is_ok());
    }

    #[test]
    fn cx2_hook_identity_survives_a_terminal_server_update() {
        let prepare = |helper| {
            let mut launch = Prepare::new(vec![], json!({}), helper);
            Codex.prepare(&mut launch).unwrap();
            launch.arguments
        };
        assert_eq!(prepare("/server/v1/host"), prepare("/server/v2/host"));
    }

    #[test]
    fn cx2_permission_attention_is_reported_by_qualified_root_hooks() {
        assert_eq!(Codex.descriptor().attention, Some(AttentionSignal::Hooks));
        let report = Codex.normalize(br#"{"hook_event_name":"PermissionRequest","session_id":"main","tool_use_id":"tool"}"#, true).unwrap();
        assert_eq!((report.event, report.scope), (Event::Waiting, Scope::Main));
    }

    #[test]
    fn the_observer_definition_stays_what_codex_trusts() {
        // Codex keeps its hook trust per handler definition: any change to it (a timeout,
        // a field) asks the user to trust the observer again.
        let flags = observer_flags("/app/helper").unwrap();
        let definition: toml::Table = flags[1].parse().unwrap();
        assert_eq!(definition["hooks"]["SessionStart"][0]["hooks"][0]["command"].as_str(),
            Some("[ -x \"$CODESENSE_ACTIVITY_HOST/activity-helper\" ] || exit 0; exec \"$CODESENSE_ACTIVITY_HOST/activity-helper\" --activity-hook codex"));
        assert_eq!(flags.len(), OBSERVER_EVENTS.len() * 2);
    }

    #[test]
    fn observer_flags_go_before_the_prompt_operand() {
        let args = vec!["-c".into(), "a=1".into(), "--".into(), "/bmad-help".into()];
        let composed = compose_observer(&args, "/app/helper").unwrap();
        let separator = composed.iter().position(|a| a == "--").unwrap();
        assert_eq!(composed[separator..], ["--", "/bmad-help"]);
        assert_eq!(composed[0], "--no-daemon");
        assert!(composed[..separator].iter().filter(|a| *a == "-c").count() > 1);
    }

    #[test]
    fn exec_keeps_root_option_before_subcommand() {
        let args = vec!["exec".into(), "--ephemeral".into(), "Reply OK".into()];
        let composed = compose_observer(&args, "/app/helper").unwrap();
        assert_eq!(
            &composed[..4],
            ["--no-daemon", "exec", "--ephemeral", "Reply OK"]
        );
    }

    #[test]
    fn session_flags_keep_existing_handlers_and_stable_observer_identity() {
        let arguments = vec![
            "-c".into(),
            "hooks.Stop=[{hooks=[{type=\"command\",command=\"user-stop\",timeout=9}]}]".into(),
            "--model".into(),
            "model".into(),
        ];
        let once = compose_observer(&arguments, "/app/helper").unwrap();
        let twice = compose_observer(&once, "/app/helper").unwrap();
        assert_eq!(once, twice);
        let (_, value) = once[2].split_once('=').unwrap();
        let table: toml::Table = format!("value={value}").parse().unwrap();
        let groups = table["value"].as_array().unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0]["hooks"][0]["command"].as_str(), Some("user-stop"));
        assert_eq!(groups[0]["hooks"][0]["timeout"].as_integer(), Some(9));
        assert_eq!(
            groups[1]["hooks"][0]["command"].as_str(),
            Some("[ -x \"$CODESENSE_ACTIVITY_HOST/activity-helper\" ] || exit 0; exec \"$CODESENSE_ACTIVITY_HOST/activity-helper\" --activity-hook codex")
        );
        assert_eq!(
            once.iter()
                .filter(|argument| argument.as_str() == "--no-daemon")
                .count(),
            1
        );
        assert_eq!(&once[3..5], &["--model", "model"]);
        assert!(compose_observer(&["-c".into(), "hooks={Stop=[]}".into()], "/app/helper").is_err());
        assert!(compose_observer(
            &[
                "-c".into(),
                "hooks.Stop=[]".into(),
                "--config=hooks.Stop=[]".into()
            ],
            "/app/helper"
        )
        .is_err());
    }

    #[test]
    fn an_unqualified_codex_reports_no_known_turn() {
        let stop = br#"{"hook_event_name":"Stop","session_id":"parent","turn_id":"turn"}"#;
        assert_eq!(Codex.normalize(stop, false).unwrap().scope, Scope::Unknown);
        // An interrupt is always the main turn's.
        let interrupt = br#"{"hook_event_name":"Interrupt","session_id":"parent"}"#;
        assert_eq!(
            Codex.normalize(interrupt, false).unwrap().scope,
            Scope::Main
        );
    }

    #[test]
    fn qualified_root_callbacks_do_not_admit_inherited_child_callbacks() {
        for event in [
            "SessionStart",
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "PermissionRequest",
            "Stop",
            "SessionEnd",
        ] {
            let mut raw = serde_json::json!({"hook_event_name": event, "session_id": "provider", "turn_id": "turn"});
            let report = Codex
                .normalize(&serde_json::to_vec(&raw).unwrap(), true)
                .unwrap();
            assert_eq!(report.scope, Scope::Main, "{event}");
            raw["agent_id"] = serde_json::json!("child");
            let report = Codex
                .normalize(&serde_json::to_vec(&raw).unwrap(), true)
                .unwrap();
            assert_eq!(report.scope, Scope::Child, "{event}");
        }
        for (event, expected) in [
            ("SubagentStart", Event::SubagentStarted),
            ("SubagentStop", Event::SubagentFinished),
        ] {
            let raw = serde_json::json!({"hook_event_name": event, "session_id": "provider", "turn_id": "child-turn", "agent_id": "child", "agent_type": "default"});
            let report = Codex
                .normalize(&serde_json::to_vec(&raw).unwrap(), false)
                .unwrap();
            assert_eq!(
                (report.event, report.scope, report.agent.as_deref()),
                (expected, Scope::Child, Some("child"))
            );
        }
    }

    /// How a fake app-server answers.
    #[derive(Clone)]
    struct Script {
        /// Its thread's status: `idle` or `active`.
        status: &'static str,
        /// Whether its enabled hooks are trusted.
        hook_trust: &'static str,
        hooks: Option<Value>,
        /// How it answers `turn/start`: with a turn, with an error, or by hanging up.
        turn: Turn,
        /// The client id of the user message its thread already holds, if any.
        holds: Option<String>,
        /// Its loaded threads, as it reads each (its status is the script's).
        threads: Vec<Value>,
        /// The cursor its list of loaded threads goes on at, if it goes on.
        more: Option<&'static str>,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Turn {
        Start,
        Refuse,
        HangUp,
    }

    impl Default for Script {
        fn default() -> Self {
            Self {
                status: "idle",
                hook_trust: "trusted",
                hooks: None,
                turn: Turn::Start,
                holds: None,
                threads: vec![json!({"id": "thread-1", "recencyAt": 1})],
                more: None,
            }
        }
    }

    /// Every request a fake app-server was sent: its method and params.
    type Seen = std::sync::Arc<std::sync::Mutex<Vec<(String, Value)>>>;

    /// A fake Codex app-server on a Unix socket in `dir`, serving every connection by
    /// `script`: its endpoint, and every request it was sent.
    fn app_server(dir: &Path, script: Script) -> (PathBuf, Seen) {
        let endpoint = dir.join("engine-1.sock");
        let listener = std::os::unix::net::UnixListener::bind(&endpoint).unwrap();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let (script, log) = (script.clone(), log.clone());
                std::thread::spawn(move || {
                    let Ok(mut socket) = tungstenite::accept(stream) else {
                        return;
                    };
                    while let Ok(Message::Text(text)) = socket.read() {
                        let request: Value = serde_json::from_str(text.as_str()).unwrap();
                        let method = request["method"].as_str().unwrap_or("").to_string();
                        log.lock()
                            .unwrap()
                            .push((method.clone(), request["params"].clone()));
                        let Some(id) = request.get("id") else {
                            continue;
                        };
                        let result = match method.as_str() {
                            "initialize" => json!({}),
                            "thread/loaded/list" => {
                                let ids: Vec<&Value> =
                                    script.threads.iter().map(|thread| &thread["id"]).collect();
                                json!({"data": ids, "nextCursor": script.more})
                            }
                            "thread/read" => {
                                let mut thread = script
                                    .threads
                                    .iter()
                                    .find(|thread| thread["id"] == request["params"]["threadId"])
                                    .cloned()
                                    .unwrap_or_default();
                                thread["status"] = json!({"type": script.status});
                                thread["cwd"] = json!("/workspace");
                                json!({"thread": thread})
                            }
                            "hooks/list" => {
                                let hooks: Vec<Value> = OBSERVER_EVENTS.iter().map(|event| {
                                    let mut name = event.to_string();
                                    name[..1].make_ascii_lowercase();
                                    json!({"eventName":name,"command":observer_command(),"handlerType":"command",
                                        "source":"sessionFlags","enabled":true,"trustStatus":script.hook_trust})
                                }).collect();
                                script.hooks.clone().unwrap_or_else(|| json!({"data":[{"cwd":"/workspace", "hooks":hooks,"errors":[],"warnings":[]}]}))
                            }
                            "turn/start" if script.turn == Turn::HangUp => return,
                            "turn/start" if script.turn == Turn::Refuse => {
                                let error =
                                    json!({"id": id, "error": {"code": -32600, "message": "no"}});
                                let _ = socket.send(Message::text(error.to_string()));
                                continue;
                            }
                            "turn/start" => json!({"turn": {"id": "turn-1"}}),
                            "thread/items/list" => {
                                let items: Vec<Value> = script
                                    .holds
                                    .iter()
                                    .map(|client| json!({"item": {"type": "userMessage", "clientId": client}}))
                                    .collect();
                                json!({"data": items})
                            }
                            _ => json!({}),
                        };
                        let reply = json!({"id": id, "result": result});
                        let _ = socket.send(Message::text(reply.to_string()));
                    }
                });
            }
        });
        (endpoint, seen)
    }

    fn sent(seen: &std::sync::Mutex<Vec<(String, Value)>>, method: &str) -> Vec<Value> {
        seen.lock()
            .unwrap()
            .iter()
            .filter(|(sent, _)| sent == method)
            .map(|(_, params)| params.clone())
            .collect()
    }

    #[test]
    fn its_engine_is_started_a_turn_only_while_idle_and_its_answer_is_read_honestly() {
        let start = |script: Script| {
            let dir = tempfile::tempdir().unwrap();
            let (endpoint, seen) = app_server(dir.path(), script);
            let mut channel = connect(&endpoint, None, None).unwrap();
            let outcome = channel.start_turn("[inbox t1]\nYou have 1 new message.", "t1");
            (outcome, seen, dir)
        };
        // Idle: a turn of its one thread, the notice its prompt, under the client id.
        let (outcome, seen, _dir) = start(Script::default());
        assert_eq!(outcome, SubmitOutcome::Accepted);
        let started = sent(&seen, "turn/start");
        assert_eq!(
            started,
            [json!({
                "threadId": "thread-1",
                "clientUserMessageId": "t1",
                "input": [{"type": "text", "text": "[inbox t1]\nYou have 1 new message."}],
            })]
        );
        // Working: never started a turn (it would join the running one).
        let (outcome, seen, _dir) = start(Script {
            status: "active",
            ..Script::default()
        });
        assert_eq!(outcome, SubmitOutcome::RefusedNoEffect);
        assert!(sent(&seen, "turn/start").is_empty());
        // Refused: no effect.
        let (outcome, _, _dir) = start(Script {
            turn: Turn::Refuse,
            ..Script::default()
        });
        assert_eq!(outcome, SubmitOutcome::RefusedNoEffect);
        // Its answer lost: read back by its client id, on a connection of its own.
        let (outcome, seen, _dir) = start(Script {
            turn: Turn::HangUp,
            holds: Some("t1".into()),
            ..Script::default()
        });
        assert_eq!(outcome, SubmitOutcome::Accepted);
        assert_eq!(sent(&seen, "thread/items/list")[0]["threadId"], "thread-1");
        let (outcome, _, _dir) = start(Script {
            turn: Turn::HangUp,
            holds: Some("another".into()),
            ..Script::default()
        });
        assert_eq!(outcome, SubmitOutcome::Uncertain);
    }

    /// The thread its TUI shows is the one its hooks named, or before any hook its only
    /// one, and in either case the latest; the engine's own threads and subagents' never
    /// are. Anything else is unclear, and no turn is started where no one sees it.
    #[test]
    fn its_turn_is_started_only_on_the_thread_its_tui_shows() {
        let thread = |id: &str, at: i64| json!({"id": id, "recencyAt": at});
        let shown = |threads: Vec<Value>, session: Option<&str>| {
            let dir = tempfile::tempdir().unwrap();
            let (endpoint, seen) = app_server(
                dir.path(),
                Script {
                    threads,
                    ..Script::default()
                },
            );
            let mut channel = connect(&endpoint, session, None).unwrap();
            let outcome = channel.start_turn("notice", "t1");
            let started = sent(&seen, "turn/start");
            // Unclear, it is refused with no effect.
            assert_eq!(outcome == SubmitOutcome::Accepted, !started.is_empty());
            started
                .first()
                .map(|started| started["threadId"].as_str().unwrap().to_owned())
        };
        // Before any hook: its only thread, past its reviewer's and a subagent's.
        let reviewer = json!({"id": "reviewer", "recencyAt": 9, "ephemeral": true});
        let subagent = json!({"id": "child", "recencyAt": 9, "parentThreadId": "main"});
        assert_eq!(
            shown(vec![thread("main", 1), reviewer, subagent], None).as_deref(),
            Some("main")
        );
        assert_eq!(shown(vec![thread("a", 1), thread("b", 2)], None), None);
        // Named by its hooks, and the latest.
        assert_eq!(
            shown(vec![thread("a", 2), thread("b", 1)], Some("a")).as_deref(),
            Some("a")
        );
        // After `/new`: the TUI shows a newer thread its hooks have not named.
        assert_eq!(shown(vec![thread("a", 1), thread("b", 2)], Some("a")), None);
        assert_eq!(shown(vec![thread("a", 2), thread("b", 2)], Some("a")), None);
        // Named, but no longer loaded.
        assert_eq!(shown(vec![thread("b", 1)], Some("a")), None);
        // A list of loaded threads that goes on leaves the latest unknown.
        let dir = tempfile::tempdir().unwrap();
        let (endpoint, seen) = app_server(
            dir.path(),
            Script {
                more: Some("next"),
                ..Script::default()
            },
        );
        let mut channel = connect(&endpoint, None, None).unwrap();
        assert_eq!(
            channel.start_turn("notice", "t1"),
            SubmitOutcome::RefusedNoEffect
        );
        assert!(sent(&seen, "turn/start").is_empty());
        // An engine that can't be reached is no connection.
        assert!(connect(&dir.path().join("none.sock"), None, None).is_err());
    }

    /// A request the engine sends while a reply is awaited is not its reply, whatever its
    /// id; the TUI answers it.
    #[test]
    fn a_request_from_its_engine_is_never_read_as_its_reply() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("engine.sock");
        let listener = std::os::unix::net::UnixListener::bind(&endpoint).unwrap();
        std::thread::spawn(move || {
            let mut socket =
                tungstenite::accept(listener.incoming().next().unwrap().unwrap()).unwrap();
            while let Ok(Message::Text(text)) = socket.read() {
                let request: Value = serde_json::from_str(text.as_str()).unwrap();
                let Some(id) = request.get("id") else {
                    continue;
                };
                // Its own request first, under the same id, then the reply.
                let ask = json!({"id": id, "method": "item/commandExecution/requestApproval", "params": {}});
                let _ = socket.send(Message::text(ask.to_string()));
                let reply = json!({"id": id, "result": {"answered": request["method"]}});
                let _ = socket.send(Message::text(reply.to_string()));
            }
        });
        let mut server = AppServer::open(&endpoint).unwrap();
        assert_eq!(
            server.request("thread/read", json!({})),
            Ok(json!({"answered": "thread/read"}))
        );
    }

    /// A host's inbox reports: what became of each notice.
    #[derive(Default)]
    struct Outlet(std::sync::Mutex<Vec<(String, crate::inbox_host::Fate)>>);

    impl crate::inbox_host::InboxOutlet for Outlet {
        fn fate(&self, _: &str, token: &str, fate: crate::inbox_host::Fate) {
            self.0.lock().unwrap().push((token.to_string(), fate));
        }
        fn waiter(&self, _: crate::domain::InboxWaiterState) {}
        fn audience(&self, _: &str) -> Option<String> {
            None
        }
        fn has_tools(&self, _: &str) -> Option<bool> {
            Some(true)
        }
    }

    #[test]
    fn its_host_starts_it_a_turn_through_its_engine_and_never_while_it_works() {
        let dir = tempfile::tempdir().unwrap();
        let (endpoint, seen) = app_server(dir.path(), Script::default());
        let outlet = std::sync::Arc::new(Outlet::default());
        let owner = crate::activity::ActivityOwner::new();
        owner.set_inbox_outlet(outlet.clone());
        let writer = owner
            .prepare_native("S1", AgentCli::Codex, "unused")
            .unwrap();
        let incarnation = owner.register("S1", Some(std::process::id())).unwrap();
        let mut at = std::time::Instant::now();
        let mut sweep = || {
            at += Duration::from_secs(6);
            owner.decay(at);
        };
        let hook = |event: &str| {
            let input = serde_json::json!({"hook_event_name": event, "session_id": "thread-1", "turn_id": "turn-1"});
            let report = Codex
                .normalize(&serde_json::to_vec(&input).unwrap(), true)
                .unwrap();
            owner
                .report_for_test(&writer, std::process::id(), report)
                .unwrap();
        };
        let reaches = |owner: &crate::activity::ActivityOwner| {
            let state = owner.inbox().state("S1");
            state.waiting || state.grace_ms.is_some()
        };
        let until_told = |count: usize| {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while outlet.0.lock().unwrap().len() < count {
                assert!(std::time::Instant::now() < deadline, "never told");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        // With no engine bound (one that never came up), its mail waits for its next check.
        sweep();
        assert!(!reaches(&owner));
        owner
            .inbox_mail("S1", "You have 1 new message.".into(), "t1".into())
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert!(outlet.0.lock().unwrap().is_empty());
        assert!(owner.inbox().holds("S1"));
        // Its qualified engine can establish initial readiness without a prompt hook.
        owner.bind_engine_for_test(&writer, endpoint).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !owner.snapshot("S1").unwrap().observed && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(owner.snapshot("S1").unwrap().observed);
        // Its held mail goes as a turn of its own.
        for event in ["SessionStart", "UserPromptSubmit", "Stop"] {
            hook(event);
        }
        sweep();
        until_told(1);
        assert_eq!(
            *outlet.0.lock().unwrap(),
            [("t1".to_string(), crate::inbox_host::Fate::Told)]
        );
        assert_eq!(sent(&seen, "turn/start")[0]["clientUserMessageId"], "t1");
        // Working, it is started nothing: its notice is held for its checks.
        hook("UserPromptSubmit");
        sweep();
        assert!(reaches(&owner) && !owner.inbox().state("S1").waiting);
        owner
            .inbox_mail("S1", "You have 2 new messages.".into(), "t2".into())
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(sent(&seen, "turn/start").len(), 1);
        assert!(owner.inbox().holds("S1"));
        // Idle again, the next sweep starts it a turn with the held one.
        hook("Stop");
        sweep();
        until_told(2);
        assert_eq!(sent(&seen, "turn/start")[1]["clientUserMessageId"], "t2");
        // Once it exits, new mail reaches it no more.
        owner.exit(
            "S1",
            &incarnation,
            crate::activity::Exit {
                code: Some(0),
                signal: None,
                requested: false,
            },
        );
        sweep();
        assert!(!reaches(&owner));
        let _ = owner.inbox_mail("S1", "You have 3 new messages.".into(), "t3".into());
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(sent(&seen, "turn/start").len(), 2);
    }

    #[test]
    fn configured_roots_are_its_home_configs_and_an_unreadable_one_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let roots = |text: Option<&str>| {
            if let Some(text) = text {
                std::fs::write(home.path().join("config.toml"), text).unwrap();
            }
            Codex.configured_roots(Some(home.path()), &[])
        };
        // No config, or one that names no roots, gives none.
        assert_eq!(roots(None), Ok(Vec::new()));
        assert_eq!(roots(Some("model = \"m\"\n")), Ok(Vec::new()));
        assert_eq!(
            roots(Some(
                "[sandbox_workspace_write]\nwritable_roots = [\"/a\", \"/b c\"]\n"
            )),
            Ok(strings(&["/a", "/b c"]))
        );
        let unparsed = roots(Some("[sandbox_workspace_write\n")).unwrap_err();
        assert!(unparsed.starts_with(&home.path().join("config.toml").display().to_string()));
        assert!(!unparsed.contains('\n'), "{unparsed}");
        assert!(roots(Some("[sandbox_workspace_write]\nwritable_roots = \"/a\"\n")).is_err());
        assert!(Codex.configured_roots(None, &[]).is_err());

        // A profile's roots replace its config's; one that names none keeps them.
        std::fs::write(
            home.path().join("config.toml"),
            "[sandbox_workspace_write]\nwritable_roots = [\"/base\"]\n",
        )
        .unwrap();
        std::fs::write(
            home.path().join("work.config.toml"),
            "[sandbox_workspace_write]\nwritable_roots = [\"/work\"]\n",
        )
        .unwrap();
        std::fs::write(home.path().join("plain.config.toml"), "model = \"m\"\n").unwrap();
        let with = |options: &[&str]| Codex.configured_roots(Some(home.path()), &strings(options));
        assert_eq!(with(&["-p", "work"]), Ok(strings(&["/work"])));
        assert_eq!(
            with(&["-m", "m", "--profile=work"]),
            Ok(strings(&["/work"]))
        );
        assert_eq!(with(&["-p", "plain"]), Ok(strings(&["/base"])));
        assert_eq!(with(&["--", "-p", "work"]), Ok(strings(&["/base"])));
    }

    #[test]
    fn rate_limits_read_every_bucket_not_token_activity() {
        let result = serde_json::json!({"rateLimitsByLimitId":{"codex":{"limitName":"Codex","primary":{"usedPercent":25,"resetsAt":200},"secondary":{"usedPercent":100,"resetsAt":300}}}});
        let windows = rate_limit_usage(&result).unwrap();
        assert_eq!(
            windows
                .iter()
                .map(|w| (w.id.as_str(), w.label.as_str(), w.used_percent))
                .collect::<Vec<_>>(),
            [
                ("codex:primary", "Codex primary", Some(25.0)),
                ("codex:secondary", "Codex secondary", Some(100.0))
            ]
        );
        assert!(rate_limit_usage(&serde_json::json!("closed")).is_err());
    }
}
