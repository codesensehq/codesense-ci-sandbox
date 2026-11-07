// SPDX-License-Identifier: MIT
//! The one interface between CodeSense and an agent CLI.
//!
//! [`AgentCli`] names a CLI; [`adapter`] returns its [`AgentCliAdapter`], the single
//! place that CLI's facts live. The desktop and the session server both reach a CLI
//! through it, so adding one is a variant plus an adapter module.
//!
//! The ports, each one answered by every adapter (no default bodies: a CLI that
//! can't support a port returns its honest typed value):
//! - [`descriptor`](AgentCliAdapter::descriptor): identity and file layout.
//! - [`probe`](AgentCliAdapter::probe): what a host checks on the installed CLI.
//! - [`prepare`](AgentCliAdapter::prepare): what a launch adds to the CLI's own argv
//!   and settings (its activity and inbox hooks).
//! - [`channel`](AgentCliAdapter::channel): the engine a launch runs on, if any, which
//!   its host reaches through [`Control::channel`] without typing into its terminal.
//! - [`inbox`](AgentCliAdapter::inbox), [`inbox_hooks`](AgentCliAdapter::inbox_hooks)
//!   and [`submit_notice`](AgentCliAdapter::submit_notice): how new mail reaches the
//!   agent.
//! - [`normalize`](AgentCliAdapter::normalize),
//!   [`session_policy`](AgentCliAdapter::session_policy) and
//!   [`status_hooks`](AgentCliAdapter::status_hooks): activity.
//! - [`control`](AgentCliAdapter::control): driving a running CLI.
//! - [`output`](AgentCliAdapter::output): structured transcripts and account usage.
//! - [`restore_tags`](AgentCliAdapter::restore_tags): what a tab needs to come back.

mod claude;
mod codex;
#[cfg(any(test, feature = "test-support"))]
pub use codex::handler_grant_for_test;
pub use codex::{
    attribution_hook_edit, attribution_hook_flags, AttributionHookEdit, ATTRIBUTION_HOOK_MARKER,
};
mod descriptor;
pub mod hermes;
mod hooks;
mod opencode;
/// A CLI that exists only in tests, answering every port the way no built-in does.
#[cfg(any(test, feature = "test-support"))]
pub mod orion;
pub mod output;
pub mod version_cache;

pub use codesense_host_proto::agent_cli::{AgentCli, NativeLaunch};
use codesense_host_proto::reverse_mcp::AgentStartup;
pub use descriptor::*;
pub(crate) use hooks::read_check;
pub use hooks::CheckInput;

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::activity::{Event, Report};

/// Whole seconds below the signed 32-bit millisecond timer ceiling.
/// The helper renews before this finite hook budget expires.
pub(crate) const INBOX_WAIT_TIMEOUT_SECS: u64 = 2_147_483;

/// How CodeSense drives one agent CLI.
pub trait AgentCliAdapter: Send + Sync {
    /// Who the CLI is and how its files are laid out.
    fn descriptor(&self) -> &'static Descriptor;

    /// What a host checks on the installed CLI before launching it.
    fn probe(&self) -> &'static Probe;

    /// Adds the activity observer, which runs `helper`, and the
    /// [`inbox_hooks`](Self::inbox_hooks) to a native launch's argv and settings. A token
    /// it does not know stays where it is, so a caller can hold a place with
    /// [`ARGV_SLOT`].
    fn prepare(&self, launch: &mut Prepare<'_>) -> Result<(), &'static str>;

    /// Per-launch trust for host-authorized workspaces. Never changes a user's config.
    fn workspace_trust(&self, _paths: &[PathBuf]) -> Vec<String> {
        Vec::new()
    }

    /// The extra roots the user's own config in its native `home` gives a launch with
    /// `options`, for a launch that names extra roots of its own
    /// ([`Descriptor::add_dir_flag`]) to keep: empty for a CLI whose extra roots add to its
    /// config's. `Err` when that config can't be read.
    fn configured_roots(
        &self,
        _home: Option<&Path>,
        _options: &[String],
    ) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }

