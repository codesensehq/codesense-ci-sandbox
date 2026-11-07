// SPDX-License-Identifier: MIT
//! The sidecar over a real socket, in-process: identity, routing, the
//! runtime's verbs, and survival of a client disconnect.

use std::collections::VecDeque;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use codesense_host_proto::{
    greet, read_frame, read_hello, write_frame, Census, Frame, GreetError, Hello, Kind, PeerHello,
    PROTO_MAJOR,
};
use codesense_pty_core::domain::{PtySessionId, TerminalDomain, TerminalDomainEvent, ViewerId};
use codesense_pty_core::protocol::{
    client_hello, instance_id, lock_path, socket_path, Inbound, Ingress, Inputs, Message, Reply,
    Request,
};
use codesense_pty_core::runner::ExitInfo;
use codesense_pty_core::spawn::SpawnIntent;
use codesense_pty_core::{SessionMeta, SessionRegistration};

use crate::server::{bind, path_arg, UNPARSEABLE_RID};
use crate::testing::{alive, registration, sh, short_root, wait_until, TestServer, WAIT};

struct TestClient {
    socket: UnixStream,
    frames: mpsc::Receiver<Frame>,
    ingress: Ingress,
    inputs: Inputs,
    events: VecDeque<TerminalDomainEvent>,
    next_rid: u64,
}

impl Drop for TestClient {
    fn drop(&mut self) {
        // The reader thread holds its own handle: close the socket itself, as a
        // quitting app does.
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
    }
}

/// This test's hello for the instance rooted at `root`.
fn hello_for(root: &Path) -> Hello {
    client_hello(instance_id(root), "test", &ViewerId::new("test"))
}

impl TestClient {
    /// Connect and greet with `hello`, returning the handshake's outcome.
    fn open(root: &Path, hello: Hello) -> (Self, Result<Hello, GreetError>) {
        let socket = UnixStream::connect(socket_path(root)).unwrap();
        let mut reader = socket.try_clone().unwrap();
        reader.set_read_timeout(Some(WAIT)).unwrap();
        let greeted = greet(&mut reader, &mut &socket, &hello);
        // Fails only once the sidecar has closed the connection.
        let _ = reader.set_read_timeout(None);
        let (tx, frames) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(Some(frame)) = read_frame(&mut reader) {
                if tx.send(frame).is_err() {
                    break;
                }
            }
        });
        let client = Self {
            socket,
            frames,
            ingress: Ingress::default(),
            inputs: Inputs::default(),
            events: VecDeque::new(),
            next_rid: 1,
        };
        (client, greeted)
    }

    /// A client of `server`'s instance that takes its events.
    fn hello(server: &TestServer) -> Self {
        let (mut client, greeted) = Self::open(server.root(), hello_for(server.root()));
        assert!(greeted.is_ok(), "{greeted:?}");
        assert_eq!(client.call(Request::Listen), Ok(Reply::Done));
        client
    }

    fn rid(&mut self) -> u64 {
        let rid = self.next_rid;
        self.next_rid += 1;
        rid
    }

    fn send(&mut self, request: Request) -> u64 {
        let rid = self.rid();
        let frame = Frame::control(&Message::Call { rid, request }).unwrap();
        write_frame(&mut self.socket, &frame).unwrap();
        rid
    }

    /// Send `data` to `session`'s input as raw bytes, without waiting.
    fn send_input(&mut self, session: &str, data: &str) -> u64 {
        let rid = self.rid();
        let next = &mut self.next_rid;
        let frames = self
            .inputs
            .write(&PtySessionId::new(session), rid, data.as_bytes(), || {
                *next += 1;
                *next - 1
            })
            .unwrap();
        for frame in frames {
            write_frame(&mut self.socket, &frame).unwrap();
        }
        rid
    }

    fn write(&mut self, session: &str, data: &str) -> Result<Reply, String> {
        let rid = self.send_input(session, data);
        self.reply(rid)
    }

    /// The next reply or event before `deadline`.
    fn next(&mut self, deadline: Instant, what: &str) -> Inbound {
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let frame = self
                .frames
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("{what} never arrived"));
            if let Some(inbound) = self.ingress.accept(&frame).unwrap() {
                return inbound;
            }
        }
    }

    fn reply(&mut self, rid: u64) -> Result<Reply, String> {
        let deadline = Instant::now() + WAIT;
        loop {
            match self.next(deadline, "reply") {
                Inbound::Reply { rid: got, result } if got == rid => return result,
                Inbound::Event(event) => self.events.push_back(event),
                Inbound::Reply { .. } | Inbound::ManifestPart { .. } => {}
            }
        }
    }

    fn call(&mut self, request: Request) -> Result<Reply, String> {
        let rid = self.send(request);
        self.reply(rid)
    }

    fn spawn(&mut self, server: &TestServer, intent: SpawnIntent, reg: SessionRegistration) -> u32 {
        server.track(&reg.session_id);
        match self.call(Request::Spawn {
            intent,
            registration: reg,
        }) {
            Ok(Reply::Spawned { pid: Some(pid) }) => pid,
            other => panic!("spawn failed: {other:?}"),
        }
    }

    fn event(&mut self, want: impl Fn(&TerminalDomainEvent) -> bool) -> TerminalDomainEvent {
        if let Some(at) = self.events.iter().position(&want) {
            return self.events.remove(at).unwrap();
        }
        let deadline = Instant::now() + WAIT;
        loop {
            match self.next(deadline, "event") {
                Inbound::Event(event) if want(&event) => return event,
                Inbound::Event(event) => self.events.push_back(event),
                Inbound::Reply { .. } | Inbound::ManifestPart { .. } => {}
            }
        }
    }

    fn output_until(&mut self, session: &str, needle: &str) -> String {
        let mut output = String::new();
        while !output.contains(needle) {
            if let TerminalDomainEvent::Output { data, .. } = self.event(
                |e| matches!(e, TerminalDomainEvent::Output { session: s, .. } if s.as_str() == session),
            ) {
                output.push_str(&data);
            }
        }
        output
    }
}

fn spawn_request(id: &str) -> Request {
    Request::Spawn {
        intent: sh("sleep 60"),
        registration: registration(id, None, false),
    }
}

fn stale_socket_fixture(path: &Path) -> std::os::fd::OwnedFd {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    let bytes = path.as_os_str().as_bytes();
    // SAFETY: sockaddr_un contains only integer fields; zeroes also terminate sun_path.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    assert!(
        !bytes.is_empty() && !bytes.contains(&0) && bytes.len() < address.sun_path.len(),
        "invalid stale socket fixture path {path:?}: expected 1..{} non-NUL bytes",
        address.sun_path.len()
    );
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (target, byte) in address.sun_path.iter_mut().zip(bytes) {
        *target = *byte as libc::c_char;
    }
    let address_len =
        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as libc::socklen_t;
    #[cfg(target_os = "macos")]
    {
        address.sun_len = address_len as u8;
    }

    let socket_type = libc::SOCK_STREAM;
    #[cfg(target_os = "linux")]
    let socket_type = socket_type | libc::SOCK_CLOEXEC;
    // SAFETY: socket takes scalar arguments and returns a new descriptor on success.
    let raw_fd = unsafe { libc::socket(libc::AF_UNIX, socket_type, 0) };
    assert!(
        raw_fd >= 0,
        "create stale socket fixture {path:?}: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: the successful socket call transferred sole ownership of this descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    #[cfg(not(target_os = "linux"))]
    {
        // SAFETY: fd is live, and F_SETFD takes an integer descriptor-flags argument.
        let cloexec = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
        assert!(
            cloexec == 0,
            "set close-on-exec for stale socket fixture {path:?}: {}",
            std::io::Error::last_os_error()
        );
    }
    // Never listen: retaining a duplicated or inherited descriptor alone does
    // not start listening, even after the original FD closes.
    // SAFETY: address is initialized, and address_len includes the path's NUL
    // terminator within its allocation. fd remains owned through the call.
    let bound = unsafe {
        libc::bind(
            fd.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            address_len,
        )
    };
    assert!(
        bound == 0,
        "bind stale socket fixture {path:?}: {}",
        std::io::Error::last_os_error()
    );
    fd
}

#[test]
fn bind_reclaims_a_stale_socket() {
    use std::os::unix::fs::FileTypeExt;

    let root = short_root();
    let path = socket_path(root.path());
    // Retain the old artifacts without acquiring a lock that a parallel fork
    // could keep alive after the fixture's descriptor closes.
    std::fs::File::create(lock_path(root.path())).expect("create empty unlocked lock file");
    drop(stale_socket_fixture(&path));
    assert!(
        std::fs::symlink_metadata(&path)
            .expect("stat stale socket fixture")
            .file_type()
            .is_socket(),
        "stale fixture must leave a socket at {path:?}"
    );
    let _again = bind(root.path()).expect("stale socket must be reclaimed");
}

