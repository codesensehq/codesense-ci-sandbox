// SPDX-License-Identifier: MIT
//! The facts that identify an agent CLI and lay out its files: its binary,
//! version floor, the env vars a session strips, how it takes a scoped MCP
//! config, and how it raises needs-you.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

pub use codesense_host_proto::reverse_mcp::StartupRules;

/// A conservative cross-host budget for one startup argv/environment string.
/// Below Linux's 128 KiB per-string cap; leaves room for the flag or environment key.
pub const MAX_INITIAL_PROMPT_BYTES: usize = 64 * 1024;

/// Who an agent CLI is and how it is laid out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    /// Human-readable name, e.g. `"Claude Code"`.
    pub name: String,
    /// The binary basename: what PATH discovery resolves, what a launch runs, and what
    /// [`crate::process::agent_signature_matches`] recognizes as running.
    pub binary: String,
    /// The lowest `(major, minor)` CLI version these recipes rely on; the dependency
    /// check refuses an older binary.
    pub min_version: (u32, u32),
    /// A machine prerequisite for this runtime, checked before launching it.
    pub machine_check: Option<MachineCheck>,
    /// Env vars stripped from a spawned session so it never inherits an API key that
    /// would silently bill instead of the login.
    pub env_suppress: Vec<String>,
    /// Env vars stripped, on top of `env_suppress`, from a process that runs under a managed
    /// account's login folder: credentials the CLI ranks above that login, which would
    /// otherwise bill (or report) another identity. A launch on the person's own login keeps
    /// them.
    pub account_env_scrub: Vec<String>,
    /// The root instructions file the CLI reads on its own (`CLAUDE.md`, `AGENTS.md`).
    pub root_doc: String,
    /// Root documents that take precedence over the injected instructions file.
    pub shadowing_root_docs: Vec<String>,
    /// Search ancestors through the nearest .git file/directory; without one, cwd only.
    pub shadowing_root_docs_up_to_git_root: bool,
    /// The argv tokens that put this CLI into headless (stdin-reading) mode, ahead of a
    /// headless launch's args.
    pub headless_entry: Vec<String>,
    /// How this CLI raises needs-you. `None` = liveness only.
    pub attention: Option<AttentionSignal>,
    /// How this CLI takes a codesense-scoped MCP config. `None` = a scoped launch runs
    /// unfiltered rather than getting a foreign CLI's flags.
    pub mcp_scope: Option<McpScope>,
    /// The argv flag this CLI takes to read a directory beyond its cwd. A team-workspace
    /// session runs in its own dir and reads the team content root through it.
    pub add_dir_flag: Option<String>,
    /// Whether it takes its starting prompt as its last argv operand, after a `--`.
    pub prompt_operand: bool,
    /// A flag that accepts its starting prompt directly (for CLIs whose prompt is an
    /// option rather than a positional operand). A headless run reads it only when its
    /// structured capability sets `argv_prompt_flag`; otherwise the prompt goes on stdin, so a
    /// prompt that starts with `-` cannot become a flag. A trailing `=` joins the literal
    /// prompt into one argv token.
    pub prompt_flag: Option<String>,
    /// Maximum UTF-8 bytes of an interactive starting task and its refusal copy.
    pub initial_prompt_limit: Option<(usize, String)>,
    /// Other spellings of the headless entry (Claude's `--print` for `-p`).
    pub headless_aliases: Vec<String>,
    /// The flag that turns every approval and permission prompt off.
    pub bypass_flag: String,
    /// The env var that relocates the CLI's own home (its login and config).
    pub home_env: String,
    /// Where the CLI keeps its own files when `home_env` is unset (`~/` is the user's home).
    /// `None` for a CLI whose session files are not kept in a home directory.
    pub home_default: Option<String>,
    /// How a launch hands it CodeSense's settings (permissions and hooks).
    pub launch_settings: LaunchSettings,
    /// What its saved-Agent startup may carry.
    pub startup: StartupRules,
    /// The startup context a launch places when its agent declares none.
    pub default_context: Vec<String>,
    /// Where a placed skill lives: `<skills_dir><name>/SKILL.md`.
    pub skills_dir: String,
    /// Where a placed command lives: `<commands_dir>[<ns>/]<name>.md`. `None`: it reads
    /// no project commands.
    pub commands_dir: Option<String>,
    /// The releases it is qualified on, read from `--version`: the range a remote task's
    /// local runtime runs on (and, for Codex, the machine's executor), and, for a CLI
    /// whose activity probe is [`ActivityProbe::Version`], the range whose hooks report
    /// its main turn. Not the dependency floor, which is [`min_version`](Self::min_version).
    ///
    /// [`ActivityProbe::Version`]: super::ActivityProbe::Version
    pub release: Release,
    /// How the CLI's own telemetry is pointed at CodeSense's local receiver. `None` for a CLI with no
    /// verified usage export.
    pub telemetry: Option<TelemetryExport>,
    /// The settings it reads hooks from in the folder it runs in, and the command its
    /// [`status_hooks`](super::AgentCliAdapter::status_hooks) run. `None` for a CLI with no
    /// status hooks.
    pub project_hooks: Option<ProjectHooks>,
    /// Whether observed activity snapshots supply usage turn boundaries and conversation
    /// checkpoints. False when timing comes from hooks or no snapshot contract is verified.
    /// A new CLI (including Hermes) must declare this fact explicitly; core stays CLI-neutral.
    pub usage_snapshot_timing: bool,
    /// Whether the CLI asks the user to trust hooks before it runs them, so attribution hooks
    /// need a host that carries that trust handshake. A new CLI must declare this explicitly.
    pub attribution_hook_trust: bool,
}

