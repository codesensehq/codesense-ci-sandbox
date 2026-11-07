// SPDX-License-Identifier: MIT
//! Bounded workspace preparation over the admitted host connection.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrepareWorkspace {
    pub request_id: String,
    pub owner_id: u64,
    pub origin_key: String,
    pub repository_key: String,
    pub clone_url: String,
    pub repo_path: String,
    pub worktree_path: String,
    pub base_ref: String,
    pub branch: String,
    pub bundle: GitBundle,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreparedWorkspace {
    pub status: String,
    pub handle: Option<String>,
    pub cwd: Option<String>,
    pub repo_path: Option<String>,
    pub repository_key: Option<String>,
    pub resolved_commit: Option<String>,
    pub branch: Option<String>,
    pub message: Option<String>,
}
impl PreparedWorkspace {
    pub fn failure(status: &str, message: impl Into<String>) -> Self {
        Self {
            status: status.into(),
            handle: None,
            cwd: None,
            repo_path: None,
            repository_key: None,
            resolved_commit: None,
            branch: None,
            message: Some(message.into()),
        }
    }
}

/// Committed Git data only. Authentication material is never part of this payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitBundle {
    pub digest: String,
    pub size: u64,
    pub commit: String,
}
pub const MAX_BUNDLE_BYTES: u64 = 64 * 1024 * 1024 * 1024;
pub const BUNDLE_CHUNK_BYTES: usize = 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleChunk {
    pub request: PrepareWorkspace,
    pub offset: u64,
    #[serde(with = "chunk_encoding")]
    pub bytes: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleProgress {
    pub next_offset: u64,
}

/// The pipelined bundle upload. Every chunk but the last is exactly
/// this long, so a chunk's index names its byte range.
pub const PIPELINED_CHUNK_BYTES: usize = 256 * 1024;
/// The smallest chunk a machine may announce; bounds the bitmap's size.
pub const MIN_PIPELINED_CHUNK_BYTES: usize = 64 * 1024;
/// How many chunks are in flight at once.
pub const UPLOAD_WINDOW: usize = 8;

/// Which chunks of one bundle upload the machine holds: a bitmap over
/// `count` chunk indices (bit `i % 8` of byte `i / 8`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReceivedChunks {
    count: u32,
    #[serde(with = "bitmap_encoding")]
    bits: Vec<u8>,
}
impl ReceivedChunks {
    /// No chunk of a `size`-byte bundle yet.
    pub fn for_size(size: u64) -> Self {
        Self::for_chunks(size, PIPELINED_CHUNK_BYTES)
    }
    /// No chunk of a `size`-byte bundle cut into `chunk_bytes` chunks yet.
    pub fn for_chunks(size: u64, chunk_bytes: usize) -> Self {
        let count = size.div_ceil(chunk_bytes.max(1) as u64) as u32;
        Self {
            count,
            bits: vec![0; (count as usize).div_ceil(8)],
        }
    }
    pub fn count(&self) -> u32 {
        self.count
    }
    pub fn contains(&self, index: u32) -> bool {
        index < self.count
            && self
                .bits
                .get(index as usize / 8)
                .is_some_and(|byte| byte & (1 << (index % 8)) != 0)
    }
    pub fn insert(&mut self, index: u32) {
        if index < self.count {
            if let Some(byte) = self.bits.get_mut(index as usize / 8) {
                *byte |= 1 << (index % 8);
            }
        }
    }
    pub fn received(&self) -> u32 {
        (0..self.count).filter(|i| self.contains(*i)).count() as u32
    }
    pub fn is_complete(&self) -> bool {
        self.received() == self.count
    }
    /// The indices still to send, in order.
    pub fn missing(&self) -> Vec<u32> {
        (0..self.count).filter(|i| !self.contains(*i)).collect()
    }
    /// Whether this map describes a `size`-byte bundle.
    pub fn fits(&self, size: u64) -> bool {
        self.fits_chunks(size, PIPELINED_CHUNK_BYTES)
    }
    /// Whether this map describes a `size`-byte bundle in `chunk_bytes` chunks.
    pub fn fits_chunks(&self, size: u64, chunk_bytes: usize) -> bool {
        let expected = Self::for_chunks(size, chunk_bytes);
        self.count == expected.count && self.bits.len() == expected.bits.len()
    }
    /// The bytes these chunks hold of a `size`-byte bundle.
    pub fn held_bytes(&self, size: u64, chunk_bytes: usize) -> u64 {
        (0..self.count)
            .filter(|i| self.contains(*i))
            .filter_map(|i| chunk_range_in(size, i, chunk_bytes))
            .map(|(_, len)| len as u64)
            .sum()
    }
}

