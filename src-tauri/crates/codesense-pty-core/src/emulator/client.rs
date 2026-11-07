// SPDX-License-Identifier: MIT
//! The host's side of the worker conversation: one FIFO queue of requests,
//! drained to the worker's stdin by a writer thread, and a reader thread that
//! routes the worker's replies.
//!
//! Sessions only ever enqueue, under their own session lock, so subscriber
//! fan-out never waits on the worker: a copy into the queue is all a batch
//! costs. The queue is never lossy while a worker is attached. Bytes are the
//! truth, and a worker that falls behind shows up as lag, not as dropped
//! output.
//!
//! Each worker the supervisor attaches is a new *generation*. Whatever was
//! queued for the one before is dropped with it, and a session's next message
//! first re-creates its terminal in the new worker at the session's current
//! position (`history_lost`). That happens under the session's lock, before
//! the message itself, so no `feed` ever reaches a worker ahead of its
//! session's `create`.

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard};
use std::time::Instant;

use log::{debug, warn};

use super::wire::{write_frame, EmulatorDump, EmulatorText, Reply, Request, TextMode};
use super::TerminalSnapshot;

/// Scrollback of a session whose spawn did not name one.
pub const DEFAULT_SCROLLBACK: u32 = 5_000;

/// Lag at which a session's `emulator_lag_bytes` is logged.
const LAG_LOG_BYTES: u64 = 1024 * 1024;

/// The queue and reply routing shared by every session of one host.
pub struct EmulatorLink {
    state: Mutex<LinkState>,
    wake: Condvar,
    /// Replies awaited by request id.
    pending: Mutex<HashMap<u64, mpsc::Sender<Reply>>>,
    /// Per session key: the last `seq_end` enqueued and the last one the
    /// worker parsed.
    progress: Mutex<HashMap<u64, Progress>>,
    next_request: AtomicU64,
    next_key: AtomicU64,
}

#[derive(Default)]
struct LinkState {
    /// Bumped on every attach; 0 before the first worker.
    generation: u64,
    attached: bool,
    queue: VecDeque<Request>,
    /// When the attached worker last replied (or was attached).
    last_reply: Option<Instant>,
}

#[derive(Debug, Default, Clone, Copy)]
struct Progress {
    enqueued: u64,
    applied: u64,
}

impl Default for EmulatorLink {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            wake: Condvar::new(),
            pending: Mutex::default(),
            progress: Mutex::default(),
            // 0 is the `hello` id.
            next_request: AtomicU64::new(1),
            next_key: AtomicU64::new(1),
        }
    }
}

impl EmulatorLink {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> MutexGuard<'_, LinkState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn progress(&self) -> MutexGuard<'_, HashMap<u64, Progress>> {
        self.progress.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn pending(&self) -> MutexGuard<'_, HashMap<u64, mpsc::Sender<Reply>>> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Hand a freshly started worker to the link: its stdin and the frames it
    /// writes. Starts that worker's writer and reader and returns its
    /// generation, which [`detach`](Self::detach) takes when it exits.
    pub fn attach(
        self: &Arc<Self>,
        mut stdin: impl Write + Send + 'static,
        frames: mpsc::Receiver<Vec<u8>>,
    ) -> Result<u64, String> {
        let generation = {
            let mut state = self.lock();
            state.generation += 1;
            state.attached = true;
            state.queue.clear();
            state.last_reply = Some(Instant::now());
            state.generation
        };
        // Requests to an earlier worker will not be answered: their waiters
        // see the channel close.
        self.pending().clear();
        self.progress().clear();
        self.wake.notify_all();

        let writer = self.clone();
        std::thread::Builder::new()
            .name("emulator-writer".into())
            .spawn(move || writer.drain(generation, &mut stdin))
            .map_err(|e| format!("emulator writer thread: {e}"))?;
        let reader = self.clone();
        std::thread::Builder::new()
            .name("emulator-replies".into())
            .spawn(move || {
                for frame in frames {
                    reader.route(generation, &frame);
                }
            })
            .map_err(|e| format!("emulator reply thread: {e}"))?;
        Ok(generation)
    }

