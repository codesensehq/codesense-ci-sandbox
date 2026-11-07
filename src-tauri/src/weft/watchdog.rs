// SPDX-License-Identifier: MIT
//! SW-WV-A4 — the silence watchdog: a background task, armed at worker dispatch,
//! that flips a worker's roster row to `needs-attention` if it goes quiet past its
//! TTL WITHOUT calling `signal_done`. So a crashed-before-signal worker surfaces
//! itself — silence is never treated as a confident finish (the runtime records the
//! attention row on the worker's behalf).
//!
//! # Off-thread invariant (AC-8)
//!
//! Every blocking I/O the watchdog performs — the `wv_roster` status write and the
//! `wv_upsert_attention` append — runs inside `await_blocking` (off the async
//! reactor). The Tauri `emit` is non-blocking by definition. The task itself parks on
//! [`tokio::time::sleep`] vs a [`tokio::sync::watch`] cancel receiver via `select!` —
//! NO `std::thread::sleep`, NO busy loop. Tests advance the mock clock with
//! [`tokio::time::advance`], never a real sleep.
//!
//! # Cancellation (AC-6)
//!
//! The watchdog is cancelled the moment `signal_done` fires, via the A3 ordered
//! `signal_done` hook registry: A4 registers ONE `watchdog-cancel` hook that calls
//! [`WeftWatchdogRegistry::cancel`], which sends `true` on the row's
//! [`watch::Sender`] and hands back the [`JoinHandle`] so the caller awaits a clean
//! exit. On the cancel path NO roster write and NO attention row happen.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;

/// The production silence TTL (SW-WV-A4, AC-5): a dispatched worker quiet this long
/// with no `signal_done` is flagged `needs-attention`. Injectable per-call via the
/// `ttl` argument on [`arm_silence_watchdog`] so tests supply a short duration
/// instead of sleeping 300 s.
pub const WORKER_SILENCE_TTL_S: u64 = 300;

