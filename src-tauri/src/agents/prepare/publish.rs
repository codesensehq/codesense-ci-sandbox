// SPDX-License-Identifier: MIT
//! SW-XS-8: the explicit publish (SPEC §5, §11.11, X6) — the only way a
//! workspace's edits reach team content.
//!
//! - **Candidates**: the workspace's managed files (what Step 2 or Step 3
//!   copied there), plus new files under the agent's declared prefixes (the
//!   prefixes every run here recorded, or the `sets` / `paths` the caller
//!   names).
//! - **Target**: a managed file goes back to the team file it was copied
//!   from (its record's source, in its `source_layer`); a new file goes to
//!   the active team's content, or to the tier `to` names.
//! - **Conflicts are never written**: a team file whose bytes differ from
//!   the managed base (someone changed it since it was copied), or a new file
//!   whose target already holds other bytes.
//! - **Removals** (review r1): only with `includeDeletes: true`, only a
//!   managed file the workspace no longer has, and only while the team file
//!   still holds the base. Without it a publish never deletes team content
//!   (a `git clean -fdx` in the workspace must not wipe the team); the
//!   would-be removals are listed (`withheldRemovals`).
//! - **Size**: a file over 4 MiB (either side) is skipped with its reason.
//!
//! On a machine a preview is ONE `WorkspaceManifest` call; a publish is that
//! call plus ONE more that streams every changed file's bytes back (X2), and,
//! only when it adds new files, one exclude-block batch (begin, seal, apply,
//! finish) so they stay hidden from git (X7). Every machine-supplied path is
//! validated as a contained relative path, and every team target must lie
//! lexically inside a content root this workspace resolves through.
//!
//! The team-content write is all-or-nothing against CodeSense's own writers:
//! every entry is written by compare-and-swap against what the plan saw (the
//! check and the write share the content mutation lock), and a race on any
//! entry rolls back the ones already written — each only while the target
//! still holds what this publish wrote. An editor writing team content
//! outside CodeSense at the same instant is not serialized.
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::progress::{PreparePhase, PrepareProgress};
use super::record::{Hash32, ManagedEntry, ManagedRecord, StatKey};
use super::Destination;
use crate::agents::manifest_resolve::ManifestTarget;
use crate::content_engine::destination::{
    BatchExclude, DestinationBatch, DestinationIdentity, DestinationIo,
};
use crate::domain::Principal;
use codesense_pty_core::remote_manifest::{EntryKind, MAX_ENTRIES};

/// The hashes a caller expects (the selected form's `expectedSource` /
/// `expectedWorkspace`): a mismatch is a conflict, never a write.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Expect {
    pub source: Option<[u8; 32]>,
    pub workspace: Option<[u8; 32]>,
    /// The selected form's `ref`: the team-relative file a NEW workspace file
    /// publishes to (default: the same path).
    pub target: Option<String>,
    /// SW-XS-27: `(size, mtime_ns)` of a new local file a count listed without
    /// reading it: a file whose stat moved since is a conflict.
    pub stat: Option<(u64, i64)>,
}

/// One explicit publish (or its preview).
pub struct PublishRequest<'a> {
    pub home: &'a Path,
    pub principal: &'a Principal,
    pub destination: Destination<'a>,
    /// Where new files and old (sourceless) managed files resolve.
    pub target: &'a ManifestTarget,
    /// Only these files or prefixes (none: every candidate).
    pub paths: &'a [String],
    /// Also look for new files under these prefixes.
    pub sets: &'a [String],
    /// The tier a new file goes to: `team` (default), `repo`, `personal`,
    /// or a full label (`team:org/team`).
    pub to: Option<&'a str>,
    pub expect: &'a BTreeMap<String, Expect>,
    /// Remove team files whose managed copy the workspace deleted. Off by
    /// default: a publish never deletes team content unless asked.
    pub include_deletes: bool,
    /// SW-XS-8b (review r1): the legacy lockfile rows of a workspace whose
    /// record was never seeded (a pre-upgrade worktree no launch touched
    /// since): read once to seed it, so its projected files publish back.
    pub legacy: Option<&'a dyn Fn() -> Result<super::LegacyRows, String>>,
    pub operation: &'a str,
    pub workspace: String,
    pub agent: String,
    pub progress: &'a dyn Fn(&PrepareProgress),
    pub validate: &'a dyn Fn() -> Result<(), String>,
    pub cancel: Option<&'a dyn Fn() -> bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PublishAction {
    Add,
    Update,
    Remove,
}

/// One change a publish makes (or a preview would make).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishEntry {
    /// Workspace-relative.
    pub path: String,
    pub action: PublishAction,
    /// The tier it is written to.
    pub target_layer: String,
    /// The team-content file it is written to.
    pub target_path: PathBuf,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishConflict {
    pub path: String,
    pub reason: String,
    pub target_layer: String,
    pub target_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PublishStatus {
    /// A preview: nothing was written.
    Preview,
    /// Every planned entry was written and nothing conflicted.
    Published,
    /// Some entries were written (`entries`); the conflicts and failures
    /// listed were not — or, when only the exclude-block update failed after
    /// the team write, everything landed but the new files are not hidden
    /// from git yet (that failure is listed).
    PublishedWithConflicts,
    /// Nothing differs from team content.
    NothingToPublish,
    /// At least one entry could not be written.
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishReport {
    pub status: PublishStatus,
    /// What was published (or, for a preview, what would be).
    pub entries: Vec<PublishEntry>,
    /// Never written: they need a person (or `hydrate_worktree
    /// {overwriteModified:true}` to take the team's version).
    pub conflicts: Vec<PublishConflict>,
    /// `[path, reason]`: tracked by the repo, excluded, a symlink…
    pub skipped: Vec<(String, String)>,
    /// `[path, reason]`.
    pub failed: Vec<(String, String)>,
    /// Candidates identical to team content.
    pub unchanged: usize,
    /// Managed files deleted in the workspace whose team file a publish with
    /// `includeDeletes: true` would remove (never removed without it).
    pub withheld_removals: Vec<String>,
    pub elapsed_ms: u64,
}

impl PublishReport {
    /// Architecture: preserve the mutation; expose only the admitted preview's readback.
    pub(crate) fn retain_readback(
        &mut self,
        visible: &Self,
        scope: &crate::context_repo::caller_scope::CallerScope,
    ) {
        let paths: BTreeSet<_> = visible
            .entries
            .iter()
            .map(|e| e.path.as_str())
            .chain(visible.conflicts.iter().map(|e| e.path.as_str()))
            .chain(visible.skipped.iter().map(|(p, _)| p.as_str()))
            .chain(visible.failed.iter().map(|(p, _)| p.as_str()))
            .chain(visible.withheld_removals.iter().map(String::as_str))
            .collect();
        if self
            .entries
            .iter()
            .all(|entry| scope.allows(&entry.target_path))
            && self
                .conflicts
                .iter()
                .all(|entry| scope.allows(&entry.target_path))
            && self
                .skipped
                .iter()
                .chain(&self.failed)
                .all(|(path, _)| paths.contains(path.as_str()))
            && self
                .withheld_removals
                .iter()
                .all(|path| paths.contains(path.as_str()))
            && self.unchanged == visible.unchanged
        {
            return;
        }
        self.entries
            .retain(|entry| scope.allows(&entry.target_path));
        self.conflicts
            .retain(|entry| scope.allows(&entry.target_path));
        self.skipped
            .retain(|(path, _)| paths.contains(path.as_str()));
        self.failed
            .retain(|(path, _)| paths.contains(path.as_str()));
        self.withheld_removals
            .retain(|path| paths.contains(path.as_str()));
        self.unchanged = visible.unchanged;
        self.status = if self.status == PublishStatus::Cancelled {
            PublishStatus::Cancelled
        } else if !self.failed.is_empty() {
            PublishStatus::Failed
        } else if !self.conflicts.is_empty() {
            PublishStatus::PublishedWithConflicts
        } else if self.entries.is_empty() {
            PublishStatus::NothingToPublish
        } else {
            PublishStatus::Published
        };
    }
}

/// What the workspace holds at one path.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seen {
    Absent,
    File {
        hash: [u8; 32],
        mode: u32,
        size: u64,
        stat: Option<StatKey>,
    },
    Other(String),
    /// SW-XS-27: a new local file a count did not read (no team file at its
    /// path to compare it with, so its bytes don't matter to the answer).
    Unhashed {
        mode: u32,
        size: u64,
        stat: Option<StatKey>,
    },
}

/// One planned change.
#[derive(Debug, Clone)]
struct Planned {
    entry: PublishEntry,
    /// What the team file must still hold (`None`: absent).
    team_expected: Option<[u8; 32]>,
    /// The workspace bytes' hash (writes).
    workspace_hash: Option<[u8; 32]>,
    mode: u32,
    stat: Option<StatKey>,
    /// A managed file keeps its provenance; a new one becomes this
    /// operation's.
    managed: Option<ManagedEntry>,
}

struct Plan {
    planned: Vec<Planned>,
    conflicts: Vec<PublishConflict>,
    skipped: Vec<(String, String)>,
    unchanged: usize,
    /// Managed entries to forget (gone on both sides) or to re-base (the
    /// team already holds the workspace bytes).
    forget: Vec<String>,
    rebase: Vec<(String, [u8; 32], Option<StatKey>)>,
    /// Remote: the manifest prefixes (asked again to read bytes).
    prefixes: Vec<String>,
    /// Remote: the block the manifest reported.
    remote_block: Option<Vec<String>>,
    /// Removals withheld (no `includeDeletes`), each with the tier it would
    /// be removed from.
    withheld: Vec<(String, String, PathBuf)>,
    /// Workspace files read to hash them (local; SW-XS-27's budget).
    hashed: usize,
    /// SW-XS-27: edits a publish can't move (`path, tier, why`): a workspace
    /// file changed or added here that is too large. Also in `skipped`.
    held: Vec<Held>,
    /// SW-XS-27: the workspace bytes' hash of every file the plan read (what a
    /// later action pins as "what was looked at").
    hashes: HashMap<String, [u8; 32]>,
    /// SW-XS-27: `(size, mtime_ns)` of every new local file it did not read.
    stats: HashMap<String, (u64, i64)>,
    /// Remote: what a cut batch applied, reported by the manifest.
    recovered: Option<crate::content_engine::destination::RecoveredBatch>,
}

/// The largest file a publish moves (the compare-and-swap writer's bound).
const MAX_PUBLISH_BYTES: u64 = 4 * 1024 * 1024;

/// `target` is lexically inside one of `roots` (no `.`/`..` component, not a
/// root itself): the last check before any team-content write.
fn contained(
    target: &Path,
    roots: &[(String, crate::context_repo::inheritance::ContentRoot)],
) -> bool {
    use std::path::Component;
    target.is_absolute()
        && !target
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        && roots
            .iter()
            .any(|(_, root)| root.relative(target).is_some())
}

fn under(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|tail| tail.starts_with('/'))
}

