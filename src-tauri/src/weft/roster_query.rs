// SPDX-License-Identifier: MIT
//! SW-WV-F3 — the fleet-view Tauri command surface: keyed roster lookup and the
//! operator force-close gate.
//!
//! These are thin, OBSERVE-half doors (ARCHITECTURE §1.6 — never drive): each
//! resolves the active team's `weft.sqlite3` handle (or the opaque worker handle)
//! and calls a sanctioned `weft::` read/write handler OFF the async/UI thread via
//! [`await_blocking`]. No raw `wv_*` SQL lives here — the reads funnel through
//! `weft::sqlite` (MODULE-HOME §5.3) and the one force-close WRITE reuses the A4
//! silence-flag single-writer (`wv_worker_needs_attention_by_session`), so the sole
//! `wv_roster` write path stays A4's audited CAS.
//!
//! ## Status vocabulary (grounded against the shipped A2/A3/A4 schema)
//!
//! The spec draft named the roster `status` values `dispatched`/`signalled`/… — but
//! the shipped `wv_roster.status` column holds the LIFECYCLE strings
//! `provision`/`dispatch`/`signal`/`handoff`/`deprovision` (plus the out-of-band
//! `needs-attention`). This module reads those; the FE close-gate keys on
//! `status === "dispatch"`.
//!
//! ## Join column (grounded against DW-B3-3)
//!
//! The FE holds the live PTY `sessionId`, and the roster gained a `session_id`
//! column (DW-B3-3) that A2's dispatch transition stamps. The keyed lookup is
//! therefore `WHERE session_id=?1` — NOT the `pane` column the pre-DW-B3-3 spec
//! draft named.

use serde::Serialize;
use tauri::Emitter;

use crate::commands::await_blocking;
use crate::commands::weft_ledger::active_weft_store_path;

/// SW-WV-F3 (AC-1/AC-2/AC-3): the four display fields the fleet-view row projects for
/// one confirmed Weft worker. `None` for a non-Weft session (no roster row) — the row
/// then renders identically to today (AC-7).
#[derive(Debug, Clone, Serialize)]
pub struct WvRosterRowView {
    /// Weft job-shape label: `impl`|`review`|`planning`|`research`|custom.
    pub subclass: Option<String>,
    /// Engine name — mirrors `AgentSessionRow.kind`.
    pub kind: Option<String>,
    /// Goal field from the 4-field dispatch brief (the backlink line).
    pub current_task: Option<String>,
    /// Lifecycle state from `wv_roster` (drives the close gate).
    pub status: Option<String>,
}

impl From<crate::weft::WvRosterView> for WvRosterRowView {
    fn from(v: crate::weft::WvRosterView) -> Self {
        Self {
            subclass: v.subclass,
            kind: v.kind,
            current_task: v.current_task,
            status: v.status,
        }
    }
}

/// SW-WV-F3 (AC-1/AC-2/AC-3): read the fleet-view roster projection for one live
/// session, keyed by its PTY `session_id`. `Ok(None)` for a non-Weft session — a
/// missing `weft.sqlite3` or no matching row is graceful, never an error. All SQLite
/// I/O runs OFF the async/UI thread via [`await_blocking`] (AC-6).
#[tauri::command]
pub async fn wv_roster_for_session(
    session_id: String,
    app: tauri::AppHandle,
) -> Result<Option<WvRosterRowView>, String> {
    // A non-Weft team has no `weft.sqlite3`; resolving its path fails. Treat that as a
    // clean "not a Weft worker" (None), never a panel error (AC-1/AC-7).
    let Ok(path) = active_weft_store_path(&app).await else {
        return Ok(None);
    };
    let view = await_blocking("wv_roster_for_session", move || {
        let store =
            crate::weft::WeftStore::open(path).map_err(|e| format!("open weft store: {e}"))?;
        crate::weft::wv_roster_view_for_session(&store, &session_id).map_err(|e| e.to_string())
    })
    .await?;
    Ok(view.map(WvRosterRowView::from))
}

