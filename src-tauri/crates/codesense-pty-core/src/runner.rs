// SPDX-License-Identifier: MIT
//! The `PtyRunner` port — the single injected seam that owns *where* an
//! interactive PTY child is spawned (a local subprocess today; a remote host
//! over SSH/tunnel later). The session registry depends only on this trait,
//! never on a concrete driver; the host resolves which driver to hand it.
//!
//! # Streaming, not batch
//!
//! Unlike a batch runner, a [`PtySession`] is a *live duplex stream* — it never
//! "returns output". [`PtyRunner::spawn`] hands back a session whose reader
//! byte-stream the registry drains, whose writer it feeds, and whose
//! [`PtySession::resize`] / [`PtySession::kill`] drive the PTY and its child.
//!
//! # Local is a driver, not a special case
//!
//! [`LocalPtyRunner`] is the built-in default driver — `portable-pty`'s
//! `openpty` + `spawn_command`, run on the blocking pool so an async caller
//! never parks a runtime worker on the fork.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use log::{error, warn};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

#[cfg(unix)]
fn signal_group(pgid: libc::pid_t, signal: i32) -> Result<(), String> {
    #[cfg(test)]
    if pgid == TEST_GROUP_SIGNAL_FAILURE_PGID {
        return Err("injected process-group signal failure".into());
    }
    if unsafe { libc::kill(-pgid, signal) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(format!("signal process group {pgid}: {error}"))
    }
}

#[cfg(all(unix, test))]
const TEST_GROUP_SIGNAL_FAILURE_PGID: libc::pid_t = libc::pid_t::MAX - 1;

/// How long the exit seam waits, after the reader reaches end-of-stream, for the
/// child to become reapable so `pty-exit` can carry its real status. A shell's
/// slave fds close as it exits, so the status is normally ready at once.
const EXIT_STATUS_GRACE: Duration = Duration::from_millis(100);

/// Runner error surface: a driver returns [`PtyRunnerError::Unsupported`] for a
/// capability it lacks so the surface can **hide** the affordance rather than
/// error, and [`PtyRunnerError::Message`] carries a driver's error text
/// byte-for-byte (the local path's spawn error string is preserved verbatim).
/// `LocalPtyRunner` never produces `Unsupported`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyRunnerError {
    Message(String),
    Unsupported,
}

impl std::fmt::Display for PtyRunnerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PtyRunnerError::Message(message) => f.write_str(message),
            PtyRunnerError::Unsupported => f.write_str("unsupported by this PTY runner"),
        }
    }
}

/// Lets a driver's `String`-returning helper `?`-convert into a
/// [`PtyRunnerError`] (the `Message` verbatim).
impl From<String> for PtyRunnerError {
    fn from(message: String) -> Self {
        PtyRunnerError::Message(message)
    }
}

/// Lets the spawn call site `?` a [`PtyRunnerError`] back into its
/// `Result<_, String>` surface with **no** string change — `Message(s)` maps to
/// `s` verbatim, so the frontend error string is byte-identical.
impl From<PtyRunnerError> for String {
    fn from(error: PtyRunnerError) -> Self {
        error.to_string()
    }
}

/// The neutral spawn request: the fully-assembled child [`CommandBuilder`]
/// (argv/env/cwd already resolved by the host) and the initial PTY size — the
/// two inputs a driver needs to open a PTY and fork the child. The live session
/// identity is the registry's key, never a field here.
pub struct PtySpec {
    /// The fully-built child command — argv, env, and cwd already resolved by
    /// the host (account/environment-binding envs applied) so the driver only
    /// spawns it. A remote driver ships this same command to its host.
    pub command: CommandBuilder,
    /// Initial terminal size for the opened PTY.
    pub size: PtySize,
}

/// How a session's child ended: `exit_code` for a normal exit, `signal` (its
/// name) when a signal killed it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExitInfo {
    pub exit_code: Option<u32>,
    pub signal: Option<String>,
}

impl From<portable_pty::ExitStatus> for ExitInfo {
    fn from(status: portable_pty::ExitStatus) -> Self {
        let signal = status.signal().map(str::to_string);
        Self {
            exit_code: signal.is_none().then(|| status.exit_code()),
            signal,
        }
    }
}

