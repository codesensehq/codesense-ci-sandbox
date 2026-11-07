// SPDX-License-Identifier: MIT
//! The delivery core's read-model fields (SPEC §7, §9.11): what a routine row
//! shows while its wake waits for a busy session (`⏰ due`). SW-AGR-2 attaches
//! [`QueuedWake`] as `agent_get.routines[].queued`; the shape is pinned by
//! `src-tauri/src/fixtures/wake/agent-routines-queued.json`.

use serde::{Deserialize, Serialize};

use super::events::WakeOrigin;
use super::{lock, DeliveryService};

/// A routine's wake that is queued on a session, waiting for it to be idle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QueuedWake {
    pub run_id: String,
    pub machine_id: String,
    pub session_id: String,
    pub marker: String,
    /// Host clock of the queued deadline; `None` until the host answered.
    pub deadline_ms: Option<u64>,
    /// Sent but never answered (the reply was lost): it may be queued,
    /// typed, or nowhere. Shown as "may be due", never as queued for sure.
    pub unconfirmed: bool,
}

impl DeliveryService {
    /// The wake of `routine_key` still queued, if any: a confirmed one before
    /// an unanswered one, the newest first; expired entries never show.
    pub(crate) fn queued_for_routine(&self, routine_key: &str) -> Option<QueuedWake> {
        let now = (self.clock)();
        let hosts = lock(&self.hosts);
        let mut queued: Vec<(bool, u64, QueuedWake)> = hosts
            .iter()
            .flat_map(|(machine, record)| {
                record.pending.values().filter_map(move |pending| {
                    let WakeOrigin::Routine {
                        routine_key: key,
                        run_id,
                    } = &pending.origin
                    else {
                        return None;
                    };
                    (key == routine_key && !pending.expired(now)).then(|| {
                        (
                            pending.unconfirmed,
                            pending.asked_at_ms,
                            QueuedWake {
                                run_id: run_id.clone(),
                                machine_id: machine.clone(),
                                session_id: pending.session.clone(),
                                marker: pending.marker.clone(),
                                deadline_ms: pending.deadline_ms,
                                unconfirmed: pending.unconfirmed,
                            },
                        )
                    })
                })
            })
            .collect();
        // A confirmed queued wake before an unanswered one, newest first: a
        // stale entry never hides a real queued wake.
        queued.sort_by_key(|(unconfirmed, asked, _)| (*unconfirmed, std::cmp::Reverse(*asked)));
        queued.into_iter().next().map(|(_, _, wake)| wake)
    }
}

/// `agent_get.routines[]`'s `queued` field for `routine_key`: `null` when
/// nothing waits, or when the delivery core is not running.
pub(crate) fn routine_queued_field(
    service: Option<&DeliveryService>,
    routine_key: &str,
) -> serde_json::Value {
    service
        .and_then(|service| service.queued_for_routine(routine_key))
        .map(|wake| serde_json::to_value(wake).unwrap_or(serde_json::Value::Null))
        .unwrap_or(serde_json::Value::Null)
}

/// Fill `agent_get`'s `routines[].queued` from the delivery core.
pub(crate) fn attach_queued(
    got: &mut crate::mcp::agent_tools::AgentGot,
    service: Option<&DeliveryService>,
) {
    for row in &mut got.routines {
        row.queued = service.and_then(|service| service.queued_for_routine(&row.id));
    }
}
