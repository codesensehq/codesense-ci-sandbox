// SPDX-License-Identifier: MIT
//! The interactive PTY runtime, free of any Tauri coupling: the spawn port and
//! its local driver, the live session registry, the 16 ms reader/batcher
//! pipeline with its emulator feed and OSC side-tap, warm-pool member handoff,
//! teardown + reaping, and the session runtime's subscribe/submit protocols.
//!
//! Every host (the in-process app and the session server) builds one
//! [`TerminalHost`] — its session registry plus the [`layout`] it owns — from its
//! [`domain::TerminalEventSink`] adapter and a blocking-pool handle, resolves a
//! [`runner::PtyRunner`], and resolves each [`spawn::SpawnIntent`] into a
//! [`runner::PtySpec`] against its own [`spawn::HostEnvironment`]; every side effect
//! the runtime produces flows back through that one sink. Every host answers
//! the one [`domain::TerminalDomain`] surface; the sidecar speaks [`protocol`]
//! to its client, which answers it too. The registry itself is crate-private.

pub mod activity;
pub mod agent_cli;
mod batcher;
pub mod domain;
pub mod emulator;
#[cfg(any(test, feature = "leak-gate"))]
mod fixtures;
pub mod git_prompt;
mod host;
pub mod inbox_host;
pub mod inbox_waiters;
pub mod layout;
#[cfg(feature = "leak-gate")]
pub mod leak_gate;
pub mod memorable_name;
pub use codesense_host_proto::nofile;
pub mod notify_hook;
pub mod osc;
pub mod process;
pub mod protocol;
#[cfg(any(debug_assertions, feature = "tui-fixture"))]
mod ptylog;
#[cfg(debug_assertions)]
mod recorder;
pub mod routine_arm;
pub mod runner;
pub mod session_wake;
pub mod shell;
pub mod spawn;
mod state;
pub mod term_state;
#[cfg(feature = "tui-fixture")]
pub mod tui_fixture;
mod warm;

pub use host::TerminalHost;
pub use state::{LeftoverReap, SessionFacts, SessionMeta, SessionRegistration};
pub use warm::{is_warm_safe_cwd, shell_quote};

#[cfg(test)]
mod tests;

pub mod context_batch;
pub mod remote_executor;
pub mod remote_files;
pub mod remote_manifest;
pub mod remote_workspace;

pub mod remote_export;
