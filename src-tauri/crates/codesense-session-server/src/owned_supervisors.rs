// SPDX-License-Identifier: MIT
//! Linux-only ownership of remote stdio instances. This is process lifecycle
//! machinery: MCP bytes flow directly between the native client and its server.
use crate::host::lock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

pub(crate) const DRAIN_TIME: Duration = Duration::from_secs(2);
const MAX_INSTANCES: usize = 32;
const MAX_CHILDREN: usize = 128;
pub(crate) const MAX_CONFIG_BYTES: usize = 256 * 1024;
static TERMINATED: AtomicBool = AtomicBool::new(false);
extern "C" fn termination_signal(_: libc::c_int) {
    TERMINATED.store(true, Ordering::Relaxed);
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ResolvedServer {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: std::collections::BTreeMap<String, String>,
}

pub(crate) struct SessionIncarnation<'a> {
    pub session: &'a str,
    pub nonce: &'a str,
}

struct Instance {
    control: UnixStream,
    result: Arc<Mutex<Option<Result<(), String>>>>,
}
struct Session {
    incarnation: String,
    draining: bool,
    instances: Vec<Instance>,
}
#[derive(Default)]
pub(crate) struct OwnedSupervisors {
    sessions: Mutex<HashMap<String, Session>>,
}
impl OwnedSupervisors {
    pub(crate) fn provision(&self, session: &str, incarnation: &str) -> Result<(), String> {
        let mut sessions = lock(&self.sessions);
        if let Some(old) = sessions.get(session) {
            if old.incarnation == incarnation && !old.draining {
                return Ok(());
            }
            if !old.draining
                || old
                    .instances
                    .iter()
                    .any(|i| !matches!(&*lock(&i.result), Some(Ok(()))))
            {
                return Err("remote MCP previous session cleanup has not settled".into());
            }
        }
        if !sessions.contains_key(session) && sessions.len() >= 256 {
            return Err("remote MCP session ownership limit reached".into());
        }
        sessions.insert(
            session.into(),
            Session {
                incarnation: incarnation.into(),
                draining: false,
                instances: vec![],
            },
        );
        Ok(())
    }