impl Descriptor {
    /// The directory the CLI keeps its own files in for a child with environment `environment`:
    /// the one `home_env` names, else the CLI's default. This is where its session log lives.
    pub fn native_home(
        &self,
        environment: impl Fn(&str) -> Option<std::ffi::OsString>,
    ) -> Option<std::path::PathBuf> {
        if let Some(home) = environment(&self.home_env) {
            let home = std::path::PathBuf::from(home);
            return home.is_absolute().then_some(home);
        }
        let default = self.home_default.as_deref()?;
        let home = match default.strip_prefix("~/") {
            Some(relative) => std::path::PathBuf::from(environment("HOME")?).join(relative),
            None => std::path::PathBuf::from(default),
        };
        home.is_absolute().then_some(home)
    }

    /// Actual native home selected by the child's environment, never its MCP projection.
    pub fn config_home(
        &self,
        environment: impl Fn(&str) -> Option<std::ffi::OsString>,
    ) -> Option<std::path::PathBuf> {
        if let Some(home) = environment(&self.home_env) {
            let home = std::path::PathBuf::from(home);
            return home.is_absolute().then_some(home);
        }
        let ScopeInjection::EnvDir { default_home, .. } = &self.mcp_scope.as_ref()?.injection
        else {
            return None;
        };
        let home = if let Some(relative) = default_home.strip_prefix("~/") {
            std::path::PathBuf::from(environment("HOME")?).join(relative)
        } else {
            std::path::PathBuf::from(default_home)
        };
        home.is_absolute().then_some(home)
    }
}

/// A runtime's read-only machine prerequisite. A missing kernel setting is not restricted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineCheck {
    pub label: &'static str,
    pub path: &'static str,
    pub restricted: &'static str,
    pub remedy: &'static str,
}
impl MachineCheck {
    pub fn warning(&self, value: &str) -> Option<&'static str> {
        (value.trim() == self.restricted).then_some(self.remedy)
    }
    pub fn verify(&self) -> Result<(), String> {
        match std::fs::read_to_string(self.path)
            .ok()
            .and_then(|value| self.warning(&value))
        {
            Some(message) => Err(message.into()),
            None => Ok(()),
        }
    }
}

/// The settings files a CLI reads hooks from in the folder it runs in (merging them with a
/// launch's own), and how CodeSense's status hook runs from them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectHooks {
    /// The committed settings file, relative to the folder.
    pub committed: String,
    /// The personal one beside it: the only one CodeSense writes, for a CLI typed by hand
    /// in a worktree it made.
    pub local: String,
    /// The status hook's command: the `codesense-notify.sh` CodeSense installs beside its
    /// host's activity helper, found through the tab's host root, and nothing outside a
    /// host's tab.
    pub status_command: String,
}

/// How a launch hands a CLI a settings object (CodeSense's permissions and hooks, with a
/// saved Agent's own).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchSettings {
    /// The whole object, as a JSON file path or inline JSON, after this flag (Claude
    /// `--settings`). A launch that already passes one has it folded in.
    Flag(String),
    /// Each top-level key as its own `key=value` override after this flag (Codex `-c`).
    Overrides(String),
    /// The whole object in a named environment variable (OpenCode's
    /// `OPENCODE_PERMISSION`).
    Env(String),
    /// Merge hooks into this YAML file in Prepare.home, leaving argv and env untouched.
    HomeConfig { file: String },
}

