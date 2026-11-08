// SPDX-License-Identifier: MIT
//! Packaged helper → daemon with no desktop connection, then snapshot recovery.
//! Default fixtures use synthetic callbacks. The explicitly ignored installed-
//! Claude fixture separately qualifies native startup hooks without a model prompt.
use codesense_host_proto::{
    read_host_greeting, write_frame, HostGreeting, PROTO_MAJOR, PROTO_MINOR,
};
use codesense_pty_core::activity::{State, SOCKET_ENV, TOKEN_ENV};
use codesense_pty_core::agent_cli::AgentCli;
use codesense_pty_core::domain::{PtySessionId, ViewerId};
use codesense_pty_core::protocol::{
    call_once, client_hello, host_pid, socket_path, Reply, Request,
};
use codesense_pty_core::spawn::SpawnIntent;
use codesense_pty_core::{SessionMeta, SessionRegistration};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const BINARY: &str = env!("CARGO_BIN_EXE_codesense-session-server");

fn daemon_command() -> Command {
    static WORKER: std::sync::Once = std::sync::Once::new();
    WORKER.call_once(|| {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let built = Command::new("pnpm")
            .args(["run", "build:emulator"])
            .current_dir(&repo)
            .status()
            .expect("build codesense-emulator first: pnpm run build:emulator");
        assert!(
            built.success(),
            "build codesense-emulator first: pnpm run build:emulator"
        );
        let compiler = Command::new("rustc").arg("-vV").output().unwrap();
        assert!(compiler.status.success());
        let version = String::from_utf8(compiler.stdout).unwrap();
        let triple = version
            .split('\n')
            .find_map(|line| line.strip_prefix("host: "))
            .expect("rustc names its host triple");
        let worker = repo
            .join("src-tauri/binaries")
            .join(format!("codesense-emulator-{triple}"));
        std::fs::copy(
            worker,
            codesense_session_server::host::emulator_worker_beside(Path::new(BINARY)),
        )
        .expect("install the built codesense-emulator beside the fixture server");
    });
    let mut daemon = Command::new(BINARY);
    // A test run inside a CodeSense terminal inherits its server's wake-hold requirement,
    // which holds every notice until a desktop decides: these daemons have none.
    daemon.env_remove("CODESENSE_LOCAL_WAKE_HOLDS");
    daemon
}

/// Warm the newly written script without bypassing the host probe's liveness bound.
fn warm_cli_version(program: &Path, expected: &str) {
    let version = cli_version_within(program, Duration::from_secs(10))
        .unwrap_or_else(|error| panic!("{} --version warm-up failed: {error}", program.display()));
    assert!(
        version.status.success(),
        "{} --version failed: {}",
        program.display(),
        String::from_utf8_lossy(&version.stderr)
    );
    assert_eq!(String::from_utf8(version.stdout).unwrap().trim(), expected);
}

fn cli_version_within(program: &Path, budget: Duration) -> std::io::Result<std::process::Output> {
    use std::io::{Read, Seek};
    use std::process::Stdio;

    // Regular files avoid a second unbounded wait for pipe EOF if a descendant
    // inherits the probe's output descriptors. Stdin is never interactive.
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    let mut child = Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .spawn()?;
    let deadline = Instant::now() + budget;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            result => {
                let killed = child.kill();
                let reaped = child.wait();
                stderr.rewind()?;
                let mut diagnostic = String::new();
                let _ = stderr.read_to_string(&mut diagnostic);
                let (kind, cause) = match result {
                    Ok(None) => (std::io::ErrorKind::TimedOut, format!("exceeded {budget:?}")),
                    Err(error) => (error.kind(), error.to_string()),
                    Ok(Some(_)) => unreachable!(),
                };
                let message = format!(
                    "{} --version {cause}; kill={killed:?}; reap={reaped:?}; stderr: {diagnostic}",
                    program.display()
                );
                return Err(std::io::Error::new(kind, message));
            }
        }
    };
    stdout.rewind()?;
    stderr.rewind()?;
    let mut output = std::process::Output {
        status,
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    stdout.read_to_end(&mut output.stdout)?;
    stderr.read_to_end(&mut output.stderr)?;
    Ok(output)
}

#[test]
fn version_warmup_bounds_a_stalled_script_and_captures_probe_output() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let program = root.path().join("version-probe");
    std::fs::write(
        &program,
        "#!/bin/sh\nprintf 'codex-cli 0.159.3'\nprintf 'probe diagnostic' >&2\n",
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let output = cli_version_within(&program, Duration::from_secs(10)).unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"codex-cli 0.159.3");
    assert_eq!(output.stderr, b"probe diagnostic");

    std::fs::write(&program, "#!/bin/sh\nexec sleep 60\n").unwrap();
    let error = cli_version_within(&program, Duration::from_millis(100)).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(error.to_string().contains("--version exceeded"));
}

struct Daemon(tempfile::TempDir);
impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(pid) = host_pid(self.0.path()) {
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
    }
}

fn connect(root: &Path) -> UnixStream {
    let mut stream = UnixStream::connect(socket_path(root)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let (HostGreeting::Supported { hello }, _) = read_host_greeting(&mut stream).unwrap() else {
        panic!("unsupported fixture host")
    };
    // `tauri-build` (the app's build script) copies the prebuilt sidecar in
    // `src-tauri/binaries/` over cargo's `target/<profile>/<name>`, the very
    // path `CARGO_BIN_EXE_*` names. A stale sidecar there predates whatever
    // these fixtures exercise and fails them far from the cause.
    assert_eq!(
        (hello.proto_major, hello.proto_minor),
        (PROTO_MAJOR, PROTO_MINOR),
        "{BINARY} is not this build (a stale Tauri sidecar copy from src-tauri/binaries?): \
         rebuild it with `cargo build -p codesense-session-server` or refresh the sidecar \
         with scripts/build-session-server.sh"
    );
    let mut hello = client_hello(
        hello.instance_id,
        "activity-fixture",
        &ViewerId::new("activity-fixture"),
    );
    hello.client_id = Some(format!("activity-fixture:{}", std::process::id()));
    write_frame(&mut stream, &hello.frame().unwrap()).unwrap();
    stream
}

fn call(stream: &mut UnixStream, request: Request) -> Reply {
    call_once(&mut stream.try_clone().unwrap(), stream, 1, request)
        .unwrap()
        .unwrap()
}

#[test]
fn packaged_activity_helper_retains_observation_while_desktop_is_absent() {
    let daemon = Daemon(
        tempfile::Builder::new()
            .prefix("activity")
            .tempdir_in("/tmp")
            .unwrap(),
    );
    assert!(daemon_command()
        .args(["--root"])
        .arg(daemon.0.path())
        .env("SHELL", "/bin/sh")
        .env(
            codesense_session_server::exit_with::EXIT_WITH_PID_ENV,
            std::process::id().to_string()
        )
        .status()
        .unwrap()
        .success());
    let root = daemon.0.path();
    let mut desktop = connect(root);
    let session = PtySessionId::new("activity-session");
    let Reply::ActivityWriter { writer } = call(
        &mut desktop,
        Request::PrepareActivity {
            session: session.clone(),
            provider: AgentCli::Claude,
        },
    ) else {
        panic!("writer")
    };
    let trigger = root.join("trigger");
    let finished = root.join("reported");
    let started = r#"{"provider":"claude","event":"started","scope":"main","reportId":"start","providerSession":"provider","turn":null,"tool":null}"#;
    let submitted = r#"{"provider":"claude","event":"submitted","scope":"main","reportId":"submit","providerSession":"provider","turn":null,"tool":null}"#;
    let script = format!("while [ ! -f {} ]; do sleep 0.01; done; printf %s {} | {} --activity-hook; printf %s {} | {} --activity-hook; touch {}; sleep 60",
        codesense_pty_core::shell_quote(trigger.to_str().unwrap()), codesense_pty_core::shell_quote(started), codesense_pty_core::shell_quote(BINARY),
        codesense_pty_core::shell_quote(submitted), codesense_pty_core::shell_quote(BINARY), codesense_pty_core::shell_quote(finished.to_str().unwrap()));
    let intent = SpawnIntent {
        remote_launch: None,
        command: Some(script),
        argv: None,
        shell: Some("/bin/sh".into()),
        cwd: root.to_string_lossy().into_owned(),
        cols: 80,
        rows: 24,
        shell_integration: false,
        starship_preset: None,
        api_port: None,
        env_remove: vec![],
        env: vec![
            (SOCKET_ENV.into(), writer.socket.clone()),
            (TOKEN_ENV.into(), writer.token.clone()),
        ],
        agent_env: Vec::new(),
        login_path: None,
    };
    let registration = SessionRegistration {
        session_id: session.as_str().into(),
        meta: SessionMeta::default(),
        home_cwd: Some(root.into()),
        osc_enabled: false,
        is_pool_member: false,
        scrollback: 100,
    };
    assert!(matches!(
        call(
            &mut desktop,
            Request::Spawn {
                intent,
                registration
            }
        ),
        Reply::Spawned { .. }
    ));
    drop(desktop);
    std::fs::write(&trigger, []).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !finished.exists() {
        assert!(Instant::now() < deadline, "helper did not finish");
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut reconnected = connect(root);
    let Reply::ActivitySnapshot { snapshots } = call(&mut reconnected, Request::ActivitySnapshot)
    else {
        panic!("snapshot")
    };
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].identity, writer.identity);
    assert_eq!(snapshots[0].state, State::Thinking);
    // PTY output contributes bounded activity revisions independently of hooks.
    assert!(snapshots[0].revision >= 3);
    assert_eq!(snapshots[0].tool_open, Some(false));
    assert!(snapshots[0].activity_age_ms.is_some());
    let _ = call(&mut reconnected, Request::Kill { session });
}

