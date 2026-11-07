// SPDX-License-Identifier: MIT
use super::*;
use crate::agent_cli::{orion::Orion, AgentCliAdapter};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener};
use std::os::unix::net::{UnixListener, UnixStream};

fn strings(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

#[test]
fn a_cli_on_an_engine_is_spawned_through_the_helper_with_both_of_its_forms() {
    let arguments = strings(&["--no-daemon", "-c", "hooks.Stop=[]", "--", "hi"]);
    let argv = launch_argv(
        "/host/activity-helper".as_ref(),
        AgentCli::Codex,
        "/bin/codex".as_ref(),
        &arguments,
    );
    assert_eq!(argv[0], "/host/activity-helper");
    let launch = launch_of(&argv).unwrap();
    let endpoint = format!("unix://{CHANNEL_ENDPOINT}");
    assert_eq!(
        launch,
        EngineLaunch {
            provider: AgentCli::Codex,
            serves_at: false,
            end_on_loss: false,
            program: "/bin/codex".into(),
            engine: ["-c", "hooks.Stop=[]", "app-server", "--listen", &endpoint]
                .map(String::from)
                .to_vec(),
            attached: ["-c", "hooks.Stop=[]", "--remote", &endpoint]
                .map(String::from)
                .to_vec(),
            standalone: ["--no-daemon", "-c", "hooks.Stop=[]"]
                .map(String::from)
                .to_vec(),
            operands: ["--", "hi"].map(String::from).to_vec(),
        }
    );
}

/// What Codex's TUI refuses, or cannot resolve, beside `--remote` (checked against Codex
/// 0.162.1): an extra root, a writable-roots override, and an MCP server it names only in
/// part (an inherited project server switched off is `{enabled=false}`, with no transport).
const REFUSED_BESIDE_REMOTE: &[&str] = &[
    "--add-dir",
    "sandbox_workspace_write.writable_roots",
    "mcp_servers",
];

#[test]
fn a_team_workspace_codex_launch_attaches_its_tui_with_nothing_it_refuses() {
    // A scoped team-workspace launch: its servers switched off and its relay added, then
    // the team content root.
    let arguments = strings(&[
        "--no-daemon",
        "-c",
        "mcp_servers={\"codesense\"={enabled=false},\"codesense_scope_1\"={command=\"/bridge\"}}",
        "-c",
        "hooks.Stop=[]",
        "--add-dir",
        "/Users/me/team's content",
        "--",
        "list the team content",
    ]);
    let argv = launch_argv(
        "/host/activity-helper".as_ref(),
        AgentCli::Codex,
        "/bin/codex".as_ref(),
        &arguments,
    );
    let launch = launch_of(&argv).unwrap();
    for option in &launch.attached {
        assert!(
            !REFUSED_BESIDE_REMOTE
                .iter()
                .any(|refused| option.contains(refused)),
            "{option}"
        );
    }
    assert!(launch
        .engine
        .iter()
        .any(|option| option
            == "sandbox_workspace_write.writable_roots=[\"/Users/me/team's content\"]"));
    assert!(launch
        .engine
        .iter()
        .any(|option| option.starts_with("mcp_servers=")));
    // Should its engine not come up, it runs on its own with its extra root as given.
    let standalone: Vec<OsString> = launch.standalone.iter().map(OsString::from).collect();
    assert_eq!(standalone, arguments[..7]);
}

#[test]
fn a_cli_on_no_engine_or_a_run_that_takes_none_is_spawned_as_given() {
    let as_given = |cli: AgentCli, arguments: Vec<OsString>| {
        let argv = launch_argv(
            "/host/activity-helper".as_ref(),
            cli,
            "/bin/cli".as_ref(),
            &arguments,
        );
        assert_eq!(argv[0], "/bin/cli");
        assert_eq!(argv[1..], arguments[..]);
    };
    // Claude runs on no engine; a one-shot Codex has no TUI to attach.
    as_given(AgentCli::Claude, strings(&["--model", "m", "--", "hi"]));
    as_given(AgentCli::Codex, strings(&["exec", "go"]));
    // An argv that is not UTF-8 can't be handed to the helper.
    use std::os::unix::ffi::OsStringExt;
    as_given(
        AgentCli::Codex,
        vec![OsString::from_vec(vec![0xff]), OsString::from("hi")],
    );
}

#[test]
fn an_engine_serves_at_its_socket_or_a_link_to_it_and_both_go_when_it_ends() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&real).unwrap();
    // Codex on Linux names its endpoint by a link to a socket of its own.
    let linked = dir.path().join("engine-linked.sock");
    std::os::unix::fs::symlink(&real, &linked).unwrap();
    assert!(is_socket(&real));
    assert!(is_socket(&linked));
    // A link to nothing, or to a file that is no socket, is no engine.
    let plain = dir.path().join("plain");
    std::fs::write(&plain, b"").unwrap();
    let to_plain = dir.path().join("engine-plain.sock");
    std::os::unix::fs::symlink(&plain, &to_plain).unwrap();
    assert!(!is_socket(&to_plain));
    assert!(!is_socket(&dir.path().join("engine-missing.sock")));
    remove_socket(&linked);
    assert!(std::fs::symlink_metadata(&linked).is_err());
    remove_socket(&real);
    assert!(!real.exists());
    // Nothing else is removed.
    remove_socket(&plain);
    assert!(plain.exists());
}