    /// The engine a launch whose final options are `options` runs on, which its host
    /// reaches through [`Control::channel`]; `None` for a CLI (or a run, such as a one-shot
    /// one) reached through its terminal alone.
    fn channel(&self, options: &[String]) -> Option<NativeChannel>;

    /// Stops only verified home-owned background replacements after the committed tree
    /// has ended. Shared with the desktop teardown port; an absent implementation does nothing.
    fn teardown_home(&self, _home: &Path, _wait_for_replacement: bool) {}

    /// End the committed native session if its bound engine dies. A CLI that does so is the
    /// committed root of its PTY, so a launch of it is a direct exec, never typed into a
    /// shell: the one fact that selects both (a CLI with a second reason to exec directly
    /// would get a fact of its own).
    fn end_on_engine_loss(&self) -> bool {
        false
    }

    /// How new mail reaches a session of this CLI.
    fn inbox(&self) -> InboxDelivery;

    /// Adds the inbox delivery and prompt-confirmation hooks to a launch, each running
    /// [`inbox_hook_command`]: nothing for [`InboxDelivery::None`].
    fn inbox_hooks(&self, launch: &mut Prepare<'_>) -> Result<(), &'static str>;

    /// Whether the host may recover mail a dead waiter left, by writing the notice into the
    /// session's terminal. A CLI whose waiter re-arms itself, and whose host never types
    /// into it, opts out; its mail waits for the waiter.
    fn allows_pty_recovery(&self) -> bool {
        true
    }

    /// Whether this hook confirms a prompt wake without waiting or handing over mail.
    fn confirms_notice(&self, _event: &str) -> bool {
        false
    }

    /// Read a native check envelope; adapters may expose opaque delivery receipts.
    fn read_check(&self, input: &mut dyn std::io::Read, max_prompt: usize) -> CheckInput {
        hooks::read_check(input, max_prompt)
    }

    /// Hands `notice` to an idle session whose [`inbox`](Self::inbox) is
    /// [`InboxDelivery::HostSubmit`], through its native `channel`.
    fn submit_notice(&self, channel: &mut dyn Channel, notice: &Notice<'_>) -> SubmitOutcome;

    /// A native hook's input as an activity report, content dropped. `qualified`: the
    /// probe found a CLI whose hooks name their main turn.
    fn normalize(&self, input: &[u8], qualified: bool) -> Option<Report>;

    /// How the CLI's own session ids relate to its invocation, and what its activity
    /// shows of it.
    fn session_policy(&self) -> SessionPolicy;

    /// The hook events at which the CLI's own settings run CodeSense's status hook
    /// (`codesense-notify`): none for a CLI that raises its status otherwise.
    fn status_hooks(&self) -> &'static [StatusHook];

    /// How a host drives a running CLI.
    fn control(&self) -> Control;

    /// What the CLI records of its work: its structured transcript and its account's
    /// usage.
    fn output(&self) -> Output;

    /// The launch tags a tab of this CLI needs to come back after a restart.
    fn restore_tags(&self) -> RestoreTags;
}

/// How a host reaches each CLI's adapter: [`adapter`], or a test's CLI in a built-in's place.
pub type Adapters = fn(AgentCli) -> &'static dyn AgentCliAdapter;

/// The adapter for `cli`.
pub fn adapter(cli: AgentCli) -> &'static dyn AgentCliAdapter {
    match cli {
        AgentCli::Claude => &claude::Claude,
        AgentCli::Codex => &codex::Codex,
        AgentCli::OpenCode => &opencode::OpenCode,
        AgentCli::Hermes => &hermes::Hermes,
    }
}

/// `cli`'s descriptor.
pub fn descriptor(cli: AgentCli) -> &'static Descriptor {
    adapter(cli).descriptor()
}