/// `InboxWaitV1` on a host whose tabs no desktop relays (this Mac's terminal server):
/// the agent's inbox hook runs the host's own helper, which waits at the host's
/// activity ingress as the agent's waiter; the desktop sees it waiting, pushes a notice,
/// and the helper hands it to the agent (stderr, exit 2) and the host reports it told.
/// A waiter naming another process than the session's agent is refused.
#[test]
fn an_agents_inbox_hook_waits_on_its_host_and_is_handed_the_pushed_notice() {
    use codesense_pty_core::domain::TerminalDomainEvent;
    use codesense_pty_core::protocol::{Inbound, Ingress};
    let daemon = Daemon(
        tempfile::Builder::new()
            .prefix("inbox-wait")
            .tempdir_in("/tmp")
            .unwrap(),
    );
    assert!(daemon_command()
        .args(["--root"])
        .arg(daemon.0.path())
        .env("SHELL", "/bin/sh")
        .env(
            codesense_session_server::exit_with::EXIT_WITH_PID_ENV,
            std::process::id().to_string()
        )
        .status()
        .unwrap()
        .success());
    let root = daemon.0.path();
    // This Mac's terminal server says it hands its agents their notices.
    let mut greeting = UnixStream::connect(socket_path(root)).unwrap();
    let (HostGreeting::Supported { hello }, _) = read_host_greeting(&mut greeting).unwrap() else {
        panic!("unsupported fixture host")
    };
    assert!(hello.supports(&codesense_host_proto::Capability::InboxWaitV1));
    drop(greeting);
    let mut desktop = connect(root);
    // It hears the host's pushes, inbox reports among them.
    assert_eq!(call(&mut desktop, Request::Listen), Reply::Done);
    let session = PtySessionId::new("inbox-session");
    assert_eq!(
        call(
            &mut desktop,
            Request::LocalMcpSessions {
                sessions: vec![session.clone()]
            },
        ),
        Reply::Done
    );
    let Reply::ActivityWriter { writer } = call(
        &mut desktop,
        Request::PrepareActivity {
            session: session.clone(),
            provider: AgentCli::Claude,
        },
    ) else {
        panic!("writer")
    };
    let quote = |path: &Path| codesense_pty_core::shell_quote(path.to_str().unwrap());
    let (stray, status, notice) = (root.join("stray"), root.join("status"), root.join("notice"));
    // The PTY's process is the agent: a stray waiter names another process and is
    // refused at once; the agent's own runs the very hook Claude is given (`$PPID` is
    // this shell, the agent) and waits for its mail.
    let hook = codesense_pty_core::agent_cli::inbox_hook_command(AgentCli::Claude, "Stop");
    let script = format!(
        "{helper} --inbox-wait claude Stop 1 </dev/null 2>/dev/null; echo $? > {stray}; \
         sh -c {hook} </dev/null 2>{notice}; echo $? > {status}; sleep 60",
        helper = codesense_pty_core::shell_quote(BINARY),
        hook = codesense_pty_core::shell_quote(&hook),
        stray = quote(&stray),
        notice = quote(&notice),
        status = quote(&status),
    );
    let intent = SpawnIntent {
        remote_launch: None,
        command: Some(script),
        argv: None,
        shell: Some("/bin/sh".into()),
        cwd: root.to_string_lossy().into_owned(),
        cols: 80,
        rows: 24,
        shell_integration: false,
        starship_preset: None,
        api_port: None,
        env_remove: vec![],
        env: vec![
            (SOCKET_ENV.into(), writer.socket.clone()),
            (TOKEN_ENV.into(), writer.token.clone()),
        ],
        agent_env: Vec::new(),
        login_path: None,
    };
    let registration = SessionRegistration {
        session_id: session.as_str().into(),
        meta: SessionMeta::default(),
        home_cwd: Some(root.into()),
        osc_enabled: false,
        is_pool_member: false,
        scrollback: 100,
    };
    assert!(matches!(
        call(
            &mut desktop,
            Request::Spawn {
                intent,
                registration
            }
        ),
        Reply::Spawned { .. }
    ));
    let waiting = |desktop: &mut UnixStream| {
        let Reply::InboxWaiters { waiters } = call(desktop, Request::InboxWaiters) else {
            panic!("waiters")
        };
        waiters.iter().any(|w| w.session == session && w.waiting)
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while !waiting(&mut desktop) {
        assert!(Instant::now() < deadline, "the agent's waiter never waited");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(std::fs::read_to_string(&stray).unwrap().trim(), "0");
    assert!(!status.exists(), "nothing woke the agent yet");
    let text = "You have 1 new message. Call read_messages with session_id inbox-session.";
    assert_eq!(
        call(
            &mut desktop,
            Request::InboxMail {
                session: session.clone(),
                notice: text.into(),
                token: "tok-1".into(),
            }
        ),
        Reply::Done
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::fs::read_to_string(&status)
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        assert!(
            Instant::now() < deadline,
            "the waiter never handed the notice over"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(std::fs::read_to_string(&status).unwrap().trim(), "2");
    assert_eq!(std::fs::read_to_string(&notice).unwrap().trim_end(), text);
    // The desktop hears it told, once the agent had it.
    let mut ingress = Ingress::default();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "no told report");
        let frame = codesense_host_proto::read_frame(&mut desktop)
            .unwrap()
            .unwrap();
        if let Ok(Some(Inbound::Event(TerminalDomainEvent::InboxAck {
            session: told,
            token,
        }))) = ingress.accept(&frame)
        {
            assert_eq!((told, token.as_str()), (session.clone(), "tok-1"));
            break;
        }
    }
    let _ = call(&mut desktop, Request::Kill { session });
}

/// A fake qualified Codex, as its engine (an app-server that runs its hooks and starts the
/// turns it is asked to) and as its TUI attached to it (which records whatever is typed into
/// the terminal). See the fixture for what each does.
const FAKE_CODEX: &str = include_str!("fixtures/fake-codex.py");

/// A Codex its host was handed whole (as this Mac's app launches one) runs on its engine,
/// prepared by its host with its observer and inbox checks. Idle, it is started a turn
/// with the pushed notice, headed by its wake key, through its engine, and reported told
/// once the engine took it. Mail that comes while it works is not handed to its engine, nor
/// to a subagent: its next tool call's check hands it over, in the same turn. Mail after its
/// last tool call goes as a turn of its own once the turn ends. Nothing is ever typed into
/// its terminal.
#[cfg(target_os = "macos")]
#[test]
fn an_idle_codex_is_started_a_turn_through_its_engine_and_a_working_one_is_handed_its_notice_by_a_check(
) {
    codex_inbox_round_trip(false, false, false);
}

/// The same for a `codex` typed by hand in a shell tab: its shell's host claims it and
/// prepares its hooks.
#[cfg(target_os = "macos")]
#[test]
fn a_hand_typed_codex_hears_its_mail_as_a_launched_one_does() {
    codex_inbox_round_trip(true, false, false);
}

/// Both launch paths use the host's captured native home and accepted hook thread.
#[cfg(target_os = "macos")]
#[test]
fn cx4_native_output_is_identical_through_the_packaged_host_for_both_launch_paths() {
    codex_inbox_round_trip(false, true, false);
    codex_inbox_round_trip(true, true, false);
}

#[cfg(target_os = "macos")]
#[test]
fn an_old_codex_runs_but_warns_that_inbox_wake_needs_an_update() {
    codex_inbox_round_trip(false, false, true);
    codex_inbox_round_trip(true, false, true);
}

/// Exercises the packaged launch path, including the shell wrapper.
#[cfg(target_os = "macos")]
fn codex_inbox_round_trip(hand_typed: bool, output: bool, old: bool) {
    use codesense_host_proto::agent_cli::NativeLaunch;
    use codesense_pty_core::domain::TerminalDomainEvent;
    use codesense_pty_core::protocol::{Inbound, Ingress};
    let daemon = Daemon(
        tempfile::Builder::new()
            .prefix("inbox-codex")
            .tempdir_in("/tmp")
            .unwrap(),
    );
    assert!(daemon_command()
        .args(["--root"])
        .arg(daemon.0.path())
        .env("SHELL", "/bin/sh")
        .env(
            codesense_session_server::exit_with::EXIT_WITH_PID_ENV,
            std::process::id().to_string()
        )
        .status()
        .unwrap()
        .success());
    let root = daemon.0.path();
    let fake = root.join("codex");
    let fixture = if old {
        FAKE_CODEX.replace("codex-cli 0.162.1", "codex-cli 0.150.0")
    } else {
        FAKE_CODEX.to_string()
    };
    std::fs::write(&fake, fixture).unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    warm_cli_version(
        &fake,
        if old {
            "codex-cli 0.150.0"
        } else {
            "codex-cli 0.162.1"
        },
    );
    // A launched one runs as Codex from npm on Linux does: its socket named by a link, its
    // hooks run by its engine's child. A hand-typed one runs as on macOS.
    if !hand_typed {
        std::fs::write(root.join("launcher"), []).unwrap();
    }
    let mut desktop = connect(root);
    desktop
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    assert_eq!(call(&mut desktop, Request::Listen), Reply::Done);
    let session = PtySessionId::new("inbox-codex");
    assert_eq!(
        call(
            &mut desktop,
            Request::LocalMcpSessions {
                sessions: vec![session.clone()]
            },
        ),
        Reply::Done
    );
    let quoted = codesense_pty_core::shell_quote(fake.to_str().unwrap());
    let (remote_launch, command, argv, shell) = if hand_typed {
        (
            None,
            // Typed once the host has registered the tab (the test writes `go`), as a person
            // types into a tab that is already open; the shell stays its parent (a last
            // command would replace it).
            Some(format!(
                "while [ ! -f {root_q}/go ]; do sleep 0.01; done; \
                 \"$CODESENSE_ACTIVITY_HOST/activity-helper\" --activity-run codex -- {quoted} -- hi; \
                 sleep 60",
                root_q = codesense_pty_core::shell_quote(root.to_str().unwrap()),
            )),
            None,
            Some("/bin/sh".into()),
        )
    } else {
        (
            Some(NativeLaunch::Cli(AgentCli::Codex)),
            None,
            // With an extra root, as a team-workspace launch has: its TUI, attached, refuses
            // one, so it reaches the engine alone.
            Some(vec![
                fake.to_string_lossy().into_owned(),
                "--add-dir".into(),
                root.to_string_lossy().into_owned(),
                "--".into(),
                "hi".into(),
            ]),
            None,
        )
    };
    let output_id = "01a15600-0001-7000-8000-000000000002";
    let native_home = root.join("native-home");
    if output {
        let logs = native_home.join("sessions/2026/10/19");
        std::fs::create_dir_all(&logs).unwrap();
        let record=format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{output_id}\"}}}}\n{{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"this session only\"}}]}}}}\n");
        std::fs::write(logs.join(format!("rollout-test-{output_id}.jsonl")), record).unwrap();
        std::fs::write(
            logs.join("rollout-newest-other.jsonl"),
            "must not capture sibling",
        )
        .unwrap();
    }
    let intent = SpawnIntent {
        remote_launch,
        command,
        argv,
        shell,
        cwd: root.to_string_lossy().into_owned(),
        cols: 80,
        rows: 24,
        shell_integration: false,
        starship_preset: None,
        api_port: None,
        env_remove: vec![],
        env: if output {
            vec![
                (
                    "CODEX_HOME".into(),
                    native_home.to_string_lossy().into_owned(),
                ),
                ("CODESENSE_TEST_THREAD_ID".into(), output_id.into()),
            ]
        } else {
            vec![]
        },
        agent_env: Vec::new(),
        login_path: None,
    };
    let registration = SessionRegistration {
        session_id: session.as_str().into(),
        meta: SessionMeta::default(),
        home_cwd: Some(root.into()),
        osc_enabled: false,
        is_pool_member: false,
        scrollback: 100,
    };
    assert!(matches!(
        call(
            &mut desktop,
            Request::Spawn {
                intent,
                registration
            }
        ),
        Reply::Spawned { .. }
    ));
    std::fs::write(root.join("go"), []).unwrap();
    let until = |what: &str, mut done: Box<dyn FnMut() -> bool + '_>| {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !done() {
            assert!(Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    let file = |name: &str| std::fs::read_to_string(root.join(name)).unwrap_or_default();
    if old {
        until(
            "old Codex starts without hooks",
            Box::new(|| root.join("unobserved").exists()),
        );
        let Reply::ActivitySnapshot { snapshots } = call(&mut desktop, Request::ActivitySnapshot)
        else {
            panic!("snapshot")
        };
        let snapshot = snapshots
            .iter()
            .find(|s| s.session == session.as_str())
            .unwrap();
        assert!(!snapshot.observed);
        assert_eq!(snapshot.reason.as_deref(), Some("inbox-update-required"));
        let Reply::InboxWaiters { waiters } = call(&mut desktop, Request::InboxWaiters) else {
            panic!("waiters")
        };
        assert!(!waiters.iter().any(|w| w.session == session && w.waiting));
        assert!(!root.join("engine").exists());
        let _ = call(&mut desktop, Request::Kill { session });
        return;
    }
    until(
        "the fake Codex never started",
        Box::new(|| root.join("ready").exists()),
    );
    if output {
        let Reply::ActivitySnapshot { snapshots } = call(&mut desktop, Request::ActivitySnapshot)
        else {
            panic!("snapshot")
        };
        let snapshot = snapshots
            .iter()
            .find(|s| s.session == session.as_str())
            .unwrap();
        let output = call(
            &mut desktop,
            Request::SessionOutput {
                session: session.clone(),
                identity: snapshot.identity.clone(),
                turn_generation: snapshot.turn_generation,
            },
        );
        let Reply::SessionOutput {
            record: Some(record),
        } = output
        else {
            panic!("native output not captured: {output:?}")
        };
        assert_eq!(record.session_id, output_id);
        assert!(record.raw.contains("this session only"));
        assert!(!record.raw.contains("sibling"));
        let parsed = record.format.parse_native(&record.raw).unwrap();
        assert_eq!(parsed.turns[0].text.as_deref(), Some("this session only"));
        assert_eq!(
            call(
                &mut desktop,
                Request::SessionOutput {
                    session: session.clone(),
                    identity: snapshot.identity.clone(),
                    turn_generation: snapshot.turn_generation + 1
                }
            ),
            Reply::SessionOutput { record: None }
        );
        let _ = call(&mut desktop, Request::Kill { session });
        return;
    }
    // Its first turn ran its observer and its checks: with no mail held, a check hands
    // nothing over and exits 0.
    let log = file("hooks.log");
    for event in ["UserPromptSubmit", "Stop"] {
        assert!(
            log.split_terminator('\n')
                .any(|line| line.contains(&format!("\"event\": \"{event}\""))),
            "{event}: {log}; argv: {}",
            file("argv")
        );
    }
    assert!(
        log.split_terminator('\n')
            .all(|line| line.contains("\"code\": 0")),
        "{log}"
    );
    let first: Vec<serde_json::Value> = log
        .split_terminator('\n')
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        first.iter().any(|hook| hook["event"] == "UserPromptSubmit"
            && hook["check"] == true
            && hook["stdout"] == ""),
        "its prompt's check ran and handed nothing over: {log}"
    );
    // Idle at its prompt, it reads as one new mail reaches.
    until(
        "the idle Codex never read as reached",
        Box::new(|| {
            let Reply::InboxWaiters { waiters } = call(&mut desktop, Request::InboxWaiters) else {
                panic!("waiters")
            };
            waiters.iter().any(|w| w.session == session && w.waiting)
        }),
    );
    let text = "You have 1 new message. Call read_messages with session_id inbox-codex.";
    assert_eq!(
        call(
            &mut desktop,
            Request::InboxMail {
                session: session.clone(),
                notice: text.into(),
                token: "tok-1".into(),
            }
        ),
        Reply::Done
    );
    until(
        "the notice never started a turn",
        Box::new(|| !file("prompt").is_empty()),
    );
    assert_eq!(file("prompt"), format!("[inbox tok-1]\n{text}"));
    // What the host reports of each notice, in order: told, released or uncertain.
    let mut ingress = Ingress::default();
    let mut fate = |events: &mut UnixStream| {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            assert!(Instant::now() < deadline, "no report");
            let frame = codesense_host_proto::read_frame(events)
                .unwrap_or_else(|error| panic!("no report: {error}; hooks: {}", file("hooks.log")))
                .unwrap();
            if let Ok(Some(Inbound::Event(event))) = ingress.accept(&frame) {
                match event {
                    TerminalDomainEvent::InboxAck { token, .. } => return ("told", token),
                    TerminalDomainEvent::InboxRelease { token, .. } => return ("released", token),
                    TerminalDomainEvent::InboxUncertain { token, .. } => {
                        return ("uncertain", token)
                    }
                    _ => {}
                }
            }
        }
    };
    assert_eq!(fate(&mut desktop), ("told", "tok-1".into()));
    // Working on that turn, it is started nothing: its host keeps the notice for its checks.
    let second = "You have 2 new messages. Call read_messages with session_id inbox-codex.";
    assert_eq!(
        call(
            &mut desktop,
            Request::InboxMail {
                session: session.clone(),
                notice: second.into(),
                token: "tok-2".into(),
            }
        ),
        Reply::Done
    );
    // Its next tool call's check hands it over as context, in the same turn.
    std::fs::write(root.join("finish-tool"), []).unwrap();
    assert_eq!(fate(&mut desktop), ("told", "tok-2".into()));
    // A second message in the same turn: its next tool call's check hands that one over.
    let fourth = "You have 4 new messages. Call read_messages with session_id inbox-codex.";
    assert_eq!(
        call(
            &mut desktop,
            Request::InboxMail {
                session: session.clone(),
                notice: fourth.into(),
                token: "tok-4".into(),
            }
        ),
        Reply::Done
    );
    std::fs::write(root.join("finish-tool-2"), []).unwrap();
    assert_eq!(fate(&mut desktop), ("told", "tok-4".into()));
    until(
        "the turn never ended",
        Box::new(|| root.join("stopped").exists()),
    );
    let handed: Vec<serde_json::Value> = file("hooks.log")
        .split_terminator('\n')
        .map(|line| serde_json::from_str(line).unwrap())
        .filter(|hook: &serde_json::Value| !hook["stdout"].as_str().unwrap().is_empty())
        .collect();
    // Each of the turn's two tool calls was handed its notice, neither to its subagent.
    assert_eq!(handed.len(), 2, "{handed:?}");
    for (hook, notice) in handed.iter().zip([second, fourth]) {
        assert_eq!(hook["event"], "PostToolUse");
        assert_eq!(
            hook["agent"],
            serde_json::Value::Null,
            "not to its subagent"
        );
        assert_eq!(hook["code"], 0);
        let output: serde_json::Value =
            serde_json::from_str(hook["stdout"].as_str().unwrap()).unwrap();
        assert_eq!(
            output["hookSpecificOutput"],
            serde_json::json!({"hookEventName": "PostToolUse", "additionalContext": notice})
        );
    }
    // No turn was started over the running one.
    assert_eq!(file("prompt"), format!("[inbox tok-1]\n{text}"));
    // Mail after its last tool call goes once the turn has ended, as a turn of its own.
    let third = "You have 3 new messages. Call read_messages with session_id inbox-codex.";
    assert_eq!(
        call(
            &mut desktop,
            Request::InboxMail {
                session: session.clone(),
                notice: third.into(),
                token: "tok-3".into(),
            }
        ),
        Reply::Done
    );
    assert_eq!(fate(&mut desktop), ("told", "tok-3".into()));
    until(
        "the turn-end notice never started a turn",
        Box::new(|| file("prompt") == format!("[inbox tok-3]\n{third}")),
    );
    // Its TUI ran attached to its engine, and nothing was ever typed into its terminal.
    assert!(file("tui").contains("--remote unix://"), "{}", file("tui"));
    assert_eq!(file("typed"), "");
    let _ = call(&mut desktop, Request::Kill { session });
    // Its engine ends with it, and its socket goes too.
    let engine = file("engine");
    let (pid, endpoint) = engine.split_once(' ').unwrap();
    let pid: i32 = pid.parse().unwrap();
    until(
        "its engine outlived it",
        Box::new(move || {
            let gone = unsafe { libc::kill(pid, 0) } != 0;
            gone && std::fs::symlink_metadata(endpoint).is_err()
        }),
    );
}

