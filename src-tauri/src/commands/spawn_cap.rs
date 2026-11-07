// SPDX-License-Identifier: MIT
//! SW-DX-6 AC-12 — bounded concurrency cap for spawn-class dashboard actions.
//! SW-DX-17d — reshaped from a fixed `Semaphore` into a **limit-aware atomic
//! counter** so the ceiling is *user-owned and live-adjustable* (a semaphore can
//! `add_permits` to grow but cannot cheaply shrink).
//!
//! Dashboard "action bridge" verbs that *spawn a session* (open a workspace,
//! fire a skill) are heavy: each one launches a real process. To keep a
//! misbehaving (or merely eager) dashboard from flooding the machine, spawns
//! are gated behind a **bounded counting cap** — at most `limit` concurrent
//! spawn-class sessions across all dashboards.
//!
//! SW-DX-17d recasts what a permit *means*: it is no longer "a dashboard launch"
//! but "**a backgrounded (unattended) session**" — the thing the user-visible
//! background-agent limit counts. The counting mechanic is identical; only the
//! acquire/release call sites moved (onto the attach⇄detach transition — see
//! `dashboard_launch`). Both **live** and **parked** (needs-attention)
//! backgrounded sessions consume a slot: a session waiting on the user still
//! occupies attention, so the cap reflects *backgrounded sessions in existence*.
//!
//! Design notes:
//! - Acquisition is **non-blocking**: [`SpawnCap::try_acquire`] does a
//!   compare-and-swap against the live `limit` and *fails fast* with a typed
//!   [`SpawnCapError`] whose string form is the `busy` prefix — it never blocks.
//! - Release is **panic-safe by construction**: [`SpawnPermit`] decrements the
//!   count on drop — including drop-during-unwind — so a spawn that panics can't
//!   leak a slot.
//! - The limit is **live-adjustable** ([`SpawnCap::set_limit`]): raising it
//!   admits previously-refused acquires; lowering it below the live count does
//!   **not** revoke held permits — it only refuses *new* acquires until the count
//!   falls back under the new limit (non-destructive by construction).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Typed rejection returned when the spawn cap is saturated. Its `Display` /
/// `to_string` form is the stable `busy` prefix, matching the reject string
/// the `analyze` bridge verb returns from its single-flight latch — so the
/// frontend can classify a saturated cap identically to a busy agent runner.
/// (SW-DX-17d: the *user-facing* copy is limit-worded at the FE boundary — see
/// AC-3's ban on `cap`/`slot`/`Busy`; this internal prefix is unchanged so the
/// existing `analyze` classification keeps working.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnCapError {
    /// The limit is reached; the caller should back off and retry later.
    Busy,
}

impl std::fmt::Display for SpawnCapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Bare `busy` prefix — see `agent_runner::run_analyze`'s reject.
            SpawnCapError::Busy => write!(f, "busy"),
        }
    }
}

impl std::error::Error for SpawnCapError {}

/// The shared counter state behind a [`SpawnCap`] and its live [`SpawnPermit`]s.
#[derive(Debug)]
struct CapInner {
    /// Slots currently held (live + parked backgrounded sessions).
    count: AtomicUsize,
    /// The live, user-owned ceiling. Adjustable via [`SpawnCap::set_limit`].
    limit: AtomicUsize,
}

/// A held slot in the [`SpawnCap`]. **RAII**: dropping this permit — normally
/// or during a panic unwind — decrements the count, returning the slot.
///
/// SW-DX-17d holds one of these for as long as a session is **backgrounded**
/// (live *or* parked); `adopt` drops it (the session became attended) and
/// `detach` re-acquires one.
#[derive(Debug)]
pub struct SpawnPermit {
    inner: Arc<CapInner>,
}

