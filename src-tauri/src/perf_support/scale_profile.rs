// SPDX-License-Identifier: MIT
//! SW-IS-F6 — N=20–50 scale-profile fixtures for the central install-state store.
//!
//! Seeds N install targets in **both** forms from one in-memory source set (so the
//! JSON lockfile tree and the [`SyncStateStore`] are provably identical), then
//! exposes the read, deferred-detection, and concurrent-write operations the F6
//! benches and the `scale_profile_harness` example measure. All logic lives here so
//! both runner surfaces stay thin shells (mirrors how `hot_paths` delegates to the
//! wave fixture). Everything is behind the `bench-support` feature — none of it
//! enters a default build — and every fixture seeds only into a `tempfile` temp
//! home, never the user's real `install.sqlite3` or lockfile tree.

use std::path::{Path, PathBuf};
use std::sync::Barrier;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use crate::content_engine::layers::LayerId;
use crate::content_engine::sync_state_store::{SyncStateStore, SyncStateStoreError};
use crate::content_engine::{Lockfile, MaterializedEntry};
use crate::domain::SourceRepoSlug;

/// The two entries-per-target cells F6 profiles (D3): the dogfood-large context
/// install and a typical smaller repo, so the go/no-go is not skewed by the
/// biggest install alone.
pub const LARGE_ENTRIES: usize = 9_400;
pub const TYPICAL_ENTRIES: usize = 200;

/// The N-worktree fan-out axis the profile sweeps (ARCH §11.7).
pub const TARGET_COUNTS: &[usize] = &[20, 35, 50];

const BENCH_ORG: &str = "bench-org/scale";
const BENCH_TEAM: &str = "default";
const BENCH_USER_ID: u64 = 1;

/// One seeded install target: the JSON lockfile path it was written to, and the
/// in-memory lockfile that seeded both forms (which carries its own
/// `code_repo_path`).
struct TargetSeed {
    lockfile: Lockfile,
}

/// N install targets seeded into a [`SyncStateStore`] from one source set.
/// SW-IS-3c retired the JSON lockfile, so the fixture seeds the store only. Built
/// in a bench's untimed setup; the timed routines are the `read_*` /
/// `reprovision_*` methods.
pub struct SyncStateStoreScaleFixture {
    // Kept alive until the fixture drops — the DB lives under this temp home.
    _home: TempDir,
    store: SyncStateStore,
    slug: SourceRepoSlug,
    targets: Vec<TargetSeed>,
}

impl SyncStateStoreScaleFixture {
    /// Seed `n_targets` code repos, each with `entries_per_target` materialized
    /// entries, into both forms under one temp home. All N targets share one slug
    /// (the dogfood shape: N worktrees of the same repo → one per-slug dir with N
    /// lockfiles, which is exactly what the reaper tree-walk pays for).
    pub fn build(n_targets: usize, entries_per_target: usize) -> Result<Self, String> {
        let home = tempfile::tempdir().map_err(|e| e.to_string())?;
        // AC-7 safety: the harness must never touch the user's real install state.
        // `tempfile::tempdir` roots under the OS temp dir, never the real
        // `codesense_home` — assert it so a future change to the seed path can't
        // silently start writing live data.
        assert!(
            home.path().starts_with(std::env::temp_dir()),
            "F6 fixture must seed under the OS temp dir, got {}",
            home.path().display()
        );
        let slug = SourceRepoSlug::new("bench-owner", "bench-repo").map_err(|e| e.to_string())?;
        let store_path = home.path().join("install-state").join("install.sqlite3");
        let store = SyncStateStore::open(&store_path).map_err(|e| e.to_string())?;

        let mut targets = Vec::with_capacity(n_targets);
        for t in 0..n_targets {
            let code_repo_path = home.path().join("code").join(format!("repo-{t}"));
            let lockfile = build_lockfile(&code_repo_path, entries_per_target);

            // Store form (real `upsert` — one IMMEDIATE txn, DELETE+INSERT).
            store
                .upsert(BENCH_USER_ID, &slug, &lockfile)
                .map_err(|e| e.to_string())?;

            targets.push(TargetSeed { lockfile });
        }

        let fixture = Self {
            _home: home,
            store,
            slug,
            targets,
        };
        fixture.assert_parity(n_targets)?;
        Ok(fixture)
    }

