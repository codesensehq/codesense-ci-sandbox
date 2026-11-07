// SPDX-License-Identifier: MIT
//! SW-XS-27: the team-file edits a workspace holds that are not in team
//! content yet — "unsaved changes" for team files edited inside workspaces.
//!
//! A count is the plan an explicit publish would make
//! ([`super::publish::observe_edits`]), so the count and a later publish never
//! disagree, and the same exclusions apply: CodeSense's own local files
//! (`MANAGED_DESTINATIONS`, `JUNK_GLOBS`), generated launch files and files
//! the repo tracks are never counted.
//!
//! **Counting only reads (X1).** The managed record is read, never loaded
//! (loading moves an unreadable record aside); legacy lockfile rows seed it in
//! memory only; on a machine the count is ONE manifest call that never
//! settles a batch an earlier call left behind — a destination with one
//! pending reports [`EditsState::UnfinishedCopy`] without calling the machine.
//! The unattended-writer guard proves no path from here reaches a write
//! primitive.

use std::path::Path;

use serde::Serialize;

use super::publish::{self, PublishRequest};
use super::record::ManagedRecord;
use super::{Destination, LegacyRows};
use crate::agents::manifest_resolve::ManifestTarget;
use crate::domain::Principal;

/// One workspace to count.
pub struct CountRequest<'a> {
    pub home: &'a Path,
    pub principal: &'a Principal,
    pub destination: Destination<'a>,
    /// The workspace's own org, team and repo (what a publish resolves).
    pub target: &'a ManifestTarget,
    /// The legacy lockfile's rows, for a workspace prepared before the
    /// managed record existed (seeded in memory only).
    pub legacy: Option<&'a dyn Fn() -> Result<LegacyRows, String>>,
}

/// How far a count got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EditsState {
    /// Counted now.
    Checked,
    /// A completed machine workspace whose directory no longer exists.
    Missing,
    /// An earlier copy to this machine workspace has not finished; counting
    /// would have to finish it (a write), so it was not counted.
    UnfinishedCopy,
    /// Couldn't count (`reason` says why). Never read as "no edits".
    Unknown,
    /// A machine workspace whose machine is not connected: not counted now
    /// (never dialled); the files, when any, are its last count.
    Offline,
}

/// What an edit is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EditKind {
    /// A copied team file edited here.
    Changed,
    /// A new file under the workspace's team folders.
    New,
    /// A copied team file removed here.
    Removed,
    /// Changed here and in the team since it was copied (or the team file
    /// can't be read): a publish refuses it until someone decides.
    Conflict,
    /// Edited here, but a publish can't move it (`reason` says why: too
    /// large). Listed so it is never invisible.
    CannotPublish,
}

/// Where publishing the file would send it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum EditDestination {
    /// The workspace's own team.
    #[serde(rename_all = "camelCase")]
    Team { name: String },
    /// A parent team the workspace's team inherits from (its name; with its
    /// org when that differs from the workspace's).
    #[serde(rename_all = "camelCase")]
    ParentTeam { name: String },
    /// Your private (personal) files.
    Private,
    /// The repository's own context tier.
    Repo,
    /// A tier label CodeSense doesn't recognise (shown as is).
    #[serde(rename_all = "camelCase")]
    Other { label: String },
}

/// One edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditedFile {
    /// Workspace-relative.
    pub path: String,
    pub kind: EditKind,
    pub destination: EditDestination,
    /// When the workspace file last changed (its mtime), on this Mac only;
    /// a machine reports none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_at_ms: Option<u64>,
    /// A conflict's reason, in the engine's words.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// `blake3:<hex>` of the workspace bytes as counted (none when not read):
    /// an action passes it back so a file rewritten since is refused.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_hash: Option<String>,
    /// `blake3:<hex>` of the team file a publish of it expects.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_hash: Option<String>,
    /// `size:mtime_ns` of a new file on this Mac listed without being read:
    /// an action passes it back in place of a hash.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_stat: Option<String>,
    /// The team file it publishes to (absolute, on this Mac).
    pub team_path: std::path::PathBuf,
    /// A `cannotPublish` edit Discard can still settle (a new file is deleted;
    /// a copied one gets the team's version back) — not when the team's own
    /// file is the one too large to move.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub discardable: bool,
}

