// SPDX-License-Identifier: MIT
//! `codesense-session-server attach --stdio`: reach this machine's host from
//! anywhere that can run a command on it — over `ssh <host> … attach --stdio`,
//! with nothing but the user's own SSH.
//!
//! The relay is a raw byte pipe between its stdin/stdout and the host's Unix
//! socket. It reads, writes and reframes nothing: the client and the host speak
//! `codesense-host-proto` frames end to end, exactly as over the local socket.
//! When no host serves the instance, the relay launches one (this same binary,
//! which binds, then detaches into its own session), so the host outlives the
//! SSH session while the relay ends with it.
//!
//! A byte pipe cannot resume a framed connection, so the relay never
//! reconnects mid-stream: when either side closes, it exits, and the client
//! reconnects by running a new relay, which greets the same host afresh.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use codesense_pty_core::protocol::socket_path;

/// The relay's usage line.
pub const USAGE: &str =
    "usage: codesense-session-server attach --stdio [--root <app-data root>] [--resources <dir>]";

/// The first wait between connection attempts to a host that is starting.
const FIRST_BACKOFF: Duration = Duration::from_millis(50);

/// The longest wait between two connection attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(1);

/// How long a launched host may take to answer on its socket.
const CONNECT_DEADLINE: Duration = Duration::from_secs(10);

/// The name of the directory, next to the binary, holding the resources an
/// installed host ships (see [`crate::install`]).
pub const RESOURCES_DIR: &str = "resources";

/// Where the relay reaches its host: the instance root and the resources a
/// host it launches ships.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub root: PathBuf,
    pub resources: Option<PathBuf>,
}

impl Target {
    /// The target named by the relay's arguments (after `attach`). The root
    /// defaults to `<home>/.codesense`, the app's own CodeSense home; the
    /// resources to `resources/` next to `binary`, when that exists. An
    /// explicit `--root` or `--resources` wins.
    pub fn from_args(args: &[String], home: Option<&Path>, binary: &Path) -> Result<Self, String> {
        let (mut stdio, mut root, mut resources) = (false, None, None);
        let mut rest = args.iter();
        while let Some(arg) = rest.next() {
            let slot = match arg.as_str() {
                "--stdio" => {
                    stdio = true;
                    continue;
                }
                "--root" => &mut root,
                "--resources" => &mut resources,
                other => return Err(format!("unknown argument {other:?}\n{USAGE}")),
            };
            if slot.is_some() {
                return Err(format!("{arg} given twice\n{USAGE}"));
            }
            let path = rest
                .next()
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .ok_or_else(|| format!("{arg} needs an absolute path\n{USAGE}"))?;
            *slot = Some(path);
        }
        if !stdio {
            return Err(format!("attach needs a transport\n{USAGE}"));
        }
        let root = match root {
            Some(root) => root,
            None => home
                .filter(|home| home.is_absolute())
                .map(|home| home.join(".codesense"))
                .ok_or_else(|| "no --root, and no absolute home to default it".to_string())?,
        };
        let resources = resources.or_else(|| {
            binary
                .parent()
                .map(|dir| dir.join(RESOURCES_DIR))
                .filter(|dir| dir.is_dir())
        });
        Ok(Self { root, resources })
    }
}

/// Run the relay: connect to the host of `target` (launching `binary` as that
/// host when none serves it), then pipe this process's stdin and stdout to it
/// until either side closes.
pub fn run(target: &Target, binary: &Path) -> Result<(), String> {
    let socket = connect_or_launch(target, binary)?;
    relay_socket(socket)
}

fn relay_socket(socket: UnixStream) -> Result<(), String> {
    let stdin = duplicate(std::io::stdin().as_fd())?;
    let stdout = duplicate(std::io::stdout().as_fd())?;
    pipe(socket, stdin, stdout).map_err(|e| format!("relay: {e}"))
}

/// Private launch uses the installed binary's restricted daemon entry, which
/// repeats the account interlock and owns a fresh descriptor before binding.
#[cfg(target_os = "linux")]
pub(crate) fn run_private(
    target: &Target,
    binary: &Path,
    owner: &crate::machine_owner::BootstrapOwner,
    expected: &str,
    version: &str,
    logical_root: &Path,
) -> Result<(), String> {
    relay_socket(connect_private(
        target,
        binary,
        owner,
        expected,
        version,
        logical_root,
    )?)
}

