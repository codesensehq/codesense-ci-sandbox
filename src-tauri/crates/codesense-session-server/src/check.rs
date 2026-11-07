// SPDX-License-Identifier: MIT
//! The Add-machine checklist: can this app put its terminal server on a
//! machine and reach it? Each step says what it found and, when it failed,
//! what the person can do next:
//!
//! 1. **reachable** — the machine answers (and presents a host key);
//! 2. **host key** — that key is the pinned one, or one the person trusted;
//!    a first sight asks for trust, a different key than the pin is blocked;
//! 3. **auth** — the bound sign-in is accepted;
//! 4. **platform** — a terminal server is built for its OS and CPU;
//! 5. **data dir** — `~/.codesense` can be written;
//! 6. **terminal server** — this app's version is installed; when it is not,
//!    the report carries the install that would put it there.
//!
//! A check changes nothing on the machine. Once it passes,
//! [`read_server_id`] greets the machine's terminal server over
//! `attach --stdio` for the identity a client pins ([`check_server_id`]).

use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use codesense_host_proto::{read_hello, PeerHello};

use crate::askpass::Askpass;
use crate::install::{self, attach_script, Bundle, Probe, Step as SshStep};
use crate::ssh::{Connector, Failure, HostKey, Mux};

/// The least free space `~/.codesense`'s disk must have: room for one
/// terminal server version and its logs.
const MIN_FREE_KB: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepId {
    Reachable,
    HostKey,
    Auth,
    Platform,
    DataDir,
    TerminalServer,
    /// Advisory runtime prerequisites do not block using another CLI.
    AgentPrerequisite,
}

const ORDER: [StepId; 6] = [
    StepId::Reachable,
    StepId::HostKey,
    StepId::Auth,
    StepId::Platform,
    StepId::DataDir,
    StepId::TerminalServer,
];

/// How a step came out. `Blocked` is a host key that differs from the pin,
/// with no way past it; `Skipped` a step an earlier one stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    NeedsTrust,
    Warn,
    Fail,
    Blocked,
    Skipped,
}

/// What the person can do about a failed step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextStep {
    EditAddress,
    UseJumpHost,
    Retry,
    ChooseKey,
    ChangeUser,
    CopyPublicKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub id: StepId,
    pub status: Status,
    pub detail: String,
    pub next_steps: Vec<NextStep>,
    /// The host-key step's presented fingerprint.
    pub fingerprint: Option<String>,
}

impl Step {
    fn new(id: StepId, status: Status, detail: impl Into<String>) -> Self {
        Self {
            id,
            status,
            detail: detail.into(),
            next_steps: Vec::new(),
            fingerprint: None,
        }
    }

    fn next(mut self, next: &[NextStep]) -> Self {
        self.next_steps = next.to_vec();
        self
    }
}

/// The install the terminal-server step offers: what it uploads, where, and
/// exactly the commands it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallOffer {
    pub size_bytes: u64,
    pub target_dir: String,
    pub commands: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub steps: Vec<Step>,
    pub install: Option<InstallOffer>,
    /// The machine's OS has no terminal server: it can only be kept for later.
    pub file_view_only: bool,
    /// The machine's key, once the host-key step passed.
    pub host_key: Option<HostKey>,
    /// This app's terminal-server version, once it is installed there.
    pub version: Option<String>,
}

impl Report {
    /// Whether every required step passed: runtime advisories do not block connection.
    pub fn passed(&self) -> bool {
        self.first_problem().is_none()
    }

    /// The first required step that did not pass.
    pub fn first_problem(&self) -> Option<&Step> {
        self.steps.iter().find(|s| {
            s.status != Status::Ok
                && !(s.id == StepId::AgentPrerequisite && s.status == Status::Warn)
        })
    }

    fn done(mut self) -> Self {
        for id in ORDER {
            if !self.steps.iter().any(|s| s.id == id) {
                self.steps.push(Step::new(id, Status::Skipped, ""));
            }
        }
        self
    }
}

