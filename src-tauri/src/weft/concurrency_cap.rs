// SPDX-License-Identifier: MIT
//! The bounded auto-dispatch concurrency cap — the runaway-dispatch rail.
//!
//! Before an auto-dispatch, the controller run reads how many child workers are
//! already LIVE for the scope and compares that against a small cap. When the fleet
//! is at capacity the claimed item PARKS as `needs-you` (via `wv_upsert_attention`)
//! instead of fanning out another worker — so a dedup miss (many claims for the same
//! work) cannot flood the fleet with N spawns.
//!
//! # Why this is a pure decision, not a loop
//!
//! Keeping agents out of core is a hard rule, and there is NO second orchestrator: the
//! controller loop is PACK PROMPT content (`weft-controller.yml`), and the run itself
//! reads the live count and applies the cap between claim and spawn. This module ships
//! only (a) the fleet live-count read seam the run consults and (b) the pure
//! spare-slot / admittance decision — no task, thread, interval, or dispatch loop. The
//! decision is total and side-effect-free so it is unit-testable without a live fleet.

/// The M1 auto-dispatch concurrency cap: the maximum number of LIVE child workers a
/// single controller scope will fan out to. A small const for M1 (a config knob is a
/// later story); the value bounds the blast radius of a dedup miss.
pub const DEFAULT_CONCURRENCY_CAP: i64 = 4;

/// The controller's decision at the claim→spawn seam, once the graduation gate has
/// admitted an item for auto-dispatch. `Admit` spawns the worker; `Park` overflows the
/// claimed item onto the attention board (`needs-you`) instead of spawning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapDecision {
    /// There is a spare slot — dispatch the worker (carries the spare count at decision
    /// time, for observability in the caller/prompt).
    Admit { spare: i64 },
    /// The fleet is at (or over) capacity — park the claimed item as `needs-you` rather
    /// than fanning out another worker.
    Park { cap: i64, live: i64 },
}

impl CapDecision {
    /// True when the decision admits a spawn.
    pub fn admits(&self) -> bool {
        matches!(self, CapDecision::Admit { .. })
    }
}

/// The spare-slot count for a scope: `cap - live`, floored at zero (an over-capacity
/// fleet reads zero spare, never negative). A negative or zero `cap` yields zero spare
/// (a cap that admits nobody parks everything — fail-closed).
pub fn spare_slots(cap: i64, live: i64) -> i64 {
    (cap - live).max(0)
}

/// The auto-dispatch admittance decision: with `live` workers already running and a cap
/// of `cap`, admit a spawn only when a slot is spare; otherwise park. This is the ONE
/// place the cap decision lives — the controller consults it between the graduation gate
/// and the spawn, and the human-fire path never reaches it (human fire is never capped).
pub fn dispatch_admittance(cap: i64, live: i64) -> CapDecision {
    if spare_slots(cap, live) >= 1 {
        CapDecision::Admit {
            spare: spare_slots(cap, live),
        }
    } else {
        CapDecision::Park { cap, live }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spare_is_cap_minus_live_floored_at_zero() {
        assert_eq!(spare_slots(4, 0), 4);
        assert_eq!(spare_slots(4, 3), 1);
        assert_eq!(spare_slots(4, 4), 0);
        // Over capacity never goes negative.
        assert_eq!(spare_slots(4, 7), 0);
        // A non-positive cap admits nobody (fail-closed).
        assert_eq!(spare_slots(0, 0), 0);
        assert_eq!(spare_slots(-1, 0), 0);
    }

    #[test]
    fn admits_while_a_slot_is_spare() {
        assert!(dispatch_admittance(4, 0).admits());
        assert!(dispatch_admittance(4, 3).admits());
        assert_eq!(dispatch_admittance(4, 3), CapDecision::Admit { spare: 1 });
    }

    #[test]
    fn parks_at_and_over_capacity() {
        // At capacity → park.
        assert_eq!(
            dispatch_admittance(4, 4),
            CapDecision::Park { cap: 4, live: 4 }
        );
        assert!(!dispatch_admittance(4, 4).admits());
        // Over capacity (a dedup miss already fanned out extra) → still park.
        assert!(!dispatch_admittance(4, 9).admits());
    }

    #[test]
    fn a_dedup_miss_never_floods_the_fleet() {
        // Simulate N claims arriving for the same work with a full fleet: every one of
        // them parks, none spawns — the runaway-dispatch rail holds.
        let cap = DEFAULT_CONCURRENCY_CAP;
        let live = DEFAULT_CONCURRENCY_CAP; // already at capacity
        let mut spawns = 0;
        let mut parks = 0;
        for _ in 0..25 {
            match dispatch_admittance(cap, live) {
                CapDecision::Admit { .. } => spawns += 1,
                CapDecision::Park { .. } => parks += 1,
            }
        }
        assert_eq!(
            spawns, 0,
            "a full fleet admits no spawn under a claim storm"
        );
        assert_eq!(parks, 25, "every overflow claim parks as needs-you");
    }

    #[test]
    fn default_cap_is_a_small_bounded_const() {
        const {
            assert!(
                DEFAULT_CONCURRENCY_CAP >= 1,
                "the cap must admit at least one worker"
            );
            assert!(
                DEFAULT_CONCURRENCY_CAP <= 16,
                "the M1 cap is a SMALL bound (blast-radius rail), not unbounded"
            );
        }
    }
}
