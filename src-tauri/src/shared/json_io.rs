// SPDX-License-Identifier: MIT
//! Atomic JSON file I/O — matching the `yaml_io.rs` pattern.

use serde::{de::DeserializeOwned, Serialize};
use std::path::Path;

/// Errors that can occur during atomic JSON I/O.
#[derive(Debug)]
pub enum JsonIoError {
    Io(std::io::Error),
    Serialize(serde_json::Error),
    Deserialize(serde_json::Error),
}

impl std::fmt::Display for JsonIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JsonIoError::Io(e) => write!(f, "json io: {e}"),
            JsonIoError::Serialize(e) => write!(f, "json serialize: {e}"),
            JsonIoError::Deserialize(e) => write!(f, "json deserialize: {e}"),
        }
    }
}

impl std::error::Error for JsonIoError {}

/// Read a JSON file and deserialize into `T`.
/// Returns the deserialization error if the file exists but contains invalid JSON.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, JsonIoError> {
    let data = std::fs::read_to_string(path).map_err(JsonIoError::Io)?;
    serde_json::from_str(&data).map_err(JsonIoError::Deserialize)
}

/// Serialize `value` to pretty JSON, write to `{path}.tmp`, then rename over
/// `path`. Creates parent directory if missing. Crash mid-write leaves
/// old file intact or no file — never a half-written document.
pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), JsonIoError> {
    let json = serde_json::to_string_pretty(value).map_err(JsonIoError::Serialize)?;
    write_atomic_inner(path, &json, None)
}

/// Same as [`write_json_atomic`] for app-owned private records: on unix the
/// file is created `0600` (an existing looser file is replaced by a `0600`
/// one) and its parent directory is created or tightened to `0700`. On other
/// platforms this is identical to [`write_json_atomic`].
pub fn write_json_atomic_private<T: Serialize>(path: &Path, value: &T) -> Result<(), JsonIoError> {
    let json = serde_json::to_string_pretty(value).map_err(JsonIoError::Serialize)?;
    write_atomic_inner(path, &json, Some(PRIVATE_FILE_MODE))
}

/// File mode for [`write_json_atomic_private`]; the parent directory gets
/// `0o700`.
const PRIVATE_FILE_MODE: u32 = 0o600;

/// R-7: Compact variant for high-frequency, machine-only consumers (e.g. the
/// frontmatter cache). Same atomic + temp-cleanup guarantees as the pretty
/// variant; only the serialization shape differs. Lockfile and user-editable
/// files (view-overrides.json) MUST keep using the pretty variant.
pub fn write_json_atomic_compact<T: Serialize>(path: &Path, value: &T) -> Result<(), JsonIoError> {
    let json = serde_json::to_string(value).map_err(JsonIoError::Serialize)?;
    write_atomic_inner(path, &json, None)
}

/// SW-CP-24: how an array at a given document path merges when a structured
/// deep-merge encounters it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArrayMerge {
    /// RFC 7386 / Helm default: the `patch` array replaces the `base` array.
    Replace,
    /// Strategic-merge-patch keyed set: union `base` + `patch`, dedup by
    /// identity, order-independent. `key = Some(field)` identifies object
    /// elements by that field — a patch element whose key matches a base
    /// element deep-merges into it, otherwise it is appended. `key = None`
    /// treats each scalar element as its own identity — matching scalars
    /// dedup (used for string sets like the permissions `allow`/`deny`).
    Union { key: Option<String> },
}

/// Resolves the [`ArrayMerge`] strategy per object path — dot-joined object
/// keys from the document root (e.g. `"hooks.Notification"`). Implementations
/// are schema-blind: codesense never learns any agent's config format, the
/// caller declares which arrays are keyed sets.
pub trait ArrayPolicy {
    fn array_merge(&self, path: &str) -> ArrayMerge;
}

/// Every array replaces (RFC 7386). Makes `deep_merge_with(.., &ReplaceAll)`
/// behaviourally identical to the legacy [`deep_merge`].
pub struct ReplaceAll;

