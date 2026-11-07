// SPDX-License-Identifier: MIT
//! Spawns the emulator worker, checks its `hello`, and restarts it when it
//! exits.
//!
//! The worker executable is copied once, when the host starts, into a file
//! private to this host, and every restart runs that copy. A sidecar outlives
//! app updates, and an update replaces the `.app` bundle (and with it the
//! worker file) in place. Re-reading the bundle after an update would start a
//! worker whose xterm differs from the one the host's sessions were built
//! with. As a second guard, a restarted worker whose `hello` differs from the
//! first one is refused rather than used.

use std::fmt;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use log::{error, info, warn};

use super::client::EmulatorLink;
use super::wire::{read_frame, write_frame, Reply, Request, WorkerInfo, HELLO_ID};
use super::WORKER_BIN;

/// How long a fresh worker has to answer `hello`. A compiled worker answers in
/// tens of milliseconds; this covers a cold disk and a loaded machine.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// How often the monitor checks whether the worker is still running.
const POLL: Duration = Duration::from_millis(20);

/// Delay before each restart attempt in a row. The first is short so a crash
/// is back well within a second; later ones back off so a worker that keeps
/// dying does not spin.
const BACKOFF: [Duration; 5] = [
    Duration::from_millis(50),
    Duration::from_millis(250),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
];

/// A worker that ran this long before exiting counts as healthy: the next
/// restart starts the backoff from the beginning. One that exits sooner counts
/// as a failure, even though it answered `hello`.
const STABLE_RUN: Duration = Duration::from_secs(30);

/// How long a worker with work outstanding may go without replying before it
/// is taken for hung, killed and restarted. A 64 KiB batch parses in a few
/// milliseconds and a full snapshot in well under a second.
pub const APPLY_DEADLINE: Duration = Duration::from_secs(10);

/// How often, and how far apart, a spawn refused with `ETXTBSY` is retried.
const TEXT_BUSY_RETRIES: u32 = 50;
const TEXT_BUSY_WAIT: Duration = Duration::from_millis(10);

/// Failures in a row (a restart that did not start, or a worker that exited
/// before `STABLE_RUN`) after which the supervisor gives up.
const MAX_FAILED_RESTARTS: usize = 5;

/// Tells apart the pinned copies of several hosts in one process (tests run
/// many).
static PIN_SEQ: AtomicU64 = AtomicU64::new(0);

/// Where a host finds its worker, and where it keeps its pinned copy.
#[derive(Debug, Clone)]
pub struct EmulatorConfig {
    /// The worker executable as shipped (inside the app bundle, or beside the
    /// host agent's binary).
    pub worker: PathBuf,
    /// A directory of this host's where the pinned copy is written. Copies left
    /// by hosts that are no longer running are removed from it.
    pub runtime_dir: PathBuf,
    /// See [`APPLY_DEADLINE`].
    pub apply_deadline: Duration,
}

impl EmulatorConfig {
    pub fn new(worker: PathBuf, runtime_dir: PathBuf) -> Self {
        Self {
            worker,
            runtime_dir,
            apply_deadline: APPLY_DEADLINE,
        }
    }
}

#[derive(Debug)]
pub enum EmulatorError {
    /// No worker at the configured path.
    Missing(PathBuf),
    /// The worker could not be copied into the runtime directory.
    Pin(String),
    /// The worker did not start, or did not answer `hello`.
    Start(String),
    /// This host already runs a worker.
    AlreadyStarted,
}

impl fmt::Display for EmulatorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(path) => write!(f, "emulator worker not found at {}", path.display()),
            Self::Pin(message) => write!(f, "emulator worker could not be pinned: {message}"),
            Self::Start(message) => write!(f, "emulator worker did not start: {message}"),
            Self::AlreadyStarted => write!(f, "this host already runs an emulator worker"),
        }
    }
}

impl std::error::Error for EmulatorError {}

/// Where the worker stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmulatorStatus {
    Running {
        pid: u32,
        info: WorkerInfo,
    },
    /// The worker exited and a restart is pending.
    Restarting,
    /// A restarted worker reported a different version from the first one. It
    /// was stopped and is not restarted again.
    Refused {
        reason: String,
    },
    /// Restarts kept failing; the supervisor stopped trying.
    Failed {
        reason: String,
    },
}