impl LaunchSettings {
    /// `settings` as `Overrides` argv: `flag key=value` per key, sorted by key. `None` for
    /// a `Flag` CLI, which takes the object whole, for settings that are no object, or for
    /// a key that is not a bare word or a value that is an object, array or null, which
    /// one override cannot spell.
    pub fn overrides(&self, settings: &serde_json::Value) -> Option<Vec<String>> {
        let Self::Overrides(flag) = self else {
            return None;
        };
        let mut keys: Vec<_> = settings.as_object()?.iter().collect();
        let bare = |key: &str| {
            !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        };
        if keys.iter().any(|(key, value)| {
            !bare(key) || value.is_object() || value.is_array() || value.is_null()
        }) {
            return None;
        }
        keys.sort_by_key(|(key, _)| *key);
        Some(
            keys.into_iter()
                .flat_map(|(key, value)| [flag.clone(), format!("{key}={value}")])
                .collect(),
        )
    }
}

/// A launch's own settings flag in its args: where it is, whether its value is joined to
/// it by `=`, and the value (inline JSON, or a file path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsFlag {
    pub range: std::ops::Range<usize>,
    pub joined: bool,
    pub value: String,
}

/// Why a launch's own settings flag cannot be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsFlagError {
    /// The bare flag ends the args, with no value after it.
    NoValue,
    /// It is given twice; the CLI would keep only the last.
    Repeated,
}

/// Where `arguments`' operands start: at a bare `--` (which they include), else past their
/// end. Everything before it is an option.
pub fn operands_at<S: AsRef<std::ffi::OsStr>>(arguments: &[S]) -> usize {
    arguments
        .iter()
        .position(|argument| argument.as_ref() == "--")
        .unwrap_or(arguments.len())
}

impl Descriptor {
    /// The prompt flag when a trailing `=` joins the literal prompt into one argv token.
    pub fn joined_prompt_flag(&self) -> Option<&str> {
        self.prompt_flag.as_deref().filter(|flag| flag.ends_with('='))
    }

    /// Whether `arguments` run the CLI one-shot, printing only its result: the first token
    /// of its headless entry, or another spelling of it, among the options.
    pub fn one_shot<S: AsRef<std::ffi::OsStr>>(&self, arguments: &[S]) -> bool {
        let markers = self
            .headless_entry
            .first()
            .into_iter()
            .chain(&self.headless_aliases);
        arguments[..operands_at(arguments)].iter().any(|argument| {
            markers
                .clone()
                .any(|marker| argument.as_ref() == marker.as_str())
        })
    }
}

/// The launch's own `flag` among the options in `args` (before any `--`), bare (its
/// value is the next arg) or joined by `=`.
pub fn find_settings_flag(
    args: &[String],
    flag: &str,
) -> Result<Option<SettingsFlag>, SettingsFlagError> {
    let mut found = None;
    let args = &args[..operands_at(args)];
    for (at, arg) in args.iter().enumerate() {
        let span = if arg == flag {
            let value = args.get(at + 1).ok_or(SettingsFlagError::NoValue)?;
            SettingsFlag {
                range: at..at + 2,
                joined: false,
                value: value.clone(),
            }
        } else if let Some(value) = arg.strip_prefix(flag).and_then(|r| r.strip_prefix('=')) {
            SettingsFlag {
                range: at..at + 1,
                joined: true,
                value: value.to_owned(),
            }
        } else {
            continue;
        };
        if found.is_some() {
            return Err(SettingsFlagError::Repeated);
        }
        found = Some(span);
    }
    Ok(found)
}

/// The releases a CLI is qualified on, read from its `--version` output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Release {
    /// What its `--version` output reads before the release number.
    pub prefix: &'static str,
    /// What its `--version` output reads after the release number.
    pub suffix: &'static str,
    /// The oldest qualified release.
    pub min: (u64, u64, u64),
    /// The first major it has not been checked against, if any.
    pub before_major: Option<u64>,
    /// An exclusive release bound, if the recipe is qualified on a narrower range.
    pub before: Option<(u64, u64, u64)>,
    /// Read only a first line bounded at VERSION_OUTPUT_MAX, then its version token.
    pub first_line: bool,
    /// The qualified range, as user-facing refusals name it.
    pub supported: &'static str,
}