#[cfg(target_os = "linux")]
pub(crate) fn connect_private(
    target: &Target,
    binary: &Path,
    owner: &crate::machine_owner::BootstrapOwner,
    expected: &str,
    version: &str,
    logical_root: &Path,
) -> Result<UnixStream, String> {
    let socket = socket_path(&target.root);
    let stream = match UnixStream::connect(&socket) {
        Ok(stream) => stream,
        Err(_) => {
            let args = vec![
                "private-serve".into(),
                owner.to_string(),
                expected.into(),
                version.into(),
                "--root".into(),
                logical_root.to_string_lossy().into_owned(),
            ];
            launch_with_args(binary, target, Some(&args))?;
            connect_with_backoff(&socket, CONNECT_DEADLINE)?
        }
    };
    Ok(stream)
}

/// An unbuffered handle on a standard stream, so every byte crosses as soon as
/// it arrives.
fn duplicate(fd: std::os::fd::BorrowedFd<'_>) -> Result<File, String> {
    fd.try_clone_to_owned()
        .map(File::from)
        .map_err(|e| format!("stdio: {e}"))
}

/// Connect to the host serving `target.root`, launching `binary` as that host
/// first when nothing answers.
pub fn connect_or_launch(target: &Target, binary: &Path) -> Result<UnixStream, String> {
    let socket = socket_path(&target.root);
    if let Ok(stream) = UnixStream::connect(&socket) {
        return Ok(stream);
    }
    // A launch that fails may have lost the race to another relay's host:
    // that host still answers.
    let launched = launch(binary, target);
    connect_with_backoff(&socket, CONNECT_DEADLINE).map_err(|err| match launched {
        Ok(()) => err,
        Err(launch) => launch,
    })
}

/// Connect to `socket`, retrying with a doubling backoff until `deadline`
/// passes: a host that holds the instance's lock may still be binding.
fn connect_with_backoff(socket: &Path, deadline: Duration) -> Result<UnixStream, String> {
    let give_up = Instant::now() + deadline;
    let mut delay = FIRST_BACKOFF;
    loop {
        match UnixStream::connect(socket) {
            Ok(stream) => return Ok(stream),
            Err(err) if Instant::now() >= give_up => {
                return Err(format!("no host answers on {socket:?}: {err}"))
            }
            Err(_) => {
                thread::sleep(delay);
                delay = (delay * 2).min(MAX_BACKOFF);
            }
        }
    }
}

/// Launch `binary` as the host of `target`. It binds the instance's socket and
/// detaches before the launched process exits, so the host listens once this
/// returns — or another host already serves the instance, and it exits at
/// once. The host never holds the relay's stdio (a held stdout would keep the
/// SSH session open); its stderr goes to `<root>/logs/pty-server.log`.
fn launch(binary: &Path, target: &Target) -> Result<(), String> {
    launch_with_args(binary, target, None)
}

fn launch_with_args(
    binary: &Path,
    target: &Target,
    private_args: Option<&[String]>,
) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt;
    let logs = target.root.join("logs");
    // Owner-only from the start: the host would tighten the root to 0700, but
    // only once it runs.
    let stderr = std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&logs)
        .and_then(|()| {
            // Cut before the host starts writing: the host appends to this
            // file for its whole life, so a cut can only happen here.
            let server_log = logs.join(crate::disconnect_log::SERVER_LOG_NAME);
            if let Err(err) = crate::disconnect_log::cap_in_place(&server_log) {
                eprintln!("codesense-session-server: could not cap {server_log:?}: {err}");
            }
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(server_log)
        })
        .map_or_else(|_| Stdio::null(), Stdio::from);
    let mut command = Command::new(binary);
    if let Some(args) = private_args {
        command.args(args);
    } else {
        if let Some(resources) = &target.resources {
            command.arg("--resources").arg(resources);
        }
        command.arg("--root").arg(&target.root);
    }
    // The host outlives this SSH connection and its login session, so nothing
    // tied to them may reach the sessions it spawns (a dead agent socket, the
    // connection's address, the session's id).
    for (name, _) in std::env::vars_os() {
        let name_text = name.to_string_lossy();
        if name_text.starts_with("SSH_") || name_text.starts_with("XDG_SESSION_") {
            command.env_remove(name);
        }
    }
    // `--root` last, as the app launches its sidecar: a host is found by its
    // args' suffix.
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .status()
        .map_err(|e| format!("launch {binary:?}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{binary:?} exited with {status}"))
    }
}

