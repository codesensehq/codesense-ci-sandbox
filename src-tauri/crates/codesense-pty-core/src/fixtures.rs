// SPDX-License-Identifier: MIT
//! Real-PTY fixtures shared by the registry tests and the resource leak gate:
//! isolated sessions built on the [`LocalPtyRunner`], a process-lifetime
//! blocking pool, sinks, and the process-census helpers that prove a child and
//! its process group are gone.

use std::sync::{Arc, Mutex, OnceLock};

use portable_pty::{CommandBuilder, PtySize};

use crate::domain::{PortUnavailable, TerminalDomainEvent, TerminalEventSink};
use crate::runner::{LocalPtyRunner, PtyRunner, PtySpec};
use crate::state::{PtyState, TerminalSession};
use crate::term_state::{SessionRuntime, SharedTermState, StreamId, SubscriberHandle};

/// One blocking pool for every fixture registry, created once and kept for the
/// process lifetime (as the host's runtime is) so reaps outlive their caller and
/// repeated leak-gate iterations see a stable pool.
pub(crate) fn fixture_runtime() -> tokio::runtime::Handle {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("fixture blocking runtime")
        })
        .handle()
        .clone()
}

/// A sink that drops every event.
pub(crate) struct NoopSink;

impl TerminalEventSink for NoopSink {
    fn emit(&self, _event: TerminalDomainEvent) -> Result<(), PortUnavailable> {
        Ok(())
    }
}

/// A sink that records every event, for asserting what the registry handed its
/// host.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct RecordingSink {
    pub(crate) events: Mutex<Vec<TerminalDomainEvent>>,
}

#[cfg(test)]
impl TerminalEventSink for RecordingSink {
    fn emit(&self, event: TerminalDomainEvent) -> Result<(), PortUnavailable> {
        self.events.lock().unwrap().push(event);
        Ok(())
    }
}

#[cfg(test)]
impl RecordingSink {
    pub(crate) fn snapshot(&self) -> Vec<TerminalDomainEvent> {
        self.events.lock().unwrap().clone()
    }

    /// Poll until `found` matches a recorded event, or panic at `timeout`.
    pub(crate) fn wait_for<T>(
        &self,
        timeout: std::time::Duration,
        found: impl Fn(&[TerminalDomainEvent]) -> Option<T>,
    ) -> T {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(hit) = found(&self.snapshot()) {
                return hit;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out; events: {:?}",
                self.snapshot()
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

/// `sh -c <script>`.
#[cfg(test)]
pub(crate) fn sh(script: &str) -> CommandBuilder {
    let mut command = CommandBuilder::new("sh");
    command.arg("-c");
    command.arg(script);
    command
}

/// A plain (non-pool, no shell integration) registration for `session_id`.
#[cfg(test)]
pub(crate) fn registration(session_id: &str) -> crate::SessionRegistration {
    crate::SessionRegistration {
        session_id: session_id.to_string(),
        meta: crate::SessionMeta::default(),
        home_cwd: None,
        osc_enabled: false,
        is_pool_member: false,
        scrollback: crate::emulator::DEFAULT_SCROLLBACK,
    }
}

/// Spawn `command` through the local runner and register it.
#[cfg(test)]
pub(crate) fn spawn(
    state: &PtyState,
    command: CommandBuilder,
    registration: crate::SessionRegistration,
) -> Result<(), String> {
    let pty = fixture_runtime()
        .block_on(LocalPtyRunner.spawn(PtySpec {
            command,
            size: PtySize::default(),
        }))
        .map_err(String::from)?;
    state.register(pty, registration)
}

/// A registry over the fixture pool with the given sink.
pub(crate) fn fixture_state(sink: Arc<dyn TerminalEventSink>) -> PtyState {
    let layout =
        crate::layout::LayoutModel::new(crate::layout::ServerIdentity::ephemeral("fixture"));
    PtyState::new(
        sink,
        fixture_runtime(),
        Arc::new(layout),
        crate::agent_cli::adapter,
    )
}

pub(crate) fn make_isolated_session(
    id: &str,
    command: &str,
    has_subscribers: bool,
) -> Result<TerminalSession, String> {
    let mut command_builder = CommandBuilder::new("sh");
    command_builder.arg("-c");
    command_builder.arg(command);
    make_isolated_session_with_builder(id, command_builder, has_subscribers)
}

/// A registry session over a real PTY + child, spawned through the
/// [`LocalPtyRunner`] and not yet inserted into any registry.
pub(crate) fn make_isolated_session_with_builder(
    id: &str,
    command_builder: CommandBuilder,
    has_subscribers: bool,
) -> Result<TerminalSession, String> {
    let pty = fixture_runtime()
        .block_on(LocalPtyRunner.spawn(PtySpec {
            command: command_builder,
            size: PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            },
        }))
        .map_err(String::from)?;
    let shared = Arc::new(Mutex::new(SharedTermState::default()));
    if has_subscribers {
        shared
            .lock()
            .map_err(|error| format!("subscriber lock: {error}"))?
            .subscribers
            .insert(StreamId(9999), SubscriberHandle::default());
    }
    Ok(TerminalSession {
        pty,
        story_id: None,
        workspace_id: None,
        epic_id: None,
        shared,
        cwd: Arc::new(Mutex::new(None)),
        home_cwd: None,
        runtime: Arc::new(SessionRuntime::new(id.to_string(), false)),
    })
}

/// Owns a fixture session until it is handed to a registry; kills and reaps it
/// if the fixture unwinds first.
#[cfg(target_os = "macos")]
pub(crate) struct LeakSessionGuard(Option<TerminalSession>);

#[cfg(target_os = "macos")]
impl LeakSessionGuard {
    pub(crate) fn new(session: TerminalSession) -> Self {
        Self(Some(session))
    }

    pub(crate) fn session(&self) -> &TerminalSession {
        self.0.as_ref().expect("armed leak-session guard")
    }

    pub(crate) fn take(&mut self) -> TerminalSession {
        self.0.take().expect("armed leak-session guard")
    }
}

#[cfg(target_os = "macos")]
impl Drop for LeakSessionGuard {
    fn drop(&mut self) {
        if let Some(session) = self.0.take() {
            session.pty.discard();
        }
    }
}

/// Tears a registered fixture session down when dropped.
#[cfg(target_os = "macos")]
pub(crate) struct RegisteredLeakSession<'a> {
    pub(crate) state: &'a PtyState,
    pub(crate) id: &'a str,
}

#[cfg(target_os = "macos")]
impl Drop for RegisteredLeakSession<'_> {
    fn drop(&mut self) {
        self.state
            .teardown(self.id, crate::domain::TeardownReason::Expire);
    }
}

