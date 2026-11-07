// SPDX-License-Identifier: MIT
use std::path::Path;

use codesense_pty_core::agent_cli::{
    ScopeInjection, MCP_BRIDGE_PATH_PLACEHOLDER, MCP_SCOPE_PATH_PLACEHOLDER,
};

use super::*;

#[test]
fn bridge_tool_allowlist_enforcement_is_a_cli_capability() {
    assert!(desktop(AgentCli::Codex).enforces_bridge_tool_allowlist);
    assert!(desktop(AgentCli::OpenCode).enforces_bridge_tool_allowlist);
    assert!(!desktop(AgentCli::Claude).enforces_bridge_tool_allowlist);
}

/// The placeholder names in a session-log glob, panicking on malformed braces or names
/// (the log resolver would silently find nothing).
fn glob_placeholders(glob: &str) -> Vec<String> {
    let name = regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").unwrap();
    let mut names = Vec::new();
    let mut rest = glob;
    while let Some(open) = rest.find(['{', '}']) {
        assert_eq!(&rest[open..=open], "{", "{glob}: unmatched `}}`");
        let after = &rest[open + 1..];
        let close = after
            .find('}')
            .unwrap_or_else(|| panic!("{glob}: unclosed `{{`"));
        let inner = &after[..close];
        assert!(name.is_match(inner), "{glob}: bad placeholder {inner:?}");
        names.push(inner.to_string());
        rest = &after[close + 1..];
    }
    names
}

#[test]
fn malformed_session_log_globs_are_caught() {
    assert_eq!(
        glob_placeholders("~/x/{enc_cwd}/{session_id}.jsonl"),
        ["enc_cwd", "session_id"]
    );
    for bad in [
        "~/x/{enc_cwd/{session_id}.jsonl",
        "~/x/{}/{session_id}.jsonl",
        "~/x/session_id}.jsonl",
        "~/x/{session_id.jsonl",
        "~/x/{9name}/{session_id}.jsonl",
    ] {
        assert!(
            std::panic::catch_unwind(|| glob_placeholders(bad)).is_err(),
            "{bad} accepted"
        );
    }
}

/// The terminal dot reads the built-in CLIs straight from their adapters.
#[tokio::test]
async fn list_agent_signals_serves_the_builtin_adapters() {
    let served =
        serde_json::to_value(crate::commands::agents::list_agent_signals().await.unwrap()).unwrap();
    assert_eq!(
        served,
        serde_json::json!([
            { "basename": "claude", "attention": "hooks", "quietSecs": 10 },
            { "basename": "codex", "attention": "hooks", "quietSecs": 10 },
            { "basename": "opencode", "attention": "hooks", "quietSecs": 10 },
            { "basename": "hermes", "attention": null, "quietSecs": 10 },
        ])
    );
}

/// The app's side of each CLI: its login provider, which maps back to it, and its
/// runner recipe.
#[test]
fn the_desktop_side_names_each_clis_login_and_runner() {
    for cli in AgentCli::every_cli() {
        assert_eq!(
            desktop(cli).account_requirement,
            if [AgentCli::OpenCode, AgentCli::Hermes].contains(&cli) {
                AccountRequirement::None
            } else {
                AccountRequirement::Optional
            }
        );
    }
    assert_eq!(provider_of(AgentCli::Claude), Some(AgentProvider::Claude));
    assert_eq!(provider_of(AgentCli::Codex), Some(AgentProvider::Codex));
    assert_eq!(provider_of(AgentCli::OpenCode), None);
    assert_eq!(
        desktop(AgentCli::Claude).runner.default_model.as_deref(),
        Some("claude-opus-4-6")
    );
    assert_eq!(desktop(AgentCli::Codex).runner.default_model, None);
    assert_eq!(
        desktop(AgentCli::OpenCode).account_requirement,
        AccountRequirement::None
    );
    assert_eq!(desktop(AgentCli::OpenCode).starter.name, "OpenCode");
    assert_eq!(desktop(AgentCli::OpenCode).runner.default_model, None);
    // OpenCode is never the runner: a headless run hands it the prompt on stdin, and an
    // argv template would put a prompt starting with `-` where a flag goes.
    assert!(desktop(AgentCli::OpenCode).runner.analyze_args.is_empty());
    assert!(desktop(AgentCli::OpenCode).runner.skill_args.is_empty());
}

#[test]
fn the_opencode_unattended_starter_keeps_auto() {
    let key = crate::agents::builtin::unattended_key(AgentCli::OpenCode);
    let starter = crate::agents::builtin::get(&key).unwrap();
    assert_eq!(starter.args, ["--auto"]);
}

