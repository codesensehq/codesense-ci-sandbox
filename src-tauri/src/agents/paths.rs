// SPDX-License-Identifier: MIT
//! One startup-manifest entry: a plain content-relative path.
//!
//! An agent's `startup:` list names files (`CLAUDE.md`) and directories
//! (`.claude/skills/bmad-build/`, meaning everything under it) relative to a
//! content root. Parsing only normalizes and refuses unsafe shapes. It never
//! decides what kind of thing a path is (skill, command, hook, doc) or whether
//! it is a file or a directory, and never touches the filesystem: whether the
//! path exists and what it expands to is the manifest resolver's job.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};

/// A normalized content-relative path. Its string form is the entry's stable
/// identity: two spellings of one path (`./a//b/` and `a/b`) parse to the same
/// value, and `Display`/`FromStr` round-trip on it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StartupPath(String);

/// Why a startup entry was refused. Each variant carries the entry as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupPathError {
    Empty(String),
    Absolute(String),
    ParentSegment(String),
    Nul(String),
    Glob(String),
    /// A `\`: content paths are `/`-separated, and on Windows a backslash
    /// would be a separator this parser never sees.
    Backslash(String),
    Reserved {
        path: String,
        dir: String,
    },
}

impl fmt::Display for StartupPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty(p) => write!(f, "startup path {p:?} is empty"),
            Self::Absolute(p) => write!(
                f,
                "startup path {p:?} is absolute; use a path relative to the content root"
            ),
            Self::ParentSegment(p) => {
                write!(f, "startup path {p:?} may not contain a `..` segment")
            }
            Self::Nul(p) => write!(f, "startup path {p:?} contains a NUL byte"),
            Self::Glob(p) => write!(
                f,
                "startup path {p:?} contains a glob character; name a file or directory instead"
            ),
            Self::Backslash(p) => write!(
                f,
                "startup path {p:?} contains a backslash; separate segments with `/`"
            ),
            Self::Reserved { path, dir } => {
                write!(
                    f,
                    "startup path {path:?} is under the reserved directory {dir:?}"
                )
            }
        }
    }
}

impl std::error::Error for StartupPathError {}

const GLOB_CHARS: [char; 4] = ['*', '?', '[', ']'];

impl StartupPath {
    /// Normalize `raw`: drop `.` segments (so a leading `./`), empty segments
    /// (so a trailing `/` and repeated `/`), then refuse unsafe shapes.
    pub fn parse(raw: &str) -> Result<Self, StartupPathError> {
        let err = |make: fn(String) -> StartupPathError| make(raw.to_string());
        if raw.contains('\0') {
            return Err(err(StartupPathError::Nul));
        }
        if is_absolute(raw) {
            return Err(err(StartupPathError::Absolute));
        }
        if raw.contains(GLOB_CHARS) {
            return Err(err(StartupPathError::Glob));
        }
        if raw.contains('\\') {
            return Err(err(StartupPathError::Backslash));
        }
        let segments: Vec<&str> = raw
            .split('/')
            .filter(|s| !s.is_empty() && *s != ".")
            .collect();
        if segments.contains(&"..") {
            return Err(err(StartupPathError::ParentSegment));
        }
        let Some(top) = segments.first() else {
            return Err(err(StartupPathError::Empty));
        };
        // Case-folded: on a case-insensitive file system `.GIT` is `.git`.
        if crate::install::story_linker::is_reserved_top_level_dir(&top.to_ascii_lowercase()) {
            return Err(StartupPathError::Reserved {
                path: raw.to_string(),
                dir: (*top).to_string(),
            });
        }
        Ok(Self(segments.join("/")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `other` lies strictly under this path, segment-wise
    /// (`a/b` contains `a/b/c`, not `a/bc`).
    pub fn contains(&self, other: &StartupPath) -> bool {
        other
            .0
            .strip_prefix(&self.0)
            .is_some_and(|rest| rest.starts_with('/'))
    }
}

/// A leading `/` or `\`, or a Windows drive prefix (`C:`).
fn is_absolute(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    raw.starts_with('/')
        || raw.starts_with('\\')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

impl fmt::Display for StartupPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for StartupPath {
    type Err = StartupPathError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl Serialize for StartupPath {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for StartupPath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_paths_normalize_to_one_identity_and_round_trip() {
        for (raw, normalized) in [
            ("CLAUDE.md", "CLAUDE.md"),
            ("./a//b/", "a/b"),
            (".claude/skills/bmad-build/", ".claude/skills/bmad-build"),
            (
                "./.claude/commands/bmad-help.md",
                ".claude/commands/bmad-help.md",
            ),
            ("a/./b", "a/b"),
            (
                "finish-codesense/02-routines/README.md",
                "finish-codesense/02-routines/README.md",
            ),
        ] {
            let path = StartupPath::parse(raw).unwrap();
            assert_eq!(path.as_str(), normalized, "{raw}");
            assert_eq!(path.to_string().parse::<StartupPath>().unwrap(), path);
        }
    }

    #[test]
    fn unsafe_startup_paths_are_refused_naming_the_entry() {
        for (raw, expected) in [
            ("", StartupPathError::Empty("".into())),
            ("./", StartupPathError::Empty("./".into())),
            (
                "/etc/passwd",
                StartupPathError::Absolute("/etc/passwd".into()),
            ),
            ("C:/x", StartupPathError::Absolute("C:/x".into())),
            ("\\x", StartupPathError::Absolute("\\x".into())),
            ("a/../b", StartupPathError::ParentSegment("a/../b".into())),
            ("..", StartupPathError::ParentSegment("..".into())),
            ("a\0b", StartupPathError::Nul("a\0b".into())),
            (
                ".claude/skills/*",
                StartupPathError::Glob(".claude/skills/*".into()),
            ),
            ("a?.md", StartupPathError::Glob("a?.md".into())),
            ("[ab].md", StartupPathError::Glob("[ab].md".into())),
            (
                "docs\\..\\..\\secrets",
                StartupPathError::Backslash("docs\\..\\..\\secrets".into()),
            ),
            ("a\\b.md", StartupPathError::Backslash("a\\b.md".into())),
            (
                ".git/config",
                StartupPathError::Reserved {
                    path: ".git/config".into(),
                    dir: ".git".into(),
                },
            ),
            (
                "./node_modules/x",
                StartupPathError::Reserved {
                    path: "./node_modules/x".into(),
                    dir: "node_modules".into(),
                },
            ),
        ] {
            assert_eq!(StartupPath::parse(raw), Err(expected), "{raw:?}");
        }
        assert!(matches!(
            StartupPath::parse(".GIT/config"),
            Err(StartupPathError::Reserved { .. })
        ));
        // A reserved name below the top level is an ordinary path segment.
        assert!(StartupPath::parse("docs/target/x.md").is_ok());
        let err = StartupPath::parse("a/../b").unwrap_err().to_string();
        assert!(err.contains("a/../b"), "{err}");
    }

    #[test]
    fn containment_is_segment_wise() {
        let dir = StartupPath::parse(".claude/skills/x").unwrap();
        assert!(dir.contains(&StartupPath::parse(".claude/skills/x/SKILL.md").unwrap()));
        assert!(!dir.contains(&StartupPath::parse(".claude/skills/xy").unwrap()));
        assert!(!dir.contains(&dir));
    }
}
