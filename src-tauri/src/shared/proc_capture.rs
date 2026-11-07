// SPDX-License-Identifier: MIT
//! SW-GT-8: the ONE subprocess spawn + capture + wall-clock-timeout primitive.
//!
//! Extracted from `agent_runner::run_agent` (SW-SD-2) so its spawn / thread-drained
//! stdout+stderr / kill-on-timeout / optional-stdin core is shared by both the agent
//! runner and the SW-GT-8 command→verdict evaluator (and, downstream, SW-GT-9
//! `loop.until` and SW-GT-19 watch `when`). Build it once here; those reactors
//! **consume, don't re-implement**.
//!
//! **Synchronous/blocking** — callers on an async runtime MUST offload it (see
//! `commands::await_blocking`). It holds no policy: it does not decide what an exit
//! code *means* (that is the caller's matcher / error mapping), only that the child
//! ran, what it emitted, and whether it had to be killed.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often the wait loop polls the child before re-checking the wall clock.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The read-buffer size for an incremental drain. Doubles as the backend batch
/// unit: one blocking `read` coalesces whatever the OS pipe has buffered, so a
/// burst of small writes surfaces as a few chunks — never one event per line.
const DRAIN_CHUNK: usize = 64 * 1024;

/// Which child stream an incremental [`CaptureChunk`] was drained from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureStream {
    Stdout,
    Stderr,
}

/// SW-OR-13: an incremental slice of a child's output, delivered to a
/// [`CaptureRequest::output_sink`] as it is drained — before the child exits. The
/// same bytes are still accumulated into the final [`CaptureOutput`], so a sink is
/// purely additive (it observes, it does not divert).
pub struct CaptureChunk {
    pub stream: CaptureStream,
    pub bytes: Vec<u8>,
}

/// Drain a child pipe to EOF on its own thread so a full pipe buffer can never
/// block the wait loop. `None` (an unpiped stream) yields no thread.
///
/// SW-OR-13: each read is appended to the returned buffer AND, when `sink` is present,
/// streamed incrementally — a copy of the chunk to `sink`. With `sink` `None` this is
/// byte-for-byte a `read_to_end` (same final buffer, same error-swallowing), so every
/// pre-OR-13 caller is unchanged.
fn spawn_drain<R: Read + Send + 'static>(
    stream: Option<R>,
    kind: CaptureStream,
    sink: Option<Sender<CaptureChunk>>,
    last_drain_ms: Option<Arc<AtomicU64>>,
    max_output_bytes: Option<usize>,
) -> Option<std::thread::JoinHandle<Vec<u8>>> {
    stream.map(|mut r| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let mut chunk = [0u8; DRAIN_CHUNK];
            loop {
                match r.read(&mut chunk) {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(limit) = max_output_bytes {
                            if buf.len() > limit {
                                buf.drain(..buf.len() - limit);
                            }
                        }
                        // SW-OR-19: stamp the wall-clock moment output was seen BEFORE handing the
                        // chunk to the sink. The emitter reads this anchor when it receives the
                        // chunk off the channel; storing first means the mpsc send→recv happens-
                        // before edge guarantees the emitter observes THIS chunk's stamp — so a
                        // heartbeat can never carry the stale `0` seed for a chunk that already
                        // arrived. Both drain threads (stdout/stderr) share the one Arc, so the slot
                        // holds the most-recent activity across streams. Wall-clock epoch-ms (not
                        // `Instant`) because the value crosses to the FE and is subtracted against
                        // `Date.now()`, like `started_at_ms`. `Relaxed`: a standalone timestamp, so
                        // eventual visibility is all a reader needs (the channel supplies the fence);
                        // cheaper in this per-chunk hot loop than the SeqCst the pid slot needs.
                        // `None` writes nothing (byte-identical for every pre-OR-19 caller).
                        if let Some(ts) = &last_drain_ms {
                            ts.store(crate::runtime::now_ms(), Ordering::Relaxed);
                        }
                        if let Some(tx) = &sink {
                            // A dropped receiver (panel closed) just discards the
                            // send — the drain keeps running so the final buffer and
                            // the persisted artifact stay complete.
                            let _ = tx.send(CaptureChunk {
                                stream: kind,
                                bytes: chunk[..n].to_vec(),
                            });
                        }
                    }
                    // `read_to_end` retries EINTR; match it so behavior is identical.
                    Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    // Swallow a real read error and keep what we have, exactly as the
                    // prior `let _ = r.read_to_end(&mut buf)` did.
                    Err(_) => break,
                }
            }
            buf
        })
    })
}