fn hex_hash(hash: &[u8; 32]) -> String {
    format!("blake3:{}", blake3::Hash::from_bytes(*hash).to_hex())
}

/// Edits by kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditCounts {
    pub changed: usize,
    pub new: usize,
    pub removed: usize,
    pub conflicts: usize,
    pub cannot_publish: usize,
}

impl EditCounts {
    pub fn total(&self) -> usize {
        self.changed + self.new + self.removed + self.conflicts + self.cannot_publish
    }
}

/// One workspace's count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceEdits {
    pub contexts: Vec<String>,
    pub state: EditsState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub counts: EditCounts,
    /// Sorted by path.
    pub files: Vec<EditedFile>,
    /// The newest `changedAtMs` among the files (this Mac only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_changed_at_ms: Option<u64>,
    pub checked_at_ms: u64,
    /// Workspace files read to hash them (the stat-cache budget).
    #[serde(skip)]
    pub hashed: usize,
}

impl WorkspaceEdits {
    fn not_counted(state: EditsState, reason: String) -> Self {
        Self {
            contexts: Vec::new(),
            state,
            reason: Some(reason),
            counts: EditCounts::default(),
            files: Vec::new(),
            last_changed_at_ms: None,
            checked_at_ms: now_ms(),
            hashed: 0,
        }
    }

    fn remote_error(reason: String) -> Self {
        use crate::content_engine::destination::{DestinationBatchError, WorkspacePathError};
        match DestinationBatchError::workspace_path_error(&reason) {
            Some(error) => Self::not_counted(
                if error == WorkspacePathError::Missing {
                    EditsState::Missing
                } else {
                    EditsState::Unknown
                },
                error.reason().into(),
            ),
            None => Self::unknown(reason),
        }
    }

    pub fn unknown(reason: impl Into<String>) -> Self {
        Self::not_counted(EditsState::Unknown, reason.into())
    }

