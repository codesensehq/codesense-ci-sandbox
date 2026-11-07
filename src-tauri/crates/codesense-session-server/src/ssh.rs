// SPDX-License-Identifier: MIT
//! Reaching a machine with the system `ssh`: the user's own config, agent,
//! jump hosts and hardware keys apply unchanged, and nothing here speaks SSH
//! itself.
//!
//! **Host keys are pinned per machine.** Every connection names its machine
//! with `HostKeyAlias=codesense-<machine>`, checks it with
//! `StrictHostKeyChecking=yes`, and reads the pinned key from that machine's
//! own known-hosts file ([`Connector::pin`]) — and only from there: neither the
//! user's `~/.ssh/known_hosts` nor the system's (with their `@cert-authority`
//! lines) can admit another key. A key is only ever pinned after a person trusted its fingerprint, or because the
//! team's declaration pinned it: [`Connector::scan`] fetches a machine's key
//! into a throwaway file to show its fingerprint and never trusts it. A key
//! that differs from the pin fails the connection — there is no automatic
//! trust-on-first-use and no disabled host-key check on an authenticating
//! connection. A host the user's own SSH config reaches through a
//! `ProxyCommand` is scanned by `ssh` itself, offering no credential, into
//! the same kind of throwaway file.
//!
//! No forward is ever opened — `ClearAllForwardings` and `ForwardAgent=no`
//! override whatever the user's config says: everything a client needs rides
//! the `attach --stdio` relay's own stdio.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use base64::Engine;
use sha2::{Digest, Sha256};

use crate::askpass::Askpass;
use crate::install::{run_within, shell_quote, Ssh};

/// How long `ssh` may take to establish a connection.
const CONNECT_TIMEOUT_SECS: u32 = 10;

/// How long a shared master connection outlives its last client.
const CONTROL_PERSIST_SECS: u32 = 60;

/// A shared master connection's socket, named by `ssh`'s hash of the
/// connection (`%C`: 40 hex digits).
const CONTROL_NAME: &str = "%C";

/// The longest master socket path every platform binds: `%C` expands to 40
/// hex digits, `ssh` first binds it with a 17-byte random suffix, and macOS
/// allows 103 bytes.
const MAX_SOCKET_PATH: usize = 103 - 40 - 17 + CONTROL_NAME.len();

/// How long a scan may take, beyond `ssh`'s own connect timeout.
const SCAN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How long `ssh -G` may take to resolve one config alias (a `Match exec`
/// runs a command).
const RESOLVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The most `~/.ssh/config` aliases resolved for the Add-machine form.
const MAX_CONFIG_HOSTS: usize = 64;

/// A value that reaches `ssh` as an argument: never an option, never two
/// words.
fn check_word(what: &str, value: &str) -> Result<(), String> {
    let ok = !value.is_empty()
        && !value.starts_with('-')
        && value
            .chars()
            .all(|c| !c.is_whitespace() && !c.is_control() && c != '"' && c != '\'');
    if ok {
        Ok(())
    } else {
        Err(format!("{what} {value:?} is not a valid SSH {what}"))
    }
}

/// Where a machine is: its host and port, the user to sign in as (the
/// user's SSH config decides when absent), and a jump host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub host: String,
    pub port: u16,
    pub user: Option<String>,
    pub jump: Option<String>,
}

impl Address {
    /// A checked address: no part of it can read as an `ssh` option.
    pub fn new(
        host: &str,
        port: u16,
        user: Option<&str>,
        jump: Option<&str>,
    ) -> Result<Self, String> {
        check_word("host", host)?;
        if host.contains('@') {
            return Err(format!("host {host:?} is not a valid SSH host"));
        }
        if port == 0 {
            return Err("port 0 is not a valid SSH port".into());
        }
        if let Some(user) = user {
            check_word("user", user)?;
            if user.contains('@') || user.contains(':') {
                return Err(format!("user {user:?} is not a valid SSH user"));
            }
        }
        if let Some(jump) = jump {
            check_word("jump host", jump)?;
        }
        Ok(Self {
            host: host.to_string(),
            port,
            user: user.map(String::from),
            jump: jump.map(String::from),
        })
    }
}

/// How the user signs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignIn {
    /// ssh-agent, or whatever identity the user's SSH config names.
    Agent,
    /// This key file, which needs no passphrase (or the agent holds it).
    Key(PathBuf),
    /// This key file, whose passphrase an [`Askpass`] serves.
    KeyWithPassphrase(PathBuf),
}

impl SignIn {
    fn identity(&self) -> Option<&Path> {
        match self {
            Self::Agent => None,
            Self::Key(path) | Self::KeyWithPassphrase(path) => Some(path),
        }
    }
}

/// Whether a connection may share a master connection with others to the
/// same machine (`ControlMaster`), or must authenticate on its own — a check
/// of the sign-in itself must never ride a master another key opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mux {
    Shared,
    Fresh,
}

/// A machine's public host key: its type and base64 blob, as in a
/// `known_hosts` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKey {
    key_type: String,
    blob: String,
}

impl HostKey {
    /// Read `"<type> <base64>"`, a public key as a known-hosts line carries it.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut words = text.split_whitespace();
        let (Some(key_type), Some(blob)) = (words.next(), words.next()) else {
            return Err(format!("{text:?} is not a host key"));
        };
        let type_ok = key_type
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@.-_".contains(&b));
        if !type_ok || STANDARD.decode(blob).is_err() {
            return Err(format!("{text:?} is not a host key"));
        }
        Ok(Self {
            key_type: key_type.to_string(),
            blob: blob.to_string(),
        })
    }

    /// `"<type> <base64>"`.
    pub fn to_line(&self) -> String {
        format!("{} {}", self.key_type, self.blob)
    }

    /// The key's SHA256 fingerprint, as `ssh-keygen -l` prints it.
    pub fn fingerprint(&self) -> String {
        let raw = STANDARD.decode(&self.blob).unwrap_or_default();
        format!("SHA256:{}", STANDARD_NO_PAD.encode(Sha256::digest(raw)))
    }
}

/// Why a connection failed, read from what `ssh` said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The host name does not resolve.
    Unresolved(String),
    /// The host (or the jump host) did not answer.
    Unreachable(String),
    /// The machine presented a key other than the pinned one.
    HostKeyChanged,
    /// The machine refused every key offered.
    AuthFailed(String),
    /// Anything else, with what `ssh` said.
    Other(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unresolved(e) | Self::Unreachable(e) | Self::AuthFailed(e) | Self::Other(e) => {
                f.write_str(e)
            }
            Self::HostKeyChanged => f.write_str("the machine's host key changed"),
        }
    }
}

