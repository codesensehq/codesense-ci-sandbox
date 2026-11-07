// SPDX-License-Identifier: MIT
//! SW-WV-F7 / SW-WV-RT1 — the FE dispatch bridges: the `wv_dispatch_worker` and
//! `wv_signal_done_bridge` Tauri commands.
//!
//! `wv_dispatch_worker` is the door the Weft Board's "Dispatch & release" button
//! (`invoke("wv_dispatch_worker", ...)` from `commands.ts::dispatchWeftWorker`,
//! SW-WV-F2) resolves to. The dispatch DECISION and every `wv_*` write still live in
//! the C1 dispatch core ([`crate::weft::wv_write_dispatch`], the sanctioned writer,
//! MODULE-HOME §5.3); this command:
//!
//! 1. gates the verb at the `Interactive` tier (ARCHITECTURE §3.1 TWO-SEAM-GATE —
//!    `InteractiveGate::admit("wv_dispatch_worker")` here + the `"wv_dispatch_worker"`
//!    entry in [`crate::commands::interactive_gate::INTERACTIVE_VERBS`]),
//! 2. resolves the active team's `weft.sqlite3` path (the canonical
//!    [`crate::weft::weft_store_path`] via `active_weft_store_path`, §1.1),
//! 3. delegates the 4-field brief write to C1 OFF the async/UI thread via
//!    [`await_blocking`] (`tokio::task::spawn_blocking`; no `RI-2-ALLOW` marker
//!    needed — that marker is only for the `with_blocking` escape hatch, per
//!    `commands/ri2_backstop.rs`), and
//! 4. (SW-WV-RT1) THREADS THE DISPATCH THROUGH to a real worker: it builds a
//!    [`crate::weft::WorkerSpawnRequest`] from the brief + the resolved roster seat
//!    and calls [`crate::weft::wv_spawn_worker`] (the same `RealWorkerLauncher` PTY
//!    path the MCP handler uses), then emits `wv-roster-changed`. So one Dispatch
//!    produces a real worker + roster row, not just a `state='open'` record.
//!
//! The FE brief (`goal` / `context` / `constraints` / `doneWhen`) maps onto the C1
//! record's four structural fields (`task` / `context` / `constraints` /
//! `acceptance` — the Done-When). `subclass` selects the worker archetype
//! (SW-WV-A2); `queueId`, when the form was opened from a Board row, becomes the
//! dispatch record's `story_id`/roster key so the brief is written under a stable
//! key. The command never runs SQL or makes a dispatch decision on the async reactor.
//!
//! `wv_signal_done_bridge` (SW-WV-RT1) is the DONE half: a Tauri door onto the same
//! `signal_done` hook registry the MCP verb dispatches, so a worker completion
//! (roster advance + done-record + handoff + stamp) is reachable from the UI and the
//! QH harness, not only the MCP protocol path. See its own docstring below.

use crate::commands::await_blocking;
use crate::commands::interactive_gate::InteractiveGate;
use crate::weft::{WorkerSpawnRequest, WorkerSubclass};

/// The resolved dispatch on the wire — mirrors the TS `WvDispatchResult`
/// (`commands.ts`, SW-WV-F2). `ok` is the green go-ahead the form releases on;
/// `dispatch_id` is the C1 record's minted id; `worker_handle` is the roster key the
/// brief was written under (the fleet-view backlink, SW-WV-F3); `done_when` echoes
/// the recorded acceptance (the externally-checkable finish line, fixed at creation).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WvDispatchResult {
    pub ok: bool,
    pub dispatch_id: String,
    pub worker_handle: String,
    pub done_when: String,
}

/// The `worker_handle` / `story_id` (roster key) a dispatch is written under. When
/// the form was opened from a Board row it carries that row's `queue_id`; otherwise
/// (header "+ Dispatch") a subclass-scoped synthetic key keeps the record keyed
/// under a stable, human-legible handle. Pure — unit-tested directly.
fn resolve_worker_handle(queue_id: Option<i64>, subclass: &str) -> String {
    match queue_id {
        Some(id) => format!("wv-queue-{id}"),
        None => format!("wv-{subclass}"),
    }
}

