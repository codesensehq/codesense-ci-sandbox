// SPDX-License-Identifier: MIT
//! SW-CP-24: the path-keyed **merge-mark store**.
//!
//! A team-synced list at `{org_root}/{team}/config/merge-marked.yaml` naming the
//! content paths that project via structured deep-merge (RFC 7386 objects + SMP
//! keyed arrays) instead of the whole-file clobber. **Path-keyed** — a flat list
//! of content-relative paths — so it can mark a file **nested inside a whole-root
//! dir** (`.claude/settings.json`), which a per-file `projections.yml` entry
//! could not reach.
//!
//! Readers: the projection engine, loaded **once per wave** (never per file).
//! Writer (the set/clear seam): CP-27's right-click toggle. This module owns both
//! halves — [`load_merge_marks_from`] (read) and
//! [`set_merge_mark`] / [`clear_merge_mark`] (atomic read-modify-write).
//!
//! **Key contract (writers MUST honor):** a mark key is the **content-relative
//! `logical_name`** the projection engine matches against — i.e. the path
//! relative to the layer's `content/` root, INCLUDING any `source_subdir` prefix
//! (`story_linker`'s `logical_name`), forward-slashed. For the whole-root cases
//! (`.mcp.json`, `.claude/settings.json`) that is just the repo-relative path.
//! Every writer must compute the identical string, or `is_marked`
//! misses and the file silently clones (clobbers) instead of merging.
//!
//! Tolerance mirrors `sprint_registry`: a missing store is the greenfield case
//! (no marks), and a malformed file — or one malformed entry — is logged and
//! skipped rather than erroring, so a hand-corrupted store can never wedge the
//! projection engine.

use std::collections::BTreeMap;
use std::path::Path;

use log::{debug, warn};
use serde::{Deserialize, Serialize};

use crate::shared::config_format::ConfigFormat;
use crate::shared::yaml_io::{write_yaml_atomic, YamlIoError};

/// The single app-managed store filename under `{org_root}/{team}/config/`.
const MERGE_MARKS_FILENAME: &str = "merge-marked.yaml";

/// Top-level YAML key holding the path→entry map. Wrapping under a named key
/// leaves room for future store-level keys without a breaking migration.
const LEDGER_MARKS_KEY: &str = "marks";

/// One marked path's on-disk entry. The content-relative `path` is the map key
/// (never duplicated in the value). `array_keys` optionally declares SMP merge
/// keys per array target (dot-joined object path → key field, e.g.
/// `hooks.Notification` → `command`); an absent/empty map merges every array by
/// the RFC 7386 default (replace).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct MarkEntry {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    array_keys: BTreeMap<String, String>,
}

/// The typed on-disk ledger, used for **writing** (deterministic key order via
/// `BTreeMap`). Borrows the marks map so the atomic write never clones it. Reads
/// go through a lenient `serde_yaml::Value` walk so a single malformed entry
/// never fails the whole load.
#[derive(Debug, Serialize)]
struct MarkLedgerWrite<'a> {
    marks: &'a BTreeMap<String, MarkEntry>,
}

/// The resolved store: content-relative path → its array-key policy. A path
/// present here projects via merge. Membership query is O(log n) by path.
/// A malformed store or entry is logged (`warn!`) and yields an empty/partial
/// set — it never wedges projection.
#[derive(Debug, Clone, Default)]
pub struct MergeMarks {
    marks: BTreeMap<String, MarkEntry>,
}

impl MergeMarks {
    /// Is `logical_path` (content-relative, forward-slashed) merge-marked?
    /// O(log n), and a trivial `false` on the common no-marks team.
    pub fn is_marked(&self, logical_path: &str) -> bool {
        self.marks.contains_key(logical_path)
    }

    /// Every marked content-relative path, in stable (sorted) order. The CP-27
    /// read seam: the FE loads this once per team into a set that drives both the
    /// in-tree merge badge and the Mark/Unmark menu label. Deterministic order
    /// (`BTreeMap` keys) keeps the FE render + tests stable.
    pub fn marked_paths(&self) -> Vec<String> {
        self.marks.keys().cloned().collect()
    }

