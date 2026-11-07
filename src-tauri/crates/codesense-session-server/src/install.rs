// SPDX-License-Identifier: MIT
//! Installing this host agent on another machine, over nothing but the user's
//! own SSH (the system `ssh`, so their config, agent, jump hosts and known
//! hosts all apply).
//!
//! A machine keeps each installed version in its own directory,
//! `~/.codesense/host/<version>/`: the binary, the terminal emulator worker it
//! runs (`codesense-emulator`, found next to the binary) and the resources it
//! ships (`resources/…`, found by `attach` next to the binary),
//! with `~/.codesense/host/current` pointing at the latest. An install:
//!
//! 1. probes the machine (`uname -sm`, the installed versions, `current`);
//! 2. when its version is missing, uploads each file with `cat >` into a fresh
//!    staging directory under `~/.codesense/host/` (no `scp`, and the machine
//!    never downloads anything);
//! 3. checks every file's sha256 there, makes the binary executable, and
//!    renames the staging directory to `<version>` in one step, then points
//!    `current` at it.
//!
//! A version directory exists only once every file in it is verified, so an
//! interrupted upload never leaves a partial binary where one is run; its
//! staging directory is cleared by a later install. A re-run finds the version
//! present and changes nothing. Nothing needs root.
//!
//! [`prepare`] probes (read-only) and plans; [`Prepared::commands`] lists
//! exactly the commands [`apply`] then runs, for a user to review first. A
//! client runs the relay of the exact version it installed
//! ([`attach_script`]), never `current`: `current` is for a person at the
//! machine's shell, so its non-atomic swap (`ln -sfn`) never races a client.

use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

use codesense_pty_core::emulator::WORKER_BIN;

use crate::attach::RESOURCES_DIR;
use crate::survival::{self, Survival};

/// The binary's name, on every machine.
pub const BINARY: &str = "codesense-session-server";
pub const BRIDGE: &str = "codesense-bridge";
pub fn bridge_artifact_name(triple: &str) -> String {
    format!("{BRIDGE}-{triple}")
}

/// The shell-integration scripts a host ships, under
/// `resources/shell-integration/`.
pub const SHELL_INIT_SCRIPTS: [&str; 3] = [
    "codesense-shell-init.zsh",
    "codesense-shell-init.bash",
    "codesense-shell-init.fish",
];

/// The emulator worker's third-party notices among the build artifacts, and
/// where a version directory keeps them.
pub const EMULATOR_NOTICES: &str = "codesense-emulator-NOTICE.md";
pub(crate) const EMULATOR_NOTICES_REL: &str = "resources/emulator/NOTICE.md";

/// Where a machine keeps its installed versions, under the user's home.
pub const HOST_DIR: &str = ".codesense/host";

/// The link to a machine's latest installed version, inside [`HOST_DIR`].
pub const CURRENT: &str = "current";
/// Immutable wire-major metadata covered by each installed version's manifest.
pub const PROTOCOL_MAJOR_FILE: &str = ".codesense-protocol-major";

/// The prefix of a staging directory, inside [`HOST_DIR`]. The leading dot
/// keeps it out of the probe's listing.
const STAGING_PREFIX: &str = ".incoming-";

/// Staging directories older than this are an interrupted install's, and a
/// later install removes them. A day: far longer than any live upload.
///
/// The final step keeps a version directory that holds a runnable binary: one
/// another install finished first is identical. One without (damaged by
/// hand) is renamed aside, never deleted in place, before the verified
/// staging takes its name; a staging directory another install's rename
/// landed inside is removed.
const STALE_STAGING_MINUTES: u32 = 24 * 60;

/// The build artifact of the binary for `triple`.
pub fn artifact_name(triple: &str) -> String {
    format!("{BINARY}-{triple}")
}

/// The build artifact of the emulator worker for `triple`.
pub fn emulator_artifact_name(triple: &str) -> String {
    format!("{WORKER_BIN}-{triple}")
}

/// The file recording the sha256 of the artifact at `artifact`, in
/// `sha256sum` format.
pub fn digest_file(artifact: &Path) -> PathBuf {
    let mut name = artifact.as_os_str().to_owned();
    name.push(".sha256");
    PathBuf::from(name)
}

/// Every target triple [`triple_for`] can name.
pub const LINUX_TRIPLES: [&str; 2] = ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"];

/// The recorded sha256 of the terminal-server binary this build carries for
/// `triple`, if it carries one: the only binary a `triple` machine may run in
/// place of an upload.
pub fn carried_binary_sha256(artifacts: &Path, triple: &str) -> Option<String> {
    recorded_sha256(&digest_file(&artifacts.join(artifact_name(triple)))).ok()
}

/// The Rust target triple of the binary a machine runs, from its `uname -sm`:
/// only Linux machines get a host agent.
pub fn triple_for(uname: &str) -> Result<&'static str, String> {
    let mut words = uname.split_whitespace();
    match (words.next(), words.next()) {
        (Some("Linux"), Some("x86_64" | "amd64")) => Ok(LINUX_TRIPLES[0]),
        (Some("Linux"), Some("aarch64" | "arm64")) => Ok(LINUX_TRIPLES[1]),
        _ => Err(format!("no host agent is built for {:?}", uname.trim())),
    }
}

