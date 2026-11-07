// SPDX-License-Identifier: MIT
//! SW-AU-80E: the claim-driven dispatcher — the runnable-queue **consumer**.
//!
//! A routine fire is a durable runnable `runs` row with a run-request pointer. This module is the
//! consumer: a single in-process task claims one runnable run at a time (`claim_ready`), hands
//! it to the routine runtime, and retires it (`finish_run`). On an empty queue it **parks** on a [`Notify`] the
//! producers and restart-recovery signal — it never busy-spins a core (AC-1).
//!
//! **Executor-agnostic core (headless-testable).** The loop holds its store operations as
//! closures ([`ClaimLoopDeps`]): `claim` yields the next runnable `run_id`, `execute` drives
//! one run end-to-end, and `finish` retires it. Production wires `execute` to read the
//! pointer and launch the routine run; a test wires a recording closure over a
//! temp-file store, so claim → execute → finish is proven without an `AppHandle` (the
//! `WaveDeps` precedent). Only the production `execute` closure needs the `AppHandle`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use log::warn;
use tauri::AppHandle;
use tokio::sync::Notify;

use crate::coordinator::fault::{self, FaultPoint};
use crate::coordinator::{default_store_path, SqliteOrchestrationStore};

/// A boxed, `Send` future — the store closures are async (production offloads rusqlite to the
/// blocking pool) so the loop can `.await` them uniformly.
type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Claim one runnable run's id, or `None` when the queue is drained. Production wraps
/// `claim_ready` off-thread; a test calls the store directly.
pub type ClaimFn = Arc<dyn Fn() -> BoxFut<Result<Option<String>, String>> + Send + Sync>;

/// Execute a claimed run end-to-end. Production: read the pointer and launch the routine run.
/// A test records the `run_id`. Contract: `Ok(())` = the run reached a
/// **terminal decision** (launched, or un-executable) → retire it; `Err(_)` = a
/// **transient** condition (context not ready) → leave it `claimed` so a later `recover` re-offers
/// it. A run that never returns (a real crash mid-execute) likewise stays `claimed` for `recover`,
/// so an executor must tolerate a re-drive of a run it already started.
pub type ExecuteFn = Arc<dyn Fn(String) -> BoxFut<Result<(), String>> + Send + Sync>;

/// Retire a finished run (`claimed → done` CAS). Production wraps `finish_run` off-thread.
pub type FinishFn = Arc<dyn Fn(String) -> BoxFut<Result<bool, String>> + Send + Sync>;

/// The claim loop's store operations, as closures — the seam that keeps the loop provable
/// headless (mirrors `content_engine::sync::wave::WaveDeps`).
pub struct ClaimLoopDeps {
    pub claim: ClaimFn,
    pub execute: ExecuteFn,
    pub finish: FinishFn,
    pub notify: Arc<Notify>,
}

/// The process-wide wake channel: producers (routine fires) and
/// restart-recovery call [`wake`] after enqueuing/re-offering a runnable row; the loop parks on
/// this handle. A static `OnceLock<Arc<Notify>>` (the `retry_scheduler::wake` idiom) so a producer
/// can signal without threading a handle through every call site.
static WAKE: std::sync::OnceLock<Arc<Notify>> = std::sync::OnceLock::new();

fn wake_handle() -> &'static Arc<Notify> {
    WAKE.get_or_init(|| Arc::new(Notify::new()))
}

/// Signal the claim loop that new runnable work is available. Cheap and non-blocking; if the loop
/// is mid-drain the permit is stored and consumed on its next park, so a wake is never lost.
pub fn wake() {
    wake_handle().notify_one();
}

/// The shared wake channel the loop parks on — the SAME handle [`wake`] signals, so a producer's
/// `wake()` reliably nudges the parked loop. The production spawn uses this for `ClaimLoopDeps::notify`.
pub fn wake_notify() -> Arc<Notify> {
    wake_handle().clone()
}

/// Drain every currently-claimable run, single-flight (await each execution before the next
/// claim), and return the count driven. Returns as soon as `claim` yields `None` (queue empty)
/// or errors (back off — the next wake retries). Test-observable via the return value.
pub async fn drain_runnable(deps: &ClaimLoopDeps) -> usize {
    let mut driven = 0;
    loop {
        match (deps.claim)().await {
            Ok(Some(run_id)) => {
                drive_one(deps, run_id).await;
                driven += 1;
            }
            Ok(None) => return driven,
            Err(e) => {
                warn!(target: "orchestration", "claim-loop claim failed (backing off until next wake): {e}");
                return driven;
            }
        }
    }
}