/// Which host key the machine must present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust<'a> {
    /// A saved machine's pin (the person's, or the team's): nothing else passes.
    Pinned(&'a HostKey),
    /// A machine being added: the fingerprint the person trusted, if any.
    Trusted(Option<&'a str>),
}

/// Where this app's terminal-server builds are.
#[derive(Debug, Clone, Copy)]
pub struct Bundled<'a> {
    pub artifacts: &'a Path,
    pub shell_init: &'a Path,
}

/// The one script the auth, platform and data-dir steps share: whether
/// `~/.codesense` can be written (without creating it), the free space, who
/// signed in, then the install probe.
fn probe_script() -> String {
    format!(
        "d=\"$HOME/.codesense\"\n\
         if [ -d \"$d\" ]; then [ -w \"$d\" ] && echo data=ok || echo data=unwritable\n\
         elif [ -w \"$HOME\" ]; then echo data=ok\n\
         else echo data=home-unwritable; fi\n\
         printf 'free=%s\\n' \"$(df -Pk \"$HOME\" 2>/dev/null | awk 'NR==2 {{print $4}}')\"\n\
         printf 'who=%s\\n' \"$(id -un 2>/dev/null)\"\n\
         {}\n{}",
        codesense_pty_core::agent_cli::machine_probe(),
        install::probe_step().script
    )
}

fn runtime_steps(output: &str) -> Vec<Step> {
    codesense_pty_core::agent_cli::machine_warnings(output)
        .into_iter()
        .map(|message| Step::new(StepId::AgentPrerequisite, Status::Warn, message))
        .collect()
}

/// Reads runtime prerequisites after the account/host key has been verified. Never writes.
pub fn runtime_checks(ssh: &install::Ssh) -> Vec<Step> {
    let script = codesense_pty_core::agent_cli::machine_probe();
    match ssh.output_within(&SshStep::new(script), PROBE_TIMEOUT) {
        Ok(output) if output.status.success() => runtime_steps(&String::from_utf8_lossy(&output.stdout)),
        _ => vec![Step::new(StepId::AgentPrerequisite, Status::Warn, "Runtime prerequisites could not be checked; test the connection again before launching an agent")],
    }
}

/// The value of `key=` in a probe's output.
fn field<'a>(output: &'a str, key: &str) -> Option<&'a str> {
    output
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
}

/// Run the checklist against the machine `connector` reaches. `scratch` holds
/// the host-key scan's throwaway file.
pub fn run(
    connector: &Connector,
    trust: Trust,
    askpass: Option<&Askpass>,
    bundled: Bundled,
    scratch: &Path,
) -> Report {
    let mut report = Report {
        steps: Vec::new(),
        install: None,
        file_view_only: false,
        host_key: None,
        version: None,
    };
    let address = connector.address();
    let started = Instant::now();
    let key = match connector.scan(scratch, pinned(trust)) {
        Ok(key) => key,
        Err(Failure::HostKeyChanged) => {
            report.steps.push(Step::new(
                StepId::Reachable,
                Status::Ok,
                format!("Reachable ({} ms)", started.elapsed().as_millis()),
            ));
            report.steps.push(Step::new(
                StepId::HostKey,
                Status::Blocked,
                "The machine no longer presents the pinned host-key algorithm",
            ));
            return report.done();
        }
        Err(failure) => {
            report
                .steps
                .push(unreachable_step(&failure, address.jump.is_some()));
            return report.done();
        }
    };
    report.steps.push(Step::new(
        StepId::Reachable,
        Status::Ok,
        format!("Reachable ({} ms)", started.elapsed().as_millis()),
    ));

    let host_step = host_key_step(trust, &key, &address.host);
    let trusted = host_step.status == Status::Ok;
    report.steps.push(host_step);
    if !trusted {
        return report.done();
    }
    if let Err(e) = connector.pin(&key) {
        report
            .steps
            .push(Step::new(StepId::Auth, Status::Fail, e).next(&[NextStep::Retry]));
        return report.done();
    }
    report.host_key = Some(key);

    let ssh = connector.ssh(Mux::Fresh, askpass);
    let output = match ssh.output_within(&SshStep::new(probe_script()), PROBE_TIMEOUT) {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).into_owned()
        }
        Ok(output) => {
            let failure = crate::ssh::classify(&String::from_utf8_lossy(&output.stderr));
            report.steps.push(auth_step(&failure));
            return report.done();
        }
        Err(e) => {
            report.steps.push(
                Step::new(
                    StepId::Auth,
                    Status::Fail,
                    format!(
                        "The machine didn't finish the check ({e}): a slow login shell or disk?"
                    ),
                )
                .next(&[NextStep::Retry]),
            );
            return report.done();
        }
    };
    let who = field(&output, "who").unwrap_or_default();
    report.steps.push(Step::new(
        StepId::Auth,
        Status::Ok,
        format!("Signed in as {who}"),
    ));

    let uname = field(&output, "uname")
        .unwrap_or_default()
        .trim()
        .to_string();
    let probe = match install::parse_probe(&output) {
        Ok(probe) => probe,
        Err(_) if uname.is_empty() => {
            report.steps.push(
                Step::new(
                    StepId::Platform,
                    Status::Fail,
                    "The machine didn't say what it runs (its shell printed something else)",
                )
                .next(&[NextStep::Retry]),
            );
            return report.done();
        }
        Err(_) => {
            report.file_view_only = true;
            report.steps.push(Step::new(
                StepId::Platform,
                Status::Warn,
                format!(
                    "{} can't run the terminal server yet — file view only (coming later)",
                    platform_name(&uname)
                ),
            ));
            return report.done();
        }
    };
    report.steps.push(Step::new(
        StepId::Platform,
        Status::Ok,
        platform_name(&uname),
    ));

    report.steps.extend(runtime_steps(&output));
    let data = data_dir_step(&output);
    let data_ok = data.status == Status::Ok;
    report.steps.push(data);
    if !data_ok {
        return report.done();
    }

    let server = terminal_server_step(&mut report, &probe, bundled, &ssh);
    report.steps.push(server);
    report.done()
}