/// One file of a [`Bundle`]: where it goes inside the version directory, where
/// it is read from here, and its sha256 (lowercase hex).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleFile {
    pub rel: String,
    pub source: PathBuf,
    pub sha256: String,
}

/// What an install puts on a machine of one triple: the binary, the emulator
/// worker and the resources they ship, under a version named for their
/// content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    pub triple: String,
    pub version: String,
    pub files: Vec<BundleFile>,
}

impl Bundle {
    /// The bundle for `triple`: its binary and emulator worker from
    /// `artifacts` (each must match the sha256 recorded next to it), the
    /// worker's third-party notices from there too, and the shell-integration
    /// scripts from `shell_init`.
    pub fn load(artifacts: &Path, shell_init: &Path, triple: &str) -> Result<Self, String> {
        let notices = artifacts.join(EMULATOR_NOTICES);
        let mut files = vec![
            verified_artifact(artifacts.join(artifact_name(triple)), BINARY)?,
            verified_artifact(artifacts.join(bridge_artifact_name(triple)), BRIDGE)?,
            verified_artifact(artifacts.join(emulator_artifact_name(triple)), WORKER_BIN)?,
            BundleFile {
                rel: EMULATOR_NOTICES_REL.to_string(),
                sha256: sha256_of(&notices)?,
                source: notices,
            },
        ];
        for script in SHELL_INIT_SCRIPTS {
            let source = shell_init.join(script);
            files.push(BundleFile {
                rel: format!("{RESOURCES_DIR}/shell-integration/{script}"),
                sha256: sha256_of(&source)?,
                source,
            });
        }
        Ok(Self::of(triple, files))
    }

    /// A bundle of `files`, versioned by this build's version and a digest of
    /// every file's path and content, so two different builds never share a
    /// version directory.
    pub fn of(triple: &str, files: Vec<BundleFile>) -> Self {
        let mut manifest = Sha256::new();
        for file in &files {
            manifest.update(format!("{}  {}\n", file.sha256, file.rel));
        }
        let protocol_major = codesense_host_proto::PROTO_MAJOR.to_string();
        let protocol_major_sha = hex(&Sha256::digest(format!("{protocol_major}\n")));
        manifest.update(format!("{protocol_major_sha}  {PROTOCOL_MAJOR_FILE}\n"));
        let digest = hex(&manifest.finalize());
        Self {
            triple: triple.to_string(),
            version: format!("{}-{}", env!("CARGO_PKG_VERSION"), &digest[..12]),
            files,
        }
    }
}

/// This process's build, as the version directory it was installed
/// under (`{version}-{digest}`, see [`Bundle::of`]), when it runs from one.
pub fn running_bundle_version() -> Option<String> {
    bundle_version_of_executable(&std::env::current_exe().ok()?)
}
/// The install layout is `…/host/{version}/codesense-session-server` for both
/// an SSH install and a private-account one. A private server is exec'd
/// through `/proc/self/fd/N/…`; resolving the path names the real directory.
pub fn bundle_version_of_executable(executable: &Path) -> Option<String> {
    let real = std::fs::canonicalize(executable).unwrap_or_else(|_| executable.to_path_buf());
    let directory = real.parent()?;
    let version = directory.file_name()?.to_str()?;
    if directory.parent()?.file_name()? != HOST_LEAF {
        return None;
    }
    bundle_digest(version).map(|_| version.to_string())
}
/// The directory every installed version sits in.
const HOST_LEAF: &str = "host";
/// The package version of this terminal-server build (no build metadata).
pub fn package_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
/// The build this binary came from — the CodeSense app release,
/// commit and build time `build.rs` stamped. The app links this crate, so it
/// reads its own stamp here too: a sidecar built alongside it matches exactly.
pub fn build_stamp() -> codesense_host_proto::BuildStamp {
    codesense_host_proto::BuildStamp {
        version: env!("CODESENSE_APP_VERSION").to_string(),
        commit: Some(env!("CODESENSE_BUILD_COMMIT"))
            .filter(|commit| !commit.is_empty())
            .map(str::to_string),
        built_at_ms: env!("CODESENSE_BUILD_TIME_MS")
            .parse::<u64>()
            .ok()
            .filter(|ms| *ms > 0),
    }
}
/// The digest part of a `{CARGO_PKG_VERSION}-{12 hex}` version directory name.
fn bundle_digest(version: &str) -> Option<&str> {
    let digest = version.strip_prefix(concat!(env!("CARGO_PKG_VERSION"), "-"))?;
    (digest.len() == 12 && digest.bytes().all(|b| b.is_ascii_hexdigit())).then_some(digest)
}
/// The version a host announces in its hello: the package version, plus its
/// installed build as semver build metadata (`0.0.20+1a2b3c4d5e6f`), so two
/// builds of one version are told apart.
pub fn announced_version(bundle: Option<&str>) -> String {
    match bundle.and_then(bundle_digest) {
        Some(digest) => format!("{}+{digest}", env!("CARGO_PKG_VERSION")),
        None => env!("CARGO_PKG_VERSION").to_string(),
    }
}
/// The installed build an announced version names (`{version}-{digest}`), or
/// `None` for a host that predates announcing it.
pub fn bundle_version_of(announced: &str) -> Option<String> {
    let (version, digest) = announced.split_once('+')?;
    (digest.len() == 12 && digest.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| format!("{version}-{digest}"))
}