/// The three effects the silence watchdog performs on a timeout (AC-5), behind a
/// trait so the task is testable WITHOUT a Tauri `MockRuntime` (none exists in this
/// repo — DW-WV-E1-1). Production wires this to an `AppHandle` + the team's
/// `WeftStore` path ([`AppHandleEffects`]); tests capture into shared state backed by
/// a real temp `WeftStore` so AC-5's roster/attention rows are genuinely written.
///
/// Every method is `async` and MUST run its store I/O inside `await_blocking` (AC-8).
pub trait WatchdogEffects: Send + Sync + 'static {
    /// Flip the worker on `session_id` `dispatch → needs-attention` in `wv_roster`
    /// (via the B1 single-writer store handler — never raw SQL). Returns the roster
    /// `name` on a win, `None` if the worker already advanced past `dispatch` (it
    /// signalled between the TTL and this write — no flag, AC-6). Off-thread.
    fn flip_roster_needs_attention(
        &self,
        session_id: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Option<String>, String>> + Send>>;

    /// Emit `mcp:agent-done { sessionId, status: "needs-attention",
    /// summary: "worker-silence-watchdog" }` on the app handle. Non-blocking.
    fn emit_agent_done(&self, session_id: &str);

    /// Append a `wv_attention` row via the B3-owned `wv_upsert_attention` handler
    /// (never raw SQL): `{ state: "needs-you", summary: "worker-silence-watchdog:
    /// <session_id>", source: "watchdog", ref: <roster_name> }`. Off-thread.
    fn upsert_attention(
        &self,
        session_id: String,
        roster_ref: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;
}

/// The `mcp:agent-done` `summary` the watchdog stamps (AC-5) — the stable token the
/// FE and audits key on to tell a silence-flag from a real worker verdict.
pub const WATCHDOG_SUMMARY: &str = "worker-silence-watchdog";

/// Arm the silence watchdog for one dispatched worker (SW-WV-A4, AC-5). Spawns a task
/// that `select!`s [`tokio::time::sleep(ttl)`] against the `cancel` receiver flipping
/// to `true`:
/// - **cancel wins** (`signal_done` fired): the task exits cleanly, NO roster write,
///   NO attention row (AC-6).
/// - **TTL wins** (silence): flip the roster to `needs-attention` (a no-op if the
///   worker already signalled — the store CAS returns `None`); on a real flip, emit
///   `mcp:agent-done` and append the `wv_attention` row (AC-5).
///
/// `ttl` is injectable so tests pass `Duration::from_millis(1)` under
/// `#[tokio::test(start_paused = true)]` + [`tokio::time::advance`], never a real
/// sleep (AC-8). All store I/O rides `await_blocking` inside `effects` (AC-8).
pub fn arm_silence_watchdog<E: WatchdogEffects>(
    effects: Arc<E>,
    session_id: String,
    mut cancel: watch::Receiver<bool>,
    ttl: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        // If the cancel already reads `true` (signal_done landed before we armed),
        // never flag — the worker finished.
        if *cancel.borrow() {
            return;
        }
        tokio::select! {
            _ = tokio::time::sleep(ttl) => {
                // Silence past the TTL. Flip the roster; the store CAS is the
                // race belt — a worker that signalled between the sleep firing and
                // this write already left `dispatch`, so `flip` returns None and we
                // emit/append nothing (AC-6).
                match effects.flip_roster_needs_attention(session_id.clone()).await {
                    Ok(Some(roster_ref)) => {
                        effects.emit_agent_done(&session_id);
                        if let Err(e) = effects
                            .upsert_attention(session_id.clone(), roster_ref)
                            .await
                        {
                            log::warn!(
                                target: "weft",
                                "silence watchdog attention-append failed for {session_id}: {e}"
                            );
                        }
                    }
                    Ok(None) => {
                        // The worker signalled first — nothing to flag.
                    }
                    Err(e) => {
                        log::warn!(
                            target: "weft",
                            "silence watchdog roster-flip failed for {session_id}: {e}"
                        );
                    }
                }
            }
            _ = cancel.changed() => {
                // signal_done cancelled us before the TTL — clean exit, no writes.
            }
        }
    })
}

/// The per-team registry of live silence watchdogs (SW-WV-A4, Task 4). Managed Tauri
/// state. Maps a worker `session_id` to its `(cancel sender, task handle)` so the A3
/// `watchdog-cancel` hook can cancel-and-await on `signal_done`.
#[derive(Clone, Default)]
#[allow(clippy::type_complexity)]
pub struct WeftWatchdogRegistry(Arc<Mutex<HashMap<String, (watch::Sender<bool>, JoinHandle<()>)>>>);

impl std::fmt::Debug for WeftWatchdogRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.0.lock().map(|m| m.len()).unwrap_or(0);
        f.debug_struct("WeftWatchdogRegistry")
            .field("live", &n)
            .finish()
    }
}

impl WeftWatchdogRegistry {
    /// A fresh empty registry.
    pub fn new() -> Self {
        WeftWatchdogRegistry(Arc::new(Mutex::new(HashMap::new())))
    }

    /// Store a live watchdog's `(cancel sender, handle)` under its `session_id`. A
    /// re-register on the same session replaces the entry (the prior handle is
    /// dropped — the caller armed a fresh watchdog).
    pub fn register(&self, session_id: String, tx: watch::Sender<bool>, handle: JoinHandle<()>) {
        if let Ok(mut map) = self.0.lock() {
            map.insert(session_id, (tx, handle));
        }
    }

    /// Cancel the watchdog for `session_id` (AC-6): remove the entry, send `true` on
    /// its cancel sender, and return the [`JoinHandle`] so the caller can `await` a
    /// clean task exit. Returns `None` for an unknown session (a bare shell or a
    /// non-Weft worker) — a no-op the `signal_done` hook tolerates.
    pub fn cancel(&self, session_id: &str) -> Option<JoinHandle<()>> {
        let (tx, handle) = {
            let mut map = self.0.lock().ok()?;
            map.remove(session_id)?
        };
        // Best-effort: a dropped receiver (task already exited) is fine.
        let _ = tx.send(true);
        Some(handle)
    }