/// The data-dir step, from the probe's output: `~/.codesense` writable (or
/// creatable), with measured room for a terminal server. Missing or malformed
/// free-space data is a failure, never assumed safe.
fn data_dir_step(output: &str) -> Step {
    let free_kb = field(output, "free").and_then(|v| v.trim().parse::<u64>().ok());
    let step = match (field(output, "data"), free_kb) {
        (Some("ok"), Some(free)) if free < MIN_FREE_KB => Step::new(
            StepId::DataDir,
            Status::Fail,
            format!("~/.codesense's disk is full ({} MB free)", free / 1024),
        ),
        (Some("ok"), Some(_)) => {
            return Step::new(StepId::DataDir, Status::Ok, "~/.codesense is writable")
        }
        (Some("ok"), None) => Step::new(
            StepId::DataDir,
            Status::Fail,
            "The machine didn't report usable free-space data for ~/.codesense",
        ),
        (Some("home-unwritable"), _) => Step::new(
            StepId::DataDir,
            Status::Fail,
            "Your home directory isn't writable, so ~/.codesense can't be created",
        ),
        _ => Step::new(StepId::DataDir, Status::Fail, "~/.codesense isn't writable"),
    };
    // Fixed on the machine, then checked again.
    step.next(&[NextStep::Retry])
}

/// The pin a scan must present a key of the same type as.
fn pinned<'a>(trust: Trust<'a>) -> Option<&'a HostKey> {
    match trust {
        Trust::Pinned(pinned) => Some(pinned),
        Trust::Trusted(_) => None,
    }
}

/// The host-key step: `key` held to `trust`.
fn host_key_step(trust: Trust, key: &HostKey, host: &str) -> Step {
    let presented = key.fingerprint();
    let mut step = match trust {
        Trust::Pinned(pinned) if pinned == key => Step::new(
            StepId::HostKey,
            Status::Ok,
            format!("Host key {presented} matches the pin"),
        ),
        Trust::Pinned(pinned) => Step::new(
            StepId::HostKey,
            Status::Blocked,
            format!(
                "Host key changed: pinned {}, the machine now presents {presented}",
                pinned.fingerprint()
            ),
        ),
        Trust::Trusted(Some(fp)) if fp == presented => Step::new(
            StepId::HostKey,
            Status::Ok,
            format!("Host key {presented} trusted"),
        ),
        Trust::Trusted(Some(fp)) => Step::new(
            StepId::HostKey,
            Status::NeedsTrust,
            format!(
                "{host} now presents a different key than the {fp} you trusted. Check it \
                 before trusting it."
            ),
        ),
        Trust::Trusted(_) => Step::new(
            StepId::HostKey,
            Status::NeedsTrust,
            format!("First time seeing {host}. Check this fingerprint before trusting it."),
        ),
    };
    step.fingerprint = Some(presented);
    step
}

