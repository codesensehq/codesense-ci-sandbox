// SPDX-License-Identifier: MIT
//! QH-3 hot-path benchmarks: sync-wave reconcile, pipeline tree scan, and PTY
//! spawn-setup. Fixtures are built in each benchmark's untimed setup closure
//! (`iter_batched`), so only the operation under test is measured. Run against
//! two revisions on one machine via `scripts/run-rust-perf-gate.sh`.

use std::time::Duration;

use codesense_lib::perf_support::{PtySpawnSetupFixture, ScanFixture, SyncWaveFixture};
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};

// Shared size matrix. The high-scale ÷ small ratio is the complexity signal the
// gate cares about; a constant-factor shift is best-effort under runner noise.
const SYNC_WAVE_REPOS: &[usize] = &[1, 8, 32];
const SCAN_STORIES: &[usize] = &[50, 200, 1000];
const PTY_ENV_REMOVE: &[usize] = &[0, 8, 32];

// Oracle calibration (AC-7): when `CODESENSE_PERF_INJECT` names a group, that
// group's timed routine does deterministic extra work so the comparator must
// classify it — and only it — as REGRESSION. Off unless the env var is set, so
// it can never contaminate a normal comparison run.
fn inject_slowdown(group: &str) {
    if std::env::var("CODESENSE_PERF_INJECT").ok().as_deref() == Some(group) {
        std::thread::sleep(Duration::from_micros(300));
    }
}

fn bench_sync_wave(c: &mut Criterion) {
    // One current-thread runtime, built once outside the measured region. The
    // flavor matters: `Wave::run` offloads launch batches to a blocking pool, so
    // the deterministic fixture is launch-free and this stays representative.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build sync-wave bench runtime");
    let mut group = c.benchmark_group("sync_wave_reconcile");
    for &repos in SYNC_WAVE_REPOS {
        group.bench_with_input(BenchmarkId::from_parameter(repos), &repos, |b, &repos| {
            b.iter_batched(
                || SyncWaveFixture::build(repos).expect("build sync-wave fixture"),
                |fixture| {
                    inject_slowdown("sync_wave_reconcile");
                    // The async reconcile does real fs work, so it cannot be
                    // optimized away — no black_box needed.
                    runtime
                        .block_on(fixture.run())
                        .expect("sync-wave reconcile");
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_pipeline_scan(c: &mut Criterion) {
    let mut group = c.benchmark_group("pipeline_scan");
    for &stories in SCAN_STORIES {
        group.bench_with_input(
            BenchmarkId::from_parameter(stories),
            &stories,
            |b, &stories| {
                b.iter_batched(
                    || ScanFixture::build(stories).expect("build scan fixture"),
                    |fixture| {
                        inject_slowdown("pipeline_scan");
                        fixture.run().expect("pipeline scan");
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

fn bench_pty_spawn_setup(c: &mut Criterion) {
    let mut group = c.benchmark_group("pty_spawn_setup");
    for &count in PTY_ENV_REMOVE {
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &count| {
            b.iter_batched(
                || PtySpawnSetupFixture::build(count).expect("build pty fixture"),
                |fixture| {
                    inject_slowdown("pty_spawn_setup");
                    fixture.run();
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

// Decision-rule knobs are set explicitly (the docs disagree on the
// `noise_threshold` default). Generous noise threshold + modest sample size fit
// a shared hosted macOS runner; the comparator's INCONCLUSIVE band absorbs the
// residual variance rather than a tight per-bench bound.
fn configured() -> Criterion {
    Criterion::default()
        .noise_threshold(0.05)
        .significance_level(0.05)
        .sample_size(20)
        .warm_up_time(Duration::from_secs(3))
        .measurement_time(Duration::from_secs(6))
}

criterion_group! {
    name = benches;
    config = configured();
    targets = bench_sync_wave, bench_pipeline_scan, bench_pty_spawn_setup
}
criterion_main!(benches);
