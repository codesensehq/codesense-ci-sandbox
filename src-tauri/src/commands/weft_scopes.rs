// SPDX-License-Identifier: MIT
//! SW-WV-B5 — the Tauri command surface for the scope layer (`wv_scopes` +
//! `wv_watches`).
//!
//! These are thin delegations, identical in shape to `weft_ledger.rs` (SW-WV-B3):
//! each resolves the active team's `weft.sqlite3` handle and calls a sanctioned
//! `weft::` scope/watch handler OFF the async/UI thread (via [`await_blocking`],
//! ARCHITECTURE §1.6, AC-8). No SQL and no `wv_*` write logic lives here — the sole
//! writer stays `src-tauri/src/weft/sqlite.rs` (§1.2, MODULE-HOME §5.3). Every
//! store-touching command body opens + uses the store INSIDE its `await_blocking`
//! closure, so the whole blocking SQLite lifecycle runs off the async/UI thread and
//! `WeftStore::open` never appears on an un-offloaded line.

use std::path::PathBuf;

use crate::commands::await_blocking;
use crate::weft::{self, ScopeRow, WatchRow};

/// SW-WV-B5: resolve the active team's `weft.sqlite3` PATH for a Tauri command —
/// the canonical `resolve_weft_store_path` (§1.1), never a re-derived path.
/// Returns the PATH (pure); each command opens + uses the store INSIDE its
/// `await_blocking` closure (§1.6). Mirrors `weft_ledger::active_weft_store_path`.
async fn active_weft_store_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let ctx = crate::commands::route_to_context::derive_active_context(app).await?;
    let team = ctx
        .team_name
        .as_deref()
        .ok_or_else(|| "no active team selected — pick a team first".to_owned())?;
    weft::resolve_weft_store_path(&ctx.codesense_home, &ctx.principal(), &ctx.org_slug, team)
}

/// SW-WV-B5: open the store from a resolved path inside a blocking closure. Kept
/// tiny so the `WeftStore::open` call is co-located with the blocking write it
/// precedes (never on the async reactor).
fn open_store(path: PathBuf) -> Result<weft::WeftStore, String> {
    // RI-2-ALLOW: only ever called from inside an `await_blocking` closure below
    weft::WeftStore::open(path).map_err(|e| format!("open weft store: {e}"))
}

/// Map a B5 scope/watch handler's error onto the Tauri `String` channel, preserving
/// the human-readable message (validation refusals read cleanly in the UI).
fn wv_err(e: weft::WvWriteError) -> String {
    e.to_string()
}

/// A scope row on the wire (camelCase). Mirrors [`ScopeRow`].
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeRowDto {
    pub scope: String,
    pub parent_scope: Option<String>,
    pub artifact_ptr: Option<String>,
    pub lease_session: Option<String>,
    /// SW-AV-17: who OPENED this scope (opener's session id), decoupled from the lease.
    pub owner_session: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub closed_at: Option<i64>,
}

impl From<ScopeRow> for ScopeRowDto {
    fn from(r: ScopeRow) -> Self {
        Self {
            scope: r.scope,
            parent_scope: r.parent_scope,
            artifact_ptr: r.artifact_ptr,
            lease_session: r.lease_session,
            owner_session: r.owner_session,
            created_at: r.created_at,
            updated_at: r.updated_at,
            closed_at: r.closed_at,
        }
    }
}

/// A watch row on the wire (camelCase). Mirrors [`WatchRow`].
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchRowDto {
    pub scope: String,
    pub kind: String,
    #[serde(rename = "ref")]
    pub reference: String,
    pub lifecycle: String,
    pub updated_at: i64,
}

impl From<WatchRow> for WatchRowDto {
    fn from(r: WatchRow) -> Self {
        Self {
            scope: r.scope,
            kind: r.kind,
            reference: r.reference,
            lifecycle: r.lifecycle,
            updated_at: r.updated_at,
        }
    }
}

/// SW-WV-B5: read a single scope row (the restart contract's query-don't-carry read
/// path — always hits the DB). Returns `None` when the scope is unknown.
#[tauri::command]
pub async fn cmd_wv_scope_get(
    app: tauri::AppHandle,
    scope: String,
) -> Result<Option<ScopeRowDto>, String> {
    let path = active_weft_store_path(&app).await?;
    let row = await_blocking("cmd_wv_scope_get", move || {
        let store = open_store(path)?;
        weft::wv_scope_get(&store, &scope).map_err(wv_err)
    })
    .await?;
    Ok(row.map(ScopeRowDto::from))
}