fn publication_rig(kind: &str) -> (tempfile::TempDir, PathBuf, Publication) {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let endpoint = dir.path().join("engine-test.sock");
    let source = Orion
        .channel(&[format!("--serves-at={kind}")])
        .unwrap()
        .serves_at;
    assert!(source.is_some());
    let publication = publish_endpoint(&endpoint, Some(dir.path()), source).unwrap();
    (dir, endpoint, publication)
}

fn peer(endpoint: &Path) -> UnixStream {
    let stream = UnixStream::connect(endpoint).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
}

#[test]
fn serves_at_is_preserved_in_the_helper_launch_without_serializing_a_resolver() {
    for kind in ["unix", "tcp"] {
        let launch = engine_launch(
            &Orion,
            crate::agent_cli::orion::ORION,
            "/bin/orion".as_ref(),
            &strings(&[&format!("--serves-at={kind}"), "--", "hello"]),
        )
        .unwrap();
        assert!(launch.serves_at);
        assert_eq!(launch.operands, ["--", "hello"]);
        let raw = serde_json::to_string(&launch).unwrap();
        assert_eq!(serde_json::from_str::<EngineLaunch>(&raw).unwrap(), launch);
    }
}

#[test]
fn serves_at_unix_links_a_later_socket_and_cleanup_keeps_the_target() {
    let (dir, endpoint, publication) = publication_rig("unix");
    assert!(!is_socket(&endpoint));
    let target = dir.path().join("orion.sock");
    let listener = UnixListener::bind(&target).unwrap();
    assert!(is_socket(&endpoint));
    let mut client = peer(&endpoint);
    let (mut server, _) = listener.accept().unwrap();
    client.write_all(b"ping").unwrap();
    let mut bytes = [0; 4];
    server.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"ping");
    drop(publication);
    assert!(std::fs::symlink_metadata(endpoint).is_err());
    assert!(is_socket(&target));
}

#[test]
fn serves_at_tcp_resolves_each_connection_and_copies_both_half_closed_directions() {
    let (dir, endpoint, publication) = publication_rig("tcp");
    // Discovery does not exist yet: refuse this connection, retaining the listener.
    let mut rejected = peer(&endpoint);
    assert!(matches!(rejected.read(&mut [0]), Ok(0)));
    for response in [b"first".as_slice(), b"second".as_slice()] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        std::fs::write(
            dir.path().join("orion.addr"),
            listener.local_addr().unwrap().to_string(),
        )
        .unwrap();
        let response = response.to_vec();
        let expected = response.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            stream.read_to_end(&mut request).unwrap();
            assert_eq!(request, b"ping");
            stream.write_all(&response).unwrap();
            stream.shutdown(Shutdown::Write).unwrap();
        });
        let mut client = peer(&endpoint);
        client.write_all(b"ping").unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let mut answer = Vec::new();
        client.read_to_end(&mut answer).unwrap();
        assert_eq!(answer, expected);
        server.join().unwrap();
    }
    drop(publication);
    assert!(!endpoint.exists());
}

#[test]
fn serves_at_tcp_preserves_websocket_upgrade_path_subprotocol_and_frames() {
    use tungstenite::client::IntoClientRequest;
    let (dir, endpoint, _publication) = publication_rig("tcp");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    std::fs::write(
        dir.path().join("orion.addr"),
        listener.local_addr().unwrap().to_string(),
    )
    .unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut socket = tungstenite::accept_hdr(
            stream,
            |request: &tungstenite::handshake::server::Request,
             mut response: tungstenite::handshake::server::Response| {
                assert_eq!(request.uri().path(), "/api/ws");
                assert_eq!(request.headers()["Sec-WebSocket-Protocol"], "orion-v1");
                response
                    .headers_mut()
                    .insert("Sec-WebSocket-Protocol", "orion-v1".parse().unwrap());
                Ok(response)
            },
        )
        .unwrap();
        let message = socket.read().unwrap();
        assert_eq!(message.to_text().unwrap(), "opaque rpc");
        socket.send(message).unwrap();
    });
    let mut request = "ws://localhost/api/ws".into_client_request().unwrap();
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", "orion-v1".parse().unwrap());
    let (mut client, response) = tungstenite::client(request, peer(&endpoint)).unwrap();
    assert_eq!(response.headers()["Sec-WebSocket-Protocol"], "orion-v1");
    client
        .send(tungstenite::Message::Text("opaque rpc".into()))
        .unwrap();
    assert_eq!(client.read().unwrap().to_text().unwrap(), "opaque rpc");
    server.join().unwrap();
}

