// SPDX-License-Identifier: MIT
//! SW-SY-8 (AC-3): residual CPU forensic counters — the "did the shipped SW-SY
//! plan actually kill the pegs, and if not, what leaked?" measurement surface.
//!
//! This is an **extension of the SW-SY-7 `sync::timing` spine, not parallel
//! machinery.** It reuses the SAME gate (`log_enabled!` at `SYNC_TIMING_TARGET`)
//! so BOTH the SW-SY-7 claim-walk timing AND every residual counter below flip
//! together. There is deliberately no second gate (no new env var, feature flag, or
//! atomic switch): the whole forensic spine arms as one (AC-3).
//!
//! **Arming it:** the gate is the ambient log level (`lib.rs` parses `CODESENSE_LOG`
//! as a bare `LevelFilter` — `debug`/`info`/…, NOT a per-target `target=level`
//! directive). A **dev build defaults to `Debug`, so the spine is already armed —
//! no env var needed**; a release build arms it with `CODESENSE_LOG=debug`. Read the
//! counters by grepping the log (`~/Library/Logs/ai.codesense/CodeSense.log`) for the
//! `content_engine::sync::timing` target (`residual_snapshot` / per-tick
//! `total_us` lines).
//!
//! The counters live in a [`ResidualCounters`] value. Production uses one
//! process-global instance ([`COUNTERS`]); the free functions ([`note_context_file_change_emit`]
//! et al.) delegate to it so call sites stay one-liners. Because the counters are a
//! *value*, a test can exercise a **local** instance with exact assertions and zero
//! interference from the global one that concurrent tests bump — no shared-static
//! reset, no cross-test flakiness.
//!
//! Each counter is a `Relaxed` atomic bumped **once** at its cost center. A single
//! relaxed add is negligible next to the `WalkDir` / IPC emit / sweep tick it
//! measures, so the counters are always live (a profiling run or a test reads
//! cumulative totals without scraping logs). The *timing* reads (`Instant::now`) at
//! the sweep sites are gated behind [`timing_enabled`], so on a
//! normal prod build (target below `Debug`) no clock is ever read — the
//! instrumentation is inert-when-off (AC-7).
//!
//! The counters map 1:1 onto the architecture.md residuals the SW-SY plan left
//! open (see the story's Dev Notes verified-citation table):
//!   - the per-path `context-file-change` IPC emit (Open-Questions #6)
//!
//! SW-XS-4 retired the reverse drain / entry / new-file / OSC-133 counters with the
//! automatic reverse triggers they measured.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// SW-SY-8 (AC-3): reuse the SW-SY-7 gate target VERBATIM so the residual
/// counters and the claim-walk timing share one switch. A `pub(crate) use`
/// (not a copied literal) guarantees the two can never drift apart.
#[cfg(test)]
pub(crate) use crate::install::story_linker::SYNC_TIMING_TARGET;

/// SW-SY-8 (AC-3): the residual counter set as a value. One process-global
/// instance ([`COUNTERS`]) backs production; tests instantiate their own so exact
/// assertions never race the global that concurrent tests bump. Every field is a
/// `Relaxed` cumulative tally (see the module header for the residual each maps to).
pub(crate) struct ResidualCounters {
    /// `context-file-change` IPC emits — the per-path renderer notification
    /// (Open-Questions #6, ~2,198/burst to already-debounced consumers). One bump
    /// per `app.emit("context-file-change", …)`.
    context_file_change_emits: AtomicU64,
}

impl ResidualCounters {
    /// A zeroed counter set. `const` so the process-global [`COUNTERS`] is a plain
    /// static (and a test can `let c = ResidualCounters::new();` locally).
    pub(crate) const fn new() -> Self {
        Self {
            context_file_change_emits: AtomicU64::new(0),
        }
    }

    /// Record one `context-file-change` IPC emit.
    #[inline]
    pub(crate) fn note_context_file_change_emit(&self) {
        self.context_file_change_emits.fetch_add(1, Relaxed);
    }

    /// Snapshot all counters (each an independent `Relaxed` load — the snapshot is
    /// not a consistent instant across counters, which is fine for a cumulative
    /// forensic read-out).
    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> ResidualSnapshot {
        ResidualSnapshot {
            context_file_change_emits: self.context_file_change_emits.load(Relaxed),
        }
    }
}

/// The process-global residual counter set that production bumps.
static COUNTERS: ResidualCounters = ResidualCounters::new();

/// Record one `context-file-change` IPC emit.
#[inline]
pub(crate) fn note_context_file_change_emit() {
    COUNTERS.note_context_file_change_emit();
}

/// SW-SY-8 (AC-3): an immutable read-out of every residual counter.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ResidualSnapshot {
    pub context_file_change_emits: u64,
}

/// SW-SY-8 (AC-3 / AC-7): is the forensic spine armed? Delegates to the single
/// [`crate::install::story_linker::sync_timing_enabled`] gate read (RF-perf-0 DRY) so
/// the residual counters and the SW-SY-7/RF-perf-0 forward+reverse timing can never
/// arm on different conditions — one gate, one read.
#[cfg(test)]
#[inline]
pub(crate) fn timing_enabled() -> bool {
    crate::install::story_linker::sync_timing_enabled()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bump helpers accumulate into the snapshot on a LOCAL instance — no shared
    /// global, so exact assertions are race-free even under parallel `cargo test`
    /// (the fix for the shared-static flakiness the review caught). This is the whole
    /// point of counters-as-a-value: `snapshot()` on the global races concurrent
    /// production bumps; on a local instance it cannot.
    #[test]
    fn counters_accumulate_exactly_on_a_local_instance() {
        let c = ResidualCounters::new();
        assert_eq!(c.snapshot(), ResidualSnapshot::default(), "starts zeroed");

        c.note_context_file_change_emit();

        let s = c.snapshot();
        assert_eq!(s.context_file_change_emits, 1);
    }

    /// Counters are unconditional (bump regardless of the gate) — the SW-SY-7
    /// atomic-always-on / timing-gated split. Verified on a local instance so it
    /// can't be polluted by (or pollute) the global counters concurrent tests use.
    #[test]
    fn counters_bump_regardless_of_gate() {
        assert!(!timing_enabled(), "gate off at default test log level");
        let c = ResidualCounters::new();
        c.note_context_file_change_emit();
        assert_eq!(
            c.snapshot().context_file_change_emits,
            1,
            "the tally is live even with the timing gate off"
        );
    }

    /// The gate reuses the SW-SY-7 target verbatim — so it can never diverge from
    /// the claim-walk timing switch. This asserts the re-export points at the exact
    /// same string the SW-SY-7 spine gates on, and that the spine is OFF by default
    /// (inert-when-off, AC-7). No counter state touched → race-free.
    #[test]
    fn gate_target_is_the_sw_sy7_spine_and_off_by_default() {
        assert_eq!(
            SYNC_TIMING_TARGET,
            crate::install::story_linker::SYNC_TIMING_TARGET,
            "residual counters must share the SW-SY-7 gate — one switch flips both"
        );
        assert!(
            !timing_enabled(),
            "spine must be OFF by default (inert-when-off); \
             the counters still tally but no Instant/format runs"
        );
    }
}