/// The most `--version` output a check reads before refusing it.
pub const VERSION_OUTPUT_MAX: usize = 128;

impl Release {
    /// The `--version` output of the oldest qualified release.
    #[cfg(any(test, feature = "test-support"))]
    pub fn minimum_output(&self) -> String {
        let (major, minor, patch) = self.min;
        format!("{}{major}.{minor}.{patch}{}", self.prefix, self.suffix)
    }

    /// Whether `version` (its `--version` output) names a qualified release. A
    /// prerelease or build of the minimum itself is not the qualified release; a
    /// prerelease of a later version sorts above the minimum and is admitted.
    pub fn qualified(&self, version: &str) -> bool {
        let version = if self.first_line {
            let first = version.split('\n').next().unwrap_or_default();
            if first.len() > VERSION_OUTPUT_MAX {
                return false;
            }
            first
        } else {
            version
        };
        version
            .trim()
            .strip_prefix(self.prefix)
            .and_then(|number| number.strip_suffix(self.suffix))
            .is_some_and(|number| {
                let token = if self.first_line {
                    number.split_whitespace().next().unwrap_or_default()
                } else {
                    number
                };
                self.version_qualified(token)
            })
    }

    /// Whether `token` (the release number alone, as the dependency check parses it)
    /// is a qualified release. The one place the range is decided.
    pub fn version_qualified(&self, token: &str) -> bool {
        release_at_least(token, self.min).is_some_and(|release| {
            self.before_major.is_none_or(|before| release.0 < before)
                && self.before.is_none_or(|before| release < before)
        })
    }

    /// The refusal a remote task gives for a CLI outside this range, naming the range.
    pub fn remote_task_refusal(&self) -> String {
        format!(
            "capabilityUnavailable: remote tasks require {}",
            self.supported
        )
    }

    /// Whether raw `--version` output is short, UTF-8 and names a qualified release.
    pub fn output_qualified(&self, stdout: &[u8]) -> bool {
        let output = if self.first_line {
            stdout
                .split(|byte| *byte == b'\n')
                .next()
                .unwrap_or_default()
        } else {
            stdout
        };
        output.len() <= VERSION_OUTPUT_MAX
            && std::str::from_utf8(output).is_ok_and(|version| self.qualified(version))
    }
}

/// Parses `major.minor.patch` and returns it when it is at least `min`.
fn release_at_least(number: &str, min: (u64, u64, u64)) -> Option<(u64, u64, u64)> {
    let core = number.split(['-', '+']).next().unwrap_or_default();
    let parts: Vec<_> = core.split('.').map(str::parse::<u64>).collect();
    let [Ok(major), Ok(minor), Ok(patch)] = parts.as_slice() else {
        return None;
    };
    let release = (*major, *minor, *patch);
    (release > min || (release == min && core == number)).then_some(release)
}

/// How an agent raises "needs you". Every mode lands on the same terminal dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AttentionSignal {
    /// Lifecycle hooks (`PermissionRequest`) post the state.
    Hooks,
    /// The agent calls the bridge's `signal_progress { phase: "blocked" }` / `ask_user`.
    Mcp,
}

/// How a CLI takes a codesense-scoped MCP config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpScope {
    /// Project-config path the CLI reads, relative to the worktree cwd (Claude:
    /// `.mcp.json`, Codex: `.codex/config.toml`).
    pub config_filename: String,
    /// Top-level key holding the server map (Claude: `mcpServers`).
    pub server_map_key: String,
    /// The codesense bridge server id, force-included in every scoped set so the agent
    /// can always `signal_done`.
    pub bridge_id: String,
    /// How this CLI registers CodeSense's bridge.
    pub bridge_entry: McpBridgeEntry,
    /// Format of the CLI's config file: how the projector parses the canonical file and
    /// serializes the scoped set.
    pub format: ConfigFragmentFormat,
    /// How the projected scoped config is delivered to the launched CLI.
    pub injection: ScopeInjection,
    /// How a launch with no allowlist still reaches the bridge, for a CLI that does not
    /// read the worktree's project config on its own. `None` = it reads it.
    pub bridge_override: Option<BridgeOverride>,
    /// How a CLI that keeps its own home lists the servers it already has, so a scoped
    /// launch switches each off and adds the selected ones as one override. `None`: the
    /// CLI takes a scoped config file instead.
    pub inventory: Option<McpInventory>,
}

