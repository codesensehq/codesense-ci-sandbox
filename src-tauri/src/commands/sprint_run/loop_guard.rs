// SPDX-License-Identifier: MIT
//! The runtime loop guards — ARCH §9 layers 2 (causal) and 3 (runtime budgets).
//!
//! These are the durable-chain protections that keep the `file -> event -> run -> effect -> file`
//! loop finite. They are **pure** decision functions over the event's own [`Causation`] chain plus a
//! set of budgets: the causal guard rejects a run whose incoming event already carries the
//! `(automation_id, target_id)` edge it is about to add (a cycle), unless the binding opted in via
//! `allow_cycle`; the runtime guard breaches on max causal depth, a rate budget, or a repeated-edge
//! circuit breaker and routes the Run to `needs_attention` rather than dropping evidence
//! (ARCH-INV-9).
//!
//! Keeping the decision pure — no store handle, no clock — is deliberate: the rate count is sourced
//! by the caller from the durable, append-only run events on the AU-80A store (never an in-memory
//! counter, AC-8) and passed in, so the loop-detection logic is exhaustively unit-testable and the
//! one IO dependency lives at the boundary.
//!
//! Layer 1 (static authoring: DAG-cycle rejection + direct self-trigger warning) is an
//! authoring-time concern that reuses the existing DAG validation (`super::dag`) and the one
//! predicate evaluator; it is not part of this runtime module.
//!
//! Routine event admission (`crate::routines::admit`) is the live caller: a routine's own run
//! finishing carries the routine's edge, so its re-fire is the rejected cycle, and its rate count
//! comes from the durable `routine_runs` rows.

use super::causation::{CausalEdge, Causation};

/// Which runtime budget a chain breached (ARCH §9 layer 3). Each maps to a truthful `loop:*` reason
/// recorded on the parked Run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopReason {
    /// The causal chain is deeper than `max_depth` allows.
    MaxDepth,
    /// Too many triggers landed in the rate window (counted from durable run events).
    RateBudget,
    /// One `(automation_id, target_id)` edge repeated past the circuit-breaker limit — the bound that
    /// keeps an `allow_cycle` opt-in finite.
    RepeatedEdge,
}

impl LoopReason {
    /// The durable reason token recorded on the `needs_attention` Run (AC-8/AC-9). One closed
    /// vocabulary — the FE maps these to copy, Rust never composes the string elsewhere.
    pub fn as_token(self) -> &'static str {
        match self {
            LoopReason::MaxDepth => "loop:max_depth",
            LoopReason::RateBudget => "loop:rate_budget",
            LoopReason::RepeatedEdge => "loop:repeated_edge",
        }
    }
}

/// The per-causal-chain budgets (ARCH §9 layer 3). Breaching any one parks the Run at
/// `needs_attention`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopBudgets {
    /// Maximum causal depth a chain may reach before a run is parked.
    pub max_depth: u32,
    /// Maximum triggers allowed in the rate window (the count is store-derived, passed to
    /// [`admit`]).
    pub rate_limit: u32,
    /// The occurrence count at which the repeated-edge circuit breaker trips: the run is parked once
    /// this edge would occur for the `repeated_edge_limit`-th time in the chain (counting the one
    /// about to be added). Bounds an `allow_cycle`'d edge; values below 2 make an opt-in cycle
    /// unrunnable (it would breach on its first repeat), so keep it ≥ 2 (default 4).
    pub repeated_edge_limit: u32,
}

impl Default for LoopBudgets {
    /// Conservative defaults: a legitimate `file -> effect -> file` chain is one or two hops deep, so
    /// these cut in well above normal operation but far below a runaway.
    fn default() -> Self {
        LoopBudgets {
            max_depth: 16,
            rate_limit: 32,
            repeated_edge_limit: 4,
        }
    }
}

/// The admission decision for one about-to-run automation on one incoming event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopVerdict {
    /// The run is admitted.
    Admit,
    /// A repeated `(automation_id, target_id)` edge with no `allow_cycle` opt-in — the run is not
    /// admitted (recorded as an append-only coordination fact by the caller, ARCH-INV-9).
    RejectCycle,
    /// A runtime budget was breached — the Run is parked at `needs_attention` with this reason,
    /// evidence retained.
    Breach(LoopReason),
}

impl LoopVerdict {
    /// The `loop:*` reason a breach carries, for recording on the parked `needs_attention` Run
    /// (AC-9). `None` for `Admit` / `RejectCycle`.
    pub fn needs_attention_reason(self) -> Option<&'static str> {
        match self {
            LoopVerdict::Breach(reason) => Some(reason.as_token()),
            _ => None,
        }
    }
}