impl ArrayPolicy for ReplaceAll {
    fn array_merge(&self, _path: &str) -> ArrayMerge {
        ArrayMerge::Replace
    }
}

/// SW-CP-24: an [`ArrayPolicy`] backed by an explicit map of document path →
/// merge-key field (the mark store's declared `array_keys`). A path present in
/// the map is a keyed object set; any path absent replaces (RFC 7386 default).
#[derive(Debug, Default, Clone)]
pub struct MapArrayPolicy {
    keyed: std::collections::BTreeMap<String, String>,
}

impl MapArrayPolicy {
    pub fn new(keyed: std::collections::BTreeMap<String, String>) -> Self {
        Self { keyed }
    }
}

impl ArrayPolicy for MapArrayPolicy {
    fn array_merge(&self, path: &str) -> ArrayMerge {
        match self.keyed.get(path) {
            Some(key) => ArrayMerge::Union {
                key: Some(key.clone()),
            },
            None => ArrayMerge::Replace,
        }
    }
}

/// Recursively merge `patch` into `base` with the legacy RFC-7386 semantics:
/// objects merge key-by-key; every array (and any object-vs-non-object type
/// mismatch) replaces the base value. Foreign keys in `base` are always
/// preserved. Thin wrapper over [`deep_merge_with`] with [`ReplaceAll`] — the
/// single JSON merge primitive `mcp::handler` (viewer-state patches) depends on.
pub fn deep_merge(base: &mut serde_json::Value, patch: &serde_json::Value) {
    deep_merge_with(base, patch, &ReplaceAll);
}

/// SW-CP-24: RFC 7386 object merge + strategic-merge-patch keyed-array set
/// (per `policy`) over `serde_json::Value`. The one merge engine both the
/// merge-marked-content projection and codesense's post-step mergers share, so
/// their composition is a cross-merger fixed point. Idempotent: merging a value
/// into itself is a no-op.
///
/// **Null semantics (RFC 7386 deviation):** a `null` in `patch` does NOT *delete*
/// a base key — the key survives. It DOES overwrite that key's value with null
/// (patch-wins at a leaf), and a whole-document `null` patch replaces `base`
/// entirely (the generic catch-all).
pub fn deep_merge_with(
    base: &mut serde_json::Value,
    patch: &serde_json::Value,
    policy: &dyn ArrayPolicy,
) {
    deep_merge_at(base, patch, policy, "");
}

fn deep_merge_at(
    base: &mut serde_json::Value,
    patch: &serde_json::Value,
    policy: &dyn ArrayPolicy,
    path: &str,
) {
    match patch {
        serde_json::Value::Object(patch_map) => {
            if let serde_json::Value::Object(base_map) = base {
                for (key, patch_val) in patch_map {
                    let child = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    let base_val = base_map
                        .entry(key.clone())
                        .or_insert(serde_json::Value::Null);
                    deep_merge_at(base_val, patch_val, policy, &child);
                }
            } else {
                *base = patch.clone();
            }
        }
        serde_json::Value::Array(patch_arr) => match policy.array_merge(path) {
            ArrayMerge::Union { key } => {
                if let serde_json::Value::Array(base_arr) = base {
                    union_array(base_arr, patch_arr, key.as_deref(), policy, path);
                } else {
                    *base = patch.clone();
                }
            }
            ArrayMerge::Replace => {
                *base = patch.clone();
            }
        },
        _ => {
            *base = patch.clone();
        }
    }
}

