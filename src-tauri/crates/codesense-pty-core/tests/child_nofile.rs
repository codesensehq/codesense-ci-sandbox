// SPDX-License-Identifier: MIT
//! A host that raised its open-file soft limit gives its PTY children
//! the original one back. It does so in the child (through its own
//! executable's shim), never by lowering its own limit, so a host holding more
//! descriptors than the original limit still spawns, and no other thread ever
//! sees EMFILE because of a spawn.
//!
//! The limit is process-wide, so each test runs in its own process. This
//! binary has no libtest harness: like the app and the session server, its
//! entry point serves the shim first (`run_helper`), so it can be its PTY
//! children's shim too.
use codesense_pty_core::runner::{LocalPtyRunner, PtyRunner, PtySession, PtySpec};
use portable_pty::{CommandBuilder, PtySize};
use std::path::Path;
use std::time::{Duration, Instant};

const STAGE: &str = "CODESENSE_NOFILE_TEST_STAGE";

const TESTS: &[(&str, fn())] = &[
    (
        "a_pty_child_gets_the_original_open_file_limit",
        a_pty_child_gets_the_original_open_file_limit,
    ),
    (
        "app_sidecar_and_pty_preserve_the_original_limit",
        app_sidecar_and_pty_preserve_the_original_limit,
    ),
    (
        "a_host_holding_more_than_the_original_limit_still_spawns",
        a_host_holding_more_than_the_original_limit_still_spawns,
    ),
    (
        "another_thread_never_gets_emfile_during_spawns",
        another_thread_never_gets_emfile_during_spawns,
    ),
    (
        "the_shim_keeps_argv_cwd_and_environment",
        the_shim_keeps_argv_cwd_and_environment,
    ),
];

fn main() {
    if codesense_pty_core::activity::run_helper() {
        return;
    }
    if let Ok(stage) = std::env::var(STAGE) {
        // `app_sidecar_and_pty_preserve_the_original_limit`'s own sidecar.
        if stage == "sidecar" {
            return sidecar_stage();
        }
        let (_, test) = TESTS
            .iter()
            .find(|(name, _)| *name == stage)
            .unwrap_or_else(|| panic!("no test {stage}"));
        return test();
    }
    if std::env::args().any(|arg| arg == "--list") {
        for (name, _) in TESTS {
            println!("{name}: test");
        }
        return;
    }
    let filter: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| !str::starts_with(arg, '-'))
        .collect();
    let mut failed = Vec::new();
    for (name, _) in TESTS {
        let wanted = |wanted: &String| str::contains(name, String::as_str(wanted));
        if !Vec::is_empty(&filter) && !filter.iter().any(wanted) {
            continue;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .env(STAGE, name)
            .status()
            .unwrap();
        println!("test {name} ... {}", if status.success() { "ok" } else { "FAILED" });
        if !status.success() {
            failed.push(*name);
        }
    }
    assert!(Vec::is_empty(&failed), "failed: {failed:?}");
}

fn limit() -> libc::rlimit {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: valid storage.
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
        0
    );
    limit
}

fn soft_limit() -> u64 {
    #[allow(clippy::unnecessary_cast)]
    let soft = limit().rlim_cur as u64;
    soft
}

/// Start from a soft limit of 256, as launchd gives an app, and raise it and
/// serve the shim the way a host does at startup. Returns the raised limit.
fn host_from_256() -> u64 {
    let mut lowered = limit();
    lowered.rlim_cur = 256;
    // SAFETY: lowering the soft limit is always allowed.
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lowered) }, 0);
    let (original, raised) = codesense_pty_core::nofile::raise(10_240).unwrap();
    assert_eq!(original, 256);
    assert!(raised > 300, "the host raised its own limit");
    codesense_pty_core::nofile::serve_child_limit();
    raised
}

/// Run `script` in a PTY through the local runner.
fn spawn_sh(script: String, cwd: Option<&Path>) -> Result<PtySession, String> {
    let mut command = CommandBuilder::new("sh");
    command.arg("-c");
    command.arg(script);
    if let Some(cwd) = cwd {
        command.cwd(cwd);
    }
    command.env("SV_PBF_290_PROBE", "kept");
    let spec = PtySpec {
        command,
        size: PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        },
    };
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RUNTIME
        .get_or_init(|| tokio::runtime::Runtime::new().unwrap())
        .block_on(LocalPtyRunner.spawn(spec))
        .map_err(String::from)
}

fn read_when_written(out: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !Path::exists(out) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    std::fs::read_to_string(out).unwrap()
}

/// `ulimit -Sn` as the PTY child sees it.
fn child_soft_limit() -> Result<String, String> {
    let temp = tempfile::tempdir().unwrap();
    let out = Path::join(temp.path(), "limit");
    let session = spawn_sh(
        format!(
            "ulimit -Sn > '{0}.next' && mv '{0}.next' '{0}'",
            Path::display(&out)
        ),
        None,
    )?;
    let limit = read_when_written(&out);
    session.kill();
    Ok(limit.trim().to_string())
}

