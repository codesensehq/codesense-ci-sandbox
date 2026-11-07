// SPDX-License-Identifier: MIT
//! Native interactive provider invocation using the existing private bridge projection.
// Only the runtime probe serves hosts without session-bound native launch.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
use codesense_host_proto::agent_cli::NativeLaunch;
use codesense_pty_core::agent_cli::{
    adapter, descriptor, render_scope_flags, validate_startup, ActivityProbe, Descriptor,
    LaunchSettings, McpInventory, McpListing, McpTransportKind, Prepare, ScopeInjection, ARGV_SLOT,
};
use codesense_pty_core::runner::PtySpec;
use std::ffi::OsString;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::usage_receiver;

#[derive(Clone, Debug)]
pub(crate) struct OwnedSettings {
    device: u64,
    inode: u64,
    digest: [u8; 32],
}
impl OwnedSettings {
    pub(crate) fn remove_if_unchanged(&self, config: &Path) -> std::io::Result<()> {
        use sha2::Digest;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let path = config.with_extension("settings.json");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.dev() != self.device
            || metadata.ino() != self.inode
            || metadata.nlink() != 1
            || metadata.len() > 64 * 1024
        {
            return Ok(());
        }
        let mut bytes = Vec::new();
        file.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() <= 64 * 1024 && <[u8; 32]>::from(sha2::Sha256::digest(&bytes)) == self.digest
        {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn prepare(
    spec: &mut PtySpec,
    launch: NativeLaunch,
    config: &Path,
    session: &str,
) -> Result<(), String> {
    prepare_agent(spec, launch, config, None, session).map(|_| ())
}

/// `session` is the launch's session id: its usage export is tagged with it.
pub(crate) fn prepare_agent(
    spec: &mut PtySpec,
    launch: NativeLaunch,
    config: &Path,
    agent: Option<&codesense_host_proto::reverse_mcp::AgentStartup>,
    session: &str,
) -> Result<Option<OwnedSettings>, String> {
    let mut owned_settings = None;
    if let Some(agent) = agent {
        validate_startup(agent, launch)?;
    }
    let Some(cli) = launch.cli() else {
        return Ok(None);
    };
    let adapter = adapter(cli);
    let descriptor = adapter.descriptor();
    if let Some(check) = descriptor.machine_check {
        check.verify()?;
    }
    let binary = descriptor.binary.as_str();
    let probe = adapter.probe();
    let (cwd, path) = login_path(spec)?;
    let path = path.as_str();
    let program = resolve_binary(path, binary).ok_or_else(|| {
        format!(
            "{binary} {}",
            codesense_host_proto::reverse_mcp::NATIVE_RUNTIME_MISSING
        )
    })?;
    let mut help = Command::new(&program);
    help.args(probe.help_subcommand)
        .arg("--help")
        .env_clear()
        .envs(spec.command.iter_full_env_as_str())
        .env("PATH", path)
        .current_dir(cwd);
    let help = bounded_output_within(&mut help, binary, probe_deadline(probe))?;
    let required = probe.launch_flags;
    if required.iter().any(|flag| !help.contains(flag)) {
        return Err(format!("{binary} lacks required native MCP configuration support ({}); update the remote runtime", required.join(", ")));
    }
    let required = probe.agent_flags;
    if agent.is_some() && required.iter().any(|flag| !help.contains(flag)) {
        return Err(format!(
            "{binary} lacks required saved-Agent settings support ({}); update the remote runtime",
            required.join(", ")
        ));
    }
    let projection: serde_json::Value = serde_json::from_slice(
        &std::fs::read(config).map_err(|e| format!("session MCP config unavailable: {e}"))?,
    )
    .map_err(|e| format!("invalid session MCP config: {e}"))?;
    let inventory = descriptor
        .mcp_scope
        .as_ref()
        .and_then(|scope| scope.inventory.as_ref());
    let inherited = match inventory {
        Some(inventory) => {
            let mut listing = Command::new(&program);
            listing
                .args(
                    inventory
                        .selector_args(agent.map_or(&[][..], |agent| agent.args.as_slice()))?,
                )
                .args(&inventory.argv)
                .env_clear()
                .envs(spec.command.iter_full_env_as_str())
                .env("PATH", path)
                .current_dir(cwd);
            let output = bounded_output(
                &mut listing,
                &format!("{binary} MCP configuration inventory"),
            )?;
            McpInventory::names(output.as_bytes())
                .map_err(|error| format!("{} cannot report its MCP configuration ({error}); update the remote runtime or its MCP registrations", descriptor.name))?
        }
        None => Default::default(),
    };
    let activity_supported = match probe.activity {
        ActivityProbe::HelpFlag(flag) => help.contains(flag),
        ActivityProbe::Version => {
            let home =
                descriptor.config_home(|key| spec.command.get_env(key).map(|v| v.to_os_string()));
            let locked = codesense_pty_core::agent_cli::version_cache::LockedRuntime::for_probe(
                probe,
                home.as_deref(),
            );
            let identity = locked.as_ref().map(|runtime| runtime.identity());
            let cached =
                codesense_pty_core::agent_cli::version_cache::read(&program, probe, identity);
            let version = cached.or_else(|| {
                let mut version = Command::new(&program);
                version
                    .arg("--version")
                    .env_clear()
                    .envs(spec.command.iter_full_env_as_str())
                    .env("PATH", path)
                    .envs(codesense_pty_core::agent_cli::version_cache::resolve_env(
                        probe.version_env,
                        locked.as_ref().map(|runtime| runtime.root()),
                    ))
                    .current_dir(cwd);
                match bounded_output_within(
                    &mut version,
                    &format!("{binary} activity version"),
                    probe_deadline(probe),
                ) {
                    Ok(output) => {
                        let _ = codesense_pty_core::agent_cli::version_cache::write(
                            &program, &output, probe, identity,
                        );
                        Some(output)
                    }
                    Err(reason) => {
                        log::warn!("{binary} activity unqualified: {reason}");
                        None
                    }
                }
            });
            let qualified = version.map(|version| descriptor.release.qualified(&version));
            set_release_qualified(spec, qualified);
            qualified == Some(true)
        }
    };
    let helper = std::env::current_exe().map_err(|error| error.to_string())?;
    let helper = helper.to_str().ok_or("activity helper path is not UTF-8")?;
    let mut options = arguments(descriptor, config, &projection, &inherited)?;
    // A launch takes settings when it carries a saved Agent's (its probe checked the
    // CLI takes them) or the activity observer (likewise); a plain one whose observer is
    // unsupported takes none. They go right after its MCP configuration, ahead of a saved
    // Agent's own args, wherever preparing moves that place.
    let takes_settings = agent.is_some() || activity_supported;
    if takes_settings {
        options.push(ARGV_SLOT.into());
    }
    let mut settings = agent.map_or_else(|| serde_json::json!({}), |agent| agent.settings.clone());
    if let Some(agent) = agent {
        options.extend(agent.args.clone());
    }
    if activity_supported {
        let mut launch = Prepare::new(options, settings, helper);
        launch.home = descriptor
            .config_home(|key| spec.command.get_env(key).map(|value| value.to_os_string()));
        adapter.prepare(&mut launch)?;
        (options, settings) = (launch.arguments, launch.settings);
    }
    if takes_settings {
        let delivered = match &descriptor.launch_settings {
            LaunchSettings::Flag(flag) => {
                // A saved Agent's settings go in a private file that goes with the session;
                // a plain launch's ride inline, so nothing is written on the machine.
                let value = if agent.is_some() {
                    let file = config.with_extension("settings.json");
                    owned_settings = Some(write_owned_settings(&file, &settings)?);
                    file.to_str().ok_or("invalid settings path")?.to_owned()
                } else {
                    settings.to_string()
                };
                vec![flag.clone(), value]
            }
            overrides @ LaunchSettings::Overrides(_) => overrides
                .overrides(&settings)
                .ok_or("launch settings cannot be passed as overrides")?,
            LaunchSettings::Env(env_var) => {
                spec.command.env(env_var, settings.to_string());
                Vec::new()
            }
            LaunchSettings::HomeConfig { file } => {
                let home = descriptor
                    .config_home(|key| spec.command.get_env(key).map(|value| value.to_os_string()))
                    .ok_or("CLI home unavailable")?;
                codesense_pty_core::activity::write_home_config(&home, file, &settings)
                    .map_err(|error| error.to_string())?;
                Vec::new()
            }
        };
        let at = options
            .iter()
            .position(|option| option == ARGV_SLOT)
            .ok_or("launch settings lost their place")?;
        options.splice(at..=at, delivered);
    }
    // The usage export's flags (Codex's `-c`) are settings too, so they go before the prompt.
    options.extend(telemetry(
        spec,
        descriptor,
        usage_receiver::advertised_port(),
        session,
        agent.map(|agent| &agent.env),
    ));
    // Flags and observers precede the prompt delimiter, including saved-Agent args.
    if let Some(prompt) = agent.and_then(|agent| agent.prompt.as_ref()) {
        options.extend(["--".into(), prompt.clone()]);
    }
    spec.command.env("PATH", path);
    let options: Vec<OsString> = options.into_iter().map(Into::into).collect();
    // An observed CLI runs on its engine, when it has one, which the helper starts.
    *spec.command.get_argv_mut() = if activity_supported {
        codesense_pty_core::activity::launch_argv(
            helper.as_ref(),
            cli,
            program.as_os_str(),
            &options,
        )
    } else {
        std::iter::once(program.into_os_string())
            .chain(options)
            .collect()
    };
    Ok(owned_settings)
}

/// Points the CLI's own usage export at this host's receiver under the launch's session id, and
/// returns the argv it needs. A variable the launch's environment already sets keeps its value, as
/// on the Mac, unless only the saved Agent's own `agent_env` set it: the export wins over that.
/// Nothing is pointed anywhere without a receiver, or for a session id the receiver would refuse.
fn telemetry(
    spec: &mut PtySpec,
    descriptor: &Descriptor,
    port: Option<u16>,
    session: &str,
    agent_env: Option<&std::collections::BTreeMap<String, String>>,
) -> Vec<String> {
    let (Some(port), Some(export)) = (port, descriptor.telemetry.as_ref()) else {
        return Vec::new();
    };
    let Some(endpoint) = usage_receiver::endpoint(port, session) else {
        return Vec::new();
    };
    let (env, argv) = export.resolved(&endpoint);
    for (key, value) in env {
        let agent_set = agent_env.is_some_and(|agent_env| agent_env.contains_key(&key));
        if agent_set || spec.command.get_env(&key).is_none() {
            spec.command.env(key, value);
        }
    }
    argv
}

/// The private settings file `settings` holding `generated`, created owner-only. A
/// retry of the same launch finds the identical file and keeps it; anything else there
/// refuses. The returned identity lets cleanup remove exactly this file.
fn write_owned_settings(
    settings: &Path,
    generated: &serde_json::Value,
) -> Result<OwnedSettings, String> {
    use sha2::Digest;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let bytes = serde_json::to_vec(generated).map_err(|e| e.to_string())?;
    if bytes.len() > 64 * 1024 {
        return Err("generated Agent settings exceed limit".into());
    }
    let metadata = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(settings)
    {
        Ok(mut output) => {
            use std::io::Write;
            output
                .write_all(&bytes)
                .and_then(|()| output.sync_all())
                .map_err(|e| e.to_string())?;
            output.metadata().map_err(|e| e.to_string())?
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(settings)
                .map_err(|e| e.to_string())?;
            let meta = file.metadata().map_err(|e| e.to_string())?;
            if !meta.is_file()
                || meta.nlink() != 1
                || meta.uid() != unsafe { libc::geteuid() }
                || meta.mode() & 0o077 != 0
                || meta.len() != bytes.len() as u64
            {
                return Err("private Agent settings identity changed".into());
            }
            let mut existing = Vec::new();
            file.take(64 * 1024 + 1)
                .read_to_end(&mut existing)
                .map_err(|e| e.to_string())?;
            if existing != bytes {
                return Err("private Agent settings conflict".into());
            }
            meta
        }
        Err(error) => return Err(format!("private Agent settings unavailable: {error}")),
    };
    Ok(OwnedSettings {
        device: metadata.dev(),
        inode: metadata.ino(),
        digest: sha2::Sha256::digest(&bytes).into(),
    })
}

/// The `-c` override that gives a hand-started Codex in this shell the CodeSense server.
pub(crate) const CODEX_MCP_ENV: &str = "CODESENSE_CODEX_MCP";

/// Ready a remote shell tab for a `claude` or `codex` typed in it: the shell-init
/// functions add the session's MCP configuration (already exported), and Codex gets the
/// CodeSense server as one `-c` override. Status and inbox hooks come with the host's
/// observer when the CLI is typed (`--activity-run`). Without a CodeSense bridge in the
/// configuration there is nothing to add.
pub(crate) fn prepare_shell(spec: &mut PtySpec, config: &Path) -> Result<(), String> {
    let projection: serde_json::Value = serde_json::from_slice(
        &std::fs::read(config).map_err(|e| format!("session MCP config unavailable: {e}"))?,
    )
    .map_err(|e| format!("invalid session MCP config: {e}"))?;
    let server = &projection["mcpServers"][codesense_host_proto::reverse_mcp::CODESENSE_SERVER];
    if !server["command"].is_string() {
        return Ok(());
    }
    let codex = format!(
        "mcp_servers.{}={{command={},args={}}}",
        codesense_host_proto::reverse_mcp::CODESENSE_SERVER,
        server["command"],
        server["args"]
    );
    spec.command.env(CODEX_MCP_ENV, codex);
    Ok(())
}

/// The account's current login PATH, read in the spec's directory with its
/// login shell, including runtimes installed after the daemon started. The
/// script is fixed trusted code; neither cwd nor caller text is shell input.
fn login_path(spec: &PtySpec) -> Result<(&Path, String), String> {
    let cwd = spec
        .command
        .get_cwd()
        .ok_or("native launch requires an existing remote directory")?;
    let shell = spec
        .command
        .get_argv()
        .first()
        .ok_or("remote login shell unavailable")?;
    let mut login = Command::new(shell);
    login
        .args(["-lc", "printf '\\n'; /usr/bin/printenv PATH"])
        .env_clear()
        .envs(spec.command.iter_full_env_as_str())
        .current_dir(cwd);
    let output = bounded_output(&mut login, "remote login PATH")?;
    let path = output
        .lines()
        .rev()
        .find(|line| !line.is_empty())
        .ok_or("remote login PATH unavailable")?;
    Ok((Path::new(cwd), path.to_owned()))
}

/// Whether `launch`'s CLI is on the account's login PATH, without starting it.
/// A shell is always there.
pub(crate) fn runtime_installed(spec: &PtySpec, launch: NativeLaunch) -> Result<bool, String> {
    let Some(cli) = launch.cli() else {
        return Ok(true);
    };
    let binary = &descriptor(cli).binary;
    let (_, path) = login_path(spec)?;
    Ok(resolve_binary(&path, binary).is_some())
}

fn resolve_binary(path: &str, binary: &str) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(binary))
        .find(|candidate| codesense_pty_core::shell::is_executable_file(candidate))
}

/// The argv that hands `cli` the session's MCP configuration: the file at `config`
/// through its config flag, or, for a CLI that keeps its own home, one override that
/// switches off everything its inventory listed (`inherited`) and adds the projection's
/// servers.
fn arguments(
    descriptor: &Descriptor,
    config: &Path,
    projection: &serde_json::Value,
    inherited: &McpListing,
) -> Result<Vec<String>, String> {
    let scope = descriptor
        .mcp_scope
        .as_ref()
        .ok_or("this runtime takes no session MCP configuration")?;
    if let Some(inventory) = &scope.inventory {
        // Reuse the bridge projection's server map. Only its command and argv cross the
        // connection, never the desktop server's command, environment or secrets.
        let servers = projection["mcpServers"]
            .as_object()
            .ok_or("session MCP server map missing")?;
        if inherited
            .keys()
            .chain(&inventory.builtin)
            .any(|id| servers.contains_key(id))
        {
            return Err(format!("selected MCP server collides with existing remote {} configuration; rename or remove that remote registration before launching", descriptor.name));
        }
        // The CLI splits the override KEY on dots without TOML unquoting. Put quoted
        // server keys inside one TOML VALUE instead, preserving exact IDs (including
        // dots). Tables merge with account config, so every inherited registration must
        // still be explicitly disabled.
        let mut entries = inventory.disabled(inherited)?;
        for (id, server) in servers {
            let key = serde_json::to_string(id).map_err(|e| e.to_string())?;
            let command = server
                .get("command")
                .ok_or("invalid bridge configuration")?;
            let args = server.get("args").ok_or("invalid bridge configuration")?;
            entries.push(format!("{key}={{command={command},args={args}}}"));
        }
        return Ok(inventory.override_argv(&scope.server_map_key, &entries, str::to_owned));
    }
    match &scope.injection {
        ScopeInjection::ConfigFlag { flag_template, .. } => Ok(render_scope_flags(
            flag_template,
            config.to_str().ok_or("invalid config path")?,
        )),
        ScopeInjection::EnvDir { .. } => {
            Err("this runtime takes no session MCP configuration file".into())
        }
        ScopeInjection::EnvContent { .. } => {
            Err("this runtime's MCP config requires local environment content".into())
        }
    }
}

struct Probe {
    child: Child,
    reaped: bool,
}
impl Probe {
    fn exited(&self) -> Result<bool, String> {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: the child is still owned and unreaped. Observe its status without
        // releasing its PID, so the process group cannot be reused before cleanup.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.child.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(unsafe { info.si_pid() } != 0)
    }
    fn finish(&mut self) -> std::io::Result<std::process::ExitStatus> {
        // SAFETY: this probe owns this unreaped leader and its fresh process group.
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        // Never signal this numeric group again, including after wait reaps the leader.
        self.reaped = true;
        self.child.wait()
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.finish();
        }
    }
}
/// What a preflight probe has to answer in, unless its adapter's version budget is longer.
const PROBE_DEADLINE: Duration = Duration::from_secs(3);

