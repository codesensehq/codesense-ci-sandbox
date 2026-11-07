// SPDX-License-Identifier: MIT
//! SW-IS-F6 concurrent-write measurement harness (AC-2). criterion's single-op
//! model doesn't fit throughput + p50/p99 tail + `SQLITE_BUSY` counting, so this
//! runs as a plain `main` that prints a readout. All logic is in
//! `codesense_lib::perf_support::scale_profile`; this is a thin shell.
//!
//! Axis-1 (AC-4, reverse-sync CPU/mem) is carved to follow-up SW-IS-F6b — it needs
//! the real launched app on the UH e2e-real harness, not this cargo harness.
//!
//! Run: `cargo run -p codesense --release --features bench-support --example scale_profile_harness`.

use codesense_lib::perf_support::scale_profile::{
    run_concurrent_store_upserts, ConcurrentWriteReport, LARGE_ENTRIES, TARGET_COUNTS,
    TYPICAL_ENTRIES,
};

// Writes per thread per cell — enough rounds to expose steady-state contention
// without a multi-minute run at the large cell.
const ROUNDS: usize = 20;

const CELLS: &[(usize, &str)] = &[(TYPICAL_ENTRIES, "typical"), (LARGE_ENTRIES, "large")];

fn main() {
    println!("# SW-IS-F6 scale profile — AC-2 concurrent write\n");
    println!(
        "rounds/thread = {ROUNDS}; N ∈ {TARGET_COUNTS:?}; cells = large({LARGE_ENTRIES}) / typical({TYPICAL_ENTRIES}) entries per target\n"
    );
    print_header();

    for &(entries, cell) in CELLS {
        for &n in TARGET_COUNTS {
            let store =
                run_concurrent_store_upserts(n, ROUNDS, entries).expect("concurrent store upserts");
            print_row(cell, n, &store);
        }
    }

    println!(
        "\n`store_upsert` busy_errors should be 0 at every N (the store is the sole write \
         substrate).\n"
    );

    // Axis-1 reverse-sync CPU/mem (watcher on vs off) is measured against the real
    // launched app on the UH e2e-real harness — carved into follow-up story
    // SW-IS-F6b, not this cargo harness.
}

fn print_header() {
    // `wall` is the post-barrier concurrent window throughput is derived from —
    // printed so the reported thrpt/s is verifiable from the row alone.
    println!(
        "{:<8} {:>4} {:<12} {:>8} {:>12} {:>10} {:>10} {:>10} {:>6}",
        "cell", "N", "op", "ops", "thrpt/s", "p50", "p99", "wall", "busy"
    );
}

fn print_row(cell: &str, n: usize, r: &ConcurrentWriteReport) {
    println!(
        "{:<8} {:>4} {:<12} {:>8} {:>12.0} {:>10} {:>10} {:>10} {:>6}",
        cell,
        n,
        r.label,
        r.ops,
        r.throughput_ops_per_sec,
        format!("{:.2?}", r.p50),
        format!("{:.2?}", r.p99),
        format!("{:.2?}", r.wall),
        r.busy_errors,
    );
}
