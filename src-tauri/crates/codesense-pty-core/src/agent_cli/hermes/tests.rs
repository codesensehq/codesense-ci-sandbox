// SPDX-License-Identifier: MIT
use super::*;

#[test]
fn only_the_qualified_first_line_of_the_real_banner_is_read() {
    let release = &DESCRIPTOR.release;
    let banner = "Hermes Agent v0.22.5 (2026.9.24) · stable · local bf0203a794a0\nInstall directory: /private/tmp/demo01-sandbox/source\nInstall method: git\nPython: 3.14.7\nOpenAI SDK: 2.24.0\nUp to date\n";
    assert!(banner.len() > VERSION_OUTPUT_MAX);
    assert!(release.qualified(banner));
    assert!(release.output_qualified(banner.as_bytes()));
    for invalid in [
        "Hermes Agent 0.22.5",
        "Hermes Agent v0.22.6",
        "Hermes Agent v0.23.0",
        "Hermes Agent v0.22.5-rc.1",
        "",
    ] {
        assert!(!release.qualified(invalid), "{invalid}");
        assert!(!release.output_qualified(invalid.as_bytes()), "{invalid}");
    }
    let long = format!("Hermes Agent v0.22.5 {}\nshort", "x".repeat(128));
    assert!(!release.qualified(&long));
    assert!(!release.output_qualified(long.as_bytes()));
    assert!(release.version_qualified("0.22.5"));
    assert!(!release.version_qualified("0.22.6"));
}

#[test]
fn preparation_adds_only_activity_hooks_without_shell_expansion() {
    let mut launch = Prepare::new(
        strings(&["chat", "-q", "hello"]),
        serde_json::json!({}),
        "/app's/helper",
    );
    Hermes.prepare(&mut launch).unwrap();
    assert_eq!(launch.arguments, ["chat", "-q", "hello"]);
    assert_eq!(launch.settings["hooks_auto_accept"], true);
    let hooks = launch.settings["hooks"].as_object().unwrap();
    assert_eq!(hooks.len(), OBSERVER_EVENTS.len());
    for event in OBSERVER_EVENTS {
        let entries = hooks[*event].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0]["command"],
            hooks::observer_command(AgentCli::Hermes, launch.helper)
        );
        assert!(!entries[0]["command"].as_str().unwrap().contains("$PPID"));
    }
    let once = launch.settings.clone();
    Hermes.prepare(&mut launch).unwrap();
    assert_eq!(launch.settings, once);
}