#[test]
fn bind_reclaims_a_stale_socket_with_a_retained_descriptor() {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::FileTypeExt;

    let root = short_root();
    let path = socket_path(root.path());
    let fixture = stale_socket_fixture(&path);
    // SAFETY: fixture owns a live descriptor, and F_GETFD takes no third argument.
    let flags = unsafe { libc::fcntl(fixture.as_raw_fd(), libc::F_GETFD) };
    assert!(
        flags >= 0,
        "read stale socket fixture descriptor flags: {}",
        std::io::Error::last_os_error()
    );
    assert_ne!(
        flags & libc::FD_CLOEXEC,
        0,
        "stale socket fixture descriptor must have close-on-exec set, got flags {flags}"
    );
    // A clone models a descriptor inherited by a concurrent fork before exec.
    let retained = fixture.try_clone().expect("clone stale socket fixture FD");
    drop(fixture);
    assert!(
        std::fs::symlink_metadata(&path)
            .expect("stat stale socket fixture")
            .file_type()
            .is_socket(),
        "stale fixture must leave a socket at {path:?}"
    );
    let result = UnixStream::connect(&path);
    assert!(
        matches!(&result, Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused),
        "stale socket with a retained descriptor must refuse connections, got {result:?}"
    );
    let _bound = bind(root.path())
        .expect("stale socket must be reclaimed while its duplicate descriptor remains alive");
    drop(retained);
}

#[test]
fn a_second_sidecar_is_refused_while_the_first_holds_the_lock() {
    let root = short_root();
    let _first = bind(root.path()).unwrap();
    assert_eq!(
        bind(root.path()).err().map(|e| e.kind()),
        Some(std::io::ErrorKind::AddrInUse)
    );
}

/// The socket and the lock live under the instance root the sidecar was given,
/// owner-only; there is no default root.
#[test]
fn the_socket_and_lock_live_under_the_instance_root() {
    use std::os::unix::fs::PermissionsExt;
    let root = short_root();
    let bound = bind(root.path()).unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&socket_path(root.path())), 0o600);
    assert_eq!(mode(root.path()), 0o700);
    assert!(lock_path(root.path()).exists());
    assert_eq!(bound.instance_id, instance_id(root.path()));

    let args = |a: &[&str], flag| path_arg(a.iter().map(|s| s.to_string()), flag);
    assert_eq!(args(&["--root", "/tmp/x"], "--root"), Some("/tmp/x".into()));
    assert_eq!(
        args(
            &["--resources", "/app/res", "--root", "/tmp/x"],
            "--resources"
        ),
        Some("/app/res".into())
    );
    assert_eq!(args(&["--root", "/tmp/x"], "--resources"), None);
    assert_eq!(args(&[], "--root"), None);
    assert_eq!(args(&["--root"], "--root"), None);

    bound.record_pid().unwrap();
    assert_eq!(
        codesense_pty_core::protocol::host_pid(root.path()),
        Some(std::process::id())
    );
    assert_eq!(
        codesense_pty_core::protocol::host_running(root.path()),
        Ok(true),
        "the bound host holds the lock"
    );
    drop(bound);
    // A child forked meanwhile by a parallel test shares the lock until it execs.
    assert!(crate::testing::wait_until(|| {
        codesense_pty_core::protocol::host_running(root.path()) == Ok(false)
    }));
}

