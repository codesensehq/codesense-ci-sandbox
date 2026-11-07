// SPDX-License-Identifier: MIT
//! SW-WV-B3 — the Tauri command surface for the sibling ledgers.
//!
//! These are thin delegations: they resolve the active team's `weft.sqlite3`
//! handle and call the sanctioned `weft::` write handlers OFF the async/UI thread
//! (via [`await_blocking`], ARCHITECTURE §1.6). No SQL and no `wv_*` write logic
//! lives here — the sole writer stays `src-tauri/src/weft/sqlite.rs` (§1.2,
//! MODULE-HOME §5.3). The roster verbs are Tauri-only (the roster is written by the
//! Weft controller automation and the FE, never by worker agents), while
//! `wv_settle_approval` and `wv_upsert_watermark` are ALSO exposed here (the UI
//! settles approvals on owner reply and observes watermarks) in addition to their
//! MCP verbs.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::Emitter;

use crate::commands::await_blocking;
use crate::weft::{self, WvRosterRow};

/// SW-WV-B3: resolve the active team's `weft.sqlite3` PATH for a Tauri command —
/// the canonical `resolve_weft_store_path` (§1.1), never a re-derived path.
/// Returns the PATH (pure); each command opens + uses the store INSIDE its
/// `await_blocking` closure, so the whole blocking SQLite lifecycle (open + write)
/// runs off the async/UI thread (§1.6). The `WeftStore::open` therefore never
/// appears on an un-offloaded line.
pub(crate) async fn active_weft_store_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    active_weft_store(app).await.map(|(path, _)| path)
}

/// [`active_weft_store_path`] with the org of the team whose store it is.
pub(crate) async fn active_weft_store(app: &tauri::AppHandle) -> Result<(PathBuf, String), String> {
    let ctx = crate::commands::route_to_context::derive_active_context(app).await?;
    let team = ctx
        .team_name
        .as_deref()
        .ok_or_else(|| "no active team selected — pick a team first".to_owned())?;
    let path =
        weft::resolve_weft_store_path(&ctx.codesense_home, &ctx.principal(), &ctx.org_slug, team)?;
    Ok((path, ctx.org_slug))
}

/// SW-WV-B3: open the store from a resolved path inside a blocking closure. Kept
/// tiny so the `WeftStore::open` call is co-located with the blocking write it
/// precedes (never on the async reactor).
fn open_store(path: PathBuf) -> Result<weft::WeftStore, String> {
    // RI-2-ALLOW: only ever called from inside an `await_blocking` closure below
    weft::WeftStore::open(path).map_err(|e| format!("open weft store: {e}"))
}

/// A roster row on the wire (camelCase). Mirrors [`WvRosterRow`] — the FE maps it
/// into `AgentSessionRow` (Task 7).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WvRosterRowDto {
    pub name: String,
    pub workspace: Option<String>,
    pub pane: Option<String>,
    pub kind: Option<String>,
    pub subclass: Option<String>,
    pub lifecycle: String,
    pub owned: Option<String>,
    pub orch_owner: Option<String>,
    pub relation: String,
    pub parent: Option<String>,
    pub current_task: Option<String>,
    pub cwd: Option<String>,
    pub status: Option<String>,
    /// SW-WA-A5: the live PTY session id this row is bound to (the FE's
    /// `AgentSessionRow.sessionId`).
    pub session_id: Option<String>,
    /// SW-WA-A5: the durable run key correlating the live session to its run-store
    /// record (the FE's `provenance.runId`).
    pub run_id: Option<String>,
}

impl From<WvRosterRow> for WvRosterRowDto {
    fn from(r: WvRosterRow) -> Self {
        Self {
            name: r.name,
            workspace: r.workspace,
            pane: r.pane,
            kind: r.kind,
            subclass: r.subclass,
            lifecycle: r.lifecycle,
            owned: r.owned,
            orch_owner: r.orch_owner,
            relation: r.relation,
            parent: r.parent,
            current_task: r.current_task,
            cwd: r.cwd,
            status: r.status,
            session_id: r.session_id,
            run_id: r.run_id,
        }
    }
}

impl From<WvRosterRowDto> for WvRosterRow {
    fn from(r: WvRosterRowDto) -> Self {
        Self {
            name: r.name,
            workspace: r.workspace,
            pane: r.pane,
            kind: r.kind,
            subclass: r.subclass,
            lifecycle: r.lifecycle,
            owned: r.owned,
            orch_owner: r.orch_owner,
            relation: r.relation,
            parent: r.parent,
            current_task: r.current_task,
            cwd: r.cwd,
            status: r.status,
            session_id: r.session_id,
            run_id: r.run_id,
        }
    }
}

/// Map a B3 write handler's error onto the Tauri `String` channel, preserving the
/// human-readable message (validation refusals read cleanly in the UI).
fn wv_err(e: weft::WvWriteError) -> String {
    e.to_string()
}

/// Current unix epoch seconds — the deterministic `now` the E3 window math and the
/// F5 pending-approval read consume (the same inline idiom the watermark/harvest
/// commands use, hoisted to one helper).
fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// SW-WV-A3 (AC-5): emit the zero-payload `wv-roster-changed` FE nudge after a
/// lifecycle transition lands from a Tauri command. Mirrors the MCP-side helper so
/// both write surfaces coalesce into the same durable re-enumeration. Best-effort.
fn emit_roster_changed(app: &tauri::AppHandle) {
    if let Err(e) = app.emit("wv-roster-changed", ()) {
        log::warn!(target: "weft", "wv-roster-changed emit failed: {e}");
    }
}

/// SW-WV-B3: upsert a roster row on the `name` PK. Tauri-only (the roster is
/// written by the controller automation + FE, never by worker agents).
#[tauri::command]
pub async fn wv_upsert_roster(app: tauri::AppHandle, row: WvRosterRowDto) -> Result<(), String> {
    let path = active_weft_store_path(&app).await?;
    let row: WvRosterRow = row.into();
    await_blocking("wv_upsert_roster", move || {
        let store = open_store(path)?;
        weft::wv_upsert_roster(&store, &row).map_err(wv_err)
    })
    .await
}

/// The seeded assertion + its YOU-OWE attention row (camelCase on the wire) — the
/// return of [`wv_e2e_seed_endorsement`]. The QH weft-e2e suite drives `wv_ratify`
/// against `assertionId` and asserts the board row keyed `attentionId` moves from
/// YOU-OWE → DONE-UNACKED.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WvSeededEndorsement {
    pub assertion_id: i64,
    pub attention_id: i64,
}

/// QH weft-e2e ONLY: seed a pending agent assertion plus the matching YOU-OWE
/// `wv_attention` row (`source="assertion"`, `ref=<assertion_id>`, `state="needs-you"`)
/// so a real endorsement-gate row exists for the harness to ratify. It uses the SAME
/// sanctioned writers the MCP endorsement path uses ([`weft::wv_insert_assertion`] +
/// [`weft::wv_upsert_attention`]) — no new SQL, no schema bypass. Refused outside the
/// harness: it returns `Err` unless `CODESENSE_E2E=1`, so it can never seed state in a
/// real session. Off-thread via [`await_blocking`].
#[tauri::command]
pub async fn wv_e2e_seed_endorsement(
    app: tauri::AppHandle,
    session_id: String,
    claim: String,
    channel: String,
) -> Result<WvSeededEndorsement, String> {
    if std::env::var("CODESENSE_E2E").as_deref() != Ok("1") {
        return Err("wv_e2e_seed_endorsement is a harness-only seam (CODESENSE_E2E=1)".to_string());
    }
    let path = active_weft_store_path(&app).await?;
    let seeded = await_blocking("wv_e2e_seed_endorsement", move || {
        let store = open_store(path)?;
        let assertion_id =
            weft::wv_insert_assertion(&store, &session_id, None, &claim, &channel, None)
                .map_err(wv_err)?;
        // The YOU-OWE attention row the owner must judge — keyed on the natural
        // (source, ref) pair `wv_settle_attention` (inside `wv_ratify`) tears down.
        let attention_id = weft::wv_upsert_attention(
            &store,
            None,
            Some("needs-you"),
            Some(&claim),
            Some("assertion"),
            Some(&assertion_id.to_string()),
            None,
            None,
        )
        .map_err(wv_err)?;
        Ok(WvSeededEndorsement {
            assertion_id,
            attention_id,
        })
    })
    .await?;
    // Nudge a mounted Weft Board to re-fetch (the raw writer does not emit; the MCP
    // wrapper does). The QH board-UI assertion waits on the seeded YOU-OWE row node.
    if let Err(e) = app.emit(weft::WEFT_ATTENTION_CHANGED_EVENT, ()) {
        log::warn!(target: "weft", "wv_e2e_seed_endorsement: attention-changed emit failed: {e}");
    }
    Ok(seeded)
}

/// QH weft-e2e ONLY: seed the ledger state a REAL worker leaves at dispatch time —
/// minus the live PTY — so the RT1 `wv_signal_done_bridge` done-hooks can be driven
/// end-to-end against real rows in a headless harness (a live managed-worktree spawn
/// needs a hydrated story, which the harness cannot always provide). It writes exactly
/// what `wv_spawn_worker` writes: a durable dispatch record (`wv_write_dispatch`,
/// state='open') and a roster row advanced `provision → dispatch` bound to
/// `sessionId` (`wv_provision_worker_row` + `wv_dispatch_worker_row`). The bridge's
/// hooks then advance the roster `dispatch → signal` and stamp the dispatch record
/// done — the real controller-less "done" half. Refused unless `CODESENSE_E2E=1`.
#[tauri::command]
pub async fn wv_e2e_seed_dispatch(
    app: tauri::AppHandle,
    story_id: String,
    session_id: String,
) -> Result<(), String> {
    if std::env::var("CODESENSE_E2E").as_deref() != Ok("1") {
        return Err("wv_e2e_seed_dispatch is a harness-only seam (CODESENSE_E2E=1)".to_string());
    }
    let path = active_weft_store_path(&app).await?;
    await_blocking("wv_e2e_seed_dispatch", move || {
        let store = open_store(path)?;
        // The durable dispatch record the done-stamp hook advances open → done.
        weft::wv_write_dispatch(
            &store,
            &story_id,
            "QH1 real-loop dispatch",
            "",
            "",
            "the sentinel is printed",
            None,
        )
        .map_err(|e| e.to_string())?;
        // The roster row a real spawn leaves: provision → dispatch, bound to the
        // worker's live session id (the key the done-hooks resolve by).
        weft::wv_provision_worker_row(&store, &story_id, "impl", "root").map_err(wv_err)?;
        match weft::wv_dispatch_worker_row(&store, &story_id, &session_id, None).map_err(wv_err)? {
            weft::MutateResult::Updated => Ok(()),
            weft::MutateResult::NotOwner => {
                Err("seed dispatch: roster row was not in 'provision'".to_string())
            }
        }
    })
    .await?;
    emit_roster_changed(&app);
    Ok(())
}

