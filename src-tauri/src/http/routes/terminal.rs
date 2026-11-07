// SPDX-License-Identifier: MIT
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::{routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use tauri::Emitter;
use utoipa::ToSchema;

use crate::http::AppState;

/// Claude Code hook payload. All hook events share these common fields.
/// Extra fields in the JSON body are silently ignored.
#[derive(Deserialize, ToSchema)]
pub struct HookPayload {
    /// The hook event name (e.g., "Stop", "StopFailure", "PermissionRequest", "UserPromptSubmit").
    pub hook_event_name: Option<String>,
    /// Stop reason — may be present in some events but not used for routing.
    #[allow(dead_code)]
    pub stop_reason: Option<String>,
    /// Present on a subagent's own hook: it is not the tab's agent, so it moves no dot.
    #[schema(value_type = Option<String>)]
    pub agent_id: Option<serde_json::Value>,
    /// The CLI's own session id, which a hook payload carries.
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct StatusResponse {
    /// One of: "updated" (a status change was emitted), "ignored" (the event was
    /// not actionable), "suppressed" (the hook was not emitted: this turn the
    /// agent declared a failed or needs-attention finish or a
    /// `signal_progress(blocked)`, or a validator failed its success, each of
    /// which suppresses every hook; or it declared a success awaiting
    /// validation, which suppresses every hook but `PermissionRequest` and
    /// `StopFailure`; a new turn or `SessionEnd` is never suppressed), or
    /// "unknown-session" (SLW-FX-17: the tab session id
    /// is not a live PTY session — or is a warm-pool placeholder — so nothing
    /// was emitted).
    pub action: &'static str,
}

/// SLW-FX-17: the outcome of classifying an inbound terminal-status post. Pure and
/// exhaustively unit-testable (the handler is not — it needs a live `AppHandle`),
/// so the liveness + placeholder + event-mapping logic all live here behind one
/// seam. `is_live` is threaded in by the handler from the terminal boundary's
/// `has_session`.
#[derive(Debug, PartialEq, Eq)]
enum TerminalStatusAction {
    /// Empty/absent session id — HTTP 400.
    BadRequest,
    /// Not a live PTY session, or a warm-pool placeholder id — logged + dropped,
    /// never emitted into a dead slot (mirrors `require_live_session`,
    /// `mcp/handler.rs`).
    UnknownSession,
    /// A recognized, live session but a non-actionable event — `{action:"ignored"}`.
    Ignored,
    /// A recognized, live session with an actionable status — emit + `"updated"`.
    Update(ResolvedHook),
}

/// A mapped hook event: the canonical event name (the frontend's Tier-1 agent
/// input) and the dot wire status it projects to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResolvedHook {
    event: &'static str,
    status: &'static str,
}

/// SLW-FX-17: classify a post without touching any `AppHandle` — `is_live` is the
/// caller's `has_session` result. A warm-pool placeholder is rejected
/// *regardless* of `is_live`, because a warming member is itself a live PTY
/// session (defense-in-depth for the failure-B correlation race).
fn classify_terminal_status(
    session_id: &str,
    is_live: bool,
    payload: &HookPayload,
) -> TerminalStatusAction {
    if session_id.is_empty() {
        return TerminalStatusAction::BadRequest;
    }
    if crate::terminal::warm_pool::is_warm_pool_id(session_id) || !is_live {
        return TerminalStatusAction::UnknownSession;
    }
    match resolve_hook(payload) {
        Some(hook) => TerminalStatusAction::Update(hook),
        None => TerminalStatusAction::Ignored,
    }
}

#[derive(Clone, Serialize, ToSchema)]
struct TerminalStatusEvent {
    tab_session_id: String,
    /// One of: "running", "done", "error", "needs-permission", "idle".
    status: String,
    /// The status hook event that produced `status` (one of
    /// `agent_state::status_hooks`). The frontend feeds it to the agent-state
    /// resolver, which keeps what the dot flattens: `thinking` vs `working`.
    hook_event: String,
}

const HEADER_TAB_SESSION_ID: &str = "x-codesense-tab-session-id";

/// Maps a hook event to its canonical name and terminal status string, through
/// the canonical agent state (`crate::agent_state`, parity-tested against the TS
/// model): the hook resolves to an `AgentState`, which projects onto the dot
/// wire literal. Returns `None` for unrecognized events (ignored). The status is
/// `"idle"` for events that clear the indicator (e.g., `SessionEnd`).
fn resolve_hook(payload: &HookPayload) -> Option<ResolvedHook> {
    if payload.agent_id.is_some() {
        return None;
    }
    // Stop fires only when Claude's turn is fully complete (not during tool use,
    // and not when the user interrupts the turn). The payload has
    // `last_assistant_message` but no `stop_reason`.
    let name = payload.hook_event_name.as_deref()?;
    let hook = crate::agent_state::status_hooks().find(|hook| hook.event == name)?;
    let state = crate::agent_state::hook_state(hook.event)?;
    Some(ResolvedHook {
        event: hook.event,
        status: state.terminal_dot_wire(),
    })
}

/// Update terminal tab AI status.
///
/// Receives Claude Code hook events and maps them to terminal tab status indicators.
/// The tab is identified by the `X-Codesense-Tab-Session-Id` header (injected into the
/// PTY environment as `CODESENSE_TAB_SESSION_ID`).
///
/// **Supported hook events:**
/// - `Stop` → status: `done` (amber dot — Claude finished, waiting for input)
/// - `StopFailure` → status: `error` (red dot)
/// - `PermissionRequest` → status: `needs-permission` (orange dot)
/// - `UserPromptSubmit` / `PreToolUse` / `PostToolUse` / `PostToolUseFailure` → status: `running` (blue pulsing dot)
/// - `SessionEnd` → status: `idle` (clears indicator — Claude exited)
///
/// Other events, and any subagent's own hook (it carries `agent_id`), return
/// `{ "action": "ignored" }`.
#[utoipa::path(
    post,
    path = "/api/v1/terminal/status",
    tag = "terminal",
    params(
        ("X-Codesense-Tab-Session-Id" = String, Header, description = "Terminal tab session ID (from CODESENSE_TAB_SESSION_ID env var)")
    ),
    request_body(content = HookPayload, description = "Claude Code hook event JSON payload"),
    responses(
        (status = 200, description = "Status updated or ignored", body = StatusResponse),
        (status = 400, description = "Missing X-Codesense-Tab-Session-Id header"),
        (status = 422, description = "Unparseable JSON body")
    )
)]
async fn terminal_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    // Use Json<T> extractor — axum enforces a 2MB default body limit.
    payload_result: Result<Json<HookPayload>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<StatusResponse>, StatusCode> {
    // Header may be absent/non-UTF8 → treat as empty; the classifier maps empty → 400.
    let tab_session_id = headers
        .get(HEADER_TAB_SESSION_ID)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let Json(payload) = payload_result.map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)?;

    // SLW-FX-17: liveness guard. A status posted under an id that is not a live
    // session (a stale/mislocated warm-pool post, an exited tab) must NOT be emitted
    // into a dead slot the way it silently was before. The exact probe
    // `require_live_session` uses (mcp/handler.rs): only a session KNOWN gone is
    // refused — one whose terminal host cannot be asked may still be running, and
    // its status belongs on its tab.
    let is_live = !tab_session_id.is_empty()
        && crate::commands::dashboard_launch::session_liveness(&state.app_handle, tab_session_id)
            .await
            .may_be_true();

    match classify_terminal_status(tab_session_id, is_live, &payload) {
        TerminalStatusAction::BadRequest => Err(StatusCode::BAD_REQUEST),
        TerminalStatusAction::UnknownSession => {
            log::warn!(
                target: "http",
                "terminal-status: dropping post for unknown/placeholder session id {:?} (not a live PTY session)",
                tab_session_id
            );
            Ok(Json(StatusResponse {
                action: "unknown-session",
            }))
        }
        TerminalStatusAction::Ignored => Ok(Json(StatusResponse { action: "ignored" })),
        TerminalStatusAction::Update(hook) => {
            record_hand_started(&state.app_handle, tab_session_id, payload.session_id.as_deref());
            Ok(Json(StatusResponse {
            action: emit_hook_status(
                &state.app_handle,
                crate::mcp::agent_dot::ledger(),
                tab_session_id,
                hook,
            ),
        }))
        }
    }
}

