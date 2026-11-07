// SPDX-License-Identifier: MIT
//! Hermes' registered adapter. Listing and managed home projection follow when usable.
use super::*;
use std::sync::LazyLock;

pub(super) struct Hermes;

static DESCRIPTOR: LazyLock<Descriptor> = LazyLock::new(|| {
    Descriptor {
    name: "Hermes Agent".into(),
    binary: "hermes".into(),
    min_version: (0, 22),
    machine_check: None,
    env_suppress: r"
        ACTUAL_API_KEY ACTUAL_BASE_URL AI_GATEWAY_API_KEY AI_GATEWAY_BASE_URL ALIBABA_CODING_PLAN_API_KEY
        ALIBABA_CODING_PLAN_BASE_URL ALIBABA_CODING_PLAN_CN_API_KEY ALIBABA_CODING_PLAN_CN_BASE_URL
        ALIBABA_TOKEN_PLAN_API_KEY ALIBABA_TOKEN_PLAN_BASE_URL ALIBABA_TOKEN_PLAN_CN_API_KEY
        ALIBABA_TOKEN_PLAN_CN_BASE_URL ANTHROPIC_API_KEY ANTHROPIC_BASE_URL ANTHROPIC_TOKEN ARCEEAI_API_KEY
        ARCEE_BASE_URL AWS_ACCESS_KEY_ID AWS_BEARER_TOKEN_BEDROCK AWS_CONTAINER_CREDENTIALS_RELATIVE_URI
        AWS_DEFAULT_REGION AWS_PROFILE AWS_REGION AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN
        AWS_WEB_IDENTITY_TOKEN_FILE AZURE_ANTHROPIC_KEY AZURE_CLIENT_ID AZURE_CLIENT_SECRET
        AZURE_FEDERATED_TOKEN_FILE AZURE_FOUNDRY_API_KEY AZURE_FOUNDRY_BASE_URL AZURE_TENANT_ID
        BEDROCK_BASE_URL CLAUDE_CODE_OAUTH_TOKEN COMMANDCODE_ANTHROPIC_BASE_URL COMMANDCODE_API_KEY
        COMMANDCODE_BASE_URL COPILOT_ACP_BASE_URL COPILOT_API_BASE_URL COPILOT_GITHUB_TOKEN CUSTOM_BASE_URL
        DASHSCOPE_API_KEY DASHSCOPE_BASE_URL DASHSCOPE_CN_BASE_URL DEEPINFRA_API_KEY DEEPINFRA_BASE_URL
        DEEPSEEK_API_KEY DEEPSEEK_BASE_URL FIREWORKS_API_KEY GEMINI_API_KEY GEMINI_BASE_URL GH_TOKEN
        GITHUB_TOKEN GLM_API_KEY GLM_BASE_URL GMI_API_KEY GMI_BASE_URL GOOGLE_API_KEY
        GOOGLE_APPLICATION_CREDENTIALS HERMES_ACCEPT_HOOKS HERMES_CODEX_BASE_URL HERMES_HOME
        HERMES_INFERENCE_MODEL HERMES_INFERENCE_PROVIDER HERMES_MAX_ITERATIONS HERMES_MODEL
        HERMES_SAFE_MODE HERMES_IGNORE_USER_CONFIG HERMES_IGNORE_RULES HERMES_TUI_GATEWAY_URL HERMES_MANAGED_DIR
        HERMES_PORTAL_BASE_URL HERMES_QWEN_BASE_URL HERMES_SHARED_AUTH_DIR HERMES_TUI_PROVIDER
        HERMES_XAI_BASE_URL HF_BASE_URL HF_TOKEN KILOCODE_API_KEY KILOCODE_BASE_URL KIMI_API_KEY
        KIMI_BASE_URL KIMI_CN_API_KEY KIMI_CODING_API_KEY LM_API_KEY LM_BASE_URL META_API_KEY META_BASE_URL
        META_MODEL_API_KEY MINIMAX_API_KEY MINIMAX_BASE_URL MINIMAX_CN_API_KEY MINIMAX_CN_BASE_URL
        MODEL_API_KEY NEBIUS_API_KEY NEBIUS_BASE_URL NEBIUS_TOKEN_FACTORY_API_KEY NOUS_API_KEY
        NOUS_INFERENCE_BASE_URL NOUS_PORTAL_BASE_URL NOVITA_API_KEY NOVITA_BASE_URL NVIDIA_API_KEY
        NVIDIA_BASE_URL OLLAMA_API_KEY OLLAMA_BASE_URL OPENAI_API_KEY OPENAI_BASE_URL OPENCODE_GO_API_KEY
        OPENCODE_GO_BASE_URL OPENCODE_ZEN_API_KEY OPENCODE_ZEN_BASE_URL OPENROUTER_API_KEY
        OPENROUTER_BASE_URL QWEN_API_KEY RAMP_ROUTER_API_KEY RAMP_ROUTER_BASE_URL ROUTER_API_KEY
        STEPFUN_API_KEY STEPFUN_BASE_URL TOKENHUB_API_KEY TOKENHUB_BASE_URL TOKENPLAN_API_KEY
        TOKENPLAN_BASE_URL UPSTAGE_API_KEY UPSTAGE_BASE_URL VERTEX_CREDENTIALS_PATH VERTEX_PROJECT_ID
        VERTEX_REGION XAI_API_KEY XAI_BASE_URL XIAOMI_API_KEY XIAOMI_BASE_URL ZAI_API_KEY Z_AI_API_KEY
    "
    .split_ascii_whitespace()
    .map(str::to_owned)
    .collect(),
    account_env_scrub: Vec::new(),
    root_doc: "AGENTS.md".into(),
    shadowing_root_docs: strings(&[".hermes.md", "HERMES.md"]),
    shadowing_root_docs_up_to_git_root: true,
    headless_entry: strings(&["chat"]),
    attention: None,
    mcp_scope: Some(McpScope {
        config_filename: "config.yaml".into(),
        server_map_key: "mcp_servers".into(),
        bridge_id: "codesense".into(),
        bridge_entry: McpBridgeEntry::CommandEnvironment { environment_key: "env" },
        format: ConfigFragmentFormat::Yaml,
        injection: ScopeInjection::EnvDir {
                carry_home: false,
            env_var: "HERMES_HOME".into(),
            passthrough: Vec::new(),
            default_home: "~/.hermes".into(),
        },
        bridge_override: None,
        inventory: None,
    }),
    add_dir_flag: None,
    prompt_operand: false,
    prompt_flag: Some("--query=".into()),
    initial_prompt_limit: Some((MAX_INITIAL_PROMPT_BYTES, "This starting task is too long for this CLI. Shorten it before launching.".into())),
    headless_aliases: Vec::new(),
    bypass_flag: "--yolo".into(),
    home_env: "HERMES_HOME".into(),
    home_default: Some("~/.hermes".into()),
    launch_settings: LaunchSettings::HomeConfig {
        file: "config.yaml".into(),
    },
    startup: StartupRules {
        name: "Hermes",
        refused: &[],
        settings_keys: &[],
        string_settings: false,
    },
    default_context: strings(&["AGENTS.md", ".agents/skills"]),
    skills_dir: ".agents/skills/".into(),
    commands_dir: None,
    release: Release {
        prefix: "Hermes Agent v",
        suffix: "",
        min: (0, 22, 5),
        before_major: None,
        before: Some((0, 22, 6)),
        first_line: true,
        supported: "Hermes Agent 0.22.5",
    },
    telemetry: None,
    usage_snapshot_timing: false,
    attribution_hook_trust: false,
    project_hooks: None,
}
});

