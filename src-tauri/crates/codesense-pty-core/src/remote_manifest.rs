// SPDX-License-Identifier: MIT
//! One streamed, hash-cached manifest of a remote workspace's files
//! under a set of prefixes. One request frame answers any file count: the
//! sidecar streams [`ManifestPart`]s (progress and entries) on the same `rid`
//! before its final [`ManifestReply`]. Hashes are BLAKE3, like every other
//! remote file observation ([`crate::remote_files::Observation`]).
use crate::remote_files::FileTarget;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

/// At most this many declared prefixes in one request.
pub const MAX_PREFIXES: usize = 32;
/// The largest manifest one request may ask for.
pub const MAX_ENTRIES: usize = 200_000;
/// Entries per streamed part: keeps each frame far below the frame limit.
pub const ENTRIES_PER_PART: usize = 512;
/// At most this many exact paths one manifest may also read.
pub const MAX_READ_PATHS: usize = 20_000;
/// At most this many bytes one manifest may stream back as file
/// contents; a larger publish is refused before anything is read.
pub const MAX_READ_BYTES: u64 = 256 * 1024 * 1024;
/// File bytes per streamed part (as one batch chunk).
pub const READ_CHUNK_BYTES: usize = crate::context_batch::CHUNK_BYTES;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestRequest {
    pub target: FileTarget,
    pub prefixes: Vec<String>,
    pub limit: usize,
    /// Also report this workspace's own exclude block, so Step 2
    /// repairs a dropped or edited block on every launch at no extra call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// `ManifestReadV1`: exact workspace-relative paths, each under
    /// one of `prefixes`, whose bytes are streamed back after the entries
    /// ([`ManifestPart::contents`]). Absent unless non-empty, so an older host
    /// (which denies unknown fields) is never sent it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read: Vec<String>,
}

/// One piece of a file `read` asked for. A file arrives as its
/// pieces in order (`offset` 0 first); `last` closes it. Its hash is the
/// manifest entry's: the sidecar re-checks it while reading, and a file that
/// changed since it was hashed is reported in [`ManifestReply::unread`]
/// instead.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestContent {
    pub path: String,
    pub offset: u64,
    #[serde(with = "crate::context_batch::chunk_bytes")]
    pub bytes: Vec<u8>,
    #[serde(default)]
    pub last: bool,
}

/// A path `read` asked for whose bytes were not sent, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnreadFile {
    pub path: String,
    pub reason: String,
}

/// One workspace's block in the clone's shared `info/exclude`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum WorkspaceExclude {
    /// The paths the block lists (none when there is no block).
    Paths { paths: Vec<String> },
    /// Broken markers or lines this code never writes: only an explicit
    /// Repair replaces it.
    HandEdited { reason: String },
}

/// What a manifest entry is. Only [`EntryKind::File`] carries a hash; every
/// other kind is reported so the diff never touches it, with a reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryKind {
    File,
    /// Reported, never followed.
    Symlink,
    /// A directory holding its own `.git`: a nested repository, skipped whole.
    NestedRepo,
    /// A fifo, socket or device.
    Other,
    /// Hard-linked, or owned by another account: never written.
    Unsafe,
    /// Changed while it was hashed; not cached, never written.
    Unstable,
    /// A name that is not UTF-8, or whose NFC form collides with a sibling's:
    /// fails on its own.
    InvalidName,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestEntry {
    /// Workspace-relative, `/`-separated, NFC.
    pub path: String,
    pub kind: EntryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<[u8; 32]>,
    #[serde(default)]
    pub size: u64,
    /// `st_mode & 0o777`, normalized to `0o755` (executable) or `0o644`.
    #[serde(default)]
    pub mode: u32,
    /// In the workspace's Git index: never written by context sync.
    #[serde(default)]
    pub tracked: bool,
    /// Larger than one batch member may be ([`crate::remote_files::MAX_FILE_BYTES`]):
    /// listed and hashed, but a batch reports it `TooLarge` instead of writing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub oversize: bool,
    /// The name on disk when it is not NFC; `path` is its NFC form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestProgress {
    /// Entries walked so far.
    pub scanned: u64,
    /// Files whose bytes were read and hashed (cache misses).
    pub hashed: u64,
    /// Files answered from the hash cache without reading them.
    pub cache_hits: u64,
    pub hashed_bytes: u64,
}

