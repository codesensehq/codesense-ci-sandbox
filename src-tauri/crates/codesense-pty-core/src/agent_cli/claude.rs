// SPDX-License-Identifier: MIT
//! Claude Code.

use std::sync::LazyLock;

use serde::Deserialize;

use super::hooks::{self, HookInput};
use super::*;

pub(super) struct Claude;

static DESCRIPTOR: LazyLock<Descriptor> = LazyLock::new(|| Descriptor {
    name: "Claude Code".into(),
    binary: "claude".into(),
    // The oldest CLI whose flags this recipe relies on; the dependency check refuses older.
    min_version: (2, 0),
    machine_check: None,
    // A subscription session must never bill an inherited API key.
    env_suppress: strings(&["ANTHROPIC_API_KEY"]),
    // The CLI ranks these above the login in its config folder.
    account_env_scrub: strings(&[
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "ANTHROPIC_PROFILE",
    ]),
    root_doc: "CLAUDE.md".into(),
    shadowing_root_docs: Vec::new(),
    shadowing_root_docs_up_to_git_root: false,
    headless_entry: strings(&["-p"]),
    // The status hooks in the per-launch settings file post `PermissionRequest`.
    attention: Some(AttentionSignal::Hooks),
    mcp_scope: Some(McpScope {
        config_filename: ".mcp.json".into(),
        server_map_key: "mcpServers".into(),
        bridge_id: "codesense".into(),
        bridge_entry: McpBridgeEntry::Command,
        format: ConfigFragmentFormat::Json,
        injection: ScopeInjection::ConfigFlag {
            flag_template: strings(&["--mcp-config", "{path}", "--strict-mcp-config"]),
            open_flag_template: Some(strings(&["--mcp-config", "{path}"])),
            settings_flag_template: Some(strings(&["--settings", "{path}"])),
        },
        bridge_override: None,
        inventory: None,
    }),
    add_dir_flag: Some("--add-dir".into()),
    prompt_operand: true,
    prompt_flag: None,
    initial_prompt_limit: None,
    headless_aliases: strings(&["--print"]),
    bypass_flag: "--dangerously-skip-permissions".into(),
    home_env: "CLAUDE_CONFIG_DIR".into(),
    home_default: Some("~/.claude".into()),
    launch_settings: LaunchSettings::Flag("--settings".into()),
    // `--setting-sources` and `--strict-mcp-config` are its own isolation switches:
    // CodeSense's scope is still passed explicitly, so a saved Agent may pass them.
    startup: StartupRules {
        name: "Claude",
        refused: &[],
        settings_keys: &["permissions", "hooks"],
        string_settings: false,
    },
    default_context: strings(&[
        "CLAUDE.md",
        ".claude/commands",
        ".claude/hooks",
        ".claude/agents",
        ".claude/skills",
    ]),
    skills_dir: ".claude/skills/".into(),
    commands_dir: Some(".claude/commands/".into()),
    telemetry: Some(TelemetryExport {
        env: vec![
            ("CLAUDE_CODE_ENABLE_TELEMETRY".into(), "1".into()),
            ("OTEL_LOGS_EXPORTER".into(), "otlp".into()),
            ("OTEL_METRICS_EXPORTER".into(), "none".into()),
            ("OTEL_EXPORTER_OTLP_PROTOCOL".into(), "http/json".into()),
            ("OTEL_EXPORTER_OTLP_ENDPOINT".into(), "{endpoint}".into()),
        ],
        argv: Vec::new(),
        default_overrides: Vec::new(),
        // One `api_request` per model call, with its own request id and reported cost.
        usage_event_name: "api_request".into(),
        usage_event_kind: None,
        usage_attributes: strings(&[
            "event.name",
            "event.timestamp",
            "request_id",
            "session.id",
            "model",
            "input_tokens",
            "output_tokens",
            "cache_read_tokens",
            "cache_creation_tokens",
            "cache_creation_5m_tokens",
            "cache_creation_1h_tokens",
            "cost_usd",
        ]),
    }),
    usage_snapshot_timing: false,
    attribution_hook_trust: false,
    project_hooks: Some(ProjectHooks {
        committed: ".claude/settings.json".into(),
        local: ".claude/settings.local.json".into(),
        status_command: crate::notify_hook::status_command(),
    }),
    // The oldest release remote tasks were qualified on, with every flag the task
    // harness passes.
    release: Release {
        prefix: "",
        suffix: " (Claude Code)",
        min: (2, 1, 141),
        before_major: None,
        before: None,
        first_line: false,
        supported: "Claude Code 2.1.141 or newer",
    },
});

