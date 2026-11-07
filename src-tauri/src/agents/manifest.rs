// SPDX-License-Identifier: MIT
//! The startup manifest: what an agent launch starts with, resolved to exact
//! files. These types are the seam between the agent model (which produces a
//! manifest) and the context lifecycle (which bootstraps a workspace from one).
//! Their JSON shape is frozen by a golden test; changing a field needs both
//! sides to agree.
//!
//! # The consumer contract
//!
//! Consumers apply **exactly** these entries. For every [`ManifestFile`] of every
//! [`ManifestEntry`]: resolve its source through the current team view, check its bytes
//! against `hash`, and write them to `destination` in the workspace (with
//! `mode`). Consumers must not re-resolve startup paths or logical paths
//! through any other layer chain. A hash mismatch means the manifest is stale:
//! ask the producer (`resolve_agent`) again, and never substitute another
//! layer's file. A manifest whose `errors` is non-empty must not be
//! bootstrapped.
//!
//! [`GeneratedEntry`]s are launch files built for one launch (the agent's
//! native CLI permissions merged with what CodeSense itself needs). They are
//! never published back to any content layer and are always written outside
//! the published context, in a per-launch run dir. [`EssentialEntry`]s are
//! generated items with no source file, written by their existing writers.
//!
//! # Hashes
//!
//! Every hash is `blake3:<64 lowercase hex>` ([`content_hash`]):
//! - `ManifestFile::hash` is the hash of the file's saved bytes.
//! - `ManifestEntry::entry_hash` is [`entry_hash`] over the entry's files.
//! - `GeneratedEntry::hash` is [`json_hash`] of its `content`.
//! - `AgentRevisionRef::definition_hash` is the hash of the `agent.yaml` bytes
//!   of that revision. It is the same whether the revision is read live or
//!   from its history snapshot.
//! - `StartupManifest::manifest_hash` is [`StartupManifest::compute_hash`].

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::context_repo::scratch_space::is_scratch_team;
use crate::domain::Principal;

use super::keys::AgentKey;
use super::paths::StartupPath;
use super::roots;
use super::service::{self, AgentError};
use codesense_pty_core::agent_cli::AgentCli;

/// `blake3:<hex>` of `bytes`: the one hash format of the seam.
pub fn content_hash(bytes: &[u8]) -> String {
    render_hash(blake3::hash(bytes))
}

/// [`content_hash`] of everything `source` yields, read in chunks so a large
/// file is never held in memory, with its byte length. When `keep` is given
/// as `(into, at_most)`, the first `at_most` bytes are also appended to `into`.
pub fn content_hash_streamed(
    source: &mut dyn io::Read,
    mut keep: Option<(&mut Vec<u8>, usize)>,
) -> io::Result<(String, u64)> {
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 16 * 1024];
    let mut len = 0u64;
    loop {
        let n = match source.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        hasher.update(&buf[..n]);
        if let Some((into, at_most)) = keep.as_mut() {
            let room = at_most.saturating_sub(into.len()).min(n);
            into.extend_from_slice(&buf[..room]);
        }
        len += n as u64;
    }
    Ok((render_hash(hasher.finalize()), len))
}

fn render_hash(hash: blake3::Hash) -> String {
    format!("blake3:{}", hash.to_hex())
}

/// The hash of a JSON value's canonical bytes: object keys sorted at every
/// level, no whitespace. Sorted here rather than trusting the map type, whose
/// order depends on serde_json's build features.
pub fn json_hash(value: &serde_json::Value) -> String {
    fn canonical(value: &serde_json::Value, out: &mut String) {
        match value {
            serde_json::Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort_unstable();
                out.push('{');
                for (i, key) in keys.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::Value::String(key.clone()).to_string());
                    out.push(':');
                    canonical(&map[key], out);
                }
                out.push('}');
            }
            serde_json::Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    canonical(item, out);
                }
                out.push(']');
            }
            scalar => out.push_str(&scalar.to_string()),
        }
    }
    let mut out = String::new();
    canonical(value, &mut out);
    content_hash(out.as_bytes())
}

/// An entry's hash: [`content_hash`] over its files sorted by `logical`, each
/// contributing `logical`, a NUL, its `hash` and a newline. Independent of the
/// order `files` is listed in.
pub fn entry_hash(files: &[ManifestFile]) -> String {
    let mut pairs: Vec<(&str, &str)> = files
        .iter()
        .map(|f| (f.logical.as_str(), f.hash.as_str()))
        .collect();
    pairs.sort_unstable();
    let mut buf = Vec::new();
    for (logical, hash) in pairs {
        buf.extend_from_slice(logical.as_bytes());
        buf.push(0);
        buf.extend_from_slice(hash.as_bytes());
        buf.push(b'\n');
    }
    content_hash(&buf)
}