#[test]
fn managed_shell_relaunch_preserves_user_hook_and_replaces_invocation() {
    let daemon = Daemon(
        tempfile::Builder::new()
            .prefix("activity-shell")
            .tempdir_in("/tmp")
            .unwrap(),
    );
    assert!(daemon_command()
        .arg("--root")
        .arg(daemon.0.path())
        .env("SHELL", "/bin/sh")
        .env(
            codesense_session_server::exit_with::EXIT_WITH_PID_ENV,
            std::process::id().to_string()
        )
        .status()
        .unwrap()
        .success());
    let root = daemon.0.path();
    let quote = |path: &Path| codesense_pty_core::shell_quote(path.to_str().unwrap());
    let fake = root.join("provider.py");
    // A callback fixture, not an actual Claude runtime. Both user and observer
    // commands execute synchronously exactly as configured in the private file.
    std::fs::write(
        &fake,
        r#"import json,os,pathlib,subprocess,sys,time
root=pathlib.Path(sys.argv[1]); run=sys.argv[2]
settings=json.loads(pathlib.Path(sys.argv[sys.argv.index('--settings')+1]).read_text())
for event in ['SessionStart','UserPromptSubmit']:
    payload=json.dumps({'hook_event_name':event,'session_id':'same-provider-resume-id'})
    for group in settings['hooks'][event]:
        for hook in group['hooks']:
            # An async hook (the inbox waiter) runs beside the turn, as Claude runs it.
            if hook.get('asyncRewake'): continue
            subprocess.run(hook['command'],shell=True,input=payload,text=True,check=True)
(root/('ready'+run)).touch()
while not (root/('release'+run)).exists(): time.sleep(0.01)
"#,
    )
    .unwrap();
    let user_marker = root.join("user-marker");
    let source = root.join("user-settings.json");
    let settings = serde_json::json!({"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":format!("printf x >> {}", quote(&user_marker))}]}]},"permissions":{"allow":["Read"]}}).to_string();
    std::fs::write(&source, &settings).unwrap();
    let command = format!("for run in 1 2; do while [ ! -f {root}/command$run ]; do sleep 0.01; done; . {root}/command$run; code=$?; printf '\\033]133;D;%d\\007\\033]133;A\\007' \"$code\"; touch {root}/done$run; done; sleep 60", root=quote(root));
    let session = PtySessionId::new("shell");
    let mut desktop = connect(root);
    assert!(matches!(
        call(
            &mut desktop,
            Request::Spawn {
                intent: SpawnIntent {
                    remote_launch: None,
                    command: Some(command),
                    argv: None,
                    shell: Some("/bin/sh".into()),
                    cwd: root.to_string_lossy().into_owned(),
                    cols: 80,
                    rows: 24,
                    shell_integration: false,
                    starship_preset: None,
                    api_port: None,
                    env_remove: vec![],
                    env: vec![],
                    agent_env: Vec::new(),
                    login_path: None,
                },
                registration: SessionRegistration {
                    session_id: session.as_str().into(),
                    meta: SessionMeta::default(),
                    home_cwd: Some(root.into()),
                    osc_enabled: true,
                    is_pool_member: false,
                    scrollback: 100
                },
            }
        ),
        Reply::Spawned { .. }
    ));
    let wait = |path: &Path| {
        let until = Instant::now() + Duration::from_secs(8);
        while !path.exists() {
            assert!(
                Instant::now() < until,
                "fixture timed out: {}",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    let mut prior: Option<codesense_pty_core::activity::Identity> = None;
    let mut prior_directory: Option<std::path::PathBuf> = None;
    for run in 1..=2 {
        let Reply::ShellActivity { launch } = call(
            &mut desktop,
            Request::PrepareShellActivity {
                session: session.clone(),
                provider: AgentCli::Claude,
            },
        ) else {
            panic!("shell launch unavailable");
        };
        if let Some(prior) = &prior {
            assert_eq!(
                launch.identity.session_incarnation,
                prior.session_incarnation
            );
            assert_ne!(launch.identity.invocation_id, prior.invocation_id);
        }
        if let Some(directory) = &prior_directory {
            assert!(
                !directory.exists(),
                "only retired launch artifacts should be cleaned"
            );
        }
        prior = Some(launch.identity.clone());
        prior_directory = Some(Path::new(&launch.bootstrap).parent().unwrap().into());
        drop(desktop);
        let line = format!(
            "{} --activity-exec {} -- /usr/bin/python3 {} {} {} --settings {}\n",
            quote(Path::new(&launch.helper)),
            quote(Path::new(&launch.bootstrap)),
            quote(&fake),
            quote(root),
            run,
            quote(&source)
        );
        std::fs::write(root.join(format!("command{run}")), line).unwrap();
        wait(&root.join(format!("ready{run}")));
        assert!(
            !Path::new(&launch.bootstrap).exists(),
            "capability file is consumed once"
        );
        desktop = connect(root);
        let Reply::ActivitySnapshot { snapshots } = call(&mut desktop, Request::ActivitySnapshot)
        else {
            panic!("snapshot unavailable");
        };
        assert_eq!(snapshots[0].identity, launch.identity);
        assert_eq!(snapshots[0].state, State::Thinking);
        assert_eq!(
            std::fs::read_to_string(&user_marker).unwrap(),
            "x".repeat(run)
        );
        assert_eq!(std::fs::read_to_string(&source).unwrap(), settings);
        std::fs::write(root.join(format!("release{run}")), []).unwrap();
        wait(&root.join(format!("done{run}")));
    }
    let _ = call(&mut desktop, Request::Kill { session });
    assert!(source.exists(), "user settings survive cleanup");
}

/// A `claude` typed by hand in a host's shell runs through the helper the host
/// names in the shell's environment, claims an invocation by ancestry (no
/// desktop, no token, no grant), keeps the person's own `--settings` hooks,
/// and its hooks fold into the same states as a launched one.
#[test]
fn hand_typed_cli_reports_through_the_shells_own_host() {
    let daemon = Daemon(
        tempfile::Builder::new()
            .prefix("activity-typed")
            .tempdir_in("/tmp")
            .unwrap(),
    );
    assert!(daemon_command()
        .arg("--root")
        .arg(daemon.0.path())
        .env("SHELL", "/bin/sh")
        .env(
            codesense_session_server::exit_with::EXIT_WITH_PID_ENV,
            std::process::id().to_string()
        )
        .status()
        .unwrap()
        .success());
    let root = daemon.0.path();
    let quote = |path: &Path| codesense_pty_core::shell_quote(path.to_str().unwrap());
    let fake = root.join("provider.py");
    std::fs::write(
        &fake,
        r#"import json,pathlib,subprocess,sys,time
root=pathlib.Path(sys.argv[1])
settings=json.loads(pathlib.Path(sys.argv[sys.argv.index('--settings')+1]).read_text())
for event,extra in [('SessionStart',{}),('UserPromptSubmit',{}),('Notification',{'notification_type':'permission_prompt'})]:
    payload=json.dumps(dict(hook_event_name=event,session_id='typed',**extra))
    for group in settings['hooks'][event]:
        for hook in group['hooks']:
            # An async hook (the inbox waiter) runs beside the turn, as Claude runs it.
            if hook.get('asyncRewake'): continue
            subprocess.run(hook['command'],shell=True,input=payload,text=True,check=True)
(root/'ready').touch()
while not (root/'release').exists(): time.sleep(0.01)
"#,
    )
    .unwrap();
    let user_marker = root.join("user-marker");
    let source = root.join("user-settings.json");
    let settings = serde_json::json!({"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":format!("printf x >> {}", quote(&user_marker))}]}]}}).to_string();
    std::fs::write(&source, &settings).unwrap();
    // What the shell wrapper runs for `claude --settings <file>`.
    let typed = format!(
        "\"$CODESENSE_ACTIVITY_HOST/activity-helper\" --activity-run claude -- /usr/bin/python3 {} {} --settings {}\n",
        quote(&fake),
        quote(root),
        quote(&source)
    );
    std::fs::write(root.join("typed"), typed).unwrap();
    let command = format!(
        "while [ ! -f {root}/go ]; do sleep 0.01; done; . {root}/typed; sleep 60",
        root = quote(root)
    );
    let session = PtySessionId::new("typed-shell");
    let mut desktop = connect(root);
    assert!(matches!(
        call(
            &mut desktop,
            Request::Spawn {
                intent: SpawnIntent {
                    remote_launch: None,
                    command: Some(command),
                    argv: None,
                    shell: Some("/bin/sh".into()),
                    cwd: root.to_string_lossy().into_owned(),
                    cols: 80,
                    rows: 24,
                    shell_integration: false,
                    starship_preset: None,
                    api_port: None,
                    env_remove: vec![],
                    env: vec![],
                    agent_env: Vec::new(),
                    login_path: None,
                },
                registration: SessionRegistration {
                    session_id: session.as_str().into(),
                    meta: SessionMeta::default(),
                    home_cwd: Some(root.into()),
                    osc_enabled: false,
                    is_pool_member: false,
                    scrollback: 100
                },
            }
        ),
        Reply::Spawned { .. }
    ));
    drop(desktop);
    std::fs::write(root.join("go"), []).unwrap();
    let until = Instant::now() + Duration::from_secs(8);
    while !root.join("ready").exists() {
        assert!(Instant::now() < until, "typed CLI never reported");
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut desktop = connect(root);
    let Reply::ActivitySnapshot { snapshots } = call(&mut desktop, Request::ActivitySnapshot)
    else {
        panic!("snapshot unavailable");
    };
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].session, session.as_str());
    assert!(snapshots[0].observed);
    assert_eq!(snapshots[0].state, State::NeedsPermission);
    assert_eq!(std::fs::read_to_string(&user_marker).unwrap(), "x");
    assert_eq!(std::fs::read_to_string(&source).unwrap(), settings);
    std::fs::write(root.join("release"), []).unwrap();
    let _ = call(&mut desktop, Request::Kill { session });
}

