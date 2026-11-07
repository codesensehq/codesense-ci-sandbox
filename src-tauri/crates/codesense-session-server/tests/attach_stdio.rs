// SPDX-License-Identifier: MIT
//! `attach --stdio` against the real built binary on this machine: the relay
//! launches the host when none runs, pipes the host's own frames through
//! unchanged, defaults the instance root to `$HOME/.codesense`, and reaches a
//! host that is already running rather than starting another.

mod common;

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Command;

use codesense_host_proto::read_frame;
use codesense_pty_core::domain::ViewerId;
use codesense_pty_core::protocol::{client_hello, host_pid, instance_id, socket_path};
use codesense_session_server::exit_with::EXIT_WITH_PID_ENV;

use common::{greeting_bytes, read_greeting_golden, send, Relay};

const BINARY: &str = env!("CARGO_BIN_EXE_codesense-session-server");

/// Kills the host serving each root, however the test ends.
struct Hosts(Vec<std::path::PathBuf>);

impl Drop for Hosts {
    fn drop(&mut self) {
        for root in &self.0 {
            if let Some(pid) = host_pid(root) {
                unsafe { libc::kill(pid as i32, libc::SIGKILL) };
            }
        }
    }
}

fn short_dir(prefix: &str) -> tempfile::TempDir {
    // macOS caps socket paths at 104 bytes; keep the fixture path short.
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("/tmp")
        .unwrap()
}

/// The relay, with `home` as its `$HOME`. The host it launches inherits its
/// environment, so it is tied to this test process's lifetime too.
fn relay(home: &Path, extra: &[&str]) -> Relay {
    let mut command = Command::new(BINARY);
    command
        .arg("attach")
        .arg("--stdio")
        .args(extra)
        .env("HOME", home)
        .env("SHELL", "/bin/sh")
        .env(EXIT_WITH_PID_ENV, std::process::id().to_string());
    Relay::spawn(command)
}

/// Greet the host through `relay` as a client of the instance it named.
fn greet(relay: &mut Relay, instance: &str) {
    let hello = client_hello(instance.to_string(), "attach", &ViewerId::new("attach"));
    send(relay, &hello.frame().unwrap());
}

/// The real host, not merely the client adapter, refuses a client that does
/// not echo the canonical instance from its greeting.
#[test]
fn attach_rejects_a_wrong_instance_echo() {
    let home = short_dir("rm4id");
    let root = home.path().join(".codesense");
    let _hosts = Hosts(vec![root]);
    let mut relay = relay(home.path(), &[]);
    let (hello, _) = read_greeting_golden(&mut relay.output);
    let wrong = client_hello(
        format!("{}-wrong", hello.instance_id),
        "attach",
        &ViewerId::new("wrong-instance"),
    );
    send(&mut relay, &wrong.frame().unwrap());
    assert_eq!(read_frame(&mut relay.output).unwrap(), None);
    assert!(relay.wait().success(), "rejected relay exits normally");
}

/// With no `--root` the relay starts the host under `$HOME/.codesense`, whose
/// greeting crosses byte for byte; when the client leaves, the relay ends and
/// the host keeps running. A second relay reaches that same host.
#[test]
fn attach_starts_the_host_under_the_codesense_home_and_reuses_it() {
    let home = short_dir("rm2h");
    let root = home.path().join(".codesense");
    let _hosts = Hosts(vec![root.clone()]);

    let mut first = relay(home.path(), &[]);
    let (hello, live) = read_greeting_golden(&mut first.output);
    assert_eq!(hello.instance_id, instance_id(&root), "the default root");
    assert_eq!(live, 0);
    assert!(socket_path(&root).exists());
    greet(&mut first, &hello.instance_id);
    first.close_input();
    assert!(first.wait().success(), "the relay ends with its client");

    let host = hello.pid as i32;
    assert!(
        unsafe { libc::kill(host, 0) } == 0,
        "the host outlives the relay"
    );

    let mut second = relay(home.path(), &[]);
    let (again, _) = read_greeting_golden(&mut second.output);
    assert_eq!(again.pid, hello.pid, "the running host, not a new one");
    assert_eq!(
        (again.server_id.as_str(), again.epoch.as_str()),
        (hello.server_id.as_str(), hello.epoch.as_str())
    );

    // Byte for byte what the host says on its own socket.
    let mut third = relay(home.path(), &[]);
    let relayed = greeting_bytes(&mut third.output);
    let mut direct = UnixStream::connect(socket_path(&root)).unwrap();
    direct.set_read_timeout(Some(common::WAIT)).unwrap();
    assert_eq!(relayed, greeting_bytes(&mut direct));
}

/// An explicit `--root` wins over the default: the host serves that instance
/// and nothing is created under `$HOME`.
#[test]
fn an_explicit_root_overrides_the_default() {
    let home = short_dir("rm2h");
    let root = short_dir("rm2r");
    let _hosts = Hosts(vec![root.path().to_path_buf()]);

    let root_arg = root.path().to_string_lossy().into_owned();
    let mut relay = relay(home.path(), &["--root", &root_arg]);
    let ours = client_hello(instance_id(root.path()), "attach", &ViewerId::new("attach"));
    let (output, input) = relay.io();
    let hello = codesense_host_proto::greet(output, input, &ours)
        .expect("the client handshake, through the relay");
    assert_eq!(hello.instance_id, instance_id(root.path()));
    assert!(!home.path().join(".codesense").exists());
}