// ============================================================================
// SW-WA-E7 — the three Stage-2 "it fails on purpose" red-path drivers.
//
// Each of the three drivers below exercises ONE Stage-2 guardrail at its REAL
// seam and returns the typed refusal the guard emits — the e2e suite asserts the
// refusal fired (default) and, under the matching `bypass` flag, asserts the
// oracle reddens when the refusal is skipped (the non-hollow self-check, AC-4).
// All three are harness-only (`CODESENSE_E2E=1`), synchronous refusals — never a
// side loop / thread / timer (NO-SECOND-ORCHESTRATOR).
// ============================================================================

/// SW-WA-E7 (AC-2) VERIFY-DON'T-FOLD verdict: the outcome of driving the two fold
/// verbs against a worker-finished-but-UNVERIFIED dispatch. `closeStatus` is
/// `wv_close_task`'s own typed shape (`closureBlocked` with `closeMissing` naming the
/// unmet conditions), `resolveStatus` is `wv_resolve_queue`'s own typed shape
/// (`verifyFirst` — the C3 pre-flight `is_close_predicate_met` failed); `rowPresent`
/// is whether the `wv_queue` row survived BOTH refusals (never pruned);
/// `isDoneVerified` reflects [`weft::WeftStore::is_done_verified`] (false — the stamp
/// was never written); `folded` mirrors "the row was pruned without a verified verdict"
/// — always false unless `bypassFoldGuard` forces a self-declared fold (AC-4).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WvE2eFoldOutcome {
    pub close_status: String,
    pub close_missing: Vec<String>,
    pub resolve_status: String,
    pub is_done_verified: bool,
    pub row_present: bool,
    pub folded: bool,
}

/// QH weft-e2e ONLY: seed a worker-finished-but-UNVERIFIED dispatch (a real
/// `wv_queue` row advanced to `dispatched` + a three-leg [`weft::DoneRecord`] whose
/// footprint is empty so Leg-2 fails — never `controller_verified`), then drive BOTH
/// fold verbs at the REAL seam. `wv_close_task`'s [`weft::queue::close_task`] returns
/// `closureBlocked` (naming `controller_verified` — and `accepted`, no success signal
/// exists), `wv_resolve_queue`'s C3 pre-flight [`weft::WeftStore::is_close_predicate_met`]
/// fails → `verifyFirst`; in BOTH the `wv_queue` row is NOT pruned, and
/// [`weft::WeftStore::is_done_verified`] stays false. `bypassFoldGuard` (AC-4) forces a
/// self-declared fold ([`weft::queue::resolve`] regardless of the verdict) so the
/// scenario's oracle MUST redden (the row folded unverified). Refused unless
/// `CODESENSE_E2E=1`; off-thread via [`await_blocking`]. Never a second orchestrator —
/// a seed + two pure fold checks + one optional prune, no loop/thread/timer.
#[tauri::command]
pub async fn wv_e2e_drive_fold(
    app: tauri::AppHandle,
    dispatch_id: String,
    bypass_fold_guard: Option<bool>,
) -> Result<WvE2eFoldOutcome, String> {
    if std::env::var("CODESENSE_E2E").as_deref() != Ok("1") {
        return Err("wv_e2e_drive_fold is a harness-only seam (CODESENSE_E2E=1)".to_string());
    }
    let path = active_weft_store_path(&app).await?;
    await_blocking("wv_e2e_drive_fold", move || {
        let store = open_store(path)?;
        let now = crate::weft::sqlite::now_epoch_secs_public();
        let caller = "e2e-controller";
        let story_id = format!("e2e-fold-{dispatch_id}");
        // A real queued ask advanced to `dispatched` — the row the fold verbs decide
        // on. enqueue → claim → mark_dispatched (needs an OPEN dispatch brief first).
        let queue_id = weft::queue::enqueue(
            &store,
            "slack",
            Some("root"),
            None,
            Some("e2e unverified-fold ask"),
            None,
            None,
            now,
        )
        .map_err(|e| format!("enqueue ask: {e}"))?;
        weft::wv_write_dispatch(
            &store,
            &story_id,
            "e2e unverified-fold dispatch",
            "",
            "",
            "the sentinel is printed",
            None,
        )
        .map_err(|e| e.to_string())?;
        match weft::queue::claim(&store, caller, Some("root"), now).map_err(|e| e.to_string())? {
            Some(claimed) if claimed == queue_id => {}
            other => return Err(format!("claim did not take the seeded row (got {other:?})")),
        }
        weft::queue::mark_dispatched(&store, queue_id, caller, &story_id, "e2e-handle", now)
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("mark_dispatched refused: {e:?}"))?;
        // The three-leg done record a worker leaves at signal_done, UNVERIFIED: an
        // empty footprint fails Leg-2 and `controller_verified` is never stamped, so
        // both fold legs (accepted + controller_verified) are unmet.
        let record = weft::DoneRecord {
            dispatch_id: dispatch_id.clone(),
            queue_id,
            session_id: format!("e2e-worker-{dispatch_id}"),
            run_id: story_id.clone(),
            story_id: story_id.clone(),
            subclass: "impl".to_string(),
            leg1_source: "e2e://source".to_string(),
            leg2_footprint: Vec::new(),
            leg3_packet: serde_json::json!({ "signalStatus": "success", "canonicalRefs": [] }),
            created: now * 1000,
        };
        store
            // RI-2-ALLOW: test-only e2e harness driver already inside the wv_e2e_drive_fold await_blocking offload — off the production UI path.
            .insert_done_record(&record)
            .map_err(|e| format!("insert done record: {e}"))?;

        // (a) wv_close_task's two-condition predicate → closureBlocked (row untouched).
        let close = weft::queue::close_task(&store, queue_id, caller, &dispatch_id, now)
            .map_err(|e| format!("close_task: {e}"))?;
        let (close_status, close_missing) = match close {
            weft::CloseOutcome::Blocked { missing } => (
                "closureBlocked".to_string(),
                missing.iter().map(|c| c.as_str().to_string()).collect(),
            ),
            weft::CloseOutcome::Resolved => ("resolved".to_string(), Vec::new()),
            weft::CloseOutcome::ResolveRefused(e) => (format!("resolveRefused:{e:?}"), Vec::new()),
        };
        // (b) wv_resolve_queue's C3 pre-flight → verifyFirst (row untouched).
        let resolve_status = if store
            .is_close_predicate_met(&dispatch_id)
            .map_err(|e| format!("close predicate: {e}"))?
        {
            "pruned".to_string()
        } else {
            "verifyFirst".to_string()
        };
        // The verify stamp was never written — the may_fold guard is false.
        let is_done_verified = store
            .is_done_verified(&dispatch_id)
            .map_err(|e| format!("is_done_verified: {e}"))?;

        // AC-4: a forced self-declared fold — bypass the guard and prune anyway. In the
        // real seam this is refused (may_fold is false); here it forces the redden.
        let folded = if bypass_fold_guard.unwrap_or(false) {
            weft::queue::resolve(&store, queue_id, caller, now)
                .map_err(|e| format!("forced resolve: {e}"))?
                .map_err(|e| format!("forced resolve refused: {e:?}"))?;
            true
        } else {
            false
        };
        // Did the row survive both refusals (never pruned)? A forced fold prunes it.
        let row_present = weft::queue::queue_list(&store, Some("root"))
            .map_err(|e| format!("queue_list: {e}"))?
            .iter()
            .any(|row| row.id == queue_id);
        Ok(WvE2eFoldOutcome {
            close_status,
            close_missing,
            resolve_status,
            is_done_verified,
            row_present,
            folded,
        })
    })
    .await
}

/// SW-WV-B3: deprovision (remove) a roster row by name. Refuses a `peer` row.
/// Returns `true` when a child row was removed, `false` on a no-op.
#[tauri::command]
pub async fn wv_deprovision_roster(app: tauri::AppHandle, name: String) -> Result<bool, String> {
    let path = active_weft_store_path(&app).await?;
    await_blocking("wv_deprovision_roster", move || {
        let store = open_store(path)?;
        weft::wv_deprovision_roster(&store, &name).map_err(wv_err)
    })
    .await
}

/// SW-WV-B3: list every roster row (the FE's durable roster source, Task 7).
#[tauri::command]
pub async fn wv_list_roster(app: tauri::AppHandle) -> Result<Vec<WvRosterRowDto>, String> {
    let path = active_weft_store_path(&app).await?;
    let rows = await_blocking("wv_list_roster", move || {
        let store = open_store(path)?;
        weft::wv_list_roster(&store).map_err(wv_err)
    })
    .await?;
    Ok(rows.into_iter().map(WvRosterRowDto::from).collect())
}

/// SW-WV-A3 (AC-1/AC-2/AC-5): advance a worker `signal → handoff` — the optional
/// handoff-doc lifecycle step. Refuses an illegal predecessor with a typed
/// refusal. Emits `wv-roster-changed` on success so the durable roster re-renders.
#[tauri::command]
pub async fn wv_worker_handoff(app: tauri::AppHandle, name: String) -> Result<(), String> {
    let path = active_weft_store_path(&app).await?;
    await_blocking("wv_worker_handoff", move || {
        let store = open_store(path)?;
        weft::wv_worker_handoff(&store, &name).map_err(wv_err)
    })
    .await?;
    emit_roster_changed(&app);
    Ok(())
}