/// One streamed piece of a manifest answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestPart {
    pub progress: ManifestProgress,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<ManifestEntry>,
    /// Bytes of the files `read` asked for, after every entry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contents: Vec<ManifestContent>,
}

/// The final answer, after every part. `entries` is the count the parts
/// carried, so a lost part cannot pass for a complete manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestReply {
    /// False when the walk stopped early (limit, unreadable directory, index
    /// unavailable). An incomplete manifest fails the diff; removals are never
    /// computed from it.
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incomplete_reason: Option<String>,
    pub entries: u64,
    pub progress: ManifestProgress,
    /// The prefixes as validated and deduplicated by the sidecar.
    pub prefixes: Vec<String>,
    /// The request's workspace block, when it named a workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_exclude: Option<WorkspaceExclude>,
    /// The files `read` asked for that were streamed whole.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read: Vec<String>,
    /// The files `read` asked for whose bytes were not sent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unread: Vec<UnreadFile>,
}

/// Validate and normalize declared prefixes: NFC, relative, no `.`/`..`/empty
/// components or Git metadata, at most [`MAX_PREFIXES`], nested prefixes
/// folded into their ancestor, sorted.
pub fn normalize_prefixes(prefixes: &[String]) -> Result<Vec<String>, String> {
    if prefixes.len() > MAX_PREFIXES {
        return Err(format!("at most {MAX_PREFIXES} context prefixes"));
    }
    let mut normalized = Vec::with_capacity(prefixes.len());
    for prefix in prefixes {
        let value: String = prefix.nfc().collect();
        let value = value.trim_end_matches('/').to_string();
        let parts: Vec<_> = value.split('/').collect();
        if value.is_empty()
            || value.starts_with('/')
            || value.len() > 4096
            || parts.len() > 64
            || value.contains(['\\', '\0'])
            || parts.iter().any(|part| {
                part.is_empty() || matches!(*part, "." | "..") || part.eq_ignore_ascii_case(".git")
            })
        {
            return Err(format!("invalid context prefix {prefix:?}"));
        }
        normalized.push(value);
    }
    normalized.sort();
    normalized.dedup();
    let mut folded: Vec<String> = Vec::with_capacity(normalized.len());
    for prefix in normalized {
        // Sorted order puts an ancestor before its descendants.
        if folded.iter().any(|ancestor| {
            prefix
                .strip_prefix(ancestor.as_str())
                .is_some_and(|tail| tail.starts_with('/'))
        }) {
            continue;
        }
        folded.push(prefix);
    }
    Ok(folded)
}

/// Whether `name` is already NFC.
pub fn is_nfc(name: &str) -> bool {
    unicode_normalization::is_nfc(name)
}
/// `name` in NFC: both sides compare paths in this form.
pub fn nfc(name: &str) -> String {
    name.nfc().collect()
}

/// The executable-or-not normalization of a file mode.
pub fn normalized_mode(st_mode: u32) -> u32 {
    if st_mode & 0o111 != 0 {
        0o755
    } else {
        0o644
    }
}