/// Seed the spawned worker's first prompt from the 4-field brief. The dispatch
/// record already fixes the Done-When at creation; this is the human-legible brief
/// the worker reads as its opening context. Empty optional fields are omitted so a
/// bare `goal` / `done_when` dispatch produces a clean prompt.
fn brief_prompt(goal: &str, context: &str, constraints: &str, done_when: &str) -> String {
    let mut out = format!("Task: {goal}\n");
    if !context.trim().is_empty() {
        out.push_str(&format!("Context: {context}\n"));
    }
    if !constraints.trim().is_empty() {
        out.push_str(&format!("Constraints: {constraints}\n"));
    }
    out.push_str(&format!("Done when: {done_when}\n"));
    out
}

/// Build the [`WorkerSpawnRequest`] for a dispatch's real worker. The dispatch's
/// roster key (`worker_handle`) is the worker's `story_id` (one live worker per
/// story per scope). An unknown subclass string falls back to
/// [`WorkerSubclass::Impl`] — the acting profile — so a dispatch always yields a
/// worker rather than silently no-op'ing. Pure — unit-tested directly.
fn build_spawn_request(
    worker_handle: &str,
    subclass: &str,
    goal: &str,
    context: &str,
    constraints: &str,
    done_when: &str,
    run_id: Option<String>,
) -> WorkerSpawnRequest {
    WorkerSpawnRequest {
        story_id: worker_handle.to_string(),
        subclass: WorkerSubclass::from_wire(subclass).unwrap_or(WorkerSubclass::Impl),
        initial_prompt: Some(brief_prompt(goal, context, constraints, done_when)),
        provider: None,
        scope: "root".to_string(),
        run_id,
        // The durable bridge has no verified caller session.
        launched_by: None,
    }
}

/// SW-WV-F7 — the Tauri bridge for the weft-board dispatch surface. A thin forwarder
/// over the C1 dispatch core: gate → resolve the active team's store path → delegate
/// the 4-field brief write off-thread. No dispatch logic, no SQL, no `wv_*` write
/// lives here (AC-3).
///
/// `InteractiveGate::admit("wv_dispatch_worker")` is the FIRST statement (AC-2),
/// before any app-state access or blocking work — a verb absent from
/// `INTERACTIVE_VERBS` returns `Err` without advancing (deny-first).
#[tauri::command]
pub async fn wv_dispatch_worker(
    app: tauri::AppHandle,
    queue_id: Option<i64>,
    subclass: String,
    goal: String,
    context: Option<String>,
    constraints: Option<String>,
    done_when: String,
) -> Result<WvDispatchResult, String> {
    InteractiveGate::admit("wv_dispatch_worker").map_err(|e| e.to_string())?;

    // The active team's weft.sqlite3 path (canonical, §1.1) — resolved on the async
    // side; the store is opened INSIDE the blocking closure by the C1 write path.
    let (path, store_org) = crate::commands::weft_ledger::active_weft_store(&app).await?;

    let worker_handle = resolve_worker_handle(queue_id, &subclass);
    let context = context.unwrap_or_default();
    let constraints = constraints.unwrap_or_default();
    let handle_for_write = worker_handle.clone();
    let done_when_for_write = done_when.clone();
    // Kept for the spawn request built after the C1 write (the closure below moves
    // the write-side copies).
    let goal_for_spawn = goal.clone();
    let context_for_spawn = context.clone();
    let constraints_for_spawn = constraints.clone();
    let done_when_for_spawn = done_when.clone();

    let spawn_path = path.clone();
    let dispatch_id = await_blocking("wv_dispatch_worker.dispatch", move || {
        let store =
            crate::weft::WeftStore::open(path).map_err(|e| format!("open weft store: {e}"))?;
        // C1's sanctioned single writer: it validates the brief (a blank `goal`/task
        // or `done_when`/acceptance is refused), co-commits the audit row, and mints
        // the record id. Zero dispatch decisions are made here.
        crate::weft::wv_write_dispatch(
            &store,
            &handle_for_write,
            &goal,
            &context,
            &constraints,
            &done_when_for_write,
            None,
        )
        .map_err(|e| e.to_string())
    })
    .await?;

    // Thread the dispatch THROUGH to a real worker: build the spawn request from the
    // brief + the resolved roster seat and open the PTY via the same
    // `RealWorkerLauncher` path the MCP handler uses. A spawn failure is surfaced but does not orphan the dispatch record — the
    // C1 row is already durably written (state='open').
    let req = build_spawn_request(
        &worker_handle,
        &subclass,
        &goal_for_spawn,
        &context_for_spawn,
        &constraints_for_spawn,
        &done_when_for_spawn,
        // SW-WA-A5: stamp the C1 dispatch record id as the roster's run_id so the
        // done-record / watchdog path can correlate the live session to this dispatch.
        Some(dispatch_id.to_string()),
    );
    crate::weft::wv_spawn_worker(&app, spawn_path, store_org, req)
        .await
        .map_err(|e| e.to_string())?;
    // Nudge the fleet view: a new worker + roster row now exist.
    if let Err(e) = tauri::Emitter::emit(&app, "wv-roster-changed", ()) {
        log::warn!(target: "weft", "wv-roster-changed emit failed: {e}");
    }

    Ok(WvDispatchResult {
        ok: true,
        dispatch_id: dispatch_id.to_string(),
        worker_handle,
        done_when,
    })
}