    /// The worker of `generation` is gone: drop what was queued for it and
    /// stop queueing until the next attach.
    pub fn detach(&self, generation: u64) {
        {
            let mut state = self.lock();
            if state.generation != generation {
                return;
            }
            state.attached = false;
            state.queue.clear();
        }
        self.pending().clear();
        self.wake.notify_all();
    }

    /// Bytes enqueued for session `key` that the worker has not parsed yet.
    pub fn lag_bytes(&self, key: u64) -> u64 {
        self.progress()
            .get(&key)
            .map_or(0, |p| p.enqueued.saturating_sub(p.applied))
    }

    /// Whether the worker owes the host anything: output it has not parsed
    /// yet, or a reply someone is waiting for.
    pub(super) fn has_outstanding(&self) -> bool {
        self.progress().values().any(|p| p.enqueued > p.applied) || !self.pending().is_empty()
    }

    /// When the worker of `generation` last replied; `None` once it is
    /// replaced or detached.
    pub(super) fn last_reply(&self, generation: u64) -> Option<Instant> {
        let state = self.lock();
        (state.generation == generation && state.attached)
            .then_some(state.last_reply)
            .flatten()
    }

    fn next_key(&self) -> u64 {
        self.next_key.fetch_add(1, Ordering::Relaxed)
    }

    /// Write queued requests to the worker of `generation` until it is
    /// replaced or detached.
    fn drain(&self, generation: u64, stdin: &mut impl Write) {
        loop {
            let request = {
                let mut state = self.lock();
                loop {
                    if state.generation != generation || !state.attached {
                        return;
                    }
                    if let Some(request) = state.queue.pop_front() {
                        break request;
                    }
                    state = self.wake.wait(state).unwrap_or_else(|e| e.into_inner());
                }
            };
            let written = serde_json::to_vec(&request)
                .map_err(|e| e.to_string())
                .and_then(|payload| write_frame(stdin, &payload).map_err(|e| e.to_string()));
            if let Err(error) = written {
                // The worker is gone; the supervisor detaches it and starts
                // the next one.
                warn!("emulator worker stopped taking requests: {error}");
                return;
            }
        }
    }

    fn route(&self, generation: u64, frame: &[u8]) {
        let current = {
            let mut state = self.lock();
            let current = state.generation == generation;
            if current {
                state.last_reply = Some(Instant::now());
            }
            current
        };
        let reply = match serde_json::from_slice::<Reply>(frame) {
            Ok(reply) => reply,
            Err(error) => {
                warn!("emulator worker: unreadable reply: {error}");
                return;
            }
        };
        let id = match &reply {
            Reply::Applied {
                session, seq_end, ..
            } => {
                if current {
                    self.record_applied(*session, *seq_end);
                }
                return;
            }
            Reply::Hello { .. } => return,
            Reply::Dump(dump) => dump.id,
            Reply::Text(text) => text.id,
            Reply::Snapshot(snapshot) => snapshot.id,
            Reply::Error { id: Some(id), .. } => *id,
            Reply::Error { id: None, message } => {
                warn!("emulator worker: {message}");
                return;
            }
        };
        if let Some(waiter) = self.pending().remove(&id) {
            let _ = waiter.send(reply);
        }
    }

    fn record_applied(&self, key: u64, seq_end: u64) {
        let mut progress = self.progress();
        let Some(entry) = progress.get_mut(&key) else {
            return;
        };
        entry.applied = entry.applied.max(seq_end);
        let lag = entry.enqueued.saturating_sub(entry.applied);
        if lag >= LAG_LOG_BYTES {
            debug!("emulator session {key}: emulator_lag_bytes={lag}");
        }
    }
}

/// A queued read's answer, awaited off the session lock.
pub struct PendingReply(mpsc::Receiver<Reply>);

impl PendingReply {
    fn wait(self, timeout: std::time::Duration) -> Result<Reply, String> {
        match self.0.recv_timeout(timeout) {
            Ok(Reply::Error { message, .. }) => Err(format!("emulator worker: {message}")),
            Ok(reply) => Ok(reply),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err(format!("emulator worker did not answer within {timeout:?}"))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err("emulator worker restarted before answering".into())
            }
        }
    }