/// The build artifact at `artifact`, installed as `rel`, once it matches the
/// sha256 recorded next to it.
fn verified_artifact(artifact: PathBuf, rel: &str) -> Result<BundleFile, String> {
    let recorded = recorded_sha256(&digest_file(&artifact))?;
    let actual = sha256_of(&artifact)?;
    if actual != recorded {
        return Err(format!(
            "{artifact:?} does not match its recorded sha256 ({actual} != {recorded})"
        ));
    }
    Ok(BundleFile {
        rel: rel.to_string(),
        source: artifact,
        sha256: actual,
    })
}

/// The sha256 recorded in a `sha256sum`-format file: its first word.
fn recorded_sha256(path: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read {path:?}: {e}"))?;
    text.split_whitespace()
        .next()
        .filter(|word| word.len() == 64 && word.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| format!("{path:?} records no sha256"))
}

/// The sha256 of the file at `path`, in lowercase hex.
pub fn sha256_of(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("open {path:?}: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("read {path:?}: {e}")),
        }
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// `value` as one single-quoted POSIX shell word.
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// `value` as one POSIX shell word, quoted only when it has to be.
pub fn shell_word(value: &str) -> String {
    let plain = !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@%+=:,./_-".contains(&b));
    if plain {
        value.to_string()
    } else {
        shell_quote(value)
    }
}

/// How to reach a machine: the `ssh` program, the arguments naming the
/// machine (options first, the destination last), and what `ssh` runs with
/// beyond this process's environment (an askpass, never a secret).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ssh {
    program: PathBuf,
    args: Vec<String>,
    env: Vec<(String, OsString)>,
}

impl Ssh {
    pub fn new(program: impl Into<PathBuf>, args: Vec<String>) -> Self {
        Self {
            program: program.into(),
            args,
            env: Vec::new(),
        }
    }

    /// This `ssh`, run with `env` added to its environment.
    pub fn with_env(mut self, env: Vec<(String, OsString)>) -> Self {
        self.env = env;
        self
    }

    /// The arguments naming the machine.
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// What `ssh` runs with beyond this process's environment.
    pub fn env(&self) -> &[(String, OsString)] {
        &self.env
    }

    /// The remote command running `script` under `sh`. The user's login shell
    /// passes it on: any POSIX shell or fish; csh and tcsh reject the newlines
    /// a multi-line script keeps inside its quotes.
    fn remote(script: &str) -> String {
        format!("sh -c {}", shell_quote(script))
    }

