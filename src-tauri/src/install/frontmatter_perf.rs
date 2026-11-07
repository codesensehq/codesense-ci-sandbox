// SPDX-License-Identifier: MIT
//! RF-perf-9 (AC-1): measurement counters for three non-sync-fanout hot paths —
//! the `story_id` frontmatter lookup (`FrontmatterCache::by_story_id`, per MCP
//! `validate_story` `DUPLICATE_STORY_ID` check / `hydrate` / `dehydrate`), the
//! pipeline-scan `build_story_ref` per-story clone (per `scan_pipeline_tree`), and
//! the warm-start `mtime_diff` pass. These sit on the MCP + scan + launch paths,
//! **not** the sync fanout SW-SY-8's [`crate::content_engine::sync::residual_metrics`]
//! measures — so they get their own counter set, but **share the one gate**.
//!
//! This is an **extension of the RF-perf-0 `sync::timing` spine, not parallel
//! machinery** (same discipline as `residual_metrics`): it reuses the SAME gate read
//! ([`crate::install::story_linker::sync_timing_enabled`]) so the timing here arms on
//! exactly the same condition as the forward/reverse spine — one gate, every path. No
//! second env var, feature flag, or atomic switch.
//!
//! Each counter is a `Relaxed` atomic bumped **once** at its cost center. A single
//! relaxed add is negligible next to the `O(N)` scan / N-clone / warm-start walk it
//! measures, so the counters are always live (a profiling run or a test reads the
//! per-call frequency without scraping logs — the "× call frequency" half of AC-1).
//! The *timing* reads (`Instant::now`) are gated behind [`timing_enabled`] /
//! [`start_timer`], so on a normal prod build (target below `Debug`) no clock is ever
//! read — inert-when-off (AC-1 "zero overhead when off").
//!
//! **Arming it** is identical to the rest of the spine: a dev build defaults to
//! `Debug` (already armed); a release build arms with `CODESENSE_LOG=debug`. Read the
//! per-call timings by grepping the log for the `content_engine::sync::timing` target
//! (`frontmatter_story_id_lookup` / `frontmatter_mtime_diff` lines).

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// RF-perf-9 (AC-1): reuse the RF-perf-0 gate target VERBATIM so this counter set and
/// the forward/reverse timing share one switch. A `pub(crate) use` (not a copied
/// literal) guarantees they can never drift apart.
pub(crate) use crate::install::story_linker::SYNC_TIMING_TARGET;

/// RF-perf-9 (AC-1): the candidate counter set as a value. One process-global instance
/// ([`COUNTERS`]) backs production; tests instantiate their own so exact assertions
/// never race the global that concurrent tests bump (the `residual_metrics` discipline).
/// Every field is a `Relaxed` cumulative tally.
pub(crate) struct FrontmatterPerfCounters {
    /// `by_story_id` invocations (per MCP `validate_story` collision check /
    /// hydrate / dehydrate / resolve-story-path — now the single `story_id` lookup).
    story_id_lookup_calls: AtomicU64,
    /// Total `entries` count observed across all `by_story_id` calls — the N the
    /// lookup is now `O(1)` against (was the `O(N)` scan size). Divided by calls it
    /// gives the mean N; the flat per-call time against a rising N is the win.
    story_id_lookup_scanned: AtomicU64,
    /// `build_story_ref` invocations — one per story per pipeline scan (the N-clone).
    build_story_ref_calls: AtomicU64,
    /// `mtime_diff` invocations — one per warm-start (the cold multi-pass).
    mtime_diff_calls: AtomicU64,
    /// RF-perf-11 (AC-1): `scan_pipeline_tree` invocations — one per FE nav-refresh
    /// tick (150 ms debounce). The denominator the per-scan phase totals hang off.
    scan_calls: AtomicU64,
    /// RF-perf-11: cumulative **cache-hit** story entries across all scans (the
    /// zero-read warm arm). With [`scan_inline_parses`] it gives the steady-state
    /// cache-hit ratio (OQ-2) — the live "× frequency" half; the `#[ignore]`d
    /// bench supplies the per-phase wall-time half.
    scan_cache_hits: AtomicU64,
    /// RF-perf-11: cumulative **inline-parsed** story entries (the cache-miss arm —
    /// genuine index gap, stale re-parse, or a cache-less test/query scan). Bumped
    /// alongside `scan_cache_hits` so `hits / (hits + inline)` is the hit ratio.
    scan_inline_parses: AtomicU64,
}

impl FrontmatterPerfCounters {
    pub(crate) const fn new() -> Self {
        Self {
            story_id_lookup_calls: AtomicU64::new(0),
            story_id_lookup_scanned: AtomicU64::new(0),
            build_story_ref_calls: AtomicU64::new(0),
            mtime_diff_calls: AtomicU64::new(0),
            scan_calls: AtomicU64::new(0),
            scan_cache_hits: AtomicU64::new(0),
            scan_inline_parses: AtomicU64::new(0),
        }
    }

    pub(crate) fn note_story_id_lookup(&self, entries: usize) {
        self.story_id_lookup_calls.fetch_add(1, Relaxed);
        self.story_id_lookup_scanned
            .fetch_add(entries as u64, Relaxed);
    }

    pub(crate) fn note_build_story_ref(&self) {
        self.build_story_ref_calls.fetch_add(1, Relaxed);
    }