/// The sole CLI whose project hooks feed the terminal status route. Hand-started
/// session records use this identity. Adding another CLI (including Hermes) must
/// preserve exactly one owner or change the route to carry an explicit identity.
pub fn project_hook_cli() -> AgentCli {
    let owners: Vec<_> = AgentCli::ALL
        .into_iter()
        .filter(|cli| descriptor(*cli).project_hooks.is_some())
        .collect();
    assert_eq!(owners.len(), 1, "exactly one CLI must own project hooks");
    owners[0]
}

/// Read-only prerequisite probes owned by the installed runtimes' descriptors.
pub fn machine_probe() -> String {
    AgentCli::ALL
        .into_iter()
        .filter_map(|cli| descriptor(cli).machine_check)
        .enumerate()
        .map(|(index, check)| {
            format!(
                "printf 'agent_check_{index}=%s\\n' \"$(cat {} 2>/dev/null || true)\"\n",
                crate::shell_quote(check.path)
            )
        })
        .collect()
}

/// Warnings from exactly the setting values the runtime probes reported.
pub fn machine_warnings(output: &str) -> Vec<&'static str> {
    AgentCli::ALL
        .into_iter()
        .filter_map(|cli| descriptor(cli).machine_check)
        .enumerate()
        .filter_map(|(index, check)| {
            let key = format!("agent_check_{index}=");
            output
                .lines()
                .find_map(|line| line.strip_prefix(&key))
                .and_then(|value| check.warning(value))
        })
        .collect()
}

/// The CLI a remote task's machine executes with, whatever CLI runs the task: the Codex
/// exec-server, on the one release range Codex qualifies for every role.
pub fn remote_executor() -> &'static Descriptor {
    descriptor(AgentCli::Codex)
}

/// `text` with each `{<binary>_release}` (such as `{codex_release}`) naming the range that
/// CLI's release check admits, so no text restates a version. `None` when it names none.
pub fn with_releases(text: &str) -> Option<String> {
    let mut filled = None::<String>;
    for cli in AgentCli::ALL {
        let Descriptor {
            binary, release, ..
        } = descriptor(cli);
        let slot = format!("{{{binary}_release}}");
        let current = filled.as_deref().unwrap_or(text);
        if current.contains(&slot) {
            filled = Some(current.replace(&slot, release.supported));
        }
    }
    filled
}

/// Whether `startup` is one a launch of `launch` can run: bounded, and leaving
/// CodeSense's own scope in place by its CLI's [`StartupRules`]. A shell runs no saved
/// Agent.
pub fn validate_startup(startup: &AgentStartup, launch: NativeLaunch) -> Result<(), String> {
    let cli = launch
        .cli()
        .ok_or("invalid or unbounded saved-Agent launch inputs")?;
    if !AgentCli::ALL.contains(&cli) {
        return Err(format!("{cli} is not available yet"));
    }
    startup.validate(&descriptor(cli).startup)
}

/// The CLI whose binary is `basename`.
pub fn by_binary(basename: &str) -> Option<AgentCli> {
    AgentCli::ALL
        .into_iter()
        .find(|cli| descriptor(*cli).binary == basename)
}

/// Executables recognized by the terminal process probe, including liveness-only CLIs.
pub fn known_label(binary: &str) -> Option<&'static str> {
    match binary {
        "claude" => Some("Claude"),
        "codex" => Some("Codex"),
        "gemini" => Some("Gemini"),
        "opencode" => Some("OpenCode"),
        "aider" => Some("Aider"),
        "cursor-agent" => Some("Cursor Agent"),
        _ => None,
    }
}

/// The time a CLI's --version has to answer unless its adapter says otherwise.
pub const DEFAULT_VERSION_BUDGET: std::time::Duration = std::time::Duration::from_secs(1);

