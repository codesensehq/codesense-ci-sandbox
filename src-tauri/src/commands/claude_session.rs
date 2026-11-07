// SPDX-License-Identifier: MIT
//! Claude live-session watcher — resolves the session-id Claude Code writes to
//! `~/.claude/sessions/*.json` after a spawn, so a launched TUI can be correlated
//! to its on-disk session.
//!
//! # AGENT-NEUTRAL DEFERRED (SW-AN-2 G-b)
//!
//! This module is intentionally **Claude-only for now** and is NOT routed through
//! the [`adapter`](crate::agents::adapter) seam. Rationale (decided in
//! SW-AN-2, AC-8): unlike the *running-detection* signature — already generic via
//! the adapter's `binary` + `codesense_pty_core::process::agent_signature_matches`
//! — this watcher reads a **Claude-proprietary session-file FORMAT**
//! ([`SessionInfo`]: `{pid, session_id, cwd, started_at}` written to
//! `~/.claude/sessions/`). No other agent produces that file, so parameterizing
//! only the *path* would be hollow generality with zero consumer (Rule of Three /
//! YAGNI). True neutrality needs a per-adapter session-file *format* abstraction,
//! which is deferred to a fast-follow story when a second agent with a
//! discoverable session file actually appears. Each Claude-specific site below is
//! marked `AGENT-NEUTRAL DEFERRED (SW-AN-2 G-b)` so the follow-up has a grep anchor.

use log::{error, warn};
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

use crate::commands::async_helpers::await_blocking;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub pid: u32,
    pub session_id: String,
    pub cwd: String,
    pub started_at: u64,
}

// AGENT-NEUTRAL DEFERRED (SW-AN-2 G-b): these Tauri event names are
// Claude-specific by design. A future per-pack session-file abstraction would
// derive them from the pack; today Claude is the only producer.
const EVENT_SESSION_FOUND: &str = "claude-session-found";
const EVENT_SESSION_TIMEOUT: &str = "claude-session-timeout";

// AGENT-NEUTRAL DEFERRED (SW-AN-2 G-b): the `~/.claude/sessions` path is
// hardcoded to Claude's session-file location on purpose — no other agent writes
// this format today (see the module header for the deferral rationale).
fn sessions_dir() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(crate::context_repo::CLAUDE_CONFIG_DIR)
        .join("sessions")
}

/// Scan a directory for a session file matching CWD and started after a given timestamp.
/// Returns the newest matching session. Exported for testing.
pub fn find_session_in_dir(dir: &Path, cwd: &str, started_after_ms: u64) -> Option<SessionInfo> {
    let entries = fs::read_dir(dir).ok()?;
    let canonical_cwd = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.into());

    let mut best: Option<SessionInfo> = None;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let session: SessionInfo = match serde_json::from_str(&content) {
            Ok(s) => s,
            Err(_) => continue,
        };

        if session.started_at < started_after_ms {
            continue;
        }

        let session_cwd =
            fs::canonicalize(&session.cwd).unwrap_or_else(|_| session.cwd.clone().into());

        if session_cwd != canonical_cwd {
            continue;
        }

        // Keep the newest match
        match &best {
            Some(existing) if existing.started_at >= session.started_at => {}
            _ => best = Some(session),
        }
    }

    best
}

use crate::commands::weak_registry::WeakArcRegistry;

/// State for session file watchers — keyed by CWD for multi-repo support.
///
/// RF-perf-8: self-pruning via [`WeakArcRegistry`] — the watcher thread owns the
/// only strong `Arc` to its cancel flag, so the entry evaporates when the watcher
/// ends (found/timeout/cancel), bounding what was `O(ever-seen terminal CWD)`.
#[derive(Default)]
pub struct SessionWatcherState {
    pub cancels: WeakArcRegistry<AtomicBool>,
}

impl SessionWatcherState {
    /// Entry count for the RF-perf-5 memory gate (`try_lock`-or-skip). RF-perf-8.
    pub fn len(&self) -> Option<usize> {
        self.cancels.len()
    }
}

