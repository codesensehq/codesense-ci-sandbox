// SPDX-License-Identifier: MIT
//! The worktree source-file watch commands. The watch itself —
//! registry, recursive root watch, git-ref watch, exclusion filter and 50 ms
//! debounce — lives in `codesense_fs_core::watch`; these wrappers run it on the
//! blocking pool and emit each change as the `worktree-file-change` event.

use std::path::PathBuf;

use codesense_fs_core::watch::{attach, detach, WorktreeFileChange, WorktreeWatchState};
use log::{debug, warn};
use tauri::{Emitter, Manager};

use crate::commands::await_blocking;

/// AC-1/AC-2/AC-6 — attach a lazy `notify` watcher rooted at the workspace's
/// worktree `cwd`; relevant changes emit `worktree-file-change { wsId,
/// changedPath }`. Returns the attach **token** the caller passes back to
/// `detach_worktree_watch` (ordering-safe teardown). Called when the inset opens.
#[tauri::command]
pub async fn lazy_attach_worktree_watch(
    app: tauri::AppHandle,
    ws_id: String,
    cwd: String,
) -> Result<u64, String> {
    // SLW-FX-24: `attach`'s `canonicalize` + recursive kernel-watch
    // registration is heavy on a fresh/large worktree, so run it on the Tokio
    // blocking pool via `await_blocking` — never inline on the async
    // command-dispatch worker (that inline body was the "freeze while it sets
    // up"). Mirrors the twin `lazy_attach_workspace` (`watch.rs:2595`).
    // `WorktreeWatchState` is Tauri-managed (a plain `Mutex` struct, NOT
    // `Arc`-backed), so — exactly like `lazy_attach_impl` — we re-derive it from
    // `app` INSIDE the closure rather than moving a borrowed `State` (not `Send`)
    // across the `spawn_blocking` boundary. The canonicalize→lock ordering inside
    // `attach` is untouched: the offload wraps the whole body.
    let cwd_for_log = cwd.clone();
    let ws_for_log = ws_id.clone();
    let token = await_blocking("lazy_attach_worktree_watch", move || {
        let state = app.state::<WorktreeWatchState>();
        let cwd_path = PathBuf::from(&cwd);
        let app_for_emit = app.clone();
        let emit = move |change: WorktreeFileChange| {
            if let Err(e) = app_for_emit.emit("worktree-file-change", change) {
                warn!(target: "worktree_watch", "worktree-file-change emit failed: {e}");
            }
        };
        attach(&state, ws_id, &cwd_path, emit)
    })
    .await?;
    debug!(target: "worktree_watch", "attach ws_id={ws_for_log} cwd={cwd_for_log} token={token}");
    Ok(token)
}

/// AC-6 — detach the workspace's worktree watcher (inset close / teardown).
/// Token-matched + leak-free (drops the watcher, stopping the OS watch).
#[tauri::command]
pub async fn detach_worktree_watch(
    app: tauri::AppHandle,
    ws_id: String,
    token: u64,
) -> Result<(), String> {
    // SLW-FX-24: `detach` itself is cheap (lock + drop the watcher), but it
    // is offloaded through the SAME `await_blocking` gateway as attach so the pair
    // is symmetric and the RI-2 backstop covers both — neither half can be
    // silently re-inlined onto the async plane. Re-derive the managed state inside
    // the closure (the borrowed `State` is not `Send`).
    let ws_for_log = ws_id.clone();
    await_blocking("detach_worktree_watch", move || {
        let state = app.state::<WorktreeWatchState>();
        detach(&state, &ws_id, token);
        Ok(())
    })
    .await?;
    debug!(target: "worktree_watch", "detach ws_id={ws_for_log} token={token}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn attach_offloads_through_await_blocking_and_returns_token() {
        // SLW-FX-24 (AC-1): the attach must run on the blocking pool. Drive
        // `attach` through the SAME `await_blocking` wrapper the production
        // command now uses, moving an owned state handle + `PathBuf` + emit sink
        // across the `spawn_blocking` boundary — this compiles ONLY if the whole
        // body is `Send + 'static`, which is exactly the offload contract. Assert
        // it still mints a valid monotonic token and registers one live watch.
        // (Real notify FS-event delivery is intentionally not awaited here — it is
        // timing-nondeterministic per AC-6; the classify/emit contract is covered
        // byte-for-byte by the `codesense_fs_core::watch` tests.)
        let dir = tempdir().unwrap();
        let state = std::sync::Arc::new(WorktreeWatchState::default());
        let state_for_closure = state.clone();
        let cwd = dir.path().to_path_buf();
        let token = crate::commands::await_blocking("lazy_attach_worktree_watch", move || {
            attach(&state_for_closure, "ws-1".into(), &cwd, |_| {})
        })
        .await
        .unwrap();
        assert!(token >= 1, "attach must mint a valid monotonic token");
        assert_eq!(
            state.live_watch_count(),
            1,
            "attach through the blocking pool must register exactly one live watch",
        );
    }
}