    pub fn dump(self, timeout: std::time::Duration) -> Result<EmulatorDump, String> {
        match self.wait(timeout)? {
            Reply::Dump(dump) => Ok(dump),
            other => Err(format!("emulator worker: expected a dump, got {other:?}")),
        }
    }

    pub fn text(self, timeout: std::time::Duration) -> Result<EmulatorText, String> {
        match self.wait(timeout)? {
            Reply::Text(text) => Ok(text),
            other => Err(format!("emulator worker: expected text, got {other:?}")),
        }
    }

    pub fn snapshot(self, timeout: std::time::Duration) -> Result<TerminalSnapshot, String> {
        match self.wait(timeout)? {
            Reply::Snapshot(reply) => Ok(reply.snapshot),
            other => Err(format!(
                "emulator worker: expected a snapshot, got {other:?}"
            )),
        }
    }
}

/// One session's terminal in the worker, owned by the session's shared state
/// and used only under its lock.
pub struct SessionEmulator {
    link: Arc<EmulatorLink>,
    key: u64,
    cols: u16,
    rows: u16,
    scrollback: u32,
    resize_gen: u32,
    /// The worker generation that holds this terminal; 0 when none does yet.
    created_in: u64,
    /// The stream position this terminal starts at with nothing before it,
    /// until its first `create`. Any later `create` (a restarted worker) or
    /// one at another position has lost the output before it.
    origin: Option<u64>,
}

impl SessionEmulator {
    /// A new terminal for a session whose stream starts at `origin`, created
    /// in the worker at once when one is attached.
    pub fn start(
        link: &Arc<EmulatorLink>,
        (cols, rows): (u16, u16),
        scrollback: u32,
        origin: u64,
    ) -> Self {
        let mut emulator = Self {
            link: link.clone(),
            key: link.next_key(),
            cols,
            rows,
            scrollback,
            resize_gen: 0,
            created_in: 0,
            origin: Some(origin),
        };
        emulator.send(origin, None);
        emulator
    }

    /// The worker's key for this terminal.
    pub fn key(&self) -> u64 {
        self.key
    }

    pub fn resize_gen(&self) -> u32 {
        self.resize_gen
    }

    /// Enqueue output delivered to the session's displays, ending the stream
    /// at `seq_end`.
    pub fn feed(&mut self, seq_end: u64, data: &str) {
        let start = seq_end.saturating_sub(data.len() as u64);
        let feed = Request::Feed {
            session: self.key,
            seq_end,
            data: data.to_string(),
        };
        if self.send(start, Some(feed)) {
            if let Some(progress) = self.link.progress().get_mut(&self.key) {
                progress.enqueued = seq_end;
            }
        }
    }

    /// Apply a PTY resize made at stream position `seq`. A resize to the
    /// current size changes nothing and sends nothing; `false` then.
    pub fn resize(&mut self, (cols, rows): (u16, u16), seq: u64) -> bool {
        if (cols, rows) == (self.cols, self.rows) {
            return false;
        }
        self.cols = cols;
        self.rows = rows;
        self.resize_gen += 1;
        let resize = Request::Resize {
            session: self.key,
            cols,
            rows,
            resize_gen: self.resize_gen,
        };
        self.send(seq, Some(resize));
        true
    }

    /// Ask for the terminal's dump, answered after everything enqueued before
    /// it. `None` when no worker is attached.
    pub fn dump(&mut self, seq: u64) -> Option<PendingReply> {
        let id = self.link.next_request.fetch_add(1, Ordering::Relaxed);
        self.request(
            id,
            seq,
            Request::Dump {
                id,
                session: self.key,
            },
        )
    }

