// SPDX-License-Identifier: MIT
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::path::Path;

/// Errors that can occur during atomic YAML file I/O.
#[derive(Debug)]
pub enum YamlIoError {
    Io(std::io::Error),
    Serialize(serde_yaml::Error),
}

impl std::fmt::Display for YamlIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            YamlIoError::Io(e) => write!(f, "io: {e}"),
            YamlIoError::Serialize(e) => write!(f, "yaml serialize: {e}"),
        }
    }
}

impl std::error::Error for YamlIoError {}

/// Read and deserialize a YAML file, defaulting to `T::default()` when the
/// file does not exist. A missing file is the "no data yet" case, not an
/// error; a present-but-malformed file surfaces as `Err(Serialize)` (the
/// shared error type wraps `serde_yaml::Error` for both directions).
///
/// The single read counterpart to [`write_yaml_atomic`], so per-user typed
/// stores (`attachments.yml`, …) share one read path
/// instead of each hand-rolling the same match + a bespoke error enum.
pub fn read_yaml_or_default<T: DeserializeOwned + Default>(path: &Path) -> Result<T, YamlIoError> {
    match std::fs::read_to_string(path) {
        Ok(contents) => serde_yaml::from_str(&contents).map_err(YamlIoError::Serialize),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(YamlIoError::Io(e)),
    }
}

/// Read and deserialize a YAML file, distinguishing a genuinely absent file
/// (`Ok(None)`) from one that is present but unreadable (`Err`). Unlike
/// [`read_yaml_or_default`], a missing file is NEVER coerced to `T::default()`
/// — so a content-addressed read-by-id store can answer "no such record" without
/// fabricating an empty value, while a corrupt file still surfaces distinctly.
/// The read counterpart used by the by-id record stores where `T` has no
/// meaningful default (e.g. an immutable `WorkflowRevision`).
pub fn read_yaml_optional<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, YamlIoError> {
    match std::fs::read_to_string(path) {
        Ok(contents) => serde_yaml::from_str(&contents)
            .map(Some)
            .map_err(YamlIoError::Serialize),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(YamlIoError::Io(e)),
    }
}

/// Serialize `value` to YAML, write to `{path}.tmp`, then rename over
/// `path`. Creates parent directory if missing. Crash mid-write leaves
/// old file intact or no file — never a half-written document.
pub fn write_yaml_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), YamlIoError> {
    let yaml = serde_yaml::to_string(value).map_err(YamlIoError::Serialize)?;
    write_atomic_inner(path, yaml.as_bytes(), "yml.tmp")
}

/// Atomically write already-serialized YAML `bytes` to `path` (temp +
/// rename, creating the parent dir). Use instead of [`write_yaml_atomic`]
/// when the exact byte content must be preserved verbatim — e.g. a
/// user-authored document whose comments and key order must survive, which a
/// `Serialize` round-trip would discard.
///
/// Unlike [`write_yaml_atomic`], the temp is per-write-unique, so concurrent
/// writers to the SAME `path` never share (and tear) one temp file — this
/// matters for the externally-driven write surfaces (the MCP
/// `write_worktree_config` tool).
///
/// SW-AU-25: a thin wrapper over the format-neutral
/// [`crate::shared::fs_atomic::write_bytes_atomic`] — the single unique-temp +
/// rename implementation (see its docs for the concurrency + naming rationale),
/// shared with the registry writer and the pack-config RMW. This fn only adds
/// the YAML error type at the boundary.
pub fn write_yaml_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<(), YamlIoError> {
    crate::shared::fs_atomic::write_bytes_atomic(path, bytes).map_err(YamlIoError::Io)
}

/// Serialize `value` to YAML and atomically write it via the unique-temp
/// [`write_yaml_bytes_atomic`]. Like [`write_yaml_atomic`] but concurrency-safe
/// for repeated writes to the SAME path: two writers never share (and tear) one
/// temp file, which matters for a keyed record store where a re-write can carry
/// different bytes under a stable key. The write counterpart to
/// [`read_yaml_optional`] for those by-id stores.
pub fn write_yaml_value_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), YamlIoError> {
    let yaml = serde_yaml::to_string(value).map_err(YamlIoError::Serialize)?;
    write_yaml_bytes_atomic(path, yaml.as_bytes())
}