/// The spawned child plus the process group captured while its PID is
/// known-current. Never resolve a group from the PID after a wait probe may have
/// reaped the leader: the numeric PID can then belong to an unrelated process.
pub(crate) struct SessionChild {
    child: Box<dyn Child + Send>,
    #[cfg(unix)]
    process_group: Option<libc::pid_t>,
    #[cfg(unix)]
    process_group_terminated: bool,
}

impl SessionChild {
    fn new(child: Box<dyn Child + Send>) -> Self {
        #[cfg(unix)]
        let process_group = child
            .process_id()
            .map(|pid| unsafe { libc::getpgid(pid as libc::pid_t) })
            .filter(|pgid| *pgid > 0 && *pgid != unsafe { libc::getpgrp() });
        Self {
            child,
            #[cfg(unix)]
            process_group,
            #[cfg(unix)]
            process_group_terminated: false,
        }
    }

    /// Nonblocking liveness probe that also closes the process-group lifecycle
    /// at the instant a natural leader exit is observed. This prevents both
    /// orphan descendants and later reuse of the recorded numeric group id.
    fn is_alive(&mut self) -> bool {
        matches!(self.exit_status(), Ok(None))
    }

    /// A nonblocking exit-status probe: `Ok(Some)` once the child exited (its
    /// recorded process group is closed at that instant), `Ok(None)` while it
    /// runs, `Err` when its status cannot be read.
    fn exit_status(&mut self) -> Result<Option<ExitInfo>, ()> {
        match self.child.try_wait() {
            Ok(None) => Ok(None),
            Ok(Some(status)) => {
                self.terminate_process_group();
                Ok(Some(ExitInfo::from(status)))
            }
            Err(_) => Err(()),
        }
    }

    /// Stop the owned child lifecycle without ever resolving a process group
    /// from a PID after a wait probe may have reaped it. Used by every path that
    /// permanently removes or replaces a session.
    fn terminate(&mut self) {
        match self.child.try_wait() {
            Ok(None) => {
                self.terminate_process_group();
                let _ = self.child.kill();
            }
            Ok(Some(_)) => self.terminate_process_group(),
            Err(error) => {
                warn!(target: "pty", "child status unavailable during teardown: {error}");
                // The owning handle is safe to use even when numeric identity
                // could not be established by the status probe.
                let _ = self.child.kill();
            }
        }
    }

    /// Cooperative replacement teardown: TERM the process group captured at
    /// spawn, wait a bounded grace period, then KILL that same group and reap
    /// the owning child synchronously. Numeric identity is never rediscovered.
    fn drain_for_replacement(&mut self, deadline: Instant) -> Result<(), String> {
        #[cfg(unix)]
        if !self.process_group_terminated {
            if let Some(pgid) = self.process_group {
                signal_group(pgid, libc::SIGTERM)?;
            }
        }
        // Do not call try_wait here: on Unix that reaps an exited group leader,
        // allowing its numeric PGID to be reused before descendant cleanup.
        // Keep the owning child (including a zombie leader) until the shared
        // grace deadline, close the recorded group, then reap exactly once.
        while Instant::now() < deadline {
            std::thread::sleep(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(5)),
            );
        }
        self.terminate_process_group_for_replacement()?;
        let kill_error = self.child.kill().err().and_then(|error| {
            (error.kind() != std::io::ErrorKind::NotFound)
                .then(|| format!("kill child during replacement drain: {error}"))
        });
        let wait_error = self
            .child
            .wait()
            .err()
            .map(|error| format!("reap child during replacement drain: {error}"));
        match (kill_error, wait_error) {
            (None, None) => Ok(()),
            (Some(error), None) | (None, Some(error)) => Err(error),
            (Some(kill), Some(wait)) => Err(format!("{kill}; {wait}")),
        }
    }

    #[cfg(unix)]
    fn terminate_process_group(&mut self) {
        if self.process_group_terminated {
            return;
        }
        self.process_group_terminated = true;
        if let Some(pgid) = self.process_group {
            let result = unsafe { libc::kill(-pgid, libc::SIGKILL) };
            if result != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    warn!(target: "pty", "process-group kill failed for {pgid}: {error}");
                }
            }
        }
    }

    #[cfg(not(unix))]
    fn terminate_process_group(&mut self) {}

    /// Replacement's checked process-group close. Unlike ordinary interactive
    /// teardown, every unexpected signal failure is part of the result and
    /// therefore prevents the caller from publishing a retirement receipt.
    #[cfg(unix)]
    fn terminate_process_group_for_replacement(&mut self) -> Result<(), String> {
        if self.process_group_terminated {
            return Ok(());
        }
        if let Some(pgid) = self.process_group {
            signal_group(pgid, libc::SIGKILL)?;
        }
        self.process_group_terminated = true;
        Ok(())
    }

    #[cfg(not(unix))]
    fn terminate_process_group_for_replacement(&mut self) -> Result<(), String> {
        Ok(())
    }
}