#[test]
fn headless_settings_are_unchanged_for_claude_and_codex() {
    assert_eq!(
        (desktop(AgentCli::Claude).headless_settings)(),
        serde_json::json!({"permissions": {"defaultMode": "dontAsk"}})
    );
    assert_eq!(
        (desktop(AgentCli::Codex).headless_settings)(),
        serde_json::json!({})
    );
    let opencode = (desktop(AgentCli::OpenCode).headless_settings)();
    assert_eq!(opencode["*"], "deny");
    assert_eq!(opencode["external_directory"], "deny");
}

#[test]
fn wake_delivery_is_declared_by_each_cli_adapter() {
    assert!(desktop(AgentCli::Claude).supports_wake_delivery);
    assert!(desktop(AgentCli::Codex).supports_wake_delivery);
    assert!(desktop(AgentCli::OpenCode).supports_wake_delivery);
    assert!(desktop(AgentCli::Hermes).supports_wake_delivery);
}

#[test]
fn remote_launch_is_declared_by_each_cli_adapter() {
    assert!(desktop(AgentCli::Claude).supports_remote_launch);
    assert!(desktop(AgentCli::Codex).supports_remote_launch);
    assert!(!desktop(AgentCli::OpenCode).supports_remote_launch);
}

#[test]
fn a_blanket_opencode_allow_escalates_the_agent() {
    use crate::agents::model::{OpenCodePermissions, Permissions};
    let with = |action: &str| crate::agents::model::Agent {
        role: None,
        id: "a".into(),
        name: "A".into(),
        description: String::new(),
        created_by: "1".into(),
        runtime: AgentCli::OpenCode,
        args: Vec::new(),
        launch: crate::agents::model::LaunchMode::Foreground,
        mcp: Default::default(),
        startup: Vec::new(),
        context: None,
        permissions: Some(Permissions {
            opencode: Some(OpenCodePermissions {
                permission: [("*".to_string(), action.to_string())].into(),
                ..Default::default()
            }),
            ..Default::default()
        }),
        default_prompt: String::new(),
        avatar: None,
        allow_wake: false,
        resume_after_restart: false,
        budget_usd: None,
        revision: 1,
        archived_at: None,
        extra: Default::default(),
        env: Default::default(),
    };
    let escalates = desktop(AgentCli::OpenCode).escalates;
    assert!(escalates(&with("allow")));
    assert!(!escalates(&with("ask")));
    assert!(!escalates(&with("deny")));
}

/// An account's status line is wired to the receiver exactly for a CLI that reports its
/// usage there; the provider list names every provider once.
#[test]
fn only_a_status_line_cli_has_its_status_line_installed() {
    for cli in AgentCli::every_cli() {
        assert_eq!(
            desktop(cli).status_line.is_some(),
            matches!(
                codesense_pty_core::agent_cli::adapter(cli).output().usage,
                codesense_pty_core::agent_cli::Usage::StatusLine { .. }
            ),
            "{cli}"
        );
        let Some(provider) = provider_of(cli) else {
            continue;
        };
        assert_eq!(AgentProvider::parse(provider.as_str()), Some(provider));
        assert!(AgentProvider::ALL.contains(&provider));
    }
    assert!(desktop(AgentCli::Claude).status_line.is_some());
}

