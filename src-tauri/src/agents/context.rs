// SPDX-License-Identifier: MIT
//! An agent's `context`: the bulk content sets a workspace gets for it
//! (SW-XS §12), and the per-repo overrides shared with the definition.
//!
//! ```yaml
//! startup: [CLAUDE.md]          # exact, hash-pinned, errors block a launch
//! context:                      # bulk prefixes; a missing one is a warning
//!   - .claude/skills            # a bare path, or
//!   - path: _bmad-output/x      # the object form
//!     dest: plans               # optional remap of the prefix
//! ```
//!
//! A per-repo override lives beside the definition, at
//! `config/agents/<id>/repos/<host>/<owner>/<repo>.yaml`, and is part of the
//! agent's revision (snapshotted with it, restored with it):
//!
//! ```yaml
//! add:    [docs/architecture]           # or [{ path: docs/architecture }]
//! remove: [.claude/skills/legacy-*]
//! ```
//!
//! **`remove` globs** match a workspace (destination) or content (logical)
//! path, or any directory above it, so a pattern naming a directory drops
//! everything under it. They are gitignore-like: `*` and `?` stay within one
//! segment, `**` crosses segments (`**/x.lock`, `docs/**`). Case follows the
//! platform's file system: insensitive on macOS and Windows, sensitive
//! elsewhere. A `remove` never drops a `startup` path: those are exact and
//! hash-pinned, and removing one is an edit to `startup`.
//!
//! When `context` is absent it resolves, at launch and per runtime, to
//! [`default_context`]; that default is never written back. Parsing here only
//! normalizes and refuses unsafe shapes; what a prefix expands to is the
//! resolver's job (`manifest_resolve::context`).

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::content_engine::{is_projection_excluded, TeamExcludes};
use crate::domain::SourceRepoSlug;

use super::paths::{StartupPath, StartupPathError};
use codesense_pty_core::agent_cli::AgentCli;

/// The most entries a `context` list (or one override's `add`) may hold.
pub const MAX_CONTEXT_ENTRIES: usize = 32;
/// The most entries `context` plus one override's `add` may hold together.
pub const MAX_COMBINED_ENTRIES: usize = 64;
/// The most `remove` patterns one override may hold.
pub const MAX_REMOVE_PATTERNS: usize = 64;

/// Version-control metadata directories: never a workspace destination, at
/// any depth, in any case.
const VCS_DIRS: [&str; 3] = [".git", ".hg", ".svn"];

/// Workspace files CodeSense or the runtime CLI owns: a `dest` may not be one
/// of them or lie inside one.
pub const MANAGED_DESTINATIONS: &[&str] = &[
    ".claude/settings.json",
    ".claude/settings.local.json",
    ".mcp.json",
    ".codex/config.toml",
];

/// SW-XS-13: the exact local-state and tool-lock files known to be junk.
/// Context resolution never carries them; the projection excludes used
/// elsewhere are unchanged. Deliberately narrow: ordinary lockfiles
/// (`Cargo.lock`, `pnpm-lock.yaml`) are content.
///
/// SW-XS-12: matched in any case, like every exclude set in
/// `codesense_fs_core::filters` (on macOS `.Claude/Settings.Local.json` is
/// the same file).
pub const JUNK_GLOBS: &[&str] = &[
    ".claude/settings.local.json",
    ".claude/scheduled_tasks.lock",
    "**/wvbuild-lock",
    "**/wv-ci-gate",
];

/// Whether `logical` is one of [`JUNK_GLOBS`].
pub fn is_junk(logical: &str) -> bool {
    use std::sync::OnceLock;
    static SET: OnceLock<globset::GlobSet> = OnceLock::new();
    SET.get_or_init(|| {
        let mut builder = globset::GlobSetBuilder::new();
        for pattern in JUNK_GLOBS {
            builder.add(
                globset::GlobBuilder::new(pattern)
                    .literal_separator(true)
                    .case_insensitive(true)
                    .build()
                    .expect("junk globs are valid"),
            );
        }
        builder.build().expect("junk globs build")
    })
    .is_match(logical)
}

/// Why a workspace path may never receive context, if it may not.
pub fn unsafe_destination(destination: &str) -> Option<String> {
    if let Some(vcs) = destination
        .split('/')
        .find(|s| VCS_DIRS.contains(&s.to_ascii_lowercase().as_str()))
    {
        return Some(format!("{vcs} is version-control metadata"));
    }
    if super::manifest_resolve::not_carried(destination) {
        return Some("it is OS, editor, build or VCS data a workspace never carries".into());
    }
    let folded = destination.to_ascii_lowercase();
    if let Some(managed) = MANAGED_DESTINATIONS.iter().find(|m| {
        folded == **m
            || folded
                .strip_prefix(**m)
                .is_some_and(|rest| rest.starts_with('/'))
    }) {
        return Some(format!("{managed} is generated or managed by CodeSense"));
    }
    if is_junk(destination) || is_projection_excluded(destination, default_excludes()) {
        return Some("it is never copied into a workspace by default".into());
    }
    None
}

