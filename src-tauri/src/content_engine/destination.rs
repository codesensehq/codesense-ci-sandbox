// SPDX-License-Identifier: MIT
//! Destination identities and filesystem facts used by the content engine.
//! Remote identities never implement `AsRef<Path>`: routing them through a local
//! filesystem API must be an explicit, rejected operation rather than a fallback.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum DestinationIdentity {
    Local { root: PathBuf },
    Remote(RemoteDestinationIdentity),
}

/// Durable identity; a connection generation validates access, but does not
/// create a new applied-state namespace on every reconnect.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteDestinationIdentity {
    pub owner_id: u64,
    pub machine_id: String,
    pub server_id: String,
    pub workspace_handle: String,
}

/// A contained file name, independent of either host's absolute directory.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DestinationRelativePath(String);
impl DestinationRelativePath {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for DestinationRelativePath {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty()
            || value.contains(['\\', '\0'])
            || value.split('/').any(|part| {
                part.is_empty() || matches!(part, "." | "..") || part.eq_ignore_ascii_case(".git")
            })
        {
            return Err(
                "destination path must be relative, contained, and outside Git metadata".into(),
            );
        }
        Ok(Self(value))
    }
}
impl From<DestinationRelativePath> for String {
    fn from(value: DestinationRelativePath) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum DestinationReference {
    /// Diagnostic location only. Applied-entry validation rejects root references.
    RemoteRoot {
        destination: RemoteDestinationIdentity,
    },
    Local {
        path: PathBuf,
    },
    Remote {
        destination: RemoteDestinationIdentity,
        relative: DestinationRelativePath,
    },
}

impl From<PathBuf> for DestinationReference {
    fn from(path: PathBuf) -> Self {
        Self::Local { path }
    }
}
impl DestinationReference {
    /// Diagnostic text only; intentionally not a filesystem conversion.
    pub fn display(&self) -> String {
        match self {
            Self::Local { path } => path.display().to_string(),
            Self::Remote {
                destination,
                relative,
            } => format!(
                "{}:{}:{}",
                destination.machine_id,
                destination.workspace_handle,
                relative.as_str(),
            ),
            Self::RemoteRoot { destination } => format!(
                "{}:{}",
                destination.machine_id, destination.workspace_handle,
            ),
        }
    }
}

/// The same applied-entry and lockfile representation as local hydration,
/// with destination references carried through every engine/store boundary.
pub type DestinationLockfile = super::lockfile::Lockfile<DestinationReference, DestinationIdentity>;
pub type DestinationEntry = super::lockfile::MaterializedEntry<DestinationReference>;

/// Local metadata may use the existing mtime shortcut. An admitted remote read
/// supplies an authoritative hash; its clock can never authorize that shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestinationObservation {
    Absent,
    Directory,
    Unsafe,
    LocalRegular {
        mtime_ns: u128,
        size: u64,
    },
    RemoteRegular {
        hash: [u8; 32],
        mtime_ns: u128,
        size: u64,
        mode: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExpectedDestination {
    Absent,
    Hash([u8; 32]),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DestinationEffect {
    Write { bytes: Vec<u8>, mode: Option<u32> },
    Remove,
    SetMode { mode: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DestinationEffectOutcome {
    /// An intact durable operation namespace proves no intent was accepted.
    /// Only this answer permits sending the same immutable intent again.
    NotStarted {
        operation_id: uuid::Uuid,
    },
    Applied(DestinationObservation),
    Conflict(DestinationObservation),
    /// The operation may have taken effect. Reconcile this ID before issuing
    /// any new operation; it is never an applied baseline acknowledgement.
    Unknown {
        operation_id: uuid::Uuid,
    },
}

impl From<codesense_pty_core::remote_files::Observation> for DestinationObservation {
    fn from(value: codesense_pty_core::remote_files::Observation) -> Self {
        use codesense_pty_core::remote_files::Observation;
        match value {
            Observation::Absent => Self::Absent,
            Observation::Directory => Self::Directory,
            Observation::Unsafe => Self::Unsafe,
            Observation::Regular {
                hash,
                mtime_ns,
                size,
                mode,
            } => Self::RemoteRegular {
                hash,
                mtime_ns: mtime_ns.into(),
                size,
                mode,
            },
        }
    }
}
impl From<&ExpectedDestination> for codesense_pty_core::remote_files::Expected {
    fn from(value: &ExpectedDestination) -> Self {
        match value {
            ExpectedDestination::Absent => Self::Absent,
            ExpectedDestination::Hash(hash) => Self::Hash { hash: *hash },
        }
    }
}
impl DestinationEffectOutcome {
    pub(crate) fn from_wire(
        value: codesense_pty_core::remote_files::EffectOutcome,
        id: uuid::Uuid,
    ) -> Result<Self, String> {
        use codesense_pty_core::remote_files::EffectOutcome;
        match value {
            EffectOutcome::Applied { observation } => Ok(Self::Applied(observation.into())),
            EffectOutcome::Conflict { observation } => Ok(Self::Conflict(observation.into())),
            EffectOutcome::NotStarted { operation_id } if operation_id == id.to_string() => {
                Ok(Self::NotStarted { operation_id: id })
            }
            EffectOutcome::Unknown { operation_id } if operation_id == id.to_string() => {
                Ok(Self::Unknown { operation_id: id })
            }
            _ => Err("remote receipt operation identity mismatch".into()),
        }
    }
}

/// Immutable file effect retained before dispatch. The entry template carries
/// the original source home, so reconciliation never reconstructs ownership
/// from a later projection configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DestinationFileIntent {
    pub relative: DestinationRelativePath,
    pub expected: ExpectedDestination,
    pub effect: DestinationEffect,
    pub entry: DestinationEntry,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum DestinationOperationIntent {
    File(Box<DestinationFileIntent>),
    EnsureDirectory {
        relative: DestinationRelativePath,
    },
    ManagedExclude {
        expected: ExpectedDestination,
        lines: Vec<String>,
    },
}

/// SW-XS-6: the marker that names an outdated terminal server in an error
/// string, so any caller can surface "Update terminal server" (the remote tab
/// refusal code `terminalServerOutdated`) without parsing prose.
pub const TERMINAL_SERVER_OUTDATED: &str = "terminalServerOutdated";

/// Why a manifest or batch could not run against a destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DestinationBatchError {
    /// The machine's terminal server predates `ContextManifestV1`, and this
    /// operation needs it. Nothing was sent; there is no per-file fallback.
    ServerOutdated {
        machine_id: String,
    },
    /// This destination has no batched wire (a local destination).
    Unsupported,
    /// The manifest stopped early (a limit, an unreadable directory, the Git
    /// index unavailable). A diff — and above all a removal — is never
    /// computed from it.
    Incomplete {
        reason: String,
    },
    /// An earlier batch to this destination was cut and could not be settled;
    /// nothing new is sent until it is.
    Unrecovered {
        batch_id: uuid::Uuid,
        reason: String,
    },
    /// SW-XS-8: the caller cancelled before the Apply; the batch was
    /// abandoned on the machine and nothing in it was applied.
    Cancelled,
    Failed(String),
}
impl std::fmt::Display for DestinationBatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ServerOutdated { machine_id } => write!(
                f,
                "[{TERMINAL_SERVER_OUTDATED}] Update the terminal server on {machine_id}: it is too old to sync agent context"
            ),
            Self::Unsupported => f.write_str("this destination has no batched sync"),
            Self::Incomplete { reason } => {
                write!(f, "the machine's file list is incomplete: {reason}")
            }
            Self::Unrecovered { batch_id, reason } => write!(
                f,
                "an earlier context sync ({batch_id}) to this machine is unsettled: {reason}"
            ),
            Self::Cancelled => f.write_str("cancelled before anything was applied"),
            Self::Failed(error) => f.write_str(error),
        }
    }
}
/// A machine path refusal, recognized only at the remote count boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspacePathError {
    Missing,
    Unsafe,
    Occupied,
}
impl WorkspacePathError {
    pub fn reason(self) -> &'static str {
        match self {
            Self::Missing => "folder no longer exists on the machine",
            Self::Unsafe => "the workspace path on the machine isn't a normal folder",
            Self::Occupied => "something else is at the workspace path",
        }
    }
}
impl DestinationBatchError {
    pub fn workspace_path_error(error: &str) -> Option<WorkspacePathError> {
        use codesense_pty_core::remote_workspace::{
            WORKSPACE_MISSING, WORKSPACE_OCCUPIED, WORKSPACE_UNSAFE,
        };
        [
            (WORKSPACE_MISSING, WorkspacePathError::Missing),
            (WORKSPACE_UNSAFE, WorkspacePathError::Unsafe),
            (WORKSPACE_OCCUPIED, WorkspacePathError::Occupied),
        ]
        .into_iter()
        .find_map(|(prefix, state)| error.starts_with(prefix).then_some(state))
    }