/// Scan the machine and hold its key to `trust`. When it passes, the key is
/// pinned for the machine's connections and returned; otherwise, why not.
pub fn verified_host_key(
    connector: &Connector,
    trust: Trust,
    scratch: &Path,
) -> Result<HostKey, String> {
    let key = connector
        .scan(scratch, pinned(trust))
        .map_err(|failure| failure.to_string())?;
    let step = host_key_step(trust, &key, &connector.address().host);
    if step.status != Status::Ok {
        return Err(step.detail);
    }
    connector.pin(&key)?;
    Ok(key)
}

/// `Linux x86_64` → `linux-x86_64`.
fn platform_name(uname: &str) -> String {
    let name = uname
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
        .to_lowercase();
    if name.is_empty() {
        "This machine".into()
    } else {
        name
    }
}

fn terminal_server_step(
    report: &mut Report,
    probe: &Probe,
    bundled: Bundled,
    ssh: &install::Ssh,
) -> Step {
    let bundle = match Bundle::load(bundled.artifacts, bundled.shell_init, &probe.triple) {
        Ok(bundle) => bundle,
        Err(e) => {
            return Step::new(
                StepId::TerminalServer,
                Status::Fail,
                format!(
                    "This app carries no terminal server for {}: {e}",
                    probe.triple
                ),
            )
            .next(&[NextStep::Retry])
        }
    };
    if probe.installed.contains(&bundle.version) {
        report.version = Some(bundle.version.clone());
        return Step::new(
            StepId::TerminalServer,
            Status::Ok,
            format!("Terminal server {} installed", bundle.version),
        );
    }
    let detail = match probe
        .current
        .as_deref()
        .or(probe.installed.first().map(String::as_str))
    {
        Some(other) => format!(
            "Terminal server {other} found — this app needs {}",
            bundle.version
        ),
        None => "Terminal server not installed".to_string(),
    };
    let size_bytes = bundle
        .files
        .iter()
        .filter_map(|f| std::fs::metadata(&f.source).ok())
        .map(|m| m.len())
        .sum();
    match install::plan(&bundle, probe, "check") {
        Ok(plan) => {
            let prepared = install::Prepared {
                probe: probe.clone(),
                plan,
            };
            report.install = Some(InstallOffer {
                size_bytes,
                target_dir: format!("~/.codesense/host/{}", bundle.version),
                commands: prepared.commands(ssh),
            });
            Step::new(StepId::TerminalServer, Status::Warn, detail)
        }
        Err(e) => Step::new(StepId::TerminalServer, Status::Fail, e).next(&[NextStep::Retry]),
    }
}

fn unreachable_step(failure: &Failure, via_jump: bool) -> Step {
    let mut next = vec![NextStep::EditAddress];
    if !via_jump {
        next.push(NextStep::UseJumpHost);
    }
    next.push(NextStep::Retry);
    let detail = match failure {
        Failure::Unresolved(e) | Failure::Unreachable(e) => e.clone(),
        other => other.to_string(),
    };
    Step::new(StepId::Reachable, Status::Fail, detail).next(&next)
}

fn auth_step(failure: &Failure) -> Step {
    match failure {
        Failure::AuthFailed(reason) => Step::new(
            StepId::Auth,
            Status::Fail,
            format!("Sign-in refused: {reason}"),
        )
        .next(&[
            NextStep::ChooseKey,
            NextStep::ChangeUser,
            NextStep::CopyPublicKey,
        ]),
        Failure::HostKeyChanged => Step::new(
            StepId::Auth,
            Status::Blocked,
            "The machine's host key changed during the check",
        ),
        other => Step::new(StepId::Auth, Status::Fail, other.to_string()).next(&[NextStep::Retry]),
    }
}

/// How long the auth/platform/data-dir probe may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);

