// SPDX-License-Identifier: MIT
//! Process census against the real built sidecar binary: the PTY child's parent
//! is the sidecar, the sidecar runs detached in its own session (not as its
//! leader) and is parented by launchd, the child keeps running after the
//! client connection drops, and instances with different app-data roots each
//! get their own sidecar.

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use codesense_host_proto::{greet, read_frame, Hello};
use codesense_pty_core::domain::ViewerId;
use codesense_pty_core::protocol::{
    call_once, client_hello, instance_id, socket_path, Reply, Request,
};
use codesense_pty_core::spawn::SpawnIntent;
use codesense_pty_core::{SessionMeta, SessionRegistration};

const SIDECAR: &str = env!("CARGO_BIN_EXE_codesense-session-server");

/// The sidecar exits once this process is gone, so a sidecar launched by a test
/// run that dies before its guards drop (a panic in setup, a timeout, a
/// SIGKILL) never outlives the run.
use codesense_session_server::exit_with::EXIT_WITH_PID_ENV;

/// Reaps everything a test launched, however it ends. Created BEFORE the
/// launch: it kills the recorded pids and, for each instance root, any sidecar
/// still serving it, so a failure before the sidecar's pid is known leaks
/// nothing either.
#[derive(Default)]
struct Cleanup {
    pids: Vec<i32>,
    roots: Vec<std::path::PathBuf>,
}