    /// Whether `error` (any error string on a launch or sync path) names an
    /// outdated terminal server.
    pub fn names_outdated_server(error: &str) -> bool {
        error.contains(&format!("[{TERMINAL_SERVER_OUTDATED}]"))
    }
}

/// SW-XS-6: a COMPLETE manifest of a destination under declared prefixes.
/// An incomplete one is never this type: it is
/// [`DestinationBatchError::Incomplete`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteManifest {
    /// The prefixes as the machine validated and folded them.
    pub prefixes: Vec<String>,
    /// Regular files, each with its hash.
    pub files: Vec<codesense_pty_core::remote_manifest::ManifestEntry>,
    /// Everything the diff must leave alone, each with its kind and reason:
    /// symlinks, nested repositories, other types, `Unsafe` (hard-linked or
    /// foreign-owned), `Unstable` (changing while hashed), `InvalidName`.
    pub skipped: Vec<codesense_pty_core::remote_manifest::ManifestEntry>,
    pub progress: codesense_pty_core::remote_manifest::ManifestProgress,
    /// A batch an earlier call left behind, settled before this manifest was
    /// read: the managed record and the exclude `expected` must learn what it
    /// applied before a diff is computed.
    pub recovered: Option<RecoveredBatch>,
    /// SW-XS-7: this workspace's own exclude block as the machine has it
    /// (`None`: not reported).
    pub workspace_exclude: Option<codesense_pty_core::remote_manifest::WorkspaceExclude>,
}