/// A CLI started by hand in a CodeSense tab has no launch to record. Its first hook writes a
/// minimal record: the tab, the CLI's own id from the payload, and org and team only when the
/// tab's session has an owner. Every other fact says why it is unknown.
fn record_hand_started(app: &tauri::AppHandle, tab_session_id: &str, native: Option<&str>) {
    use tauri::Manager;
    let Some(state) = app.try_state::<crate::commands::dashboard_launch::DashboardLaunchState>() else {
        return;
    };
    // A launch writes its own record at bind; a terminal the app registered is an ordinary
    // shell and gets the hand-started one.
    if state.is_agent_launch(tab_session_id) {
        return;
    }
    let (org, team) = state.terminal_owner(tab_session_id);
    let record = crate::session_usage::record::hand_started(
        tab_session_id,
        native,
        org.as_deref(),
        team.as_deref(),
    );
    tokio::spawn(crate::session_usage::record::record_hand_started_async(record));
}

/// Emit a live session's mapped hook, unless the dot ledger suppresses it.
/// The ledger gates the whole emit, so a suppressed hook reaches neither the
/// dot nor the frontend's agent-state record. Returns the response action:
/// `"updated"`, or `"suppressed"` (the hook was not emitted).
fn emit_hook_status<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    ledger: &crate::mcp::agent_dot::DotLedger,
    tab_session_id: &str,
    hook: ResolvedHook,
) -> &'static str {
    let event = terminal_status_event(tab_session_id, hook);
    let admitted =
        crate::mcp::agent_dot::admit_hook(app, ledger, tab_session_id, hook.event, || {
            if let Err(e) = app.emit("terminal-status-update", &event) {
                log::warn!(target: "http", "Failed to emit terminal-status-update: {}", e);
            }
        });
    if admitted {
        "updated"
    } else {
        "suppressed"
    }
}