/// Read a failed `ssh`'s standard error.
pub fn classify(stderr: &str) -> Failure {
    let text = stderr.trim();
    let line = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(text)
        .trim()
        .to_string();
    let has = |needle: &str| text.contains(needle);
    // A failed verification is a changed key only when it is about a pinned
    // machine (named by its `codesense-` alias): a jump host's unknown key
    // fails the same way, and is not the machine's.
    let verification =
        has("REMOTE HOST IDENTIFICATION HAS CHANGED") || has("Host key verification failed");
    if verification && has("codesense-") {
        Failure::HostKeyChanged
    } else if verification {
        Failure::Other(format!(
            "{line} (the jump host's key could not be verified: check it with ssh to the jump host)"
        ))
    } else if has("Could not resolve hostname")
        || has("Name or service not known")
        || has("nodename nor servname provided")
        || has("getaddrinfo")
    {
        Failure::Unresolved(if line.to_ascii_lowercase().contains("resolve") {
            line
        } else {
            format!("Could not resolve hostname: {line}")
        })
    } else if has("Connection refused")
        || has("timed out")
        || has("No route to host")
        || has("Network is unreachable")
        || has("Connection reset")
        || has("kex_exchange_identification")
        || has("Connection closed by UNKNOWN")
    {
        Failure::Unreachable(line)
    } else if has("Permission denied")
        || has("Too many authentication failures")
        || has("incorrect passphrase")
        || has("Load key")
    {
        Failure::AuthFailed(line)
    } else {
        Failure::Other(if line.is_empty() {
            "ssh failed without saying why".into()
        } else {
            line
        })
    }
}

/// The files the connector keeps: each machine's pinned key and shared
/// master sockets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub known_hosts_dir: PathBuf,
    pub control_dir: PathBuf,
}

impl Paths {
    /// The connector's files under a CodeSense home (`~/.codesense`).
    pub fn under(codesense_home: &Path) -> Self {
        let ssh = codesense_home.join("ssh");
        Self {
            known_hosts_dir: ssh.join("known_hosts.d"),
            control_dir: ssh.join("cm"),
        }
    }

    /// `machine`'s pin: the known-hosts file holding the one key its
    /// connections accept.
    pub fn pin_file(&self, machine: &str) -> PathBuf {
        self.known_hosts_dir.join(alias(machine))
    }

    /// Pin `key` for `machine`.
    pub fn pin(&self, machine: &str, key: &HostKey) -> Result<(), String> {
        write_pin(&self.known_hosts_dir, &alias(machine), key)
    }
}

/// Write `alias`'s known-hosts file in `dir`: `key`, and nothing else.
fn write_pin(dir: &Path, alias: &str, key: &HostKey) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    static PIN_FILES: std::sync::Mutex<()> = std::sync::Mutex::new(());
    static STAGE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let _held = PIN_FILES.lock().unwrap_or_else(|e| e.into_inner());
    std::fs::create_dir_all(dir).map_err(|e| format!("create {dir:?}: {e}"))?;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    let target = dir.join(alias);
    let staged = dir.join(format!(
        ".{alias}.incoming-{}-{}",
        std::process::id(),
        STAGE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let result = std::fs::write(&staged, format!("{alias} {}\n", key.to_line()))
        .and_then(|()| std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600)))
        .and_then(|()| std::fs::rename(&staged, &target))
        .map_err(|e| format!("pin the host key in {target:?}: {e}"));
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result
}

/// The `HostKeyAlias` a machine's connections check its key under.
fn alias(machine: &str) -> String {
    format!("codesense-{machine}")
}

/// One machine as `ssh` reaches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connector {
    address: Address,
    sign_in: SignIn,
    alias: String,
    paths: Paths,
    program: PathBuf,
    scan_program: PathBuf,
}

impl Connector {
    /// The connector for `machine` (a stable id: its pin and master socket
    /// are named for it) at `address`.
    pub fn new(
        machine: &str,
        address: Address,
        sign_in: SignIn,
        paths: Paths,
    ) -> Result<Self, String> {
        let safe = !machine.is_empty()
            && machine
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            && !machine.starts_with('.');
        if !safe {
            return Err(format!("{machine:?} is not a machine id"));
        }
        if let Some(key) = sign_in.identity() {
            // One argument of its own: spaces are fine, an option is not.
            let Some(text) = key.to_str() else {
                return Err("the key-file path is not valid UTF-8".into());
            };
            if text.is_empty() || text.starts_with('-') || text.chars().any(char::is_control) {
                return Err(format!("{text:?} is not a key file"));
            }
        }
        Ok(Self {
            address,
            sign_in,
            alias: alias(machine),
            paths,
            program: PathBuf::from("ssh"),
            scan_program: PathBuf::from("ssh-keyscan"),
        })
    }