/// Execute one claimed run, then retire it — unless a fault or an execution error means it must
/// stay `claimed` for `recover` to re-offer.
async fn drive_one(deps: &ClaimLoopDeps, run_id: String) {
    match (deps.execute)(run_id.clone()).await {
        Ok(()) => {
            // Injected crash point (AC-2): the effects executed but the run is not yet retired.
            // Leaving it `claimed` is exactly what a real crash here does — the lease expires,
            // `recover` reclaims it to `runnable`, and the run is re-driven. `should_abort` is a
            // `const false` outside tests, so this compiles away in a shipped build.
            if fault::should_abort(FaultPoint::ClaimPostExecutePreDone) {
                return;
            }
            match (deps.finish)(run_id.clone()).await {
                Ok(true) => {}
                Ok(false) => warn!(
                    target: "orchestration",
                    "claim-loop finish: {run_id} was not claimed at finish (already reclaimed/done)"
                ),
                Err(e) => warn!(
                    target: "orchestration",
                    "claim-loop finish_run failed for {run_id} (lease will reclaim): {e}"
                ),
            }
        }
        // A transient condition (e.g. no active context yet at startup) → leave the run `claimed` so
        // a later `recover` (next startup, once the condition clears) re-offers it. Terminal outcomes
        // — including un-executable/observe-only runs — return `Ok` above and are retired, so this arm
        // never spins a poison run. `recover` runs at startup, so an unrecovered mid-session claim is
        // re-driven on the next launch, not re-tried in-session.
        Err(e) => warn!(
            target: "orchestration",
            "claim-loop execute deferred for {run_id} (transient — will re-drive on next recover): {e}"
        ),
    }
}

/// The long-lived consumer task (production). Drains the queue, then parks on the wake channel
/// until a producer or restart-recovery signals new work — never busy-spins (AC-1). Mirrors
/// `content_engine::sync::wave::spawn_reverse_drain_loop`.
pub async fn run_claim_loop(deps: ClaimLoopDeps) {
    loop {
        drain_runnable(&deps).await;
        deps.notify.notified().await;
    }
}

/// The production claim-loop deps and startup spawn. The loop opens the coordinator at
/// `codesense_home` (startup-safe — claim/finish need only the store path, not an active team), and
/// its executor hands each claimed routine run to the routine runtime. Durable work left by a prior
/// crash/quit is re-driven via `recover` before the steady-state loop begins.
pub fn spawn_claim_loop(app: AppHandle) {
    let Some(home_dir) = crate::commands::paths::codesense_home_dir() else {
        warn!(target: "orchestration", "claim loop not started: no resolvable codesense_home");
        return;
    };
    let store = match SqliteOrchestrationStore::open(default_store_path(&home_dir)) {
        Ok(store) => store,
        Err(error) => {
            warn!(target: "orchestration", "claim loop not started: store open failed: {error}");
            return;
        }
    };
    let home = home_dir.to_string_lossy().into_owned();
    let sweep_store = store.clone();
    let deps = claim_loop_deps(app.clone(), store.clone(), home);
    // The consumer: re-drive durable work left by a prior crash/quit once at startup, then run the
    // claim loop (park/wake) forever.
    let reconcile_app = app.clone();
    tauri::async_runtime::spawn(async move {
        redrive_recovered(&store).await;
        // Routine runs the previous app left `running` are settled against their sessions once
        // the durable queue is re-offered; they are never relaunched.
        crate::routines::finish::reconcile_on_startup(&reconcile_app).await;
        run_claim_loop(deps).await;
    });
    // In-session reclaim sweep: periodically re-run `recover` so a run left `claimed` by a transient
    // failure (e.g. no active team yet at startup) — or by a mid-session crash of one dispatch — is
    // reclaimed and re-offered **without waiting for the next app launch**. Reclaim only fires once a
    // claim's lease has expired (`CLAIM_LEASE_MS`), so this is the in-session retry cadence, not a
    // busy-retry; a persistently-transient run simply retries each sweep after its lease lapses.
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(RECLAIM_SWEEP_INTERVAL).await;
            redrive_recovered(&sweep_store).await;
        }
    });
}

/// How far ahead of "now" a claim's lease is stamped (ms). A crash-abandoned claim is reclaimable by
/// `recover` once this window elapses. Generous: single-flight execution is short (a routine run
/// returns at launch), and an over-long lease only delays reclaim, never risks a live double-claim.
const CLAIM_LEASE_MS: i64 = 5 * 60 * 1000;

