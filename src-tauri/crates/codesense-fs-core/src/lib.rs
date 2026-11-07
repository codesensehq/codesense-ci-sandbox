// SPDX-License-Identifier: MIT
//! The worktree file service, free of any Tauri coupling: gated directory
//! listing and text reads, the hidden/secret/VCS filters every listing applies,
//! the root gate (a lexical allow-list over an injected root set), the natural
//! sort order a listing is returned in, and the debounced worktree watch.
//!
//! A host supplies only its gate — an [`FsGate`] says which paths it admits —
//! and every list/read goes through the one [`FsService`] implementation here.
//! The in-process app gates on its context repo plus the registered worktree
//! roots (lexically, following symlinks as the local editor always has); a
//! [`RootSetFs`] gates on an explicit root set and also confines where a path
//! resolves.
//!
//! **Watch semantics.** [`watch::attach`] registers one *recursive* OS watch on
//! the worktree root (plus a best-effort watch on its git ref locations) and
//! filters the event stream through an excluded heavy-dir list and a 50 ms
//! leading-edge debounce. That is what the local worktree editor uses.

pub mod filters;
pub mod gate;
mod listing;
pub mod natural_sort;
mod read;
mod service;
pub mod watch;

pub use listing::FileEntry;
pub use service::{FsGate, FsService, RootSetFs};
