// SPDX-License-Identifier: MIT
//! Bounded outbound context transport. File effects retain their original IDs.
use crate::remote_files::{EffectOutcome, Expected, FileTarget, Observation};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const VERSION: u32 = 1;
/// The bounds a `SelectedContextBatchV1`-only host enforces. A desktop never
/// sends more than this to a host without `ContextManifestV1`.
pub const MAX_ITEMS: usize = 64;
pub const MAX_RAW_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_ENCODED_BYTES: u64 = 17 * 1024 * 1024;
pub const MAX_CONTROL_BYTES: usize = 64 * 1024;
pub const CHUNK_BYTES: usize = 512 * 1024;
/// With `ContextManifestV1` the caps are lifted in favor of streamed
/// chunks. A batch is bounded by its operation count (the Begin control frame
/// must fit one frame) and by the host's staging headroom, not by 64 / 16 MiB.
pub const STREAMED_MAX_OPERATIONS: usize = 20_000;
pub const STREAMED_MAX_RAW_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const STREAMED_MAX_ENCODED_BYTES: u64 = STREAMED_MAX_RAW_BYTES + 64 * 1024 * 1024;
pub const STREAMED_MAX_CONTROL_BYTES: usize = 12 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Codec {
    Identity,
    Gzip,
    /// One zstd frame carrying its content size. Sent only to a
    /// host advertising `ContextBatchZstdV1`.
    Zstd,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatchItem {
    pub index: usize,
    pub operation_id: String,
    pub path: String,
    pub offset: u64,
    pub length: u64,
    pub hash: [u8; 32],
    pub mode: u32,
    pub expected: Expected,
    pub dependencies: Vec<usize>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatchManifest {
    pub request_id: String,
    pub generation: u64,
    pub codec: Codec,
    pub raw_length: u64,
    pub encoded_length: u64,
    pub raw_digest: [u8; 32],
    pub encoded_digest: [u8; 32],
    pub items: Vec<BatchItem>,
    /// `ContextManifestV1` extensions. Absent (and so absent from the digest) in a batch
    /// an older host can take.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removes: Vec<RemoveItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chmods: Vec<ChmodItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ensure_dirs: Vec<DirItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_block: Option<Box<ManagedExcludeBlock>>,
}
/// Remove one file, only while it still has the hash the diff saw. An already
/// absent file counts as removed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoveItem {
    pub operation_id: String,
    pub path: String,
    pub expected_pre_hash: [u8; 32],
}
/// Set one file's mode, only while it still has the hash the diff saw.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChmodItem {
    pub operation_id: String,
    pub path: String,
    pub mode: u32,
    pub expected_pre_hash: [u8; 32],
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirItem {
    pub operation_id: String,
    pub path: String,
}
/// This workspace's own marker block in the clone's shared
/// `.git/info/exclude`. `paths` are workspace-relative file
/// paths; the host writes each as an anchored, escaped pattern. The write is a
/// compare-and-swap: it applies only while the block still lists exactly
/// `expected`. Adds are written before any file is renamed into place; the
/// final block is written after removals.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagedExcludeBlock {
    pub operation_id: String,
    pub workspace_id: String,
    pub expected: Vec<String>,
    pub paths: Vec<String>,
    /// Replace this workspace's block whatever it holds (after a Conflict
    /// caused by a hand-edited block). Explicit only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub repair: bool,
}
impl ManagedExcludeBlock {
    pub fn validate(&self) -> Result<(), String> {
        uuid::Uuid::parse_str(&self.operation_id).map_err(|_| "invalid exclude operation UUID")?;
        if self.workspace_id.is_empty()
            || self.workspace_id.len() > 128
            || !self
                .workspace_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        {
            return Err("invalid exclude workspace id".into());
        }
        for path in self.expected.iter().chain(&self.paths) {
            validate_relative(path)?;
        }
        Ok(())
    }
}
/// One workspace-relative path: no empty / `.` / `..` components, no Git
/// metadata, no separators other than `/`.
pub fn validate_relative(path: &str) -> Result<(), String> {
    let parts: Vec<_> = path.split('/').collect();
    if path.is_empty()
        || path.len() > 4096
        || parts.len() > 64
        || path.contains(['\\', '\0', '\n', '\r'])
        || parts.iter().any(|part| {
            part.is_empty() || matches!(*part, "." | "..") || part.eq_ignore_ascii_case(".git")
        })
    {
        return Err("invalid selected relative path".into());
    }
    Ok(())
}
impl BatchManifest {
    /// Whether a host with only `SelectedContextBatchV1` can take this batch:
    /// writes only, within the original 64-item / 16 MiB caps.
    pub fn fits_legacy(&self) -> bool {
        self.removes.is_empty()
            && self.chmods.is_empty()
            && self.ensure_dirs.is_empty()
            && self.exclude_block.is_none()
            && !self.items.is_empty()
            && self.items.len() <= MAX_ITEMS
            && self.raw_length <= MAX_RAW_BYTES
            && self.encoded_length <= MAX_ENCODED_BYTES
    }
    /// Every operation identity in this batch, in apply order.
    pub fn operation_count(&self) -> usize {
        self.items.len()
            + self.removes.len()
            + self.chmods.len()
            + self.ensure_dirs.len()
            + usize::from(self.exclude_block.is_some())
    }
    pub fn digest(&self) -> Result<[u8; 32], String> {
        Ok(*blake3::hash(&serde_json::to_vec(self).map_err(|e| e.to_string())?).as_bytes())
    }
    pub fn validate(&self) -> Result<(), String> {
        uuid::Uuid::parse_str(&self.request_id).map_err(|_| "invalid selected request UUID")?;
        if self.generation == 0
            || self.operation_count() == 0
            || self.operation_count() > STREAMED_MAX_OPERATIONS
            || self.raw_length > STREAMED_MAX_RAW_BYTES
            || self.encoded_length > STREAMED_MAX_ENCODED_BYTES
        {
            return Err("selected batch exceeds semantic capacity".into());
        }
        if self.codec == Codec::Identity
            && (self.raw_length != self.encoded_length || self.raw_digest != self.encoded_digest)
        {
            return Err("identity payload metadata differs".into());
        }
        if !self.items.is_empty() {
            validate_paths_within(
                self.items.iter().map(|item| item.path.as_str()),
                STREAMED_MAX_OPERATIONS,
            )?;
        }
        let mut indices = HashSet::new();
        let mut ids = HashSet::new();
        let mut targets = HashSet::new();
        for item in &self.items {
            targets.insert(item.path.as_str());
        }
        for (operation_id, path) in self
            .removes
            .iter()
            .map(|item| (&item.operation_id, &item.path))
            .chain(
                self.chmods
                    .iter()
                    .map(|item| (&item.operation_id, &item.path)),
            )
        {
            validate_relative(path)?;
            let id = uuid::Uuid::parse_str(operation_id).map_err(|_| "invalid member UUID")?;
            if !ids.insert(id) {
                return Err("invalid batch member identity, mode or byte range".into());
            }
            if !targets.insert(path.as_str()) {
                return Err("a batch names one path in two file operations".into());
            }
        }
        if self.chmods.iter().any(|item| item.mode & !0o777 != 0) {
            return Err("invalid batch member identity, mode or byte range".into());
        }
        for item in &self.ensure_dirs {
            validate_relative(&item.path)?;
            let id =
                uuid::Uuid::parse_str(&item.operation_id).map_err(|_| "invalid member UUID")?;
            if !ids.insert(id) {
                return Err("invalid batch member identity, mode or byte range".into());
            }
        }
        if let Some(block) = &self.exclude_block {
            block.validate()?;
            let id =
                uuid::Uuid::parse_str(&block.operation_id).map_err(|_| "invalid member UUID")?;
            if !ids.insert(id) {
                return Err("invalid batch member identity, mode or byte range".into());
            }
        }
        let mut offset = 0u64;
        for item in &self.items {
            let operation_id =
                uuid::Uuid::parse_str(&item.operation_id).map_err(|_| "invalid member UUID")?;
            if !indices.insert(item.index)
                || !ids.insert(operation_id)
                || item.mode & !0o777 != 0
                || item.length > crate::remote_files::MAX_FILE_BYTES as u64
                || item.offset != offset
            {
                return Err("invalid batch member identity, mode or byte range".into());
            }
            offset = offset
                .checked_add(item.length)
                .ok_or("batch range overflow")?;
        }
        if offset != self.raw_length {
            return Err("batch ranges do not partition payload".into());
        }
        self.apply_order()?;
        self.validate_targets()
    }
    /// Member positions in an order that runs every dependency first (Kahn;
    /// linear in members plus edges).
    pub fn apply_order(&self) -> Result<Vec<usize>, String> {
        let position: std::collections::HashMap<usize, usize> = self
            .items
            .iter()
            .enumerate()
            .map(|(position, item)| (item.index, position))
            .collect();
        let mut waiting = vec![0usize; self.items.len()];
        let mut dependents = vec![Vec::new(); self.items.len()];
        for (at, item) in self.items.iter().enumerate() {
            for dependency in &item.dependencies {
                let &from = position
                    .get(dependency)
                    .ok_or("batch dependencies missing or cyclic")?;
                waiting[at] += 1;
                dependents[from].push(at);
            }
        }
        let mut ready: Vec<usize> = (0..self.items.len())
            .filter(|at| waiting[*at] == 0)
            .rev()
            .collect();
        let mut order = Vec::with_capacity(self.items.len());
        while let Some(at) = ready.pop() {
            order.push(at);
            for &next in dependents[at].iter().rev() {
                waiting[next] -= 1;
                if waiting[next] == 0 {
                    ready.push(next);
                }
            }
        }
        if order.len() != self.items.len() {
            return Err("batch dependencies missing or cyclic".into());
        }
        Ok(order)
    }
    /// Across every list: paths are NFC; no file is named twice; no file sits
    /// at or above another file or a directory this batch names.
    fn validate_targets(&self) -> Result<(), String> {
        let files: Vec<&str> = self
            .items
            .iter()
            .map(|item| item.path.as_str())
            .chain(self.removes.iter().map(|item| item.path.as_str()))
            .chain(self.chmods.iter().map(|item| item.path.as_str()))
            .collect();
        let directories = self.ensure_dirs.iter().map(|item| item.path.as_str());
        let file_set: HashSet<&str> = files.iter().copied().collect();
        for path in files.iter().copied().chain(directories.clone()) {
            if !crate::remote_manifest::is_nfc(path) {
                return Err("batch paths must be NFC".into());
            }
            let mut ancestor = path;
            while let Some((parent, _)) = ancestor.rsplit_once('/') {
                if file_set.contains(parent) {
                    return Err("a batch names a file as another path's directory".into());
                }
                ancestor = parent;
            }
        }
        for directory in directories {
            if file_set.contains(directory) {
                return Err("a batch names one path as a file and a directory".into());
            }
        }
        Ok(())
    }
}
/// Exact remote names; case remains significant, as in the single-file port.
/// `InspectSelected` keeps its original 64-path bound.
pub fn validate_paths<'a>(paths: impl IntoIterator<Item = &'a str>) -> Result<(), String> {
    validate_paths_within(paths, MAX_ITEMS)
}
/// [`validate_paths`] with an explicit bound. Overlap is checked on sorted
/// paths, so a 20,000-path batch is not quadratic.
pub fn validate_paths_within<'a>(
    paths: impl IntoIterator<Item = &'a str>,
    max: usize,
) -> Result<(), String> {
    let mut paths: Vec<_> = paths.into_iter().collect();
    if paths.is_empty() || paths.len() > max {
        return Err(format!("select 1–{max} exact paths"));
    }
    for path in &paths {
        let parts: Vec<_> = path.split('/').collect();
        if path.len() > 4096
            || parts.len() > 64
            || path.contains(['\\', '\0'])
            || parts.iter().any(|part| {
                part.is_empty() || matches!(*part, "." | "..") || part.eq_ignore_ascii_case(".git")
            })
        {
            return Err("invalid selected relative path".into());
        }
    }
    // Sorted with '/' ordered first, an ancestor directly precedes the first
    // of its descendants, and equal paths are adjacent.
    paths.sort_by(|a, b| a.split('/').cmp(b.split('/')));
    for pair in paths.windows(2) {
        let (path, other) = (pair[0], pair[1]);
        if path == other
            || other
                .strip_prefix(path)
                .is_some_and(|tail| tail.starts_with('/'))
        {
            return Err("selected paths overlap".into());
        }
    }
    Ok(())
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InspectItem {
    pub path: String,
    pub expected_hash: Option<[u8; 32]>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InspectedItem {
    pub path: String,
    pub observation: Observation,
    /// The item's `expected_hash` no longer matches. Reported per
    /// item instead of failing the whole inspection; never set by an older
    /// host, which failed the call instead.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mismatch: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FinishDisposition {
    Settled,
    Abandon,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum BatchOperation {
    InspectSelected {
        items: Vec<InspectItem>,
    },
    Begin {
        manifest: BatchManifest,
    },
    Chunk {
        offset: u64,
        #[serde(with = "chunk_bytes")]
        bytes: Vec<u8>,
    },
    Seal,
    Apply,
    Status,
    Finish {
        disposition: FinishDisposition,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatchRequest {
    pub version: u32,
    pub target: FileTarget,
    pub batch_id: String,
    pub manifest_digest: [u8; 32],
    pub operation: BatchOperation,
}
impl BatchRequest {
    /// Control metadata only; chunk bodies have their own independently bounded codec.
    pub fn control_size(&self) -> Result<usize, String> {
        match &self.operation {
            BatchOperation::Chunk { .. } => serde_json::to_vec(&Self {
                operation: BatchOperation::Chunk {
                    offset: 0,
                    bytes: Vec::new(),
                },
                ..self.clone()
            }),
            _ => serde_json::to_vec(self),
        }
        .map(|bytes| bytes.len())
        .map_err(|e| e.to_string())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TransferPhase {
    Missing,
    Uploading,
    Verified,
    Applying,
    Finished,
    Abandoned,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum MemberOutcome {
    Effect {
        outcome: EffectOutcome,
    },
    Blocked {
        dependencies: Vec<usize>,
    },
    /// Not attempted, for `reason` (the exclude block's
    /// compare-and-swap failed, so no file may be written unexcluded).
    Skipped {
        reason: String,
    },
    /// Larger than one batch member may be
    /// ([`crate::remote_files::MAX_FILE_BYTES`]). Never sent, never written;
    /// the rest of the batch applies.
    TooLarge {
        size: u64,
        limit: u64,
    },
}
/// Which list of a batch an extension result belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OperationKind {
    Remove,
    Chmod,
    EnsureDir,
    ExcludeBlock,
}
/// One result of a batch's extension lists; `index` is the position in its
/// list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationResult {
    pub kind: OperationKind,
    pub index: usize,
    pub operation_id: String,
    pub outcome: MemberOutcome,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MemberResult {
    pub index: usize,
    pub operation_id: String,
    pub outcome: MemberOutcome,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransferStatus {
    pub phase: TransferPhase,
    pub offset: u64,
    pub items: Vec<MemberResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<OperationResult>,
    /// The workspace's exclude block as this batch left it (the paths of its
    /// final write), once that write applied: the `expected` of the next
    /// batch's compare-and-swap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_paths: Option<Vec<String>>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum BatchReply {
    Inspected { items: Vec<InspectedItem> },
    Transfer { status: TransferStatus },
}
pub(crate) mod chunk_bytes {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        if bytes.len() > super::CHUNK_BYTES {
            return Err(serde::ser::Error::custom("batch chunk exceeds limit"));
        }
        serializer.serialize_str(&STANDARD.encode(bytes))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        if encoded.len() > super::CHUNK_BYTES.div_ceil(3) * 4 {
            return Err(serde::de::Error::custom("batch chunk exceeds limit"));
        }
        let bytes = STANDARD.decode(encoded).map_err(serde::de::Error::custom)?;
        if bytes.len() > super::CHUNK_BYTES {
            return Err(serde::de::Error::custom("batch chunk exceeds limit"));
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn manifest() -> BatchManifest {
        BatchManifest {
            request_id: uuid::Uuid::new_v4().to_string(),
            generation: 1,
            codec: Codec::Identity,
            raw_length: 1,
            encoded_length: 1,
            raw_digest: [1; 32],
            encoded_digest: [1; 32],
            items: vec![BatchItem {
                index: 7,
                operation_id: uuid::Uuid::new_v4().to_string(),
                path: "docs/a".into(),
                offset: 0,
                length: 1,
                hash: [1; 32],
                mode: 0o644,
                expected: Expected::Absent,
                dependencies: vec![],
            }],
            removes: vec![],
            chmods: vec![],
            ensure_dirs: vec![],
            exclude_block: None,
        }
    }
    fn id() -> String {
        uuid::Uuid::new_v4().to_string()
    }
    #[test]
    fn typed_manifest_roundtrip_rejects_invalid_ranges_paths_and_dependencies() {
        let good = manifest();
        good.validate().unwrap();
        let restored: BatchManifest =
            serde_json::from_slice(&serde_json::to_vec(&good).unwrap()).unwrap();
        assert_eq!(good, restored);
        assert_eq!(good.digest().unwrap(), restored.digest().unwrap());
        for fault in 0..6 {
            let mut bad = good.clone();
            match fault {
                0 => bad.items[0].offset = 1,
                1 => bad.items[0].path = "../escape".into(),
                2 => bad.items[0].dependencies.push(7),
                3 => bad.items[0].mode = 0o4755,
                4 => bad.raw_length = STREAMED_MAX_RAW_BYTES + 1,
                _ => bad.items.push(bad.items[0].clone()),
            }
            assert!(bad.validate().is_err(), "fault {fault}");
        }
    }
    /// The 64-item / 16 MiB caps are lifted; only the streamed
    /// bounds apply. A legacy-shaped batch keeps its digest.
    #[test]
    fn streamed_batches_lift_the_item_and_byte_caps() {
        let legacy = manifest();
        let legacy_json = serde_json::to_value(&legacy).unwrap();
        assert!(legacy_json.get("removes").is_none());
        assert!(legacy_json.get("excludeBlock").is_none());
        assert!(legacy.fits_legacy());
        let mut big = manifest();
        let member = 1024 * 1024u64;
        big.items = (0..200)
            .map(|index| BatchItem {
                index,
                operation_id: id(),
                path: format!("docs/{index}.bin"),
                offset: index as u64 * member,
                length: member,
                hash: [1; 32],
                mode: 0o644,
                expected: Expected::Absent,
                dependencies: vec![],
            })
            .collect();
        big.raw_length = 200 * member;
        big.encoded_length = 200 * member;
        big.validate().unwrap();
        assert!(
            !big.fits_legacy(),
            "over 64 items / 16 MiB is streamed only"
        );
        let mut over = big.clone();
        over.encoded_length = STREAMED_MAX_ENCODED_BYTES + 1;
        assert!(over.validate().is_err());
    }
    #[test]
    fn extension_lists_validate_identity_paths_and_modes() {
        let mut good = manifest();
        good.removes.push(RemoveItem {
            operation_id: id(),
            path: "old/a.md".into(),
            expected_pre_hash: [2; 32],
        });
        good.chmods.push(ChmodItem {
            operation_id: id(),
            path: "bin/run".into(),
            mode: 0o755,
            expected_pre_hash: [3; 32],
        });
        good.ensure_dirs.push(DirItem {
            operation_id: id(),
            path: "empty/dir".into(),
        });
        good.exclude_block = Some(Box::new(ManagedExcludeBlock {
            operation_id: id(),
            workspace_id: "ws-1".into(),
            expected: vec!["old/a.md".into()],
            paths: vec!["docs/a".into()],
            repair: false,
        }));
        good.validate().unwrap();
        assert!(!good.fits_legacy());
        let back: BatchManifest =
            serde_json::from_slice(&serde_json::to_vec(&good).unwrap()).unwrap();
        assert_eq!(back, good);
        // Only removals: no payload members at all is a valid batch.
        let mut removals = good.clone();
        removals.items.clear();
        removals.raw_length = 0;
        removals.encoded_length = 0;
        removals.validate().unwrap();
        for fault in 0..9 {
            let mut bad = good.clone();
            match fault {
                6 => bad.ensure_dirs[0].path = "docs/a/below-a-file".into(),
                7 => bad.removes[0].path = "cafe\u{301}.md".into(),
                8 => bad.chmods[0].path = "docs/a/x".into(),
                0 => bad.removes[0].path = "docs/a".into(),
                1 => bad.chmods[0].mode = 0o4755,
                2 => bad.ensure_dirs[0].path = "../x".into(),
                3 => bad.exclude_block.as_mut().unwrap().workspace_id = "a b".into(),
                4 => bad.removes[0].operation_id = bad.chmods[0].operation_id.clone(),
                _ => bad.exclude_block.as_mut().unwrap().paths[0] = ".git/x".into(),
            }
            assert!(bad.validate().is_err(), "fault {fault}");
        }
    }
    #[test]
    fn many_paths_overlap_check_is_sorted_not_quadratic() {
        let paths: Vec<String> = (0..20_000).map(|i| format!("d{}/f{i}", i % 97)).collect();
        validate_paths_within(paths.iter().map(String::as_str), 20_000).unwrap();
        let overlap = ["a/b".to_string(), "a".to_string(), "a-b".to_string()];
        assert!(validate_paths_within(overlap.iter().map(String::as_str), 3).is_err());
        let siblings = ["a-b/c".to_string(), "a".to_string(), "a.b".to_string()];
        validate_paths_within(siblings.iter().map(String::as_str), 3).unwrap();
        assert!(
            validate_paths((0..65).map(|_| "x")).is_err(),
            "inspect keeps 64"
        );
    }
    #[test]
    fn textual_uuid_aliases_cannot_name_two_members() {
        let mut candidate = manifest();
        candidate.items[0].operation_id = "abcdefab-cdef-abcd-efab-cdefabcdefab".into();
        let mut alias = candidate.items[0].clone();
        alias.operation_id = alias.operation_id.to_uppercase();
        alias.index = 8;
        alias.path = "docs/b".into();
        alias.offset = 1;
        candidate.items.push(alias);
        candidate.raw_length = 2;
        candidate.encoded_length = 2;
        assert!(candidate.validate().unwrap_err().contains("identity"));
    }
    #[test]
    fn bounded_chunk_roundtrip_fits_frame_and_rejects_oversize() {
        let request = BatchRequest {
            version: VERSION,
            target: FileTarget {
                owner_id: 1,
                origin_key: "origin".into(),
                request_id: "setup".into(),
                handle: "work".into(),
            },
            batch_id: uuid::Uuid::new_v4().to_string(),
            manifest_digest: [0; 32],
            operation: BatchOperation::Chunk {
                offset: 0,
                bytes: vec![1; CHUNK_BYTES],
            },
        };
        let encoded = serde_json::to_vec(&request).unwrap();
        assert!(encoded.len() < 1024 * 1024);
        assert!(request.control_size().unwrap() < MAX_CONTROL_BYTES);
        assert_eq!(
            serde_json::from_slice::<BatchRequest>(&encoded).unwrap(),
            request
        );
        let mut value = serde_json::to_value(&request).unwrap();
        use base64::Engine;
        value["operation"]["bytes"] =
            serde_json::json!(base64::engine::general_purpose::STANDARD.encode(vec![
                0;
                CHUNK_BYTES
                    + 1
            ]));
        assert!(serde_json::from_value::<BatchRequest>(value).is_err());
    }
}