/// How long greeting a terminal server may take, including starting it.
pub const GREETING_TIMEOUT: Duration = Duration::from_secs(20);

/// Greet the terminal server of `version` on the machine `ssh` reaches (the
/// relay starts it when none runs) and return its identity.
pub fn read_server_id(
    ssh: &install::Ssh,
    version: &str,
    timeout: Duration,
) -> Result<String, String> {
    read_server_id_script(ssh, &attach_script(version)?, timeout)
}

/// Fixed backend-built relay entry, sharing the same greeting/reap boundary.
pub fn read_server_id_script(
    ssh: &install::Ssh,
    script: &str,
    timeout: Duration,
) -> Result<String, String> {
    let mut child = ssh
        .command(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("run ssh: {e}"))?;
    let mut stdout = child.stdout.take().ok_or("no relay output")?;
    let mut stderr = child.stderr.take().ok_or("no relay errors")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read_hello(&mut stdout));
    });
    // Drained as it comes, so a chatty relay never blocks on a full pipe.
    let said = std::thread::spawn(move || {
        let mut said = String::new();
        let _ = stderr.read_to_string(&mut said);
        said
    });
    let result = rx.recv_timeout(timeout);
    let _ = child.kill();
    let _ = child.wait();
    match result {
        Ok(Ok(PeerHello::Compatible(hello))) if !hello.server_id.is_empty() => Ok(hello.server_id),
        Ok(Ok(PeerHello::Compatible(_))) => Err("the terminal server has no identity".into()),
        Ok(Ok(PeerHello::Incompatible { proto_major, .. })) => Err(format!(
            "the terminal server speaks protocol {proto_major}; this app speaks {}",
            codesense_host_proto::PROTO_MAJOR
        )),
        Ok(Err(e)) => {
            let said = said.join().unwrap_or_default();
            Err(match crate::ssh::classify(&said) {
                Failure::Other(said) => format!("the terminal server did not greet: {e} ({said})"),
                failure => failure.to_string(),
            })
        }
        Err(_) => Err("the terminal server did not greet in time".into()),
    }
}

