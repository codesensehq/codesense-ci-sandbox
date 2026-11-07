// SPDX-License-Identifier: MIT
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
static SAVE_COUNTER: AtomicU64 = AtomicU64::new(0);
static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Dependency-free stable note id: `{unix_nanos:x}-{counter:x}`. Filename-safe
/// (hex + hyphen), used verbatim as `{id}.md` and in `cs:note:<id>` links
/// (SW-SN-9). Assigned once at create; never regenerated (AC-1 immutability).
pub(crate) fn generate_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let counter = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:x}-{:x}", nanos, counter)
}

/// Atomic write of `filename` under `dir` (unique tmp + rename), generalizing
/// the legacy `save_notes_at` discipline to any filename. Sweeps cross-PID
/// orphan tmp files afterward.
pub(crate) fn atomic_write(dir: &Path, filename: &str, content: &[u8]) -> Result<(), String> {
    let counter = SAVE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(
        "{}.tmp.{}.{}",
        filename,
        std::process::id(),
        counter
    ));
    fs::write(&tmp, content).map_err(|e| format!("failed to write tmp for {}: {}", filename, e))?;
    if let Err(e) = fs::rename(&tmp, dir.join(filename)) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("failed to rename {}: {}", filename, e));
    }
    sweep_notes_tmp_files(dir);
    Ok(())
}

/// Generalized orphan-tmp sweep for the notes dir: removes any
/// `<base>.tmp.<pid>.<counter>` whose pid differs from the current process
/// (leftovers from prior crashed lifetimes). Same-PID in-flight tmp files are
/// preserved (a concurrent save may not have renamed yet).
pub(crate) fn sweep_notes_tmp_files(dir: &Path) {
    let my_pid = std::process::id();
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some(idx) = name.find(".tmp.") else {
            continue;
        };
        // suffix = "<pid>.<counter>"
        let mut parts = name[idx + 5..].split('.');
        let (Some(pid_s), Some(ctr_s), None) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        if pid_s.is_empty()
            || ctr_s.is_empty()
            || !pid_s.bytes().all(|b| b.is_ascii_digit())
            || !ctr_s.bytes().all(|b| b.is_ascii_digit())
        {
            continue;
        }
        if pid_s.parse::<u32>().ok() == Some(my_pid) {
            continue; // keep our own in-flight tmp
        }
        let _ = fs::remove_file(entry.path());
    }
}
