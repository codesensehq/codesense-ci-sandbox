// SPDX-License-Identifier: MIT
//! Orion: a CLI that exists only in tests, built from the guide "Adding an agent CLI to
//! CodeSense" to show its adapter port by port. Its host hands it its mail
//! ([`InboxDelivery::HostSubmit`]) through the engine it runs on, a fake app server.
//!
//! [`AgentCli`] is closed, so Orion runs in a built-in's place ([`ORION`]). Only an
//! [`ActivityOwner::with_adapters`](crate::activity::ActivityOwner::with_adapters) owner
//! reaches it, through [`adapters`]: the activity owner's inbox paths (mail, submit, sweep,
//! check-hook admission) see Orion, while every other lookup of [`ORION`] (the hook
//! helper's own process, session wake, the session server's control) sees the built-in.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use super::hooks::{self, HookInput};
use super::*;
use crate::activity::Event;

/// The built-in id Orion runs under in an owner that reaches it through [`adapters`].
pub const ORION: AgentCli = AgentCli::Codex;

/// The adapters of an owner whose [`ORION`] sessions run Orion.
pub fn adapters(cli: AgentCli) -> &'static dyn AgentCliAdapter {
    if cli == ORION {
        &Orion
    } else {
        adapter(cli)
    }
}

pub struct Orion;

static DESCRIPTOR: LazyLock<Descriptor> = LazyLock::new(|| Descriptor {
    name: "Orion".into(),
    binary: "orion".into(),
    min_version: (1, 0),
    machine_check: None,
    env_suppress: strings(&["ORION_API_KEY"]),
    account_env_scrub: Vec::new(),
    root_doc: "ORION.md".into(),
    shadowing_root_docs: Vec::new(),
    shadowing_root_docs_up_to_git_root: false,
    headless_entry: strings(&["run"]),
    // It raises needs-you through the bridge.
    attention: Some(AttentionSignal::Mcp),
    // It takes no scoped MCP config: a scoped launch runs unfiltered.
    mcp_scope: None,
    add_dir_flag: None,
    prompt_operand: true,
    prompt_flag: None,
    initial_prompt_limit: None,
    headless_aliases: Vec::new(),
    bypass_flag: "--yolo".into(),
    home_env: "ORION_HOME".into(),
    home_default: None,
    launch_settings: LaunchSettings::Overrides("--set".into()),
    startup: StartupRules {
        name: "Orion",
        refused: &[],
        settings_keys: &["approval"],
        string_settings: true,
    },
    default_context: strings(&["ORION.md"]),
    skills_dir: ".orion/skills/".into(),
    commands_dir: None,
    release: Release {
        prefix: "orion ",
        suffix: "",
        min: (1, 0, 0),
        before_major: Some(2),
        before: None,
        first_line: false,
        supported: "Orion 1.x",
    },
    telemetry: None,
    usage_snapshot_timing: false,
    attribution_hook_trust: false,
    // Its status comes from its activity observer.
    project_hooks: None,
});

static PROBE: Probe = Probe {
    cache_version: false,
    version_env: &[],
    version_budget: super::DEFAULT_VERSION_BUDGET,
    launch_flags: &["--remote", "--hook"],
    agent_flags: &[],
    activity: ActivityProbe::Version,
    help_subcommand: &[],
};

/// Every hook its activity observer folds, in its own names.
const OBSERVER_EVENTS: &[&str] = &[
    "Boot",
    "Prompt",
    "ToolStart",
    "ToolDone",
    "TurnEnd",
    "Exit",
    "Spawn",
    "Join",
];

/// Its check hooks: after each tool call and at each turn end, so a working Orion hears its
/// mail in the same turn.
const CHECKS: &[Check] = &[
    Check {
        event: "ToolDone",
        hand_over: HandOver::AdditionalContext,
    },
    Check {
        event: "TurnEnd",
        hand_over: HandOver::AdditionalContext,
    },
];