/// The child shared by the registry (liveness probes, kill), the batcher's exit
/// seam (the real exit status), and the background reaper (the final wait).
#[derive(Clone)]
pub(crate) struct ChildHandle(Arc<Mutex<SessionChild>>);

impl ChildHandle {
    fn lock(&self) -> MutexGuard<'_, SessionChild> {
        // A poisoned child lock still guards a usable owning handle; recover it
        // so a teardown or reap is never skipped.
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn is_alive(&self) -> bool {
        self.lock().is_alive()
    }

    pub(crate) fn terminate(&self) {
        self.lock().terminate();
    }

    /// Block until the child is reaped.
    pub(crate) fn wait(&self) {
        let _ = self.lock().child.wait();
    }

    pub(crate) fn drain_for_replacement(&self, deadline: Instant) -> Result<(), String> {
        self.lock().drain_for_replacement(deadline)
    }

    /// The child's exit status once the reader has reached end-of-stream,
    /// waiting at most [`EXIT_STATUS_GRACE`] for it to become reapable. `None`
    /// when the child is still running (it closed its terminal but lives on), its
    /// status cannot be read, or a teardown's reaper holds it past the grace
    /// window. Observing the exit closes the recorded process group, exactly as
    /// the liveness probe does, so the reaped leader's group id is never reused.
    pub(crate) fn exit_info_after_eof(&self) -> Option<ExitInfo> {
        let deadline = Instant::now() + EXIT_STATUS_GRACE;
        loop {
            // `try_lock`: a teardown's reaper may hold the child for its blocking
            // wait; never queue the exit report behind it.
            let probe = match self.0.try_lock() {
                Ok(mut guard) => Some(guard.exit_status()),
                Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                    Some(poisoned.into_inner().exit_status())
                }
                Err(std::sync::TryLockError::WouldBlock) => None,
            };
            match probe {
                Some(Ok(Some(status))) => return Some(status),
                Some(Err(())) => return None,
                _ if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(1)),
                _ => return None,
            }
        }
    }

    /// Whether the child has exited, observed WITHOUT closing its process group
    /// (so a caller can prove teardown itself closes the group).
    #[cfg(feature = "leak-gate")]
    pub(crate) fn has_exited(&self) -> Result<bool, String> {
        self.lock()
            .child
            .try_wait()
            .map(|status| status.is_some())
            .map_err(|error| error.to_string())
    }

    #[cfg(all(unix, test))]
    pub(crate) fn process_group(&self) -> Option<libc::pid_t> {
        self.lock().process_group
    }
}

