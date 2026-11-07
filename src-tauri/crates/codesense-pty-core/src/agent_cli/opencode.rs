// SPDX-License-Identifier: MIT
//! OpenCode's native CLI adapter. Its activity comes from a CodeSense plugin that turns
//! OpenCode's events into the shared hook format; the ports it has no proof for stay
//! explicitly unsupported.

use std::sync::LazyLock;

use super::hooks::HookInput;
use super::*;

mod transcript;

pub(super) struct OpenCode;

static DESCRIPTOR: LazyLock<Descriptor> = LazyLock::new(|| Descriptor {
    name: "OpenCode".into(),
    binary: "opencode".into(),
    min_version: (2, 1),
    machine_check: None,
    env_suppress: strings(&[
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "OPENCODE_API_KEY",
        "GOOGLE_API_KEY",
        "GEMINI_API_KEY",
        "OPENROUTER_API_KEY",
        "DEEPSEEK_API_KEY",
        "OPENCODE_AUTH_CONTENT",
        "OPENCODE_CONFIG_DIR",
        "OPENCODE_CONFIG",
    ]),
    account_env_scrub: Vec::new(),
    root_doc: "AGENTS.md".into(),
    shadowing_root_docs: Vec::new(),
    shadowing_root_docs_up_to_git_root: false,
    headless_entry: strings(&["run", "--format", "json"]),
    // The plugin's PermissionRequest hook reports needs-you through normalize().
    attention: Some(AttentionSignal::Hooks),
    mcp_scope: Some(McpScope {
        config_filename: "opencode.json".into(),
        server_map_key: "mcp".into(),
        bridge_id: "codesense".into(),
        bridge_entry: McpBridgeEntry::LocalCommandArray {
            environment_key: "environment",
        },
        format: ConfigFragmentFormat::Json,
        injection: ScopeInjection::EnvContent {
            env_var: "OPENCODE_CONFIG_CONTENT".into(),
        },
        bridge_override: None,
        inventory: None,
    }),
    add_dir_flag: None,
    prompt_operand: false,
    prompt_flag: Some("--prompt".into()),
    initial_prompt_limit: None,
    headless_aliases: Vec::new(),
    bypass_flag: "--auto".into(),
    home_env: "XDG_CONFIG_HOME".into(),
    home_default: None,
    launch_settings: LaunchSettings::Env("OPENCODE_CONFIG_CONTENT".into()),
    startup: StartupRules {
        name: "OpenCode",
        refused: &[],
        settings_keys: &["permissions"],
        string_settings: false,
    },
    default_context: strings(&["AGENTS.md", ".agents/skills"]),
    skills_dir: ".agents/skills/".into(),
    commands_dir: None,
    release: Release {
        prefix: "opencode v",
        suffix: "",
        min: (2, 1, 4),
        before_major: Some(3),
        before: None,
        first_line: false,
        supported: "OpenCode 2.1.4 or a newer 2.x release",
    },
    // The plugin posts one record per finished assistant message to this endpoint.
    telemetry: Some(TelemetryExport {
        env: vec![("CODESENSE_USAGE_ENDPOINT".into(), "{endpoint}".into())],
        argv: Vec::new(),
        default_overrides: Vec::new(),
        usage_event_name: "opencode.usage".into(),
        usage_event_kind: None,
        usage_attributes: strings(&[
            "event.name",
            "event.timestamp",
            "session.id",
            "message_id",
            "model",
            "provider",
            "input",
            "output",
            "reasoning",
            "cache_read",
            "cache_write",
            "cost",
        ]),
    }),
    usage_snapshot_timing: false,
    attribution_hook_trust: false,
    project_hooks: None,
});

/// The run's stdout is its transcript: OpenCode keeps its sessions in a database, not a
/// log file, so there is no session record to read and nothing to capture.
static TRANSCRIPT: LazyLock<StructuredTranscriptCapability> =
    LazyLock::new(|| StructuredTranscriptCapability {
        headless: Some(HeadlessStructured {
            request_argv: Vec::new(),
            format: TranscriptFormat::OpenCodeRunJson,
            argv_prompt_flag: None,
        }),
        interactive: None,
    });