/// Watch ~/.claude/sessions/ for a new session file matching the CWD and spawn timestamp.
/// Emits "claude-session-found" (with cwd in payload) when found, or "claude-session-timeout" on timeout.
/// Scoped per CWD — safe for multi-repo.
#[tauri::command]
pub async fn watch_for_claude_session(
    app: AppHandle,
    cwd: String,
    started_after_ms: u64,
) -> Result<(), String> {
    // NB-7 / Task 3.10: cancel-bookkeeping + initial dir scan run on the
    // blocking pool. The `thread::spawn` block below moves a freshly-cloned
    // `app` into a long-lived thread; that thread is independent of the
    // command future and doesn't need await_blocking. Only the synchronous
    // fs work (create_dir_all + find_session_in_dir) up to the watcher
    // spawn benefits from spawn_blocking — and it's where the UI-thread
    // wedge can happen on cold start.
    let app_for_pre = app.clone();
    let cwd_for_pre = cwd.clone();
    let (cancel, dir, found_already) = await_blocking::<
        (Arc<AtomicBool>, std::path::PathBuf, Option<SessionInfo>),
        _,
    >("watch_for_claude_session/preflight", move || {
        let watcher_state = app_for_pre.state::<SessionWatcherState>();

        // RF-perf-8: atomically install a fresh cancel flag for this CWD and
        // cancel any live prior watcher, in ONE lock acquisition — no
        // check-then-act window (AC-4). The strong `Arc` is moved into the watcher
        // thread below, so the map's weak self-evicts when the watcher ends.
        let (cancel, previous) = watcher_state
            .cancels
            .swap(&cwd_for_pre, || Arc::new(AtomicBool::new(false)));
        if let Some(previous) = previous {
            previous.store(true, Ordering::SeqCst);
        }

        let dir = sessions_dir();
        if !dir.exists() {
            fs::create_dir_all(&dir)
                .map_err(|e| format!("Failed to create sessions dir: {}", e))?;
        }

        let already = find_session_in_dir(&dir, &cwd_for_pre, started_after_ms);
        Ok((cancel, dir, already))
    })
    .await?;

    if let Some(session) = found_already {
        let _ = app.emit(EVENT_SESSION_FOUND, session);
        return Ok(());
    }

    let cwd_for_thread = cwd.clone();
    // Spawn a background thread with the watcher
    thread::spawn(move || {
        let timeout = Duration::from_secs(60);
        let start = Instant::now();

        let (tx, rx) = std::sync::mpsc::channel();
        let mut watcher = match RecommendedWatcher::new(tx, Config::default()) {
            Ok(w) => w,
            Err(e) => {
                error!(target: "session", "Failed to create session watcher: {}", e);
                return;
            }
        };

        if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
            error!(target: "session", "Failed to watch sessions dir: {}", e);
            return;
        }

        let mut found = false;
        loop {
            if cancel.load(Ordering::SeqCst) {
                break;
            }

            if start.elapsed() >= timeout {
                break;
            }

            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(Ok(_event)) => {
                    if let Some(session) =
                        find_session_in_dir(&dir, &cwd_for_thread, started_after_ms)
                    {
                        let _ = app.emit(EVENT_SESSION_FOUND, session);
                        found = true;
                        break;
                    }
                }
                Ok(Err(e)) => {
                    warn!(target: "session", "Watch error: {}", e);
                    break;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if let Some(session) =
                        find_session_in_dir(&dir, &cwd_for_thread, started_after_ms)
                    {
                        let _ = app.emit(EVENT_SESSION_FOUND, session);
                        found = true;
                        break;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        // Emit timeout event if we didn't find anything
        if !found && !cancel.load(Ordering::SeqCst) {
            let _ = app.emit(EVENT_SESSION_TIMEOUT, cwd_for_thread);
        }
    });

    Ok(())
}

/// Cancel the session watcher for a specific CWD
#[tauri::command]
pub fn cancel_session_watcher(app: AppHandle, cwd: String) {
    let watcher_state = app.state::<SessionWatcherState>();
    // RF-perf-8: `get` upgrades-or-None — a cancel after the watcher already tore
    // down is a safe no-op. O(1), never sweeps (safe on the main thread).
    if let Some(cancel) = watcher_state.cancels.get(&cwd) {
        cancel.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_session(dir: &Path, pid: u32, session_id: &str, cwd: &str, started_at: u64) {
        let content = serde_json::json!({
            "pid": pid,
            "sessionId": session_id,
            "cwd": cwd,
            "startedAt": started_at,
            "kind": "interactive",
            "entrypoint": "cli"
        });
        fs::write(dir.join(format!("{}.json", pid)), content.to_string()).unwrap();
    }

    #[test]
    fn test_find_session_in_dir_matches_cwd_and_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_str().unwrap();

        // Session before our timestamp — should NOT match
        write_session(dir.path(), 100, "old-id", cwd, 1000);
        // Session after our timestamp — should match
        write_session(dir.path(), 200, "new-id", cwd, 2000);

        let result = find_session_in_dir(dir.path(), cwd, 1500);
        assert!(result.is_some());
        assert_eq!(result.unwrap().session_id, "new-id");
    }

    #[test]
    fn test_find_session_in_dir_picks_newest() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_str().unwrap();

        write_session(dir.path(), 100, "first", cwd, 2000);
        write_session(dir.path(), 200, "second", cwd, 3000);

        let result = find_session_in_dir(dir.path(), cwd, 1500);
        assert!(result.is_some());
        assert_eq!(result.unwrap().session_id, "second");
    }

    #[test]
    fn test_find_session_in_dir_filters_wrong_cwd() {
        let dir = tempfile::tempdir().unwrap();

        write_session(dir.path(), 100, "wrong-cwd", "/some/other/path", 2000);

        let result = find_session_in_dir(dir.path(), dir.path().to_str().unwrap(), 1500);
        assert!(result.is_none());
    }

    #[test]
    fn test_find_session_in_dir_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = find_session_in_dir(dir.path(), "/any/path", 0);
        assert!(result.is_none());
    }

    #[test]
    fn test_find_session_in_dir_nonexistent_dir() {
        let result = find_session_in_dir(Path::new("/nonexistent/path"), "/any", 0);
        assert!(result.is_none());
    }
}