/// SW-WV-B5: create a scope idempotently. Returns `true` when created, `false` when
/// the scope already existed (no overwrite).
#[tauri::command]
pub async fn cmd_wv_scope_create(
    app: tauri::AppHandle,
    scope: String,
    parent_scope: Option<String>,
    artifact_ptr: Option<String>,
    // SW-AV-17: the opener's session id (who-opened-this-scope), recorded independent
    // of any lease. Optional — a caller that does not know its session records `None`.
    owner_session: Option<String>,
) -> Result<bool, String> {
    let path = active_weft_store_path(&app).await?;
    await_blocking("cmd_wv_scope_create", move || {
        let store = open_store(path)?;
        weft::wv_scope_create(
            &store,
            &scope,
            parent_scope.as_deref(),
            artifact_ptr.as_deref(),
            owner_session.as_deref(),
        )
        .map_err(wv_err)
    })
    .await
}

/// SW-WV-B5: update a scope's `artifact_ptr` (a pointer, never a copy — no
/// filesystem side-effect). Returns `false` on a no-op (unknown scope).
#[tauri::command]
pub async fn cmd_wv_scope_update_artifact_ptr(
    app: tauri::AppHandle,
    scope: String,
    artifact_ptr: String,
) -> Result<bool, String> {
    let path = active_weft_store_path(&app).await?;
    await_blocking("cmd_wv_scope_update_artifact_ptr", move || {
        let store = open_store(path)?;
        weft::wv_scope_update_artifact_ptr(&store, &scope, &artifact_ptr).map_err(wv_err)
    })
    .await
}

/// SW-WV-B5: close a scope (clears the lease cache, stamps `closed_at`; never
/// deletes the row). A double-close is a typed refusal, not a silent success.
/// Returns the `closed_at` epoch-ms stamp.
#[tauri::command]
pub async fn cmd_wv_scope_close(app: tauri::AppHandle, scope: String) -> Result<i64, String> {
    let path = active_weft_store_path(&app).await?;
    await_blocking("cmd_wv_scope_close", move || {
        let store = open_store(path)?;
        weft::wv_scope_close(&store, &scope).map_err(wv_err)
    })
    .await
}

/// SW-WV-B5: upsert a watch on the composite key `(scope, kind, ref)`. `lifecycle`
/// is validated against the enum before the write (unknown value rejected).
#[tauri::command]
pub async fn cmd_wv_watch_upsert(
    app: tauri::AppHandle,
    scope: String,
    kind: String,
    reference: String,
    lifecycle: String,
) -> Result<(), String> {
    let path = active_weft_store_path(&app).await?;
    await_blocking("cmd_wv_watch_upsert", move || {
        let store = open_store(path)?;
        weft::wv_watch_upsert(&store, &scope, &kind, &reference, &lifecycle).map_err(wv_err)
    })
    .await
}

/// SW-WV-B5: close a watch with positive evidence — a `close_reason` is REQUIRED
/// (silence never closes a watch). Returns `false` on a no-op (unknown key).
#[tauri::command]
pub async fn cmd_wv_watch_close(
    app: tauri::AppHandle,
    scope: String,
    kind: String,
    reference: String,
    close_reason: String,
) -> Result<bool, String> {
    let path = active_weft_store_path(&app).await?;
    await_blocking("cmd_wv_watch_close", move || {
        let store = open_store(path)?;
        weft::wv_watch_close(&store, &scope, &kind, &reference, &close_reason).map_err(wv_err)
    })
    .await
}

/// SW-WV-B5: mark a watch `quiet` (idle — slower cadence, never auto-dropped; does
/// NOT set `closed`). Returns `false` on a no-op (unknown key).
#[tauri::command]
pub async fn cmd_wv_watch_mark_quiet(
    app: tauri::AppHandle,
    scope: String,
    kind: String,
    reference: String,
) -> Result<bool, String> {
    let path = active_weft_store_path(&app).await?;
    await_blocking("cmd_wv_watch_mark_quiet", move || {
        let store = open_store(path)?;
        weft::wv_watch_mark_quiet(&store, &scope, &kind, &reference).map_err(wv_err)
    })
    .await
}

/// SW-WV-B5: list a scope's watches (the D1-triage read path). When
/// `include_closed` is false, `closed` watches are filtered out.
#[tauri::command]
pub async fn cmd_wv_watch_list(
    app: tauri::AppHandle,
    scope: String,
    include_closed: bool,
) -> Result<Vec<WatchRowDto>, String> {
    let path = active_weft_store_path(&app).await?;
    let rows = await_blocking("cmd_wv_watch_list", move || {
        let store = open_store(path)?;
        weft::wv_watch_list(&store, &scope, include_closed).map_err(wv_err)
    })
    .await?;
    Ok(rows.into_iter().map(WatchRowDto::from).collect())
}