pub(super) use transcript::{parse as parse_run_json, run_error, run_summary};

static PROBE: Probe = Probe {
    cache_version: false,
    version_env: &[],
    version_budget: super::DEFAULT_VERSION_BUDGET,
    launch_flags: &["--standalone", "--format", "json"],
    agent_flags: &["-m"],
    activity: ActivityProbe::Version,
    help_subcommand: &["run"],
};

impl AgentCliAdapter for OpenCode {
    fn descriptor(&self) -> &'static Descriptor {
        &DESCRIPTOR
    }

    fn probe(&self) -> &'static Probe {
        &PROBE
    }

    fn prepare(&self, launch: &mut Prepare<'_>) -> Result<(), &'static str> {
        if !launch.arguments.iter().any(|arg| arg == "--standalone") {
            launch.arguments.insert(0, "--standalone".into());
        }
        Ok(())
    }

    fn channel(&self, _options: &[String]) -> Option<NativeChannel> {
        None
    }

    /// The plugin runs the waiter at the shared hook events, so the helper's
    /// `--inbox-wait opencode <event>` finds its arming here. A mail notice is submitted
    /// by the plugin in-process, never typed into the terminal, and confirmed to the helper
    /// once submitted, so a notice the plugin could not deliver stays with the host.
    fn inbox(&self) -> InboxDelivery {
        InboxDelivery::Waiter {
            events: WAITER_EVENTS,
            replace_events: WAITER_REPLACE_EVENTS,
            hand_over: HandOver::RewakeConfirmed,
        }
    }

    /// Its plugin re-arms the waiter itself and submits mail in process, so nothing is ever
    /// typed into its terminal to recover a notice.
    fn allows_pty_recovery(&self) -> bool {
        false
    }

    /// The plugin carries the waiter: there is no hook to add to the launch.
    fn inbox_hooks(&self, _launch: &mut Prepare<'_>) -> Result<(), &'static str> {
        Ok(())
    }

    fn submit_notice(&self, _channel: &mut dyn Channel, _notice: &Notice<'_>) -> SubmitOutcome {
        SubmitOutcome::RefusedNoEffect
    }

    fn normalize(&self, input: &[u8], qualified: bool) -> Option<Report> {
        let hook = HookInput::parse(input)?;
        let event = match hook.shared_event() {
            Some(event) => event,
            None => match hook.name() {
                // The plugin's own names: a question awaits the user's answer, and work no
                // prompt started (a compaction, a resumed task) is the agent's own.
                "Question" => (Event::Waiting, Some("question".to_owned())),
                "PreCompact" => (Event::Compacting, None),
                _ => return None,
            },
        };
        let main = qualified || event.0 == Event::Interrupted;
        hook.report(AgentCli::OpenCode, event, main)
    }

    /// A main session started, or returned to, rebinds the tab (the user switches sessions
    /// in one process). No provider session exists before the plugin's first start, and a
    /// child session's start and idle are its subagents'.
    fn session_policy(&self) -> SessionPolicy {
        SessionPolicy {
            bind_on_submit: false,
            rebind_on_start: true,
            rebind_to_retired: true,
            observed_from_start: false,
            subagents: Subagents::Reported,
        }
    }

    fn status_hooks(&self) -> &'static [StatusHook] {
        &[]
    }

    fn control(&self) -> Control {
        Control {
            native_readiness: false,
            unready_bound_ms: None,
            exit_line: "/quit",
            bracketed_paste: false,
            submit_starts_turn: false,
            command_prefix: Some('/'),
            channel: None,
        }
    }

    fn output(&self) -> Output {
        Output {
            session_log: None,
            session_record: None,
            transcript: Some(&TRANSCRIPT),
            usage: Usage::None,
        }
    }

    fn restore_tags(&self) -> RestoreTags {
        RestoreTags {
            permission_mode: false,
        }
    }
}

#[cfg(test)]
mod tests;