    /// Use `program` as `ssh`.
    pub fn with_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = program.into();
        self
    }

    /// Use `program` for unauthenticated host-key discovery. Primarily useful
    /// for hermetic tests; production always uses the system `ssh-keyscan`.
    pub fn with_scan_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.scan_program = program.into();
        self
    }

    pub fn address(&self) -> &Address {
        &self.address
    }

    pub fn sign_in(&self) -> &SignIn {
        &self.sign_in
    }

    /// This machine's own known-hosts file, holding its pinned key.
    pub fn known_hosts(&self) -> PathBuf {
        self.paths.known_hosts_dir.join(&self.alias)
    }

    /// This connector with its pin kept under `known_hosts_dir` instead: a
    /// check that must not change the machine's real pin.
    pub fn pinning_in(mut self, known_hosts_dir: PathBuf) -> Self {
        self.paths.known_hosts_dir = known_hosts_dir;
        self
    }

    /// Pin `key` for this machine: the only key its connections accept.
    pub fn pin(&self, key: &HostKey) -> Result<(), String> {
        write_pin(&self.paths.known_hosts_dir, &self.alias, key)
    }

    /// The options every connection carries: the address, the pin's alias
    /// and the timeouts.
    fn common(&self) -> Vec<String> {
        let mut args = vec![
            "-p".into(),
            self.address.port.to_string(),
            "-o".into(),
            format!("ConnectTimeout={CONNECT_TIMEOUT_SECS}"),
            "-o".into(),
            "ServerAliveInterval=15".into(),
            "-o".into(),
            "ServerAliveCountMax=3".into(),
            "-o".into(),
            format!("HostKeyAlias={}", self.alias),
            "-o".into(),
            "UpdateHostKeys=no".into(),
            "-o".into(),
            "ClearAllForwardings=yes".into(),
            "-o".into(),
            "ForwardAgent=no".into(),
            "-o".into(),
            "ForwardX11=no".into(),
            "-o".into(),
            "ForwardX11Trusted=no".into(),
            "-o".into(),
            "VerifyHostKeyDNS=no".into(),
        ];
        // Options that would let the user's config admit another key, turned
        // off where this `ssh` has them: a host reached through the user's
        // ProxyCommand is held to its pin like any other.
        for option in [
            "KnownHostsCommand=none",
            "NoHostAuthenticationForProxyCommand=no",
        ] {
            if knows_option(&self.program, option) {
                args.extend(["-o".into(), option.into()]);
            }
        }
        if let Some(user) = &self.address.user {
            args.extend(["-l".into(), user.clone()]);
        }
        if let Some(jump) = &self.address.jump {
            args.extend(["-J".into(), jump.clone()]);
        }
        args
    }

    /// `path` as one word of an `ssh -o` value.
    fn option_path(path: &Path) -> String {
        // ssh parses -o values with its config tokenizer even though argv has
        // already preserved the word. Quote and escape the characters that
        // tokenizer treats specially; Connector::new rejects non-UTF-8 key
        // paths and CodeSense-owned paths use the same encoding here.
        let text = path.to_string_lossy();
        format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
    }

    /// The `ssh` that connects to this machine, checking its pinned key and
    /// signing in as bound. A passphrase-protected key needs the [`Askpass`]
    /// serving its passphrase.
    pub fn ssh(&self, mux: Mux, askpass: Option<&Askpass>) -> Ssh {
        let mut args = self.common();
        args.extend([
            "-o".into(),
            "StrictHostKeyChecking=yes".into(),
            "-o".into(),
            format!(
                "UserKnownHostsFile={}",
                Self::option_path(&self.known_hosts())
            ),
            "-o".into(),
            "GlobalKnownHostsFile=/dev/null".into(),
            "-o".into(),
            "PasswordAuthentication=no".into(),
            "-o".into(),
            "KbdInteractiveAuthentication=no".into(),
        ]);
        // A master socket's path must fit a Unix socket address; where it
        // cannot, connections simply don't share one.
        // `%C` hashes only ssh's connection tuple. Prefix it with the stable
        // CodeSense machine alias so two records for the same address cannot
        // inherit each other's authenticated master.
        let control = self
            .paths
            .control_dir
            .join(format!("{}-{CONTROL_NAME}", self.alias));
        let mux = match mux {
            Mux::Shared if control.as_os_str().len() > MAX_SOCKET_PATH => Mux::Fresh,
            mux => mux,
        };
        match mux {
            Mux::Shared => args.extend([
                "-o".into(),
                "ControlMaster=auto".into(),
                "-o".into(),
                format!("ControlPath={}", Self::option_path(&control)),
                "-o".into(),
                format!("ControlPersist={CONTROL_PERSIST_SECS}"),
            ]),
            Mux::Fresh => args.extend([
                "-o".into(),
                "ControlMaster=no".into(),
                "-o".into(),
                "ControlPath=none".into(),
            ]),
        }
        if let Some(identity) = self.sign_in.identity() {
            args.extend([
                "-i".into(),
                identity.to_string_lossy().into_owned(),
                "-o".into(),
                "IdentitiesOnly=yes".into(),
            ]);
        }
        let env = match (&self.sign_in, askpass) {
            (SignIn::KeyWithPassphrase(_), Some(askpass)) => {
                args.extend([
                    "-o".into(),
                    "BatchMode=no".into(),
                    "-o".into(),
                    "NumberOfPasswordPrompts=1".into(),
                ]);
                askpass.env()
            }
            _ => {
                args.extend(["-o".into(), "BatchMode=yes".into()]);
                Vec::new()
            }
        };
        args.extend(["--".into(), self.address.host.clone()]);
        Ssh::new(&self.program, args).with_env(env)
    }

    /// Fetch the key the machine presents, for a person to check before
    /// trusting it — of `like`'s type, when checking it against a pin (a
    /// machine holds a key of each type, and only one is pinned). It lands in
    /// a throwaway file under `scratch` and is never pinned; the connection
    /// offers no credential at all.
    ///
    /// The host is first resolved as `ssh` would (`ssh -G`), so an alias's
    /// `HostName` and `Port` apply, and a host the user's config reaches
    /// through a `ProxyCommand` is scanned by `ssh` itself.
    pub fn scan(&self, scratch: &Path, like: Option<&HostKey>) -> Result<HostKey, Failure> {
        let dir = tempfile_dir(scratch).map_err(Failure::Other)?;
        // A jump host overrides any proxy the config names, and keyscan runs
        // on the jump host, which resolves the name itself.
        let resolved = match self.address.jump {
            Some(_) => None,
            None => self.resolve(),
        };
        match resolved {
            Some(resolved) if resolved.proxy_command.is_some() => {
                self.scan_through_ssh(&dir.path, like)
            }
            Some(resolved) => self.keyscan(&resolved.host_name, resolved.port, like),
            None => self.keyscan(&self.address.host, self.address.port, like),
        }
    }

    /// The host as the user's SSH config resolves it; none when `ssh -G`
    /// can't say.
    fn resolve(&self) -> Option<ConfigHost> {
        let mut args = vec!["-G".to_string(), "-p".into(), self.address.port.to_string()];
        if let Some(user) = &self.address.user {
            args.extend(["-l".into(), user.clone()]);
        }
        args.extend(["--".into(), self.address.host.clone()]);
        let output = run_within(
            Command::new(&self.program).args(&args).stdin(Stdio::null()),
            RESOLVE_TIMEOUT,
        )
        .ok()
        .filter(|o| o.status.success())?;
        parse_resolved(&self.address.host, &String::from_utf8_lossy(&output.stdout))
    }

    /// `ssh-keyscan` of `host`, directly or on the jump host.
    fn keyscan(&self, host: &str, port: u16, like: Option<&HostKey>) -> Result<HostKey, Failure> {
        let args = vec![
            "-T".into(),
            CONNECT_TIMEOUT_SECS.to_string(),
            "-p".into(),
            port.to_string(),
            host.to_string(),
        ];
        let mut command = if let Some(jump) = &self.address.jump {
            // ssh-keyscan has no ProxyJump support. Run it on the already
            // configured jump host: the target connection remains
            // unauthenticated while discovery sees the key from the same
            // network path subsequent -J connections use.
            let mut command = Command::new(&self.program);
            let remote_command = keyscan_remote_command(&args);
            command.args([
                "-o",
                "BatchMode=yes",
                "-o",
                "ClearAllForwardings=yes",
                "--",
                jump,
                &remote_command,
            ]);
            command
        } else {
            let mut command = Command::new(&self.scan_program);
            command.args(&args);
            command
        };
        let output =
            run_within(command.stdin(Stdio::null()), SCAN_TIMEOUT).map_err(Failure::Unreachable)?;
        pick_key(
            known_host_keys(&String::from_utf8_lossy(&output.stdout)),
            like,
        )
        .ok_or_else(|| {
            let failure = classify(&String::from_utf8_lossy(&output.stderr));
            unkeyed_failure(like.is_some(), failure)
        })
    }

    /// Fetch the key through `ssh` itself, for a host the user's config
    /// reaches through a `ProxyCommand`: `ssh-keyscan` can't run one. `ssh`
    /// records the key it was shown in a throwaway known-hosts file in `dir`
    /// and then fails to sign in, because every way to sign in is off.
    fn scan_through_ssh(&self, dir: &Path, like: Option<&HostKey>) -> Result<HostKey, Failure> {
        let known_hosts = dir.join("known_hosts");
        let mut args = self.common();
        args.extend(
            [
                "StrictHostKeyChecking=accept-new".to_string(),
                format!("UserKnownHostsFile={}", Self::option_path(&known_hosts)),
                "GlobalKnownHostsFile=/dev/null".into(),
                "HashKnownHosts=no".into(),
                "CheckHostIP=no".into(),
                "ControlMaster=no".into(),
                "ControlPath=none".into(),
                "BatchMode=yes".into(),
                "PubkeyAuthentication=no".into(),
                "PasswordAuthentication=no".into(),
                "KbdInteractiveAuthentication=no".into(),
                "GSSAPIAuthentication=no".into(),
                "HostbasedAuthentication=no".into(),
                "IdentityAgent=none".into(),
                "PKCS11Provider=none".into(),
                "PermitLocalCommand=no".into(),
                "RemoteCommand=none".into(),
                "RequestTTY=no".into(),
            ]
            .into_iter()
            .flat_map(|option| ["-o".to_string(), option]),
        );
        if let Some(pinned) = like {
            args.extend([
                "-o".into(),
                format!("HostKeyAlgorithms={}", host_key_algorithms(pinned)),
            ]);
        }
        // `true`, not `-N`: a server that lets anyone in ends the session at
        // once instead of holding it open until the timeout.
        args.extend(["--".into(), self.address.host.clone(), "true".into()]);
        let ran = run_within(
            Command::new(&self.program).args(&args).stdin(Stdio::null()),
            SCAN_TIMEOUT,
        );
        // The key is recorded before sign-in fails, so read it whatever the
        // exit (even a timeout).
        let recorded = std::fs::read_to_string(&known_hosts).unwrap_or_default();
        if let Some(key) = pick_key(known_host_keys(&recorded), like) {
            return Ok(key);
        }
        let host = &self.address.host;
        let failure = match ran {
            Err(_) => Failure::Unreachable(format!(
                "ssh got no host key from {host} within {} s through its ProxyCommand. Check \
                 that ssh {host} works in a terminal.",
                SCAN_TIMEOUT.as_secs()
            )),
            Ok(output) if output.stderr.iter().all(u8::is_ascii_whitespace) => {
                Failure::Other(format!(
                    "ssh got no host key from {host} through its ProxyCommand and gave no \
                     reason. Check that ssh {host} works in a terminal."
                ))
            }
            Ok(output) => classify(&String::from_utf8_lossy(&output.stderr)),
        };
        Err(unkeyed_failure(like.is_some(), failure))
    }
}