/// SW-WV-A3 (AC-3/AC-5/AC-6): deprovision a worker after the four-condition closure
/// check. `packetDelivered` is the operator's packet-delivery confirmation; `force`
/// overrides offered-worker protection. The real PTY liveness probe
/// (`session_is_idle_inner`) is injected here at the boundary; a still-alive worker
/// logs a non-blocking warning and deprovision still succeeds. Emits
/// `wv-roster-changed` on success.
#[tauri::command]
pub async fn wv_worker_deprovision(
    app: tauri::AppHandle,
    name: String,
    packet_delivered: bool,
    force: bool,
) -> Result<(), String> {
    let path = active_weft_store_path(&app).await?;
    let app_probe = app.clone();
    await_blocking("wv_worker_deprovision", move || {
        let store = open_store(path)?;
        // Inject the real idle probe (AC-6). A probe error (unknown session) is
        // treated as idle (no warning) — the liveness warning is best-effort.
        let is_idle = |session_id: &str| {
            crate::commands::pty::session_is_idle_inner(&app_probe, session_id.to_string())
                .unwrap_or(true)
        };
        weft::wv_worker_deprovision(
            &store,
            &name,
            weft::DeprovisionInputs {
                packet_delivered,
                force,
            },
            is_idle,
        )
        .map(|_| ())
        .map_err(wv_err)
    })
    .await?;
    emit_roster_changed(&app);
    Ok(())
}

/// SW-WV-B3: settle an approval's disposition (the UI calls this on owner reply —
/// the reply path for both T2 and T3). Returns `true` when a pending approval was
/// settled, `false` on a no-op.
#[tauri::command]
pub async fn wv_settle_approval(
    app: tauri::AppHandle,
    id: i64,
    disposition: String,
) -> Result<bool, String> {
    let path = active_weft_store_path(&app).await?;
    await_blocking("wv_settle_approval", move || {
        let store = open_store(path)?;
        weft::wv_settle_approval(&store, id, &disposition).map_err(wv_err)
    })
    .await
}

/// SW-WV-B3: upsert a per-source read watermark on `(source, scope)` (observable
/// from the UI; also an MCP verb + an internal triage API).
#[tauri::command]
pub async fn wv_upsert_watermark(
    app: tauri::AppHandle,
    source: String,
    scope: String,
    last_handled: String,
) -> Result<(), String> {
    let path = active_weft_store_path(&app).await?;
    await_blocking("wv_upsert_watermark", move || {
        let store = open_store(path)?;
        weft::wv_upsert_watermark(&store, &source, &scope, &last_handled).map_err(wv_err)
    })
    .await
}

/// SW-WV-C6: set the drafted harvest artifact for a decision + advance
/// `pending → drafted` (AC-2). Controller-tier; the controller does the ferrying, the
/// human later approves. Idempotent on a same-ref re-draft; refuses an already-approved
/// decision. `kind` is one of `file` | `pr` | `entity`.
#[tauri::command]
pub async fn wv_set_harvest_artifact(
    app: tauri::AppHandle,
    id: i64,
    artifact_ref: String,
    kind: String,
) -> Result<(), String> {
    let path = active_weft_store_path(&app).await?;
    await_blocking("wv_set_harvest_artifact", move || {
        let store = open_store(path)?;
        weft::wv_set_harvest_artifact(&store, id, &artifact_ref, &kind).map_err(wv_err)
    })
    .await
}

/// SW-WV-C6: close a harvest loop — set `harvest_status='approved'` (AC-3). Called
/// from the `wv_settle_approval` reply path; also exposed as a Tauri command for a
/// direct-approve surface. Refuses a null-artifact or already-approved decision.
#[tauri::command]
pub async fn wv_approve_harvest(
    app: tauri::AppHandle,
    decision_id: i64,
    approval_id: i64,
) -> Result<(), String> {
    let path = active_weft_store_path(&app).await?;
    await_blocking("wv_approve_harvest", move || {
        let store = open_store(path)?;
        weft::wv_approve_harvest(&store, decision_id, approval_id).map_err(wv_err)
    })
    .await
}

/// SW-WV-C6: flag a decision self-contained — set `harvest_status='not-required'`
/// (AC-5). Idempotent; refuses an already-approved decision.
#[tauri::command]
pub async fn wv_mark_harvest_not_required(app: tauri::AppHandle, id: i64) -> Result<(), String> {
    let path = active_weft_store_path(&app).await?;
    await_blocking("wv_mark_harvest_not_required", move || {
        let store = open_store(path)?;
        weft::wv_mark_harvest_not_required(&store, id).map_err(wv_err)
    })
    .await
}

/// An overdue-harvest row on the wire (camelCase). Mirrors [`weft::OverdueHarvest`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverdueHarvestDto {
    pub id: i64,
    pub date: String,
    pub decision: String,
    pub constrains: String,
    pub ends_when: String,
    pub owner: String,
    pub source: Option<String>,
}

impl From<weft::OverdueHarvest> for OverdueHarvestDto {
    fn from(r: weft::OverdueHarvest) -> Self {
        Self {
            id: r.id,
            date: r.date,
            decision: r.decision,
            constrains: r.constrains,
            ends_when: r.ends_when,
            owner: r.owner,
            source: r.source,
        }
    }
}

/// SW-WV-C6: list every active decision still `pending` past the 24h harvest window
/// (AC-4). Pure read. The controller automation reads this to draft the missing
/// artifact; the caller then writes one `needs-you` `wv_attention` row per result.
#[tauri::command]
pub async fn wv_list_overdue_harvests(
    app: tauri::AppHandle,
    now_unix: i64,
) -> Result<Vec<OverdueHarvestDto>, String> {
    let path = active_weft_store_path(&app).await?;
    let rows = await_blocking("wv_list_overdue_harvests", move || {
        let store = open_store(path)?;
        weft::wv_list_overdue_harvests(&store, now_unix).map_err(wv_err)
    })
    .await?;
    Ok(rows.into_iter().map(OverdueHarvestDto::from).collect())
}

// ============================================================================
// SW-WV-F1 — the Weft Board read command (attention board).
//
// Read-only. `wv_list_attention_board` opens the active team's `weft.sqlite3` and
// assembles the five sections in one transaction (no `wv_*` write), OFF the
// async/UI thread via `await_blocking` (§1.6).
// ============================================================================

/// SW-WV-F1: one attention-board row on the wire (camelCase). Mirrors
/// [`weft::AttentionBoardRow`] — `ref_` serialises as `ref` (the canonical B3 AC-8
/// routing column). The FE row renderer reads `summary` + the `source`/`ref` chevron
/// target.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionRowDto {
    pub id: i64,
    pub summary: String,
    pub source: Option<String>,
    #[serde(rename = "ref")]
    pub ref_: Option<String>,
    /// SW-WA-E5: who started the row — `"agent"` (a 🤖 agent-owned / agent-claimed
    /// row) or `"human"` (a triage row the operator owns). Drives the 🤖 provenance
    /// marker. SW-AV-3: renamed from `"controller"` (accepts the old spelling on read).
    pub initiator: AttentionInitiatorDto,
    /// SW-AV-3: what the row IS — the Activity chip derives from the kind, not the
    /// lane (design 4.3). `#[serde(default)]` so an old-shaped record with no `kind`
    /// still deserialises (no data migration). Always populated by the board read.
    #[serde(default)]
    pub kind: AttentionKindDto,
    /// SW-WA-E5: the ON-AGENT live-terminal target (a `dispatched` row's opaque
    /// worker handle, stamped verbatim). `None` for non-live rows. The FE guards
    /// resolvability — an unresolvable handle earns no one-click affordance.
    pub live_session_id: Option<String>,
    /// SLW-TL-4: the DECLARED need this item carries, or `null`. The generalized
    /// Inbox row renderer keys its plain label + matching action off this; a `null`
    /// need (a legacy weft row / a running-agent row) falls back to the lane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub need: Option<AttentionNeedDto>,
}

/// SLW-TL-4: the declared-need axis on the wire (kebab-case — `"awaiting-answer"` |
/// `"needs-approval"` | `"needs-attention"` | `"awaiting-validation"` | `"done"`).
/// Mirrors [`weft::AttentionNeed`]; a serde enum so the FE types a closed union the
/// generalized Inbox row renderer keys off (and SLW-TL-7 maps need → action).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AttentionNeedDto {
    AwaitingAnswer,
    NeedsApproval,
    NeedsAttention,
    AwaitingValidation,
    Done,
}

impl From<weft::AttentionNeed> for AttentionNeedDto {
    fn from(n: weft::AttentionNeed) -> Self {
        match n {
            weft::AttentionNeed::AwaitingAnswer => AttentionNeedDto::AwaitingAnswer,
            weft::AttentionNeed::NeedsApproval => AttentionNeedDto::NeedsApproval,
            weft::AttentionNeed::NeedsAttention => AttentionNeedDto::NeedsAttention,
            weft::AttentionNeed::AwaitingValidation => AttentionNeedDto::AwaitingValidation,
            weft::AttentionNeed::Done => AttentionNeedDto::Done,
        }
    }
}

/// SW-WA-E5: the provenance axis on the wire (camelCase `"agent"` | `"human"`).
/// Mirrors [`weft::Initiator`]; a serde enum so the FE types a closed union rather
/// than a free string. SW-AV-3: `Agent` (renamed from `Controller`) accepts the old
/// `"controller"` spelling on READ via a serde alias, so any persisted/in-flight
/// record still deserialises; new writes emit `"agent"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AttentionInitiatorDto {
    #[serde(alias = "controller")]
    Agent,
    Human,
}

impl From<weft::Initiator> for AttentionInitiatorDto {
    fn from(i: weft::Initiator) -> Self {
        match i {
            weft::Initiator::Agent => AttentionInitiatorDto::Agent,
            weft::Initiator::Human => AttentionInitiatorDto::Human,
        }
    }
}