    /// SW-XS-14: the declared array keys for a marked path (dot path → key
    /// field); empty for an unmarked path or one with no keys. Carried on the
    /// launch's generated entry so the workspace merge applies the same policy.
    pub fn array_keys(&self, logical_path: &str) -> BTreeMap<String, String> {
        self.marks
            .get(logical_path)
            .map(|e| e.array_keys.clone())
            .unwrap_or_default()
    }
}

/// Load the merge-mark store from a team `config/` dir (reads
/// `config_dir/merge-marked.yaml`). Pure over the filesystem (no app/Tauri
/// state) so it is directly unit-testable. Never errors — an unreadable or
/// malformed store (or entry) is `warn!`-logged and yields an empty/partial set,
/// so a hand-corrupted store can never wedge projection.
pub fn load_merge_marks_from(config_dir: &Path) -> MergeMarks {
    let path = config_dir.join(MERGE_MARKS_FILENAME);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            debug!(
                target: "merge_marks",
                "no merge-mark store at {} — no marks", path.display()
            );
            return MergeMarks::default();
        }
        Err(e) => {
            warn!(
                target: "merge_marks",
                "cannot read {}: {e} — no marks", path.display()
            );
            return MergeMarks::default();
        }
    };

    if text.trim().is_empty() {
        return MergeMarks::default();
    }

    let doc: serde_yaml::Value = match serde_yaml::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            warn!(
                target: "merge_marks",
                "malformed merge-mark store {}: {e} — no marks", path.display()
            );
            return MergeMarks::default();
        }
    };

    let mut marks: BTreeMap<String, MarkEntry> = BTreeMap::new();

    match doc.get(LEDGER_MARKS_KEY) {
        Some(serde_yaml::Value::Mapping(map)) => {
            for (k, v) in map {
                let Some(pathkey) = k.as_str() else {
                    warn!(target: "merge_marks", "skipping merge mark with non-string path");
                    continue;
                };
                match serde_yaml::from_value::<MarkEntry>(v.clone()) {
                    Ok(entry) => {
                        marks.insert(pathkey.to_string(), entry);
                    }
                    Err(e) => {
                        warn!(target: "merge_marks", "skipping malformed merge mark '{pathkey}': {e}");
                    }
                }
            }
        }
        Some(_) => {
            warn!(target: "merge_marks", "merge-mark `{LEDGER_MARKS_KEY}` is not a mapping — ignored");
        }
        None => {}
    }

    MergeMarks { marks }
}

/// Failure of the set/clear seam.
#[derive(Debug)]
pub enum MarkError {
    /// The path is not a structured (JSON/JSONC/YAML/TOML) file — a merge mark
    /// on it is rejected (AC-7), never silently clobbered.
    Unsupported(String),
    Io(YamlIoError),
}

impl std::fmt::Display for MarkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MarkError::Unsupported(p) => {
                write!(f, "cannot merge-mark non-structured file: {p}")
            }
            MarkError::Io(e) => write!(f, "merge-mark store write: {e}"),
        }
    }
}

impl std::error::Error for MarkError {}

/// Mark `logical_path` (content-relative) for structured merge, with optional
/// declared `array_keys`. Rejects a non-structured path so a bad mark never
/// reaches materialize. Idempotent (re-marking updates the array keys). Atomic
/// read-modify-write via [`write_yaml_atomic`].
pub fn set_merge_mark(
    config_dir: &Path,
    logical_path: &str,
    array_keys: BTreeMap<String, String>,
) -> Result<(), MarkError> {
    if ConfigFormat::detect(Path::new(logical_path)).is_none() {
        return Err(MarkError::Unsupported(logical_path.to_string()));
    }
    let mut store = load_merge_marks_from(config_dir);
    store
        .marks
        .insert(logical_path.to_string(), MarkEntry { array_keys });
    save_merge_marks_to(config_dir, &store.marks).map_err(MarkError::Io)
}

/// Clear a merge mark (idempotent — clearing an unmarked path is `Ok`, no write).
pub fn clear_merge_mark(config_dir: &Path, logical_path: &str) -> Result<(), MarkError> {
    let mut store = load_merge_marks_from(config_dir);
    if store.marks.remove(logical_path).is_some() {
        save_merge_marks_to(config_dir, &store.marks).map_err(MarkError::Io)?;
    }
    Ok(())
}