static PROBE: Probe = Probe {
    cache_version: false,
    version_env: &[],
    version_budget: super::DEFAULT_VERSION_BUDGET,
    launch_flags: &["--mcp-config", "--strict-mcp-config"],
    agent_flags: &["--settings"],
    activity: ActivityProbe::HelpFlag("--settings"),
    help_subcommand: &[],
};

static TRANSCRIPT: LazyLock<StructuredTranscriptCapability> =
    LazyLock::new(|| StructuredTranscriptCapability {
        headless: Some(HeadlessStructured {
            request_argv: strings(&["--output-format", "stream-json", "--verbose"]),
            format: TranscriptFormat::AnthropicStreamJson,
            argv_prompt_flag: None,
        }),
        interactive: Some(InteractiveStructured {
            session_log_glob: "~/.claude/projects/{enc_cwd}/{session_id}.jsonl".into(),
            session_id_argv: Some(strings(&["--session-id", "{session_id}"])),
            format: TranscriptFormat::AnthropicSessionJsonl,
        }),
    });

/// The session log of the Claude session `id` under its home `home`: the one
/// `projects/<encoded cwd>/<id>.jsonl`, found by the id alone so a relocated home or a cwd
/// spelling never matters. `None` unless exactly one regular file (not a link) under `home` is it.
fn session_log(home: &std::path::Path, id: &str) -> Option<std::path::PathBuf> {
    use std::fs;
    uuid::Uuid::parse_str(id)
        .ok()
        .filter(|u| u.to_string() == id)?;
    if !home.is_absolute() {
        return None;
    }
    let projects = home.join("projects");
    if !fs::symlink_metadata(&projects).ok()?.is_dir() {
        return None;
    }
    let root = projects.canonicalize().ok()?;
    let name = format!("{id}.jsonl");
    let mut found = None;
    for entry in fs::read_dir(&root).ok()? {
        let dir = entry.ok()?;
        if !dir.file_type().ok()?.is_dir() {
            continue;
        }
        let candidate = dir.path().join(&name);
        if fs::symlink_metadata(&candidate).is_ok_and(|m| m.is_file()) {
            if found.replace(candidate).is_some() {
                return None;
            }
        }
    }
    let canon = found?.canonicalize().ok()?;
    canon.starts_with(&root).then_some(canon)
}

/// Every hook the activity observer folds.
const OBSERVER_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "Notification",
    "SubagentStart",
    "SubagentStop",
    "PreCompact",
    "Stop",
    "StopFailure",
    "SessionEnd",
];

/// The hooks its status hook runs at: exactly what the status receiver maps.
const STATUS_HOOKS: &[StatusHook] = &[
    StatusHook {
        event: "Stop",
        reports: Event::Stopped,
    },
    StatusHook {
        event: "StopFailure",
        reports: Event::Failed,
    },
    StatusHook {
        event: "PermissionRequest",
        reports: Event::Waiting,
    },
    StatusHook {
        event: "UserPromptSubmit",
        reports: Event::Submitted,
    },
    StatusHook {
        event: "PreToolUse",
        reports: Event::ToolStarted,
    },
    StatusHook {
        event: "PostToolUse",
        reports: Event::ToolFinished,
    },
    StatusHook {
        event: "PostToolUseFailure",
        reports: Event::ToolFinished,
    },
    StatusHook {
        event: "SessionEnd",
        reports: Event::Ended,
    },
];

/// Appends one stable, synchronous observer to a generated settings object, after the
/// user's own handlers; composing again changes nothing.
fn compose_observer(settings: &mut serde_json::Value, helper: &str) -> Result<(), &'static str> {
    let command = hooks::observer_command(AgentCli::Claude, helper);
    hooks::append_settings_hooks(
        settings,
        OBSERVER_EVENTS,
        |_| serde_json::json!({"hooks": [{"type": "command", "command": command, "timeout": 1}]}),
    )
}

/// The subagents a main `Stop`'s `background_tasks` lists as not yet ended, by the
/// `agent_id` their hooks carry (its background shells are not subagents). None when it
/// lists no tasks, or one it can't name, or more than a report holds: the count is then
/// left alone rather than cut.
fn running_subagents(hook: &HookInput) -> Option<Vec<String>> {
    let ids = hook
        .value("background_tasks")?
        .as_array()?
        .iter()
        .filter(|task| {
            task["type"] == "subagent"
                && !matches!(
                    task["status"].as_str(),
                    Some("completed" | "failed" | "killed")
                )
        })
        .map(|task| {
            task["id"]
                .as_str()
                .filter(|id| crate::activity::opaque(id))
                .map(str::to_owned)
        })
        .collect::<Option<Vec<String>>>()?;
    (ids.len() <= crate::activity::MAX_SUBAGENTS).then_some(ids)
}

