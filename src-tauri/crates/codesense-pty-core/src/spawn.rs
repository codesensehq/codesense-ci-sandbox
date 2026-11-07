// SPDX-License-Identifier: MIT
//! Spawn resolution, run by the host a session is spawned on. The app sends a
//! [`SpawnIntent`] — what to run, where, at what size, and what it adds to the
//! environment — and the host resolves it into the exact child command against
//! its own [`HostEnvironment`]: the user's shell (zsh, bash or fish,
//! see [`crate::shell`]), the host's CodeSense bin dir, the managed RC wrappers
//! written on the host's disk, and the shell-integration and starship resources
//! shipped with the host. The in-process host and the session-server sidecar
//! run this same code, so a local spawn resolves identically on either.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use log::warn;
use portable_pty::{CommandBuilder, PtySize};
use serde::{Deserialize, Serialize};

use crate::runner::PtySpec;
use crate::shell::{
    is_executable_file, passwd_login_shell, resolve_shell_from, ShellKind, UserShell,
};

/// What the app asks a host to spawn. Everything here is the app's to decide;
/// everything that depends on where the child runs is resolved by the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnIntent {
    /// A launch marker carried independently with the spawn. With a reverse-MCP
    /// broker, every marker requires live session MCP provisioning. Without one,
    /// Claude/Codex may launch locally only with a nonempty direct argv; Shell
    /// still requires provisioning. None preserves legacy shell wire behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_launch: Option<codesense_host_proto::agent_cli::NativeLaunch>,
    /// `None`: the user's interactive login shell. `Some`: the login shell
    /// running this command line (`-c`) — zsh for a fish user, as the command
    /// is POSIX shell.
    pub command: Option<String>,
    /// An argv to spawn directly as PID-1 of the PTY — no login
    /// shell wrapper, no `.zshrc`/`.zprofile`, no shell-integration. When set,
    /// overrides [`Self::command`] and makes the spawn non-interactive. Used
    /// to run a managed agent CLI (claude, codex) as PID-1 so Ctrl+C routes
    /// to the agent in-process and the session ends on agent exit — the
    /// classic REPL contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argv: Option<Vec<String>>,
    /// The user's Settings → Terminal shell, when set; else the host detects
    /// the user's shell.
    pub shell: Option<String>,
    /// The working directory the app asked for; an empty or missing one is
    /// not entered (see [`SpawnIntent::spawn_dir`]).
    pub cwd: String,
    pub cols: u16,
    pub rows: u16,
    /// Shell integration (OSC 7/133 and the inline prompt), for an
    /// interactive shell.
    pub shell_integration: bool,
    /// The starship prompt preset; `plain` (or none) for the user's own
    /// prompt.
    pub starship_preset: Option<String>,
    /// The CodeSense HTTP API port as the child reaches it
    /// (`CODESENSE_API_PORT`); `None` when the API is not bound.
    pub api_port: Option<u16>,
    /// Inherited variables the child must not see.
    pub env_remove: Vec<String>,
    /// Variables set last, over everything else: the consumed one-use
    /// account and MCP-scope bindings.
    pub env: Vec<(String, String)>,
    /// A saved Agent's own variables, set first, under everything else: every
    /// variable CodeSense sets for the spawn, the strip of guarded ones
    /// included, wins over one of the same name, and the
    /// [`CODESENSE_LAUNCH_ENV`] names are never taken from it. A host before
    /// `AgentEnvV1` drops this field, so a client sends it only to a host that
    /// advertises that capability.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_env: Vec<(String, String)>,
    /// The user's login-shell PATH, for an argv-direct spawn whose argv[0] is
    /// absolute; the host's own PATH when absent, empty or argv[0] is bare. CodeSense's bin dir still goes first and a
    /// saved Agent's `env` still cannot set PATH. A host before this field
    /// ignores it and keeps its own PATH.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_path: Option<String>,
}

/// The variables that are always CodeSense's on a launch, whether or not it sets them on this
/// one: a saved Agent's own `env` never supplies them.
pub const CODESENSE_LAUNCH_ENV: [&str; 7] = [
    "CODESENSE_TAB_SESSION_ID",
    "CODESENSE_API_PORT",
    "CODESENSE_MCP_CONFIG",
    crate::activity::HOST_ENV,
    crate::activity::SOCKET_ENV,
    crate::activity::TOKEN_ENV,
    crate::activity::RELEASE_QUALIFIED_ENV,
];

/// Whether a saved Agent's `env` may supply `name`: any name but [`CODESENSE_LAUNCH_ENV`].
pub fn agent_may_set(name: &str) -> bool {
    !CODESENSE_LAUNCH_ENV.contains(&name) && !telemetry_owned_env(name)
}

/// Export configuration is owned by the engine, including on direct YAML edits.
pub fn telemetry_owned_env(name: &str) -> bool {
    name.starts_with("OTEL_")
        || matches!(
            name,
            "CLAUDE_CODE_ENABLE_TELEMETRY"
                | "DISABLE_TELEMETRY"
                | "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"
                | "DO_NOT_TRACK"
        )
}

/// `program` run with the host's variables and, under everything the spawn sets later, a saved
/// Agent's own.
fn agent_command(
    host: &HostEnvironment,
    program: &str,
    agent_env: &[(String, String)],
) -> CommandBuilder {
    let mut cmd = host.command(program);
    for (key, value) in agent_env.iter().filter(|(key, _)| agent_may_set(key)) {
        cmd.env(key, value);
    }
    cmd
}

/// The environment variables a child inherits from its host. Never `Debug`:
/// it holds every inherited secret.
#[derive(Clone)]
enum Vars {
    /// This process's own, as they are at each spawn.
    Process,
    /// Exactly these.
    Given(HashMap<String, String>),
}

/// What a host resolves a [`SpawnIntent`] against: its CodeSense home (the
/// bin dir and the managed shell dir live under it), the directory of the
/// resources it ships, and the environment variables its children inherit.
#[derive(Clone)]
pub struct HostEnvironment {
    codesense_home: Option<PathBuf>,
    resources: Option<PathBuf>,
    vars: Vars,
}

impl HostEnvironment {
    /// This process's environment, with its CodeSense home and the directory
    /// holding the resources it ships (the shell-integration script, the
    /// starship binary and its presets).
    pub fn of_process(codesense_home: Option<PathBuf>, resources: Option<PathBuf>) -> Self {
        Self {
            codesense_home,
            resources,
            vars: Vars::Process,
        }
    }

    /// An environment of exactly `vars`.
    pub fn with_vars(
        codesense_home: Option<PathBuf>,
        resources: Option<PathBuf>,
        vars: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        Self {
            codesense_home,
            resources,
            vars: Vars::Given(vars.into_iter().collect()),
        }
    }

    /// The directory of the resources this host ships, when it has one.
    pub fn resources(&self) -> Option<&Path> {
        self.resources.as_deref()
    }

    fn var(&self, name: &str) -> Option<String> {
        match &self.vars {
            Vars::Process => std::env::var(name).ok(),
            Vars::Given(vars) => vars.get(name).cloned(),
        }
    }

    /// A command running `program` with this host's variables.
    fn command(&self, program: &str) -> CommandBuilder {
        let mut cmd = CommandBuilder::new(program);
        if let Vars::Given(vars) = &self.vars {
            cmd.env_clear();
            for (key, value) in vars {
                cmd.env(key, value);
            }
        }
        cmd
    }

    /// The shell a session runs: the user's override when usable, else this
    /// host's `$SHELL`, the account's login shell, then zsh.
    fn shell(&self, override_shell: Option<&str>) -> UserShell {
        resolve_shell_from(
            override_shell,
            self.var("SHELL").as_deref(),
            passwd_login_shell,
            is_executable_file,
        )
    }

    /// The shipped resource at `rel`, when it exists.
    fn resource(&self, rel: &str) -> Option<PathBuf> {
        self.resources
            .as_ref()
            .map(|dir| dir.join(rel))
            .filter(|path| path.exists())
    }
}

/// Resolve the dir whose RC files the managed `ZDOTDIR` wrapper should source.
///
/// The wrapper writes `<shell_dir>/.zshenv` = `source "<base>/.zshenv"`, then
/// sets `ZDOTDIR=<shell_dir>` for the child. So a nested codesense shell starts
/// with `ZDOTDIR` ALREADY equal to `<shell_dir>`. If we captured that as the
/// base, the generated `.zshenv` would `source` itself → infinite recursion
/// (zsh aborts with "job table full or recursion limit exceeded"). Guard: when
/// the inherited `ZDOTDIR` is our own managed dir (or unset), fall back to the
/// real `$HOME` dotfiles instead.
fn resolve_source_zdotdir(
    current: Option<String>,
    shell_dir: &Path,
    home: Option<String>,
) -> String {
    match current {
        Some(z) if Path::new(&z) != shell_dir => z,
        _ => home.unwrap_or_default(),
    }
}

/// Of the caller-named vars to strip from a guarded child
/// session, return those that were actually inherited (present and non-empty in
/// the lookup) — the host logs these so a stray `ANTHROPIC_API_KEY` in its
/// environment is surfaced rather than silently dropped.
fn inherited_env_to_warn(names: &[String], lookup: impl Fn(&str) -> Option<String>) -> Vec<String> {
    names
        .iter()
        .filter(|n| lookup(n).is_some_and(|v| !v.is_empty()))
        .cloned()
        .collect()
}