/// SW-XS-8: the bytes a manifest read back (`read`), by path, and the paths
/// it could not read with the reason.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadFiles {
    pub files: std::collections::BTreeMap<String, Vec<u8>>,
    pub unread: Vec<(String, String)>,
}

/// What a cut batch did, once settled by [`DestinationIo::reconcile_pending`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredBatch {
    pub batch_id: uuid::Uuid,
    pub phase: codesense_pty_core::context_batch::TransferPhase,
    /// Its writes' outcomes by `index` (the position in the original batch's
    /// `writes`); oversize writes were never sent and do not appear.
    pub writes: Vec<codesense_pty_core::context_batch::MemberResult>,
    pub operations: Vec<codesense_pty_core::context_batch::OperationResult>,
    /// The exclude block as it left it: the next `expected`.
    pub exclude_paths: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchWrite {
    pub operation_id: uuid::Uuid,
    pub path: DestinationRelativePath,
    pub bytes: Vec<u8>,
    pub mode: u32,
    /// The pre-hash: written only while the destination still holds this.
    pub expected: ExpectedDestination,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchRemove {
    pub operation_id: uuid::Uuid,
    pub path: DestinationRelativePath,
    pub expected_pre_hash: [u8; 32],
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchChmod {
    pub operation_id: uuid::Uuid,
    pub path: DestinationRelativePath,
    pub mode: u32,
    pub expected_pre_hash: [u8; 32],
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchDir {
    pub operation_id: uuid::Uuid,
    pub path: DestinationRelativePath,
}
/// This workspace's own block in the clone's shared exclude file, written by
/// compare-and-swap against `expected`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchExclude {
    pub operation_id: uuid::Uuid,
    pub workspace_id: String,
    pub expected: Vec<String>,
    pub paths: Vec<String>,
    /// Replace this workspace's block whatever it holds, after a Conflict
    /// from a hand-edited block. Explicit only (a "Repair" action).
    pub repair: bool,
}
/// One Apply's worth of changes. Every ID belongs to this attempt alone: a
/// retry of the same attempt replays its stored outcomes; a new attempt gets
/// new IDs and is never answered from an old one's receipts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestinationBatch {
    pub batch_id: uuid::Uuid,
    pub writes: Vec<BatchWrite>,
    pub removes: Vec<BatchRemove>,
    pub chmods: Vec<BatchChmod>,
    pub ensure_dirs: Vec<BatchDir>,
    pub exclude: Option<BatchExclude>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestinationBatchOutcome {
    pub phase: codesense_pty_core::context_batch::TransferPhase,
    /// One per write, in `writes` order. A write larger than one member may
    /// be is `TooLarge` here and was never sent; the rest still applied.
    pub writes: Vec<codesense_pty_core::context_batch::MemberResult>,
    pub operations: Vec<codesense_pty_core::context_batch::OperationResult>,
    /// The exclude block as this batch left it, once its final write applied.
    pub exclude_paths: Option<Vec<String>>,
    /// A batch an earlier call left behind, settled before this one ran.
    pub recovered: Option<RecoveredBatch>,
}

/// Blocking I/O port used from the engine's existing blocking boundary. Policy,
/// projection selection, merge and reverse promotion remain engine decisions.
pub trait DestinationIo: Send + Sync {
    /// SW-XS-6: everything under `prefixes` with hashes, in one call.
    fn manifest(
        &self,
        _prefixes: &[String],
        _limit: usize,
        _progress: &mut dyn FnMut(&codesense_pty_core::remote_manifest::ManifestProgress),
    ) -> Result<CompleteManifest, DestinationBatchError> {
        Err(DestinationBatchError::Unsupported)
    }
    /// SW-XS-27: [`Self::manifest`] for a read-only count. ONE call, and it
    /// never settles a batch an earlier call left behind: a destination with
    /// one pending refuses ([`Self::has_pending`] says so first) instead of
    /// resuming it, because resuming applies writes on the machine.
    fn manifest_observe(
        &self,
        _prefixes: &[String],
        _limit: usize,
        _progress: &mut dyn FnMut(&codesense_pty_core::remote_manifest::ManifestProgress),
    ) -> Result<CompleteManifest, DestinationBatchError> {
        Err(DestinationBatchError::Unsupported)
    }
    /// SW-XS-27: whether a batch an earlier call left behind is waiting to
    /// be settled (read from the journal; settles nothing).
    fn has_pending(&self) -> bool {
        false
    }
    /// SW-XS-8: [`Self::manifest`] that also streams back the bytes of the
    /// exact files `read` names (each under `prefixes`), in the SAME call:
    /// an explicit publish reads every changed file at once.
    fn manifest_reading(
        &self,
        _prefixes: &[String],
        _limit: usize,
        _read: &[String],
        _progress: &mut dyn FnMut(&codesense_pty_core::remote_manifest::ManifestProgress),
    ) -> Result<(CompleteManifest, ReadFiles), DestinationBatchError> {
        Err(DestinationBatchError::Unsupported)
    }
    /// SW-XS-6: settle a batch an earlier call to this destination left
    /// behind (cut by a timeout, a lost connection or an app exit): resume
    /// and finish it, or abandon it if it never applied. Returns what it did,
    /// so the caller updates its managed record and exclude `expected` BEFORE
    /// any diff. `manifest()` and `apply_batch()` call it first themselves.
    ///
    /// Contract: the caller holds the destination lock across reconcile,
    /// manifest, diff and `apply_batch` (SW-XS-7), so no other writer moves
    /// the workspace between them.
    fn reconcile_pending(&self) -> Result<Option<RecoveredBatch>, DestinationBatchError> {
        Ok(None)
    }
    /// SW-XS-6: every write, removal, mode change, directory and the exclude
    /// block in ONE Apply, each entry checked against its pre-hash.
    ///
    /// The batch's id is journaled before anything is sent. A call that was
    /// cut (timeout, lost connection, app exit) is recovered by the next call
    /// to this destination, or by calling again with the same batch: the cut
    /// batch is resumed (Apply is idempotent by operation id) and settled
    /// before anything new is sent.
    ///
    /// `progress(sent, total)` returns whether to go on: `false` before the
    /// Apply abandons the batch on the machine (nothing in it applies) and
    /// returns [`DestinationBatchError::Cancelled`] (SW-XS-8).
    fn apply_batch(
        &self,
        _batch: &DestinationBatch,
        _progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<DestinationBatchOutcome, DestinationBatchError> {
        Err(DestinationBatchError::Unsupported)
    }
    /// None means the captured peer did not negotiate outbound batching.
    /// The adapter includes its real target/routing envelope in this bound.
    /// Stable admitted workspace setup identity, never a transport epoch.
    fn selected_batch_target(
        &self,
    ) -> Result<Option<codesense_pty_core::remote_files::FileTarget>, String> {
        Ok(None)
    }
    fn selected_batch_control_size(
        &self,
        _batch_id: uuid::Uuid,
        _manifest_digest: [u8; 32],
        _operation: &codesense_pty_core::context_batch::BatchOperation,
    ) -> Result<Option<usize>, String> {
        Ok(None)
    }
    fn selected_batch(
        &self,
        _batch_id: uuid::Uuid,
        _manifest_digest: [u8; 32],
        _operation: codesense_pty_core::context_batch::BatchOperation,
    ) -> Result<codesense_pty_core::context_batch::BatchReply, String> {
        Err("selected context batching was not negotiated".into())
    }
    fn identity(&self) -> &DestinationIdentity;
    /// Revalidate immediately before desktop source effects and publishing results.
    fn validate_authority(&self) -> Result<(), String>;
    fn inspect(&self, path: &DestinationRelativePath) -> Result<DestinationObservation, String>;
    fn read(
        &self,
        path: &DestinationRelativePath,
        expected_hash: Option<[u8; 32]>,
        limit: usize,
    ) -> Result<(DestinationObservation, Vec<u8>), String>;
    /// Return the complete bounded set or an error; truncation is not a complete scan.
    fn enumerate(
        &self,
        prefix: Option<&DestinationRelativePath>,
        limit: usize,
    ) -> Result<Vec<DestinationRelativePath>, String>;
    fn apply(
        &self,
        operation_id: uuid::Uuid,
        path: &DestinationRelativePath,
        expected: &ExpectedDestination,
        effect: &DestinationEffect,
    ) -> Result<DestinationEffectOutcome, String>;
    fn reconcile(&self, operation_id: uuid::Uuid) -> Result<DestinationEffectOutcome, String>;
    /// A complete index snapshot; unknown/incomplete index knowledge cannot authorize writes.
    fn tracked_paths(
        &self,
        prefix: Option<&DestinationRelativePath>,
        limit: usize,
    ) -> Result<Vec<DestinationRelativePath>, String>;
    /// Idempotent creation confined to real directories under the held workspace root.
    /// Existing regular files or symlinks are conflicts, never replaced.
    fn ensure_directories(
        &self,
        operation_id: uuid::Uuid,
        path: &DestinationRelativePath,
    ) -> Result<DestinationEffectOutcome, String>;
    /// The separate Git metadata capability exposes only the CodeSense-managed
    /// exclude block. The adapter preserves every byte outside that block.
    fn read_managed_exclude(&self) -> Result<ManagedExcludeState, String>;
    fn replace_managed_exclude(
        &self,
        operation_id: uuid::Uuid,
        expected: &ExpectedDestination,
        lines: &[String],
    ) -> Result<DestinationEffectOutcome, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedExcludeState {
    /// Hash of the complete exclude file, including user-owned lines.
    pub expected: ExpectedDestination,
    pub managed_lines: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn destination_relative_paths_cannot_address_roots_or_git_metadata() {
        for value in [
            "",
            "/tmp/file",
            "../file",
            "a/../file",
            "a//file",
            "a\\file",
            ".git/config",
            "a/.git/config",
            ".GIT/config",
        ] {
            assert!(
                DestinationRelativePath::try_from(value.to_string()).is_err(),
                "{value}"
            );
        }
        assert!(
            DestinationRelativePath::try_from("_bmad-output/epic/story.md".to_string()).is_ok()
        );
    }
    #[test]
    fn same_remote_path_cannot_collapse_machine_identity() {
        let first = RemoteDestinationIdentity {
            owner_id: 41,
            machine_id: "one".into(),
            server_id: "server".into(),
            workspace_handle: "worktree".into(),
        };
        let mut second = first.clone();
        second.machine_id = "two".into();
        assert_ne!(
            DestinationIdentity::Remote(first),
            DestinationIdentity::Remote(second)
        );
    }
    #[test]
    fn destination_file_journal_round_trips_real_entry_metadata() {
        let identity = RemoteDestinationIdentity {
            owner_id: 41,
            machine_id: "one".into(),
            server_id: "server".into(),
            workspace_handle: "worktree".into(),
        };
        let relative = DestinationRelativePath::try_from("docs/file.md".to_string()).unwrap();
        for mtime in [0, 1_798_771_200_123_456_789u128] {
            let intent = DestinationOperationIntent::File(Box::new(DestinationFileIntent {
                relative: relative.clone(),
                expected: ExpectedDestination::Absent,
                effect: DestinationEffect::Write {
                    bytes: b"saved context".to_vec(),
                    mode: Some(0o644),
                },
                entry: DestinationEntry {
                    logical_name: "docs/file.md".into(),
                    dest_path: DestinationReference::Remote {
                        destination: identity.clone(),
                        relative: relative.clone(),
                    },
                    source_layer: crate::content_engine::layers::LayerId::TeamProjection,
                    source_path: PathBuf::from("/canonical/docs/file.md"),
                    last_materialized_hash: *blake3::hash(b"saved context").as_bytes(),
                    last_materialized_mtime_ns: mtime,
                    last_materialized_size: Some(13),
                    projection_key: Some("docs".into()),
                    source_oid: None,
                },
            }));
            let value = serde_json::to_value(&intent).unwrap();
            let persisted = serde_json::to_string(&value).unwrap();
            let value: serde_json::Value = serde_json::from_str(&persisted).unwrap();
            assert_eq!(
                serde_json::from_value::<DestinationOperationIntent>(value).unwrap(),
                intent
            );
        }
    }
}