/// A live interactive PTY session — a bidirectional byte stream plus resize and
/// kill, expressed in neutral terms over `portable-pty` handles.
pub struct PtySession {
    /// Master side — drives resize; a `LocalPtyRunner` owns a real local master
    /// fd, a remote driver owns a wire proxy.
    master: Box<dyn MasterPty + Send>,
    child: ChildHandle,
    /// PID of the spawned shell, captured once at spawn (stable for the
    /// session's life). Source for the idle / agent-liveness process-tree checks.
    pid: Option<u32>,
    /// Reader byte-stream out of the PTY, until the registry's reader thread
    /// takes it.
    reader: Option<Box<dyn Read + Send>>,
    /// Writer sink into the PTY, behind its OWN lock so a write clones
    /// the handle out under a brief registry lock and does `write_all`+`flush`
    /// outside it — one terminal's I/O can't stall another's keystroke.
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl PtySession {
    /// Assemble a session from a driver's handles, capturing the child's PID and
    /// process group while they are known-current.
    pub fn new(
        master: Box<dyn MasterPty + Send>,
        child: Box<dyn Child + Send>,
        reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
    ) -> Self {
        let pid = child.process_id();
        Self {
            master,
            child: ChildHandle(Arc::new(Mutex::new(SessionChild::new(child)))),
            pid,
            reader: Some(reader),
            writer: Arc::new(Mutex::new(writer)),
        }
    }

    /// PID of the spawned child, when the driver exposes one.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// The PTY's foreground process group (`tcgetpgrp` on the master), when
    /// the driver can tell.
    #[cfg(feature = "e2e-dump")]
    pub fn foreground_pgid(&self) -> Option<i32> {
        self.master.process_group_leader()
    }

    /// The PTY's size as `(cols, rows)`; 80×24 when the driver cannot say.
    pub fn size(&self) -> (u16, u16) {
        self.master
            .get_size()
            .map_or((80, 24), |size| (size.cols, size.rows))
    }

    /// Resize the underlying PTY. Delegates to the master handle — the local
    /// driver's real resize, a remote driver's wire resize.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), PtyRunnerError> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| PtyRunnerError::Message(error.to_string()))
    }

    /// Kill the session's child: a still-running child has its process group
    /// SIGKILLed and is killed; an already-exited child only has its recorded
    /// group closed. Idempotent.
    pub fn kill(&self) {
        self.child.terminate();
    }

    /// Nonblocking liveness probe (see [`SessionChild::is_alive`]).
    pub fn is_alive(&self) -> bool {
        self.child.is_alive()
    }

    pub(crate) fn take_reader(&mut self) -> Option<Box<dyn Read + Send>> {
        self.reader.take()
    }

    pub(crate) fn writer(&self) -> Arc<Mutex<Box<dyn Write + Send>>> {
        self.writer.clone()
    }

    pub(crate) fn child(&self) -> ChildHandle {
        self.child.clone()
    }

    /// Kill the child and block until it is reaped — for a session that never
    /// reached the registry (a failed registration), where there is no command
    /// path to keep responsive.
    pub fn discard(self) {
        self.kill();
        self.child.wait();
    }

    /// Consume the registry's owning session and synchronously retire its
    /// captured process group and child.
    pub(crate) fn drain_for_replacement(self, deadline: Instant) -> Result<(), String> {
        self.child.drain_for_replacement(deadline)
    }

    /// Release the PTY handles now and reap the (already killed) child on the
    /// bounded blocking pool, so a close never blocks its caller on `wait`.
    pub(crate) fn reap_in_background(self, blocking: &tokio::runtime::Handle) {
        let Self { child, .. } = self;
        reap_child_in_background(blocking, child);
    }
}

fn reap_child_in_background(blocking: &tokio::runtime::Handle, child: ChildHandle) {
    #[cfg(feature = "leak-gate")]
    crate::leak_gate::PTY_REAPERS_IN_FLIGHT.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    // The bounded blocking pool keeps one pathological child wait from
    // head-of-line blocking later sessions, while the caller still returns
    // immediately and never creates an unbounded thread per close.
    blocking.spawn_blocking(move || {
        child.wait();
        #[cfg(feature = "leak-gate")]
        crate::leak_gate::PTY_REAPERS_IN_FLIGHT.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    });
}

/// The interactive-PTY spawn port. One method — [`PtyRunner::spawn`] — opens a
/// live duplex [`PtySession`]. Dyn-dispatched as `Box<dyn PtyRunner>` so the
/// host can hand the registry a driver without the registry knowing which.
#[async_trait]
pub trait PtyRunner: Send + Sync {
    /// Spawn the child described by `spec` and return a live [`PtySession`]. The
    /// sole method: a session is a running stream, not a value that returns
    /// output.
    async fn spawn(&self, spec: PtySpec) -> Result<PtySession, PtyRunnerError>;
}

