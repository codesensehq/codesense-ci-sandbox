// SPDX-License-Identifier: MIT
//! The read side: the adapter interface and what it returns.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Where a range's attribution comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Basis {
    /// Read from a git-ai note on the commit that introduced the lines.
    Exact,
    /// Nothing recorded the lines: no note on their commit, or not in the note. This is
    /// never "human": a line nobody tracked may have come from anyone.
    Unknown,
}

/// Tool name of a line a human wrote, as git-ai records it.
pub(crate) const HUMAN_TOOL: &str = "human";

/// A run of consecutive lines (1-based, inclusive) with one origin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LineRange {
    pub start: u32,
    pub end: u32,
    /// The commit that introduced these lines, as followed by blame.
    pub commit: Option<String>,
    /// git-ai checkpoint trace; absent for human, legacy prompt keys and unknown lines.
    pub trace_id: Option<String>,
    /// `claude`, `codex`, ... or [`HUMAN_TOOL`]; `None` when the basis is unknown.
    pub tool: Option<String>,
    /// The agent CLI's own session id (`None` for a human line or an unknown one).
    pub native_session_id: Option<String>,
    /// The model, when the capture tool knew it.
    pub model: Option<String>,
    pub basis: Basis,
}

pub(crate) const OBJECTS_NOT_LOCAL: &str = "objects not in local copy";
pub(crate) const NO_LOCAL_COPY: &str = "no local copy";
pub(crate) const REPO_NOT_FOUND: &str = "repository not found locally";
pub(crate) const REVISION_NOT_LOCAL: &str = "revision not in local copy";
pub(crate) const PATH_NOT_IN_REVISION: &str = "path not in revision";
/// The needed history is cut off by a shallow copy; `refresh_history` deepens it.
pub(crate) const HISTORY_PAST_SHALLOW: &str = "history past shallow boundary; call refresh_history";
/// Some of a story's merges were found, but the scan reached the shallow boundary first.
pub(crate) const HISTORY_PAST_SHALLOW_PARTIAL: &str =
    "history past shallow boundary; older merges may be missing; call refresh_history";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AttributionError {
    /// `git` could not read the path at the revision.
    Git(String),
    /// A required object is promised by a partial clone but is not available locally.
    ObjectsNotLocal,
}

impl std::fmt::Display for AttributionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Git(message) => write!(f, "git: {message}"),
            Self::ObjectsNotLocal => write!(f, "{OBJECTS_NOT_LOCAL}"),
        }
    }
}

/// Reads line attribution out of a repository.
pub(crate) trait AttributionSource {
    /// The origin of every line of `path` at `rev` in `repo`, as consecutive ranges in
    /// line order.
    fn ranges(
        &self,
        repo: &Path,
        rev: &str,
        path: &str,
    ) -> Result<Vec<LineRange>, AttributionError>;
}