fn default_excludes() -> &'static TeamExcludes {
    use std::sync::OnceLock;
    static DEFAULT: OnceLock<TeamExcludes> = OnceLock::new();
    DEFAULT.get_or_init(TeamExcludes::default_only)
}

/// One context set: a content-relative prefix (a file or a directory), and
/// where it lands in the workspace when not at its own path.
///
/// Read as either a bare path (`docs/architecture`) or the object form
/// (`{path: docs/architecture, dest: plans}`); always written as the object.
/// Keys a newer build added are kept and written back as they were.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct ContextEntry {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest: Option<String>,
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_yaml::Value>,
}

/// Reads a [`ContextEntry`] from a bare string or a map. With `strict`, a key
/// other than `path`/`dest` is refused by name (tool input); without, it is
/// kept (a stored definition).
pub(crate) struct EntryVisitor {
    pub strict: bool,
}

impl<'de> Visitor<'de> for EntryVisitor {
    type Value = ContextEntry;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a content path, or {path, dest?}")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<ContextEntry, E> {
        Ok(ContextEntry::new(v))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<ContextEntry, A::Error> {
        let mut path = None;
        let mut dest = None;
        let mut extra = BTreeMap::new();
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "path" => path = Some(map.next_value::<String>()?),
                "dest" => dest = map.next_value::<Option<String>>()?,
                other if self.strict => {
                    return Err(de::Error::custom(format!(
                        "unknown key `{other}` in a context entry; an entry is a path or {{path, dest}}"
                    )))
                }
                _ => {
                    let value = map.next_value::<serde_yaml::Value>()?;
                    extra.insert(key, value);
                }
            }
        }
        let path = path.ok_or_else(|| de::Error::missing_field("path"))?;
        Ok(ContextEntry { path, dest, extra })
    }
}

impl<'de> Deserialize<'de> for ContextEntry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(EntryVisitor { strict: false })
    }
}

impl ContextEntry {
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            dest: None,
            extra: BTreeMap::new(),
        }
    }

    pub fn with_dest(path: impl Into<String>, dest: impl Into<String>) -> Self {
        Self {
            dest: Some(dest.into()),
            ..Self::new(path)
        }
    }

    /// The parsed prefix. Only valid on a normalized entry.
    pub(crate) fn prefix(&self) -> Option<StartupPath> {
        StartupPath::parse(&self.path).ok()
    }

    /// Where the entry's prefix lands in the workspace.
    pub fn target(&self) -> &str {
        self.dest.as_deref().unwrap_or(&self.path)
    }

    /// Whether this entry delivers `other`'s prefix, and lands it where `other` would
    /// (SW-XS-16): `other` is then redundant beside it. A nested entry with its own
    /// `dest` is not covered by a folder above it that lands it elsewhere.
    pub fn places_like(&self, other: &ContextEntry) -> bool {
        let inside = other.path == self.path
            || other
                .path
                .strip_prefix(self.path.as_str())
                .is_some_and(|rest| rest.starts_with('/'));
        inside && self.destination_of(&other.path) == other.target()
    }

    /// Where the file at `logical` (under this entry's prefix) lands.
    pub(crate) fn destination_of(&self, logical: &str) -> String {
        match &self.dest {
            None => logical.to_string(),
            Some(dest) => match logical.strip_prefix(self.path.as_str()) {
                Some("") => dest.clone(),
                Some(rest) => format!("{dest}{rest}"),
                None => logical.to_string(),
            },
        }
    }
}

/// A per-repo override of one agent's context, for one repo.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoOverride {
    /// Extra sets this repo's workspace gets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub add: Vec<ContextEntry>,
    /// Globs this repo's workspace never gets (see the module docs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<String>,
    /// Keys a newer build added, kept and written back as they were.
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_yaml::Value>,
}

impl RepoOverride {
    pub fn is_empty(&self) -> bool {
        self.add.is_empty() && self.remove.is_empty() && self.extra.is_empty()
    }

