// SPDX-License-Identifier: MIT
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};

use tokio::sync::oneshot;

/// Full viewer state pushed from the frontend as a JSON blob.
/// Stored in Rust memory for MCP tools to read without disk I/O.
/// No typed struct — we store raw JSON so the frontend can evolve
/// without requiring Rust changes.
#[derive(Default)]
pub struct McpViewerState {
    inner: RwLock<serde_json::Value>,
    snapshot_sequence: AtomicU64,
    published_snapshot_sequence: Mutex<u64>,
}

/// Typed snapshot of the currently-selected `(org_slug, team_name)`.
/// Source of truth for "which team is active right now" as observed
/// from the Rust side. Updated synchronously by
/// `commands::content_engine::set_active_team_for_wave` — the canonical
/// "team is changing" Tauri command, called by `use-workspace-viewer-sync.ts`
/// on first mount and on every team switch.
///
/// The context MCP tools (`sense_sync_*`, `route_file_to_context`, …)
/// need to know which team to target without the caller supplying the
/// slug. Reading this state in `mcp/handler.rs::derive_active_context`
/// keeps that race-free: the moment `set_active_team_for_wave` returns,
/// a tool call from a sibling terminal sees the new team.
/// `McpViewerState` was the alternative (frontend-pushed JSON mirror,
/// debounced) — rejected because its push cycle could lag a team click
/// by ~100ms, opening a window for a call to write to the wrong team.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveTeam {
    pub org_slug: String,
    pub team_name: String,
}

/// Holds the last value set via [`ActiveTeamState::update`]. `None`
/// until the first team selection — `derive_active_context` surfaces
/// that to MCP callers as an `InvalidParams` error (the caller tells the
/// user that no team is active).
#[derive(Default)]
pub struct ActiveTeamState {
    inner: RwLock<Option<ActiveTeam>>,
}

impl ActiveTeamState {
    pub(crate) fn clear(&self) {
        *self.inner.write().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub(crate) fn clear_org(&self, org_slug: &str) {
        let mut active = self.inner.write().unwrap_or_else(|e| e.into_inner());
        if active
            .as_ref()
            .is_some_and(|team| team.org_slug == org_slug)
        {
            *active = None;
        }
    }

    /// Atomically replace the active context. Called by
    /// `set_active_team_for_wave` after segment validation has already
    /// rejected empty/whitespace-padded inputs, so the values stored
    /// here are guaranteed clean.
    pub fn update(&self, ctx: ActiveTeam) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        *inner = Some(ctx);
    }