/// The checks that guarded hand-written descriptor files, kept as asserts over the
/// adapters: every template the launch path substitutes is well formed.
#[test]
fn every_adapter_recipe_is_well_formed() {
    let one_token = |label: &str, template: &[String], placeholder: &str| {
        let count: usize = template
            .iter()
            .map(|t| t.matches(placeholder).count())
            .sum();
        assert_eq!(count, 1, "{label} carries {placeholder} once: {template:?}");
        for token in template {
            assert!(
                !token.is_empty() && !token.chars().any(char::is_whitespace),
                "{label} token {token:?} is one argv element"
            );
        }
    };
    for cli in AgentCli::every_cli() {
        let id = cli.as_str();
        let descriptor = descriptor(cli);
        let runner = &desktop(cli).runner;
        assert!(!descriptor.binary.is_empty(), "{id}: names a binary");
        assert!(
            descriptor
                .binary
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "{id}: the binary is a bare basename"
        );
        if runner.default_model.is_some() {
            for args in [&runner.analyze_args, &runner.skill_args] {
                assert!(
                    args.iter()
                        .any(|a| a == crate::commands::agent_runner::MODEL_PLACEHOLDER),
                    "{id}: a default model needs a {{model}} site in {args:?}"
                );
            }
        }
        if let Some(interactive) = adapter(cli)
            .output()
            .transcript
            .and_then(|s| s.interactive.as_ref())
        {
            if let Some(template) = interactive.session_id_argv.as_ref() {
                assert!(expand_session_id_argv(template, "s").is_ok(), "{id}");
            } else {
                assert!(
                    adapter(cli).output().session_record.is_some(),
                    "{id}: native record captures its own thread id"
                );
            }
            assert_eq!(
                glob_placeholders(&interactive.session_log_glob)
                    .iter()
                    .filter(|name| *name == "session_id")
                    .count(),
                1,
                "{id}: the session log is found by the minted id"
            );
        }
        let scope = descriptor.mcp_scope.as_ref().unwrap();
        assert_eq!(scope.bridge_id, "codesense", "{id}");
        assert!(
            !scope.server_map_key.trim().is_empty() && !scope.config_filename.is_empty(),
            "{id}: the server map is found at a key in a named file"
        );
        assert!(
            Path::new(&scope.config_filename)
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
            "{id}: the config path stays inside the worktree"
        );
        match &scope.injection {
            ScopeInjection::ConfigFlag {
                flag_template,
                open_flag_template,
                settings_flag_template,
            } => {
                one_token("flag_template", flag_template, MCP_SCOPE_PATH_PLACEHOLDER);
                for template in [open_flag_template, settings_flag_template]
                    .into_iter()
                    .flatten()
                {
                    one_token("config_flag", template, MCP_SCOPE_PATH_PLACEHOLDER);
                }
            }
            ScopeInjection::EnvDir {
                env_var,
                passthrough,
                ..
            } => {
                // It is rendered as an inline `KEY=…` shell prefix on the interactive path.
                assert!(
                    env_var.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                        && env_var
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_'),
                    "{id}: {env_var:?} is an env var name"
                );
                for file in passthrough {
                    let mut parts = Path::new(&file.name).components();
                    assert!(
                        matches!(
                            (parts.next(), parts.next()),
                            (Some(std::path::Component::Normal(_)), None)
                        ),
                        "{id}: {:?} is a bare filename",
                        file.name
                    );
                }
            }
            ScopeInjection::EnvContent { env_var } => {
                assert!(!env_var.is_empty(), "{id}: inline config env var is named");
            }
        }
        if let Some(over) = &scope.bridge_override {
            one_token(
                "bridge_override",
                &over.flag_template,
                MCP_BRIDGE_PATH_PLACEHOLDER,
            );
        }
    }
}

#[test]
fn session_id_argv_validates_closed_template_and_expands_tokens() {
    let template = strings(&["--session-id", "{session_id}"]);
    assert_eq!(
        expand_session_id_argv(&template, "uuid-1").unwrap(),
        vec!["--session-id", "uuid-1"]
    );
    for invalid in [
        vec![],
        strings(&["", "{session_id}"]),
        strings(&["--session-id"]),
        strings(&["{session_id}", "{session_id}"]),
        strings(&["--session-id={session_id}"]),
        strings(&["{unknown}", "{session_id}"]),
    ] {
        assert!(
            expand_session_id_argv(&invalid, "uuid-1").is_err(),
            "invalid template accepted: {invalid:?}"
        );
    }
}

/// Running-detection keys on the adapter's basename, so a flag-bearing launch still
/// matches and another CLI does not.
#[test]
fn a_running_agent_is_recognized_by_its_adapters_basename() {
    use codesense_pty_core::process::agent_signature_matches;
    let needle = descriptor(AgentCli::Codex).binary.as_str();
    assert!(agent_signature_matches(
        "codex",
        Some("/opt/homebrew/bin/codex"),
        &["codex"],
        needle
    ));
    assert!(agent_signature_matches(
        "codex",
        None,
        &["codex", "--yolo"],
        needle
    ));
    assert!(!agent_signature_matches(
        "claude",
        Some("/opt/homebrew/bin/claude"),
        &["claude"],
        needle
    ));
}