/// SW-CP-24: order-independent set-union of `patch` into `base`.
///
/// Identity is the declared `key` field when the element carries it (matching
/// elements deep-merge via `policy`); otherwise — a scalar set (`key = None`)
/// OR a keyed element that happens to LACK the key field — identity is the whole
/// element (matching elements are a no-op). The keyless-in-a-keyed-array
/// fallback is load-bearing for idempotence: without it a patch element missing
/// the key would append every wave and grow the array unbounded (breaking AC-3
/// for e.g. a `hooks.*` entry authored without a `command`). Appends any element
/// with no match. Idempotent by construction.
pub fn union_array(
    base: &mut Vec<serde_json::Value>,
    patch: &[serde_json::Value],
    key: Option<&str>,
    policy: &dyn ArrayPolicy,
    path: &str,
) {
    for patch_el in patch {
        // The key field's value, only when this is a keyed array AND the element
        // actually carries the key — this is what distinguishes a keyed-object
        // merge from a whole-element identity dedup.
        let key_val = key.and_then(|k| patch_el.get(k));
        let matched = match (key, key_val) {
            (Some(k), Some(pk)) => base.iter().position(|b| b.get(k) == Some(pk)),
            // scalar set, or a keyed element missing the key → whole-element identity
            _ => base.iter().position(|b| b == patch_el),
        };
        match matched {
            // Keyed object match → deep-merge in place. Whole-element identity
            // match (scalar set, or keyless fallback) → already present, no-op.
            Some(i) if key_val.is_some() => deep_merge_at(&mut base[i], patch_el, policy, path),
            Some(_) => {}
            None => base.push(patch_el.clone()),
        }
    }
}

/// Shared temp+rename+cleanup for both pretty and compact atomic writers.
/// `private_mode = Some(m)` (unix only) creates the temp file with mode `m`
/// and creates or tightens the parent directory to `0700`.
fn write_atomic_inner(
    path: &Path,
    json: &str,
    private_mode: Option<u32>,
) -> Result<(), JsonIoError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(JsonIoError::Io)?;
        #[cfg(unix)]
        if private_mode.is_some() && !std::ffi::OsStr::is_empty(parent.as_os_str()) {
            // Best-effort: a parent we cannot tighten (not ours, or a symlink)
            // must not block the write; the file itself is still exactly 0600.
            if let Err(e) = tighten_private_dir(parent) {
                log::warn!("could not set {} to 0700: {e}", std::path::Path::display(parent));
            }
        }
    }

    let tmp_path = path.with_extension("json.tmp");
    if let Err(e) = write_tmp(&tmp_path, json, private_mode) {
        // A failed write may have left a partial tmp; clean it up so the next
        // attempt doesn't have to.
        let _ = std::fs::remove_file(&tmp_path);
        return Err(JsonIoError::Io(e));
    }
    if let Err(e) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(JsonIoError::Io(e));
    }

    Ok(())
}