    /// Clone the current snapshot. Returns `None` before the first team
    /// selection.
    pub fn get(&self) -> Option<ActiveTeam> {
        self.inner.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl McpViewerState {
    fn next_snapshot_sequence(&self) -> u64 {
        self.snapshot_sequence.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn update(&self, state: serde_json::Value) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        *inner = state;
    }

    pub fn replace_workspace_groups(&self, team: &str, groups: serde_json::Value, revision: u64) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        if !inner["customGroupsByTeam"].is_object() {
            inner["customGroupsByTeam"] = serde_json::json!({});
        }
        inner["customGroupsByTeam"][team] = groups;
        if !inner["workspaceGroupVersionsByTeam"].is_object() {
            inner["workspaceGroupVersionsByTeam"] = serde_json::json!({});
        }
        inner["workspaceGroupVersionsByTeam"][team] = serde_json::json!(revision);
    }
    fn update_snapshot(&self, sequence: u64, mut snapshot: serde_json::Value) {
        let mut published = self
            .published_snapshot_sequence
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if sequence <= *published {
            return;
        }
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        super::workspace_groups::reconcile_snapshot(&inner, &mut snapshot);
        *inner = snapshot;
        *published = sequence;
    }

    pub fn get(&self) -> serde_json::Value {
        self.inner.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Atomically merge a JSON patch into the current state under a single write lock.
    /// Prevents concurrent callers from interleaving read-merge-write sequences.
    pub fn merge_patch(
        &self,
        patch: &serde_json::Value,
        merge_fn: fn(&mut serde_json::Value, &serde_json::Value),
    ) -> serde_json::Value {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        merge_fn(&mut inner, patch);
        inner.clone()
    }

    /// Drop the workspace rows `matches` picks in `team_key` from the mirror
    /// (open and minimized alike). Returns how many rows went.
    pub fn retire_workspaces(
        &self,
        team_key: &str,
        matches: &dyn Fn(&serde_json::Value) -> bool,
    ) -> usize {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        retire_workspace_rows(&mut inner, team_key, matches)
    }
}

/// Drop the workspace rows `matches` picks from one team of a viewer-state
/// object: the mirror's root, or the saved state's `viewer` section. Removes
/// them from `workspaces` and from the minimized `softClosedWorkspaces`
/// shadows, and clears `activeWorkspace` for that team when it named a dropped
/// row. Returns how many rows were dropped.
pub(crate) fn retire_workspace_rows(
    viewer: &mut serde_json::Value,
    team_key: &str,
    matches: &dyn Fn(&serde_json::Value) -> bool,
) -> usize {
    let row_id = |row: &serde_json::Value| {
        row.get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let mut dropped: Vec<String> = Vec::new();
    if let Some(rows) = viewer
        .get_mut("workspaces")
        .and_then(|teams| teams.get_mut(team_key))
        .and_then(serde_json::Value::as_array_mut)
    {
        rows.retain(|row| {
            let hit = matches(row);
            if hit {
                dropped.push(row_id(row));
            }
            !hit
        });
    }
    if let Some(shadows) = viewer
        .get_mut("softClosedWorkspaces")
        .and_then(|teams| teams.get_mut(team_key))
        .and_then(serde_json::Value::as_object_mut)
    {
        shadows.retain(|_, shadow| match shadow.get("workspace") {
            Some(row) if matches(row) => {
                dropped.push(row_id(row));
                false
            }
            _ => true,
        });
    }
    let active_named_dropped = viewer
        .get("activeWorkspace")
        .and_then(|active| active.get(team_key))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|id| dropped.iter().any(|d| d == id));
    if active_named_dropped {
        if let Some(active) = viewer
            .get_mut("activeWorkspace")
            .and_then(serde_json::Value::as_object_mut)
        {
            active.remove(team_key);
        }
    }
    dropped.len()
}

/// Tauri command: frontend pushes full viewer state to Rust memory.
/// Called from persistState() in the Zustand viewer store. Keep this
/// synchronous so the sequence is assigned in IPC dispatch order.
#[tauri::command]
pub fn push_viewer_state(
    app: tauri::AppHandle,
    expected_principal: Option<String>,
    snapshot: serde_json::Value,
) -> Result<(), String> {
    use tauri::Manager;
    let state = app.state::<McpViewerState>();
    let sequence = state.next_snapshot_sequence();
    publish_viewer_state(
        &app.state::<crate::commands::local_profile::LocalProfileState>(),
        &state,
        expected_principal.as_deref(),
        sequence,
        snapshot,
    )?;
    crate::live_hub::inputs_changed(&app);
    Ok(())
}

pub(crate) fn publish_viewer_state(
    profile: &crate::commands::local_profile::LocalProfileState,
    state: &McpViewerState,
    expected: Option<&str>,
    sequence: u64,
    snapshot: serde_json::Value,
) -> Result<(), String> {
    if !snapshot.is_object() {
        return Err("snapshot must be a JSON object".into());
    }
    let expected = expected.ok_or("Choose a local profile before publishing viewer state")?;
    profile.with_expected_published(expected, || {
        state.update_snapshot(sequence, snapshot);
        Ok(())
    })
}

/// Outcome of a frontend canvas render: `Ok(pngBase64)` or `Err(message)`.
///
/// MCP architectural reframe (2026-10): previously `Ok(pngPath)` — the
/// frontend wrote an `.excalidraw.png` sidecar on disk and handed the path
/// back. Now the frontend returns the base64-encoded PNG bytes DIRECTLY so
/// the MCP verb can embed them inline in the tool response. No sidecar is
/// written: agents consume the render inline, no PNG lands in the docs tree.
pub type CanvasRenderResult = Result<String, String>;

/// Correlated request/response bridge for the `render_canvas` MCP
/// verb. The render itself can only happen in the live webview (Excalidraw's
/// exporter is DOM-bound), so the Rust verb emits a `canvas:render-request`
/// event and awaits the frontend's reply through here: each request registers
/// a [`oneshot`] sender keyed by a process-unique id; the frontend renders the
/// scene to a base64 PNG and calls [`resolve_canvas_render`], which routes
/// the result back to the matching sender. Mirrors `WorktreeRunRegistry`
/// (`commands::worktree`) — the same insert/take oneshot pattern.
#[derive(Default)]
pub struct CanvasRenderRegistry {
    inner: Mutex<HashMap<String, oneshot::Sender<CanvasRenderResult>>>,
    seq: AtomicU64,
}

impl CanvasRenderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate a unique request id, store its sender, and return
    /// `(request_id, receiver)`. The id is process-monotonic — unique without
    /// pulling in a uuid dependency.
    pub fn register(&self) -> (String, oneshot::Receiver<CanvasRenderResult>) {
        let id = format!("canvas-render-{}", self.seq.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), tx);
        (id, rx)
    }

    /// Remove and return the sender for `request_id`, if still pending.
    /// Consume-once: a second resolve (e.g. a multi-window `emit` broadcast,
    /// or a late reply after the verb already timed out) finds nothing and is
    /// a no-op — first reply wins.
    pub fn take(&self, request_id: &str) -> Option<oneshot::Sender<CanvasRenderResult>> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(request_id)
    }
}