/// How often the in-session reclaim sweep re-runs `recover`. Combined with `CLAIM_LEASE_MS`, a run
/// stuck by a transient condition re-drives within roughly one lease + one sweep of the condition
/// clearing — without an app relaunch. Kept modest so the sweep is negligible background work.
const RECLAIM_SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(120);

/// Epoch-millis "now" — the caller-supplied clock the clock-free store compares leases against. One
/// spelling for both the lease deadline (`now + CLAIM_LEASE_MS`) and `recover`'s cutoff (`now`).
fn now_millis() -> i64 {
    // Reuse the one wall-clock epoch-millis source; the store's lease/cutoff comparisons are `i64`.
    crate::runtime::now_ms() as i64
}

/// Build the production [`ClaimLoopDeps`]: `claim`/`finish` wrap the store off-thread; `execute`
/// launches a claimed routine run; `notify` is the shared wake channel producers signal.
fn claim_loop_deps(app: AppHandle, store: SqliteOrchestrationStore, home: String) -> ClaimLoopDeps {
    let worker = crate::coordinator::WorkerId::new(format!("claim-loop-{}", std::process::id()));
    let claim_store = store.clone();
    let finish_store = store.clone();
    let finish_worker = worker.clone();
    ClaimLoopDeps {
        claim: std::sync::Arc::new(move || {
            let store = claim_store.clone();
            let worker = worker.clone();
            Box::pin(async move {
                let lease = now_millis() + CLAIM_LEASE_MS;
                crate::commands::await_blocking("claim_loop_claim", move || {
                    use crate::coordinator::OrchestrationStore;
                    store
                        .claim_ready(&worker, lease)
                        .map(|opt| opt.map(|c| c.run_id))
                        .map_err(|e| e.to_string())
                })
                .await
            })
        }),
        execute: std::sync::Arc::new(move |run_id| {
            let app = app.clone();
            let store = store.clone();
            let home = home.clone();
            Box::pin(async move { execute_claimed_run(app, store, home, run_id).await })
        }),
        finish: std::sync::Arc::new(move |run_id| {
            let store = finish_store.clone();
            let worker = finish_worker.clone();
            Box::pin(async move {
                crate::commands::await_blocking("claim_loop_finish", move || {
                    store
                        .finish_run(&run_id, &worker)
                        .map_err(|e| e.to_string())
                })
                .await
            })
        }),
        notify: wake_notify(),
    }
}

/// Execute one claimed run: read its durable pointer off-thread and hand a routine pointer to the
/// routine runtime, which carries its own routing (org, team and owner from the routine key).
///
/// **Return contract (the loop acts on it — see [`drive_one`]):**
/// - `Ok(())` = the run reached a **terminal decision** → the loop retires it (`finish_run`). This
///   covers a launched routine run **and** an un-executable one (missing or unrecognised pointer):
///   a re-drive re-reads the same bytes, so retiring avoids a poison `claimed → runnable` loop.
/// - `Err(_)` = a **transient** condition (the coordinator read failed, or the routine runtime
///   reported one) → the loop leaves the run `claimed`, so a later `recover` re-offers it.
async fn execute_claimed_run(
    app: AppHandle,
    store: SqliteOrchestrationStore,
    home: String,
    run_id: String,
) -> Result<(), String> {
    let read_id = run_id.clone();
    let pointer_json = crate::commands::await_blocking("claim_loop_read_pointer", move || {
        store.run_request_json(&read_id).map_err(|e| e.to_string())
    })
    .await?;
    let Some(json) = pointer_json else {
        warn!(target: "orchestration", "claim loop: claimed run {run_id} has no run-request pointer — retiring (unrecoverable)");
        return Ok(());
    };
    let launcher = crate::routines::dispatch::AppLauncher::new(app);
    match crate::routines::dispatch::execute_pointer_json(
        std::path::PathBuf::from(&home),
        &run_id,
        &json,
        &launcher,
    )
    .await
    {
        Some(result) => result,
        None => {
            warn!(target: "orchestration", "claim loop: run {run_id} is not a routine run — retiring (unrecoverable)");
            Ok(())
        }
    }
}