impl Drop for SpawnPermit {
    fn drop(&mut self) {
        // Return the slot. `fetch_sub` on a count we know is ≥ 1 (this permit is
        // live) can never underflow.
        self.inner.count.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A bounded, counting concurrency cap for spawn-class dashboard actions.
///
/// Cheap to `clone` (shares the inner `Arc<CapInner>`), so a single cap can be
/// held in shared app state and handed to every spawn call site.
#[derive(Debug, Clone)]
pub struct SpawnCap {
    inner: Arc<CapInner>,
}

impl SpawnCap {
    /// Create a cap allowing at most `n` concurrent backgrounded sessions.
    pub fn new(n: usize) -> Self {
        SpawnCap {
            inner: Arc::new(CapInner {
                count: AtomicUsize::new(0),
                limit: AtomicUsize::new(n),
            }),
        }
    }

    /// Try to reserve a slot **without blocking**. Returns a [`SpawnPermit`]
    /// (hold it for the session's backgrounded lifetime) on success, or
    /// [`SpawnCapError::Busy`] immediately when `count >= limit`.
    ///
    /// A lock-free CAS loop over the live count — never awaits, so a saturated
    /// cap fails fast rather than queueing.
    pub fn try_acquire(&self) -> Result<SpawnPermit, SpawnCapError> {
        let mut cur = self.inner.count.load(Ordering::Acquire);
        loop {
            // Read the limit inside the loop so a concurrent `set_limit` is
            // honored on the next CAS attempt.
            if cur >= self.inner.limit.load(Ordering::Acquire) {
                return Err(SpawnCapError::Busy);
            }
            match self.inner.count.compare_exchange_weak(
                cur,
                cur + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Ok(SpawnPermit {
                        inner: Arc::clone(&self.inner),
                    })
                }
                // Lost the race — retry with the freshly-observed count.
                Err(observed) => cur = observed,
            }
        }
    }

    /// Set the live ceiling (SW-DX-17d, user-owned limit). Raising it admits
    /// previously-refused acquires; **lowering it below the live count does not
    /// revoke held permits** — it only refuses *new* acquires until the count
    /// falls back under the new limit. Clamped to a floor of 1 so the ceiling can
    /// never wedge every future launch to zero.
    pub fn set_limit(&self, n: usize) {
        self.inner.limit.store(n.max(1), Ordering::Release);
    }

    /// Take a slot for an agent that is already running (one restored after an app
    /// restart), even past the limit: it cannot be un-run, so it is counted and new
    /// launches wait until enough slots free.
    pub fn force_acquire(&self) -> SpawnPermit {
        self.try_acquire().unwrap_or_else(|_| {
            self.inner.count.fetch_add(1, Ordering::AcqRel);
            SpawnPermit {
                inner: Arc::clone(&self.inner),
            }
        })
    }

    /// Number of slots currently free vs. the live limit (saturating — a limit
    /// lowered below the live count reads `0`, never a wrapped huge number).
    pub fn available(&self) -> usize {
        self.inner
            .limit
            .load(Ordering::Acquire)
            .saturating_sub(self.inner.count.load(Ordering::Acquire))
    }

    /// Slots currently held (live + parked backgrounded sessions) — the `N` in
    /// "N of M". Observability/tests: the panel derives its live `N` from the
    /// `backgrounded` row flags, so there is no command-layer caller.
    #[allow(dead_code)]
    pub fn in_use(&self) -> usize {
        self.inner.count.load(Ordering::Acquire)
    }

    /// The live, user-owned maximum (the `M` in "N of M"). Observability/tests:
    /// the panel's `M` is the FE-owned setting, so there is no command caller.
    #[allow(dead_code)]
    pub fn capacity(&self) -> usize {
        self.inner.limit.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AC-12: cap=1 — first acquire succeeds, a concurrent second acquire
    /// (while the first permit is still held) fails fast with `busy`.
    #[test]
    fn cap_one_second_concurrent_is_busy() {
        let cap = SpawnCap::new(1);

        let first = cap.try_acquire().expect("first acquire should succeed");
        assert_eq!(cap.available(), 0);

        let err = cap
            .try_acquire()
            .expect_err("second acquire should be rejected while saturated");
        assert_eq!(err, SpawnCapError::Busy);
        // The typed error's string form is the stable `busy` prefix.
        assert_eq!(err.to_string(), "busy");

        // Keep `first` alive through the assertions above.
        drop(first);
    }

    /// AC-12: releasing (dropping) a held permit frees the slot — a subsequent
    /// acquire succeeds and `available()` returns to full.
    #[test]
    fn a_forced_slot_counts_past_the_limit_until_dropped() {
        let cap = SpawnCap::new(1);
        let first = cap.force_acquire();
        let over = cap.force_acquire();
        assert_eq!(cap.in_use(), 2);
        assert_eq!(cap.available(), 0);
        drop(first);
        assert_eq!(cap.try_acquire().unwrap_err(), SpawnCapError::Busy);
        drop(over);
        assert!(cap.try_acquire().is_ok());
    }

    #[test]
    fn dropping_permit_frees_slot() {
        let cap = SpawnCap::new(1);

        let permit = cap.try_acquire().expect("acquire should succeed");
        assert_eq!(cap.available(), 0);
        drop(permit);

        assert_eq!(cap.available(), 1, "slot should be back to full after drop");
        let _again = cap
            .try_acquire()
            .expect("acquire should succeed again after release");
        assert_eq!(cap.available(), 0);
    }

    /// AC-12: a parked session holds its slot — modeled by keeping the permit
    /// in scope (not dropped). The cap stays saturated for as long as the
    /// permit is alive.
    #[test]
    fn parked_session_holds_slot() {
        let cap = SpawnCap::new(1);

        // A "parked" session: acquired, never dropped for the duration.
        let _parked = cap.try_acquire().expect("parked acquire should succeed");

        // While parked, the cap stays saturated across repeated attempts.
        assert_eq!(cap.available(), 0);
        assert_eq!(cap.try_acquire().unwrap_err(), SpawnCapError::Busy);
        assert_eq!(cap.try_acquire().unwrap_err(), SpawnCapError::Busy);
        assert_eq!(cap.available(), 0);
    }

    /// AC-12: panic-safety — a permit acquired inside a `catch_unwind` closure
    /// that panics is still released on unwind (RAII), so `available()` returns
    /// to full afterward.
    #[test]
    fn permit_released_on_panic() {
        let cap = SpawnCap::new(1);

        // Suppress the default panic hook so the intentional panic below does
        // not pollute test stderr; restore it afterward.
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));

        let result = std::panic::catch_unwind({
            let cap = cap.clone();
            move || {
                let _permit = cap.try_acquire().expect("acquire inside closure");
                assert_eq!(cap.available(), 0);
                panic!("simulated spawn panic while holding the permit");
            }
        });

        std::panic::set_hook(prev_hook);

        assert!(result.is_err(), "closure was expected to panic");
        assert_eq!(
            cap.available(),
            1,
            "permit must be released on panic unwind (RAII)"
        );
    }

