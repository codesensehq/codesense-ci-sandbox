// SPDX-License-Identifier: MIT
//! Seam-level fault injection (AC-8).
//!
//! The acceptance evidence for the multi-row atomicity cases is a harness that
//! aborts a coordination operation at a precise mid-point — after the first write
//! but before the commit — and asserts no partial state survives. This mirrors the
//! established thread-local fault-flag idiom in
//! `context_repo/team_rename.rs::FAULT_BEFORE_NON_GIT_MOVES`; it is compiled only
//! under `test` so no abort point can fire in a shipped build.
//!
//! What the abort proves, precisely: it forces an early return that drops the open
//! `Transaction`, which rolls back the uncommitted writes. That is the *same* end
//! state a real process crash mid-transaction produces — SQLite discards the
//! uncommitted WAL frames on the next open — so this in-process abort is a faithful
//! stand-in for crash atomicity without the flakiness of forking and killing a
//! child. Durability of *committed* state across a real restart is proven
//! separately by reopening the store from its path (AC-7).

/// A named point inside a multi-row operation where the harness can force an
/// abort. Each maps to a "post-first-write / pre-commit" window a real crash
/// could land in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultPoint {
    /// `recover`: after abandoned claims are reclaimed to `runnable`, before the
    /// durable work is scanned — a crash here must roll the reclaim back so a retry
    /// reclaims idempotently (SW-AU-80B AC-4).
    RecoverPostReclaimPreScan,
    /// `enqueue_on_accept`: after the runnable run row is written, before the delivery
    /// is consumed and the run-request pointer is persisted — a crash here must leave
    /// neither a runnable row without its pointer nor a consumed delivery without its
    /// run (SW-AU-80D AC-1).
    EnqueuePostRunnablePrePointer,
    /// The claim-driven dispatcher (SW-AU-80E): after a claimed run's effects executed,
    /// before `finish_run` transitions it `claimed → done`. A crash here leaves the run
    /// `claimed`; its lease later expires, `recover` reclaims it to `runnable`, and the
    /// re-drive re-executes (the lease is a timeout, not a fence). Injected in the loop,
    /// not the store, so it lives beside the store's points but fires in `claim_loop`.
    ClaimPostExecutePreDone,
}

#[cfg(test)]
thread_local! {
    /// The armed abort point for the current thread, if any. Thread-local so a
    /// concurrent test on another thread is unaffected — matching the
    /// `team_rename.rs` pattern.
    static ARMED: std::cell::Cell<Option<FaultPoint>> = const { std::cell::Cell::new(None) };
}

/// Arm `point` to abort the next operation that reaches it **on this thread**.
/// Test-only.
#[cfg(test)]
pub fn arm(point: FaultPoint) {
    ARMED.with(|c| c.set(Some(point)));
}

/// Disarm any armed fault on this thread. Test-only.
#[cfg(test)]
pub fn disarm() {
    ARMED.with(|c| c.set(None));
}

/// If `point` is armed on this thread, consume the arming and return `true`
/// (the caller aborts). One-shot: a subsequent pass through the same point does
/// not re-fire unless re-armed, so a retry after the injected crash proceeds.
///
/// In non-test builds this is a `const false` — the abort machinery does not
/// exist outside tests, so there is zero shipped overhead and no way to trip it.
#[inline]
pub fn should_abort(point: FaultPoint) -> bool {
    #[cfg(test)]
    {
        ARMED.with(|c| {
            if c.get() == Some(point) {
                c.set(None);
                true
            } else {
                false
            }
        })
    }
    #[cfg(not(test))]
    {
        let _ = point;
        false
    }
}