/// A `codex` typed by hand whose `--version` qualifies is told so, and gets the
/// observer's `-c` hooks ahead of its prompt operand. Its engine never comes up (this fake
/// has none), so it runs on its own, as it was typed.
#[test]
fn hand_typed_codex_is_qualified_by_its_version_and_gets_the_observer() {
    let (qualified, argv) = run_hand_typed_codex("codex-cli 0.159.3");
    assert_eq!(qualified, "1");
    let argv: Vec<_> = argv.split_terminator('\n').collect();
    assert_eq!(argv[0], "--no-daemon");
    let operand = argv.iter().position(|a| *a == "--").unwrap();
    assert_eq!(argv[operand..], ["--", "hello"]);
    assert!(argv[..operand]
        .iter()
        .any(|a| a.starts_with("hooks.SessionStart=") && a.contains("--activity-hook codex")));
    // And its inbox checks, as a launched Codex gets them.
    for event in ["UserPromptSubmit", "PostToolUse"] {
        assert!(
            argv[..operand]
                .iter()
                .any(|a| a.starts_with(&format!("hooks.{event}="))
                    && a.contains(&format!("--inbox-wait codex {event}"))),
            "{event}"
        );
    }
}

/// A `codex` typed by hand outside the qualified releases runs exactly as typed and is
/// not told it qualified.
#[test]
#[ignore = "quarantine: https://github.com/codesensehq/codesense/issues/3"]
fn hand_typed_codex_outside_the_qualified_releases_runs_as_typed() {
    let (qualified, argv) = run_hand_typed_codex("codex-cli 0.158.0");
    assert_eq!(qualified, "");
    assert_eq!(
        argv.split_terminator('\n').collect::<Vec<_>>(),
        ["--model", "m", "--", "hello"]
    );
}