/// Tauri command: the frontend render bridge reports a finished render back to
/// the waiting `render_canvas` MCP verb. Exactly one of `png_base64` / `error`
/// is expected; the receiver may already be gone (the verb timed out), in
/// which case the send is silently dropped.
///
/// MCP architectural reframe (2026-10): the reply carries the base64 PNG
/// bytes DIRECTLY (previously the frontend wrote an `.excalidraw.png` sidecar
/// and reported the path). The MCP verb embeds the bytes inline in its tool
/// response — no PNG lands on disk.
#[tauri::command]
pub fn resolve_canvas_render(
    registry: tauri::State<'_, CanvasRenderRegistry>,
    request_id: String,
    png_base64: Option<String>,
    error: Option<String>,
) -> Result<(), String> {
    if let Some(tx) = registry.take(&request_id) {
        let result: CanvasRenderResult = match (png_base64, error) {
            (Some(data), _) => Ok(data),
            (None, Some(e)) => Err(e),
            (None, None) => Err("render resolved with neither bytes nor an error".into()),
        };
        let _ = tx.send(result);
    }
    Ok(())
}

/// Maximum UTF-8 bytes returned for a rendered document (512 KiB).
pub const RENDERED_DOCUMENT_MAX_BYTES: usize = 512 * 1024;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderedDocumentCapture {
    pub content: String,
    pub captured_at: String,
    pub truncated: bool,
}
impl RenderedDocumentCapture {
    fn bound_content(&mut self) {
        if self.content.len() > RENDERED_DOCUMENT_MAX_BYTES {
            let mut end = RENDERED_DOCUMENT_MAX_BYTES;
            while !self.content.is_char_boundary(end) {
                end -= 1;
            }
            self.content.truncate(end);
            self.truncated = true;
        }
    }
}
pub type RenderedDocumentResult = Result<RenderedDocumentCapture, String>;

#[derive(Default)]
pub struct RenderedDocumentRegistry {
    callers: Mutex<HashMap<String, crate::context_repo::caller_scope::CallerScope>>,
    inner: Mutex<HashMap<String, oneshot::Sender<RenderedDocumentResult>>>,
    seq: AtomicU64,
}

impl RenderedDocumentRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate a unique request id, store its sender, and return
    /// `(request_id, receiver)`. The id is process-monotonic — unique without
    /// pulling in a uuid dependency.
    pub fn register(&self) -> (String, oneshot::Receiver<RenderedDocumentResult>) {
        self.register_for(Default::default())
    }
    pub(crate) fn register_for(
        &self,
        caller: crate::context_repo::caller_scope::CallerScope,
    ) -> (String, oneshot::Receiver<RenderedDocumentResult>) {
        let id = format!(
            "rendered-document-{}",
            self.seq.fetch_add(1, Ordering::Relaxed)
        );
        let (tx, rx) = oneshot::channel();
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), tx);
        self.callers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), caller);
        (id, rx)
    }

    /// Remove and return the sender for `request_id`, if still pending.
    /// Consume-once: a second resolve (e.g. a multi-window `emit` broadcast,
    /// or a late reply after the verb already timed out) finds nothing and is
    /// a no-op — first reply wins.
    pub fn take(&self, request_id: &str) -> Option<oneshot::Sender<RenderedDocumentResult>> {
        self.callers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(request_id);
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(request_id)
    }
}

/// Check the trusted open frame before asking it for any DOM; no refresh or re-render.
#[tauri::command]
pub fn admit_rendered_document<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    registry: tauri::State<'_, RenderedDocumentRegistry>,
    request_id: String,
    frame_id: Option<String>,
) -> Result<(), String> {
    admit_rendered_document_for(&app, &registry, &request_id, frame_id.as_deref())
}

pub(crate) fn admit_rendered_document_for<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    registry: &RenderedDocumentRegistry,
    request_id: &str,
    frame_id: Option<&str>,
) -> Result<(), String> {
    let caller = registry
        .callers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(request_id)
        .cloned();
    let admitted = caller.is_some_and(|caller| {
        if matches!(
            caller,
            crate::context_repo::caller_scope::CallerScope::Human
        ) {
            return true;
        }
        frame_id
            .and_then(|id| crate::context_repo::caller_scope::for_page(app, None, Some(id)).ok())
            .is_some_and(|viewer| caller.admits_viewer(&viewer))
    });
    if admitted {
        return Ok(());
    }
    if let Some(tx) = registry.take(request_id) {
        let _ = tx.send(Err(crate::context_repo::caller_scope::MISSING.into()));
    }
    Err(crate::context_repo::caller_scope::MISSING.into())
}

/// Resolve one rendered-DOM read. Late replies are harmless; enforce the UTF-8
/// output cap again at the native boundary, independently of the iframe shim.
#[tauri::command]
pub fn resolve_rendered_document(
    registry: tauri::State<'_, RenderedDocumentRegistry>,
    request_id: String,
    capture: Option<RenderedDocumentCapture>,
    error: Option<String>,
) -> Result<(), String> {
    if let Some(tx) = registry.take(&request_id) {
        let result: RenderedDocumentResult = match (capture, error) {
            (Some(mut capture), None) => {
                capture.bound_content();
                Ok(capture)
            }
            (_, Some(e)) => Err(e),
            (None, None) => Err("capture resolved with neither content nor an error".into()),
        };
        let _ = tx.send(result);
    }
    Ok(())
}