fn hex(hash: &[u8; 32]) -> String {
    blake3::Hash::from_bytes(*hash).to_hex().to_string()
}

fn team_hash(path: &Path) -> Result<Option<[u8; 32]>, String> {
    team_state(path, None).map(|state| state.map(|(hash, _)| hash))
}

/// The team file's hash and size. `known`: a managed entry's base and the
/// team file's stat when it was last written or copied; a team file whose
/// stat still matches is not hashed again.
fn team_state(
    path: &Path,
    known: Option<(&ManagedEntry, i64)>,
) -> Result<Option<([u8; 32], u64)>, String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_file() => {
            if let Some((entry, saved_at_ns)) = known {
                if StatKey::of(&meta).trusted(entry.source_stat, saved_at_ns) {
                    return Ok(Some((entry.hash.0, meta.len())));
                }
            }
            crate::content_engine::atomic_io::hash_file(path)
                .map(|hash| Some((hash, meta.len())))
                .map_err(|e| e.to_string())
        }
        Ok(_) => Err("its team-content path is not a regular file".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

/// Where a new file goes: the tier `to` names among `roots`.
fn new_file_root<'r>(
    roots: &'r [(String, crate::context_repo::inheritance::ContentRoot)],
    target: &ManifestTarget,
    to: Option<&str>,
) -> Result<&'r (String, crate::context_repo::inheritance::ContentRoot), String> {
    let team = format!("team:{}/{}", target.org, target.team);
    let wanted = match to.unwrap_or("team") {
        "team" => team,
        "repo" => roots
            .iter()
            .find(|(label, _)| label.starts_with("repo:"))
            .map(|(label, _)| label.clone())
            .ok_or("this workspace has no repo tier to publish to")?,
        "personal" => roots
            .iter()
            .find(|(label, _)| label.starts_with("personal"))
            .map(|(label, _)| label.clone())
            .ok_or("you have no Private space to publish to")?,
        label => label.to_string(),
    };
    roots
        .iter()
        .find(|(label, _)| *label == wanted)
        .ok_or_else(|| format!("{wanted} is not a tier this workspace resolves through"))
}

/// The team file a managed entry publishes to, with its tier.
fn recorded_roots(
    request: &PublishRequest<'_>,
    entry: &ManagedEntry,
) -> Result<Vec<(String, crate::context_repo::inheritance::ContentRoot)>, String> {
    let mut target = request.target.clone();
    target.context_root = match entry.source_context.as_deref() {
        None | Some("clone") => None,
        Some(id) => {
            let row = crate::install::worktree_index::resolve_context_record(
                request.home,
                request.principal,
                &target.org,
                None,
                id,
            )?;
            Some(crate::agents::manifest_resolve::ContextRoot {
                id: row.id,
                root: row.root,
            })
        }
    };
    Ok(super::step3::labelled_roots(
        request.home,
        request.principal,
        &target,
    ))
}

fn managed_target(
    roots: &[(String, crate::context_repo::inheritance::ContentRoot)],
    original: &[(String, crate::context_repo::inheritance::ContentRoot)],
    path: &str,
    entry: &ManagedEntry,
) -> Result<(String, PathBuf), String> {
    if let Some(source) = &entry.source {
        let (label, relative) = original
            .iter()
            .filter_map(|(label, root)| root.relative(source).map(|rel| (label, rel)))
            .min_by_key(|(_, rel)| rel.components().count())
            .ok_or("not found")?;
        let chosen = roots
            .iter()
            .find(|(candidate, _)| candidate == label)
            .ok_or("not found")?;
        let target = chosen
            .1
            .file_path(relative.to_str().ok_or("not found")?)
            .ok_or("not found")?;
        return Ok((label.clone(), target));
    }

    // An older or seeded entry: the tier that provides the path now, else
    // the tier it names, else the team.
    if let Some((label, root)) = roots.iter().find(|(_, root)| root.exists(path)) {
        return Ok((label.clone(), root.file_path(path).ok_or("not found")?));
    }
    let (label, root) = roots
        .iter()
        .find(|(label, _)| *label == entry.source_layer)
        .or_else(|| roots.iter().find(|(label, _)| label.starts_with("team:")))
        .ok_or("this workspace resolves through no team content")?;
    Ok((label.clone(), root.file_path(path).ok_or("not found")?))
}

/// Paths that may never be published.
fn unpublishable(path: &str, excludes: &crate::content_engine::TeamExcludes) -> Option<String> {
    crate::agents::context::unsafe_destination(path).or_else(|| {
        crate::content_engine::is_projection_excluded(path, excludes)
            .then(|| "it is excluded from team content".into())
    })
}

struct Observed {
    seen: HashMap<String, (Seen, bool)>,
    /// Files under the new-file prefixes the record doesn't manage.
    new_files: Vec<String>,
    /// Remote: the manifest prefixes and the block it reported.
    prefixes: Vec<String>,
    block: Option<Vec<String>>,
    /// Remote: what a cut batch applied (settled into the record).
    recovered: Option<crate::content_engine::destination::RecoveredBatch>,
    /// Workspace files read to hash them (local).
    hashed: usize,
}

/// Folders a new-file walk never descends into: dependency and build trees
/// are never team content, and walking them dominates a large workspace.
const NEVER_WALKED: &[&str] = &["node_modules", ".git", "target", ".venv", "__pycache__"];

fn observe_local(
    root: &Path,
    record: &ManagedRecord,
    managed: &[String],
    new_prefixes: &[String],
    skip: &dyn Fn(&str) -> bool,
    lazy_new: bool,
) -> Result<Observed, String> {
    let tracked = super::local::Tracked::open(root)?;
    let mut seen = HashMap::new();
    let hashed = std::cell::Cell::new(0usize);
    let look = |path: &str, cached: Option<&ManagedEntry>| -> Seen {
        let full = root.join(path);
        // Never through a symlinked folder.
        if let Ok(parent) = full.parent().unwrap_or(root).canonicalize() {
            if !parent.starts_with(root) {
                return Seen::Other("its folder resolves outside the workspace".into());
            }
        }
        match std::fs::symlink_metadata(&full) {
            Ok(meta) if meta.file_type().is_file() => {
                let stat = StatKey::of(&meta);
                #[cfg(unix)]
                let mode = {
                    use std::os::unix::fs::PermissionsExt;
                    codesense_pty_core::remote_manifest::normalized_mode(meta.permissions().mode())
                };
                #[cfg(not(unix))]
                let mode = 0o644;
                let hash = match cached.filter(|e| stat.trusted(e.dest_stat, record.saved_at_ns)) {
                    Some(entry) => entry.hash.0,
                    None => {
                        hashed.set(hashed.get() + 1);
                        match crate::content_engine::atomic_io::hash_file(&full) {
                            Ok(hash) => hash,
                            Err(error) => return Seen::Other(error.to_string()),
                        }
                    }
                };
                Seen::File {
                    hash,
                    mode,
                    size: meta.len(),
                    stat: Some(stat),
                }
            }
            Ok(meta) if meta.file_type().is_symlink() => Seen::Other("a symlink is there".into()),
            Ok(_) => Seen::Other("a directory or special file is there".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Seen::Absent,
            Err(error) => Seen::Other(error.to_string()),
        }
    };
    for path in managed {
        let state = look(path, record.entries.get(path));
        seen.insert(path.clone(), (state, tracked.contains(path)));
    }
    let mut new_files = Vec::new();
    for prefix in new_prefixes {
        let start = root.join(prefix);
        let walker = walkdir::WalkDir::new(&start)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| {
                if !entry.file_type().is_dir() || entry.depth() == 0 {
                    return true;
                }
                // Never into a nested repository, a dependency tree, or a
                // folder team content excludes.
                let name = entry.file_name().to_string_lossy();
                if NEVER_WALKED.contains(&name.as_ref()) || entry.path().join(".git").exists() {
                    return false;
                }
                entry
                    .path()
                    .strip_prefix(root)
                    .ok()
                    .and_then(|rel| rel.to_str())
                    .is_none_or(|rel| !skip(&format!("{}/", rel.replace('\\', "/"))))
            });
        for entry in walker.filter_map(Result::ok) {
            if !entry.file_type().is_file() {
                continue;
            }
            let Ok(relative) = entry.path().strip_prefix(root) else {
                continue;
            };
            let Some(path) = relative.to_str().map(|p| p.replace('\\', "/")) else {
                continue;
            };
            if record.entries.contains_key(&path) || seen.contains_key(&path) {
                continue;
            }
            // Filter before hashing: tracked or unpublishable files are never
            // candidates, so they are never read.
            if tracked.contains(&path) || skip(&path) {
                continue;
            }
            let state = if lazy_new {
                // A count reads a new file only if the plan needs its bytes.
                match std::fs::symlink_metadata(root.join(&path)) {
                    Ok(meta) if meta.file_type().is_file() => {
                        #[cfg(unix)]
                        let mode = {
                            use std::os::unix::fs::PermissionsExt;
                            codesense_pty_core::remote_manifest::normalized_mode(
                                meta.permissions().mode(),
                            )
                        };
                        #[cfg(not(unix))]
                        let mode = 0o644;
                        Seen::Unhashed {
                            mode,
                            size: meta.len(),
                            stat: Some(StatKey::of(&meta)),
                        }
                    }
                    _ => look(&path, None),
                }
            } else {
                look(&path, None)
            };
            seen.insert(path.clone(), (state, false));
            new_files.push(path);
        }
    }
    new_files.sort();
    Ok(Observed {
        seen,
        new_files,
        prefixes: Vec::new(),
        block: None,
        recovered: None,
        hashed: hashed.get(),
    })
}