/// Shared atomic-write body: write to a temp file named with `tmp_ext`,
/// then rename over the destination. Caller passes already-serialized
/// bytes so the helper stays format-agnostic.
fn write_atomic_inner(path: &Path, bytes: &[u8], tmp_ext: &str) -> Result<(), YamlIoError> {
    // SW-OC-16: the bundled CodeSense docs are read-only (the one rule, at the writer).
    crate::context_repo::codesense_docs::refuse_io_write_at(path).map_err(YamlIoError::Io)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(YamlIoError::Io)?;
    }

    let tmp_path = path.with_extension(tmp_ext);
    if let Err(e) = std::fs::write(&tmp_path, bytes) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(YamlIoError::Io(e));
    }
    if let Err(e) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(YamlIoError::Io(e));
    }

    Ok(())
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

    #[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
    struct SampleStore {
        #[serde(default)]
        items: Vec<String>,
    }

    #[test]
    fn read_yaml_or_default_missing_file_is_default() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("absent.yml");
        let store: SampleStore = read_yaml_or_default(&path).unwrap();
        assert_eq!(store, SampleStore::default());
    }

    #[test]
    fn read_yaml_or_default_reads_written_value() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("store.yml");
        let value = SampleStore {
            items: vec!["a".into(), "b".into()],
        };
        write_yaml_atomic(&path, &value).unwrap();
        let read_back: SampleStore = read_yaml_or_default(&path).unwrap();
        assert_eq!(read_back, value);
    }

    #[test]
    fn write_yaml_value_atomic_round_trips_via_optional_read() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("nested").join("v.yml");
        let value = Sample {
            name: "v".into(),
            count: 3,
        };
        write_yaml_value_atomic(&path, &value).unwrap();
        let read_back: Option<Sample> = read_yaml_optional(&path).unwrap();
        assert_eq!(read_back, Some(value));
        // No stray unique-temp residue.
        let strays: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(strays.is_empty(), "stray tmp files: {strays:?}");
    }

    #[test]
    fn read_yaml_optional_missing_file_is_none() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("absent.yml");
        let value: Option<Sample> = read_yaml_optional(&path).unwrap();
        assert_eq!(value, None, "a missing file must be None, never a default");
    }

    #[test]
    fn read_yaml_optional_reads_written_value() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("store.yml");
        let value = Sample {
            name: "hi".into(),
            count: 7,
        };
        write_yaml_atomic(&path, &value).unwrap();
        let read_back: Option<Sample> = read_yaml_optional(&path).unwrap();
        assert_eq!(read_back, Some(value));
    }

    #[test]
    fn read_yaml_optional_malformed_is_err_distinct_from_none() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("bad.yml");
        std::fs::write(&path, "not: valid: yaml: [[[").unwrap();
        let result: Result<Option<Sample>, _> = read_yaml_optional(&path);
        assert!(
            matches!(result, Err(YamlIoError::Serialize(_))),
            "a present-but-malformed file must be Err, never Ok(None)"
        );
    }

    #[test]
    fn read_yaml_or_default_malformed_is_err() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("bad.yml");
        std::fs::write(&path, "not: valid: yaml: [[[").unwrap();
        let result: Result<SampleStore, _> = read_yaml_or_default(&path);
        assert!(
            matches!(result, Err(YamlIoError::Serialize(_))),
            "malformed YAML should surface as a Serialize error"
        );
    }

    #[test]
    fn happy_path_write_and_read_back() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("data.yml");
        let value = Sample {
            name: "hello".into(),
            count: 42,
        };
        write_yaml_atomic(&path, &value).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        let read_back: Sample = serde_yaml::from_str(&contents).unwrap();
        assert_eq!(read_back, value);
    }

    #[test]
    fn creates_parent_directories() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("a").join("b").join("data.yml");
        let value = Sample {
            name: "nested".into(),
            count: 1,
        };
        write_yaml_atomic(&path, &value).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn no_tmp_sibling_on_success() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("data.yml");
        let value = Sample {
            name: "clean".into(),
            count: 0,
        };
        write_yaml_atomic(&path, &value).unwrap();

        let tmp_path = path.with_extension("yml.tmp");
        assert!(
            !tmp_path.exists(),
            ".tmp file should not remain after success"
        );
    }

    /// A type whose `Serialize` impl always fails.
    struct AlwaysFail;

    impl Serialize for AlwaysFail {
        fn serialize<S: serde::Serializer>(&self, _s: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("intentional failure"))
        }
    }

    #[test]
    fn serialize_error_leaves_original_untouched() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("data.yml");

        // Write an initial file.
        std::fs::write(&path, b"original content").unwrap();

        let result = write_yaml_atomic(&path, &AlwaysFail);
        assert!(result.is_err(), "should fail to serialize");

        // Original file must be untouched.
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "original content");

        // No .tmp left behind.
        let tmp_path = path.with_extension("yml.tmp");
        assert!(
            !tmp_path.exists(),
            ".tmp file should not remain after error"
        );
    }

    #[test]
    fn rename_failure_cleans_up_temp_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("data.yml");
        // Pre-create `path` as a non-empty directory so `rename(tmp, path)` fails.
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("guard"), b"").unwrap();

        let val = Sample {
            name: "noop".into(),
            count: 0,
        };
        let result = write_yaml_atomic(&path, &val);
        assert!(matches!(result, Err(YamlIoError::Io(_))));

        let tmp_path = path.with_extension("yml.tmp");
        assert!(
            !tmp_path.exists(),
            "temp file {} should have been cleaned up after rename failure",
            tmp_path.display()
        );
    }

    #[test]
    fn write_failure_exercises_cleanup_branch() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("data.yml");
        let tmp_path = path.with_extension("yml.tmp");
        // Pre-create `tmp_path` as a directory so `fs::write(tmp, ...)` fails with EISDIR.
        // The cleanup branch then attempts `remove_file(tmp_path)`, which silently
        // no-ops on a directory. This exercises the write-failure cleanup path.
        std::fs::create_dir(&tmp_path).unwrap();

        let val = Sample {
            name: "noop".into(),
            count: 0,
        };
        let result = write_yaml_atomic(&path, &val);
        assert!(matches!(result, Err(YamlIoError::Io(_))));
        // Target was never created.
        assert!(!path.exists());
        // tmp_path is still the directory we pre-created; cleanup's `remove_file`
        // can't remove a directory but the cleanup branch was reached.
        assert!(tmp_path.is_dir());
    }

    #[test]
    fn write_yaml_bytes_atomic_preserves_exact_bytes() {
        let dir = tempdir().unwrap();
        // Dotted stem (`my.repo.yaml`) exercises the `with_extension` temp-name
        // derivation on a name with embedded dots — a valid repo-slug shape.
        let path = dir.path().join("nested").join("my.repo.yaml");
        // Comments + key order survive only because we write raw bytes (a
        // Serialize round-trip would discard them).
        let raw = "# a comment\nversion: 1\nsetup:\n  - npm ci   # trailing comment\n";
        write_yaml_bytes_atomic(&path, raw.as_bytes()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
        // Parent created; no stray tmp from the unique-suffix scheme.
        let strays: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(strays.is_empty(), "stray tmp files: {strays:?}");
    }

    #[test]
    fn write_yaml_bytes_atomic_concurrent_writers_never_tear() {
        use std::sync::{Arc, Barrier};
        // N threads write distinct COMPLETE payloads to the SAME path. With a
        // shared fixed tmp name their writes could interleave into one torn
        // file; with per-write-unique temps the final file must be exactly one
        // of the payloads (last-writer-wins), never a mix.
        //
        // A barrier releases every thread into its write simultaneously so the
        // writes genuinely contend — without it the OS often serializes them on
        // a fast fs, leaving the race window unexercised (a weak guard).
        let dir = tempdir().unwrap();
        let path = Arc::new(dir.path().join("race.yaml"));
        let payloads: Vec<String> = (0..8)
            .map(|i| format!("version: 1\nsetup:\n  - cmd-{i}\n"))
            .collect();
        let barrier = Arc::new(Barrier::new(payloads.len()));
        let handles: Vec<_> = payloads
            .iter()
            .map(|p| {
                let p = p.clone();
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    write_yaml_bytes_atomic(&path, p.as_bytes()).unwrap()
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let final_content = std::fs::read_to_string(path.as_path()).unwrap();
        assert!(
            payloads.contains(&final_content),
            "final file is torn — not equal to any single payload: {final_content:?}"
        );
        // No tmp residue from any of the writers.
        let strays: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(strays.is_empty(), "stray tmp files: {strays:?}");
    }
}