/// What a host checks on the installed CLI.
#[derive(Debug, Clone, Copy)]
pub struct Probe {
    /// Allow version caching only for a caller-pinned prepared install. Other CLIs
    /// keep probing live so contextual dispatchers report the current workspace.
    pub cache_version: bool,
    /// Environment for --version; hosts skip unresolved {runtime} values.
    pub version_env: &'static [(&'static str, &'static str)],
    /// How long --version (and a remote host's --help) may take to answer before the CLI
    /// is unqualified. A slow cold start (an interpreter importing its packages) sets
    /// more than the default.
    pub version_budget: std::time::Duration,
    /// The flags a native launch relies on: a host refuses a CLI whose `--help` lacks
    /// any.
    pub launch_flags: &'static [&'static str],
    /// The flags a saved Agent's launch also relies on.
    pub agent_flags: &'static [&'static str],
    /// How a host learns whether the CLI's activity hooks can be observed.
    pub activity: ActivityProbe,
    /// The subcommand whose `--help` lists `launch_flags` and `agent_flags`, when the
    /// bare `--help` does not (its flags live under the subcommand). Empty for a CLI
    /// whose bare `--help` lists them.
    pub help_subcommand: &'static [&'static str],
}

/// How a host learns whether a CLI's activity hooks can be observed.
#[derive(Debug, Clone, Copy)]
pub enum ActivityProbe {
    /// Its `--help` lists this flag.
    HelpFlag(&'static str),
    /// Its `--version` output names a release in its descriptor's
    /// [`release`](Descriptor::release) range. A CLI that passes reports its main turn
    /// ([`AgentCliAdapter::normalize`]'s `qualified`).
    Version,
}

/// A token that holds a place in a [`Prepare`]'s argv for something the caller adds
/// once the launch is prepared. No real argument holds a NUL.
pub const ARGV_SLOT: &str = "\0codesense-argv-slot";

/// A native launch being prepared: its argv (options, then a `--` and its operands)
/// and its private settings object.
pub struct Prepare<'a> {
    /// This preparation owns an interactive startup; observer/headless calls leave it false.
    pub interactive: bool,
    pub arguments: Vec<String>,
    pub settings: serde_json::Value,
    /// What the generated hooks run.
    pub helper: &'a str,
    /// Actual native config home, explicitly selected by the spawning host.
    pub home: Option<PathBuf>,
}

impl<'a> Prepare<'a> {
    /// A launch with `arguments` and `settings`.
    pub fn new(arguments: Vec<String>, settings: serde_json::Value, helper: &'a str) -> Self {
        Self {
            arguments,
            settings,
            helper,
            interactive: false,
            home: None,
        }
    }
}

/// The engine a launch runs its CLI on, which its host reaches it through
/// ([`Control::channel`]). The engine runs the CLI's own program with `engine`, under the
/// CLI's own process, and ends with it; the CLI runs with `attached` in place of its
/// options (its operands follow as before). Should the engine not come up, the CLI runs
/// with its own options. [`CHANNEL_ENDPOINT`] stands for the engine's socket in both.
#[derive(Debug, Clone)]
pub struct NativeChannel {
    pub engine: Vec<String>,
    pub attached: Vec<String>,
    /// An engine that chooses its own endpoint in its native home. The keeper publishes
    /// it at the host's socket; `None` retains the argv-selected endpoint.
    pub serves_at: Option<EngineEndpoint>,
}

/// Adapter-owned endpoint discovery. TCP is resolved for each incoming connection, so
/// a dynamic port need not exist when the keeper starts. Protocol/auth stay in the adapter.
#[derive(Debug, Clone, Copy)]
pub enum EngineEndpoint {
    Unix(fn(&Path) -> PathBuf),
    Tcp(fn(&Path) -> std::io::Result<std::net::SocketAddr>),
}

/// The token that stands for a [`NativeChannel`]'s socket in its arguments. No real
/// argument holds a NUL.
pub const CHANNEL_ENDPOINT: &str = "\0codesense-channel-endpoint";

/// How a host connects to a CLI's engine at `endpoint`, for the session its hooks last
/// named (`session`, the CLI's own id for it; `None` before any hook). A connection to an
/// engine that can't tell which of its sessions the CLI shows now refuses its calls with no
/// effect; one that can't be made, or read, is an error. `home` is the native home
/// captured when this engine was bound, never the host process's ambient home.
pub type ChannelConnect = fn(
    endpoint: &Path,
    session: Option<&str>,
    home: Option<&Path>,
) -> std::io::Result<Box<dyn Channel>>;