/// How an MCP server the CLI lists reaches its tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum McpTransportKind {
    Stdio,
    /// Also what a server with an unreadable transport counts as.
    #[default]
    Http,
}

/// The servers a CLI lists, by name.
pub type McpListing = BTreeMap<String, McpTransportKind>;

/// The native shape of a CodeSense MCP server entry for a CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpBridgeEntry {
    Command,
    /// A command whose managed MCP child accepts only an explicit server environment.
    CommandEnvironment {
        environment_key: &'static str,
    },
    LocalCommandArray {
        environment_key: &'static str,
    },
}

impl McpBridgeEntry {
    /// Where a managed command must receive the instance route and verified caller.
    pub fn explicit_environment_key(self) -> Option<&'static str> {
        match self {
            Self::CommandEnvironment { environment_key } => Some(environment_key),
            _ => None,
        }
    }

    /// Build the descriptor-owned bridge entry. A known session is passed explicitly to
    /// OpenCode because its MCP child environment is opt-in; a hand-typed CLI inherits the
    /// tab environment and leaves the environment map absent.
    pub fn render(self, command: &str, session_id: Option<&str>) -> serde_json::Value {
        match self {
            Self::Command | Self::CommandEnvironment { .. } => {
                serde_json::json!({ "command": command })
            }
            Self::LocalCommandArray { environment_key } => {
                let mut entry = serde_json::json!({
                    "type": "local",
                    "command": [command],
                    "enabled": true
                });
                if let Some(session_id) = session_id {
                    entry[environment_key] = serde_json::json!({
                        "CODESENSE_TAB_SESSION_ID": session_id
                    });
                }
                entry
            }
        }
    }
}

/// How a CLI lists its own registered MCP servers, and the override a scoped launch
/// gives it in their place (Codex: `mcp list --json`, then `-c mcp_servers={…}`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpInventory {
    /// The argv that prints the registered servers as a JSON array of `{"name": …}`.
    pub argv: Vec<String>,
    /// Servers the CLI registers by itself, switched off whether it lists them or not.
    pub builtin: Vec<String>,
    /// What a stdio server is given beside `enabled=false`, so the switch-off names a
    /// complete server of its own type without its real command line.
    pub stdio_fields: String,
    /// The same for a remote server, and for a built-in one the CLI does not list.
    pub http_fields: String,
    /// The argv ahead of the override's value.
    pub override_args: Vec<String>,
    /// The launch's own flags that select which configuration it reads, each taking a
    /// value; the inventory is read under the same ones.
    pub selectors: Vec<String>,
    /// Launch flags a scoped launch refuses, each with why.
    pub refused: Vec<RefusedFlag>,
}

/// Launch flags a scoped launch cannot honor, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedFlag {
    pub flags: Vec<String>,
    pub reason: String,
}

/// `Some(None)` when `arg` is `flag`, `Some(Some(value))` when it carries the value
/// joined (`--long=value`, or a two-character `-Xvalue`), `None` otherwise.
pub(super) fn flag_value<'a>(arg: &'a str, flag: &str) -> Option<Option<&'a str>> {
    if arg == flag {
        return Some(None);
    }
    let rest = arg.strip_prefix(flag)?;
    if flag.starts_with("--") {
        rest.strip_prefix('=').map(Some)
    } else {
        (flag.len() == 2 && !rest.is_empty()).then_some(Some(rest))
    }
}

impl McpInventory {
    /// The launch's configuration selectors among its options in `args` (before any
    /// `--`), each as its flag and its value, so the inventory is read as the launch will
    /// run. A refused flag is refused with its reason.
    pub fn selector_args(&self, args: &[String]) -> Result<Vec<String>, String> {
        let mut selected = Vec::new();
        let mut args = args.iter().take_while(|arg| arg.as_str() != "--");
        while let Some(arg) = args.next() {
            if let Some(refused) = self.refused.iter().find(|refused| {
                refused
                    .flags
                    .iter()
                    .any(|flag| flag_value(arg, flag).is_some())
            }) {
                return Err(format!("{arg}: {}", refused.reason));
            }
            let Some((flag, value)) = self
                .selectors
                .iter()
                .find_map(|flag| flag_value(arg, flag).map(|value| (flag, value)))
            else {
                continue;
            };
            match value {
                Some(value) if flag.starts_with("--") => selected.push(format!("{flag}={value}")),
                Some(value) => selected.extend([flag.clone(), value.to_owned()]),
                None => {
                    let value = args
                        .next()
                        .ok_or_else(|| format!("{flag} is missing its value"))?;
                    selected.extend([flag.clone(), value.clone()]);
                }
            }
        }
        Ok(selected)
    }

