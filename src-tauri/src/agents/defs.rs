// SPDX-License-Identifier: MIT
//! The on-disk layout of agent definitions and their revisions.
//!
//! ```text
//! {team}/config/agents/<id>/agent.yaml           current definition
//! {team}/config/agents/<id>/revisions/<n>.yml    immutable snapshot of revision n
//! {team}/config/agents/<id>/repos/<host>/<owner>/<repo>.yaml
//!                                                per-repo context override (SW-XS-13)
//! ```
//!
//! `config/` is the team's git-tracked, never-projected sibling of `content/`,
//! so a definition travels with the context repo but is never materialized into
//! a source repo or a consumer team.
//!
//! Which directory a team's `config/` is lives in [`super::roots`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::routines::model::Author;
pub use crate::shared::revisioned_defs::DefsError;
use crate::shared::revisioned_defs::{self, read_optional, revision_path, yaml_err};
use crate::shared::yaml_io::{write_yaml_bytes_atomic, write_yaml_value_atomic, YamlIoError};

use crate::domain::SourceRepoSlug;

use super::context::{ContextError, RepoOverride};
use super::manifest::content_hash;
use super::model::Agent;

pub const AGENTS_DIR: &str = "agents";
pub const AGENT_FILE: &str = "agent.yaml";
/// The per-repo context overrides beside an agent's definition.
pub const REPO_OVERRIDES_DIR: &str = "repos";
const REPO_OVERRIDE_EXT: &str = ".yaml";

/// `{team}/config/agents`.
pub fn agents_dir(team_config_dir: &Path) -> PathBuf {
    team_config_dir.join(AGENTS_DIR)
}

/// One immutable revision file: the full definition as saved, plus who saved
/// it and when.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RevisionRecord {
    pub revision: u32,
    pub author: Author,
    /// RFC 3339 UTC.
    pub at: String,
    /// [`content_hash`] of the `agent.yaml` bytes this revision was saved as,
    /// so a revision keeps one hash whether it is read live or from here.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub definition_hash: String,
    pub agent: Agent,
    /// SW-XS-13: the per-repo context overrides as they were at this revision,
    /// keyed `host/owner/repo`. Part of the revision: covered by
    /// `definition_hash` and restored with it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub repo_overrides: RepoOverrides,
}

/// Per-repo overrides keyed `host/owner/repo`.
pub type RepoOverrides = BTreeMap<String, RepoOverride>;

/// The hash a revision carries: the `agent.yaml` bytes, then (only when there
/// are any, so an agent without overrides keeps its old hash) its overrides.
pub fn definition_hash(yaml: &[u8], overrides: &RepoOverrides) -> String {
    if overrides.is_empty() {
        return content_hash(yaml);
    }
    let mut bytes = yaml.to_vec();
    bytes.extend_from_slice(b"\n# repo overrides\n");
    bytes.extend_from_slice(
        serde_yaml::to_string(overrides)
            .expect("overrides serialize to YAML")
            .as_bytes(),
    );
    content_hash(&bytes)
}

/// A revision as [`read_pinned_full`] serves it.
#[derive(Debug, Clone, PartialEq)]
pub struct PinnedDef {
    pub agent: Agent,
    pub definition_hash: String,
    pub repo_overrides: RepoOverrides,
}

/// The current definition in `dir`, or `None` when there is none.
pub fn read_def(dir: &Path) -> Result<Option<Agent>, DefsError> {
    read_optional(&dir.join(AGENT_FILE))
}

/// The current bytes and their reference, loaded once for shared read models.
pub(crate) fn read_current_snapshot(dir: &Path) -> Result<Option<PinnedDef>, DefsError> {
    let path = dir.join(AGENT_FILE);
    let Some(bytes) = read_bytes(&path)? else {
        return Ok(None);
    };
    let agent = parse(&path, &bytes)?;
    let (repo_overrides, _) = read_repo_overrides(dir)?;
    Ok(Some(PinnedDef {
        agent,
        definition_hash: definition_hash(&bytes, &repo_overrides),
        repo_overrides,
    }))
}

/// Revision `n` of the agent in `dir`, or `None` when it does not exist.
pub fn read_revision(dir: &Path, n: u32) -> Result<Option<RevisionRecord>, DefsError> {
    read_optional(&revision_path(dir, n))
}