    pub(crate) fn note_mtime_diff(&self) {
        self.mtime_diff_calls.fetch_add(1, Relaxed);
    }

    /// RF-perf-11 (AC-1): one aggregate tally per `scan_pipeline_tree` — a single
    /// set of `Relaxed` adds at scan end (not one per file), mirroring
    /// `note_story_id_lookup`'s add-the-total shape. `hits` + `inline` = the scan's
    /// story-entry count; their running ratio is the steady-state cache-hit ratio.
    pub(crate) fn note_scan(&self, hits: usize, inline: usize) {
        self.scan_calls.fetch_add(1, Relaxed);
        self.scan_cache_hits.fetch_add(hits as u64, Relaxed);
        self.scan_inline_parses.fetch_add(inline as u64, Relaxed);
    }

    /// Test/benchmark-only readout — production bumps the counters but never reads a
    /// snapshot (live cost is observed via the gated per-call `debug!` emits, live
    /// frequency by counting them). `#[cfg(test)]` so it is not dead code in prod.
    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> FrontmatterPerfSnapshot {
        FrontmatterPerfSnapshot {
            story_id_lookup_calls: self.story_id_lookup_calls.load(Relaxed),
            story_id_lookup_scanned: self.story_id_lookup_scanned.load(Relaxed),
            build_story_ref_calls: self.build_story_ref_calls.load(Relaxed),
            mtime_diff_calls: self.mtime_diff_calls.load(Relaxed),
            scan_calls: self.scan_calls.load(Relaxed),
            scan_cache_hits: self.scan_cache_hits.load(Relaxed),
            scan_inline_parses: self.scan_inline_parses.load(Relaxed),
        }
    }
}

/// RF-perf-9: process-global counters. Production bumps this via the free functions
/// below; tests use a local [`FrontmatterPerfCounters`] instance for race-free asserts.
pub(crate) static COUNTERS: FrontmatterPerfCounters = FrontmatterPerfCounters::new();

/// One-liner delegates so call sites stay terse (mirrors `residual_metrics`).
pub(crate) fn note_story_id_lookup(entries: usize) {
    COUNTERS.note_story_id_lookup(entries);
}
pub(crate) fn note_build_story_ref() {
    COUNTERS.note_build_story_ref();
}
pub(crate) fn note_mtime_diff() {
    COUNTERS.note_mtime_diff();
}
pub(crate) fn note_scan(hits: usize, inline: usize) {
    COUNTERS.note_scan(hits, inline);
}

/// RF-perf-9 (AC-1): is the timing spine armed? Delegates to the single RF-perf-0 gate
/// read so this and the forward/reverse timing can never arm on different conditions —
/// one gate, every path.
#[inline]
pub(crate) fn timing_enabled() -> bool {
    crate::install::story_linker::sync_timing_enabled()
}

/// RF-perf-9 (AC-1): `Some(Instant::now())` only when the spine is armed; `None`
/// otherwise (no clock read on a normal prod build). Mirrors
/// `residual_metrics::start_timer`.
#[inline]
pub(crate) fn start_timer() -> Option<std::time::Instant> {
    timing_enabled().then(std::time::Instant::now)
}

/// RF-perf-9: a plain-value readout of [`COUNTERS`] for the `#[ignore]`d benchmark and
/// the always-on-tally unit tests. Test-only (production never reads a snapshot).
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct FrontmatterPerfSnapshot {
    pub story_id_lookup_calls: u64,
    pub story_id_lookup_scanned: u64,
    pub build_story_ref_calls: u64,
    pub mtime_diff_calls: u64,
    pub scan_calls: u64,
    pub scan_cache_hits: u64,
    pub scan_inline_parses: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rf_perf9_counters_tally_per_call_and_scanned() {
        // A LOCAL instance — never the global COUNTERS — so concurrent tests can't
        // race these exact assertions (the residual_metrics discipline).
        let c = FrontmatterPerfCounters::new();
        c.note_story_id_lookup(100);
        c.note_story_id_lookup(50);
        c.note_build_story_ref();
        c.note_build_story_ref();
        c.note_build_story_ref();
        c.note_mtime_diff();
        // RF-perf-11: two scans — 100 hits/5 inline, then 40 hits/10 inline.
        c.note_scan(100, 5);
        c.note_scan(40, 10);

        let s = c.snapshot();
        assert_eq!(s.story_id_lookup_calls, 2);
        assert_eq!(
            s.story_id_lookup_scanned, 150,
            "sums entries observed per call"
        );
        assert_eq!(s.build_story_ref_calls, 3, "one bump per story per scan");
        assert_eq!(s.mtime_diff_calls, 1);
        assert_eq!(s.scan_calls, 2, "one bump per scan");
        assert_eq!(s.scan_cache_hits, 140, "sums hit entries across scans");
        assert_eq!(s.scan_inline_parses, 15, "sums inline-parsed entries");
    }

    #[test]
    fn rf_perf9_start_timer_is_none_when_gate_off() {
        // No logger in the test binary → SYNC_TIMING_TARGET is below Debug → the gate
        // is off → no clock is read. Proves the inert-when-off invariant (AC-1).
        assert!(!timing_enabled(), "gate must be off with no logger armed");
        assert!(
            start_timer().is_none(),
            "start_timer must not read the clock when the gate is off"
        );
    }
}