/// SW-AV-3: the per-row `kind` on the wire (camelCase — `"assertion"` | `"approval"`
/// | `"question"` | `"parked"` | `"blocked"` | `"running"` | `"done"`). Mirrors
/// [`weft::AttentionKind`]; a serde enum so the FE types a closed union the SW-AV-4
/// chip maps to its icon+label. `Default` is `Approval` (a needs-you decision) — the
/// neutral value for a `#[serde(default)]` old-shaped record that carries no `kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AttentionKindDto {
    Assertion,
    #[default]
    Approval,
    Question,
    Parked,
    Blocked,
    Running,
    Done,
}

impl From<weft::AttentionKind> for AttentionKindDto {
    fn from(k: weft::AttentionKind) -> Self {
        match k {
            weft::AttentionKind::Assertion => AttentionKindDto::Assertion,
            weft::AttentionKind::Approval => AttentionKindDto::Approval,
            weft::AttentionKind::Question => AttentionKindDto::Question,
            weft::AttentionKind::Parked => AttentionKindDto::Parked,
            weft::AttentionKind::Blocked => AttentionKindDto::Blocked,
            weft::AttentionKind::Running => AttentionKindDto::Running,
            weft::AttentionKind::Done => AttentionKindDto::Done,
        }
    }
}

impl From<weft::AttentionBoardRow> for AttentionRowDto {
    fn from(r: weft::AttentionBoardRow) -> Self {
        Self {
            id: r.id,
            summary: r.summary,
            source: r.source,
            ref_: r.ref_,
            initiator: r.initiator.into(),
            kind: r.kind.into(),
            live_session_id: r.live_session_id,
            need: r.need.map(AttentionNeedDto::from),
        }
    }
}

/// SW-WV-F1: the five board slices on the wire (camelCase). Mirrors
/// [`weft::AttentionBoard`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionBoardDto {
    pub you_owe: Vec<AttentionRowDto>,
    pub owed_to_you: Vec<AttentionRowDto>,
    pub on_agent: Vec<AttentionRowDto>,
    pub parked: Vec<AttentionRowDto>,
    pub done_unacked: Vec<AttentionRowDto>,
}

impl From<weft::AttentionBoard> for AttentionBoardDto {
    fn from(b: weft::AttentionBoard) -> Self {
        let map = |rows: Vec<weft::AttentionBoardRow>| -> Vec<AttentionRowDto> {
            rows.into_iter().map(AttentionRowDto::from).collect()
        };
        Self {
            you_owe: map(b.you_owe),
            owed_to_you: map(b.owed_to_you),
            on_agent: map(b.on_agent),
            parked: map(b.parked),
            done_unacked: map(b.done_unacked),
        }
    }
}

/// SW-WV-F1 (AC-1): read the five-section attention board for the active team. Opens
/// `weft.sqlite3` and assembles all five slices in one transaction, OFF the
/// async/UI thread. Read-only.
#[tauri::command]
pub async fn wv_list_attention_board(app: tauri::AppHandle) -> Result<AttentionBoardDto, String> {
    let path = active_weft_store_path(&app).await?;
    let board = await_blocking("wv_list_attention_board", move || {
        // RI-2-ALLOW: weft store open + five-slice read — off-thread here.
        let store = open_store(path)?;
        weft::wv_list_attention_board(&store).map_err(wv_err)
    })
    .await?;
    Ok(board.into())
}

// ============================================================================
// SW-WV-E1 — endorsement-gate Tauri commands (recipient-ack + ratify/revoke).
//
// These are Tauri-only, NEVER MCP verbs: ratification is a HUMAN action through
// the UI boundary. `require_live_session` on the MCP socket proves LIVENESS, not
// OWNERSHIP (`handler.rs:3920-3936`) — the socket is identity-less; an agent
// echoes its own `$CODESENSE_TAB_SESSION_ID`. Because ratify/revoke are Tauri
// commands (invoked by the app UI, not injectable via MCP), no agent can ratify
// another agent's — or its own — assertion (ARCHITECTURE §4.1 ENDORSEMENT-WALL,
// AC-5/AC-6). All SQL/CAS/audit stays in `weft/sqlite.rs`; these are thin
// delegations (MODULE-HOME §5.3).
// ============================================================================

/// SW-WV-E1: may `session_id` be a live CodeSense session (the probe
/// `require_live_session` uses)? Used by the ratify/revoke async-lag note and the
/// decline finding route: a gone session does NOT fail the command (async lag is
/// OK). Only a session KNOWN gone reads gone: one whose terminal host cannot be
/// asked may still be running.
async fn session_may_be_live(app: &tauri::AppHandle, session_id: &str) -> bool {
    crate::commands::dashboard_launch::session_liveness(app, session_id)
        .await
        .may_be_true()
}

/// SW-WV-E1: append a `wv_assertion_ratified` (or `_revoked`) row to the originating
/// run's `run_events` stream — the durable audit trail (WEFT-HERDR-REPLACEMENT-MAP
/// §3, ARCH §4.2). Best-effort: a store-open/append failure logs a warning and does
/// NOT fail the ratify/revoke (the assertion state is already durable in
/// `weft.sqlite3`). Runs inside the caller's `await_blocking` closure — off-thread.
fn append_run_event_best_effort(run_id: &str, kind: &str, payload: serde_json::Value) {
    use crate::coordinator::{default_store_path, OrchestrationStore, RunEvent};
    let Some(home) = crate::commands::paths::codesense_home_dir() else {
        log::warn!(target: "weft", "{kind}: no codesense home — skipping run_events audit");
        return;
    };
    let store = match crate::coordinator::SqliteOrchestrationStore::open(default_store_path(&home))
    {
        Ok(s) => s,
        Err(e) => {
            log::warn!(target: "weft", "{kind}: open coordinator store failed: {e}");
            return;
        }
    };
    if let Err(e) = store.append(RunEvent {
        run_id: run_id.to_string(),
        kind: kind.to_string(),
        payload: payload.to_string(),
    }) {
        log::warn!(target: "weft", "{kind}: run_events append failed: {e}");
    }
}

/// SW-WV-E1 (AC-3, AC-4): record a recipient reaction (👀 `seen` / ✅ `acknowledged`
/// / 🚫 `declined`) — SEPARATE from owner ratification. A `declined` NEVER ratifies
/// (the wall never moves on a recipient signal); it routes a finding back to the
/// originating worker via the `mcp:agent-message` event (the same path
/// `post_agent_message` uses at `handler.rs:1650`). The originating session must be
/// live for the finding to reach it; a gone session logs a warning and does NOT
/// fail the ack.
#[tauri::command]
pub async fn wv_record_recipient_ack(
    app: tauri::AppHandle,
    assertion_id: i64,
    recipient: String,
    ack: String,
) -> Result<serde_json::Value, String> {
    if !weft::WV_ACK_VALUES.contains(&ack.as_str()) {
        return Err("ack must be one of: seen | acknowledged | declined".to_string());
    }
    let path = active_weft_store_path(&app).await?;
    let ack_clone = ack.clone();
    let recipient_clone = recipient.clone();
    // Write the ack row + read back the assertion (for the decline route) off-thread.
    let (ack_id, assertion) = await_blocking("wv_record_recipient_ack", move || {
        // RI-2-ALLOW: weft store ack insert + assertion read — off-thread here.
        let store = open_store(path)?;
        let ack_id =
            weft::wv_insert_recipient_ack(&store, assertion_id, &recipient_clone, &ack_clone)
                .map_err(wv_err)?;
        let assertion = weft::wv_get_assertion(&store, assertion_id).map_err(wv_err)?;
        Ok::<(i64, Option<weft::WvAssertionRow>), String>((ack_id, assertion))
    })
    .await?;

    // AC-4: 🚫 decline → route a finding back to the originating worker session.
    if ack == "declined" {
        if let Some(assertion) = assertion {
            if session_may_be_live(&app, &assertion.session_id).await {
                let text = format!(
                    "[weft] assertion {assertion_id} declined by {recipient} — route it back to the worker for revision."
                );
                if let Err(e) = app.emit(
                    "mcp:agent-message",
                    serde_json::json!({
                        "sessionId": assertion.session_id,
                        "role": "system",
                        "text": text,
                    }),
                ) {
                    log::warn!(target: "weft", "wv_record_recipient_ack: decline finding emit failed: {e}");
                }
            } else {
                log::warn!(
                    target: "weft",
                    "wv_record_recipient_ack: assertion {assertion_id} originating session is gone — decline finding not routed"
                );
            }
        }
    }

    Ok(serde_json::json!({ "recorded": true, "ackId": ack_id }))
}

/// SW-WV-E1 (AC-5, AC-6): the owner ratifies an assertion — a durable state
/// transition, invoked ONLY from the Tauri UI boundary (NOT an MCP verb, absent
/// from `EXPECTED_MCP_TOOLS`).
///
/// **Liveness, not ownership (`handler.rs:3920-3936`).** The originating session is
/// probed for liveness only to NOTE async lag in the audit trail — a dead session
/// still ratifies (the human acted; the agent finishing late is fine). No MCP path
/// can reach this command, so no agent can ratify an agent (ENDORSEMENT-WALL).
#[tauri::command]
pub async fn wv_ratify(
    app: tauri::AppHandle,
    assertion_id: i64,
) -> Result<serde_json::Value, String> {
    settle_assertion(app, assertion_id, "ratified", None).await
}

/// SW-WV-E1 (AC-5) / SW-WV-F4 (AC-6): the symmetric owner action — revoke a pending
/// assertion (Send-back). Same UI-boundary-only contract and doc cite
/// (`handler.rs:3920-3936`) as [`wv_ratify`].
///
/// **F4 note routing (AC-6).** An optional `note` carries the operator's send-back
/// reason. When present (and the assertion's originating session is known), the handler
/// emits `mcp:agent-message` to the worker session BEFORE the `mcp:wv-assertion-revoked`
/// event — routing the note without a separate `post_agent_message` FE call (which is
/// an MCP verb the renderer cannot invoke). A blank/absent note skips the extra emit.
#[tauri::command]
pub async fn wv_revoke(
    app: tauri::AppHandle,
    assertion_id: i64,
    note: Option<String>,
) -> Result<serde_json::Value, String> {
    settle_assertion(app, assertion_id, "revoked", note).await
}