/// A host's connection to a running CLI's engine. It speaks the engine's own protocol;
/// a caller sees only what it asked for and an honest outcome.
pub trait Channel: Send {
    /// Native evidence that the initial interactive session is idle and its lifecycle
    /// observers are enabled and trusted. Absence grants no startup authority.
    fn ready_session(&mut self) -> Option<String> {
        None
    }

    /// Starts a turn of the idle session with `text` as its prompt, under `client_id`
    /// (the caller's, and how an outcome left uncertain is reconciled): `Accepted` once
    /// the engine started it, `RefusedNoEffect` when it was refused or never sent,
    /// `Uncertain` when it may have been started and that could not be told.
    fn start_turn(&mut self, text: &str, client_id: &str) -> SubmitOutcome;

    /// The provider's receipt for the last submit, when delivery requires a later hook.
    /// A receipt is not evidence that the agent consumed the notice.
    fn receipt_id(&self) -> Option<&str> {
        None
    }
}

/// How new mail reaches a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxDelivery {
    /// The CLI runs a CodeSense wait hook, armed at `events`, that hands the notice over
    /// as `hand_over` ([`HandOver::Rewake`] or [`HandOver::RewakeConfirmed`]) to wake it.
    /// A waiter armed at one of `replace_events` takes the session's place; any other
    /// defers to the one already waiting. A one-shot run's hooks only look, never wait.
    Waiter {
        events: &'static [&'static str],
        replace_events: &'static [&'static str],
        hand_over: HandOver,
    },
    /// The host hands the notice over through
    /// [`submit_notice`](AgentCliAdapter::submit_notice) while the CLI is idle, and its
    /// `checks` hooks ask once, never waiting, so a working CLI hears it in the same
    /// turn, each as its own [`Check::hand_over`]. A check that carries the hook's prompt
    /// also settles an uncertain submit whose wake key (its token) the prompt holds.
    HostSubmit { checks: &'static [Check] },
    /// No wake path: mail waits for the agent's next turn.
    None,
}

/// A host-submitted CLI's check hook: the event it runs at, and how a notice is handed
/// to the CLI there (a CLI may take one form at one event and refuse it at another).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Check {
    pub event: &'static str,
    pub hand_over: HandOver,
}

impl InboxDelivery {
    /// The hook events whose hook asks for mail.
    pub fn events(&self) -> Vec<&'static str> {
        match self {
            Self::Waiter { events, .. } => events.to_vec(),
            Self::HostSubmit { checks } => checks.iter().map(|check| check.event).collect(),
            Self::None => Vec::new(),
        }
    }

    /// How a hook armed at `event` hands a notice over: `None` when no inbox hook runs
    /// there.
    pub fn hand_over(&self, event: &str) -> Option<HandOver> {
        match self {
            Self::Waiter {
                events, hand_over, ..
            } => events.contains(&event).then_some(*hand_over),
            Self::HostSubmit { checks } => checks
                .iter()
                .find(|check| check.event == event)
                .map(|check| check.hand_over),
            Self::None => None,
        }
    }
}

/// How an inbox hook hands its CLI a notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandOver {
    /// The notice on stderr and exit status 2, which wakes an idle CLI (or reaches a
    /// running turn at its next step).
    Rewake,
    /// The notice on stderr and exit status 2 as for [`Rewake`](Self::Rewake), but the
    /// host's helper tells the host it was printed (so counts it told) only once the hook's
    /// own process confirms that it delivered the notice, by writing
    /// [`WAITER_CONFIRMATION_LINE`] to the helper's stdin. Stdin closed, a different line
    /// or no line within [`CONFIRMATION_WAIT`] says it was not delivered: the helper tells
    /// the host nothing and the host keeps the notice for the next waiter.
    RewakeConfirmed,
    /// The notice as the hook's JSON `additionalContext` on stdout, exit status 0.
    AdditionalContext,
    /// Native `{context: ...}` on stdout, at a provider's supported turn boundary.
    Context,
    /// Admit the check but leave mail held for a later supported context boundary.
    Deferred,
}