/// Types `codex --model m -- hello` into a host shell, with a fake whose `--version`
/// answers `version`: what it was told about qualifying, and its argv, one per line.
fn run_hand_typed_codex(version: &str) -> (String, String) {
    let daemon = Daemon(
        tempfile::Builder::new()
            .prefix("activity-codex")
            .tempdir_in("/tmp")
            .unwrap(),
    );
    assert!(daemon_command()
        .arg("--root")
        .arg(daemon.0.path())
        .env("SHELL", "/bin/sh")
        .env(
            codesense_session_server::exit_with::EXIT_WITH_PID_ENV,
            std::process::id().to_string()
        )
        .status()
        .unwrap()
        .success());
    let root = daemon.0.path();
    let quote = |path: &Path| codesense_pty_core::shell_quote(path.to_str().unwrap());
    let fake = root.join("codex");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then printf '{version}'; exit 0; fi\ncase \" $* \" in *\" app-server \"*) exit 1;; esac\nprintf '%s\\n' \"$@\" > {argv}\nprintf '%s' \"$CODESENSE_ACTIVITY_RELEASE_QUALIFIED\" > {qualified}\ntouch {ready}\n",
            argv = quote(&root.join("argv")),
            qualified = quote(&root.join("qualified")),
            ready = quote(&root.join("ready")),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    warm_cli_version(&fake, version);
    let typed = format!(
        "\"$CODESENSE_ACTIVITY_HOST/activity-helper\" --activity-run codex -- {} --model m -- hello\n",
        quote(&fake)
    );
    std::fs::write(root.join("typed"), typed).unwrap();
    let command = format!(
        "while [ ! -f {root}/go ]; do sleep 0.01; done; . {root}/typed; sleep 60",
        root = quote(root)
    );
    let session = PtySessionId::new("typed-codex");
    let mut desktop = connect(root);
    assert!(matches!(
        call(
            &mut desktop,
            Request::Spawn {
                intent: SpawnIntent {
                    remote_launch: None,
                    command: Some(command),
                    argv: None,
                    shell: Some("/bin/sh".into()),
                    cwd: root.to_string_lossy().into_owned(),
                    cols: 80,
                    rows: 24,
                    shell_integration: false,
                    starship_preset: None,
                    api_port: None,
                    env_remove: vec![],
                    env: vec![],
                    agent_env: Vec::new(),
                    login_path: None,
                },
                registration: SessionRegistration {
                    session_id: session.as_str().into(),
                    meta: SessionMeta::default(),
                    home_cwd: Some(root.into()),
                    osc_enabled: false,
                    is_pool_member: false,
                    scrollback: 100
                },
            }
        ),
        Reply::Spawned { .. }
    ));
    std::fs::write(root.join("go"), []).unwrap();
    let until = Instant::now() + Duration::from_secs(8);
    while !root.join("ready").exists() {
        assert!(Instant::now() < until, "typed codex never ran");
        std::thread::sleep(Duration::from_millis(10));
    }
    let told = std::fs::read_to_string(root.join("qualified")).unwrap();
    let argv = std::fs::read_to_string(root.join("argv")).unwrap();
    let _ = call(&mut desktop, Request::Kill { session });
    (told, argv)
}