/// The run's stdout is its transcript: Hermes keeps its sessions in a database of its own,
/// so there is no session log to read. The task rides one literal `--query=` token.
static TRANSCRIPT: LazyLock<StructuredTranscriptCapability> =
    LazyLock::new(|| StructuredTranscriptCapability {
        headless: Some(HeadlessStructured {
            request_argv: strings(&["-Q", "--format", "stream-json"]),
            format: TranscriptFormat::HermesStreamJson,
            argv_prompt_flag: DESCRIPTOR.joined_prompt_flag().map(str::to_owned),
        }),
        interactive: None,
    });

static PROBE: Probe = Probe {
    cache_version: true,
    version_env: &[
        ("HERMES_HOME", "{runtime}/probe-home"),
        ("HTTPS_PROXY", "http://127.0.0.1:9"),
        ("HTTP_PROXY", "http://127.0.0.1:9"),
    ],
    // A cold Python start imports its packages first: 0.8 to 2.8 s measured on a loaded Mac.
    version_budget: std::time::Duration::from_secs(5),
    launch_flags: &[],
    agent_flags: &["--tui", "--query", "--yolo"],
    activity: ActivityProbe::Version,
    help_subcommand: &["chat"],
};

const OBSERVER_EVENTS: &[&str] = &[
    "on_session_start",
    "pre_llm_call",
    "pre_tool_call",
    "post_tool_call",
    "pre_approval_request",
    "post_approval_response",
    "subagent_start",
    "subagent_stop",
    "on_session_end",
];