/// Over the socket: the child's output arrives as the runtime's own events and
/// its real non-zero exit code crosses in the `Exit` event; a malformed call is
/// answered with an error, and runtime errors cross verbatim.
#[test]
fn spawn_streams_output_and_the_real_exit_code() {
    let server = TestServer::start();
    let mut client = TestClient::hello(&server);
    let garbage = Frame {
        kind: Kind::CONTROL,
        channel: 0,
        payload: b"not json".to_vec(),
    };
    write_frame(&mut client.socket, &garbage).unwrap();
    assert!(client.reply(UNPARSEABLE_RID).is_err());
    let newer_request = Frame {
        kind: Kind::CONTROL,
        channel: 0,
        payload: br#"{"type":"call","rid":77,"request":{"op":"added_later"}}"#.to_vec(),
    };
    write_frame(&mut client.socket, &newer_request).unwrap();
    assert!(
        client
            .reply(77)
            .is_err_and(|e| e.starts_with("unsupported request")),
        "a request this build cannot read is answered on its own rid"
    );
    let newer_kind = Frame {
        kind: Kind(0x7f),
        channel: 5,
        payload: vec![1, 2, 3],
    };
    write_frame(&mut client.socket, &newer_kind).unwrap();
    assert_eq!(
        client.call(Request::IsIdle {
            session: "none".into()
        }),
        Ok(Reply::Flag { value: true }),
        "a frame kind this build does not take is skipped"
    );

    client.spawn(
        &server,
        sh("printf 'SOCK_OK\\n'; exit 3"),
        registration("e2e-1", None, false),
    );
    assert!(client.output_until("e2e-1", "SOCK_OK").contains("SOCK_OK"));
    match client.event(|e| matches!(e, TerminalDomainEvent::Exit { .. })) {
        TerminalDomainEvent::Exit {
            session, status, ..
        } => {
            assert_eq!(session.as_str(), "e2e-1");
            assert_eq!(
                status,
                Some(ExitInfo {
                    exit_code: Some(3),
                    signal: None
                })
            );
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(
        client.write("nope", "x"),
        Err("write_pty: session not found: nope".to_string())
    );
}

/// Over the socket, a hosted child's end says how it ended: killed by a signal
/// from outside, it reports the signal and was not requested; ended by the
/// sidecar's kill verb, it reports a requested exit.
#[test]
fn a_hosted_exit_carries_its_signal_and_whether_it_was_requested() {
    let server = TestServer::start();
    let mut client = TestClient::hello(&server);
    let exit_of = |client: &mut TestClient, id: &'static str| match client
        .event(|e| matches!(e, TerminalDomainEvent::Exit { session, .. } if session.as_str() == id))
    {
        TerminalDomainEvent::Exit {
            status, requested, ..
        } => (status, requested),
        other => panic!("unexpected {other:?}"),
    };

    client.spawn(
        &server,
        sh("kill -9 $$"),
        registration("crashed", None, false),
    );
    let (status, requested) = exit_of(&mut client, "crashed");
    let status = status.expect("reaped");
    assert_eq!(status.exit_code, None);
    assert!(status.signal.is_some(), "the signal crosses the wire");
    assert!(!requested);

    client.spawn(
        &server,
        sh("sleep 30"),
        registration("stopped", None, false),
    );
    assert!(client
        .call(Request::Kill {
            session: PtySessionId::new("stopped"),
        })
        .is_ok());
    let (_, requested) = exit_of(&mut client, "stopped");
    assert!(requested, "the sidecar's own kill is a requested exit");
}

/// Only a client of this instance at this protocol major gets past the
/// handshake: any other learns the sidecar's identity and census and is
/// disconnected before it can drive the runtime. A later minor is admitted.
#[test]
fn only_this_instance_gets_past_the_handshake() {
    let server = TestServer::start();
    for hello in [
        Hello {
            instance_id: "/somewhere/else".into(),
            ..hello_for(server.root())
        },
        Hello {
            proto_major: PROTO_MAJOR + 1,
            ..hello_for(server.root())
        },
    ] {
        let (mut stranger, greeted) = TestClient::open(server.root(), hello);
        let answer = greeted.expect("the sidecar greets every client");
        assert_eq!(answer.instance_id, instance_id(server.root()));
        let call = Frame::control(&Message::Call {
            rid: 1,
            request: spawn_request("stranger"),
        })
        .unwrap();
        let _ = write_frame(&mut stranger.socket, &call);
        assert!(
            stranger
                .frames
                .recv_timeout(Duration::from_secs(2))
                .is_err(),
            "the connection is closed without answering"
        );
    }
    assert_eq!(
        server.host().terminal().has_session(&"stranger".into()),
        Ok(false)
    );

    let later_minor = Hello {
        proto_minor: u16::MAX,
        ..hello_for(server.root())
    };
    let (mut newer, greeted) = TestClient::open(server.root(), later_minor);
    assert!(greeted.is_ok());
    assert_eq!(
        newer.call(Request::IsIdle {
            session: "none".into()
        }),
        Ok(Reply::Flag { value: true })
    );
}

/// The sidecar's hello carries its persisted identity, this lifetime's epoch
/// and its capabilities, and its census counts the sessions a restart would
/// end (not an exited one, nor an unattached warm-pool shell) — sent even to
/// a client at another major, which it then turns away.
#[test]
fn the_greeting_names_the_host_and_counts_its_live_sessions() {
    let server = TestServer::start();
    let mut client = TestClient::hello(&server);
    client.spawn(&server, sh("sleep 60"), registration("live-1", None, false));
    client.spawn(&server, sh("sleep 60"), registration("live-2", None, false));
    client.spawn(
        &server,
        sh("sleep 60"),
        registration("__warm_pool_c", None, true),
    );
    client.spawn(&server, sh("exit 0"), registration("done", None, false));
    client.event(
        |e| matches!(e, TerminalDomainEvent::Exit { session, .. } if session.as_str() == "done"),
    );

    let mut socket = UnixStream::connect(socket_path(server.root())).unwrap();
    socket.set_read_timeout(Some(WAIT)).unwrap();
    let older = Hello {
        proto_major: PROTO_MAJOR - 1,
        ..hello_for(server.root())
    };
    write_frame(&mut socket, &older.frame().unwrap()).unwrap();
    let hello = match read_hello(&mut socket) {
        Ok(PeerHello::Compatible(hello)) => hello,
        other => panic!("unexpected {other:?}"),
    };
    let identity = server.host().terminal().identity().unwrap();
    assert_eq!(hello.server_id, identity.server.id.as_str());
    assert_eq!(hello.server_name, identity.server.name);
    assert_eq!(hello.epoch, identity.epoch);
    assert_eq!(hello.rev, identity.rev);
    assert!(hello.supports(&codesense_host_proto::Capability::Terminals));
    assert_eq!(hello.pid, std::process::id());
    // The host names the build it came from.
    assert_eq!(hello.build, Some(crate::install::build_stamp()));
    let census = read_frame(&mut socket).unwrap().expect("census");
    assert_eq!(Census::from_frame(&census), Ok(Census { live_sessions: 2 }));
    assert_eq!(
        read_frame(&mut socket),
        Ok(None),
        "a client at another major is disconnected"
    );
}

/// Write and resize reach the hosted PTY; the idle probe sees its child.
#[test]
fn write_resize_and_idle_reach_the_pty() {
    let server = TestServer::start();
    let mut client = TestClient::hello(&server);
    client.spawn(
        &server,
        sh("read line; stty size; printf 'GOT=%s\\n' \"$line\"; sleep 60"),
        registration("io-1", None, false),
    );
    client
        .call(Request::Resize {
            viewer: codesense_pty_core::domain::ViewerId::new("test"),
            session: "io-1".into(),
            cols: 132,
            rows: 50,
        })
        .unwrap();
    client.write("io-1", "ping\r").unwrap();
    let output = client.output_until("io-1", "GOT=ping");
    assert!(output.contains("50 132"), "resized: {output:?}");
    let probe = std::cell::RefCell::new(TestClient::hello(&server));
    assert!(wait_until(|| probe.borrow_mut().call(Request::IsIdle {
        session: "io-1".into(),
    }) == Ok(Reply::Flag { value: false })));
    assert_eq!(
        client.call(Request::IsIdle {
            session: "gone".into(),
        }),
        Ok(Reply::Flag { value: true }),
        "an unknown session reads idle, as in-process"
    );
}

/// The host agent's terminal service over the wire, as a remote machine's
/// client reaches it: the greeting names the emulator, a subscribe answers
/// with the emulator's snapshot, a resize follows the stream's output as a
/// `StreamResized`, and the emulator's text is read.
#[test]
fn the_host_serves_snapshot_resize_and_text_over_the_wire() {
    let server = TestServer::start();
    let (mut client, greeted) = TestClient::open(server.root(), hello_for(server.root()));
    assert_eq!(
        greeted.unwrap().emulator,
        Some(codesense_host_proto::EmulatorHello::current())
    );
    assert_eq!(client.call(Request::Listen), Ok(Reply::Done));
    client.spawn(
        &server,
        sh("stty -echo; printf 'BEFORE\\n'; read line; printf 'AFTER\\n'; sleep 60"),
        registration("wire-1", None, false),
    );
    client.output_until("wire-1", "BEFORE");
    let Ok(Reply::Subscribed { result }) = client.call(Request::Subscribe {
        session: "wire-1".into(),
        viewport: codesense_pty_core::domain::ViewportId::new("vp"),
    }) else {
        panic!("no snapshot");
    };
    assert!(
        result.snapshot.vt.contains("BEFORE"),
        "{:?}",
        result.snapshot
    );
    assert_eq!((result.snapshot.cols, result.snapshot.rows), (80, 24));
    let stream = result.stream_id;
    client
        .call(Request::Resize {
            viewer: ViewerId::new("test"),
            session: "wire-1".into(),
            cols: 100,
            rows: 30,
        })
        .unwrap();
    client.write("wire-1", "go\r").unwrap();
    let resized = client.event(
        |e| matches!(e, TerminalDomainEvent::StreamResized { stream: s, .. } if *s == stream),
    );
    assert_eq!(
        resized,
        TerminalDomainEvent::StreamResized {
            stream,
            at_seq: result.snapshot.seq,
            resize_gen: result.snapshot.resize_gen + 1,
            cols: 100,
            rows: 30,
        }
    );
    let after = client.event(
        |e| matches!(e, TerminalDomainEvent::StreamOutput { stream: s, .. } if *s == stream),
    );
    let TerminalDomainEvent::StreamOutput { seq, .. } = after else {
        unreachable!()
    };
    assert!(seq > result.snapshot.seq, "the stream follows its snapshot");
    let client = std::cell::RefCell::new(client);
    assert!(wait_until(|| {
        matches!(
            client.borrow_mut().call(Request::ReadText {
                session: "wire-1".into(),
                lines: 10,
                mode: codesense_pty_core::emulator::TextMode::Normal,
            }),
            Ok(Reply::Text { text }) if text.text.contains("AFTER")
        )
    }));
    assert!(client
        .borrow_mut()
        .call(Request::DebugDump {
            session: "wire-1".into()
        })
        .unwrap_err()
        .starts_with("unsupported"));
}

/// A child that never reads its input cannot stall other requests: after more
/// input than the PTY buffers is queued for it, another session still receives
/// its input and can be killed promptly.
#[test]
fn a_child_that_ignores_input_does_not_block_other_requests() {
    let server = TestServer::start();
    let mut client = TestClient::hello(&server);
    let deaf = client.spawn(&server, sh("sleep 60"), registration("deaf", None, false));
    client.spawn(
        &server,
        sh("read line; printf 'HEARD=%s\\n' \"$line\"; sleep 5"),
        registration("ears", None, false),
    );
    let chunk = "x".repeat(16 * 1024);
    for _ in 0..64 {
        client.send_input("deaf", &chunk);
    }
    client.write("ears", "hi\r").unwrap();
    client.output_until("ears", "HEARD=hi");
    client
        .call(Request::Kill {
            session: "deaf".into(),
        })
        .unwrap();
    assert!(wait_until(|| !alive(deaf)));
}

/// The app quits: its connection goes away. A persisting session's child keeps
/// running and keeps appending to the sidecar's ring; the exit-time complement
/// kill spares it.
#[test]
fn a_persisting_session_survives_the_client_and_keeps_filling_the_ring() {
    let server = TestServer::start();
    let mut client = TestClient::hello(&server);
    let pid = client.spawn(
        &server,
        sh("printf 'BEFORE\\n'; sleep 1; printf 'AFTER\\n'; sleep 30"),
        registration("keep-1", Some("SW-1"), false),
    );
    client.output_until("keep-1", "BEFORE");
    client.call(Request::ReapNonPersisting).unwrap();
    drop(client);

    assert!(
        wait_until(|| server.text("keep-1").contains("AFTER")),
        "output after the client left reaches the terminal: {:?}",
        server.text("keep-1")
    );
    assert!(alive(pid), "the child outlives its client");
}

/// A scoped exit reap takes only the non-persisting sessions it names: one
/// opened by another client, or claimed by none, runs on.
#[test]
fn a_scoped_reap_kills_only_the_sessions_it_names() {
    let server = TestServer::start();
    let mut a = TestClient::hello(&server);
    let mut b = TestClient::hello(&server);
    let a_plain = a.spawn(
        &server,
        sh("sleep 60"),
        registration("a-plain", None, false),
    );
    let a_member = a.spawn(
        &server,
        sh("sleep 60"),
        registration("__warm_pool_a", None, true),
    );
    let b_plain = b.spawn(
        &server,
        sh("sleep 60"),
        registration("b-plain", None, false),
    );
    let b_member = b.spawn(
        &server,
        sh("sleep 60"),
        registration("__warm_pool_b", None, true),
    );
    a.call(Request::ReapOwnedNonPersisting {
        sessions: vec!["a-plain".into(), "__warm_pool_a".into(), "gone".into()],
    })
    .unwrap();
    assert!(wait_until(|| !alive(a_plain) && !alive(a_member)));
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(
        alive(b_plain) && alive(b_member),
        "another client's sessions run on"
    );
    for session in ["b-plain", "__warm_pool_b"] {
        b.call(Request::Kill {
            session: session.into(),
        })
        .unwrap();
    }
}

/// Reap as `client` until `gone` (a pool member only the last client takes)
/// is: the host has seen every other client's connection close.
fn reap_until_alone(client: &mut TestClient, gone: u32) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while alive(gone) && std::time::Instant::now() < deadline {
        client
            .call(Request::ReapOwnedNonPersisting { sessions: vec![] })
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        wait_until(|| !alive(gone)),
        "the last client reaps a pool member"
    );
}

/// The last client to quit takes every warm-pool member no client claims at
/// once, but another app's tab only once no viewing client has been connected
/// for the leftover grace (and the tab went unviewed that long): an app that
/// may only be reconnecting keeps its tabs, however long they went unviewed,
/// while a probe that never viewed a terminal does not hold the reap off.
/// While another client is connected, the reap takes only what it names.
#[test]
fn the_last_client_reaps_leftovers_only_once_alone_past_the_grace() {
    let grace = std::time::Duration::from_secs(5 * 60);
    let server = TestServer::start();
    let mut crashed = TestClient::hello(&server);
    let member = crashed.spawn(
        &server,
        sh("sleep 60"),
        registration("__warm_pool_left", None, true),
    );
    let tab = crashed.spawn(
        &server,
        sh("sleep 60"),
        registration("left-tab", None, false),
    );
    view(&mut crashed, "left-tab");
    drop(crashed);

    let mut quitting = TestClient::hello(&server);
    reap_until_alone(&mut quitting, member);
    assert!(alive(tab), "its app left just now: it may be reconnecting");

    let mut other = TestClient::hello(&server);
    let others_member = other.spawn(
        &server,
        sh("sleep 60"),
        registration("__warm_pool_other", None, true),
    );
    view(&mut other, "left-tab");
    server.host().advance_clock(grace * 2);
    quitting
        .call(Request::ReapOwnedNonPersisting { sessions: vec![] })
        .unwrap();
    assert!(
        alive(tab) && alive(others_member),
        "another client is connected"
    );

    drop(other);
    reap_until_alone(&mut quitting, others_member);
    assert!(
        alive(tab),
        "unviewed past the grace, but a client left just now"
    );

    server.host().advance_clock(grace * 2);
    // A connection that never viewed a terminal (a probe, a CLI call)
    // closing meanwhile does not count as an app that may come back.
    let mut probe = TestClient::hello(&server);
    assert_eq!(
        probe.call(Request::HasSession {
            session: "left-tab".into(),
        }),
        Ok(Reply::Flag { value: true })
    );
    drop(probe);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while alive(tab) && std::time::Instant::now() < deadline {
        quitting
            .call(Request::ReapOwnedNonPersisting { sessions: vec![] })
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        wait_until(|| !alive(tab)),
        "alone past the grace: a leftover, whatever probed meanwhile"
    );
}

/// Attach a viewport of `session` on `client`, as an app does.
fn view(client: &mut TestClient, session: &str) {
    assert!(matches!(
        client.call(Request::Subscribe {
            session: session.into(),
            viewport: codesense_pty_core::domain::ViewportId::new("vp"),
        }),
        Ok(Reply::Subscribed { .. })
    ));
}

/// A connected app, as its hello names it, listening to events.
fn app_client(server: &TestServer, client: &str) -> TestClient {
    let mut hello = hello_for(server.root());
    hello.client_id = Some(client.into());
    let (mut app, greeted) = TestClient::open(server.root(), hello);
    let greeted = greeted.expect("greeted");
    assert!(greeted.supports(&codesense_host_proto::Capability::ServerStatusV1));
    assert_eq!(app.call(Request::Listen), Ok(Reply::Done));
    app
}

/// What the host says it holds, as `client` asks.
fn server_status(client: &mut TestClient) -> codesense_pty_core::protocol::ServerStatus {
    match client.call(Request::ServerStatus) {
        Ok(Reply::ServerStatus { status }) => *status,
        other => panic!("no status: {other:?}"),
    }
}

/// The host answers what it holds: its sessions (running, and exited
/// but kept), not its warm-pool shells; the apps listening to it, the asker's
/// own counted apart; and the sessions no app holds — one whose opening app
/// has been gone past the grace and that nothing shows, until a viewport
/// shows it.
#[test]
fn the_host_answers_what_it_holds_and_what_no_app_holds() {
    let server = TestServer::start();
    let mut a = app_client(&server, "app-a");
    let mut b = app_client(&server, "app-b");
    a.spawn(
        &server,
        sh("sleep 60"),
        registration("a-shell", None, false),
    );
    let done = a.spawn(&server, sh("exit 0"), registration("a-done", None, false));
    a.spawn(
        &server,
        sh("sleep 60"),
        registration("__warm_pool_a", None, true),
    );
    b.spawn(&server, sh("sleep 60"), registration("b-left", None, false));
    assert!(wait_until(|| !alive(done)));

    let status = server_status(&mut a);
    assert_eq!(
        (status.sessions, status.running, status.exited_kept),
        (3, 2, 1)
    );
    assert_eq!(status.running_agents, 0);
    assert_eq!((status.clients, status.other_clients), (2, 1));
    assert!(status.unowned.is_empty(), "every opener is connected");
    assert!(status.started_at_ms > 0);
    // Its open-file budget, counted from its own descriptors.
    let nofile = status.nofile.expect("the host reads its open-file budget");
    assert!(nofile.open.is_some_and(|open| open > 0 && open <= nofile.soft));

    drop(b);
    assert!(wait_until(|| server.host().connection_count() == 1));
    let status = server_status(&mut a);
    assert_eq!((status.clients, status.other_clients), (1, 0));
    assert!(
        status.unowned.is_empty(),
        "its app left just now: it may be reconnecting or relaunching"
    );

    server.host().advance_clock(LEFTOVER_GRACE_FOR_TESTS);
    let status = server_status(&mut a);
    let unowned: Vec<_> = status
        .unowned
        .iter()
        .map(|left| (left.session.as_str(), left.running))
        .collect();
    assert_eq!(
        unowned,
        [("b-left", true)],
        "its app is gone past the grace"
    );
    assert!(status.unowned[0].started_at_ms > 0);

    assert!(matches!(
        a.call(Request::Subscribe {
            session: "b-left".into(),
            viewport: codesense_pty_core::domain::ViewportId::new("vp"),
        }),
        Ok(Reply::Subscribed { .. })
    ));
    assert!(
        server_status(&mut a).unowned.is_empty(),
        "a viewport shows it now"
    );
}

/// The grace a test moves the host's clock past (the host's own is 5 minutes).
const LEFTOVER_GRACE_FOR_TESTS: Duration = Duration::from_secs(5 * 60);

/// A relaunched app claims what it holds, so a session its previous
/// process opened is never listed; End re-checks as it ends: refused while an
/// app holds the session, it ends one no app holds.
#[test]
fn a_claimed_session_is_held_and_end_rechecks_as_it_ends() {
    let server = TestServer::start();
    let mut first = app_client(&server, "app-first-run");
    let kept = first.spawn(&server, sh("sleep 60"), registration("kept", None, false));
    let left = first.spawn(&server, sh("sleep 60"), registration("left", None, false));
    drop(first);
    assert!(wait_until(|| server.host().connection_count() == 0));

    let mut relaunched = app_client(&server, "app-second-run");
    let refused = relaunched.call(Request::EndUnownedSession {
        session: "left".into(),
    });
    assert!(
        refused.is_err(),
        "inside the grace an app may be coming back"
    );
    assert_eq!(
        relaunched.call(Request::ClaimSessions {
            sessions: vec!["kept".into(), "gone".into()],
        }),
        Ok(Reply::Done)
    );
    server.host().advance_clock(LEFTOVER_GRACE_FOR_TESTS);
    let unowned: Vec<_> = server_status(&mut relaunched)
        .unowned
        .into_iter()
        .map(|left| left.session.as_str().to_string())
        .collect();
    assert_eq!(unowned, ["left"], "the claimed session is held");

    assert!(relaunched
        .call(Request::EndUnownedSession {
            session: "kept".into(),
        })
        .is_err());
    assert!(alive(kept), "an app holds it: nothing ends");
    assert_eq!(
        relaunched.call(Request::EndUnownedSession {
            session: "left".into(),
        }),
        Ok(Reply::Done)
    );
    assert!(wait_until(|| !alive(left)), "no app holds it: it ends");
    relaunched
        .call(Request::Kill {
            session: "kept".into(),
        })
        .unwrap();
}

/// A layout tab open in `app`, its process spawned by that app.
fn open_tab_in(server: &TestServer, app: &mut TestClient, session: &str) {
    open_entry_in(
        server,
        app,
        session,
        codesense_pty_core::layout::Placement::Tab,
    );
}

/// A layout entry placed as `placement` (a tab, or a hidden background
/// entry) open in `app`, its process spawned by that app.
fn open_entry_in(
    server: &TestServer,
    app: &mut TestClient,
    session: &str,
    placement: codesense_pty_core::layout::Placement,
) {
    use codesense_pty_core::layout::{OpenTab, WorkspaceDescriptor};
    let request = OpenTab {
        viewer: ViewerId::new("app"),
        workspace: codesense_pty_core::domain::WorkspaceKey::new("root:acme/app"),
        session: PtySessionId::new(session),
        descriptor: Some(WorkspaceDescriptor {
            display_name: "app".into(),
            kind: "root".into(),
            ..Default::default()
        }),
        name: None,
        role: None,
        tags: Default::default(),
        placement,
        index: None,
        activate: false,
    };
    let id = session.to_string();
    server
        .host()
        .terminal()
        .open_tab(
            request,
            Box::new(move || {
                app.spawn(server, sh("sleep 60"), registration(&id, None, false));
                Ok(())
            }),
        )
        .expect("tab opened");
}

/// Another app's claim never takes a live app's sessions; and a layout
/// entry — a tab, or a hidden (background) one — is never one no app holds
/// (every app shows it), even off-screen, its opener gone and the grace past.
#[test]
fn a_claim_never_takes_a_live_apps_sessions_and_a_layout_entry_is_never_listed() {
    let server = TestServer::start();
    let mut a = app_client(&server, "app-a");
    a.spawn(
        &server,
        sh("sleep 60"),
        registration("a-background", None, false),
    );
    open_tab_in(&server, &mut a, "a-tab");
    open_entry_in(
        &server,
        &mut a,
        "a-hidden",
        codesense_pty_core::layout::Placement::Background,
    );

    let mut b = app_client(&server, "app-b");
    assert_eq!(
        b.call(Request::ClaimSessions {
            sessions: vec!["a-background".into(), "a-tab".into()],
        }),
        Ok(Reply::Done)
    );
    drop(b);
    assert!(wait_until(|| server.host().connection_count() == 1));
    server.host().advance_clock(LEFTOVER_GRACE_FOR_TESTS);
    assert!(
        server_status(&mut a).unowned.is_empty(),
        "A still holds its own: B's claim took nothing"
    );

    drop(a);
    let mut c = app_client(&server, "app-c");
    assert!(wait_until(|| server.host().connection_count() == 1));
    server.host().advance_clock(LEFTOVER_GRACE_FOR_TESTS);
    let unowned: Vec<_> = server_status(&mut c)
        .unowned
        .into_iter()
        .map(|left| left.session.as_str().to_string())
        .collect();
    assert_eq!(
        unowned,
        ["a-background"],
        "a layout entry — a tab or a hidden one — is never listed"
    );
    for entry in ["a-tab", "a-hidden"] {
        assert!(
            c.call(Request::EndUnownedSession {
                session: entry.into(),
            })
            .is_err(),
            "{entry}: End is refused"
        );
    }
    for session in ["a-background", "a-tab", "a-hidden"] {
        c.call(Request::Kill {
            session: session.into(),
        })
        .unwrap();
    }
}

/// A warm-hit terminal is hosted like a cold spawn: the member lives in the
/// sidecar, the handoff runs there, the unhanded member is reaped at exit, and
/// the handed-off session survives the client and keeps filling its terminal.
#[test]
fn a_warm_hit_terminal_survives_like_a_cold_spawn() {
    let server = TestServer::start();
    let target = short_root();
    let mut client = TestClient::hello(&server);
    let member = client.spawn(
        &server,
        sh("exec /bin/sh"),
        registration("__warm_pool_a", None, true),
    );
    let spare = client.spawn(
        &server,
        sh("exec /bin/sh"),
        registration("__warm_pool_b", None, true),
    );
    server.track("warm-1");
    client
        .call(Request::HandoffWarmMember {
            member: "__warm_pool_a".into(),
            session: "warm-1".into(),
            target_cwd: target.path().to_string_lossy().into_owned(),
            cols: 100,
            rows: 30,
            meta: SessionMeta {
                story_id: Some("SW-2".into()),
                ..SessionMeta::default()
            },
        })
        .unwrap();
    client
        .write("warm-1", "while :; do echo TICK; sleep 0.2; done\n")
        .unwrap();
    client.call(Request::ReapNonPersisting).unwrap();
    assert!(
        wait_until(|| !alive(spare)),
        "the unhanded member is reaped"
    );
    drop(client);

    let ticks = || server.text("warm-1").matches("TICK").count();
    assert!(wait_until(|| ticks() >= 2));
    let seen = ticks();
    assert!(
        wait_until(|| ticks() > seen),
        "the ring keeps filling after the client left"
    );
    assert!(alive(member), "the handed-off child outlives its client");
}

/// A kill sent right behind a spawn of the same session is never lost, however
/// the two interleave: either it tears down the registered session, or it
/// lands while the spawn is in flight and the spawn tears the session down as
/// it registers and reports that.
#[test]
fn a_kill_racing_the_spawn_never_leaves_the_session_running() {
    let server = TestServer::start();
    let mut spawner = TestClient::hello(&server);
    let mut killer = TestClient::hello(&server);
    for round in 0..10 {
        let id = format!("racy-{round}");
        server.track(&id);
        let spawn = spawner.send(spawn_request(&id));
        killer
            .call(Request::Kill {
                session: id.as_str().into(),
            })
            .unwrap();
        match spawner.reply(spawn) {
            Ok(Reply::Spawned { .. }) => {
                // The kill ran before the spawn began: kill again, as a
                // caller seeing the session would.
                killer
                    .call(Request::Kill {
                        session: id.as_str().into(),
                    })
                    .unwrap();
            }
            // The kill landed while the spawn was in flight: cancelled.
            Ok(Reply::Cancelled) => {}
            other => panic!("unexpected {other:?}"),
        }
        assert!(
            wait_until(|| server
                .host()
                .terminal()
                .has_session(&PtySessionId::new(id.as_str()))
                == Ok(false)),
            "{id} is not left registered"
        );
    }
}

/// A lost spawn reply is cancelled however far the spawn got. A spawn not yet
/// read is refused when it arrives (its caller already opened it elsewhere, so
/// the refusal falls open), and a cancel for a session the sidecar never
/// registered reports no removal (the id may already be open in the app).
#[test]
fn cancelling_a_lost_spawn_covers_a_spawn_not_yet_read() {
    let server = TestServer::start();
    let mut client = TestClient::hello(&server);
    client
        .call(Request::CancelSpawn {
            session: "late".into(),
        })
        .unwrap();
    assert_eq!(
        client.call(spawn_request("late")),
        Err("spawn_pty: session late was cancelled".to_string()),
        "the spawn read after the cancel is refused"
    );
    assert_eq!(
        server.host().terminal().has_session(&"late".into()),
        Ok(false)
    );
    server.track("late");
    assert!(
        matches!(
            client.call(spawn_request("late")),
            Ok(Reply::Spawned { .. })
        ),
        "the cancel applies to one spawn only"
    );
    assert!(
        !client.events.iter().any(|e| matches!(
            e,
            TerminalDomainEvent::SessionRemoved { session, .. } if session.as_str() == "late"
        )),
        "nothing was removed, so nothing is reported"
    );
}

/// A cancel for a registered session kills it before it is answered.
#[test]
fn cancelling_a_registered_spawn_kills_it_before_answering() {
    let server = TestServer::start();
    let mut client = TestClient::hello(&server);
    let pid = client.spawn(&server, sh("sleep 60"), registration("done", None, false));
    client
        .call(Request::CancelSpawn {
            session: "done".into(),
        })
        .unwrap();
    assert_eq!(
        server.host().terminal().has_session(&"done".into()),
        Ok(false)
    );
    assert!(wait_until(|| !alive(pid)));
}

/// A cancel that finds the spawn in flight is answered only once that spawn
/// is torn down, and the spawn answers `Cancelled`: when the caller hears the
/// cancel confirmed, the sidecar no longer runs it.
#[test]
fn cancelling_a_spawn_in_flight_answers_once_it_is_torn_down() {
    let server = TestServer::start();
    let mut spawner = TestClient::hello(&server);
    let mut canceller = TestClient::hello(&server);
    for round in 0..10 {
        let id = format!("inflight-{round}");
        server.track(&id);
        let spawn = spawner.send(spawn_request(&id));
        canceller
            .call(Request::CancelSpawn {
                session: id.as_str().into(),
            })
            .unwrap();
        // Confirmed: the sidecar runs nothing under this id any more.
        assert!(
            server
                .host()
                .terminal()
                .has_session(&PtySessionId::new(id.as_str()))
                == Ok(false),
            "{id}"
        );
        match spawner.reply(spawn) {
            Ok(Reply::Cancelled) => {}
            // The cancel was read before the spawn: that spawn is refused.
            Err(message) => assert!(message.contains("was cancelled"), "{message}"),
            // The spawn registered before the cancel: the cancel killed it.
            Ok(Reply::Spawned { pid: Some(pid) }) => assert!(wait_until(|| !alive(pid))),
            other => panic!("unexpected {other:?}"),
        }
    }
}

/// A connection that closes detaches the viewports it attached, so a stream
/// nobody reads never keeps a session looking viewed: while attached the exit
/// reap spares it as persisting, once its viewer's connection is gone it goes.
#[test]
fn a_closed_connection_detaches_its_viewports() {
    let server = TestServer::start();
    let mut client = TestClient::hello(&server);
    client.spawn(&server, sh("sleep 60"), registration("viewed", None, false));
    assert!(matches!(
        client.call(Request::Subscribe {
            session: "viewed".into(),
            viewport: codesense_pty_core::domain::ViewportId::new("vp"),
        }),
        Ok(Reply::Subscribed { .. })
    ));
    let reaper = std::cell::RefCell::new(TestClient::hello(&server));
    let reap = || {
        reaper
            .borrow_mut()
            .call(Request::ReapNonPersisting)
            .unwrap()
    };
    reap();
    let terminal = server.host().terminal();
    assert_eq!(
        terminal.has_session(&"viewed".into()),
        Ok(true),
        "an attached viewport keeps it"
    );
    drop(client);
    assert!(
        wait_until(|| {
            reap();
            terminal.has_session(&"viewed".into()) == Ok(false)
        }),
        "the viewport was detached with its connection"
    );
}

/// One runtime: the sidecar hosts `codesense-pty-core` and re-implements none
/// of it — no reader loop, batcher, replay buffer, process-group kill, exit
/// mapping, or displacement of its own.
#[test]
fn the_sidecar_hosts_pty_core_and_reimplements_none_of_it() {
    let sources = [
        ("host.rs", include_str!("host.rs")),
        ("server.rs", include_str!("server.rs")),
        ("main.rs", include_str!("main.rs")),
        ("detach.rs", include_str!("detach.rs")),
    ];
    let forbidden = [
        ["run_pty", "_batcher"].concat(),
        ["PTY_BATCH", "_WINDOW"].concat(),
        ["Replay", "Buffer"].concat(),
        ["SharedTerm", "State"].concat(),
        ["getp", "gid"].concat(),
        ["kill", "pg"].concat(),
        ["SIG", "KILL"].concat(),
        ["try", "_wait"].concat(),
        ["Exit", "Status"].concat(),
        ["exit", "_code()"].concat(),
        ["open", "pty"].concat(),
        ["spawn", "_command"].concat(),
        ["take", "_reader"].concat(),
        ["read(&mut ", "buf"].concat(),
        ["sessions.", "insert("].concat(),
        ["block", "_on("].concat(),
    ];
    for (name, source) in sources {
        for token in &forbidden {
            assert!(
                !source.contains(token.as_str()),
                "{name} re-implements runtime code (`{token}`); call codesense-pty-core instead"
            );
        }
    }
}

#[test]
fn queued_native_spawn_without_live_provision_never_runs_shell_fallback() {
    use codesense_host_proto::agent_cli::{AgentCli, NativeLaunch};
    let server = TestServer::start();
    #[cfg(target_os = "linux")]
    server.enable_reverse_mcp();
    let mut client = TestClient::hello(&server);
    for (id, launch) in [
        ("missing-claude", NativeLaunch::Cli(AgentCli::Claude)),
        ("retired-codex", NativeLaunch::Cli(AgentCli::Codex)),
        ("profile-shell", NativeLaunch::Shell),
    ] {
        #[cfg(target_os = "linux")]
        if matches!(
            launch,
            NativeLaunch::Cli(AgentCli::Codex) | NativeLaunch::Shell
        ) {
            assert_eq!(
                client.call(Request::PrepareReverseMcp {
                    session: PtySessionId::new(id),
                    binding_nonce: "a".repeat(64),
                    server_ids: codesense_host_proto::reverse_mcp::default_servers(),
                    remote_entries: vec![],
                    launch,
                    agent_startup: None,
                }),
                Ok(Reply::Done)
            );
            assert_eq!(
                client.call(Request::ReleaseReverseMcp {
                    session: PtySessionId::new(id)
                }),
                Ok(Reply::Done)
            );
        }
        let mut intent = sh("printf FORBIDDEN_SHELL_FALLBACK; sleep 60");
        intent.remote_launch = Some(launch);
        let rid = client.send(Request::Spawn {
            intent,
            registration: registration(id, None, false),
        });
        assert!(client
            .reply(rid)
            .unwrap_err()
            .contains("live session MCP provisioning"));
        assert_eq!(
            server.host().terminal().has_session(&PtySessionId::new(id)),
            Ok(false)
        );
    }
    // A refused native request does not poison the connection or ordinary shell behavior.
    let pid = client.spawn(
        &server,
        sh("printf LEGITIMATE_SHELL; sleep 60"),
        registration("legitimate-shell", None, false),
    );
    assert!(alive(pid));
    assert!(client
        .output_until("legitimate-shell", "LEGITIMATE_SHELL")
        .contains("LEGITIMATE_SHELL"));
    client
        .call(Request::Kill {
            session: PtySessionId::new("legitimate-shell"),
        })
        .unwrap();
}

/// A local host has no reverse broker, including on Linux unless enabled.
/// Missing/empty argv must not turn a marked request into a command-line shell.
#[test]
fn no_broker_rejects_marked_launches_without_direct_native_argv() {
    use codesense_host_proto::agent_cli::{AgentCli, NativeLaunch};
    let server = TestServer::start();
    let mut client = TestClient::hello(&server);
    let fallback = server.root().join("forbidden-fallback");
    let command = format!(
        "touch {}; sleep 60",
        codesense_pty_core::shell_quote(fallback.to_str().unwrap())
    );
    for (name, launch) in [
        ("claude", NativeLaunch::Cli(AgentCli::Claude)),
        ("codex", NativeLaunch::Cli(AgentCli::Codex)),
        ("shell", NativeLaunch::Shell),
    ] {
        for (kind, argv) in [("absent", None), ("empty", Some(Vec::new()))] {
            let id = format!("{name}-{kind}");
            server.track(&id);
            let mut intent = sh(&command);
            intent.remote_launch = Some(launch);
            intent.argv = argv;
            assert_eq!(
                client.call(Request::Spawn {
                    intent,
                    registration: registration(&id, None, false),
                }),
                Err("remote launch requires live session MCP provisioning".into())
            );
            assert_eq!(
                server
                    .host()
                    .terminal()
                    .has_session(&PtySessionId::new(&id)),
                Ok(false)
            );
        }
    }
    // Direct argv is a local native-provider exception, not a shell bypass.
    server.track("shell-direct");
    let mut intent = sh(&command);
    intent.remote_launch = Some(NativeLaunch::Shell);
    intent.argv = Some(vec!["/bin/sh".into(), "-c".into(), command]);
    assert_eq!(
        client.call(Request::Spawn {
            intent,
            registration: registration("shell-direct", None, false),
        }),
        Err("remote launch requires live session MCP provisioning".into())
    );
    assert_eq!(
        server
            .host()
            .terminal()
            .has_session(&PtySessionId::new("shell-direct")),
        Ok(false)
    );
    assert!(
        !fallback.exists(),
        "a rejected launch ran its shell command"
    );
}

#[test]
fn no_broker_rejects_before_writing_shell_wrappers_and_releases_the_session_id() {
    use codesense_host_proto::agent_cli::NativeLaunch;
    use codesense_pty_core::spawn::HostEnvironment;
    let root = short_root();
    let codesense_home = root.path().join("isolated-codesense-home");
    let user_home = root.path().join("isolated-user-home");
    std::fs::create_dir(&user_home).unwrap();
    let environment = HostEnvironment::with_vars(
        Some(codesense_home.clone()),
        None,
        [
            ("SHELL".into(), "/bin/bash".into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("HOME".into(), user_home.to_string_lossy().into_owned()),
        ],
    );
    let server = TestServer::start_at_with_environment(root.path(), environment);
    let mut client = TestClient::hello(&server);
    let id = "rejected-interactive-shell";
    server.track(id);
    let mut intent = sh("");
    intent.command = None;
    intent.env.clear();
    intent.remote_launch = Some(NativeLaunch::Shell);
    assert!(intent.is_interactive());
    assert_eq!(
        client.call(Request::Spawn {
            intent: intent.clone(),
            registration: registration(id, None, false),
        }),
        Err("remote launch requires live session MCP provisioning".into())
    );
    assert!(
        !codesense_home.exists(),
        "rejection wrote managed shell resources"
    );
    assert_eq!(std::fs::read_dir(&user_home).unwrap().count(), 0);
    assert_eq!(
        server.host().terminal().has_session(&PtySessionId::new(id)),
        Ok(false)
    );
    assert!(server.host().terminal().activity().snapshot(id).is_none());

    // Reusing the refused ID proves no spawn reservation remains. This same
    // interactive intent, now unmarked, also proves the fixture writes wrappers
    // when resolution actually runs; the no-write assertion above is not vacuous.
    intent.remote_launch = None;
    let pid = client.spawn(&server, intent, registration(id, None, false));
    assert!(alive(pid));
    assert!(codesense_home.join("shell/bash/bashrc").is_file());
    assert_eq!(
        server.host().terminal().has_session(&PtySessionId::new(id)),
        Ok(true)
    );
    client
        .call(Request::Kill {
            session: PtySessionId::new(id),
        })
        .unwrap();
}

#[test]
fn no_broker_direct_native_argv_launches_and_binds_activity_without_shell_fallback() {
    use codesense_host_proto::agent_cli::{AgentCli, NativeLaunch};
    use codesense_pty_core::activity::{SOCKET_ENV, TOKEN_ENV};
    let server = TestServer::start();
    server.host().start_activity(server.root()).unwrap();
    let mut client = TestClient::hello(&server);
    let fallback = server.root().join("forbidden-fallback");
    for (id, launch, provider) in [
        (
            "local-claude",
            NativeLaunch::Cli(AgentCli::Claude),
            AgentCli::Claude,
        ),
        (
            "local-codex",
            NativeLaunch::Cli(AgentCli::Codex),
            AgentCli::Codex,
        ),
    ] {
        let mut intent = sh(&format!(
            "touch {}; sleep 60",
            codesense_pty_core::shell_quote(fallback.to_str().unwrap())
        ));
        intent.remote_launch = Some(launch);
        intent.argv = Some(vec![
            "/bin/sh".into(),
            "-c".into(),
            format!(
                "test -S \"${{{SOCKET_ENV}}}\" && test -n \"${{{TOKEN_ENV}}}\" && printf DIRECT_NATIVE_BOUND; sleep 60"
            ),
        ]);
        let pid = client.spawn(&server, intent, registration(id, None, false));
        assert!(alive(pid));
        assert!(client
            .output_until(id, "DIRECT_NATIVE_BOUND")
            .contains("DIRECT_NATIVE_BOUND"));
        let snapshot = server.host().terminal().activity().snapshot(id).unwrap();
        assert_eq!(snapshot.provider, provider);
        assert!(!fallback.exists(), "direct argv fell back to command");
        client
            .call(Request::Kill {
                session: PtySessionId::new(id),
            })
            .unwrap();
    }
    // The guard does not change the ordinary unmarked shell contract.
    let pid = client.spawn(
        &server,
        sh("printf ORDINARY_SHELL; sleep 60"),
        registration("ordinary-shell", None, false),
    );
    assert!(alive(pid));
    assert!(client
        .output_until("ordinary-shell", "ORDINARY_SHELL")
        .contains("ORDINARY_SHELL"));
    client
        .call(Request::Kill {
            session: PtySessionId::new("ordinary-shell"),
        })
        .unwrap();
}

/// A saved Agent's Claude, handed its whole argv with a generated `--settings` file, is
/// spawned with that flag naming a private copy that carries the activity observer and
/// keeps the inbox-wait hooks (so its wakeups are delivered, not dropped as unobserved).
#[test]
fn a_saved_agent_claude_spawns_with_its_settings_folded_into_an_observed_copy() {
    use codesense_host_proto::agent_cli::{AgentCli, NativeLaunch};
    use std::os::unix::fs::PermissionsExt;
    let server = TestServer::start();
    server.host().start_activity(server.root()).unwrap();
    let mut client = TestClient::hello(&server);
    let generated = server.root().join("generated-settings.json");
    std::fs::write(
        &generated,
        r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"x --inbox-wait claude Stop"}]}]}}"#,
    )
    .unwrap();
    // Stands in for the CLI: prints the settings file the flag names.
    let claude = server.root().join("claude");
    std::fs::write(
        &claude,
        "#!/bin/sh\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = --settings ]; then printf 'FOLDED:%s:END' \"$(cat \"$2\")\"; fi\n  shift\ndone\nsleep 60\n",
    )
    .unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut intent = sh("sleep 60");
    intent.remote_launch = Some(NativeLaunch::Cli(AgentCli::Claude));
    intent.argv = Some(vec![
        claude.into_os_string().into_string().unwrap(),
        "--settings".into(),
        generated.into_os_string().into_string().unwrap(),
    ]);
    client.spawn(&server, intent, registration("saved-claude", None, false));
    let output = client.output_until("saved-claude", ":END");
    let folded = output
        .split("FOLDED:")
        .nth(1)
        .and_then(|rest| rest.split(":END").next())
        .unwrap_or_else(|| panic!("no folded settings in {output:?}"));
    assert!(folded.contains("--activity-hook claude"), "{folded}");
    assert!(folded.contains("--inbox-wait claude Stop"), "{folded}");
    client
        .call(Request::Kill {
            session: PtySessionId::new("saved-claude"),
        })
        .unwrap();
}