#[test]
fn hermes_keeps_unimplemented_output_ports_and_its_listed_home_yaml_recipe() {
    assert!(Hermes.channel(&[]).is_none());
    assert!(matches!(Hermes.inbox(), InboxDelivery::HostSubmit { .. }));
    assert!(!Hermes.allows_pty_recovery());
    assert!(Hermes.status_hooks().is_empty());
    assert!(Hermes
        .normalize(br#"{"hook_event_name":"on_session_start"}"#, true)
        .is_none());
    assert!(matches!(Hermes.output().usage, Usage::None));
    assert!(DESCRIPTOR.attention.is_none() && DESCRIPTOR.telemetry.is_none());
    assert_eq!(DESCRIPTOR.root_doc, "AGENTS.md");
    assert_eq!(DESCRIPTOR.env_suppress.len(), 136);
    for name in [
        "HERMES_HOME",
        "HERMES_ACCEPT_HOOKS",
        "HERMES_MAX_ITERATIONS",
        "HERMES_SAFE_MODE",
        "HERMES_IGNORE_USER_CONFIG",
        "HERMES_IGNORE_RULES",
        "HERMES_TUI_GATEWAY_URL",
        "HERMES_MANAGED_DIR",
    ] {
        assert!(DESCRIPTOR.env_suppress.iter().any(|key| key == name));
    }
    assert!(
        matches!(&DESCRIPTOR.launch_settings, LaunchSettings::HomeConfig { file } if file == "config.yaml")
    );
    let scope = DESCRIPTOR.mcp_scope.as_ref().unwrap();
    assert_eq!(scope.format, ConfigFragmentFormat::Yaml);
    assert!(
        matches!(&scope.injection, ScopeInjection::EnvDir { env_var, .. } if env_var == &DESCRIPTOR.home_env)
    );
    validate_startup(
        &codesense_host_proto::reverse_mcp::AgentStartup {
            args: Vec::new(),
            settings: serde_json::json!({}),
            prompt: None,
            env: Default::default(),
        },
        NativeLaunch::Cli(AgentCli::Hermes),
    )
    .unwrap();
    // Its release marker finds it by its binary, as every listed CLI's does.
    assert_eq!(by_binary("hermes"), Some(AgentCli::Hermes));
    assert_eq!(known_label("hermes"), None);
}

#[test]
fn real_hook_payloads_preserve_turn_scope_and_deduplicate_approvals() {
    use crate::activity::{Event, Scope};
    let rows: Vec<serde_json::Value> = include_str!("fixtures/activity.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let mut children = Vec::new();
    for raw in rows {
        let bytes = serde_json::to_vec(&raw).unwrap();
        let report = Hermes.normalize(&bytes, true).expect("real observer hook");
        assert!(report.turn.as_deref().is_none_or(crate::activity::opaque));
        if raw["extra"]["platform"] == "subagent" {
            assert_eq!(report.scope, Scope::Child);
            assert_eq!(report.agent.as_deref(), raw["session_id"].as_str());
        }
        match raw["hook_event_name"].as_str().unwrap() {
            "on_session_end" => assert_eq!(report.event, Event::Stopped),
            "pre_llm_call" => assert_eq!(report.event, Event::Submitted),
            "pre_approval_request" => {
                assert_eq!(report.event, Event::Waiting);
                assert_eq!(
                    report.report_id,
                    Hermes.normalize(&bytes, true).unwrap().report_id
                );
                assert_eq!(
                    report.tool.as_deref(),
                    raw["extra"]["tool_call_id"].as_str()
                );
            }
            "subagent_start" | "subagent_stop" => children.push(report.agent.unwrap()),
            _ => {}
        }
    }
    assert_eq!(children.len(), 2);
    assert_eq!(
        children[0], children[1],
        "start and stop must count the same child"
    );
    let raw = br#"{"hook_event_name":"pre_llm_call","session_id":"resumed","extra":{"platform":"cli","turn_id":"next"}}"#;
    let report = Hermes.normalize(raw, true).unwrap();
    assert_eq!(
        (report.event, report.scope),
        (Event::Submitted, Scope::Main)
    );
    assert_eq!(Hermes.normalize(raw, false).unwrap().scope, Scope::Unknown);
}

#[test]
fn only_main_turn_start_native_message_receipts_confirm_a_notice() {
    for (event, platform, expected) in [
        ("pre_llm_call", "cli", true),
        ("post_tool_call", "cli", false),
        ("pre_llm_call", "subagent", false),
    ] {
        let raw = serde_json::json!({"hook_event_name":event,"session_id":"s","extra":{
        "platform":platform,"conversation_history":[
            {"role":"assistant","platform_message_id":"wrong"},
            {"role":"user","platform_message_id":"provider-admission","content":"not a wake key"}
        ]}});
        let input =
            activity::read_check(&mut std::io::Cursor::new(serde_json::to_vec(&raw).unwrap()));
        assert!(
            input.prompt.is_none(),
            "text never stands in for a native receipt"
        );
        assert_eq!(
            input.receipts,
            if expected {
                vec!["provider-admission".to_string()]
            } else {
                vec![]
            }
        );
        assert_eq!(input.main, platform != "subagent");
    }
}

#[test]
fn gateway_connect_mints_a_private_ticket_and_queued_is_not_consumption_or_readiness() {
    use super::gateway_fixture::{GatewayFixture, McpServer, ADMISSION_ID, TICKET};
    use serde_json::json;
    for tools_loaded in [false, true] {
        let home = tempfile::tempdir_in("/tmp").unwrap();
        let endpoint = home.path().join("engine.sock");
        let gateway = GatewayFixture::start(home.path(), &endpoint, move || McpServer {
            connected: tools_loaded,
            tools: u64::from(tools_loaded),
        });
        let mut connected = channel::connect(&endpoint, None, Some(home.path())).unwrap();
        assert_eq!(connected.ready_session(), tools_loaded.then(|| "s".into()));
        assert_eq!(
            connected.start_turn("notice", "reservation"),
            if tools_loaded {
                SubmitOutcome::Uncertain
            } else {
                SubmitOutcome::RefusedNoEffect
            }
        );
        assert_eq!(connected.receipt_id(), tools_loaded.then_some(ADMISSION_ID));
        let observed = gateway.observed();
        assert_eq!(
            observed.control,
            [
                ("identify".to_string(), json!({})),
                (
                    "session-ticket".to_string(),
                    json!({"profile_id":"profile","instance_id":"instance","purpose":"interactive"})
                ),
            ]
        );
        assert_eq!(
            observed.subprotocols,
            [format!("hermes-gateway-v1, hermes-gateway-ticket.{TICKET}")]
        );
        let methods: Vec<_> = observed
            .rpc
            .iter()
            .map(|(method, _)| method.as_str())
            .collect();
        assert_eq!(
            methods,
            if tools_loaded {
                vec!["session.list", "session.resume", "prompt.submit"]
            } else {
                vec!["session.list", "session.resume"]
            }
        );
        if tools_loaded {
            assert_eq!(
                observed.prompts(),
                [&json!({"session_id":"s","input_id":"reservation","text":"notice"})]
            );
        }
        let mut files: Vec<_> = std::fs::read_dir(home.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        files.sort();
        assert_eq!(
            files,
            ["config.yaml", "engine.sock", "gateway.sock"],
            "authentication never creates a token file"
        );
    }
}

#[test]
fn probed_policy_and_hook_folding_are_pinned() {
    let control = Hermes.control();
    assert!(control.native_readiness && control.bracketed_paste);
    assert_eq!(control.exit_line, "/quit");
    assert_eq!(control.command_prefix, Some('/'));
    assert_eq!(control.unready_bound_ms, Some(10000));
    let policy = Hermes.session_policy();
    assert!(policy.rebind_on_start && policy.rebind_to_retired && policy.observed_from_start);
    assert_eq!(policy.subagents, Subagents::Reported);
    let mut launch = Prepare::new(vec![], serde_json::json!({}), "test helper");
    Hermes.prepare(&mut launch).unwrap();
    Hermes.inbox_hooks(&mut launch).unwrap();
    let once = launch.settings.clone();
    Hermes.prepare(&mut launch).unwrap();
    Hermes.inbox_hooks(&mut launch).unwrap();
    assert_eq!(once, launch.settings);
    for event in OBSERVER_EVENTS {
        for hook in launch.settings["hooks"][*event].as_array().unwrap() {
            assert_eq!(hook["timeout"], 2);
            assert_eq!(hook["fail_closed"], false);
            assert!(!hook["command"].as_str().unwrap().contains("$PPID"));
        }
    }
    for event in Hermes.inbox().events() {
        let suffix = format!(" --inbox-wait hermes {event}");
        assert!(launch.settings["hooks"][event]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["command"]
                .as_str()
                .is_some_and(|command| command.ends_with(&suffix))));
    }
}

#[test]
fn preparation_preserves_scoped_blocks_and_unrelated_keys_once() {
    let mut launch = Prepare::new(
        Vec::new(),
        serde_json::json!({
            "unrelated": {"keep": true},
            "hooks": {"pre_tool_call": [{"matcher":"^write_file$", "command":"--hook-block hermes", "fail_closed":true}]}
        }),
        "/app's/helper",
    );
    Hermes.prepare(&mut launch).unwrap();
    let blocks = launch.settings["hooks"]["pre_tool_call"]
        .as_array()
        .unwrap();
    assert_eq!(blocks.len(), 2);
    assert_eq!(
        blocks[0]["command"],
        format!("{} --hook-block hermes", crate::shell_quote(launch.helper))
    );
    assert_eq!(launch.settings["unrelated"]["keep"], true);
    Hermes.inbox_hooks(&mut launch).unwrap();
    let once = launch.settings.clone();
    Hermes.prepare(&mut launch).unwrap();
    Hermes.inbox_hooks(&mut launch).unwrap();
    assert_eq!(launch.settings, once);
}

#[test]
fn managed_preparation_rejects_every_recorded_native_config_bypass() {
    let recorded: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/hook-bypass-parser.json")).unwrap();
    assert_eq!(
        recorded["pinned_commit"],
        "bf0203a794a07aac3ff764972ab9087baa4e4f4e"
    );
    let mut refused = 0;
    let mut controls = 0;
    for row in recorded["rows"].as_array().unwrap() {
        let arguments = row["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|arg| arg.as_str().unwrap().to_owned())
            .collect();
        let mut launch = Prepare::new(arguments, serde_json::json!({}), "/helper");
        let result = Hermes.prepare(&mut launch);
        if row["bypass_selected"] == true {
            assert!(result.is_err(), "accepted native bypass: {row}");
            refused += 1;
        } else if row["accepted_by_pinned_scan_and_parser"] == true {
            assert!(
                result.is_ok(),
                "valid native control must remain allowed: {row}"
            );
            controls += 1;
        }
    }
    assert!(refused > 10 && controls >= 3);
    for arg in ["-px", "-p=x", "--profile=x"] {
        let mut launch = Prepare::new(strings(&[arg]), serde_json::json!({}), "/helper");
        assert!(Hermes.prepare(&mut launch).is_err(), "{arg}");
    }
}

#[test]
fn native_engine_keeps_the_prepared_argv() {
    use std::ffi::{OsStr, OsString};
    for given in [
        strings(&["chat", "--tui"]),
        strings(&["chat", "--tui", "--query=task --literal"]),
        strings(&["chat", "--tui", "--", "literal"]),
    ] {
        let args: Vec<OsString> = given.iter().map(OsString::from).collect();
        let wrapped = crate::activity::launch_argv(
            OsStr::new("helper"),
            AgentCli::Hermes,
            OsStr::new("hermes"),
            &args,
        );
        // Decoded as the helper decodes it, provider and all.
        let launch = crate::activity::launch_of(&wrapped).unwrap();
        assert_eq!(launch.provider, AgentCli::Hermes);
        assert_eq!(launch.engine, ["gateway", "run", "--quiet"]);
        assert!(launch.end_on_loss);
        assert!(launch.serves_at);
        assert_eq!(
            launch
                .attached
                .iter()
                .chain(&launch.operands)
                .cloned()
                .collect::<Vec<_>>(),
            given
        );
        assert_eq!(launch.attached, launch.standalone);
    }
    assert!(Hermes
        .channel(&strings(&["chat", "--query=headless"]))
        .is_none());
}

#[test]
fn parent_turn_end_and_child_completion_commute_in_the_shared_owner() {
    use crate::activity::{ActivityOwner, State};
    let rows: Vec<serde_json::Value> = include_str!("fixtures/activity.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let child_start = rows
        .iter()
        .find(|row| row["hook_event_name"] == "subagent_start")
        .unwrap();
    let child_stop = rows
        .iter()
        .find(|row| row["hook_event_name"] == "subagent_stop")
        .unwrap();
    let own_child_stop = rows
        .iter()
        .find(|row| {
            row["hook_event_name"] == "on_session_end" && row["extra"]["platform"] == "subagent"
        })
        .unwrap();
    for child_finishes_first in [false, true] {
        let owner = ActivityOwner::new();
        let writer = owner
            .prepare_native("hermes-order", AgentCli::Hermes, "unused")
            .unwrap();
        owner.register("hermes-order", Some(std::process::id()));
        let fold = |raw: &serde_json::Value| {
            let report = Hermes
                .normalize(&serde_json::to_vec(raw).unwrap(), true)
                .unwrap();
            owner
                .report_for_test(&writer, std::process::id(), report)
                .unwrap()
        };
        let mut main = child_start.clone();
        main["hook_event_name"] = "pre_llm_call".into();
        main["extra"]["turn_id"] = main["extra"]["parent_turn_id"].clone();
        assert_eq!(fold(&main).state, State::Thinking);
        let started = fold(child_start);
        assert_eq!(
            (started.state, started.subagents_open),
            (State::Thinking, 1)
        );
        // Its own on_session_end describes a child turn, never its parent's turn.
        let report = Hermes
            .normalize(&serde_json::to_vec(own_child_stop).unwrap(), true)
            .unwrap();
        assert!(owner
            .report_for_test(&writer, std::process::id(), report)
            .is_err());
        let unchanged = owner.snapshot("hermes-order").unwrap();
        assert_eq!(
            (unchanged.state, unchanged.subagents_open),
            (State::Thinking, 1)
        );
        main["hook_event_name"] = "on_session_end".into();
        if child_finishes_first {
            let stopped = fold(child_stop);
            assert_eq!(
                (stopped.state, stopped.subagents_open),
                (State::Thinking, 0)
            );
            fold(&main);
        } else {
            let stopped = fold(&main);
            assert_eq!((stopped.state, stopped.subagents_open), (State::Done, 1));
            fold(child_stop);
        }
        let final_state = owner.snapshot("hermes-order").unwrap();
        assert_eq!(
            (final_state.state, final_state.subagents_open),
            (State::Done, 0)
        );
        assert!(
            final_state.exit.is_none(),
            "turn end keeps the session alive"
        );
    }
}
