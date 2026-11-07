// SPDX-License-Identifier: MIT
//! `codesense-session-server` — the long-lived PTY session-server sidecar.
//!
//! The CodeSense app launches this binary detached from its own session with
//! `--root <app-data root>` (and `--resources <dir>`, the resources it ships:
//! the shell-integration script and starship; `--emulator <worker>`, its
//! terminal emulator worker) and talks to it over
//! `<root>/pty-server.sock`. The
//! sidecar hosts the same `codesense-pty-core` runtime the app otherwise hosts
//! in-process — spawn, reader, 16 ms batcher, and the source-of-truth replay
//! buffer — so a running session outlives the app: quitting CodeSense drops the
//! socket client, while the child keeps running and keeps draining into the
//! sidecar's buffer.
//!
//! The sidecar is never a hard dependency: when it is absent or unreachable the
//! app hosts the runtime in-process. Diagnostics go to stderr, which the app
//! points at `<root>/logs/pty-server.log`.
//!
//! `codesense-session-server attach --stdio` is the same binary as a relay: it
//! pipes its stdio to the host of this machine, launching one when none runs,
//! so a client reaches a remote host over `ssh <host> … attach --stdio`.
//!
//! With `CODESENSE_ASKPASS_SOCK` set it is `ssh`'s askpass program instead,
//! answering a key's passphrase prompt from that socket (see `askpass`).

use codesense_pty_core::layout::ServerIdentity;
use codesense_pty_core::spawn::HostEnvironment;
use codesense_session_server::host::{emulator_worker_beside, Host};
#[cfg(target_os = "linux")]
use codesense_session_server::replace;
use codesense_session_server::{askpass, attach, detach, exit_with, server};

/// Writes `log` records (the runtime's and ours) to stderr.
struct StderrLog;