/// The host itself reads its account's home, so a person's new remote
/// terminal opens there without anyone guessing the path.
#[test]
fn account_home_is_the_host_accounts_absolute_home() {
    let server = TestServer::start();
    let (mut client, greeted) = TestClient::open(server.root(), hello_for(server.root()));
    let hello = greeted.expect("greeted");
    assert!(hello.supports(&codesense_host_proto::Capability::AccountHome));
    assert_eq!(client.call(Request::Listen), Ok(Reply::Done));
    let home = std::fs::canonicalize(dirs::home_dir().expect("test account home")).unwrap();
    assert!(home.is_absolute() && home.is_dir());
    assert_eq!(
        client.call(Request::AccountHome),
        Ok(Reply::Cwd { cwd: Some(home) })
    );
}

/// The caption's prompt segment is read on the host: an unknown session has
/// no cwd and so no Git segment, but still the account's home.
#[test]
fn the_host_answers_a_session_prompt_with_its_own_home() {
    let server = TestServer::start();
    let (mut client, greeted) = TestClient::open(server.root(), hello_for(server.root()));
    let hello = greeted.expect("greeted");
    assert!(hello.supports(&codesense_host_proto::Capability::SessionPrompt));
    assert_eq!(client.call(Request::Listen), Ok(Reply::Done));
    let home = std::fs::canonicalize(dirs::home_dir().expect("test account home")).unwrap();
    assert_eq!(
        client.call(Request::SessionPrompt {
            session: PtySessionId::new("no-such-session"),
        }),
        Ok(Reply::Prompt {
            prompt: Box::new(codesense_pty_core::git_prompt::SessionPrompt {
                cwd: None,
                home: Some(home),
                git: None,
            })
        })
    );
}