/// The host keys in known-hosts lines (`<host> <type> <base64>`), as
/// `ssh-keyscan` prints them and `ssh` records them.
fn known_host_keys(text: &str) -> Vec<HostKey> {
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let _host = words.next()?;
            HostKey::parse(&format!("{} {}", words.next()?, words.next()?)).ok()
        })
        .collect()
}

/// The key to show or compare: of the pinned key's type when there is a pin,
/// else the modern key the interactive ssh negotiation selects on our
/// supported hosts (deterministic first sight).
fn pick_key(keys: Vec<HostKey>, like: Option<&HostKey>) -> Option<HostKey> {
    like.and_then(|pinned| {
        keys.iter()
            .find(|key| key.key_type == pinned.key_type)
            .cloned()
    })
    .or_else(|| {
        ["ssh-ed25519", "ecdsa-sha2-nistp256", "ssh-rsa"]
            .into_iter()
            .find_map(|kind| keys.iter().find(|key| key.key_type == kind).cloned())
    })
    .or_else(|| keys.into_iter().next())
}

/// The `HostKeyAlgorithms` that make `ssh` ask for a key of `pinned`'s type:
/// an RSA key is signed with the SHA-2 algorithms.
fn host_key_algorithms(pinned: &HostKey) -> String {
    match pinned.key_type.as_str() {
        "ssh-rsa" => "rsa-sha2-512,rsa-sha2-256,ssh-rsa".into(),
        other => other.to_string(),
    }
}

/// Why a scan that received no host key failed. With a pin and no key received,
/// an unexplained failure (`Failure::Other`) is reported as unreachable, never
/// as a changed key. A jump host's unverified key keeps its own message, since
/// the fix is to check the jump host, not the network path to the machine.
/// Other variants pass through as `classify` set them; `HostKeyChanged` comes
/// only from `ssh`'s own verification output naming a changed key.
fn unkeyed_failure(pinned: bool, failure: Failure) -> Failure {
    match (pinned, failure) {
        (true, Failure::Other(message)) if !message.contains("jump host") => {
            Failure::Unreachable(format!(
                "could not reach the machine to check its key, so nothing was compared \
             (check the network path and the SSH port; ssh said: {message})"
            ))
        }
        (_, other) => other,
    }
}

/// One shell-safe command for a jump host. OpenSSH joins remote-command argv
/// into shell input, so passing individually checked words is not sufficient:
/// metacharacters such as `$`, `;`, and backticks remain active. Quote every
/// word using the same POSIX encoder as the installer.
fn keyscan_remote_command(args: &[String]) -> String {
    std::iter::once("ssh-keyscan".to_string())
        .chain(args.iter().map(|arg| shell_quote(arg)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether the local `ssh` knows `option`, which must then be set so the
/// user's config can't admit another key: `KnownHostsCommand` (OpenSSH 8.5+),
/// and `NoHostAuthenticationForProxyCommand`, which not every `ssh` build has.
/// An `ssh` that refuses an option has no such setting to turn off.
fn knows_option(program: &Path, option: &'static str) -> bool {
    type Cache = std::collections::HashMap<(PathBuf, &'static str), bool>;
    static KNOWS: std::sync::OnceLock<std::sync::Mutex<Cache>> = std::sync::OnceLock::new();
    let cache = KNOWS.get_or_init(Default::default);
    let key = (program.to_path_buf(), option);
    if let Some(knows) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return *knows;
    }
    let probed = run_within(
        Command::new(program)
            .args(["-G", "-o", option, "codesense-probe"])
            .stdin(Stdio::null()),
        RESOLVE_TIMEOUT,
    );
    match probed {
        // Only `ssh` itself naming the option unknown turns it off; a probe
        // that could not run keeps it on, and is asked again next time.
        Ok(output) => {
            let said = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
            let unknown = !output.status.success()
                && (said.contains("bad configuration option")
                    || said.contains("unsupported option"));
            let knows = !unknown;
            cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, knows);
            knows
        }
        Err(_) => true,
    }
}

/// A directory removed when dropped.
struct TempDir {
    path: PathBuf,
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn tempfile_dir(parent: &Path) -> Result<TempDir, String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(parent).map_err(|e| format!("create {parent:?}: {e}"))?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let path = parent.join(format!("scan-{:x}-{nanos:x}", std::process::id()));
    std::fs::create_dir(&path).map_err(|e| format!("create {path:?}: {e}"))?;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700));
    Ok(TempDir { path })
}

/// A `Host` of the user's `~/.ssh/config`, resolved as `ssh` would.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigHost {
    pub alias: String,
    pub host_name: String,
    pub user: Option<String>,
    pub port: u16,
    /// The alias's `ProxyJump`, as ssh resolved it.
    pub proxy_jump: Option<String>,
    /// The first `IdentityFile` the alias names beyond ssh's defaults.
    pub identity_file: Option<String>,
    /// The alias's `ProxyCommand`, which `ssh` runs to reach it.
    pub proxy_command: Option<String>,
}

/// The identity files `ssh -G` lists for every host when nothing names one.
const DEFAULT_IDENTITY_FILES: &[&str] = &[
    "id_rsa",
    "id_ecdsa",
    "id_ecdsa_sk",
    "id_ed25519",
    "id_ed25519_sk",
    "id_xmss",
    "id_dsa",
];

fn is_default_identity(path: &str) -> bool {
    let path = path.trim_matches('"');
    DEFAULT_IDENTITY_FILES.iter().any(|name| {
        path.strip_suffix(name)
            .is_some_and(|dir| dir.ends_with("/.ssh/") || dir == "~/.ssh/")
    })
}

