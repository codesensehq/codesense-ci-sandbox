// SPDX-License-Identifier: MIT
use std::path::Path;

use codesense_host_proto::reverse_mcp::AgentStartup;

use super::*;
use crate::activity::{Event, Scope};

#[test]
fn environment_delivery_shapes_are_explicit() {
    let settings = LaunchSettings::Env("OPENCODE_CONFIG_CONTENT".into());
    assert!(matches!(settings, LaunchSettings::Env(var) if var == "OPENCODE_CONFIG_CONTENT"));
    let injection = ScopeInjection::EnvContent {
        env_var: "OPENCODE_CONFIG_CONTENT".into(),
    };
    assert!(matches!(
        injection,
        ScopeInjection::EnvContent { env_var } if env_var == "OPENCODE_CONFIG_CONTENT"
    ));
    assert_eq!(
        McpBridgeEntry::LocalCommandArray {
            environment_key: "environment",
        }
        .render("/bin/codesense-bridge", Some("tab-7")),
        serde_json::json!({
            "type": "local",
            "command": ["/bin/codesense-bridge"],
            "enabled": true,
            "environment": {"CODESENSE_TAB_SESSION_ID": "tab-7"}
        })
    );
}

#[test]
fn every_cli_has_its_own_adapter() {
    for cli in AgentCli::every_cli() {
        let descriptor = descriptor(cli);
        assert_eq!(descriptor.binary, cli.as_str());
    }
    assert_eq!(
        (
            descriptor(AgentCli::Claude).name.as_str(),
            descriptor(AgentCli::Codex).name.as_str()
        ),
        ("Claude Code", "OpenAI Codex")
    );
}

/// A managed OpenCode launch drops ambient config selectors and the credential sources
/// that would bypass the user's stored login.
#[test]
fn opencode_suppresses_config_selectors_and_extra_credential_sources() {
    let suppressed = &descriptor(AgentCli::OpenCode).env_suppress;
    for name in [
        "OPENCODE_CONFIG_DIR",
        "OPENCODE_CONFIG",
        "OPENCODE_AUTH_CONTENT",
        "DEEPSEEK_API_KEY",
    ] {
        assert!(
            suppressed.iter().any(|n| n == name),
            "{name} not suppressed"
        );
    }
}

#[test]
fn opencode_has_an_independent_identity_and_honest_unsupported_ports() {
    let cli = AgentCli::OpenCode;
    let descriptor = descriptor(cli);
    let adapter = adapter(cli);
    assert_eq!(descriptor.binary, "opencode");
    assert_eq!(descriptor.release.min, (2, 1, 4));
    assert_eq!(descriptor.release.before_major, Some(3));
    assert!(descriptor.release.qualified("opencode v2.1.4"));
    assert!(!descriptor.release.qualified("2.0.0"));
    assert!(!descriptor.prompt_operand);
    assert_eq!(descriptor.prompt_flag.as_deref(), Some("--prompt"));
    assert_eq!(descriptor.bypass_flag, "--auto");
    // Its plugin runs the waiter at the shared hook events Claude's hooks do, and confirms
    // each notice it delivers; Claude's hook needs no confirmation.
    assert_eq!(
        adapter.inbox(),
        InboxDelivery::Waiter {
            events: &["SessionStart", "Stop", "StopFailure", "PostToolUse"],
            replace_events: &["SessionStart", "Stop", "StopFailure"],
            hand_over: HandOver::RewakeConfirmed,
        }
    );
    assert!(matches!(
        super::adapter(AgentCli::Claude).inbox(),
        InboxDelivery::Waiter {
            hand_over: HandOver::Rewake,
            ..
        }
    ));
    assert!(adapter.normalize(b"{}", true).is_none());
    assert!(matches!(adapter.output().usage, Usage::None));
    let transcript = adapter
        .output()
        .transcript
        .expect("stdout is its transcript");
    let headless = transcript.headless.as_ref().expect("headless structured");
    assert!(headless.request_argv.is_empty());
    assert_eq!(headless.format, TranscriptFormat::OpenCodeRunJson);
    assert!(transcript.interactive.is_none());
    assert!(!TranscriptFormat::OpenCodeRunJson.requires_captured_session());
    assert_eq!(
        serde_json::to_string(&TranscriptFormat::OpenCodeRunJson).unwrap(),
        "\"opencode-run-json-v1\""
    );
    assert_eq!(descriptor.attention, Some(AttentionSignal::Hooks));
    assert!(descriptor.project_hooks.is_none());
}

#[test]
fn opencode_hands_its_plugin_the_usage_endpoint() {
    let export = descriptor(AgentCli::OpenCode).telemetry.as_ref().unwrap();
    let (env, argv) = export.resolved("http://127.0.0.1:1/api/v1/telemetry/tok");
    assert!(argv.is_empty());
    assert!(env.contains(&(
        "CODESENSE_USAGE_ENDPOINT".to_string(),
        "http://127.0.0.1:1/api/v1/telemetry/tok".to_string()
    )));
    assert!(export.is_usage_record(Some("opencode.usage"), None));
    assert!(!export.is_usage_record(Some("api_request"), None));
}

#[test]
fn claude_drives_the_claude_binary() {
    let claude = descriptor(AgentCli::Claude);
    assert_eq!(claude.min_version, (2, 0));
    assert_eq!(claude.env_suppress, ["ANTHROPIC_API_KEY"]);
    assert_eq!(claude.root_doc, "CLAUDE.md");
    assert_eq!(claude.headless_entry, ["-p"]);
    assert_eq!(claude.add_dir_flag.as_deref(), Some("--add-dir"));
    assert_eq!(claude.attention, Some(AttentionSignal::Hooks));

    let scope = claude.mcp_scope.as_ref().unwrap();
    assert_eq!(
        (
            scope.config_filename.as_str(),
            scope.server_map_key.as_str(),
            scope.format
        ),
        (".mcp.json", "mcpServers", ConfigFragmentFormat::Json)
    );
    assert_eq!(scope.bridge_override, None);
    // The per-launch settings file that carries the status hooks rides this template.
    assert_eq!(
        scope.injection,
        ScopeInjection::ConfigFlag {
            flag_template: strings(&["--mcp-config", "{path}", "--strict-mcp-config"]),
            open_flag_template: Some(strings(&["--mcp-config", "{path}"])),
            settings_flag_template: Some(strings(&["--settings", "{path}"])),
        }
    );

    let transcript = adapter(AgentCli::Claude).output().transcript.unwrap();
    let headless = transcript.headless.as_ref().unwrap();
    assert_eq!(
        headless.request_argv,
        ["--output-format", "stream-json", "--verbose"]
    );
    assert_eq!(
        serde_json::to_value(headless.format).unwrap(),
        "anthropic-stream-json-v1"
    );
    let interactive = transcript.interactive.as_ref().unwrap();
    assert_eq!(
        interactive.session_log_glob,
        "~/.claude/projects/{enc_cwd}/{session_id}.jsonl"
    );
    assert_eq!(
        serde_json::to_value(interactive.format).unwrap(),
        "anthropic-session-jsonl-v1"
    );
}

