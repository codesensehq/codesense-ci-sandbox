// SPDX-License-Identifier: MIT
use std::fmt;

/// Application error types
#[derive(Debug)]
pub enum AppError {
    SymlinkFailed {
        repo: String,
        path: String,
        reason: String,
    },
    GitignoreWriteFailed {
        repo: String,
        reason: String,
    },
    ConfigInvalid {
        reason: String,
    },
    GitCloneFailed {
        url: String,
        reason: String,
    },
    LockFileFailed {
        path: String,
        reason: String,
    },
    InstallFailed {
        reason: String,
    },
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::SymlinkFailed { repo, path, reason } => {
                write!(f, "Symlink failed for {} at {}: {}", repo, path, reason)
            }
            AppError::GitignoreWriteFailed { repo, reason } => {
                write!(f, "Gitignore write failed for {}: {}", repo, reason)
            }
            AppError::ConfigInvalid { reason } => {
                write!(f, "Config invalid: {}", reason)
            }
            AppError::GitCloneFailed { url, reason } => {
                write!(f, "Git clone failed for {}: {}", url, reason)
            }
            AppError::LockFileFailed { path, reason } => {
                write!(f, "Lock file error at {}: {}", path, reason)
            }
            AppError::InstallFailed { reason } => {
                write!(f, "Install failed: {}", reason)
            }
        }
    }
}

impl std::error::Error for AppError {}

impl From<AppError> for String {
    fn from(err: AppError) -> String {
        err.to_string()
    }
}

/// Build an `AppError::InstallFailed` whose reason is `"{ctx}: {e}"`.
///
/// Used at every `map_err` site in the install pipeline that needs to wrap an
/// underlying I/O / parse error with contextual information. Sites that have
/// no underlying error (only a static message) keep building `InstallFailed`
/// inline.
pub fn install_err<E: std::fmt::Display>(ctx: &str, e: E) -> AppError {
    AppError::InstallFailed {
        reason: format!("{}: {}", ctx, e),
    }
}