// ============================================================================
// SW-WV-B5 grep-based #[test]s — kept LOCAL to this command file so they fail at
// compile/test time if the command list changes (story Tasks 7 & 8).
// ============================================================================

/// Sentinel marking the end of production code. The self-scanning grep tests below
/// scan ONLY the source ABOVE this line, so the banned-token literals that appear
/// inside the test assertions cannot false-trip the scan of their own file. All
/// eight command bodies + the helpers sit above it, so the scan still covers every
/// production line.
#[cfg(test)]
const PRODUCTION_CODE_END: &str = "PRODUCTION_CODE_END_SENTINEL";

#[cfg(test)]
mod tests {
    /// The production-code portion of this file — everything before the
    /// [`super::PRODUCTION_CODE_END`] sentinel. Scanning this slice (rather than the
    /// whole file) keeps the grep honest about the real command bodies while the test
    /// module's own banned-token literals stay out of scope.
    fn production_src() -> &'static str {
        let full = include_str!("weft_scopes.rs");
        let marker = super::PRODUCTION_CODE_END;
        // Split at the sentinel's DEFINITION line (the first occurrence is the
        // `const` above; everything after it is test scaffolding).
        match full.find(marker) {
            Some(idx) => &full[..idx],
            None => full,
        }
    }

    /// AC-8 (Task 8): every store-touching command dispatches its DB call via
    /// `await_blocking` (the blocking-pool wrapper), and NONE of the banned
    /// UI-thread-blocking patterns appear in the production code. Walks this file's
    /// own source via `include_str!` so the count stays in sync with the
    /// implementation.
    #[test]
    fn every_store_command_uses_await_blocking_and_no_banned_patterns() {
        let src = production_src();

        // (a) `await_blocking` appears at least 8 times — one per store-touching
        //     command. Count only the invocation form (`await_blocking(`) so the
        //     doc/comment mentions of the name don't inflate the count.
        let calls = src.matches("await_blocking(").count();
        assert!(
            calls >= 8,
            "expected >=8 `await_blocking(` calls (one per store-touching command), found {calls}"
        );

        // (b) No banned UI-thread-blocking pattern appears. `spawn_blocking` is
        //     banned here (use `await_blocking` instead, per the story); a bare
        //     `.lock().unwrap()` outside an `await_blocking` closure would block the
        //     reactor. These are string-scans over the whole file — none of these
        //     tokens legitimately appears in a delegation-only command file.
        for banned in [
            "std::thread::sleep",
            "block_on",
            "spawn_blocking",
            ".lock().unwrap()",
        ] {
            assert!(
                !src.contains(banned),
                "banned UI-thread-blocking pattern {banned:?} must not appear in weft_scopes.rs \
                 (use `await_blocking` for the blocking-pool dispatch, ARCHITECTURE §1.6)"
            );
        }
    }

    /// AC-4 (Task 7): no session-local scope cache. Walks this file's source and
    /// asserts none of the cache-shaped patterns appear — the restart round-trip
    /// (AC-4, in `weft::sqlite::b5_tests`) proves the DB is durable; this grep
    /// proves there is no parallel in-process cache that would hide a cold start.
    #[test]
    fn no_session_local_scope_cache() {
        let src = production_src();
        for banned in ["OnceLock", "Lazy", "static.*Scope"] {
            // The `static.*Scope` "pattern" is checked as its literal component
            // tokens: a `static` item that materializes a scope map. Scan for a
            // `static ` declaration line that also mentions `Scope`.
            if banned == "static.*Scope" {
                for line in src.lines() {
                    let t = line.trim_start();
                    // Skip doc/comment lines so a prose mention can't false-trip.
                    if t.starts_with("//") || t.starts_with("/*") || t.starts_with('*') {
                        continue;
                    }
                    assert!(
                        !(t.starts_with("static ") && line.contains("Scope")),
                        "a `static` holding a Scope map would be a session-local cache: {line}"
                    );
                }
                continue;
            }
            assert!(
                !src.contains(banned),
                "session-local-cache pattern {banned:?} must not appear in weft_scopes.rs \
                 (the store is the sole scope substrate — query-don't-carry, AC-4)"
            );
        }
        // Also forbid a materialized in-process scope map behind a Mutex/HashMap.
        assert!(
            !src.contains("HashMap") || !src.contains("Scope"),
            "a HashMap over scopes would be a session-local cache (AC-4)"
        );
    }
}