/// The built-in default driver. Opens a local PTY (`native_pty_system().openpty`),
/// forks the child (`slave.spawn_command`), and clones the reader / takes the
/// writer, preserving every error string — on the blocking pool, since the
/// open + fork are synchronous syscalls.
pub struct LocalPtyRunner;

#[async_trait]
impl PtyRunner for LocalPtyRunner {
    async fn spawn(&self, spec: PtySpec) -> Result<PtySession, PtyRunnerError> {
        tokio::task::spawn_blocking(move || open_local_session(spec))
            .await
            .map_err(|error| PtyRunnerError::Message(format!("PTY spawn task failed: {error}")))?
    }
}

fn open_local_session(spec: PtySpec) -> Result<PtySession, PtyRunnerError> {
    let PtySpec { command, size } = spec;
    let pty_system = native_pty_system();
    // The per-failure `error!(target: "pty", ...)` lines keep an
    // open/reader/writer failure logged under its own distinct classification,
    // not folded into a single "failed to spawn" line at the call site.
    let pair = pty_system.openpty(size).map_err(|error| {
        error!(target: "pty", "spawn_pty: failed to open PTY: {}", error);
        PtyRunnerError::Message(format!("Failed to open PTY: {}", plainly(&*error)))
    })?;
    // Every fallible master-side step precedes the fork, so a failure never
    // leaves a forked child behind.
    let reader = pair.master.try_clone_reader().map_err(|error| {
        error!(target: "pty", "spawn_pty: failed to clone reader: {}", error);
        PtyRunnerError::Message(format!("Failed to get reader: {}", plainly(&*error)))
    })?;
    let writer = pair.master.take_writer().map_err(|error| {
        error!(target: "pty", "spawn_pty: failed to get writer: {}", error);
        PtyRunnerError::Message(format!("Failed to get writer: {}", plainly(&*error)))
    })?;
    // The child gets the open-file soft limit a login would, not the
    // host's raised one: set in the child, never by lowering this host's.
    let child = pair
        .slave
        .spawn_command(with_original_limit(command))
        .map_err(|error| {
            error!(target: "pty", "spawn_pty: failed to spawn command: {}", error);
            PtyRunnerError::Message(format!("Failed to spawn: {}", plainly(&*error)))
        })?;
    Ok(PtySession::new(pair.master, child, reader, writer))
}

/// `command` run through this host's child-limit shim
/// ([`crate::nofile::child_argv`]), which sets the original open-file soft
/// limit in the child and execs the command in its place: same pid, argv,
/// environment, working directory and terminal. Only the argv changes, so
/// every launch golden (built before this runner) is untouched. The command
/// runs as given when the host serves no shim, when it is a default-program
/// builder (no caller makes one), or when its program cannot be resolved here,
/// so `portable-pty` reports a missing program as it always has.
fn with_original_limit(command: CommandBuilder) -> CommandBuilder {
    let Some(program) = resolve_program(&command) else {
        return command;
    };
    let Some(argv) = crate::nofile::child_argv(program.as_os_str(), command.get_argv()) else {
        return command;
    };
    let mut wrapped = command;
    *wrapped.get_argv_mut() = argv;
    wrapped
}