/// The concrete aliases a config's `Host` lines name (patterns and negations
/// are not machines), in order, once each.
pub fn config_aliases(config: &str) -> Vec<String> {
    let mut aliases: Vec<String> = Vec::new();
    for line in config.lines() {
        let line = line.trim();
        let mut words = line.split_whitespace();
        let Some(keyword) = words.next() else {
            continue;
        };
        let keyword = keyword.split('=').next().unwrap_or_default();
        if !keyword.eq_ignore_ascii_case("host") {
            continue;
        }
        let rest = line[keyword.len()..].trim_start_matches(['=', ' ', '\t']);
        let rest = rest.split('#').next().unwrap_or_default();
        for alias in rest.split_whitespace() {
            let alias = alias.trim_matches('"');
            let concrete = !alias.is_empty()
                && !alias.contains(['*', '?', '!'])
                && check_word("host", alias).is_ok();
            if concrete && !aliases.iter().any(|a| a == alias) {
                aliases.push(alias.to_string());
            }
        }
    }
    aliases
}

/// Read `ssh -G <alias>`: the host name, user and port it resolves to.
pub fn parse_resolved(alias: &str, output: &str) -> Option<ConfigHost> {
    let (mut host_name, mut user, mut port) = (None, None, 22u16);
    let (mut proxy_jump, mut identity_file, mut proxy_command) = (None, None, None);
    let set = |value: &str| {
        let value = value.trim();
        (!value.is_empty() && !value.eq_ignore_ascii_case("none")).then(|| value.to_string())
    };
    for line in output.lines() {
        match line.split_once(' ') {
            Some(("hostname", value)) => host_name = Some(value.trim().to_string()),
            Some(("user", value)) => user = Some(value.trim().to_string()),
            Some(("port", value)) => port = value.trim().parse().unwrap_or(22),
            Some(("proxyjump", value)) => proxy_jump = set(value),
            Some(("proxycommand", value)) => proxy_command = set(value),
            Some(("identityfile", value))
                if identity_file.is_none() && !is_default_identity(value.trim()) =>
            {
                identity_file = set(value);
            }
            _ => {}
        }
    }
    Some(ConfigHost {
        alias: alias.to_string(),
        host_name: host_name?,
        user,
        port,
        proxy_jump,
        identity_file,
        proxy_command,
    })
}

/// The machines the user's `~/.ssh/config` (under `home`) names, each
/// resolved by `ssh -G` so `Match`, `Include` and defaults apply.
pub fn config_hosts(home: &Path) -> Vec<ConfigHost> {
    let config = std::fs::read_to_string(home.join(".ssh").join("config")).unwrap_or_default();
    config_aliases(&config)
        .into_iter()
        .take(MAX_CONFIG_HOSTS)
        .filter_map(|alias| {
            let output = run_within(
                Command::new("ssh")
                    .args(["-G", "--", &alias])
                    .stdin(Stdio::null()),
                RESOLVE_TIMEOUT,
            )
            .ok()
            .filter(|o| o.status.success())?;
            parse_resolved(&alias, &String::from_utf8_lossy(&output.stdout))
        })
        .collect()
}

/// A key the user's ssh-agent holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentKey {
    pub key_type: String,
    pub fingerprint: String,
    pub comment: String,
    pub public_key: String,
}

/// Read `ssh-add -L`: one public key per line.
pub fn parse_agent_keys(output: &str) -> Vec<AgentKey> {
    output
        .lines()
        .filter_map(|line| {
            let key = HostKey::parse(line).ok()?;
            let comment = line
                .split_whitespace()
                .skip(2)
                .collect::<Vec<_>>()
                .join(" ");
            let key_type = key_label(&key.key_type);
            Some(AgentKey {
                key_type,
                fingerprint: key.fingerprint(),
                comment,
                public_key: line.trim().to_string(),
            })
        })
        .collect()
}

/// A key type as a person reads it: `ssh-ed25519` → `ed25519`,
/// `sk-ssh-ed25519@openssh.com` → `ed25519-sk`, `ecdsa-sha2-nistp256` → `ecdsa`.
fn key_label(key_type: &str) -> String {
    let (base, hardware) = match key_type.strip_prefix("sk-") {
        Some(rest) => (rest, true),
        None => (key_type, false),
    };
    let base = base.split('@').next().unwrap_or(base);
    let name = match base.strip_prefix("ssh-") {
        Some(name) => name,
        None if base.starts_with("ecdsa") => "ecdsa",
        None => base,
    };
    if hardware {
        format!("{name}-sk")
    } else {
        name.to_string()
    }
}