/// Set `dir` to `0700` unless it is a symlink (the target is not ours to chmod).
#[cfg(unix)]
fn tighten_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if std::fs::symlink_metadata(dir)?.file_type().is_symlink() {
        return Err(std::io::Error::other("is a symlink; left unchanged"));
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

fn write_tmp(tmp_path: &Path, json: &str, private_mode: Option<u32>) -> std::io::Result<()> {
    #[cfg(unix)]
    if let Some(mode) = private_mode {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        // A stale temp left by a crash keeps its old mode through `open`, so
        // drop it first and create fresh with the requested mode.
        let _ = std::fs::remove_file(tmp_path);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(tmp_path)?;
        // `mode` is masked by the umask; make the final mode exact.
        file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        return file.write_all(json.as_bytes());
    }
    let _ = private_mode;
    std::fs::write(tmp_path, json.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use tempfile::tempdir;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Sample {
        name: String,
        count: u32,
    }

    #[cfg(unix)]
    #[test]
    fn private_writer_sets_0600_file_and_0700_parent_and_default_writer_unchanged() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let dir = tempdir().unwrap();
        let val = Sample {
            name: "x".into(),
            count: 1,
        };
        let path = dir.path().join("priv").join("a.json");
        write_json_atomic_private(&path, &val).unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        // A stale looser temp from a crash does not leak its mode.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, "stale").unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_json_atomic_private(&path, &val).unwrap();
        assert_eq!(mode(&path), 0o600);
        // The default writer leaves the parent directory alone.
        let plain = dir.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o755)).unwrap();
        write_json_atomic(&plain.join("b.json"), &val).unwrap();
        assert_eq!(mode(&plain), 0o755);
    }

    #[cfg(unix)]
    #[test]
    fn private_writer_leaves_a_symlinked_parent_alone() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let dir = tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755)).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let val = Sample {
            name: "x".into(),
            count: 1,
        };
        write_json_atomic_private(&link.join("a.json"), &val).unwrap();
        assert_eq!(mode(&real), 0o755);
        assert_eq!(mode(&real.join("a.json")), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn tightening_an_unowned_parent_errors_instead_of_panicking() {
        // `/usr` is root-owned and not a symlink: chmod fails for a normal
        // user, and the writer logs that error and carries on (the file is
        // still created 0600 by `write_tmp`).
        let err = tighten_private_dir(Path::new("/usr")).err();
        let is_root = std::process::Command::new("id")
            .arg("-u")
            .output()
            .is_ok_and(|o| o.stdout == b"0\n");
        assert_eq!(err.is_some(), !is_root);
    }

    #[test]
    fn round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.json");
        let val = Sample {
            name: "hello".into(),
            count: 42,
        };
        write_json_atomic(&path, &val).unwrap();
        let loaded: Sample = read_json(&path).unwrap();
        assert_eq!(val, loaded);
    }

    #[test]
    fn read_missing_file_returns_io_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("missing.json");
        let result: Result<Sample, _> = read_json(&path);
        assert!(matches!(result, Err(JsonIoError::Io(_))));
    }

    #[test]
    fn read_invalid_json_returns_deserialize_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, "not json").unwrap();
        let result: Result<Sample, _> = read_json(&path);
        assert!(matches!(result, Err(JsonIoError::Deserialize(_))));
    }

    #[test]
    fn write_creates_parent_dirs() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sub").join("dir").join("test.json");
        let val = Sample {
            name: "nested".into(),
            count: 1,
        };
        write_json_atomic(&path, &val).unwrap();
        let loaded: Sample = read_json(&path).unwrap();
        assert_eq!(val, loaded);
    }

    #[test]
    fn atomic_write_no_partial_on_overwrite() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.json");
        let v1 = Sample {
            name: "first".into(),
            count: 1,
        };
        let v2 = Sample {
            name: "second".into(),
            count: 2,
        };
        write_json_atomic(&path, &v1).unwrap();
        write_json_atomic(&path, &v2).unwrap();
        let loaded: Sample = read_json(&path).unwrap();
        assert_eq!(v2, loaded);
    }

    #[test]
    fn rename_failure_cleans_up_temp_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.json");
        // Pre-create `path` as a non-empty directory so `rename(tmp, path)` fails.
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("guard"), b"").unwrap();

        let val = Sample {
            name: "noop".into(),
            count: 0,
        };
        let result = write_json_atomic(&path, &val);
        assert!(matches!(result, Err(JsonIoError::Io(_))));

        let tmp_path = path.with_extension("json.tmp");
        assert!(
            !tmp_path.exists(),
            "temp file {} should have been cleaned up after rename failure",
            tmp_path.display()
        );
    }

    #[test]
    fn compact_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.json");
        let val = Sample {
            name: "compact".into(),
            count: 7,
        };
        write_json_atomic_compact(&path, &val).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        // Compact form has no leading whitespace before keys (pretty form does).
        assert!(
            !raw.contains("\n  \""),
            "compact form should not contain pretty-printed indentation: {raw:?}",
        );
        let loaded: Sample = read_json(&path).unwrap();
        assert_eq!(val, loaded);
    }

    #[test]
    fn pretty_vs_compact_both_read_via_read_json() {
        // read_json is form-agnostic — both variants must round-trip.
        let dir = tempdir().unwrap();
        let pretty_path = dir.path().join("pretty.json");
        let compact_path = dir.path().join("compact.json");
        let val = Sample {
            name: "agnostic".into(),
            count: 13,
        };
        write_json_atomic(&pretty_path, &val).unwrap();
        write_json_atomic_compact(&compact_path, &val).unwrap();

        let from_pretty: Sample = read_json(&pretty_path).unwrap();
        let from_compact: Sample = read_json(&compact_path).unwrap();
        assert_eq!(val, from_pretty);
        assert_eq!(val, from_compact);

        // Sanity: the on-disk bytes differ.
        let pretty_raw = std::fs::read_to_string(&pretty_path).unwrap();
        let compact_raw = std::fs::read_to_string(&compact_path).unwrap();
        assert_ne!(pretty_raw, compact_raw);
        assert!(
            pretty_raw.len() > compact_raw.len(),
            "pretty is larger than compact"
        );
    }

    #[test]
    fn write_failure_exercises_cleanup_branch() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.json");
        let tmp_path = path.with_extension("json.tmp");
        // Pre-create `tmp_path` as a directory so `fs::write(tmp, ...)` fails with EISDIR.
        // The cleanup branch then attempts `remove_file(tmp_path)`, which silently
        // no-ops on a directory. This exercises the write-failure cleanup path.
        std::fs::create_dir(&tmp_path).unwrap();

        let val = Sample {
            name: "noop".into(),
            count: 0,
        };
        let result = write_json_atomic(&path, &val);
        assert!(matches!(result, Err(JsonIoError::Io(_))));
        // Target was never created.
        assert!(!path.exists());
        // tmp_path is still the directory we pre-created; cleanup's `remove_file`
        // can't remove a directory but the cleanup branch was reached.
        assert!(tmp_path.is_dir());
    }

    #[test]
    fn deep_merge_objects_merged_recursively() {
        let mut base = serde_json::json!({"a": 1, "b": {"x": 10}});
        let patch = serde_json::json!({"b": {"y": 20}, "c": 3});
        deep_merge(&mut base, &patch);
        assert_eq!(
            base,
            serde_json::json!({"a": 1, "b": {"x": 10, "y": 20}, "c": 3})
        );
    }

    #[test]
    fn deep_merge_non_object_replaced() {
        let mut base = serde_json::json!({"a": 1});
        let patch = serde_json::json!({"a": 2});
        deep_merge(&mut base, &patch);
        assert_eq!(base, serde_json::json!({"a": 2}));
    }

    #[test]
    fn deep_merge_arrays_replaced_not_merged() {
        let mut base = serde_json::json!({"a": [1, 2]});
        let patch = serde_json::json!({"a": [3]});
        deep_merge(&mut base, &patch);
        assert_eq!(base, serde_json::json!({"a": [3]}));
    }

    #[test]
    fn deep_merge_preserves_foreign_keys() {
        let mut base = serde_json::json!({"keep": 1, "nested": {"keep2": 2}});
        let patch = serde_json::json!({"nested": {"new": 3}});
        deep_merge(&mut base, &patch);
        assert_eq!(
            base,
            serde_json::json!({"keep": 1, "nested": {"keep2": 2, "new": 3}})
        );
    }

    // --- SW-CP-24: generalized engine (deep_merge_with + ArrayPolicy) ---

    fn policy(pairs: &[(&str, &str)]) -> MapArrayPolicy {
        MapArrayPolicy::new(
            pairs
                .iter()
                .map(|(p, k)| (p.to_string(), k.to_string()))
                .collect(),
        )
    }

    #[test]
    fn deep_merge_with_replace_all_matches_legacy() {
        // The wrapper and the explicit ReplaceAll policy must agree, and arrays
        // must still replace (the legacy contract callers rely on).
        let mut a = serde_json::json!({"x": [1, 2], "y": {"n": 1}});
        let mut b = a.clone();
        let patch = serde_json::json!({"x": [3], "y": {"m": 2}});
        deep_merge(&mut a, &patch);
        deep_merge_with(&mut b, &patch, &ReplaceAll);
        assert_eq!(a, b);
        assert_eq!(a, serde_json::json!({"x": [3], "y": {"n": 1, "m": 2}}));
    }

    #[test]
    fn keyed_array_union_dedups_appends_and_merges() {
        // hooks.Notification keyed by "command": a re-declared command merges
        // (dedup), a new command appends, order-independent.
        let mut base = serde_json::json!({
            "hooks": { "Notification": [ {"command": "a", "keep": true} ] }
        });
        let patch = serde_json::json!({
            "hooks": { "Notification": [ {"command": "a", "added": 1}, {"command": "b"} ] }
        });
        deep_merge_with(
            &mut base,
            &patch,
            &policy(&[("hooks.Notification", "command")]),
        );
        assert_eq!(
            base,
            serde_json::json!({
                "hooks": { "Notification": [
                    {"command": "a", "keep": true, "added": 1},
                    {"command": "b"}
                ] }
            })
        );
    }

    #[test]
    fn unkeyed_array_replaces() {
        // No declared key for this path → RFC 7386 replace.
        let mut base = serde_json::json!({"list": [1, 2, 3]});
        let patch = serde_json::json!({"list": [9]});
        deep_merge_with(&mut base, &patch, &policy(&[("other", "id")]));
        assert_eq!(base, serde_json::json!({"list": [9]}));
    }

    #[test]
    fn scalar_identity_union_via_union_array() {
        // The permissions set-union case (allow/deny) — key=None, dedup by value.
        let mut base = vec![serde_json::json!("a"), serde_json::json!("b")];
        let patch = vec![serde_json::json!("b"), serde_json::json!("c")];
        union_array(&mut base, &patch, None, &ReplaceAll, "");
        assert_eq!(
            base,
            vec![
                serde_json::json!("a"),
                serde_json::json!("b"),
                serde_json::json!("c")
            ]
        );
    }

    #[test]
    fn deep_merge_with_is_idempotent() {
        // Fixed point: merging a value into itself changes nothing — the
        // property the cross-merger composition (AC-3) rests on.
        let pol = policy(&[("hooks.Notification", "command")]);
        let doc = serde_json::json!({
            "mcpServers": { "codesense": {"command": "/abs/bridge"} },
            "hooks": { "Notification": [ {"command": "x"}, {"command": "y"} ] },
            "list": [1, 2]
        });
        let mut once = doc.clone();
        deep_merge_with(&mut once, &doc, &pol);
        assert_eq!(once, doc, "merge into self must be a no-op");
        let mut twice = once.clone();
        deep_merge_with(&mut twice, &doc, &pol);
        assert_eq!(twice, once, "second merge must also be a no-op");
    }

    #[test]
    fn keyed_union_is_idempotent_when_element_lacks_the_key() {
        // Regression (code-review): a keyed array whose element LACKS the declared
        // key must dedup by whole-element identity, not append every wave (which
        // would grow the array unbounded and break AC-3). Driving case: a
        // `hooks.Notification` entry authored without a `command`.
        let pol = policy(&[("hooks.Notification", "command")]);
        let doc = serde_json::json!({
            "hooks": { "Notification": [ {"command": "a"}, {"matcher": "x"} ] }
        });
        let mut once = doc.clone();
        deep_merge_with(&mut once, &doc, &pol);
        assert_eq!(
            once, doc,
            "merge into self must not duplicate the keyless element"
        );
        let mut twice = once.clone();
        deep_merge_with(&mut twice, &doc, &pol);
        assert_eq!(
            twice, once,
            "second wave must stay byte-stable (no unbounded growth)"
        );
        assert_eq!(once["hooks"]["Notification"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn union_is_order_independent() {
        let pol = policy(&[("a", "id")]);
        let patch = serde_json::json!({"a": [{"id": 1}, {"id": 2}]});
        let mut forward = serde_json::json!({"a": [{"id": 1}]});
        let mut reverse = serde_json::json!({"a": [{"id": 2}]});
        deep_merge_with(&mut forward, &patch, &pol);
        deep_merge_with(&mut reverse, &patch, &pol);
        // Both converge to the same identity set (compare as sets of ids).
        let ids = |v: &serde_json::Value| {
            let mut got: Vec<i64> = v["a"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["id"].as_i64().unwrap())
                .collect();
            got.sort_unstable();
            got
        };
        assert_eq!(ids(&forward), vec![1, 2]);
        assert_eq!(ids(&reverse), vec![1, 2]);
    }
}