    /// Guard the seed: the store must enumerate the same N targets before any timing,
    /// or the numbers are meaningless. SW-IS-3c: store is the sole substrate, so this
    /// is a store-only count check (the JSON parity comparison is retired).
    fn assert_parity(&self, n_targets: usize) -> Result<(), String> {
        let store_paths = self
            .store
            .enumerate_for_team(BENCH_ORG, Some(BENCH_TEAM))
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|(_slug, path)| path)
            .collect::<std::collections::BTreeSet<_>>();
        if store_paths.len() != n_targets {
            return Err(format!(
                "store seeded {} targets, expected {n_targets}",
                store_paths.len()
            ));
        }
        Ok(())
    }

    /// AC-1 store read: the indexed SELECT that replaces the tree-walk. Returns the
    /// row count so the criterion routine can `black_box` it against dead-code
    /// elimination.
    pub fn read_store(&self) -> usize {
        self.store
            .enumerate_for_team(BENCH_ORG, Some(BENCH_TEAM))
            .expect("store enumerate")
            .len()
    }

    /// AC-3 store reprovision: an unchanged `upsert` still DELETE+INSERTs all rows
    /// (no-op detection was deferred in SW-IS-1). Measures that always-rewrite cost.
    pub fn reprovision_store(&self) {
        let target = &self.targets[0];
        self.store
            .upsert(BENCH_USER_ID, &self.slug, &target.lockfile)
            .expect("store reprovision upsert");
    }
}

/// One materialized file's bookkeeping, deterministic per index. Absoluteness +
/// NUL invariants (`invariant_check_entries`) hold: `dest_path` descends from an
/// absolute temp `code_repo_path`, `source_path` is rooted absolute.
fn build_lockfile(code_repo_path: &Path, entries: usize) -> Lockfile {
    let mut lockfile = Lockfile::empty(
        BENCH_ORG.to_string(),
        Some(BENCH_TEAM.to_string()),
        code_repo_path.to_path_buf(),
    );
    lockfile.entries = (0..entries)
        .map(|i| {
            let logical_name = format!("skills/skill-{i}.md");
            MaterializedEntry {
                dest_path: code_repo_path.join(&logical_name),
                source_path: PathBuf::from(format!("/layer/source/{logical_name}")),
                logical_name,
                source_layer: LayerId::TeamProjection,
                last_materialized_hash: [(i % 256) as u8; 32],
                last_materialized_mtime_ns: 1_700_000_000_000_000_000_u128 + i as u128,
                projection_key: None,
                source_oid: None,
                last_materialized_size: Some(1024),
            }
        })
        .collect();
    lockfile
}

/// AC-2 concurrent-write result. `ops` counts every attempt (successes plus any
/// that timed out busy); `p50`/`p99` are over all attempt latencies pooled across
/// threads — a busy op's full `busy_timeout` wait IS a real tail sample, so it is
/// included, else the p99 the 3c gate rests on would understate the tail. `wall` is
/// the widest single-thread post-barrier window (the throughput denominator).
#[derive(Debug, Clone)]
pub struct ConcurrentWriteReport {
    pub label: &'static str,
    pub ops: usize,
    pub wall: Duration,
    pub throughput_ops_per_sec: f64,
    pub p50: Duration,
    pub p99: Duration,
    pub busy_errors: usize,
}

/// AC-2 — N threads concurrently `upsert` their own distinct target into the one
/// shared (WAL) store, `rounds` times each. Mirrors the SW-IS-1
/// `ac5_concurrent_upserts_no_busy` `thread::scope` shape; `busy_errors` counts
/// ops that failed as database-locked, which `busy_timeout` should keep at zero.
pub fn run_concurrent_store_upserts(
    n_threads: usize,
    rounds: usize,
    entries_per_target: usize,
) -> Result<ConcurrentWriteReport, String> {
    let fixture = SyncStateStoreScaleFixture::build(n_threads, entries_per_target)?;
    Ok(run_concurrent(&fixture, "store_upsert", rounds, |target| {
        fixture
            .store
            .upsert(BENCH_USER_ID, &fixture.slug, &target.lockfile)
            .map_err(classify_store_error)
    }))
}

/// A concurrent write op's failure, classified so the driver counts genuine
/// contention separately from a real fault.
enum WriteError {
    /// `SQLITE_BUSY`/`SQLITE_LOCKED` — `busy_timeout` did not absorb the contention.
    Busy,
    /// Any other error — a real fault that aborts the run.
    Fault(String),
}

