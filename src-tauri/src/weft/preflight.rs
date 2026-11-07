// SPDX-License-Identifier: MIT
//! SW-WV-F2 — the dispatch preflight: the one read the Board's dispatch form runs
//! before it enables "Dispatch & release".
//!
//! The preflight answers two yes/no questions and folds them into one verdict so
//! the operator sees a single status line, never two. Dispatch is ungated: any agent
//! may dispatch, and the queue-row CAS is the coordination.
//!
//! 1. **queue_row_ok** — when a `queue_id` is supplied (the form was opened from a
//!    Board row), that `wv_queue` row must still exist and be `state='pending'`.
//!    When no `queue_id` is supplied (opened from the header "+ Dispatch"), the check
//!    is skipped (`true`).
//! 2. **capacity_ok** — is the fleet under its live-worker ceiling? The `wv_roster`
//!    lifecycle has NO `idle` status (its live states are `provision`/`dispatch`/
//!    `signal`; a slot is freed by `handoff`/`deprovision` — see
//!    [`WV_WORKER_LIVE_STATES`]). There is no configured worker cap in F2's scope
//!    (a per-scope cap lands with B5/scopes), so capacity is currently always
//!    available: a first dispatch and every subsequent one is admitted. The
//!    dimension is kept in the verdict shape so B5 can gate it without a wire change.
//!
//! `ok = queue_row_ok && capacity_ok`. `message` names the first failing
//! dimension so the amber status line is specific.
//!
//! Every SQLite read runs inside an [`await_blocking`] closure
//! (`tokio::task::spawn_blocking` — safe-by-construction, no `RI-2-ALLOW` marker
//! needed; that marker is only for `with_blocking` escape-hatch mints, per
//! `commands/ri2_backstop.rs`). The command never blocks the async reactor.

use crate::commands::await_blocking;

/// The preflight verdict on the wire (camelCase). One compact line in the UI:
/// `ok` → green "Ready to dispatch"; `!ok` → amber `message`.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WvPreflightResult {
    pub ok: bool,
    pub queue_row_ok: bool,
    pub capacity_ok: bool,
    pub message: Option<String>,
}

impl WvPreflightResult {
    /// Fold the two dimensions into the verdict + the first-failing-dimension
    /// message (`None` when `ok`). Pure — unit-tested directly.
    fn from_dimensions(queue_row_ok: bool, capacity_ok: bool) -> Self {
        let ok = queue_row_ok && capacity_ok;
        let message = if ok {
            None
        } else if !queue_row_ok {
            Some("That board item is no longer pending — it may already be claimed.".to_owned())
        } else {
            Some("No idle worker slot is available right now.".to_owned())
        };
        Self {
            ok,
            queue_row_ok,
            capacity_ok,
            message,
        }
    }
}

/// The queue-row + capacity reads against the active team's `weft.sqlite3`.
/// Synchronous SQLite on a caller-supplied [`WeftStore`]; the caller runs it inside
/// `await_blocking`. Returns `(queue_row_ok, capacity_ok)`.
fn queue_and_capacity_blocking(
    store: &crate::weft::WeftStore,
    queue_id: Option<i64>,
) -> Result<(bool, bool), String> {
    use rusqlite::OptionalExtension;
    let conn = store.conn().map_err(|e| e.to_string())?;

    let queue_row_ok = match queue_id {
        Some(id) => {
            let found: Option<i64> = conn
                .query_row(
                    "SELECT 1 FROM wv_queue WHERE id=?1 AND state='pending'",
                    rusqlite::params![id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            found.is_some()
        }
        None => true,
    };

    // Capacity: the roster lifecycle has no `idle` state and F2 has no configured
    // worker cap (a per-scope cap arrives with B5/scopes), so capacity is always
    // available today. Reading the live-worker count here would gate nothing and only
    // risk a false block, so the check is a constant `true` until B5 wires a ceiling.
    let capacity_ok = true;

    Ok((queue_row_ok, capacity_ok))
}

/// SW-WV-F2 — resolve the dispatch preflight for the active team. `session_id` is
/// NOT a caller argument (the renderer must not be able to spoof session identity);
/// all reads run off the async/UI thread via `await_blocking`.
#[tauri::command]
pub async fn preflight_wv_dispatch(
    app: tauri::AppHandle,
    queue_id: Option<i64>,
    subclass: String,
) -> Result<WvPreflightResult, String> {
    // `subclass` is validated/consumed downstream by the dispatch call; the preflight
    // itself is subclass-agnostic (capacity is not yet per-subclass). Accept + ignore
    // so the FE contract carries it through one call shape.
    let _ = subclass;

    // The active team's weft.sqlite3 path (canonical, §1.1) — resolved on the async
    // side; the store is opened INSIDE the blocking closure.
    let path = crate::commands::weft_ledger::active_weft_store_path(&app).await?;

    await_blocking("preflight_wv_dispatch", move || {
        let store =
            crate::weft::WeftStore::open(path).map_err(|e| format!("open weft store: {e}"))?;
        let (queue_row_ok, capacity_ok) = queue_and_capacity_blocking(&store, queue_id)?;
        Ok(WvPreflightResult::from_dimensions(
            queue_row_ok,
            capacity_ok,
        ))
    })
    .await
}

#[cfg(test)]
mod tests;
