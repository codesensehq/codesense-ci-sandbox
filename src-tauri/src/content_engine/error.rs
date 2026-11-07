// SPDX-License-Identifier: MIT
use std::fmt;
use std::io;
use std::path::PathBuf;

use crate::models::error::AppError;

/// Failure modes produced anywhere inside [`crate::content_engine`].
///
/// The module owns its own error type rather than extending [`AppError`]
/// so consumers (which are landing across SS.4a / SS.4b / SS.5 / SS.11)
/// can evolve without forcing churn into the shared `AppError` enum.
/// Conversion to `AppError` lives at the Tauri command boundary via the
/// [`From<Error> for AppError`] impl below — a one-way edge.
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    /// A notify-backend failure (watch limit exceeded, path not found,
    /// platform error). The structured `notify::Error` is preserved so
    /// callers can branch on `kind()` without re-parsing a string.
    Notify(notify::Error),
    /// `git rev-parse` exited non-zero — the candidate path is not a git
    /// working tree. The stderr is captured for diagnostics (kept short).
    NotAGitWorkingTree {
        path: PathBuf,
        stderr: String,
    },
    /// The `git` binary is missing from PATH (`io::ErrorKind::NotFound`
    /// when spawning).
    GitNotFound,
    /// The `# >>> codesense managed` / `# <<< codesense managed` block
    /// markers are malformed — either an unterminated start, a stray end,
    /// or nested markers.
    InvalidExcludeBlock {
        reason: String,
    },
    /// A caller-supplied entry for `git_exclude::add_entries` is
    /// structurally invalid (empty, contains a newline, or collides with
    /// a block-marker line). Rejected before any disk write so the
    /// managed block cannot be corrupted by injection.
    InvalidEntry {
        entry: String,
        reason: &'static str,
    },
    /// A v3 [`super::lockfile::Lockfile`] file was read whose
    /// `schema_version` does not match the expected
    /// [`super::lockfile::SCHEMA_VERSION`]. SS-3 introduces v3; v1 and v2
    /// lockfiles live at disjoint paths (see SS-3 AC-11) and are migrated
    /// by SS-12. This variant fires defensively against the unlikely case
    /// of a path-crossing regression that hands an older lockfile to the
    /// v3 reader.
    UnsupportedSchemaVersion {
        found: u32,
        expected: u32,
    },
    /// A `team_name` argument to
    /// [`super::lockfile::lockfile_path`] failed slug-safety validation
    /// (`/`, `\`, `.`, `..`, control characters, NUL, empty string,
    /// whitespace-only, leading dot, or collision with the reserved
    /// `_none` no-team segment). The `reason` carries the specific rule
    /// that fired.
    InvalidTeamName {
        reason: String,
    },
    /// An `org_slug` derived from a [`super::layers::ResolverContext`]
    /// failed slug-safety validation when passed to
    /// [`super::lockfile::lockfile_path`]. Same rule set as
    /// [`Error::InvalidTeamName`]. Defensive: most callers source
    /// `org_slug` from `normalize_source_repo_url`, which already
    /// rejects unsafe forms, but `lockfile_path` validates anyway since
    /// `ResolverContext` exposes the field publicly.
    InvalidOrgSlug {
        reason: String,
    },
    /// A [`crate::domain::SourceRepoSlug`] field (`owner` or `repo`)
    /// failed slug-safety validation. The struct's `pub` fields admit
    /// hand-construction that bypasses `SourceRepoSlug::new`'s
    /// constructor guard; SS-3's resolver and lockfile path math add
    /// defense-in-depth validation so a hand-built slug like
    /// `SourceRepoSlug { owner: "..".into(), repo: "..".into() }`
    /// can't escape the codesense_home tree.
    InvalidSourceRepoSlug {
        field: &'static str,
        reason: String,
    },
    /// SS-4a: a `Mutex` inside [`crate::content_engine::watcher::Watcher`]
    /// is poisoned (a prior holder panicked while inside the critical
    /// section). Surfaced from `Watcher::watch` / `Watcher::unwatch` so
    /// the caller can decide whether to recreate the watcher or treat as
    /// fatal. Today's `commands/watch.rs` callers map to a string error
    /// at the Tauri boundary (same disposition as the existing R-2
    /// `WatcherState` mutex-poisoning logs).
    WatcherLockPoisoned,
    /// SS-4c: [`crate::content_engine::sync::WaveDeps::from_app`] required
    /// `WatcherState` to be registered as Tauri-managed state, but
    /// `app.try_state::<WatcherState>()` returned `None`. Precondition
    /// gate — never fires in production because Tauri setup registers
    /// `WatcherState` before any command can call into the wave engine.
    /// Test fixtures that construct an `AppHandle` without `WatcherState`
    /// surface this variant.
    WatcherStateNotRegistered,
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Error::Io(err)
    }
}

impl From<notify::Error> for Error {
    fn from(err: notify::Error) -> Self {
        Error::Notify(err)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {}", e),
            Error::Notify(e) => write!(f, "watcher error: {}", e),
            Error::NotAGitWorkingTree { path, stderr } => write!(
                f,
                "not a git working tree at {}: {}",
                path.display(),
                stderr.trim()
            ),
            Error::GitNotFound => write!(f, "git binary not found on PATH"),
            Error::InvalidExcludeBlock { reason } => {
                write!(
                    f,
                    "invalid codesense managed block in info/exclude: {}",
                    reason
                )
            }
            Error::InvalidEntry { entry, reason } => {
                write!(f, "invalid exclude entry {:?}: {}", entry, reason)
            }
            Error::UnsupportedSchemaVersion { found, expected } => write!(
                f,
                "unsupported lockfile schema version: found {}, expected {}",
                found, expected
            ),
            Error::InvalidTeamName { reason } => {
                write!(f, "invalid team name: {}", reason)
            }
            Error::InvalidOrgSlug { reason } => {
                write!(f, "invalid org slug: {}", reason)
            }
            Error::InvalidSourceRepoSlug { field, reason } => {
                write!(f, "invalid SourceRepoSlug.{}: {}", field, reason)
            }
            Error::WatcherLockPoisoned => {
                write!(f, "content_engine watcher mutex is poisoned")
            }
            Error::WatcherStateNotRegistered => write!(
                f,
                "WaveDeps::from_app: WatcherState is not registered as Tauri-managed state"
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::Notify(e) => Some(e),
            _ => None,
        }
    }
}

impl From<Error> for AppError {
    fn from(err: Error) -> Self {
        AppError::InstallFailed {
            reason: err.to_string(),
        }
    }
}