    /// The override with every entry normalized, or why it cannot be saved.
    /// Checks the override alone; [`Self::normalized_with`] also checks it
    /// against the agent's context.
    pub fn normalized(&self) -> Result<Self, ContextError> {
        if self.remove.len() > MAX_REMOVE_PATTERNS {
            return Err(ContextError::TooManyRemove(self.remove.len()));
        }
        let add = normalize_entries(&self.add)?;
        let mut remove: Vec<String> = Vec::with_capacity(self.remove.len());
        for raw in &self.remove {
            let pattern = normalize_remove(raw)?;
            if !remove.contains(&pattern) {
                remove.push(pattern);
            }
        }
        Ok(Self {
            add,
            remove,
            extra: self.extra.clone(),
        })
    }

    /// [`Self::normalized`], plus: `context` and `add` together stay within
    /// [`MAX_COMBINED_ENTRIES`] and their destinations do not collide.
    pub fn normalized_with(&self, context: &[ContextEntry]) -> Result<Self, ContextError> {
        let normalized = self.normalized()?;
        let combined = context.len() + normalized.add.len();
        if combined > MAX_COMBINED_ENTRIES {
            return Err(ContextError::TooManyCombined(combined));
        }
        let mut all = context.to_vec();
        all.extend(normalized.add.iter().cloned());
        normalize_unbounded(&all)?;
        Ok(normalized)
    }

    /// The compiled `remove` globs. Only valid on a normalized override.
    pub(crate) fn remove_set(&self) -> globset::GlobSet {
        let mut builder = globset::GlobSetBuilder::new();
        for pattern in &self.remove {
            if let Ok(glob) = remove_glob(pattern) {
                builder.add(glob);
            }
        }
        builder
            .build()
            .unwrap_or_else(|_| globset::GlobSet::empty())
    }
}

/// A `remove` glob. SW-XS-12: in any case on every platform, the policy every
/// exclude set follows (`codesense_fs_core::filters::case_insensitive_glob`):
/// removing a case variant too fails closed, and the agent editor's preview
/// (`globMatcher`) already ignores case, so the two now agree on Linux too.
fn remove_glob(pattern: &str) -> Result<globset::Glob, globset::Error> {
    globset::GlobBuilder::new(pattern)
        .literal_separator(true)
        .case_insensitive(true)
        .build()
}

/// Why a `context` list or a repo override cannot be saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextError {
    InvalidPath(StartupPathError),
    /// A content path with a VCS segment below the top level.
    UnsafePath {
        path: String,
        why: String,
    },
    InvalidDest {
        path: String,
        error: StartupPathError,
    },
    /// A `dest` the workspace may never receive context at.
    UnsafeDest {
        path: String,
        dest: String,
        why: String,
    },
    /// Two entries land at the same or nested workspace paths.
    DestCollision {
        first: String,
        second: String,
    },
    TooMany(usize),
    TooManyCombined(usize),
    TooManyRemove(usize),
    InvalidRemove {
        pattern: String,
        why: String,
    },
    InvalidRepo {
        repo: String,
        why: String,
    },
}

impl fmt::Display for ContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath(e) => write!(f, "context {}", strip_startup(e)),
            Self::UnsafePath { path, why } => write!(f, "context path {path:?}: {why}"),
            Self::InvalidDest { path, error } => {
                write!(f, "context {path:?}: dest {}", strip_startup(error))
            }
            Self::UnsafeDest { path, dest, why } => write!(
                f,
                "context {path:?}: dest {dest:?} cannot receive context: {why}"
            ),
            Self::DestCollision { first, second } => write!(
                f,
                "context {second:?} lands at or inside where {first:?} lands (or the other way \
                 round); give one of them a different dest"
            ),
            Self::TooMany(n) => write!(
                f,
                "context has {n} entries; an agent has at most {MAX_CONTEXT_ENTRIES}"
            ),
            Self::TooManyCombined(n) => write!(
                f,
                "context plus a repo override's add has {n} entries; at most \
                 {MAX_COMBINED_ENTRIES} together"
            ),
            Self::TooManyRemove(n) => write!(
                f,
                "a repo override has {n} remove patterns; at most {MAX_REMOVE_PATTERNS}"
            ),
            Self::InvalidRemove { pattern, why } => {
                write!(f, "repo override remove {pattern:?}: {why}")
            }
            Self::InvalidRepo { repo, why } => write!(f, "repo {repo:?}: {why}"),
        }
    }
}

/// A startup-path error worded for a context path.
fn strip_startup(e: &StartupPathError) -> String {
    let text = e.to_string();
    text.strip_prefix("startup ").unwrap_or(&text).to_string()
}

impl std::error::Error for ContextError {}