/// One `--hook <event>=<command>` pair per event.
fn hook_args(events: &[&str], command: impl Fn(&str) -> String) -> Vec<String> {
    events
        .iter()
        .flat_map(|event| ["--hook".to_string(), format!("{event}={}", command(event))])
        .collect()
}

/// Puts `added` among `arguments`' options: ahead of a bare `--` and its operands.
fn before_operands(arguments: &mut Vec<String>, added: Vec<String>) {
    let at = operands_at(arguments);
    arguments.splice(at..at, added);
}

impl AgentCliAdapter for Orion {
    fn descriptor(&self) -> &'static Descriptor {
        &DESCRIPTOR
    }

    fn probe(&self) -> &'static Probe {
        &PROBE
    }

    /// The observer and the check hooks ride its argv, ahead of any `--`.
    fn prepare(&self, launch: &mut Prepare<'_>) -> Result<(), &'static str> {
        let command = hooks::observer_command(ORION, launch.helper);
        before_operands(
            &mut launch.arguments,
            hook_args(OBSERVER_EVENTS, |_| command.clone()),
        );
        self.inbox_hooks(launch)
    }

    /// Its app server, which its TUI attaches to.
    fn channel(&self, options: &[String]) -> Option<NativeChannel> {
        let mut attached = options.to_vec();
        attached.extend(["--remote".into(), CHANNEL_ENDPOINT.into()]);
        Some(NativeChannel {
            engine: strings(&["serve", "--listen", CHANNEL_ENDPOINT]),
            attached,
            serves_at: options.iter().find_map(|option| match option.as_str() {
                "--serves-at=unix" => Some(EngineEndpoint::Unix(|home| home.join("orion.sock"))),
                "--serves-at=tcp" => Some(EngineEndpoint::Tcp(|home| {
                    std::fs::read_to_string(home.join("orion.addr"))?
                        .trim()
                        .parse()
                        .map_err(|error| {
                            std::io::Error::new(std::io::ErrorKind::InvalidData, error)
                        })
                })),
                _ => None,
            }),
        })
    }

    fn end_on_engine_loss(&self) -> bool {
        true
    }

    fn teardown_home(&self, home: &Path, _wait_for_replacement: bool) {
        let stop = SERVERS
            .lock()
            .unwrap()
            .get(home)
            .and_then(|server| *server.teardown.lock().unwrap());
        if let Some(stop) = stop {
            stop(home);
        }
    }

    fn inbox(&self) -> InboxDelivery {
        InboxDelivery::HostSubmit { checks: CHECKS }
    }

    fn inbox_hooks(&self, launch: &mut Prepare<'_>) -> Result<(), &'static str> {
        let checks = hook_args(&self.inbox().events(), |event| {
            inbox_hook_command(ORION, event)
        });
        before_operands(&mut launch.arguments, checks);
        Ok(())
    }

    fn submit_notice(&self, channel: &mut dyn Channel, notice: &Notice<'_>) -> SubmitOutcome {
        channel.start_turn(notice.text, notice.token)
    }

    fn normalize(&self, input: &[u8], _qualified: bool) -> Option<Report> {
        let hook = HookInput::parse(input)?;
        let event = match hook.name() {
            "Boot" => Event::Started,
            "Prompt" => Event::Submitted,
            "ToolStart" => Event::ToolStarted,
            "ToolDone" => Event::ToolFinished,
            "TurnEnd" => Event::Stopped,
            "Exit" => Event::Ended,
            "Spawn" => Event::SubagentStarted,
            "Join" => Event::SubagentFinished,
            _ => return None,
        };
        hook.report(ORION, (event, None), true)
    }

    fn session_policy(&self) -> SessionPolicy {
        SessionPolicy {
            bind_on_submit: false,
            rebind_on_start: false,
            rebind_to_retired: false,
            observed_from_start: true,
            subagents: Subagents::Reported,
        }
    }

    /// Its status comes from its activity observer.
    fn status_hooks(&self) -> &'static [StatusHook] {
        &[]
    }

    fn control(&self) -> Control {
        Control {
            native_readiness: false,
            unready_bound_ms: None,
            exit_line: ":quit",
            bracketed_paste: false,
            submit_starts_turn: true,
            command_prefix: Some(':'),
            channel: Some(connect),
        }
    }

    fn output(&self) -> Output {
        Output {
            session_log: None,
            session_record: None,
            transcript: None,
            usage: Usage::None,
        }
    }

    fn restore_tags(&self) -> RestoreTags {
        RestoreTags {
            permission_mode: false,
        }
    }
}

