// SPDX-License-Identifier: MIT
//! Answering `ssh`'s key-passphrase prompt without the passphrase ever
//! reaching an argument list, an environment or a file.
//!
//! `ssh` asks an `SSH_ASKPASS` program for a passphrase. That program is this
//! binary: with [`SOCKET_ENV`] set it is the askpass client, which hands the
//! prompt to a Unix socket the process running `ssh` serves for the length of
//! one operation ([`Askpass::serve`]) and prints the answer. The socket lives in
//! a fresh owner-only directory, so only this user's processes can connect,
//! and both vanish when the operation ends. The environment names the socket,
//! never the secret.
//!
//! The server answers only a prompt for **its own key's** passphrase — the
//! prompt must name that key file. A password, a PIN, a host-key question, or
//! a jump host asking about anything else gets nothing.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

/// The environment variable naming the socket an askpass client reads from.
/// Set, it turns this binary into the askpass client.
pub const SOCKET_ENV: &str = "CODESENSE_ASKPASS_SOCK";
pub const GIT_MODE_ENV: &str = "CODESENSE_ASKPASS_GIT";

/// How long either side waits on the socket.
const SOCKET_TIMEOUT: Duration = Duration::from_secs(10);

/// The longest prompt the server reads.
const MAX_PROMPT: u64 = 4096;

/// A secret whose `Debug` never shows it.
struct Hidden(Vec<u8>);

impl std::fmt::Debug for Hidden {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

impl Drop for Hidden {
    fn drop(&mut self) {
        self.0.iter_mut().for_each(|byte| *byte = 0);
    }
}

/// A key's passphrase served to the askpass clients of one operation.
/// Dropping it stops serving and removes the socket.
#[derive(Debug)]
pub struct Askpass {
    program: PathBuf,
    dir: PathBuf,
    socket: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Askpass {
    /// Serve `secret`, the passphrase of the key file `key`, on a fresh socket
    /// under `parent` (created owner-only), to be asked for by `program` —
    /// this binary.
    pub fn serve(parent: &Path, program: &Path, key: &Path, secret: &str) -> std::io::Result<Self> {
        Self::serve_prompt(
            parent,
            program,
            key.as_os_str().as_bytes().to_vec(),
            secret,
            false,
        )
    }

    /// Serve only Git's password prompt for this exact credential-free HTTPS URL.
    /// The caller fixes credential.username and credential.useHttpPath; redirects
    /// asking for another origin/path receive no secret.
    pub fn serve_git(
        parent: &Path,
        program: &Path,
        url: &str,
        username: &str,
        secret: &str,
    ) -> std::io::Result<Self> {
        let remote = url
            .strip_prefix("https://")
            .filter(|value| !value.contains(['@', '\n', '\r', '\0', '\'']))
            .ok_or_else(|| std::io::Error::other("invalid Git origin"))?;
        if username.is_empty()
            || !username
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
        {
            return Err(std::io::Error::other("invalid Git username"));
        }
        let prompt = format!("Password for 'https://{username}@{remote}': ");
        Self::serve_prompt(parent, program, prompt.into_bytes(), secret, true)
    }

    fn serve_prompt(
        parent: &Path,
        program: &Path,
        key: Vec<u8>,
        secret: &str,
        exact: bool,
    ) -> std::io::Result<Self> {
        std::fs::create_dir_all(parent)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        let dir = parent.join(nonce());
        std::fs::create_dir(&dir)?;
        let bound = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .and_then(|()| UnixListener::bind(dir.join("s")))
            .and_then(|listener| {
                std::fs::set_permissions(dir.join("s"), std::fs::Permissions::from_mode(0o600))
                    .map(|()| listener)
            });
        let listener = match bound {
            Ok(listener) => listener,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(e);
            }
        };
        let stop = Arc::new(AtomicBool::new(false));
        let secret = Arc::new(Hidden(secret.as_bytes().to_vec()));
        let thread = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(stream) = stream {
                        let key = key.clone();
                        let secret = secret.clone();
                        // A local process can connect and never finish its
                        // prompt. Serve clients independently so it cannot
                        // starve ssh's real askpass request.
                        let stopped = stop.clone();
                        std::thread::spawn(move || {
                            answer_one(stream, &key, &secret, exact, &stopped)
                        });
                    }
                }
            })
        };
        Ok(Self {
            program: program.to_path_buf(),
            socket: dir.join("s"),
            dir,
            stop,
            thread: Some(thread),
        })
    }

    /// The environment that makes `ssh` ask this server: the askpass program,
    /// forced even without a display, and the socket. Never the secret.
    pub fn env(&self) -> Vec<(String, OsString)> {
        vec![
            ("SSH_ASKPASS".into(), self.program.clone().into_os_string()),
            ("SSH_ASKPASS_REQUIRE".into(), "force".into()),
            (SOCKET_ENV.into(), self.socket.clone().into_os_string()),
        ]
    }
}