#[test]
fn codex_drives_the_codex_binary_through_a_scoped_home() {
    let codex = descriptor(AgentCli::Codex);
    assert_eq!(codex.min_version, (0, 140));
    assert_eq!(codex.env_suppress, ["OPENAI_API_KEY"]);
    assert_eq!(codex.root_doc, "AGENTS.md");
    assert_eq!(codex.headless_entry, ["exec"]);
    assert_eq!(codex.attention, Some(AttentionSignal::Hooks));
    assert!(adapter(AgentCli::Codex).output().transcript.is_some());

    let scope = codex.mcp_scope.as_ref().unwrap();
    assert_eq!(scope.format, ConfigFragmentFormat::Toml);
    assert_eq!(scope.config_filename, ".codex/config.toml");
    assert_eq!(scope.server_map_key, "mcp_servers");
    assert_eq!(
        scope.injection,
        ScopeInjection::EnvDir {
            carry_home: true,
            env_var: "CODEX_HOME".into(),
            passthrough: vec![Passthrough {
                name: "auth.json".into(),
                mode: PassthroughMode::Copy,
            }],
            default_home: "~/.codex".into(),
        }
    );
    // The bridge override registers the bridge under the scope's own map key and id.
    let flags = scope
        .bridge_override
        .as_ref()
        .unwrap()
        .render(Path::new("/b"))
        .unwrap();
    let key = format!("{}.{}.command=/b", scope.server_map_key, scope.bridge_id);
    assert_eq!(flags, ["-c".to_string(), key]);
}

#[test]
fn a_bridge_path_that_would_split_the_argv_renders_nothing() {
    let codex = descriptor(AgentCli::Codex).mcp_scope.as_ref().unwrap();
    let bridge = codex.bridge_override.as_ref().unwrap();
    assert_eq!(bridge.render(Path::new("/a b/bridge")), None);
    assert_eq!(bridge.render(Path::new("")), None);
}

#[test]
fn the_host_probes_each_cli_for_what_its_launch_relies_on() {
    for cli in AgentCli::every_cli() {
        assert_eq!(
            adapter(cli).probe().cache_version,
            cli == AgentCli::Hermes,
            "{cli}"
        );
    }
    assert!(!orion::Orion.probe().cache_version);
    let claude = adapter(AgentCli::Claude).probe();
    assert_eq!(claude.launch_flags, ["--mcp-config", "--strict-mcp-config"]);
    assert_eq!(claude.agent_flags, ["--settings"]);
    assert!(matches!(
        claude.activity,
        ActivityProbe::HelpFlag("--settings")
    ));
    let hermes = adapter(AgentCli::Hermes).probe();
    assert!(hermes.launch_flags.is_empty());
    assert_eq!(hermes.agent_flags, ["--tui", "--query", "--yolo"]);
    assert!(matches!(hermes.activity, ActivityProbe::Version));
    assert_eq!(hermes.help_subcommand, ["chat"]);
    assert_eq!(hermes.version_env.len(), 3);
    let codex = adapter(AgentCli::Codex).probe();
    assert_eq!(codex.launch_flags, ["--no-daemon", "--config"]);
    assert!(codex.agent_flags.is_empty());
    assert!(matches!(codex.activity, ActivityProbe::Version));
}

#[test]
fn claude_hears_mail_through_its_waiter_and_codex_through_its_host_and_checks() {
    let InboxDelivery::Waiter {
        events,
        replace_events,
        ..
    } = adapter(AgentCli::Claude).inbox()
    else {
        panic!("Claude is woken by its waiter");
    };
    assert_eq!(
        events,
        ["SessionStart", "Stop", "StopFailure", "PostToolUse"]
    );
    // A boot or a turn end takes the place; a tool call's re-arm and a re-ask defer.
    assert_eq!(replace_events, ["SessionStart", "Stop", "StopFailure"]);
    assert!(replace_events.iter().all(|event| events.contains(event)));
    // Codex is started a turn with its notice while idle, and its checks hand it over as
    // context while it works: at a prompt or a tool call. Not at a turn end, whose
    // continuation its activity would not show.
    let codex = adapter(AgentCli::Codex).inbox();
    assert!(matches!(codex, InboxDelivery::HostSubmit { .. }));
    assert_eq!(codex.events(), ["UserPromptSubmit", "PostToolUse"]);
    for event in ["UserPromptSubmit", "PostToolUse"] {
        assert_eq!(
            codex.hand_over(event),
            Some(HandOver::AdditionalContext),
            "{event}"
        );
    }
    assert_eq!(codex.hand_over("Stop"), None);
    assert_eq!(codex.hand_over("SessionStart"), None);
    // A waiter hands every notice over as a rewake.
    let claude = adapter(AgentCli::Claude).inbox();
    assert_eq!(claude.hand_over("PostToolUse"), Some(HandOver::Rewake));
    assert_eq!(claude.hand_over("UserPromptSubmit"), None);
    assert_eq!(InboxDelivery::None.hand_over("Stop"), None);
}

#[test]
fn the_inbox_hooks_run_the_tabs_host_helper_for_the_agent_that_ran_them() {
    let mut claude = Prepare::new(
        vec![],
        serde_json::json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "mine"}]}]}}),
        "/unused",
    );
    adapter(AgentCli::Claude).inbox_hooks(&mut claude).unwrap();
    let again = claude.settings.clone();
    adapter(AgentCli::Claude).inbox_hooks(&mut claude).unwrap();
    assert_eq!(claude.settings, again, "arming twice adds nothing");
    for event in adapter(AgentCli::Claude).inbox().events() {
        let groups = claude.settings["hooks"][event].as_array().unwrap();
        let waiter = &groups.last().unwrap()["hooks"][0];
        assert_eq!(
            waiter["command"],
            format!(
                "[ -x \"$CODESENSE_ACTIVITY_HOST/activity-helper\" ] || exit 0; \
                 exec \"$CODESENSE_ACTIVITY_HOST/activity-helper\" --inbox-wait claude {event} \"$PPID\""
            )
        );
        assert_eq!(waiter["asyncRewake"], true);
        assert_eq!(waiter["timeout"], INBOX_WAIT_TIMEOUT_SECS);
    }
    // The user's own hook stays, ahead of the waiter.
    assert_eq!(
        claude.settings["hooks"]["Stop"][0]["hooks"][0]["command"],
        "mine"
    );
    // Codex takes its checks as `-c` overrides, ahead of its prompt, and its settings
    // stay untouched.
    let mut codex = Prepare::new(
        vec!["--".into(), "hi".into()],
        serde_json::json!({}),
        "/unused",
    );
    adapter(AgentCli::Codex).inbox_hooks(&mut codex).unwrap();
    let again = codex.arguments.clone();
    adapter(AgentCli::Codex).inbox_hooks(&mut codex).unwrap();
    assert_eq!(codex.arguments, again, "arming twice adds nothing");
    assert_eq!(codex.settings, serde_json::json!({}));
    assert_eq!(codex.arguments[codex.arguments.len() - 2..], ["--", "hi"]);
    for event in adapter(AgentCli::Codex).inbox().events() {
        let value = codex
            .arguments
            .iter()
            .find_map(|argument| argument.strip_prefix(&format!("hooks.{event}=")))
            .unwrap_or_else(|| panic!("{event} runs a check"));
        let table: toml::Table = format!("value={value}").parse().unwrap();
        let check = &table["value"][0]["hooks"][0];
        assert_eq!(
            check["command"].as_str(),
            Some(inbox_hook_command(AgentCli::Codex, event).as_str())
        );
        // It looks once, so it needs no async mode, and its timeout outlasts a look.
        assert!(check.get("async").is_none());
        assert_eq!(check["timeout"].as_integer(), Some(10));
    }
}