    /// Not counted now, for `reason` (e.g. a machine that isn't connected).
    pub fn not_counted_now(state: EditsState, reason: impl Into<String>) -> Self {
        Self::not_counted(state, reason.into())
    }
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Where a tier label (`team:org/team`, `personal…`, `repo:…`) publishes.
pub fn destination_of(layer: &str, target: &ManifestTarget) -> EditDestination {
    if let Some(team) = layer.strip_prefix("team:") {
        if team == format!("{}/{}", target.org, target.team) {
            return EditDestination::Team {
                name: target.team.clone(),
            };
        }
        // `org/team`, where an org is itself `owner/repo`.
        let name = match team.rsplit_once('/') {
            Some((org, name)) if org == target.org => name.to_string(),
            _ => team.to_string(),
        };
        return EditDestination::ParentTeam { name };
    }
    if layer == "personal" || layer.starts_with("personal:") {
        return EditDestination::Private;
    }
    if layer.starts_with("repo:") {
        return EditDestination::Repo;
    }
    EditDestination::Other {
        label: layer.to_string(),
    }
}

/// Count one workspace's team-file edits. Reads only; never fails — what it
/// couldn't do is its `state` and `reason`.
pub fn count(request: &CountRequest<'_>) -> WorkspaceEdits {
    count_for(request, &Default::default())
}

pub(crate) fn count_for(
    request: &CountRequest<'_>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> WorkspaceEdits {
    let identity = publish::identity_of(request.destination);
    let mut record = match ManagedRecord::read(request.home, request.principal, &identity) {
        Ok(record) => record,
        Err(reason) => return WorkspaceEdits::unknown(reason),
    };
    if !record.seeded {
        if let Some(legacy) = request.legacy {
            match legacy() {
                // In memory only: a count writes nothing.
                Ok(rows) => {
                    record.seed(rows);
                }
                Err(error) => return WorkspaceEdits::remote_error(error),
            }
        }
    }
    if let Destination::Remote { io } = request.destination {
        if io.has_pending() {
            return WorkspaceEdits::not_counted(
                EditsState::UnfinishedCopy,
                "an earlier copy to this workspace has not finished".into(),
            );
        }
    }
    let expect = std::collections::BTreeMap::new();
    let publish_request = PublishRequest {
        home: request.home,
        principal: request.principal,
        destination: request.destination,
        target: request.target,
        paths: &[],
        sets: &[],
        to: None,
        expect: &expect,
        include_deletes: false,
        legacy: None,
        operation: "count",
        workspace: String::new(),
        agent: String::new(),
        progress: &|_| {},
        validate: &|| Ok(()),
        cancel: None,
    };
    let edits = match publish::observe_edits_for(&publish_request, &record, scope) {
        Ok(edits) => edits,
        Err(reason) => return WorkspaceEdits::remote_error(reason),
    };
    let local_root = match request.destination {
        Destination::Local { root } => root.canonicalize().ok(),
        Destination::Remote { .. } => None,
    };
    let changed_at = |path: &str| -> Option<u64> {
        let meta = std::fs::symlink_metadata(local_root.as_ref()?.join(path)).ok()?;
        let modified = meta.modified().ok()?;
        modified
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_millis() as u64)
    };
    let hashes = &edits.hashes;
    let team_hashes = &edits.team_hashes;
    let ws_hash = |path: &str| hashes.get(path).map(hex_hash);
    let stats = &edits.stats;
    let ws_stat = |path: &str| {
        stats
            .get(path)
            .map(|(size, mtime)| format!("{size}:{mtime}"))
    };
    let team_hash = |path: &str| team_hashes.get(path).map(hex_hash);
    let mut files = Vec::new();
    let mut counts = EditCounts::default();
    for entry in edits.changed {
        counts.changed += 1;
        files.push(EditedFile {
            changed_at_ms: changed_at(&entry.path),
            destination: destination_of(&entry.target_layer, request.target),
            path: entry.path,
            kind: EditKind::Changed,
            reason: None,
            workspace_hash: None,
            team_hash: None,
            workspace_stat: None,
            team_path: entry.target_path,
            discardable: false,
        });
    }
    for entry in edits.added {
        counts.new += 1;
        files.push(EditedFile {
            changed_at_ms: changed_at(&entry.path),
            destination: destination_of(&entry.target_layer, request.target),
            path: entry.path,
            kind: EditKind::New,
            reason: None,
            workspace_hash: None,
            team_hash: None,
            workspace_stat: None,
            team_path: entry.target_path,
            discardable: false,
        });
    }
    for (path, layer, team_path) in edits.removed {
        counts.removed += 1;
        files.push(EditedFile {
            changed_at_ms: None,
            destination: destination_of(&layer, request.target),
            path,
            kind: EditKind::Removed,
            reason: None,
            workspace_hash: None,
            team_hash: None,
            workspace_stat: None,
            team_path,
            discardable: false,
        });
    }
    for conflict in edits.conflicts {
        counts.conflicts += 1;
        files.push(EditedFile {
            changed_at_ms: changed_at(&conflict.path),
            destination: destination_of(&conflict.target_layer, request.target),
            path: conflict.path,
            kind: EditKind::Conflict,
            reason: Some(conflict.reason),
            workspace_hash: None,
            team_hash: None,
            workspace_stat: None,
            team_path: conflict.target_path,
            discardable: false,
        });
    }
    for held in edits.held {
        counts.cannot_publish += 1;
        files.push(EditedFile {
            changed_at_ms: changed_at(&held.path),
            destination: destination_of(&held.layer, request.target),
            path: held.path,
            kind: EditKind::CannotPublish,
            reason: Some(held.why),
            team_path: held.target,
            discardable: held.discardable,
            workspace_hash: None,
            team_hash: None,
            workspace_stat: None,
        });
    }
    for file in &mut files {
        file.workspace_hash = ws_hash(&file.path);
        file.team_hash = team_hash(&file.path);
        if file.workspace_hash.is_none() {
            file.workspace_stat = ws_stat(&file.path);
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path).then(a.kind.cmp(&b.kind)));
    WorkspaceEdits {
        contexts: record
            .entries
            .values()
            .filter(|e| {
                !e.source_layer.starts_with("personal")
                    && e.source_layer != super::want::GENERATED_LAYER
            })
            .map(|e| e.source_context.clone().unwrap_or_else(|| "clone".into()))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect(),
        state: EditsState::Checked,
        reason: None,
        last_changed_at_ms: files.iter().filter_map(|f| f.changed_at_ms).max(),
        counts,
        files,
        checked_at_ms: now_ms(),
        hashed: edits.hashed,
    }
}

#[cfg(test)]
#[path = "edits_tests.rs"]
mod tests;