/// SW-OR-13: whether a cooperative-cancel flag is present and set — the ONE predicate the
/// pre-spawn guard and the wait loop share, so the two cancellation checks can never drift.
fn cancel_requested(cancel: &Option<Arc<AtomicBool>>) -> bool {
    cancel.as_ref().is_some_and(|c| c.load(Ordering::SeqCst))
}

/// A fully-specified subprocess to spawn. `program` + `args` are the raw argv (no
/// shell is interposed); a caller that wants shell semantics passes
/// `program = "/bin/sh"`, `args = ["-c", line]`.
pub struct CaptureRequest {
    /// The binary to execute.
    pub program: PathBuf,
    /// The argv (excluding argv0).
    pub args: Vec<String>,
    /// Working directory the child is spawned in; `None` inherits the app cwd.
    pub cwd: Option<PathBuf>,
    /// Env vars applied **over** the inherited environment (e.g. an override
    /// `PATH`). Empty = pure inherit.
    pub env: Vec<(String, String)>,
    /// SW-AN-46: env var names to STRIP from the inherited environment before spawn (the
    /// PTY-less parity of a terminal spawn's `env_remove`) — e.g. the launched adapter's
    /// `env_suppress` so a headless agent can't see a host API key that bypasses managed
    /// auth. Applied BEFORE `env`, so an explicit override in `env` still wins. Default empty
    /// = strip nothing (byte-identical for every pre-AN-46 caller).
    pub env_remove: Vec<String>,
    /// Bytes fed to the child's stdin, then EOF. `None` gives the child a null
    /// stdin (immediate EOF) — the right default for a validator command that
    /// takes no input.
    pub stdin: Option<Vec<u8>>,
    /// Wall-clock ceiling; the child is killed if it runs longer.
    pub timeout: Duration,
    /// SW-AN-23: run the child in its OWN process group (unix) and, on timeout, kill
    /// the WHOLE group — not just the direct child. Without this, a child that forks a
    /// pipe-holding grandchild (an install script backgrounding `npm i` etc.) keeps our
    /// stdout/stderr pipes open after the direct child is killed, so the drain threads
    /// block past the wall clock and the timeout is defeated. Default `false` preserves
    /// the single-child behavior for every existing caller (agent runner, evaluators);
    /// only the caller that asks for it opts in. No-op on non-unix.
    ///
    /// Residual (unchanged by this flag): a child that exits 0 but leaves a backgrounded
    /// grandchild holding the stdout/stderr pipe still blocks the drain until that
    /// grandchild closes it — the group kill fires only on the *timeout* path, not on a
    /// clean exit. A well-behaved install script that daemonizes should redirect/close
    /// its stdio; capturing output inherently waits for the pipe writers to close.
    pub process_group: bool,
    /// SW-OR-10: an optional slot the spawned child's PID is published into **the moment
    /// it is spawned**, so an EXTERNAL supervisor can terminate a still-running child
    /// before the wall-clock timeout. Paired with `process_group: true`, the published id
    /// is also the child's pgid, so the supervisor can `killpg` the whole tree. The value
    /// is `0` until spawn; a `killpg` on it after the child exits is a harmless ESRCH.
    /// `None` (every existing caller) publishes nothing — byte-identical prior behavior.
    /// This does NOT change the wait/timeout loop; a killed child is observed via the
    /// normal `try_wait`.
    pub pid_sink: Option<Arc<AtomicU32>>,
    /// SW-OR-13: an incremental output sink. When present, each drained stdout/stderr
    /// chunk is streamed here (a copy) as it arrives — in addition to the final
    /// buffer, never instead of it. The channel closes when both drain threads reach
    /// EOF, so the receiver learns the stream ended. `None` (every pre-OR-13 caller)
    /// streams nothing — byte-identical prior behavior.
    pub output_sink: Option<Sender<CaptureChunk>>,
    /// SW-OR-19: an optional last-output timestamp slot. When present, each drained
    /// stdout/stderr chunk stores the current wall-clock epoch-ms into it (both drain
    /// threads share the one Arc). A supervisor reads it to surface a fleet-glance
    /// "last output Ns ago" heartbeat on the agent row without a per-chunk reload.
    /// `None` (every pre-OR-19 caller) stamps nothing — byte-identical prior behavior.
    pub last_drain_ms: Option<Arc<AtomicU64>>,
    /// SW-OR-13: cooperative cancellation. Checked once just before spawn — closing
    /// the pre-spawn race where `pid_sink` is still `0` so no external `killpg` can yet
    /// reach the child — and on every wait-loop poll. When it flips `true` the child is
    /// killed exactly like a timeout and [`CaptureOutput::cancelled`] is set. `None`
    /// never cancels (byte-identical prior behavior).
    pub cancel: Option<Arc<AtomicBool>>,
    /// Retain only the tail of each stream; None preserves full capture.
    pub max_output_bytes: Option<usize>,
}