#[test]
fn codex_starts_a_turn_through_its_engine_and_claude_takes_no_notice_from_its_host() {
    let notice = Notice {
        text: "[inbox t-1]\nYou have 1 new message.",
        token: "t-1",
    };
    let endpoint = tempfile::tempdir().unwrap();
    let server = orion::serve(endpoint.path());
    let connect = orion::Orion.control().channel.unwrap();
    let mut channel = connect(endpoint.path(), None, None).unwrap();
    assert_eq!(
        adapter(AgentCli::Claude).submit_notice(&mut *channel, &notice),
        SubmitOutcome::RefusedNoEffect
    );
    assert!(server.handed().is_empty(), "Claude is handed nothing");
    // Codex's engine starts the turn under the notice's token, and its outcome is the
    // host's to read, whatever it is.
    for outcome in [
        SubmitOutcome::Accepted,
        SubmitOutcome::RefusedNoEffect,
        SubmitOutcome::Uncertain,
    ] {
        server.answer(&[outcome]);
        assert_eq!(
            adapter(AgentCli::Codex).submit_notice(&mut *channel, &notice),
            outcome
        );
    }
    assert_eq!(server.handed(), ["t-1", "t-1", "t-1"]);
}

#[test]
fn codex_runs_on_its_app_server_with_its_engine_config_and_its_tui_attached() {
    let options = strings(&[
        "--no-daemon",
        "-c",
        "hooks.Stop=[]",
        "--model",
        "m",
        "--config=mcp_servers.x={}",
        "-p",
        "work",
        "-cmodel_verbosity=low",
        "--enable",
        "a",
        "--disable=b",
        "--dangerously-bypass-approvals-and-sandbox",
    ]);
    let channel = adapter(AgentCli::Codex).channel(&options).unwrap();
    let endpoint = format!("unix://{CHANNEL_ENDPOINT}");
    // Its engine takes every config override (however it is spelled), its profile and
    // its features, and serves on its socket.
    assert_eq!(
        channel.engine,
        strings(&[
            "-c",
            "hooks.Stop=[]",
            "--config=mcp_servers.x={}",
            "-p",
            "work",
            "-cmodel_verbosity=low",
            "--enable",
            "a",
            "--disable=b",
            "app-server",
            "--listen",
            &endpoint,
        ])
    );
    // Its TUI keeps its session's options, on no daemon, attached to that socket; its MCP
    // servers run on its engine alone.
    assert_eq!(
        channel.attached,
        strings(&[
            "-c",
            "hooks.Stop=[]",
            "--model",
            "m",
            "-p",
            "work",
            "-cmodel_verbosity=low",
            "--enable",
            "a",
            "--disable=b",
            "--dangerously-bypass-approvals-and-sandbox",
            "--remote",
            &endpoint,
        ])
    );
    // A run with no TUI (one-shot, or another subcommand, which refuses `--remote`, or a
    // single word that may be one), or one on a remote of its own, takes no engine; one
    // that resumes a session, or starts with a prompt, runs its TUI on one, whatever its
    // options' values spell. Claude runs on none.
    for run in [
        &["exec", "go"][..],
        &["-m", "m", "login", "status"],
        &["mcp", "list"],
        &["--version"],
        &["--remote", "ws://elsewhere"],
        &["newer-subcommand"],
    ] {
        assert!(
            adapter(AgentCli::Codex).channel(&strings(run)).is_none(),
            "{run:?}"
        );
    }
    for run in [&["resume", "--last"][..], &["-m", "login", "fix the build"]] {
        assert!(
            adapter(AgentCli::Codex).channel(&strings(run)).is_some(),
            "{run:?}"
        );
    }
    assert!(adapter(AgentCli::Claude).channel(&options).is_none());
}

