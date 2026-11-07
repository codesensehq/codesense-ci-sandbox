// SPDX-License-Identifier: MIT
use super::*;
use crate::attribution::testing::script;
use std::sync::atomic::{AtomicUsize, Ordering};
struct Fake {
    bytes: Vec<u8>,
    calls: Arc<AtomicUsize>,
    error: bool,
}
impl Fetch for Fake {
    fn fetch<'a>(&'a self, sink: &'a mut (dyn Write + Send)) -> FetchFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            sink.write_all(&self.bytes).map_err(Failure::retryable)?;
            if self.error {
                Err(Failure::retryable("offline"))
            } else {
                Ok(())
            }
        })
    }
}
fn fake(body: &str) -> (Fake, Pin) {
    let bytes = format!("#!/bin/sh\n{body}\n").into_bytes();
    let mut pin = PIN.clone();
    pin.expected_version = "1.7.5";
    pin.sha256 = crate::shared::hash::sha256_hex(&bytes);
    (
        Fake {
            bytes,
            calls: Arc::new(AtomicUsize::new(0)),
            error: false,
        },
        pin,
    )
}
fn run(home: &Path, fetch: &dyn Fetch, pin: &Pin) -> Result<(), Failure> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(install_once(home, None, fetch, pin))
}
fn staged(home: &Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(home.join("bin"))
        .unwrap()
        .flatten()
        .map(|x| x.path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".git-ai.tmp")
        })
        .collect()
}
#[test]
fn a_staged_binary_is_invoked_as_git_ai_and_installs() {
    let home = tempfile::tempdir().unwrap();
    let executable = home.path().join("git-ai-macos-arm64");
    crate::attribution::testing::argv0_binary(&executable);
    let bytes = std::fs::read(&executable).unwrap();
    let mut pin = PIN.clone();
    pin.expected_version = "1.7.5";
    pin.sha256 = crate::shared::hash::sha256_hex(&bytes);
    let fetch = Fake {
        bytes,
        calls: Arc::new(AtomicUsize::new(0)),
        error: false,
    };
    let result = run(home.path(), &fetch, &pin);
    assert!(
        result.is_ok(),
        "staged probe must not reject git's proxy version: {result:?}"
    );
    assert_eq!(
        std::fs::read(home.path().join("bin/git-ai")).unwrap(),
        fetch.bytes
    );
    assert!(staged(home.path()).is_empty());
}