/// Raw outcome of a captured spawn. `status` is `None` **iff** the child was killed
/// on timeout (`timed_out == true`). `stdout`/`stderr` are the full captured bytes.
#[derive(Debug)]
pub struct CaptureOutput {
    /// Exit status, or `None` if the child was killed on timeout.
    pub status: Option<ExitStatus>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// True iff the child exceeded `timeout` and was killed.
    pub timed_out: bool,
    /// SW-OR-13: true iff a cooperative `cancel` fired — either before spawn (no child
    /// ever ran; `stdout`/`stderr` empty) or mid-run (child killed). Distinct from
    /// `timed_out`: a cancel is a user Stop, a timeout is the wall-clock ceiling.
    pub cancelled: bool,
}

impl CaptureOutput {
    /// The process exit code, or `None` if it was killed / died by signal / timed
    /// out (no code available).
    pub fn exit_code(&self) -> Option<i32> {
        self.status.and_then(|s| s.code())
    }
}

/// Why a spawn could not produce a [`CaptureOutput`] at all. Split by syscall so a
/// caller can preserve its own phrasing (e.g. `run_agent`'s "failed to spawn
/// agent" vs "failed to wait for agent").
#[derive(Debug)]
pub enum CaptureError {
    Spawn(std::io::Error),
    Wait(std::io::Error),
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureError::Spawn(e) => write!(f, "failed to spawn: {e}"),
            CaptureError::Wait(e) => write!(f, "failed to wait: {e}"),
        }
    }
}