/// The hook events a CLI's inbox waiter (an `asyncRewake` hook, or a plugin that runs the
/// helper itself) is armed at: its boot and every turn end (one that failed included), which take the session's place, and after
/// each tool call, which re-arms a session whose waiter woke it mid-turn (so a second
/// message still reaches the running turn) and otherwise defers to the waiter already
/// waiting. A waiter's own re-ask names `PostToolUse`, so it defers the same way.
pub(crate) const WAITER_EVENTS: &[&str] = &["SessionStart", "Stop", "StopFailure", "PostToolUse"];

/// The [`WAITER_EVENTS`] whose waiter takes the session's place.
pub(crate) const WAITER_REPLACE_EVENTS: &[&str] = &["SessionStart", "Stop", "StopFailure"];

/// The helper option an inbox hook runs (see [`inbox_hook_command`]).
pub const INBOX_WAIT_ARG: &str = "--inbox-wait";

/// What a waiter prints, with exit status 2, to renew itself before its hook times out. It
/// is not mail: a host that runs the helper itself (a plugin) re-spawns it and hands over
/// nothing.
pub const WAITER_RENEWAL_LINE: &str = "Re-arming inbox waiter; no action needed.";

/// The line a [`HandOver::RewakeConfirmed`] hook's process writes to its helper's stdin once
/// it has delivered the notice the helper printed.
pub const WAITER_CONFIRMATION_LINE: &str = "delivered";

/// How long a [`HandOver::RewakeConfirmed`] helper waits for that line: less than the
/// host's wait for the helper to say it printed.
pub const CONFIRMATION_WAIT: std::time::Duration = std::time::Duration::from_secs(8);

/// The command an inbox hook armed at `event` of a `cli` session runs: the helper of
/// the host the CLI's tab runs on, found through the tab's environment (so a launch
/// prepared before its host was chosen reaches whichever runs it), naming the CLI
/// process that ran the hook (`$PPID`), which the host checks is the session's agent.
/// Outside a host's tab it does nothing.
pub fn inbox_hook_command(cli: AgentCli, event: &str) -> String {
    let helper = format!(
        "\"${}/{}\"",
        crate::activity::HOST_ENV,
        crate::activity::HELPER_NAME
    );
    format!("[ -x {helper} ] || exit 0; exec {helper} {INBOX_WAIT_ARG} {cli} {event} \"$PPID\"")
}

/// A "you have mail" notice and the reservation it confirms.
#[derive(Debug, Clone, Copy)]
pub struct Notice<'a> {
    pub text: &'a str,
    /// The reservation token: the client id it is handed over under.
    pub token: &'a str,
}

/// What handing something to a CLI did. The host confirms a notice's reservation only on
/// `Accepted`, releases it on `RefusedNoEffect`, and holds it, never re-sending, while
/// `Uncertain`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitOutcome {
    Accepted,
    RefusedNoEffect,
    Uncertain,
}

/// How a CLI's own session ids relate to its invocation, and what its activity shows of
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionPolicy {
    /// A submitted main turn may also establish/rebind its provider session without a
    /// separate start hook. Rebinding still requires the flags below.
    pub bind_on_submit: bool,
    /// A main start with a new session id (Claude's `/clear`) moves the running
    /// invocation onto it.
    pub rebind_on_start: bool,
    /// A main start naming a session the invocation left earlier moves it back onto it,
    /// for a CLI that lets its user return to a session in the same process.
    pub rebind_to_retired: bool,
    /// Every hook it runs that is not a subagent's reports its main turn, qualified or
    /// not, so a live session not observed yet is only waiting for its first hook.
    pub observed_from_start: bool,
    /// Whether it reports its subagents' starts and stops.
    pub subagents: Subagents,
}

/// Whether a CLI reports its subagents: [`Event::SubagentStarted`] and
/// [`Event::SubagentFinished`] from [`AgentCliAdapter::normalize`], and its subagents'
/// own hooks naming them ([`Report::agent`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subagents {
    /// It does: its session counts the subagents that run, and a subagent's hook seen
    /// with no start opens one.
    Reported,
    /// It does not: its session never counts any.
    None,
}