/// A launch can ask the host whether its agent CLI is installed for the
/// account, answered from the login PATH without starting anything.
#[test]
fn the_host_answers_whether_an_agent_cli_is_installed() {
    use codesense_host_proto::agent_cli::{AgentCli, NativeLaunch};
    let server = TestServer::start();
    let (mut client, greeted) = TestClient::open(server.root(), hello_for(server.root()));
    let hello = greeted.expect("greeted");
    assert!(hello.supports(&codesense_host_proto::Capability::NativeRuntimeProbe));
    assert!(hello.supports(&codesense_host_proto::Capability::WorkspaceHead));
    assert!(hello.supports(&codesense_host_proto::Capability::WorkspaceEditorV1));
    // A saved Agent's own variables are set on its spawn here.
    assert!(hello.supports(&codesense_host_proto::Capability::AgentEnvV1));
    // A retained session's lost exit can be asked for again.
    assert!(hello.supports(&codesense_host_proto::Capability::ExitRedeliveryV1));
    // A quitting client's reap spares another client's sessions.
    assert!(hello.supports(&codesense_host_proto::Capability::ScopedReapV1));
    assert_eq!(hello.proto_minor, codesense_host_proto::PROTO_MINOR);
    assert_eq!(client.call(Request::Listen), Ok(Reply::Done));
    let cwd = server.root().to_str().unwrap().to_string();
    assert_eq!(
        client.call(Request::ProbeNativeRuntime {
            cwd: cwd.clone(),
            launch: NativeLaunch::Shell,
        }),
        Ok(Reply::Flag { value: true })
    );
    assert!(matches!(
        client.call(Request::ProbeNativeRuntime {
            cwd,
            launch: NativeLaunch::Cli(AgentCli::Claude),
        }),
        Ok(Reply::Flag { .. })
    ));
}