    /// The registered servers in the output of [`argv`](Self::argv), each with its
    /// transport kind: at most 256, each named once. An unreadable transport counts as
    /// remote.
    pub fn names(output: &[u8]) -> Result<McpListing, &'static str> {
        let rows: Vec<serde_json::Value> =
            serde_json::from_slice(output).map_err(|_| "invalid MCP inventory")?;
        if rows.len() > 256 {
            return Err("MCP inventory exceeds server limit");
        }
        let mut listed = BTreeMap::new();
        for row in rows {
            let name = row
                .get("name")
                .and_then(serde_json::Value::as_str)
                .filter(|name| !name.is_empty() && name.len() <= 256)
                .ok_or("MCP inventory has an invalid registration")?;
            let kind = match row["transport"]["type"].as_str() {
                Some("stdio") => McpTransportKind::Stdio,
                _ => McpTransportKind::Http,
            };
            if listed.insert(name.to_owned(), kind).is_some() {
                return Err("MCP inventory has duplicate registrations");
            }
        }
        Ok(listed)
    }

    /// The override entries that switch off every server in `inherited` (what its
    /// inventory listed) and every built-in one. The CLI validates the merged
    /// configuration on every write, so each entry carries a placeholder transport of the
    /// server's own type. Never the real one: its args, url and env land in argv.
    pub fn disabled(&self, inherited: &McpListing) -> Result<Vec<String>, String> {
        let mut entries = Vec::new();
        for (name, kind) in inherited
            .iter()
            .filter(|(name, _)| !self.builtin.contains(name))
        {
            let key = serde_json::to_string(name).map_err(|e| e.to_string())?;
            entries.push(format!("{key}={{enabled=false,{}}}", self.fields(*kind)));
        }
        for builtin in &self.builtin {
            let kind = inherited.get(builtin).copied().unwrap_or_default();
            entries.push(format!("{builtin}={{enabled=false,{}}}", self.fields(kind)));
        }
        Ok(entries)
    }

    fn fields(&self, kind: McpTransportKind) -> &str {
        match kind {
            McpTransportKind::Stdio => &self.stdio_fields,
            McpTransportKind::Http => &self.http_fields,
        }
    }

    /// The override argv that sets the server map `map_key` to exactly `entries`, its
    /// value shaped by `quote` for the caller's launch builder.
    pub fn override_argv(
        &self,
        map_key: &str,
        entries: &[String],
        quote: impl Fn(&str) -> String,
    ) -> Vec<String> {
        let mut argv = self.override_args.clone();
        argv.push(quote(&format!("{map_key}={{{}}}", entries.join(","))));
        argv
    }
}

/// Launch flags that register the codesense bridge for one invocation without touching
/// the CLI's own config (Codex: `["-c", "mcp_servers.codesense.command={bridge}"]`).
/// Exactly one token carries [`MCP_BRIDGE_PATH_PLACEHOLDER`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeOverride {
    pub flag_template: Vec<String>,
}

impl BridgeOverride {
    /// The launch flags with the bridge path substituted. `None` when the bridge path is
    /// unavailable or would not survive the verbatim argv join (whitespace).
    pub fn render(&self, bridge: &Path) -> Option<Vec<String>> {
        let bridge = bridge.to_str()?;
        if bridge.is_empty() || bridge.chars().any(char::is_whitespace) {
            return None;
        }
        Some(
            self.flag_template
                .iter()
                .map(|t| t.replace(MCP_BRIDGE_PATH_PLACEHOLDER, bridge))
                .collect(),
        )
    }
}

/// The `{bridge}` placeholder [`BridgeOverride::flag_template`] substitutes.
pub const MCP_BRIDGE_PATH_PLACEHOLDER: &str = "{bridge}";