/// Supervises one emulator worker for one host. Dropping it stops the worker
/// and removes the pinned copy.
pub struct EmulatorSupervisor {
    shared: Arc<Shared>,
    monitor: Option<JoinHandle<()>>,
}

struct Shared {
    /// Where every started worker is handed for the host's sessions to use.
    link: Arc<EmulatorLink>,
    pinned: PathBuf,
    runtime_dir: PathBuf,
    apply_deadline: Duration,
    /// The first worker's `hello`: every restart must match it.
    first: WorkerInfo,
    state: Mutex<State>,
    changed: Condvar,
}

struct State {
    status: EmulatorStatus,
    worker: Option<Worker>,
    stopping: bool,
}

struct Worker {
    child: Child,
    /// The worker's generation on the link.
    generation: u64,
}

/// A worker that answered `hello`, before it is handed to the link.
struct Launched {
    child: Child,
    pid: u32,
    info: WorkerInfo,
    stdin: ChildStdin,
    frames: mpsc::Receiver<Vec<u8>>,
}

impl Launched {
    /// Give the worker's pipes to `link`; a failure stops the worker.
    fn attach(mut self, link: &Arc<EmulatorLink>) -> Result<(Worker, u32, WorkerInfo), String> {
        match link.attach(self.stdin, self.frames) {
            Ok(generation) => Ok((
                Worker {
                    child: self.child,
                    generation,
                },
                self.pid,
                self.info,
            )),
            Err(message) => {
                let _ = self.child.kill();
                let _ = self.child.wait();
                Err(message)
            }
        }
    }
}

impl EmulatorSupervisor {
    /// Pin `config.worker`, start it, check its `hello` and attach it to
    /// `link`, as every restart is. Fails without panicking when the worker is
    /// missing or does not answer; the host keeps running without an
    /// emulator.
    pub fn start(config: EmulatorConfig, link: Arc<EmulatorLink>) -> Result<Self, EmulatorError> {
        if !config.worker.is_file() {
            return Err(EmulatorError::Missing(config.worker));
        }
        let pinned = pin(&config.worker, &config.runtime_dir).map_err(EmulatorError::Pin)?;
        let (worker, pid, info) = match launch(&pinned, &config.runtime_dir)
            .and_then(|launched| launched.attach(&link))
        {
            Ok(started) => started,
            Err(message) => {
                let _ = std::fs::remove_file(&pinned);
                return Err(EmulatorError::Start(message));
            }
        };
        log_started(pid, &info);
        let shared = Arc::new(Shared {
            link,
            pinned,
            runtime_dir: config.runtime_dir,
            apply_deadline: config.apply_deadline,
            first: info.clone(),
            state: Mutex::new(State {
                status: EmulatorStatus::Running { pid, info },
                worker: Some(worker),
                stopping: false,
            }),
            changed: Condvar::new(),
        });
        let spawned = {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("emulator-supervisor".into())
                .spawn(move || monitor(&shared))
        };
        let mut supervisor = Self {
            shared,
            monitor: None,
        };
        match spawned {
            Ok(monitor) => {
                supervisor.monitor = Some(monitor);
                Ok(supervisor)
            }
            // Dropping the supervisor stops the worker it just started.
            Err(e) => Err(EmulatorError::Start(format!("supervisor thread: {e}"))),
        }
    }

    pub fn status(&self) -> EmulatorStatus {
        self.shared.lock().status.clone()
    }

    /// The pinned copy every start and restart runs.
    pub fn worker_path(&self) -> &Path {
        &self.shared.pinned
    }