/// A fake app server: it answers each turn it is asked to start as the test scripted, and
/// records the client ids it was handed and the sessions it was connected for.
#[derive(Default)]
pub struct Server {
    teardown: Mutex<Option<fn(&Path)>>,
    answers: Mutex<Vec<SubmitOutcome>>,
    handed: Mutex<Vec<String>>,
    sessions: Mutex<Vec<Option<String>>>,
    homes: Mutex<Vec<Option<PathBuf>>>,
    ready: Mutex<Option<String>>,
}

impl Server {
    pub fn teardown_with(&self, stop: fn(&Path)) {
        *self.teardown.lock().unwrap() = Some(stop);
    }
    /// The next turns are answered `answers`, in order; past them, `Accepted`.
    pub fn answer(&self, answers: &[SubmitOutcome]) {
        *self.answers.lock().unwrap() = answers.to_vec();
    }

    /// The client ids of the turns it was asked to start, in order.
    pub fn handed(&self) -> Vec<String> {
        self.handed.lock().unwrap().clone()
    }

    /// The session each connection to it was made for, in order.
    pub fn sessions(&self) -> Vec<Option<String>> {
        self.sessions.lock().unwrap().clone()
    }

    pub fn homes(&self) -> Vec<Option<PathBuf>> {
        self.homes.lock().unwrap().clone()
    }

    pub fn ready(&self, session: &str) {
        *self.ready.lock().unwrap() = Some(session.to_owned());
    }
}

/// Every fake app server, by the endpoint its host gave it.
static SERVERS: LazyLock<Mutex<HashMap<PathBuf, Arc<Server>>>> = LazyLock::new(Default::default);

/// Serves a fake app server at `endpoint`.
pub fn serve(endpoint: &Path) -> Arc<Server> {
    let server = Arc::new(Server::default());
    SERVERS
        .lock()
        .unwrap()
        .insert(endpoint.to_path_buf(), server.clone());
    server
}

struct Connection(Arc<Server>);

impl Channel for Connection {
    fn ready_session(&mut self) -> Option<String> {
        self.0.ready.lock().unwrap().clone()
    }

    fn start_turn(&mut self, _text: &str, client_id: &str) -> SubmitOutcome {
        self.0.handed.lock().unwrap().push(client_id.to_string());
        let mut answers = self.0.answers.lock().unwrap();
        if answers.is_empty() {
            SubmitOutcome::Accepted
        } else {
            answers.remove(0)
        }
    }
}

/// [`Control::channel`]: a connection to the app server at `endpoint`, for `session`.
fn connect(
    endpoint: &Path,
    session: Option<&str>,
    home: Option<&Path>,
) -> std::io::Result<Box<dyn Channel>> {
    let server = SERVERS
        .lock()
        .unwrap()
        .get(endpoint)
        .cloned()
        .ok_or(std::io::ErrorKind::NotFound)?;
    server
        .sessions
        .lock()
        .unwrap()
        .push(session.map(str::to_owned));
    server.homes.lock().unwrap().push(home.map(Path::to_owned));
    Ok(Box::new(Connection(server)))
}

#[cfg(test)]
mod tests;