    /// Gate check and retention are atomic with End. The peer has not spawned
    /// anything yet; failure or lost acknowledgement therefore launches nothing.
    pub(crate) fn register(
        &self,
        identity: SessionIncarnation<'_>,
        mut control: UnixStream,
        supervisor: OwnedFd,
        client: &OwnedFd,
        config: &ResolvedServer,
        live: bool,
    ) -> Result<(), String> {
        let SessionIncarnation {
            session,
            nonce: incarnation,
        } = identity;
        let bytes =
            serde_json::to_vec(config).map_err(|_| "remote MCP configuration unavailable")?;
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err("remote MCP resolved configuration exceeds limit".into());
        }
        let mut sessions = lock(&self.sessions);
        let owner = sessions
            .get_mut(session)
            .filter(|s| s.incarnation == incarnation && !s.draining)
            .ok_or("remote MCP session is draining")?;
        owner
            .instances
            .retain(|instance| !matches!(&*lock(&instance.result), Some(Ok(()))));
        if owner.instances.len() >= MAX_INSTANCES || !live {
            return Err("remote MCP registration unavailable".into());
        }
        let result = Arc::new(Mutex::new(None));
        let retained = control
            .try_clone()
            .map_err(|_| "remote MCP control unavailable")?;
        // Install before acknowledging. Even a lost response remains owned.
        owner.instances.push(Instance {
            control: retained,
            result: result.clone(),
        });
        drop(sessions);
        let registration = (|| {
            codesense_host_proto::broker::send_fd(&control, client.as_raw_fd())
                .map_err(|_| "remote MCP lifetime unavailable")?;
            control
                .write_all(&(bytes.len() as u32).to_be_bytes())
                .and_then(|_| control.write_all(&bytes))
                .map_err(|_| "remote MCP registration acknowledgement unavailable")
        })();
        if registration.is_err() {
            let _ = control.shutdown(std::net::Shutdown::Both);
        }
        std::thread::spawn(move || {
            let _ = control.set_read_timeout(None);
            let mut byte = [0];
            let clean = control.read_exact(&mut byte).is_ok() && byte == [b'C'];
            let deadline = Instant::now() + DRAIN_TIME;
            while !exited(&supervisor) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            *lock(&result) = Some(if clean && exited(&supervisor) {
                Ok(())
            } else {
                Err("remote MCP supervisor cleanup incomplete".into())
            });
        });
        registration.map_err(str::to_owned)
    }

    pub(crate) fn settled(&self, session: &str, incarnation: &str) -> bool {
        lock(&self.sessions).get(session).is_some_and(|owner| {
            owner.incarnation == incarnation
                && owner.draining
                && owner
                    .instances
                    .iter()
                    .all(|i| matches!(&*lock(&i.result), Some(Ok(()))))
        })
    }

    pub(crate) fn forget_settled(&self, session: &str, incarnation: &str) {
        let mut sessions = lock(&self.sessions);
        if sessions.get(session).is_some_and(|owner| {
            owner.incarnation == incarnation
                && owner.draining
                && owner
                    .instances
                    .iter()
                    .all(|i| matches!(&*lock(&i.result), Some(Ok(()))))
        }) {
            sessions.remove(session);
        }
    }

    pub(crate) fn begin_drain(&self, session: &str) {
        if let Some(owner) = lock(&self.sessions).get_mut(session) {
            owner.draining = true;
            for instance in &mut owner.instances {
                // Shutdown the write direction only; the receipt returns on read.
                let _ = instance.control.shutdown(std::net::Shutdown::Write);
            }
        }
    }
    pub(crate) fn begin_all(&self) {
        let ids: Vec<_> = lock(&self.sessions).keys().cloned().collect();
        for id in ids {
            self.begin_drain(&id);
        }
    }
    pub(crate) fn wait(&self, session: Option<&str>, deadline: Instant) -> Result<(), String> {
        loop {
            let mut pending = false;
            for (id, owner) in lock(&self.sessions).iter() {
                if session.is_some_and(|wanted| wanted != id) {
                    continue;
                }
                for instance in &owner.instances {
                    match &*lock(&instance.result) {
                        Some(Ok(())) => {}
                        Some(Err(error)) => return Err(error.clone()),
                        None => pending = true,
                    }
                }
            }
            if !pending {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("remote MCP cleanup deadline expired; cleanup incomplete".into());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

pub(crate) fn pidfd(pid: u32) -> Result<OwnedFd, String> {
    // SAFETY: pidfd_open has no pointer arguments and returns a new owned fd.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        return Err("remote MCP requires Linux pidfd support".into());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
    // Probe signaling support without sending a signal.
    if unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd.as_raw_fd(),
            0,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    } != 0
    {
        return Err("remote MCP requires Linux pidfd signaling support".into());
    }
    Ok(fd)
}
fn exited(fd: &OwnedFd) -> bool {
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: poll points to one initialized record.
    unsafe { libc::poll(&mut poll, 1, 0) > 0 && poll.revents != 0 }
}

/// Runs only in the dedicated, single-threaded supervisor executable mode.
/// No other thread/signal handler may wait on this process's children.
pub(crate) fn supervise(
    mut control: UnixStream,
    client: OwnedFd,
    config: ResolvedServer,
) -> Result<(), String> {
    control
        .set_nonblocking(true)
        .map_err(|_| "remote MCP control unavailable")?;
    let stopped = |control: &mut UnixStream| {
        let mut byte = [0];
        TERMINATED.load(Ordering::Relaxed)
            || exited(&client)
            || match control.read(&mut byte) {
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => false,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => false,
                _ => true,
            }
    };
    // Native clients may leave additional inherited descriptors without their
    // environment variables. Seal every non-stdio descriptor before spawning;
    // the single-threaded supervisor has no concurrent descriptor mutations.
    for entry in std::fs::read_dir("/proc/self/fd")
        .map_err(|_| "remote MCP descriptor inventory unavailable")?
    {
        let entry = entry.map_err(|_| "remote MCP descriptor inventory unavailable")?;
        if let Some(fd) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
            .filter(|fd| *fd > 2)
        {
            codesense_host_proto::broker::set_inheritable(fd, false)
                .map_err(|_| "remote MCP descriptor isolation unavailable")?;
        }
    }
    let mut spawned = None;
    let mut startup_failed = false;
    let mut server_failed = false;
    // The Host has retained our pidfd/control and supplied the captured client
    // pidfd/config before this function runs. Only now may we leave the PTY's
    // process group. A native client may already have isolated its MCP child.
    let isolation_failed = !stopped(&mut control)
        && unsafe { libc::getpgrp() } != std::process::id() as i32
        && unsafe { libc::setsid() } < 0;
    if !isolation_failed && !stopped(&mut control) {
        spawned = Command::new(&config.command)
            .args(&config.args)
            .current_dir(&config.cwd)
            .env_clear()
            .envs(&config.env)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .ok();
        startup_failed = spawned.is_none();
    }
    if let Some(child) = spawned {
        // Keep the direct server unreaped until the drain owns all adopted children.
        loop {
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let status = unsafe {
                libc::waitid(
                    libc::P_PID,
                    child.id(),
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if stopped(&mut control) {
                break;
            }
            if status != 0 {
                server_failed = true;
                break;
            }
            if unsafe { info.si_pid() } != 0 {
                server_failed =
                    info.si_code != libc::CLD_EXITED || unsafe { info.si_status() } != 0;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // Child::drop does not reap; the single drain below owns every wait.
        drop(child);
    }
    let cleanup = drain_children(Instant::now() + DRAIN_TIME);
    let _ = control.set_nonblocking(false);
    let _ = control.set_write_timeout(Some(Duration::from_millis(100)));
    let _ = control.write_all(if cleanup.is_ok() { b"C" } else { b"F" });
    cleanup?;
    if isolation_failed {
        return Err("remote MCP supervisor process isolation unavailable".into());
    }
    if server_failed {
        return Err("selected remote MCP server exited unsuccessfully".into());
    }
    if startup_failed {
        Err("selected remote MCP server failed to start; verify remote executable and cwd".into())
    } else {
        Ok(())
    }
}
pub(crate) fn initialize_supervisor() -> Result<(), String> {
    let own = pidfd(std::process::id())?;
    drop(own);
    // SAFETY: this entry point runs before creating any threads or children.
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
        return Err("remote MCP requires Linux child subreaper support".into());
    }
    // Prevent privilege elevation even through a trusted interpreter/wrapper.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err("remote MCP same-account execution restriction unavailable".into());
    }
    // These handlers only set a lock-free flag. The main loop owns all waits.
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = termination_signal as *const () as usize;
        if unsafe { libc::sigemptyset(&mut action.sa_mask) } != 0
            || unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) } != 0
        {
            return Err("remote MCP termination observation unavailable".into());
        }
    }
    Ok(())
}
fn drain_children(deadline: Instant) -> Result<(), String> {
    let children = format!("/proc/self/task/{}/children", std::process::id());
    loop {
        let owned = std::fs::read_to_string(&children)
            .map_err(|_| "remote MCP child ownership unavailable")?;
        let pids: Vec<u32> = owned
            .split_whitespace()
            .take(MAX_CHILDREN)
            .map(|v| v.parse().map_err(|_| "invalid owned child identity"))
            .collect::<Result<_, _>>()?;
        // Only our unreaped direct children are enumerated. Their numeric PIDs
        // cannot be recycled until this single-threaded owner calls waitpid.
        for pid in pids {
            let fd = pidfd(pid)?;
            if unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    fd.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            } != 0
                && !exited(&fd)
            {
                return Err("remote MCP child termination failed; cleanup incomplete".into());
            }
        }
        loop {
            let status = unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) };
            if status > 0 {
                continue;
            }
            if status < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD) {
                return Ok(());
            }
            break;
        }
        if Instant::now() >= deadline {
            return Err("remote MCP descendant cleanup deadline expired".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> ResolvedServer {
        ResolvedServer {
            command: "/bin/true".into(),
            args: vec![],
            cwd: "/".into(),
            env: Default::default(),
        }
    }
    #[test]
    fn closed_gate_and_wrong_incarnation_refuse_before_acknowledgement() {
        let owners = OwnedSupervisors::default();
        owners.provision("session", "first").unwrap();
        owners.begin_drain("session");
        for incarnation in ["first", "other"] {
            let (control, mut peer) = UnixStream::pair().unwrap();
            assert!(owners
                .register(
                    SessionIncarnation {
                        session: "session",
                        nonce: incarnation
                    },
                    control,
                    pidfd(std::process::id()).unwrap(),
                    &pidfd(std::process::id()).unwrap(),
                    &config(),
                    true
                )
                .is_err());
            let mut byte = [0];
            assert_eq!(peer.read(&mut byte).unwrap(), 0);
        }
        owners.wait(Some("session"), Instant::now()).unwrap();
        owners.provision("session", "second").unwrap();
        assert!(!owners.settled("session", "first"));
    }
    #[test]
    fn failed_supervisor_and_timeout_never_grant_success_or_same_id_reuse() {
        for failed in [false, true] {
            let owners = OwnedSupervisors::default();
            owners.provision("session", "first").unwrap();
            let (control, _peer) = UnixStream::pair().unwrap();
            lock(&owners.sessions)
                .get_mut("session")
                .unwrap()
                .instances
                .push(Instance {
                    control,
                    result: Arc::new(Mutex::new(if failed {
                        Some(Err("failed fixture".into()))
                    } else {
                        None
                    })),
                });
            owners.begin_all();
            assert!(owners
                .wait(None, Instant::now() + Duration::from_millis(10))
                .is_err());
            assert!(owners.provision("session", "second").is_err());
            assert!(!owners.settled("session", "first"));
        }
    }
    #[test]
    fn end_closes_control_but_retains_receipt_direction_and_never_signals_sentinel() {
        let owners = OwnedSupervisors::default();
        owners.provision("session", "first").unwrap();
        let (control, mut peer) = UnixStream::pair().unwrap();
        let result = Arc::new(Mutex::new(None));
        lock(&owners.sessions)
            .get_mut("session")
            .unwrap()
            .instances
            .push(Instance {
                control,
                result: result.clone(),
            });
        owners.begin_drain("session");
        let mut byte = [0];
        assert_eq!(peer.read(&mut byte).unwrap(), 0);
        peer.write_all(b"C").unwrap();
        lock(&owners.sessions).get_mut("session").unwrap().instances[0]
            .control
            .read_exact(&mut byte)
            .unwrap();
        assert_eq!(byte, [b'C']);
        *lock(&result) = Some(Ok(()));
        owners.wait(Some("session"), Instant::now()).unwrap();
        assert!(owners.settled("session", "first"));
    }
}