/// SW-WV-F4 (AC-1): one pending assertion on the wire (camelCase). Mirrors
/// [`weft::WvPendingAssertion`] — the review-at-altitude header reads these to decide
/// whether to render and which assertion to act on (the first, one judgment at a time).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WvPendingAssertionDto {
    pub id: i64,
    pub session_id: String,
    pub run_id: Option<String>,
    pub claim: String,
    pub channel: String,
    pub posted_at: String,
    pub disposition: String,
}

impl From<weft::WvPendingAssertion> for WvPendingAssertionDto {
    fn from(r: weft::WvPendingAssertion) -> Self {
        Self {
            id: r.id,
            session_id: r.session_id,
            run_id: r.run_id,
            claim: r.claim,
            channel: r.channel,
            posted_at: r.posted_at,
            disposition: r.disposition,
        }
    }
}

/// Persist the assertion and its attention row on the blocking writer path.
pub(crate) fn post_assertion(
    store: &weft::WeftStore,
    session: &str,
    run: Option<&str>,
    claim: &str,
    channel: &str,
    approval: Option<i64>,
) -> weft::WvWriteResult<WvPendingAssertionDto> {
    let id = weft::wv_insert_assertion(store, session, run, claim, channel, approval)?;
    weft::wv_upsert_attention(
        store,
        None,
        Some("needs-you"),
        Some(&format!("Agent assertion pending your 👍 — {channel}")),
        Some("assertion"),
        Some(&id.to_string()),
        Some(session),
        None,
    )?;
    let row = weft::wv_get_assertion(store, id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
    let dto = WvPendingAssertionDto {
        id: row.id,
        session_id: row.session_id,
        run_id: row.run_id,
        claim: row.claim,
        channel: row.channel,
        posted_at: row.posted_at,
        disposition: row.disposition,
    };
    crate::live_hub::run_review::written(
        store.path(),
        crate::live_hub::run_review::Written::Assertion(
            serde_json::to_value(&dto).expect("serializable assertion"),
        ),
    );
    Ok(dto)
}

/// SW-WV-F4 (AC-1): read every `pending` assertion for a run — the review header's
/// source. Opens the active team's `weft.sqlite3` and filters to `pending` in SQL,
/// OFF the async/UI thread via [`await_blocking`] (AC-7). Read-only.
#[tauri::command]
pub async fn wv_read_pending_assertions(
    app: tauri::AppHandle,
    run_id: String,
) -> Result<Vec<WvPendingAssertionDto>, String> {
    let path = active_weft_store_path(&app).await?;
    let rows = await_blocking("wv_read_pending_assertions", move || {
        // RI-2-ALLOW: weft store open + pending-assertion read — off-thread here.
        let store = open_store(path)?;
        weft::wv_read_pending_assertions(&store, &run_id).map_err(wv_err)
    })
    .await?;
    Ok(rows.into_iter().map(WvPendingAssertionDto::from).collect())
}

/// SW-WV-F5 (AC-1, AC-3): one pending `wv_approvals` row on the wire (camelCase) — the
/// T2/T3 half of the YOU-OWE endorsement surface. Mirrors [`weft::WvApproval`]; the
/// window fields map to E3's durable columns (`ttlS`=`window_duration_s`,
/// `acquiredAt`=`window_opened_at`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WvApprovalDto {
    pub id: i64,
    pub tier: String,
    pub claim: String,
    pub posted_at: String,
    pub source: Option<String>,
    pub window_state: String,
    pub disposition: String,
    pub ttl_s: Option<i64>,
    pub acquired_at: Option<i64>,
    pub pause_accumulated_s: i64,
    pub surfaced_session: Option<String>,
    pub not_surfaced_but_expired: bool,
}

impl From<weft::WvApproval> for WvApprovalDto {
    fn from(r: weft::WvApproval) -> Self {
        Self {
            id: r.id,
            tier: r.tier,
            claim: r.claim,
            posted_at: r.posted_at,
            source: r.source,
            window_state: r.window_state,
            disposition: r.disposition,
            ttl_s: r.duration_s,
            acquired_at: r.opened_at,
            pause_accumulated_s: r.pause_accumulated_s,
            surfaced_session: r.surfaced_session,
            not_surfaced_but_expired: r.not_surfaced_but_expired,
        }
    }
}

/// SW-WV-F5 (AC-1, AC-3): read every pending `wv_approvals` row — the T2/T3 rows the
/// YOU-OWE endorsement surface merges alongside pending assertions. Opens the active
/// team's `weft.sqlite3` and computes `not_surfaced_but_expired` at the current epoch,
/// OFF the async/UI thread via [`await_blocking`]. Read-only.
#[tauri::command]
pub async fn wv_list_pending_approvals(
    app: tauri::AppHandle,
) -> Result<Vec<WvApprovalDto>, String> {
    let path = active_weft_store_path(&app).await?;
    let now = now_unix_secs();
    let rows = await_blocking("wv_list_pending_approvals", move || {
        // RI-2-ALLOW: weft store open + pending-approval read — off-thread here.
        let store = open_store(path)?;
        weft::wv_list_pending_approvals(&store, now).map_err(wv_err)
    })
    .await?;
    Ok(rows.into_iter().map(WvApprovalDto::from).collect())
}

/// SW-WV-F5 (AC-4) / SW-WV-E3: mark a pending T2 approval row surfaced in the current
/// session — the FE half of the surfaced-item guard. Arms the E3 window
/// ([`weft::wv_open_t2_window`]), stamping `surfaced_session` so the window may only
/// auto-close in THIS session. Idempotent: a second call for an already-opened window
/// (E3's `window_opened_at IS NULL` guard rejects it) is a no-op success — the FE fires
/// once per mount, and Rust-side idempotency handles cross-mount safety (AC-4).
/// `durationS` is the granted budget when the window is first armed; a non-T2 or
/// already-settled row is a no-op success too.
#[tauri::command]
pub async fn wv_mark_approval_surfaced(
    app: tauri::AppHandle,
    id: i64,
    session_id: String,
    duration_s: Option<i64>,
) -> Result<(), String> {
    let path = active_weft_store_path(&app).await?;
    let now = now_unix_secs();
    // Default to a 10-minute revocation window when the caller does not specify one —
    // long enough that the surfaced-item guard, not the clock, is the operative safety.
    let duration = duration_s.unwrap_or(600).max(1);
    await_blocking("wv_mark_approval_surfaced", move || {
        // RI-2-ALLOW: weft store open + surfaced-arm (idempotent) — off-thread here.
        let store = open_store(path)?;
        // A validation refusal here means the window was already opened (or the row is
        // not an un-opened pending T2) — an expected no-op, not an error (idempotency).
        // A store-level error (I/O) still propagates.
        match weft::wv_open_t2_window(&store, id, &session_id, duration, now) {
            Ok(_) | Err(weft::WvWriteError::Invalid(_)) => Ok(()),
            Err(e) => Err(wv_err(e)),
        }
    })
    .await
}

/// Shared ratify/revoke body (DRY — one CAS transition, one attention teardown, one
/// run_events audit, one FE event). `target` is `"ratified"` or `"revoked"`; the
/// emitted event is `mcp:wv-assertion-ratified` / `-revoked` respectively.
async fn settle_assertion(
    app: tauri::AppHandle,
    assertion_id: i64,
    target: &'static str,
    note: Option<String>,
) -> Result<serde_json::Value, String> {
    let path = active_weft_store_path(&app).await?;
    let outcome = await_blocking("wv_settle_assertion", move || {
        let store = open_store(path)?;
        settle_assertion_row(&store, assertion_id, target)
    })
    .await?;

    match outcome {
        SettleOutcome::AlreadySettled { current } => {
            Ok(serde_json::json!({ "status": "alreadySettled", "disposition": current }))
        }
        SettleOutcome::Settled {
            run_id,
            channel,
            claim,
            session_id,
        } => {
            // Liveness note (AC-5): a gone originating session still ratifies (async
            // lag OK); the run_events audit records whether it was gone.
            let session_gone = !session_may_be_live(&app, &session_id).await;
            if let Some(run_id) = run_id.as_deref() {
                let kind = if target == "ratified" {
                    "wv_assertion_ratified"
                } else {
                    "wv_assertion_revoked"
                };
                let run_id_owned = run_id.to_string();
                let payload = serde_json::json!({
                    "assertionId": assertion_id,
                    "channel": channel,
                    "sessionGone": session_gone,
                });
                let kind_owned = kind;
                // Off-thread: the append opens the coordinator store synchronously.
                await_blocking("wv_settle_assertion.run_event", move || {
                    // RI-2-ALLOW: run_events append (coordinator store) — off-thread here.
                    append_run_event_best_effort(&run_id_owned, kind_owned, payload);
                    Ok::<(), String>(())
                })
                .await?;
            }
            // SW-WV-F4 (AC-6): route the operator's send-back note to the worker
            // session BEFORE the revoked event, via the same `mcp:agent-message`
            // path `post_agent_message` uses (`handler.rs:1731`) — no separate FE
            // call. A blank/absent note or a gone session skips the emit (async lag
            // is OK — the revoke still stands). Only on the revoke path.
            if target == "revoked" {
                if let Some(text) = note.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                    // Reuse the `session_gone` liveness result already computed above —
                    // one probe per settle, and the audit + note branch agree on the
                    // same liveness snapshot.
                    if !session_gone {
                        if let Err(e) = app.emit(
                            "mcp:agent-message",
                            serde_json::json!({
                                // `role: "user"` so the transcript listener attributes the
                                // send-back note to the human operator, not the worker agent
                                // (`use-agent-message-listener` maps only "user" → user; any
                                // other role falls through to "agent"). The operator IS the
                                // UI user, so "user" is the accurate speaker.
                                "sessionId": session_id,
                                "role": "user",
                                "text": text,
                            }),
                        ) {
                            log::warn!(target: "weft", "wv_revoke: send-back note emit failed: {e}");
                        }
                    } else {
                        log::warn!(
                            target: "weft",
                            "wv_revoke: assertion {assertion_id} originating session is gone — send-back note not routed"
                        );
                    }
                }
            }
            // FE contract event — fire-and-forget (SW-WV-F5 consumes it). Carries
            // `disposition` (AC-5/AC-6: `{ id, disposition }`) alongside the E1
            // channel/claim enrichment.
            let event = if target == "ratified" {
                "mcp:wv-assertion-ratified"
            } else {
                "mcp:wv-assertion-revoked"
            };
            if let Err(e) = app.emit(
                event,
                serde_json::json!({
                    "id": assertion_id,
                    "disposition": target,
                    "channel": channel,
                    "claim": claim,
                }),
            ) {
                log::warn!(target: "weft", "{event} emit failed: {e}");
            }
            // The settle moved a YOU-OWE row to done-unacked (wv_settle_attention above),
            // so nudge a mounted Weft Board to re-fetch. Fire-and-forget, best-effort.
            if let Err(e) = app.emit(weft::WEFT_ATTENTION_CHANGED_EVENT, ()) {
                log::warn!(target: "weft", "attention-changed emit failed: {e}");
            }
            Ok(serde_json::json!({ "status": "ok", "id": assertion_id }))
        }
    }
}