/// The manifest prefixes that cover `paths` (at most 32: folded to their
/// top-level folders when there are more).
fn cover(paths: &[String]) -> Result<Vec<String>, String> {
    use codesense_pty_core::remote_manifest::normalize_prefixes;
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    normalize_prefixes(paths).or_else(|_| {
        let mut tops: Vec<String> = paths
            .iter()
            .map(|p| p.split('/').next().unwrap_or(p).to_string())
            .collect();
        tops.sort();
        tops.dedup();
        normalize_prefixes(&tops)
    })
}

fn observe_remote(
    io: &dyn DestinationIo,
    record: &ManagedRecord,
    managed: &[String],
    new_prefixes: &[String],
    progress: &mut dyn FnMut(&codesense_pty_core::remote_manifest::ManifestProgress),
) -> Result<Observed, String> {
    let prefixes = remote_prefixes(managed, new_prefixes)?;
    let manifest = if prefixes.is_empty() {
        None
    } else {
        Some(
            io.manifest(&prefixes, MAX_ENTRIES, progress)
                .map_err(|e| e.to_string())?,
        )
    };
    observed_from_manifest(manifest, prefixes, record, managed, new_prefixes)
}

/// SW-XS-27: [`observe_remote`] for a count — the same ONE manifest call,
/// through [`DestinationIo::manifest_observe`], which never settles a batch
/// an earlier call left behind (that is a write on the machine). Its call
/// graph never reaches `reconcile_pending`.
fn observe_remote_read_only(
    io: &dyn DestinationIo,
    record: &ManagedRecord,
    managed: &[String],
    new_prefixes: &[String],
) -> Result<Observed, String> {
    let prefixes = remote_prefixes(managed, new_prefixes)?;
    // Even with no copied files, the manifest verifies the completed workspace
    // directory. An empty prefix set reads no content.
    let manifest = Some(
        io.manifest_observe(&prefixes, MAX_ENTRIES, &mut |_| {})
            .map_err(|e| e.to_string())?,
    );
    observed_from_manifest(manifest, prefixes, record, managed, new_prefixes)
}

fn remote_prefixes(managed: &[String], new_prefixes: &[String]) -> Result<Vec<String>, String> {
    let mut wanted: Vec<String> = managed.to_vec();
    wanted.extend(new_prefixes.iter().cloned());
    cover(&wanted)
}

/// SW-XS-27: what `paths` hold in the workspace now (`None`: absent or not a
/// file) — an explicit action checking that a file is still what the person
/// looked at. On a machine, ONE read-only manifest call (refused while an
/// earlier copy there is unfinished: try again once it settles).
pub(crate) fn workspace_hashes(
    destination: Destination<'_>,
    paths: &[String],
) -> Result<HashMap<String, Option<[u8; 32]>>, String> {
    let mut out = HashMap::new();
    if paths.is_empty() {
        return Ok(out);
    }
    match destination {
        Destination::Local { root } => {
            let root = root
                .canonicalize()
                .map_err(|e| format!("workspace {} is unavailable: {e}", root.display()))?;
            for path in paths {
                let full = root.join(path);
                let hash = match std::fs::symlink_metadata(&full) {
                    Ok(meta) if meta.file_type().is_file() => Some(
                        crate::content_engine::atomic_io::hash_file(&full)
                            .map_err(|e| e.to_string())?,
                    ),
                    _ => None,
                };
                out.insert(path.clone(), hash);
            }
        }
        Destination::Remote { io } => {
            // Read-only: a check never settles a batch an earlier call left
            // behind (the operation itself does, under its lock).
            let prefixes = remote_prefixes(paths, &[])?;
            let manifest = io
                .manifest_observe(&prefixes, MAX_ENTRIES, &mut |_| {})
                .map_err(|e| e.to_string())?;
            for path in paths {
                let hash = manifest
                    .files
                    .iter()
                    .find(|e| &e.path == path && e.kind == EntryKind::File)
                    .and_then(|e| e.hash);
                out.insert(path.clone(), hash);
            }
        }
    }
    Ok(out)
}

/// What one manifest says about the workspace (pure: no I/O).
fn observed_from_manifest(
    manifest: Option<crate::content_engine::destination::CompleteManifest>,
    prefixes: Vec<String>,
    record: &ManagedRecord,
    managed: &[String],
    new_prefixes: &[String],
) -> Result<Observed, String> {
    let mut seen = HashMap::new();
    let mut new_files = Vec::new();
    let mut block = None;
    let mut recovered = None;
    if let Some(manifest) = manifest {
        if let Some(codesense_pty_core::remote_manifest::WorkspaceExclude::Paths { paths }) =
            manifest.workspace_exclude
        {
            block = Some(paths);
        }
        let in_scope = |path: &str| {
            managed.iter().any(|m| m == path) || new_prefixes.iter().any(|p| under(path, p))
        };
        // Review r1 M1: the machine names paths; never trust one that is not
        // a contained relative path (`docs/../../.ssh/…`).
        for entry in manifest.files.iter().chain(&manifest.skipped) {
            if crate::content_engine::destination::DestinationRelativePath::try_from(
                entry.path.clone(),
            )
            .is_err()
            {
                return Err(format!(
                    "the machine reported an unsafe path {:?}; nothing was published",
                    entry.path
                ));
            }
        }
        recovered = manifest.recovered.clone();
        for entry in manifest.files.iter().chain(&manifest.skipped) {
            if !in_scope(&entry.path) {
                continue;
            }
            let state = match (entry.kind, entry.hash) {
                (EntryKind::File, Some(hash)) => Seen::File {
                    hash,
                    mode: entry.mode,
                    size: entry.size,
                    stat: None,
                },
                _ => Seen::Other(
                    entry
                        .reason
                        .clone()
                        .unwrap_or_else(|| format!("{:?} on the machine", entry.kind)),
                ),
            };
            if !record.entries.contains_key(&entry.path) && entry.kind == EntryKind::File {
                new_files.push(entry.path.clone());
            }
            seen.insert(entry.path.clone(), (state, entry.tracked));
        }
    }
    for path in managed {
        seen.entry(path.clone()).or_insert((Seen::Absent, false));
    }
    new_files.sort();
    Ok(Observed {
        seen,
        new_files,
        prefixes,
        block,
        recovered,
        hashed: 0,
    })
}

/// Never a candidate (a path, or a folder with a trailing `/`).
type Skip<'s> = dyn Fn(&str) -> bool + 's;

/// Observes the workspace: `(managed paths, new-file prefixes, never a
/// candidate)`.
type Observe<'o> = dyn FnMut(&[String], &[String], &Skip<'_>) -> Result<Observed, String> + 'o;

fn plan(
    request: &PublishRequest<'_>,
    record: &ManagedRecord,
    observe: &mut Observe<'_>,
    follow_sources: bool,
) -> Result<Plan, String> {
    plan_for(
        request,
        record,
        observe,
        follow_sources,
        &Default::default(),
    )
}