    /// The `ssh` command running `script` on the machine.
    pub fn command(&self, script: &str) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .arg(Self::remote(script))
            .envs(self.env.iter().map(|(key, value)| (key, value)));
        command
    }

    /// The command line of [`Ssh::command`], as a user would type it.
    pub fn command_line(&self, script: &str) -> String {
        std::iter::once(self.program.to_string_lossy().into_owned())
            .chain(self.args.iter().cloned())
            .chain(std::iter::once(Self::remote(script)))
            .map(|word| shell_word(&word))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Run `step` to completion, whatever its exit status.
    pub fn output(&self, step: &Step) -> Result<std::process::Output, String> {
        let stdin = match &step.upload {
            Some(source) => {
                Stdio::from(File::open(source).map_err(|e| format!("open {source:?}: {e}"))?)
            }
            None => Stdio::null(),
        };
        self.command(&step.script)
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("run {:?}: {e}", self.program))
    }

    /// [`Ssh::output`], killed when it outlives `timeout` — a remote command
    /// can hang while the connection's keepalives keep answering.
    pub fn output_within(
        &self,
        step: &Step,
        timeout: std::time::Duration,
    ) -> Result<std::process::Output, String> {
        let stdin = match &step.upload {
            Some(source) => {
                Stdio::from(File::open(source).map_err(|e| format!("open {source:?}: {e}"))?)
            }
            None => Stdio::null(),
        };
        run_within(self.command(&step.script).stdin(stdin), timeout)
    }

    /// Run `step` and return its standard output. An upload may take a slow
    /// link a while; anything else that runs this long has hung.
    pub fn run(&self, step: &Step) -> Result<String, String> {
        let timeout = if step.upload.is_some() {
            std::time::Duration::from_secs(15 * 60)
        } else {
            std::time::Duration::from_secs(2 * 60)
        };
        let output = self.output_within(step, timeout)?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(format!(
                "{} failed ({}): {}",
                step.describe(),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }
}

/// Run `command` to completion with its output captured, killing it when it
/// outlives `timeout`.
pub fn run_within(
    command: &mut Command,
    timeout: std::time::Duration,
) -> Result<std::process::Output, String> {
    // Put the command in its own process group. ssh may start helpers (and a
    // remote-command transport); a timeout owns and terminates the whole tree,
    // never just the immediate child.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("run {:?}: {e}", command.get_program()))?;
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(timeout) {
        Ok(output) => output.map_err(|e| e.to_string()),
        Err(_) => {
            // SAFETY: `pid` is the leader of the process group we created
            // above. A negative pid addresses that group only.
            unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
            // Reap synchronously: returning while the waiter or descendants
            // remain live makes retries accumulate leaked processes.
            let _ = rx.recv();
            Err(format!(
                "{:?} did not finish within {} s",
                command.get_program(),
                timeout.as_secs()
            ))
        }
    }
}

#[cfg(test)]
mod build_identity_tests {
    use super::*;

    /// The stamp names the app release (`src-tauri/Cargo.toml`),
    /// never this crate's fixed `0.1.0`, and a build time.
    #[test]
    fn agr19_the_build_stamp_names_the_app_release() {
        // The release bump keeps tauri.conf.json's version equal to the app
        // manifest's (release-check enforces it), so it checks the stamp
        // without build.rs's parser.
        let conf = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tauri.conf.json"),
        )
        .unwrap();
        let after = &conf[conf.find("\"version\": \"").unwrap() + "\"version\": \"".len()..];
        let app_version = &after[..after.find('"').unwrap()];
        let stamp = build_stamp();
        assert_eq!(stamp.version, app_version);
        assert!(stamp.built_at_ms.is_some());
        if let Some(commit) = &stamp.commit {
            assert_eq!(commit.len(), 12);
            assert!(commit.bytes().all(|b| b.is_ascii_hexdigit()));
        }
    }

    /// Two builds of one package version announce different
    /// versions, and the desktop reads back the installed build.
    #[test]
    fn a_build_is_announced_and_read_back_by_its_digest() {
        let one = format!("{}-0123456789ab", env!("CARGO_PKG_VERSION"));
        let two = format!("{}-ba9876543210", env!("CARGO_PKG_VERSION"));
        assert_ne!(announced_version(Some(&one)), announced_version(Some(&two)));
        assert_eq!(
            bundle_version_of(&announced_version(Some(&one))).as_deref(),
            Some(one.as_str())
        );
        // Not run from an install directory: the plain version, no build.
        assert_eq!(announced_version(None), env!("CARGO_PKG_VERSION"));
        assert_eq!(
            announced_version(Some("not-a-bundle")),
            env!("CARGO_PKG_VERSION")
        );
        assert_eq!(bundle_version_of(env!("CARGO_PKG_VERSION")), None);
    }

    /// The running build is read from the real install directory, also when
    /// the executable was reached through a descriptor link.
    #[test]
    fn the_running_build_is_resolved_through_links() {
        let temp = tempfile::tempdir().unwrap();
        let version = format!("{}-0123456789ab", env!("CARGO_PKG_VERSION"));
        let installed = temp.path().join("host").join(&version);
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::write(installed.join(BINARY), b"").unwrap();
        let link = temp.path().join("fd-7");
        std::os::unix::fs::symlink(&installed, &link).unwrap();
        assert_eq!(
            bundle_version_of_executable(&link.join(BINARY)).as_deref(),
            Some(version.as_str())
        );
        // A development binary outside the install layout has no build.
        let dev = temp.path().join("target").join(&version).join(BINARY);
        assert_eq!(bundle_version_of_executable(&dev), None);
    }
}

#[cfg(test)]
mod timeout_tests {
    use super::*;

    #[test]
    fn timeout_kills_and_reaps_the_owned_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("descendant-pid");
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("sleep 30 & echo $! > \"$1\"; wait")
            .arg("codesense-timeout-test")
            .arg(&pid_file);
        let error = run_within(&mut command, std::time::Duration::from_millis(250)).unwrap_err();
        assert!(error.contains("did not finish"), "{error}");
        let pid: libc::pid_t = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // kill(pid, 0) reports ESRCH only after the descendant is gone.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        assert!(!alive, "descendant {pid} survived its owner's timeout");
    }
}

/// What a probe found on a machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// The triple of the binary the machine runs.
    pub triple: String,
    /// The version `current` points at.
    pub current: Option<String>,
    /// The installed versions.
    pub installed: Vec<String>,
}

/// The probe: the machine's `uname -sm`, where `current` points, and each
/// installed version whose expected manifest and every payload digest verify.
pub fn probe_step() -> Step {
    Step::new(format!(
        "H=\"$HOME/{HOST_DIR}\"\n\
         printf 'uname=%s\\n' \"$(uname -sm)\"\n\
         printf 'current=%s\\n' \"$(readlink \"$H/{CURRENT}\" 2>/dev/null)\"\n\
         for e in \"$H\"/*; do\n\
         \x20 [ ! -L \"$e\" ] && [ -x \"$e/{BINARY}\" ] && [ -f \"$e/.codesense-manifest\" ] && [ \"$(cat \"$e/{PROTOCOL_MAJOR_FILE}\" 2>/dev/null)\" = \"{}\" ] || continue\n\
         \x20 v=${{e##*/}}; want=${{v##*-}}\n\
         \x20 if command -v sha256sum >/dev/null 2>&1; then got=$(sha256sum \"$e/.codesense-manifest\" | cut -c1-12); check='sha256sum -c'\n\
         \x20 elif command -v shasum >/dev/null 2>&1; then got=$(shasum -a 256 \"$e/.codesense-manifest\" | cut -c1-12); check='shasum -a 256 -c'\n\
         \x20 else continue; fi\n\
         \x20 [ \"$got\" = \"$want\" ] && (cd \"$e\" && $check .codesense-manifest >/dev/null 2>&1) && printf 'installed=%s\\n' \"$v\"\n\
         done\n\
         exit 0"
    , codesense_host_proto::PROTO_MAJOR))
}

