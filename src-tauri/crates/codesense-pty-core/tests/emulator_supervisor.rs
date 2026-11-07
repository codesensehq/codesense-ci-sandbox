// SPDX-License-Identifier: MIT
//! The emulator worker supervisor against the real bundled worker
//! (`scripts/build-emulator.sh` builds it into `src-tauri/binaries/`, or
//! `CODESENSE_EMULATOR_WORKER` names one) and against shell stubs that answer
//! `hello` with chosen versions.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use codesense_pty_core::domain::{PortUnavailable, TerminalDomainEvent, TerminalEventSink};
use codesense_pty_core::emulator::{
    EmulatorConfig, EmulatorError, EmulatorLink, EmulatorStatus, EmulatorSupervisor,
    SessionEmulator, WORKER_BIN,
};
use codesense_pty_core::layout::ServerIdentity;
use codesense_pty_core::TerminalHost;

const START: Duration = Duration::from_secs(15);

/// The Rust triple of the machine running the tests, as the build scripts name
/// artifacts.
fn host_triple() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        other => panic!("no emulator worker is built for {other:?}"),
    }
}

fn bundled_worker() -> PathBuf {
    if let Some(path) = std::env::var_os("CODESENSE_EMULATOR_WORKER") {
        return PathBuf::from(path);
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../binaries")
        .join(format!("{WORKER_BIN}-{}", host_triple()));
    assert!(
        path.is_file(),
        "no emulator worker at {} — build it with `pnpm run build:emulator`",
        path.display()
    );
    path
}

/// The `@xterm/headless` version the root package pins.
fn pinned_xterm_version() -> String {
    let manifest = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../package.json"),
    )
    .unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    manifest["dependencies"]["@xterm/headless"]
        .as_str()
        .expect("@xterm/headless is a root dependency")
        .to_string()
}

/// Every log line the supervisors in this test binary write.
fn log_lines() -> &'static Mutex<Vec<String>> {
    static LINES: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    static LOGGER: Capture = Capture;
    let lines = LINES.get_or_init(|| {
        let _ = log::set_logger(&LOGGER);
        log::set_max_level(log::LevelFilter::Info);
        Mutex::default()
    });
    struct Capture;
    impl log::Log for Capture {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }
        fn log(&self, record: &log::Record<'_>) {
            log_lines()
                .lock()
                .unwrap()
                .push(format!("{} {}", record.level(), record.args()));
        }
        fn flush(&self) {}
    }
    lines
}

fn lines_matching(needle: &str) -> Vec<String> {
    log_lines()
        .lock()
        .unwrap()
        .iter()
        .filter(|line| line.contains(needle))
        .cloned()
        .collect()
}

fn config(worker: &Path, dir: &Path) -> EmulatorConfig {
    EmulatorConfig::new(worker.to_path_buf(), dir.join("runtime"))
}

fn running(status: &EmulatorStatus) -> (u32, String) {
    match status {
        EmulatorStatus::Running { pid, info } => (*pid, info.xterm_version.clone()),
        other => panic!("worker not running: {other:?}"),
    }
}

fn kill(pid: u32) {
    // SAFETY: signalling a child this test's supervisor started.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) }, 0);
}