/// Explicit opt-in: starts the installed Claude TUI in this trusted worktree,
/// observes startup hooks without sending a prompt, then closes its owned PTY.
#[test]
#[ignore = "requires an installed authenticated Claude runtime; no model prompt is sent"]
fn installed_claude_startup_preserves_user_hook_with_desktop_absent() {
    installed_claude_fixture(false);
}

#[test]
#[ignore = "requires installed authenticated Claude; sends one small tool-free model prompt"]
fn installed_claude_submit_and_stop_reach_owner_with_desktop_absent() {
    installed_claude_fixture(true);
}

fn installed_claude_fixture(turn: bool) {
    let provider = std::env::var("CODESENSE_ACTIVITY_CLAUDE").expect("explicit Claude binary");
    let cwd = std::env::var("CODESENSE_ACTIVITY_CWD").expect("explicit trusted working directory");
    let daemon = Daemon(
        tempfile::Builder::new()
            .prefix("activity-native")
            .tempdir_in("/tmp")
            .unwrap(),
    );
    let root = daemon.0.path();
    assert!(daemon_command()
        .arg("--root")
        .arg(root)
        .env("SHELL", "/bin/sh")
        .env(
            codesense_session_server::exit_with::EXIT_WITH_PID_ENV,
            std::process::id().to_string()
        )
        .status()
        .unwrap()
        .success());
    let quote = |path: &Path| codesense_pty_core::shell_quote(path.to_str().unwrap());
    let source = root.join("user-settings.json");
    let marker = root.join("user-hook");
    // No server is authorized by this hook-only fixture. Avoid a native MCP
    // approval prompt without changing saved trust or accepting any server.
    let mcp = root.join("empty-mcp.json");
    std::fs::write(&mcp, r#"{"mcpServers":{}}"#).unwrap();
    let mut settings = serde_json::json!({"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":format!("printf x >> {}", quote(&marker))}]}]}});
    if turn {
        for (event, started, release) in [
            ("UserPromptSubmit", "submitted", "continue"),
            ("Stop", "stopped", "finish"),
        ] {
            settings["hooks"][event] = serde_json::json!([{"hooks":[{"type":"command", "command": format!("touch {}; while [ ! -f {} ]; do sleep 0.01; done", quote(&root.join(started)), quote(&root.join(release)))}]}]);
        }
    }
    let settings = settings.to_string();
    std::fs::write(&source, &settings).unwrap();
    let session = PtySessionId::new("native-claude");
    let mut desktop = connect(root);
    let command_file = root.join("command");
    let command = format!(
        "while [ ! -f {file} ]; do sleep 0.01; done; . {file}; sleep 60",
        file = quote(&command_file)
    );
    assert!(matches!(
        call(
            &mut desktop,
            Request::Spawn {
                intent: SpawnIntent {
                    remote_launch: None,
                    command: Some(command),
                    argv: None,
                    shell: Some("/bin/sh".into()),
                    cwd,
                    cols: 100,
                    rows: 30,
                    shell_integration: false,
                    starship_preset: None,
                    api_port: None,
                    env_remove: vec![],
                    env: vec![],
                    agent_env: Vec::new(),
                    login_path: None,
                },
                registration: SessionRegistration {
                    session_id: session.as_str().into(),
                    meta: SessionMeta::default(),
                    home_cwd: None,
                    osc_enabled: false,
                    is_pool_member: false,
                    scrollback: 100
                },
            }
        ),
        Reply::Spawned { .. }
    ));
    let Reply::ShellActivity { launch } = call(
        &mut desktop,
        Request::PrepareShellActivity {
            session: session.clone(),
            provider: AgentCli::Claude,
        },
    ) else {
        panic!("shell launch unavailable")
    };
    drop(desktop);
    std::fs::write(
        &command_file,
        format!(
            "{} --activity-exec {} -- {} --settings {} --strict-mcp-config --mcp-config {}{}\n",
            quote(Path::new(&launch.helper)),
            quote(Path::new(&launch.bootstrap)),
            quote(Path::new(&provider)),
            quote(&source),
            quote(&mcp),
            if turn {
                " --print 'Reply with OK only. Do not use tools.'"
            } else {
                ""
            }
        ),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(25);
    while !marker.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    if turn && marker.exists() {
        let wait_for = |name: &str| {
            let deadline = Instant::now() + Duration::from_secs(60);
            while !root.join(name).exists() {
                assert!(
                    Instant::now() < deadline,
                    "native callback did not arrive: {name}"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        let observed = |expected| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let mut viewer = connect(root);
                let Reply::ActivitySnapshot { snapshots } =
                    call(&mut viewer, Request::ActivitySnapshot)
                else {
                    panic!("snapshot unavailable")
                };
                if snapshots.iter().any(|snapshot| {
                    snapshot.identity == launch.identity && snapshot.state == expected
                }) {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "native callback not reflected by owner: {expected:?}"
                );
                std::thread::sleep(Duration::from_millis(25));
            }
        };
        wait_for("submitted");
        observed(State::Thinking);
        std::fs::write(root.join("continue"), []).unwrap();
        wait_for("stopped");
        observed(State::Done);
    }
    let mut desktop = connect(root);
    // Claude runs a hook event's commands in parallel: the person's marker can
    // land before the observer's report does.
    let deadline = Instant::now() + Duration::from_secs(5);
    let snapshots = loop {
        let Reply::ActivitySnapshot { snapshots } = call(&mut desktop, Request::ActivitySnapshot)
        else {
            panic!("snapshot unavailable")
        };
        if snapshots.iter().any(|snapshot| snapshot.observed) || Instant::now() >= deadline {
            break snapshots;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let _ = call(&mut desktop, Request::Kill { session });
    assert!(
        marker.exists(),
        "native startup hook did not run; trust/auth/startup may require interaction"
    );
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "x");
    assert_eq!(std::fs::read_to_string(source).unwrap(), settings);
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].identity, launch.identity);
    assert!(
        snapshots[0].observed,
        "user hook ran but owner did not receive native observer"
    );
}

