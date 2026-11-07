// SPDX-License-Identifier: MIT
//! SS-FU-17 AC-5: process-space liveness — a crash-honest, machine-readable
//! "is codesense running?" signal.
//!
//! Liveness is a *process* question; its honest answer lives in process-space,
//! never in mutating the user's repos (that's why clean-on-close was
//! rejected). This module writes a heartbeat under codesense's own state dir
//! (`~/.codesense/runtime/heartbeat.json`), refreshed on an interval while the
//! app runs and removed on graceful exit.
//!
//! **Crash-honest by staleness, not presence.** A bare presence file would
//! linger after a crash / `kill -9` / OOM and falsely read "running." Instead
//! the heartbeat carries an `updated_at_ms` that the writer bumps every
//! [`HEARTBEAT_INTERVAL_MS`]; a reader treats a heartbeat older than
//! [`HEARTBEAT_STALE_AFTER_MS`] (several missed beats) as **not running**, so
//! the signal self-heals after a hard kill without any cleanup running.
//!
//! External observers (the `codesense-*` hooks / `sense-*` skills of AC-4, or
//! any sibling process) read this file to degrade gracefully when the app is
//! down. The in-app / menu-bar visual indicator is the UX-led surface that
//! renders this same state.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::commands::paths::codesense_home_dir;

/// How often the running app rewrites the heartbeat.
pub const HEARTBEAT_INTERVAL_MS: u64 = 5_000;
/// A heartbeat older than this is treated as dead — ~3 missed beats, so a
/// brief GC pause or scheduling hiccup doesn't flap the signal, but a real
/// crash surfaces as "not running" within a few seconds.
///
/// Part of the **liveness-reader API** ([`read_liveness_from`] /
/// [`current_liveness`] / [`Liveness`]): the data layer the UX-deferred
/// in-app indicator and external observers consume, and which the unit tests
/// exercise to prove crash-honesty. The binary itself only *writes* the
/// heartbeat today, so the reader side carries `#[allow(dead_code)]` until the
/// indicator lands.
#[allow(dead_code)]
pub const HEARTBEAT_STALE_AFTER_MS: u64 = 15_000;

/// On-disk heartbeat record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heartbeat {
    /// OS process id of the running app (diagnostic; not used for liveness).
    pub pid: u32,
    /// Wall-clock ms-since-epoch when this app process started.
    pub started_at_ms: u64,
    /// Wall-clock ms-since-epoch of the most recent beat. Staleness of this
    /// field — not the file's existence — is what makes liveness crash-honest.
    pub updated_at_ms: u64,
}

/// Crash-honest running/not-running verdict derived from the heartbeat.
/// Reader-side API — see the [`HEARTBEAT_STALE_AFTER_MS`] note.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    Running,
    NotRunning,
}

/// `~/.codesense/runtime/` — codesense's own runtime-state dir. Never a user
/// repo.
pub fn runtime_dir() -> Option<PathBuf> {
    codesense_home_dir().map(|h| h.join("runtime"))
}

/// `~/.codesense/runtime/heartbeat.json`.
pub fn heartbeat_path() -> Option<PathBuf> {
    runtime_dir().map(|d| d.join("heartbeat.json"))
}