/// Evaluate the causal (L2) and runtime (L3) guards for one run admission.
///
/// - `causation` is the incoming event's own chain (carried on the envelope, AU-74 + AC-5).
/// - `edge` is the `(automation_id, target_id)` this run would add.
/// - `allow_cycle` is the binding policy opt-in (AU-80C `policy:` block); when unset a repeat is
///   rejected.
/// - `rate_count` is the number of triggers already seen in the rate window, sourced by the caller
///   from the durable append-only run events (AC-8) — never an in-memory counter.
///
/// Runaway depth and rate are chain-global breaches checked first (independent of the cycle opt-in);
/// then a repeated edge is either rejected (no opt-in) or admitted-but-bounded by the repeated-edge
/// circuit breaker (opted in).
pub fn admit(
    causation: &Causation,
    edge: &CausalEdge,
    allow_cycle: bool,
    budgets: &LoopBudgets,
    rate_count: u32,
) -> LoopVerdict {
    // Depth is the greater of the carried `chain_depth` and the actual `ancestry` length, so a
    // producer that under-reports one (they are separate fields) cannot slip past the depth budget.
    let depth = causation.chain_depth.max(causation.ancestry.len() as u32);
    if depth >= budgets.max_depth {
        return LoopVerdict::Breach(LoopReason::MaxDepth);
    }
    if rate_count >= budgets.rate_limit {
        return LoopVerdict::Breach(LoopReason::RateBudget);
    }

    // Occurrences of this edge once the run adds it (existing in the chain + the one about to be added).
    let occurrences = causation.ancestry.iter().filter(|e| *e == edge).count() as u32 + 1;
    if occurrences >= 2 {
        if allow_cycle {
            if occurrences >= budgets.repeated_edge_limit {
                return LoopVerdict::Breach(LoopReason::RepeatedEdge);
            }
            return LoopVerdict::Admit;
        }
        return LoopVerdict::RejectCycle;
    }

    LoopVerdict::Admit
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(depth: u32, edges: &[(&str, &str)]) -> Causation {
        Causation {
            chain_depth: depth,
            ancestry: edges.iter().map(|(a, t)| CausalEdge::new(*a, *t)).collect(),
            ..Causation::default()
        }
    }

    // AC-7: a fresh edge on a shallow chain is admitted.
    #[test]
    fn admits_a_novel_edge() {
        let c = chain(1, &[("auto-1", "SW-AU-70")]);
        let edge = CausalEdge::new("auto-2", "SW-AU-93");
        assert_eq!(
            admit(&c, &edge, false, &LoopBudgets::default(), 0),
            LoopVerdict::Admit
        );
    }

    // AC-7: a repeated (automation_id, target_id) with no opt-in is rejected — the run is not admitted.
    #[test]
    fn rejects_a_repeated_edge_without_opt_in() {
        let edge = CausalEdge::new("auto-1", "SW-AU-93");
        let c = chain(2, &[("auto-1", "SW-AU-93")]);
        assert_eq!(
            admit(&c, &edge, false, &LoopBudgets::default(), 0),
            LoopVerdict::RejectCycle
        );
    }

    // AC-7: the same repeat with allow_cycle is admitted (until the circuit breaker, below).
    #[test]
    fn admits_a_repeated_edge_when_cycle_allowed() {
        let edge = CausalEdge::new("auto-1", "SW-AU-93");
        let c = chain(2, &[("auto-1", "SW-AU-93")]);
        assert_eq!(
            admit(&c, &edge, true, &LoopBudgets::default(), 0),
            LoopVerdict::Admit
        );
    }

    // AC-8: max causal depth breaches → needs_attention with loop:max_depth.
    #[test]
    fn breaches_on_max_depth() {
        let c = chain(16, &[]);
        let edge = CausalEdge::new("auto-1", "SW-AU-93");
        let v = admit(&c, &edge, false, &LoopBudgets::default(), 0);
        assert_eq!(v, LoopVerdict::Breach(LoopReason::MaxDepth));
        assert_eq!(v.needs_attention_reason(), Some("loop:max_depth"));
    }

    // AC-8: the rate budget (store-sourced count) breaches → loop:rate_budget.
    #[test]
    fn breaches_on_rate_budget() {
        let c = chain(1, &[]);
        let edge = CausalEdge::new("auto-1", "SW-AU-93");
        let v = admit(&c, &edge, false, &LoopBudgets::default(), 32);
        assert_eq!(v, LoopVerdict::Breach(LoopReason::RateBudget));
        assert_eq!(v.needs_attention_reason(), Some("loop:rate_budget"));
    }

    // AC-8: an allow_cycle'd edge is still bounded by the repeated-edge circuit breaker.
    #[test]
    fn breaches_on_repeated_edge_circuit_breaker() {
        let edge = CausalEdge::new("auto-1", "SW-AU-93");
        // Three prior occurrences + the one about to be added = 4 = repeated_edge_limit.
        let c = chain(
            4,
            &[
                ("auto-1", "SW-AU-93"),
                ("auto-1", "SW-AU-93"),
                ("auto-1", "SW-AU-93"),
            ],
        );
        let v = admit(&c, &edge, true, &LoopBudgets::default(), 0);
        assert_eq!(v, LoopVerdict::Breach(LoopReason::RepeatedEdge));
        assert_eq!(v.needs_attention_reason(), Some("loop:repeated_edge"));
    }

    // AC-9: admit / reject carry no needs_attention reason (only a breach parks the Run).
    #[test]
    fn only_breach_carries_a_needs_attention_reason() {
        assert_eq!(LoopVerdict::Admit.needs_attention_reason(), None);
        assert_eq!(LoopVerdict::RejectCycle.needs_attention_reason(), None);
    }
}