/// How a scoped launch hands the projected config to the CLI: a closed set of delivery
/// mechanisms, named by shape, so the projector's dispatch is exhaustively type-checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeInjection {
    /// Write the scoped config to an ephemeral file and point the CLI at it via launch
    /// flags; one `flag_template` token carries [`MCP_SCOPE_PATH_PLACEHOLDER`] (Claude:
    /// `["--mcp-config", "{path}", "--strict-mcp-config"]`).
    ///
    /// `open_flag_template` is how a launch with no allowlist adds a config holding only
    /// the bridge on top of the CLI's own servers (Claude: `["--mcp-config", "{path}"]`).
    /// `settings_flag_template` is how a launch takes a per-launch settings file, which
    /// carries CodeSense's status hooks (Claude: `["--settings", "{path}"]`). Either
    /// absent: that launch adds nothing.
    ConfigFlag {
        flag_template: Vec<String>,
        open_flag_template: Option<Vec<String>>,
        settings_flag_template: Option<Vec<String>>,
    },
    /// Write the scoped config into an ephemeral per-session dir and export that dir to
    /// the child via an env var, bringing `passthrough` files (e.g. credentials) in from
    /// the CLI's real home so relocating the home doesn't break them (Codex:
    /// `CODEX_HOME`).
    EnvDir {
        /// Env var exported to the child, pointing at the ephemeral scoped-config dir.
        env_var: String,
        /// Carry personal config and passthrough files into the managed home.
        carry_home: bool,
        /// Bare filenames brought in from the real home (its current `env_var` value, or
        /// `default_home`). An absent source is skipped with a breadcrumb.
        passthrough: Vec<Passthrough>,
        /// The CLI's real home when `env_var` is unset, with a leading `~` expanded.
        default_home: String,
    },
    /// Serialize the complete scoped config into an environment variable; no file is
    /// written into the project or session scope directory.
    EnvContent { env_var: String },
}

/// A file brought from the CLI's real home into the ephemeral scoped-config dir under
/// [`ScopeInjection::EnvDir`] (e.g. `auth.json`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Passthrough {
    /// Bare filename: one path segment, no separators or `..`.
    pub name: String,
    pub mode: PassthroughMode,
}

/// How an [`EnvDir`](ScopeInjection::EnvDir) `passthrough` file is brought in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassthroughMode {
    /// Symlink the real file into the ephemeral dir (no secret copied). Unix only; falls
    /// back to `Copy` on non-unix platforms.
    Symlink,
    /// Copy the real file (0600) into the ephemeral dir.
    Copy,
}

/// `template` with `path` in place of [`MCP_SCOPE_PATH_PLACEHOLDER`]. The caller owns
/// quoting.
pub fn render_scope_flags(template: &[String], path: &str) -> Vec<String> {
    template
        .iter()
        .map(|token| token.replace(MCP_SCOPE_PATH_PLACEHOLDER, path))
        .collect()
}

/// The `{path}` placeholder a [`ScopeInjection::ConfigFlag`] template substitutes.
pub const MCP_SCOPE_PATH_PLACEHOLDER: &str = "{path}";

/// The on-disk format of a CLI's MCP config file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFragmentFormat {
    Json,
    Yaml,
    Toml,
}

/// A CLI's structured-transcript capability, on top of the plain-text floor. An absent
/// arm means that mode has no structured capture; the `format` id selects the parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructuredTranscriptCapability {
    pub headless: Option<HeadlessStructured>,
    pub interactive: Option<InteractiveStructured>,
}

/// Headless structured output: `request_argv` is appended to the headless launch args to
/// switch stdout to a structured format, and `format` selects the parser for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadlessStructured {
    pub request_argv: Vec<String>,
    pub format: TranscriptFormat,
    /// The run's task rides argv as one literal `<flag><task>` token (a
    /// [`Descriptor::joined_prompt_flag`]) instead of stdin, for a CLI whose structured mode
    /// reads its query from an option. `None` keeps the task on stdin.
    pub argv_prompt_flag: Option<String>,
}

/// Interactive structured transcript: the CLI writes its own session log, which CodeSense
/// reads at session end (off the hot path, no PTY tee).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveStructured {
    /// Path pattern to the CLI's own session log, placeholders resolved trusted-side.
    pub session_log_glob: String,
    /// Argv that gives a freshly spawned CLI a CodeSense-minted session id, with one
    /// standalone `{session_id}` token.
    pub session_id_argv: Option<Vec<String>>,
    /// The format selecting the parser.
    pub format: TranscriptFormat,
}

/// A structured transcript's format, which selects its parser. It is recorded by its id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
pub enum TranscriptFormat {
    /// NDJSON of API messages streamed on stdout, ending in a `result` event that holds
    /// the run's own final result.
    #[serde(rename = "anthropic-stream-json-v1")]
    AnthropicStreamJson,
    /// JSONL of API messages in the CLI's own session log.
    #[serde(rename = "anthropic-session-jsonl-v1")]
    AnthropicSessionJsonl,
    /// The native, thread-bound session record.
    #[serde(rename = "codex-session-jsonl-v1")]
    CodexSessionJsonl,
    /// JSONL events on stdout from `run --format json`.
    #[serde(rename = "opencode-run-json-v1")]
    OpenCodeRunJson,
    /// JSONL events on stdout from `chat -Q --format stream-json`, ending in one `result`.
    #[serde(rename = "hermes-stream-json-v1")]
    HermesStreamJson,
}