/// A hook that never closes its input, or hands over what is no report, never holds its CLI:
/// the helper gives up within its own budget and exits neutrally. Every CLI the host knows,
/// including those not yet listed.
#[test]
fn a_slow_or_failing_activity_hook_exits_neutrally_within_its_budget_for_every_cli() {
    use std::process::Stdio;
    for cli in AgentCli::every_cli() {
        for (closes_input, input) in [(false, ""), (true, "not a report")] {
            let mut child = Command::new(BINARY)
                .args(["--activity-hook", cli.as_str()])
                // A binding that names nothing: the helper gets as far as parsing its input.
                .env(SOCKET_ENV, "/nonexistent/activity.sock")
                .env(TOKEN_ENV, "unbound")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let mut stdin = child.stdin.take().unwrap();
            std::io::copy(&mut input.as_bytes(), &mut stdin).unwrap();
            // Held open until the helper has exited, or closed after what it was handed.
            let held = (!closes_input).then_some(stdin);
            // The test owns the bound: a helper that waited on the open input fails here.
            let (done, exited) = std::sync::mpsc::channel();
            std::thread::spawn(move || done.send(child.wait()).unwrap());
            let status = exited
                .recv_timeout(Duration::from_secs(10))
                .unwrap_or_else(|_| panic!("{cli}: the hook helper must not hang"))
                .unwrap();
            assert!(status.success(), "{cli}: exits neutrally");
            drop(held);
        }
    }
}

/// Hermes's hooks, as the gateway runs them: a main turn's `pre_llm_call` and a tool's
/// `post_tool_call`.
const HERMES_PRE_LLM_CALL: &str = r#"{"hook_event_name":"pre_llm_call","tool_name":null,"tool_input":null,"session_id":"20261027_204421_0dfc4e","cwd":"/work","profile":"default","extra":{"turn_id":"t1","platform":"cli","conversation_history":[{"role":"user","content":"hi","platform_message_id":"m1"}],"telemetry_schema_version":"hermes.observer.v1"}}"#;
const HERMES_POST_TOOL_CALL: &str = r#"{"hook_event_name":"post_tool_call","tool_name":"read_file","tool_input":{},"session_id":"20261027_204421_0dfc4e","cwd":"/work","profile":"default","extra":{"turn_id":"t1","platform":"cli","telemetry_schema_version":"hermes.observer.v1"}}"#;

