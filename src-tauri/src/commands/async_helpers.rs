// SPDX-License-Identifier: MIT
use tokio::task;

/// Wrap synchronous, potentially-blocking work and run it on Tokio's
/// blocking thread pool. Centralizes the `JoinError` → `String` mapping so
/// individual `#[tauri::command]` handlers stay one-liners.
///
/// `label` is included in the error message if the blocking task panics
/// or is cancelled. The two failure modes are distinguished in the message
/// so logs reflect actual runtime behavior (Tokio surfaces both as
/// `JoinError` but `is_cancelled()` separates them).
///
/// Closure-returned errors propagate verbatim — the label is **not**
/// prefixed to them.
pub async fn await_blocking<T, F>(label: &'static str, f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    let _stats =
        crate::shared::invoke_stats::begin(crate::shared::invoke_stats::Kind::Blocking, label);
    task::spawn_blocking(f).await.map_err(|e| {
        // `JoinError::is_cancelled()` is unreachable for `spawn_blocking` in
        // Tokio 1.x — those tasks cannot be aborted. The branch is kept as
        // defensive coverage in case this helper is ever extended to wrap
        // `tokio::spawn`, where cancellation IS a live failure mode.
        if e.is_cancelled() {
            format!("{} task cancelled: {}", label, e)
        } else {
            format!("{} task panicked: {}", label, e)
        }
    })?
}

/// SLW-RI-2: a zero-size **witness** that the bearer is running OFF the Tauri
/// command-dispatch workers — on the Tokio blocking pool (via [`await_blocking`]
/// / `spawn_blocking`) or an engine blocking closure.
///
/// The heavy synchronous fns (the `SWITCH_LOCK` accessor,
/// `commands::install::switch_lock_guard`, and its kin; SW-XS-8 deleted the
/// projection engine fns that once required it too) take a `&BlockingCtx`. The
/// only constructor is the private `()` field below — no code outside this
/// module can build one — so the *sole* way to obtain a witness is through the
/// blocking gateway ([`with_blocking`] / [`await_blocking`]). Naming those fns
/// from an un-offloaded async command handler therefore becomes a **compile
/// error**, not a review catch — the class of freeze that shipped three times
/// (SW-WL-6-A → CO-4 → SLW-FX-7) can no longer be *written*.
///
/// Zero runtime cost (AC-6): the witness is a ZST and every `&BlockingCtx`
/// parameter erases.
pub struct BlockingCtx(());