/// The keys the user's ssh-agent holds; none when no agent answers.
pub fn agent_keys() -> Vec<AgentKey> {
    Command::new("ssh-add")
        .arg("-L")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| parse_agent_keys(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

/// `env` with the askpass variables it names, for tests that check a secret
/// never reaches them.
pub fn env_words(ssh: &Ssh) -> Vec<OsString> {
    ssh.env()
        .iter()
        .map(|(key, value)| {
            let mut word = OsString::from(key);
            word.push("=");
            word.push(value);
            word
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &Path) -> Paths {
        Paths::under(dir)
    }

    fn connector(dir: &Path, sign_in: SignIn) -> Connector {
        let address = Address::new("box.int", 2222, Some("dev"), Some("bastion")).unwrap();
        Connector::new("devbox-1a2b", address, sign_in, paths(dir)).unwrap()
    }

    #[test]
    fn nothing_in_an_address_can_read_as_an_option() {
        assert!(Address::new("-oProxyCommand=x", 22, None, None).is_err());
        assert!(Address::new("box", 22, Some("-l"), None).is_err());
        assert!(Address::new("box", 22, None, Some("-J")).is_err());
        assert!(Address::new("two words", 22, None, None).is_err());
        assert!(Address::new("a@b", 22, None, None).is_err());
        assert!(Address::new("box", 0, None, None).is_err());
        assert!(Address::new("box.int", 22, Some("dev"), Some("dev@bastion:2200")).is_ok());
        let address = Address::new("box", 22, None, None).unwrap();
        assert!(Connector::new(
            "../x",
            address.clone(),
            SignIn::Agent,
            paths(Path::new("/c"))
        )
        .is_err());
        assert!(Connector::new(
            "m",
            address,
            SignIn::Key("-oProxyCommand=x".into()),
            paths(Path::new("/c"))
        )
        .is_err());
    }

    #[test]
    fn jump_host_keyscan_quotes_remote_shell_metacharacters() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("injected");
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let scanner = bin.join("ssh-keyscan");
        std::fs::write(&scanner, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
        std::fs::set_permissions(&scanner, std::fs::Permissions::from_mode(0o700)).unwrap();
        let hostile = format!("box;touch${{IFS}}{}", marker.display());
        let command = keyscan_remote_command(&[
            "-T".into(),
            "10".into(),
            "-p".into(),
            "22".into(),
            hostile.clone(),
        ]);
        let output = Command::new("/bin/sh")
            .args(["-c", &command])
            .env("PATH", &bin)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(!marker.exists(), "host text executed as a remote command");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().lines().last(),
            Some(hostile.as_str())
        );
    }

    /// Every authenticating connection checks the machine's own pin strictly,
    /// names the destination after `--`, and opens no forward.
    #[test]
    fn a_connection_checks_the_pin_strictly_and_forwards_nothing() {
        let dir = Path::new("/c");
        let ssh = connector(dir, SignIn::Agent).ssh(Mux::Shared, None);
        let args = ssh.args().join(" ");
        assert!(args.contains("-o StrictHostKeyChecking=yes"));
        assert!(args.contains("-o HostKeyAlias=codesense-devbox-1a2b"));
        assert!(
            args.contains("-o UserKnownHostsFile=\"/c/ssh/known_hosts.d/codesense-devbox-1a2b\"")
        );
        assert!(
            args.contains("-o GlobalKnownHostsFile=/dev/null"),
            "only the pin admits a key"
        );
        assert!(args.contains("-o ClearAllForwardings=yes") && args.contains("-o ForwardAgent=no"));
        assert!(args.contains("-o BatchMode=yes"));
        assert!(args.contains("-o ControlMaster=auto"));
        assert!(args.contains("codesense-devbox-1a2b-%C"));
        assert!(args.contains("-p 2222") && args.contains("-l dev") && args.contains("-J bastion"));
        assert!(args.ends_with("-- box.int"));
        let unsafe_tofu = ["accept", "new"].join("-");
        assert!(!args.contains(&unsafe_tofu) && !args.contains("StrictHostKeyChecking=no"));
        for forward in ["-L", "-R", "-D", "-W"] {
            assert!(!ssh.args().iter().any(|a| a == forward), "{forward}");
        }
        let deep = Path::new("/a-very-deep-directory-that-leaves-no-room-for-a-master-socket-path");
        let unshared = connector(deep, SignIn::Agent).ssh(Mux::Shared, None);
        assert!(
            unshared.args().join(" ").contains("-o ControlPath=none"),
            "a master socket that can't be bound is not asked for"
        );
        let fresh = connector(dir, SignIn::Key("/k/id".into())).ssh(Mux::Fresh, None);
        let args = fresh.args().join(" ");
        assert!(args.contains("-o ControlPath=none"));
        assert!(args.contains("-i /k/id -o IdentitiesOnly=yes"));
    }

    #[test]
    fn a_passphrase_key_asks_the_askpass_and_nothing_else() {
        // Distinctive, so no random tempdir name in argv can contain it.
        const SECRET: &str = "codesense-test-passphrase-7f3a";
        let dir = tempfile::tempdir().unwrap();
        let askpass = Askpass::serve(
            &dir.path().join("a"),
            Path::new("/bin/false"),
            Path::new("/k/id"),
            SECRET,
        )
        .unwrap();
        let ssh = connector(dir.path(), SignIn::KeyWithPassphrase("/k/id".into()))
            .ssh(Mux::Fresh, Some(&askpass));
        let args = ssh.args().join(" ");
        assert!(args.contains("-o BatchMode=no"));
        assert!(args.contains("-o PasswordAuthentication=no"));
        assert!(args.contains("-o KbdInteractiveAuthentication=no"));
        assert!(!args.contains(SECRET));
        let env = env_words(&ssh);
        assert_eq!(env.len(), 3);
        assert!(env.iter().all(|w| !w.to_string_lossy().contains(SECRET)));
    }

    #[test]
    fn a_pin_is_the_machine_alias_and_its_key_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let c = connector(dir.path(), SignIn::Agent);
        let key = HostKey::parse(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl",
        )
        .unwrap();
        c.pin(&key).unwrap();
        let text = std::fs::read_to_string(c.known_hosts()).unwrap();
        assert_eq!(text, format!("codesense-devbox-1a2b {}\n", key.to_line()));
        let mode = std::fs::metadata(c.known_hosts())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    /// The fingerprint is `ssh-keygen -l`'s.
    #[test]
    fn a_fingerprint_matches_ssh_keygen() {
        let dir = tempfile::tempdir().unwrap();
        let key_file = dir.path().join("k");
        let made = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&key_file)
            .status()
            .unwrap();
        assert!(made.success());
        let public = std::fs::read_to_string(key_file.with_extension("pub")).unwrap();
        let listed = Command::new("ssh-keygen")
            .args(["-l", "-E", "sha256", "-f"])
            .arg(key_file.with_extension("pub"))
            .output()
            .unwrap();
        let listed = String::from_utf8_lossy(&listed.stdout);
        let key = HostKey::parse(&public).unwrap();
        assert_eq!(
            listed.split_whitespace().nth(1),
            Some(key.fingerprint().as_str())
        );
        assert!(HostKey::parse("ssh-ed25519 not*base64").is_err());
        assert!(HostKey::parse("onlyoneword").is_err());
    }

    #[test]
    fn ssh_failures_are_told_apart() {
        assert_eq!(
            classify("ssh: Could not resolve hostname nope: nodename nor servname provided\n"),
            Failure::Unresolved(
                "ssh: Could not resolve hostname nope: nodename nor servname provided".into()
            )
        );
        assert!(matches!(
            classify("ssh: connect to host 192.0.2.1 port 22: Operation timed out"),
            Failure::Unreachable(_)
        ));
        assert!(matches!(
            classify("ssh: connect to host box port 22: Connection refused"),
            Failure::Unreachable(_)
        ));
        assert_eq!(
            classify("@@@\nWARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!\n@@@\nHost key for codesense-box-1a2b3c has changed and you have requested strict checking.\nHost key verification failed.\n"),
            Failure::HostKeyChanged
        );
        assert!(matches!(
            classify("dev@127.0.0.1: Permission denied (publickey).\r\n"),
            Failure::AuthFailed(_)
        ));
        assert!(matches!(classify(""), Failure::Other(_)));
        let jump = classify("No ED25519 host key is known for bastion and you have requested strict checking.\nHost key verification failed.\n");
        assert!(
            matches!(&jump, Failure::Other(e) if e.contains("jump host")),
            "{jump:?}"
        );
        assert_eq!(
            classify("No ED25519 host key is known for codesense-box-1a2b3c and you have requested strict checking.\nHost key verification failed.\n"),
            Failure::HostKeyChanged,
            "a pinned machine no longer offering its pinned key type has changed"
        );
    }

    #[test]
    fn an_unexplained_scan_failure_is_unreachable_unless_ssh_names_a_changed_key() {
        // A filtered port: ssh says nothing, so nothing could have changed.
        let silent = classify("");
        assert!(matches!(
            unkeyed_failure(true, silent),
            Failure::Unreachable(message) if message.contains("check the network path")
        ));
        // A closed port with a pinned key: the reason is a network one.
        assert!(matches!(
            unkeyed_failure(true, classify("ssh: connect to host box port 22: Connection refused")),
            Failure::Unreachable(message) if message.contains("Connection refused")
        ));
        // ssh says the key changed: that is the one case to report as changed.
        assert_eq!(
            unkeyed_failure(true, classify("Host key for codesense-box-1a2b3c has changed.\nHost key verification failed.\n")),
            Failure::HostKeyChanged
        );
        assert!(matches!(
            unkeyed_failure(true, Failure::Unresolved("nope".into())),
            Failure::Unresolved(_)
        ));
        // Auth is never attempted by a scan, but if ssh says so, it is not a key change.
        assert!(matches!(
            unkeyed_failure(true, Failure::AuthFailed("Permission denied".into())),
            Failure::AuthFailed(_)
        ));
        // A jump host's unverified key keeps its own, more accurate, message.
        let jump = classify("No ED25519 host key is known for bastion and you have requested strict checking.\nHost key verification failed.\n");
        assert!(matches!(
            unkeyed_failure(true, jump),
            Failure::Other(message) if message.contains("jump host")
        ));
        assert!(matches!(
            unkeyed_failure(false, classify("")),
            Failure::Other(_)
        ));
    }

    #[test]
    fn a_pinned_scan_through_scan_reports_silence_as_unreachable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let pinned = HostKey::parse(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl",
        )
        .unwrap();
        let fake = |name: &str, body: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        // No jump host here: the scan must reach the fake key scanner directly.
        // `ssh -G` can't resolve it, so the typed host is scanned.
        let unresolved = fake("unresolved-ssh", "exit 255");
        let direct = || {
            let address = Address::new("box.int", 2222, Some("dev"), None).unwrap();
            Connector::new("devbox-1a2b", address, SignIn::Agent, paths(dir.path()))
                .unwrap()
                .with_program(&unresolved)
        };
        // A filtered port: keyscan prints nothing and ssh says nothing.
        let silent = direct().with_scan_program(fake("silent", "exit 0"));
        assert!(matches!(
            silent.scan(dir.path(), Some(&pinned)),
            Err(Failure::Unreachable(message)) if message.contains("check the network path")
        ));
        // A closed port: ssh says so, and the reason stays a network one.
        let closed = direct().with_scan_program(fake(
            "closed",
            "echo 'ssh: connect to host box port 22: Connection refused' >&2; exit 255",
        ));
        assert!(matches!(
            closed.scan(dir.path(), Some(&pinned)),
            Err(Failure::Unreachable(message)) if message.contains("Connection refused")
        ));
        // ssh names a changed key: that is still reported as a changed key.
        let changed = direct().with_scan_program(fake(
            "changed",
            "echo 'Host key for codesense-devbox-1a2b has changed.' >&2; echo 'Host key verification failed.' >&2; exit 1",
        ));
        assert!(matches!(
            changed.scan(dir.path(), Some(&pinned)),
            Err(Failure::HostKeyChanged)
        ));
    }

    /// A setting that could skip the pin is turned off where this `ssh` has
    /// it, and left out where `ssh` would refuse it (upstream OpenSSH has no
    /// `NoHostAuthenticationForProxyCommand`).
    #[test]
    fn options_that_could_skip_the_pin_are_off_where_ssh_has_them() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fake = |name: &str, body: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let args = |program: &Path| {
            let connector = connector(dir.path(), SignIn::Agent).with_program(program);
            let ssh = connector.ssh(Mux::Fresh, None);
            ssh.args().join(" ")
        };
        let apple = fake("apple-ssh", "exit 0");
        let upstream = fake(
            "upstream-ssh",
            "case \"$*\" in *NoHostAuthenticationForProxyCommand*) \
             echo 'command-line: line 0: Bad configuration option: \
             nohostauthenticationforproxycommand' >&2; exit 255;; esac",
        );
        let with = args(&apple);
        assert!(with.contains("-o KnownHostsCommand=none"), "{with}");
        assert!(
            with.contains("-o NoHostAuthenticationForProxyCommand=no"),
            "{with}"
        );
        let without = args(&upstream);
        assert!(without.contains("-o KnownHostsCommand=none"), "{without}");
        assert!(
            !without.contains("NoHostAuthenticationForProxyCommand"),
            "{without}"
        );
    }

    const ED25519: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";

    /// A fake `ssh` and `ssh-keyscan` under `dir`: `ssh -G` prints
    /// `resolved`; any other `ssh` logs its arguments one per line and runs
    /// `then`; the key scanner logs its arguments and prints `ED25519`.
    fn fakes(dir: &Path, resolved: &str, then: &str) -> (PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let write = |name: &str, body: String| {
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let log = |name: &str| shell_quote(&dir.join(name).to_string_lossy());
        let ssh = write(
            "ssh",
            format!(
                "if [ \"$1\" = -G ]; then printf '%s\\n' {resolved}; exit 0; fi\n\
                 printf '%s\\n' \"$@\" > {log}\n{then}",
                resolved = shell_quote(resolved),
                log = log("ssh.args"),
            ),
        );
        let keyscan = write(
            "ssh-keyscan",
            format!(
                "printf '%s\\n' \"$@\" > {log}\necho \"$5 {ED25519}\"",
                log = log("keyscan.args"),
            ),
        );
        (ssh, keyscan)
    }

    /// The `-o` options a logged `ssh` was given, and its words after `--`.
    fn logged(dir: &Path, name: &str) -> (Vec<String>, Vec<String>) {
        let words: Vec<String> = std::fs::read_to_string(dir.join(name))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect();
        let options = words
            .windows(2)
            .filter(|w| w[0] == "-o")
            .map(|w| w[1].clone())
            .collect();
        let after = words
            .iter()
            .position(|w| w == "--")
            .map(|at| words[at + 1..].to_vec())
            .unwrap_or_default();
        (options, after)
    }

    fn coder(dir: &Path, ssh: &Path, keyscan: &Path) -> Connector {
        let address = Address::new("coder.ws", 22, Some("coder"), None).unwrap();
        Connector::new("coder-ws-1a2b", address, SignIn::Agent, paths(dir))
            .unwrap()
            .with_program(ssh)
            .with_scan_program(keyscan)
    }

    const PROXIED: &str = "hostname coder.ws\nport 22\nproxycommand coder ssh --stdio ws";

    /// A host the user's config reaches through a ProxyCommand is scanned by
    /// `ssh` itself: no credential offered, the key recorded only in a
    /// throwaway file under its machine alias, and never by `ssh-keyscan`.
    #[test]
    fn a_proxy_command_host_is_scanned_through_ssh() {
        let dir = tempfile::tempdir().unwrap();
        // Record the key where ssh was told to, then fail to sign in.
        let record = format!(
            "for a in \"$@\"; do case \"$a\" in UserKnownHostsFile=*) f=${{a#UserKnownHostsFile=}};; esac; done\n\
             f=${{f#\\\"}}; f=${{f%\\\"}}\n\
             echo \"codesense-coder-ws-1a2b {ED25519}\" > \"$f\"\n\
             echo 'Permission denied (publickey).' >&2; exit 255"
        );
        let (ssh, keyscan) = fakes(dir.path(), PROXIED, &record);
        let scratch = dir.path().join("scratch");
        let key = coder(dir.path(), &ssh, &keyscan)
            .scan(&scratch, None)
            .unwrap();
        assert_eq!(key.to_line(), ED25519);
        assert!(!dir.path().join("keyscan.args").exists(), "ssh-keyscan ran");
        let (options, after) = logged(dir.path(), "ssh.args");
        for option in [
            "StrictHostKeyChecking=accept-new",
            "GlobalKnownHostsFile=/dev/null",
            "HostKeyAlias=codesense-coder-ws-1a2b",
            "KnownHostsCommand=none",
            "HashKnownHosts=no",
            "ControlMaster=no",
            "ControlPath=none",
            "BatchMode=yes",
            "PubkeyAuthentication=no",
            "PasswordAuthentication=no",
            "KbdInteractiveAuthentication=no",
            "GSSAPIAuthentication=no",
            "HostbasedAuthentication=no",
            "IdentityAgent=none",
            "PKCS11Provider=none",
            "PermitLocalCommand=no",
            "RemoteCommand=none",
            "ClearAllForwardings=yes",
            "ForwardAgent=no",
        ] {
            assert!(
                options.iter().any(|o| o == option),
                "missing {option}: {options:?}"
            );
        }
        let known_hosts = options
            .iter()
            .find_map(|o| o.strip_prefix("UserKnownHostsFile="))
            .unwrap()
            .trim_matches('"');
        assert!(
            Path::new(known_hosts).starts_with(&scratch),
            "{known_hosts}"
        );
        assert!(!options.iter().any(|o| o.starts_with("ProxyCommand")));
        assert!(!options.iter().any(|o| o.starts_with("HostKeyAlgorithms")));
        assert_eq!(after, ["coder.ws", "true"]);
        // The throwaway file is gone, and nothing was pinned.
        assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
        assert!(!paths(dir.path()).pin_file("coder-ws-1a2b").exists());
    }

    #[test]
    fn a_pinned_proxy_scan_asks_for_the_pinned_key_type() {
        let dir = tempfile::tempdir().unwrap();
        let (ssh, keyscan) = fakes(dir.path(), PROXIED, "exit 255");
        let rsa = HostKey::parse("ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAAAgQC7").unwrap();
        let _ = coder(dir.path(), &ssh, &keyscan).scan(&dir.path().join("scratch"), Some(&rsa));
        let (options, _) = logged(dir.path(), "ssh.args");
        assert!(
            options
                .iter()
                .any(|o| o == "HostKeyAlgorithms=rsa-sha2-512,rsa-sha2-256,ssh-rsa"),
            "{options:?}"
        );
    }

    #[test]
    fn a_silent_proxy_scan_names_the_host_and_what_to_try() {
        let dir = tempfile::tempdir().unwrap();
        let (ssh, keyscan) = fakes(dir.path(), PROXIED, "exit 255");
        let failure = coder(dir.path(), &ssh, &keyscan)
            .scan(&dir.path().join("scratch"), None)
            .unwrap_err();
        assert_eq!(
            failure.to_string(),
            "ssh got no host key from coder.ws through its ProxyCommand and gave no reason. \
             Check that ssh coder.ws works in a terminal."
        );
        // What ssh says still reaches the person.
        let (ssh, keyscan) = fakes(
            dir.path(),
            PROXIED,
            "echo 'Coder: workspace \"ws\" not found' >&2; exit 255",
        );
        let failure = coder(dir.path(), &ssh, &keyscan)
            .scan(&dir.path().join("scratch"), None)
            .unwrap_err();
        assert!(failure.to_string().contains("not found"), "{failure}");
    }

    /// An alias with a `HostName` and no proxy is keyscanned where `ssh`
    /// would connect.
    #[test]
    fn a_host_name_alias_is_keyscanned_at_its_host_name() {
        let dir = tempfile::tempdir().unwrap();
        let (ssh, keyscan) = fakes(dir.path(), "hostname 192.0.2.5\nport 2200", "exit 255");
        let key = coder(dir.path(), &ssh, &keyscan)
            .scan(&dir.path().join("scratch"), None)
            .unwrap();
        assert_eq!(key.to_line(), ED25519);
        let args = std::fs::read_to_string(dir.path().join("keyscan.args")).unwrap();
        assert!(args.ends_with("-p\n2200\n192.0.2.5\n"), "{args}");
        assert!(!dir.path().join("ssh.args").exists(), "ssh connected");
    }

    /// A jump host overrides any proxy: the config is not consulted, and the
    /// scan runs on the jump host as before.
    #[test]
    fn a_jump_host_scan_does_not_resolve_the_config() {
        let dir = tempfile::tempdir().unwrap();
        let (ssh, keyscan) = fakes(dir.path(), PROXIED, "exit 255");
        let address = Address::new("box.int", 22, None, Some("bastion")).unwrap();
        let _ = Connector::new("box-1a2b", address, SignIn::Agent, paths(dir.path()))
            .unwrap()
            .with_program(&ssh)
            .with_scan_program(&keyscan)
            .scan(&dir.path().join("scratch"), None);
        let (_, after) = logged(dir.path(), "ssh.args");
        assert_eq!(after.first().map(String::as_str), Some("bastion"));
        assert!(after[1].starts_with("ssh-keyscan "), "{after:?}");
    }

    #[test]
    fn config_hosts_are_the_concrete_aliases() {
        let config = "Host *\n  ForwardAgent no\nHost devbox bastion\n  HostName devbox.int\nHost=flaky-vm\nhost !nope ci-* -evil\nMatch host x\nHost devbox\nHost staging # the old one\n";
        assert_eq!(
            config_aliases(config),
            ["devbox", "bastion", "flaky-vm", "staging"]
        );
        let resolved =
            parse_resolved("devbox", "user dev\nhostname devbox.int\nport 2222\n").unwrap();
        assert_eq!(
            resolved,
            ConfigHost {
                alias: "devbox".into(),
                host_name: "devbox.int".into(),
                user: Some("dev".into()),
                port: 2222,
                proxy_jump: None,
                identity_file: None,
                proxy_command: None,
            }
        );
        assert!(parse_resolved("x", "user dev\n").is_none());
    }

    #[test]
    fn an_alias_keeps_its_jump_host_key_and_proxy_command() {
        let defaults = "identityfile ~/.ssh/id_rsa\nidentityfile ~/.ssh/id_ecdsa\n\
                        identityfile /Users/dev/.ssh/id_ed25519\n";
        let plain = parse_resolved("devbox", &format!("hostname devbox.int\n{defaults}")).unwrap();
        assert_eq!(plain.identity_file, None, "ssh's defaults name no key");
        let resolved = parse_resolved(
            "devbox",
            &format!(
                "hostname devbox.int\nproxyjump bastion.acme.io\n\
                 identityfile ~/.ssh/work_ed25519\n{defaults}proxycommand none\n"
            ),
        )
        .unwrap();
        assert_eq!(resolved.proxy_jump.as_deref(), Some("bastion.acme.io"));
        assert_eq!(
            resolved.identity_file.as_deref(),
            Some("~/.ssh/work_ed25519")
        );
        assert_eq!(resolved.proxy_command, None);
        let proxied = parse_resolved(
            "vm",
            "hostname vm.int\nproxycommand cloudflared access ssh --hostname %h\n",
        )
        .unwrap();
        assert_eq!(
            proxied.proxy_command.as_deref(),
            Some("cloudflared access ssh --hostname %h")
        );
    }

    #[test]
    fn agent_keys_carry_their_fingerprint_and_public_line() {
        let line = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl dev@mac";
        let keys = parse_agent_keys(&format!("{line}\nnot a key\n"));
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key_type, "ed25519");
        assert_eq!(keys[0].comment, "dev@mac");
        assert_eq!(keys[0].public_key, line);
        assert!(keys[0].fingerprint.starts_with("SHA256:"));
        assert_eq!(key_label("sk-ssh-ed25519@openssh.com"), "ed25519-sk");
        assert_eq!(key_label("ecdsa-sha2-nistp256"), "ecdsa");
        assert_eq!(key_label("sk-ecdsa-sha2-nistp256@openssh.com"), "ecdsa-sk");
        assert_eq!(key_label("ssh-rsa"), "rsa");
    }
}