/// A packaged daemon with no desktop, its desktop connection, and a Hermes invocation for
/// `session` whose PTY runs `script` (with `$HOOK_ENV` set to its activity binding).
fn hermes_session(
    prefix: &str,
    session: &PtySessionId,
    script: impl FnOnce(&Path) -> String,
) -> (Daemon, UnixStream) {
    let daemon = Daemon(
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("/tmp")
            .unwrap(),
    );
    assert!(daemon_command()
        .args(["--root"])
        .arg(daemon.0.path())
        .env("SHELL", "/bin/sh")
        .env(
            codesense_session_server::exit_with::EXIT_WITH_PID_ENV,
            std::process::id().to_string()
        )
        .status()
        .unwrap()
        .success());
    let root = daemon.0.path().to_path_buf();
    let mut desktop = connect(&root);
    assert_eq!(call(&mut desktop, Request::Listen), Reply::Done);
    assert_eq!(
        call(
            &mut desktop,
            Request::LocalMcpSessions {
                sessions: vec![session.clone()]
            },
        ),
        Reply::Done
    );
    let Reply::ActivityWriter { writer } = call(
        &mut desktop,
        Request::PrepareActivity {
            session: session.clone(),
            provider: AgentCli::Hermes,
        },
    ) else {
        panic!("writer")
    };
    let intent = SpawnIntent {
        remote_launch: None,
        command: Some(script(&root)),
        argv: None,
        shell: Some("/bin/sh".into()),
        cwd: root.to_string_lossy().into_owned(),
        cols: 80,
        rows: 24,
        shell_integration: false,
        starship_preset: None,
        api_port: None,
        env_remove: vec![],
        env: vec![
            (SOCKET_ENV.into(), writer.socket.clone()),
            (TOKEN_ENV.into(), writer.token.clone()),
            (
                codesense_pty_core::activity::RELEASE_QUALIFIED_ENV.into(),
                "1".into(),
            ),
        ],
        agent_env: Vec::new(),
        login_path: None,
    };
    let registration = SessionRegistration {
        session_id: session.as_str().into(),
        meta: SessionMeta::default(),
        home_cwd: Some(root.clone()),
        osc_enabled: false,
        is_pool_member: false,
        scrollback: 100,
    };
    assert!(matches!(
        call(
            &mut desktop,
            Request::Spawn {
                intent,
                registration
            }
        ),
        Reply::Spawned { .. }
    ));
    (daemon, desktop)
}

fn wait_for(path: &Path, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A Hermes hook's report reaches its host through the packaged activity helper: the host
/// prepares a Hermes invocation, the helper parses `--activity-hook hermes` and normalizes
/// the hook, and the snapshot the host sends back carries it.
#[test]
fn a_hermes_hook_report_reaches_its_host_through_the_packaged_helper() {
    let session = PtySessionId::new("hermes-hook-session");
    let (daemon, mut desktop) = hermes_session("hermes-hook", &session, |root| {
        format!(
            "printf %s {hook} | {helper} --activity-hook hermes; touch {finished}; sleep 60",
            hook = codesense_pty_core::shell_quote(HERMES_PRE_LLM_CALL),
            helper = codesense_pty_core::shell_quote(BINARY),
            finished = codesense_pty_core::shell_quote(root.join("reported").to_str().unwrap()),
        )
    });
    wait_for(&daemon.0.path().join("reported"), "helper did not finish");
    let Reply::ActivitySnapshot { snapshots } = call(&mut desktop, Request::ActivitySnapshot)
    else {
        panic!("snapshot")
    };
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].state, State::Thinking, "{:?}", snapshots[0]);
    let _ = call(&mut desktop, Request::Kill { session });
}

/// Mail for a working Hermes reaches it through its own checks at its host: the packaged
/// helper parses `--inbox-wait hermes`, a `post_tool_call` check defers the notice (it hands
/// nothing over and the notice stays untold), and the next `pre_llm_call` check hands it over
/// as next-turn context, which the host reports told.
#[test]
fn a_working_hermes_is_handed_its_mail_by_its_checks_at_its_host() {
    use codesense_pty_core::domain::TerminalDomainEvent;
    use codesense_pty_core::protocol::{Inbound, Ingress};
    let session = PtySessionId::new("hermes-check-session");
    let (daemon, mut desktop) = hermes_session("hermes-check", &session, |root| {
        let at = |name: &str| codesense_pty_core::shell_quote(root.join(name).to_str().unwrap());
        let check = |event: &str, input: &str, name: &str| {
            format!(
                "printf %s {input} | {helper} --inbox-wait hermes {event} $$ >{out} 2>{out}.err; echo $? >{out}.status",
                input = codesense_pty_core::shell_quote(input),
                helper = codesense_pty_core::shell_quote(BINARY),
                out = at(name),
            )
        };
        // Its turn starts first, as Hermes reports it: mail then comes to a working agent.
        format!(
            "printf %s {turn} | {helper} --activity-hook hermes; \
             while [ ! -f {trigger} ]; do sleep 0.01; done; {tool}; {llm}; touch {done}; sleep 60",
            turn = codesense_pty_core::shell_quote(HERMES_PRE_LLM_CALL),
            helper = codesense_pty_core::shell_quote(BINARY),
            trigger = at("trigger"),
            tool = check("post_tool_call", HERMES_POST_TOOL_CALL, "tool"),
            llm = check("pre_llm_call", HERMES_PRE_LLM_CALL, "llm"),
            done = at("done"),
        )
    });
    let root = daemon.0.path();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let Reply::ActivitySnapshot { snapshots } = call(&mut desktop, Request::ActivitySnapshot)
        else {
            panic!("snapshot")
        };
        if snapshots
            .first()
            .is_some_and(|s| s.state == State::Thinking)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "its turn never started: {snapshots:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let text = "You have 1 new message. Call read_messages with session_id hermes-check-session.";
    assert_eq!(
        call(
            &mut desktop,
            Request::InboxMail {
                session: session.clone(),
                notice: text.into(),
                token: "tok-h".into(),
            }
        ),
        Reply::Done
    );
    std::fs::write(root.join("trigger"), []).unwrap();
    wait_for(&root.join("done"), "the checks never ran");
    let read = |name: &str| std::fs::read_to_string(root.join(name)).unwrap();
    assert_eq!(read("tool.status").trim(), "0");
    assert_eq!(
        read("tool"),
        "",
        "a tool step defers the notice to the next turn"
    );
    assert_eq!(read("llm.status").trim(), "0", "{}", read("llm.err"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(read("llm").trim()).unwrap_or_default(),
        serde_json::json!({"context": text}),
        "the next turn is handed the notice as its context: {}",
        read("llm.err")
    );
    // The desktop hears it told once Hermes had it.
    let mut ingress = Ingress::default();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "no told report");
        let frame = codesense_host_proto::read_frame(&mut desktop)
            .unwrap()
            .unwrap();
        if let Ok(Some(Inbound::Event(TerminalDomainEvent::InboxAck {
            session: told,
            token,
        }))) = ingress.accept(&frame)
        {
            assert_eq!((told, token.as_str()), (session.clone(), "tok-h"));
            break;
        }
    }
    let _ = call(&mut desktop, Request::Kill { session });
}