    /// Number of live watchdogs (diagnostics / tests).
    pub fn len(&self) -> usize {
        self.0.lock().map(|m| m.len()).unwrap_or(0)
    }

    /// Whether any watchdog is live.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The production [`WatchdogEffects`]: an `AppHandle` for the emit + a resolved team
/// `WeftStore` path for the two off-thread store writes (SW-WV-A4). The roster flip
/// routes through [`crate::weft::wv_worker_needs_attention_by_session`] and the
/// attention append through the B3-owned [`crate::weft::wv_upsert_attention`] —
/// never raw SQL (§1.6, D3). Both writes run inside `await_blocking`.
pub struct AppHandleEffects {
    pub app: tauri::AppHandle,
    pub store_path: std::path::PathBuf,
}

impl WatchdogEffects for AppHandleEffects {
    fn flip_roster_needs_attention(
        &self,
        session_id: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Option<String>, String>> + Send>>
    {
        let path = self.store_path.clone();
        Box::pin(async move {
            crate::commands::await_blocking("weft.watchdog.flip_roster", move || {
                let store = crate::weft::WeftStore::open(path)
                    .map_err(|e| format!("watchdog store open: {e}"))?;
                crate::weft::wv_worker_needs_attention_by_session(&store, &session_id)
                    .map_err(|e| e.to_string())
            })
            .await
        })
    }

    fn emit_agent_done(&self, session_id: &str) {
        use tauri::Emitter;
        let payload = serde_json::json!({
            "sessionId": session_id,
            "status": crate::weft::WV_ROSTER_NEEDS_ATTENTION_STATUS,
            "summary": WATCHDOG_SUMMARY,
        });
        if let Err(e) = self.app.emit("mcp:agent-done", payload) {
            log::warn!(target: "weft", "silence watchdog mcp:agent-done emit failed: {e}");
        }
    }

    fn upsert_attention(
        &self,
        session_id: String,
        roster_ref: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>> {
        let path = self.store_path.clone();
        Box::pin(async move {
            crate::commands::await_blocking("weft.watchdog.upsert_attention", move || {
                let store = crate::weft::WeftStore::open(path)
                    .map_err(|e| format!("watchdog store open: {e}"))?;
                let summary = format!("{WATCHDOG_SUMMARY}: {session_id}");
                crate::weft::wv_upsert_attention(
                    &store,
                    None,
                    Some("needs-you"),
                    Some(&summary),
                    Some("watchdog"),
                    Some(&roster_ref),
                    None,
                    None,
                )
                .map(|_id| ())
                .map_err(|e| e.to_string())
            })
            .await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::weft::{wv_provision_worker_row, WeftStore};

    /// A test [`WatchdogEffects`] backed by a REAL temp `WeftStore` (so AC-5's
    /// roster/attention rows are genuinely written) plus a captured emit — no Tauri
    /// `MockRuntime` needed (DW-WV-E1-1). The store writes still route through the
    /// same B1/B3 handlers production uses.
    struct TestEffects {
        store_path: std::path::PathBuf,
        emitted: Arc<Mutex<Vec<serde_json::Value>>>,
    }

    impl WatchdogEffects for TestEffects {
        fn flip_roster_needs_attention(
            &self,
            session_id: String,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Option<String>, String>> + Send>,
        > {
            let path = self.store_path.clone();
            Box::pin(async move {
                // RI-2-ALLOW: watchdog test flip — the real handler off the reactor.
                crate::commands::await_blocking("test.watchdog.flip", move || {
                    let store = WeftStore::open(path).map_err(|e| e.to_string())?;
                    crate::weft::wv_worker_needs_attention_by_session(&store, &session_id)
                        .map_err(|e| e.to_string())
                })
                .await
            })
        }

        fn emit_agent_done(&self, session_id: &str) {
            self.emitted.lock().unwrap().push(serde_json::json!({
                "sessionId": session_id,
                "status": crate::weft::WV_ROSTER_NEEDS_ATTENTION_STATUS,
                "summary": WATCHDOG_SUMMARY,
            }));
        }

        fn upsert_attention(
            &self,
            session_id: String,
            roster_ref: String,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>
        {
            let path = self.store_path.clone();
            Box::pin(async move {
                // RI-2-ALLOW: watchdog test append — the real B3 handler off the reactor.
                crate::commands::await_blocking("test.watchdog.attention", move || {
                    let store = WeftStore::open(path).map_err(|e| e.to_string())?;
                    let summary = format!("{WATCHDOG_SUMMARY}: {session_id}");
                    crate::weft::wv_upsert_attention(
                        &store,
                        None,
                        Some("needs-you"),
                        Some(&summary),
                        Some("watchdog"),
                        Some(&roster_ref),
                        None,
                        None,
                    )
                    .map(|_| ())
                    .map_err(|e| e.to_string())
                })
                .await
            })
        }
    }

    // Seed a temp store with one dispatched worker (provision → dispatch), returning
    // (store_path, session_id, roster_name).
    fn seed_dispatched_worker() -> (tempfile::TempDir, std::path::PathBuf, String, String) {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("team").join("weft.sqlite3");
        let store = WeftStore::open(path.clone()).unwrap();
        let name = "SW-WV-A4".to_string();
        let session_id = "sess-A4".to_string();
        wv_provision_worker_row(&store, &name, "impl", "epic:wv").unwrap();
        let r = crate::weft::wv_dispatch_worker_row(&store, &name, &session_id, None).unwrap();
        assert_eq!(r, crate::weft::MutateResult::Updated);
        (tmp, path, session_id, name)
    }

    fn roster_status(path: &std::path::Path, session_id: &str) -> Option<String> {
        let store = WeftStore::open(path.to_path_buf()).unwrap();
        let conn = store.conn().unwrap();
        conn.query_row(
            "SELECT status FROM wv_roster WHERE session_id=?1",
            rusqlite::params![session_id],
            |r| r.get(0),
        )
        .ok()
    }

    fn attention_summaries(path: &std::path::Path) -> Vec<String> {
        let store = WeftStore::open(path.to_path_buf()).unwrap();
        let conn = store.conn().unwrap();
        let mut stmt = conn
            .prepare("SELECT summary FROM wv_attention ORDER BY id")
            .unwrap();
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        rows
    }

    // ---- AC-5: silence timeout flips roster + emits event + appends attention ----

    #[tokio::test(start_paused = true)]
    async fn watchdog_silence_timeout_writes_roster_and_emits_event() {
        let (_tmp, path, session_id, name) = seed_dispatched_worker();
        let emitted = Arc::new(Mutex::new(Vec::new()));
        let effects = Arc::new(TestEffects {
            store_path: path.clone(),
            emitted: emitted.clone(),
        });
        let (_cancel_tx, cancel_rx) = watch::channel(false);

        // Arm with an injected 1 ms TTL; do NOT cancel — advance the mock clock past
        // the TTL so the sleep fires deterministically (no real wall time).
        let handle = arm_silence_watchdog(
            effects,
            session_id.clone(),
            cancel_rx,
            Duration::from_millis(1),
        );
        tokio::time::advance(Duration::from_millis(2)).await;
        handle.await.unwrap();

        // (a) roster flipped to needs-attention.
        assert_eq!(
            roster_status(&path, &session_id).as_deref(),
            Some(crate::weft::WV_ROSTER_NEEDS_ATTENTION_STATUS)
        );
        // (b) the captured mcp:agent-done payload carries the status + summary.
        let events = emitted.lock().unwrap();
        assert_eq!(events.len(), 1, "exactly one agent-done emit");
        assert_eq!(events[0]["status"], "needs-attention");
        assert_eq!(events[0]["summary"], WATCHDOG_SUMMARY);
        assert_eq!(events[0]["sessionId"], session_id);
        drop(events);
        // (c) a wv_attention row with a non-empty summary was appended.
        let summaries = attention_summaries(&path);
        assert_eq!(summaries.len(), 1);
        assert!(!summaries[0].is_empty());
        assert!(summaries[0].contains(WATCHDOG_SUMMARY));
        assert!(summaries[0].contains(&session_id));
        // sanity: the roster name is the attention ref (not asserted on above but the
        // provision seeded it).
        assert_eq!(name, "SW-WV-A4");
    }

    // ---- AC-6: cancel before timeout writes nothing, no leaked future ----

    #[tokio::test]
    async fn watchdog_cancel_before_timeout_writes_nothing() {
        let (_tmp, path, session_id, _name) = seed_dispatched_worker();
        let emitted = Arc::new(Mutex::new(Vec::new()));
        let effects = Arc::new(TestEffects {
            store_path: path.clone(),
            emitted: emitted.clone(),
        });
        let (cancel_tx, cancel_rx) = watch::channel(false);

        let registry = WeftWatchdogRegistry::new();
        // A long TTL so the timeout never fires within the test.
        let handle = arm_silence_watchdog(
            effects,
            session_id.clone(),
            cancel_rx,
            Duration::from_secs(60),
        );
        registry.register(session_id.clone(), cancel_tx, handle);
        assert_eq!(registry.len(), 1);

        // Cancel via the registry (the A3 signal_done watchdog-cancel hook path) and
        // await a clean exit.
        let joined = registry
            .cancel(&session_id)
            .expect("a live watchdog was registered");
        joined.await.expect("watchdog task exits cleanly on cancel");

        // The roster stayed `dispatch` — no needs-attention flag.
        assert_eq!(
            roster_status(&path, &session_id).as_deref(),
            Some("dispatch")
        );
        // No emit, no attention row.
        assert!(emitted.lock().unwrap().is_empty());
        assert!(attention_summaries(&path).is_empty());
        // The registry entry was removed by cancel.
        assert!(registry.is_empty());
    }

    /// AC-6: cancelling an unknown session is a no-op (`None`) — the hook tolerates a
    /// bare shell / non-Weft worker completion.
    #[test]
    fn cancel_unknown_session_is_a_noop() {
        let registry = WeftWatchdogRegistry::new();
        assert!(registry.cancel("no-such-session").is_none());
    }

    /// AC-6 belt: even if the TTL fires, a worker that already signalled (roster left
    /// `dispatch`) is never clobbered — the store CAS returns None so no emit/append.
    #[tokio::test(start_paused = true)]
    async fn watchdog_does_not_clobber_a_signalled_worker() {
        let (_tmp, path, session_id, _name) = seed_dispatched_worker();
        // Simulate the worker having signalled: advance the roster past `dispatch`.
        {
            let store = WeftStore::open(path.clone()).unwrap();
            assert!(crate::weft::wv_worker_signal_by_session(&store, &session_id).unwrap());
        }
        let emitted = Arc::new(Mutex::new(Vec::new()));
        let effects = Arc::new(TestEffects {
            store_path: path.clone(),
            emitted: emitted.clone(),
        });
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let handle = arm_silence_watchdog(
            effects,
            session_id.clone(),
            cancel_rx,
            Duration::from_millis(1),
        );
        tokio::time::advance(Duration::from_millis(2)).await;
        handle.await.unwrap();

        // Roster stayed `signal`; no silence flag, no emit, no attention row.
        assert_eq!(roster_status(&path, &session_id).as_deref(), Some("signal"));
        assert!(emitted.lock().unwrap().is_empty());
        assert!(attention_summaries(&path).is_empty());
    }
}