/// Read a probe's output.
pub fn parse_probe(output: &str) -> Result<Probe, String> {
    let mut uname = None;
    let mut current = None;
    let mut installed = Vec::new();
    for line in output.lines() {
        match line.split_once('=') {
            Some(("uname", value)) => uname = Some(value.to_string()),
            Some(("current", value)) if !value.is_empty() => current = Some(value.to_string()),
            Some(("installed", value)) => installed.push(value.to_string()),
            _ => {}
        }
    }
    let uname = uname.ok_or_else(|| format!("the probe did not answer: {output:?}"))?;
    Ok(Probe {
        triple: triple_for(&uname)?.to_string(),
        current,
        installed,
    })
}

/// One command of an install: a script run under `sh` on the machine, with a
/// local file as its standard input when it uploads one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub script: String,
    pub upload: Option<PathBuf>,
}

impl Step {
    /// A step that runs `script` and uploads nothing.
    pub fn new(script: impl Into<String>) -> Self {
        Self {
            script: script.into(),
            upload: None,
        }
    }

    fn describe(&self) -> String {
        match &self.upload {
            Some(source) => format!("upload of {source:?}"),
            None => format!("`{}`", self.script.lines().next().unwrap_or_default()),
        }
    }
}

/// The commands that bring a machine to a bundle's version. Empty when it is
/// installed and current already.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub version: String,
    pub steps: Vec<Step>,
}

impl Plan {
    /// Every command this plan runs, as a user would type it.
    pub fn commands(&self, ssh: &Ssh) -> Vec<String> {
        self.steps
            .iter()
            .map(|step| match &step.upload {
                Some(source) => format!(
                    "{} < {}",
                    ssh.command_line(&step.script),
                    shell_quote(&source.to_string_lossy())
                ),
                None => ssh.command_line(&step.script),
            })
            .collect()
    }
}

/// Only names this module generates reach a script unquoted; refuse anything
/// else rather than quote it. A relative path inside a version directory.
fn check_name(name: &str) -> Result<(), String> {
    if name.split('/').all(|part| check_component(part).is_ok()) {
        Ok(())
    } else {
        Err(format!("unsafe name {name:?} in an install"))
    }
}

/// [`check_name`] for one path component: a version, a nonce or a digest.
fn check_component(name: &str) -> Result<(), String> {
    let safe = !name.is_empty()
        && name != ".."
        && name != "."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    if safe {
        Ok(())
    } else {
        Err(format!("unsafe name {name:?} in an install"))
    }
}