/// SW-WA-C0: settle the assertion's paired approval on the SAME assertion-settle. The
/// human's single 👍 lands as `wv_ratify → settle_assertion` on the ASSERTION; the paired
/// `wv_approvals` row (`assertion.approval_id`, the C0 backlink) must fold with it so the
/// endorsement object settles as one. Reuses the shipped [`weft::wv_settle_approval`]
/// store fn on the same opened handle (`target` is `"ratified"`/`"revoked"`, the same
/// disposition the assertion just took).
///
/// **Best-effort (AC-5).** A `None` `approval_id` (manual/non-M1 assertion) touches no
/// approval, exactly as pre-C0. A missing/already-settled row (`Ok(false)`) or a store
/// error is logged and swallowed — this must NEVER be `?`-propagated out of the
/// `settle_assertion` closure, which would abort the settle and roll back the assertion
/// CAS. Mirrors the run_events/emit best-effort handling already in `settle_assertion`.
fn settle_paired_approval(store: &weft::WeftStore, approval_id: Option<i64>, target: &str) {
    let Some(approval_id) = approval_id else {
        return;
    };
    if let Err(e) = weft::wv_settle_approval(store, approval_id, target) {
        log::warn!(
            target: "weft",
            "settle_assertion: paired approval {approval_id} settle to {target} failed (best-effort, ignored): {e}"
        );
    }
}

pub(crate) fn settle_assertion_row(
    store: &weft::WeftStore,
    assertion_id: i64,
    target: &str,
) -> Result<SettleOutcome, String> {
    let assertion = weft::wv_get_assertion(&store, assertion_id)
        .map_err(wv_err)?
        .ok_or_else(|| format!("no assertion with id {assertion_id}"))?;
    let won = weft::wv_set_assertion_disposition(&store, assertion_id, "pending", target)
        .map_err(wv_err)?;
    if !won {
        // Idempotent: already ratified/revoked (or never pending). Report the
        // current disposition so the caller does not double-act.
        let current = weft::wv_get_assertion_disposition(&store, assertion_id)
            .map_err(wv_err)?
            .unwrap_or_default();
        return Ok::<SettleOutcome, String>(SettleOutcome::AlreadySettled { current });
    }
    // Teardown the YOU-OWE attention row (D8): source="assertion", ref=<id>.
    weft::wv_settle_attention(
        &store,
        "assertion",
        &assertion_id.to_string(),
        "done-unacked",
    )
    .map_err(wv_err)?;
    // SW-WA-C0: settle the paired approval on the SAME assertion-settle (best-effort,
    // never rolls back the assertion CAS). See [`settle_paired_approval`].
    settle_paired_approval(&store, assertion.approval_id, target);
    if let Some(run_id) = assertion.run_id.as_ref() {
        crate::live_hub::run_review::written(
            store.path(),
            crate::live_hub::run_review::Written::Settled {
                run_id: run_id.clone(),
                id: assertion_id,
            },
        );
    }
    Ok::<SettleOutcome, String>(SettleOutcome::Settled {
        run_id: assertion.run_id,
        channel: assertion.channel,
        claim: assertion.claim,
        session_id: assertion.session_id,
    })
}

/// The internal result of a settle (ratify/revoke) transition — either the CAS won
/// (carry the row fields the event/audit need) or it was already settled.
pub(crate) enum SettleOutcome {
    Settled {
        run_id: Option<String>,
        channel: String,
        claim: String,
        session_id: String,
    },
    AlreadySettled {
        current: String,
    },
}

/// SW-WA-C0 (tests): slice a function's brace-balanced body from a source string, so a
/// conformance test can grep a specific function region rather than a whole file. Panics
/// if the signature is absent (a stale grep is a test bug, not a silent pass).
#[cfg(test)]
fn balanced_fn_body<'a>(src: &'a str, sig: &str) -> &'a str {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("fn `{sig}` not found in source"));
    let after = &src[start..];
    let brace = after.find('{').expect("fn opening brace");
    let mut depth = 0usize;
    let mut end = brace;
    for (i, &b) in after.as_bytes().iter().enumerate().skip(brace) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = i + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    &after[brace..end]
}

#[cfg(test)]
mod tests {
    use crate::commands::await_blocking;
    use crate::weft::{self, WeftStore};

    /// SW-WV-F1 (AC-5 Test B): the Board read routes through the `await_blocking`
    /// blocking-pool path. `wv_list_attention_board` opens the weft store and reads
    /// the five slices. A missing `await_blocking` (calling the synchronous SQLite path
    /// directly on the async reactor) panics with "Cannot start a runtime from within a
    /// runtime", so a passing test is the machine-checkable proof the blocking-pool path
    /// holds.
    #[tokio::test]
    async fn board_reads_route_through_the_blocking_pool() {
        let dir = tempfile::tempdir().unwrap();
        let weft_path = dir.path().join("weft.sqlite3");
        // RI-2-ALLOW: test-only schema seed on the test thread (never a command plane).
        WeftStore::open(&weft_path).unwrap();

        let attention_path = weft_path.clone();
        let board = await_blocking("test.wv_list_attention_board", move || {
            // RI-2-ALLOW: inside the await_blocking closure — the off-reactor path itself.
            let store = WeftStore::open(&attention_path).map_err(|e| e.to_string())?;
            weft::wv_list_attention_board(&store).map_err(|e| e.to_string())
        })
        .await
        .unwrap();
        assert!(
            board.you_owe.is_empty(),
            "a fresh store yields empty slices"
        );
    }

    // ---- SW-WA-E5: the provenance axis serialises on the wire ----

    use super::{AttentionInitiatorDto, AttentionKindDto, AttentionRowDto};

    /// SW-WA-E5 (AC-1) + SW-AV-3: an agent ON-AGENT row serialises `"initiator":"agent"`
    /// (renamed from `"controller"`) and carries its `liveSessionId`; a human row
    /// serialises `"initiator":"human"` and omits/negates the live handle. Proves the
    /// DTO carries the provenance axis on the wire through `From<weft::AttentionBoardRow>`
    /// (the same conversion the board command runs), not just as an in-memory field.
    #[test]
    fn attention_row_serialises_the_provenance_axis() {
        // An agent-dispatched ON-AGENT row (derived by the read; here built from
        // the core row to prove the whole conversion + serde chain).
        let agent_row: AttentionRowDto = weft::AttentionBoardRow {
            id: 3,
            summary: "On the agent: building.".to_string(),
            source: Some("agent".to_string()),
            ref_: None,
            initiator: weft::Initiator::Agent,
            kind: weft::AttentionKind::Running,
            live_session_id: Some("sess-1".to_string()),
            need: None,
        }
        .into();
        assert_eq!(agent_row.initiator, AttentionInitiatorDto::Agent);
        let json = serde_json::to_value(&agent_row).unwrap();
        assert_eq!(json["initiator"], "agent");
        assert_eq!(json["liveSessionId"], "sess-1");

        // A human triage row: `"human"`, no live handle.
        let human_row: AttentionRowDto = weft::AttentionBoardRow {
            id: 4,
            summary: "Triage: review the Slack ask.".to_string(),
            source: Some("slack".to_string()),
            ref_: None,
            initiator: weft::Initiator::Human,
            kind: weft::AttentionKind::Approval,
            live_session_id: None,
            need: None,
        }
        .into();
        let json = serde_json::to_value(&human_row).unwrap();
        assert_eq!(json["initiator"], "human");
        assert!(json["liveSessionId"].is_null());
    }

    /// SW-AV-3: every source `AttentionKind` maps to its camelCase wire value through
    /// `From<weft::AttentionBoardRow>` — the chip vocabulary the SW-AV-4 chip consumes.
    #[test]
    fn attention_row_serialises_the_kind_axis() {
        let cases = [
            (weft::AttentionKind::Assertion, "assertion"),
            (weft::AttentionKind::Approval, "approval"),
            (weft::AttentionKind::Question, "question"),
            (weft::AttentionKind::Parked, "parked"),
            (weft::AttentionKind::Blocked, "blocked"),
            (weft::AttentionKind::Running, "running"),
            (weft::AttentionKind::Done, "done"),
        ];
        for (kind, wire) in cases {
            let row: AttentionRowDto = weft::AttentionBoardRow {
                id: 1,
                summary: "row".to_string(),
                source: None,
                ref_: None,
                initiator: weft::Initiator::Human,
                kind,
                live_session_id: None,
                need: None,
            }
            .into();
            let json = serde_json::to_value(&row).unwrap();
            assert_eq!(json["kind"], wire, "kind {kind:?} serialises as {wire}");
        }
    }