pub struct RenderedDocumentPending<'a> {
    pub registry: &'a RenderedDocumentRegistry,
    pub request_id: String,
}
impl Drop for RenderedDocumentPending<'_> {
    fn drop(&mut self) {
        self.registry.take(&self.request_id);
    }
}

/// MCP elicitation's reply model (accept / decline / cancel), plus
/// `handled` for a question the human settled outside it. `cancel` covers every
/// no-wedge exit (dismiss / timeout / session-death), so the agent never wedges
/// and always gets a typed outcome to branch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AskUserAction {
    Accept,
    Decline,
    Cancel,
    /// The human settled the question themselves, outside the ask (e.g. did the
    /// action in the pane), and marked it handled.
    Handled,
}

/// The resolved reply to a blocking `ask_user`. `content` is present only on
/// `accept` (the answer text). Serialized back to the agent as the verb result.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AskUserOutcome {
    pub action: AskUserAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

impl AskUserOutcome {
    pub fn accept(content: String) -> Self {
        Self {
            action: AskUserAction::Accept,
            content: Some(content),
        }
    }
    pub fn decline() -> Self {
        Self {
            action: AskUserAction::Decline,
            content: None,
        }
    }
    pub fn cancel() -> Self {
        Self {
            action: AskUserAction::Cancel,
            content: None,
        }
    }
    pub fn handled() -> Self {
        Self {
            action: AskUserAction::Handled,
            content: None,
        }
    }
}

/// The outcome a UI reply names: `action` is `accept` / `decline` / `cancel` /
/// `handled`, and `content` is the answer text on `accept`.
fn outcome_for(action: &str, content: Option<String>) -> Result<AskUserOutcome, String> {
    match action {
        "accept" => Ok(AskUserOutcome::accept(content.unwrap_or_default())),
        "decline" => Ok(AskUserOutcome::decline()),
        "cancel" => Ok(AskUserOutcome::cancel()),
        "handled" => Ok(AskUserOutcome::handled()),
        other => Err(format!("unknown ask_user action: {other}")),
    }
}

/// One pending blocking `ask_user`: the reply channel plus the `session_id` it
/// belongs to, so the session-death sweep can cancel-by-session.
struct PendingAsk {
    session_id: String,
    owner: Option<crate::commands::pty_host::LocalEffectClaim>,
    tx: oneshot::Sender<AskUserOutcome>,
}

/// Correlated request/response bridge for the blocking `ask_user` MCP
/// verb. Structurally identical to [`CanvasRenderRegistry`] (register a oneshot,
/// emit an event, await the reply), BUT each entry also carries its `session_id`
/// so [`AskUserRegistry::cancel_by_session`] can resolve every pending ask for a
/// session whose liveness drops (a human
/// question's real exits are dismiss + session-death; the bounded timeout is
/// only a last-resort backstop, unlike `render_canvas`'s 6s bound).
#[derive(Default)]
pub struct AskUserRegistry {
    changed: tokio::sync::Notify,
    inner: Mutex<HashMap<String, PendingAsk>>,
    seq: AtomicU64,
}

impl AskUserRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) async fn changed(&self) {
        self.changed.notified().await;
    }

    pub(crate) fn pending_sessions(&self) -> std::collections::BTreeSet<String> {
        self.inner
            .lock()
            .unwrap()
            .values()
            .map(|ask| ask.session_id.clone())
            .collect()
    }

    /// Allocate a unique correlation id for `session_id`, store its sender, and
    /// return `(correlation_id, receiver)`. The id is Rust-minted (the agent
    /// never supplies it) and process-monotonic — unique without a uuid dep.
    pub fn register(&self, session_id: &str) -> (String, oneshot::Receiver<AskUserOutcome>) {
        self.register_owned(session_id, None)
    }

    pub(crate) fn register_owned(
        &self,
        session_id: &str,
        owner: Option<crate::commands::pty_host::LocalEffectClaim>,
    ) -> (String, oneshot::Receiver<AskUserOutcome>) {
        let id = format!("ask-user-{}", self.seq.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).insert(
            id.clone(),
            PendingAsk {
                session_id: session_id.to_string(),
                owner,
                tx,
            },
        );
        self.changed.notify_one();
        (id, rx)
    }

    pub(crate) fn cancel_owned(
        &self,
        owner: &crate::commands::pty_host::LocalEffectClaim,
    ) -> usize {
        let senders = {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let ids: Vec<_> = guard
                .iter()
                .filter(|(_, ask)| ask.owner.as_ref() == Some(owner))
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter()
                .filter_map(|id| guard.remove(&id).map(|ask| ask.tx))
                .collect::<Vec<_>>()
        };
        let count = senders.len();
        self.changed.notify_one();
        for tx in senders {
            let _ = tx.send(AskUserOutcome::cancel());
        }
        count
    }

    pub(crate) fn transfer_owned(
        &self,
        old: &crate::commands::pty_host::LocalEffectClaim,
        new: &crate::commands::pty_host::LocalEffectClaim,
    ) {
        for ask in self
            .inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .values_mut()
        {
            if ask.owner.as_ref() == Some(old) {
                ask.owner = Some(new.clone());
            }
        }
    }

    /// Remove and return the sender for `correlation_id` (consume-once — a late
    /// or duplicate resolve finds nothing and is a no-op, first reply wins).
    pub fn take(&self, correlation_id: &str) -> Option<oneshot::Sender<AskUserOutcome>> {
        let sender = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(correlation_id)
            .map(|p| p.tx);
        if sender.is_some() {
            self.changed.notify_one();
        }
        sender
    }

    /// Session-death sweep: resolve every pending ask for `session_id` with
    /// a typed `cancel`, so the agent isn't wedged for the full timeout window
    /// after its own session dies. Returns how many were swept. Driven DRY from
    /// the existing PTY teardown path — no new poller, no new command.
    pub fn cancel_by_session(&self, session_id: &str) -> usize {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let dead: Vec<String> = guard
            .iter()
            .filter(|(_, p)| p.session_id == session_id)
            .map(|(id, _)| id.clone())
            .collect();
        let mut swept = 0;
        for id in dead {
            if let Some(p) = guard.remove(&id) {
                let _ = p.tx.send(AskUserOutcome::cancel());
                swept += 1;
            }
        }
        if swept > 0 {
            self.changed.notify_one();
        }
        swept
    }
}