/// The byte range of chunk `index` of a `size`-byte bundle.
pub fn chunk_range(size: u64, index: u32) -> Option<(u64, usize)> {
    chunk_range_in(size, index, PIPELINED_CHUNK_BYTES)
}

/// The byte range of chunk `index` of a `size`-byte bundle cut into
/// `chunk_bytes` chunks.
pub fn chunk_range_in(size: u64, index: u32, chunk_bytes: usize) -> Option<(u64, usize)> {
    if chunk_bytes == 0 {
        return None;
    }
    let start = (index as u64).checked_mul(chunk_bytes as u64)?;
    if start >= size {
        return None;
    }
    Some((start, (size - start).min(chunk_bytes as u64) as usize))
}

/// What the machine holds of one pipelined upload. After a server restart
/// this is recomputed from the staged bytes, never from earlier acks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleUploadStatus {
    pub chunk_bytes: u32,
    pub received: ReceivedChunks,
    /// Sealed: complete, flushed once, and its digest verified.
    pub sealed: bool,
}

/// One chunk of a pipelined upload, named by index and carrying its own hash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StagedBundleChunk {
    pub request: PrepareWorkspace,
    pub index: u32,
    /// Lowercase hex SHA-256 of `bytes`.
    pub sha256: String,
    #[serde(with = "chunk_encoding")]
    pub bytes: Vec<u8>,
}

/// Move a prepared workspace to a newer base commit. `upload` is the
/// bundle carrier: its request id names the staged delta, its paths and
/// identity must equal the prepared workspace's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateWorkspaceBase {
    pub target: crate::remote_files::FileTarget,
    pub upload: PrepareWorkspace,
    /// Only check that the worktree could move (clean, on its branch); the
    /// bundle need not be uploaded yet.
    #[serde(default)]
    pub precheck: bool,
    /// Remove managed copies that the new base now tracks instead of refusing.
    #[serde(default)]
    pub remove_managed_copies: bool,
    /// The desktop's managed records for this workspace: what CodeSense last
    /// wrote at each path. A copy whose bytes differ was edited there.
    #[serde(default)]
    pub managed: Vec<ManagedCopy>,
    /// Earlier update ids of this workspace the desktop abandoned (the base
    /// moved on): their staging is reclaimed.
    #[serde(default)]
    pub discard_uploads: Vec<String>,
}

/// One managed record: a workspace-relative path and the BLAKE3 (hex) of the
/// bytes CodeSense last wrote there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagedCopy {
    pub path: String,
    pub blake3: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceBaseUpdate {
    /// `ready` (precheck passed), `updated`, `upToDate`, `refusedDirty`,
    /// `refusedDiverged`, `refusedManagedOverlap` or `refused`.
    pub status: String,
    pub previous_commit: Option<String>,
    pub commit: Option<String>,
    /// Managed copies the new base tracks (refused) or that were removed.
    #[serde(default)]
    pub managed_overlap: Vec<String>,
    /// Managed copies the new base tracks that were edited in the workspace:
    /// never removed; publish or discard them first.
    #[serde(default)]
    pub modified_managed: Vec<String>,
    /// Ignored files the new base tracks that CodeSense did not place: never
    /// overwritten.
    #[serde(default)]
    pub unmanaged_overlap: Vec<String>,
    /// The worktree's changes when it is dirty (bounded).
    #[serde(default)]
    pub dirty: Vec<String>,
    pub message: Option<String>,
}

/// The sender side of the pipelined upload, independent of the wire.
pub mod pipeline {
    use super::ReceivedChunks;

    /// A link that carries chunks and their acknowledgements. Acks may come
    /// back in any order.
    pub trait ChunkLink {
        /// Put chunk `index` on the wire without waiting for its ack.
        fn submit(&mut self, index: u32, bytes: Vec<u8>) -> Result<(), String>;
        /// Wait for the next acknowledgement; returns the chunk index.
        fn next_ack(&mut self) -> Result<u32, String>;
    }

    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Sent {
        pub chunks: u32,
        pub max_in_flight: usize,
    }

