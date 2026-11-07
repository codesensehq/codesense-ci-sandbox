// SPDX-License-Identifier: MIT
//! The emulator worker every terminal host runs: a JS process holding one
//! headless xterm per session, fed over its stdin/stdout ([`wire`]) through the
//! host's [`EmulatorLink`], spawned and restarted by the [`EmulatorSupervisor`]. It lives here so the in-process
//! host, the session-server sidecar and remote host agents all get it the same
//! way.

mod client;
#[cfg(any(test, feature = "test-support"))]
mod echo;
mod supervisor;
pub mod wire;

pub use client::{EmulatorLink, PendingReply, SessionEmulator, DEFAULT_SCROLLBACK};
#[cfg(any(test, feature = "test-support"))]
pub use echo::attach_echo;
pub use supervisor::{
    EmulatorConfig, EmulatorError, EmulatorStatus, EmulatorSupervisor, APPLY_DEADLINE,
};
pub use wire::{EmulatorDump, EmulatorText, TextMode, WorkerInfo};

use serde::{Deserialize, Serialize};

/// What a display is rebuilt from: the worker's terminal serialized (xterm.js
/// SerializeAddon output plus what the addon misses) with the size and stream
/// position it was taken at. Written into a fresh xterm of `cols × rows`, `vt`
/// reproduces the terminal exactly; output with `seq_end > seq` and resizes
/// with `resize_gen > resize_gen` follow it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSnapshot {
    pub cols: u16,
    pub rows: u16,
    /// Byte offset of the last output the snapshot holds.
    pub seq: u64,
    /// Resizes applied up to `seq`.
    pub resize_gen: u32,
    /// The session's scrollback-lines setting.
    pub scrollback: u32,
    pub vt: String,
}

#[cfg(any(test, feature = "test-support"))]
impl TerminalSnapshot {
    /// An 80×24 snapshot holding `vt`, ending at `seq`, for tests.
    pub fn of_text(vt: &str, seq: u64) -> Self {
        Self {
            cols: 80,
            rows: 24,
            seq,
            resize_gen: 0,
            scrollback: DEFAULT_SCROLLBACK,
            vt: vt.to_string(),
        }
    }
}

/// The worker executable's name as shipped: in the app bundle's
/// `Contents/MacOS` and in a remote machine's version directory. Build
/// artifacts carry a `-<target triple>` suffix.
pub const WORKER_BIN: &str = "codesense-emulator";