/// Tauri command: the frontend answer bridge resolves a blocking `ask_user`.
/// `action` is one of `accept` / `decline` / `cancel` / `handled`; `content`
/// carries the answer text on `accept`. The receiver may already be gone (the verb timed
/// out or the session was swept), in which case the send is silently dropped.
#[tauri::command]
pub fn resolve_ask_user(
    registry: tauri::State<'_, AskUserRegistry>,
    correlation_id: String,
    action: String,
    content: Option<String>,
) -> Result<(), String> {
    let outcome = outcome_for(&action, content)?;
    if let Some(tx) = registry.take(&correlation_id) {
        let _ = tx.send(outcome);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn retired_renderer_cannot_publish_viewer_mirror() {
        let home = tempfile::tempdir().unwrap();
        for id in ["41", "42"] {
            std::fs::create_dir_all(home.path().join("users").join(id)).unwrap();
        }
        let profile = crate::commands::local_profile::LocalProfileState::load(home.path()).unwrap();
        let state = McpViewerState::default();
        profile.select("41", None).unwrap();
        publish_viewer_state(
            &profile,
            &state,
            Some("41"),
            1,
            json!({"activeWorkspace":"A"}),
        )
        .unwrap();
        profile.select("42", None).unwrap();
        publish_viewer_state(
            &profile,
            &state,
            Some("42"),
            2,
            json!({"activeWorkspace":"B"}),
        )
        .unwrap();
        for expected in [Some("41"), Some("042"), None] {
            assert!(publish_viewer_state(
                &profile,
                &state,
                expected,
                3,
                json!({"activeWorkspace":"stale"})
            )
            .is_err());
            assert_eq!(state.get(), json!({"activeWorkspace":"B"}));
        }
    }

    #[test]
    fn cp3_older_viewer_snapshot_cannot_replace_newer_dispatch() {
        let state = McpViewerState::default();
        // Production allocates both sequences synchronously at dispatch, then
        // the older command may finish publishing after the newer one.
        let older_sequence = state.next_snapshot_sequence();
        let newer_sequence = state.next_snapshot_sequence();
        state.update_snapshot(newer_sequence, json!({"workspaces":["new"]}));
        state.update_snapshot(older_sequence, json!({"workspaces":["old"]}));
        assert_eq!(state.get(), json!({"workspaces":["new"]}));
    }

    #[test]
    fn test_update_and_get_round_trip() {
        let state = McpViewerState::default();
        let value = json!({"openedTeams": ["acme:default"], "settings": {"theme": "dark"}});
        state.update(value.clone());
        assert_eq!(state.get(), value);
    }

    // ActiveTeamState behaviors.

    #[test]
    fn active_context_state_is_none_until_first_update() {
        let state = ActiveTeamState::default();
        assert!(state.get().is_none());
    }

    #[test]
    fn active_context_state_round_trips() {
        let state = ActiveTeamState::default();
        let ctx = ActiveTeam {
            org_slug: "acme/api".into(),
            team_name: "platform".into(),
        };
        state.update(ctx.clone());
        assert_eq!(state.get(), Some(ctx));
    }

    #[test]
    fn active_context_state_update_overwrites_previous() {
        let state = ActiveTeamState::default();
        state.update(ActiveTeam {
            org_slug: "acme/api".into(),
            team_name: "team-a".into(),
        });
        state.update(ActiveTeam {
            org_slug: "acme/api".into(),
            team_name: "team-b".into(),
        });
        let after = state.get().expect("populated");
        assert_eq!(after.team_name, "team-b");
    }

    #[test]
    fn active_context_state_concurrent_writes_do_not_panic() {
        use std::sync::Arc;
        use std::thread;

        let state = Arc::new(ActiveTeamState::default());
        let mut handles = vec![];
        for i in 0..10 {
            let s = Arc::clone(&state);
            handles.push(thread::spawn(move || {
                s.update(ActiveTeam {
                    org_slug: "acme/api".into(),
                    team_name: format!("team-{}", i),
                });
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        // Exact team_name nondeterministic, but get() must yield Some(_).
        assert!(state.get().is_some());
    }

    #[test]
    fn rendered_document_native_cap_preserves_utf8_and_truncation() {
        let mut capture = RenderedDocumentCapture {
            content: "€".repeat(RENDERED_DOCUMENT_MAX_BYTES / 3 + 1),
            captured_at: "2026-10-02T12:00:00Z".into(),
            truncated: false,
        };
        capture.bound_content();
        assert_eq!(capture.content.len(), RENDERED_DOCUMENT_MAX_BYTES - 2);
        assert!(capture.truncated);
        assert!(!capture.content.contains('\u{fffd}'));
        capture.bound_content();
        assert!(
            capture.truncated,
            "do not erase iframe truncation on a bounded reply"
        );
        capture.content = "small".into();
        capture.truncated = false;
        capture.bound_content();
        assert_eq!(capture.content, "small");
        assert!(!capture.truncated);
    }

    #[tokio::test]
    async fn rendered_document_registry_correlates_and_cleans_cancelled_requests() {
        let registry = RenderedDocumentRegistry::new();
        let (id, rx) = registry.register();
        let (other, other_rx) = registry.register();
        assert_ne!(id, other);
        registry
            .take(&id)
            .unwrap()
            .send(Err("closed".into()))
            .unwrap();
        assert!(registry.take(&id).is_none());
        assert_eq!(rx.await.unwrap().unwrap_err(), "closed");
        drop(RenderedDocumentPending {
            registry: &registry,
            request_id: other.clone(),
        });
        assert!(registry.take(&other).is_none());
        assert!(other_rx.await.is_err());
    }

    // CanvasRenderRegistry behaviors.

    #[test]
    fn canvas_render_registry_ids_are_unique() {
        let reg = CanvasRenderRegistry::new();
        let (id1, _rx1) = reg.register();
        let (id2, _rx2) = reg.register();
        assert_ne!(id1, id2, "each register() must mint a fresh id");
    }

    #[test]
    fn canvas_render_registry_routes_result_to_receiver() {
        let reg = CanvasRenderRegistry::new();
        let (id, mut rx) = reg.register();
        let tx = reg.take(&id).expect("sender present until taken");
        tx.send(Ok("/ctx/team/design.excalidraw.png".to_string()))
            .expect("receiver still live");
        assert_eq!(
            rx.try_recv().expect("result delivered"),
            Ok("/ctx/team/design.excalidraw.png".to_string())
        );
    }

    #[test]
    fn canvas_render_registry_take_is_consume_once() {
        let reg = CanvasRenderRegistry::new();
        let (id, _rx) = reg.register();
        assert!(reg.take(&id).is_some(), "first take yields the sender");
        assert!(
            reg.take(&id).is_none(),
            "second take (multi-window / late reply) is a no-op"
        );
    }

    #[test]
    fn canvas_render_registry_take_unknown_id_is_none() {
        let reg = CanvasRenderRegistry::new();
        assert!(reg.take("canvas-render-999").is_none());
    }

    // AskUserRegistry behaviors.

    #[test]
    fn ask_user_registry_ids_are_unique() {
        let reg = AskUserRegistry::new();
        let (id1, _rx1) = reg.register("sess-a");
        let (id2, _rx2) = reg.register("sess-a");
        assert_ne!(id1, id2, "each register() must mint a fresh correlation id");
    }

    #[test]
    fn ask_user_registry_routes_accept_to_receiver() {
        let reg = AskUserRegistry::new();
        let (id, mut rx) = reg.register("sess-a");
        let tx = reg.take(&id).expect("sender present until taken");
        tx.send(AskUserOutcome::accept("option B".into()))
            .expect("receiver still live");
        assert_eq!(
            rx.try_recv().expect("outcome delivered"),
            AskUserOutcome::accept("option B".into())
        );
    }

    #[test]
    fn ask_user_registry_take_is_consume_once() {
        let reg = AskUserRegistry::new();
        let (id, _rx) = reg.register("sess-a");
        assert!(reg.take(&id).is_some(), "first take yields the sender");
        assert!(
            reg.take(&id).is_none(),
            "second take is a no-op — first reply wins"
        );
    }

    #[test]
    fn ask_user_registry_concurrent_correlations_resolve_independently() {
        let reg = AskUserRegistry::new();
        let (id1, mut rx1) = reg.register("sess-a");
        let (id2, mut rx2) = reg.register("sess-b");
        // Resolve out of order — each correlation id routes to its own receiver.
        reg.take(&id2)
            .unwrap()
            .send(AskUserOutcome::decline())
            .unwrap();
        reg.take(&id1)
            .unwrap()
            .send(AskUserOutcome::accept("yes".into()))
            .unwrap();
        assert_eq!(
            rx1.try_recv().unwrap(),
            AskUserOutcome::accept("yes".into())
        );
        assert_eq!(rx2.try_recv().unwrap(), AskUserOutcome::decline());
    }

    #[test]
    fn ask_user_registry_cancel_by_session_sweeps_only_that_session() {
        let reg = AskUserRegistry::new();
        let (id_a1, mut rx_a1) = reg.register("sess-a");
        let (id_a2, mut rx_a2) = reg.register("sess-a");
        let (id_b, mut rx_b) = reg.register("sess-b");

        let swept = reg.cancel_by_session("sess-a");
        assert_eq!(swept, 2, "both sess-a asks swept");

        // sess-a receivers got a typed cancel...
        assert_eq!(rx_a1.try_recv().unwrap(), AskUserOutcome::cancel());
        assert_eq!(rx_a2.try_recv().unwrap(), AskUserOutcome::cancel());
        // ...and their slots are gone.
        assert!(reg.take(&id_a1).is_none());
        assert!(reg.take(&id_a2).is_none());
        // sess-b is untouched and still resolvable.
        assert!(rx_b.try_recv().is_err(), "sess-b not yet resolved");
        assert!(reg.take(&id_b).is_some(), "sess-b slot still pending");
    }

    #[test]
    fn ask_user_registry_cancel_by_unknown_session_is_zero() {
        let reg = AskUserRegistry::new();
        let (_id, _rx) = reg.register("sess-a");
        assert_eq!(reg.cancel_by_session("ghost"), 0);
    }

    #[test]
    fn resolve_ask_user_outcome_constructors() {
        assert_eq!(
            AskUserOutcome::accept("x".into()),
            AskUserOutcome {
                action: AskUserAction::Accept,
                content: Some("x".into())
            }
        );
        assert_eq!(AskUserOutcome::decline().content, None);
        assert_eq!(AskUserOutcome::cancel().action, AskUserAction::Cancel);
        assert_eq!(AskUserOutcome::handled().action, AskUserAction::Handled);
    }

    #[test]
    fn a_ui_reply_can_mark_the_question_handled() {
        assert_eq!(outcome_for("handled", None), Ok(AskUserOutcome::handled()));
        assert_eq!(
            serde_json::to_value(AskUserOutcome::handled()).unwrap(),
            json!({"action": "handled"})
        );
        assert_eq!(
            outcome_for("accept", Some("yes".into())),
            Ok(AskUserOutcome::accept("yes".into()))
        );
        assert!(outcome_for("shrug", None).is_err());
    }

    #[test]
    fn ask_user_outcome_serializes_camel_and_omits_none_content() {
        // accept → {"action":"accept","content":"..."}
        let accept = serde_json::to_value(AskUserOutcome::accept("hi".into())).unwrap();
        assert_eq!(accept, json!({"action": "accept", "content": "hi"}));
        // cancel → {"action":"cancel"} (content omitted, not null)
        let cancel = serde_json::to_value(AskUserOutcome::cancel()).unwrap();
        assert_eq!(cancel, json!({"action": "cancel"}));
    }

    #[test]
    fn test_default_state_is_null() {
        let state = McpViewerState::default();
        assert_eq!(state.get(), serde_json::Value::Null);
    }

    #[test]
    fn test_update_overwrites_previous() {
        let state = McpViewerState::default();
        state.update(json!({"a": 1}));
        state.update(json!({"b": 2}));
        assert_eq!(state.get(), json!({"b": 2}));
    }

    #[test]
    fn test_merge_patch_adds_new_keys() {
        let state = McpViewerState::default();
        state.update(json!({"a": 1}));

        fn merge(base: &mut serde_json::Value, patch: &serde_json::Value) {
            if let (Some(b), Some(p)) = (base.as_object_mut(), patch.as_object()) {
                for (k, v) in p {
                    b.insert(k.clone(), v.clone());
                }
            }
        }

        let result = state.merge_patch(&json!({"b": 2}), merge);
        assert_eq!(result, json!({"a": 1, "b": 2}));
        // Verify state was persisted
        assert_eq!(state.get(), json!({"a": 1, "b": 2}));
    }

    #[test]
    fn test_merge_patch_overwrites_existing_keys() {
        let state = McpViewerState::default();
        state.update(json!({"theme": "dark"}));

        fn merge(base: &mut serde_json::Value, patch: &serde_json::Value) {
            if let (Some(b), Some(p)) = (base.as_object_mut(), patch.as_object()) {
                for (k, v) in p {
                    b.insert(k.clone(), v.clone());
                }
            }
        }

        state.merge_patch(&json!({"theme": "light"}), merge);
        assert_eq!(state.get()["theme"], json!("light"));
    }

    #[test]
    fn test_merge_patch_returns_merged_state() {
        let state = McpViewerState::default();
        state.update(json!({"x": 1}));

        fn merge(base: &mut serde_json::Value, patch: &serde_json::Value) {
            *base = patch.clone();
        }

        let result = state.merge_patch(&json!({"y": 2}), merge);
        assert_eq!(result, json!({"y": 2}));
    }

    #[test]
    fn test_concurrent_access_does_not_panic() {
        use std::sync::Arc;
        use std::thread;

        let state = Arc::new(McpViewerState::default());
        let mut handles = vec![];

        for i in 0..10 {
            let s = Arc::clone(&state);
            handles.push(thread::spawn(move || {
                s.update(json!({"iteration": i}));
                let _ = s.get();
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        // Should have one of the values, not panic
        let val = state.get();
        assert!(val.is_object());
    }

    #[test]
    fn test_update_accepts_object() {
        let state = McpViewerState::default();
        state.update(json!({"key": "value"}));
        assert_eq!(state.get()["key"], json!("value"));
    }

    #[test]
    fn test_update_accepts_nested_objects() {
        let state = McpViewerState::default();
        let nested = json!({
            "settings": {"theme": "dark", "font": 14},
            "tabs": [{"name": "file.md", "path": "/tmp/file.md"}]
        });
        state.update(nested.clone());
        assert_eq!(state.get(), nested);
    }

    #[test]
    fn test_get_clones_value() {
        let state = McpViewerState::default();
        state.update(json!({"a": 1}));

        let val1 = state.get();
        let val2 = state.get();
        // Both should be equal but independent clones
        assert_eq!(val1, val2);

        // Mutating one doesn't affect the state
        // (this is a compile-time guarantee since get() returns owned Value)
    }

    #[test]
    fn test_merge_patch_on_null_state() {
        let state = McpViewerState::default();
        // Default state is Null — merge_patch should still work

        fn replace(base: &mut serde_json::Value, patch: &serde_json::Value) {
            *base = patch.clone();
        }

        let result = state.merge_patch(&json!({"new": true}), replace);
        assert_eq!(result, json!({"new": true}));
        assert_eq!(state.get(), json!({"new": true}));
    }

    #[test]
    fn test_concurrent_merge_patch() {
        use std::sync::Arc;
        use std::thread;

        let state = Arc::new(McpViewerState::default());
        state.update(json!({"counter": 0}));

        fn merge(base: &mut serde_json::Value, patch: &serde_json::Value) {
            if let (Some(b), Some(p)) = (base.as_object_mut(), patch.as_object()) {
                for (k, v) in p {
                    b.insert(k.clone(), v.clone());
                }
            }
        }

        let mut handles = vec![];
        for i in 0..10 {
            let s = Arc::clone(&state);
            handles.push(thread::spawn(move || {
                s.merge_patch(&json!({format!("key_{}", i): i}), merge);
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        let val = state.get();
        let obj = val.as_object().unwrap();
        // Should have counter + all 10 keys (11 total)
        assert_eq!(obj.len(), 11);
    }

    /// Retiring a story's rows leaves the viewer holding neither its open row
    /// nor its minimized shadow, drops the team's active pointer to it, and
    /// touches no other team or row.
    #[test]
    fn retiring_a_story_removes_its_open_and_minimized_rows_only() {
        let story =
            |id: &str, story_id: &str| json!({ "id": id, "kind": "story", "storyId": story_id });
        let mut viewer = json!({
            "workspaces": {
                "acme:core": [
                    story("sw-fx-123-slug", "SW-FX-123"),
                    story("sw-other", "SW-OTHER"),
                ],
                "acme:infra": [story("sw-fx-123-slug", "SW-FX-123")],
            },
            "softClosedWorkspaces": {
                "acme:core": {
                    "sw-fx-min": { "workspace": story("sw-fx-min", "SW-FX-123") },
                },
            },
            "activeWorkspace": { "acme:core": "sw-fx-123-slug", "acme:infra": "sw-fx-123-slug" },
        });
        let matches = |row: &serde_json::Value| {
            row.get("storyId").and_then(|v| v.as_str()) == Some("SW-FX-123")
        };
        let dropped = retire_workspace_rows(&mut viewer, "acme:core", &matches);
        assert_eq!(dropped, 2);
        assert_eq!(
            viewer["workspaces"]["acme:core"],
            json!([story("sw-other", "SW-OTHER")])
        );
        assert_eq!(viewer["softClosedWorkspaces"]["acme:core"], json!({}));
        assert!(viewer["activeWorkspace"].get("acme:core").is_none());
        // The other team keeps its row and its active pointer.
        assert_eq!(
            viewer["workspaces"]["acme:infra"],
            json!([story("sw-fx-123-slug", "SW-FX-123")])
        );
        assert_eq!(viewer["activeWorkspace"]["acme:infra"], "sw-fx-123-slug");
        // Nothing to drop: a second retire is a no-op.
        assert_eq!(retire_workspace_rows(&mut viewer, "acme:core", &matches), 0);
    }
}