#[test]
fn pin_and_unsupported_platform_never_fetches() {
    let parsed = Pin::parse(include_str!("../../git-ai.pin")).expect("invalid bundled git-ai pin");
    assert_eq!(PIN.expected_version, parsed.expected_version);
    assert_eq!(PIN.asset, "git-ai-macos-arm64");
    assert_eq!(PIN.platform, "macos-arm64");
    assert_eq!(PIN.resource, "resources/git-ai");
    assert_eq!(PIN.sha256, parsed.sha256);
    let home = tempfile::tempdir().unwrap();
    let (fetch, pin) = fake("echo 1.7.5");
    let fetch = Arc::new(fetch);
    let service = Service::new(home.path().into(), None, fetch.clone(), pin, false);
    assert_eq!(
        service.request(Trigger::Startup),
        Some(AttributionStatus::Unsupported {
            reason: "platform".into()
        })
    );
    assert_eq!(fetch.calls.load(Ordering::SeqCst), 0);
    assert!(!home.path().join("bin/git-ai").exists());
}
#[test]
fn digest_short_and_oversize_streams_leave_no_executable_or_temp() {
    for bytes in [b"short".to_vec(), vec![0; MAX_BYTES as usize + 1]] {
        let home = tempfile::tempdir().unwrap();
        let fetch = Fake {
            bytes,
            calls: Arc::new(AtomicUsize::new(0)),
            error: false,
        };
        assert!(run(home.path(), &fetch, &PIN).is_err());
        assert!(!home.path().join("bin/git-ai").exists());
        assert!(staged(home.path()).is_empty());
    }
}
#[test]
fn failed_replacement_preserves_old_bytes_and_version_mismatch_is_sticky() {
    let home = tempfile::tempdir().unwrap();
    let old = home.path().join("bin/git-ai");
    script(&old, "echo 1.7.4");
    let before = std::fs::read(&old).unwrap();
    let (fetch, pin) = fake("echo 1.7.6");
    assert!(run(home.path(), &fetch, &pin).unwrap_err().sticky);
    assert_eq!(std::fs::read(old).unwrap(), before);
    assert!(staged(home.path()).is_empty());
    // A locally linked executable exercises the write-descriptor/exec boundary
    // without Apple's platform-signature restrictions on relocated /bin/bash.
    let source = home.path().join("version.c");
    let executable = home.path().join("native-version");
    std::fs::write(
        &source,
        "#include <stdio.h>\nint main(void) { puts(\"1.7.5\"); return 0; }\n",
    )
    .unwrap();
    let output = std::process::Command::new("cc")
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let native = std::fs::read(&executable).unwrap();
    #[cfg(target_os = "macos")]
    assert!(
        native.starts_with(&[0xca, 0xfe, 0xba, 0xbe])
            || native.starts_with(&[0xcf, 0xfa, 0xed, 0xfe])
            || native.starts_with(&[0xca, 0xfe, 0xba, 0xbf]),
        "the replay must use a native Mach-O executable, not a shebang script"
    );
    let mut pin = PIN.clone();
    pin.expected_version = "1.7.5";
    pin.sha256 = crate::shared::hash::sha256_hex(&native);
    let fetch = Fake {
        bytes: native,
        calls: Arc::new(AtomicUsize::new(0)),
        error: false,
    };
    run(home.path(), &fetch, &pin).unwrap();
    assert_eq!(
        std::fs::read(home.path().join("bin/git-ai")).unwrap(),
        fetch.bytes
    );
}
#[test]
fn reuse_path_newer_own_downgrade_and_symlink() {
    for version in ["1.7.6", "1.8.0"] {
        let home = tempfile::tempdir().unwrap();
        script(&home.path().join("bin/git-ai"), &format!("echo {version}"));
        let (fetch, pin) = fake("echo 1.7.5");
        run(home.path(), &fetch, &pin).unwrap();
        assert_eq!(fetch.calls.load(Ordering::SeqCst), 0);
    }
    let home = tempfile::tempdir().unwrap();
    let path = tempfile::tempdir().unwrap();
    script(&path.path().join("git-ai"), "echo 1.7.6");
    let (fetch, pin) = fake("echo 1.7.5");
    let paths = std::env::join_paths([path.path()]).unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(install_once(home.path(), Some(&paths), &fetch, &pin))
        .unwrap();
    assert_eq!(fetch.calls.load(Ordering::SeqCst), 1);
    std::fs::remove_file(home.path().join("bin/git-ai")).unwrap();
    std::os::unix::fs::symlink(path.path().join("git-ai"), home.path().join("bin/git-ai")).unwrap();
    run(home.path(), &fetch, &pin).unwrap();
    assert!(std::fs::symlink_metadata(home.path().join("bin/git-ai"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fetch.calls.load(Ordering::SeqCst), 1);
}
#[test]
fn replaces_old_and_broken_own_and_sweeps_orphans_without_touching_configs() {
    let real_user = crate::routines::test_support::TempHome::new();
    for body in ["echo 1.7.4", "exit 1"] {
        let home = tempfile::tempdir().unwrap();
        script(&home.path().join("bin/git-ai"), body);
        std::fs::write(home.path().join("bin/.git-ai.tmp-orphan"), b"partial").unwrap();
        let user_home = real_user.root();
        let protected = [".gitconfig", ".claude/settings.json", ".codex/config.toml"]
            .map(|p| user_home.join(p));
        for (path, bytes) in protected.iter().zip([
            "[user]\nname = Test\n",
            "{\"permissions\":{}}",
            "model = \"test\"\n",
        ]) {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        let hashes = || {
            protected
                .each_ref()
                .map(|p| crate::shared::hash::sha256_hex(&std::fs::read(p).unwrap()))
        };
        let before = hashes();
        // The version probe deliberately writes user-shaped files. Its managed HOME
        // must receive them, while the real (sandboxed) user's files keep their hashes.
        let (fetch, pin) = fake(
            r#"/bin/mkdir -p "$HOME/.claude" "$HOME/.codex"
printf managed > "$HOME/.gitconfig"
printf managed > "$HOME/.claude/settings.json"
printf managed > "$HOME/.codex/config.toml"
echo 1.7.5"#,
        );
        run(home.path(), &fetch, &pin).unwrap();
        assert_eq!(
            std::fs::read(home.path().join("bin/git-ai")).unwrap(),
            fetch.bytes
        );
        assert!(staged(home.path()).is_empty());
        assert_eq!(hashes(), before);
        assert_eq!(
            std::fs::read(home.path().join("git-ai/home/.gitconfig")).unwrap(),
            b"managed"
        );
    }
}
#[test]
fn network_failure_is_retryable_and_digest_failure_is_sticky() {
    let home = tempfile::tempdir().unwrap();
    let (mut fetch, pin) = fake("echo 1.7.5");
    fetch.error = true;
    assert!(!run(home.path(), &fetch, &pin).unwrap_err().sticky);
    fetch.error = false;
    let error = run(home.path(), &fetch, &PIN).unwrap_err();
    assert!(error.sticky);
    assert!(error
        .message
        .contains("bundled git-ai changed (sha mismatch)"));
}

struct Paused {
    bytes: Vec<u8>,
    calls: Arc<AtomicUsize>,
    entered: std::sync::Barrier,
    release: std::sync::Barrier,
}
fn paused_download(body: &str) -> (Arc<Paused>, Pin) {
    let (fetch, pin) = fake(body);
    (
        Arc::new(Paused {
            bytes: fetch.bytes,
            calls: fetch.calls,
            entered: std::sync::Barrier::new(2),
            release: std::sync::Barrier::new(2),
        }),
        pin,
    )
}
impl Fetch for Paused {
    fn fetch<'a>(&'a self, sink: &'a mut (dyn Write + Send)) -> FetchFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.entered.wait();
            self.release.wait();
            sink.write_all(&self.bytes).map_err(Failure::retryable)
        })
    }
}
#[test]
fn single_flight_both_event_orders_and_restart() {
    let home = tempfile::tempdir().unwrap();
    let (paused, pin) = paused_download("echo 1.7.5");
    let service = Service::new(home.path().into(), None, paused.clone(), pin.clone(), true);
    assert_eq!(
        service.request(Trigger::Startup),
        Some(AttributionStatus::Installing)
    );
    paused.entered.wait();
    assert_eq!(
        service.wait(Duration::ZERO),
        Some(AttributionStatus::Installing)
    );
    assert_eq!(
        service.request(Trigger::Launch),
        Some(AttributionStatus::Installing)
    );
    paused.release.wait();
    assert_eq!(service.wait(Duration::from_secs(30)), None);
    assert_eq!(paused.calls.load(Ordering::SeqCst), 1);
    let (fetch, _) = fake("echo 1.7.5");
    let fetch = Arc::new(fetch);
    let restarted = Service::new(home.path().into(), None, fetch.clone(), pin, true);
    restarted.request(Trigger::Startup);
    assert_eq!(restarted.wait(Duration::from_secs(30)), None);
    assert_eq!(fetch.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn backoff_network_retries_after_delay_but_mismatch_requires_explicit_request() {
    for sticky in [true, false] {
        let home = tempfile::tempdir().unwrap();
        let (mut fetch, pin) = fake("echo 1.7.5");
        fetch.error = !sticky;
        let fetch = Arc::new(fetch);
        let pin = if sticky { PIN.clone() } else { pin };
        let service = Service::new(home.path().into(), None, fetch.clone(), pin, true);
        service.request(Trigger::Launch);
        assert!(matches!(
            service.wait(Duration::from_secs(30)),
            Some(AttributionStatus::InstallFailed { .. })
        ));
        service.request(Trigger::Launch);
        assert_eq!(fetch.calls.load(Ordering::SeqCst), 1);
        service.state.lock().unwrap().failed.as_mut().unwrap().1 =
            Instant::now() - Duration::from_secs(61);
        service.request(Trigger::Launch);
        service.wait(Duration::from_secs(30));
        assert_eq!(
            fetch.calls.load(Ordering::SeqCst),
            if sticky { 1 } else { 2 }
        );
        service.request(Trigger::OnDemand);
        service.wait(Duration::from_secs(30));
        assert_eq!(
            fetch.calls.load(Ordering::SeqCst),
            if sticky { 2 } else { 3 }
        );
    }
}
#[test]
fn two_instances_share_lock_and_redetect_before_downloading() {
    let home = tempfile::tempdir().unwrap();
    let (fetch, pin) = fake("echo 1.7.5");
    let fetch = Arc::new(fetch);
    let a = Service::new(home.path().into(), None, fetch.clone(), pin.clone(), true);
    let b = Service::new(home.path().into(), None, fetch.clone(), pin, true);
    a.request(Trigger::Startup);
    b.request(Trigger::Startup);
    a.wait(Duration::from_secs(30));
    b.wait(Duration::from_secs(30));
    assert_eq!(fetch.calls.load(Ordering::SeqCst), 1);
}
fn launch_prepare_claude_codex_and_relaunch_see_installing_then_fresh_capture() {
    use crate::attribution::testing::Launch;
    use codesense_pty_core::agent_cli::AgentCli;
    let home = crate::routines::test_support::TempHome::new();
    struct RestorePath(Option<OsString>);
    impl Drop for RestorePath {
        fn drop(&mut self) {
            match self.0.take() {
                Some(path) => std::env::set_var("PATH", path),
                None => std::env::remove_var("PATH"),
            }
        }
    }
    let _path_guard = RestorePath(std::env::var_os("PATH"));
    let fake_bin = home.root().join("path-bin");
    let path_calls = home.root().join("path-executions");
    script(
        &fake_bin.join("git-ai"),
        &format!("echo executed >> '{}'\necho 1.7.5", path_calls.display()),
    );
    let mut paths = vec![fake_bin];
    paths.extend(std::env::split_paths(
        std::env::var_os("PATH")
            .as_deref()
            .unwrap_or(OsStr::new("")),
    ));
    let paths = std::env::join_paths(paths).unwrap();
    std::env::set_var("PATH", &paths);
    for entry in ["prepare", "renderer", "headless", "mcp", "respawn"] {
        for cli in [AgentCli::Claude, AgentCli::Codex] {
            let fixture = Launch::new(cli);
            fixture.uninstall();
            let (paused, pin) = paused_download("echo 1.7.5");
            let service = Service::new(fixture.home().into(), None, paused.clone(), pin, true);
            services()
                .lock()
                .unwrap()
                .insert(fixture.home().into(), service.clone());
            let launch = || {
                match entry {
                    "prepare" => {
                        crate::attribution::launch::prepare(
                            &crate::attribution::launch::Request {
                                codesense_home: fixture.home(),
                                principal: &crate::agents::resolve::tests::alice(),
                                cwd: &fixture.cwd,
                                runtime: cli,
                            },
                            Some(&paths),
                        );
                    }
                    "renderer" => {
                        let (_, _, id) =
                            crate::commands::agents_launch::tests::attribution_entry::launched(
                                &fixture,
                            );
                        crate::commands::mcp_scope::remove_scope_config(&id);
                    }
                    "headless" | "mcp" => {
                        let (_, _, _, id) = if entry == "headless" {
                            crate::commands::dashboard_launch::attribution_entry_tests::provisioned(
                                &fixture,
                            )
                        } else {
                            crate::commands::mcp_workspace::attribution_entry_tests::scoped(
                                &fixture,
                            )
                        };
                        crate::commands::mcp_scope::remove_scope_config(&id);
                    }
                    "respawn" => {
                        crate::commands::mcp_authoring::tests::attribution_respawn(&fixture, cli)
                    }
                    _ => unreachable!(),
                }
                fixture.status().unwrap()
            };
            assert_eq!(launch(), AttributionStatus::Installing, "{entry} {cli:?}");
            paused.entered.wait();
            assert_eq!(launch(), AttributionStatus::Installing, "{entry} {cli:?}");
            paused.release.wait();
            service.wait(Duration::from_secs(30));
            let _daemon = fixture.daemon_running();
            assert_eq!(launch(), AttributionStatus::Active, "{entry} {cli:?}");
            assert_eq!(paused.calls.load(Ordering::SeqCst), 1);
            assert!(
                !path_calls.exists(),
                "{entry} {cli:?}: PATH git-ai must never execute"
            );
        }
    }
}

#[test]
fn orphan_after_interrupted_write_is_swept_and_no_partial_binary_is_visible() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("bin")).unwrap();
    let mut temp = tempfile::Builder::new()
        .prefix(".git-ai.tmp")
        .tempfile_in(home.path().join("bin"))
        .unwrap();
    temp.write_all(b"incomplete download").unwrap();
    let (_, orphan) = temp.keep().unwrap();
    assert!(!home.path().join("bin/git-ai").exists());
    let (fetch, pin) = fake("echo 1.7.5");
    run(home.path(), &fetch, &pin).unwrap();
    assert!(!orphan.exists());
}
#[test]
fn failed_upgrade_keeps_old_binary_but_capture_off_and_reports_warning() {
    use crate::attribution::testing::Launch;
    use codesense_pty_core::agent_cli::AgentCli;
    let fixture = Launch::new(AgentCli::Claude);
    let _daemon = fixture.daemon_running();
    let (mut fetch, mut pin) = fake("echo 1.7.6");
    pin.expected_version = "1.7.6";
    fetch.error = true;
    let service = Service::new(fixture.home().into(), None, Arc::new(fetch), pin, true);
    services()
        .lock()
        .unwrap()
        .insert(fixture.home().into(), service.clone());
    service.request(Trigger::Startup);
    service.wait(Duration::from_secs(30));
    let doctor = crate::attribution::launch::doctor(fixture.home(), None);
    assert!(!matches!(doctor.status, AttributionStatus::Active));
    assert!(doctor.warnings.iter().any(|w| w.contains("offline")));
}
#[test]
fn only_installer_owns_binary_publication_and_no_global_hooks_are_run() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let publish = std::fs::read_to_string(src.join("attribution/install.rs")).unwrap();
    let publication = "std::fs::rename(&temp, dir.join(\"git-ai\"))";
    assert!(publish.contains(publication));
    let owners: Vec<_> = walkdir::WalkDir::new(&src)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_file() && entry.path().extension().is_some_and(|e| e == "rs")
        })
        .filter(|entry| {
            std::fs::read_to_string(entry.path())
                .unwrap()
                .contains(publication)
        })
        .map(|entry| entry.into_path())
        .collect();
    assert_eq!(owners, [src.join("attribution/install.rs")]);
    for file in [
        "lib.rs",
        "agents/launch.rs",
        "attribution/launch.rs",
        "mcp/local_setup.rs",
        "attribution/managed.rs",
    ] {
        let source = std::fs::read_to_string(src.join(file)).unwrap();
        assert!(
            !source.contains(".args([\"install-hooks\"])")
                && !source.contains(".arg(\"install-hooks\")")
        );
    }
    let startup = std::fs::read_to_string(src.join("lib.rs")).unwrap();
    let initialize = startup
        .find("attribution::install::initialize(")
        .expect("startup registers bundled git-ai");
    let ensure = startup
        .find("attribution::install::ensure_installed")
        .expect("startup starts installation");
    assert!(
        initialize < ensure,
        "register bundled resource before requesting installation"
    );
    let mcp_start = startup
        .find("mcp::start_mcp_server(")
        .expect("startup starts the MCP server");
    assert!(
        initialize < mcp_start && ensure < mcp_start,
        "register and request bundled installation before accepting MCP requests"
    );
    assert!(publish.contains("register_bundle("));
    assert!(publish.contains("crate::commands::paths::resolve_bundled_resource(app, PIN.resource)"));
    let mcp = std::fs::read_to_string(src.join("mcp/local_setup.rs")).unwrap();
    assert!(mcp.contains("Trigger::OnDemand"));
}