/// Wall-clock ms since the Unix epoch. Shared with the renderer watchdog
/// (SW-FX-64), which reuses this rather than re-deriving the same clock.
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Write `hb` to `path` (creating the runtime dir as needed) via a
/// tmp-then-rename so a reader never observes a half-written file. The tmp
/// name carries the writer's pid so two app instances (e.g. an old process
/// overlapping a restart) can't clobber each other's tmp before the rename.
pub fn write_heartbeat_to(path: &Path, hb: &Heartbeat) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(hb)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let file_name = path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("heartbeat.json");
    let tmp = path.with_file_name(format!("{file_name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Read the heartbeat at `path` and decide liveness against `now_ms`. A
/// missing file, an unparseable file, or a heartbeat older than
/// `stale_after_ms` all resolve to [`Liveness::NotRunning`].
#[allow(dead_code)] // reader-side API; see HEARTBEAT_STALE_AFTER_MS note.
pub fn read_liveness_from(path: &Path, now_ms: u64, stale_after_ms: u64) -> Liveness {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return Liveness::NotRunning,
    };
    let hb: Heartbeat = match serde_json::from_slice(&bytes) {
        Ok(h) => h,
        Err(_) => return Liveness::NotRunning,
    };
    if now_ms.saturating_sub(hb.updated_at_ms) <= stale_after_ms {
        Liveness::Running
    } else {
        Liveness::NotRunning
    }
}

/// Read the canonical heartbeat and decide liveness now. Convenience over
/// [`read_liveness_from`] for production callers.
#[allow(dead_code)] // reader-side API; see HEARTBEAT_STALE_AFTER_MS note.
pub fn current_liveness() -> Liveness {
    match heartbeat_path() {
        Some(p) => read_liveness_from(&p, now_ms(), HEARTBEAT_STALE_AFTER_MS),
        None => Liveness::NotRunning,
    }
}

/// Remove the heartbeat file (graceful-exit cleanup). Best-effort and
/// idempotent — staleness already covers the crash path, so a failure here
/// is harmless.
pub fn clear_heartbeat() {
    if let Some(p) = heartbeat_path() {
        let _ = std::fs::remove_file(p);
    }
}

/// Start the periodic heartbeat writer. Writes an immediate beat, then
/// refreshes every [`HEARTBEAT_INTERVAL_MS`] on a background task for the life
/// of the process. Called once from Tauri `setup()`. No-op if the home dir
/// can't be resolved.
pub fn start_heartbeat() {
    let Some(path) = heartbeat_path() else {
        log::warn!(target: "runtime", "heartbeat: cannot resolve ~/.codesense; liveness signal disabled");
        return;
    };
    let pid = std::process::id();
    let started = now_ms();
    if let Err(e) = write_heartbeat_to(
        &path,
        &Heartbeat {
            pid,
            started_at_ms: started,
            updated_at_ms: started,
        },
    ) {
        log::warn!(target: "runtime", "heartbeat: initial write failed: {e}");
    }
    tauri::async_runtime::spawn(async move {
        let mut interval =
            tokio::time::interval(std::time::Duration::from_millis(HEARTBEAT_INTERVAL_MS));
        loop {
            interval.tick().await;
            if let Err(e) = write_heartbeat_to(
                &path,
                &Heartbeat {
                    pid,
                    started_at_ms: started,
                    updated_at_ms: now_ms(),
                },
            ) {
                log::warn!(target: "runtime", "heartbeat: refresh write failed: {e}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn hb(updated_at_ms: u64) -> Heartbeat {
        Heartbeat {
            pid: 1234,
            started_at_ms: 1_000,
            updated_at_ms,
        }
    }

    #[test]
    fn fresh_heartbeat_reads_running() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("runtime/heartbeat.json");
        write_heartbeat_to(&path, &hb(100_000)).unwrap();
        // now is only 2s after the beat → well within the 15s stale window.
        assert_eq!(
            read_liveness_from(&path, 102_000, HEARTBEAT_STALE_AFTER_MS),
            Liveness::Running
        );
    }

    #[test]
    fn stale_heartbeat_reads_not_running() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("runtime/heartbeat.json");
        write_heartbeat_to(&path, &hb(100_000)).unwrap();
        // 20s later, no refresh (process died) → stale → not running.
        assert_eq!(
            read_liveness_from(&path, 120_000, HEARTBEAT_STALE_AFTER_MS),
            Liveness::NotRunning
        );
    }

    #[test]
    fn missing_heartbeat_reads_not_running() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("runtime/heartbeat.json");
        assert_eq!(
            read_liveness_from(&path, 1, HEARTBEAT_STALE_AFTER_MS),
            Liveness::NotRunning
        );
    }

    #[test]
    fn corrupt_heartbeat_reads_not_running() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("runtime/heartbeat.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(
            read_liveness_from(&path, 1, HEARTBEAT_STALE_AFTER_MS),
            Liveness::NotRunning
        );
    }

    #[test]
    fn clear_then_read_is_not_running() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("runtime/heartbeat.json");
        write_heartbeat_to(&path, &hb(100_000)).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            read_liveness_from(&path, 100_001, HEARTBEAT_STALE_AFTER_MS),
            Liveness::NotRunning
        );
    }
}
