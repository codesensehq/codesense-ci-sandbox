// SPDX-License-Identifier: MIT
//! `~/.codesense/workspace-chat/index.json` — the registry of known workspace
//! chats (archive flag, display name, head id).
//!
//! The index is a **cache**, not authority. The authoritative record is the
//! log file (`chats/<slug>.jsonl`); the index is rebuilt from log tail scans
//! when it is missing or corrupt. Writes go through
//! `shared::json_io::write_json_atomic` (tmp-then-rename, same discipline as
//! the workspace registry at `~/.codesense/workspace-registry.json`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::shared::json_io::{read_json, write_json_atomic, JsonIoError};

/// Current on-disk schema version.
pub(crate) const SCHEMA_VERSION: u32 = 1;

/// The directory under `codesense_home` the engine owns.
pub(crate) const CHAT_ROOT_DIRNAME: &str = "workspace-chat";

/// The chats subdirectory inside the root.
pub(crate) const CHATS_DIRNAME: &str = "chats";

/// The index filename inside the root.
pub(crate) const INDEX_FILENAME: &str = "index.json";

/// A workspace's row in the index.
///
/// `archived_at_ms` here is independent of SW-WC-1's `workspace-registry.json`
/// archive flag: both exist by design (the chat engine owns its own
/// read-authoritative cache so a chat list render never has to also open the
/// workspace registry). Writes to this field are only made by the chat engine
/// itself; the admission check that gates `workspace_chat_send` reads SW-WC-1's
/// registry state as the source of truth (per design doc §10 and AC-7). The
/// chat-local flag is kept so the index can carry an "archived message was
/// stamped at this time" record without having to touch the SW-WC-1 registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatIndexEntry {
    pub workspace_id: String,
    pub slug: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub created_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message_at_ms: Option<i64>,
    /// Monotonic last message id. `0` when the log is empty.
    pub head_id: i64,
    /// Chat-local archive stamp. The admission check reads SW-WC-1's registry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at_ms: Option<i64>,
}

/// The on-disk index document.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PersistedIndex {
    pub version: u32,
    pub workspaces: HashMap<String, ChatIndexEntry>,
}

impl Default for PersistedIndex {
    fn default() -> Self {
        PersistedIndex {
            version: SCHEMA_VERSION,
            workspaces: HashMap::new(),
        }
    }
}

/// The chat engine's root directory under `codesense_home`.
pub(crate) fn chat_root(codesense_home: &Path) -> PathBuf {
    codesense_home.join(CHAT_ROOT_DIRNAME)
}

/// The absolute path to the chats directory.
pub(crate) fn chats_dir(codesense_home: &Path) -> PathBuf {
    chat_root(codesense_home).join(CHATS_DIRNAME)
}

/// The absolute path to the index file.
pub(crate) fn index_path(codesense_home: &Path) -> PathBuf {
    chat_root(codesense_home).join(INDEX_FILENAME)
}

/// The absolute path to a workspace's log file.
pub(crate) fn log_path(codesense_home: &Path, slug: &str) -> PathBuf {
    chats_dir(codesense_home).join(format!("{slug}.jsonl"))
}

/// A path-safe, cross-platform slug for a workspace id. BLAKE3-16 hex (32
/// chars): short, collision-resistant for the expected cardinality (tens of
/// workspaces), and never leaks path separators the way a raw workspace id
/// would.
pub fn workspace_slug(workspace_id: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(workspace_id.as_bytes());
    let digest = hasher.finalize();
    let bytes = digest.as_bytes();
    let mut out = String::with_capacity(32);
    for b in bytes.iter().take(16) {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Load the index from disk. A missing or wrong-version file is logged and
/// treated as empty — the index is a cache, so a corrupt one must never fail
/// app boot (matches `workspace_registry::load_registry`).
pub(crate) fn load_index(path: &Path) -> PersistedIndex {
    match read_json::<PersistedIndex>(path) {
        Ok(idx) if idx.version == SCHEMA_VERSION => idx,
        Ok(other) => {
            log::warn!(
                target: "workspace_chat",
                "chat index at {} has unsupported version {} (expected {}); starting empty",
                path.display(),
                other.version,
                SCHEMA_VERSION,
            );
            PersistedIndex::default()
        }
        Err(JsonIoError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            PersistedIndex::default()
        }
        Err(error) => {
            log::warn!(
                target: "workspace_chat",
                "chat index at {} could not be read ({error}); starting empty",
                path.display(),
            );
            PersistedIndex::default()
        }
    }
}

/// Write `index` atomically.
pub(crate) fn write_index(path: &Path, index: &PersistedIndex) -> Result<(), JsonIoError> {
    write_json_atomic(path, index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_is_stable_and_32_hex_chars() {
        let a = workspace_slug("story:ex-1");
        let b = workspace_slug("story:ex-1");
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn slug_differs_across_distinct_workspace_ids() {
        let a = workspace_slug("story:a");
        let b = workspace_slug("story:b");
        assert_ne!(a, b);
    }

    #[test]
    fn roundtrip_through_disk_preserves_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("index.json");
        let mut idx = PersistedIndex::default();
        idx.workspaces.insert(
            "story:a".to_string(),
            ChatIndexEntry {
                workspace_id: "story:a".to_string(),
                slug: workspace_slug("story:a"),
                display_name: Some("Story A".into()),
                created_at_ms: 1,
                last_message_at_ms: Some(2),
                head_id: 7,
                archived_at_ms: None,
            },
        );
        write_index(&path, &idx).unwrap();
        let back = load_index(&path);
        assert_eq!(back.version, SCHEMA_VERSION);
        assert_eq!(back.workspaces.len(), 1);
        assert_eq!(back.workspaces["story:a"].head_id, 7);
        assert_eq!(
            back.workspaces["story:a"].slug,
            idx.workspaces["story:a"].slug
        );
    }

    #[test]
    fn missing_file_loads_as_default_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("missing.json");
        let back = load_index(&path);
        assert_eq!(back.version, SCHEMA_VERSION);
        assert!(back.workspaces.is_empty());
    }

    #[test]
    fn corrupt_file_loads_as_default_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bad.json");
        std::fs::write(&path, "not json").unwrap();
        let back = load_index(&path);
        assert!(back.workspaces.is_empty());
    }
}