    /// Send every chunk `received` lacks, keeping up to `window` in flight,
    /// and mark each one as its ack arrives. `progress` gets the bytes the
    /// machine holds after each ack.
    #[allow(clippy::too_many_arguments)]
    pub fn send_missing(
        transport: &mut dyn ChunkLink,
        received: &mut ReceivedChunks,
        size: u64,
        chunk_bytes: usize,
        window: usize,
        read: &mut dyn FnMut(u32) -> Result<Vec<u8>, String>,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<Sent, String> {
        if !received.fits_chunks(size, chunk_bytes) || window == 0 {
            return Err("bundle upload status does not match the bundle".into());
        }
        // A running total: each ack adds its own chunk, never a rescan.
        let mut held = received.held_bytes(size, chunk_bytes);
        let mut queue = received.missing().into_iter();
        let mut in_flight = std::collections::BTreeSet::new();
        let mut sent = Sent::default();
        loop {
            while in_flight.len() < window {
                let Some(index) = queue.next() else { break };
                let bytes = read(index)?;
                transport.submit(index, bytes)?;
                in_flight.insert(index);
                sent.chunks += 1;
                sent.max_in_flight = sent.max_in_flight.max(in_flight.len());
            }
            if in_flight.is_empty() {
                break;
            }
            let index = transport.next_ack()?;
            if !in_flight.remove(&index) {
                return Err("the machine acknowledged a chunk that was not sent".into());
            }
            received.insert(index);
            held +=
                super::chunk_range_in(size, index, chunk_bytes).map_or(0, |(_, len)| len as u64);
            progress(held, size);
        }
        if !received.is_complete() {
            return Err("bundle upload ended incomplete; resume original request".into());
        }
        Ok(sent)
    }
}

pub(crate) mod bitmap_encoding {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde::{Deserialize, Deserializer, Serializer};
    /// 64 GiB in the smallest chunks a machine may announce.
    const MAX_BYTES: usize =
        (super::MAX_BUNDLE_BYTES / super::MIN_PIPELINED_CHUNK_BYTES as u64 / 8) as usize;
    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        if encoded.len() > MAX_BYTES.div_ceil(3) * 4 {
            return Err(serde::de::Error::custom("chunk bitmap exceeds limit"));
        }
        STANDARD.decode(encoded).map_err(serde::de::Error::custom)
    }
}

/// Read-only knowledge from the exact enrolled daemon's managed repository.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InspectRepository {
    pub owner_id: u64,
    pub repository_key: String,
    pub clone_url: String,
    pub repo_path: String,
    pub commit: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepositoryKnowledge {
    pub has_commit: bool,
    pub imported_commits: Vec<String>,
}

/// Ask the enrolled daemon to tear down a managed worktree on this
/// machine. Idempotent: a missing worktree returns `removed: false` + a note,
/// NOT an error. The remote-side handler takes the per-workspace lock from
/// `locks::workspace`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoveWorktree {
    /// Architecture: deletion must still name the reported source commit.
    pub expected_tip: String,
    /// Policy: only the caller can authorize discarding a dirty checkout.
    #[serde(default)]
    pub discard_uncommitted: bool,
    pub owner_id: u64,
    pub repository_key: String,
    pub repo_path: String,
    pub worktree_path: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoveWorktreeResult {
    pub removed: bool,
    pub note: Option<String>,
}

/// Ask the enrolled daemon whether a managed worktree
/// on this machine has uncommitted changes. Mirrors the desktop-side
/// `check_worktree_dirty` body on the remote so a remote → local move can
/// honour `dirty_worktree` pre-flight refusal without the desktop needing a
/// mounted copy of the remote tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CheckWorktreeDirtyRemote {
    /// Policy: refresh remote-tracking refs for a move report; cleanup only reads.
    #[serde(default)]
    pub refresh_refs: bool,
    pub owner_id: u64,
    pub repository_key: String,
    pub repo_path: String,
    pub worktree_path: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteWorktreeDirtyStatus {
    /// Architecture: the source tip and its commits absent from all remote refs.
    #[serde(default)]
    pub source_tip: String,
    #[serde(default)]
    pub unpushed_count: usize,
    #[serde(default)]
    pub fetch_error: Option<String>,
    pub dirty: bool,
    /// `git status --porcelain` paths, truncated to a bounded count so one
    /// pathological working tree can't blow the frame cap.
    pub paths: Vec<String>,
}