    /// AC-12: cap N>1 — acquire N permits, the (N+1)th fails `busy`, drop one,
    /// and the next acquire succeeds.
    #[test]
    fn cap_n_saturates_and_recovers() {
        const N: usize = 3;
        let cap = SpawnCap::new(N);

        let mut permits = Vec::new();
        for _ in 0..N {
            permits.push(cap.try_acquire().expect("acquire within capacity"));
        }
        assert_eq!(cap.available(), 0);
        assert_eq!(cap.in_use(), N);

        assert_eq!(
            cap.try_acquire().unwrap_err(),
            SpawnCapError::Busy,
            "the (N+1)th acquire must be rejected"
        );

        // Free one slot; the next acquire now succeeds.
        permits.pop();
        assert_eq!(cap.available(), 1);
        let _next = cap
            .try_acquire()
            .expect("acquire should succeed after one release");
        assert_eq!(cap.available(), 0);
    }

    // ── SW-DX-17d: live-adjustable limit (user-owned background-agent limit) ──

    /// Raising the limit admits an acquire that was refused at the old ceiling.
    #[test]
    fn raising_the_limit_admits_a_previously_refused_acquire() {
        let cap = SpawnCap::new(1);
        let _first = cap.try_acquire().expect("first fits under limit 1");
        assert_eq!(cap.try_acquire().unwrap_err(), SpawnCapError::Busy);

        cap.set_limit(2);
        assert_eq!(cap.available(), 1, "raising the limit frees headroom");
        let _second = cap
            .try_acquire()
            .expect("the raised limit admits a second acquire");
        assert_eq!(cap.in_use(), 2);
        assert_eq!(cap.capacity(), 2);
    }

    /// Lowering the limit below the live count does NOT revoke held permits — it
    /// only refuses new acquires until the count falls back under the new limit.
    #[test]
    fn lowering_the_limit_does_not_revoke_live_permits() {
        let cap = SpawnCap::new(3);
        let a = cap.try_acquire().unwrap();
        let _b = cap.try_acquire().unwrap();
        let _c = cap.try_acquire().unwrap();
        assert_eq!(cap.in_use(), 3);

        // Shrink the ceiling under the live count.
        cap.set_limit(1);
        assert_eq!(cap.in_use(), 3, "held permits are NOT revoked");
        assert_eq!(cap.available(), 0, "saturating: no wrap to a huge number");
        // No new acquire until the count falls under the new limit.
        assert_eq!(cap.try_acquire().unwrap_err(), SpawnCapError::Busy);

        // Dropping two brings count (1) under limit (1)? No — need count < 1.
        drop(a);
        assert_eq!(cap.in_use(), 2);
        assert_eq!(cap.try_acquire().unwrap_err(), SpawnCapError::Busy);
    }

    /// `set_limit` clamps to a floor of 1 so the ceiling can't wedge to zero.
    #[test]
    fn set_limit_clamps_to_a_floor_of_one() {
        let cap = SpawnCap::new(4);
        cap.set_limit(0);
        assert_eq!(cap.capacity(), 1, "0 is clamped up to 1");
        let _one = cap.try_acquire().expect("the floor still admits one");
        assert_eq!(cap.try_acquire().unwrap_err(), SpawnCapError::Busy);
    }
}