pub(crate) fn register(home: &Path) -> Arc<AtomicUsize> {
    let (fetch, pin) = fake("echo 1.7.5");
    let count = fetch.calls.clone();
    services().lock().unwrap().insert(
        home.into(),
        Service::new(home.into(), None, Arc::new(fetch), pin, true),
    );
    count
}
#[test]
fn startup_entry_finishes_then_launch_reuses_install() {
    let home = tempfile::tempdir().unwrap();
    let count = register(home.path());
    assert_eq!(
        ensure_installed(home.path(), None, Trigger::Startup),
        Some(AttributionStatus::Installing)
    );
    let service = service(home.path(), None).unwrap();
    assert_eq!(service.wait(Duration::from_secs(30)), None);
    ensure_installed(home.path(), None, Trigger::Launch);
    service.wait(Duration::from_secs(30));
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn cross_process_lock_serializes_two_apps_and_redetects() {
    const KEY: &str = "CODESENSE_TEST_INSTALL_LOCK_HOME";
    if let Some(home) = std::env::var_os(KEY) {
        struct Recorded {
            fake: Fake,
            log: PathBuf,
        }
        impl Fetch for Recorded {
            fn fetch<'a>(&'a self, sink: &'a mut (dyn Write + Send)) -> FetchFuture<'a> {
                Box::pin(async move {
                    OpenOptions::new()
                        .append(true)
                        .create(true)
                        .open(&self.log)
                        .map_err(Failure::retryable)?
                        .write_all(b"download\n")
                        .map_err(Failure::retryable)?;
                    self.fake.fetch(sink).await
                })
            }
        }
        let home = PathBuf::from(home);
        let (fake, pin) = fake("echo 1.7.5");
        let fetch = Recorded {
            fake,
            log: home.join("fetches"),
        };
        run(&home, &fetch, &pin).unwrap();
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    let mut children = Vec::new();
    for _ in 0..2 {
        children.push(std::process::Command::new(&exe)
            .args(["--exact", "attribution::install::tests::cross_process_lock_serializes_two_apps_and_redetects", "--nocapture"])
            .env(KEY, home.path()).spawn().unwrap());
    }
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    assert_eq!(
        std::fs::read_to_string(home.path().join("fetches")).unwrap(),
        "download\n"
    );
}

#[test]
fn pin_bump_upgrades_a_still_supported_older_own_copy() {
    let home = tempfile::tempdir().unwrap();
    script(&home.path().join("bin/git-ai"), "echo 1.7.5");
    let (fetch, mut pin) = fake("echo 1.7.6");
    pin.expected_version = "1.7.6";
    run(home.path(), &fetch, &pin).unwrap();
    assert_eq!(
        std::fs::read(home.path().join("bin/git-ai")).unwrap(),
        fetch.bytes
    );
    assert_eq!(fetch.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn fork_suffix_is_preserved_by_the_exact_download_version_gate() {
    let home = tempfile::tempdir().unwrap();
    let (fetch, mut pin) = fake("echo 1.7.5-cs.1");
    pin.expected_version = "1.7.5-cs.1";
    run(home.path(), &fetch, &pin).unwrap();
    assert_eq!(
        std::fs::read(home.path().join("bin/git-ai")).unwrap(),
        fetch.bytes
    );
    let home = tempfile::tempdir().unwrap();
    pin.expected_version = "1.7.5-cs.2";
    assert!(run(home.path(), &fetch, &pin).unwrap_err().sticky);
    assert!(!home.path().join("bin/git-ai").exists());
}

#[test]
fn successful_upgrade_persists_old_daemon_version_across_restarts_until_next_launch() {
    use crate::attribution::testing::{logging_binary, version_script, Launch};
    use codesense_pty_core::agent_cli::AgentCli;
    let fixture = Launch::new(AgentCli::Claude);
    fixture.install(&version_script("1.7.5"));
    let _old_daemon = fixture.daemon_running();
    let stamp = fixture.home().join("git-ai/daemon-binary-version");
    assert!(!stamp.exists());
    let asset = fixture.home().join("new-git-ai-fixture");
    logging_binary(&asset, &fixture.log(), &version_script("1.7.6"));
    let bytes = std::fs::read(asset).unwrap();
    let mut pin = PIN.clone();
    pin.expected_version = "1.7.6";
    pin.sha256 = crate::shared::hash::sha256_hex(&bytes);
    let fetch = Arc::new(Fake {
        bytes,
        calls: Arc::new(AtomicUsize::new(0)),
        error: false,
    });
    let recreate_service = || {
        let service = Service::new(
            fixture.home().into(),
            None,
            fetch.clone(),
            pin.clone(),
            true,
        );
        services()
            .lock()
            .unwrap()
            .insert(fixture.home().into(), service.clone());
        service
    };
    let installing = recreate_service();
    installing.request(Trigger::Startup);
    assert_eq!(installing.wait(Duration::from_secs(30)), None);
    assert_eq!(std::fs::read_to_string(&stamp).unwrap(), "1.7.5");
    drop(installing);

    let restarted = recreate_service();
    restarted.request(Trigger::Startup);
    assert_eq!(restarted.wait(Duration::from_secs(30)), None);
    let doctor = crate::attribution::launch::doctor(fixture.home(), None);
    assert_eq!(doctor.git_ai_version.as_deref(), Some("1.7.6"));
    assert_eq!(
        doctor.status,
        AttributionStatus::Off {
            reason: super::super::OffReason::NotesWouldPush
        }
    );
    assert!(doctor
        .warnings
        .iter()
        .any(|warning| warning.contains("restart pending")));
    assert_eq!(std::fs::read_to_string(&stamp).unwrap(), "1.7.5");
    drop(restarted);

    let _reopened = recreate_service();
    let plan = crate::attribution::launch::prepare(
        &crate::attribution::launch::Request {
            codesense_home: fixture.home(),
            principal: &crate::agents::resolve::tests::alice(),
            cwd: &fixture.cwd,
            runtime: AgentCli::Claude,
        },
        None,
    );
    assert_eq!(plan.status, AttributionStatus::Active);
    assert!(fixture
        .calls()
        .iter()
        .any(|call| call.starts_with("bg restart")));
    assert_eq!(
        std::fs::read_to_string(&stamp).unwrap(),
        crate::attribution::managed::Managed::daemon_stamp("1.7.6")
    );
    assert!(!crate::attribution::launch::doctor(fixture.home(), None)
        .warnings
        .iter()
        .any(|warning| warning.contains("restart pending")));
    assert_eq!(fetch.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn probe_failures_retry_after_backoff_but_successful_wrong_version_is_sticky() {
    let timeout = format!("timed out after {}s", detect::VERSION_TIMEOUT.as_secs());
    for (body, reason, sticky) in [
        (
            r#"/bin/mkdir -p "$HOME"
if [ ! -e "$HOME/probed" ]; then
  : > "$HOME/probed"
  exec /bin/sleep 60
fi
echo 1.7.5"#,
            timeout.as_str(),
            false,
        ),
        (
            r#"/bin/mkdir -p "$HOME"
if [ ! -e "$HOME/probed" ]; then
  : > "$HOME/probed"
  echo 'scanner busy' >&2
  exit 9
fi
echo 1.7.5"#,
            "scanner busy",
            false,
        ),
        ("echo 1.7.6", "version mismatch", true),
    ] {
        let home = tempfile::tempdir().unwrap();
        let (fetch, pin) = fake(body);
        let fetch = Arc::new(fetch);
        let service = Service::new(home.path().into(), None, fetch.clone(), pin, true);
        service.request(Trigger::Startup);
        let status = service.wait(detect::VERSION_TIMEOUT + Duration::from_secs(30));
        let Some(AttributionStatus::InstallFailed { message }) = status else {
            panic!("probe must fail: {status:?}");
        };
        assert!(message.contains(reason), "{message}");
        if !sticky {
            assert!(message.contains("version probe"), "{message}");
        }
        assert_eq!(
            service
                .state
                .lock()
                .unwrap()
                .failed
                .as_ref()
                .unwrap()
                .0
                .sticky,
            sticky
        );
        assert!(!home.path().join("bin/git-ai").exists());
        assert!(staged(home.path()).is_empty());
        service.request(Trigger::Launch);
        assert_eq!(
            fetch.calls.load(Ordering::SeqCst),
            1,
            "backoff must suppress an immediate retry"
        );
        service.state.lock().unwrap().failed.as_mut().unwrap().1 =
            Instant::now() - BACKOFF - Duration::from_secs(1);
        service.request(Trigger::Launch);
        let status = service.wait(detect::VERSION_TIMEOUT + Duration::from_secs(30));
        if sticky {
            assert!(matches!(
                status,
                Some(AttributionStatus::InstallFailed { .. })
            ));
            assert_eq!(fetch.calls.load(Ordering::SeqCst), 1);
        } else {
            assert_eq!(status, None);
            assert!(home.path().join("bin/git-ai").exists());
            assert_eq!(fetch.calls.load(Ordering::SeqCst), 2);
        }
    }
}

// Bundled delivery integration cases. Every asset is a native argv[0]-sensitive
// fixture copied through the production Fetch and install_once seams.
fn bundle(home: &Path, version: &'static str, extra: &str) -> (Arc<Bundled>, Pin) {
    let asset = home.join("resources/git-ai");
    crate::attribution::testing::argv0_binary_with_version(&asset, version, extra);
    let mut pin = PIN.clone();
    pin.expected_version = version;
    pin.sha256 = crate::shared::hash::sha256_hex(&std::fs::read(&asset).unwrap());
    (Arc::new(Bundled { path: Some(asset) }), pin)
}
fn bundle_service(home: &Path, fetch: Arc<Bundled>, pin: Pin) -> Arc<Service> {
    let service = Service::new(home.into(), None, fetch, pin, true);
    services()
        .lock()
        .unwrap()
        .insert(home.into(), service.clone());
    service
}
fn start(service: &Arc<Service>) {
    assert_eq!(
        service.request(Trigger::Startup),
        Some(AttributionStatus::Installing)
    );
    assert_eq!(service.wait(Duration::from_secs(30)), None);
}

fn bundled_case_1_fresh_install_verifies_native_bytes_and_reports_active() {
    let fixture =
        crate::attribution::testing::Launch::new(codesense_pty_core::agent_cli::AgentCli::Claude);
    fixture.uninstall();
    let (fetch, pin) = bundle(fixture.home(), PIN.expected_version, "");
    let bytes = std::fs::read(fetch.path.as_ref().unwrap()).unwrap();
    let service = bundle_service(fixture.home(), fetch, pin);
    start(&service);
    assert_eq!(
        std::fs::read(fixture.home().join("bin/git-ai")).unwrap(),
        bytes
    );
    assert!(staged(fixture.home()).is_empty());
    let _daemon = fixture.daemon_running();
    assert_eq!(
        crate::attribution::launch::doctor(fixture.home(), None).status,
        AttributionStatus::Active
    );
}

fn bundled_case_2_same_core_upstream_is_replaced_with_one_previous_and_notice() {
    let fixture =
        crate::attribution::testing::Launch::new(codesense_pty_core::agent_cli::AgentCli::Claude);
    let own = fixture.home().join("bin/git-ai");
    crate::attribution::testing::argv0_binary(&own);
    let old = std::fs::read(&own).unwrap();
    std::fs::write(fixture.home().join("bin/git-ai.prev-1.7.4"), "obsolete").unwrap();
    let (fetch, pin) = bundle(fixture.home(), PIN.expected_version, "");
    let bytes = std::fs::read(fetch.path.as_ref().unwrap()).unwrap();
    let service = bundle_service(fixture.home(), fetch, pin);
    start(&service);
    assert_eq!(std::fs::read(&own).unwrap(), bytes);
    let previous = fixture.home().join("bin/git-ai.prev-1.7.5");
    assert_eq!(std::fs::read(&previous).unwrap(), old);
    assert!(!fixture.home().join("bin/git-ai.prev-1.7.4").exists());
    let _daemon = fixture.daemon_running();
    let doctor = crate::attribution::launch::doctor(fixture.home(), None);
    assert!(doctor.warnings.iter().any(|w| w
        == &format!(
            "replaced git-ai 1.7.5 → {} (previous kept at {})",
            PIN.expected_version,
            previous.display()
        )));
    assert!(!crate::attribution::launch::doctor(fixture.home(), None)
        .warnings
        .iter()
        .any(|w| w.starts_with("replaced git-ai")));
}

fn bundled_case_3_exact_pin_keeps_binary_without_rewriting() {
    use std::os::unix::fs::MetadataExt;
    let home = tempfile::tempdir().unwrap();
    let own = home.path().join("bin/git-ai");
    crate::attribution::testing::argv0_binary_with_version(&own, PIN.expected_version, "");
    let before = std::fs::metadata(&own).unwrap();
    // It must not even need the bundle when the exact pin is already installed.
    let service = bundle_service(home.path(), Arc::new(Bundled { path: None }), PIN.clone());
    start(&service);
    let after = std::fs::metadata(&own).unwrap();
    assert_eq!(
        (before.ino(), before.modified().unwrap()),
        (after.ino(), after.modified().unwrap())
    );
    assert!(!home.path().join("bin/.git-ai-previous").exists());
}

fn bundled_case_4_newer_numeric_cores_are_preserved_and_unsupported() {
    for version in ["1.7.6", "1.8.0"] {
        let home = tempfile::tempdir().unwrap();
        let own = home.path().join("bin/git-ai");
        crate::attribution::testing::argv0_binary_with_version(&own, version, "");
        let before = std::fs::read(&own).unwrap();
        let service = bundle_service(home.path(), Arc::new(Bundled { path: None }), PIN.clone());
        start(&service);
        assert_eq!(std::fs::read(&own).unwrap(), before);
        assert_eq!(
            crate::attribution::launch::doctor(home.path(), None).status,
            AttributionStatus::Unsupported {
                reason: "newer".into()
            }
        );
        assert!(!home.path().join("bin/.git-ai-previous").exists());
    }
}

fn bundled_case_5_missing_checksum_and_version_failures_are_sticky() {
    for failure in ["missing", "checksum", "version"] {
        let home = tempfile::tempdir().unwrap();
        let (fetch, mut pin) = bundle(home.path(), "1.7.5-cs.1", "");
        if failure == "missing" {
            std::fs::remove_file(fetch.path.as_ref().unwrap()).unwrap();
        } else if failure == "checksum" {
            pin.sha256 = "0".repeat(64);
        } else {
            pin.expected_version = "1.7.5-cs.2";
        }
        let service = bundle_service(home.path(), fetch, pin);
        service.request(Trigger::Startup);
        let status = service.wait(Duration::from_secs(30));
        let Some(AttributionStatus::InstallFailed { message }) = status else {
            panic!("{failure}: {status:?}")
        };
        assert!(
            message.contains(match failure {
                "missing" => "bundle_missing",
                "checksum" => "sha_mismatch",
                _ => "version_mismatch",
            }),
            "{message}"
        );
        assert!(
            service
                .state
                .lock()
                .unwrap()
                .failed
                .as_ref()
                .unwrap()
                .0
                .sticky
        );
        service.state.lock().unwrap().failed.as_mut().unwrap().1 =
            Instant::now() - BACKOFF - Duration::from_secs(1);
        assert!(matches!(
            service.request(Trigger::Launch),
            Some(AttributionStatus::InstallFailed { .. })
        ));
        assert!(!home.path().join("bin/git-ai").exists());
        assert!(staged(home.path()).is_empty());
    }
}

fn bundled_case_6_path_copy_is_reported_without_any_execution() {
    let home = tempfile::tempdir().unwrap();
    let user_bin = home.path().join("user-bin");
    let calls = home.path().join("user-calls");
    script(
        &user_bin.join("git-ai"),
        &format!("echo called >> '{}'\necho 1.7.5", calls.display()),
    );
    let path = std::env::join_paths([user_bin]).unwrap();
    let (fetch, pin) = bundle(home.path(), PIN.expected_version, "");
    let service = Service::new(home.path().into(), Some(path.clone()), fetch, pin, true);
    services()
        .lock()
        .unwrap()
        .insert(home.path().into(), service.clone());
    start(&service);
    let doctor = crate::attribution::launch::doctor(home.path(), Some(&path));
    assert_eq!(
        doctor.binary.as_deref(),
        Some(home.path().join("bin/git-ai").to_str().unwrap())
    );
    assert!(doctor
        .warnings
        .iter()
        .any(|w| w.starts_with("user_copy_present")));
    assert!(!calls.exists(), "the user git-ai must never be executed");
}

fn bundled_case_8_restart_pending_stays_off_until_pinned_daemon_restarts() {
    use crate::attribution::testing::Launch;
    use codesense_pty_core::agent_cli::AgentCli;
    let fixture = Launch::new(AgentCli::Claude);
    crate::attribution::testing::argv0_binary(&fixture.home().join("bin/git-ai"));
    let _old_daemon = fixture.daemon_running();
    let (fetch, pin) = bundle(
        fixture.home(),
        PIN.expected_version,
        r#"if [ "$1 $2" = 'bg restart' ] && [ -e "$HOME/restart-fails" ]; then exit 9; fi"#,
    );
    let service = bundle_service(fixture.home(), fetch.clone(), pin.clone());
    start(&service);
    let stamp = fixture.home().join("git-ai/daemon-binary-version");
    assert_eq!(std::fs::read_to_string(&stamp).unwrap(), "1.7.5");
    // Re-open the install service to prove the daemon stamp persists across app starts.
    let reopened = bundle_service(fixture.home(), fetch, pin);
    start(&reopened);
    let doctor = crate::attribution::launch::doctor(fixture.home(), None);
    assert_eq!(
        doctor.status,
        AttributionStatus::Off {
            reason: super::super::OffReason::NotesWouldPush
        }
    );
    assert!(doctor
        .warnings
        .iter()
        .any(|w| w.contains("restart pending")));
    let managed_home = fixture.home().join("git-ai/home");
    std::fs::create_dir_all(&managed_home).unwrap();
    let fail_restart = managed_home.join("restart-fails");
    std::fs::write(&fail_restart, "fail once").unwrap();
    let prepare = || {
        crate::attribution::launch::prepare(
            &crate::attribution::launch::Request {
                codesense_home: fixture.home(),
                principal: &crate::agents::resolve::tests::alice(),
                cwd: &fixture.cwd,
                runtime: AgentCli::Claude,
            },
            None,
        )
    };
    let pending = prepare();
    assert_eq!(
        pending.status,
        AttributionStatus::Off {
            reason: super::super::OffReason::NotesWouldPush
        }
    );
    assert!(pending.hook_command.is_none() && pending.env.is_empty());
    assert_eq!(std::fs::read_to_string(&stamp).unwrap(), "1.7.5");
    std::fs::remove_file(fail_restart).unwrap();
    assert_eq!(prepare().status, AttributionStatus::Active);
    assert_eq!(
        std::fs::read_to_string(&stamp).unwrap(),
        crate::attribution::managed::Managed::daemon_stamp(PIN.expected_version)
    );
    let calls = std::fs::read_to_string(managed_home.join("argv0-calls")).unwrap();
    assert_eq!(calls.lines().filter(|l| *l == "bg restart").count(), 2);
    assert!(!crate::attribution::launch::doctor(fixture.home(), None)
        .warnings
        .iter()
        .any(|w| w.contains("restart pending")));
}

fn bundled_revert_is_respected_across_restarts_until_explicit_request() {
    let home = tempfile::tempdir().unwrap();
    let own = home.path().join("bin/git-ai");
    crate::attribution::testing::argv0_binary(&own);
    let old = std::fs::read(&own).unwrap();
    let (fetch, pin) = bundle(home.path(), PIN.expected_version, "");
    let service = bundle_service(home.path(), fetch.clone(), pin.clone());
    start(&service);
    std::fs::rename(home.path().join("bin/git-ai.prev-1.7.5"), &own).unwrap();
    for _ in 0..2 {
        let reopened = bundle_service(home.path(), fetch.clone(), pin.clone());
        start(&reopened);
        assert_eq!(std::fs::read(&own).unwrap(), old);
        assert!(home.path().join("bin/user-restored").exists());
        assert_eq!(
            crate::attribution::launch::doctor(home.path(), None).status,
            AttributionStatus::Unsupported {
                reason: "user_pinned".into()
            }
        );
    }
    let current = super::service(home.path(), None).unwrap();
    current.request(Trigger::OnDemand);
    assert_eq!(current.wait(Duration::from_secs(30)), None);
    assert!(!home.path().join("bin/user-restored").exists());
    assert_ne!(std::fs::read(&own).unwrap(), old);
}

#[test]
fn bundled_replacement_changes_no_notes_managed_config_or_user_files() {
    let home = tempfile::tempdir().unwrap();
    let own = home.path().join("bin/git-ai");
    crate::attribution::testing::argv0_binary(&own);
    let repo = git2::Repository::init(home.path().join("repo")).unwrap();
    let blob = repo.blob(b"attribution note").unwrap();
    for name in ["refs/notes/ai", "refs/notes/sense", "refs/notes/custom"] {
        repo.reference(name, blob, true, "fixture").unwrap();
    }
    let protected = [
        "user/.git-ai/config.json",
        "user/.gitconfig",
        "user/.claude/settings.json",
        "user/.codex/config.toml",
        "git-ai/home/.git-ai/config.json",
        "git-ai/config-applied",
    ]
    .map(|p| home.path().join(p));
    for path in &protected {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("untouched {}", path.display())).unwrap();
    }
    let hashes = || {
        protected
            .each_ref()
            .map(|p| crate::shared::hash::sha256_hex(&std::fs::read(p).unwrap()))
    };
    let notes = || {
        repo.references_glob("refs/notes/*")
            .unwrap()
            .map(|r| {
                let r = r.unwrap();
                (r.name().unwrap().to_string(), r.target().unwrap())
            })
            .collect::<Vec<_>>()
    };
    let before = (hashes(), notes());
    let (fetch, pin) = bundle(home.path(), PIN.expected_version, "");
    let service = bundle_service(home.path(), fetch, pin);
    start(&service);
    assert_eq!((hashes(), notes()), before);
}

#[test]
fn invalid_checked_in_pin_is_a_tested_panic() {
    for invalid in ["", "version=garbage", "version=1.7.5\nversion=1.7.5"] {
        assert!(std::panic::catch_unwind(
            || Pin::parse(invalid).expect("invalid bundled git-ai pin")
        )
        .is_err());
    }
}

#[test]
fn bundled_delivery_case_table() {
    let cases: [(&str, fn()); 19] = [
        (
            "1 fresh bundle",
            bundled_case_1_fresh_install_verifies_native_bytes_and_reports_active,
        ),
        (
            "2 replace upstream same core",
            bundled_case_2_same_core_upstream_is_replaced_with_one_previous_and_notice,
        ),
        (
            "3 keep exact pin",
            bundled_case_3_exact_pin_keeps_binary_without_rewriting,
        ),
        (
            "4 keep newer core unsupported",
            bundled_case_4_newer_numeric_cores_are_preserved_and_unsupported,
        ),
        (
            "5 sticky bundle failures",
            bundled_case_5_missing_checksum_and_version_failures_are_sticky,
        ),
        (
            "6 PATH isolation",
            bundled_case_6_path_copy_is_reported_without_any_execution,
        ),
        (
            "7 all launch entry paths",
            launch_prepare_claude_codex_and_relaunch_see_installing_then_fresh_capture,
        ),
        (
            "8 daemon restart",
            bundled_case_8_restart_pending_stays_off_until_pinned_daemon_restarts,
        ),
        (
            "manual revert across restarts",
            bundled_revert_is_respected_across_restarts_until_explicit_request,
        ),
        (
            "ordinary explicit check keeps future revert",
            ordinary_explicit_check_preserves_a_later_manual_revert,
        ),
        (
            "explicit intent in both arrival orders and restart",
            explicit_intent_survives_both_arrival_orders_and_restart,
        ),
        (
            "no-op checks retain replacement notice",
            replacement_notice_survives_noop_checks_until_doctor_reads_it,
        ),
        (
            "shared startup registration",
            shared_registration_from_empty_registry_installs_the_native_bundle,
        ),
        (
            "setup envelope persisted replacement and daemon restart",
            setup_wire_replacement_and_restart_pending_survive_service_restart,
        ),
        (
            "setup envelope persistent install failure and retry",
            setup_wire_install_failures_survive_restart_and_restart_retries,
        ),
        (
            "setup envelope newer/restored status and recovery",
            setup_wire_newer_and_restored_copies_are_unsupported_and_actionable,
        ),
        (
            "setup envelope installing",
            setup_wire_reports_installing_through_the_production_envelope,
        ),
        (
            "replace → restart before first doctor read → notice once, then active",
            replacement_notice_survives_restart_before_first_doctor_read,
        ),
        (
            "two diagnostic sessions announce once",
            two_diagnostic_sessions_announce_a_pending_receipt_only_once,
        ),
    ];
    let mut failed = Vec::new();
    for (name, run) in cases {
        if std::panic::catch_unwind(run).is_err() {
            failed.push(name);
        } else {
            eprintln!("bundled delivery case {name}: passed");
        }
    }
    assert!(
        failed.is_empty(),
        "bundled delivery cases failed: {failed:?}"
    );
}

fn ordinary_explicit_check_preserves_a_later_manual_revert() {
    let home = tempfile::tempdir().unwrap();
    let own = home.path().join("bin/git-ai");
    crate::attribution::testing::argv0_binary(&own);
    let old = std::fs::read(&own).unwrap();
    let (fetch, pin) = bundle(home.path(), PIN.expected_version, "");
    let service = bundle_service(home.path(), fetch.clone(), pin.clone());
    start(&service);
    let receipt = home.path().join("bin/.git-ai-previous");
    let before = std::fs::read(&receipt).unwrap();
    service.request(Trigger::OnDemand);
    assert_eq!(service.wait(Duration::from_secs(30)), None);
    assert_eq!(std::fs::read(&receipt).unwrap(), before);
    std::fs::rename(home.path().join("bin/git-ai.prev-1.7.5"), &own).unwrap();
    let restarted = bundle_service(home.path(), fetch, pin);
    start(&restarted);
    assert_eq!(std::fs::read(&own).unwrap(), old);
    assert_eq!(
        crate::attribution::launch::doctor(home.path(), None).status,
        AttributionStatus::Unsupported {
            reason: "user_pinned".into()
        }
    );
}

fn explicit_intent_survives_both_arrival_orders_and_restart() {
    for explicit_first in [false, true] {
        let home = tempfile::tempdir().unwrap();
        let own = home.path().join("bin/git-ai");
        crate::attribution::testing::argv0_binary(&own);
        let (fetch, pin) = bundle(home.path(), PIN.expected_version, "");
        let service = bundle_service(home.path(), fetch.clone(), pin.clone());
        start(&service);
        std::fs::rename(home.path().join("bin/git-ai.prev-1.7.5"), &own).unwrap();

        // Hold the production install lock until both ordered triggers have arrived.
        // The worker is in flight but cannot detect the restored bytes yet.
        let lock = InstallLock::acquire(&home.path().join("bin")).unwrap();
        let entered = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let keeper = {
            let entered = entered.clone();
            let release = release.clone();
            std::thread::spawn(move || {
                entered.wait();
                release.wait();
                drop(lock);
            })
        };
        entered.wait();
        let triggers = if explicit_first {
            [Trigger::OnDemand, Trigger::Startup]
        } else {
            [Trigger::Startup, Trigger::OnDemand]
        };
        let statuses = triggers.map(|trigger| service.request(trigger));
        release.wait();
        keeper.join().unwrap();
        for status in statuses {
            assert_eq!(status, Some(AttributionStatus::Installing));
        }
        assert_eq!(service.wait(Duration::from_secs(30)), None);
        assert!(
            !home.path().join("bin/user-restored").exists(),
            "explicit first: {explicit_first}"
        );
        let bytes = std::fs::read(&own).unwrap();
        assert_eq!(bytes, std::fs::read(fetch.path.as_ref().unwrap()).unwrap());
        let reopened = bundle_service(home.path(), fetch, pin);
        start(&reopened);
        assert_eq!(std::fs::read(&own).unwrap(), bytes);
        assert!(!home.path().join("bin/user-restored").exists());
        let doctor = crate::attribution::launch::doctor(home.path(), None);
        assert_eq!(doctor.git_ai_version.as_deref(), Some(PIN.expected_version));
        assert_eq!(doctor.supported_versions, PIN.expected_version);
    }
}

fn replacement_notice_survives_noop_checks_until_doctor_reads_it() {
    let home = tempfile::tempdir().unwrap();
    crate::attribution::testing::argv0_binary(&home.path().join("bin/git-ai"));
    let (fetch, pin) = bundle(home.path(), PIN.expected_version, "");
    let service = bundle_service(home.path(), fetch, pin);
    start(&service);
    for trigger in [Trigger::Startup, Trigger::OnDemand] {
        service.request(trigger);
        assert_eq!(service.wait(Duration::from_secs(30)), None);
    }
    let doctor = crate::attribution::launch::doctor(home.path(), None);
    assert!(doctor
        .warnings
        .iter()
        .any(|w| w.starts_with("replaced git-ai 1.7.5 → ")));
    assert!(!crate::attribution::launch::doctor(home.path(), None)
        .warnings
        .iter()
        .any(|w| w.starts_with("replaced git-ai")));
}

fn shared_registration_from_empty_registry_installs_the_native_bundle() {
    let home = tempfile::tempdir().unwrap();
    assert!(super::service(home.path(), None).is_none());
    let (fetch, pin) = bundle(home.path(), PIN.expected_version, "");
    let expected = std::fs::read(fetch.path.as_ref().unwrap()).unwrap();
    let service = register_bundle(home.path(), None, fetch.path.clone(), pin.clone(), true);
    assert!(Arc::ptr_eq(
        &service,
        &super::service(home.path(), None).unwrap()
    ));
    assert!(
        Arc::ptr_eq(
            &service,
            &register_bundle(home.path(), None, None, pin, true)
        ),
        "startup registration must retain the first resolved bundled resource"
    );
    assert_eq!(
        ensure_installed(home.path(), None, Trigger::Startup),
        Some(AttributionStatus::Installing)
    );
    assert_eq!(service.wait(Duration::from_secs(30)), None);
    assert_eq!(
        std::fs::read(home.path().join("bin/git-ai")).unwrap(),
        expected
    );
    assert_eq!(
        crate::attribution::launch::doctor(home.path(), None).supported_versions,
        PIN.expected_version
    );
}

/// Roll back a real native temp bundle and register the restarted service, for MCP tests.
pub(crate) fn register_restored_bundle(home: &Path) {
    crate::attribution::testing::argv0_binary(&home.join("bin/git-ai"));
    let (fetch, pin) = bundle(home, PIN.expected_version, "");
    let service = bundle_service(home, fetch.clone(), pin.clone());
    start(&service);
    std::fs::rename(home.join("bin/git-ai.prev-1.7.5"), home.join("bin/git-ai")).unwrap();
    let restarted = bundle_service(home, fetch, pin);
    start(&restarted);
    assert!(home.join("bin/user-restored").exists());
}

/// Recreate the registered installer as app startup does, retaining its temp asset.
pub(crate) fn restart_registered_bundle(home: &Path) {
    let prior = super::service(home, None).unwrap();
    let restarted = Service::new(
        home.into(),
        prior.path.clone(),
        prior.fetch.clone(),
        prior.pin.clone(),
        prior.supported,
    );
    services()
        .lock()
        .unwrap()
        .insert(home.into(), restarted.clone());
    start(&restarted);
}

fn setup_wire(home: &Path, path: Option<&OsStr>) -> serde_json::Value {
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let state = tauri::async_runtime::block_on(crate::mcp::setup_tools::describe_setup_at(
        app.handle(),
        home.to_owned(),
        1,
        path.map(OsStr::to_os_string),
    ))
    .unwrap();
    let reply = crate::mcp::setup_tools::ToolEnvelope::Ok(state);
    let result = crate::mcp::handler::envelope_json(&reply).unwrap();
    let wire: serde_json::Value =
        serde_json::from_str(&result.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(wire["status"], "ok");
    wire["data"]["attribution"].clone()
}
fn launch_and_worktree_status(fixture: &crate::attribution::testing::Launch) -> serde_json::Value {
    let plan = crate::attribution::launch::prepare(
        &crate::attribution::launch::Request {
            codesense_home: fixture.home(),
            principal: &crate::agents::resolve::tests::alice(),
            cwd: &fixture.cwd,
            runtime: codesense_pty_core::agent_cli::AgentCli::Claude,
        },
        None,
    );
    let record = crate::install::worktree_index::read_record(
        fixture.home(),
        &crate::agents::resolve::tests::alice(),
        &fixture.cwd,
    )
    .unwrap()
    .unwrap();
    let row = crate::commands::worktree::WorktreeRow::story(
        crate::commands::worktree::managed_row_from_record(record, false),
    );
    let wire = serde_json::to_value(row).unwrap();
    assert_eq!(
        wire["attribution"]["status"],
        serde_json::to_value(plan.status).unwrap()
    );
    wire["attribution"]["status"].clone()
}

fn setup_wire_replacement_and_restart_pending_survive_service_restart() {
    use crate::attribution::testing::Launch;
    use codesense_pty_core::agent_cli::AgentCli;
    let fixture = Launch::new(AgentCli::Claude);
    crate::attribution::testing::argv0_binary(&fixture.home().join("bin/git-ai"));
    let _old_daemon = fixture.daemon_running();
    let (fetch, pin) = bundle(fixture.home(), PIN.expected_version, "");
    let service = bundle_service(fixture.home(), fetch.clone(), pin.clone());
    start(&service);
    let first = setup_wire(fixture.home(), None);
    assert_eq!(first["status"]["kind"], "off");
    assert_eq!(first["status"]["reason"], "notes_would_push");
    assert_eq!(first["previousBinary"]["version"], "1.7.5");
    assert_eq!(first["previousBinary"]["replacement"], PIN.expected_version);
    assert_eq!(
        first["previousBinary"]["path"],
        fixture
            .home()
            .join("bin/git-ai.prev-1.7.5")
            .to_str()
            .unwrap()
    );
    let warnings = first["warnings"].as_array().unwrap();
    assert!(warnings
        .iter()
        .any(|w| w.as_str().unwrap().starts_with("replaced git-ai 1.7.5 → ")));
    assert!(warnings
        .iter()
        .any(|w| w.as_str().unwrap().contains("restart pending")));
    services().lock().unwrap().remove(fixture.home());
    let reopened = setup_wire(fixture.home(), None);
    assert_eq!(reopened["previousBinary"], first["previousBinary"]);
    assert_eq!(reopened["status"], first["status"]);
    assert!(!reopened["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w.as_str().unwrap().starts_with("replaced git-ai")));
    let _restarted = bundle_service(fixture.home(), fetch, pin);
    assert_eq!(launch_and_worktree_status(&fixture)["kind"], "active");
    let ready = setup_wire(fixture.home(), None);
    assert_eq!(ready["status"]["kind"], "active");
    assert_eq!(ready["previousBinary"], first["previousBinary"]);
}

fn setup_wire_install_failures_survive_restart_and_restart_retries() {
    use crate::attribution::testing::Launch;
    use codesense_pty_core::agent_cli::AgentCli;
    for reason in ["bundle_missing", "sha_mismatch"] {
        let fixture = Launch::new(AgentCli::Claude);
        fixture.uninstall();
        let (fetch, mut pin) = bundle(fixture.home(), PIN.expected_version, "");
        if reason == "bundle_missing" {
            std::fs::remove_file(fetch.path.as_ref().unwrap()).unwrap();
        } else {
            pin.sha256 = "0".repeat(64);
        }
        let service = bundle_service(fixture.home(), fetch, pin);
        service.request(Trigger::Startup);
        assert!(matches!(
            service.wait(Duration::from_secs(30)),
            Some(AttributionStatus::InstallFailed { .. })
        ));
        let failed = setup_wire(fixture.home(), None);
        assert_eq!(failed["status"]["kind"], "install_failed");
        assert!(failed["status"]["message"]
            .as_str()
            .unwrap()
            .starts_with(reason));
        assert_eq!(launch_and_worktree_status(&fixture), failed["status"]);
        services().lock().unwrap().remove(fixture.home());
        assert_eq!(setup_wire(fixture.home(), None)["status"], failed["status"]);
        let (fetch, pin) = bundle(fixture.home(), PIN.expected_version, "");
        let restarted = bundle_service(fixture.home(), fetch, pin);
        assert_eq!(setup_wire(fixture.home(), None)["status"], failed["status"]);
        start(&restarted);
        assert!(!fixture.home().join("bin/.git-ai-install-failure").exists());
        assert_ne!(
            setup_wire(fixture.home(), None)["status"]["kind"],
            "install_failed"
        );
    }
}

fn setup_wire_newer_and_restored_copies_are_unsupported_and_actionable() {
    use crate::attribution::testing::Launch;
    use codesense_pty_core::agent_cli::AgentCli;
    for version in ["1.7.6", "1.8.0"] {
        let fixture = Launch::new(AgentCli::Claude);
        crate::attribution::testing::argv0_binary_with_version(
            &fixture.home().join("bin/git-ai"),
            version,
            "",
        );
        let service = bundle_service(
            fixture.home(),
            Arc::new(Bundled { path: None }),
            PIN.clone(),
        );
        start(&service);
        let doctor = setup_wire(fixture.home(), None);
        assert_eq!(doctor["status"]["kind"], "unsupported");
        assert_eq!(doctor["status"]["reason"], "newer");
        assert_eq!(launch_and_worktree_status(&fixture), doctor["status"]);
        services().lock().unwrap().remove(fixture.home());
        assert_eq!(setup_wire(fixture.home(), None)["status"], doctor["status"]);
    }
    let fixture = Launch::new(AgentCli::Claude);
    register_restored_bundle(fixture.home());
    let user_bin = fixture.home().join("user-bin");
    let log = fixture.home().join("user-bin-executions");
    script(
        &user_bin.join("git-ai"),
        &format!("echo executed >> '{}'\necho 1.7.5", log.display()),
    );
    let path = std::env::join_paths([user_bin]).unwrap();
    let restored = setup_wire(fixture.home(), Some(&path));
    assert_eq!(restored["status"]["kind"], "unsupported");
    assert_eq!(restored["status"]["reason"], "user_pinned");
    assert_eq!(launch_and_worktree_status(&fixture), restored["status"]);
    let warnings = restored["warnings"].as_array().unwrap();
    assert!(warnings.iter().any(
        |w| w.as_str().unwrap().contains("save_source_repo_attachment")
            && w.as_str().unwrap().contains(r#""attribution":"on""#)
    ));
    assert!(warnings
        .iter()
        .any(|w| w.as_str().unwrap().starts_with("user_copy_present")));
    services().lock().unwrap().remove(fixture.home());
    let restarted = setup_wire(fixture.home(), Some(&path));
    assert_eq!(restarted["status"], restored["status"]);
    assert_eq!(restarted["previousBinary"], restored["previousBinary"]);
    assert_eq!(restarted["previousBinary"]["version"], "1.7.5");
    assert!(
        !log.exists(),
        "reading the doctor must never execute the PATH git-ai"
    );
}

fn setup_wire_reports_installing_through_the_production_envelope() {
    let home = tempfile::tempdir().unwrap();
    let (fetch, pin) = paused_download("echo 1.7.5");
    let service = Service::new(home.path().into(), None, fetch.clone(), pin, true);
    services()
        .lock()
        .unwrap()
        .insert(home.path().into(), service.clone());
    service.request(Trigger::Startup);
    fetch.entered.wait();
    let installing = setup_wire(home.path(), None);
    fetch.release.wait();
    assert_eq!(service.wait(Duration::from_secs(30)), None);
    assert_eq!(installing["status"]["kind"], "installing");
    assert!(installing["previousBinary"].is_null());
}

fn replacement_notice_survives_restart_before_first_doctor_read() {
    use crate::attribution::testing::Launch;
    use codesense_pty_core::agent_cli::AgentCli;
    let fixture = Launch::new(AgentCli::Claude);
    crate::attribution::testing::argv0_binary(&fixture.home().join("bin/git-ai"));
    let (fetch, pin) = bundle(fixture.home(), PIN.expected_version, "");
    let installed = bundle_service(fixture.home(), fetch.clone(), pin.clone());
    start(&installed);
    // The app exits before any doctor call can consume the replacement notice.
    services().lock().unwrap().remove(fixture.home());
    let restarted = bundle_service(fixture.home(), fetch.clone(), pin.clone());
    start(&restarted);
    let _daemon = fixture.daemon_running();
    let first = setup_wire(fixture.home(), None);
    assert_eq!(first["status"]["kind"], "active");
    let expected = format!(
        "replaced git-ai 1.7.5 → {} (previous kept at {})",
        PIN.expected_version,
        fixture.home().join("bin/git-ai.prev-1.7.5").display()
    );
    assert!(
        first["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str() == Some(&expected)),
        "an unread replacement notice must survive app restart: {first}"
    );
    let second = setup_wire(fixture.home(), None);
    assert_eq!(second["status"]["kind"], "active");
    assert!(!second["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|warning| warning.as_str() == Some(&expected)));
    services().lock().unwrap().remove(fixture.home());
    let reopened = bundle_service(fixture.home(), fetch, pin);
    start(&reopened);
    let after_shown_restart = setup_wire(fixture.home(), None);
    assert_eq!(after_shown_restart["status"]["kind"], "active");
    assert!(
        !after_shown_restart["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str() == Some(&expected)),
        "an already shown notice must stay shown after restart"
    );
}

fn two_diagnostic_sessions_announce_a_pending_receipt_only_once() {
    use crate::attribution::testing::Launch;
    use codesense_pty_core::agent_cli::AgentCli;
    let fixture = Launch::new(AgentCli::Claude);
    crate::attribution::testing::argv0_binary(&fixture.home().join("bin/git-ai"));
    let (fetch, pin) = bundle(fixture.home(), PIN.expected_version, "");
    let service = bundle_service(fixture.home(), fetch, pin);
    start(&service);
    services().lock().unwrap().remove(fixture.home());
    let _daemon = fixture.daemon_running();
    let gate = Arc::new(std::sync::Barrier::new(3));
    let readers: Vec<_> = (0..2)
        .map(|_| {
            let home = fixture.home().to_owned();
            let gate = gate.clone();
            std::thread::spawn(move || {
                gate.wait();
                setup_wire(&home, None)
            })
        })
        .collect();
    gate.wait();
    let replies: Vec<_> = readers
        .into_iter()
        .map(|reader| reader.join().unwrap())
        .collect();
    for reply in &replies {
        assert_eq!(reply["status"]["kind"], "active");
    }
    assert_eq!(
        replies
            .iter()
            .flat_map(|reply| reply["warnings"].as_array().unwrap())
            .filter(|warning| warning.as_str().unwrap().starts_with("replaced git-ai"))
            .count(),
        1
    );
    let receipt: serde_json::Value = serde_json::from_slice(
        &std::fs::read(fixture.home().join("bin/.git-ai-previous")).unwrap(),
    )
    .unwrap();
    assert_eq!(receipt["notice"], "shown");
}

/// Deterministic completion seam shared with the checkpoint watch registration test.
pub(crate) fn complete_controlled_install(home: &Path, before_release: impl FnOnce()) {
    let (paused, pin) = paused_download("echo 1.7.5");
    let service = Service::new(home.into(), None, paused.clone(), pin, true);
    assert_eq!(
        service.request(Trigger::Startup),
        Some(AttributionStatus::Installing)
    );
    paused.entered.wait();
    before_release();
    paused.release.wait();
    assert_eq!(service.wait(Duration::from_secs(30)), None);
}