impl Cleanup {
    fn rooted_at(roots: &[&Path]) -> Self {
        Self {
            pids: Vec::new(),
            roots: roots.iter().map(|root| root.to_path_buf()).collect(),
        }
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        for pid in &self.pids {
            unsafe { libc::kill(*pid, libc::SIGKILL) };
        }
        for root in &self.roots {
            for pid in sidecars_rooted_at(root) {
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }
}

/// The pids of this test binary's sidecar processes serving `root`.
fn sidecars_rooted_at(root: &Path) -> Vec<i32> {
    let wanted = format!("{SIDECAR} --root {}", root.display());
    // Runs from `Cleanup::drop`, possibly mid-unwind: never panic here.
    let Ok(out) = Command::new("ps")
        .args(["-Aww", "-o", "pid=,args="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .split('\n')
        .filter_map(|line| {
            let (pid, args) = line.trim().split_once(' ')?;
            (args.trim() == wanted).then(|| pid.parse().ok())?
        })
        .collect()
}

/// A sidecar command for `root`, tied to this test process's lifetime.
fn sidecar_command(root: &Path) -> Command {
    let mut command = Command::new(SIDECAR);
    command
        .arg("--root")
        .arg(root)
        .env(EXIT_WITH_PID_ENV, std::process::id().to_string())
        .env("SHELL", "/bin/bash")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command
}

/// `ps -o <field>= -p <pid>` as a trimmed string (`args` rather than the
/// truncated `comm` when matching a command name).
fn ps_field(pid: i32, field: &str) -> String {
    let out = Command::new("ps")
        .args(["-o", &format!("{field}="), "-p", &pid.to_string()])
        .output()
        .expect("run ps");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn ps_pid(pid: i32, field: &str) -> i32 {
    ps_field(pid, field)
        .parse()
        .unwrap_or_else(|_| panic!("ps {field} for {pid}"))
}

fn short_root(prefix: &str) -> tempfile::TempDir {
    // macOS caps socket paths at 104 bytes; keep the fixture path short.
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("/tmp")
        .unwrap()
}

/// This test's hello as a client of the instance rooted at `root`.
fn hello_for(root: &Path) -> Hello {
    client_hello(instance_id(root), "census", &ViewerId::new("census"))
}

/// Connect to the socket of `root` once it listens and greet the sidecar as a
/// client of `instance`; returns the connection and the sidecar's hello.
fn greet_as(root: &Path, instance: &Path) -> (UnixStream, Hello) {
    let deadline = Instant::now() + Duration::from_secs(20);
    let stream = loop {
        if let Ok(stream) = UnixStream::connect(socket_path(root)) {
            break stream;
        }
        assert!(
            Instant::now() < deadline,
            "sidecar never listened under {root:?}"
        );
        thread::sleep(Duration::from_millis(50));
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let hello = greet(&mut &stream, &mut &stream, &hello_for(instance)).expect("handshake");
    (stream, hello)
}

/// Connect to the sidecar of `root` and complete the handshake; returns the
/// connection and the sidecar's pid.
fn connect(root: &Path) -> (UnixStream, i32) {
    let (stream, hello) = greet_as(root, root);
    assert_eq!(
        hello.instance_id,
        instance_id(root),
        "the sidecar serves this instance"
    );
    (stream, hello.pid as i32)
}

/// The sidecar runs detached from whoever launched it: in its own session and
/// process group, neither of them this test's, and not a session leader (the
/// double fork), so it can never acquire a controlling terminal.
fn assert_detached(sidecar: i32) {
    let session = unsafe { libc::getsid(sidecar) };
    assert!(session > 0, "sidecar {sidecar} has a session");
    assert_ne!(
        session,
        unsafe { libc::getsid(0) },
        "not the launcher's session"
    );
    assert_ne!(session, sidecar, "not a session leader");
    assert_ne!(
        unsafe { libc::getpgid(sidecar) },
        unsafe { libc::getpgrp() },
        "not the launcher's process group"
    );
}

fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Send one call and wait for its reply (the connection takes no events).
fn call(stream: &mut UnixStream, rid: u64, request: Request) -> Result<Reply, String> {
    call_once(&mut &*stream, &mut &*stream, rid, request).expect("call")
}

fn spawn_sleeper(stream: &mut UnixStream, id: &str) -> i32 {
    let request = Request::Spawn {
        // The sidecar's own shell runs it; `exec` keeps the child's pid.
        intent: SpawnIntent {
            remote_launch: Default::default(),
            command: Some("exec /bin/sleep 60".into()),
            argv: None,
            shell: None,
            cwd: String::new(),
            cols: 80,
            rows: 24,
            shell_integration: false,
            starship_preset: None,
            api_port: None,
            env_remove: Vec::new(),
            env: vec![("HOME".into(), "/var/empty".into())],
            agent_env: Vec::new(),
            login_path: None,
        },
        registration: SessionRegistration {
            session_id: id.into(),
            meta: SessionMeta {
                story_id: Some("census".into()),
                ..SessionMeta::default()
            },
            home_cwd: None,
            osc_enabled: false,
            is_pool_member: false,
            scrollback: codesense_pty_core::emulator::DEFAULT_SCROLLBACK,
        },
    };
    match call(stream, 1, request) {
        Ok(Reply::Spawned { pid: Some(pid) }) => pid as i32,
        other => panic!("spawn failed: {other:?}"),
    }
}

/// Launch the sidecar for `root` through a short-lived shell, the way a
/// quitting app leaves it behind: the shell exits right after backgrounding it.
fn launch_via_exiting_shell(root: &Path) {
    launch_via_exiting_shell_tied_to(root, std::process::id() as i32);
}

/// [`launch_via_exiting_shell`], with the sidecar tied to `holder`'s lifetime.
fn launch_via_exiting_shell_tied_to(root: &Path, holder: i32) {
    let launcher = Command::new("sh")
        .arg("-c")
        .arg("\"$0\" --root \"$1\" </dev/null >/dev/null 2>&1 &")
        .arg(SIDECAR)
        .arg(root)
        .env(EXIT_WITH_PID_ENV, holder.to_string())
        .env("SHELL", "/bin/bash")
        .status()
        .expect("launch sidecar");
    assert!(launcher.success());
}

#[test]
fn child_is_hosted_by_a_detached_sidecar_and_outlives_the_client() {
    let root = short_root("rt6c");
    let mut cleanup = Cleanup::rooted_at(&[root.path()]);
    launch_via_exiting_shell(root.path());

    let (mut client, sidecar) = connect(root.path());
    cleanup.pids.push(sidecar);
    let child = spawn_sleeper(&mut client, "census-1");
    cleanup.pids.push(child);

    assert_eq!(
        ps_pid(child, "ppid"),
        sidecar,
        "the child's parent is the sidecar"
    );
    assert!(
        ps_field(sidecar, "args").contains("codesense-session-server"),
        "the child's parent is the sidecar, got {:?}",
        ps_field(sidecar, "args")
    );
    assert_detached(sidecar);
    let sidecar_parent = ps_pid(sidecar, "ppid");
    assert_ne!(
        sidecar_parent,
        std::process::id() as i32,
        "not parented to the launcher"
    );
    #[cfg(target_os = "macos")]
    assert_eq!(sidecar_parent, 1, "reparented to launchd");

    // The app quits: its connection goes away. The hosted child keeps running.
    drop(client);
    thread::sleep(Duration::from_millis(500));
    assert!(alive(child), "child survives the client disconnect");
    assert!(alive(sidecar), "sidecar survives the client disconnect");

    // Rediscovery: a relaunched app reaches the same sidecar over the same
    // socket, and it identifies as this instance.
    let (mut again, answered_by) = connect(root.path());
    assert_eq!(answered_by, sidecar, "the same sidecar answers");
    call(
        &mut again,
        2,
        Request::Kill {
            session: "census-1".into(),
        },
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while alive(child) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !alive(child),
        "Kill over a fresh connection reaps the child"
    );
    cleanup.pids.retain(|pid| *pid != child);
}

/// A sidecar started as its own process-group leader (as from an interactive
/// shell with job control) still detaches: the launched process exits and the
/// server runs on in a fresh session, parented by launchd.
#[test]
fn a_sidecar_started_as_a_group_leader_forks_into_its_own_session() {
    use std::os::unix::process::CommandExt;

    let root = short_root("rt6f");
    let mut cleanup = Cleanup::rooted_at(&[root.path()]);
    let mut launched = sidecar_command(root.path())
        .process_group(0)
        .spawn()
        .expect("launch sidecar as a group leader");
    let launched_pid = launched.id() as i32;
    assert!(
        launched.wait().expect("launched process exits").success(),
        "the launched process exits once it has forked"
    );

    let (mut client, sidecar) = connect(root.path());
    cleanup.pids.push(sidecar);
    let child = spawn_sleeper(&mut client, "fork-1");
    cleanup.pids.push(child);
    assert_eq!(ps_pid(child, "ppid"), sidecar);
    assert_ne!(sidecar, launched_pid, "the server is the forked process");
    assert_detached(sidecar);
    #[cfg(target_os = "macos")]
    assert_eq!(ps_pid(sidecar, "ppid"), 1, "reparented to launchd");
}

/// Two instances with different app-data roots never share a sidecar: each
/// root gets its own socket, its own sidecar process and its own identity, and
/// a second launch for a root that already has one exits instead of competing.
#[test]
fn instances_with_different_roots_never_share_a_sidecar() {
    let a = short_root("rt6ia");
    let b = short_root("rt6ib");
    let mut cleanup = Cleanup::rooted_at(&[a.path(), b.path()]);
    launch_via_exiting_shell(a.path());
    launch_via_exiting_shell(b.path());

    let (mut client_a, sidecar_a) = connect(a.path());
    cleanup.pids.push(sidecar_a);
    let (mut client_b, sidecar_b) = connect(b.path());
    cleanup.pids.push(sidecar_b);
    assert_ne!(sidecar_a, sidecar_b, "one sidecar per instance root");
    let child_a = spawn_sleeper(&mut client_a, "same-id");
    cleanup.pids.push(child_a);
    let child_b = spawn_sleeper(&mut client_b, "same-id");
    cleanup.pids.push(child_b);
    assert_eq!(ps_pid(child_a, "ppid"), sidecar_a);
    assert_eq!(ps_pid(child_b, "ppid"), sidecar_b);
    assert!(
        alive(child_a) && alive(child_b),
        "same id, separate sessions"
    );

    // Instance A's client reaching B's socket learns it is B's and is turned
    // away before it can drive B's runtime.
    let (crossed, answer) = greet_as(b.path(), a.path());
    assert_eq!(answer.instance_id, instance_id(b.path()));
    assert_eq!(
        read_frame(&mut &crossed),
        Ok(None),
        "the foreign client is disconnected"
    );

    // A duplicate launch for root A finds the lock held and exits.
    let duplicate = sidecar_command(a.path())
        .status()
        .expect("run a duplicate sidecar");
    assert!(duplicate.success());
    assert_eq!(
        ps_pid(child_a, "ppid"),
        sidecar_a,
        "the first sidecar stays"
    );
}

/// A sidecar tied to a process (the test run that launched it) exits once that
/// process is gone — the process census of a killed test run holds no
/// leftover sidecar and no leftover hosted child, whatever the run's guards
/// managed to do.
#[test]
fn a_sidecar_tied_to_a_process_exits_when_that_process_dies() {
    let root = short_root("rt6x");
    let mut cleanup = Cleanup::rooted_at(&[root.path()]);
    // Stands in for a test run that dies without dropping its guards.
    let mut holder = Command::new("/bin/sleep")
        .arg("60")
        .spawn()
        .expect("start the holder");
    cleanup.pids.push(holder.id() as i32);
    launch_via_exiting_shell_tied_to(root.path(), holder.id() as i32);

    let (mut client, sidecar) = connect(root.path());
    cleanup.pids.push(sidecar);
    let child = spawn_sleeper(&mut client, "tied-1");
    cleanup.pids.push(child);
    thread::sleep(Duration::from_millis(500));
    assert!(alive(sidecar), "the sidecar serves while its holder lives");

    holder.kill().expect("kill the holder");
    holder.wait().expect("reap the holder");
    let deadline = Instant::now() + Duration::from_secs(20);
    while (alive(sidecar) || alive(child)) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive(sidecar), "the sidecar exits with its holder");
    assert!(!alive(child), "its hosted children go with it");
    assert!(
        sidecars_rooted_at(root.path()).is_empty(),
        "no sidecar still serves the root"
    );
}