/// A misused relay says so on stderr and exits non-zero, writing nothing to
/// stdout (which is the protocol stream).
#[test]
fn a_bad_invocation_fails_without_writing_to_stdout() {
    let out = Command::new(BINARY)
        .args(["attach", "--root", "/tmp/x"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("attach --stdio"));
}

/// Change only the exec'd fixture's limit, leaving the parallel test harness alone.
fn incoming_limit(command: &mut Command, soft: libc::rlim_t) {
    use std::os::unix::process::CommandExt;
    // SAFETY: the child callback only calls rlimit syscalls on stack storage.
    unsafe {
        command.pre_exec(move || {
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            limit.rlim_cur = soft;
            if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

fn assert_pty_limit(relay: &mut Relay, root: &Path, mut argv: Vec<String>, soft: u64) {
    use codesense_pty_core::protocol::{call_once, Reply, Request};
    use codesense_pty_core::spawn::SpawnIntent;
    use codesense_pty_core::{SessionMeta, SessionRegistration};
    use std::time::{Duration, Instant};

    let out = root.join("pty-limit");
    argv.extend([
        "/bin/sh".into(),
        "-c".into(),
        "ulimit -Sn > \"$1.next\" && mv \"$1.next\" \"$1\"".into(),
        "limit".into(),
        out.to_str().unwrap().into(),
    ]);
    let (reader, writer) = relay.io();
    let reply = call_once(
        reader,
        writer,
        1,
        Request::Spawn {
            intent: SpawnIntent {
                remote_launch: None,
                command: None,
                argv: Some(argv),
                shell: None,
                cwd: root.to_str().unwrap().into(),
                cols: 80,
                rows: 24,
                shell_integration: false,
                starship_preset: None,
                api_port: None,
                env_remove: vec![],
                env: vec![],
                agent_env: vec![],
                login_path: None,
            },
            registration: SessionRegistration {
                session_id: "nofile".into(),
                meta: SessionMeta::default(),
                home_cwd: None,
                osc_enabled: false,
                is_pool_member: false,
                scrollback: codesense_pty_core::emulator::DEFAULT_SCROLLBACK,
            },
        },
    )
    .unwrap()
    .unwrap();
    assert!(matches!(reply, Reply::Spawned { .. }), "{reply:?}");
    let deadline = Instant::now() + common::WAIT;
    while !out.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        std::fs::read_to_string(out).unwrap().trim(),
        soft.to_string(),
        "the user command must keep the incoming limit"
    );
}

#[test]
fn cold_attach_preserves_the_incoming_pty_limit() {
    for soft in [256, 1024] {
        let home = short_dir("nofile");
        let root = home.path().join(".codesense");
        let _hosts = Hosts(vec![root.clone()]);
        let mut command = Command::new(BINARY);
        command
            .args(["attach", "--stdio"])
            .env("HOME", home.path())
            .env("SHELL", "/bin/sh")
            .env(EXIT_WITH_PID_ENV, std::process::id().to_string());
        incoming_limit(&mut command, soft);
        let mut relay = Relay::spawn(command);
        let (hello, _) = read_greeting_golden(&mut relay.output);
        greet(&mut relay, &hello.instance_id);
        assert_pty_limit(&mut relay, &root, vec![], soft as u64);
        let log = std::fs::read_to_string(root.join("logs/pty-server.log")).unwrap();
        assert!(log.contains(&format!("soft {soft} -> 10240")), "{log}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn bootstrap_wrapped_pty_preserves_the_incoming_limit() {
    use codesense_host_proto::{broker, reverse_mcp as wire};
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixListener;

    for soft in [256, 1024] {
        let home = short_dir("bootlimit");
        let root = home.path().join(".codesense");
        let _hosts = Hosts(vec![root.clone()]);
        let mut host = Command::new(BINARY);
        host.arg("--root")
            .arg(&root)
            .env("HOME", home.path())
            .env("SHELL", "/bin/sh")
            .env(EXIT_WITH_PID_ENV, std::process::id().to_string());
        incoming_limit(&mut host, soft);
        assert!(host.status().unwrap().success());
        let mut relay = relay(home.path(), &[]);
        let (hello, _) = read_greeting_golden(&mut relay.output);
        greet(&mut relay, &hello.instance_id);

        let socket = home.path().join("broker.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let authority = std::thread::spawn(move || {
            let (mut client, _) = listener.accept().unwrap();
            client.set_read_timeout(Some(common::WAIT)).unwrap();
            let mut mode = [0];
            client.read_exact(&mut mode).unwrap();
            assert_eq!(mode, [wire::MODE_BOOTSTRAP]);
            let (capability, _peer) = UnixStream::pair().unwrap();
            broker::send_fd(&client, capability.as_raw_fd()).unwrap();
        });
        assert_pty_limit(
            &mut relay,
            &root,
            vec![
                BINARY.into(),
                "--mcp-session-bootstrap".into(),
                socket.to_str().unwrap().into(),
                "fixture-config".into(),
            ],
            soft as u64,
        );
        authority.join().unwrap();
    }
}
