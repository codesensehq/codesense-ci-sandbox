// SPDX-License-Identifier: MIT
//! SW-AU-25: the one format-neutral atomic file writer — write to a
//! per-write-unique temp sibling then `fs::rename` over the target.
//!
//! Rename is atomic on the same filesystem, so a concurrent reader observes
//! either the old file or the new one **whole** — never a truncated/torn one
//! (the failure mode a plain `std::fs::write` exposes mid-write). The temp name
//! is `<dest>.tmp.<pid>.<counter>` so two writers to the SAME path never share
//! one temp file (whose interleaved `fs::write`s the rename would then publish
//! as a torn document): the renames still race last-writer-wins, but each
//! publishes a COMPLETE file. Parent dirs are created defensively.
//!
//! The `.tmp.` **infix** (not a trailing `.tmp`) is load-bearing: the content
//! watcher's `is_transient_editor_file` guard matches `.tmp.`, so these
//! transient siblings are suppressed (SW-SC-11) rather than emitted as spurious
//! content-file changes when a target lives under a recursively-watched
//! `content/` dir. Same `.tmp.` infix as `content_engine::atomic_io` (that
//! writer additionally fsyncs for git-sync durability — a heavier, separate
//! concern; this primitive is atomicity-only, like the yaml/json writers).
//!
//! This is the primitive `shared::yaml_io::write_yaml_bytes_atomic`, the
//! registry writer `context_repo::write_edit_to_workdir`, and the serialized
//! team-config RMW (`config_editor::rmw_team_config_file_with_decision`) all delegate to, so
//! there is a single temp+rename implementation rather than a copy per caller.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The per-write-unique temp sibling for `path`: `<path>.tmp.<pid>.<nonce>`. The
/// `.tmp.` infix is deliberate — see the module docs (the watcher's transient
/// guard keys on it). Kept as a named helper so that invariant is unit-testable.
fn tmp_sibling(path: &Path, nonce: u64) -> PathBuf {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp.{}.{}", std::process::id(), nonce));
    PathBuf::from(tmp)
}

/// Atomically write `bytes` to `path` via a per-write-unique temp file +
/// rename. Creates the parent directory if missing. A concurrent reader sees
/// the old or the new file whole, never a torn one; a concurrent writer to the
/// same path never shares the temp, so every rename publishes a complete file.
///
/// Uniqueness: the process-wide counter guarantees it *within* a process; the
/// pid disambiguates *across* concurrently-live processes. The error paths
/// remove the temp; because the names are unique, a later writer never reuses
/// (and so never self-cleans) an earlier temp — the only way one survives is a
/// hard kill (SIGKILL / power loss) in the write→rename gap. Such an orphan is
/// inert: it is never merged into the target and the rename stays atomic, and
/// the watcher's transient guard ignores it (the `.tmp.` infix), so it never
/// emits a spurious change.
pub fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    // SW-OC-16: the bundled CodeSense docs are read-only (the one rule, at the writer).
    crate::context_repo::codesense_docs::refuse_io_write_at(path)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let nonce = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_path = tmp_sibling(path, nonce);

    if let Err(e) = std::fs::write(&tmp_path, bytes) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }
    Ok(())
}

/// `std::fs::remove_file` under the same rule as the writers: nothing is deleted from the
/// read-only CodeSense docs (SW-OC-16). Delete through this, not `std::fs`, so a new delete
/// path inherits the rule.
pub fn remove_file(path: &Path) -> std::io::Result<()> {
    crate::context_repo::codesense_docs::refuse_io_write_at(path)?;
    std::fs::remove_file(path)
}

/// `std::fs::remove_dir_all` under the same rule (see [`remove_file`]).
pub fn remove_dir_all(path: &Path) -> std::io::Result<()> {
    crate::context_repo::codesense_docs::refuse_io_write_at(path)?;
    std::fs::remove_dir_all(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn temp_sibling_is_recognized_as_transient_by_the_watcher_guard() {
        // SW-SC-11 regression guard: a target under a recursively-watched `content/` dir (e.g.
        // `save_team_config`'s schema/taxonomy write) must NOT emit a spurious change for the
        // transient temp. The content watcher suppresses temps via `is_transient_editor_file`,
        // which keys on a `.tmp.` INFIX — so the temp name must carry it, not a trailing `.tmp`.
        let target = Path::new("/team/content/field-schema.yml");
        let tmp = tmp_sibling(target, 7);
        let name = tmp.file_name().unwrap().to_str().unwrap();
        assert!(
            name.contains(".tmp."),
            "temp name lacks the `.tmp.` infix: {name}"
        );
        assert!(
            crate::commands::watch::is_transient_editor_file(&tmp),
            "watcher would emit a spurious change for {name}"
        );
        // …and it is a real sibling (same parent), so the rename stays same-filesystem.
        assert_eq!(tmp.parent(), target.parent());
    }

    #[test]
    fn writes_and_reads_back_creating_parents() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("deeper").join("data.bin");
        write_bytes_atomic(&path, b"hello world").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello world");
    }

    #[test]
    fn overwrites_existing_whole() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("data.txt");
        write_bytes_atomic(&path, b"first").unwrap();
        write_bytes_atomic(&path, b"second-and-longer").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second-and-longer");
    }

    #[test]
    fn no_tmp_sibling_left_on_success() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("data.txt");
        write_bytes_atomic(&path, b"x").unwrap();
        let strays: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(strays.is_empty(), "stray tmp files: {strays:?}");
    }

    #[test]
    fn write_failure_cleans_up_and_errors() {
        // Point `path` at an existing directory so the final rename fails
        // (can't rename a file over a non-empty dir) → the cleanup branch runs.
        let dir = tempdir().unwrap();
        let path = dir.path().join("occupied");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("guard"), b"").unwrap();

        let err = write_bytes_atomic(&path, b"payload");
        assert!(err.is_err());
        let strays: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(
            strays.is_empty(),
            "temp not cleaned after failure: {strays:?}"
        );
    }

    #[test]
    fn concurrent_writers_to_one_path_never_tear() {
        use std::sync::{Arc, Barrier};
        // N threads write distinct COMPLETE payloads to the SAME path. With a
        // shared fixed temp name their writes could interleave into one torn
        // file; with per-write-unique temps the final file must equal exactly
        // one payload (last-writer-wins), never a mix. A barrier releases every
        // thread simultaneously so the writes genuinely contend.
        let dir = tempdir().unwrap();
        let path = Arc::new(dir.path().join("race.bin"));
        let payloads: Vec<String> = (0..8).map(|i| format!("payload-number-{i}")).collect();
        let barrier = Arc::new(Barrier::new(payloads.len()));
        let handles: Vec<_> = payloads
            .iter()
            .map(|p| {
                let p = p.clone();
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    write_bytes_atomic(&path, p.as_bytes()).unwrap();
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
        let strays: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(strays.is_empty(), "stray tmp files: {strays:?}");
    }
}