fn read_bytes(path: &Path) -> Result<Option<Vec<u8>>, DefsError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(DefsError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn parse<T: serde::de::DeserializeOwned>(path: &Path, bytes: &[u8]) -> Result<T, DefsError> {
    serde_yaml::from_slice(bytes).map_err(|e| yaml_err(path)(YamlIoError::Serialize(e)))
}

/// The definition at revision `n` and its [`definition_hash`]: the live
/// definition while it is at `n`, else revision `n`'s snapshot. A snapshot
/// newer than the live definition is a save that never completed and is not a
/// revision.
pub fn read_pinned(dir: &Path, n: u32) -> Result<Option<(Agent, String)>, DefsError> {
    Ok(read_pinned_full(dir, n)?.map(|p| (p.agent, p.definition_hash)))
}

/// [`read_pinned`] with the revision's per-repo overrides.
pub fn read_pinned_full(dir: &Path, n: u32) -> Result<Option<PinnedDef>, DefsError> {
    let live_path = dir.join(AGENT_FILE);
    let Some(live_bytes) = read_bytes(&live_path)? else {
        return Ok(None);
    };
    let live: Agent = parse(&live_path, &live_bytes)?;
    if live.revision == n {
        let (overrides, _) = read_repo_overrides(dir)?;
        return Ok(Some(PinnedDef {
            definition_hash: definition_hash(&live_bytes, &overrides),
            agent: live,
            repo_overrides: overrides,
        }));
    }
    if n > live.revision {
        return Ok(None);
    }
    let path = revision_path(dir, n);
    let Some(bytes) = read_bytes(&path)? else {
        return Ok(None);
    };
    let record: RevisionRecord = parse(&path, &bytes)?;
    // A snapshot written by hand carries no hash; its own bytes stand in.
    let hash = if record.definition_hash.is_empty() {
        content_hash(&bytes)
    } else {
        record.definition_hash
    };
    Ok(Some(PinnedDef {
        agent: record.agent,
        definition_hash: hash,
        repo_overrides: record.repo_overrides,
    }))
}

/// Every revision of the agent in `dir` up to `current`, oldest first. Files
/// that are not `<n>.yml`, and snapshots past `current` (a save that never
/// completed), are ignored.
pub fn read_revisions(dir: &Path, current: u32) -> Result<Vec<RevisionRecord>, DefsError> {
    let mut out = Vec::new();
    for n in revisioned_defs::revision_numbers(dir)? {
        if n > current {
            break;
        }
        if let Some(rec) = read_revision(dir, n)? {
            out.push(rec);
        }
    }
    Ok(out)
}

/// Write `def` as revision `def.revision`: first the immutable snapshot, then
/// the current `agent.yaml`. Both writes are atomic renames, so a crash between
/// them leaves the previous current definition intact and the orphan snapshot
/// is overwritten by the next save of the same number. Callers hold the
/// agent's lock (see [`with_agent_lock`]).
pub fn write_revision(dir: &Path, def: &Agent, author: &Author, at: &str) -> Result<(), DefsError> {
    let path = dir.join(AGENT_FILE);
    let yaml =
        serde_yaml::to_string(def).map_err(|e| yaml_err(&path)(YamlIoError::Serialize(e)))?;
    // The overrides on disk now are this revision's: a save writes them first.
    let (repo_overrides, _) = read_repo_overrides(dir)?;
    let record = RevisionRecord {
        revision: def.revision,
        author: author.clone(),
        at: at.to_string(),
        definition_hash: definition_hash(yaml.as_bytes(), &repo_overrides),
        agent: def.clone(),
        repo_overrides,
    };
    let rev_path = revision_path(dir, def.revision);
    write_yaml_value_atomic(&rev_path, &record).map_err(yaml_err(&rev_path))?;
    write_yaml_bytes_atomic(&path, yaml.as_bytes()).map_err(yaml_err(&path))
}

/// Snapshot the live definition as its own revision when it has none (a
/// hand-written or pulled definition), so it stays pinnable, with the same
/// hash, after the next save moves past it. The snapshot is attributed to its
/// creator.
pub fn snapshot_if_missing(dir: &Path, current: &Agent, at: &str) -> Result<(), DefsError> {
    let path = revision_path(dir, current.revision);
    if path.exists() {
        return Ok(());
    }
    let live_path = dir.join(AGENT_FILE);
    let live_bytes = read_bytes(&live_path)?.unwrap_or_default();
    let (repo_overrides, _) = read_repo_overrides(dir)?;
    let record = RevisionRecord {
        revision: current.revision,
        author: Author::user(current.created_by.clone()),
        at: at.to_string(),
        definition_hash: definition_hash(&live_bytes, &repo_overrides),
        agent: current.clone(),
        repo_overrides,
    };
    write_yaml_value_atomic(&path, &record).map_err(yaml_err(&path))
}

/// Every agent definition under `agents_dir`, as `(dir, result)`. A dir with
/// no `agent.yaml` is skipped; a malformed one is returned as an error so the
/// caller decides whether to skip it. `_`-prefixed dirs (`_builtin`,
/// `_unassigned`: routine homes, SW-AGR-1) are never agents and are skipped
/// silently, whatever they hold.
pub fn scan(agents_dir: &Path) -> Vec<(PathBuf, Result<Agent, DefsError>)> {
    revisioned_defs::scan(agents_dir, AGENT_FILE)
        .into_iter()
        .filter(|(dir, _)| {
            !dir.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('_'))
        })
        .collect()
}