/// `command`'s program as `portable-pty` resolves it: an absolute path as is; a
/// `./` or `../` path against the working directory; any other name through
/// each `PATH` entry, itself taken against the working directory (else
/// `$HOME`, where `portable-pty` falls back to). `None` when it finds no
/// executable file, or the builder runs the default program.
fn resolve_program(command: &CommandBuilder) -> Option<std::path::PathBuf> {
    use std::path::{Component, Path};
    let program = Path::new(command.get_argv().first()?);
    let executable = |path: std::path::PathBuf| {
        let bytes = std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str());
        let c_path = std::ffi::CString::new(bytes).ok()?;
        // SAFETY: a NUL-terminated path; `access` only reads it.
        let runnable = unsafe { libc::access(c_path.as_ptr(), libc::X_OK) } == 0;
        (runnable && Path::is_file(&path)).then_some(path)
    };
    if program.is_absolute() {
        return executable(program.to_path_buf());
    }
    let cwd = command
        .get_cwd()
        .map(Path::new)
        .filter(|cwd| Path::is_dir(cwd))
        .or_else(|| command.get_env("HOME").map(Path::new))?;
    if matches!(
        program.components().next(),
        Some(Component::CurDir | Component::ParentDir)
    ) {
        return executable(Path::join(cwd, program));
    }
    std::env::split_paths(command.get_env("PATH")?)
        .find_map(|dir| executable(Path::join(&Path::join(cwd, dir), program)))
}

