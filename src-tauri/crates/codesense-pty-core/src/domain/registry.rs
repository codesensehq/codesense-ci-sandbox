// SPDX-License-Identifier: MIT
//! The Tauri-free seam between the session runtime and the live session
//! registry. Each method encapsulates exactly one atomic region over the
//! registry's locks, reproducing the registry's lock order (`sessions` → per-session
//! `shared`/`writer` → `stream_index`). The runtime composes these calls with
//! the domain ports and its own settle/confirm coordination; the concrete
//! `PtyState` implementation owns the lock discipline and process/teardown
//! machinery.
//!
//! Errors are `String` on purpose: the command layer's observable error text
//! (e.g. `write_pty`'s not-found signal) must survive byte-for-byte,
//! and a `String` carries no framework type — the domain stays Tauri-free.

use std::sync::{Arc, Mutex};

use crate::emulator::TerminalSnapshot;
use crate::term_state::{SessionRuntime, SharedTermState, StreamId};

/// The per-session handles + immutable metadata a viewport attach needs, cloned
/// out from under the `sessions` map lock in one pass (the map lock is released
/// before the caller does any settle waiting or snapshot copying).
pub struct SessionView {
    /// The per-session replay + subscriber state, guarded by its own lock. The
    /// runtime polls its watermark during the cold-settle wait and snapshots it
    /// at attach time.
    pub shared: Arc<Mutex<SharedTermState>>,
    /// The live control shared with the batcher — read lock-free for the
    /// `suppress_until_prompt` warm-handoff gate during the attach wait.
    pub runtime: Arc<SessionRuntime>,
    pub story_id: Option<String>,
    pub workspace_id: Option<String>,
    pub epic_id: Option<String>,
}

/// What a completed viewport attach hands back — the emulator's terminal the
/// display is built from, and the new stream, which carries what follows it.
pub struct AttachOutcome {
    pub snapshot: TerminalSnapshot,
    pub stream_id: StreamId,
}

/// The live session registry the runtime operates over. Implemented for the
/// process-global `PtyState`; faked in the deterministic domain suite.
pub trait TerminalRegistry: Send + Sync {
    /// Clone a session's shared handle + metadata under a brief `sessions` lock.
    /// The lock is released on return, so no later settle wait or snapshot copy
    /// holds the global map. A missing session is a hard error (the exact
    /// subscribe message is preserved).
    fn session_view(&self, session: &str) -> Result<SessionView, String>;

    /// Clone just a session's shared handle (the submit path polls its watermark
    /// and needs nothing else). A missing session is a hard error (parity with
    /// `write`).
    fn shared_handle(&self, session: &str) -> Result<Arc<Mutex<SharedTermState>>, String>;

    /// Register a new subscriber and enqueue the emulator's snapshot atomically
    /// against the batcher (one `shared` lock), publish the stream into the
    /// index, re-check the session was not displaced by a same-id respawn in
    /// the gap, then wait (off the lock) for the snapshot. A displaced session
    /// or a snapshot that does not come undoes the registration and fails.
    /// `shared` is the handle a prior [`session_view`](Self::session_view)
    /// returned.
    fn attach_viewport(
        &self,
        session: &str,
        shared: &Arc<Mutex<SharedTermState>>,
    ) -> Result<AttachOutcome, String>;

    /// Resize a session's PTY and its emulator terminal, and tell each attached
    /// viewport stream when the size changed, all in one region against the
    /// batcher. A missing session is a no-op (parity with the pre-extraction
    /// command).
    fn resize(&self, session: &str, cols: u16, rows: u16) -> Result<(), String>;

    /// Write bytes to a session's PTY off the `sessions` map lock. A missing
    /// session is a hard error — the exact message is preserved.
    fn write(&self, session: &str, bytes: &[u8]) -> Result<(), String>;

    /// Whether the session's shell foreground is idle — no live descendant
    /// process under it (the process-tree census, kept on the registry side so
    /// process inspection goes through the registry trait). A missing or
    /// exited session reports idle.
    fn is_idle(&self, session: &str) -> Result<bool, String>;

    /// Tear down a session (deliberate human close): remove it under the map
    /// lock, kill the child, prune its stream-index entries, reap off the
    /// caller's path, and hand the host its teardown side effects (dashboard
    /// slot release + audit, scoped MCP config, launch provenance) through the
    /// event sink. A missing id is a no-op; idempotent.
    fn kill(&self, session: &str);
}