fn terminal_status_event(tab_session_id: &str, hook: ResolvedHook) -> TerminalStatusEvent {
    TerminalStatusEvent {
        tab_session_id: tab_session_id.to_string(),
        status: hook.status.to_string(),
        hook_event: hook.event.to_string(),
    }
}

pub fn router() -> Router<AppState> {
    Router::new().route("/api/v1/terminal/status", post(terminal_status))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(event: &'static str, status: &'static str) -> TerminalStatusAction {
        TerminalStatusAction::Update(ResolvedHook { event, status })
    }

    fn resolve_status(payload: &HookPayload) -> Option<&'static str> {
        resolve_hook(payload).map(|hook| hook.status)
    }

    fn payload(event: &str, stop_reason: Option<&str>) -> HookPayload {
        HookPayload {
            hook_event_name: Some(event.to_string()),
            stop_reason: stop_reason.map(|s| s.to_string()),
            agent_id: None,
            session_id: None,
        }
    }

    #[test]
    fn test_stop_resolves_to_done() {
        assert_eq!(resolve_status(&payload("Stop", None)), Some("done"));
    }

    #[test]
    fn test_stop_with_any_stop_reason_still_resolves_to_done() {
        // Stop hook only fires when Claude's turn is complete — stop_reason is irrelevant
        assert_eq!(
            resolve_status(&payload("Stop", Some("end_turn"))),
            Some("done")
        );
        assert_eq!(
            resolve_status(&payload("Stop", Some("tool_use"))),
            Some("done")
        );
    }

    #[test]
    fn test_stop_failure_resolves_to_error() {
        assert_eq!(resolve_status(&payload("StopFailure", None)), Some("error"));
    }

    #[test]
    fn test_permission_request_resolves_to_needs_permission() {
        assert_eq!(
            resolve_status(&payload("PermissionRequest", None)),
            Some("needs-permission")
        );
    }

    #[test]
    fn test_user_prompt_submit_resolves_to_running() {
        assert_eq!(
            resolve_status(&payload("UserPromptSubmit", None)),
            Some("running")
        );
    }

    #[test]
    fn test_post_tool_use_resolves_to_running() {
        assert_eq!(
            resolve_status(&payload("PostToolUse", None)),
            Some("running")
        );
    }

    #[test]
    fn test_post_tool_use_failure_resolves_to_running() {
        assert_eq!(
            resolve_status(&payload("PostToolUseFailure", None)),
            Some("running")
        );
    }

    #[test]
    fn test_pre_tool_use_resolves_to_running() {
        assert_eq!(
            resolve_status(&payload("PreToolUse", None)),
            Some("running")
        );
    }

    #[test]
    fn test_session_end_resolves_to_idle() {
        assert_eq!(resolve_status(&payload("SessionEnd", None)), Some("idle"));
    }

    #[test]
    fn test_unknown_event_is_ignored() {
        assert_eq!(resolve_status(&payload("SomeOtherEvent", None)), None);
    }

    #[test]
    fn test_missing_event_name_is_ignored() {
        let p = HookPayload {
            hook_event_name: None,
            stop_reason: None,
            agent_id: None,
            session_id: None,
        };
        assert_eq!(resolve_status(&p), None);
    }

    #[test]
    fn test_parse_full_hook_payload_ignores_extra_fields() {
        let json = r#"{
            "session_id": "abc123",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/home/user/project",
            "permission_mode": "default",
            "hook_event_name": "Stop",
            "stop_reason": "end_turn"
        }"#;
        let p: HookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(resolve_status(&p), Some("done"));
    }

    #[test]
    fn a_subagents_own_hook_moves_no_dot() {
        for event in ["PreToolUse", "PostToolUse", "Stop", "PermissionRequest"] {
            for id in [serde_json::json!("a1"), serde_json::json!(7)] {
                let json = serde_json::json!({"hook_event_name": event, "agent_id": id, "agent_type": "general-purpose"});
                let p: HookPayload = serde_json::from_value(json).unwrap();
                assert_eq!(resolve_status(&p), None, "{event}");
            }
        }
        let main: HookPayload = serde_json::from_value(
            serde_json::json!({"hook_event_name": "Stop", "agent_id": null}),
        )
        .unwrap();
        assert_eq!(resolve_status(&main), Some("done"));
        assert_eq!(
            resolve_status(&payload("PreToolUse", None)),
            Some("running")
        );
    }

    #[test]
    fn test_parse_invalid_json_fails() {
        let result = serde_json::from_str::<HookPayload>("not json");
        assert!(result.is_err());
    }

    // ---- SLW-FX-17: classify_terminal_status (the testable liveness seam) ----

    #[test]
    fn test_classify_empty_id_is_bad_request() {
        // Empty/absent header → 400, independent of liveness or event.
        assert_eq!(
            classify_terminal_status("", false, &payload("Stop", None)),
            TerminalStatusAction::BadRequest
        );
        assert_eq!(
            classify_terminal_status("", true, &payload("Stop", None)),
            TerminalStatusAction::BadRequest
        );
    }

    #[test]
    fn test_classify_non_live_id_is_unknown_session() {
        // A well-formed real-shaped id that is not a live PTY session → dropped,
        // never emitted into a dead slot (the failure-B / stale-post guard).
        assert_eq!(
            classify_terminal_status("shell-abc-123", false, &payload("Stop", None)),
            TerminalStatusAction::UnknownSession
        );
    }

    #[test]
    fn test_classify_warm_pool_placeholder_is_unknown_even_when_live() {
        // Defense-in-depth: a warming member IS a live PTY session, so is_live=true,
        // yet the placeholder id must still be rejected — it is never a real tab.
        assert_eq!(
            classify_terminal_status("__warm_pool_3", true, &payload("UserPromptSubmit", None)),
            TerminalStatusAction::UnknownSession
        );
        assert_eq!(
            classify_terminal_status("__warm_pool_3", false, &payload("Stop", None)),
            TerminalStatusAction::UnknownSession
        );
    }

    #[test]
    fn test_classify_live_id_non_actionable_event_is_ignored() {
        assert_eq!(
            classify_terminal_status("shell-abc-123", true, &payload("SomeOtherEvent", None)),
            TerminalStatusAction::Ignored
        );
    }

    #[test]
    fn test_classify_live_id_actionable_event_updates() {
        assert_eq!(
            classify_terminal_status("shell-abc-123", true, &payload("Stop", None)),
            update("Stop", "done")
        );
        assert_eq!(
            classify_terminal_status("shell-abc-123", true, &payload("PermissionRequest", None)),
            update("PermissionRequest", "needs-permission")
        );
        assert_eq!(
            classify_terminal_status("shell-abc-123", true, &payload("SessionEnd", None)),
            update("SessionEnd", "idle")
        );
    }

    // ---- Payloads captured from a real `claude` run (Claude Code 2.1.x). The
    // same files feed `scripts/codesense-notify-hook.test.sh`, which proves the
    // hook script forwards them to this route unmodified. ----

    const REAL_HOOK_PAYLOADS: &[(&str, &str)] = &[
        (
            "PermissionRequest",
            include_str!("fixtures/claude-hooks/PermissionRequest.json"),
        ),
        (
            "StopFailure",
            include_str!("fixtures/claude-hooks/StopFailure.json"),
        ),
        ("Stop", include_str!("fixtures/claude-hooks/Stop.json")),
        (
            "PostToolUse",
            include_str!("fixtures/claude-hooks/PostToolUse.json"),
        ),
        (
            "PostToolUseFailure",
            include_str!("fixtures/claude-hooks/PostToolUseFailure.json"),
        ),
        (
            "UserPromptSubmit",
            include_str!("fixtures/claude-hooks/UserPromptSubmit.json"),
        ),
        (
            "SessionEnd",
            include_str!("fixtures/claude-hooks/SessionEnd.json"),
        ),
        (
            "PreToolUse",
            include_str!("fixtures/claude-hooks/PreToolUse.json"),
        ),
    ];

    #[test]
    fn real_claude_payloads_resolve_through_the_receiver_guard() {
        let expected = |event: &'static str| match event {
            "PermissionRequest" => update(event, "needs-permission"),
            "StopFailure" => update(event, "error"),
            "Stop" => update(event, "done"),
            "PreToolUse" | "PostToolUse" | "PostToolUseFailure" | "UserPromptSubmit" => {
                update(event, "running")
            }
            "SessionEnd" => update(event, "idle"),
            other => panic!("no expectation for {other}"),
        };
        for (event, raw) in REAL_HOOK_PAYLOADS {
            let p: HookPayload = serde_json::from_str(raw)
                .unwrap_or_else(|e| panic!("{event} fixture must parse as HookPayload: {e}"));
            assert_eq!(p.hook_event_name.as_deref(), Some(*event), "fixture name");
            assert_eq!(
                classify_terminal_status("shell-abc-123", true, &p),
                expected(event),
                "{event}"
            );
            // A post for a dead/placeholder session is still dropped.
            assert_eq!(
                classify_terminal_status("__warm_pool_1", true, &p),
                TerminalStatusAction::UnknownSession,
                "{event} on a placeholder"
            );
        }
    }

    #[test]
    fn every_registered_hook_event_has_a_real_payload_and_a_status() {
        for event in crate::agent_state::status_hooks().map(|hook| hook.event) {
            let (_, raw) = REAL_HOOK_PAYLOADS
                .iter()
                .find(|(e, _)| *e == event)
                .unwrap_or_else(|| panic!("no captured payload for {event}"));
            let p: HookPayload = serde_json::from_str(raw).unwrap();
            assert!(resolve_status(&p).is_some(), "{event} must resolve");
        }
    }

    /// Hook sequences observed in real Claude turns, with the event the receiver
    /// must emit for each step and the agent state it carries. The frontend
    /// seam test (`use-terminal-status-events.agent-turn.test.ts`) replays the
    /// same `emitted` payloads into the real listener and store.
    const TURN_SEQUENCES: &str = include_str!("fixtures/claude-hooks/turns/turn-sequences.json");

    fn real_payload(event: &str) -> HookPayload {
        let (_, raw) = REAL_HOOK_PAYLOADS
            .iter()
            .find(|(e, _)| *e == event)
            .unwrap_or_else(|| panic!("no captured payload for {event}"));
        serde_json::from_str(raw).unwrap()
    }

    #[test]
    fn real_turn_sequences_emit_the_hook_event_and_its_agent_state() {
        let golden: serde_json::Value = serde_json::from_str(TURN_SEQUENCES).unwrap();
        let session = golden["tab_session_id"].as_str().unwrap();
        let turns = golden["turns"].as_object().unwrap();
        assert!(turns.contains_key("silent-turn") && turns.contains_key("tool-turn"));
        for (name, steps) in turns {
            let steps = steps.as_array().unwrap();
            assert_eq!(
                steps.first().unwrap()["hook"],
                "UserPromptSubmit",
                "{name} opens"
            );
            assert_eq!(steps.last().unwrap()["hook"], "Stop", "{name} closes");
            for step in steps {
                let hook = step["hook"].as_str().unwrap();
                let TerminalStatusAction::Update(resolved) =
                    classify_terminal_status(session, true, &real_payload(hook))
                else {
                    panic!("{name}: {hook} must update");
                };
                let emitted =
                    serde_json::to_value(terminal_status_event(session, resolved)).unwrap();
                assert_eq!(emitted, step["emitted"], "{name}: {hook} wire event");
                assert_eq!(
                    crate::agent_state::hook_state(hook).map(|s| s.as_wire()),
                    step["agent_state"].as_str(),
                    "{name}: {hook} agent state"
                );
            }
        }
    }

    /// The route's emit path under a latch or a hold: `"suppressed"`, nothing on
    /// the wire; an unguarded session's hook goes out with its `hook_event`.
    #[test]
    fn a_latched_session_gets_suppressed_and_nothing_is_emitted() {
        use crate::mcp::agent_dot::{on_signal_done, DotLedger};
        use std::sync::{Arc, Mutex};
        use tauri::Listener;

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let seen: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();
        let sink = seen.clone();
        app.listen("terminal-status-update", move |e| {
            sink.lock()
                .unwrap()
                .push(serde_json::from_str(e.payload()).unwrap());
        });
        let ledger = DotLedger::default();
        let live = |_: &tauri::AppHandle<tauri::test::MockRuntime>, _: &str| true;
        let stop = resolve_hook(&payload("Stop", None)).unwrap();

        on_signal_done(app.handle(), &ledger, "latched", "failed", live);
        let before = seen.lock().unwrap().len();
        assert_eq!(
            emit_hook_status(app.handle(), &ledger, "latched", stop),
            "suppressed"
        );
        assert_eq!(seen.lock().unwrap().len(), before, "nothing emitted");

        assert_eq!(
            emit_hook_status(app.handle(), &ledger, "free", stop),
            "updated"
        );
        assert_eq!(
            seen.lock().unwrap().last().unwrap(),
            &serde_json::json!({
                "tab_session_id": "free",
                "status": "done",
                "hook_event": "Stop",
            })
        );
    }

    /// A mock app managing the launch registry, whose turn-state cache `list_sessions`
    /// reads.
    fn app_with_launch_registry() -> tauri::App<tauri::test::MockRuntime> {
        use tauri::Manager;
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        app.manage(crate::commands::dashboard_launch::DashboardLaunchState::new(2));
        app
    }

    fn last_turn_state(
        app: &tauri::App<tauri::test::MockRuntime>,
        session: &str,
    ) -> Option<crate::agent_state::AgentState> {
        use tauri::Manager;
        app.state::<crate::commands::dashboard_launch::DashboardLaunchState>()
            .turns()
            .last(session)
    }

    /// Post `event` for `session` the way the route does: classify, then emit through
    /// the dot ledger when actionable.
    fn post_hook(
        app: &tauri::App<tauri::test::MockRuntime>,
        ledger: &crate::mcp::agent_dot::DotLedger,
        session: &str,
        is_live: bool,
        event: &str,
    ) {
        if let TerminalStatusAction::Update(hook) =
            classify_terminal_status(session, is_live, &real_payload(event))
        {
            emit_hook_status(app.handle(), ledger, session, hook);
        }
    }

    #[test]
    fn terminal_status_updates_turn_state_cache() {
        use crate::agent_state::AgentState;
        let app = app_with_launch_registry();
        let ledger = crate::mcp::agent_dot::DotLedger::default();

        // A real tool turn: each hook leaves the state the list reads.
        for (event, state) in [
            ("UserPromptSubmit", AgentState::Thinking),
            ("PreToolUse", AgentState::Working),
            ("PostToolUse", AgentState::Thinking),
            ("PermissionRequest", AgentState::NeedsPermission),
            ("Stop", AgentState::Done),
            ("StopFailure", AgentState::Errored),
        ] {
            post_hook(&app, &ledger, "shell-a", true, event);
            assert_eq!(last_turn_state(&app, "shell-a"), Some(state), "{event}");
        }

        // SessionEnd drops the entry instead of storing Idle.
        post_hook(&app, &ledger, "shell-a", true, "SessionEnd");
        assert_eq!(last_turn_state(&app, "shell-a"), None);

        // A dropped post (dead session or warm-pool placeholder) records nothing.
        post_hook(&app, &ledger, "shell-dead", false, "Stop");
        post_hook(&app, &ledger, "__warm_pool_1", true, "Stop");
        assert_eq!(last_turn_state(&app, "shell-dead"), None);
        assert_eq!(last_turn_state(&app, "__warm_pool_1"), None);
    }

    /// MCP signals write the dot through the ledger, not the hook route; the
    /// `list_sessions` state follows them, and a hook the ledger suppresses (so the dot
    /// never shows it) does not move it either.
    #[test]
    fn signal_done_updates_list_sessions_state() {
        use crate::agent_state::AgentState;
        use crate::mcp::agent_dot::{
            on_ask_user_open, on_signal_done, on_signal_progress, DotLedger,
        };
        let app = app_with_launch_registry();
        let ledger = DotLedger::default();
        let live = |_: &tauri::AppHandle<tauri::test::MockRuntime>, _: &str| true;
        let sid = "shell-signal";

        post_hook(&app, &ledger, sid, true, "UserPromptSubmit");
        on_signal_done(app.handle(), &ledger, sid, "success", live);
        assert_eq!(last_turn_state(&app, sid), Some(AgentState::Done));

        // A declared failure latches: the turn's trailing `Stop` is suppressed on the
        // dot, so the list keeps `errored` too.
        post_hook(&app, &ledger, sid, true, "UserPromptSubmit");
        on_signal_done(app.handle(), &ledger, sid, "failed", live);
        assert_eq!(last_turn_state(&app, sid), Some(AgentState::Errored));
        post_hook(&app, &ledger, sid, true, "Stop");
        assert_eq!(last_turn_state(&app, sid), Some(AgentState::Errored));

        // `signal_progress(blocked)` and an open `ask_user` wait on a person; the ask's
        // resolution puts the agent back to work.
        post_hook(&app, &ledger, sid, true, "UserPromptSubmit");
        on_signal_progress(
            app.handle(),
            &ledger,
            sid,
            crate::roles::Phase::Blocked,
            live,
        );
        assert_eq!(last_turn_state(&app, sid), Some(AgentState::Blocked));
        on_signal_progress(app.handle(), &ledger, sid, crate::roles::Phase::Dev, live);
        assert_eq!(last_turn_state(&app, sid), Some(AgentState::Working));
        let ask = on_ask_user_open(app.handle(), &ledger, sid, live).expect("ask opens");
        assert_eq!(
            last_turn_state(&app, sid),
            Some(AgentState::NeedsPermission)
        );
        ask.resolve();
        assert_eq!(last_turn_state(&app, sid), Some(AgentState::Working));
    }

    /// The tab dot as the frontend listener folds `terminal-status-update` emits
    /// (`use-terminal-status-events.ts`): a hook that cannot clear a pending permission
    /// (a sibling tool call's `PreToolUse`) leaves `needs-permission` standing.
    fn dot_of(seen: &std::sync::Mutex<Vec<serde_json::Value>>, session: &str) -> Option<String> {
        let mut dot: Option<String> = None;
        for p in seen.lock().unwrap().iter() {
            if p["tab_session_id"] != session {
                continue;
            }
            if let Some(event) = p["hook_event"].as_str() {
                if !crate::agent_state::hook_clears_permission(event)
                    && dot.as_deref() == Some("needs-permission")
                {
                    continue;
                }
            }
            let status = p["status"].as_str().unwrap();
            dot = (status != "idle").then(|| status.to_string());
        }
        dot
    }

    /// A `PreToolUse` admitted while an `ask_user` or a `PermissionRequest` is pending
    /// does not clear `needs-permission` on the dot, so `list_sessions` keeps `needs-you`.
    #[test]
    fn pretooluse_during_pending_ask_keeps_needs_you_in_list_sessions() {
        use crate::agent_state::AgentState;
        use crate::mcp::agent_dot::{on_ask_user_open, DotLedger};
        use crate::mcp::session_list::{session_state, SessionState};
        use tauri::Listener;
        let app = app_with_launch_registry();
        let seen: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>> =
            std::sync::Arc::default();
        let sink = seen.clone();
        app.listen("terminal-status-update", move |event| {
            sink.lock()
                .unwrap()
                .push(serde_json::from_str(event.payload()).unwrap());
        });
        let ledger = DotLedger::default();
        let live = |_: &tauri::AppHandle<tauri::test::MockRuntime>, _: &str| true;
        let agree = |sid: &str, dot: &str, state: SessionState| {
            assert_eq!(dot_of(&seen, sid).as_deref(), Some(dot), "{sid} dot");
            assert_eq!(
                session_state(true, last_turn_state(&app, sid)),
                state,
                "{sid} list_sessions"
            );
        };

        // An open ask, then a sibling tool call's PreToolUse.
        let sid = "shell-ask";
        post_hook(&app, &ledger, sid, true, "UserPromptSubmit");
        let ask = on_ask_user_open(app.handle(), &ledger, sid, live).expect("ask opens");
        post_hook(&app, &ledger, sid, true, "PreToolUse");
        agree(sid, "needs-permission", SessionState::NeedsYou);
        // A hook that does clear a permission moves both.
        post_hook(&app, &ledger, sid, true, "PostToolUse");
        agree(sid, "running", SessionState::Running);
        ask.resolve();
        agree(sid, "running", SessionState::Running);

        // A pending PermissionRequest, then a PreToolUse.
        let sid = "shell-perm";
        post_hook(&app, &ledger, sid, true, "UserPromptSubmit");
        post_hook(&app, &ledger, sid, true, "PermissionRequest");
        post_hook(&app, &ledger, sid, true, "PreToolUse");
        agree(sid, "needs-permission", SessionState::NeedsYou);
        assert_eq!(
            last_turn_state(&app, sid),
            Some(AgentState::NeedsPermission)
        );
        // Without a pending permission, PreToolUse reads as work on both.
        post_hook(&app, &ledger, sid, true, "PostToolUse");
        post_hook(&app, &ledger, sid, true, "PreToolUse");
        agree(sid, "running", SessionState::Running);
    }

    #[test]
    fn test_is_warm_pool_id() {
        use crate::terminal::warm_pool::is_warm_pool_id;
        assert!(is_warm_pool_id("__warm_pool_1"));
        assert!(is_warm_pool_id("__warm_pool_999"));
        assert!(!is_warm_pool_id("shell-abc-123"));
        assert!(!is_warm_pool_id(""));
    }
}