#[test]
fn an_agent_launch_carries_its_adapters_shape() {
    let resolved = ResolvedLaunchProfile::for_agent(
        AgentCli::Codex,
        "acme/api/backend/dev".into(),
        strings(&["--model", "o3"]),
        false,
        strings(&["slack"]),
        LaunchMode::Headless,
    );
    assert_eq!(resolved.binary, "codex");
    assert_eq!(resolved.args, ["--model", "o3"]);
    assert!(!resolved.escalating);
    assert_eq!(resolved.mcp_allow, Some(strings(&["slack"])));
    assert_eq!(resolved.mode, LaunchMode::Headless);
    assert_eq!(
        resolved.mcp_scope.as_ref(),
        descriptor(AgentCli::Codex).mcp_scope.as_ref()
    );
    assert_eq!(resolved.env_suppress, ["OPENAI_API_KEY"]);
    assert_eq!(
        resolved.structured_interactive.as_ref(),
        adapter(AgentCli::Codex)
            .output()
            .transcript
            .and_then(|t| t.interactive.as_ref())
    );
    assert!(!resolved.capture_raw_terminal);

    let interactive = ResolvedLaunchProfile::for_agent(
        AgentCli::Claude,
        "k".into(),
        Vec::new(),
        false,
        Vec::new(),
        LaunchMode::Interactive,
    );
    assert_eq!(
        interactive.structured_interactive.as_ref(),
        adapter(AgentCli::Claude)
            .output()
            .transcript
            .and_then(|s| s.interactive.as_ref())
    );
}

/// A blanket bypass flag in the agent's args marks the launch escalating even when the
/// agent's own verdict says otherwise.
#[test]
fn an_escalating_flag_in_the_args_marks_the_launch_escalating() {
    for (args, escalating) in [
        (strings(&["--dangerously-skip-permissions"]), true),
        (
            strings(&["--dangerously-bypass-approvals-and-sandbox"]),
            true,
        ),
        (strings(&["--model", "claude-opus-4-6"]), false),
    ] {
        let resolved = ResolvedLaunchProfile::for_agent(
            AgentCli::Claude,
            "k".into(),
            args.clone(),
            false,
            Vec::new(),
            LaunchMode::Interactive,
        );
        assert_eq!(resolved.escalating, escalating, "{args:?}");
    }
}

#[test]
fn the_webview_sees_the_env_strip_but_not_the_internal_shape() {
    let resolved = ResolvedLaunchProfile::for_agent(
        AgentCli::Codex,
        "k".into(),
        Vec::new(),
        false,
        Vec::new(),
        LaunchMode::Interactive,
    );
    let json = serde_json::to_value(&resolved).unwrap();
    assert_eq!(json["env_suppress"], serde_json::json!(["OPENAI_API_KEY"]));
    for internal in ["mcp_scope", "structured_interactive"] {
        assert!(json.get(internal).is_none(), "{internal} stays server-side");
    }
}

/// Each runtime's launch settings are its own: Claude's whole settings body, Codex's keys.
#[test]
fn each_runtime_generates_its_own_launch_settings() {
    use crate::agents::model::{Agent, LaunchMode};
    let agent = |runtime| Agent {
        role: None,
        id: "a".into(),
        name: "a".into(),
        description: String::new(),
        created_by: String::new(),
        runtime,
        args: Vec::new(),
        launch: LaunchMode::Headless,
        mcp: Default::default(),
        startup: Vec::new(),
        context: None,
        permissions: None,
        default_prompt: String::new(),
        avatar: None,
        allow_wake: false,
        resume_after_restart: false,
        budget_usd: None,
        revision: 1,
        archived_at: None,
        extra: Default::default(),
        env: Default::default(),
    };
    let claude =
        (desktop(AgentCli::Claude).launch_settings)(&agent(AgentCli::Claude), LaunchMode::Headless);
    assert!(claude.content["permissions"]["allow"].is_array());
    let codex =
        (desktop(AgentCli::Codex).launch_settings)(&agent(AgentCli::Codex), LaunchMode::Headless);
    assert_eq!(codex.content["approval_policy"], "never");
}

#[test]
fn the_opencode_plugin_is_installed_with_the_waiter_protocol_its_adapter_declares() {
    use codesense_pty_core::agent_cli::{
        adapter, InboxDelivery, WAITER_CONFIRMATION_LINE, WAITER_RENEWAL_LINE,
    };
    let installed = super::opencode::plugin_source();
    assert!(
        !installed.contains("/* @waiter */"),
        "the slot is filled, not left in the install"
    );
    let InboxDelivery::Waiter {
        events,
        replace_events,
        ..
    } = adapter(AgentCli::OpenCode).inbox()
    else {
        panic!("OpenCode declares a waiter");
    };
    let expected = serde_json::json!({
        "events": events,
        "replace": replace_events,
        "renewal": WAITER_RENEWAL_LINE,
        "confirmation": WAITER_CONFIRMATION_LINE,
    });
    assert!(
        installed.contains(&format!("const WAITER = {expected};")),
        "{installed}"
    );
    // The line the plugin recognises is the one the shared helper prints.
    assert_eq!(
        WAITER_RENEWAL_LINE,
        "Re-arming inbox waiter; no action needed."
    );
}