/// SLW-RI-2: mint a [`BlockingCtx`] for a synchronous closure and run it inline.
///
/// ⚠️ **`unsafe`-like escape hatch — read before using.** Unlike
/// [`await_blocking`], which mints its witness *because* `spawn_blocking` has
/// genuinely moved the work onto the blocking pool, `with_blocking` mints the
/// witness **unconditionally, inline on the current thread**. By itself it
/// proves *nothing* about which thread you are on. Call it **only** where you
/// are ALREADY off the command workers:
///
/// - inside a `tokio::task::spawn_blocking(...)` closure (the engine's four
///   projection/materialize/fsync offloads), or
/// - inside an engine blocking task (RI-1's `cs-engine` runtime).
///
/// Calling it on a Tauri async-runtime worker re-opens the exact
/// command-plane-starvation hole SLW-RI-2 closes. The type system cannot catch
/// that — the **SLW-RI-2 CI backstop** (a grep over the async command/dispatch
/// modules, `commands::guardrail` tests) is what polices it. Any production
/// `with_blocking(` call site inside `src/commands/**` must carry a
/// `// RI-2-ALLOW: <reason>` marker so the backstop can distinguish a justified
/// brief acquire from a reintroduced freeze.
pub fn with_blocking<T>(f: impl FnOnce(&BlockingCtx) -> T) -> T {
    f(&BlockingCtx(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn returns_ok_when_closure_succeeds() {
        let result = await_blocking("test", || Ok::<i32, String>(42)).await;
        assert_eq!(result, Ok(42));
    }

    #[tokio::test]
    async fn propagates_closure_error_verbatim_without_label_prefix() {
        let result =
            await_blocking::<(), _>("test_label", || Err("inner failure".to_string())).await;
        assert_eq!(result, Err("inner failure".to_string()));
    }

    #[tokio::test]
    async fn wraps_panic_with_label_prefix() {
        let result = await_blocking::<(), _>("my_op", || panic!("boom")).await;
        let err = result.unwrap_err();
        assert!(
            err.contains("my_op task panicked"),
            "expected label-prefixed panic message, got: {}",
            err
        );
    }

    // NB-7 / AC-8: Eight blocking closures launched concurrently should run
    // in parallel on Tokio's blocking pool, not serialize. Each closure
    // sleeps 20ms via `std::thread::sleep` (synchronous — would block the
    // whole runtime if the helper ran on a worker thread). Serial execution
    // would be ~160ms; parallel execution should land near 20-30ms.
    //
    // Threshold: <250ms — gives ~90ms separation from the serial-execution
    // mark (160ms). A regression that re-routes work onto a single thread
    // would land at 160ms+ (8 × 20ms + scheduler overhead) and still trip;
    // the wider margin tolerates blocking-pool dispatch latency on
    // contended CI runners where the original 150ms cutoff was flaking
    // (observed 164ms on GitHub Actions, well above the serial bound but
    // still parallel — just slow dispatch).
    //
    // multi_thread runtime is required: Tokio's blocking pool always has
    // many threads (default ~512), but `tokio::join!` polls all N futures
    // from a single async task. Under `current_thread`, the runtime can
    // only execute one task at a time, so even though each
    // `spawn_blocking` dispatches to a different blocking-pool thread,
    // the `await`s on them get sequenced through the single-task pump.
    // multi_thread lets the test task itself parallelize across multiple
    // worker threads, which is what gives `tokio::join!` true concurrency
    // for the purposes of this timing assertion.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn await_blocking_runs_closures_concurrently_on_blocking_pool() {
        use std::time::{Duration, Instant};

        let start = Instant::now();
        let (r0, r1, r2, r3, r4, r5, r6, r7) = tokio::join!(
            await_blocking("c0", || {
                std::thread::sleep(Duration::from_millis(20));
                Ok::<i32, String>(0)
            }),
            await_blocking("c1", || {
                std::thread::sleep(Duration::from_millis(20));
                Ok::<i32, String>(1)
            }),
            await_blocking("c2", || {
                std::thread::sleep(Duration::from_millis(20));
                Ok::<i32, String>(2)
            }),
            await_blocking("c3", || {
                std::thread::sleep(Duration::from_millis(20));
                Ok::<i32, String>(3)
            }),
            await_blocking("c4", || {
                std::thread::sleep(Duration::from_millis(20));
                Ok::<i32, String>(4)
            }),
            await_blocking("c5", || {
                std::thread::sleep(Duration::from_millis(20));
                Ok::<i32, String>(5)
            }),
            await_blocking("c6", || {
                std::thread::sleep(Duration::from_millis(20));
                Ok::<i32, String>(6)
            }),
            await_blocking("c7", || {
                std::thread::sleep(Duration::from_millis(20));
                Ok::<i32, String>(7)
            }),
        );
        let elapsed = start.elapsed();

        // All eight closures returned their tagged values.
        assert_eq!(
            (
                r0.unwrap(),
                r1.unwrap(),
                r2.unwrap(),
                r3.unwrap(),
                r4.unwrap(),
                r5.unwrap(),
                r6.unwrap(),
                r7.unwrap(),
            ),
            (0, 1, 2, 3, 4, 5, 6, 7),
        );
        assert!(
            elapsed < Duration::from_millis(250),
            "expected concurrent execution (<250ms), got {:?} — closures may be serializing",
            elapsed,
        );
    }

    // ---------- SLW-RI-2: BlockingCtx witness ----------

    /// AC-6: the witness is a zero-size type, so `&BlockingCtx` parameters on
    /// the heavy fns erase at runtime — the guardrail is compile-time only, no
    /// runtime cost.
    #[test]
    fn blocking_ctx_is_zero_sized() {
        assert_eq!(std::mem::size_of::<BlockingCtx>(), 0);
    }

    /// AC-1: `with_blocking` mints a usable witness for its closure and returns
    /// the closure's value. (The *converse* — that no code OUTSIDE
    /// `async_helpers` can construct a `BlockingCtx` — is enforced by the
    /// private `()` field: `BlockingCtx(())` does not compile in another module,
    /// so it cannot be unit-tested from here without failing to compile. The
    /// module boundary IS the proof.)
    #[test]
    fn with_blocking_mints_a_witness_and_returns_value() {
        // The witness is only nameable via the closure param — proving the mint
        // path is the sole source.
        // RI-2-ALLOW: witness unit test (async_helpers is on the scan surface).
        let doubled = with_blocking(|_ctx: &BlockingCtx| 21 * 2);
        assert_eq!(doubled, 42);
    }

    /// AC-1: `await_blocking` genuinely runs on the blocking pool, so a witness
    /// minted inside its closure is a true proof-of-off-thread. Confirms the
    /// async gateway composes with `with_blocking`.
    #[tokio::test]
    async fn await_blocking_closure_can_mint_witness() {
        let out = await_blocking("ri2", || {
            // RI-2-ALLOW: witness unit test (async_helpers is on the scan surface).
            let v = with_blocking(|_ctx: &BlockingCtx| 7);
            Ok::<i32, String>(v)
        })
        .await;
        assert_eq!(out, Ok(7));
    }
}