pub(super) fn strings(tokens: &[&str]) -> Vec<String> {
    tokens.iter().map(|t| t.to_string()).collect()
}

/// The settings that point a CLI's own OpenTelemetry export at CodeSense's local receiver. `{endpoint}`
/// in an env value or argv token stands for the receiver's base URL. The CLI sends its usage records
/// there while a session runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelemetryExport {
    pub env: Vec<(String, String)>,
    pub argv: Vec<String>,
    /// Config overrides (`-c key=value` tokens) CodeSense sets by default, each under the config
    /// key it sets. A launch whose args already set that key keeps its own value: that is how a
    /// saved agent opts back in to what a default turns off.
    pub default_overrides: Vec<(String, Vec<String>)>,
    /// The `event.name` of the record that carries one model call's usage.
    pub usage_event_name: String,
    /// Its `event.kind` too, when the name alone also names records that are not usage.
    pub usage_event_kind: Option<String>,
    /// The attributes a usage record keeps when a machine stores it for the desktop: the ones the
    /// desktop's usage mapper reads, and the cache and reasoning splits a later price may need.
    /// Everything else the CLI attaches, the account's email and ids among it, is dropped.
    pub usage_attributes: Vec<String>,
}

impl TelemetryExport {
    /// Whether a record with this `event.name` and `event.kind` is this CLI's usage record.
    pub fn is_usage_record(&self, name: Option<&str>, kind: Option<&str>) -> bool {
        name == Some(self.usage_event_name.as_str())
            && self
                .usage_event_kind
                .as_deref()
                .is_none_or(|wanted| kind == Some(wanted))
    }

    /// The env pairs and argv tokens with every `{endpoint}` replaced by `endpoint`.
    pub fn resolved(&self, endpoint: &str) -> (Vec<(String, String)>, Vec<String>) {
        let env = self
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.replace("{endpoint}", endpoint)))
            .collect();
        let argv = self
            .argv
            .iter()
            .map(|t| t.replace("{endpoint}", endpoint))
            .collect();
        (env, argv)
    }
}

impl TranscriptFormat {
    /// Native formats are parsed by the adapter owning their wire shape.
    pub fn parse_native(self, raw: &str) -> Option<super::output::SessionTranscript> {
        match self {
            Self::CodexSessionJsonl => super::codex::parse_session_record(raw),
            Self::OpenCodeRunJson => super::opencode::parse_run_json(raw),
            Self::HermesStreamJson => super::hermes::parse_stream_json(raw),
            _ => None,
        }
    }

    /// [`parse_native`](Self::parse_native) over the record's lines one at a time. Only a format
    /// whose CLI keeps a session log is read this way; the others return `None`.
    pub fn parse_native_lines<I>(self, lines: I) -> Option<super::output::SessionTranscript>
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        match self {
            Self::CodexSessionJsonl => super::codex::parse_rollout_lines(lines),
            _ => None,
        }
    }

    /// What a native format's own record says the run concluded with, when it says.
    pub fn native_result_summary(self, raw: &str) -> Option<String> {
        match self {
            Self::OpenCodeRunJson => super::opencode::run_summary(raw),
            Self::HermesStreamJson => super::hermes::run_summary(raw),
            _ => None,
        }
    }

    /// Why a native format's own record says the run failed, whatever its exit code was.
    pub fn native_run_failure(self, raw: &str) -> Option<String> {
        match self {
            Self::OpenCodeRunJson => super::opencode::run_error(raw),
            Self::HermesStreamJson => super::hermes::run_failure(raw),
            _ => None,
        }
    }

    /// The routine's `result_text` comes only from completion signals: this format's process
    /// output (final prose, error text) stays transcript evidence and is never a result.
    pub fn result_is_signal_only(self) -> bool {
        matches!(self, Self::HermesStreamJson)
    }

    /// This native format must never fall back to a wildcard/mtime attribution.
    pub fn requires_captured_session(self) -> bool {
        matches!(self, Self::CodexSessionJsonl)
    }
}