    /// Ask for the terminal's text (see [`TextMode`]).
    pub fn text(&mut self, seq: u64, lines: u32, mode: TextMode) -> Option<PendingReply> {
        let id = self.link.next_request.fetch_add(1, Ordering::Relaxed);
        let text = Request::Text {
            id,
            session: self.key,
            lines,
            mode,
        };
        self.request(id, seq, text)
    }

    /// Ask for the terminal serialized, answered after everything enqueued
    /// before it; the worker reports the position it describes.
    pub fn snapshot(&mut self, seq: u64) -> Option<PendingReply> {
        let id = self.link.next_request.fetch_add(1, Ordering::Relaxed);
        self.request(
            id,
            seq,
            Request::Snapshot {
                id,
                session: self.key,
            },
        )
    }

    /// A new process took over the session in place, its output starting at
    /// `seq_base`, on a PTY of `size`. The worker starts it a fresh terminal
    /// holding the old normal buffer and a seam line.
    pub fn respawn(&mut self, seq_base: u64, size: (u16, u16)) {
        {
            let mut state = self.link.lock();
            if state.attached && self.created_in == state.generation {
                state.queue.push_back(Request::Respawn {
                    session: self.key,
                    seq_base,
                    resize_gen_base: self.resize_gen,
                });
                drop(state);
                self.reset_progress(seq_base);
                self.link.wake.notify_all();
            }
        }
        // Otherwise the next message re-creates it at `seq_base`, history
        // lost.
        self.resize(size, seq_base);
        self.send(seq_base, None);
    }

    /// Replace the terminal with an empty one of `size` whose stream starts at
    /// `origin` (a warm-pool member handed off: its boot output is dropped).
    pub fn restart(&mut self, origin: u64, (cols, rows): (u16, u16)) {
        self.dispose_in_worker();
        self.key = self.link.next_key();
        self.cols = cols;
        self.rows = rows;
        self.created_in = 0;
        self.origin = Some(origin);
        self.send(origin, None);
    }

    /// The session is gone: free its terminal in the worker.
    pub fn dispose(mut self) {
        self.dispose_in_worker();
    }

    fn dispose_in_worker(&mut self) {
        {
            let mut state = self.link.lock();
            if state.attached && self.created_in == state.generation {
                state
                    .queue
                    .push_back(Request::Dispose { session: self.key });
            }
        }
        self.link.progress().remove(&self.key);
        self.link.wake.notify_all();
    }

    fn request(&mut self, id: u64, seq: u64, request: Request) -> Option<PendingReply> {
        let (tx, rx) = mpsc::channel();
        self.link.pending().insert(id, tx);
        if self.send(seq, Some(request)) {
            Some(PendingReply(rx))
        } else {
            self.link.pending().remove(&id);
            None
        }
    }

    /// Enqueue `message`, first creating the terminal in the attached worker
    /// if it does not hold it yet, at stream position `seq`. `false` when no
    /// worker is attached (nothing is queued; the next message after an
    /// attach re-creates the terminal).
    fn send(&mut self, seq: u64, message: Option<Request>) -> bool {
        let mut state = self.link.lock();
        if !state.attached {
            return false;
        }
        let created = self.created_in != state.generation;
        if created {
            state.queue.push_back(Request::Create {
                session: self.key,
                cols: self.cols,
                rows: self.rows,
                scrollback: self.scrollback,
                seq_base: seq,
                resize_gen_base: self.resize_gen,
                history_lost: self.origin != Some(seq),
            });
            self.created_in = state.generation;
            self.origin = None;
        }
        if let Some(message) = message {
            state.queue.push_back(message);
        }
        drop(state);
        if created {
            self.reset_progress(seq);
        }
        self.link.wake.notify_all();
        true
    }

    fn reset_progress(&self, seq: u64) {
        self.link.progress().insert(
            self.key,
            Progress {
                enqueued: seq,
                applied: seq,
            },
        );
    }
}