/// The working directory a spawn enters. An empty
/// or non-existent `cwd` is `None`, so the child is NOT `chdir`ed — a login
/// shell cannot `chdir("")`, so setting an empty cwd silently lands it in
/// `$HOME`, which is always wrong for a story dev terminal. The child then
/// inherits the host's own dir: the minimal, non-regressing floor, never an
/// implicit `$HOME`.
fn spawn_dir(cwd: &str) -> Option<&str> {
    if cwd.is_empty() || !Path::new(cwd).is_dir() {
        None
    } else {
        Some(cwd)
    }
}

/// The host-resolved inputs of one spawn: every value that depends on where
/// the child runs.
struct ResolvedSpawnResources {
    /// The user's shell, which runs the session.
    shell: UserShell,
    /// `<codesense home>/bin` when it exists — prepended to `PATH` and
    /// exported as `__CODESENSE_BIN` for the interactive-shell RC wrapper.
    codesense_bin: Option<PathBuf>,
    /// The managed RC-wrapper dir for an interactive shell:
    /// `<codesense home>/shell/zsh` (the `ZDOTDIR`) or `.../shell/bash` (the
    /// `--rcfile`). fish needs none (`--init-command`).
    shell_dir: Option<PathBuf>,
    /// The user's original `ZDOTDIR` the managed zsh wrappers source; set
    /// whenever `shell_dir` is for a zsh session.
    source_zdotdir: Option<String>,
    /// The shipped shell-init script (`__CODESENSE_SHELL_INIT`) when shell
    /// integration is active.
    shell_init: Option<PathBuf>,
    /// `(starship binary, starship config)` when the prompt uses it.
    starship: Option<(PathBuf, PathBuf)>,
    /// The `PATH` the assembly builds on: the intent's login PATH for an
    /// argv-direct spawn whose argv[0] is absolute, else the inherited one.
    base_path: String,
}

fn resolve_spawn_resources(host: &HostEnvironment, intent: &SpawnIntent) -> ResolvedSpawnResources {
    let osc_enabled = intent.osc_enabled();
    let shell = host.shell(intent.shell.as_deref());
    // Argv-direct spawn bypasses the shell wrapper entirely; the
    // resolved shell is reported for logging only and no RC wrappers / init
    // scripts / starship resources are prepared.
    let argv_direct = intent.argv.is_some();
    let codesense_bin = host
        .codesense_home
        .as_ref()
        .map(|home| home.join("bin"))
        .filter(|bin| bin.exists());

    let mut shell_dir = None;
    let mut source_zdotdir = None;
    if let (true, false, Some(home)) = (intent.is_interactive(), argv_direct, &host.codesense_home)
    {
        match shell.kind {
            ShellKind::Zsh => {
                let dir = home.join("shell").join("zsh");
                source_zdotdir = Some(resolve_source_zdotdir(
                    host.var("ZDOTDIR"),
                    &dir,
                    host.var("HOME"),
                ));
                shell_dir = Some(dir);
            }
            ShellKind::Bash => shell_dir = Some(home.join("shell").join("bash")),
            ShellKind::Fish => {}
        }
    }

    let shell_init = if osc_enabled && !argv_direct {
        host.resource(&format!(
            "shell-integration/codesense-shell-init.{}",
            shell.kind.init_script_ext()
        ))
    } else {
        None
    };

    let mut starship = None;
    if let Some(preset) = intent
        .starship_preset
        .as_deref()
        .filter(|preset| osc_enabled && !argv_direct && *preset != "plain")
    {
        let config = format!("starship-{preset}.toml");
        if let (Some(bin), Some(config)) = (host.resource("starship"), host.resource(&config)) {
            starship = Some((bin, config));
        }
    }

    // An agent launched without a shell gets the user's login-shell PATH, so
    // its hooks and MCP servers find Homebrew/nvm tools. Only an absolute
    // argv[0] takes it: a bare one resolves against the child's PATH, and the
    // login PATH must never change which agent binary runs (#2167).
    let absolute_argv0 = intent
        .argv
        .as_ref()
        .and_then(|argv| argv.first())
        .is_some_and(|program| Path::new(program).is_absolute());
    let base_path = match intent.login_path.as_deref() {
        Some(login_path) if absolute_argv0 && !login_path.is_empty() => login_path.to_string(),
        _ => host.var("PATH").unwrap_or_default(),
    };

    ResolvedSpawnResources {
        shell,
        codesense_bin,
        shell_dir,
        source_zdotdir,
        shell_init,
        starship,
        base_path,
    }
}

/// Write the managed `ZDOTDIR` RC wrappers into `shell_dir`, each sourcing the
/// user's own file from `original_zdotdir`. Best-effort: a wrapper that cannot
/// be written leaves the user's own RC files unsourced, never a failed spawn.
fn write_zsh_wrappers(shell_dir: &Path, original_zdotdir: &str) {
    let _ = std::fs::create_dir_all(shell_dir);
    let source = |name: &str| {
        format!("[ -f \"{original_zdotdir}/{name}\" ] && source \"{original_zdotdir}/{name}\"\n")
    };
    let wrappers = [
        (".zshenv", source(".zshenv")),
        (".zprofile", source(".zprofile")),
        (
            ".zshrc",
            format!(
                "{}[ -n \"$__CODESENSE_BIN\" ] && export PATH=\"$__CODESENSE_BIN:$PATH\"\n\
                 [ -n \"$__CODESENSE_SHELL_INIT\" ] && source \"$__CODESENSE_SHELL_INIT\"\n\
                 setopt HIST_IGNORE_SPACE\n",
                source(".zshrc")
            ),
        ),
        (".zlogin", source(".zlogin")),
    ];
    for (name, content) in wrappers {
        let _ = write_if_changed(&shell_dir.join(name), &content);
    }
}

/// Write `content` to `path` only when it differs, through a uniquely named
/// temp file renamed into place, so a shell starting meanwhile (another
/// spawn, on this host or the other) never reads a half-written file.
fn write_if_changed(path: &Path, content: &str) -> std::io::Result<()> {
    if std::fs::read_to_string(path).is_ok_and(|current| current == content) {
        return Ok(());
    }
    static STAGED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = STAGED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = path
        .file_name()
        .map_or_else(Default::default, |n| n.to_string_lossy());
    let staged = path.with_file_name(format!("{name}.{}.{n}.tmp", std::process::id()));
    let written = std::fs::write(&staged, content).and_then(|()| std::fs::rename(&staged, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    written
}

/// Make sure `<dir>/bashrc` holds [`BASH_RC_WRAPPER`]. `None` when
/// it can't be written, so the caller falls back to a plain login bash instead
/// of pointing at a missing rcfile.
fn ensure_bash_rc_wrapper(dir: &Path) -> Option<PathBuf> {
    let path = dir.join("bashrc");
    let written =
        std::fs::create_dir_all(dir).and_then(|()| write_if_changed(&path, BASH_RC_WRAPPER));
    match written {
        Ok(()) => Some(path),
        Err(e) => {
            warn!(target: "pty", "spawn_pty: bash rc wrapper not written to {}: {e}", path.display());
            None
        }
    }
}

/// The managed bash `--rcfile`. An rcfile replaces bash's login-file
/// reading, so it sources the same files a login shell would (`/etc/profile`, then
/// the first of `~/.bash_profile`/`~/.bash_login`/`~/.profile`), falling back to
/// `~/.bashrc` for users who keep everything there, then applies CodeSense's
/// PATH entry and shell integration — the bash analogue of the zsh `ZDOTDIR`
/// wrappers.
const BASH_RC_WRAPPER: &str = r#"# Managed by CodeSense. Sources your login files, then CodeSense's shell integration.
[ -f /etc/profile ] && . /etc/profile
if [ -f "$HOME/.bash_profile" ]; then . "$HOME/.bash_profile"
elif [ -f "$HOME/.bash_login" ]; then . "$HOME/.bash_login"
elif [ -f "$HOME/.profile" ]; then . "$HOME/.profile"
elif [ -f "$HOME/.bashrc" ]; then . "$HOME/.bashrc"
fi
[ -n "$__CODESENSE_BIN" ] && export PATH="$__CODESENSE_BIN:$PATH"
case ":$HISTCONTROL:" in
  *:ignorespace:*|*:ignoreboth:*) ;;
  *) HISTCONTROL="${HISTCONTROL:+$HISTCONTROL:}ignorespace" ;;
esac
[ -n "$__CODESENSE_SHELL_INIT" ] && . "$__CODESENSE_SHELL_INIT"
"#;

/// Fish runs its own config first, then this: CodeSense's PATH entry
/// and shell integration, each only when the spawn provided it.
const FISH_INIT_COMMAND: &str = "set -q __CODESENSE_BIN; and set -gx PATH $__CODESENSE_BIN $PATH; \
set -q __CODESENSE_SHELL_INIT; and source $__CODESENSE_SHELL_INIT";

/// Argv for the user's shell, with this host's variables.
///
/// - Interactive: zsh `-l` (its RC wrappers come in through `ZDOTDIR`); bash
///   `--rcfile <wrapper> -i` when the wrapper was written, else `-l`; fish `-l`
///   with `--init-command`.
/// - `-c <command>`: the user's shell as a login shell when it runs POSIX-style
///   commands (zsh, bash); fish users' commands run under the fallback zsh,
///   since CodeSense's command strings are POSIX shell.
///
/// A saved Agent's `agent_env` goes under `SHELL`, which is always the shell's.
fn shell_command(
    host: &HostEnvironment,
    shell: &UserShell,
    command: Option<&str>,
    bash_rcfile: Option<&Path>,
    agent_env: &[(String, String)],
) -> CommandBuilder {
    let mut cmd = match command {
        Some(command) => {
            let program = if shell.kind.runs_posix_commands() {
                shell.path.clone()
            } else {
                UserShell::fallback().path
            };
            let mut cmd = agent_command(host, &program.to_string_lossy(), agent_env);
            cmd.args(["-l", "-c", command]);
            cmd
        }
        None => {
            let mut cmd = agent_command(host, &shell.path.to_string_lossy(), agent_env);
            match (shell.kind, bash_rcfile) {
                (ShellKind::Bash, Some(rcfile)) => {
                    cmd.arg("--rcfile");
                    cmd.arg(rcfile);
                    cmd.arg("-i");
                }
                (ShellKind::Fish, _) => {
                    cmd.args(["-l", "--init-command", FISH_INIT_COMMAND]);
                }
                _ => {
                    cmd.arg("-l");
                }
            }
            cmd
        }
    };
    cmd.env("SHELL", shell.path.to_string_lossy().as_ref());
    cmd
}