/// A hook event at which a CLI's own settings run CodeSense's status hook, and what it
/// reports there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusHook {
    pub event: &'static str,
    pub reports: Event,
}

/// How a host drives a running CLI.
#[derive(Debug, Clone, Copy)]
pub struct Control {
    /// The bound engine can establish initial observation through
    /// `Channel::ready_session`. Other channels wait for lifecycle hooks.
    pub native_readiness: bool,
    /// How long after a launch binds a native-readiness CLI may stay unready before
    /// it is called stuck on a startup screen; `None` takes the host's default.
    pub unready_bound_ms: Option<u64>,
    /// The line that asks it to exit.
    pub exit_line: &'static str,
    /// It accepts bracketed paste: wait for the terminal mode before typing, and
    /// delimit non-command text so Enter is never read as part of the paste.
    /// Native startup observation is a separate readiness gate.
    pub bracketed_paste: bool,
    /// A line it takes at an idle prompt starts a turn its activity shows, so only that
    /// turn proves the line was submitted.
    pub submit_starts_turn: bool,
    /// What its own commands start with (`/model`): a line that is one goes as typed,
    /// to be read as a command, and may start no turn.
    pub command_prefix: Option<char>,
    /// How a host reaches its engine ([`AgentCliAdapter::channel`]); `None` for a CLI that
    /// runs on none.
    pub channel: Option<ChannelConnect>,
}

/// What a CLI records of its work.
#[derive(Debug, Clone, Copy)]
pub struct Output {
    /// Read only the native record named by a captured thread id in its captured config home.
    pub session_record: Option<fn(&std::path::Path, &str) -> Option<output::SessionRecord>>,
    /// The CLI's own session log for the native session `id` under its home `home`: one regular
    /// file, found by that id alone (never by directory name, cwd or recency), or `None`.
    pub session_log: Option<fn(&std::path::Path, &str) -> Option<std::path::PathBuf>>,
    /// Its structured transcript, if it has one.
    pub transcript: Option<&'static StructuredTranscriptCapability>,
    /// How CodeSense learns its account's allowance windows.
    pub usage: Usage,
}

/// How CodeSense learns a CLI account's allowance windows.
#[derive(Debug, Clone, Copy)]
pub enum Usage {
    /// The CLI hands its status line command a JSON object each time it redraws, which
    /// `parse` reads (`None`: it carried no windows); CodeSense never polls it.
    StatusLine {
        /// What a snapshot from it is recorded as coming from.
        source: &'static str,
        parse: fn(&[u8]) -> Result<Option<Vec<UsageWindow>>, String>,
    },
    /// CodeSense asks for them while it wants them.
    Poll(&'static UsagePoll),
    /// It reports none: its accounts show no allowance windows.
    None,
}

/// How CodeSense asks a CLI for its account's allowance windows: it runs the CLI with
/// `argv`, in the account's home, writes `requests` (one JSON-RPC message a line) to it,
/// and reads the windows from the result of the reply with id `reply_id`.
#[derive(Debug)]
pub struct UsagePoll {
    /// What a snapshot from it is recorded as coming from.
    pub source: &'static str,
    pub argv: &'static [&'static str],
    pub requests: &'static str,
    pub reply_id: u64,
    pub parse: fn(&serde_json::Value) -> Result<Vec<UsageWindow>, String>,
    /// How often it is asked while someone looks at usage, and while no one does.
    pub visible: Duration,
    pub hidden: Duration,
}

/// One allowance window of an account, as its CLI reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageWindow {
    pub id: String,
    pub label: String,
    pub used_percent: Option<f64>,
    /// When it resets, in epoch seconds.
    pub resets_at: Option<i64>,
}

/// The launch tags a tab needs to come back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestoreTags {
    /// Its permission mode, besides the CLI it runs.
    pub permission_mode: bool,
}

#[cfg(test)]
mod tests;
