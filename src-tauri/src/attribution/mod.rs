// SPDX-License-Identifier: MIT
//! Line attribution: which agent session wrote each committed line.
//!
//! CodeSense reads attribution through [`ranges::AttributionSource`], so the capture tool behind it
//! (git-ai, in [`git_ai`]) can change without touching a caller. The launch side
//! ([`launch`]) switches capture on for the managed worktrees CodeSense launches agents
//! into and records whether it is working ([`AttributionStatus`]).

pub(crate) mod authorship;
pub(crate) mod catch_up;
pub(crate) mod watcher;
pub(crate) mod checkpoints;
pub(crate) mod detect;
pub(crate) mod diff;
pub(crate) mod git_ai;
pub(crate) mod history;
pub(crate) mod install;
pub(crate) mod launch;
pub(crate) mod managed;
#[cfg(test)]
mod measurement;
pub mod prefs;
pub(crate) mod ranges;
pub(crate) mod setup;
#[cfg(test)]
pub(crate) mod testing;
pub(crate) mod version;
pub(crate) mod why_line;
pub(crate) mod turns;
pub(crate) mod unsupported;
mod why_line_cache;

use serde::{Deserialize, Serialize};

/// Why attribution is off for a worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OffReason {
    /// The repo's setting is `off`.
    DisabledForRepo,
    /// The git-ai in use cannot keep this repo's notes local (it refused the local-notes
    /// config), so capture would push them; a repo set `notes: push` still captures. Also
    /// used while an old managed daemon awaits a restart onto the pinned build and config.
    NotesWouldPush,
    /// The launch directory is not a managed worktree, so there is nothing to record on.
    NotManagedWorktree,
    /// The worktree's repo isn't attached on this Mac, so git-ai's daemon, scoped to attached
    /// clones, ignores it. Attaching its clone tracks the next launch, with no app restart.
    RepoNotAttached,
}

/// Whether attribution is working for a worktree, computed at launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttributionStatus {
    /// Wired: the env and checkpoint hooks are on the launch. Not proof that a
    /// checkpoint has been seen.
    Active,
    Off {
        reason: OffReason,
    },
    NotInstalled,
    /// Installation is in progress; sessions already launched need a relaunch.
    Installing,
    InstallFailed {
        message: String,
    },
    UnsupportedVersion {
        found: String,
    },
    /// This kind of launch cannot be captured (a remote machine, OpenCode).
    Unsupported {
        reason: String,
    },
    Error {
        message: String,
    },
}

/// The status of the last launch into a worktree, with the git-ai it ran against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributionState {
    pub status: AttributionStatus,
    pub git_ai_version: Option<String>,
    /// Unix milliseconds the launch computed it at.
    pub checked_at: u64,
}

/// What a remote-machine launch reports: capture is not wired there.
#[allow(dead_code)] // reported once a remote launch has a record to hold it (SV-CDE-13+)
pub(crate) fn remote_status() -> AttributionStatus {
    AttributionStatus::Unsupported {
        reason: "remote_machine".into(),
    }
}

#[cfg(test)]
mod context_capture_tests;
#[cfg(test)]
mod guard_tests;
#[cfg(test)]
mod notes_tests;
#[cfg(test)]
mod repo_roots_tests;
#[cfg(test)]
pub(crate) mod unsupported_tests;