/// Lets a test answer this thread's probes without starting a process, and read the
/// deadline each was given.
#[cfg(test)]
mod probe_stub {
    use std::{cell::RefCell, time::Duration};

    type Answer = Box<dyn Fn(&str) -> String>;
    thread_local! {
        static STUB: RefCell<Option<(Answer, Vec<(String, Duration)>)>> = const { RefCell::new(None) };
    }

    /// Answer the calling thread's probes with `answer(label)` from now on.
    pub(super) fn install(answer: impl Fn(&str) -> String + 'static) {
        STUB.with(|stub| *stub.borrow_mut() = Some((Box::new(answer), Vec::new())));
    }

    /// The `(label, deadline)` of every probe answered since [`install`].
    pub(super) fn deadlines() -> Vec<(String, Duration)> {
        STUB.with(|stub| {
            stub.borrow_mut()
                .take()
                .map(|(_, log)| log)
                .unwrap_or_default()
        })
    }

    pub(super) fn answer(label: &str, deadline: Duration) -> Option<String> {
        STUB.with(|stub| {
            let mut stub = stub.borrow_mut();
            let (answer, log) = stub.as_mut()?;
            log.push((label.to_owned(), deadline));
            Some(answer(label))
        })
    }
}

/// What `--help` and `--version` of `probe`'s CLI have to answer in.
fn probe_deadline(probe: &codesense_pty_core::agent_cli::Probe) -> Duration {
    probe.version_budget.max(PROBE_DEADLINE)
}