/// Assemble the child command — argv, cwd, and the full managed environment
/// (`TERM`/`LANG`/PATH, the `ZDOTDIR` RC wrappers, shell-init, starship, and
/// finally the intent's own variables) — from the intent and its resolved
/// resources.
fn build_spawn_command(
    session_id: &str,
    intent: &SpawnIntent,
    resolved: &ResolvedSpawnResources,
    host: &HostEnvironment,
) -> CommandBuilder {
    let osc_enabled = intent.osc_enabled();
    let preset = intent.starship_preset.as_deref();

    let bash_rcfile = match (&resolved.shell_dir, resolved.shell.kind) {
        (Some(dir), ShellKind::Bash) => ensure_bash_rc_wrapper(dir),
        _ => None,
    };
    let mut cmd = match intent.argv.as_deref() {
        // Argv-direct — spawn the first arg as PID-1 of the PTY,
        // no shell wrapper, no RC wrappers, no shell-integration. Used by
        // managed agent launches so the agent CLI owns the PTY from the
        // start and Ctrl+C routes to the agent in-process.
        Some([program, args @ ..]) => {
            let mut cmd = agent_command(host, program, &intent.agent_env);
            cmd.args(args);
            cmd
        }
        // Empty argv falls through to the interactive shell: a mis-set empty
        // vec cannot leave the spawn with no command.
        _ => shell_command(
            host,
            &resolved.shell,
            intent.command.as_deref(),
            bash_rcfile.as_deref(),
            &intent.agent_env,
        ),
    };
    match spawn_dir(&intent.cwd) {
        Some(dir) => {
            cmd.cwd(dir);
        }
        None => {
            warn!(target: "pty", "spawn_pty: empty/nonexistent cwd {:?} — inheriting process dir, not chdir-ing to $HOME", intent.cwd);
        }
    }
    cmd.env("TERM", "xterm-256color");
    cmd.env("LANG", "en_US.UTF-8");
    cmd.env("COLUMNS", intent.cols.to_string());
    cmd.env("LINES", intent.rows.to_string());

    cmd.env("CODESENSE_TAB_SESSION_ID", session_id);
    if let Some(port) = intent.api_port {
        cmd.env("CODESENSE_API_PORT", port.to_string());
    }

    let mut path = resolved.base_path.clone();
    if let Some(codesense_bin) = &resolved.codesense_bin {
        cmd.env("__CODESENSE_BIN", codesense_bin.to_string_lossy().as_ref());
        path = format!("{}:{}", codesense_bin.display(), path);
    }

    if let (ShellKind::Zsh, Some(shell_dir), Some(original_zdotdir)) = (
        resolved.shell.kind,
        &resolved.shell_dir,
        &resolved.source_zdotdir,
    ) {
        write_zsh_wrappers(shell_dir, original_zdotdir);
        cmd.env("ZDOTDIR", shell_dir.to_string_lossy().as_ref());
    }

    if osc_enabled {
        match &resolved.shell_init {
            Some(path) => {
                cmd.env("__CODESENSE_SHELL_INIT", path.to_string_lossy().as_ref());
            }
            None => {
                warn!(target: "pty", "spawn_pty: shell-init script not found");
            }
        }
        if preset == Some("plain") {
            cmd.env("__CODESENSE_PLAIN", "1");
        }
    }

    if osc_enabled && preset != Some("plain") {
        match &resolved.starship {
            Some((bin, config)) => {
                cmd.env("STARSHIP_CONFIG", config.to_string_lossy().as_ref());
                if let Some(bin_dir) = bin.parent() {
                    path = format!("{}:{}", bin_dir.display(), path);
                }
            }
            None => {
                warn!(target: "pty", "spawn_pty: starship resources not found");
            }
        }
    }

    cmd.env("PATH", &path);

    for present in inherited_env_to_warn(&intent.env_remove, |k| host.var(k)) {
        warn!(target: "pty", "spawn_pty: stripping inherited env var {present} from guarded child session");
    }
    for name in &intent.env_remove {
        cmd.env_remove(name);
    }
    for (key, value) in &intent.env {
        cmd.env(key, value);
    }

    cmd
}

impl SpawnIntent {
    /// This intent running the saved Agent `startup` carries: its own variables become the
    /// spawn's [`Self::agent_env`]. No startup leaves the intent as it is.
    pub fn with_agent_startup(
        mut self,
        startup: Option<&codesense_host_proto::reverse_mcp::AgentStartup>,
    ) -> Self {
        if let Some(startup) = startup {
            self.agent_env = startup.env.clone().into_iter().collect();
        }
        self
    }

    /// Whether this is the user's interactive shell (no command to run).
    /// An [`Self::argv`]-direct spawn is NOT interactive even when
    /// `command` is `None` — the spawn runs the given program as PID-1 of the
    /// PTY, not the user's login shell.
    pub fn is_interactive(&self) -> bool {
        self.command.is_none() && self.argv.is_none()
    }

    /// Whether the session's output carries shell integration (OSC 7/133):
    /// only an interactive shell gets the injected init.
    pub fn osc_enabled(&self) -> bool {
        self.shell_integration
            && self.is_interactive()
            && self.remote_launch.is_none_or(|launch| launch.is_shell())
    }

    /// The directory the child starts in, when the intent names a usable one
    /// on this machine — the session's stable home for liveness.
    pub fn spawn_dir(&self) -> Option<PathBuf> {
        spawn_dir(&self.cwd).map(PathBuf::from)
    }