impl ManifestRequest {
    pub fn validate(&self) -> Result<Vec<String>, String> {
        if self.limit == 0 || self.limit > MAX_ENTRIES {
            return Err("invalid manifest limit".into());
        }
        if self.read.len() > MAX_READ_PATHS {
            return Err(format!(
                "at most {MAX_READ_PATHS} files can be read at once"
            ));
        }
        let prefixes = normalize_prefixes(&self.prefixes)?;
        for path in &self.read {
            let covered = prefixes.iter().any(|prefix| {
                path == prefix
                    || path
                        .strip_prefix(prefix.as_str())
                        .is_some_and(|tail| tail.starts_with('/'))
            });
            if !covered || normalize_prefixes(std::slice::from_ref(path)).is_err() {
                return Err(format!(
                    "read path {path:?} is not under the manifest's prefixes"
                ));
            }
        }
        Ok(prefixes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn prefixes_are_validated_nfc_normalized_and_nested_ones_folded() {
        assert_eq!(
            normalize_prefixes(&strings(&[
                ".claude/skills/a",
                "docs",
                ".claude/skills",
                "docs/",
                "_bmad"
            ]))
            .unwrap(),
            strings(&[".claude/skills", "_bmad", "docs"])
        );
        // NFD "é" is folded to NFC.
        assert_eq!(
            normalize_prefixes(&strings(&["cafe\u{301}"])).unwrap(),
            strings(&["caf\u{e9}"])
        );
        // A sibling that shares a string prefix is not nested.
        assert_eq!(
            normalize_prefixes(&strings(&["doc", "docs"])).unwrap(),
            strings(&["doc", "docs"])
        );
        for bad in [
            "", ".", "..", "/abs", "a/../b", "a//b", ".git", "a/.GIT/x", "a\\b",
        ] {
            assert!(normalize_prefixes(&strings(&[bad])).is_err(), "{bad:?}");
        }
        let many: Vec<String> = (0..=MAX_PREFIXES).map(|i| format!("p{i}")).collect();
        assert!(normalize_prefixes(&many).is_err());
    }

    #[test]
    fn modes_normalize_to_executable_or_not() {
        assert_eq!(normalized_mode(0o100750), 0o755);
        assert_eq!(normalized_mode(0o100600), 0o644);
    }

    /// Without a workspace id the request has no `workspaceId` key,
    /// so a host from before `ManifestWorkspaceExcludeV1` (which denies
    /// unknown fields) still parses it; with one, the field round-trips.
    #[test]
    fn workspace_id_is_absent_unless_named() {
        let target = FileTarget {
            owner_id: 1,
            origin_key: "a".into(),
            request_id: "r".into(),
            handle: "h".into(),
        };
        let mut request = ManifestRequest {
            target,
            prefixes: strings(&["docs"]),
            limit: 10,
            workspace_id: None,
            read: Vec::new(),
        };
        let json = serde_json::to_value(&request).unwrap();
        assert!(json.get("workspaceId").is_none(), "{json}");
        assert!(json.get("read").is_none(), "{json}");
        request.workspace_id = Some("ws".into());
        let back: ManifestRequest =
            serde_json::from_value(serde_json::to_value(&request).unwrap()).unwrap();
        assert_eq!(back, request);
    }

    #[test]
    fn parts_and_replies_round_trip() {
        let part = ManifestPart {
            progress: ManifestProgress {
                scanned: 3,
                hashed: 1,
                cache_hits: 2,
                hashed_bytes: 9,
            },
            entries: vec![ManifestEntry {
                path: "docs/a.md".into(),
                kind: EntryKind::File,
                hash: Some([7; 32]),
                size: 9,
                mode: 0o644,
                tracked: false,
                oversize: false,
                disk_name: None,
                reason: None,
            }],
            contents: Vec::new(),
        };
        let back: ManifestPart =
            serde_json::from_slice(&serde_json::to_vec(&part).unwrap()).unwrap();
        assert_eq!(back, part);
    }

    /// `read` names exact files under the prefixes; anything else is
    /// refused before the walk. Contents round-trip.
    #[test]
    fn read_paths_must_lie_under_the_prefixes() {
        let target = FileTarget {
            owner_id: 1,
            origin_key: "a".into(),
            request_id: "r".into(),
            handle: "h".into(),
        };
        let mut request = ManifestRequest {
            target,
            prefixes: strings(&["docs", ".claude/skills"]),
            limit: 10,
            workspace_id: None,
            read: strings(&["docs/a.md", ".claude/skills/x/SKILL.md"]),
        };
        assert!(request.validate().is_ok());
        for bad in ["other/a.md", "docsx/a.md", "docs/../x", "docs"] {
            request.read = strings(&[bad]);
            if bad == "docs" {
                // A prefix that is itself a file may be read.
                assert!(request.validate().is_ok());
            } else {
                assert!(request.validate().is_err(), "{bad}");
            }
        }
        let content = ManifestContent {
            path: "docs/a.md".into(),
            offset: 0,
            bytes: b"hello".to_vec(),
            last: true,
        };
        let back: ManifestContent =
            serde_json::from_slice(&serde_json::to_vec(&content).unwrap()).unwrap();
        assert_eq!(back, content);
    }
}