mod native_args;
mod transcript;
pub(super) use transcript::{parse as parse_stream_json, run_failure, run_summary};
pub use native_args::{native_arguments, NativeArguments, Toolsets};

impl AgentCliAdapter for Hermes {
    fn descriptor(&self) -> &'static Descriptor {
        &DESCRIPTOR
    }
    fn probe(&self) -> &'static Probe {
        &PROBE
    }
    fn prepare(&self, launch: &mut Prepare<'_>) -> Result<(), &'static str> {
        // Interactive planning normalizes entry argv; native admission uses the one resolver.
        if !launch.interactive {
            native_arguments(&launch.arguments).check_managed()?;
        }
        if launch.interactive {
            if launch.arguments.first().map(String::as_str) != Some("chat") {
                launch.arguments.insert(0, "chat".into());
            }
            if !native_arguments(&launch.arguments).tui {
                launch.arguments.insert(1, "--tui".into());
            }
        }
        let command = hooks::observer_command(AgentCli::Hermes, launch.helper);
        activity::fold_hooks(
            &mut launch.settings,
            launch.helper,
            OBSERVER_EVENTS
                .iter()
                .map(|event| (*event, command.clone())),
        )
    }
    fn teardown_home(&self, home: &std::path::Path, wait_for_replacement: bool) {
        lifecycle::teardown_home(home, wait_for_replacement);
    }
    fn end_on_engine_loss(&self) -> bool {
        true
    }
    fn channel(&self, options: &[String]) -> Option<NativeChannel> {
        bool::then(native_arguments(options).tui, || NativeChannel {
            engine: strings(&["gateway", "run", "--quiet"]),
            attached: options.to_vec(),
            serves_at: Some(EngineEndpoint::Tcp(channel::resolve)),
        })
    }
    fn inbox(&self) -> InboxDelivery {
        InboxDelivery::HostSubmit {
            checks: &[
                Check {
                    event: "post_tool_call",
                    hand_over: HandOver::Deferred,
                },
                Check {
                    event: "pre_llm_call",
                    hand_over: HandOver::Context,
                },
            ],
        }
    }
    fn inbox_hooks(&self, launch: &mut Prepare<'_>) -> Result<(), &'static str> {
        let helper = launch.helper;
        let delivery = self.inbox();
        let commands = delivery.events().into_iter().map(|event| {
            // Hermes execs without a shell; the helper captures getppid at call time.
            (
                event,
                format!("{} --inbox-wait hermes {event}", crate::shell_quote(helper)),
            )
        });
        activity::fold_hooks(&mut launch.settings, helper, commands)
    }
    fn allows_pty_recovery(&self) -> bool {
        false
    }
    fn confirms_notice(&self, event: &str) -> bool {
        event == "pre_llm_call"
    }
    fn read_check(&self, input: &mut dyn std::io::Read, _max_prompt: usize) -> CheckInput {
        activity::read_check(input)
    }
    fn submit_notice(&self, channel: &mut dyn Channel, notice: &Notice<'_>) -> SubmitOutcome {
        channel.start_turn(notice.text, notice.token)
    }
    fn normalize(&self, input: &[u8], qualified: bool) -> Option<Report> {
        activity::normalize(input, qualified)
    }
    fn session_policy(&self) -> SessionPolicy {
        SessionPolicy {
            bind_on_submit: true,
            rebind_on_start: true,
            rebind_to_retired: true,
            observed_from_start: true,
            subagents: Subagents::Reported,
        }
    }
    fn status_hooks(&self) -> &'static [StatusHook] {
        &[]
    }
    fn control(&self) -> Control {
        Control {
            native_readiness: true,
            unready_bound_ms: Some(10000),
            exit_line: "/quit",
            bracketed_paste: true,
            submit_starts_turn: false,
            command_prefix: Some('/'),
            channel: Some(channel::connect),
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
            permission_mode: true,
        }
    }
}

#[cfg(test)]
mod tests;

mod lifecycle;

mod activity;

mod channel;

/// A gateway for the tests of what reaches Hermes through its engine.
#[cfg(any(test, feature = "test-support"))]
pub mod gateway_fixture;

#[cfg(test)]
mod owner_turn_tests;