#[cfg(unix)]
pub(crate) fn wait_for_process_exit(
    pid: libc::pid_t,
    process_group: Option<libc::pid_t>,
    stable_identity: Option<StableProcessIdentity>,
    timeout: std::time::Duration,
) -> Result<std::time::Duration, String> {
    let started = std::time::Instant::now();
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let process_exists = |target| {
            (unsafe { libc::kill(target, 0) }) == 0
                || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
        };
        let current_identity = stable_process_identity(pid)?;
        let child_reaped = !process_exists(pid);
        if !child_reaped
            && current_identity.is_some()
            && stable_identity.is_some()
            && current_identity != stable_identity
        {
            return Err(format!(
                "PTY child identity changed before reaping: pid={pid}"
            ));
        }
        let group_exists = process_group.is_some_and(|pgid| process_exists(-pgid));
        if child_reaped && !group_exists {
            return Ok(started.elapsed());
        }
        if std::time::Instant::now() >= deadline {
            if let Some(pgid) = process_group.filter(|pgid| *pgid != unsafe { libc::getpgrp() }) {
                unsafe { libc::kill(-pgid, libc::SIGKILL) };
            }
            return Err(format!(
                "LEAK[child]: process resources survived teardown deadline: pid={pid}, pgid={process_group:?}"
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StableProcessIdentity {
    pub(crate) started_seconds: u64,
    pub(crate) started_microseconds: u64,
}

#[cfg(target_os = "macos")]
pub(crate) fn stable_process_identity(
    pid: libc::pid_t,
) -> Result<Option<StableProcessIdentity>, String> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let expected = std::mem::size_of::<libc::proc_bsdinfo>();
    let filled = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            expected as libc::c_int,
        )
    };
    if filled == 0 {
        return Ok(None);
    }
    if filled < 0 || filled as usize != expected {
        return Err(format!(
            "process identity census failed for pid {pid}: {}",
            std::io::Error::last_os_error()
        ));
    }
    let info = unsafe { info.assume_init() };
    Ok(Some(StableProcessIdentity {
        started_seconds: info.pbi_start_tvsec,
        started_microseconds: info.pbi_start_tvusec,
    }))
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn stable_process_identity(
    _pid: libc::pid_t,
) -> Result<Option<StableProcessIdentity>, String> {
    Ok(None)
}

/// A stand-in emulator worker attached to a registry's link: it records every
/// request the host writes, can stall its stdin like a worker that stopped
/// reading, and answers whatever the test sends back. Snapshot requests it
/// answers itself, as the worker would: at the position of the last request
/// before them.
#[cfg(test)]
pub(crate) struct FakeWorker {
    pipe: Arc<FakePipe>,
    pub(crate) generation: u64,
    replies: std::sync::mpsc::Sender<Vec<u8>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(test)]
impl Drop for FakeWorker {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Answer every snapshot request in `requests` not in `answered`, from the
/// requests before it.
#[cfg(test)]
fn answer_snapshots(
    requests: &[serde_json::Value],
    answered: &mut std::collections::HashSet<u64>,
    replies: &std::sync::mpsc::Sender<Vec<u8>>,
) {
    use serde_json::json;
    let mut terminals: std::collections::HashMap<u64, serde_json::Value> = Default::default();
    for request in requests {
        let session = request["session"].as_u64().unwrap_or_default();
        match request["type"].as_str() {
            Some("create") => {
                terminals.insert(
                    session,
                    json!({
                        "cols": request["cols"], "rows": request["rows"],
                        "seq": request["seq_base"], "resize_gen": request["resize_gen_base"],
                        "scrollback": request["scrollback"], "vt": "",
                    }),
                );
            }
            Some("feed") => {
                if let Some(t) = terminals.get_mut(&session) {
                    t["seq"] = request["seq_end"].clone();
                    let vt = format!(
                        "{}{}",
                        t["vt"].as_str().unwrap(),
                        request["data"].as_str().unwrap()
                    );
                    t["vt"] = vt.into();
                }
            }
            Some("resize") => {
                if let Some(t) = terminals.get_mut(&session) {
                    t["cols"] = request["cols"].clone();
                    t["rows"] = request["rows"].clone();
                    t["resize_gen"] = request["resize_gen"].clone();
                }
            }
            Some("snapshot") => {
                let id = request["id"].as_u64().unwrap();
                if answered.insert(id) {
                    let reply = json!({
                        "type": "snapshot", "id": id, "session": session,
                        "snapshot": terminals.get(&session).cloned().unwrap_or_default(),
                    });
                    let _ = replies.send(serde_json::to_vec(&reply).unwrap());
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
#[derive(Default)]
struct FakePipe {
    bytes: Mutex<Vec<u8>>,
    stalled: Mutex<bool>,
    unstalled: std::sync::Condvar,
    /// Keep nothing (a throughput run).
    discard: bool,
}

#[cfg(test)]
struct FakeStdin(Arc<FakePipe>);

#[cfg(test)]
impl std::io::Write for FakeStdin {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut stalled = self.0.stalled.lock().unwrap();
        while *stalled {
            stalled = self.0.unstalled.wait(stalled).unwrap();
        }
        drop(stalled);
        if !self.0.discard {
            self.0.bytes.lock().unwrap().extend_from_slice(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
impl FakeWorker {
    pub(crate) fn attach(state: &PtyState) -> Self {
        Self::attach_to(state, false)
    }

    /// A worker that takes every request and keeps none.
    pub(crate) fn attach_discarding(state: &PtyState) -> Self {
        Self::attach_to(state, true)
    }

    fn attach_to(state: &PtyState, discard: bool) -> Self {
        let pipe = Arc::new(FakePipe {
            discard,
            ..FakePipe::default()
        });
        let (replies, frames) = std::sync::mpsc::channel();
        let generation = state
            .emulator
            .attach(FakeStdin(pipe.clone()), frames)
            .expect("attach the fake worker");
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker = Self {
            pipe,
            generation,
            replies,
            stop: stop.clone(),
        };
        if !discard {
            let pipe = worker.pipe.clone();
            let replies = worker.replies.clone();
            std::thread::spawn(move || {
                let mut answered = std::collections::HashSet::new();
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let requests = FakeWorker::parse(&pipe.bytes.lock().unwrap());
                    answer_snapshots(&requests, &mut answered, &replies);
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
            });
        }
        worker
    }

    /// Every request written so far, in order.
    pub(crate) fn requests(&self) -> Vec<serde_json::Value> {
        let bytes = self.pipe.bytes.lock().unwrap().clone();
        Self::parse(&bytes)
    }

    fn parse(bytes: &[u8]) -> Vec<serde_json::Value> {
        let mut input = bytes;
        let mut requests = Vec::new();
        while let Ok(Some(frame)) = crate::emulator::wire::read_frame(&mut input) {
            requests.push(serde_json::from_slice(&frame).unwrap());
        }
        requests
    }

    /// Wait until the requests satisfy `done`, and return them.
    pub(crate) fn wait_for(
        &self,
        timeout: std::time::Duration,
        done: impl Fn(&[serde_json::Value]) -> bool,
    ) -> Vec<serde_json::Value> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let requests = self.requests();
            if done(&requests) {
                return requests;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out; requests: {requests:#?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Stop (or resume) reading the host's requests.
    pub(crate) fn stall(&self, stalled: bool) {
        *self.pipe.stalled.lock().unwrap() = stalled;
        self.pipe.unstalled.notify_all();
    }

    pub(crate) fn reply(&self, reply: serde_json::Value) {
        self.replies
            .send(serde_json::to_vec(&reply).unwrap())
            .unwrap();
    }
}