/// SW-WV-RT1 — the DONE bridge: a Tauri door onto the SAME `signal_done` hook
/// registry the MCP `signal_done` verb dispatches
/// ([`crate::mcp::handler::build_signal_done_hooks`]). It exists so a worker
/// completion can be driven from a Tauri command — reachable from the UI and the QH
/// harness (`core.invoke`) — not only the MCP protocol path. It assembles the
/// identical ordered hook set (roster advance `dispatch → signal`,
/// dispatch-done-stamp, done-record, handoff-record, watchdog-cancel,
/// `wv_signals`-insert) and dispatches it for the completing session.
///
/// Unlike the MCP verb (which `tokio::spawn`s the dispatch so `signal_done` returns
/// before any hook resolves) this AWAITS the dispatch, so the caller — a test or a QH
/// step — sees the roster advanced and the done row stamped on return. Each hook is
/// still isolated behind its own `Result` inside `dispatch_hooks`; a missing active
/// team / non-worker session is a graceful skip. The done `status` is fixed to
/// `success` (the bridge closes a worker whose done record is being folded); a caller
/// needing a different status uses the MCP `signal_done` verb. The store I/O each hook
/// performs runs off-thread via `await_blocking` inside the hook, so no `WeftStore`
/// call touches this command thread.
#[tauri::command]
pub async fn wv_signal_done_bridge(
    app: tauri::AppHandle,
    session_id: String,
    done_record: Option<crate::mcp::handler::WireDoneRecord>,
) -> Result<(), String> {
    use tauri::Manager;
    let context = crate::commands::route_to_context::derive_active_context(&app).await?;
    let launch = app.try_state::<crate::commands::dashboard_launch::DashboardLaunchState>();
    let path = crate::mcp::local_identity::weft_store_path(
        &context,
        Some(&session_id),
        launch.as_deref(),
        None,
    )?;
    let worker_key = launch
        .as_deref()
        .and_then(|state| state.handle_for_session(&session_id))
        .unwrap_or_else(|| session_id.clone());
    let registry = crate::mcp::handler::build_signal_done_hooks(
        &app,
        path,
        worker_key,
        "success".to_string(),
        None,
        done_record,
    );
    registry.dispatch_hooks(session_id).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::interactive_gate::{GateRefusal, INTERACTIVE_VERBS};

    /// AC-2: the gate is the door. `wv_dispatch_worker` MUST be in `INTERACTIVE_VERBS`
    /// (else the command's first statement rejects every call), and an unadmitted verb
    /// is refused `not-granted:` without advancing. Removing the entry flips the first
    /// assertion red — the machine-checkable proof the two-seam gate holds.
    #[test]
    fn wv_dispatch_worker_is_an_interactive_verb() {
        assert!(
            INTERACTIVE_VERBS.contains(&"wv_dispatch_worker"),
            "wv_dispatch_worker must be admitted — the command's first statement gates on it"
        );
        assert!(InteractiveGate::admit("wv_dispatch_worker").is_ok());
    }

    /// AC-2 (deny-first): a verb outside the tier is refused with the typed
    /// `not-granted:` prefix and never reaches the store path.
    #[test]
    fn an_unadmitted_verb_is_refused_not_granted() {
        let refusal =
            InteractiveGate::admit("wv_dispatch_worker_not_a_real_verb").expect_err("not admitted");
        assert!(matches!(refusal, GateRefusal::NotGranted(_)));
        assert!(refusal.to_string().starts_with("not-granted: "));
    }

    /// AC-1/AC-5: the result carries all four fields the TS `WvDispatchResult`
    /// declares (`ok` / `dispatch_id` / `worker_handle` / `done_when`) — a
    /// compile-time shape check plus the serialized wire keys.
    #[test]
    fn wv_dispatch_result_has_all_fields() {
        let r = WvDispatchResult {
            ok: true,
            dispatch_id: "7".to_string(),
            worker_handle: "wv-queue-42".to_string(),
            done_when: "PR opened".to_string(),
        };
        assert!(r.ok);
        assert_eq!(r.dispatch_id, "7");
        assert_eq!(r.worker_handle, "wv-queue-42");
        assert_eq!(r.done_when, "PR opened");
        let json = serde_json::to_value(&r).unwrap();
        for key in ["ok", "dispatch_id", "worker_handle", "done_when"] {
            assert!(json.get(key).is_some(), "wire shape must carry {key}");
        }
    }

    /// The worker-handle resolver: a Board-row dispatch keys under its queue id; a
    /// header dispatch falls back to a subclass-scoped handle. Both are stable,
    /// human-legible roster keys the fleet-view backlink (F3) can display.
    #[test]
    fn worker_handle_prefers_queue_id_then_subclass() {
        assert_eq!(resolve_worker_handle(Some(42), "impl"), "wv-queue-42");
        assert_eq!(resolve_worker_handle(None, "review"), "wv-review");
    }

    /// The spawn request built after the C1 write threads the dispatch through to a
    /// real worker: the roster key becomes the worker's `story_id`, the subclass is
    /// parsed, and the 4-field brief seeds the initial prompt. An unknown subclass
    /// falls back to `impl` so a dispatch always yields a worker.
    #[test]
    fn build_spawn_request_carries_handle_subclass_and_brief() {
        let req = build_spawn_request(
            "wv-queue-42",
            "review",
            "do the thing",
            "some context",
            "no force push",
            "PR opened",
            Some("disp-7".to_string()),
        );
        assert_eq!(req.story_id, "wv-queue-42");
        assert_eq!(req.subclass, WorkerSubclass::Review);
        assert_eq!(req.scope, "root");
        assert_eq!(
            req.run_id.as_deref(),
            Some("disp-7"),
            "the dispatch id is threaded as the roster run correlation (SW-WA-A5)"
        );
        let prompt = req.initial_prompt.expect("brief seeds the prompt");
        assert!(prompt.contains("do the thing"));
        assert!(prompt.contains("some context"));
        assert!(prompt.contains("no force push"));
        assert!(prompt.contains("PR opened"));
    }

    #[test]
    fn build_spawn_request_defaults_unknown_subclass_to_impl() {
        let req = build_spawn_request("wv-x", "not-a-subclass", "g", "", "", "d", None);
        assert_eq!(req.subclass, WorkerSubclass::Impl);
    }

    /// A bare `goal` / `done_when` dispatch (no context / constraints) produces a
    /// clean prompt that omits the empty optional lines.
    #[test]
    fn brief_prompt_omits_empty_optional_fields() {
        let prompt = brief_prompt("build X", "", "  ", "merged");
        assert!(prompt.contains("Task: build X"));
        assert!(prompt.contains("Done when: merged"));
        assert!(!prompt.contains("Context:"));
        assert!(!prompt.contains("Constraints:"));
    }
}
