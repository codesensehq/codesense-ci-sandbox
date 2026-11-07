// SPDX-License-Identifier: MIT
//! The agent session broadcast hub. Agent signals (`signal_done`,
//! `post_agent_message`, `signal_progress`, routed from `mcp/handler.rs`) fan in
//! as [`StreamEvent::Session`]; in-process consumers (the routine runtime's
//! agent-done intake) subscribe. The hub also owns the scoped read-token store.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

use super::token::ScopedCapabilityStore;
use super::{SessionStatus, StatusDelta};

/// Broadcast channel depth: absorbs a burst without lagging a briefly-busy consumer.
const CAPACITY: usize = 256;

/// One item on the broadcast channel.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    Session { delta: StatusDelta },
}

struct HubInner {
    tx: broadcast::Sender<StreamEvent>,
    tokens: ScopedCapabilityStore,
    /// Each session's latest `signal_progress`, by session id, so a read (the
    /// `list_sessions` row, a dashboard's `sessions()`) shows where it is now.
    latest_progress: Mutex<HashMap<String, SessionProgress>>,
    /// Each session's latest `signal_done` (status and summary), by session
    /// id: SW-AGR-19's resume brief says where it stood.
    latest_done: Mutex<HashMap<String, (String, Option<String>, i64)>>,
}

/// A session's latest `signal_progress`: `phase` is `planning` | `dev` | `review` |
/// `pr` | `blocked` | `done`; `step`/`total` an N-of-M sub-progress; `atMs` when it
/// was reported (epoch milliseconds).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionProgress {
    pub phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub at_ms: i64,
}

/// Cheaply-cloneable handle (Arc-backed) for Tauri managed state.
#[derive(Clone)]
pub struct DashboardStreamHub {
    inner: Arc<HubInner>,
}

impl DashboardStreamHub {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(CAPACITY);
        Self {
            inner: Arc::new(HubInner {
                tx,
                tokens: ScopedCapabilityStore::default(),
                latest_progress: Mutex::new(HashMap::new()),
                latest_done: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// The scoped read-token store (minted per dashboard render).
    pub fn tokens(&self) -> &ScopedCapabilityStore {
        &self.inner.tokens
    }

    /// A fresh receiver of session events.
    pub fn subscribe(&self) -> broadcast::Receiver<StreamEvent> {
        self.inner.tx.subscribe()
    }

    /// An agent `signal_done` — broadcast a session-status delta. `needs-attention`
    /// is carried verbatim (never collapsed to `failed`). Unknown status (already
    /// rejected by the MCP handler) is a no-op.
    pub fn on_session_status(&self, session_id: &str, status: &str, summary: Option<String>) {
        let Some(parsed) = SessionStatus::from_wire(status) else {
            return;
        };
        if let Ok(mut latest) = self.inner.latest_done.lock() {
            latest.insert(
                session_id.to_string(),
                (status.to_string(), summary.clone(), now_ms()),
            );
        }
        let status = parsed;
        let _ = self.inner.tx.send(StreamEvent::Session {
            delta: StatusDelta::SessionStatus {
                session_id: session_id.to_string(),
                status,
                summary,
            },
        });
    }

    /// An agent `post_agent_message` — broadcast a session-message delta.
    pub fn on_session_message(&self, session_id: &str, role: &str, text: &str) {
        let _ = self.inner.tx.send(StreamEvent::Session {
            delta: StatusDelta::SessionMessage {
                session_id: session_id.to_string(),
                role: role.to_string(),
                text: text.to_string(),
            },
        });
    }

    /// An agent `signal_progress` (SW-GW-4) — broadcast a mid-run progress delta.
    /// Self-guarding like `on_session_status`: an unknown `phase` (already rejected
    /// by the MCP handler) is a no-op, never a broadcast of garbage.
    pub fn on_session_progress(
        &self,
        session_id: &str,
        phase: &str,
        step: Option<u32>,
        total: Option<u32>,
        note: Option<String>,
    ) {
        let Some(parsed) = crate::roles::Phase::from_wire(phase) else {
            return;
        };
        if let Ok(mut latest) = self.inner.latest_progress.lock() {
            latest.insert(
                session_id.to_string(),
                SessionProgress {
                    phase: phase.to_string(),
                    step,
                    total,
                    note: note.clone(),
                    at_ms: now_ms(),
                },
            );
        }
        let phase = parsed;
        let _ = self.inner.tx.send(StreamEvent::Session {
            delta: StatusDelta::SessionProgress {
                session_id: session_id.to_string(),
                phase,
                step,
                total,
                note,
            },
        });
    }
}

impl DashboardStreamHub {
    /// The latest `signal_progress` of `session_id`, if it reported any.
    pub fn latest_progress(&self, session_id: &str) -> Option<SessionProgress> {
        self.inner
            .latest_progress
            .lock()
            .ok()
            .and_then(|latest| latest.get(session_id).cloned())
    }
}

impl DashboardStreamHub {
    /// The latest `signal_done` of `session_id` (its status and summary), if
    /// it reported one.
    pub fn latest_done(&self, session_id: &str) -> Option<(String, Option<String>)> {
        self.inner.latest_done.lock().ok().and_then(|latest| {
            latest
                .get(session_id)
                .map(|(status, summary, _)| (status.clone(), summary.clone()))
        })
    }

    /// Keep completed work out of the next turn's title until fresh progress arrives.
    pub(crate) fn progress_note_for_title(&self, session_id: &str) -> Option<String> {
        let progress = self.latest_progress(session_id)?;
        if progress.phase == "blocked" {
            return None;
        }
        let latest = self.inner.latest_done.lock().ok()?;
        if latest
            .get(session_id)
            .is_some_and(|(_, _, at_ms)| progress.at_ms <= *at_ms)
        {
            return None;
        }
        progress.note
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

impl Default for DashboardStreamHub {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agr19_the_latest_done_is_kept_per_session() {
        let hub = DashboardStreamHub::new();
        assert_eq!(hub.latest_done("s1"), None);
        hub.on_session_status("s1", "failed", Some("first".into()));
        hub.on_session_status("s1", "success", Some("second".into()));
        hub.on_session_status("s1", "bogus", None);
        assert_eq!(
            hub.latest_done("s1"),
            Some(("success".into(), Some("second".into())))
        );
    }

    #[test]
    fn session_signals_reach_subscribers_and_unknown_values_are_dropped() {
        let hub = DashboardStreamHub::new();
        let mut rx = hub.subscribe();
        hub.on_session_status("s1", "needs-attention", Some("why".into()));
        hub.on_session_status("s1", "bogus", None);
        hub.on_session_message("s1", "agent", "hi");
        hub.on_session_progress("s1", "not-a-phase", None, None, None);
        match rx.try_recv().unwrap() {
            StreamEvent::Session {
                delta:
                    StatusDelta::SessionStatus {
                        status, summary, ..
                    },
            } => {
                assert_eq!(status, SessionStatus::NeedsAttention);
                assert_eq!(summary.as_deref(), Some("why"));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(
            rx.try_recv().unwrap(),
            StreamEvent::Session {
                delta: StatusDelta::SessionMessage { .. }
            }
        ));
        assert!(
            rx.try_recv().is_err(),
            "unknown status/phase are not broadcast"
        );
    }
}
