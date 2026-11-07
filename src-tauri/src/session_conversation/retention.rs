// SPDX-License-Identifier: MIT
//! Retention: a stored conversation past its retention is removed and marked `expired`. The
//! tool-call index stays, so names and counts outlive the text. Runs once at boot and then daily.

use std::path::Path;
use std::time::Duration;

use super::{conversation_file, conversations_dir, Policy};
use crate::session_usage::UsageStore;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// Expire every conversation past retention at `now_ms`; returns how many.
pub fn sweep_in(
    store_path: &Path,
    dir: &Path,
    now_ms: i64,
    policy: &Policy,
) -> Result<usize, String> {
    let store = UsageStore::open(store_path)?;
    let cutoff = now_ms - policy.retention_days * DAY_MS;
    let mut expired = 0;
    for session in store.expirable_conversations(cutoff)? {
        if let Some(file) = conversation_file(dir, &session) {
            match std::fs::remove_file(&file) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    // Keep the row `present` so the next sweep retries the removal.
                    log::warn!(target: "session_conversation", "expired conversation of {session} not removed: {e}");
                    continue;
                }
            }
        }
        store.mark_conversation_expired(&session)?;
        expired += 1;
    }
    Ok(expired)
}

fn sweep_now() {
    let (Ok(store), Some(dir)) = (crate::session_usage::store_path(), conversations_dir()) else {
        return;
    };
    let now = chrono::Utc::now().timestamp_millis();
    match sweep_in(&store, &dir, now, &Policy::default()) {
        Ok(0) => {}
        Ok(n) => log::info!(target: "session_conversation", "{n} stored conversation(s) expired"),
        Err(e) => log::warn!(target: "session_conversation", "retention sweep failed: {e}"),
    }
}

/// Sweep now, then every day. One background thread; a failure is logged and the next day retries.
pub fn start() {
    let spawned = std::thread::Builder::new()
        .name("session-conversation-retention".into())
        .spawn(|| loop {
            sweep_now();
            std::thread::sleep(Duration::from_secs(24 * 60 * 60));
        });
    if let Err(e) = spawned {
        log::warn!(target: "session_conversation", "retention sweep not started: {e}");
    }
}