/// Pipe `input` to `socket` and `socket` to `output`, byte for byte, until the
/// host closes the connection or `output` fails. When `input` ends the
/// socket's write half is shut, so the host sees the client leave and closes
/// its side.
pub fn pipe(
    socket: UnixStream,
    mut input: impl Read + Send + 'static,
    mut output: impl Write,
) -> std::io::Result<()> {
    let mut upstream = socket.try_clone()?;
    thread::Builder::new()
        .name("attach-upstream".into())
        .spawn(move || {
            let _ = forward(&mut input, &mut upstream);
            let _ = upstream.shutdown(std::net::Shutdown::Write);
        })?;
    let mut downstream = socket;
    forward(&mut downstream, &mut output)
}

/// Copy `from` to `to` until `from` ends, writing whatever each read returns at
/// once. Not `std::io::copy`: on Linux it splices between a pipe and a socket,
/// which can hold bytes back until the source ends — a stalled handshake.
fn forward(from: &mut impl Read, to: &mut impl Write) -> std::io::Result<()> {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) => return to.flush(),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        to.write_all(&buf[..n])?;
        to.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| arg.to_string()).collect()
    }

    /// With no `--root` the relay uses `<home>/.codesense`; an explicit root
    /// wins.
    #[test]
    fn the_root_defaults_to_the_codesense_home() {
        let home = Path::new("/home/dev");
        let binary = Path::new("/nonexistent/bin/codesense-session-server");
        let target = Target::from_args(&args(&["--stdio"]), Some(home), binary).unwrap();
        assert_eq!(target.root, Path::new("/home/dev/.codesense"));

        let target =
            Target::from_args(&args(&["--stdio", "--root", "/srv/x"]), Some(home), binary).unwrap();
        assert_eq!(target.root, Path::new("/srv/x"));

        assert!(Target::from_args(&args(&["--stdio"]), None, binary).is_err());
        assert!(
            Target::from_args(&args(&["--stdio"]), Some(Path::new("rel")), binary).is_err(),
            "a relative home is no root"
        );
    }

    /// An installed host's resources sit next to its binary; an explicit
    /// `--resources` wins, and a binary without them ships none.
    #[test]
    fn the_resources_default_to_those_next_to_the_binary() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("codesense-session-server");
        let home = Some(Path::new("/home/dev"));
        let none = Target::from_args(&args(&["--stdio"]), home, &binary).unwrap();
        assert_eq!(none.resources, None);

        std::fs::create_dir(dir.path().join(RESOURCES_DIR)).unwrap();
        let shipped = Target::from_args(&args(&["--stdio"]), home, &binary).unwrap();
        assert_eq!(shipped.resources, Some(dir.path().join(RESOURCES_DIR)));

        let explicit =
            Target::from_args(&args(&["--stdio", "--resources", "/r"]), home, &binary).unwrap();
        assert_eq!(explicit.resources, Some(PathBuf::from("/r")));
    }

    #[test]
    fn attach_needs_stdio_and_known_arguments() {
        let binary = Path::new("/bin/x");
        let home = Some(Path::new("/h"));
        assert!(Target::from_args(&args(&[]), home, binary).is_err());
        assert!(Target::from_args(&args(&["--stdio", "--tcp"]), home, binary).is_err());
        assert!(Target::from_args(&args(&["--stdio", "--root"]), home, binary).is_err());
        assert!(Target::from_args(
            &args(&["--stdio", "--root", "/a", "--root", "/b"]),
            home,
            binary
        )
        .is_err());
        for bad in ["--stdio", "relative/root", ""] {
            assert!(
                Target::from_args(&args(&["--root", bad, "--stdio"]), home, binary).is_err(),
                "{bad:?} is no root"
            );
        }
    }

    /// Every byte crosses unchanged both ways — newlines, NULs and all 256
    /// values, well past any buffer size — and the relay ends once the host
    /// closes after its client's input ended.
    #[test]
    fn the_relay_is_a_transparent_byte_pipe() {
        let dir = tempfile::Builder::new()
            .prefix("rm2")
            .tempdir_in("/tmp")
            .unwrap();
        let path = dir.path().join("echo.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let echo = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut writer = stream.try_clone().unwrap();
            std::io::copy(&mut stream, &mut writer).unwrap();
        });

        let sent: Vec<u8> = (0..3 * 1024 * 1024u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        let mut received = Vec::new();
        let socket = UnixStream::connect(&path).unwrap();
        pipe(socket, std::io::Cursor::new(sent.clone()), &mut received).unwrap();
        echo.join().unwrap();
        assert_eq!(received.len(), sent.len());
        assert!(received == sent, "the relay changed the bytes");
    }

    /// Bytes cross while both ends stay open — a handshake cannot wait for
    /// either side to finish. Pipes on both sides, as under `ssh`.
    #[test]
    fn the_relay_forwards_before_either_side_ends() {
        let dir = tempfile::Builder::new()
            .prefix("rm2")
            .tempdir_in("/tmp")
            .unwrap();
        let path = dir.path().join("echo.sock");
        let listener = UnixListener::bind(&path).unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut writer = stream.try_clone().unwrap();
            let _ = std::io::copy(&mut stream, &mut writer);
        });

        let (relay_in, mut client_in) = std::io::pipe().unwrap();
        let (client_out, relay_out) = std::io::pipe().unwrap();
        let socket = UnixStream::connect(&path).unwrap();
        thread::spawn(move || pipe(socket, relay_in, relay_out));

        let (echoed, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let mut client_out = client_out;
            let mut byte = [0u8; 1];
            while client_out.read_exact(&mut byte).is_ok() {
                if echoed.send(byte[0]).is_err() {
                    break;
                }
            }
        });
        for round in 0..3u8 {
            client_in.write_all(&[round, b'\n', 0]).unwrap();
            for expected in [round, b'\n', 0] {
                let got = rx
                    .recv_timeout(Duration::from_secs(5))
                    .expect("echoed while both ends are open");
                assert_eq!(got, expected);
            }
        }
    }

    /// When the host closes first the relay ends, though its client's input is
    /// still open: the client learns to reconnect.
    #[test]
    fn the_relay_ends_when_the_host_closes_first() {
        let dir = tempfile::Builder::new()
            .prefix("rm2")
            .tempdir_in("/tmp")
            .unwrap();
        let path = dir.path().join("closing.sock");
        let listener = UnixListener::bind(&path).unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(b"bye").unwrap();
        });

        let (relay_in, _client_in) = std::io::pipe().unwrap();
        let socket = UnixStream::connect(&path).unwrap();
        let (done, ended) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let mut received = Vec::new();
            let result = pipe(socket, relay_in, &mut received).map(|()| received);
            let _ = done.send(result);
        });
        let received = ended
            .recv_timeout(Duration::from_secs(5))
            .expect("the relay ends while its input is open")
            .unwrap();
        assert_eq!(received, b"bye");
    }

    /// A host that is still binding is reached once it listens.
    #[test]
    fn connecting_waits_for_a_starting_host() {
        let dir = tempfile::Builder::new()
            .prefix("rm2")
            .tempdir_in("/tmp")
            .unwrap();
        let path = dir.path().join("late.sock");
        let late = path.clone();
        let binder = thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            let listener = UnixListener::bind(&late).unwrap();
            listener.accept().unwrap();
        });
        connect_with_backoff(&path, Duration::from_secs(5)).expect("reached");
        binder.join().unwrap();
        assert!(connect_with_backoff(&dir.path().join("never.sock"), Duration::ZERO).is_err());
    }
}