fn save_merge_marks_to(
    config_dir: &Path,
    marks: &BTreeMap<String, MarkEntry>,
) -> Result<(), YamlIoError> {
    write_yaml_atomic(
        &config_dir.join(MERGE_MARKS_FILENAME),
        &MarkLedgerWrite { marks },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn missing_store_is_empty_not_error() {
        let dir = tempdir().unwrap();
        let store = load_merge_marks_from(dir.path());
        assert!(store.marks.is_empty());
    }

    #[test]
    fn set_then_load_marks_the_path() {
        let dir = tempdir().unwrap();
        set_merge_mark(dir.path(), ".mcp.json", BTreeMap::new()).unwrap();
        let store = load_merge_marks_from(dir.path());
        assert!(store.is_marked(".mcp.json"));
        assert!(!store.is_marked("other.json"));
    }

    #[test]
    fn reaches_a_path_nested_in_a_whole_root_dir() {
        // AC-6: the whole reason for a path-keyed store — mark a file nested
        // inside a whole-root dir, which a projections.yml entry cannot.
        let dir = tempdir().unwrap();
        set_merge_mark(dir.path(), ".claude/settings.json", BTreeMap::new()).unwrap();
        let store = load_merge_marks_from(dir.path());
        assert!(store.is_marked(".claude/settings.json"));
    }

    #[test]
    fn clear_removes_the_mark_and_is_idempotent() {
        let dir = tempdir().unwrap();
        set_merge_mark(dir.path(), ".mcp.json", BTreeMap::new()).unwrap();
        clear_merge_mark(dir.path(), ".mcp.json").unwrap();
        assert!(!load_merge_marks_from(dir.path()).is_marked(".mcp.json"));
        // Clearing an already-absent mark is Ok (no write, no error).
        clear_merge_mark(dir.path(), ".mcp.json").unwrap();
    }

    #[test]
    fn marked_paths_lists_every_mark_in_stable_order() {
        // CP-27 read seam: the FE loads the full mark set once per team to drive
        // the tree badge + the Mark/Unmark label. Order is deterministic (BTreeMap).
        let dir = tempdir().unwrap();
        set_merge_mark(dir.path(), ".mcp.json", BTreeMap::new()).unwrap();
        set_merge_mark(dir.path(), ".claude/settings.json", BTreeMap::new()).unwrap();
        let paths = load_merge_marks_from(dir.path()).marked_paths();
        assert_eq!(paths, vec![".claude/settings.json", ".mcp.json"]);
    }

    #[test]
    fn marked_paths_is_empty_on_greenfield() {
        let dir = tempdir().unwrap();
        assert!(load_merge_marks_from(dir.path()).marked_paths().is_empty());
    }

    #[test]
    fn non_structured_path_is_rejected() {
        // AC-7: a mark on a non-structured file is rejected at set-time.
        let dir = tempdir().unwrap();
        let err = set_merge_mark(dir.path(), ".editorconfig", BTreeMap::new());
        assert!(matches!(err, Err(MarkError::Unsupported(_))));
        assert!(!load_merge_marks_from(dir.path()).is_marked(".editorconfig"));
    }

    #[test]
    fn array_keys_round_trip_into_policy() {
        use crate::shared::json_io::{ArrayMerge, ArrayPolicy};
        let dir = tempdir().unwrap();
        let mut keys = BTreeMap::new();
        keys.insert("hooks.Notification".to_string(), "command".to_string());
        set_merge_mark(dir.path(), ".claude/settings.json", keys).unwrap();

        let store = load_merge_marks_from(dir.path());
        let policy =
            crate::shared::json_io::MapArrayPolicy::new(store.array_keys(".claude/settings.json"));
        assert_eq!(
            policy.array_merge("hooks.Notification"),
            ArrayMerge::Union {
                key: Some("command".to_string())
            }
        );
        assert_eq!(policy.array_merge("other"), ArrayMerge::Replace);
    }

    #[test]
    fn malformed_store_does_not_wedge() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join(MERGE_MARKS_FILENAME),
            "marks: [not, a, map]\n",
        )
        .unwrap();
        // Malformed → logged + empty set, never a panic/error that wedges projection.
        let store = load_merge_marks_from(dir.path());
        assert!(store.marks.is_empty());
    }
}
