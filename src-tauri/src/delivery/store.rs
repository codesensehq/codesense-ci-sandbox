// SPDX-License-Identifier: MIT
//! What the desktop keeps per terminal server across app restarts:
//!
//! - the outcome-log cursor (`WakeCursor`): the server lifetime last read, the
//!   log position, and the delivery ids already taken, so a restarted app
//!   catches up exactly once and a restarted server is re-read from 0;
//! - the deliveries this desktop asked for that are not final yet, each with
//!   the server lifetime that holds it, so their outcome finds its run, a
//!   queued wake still shows on an offline machine's agent after an app
//!   restart, and only a wake the old lifetime held is called lost when the
//!   server restarts;
//! - each tab's wake history (last [`WAKE_HISTORY_CAP`]).
//!
//! One JSON file per machine under `{codesense_home}/wake/hosts/`, written
//! whole through an fsynced temp file and a rename. An unreadable file starts
//! over in *replay*: the next catch-up is shown, never recorded as runs again.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::events::{WakeHistoryEntry, WakeOrigin, WAKE_HISTORY_CAP};
use crate::terminal::session_server::session_wake::WakeCursor;

/// How many tabs' histories a machine's record keeps (most recently woken).
const HISTORY_SESSIONS_CAP: usize = 200;

/// A delivery this desktop asked for whose outcome is not final yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PendingDelivery {
    /// `None` while the host has not answered.
    pub delivery_id: Option<String>,
    pub session: String,
    pub idempotency_key: String,
    pub marker: String,
    pub origin: WakeOrigin,
    /// Host clock of the queued deadline, once known.
    pub deadline_ms: Option<u64>,
    /// The server lifetime that holds it: the one that answered, or, until it
    /// answers, the one this desktop last read.
    #[serde(default)]
    pub host_boot_id: Option<String>,
    /// Sent, but the host never answered (the reply was lost): it may be
    /// queued, typed, or nowhere.
    #[serde(default)]
    pub unconfirmed: bool,
    /// When this desktop asked (its own clock; only ever compared locally).
    pub asked_at_ms: u64,
    /// How long it may wait for its session to be idle.
    #[serde(default)]
    pub max_wait_ms: u64,
}

/// How long past its wait a pending wake may still be reported before this
/// desktop calls it `unknown`: the server's own write and report, and a
/// catch-up's round trip, fit well inside it.
pub(crate) const EXPIRY_GRACE_MS: u64 = 10 * 60 * 1000;

impl PendingDelivery {
    /// Past its wait and the grace, at desktop time `now_ms`: its outcome is
    /// not coming (a lost reply, an outcome dropped from an overflowing log).
    pub(crate) fn expired(&self, now_ms: u64) -> bool {
        self.asked_at_ms
            .saturating_add(self.max_wait_ms)
            .saturating_add(EXPIRY_GRACE_MS)
            < now_ms
    }
}

/// Everything kept for one machine.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct HostRecord {
    /// The machine id this record belongs to.
    pub machine: String,
    pub cursor: WakeCursor,
    /// By [`pending_key`].
    pub pending: BTreeMap<String, PendingDelivery>,
    /// By session id; oldest first.
    pub history: BTreeMap<String, VecDeque<WakeHistoryEntry>>,
    /// Session ids in the order they were last woken (for the history cap).
    pub history_order: VecDeque<String>,
    /// The record was lost (unreadable): the next catch-up re-reads outcomes
    /// already recorded once, so it is shown but never routed as runs.
    pub replaying: bool,
}

/// A pending delivery's key: idempotency keys are scoped to their session.
pub(crate) fn pending_key(session: &str, idempotency_key: &str) -> String {
    format!("{session}\u{1f}{idempotency_key}")
}

impl HostRecord {
    pub(crate) fn push_history(&mut self, session: &str, entry: WakeHistoryEntry) {
        let rows = self.history.entry(session.to_string()).or_default();
        rows.retain(|row| entry.delivery_id.is_empty() || row.delivery_id != entry.delivery_id);
        rows.push_back(entry);
        while rows.len() > WAKE_HISTORY_CAP {
            rows.pop_front();
        }
        self.history_order.retain(|s| s != session);
        self.history_order.push_back(session.to_string());
        while self.history_order.len() > HISTORY_SESSIONS_CAP {
            if let Some(old) = self.history_order.pop_front() {
                self.history.remove(&old);
            }
        }
    }

    /// Remember that `delivery_id` was taken, within the cursor's cap.
    /// `false` when it was taken before (or has no id).
    pub(crate) fn mark_seen(&mut self, delivery_id: &str) -> bool {
        if delivery_id.is_empty() || self.cursor.seen.iter().any(|seen| seen == delivery_id) {
            return false;
        }
        self.cursor.seen.push_back(delivery_id.to_string());
        while self.cursor.seen.len() > codesense_pty_core::session_wake::OUTCOME_LOG_CAP {
            self.cursor.seen.pop_front();
        }
        true
    }
}

/// Where host records live; `None` keeps them in memory only.
#[derive(Debug, Clone)]
pub(crate) struct RecordDir(Option<PathBuf>);

impl RecordDir {
    pub(crate) fn at(dir: Option<PathBuf>) -> Self {
        Self(dir)
    }

    pub(crate) fn under_home(codesense_home: &Path) -> Self {
        Self(Some(codesense_home.join("wake").join("hosts")))
    }

    fn path(&self, machine: &str) -> Option<PathBuf> {
        let name: String = machine
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        // Distinct ids that sanitize alike keep distinct files.
        let digest = &sha256_hex(machine)[..12];
        Some(self.0.as_ref()?.join(format!("{name}-{digest}.json")))
    }

    /// The machine's record; an empty one when none was kept, and an empty
    /// one in replay when it is unreadable (a torn or hand-edited file costs
    /// one full re-read that is shown, never routed as runs).
    pub(crate) fn load(&self, machine: &str) -> HostRecord {
        let fresh = || HostRecord {
            machine: machine.to_string(),
            ..HostRecord::default()
        };
        let Some(path) = self.path(machine) else {
            return fresh();
        };
        match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<HostRecord>(&bytes) {
                Ok(mut record) => {
                    record.machine = machine.to_string();
                    record
                }
                Err(error) => {
                    log::warn!(target: "wake", "wake record {path:?} unreadable, replaying: {error}");
                    HostRecord {
                        replaying: true,
                        ..fresh()
                    }
                }
            },
            Err(_) => fresh(),
        }
    }

    /// Every readable record kept (so an offline machine's queued wakes show
    /// after an app restart, before it reconnects).
    pub(crate) fn load_all(&self) -> Vec<HostRecord> {
        let Some(dir) = &self.0 else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .filter_map(|entry| std::fs::read(entry.path()).ok())
            .filter_map(|bytes| serde_json::from_slice::<HostRecord>(&bytes).ok())
            .filter(|record| !record.machine.is_empty())
            .collect()
    }

    /// Write the record durably (fsynced temp file, then rename).
    pub(crate) fn save(&self, machine: &str, record: &HostRecord) -> Result<(), String> {
        let Some(path) = self.path(machine) else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{dir:?}: {e}"))?;
        }
        let bytes = serde_json::to_vec(record).map_err(|e| e.to_string())?;
        crate::content_engine::atomic_io::write_tmp_fsync_rename(&path, &bytes)
            .map_err(|e| format!("{path:?}: {e}"))
    }
}

fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