/// Spawn `req`, feed optional stdin + drain stdout/stderr on dedicated threads, and
/// wait with a wall-clock timeout (killing a hung child). Only a spawn or wait
/// syscall failure is an `Err`; a non-zero exit or a timeout is a normal
/// [`CaptureOutput`].
///
/// The three-thread I/O drive is load-bearing: a child that emits more than a pipe
/// buffer (~64 KiB) would otherwise block on a full stdout pipe and only die at the
/// kill, and a child given a stdin larger than the buffer that never drains it
/// would block our `write_all` forever. Off-thread, the timeout loop always
/// governs: a hung child is killed, which breaks its pipes and lets the helper
/// threads finish. A stdin write error (SIGPIPE from an early-exiting child) is
/// intentionally ignored — the child's own exit/timeout is the authority.
pub fn spawn_capture(req: CaptureRequest) -> Result<CaptureOutput, CaptureError> {
    let mut command = Command::new(&req.program);
    command
        .args(&req.args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if req.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    // SW-AN-46: strip guarded vars from the inherited env FIRST, so an explicit `env`
    // override below still wins on a colliding key.
    for name in &req.env_remove {
        command.env_remove(name);
    }
    for (k, v) in &req.env {
        command.env(k, v);
    }
    if let Some(dir) = req.cwd.as_deref() {
        command.current_dir(dir);
    }
    // SW-AN-23: put the child in a fresh process group (pgid == its own pid) so a
    // timeout can SIGKILL the whole tree, not just the leader. Unix-only; a no-op flag
    // elsewhere.
    #[cfg(unix)]
    if req.process_group {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    // SW-OR-13: pre-spawn cancellation. A Stop that lands in the window before the
    // child exists (`pid_sink` still `0`, so no external `killpg` can reach it) aborts
    // HERE — the narrow residual SW-OR-10 documented — instead of producing a
    // momentarily-unkillable child that runs to the wall-clock ceiling.
    if cancel_requested(&req.cancel) {
        return Ok(CaptureOutput {
            status: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
            timed_out: false,
            cancelled: true,
        });
    }

    let mut child = command.spawn().map_err(CaptureError::Spawn)?;

    // SW-OR-10: publish the live child's PID so an external supervisor can terminate it
    // (with `process_group`, this id is the pgid → a `killpg` reaps the whole tree). Done
    // the instant the child exists, before the (possibly long) wait below.
    if let Some(sink) = &req.pid_sink {
        sink.store(child.id(), Ordering::SeqCst);
    }

    let stdout_reader = spawn_drain(
        child.stdout.take(),
        CaptureStream::Stdout,
        req.output_sink.clone(),
        req.last_drain_ms.clone(),
        req.max_output_bytes,
    );
    let stderr_reader = spawn_drain(
        child.stderr.take(),
        CaptureStream::Stderr,
        req.output_sink.clone(),
        req.last_drain_ms.clone(),
        req.max_output_bytes,
    );
    let stdin_writer = match (child.stdin.take(), req.stdin) {
        (Some(mut stdin), Some(bytes)) => Some(std::thread::spawn(move || {
            // Ignore write errors (broken pipe if the child exits before draining);
            // dropping `stdin` at thread end signals EOF.
            let _ = stdin.write_all(&bytes);
        })),
        _ => None,
    };

    let start = Instant::now();
    let mut timed_out = false;
    let mut cancelled = false;
    let status = loop {
        // Keep the group leader unreaped while descendants can hold its pipes.
        // Its reserved pid keeps both cooperative cancellation and killpg safe.
        let pipes_open = cfg!(unix)
            && req.process_group
            && (stdout_reader.as_ref().is_some_and(|h| !h.is_finished())
                || stderr_reader.as_ref().is_some_and(|h| !h.is_finished())
                || stdin_writer.as_ref().is_some_and(|h| !h.is_finished()));
        let waited = if pipes_open {
            Ok(None)
        } else {
            child.try_wait()
        };
        match waited {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                // SW-OR-13: a mid-run cooperative Stop kills the child exactly like a
                // timeout. `release_for_session`'s external `killpg` also handles this,
                // but the token makes cancellation robust even in the sliver where the
                // pid is not yet published — and unifies the kill path.
                let cancel_now = cancel_requested(&req.cancel);
                if cancel_now || start.elapsed() >= req.timeout {
                    // SW-AN-23: kill the whole process group BEFORE reaping the leader
                    // (its pid is still valid here), so a pipe-holding grandchild dies
                    // too and the drain threads below can reach EOF. Falls back to a
                    // lone-child kill when process groups aren't requested / available.
                    #[cfg(unix)]
                    if req.process_group {
                        // Safety: `killpg` just sends a signal; an empty/gone group is a
                        // harmless ESRCH. pgid == the (not-yet-reaped) child's pid.
                        unsafe {
                            libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
                        }
                    }
                    let _ = child.kill();
                    let _ = child.wait();
                    if cancel_now {
                        cancelled = true;
                    } else {
                        timed_out = true;
                    }
                    break None;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(e) => return Err(CaptureError::Wait(e)),
        }
    };

    // SW-OR-10: the child is now REAPED (both loop arms `wait`/`try_wait`-reap it), so its
    // pid is free for OS reuse — clear the published slot immediately so an external
    // supervisor's later `killpg` (e.g. `release_for_session` on the natural-completion
    // path) can never signal a recycled, unrelated process group. The pid is thus non-zero
    // ONLY while the child's pid is still reserved.
    if let Some(sink) = &req.pid_sink {
        sink.store(0, Ordering::SeqCst);
    }

    // The threads finish once the pipes close (on exit or kill).
    if let Some(h) = stdin_writer {
        let _ = h.join();
    }
    let stdout = stdout_reader
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();
    let stderr = stderr_reader
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();

    Ok(CaptureOutput {
        status,
        stdout,
        stderr,
        timed_out,
        cancelled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn test_dir(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join("codesense_proc_capture_test")
            .join(format!("{}_{}_{}", label, std::process::id(), id));
        let _ = fs::create_dir_all(&dir);
        dir
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    fn write_script(dir: &Path, script: &str) -> PathBuf {
        let path = dir.join("cmd");
        fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    fn req(program: PathBuf) -> CaptureRequest {
        CaptureRequest {
            program,
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            stdin: None,
            timeout: Duration::from_secs(10),
            process_group: false,
            pid_sink: None,
            output_sink: None,
            last_drain_ms: None,
            max_output_bytes: None,
            cancel: None,
        }
    }

    #[test]
    fn bounded_capture_retains_stream_tails() {
        let mut request = req(PathBuf::from("/bin/sh"));
        request.args = vec![
            "-c".into(),
            "printf '%1000s' ''; printf end; printf '%1000s' '' >&2; printf err >&2".into(),
        ];
        request.max_output_bytes = Some(32);
        let out = spawn_capture(request).unwrap();
        assert_eq!(out.stdout.len(), 32);
        assert_eq!(out.stderr.len(), 32);
        assert!(out.stdout.ends_with(b"end"));
        assert!(out.stderr.ends_with(b"err"));
    }

    #[test]
    fn captures_exit_code_and_streams() {
        let dir = test_dir("exit");
        let bin = write_script(&dir, "#!/bin/sh\necho out\necho err >&2\nexit 3");
        let out = spawn_capture(req(bin)).unwrap();
        assert_eq!(out.exit_code(), Some(3));
        assert!(!out.timed_out);
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "out");
        assert_eq!(String::from_utf8_lossy(&out.stderr).trim(), "err");
        cleanup(&dir);
    }

    #[test]
    fn env_remove_strips_inherited_var_and_env_add_wins() {
        // SW-AN-46 (AC-3): a name in `env_remove` is stripped from the child's inherited
        // environment; an explicit `env` add on the same key still wins (env_remove applies
        // FIRST). Uniquely-named probes avoid collisions with parallel tests.
        std::env::set_var("AN46_STRIP_PROBE", "inherited");
        std::env::set_var("AN46_KEEP_PROBE", "inherited");
        let out = spawn_capture(CaptureRequest {
            args: vec![
                "-c".to_string(),
                "printf '%s|%s' \"${AN46_STRIP_PROBE:-GONE}\" \"${AN46_KEEP_PROBE:-GONE}\""
                    .to_string(),
            ],
            env: vec![("AN46_KEEP_PROBE".to_string(), "override".to_string())],
            env_remove: vec![
                "AN46_STRIP_PROBE".to_string(),
                "AN46_KEEP_PROBE".to_string(),
            ],
            ..req(PathBuf::from("/bin/sh"))
        })
        .unwrap();
        std::env::remove_var("AN46_STRIP_PROBE");
        std::env::remove_var("AN46_KEEP_PROBE");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "GONE|override");
    }

    #[test]
    fn zero_exit_is_success() {
        let dir = test_dir("ok");
        let bin = write_script(&dir, "#!/bin/sh\nexit 0");
        let out = spawn_capture(req(bin)).unwrap();
        assert_eq!(out.exit_code(), Some(0));
        assert!(out.status.unwrap().success());
        cleanup(&dir);
    }

    #[test]
    fn spawn_failure_is_err() {
        let err = spawn_capture(req(PathBuf::from("/nonexistent/bin"))).unwrap_err();
        assert!(matches!(err, CaptureError::Spawn(_)));
        assert!(err.to_string().contains("failed to spawn"));
    }

    #[test]
    fn pid_sink_is_published_then_cleared_when_the_child_is_reaped() {
        // SW-OR-10: a supervisor reads the sink to `killpg` a LIVE child; once the child
        // is reaped its pid is reusable, so `spawn_capture` MUST reset the sink to 0 — else
        // a later killpg could hit a recycled, unrelated process group.
        let dir = test_dir("pidsink");
        let bin = write_script(&dir, "#!/bin/sh\nexit 0");
        let sink = Arc::new(AtomicU32::new(0));
        let mut r = req(bin);
        r.pid_sink = Some(sink.clone());
        let out = spawn_capture(r).unwrap();
        assert_eq!(out.exit_code(), Some(0));
        assert_eq!(
            sink.load(Ordering::SeqCst),
            0,
            "the sink is reset to 0 once the child is reaped (no killpg on a reused pid)"
        );
        cleanup(&dir);
    }

    #[test]
    fn output_sink_streams_chunks_and_final_buffer_is_intact() {
        // SW-OR-13 (AC-1): with a sink, each drained chunk is streamed live AND the
        // full buffer still lands in CaptureOutput — the sink is additive, not a divert.
        let dir = test_dir("sink");
        let bin = write_script(&dir, "#!/bin/sh\necho out\necho err >&2\nexit 0");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut r = req(bin);
        r.output_sink = Some(tx);
        let out = spawn_capture(r).unwrap();
        // The sender clones drop at drain-EOF, so the channel closes and this drains it.
        let mut streamed_out = Vec::new();
        let mut streamed_err = Vec::new();
        for chunk in rx {
            match chunk.stream {
                CaptureStream::Stdout => streamed_out.extend_from_slice(&chunk.bytes),
                CaptureStream::Stderr => streamed_err.extend_from_slice(&chunk.bytes),
            }
        }
        assert_eq!(streamed_out, out.stdout, "streamed stdout == final buffer");
        assert_eq!(streamed_err, out.stderr, "streamed stderr == final buffer");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "out");
        assert_eq!(String::from_utf8_lossy(&out.stderr).trim(), "err");
        cleanup(&dir);
    }

    #[test]
    fn last_drain_ms_advances_to_a_fresh_timestamp_on_drained_output() {
        // SW-OR-19 (AC-1/AC-3): with the slot present it holds `0` until output, then a
        // wall-clock epoch-ms no older than a stamp taken just before the run — the fleet-row
        // heartbeat anchor. Without the slot (`None`) the run is byte-identical (covered by the
        // other capture tests, which all pass `None`).
        let dir = test_dir("lastdrain");
        let bin = write_script(&dir, "#!/bin/sh\necho out\necho err >&2\nexit 0");
        let slot = Arc::new(AtomicU64::new(0));
        let before = crate::runtime::now_ms();
        let mut r = req(bin);
        r.last_drain_ms = Some(slot.clone());
        let out = spawn_capture(r).unwrap();
        assert_eq!(out.exit_code(), Some(0));
        let stamped = slot.load(Ordering::SeqCst);
        assert!(
            stamped >= before,
            "the slot advances to a fresh epoch-ms once output drains (got {stamped}, before {before})"
        );
        cleanup(&dir);
    }

    #[test]
    fn pre_spawn_cancel_returns_without_running_a_child() {
        // SW-OR-13 (AC-4): a cancel already set before spawn aborts immediately — the
        // side-effecting script must NOT run, proving no child was spawned.
        let dir = test_dir("precancel");
        let marker = dir.join("ran");
        let bin = write_script(
            &dir,
            &format!("#!/bin/sh\ntouch {}\n", marker.to_string_lossy()),
        );
        let cancel = Arc::new(AtomicBool::new(true));
        let mut r = req(bin);
        r.cancel = Some(cancel);
        let out = spawn_capture(r).unwrap();
        assert!(out.cancelled, "pre-spawn cancel is reported");
        assert!(!out.timed_out);
        assert!(out.status.is_none());
        assert!(
            !marker.exists(),
            "the child must never have run (no side effect)"
        );
        cleanup(&dir);
    }

    #[test]
    fn mid_run_cancel_kills_promptly() {
        // SW-OR-13 (AC-4): a cancel flipped while a long child runs interrupts it well
        // before the wall-clock timeout, reported as `cancelled` (not `timed_out`).
        let dir = test_dir("midcancel");
        // sleep 30 (not 5): the child must OUTLIVE any parallel-suite scheduler stall. If the wait
        // loop is starved past a shorter sleep, the child exits naturally before the loop observes
        // the cancel → `out.cancelled` would flake false. A 30s child is reliably still running when
        // the loop next polls (cancel was set at 150ms), so the kill path is taken deterministically.
        // This default-mode contract owns only the direct child.
        let bin = write_script(&dir, "#!/bin/sh\nexec sleep 30");
        let cancel = Arc::new(AtomicBool::new(false));
        let flip = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            flip.store(true, Ordering::SeqCst);
        });
        let mut r = req(bin);
        r.timeout = Duration::from_secs(60);
        r.cancel = Some(cancel);
        let start = Instant::now();
        let out = spawn_capture(r).unwrap();
        assert!(out.cancelled, "mid-run cancel is reported");
        assert!(!out.timed_out, "a cancel is not a timeout");
        // `out.cancelled` above is the real check; this anti-hang bound (10s — generous vs the child's
        // 30s sleep) just proves promptness without flaking under parallel-suite scheduler jitter.
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "cancel interrupts long before the ceiling"
        );
        cleanup(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn group_cancel_closes_pipes_held_by_a_descendant() {
        let dir = test_dir("group_cancel");
        let bin = write_script(
            &dir,
            "#!/bin/sh\n(sh -c 'echo descendant-ready; exec sleep 30') &\nwait",
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let flip = cancel.clone();
        let (tx, rx) = std::sync::mpsc::channel::<CaptureChunk>();
        // Cancel only after the descendant is known to hold the output pipe.
        let canceller = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut output = Vec::new();
            while !String::from_utf8_lossy(&output).contains("descendant-ready") {
                let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                    break;
                };
                match rx.recv_timeout(remaining) {
                    Ok(chunk) => output.extend(chunk.bytes),
                    Err(_) => break,
                }
            }
            flip.store(true, Ordering::SeqCst);
            String::from_utf8_lossy(&output).contains("descendant-ready")
        });
        let mut r = req(bin);
        r.process_group = true;
        r.timeout = Duration::from_secs(60);
        r.cancel = Some(cancel);
        r.output_sink = Some(tx);
        let start = Instant::now();
        let out = spawn_capture(r).unwrap();
        assert!(
            canceller.join().unwrap(),
            "descendant must have started before cancellation"
        );
        assert!(out.cancelled);
        assert!(!out.timed_out);
        assert!(out.status.is_none());
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "cancel must close descendant pipes before its 30s sleep ends"
        );
        cleanup(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn group_timeout_closes_pipes_held_by_a_descendant() {
        let dir = test_dir("group_timeout");
        // Both parent and descendant keep the captured pipes open. A leader-only
        // kill would block the reader joins for the descendant's full 30 seconds.
        let bin = write_script(
            &dir,
            "#!/bin/sh\n(sh -c 'echo descendant-ready; exec sleep 30') &\nwait",
        );
        let mut r = req(bin);
        r.process_group = true;
        r.timeout = Duration::from_secs(2);
        let start = Instant::now();
        let out = spawn_capture(r).unwrap();
        assert!(out.timed_out);
        assert!(out.status.is_none());
        assert!(String::from_utf8_lossy(&out.stdout).contains("descendant-ready"));
        // Returning means both capture readers observed EOF, proving that the
        // descendant lost its pipe handles as well as the parent being reaped.
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "descendant kept capture pipes open"
        );
        cleanup(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn group_timeout_still_applies_after_the_leader_exits() {
        let dir = test_dir("exited_group_timeout");
        let bin = write_script(
            &dir,
            "#!/bin/sh\nsleep 300 &\nprintf background-ready\nexit 0",
        );
        let mut r = req(bin);
        r.process_group = true;
        r.timeout = Duration::from_secs(2);
        let start = Instant::now();
        let out = spawn_capture(r).unwrap();
        assert!(out.timed_out);
        assert!(out.status.is_none());
        assert!(String::from_utf8_lossy(&out.stdout).contains("background-ready"));
        assert!(start.elapsed() < Duration::from_secs(10));
        cleanup(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn group_cancel_still_applies_after_the_leader_exits() {
        let dir = test_dir("exited_group_cancel");
        let bin = write_script(
            &dir,
            "#!/bin/sh\nsleep 300 &\nprintf background-ready\nexit 0",
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut r = req(bin);
        r.process_group = true;
        r.cancel = Some(cancel.clone());
        r.output_sink = Some(sender);
        let capture = std::thread::spawn(move || spawn_capture(r).unwrap());
        receiver.recv_timeout(Duration::from_secs(10)).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        cancel.store(true, Ordering::SeqCst);
        let out = capture.join().unwrap();
        assert!(out.cancelled);
        assert!(out.status.is_none());
        assert!(String::from_utf8_lossy(&out.stdout).contains("background-ready"));
        cleanup(&dir);
    }

    #[test]
    fn times_out_and_kills_with_no_status() {
        let dir = test_dir("timeout");
        // sleep 30 (not 5): the child must OUTLIVE any parallel-suite scheduler stall, so the
        // 200ms-timeout wait loop reliably observes it still RUNNING and takes the kill path. A
        // shorter sleep could finish naturally during a starvation stall → `out.timed_out` flakes.
        // Default capture deliberately kills only the direct child.
        let bin = write_script(&dir, "#!/bin/sh\nexec sleep 30");
        let mut r = req(bin);
        r.timeout = Duration::from_millis(200);
        let start = Instant::now();
        let out = spawn_capture(r).unwrap();
        assert!(out.timed_out);
        assert!(out.status.is_none());
        assert_eq!(out.exit_code(), None);
        // `out.timed_out` above is the real check; this anti-hang bound (10s) proves the kill
        // happened (a failed kill would run the child's full 30s) without flaking under jitter.
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "should kill promptly"
        );
        cleanup(&dir);
    }

    #[test]
    fn runs_in_cwd() {
        let dir = test_dir("cwd_bin");
        let anchor = test_dir("cwd_anchor");
        let anchor_canon = anchor.canonicalize().unwrap();
        let bin = write_script(&dir, "#!/bin/sh\npwd -P");
        let mut r = req(bin);
        r.cwd = Some(anchor_canon.clone());
        let out = spawn_capture(r).unwrap();
        assert_eq!(
            fs::canonicalize(String::from_utf8_lossy(&out.stdout).trim()).unwrap(),
            anchor_canon
        );
        cleanup(&dir);
        cleanup(&anchor);
    }

    #[test]
    fn applies_env_overrides() {
        let dir = test_dir("env");
        let bin = write_script(&dir, "#!/bin/sh\necho \"$CS_GT8_MARK\"");
        let mut r = req(bin);
        r.env = vec![("CS_GT8_MARK".to_string(), "hello".to_string())];
        let out = spawn_capture(r).unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello");
        cleanup(&dir);
    }

    #[test]
    fn feeds_optional_stdin() {
        let dir = test_dir("stdin");
        let bin = write_script(&dir, "#!/bin/sh\ncat");
        let mut r = req(bin);
        r.stdin = Some(b"piped-in".to_vec());
        let out = spawn_capture(r).unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "piped-in");
        cleanup(&dir);
    }

    #[test]
    fn null_stdin_gives_immediate_eof() {
        // With no stdin, a `cat` sees EOF at once and exits cleanly rather than
        // blocking — the right default for a validator command.
        let dir = test_dir("nullstdin");
        let bin = write_script(&dir, "#!/bin/sh\ncat\necho done");
        let out = spawn_capture(req(bin)).unwrap();
        assert!(out.status.unwrap().success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "done");
        cleanup(&dir);
    }

    #[test]
    fn drains_large_output_without_deadlock() {
        // ~270 KiB of stdout, past the ~64 KiB pipe buffer: concurrent draining
        // must let it complete fast rather than blocking on a full pipe.
        let dir = test_dir("large");
        let bin = write_script(
            &dir,
            "#!/bin/sh\nawk 'BEGIN{for(i=0;i<8192;i++)printf \"0123456789ABCDEF0123456789ABCDEF\\n\"}'",
        );
        let mut r = req(bin);
        r.timeout = Duration::from_secs(10);
        let start = Instant::now();
        let out = spawn_capture(r).unwrap();
        assert!(out.stdout.len() > 128 * 1024, "got {}", out.stdout.len());
        assert!(start.elapsed() < Duration::from_secs(5));
        cleanup(&dir);
    }
}