/// A terminal spawns into W2 while W1 is parked inside a long
/// prepare. A PTY spawn never takes a workspace lock.
#[test]
fn a_terminal_spawns_while_another_workspace_prepares() {
    let (mut held, _, w2_worktree, _) =
        crate::remote_workspace::concurrency_tests::w1_preparing_beside_w2();
    let server = TestServer::start();
    let mut client = TestClient::hello(&server);
    let mut intent = sh("sleep 60");
    intent.cwd = w2_worktree;
    client.spawn(
        &server,
        intent,
        registration("xs2-spawn-beside-prepare", None, false),
    );
    assert!(held.still_held(), "the spawn waited for the prepare");
    assert_eq!(held.release(), "prepared");
}

/// A terminal spawns into the SAME workspace while its long batch
/// Apply is mid-write of a member.
#[test]
fn a_terminal_spawns_into_a_workspace_during_its_long_apply() {
    let (mut held, _) = crate::remote_workspace::concurrency_tests::w1_applying("member-write");
    let server = TestServer::start();
    let mut client = TestClient::hello(&server);
    let mut intent = sh("sleep 60");
    intent.cwd = held.worktree().to_string();
    client.spawn(
        &server,
        intent,
        registration("xs2-spawn-during-apply", None, false),
    );
    assert!(held.still_held(), "the spawn waited for the Apply");
    assert_eq!(held.release(), "Applying");
}