/// The agent definition a launch ran: which agent, at which revision, and the
/// hash of exactly the bytes that revision was read from. It renders as
/// `<key>@<revision>`, the provenance recorded on a workspace or run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentRevisionRef {
    /// The agent's stable key (`<org>/<team>/<id>`, or
    /// `_personal/<user>/<org>/_scratch/<id>`).
    pub key: AgentKey,
    /// The definition revision the manifest was resolved from.
    pub revision: u32,
    /// [`content_hash`] of that revision's `agent.yaml` bytes.
    pub definition_hash: String,
}

impl fmt::Display for AgentRevisionRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.key, self.revision)
    }
}

impl AgentRevisionRef {
    /// The agent key of a rendered `"<key>@<revision>"` stamp, as a launch
    /// records it.
    pub fn key_of(stamp: &str) -> Option<AgentKey> {
        let (key, revision) = stamp.rsplit_once('@')?;
        revision.parse::<u32>().ok()?;
        AgentKey::parse(key)
    }
}

/// An agent's effective startup context, resolved to exact files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct StartupManifest {
    /// The agent revision this manifest was resolved from.
    pub agent: AgentRevisionRef,
    /// The CLI the launch runs; it decides which generated targets apply.
    pub runtime: AgentCli,
    /// Content entries in application order: declared paths in the order the
    /// agent lists them, each followed by any dependency it pulled in, then
    /// the entries CodeSense requires.
    pub entries: Vec<ManifestEntry>,
    /// Generated items CodeSense always provides, which have no source file.
    pub essentials: Vec<EssentialEntry>,
    /// Per-launch generated files, never published back.
    pub generated: Vec<GeneratedEntry>,
    /// Sums over every [`ManifestFile`] of every entry.
    pub totals: ManifestTotals,
    /// Why the manifest cannot be applied as is. Non-empty means: do not
    /// bootstrap.
    pub errors: Vec<ManifestError>,
    /// [`StartupManifest::compute_hash`]: identifies this exact manifest.
    pub manifest_hash: String,
}

impl StartupManifest {
    /// [`json_hash`] of this manifest's JSON with `manifest_hash` left empty,
    /// so it covers every other field.
    pub fn compute_hash(&self) -> String {
        let mut unhashed = self.clone();
        unhashed.manifest_hash = String::new();
        json_hash(&serde_json::to_value(&unhashed).expect("manifest serializes to JSON"))
    }
}

/// One startup path, expanded to its files, as resolved in one layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ManifestEntry {
    /// The normalized startup path: the entry's stable identity. It names a
    /// file or a directory relative to the layer's content root.
    pub path: StartupPath,
    /// Why the entry is in the manifest.
    pub origin: EntryOrigin,
    /// The content layer every file of this entry comes from.
    pub layer: ContentLayer,
    /// [`entry_hash`] over `files`.
    pub entry_hash: String,
    /// `path` expanded: the one file for a file entry, every regular file under
    /// it (sorted by `logical`) for a directory entry.
    pub files: Vec<ManifestFile>,
}

/// Why an entry is in the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EntryOrigin {
    /// Listed in the agent's `startup:`.
    Declared,
    /// Pulled in because the entry at `of` requires it.
    Dependency {
        /// The entry that requires this one.
        of: StartupPath,
    },
    /// Content CodeSense itself needs at launch (e.g. the notify hook script).
    Required,
}

/// A content layer: a team's or a user's personal `content/` dir.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContentLayer {
    /// A team root: the target team, a hop of its team chain, or a mapped
    /// provider team.
    Team {
        /// The org slug (`owner/repo`).
        org: String,
        /// The team name within the org.
        team: String,
    },
    /// A local user's scratch space: one org's folder, or the all-orgs
    /// folder of inert items when `org` is absent.
    Personal {
        /// The local user id whose scratch space this is.
        account: String,
        /// The org whose scratch folder this is; `None` for all-orgs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        org: Option<String>,
    },
}