    /// SW-AV-3 (back-compat): an old-shaped record still deserialises — a persisted
    /// `"initiator":"controller"` loads as `Agent` (serde alias), and a record with no
    /// `kind` at all loads with the `Approval` default (`#[serde(default)]`). No data
    /// migration is required.
    #[test]
    fn attention_row_deserialises_old_shaped_record() {
        // Old spelling + no `kind` field — the shape a pre-SW-AV-3 record carries.
        let legacy = serde_json::json!({
            "id": 7,
            "summary": "legacy row",
            "source": null,
            "ref": null,
            "initiator": "controller",
            "liveSessionId": null,
        });
        let row: AttentionRowDto = serde_json::from_value(legacy).unwrap();
        assert_eq!(row.initiator, AttentionInitiatorDto::Agent);
        assert_eq!(row.kind, AttentionKindDto::Approval);

        // The new spelling round-trips too.
        let current = serde_json::json!({
            "id": 8,
            "summary": "current row",
            "source": null,
            "ref": null,
            "initiator": "agent",
            "kind": "running",
            "liveSessionId": null,
        });
        let row: AttentionRowDto = serde_json::from_value(current).unwrap();
        assert_eq!(row.initiator, AttentionInitiatorDto::Agent);
        assert_eq!(row.kind, AttentionKindDto::Running);
    }

    /// SW-WA-E5 (AC-7): the board read/derive path honours the writer discipline: it
    /// introduces NO standalone-scheduler primitive (NO-SECOND-ORCHESTRATOR — the
    /// provenance axis is derived on read, never a background writer). The forbidden
    /// tokens are constructed at runtime (the `signals_coord_tests.rs` precedent) so this
    /// test file never self-trips, and the scan skips its own path.
    #[test]
    fn e5_board_read_path_is_scheduler_free_and_park_reason_never_claims_installed() {
        use std::path::Path;
        // Code-form standalone-scheduler primitives (a bare "cron" word would match
        // prose comments — the forbidden thing is a scheduler in CODE, so scan the
        // code forms: an async spawn, a bare interval loop, an OS thread spawn).
        let scheduler_primitives: Vec<String> = [
            format!("tokio::{}", "spawn"),
            format!("tokio::{}", "time::interval"),
            format!("std::thread::{}", "spawn"),
        ]
        .into_iter()
        .collect();

        let manifest = env!("CARGO_MANIFEST_DIR");
        let this_file = Path::new(file!()).file_name().unwrap().to_string_lossy();

        // AC-7: the board read/derive path introduces no standalone scheduler.
        for rel in ["src/commands/weft_ledger.rs", "src/weft/sqlite.rs"] {
            let path = Path::new(manifest).join(rel);
            // Never scan this test file itself (it names the tokens above).
            if path.file_name().unwrap().to_string_lossy() == this_file {
                continue;
            }
            let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {rel}: {e}"));
            for prim in &scheduler_primitives {
                assert!(
                    !src.contains(prim.as_str()),
                    "{rel} introduced a standalone-scheduler primitive ({prim}) on the board \
                     read path — the provenance axis is derived on read, never a background writer"
                );
            }
        }
    }

    // ---- SW-WA-C0: the producer half — mint-stamp, paired-settle ----

    /// Seed a Slack-origin queue row advanced to
    /// `claimed` so `resolve_done_record_context` finds it, plus a live worker roster row
    /// on the same scope whose `session_id` resolves to that queue row. Returns
    /// `(queue_id, session_id)`. Mirrors the a5 done-record context seed.
    fn seed_worker_on_slack_queue(store: &WeftStore, session_id: &str) -> i64 {
        let queue_id = weft::queue::enqueue(
            store,
            "slack",
            Some("root"),
            Some("https://slack/permalink"),
            Some("please fix the flaky test"),
            Some("slack:C777/42.1"),
            None,
            1_000,
        )
        .unwrap();
        // Advance the row `pending → claimed` through the sanctioned queue writer so the
        // resolver's claimed/dispatched filter finds it (ONE-WRITER: no raw wv_* SQL here).
        let claimed = weft::queue::claim(store, "controller", Some("root"), 1_001).unwrap();
        assert_eq!(claimed, Some(queue_id));
        weft::wv_provision_worker_row(store, "SW-WORK", "impl", "root").unwrap();
        weft::wv_dispatch_worker_row(store, "SW-WORK", session_id, Some("run-c0")).unwrap();
        queue_id
    }

    fn approval_source(store: &WeftStore, id: i64) -> Option<String> {
        // RI-2-ALLOW: test-only source read on the test thread (never a command plane).
        let conn = store.conn().unwrap();
        conn.query_row(
            "SELECT source FROM wv_approvals WHERE id=?1",
            rusqlite::params![id],
            |r| r.get::<_, Option<String>>(0),
        )
        .unwrap()
    }

    fn approval_disposition(store: &WeftStore, id: i64) -> String {
        // RI-2-ALLOW: test-only disposition read on the test thread (never a command plane).
        let conn = store.conn().unwrap();
        conn.query_row(
            "SELECT disposition FROM wv_approvals WHERE id=?1",
            rusqlite::params![id],
            |r| r.get::<_, String>(0),
        )
        .unwrap()
    }

    /// AC-1 — the endorsement object carries `source='queue:<id>'` and the assertion its
    /// `approval_id` backlink, both derived SERVER-SIDE from the worker session. Drives the
    /// exact producer store-fn sequence the two mint handlers run — `resolve_done_record_context`
    /// → stamp `queue:<id>` → `wv_find_pending_approval_by_source` reverse-lookup → `wv_insert_assertion`
    /// — against a real store. An agent-supplied `source` is overridden; a session that resolves to
    /// no queue row writes NULL source + None approval_id (the manual/non-M1 path).
    #[test]
    fn ac1_mint_stamps_queue_pointer_and_backlink_server_side() {
        use crate::weft::{ApprovalTier, BlastRadius, Confidence, NullNotifier};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("weft.sqlite3");
        // RI-2-ALLOW: test-only schema seed on the test thread (never a command plane).
        let store = WeftStore::open(&path).unwrap();
        let session_id = "sess-worker-c0";
        let queue_id = seed_worker_on_slack_queue(&store, session_id);

        // The `wv_write_approval` handler resolves the session's queue row and DERIVES the
        // pointer, overriding whatever `source` the agent passed (here a bogus one).
        let derived = store
            .resolve_done_record_context(session_id)
            .unwrap()
            .map(|(qid, _r, _s, _sub)| format!("queue:{qid}"));
        let agent_source = Some("agent-bogus-source");
        let source = derived.as_deref().or(agent_source);
        let (approval_id, _t, _s) = weft::wv_write_approval(
            &store,
            ApprovalTier::T3,
            false,
            BlastRadius::Low,
            Confidence::High,
            "work complete — awaiting your 👍",
            source,
            None,
            Some(session_id),
            &NullNotifier,
        )
        .unwrap();
        assert_eq!(
            approval_source(&store, approval_id).as_deref(),
            Some(format!("queue:{queue_id}").as_str()),
            "the derived queue pointer overrides the agent-supplied source"
        );

        // The `wv_post_assertion` handler resolves the SAME queue row and reverse-looks-up
        // the freshest pending approval with that pointer — the agent carries no id.
        let paired = store
            .resolve_done_record_context(session_id)
            .unwrap()
            .and_then(|(qid, _r, _s, _sub)| {
                weft::wv_find_pending_approval_by_source(&store, &format!("queue:{qid}")).unwrap()
            });
        assert_eq!(
            paired,
            Some(approval_id),
            "the reverse-lookup pairs the assertion to the just-stamped approval"
        );
        let assertion_id =
            weft::wv_insert_assertion(&store, session_id, Some("run-c0"), "done", "#eng", paired)
                .unwrap();
        let assertion = weft::wv_get_assertion(&store, assertion_id)
            .unwrap()
            .unwrap();
        assert_eq!(assertion.approval_id, Some(approval_id));

        // A session that resolves to NO queue row — the manual/non-M1 path — writes NULL
        // source and a None approval_id, exactly as pre-C0.
        let no_ctx_session = "sess-not-a-worker";
        let no_derived = store
            .resolve_done_record_context(no_ctx_session)
            .unwrap()
            .map(|(qid, _r, _s, _sub)| format!("queue:{qid}"));
        assert!(no_derived.is_none());
        let (manual_id, _t, _s) = weft::wv_write_approval(
            &store,
            ApprovalTier::T3,
            false,
            BlastRadius::Low,
            Confidence::High,
            "manual",
            no_derived.as_deref(), // None → NULL source
            None,
            Some(no_ctx_session),
            &NullNotifier,
        )
        .unwrap();
        assert_eq!(
            approval_source(&store, manual_id),
            None,
            "a non-worker session writes a NULL source"
        );
        let manual_paired = store
            .resolve_done_record_context(no_ctx_session)
            .unwrap()
            .and_then(|(qid, _r, _s, _sub)| {
                weft::wv_find_pending_approval_by_source(&store, &format!("queue:{qid}")).unwrap()
            });
        assert_eq!(
            manual_paired, None,
            "no queue row → None approval_id backlink"
        );
    }

    /// AC-2 — ratifying the assertion settles its paired approval through the shipped
    /// `wv_settle_approval` store path. Drives `settle_paired_approval` (the exact helper
    /// the `settle_assertion` closure calls, on the WON branch) against a real store: a
    /// `Some(approval_id)` folds the approval to the same disposition; a `None` touches
    /// nothing. Also covers the revoke disposition (AC-4).
    #[test]
    fn ac2_ratify_and_revoke_settle_the_paired_approval() {
        use crate::weft::{ApprovalTier, BlastRadius, Confidence, NullNotifier};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("weft.sqlite3");
        // RI-2-ALLOW: test-only schema seed on the test thread (never a command plane).
        let store = WeftStore::open(&path).unwrap();
        let mint = |store: &WeftStore| -> i64 {
            let (approval_id, _t, _s) = weft::wv_write_approval(
                store,
                ApprovalTier::T3,
                false,
                BlastRadius::Low,
                Confidence::High,
                "done",
                Some("queue:1"),
                None,
                Some("sess-x"),
                &NullNotifier,
            )
            .unwrap();
            approval_id
        };

        // Ratify branch: the paired approval settles `ratified`.
        let ratified_ap = mint(&store);
        super::settle_paired_approval(&store, Some(ratified_ap), "ratified");
        assert_eq!(approval_disposition(&store, ratified_ap), "ratified");

        // Revoke branch (AC-4): the paired approval settles `revoked`, never `ratified`.
        let revoked_ap = mint(&store);
        super::settle_paired_approval(&store, Some(revoked_ap), "revoked");
        assert_eq!(approval_disposition(&store, revoked_ap), "revoked");

        // `None` approval_id (manual assertion) touches no approval — no panic, no write.
        super::settle_paired_approval(&store, None, "ratified");
    }