    /// Wait up to `timeout` for a status that satisfies `done`, and return it;
    /// `None` on timeout.
    pub fn wait_for(
        &self,
        timeout: Duration,
        done: impl Fn(&EmulatorStatus) -> bool,
    ) -> Option<EmulatorStatus> {
        let deadline = Instant::now() + timeout;
        let mut state = self.shared.lock();
        loop {
            if done(&state.status) {
                return Some(state.status.clone());
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            state = self
                .shared
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

impl Drop for EmulatorSupervisor {
    fn drop(&mut self) {
        {
            let mut state = self.shared.lock();
            state.stopping = true;
            if let Some(mut worker) = state.worker.take() {
                self.shared.link.detach(worker.generation);
                let _ = worker.child.kill();
                let _ = worker.child.wait();
            }
        }
        self.shared.changed.notify_all();
        if let Some(monitor) = self.monitor.take() {
            let _ = monitor.join();
        }
        let _ = std::fs::remove_file(&self.shared.pinned);
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_status(&self, state: &mut State, status: EmulatorStatus) {
        state.status = status;
        self.changed.notify_all();
    }
}

/// Watch the running worker and restart it from the pinned copy when it exits
/// or hangs, until the supervisor is dropped, a restart is refused, or restarts keep
/// failing.
fn monitor(shared: &Shared) {
    // Restarts since the worker last ran for `STABLE_RUN` (picks the backoff),
    // and failures since then (gives up at `MAX_FAILED_RESTARTS`): a failed
    // launch, or a worker that exited early. A worker that answers `hello` and
    // then dies at once is a crash loop, not a recovery.
    let mut recent = 0usize;
    let mut failed = 0usize;
    let mut started = Instant::now();
    loop {
        let mut state = shared.lock();
        // Since when the worker has had work outstanding, as seen by this loop.
        let mut busy_since: Option<Instant> = None;
        let exit = loop {
            if state.stopping {
                return;
            }
            let Some(worker) = state.worker.as_mut() else {
                return;
            };
            match worker.child.try_wait() {
                Ok(Some(exit)) => break exit.to_string(),
                Ok(None) => {}
                Err(e) => break format!("wait failed: {e}"),
            }
            if let Some(silent) = stalled(shared, worker.generation, &mut busy_since) {
                // A worker that stopped applying without exiting would let the
                // queue grow for good.
                let _ = worker.child.kill();
                let _ = worker.child.wait();
                break format!("killed: no reply for {silent:?} with work outstanding");
            }
            state = shared
                .changed
                .wait_timeout(state, POLL)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        };
        if let Some(worker) = state.worker.take() {
            shared.link.detach(worker.generation);
        }
        if started.elapsed() >= STABLE_RUN {
            recent = 0;
            failed = 0;
        } else {
            failed += 1;
        }
        if failed >= MAX_FAILED_RESTARTS {
            let reason = format!(
                "the worker exited {failed} times in a row within {STABLE_RUN:?} of starting \
                 (last: {exit}); not restarting"
            );
            error!("emulator worker failed: {reason}");
            shared.set_status(&mut state, EmulatorStatus::Failed { reason });
            return;
        }
        shared.set_status(&mut state, EmulatorStatus::Restarting);
        error!("emulator worker exited ({exit}); restarting");

        loop {
            let delay = BACKOFF[recent.min(BACKOFF.len() - 1)];
            recent += 1;
            let deadline = Instant::now() + delay;
            while let Some(left) = deadline.checked_duration_since(Instant::now()) {
                if state.stopping {
                    return;
                }
                state = shared
                    .changed
                    .wait_timeout(state, left)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
            if state.stopping {
                return;
            }
            // Launch outside the lock: `hello` can take a while, and status
            // readers must not wait on it.
            drop(state);
            let launched = launch(&shared.pinned, &shared.runtime_dir);
            state = shared.lock();
            match launched {
                Ok(mut launched) => {
                    if state.stopping {
                        let _ = launched.child.kill();
                        let _ = launched.child.wait();
                        return;
                    }
                    if let Some(reason) = skew(&shared.first, &launched.info) {
                        let _ = launched.child.kill();
                        let _ = launched.child.wait();
                        error!("emulator worker refused: {reason}; not restarting");
                        shared.set_status(&mut state, EmulatorStatus::Refused { reason });
                        return;
                    }
                    // Sessions re-create their terminals in it as they next
                    // send (the link's generation).
                    let (worker, pid, info) = match launched.attach(&shared.link) {
                        Ok(attached) => attached,
                        Err(message) => {
                            failed += 1;
                            error!("emulator worker restart failed: {message}");
                            continue;
                        }
                    };
                    log_started(pid, &info);
                    state.worker = Some(worker);
                    shared.set_status(&mut state, EmulatorStatus::Running { pid, info });
                    started = Instant::now();
                    break;
                }
                Err(message) => {
                    failed += 1;
                    if failed >= MAX_FAILED_RESTARTS {
                        let reason = format!(
                            "{failed} failures in a row; the last restart: {message}; \
                             not restarting"
                        );
                        error!("emulator worker failed: {reason}");
                        shared.set_status(&mut state, EmulatorStatus::Failed { reason });
                        return;
                    }
                    error!("emulator worker restart failed: {message}");
                }
            }
        }
    }
}

/// How long the worker of `generation` has been silent while it owes the host
/// output or a reply, once that is past the deadline. `busy_since` is when this
/// caller first saw work outstanding, cleared once none is.
fn stalled(shared: &Shared, generation: u64, busy_since: &mut Option<Instant>) -> Option<Duration> {
    if !shared.link.has_outstanding() {
        *busy_since = None;
        return None;
    }
    let busy = *busy_since.get_or_insert_with(Instant::now);
    let last_reply = shared.link.last_reply(generation)?;
    let silent = busy.max(last_reply).elapsed();
    (silent >= shared.apply_deadline).then_some(silent)
}

/// Why `restarted` may not replace `first`, if it may not.
fn skew(first: &WorkerInfo, restarted: &WorkerInfo) -> Option<String> {
    if restarted.xterm_version != first.xterm_version {
        return Some(format!(
            "xterm_version {} differs from the pinned {}",
            restarted.xterm_version, first.xterm_version
        ));
    }
    if restarted.worker_wire_version != first.worker_wire_version {
        return Some(format!(
            "worker_wire_version {} differs from the pinned {}",
            restarted.worker_wire_version, first.worker_wire_version
        ));
    }
    None
}

fn log_started(pid: u32, info: &WorkerInfo) {
    info!(
        "emulator worker started: runtime={} version={} xterm_version={} worker_wire_version={} pid={pid}",
        info.runtime, info.version, info.xterm_version, info.worker_wire_version
    );
}

/// Copy `worker` into `runtime_dir` as this host's private file, named
/// `codesense-emulator.<machine>.<pid>.<n>`. On macOS the copy is a clone (no
/// data is duplicated); the signature travels with the file.
fn pin(worker: &Path, runtime_dir: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(runtime_dir)
        .map_err(|e| format!("create {}: {e}", runtime_dir.display()))?;
    let machine = machine_tag();
    remove_stale_pins(runtime_dir, &machine);
    let pinned = runtime_dir.join(format!(
        "{WORKER_BIN}.{machine}.{}.{}",
        std::process::id(),
        PIN_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::copy(worker, &pinned)
        .map_err(|e| format!("copy {} to {}: {e}", worker.display(), pinned.display()))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&pinned, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("chmod {}: {e}", pinned.display()))?;
    Ok(pinned)
}

/// This machine's name, reduced to characters that keep a pin's name
/// parseable. A home directory can be shared between machines (NFS), and a
/// pid is only meaningful on the machine that issued it.
fn machine_tag() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is writable for its whole length, which is what we pass.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    let name = if rc == 0 {
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..end]).into_owned()
    } else {
        String::new()
    };
    let tag: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    if tag.is_empty() {
        "localhost".into()
    } else {
        tag
    }
}

/// Remove this machine's pinned copies whose host process is gone (a host
/// killed before it could clean up). Other machines' copies are theirs to
/// judge.
fn remove_stale_pins(runtime_dir: &Path, machine: &str) {
    let Ok(entries) = std::fs::read_dir(runtime_dir) else {
        return;
    };
    let prefix = format!("{WORKER_BIN}.{machine}.");
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|name| name.strip_prefix(&prefix))
            .and_then(|rest| rest.split('.').next())
            .and_then(|pid| pid.parse::<libc::pid_t>().ok())
        else {
            continue;
        };
        if !process_alive(pid) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn process_alive(pid: libc::pid_t) -> bool {
    // SAFETY: signal 0 only checks that the process exists and may be signalled.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Start the worker at `path` and wait for its `hello`.
fn launch(path: &Path, cwd: &Path) -> Result<Launched, String> {
    let mut child = spawn(path, cwd)?;
    let pid = child.id();
    let result = handshake(&mut child, pid);
    match result {
        Ok((stdin, info, frames)) => Ok(Launched {
            child,
            pid,
            info,
            stdin,
            frames,
        }),
        Err(message) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(message)
        }
    }
}

/// Spawn the worker. Right after the copy, a fork elsewhere in the host (a PTY
/// spawn) can still hold the copy's write descriptor until that child execs, and
/// Linux refuses to exec a file open for writing (`ETXTBSY`); that clears
/// within moments, so it is retried.
fn spawn(path: &Path, cwd: &Path) -> Result<Child, String> {
    let mut busy = 0;
    loop {
        match Command::new(path)
            .env_clear()
            .envs(worker_env())
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => return Ok(child),
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) && busy < TEXT_BUSY_RETRIES => {
                busy += 1;
                std::thread::sleep(TEXT_BUSY_WAIT);
            }
            Err(e) => return Err(format!("spawn {}: {e}", path.display())),
        }
    }
}

/// The host's environment variables a worker gets. Everything else is
/// dropped: `BUN_*`, `NODE_*` and the like would change how the runtime behaves,
/// and the rest (tokens, a user's shell settings) is none of its business.
const WORKER_ENV: [&str; 7] = ["PATH", "HOME", "TMPDIR", "LANG", "LC_ALL", "LC_CTYPE", "TZ"];

fn worker_env() -> impl Iterator<Item = (&'static str, std::ffi::OsString)> {
    WORKER_ENV
        .into_iter()
        .filter_map(|key| std::env::var_os(key).map(|value| (key, value)))
}

/// Send `hello` and wait for the answer. Returns the worker's stdin, its
/// `hello`, and every frame it writes after that.
fn handshake(
    child: &mut Child,
    pid: u32,
) -> Result<(ChildStdin, WorkerInfo, mpsc::Receiver<Vec<u8>>), String> {
    let (Some(mut stdin), Some(mut stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err("worker stdio was not piped".into());
    };
    // stderr is for humans: forward it to the host's log.
    spawn_named("emulator-stderr", move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            warn!("emulator worker {pid}: {line}");
        }
    })?;
    // One reader per worker drains stdout for the worker's whole life, so the
    // worker never blocks on a full pipe.
    let (replies, received) = mpsc::channel();
    spawn_named("emulator-reader", move || loop {
        match read_frame(&mut stdout) {
            Ok(Some(frame)) => {
                let _ = replies.send(frame);
            }
            Ok(None) => return,
            Err(e) => {
                warn!("emulator worker {pid}: unreadable output: {e}");
                return;
            }
        }
    })?;
    let hello = serde_json::to_vec(&Request::Hello { id: HELLO_ID })
        .map_err(|e| format!("encode hello: {e}"))?;
    write_frame(&mut stdin, &hello).map_err(|e| format!("send hello: {e}"))?;
    let frame = received
        .recv_timeout(HELLO_TIMEOUT)
        .map_err(|_| format!("no hello reply within {HELLO_TIMEOUT:?}"))?;
    match serde_json::from_slice::<Reply>(&frame) {
        Ok(Reply::Hello { id, info }) if id == HELLO_ID => Ok((stdin, info, received)),
        Ok(Reply::Hello { id, .. }) => Err(format!("hello reply carried id {id}")),
        Ok(Reply::Error { message, .. }) => Err(format!("hello refused: {message}")),
        Ok(other) => Err(format!("expected a hello reply, got {other:?}")),
        Err(e) => Err(format!("unreadable hello reply: {e}")),
    }
}

fn spawn_named(name: &str, body: impl FnOnce() + Send + 'static) -> Result<(), String> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(body)
        .map(drop)
        .map_err(|e| format!("{name} thread: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_this_machines_pins_of_dead_hosts_are_swept() {
        let dir = tempfile::tempdir().unwrap();
        let machine = machine_tag();
        // A pid far above any real one is not alive here.
        let dead = format!("{WORKER_BIN}.{machine}.999999999.0");
        let live = format!("{WORKER_BIN}.{machine}.{}.0", std::process::id());
        let elsewhere = format!("{WORKER_BIN}.other-box.999999999.0");
        for name in [&dead, &live, &elsewhere, &"unrelated".to_string()] {
            std::fs::write(dir.path().join(name), b"").unwrap();
        }
        remove_stale_pins(dir.path(), &machine);
        assert!(!dir.path().join(&dead).exists());
        assert!(dir.path().join(&live).exists());
        assert!(
            dir.path().join(&elsewhere).exists(),
            "another machine's pin"
        );
        assert!(dir.path().join("unrelated").exists());
    }

    #[test]
    fn a_worker_sees_only_the_allowed_environment() {
        assert!(worker_env().all(|(key, _)| WORKER_ENV.contains(&key)));
        assert!(!WORKER_ENV
            .iter()
            .any(|key| key.starts_with("BUN_") || key.starts_with("NODE_")));
    }

    #[test]
    fn the_machine_tag_keeps_pin_names_parseable() {
        let tag = machine_tag();
        assert!(!tag.is_empty());
        assert!(tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }
}
