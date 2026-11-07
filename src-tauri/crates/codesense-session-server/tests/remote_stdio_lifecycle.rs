// SPDX-License-Identifier: MIT
#![cfg(target_os = "linux")]
//! Real supervisor executable, controlled broker handshake and real Linux
//! descendants. This proves the lifecycle machinery, not provider inference.
use codesense_host_proto::{broker, reverse_mcp as wire};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn accept(listener: &UnixListener) -> UnixStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        match listener.accept() {
            Ok((socket, _)) => return socket,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("fixture accept failed: {error}"),
        }
        assert!(Instant::now() < deadline, "supervisor never registered");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn sleep() -> Process {
    Process(Command::new("/bin/sleep").arg("60").spawn().unwrap())
}
fn pidfd(pid: u32) -> OwnedFd {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    assert!(fd >= 0);
    unsafe { OwnedFd::from_raw_fd(fd as i32) }
}
fn exited(fd: &OwnedFd) -> bool {
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    unsafe { libc::poll(&mut poll, 1, 0) > 0 }
}
fn wait_file(path: &std::path::Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(pid) = text.trim().parse() {
                return pid;
            }
        }
        assert!(Instant::now() < deadline, "fixture did not start");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn finish(process: &mut Process, success: bool) {
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        if let Some(status) = process.0.try_wait().unwrap() {
            assert_eq!(status.success(), success);
            break;
        }
        assert!(Instant::now() < deadline, "supervisor exceeded deadline");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn real_supervisor_settles_detached_descendants_on_end_client_exit_and_server_exit() {
    for cause in [
        "end", "client", "server", "startup", "preexit", "signal", "crash", "many", "stdio",
    ] {
        for arrangement in 0..3 {
            let root = tempfile::tempdir().unwrap();
            let socket = root.path().join("broker.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let mut client = sleep();
            let mut unrelated = sleep();
            let inherited = std::fs::File::create(root.path().join("private-descriptor")).unwrap();
            broker::set_inheritable(inherited.as_raw_fd(), true).unwrap();
            let mut command = Command::new(env!("CARGO_BIN_EXE_codesense-session-server"));
            command
                .args([
                    "--mcp-stdio-supervisor",
                    socket.to_str().unwrap(),
                    "remote-fixture",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if arrangement == 1 {
                command.process_group(0);
            }
            if arrangement == 2 {
                unsafe {
                    command.pre_exec(|| {
                        if libc::setsid() < 0 {
                            Err(std::io::Error::last_os_error())
                        } else {
                            Ok(())
                        }
                    });
                }
            }
            let mut supervisor = Process(command.spawn().unwrap());
            broker::set_inheritable(inherited.as_raw_fd(), false).unwrap();
            let mut control = accept(&listener);
            control
                .set_read_timeout(Some(Duration::from_secs(4)))
                .unwrap();
            let mut mode = [0];
            control.read_exact(&mut mode).unwrap();
            assert_eq!(mode, [wire::MODE_REACQUIRE]);
            let (capability, _peer) = UnixStream::pair().unwrap();
            broker::send_fd(&control, capability.as_raw_fd()).unwrap();
            control.read_exact(&mut mode).unwrap();
            assert_eq!(mode, [5]);
            drop(broker::receive_fd(&control).unwrap());
            wire::write_challenge(&mut control, "fixture").unwrap();
            assert_eq!(
                wire::read_message(&mut control, 128, |_| Ok(())).unwrap(),
                format!("{} fixture\n", wire::AUDIENCE)
            );
            assert_eq!(
                wire::read_server_selector(&mut control, |_| Ok(())).unwrap(),
                "remote-fixture"
            );
            // Before the registration acknowledgement, an ordinary child must
            // still belong to the launching PTY/client group. Early setsid would
            // escape that owner before the Host had acknowledged ownership.
            if arrangement == 0 {
                assert_eq!(unsafe { libc::getpgid(supervisor.0.id() as i32) }, unsafe {
                    libc::getpgrp()
                });
            }
            let lifetime = pidfd(client.0.id());
            if cause == "preexit" {
                client.0.kill().unwrap();
                client.0.wait().unwrap();
            }
            broker::send_fd(&control, lifetime.as_raw_fd()).unwrap();
            let script = if cause == "stdio" {
                "echo $$ > descendant; printf controlled-diagnostic >&2; cat"
            } else if cause == "server" {
                "setsid /bin/sh -c 'echo $$ > descendant; sleep 60' & sleep 0.15; exit 0"
            } else if cause == "crash" {
                "setsid /bin/sh -c 'echo $$ > descendant; sleep 60' & sleep 0.15; exit 7"
            } else if cause == "many" {
                "i=0; while [ $i -lt 140 ]; do sleep 60 & echo $! >> owned; i=$((i+1)); done; setsid /bin/sh -c 'echo $$ > descendant; sleep 60' & wait"
            } else {
                "setsid /bin/sh -c 'echo $$ > descendant; sleep 60' & wait"
            };
            let script = format!("for fd in /proc/$$/fd/*; do readlink \"$fd\"; done > descriptors; cat /proc/$$/status > status; {script}");
            let config = serde_json::json!({
                "command": if cause == "startup" { "/missing-fixture-program" } else { "/bin/sh" },
                "args": ["-c", script], "cwd": root.path(), "env": {"PATH":"/usr/bin:/bin"}
            });
            let bytes = serde_json::to_vec(&config).unwrap();
            control
                .write_all(&(bytes.len() as u32).to_be_bytes())
                .unwrap();
            control.write_all(&bytes).unwrap();
            let descendant = if cause != "startup" && cause != "preexit" {
                Some(pidfd(wait_file(&root.path().join("descendant"))))
            } else {
                None
            };
            let extra: Vec<_> = if cause == "many" {
                std::fs::read_to_string(root.path().join("owned"))
                    .unwrap()
                    .lines()
                    .map(|pid| pidfd(pid.parse().unwrap()))
                    .collect()
            } else {
                vec![]
            };
            if cause == "stdio" {
                let payload = b"{\"jsonrpc\":\"2.0\",\"id\":1}\nopaque\0bytes\n";
                supervisor
                    .0
                    .stdin
                    .as_mut()
                    .unwrap()
                    .write_all(payload)
                    .unwrap();
                let output = supervisor.0.stdout.as_mut().unwrap();
                let fd = output.as_raw_fd();
                assert_eq!(
                    unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) },
                    0
                );
                let mut actual = Vec::new();
                let deadline = Instant::now() + Duration::from_secs(2);
                while actual.len() < payload.len() {
                    let mut buffer = [0; 64];
                    match output.read(&mut buffer) {
                        Ok(0) => panic!("stdio closed before exact response"),
                        Ok(n) => actual.extend_from_slice(&buffer[..n]),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(error) => panic!("stdio failed: {error}"),
                    }
                    assert!(Instant::now() < deadline, "stdio response timed out");
                    std::thread::sleep(Duration::from_millis(5));
                }
                assert_eq!(actual, payload);
            }
            if cause == "signal" {
                let fd = pidfd(supervisor.0.id());
                assert_eq!(
                    unsafe {
                        libc::syscall(
                            libc::SYS_pidfd_send_signal,
                            fd.as_raw_fd(),
                            libc::SIGTERM,
                            std::ptr::null::<libc::siginfo_t>(),
                            0,
                        )
                    },
                    0
                );
            }
            if cause == "end" || cause == "many" || cause == "stdio" {
                control.shutdown(std::net::Shutdown::Write).unwrap();
            }
            if cause == "client" {
                client.0.kill().unwrap();
                client.0.wait().unwrap();
            }
            let mut receipt = [0];
            control.read_exact(&mut receipt).unwrap();
            assert_eq!(receipt, [b'C']);
            finish(&mut supervisor, cause != "startup" && cause != "crash");
            if cause == "stdio" {
                let mut diagnostics = String::new();
                supervisor
                    .0
                    .stderr
                    .as_mut()
                    .unwrap()
                    .read_to_string(&mut diagnostics)
                    .unwrap();
                assert_eq!(diagnostics, "controlled-diagnostic");
            }
            for child in extra {
                assert!(exited(&child), "bounded batch left a child alive");
            }
            if cause == "preexit" {
                assert!(
                    !root.path().join("descriptors").exists(),
                    "server spawned after client exit"
                );
            }
            if cause != "startup" && cause != "preexit" {
                assert!(!std::fs::read_to_string(root.path().join("descriptors"))
                    .unwrap()
                    .contains("private-descriptor"));
                assert!(std::fs::read_to_string(root.path().join("status"))
                    .unwrap()
                    .contains("NoNewPrivs:\t1"));
            }
            if let Some(descendant) = descendant {
                assert!(exited(&descendant), "detached descendant survived {cause}");
            }
            assert!(unrelated.0.try_wait().unwrap().is_none());
        }
    }
}

#[test]
fn registration_refusal_spawns_nothing() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("broker.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut supervisor = Process(
        Command::new(env!("CARGO_BIN_EXE_codesense-session-server"))
            .args([
                "--mcp-stdio-supervisor",
                socket.to_str().unwrap(),
                "remote-fixture",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let control = accept(&listener);
    drop(control);
    finish(&mut supervisor, false);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}