fn plan_for(
    request: &PublishRequest<'_>,
    record: &ManagedRecord,
    observe: &mut Observe<'_>,
    follow_sources: bool,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<Plan, String> {
    let selected = |path: &str| {
        (request.paths.is_empty() && request.sets.is_empty())
            || request
                .paths
                .iter()
                .chain(request.sets)
                .any(|p| under(path, p.trim_end_matches('/')))
    };
    let managed: Vec<String> = record
        .entries
        .keys()
        .filter(|path| selected(path))
        .cloned()
        .collect();
    let new_prefixes: Vec<String> = if request.paths.is_empty() && request.sets.is_empty() {
        record.prefixes.iter().cloned().collect()
    } else {
        request
            .paths
            .iter()
            .chain(request.sets)
            .map(|p| p.trim_end_matches('/').to_string())
            .collect()
    };
    let mut new_prefixes: Vec<String> = new_prefixes
        .into_iter()
        .filter(|p| crate::agents::context::unsafe_destination(p).is_none())
        .collect();
    new_prefixes.sort();
    new_prefixes.dedup();
    let roots = super::step3::labelled_roots(request.home, request.principal, request.target);
    let team_root = roots
        .iter()
        .find(|(label, _)| *label == format!("team:{}/{}", request.target.org, request.target.team))
        .map(|(_, root)| root.clone());
    let excludes = team_root
        .as_ref()
        .map(|root| root.excludes())
        .unwrap_or_else(
            || std::sync::Arc::new(crate::content_engine::TeamExcludes::default_only()),
        );
    let skip = |path: &str| {
        if let Some(dir) = path.strip_suffix('/') {
            // A folder: skipped when everything in it would be excluded.
            return crate::content_engine::is_projection_excluded(
                &format!("{dir}/.codesense-probe"),
                &excludes,
            );
        }
        unpublishable(path, &excludes).is_some()
    };
    let observed = observe(&managed, &new_prefixes, &skip)?;
    let mut out = Plan {
        planned: Vec::new(),
        conflicts: Vec::new(),
        skipped: Vec::new(),
        unchanged: 0,
        forget: Vec::new(),
        rebase: Vec::new(),
        prefixes: observed.prefixes.clone(),
        remote_block: observed.block.clone(),
        withheld: Vec::new(),
        recovered: observed.recovered.clone(),
        hashed: observed.hashed,
        held: Vec::new(),
        hashes: observed
            .seen
            .iter()
            .filter_map(|(path, (seen, _))| match seen {
                Seen::File { hash, .. } => Some((path.clone(), *hash)),
                _ => None,
            })
            .collect(),
        stats: observed
            .seen
            .iter()
            .filter_map(|(path, (seen, _))| match seen {
                Seen::Unhashed {
                    size,
                    stat: Some(stat),
                    ..
                } => Some((path.clone(), (*size, stat.mtime_ns))),
                _ => None,
            })
            .collect(),
    };
    let too_large = |size: u64| {
        format!(
            "it is {} MiB; a publish moves files up to {} MiB (copy it by hand)",
            size.div_ceil(1024 * 1024),
            MAX_PUBLISH_BYTES / (1024 * 1024)
        )
    };
    let conflict = |out: &mut Plan, path: &str, reason: String, layer: &str, target: &Path| {
        out.conflicts.push(PublishConflict {
            path: path.to_string(),
            reason,
            target_layer: layer.to_string(),
            target_path: target.to_path_buf(),
        });
    };
    let mut source_cache = HashMap::new();
    for path in &managed {
        let entry = &record.entries[path];
        let (seen, tracked) = observed
            .seen
            .get(path)
            .cloned()
            .unwrap_or((Seen::Absent, false));
        if tracked {
            out.skipped.push((
                path.clone(),
                "tracked by the repo; publish it with git".into(),
            ));
            continue;
        }
        if let Some(why) = unpublishable(path, &excludes) {
            out.skipped.push((path.clone(), why));
            continue;
        }
        if entry.source_layer == super::want::GENERATED_LAYER {
            // A launch's merged file: generated, never team content.
            out.skipped.push((
                path.clone(),
                "generated by CodeSense at launch; edit its sources instead".into(),
            ));
            continue;
        }
        let original = match source_cache
            .entry(entry.source_context.clone())
            .or_insert_with(|| recorded_roots(request, entry))
            .as_ref()
        {
            Ok(roots) => roots,
            Err(why) if follow_sources => return Err(why.clone()),
            Err(why) => {
                out.skipped.push((path.clone(), why.clone()));
                continue;
            }
        };
        let file_roots = if follow_sources { original } else { &roots };
        let (layer, target) = match managed_target(file_roots, original, path, entry) {
            Ok(found) => found,
            Err(why) => {
                out.skipped.push((path.clone(), why));
                continue;
            }
        };
        if !scope.allows(&target) {
            continue;
        }
        if !contained(&target, file_roots) {
            out.skipped.push((
                path.clone(),
                "its team file is outside this workspace's team content".into(),
            ));
            continue;
        }
        let team = match team_state(
            &target,
            (entry.source.as_deref() == Some(target.as_path()))
                .then_some((entry, record.saved_at_ns)),
        ) {
            Ok(Some((_, size))) if size > MAX_PUBLISH_BYTES => {
                // SW-XS-27: an edit (or removal) here is still an edit, even
                // though a publish can't move it.
                let edited = match &seen {
                    Seen::File { hash, .. } => *hash != entry.hash.0,
                    Seen::Absent => true,
                    Seen::Other(_) | Seen::Unhashed { .. } => false,
                };
                if edited {
                    out.held.push(Held {
                        path: path.clone(),
                        layer: layer.clone(),
                        target: target.clone(),
                        why: too_large(size),
                        new: false,
                        discardable: false,
                    });
                }
                out.skipped.push((path.clone(), too_large(size)));
                continue;
            }
            Ok(team) => team.map(|(hash, _)| hash),
            Err(why) => {
                conflict(&mut out, path, why, &layer, &target);
                continue;
            }
        };
        let expect = request.expect.get(path).cloned().unwrap_or_default();
        match seen {
            Seen::File { hash, .. } if hash == entry.hash.0 && expect.workspace.is_none() => {
                out.unchanged += 1;
            }
            Seen::File { size, .. } if size > MAX_PUBLISH_BYTES => {
                out.held.push(Held {
                    path: path.clone(),
                    layer: layer.clone(),
                    target: target.clone(),
                    why: too_large(size),
                    new: false,
                    discardable: true,
                });
                out.skipped.push((path.clone(), too_large(size)));
            }
            Seen::File {
                hash,
                mode,
                size,
                stat,
            } => {
                if expect.workspace.is_some_and(|want| want != hash) {
                    conflict(
                        &mut out,
                        path,
                        "the workspace file changed since the preview".into(),
                        &layer,
                        &target,
                    );
                } else if expect.source.is_some_and(|want| Some(want) != team) {
                    conflict(
                        &mut out,
                        path,
                        "the team file changed since the preview".into(),
                        &layer,
                        &target,
                    );
                } else if hash == entry.hash.0 {
                    out.unchanged += 1;
                } else if team == Some(hash) {
                    // Already in team content (published elsewhere).
                    out.rebase.push((path.clone(), hash, stat));
                    out.unchanged += 1;
                } else if team != Some(entry.hash.0) {
                    conflict(
                        &mut out,
                        path,
                        match team {
                            Some(team) => format!(
                                "the team file changed since it was copied here (base {}, team {})",
                                &hex(&entry.hash.0)[..12],
                                &hex(&team)[..12]
                            ),
                            None => "the team file was removed since it was copied here".into(),
                        },
                        &layer,
                        &target,
                    );
                } else {
                    out.planned.push(Planned {
                        entry: PublishEntry {
                            path: path.clone(),
                            action: PublishAction::Update,
                            target_layer: layer,
                            target_path: target,
                            bytes: size,
                        },
                        team_expected: team,
                        workspace_hash: Some(hash),
                        mode,
                        stat,
                        managed: Some(entry.clone()),
                    });
                }
            }
            Seen::Absent => match team {
                None => out.forget.push(path.clone()),
                Some(team) if team == entry.hash.0 && !request.include_deletes => {
                    out.withheld.push((path.clone(), layer, target))
                }
                Some(team) if team == entry.hash.0 => out.planned.push(Planned {
                    entry: PublishEntry {
                        path: path.clone(),
                        action: PublishAction::Remove,
                        target_layer: layer,
                        target_path: target,
                        bytes: 0,
                    },
                    team_expected: Some(team),
                    workspace_hash: None,
                    mode: entry.mode,
                    stat: None,
                    managed: Some(entry.clone()),
                }),
                Some(_) => conflict(
                    &mut out,
                    path,
                    "removed here, but the team file changed since it was copied".into(),
                    &layer,
                    &target,
                ),
            },
            Seen::Other(why) => out.skipped.push((path.clone(), why)),
            // Only new files are ever left unread.
            Seen::Unhashed { .. } => out.skipped.push((path.clone(), "it was not read".into())),
        }
    }
    for path in &observed.new_files {
        if !selected(path) {
            continue;
        }
        let (seen, tracked) = observed.seen[path].clone();
        if tracked {
            // The repo's own files are never team content.
            continue;
        }
        if let Some(why) = unpublishable(path, &excludes) {
            out.skipped.push((path.clone(), why));
            continue;
        }
        let (known, mode, size, stat) = match seen {
            Seen::File {
                hash,
                mode,
                size,
                stat,
            } => (Some(hash), mode, size, stat),
            Seen::Unhashed { mode, size, stat } => (None, mode, size, stat),
            Seen::Other(why) => {
                out.skipped.push((path.clone(), why));
                continue;
            }
            Seen::Absent => continue,
        };
        let (layer, root) = match new_file_root(&roots, request.target, request.to) {
            Ok(found) => found.clone(),
            Err(why) => {
                out.skipped.push((path.clone(), why));
                continue;
            }
        };
        let expect = request.expect.get(path).cloned().unwrap_or_default();
        let relative = expect.target.as_deref().unwrap_or(path);
        if crate::agents::context::unsafe_destination(relative).is_some() {
            out.skipped.push((
                path.clone(),
                format!("{relative:?} is not a team-content path"),
            ));
            continue;
        }
        let Some(target) = root.file_path(relative) else {
            continue;
        };
        if !scope.allows(&target) {
            continue;
        }
        if !contained(&target, &roots) {
            out.skipped.push((
                path.clone(),
                "its team file would be outside this workspace's team content".into(),
            ));
            continue;
        }
        // SW-XS-27: a new file listed unread is pinned by its stat (before the
        // size check: a too-large new file is pinned the same way).
        if let Some(want) = expect.stat {
            if stat.map(|stat| (size, stat.mtime_ns)) != Some(want) {
                conflict(
                    &mut out,
                    path,
                    "the workspace file changed since the preview".into(),
                    &layer,
                    &target,
                );
                continue;
            }
        }
        if size > MAX_PUBLISH_BYTES {
            out.held.push(Held {
                path: path.clone(),
                layer: layer.clone(),
                target: target.clone(),
                why: too_large(size),
                new: true,
                discardable: true,
            });
            if let Some(hash) = known {
                out.hashes.insert(path.clone(), hash);
            }
            out.skipped.push((path.clone(), too_large(size)));
            continue;
        }
        let team_now = team_hash(&target);
        // SW-XS-27: a count left a new file unread; read it only when its bytes
        // decide the answer (a team file at its path, or an expected hash).
        let hash = match known {
            Some(hash) => Some(hash),
            None if expect.workspace.is_some() || matches!(team_now, Ok(Some(_))) => {
                match local_hash(request, path) {
                    Ok(hash) => {
                        out.hashed += 1;
                        Some(hash)
                    }
                    Err(why) => {
                        out.skipped.push((path.clone(), why));
                        continue;
                    }
                }
            }
            None => None,
        };
        if let Some(hash) = hash {
            out.hashes.insert(path.clone(), hash);
        }
        match team_now {
            Err(why) => conflict(&mut out, path, why, &layer, &target),
            Ok(team) if expect.source.is_some() && team != expect.source => conflict(
                &mut out,
                path,
                "the team file changed since the preview".into(),
                &layer,
                &target,
            ),
            Ok(_) if expect.workspace.is_some_and(|want| Some(want) != hash) => conflict(
                &mut out,
                path,
                "the workspace file changed since the preview".into(),
                &layer,
                &target,
            ),
            Ok(Some(team)) if Some(team) == hash => {
                out.rebase.push((path.clone(), team, stat));
                out.unchanged += 1;
            }
            // The caller names the team version it started from: an update.
            Ok(Some(team)) if expect.source == Some(team) => out.planned.push(Planned {
                entry: PublishEntry {
                    path: path.clone(),
                    action: PublishAction::Update,
                    target_layer: layer,
                    target_path: target,
                    bytes: size,
                },
                team_expected: Some(team),
                workspace_hash: hash,
                mode,
                stat,
                managed: None,
            }),
            Ok(Some(_)) => conflict(
                &mut out,
                path,
                "team content already has a different file there, and this one was never copied from it"
                    .into(),
                &layer,
                &target,
            ),
            Ok(None) => out.planned.push(Planned {
                entry: PublishEntry {
                    path: path.clone(),
                    action: PublishAction::Add,
                    target_layer: layer,
                    target_path: target,
                    bytes: size,
                },
                team_expected: None,
                workspace_hash: hash,
                mode,
                stat,
                managed: None,
            }),
        }
    }
    out.planned.sort_by(|a, b| a.entry.path.cmp(&b.entry.path));
    Ok(out)
}

/// The hash of a workspace file on this Mac (a count reading a new file late).
fn local_hash(request: &PublishRequest<'_>, path: &str) -> Result<[u8; 32], String> {
    let Destination::Local { root } = request.destination else {
        return Err("only a local new file is ever left unread".into());
    };
    let root = root
        .canonicalize()
        .map_err(|e| format!("workspace {} is unavailable: {e}", root.display()))?;
    crate::content_engine::atomic_io::hash_file(&root.join(path)).map_err(|e| e.to_string())
}

/// Seed an unseeded record from the legacy lockfile, as Step 2 does (§11.2).
/// Returns whether it changed. A busy store leaves it unseeded (retried).
fn seed_if_needed(request: &PublishRequest<'_>, record: &mut ManagedRecord) -> bool {
    if record.seeded {
        return false;
    }
    let Some(legacy) = request.legacy else {
        return false;
    };
    match legacy() {
        Ok(rows) => {
            record.seed(rows);
            true
        }
        Err(error) => {
            log::warn!(target: "agents", "the legacy lockfile could not be read to seed: {error}");
            false
        }
    }
}

pub(super) fn identity_of(destination: Destination<'_>) -> DestinationIdentity {
    match destination {
        Destination::Local { root } => {
            crate::content_engine::context_lifecycle::local_identity(root)
        }
        Destination::Remote { io } => io.identity().clone(),
    }
}

fn emit(request: &PublishRequest<'_>, phase: PreparePhase, files: u64, bytes: u64, total: u64) {
    (request.progress)(&PrepareProgress {
        workspace: request.workspace.clone(),
        agent: request.agent.clone(),
        phase,
        files,
        bytes,
        total,
        note: None,
        summary: None,
        operation_id: Some(request.operation.to_string()),
        operation_kind: None,
    });
}

fn observe_for<'r>(
    request: &'r PublishRequest<'r>,
    record: &'r ManagedRecord,
) -> impl FnMut(&[String], &[String], &Skip<'_>) -> Result<Observed, String> + 'r {
    move |managed, new_prefixes, skip| match request.destination {
        Destination::Local { root } => {
            let root = root
                .canonicalize()
                .map_err(|e| format!("workspace {} is unavailable: {e}", root.display()))?;
            observe_local(&root, record, managed, new_prefixes, skip, false)
        }
        Destination::Remote { io } => {
            io.validate_authority()?;
            observe_remote(io, record, managed, new_prefixes, &mut |progress| {
                emit(
                    request,
                    PreparePhase::Diff,
                    progress.scanned,
                    progress.hashed_bytes,
                    0,
                )
            })
        }
    }
}

/// What a publish would do: ONE manifest call on a machine, a local walk
/// here. Writes nothing of its own. On a machine, though, the manifest call
/// first settles an Apply a previous explicit call left pending
/// (`reconcile_pending` → `recover`), so a remote preview (an MCP call, X1 (c))
/// can complete that earlier write. The app's preview command is local only.
pub fn preview(request: &PublishRequest<'_>) -> Result<PublishReport, String> {
    preview_for(request, &Default::default())
}

pub(crate) fn preview_for(
    request: &PublishRequest<'_>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<PublishReport, String> {
    let started = std::time::Instant::now();
    let identity = identity_of(request.destination);
    let mut record = ManagedRecord::load(request.home, request.principal, &identity);
    // In memory only: a preview writes nothing.
    seed_if_needed(request, &mut record);
    emit(
        request,
        PreparePhase::Diff,
        0,
        0,
        record.entries.len() as u64,
    );
    let plan = plan_for(
        request,
        &record,
        &mut observe_for(request, &record),
        false,
        scope,
    )?;
    Ok(PublishReport {
        status: PublishStatus::Preview,
        entries: plan.planned.into_iter().map(|p| p.entry).collect(),
        conflicts: plan.conflicts,
        skipped: plan.skipped,
        failed: Vec::new(),
        unchanged: plan.unchanged,
        withheld_removals: plan.withheld.into_iter().map(|(path, _, _)| path).collect(),
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// SW-XS-27: what [`discard_new_files`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscardedNew {
    /// Deleted from the workspace.
    pub removed: Vec<String>,
    /// `[path, why]`: still there (it changed since it was looked at, or the
    /// machine refused).
    pub failed: Vec<(String, String)>,
    /// Asked for, but not a new file a publish would add (copied, tracked,
    /// excluded, outside the workspace's team folders, or gone): the call is
    /// refused before anything is deleted, so this is empty on success.
    pub not_new: Vec<String>,
}

/// The new files among `request.paths` (each with the bytes the plan saw),
/// and the paths that are not new files under the workspace's team folders.
/// What a Discard of new files would do with `request.paths`.
struct NewSelection {
    /// The new files to delete, each with the bytes the plan saw (`None`:
    /// not read — only on a read-only check).
    doomed: Vec<(String, Option<[u8; 32]>)>,
    /// Asked for with an expected hash the file no longer holds.
    changed: Vec<String>,
    /// Not a new file under the workspace's team folders.
    not_new: Vec<String>,
}

fn new_files_in(request: &PublishRequest<'_>, record: &ManagedRecord, plan: &Plan) -> NewSelection {
    let in_folders = |path: &str| record.prefixes.iter().any(|prefix| under(path, prefix));
    let mut doomed: Vec<(String, Option<[u8; 32]>)> = plan
        .planned
        .iter()
        // Exactly the named files: never a file found under a named folder.
        .filter(|p| request.paths.contains(&p.entry.path))
        .filter(|p| p.entry.action == PublishAction::Add && p.managed.is_none())
        // Only new files under the workspace's team folders: a publish of a
        // named path looks anywhere, a discard never does.
        .filter(|p| in_folders(&p.entry.path))
        .map(|p| (p.entry.path.clone(), p.workspace_hash))
        .collect();
    // A new file too large to publish can still be thrown away.
    doomed.extend(
        plan.held
            .iter()
            .filter(|h| h.new && request.paths.contains(&h.path) && in_folders(&h.path))
            .map(|h| (h.path.clone(), plan.hashes.get(&h.path).copied())),
    );
    let changed: Vec<String> = plan
        .conflicts
        .iter()
        .filter(|c| request.paths.contains(&c.path) && in_folders(&c.path))
        .filter(|c| {
            request
                .expect
                .get(&c.path)
                .is_some_and(|e| e.workspace.is_some() || e.stat.is_some())
        })
        .map(|c| c.path.clone())
        .collect();
    let not_new = request
        .paths
        .iter()
        .filter(|path| !doomed.iter().any(|(p, _)| p == *path) && !changed.contains(path))
        .cloned()
        .collect();
    NewSelection {
        doomed,
        changed,
        not_new,
    }
}

fn not_new_error(paths: &[String]) -> String {
    format!(
        "{} is not a file CodeSense copied here or a new file under the workspace's team folders",
        paths.join(", ")
    )
}

/// SW-XS-27: refuse a Discard whose new-file paths are not all new files under
/// the workspace's team folders — checked read-only BEFORE any other part of
/// the discard writes, so a refused call changes nothing.
pub fn check_new_files(request: &PublishRequest<'_>) -> Result<(), String> {
    let identity = identity_of(request.destination);
    let mut record = ManagedRecord::read(request.home, request.principal, &identity)?;
    if !record.seeded {
        if let Some(legacy) = request.legacy {
            record.seed(legacy()?);
        }
    }
    let plan = plan(
        request,
        &record,
        &mut observe_read_only(request, &record),
        false,
    )?;
    let selection = new_files_in(request, &record, &plan);
    if selection.not_new.is_empty() {
        Ok(())
    } else {
        Err(not_new_error(&selection.not_new))
    }
}

/// SW-XS-27 "Discard" for NEW files (never copied from the team): exactly the
/// files a publish of `request.paths` would add, deleted from the workspace
/// instead — each only while it still holds the bytes the plan saw (here:
/// re-hashed just before; on a machine: one batch whose removals are
/// compare-and-swapped). Copied files are not touched here (their Discard
/// puts the team's version back), and team content is never written. An
/// explicit operation: under the destination's step lock, like a publish.
pub fn discard_new_files(request: &PublishRequest<'_>) -> Result<DiscardedNew, String> {
    if request.paths.is_empty() {
        return Err("name the new files to discard".into());
    }
    let identity = identity_of(request.destination);
    let lock = super::step_lock(&identity);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    let _lease = crate::content_engine::context_lifecycle::reserve_selected_within(
        request.home,
        &identity,
        super::ADMISSION_WAIT,
    )?;
    let mut record = ManagedRecord::load(request.home, request.principal, &identity);
    let mut record_changed = seed_if_needed(request, &mut record);
    if let Destination::Remote { io } = request.destination {
        let recovered = io.reconcile_pending().map_err(|e| e.to_string())?;
        record_changed |= recovered.is_some();
        super::remote::settle_record(&mut record, recovered);
    }
    let mut plan = plan(request, &record, &mut observe_for(request, &record), false)?;
    if plan.recovered.is_some() {
        record_changed = true;
        super::remote::settle_record(&mut record, plan.recovered.take());
    }
    let selection = new_files_in(request, &record, &plan);
    let mut out = DiscardedNew {
        not_new: selection.not_new,
        failed: selection
            .changed
            .into_iter()
            .map(|path| (path, "it changed since it was looked at".to_string()))
            .collect(),
        ..DiscardedNew::default()
    };
    // All or nothing: a path that is not a new file refuses the whole call
    // before anything is deleted.
    if !out.not_new.is_empty() {
        return Err(not_new_error(&out.not_new));
    }
    // Every file here was read (`observe_for`): its bytes are what a delete
    // compares against. One without (it couldn't be read) is reported.
    let mut doomed: Vec<(String, [u8; 32])> = Vec::new();
    for (path, hash) in selection.doomed {
        match hash {
            Some(hash) => doomed.push((path, hash)),
            None => out
                .failed
                .push((path, "it couldn't be read to check it".into())),
        }
    }
    (request.validate)()?;
    if request.cancel.is_some_and(|cancel| cancel()) {
        return Err("cancelled before anything was deleted".into());
    }
    match request.destination {
        Destination::Local { root } => {
            let root = root
                .canonicalize()
                .map_err(|e| format!("workspace {} is unavailable: {e}", root.display()))?;
            for (path, hash) in doomed {
                let full = root.join(&path);
                match crate::content_engine::atomic_io::hash_file(&full) {
                    Ok(now) if now == hash => match std::fs::remove_file(&full) {
                        Ok(()) => out.removed.push(path),
                        Err(error) => out.failed.push((path, error.to_string())),
                    },
                    Ok(_) => out
                        .failed
                        .push((path, "it changed since it was looked at".into())),
                    Err(error) => out.failed.push((path, error.to_string())),
                }
            }
        }
        Destination::Remote { io } => {
            if !doomed.is_empty() {
                let mut removes = Vec::new();
                for (path, hash) in &doomed {
                    removes.push(crate::content_engine::destination::BatchRemove {
                        operation_id: uuid::Uuid::new_v4(),
                        path:
                            crate::content_engine::destination::DestinationRelativePath::try_from(
                                path.clone(),
                            )?,
                        expected_pre_hash: *hash,
                    });
                }
                let ids: Vec<(String, String)> = removes
                    .iter()
                    .map(|r| (r.operation_id.to_string(), r.path.as_str().to_string()))
                    .collect();
                let outcome = io
                    .apply_batch(
                        &DestinationBatch {
                            batch_id: uuid::Uuid::new_v4(),
                            writes: Vec::new(),
                            removes,
                            chmods: Vec::new(),
                            ensure_dirs: Vec::new(),
                            exclude: None,
                        },
                        &mut |_, _| true,
                    )
                    .map_err(|e| e.to_string())?;
                for (id, path) in ids {
                    let result = outcome.operations.iter().find(|o| o.operation_id == id);
                    match result.map(|o| &o.outcome) {
                        Some(codesense_pty_core::context_batch::MemberOutcome::Effect {
                            outcome: codesense_pty_core::remote_files::EffectOutcome::Applied { .. },
                        }) => out.removed.push(path),
                        Some(codesense_pty_core::context_batch::MemberOutcome::Effect {
                            outcome:
                                codesense_pty_core::remote_files::EffectOutcome::Conflict { .. },
                        }) => out.failed.push((
                            path,
                            "it changed on the machine since it was looked at".into(),
                        )),
                        _ => out
                            .failed
                            .push((path, "the machine didn't confirm it".into())),
                    }
                }
                if outcome.recovered.is_some() {
                    record_changed = true;
                    super::remote::settle_record(&mut record, outcome.recovered);
                }
            }
        }
    }
    if record_changed {
        record.save(request.home, request.principal, &identity)?;
    }
    Ok(out)
}

/// SW-XS-27: what a workspace holds that team content doesn't have yet —
/// the plan a publish would make, observed without writing anything. The
/// caller reads (never loads) the managed record and seeds it in memory.
/// On a machine it is ONE manifest call that never settles an earlier cut
/// batch ([`DestinationIo::manifest_observe`]).

pub(crate) fn observe_edits_for(
    request: &PublishRequest<'_>,
    record: &ManagedRecord,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<Edits, String> {
    let plan = plan_for(
        request,
        record,
        &mut observe_read_only(request, record),
        true,
        scope,
    )?;
    let mut edits = Edits {
        conflicts: plan.conflicts,
        // Without `includeDeletes` every removal is withheld, never planned.
        removed: plan.withheld,
        held: plan.held,
        hashed: plan.hashed,
        hashes: plan.hashes,
        stats: plan.stats,
        ..Edits::default()
    };
    for planned in plan.planned {
        if let Some(team) = planned.team_expected {
            edits.team_hashes.insert(planned.entry.path.clone(), team);
        }
        match planned.entry.action {
            PublishAction::Add => edits.added.push(planned.entry),
            PublishAction::Update => edits.changed.push(planned.entry),
            PublishAction::Remove => edits.removed.push((
                planned.entry.path,
                planned.entry.target_layer,
                planned.entry.target_path,
            )),
        }
    }
    Ok(edits)
}

/// SW-XS-27: an edit a publish can't move.
#[derive(Debug, Clone)]
pub(crate) struct Held {
    pub path: String,
    /// The tier it would go to.
    pub layer: String,
    /// The team file it would go to.
    pub target: PathBuf,
    pub why: String,
    /// A new file (never copied here): a Discard deletes it.
    pub new: bool,
    /// Discard can put the team's version back or delete it (not when the
    /// team's own file is the one too large to move).
    pub discardable: bool,
}

/// The edits [`observe_edits`] found.
#[derive(Debug, Default)]
pub(crate) struct Edits {
    /// Copies edited here (the team still holds what was copied).
    pub changed: Vec<PublishEntry>,
    /// New files under the workspace's team folders.
    pub added: Vec<PublishEntry>,
    /// Copies removed here: `(path, tier, team file)`.
    pub removed: Vec<(String, String, PathBuf)>,
    /// Changed here and in the team (or the team file can't be read).
    pub conflicts: Vec<PublishConflict>,
    /// Edits a publish can't move.
    pub held: Vec<Held>,
    /// The workspace bytes' hash of every file read.
    pub hashes: HashMap<String, [u8; 32]>,
    /// The team file's hash a planned change expects (none: absent).
    pub team_hashes: HashMap<String, [u8; 32]>,
    /// `(size, mtime_ns)` of every new local file not read.
    pub stats: HashMap<String, (u64, i64)>,
    /// Workspace files read to hash them (local).
    pub hashed: usize,
}

fn observe_read_only<'r>(
    request: &'r PublishRequest<'r>,
    record: &'r ManagedRecord,
) -> impl FnMut(&[String], &[String], &Skip<'_>) -> Result<Observed, String> + 'r {
    move |managed, new_prefixes, skip| match request.destination {
        Destination::Local { root } => {
            let root = root
                .canonicalize()
                .map_err(|e| format!("workspace {} is unavailable: {e}", root.display()))?;
            observe_local(&root, record, managed, new_prefixes, skip, true)
        }
        Destination::Remote { io } => {
            io.validate_authority()?;
            observe_remote_read_only(io, record, managed, new_prefixes)
        }
    }
}

/// Publish: the plan, the changed files' bytes (ONE more call on a
/// machine), then one transactional write into team content and the
/// managed record.
pub fn publish(request: &PublishRequest<'_>) -> Result<PublishReport, String> {
    let started = std::time::Instant::now();
    let identity = identity_of(request.destination);
    // The same per-destination lock Step 2 and Step 3 hydrates hold.
    let lock = super::step_lock(&identity);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    // Drain any in-process legacy writer on this destination, as Step 2 does.
    let _lease = crate::content_engine::context_lifecycle::reserve_selected_within(
        request.home,
        &identity,
        super::ADMISSION_WAIT,
    )?;
    let mut record = ManagedRecord::load(request.home, request.principal, &identity);
    // Review r1: what a cut batch applied is settled into the record (under
    // the held lock) before anything is planned against it.
    let mut record_changed = seed_if_needed(request, &mut record);
    if let Destination::Remote { io } = request.destination {
        let recovered = io.reconcile_pending().map_err(|e| e.to_string())?;
        record_changed |= recovered.is_some();
        super::remote::settle_record(&mut record, recovered);
    }
    emit(
        request,
        PreparePhase::Diff,
        0,
        0,
        record.entries.len() as u64,
    );
    let mut plan = plan(request, &record, &mut observe_for(request, &record), false)?;
    if plan.recovered.is_some() {
        record_changed = true;
        super::remote::settle_record(&mut record, plan.recovered.take());
    }
    let cancelled = || request.cancel.is_some_and(|cancel| cancel());
    let mut report = PublishReport {
        status: PublishStatus::Published,
        entries: Vec::new(),
        conflicts: plan.conflicts.clone(),
        skipped: plan.skipped.clone(),
        failed: Vec::new(),
        unchanged: plan.unchanged,
        withheld_removals: plan
            .withheld
            .iter()
            .map(|(path, _, _)| path.clone())
            .collect(),
        elapsed_ms: 0,
    };
    // Re-base what team content already holds, forget what is gone on both
    // sides: no write needed.
    record_changed |= !plan.rebase.is_empty() || !plan.forget.is_empty();
    for path in &plan.forget {
        record.entries.remove(path);
    }
    let new_managed: Vec<String> = plan
        .rebase
        .iter()
        .filter(|(path, _, _)| !record.entries.contains_key(path))
        .map(|(path, _, _)| path.clone())
        .collect();
    let roots = super::step3::labelled_roots(request.home, request.principal, request.target);
    let mut source_cache = HashMap::new();
    for (path, hash, stat) in &plan.rebase {
        let existing = record.entries.get(path).cloned();
        let (layer, source) = match &existing {
            Some(entry) => source_cache
                .entry(entry.source_context.clone())
                .or_insert_with(|| recorded_roots(request, entry))
                .as_ref()
                .ok()
                .and_then(|original| managed_target(&roots, original, path, entry).ok())
                .unzip(),
            None => new_file_root(&roots, request.target, request.to)
                .ok()
                .and_then(|(label, root)| root.file_path(path).map(|path| (label.clone(), path)))
                .unzip(),
        };
        record.entries.insert(
            path.clone(),
            ManagedEntry {
                source_context: request
                    .target
                    .source_context(layer.as_deref().unwrap_or("")),
                hash: Hash32(*hash),
                // Adopted, not written: team content already held these
                // bytes, so an unroute must never delete that team file
                // (only `step3:route:` entries are deletable, review r1).
                origin: existing
                    .as_ref()
                    .map(|e| e.origin.clone())
                    .unwrap_or_else(|| {
                        format!(
                            "step3:adopt:{}",
                            request
                                .operation
                                .strip_prefix("route:")
                                .unwrap_or(request.operation)
                        )
                    }),
                source_layer: layer.unwrap_or_default(),
                mode: existing.as_ref().map(|e| e.mode).unwrap_or(0o644),
                dest_stat: *stat,
                source_stat: None,
                source,
            },
        );
    }
    if plan.planned.is_empty() {
        // Review r1: files adopted as managed (team content already holds
        // their bytes) are hidden from git even when nothing is written.
        if !new_managed.is_empty() {
            if let Err(error) = exclude_added(request, &identity, &mut record, &plan, &new_managed)
            {
                report.failed.push((".git/info/exclude".into(), error));
            }
        }
        if record_changed {
            record.save(request.home, request.principal, &identity)?;
        }
        report.status = if !report.failed.is_empty() || !report.conflicts.is_empty() {
            PublishStatus::Failed
        } else {
            PublishStatus::NothingToPublish
        };
        report.elapsed_ms = started.elapsed().as_millis() as u64;
        return Ok(report);
    }
    // The changed files' bytes.
    let total: u64 = plan.planned.iter().map(|p| p.entry.bytes).sum();
    emit(request, PreparePhase::Transfer, 0, 0, total);
    let reads: Vec<String> = plan
        .planned
        .iter()
        .filter(|p| p.workspace_hash.is_some())
        .map(|p| p.entry.path.clone())
        .collect();
    let mut bytes: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    match request.destination {
        Destination::Local { root } => {
            let root = root.canonicalize().map_err(|e| e.to_string())?;
            for path in &reads {
                if cancelled() {
                    break;
                }
                match std::fs::read(root.join(path)) {
                    Ok(read) => {
                        bytes.insert(path.clone(), read);
                    }
                    Err(error) => report.failed.push((path.clone(), error.to_string())),
                }
            }
        }
        Destination::Remote { io } => {
            if !reads.is_empty() && !cancelled() {
                io.validate_authority()?;
                // Only the folders holding the changed files are walked again.
                let read_prefixes = cover(&reads).unwrap_or_else(|_| plan.prefixes.clone());
                let (again, read) = io
                    .manifest_reading(&read_prefixes, MAX_ENTRIES, &reads, &mut |progress| {
                        emit(
                            request,
                            PreparePhase::Transfer,
                            progress.scanned,
                            progress.hashed_bytes,
                            total,
                        )
                    })
                    .map_err(|e| e.to_string())?;
                if again.recovered.is_some() {
                    record_changed = true;
                    super::remote::settle_record(&mut record, again.recovered);
                }
                for (path, why) in read.unread {
                    report.failed.push((path, why));
                }
                // Only bytes this publish asked for (review r1 M1).
                let asked: std::collections::HashSet<&String> = reads.iter().collect();
                bytes = read
                    .files
                    .into_iter()
                    .filter(|(path, _)| asked.contains(path))
                    .collect();
            }
        }
    }
    if cancelled() {
        if record_changed {
            record.save(request.home, request.principal, &identity)?;
        }
        report.status = PublishStatus::Cancelled;
        report.elapsed_ms = started.elapsed().as_millis() as u64;
        return Ok(report);
    }
    // Only what was read back with the planned hash is written.
    let mut writable = Vec::new();
    for planned in &plan.planned {
        match planned.workspace_hash {
            None => writable.push(planned),
            Some(hash) => match bytes.get(&planned.entry.path) {
                Some(read) if *blake3::hash(read).as_bytes() == hash => writable.push(planned),
                Some(_) => report.conflicts.push(PublishConflict {
                    path: planned.entry.path.clone(),
                    reason: "the workspace file changed while it was being published".into(),
                    target_layer: planned.entry.target_layer.clone(),
                    target_path: planned.entry.target_path.clone(),
                }),
                None => {
                    if !report.failed.iter().any(|(p, _)| *p == planned.entry.path) {
                        report
                            .failed
                            .push((planned.entry.path.clone(), "it could not be read".into()));
                    }
                }
            },
        }
    }
    emit(request, PreparePhase::Apply, 0, 0, writable.len() as u64);
    (request.validate)()?;
    match write_transaction(&writable, &bytes, &roots) {
        Ok(stats) => {
            for (planned, source_stat) in writable.iter().zip(stats) {
                let path = &planned.entry.path;
                match planned.entry.action {
                    PublishAction::Remove => {
                        record.entries.remove(path);
                    }
                    PublishAction::Add | PublishAction::Update => {
                        let previous = planned.managed.clone();
                        record.entries.insert(
                            path.clone(),
                            ManagedEntry {
                                source_context: request
                                    .target
                                    .source_context(&planned.entry.target_layer),
                                hash: Hash32(planned.workspace_hash.expect("a write has a hash")),
                                origin: previous
                                    .as_ref()
                                    .map(|e| e.origin.clone())
                                    .unwrap_or_else(|| format!("step3:{}", request.operation)),
                                source_layer: planned.entry.target_layer.clone(),
                                mode: planned.mode,
                                dest_stat: planned.stat,
                                source_stat,
                                source: Some(planned.entry.target_path.clone()),
                            },
                        );
                    }
                }
                report.entries.push(planned.entry.clone());
            }
            record_changed = true;
        }
        Err(failed) => {
            report.failed.push((failed.path, failed.why));
            report.failed.extend(failed.unrestored);
        }
    }
    // A published new file is managed now: hide it from git like any copy.
    let added: Vec<String> = report
        .entries
        .iter()
        .filter(|e| e.action == PublishAction::Add)
        .map(|e| e.path.clone())
        .chain(new_managed)
        .collect();
    if !added.is_empty() {
        if let Err(error) = exclude_added(request, &identity, &mut record, &plan, &added) {
            report.failed.push((".git/info/exclude".into(), error));
        }
    }
    if record_changed {
        record.save(request.home, request.principal, &identity)?;
    }
    report.status = status_of(&report);
    report.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

/// The final state of a publish that got as far as writing.
fn status_of(report: &PublishReport) -> PublishStatus {
    match (
        report.entries.is_empty(),
        report.conflicts.is_empty(),
        report.failed.is_empty(),
    ) {
        (_, true, true) => PublishStatus::Published,
        // Something landed: say so, with what did not (review r1).
        (false, _, _) => PublishStatus::PublishedWithConflicts,
        _ => PublishStatus::Failed,
    }
}

/// Why [`write_transaction`] wrote nothing (it rolled back), and the targets
/// its rollback could not restore.
#[derive(Debug)]
struct TransactionFailed {
    path: String,
    why: String,
    unrestored: Vec<(String, String)>,
}

/// Write every entry by compare-and-swap (each check and its write share the
/// content mutation lock); a race on any entry rolls back the ones already
/// written — each only while its target still holds what this publish wrote,
/// so a later writer's bytes are never clobbered. Every target must lie
/// lexically inside one of `roots`. Returns each written team file's stat.
fn write_transaction(
    writable: &[&Planned],
    bytes: &BTreeMap<String, Vec<u8>>,
    roots: &[(String, crate::context_repo::inheritance::ContentRoot)],
) -> Result<Vec<Option<StatKey>>, TransactionFailed> {
    use crate::content_engine::atomic_io::{
        remove_expected, write_tmp_fsync_rename_expected, write_tmp_fsync_rename_expected_mode,
    };
    const RACED: &str = "the team file changed while publishing; nothing was published";
    // What each target held before (to restore) and what this publish left
    // there (`None`: removed it).
    type Done<'p> = (&'p Planned, Option<Vec<u8>>, Option<[u8; 32]>);
    let mut done: Vec<Done<'_>> = Vec::new();
    // Returns the targets the rollback could not restore (another writer
    // changed them since): reported, never clobbered.
    let rollback = |done: &[Done<'_>]| -> Vec<(String, String)> {
        let mut left = Vec::new();
        for (planned, original, wrote) in done.iter().rev() {
            let target = &planned.entry.target_path;
            let restored = match (original, wrote) {
                // Restore the old bytes only over exactly what we wrote.
                (Some(original), Some(wrote)) => {
                    write_tmp_fsync_rename_expected(target, original, Some(*wrote))
                }
                // Put a removed file back only where nothing appeared since.
                (Some(original), None) => write_tmp_fsync_rename_expected(target, original, None),
                // Take an added file away only while it holds our bytes.
                (None, Some(wrote)) => remove_expected(target, *wrote),
                (None, None) => Ok(true),
            };
            if !matches!(restored, Ok(true)) {
                log::warn!(
                    target: "agents",
                    "publish rollback left {} as another writer changed it",
                    target.display()
                );
                left.push((
                    planned.entry.path.clone(),
                    format!(
                        "published, then not rolled back: {} changed again since (check it)",
                        target.display()
                    ),
                ));
            }
        }
        left
    };
    let mut stats = Vec::with_capacity(writable.len());
    for planned in writable {
        let target = &planned.entry.target_path;
        if !contained(target, roots) {
            let unrestored = rollback(&done);
            return Err(TransactionFailed {
                path: planned.entry.path.clone(),
                why: "its team file is outside this workspace's team content".into(),
                unrestored,
            });
        }
        let original = match planned.team_expected {
            Some(_) => match std::fs::read(target) {
                Ok(original) => Some(original),
                Err(error) => {
                    let unrestored = rollback(&done);
                    return Err(TransactionFailed {
                        path: planned.entry.path.clone(),
                        why: error.to_string(),
                        unrestored,
                    });
                }
            },
            None => None,
        };
        if original
            .as_ref()
            .map(|o| *blake3::hash(o).as_bytes())
            .ne(&planned.team_expected)
        {
            let unrestored = rollback(&done);
            return Err(TransactionFailed {
                path: planned.entry.path.clone(),
                why: RACED.into(),
                unrestored,
            });
        }
        let result: Result<Option<[u8; 32]>, String> = match planned.entry.action {
            PublishAction::Remove => match planned.team_expected {
                // The hash check and the delete are one step under the lock.
                Some(expected) => match remove_expected(target, expected) {
                    Ok(true) => Ok(None),
                    Ok(false) => Err(RACED.into()),
                    Err(error) => Err(error.to_string()),
                },
                None => Err(RACED.into()),
            },
            PublishAction::Add | PublishAction::Update => {
                let data = &bytes[&planned.entry.path];
                match write_tmp_fsync_rename_expected_mode(
                    target,
                    data,
                    planned.team_expected,
                    Some(planned.mode),
                ) {
                    Ok(true) => Ok(Some(*blake3::hash(data).as_bytes())),
                    Ok(false) => Err(RACED.into()),
                    Err(error) => Err(error.to_string()),
                }
            }
        };
        let wrote = match result {
            Ok(wrote) => wrote,
            Err(why) => {
                let unrestored = rollback(&done);
                return Err(TransactionFailed {
                    path: planned.entry.path.clone(),
                    why,
                    unrestored,
                });
            }
        };
        stats.push(
            std::fs::symlink_metadata(target)
                .ok()
                .map(|meta| StatKey::of(&meta)),
        );
        done.push((planned, original, wrote));
    }
    Ok(stats)
}

/// Add newly managed paths to this workspace's exclude block (X7).
fn exclude_added(
    request: &PublishRequest<'_>,
    identity: &DestinationIdentity,
    record: &mut ManagedRecord,
    plan: &Plan,
    added: &[String],
) -> Result<(), String> {
    let workspace_id = super::workspace_id(identity);
    match request.destination {
        Destination::Local { root } => {
            let root = root.canonicalize().map_err(|e| e.to_string())?;
            if !root.join(".git").exists() {
                return Ok(());
            }
            let actual = super::local::actual_block(&root, &workspace_id)?
                .map_err(|reason| format!("the exclude block was edited by hand: {reason}"))?;
            let mut paths: BTreeSet<String> = actual.iter().cloned().collect();
            paths.extend(added.iter().cloned());
            let paths: Vec<String> = paths.into_iter().collect();
            if paths != actual {
                record.exclude = Some(super::local::write_block(
                    &root,
                    &workspace_id,
                    &actual,
                    &paths,
                )?);
            }
            Ok(())
        }
        Destination::Remote { io } => {
            let expected = plan
                .remote_block
                .clone()
                .or_else(|| record.exclude.clone())
                .unwrap_or_default();
            let mut paths: BTreeSet<String> = expected.iter().cloned().collect();
            paths.extend(added.iter().cloned());
            let paths: Vec<String> = paths.into_iter().collect();
            if paths == expected {
                return Ok(());
            }
            let outcome = io
                .apply_batch(
                    &DestinationBatch {
                        batch_id: uuid::Uuid::new_v4(),
                        writes: Vec::new(),
                        removes: Vec::new(),
                        chmods: Vec::new(),
                        ensure_dirs: Vec::new(),
                        exclude: Some(BatchExclude {
                            operation_id: uuid::Uuid::new_v4(),
                            workspace_id,
                            expected,
                            paths: paths.clone(),
                            repair: false,
                        }),
                    },
                    &mut |_, _| true,
                )
                .map_err(|e| e.to_string())?;
            record.exclude = outcome.exclude_paths.or(Some(paths));
            Ok(())
        }
    }
}

/// What [`route`] did with one workspace file (SLW-WD-3 `route_file_to_context`
/// on the engine, SW-XS-8b).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteResult {
    /// Published into team content as a new file; managed and hidden from git.
    Routed,
    /// Team content already holds it and the workspace copy is managed.
    AlreadyRouted,
    /// Not a regular file in the workspace (a symlink, a folder…).
    Refused,
    /// Team content already holds different bytes there.
    Collision {
        source_hash: [u8; 32],
        dest_hash: [u8; 32],
    },
    /// The repo tracks it: it is code, not context.
    AlreadyTracked,
    /// A path context never carries (VCS data, generated settings, excluded).
    NotProjectable,
}

/// The origin prefix of a file [`route`] put into team content: only such a
/// file's team copy is ever deleted by [`unroute`].
pub const ROUTE_ORIGIN_PREFIX: &str = "step3:route:";

/// Make sure `logical` is in this local workspace's exclude block (a route
/// whose exclude write failed earlier is repaired by routing again).
fn ensure_excluded(
    root: &Path,
    identity: &DestinationIdentity,
    record: &mut ManagedRecord,
    logical: &str,
) -> Result<bool, String> {
    if !root.join(".git").exists() {
        return Ok(false);
    }
    let workspace_id = super::workspace_id(identity);
    let actual = super::local::actual_block(root, &workspace_id)?
        .map_err(|reason| format!("the exclude block was edited by hand: {reason}"))?;
    if actual.iter().any(|path| path == logical) {
        return Ok(false);
    }
    let mut paths: BTreeSet<String> = actual.iter().cloned().collect();
    paths.insert(logical.to_string());
    let paths: Vec<String> = paths.into_iter().collect();
    record.exclude = Some(super::local::write_block(
        root,
        &workspace_id,
        &actual,
        &paths,
    )?);
    Ok(true)
}

pub(crate) fn route_for(
    home: &Path,
    principal: &Principal,
    root: &Path,
    target: &ManifestTarget,
    logical: &str,
    operation: &str,
    legacy: Option<&dyn Fn() -> Result<super::LegacyRows, String>>,
    caller: &crate::context_repo::caller_scope::CallerScope,
) -> Result<RouteResult, String> {
    let label = format!("team:{}/{}", target.org, target.team);
    let readable = matches!(
        caller,
        crate::context_repo::caller_scope::CallerScope::Human
    ) || super::step3::labelled_roots(home, principal, target)
        .into_iter()
        .find(|(name, _)| *name == label)
        .and_then(|(_, root)| root.file_path(logical.trim_start_matches("./")))
        .is_some_and(|path| caller.allows(&path));
    crate::context_repo::caller_scope::readback(
        readable,
        route(home, principal, root, target, logical, operation, legacy),
    )
}

/// Route one local workspace file into the team's content (an explicit
/// publish of that one path to the team tier), then manage it (origin
/// `step3:route:<operation>`) and hide it from git. Nothing is written on any
/// refusal; a file already managed here is already routed (its exclude is
/// repaired if an earlier write of it failed).
pub fn route(
    home: &Path,
    principal: &Principal,
    root: &Path,
    target: &ManifestTarget,
    logical: &str,
    operation: &str,
    legacy: Option<&dyn Fn() -> Result<super::LegacyRows, String>>,
) -> Result<RouteResult, String> {
    let logical = logical.trim_start_matches("./").to_string();
    if crate::content_engine::destination::DestinationRelativePath::try_from(logical.clone())
        .is_err()
        || crate::agents::context::unsafe_destination(&logical).is_some()
    {
        return Ok(RouteResult::NotProjectable);
    }
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    if super::local::Tracked::open(&root)?.contains(&logical) {
        return Ok(RouteResult::AlreadyTracked);
    }
    match std::fs::symlink_metadata(root.join(&logical)) {
        Ok(meta) if meta.file_type().is_file() => {}
        _ => return Ok(RouteResult::Refused),
    }
    let identity = crate::content_engine::context_lifecycle::local_identity(&root);
    {
        let lock = super::step_lock(&identity);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut record = ManagedRecord::load(home, principal, &identity);
        // A pre-upgrade worktree: its old projected files are managed (they
        // came from team content), so routing one is AlreadyRouted.
        if !record.seeded {
            if let Some(legacy) = legacy {
                match legacy() {
                    Ok(rows) => {
                        record.seed(rows);
                        record.save(home, principal, &identity)?;
                    }
                    Err(error) => {
                        log::warn!(target: "agents", "the legacy lockfile could not be read to seed: {error}")
                    }
                }
            }
        }
        if record.entries.contains_key(&logical) {
            if ensure_excluded(&root, &identity, &mut record, &logical)? {
                record.save(home, principal, &identity)?;
            }
            return Ok(RouteResult::AlreadyRouted);
        }
    }
    let paths = vec![logical.clone()];
    let expect = BTreeMap::new();
    let operation = format!("route:{operation}");
    let report = publish(&PublishRequest {
        home,
        principal,
        destination: Destination::Local { root: &root },
        target,
        paths: &paths,
        sets: &[],
        to: Some("team"),
        expect: &expect,
        include_deletes: false,
        legacy: None,
        operation: &operation,
        workspace: root.display().to_string(),
        agent: "route".into(),
        progress: &|_| {},
        validate: &|| Ok(()),
        cancel: None,
    })?;
    if let Some(conflict) = report.conflicts.first() {
        let team = team_hash(&conflict.target_path)?.unwrap_or_default();
        let workspace = crate::content_engine::atomic_io::hash_file(&root.join(&logical))
            .map_err(|e| e.to_string())?;
        return Ok(RouteResult::Collision {
            source_hash: team,
            dest_hash: workspace,
        });
    }
    if let Some((_, why)) = report.skipped.first() {
        log::info!(target: "agents", "route of {logical} skipped: {why}");
        return Ok(RouteResult::NotProjectable);
    }
    if let Some((_, why)) = report.failed.first() {
        return Err(why.clone());
    }
    Ok(if report.entries.is_empty() {
        RouteResult::AlreadyRouted
    } else {
        RouteResult::Routed
    })
}

/// The inverse of [`route`]: the file stops being managed and shows in git
/// again. Its team copy is deleted only when [`route`] put it there (origin
/// `step3:route:`), it lies in the active team's own content, and it still
/// holds what was routed or what the workspace has — a file a hydrate or a
/// launch copied in is only unmanaged, never deleted from the team. Each step
/// is idempotent.
pub fn unroute(
    home: &Path,
    principal: &Principal,
    root: &Path,
    target: &ManifestTarget,
    logical: &str,
) -> Result<(), String> {
    let logical = logical.trim_start_matches("./");
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let identity = crate::content_engine::context_lifecycle::local_identity(&root);
    let lock = super::step_lock(&identity);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut record = ManagedRecord::load(home, principal, &identity);
    let team_root = super::step3::labelled_roots(home, principal, target)
        .into_iter()
        .find(|(label, _)| *label == format!("team:{}/{}", target.org, target.team))
        .map(|(_, root)| root);
    if let Some(entry) = record.entries.get(logical).cloned() {
        let routed_here = entry.origin.starts_with(ROUTE_ORIGIN_PREFIX);
        if let (true, Some(source), Some(team_root)) = (routed_here, &entry.source, &team_root) {
            let roots = vec![(String::new(), team_root.clone())];
            if contained(source, &roots) {
                let workspace =
                    crate::content_engine::atomic_io::hash_file(&root.join(logical)).ok();
                for expected in [Some(entry.hash.0), workspace].into_iter().flatten() {
                    if crate::content_engine::atomic_io::remove_expected(source, expected)
                        .map_err(|e| e.to_string())?
                    {
                        break;
                    }
                }
            }
        }
        record.entries.remove(logical);
        record.save(home, principal, &identity)?;
    }
    if root.join(".git").exists() {
        let workspace_id = super::workspace_id(&identity);
        let actual = super::local::actual_block(&root, &workspace_id)?
            .map_err(|reason| format!("the exclude block was edited by hand: {reason}"))?;
        if actual.iter().any(|path| path == logical) {
            let paths: Vec<String> = actual.iter().filter(|p| *p != logical).cloned().collect();
            record.exclude = Some(super::local::write_block(
                &root,
                &workspace_id,
                &actual,
                &paths,
            )?);
            record.save(home, principal, &identity)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "publish_tests.rs"]
mod tests;
