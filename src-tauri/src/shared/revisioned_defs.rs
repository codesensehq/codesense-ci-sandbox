// SPDX-License-Identifier: MIT
//! The mechanics shared by every revisioned, git-synced definition directory
//! (routines, agents):
//!
//! ```text
//! <defs dir>/<id>/<file>                  current definition
//! <defs dir>/<id>/revisions/<n>.yml       immutable snapshot of revision n
//! ```
//!
//! Each kind keeps its own definition type, file name and revision record;
//! this module owns only what is identical across them: the error type,
//! revision file naming and listing, the directory scan and the per-definition
//! write lock.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use serde::de::DeserializeOwned;

use crate::shared::yaml_io::{read_yaml_optional, YamlIoError};

pub const REVISIONS_DIR: &str = "revisions";

#[derive(Debug)]
pub enum DefsError {
    Yaml {
        path: PathBuf,
        source: YamlIoError,
    },
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl std::fmt::Display for DefsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Yaml { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for DefsError {}

/// Tag a YAML failure with the file it happened in.
pub fn yaml_err(path: &Path) -> impl FnOnce(YamlIoError) -> DefsError + '_ {
    move |source| DefsError::Yaml {
        path: path.to_path_buf(),
        source,
    }
}

/// `<dir>/revisions/<n>.yml`.
pub fn revision_path(dir: &Path, n: u32) -> PathBuf {
    dir.join(REVISIONS_DIR).join(format!("{n}.yml"))
}

/// The YAML file at `path`, or `None` when it does not exist.
pub fn read_optional<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, DefsError> {
    read_yaml_optional(path).map_err(yaml_err(path))
}

/// The revision numbers that have a snapshot in `dir`, ascending. Files that
/// are not `<n>.yml` are ignored; no revisions dir means none.
pub fn revision_numbers(dir: &Path) -> Result<Vec<u32>, DefsError> {
    let rev_dir = dir.join(REVISIONS_DIR);
    let entries = match std::fs::read_dir(&rev_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(DefsError::Io {
                path: rev_dir,
                source,
            })
        }
    };
    let mut numbers: Vec<u32> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_suffix(".yml")?
                .parse::<u32>()
                .ok()
        })
        .collect();
    numbers.sort_unstable();
    Ok(numbers)
}

/// Every definition under `defs_dir`, as `(dir, result)`, sorted by dir. A dir
/// with no `file` is skipped; a malformed one is returned as an error so the
/// caller decides whether to skip it.
pub fn scan<T: DeserializeOwned>(
    defs_dir: &Path,
    file: &str,
) -> Vec<(PathBuf, Result<T, DefsError>)> {
    let Ok(entries) = std::fs::read_dir(defs_dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    dirs.sort();
    dirs.into_iter()
        .filter_map(|dir| match read_optional(&dir.join(file)) {
            Ok(None) => None,
            Ok(Some(def)) => Some((dir, Ok(def))),
            Err(e) => Some((dir, Err(e))),
        })
        .collect()
}

static DEF_LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Run `f` while holding the in-process lock for the definition at `dir`, so
/// two read-check-write saves of one definition cannot interleave.
pub fn with_def_lock<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
    let lock = {
        let mut locks = DEF_LOCKS.lock().unwrap_or_else(|e| e.into_inner());
        locks.entry(dir.to_path_buf()).or_default().clone()
    };
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    f()
}