/// NFC, then the startup-path rules: relative, no `..`, `.` or empty, no
/// globs, no backslash, no reserved top-level directory.
fn normalize_path(raw: &str) -> Result<StartupPath, StartupPathError> {
    let nfc: String = raw.nfc().collect();
    StartupPath::parse(&nfc)
}

/// The key two workspace paths share when a case-insensitive file system
/// would treat them as one (the entries are already NFC).
fn fold(path: &str) -> String {
    path.to_lowercase()
}

/// Whether `a` and `b` are the same path or one lies inside the other,
/// segment-wise.
fn overlaps(a: &str, b: &str) -> bool {
    let inside = |outer: &str, inner: &str| {
        inner
            .strip_prefix(outer)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    };
    inside(a, b) || inside(b, a)
}

/// Every entry normalized, in order, at most [`MAX_CONTEXT_ENTRIES`]:
/// - NFC, relative, no `..`/`.`/empty, no VCS segment, `dest` a safe place;
/// - an entry another one already covers with the same placement is dropped
///   (whichever order they come in);
/// - two entries whose workspace targets are the same or nested (segment-
///   wise, ignoring case) are refused.
pub fn normalize_entries(entries: &[ContextEntry]) -> Result<Vec<ContextEntry>, ContextError> {
    if entries.len() > MAX_CONTEXT_ENTRIES {
        return Err(ContextError::TooMany(entries.len()));
    }
    normalize_unbounded(entries)
}

fn normalize_unbounded(entries: &[ContextEntry]) -> Result<Vec<ContextEntry>, ContextError> {
    let mut parsed: Vec<ContextEntry> = Vec::with_capacity(entries.len());
    for entry in entries {
        let path = normalize_path(&entry.path).map_err(ContextError::InvalidPath)?;
        if let Some(vcs) = path
            .as_str()
            .split('/')
            .find(|s| VCS_DIRS.contains(&s.to_ascii_lowercase().as_str()))
        {
            return Err(ContextError::UnsafePath {
                path: entry.path.clone(),
                why: format!("{vcs} is version-control metadata"),
            });
        }
        let dest = match &entry.dest {
            None => None,
            Some(raw) => {
                let dest = normalize_path(raw).map_err(|error| ContextError::InvalidDest {
                    path: entry.path.clone(),
                    error,
                })?;
                if let Some(why) = unsafe_destination(dest.as_str()) {
                    return Err(ContextError::UnsafeDest {
                        path: entry.path.clone(),
                        dest: dest.to_string(),
                        why,
                    });
                }
                // A dest equal to the path is no remap.
                (dest != path).then(|| dest.to_string())
            }
        };
        parsed.push(ContextEntry {
            path: path.to_string(),
            dest,
            extra: entry.extra.clone(),
        });
    }

    // Order-independent: an entry is redundant when another covers its prefix
    // and places it identically (an exact duplicate keeps its first copy).
    let redundant = |i: usize| {
        let me = &parsed[i];
        let Some(my_path) = me.prefix() else {
            return false;
        };
        parsed.iter().enumerate().any(|(j, other)| {
            if i == j {
                return false;
            }
            let Some(other_path) = other.prefix() else {
                return false;
            };
            let same_place = other.destination_of(&me.path) == me.destination_of(&me.path);
            same_place && (other_path.contains(&my_path) || (other_path == my_path && j < i))
        })
    };
    let kept: Vec<ContextEntry> = (0..parsed.len())
        .filter(|&i| !redundant(i))
        .map(|i| parsed[i].clone())
        .collect();

    for (i, a) in kept.iter().enumerate() {
        for b in &kept[i + 1..] {
            if overlaps(&fold(a.target()), &fold(b.target())) {
                return Err(ContextError::DestCollision {
                    first: a.path.clone(),
                    second: b.path.clone(),
                });
            }
        }
    }
    Ok(kept)
}

/// A `remove` pattern: NFC, relative, no `..`, a valid glob.
fn normalize_remove(raw: &str) -> Result<String, ContextError> {
    let bad = |why: &str| ContextError::InvalidRemove {
        pattern: raw.to_string(),
        why: why.to_string(),
    };
    let nfc: String = raw.nfc().collect();
    let trimmed = nfc.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_prefix("./").unwrap_or(trimmed);
    if trimmed.is_empty() || trimmed == "." {
        return Err(bad("is empty"));
    }
    if trimmed.contains('\0') {
        return Err(bad("contains a NUL byte"));
    }
    if trimmed.starts_with('/')
        || trimmed.contains('\\')
        || trimmed.as_bytes().get(1) == Some(&b':')
    {
        return Err(bad("must be a relative content path with `/` separators"));
    }
    if trimmed.split('/').any(|s| s == "..") {
        return Err(bad("may not contain a `..` segment"));
    }
    remove_glob(trimmed).map_err(|e| bad(&e.to_string()))?;
    Ok(trimmed.to_string())
}

