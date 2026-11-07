// SPDX-License-Identifier: MIT
//! SW-IS-F6 Axis-2 single-op benches: the install-state read win (AC-1) and the
//! deferred-change-detection cost (AC-3). criterion fits these because each is one
//! operation measured in isolation; the concurrent-write (AC-2) and Axis-1 (AC-4)
//! measurements are throughput/percentile/time-window shaped and live in the
//! `scale_profile_harness` example instead. All logic is in
//! `codesense_lib::perf_support::scale_profile`; this file is a thin shell.
//!
//! Run: `cargo bench -p codesense --features bench-support --bench scale_profile`.

use std::time::Duration;

use codesense_lib::perf_support::scale_profile::{
    SyncStateStoreScaleFixture, LARGE_ENTRIES, TARGET_COUNTS, TYPICAL_ENTRIES,
};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

// Both entries-per-target cells (D3) so the read win isn't judged on the biggest
// install alone.
const CELLS: &[(usize, &str)] = &[(TYPICAL_ENTRIES, "typical"), (LARGE_ENTRIES, "large")];

// AC-1 (post-3c): store indexed SELECT enumerate cost, at each N × cell. The JSON
// per-slug tree-walk baseline is retired — the store is the sole substrate — so this
// now measures the surviving store read in isolation. The fixture is seeded once per
// cell/N (untimed) and re-read.
fn bench_enumerate_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("is_enumerate_read");
    for &(entries, cell) in CELLS {
        for &n in TARGET_COUNTS {
            let fixture =
                SyncStateStoreScaleFixture::build(n, entries).expect("seed enumerate fixture");
            let id = format!("{cell}/n{n}");
            group.bench_with_input(BenchmarkId::new("store", &id), &fixture, |b, fx| {
                b.iter(|| black_box(fx.read_store()));
            });
        }
    }
    group.finish();
}

// AC-3 (post-3c): the unchanged-reprovision case — store `upsert` (always
// DELETE+INSERT) per-target rewrite cost. The JSON `write_if_changed` no-op-skip
// baseline is retired with the JSON substrate. One target is enough; the question is
// per-target rewrite cost, not fan-out.
fn bench_reprovision_unchanged(c: &mut Criterion) {
    let mut group = c.benchmark_group("is_reprovision_unchanged");
    for &(entries, cell) in CELLS {
        let fixture =
            SyncStateStoreScaleFixture::build(1, entries).expect("seed reprovision fixture");
        group.bench_with_input(BenchmarkId::new("store_upsert", cell), &fixture, |b, fx| {
            b.iter(|| fx.reprovision_store())
        });
    }
    group.finish();
}

// Same runner discipline as `hot_paths`: modest sample size + bounded windows so a
// full run finishes in minutes on a shared macOS runner, seed cost untimed.
fn bench_sessions_read(c: &mut Criterion) {
    let fixture = codesense_lib::perf_support::live_hub::SessionsFixture::build();
    c.bench_function("sessions_read/50_sessions_5000_messages", |b| {
        b.iter(|| black_box(fixture.read()))
    });
}

fn bench_inbox(c: &mut Criterion) {
    let mut fixture = codesense_lib::perf_support::live_hub::InboxFixture::build();
    assert!(fixture.first_snapshot() <= 200);
    c.bench_function("inbox_first_snapshot/5000_messages", |b| {
        b.iter(|| black_box(fixture.first_snapshot()))
    });
    c.bench_function("inbox_window_move/5000_messages", |b| {
        b.iter(|| black_box(fixture.window()))
    });
}
fn bench_agent_list(c: &mut Criterion) {
    let mut fixture = codesense_lib::perf_support::live_hub::AgentListFixture::build();
    assert_eq!(fixture.snapshot(), 50);
    let delta_bytes = fixture.delta();
    assert!(delta_bytes < 2048);
    println!("agentList D5: 50 reference rows, structural delta {delta_bytes} bytes");
    c.bench_function("agent_list_reference_delta/50_sessions_2_hosts", |b| {
        b.iter(|| black_box(fixture.delta()))
    });
}
fn configured() -> Criterion {
    Criterion::default()
        .noise_threshold(0.05)
        .significance_level(0.05)
        .sample_size(20)
        .warm_up_time(Duration::from_secs(2))
        .measurement_time(Duration::from_secs(5))
}

criterion_group! {
    name = benches;
    config = configured();
    targets = bench_enumerate_read, bench_reprovision_unchanged, bench_sessions_read, bench_inbox, bench_agent_list
}
criterion_main!(benches);