#[test]
fn stopping_a_tcp_publication_closes_active_connections_and_removes_only_its_socket() {
    let (dir, endpoint, publication) = publication_rig("tcp");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    std::fs::write(
        dir.path().join("orion.addr"),
        listener.local_addr().unwrap().to_string(),
    )
    .unwrap();
    let mut client = peer(&endpoint);
    let (mut server, _) = listener.accept().unwrap();
    server
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    drop(publication);
    assert_eq!(client.read(&mut [0]).unwrap(), 0);
    assert_eq!(server.read(&mut [0]).unwrap(), 0);
    assert!(!endpoint.exists());
    assert!(dir.path().join("orion.addr").exists());
}

// A child test process runs the real keeper; only adapter lookup is substituted, as it
// is for an ActivityOwner running Orion. Nothing is installed in the user's environment.
#[test]
fn keeper_process_fixture() {
    let Ok(raw) = std::env::var("CODESENSE_TEST_KEEPER_LAUNCH") else {
        return;
    };
    let endpoint = std::env::var_os("CODESENSE_TEST_KEEPER_ENDPOINT").unwrap();
    std::process::exit(keep_engine_with(
        &[endpoint, raw.into()],
        crate::agent_cli::orion::adapters,
    ));
}

#[test]
fn the_real_keeper_publishes_unix_and_tcp_and_reaps_its_engine_when_stopped() {
    struct Keeper(std::process::Child);
    impl Drop for Keeper {
        fn drop(&mut self) {
            stop(&mut self.0, KEEPER_STOP);
        }
    }
    const ENGINE: &str = r#"
import os, pathlib, signal, socket, sys
home, kind = pathlib.Path(sys.argv[1]), sys.argv[2]
s = socket.socket(socket.AF_UNIX if kind == 'unix' else socket.AF_INET)
s.bind(str(home / 'orion.sock') if kind == 'unix' else ('127.0.0.1', 0))
s.listen()
if kind == 'tcp': (home / 'orion.addr').write_text('127.0.0.1:' + str(s.getsockname()[1]))
(home / 'engine.pid').write_text(str(os.getpid()))
c, _ = s.accept()
c.sendall(c.recv(1024))
c.close()
signal.pause()
"#;
    for kind in ["unix", "tcp"] {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let endpoint = dir.path().join("engine-keeper.sock");
        let launch = EngineLaunch {
            provider: crate::agent_cli::orion::ORION,
            serves_at: true,
            end_on_loss: false,
            program: "/usr/bin/python3".into(),
            engine: vec![
                "-c".into(),
                ENGINE.into(),
                dir.path().to_str().unwrap().into(),
                kind.into(),
            ],
            standalone: vec![format!("--serves-at={kind}")],
            attached: vec![],
            operands: vec![],
        };
        let mut keeper = Keeper(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "activity::engine::tests::keeper_process_fixture",
                    "--nocapture",
                ])
                .env(
                    "CODESENSE_TEST_KEEPER_LAUNCH",
                    serde_json::to_string(&launch).unwrap(),
                )
                .env("CODESENSE_TEST_KEEPER_ENDPOINT", &endpoint)
                .env("ORION_HOME", dir.path())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while !dir.path().join("engine.pid").exists() || !is_socket(&endpoint) {
            assert!(
                keeper.0.try_wait().unwrap().is_none(),
                "keeper exited before publishing"
            );
            assert!(Instant::now() < deadline, "engine never published");
            std::thread::sleep(Duration::from_millis(10));
        }
        let pid: u32 = std::fs::read_to_string(dir.path().join("engine.pid"))
            .unwrap()
            .parse()
            .unwrap();
        let mut client = peer(&endpoint);
        client.write_all(b"roundtrip").unwrap();
        let mut answer = [0; 9];
        client.read_exact(&mut answer).unwrap();
        assert_eq!(&answer, b"roundtrip");
        stop(&mut keeper.0, KEEPER_STOP);
        assert!(!endpoint.exists());
        assert!(
            super::super::process::inspect(pid).is_none(),
            "engine outlived keeper"
        );
    }
}