/// Max paths carried on the wire for `RemoteWorktreeDirtyStatus`. Enough to
/// name the first few offenders; the UI shows "… and N more" past this cap.
pub const DIRTY_PATHS_WIRE_CAP: usize = 32;
pub(crate) mod chunk_encoding {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        if encoded.len() > super::BUNDLE_CHUNK_BYTES.div_ceil(3) * 4 {
            return Err(serde::de::Error::custom("bundle chunk exceeds limit"));
        }
        let bytes = STANDARD.decode(encoded).map_err(serde::de::Error::custom)?;
        if bytes.len() > super::BUNDLE_CHUNK_BYTES {
            return Err(serde::de::Error::custom("bundle chunk exceeds limit"));
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Serialize, Deserialize)]
    struct ChunkOnly {
        #[serde(with = "chunk_encoding")]
        bytes: Vec<u8>,
    }
    #[test]
    fn base64_chunk_roundtrip_fits_control_frame_and_rejects_oversize() {
        let wire = serde_json::to_vec(&ChunkOnly {
            bytes: vec![255; BUNDLE_CHUNK_BYTES],
        })
        .unwrap();
        assert!(wire.len() < 2 * 1024 * 1024);
        assert_eq!(
            serde_json::from_slice::<ChunkOnly>(&wire)
                .unwrap()
                .bytes
                .len(),
            BUNDLE_CHUNK_BYTES
        );
        let oversized = serde_json::to_vec(&ChunkOnly {
            bytes: vec![255; BUNDLE_CHUNK_BYTES + 1],
        })
        .unwrap();
        assert!(serde_json::from_slice::<ChunkOnly>(&oversized).is_err());
    }
}

/// Stable prefixes carried by filesystem failures across the terminal wire.
pub const WORKSPACE_MISSING: &str = "REMOTE_WORKSPACE_MISSING:";
pub const WORKSPACE_UNSAFE: &str = "REMOTE_WORKSPACE_UNSAFE:";
pub const WORKSPACE_OCCUPIED: &str = "REMOTE_WORKSPACE_OCCUPIED:";

/// Forget only the stale ownership of a completed workspace whose folder is gone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForgetMissingWorkspace {
    pub target: crate::remote_files::FileTarget,
}

/// Architecture: one cross-process admission fence covers app and sidecar launches.
/// Pending launches hold shared locks until registration completes. Removal takes
/// the worktree's exclusive lock before reading sessions; neither can pass the other.
#[cfg(unix)]
pub mod removal {
    use std::fs::{File, OpenOptions};
    use std::os::fd::AsRawFd;
    use std::os::unix::{ffi::OsStrExt, fs::OpenOptionsExt};
    use std::path::Path;

    pub struct Admission(Vec<File>);

    fn lock(path: &Path, exclusive: bool) -> Result<File, String> {
        let path = path
            .canonicalize()
            .map_err(|e| format!("workspace admission: {e}"))?;
        let key = blake3::hash(path.as_os_str().as_bytes());
        // Architecture: a stable host-local inode shares the fence across the Mac
        // app and its sidecar. Never unlink these files while another host may use them.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .open(format!("/tmp/codesense-worktree-{}-{key}.lock", unsafe {
                libc::geteuid()
            }))
            .map_err(|e| format!("workspace admission: {e}"))?;
        let mode = if exclusive {
            libc::LOCK_EX
        } else {
            libc::LOCK_SH
        };
        if unsafe { libc::flock(file.as_raw_fd(), mode | libc::LOCK_NB) } != 0 {
            return Err(if exclusive {
                "runningAgents: a launch is pending or removal is active; source retained"
            } else {
                "spawn refused: workspace is removing"
            }
            .into());
        }
        Ok(file)
    }

    pub fn admit_spawn(cwd: &str) -> Result<Admission, String> {
        // Keep the existing missing-cwd behavior; no child can enter a removed directory.
        let path = Path::new(cwd);
        if !path.is_dir() {
            return Ok(Admission(Vec::new()));
        }
        let path = path.canonicalize().map_err(|e| e.to_string())?;
        path.ancestors()
            .map(|p| lock(p, false))
            .collect::<Result<Vec<_>, _>>()
            .map(Admission)
    }

    pub fn begin_removal(path: &str) -> Result<Admission, String> {
        // Architecture: this exclusive workspace lock marks it removing, counts
        // admitted-but-unregistered launches, and clears itself on every return.
        if !Path::new(path).exists() {
            return Ok(Admission(Vec::new()));
        }
        lock(Path::new(path), true).map(|file| Admission(vec![file]))
    }

    impl Drop for Admission {
        fn drop(&mut self) {
            for file in &self.0 {
                unsafe {
                    libc::flock(file.as_raw_fd(), libc::LOCK_UN);
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn pending_launch_and_removal_cannot_both_proceed() {
            let dir = tempfile::tempdir().unwrap();
            let nested = dir.path().join("nested");
            std::fs::create_dir(&nested).unwrap();
            let cwd = nested.to_str().unwrap();
            let worktree = dir.path().to_str().unwrap();
            let pending = admit_spawn(cwd).unwrap();
            assert!(begin_removal(worktree).is_err());
            drop(pending);
            let removing = begin_removal(worktree).unwrap();
            assert!(admit_spawn(cwd).is_err());
            drop(removing);
            assert!(admit_spawn(cwd).is_ok());
        }
    }
}