#[test]
fn only_a_qualified_opencode_adds_its_plugin_and_other_adapters_ignore_the_flag() {
    let _home = crate::routines::test_support::TempHome::new();
    for cli in [AgentCli::Claude, AgentCli::Codex] {
        for qualified in [true, false] {
            let entries = (desktop(cli).inline_config)(qualified).unwrap();
            assert!(entries.is_empty(), "{cli:?} qualified={qualified}");
        }
    }
    let opencode = desktop(AgentCli::OpenCode);
    assert!((opencode.inline_config)(false).unwrap().is_empty());
    let entries = (opencode.inline_config)(true).unwrap();
    let url = entries["plugins"][0].as_str().unwrap();
    let path = url::Url::parse(url).unwrap().to_file_path().unwrap();
    let installed = std::fs::read_to_string(path.join("server.js")).unwrap();
    // Through the real install path the plugin has both slots filled, with the range the
    // desktop checks versions against and the protocol its adapter declares, and none left.
    use codesense_pty_core::agent_cli::{
        adapter, InboxDelivery, WAITER_CONFIRMATION_LINE, WAITER_RENEWAL_LINE,
    };
    let InboxDelivery::Waiter {
        events,
        replace_events,
        ..
    } = adapter(AgentCli::OpenCode).inbox()
    else {
        panic!("OpenCode declares a waiter");
    };
    let protocol = serde_json::json!({
        "events": events,
        "replace": replace_events,
        "renewal": WAITER_RENEWAL_LINE,
        "confirmation": WAITER_CONFIRMATION_LINE,
    });
    assert!(
        installed.contains(r#"/* @supported */ {"beforeMajor":3,"min":[2,1,4]}"#),
        "range not written"
    );
    assert!(
        installed.contains(&format!("const WAITER = {protocol};")),
        "waiter protocol not written"
    );
    assert!(!installed.contains("/* @supported */ null"));
    assert!(!installed.contains("/* @waiter */ null"));
}

#[test]
fn hermes_is_listed_and_available_to_users() {
    let runtime = AgentCli::Hermes;
    assert!(AgentCli::ALL.contains(&runtime));
    assert_eq!(runtime.as_str().parse::<AgentCli>(), Ok(runtime));
    assert_eq!(
        serde_json::from_str::<AgentCli>(r#""hermes""#).unwrap(),
        runtime
    );
    assert!(crate::agents::builtin::all()
        .iter()
        .any(|(_, agent)| agent.runtime == runtime));
    let mut agent = crate::agents::resolve::tests::draft("hermes-agent", runtime);
    agent.revision = 1;
    agent.validate().unwrap();
    let decoded: crate::agents::model::Agent =
        serde_json::from_value(serde_json::to_value(&agent).unwrap()).unwrap();
    assert_eq!(decoded.runtime, runtime);
    let mut permissions = Permissions::default();
    assert!((desktop(runtime).permissions.validate)(&permissions).is_ok());
    (desktop(runtime).permissions.set)(&mut permissions, serde_json::json!({"allow": true}))
        .unwrap();
    assert!((desktop(runtime).permissions.validate)(&permissions).is_err());
    (desktop(runtime).permissions.set)(&mut permissions, serde_json::Value::Null).unwrap();
    assert!((desktop(runtime).permissions.validate)(&permissions).is_ok());
    assert!(!desktop(runtime).supports_remote_launch);
    assert!(desktop(runtime).preview);
    assert!(desktop(runtime).supports_wake_delivery);
    assert!(desktop(runtime).enforces_bridge_tool_allowlist);
    assert_eq!(
        desktop(runtime).attribution,
        Attribution::Unsupported("hermes")
    );
    let fixture = crate::attribution::testing::Launch::new(AgentCli::Claude);
    let plan = crate::attribution::launch::prepare(
        &crate::attribution::launch::Request {
            codesense_home: fixture.home(),
            principal: &crate::agents::resolve::tests::alice(),
            cwd: &fixture.cwd,
            runtime,
        },
        None,
    );
    assert_eq!(
        plan.status,
        crate::attribution::AttributionStatus::Unsupported {
            reason: "hermes".into()
        }
    );
    assert!(plan.hook_command.is_none());
    assert_eq!(
        plan.env,
        crate::attribution::unsupported::launch_env("hermes")
    );
    assert!(codesense_pty_core::agent_cli::known_label("hermes").is_none());
}