/// Run `f` while holding the in-process lock for the agent at `dir`, so two
/// read-check-write saves of one agent cannot interleave.
pub fn with_agent_lock<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
    revisioned_defs::with_def_lock(dir, f)
}

/// `<dir>/repos/<host>/<owner>/<repo>.yaml`: the agent's override for `slug`,
/// filed under the repo's canonical (case-folded, host-aware) path key.
pub fn repo_override_path(dir: &Path, slug: &SourceRepoSlug) -> Result<PathBuf, ContextError> {
    let key = slug
        .physical_path_key()
        .map_err(|e| ContextError::InvalidRepo {
            repo: format!("{}/{}", slug.owner, slug.repo),
            why: e.to_string(),
        })?;
    let mut path = key.append_to(&dir.join(REPO_OVERRIDES_DIR));
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(REPO_OVERRIDE_EXT);
    path.set_file_name(name);
    Ok(path)
}

/// The agent's override for the repo filed at `path`, if any.
pub fn read_repo_override_at(path: &Path) -> Result<Option<RepoOverride>, DefsError> {
    read_optional(path)
}

/// Write (or, when empty, remove) the override at `path`, pruning the empty
/// `<host>/<owner>` directories a removal leaves under `<dir>/repos`.
pub fn write_repo_override_at(
    dir: &Path,
    path: &Path,
    value: &RepoOverride,
) -> Result<(), DefsError> {
    if !value.is_empty() {
        return write_yaml_value_atomic(path, value).map_err(yaml_err(path));
    }
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(DefsError::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    }
    let stop = dir.join(REPO_OVERRIDES_DIR);
    let mut at = path.parent();
    while let Some(parent) = at {
        if !parent.starts_with(&stop) || std::fs::remove_dir(parent).is_err() {
            break;
        }
        at = parent.parent();
    }
    Ok(())
}

/// The override file for the label `host/owner/repo` (as a revision records
/// it), or `None` when the label is not three plain path segments.
pub fn repo_override_path_for_label(dir: &Path, label: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = label.split('/').collect();
    let plain = |p: &&str| {
        !p.is_empty() && *p != "." && *p != ".." && !p.contains('\\') && !p.contains('\0')
    };
    if parts.len() != 3 || !parts.iter().all(plain) {
        return None;
    }
    let mut path = dir.join(REPO_OVERRIDES_DIR).join(parts[0]).join(parts[1]);
    path.push(format!("{}{REPO_OVERRIDE_EXT}", parts[2]));
    Some(path)
}