/// The plan that brings a machine in the `probed` state to `bundle`'s version,
/// staging its upload under a directory named for `nonce`.
pub fn plan(bundle: &Bundle, probed: &Probe, nonce: &str) -> Result<Plan, String> {
    if bundle.triple != probed.triple {
        return Err(format!(
            "the machine runs {}, the bundle is {}",
            probed.triple, bundle.triple
        ));
    }
    let version = &bundle.version;
    check_component(version)?;
    check_component(nonce)?;
    for file in &bundle.files {
        check_name(&file.rel)?;
        check_component(&file.sha256)?;
    }
    let host = format!("H=\"$HOME/{HOST_DIR}\"");
    let link = format!("ln -sfn '{version}' \"$H/{CURRENT}\"");
    let mut steps = Vec::new();
    if probed.installed.contains(version) {
        if probed.current.as_deref() != Some(version.as_str()) {
            steps.push(Step::new(format!("set -e\n{host}\n{link}")));
        }
        return Ok(Plan {
            version: version.clone(),
            steps,
        });
    }

    let staging = format!("\"$H\"/{STAGING_PREFIX}{version}-{nonce}");
    let mut dirs: Vec<&str> = bundle
        .files
        .iter()
        .filter_map(|file| file.rel.rsplit_once('/').map(|(dir, _)| dir))
        .collect();
    dirs.sort_unstable();
    dirs.dedup();
    let make_dirs: String = dirs
        .iter()
        .map(|dir| format!("\nmkdir -p {staging}/{dir}"))
        .collect();
    steps.push(Step::new(format!(
        "set -e\numask 077\n{host}\n\
         mkdir -p \"$H\"\n\
         chmod 700 \"$HOME/.codesense\" \"$H\"\n\
         find \"$H\" -maxdepth 1 -name '{STAGING_PREFIX}*' -mmin +{STALE_STAGING_MINUTES} -exec rm -rf {{}} + || true\n\
         mkdir {staging}{make_dirs}"
    )));
    for file in &bundle.files {
        steps.push(Step {
            script: format!("set -e\numask 077\n{host}\ncat > {staging}/{}", file.rel),
            upload: Some(file.source.clone()),
        });
    }
    let sums: String = bundle
        .files
        .iter()
        .map(|file| format!(" '{}  {}'", file.sha256, file.rel))
        .collect();
    let protocol_major = codesense_host_proto::PROTO_MAJOR.to_string();
    let protocol_major_sha = hex(&Sha256::digest(format!("{protocol_major}\n")));
    let sums = format!("{sums} '{protocol_major_sha}  {PROTOCOL_MAJOR_FILE}'");
    steps.push(Step::new(format!(
        "set -e\n{host}\nS={staging}\n\
         cd \"$S\"\n\
         if command -v sha256sum >/dev/null 2>&1; then sum=sha256sum\n\
         elif command -v shasum >/dev/null 2>&1; then sum='shasum -a 256'\n\
         else\n\
         \x20 rm -rf \"$S\"\n\
         \x20 echo 'no sha256sum or shasum on the machine: nothing installed' >&2\n\
         \x20 exit 4\n\
         fi\n\
         printf '%s\\n' '{protocol_major}' > {PROTOCOL_MAJOR_FILE}\n\
         printf '%s\\n'{sums} > .codesense-manifest\n\
         if ! printf '%s\\n'{sums} | $sum -c >/dev/null 2>&1; then\n\
         \x20 rm -rf \"$S\"\n\
         \x20 echo 'sha256 mismatch after upload: nothing installed' >&2\n\
         \x20 exit 3\n\
         fi\n\
         chmod 700 {BINARY} {BRIDGE} {WORKER_BIN}\n\
         rc=0\n\
         ./{BINARY} attach >/dev/null 2>&1 || rc=$?\n\
         if [ \"$rc\" -ne 1 ]; then\n\
         \x20 rm -rf \"$S\"\n\
         \x20 echo \"the host agent does not run here (exit $rc; a noexec home?): nothing installed\" >&2\n\
         \x20 exit 5\n\
         fi\n\
         cd \"$H\"\n\
         if [ -x '{version}/{BINARY}' ] && [ -f '{version}/.codesense-manifest' ] \
            && cmp -s \"$S/.codesense-manifest\" '{version}/.codesense-manifest' \
            && (cd '{version}' && $sum -c .codesense-manifest >/dev/null 2>&1); then\n\
         \x20 rm -rf \"$S\"\n\
         else\n\
         \x20 [ ! -e '{version}' ] || mv '{version}' '{STAGING_PREFIX}broken-{version}-{nonce}'\n\
         \x20 mv \"$S\" '{version}'\n\
         \x20 rm -rf './{version}/{STAGING_PREFIX}{version}-{nonce}' '{STAGING_PREFIX}broken-{version}-{nonce}'\n\
         fi\n\
         {link}"
    )));
    Ok(Plan {
        version: version.clone(),
        steps,
    })
}

/// What an install did, and whether the host it installed outlives logout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub triple: String,
    pub version: String,
    /// Whether the machine changed: false when it was installed and current.
    pub changed: bool,
    pub survival: Survival,
}

/// Probe the machine `ssh` reaches.
pub fn probe(ssh: &Ssh) -> Result<Probe, String> {
    parse_probe(&ssh.run(&probe_step())?)
}

/// A machine probed and its install planned, not yet changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    pub probe: Probe,
    pub plan: Plan,
}

impl Prepared {
    /// Every command [`apply`] runs, in order, as a user would type it: the
    /// plan's steps, then the read-only logout-survival probe.
    pub fn commands(&self, ssh: &Ssh) -> Vec<String> {
        let mut commands = self.plan.commands(ssh);
        commands.push(ssh.command_line(&survival::probe_step().script));
        commands
    }
}

/// Probe the machine `ssh` reaches and plan its install: the artifact for its
/// triple from `artifacts`, with the shell-integration scripts from
/// `shell_init`. The plan is empty when that version is installed and current.
pub fn prepare(ssh: &Ssh, artifacts: &Path, shell_init: &Path) -> Result<Prepared, String> {
    let probe = probe(ssh)?;
    let bundle = Bundle::load(artifacts, shell_init, &probe.triple)?;
    let plan = plan(&bundle, &probe, &nonce())?;
    Ok(Prepared { probe, plan })
}

/// Run exactly [`Prepared::commands`]. Once the version is in place, a
/// failed survival probe reads as [`Survival::Unknown`]: the install stands.
pub fn apply(ssh: &Ssh, prepared: &Prepared) -> Result<Installed, String> {
    for step in &prepared.plan.steps {
        ssh.run(step)?;
    }
    Ok(Installed {
        triple: prepared.probe.triple.clone(),
        version: prepared.plan.version.clone(),
        changed: !prepared.plan.steps.is_empty(),
        survival: survival::probe(ssh).unwrap_or(Survival::Unknown {
            user: String::new(),
            systemd: false,
        }),
    })
}