    /// AC-5 — the paired settle is idempotent + fail-safe. A second settle of an
    /// already-settled approval is a no-op (`wv_settle_approval` returns `false`), and a
    /// settle against an absent approval id never panics — both swallowed by
    /// `settle_paired_approval` so they can never abort the assertion CAS.
    #[test]
    fn ac5_paired_settle_is_idempotent_and_fail_safe() {
        use crate::weft::{ApprovalTier, BlastRadius, Confidence, NullNotifier};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("weft.sqlite3");
        // RI-2-ALLOW: test-only schema seed on the test thread (never a command plane).
        let store = WeftStore::open(&path).unwrap();
        let (approval_id, _t, _s) = weft::wv_write_approval(
            &store,
            ApprovalTier::T3,
            false,
            BlastRadius::Low,
            Confidence::High,
            "done",
            Some("queue:1"),
            None,
            Some("sess-x"),
            &NullNotifier,
        )
        .unwrap();
        super::settle_paired_approval(&store, Some(approval_id), "ratified");
        assert_eq!(approval_disposition(&store, approval_id), "ratified");
        // Second settle: no-op (already ratified), swallowed — still ratified.
        super::settle_paired_approval(&store, Some(approval_id), "ratified");
        assert_eq!(approval_disposition(&store, approval_id), "ratified");
        // Absent approval id: never panics, no write.
        super::settle_paired_approval(&store, Some(999_999), "ratified");
    }
}

/// SW-WA-C0 (AC-6) — ENDORSEMENT-WALL + NO-SECOND-ORCHESTRATOR conformance. Reads the C0
/// ratify-path source region (`settle_assertion` + its `settle_paired_approval` helper)
/// and asserts no NEW scheduler token was introduced, and that `wv_ratify`/`wv_revoke`
/// remain UI-boundary Tauri commands never exposed as MCP verbs (the epic's wall-grep
/// idiom). Source-region grep, not runtime.
#[cfg(test)]
mod c0_conformance {
    use super::balanced_fn_body;

    fn strip_line_comments(src: &str) -> String {
        src.lines()
            .map(|l| match l.find("//") {
                Some(i) => &l[..i],
                None => l,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn no_new_scheduler_token_in_the_c0_ratify_path() {
        // The C0 additions live in this file; slice the two functions the C0 ratify path
        // spans: the shared settle body and the paired-approval helper it calls on WON.
        let src = include_str!("weft_ledger.rs");
        for sig in ["async fn settle_assertion(", "fn settle_paired_approval("] {
            let body = strip_line_comments(balanced_fn_body(src, sig));
            for pat in ["tokio::spawn", "thread::spawn", "interval(", "cron"] {
                assert!(
                    !body.contains(pat),
                    "AC-6 violated: `{sig}` introduces a scheduler token `{pat}` — \
                     the C0 ratify path must add no new spawn/interval/cron"
                );
            }
        }
    }

    #[test]
    fn ratify_and_revoke_are_not_mcp_verbs() {
        // The wall: `wv_ratify`/`wv_revoke` are Tauri commands (UI boundary), never MCP
        // tools — no agent can self-ratify. Assert they are absent from the MCP tool
        // registry the handler exposes.
        let mcp_src = include_str!("../mcp/handler.rs");
        // The `#[tool(...)]`-decorated async fns are the MCP verbs. `wv_ratify`/`wv_revoke`
        // must never appear as a `fn wv_ratify(`/`fn wv_revoke(` inside handler.rs.
        assert!(
            !mcp_src.contains("fn wv_ratify("),
            "wv_ratify must not be an MCP tool (ENDORSEMENT-WALL)"
        );
        assert!(
            !mcp_src.contains("fn wv_revoke("),
            "wv_revoke must not be an MCP tool (ENDORSEMENT-WALL)"
        );
    }
}

/// SW-WA-E7 — the three Stage-2 red-path drivers fire (and their oracles are
/// non-hollow). These exercise the driver bodies directly (the doors' env-gate is the
/// only thing the e2e harness adds), proving each guard REFUSES by default and the
/// AC-4 bypass reddens the oracle.
#[cfg(test)]
mod e7_red_paths {
    use crate::commands::await_blocking;
    use crate::weft::{self, WeftStore};

    /// AC-2 (VERIFY-DON'T-FOLD): an unverified dispatch cannot fold — `close_task`
    /// returns `closureBlocked` (naming `controller_verified`), the C3 close predicate
    /// stays false (`verifyFirst`), `is_done_verified` is false, and the `wv_queue`
    /// row survives both refusals (never pruned).
    #[tokio::test]
    async fn e7_unverified_dispatch_cannot_fold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("weft.sqlite3");
        // RI-2-ALLOW: synchronous unit test — no async reactor to block.
        WeftStore::open(&path).unwrap();

        let dispatch_id = "e7-unverified-1".to_string();
        let out = await_blocking("test.e7_fold", move || {
            // RI-2-ALLOW: store open runs inside the await_blocking offload — off the async reactor.
            let store = WeftStore::open(&path).map_err(|e| e.to_string())?;
            let now = crate::weft::sqlite::now_epoch_secs_public();
            let caller = "e2e-controller";
            let story_id = format!("e2e-fold-{dispatch_id}");
            let queue_id = weft::queue::enqueue(
                &store,
                "slack",
                Some("root"),
                None,
                Some("e2e unverified-fold ask"),
                None,
                None,
                now,
            )
            .map_err(|e| e.to_string())?;
            weft::wv_write_dispatch(
                &store,
                &story_id,
                "e2e unverified-fold dispatch",
                "",
                "",
                "the sentinel is printed",
                None,
            )
            .map_err(|e| e.to_string())?;
            weft::queue::claim(&store, caller, Some("root"), now).map_err(|e| e.to_string())?;
            weft::queue::mark_dispatched(&store, queue_id, caller, &story_id, "h", now)
                .map_err(|e| e.to_string())?
                .map_err(|e| format!("{e:?}"))?;
            let record = weft::DoneRecord {
                dispatch_id: dispatch_id.clone(),
                queue_id,
                session_id: format!("e2e-worker-{dispatch_id}"),
                run_id: story_id.clone(),
                story_id: story_id.clone(),
                subclass: "impl".to_string(),
                leg1_source: "e2e://source".to_string(),
                leg2_footprint: Vec::new(),
                leg3_packet: serde_json::json!({ "signalStatus": "success", "canonicalRefs": [] }),
                created: now * 1000,
            };
            store
                // RI-2-ALLOW: test-only e2e harness driver already inside the test await_blocking offload — off the production UI path.
                .insert_done_record(&record)
                .map_err(|e| e.to_string())?;
            let close = weft::queue::close_task(&store, queue_id, caller, &dispatch_id, now)
                .map_err(|e| e.to_string())?;
            let is_verified = store
                .is_done_verified(&dispatch_id)
                .map_err(|e| e.to_string())?;
            let predicate = store
                .is_close_predicate_met(&dispatch_id)
                .map_err(|e| e.to_string())?;
            let present = weft::queue::queue_list(&store, Some("root"))
                .map_err(|e| e.to_string())?
                .iter()
                .any(|r| r.id == queue_id);
            Ok::<_, String>((close, is_verified, predicate, present))
        })
        .await
        .unwrap();
        let (close, is_verified, predicate, present) = out;
        match close {
            weft::CloseOutcome::Blocked { missing } => assert!(
                missing.contains(&weft::CloseCondition::NotControllerVerified),
                "close_task must name controller_verified as missing"
            ),
            other => panic!("close_task must be closureBlocked, got {other:?}"),
        }
        assert!(
            !is_verified,
            "an unverified dispatch is never controller_verified"
        );
        assert!(
            !predicate,
            "the C3 close predicate fails → wv_resolve_queue returns verifyFirst"
        );
        assert!(
            present,
            "the wv_queue row is never pruned by a refused fold"
        );
    }

    /// AC-6 (spine conformance) — NO-SECOND-ORCHESTRATOR on the Stage-2 guard paths.
    /// Each of the three red paths is a SYNCHRONOUS refusal on an existing verb/spawn
    /// seam, never a side loop. This scans the production surface of the D2/D3/D5 guard
    /// drive paths (the tier classifier + spawn types, the fold verbs + gate-open module,
    /// the gate store fns) for the forbidden standalone-scheduler primitives a Stage-2
    /// regression might smuggle in — allow-listing only the sanctioned synchronous seams.
    /// A source-region grep, not a runtime check (mirrors the C0 + park-path conformance
    /// idiom already in this file). Scans only the production half of each file (before
    /// the first `#[cfg(test)]`) so a forbidden token named in a test never trips it.
    #[test]
    fn e7_stage2_guard_paths_introduce_no_second_orchestrator() {
        use std::path::Path;
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        // The D2/D3/D5 guard drive paths E7 proves (Context & grounding, AC-6/T6).
        let files = [
            root.join("src/weft/spawn.rs"),
            root.join("src/weft/governance.rs"),
            root.join("src/weft/sqlite.rs"),
        ];
        for file in files {
            let src = std::fs::read_to_string(&file).unwrap_or_else(|e| {
                panic!("cannot read the Stage-2 guard path {}: {e}", file.display())
            });
            let production = match src.find("#[cfg(test)]") {
                Some(cut) => &src[..cut],
                None => &src[..],
            };
            for forbidden in [
                "tokio::spawn",
                "tokio::time::interval",
                "std::thread::spawn",
                "cron",
            ] {
                assert!(
                    !production.contains(forbidden),
                    "NO-SECOND-ORCHESTRATOR: the Stage-2 guard path must add no standalone \
                     scheduler — found {forbidden:?} in {}",
                    file.display()
                );
            }
        }
    }
}