/// The context a launch uses when the definition has none (SPEC §12.2):
/// per runtime, and the team-workspace launch's own. Resolved at launch,
/// and never written back.
pub fn default_context(runtime: AgentCli, _team_workspace: bool) -> Vec<ContextEntry> {
    codesense_pty_core::agent_cli::descriptor(runtime)
        .default_context
        .iter()
        .map(|p| ContextEntry::new(p.as_str()))
        .collect()
}

/// A repo as the agent tools name it: `host/owner/repo`, or `owner/repo`,
/// which always means github.com. A clone URL works too
/// (`https://host/owner/repo.git`, `git@host:owner/repo.git`): the scheme,
/// the `git@` form and a trailing `.git` are stripped.
pub fn parse_repo(raw: &str) -> Result<SourceRepoSlug, ContextError> {
    let bad = |why: String| ContextError::InvalidRepo {
        repo: raw.to_string(),
        why,
    };
    let mut text = raw.trim();
    for scheme in ["https://", "http://", "ssh://"] {
        if let Some(rest) = text.strip_prefix(scheme) {
            text = rest;
        }
    }
    let ssh;
    if let Some(rest) = text.strip_prefix("git@") {
        ssh = rest.replacen(':', "/", 1);
        text = &ssh;
    }
    let text = text.trim_matches('/');
    let text = text.strip_suffix(".git").unwrap_or(text);
    let parts: Vec<&str> = text.split('/').collect();
    let slug = match parts.as_slice() {
        [owner, repo] => SourceRepoSlug::new(*owner, *repo),
        [host, owner, repo] => {
            let provider =
                crate::commands::forge::provider_for_host(host).unwrap_or("self-managed");
            SourceRepoSlug::with_provider_and_host(*owner, *repo, provider, Some(*host))
        }
        _ => return Err(bad("name it as owner/repo or host/owner/repo".into())),
    };
    slug.map_err(|e| bad(e.to_string()))
}