/// A spawn error that was EMFILE says so plainly, naming this
/// process and its counts; any other error reads exactly as before.
fn plainly(error: &(dyn std::error::Error + 'static)) -> String {
    let emfile = std::iter::successors(Some(error), |cause| cause.source()).any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .and_then(std::io::Error::raw_os_error)
            == Some(libc::EMFILE)
            // `openpty` reports its errno only in its message (`{:?}` of it).
            || str::contains(&cause.to_string(), &format!("code: {},", libc::EMFILE))
    });
    if !emfile {
        return error.to_string();
    }
    let process = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.file_name().map(|name| name.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "this process".into());
    format!("{} ({error})", crate::nofile::limit_reached(&process))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug)]
    struct ExitedChild(Arc<Mutex<Vec<&'static str>>>);

    impl portable_pty::ChildKiller for ExitedChild {
        fn kill(&mut self) -> std::io::Result<()> {
            self.0.lock().unwrap().push("kill-child");
            Ok(())
        }
        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            Box::new(self.clone())
        }
    }

    impl Child for ExitedChild {
        fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
            self.0.lock().unwrap().push("try-wait");
            Ok(Some(portable_pty::ExitStatus::with_exit_code(0)))
        }
        fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
            self.0.lock().unwrap().push("owning-wait");
            Ok(portable_pty::ExitStatus::with_exit_code(0))
        }
        fn process_id(&self) -> Option<u32> {
            None
        }
    }

    #[cfg(unix)]
    #[test]
    fn rm4d_natural_exit_checks_recorded_group_cleanup() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut child = SessionChild {
            child: Box::new(ExitedChild(calls.clone())),
            process_group: Some(libc::pid_t::MAX),
            process_group_terminated: false,
        };
        child
            .drain_for_replacement(Instant::now() + Duration::from_millis(10))
            .unwrap();
        assert!(child.process_group_terminated);
        assert_eq!(*calls.lock().unwrap(), ["kill-child", "owning-wait"]);
    }

    #[cfg(unix)]
    #[test]
    fn rm4d_natural_exit_propagates_group_signal_failure() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut child = SessionChild {
            child: Box::new(ExitedChild(calls.clone())),
            process_group: Some(TEST_GROUP_SIGNAL_FAILURE_PGID),
            process_group_terminated: false,
        };
        assert_eq!(
            child.drain_for_replacement(Instant::now() + Duration::from_millis(10)),
            Err("injected process-group signal failure".into())
        );
        assert!(!child.process_group_terminated);
        assert!(calls.lock().unwrap().is_empty());
    }

    /// The built-in local driver really opens a PTY, forks the child, and
    /// exposes the neutral reader/resize/kill surface. Drives the child to echo
    /// a line and reads it back, proving the reader byte-stream is the live PTY
    /// output.
    #[tokio::test]
    async fn local_runner_spawns_and_streams_over_the_neutral_surface() {
        let mut command = CommandBuilder::new("sh");
        command.arg("-c");
        command.arg("printf 'RT1_OK\\n'");
        let spec = PtySpec {
            command,
            size: PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            },
        };
        let mut session = LocalPtyRunner
            .spawn(spec)
            .await
            .expect("local spawn succeeds");
        assert!(session.pid().is_some(), "a local child exposes its pid");
        // The neutral resize surface delegates to the real master fd.
        session.resize(100, 30).expect("resize the live PTY");

        let mut reader = session.take_reader().expect("reader is handed out once");
        assert!(session.take_reader().is_none(), "the reader moves out once");
        let out = tokio::task::spawn_blocking(move || {
            let mut out = Vec::new();
            let mut buf = [0u8; 256];
            // Read until the child's line arrives or the PTY closes (EOF).
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        out.extend_from_slice(&buf[..n]);
                        if out.windows(6).any(|w| w == b"RT1_OK") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            out
        })
        .await
        .unwrap();
        assert!(
            out.windows(6).any(|w| w == b"RT1_OK"),
            "expected the child's stdout over the neutral reader, got {:?}",
            String::from_utf8_lossy(&out)
        );
        session.child().wait();
    }

    #[test]
    fn error_message_maps_to_string_verbatim() {
        // A driver's error string must reach the command surface unchanged, so
        // the spawn error the frontend sees is identical.
        let error = PtyRunnerError::Message("Failed to spawn: boom".to_string());
        let as_string: String = error.into();
        assert_eq!(as_string, "Failed to spawn: boom");
    }

    /// The exit seam reports the child's real status: a non-zero exit code
    /// survives as-is, and a signal death reports the signal instead of a code.
    #[tokio::test]
    async fn exit_info_carries_the_real_exit_status() {
        for (script, expected) in [
            (
                "exit 3",
                ExitInfo {
                    exit_code: Some(3),
                    signal: None,
                },
            ),
            (
                "exit 0",
                ExitInfo {
                    exit_code: Some(0),
                    signal: None,
                },
            ),
        ] {
            let mut command = CommandBuilder::new("sh");
            command.arg("-c");
            command.arg(script);
            let session = LocalPtyRunner
                .spawn(PtySpec {
                    command,
                    size: PtySize::default(),
                })
                .await
                .expect("local spawn succeeds");
            session.child().wait();
            assert_eq!(session.child().exit_info_after_eof(), Some(expected));
        }

        let mut command = CommandBuilder::new("sh");
        command.arg("-c");
        command.arg("sleep 30");
        let session = LocalPtyRunner
            .spawn(PtySpec {
                command,
                size: PtySize::default(),
            })
            .await
            .expect("local spawn succeeds");
        session.kill();
        session.child().wait();
        let killed = session.child().exit_info_after_eof().expect("reaped");
        assert_eq!(killed.exit_code, None, "a signal death has no exit code");
        assert!(killed.signal.is_some(), "a signal death names its signal");
    }

    #[derive(Debug, Clone)]
    struct BlockingWaitChild {
        release: Arc<(Mutex<bool>, std::sync::Condvar)>,
    }

    impl portable_pty::ChildKiller for BlockingWaitChild {
        fn kill(&mut self) -> std::io::Result<()> {
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            Box::new(self.clone())
        }
    }

    impl Child for BlockingWaitChild {
        fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
            Ok(None)
        }

        fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
            let (lock, ready) = &*self.release;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = ready.wait(released).unwrap();
            }
            Ok(portable_pty::ExitStatus::with_exit_code(0))
        }

        fn process_id(&self) -> Option<u32> {
            None
        }
    }

    #[test]
    fn child_reaper_enqueue_returns_before_wait_completes() {
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let child = ChildHandle(Arc::new(Mutex::new(SessionChild::new(Box::new(
            BlockingWaitChild {
                release: release.clone(),
            },
        )))));
        let (returned_tx, returned_rx) = std::sync::mpsc::channel();
        let caller = std::thread::spawn(move || {
            reap_child_in_background(&crate::fixtures::fixture_runtime(), child);
            returned_tx.send(()).unwrap();
        });

        let returned = returned_rx.recv_timeout(Duration::from_millis(250));
        let (lock, ready) = &*release;
        *lock.lock().unwrap() = true;
        ready.notify_all();
        caller.join().unwrap();
        returned.expect("reaper enqueue must return while child.wait is still blocked");
    }

    /// A child still running at end-of-stream reports no status once the grace
    /// window lapses — the exit seam never blocks on a live process.
    #[tokio::test]
    async fn exit_info_is_none_for_a_child_that_outlives_the_grace_window() {
        let mut command = CommandBuilder::new("sh");
        command.arg("-c");
        command.arg("sleep 30");
        let session = LocalPtyRunner
            .spawn(PtySpec {
                command,
                size: PtySize::default(),
            })
            .await
            .expect("local spawn succeeds");
        let started = Instant::now();
        assert_eq!(session.child().exit_info_after_eof(), None);
        assert!(started.elapsed() < Duration::from_secs(2));
        session.discard();
    }

    /// The shim gets the program `portable-pty` would have run, so
    /// a missing one is still `portable-pty`'s own synchronous error.
    #[cfg(unix)]
    #[test]
    fn resolves_the_program_as_portable_pty_does() {
        use std::os::unix::fs::PermissionsExt;
        let cwd = tempfile::tempdir().unwrap();
        let script = std::path::Path::join(cwd.path(), "tool");
        std::fs::write(&script, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::create_dir(std::path::Path::join(cwd.path(), "bin")).unwrap();
        let relative_tool = std::path::Path::join(cwd.path(), "bin/relative-tool");
        std::fs::copy(&script, &relative_tool).unwrap();
        let resolve = |program: &str| {
            let mut command = CommandBuilder::new(program);
            command.cwd(cwd.path());
            command.env("PATH", "/bin:bin");
            resolve_program(&command)
        };
        assert_eq!(resolve("sh"), Some("/bin/sh".into()), "through PATH");
        assert_eq!(
            resolve("relative-tool"),
            Some(std::path::PathBuf::clone(&relative_tool)),
            "a relative PATH entry is taken against the working directory"
        );
        assert_eq!(resolve("./tool"), Some(std::path::Path::join(cwd.path(), "./tool")));
        assert_eq!(resolve("/bin/sh"), Some("/bin/sh".into()));
        assert_eq!(resolve("tool"), None, "a bare name never searches the cwd");
        assert_eq!(resolve("definitely-not-installed-sv-pbf-290"), None);
        assert_eq!(resolve("/bin"), None, "a directory is not a program");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(resolve("./tool"), None, "nor is a file without exec");
        assert_eq!(resolve_program(&CommandBuilder::new_default_prog()), None);
    }

    /// EMFILE names the process and its counts; any other
    /// error reads exactly as before.
    #[test]
    fn an_emfile_spawn_error_says_the_open_file_limit_was_reached() {
        let emfile = std::io::Error::from_raw_os_error(libc::EMFILE);
        let text = plainly(&emfile);
        assert!(str::starts_with(&text, "open-file limit reached in "), "{text}");
        let pid = format!("(pid {})", std::process::id());
        assert!(str::contains(&text, &pid), "{text}");
        assert!(str::contains(&text, " open of soft limit "), "{text}");
        assert!(str::ends_with(&text, &format!("({emfile})")), "{text}");
        // `openpty`'s own wording carries the errno only in its message.
        let openpty = std::io::Error::other(format!(
            "failed to openpty: Os {{ code: {}, kind: Uncategorized, message: \"Too many open files\" }}",
            libc::EMFILE
        ));
        assert!(str::starts_with(&plainly(&openpty), "open-file limit reached in "));
        // A wrapped EMFILE (the reader/writer `Dup { source }` shape) is found
        // through `source()`.
        #[derive(Debug)]
        struct Wrapped(std::io::Error);
        impl std::fmt::Display for Wrapped {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("failed to dup the pty")
            }
        }
        impl std::error::Error for Wrapped {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }
        let wrapped = Wrapped(std::io::Error::from_raw_os_error(libc::EMFILE));
        assert!(str::starts_with(&plainly(&wrapped), "open-file limit reached in "));
        let wrapped_other = Wrapped(std::io::Error::from_raw_os_error(libc::ENOENT));
        assert_eq!(plainly(&wrapped_other), "failed to dup the pty");
        let other = std::io::Error::from_raw_os_error(libc::ENOENT);
        assert_eq!(plainly(&other), other.to_string());
    }
}