/// The host advertises the pipelined upload and the base update, and
/// serves all four requests over the wire: status, indexed chunks (acked by
/// index), one seal, and an update precheck — then the prepare they feed.
#[test]
fn xs9_the_host_serves_the_pipelined_upload_and_base_update() {
    use codesense_pty_core::remote_files::FileTarget;
    use codesense_pty_core::remote_workspace::{
        chunk_range, GitBundle, StagedBundleChunk, UpdateWorkspaceBase,
    };
    use sha2::{Digest, Sha256};
    let (temp, request) = crate::remote_workspace::tests::fixture();
    let bytes = std::fs::read(temp.path().join("input.bundle")).unwrap();
    let server = TestServer::start();
    server
        .host()
        .enable_private_workspaces_at(temp.path().to_path_buf(), 41);
    let (mut client, greeted) = TestClient::open(server.root(), hello_for(server.root()));
    let hello = greeted.expect("greeted");
    assert!(hello.supports(&codesense_host_proto::Capability::BundlePipelineV1));
    assert!(hello.supports(&codesense_host_proto::Capability::WorkspaceBaseUpdateV1));
    assert_eq!(client.call(Request::Listen), Ok(Reply::Done));

    let Ok(Reply::BundleUploadStatus { status }) = client.call(Request::BundleUploadStatus {
        request: request.clone(),
    }) else {
        panic!("expected an upload status")
    };
    assert!(!status.sealed && status.received.received() == 0);
    for index in status.received.missing() {
        let (offset, len) = chunk_range(bytes.len() as u64, index).unwrap();
        let part = bytes[offset as usize..offset as usize + len].to_vec();
        assert_eq!(
            client.call(Request::UploadBundleChunk {
                chunk: StagedBundleChunk {
                    request: request.clone(),
                    index,
                    sha256: format!("{:x}", Sha256::digest(&part)),
                    bytes: part,
                },
            }),
            Ok(Reply::BundleChunkStaged { index })
        );
    }
    assert_eq!(
        client.call(Request::SealWorkspaceBundle {
            request: request.clone()
        }),
        Ok(Reply::Done)
    );
    let Ok(Reply::RemoteWorkspacePrepared { result }) =
        client.call(Request::PrepareRemoteWorkspace {
            request: request.clone(),
        })
    else {
        panic!("expected a prepared workspace")
    };
    assert_eq!(result.status, "prepared", "{result:?}");
    let mut carrier = request.clone();
    carrier.request_id = "00000000-0000-4000-8000-0000000000d1".into();
    carrier.bundle = GitBundle {
        size: 0,
        digest: format!("{:x}", Sha256::digest([])),
        commit: request.bundle.commit.clone(),
    };
    let Ok(Reply::WorkspaceBaseUpdated { result }) = client.call(Request::UpdateWorkspaceBase {
        request: Box::new(UpdateWorkspaceBase {
            target: FileTarget {
                owner_id: 41,
                origin_key: request.origin_key.clone(),
                request_id: request.request_id.clone(),
                handle: result.handle.clone().unwrap(),
            },
            upload: carrier,
            precheck: true,
            remove_managed_copies: false,
            managed: Vec::new(),
            discard_uploads: Vec::new(),
        }),
    }) else {
        panic!("expected a base update answer")
    };
    assert_eq!(result.status, "ready", "{result:?}");
    assert_eq!(result.previous_commit, Some(request.bundle.commit));
}