/// Re-drive durable work: reclaim abandoned (lease-expired) claims to `runnable` and re-offer the
/// durable set to the loop. Run once at **startup** (re-drives work left by a prior crash/quit) and
/// again on the **in-session reclaim sweep** (so a run left `claimed` by a transient failure re-drives
/// without an app relaunch). `recover` reclaims + reports in one transaction; the loop drains every
/// `runnable` row (including the freshly reclaimed), so a `wake` is all that is needed. `pending_deliveries` are logged, not
/// re-fired: the event envelope that produced them is not durable, so a delivery with no run is a
/// spent dedup key, not lost work to replay.
async fn redrive_recovered(store: &SqliteOrchestrationStore) {
    let store = store.clone();
    let report = match crate::commands::await_blocking("claim_loop_recover", move || {
        use crate::coordinator::OrchestrationStore;
        store.recover(now_millis()).map_err(|e| e.to_string())
    })
    .await
    {
        Ok(report) => report,
        Err(error) => {
            warn!(target: "orchestration", "orchestration recovery sweep failed (steady state continues): {error}");
            return;
        }
    };
    // Only log when the sweep actually did something, so the periodic sweep is silent at idle.
    if report.reclaimed_claims > 0
        || !report.runnable_runs.is_empty()
        || !report.pending_deliveries.is_empty()
    {
        log::info!(
            target: "orchestration",
            "orchestration recovery: reclaimed={} runnable={} pending_deliveries={}",
            report.reclaimed_claims,
            report.runnable_runs.len(),
            report.pending_deliveries.len(),
        );
    }
    if !report.runnable_runs.is_empty() {
        wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinator::sqlite::SqliteOrchestrationStore;
    use crate::coordinator::store::OrchestrationStore;
    use crate::coordinator::types::{RunState, WorkerId};
    use std::sync::Mutex;

    const LEASE_NEVER: i64 = i64::MAX;

    fn store(dir: &std::path::Path) -> SqliteOrchestrationStore {
        SqliteOrchestrationStore::open(dir.join("loop.sqlite3")).expect("store opens")
    }

    /// Build headless deps over `store`: `claim` maps `claim_ready` to a `run_id`, `execute`
    /// records the id (returning the caller-supplied result), `finish` retires via `finish_run`.
    /// Every closure calls the store **synchronously** (on the driving thread) so the thread-local
    /// fault arm is honored and no `AppHandle` is needed.
    fn recording_deps(
        store: SqliteOrchestrationStore,
        recorded: Arc<Mutex<Vec<String>>>,
        exec_result: Result<(), String>,
    ) -> ClaimLoopDeps {
        let claim_store = store.clone();
        let finish_store = store.clone();
        ClaimLoopDeps {
            claim: Arc::new(move || {
                let s = claim_store.clone();
                Box::pin(async move {
                    s.claim_ready(&WorkerId::new("test-worker"), LEASE_NEVER)
                        .map(|opt| opt.map(|c| c.run_id))
                        .map_err(|e| e.to_string())
                })
            }),
            execute: Arc::new(move |run_id| {
                let recorded = recorded.clone();
                let result = exec_result.clone();
                Box::pin(async move {
                    recorded.lock().unwrap().push(run_id);
                    result
                })
            }),
            finish: Arc::new(move |run_id| {
                let s = finish_store.clone();
                Box::pin(async move {
                    s.finish_run(&run_id, &WorkerId::new("test-worker"))
                        .map_err(|e| e.to_string())
                })
            }),
            notify: Arc::new(Notify::new()),
        }
    }

    // AC-1/AC-3: the loop claims a runnable run, executes it through the closure with the durable
    // run_id, and retires it claimed → done — exactly once.
    #[tokio::test]
    async fn drains_claims_executes_then_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.enqueue_runnable("run-1").unwrap();
        s.enqueue_runnable("run-2").unwrap();

        let recorded = Arc::new(Mutex::new(Vec::new()));
        let deps = recording_deps(s.clone(), recorded.clone(), Ok(()));
        let driven = drain_runnable(&deps).await;

        assert_eq!(driven, 2, "both runnable rows drained single-flight");
        let mut ran = recorded.lock().unwrap().clone();
        ran.sort();
        assert_eq!(ran, vec!["run-1".to_string(), "run-2".to_string()]);
        assert_eq!(s.count_runs_in_state(RunState::Done).unwrap(), 2);
        assert_eq!(s.count_runs_in_state(RunState::Runnable).unwrap(), 0);
        assert_eq!(s.count_runs_in_state(RunState::Claimed).unwrap(), 0);
    }

    // AC-1: an empty queue drives nothing and returns immediately (the caller then parks on the
    // Notify — no busy-spin). A claim on an empty store makes exactly one `claim_ready` probe.
    #[tokio::test]
    async fn empty_queue_drives_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let deps = recording_deps(s, recorded.clone(), Ok(()));
        assert_eq!(drain_runnable(&deps).await, 0);
        assert!(recorded.lock().unwrap().is_empty());
    }

    // AC-2: a crash between execute and finish (ClaimPostExecutePreDone) leaves the run `claimed`,
    // never `done`, so its lease expiry lets `recover` reclaim + re-drive it. The effects ran once.
    #[tokio::test]
    async fn fault_between_execute_and_finish_leaves_run_claimed() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.enqueue_runnable("run-1").unwrap();
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let deps = recording_deps(s.clone(), recorded.clone(), Ok(()));

        fault::arm(FaultPoint::ClaimPostExecutePreDone);
        drain_runnable(&deps).await;
        fault::disarm();

        assert_eq!(
            recorded.lock().unwrap().as_slice(),
            ["run-1"],
            "effects executed once"
        );
        assert_eq!(
            s.count_runs_in_state(RunState::Claimed).unwrap(),
            1,
            "the run stays claimed after a pre-finish crash — recover will reclaim it"
        );
        assert_eq!(s.count_runs_in_state(RunState::Done).unwrap(), 0);
        // The reclaim-on-lease-expiry is proven in the store recovery tests; here we assert only
        // the loop's half: a pre-finish crash never marks the run done, so nothing is lost.
    }

    // AC-1: the long-lived `run_claim_loop` parks on an empty queue and drains a row enqueued after
    // it parked, once woken — the production park/wake path (not just `drain_runnable` in isolation).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_claim_loop_parks_then_drains_on_wake() {
        use std::time::Duration;
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let deps = recording_deps(s.clone(), recorded.clone(), Ok(()));
        let notify = deps.notify.clone();

        // Empty queue → the loop drains nothing and parks on the notify.
        let handle = tokio::spawn(run_claim_loop(deps));
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Enqueue a row after the park, then wake — the loop must pick it up.
        s.enqueue_runnable("run-1").unwrap();
        notify.notify_one();

        let drained = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if s.count_runs_in_state(RunState::Done).unwrap() == 1 {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or(false);
        handle.abort();
        assert!(
            drained,
            "the parked loop drained a row enqueued + woken after it parked"
        );
        assert_eq!(recorded.lock().unwrap().as_slice(), ["run-1"]);
    }

    // AC-1: an execution error leaves the run `claimed` (never `done`) — the same re-offer path a
    // fault takes. The loop must not retire a run whose effects did not complete.
    #[tokio::test]
    async fn execute_error_leaves_run_claimed() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.enqueue_runnable("run-1").unwrap();
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let deps = recording_deps(s.clone(), recorded.clone(), Err("boom".into()));
        drain_runnable(&deps).await;
        assert_eq!(s.count_runs_in_state(RunState::Claimed).unwrap(), 1);
        assert_eq!(s.count_runs_in_state(RunState::Done).unwrap(), 0);
    }

    // SW-AU-80E (Gap 2 — in-session reclaim sweep): `redrive_recovered` reclaims a run left `claimed`
    // with an expired lease and re-offers it to the loop, so a transiently-stuck run recovers WITHOUT
    // an app relaunch. This is the unit the periodic sweep re-runs; the timer itself is a thin wrapper.
    #[tokio::test]
    async fn redrive_recovered_reclaims_an_expired_claim_for_in_session_retry() {
        use crate::coordinator::types::RunState as OrchRunState;
        use crate::coordinator::{OrchestrationStore, WorkerId};
        let home = tempfile::tempdir().unwrap();
        let store = SqliteOrchestrationStore::open(default_store_path(home.path())).unwrap();
        store.enqueue_runnable("run-1").unwrap();
        // Claim with a lease of 1ms-since-epoch — long past `now`, so the sweep's `recover(now)` sees
        // it as abandoned. (Mirrors a run left claimed by a transient execute failure.)
        store
            .claim_ready(&WorkerId::new("stuck"), 1)
            .unwrap()
            .expect("claimable");
        assert_eq!(store.count_runs_in_state(OrchRunState::Claimed).unwrap(), 1);

        redrive_recovered(&store).await;

        assert_eq!(
            store.count_runs_in_state(OrchRunState::Runnable).unwrap(),
            1,
            "the in-session sweep reclaimed the expired claim back to runnable"
        );
        assert_eq!(store.count_runs_in_state(OrchRunState::Claimed).unwrap(), 0);
    }
}