/// The `host/owner/repo` a repo's override file is filed under, as stored.
pub fn repo_label(slug: &SourceRepoSlug) -> Result<String, ContextError> {
    let key = slug
        .physical_path_key()
        .map_err(|e| ContextError::InvalidRepo {
            repo: format!("{}/{}", slug.owner, slug.repo),
            why: e.to_string(),
        })?;
    let path = key.append_to(std::path::Path::new(""));
    Ok(path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(raw: &[(&str, Option<&str>)]) -> Vec<ContextEntry> {
        raw.iter()
            .map(|(p, d)| ContextEntry {
                path: (*p).into(),
                dest: d.map(str::to_string),
                extra: BTreeMap::new(),
            })
            .collect()
    }

    #[test]
    fn entries_normalize_nfc_and_drop_nested_duplicates() {
        let decomposed = "docs/cafe\u{301}";
        let got = normalize_entries(&entries(&[
            ("./docs//", None),
            ("docs/a", None),
            (decomposed, None),
            (".claude/skills", None),
            (".claude/skills", None),
            ("_bmad-output/x", Some("plans")),
            ("_bmad-output/x/y", Some("plans/y")),
            ("notes", Some("notes/")),
        ]))
        .unwrap();
        assert_eq!(
            got,
            entries(&[
                ("docs", None),
                (".claude/skills", None),
                ("_bmad-output/x", Some("plans")),
                ("notes", None),
            ])
        );
        let nfc = normalize_entries(&entries(&[("a\u{301}", None)])).unwrap();
        assert_eq!(nfc[0].path, "\u{e1}");
    }

    /// L8: whether the nested entry comes first or last, it is dropped.
    #[test]
    fn nested_dedup_is_order_independent() {
        let a = normalize_entries(&entries(&[("docs/a", None), ("docs", None)])).unwrap();
        let b = normalize_entries(&entries(&[("docs", None), ("docs/a", None)])).unwrap();
        assert_eq!(a, entries(&[("docs", None)]));
        assert_eq!(b, entries(&[("docs", None)]));
        let remapped =
            normalize_entries(&entries(&[("x/y", Some("plans/y")), ("x", Some("plans"))])).unwrap();
        assert_eq!(remapped, entries(&[("x", Some("plans"))]));
    }

    /// SW-XS-16 r2: the glob lists the agent editor checks a `dest` against are emitted
    /// from these constants into `generated/context_filter_globs.json`, which the client
    /// imports (`agent-form.ts`), so the two cannot drift. Regenerate with
    /// `UPDATE_CONTEXT_FIXTURES=1 cargo test -p codesense --lib filter_globs_fixture`.
    #[test]
    fn filter_globs_fixture_is_current() {
        use codesense_fs_core::filters::{IGNORE_GLOBS, RUNTIME_STATE_GLOBS, SECRET_GLOBS};
        let want = serde_json::json!({
            "vcs_dirs": VCS_DIRS,
            "managed_destinations": MANAGED_DESTINATIONS,
            "junk": JUNK_GLOBS,
            "ignore": IGNORE_GLOBS,
            "secret": SECRET_GLOBS,
            "runtime_state": RUNTIME_STATE_GLOBS,
            // SW-XS-12: the sets codesense-fs-core matches in any case (its
            // `case_insensitive_glob`, `is_vcs_metadata_dir`); the editor
            // reads which, so the two match `config/.ENV` alike.
            "case_insensitive": ["vcs_dirs", "junk", "ignore", "secret", "runtime_state"],
        });
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/agents/generated/context_filter_globs.json"
        );
        if std::env::var_os("UPDATE_CONTEXT_FIXTURES").is_some() {
            let text = serde_json::to_string_pretty(&want).unwrap();
            std::fs::write(path, format!("{text}\n")).unwrap();
        }
        let have: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(
            have, want,
            "stale fixture: rerun with UPDATE_CONTEXT_FIXTURES=1"
        );
    }

    /// SW-XS-16: the rules the agent editor checks a typed entry by, before the round
    /// trip (`agent-form.ts` `checkContextPath` / `checkContextDest`), are pinned to the
    /// same cases here and there (`agent-form.test.ts`), so the two never drift.
    #[test]
    fn entry_rules_match_the_shared_fixture() {
        #[derive(serde::Deserialize)]
        struct Case {
            path: String,
            dest: Option<String>,
            ok: bool,
        }
        let cases: Vec<Case> =
            serde_json::from_str(include_str!("testdata/context_entry_cases.json")).unwrap();
        assert!(cases.len() >= 30);
        for case in cases {
            let entry = ContextEntry {
                dest: case.dest.clone(),
                ..ContextEntry::new(case.path.clone())
            };
            assert_eq!(
                normalize_entries(&[entry]).is_ok(),
                case.ok,
                "{:?} -> {:?}",
                case.path,
                case.dest
            );
        }
    }

    #[test]
    fn invalid_entries_are_refused() {
        for bad in [
            entries(&[("..", None)]),
            entries(&[("a/../b", None)]),
            entries(&[("/etc", None)]),
            entries(&[("", None)]),
            entries(&[(".", None)]),
            entries(&[("docs", Some("../x"))]),
            entries(&[("docs", Some("/abs"))]),
            entries(&[(".git/x", None)]),
            entries(&[("vendor/.git/hooks", None)]),
        ] {
            assert!(normalize_entries(&bad).is_err(), "{bad:?}");
        }
        let many: Vec<ContextEntry> = (0..=MAX_CONTEXT_ENTRIES)
            .map(|i| ContextEntry::new(format!("d{i}")))
            .collect();
        assert_eq!(
            normalize_entries(&many),
            Err(ContextError::TooMany(MAX_CONTEXT_ENTRIES + 1))
        );
        assert!(normalize_entries(&many[..MAX_CONTEXT_ENTRIES]).is_ok());
    }

    /// H1: a dest may never be VCS data, cruft, excluded, junk or managed.
    #[test]
    fn unsafe_dests_are_refused() {
        for dest in [
            "vendor/.git/hooks",
            "pkg/.git",
            "pkg/.GIT",
            "a/.hg",
            "a/.svn/x",
            ".claude/settings.local.json",
            ".claude/settings.json",
            ".Claude/Settings.json",
            ".mcp.json",
            ".codex/config.toml",
            ".env",
            "x/.DS_Store",
            "x/wvbuild-lock",
        ] {
            let got = normalize_entries(&entries(&[("docs", Some(dest))]));
            assert!(
                matches!(
                    got,
                    Err(ContextError::UnsafeDest { .. }) | Err(ContextError::InvalidDest { .. })
                ),
                "{dest}: {got:?}"
            );
        }
        assert!(normalize_entries(&entries(&[("docs", Some(".claude/docs"))])).is_ok());
    }

    /// H2: two entries landing at the same or nested places are refused,
    /// whatever the case; the same source nested consistently is not.
    #[test]
    fn colliding_destinations_are_refused() {
        for bad in [
            entries(&[("a", Some("x")), ("b", Some("x"))]),
            entries(&[("a", Some("x")), ("b", Some("x/y"))]),
            entries(&[("a", None), ("b", Some("a/b"))]),
            entries(&[("a", Some("Plans")), ("b", Some("plans"))]),
            entries(&[("docs", None), ("docs/x", Some("docs/y"))]),
            entries(&[("docs", Some("d")), ("docs", Some("e/f")), ("e", None)]),
        ] {
            assert!(
                matches!(
                    normalize_entries(&bad),
                    Err(ContextError::DestCollision { .. })
                ),
                "{bad:?}"
            );
        }
        for ok in [
            entries(&[("docs", None), ("docs/x", Some("plans"))]),
            entries(&[("docs", Some("d")), ("docs/x", None)]),
            entries(&[("a", None), ("ab", None)]),
        ] {
            assert!(normalize_entries(&ok).is_ok(), "{ok:?}");
        }
    }

    #[test]
    fn remove_patterns_are_relative_globs_and_capped() {
        let o = RepoOverride {
            remove: vec!["./.claude/skills/legacy-*/".into(), "x/**".into()],
            ..RepoOverride::default()
        }
        .normalized()
        .unwrap();
        assert_eq!(o.remove, vec![".claude/skills/legacy-*", "x/**"]);
        for bad in ["", "/abs", "a/../b", "a\\b", "a[", "C:/x"] {
            let o = RepoOverride {
                remove: vec![bad.into()],
                ..RepoOverride::default()
            };
            assert!(o.normalized().is_err(), "{bad:?}");
        }
        let many = RepoOverride {
            remove: (0..=MAX_REMOVE_PATTERNS).map(|i| format!("p{i}")).collect(),
            ..RepoOverride::default()
        };
        assert_eq!(
            many.normalized(),
            Err(ContextError::TooManyRemove(MAX_REMOVE_PATTERNS + 1))
        );
    }

    /// SW-XS-12: a `remove` glob matches every case on every platform (the
    /// exclude-set policy), so the preview and the server agree on Linux too.
    #[test]
    fn remove_globs_match_every_case() {
        let o = RepoOverride {
            remove: vec![".claude/skills/legacy-*".into(), "Docs/Old.md".into()],
            ..RepoOverride::default()
        }
        .normalized()
        .unwrap();
        let set = o.remove_set();
        for removed in [
            ".claude/skills/legacy-a",
            ".CLAUDE/Skills/LEGACY-b",
            "docs/old.md",
            "DOCS/OLD.MD",
        ] {
            assert!(set.is_match(removed), "{removed}");
        }
        assert!(!set.is_match(".claude/skills/current"));
        assert!(
            !set.is_match("docs/old.md/x"),
            "a glob without ** stays one path"
        );
    }

    /// L9 + H2 across context and an override's add.
    #[test]
    fn an_override_is_checked_against_the_context() {
        let context: Vec<ContextEntry> = (0..32)
            .map(|i| ContextEntry::new(format!("c{i}")))
            .collect();
        let add: Vec<ContextEntry> = (0..32)
            .map(|i| ContextEntry::new(format!("a{i}")))
            .collect();
        let o = RepoOverride {
            add: add.clone(),
            ..RepoOverride::default()
        };
        assert!(o.normalized_with(&context).is_ok());
        let mut more = context.clone();
        more.push(ContextEntry::new("extra"));
        assert_eq!(
            o.normalized_with(&more),
            Err(ContextError::TooManyCombined(65))
        );
        let clash = RepoOverride {
            add: vec![ContextEntry::with_dest("other", "docs/x")],
            ..RepoOverride::default()
        };
        assert!(matches!(
            clash.normalized_with(&[ContextEntry::new("docs")]),
            Err(ContextError::DestCollision { .. })
        ));
        let nested = RepoOverride {
            add: vec![ContextEntry::new("docs/architecture")],
            ..RepoOverride::default()
        };
        assert!(nested.normalized_with(&[ContextEntry::new("docs")]).is_ok());
    }

    #[test]
    fn a_bare_path_reads_as_an_entry_and_writes_as_the_object_form() {
        let yaml = "add:\n  - docs/architecture\n  - { path: _bmad-output/x, dest: plans }\nremove: [.claude/skills/legacy-*]\n";
        let o: RepoOverride = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            o.add,
            vec![
                ContextEntry::new("docs/architecture"),
                ContextEntry::with_dest("_bmad-output/x", "plans"),
            ]
        );
        let written = serde_yaml::to_string(&o).unwrap();
        assert!(written.contains("- path: docs/architecture"), "{written}");
        let back: RepoOverride = serde_yaml::from_str(&written).unwrap();
        assert_eq!(back, o);
        let context: Vec<ContextEntry> = serde_yaml::from_str("[docs, {path: a}]").unwrap();
        assert_eq!(
            context,
            vec![ContextEntry::new("docs"), ContextEntry::new("a")]
        );
        assert!(serde_yaml::from_str::<ContextEntry>("{dest: x}").is_err());
    }

    /// L5: keys a newer build wrote survive a read and a write.
    #[test]
    fn unknown_stored_keys_round_trip() {
        let yaml = "add:\n  - { path: docs, weight: 3 }\nremove: [x]\npriority: high\n";
        let o: RepoOverride = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(o.add[0].extra["weight"], serde_yaml::Value::from(3));
        assert_eq!(o.extra["priority"], serde_yaml::Value::from("high"));
        let normalized = o.normalized().unwrap();
        let back: RepoOverride =
            serde_yaml::from_str(&serde_yaml::to_string(&normalized).unwrap()).unwrap();
        assert_eq!(back, o);
    }

    #[test]
    fn dest_remaps_the_prefix_only() {
        let e = ContextEntry::with_dest("_bmad-output/x", "plans");
        assert_eq!(e.destination_of("_bmad-output/x/a/b.md"), "plans/a/b.md");
        assert_eq!(e.destination_of("_bmad-output/x"), "plans");
        assert_eq!(
            ContextEntry::new("docs").destination_of("docs/a.md"),
            "docs/a.md"
        );
    }

    #[test]
    fn junk_is_exact_and_ordinary_lockfiles_are_content() {
        for junk in [
            ".claude/settings.local.json",
            ".claude/scheduled_tasks.lock",
            ".claude/wvbuild-lock",
            ".claude/wv-ci-gate",
            "wv-ci-gate",
            ".claude/skills/x/wvbuild-lock",
        ] {
            assert!(is_junk(junk), "{junk}");
        }
        for kept in [
            "Cargo.lock",
            "pnpm-lock.yaml",
            "app/Cargo.lock",
            "yarn.lock",
            ".claude/settings.json",
            "docs/locking.md",
        ] {
            assert!(!is_junk(kept), "{kept}");
        }
        // SW-XS-12: any case, as fs-core's exclude sets.
        for junk in [
            ".Claude/Settings.Local.JSON",
            "x/WV-CI-GATE",
            "a/WvBuild-Lock",
        ] {
            assert!(is_junk(junk), "{junk}");
        }
    }

    #[test]
    fn defaults_follow_the_runtime_and_never_carry_plans_or_junk() {
        let claude: Vec<String> = default_context(AgentCli::Claude, false)
            .into_iter()
            .map(|e| e.path)
            .collect();
        assert_eq!(
            claude,
            [
                "CLAUDE.md",
                ".claude/commands",
                ".claude/hooks",
                ".claude/agents",
                ".claude/skills"
            ]
        );
        let codex: Vec<String> = default_context(AgentCli::Codex, false)
            .into_iter()
            .map(|e| e.path)
            .collect();
        assert_eq!(codex, ["AGENTS.md", ".agents/skills"]);
        for runtime in [AgentCli::Claude, AgentCli::Codex] {
            let paths: Vec<String> = default_context(runtime, true)
                .into_iter()
                .map(|e| e.path)
                .collect();
            assert_eq!(
                paths,
                default_context(runtime, false)
                    .into_iter()
                    .map(|e| e.path)
                    .collect::<Vec<_>>()
            );
        }
        for runtime in [AgentCli::Claude, AgentCli::Codex] {
            for tw in [false, true] {
                for e in default_context(runtime, tw) {
                    for never in ["_bmad-output", ".playwright-mcp", ".opencode", "_lab"] {
                        assert!(!e.path.starts_with(never), "{e:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn repos_parse_with_or_without_a_host_and_label_host_owner_repo() {
        let gh = parse_repo("alderco/codesense").unwrap();
        assert_eq!(repo_label(&gh).unwrap(), "github.com/alderco/codesense");
        let gl = parse_repo("gitlab.com/Team/App").unwrap();
        assert_eq!(repo_label(&gl).unwrap(), "gitlab.com/team/app");
        for url in [
            "https://github.com/alderco/codesense",
            "https://github.com/alderco/codesense.git",
            "github.com/alderco/codesense/",
            "git@github.com:alderco/codesense.git",
            "alderco/codesense.git",
        ] {
            assert_eq!(
                repo_label(&parse_repo(url).unwrap()).unwrap(),
                "github.com/alderco/codesense",
                "{url}"
            );
        }
        assert!(parse_repo("just-one").is_err());
        assert!(parse_repo("a/b/c/d").is_err());
    }
}