/// The status line input's allowance windows (Pro and Max plans).
fn status_line_usage(bytes: &[u8]) -> Result<Option<Vec<UsageWindow>>, String> {
    #[derive(Deserialize)]
    struct StatusInput {
        #[serde(default)]
        rate_limits: Option<RateLimits>,
    }
    #[derive(Deserialize)]
    struct RateLimits {
        #[serde(default)]
        five_hour: Option<Window>,
        #[serde(default)]
        seven_day: Option<Window>,
    }
    #[derive(Deserialize)]
    struct Window {
        used_percentage: f64,
        #[serde(default)]
        resets_at: Option<i64>,
    }
    let input: StatusInput = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let Some(rate_limits) = input.rate_limits else {
        return Ok(None);
    };
    let windows = [
        ("five_hour", "5 hour", rate_limits.five_hour),
        ("seven_day", "7 day", rate_limits.seven_day),
    ]
    .into_iter()
    .filter_map(|(id, label, window)| {
        window.map(|window| UsageWindow {
            id: id.into(),
            label: label.into(),
            used_percent: Some(window.used_percentage),
            resets_at: window.resets_at,
        })
    })
    .collect();
    Ok(Some(windows))
}

impl AgentCliAdapter for Claude {
    fn descriptor(&self) -> &'static Descriptor {
        &DESCRIPTOR
    }

    fn probe(&self) -> &'static Probe {
        &PROBE
    }

    /// The observer and the inbox waiter ride the launch's settings, after the user's
    /// own handlers.
    fn prepare(&self, launch: &mut Prepare<'_>) -> Result<(), &'static str> {
        compose_observer(&mut launch.settings, launch.helper)?;
        self.inbox_hooks(launch)
    }

    /// It is reached through its terminal and its waiter.
    fn channel(&self, _: &[String]) -> Option<NativeChannel> {
        None
    }

    fn inbox(&self) -> InboxDelivery {
        InboxDelivery::Waiter {
            events: WAITER_EVENTS,
            replace_events: WAITER_REPLACE_EVENTS,
            hand_over: HandOver::Rewake,
        }
    }

    /// The waiter rides the launch's settings as one `asyncRewake` hook per event; a
    /// settings object that already arms it is left as it is.
    fn inbox_hooks(&self, launch: &mut Prepare<'_>) -> Result<(), &'static str> {
        hooks::append_settings_hooks(&mut launch.settings, &["UserPromptSubmit"], |event| {
            serde_json::json!({"matcher": "", "hooks": [{
                "type": "command", "command": inbox_hook_command(AgentCli::Claude, event), "timeout": 5,
            }]})
        })?;
        hooks::append_settings_hooks(&mut launch.settings, WAITER_EVENTS, |event| {
            serde_json::json!({"matcher": "", "hooks": [{
                "type": "command",
                "command": inbox_hook_command(AgentCli::Claude, event),
                "asyncRewake": true,
                "timeout": INBOX_WAIT_TIMEOUT_SECS,
            }]})
        })
    }

    fn confirms_notice(&self, event: &str) -> bool {
        event == "UserPromptSubmit"
    }

    /// Normal delivery uses its waiter; recovery uses the guarded terminal path.
    fn submit_notice(&self, _: &mut dyn Channel, _: &Notice<'_>) -> SubmitOutcome {
        SubmitOutcome::RefusedNoEffect
    }

    /// Every hook that is not a subagent's names its main turn; its own events report its
    /// prompts and compaction, and its main `Stop` the subagents still running.
    fn normalize(&self, input: &[u8], _qualified: bool) -> Option<Report> {
        let hook = HookInput::parse(input)?;
        let event = match hook.shared_event() {
            Some(event) => event,
            None => match hook.name() {
                "Notification" => match hook.field("notification_type")? {
                    "permission_prompt" => (Event::Waiting, None),
                    "elicitation_dialog" | "elicitation_url_dialog" => {
                        (Event::Waiting, Some("question".to_owned()))
                    }
                    "idle_prompt" => (Event::Prompting, None),
                    _ => return None,
                },
                "PreCompact" => (Event::Compacting, None),
                _ => return None,
            },
        };
        let mut report = hook.report(AgentCli::Claude, event, true)?;
        if report.event == Event::Stopped && report.scope == crate::activity::Scope::Main {
            report.subagents = running_subagents(&hook);
        }
        Some(report)
    }

    /// `/clear` starts a new main session inside the same process.
    fn session_policy(&self) -> SessionPolicy {
        SessionPolicy {
            bind_on_submit: false,
            rebind_on_start: true,
            rebind_to_retired: false,
            observed_from_start: true,
            subagents: Subagents::Reported,
        }
    }

    fn status_hooks(&self) -> &'static [StatusHook] {
        STATUS_HOOKS
    }

    /// One still starting can swallow a paste's end, after which every Enter is a
    /// newline, so it is typed into only once bracketed paste is on.
    fn control(&self) -> Control {
        Control {
            native_readiness: false,
            unready_bound_ms: None,
            exit_line: "/exit",
            bracketed_paste: true,
            submit_starts_turn: true,
            command_prefix: Some('/'),
            channel: None,
        }
    }

    fn output(&self) -> Output {
        Output {
            session_log: Some(session_log),
            session_record: None,
            transcript: Some(&TRANSCRIPT),
            usage: Usage::StatusLine {
                source: "claude_status_line",
                parse: status_line_usage,
            },
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
    fn the_observer_listens_on_every_folded_hook() {
        let mut settings = serde_json::json!({});
        compose_observer(&mut settings, "/app/helper").unwrap();
        for event in OBSERVER_EVENTS {
            assert_eq!(
                settings["hooks"][event][0]["hooks"][0]["command"],
                "'/app/helper' --activity-hook claude",
                "{event}"
            );
        }
        for event in [
            "Notification",
            "SubagentStart",
            "SubagentStop",
            "PreCompact",
            "StopFailure",
        ] {
            assert!(OBSERVER_EVENTS.contains(&event));
        }
    }

    const ID: &str = "c1a0de00-0000-4000-a000-000000000201";

    fn log_in(home: &std::path::Path, project: &str) -> std::path::PathBuf {
        let dir = home.join("projects").join(project);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("{ID}.jsonl"));
        std::fs::write(&file, "{}\n").unwrap();
        file
    }

    #[test]
    fn a_session_log_is_found_by_its_id_whatever_the_project_dir_is_called() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path().canonicalize().unwrap();
        let file = log_in(&home, "-any-encoding-of-a-cwd");
        assert_eq!(session_log(&home, ID), Some(file.canonicalize().unwrap()));
        assert_eq!(session_log(&home, "not-a-uuid"), None);
        assert_eq!(
            session_log(&home, "11111111-2222-4333-8444-555555555555"),
            None
        );
    }

    #[test]
    fn a_session_log_found_twice_or_through_a_link_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path().canonicalize().unwrap();
        log_in(&home, "-a");
        log_in(&home, "-b");
        assert_eq!(
            session_log(&home, ID),
            None,
            "two logs with one id is ambiguous"
        );

        let linked = tempfile::tempdir().unwrap();
        let linked = linked.path().canonicalize().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let target = elsewhere.path().join("log.jsonl");
        std::fs::write(&target, "{}\n").unwrap();
        let dir = linked.join("projects/-a");
        std::fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(&target, dir.join(format!("{ID}.jsonl"))).unwrap();
        assert_eq!(session_log(&linked, ID), None);
        assert_eq!(session_log(std::path::Path::new("relative/home"), ID), None);
    }

    #[test]
    fn the_native_home_is_the_one_the_environment_names_else_the_default() {
        let d = descriptor(AgentCli::Claude);
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| std::ffi::OsString::from(v))
            }
        };
        assert_eq!(
            d.native_home(env(&[
                ("HOME", "/Users/me"),
                ("CLAUDE_CONFIG_DIR", "/acct/claude")
            ])),
            Some("/acct/claude".into())
        );
        assert_eq!(
            d.native_home(env(&[("HOME", "/Users/me")])),
            Some("/Users/me/.claude".into())
        );
        assert_eq!(
            d.native_home(env(&[("CLAUDE_CONFIG_DIR", "relative")])),
            None
        );
        assert_eq!(
            descriptor(AgentCli::Codex).native_home(env(&[("HOME", "/Users/me")])),
            Some("/Users/me/.codex".into())
        );
        assert_eq!(
            descriptor(AgentCli::OpenCode).native_home(env(&[("HOME", "/Users/me")])),
            None
        );
    }

    #[test]
    fn private_settings_preserve_user_handlers_and_do_not_duplicate_observer() {
        let original = serde_json::json!({"permissions":{"allow":["Read"]},"hooks":{"Stop":[{"matcher":"*","hooks":[{"type":"command","command":"user-stop","timeout":42}]}]}});
        let mut settings = original.clone();
        compose_observer(&mut settings, "/app/helper").unwrap();
        let once = settings.clone();
        compose_observer(&mut settings, "/app/helper").unwrap();
        assert_eq!(once, settings);
        assert_eq!(settings["hooks"]["Stop"][0], original["hooks"]["Stop"][0]);
        assert_eq!(settings["permissions"], original["permissions"]);
    }

    #[test]
    fn normalization_drops_sensitive_content_and_names_the_subagent() {
        let raw = br#"{"hook_event_name":"Stop","session_id":"session","agent_id":"child","last_assistant_message":"SECRET","transcript_path":"SECRET","tool_input":{"password":"SECRET"}}"#;
        let report = Claude.normalize(raw, false).unwrap();
        assert_eq!(report.scope, Scope::Child);
        assert!(!serde_json::to_string(&report).unwrap().contains("SECRET"));
        for (event, expected) in [
            ("SubagentStart", Event::SubagentStarted),
            ("SubagentStop", Event::SubagentFinished),
        ] {
            let raw = serde_json::json!({"hook_event_name": event, "session_id": "s", "agent_id": "child", "agent_type": "general-purpose", "last_assistant_message": "SECRET"});
            let report = Claude
                .normalize(&serde_json::to_vec(&raw).unwrap(), false)
                .unwrap();
            assert_eq!(
                (report.event, report.scope, report.agent.as_deref()),
                (expected, Scope::Child, Some("child"))
            );
            assert!(!serde_json::to_string(&report).unwrap().contains("SECRET"));
        }
    }

    #[test]
    fn a_main_stop_lists_the_subagents_still_running_and_not_its_shells() {
        let stop = |tasks: serde_json::Value| {
            let raw = serde_json::json!({"hook_event_name": "Stop", "session_id": "s", "background_tasks": tasks});
            Claude
                .normalize(&serde_json::to_vec(&raw).unwrap(), false)
                .unwrap()
                .subagents
        };
        let tasks = serde_json::json!([
            {"id": "a1", "type": "subagent", "status": "running", "description": "SECRET"},
            {"id": "a2", "type": "subagent", "status": "completed"},
            {"id": "b1", "type": "shell", "status": "running", "command": "SECRET"},
        ]);
        assert_eq!(stop(tasks), Some(vec!["a1".to_owned()]));
        assert_eq!(stop(serde_json::json!([])), Some(vec![]));
        let unnamed = serde_json::json!([{"type": "subagent", "status": "running"}]);
        assert_eq!(stop(unnamed), None);
        let pending = serde_json::json!([{"id": "a3", "type": "subagent", "status": "pending"}]);
        assert_eq!(stop(pending), Some(vec!["a3".to_owned()]));
        // An id a report can't carry: the Stop still counts, its list does not.
        let long =
            serde_json::json!([{"id": "x".repeat(200), "type": "subagent", "status": "running"}]);
        assert_eq!(stop(long), None);
        let many: Vec<_> = (0..=crate::activity::MAX_SUBAGENTS)
            .map(|i| serde_json::json!({"id": format!("a{i}"), "type": "subagent", "status": "running"}))
            .collect();
        assert_eq!(stop(serde_json::json!(many)), None);
        let bare = Claude
            .normalize(br#"{"hook_event_name":"Stop","session_id":"s"}"#, false)
            .unwrap();
        assert_eq!(bare.subagents, None);
    }

    #[test]
    fn the_status_line_reads_documented_pro_max_windows_and_absence() {
        let windows = status_line_usage(br#"{"rate_limits":{"five_hour":{"used_percentage":25,"resets_at":200},"seven_day":{"used_percentage":50,"resets_at":300}}}"#)
            .unwrap()
            .unwrap();
        assert_eq!(
            windows
                .iter()
                .map(|w| (w.id.as_str(), w.used_percent, w.resets_at))
                .collect::<Vec<_>>(),
            [
                ("five_hour", Some(25.0), Some(200)),
                ("seven_day", Some(50.0), Some(300))
            ]
        );
        assert!(status_line_usage(br#"{"model":{"id":"x"}}"#)
            .unwrap()
            .is_none());
        assert!(status_line_usage(b"not json").is_err());
    }
}