/// The executable a running process was started from (its argv[0]).
fn command_of(pid: u32) -> String {
    let out = std::process::Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A worker stand-in: answers `hello` with `xterm_version` and `wire`, then
/// holds stdin open like the real worker. Each run appends a line to `runs`.
fn write_stub(path: &Path, runs: &Path, versions: &[(&str, u32)]) {
    write_stub_then(path, runs, versions, "exec cat > /dev/null");
}

/// [`write_stub`] that runs `then` after answering instead of holding stdin.
fn write_stub_then(path: &Path, runs: &Path, versions: &[(&str, u32)], then: &str) {
    let replies: Vec<String> = versions
        .iter()
        .map(|(xterm, wire)| {
            let body = format!(
                r#"{{"id":0,"type":"hello","runtime":"stub","version":"0","xterm_version":"{xterm}","worker_wire_version":{wire}}}"#
            );
            let header: String = (body.len() as u32)
                .to_be_bytes()
                .iter()
                .map(|b| format!("\\{b:03o}"))
                .collect();
            format!("{header}{body}")
        })
        .collect();
    // The n-th run answers with the n-th reply, the last one from then on.
    let mut script = String::from("#!/bin/sh\n");
    script += &format!("echo run >> '{}'\n", runs.display());
    script += &format!("n=$(wc -l < '{}')\n", runs.display());
    for (i, reply) in replies.iter().enumerate() {
        let test = if i + 1 == replies.len() {
            "true".to_string()
        } else {
            format!("[ \"$n\" -le {} ]", i + 1)
        };
        let keyword = if i == 0 { "if" } else { "elif" };
        script += &format!("{keyword} {test}; then printf '{reply}'\n");
    }
    script += &format!("fi\n{then}\n");
    let staging = path.with_extension("tmp");
    let mut file = std::fs::File::create(&staging).unwrap();
    file.write_all(script.as_bytes()).unwrap();
    drop(file);
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Replace the file the way an app update does: a new file under the old name.
    std::fs::rename(&staging, path).unwrap();
}

fn runs(path: &Path) -> usize {
    std::fs::read_to_string(path).map_or(0, |text| text.lines().count())
}

#[test]
fn spawn_hello_roundtrip() {
    log_lines();
    let dir = tempfile::tempdir().unwrap();
    let supervisor =
        EmulatorSupervisor::start(config(&bundled_worker(), dir.path()), EmulatorLink::new())
            .unwrap();
    let EmulatorStatus::Running { pid, info } = supervisor.status() else {
        panic!("worker not running: {:?}", supervisor.status());
    };
    assert_eq!(info.runtime, "bun");
    assert!(!info.version.is_empty());
    assert_eq!(info.xterm_version, pinned_xterm_version());
    assert_eq!(info.worker_wire_version, 2);
    assert_eq!(
        lines_matching(&format!("pid={pid}")).len(),
        1,
        "one start log line"
    );
    assert!(
        lines_matching(&format!("pid={pid}"))[0].contains("emulator worker started: runtime=bun")
    );
}

#[test]
fn restarts_after_kill() {
    log_lines();
    let dir = tempfile::tempdir().unwrap();
    let supervisor =
        EmulatorSupervisor::start(config(&bundled_worker(), dir.path()), EmulatorLink::new())
            .unwrap();
    let (first, version) = running(&supervisor.status());
    assert_eq!(
        command_of(first),
        supervisor.worker_path().display().to_string()
    );

    for _ in 0..2 {
        let (before, _) = running(&supervisor.status());
        let killed = Instant::now();
        kill(before);
        let status = supervisor
            .wait_for(
                START,
                |s| matches!(s, EmulatorStatus::Running { pid, .. } if *pid != before),
            )
            .expect("worker restarted");
        let restarted_in = killed.elapsed();
        assert!(
            restarted_in < Duration::from_secs(1),
            "restart took {restarted_in:?}"
        );
        let (after, restarted_version) = running(&status);
        assert_eq!(restarted_version, version);
        assert_eq!(
            command_of(after),
            supervisor.worker_path().display().to_string(),
            "restarted from the pinned path"
        );
        let started = lines_matching("started: runtime=bun")
            .into_iter()
            .filter(|line| line.ends_with(&format!("pid={after}")))
            .count();
        assert_eq!(started, 1, "one start log line per restart");
    }
}

#[test]
fn refuses_version_skew_on_restart() {
    log_lines();
    for (skewed, reason) in [
        (("9.9.9", 1), "xterm_version 9.9.9"),
        (("6.0.0", 2), "worker_wire_version 2"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (worker, runs_file) = (dir.path().join("worker"), dir.path().join("runs"));
        write_stub(&worker, &runs_file, &[("6.0.0", 1), skewed]);
        let supervisor =
            EmulatorSupervisor::start(config(&worker, dir.path()), EmulatorLink::new()).unwrap();
        let (pid, _) = running(&supervisor.status());
        kill(pid);
        let status = supervisor
            .wait_for(START, |s| matches!(s, EmulatorStatus::Refused { .. }))
            .expect("skewed worker refused");
        let EmulatorStatus::Refused { reason: refused } = status else {
            unreachable!()
        };
        assert!(refused.contains(reason), "{refused}");
        assert!(lines_matching("emulator worker refused")
            .iter()
            .any(|line| line.starts_with("ERROR") && line.contains(reason)));
        // No crash loop: nothing runs the worker again.
        std::thread::sleep(Duration::from_millis(1500));
        assert_eq!(runs(&runs_file), 2);
        assert!(matches!(
            supervisor.status(),
            EmulatorStatus::Refused { .. }
        ));
    }
}

#[test]
fn path_resolved_once() {
    log_lines();
    let dir = tempfile::tempdir().unwrap();
    let (worker, runs_file) = (dir.path().join("worker"), dir.path().join("runs"));
    write_stub(&worker, &runs_file, &[("6.0.0", 1)]);
    let supervisor =
        EmulatorSupervisor::start(config(&worker, dir.path()), EmulatorLink::new()).unwrap();
    let (pid, _) = running(&supervisor.status());

    // An update replaces the bundle's worker with one bundling another xterm.
    let updated_runs = dir.path().join("updated-runs");
    write_stub(&worker, &updated_runs, &[("7.0.0", 1)]);
    kill(pid);
    let status = supervisor
        .wait_for(
            START,
            |s| matches!(s, EmulatorStatus::Running { pid: p, .. } if *p != pid),
        )
        .expect("worker restarted");
    assert_eq!(running(&status).1, "6.0.0");
    assert_eq!(runs(&updated_runs), 0, "the updated file never ran");
    assert_eq!(runs(&runs_file), 2);
}

#[test]
fn missing_runtime_is_a_clear_error_and_the_host_keeps_serving() {
    log_lines();
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("no-such-worker");
    let error = EmulatorSupervisor::start(config(&missing, dir.path()), EmulatorLink::new())
        .err()
        .expect("a missing worker cannot start");
    assert!(matches!(error, EmulatorError::Missing(_)));
    assert_eq!(
        error.to_string(),
        format!("emulator worker not found at {}", missing.display())
    );

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let host = TerminalHost::new(
        ServerIdentity::load_or_ephemeral(dir.path(), "test"),
        Arc::new(NoSink),
        runtime.handle().clone(),
    );
    assert!(host.start_emulator(config(&missing, dir.path())).is_err());
    assert_eq!(host.emulator_status(), None);
    assert!(host.identity().is_ok(), "the host still serves");
}

#[test]
fn a_host_runs_one_worker() {
    log_lines();
    let dir = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let host = TerminalHost::new(
        ServerIdentity::load_or_ephemeral(dir.path(), "test"),
        Arc::new(NoSink),
        runtime.handle().clone(),
    );
    host.start_emulator(config(&bundled_worker(), dir.path()))
        .unwrap();
    assert!(matches!(
        host.emulator_status(),
        Some(EmulatorStatus::Running { .. })
    ));
    assert!(matches!(
        host.start_emulator(config(&bundled_worker(), dir.path())),
        Err(EmulatorError::AlreadyStarted)
    ));
}

#[test]
fn dropping_the_supervisor_stops_the_worker_and_removes_its_copy() {
    log_lines();
    let dir = tempfile::tempdir().unwrap();
    let supervisor =
        EmulatorSupervisor::start(config(&bundled_worker(), dir.path()), EmulatorLink::new())
            .unwrap();
    let (pid, _) = running(&supervisor.status());
    let pinned = supervisor.worker_path().to_path_buf();
    assert!(pinned.is_file());
    drop(supervisor);
    assert!(!pinned.exists());
    assert!(command_of(pid).is_empty(), "worker {pid} still running");
}

struct NoSink;

impl TerminalEventSink for NoSink {
    fn emit(&self, _: TerminalDomainEvent) -> Result<(), PortUnavailable> {
        Ok(())
    }
}

#[test]
fn a_worker_that_dies_right_after_hello_is_given_up_on() {
    log_lines();
    let dir = tempfile::tempdir().unwrap();
    let (worker, runs_file) = (dir.path().join("worker"), dir.path().join("runs"));
    write_stub_then(&worker, &runs_file, &[("6.0.0", 1)], "exit 0");
    let supervisor =
        EmulatorSupervisor::start(config(&worker, dir.path()), EmulatorLink::new()).unwrap();
    let status = supervisor
        .wait_for(Duration::from_secs(30), |s| {
            matches!(s, EmulatorStatus::Failed { .. })
        })
        .expect("the crash loop was given up on");
    let EmulatorStatus::Failed { reason } = status else {
        unreachable!()
    };
    assert!(reason.contains("exited 5 times in a row"), "{reason}");
    assert!(lines_matching("emulator worker failed")
        .iter()
        .any(|line| line.starts_with("ERROR") && line.contains("exited 5 times")));
    // One start plus four restarts, and nothing after giving up.
    assert_eq!(runs(&runs_file), 5);
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(runs(&runs_file), 5);
}

#[test]
fn a_worker_gets_a_scrubbed_environment() {
    log_lines();
    let dir = tempfile::tempdir().unwrap();
    let (worker, runs_file) = (dir.path().join("worker"), dir.path().join("runs"));
    let dump = dir.path().join("env");
    write_stub_then(
        &worker,
        &runs_file,
        &[("6.0.0", 1)],
        &format!("env > '{}'\nexec cat > /dev/null", dump.display()),
    );
    let allowed = ["PATH", "HOME", "TMPDIR", "LANG", "LC_ALL", "LC_CTYPE", "TZ"];
    // sh itself sets these.
    let shell = ["PWD", "SHLVL", "_", "OLDPWD"];
    assert!(
        std::env::vars().any(|(key, _)| !allowed.contains(&key.as_str())),
        "the test process has variables a worker must not see"
    );
    let _supervisor =
        EmulatorSupervisor::start(config(&worker, dir.path()), EmulatorLink::new()).unwrap();
    let deadline = Instant::now() + START;
    while !dump.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let env = std::fs::read_to_string(&dump).unwrap();
    for line in env.lines() {
        let key = line.split('=').next().unwrap();
        assert!(
            allowed.contains(&key) || shell.contains(&key),
            "the worker saw {key}"
        );
    }
    assert!(env.lines().any(|line| line.starts_with("PATH=")));
}

/// A worker stand-in that answers `hello` and then reads every request without
/// ever acknowledging one, supervised with a short apply deadline.
fn hung_worker(dir: &Path) -> (EmulatorSupervisor, Arc<EmulatorLink>, PathBuf) {
    let (worker, runs_file) = (dir.join("worker"), dir.join("runs"));
    write_stub(&worker, &runs_file, &[("6.0.0", 1)]);
    let mut config = config(&worker, dir);
    config.apply_deadline = Duration::from_millis(300);
    let link = EmulatorLink::new();
    let supervisor = EmulatorSupervisor::start(config, link.clone()).unwrap();
    (supervisor, link, runs_file)
}

#[test]
fn a_worker_that_stops_applying_is_killed_and_restarted() {
    log_lines();
    let dir = tempfile::tempdir().unwrap();
    let (supervisor, link, runs_file) = hung_worker(dir.path());
    let (first, _) = running(&supervisor.status());
    let mut session = SessionEmulator::start(&link, (80, 24), 100, 0);
    session.feed(5, "hello");
    let status = supervisor
        .wait_for(
            START,
            |s| matches!(s, EmulatorStatus::Running { pid, .. } if *pid != first),
        )
        .expect("the hung worker was replaced");
    let (second, _) = running(&status);
    assert_eq!(runs(&runs_file), 2);
    assert!(!process_exists(first), "the hung worker was killed");
    assert!(lines_matching("emulator worker exited")
        .iter()
        .any(|line| line.contains("no reply for") && line.contains("work outstanding")));
    // The restart dropped the unapplied feed, so the new worker owes nothing
    // and is left alone.
    std::thread::sleep(Duration::from_millis(900));
    assert_eq!(running(&supervisor.status()).0, second);
    session.dispose();
}

#[test]
fn an_idle_worker_that_never_replies_is_left_running() {
    log_lines();
    let dir = tempfile::tempdir().unwrap();
    let (supervisor, _link, runs_file) = hung_worker(dir.path());
    let (first, _) = running(&supervisor.status());
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(running(&supervisor.status()).0, first);
    assert_eq!(runs(&runs_file), 1);
}

fn process_exists(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}