impl ContentLayer {
    /// An explicit owning layer's `content/` directory. Inherited resolution
    /// goes through TeamView's filtered roots.
    ///
    /// The layer's `content/` dir on this machine, for a launch by `principal`
    /// (team layers resolve through that user's checkouts; a personal layer
    /// is that user's own space). This addresses ownership, not inheritance.
    ///
    /// A layer read off the wire is checked before it becomes a path: a team
    /// layer must name a valid org and team other than the personal org (that
    /// space is only ever a `Personal` layer), and a personal layer must be
    /// `principal`'s own. Anything else is [`AgentError::InvalidLocation`], so
    /// no layer points outside the context-repo tree or into another user's
    /// personal space.
    pub fn content_root(
        &self,
        codesense_home: &Path,
        principal: &Principal,
    ) -> Result<PathBuf, AgentError> {
        match self {
            Self::Team { org, team } => {
                if is_scratch_team(team) {
                    return Err(AgentError::InvalidLocation(
                        "the private space is yours alone, not a team's".into(),
                    ));
                }
                service::check_org(org)?;
                service::check_team(team)?;
                roots::team_content_dir(codesense_home, principal, org, team)
            }
            Self::Personal { account, org } => {
                // Personal content stays private by placement: only the
                // launching user's own space resolves, as with personal keys.
                if account != principal.as_str() {
                    return Err(AgentError::InvalidLocation(format!(
                        "{account:?} is not this user's private space"
                    )));
                }
                match org {
                    Some(org) => {
                        service::check_org(org)?;
                        roots::personal_content_dir(codesense_home, principal, org)
                    }
                    None => Ok(roots::all_orgs_content_dir(codesense_home, principal)),
                }
            }
        }
    }
}

/// One file to place in the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ManifestFile {
    /// The path relative to the layer's content root;
    /// `content_root(layer).join(logical)` is the exact source.
    pub logical: String,
    /// The workspace-relative path to write. Carried explicitly so consumers
    /// never derive it.
    pub destination: String,
    /// [`content_hash`] of the saved bytes.
    pub hash: String,
    /// The byte length of the saved file.
    pub bytes: u64,
    /// The permission bits to write it with: only `mode & 0o777` is
    /// meaningful (e.g. `0o755`, 493 on the wire, for a hook script; `0o644`
    /// otherwise). Never file-type bits.
    pub mode: u32,
    /// SLW-PT-20: the layer this file comes from when it differs from its entry's — a
    /// startup FOLDER merges per file across the org's scratch space, the team and
    /// all-orgs exactly as the team's worktree does. Absent = the entry's layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<ContentLayer>,
}

impl ManifestFile {
    /// The layer this file is read from.
    pub fn layer_in<'a>(&'a self, entry: &'a ContentLayer) -> &'a ContentLayer {
        self.layer.as_ref().unwrap_or(entry)
    }
}

/// A generated item CodeSense always provides, with no source file (e.g. the
/// MCP bridge registration).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct EssentialEntry {
    /// A stable id, e.g. `codesense-bridge`.
    pub id: String,
    /// Why CodeSense needs it, shown in the preview.
    pub reason: String,
}

/// Where a generated launch file goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeneratedTarget {
    /// The launch's own settings for its runtime (permissions and what CodeSense needs),
    /// handed over the way its descriptor's `launch_settings` says: Claude's whole
    /// per-launch `--settings` file, never the shared workspace
    /// `.claude/settings.local.json`, so two agents launched into one folder never see
    /// each other's rules; Codex's keys as `-c` overrides.
    LaunchSettings,
    /// SW-XS-14: a file in the workspace at [`GeneratedEntry::path`], built at
    /// launch as the deep merge of a merge-marked content file across its
    /// layers (`merge-marked.yaml`), then merged into the workspace's own copy
    /// (the workspace's keys win).
    WorkspaceFile,
}

/// A generated launch file: the agent's native permissions merged with what
/// CodeSense requires. Written per launch outside the published context, never
/// published back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GeneratedEntry {
    /// A stable id, e.g. `claude-permissions`.
    pub id: String,
    /// Where the content is written.
    pub target: GeneratedTarget,
    /// The content, in the target's own shape.
    pub content: serde_json::Value,
    /// [`json_hash`] of `content`.
    pub hash: String,
    /// The parts of `content` CodeSense added and the agent cannot drop, so a
    /// preview can mark them.
    pub required: Vec<String>,
    /// For [`GeneratedTarget::WorkspaceFile`]: the workspace-relative path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// For [`GeneratedTarget::WorkspaceFile`]: the merge mark's array keys
    /// (dot path → key field); an array without one replaces.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub merge_keys: BTreeMap<String, String>,
}

/// Sums over every file of every entry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ManifestTotals {
    /// How many files.
    pub files: u64,
    /// How many bytes.
    pub bytes: u64,
}

/// One reason a manifest cannot be applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ManifestError {
    /// The startup path at fault, when the error belongs to one.
    pub path: Option<StartupPath>,
    /// What kind of failure.
    pub code: ManifestErrorCode,
    /// A sentence for the user.
    pub message: String,
}

/// What kind of failure a [`ManifestError`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestErrorCode {
    /// The path exists in no layer.
    Missing,
    /// The path resolves outside the content root (e.g. through a symlink).
    EscapesContent,
    /// Dependencies require each other in a loop.
    DependencyCycle,
    /// The entry or its dependency declaration is malformed.
    Invalid,
}