impl log::Log for StderrLog {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!(
                "codesense-session-server [{}] {}: {}",
                record.level(),
                record.target(),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}

static LOGGER: StderrLog = StderrLog;

fn main() {
    if codesense_pty_core::activity::run_helper() {
        return;
    }
    #[cfg(target_os = "linux")]
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--mcp-stdio-supervisor"))
    {
        let args: Vec<_> = std::env::args_os().skip(2).collect();
        if let Err(error) = codesense_session_server::reverse_mcp::stdio_supervisor(&args) {
            eprintln!("remote MCP supervisor: {error}");
            std::process::exit(1);
        }
        return;
    }

    #[cfg(target_os = "linux")]
    if std::env::args_os().nth(1).as_deref()
        == Some(std::ffi::OsStr::new("--mcp-session-bootstrap"))
    {
        let args: Vec<_> = std::env::args_os().skip(2).collect();
        if let Err(error) = codesense_session_server::reverse_mcp::bootstrap(&args) {
            eprintln!("reverse MCP bootstrap: {error}");
            std::process::exit(1);
        }
        return;
    }
    // `ssh` runs this binary as its SSH_ASKPASS, with the prompt as the only
    // argument (not always valid text) and the passphrase's socket in the
    // environment.
    if let Some(socket) = std::env::var_os(askpass::SOCKET_ENV) {
        let prompt: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
        std::process::exit(askpass::run_client(&prompt, std::path::Path::new(&socket)));
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(target_os = "linux")]
    if args.first().map(String::as_str) == Some("private-account") {
        std::process::exit(
            match codesense_session_server::private_account_native::run(&args[1..]) {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("{error}");
                    1
                }
            },
        );
    }
    if args.first().map(String::as_str) == Some("attach") {
        std::process::exit(match relay(&args[1..]) {
            Ok(()) => 0,
            Err(err) => {
                eprintln!("codesense-session-server attach: {err}");
                1
            }
        });
    }
    #[cfg(target_os = "linux")]
    if args.first().map(String::as_str) == Some("replace") {
        if args.len() != 1 {
            eprintln!("usage: codesense-session-server replace");
            std::process::exit(2);
        }
        std::process::exit(
            match replace::Authorization::read_stdin()
                .and_then(|authorization| replace::authorized(&authorization))
            {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("codesense-session-server replace: {error}");
                    1
                }
            },
        );
    }
    // Only the long-lived host raises its budget. Relays and exec wrappers
    // preserve the incoming limit for the host or user command they launch;
    // its PTY children get it back through this binary's shim (`run_helper`).
    codesense_pty_core::nofile::raise(10_240);
    codesense_pty_core::nofile::serve_child_limit();
    #[cfg(target_os = "linux")]
    let private = if args.first().map(String::as_str) == Some("private-serve") {
        match codesense_session_server::private_account_native::prepare_serve(&args[1..]) {
            Ok(root) => Some(root),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
    } else {
        None
    };
    let Some(root) = server::path_arg(args.iter().cloned(), "--root") else {
        eprintln!(
            "usage: codesense-session-server [--resources <dir>] [--emulator <worker>] --root <app-data root>"
        );
        eprintln!("{}", attach::USAGE);
        std::process::exit(1);
    };
    let logical_root = root.clone();
    let resources = server::path_arg(args.iter().cloned(), "--resources");
    #[cfg(target_os = "linux")]
    let (root, resources) = match &private {
        Some(private) => (private.filesystem_root(), Some(private.resources.clone())),
        None => (root, resources),
    };

    // Bind first, so the launcher returns only once the socket listens (or a
    // sidecar already serves this instance), then detach before any thread
    // exists: the detach forks, and the forked server keeps the listener and
    // the singleton lock.
    let bound = match server::bind(&root) {
        Ok(bound) => bound,
        Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => {
            // Another sidecar already serves this instance; the app uses it.
            std::process::exit(0);
        }
        Err(err) => {
            eprintln!("codesense-session-server: bind failed under {root:?}: {err}");
            std::process::exit(1);
        }
    };
    if let Err(err) = detach::detach() {
        eprintln!("codesense-session-server: {err}");
        std::process::exit(1);
    }
    if let Err(err) = bound.record_pid() {
        eprintln!("codesense-session-server: record pid failed: {err}");
    }
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
    // Still single-threaded: take the opt-in lifetime tie out of the
    // environment before any thread (or hosted session) could observe it.
    if let Some(pid) = exit_with::take_from_env() {
        exit_with::exit_when_gone(pid);
    }
    // After the detach's fork (which would not carry the thread) and the
    // single-threaded environment edit above. Only the app and this server
    // run the open-file limit monitor. The bridge is deliberately not
    // watched: its stderr is the MCP client's error stream, which a warning
    // would pollute, and it holds only its stdio and one socket, so it cannot
    // plausibly approach its limit.
    codesense_pty_core::nofile::watch("codesense-session-server", |line| {
        log::warn!(target: "nofile", "{line}")
    });

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("pty-server-rt")
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("codesense-session-server: runtime failed: {err}");
            std::process::exit(1);
        }
    };

    log::info!(
        "pid {} serving instance {:?}",
        std::process::id(),
        bound.instance_id
    );
    // The instance's one terminal-server identity, persisted under its root.
    let identity = ServerIdentity::load_or_ephemeral(&root, ServerIdentity::local_host_name());
    // Spawns resolve against this process's environment: the app's, as it
    // launched the sidecar, under the instance's CodeSense home (the root).
    let environment = HostEnvironment::of_process(Some(logical_root.clone()), resources);
    #[cfg(target_os = "linux")]
    let host = Host::new_replaceable(
        bound.instance_id.clone(),
        identity,
        environment,
        runtime.handle().clone(),
        root.clone(),
    );
    #[cfg(not(target_os = "linux"))]
    let host = Host::new(
        bound.instance_id.clone(),
        identity,
        environment,
        runtime.handle().clone(),
    );
    #[cfg(target_os = "linux")]
    if let Some(private) = &private {
        host.enable_private_workspaces(private);
    }
    if std::env::var_os("CODESENSE_LOCAL_WAKE_HOLDS").is_some() {
        host.require_wake_holds();
    }
    if let Err(error) = host.start_activity(&root) {
        log::error!("activity reporting unavailable: {error}");
    }
    if let Err(error) = host.start_wake(&root) {
        log::error!("session wakes and timers unavailable: {error}");
    }
    if let Err(error) = host.start_arms(&root) {
        log::error!("machine-armed routines unavailable: {error}");
    }
    // The app names the worker it ships; a host installed on another machine
    // finds its own beside its binary.
    let emulator = server::path_arg(args.iter().cloned(), "--emulator").map_or_else(
        || std::env::current_exe().map(|binary| emulator_worker_beside(&binary)),
        Ok,
    );
    match emulator {
        Ok(worker) => host.start_emulator(worker, &root),
        Err(error) => log::error!("serving without an emulator worker: own binary: {error}"),
    }
    #[cfg(target_os = "linux")]
    if private.is_some() {
        if let Err(error) = host.start_reverse_mcp(&logical_root) {
            log::error!("reverse MCP unavailable: {error}");
        }
    }
    // The loopback receiver for the agent CLIs' usage export; the Mac pulls what it spools.
    let usage = root.join("usage");
    match codesense_session_server::usage_receiver::start(&usage) {
        Ok(receiver) => {
            log::info!("usage receiver on 127.0.0.1:{}", receiver.port);
            codesense_session_server::usage_receiver::advertise(receiver);
        }
        Err(error) => log::error!("usage receiver unavailable: {error}"),
    }
    server::serve(&bound.listener, host, &root);
}

/// `attach`: relay stdio to this machine's host, launching this binary as that
/// host when none serves the instance. Every failure exits 1 — an install's
/// check that the binary runs on its machine relies on that.
fn relay(args: &[String]) -> Result<(), String> {
    let binary = std::env::current_exe().map_err(|e| format!("own binary: {e}"))?;
    let home = dirs::home_dir();
    let target = attach::Target::from_args(args, home.as_deref(), &binary)?;
    attach::run(&target, &binary)
}