    /// Resolve the intent into the exact spawn of session `session_id` on the
    /// host `host`. Writes the managed RC wrappers for an interactive shell,
    /// so it may block on the host's disk.
    pub fn resolve(&self, session_id: &str, host: &HostEnvironment) -> PtySpec {
        let resolved = resolve_spawn_resources(host, self);
        PtySpec {
            command: build_spawn_command(session_id, self, &resolved, host),
            size: PtySize {
                rows: self.rows,
                cols: self.cols,
                pixel_width: 0,
                pixel_height: 0,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osc::{OscEvent, OscParser};
    use portable_pty::native_pty_system;
    use std::io::{Read, Write};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    /// A host rooted in a temp dir: a CodeSense home with a bin dir, shipped
    /// resources with the shell-init scripts, starship and its presets, and
    /// the variables given.
    struct Fixture {
        _tmp: tempfile::TempDir,
        home: PathBuf,
        resources: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let home = tmp.path().join("codesense");
            let resources = tmp.path().join("resources");
            std::fs::create_dir_all(home.join("bin")).unwrap();
            std::fs::create_dir_all(resources.join("shell-integration")).unwrap();
            for file in [
                "shell-integration/codesense-shell-init.zsh",
                "shell-integration/codesense-shell-init.bash",
                "starship",
                "starship-compact.toml",
                "starship-full.toml",
            ] {
                std::fs::write(resources.join(file), "").unwrap();
            }
            Self {
                _tmp: tmp,
                home,
                resources,
            }
        }

        fn host(&self, vars: &[(&str, &str)]) -> HostEnvironment {
            HostEnvironment::with_vars(
                Some(self.home.clone()),
                Some(self.resources.clone()),
                given(vars),
            )
        }
    }

    fn given(vars: &[(&str, &str)]) -> Vec<(String, String)> {
        vars.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn interactive(cwd: &str) -> SpawnIntent {
        SpawnIntent {
            remote_launch: Default::default(),
            command: None,
            argv: None,
            shell: None,
            cwd: cwd.to_string(),
            cols: 120,
            rows: 40,
            shell_integration: true,
            starship_preset: Some("compact".to_string()),
            api_port: Some(8737),
            env_remove: Vec::new(),
            env: Vec::new(),
            agent_env: Vec::new(),
            login_path: None,
        }
    }

    #[test]
    fn required_remote_launch_survives_wire_while_legacy_shell_omits_it() {
        use codesense_host_proto::agent_cli::{AgentCli, NativeLaunch};
        let mut intent = interactive("/tmp");
        let legacy = serde_json::to_value(&intent).unwrap();
        assert!(legacy.get("remote_launch").is_none());
        assert_eq!(
            serde_json::from_value::<SpawnIntent>(legacy)
                .unwrap()
                .remote_launch,
            None
        );
        for launch in [
            NativeLaunch::Shell,
            NativeLaunch::Cli(AgentCli::Claude),
            NativeLaunch::Cli(AgentCli::Codex),
        ] {
            intent.remote_launch = Some(launch);
            let restored: SpawnIntent =
                serde_json::from_value(serde_json::to_value(&intent).unwrap()).unwrap();
            assert_eq!(restored.remote_launch, Some(launch));
            assert_eq!(restored.osc_enabled(), launch.is_shell());
        }
    }

    fn argv(spec: &PtySpec) -> Vec<String> {
        spec.command
            .get_argv()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn env(spec: &PtySpec, key: &str) -> Option<String> {
        spec.command
            .get_env(key)
            .map(|value| value.to_string_lossy().into_owned())
    }

    /// The host resolves the shell, the bin dir, the managed `ZDOTDIR` and the
    /// shipped shell-init and starship from its own environment, and the
    /// result is byte-for-byte the spawn the app used to assemble for the same
    /// inputs: `<shell> -l`, the same variables, the same RC wrappers.
    #[test]
    fn the_host_resolves_the_spawn_from_its_own_environment() {
        let fx = Fixture::new();
        let host = fx.host(&[
            ("SHELL", "/bin/zsh"),
            ("PATH", "/usr/bin:/bin"),
            ("HOME", "/Users/me"),
        ]);
        let cwd = fx.resources.to_string_lossy().into_owned();
        let spec = interactive(&cwd).resolve("tab-1", &host);

        assert_eq!(argv(&spec), ["/bin/zsh", "-l"]);
        assert_eq!(
            spec.command
                .get_cwd()
                .map(|c| c.to_string_lossy().into_owned()),
            Some(cwd)
        );
        assert_eq!((spec.size.cols, spec.size.rows), (120, 40));
        let bin = fx.home.join("bin");
        let shell_dir = fx.home.join("shell").join("zsh");
        let expected = [
            ("TERM", "xterm-256color".to_string()),
            ("LANG", "en_US.UTF-8".to_string()),
            ("COLUMNS", "120".to_string()),
            ("LINES", "40".to_string()),
            ("SHELL", "/bin/zsh".to_string()),
            ("CODESENSE_TAB_SESSION_ID", "tab-1".to_string()),
            ("CODESENSE_API_PORT", "8737".to_string()),
            ("__CODESENSE_BIN", bin.display().to_string()),
            ("ZDOTDIR", shell_dir.display().to_string()),
            (
                "__CODESENSE_SHELL_INIT",
                fx.resources
                    .join("shell-integration/codesense-shell-init.zsh")
                    .display()
                    .to_string(),
            ),
            (
                "STARSHIP_CONFIG",
                fx.resources
                    .join("starship-compact.toml")
                    .display()
                    .to_string(),
            ),
            (
                "PATH",
                format!("{}:{}:/usr/bin:/bin", fx.resources.display(), bin.display()),
            ),
        ];
        for (key, value) in expected {
            assert_eq!(env(&spec, key).as_deref(), Some(value.as_str()), "{key}");
        }
        assert_eq!(env(&spec, "__CODESENSE_PLAIN"), None);
        assert_eq!(
            std::fs::read_to_string(shell_dir.join(".zshenv")).unwrap(),
            "[ -f \"/Users/me/.zshenv\" ] && source \"/Users/me/.zshenv\"\n"
        );
        assert_eq!(
            std::fs::read_to_string(shell_dir.join(".zshrc")).unwrap(),
            "[ -f \"/Users/me/.zshrc\" ] && source \"/Users/me/.zshrc\"\n\
             [ -n \"$__CODESENSE_BIN\" ] && export PATH=\"$__CODESENSE_BIN:$PATH\"\n\
             [ -n \"$__CODESENSE_SHELL_INIT\" ] && source \"$__CODESENSE_SHELL_INIT\"\n\
             setopt HIST_IGNORE_SPACE\n"
        );
        for rc in [".zprofile", ".zlogin"] {
            assert!(shell_dir.join(rc).exists(), "{rc}");
        }
    }

    /// The Settings override is the host's to honour when it names a shell
    /// there; a command runs under the host's own shell as a login shell,
    /// and with nothing shipped there is no shell-init or starship.
    #[test]
    fn the_override_and_the_hosts_own_resources_decide() {
        let fx = Fixture::new();
        let host = fx.host(&[("SHELL", "/bin/zsh"), ("PATH", "/usr/bin:/bin")]);
        let bash = SpawnIntent {
            shell: Some("/bin/bash".to_string()),
            ..interactive("")
        }
        .resolve("tab-2", &host);
        let rcfile = fx.home.join("shell/bash/bashrc");
        assert_eq!(
            argv(&bash),
            ["/bin/bash", "--rcfile", rcfile.to_str().unwrap(), "-i"]
        );
        assert_eq!(std::fs::read_to_string(&rcfile).unwrap(), BASH_RC_WRAPPER);
        assert_eq!(env(&bash, "ZDOTDIR"), None);
        assert_eq!(
            env(&bash, "__CODESENSE_SHELL_INIT"),
            Some(
                fx.resources
                    .join("shell-integration/codesense-shell-init.bash")
                    .display()
                    .to_string()
            )
        );

        let bare = HostEnvironment::with_vars(
            None,
            None,
            given(&[("SHELL", "/bin/bash"), ("PATH", "/opt/bin")]),
        );
        let command = SpawnIntent {
            remote_launch: Default::default(),
            command: Some("make test".to_string()),
            ..interactive("")
        }
        .resolve("tab-3", &bare);
        assert_eq!(argv(&command), ["/bin/bash", "-l", "-c", "make test"]);
        assert_eq!(
            command.command.get_cwd(),
            None,
            "an empty cwd is not entered"
        );
        assert_eq!(env(&command, "PATH").as_deref(), Some("/opt/bin"));
        for absent in [
            "__CODESENSE_BIN",
            "ZDOTDIR",
            "__CODESENSE_SHELL_INIT",
            "STARSHIP_CONFIG",
        ] {
            assert_eq!(env(&command, absent), None, "{absent}");
        }
    }

    /// The `plain` preset turns starship off and says so; env removals strip
    /// inherited variables; the intent's own variables are set last, over
    /// everything else.
    #[test]
    fn plain_prompt_removals_and_the_intents_own_variables() {
        let fx = Fixture::new();
        let host = fx.host(&[("SHELL", "/bin/zsh"), ("ANTHROPIC_API_KEY", "sk-leak")]);
        let intent = SpawnIntent {
            starship_preset: Some("plain".to_string()),
            env_remove: vec!["ANTHROPIC_API_KEY".to_string()],
            env: vec![
                ("CLAUDE_CONFIG_DIR".to_string(), "/acct".to_string()),
                ("TERM".to_string(), "dumb".to_string()),
            ],
            ..interactive("")
        };
        let spec = intent.resolve("tab-4", &host);
        assert_eq!(env(&spec, "__CODESENSE_PLAIN").as_deref(), Some("1"));
        assert_eq!(env(&spec, "STARSHIP_CONFIG"), None);
        assert_eq!(env(&spec, "ANTHROPIC_API_KEY"), None);
        assert_eq!(env(&spec, "CLAUDE_CONFIG_DIR").as_deref(), Some("/acct"));
        assert_eq!(env(&spec, "TERM").as_deref(), Some("dumb"));
    }

    /// A non-interactive command gets no shell integration and no managed
    /// RC wrappers, whatever the flag says.
    #[test]
    fn a_command_gets_no_shell_integration() {
        let fx = Fixture::new();
        let host = fx.host(&[("SHELL", "/bin/zsh")]);
        let intent = SpawnIntent {
            remote_launch: Default::default(),
            command: Some("true".to_string()),
            ..interactive("")
        };
        assert!(!intent.osc_enabled());
        let spec = intent.resolve("tab-5", &host);
        assert_eq!(env(&spec, "ZDOTDIR"), None);
        assert_eq!(env(&spec, "__CODESENSE_SHELL_INIT"), None);
        assert!(!fx.home.join("shell").exists());
    }

    /// Bash falls back to a login shell when its rc wrapper cannot
    /// be written, rather than pointing at a missing rcfile.
    #[test]
    fn bash_falls_back_to_a_login_shell_when_the_rc_wrapper_cannot_be_written() {
        let tmp = tempfile::tempdir().unwrap();
        // A regular file where the CodeSense home should be: no dir under it.
        let blocked = tmp.path().join("blocked");
        std::fs::write(&blocked, "").unwrap();
        let host =
            HostEnvironment::with_vars(Some(blocked), None, given(&[("SHELL", "/bin/bash")]));
        let spec = interactive("").resolve("s", &host);
        assert_eq!(argv(&spec), ["/bin/bash", "-l"]);
    }

    /// Managed RC files are rewritten only when they differ, via a temp file
    /// renamed into place, and leave no temp file behind.
    #[test]
    fn rc_wrappers_are_rewritten_only_when_they_differ() {
        let tmp = tempfile::tempdir().unwrap();
        let path = ensure_bash_rc_wrapper(tmp.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), BASH_RC_WRAPPER);
        std::fs::write(&path, "stale").unwrap();
        ensure_bash_rc_wrapper(tmp.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), BASH_RC_WRAPPER);

        let zsh = tmp.path().join("zsh");
        write_zsh_wrappers(&zsh, "/Users/me");
        let stamp = std::fs::metadata(zsh.join(".zshrc"))
            .unwrap()
            .modified()
            .unwrap();
        std::thread::sleep(Duration::from_millis(20));
        write_zsh_wrappers(&zsh, "/Users/me");
        assert_eq!(
            std::fs::metadata(zsh.join(".zshrc"))
                .unwrap()
                .modified()
                .unwrap(),
            stamp,
            "an unchanged wrapper is left alone"
        );
        for dir in [tmp.path(), zsh.as_path()] {
            let leftovers = std::fs::read_dir(dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
                .count();
            assert_eq!(leftovers, 0);
        }
    }

    fn find_fish() -> Option<String> {
        [
            "/opt/homebrew/bin/fish",
            "/usr/local/bin/fish",
            "/usr/bin/fish",
        ]
        .into_iter()
        .find(|p| Path::new(p).exists())
        .map(str::to_string)
    }

    /// Argv per shell.
    #[test]
    fn argv_per_shell() {
        let fx = Fixture::new();
        let host = fx.host(&[("SHELL", "/bin/zsh")]);
        let build = |shell: &str, command: Option<&str>| {
            SpawnIntent {
                shell: Some(shell.to_string()),
                command: command.map(str::to_string),
                ..interactive("")
            }
            .resolve("s", &host)
        };
        assert_eq!(argv(&build("/bin/zsh", None)), ["/bin/zsh", "-l"]);
        let bash_c = build("/bin/bash", Some("echo hi"));
        assert_eq!(argv(&bash_c), ["/bin/bash", "-l", "-c", "echo hi"]);
        assert_eq!(env(&bash_c, "SHELL").as_deref(), Some("/bin/bash"));

        // fish, installed or not.
        let fish = UserShell {
            path: PathBuf::from("/opt/homebrew/bin/fish"),
            kind: ShellKind::Fish,
            source: crate::shell::ShellSource::Env,
        };
        let words = |cmd: &CommandBuilder| -> Vec<String> {
            cmd.get_argv()
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        let interactive_fish = shell_command(&host, &fish, None, None, &[]);
        assert_eq!(
            words(&interactive_fish)[..3],
            ["/opt/homebrew/bin/fish", "-l", "--init-command"]
        );
        // Commands run under zsh for fish users: they are POSIX shell.
        let fish_c = shell_command(&host, &fish, Some("echo hi"), None, &[]);
        assert_eq!(words(&fish_c)[1..], ["-l", "-c", "echo hi"]);
        assert!(words(&fish_c)[0].ends_with("zsh"));
        assert_eq!(
            fish_c.get_env("SHELL").and_then(|v| v.to_str()),
            Some("/opt/homebrew/bin/fish")
        );
    }

    /// Drive `spec` in a real PTY: wait for its first prompt mark, type
    /// `line`, wait until every `expect` appears, then exit.
    fn drive(spec: PtySpec, line: &str, expect: &[&str]) -> String {
        let pair = native_pty_system().openpty(spec.size).unwrap();
        let mut child = pair.slave.spawn_command(spec.command).unwrap();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let mut writer = pair.master.take_writer().unwrap();
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        let mut out = String::new();
        let wait_for = |out: &mut String, needles: &[&str]| {
            let deadline = Instant::now() + Duration::from_secs(20);
            while !needles.iter().all(|n| out.contains(n)) {
                let left = deadline.saturating_duration_since(Instant::now());
                match rx.recv_timeout(left) {
                    Ok(chunk) => out.push_str(&String::from_utf8_lossy(&chunk)),
                    Err(_) => break,
                }
            }
        };
        wait_for(&mut out, &["\x1b]133;A"]);
        writer.write_all(format!("{line}\r").as_bytes()).unwrap();
        wait_for(&mut out, expect);
        let _ = writer.write_all(b"exit\r");
        let _ = child.kill();
        let _ = child.wait();
        out
    }

    /// A shell in a real PTY that a test types into, reading its output.
    struct Typed {
        child: Box<dyn portable_pty::Child + Send + Sync>,
        writer: Box<dyn Write + Send>,
        rx: mpsc::Receiver<Vec<u8>>,
        out: String,
        _master: Box<dyn portable_pty::MasterPty + Send>,
    }

    impl Typed {
        fn start(spec: PtySpec) -> Self {
            let pair = native_pty_system().openpty(spec.size).unwrap();
            let child = pair.slave.spawn_command(spec.command).unwrap();
            drop(pair.slave);
            let mut reader = pair.master.try_clone_reader().unwrap();
            let writer = pair.master.take_writer().unwrap();
            let (tx, rx) = mpsc::channel::<Vec<u8>>();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            });
            let mut typed = Self {
                child,
                writer,
                rx,
                out: String::new(),
                _master: pair.master,
            };
            typed.wait_for_count("\x1b]133;A", 1);
            typed
        }

        /// Wait until `needle` has appeared `count` times in the output.
        fn wait_for_count(&mut self, needle: &str, count: usize) {
            let deadline = Instant::now() + Duration::from_secs(20);
            while self.out.matches(needle).count() < count {
                let left = deadline.saturating_duration_since(Instant::now());
                match self.rx.recv_timeout(left) {
                    Ok(chunk) => self.out.push_str(&String::from_utf8_lossy(&chunk)),
                    Err(_) => panic!("{needle:?} x{count} never arrived: {:?}", self.out),
                }
            }
        }

        /// Send `bytes`, then wait for the prompt after it.
        fn send_then_prompt(&mut self, bytes: &[u8]) {
            let prompts = self.out.matches("\x1b]133;A").count();
            self.writer.write_all(bytes).unwrap();
            self.wait_for_count("\x1b]133;A", prompts + 1);
        }

        /// The command-end statuses consumers see so far, in order: the output
        /// through the same parser the session uses, so a repeated D counts once.
        fn command_ends(&self) -> Vec<String> {
            let mut parser = OscParser::default();
            let mut events = Vec::new();
            parser.feed(self.out.as_bytes(), &mut events);
            events
                .into_iter()
                .filter_map(|e| match e.event {
                    OscEvent::CommandEnd(code) => code.map(|c| c.to_string()),
                    _ => None,
                })
                .collect()
        }
    }

    impl Drop for Typed {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// `shell` with the shipped integration, against a throwaway `$HOME`
    /// holding `rc` (`(file, contents)`) when given.
    fn integrated_shell(shell: &str, tmp: &Path, rc: Option<(&str, &str)>) -> PtySpec {
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        if let Some((file, contents)) = rc {
            let path = home.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
        let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources");
        let host = HostEnvironment::with_vars(
            Some(tmp.join("cs")),
            Some(shipped),
            given(&[
                ("HOME", home.to_str().unwrap()),
                ("XDG_CONFIG_HOME", home.join(".config").to_str().unwrap()),
                ("PATH", &std::env::var("PATH").unwrap_or_default()),
            ]),
        );
        SpawnIntent {
            shell: Some(shell.to_string()),
            starship_preset: Some("plain".to_string()),
            api_port: None,
            ..interactive(tmp.to_str().unwrap())
        }
        .resolve("shell-test", &host)
    }

    /// A foreground job the user suspends (Ctrl-Z) has not ended: the shell
    /// reports no command-end for it, and its `fg` reports its real end; a
    /// failing command and a signal death report their statuses.
    fn assert_suspend_is_not_a_command_end(shell: &str, rc: Option<(&str, &str)>) {
        assert_command_ends_around_suspend(shell, rc, &["1", "137"], &["1", "137", "130"], "");
    }

    /// Runs the suspend script and asserts the command ends consumers see after
    /// the Ctrl-Z and at the end.
    fn assert_command_ends_around_suspend(
        shell: &str,
        rc: Option<(&str, &str)>,
        after_suspend_want: &[&str],
        end_want: &[&str],
        known_quirks: &str,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let mut sh = Typed::start(integrated_shell(shell, tmp.path(), rc));
        sh.send_then_prompt(b"false\r");
        sh.send_then_prompt(b"sh -c 'kill -9 $$'\r");
        sh.writer.write_all(b"sleep 30\r").unwrap();
        sh.wait_for_count("sleep 30", 1);
        std::thread::sleep(Duration::from_millis(300));
        sh.send_then_prompt(b"\x1a");
        let after_suspend = sh.command_ends();
        sh.writer.write_all(b"fg\r").unwrap();
        sh.wait_for_count("fg", 1);
        std::thread::sleep(Duration::from_millis(300));
        sh.send_then_prompt(b"\x03");
        assert_eq!(
            after_suspend, after_suspend_want,
            "{shell} {known_quirks}: {:?}",
            sh.out
        );
        assert_eq!(
            sh.command_ends(),
            end_want,
            "{shell} {known_quirks}: {:?}",
            sh.out
        );
    }

    #[test]
    fn zsh_reports_no_command_end_for_a_suspended_job() {
        assert_suspend_is_not_a_command_end("/bin/zsh", None);
    }

    /// The user's `setopt KSH_ARRAYS` (zero-based arrays) does not shift the
    /// stop statuses the integration computes.
    #[test]
    fn zsh_with_ksh_arrays_reports_no_command_end_for_a_suspended_job() {
        assert_suspend_is_not_a_command_end("/bin/zsh", Some((".zshrc", "setopt KSH_ARRAYS\n")));
    }

    #[test]
    fn bash_reports_no_command_end_for_a_suspended_job() {
        assert_suspend_is_not_a_command_end("/bin/bash", None);
    }

    /// The installed fish and its major version, or `None` when fish is missing.
    fn fish_with_major() -> Option<(String, u32)> {
        let fish = find_fish()?;
        let version = std::process::Command::new(&fish)
            .arg("--version")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap();
        let major = version
            .rsplit(' ')
            .next()
            .and_then(|v| v.split('.').next())
            .and_then(|m| m.trim().parse().ok())
            .unwrap_or_else(|| panic!("unreadable fish version {version:?}"));
        Some((fish, major))
    }

    #[test]
    fn fish3_reports_no_command_end_for_a_suspended_job() {
        let Some((fish, major)) = fish_with_major() else {
            eprintln!("fish not installed; skipping");
            return;
        };
        if major >= 4 {
            eprintln!("fish {major} installed; see the fish4 test");
            return;
        }
        assert_suspend_is_not_a_command_end(&fish, None);
    }

    /// fish 4 emits OSC 133 itself, beside the integration's, and its D cannot
    /// be switched off. Two of its statuses are wrong, and are pinned here so
    /// nobody mistakes them for intended behaviour:
    /// - Ctrl-Z: a native D reports the stale `$status` of the previous
    ///   command (the second 137), since fish does not set `$status` for a
    ///   suspend.
    /// - ^C of a `fg`'d job: fish 4 reports 0, not 130.
    ///
    /// The doubled D itself is one end per command (`OscParser`).
    #[test]
    fn fish4_suspend_reports_a_stale_status_end_and_fg_interrupt_reports_zero() {
        let Some((fish, major)) = fish_with_major() else {
            eprintln!("fish not installed; skipping");
            return;
        };
        if major < 4 {
            eprintln!("fish {major} installed; see the fish3 test");
            return;
        }
        assert_command_ends_around_suspend(
            &fish,
            None,
            &["1", "137", "137"],
            &["1", "137", "137", "0"],
            "fish 4 native D on Ctrl-Z reports stale $status; fish 4 reports 0 for ^C of a fg'd job",
        );
    }

    const PROBE: &str = r#"printf 'M=%s:%s\n' "$CS_RC_MARKER" "$PWD""#;

    /// A real `shell` terminal against a throwaway `$HOME` and the
    /// shell-integration scripts the app ships: it runs the user's own rc
    /// file, starts in the worktree, and reports OSC 7/133.
    fn assert_shell_runs_users_rc(shell: &str, rc_file: &str, rc_line: &str, marker: &str) {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let worktree = tmp.path().join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        let rc = home.join(rc_file);
        std::fs::create_dir_all(rc.parent().unwrap()).unwrap();
        std::fs::write(&rc, format!("{rc_line}\n")).unwrap();
        let worktree = worktree.canonicalize().unwrap();
        let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources");
        let host = HostEnvironment::with_vars(
            Some(tmp.path().join("cs")),
            Some(shipped),
            given(&[
                ("HOME", home.to_str().unwrap()),
                ("XDG_CONFIG_HOME", home.join(".config").to_str().unwrap()),
                ("XDG_DATA_HOME", home.join(".local/share").to_str().unwrap()),
                ("PATH", &std::env::var("PATH").unwrap_or_default()),
            ]),
        );
        let spec = SpawnIntent {
            shell: Some(shell.to_string()),
            starship_preset: Some("plain".to_string()),
            api_port: None,
            cols: 200,
            ..interactive(worktree.to_str().unwrap())
        }
        .resolve("shell-test", &host);
        let want = format!("M={marker}:{}", worktree.display());
        let out = drive(spec, PROBE, &[&want, "\x1b]133;D;0"]);
        assert!(out.contains(&want), "rc marker / cwd missing: {out:?}");
        assert!(
            out.contains("\x1b]7;file://"),
            "no OSC 7 cwd report: {out:?}"
        );
        assert!(
            out.contains("\x1b]133;D;0"),
            "no command-end mark after the probe: {out:?}"
        );
    }

    #[test]
    fn bash_terminal_sources_bash_profile_and_starts_in_the_worktree() {
        assert_shell_runs_users_rc(
            "/bin/bash",
            ".bash_profile",
            "export CS_RC_MARKER=bash-profile",
            "bash-profile",
        );
    }

    #[test]
    fn bash_terminal_falls_back_to_bashrc() {
        assert_shell_runs_users_rc(
            "/bin/bash",
            ".bashrc",
            "export CS_RC_MARKER=bashrc",
            "bashrc",
        );
    }

    #[test]
    fn zsh_terminal_keeps_its_zdotdir_wrappers() {
        assert_shell_runs_users_rc("/bin/zsh", ".zshrc", "export CS_RC_MARKER=zshrc", "zshrc");
    }

    #[test]
    fn fish_terminal_reads_config_fish() {
        let Some(fish) = find_fish() else {
            eprintln!("fish not installed; skipping");
            return;
        };
        assert_shell_runs_users_rc(
            &fish,
            ".config/fish/config.fish",
            "set -gx CS_RC_MARKER fish-config",
            "fish-config",
        );
    }

    /// A real `shell` terminal with the shipped integration, a stub `claude` and
    /// `codex` first on `PATH` (each logs its argv to `<tmp>/argv.log`, one
    /// `<arg>` per argument, a line per run) and the tab's variables `env`; types
    /// `line`, then returns the log lines.
    fn typed_agents(
        shell: &str,
        tmp: &Path,
        env: &[(&str, &str)],
        rc: Option<(&str, &str)>,
        line: &str,
    ) -> Vec<String> {
        let stubs = tmp.join("stubs");
        std::fs::create_dir_all(&stubs).unwrap();
        let log = tmp.join("argv.log");
        let _ = std::fs::remove_file(&log);
        for name in ["claude", "codex"] {
            let stub = stubs.join(name);
            std::fs::write(
                &stub,
                format!(
                    "#!/bin/sh\nif [ \"$1\" = --help ]; then echo '      --no-daemon'; exit 0; fi\n\
                     for a in \"$@\"; do printf '<%s>' \"$a\"; done >> '{log}'\necho >> '{log}'\n",
                    log = log.display()
                ),
            )
            .unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        if let Some((file, contents)) = rc {
            let path = home.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
        let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources");
        let path = format!(
            "{}:{}",
            stubs.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let host = HostEnvironment::with_vars(
            Some(tmp.join("cs")),
            Some(shipped),
            given(&[
                ("HOME", home.to_str().unwrap()),
                ("XDG_CONFIG_HOME", home.join(".config").to_str().unwrap()),
                ("XDG_DATA_HOME", home.join(".local/share").to_str().unwrap()),
                ("PATH", &path),
            ]),
        );
        let spec = SpawnIntent {
            shell: Some(shell.to_string()),
            starship_preset: Some("plain".to_string()),
            api_port: None,
            cols: 400,
            env: given(env),
            ..interactive(tmp.to_str().unwrap())
        }
        .resolve("shell-test", &host);
        // Ensure the stubs (not a real CLI) answer, even though login rc files may
        // reorder PATH.
        let line = format!(
            "PATH={}:$PATH; {line}; printf '%s-%s\\n' END MARK",
            stubs.display()
        );
        let line = if shell.ends_with("fish") {
            line.replacen("PATH=", "set PATH ", 1)
                .replacen(":$PATH;", " $PATH;", 1)
        } else {
            line
        };
        let out = drive(spec, &line, &["END-MARK"]);
        assert!(out.contains("END-MARK"), "{shell} never finished: {out:?}");
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// In a CodeSense tab, `claude` and `codex` typed by hand start connected to
    /// CodeSense; the user's own flags, a project's own `codesense` server,
    /// subcommands, `--remote` and `command claude` are left as they are.
    fn assert_agents_auto_connect(shell: &str) {
        let tmp = tempfile::tempdir().unwrap();
        let tmp = tmp.path().canonicalize().unwrap();
        let config = tmp.join("mcp.json");
        std::fs::write(&config, "{}").unwrap();
        std::fs::create_dir_all(tmp.join("proj/sub")).unwrap();
        std::fs::write(
            tmp.join("proj/.mcp.json"),
            r#"{"mcpServers": {"codesense" : {"command": "codesense-bridge"}}}"#,
        )
        .unwrap();
        let codex = r#"mcp_servers.codesense={command="/b",args=["--remote-socket","/s"]}"#;
        let tab = [
            ("CODESENSE_MCP_CONFIG", config.to_str().unwrap()),
            ("CODESENSE_CODEX_MCP", codex),
        ];
        let c = config.display();
        // A tab on this Mac.
        let lines = typed_agents(
            shell,
            &tmp,
            &tab,
            None,
            "claude 'fix it'; claude --mcp-config x.json hi; claude --settings=s.json; \
             claude --strict-mcp-config a; claude mcp list; command claude plain; \
             claude -- --settings; codex 'x y'; cd proj/sub; claude p; cd ../..",
        );
        assert_eq!(
            lines,
            [
                format!("<--mcp-config={c}><fix it>"),
                "<--mcp-config><x.json><hi>".to_string(),
                format!("<--mcp-config={c}><--settings=s.json>"),
                "<--strict-mcp-config><a>".to_string(),
                "<mcp><list>".to_string(),
                "<plain>".to_string(),
                // After `--` everything is the user's prompt.
                format!("<--mcp-config={c}><--><--settings>"),
                // Codex keeps its shared daemon on this Mac.
                format!("<-c><{codex}><x y>"),
                // The folder's own `codesense` server is kept.
                "<p>".to_string(),
            ],
            "{shell}: this Mac"
        );
        // A tab on a machine (its bridge socket is set).
        let mut remote = tab.to_vec();
        remote.push(("CODESENSE_MCP_SOCK", "/s"));
        let lines = typed_agents(
            shell,
            &tmp,
            &remote,
            None,
            "codex 'x y'; codex --no-daemon z; codex --remote=ws://a q; codex --remote ws://a r; \
             cd proj/sub; claude p; cd ../..",
        );
        assert_eq!(
            lines,
            [
                // Its MCP servers run inside the tab, off the shared daemon.
                format!("<--no-daemon><-c><{codex}><x y>"),
                format!("<-c><{codex}><--no-daemon><z>"),
                "<--remote=ws://a><q>".to_string(),
                "<--remote><ws://a><r>".to_string(),
                // A folder's entry cannot reach this tab: the tab's replaces it.
                format!("<--mcp-config={c}><p>"),
            ],
            "{shell}: a machine"
        );
        // With the tab's terminal host there, every agent run goes through the
        // host's helper (which claims an invocation and starts the CLI);
        // subcommands and version checks run the plain CLI.
        let host = tmp.join("host");
        std::fs::create_dir_all(&host).unwrap();
        let helper = host.join(crate::activity::HELPER_NAME);
        std::fs::write(
            &helper,
            format!(
                "#!/bin/sh\nprintf '<helper>' >> '{log}'\n\
                 for a in \"$@\"; do printf '<%s>' \"$a\"; done >> '{log}'\necho >> '{log}'\n",
                log = tmp.join("argv.log").display()
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut observed = tab.to_vec();
        observed.push((crate::activity::HOST_ENV, host.to_str().unwrap()));
        let lines = typed_agents(
            shell,
            &tmp,
            &observed,
            None,
            "claude 'fix it'; claude --settings=s.json; claude mcp list; claude --version; \
             codex 'x y'; codex login; codex --version",
        );
        assert_eq!(
            lines,
            [
                format!("<helper><--activity-run><claude><--><claude><--mcp-config={c}><fix it>"),
                format!(
                    "<helper><--activity-run><claude><--><claude><--mcp-config={c}><--settings=s.json>"
                ),
                "<mcp><list>".to_string(),
                "<--version>".to_string(),
                format!("<helper><--activity-run><codex><--><codex><-c><{codex}><x y>"),
                "<login>".to_string(),
                "<--version>".to_string(),
            ],
            "{shell}: observed by the tab's host"
        );
    }

    /// Outside a CodeSense tab's variables (or with their files gone), and with
    /// the user's own `claude` function, nothing is added.
    fn assert_agents_untouched_without_the_tab(shell: &str, rc: (&str, &str)) {
        let tmp = tempfile::tempdir().unwrap();
        let tmp = tmp.path().canonicalize().unwrap();
        let lines = typed_agents(
            shell,
            &tmp,
            &[(
                "CODESENSE_MCP_CONFIG",
                tmp.join("gone.json").to_str().unwrap(),
            )],
            None,
            "claude a; codex b",
        );
        assert_eq!(lines, ["<a>", "<b>"], "{shell}");
        let tmp = tempfile::tempdir().unwrap();
        let tmp = tmp.path().canonicalize().unwrap();
        let config = tmp.join("mcp.json");
        std::fs::write(&config, "{}").unwrap();
        let lines = typed_agents(
            shell,
            &tmp,
            &[("CODESENSE_MCP_CONFIG", config.to_str().unwrap())],
            Some(rc),
            "claude a",
        );
        assert_eq!(
            lines,
            ["<own>", "<a>"],
            "{shell}: the user's function stays"
        );
    }

    #[test]
    fn zsh_agents_auto_connect() {
        assert_agents_auto_connect("/bin/zsh");
        assert_agents_untouched_without_the_tab(
            "/bin/zsh",
            (
                ".zshrc",
                "claude() { command claude own; command claude \"$@\"; }",
            ),
        );
    }

    #[test]
    fn bash_agents_auto_connect() {
        assert_agents_auto_connect("/bin/bash");
        assert_agents_untouched_without_the_tab(
            "/bin/bash",
            (
                ".bashrc",
                "claude() { command claude own; command claude \"$@\"; }",
            ),
        );
    }

    #[test]
    fn fish_agents_auto_connect() {
        let Some(fish) = find_fish() else {
            eprintln!("fish not installed; skipping");
            return;
        };
        assert_agents_auto_connect(&fish);
        assert_agents_untouched_without_the_tab(
            &fish,
            (
                ".config/fish/config.fish",
                "function claude; command claude own; command claude $argv; end",
            ),
        );
    }

    /// The spawn dir never yields an empty/invalid dir
    /// (which would `chdir("")` → `$HOME` for a login shell).
    #[test]
    fn spawn_dir_floors_empty_and_missing_never_home() {
        assert_eq!(spawn_dir(""), None);
        assert_eq!(spawn_dir("/definitely/not/a/real/dir/sw-fx-41"), None);
        let real = std::env::temp_dir();
        let real_str = real.to_str().expect("temp dir path is valid utf-8");
        assert_eq!(spawn_dir(real_str), Some(real_str));
        assert_eq!(interactive(real_str).spawn_dir(), Some(real.clone()));
    }

    /// The warn-list only surfaces vars that are actually
    /// inherited (present + non-empty).
    #[test]
    fn inherited_env_to_warn_reports_only_present_nonempty() {
        let names = vec![
            "ANTHROPIC_API_KEY".to_string(),
            "EMPTY_VAR".to_string(),
            "ABSENT_VAR".to_string(),
        ];
        let fake_env = |k: &str| match k {
            "ANTHROPIC_API_KEY" => Some("sk-secret".to_string()),
            "EMPTY_VAR" => Some(String::new()),
            _ => None,
        };
        assert_eq!(
            inherited_env_to_warn(&names, fake_env),
            vec!["ANTHROPIC_API_KEY".to_string()]
        );
        assert!(inherited_env_to_warn(&names, |_| None).is_empty());
        assert!(inherited_env_to_warn(&[], fake_env).is_empty());
    }

    #[test]
    fn zdotdir_uses_inherited_when_not_the_managed_dir() {
        let shell_dir = Path::new("/cs/shell/zsh");
        let got = resolve_source_zdotdir(
            Some("/Users/me".to_string()),
            shell_dir,
            Some("/Users/me".to_string()),
        );
        assert_eq!(got, "/Users/me");
    }

    #[test]
    fn zdotdir_falls_back_to_home_when_inherited_is_managed_dir() {
        // Nested codesense shell: inherited ZDOTDIR already == shell_dir.
        // Must NOT echo shell_dir back (that self-sources → recursion).
        let shell_dir = Path::new("/cs/shell/zsh");
        let got = resolve_source_zdotdir(
            Some("/cs/shell/zsh".to_string()),
            shell_dir,
            Some("/Users/me".to_string()),
        );
        assert_eq!(got, "/Users/me");
    }

    #[test]
    fn zdotdir_falls_back_to_home_when_unset() {
        let shell_dir = Path::new("/cs/shell/zsh");
        let got = resolve_source_zdotdir(None, shell_dir, Some("/Users/me".to_string()));
        assert_eq!(got, "/Users/me");
    }

    /// An argv-direct intent spawns its first arg as PID-1 of the
    /// PTY, no shell wrapper, no shell integration, no RC wrappers. The intent's
    /// own env rides onto the child, so the managed-launch SOCKET_ENV / TOKEN_ENV
    /// bindings (prepared pre-spawn) reach the agent at exec.
    #[test]
    fn argv_direct_bypasses_the_shell_wrapper_and_passes_env() {
        let fx = Fixture::new();
        let host = fx.host(&[("SHELL", "/bin/zsh"), ("PATH", "/usr/bin:/bin")]);
        let cwd = fx.resources.to_string_lossy().into_owned();
        let intent = SpawnIntent {
            argv: Some(vec![
                "/opt/claude".to_string(),
                "--one".to_string(),
                "two".to_string(),
            ]),
            env: vec![
                (
                    "CODESENSE_ACTIVITY_SOCKET".to_string(),
                    "/tmp/act.sock".to_string(),
                ),
                (
                    "CODESENSE_ACTIVITY_TOKEN".to_string(),
                    "tok-xyz".to_string(),
                ),
            ],
            ..interactive(&cwd)
        };
        assert!(!intent.is_interactive(), "argv is non-interactive");
        assert!(
            !intent.osc_enabled(),
            "argv-direct has no shell integration"
        );
        let spec = intent.resolve("tab-argv", &host);
        assert_eq!(argv(&spec), ["/opt/claude", "--one", "two"]);
        // No ZDOTDIR / shell-init / starship for an argv-direct spawn.
        assert_eq!(env(&spec, "ZDOTDIR"), None);
        assert_eq!(env(&spec, "__CODESENSE_SHELL_INIT"), None);
        assert_eq!(env(&spec, "STARSHIP_CONFIG"), None);
        // The intent's env lands on the child so the activity bindings reach exec time.
        assert_eq!(
            env(&spec, "CODESENSE_ACTIVITY_SOCKET").as_deref(),
            Some("/tmp/act.sock")
        );
        assert_eq!(
            env(&spec, "CODESENSE_ACTIVITY_TOKEN").as_deref(),
            Some("tok-xyz")
        );
        // The CodeSense bin dir still prefixes PATH so a launcher that
        // relies on it (`codesense-bridge` under `claude`) resolves.
        let path = env(&spec, "PATH").unwrap_or_default();
        assert!(
            path.starts_with(&fx.home.join("bin").display().to_string()),
            "codesense bin prepended: {path}"
        );
    }

    /// A saved Agent's variables reach the child, under every variable
    /// CodeSense sets: its identity, API port, PATH, the guarded-variable
    /// strip and the one-use bindings all win on a clash.
    #[test]
    fn agent_env_reaches_the_child_under_codesense_variables() {
        let fx = Fixture::new();
        let host = fx.host(&[("SHELL", "/bin/zsh"), ("PATH", "/usr/bin:/bin")]);
        let cwd = fx.resources.to_string_lossy().into_owned();
        let pairs = |pairs: &[(&str, &str)]| given(pairs);
        let intent = SpawnIntent {
            argv: Some(vec!["/opt/claude".to_string()]),
            env_remove: vec!["ANTHROPIC_API_KEY".to_string()],
            env: pairs(&[("CODESENSE_MCP_CONFIG", "/scoped.json")]),
            agent_env: pairs(&[
                ("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1"),
                ("CODESENSE_TAB_SESSION_ID", "spoofed"),
                ("CODESENSE_API_PORT", "1"),
                ("PATH", "/agent/bin"),
                ("ANTHROPIC_API_KEY", "agent-key"),
                ("CODESENSE_MCP_CONFIG", "/agent.json"),
            ]),
            ..interactive(&cwd)
        };
        let spec = intent.resolve("tab-agent", &host);
        assert_eq!(
            env(&spec, "CLAUDE_CODE_DISABLE_AUTO_MEMORY").as_deref(),
            Some("1")
        );
        assert_eq!(
            env(&spec, "CODESENSE_TAB_SESSION_ID").as_deref(),
            Some("tab-agent")
        );
        assert_eq!(env(&spec, "CODESENSE_API_PORT").as_deref(), Some("8737"));
        assert!(!env(&spec, "PATH").unwrap().contains("/agent/bin"));
        assert_eq!(env(&spec, "ANTHROPIC_API_KEY"), None);
        assert_eq!(
            env(&spec, "CODESENSE_MCP_CONFIG").as_deref(),
            Some("/scoped.json")
        );
        // CodeSense's own launch variables are never taken from the agent, even on a spawn that
        // sets none of them; on a shell-hosted spawn `SHELL` is the shell's.
        let shell_hosted = SpawnIntent {
            api_port: None,
            agent_env: pairs(&[
                ("CODESENSE_TAB_SESSION_ID", "spoofed"),
                ("CODESENSE_API_PORT", "1"),
                ("CODESENSE_MCP_CONFIG", "/agent.json"),
                (crate::activity::HOST_ENV, "/agent"),
                (crate::activity::SOCKET_ENV, "/agent.sock"),
                (crate::activity::TOKEN_ENV, "agent"),
                (crate::activity::RELEASE_QUALIFIED_ENV, "1"),
                ("SHELL", "/agent/sh"),
                ("AGENT_ONLY", "yes"),
            ]),
            ..interactive(&cwd)
        };
        let spec = shell_hosted.resolve("tab-shell", &host);
        assert_eq!(env(&spec, "AGENT_ONLY").as_deref(), Some("yes"));
        assert_eq!(env(&spec, "SHELL").as_deref(), Some("/bin/zsh"));
        assert_eq!(
            env(&spec, "CODESENSE_TAB_SESSION_ID").as_deref(),
            Some("tab-shell")
        );
        for name in [
            "CODESENSE_API_PORT",
            "CODESENSE_MCP_CONFIG",
            crate::activity::HOST_ENV,
            crate::activity::SOCKET_ENV,
            crate::activity::TOKEN_ENV,
            crate::activity::RELEASE_QUALIFIED_ENV,
        ] {
            assert_eq!(env(&spec, name), None, "{name}");
        }
        // A remote host takes them from the saved Agent's startup, as carried on the wire.
        let startup = codesense_host_proto::reverse_mcp::AgentStartup {
            args: Vec::new(),
            settings: serde_json::json!({}),
            prompt: None,
            env: [(
                "CLAUDE_CODE_DISABLE_AUTO_MEMORY".to_string(),
                "1".to_string(),
            )]
            .into(),
        };
        let startup: codesense_host_proto::reverse_mcp::AgentStartup =
            serde_json::from_value(serde_json::to_value(&startup).unwrap()).unwrap();
        let remote = SpawnIntent {
            argv: Some(vec!["/opt/claude".to_string()]),
            ..interactive(&cwd)
        }
        .with_agent_startup(Some(&startup));
        let spec = remote.resolve("tab-remote", &host);
        assert_eq!(
            env(&spec, "CLAUDE_CODE_DISABLE_AUTO_MEMORY").as_deref(),
            Some("1")
        );
        assert_eq!(
            env(&spec, "CODESENSE_TAB_SESSION_ID").as_deref(),
            Some("tab-remote")
        );
        assert_eq!(
            remote.clone().with_agent_startup(None).agent_env,
            remote.agent_env
        );
        // An intent with no agent variables keeps its old wire shape.
        let wire = serde_json::to_value(interactive("/tmp")).unwrap();
        assert!(wire.get("agent_env").is_none());
        let restored: SpawnIntent =
            serde_json::from_value(serde_json::to_value(&intent).unwrap()).unwrap();
        assert_eq!(restored, intent);
    }

    /// An argv-direct agent builds its PATH on the user's login-shell PATH,
    /// under CodeSense's bin dir.
    #[test]
    fn argv_spawn_uses_the_login_path_when_given() {
        let fx = Fixture::new();
        let host = fx.host(&[("SHELL", "/bin/zsh"), ("PATH", "/usr/bin:/bin")]);
        let cwd = fx.resources.to_string_lossy().into_owned();
        let intent = SpawnIntent {
            argv: Some(vec!["/opt/claude".to_string()]),
            login_path: Some("/opt/homebrew/bin:/usr/bin:/bin".to_string()),
            ..interactive(&cwd)
        };
        let spec = intent.resolve("tab-login", &host);
        assert_eq!(
            env(&spec, "PATH").unwrap(),
            format!(
                "{}:/opt/homebrew/bin:/usr/bin:/bin",
                fx.home.join("bin").display()
            )
        );
    }

    /// With no login PATH, or an empty one, an argv-direct agent keeps the
    /// host's PATH.
    #[test]
    fn argv_spawn_without_a_login_path_keeps_the_host_path() {
        let fx = Fixture::new();
        let host = fx.host(&[("SHELL", "/bin/zsh"), ("PATH", "/usr/bin:/bin")]);
        let cwd = fx.resources.to_string_lossy().into_owned();
        let expected = format!("{}:/usr/bin:/bin", fx.home.join("bin").display());
        for login_path in [None, Some(String::new())] {
            let intent = SpawnIntent {
                argv: Some(vec!["/opt/claude".to_string()]),
                login_path: login_path.clone(),
                ..interactive(&cwd)
            };
            let spec = intent.resolve("tab-host-path", &host);
            assert_eq!(env(&spec, "PATH").unwrap(), expected, "{login_path:?}");
        }
    }

    /// A shell spawn builds on the host's PATH even when a login PATH rides
    /// along: the login shell sets its own.
    #[test]
    fn a_shell_spawn_ignores_the_login_path() {
        let fx = Fixture::new();
        let host = fx.host(&[("SHELL", "/bin/zsh"), ("PATH", "/usr/bin:/bin")]);
        let cwd = fx.resources.to_string_lossy().into_owned();
        let intent = SpawnIntent {
            login_path: Some("/opt/homebrew/bin:/usr/bin:/bin".to_string()),
            ..interactive(&cwd)
        };
        let spec = intent.resolve("tab-shell-path", &host);
        let path = env(&spec, "PATH").unwrap();
        assert!(path.ends_with(":/usr/bin:/bin"), "{path}");
        assert!(!path.contains("/opt/homebrew/bin"), "{path}");
    }

    /// #2167: a login PATH holding another `claude` never changes which
    /// binary runs; argv[0] stays the absolute path CodeSense resolved.
    #[test]
    fn a_login_path_does_not_change_the_agent_binary() {
        let fx = Fixture::new();
        let host = fx.host(&[("SHELL", "/bin/zsh"), ("PATH", "/usr/bin:/bin")]);
        let cwd = fx.resources.to_string_lossy().into_owned();
        let stale = fx.resources.join("stale-bin");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("claude"), "#!/bin/sh\n").unwrap();
        let intent = SpawnIntent {
            argv: Some(vec!["/opt/claude".to_string(), "--one".to_string()]),
            login_path: Some(format!("{}:/usr/bin:/bin", stale.display())),
            ..interactive(&cwd)
        };
        let spec = intent.resolve("tab-stale", &host);
        assert_eq!(argv(&spec), ["/opt/claude", "--one"]);
        assert!(env(&spec, "PATH")
            .unwrap()
            .contains(&stale.display().to_string()));
    }

    /// A bare argv[0] resolves against the child's PATH, so it never gets the
    /// login PATH: a `claude` on that PATH cannot be the binary that runs.
    #[test]
    fn a_bare_argv0_keeps_the_host_path() {
        let fx = Fixture::new();
        let host = fx.host(&[("SHELL", "/bin/zsh"), ("PATH", "/usr/bin:/bin")]);
        let cwd = fx.resources.to_string_lossy().into_owned();
        let stale = fx.resources.join("stale-bin");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("claude"), "#!/bin/sh\n").unwrap();
        let intent = SpawnIntent {
            argv: Some(vec!["claude".to_string(), "--one".to_string()]),
            login_path: Some(format!("{}:/usr/bin:/bin", stale.display())),
            ..interactive(&cwd)
        };
        let spec = intent.resolve("tab-bare", &host);
        assert_eq!(argv(&spec), ["claude", "--one"]);
        assert_eq!(
            env(&spec, "PATH").unwrap(),
            format!("{}:/usr/bin:/bin", fx.home.join("bin").display())
        );
    }

    /// The intent crosses the wire whole.
    #[test]
    fn the_intent_round_trips_through_serde() {
        let intent = SpawnIntent {
            remote_launch: Default::default(),
            command: Some("claude".to_string()),
            shell: Some("/bin/bash".to_string()),
            starship_preset: Some("full".to_string()),
            env_remove: vec!["ANTHROPIC_API_KEY".to_string()],
            env: vec![("K".to_string(), "V".to_string())],
            ..interactive("/tmp")
        };
        let wire = serde_json::to_string(&intent).unwrap();
        assert_eq!(serde_json::from_str::<SpawnIntent>(&wire).unwrap(), intent);
    }
}