// A desktop that bound its routine secret
// heartbeats; once silent past them its link is reaped in seconds (a
// half-open link must not hold its agents), while an unbound client is
// left alone.
#[test]
fn a_bound_desktop_silent_past_its_heartbeats_is_reaped() {
    let server = TestServer::start();
    server.host().set_liveness(Duration::from_millis(300));
    let mut quiet = TestClient::hello(&server);
    let mut bound = TestClient::hello(&server);
    assert_eq!(
        bound.call(Request::RoutineArmBindSecret {
            secret: "a".repeat(64),
            desktop: Some("mac-1".into()),
        }),
        Ok(Reply::Done)
    );
    let reaped = Instant::now() + Duration::from_secs(5);
    loop {
        match bound
            .frames
            .recv_timeout(reaped.saturating_duration_since(Instant::now()))
        {
            Ok(_) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("the silent bound desktop was not reaped")
            }
        }
    }
    assert!(matches!(
        quiet.frames.recv_timeout(Duration::from_millis(500)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    assert!(
        quiet.call(Request::Listen).is_ok(),
        "an unbound client stays"
    );
    // A bad identity is refused.
    let mut other = TestClient::hello(&server);
    assert!(other
        .call(Request::RoutineArmBindSecret {
            secret: "b".repeat(64),
            desktop: Some("has space".into()),
        })
        .is_err());
}

// A Mac in App Nap skips heartbeats for a
// while: only minutes of silence reap its link, never seconds; and a desktop
// that reconnects closes its own older link at once instead.
#[test]
fn a_reconnecting_desktop_closes_its_older_link_and_slow_heartbeats_are_not_reaped() {
    assert!(
        crate::host::DESKTOP_LIVENESS >= Duration::from_secs(120),
        "heartbeats delayed by App Nap (well past 20 s) must not reap a link"
    );
    let server = TestServer::start();
    let mut first = TestClient::hello(&server);
    let bind = |identity: &str, secret: char| Request::RoutineArmBindSecret {
        secret: secret.to_string().repeat(64),
        desktop: Some(identity.into()),
    };
    assert_eq!(first.call(bind("mac-a", 'a')), Ok(Reply::Done));
    // Silent for longer than the old 20 s budget's scaled stand-in: still
    // connected under the real (minutes) budget.
    assert!(matches!(
        first.frames.recv_timeout(Duration::from_millis(500)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    let mut other = TestClient::hello(&server);
    assert_eq!(other.call(bind("mac-b", 'b')), Ok(Reply::Done));
    let mut again = TestClient::hello(&server);
    assert_eq!(again.call(bind("mac-a", 'c')), Ok(Reply::Done));
    let closed = Instant::now() + Duration::from_secs(5);
    loop {
        match first
            .frames
            .recv_timeout(closed.saturating_duration_since(Instant::now()))
        {
            Ok(_) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => panic!("the older link of mac-a stayed open"),
        }
    }
    assert!(
        other.call(Request::Listen).is_ok(),
        "another desktop's link stays"
    );
    assert!(again.call(Request::Listen).is_ok());
}