impl Drop for Askpass {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop so it sees the stop; if nothing can wake it,
        // leave it parked rather than hang here.
        let woken = UnixStream::connect(&self.socket).is_ok();
        if let Some(thread) = self.thread.take() {
            if woken {
                let _ = thread.join();
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Serve one client: read its prompt, and answer it only when it asks for
/// the passphrase of `key`.
fn answer_one(
    mut stream: UnixStream,
    key: &[u8],
    secret: &Hidden,
    exact: bool,
    stopped: &AtomicBool,
) {
    let _ = stream.set_read_timeout(Some(SOCKET_TIMEOUT));
    let _ = stream.set_write_timeout(Some(SOCKET_TIMEOUT));
    // Bytes, not text: `ssh` may cut a non-ASCII path mid-character.
    let mut prompt = Vec::new();
    if (&mut stream)
        .take(MAX_PROMPT)
        .read_to_end(&mut prompt)
        .is_err()
    {
        return;
    }
    let matches = if exact {
        prompt == key
    } else {
        asks_for_key(&prompt, key)
    };
    if matches && !stopped.load(Ordering::SeqCst) {
        let _ = stream.write_all(&secret.0);
    }
}

/// Whether `prompt` is `ssh`'s or `ssh-keygen`'s own question for the
/// passphrase of `key` — never a server's keyboard-interactive text, which
/// `ssh` shows prefixed with `(user@host)`. `ssh` quotes at most the first 100
/// bytes of the path; an `ssh-keygen` too old to name the file asks a bare
/// "Enter passphrase:".
pub fn asks_for_key(prompt: &[u8], key: &[u8]) -> bool {
    let named = prompt
        .strip_prefix(b"Enter passphrase for key '".as_slice())
        .and_then(|rest| rest.strip_suffix(b"': ".as_slice()))
        .or_else(|| {
            prompt
                .strip_prefix(b"Enter passphrase for \"".as_slice())
                .and_then(|rest| rest.strip_suffix(b"\": ".as_slice()))
        });
    match named {
        Some(path) => {
            path == key || (key.len() > 100 && path.len() == 100 && key.starts_with(path))
        }
        // A bare prompt cannot be tied to the configured key and could be a
        // different key or remote keyboard-interactive question.
        None => false,
    }
}

/// Whether `prompt` asks for a key's passphrase — the only question the
/// client passes on.
pub fn is_passphrase_prompt(prompt: &str) -> bool {
    prompt.to_ascii_lowercase().contains("passphrase")
}

/// The askpass client: pass `prompt` to the server on `socket` and write its
/// answer to `out`, followed by a newline.
pub fn answer(prompt: &[u8], socket: &Path, out: &mut impl Write) -> Result<(), String> {
    let text = String::from_utf8_lossy(prompt);
    if !is_passphrase_prompt(&text) {
        return Err(format!("not a key passphrase prompt: {text:?}"));
    }
    answer_prompt(prompt, socket, out)
}
fn answer_prompt(prompt: &[u8], socket: &Path, out: &mut impl Write) -> Result<(), String> {
    let mut stream = UnixStream::connect(socket).map_err(|e| format!("connect {socket:?}: {e}"))?;
    // macOS refuses a timeout (EINVAL) on a socket the server already closed;
    // the read then returns at once anyway.
    let _ = stream.set_read_timeout(Some(SOCKET_TIMEOUT));
    stream
        .write_all(prompt)
        .and_then(|()| stream.shutdown(Shutdown::Write))
        .map_err(|e| format!("ask {socket:?}: {e}"))?;
    let mut secret = Hidden(Vec::new());
    stream
        .read_to_end(&mut secret.0)
        .map_err(|e| format!("read {socket:?}: {e}"))?;
    if secret.0.is_empty() {
        return Err("no passphrase for this prompt".into());
    }
    out.write_all(&secret.0)
        .and_then(|()| out.write_all(b"\n"))
        .and_then(|()| out.flush())
        .map_err(|e| format!("answer: {e}"))
}

fn answer_with_mode(
    prompt: &[u8],
    socket: &Path,
    out: &mut impl Write,
    git: bool,
) -> Result<(), String> {
    if git && prompt.starts_with(b"Password for 'https://") {
        answer_prompt(prompt, socket, out)
    } else {
        answer(prompt, socket, out)
    }
}

/// The askpass client as a process: `args` are its arguments (the prompt
/// first). Returns the exit code.
pub fn run_client(args: &[OsString], socket: &Path) -> i32 {
    let prompt = args.first().map(|a| a.as_bytes()).unwrap_or_default();
    let git = std::env::var_os(GIT_MODE_ENV).is_some_and(|value| value == "1");
    let mut stdout = std::io::stdout().lock();
    let result = answer_with_mode(prompt, socket, &mut stdout, git);
    match result {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("codesense-session-server askpass: {err}");
            1
        }
    }
}

/// Whether `passphrase` unlocks the private key at `key`, asked through the
/// askpass (`program`, serving from a socket under `dir`) exactly as sign-in
/// asks it. A key that needs no passphrase is refused: there is nothing to
/// keep.
pub fn check_passphrase(
    key: &Path,
    passphrase: &str,
    program: &Path,
    dir: &Path,
) -> Result<(), String> {
    use std::process::{Command, Stdio};
    if !key.is_file() {
        return Err(format!("There is no key file at {}", key.display()));
    }
    let unlocked = |envs: Vec<(String, OsString)>, args: &[&str]| {
        let mut command = Command::new("ssh-keygen");
        command.args(args).arg(key).envs(envs).stdin(Stdio::null());
        crate::install::run_within(&mut command, Duration::from_secs(15))
            .map(|o| {
                (
                    o.status.success(),
                    String::from_utf8_lossy(&o.stderr).trim().to_string(),
                )
            })
            .map_err(|e| format!("run ssh-keygen: {e}"))
    };
    if unlocked(Vec::new(), &["-y", "-P", "", "-f"])?.0 {
        return Err(format!(
            "{} has no passphrase. Choose \"Key file\" instead.",
            key.display()
        ));
    }
    let askpass = Askpass::serve(dir, program, key, passphrase)
        .map_err(|e| format!("serve the passphrase: {e}"))?;
    match unlocked(askpass.env(), &["-y", "-f"])? {
        (true, _) => Ok(()),
        (false, said) => Err(format!(
            "That passphrase doesn't unlock {} ({said})",
            key.display()
        )),
    }
}

/// A name no other served socket's directory has.
fn nonce() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("{:x}-{nanos:x}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_socket_answers_only_exact_origin_in_explicit_client_mode() {
        let temp = tempfile::tempdir().unwrap();
        let server = Askpass::serve_git(
            temp.path(),
            Path::new("/unused"),
            "https://github.com/owner/repo",
            "x-access-token",
            "local-only-token",
        )
        .unwrap();
        let prompt = b"Password for 'https://x-access-token@github.com/owner/repo': ";
        let mut out = Vec::new();
        assert!(answer_with_mode(prompt, &server.socket, &mut out, false).is_err());
        answer_with_mode(prompt, &server.socket, &mut out, true).unwrap();
        assert_eq!(out, b"local-only-token\n");
        assert!(answer_with_mode(
            b"Password for 'https://x-access-token@other.example/owner/repo': ",
            &server.socket,
            &mut Vec::new(),
            true
        )
        .is_err());
        assert!(answer_with_mode(
            b"Password for 'https://x-access-token@github.com/other/repo': ",
            &server.socket,
            &mut Vec::new(),
            true
        )
        .is_err());
    }
    #[test]
    fn stopped_git_broker_never_answers_pending_prompt() {
        let (server, mut client) = UnixStream::pair().unwrap();
        let stopped = AtomicBool::new(true);
        let secret = Hidden(b"never-send".to_vec());
        client.write_all(b"expected").unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        answer_one(server, b"expected", &secret, true, &stopped);
        let mut out = Vec::new();
        client.read_to_end(&mut out).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn only_a_passphrase_prompt_is_answered() {
        assert!(is_passphrase_prompt("Enter passphrase for key '/k': "));
        assert!(is_passphrase_prompt("Enter passphrase: "));
        assert!(!is_passphrase_prompt("dev@box's password: "));
        assert!(!is_passphrase_prompt("Enter PIN for ED25519-SK key /k: "));
        assert!(!is_passphrase_prompt(
            "Are you sure you want to continue connecting (yes/no/[fingerprint])? "
        ));
    }

    #[test]
    fn only_the_tools_own_prompt_for_the_key_is_answered() {
        let asks = |prompt: &str, key: &str| asks_for_key(prompt.as_bytes(), key.as_bytes());
        let key = "/k/id";
        assert!(asks("Enter passphrase for key '/k/id': ", key));
        assert!(asks("Enter passphrase for \"/k/id\": ", key));
        assert!(!asks("Enter passphrase: ", key));
        assert!(!asks("(u@jump) Enter passphrase for key '/k/id': ", key));
        assert!(!asks("Enter passphrase for key '/k/other': ", key));
        let long = format!("/{}", "d/".repeat(60));
        let shown = format!("Enter passphrase for key '{}': ", &long[..100]);
        assert!(asks(&shown, &long), "ssh cuts the path at 100 bytes");
        assert!(!asks(
            &format!("Enter passphrase for key '{}': ", &long[..99]),
            &long
        ));
        // A cut through a multi-byte character is still the key's prompt.
        let accented = format!("/{}é/id", "a".repeat(98));
        let mut cut = b"Enter passphrase for key '".to_vec();
        cut.extend_from_slice(&accented.as_bytes()[..100]);
        cut.extend_from_slice(b"': ");
        assert!(asks_for_key(&cut, accented.as_bytes()));
    }

    #[test]
    fn a_served_secret_answers_only_its_own_keys_prompt_and_is_gone_after() {
        let parent = tempfile::tempdir().unwrap();
        let key = Path::new("/keys/id_work");
        let askpass =
            Askpass::serve(parent.path(), Path::new("/bin/false"), key, "s3cret").unwrap();
        let env = askpass.env();
        assert!(env
            .iter()
            .all(|(_, value)| !value.to_string_lossy().contains("s3cret")));
        let socket = PathBuf::from(&env[2].1);
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(socket.parent().unwrap()), 0o700);
        assert_eq!(mode(&socket), 0o600);

        for _ in 0..6 {
            let mut out = Vec::new();
            answer(
                b"Enter passphrase for key '/keys/id_work': ",
                &socket,
                &mut out,
            )
            .unwrap();
            assert_eq!(out, b"s3cret\n", "every prompt for the key is answered");
        }
        for other in [
            "Enter passphrase for key '/keys/id_other': ",
            "(dev@jump) passphrase: ",
            "(dev@jump) Enter passphrase for key '/keys/id_work': ",
            "Enter passphrase for key '/keys/id_work': and more",
            "dev@box's password: ",
        ] {
            let mut refused = Vec::new();
            assert!(
                answer(other.as_bytes(), &socket, &mut refused).is_err(),
                "{other}"
            );
            assert!(refused.is_empty(), "{other} gets nothing");
        }

        drop(askpass);
        assert!(!socket.exists(), "the socket is gone with the operation");
        assert!(!socket.parent().unwrap().exists());
    }
}
