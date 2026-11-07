// SPDX-License-Identifier: MIT
//! The `agents:changed` app event: emitted after every agent definition write
//! (save, archive, unarchive, restore, delete, copy) and every change to a
//! user's account binding, whichever surface made it (the Agents tab or the
//! MCP tools), so any view of agents knows to re-read.
//!
//! The agent service is `AppHandle`-free, so the app installs the emitter once
//! at startup (the `routines::events` idiom). Before that, and in tests, a
//! change notifies nobody.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// The app event name.
pub const AGENTS_CHANGED: &str = "agents:changed";

/// The event payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentChanged {
    /// The rendered key of the agent that changed.
    pub key: String,
    /// SW-XS-15: the revision a definition write produced, so a view that made the
    /// write can tell its own echo (already shown) from someone else's change.
    /// Absent for changes that make no revision (a binding).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<u32>,
}

type Sink = Box<dyn Fn(&AgentChanged) + Send + Sync>;

static SINK: OnceLock<Sink> = OnceLock::new();

/// Install the process's emitter. Only the first install takes effect.
pub fn install(sink: impl Fn(&AgentChanged) + Send + Sync + 'static) {
    let _ = SINK.set(Box::new(sink));
}

/// Install the app emitter for `agents:changed`.
pub fn install_app_emitter(app: tauri::AppHandle) {
    use tauri::Emitter;
    install(move |change| {
        if let Err(e) = app.emit(AGENTS_CHANGED, change) {
            log::debug!(target: "agents", "agents:changed emit failed: {e}");
        }
    });
}

/// Tell any listener that agent `key` (or this user's binding for it) changed.
pub fn changed(key: &str) {
    emit(key, None);
}

/// [`changed`], for a definition write that produced `revision`.
pub fn changed_at(key: &str, revision: u32) {
    emit(key, Some(revision));
}

fn emit(key: &str, revision: Option<u32>) {
    crate::session_usage::spend_caps::changed();
    if let Some(sink) = SINK.get() {
        sink(&AgentChanged {
            key: key.to_string(),
            revision,
        });
    }
}

#[cfg(test)]
pub(crate) mod recorder {
    use std::sync::Mutex;

    use super::AgentChanged;

    /// Every `agents:changed` this test process emitted. The sink is installed
    /// on first use; nothing else installs one under test.
    static CHANGES: Mutex<Vec<AgentChanged>> = Mutex::new(Vec::new());

    /// Start recording `agents:changed`.
    pub(crate) fn record() {
        static INSTALL: std::sync::Once = std::sync::Once::new();
        INSTALL.call_once(|| {
            super::install(|change| {
                CHANGES
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(change.clone())
            })
        });
    }

    /// How many `agents:changed` were emitted for `key` so far. Tests running
    /// in parallel share the sink, so each uses its own agent ids.
    pub(crate) fn count(key: &str) -> usize {
        CHANGES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|c| c.key == key)
            .count()
    }

    pub(crate) fn last(key: &str) -> AgentChanged {
        CHANGES
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|c| c.key == key)
            .unwrap()
            .clone()
    }
}