fn bounded_output(command: &mut Command, label: &str) -> Result<String, String> {
    bounded_output_within(command, label, PROBE_DEADLINE)
}

fn bounded_output_within(
    command: &mut Command,
    label: &str,
    budget: Duration,
) -> Result<String, String> {
    #[cfg(test)]
    if let Some(answer) = probe_stub::answer(label, budget) {
        return Ok(answer);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = Probe {
        child: command
            .spawn()
            .map_err(|e| format!("{label} unavailable: {e}"))?,
        reaped: false,
    };
    let mut output = child
        .child
        .stdout
        .take()
        .ok_or("runtime probe output unavailable")?;
    // SAFETY: F_GETFL/F_SETFL operate on this live, owned pipe descriptor.
    unsafe {
        let flags = libc::fcntl(output.as_raw_fd(), libc::F_GETFL);
        if flags < 0 || libc::fcntl(output.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) < 0
        {
            return Err("runtime probe output unavailable".into());
        }
    }
    let deadline = Instant::now() + budget;
    let mut bytes = Vec::new();
    loop {
        let mut buffer = [0; 8192];
        loop {
            match output.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    if bytes.len() + n > 64 * 1024 {
                        return Err(format!("{label} output exceeds limit"));
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(format!("{label} probe failed: {e}")),
            }
        }
        if child.exited()? {
            let status = child.finish().map_err(|e| e.to_string())?;
            if !status.success() {
                return Err(format!(
                    "{label} probe failed; check the remote runtime and login environment"
                ));
            }
            // The pipe was drained before checking completion; drain final buffered bytes.
            loop {
                match output.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        if bytes.len() + n > 64 * 1024 {
                            return Err(format!("{label} output exceeds limit"));
                        }
                        bytes.extend_from_slice(&buffer[..n]);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e.to_string()),
                }
            }
            return String::from_utf8(bytes).map_err(|_| format!("{label} output is not UTF8"));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{label} preflight timed out; check the remote login environment"
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Tells the launch `spec` whether its CLI's release qualified (its hooks name their main
/// turn), and never what its own environment claimed.
pub(crate) fn set_release_qualified(spec: &mut PtySpec, qualified: Option<bool>) {
    spec.command
        .env_remove(codesense_pty_core::activity::RELEASE_QUALIFIED_ENV);
    if let Some(qualified) = qualified {
        spec.command.env(
            codesense_pty_core::activity::RELEASE_QUALIFIED_ENV,
            if qualified { "1" } else { "0" },
        );
    }
}

/// Transfer a known unsupported release to its bound activity, for local and remote launches.
pub(crate) fn report_inbox_update(
    spec: &PtySpec,
    activity: &codesense_pty_core::activity::ActivityOwner,
    token: &str,
) {
    if spec
        .command
        .get_env(codesense_pty_core::activity::RELEASE_QUALIFIED_ENV)
        == Some(std::ffi::OsStr::new("0"))
    {
        activity.require_inbox_update(token, None);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use codesense_pty_core::agent_cli::AgentCli;

    /// The CLI `spec` launches, as its program, its own options and its operands, and its
    /// engine's arguments when it is spawned on one, through the helper.
    fn launched(spec: &PtySpec) -> (Vec<String>, Option<Vec<String>>) {
        let argv: Vec<std::ffi::OsString> = spec.command.get_argv().to_vec();
        let Some(launch) = codesense_pty_core::activity::launch_of(&argv) else {
            let argv = argv
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect();
            return (argv, None);
        };
        let mut own = vec![launch.program];
        own.extend(launch.standalone);
        own.extend(launch.operands);
        (own, Some(launch.engine))
    }

    pub(crate) fn fixture(
        launch: NativeLaunch,
        help: &str,
    ) -> (tempfile::TempDir, PtySpec, PathBuf) {
        fixture_answering(launch, help, "codex-cli 0.159.3", "[]")
    }

    /// [`fixture`] with a CLI whose `--version` answers `version` and whose MCP
    /// inventory lists `inventory`.
    pub(crate) fn fixture_answering(
        launch: NativeLaunch,
        help: &str,
        version: &str,
        inventory: &str,
    ) -> (tempfile::TempDir, PtySpec, PathBuf) {
        use codesense_pty_core::spawn::{HostEnvironment, SpawnIntent};
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let shell = bin.join("login-shell");
        std::fs::write(
            &shell,
            format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", bin.display()),
        )
        .unwrap();
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o700)).unwrap();
        let binary = bin.join(&descriptor(launch.cli().unwrap()).binary);
        std::fs::write(&binary, format!("#!/bin/sh\nif [ \"$1\" = --help ]; then printf '%s\\n' '{help}'; elif [ \"$1\" = --version ]; then printf '{version}'; elif [ \"$1\" = --no-daemon ] && [ \"$2\" = mcp ]; then printf '%s' '{inventory}'; else printf '%s' \"$PWD\" > effect; fi\n")).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = root.path().join("private.json");
        let projection = codesense_host_proto::reverse_mcp::server_configuration(
            "/bridge",
            "/socket",
            vec!["desktop sentinel".into()],
        )
        .unwrap();
        std::fs::write(&config, serde_json::to_vec(&projection).unwrap()).unwrap();
        let intent = SpawnIntent {
            remote_launch: Default::default(),
            command: None,
            argv: None,
            shell: None,
            cwd: root.path().to_str().unwrap().into(),
            cols: 80,
            rows: 24,
            shell_integration: false,
            starship_preset: None,
            api_port: None,
            env_remove: vec![],
            env: vec![],
            agent_env: Vec::new(),
            login_path: None,
        };
        let environment = HostEnvironment::with_vars(
            None,
            None,
            [
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("HOME".into(), root.path().to_str().unwrap().into()),
            ],
        );
        let mut spec = intent.resolve("controlled-native-test", &environment);
        spec.command.get_argv_mut()[0] = shell.into_os_string();
        (root, spec, config)
    }

    #[test]
    fn cx2_saved_configuration_selectors_reach_remote_mcp_inventory() {
        let (root, mut spec, config) =
            fixture(NativeLaunch::Cli(AgentCli::Codex), "--no-daemon --config");
        let binary = root.path().join("bin/codex");
        std::fs::write(
            &binary,
            r#"#!/bin/sh
case "$1" in
--help) echo '--no-daemon --config';;
--version) echo 'codex-cli 0.162.1';;
--profile) [ "$2" = selected ] || exit 1
printf '[{"name":"profile-only"}]';;
--no-daemon) printf '[]';;
*) exit 1;;
esac
"#,
        )
        .unwrap();
        let startup = codesense_host_proto::reverse_mcp::AgentStartup {
            args: vec![
                "--profile".into(),
                "selected".into(),
                "--model".into(),
                "saved-model".into(),
            ],
            settings: serde_json::json!({"approval_policy":"on-request", "sandbox_mode":"workspace-write"}),
            prompt: Some("work".into()),
            env: Default::default(),
        };
        prepare_agent(
            &mut spec,
            NativeLaunch::Cli(AgentCli::Codex),
            &config,
            Some(&startup),
            "profile-selection",
        )
        .unwrap();
        let (argv, engine) = launched(&spec);
        assert!(argv
            .windows(2)
            .any(|pair| pair == ["--model", "saved-model"]));
        assert!(
            engine
                .unwrap()
                .iter()
                .any(|arg| arg.starts_with("mcp_servers=") && arg.contains("profile-only")),
            "profile inventory must be scoped too"
        );
    }

    #[test]
    fn bounded_probe_refuses_timeout_and_oversized_output_and_retires_owned_helpers() {
        for oversized in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let effect = root.path().join("helper-effect");
            let identity = root.path().join("leader-pid");
            let mut command = Command::new("/bin/sh");
            command.args(["-c", if oversized {
                "printf '%s' $$ > \"$2\"; (sleep 0.3; printf leaked > \"$1\") & /usr/bin/head -c 65537 /dev/zero; sleep 10"
            } else {
                "printf '%s' $$ > \"$2\"; (sleep 3.5; printf leaked > \"$1\") & sleep 10"
            }, "probe"]).arg(&effect).arg(&identity);
            let started = Instant::now();
            let error = bounded_output(&mut command, "bounded fixture").unwrap_err();
            assert!(
                error.contains(if oversized {
                    "output exceeds limit"
                } else {
                    "preflight timed out"
                }),
                "{error}"
            );
            assert!(started.elapsed() < Duration::from_secs(5));
            if !oversized {
                assert!(started.elapsed() >= Duration::from_secs(3));
            }
            let leader: i32 = std::fs::read_to_string(identity).unwrap().parse().unwrap();
            let mut status = 0;
            // The owned leader has been reaped by cleanup, not left running/zombied.
            assert_eq!(
                unsafe { libc::waitpid(leader, &mut status, libc::WNOHANG) },
                -1
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ECHILD)
            );
            std::thread::sleep(Duration::from_millis(700));
            assert!(!effect.exists(), "owned helper survived probe refusal");
        }
    }

    #[test]
    fn successful_probe_retires_helpers_before_reaping_its_group_leader() {
        let root = tempfile::tempdir().unwrap();
        let effect = root.path().join("helper-effect");
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                "(sleep 0.3; printf leaked > \"$1\") & printf complete",
                "probe",
            ])
            .arg(&effect);
        assert_eq!(
            bounded_output(&mut command, "controlled probe").unwrap(),
            "complete"
        );
        std::thread::sleep(Duration::from_millis(500));
        assert!(!effect.exists());
    }

    #[test]
    fn successful_probe_keeps_leader_unreaped_until_group_cleanup() {
        let child = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .process_group(0)
            .spawn()
            .unwrap();
        let mut probe = Probe {
            child,
            reaped: false,
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        while !probe.exited().unwrap() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        // A second observation still sees our child: observation did not reap it.
        assert!(probe.exited().unwrap());
        assert!(probe.finish().unwrap().success());
        assert!(probe.reaped);
        assert!(probe.exited().is_err());
        // Drop observes reaped=true and must never signal a recyclable numeric PGID.
    }

    #[test]
    fn saved_agent_native_settings_prompt_hooks_and_retry_preserve_remote_account() {
        use codesense_host_proto::reverse_mcp::AgentStartup;
        for launch in [
            NativeLaunch::Cli(AgentCli::Claude),
            NativeLaunch::Cli(AgentCli::Codex),
        ] {
            let (root, mut spec, config) = fixture(
                launch,
                "--mcp-config --strict-mcp-config --settings --no-daemon --config",
            );
            let shell = spec.command.get_argv()[0].clone();
            let startup = AgentStartup {
                args: vec!["--model".into(), "saved-model".into()],
                settings: if launch == NativeLaunch::Cli(AgentCli::Claude) {
                    serde_json::json!({"permissions":{"defaultMode":"acceptEdits"}})
                } else {
                    serde_json::json!({"approval_policy":"on-request","sandbox_mode":"workspace-write"})
                },
                prompt: Some("--this-is-a-prompt".into()),
                env: Default::default(),
            };
            let owned =
                prepare_agent(&mut spec, launch, &config, Some(&startup), "test-session").unwrap();
            let (argv, engine) = launched(&spec);
            assert_eq!(&argv[argv.len() - 2..], &["--", "--this-is-a-prompt"]);
            assert!(argv
                .windows(2)
                .any(|pair| pair == ["--model", "saved-model"]));
            assert_eq!(
                spec.command.get_env("HOME").unwrap(),
                root.path().as_os_str()
            );
            assert!(spec.command.get_env("CODEX_HOME").is_none());
            assert!(
                !root.path().join("effect").exists(),
                "preflight must not run the agent"
            );
            if launch == NativeLaunch::Cli(AgentCli::Claude) {
                assert!(engine.is_none(), "Claude runs on no engine");
                let settings = config.with_extension("settings.json");
                let value: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
                assert_eq!(value["permissions"], startup.settings["permissions"]);
                assert_eq!(
                    argv.iter()
                        .filter(|arg| arg.as_str() == "--settings")
                        .count(),
                    1
                );
                let stop_groups = value["hooks"]["Stop"].as_array().unwrap();
                assert_eq!(
                    stop_groups
                        .iter()
                        .filter(
                            |group| group["hooks"].as_array().unwrap().iter().any(|hook| {
                                hook["command"].as_str().is_some_and(|command| {
                                    command.ends_with("--activity-hook claude")
                                })
                            })
                        )
                        .count(),
                    1,
                    "same generated settings carry one observer"
                );
                // The inbox waiter, a rewake hook that outlasts an idle wait, arms at its
                // boot, every turn end and after each tool call: the tab host's helper, as
                // on every host.
                for event in adapter(AgentCli::Claude).inbox().events() {
                    let waiter = value["hooks"][event]
                        .as_array()
                        .unwrap()
                        .iter()
                        .flat_map(|group| group["hooks"].as_array().unwrap())
                        .find(|hook| {
                            hook["command"]
                                == codesense_pty_core::agent_cli::inbox_hook_command(
                                    AgentCli::Claude,
                                    event,
                                )
                        })
                        .unwrap_or_else(|| panic!("{event} arms the waiter"));
                    assert_eq!(waiter["asyncRewake"], true, "{event}");
                }
                spec.command.get_argv_mut()[0] = shell.clone();
                prepare_agent(&mut spec, launch, &config, Some(&startup), "test-session").unwrap();
                std::fs::write(&settings, b"foreign").unwrap();
                spec.command.get_argv_mut()[0] = shell;
                assert!(
                    prepare_agent(&mut spec, launch, &config, Some(&startup), "test-session")
                        .unwrap_err()
                        .contains("identity changed")
                );
                owned
                    .as_ref()
                    .unwrap()
                    .remove_if_unchanged(&config)
                    .unwrap();
                assert_eq!(std::fs::read(&settings).unwrap(), b"foreign");
                std::fs::write(&settings, serde_json::to_vec(&value).unwrap()).unwrap();
                owned.unwrap().remove_if_unchanged(&config).unwrap();
                assert!(!settings.exists());
            } else {
                assert!(argv
                    .iter()
                    .any(|arg| arg == "approval_policy=\"on-request\""));
                assert!(argv.iter().any(|arg| arg.contains("--activity-hook codex")));
                // Its inbox checks ride beside the observer, each the tab host's helper.
                for event in adapter(AgentCli::Codex).inbox().events() {
                    assert!(
                        argv.iter()
                            .any(|arg| arg.starts_with(&format!("hooks.{event}="))
                                && arg.contains(&format!("--inbox-wait codex {event}"))),
                        "{event}"
                    );
                }
                assert!(argv
                    .iter()
                    .any(|arg| arg == "sandbox_mode=\"workspace-write\""));
                // Its engine, which runs its hooks, takes the same config, and serves its TUI.
                let engine = engine.expect("Codex runs on its engine");
                assert!(engine
                    .iter()
                    .any(|arg| arg.contains("--activity-hook codex")));
                assert!(engine
                    .iter()
                    .any(|arg| arg == "approval_policy=\"on-request\""));
                assert!(engine.windows(2).any(|pair| pair[0] == "app-server"));
            }
        }
    }

    /// A saved Claude's settings arrive with the Agent's own hook already on every
    /// lifecycle event (the `codesense-notify-settings` essential). The inbox waiter and
    /// the activity observer are added beside it, once per event, and are not the first
    /// group: whoever runs only `hooks[event][0]` misses them.
    #[test]
    fn saved_agent_settings_that_already_wire_an_event_still_get_the_waiter_and_observer() {
        use codesense_host_proto::reverse_mcp::AgentStartup;
        let (_root, mut spec, config) = fixture(
            NativeLaunch::Cli(AgentCli::Claude),
            "--mcp-config --strict-mcp-config --settings",
        );
        let notify = serde_json::json!([{"matcher":"","hooks":[{"type":"command","command":codesense_pty_core::notify_hook::status_command(),"timeout":5}]}]);
        let events = [
            "SessionStart",
            "Stop",
            "StopFailure",
            "PermissionRequest",
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "PostToolUseFailure",
            "SessionEnd",
        ];
        let mut settings =
            serde_json::json!({"permissions":{"defaultMode":"acceptEdits"},"hooks":{}});
        for event in events {
            settings["hooks"][event] = notify.clone();
        }
        let startup = AgentStartup {
            args: vec![],
            settings,
            prompt: None,
            env: Default::default(),
        };
        prepare_agent(
            &mut spec,
            NativeLaunch::Cli(AgentCli::Claude),
            &config,
            Some(&startup),
            "test-session",
        )
        .unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(config.with_extension("settings.json")).unwrap())
                .unwrap();
        for event in events {
            let commands: Vec<&str> = value["hooks"][event]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|group| group["hooks"].as_array().unwrap())
                .filter_map(|hook| hook["command"].as_str())
                .collect();
            assert_eq!(commands[0], notify[0]["hooks"][0]["command"], "{event}");
            // Only the inbox waiter's events run the waiter; status is the host's
            // activity observer's, on every event.
            assert_eq!(
                commands
                    .iter()
                    .filter(|command| **command
                        == codesense_pty_core::agent_cli::inbox_hook_command(
                            AgentCli::Claude,
                            event
                        ))
                    .count(),
                usize::from(
                    adapter(AgentCli::Claude).inbox().events().contains(&event)
                        || adapter(AgentCli::Claude).confirms_notice(event)
                ),
                "{event}"
            );
            assert_eq!(
                commands
                    .iter()
                    .filter(|command| command.contains("--activity-hook claude"))
                    .count(),
                1,
                "{event} is observed exactly once"
            );
        }
    }

    pub(crate) fn shell_spec(root: &Path) -> PtySpec {
        use codesense_pty_core::spawn::{HostEnvironment, SpawnIntent};
        SpawnIntent {
            remote_launch: Default::default(),
            command: None,
            argv: None,
            shell: None,
            cwd: root.to_str().unwrap().into(),
            cols: 80,
            rows: 24,
            shell_integration: true,
            starship_preset: None,
            api_port: None,
            env_remove: vec![],
            env: vec![],
            agent_env: Vec::new(),
            login_path: None,
        }
        .resolve(
            "shell-tab",
            &HostEnvironment::with_vars(None, None, [("PATH".into(), "/usr/bin:/bin".into())]),
        )
    }

    #[test]
    fn a_shell_tab_gets_the_codex_server_and_nothing_written() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("mcp-session-abc.json");
        let projection = codesense_host_proto::reverse_mcp::server_configuration(
            "/bridge path",
            "/socket",
            vec!["codesense".into()],
        )
        .unwrap();
        std::fs::write(&config, serde_json::to_vec(&projection).unwrap()).unwrap();
        let mut spec = shell_spec(root.path());
        prepare_shell(&mut spec, &config).unwrap();
        // Codex: one `-c` value with the bridge and its socket.
        assert_eq!(
            spec.command
                .get_env(CODEX_MCP_ENV)
                .map(|value| value.to_string_lossy().into_owned())
                .as_deref(),
            Some(
                r#"mcp_servers.codesense={command="/bridge path",args=["--remote-socket","/socket"]}"#
            )
        );
        // A typed Claude's status and inbox hooks come with the host's observer: no
        // settings file goes with the shell.
        assert!(!root.path().join("mcp-session-abc.settings.json").exists());
    }

    #[test]
    fn telemetry_points_each_cli_at_the_receiver_under_its_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut claude = shell_spec(dir.path());
        assert!(telemetry(
            &mut claude,
            descriptor(AgentCli::Claude),
            Some(4567),
            "sess-1",
            None
        )
        .is_empty());
        assert_eq!(
            claude
                .command
                .get_env("OTEL_EXPORTER_OTLP_ENDPOINT")
                .unwrap(),
            "http://127.0.0.1:4567/api/v1/telemetry/sess-1"
        );
        assert_eq!(
            claude
                .command
                .get_env("CLAUDE_CODE_ENABLE_TELEMETRY")
                .unwrap(),
            "1"
        );

        let mut codex = shell_spec(dir.path());
        let argv = telemetry(
            &mut codex,
            descriptor(AgentCli::Codex),
            Some(4567),
            "sess-2",
            None,
        );
        assert!(argv
            .iter()
            .any(|arg| arg.contains("http://127.0.0.1:4567/api/v1/telemetry/sess-2/v1/logs")));
    }

    #[test]
    fn telemetry_keeps_a_variable_the_launch_already_sets_and_is_withheld_without_a_receiver() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = shell_spec(dir.path());
        spec.command.env("CLAUDE_CODE_ENABLE_TELEMETRY", "0");
        telemetry(
            &mut spec,
            descriptor(AgentCli::Claude),
            Some(4567),
            "sess-1",
            None,
        );
        assert_eq!(
            spec.command
                .get_env("CLAUDE_CODE_ENABLE_TELEMETRY")
                .unwrap(),
            "0"
        );
        // A saved Agent's own variable of the same name loses to the export.
        let agent_env = [("CLAUDE_CODE_ENABLE_TELEMETRY".to_string(), "0".to_string())].into();
        telemetry(
            &mut spec,
            descriptor(AgentCli::Claude),
            Some(4567),
            "sess-1",
            Some(&agent_env),
        );
        assert_eq!(
            spec.command
                .get_env("CLAUDE_CODE_ENABLE_TELEMETRY")
                .unwrap(),
            "1"
        );

        let mut none = shell_spec(dir.path());
        assert!(telemetry(&mut none, descriptor(AgentCli::Codex), None, "sess-1", None).is_empty());
        assert!(none
            .command
            .get_env("OTEL_EXPORTER_OTLP_ENDPOINT")
            .is_none());
        assert!(telemetry(
            &mut none,
            descriptor(AgentCli::Codex),
            Some(4567),
            "bad/session",
            None
        )
        .is_empty());
    }

    #[test]
    fn a_shell_without_a_codesense_bridge_gets_nothing() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("mcp-session-abc.json");
        std::fs::write(&config, br#"{"mcpServers":{}}"#).unwrap();
        let mut spec = shell_spec(root.path());
        prepare_shell(&mut spec, &config).unwrap();
        assert!(spec.command.get_env(CODEX_MCP_ENV).is_none());
        // An unreadable configuration is an error the caller logs; the shell still opens.
        assert!(prepare_shell(&mut spec, &root.path().join("missing.json")).is_err());
    }

    /// A saved Agent may pass Claude's isolation flags remotely. CodeSense's
    /// own scope (its bridge config and its one settings file) still loads beside them.
    #[test]
    fn a_saved_agent_with_isolation_flags_keeps_codesense_scope_remotely() {
        use codesense_host_proto::reverse_mcp::AgentStartup;
        let (_root, mut spec, config) = fixture(
            NativeLaunch::Cli(AgentCli::Claude),
            "--mcp-config --strict-mcp-config --settings",
        );
        let startup = AgentStartup {
            args: vec![
                "--setting-sources=local".into(),
                "--strict-mcp-config".into(),
                "--model".into(),
                "saved-model".into(),
            ],
            settings: serde_json::json!({"permissions":{"defaultMode":"acceptEdits"}}),
            prompt: None,
            env: Default::default(),
        };
        prepare_agent(
            &mut spec,
            NativeLaunch::Cli(AgentCli::Claude),
            &config,
            Some(&startup),
            "test-session",
        )
        .unwrap();
        let argv: Vec<String> = spec
            .command
            .get_argv()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        let config_arg = config.to_str().unwrap();
        assert!(argv
            .windows(2)
            .any(|pair| pair[0] == "--mcp-config" && pair[1] == config_arg));
        let settings_path = config.with_extension("settings.json");
        assert_eq!(
            argv.iter()
                .filter(|arg| arg.as_str() == "--settings")
                .count(),
            1
        );
        let at = argv.iter().position(|arg| arg == "--settings").unwrap();
        assert_eq!(argv[at + 1], settings_path.to_str().unwrap());
        // The agent's flags pass through after CodeSense's.
        assert!(argv.iter().any(|arg| arg == "--setting-sources=local"));
        assert!(argv
            .windows(2)
            .any(|pair| pair == ["--model", "saved-model"]));
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&settings_path).unwrap()).unwrap();
        assert_eq!(value["permissions"]["defaultMode"], "acceptEdits");
        assert!(value["hooks"]["Stop"]
            .as_array()
            .is_some_and(|hooks| !hooks.is_empty()));
    }

    #[test]
    fn a_plain_claude_carries_its_hooks_inline_and_writes_nothing() {
        let (_root, mut spec, config) = fixture(
            NativeLaunch::Cli(AgentCli::Claude),
            "--mcp-config --strict-mcp-config --settings",
        );
        let owned = prepare_agent(
            &mut spec,
            NativeLaunch::Cli(AgentCli::Claude),
            &config,
            None,
            "test-session",
        )
        .unwrap();
        assert!(owned.is_none());
        assert!(!config.with_extension("settings.json").exists());
        let argv: Vec<_> = spec
            .command
            .get_argv()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        let at = argv.iter().position(|arg| arg == "--settings").unwrap();
        assert_eq!(
            argv.iter()
                .filter(|arg| arg.as_str() == "--settings")
                .count(),
            1
        );
        let value: serde_json::Value = serde_json::from_str(&argv[at + 1]).unwrap();
        // It reports its status and hears about its inbox like a saved Agent does.
        for event in adapter(AgentCli::Claude).inbox().events() {
            assert!(
                value["hooks"][event]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|group| group["hooks"][0]["command"]
                        == codesense_pty_core::agent_cli::inbox_hook_command(
                            AgentCli::Claude,
                            event
                        )),
                "{event}"
            );
        }
        assert!(value["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .any(|group| group["hooks"][0]["command"]
                .as_str()
                .is_some_and(|command| command.ends_with("--activity-hook claude"))));
    }

    #[test]
    fn native_preflight_resolves_new_login_path_and_process_keeps_requested_cwd() {
        for launch in [
            NativeLaunch::Cli(AgentCli::Claude),
            NativeLaunch::Cli(AgentCli::Codex),
        ] {
            let (root, mut spec, config) = fixture(
                launch,
                "--mcp-config --strict-mcp-config --no-daemon --config",
            );
            codesense_pty_core::agent_cli::version_cache::known_release_for_test(
                &root.path().join("bin").join(launch.cli().unwrap().as_str()),
                "codex-cli 0.159.3",
            );
            prepare(&mut spec, launch, &config, "test-session").unwrap();
            let (argv, _) = launched(&spec);
            assert_eq!(
                Path::new(&argv[0]),
                root.path()
                    .join("bin")
                    .join(&descriptor(launch.cli().unwrap()).binary)
            );
            let status = Command::new(&argv[0])
                .args(&argv[1..])
                .env_clear()
                .envs(spec.command.iter_full_env_as_str())
                .current_dir(spec.command.get_cwd().unwrap())
                .status()
                .unwrap();
            assert!(status.success());
            assert_eq!(
                std::fs::read_to_string(root.path().join("effect")).unwrap(),
                root.path().canonicalize().unwrap().to_str().unwrap()
            );
            assert_eq!(
                spec.command.get_env("HOME").unwrap(),
                root.path().as_os_str()
            );
            assert!(spec.command.get_env("CODEX_HOME").is_none());
        }
    }

    #[test]
    fn runtime_probe_reads_the_login_path_without_starting_the_cli() {
        for launch in [
            NativeLaunch::Cli(AgentCli::Claude),
            NativeLaunch::Cli(AgentCli::Codex),
        ] {
            let (root, spec, _) = fixture(launch, "");
            assert_eq!(runtime_installed(&spec, launch), Ok(true));
            assert!(
                !root.path().join("effect").exists(),
                "the probe started the CLI"
            );
            std::fs::remove_file(
                root.path()
                    .join("bin")
                    .join(&descriptor(launch.cli().unwrap()).binary),
            )
            .unwrap();
            assert_eq!(runtime_installed(&spec, launch), Ok(false));
        }
        let (_root, spec, _) = fixture(NativeLaunch::Cli(AgentCli::Claude), "");
        assert_eq!(runtime_installed(&spec, NativeLaunch::Shell), Ok(true));
    }

    #[test]
    fn native_preflight_refuses_missing_runtime_and_unsupported_configuration() {
        let (root, mut spec, config) = fixture(NativeLaunch::Cli(AgentCli::Codex), "--config");
        assert!(prepare(
            &mut spec,
            NativeLaunch::Cli(AgentCli::Codex),
            &config,
            "test-session"
        )
        .unwrap_err()
        .contains("--no-daemon"));
        std::fs::remove_file(root.path().join("bin/codex")).unwrap();
        assert!(prepare(
            &mut spec,
            NativeLaunch::Cli(AgentCli::Codex),
            &config,
            "test-session"
        )
        .unwrap_err()
        .contains("install it"));
        assert!(!root.path().join("effect").exists());
    }

    #[test]
    fn a_saved_agent_refuses_a_cli_without_its_settings_flag_but_a_plain_launch_runs() {
        use codesense_host_proto::reverse_mcp::AgentStartup;
        let (root, mut spec, config) = fixture(
            NativeLaunch::Cli(AgentCli::Claude),
            "--mcp-config --strict-mcp-config",
        );
        let startup = AgentStartup {
            args: vec![],
            settings: serde_json::json!({}),
            prompt: None,
            env: Default::default(),
        };
        let error = prepare_agent(
            &mut spec,
            NativeLaunch::Cli(AgentCli::Claude),
            &config,
            Some(&startup),
            "test-session",
        )
        .unwrap_err();
        assert!(
            error.contains("saved-Agent settings support (--settings)"),
            "{error}"
        );
        assert!(!root.path().join("effect").exists());
        prepare(
            &mut spec,
            NativeLaunch::Cli(AgentCli::Claude),
            &config,
            "test-session",
        )
        .unwrap();
        // A plain launch of a CLI that takes no settings is given none.
        assert!(!spec
            .command
            .get_argv()
            .iter()
            .any(|arg| arg.to_string_lossy().starts_with("--settings")));
    }

    /// What the remote account's own Codex lists is switched off in the launch's one
    /// override; an inventory that names a server twice is refused.
    #[test]
    fn a_remote_codex_switches_off_what_its_inventory_lists() {
        let (_root, mut spec, config) = fixture_answering(
            NativeLaunch::Cli(AgentCli::Codex),
            "--no-daemon --config",
            "codex-cli 0.159.3",
            r#"[{"name":"ambient.dot","transport":{"type":"stdio","command":"/bin/ambient","args":["--api-key","SECRET"]}}]"#,
        );
        prepare(
            &mut spec,
            NativeLaunch::Cli(AgentCli::Codex),
            &config,
            "test-session",
        )
        .unwrap();
        // Its MCP servers are engine config: they ride its engine.
        let engine = launched(&spec).1.unwrap();
        let servers = engine
            .iter()
            .find(|arg| arg.starts_with("mcp_servers="))
            .unwrap();
        assert!(
            servers.contains(r#""ambient.dot"={enabled=false,command="/usr/bin/false"}"#),
            "{servers}"
        );
        assert!(
            !servers.contains("SECRET") && !servers.contains("/bin/ambient"),
            "{servers}"
        );
        let (_root, mut spec, config) = fixture_answering(
            NativeLaunch::Cli(AgentCli::Codex),
            "--no-daemon --config",
            "codex-cli 0.159.3",
            r#"[{"name":"a"},{"name":"a"}]"#,
        );
        let error = prepare(
            &mut spec,
            NativeLaunch::Cli(AgentCli::Codex),
            &config,
            "test-session",
        )
        .unwrap_err();
        assert!(error.contains("duplicate registrations"), "{error}");
    }

    /// A saved Codex Agent's keys reach a Codex outside the observed releases too, as
    /// overrides ahead of its own args, with no observer.
    #[test]
    fn a_saved_codex_agent_on_an_unobserved_codex_keeps_its_overrides() {
        use codesense_host_proto::reverse_mcp::AgentStartup;
        let (_root, mut spec, config) = fixture_answering(
            NativeLaunch::Cli(AgentCli::Codex),
            "--no-daemon --config",
            "codex-cli 0.158.0",
            "[]",
        );
        let startup = AgentStartup {
            args: vec!["--model".into(), "saved".into()],
            settings: serde_json::json!({"sandbox_mode":"read-only","approval_policy":"never"}),
            prompt: None,
            env: Default::default(),
        };
        prepare_agent(
            &mut spec,
            NativeLaunch::Cli(AgentCli::Codex),
            &config,
            Some(&startup),
            "test-session",
        )
        .unwrap();
        let argv: Vec<_> = spec
            .command
            .get_argv()
            .iter()
            .skip(1)
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            argv[3..],
            [
                "-c",
                "approval_policy=\"never\"",
                "-c",
                "sandbox_mode=\"read-only\"",
                "--model",
                "saved"
            ]
        );
        assert!(!argv.iter().any(|arg| arg.starts_with("hooks.")));
        assert_eq!(
            spec.command
                .get_env(codesense_pty_core::activity::RELEASE_QUALIFIED_ENV),
            Some(std::ffi::OsStr::new("0"))
        );
    }

    /// A cold Hermes takes seconds to print `chat --help` and `--version`, so both are
    /// given its adapter's budget; every other CLI keeps the shared deadline. Probes are
    /// answered by the test, so the deadline each was handed is read, never waited for.
    #[test]
    fn help_and_version_probes_are_given_the_adapters_budget() {
        for cli in AgentCli::every_cli() {
            let launch = NativeLaunch::Cli(cli);
            let adapter = adapter(cli);
            let version = adapter.descriptor().release.minimum_output();
            let (root, mut spec, config) = fixture_answering(launch, "", &version, "[]");
            let bin = root.path().join("bin").display().to_string();
            probe_stub::install(move |label| {
                if label == "remote login PATH" {
                    format!("\n{bin}\n")
                } else if label.ends_with("activity version") {
                    version.clone()
                } else if label.ends_with("MCP configuration inventory") {
                    "[]".to_owned()
                } else {
                    "--mcp-config --strict-mcp-config --no-daemon --config --tui --query --yolo --settings --standalone --format json --remote --hook -m".to_owned()
                }
            });
            let _ = prepare(&mut spec, launch, &config, "s");
            let given = probe_stub::deadlines();
            let probe = adapter.probe();
            let expected = probe.version_budget.max(PROBE_DEADLINE);
            let binary = adapter.descriptor().binary.as_str();
            let deadline_of = |label: &str| {
                given
                    .iter()
                    .find(|(given, _)| given == label)
                    .map(|(_, deadline)| *deadline)
            };
            assert_eq!(deadline_of(binary), Some(expected), "{cli}: --help");
            if matches!(probe.activity, ActivityProbe::Version) {
                assert_eq!(
                    deadline_of(&format!("{binary} activity version")),
                    Some(expected),
                    "{cli}: --version"
                );
            }
        }
        assert!(adapter(AgentCli::Hermes).probe().version_budget > PROBE_DEADLINE);
        assert_eq!(
            adapter(AgentCli::Codex)
                .probe()
                .version_budget
                .max(PROBE_DEADLINE),
            PROBE_DEADLINE
        );
    }

    #[test]
    fn prepared_script_identity_reaches_native_qualification_without_spawning_version() {
        use codesense_pty_core::agent_cli::version_cache::{self, LockedRuntime, RuntimeIdentity};
        let (root, mut spec, config) =
            fixture_answering(NativeLaunch::Cli(AgentCli::Hermes), "-q --yolo", "", "[]");
        let root = root.path().canonicalize().unwrap();
        let binary = root.join("bin/hermes");
        let interpreter = root.join("python");
        std::fs::copy("/bin/sh", &interpreter).unwrap();
        // macOS refuses a relocated platform binary until it has a local signature.
        #[cfg(target_os = "macos")]
        assert!(std::process::Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(&interpreter)
            .output()
            .unwrap()
            .status
            .success());
        std::fs::write(&binary, format!("#!{}\ncase \"$*\" in *--help*) echo '-q --yolo';; *--version*) touch '{}'; exit 1;; esac\n", interpreter.display(), root.join("version-ran").display())).unwrap();
        // Preparation has already executed the real interpreter before warming its cache.
        // Validate the relocated signed stub here too, outside the host's probe deadline.
        let help = Command::new(&binary)
            .args(["chat", "--help"])
            .output()
            .unwrap();
        assert!(help.status.success());
        assert_eq!(String::from_utf8(help.stdout).unwrap().trim(), "-q --yolo");
        let identity =
            RuntimeIdentity::from_prepared_install((0, 22, 5), "0123456789ab", &root).unwrap();
        let runtime = root.join(identity.key());
        std::fs::create_dir(&runtime).unwrap();
        let home = root.join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("config.yaml"), "{}\n").unwrap();
        {
            let locked = LockedRuntime::acquire(&runtime, identity).unwrap();
            std::fs::write(runtime.join("READY"), "manifest").unwrap();
            version_cache::write(
                &binary,
                "Hermes Agent v0.22.5 local 0123456789ab",
                adapter(AgentCli::Hermes).probe(),
                Some(locked.identity()),
            )
            .unwrap();
            locked.bind_home(&home).unwrap();
        }
        spec.command.env("HERMES_HOME", &home);
        let reason = prepare(
            &mut spec,
            NativeLaunch::Cli(AgentCli::Hermes),
            &config,
            "prepared",
        )
        .expect_err("EnvDir remains unsupported on the remote native path");
        assert!(
            reason.contains("this runtime takes no session MCP configuration file"),
            "{reason}"
        );
        assert_eq!(
            spec.command
                .get_env(codesense_pty_core::activity::RELEASE_QUALIFIED_ENV),
            Some(std::ffi::OsStr::new("1"))
        );
        assert!(!root.join("version-ran").exists());
    }

    fn dispatcher_qualification_in_context_b(version_a: &str, version_b: &str, expected: &str) {
        let (root, mut spec, config) = fixture_answering(
            NativeLaunch::Cli(AgentCli::Codex),
            "--no-daemon --config",
            "",
            "[]",
        );
        let a = root.path().join("project-a");
        let b = root.path().join("project-b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        std::fs::write(a.join("selected-version"), format!("{version_a}\n")).unwrap();
        std::fs::write(b.join("selected-version"), format!("{version_b}\n")).unwrap();
        let binary = root.path().join("bin/codex");
        std::fs::write(
            &binary,
            r#"#!/bin/sh
case "$1" in
--help) echo '--no-daemon --config';;
--version) read -r selected < selected-version; printf 'codex-cli %s\n' "$selected";;
--no-daemon) printf '[]';;
esac
"#,
        )
        .unwrap();
        let identity_before = std::fs::canonicalize(&binary).unwrap();
        let modified_before = std::fs::metadata(&binary).unwrap().modified().unwrap();
        let warmed = Command::new(&binary)
            .arg("--version")
            .current_dir(&a)
            .output()
            .unwrap();
        assert!(warmed.status.success());
        let raw = String::from_utf8(warmed.stdout).unwrap();
        codesense_pty_core::agent_cli::version_cache::seed_for_test(&binary, &raw).unwrap();
        spec.command.cwd(&b);
        prepare(
            &mut spec,
            NativeLaunch::Cli(AgentCli::Codex),
            &config,
            "context-b",
        )
        .unwrap();
        assert_eq!(std::fs::canonicalize(&binary).unwrap(), identity_before);
        assert_eq!(
            std::fs::metadata(&binary).unwrap().modified().unwrap(),
            modified_before
        );
        assert_eq!(
            spec.command
                .get_env(codesense_pty_core::activity::RELEASE_QUALIFIED_ENV),
            Some(std::ffi::OsStr::new(expected))
        );
    }

    #[test]
    fn unchanged_dispatcher_qualified_a_to_unqualified_b_uses_launch_context() {
        dispatcher_qualification_in_context_b("0.162.1", "0.158.0", "0");
    }

    #[test]
    fn unchanged_dispatcher_unqualified_a_to_qualified_b_uses_launch_context() {
        dispatcher_qualification_in_context_b("0.158.0", "0.162.1", "1");
    }

    #[test]
    fn native_release_qualification_ignores_poisoned_legacy_cache() {
        let (root, mut spec, config) = fixture_answering(
            NativeLaunch::Cli(AgentCli::Codex),
            "--no-daemon --config",
            "codex-cli 0.158.0",
            "[]",
        );
        let binary = root.path().join("bin/codex");
        codesense_pty_core::agent_cli::version_cache::seed_for_test(&binary, "codex-cli 0.162.1")
            .unwrap();
        prepare(
            &mut spec,
            NativeLaunch::Cli(AgentCli::Codex),
            &config,
            "test-session",
        )
        .unwrap();
        assert_eq!(
            spec.command
                .get_env(codesense_pty_core::activity::RELEASE_QUALIFIED_ENV),
            Some(std::ffi::OsStr::new("0"))
        );
    }

    #[test]
    fn native_codex_preflight_reports_only_known_old_releases_to_its_activity() {
        for (version, warning) in [
            (Some("codex-cli 0.158.0"), true),
            (Some("codex-cli 0.162.1"), false),
            (None, false),
        ] {
            let (root, mut spec, config) = fixture_answering(
                NativeLaunch::Cli(AgentCli::Codex),
                "--no-daemon --config",
                version.unwrap_or(""),
                "[]",
            );
            if version.is_none() {
                std::fs::write(root.path().join("bin/codex"),
                    "#!/bin/sh\ncase \"$1\" in\n--help) echo '--no-daemon --config';;\n--version) exit 1;;\n*) printf '[]';;\nesac\n").unwrap();
            }
            prepare(
                &mut spec,
                NativeLaunch::Cli(AgentCli::Codex),
                &config,
                "test-session",
            )
            .unwrap();
            let activity = codesense_pty_core::activity::ActivityOwner::new();
            let writer = activity
                .prepare_native("test-session", AgentCli::Codex, "unused")
                .unwrap();
            report_inbox_update(&spec, &activity, &writer.token);
            let snapshot = activity.snapshot("test-session").unwrap();
            assert_eq!(
                snapshot.reason.as_deref(),
                warning.then_some("inbox-update-required")
            );
            assert!(!snapshot.observed);
            assert!(snapshot.exit.is_none(), "the launch remains allowed");
        }
    }

    #[test]
    fn native_preflight_refuses_invalid_directory_and_unreadable_session_configuration() {
        let (root, mut spec, config) = fixture(
            NativeLaunch::Cli(AgentCli::Claude),
            "--mcp-config --strict-mcp-config",
        );
        spec.command.clear_cwd();
        assert!(prepare(
            &mut spec,
            NativeLaunch::Cli(AgentCli::Claude),
            &config,
            "test-session"
        )
        .unwrap_err()
        .contains("existing remote directory"));
        spec.command.cwd(root.path());
        std::fs::write(&config, b"invalid").unwrap();
        assert!(prepare(
            &mut spec,
            NativeLaunch::Cli(AgentCli::Claude),
            &config,
            "test-session"
        )
        .unwrap_err()
        .contains("invalid session MCP config"));
        assert!(!root.path().join("effect").exists());
    }

    #[test]
    fn codex_disables_inherited_inventory_and_refuses_selected_collisions() {
        let config = Path::new("/private/session.json");
        let projection =
            codesense_host_proto::reverse_mcp::bridge_configuration("/bridge", "/socket");
        let args = arguments(
            descriptor(AgentCli::Codex),
            config,
            &projection,
            &[
                ("unselected".into(), McpTransportKind::Http),
                ("ambient.dot".into(), McpTransportKind::Stdio),
            ]
            .into(),
        )
        .unwrap();
        assert_eq!(&args[..2], ["--no-daemon", "-c"]);
        assert_eq!(args.len(), 3);
        assert!(args[2].contains("\"unselected\"={enabled=false,url=\"http://127.0.0.1:9\"}"));
        assert!(args[2].contains("\"ambient.dot\"={enabled=false,command=\"/usr/bin/false\"}"));
        assert!(args[2].contains("codex_apps={enabled=false,url=\"http://127.0.0.1:9\"}"));
        let inherited_apps = arguments(
            descriptor(AgentCli::Codex),
            config,
            &projection,
            &[("codex_apps".into(), McpTransportKind::Http)].into(),
        )
        .unwrap();
        assert!(inherited_apps[2].contains("codex_apps={enabled=false,url=\"http://127.0.0.1:9\"}"));
        assert!(arguments(
            descriptor(AgentCli::Codex),
            config,
            &projection,
            &[("codesense".into(), McpTransportKind::Http)].into()
        )
        .unwrap_err()
        .contains("collides"));
    }

    #[test]
    #[ignore = "requires explicit installed Codex binary; inventory only, no MCP spawn or authentication"]
    fn installed_codex_accepts_exact_projection_names_and_disables_ambient() {
        let binary = std::env::var("CODESENSE_TEST_CODEX_BINARY").unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("config.toml"),
            "[mcp_servers.\"ambient.dot\"]\ncommand=\"/bin/false\"\n[mcp_servers.ambient_http]\nurl=\"http://127.0.0.1:9\"\n").unwrap();
        let projection = codesense_host_proto::reverse_mcp::server_configuration(
            "/bin/false",
            "/fixture/socket",
            vec!["selected.dot".into(), "remote-62726f77736572".into()],
        )
        .unwrap();
        let arguments = arguments(
            descriptor(AgentCli::Codex),
            Path::new("/unused"),
            &projection,
            &[
                ("ambient.dot".into(), McpTransportKind::Stdio),
                ("ambient_http".into(), McpTransportKind::Http),
            ]
            .into(),
        )
        .unwrap();
        let mut command = Command::new(binary);
        command
            .args(arguments)
            .args(["mcp", "list", "--json"])
            .env("CODEX_HOME", root.path());
        let output = bounded_output(&mut command, "installed Codex inventory").unwrap();
        let inventory: serde_json::Value = serde_json::from_str(&output).unwrap();
        let entries = inventory.as_array().unwrap();
        assert_eq!(entries.len(), 6);
        for (name, enabled) in [
            ("codesense", true),
            ("ambient.dot", false),
            ("ambient_http", false),
            ("codex_apps", false),
            ("selected.dot", true),
            ("remote-62726f77736572", true),
        ] {
            let entry = entries.iter().find(|entry| entry["name"] == name).unwrap();
            assert_eq!(entry["enabled"], enabled, "{name}");
        }
    }

    #[test]
    fn providers_use_private_bridge_projection_without_account_override() {
        let config = Path::new("/private/session config.json");
        let projection = codesense_host_proto::reverse_mcp::server_configuration(
            "/installed/bridge",
            "/private/socket",
            vec!["desktop sentinel".into()],
        )
        .unwrap();
        assert_eq!(
            arguments(
                descriptor(AgentCli::Claude),
                config,
                &projection,
                &[].into()
            )
            .unwrap(),
            [
                "--mcp-config",
                "/private/session config.json",
                "--strict-mcp-config"
            ]
        );
        let codex =
            arguments(descriptor(AgentCli::Codex), config, &projection, &[].into()).unwrap();
        assert_eq!(&codex[..2], ["--no-daemon", "-c"]);
        assert_eq!(codex.len(), 3);
        assert!(codex[2].starts_with("mcp_servers={"));
        assert!(codex[2].contains("\"desktop sentinel\"={command=\"/installed/bridge\",args=[\"--remote-socket\",\"/private/socket\",\"--server-id\",\"desktop sentinel\"]}"));
        assert!(!codex.join(" ").contains("auth"));
    }
}