/// [`prepare`], then [`apply`].
pub fn install(ssh: &Ssh, artifacts: &Path, shell_init: &Path) -> Result<Installed, String> {
    apply(ssh, &prepare(ssh, artifacts, shell_init)?)
}

/// A name no other install's staging directory has.
fn nonce() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("{:x}-{nanos:x}", std::process::id())
}

/// The script that runs the relay of the installed `version`: what a client
/// runs over `ssh` to reach the machine's host.
pub fn attach_script(version: &str) -> Result<String, String> {
    check_component(version)?;
    Ok(format!(
        "exec \"$HOME/{HOST_DIR}/{version}/{BINARY}\" attach --stdio"
    ))
}

/// The only remote command permitted to enter the destructive replacement
/// path. The caller chooses an already-validated installed version; all
/// process authority is supplied separately over bounded stdin.
pub fn replace_script(version: &str) -> Result<String, String> {
    check_component(version)?;
    Ok(format!(
        "exec \"$HOME/{HOST_DIR}/{version}/{BINARY}\" replace"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probed(installed: &[&str], current: Option<&str>) -> Probe {
        Probe {
            triple: "x86_64-unknown-linux-musl".into(),
            current: current.map(String::from),
            installed: installed.iter().map(|v| v.to_string()).collect(),
        }
    }

    fn bundle(dir: &Path) -> Bundle {
        let file = |rel: &str, body: &str| {
            let source = dir.join(rel.replace('/', "_"));
            std::fs::write(&source, body).unwrap();
            BundleFile {
                rel: rel.into(),
                sha256: sha256_of(&source).unwrap(),
                source,
            }
        };
        Bundle::of(
            "x86_64-unknown-linux-musl",
            vec![
                file(BINARY, "binary"),
                file(
                    "resources/shell-integration/codesense-shell-init.zsh",
                    "zsh",
                ),
            ],
        )
    }

    #[test]
    fn uname_names_the_triple() {
        assert_eq!(
            triple_for("Linux x86_64\n"),
            Ok("x86_64-unknown-linux-musl")
        );
        assert_eq!(
            triple_for("Linux aarch64"),
            Ok("aarch64-unknown-linux-musl")
        );
        assert!(
            triple_for("Darwin arm64").is_err(),
            "no macOS host agent ships"
        );
        assert!(triple_for("Linux riscv64").is_err());
        assert!(triple_for("").is_err());
    }

    #[test]
    fn a_probe_reads_uname_current_and_the_installed_versions() {
        let probe = parse_probe(
            "uname=Linux aarch64\ncurrent=0.1.0-abc\ninstalled=0.1.0-abc\ninstalled=0.1.0-def\n",
        )
        .unwrap();
        assert_eq!(probe.triple, "aarch64-unknown-linux-musl");
        assert_eq!(probe.current.as_deref(), Some("0.1.0-abc"));
        assert_eq!(probe.installed, ["0.1.0-abc", "0.1.0-def"]);

        let fresh = parse_probe("uname=Linux x86_64\ncurrent=\n").unwrap();
        assert_eq!(fresh.current, None);
        assert!(fresh.installed.is_empty());

        assert!(parse_probe("").is_err(), "no answer is no probe");
    }

    /// Each artifact must match the sha256 recorded next to it; the bundle
    /// carries the binary, the emulator worker and its notices, and the
    /// shell-integration scripts, under a version named for their content.
    #[test]
    fn a_bundle_is_its_verified_artifacts_and_the_shell_init_scripts() {
        let dir = tempfile::tempdir().unwrap();
        let triple = "aarch64-unknown-linux-musl";
        let artifact = dir.path().join(artifact_name(triple));
        std::fs::write(&artifact, b"elf").unwrap();
        let sha = sha256_of(&artifact).unwrap();
        std::fs::write(digest_file(&artifact), format!("{sha}  x\n")).unwrap();
        let bridge = dir.path().join(bridge_artifact_name(triple));
        std::fs::write(&bridge, b"bridge").unwrap();
        let bridge_sha = sha256_of(&bridge).unwrap();
        std::fs::write(digest_file(&bridge), format!("{bridge_sha}  bridge\n")).unwrap();
        let worker = dir.path().join(emulator_artifact_name(triple));
        std::fs::write(&worker, b"bun").unwrap();
        let worker_sha = sha256_of(&worker).unwrap();
        std::fs::write(digest_file(&worker), format!("{worker_sha}  y\n")).unwrap();
        std::fs::write(dir.path().join(EMULATOR_NOTICES), "MIT").unwrap();
        for script in SHELL_INIT_SCRIPTS {
            std::fs::write(dir.path().join(script), script).unwrap();
        }

        let bundle = Bundle::load(dir.path(), dir.path(), triple).unwrap();
        let rels: Vec<&str> = bundle.files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(
            rels,
            [
                BINARY,
                BRIDGE,
                "codesense-emulator",
                "resources/emulator/NOTICE.md",
                "resources/shell-integration/codesense-shell-init.zsh",
                "resources/shell-integration/codesense-shell-init.bash",
                "resources/shell-integration/codesense-shell-init.fish",
            ]
        );
        assert_eq!(bundle.files[0].sha256, sha);
        assert_eq!(bundle.files[1].sha256, bridge_sha);
        assert_eq!(bundle.files[2].sha256, worker_sha);
        assert!(bundle
            .version
            .starts_with(concat!(env!("CARGO_PKG_VERSION"), "-")));

        std::fs::write(dir.path().join("codesense-shell-init.fish"), "changed").unwrap();
        let changed = Bundle::load(dir.path(), dir.path(), triple).unwrap();
        assert_ne!(
            changed.version, bundle.version,
            "the version follows content"
        );

        std::fs::write(&worker, b"tampered").unwrap();
        let err = Bundle::load(dir.path(), dir.path(), triple).unwrap_err();
        assert!(err.contains("does not match its recorded sha256"), "{err}");
        std::fs::write(&worker, b"bun").unwrap();

        std::fs::write(&artifact, b"tampered").unwrap();
        let err = Bundle::load(dir.path(), dir.path(), triple).unwrap_err();
        assert!(err.contains("does not match its recorded sha256"), "{err}");
    }

    /// A fresh machine: prepare staging, upload each file, verify and move into
    /// place, point `current`. Uploads never write the version directory.
    #[test]
    fn a_missing_version_is_staged_verified_and_moved_into_place() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = bundle(dir.path());
        let plan = plan(&bundle, &probed(&[], None), "n1").unwrap();
        assert_eq!(plan.steps.len(), 1 + bundle.files.len() + 1);
        let staging = format!(".incoming-{}-n1", bundle.version);
        for (step, file) in plan.steps[1..=bundle.files.len()].iter().zip(&bundle.files) {
            assert_eq!(step.upload.as_ref(), Some(&file.source));
            assert!(step
                .script
                .contains(&format!("cat > \"$H\"/{staging}/{}", file.rel)));
        }
        let last = &plan.steps.last().unwrap().script;
        for file in &bundle.files {
            assert!(last.contains(&format!("'{}  {}'", file.sha256, file.rel)));
        }
        let verify = last.find("-c >/dev/null").unwrap();
        let rename = last
            .find(&format!("mv \"$S\" '{}'", bundle.version))
            .unwrap();
        let link = last.find("ln -sfn").unwrap();
        assert!(
            verify < rename && rename < link,
            "verify, then rename, then link"
        );
        assert!(plan.steps.iter().all(|step| !step.script.contains("sudo")));
    }

    /// A re-run is a no-op; a version installed but not current is only
    /// linked.
    #[test]
    fn an_installed_version_changes_nothing_or_only_the_link() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = bundle(dir.path());
        let v = bundle.version.as_str();
        assert!(plan(&bundle, &probed(&[v], Some(v)), "n")
            .unwrap()
            .steps
            .is_empty());
        let relink = plan(&bundle, &probed(&[v, "old"], Some("old")), "n").unwrap();
        assert_eq!(relink.steps.len(), 1);
        assert!(relink.steps[0].script.contains("ln -sfn"));
        assert!(relink.steps[0].upload.is_none());
    }

    #[test]
    fn a_plan_refuses_another_triple_and_unsafe_names() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = bundle(dir.path());
        let mut other = probed(&[], None);
        other.triple = "aarch64-unknown-linux-musl".into();
        assert!(plan(&bundle, &other, "n").is_err());
        assert!(plan(&bundle, &probed(&[], None), "n; rm -rf ~").is_err());
        assert!(
            plan(&bundle, &probed(&[], None), "a/b").is_err(),
            "a nonce is one component"
        );
        let mut nested = bundle.clone();
        nested.version = "0.1.0/x".into();
        assert!(plan(&nested, &probed(&[], None), "n").is_err());
        assert!(attach_script("../x").is_err());
        assert!(replace_script("../x").is_err());
        let mut escaping = bundle.clone();
        escaping.files[0].rel = "../../.bashrc".into();
        assert!(plan(&escaping, &probed(&[], None), "n").is_err());
    }

    #[test]
    fn rm4d_replacement_is_the_fixed_installed_verb() {
        assert_eq!(
            replace_script("0.1.0-abc").unwrap(),
            "exec \"$HOME/.codesense/host/0.1.0-abc/codesense-session-server\" replace"
        );
    }

    /// "Show commands" lists every command, runnable as shown.
    #[test]
    fn the_commands_are_what_a_user_would_type() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = bundle(dir.path());
        let ssh = Ssh::new("ssh", vec!["-p".into(), "2222".into(), "dev@box".into()]);
        let plan = plan(&bundle, &probed(&[], None), "n").unwrap();
        let commands = plan.commands(&ssh);
        assert_eq!(commands.len(), plan.steps.len());
        assert!(commands[0].starts_with("ssh -p 2222 dev@box 'sh -c '\\''set -e"));
        assert!(commands[1].ends_with(&format!(
            " < '{}'",
            bundle.files[0].source.to_string_lossy()
        )));
    }

    #[test]
    fn quoting_survives_single_quotes() {
        let out = Command::new("sh")
            .arg("-c")
            .arg(format!("printf %s {}", shell_quote("it's \"$HOME\" `x`")))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "it's \"$HOME\" `x`");
    }
}