fn assert_pty_limit(expected: u64, raised: u64) {
    assert_eq!(child_soft_limit().unwrap(), expected.to_string());
    assert_eq!(soft_limit(), raised, "the host keeps its raised limit");
}

fn a_pty_child_gets_the_original_open_file_limit() {
    let raised = host_from_256();
    assert_pty_limit(256, raised);
}

fn app_sidecar_and_pty_preserve_the_original_limit() {
    let raised = host_from_256();
    let mut sidecar = std::process::Command::new(std::env::current_exe().unwrap());
    sidecar.env(STAGE, "sidecar");
    codesense_pty_core::nofile::inherit_original_limit(&mut sidecar);
    assert!(sidecar.status().unwrap().success(), "sidecar stage failed");
    assert_eq!(soft_limit(), raised, "app retains its raised budget");
}

fn sidecar_stage() {
    assert_eq!(soft_limit(), 256, "the sidecar starts with the original limit");
    let (_, raised) = codesense_pty_core::nofile::raise(10_240).unwrap();
    assert!(raised > 256);
    codesense_pty_core::nofile::serve_child_limit();
    assert_pty_limit(256, raised);
    assert_eq!(soft_limit(), raised, "sidecar retains its raised budget");
}

/// The 2026-10-29 failure. A host holding more descriptors than the
/// original limit spawned with that limit back in force (`with_child_limit`),
/// so the spawn's own pipe and slave dups failed: `Failed to spawn: Too many
/// open files (os error 24)`. Now the spawn runs under the raised limit.
fn a_host_holding_more_than_the_original_limit_still_spawns() {
    let raised = host_from_256();
    let held: Vec<_> = (0..300)
        .map(|_| std::fs::File::open("/dev/null").unwrap())
        .collect();
    let open = codesense_pty_core::nofile::usage().unwrap().open.unwrap();
    assert!(open > 300, "the host holds {open} descriptors, over 256");
    assert_eq!(
        child_soft_limit().expect("spawn above the original limit"),
        "256",
        "the child still starts with the original limit"
    );
    assert_eq!(soft_limit(), raised);
    drop(held);
}

/// While spawns run, another thread holding the host near the original
/// limit keeps opening descriptors. The old window lowered the whole process to
/// 256 during each fork, so that thread's opens failed with EMFILE.
fn another_thread_never_gets_emfile_during_spawns() {
    host_from_256();
    let held: Vec<_> = (0..250)
        .map(|_| std::fs::File::open("/dev/null").unwrap())
        .collect();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let opener = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut failures = Vec::new();
            let mut opens = 0u64;
            while !stop.load(std::sync::atomic::Ordering::Acquire) {
                let batch: Vec<_> = (0..20)
                    .map(|_| std::fs::File::open("/dev/null"))
                    .collect();
                for result in batch {
                    match result {
                        Ok(_) => opens += 1,
                        Err(error) => failures.push(error.to_string()),
                    }
                }
            }
            (opens, failures)
        })
    };
    for _ in 0..20 {
        assert_eq!(child_soft_limit().expect("spawn"), "256");
    }
    stop.store(true, std::sync::atomic::Ordering::Release);
    let (opens, failures) = std::thread::JoinHandle::join(opener).unwrap();
    assert!(opens > 0, "the other thread ran during the spawns");
    assert!(Vec::is_empty(&failures), "another thread failed: {failures:?}");
    drop(held);
}

/// The shim execs the command in its own place: the argv the host built (not
/// the shim's), its working directory and its environment.
fn the_shim_keeps_argv_cwd_and_environment() {
    host_from_256();
    let temp = tempfile::tempdir().unwrap();
    let out = Path::join(temp.path(), "seen");
    let session = spawn_sh(
        format!(
            "{{ ps -o args= -p $$; pwd -P; printf '%s\\n' \"$SV_PBF_290_PROBE\"; }} > '{0}.next' && mv '{0}.next' '{0}'",
            Path::display(&out)
        ),
        Some(temp.path()),
    )
    .unwrap();
    let seen = read_when_written(&out);
    session.kill();
    let lines: Vec<&str> = str::lines(&seen).collect();
    assert!(str::starts_with(lines[0], "sh -c "), "argv: {seen}");
    assert!(!str::contains(lines[0], "--nofile-exec"), "argv: {seen}");
    assert_eq!(
        Path::new(lines[1]),
        std::fs::canonicalize(temp.path()).unwrap(),
        "cwd: {seen}"
    );
    assert_eq!(lines[2], "kept", "environment: {seen}");
}
