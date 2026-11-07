// SPDX-License-Identifier: MIT
//! Effects-only filesystem port for the existing hydration owner.
use serde::{Deserialize, Serialize};
pub const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_PATHS: usize = 100_000;
/// A lock-free read that kept overlapping writes past its deadline.
/// Nothing was changed; the caller may retry the read.
pub const WORKSPACE_BUSY: &str = "remote workspace is busy with writes; retry the read";
mod optional_bytes {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(
        bytes: &Option<Vec<u8>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match bytes {
            Some(bytes) => super::bytes::serialize(bytes, serializer),
            None => serializer.serialize_none(),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Vec<u8>>, D::Error> {
        #[derive(Deserialize)]
        struct Wrapped(#[serde(with = "super::bytes")] Vec<u8>);
        Ok(Option::<Wrapped>::deserialize(deserializer)?.map(|Wrapped(bytes)| bytes))
    }
}

impl FileOperation {
    /// Reads take no lock on the machine and change nothing; only they can
    /// answer [`WORKSPACE_BUSY`], and only they are retried on it.
    /// An explicit allow-list: a variant added later is not a read until it
    /// is listed here.
    pub fn is_read(&self) -> bool {
        matches!(
            self,
            Self::Inspect { .. }
                | Self::Changes { .. }
                | Self::Redline { .. }
                | Self::Read { .. }
                | Self::Enumerate { .. }
                | Self::TrackedPaths { .. }
                | Self::List { .. }
                | Self::ReadHead { .. }
                | Self::ListFiles { .. }
                | Self::ReadManagedExclude
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileTarget {
    pub owner_id: u64,
    pub origin_key: String,
    pub request_id: String,
    pub handle: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileRequest {
    pub target: FileTarget,
    pub operation: FileOperation,
}
/// One child of a [`FileOperation::List`] directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListEntry {
    pub name: String,
    pub is_dir: bool,
    /// Byte size of a regular file; `None` for a directory or anything else.
    pub size: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum FileOperation {
    Inspect {
        path: String,
    },
    Read {
        path: String,
        expected_hash: Option<[u8; 32]>,
        limit: usize,
    },
    Enumerate {
        prefix: Option<String>,
        limit: usize,
    },
    /// One directory level (`None` = the worktree root), for an editor's lazy
    /// file tree. Unlike `Enumerate` it never descends, so a sibling
    /// `node_modules` or `target` cannot push a root listing past the limit.
    /// Only sent to a host advertising the workspace-editor capability.
    List {
        path: Option<String>,
    },
    /// The committed (`HEAD`) bytes of one file, so an editor can mark lines
    /// changed since the last commit. Workspace-editor capability only.
    ReadHead {
        path: String,
    },
    /// Every file an editor's quick-open should offer: tracked plus untracked
    /// files Git does not ignore. Workspace-editor capability only.
    ListFiles {
        limit: usize,
    },
    Apply {
        operation_id: String,
        path: String,
        expected: Expected,
        effect: Effect,
    },
    Reconcile {
        operation_id: String,
    },
    TrackedPaths {
        prefix: Option<String>,
        limit: usize,
    },
    EnsureDirectories {
        operation_id: String,
        path: String,
    },
    ReadManagedExclude,
    ReplaceManagedExclude {
        operation_id: String,
        expected: Expected,
        lines: Vec<String>,
    },
    /// `ContextManifestV1`: replace this workspace's own marker
    /// block in the clone's shared exclude file, only while it lists exactly
    /// `expected` (compare-and-swap). `paths` are workspace-relative files,
    /// written as anchored, escaped patterns; an empty list removes the block.
    ReplaceWorkspaceExclude {
        operation_id: String,
        workspace_id: String,
        expected: Vec<String>,
        paths: Vec<String>,
        /// The explicit repair of a block someone edited by hand (a foreign
        /// line, a broken marker): replace this workspace's block whatever it
        /// holds. Never set by an ordinary sync.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        repair: bool,
    },
    /// The worktree's changed files against its fork point (or `HEAD` when
    /// `base_ref` is `"HEAD"`), committed and uncommitted work alike, untracked
    /// files included. Read-only. Answered with [`FileReply::Changes`].
    Changes {
        base_ref: Option<String>,
    },
    /// One changed file's baseline and working-tree bytes, against the same
    /// baseline [`FileOperation::Changes`] resolves. `old_path` names the
    /// pre-rename path the baseline side is read from. Answered with
    /// [`FileReply::Redline`].
    Redline {
        path: String,
        old_path: Option<String>,
        base_ref: Option<String>,
    },
}
/// Coarse change class of one path, mirroring the desktop's A/M/D marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeStatus {
    Added,
    Modified,
    Deleted,
}

/// One changed path as the remote worktree's Git reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangedFile {
    pub path: String,
    pub status: ChangeStatus,
    /// The pre-rename path when Git paired this change as a rename.
    pub old_path: Option<String>,
    /// Line counts; `None` for a binary change or one Git could not count.
    pub added: Option<u32>,
    pub removed: Option<u32>,
    /// The file mode (or symlink-ness) changed.
    pub mode_changed: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Expected {
    Absent,
    Hash { hash: [u8; 32] },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Effect {
    Write {
        #[serde(with = "bytes")]
        bytes: Vec<u8>,
        mode: Option<u32>,
    },
    Remove,
    SetMode {
        mode: u32,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Observation {
    Absent,
    Directory,
    Unsafe,
    Regular {
        hash: [u8; 32],
        mtime_ns: u64,
        size: u64,
        mode: u32,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum EffectOutcome {
    NotStarted { operation_id: String },
    Applied { observation: Observation },
    Conflict { observation: Observation },
    Unknown { operation_id: String },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum FileReply {
    Inspected {
        observation: Observation,
    },
    Read {
        observation: Observation,
        #[serde(with = "bytes")]
        bytes: Vec<u8>,
    },
    Paths {
        paths: Vec<String>,
    },
    /// Answer to [`FileOperation::List`]: directories first, then files.
    Listing {
        entries: Vec<ListEntry>,
    },
    /// Answer to [`FileOperation::ReadHead`]; `None` when `HEAD` has no such
    /// file (new, untracked, or a directory/submodule entry).
    Head {
        #[serde(with = "optional_bytes")]
        bytes: Option<Vec<u8>>,
    },
    /// Answer to [`FileOperation::ListFiles`]; `truncated` when the repository
    /// holds more files than the requested limit.
    FileList {
        paths: Vec<String>,
        truncated: bool,
    },
    Effect {
        outcome: EffectOutcome,
    },
    ManagedExclude {
        expected: Expected,
        lines: Vec<String>,
    },
    Changes {
        /// The resolved baseline commit (or empty-tree) object name.
        baseline: String,
        files: Vec<ChangedFile>,
        /// Changes whenever any listed file's content or metadata, the
        /// baseline, or the listing itself changes — a cheap watch signal.
        stamp: String,
    },
    Redline {
        /// The baseline side; `None` when the path is absent there (added).
        #[serde(with = "optional_bytes")]
        before: Option<Vec<u8>>,
        /// The working-tree side; `None` when the path is gone (deleted).
        #[serde(with = "optional_bytes")]
        after: Option<Vec<u8>>,
        /// A side exists but is not a readable regular file (a symlink, a
        /// submodule, a hard-linked file); render it as an opaque change.
        opaque: bool,
    },
}
mod bytes {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        if encoded.len() > super::MAX_FILE_BYTES.div_ceil(3) * 4 {
            return Err(serde::de::Error::custom("remote file exceeds limit"));
        }
        let bytes = STANDARD.decode(encoded).map_err(serde::de::Error::custom)?;
        if bytes.len() > super::MAX_FILE_BYTES {
            return Err(serde::de::Error::custom("remote file exceeds limit"));
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod editor_wire_tests {
    use super::*;

    fn round_trip(reply: &FileReply) -> FileReply {
        serde_json::from_str(&serde_json::to_string(reply).unwrap()).unwrap()
    }

    #[test]
    fn head_reply_round_trips_present_and_absent_bytes() {
        let present = FileReply::Head {
            bytes: Some(b"fn main() {}\n".to_vec()),
        };
        assert_eq!(round_trip(&present), present);
        let absent = FileReply::Head { bytes: None };
        assert_eq!(
            serde_json::to_value(&absent).unwrap(),
            serde_json::json!({"kind": "head", "bytes": null})
        );
        assert_eq!(round_trip(&absent), absent);
    }

    #[test]
    fn listing_and_editor_operations_use_camel_case_wire_names() {
        let listing = FileReply::Listing {
            entries: vec![ListEntry {
                name: "src".into(),
                is_dir: true,
                size: None,
            }],
        };
        assert_eq!(
            serde_json::to_value(&listing).unwrap(),
            serde_json::json!({"kind": "listing", "entries": [{"name": "src", "isDir": true, "size": null}]})
        );
        assert_eq!(round_trip(&listing), listing);
        let op = FileOperation::ReadHead {
            path: "src/main.rs".into(),
        };
        assert_eq!(
            serde_json::to_value(&op).unwrap(),
            serde_json::json!({"kind": "readHead", "path": "src/main.rs"})
        );
    }
}

#[cfg(test)]
mod read_tests {
    use super::*;

    #[test]
    fn only_reads_are_retryable_on_busy() {
        let id = "00000000-0000-4000-8000-000000000001".to_string();
        assert!(FileOperation::Inspect { path: "a".into() }.is_read());
        assert!(FileOperation::Changes { base_ref: None }.is_read());
        assert!(FileOperation::Redline {
            path: "a".into(),
            old_path: None,
            base_ref: None
        }
        .is_read());
        assert!(FileOperation::ReadManagedExclude.is_read());
        assert!(FileOperation::ReadHead { path: "a".into() }.is_read());
        assert!(FileOperation::List { path: None }.is_read());
        assert!(FileOperation::ListFiles { limit: 10 }.is_read());
        assert!(!FileOperation::Reconcile {
            operation_id: id.clone()
        }
        .is_read());
        assert!(!FileOperation::EnsureDirectories {
            operation_id: id,
            path: "a".into()
        }
        .is_read());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_operations_and_replies_round_trip() {
        let operation = FileOperation::Redline {
            path: "src/new.rs".into(),
            old_path: Some("src/old.rs".into()),
            base_ref: Some("HEAD".into()),
        };
        let json = serde_json::to_string(&operation).unwrap();
        assert_eq!(
            serde_json::from_str::<FileOperation>(&json).unwrap(),
            operation
        );

        for reply in [
            FileReply::Redline {
                before: None,
                after: Some(b"fn main() {}\n".to_vec()),
                opaque: false,
            },
            FileReply::Redline {
                before: Some(vec![0, 1, 2]),
                after: None,
                opaque: true,
            },
            FileReply::Changes {
                baseline: "abc".into(),
                files: vec![ChangedFile {
                    path: "a".into(),
                    status: ChangeStatus::Modified,
                    old_path: None,
                    added: Some(1),
                    removed: None,
                    mode_changed: true,
                }],
                stamp: "1".into(),
            },
        ] {
            let json = serde_json::to_string(&reply).unwrap();
            assert_eq!(serde_json::from_str::<FileReply>(&json).unwrap(), reply);
        }
    }

    #[test]
    fn diff_operations_refuse_unknown_fields() {
        // Strict decoding is what makes an older server answer a newer call with
        // "unsupported request" rather than silently running something else.
        let json = r#"{"kind":"changes","baseRef":null,"extra":1}"#;
        assert!(serde_json::from_str::<FileOperation>(json).is_err());
    }
}