/// SW-WV-F3 (AC-5): the operator override — Close a still-`dispatch` worker that never
/// declared done, marking it `needs-attention` so it is never silently lost. Resolves
/// the opaque `handle` to its live session id, then reuses A4's audited single-writer
/// CAS (`dispatch → needs-attention`, `WHERE session_id=? AND status='dispatch'`) — a
/// worker that signalled between the click and the write is a no-op (never clobbered).
/// Emits `mcp:agent-done` (the same shape the A4 watchdog emits) so a mounted Board /
/// roster re-fetches. All SQLite I/O off-thread (AC-6).
#[tauri::command]
pub async fn wv_worker_force_close(handle: String, app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;

    let session_id = app
        .try_state::<crate::commands::dashboard_launch::DashboardLaunchState>()
        .and_then(|state| state.resolve(&handle))
        .ok_or_else(|| "unknown-handle".to_owned())?;

    // A non-Weft team has no store — nothing to force-close (graceful no-op).
    let Ok(path) = active_weft_store_path(&app).await else {
        return Ok(());
    };
    let cas_session = session_id.clone();
    let flipped = await_blocking("wv_worker_force_close", move || {
        let store =
            crate::weft::WeftStore::open(path).map_err(|e| format!("open weft store: {e}"))?;
        // A4's single-writer CAS `dispatch → needs-attention` (audit co-committed).
        // `Ok(None)` on a row already past `dispatch` — idempotent, never clobbers.
        crate::weft::wv_worker_needs_attention_by_session(&store, &cas_session)
            .map_err(|e| e.to_string())
    })
    .await?;

    // Nudge the Board / roster to re-render (best-effort, fire-and-forget) — the same
    // event + payload shape the A4 silence watchdog raises about a quiet worker.
    if flipped.is_some() {
        if let Err(e) = app.emit(
            "mcp:agent-done",
            serde_json::json!({
                "sessionId": session_id,
                "status": crate::weft::WV_ROSTER_NEEDS_ATTENTION_STATUS,
                "summary": "operator-force-close",
            }),
        ) {
            log::warn!(target: "weft", "wv_worker_force_close mcp:agent-done emit failed: {e}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::commands::await_blocking;
    use crate::weft::{self, wv_roster_view_for_session, WeftStore, WvRosterRow};

    /// Seed one roster row on `name` (PK) with a bound live `session_id` + status, so
    /// the F3 keyed reads have a row to resolve. `wv_upsert_roster` writes the whole
    /// row including `session_id` (the SW-WA-A5 correlation column A2's dispatch
    /// transition stamps), so the seeded row mirrors a live dispatched worker.
    fn seed_worker(store: &WeftStore, name: &str, session_id: &str, status: &str) {
        weft::wv_upsert_roster(
            store,
            &WvRosterRow {
                name: name.to_string(),
                workspace: None,
                pane: None,
                kind: Some("claude".to_string()),
                subclass: Some("impl".to_string()),
                lifecycle: "task".to_string(),
                owned: None,
                orch_owner: None,
                relation: "peer".to_string(),
                parent: None,
                current_task: Some("Add auth to login flow".to_string()),
                cwd: None,
                status: Some(status.to_string()),
                session_id: Some(session_id.to_string()),
                run_id: None,
            },
        )
        .unwrap();
    }

    #[tokio::test]
    async fn roster_view_round_trips_all_fields_and_unknown_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("weft.sqlite3");
        // RI-2-ALLOW: test-only schema seed on the test thread (never a command plane).
        let store = WeftStore::open(&path).unwrap();
        seed_worker(&store, "w1", "sess-1", "dispatch");

        let view = await_blocking("test.wv_roster_view_for_session", move || {
            // RI-2-ALLOW: inside the await_blocking closure — the off-reactor path itself.
            let store = WeftStore::open(&path).map_err(|e| e.to_string())?;
            wv_roster_view_for_session(&store, "sess-1").map_err(|e| e.to_string())
        })
        .await
        .unwrap()
        .expect("a seeded worker resolves a view");
        assert_eq!(view.subclass.as_deref(), Some("impl"));
        assert_eq!(view.kind.as_deref(), Some("claude"));
        assert_eq!(view.current_task.as_deref(), Some("Add auth to login flow"));
        assert_eq!(view.status.as_deref(), Some("dispatch"));

        // An unknown session id → Ok(None), never an error (non-Weft session).
        let dir2 = tempfile::tempdir().unwrap();
        let path2 = dir2.path().join("weft.sqlite3");
        WeftStore::open(&path2).unwrap();
        let miss = await_blocking("test.wv_roster_view_miss", move || {
            let store = WeftStore::open(&path2).map_err(|e| e.to_string())?;
            wv_roster_view_for_session(&store, "nobody").map_err(|e| e.to_string())
        })
        .await
        .unwrap();
        assert!(
            miss.is_none(),
            "an unknown session resolves None, not an error"
        );
    }

    #[tokio::test]
    async fn force_close_flips_dispatch_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("weft.sqlite3");
        // RI-2-ALLOW: test-only schema seed on the test thread (never a command plane).
        let store = WeftStore::open(&path).unwrap();
        seed_worker(&store, "w", "sess-x", "dispatch");

        // First force-close: `dispatch → needs-attention`, returns the roster name.
        let p1 = path.clone();
        let name = await_blocking("test.force_close", move || {
            let store = WeftStore::open(&p1).map_err(|e| e.to_string())?;
            weft::wv_worker_needs_attention_by_session(&store, "sess-x").map_err(|e| e.to_string())
        })
        .await
        .unwrap();
        assert_eq!(name.as_deref(), Some("w"));

        // The row now reads `needs-attention`.
        let p2 = path.clone();
        let status = await_blocking("test.status_after", move || {
            let store = WeftStore::open(&p2).map_err(|e| e.to_string())?;
            wv_roster_view_for_session(&store, "sess-x").map_err(|e| e.to_string())
        })
        .await
        .unwrap()
        .and_then(|view| view.status);
        assert_eq!(
            status.as_deref(),
            Some(weft::WV_ROSTER_NEEDS_ATTENTION_STATUS)
        );

        // Idempotent: a second force-close is a no-op (row past `dispatch`) → Ok(None).
        let p3 = path.clone();
        let again = await_blocking("test.force_close_again", move || {
            let store = WeftStore::open(&p3).map_err(|e| e.to_string())?;
            weft::wv_worker_needs_attention_by_session(&store, "sess-x").map_err(|e| e.to_string())
        })
        .await
        .unwrap();
        assert!(
            again.is_none(),
            "a second force-close never clobbers a settled row"
        );
    }
}