/// Classify a store `upsert` failure by rusqlite error **code** (not `Display`
/// text, which is version/locale-fragile): `SQLITE_BUSY`/`SQLITE_LOCKED` → `Busy`.
fn classify_store_error(e: SyncStateStoreError) -> WriteError {
    if let SyncStateStoreError::Sqlite(rusqlite::Error::SqliteFailure(ffi, _)) = &e {
        if matches!(
            ffi.code,
            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
        ) {
            return WriteError::Busy;
        }
    }
    WriteError::Fault(e.to_string())
}

/// Shared concurrent-write driver: one thread per seeded target, all released from
/// a barrier so the writers genuinely contend, each timing its own `rounds` ops via
/// `write_op`. A `WriteError::Busy` increments `busy_errors`; a `Fault` is a real
/// error and panics. The reported `wall` is the widest post-barrier thread window
/// (thread spawn + barrier arrival are excluded from the throughput denominator).
fn run_concurrent<F>(
    fixture: &SyncStateStoreScaleFixture,
    label: &'static str,
    rounds: usize,
    write_op: F,
) -> ConcurrentWriteReport
where
    F: Fn(&TargetSeed) -> Result<(), WriteError> + Sync,
{
    let n_threads = fixture.targets.len();
    let barrier = Barrier::new(n_threads);
    let per_thread: Vec<ThreadRun> = std::thread::scope(|s| {
        let handles: Vec<_> = fixture
            .targets
            .iter()
            .map(|target| {
                let barrier = &barrier;
                let write_op = &write_op;
                s.spawn(move || {
                    let mut latencies = Vec::with_capacity(rounds);
                    let mut busy = 0usize;
                    barrier.wait();
                    let window_start = Instant::now();
                    for _ in 0..rounds {
                        let op = Instant::now();
                        match write_op(target) {
                            Ok(()) => latencies.push(op.elapsed().as_nanos()),
                            // A busy op waited out the full `busy_timeout` before
                            // failing — that wait is a genuine tail-latency sample,
                            // so record it AND count it, rather than dropping the
                            // longest events from the p99.
                            Err(WriteError::Busy) => {
                                latencies.push(op.elapsed().as_nanos());
                                busy += 1;
                            }
                            Err(WriteError::Fault(m)) => {
                                panic!("{label} op failed (not busy): {m}")
                            }
                        }
                    }
                    ThreadRun {
                        latencies,
                        busy,
                        window: window_start.elapsed(),
                    }
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    report(label, per_thread)
}

/// One worker thread's contribution: its per-op latencies, its busy count, and the
/// post-barrier window it was active for.
struct ThreadRun {
    latencies: Vec<u128>,
    busy: usize,
    window: Duration,
}

fn report(label: &'static str, per_thread: Vec<ThreadRun>) -> ConcurrentWriteReport {
    // p50/p99 pool every op latency across all threads; `wall` is the widest single
    // thread window (the throughput denominator). Two different aggregation bases,
    // intentionally: percentiles want the full op population, throughput wants the
    // real concurrent duration.
    let mut latencies = Vec::new();
    let mut busy_errors = 0usize;
    let mut wall = Duration::ZERO;
    for run in per_thread {
        latencies.extend(run.latencies);
        busy_errors += run.busy;
        wall = wall.max(run.window);
    }
    latencies.sort_unstable();
    let ops = latencies.len();
    let throughput_ops_per_sec = if wall.is_zero() {
        0.0
    } else {
        ops as f64 / wall.as_secs_f64()
    };
    ConcurrentWriteReport {
        label,
        ops,
        wall,
        throughput_ops_per_sec,
        p50: percentile(&latencies, 50.0),
        p99: percentile(&latencies, 99.0),
        busy_errors,
    }
}

/// Nearest-rank percentile of a pre-sorted nanosecond slice. Empty → zero.
fn percentile(sorted_nanos: &[u128], p: f64) -> Duration {
    if sorted_nanos.is_empty() {
        return Duration::ZERO;
    }
    let last = sorted_nanos.len() - 1;
    let rank = ((p / 100.0) * last as f64).round() as usize;
    Duration::from_nanos(sorted_nanos[rank.min(last)] as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_parity_holds() {
        // Small cell — parity is structural, independent of entry count. Post-3c the
        // store is the sole substrate, so parity is a store-only target count.
        let fixture = SyncStateStoreScaleFixture::build(8, 4).expect("build fixture");
        assert_eq!(fixture.read_store(), 8, "store enumerates all targets");
    }

    #[test]
    fn concurrent_store_upserts_report_zero_busy() {
        let report = run_concurrent_store_upserts(6, 3, 4).expect("concurrent upserts");
        assert_eq!(report.ops, 18, "6 threads × 3 rounds");
        assert_eq!(report.busy_errors, 0, "busy_timeout must absorb contention");
        assert!(report.p50 <= report.p99, "p50 ≤ p99");
    }

    #[test]
    fn percentile_nearest_rank() {
        // value == index, so the expected rank reads directly: p maps 0 → first,
        // 100 → last, via round(p/100 * (len-1)).
        let sorted: Vec<u128> = (0..100).collect();
        assert_eq!(percentile(&sorted, 0.0), Duration::from_nanos(0));
        assert_eq!(percentile(&sorted, 50.0), Duration::from_nanos(50));
        assert_eq!(percentile(&sorted, 99.0), Duration::from_nanos(98));
        assert_eq!(percentile(&sorted, 100.0), Duration::from_nanos(99));
        assert_eq!(percentile(&[], 50.0), Duration::ZERO);
    }

    /// The busy classifier is load-bearing (AC-2's "busy_errors = 0"): it must
    /// match on the rusqlite error CODE, and only `BUSY`/`LOCKED` count as busy.
    /// SQLITE_BUSY = 5, SQLITE_LOCKED = 6.
    #[test]
    fn classify_store_error_matches_busy_and_locked_by_code() {
        let busy = SyncStateStoreError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(5),
            None,
        ));
        assert!(matches!(classify_store_error(busy), WriteError::Busy));

        let locked = SyncStateStoreError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(6),
            None,
        ));
        assert!(matches!(classify_store_error(locked), WriteError::Busy));

        // A non-contention sqlite failure (SQLITE_CONSTRAINT = 19) is a real fault.
        let constraint = SyncStateStoreError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(19),
            None,
        ));
        assert!(matches!(
            classify_store_error(constraint),
            WriteError::Fault(_)
        ));

        // A non-sqlite error is a real fault too.
        assert!(matches!(
            classify_store_error(SyncStateStoreError::Data("nope".to_string())),
            WriteError::Fault(_)
        ));
    }

    /// AC-7 safety: the seeded home is under the OS temp dir, never the real
    /// `codesense_home`. (The `build` assert enforces this; pin it as a test too.)
    #[test]
    fn fixture_seeds_only_under_temp_dir() {
        let fixture = SyncStateStoreScaleFixture::build(1, 1).expect("build fixture");
        assert!(fixture._home.path().starts_with(std::env::temp_dir()));
    }

    /// One changed-reprovision measurement result: pooled op latencies + busy count.
    struct ReprovisionRun {
        ops: usize,
        p50: Duration,
        p99: Duration,
        max: Duration,
        busy_errors: usize,
    }

    /// Shared driver for the SW-IS-F8/F9 changed-reprovision measurement. N=50 threads each
    /// own a distinct target and, `ROUNDS` times, change exactly ONE entry then persist it —
    /// via the full `upsert` (F8, `targeted=false`) or the targeted `apply_entry_changes`
    /// (F9, `targeted=true`). The ONLY variable between the F8 and F9 runs is that persist
    /// call — same footprint, threads, rounds, and mutation — so the delta is attributable.
    /// `stagger=true` spreads thread arrivals (`ti * 200µs` past the barrier) to model real
    /// fan-out arrival skew instead of the maximally-adversarial simultaneous hit. p50/p99/
    /// max pool every op; a `busy` op's full `busy_timeout` wait IS a tail sample, so it is
    /// recorded AND counted.
    fn run_changed_reprovision(targeted: bool, stagger: bool) -> ReprovisionRun {
        const THREADS: usize = 50;
        const ROUNDS: usize = 20;
        let fixture = SyncStateStoreScaleFixture::build(THREADS, LARGE_ENTRIES).expect("seed");
        let barrier = Barrier::new(THREADS);
        let per_thread: Vec<(Vec<u128>, usize)> = std::thread::scope(|s| {
            let handles: Vec<_> = fixture
                .targets
                .iter()
                .enumerate()
                .map(|(ti, target)| {
                    let barrier = &barrier;
                    let store = &fixture.store;
                    let slug = &fixture.slug;
                    s.spawn(move || {
                        let mut lf = target.lockfile.clone();
                        let mut lat = Vec::with_capacity(ROUNDS);
                        let mut busy = 0usize;
                        barrier.wait();
                        // Model staggered peer arrival with a FIXED linear `ti * 200µs`
                        // offset — a coarse stand-in for "each peer reaches the store after
                        // its own materialize work", NOT a real materialize-time distribution.
                        // Enough to show a synchronized tight-Barrier isn't what creates the
                        // convoy; not a claim about arbitrary arrival distributions.
                        if stagger {
                            std::thread::sleep(Duration::from_micros(ti as u64 * 200));
                        }
                        for r in 0..ROUNDS {
                            // Change exactly one file this round — a unique value so it
                            // is always a genuine change, never a no-op skip.
                            let idx = r % lf.entries.len();
                            lf.entries[idx].last_materialized_mtime_ns =
                                3_000_000_000_000_000_000_u128 + (ti * ROUNDS + r) as u128;
                            let op = Instant::now();
                            // The ONE difference between the F8 and F9 measurement: the full
                            // manifest upsert vs the targeted single-entry write.
                            let result = if targeted {
                                store.apply_entry_changes(
                                    BENCH_USER_ID,
                                    slug,
                                    &lf,
                                    &[&lf.entries[idx]],
                                    &[],
                                )
                            } else {
                                store.upsert(BENCH_USER_ID, slug, &lf)
                            };
                            match result {
                                Ok(()) => lat.push(op.elapsed().as_nanos()),
                                Err(e) => match classify_store_error(e) {
                                    WriteError::Busy => {
                                        lat.push(op.elapsed().as_nanos());
                                        busy += 1;
                                    }
                                    WriteError::Fault(m) => panic!("fault: {m}"),
                                },
                            }
                        }
                        (lat, busy)
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let mut latencies = Vec::new();
        let mut busy_errors = 0usize;
        for (lat, busy) in per_thread {
            latencies.extend(lat);
            busy_errors += busy;
        }
        latencies.sort_unstable();
        ReprovisionRun {
            ops: latencies.len(),
            p50: percentile(&latencies, 50.0),
            p99: percentile(&latencies, 99.0),
            max: percentile(&latencies, 100.0),
            busy_errors,
        }
    }

    fn print_reprovision(label: &str, r: &ReprovisionRun) {
        println!(
            "{label} changed-reprovision threads=50 rounds=20 entries={} ops={} p50={:?} p99={:?} max={:?} busy_errors={}",
            LARGE_ENTRIES, r.ops, r.p50, r.p99, r.max, r.busy_errors,
        );
    }

    /// SW-IS-F8 baseline (run explicitly, not a pass/fail gate): a CHANGED reprovision under
    /// N=50 concurrency at the large footprint via the FULL `upsert` — the store-seam lock
    /// convoy F9 targets. Run with:
    /// `cargo test -p codesense --lib scale_profile::tests::measure_changed_reprovision_p99 -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn measure_changed_reprovision_p99() {
        print_reprovision("F8-MEASURE", &run_changed_reprovision(false, false));
    }

    /// SW-IS-F9 (AC-5 gate): the SAME harness via the targeted `apply_entry_changes` — the
    /// per-op writer-lock hold drops from read+decode of the whole manifest to one row op.
    #[test]
    #[ignore]
    fn measure_changed_reprovision_p99_targeted() {
        print_reprovision("F9-MEASURE", &run_changed_reprovision(true, false));
    }

    /// SW-IS-F9 (AC-5): staggered-arrival variant of the baseline — models production fan-out
    /// skew (not a synchronized herd), so the convoy is not over-stated.
    #[test]
    #[ignore]
    fn measure_changed_reprovision_staggered_baseline() {
        print_reprovision("F8-MEASURE-STAGGER", &run_changed_reprovision(false, true));
    }

    /// SW-IS-F9 (AC-5): staggered-arrival variant of the targeted path.
    #[test]
    #[ignore]
    fn measure_changed_reprovision_staggered_targeted() {
        print_reprovision("F9-MEASURE-STAGGER", &run_changed_reprovision(true, true));
    }
}