/// The roots in the one writable-roots override among `options`, which must be exactly one.
fn writable_roots(options: &[String]) -> Vec<String> {
    let overrides: Vec<&String> = options
        .iter()
        .filter(|option| option.starts_with("sandbox_workspace_write.writable_roots="))
        .collect();
    assert_eq!(overrides.len(), 1, "{options:?}");
    let table: toml::Table = overrides[0].parse().unwrap();
    table["sandbox_workspace_write"]["writable_roots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|root| root.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn codex_keeps_what_its_attached_tui_refuses_on_its_engine() {
    let root = "/team's content/root";
    let options = strings(&[
        "--add-dir",
        root,
        "-c",
        "mcp_servers={\"codesense\"={enabled=false}}",
        "--add-dir=/second",
        "--config",
        "sandbox_workspace_write.writable_roots=[\"/mine\",\"/second\"]",
        "-cmcp_servers.codesense.command=/bridge",
        "-c",
        "sandbox_workspace_write={writable_roots=[\"/table\"],network_access=true}",
        "-c",
        "hooks.Stop=[]",
        "-m",
        "m",
    ]);
    let channel = adapter(AgentCli::Codex).channel(&options).unwrap();
    // Its engine takes every root once, in one override after the rest of their table,
    // and its MCP servers.
    assert_eq!(
        writable_roots(&channel.engine),
        [root, "/second", "/mine", "/table"]
    );
    for kept in [
        "mcp_servers={\"codesense\"={enabled=false}}",
        "-cmcp_servers.codesense.command=/bridge",
        "sandbox_workspace_write={writable_roots=[\"/table\"],network_access=true}",
        "hooks.Stop=[]",
    ] {
        assert!(channel.engine.iter().any(|o| o == kept), "{kept}");
    }
    assert!(!channel.engine.iter().any(|o| o.starts_with("--add-dir")));
    // Its TUI takes none of them: neither an extra root nor an MCP server.
    let endpoint = format!("unix://{CHANNEL_ENDPOINT}");
    assert_eq!(
        channel.attached,
        strings(&["-c", "hooks.Stop=[]", "-m", "m", "--remote", &endpoint])
    );
    // A run with no TUI keeps its extra roots as given; so does Claude, which has no engine.
    let exec = strings(&["--add-dir", root, "exec", "go"]);
    assert!(adapter(AgentCli::Codex).channel(&exec).is_none());
    assert!(adapter(AgentCli::Claude)
        .channel(&strings(&["--add-dir", root]))
        .is_none());
    assert_eq!(
        descriptor(AgentCli::Claude).add_dir_flag.as_deref(),
        Some("--add-dir")
    );
}

#[test]
fn each_adapter_normalizes_its_own_hooks() {
    let start = br#"{"hook_event_name":"SessionStart","session_id":"s"}"#;
    for cli in AgentCli::every_cli() {
        if cli == AgentCli::Hermes {
            assert!(adapter(cli).normalize(start, true).is_none());
            continue;
        }
        let report = adapter(cli).normalize(start, true).unwrap();
        assert_eq!((report.provider, report.event), (cli, Event::Started));
    }
    let notification = br#"{"hook_event_name":"Notification","notification_type":"idle_prompt"}"#;
    assert_eq!(
        adapter(AgentCli::Claude)
            .normalize(notification, false)
            .unwrap()
            .event,
        Event::Prompting
    );
    assert!(adapter(AgentCli::Codex)
        .normalize(notification, true)
        .is_none());
    let stop = br#"{"hook_event_name":"Stop","session_id":"s"}"#;
    assert_eq!(
        adapter(AgentCli::Codex)
            .normalize(stop, false)
            .unwrap()
            .scope,
        Scope::Unknown
    );
}

#[test]
fn cli_session_rebind_policy_matches_their_native_lifecycle() {
    let claude = adapter(AgentCli::Claude).session_policy();
    assert!(claude.rebind_on_start && claude.observed_from_start);
    let codex = adapter(AgentCli::Codex).session_policy();
    assert!(codex.rebind_on_start && !codex.observed_from_start);
    let opencode = adapter(AgentCli::OpenCode).session_policy();
    assert!(opencode.rebind_on_start && !opencode.observed_from_start);
    let hermes = adapter(AgentCli::Hermes).session_policy();
    assert!(hermes.bind_on_submit);
    assert!(!claude.bind_on_submit && !codex.bind_on_submit && !opencode.bind_on_submit);
    assert!(hermes.rebind_on_start && hermes.rebind_to_retired);
    assert!(hermes.observed_from_start);
    // OpenCode and Hermes let their users return to a session in the same process.
    assert!(opencode.rebind_to_retired && !claude.rebind_to_retired && !codex.rebind_to_retired);
}

#[test]
fn claudes_status_hooks_report_what_its_own_hooks_do_and_codex_has_none() {
    let claude = adapter(AgentCli::Claude);
    assert_eq!(
        claude
            .status_hooks()
            .iter()
            .map(|hook| hook.event)
            .collect::<Vec<_>>(),
        [
            "Stop",
            "StopFailure",
            "PermissionRequest",
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "PostToolUseFailure",
            "SessionEnd",
        ]
    );
    for hook in claude.status_hooks() {
        let input = serde_json::json!({"hook_event_name": hook.event, "session_id": "s"});
        let report = claude
            .normalize(&serde_json::to_vec(&input).unwrap(), false)
            .unwrap();
        assert_eq!(report.event, hook.reports, "{}", hook.event);
    }
    assert!(adapter(AgentCli::Codex).status_hooks().is_empty());
    // Project status hooks need folder settings; activity hooks can report attention directly.
    let orion: &dyn AgentCliAdapter = &super::orion::Orion;
    for adapter in AgentCli::every_cli().map(adapter).chain([orion]) {
        let name = &adapter.descriptor().name;
        let hooked = !adapter.status_hooks().is_empty();
        if hooked {
            assert_eq!(
                adapter.descriptor().attention,
                Some(AttentionSignal::Hooks),
                "{name}"
            );
        }
        assert_eq!(
            adapter.descriptor().project_hooks.is_some(),
            hooked,
            "{name}"
        );
    }
    // Claude's status hook runs the script its tab's host installed, never a workspace's.
    let hooks = descriptor(AgentCli::Claude).project_hooks.as_ref().unwrap();
    assert_eq!(hooks.committed, ".claude/settings.json");
    assert_eq!(hooks.local, ".claude/settings.local.json");
    assert_eq!(
        hooks.status_command,
        "[ -x \"$CODESENSE_ACTIVITY_HOST/codesense-notify.sh\" ] && \"$CODESENSE_ACTIVITY_HOST/codesense-notify.sh\" || true"
    );
}

#[test]
fn a_cli_observed_from_start_names_its_main_turn_from_its_first_hook() {
    let start = br#"{"hook_event_name":"SessionStart","session_id":"s"}"#;
    for cli in AgentCli::every_cli() {
        let report = adapter(cli).normalize(start, false);
        if cli == AgentCli::Hermes {
            assert!(
                report.is_none(),
                "normalization awaits its own adapter implementation"
            );
            continue;
        }
        assert_eq!(
            adapter(cli).session_policy().observed_from_start,
            report.is_some_and(|report| report.scope == Scope::Main),
            "{cli}"
        );
    }
}

#[test]
fn the_shared_hook_format_drops_content_and_names_its_events() {
    let claude = adapter(AgentCli::Claude);
    let report = |input: &str| claude.normalize(input.as_bytes(), false);
    // A failure reason outside the known set never carries the CLI's own text.
    let failed =
        report(r#"{"hook_event_name":"StopFailure","session_id":"s","error":"SECRET detail"}"#)
            .unwrap();
    assert_eq!(
        (failed.event, failed.reason.as_deref()),
        (Event::Failed, Some("unknown"))
    );
    let interrupted =
        report(r#"{"hook_event_name":"PostToolUseFailure","session_id":"s","is_interrupt":true}"#)
            .unwrap();
    assert_eq!(interrupted.event, Event::Interrupted);
    assert!(report(r#"{"session_id":"s"}"#).is_none());
    assert!(report(r#"{"hook_event_name":7,"session_id":"s"}"#).is_none());
    assert!(report("not json").is_none());
}

#[test]
fn prepare_adds_the_observer_and_the_inbox_hooks_where_each_cli_reads_them() {
    let mut hermes = Prepare::new(vec![], serde_json::json!({}), "/app/helper");
    adapter(AgentCli::Hermes).prepare(&mut hermes).unwrap();
    assert!(hermes.arguments.is_empty());
    assert_eq!(
        hermes.settings["hooks"]["on_session_start"][0]["command"],
        "'/app/helper' --activity-hook hermes"
    );
    assert_eq!(hermes.settings["hooks_auto_accept"], true);
    assert_eq!(hermes.settings["hooks"].as_object().unwrap().len(), 9);
    let mut claude = Prepare::new(
        vec!["--model".into(), "m".into()],
        serde_json::json!({}),
        "/app/helper",
    );
    adapter(AgentCli::Claude).prepare(&mut claude).unwrap();
    assert_eq!(claude.arguments, ["--model", "m"]);
    assert_eq!(
        claude.settings["hooks"]["Stop"][0]["hooks"][0]["command"],
        "'/app/helper' --activity-hook claude"
    );
    assert!(claude.settings["hooks"]["Stop"][1]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .contains("--inbox-wait claude Stop"));
    let mut codex = Prepare::new(
        vec!["--".into(), "hi".into()],
        serde_json::json!({}),
        "/app/helper",
    );
    adapter(AgentCli::Codex).prepare(&mut codex).unwrap();
    assert_eq!(codex.settings, serde_json::json!({}));
    assert_eq!(codex.arguments[0], "--no-daemon");
    assert_eq!(codex.arguments[codex.arguments.len() - 2..], ["--", "hi"]);
    // At an event both run at, the observer's group comes first and the check's after
    // it, in one override: the observer's group keeps the place its trust is kept under.
    let tool: Vec<&String> = codex
        .arguments
        .iter()
        .filter(|argument| argument.starts_with("hooks.PostToolUse="))
        .collect();
    assert_eq!(tool.len(), 1);
    let table: toml::Table = format!("value={}", tool[0].split_once('=').unwrap().1)
        .parse()
        .unwrap();
    let groups = table["value"].as_array().unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(
        groups[0]["hooks"][0]["command"].as_str(),
        Some("[ -x \"$CODESENSE_ACTIVITY_HOST/activity-helper\" ] || exit 0; exec \"$CODESENSE_ACTIVITY_HOST/activity-helper\" --activity-hook codex")
    );
    assert!(groups[1]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .contains("--inbox-wait codex PostToolUse"));
    // A turn end runs the observer alone.
    let stop = codex
        .arguments
        .iter()
        .find(|argument| argument.starts_with("hooks.Stop="))
        .unwrap();
    assert!(!stop.contains("--inbox-wait"));
}

#[test]
fn control_and_restore_tags_keep_todays_behavior() {
    for cli in [AgentCli::Claude, AgentCli::Codex, AgentCli::Hermes] {
        let control = adapter(cli).control();
        assert!(!control.exit_line.is_empty());
        assert_eq!(control.command_prefix, Some('/'));
        assert!(adapter(cli).restore_tags().permission_mode);
    }
    assert_eq!(adapter(AgentCli::OpenCode).control().exit_line, "/quit");
    assert!(!adapter(AgentCli::OpenCode).restore_tags().permission_mode);
    assert_eq!(adapter(AgentCli::Claude).control().exit_line, "/exit");
    // Codex is reached through its engine; Claude through its terminal and its waiter.
    assert!(adapter(AgentCli::Codex).control().channel.is_some());
    assert!(adapter(AgentCli::Claude).control().channel.is_none());
    // Both TUIs wait for bracketed-paste readiness and prove a typed line by
    // the turn it starts.
    let claude = adapter(AgentCli::Claude).control();
    assert!(claude.bracketed_paste && claude.submit_starts_turn);
    let codex = adapter(AgentCli::Codex).control();
    assert!(codex.bracketed_paste && codex.submit_starts_turn);
}

#[test]
fn claude_reports_usage_on_its_status_line_and_codex_is_polled() {
    assert!(matches!(
        adapter(AgentCli::Claude).output().usage,
        Usage::StatusLine {
            source: "claude_status_line",
            ..
        }
    ));
    let Usage::Poll(poll) = adapter(AgentCli::Codex).output().usage else {
        panic!("Codex is polled");
    };
    assert_eq!(poll.source, "codex_app_server");
    assert_eq!(poll.argv, ["app-server"]);
    // It runs in the account's own home, never on an inherited API key.
    let codex = descriptor(AgentCli::Codex);
    assert_eq!(codex.home_env, "CODEX_HOME");
    assert_eq!(codex.env_suppress, ["OPENAI_API_KEY"]);
    assert_eq!(
        (poll.visible, poll.hidden),
        (Duration::from_secs(60), Duration::from_secs(300))
    );
    // The reply it reads is the last request's.
    let last: serde_json::Value =
        serde_json::from_str(poll.requests.lines().last().unwrap()).unwrap();
    assert_eq!(last["id"], poll.reply_id);
}

fn codex_release() -> &'static Release {
    &descriptor(AgentCli::Codex).release
}

fn claude_release() -> &'static Release {
    &descriptor(AgentCli::Claude).release
}

#[test]
fn codex_versions_in_range_qualify_and_anything_else_stays_unobserved() {
    for version in [
        "codex-cli 0.159.3",
        "codex-cli 0.160.0\n",
        "codex-cli 0.231.12",
        "codex-cli 0.162.1\n",
        "codex-cli 0.162.1-alpha.2",
    ] {
        assert!(codex_release().qualified(version), "{version}");
    }
    for version in [
        "codex-cli 0.158.0",
        "codex-cli 0.159.2",
        "codex-cli 0.159.3-alpha.1",
        "codex-cli 0.12.0",
        "codex-cli 1.0.0",
        "codex 0.160.0",
        "codex-cli banana",
        "",
    ] {
        assert!(!codex_release().qualified(version), "{version}");
    }
}

#[test]
fn version_output_needs_a_short_utf8_qualified_answer() {
    let codex = codex_release();
    assert!(codex.output_qualified(b"codex-cli 0.162.1\n"));
    assert!(!codex.output_qualified(b"codex-cli 0.158.0\n"));
    assert!(!codex.output_qualified(b"codex-cli 0.162.1\xff"));
    let mut long = b"codex-cli 0.162.1".to_vec();
    long.resize(VERSION_OUTPUT_MAX, b' ');
    assert!(codex.output_qualified(&long));
    long.push(b' ');
    assert!(!codex.output_qualified(&long));
    let (major, minor, patch) = codex.min;
    let before = codex.before_major.unwrap() - 1;
    assert_eq!(
        codex.supported,
        format!("Codex {major}.{minor}.{patch} or a newer {before}.x release")
    );
}

#[test]
fn claude_task_version_output_needs_the_floor_or_newer() {
    let claude = claude_release();
    for output in [
        &b"2.1.141 (Claude Code)\n"[..],
        b"2.1.296 (Claude Code)\n",
        b"2.2.0 (Claude Code)",
        b"3.0.0 (Claude Code)\n",
    ] {
        assert!(
            claude.output_qualified(output),
            "{}",
            String::from_utf8_lossy(output)
        );
    }
    for output in [
        &b"2.1.140 (Claude Code)\n"[..],
        b"2.0.999 (Claude Code)\n",
        b"2.1.141-beta.1 (Claude Code)\n",
        b"2.1.296\n",
        b"codex-cli 0.162.1\n",
        b"2.1.296 (Claude Code)\xff",
        b"",
    ] {
        assert!(
            !claude.output_qualified(output),
            "{}",
            String::from_utf8_lossy(output)
        );
    }
    let mut long = b"2.1.296 (Claude Code)".to_vec();
    long.resize(VERSION_OUTPUT_MAX, b' ');
    assert!(claude.output_qualified(&long));
    long.push(b' ');
    assert!(!claude.output_qualified(&long));
    let (major, minor, patch) = claude.min;
    assert_eq!(
        claude.supported,
        format!("Claude Code {major}.{minor}.{patch} or newer")
    );
}

fn startup(args: &[&str], settings: serde_json::Value) -> AgentStartup {
    AgentStartup {
        args: args.iter().map(|arg| arg.to_string()).collect(),
        settings,
        prompt: None,
        env: Default::default(),
    }
}

#[test]
fn saved_agent_env_is_bounded_and_named_portably() {
    let claude = NativeLaunch::Cli(AgentCli::Claude);
    let with_env = |pairs: Vec<(String, String)>| AgentStartup {
        env: pairs.into_iter().collect(),
        ..startup(&[], serde_json::json!({}))
    };
    let valid = with_env(vec![
        ("CLAUDE_CODE_DISABLE_AUTO_MEMORY".into(), "1".into()),
        ("_EMPTY".into(), String::new()),
    ]);
    validate_startup(&valid, claude).unwrap();
    // It crosses the wire as `env`, and a startup without one keeps its old shape.
    let json = serde_json::to_value(&valid).unwrap();
    assert_eq!(json["env"]["CLAUDE_CODE_DISABLE_AUTO_MEMORY"], "1");
    assert_eq!(serde_json::from_value::<AgentStartup>(json).unwrap(), valid);
    let bare = serde_json::to_value(startup(&[], serde_json::json!({}))).unwrap();
    assert!(bare.get("env").is_none());
    for (name, value) in [
        ("1ST", "x".to_string()),
        ("HAS-DASH", "x".to_string()),
        ("", "x".to_string()),
        ("NUL", "a\0b".to_string()),
        ("LONG", "x".repeat(4097)),
    ] {
        assert!(
            validate_startup(&with_env(vec![(name.into(), value)]), claude).is_err(),
            "{name}"
        );
    }
    let many = (0..65).map(|i| (format!("V{i}"), "x".into())).collect();
    assert!(validate_startup(&with_env(many), claude).is_err());
}

#[test]
fn saved_agent_rejects_compact_and_long_workspace_or_config_overrides() {
    let codex = NativeLaunch::Cli(AgentCli::Codex);
    let settings = serde_json::json!({"approval_policy":"on-request"});
    for arg in [
        "-C/tmp",
        "-ckey=value",
        "--cd=/tmp",
        "--config=mcp_servers.x.command=evil",
        "--mcp-config=foreign",
        "--settings=foreign",
    ] {
        assert!(
            validate_startup(&startup(&[arg], settings.clone()), codex).is_err(),
            "{arg}"
        );
    }
    let valid = AgentStartup {
        prompt: Some("--literal prompt".into()),
        ..startup(&["--model", "saved"], settings.clone())
    };
    validate_startup(&valid, codex).unwrap();
    // A shell runs no saved Agent.
    assert!(validate_startup(&valid, NativeLaunch::Shell).is_err());
}

#[test]
fn saved_claude_agent_may_pass_isolation_flags_but_not_scope_flags() {
    let claude = NativeLaunch::Cli(AgentCli::Claude);
    let settings = serde_json::json!({"permissions":{"defaultMode":"acceptEdits"}});
    let startup = |args: &[&str]| startup(args, settings.clone());
    validate_startup(
        &startup(&[
            "--setting-sources=local",
            "--strict-mcp-config",
            "--model",
            "saved",
        ]),
        claude,
    )
    .unwrap();
    validate_startup(&startup(&["--setting-sources", "local"]), claude).unwrap();
    for arg in [
        "--mcp-config",
        "--mcp-config=foreign",
        "--settings",
        "--settings={}",
    ] {
        let error = validate_startup(&startup(&[arg]), claude).unwrap_err();
        let flag = arg.split('=').next().unwrap();
        assert!(error.contains(&format!("{flag:?}")), "{error}");
    }
    // Codex has neither isolation flag, so it still refuses them, by name.
    let codex = self::startup(
        &["--strict-mcp-config"],
        serde_json::json!({"approval_policy":"on-request"}),
    );
    let error = validate_startup(&codex, NativeLaunch::Cli(AgentCli::Codex)).unwrap_err();
    assert!(error.contains("\"--strict-mcp-config\""), "{error}");
}

#[test]
fn each_cli_takes_only_its_own_settings() {
    let claude = NativeLaunch::Cli(AgentCli::Claude);
    let codex = NativeLaunch::Cli(AgentCli::Codex);
    validate_startup(
        &startup(&[], serde_json::json!({"permissions": {}, "hooks": {}})),
        claude,
    )
    .unwrap();
    let error = validate_startup(
        &startup(&[], serde_json::json!({"approval_policy": "never"})),
        claude,
    )
    .unwrap_err();
    assert_eq!(error, "unsupported Claude saved-Agent settings");
    let error = validate_startup(
        &startup(&[], serde_json::json!({"approval_policy": true})),
        codex,
    )
    .unwrap_err();
    assert_eq!(error, "unsupported Codex saved-Agent settings");
}

#[test]
fn settings_become_overrides_only_for_an_override_cli() {
    let settings = serde_json::json!({"sandbox_mode": "read-only", "approval_policy": "never"});
    assert_eq!(
        descriptor(AgentCli::Codex)
            .launch_settings
            .overrides(&settings)
            .unwrap(),
        [
            "-c",
            "approval_policy=\"never\"",
            "-c",
            "sandbox_mode=\"read-only\""
        ]
    );
    assert_eq!(
        descriptor(AgentCli::Claude)
            .launch_settings
            .overrides(&settings),
        None
    );
}

#[test]
fn an_override_spells_only_scalars() {
    let overrides = &descriptor(AgentCli::Codex).launch_settings;
    assert_eq!(
        overrides.overrides(&serde_json::json!({"hooks": {"Stop": []}})),
        None
    );
    assert_eq!(overrides.overrides(&serde_json::json!(["a"])), None);
    assert!(overrides
        .overrides(&serde_json::json!({}))
        .unwrap()
        .is_empty());
}

#[test]
fn each_cli_names_the_env_var_that_moves_its_home_and_its_bypass_flag() {
    let claude = descriptor(AgentCli::Claude);
    assert_eq!(claude.home_env, "CLAUDE_CONFIG_DIR");
    assert_eq!(claude.bypass_flag, "--dangerously-skip-permissions");
    let codex = descriptor(AgentCli::Codex);
    assert_eq!(codex.home_env, "CODEX_HOME");
    assert_eq!(
        codex.bypass_flag,
        "--dangerously-bypass-approvals-and-sandbox"
    );
}

#[test]
fn an_inventory_switches_off_each_listed_server_and_every_builtin() {
    let inventory = descriptor(AgentCli::Codex)
        .mcp_scope
        .as_ref()
        .unwrap()
        .inventory
        .as_ref()
        .unwrap();
    assert_eq!(
        inventory
            .disabled(
                &[
                    ("b.x".into(), McpTransportKind::Http),
                    ("a".into(), McpTransportKind::Stdio),
                ]
                .into()
            )
            .unwrap(),
        [
            "\"a\"={enabled=false,command=\"/usr/bin/false\"}",
            "\"b.x\"={enabled=false,url=\"http://127.0.0.1:9\"}",
            "codex_apps={enabled=false,url=\"http://127.0.0.1:9\"}",
        ]
    );
    assert_eq!(
        inventory
            .disabled(&[("codex_apps".into(), McpTransportKind::Http)].into())
            .unwrap(),
        ["codex_apps={enabled=false,url=\"http://127.0.0.1:9\"}"]
    );
}

#[test]
fn an_inventory_lists_each_server_once() {
    assert_eq!(
        McpInventory::names(br#"[{"name":"b"},{"name":"a"}]"#).unwrap(),
        [
            ("a".to_string(), McpTransportKind::Http),
            ("b".to_string(), McpTransportKind::Http)
        ]
        .into()
    );
    assert_eq!(
        McpInventory::names(br#"[{"name":"s","transport":{"type":"stdio","command":"c"}}]"#)
            .unwrap(),
        [("s".to_string(), McpTransportKind::Stdio)].into()
    );
    assert!(McpInventory::names(br#"[{"name":"a"},{"name":"a"}]"#).is_err());
    assert!(McpInventory::names(br#"[{"name":""}]"#).is_err());
    assert!(McpInventory::names(b"{}").is_err());
}

#[test]
fn a_launchs_own_settings_flag_is_found_once_bare_or_joined() {
    let args = |list: &[&str]| list.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    assert_eq!(
        find_settings_flag(&args(&["-x", "--settings", "{}"]), "--settings").unwrap(),
        Some(SettingsFlag {
            range: 1..3,
            joined: false,
            value: "{}".into()
        })
    );
    assert_eq!(
        find_settings_flag(&args(&["--settings=a.json"]), "--settings").unwrap(),
        Some(SettingsFlag {
            range: 0..1,
            joined: true,
            value: "a.json".into()
        })
    );
    assert_eq!(
        find_settings_flag(&args(&["--settingsx"]), "--settings").unwrap(),
        None
    );
    assert_eq!(
        find_settings_flag(&args(&["--settings"]), "--settings"),
        Err(SettingsFlagError::NoValue)
    );
    assert_eq!(
        find_settings_flag(&args(&["--settings=a", "--settings", "b"]), "--settings"),
        Err(SettingsFlagError::Repeated)
    );
    // A prompt operand is no option.
    assert_eq!(
        find_settings_flag(&args(&["--", "--settings=x"]), "--settings").unwrap(),
        None
    );
}

/// Facts the descriptor states twice agree.
#[test]
fn a_descriptor_states_each_fact_once_in_effect() {
    for cli in AgentCli::every_cli() {
        let descriptor = descriptor(cli);
        let scope = descriptor.mcp_scope.as_ref().unwrap();
        if let ScopeInjection::EnvDir { env_var, .. } = &scope.injection {
            assert_eq!(*env_var, descriptor.home_env, "{cli}");
        }
        if let (
            ScopeInjection::ConfigFlag {
                settings_flag_template: Some(template),
                ..
            },
            LaunchSettings::Flag(flag),
        ) = (&scope.injection, &descriptor.launch_settings)
        {
            assert_eq!(template[0], *flag, "{cli}");
        }
        assert!(!descriptor.headless_entry.is_empty(), "{cli}");
    }
}

#[test]
fn an_override_key_is_a_bare_word() {
    let overrides = &descriptor(AgentCli::Codex).launch_settings;
    assert_eq!(overrides.overrides(&serde_json::json!({"a.b": "x"})), None);
    assert_eq!(
        overrides.overrides(&serde_json::json!({"sandbox_mode": null})),
        None
    );
}

#[test]
fn an_inventory_is_read_under_the_launchs_own_selectors() {
    let inventory = descriptor(AgentCli::Codex)
        .mcp_scope
        .as_ref()
        .unwrap()
        .inventory
        .as_ref()
        .unwrap();
    let args = |list: &[&str]| list.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    assert_eq!(
        inventory
            .selector_args(&args(&[
                "--model",
                "m",
                "-c",
                "a=1",
                "-pwork",
                "--config=b=2",
                "--profile",
                "p",
                "exec"
            ]))
            .unwrap(),
        ["-c", "a=1", "-p", "work", "--config=b=2", "--profile", "p"]
    );
    assert!(inventory.selector_args(&args(&["-c"])).is_err());
    for refused in [
        "--remote",
        "--remote=ws://x",
        "-C",
        "-C/tmp",
        "--cd",
        "--cd=/tmp",
    ] {
        assert!(
            inventory.selector_args(&args(&[refused])).is_err(),
            "{refused}"
        );
    }
    assert!(inventory
        .selector_args(&args(&["--config-file"]))
        .unwrap()
        .is_empty());
    // A prompt operand is no option.
    assert!(inventory
        .selector_args(&args(&["--", "-C keep this", "-c x"]))
        .unwrap()
        .is_empty());
}

/// CodeSense's hooks ride a settings object, which only a settings-flag CLI takes whole:
/// a CLI that hears mail through a waiter or raises needs-you through status hooks
/// takes its settings by flag. OpenCode's hooks ride its plugin, not a settings object.
#[test]
fn a_cli_that_carries_codesense_hooks_takes_its_settings_by_flag() {
    for cli in AgentCli::every_cli().filter(|cli| *cli != AgentCli::OpenCode) {
        let adapter = adapter(cli);
        let hooks = matches!(adapter.inbox(), InboxDelivery::Waiter { .. })
            || !adapter.status_hooks().is_empty();
        if hooks {
            assert!(
                matches!(
                    adapter.descriptor().launch_settings,
                    LaunchSettings::Flag(_)
                ),
                "{cli}"
            );
        }
    }
}

#[test]
fn an_inventory_override_is_its_args_then_one_shaped_value() {
    let inventory = descriptor(AgentCli::Codex)
        .mcp_scope
        .as_ref()
        .unwrap()
        .inventory
        .as_ref()
        .unwrap();
    assert_eq!(
        inventory.override_argv("mcp_servers", &["a={enabled=false}".into()], |value| {
            format!("'{value}'")
        }),
        ["--no-daemon", "-c", "'mcp_servers={a={enabled=false}}'"]
    );
    assert_eq!(
        inventory.override_argv("mcp_servers", &[], str::to_owned),
        ["--no-daemon", "-c", "mcp_servers={}"]
    );
}

#[test]
fn a_release_names_the_minimum_it_admits() {
    for cli in [AgentCli::Claude, AgentCli::Codex] {
        let release = &descriptor(cli).release;
        let (major, minor, patch) = release.min;
        let min = format!("{major}.{minor}.{patch}");
        assert!(
            release.supported.contains(&min),
            "{cli:?}: {}",
            release.supported
        );
        assert!(release.remote_task_refusal().contains(release.supported));
    }
}

#[test]
fn release_slots_name_each_clis_own_range() {
    let codex = descriptor(AgentCli::Codex).release.supported;
    let claude = descriptor(AgentCli::Claude).release.supported;
    assert_eq!(
        with_releases("machine {codex_release}; Mac {claude_release}").as_deref(),
        Some(format!("machine {codex}; Mac {claude}").as_str())
    );
    assert_eq!(with_releases("needs {to} and {sessionId}"), None);
    assert_eq!(remote_executor().release.supported, codex);
}

/// Codex validates the merged config on every write, so each server the scoped launch
/// switches off still names a complete transport of its own type. The placeholder never
/// carries the server's real args, cwd, url or env: they land in the process list.
#[test]
fn a_scoped_codex_switches_off_listed_servers_with_type_matched_placeholders() {
    let inventory = descriptor(AgentCli::Codex)
        .mcp_scope
        .as_ref()
        .unwrap()
        .inventory
        .as_ref()
        .unwrap();
    let listed = McpInventory::names(
        br#"[
        {"name":"proj.srv","transport":{"type":"stdio","command":"/bin/real","args":["--api-key","SECRET"],"env":{"K":"ENVSECRET"},"env_vars":["HOME"],"cwd":"/secret/cwd"}},
        {"name":"web","transport":{"type":"streamable_http","url":"http://h/mcp?token=URLSECRET","bearer_token_env_var":"TOK"}},
        {"name":"codex_apps","transport":{"type":"streamable_http","url":"http://apps/mcp?token=URLSECRET"}},
        {"name":"bare"}
    ]"#,
    )
    .unwrap();
    let entries = inventory.disabled(&listed).unwrap();
    let text = format!("mcp_servers={{{}}}", entries.join(","));
    for secret in [
        "SECRET",
        "/bin/real",
        "/secret/cwd",
        "h/mcp",
        "apps/mcp",
        "HOME",
        "TOK",
    ] {
        assert!(!text.contains(secret), "{secret} leaked: {text}");
    }
    let parsed: toml::Value = toml::from_str(&text).unwrap();
    let servers = parsed["mcp_servers"].as_table().unwrap();
    for (name, row) in servers {
        assert_eq!(row["enabled"].as_bool(), Some(false), "{name}");
    }
    // A stdio server gets a command, never a url (Codex merges the override over its
    // project layer, and a stdio server cannot take a url).
    assert!(servers["proj.srv"].get("command").is_some());
    assert!(servers["proj.srv"].get("url").is_none());
    // Remote, built-in and unreadable servers get the dead local url.
    for name in ["web", "codex_apps", "bare"] {
        assert_eq!(
            servers[name]["url"].as_str(),
            Some("http://127.0.0.1:9"),
            "{name}"
        );
        assert!(servers[name].get("command").is_none(), "{name}");
    }
}

/// Terminal recovery of a dead waiter's mail is the shared default; a CLI whose waiter
/// re-arms itself opts out.
#[test]
fn only_a_cli_whose_waiter_re_arms_itself_opts_out_of_terminal_recovery() {
    for cli in AgentCli::every_cli() {
        assert_eq!(
            adapter(cli).allows_pty_recovery(),
            !matches!(cli, AgentCli::OpenCode | AgentCli::Hermes),
            "{cli}"
        );
    }
}

#[test]
fn version_qualified_judges_the_parsed_token_with_the_same_range_as_raw_output() {
    let opencode = &descriptor(AgentCli::OpenCode).release;
    for token in ["2.1.4", "2.1.21", "2.99.0", "2.1.5-beta.1"] {
        assert!(opencode.version_qualified(token), "{token}");
    }
    for token in [
        "1.17.9",
        "2.1.4-rc.1",
        "2.0.0",
        "2.0.1",
        "banana",
        "",
        "v1.18.21",
    ] {
        assert!(!opencode.version_qualified(token), "{token}");
    }
    assert!(claude_release().version_qualified("2.1.141"));
    assert!(claude_release().version_qualified("3.0.0"));
    assert!(!claude_release().version_qualified("2.1.140"));
    assert!(codex_release().version_qualified("0.162.1"));
    assert!(!codex_release().version_qualified("1.0.0"));
    // Raw output and its bare token agree.
    for (release, raw, token) in [
        (claude_release(), "2.1.150 (Claude Code)", "2.1.150"),
        (codex_release(), "codex-cli 0.158.0", "0.158.0"),
        (
            &descriptor(AgentCli::OpenCode).release,
            "opencode v2.1.4",
            "2.1.4",
        ),
    ] {
        assert_eq!(
            release.qualified(raw),
            release.version_qualified(token),
            "{raw}"
        );
    }
}

/// The credentials a managed account's launch drops are the adapter's to name: Claude's
/// tokens and cloud switches, Codex's exec key, and nothing for a CLI with no managed login.
#[test]
fn a_managed_account_scrubs_the_credentials_its_cli_ranks_above_the_login() {
    let scrub = |cli| descriptor(cli).account_env_scrub.clone();
    assert_eq!(
        scrub(AgentCli::Claude),
        [
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
            "ANTHROPIC_PROFILE",
        ]
    );
    assert_eq!(scrub(AgentCli::Codex), ["CODEX_API_KEY"]);
    assert!(scrub(AgentCli::OpenCode).is_empty());
}

#[test]
fn project_hooks_have_exactly_one_owner() {
    let owners: Vec<_> = AgentCli::every_cli()
        .filter(|cli| descriptor(*cli).project_hooks.is_some())
        .collect();
    assert_eq!(owners, [AgentCli::Claude]);
    assert_eq!(project_hook_cli(), AgentCli::Claude);
    assert!(super::orion::Orion.descriptor().project_hooks.is_none());
}

#[test]
fn usage_snapshot_timing_is_an_explicit_adapter_fact() {
    for (cli, expected) in [
        (AgentCli::Claude, false),
        (AgentCli::Codex, true),
        (AgentCli::OpenCode, false),
        (AgentCli::Hermes, false),
    ] {
        assert_eq!(descriptor(cli).usage_snapshot_timing, expected, "{cli}");
    }
    assert!(!super::orion::Orion.descriptor().usage_snapshot_timing);
}

#[test]
fn attribution_hook_trust_is_an_explicit_adapter_fact() {
    for (cli, expected) in [
        (AgentCli::Claude, false),
        (AgentCli::Codex, true),
        (AgentCli::OpenCode, false),
        (AgentCli::Hermes, false),
    ] {
        assert_eq!(descriptor(cli).attribution_hook_trust, expected, "{cli}");
    }
    assert!(!super::orion::Orion.descriptor().attribution_hook_trust);
}

/// A release that cannot be probed is unqualified for every CLI whose activity is qualified
/// by its version, listed or not: nothing is written into its home (no hooks), no prepared
/// arguments come back (so the host wraps no engine and no waiter is armed), and the launch
/// is told why. A CLI qualified by a help flag is not probed by version at all.
#[test]
fn an_unprobeable_release_gets_no_hooks_no_engine_and_a_stated_reason_for_every_cli() {
    let mut versioned = 0;
    for cli in AgentCli::every_cli() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let prepared = crate::activity::prepare_direct(
            cli,
            std::ffi::OsStr::new("/nonexistent/agent-cli"),
            &[],
            "/helper",
            Some(home.clone()),
            Some(dir.path()),
        );
        assert!(prepared.fatal_error.is_none(), "{cli}");
        if !matches!(adapter(cli).probe().activity, ActivityProbe::Version) {
            continue;
        }
        versioned += 1;
        assert!(prepared.arguments.is_none(), "{cli}: no hooks, no engine");
        assert_eq!(
            prepared.unqualified_reason,
            Some("version probe failed or timed out"),
            "{cli}"
        );
        assert_eq!(
            std::fs::read_dir(&home).unwrap().count(),
            0,
            "{cli}: home untouched"
        );
    }
    assert!(
        versioned >= 2,
        "the table covers the version-qualified CLIs"
    );
}