/// A terminal server's identity against the one pinned for this machine: a
/// different one at a pinned machine is a hard stop, like a changed host key.
pub fn check_server_id(pinned: Option<&str>, presented: &str) -> Result<(), String> {
    match pinned {
        Some(pinned) if pinned != presented => Err(format!(
            "A different terminal server answers on this machine (pinned {pinned}, now \
             {presented}). It is blocked: nothing connects until whoever manages it \
             confirms the change."
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cx2_review_runtime_advisory_and_failure_have_consistent_readiness() {
        let mut report = Report {
            steps: ORDER
                .into_iter()
                .map(|id| Step::new(id, Status::Ok, "ok"))
                .collect(),
            install: None,
            file_view_only: false,
            host_key: None,
            version: None,
        };
        report.steps.push(Step::new(
            StepId::AgentPrerequisite,
            Status::Warn,
            "optional runtime",
        ));
        assert!(report.passed());
        assert!(report.first_problem().is_none());
        report.steps.last_mut().unwrap().status = Status::Fail;
        assert!(!report.passed());
        assert!(report.first_problem().is_some());
    }

    #[test]
    fn cx2_machine_report_warns_with_the_sandbox_fix() {
        let warnings = runtime_steps("agent_check_0=1\n");
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].status, Status::Warn);
        assert!(warnings[0].detail.contains("sudo sysctl --system"));
        assert!(runtime_steps("agent_check_0=0\n").is_empty());
        assert!(runtime_steps("").is_empty());
    }

    #[test]
    fn a_report_lists_every_step_in_order() {
        let report = Report {
            steps: vec![Step::new(StepId::Reachable, Status::Fail, "down")],
            install: None,
            file_view_only: false,
            host_key: None,
            version: None,
        }
        .done();
        let ids: Vec<StepId> = report.steps.iter().map(|s| s.id).collect();
        assert_eq!(ids, ORDER);
        assert!(report.steps[1..]
            .iter()
            .all(|s| s.status == Status::Skipped));
        assert!(!report.passed());
        assert_eq!(report.first_problem().unwrap().id, StepId::Reachable);
    }

    #[test]
    fn unreachable_offers_the_address_a_jump_host_and_retry() {
        let step = unreachable_step(&Failure::Unreachable("timed out".into()), false);
        assert_eq!(
            step.next_steps,
            [
                NextStep::EditAddress,
                NextStep::UseJumpHost,
                NextStep::Retry
            ]
        );
        let via_jump = unreachable_step(&Failure::Unresolved("no such host".into()), true);
        assert_eq!(
            via_jump.next_steps,
            [NextStep::EditAddress, NextStep::Retry]
        );
        assert_eq!(via_jump.detail, "no such host");
    }

    #[test]
    fn a_refused_sign_in_offers_a_key_a_user_and_the_public_key() {
        let step = auth_step(&Failure::AuthFailed(
            "Permission denied (publickey).".into(),
        ));
        assert_eq!(step.status, Status::Fail);
        assert_eq!(
            step.next_steps,
            [
                NextStep::ChooseKey,
                NextStep::ChangeUser,
                NextStep::CopyPublicKey
            ]
        );
        assert_eq!(auth_step(&Failure::HostKeyChanged).status, Status::Blocked);
    }

    #[test]
    fn the_probe_script_reads_back_its_fields() {
        let script = probe_script();
        assert!(script.contains("uname -sm"), "it carries the install probe");
        let out = "data=ok\nfree=1024\nwho=dev\nuname=Linux x86_64\ncurrent=\n";
        assert_eq!(field(out, "who"), Some("dev"));
        assert_eq!(field(out, "free"), Some("1024"));
        assert_eq!(field(out, "missing"), None);
        assert_eq!(platform_name("Linux x86_64"), "linux-x86_64");
        assert_eq!(platform_name("FreeBSD amd64"), "freebsd-amd64");
    }

    #[test]
    fn the_data_dir_step_reads_space_and_writability() {
        let step = |out: &str| data_dir_step(out);
        assert_eq!(step("data=ok\nfree=10485760\n").status, Status::Ok);
        assert_eq!(
            step("data=ok\n").status,
            Status::Fail,
            "missing free-space evidence is unsafe"
        );
        assert_eq!(step("data=ok\nfree=not-a-number\n").status, Status::Fail);
        let full = step("data=ok\nfree=2048\n");
        assert_eq!(full.status, Status::Fail);
        assert!(
            full.detail.contains("disk is full (2 MB free)"),
            "{}",
            full.detail
        );
        assert_eq!(full.next_steps, [NextStep::Retry]);
        let home = step("data=home-unwritable\nfree=10485760\n");
        assert!(home.detail.contains("home directory isn't writable"));
        assert_eq!(
            step("data=unwritable\n").detail,
            "~/.codesense isn't writable"
        );
    }

    #[test]
    fn the_fingerprint_shown_at_first_sight_is_the_one_that_verifies() {
        let key = HostKey::parse(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl",
        )
        .unwrap();
        let shown = host_key_step(Trust::Trusted(None), &key, "box")
            .fingerprint
            .unwrap();
        let trusted = host_key_step(Trust::Trusted(Some(&shown)), &key, "box");
        assert_eq!(trusted.status, Status::Ok, "{}", trusted.detail);
    }

    #[test]
    fn a_key_that_changed_after_trust_is_shown_again() {
        let key = HostKey::parse(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl",
        )
        .unwrap();
        let step = host_key_step(Trust::Trusted(Some("SHA256:old")), &key, "box");
        assert_eq!(step.status, Status::NeedsTrust);
        assert!(step.detail.contains("SHA256:old"), "{}", step.detail);
        let other = HostKey::parse("ssh-ed25519 AAAA").unwrap();
        assert_eq!(
            host_key_step(Trust::Pinned(&other), &key, "box").status,
            Status::Blocked
        );
    }

    #[test]
    fn a_different_server_at_a_pinned_machine_is_a_hard_stop() {
        assert!(check_server_id(None, "srv-a").is_ok(), "first sight pins");
        assert!(check_server_id(Some("srv-a"), "srv-a").is_ok());
        let err = check_server_id(Some("srv-a"), "srv-b").unwrap_err();
        assert!(err.contains("srv-a") && err.contains("srv-b"), "{err}");
    }
}