/// Every file under `<dir>/repos`, with its bytes: what a failed save puts
/// back exactly ([`restore_override_files`]), malformed files included.
pub fn snapshot_override_files(dir: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>, DefsError> {
    let mut out = BTreeMap::new();
    for path in override_files(dir)? {
        if let Some(bytes) = read_bytes(&path)? {
            out.insert(path, bytes);
        }
    }
    Ok(out)
}

/// Put `<dir>/repos` back to `snapshot`, byte for byte: every file it held is
/// rewritten as it was, and only files it did not hold are removed.
pub fn restore_override_files(
    dir: &Path,
    snapshot: &BTreeMap<PathBuf, Vec<u8>>,
) -> Result<(), DefsError> {
    for path in override_files(dir)? {
        if !snapshot.contains_key(&path) {
            write_repo_override_at(dir, &path, &RepoOverride::default())?;
        }
    }
    for (path, bytes) in snapshot {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| DefsError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        write_yaml_bytes_atomic(path, bytes).map_err(yaml_err(path))?;
    }
    Ok(())
}

/// Every regular file anywhere under `<dir>/repos`.
fn override_files(dir: &Path) -> Result<Vec<PathBuf>, DefsError> {
    let root = dir.join(REPO_OVERRIDES_DIR);
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(at) = stack.pop() {
        let entries = match std::fs::read_dir(&at) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => return Err(DefsError::Io { path: at, source }),
        };
        for entry in entries {
            let entry = entry.map_err(|source| DefsError::Io {
                path: at.clone(),
                source,
            })?;
            let kind = entry.file_type().map_err(|source| DefsError::Io {
                path: entry.path(),
                source,
            })?;
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                out.push(entry.path());
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Make the override files exactly `wanted` (restoring a revision): write
/// each, remove every other `.yaml` override file, malformed ones included.
pub fn replace_repo_overrides(dir: &Path, wanted: &RepoOverrides) -> Result<(), DefsError> {
    let keep: Vec<PathBuf> = wanted
        .keys()
        .filter_map(|label| repo_override_path_for_label(dir, label))
        .collect();
    for path in override_files(dir)? {
        let is_override = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(REPO_OVERRIDE_EXT));
        if is_override && !keep.contains(&path) {
            write_repo_override_at(dir, &path, &RepoOverride::default())?;
        }
    }
    for (label, value) in wanted {
        if let Some(path) = repo_override_path_for_label(dir, label) {
            write_repo_override_at(dir, &path, value)?;
        }
    }
    Ok(())
}

/// Every per-repo override of the agent in `dir`, keyed `host/owner/repo`,
/// plus one message per override file that cannot be read (skipped, so one
/// bad hand edit hides only itself). Anything that is not
/// `<host>/<owner>/<repo>.yaml` is ignored.
pub fn read_repo_overrides(dir: &Path) -> Result<(RepoOverrides, Vec<String>), DefsError> {
    let root = dir.join(REPO_OVERRIDES_DIR);
    let list = |at: &Path| -> Result<Vec<(String, PathBuf)>, DefsError> {
        match std::fs::read_dir(at) {
            Ok(entries) => {
                let mut out = Vec::new();
                for entry in entries {
                    let entry = entry.map_err(|source| DefsError::Io {
                        path: at.to_path_buf(),
                        source,
                    })?;
                    if let Ok(name) = entry.file_name().into_string() {
                        out.push((name, entry.path()));
                    }
                }
                out.sort();
                Ok(out)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(source) => Err(DefsError::Io {
                path: at.to_path_buf(),
                source,
            }),
        }
    };
    let mut out = RepoOverrides::new();
    let mut problems = Vec::new();
    for (host, host_dir) in list(&root)? {
        if !host_dir.is_dir() {
            continue;
        }
        for (owner, owner_dir) in list(&host_dir)? {
            if !owner_dir.is_dir() {
                continue;
            }
            for (file, path) in list(&owner_dir)? {
                let Some(repo) = file.strip_suffix(REPO_OVERRIDE_EXT) else {
                    continue;
                };
                if !path.is_file() {
                    continue;
                }
                let label = format!("{host}/{owner}/{repo}");
                // L6: a folder or file name in another case than the canonical
                // (lower-case) key is found only on a case-insensitive disk.
                if label != label.to_lowercase() {
                    problems.push(format!(
                        "{} is not in canonical case; rename it to repos/{}.yaml",
                        path.display(),
                        label.to_lowercase()
                    ));
                }
                match read_repo_override_at(&path) {
                    Ok(Some(value)) => {
                        out.insert(label, value);
                    }
                    Ok(None) => {}
                    Err(e) => problems.push(format!("skipped {}: {e}", path.display())),
                }
            }
        }
    }
    Ok((out, problems))
}

