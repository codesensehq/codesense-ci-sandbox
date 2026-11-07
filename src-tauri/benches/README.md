# Rust perf regression gate

`criterion` micro-benchmarks over the Rust hot paths (sync-wave reconcile,
pipeline tree scan, PTY spawn-setup), compared base-vs-candidate on one machine.
The harness is dev-only: `criterion` is a dev-dependency and the bench target is
gated behind the `bench-support` feature, so a default `cargo build`/`--release`
carries none of it (proved by `scripts/verify-criterion-release-absence.sh`).

## Run the gate locally

```bash
# Compare HEAD against its merge-base with main, same runner, back-to-back:
cargo install cargo-criterion            # one-time
./scripts/run-rust-perf-gate.sh --base <base-sha> --candidate HEAD
```

The runner benches base then candidate in one throwaway worktree, keeping the
criterion data dir between them so the candidate run reports a relative `change`.
`scripts/validate-rust-perf-report.py` turns that into a per-benchmark verdict:

| Verdict            | Meaning                                                        |
| ------------------ | ------------------------------------------------------------- |
| `PASS`             | change CI upper bound is under the threshold                  |
| `REGRESSION`       | change CI lower bound is over the threshold (fails the gate)  |
| `INCONCLUSIVE`     | CI straddles the threshold — re-sample; non-gating            |
| `FORWARD_BASELINE` | no baseline yet (this run establishes it); non-gating         |
| `COULD_NOT_MEASURE`| malformed/missing evidence — fails the gate                   |

Run one benchmark while investigating:

```bash
cargo bench -p codesense --bench hot_paths --features bench-support -- pipeline_scan/1000
```

## Add or investigate a benchmark

1. Put the fixture builder in `src-tauri/src/perf_support.rs`. Split construction
   (untimed) from the operation under test (timed) — the bench calls `build()` in
   the `iter_batched` setup closure and the operation in the routine, so fixture
   and input construction are never measured. Reuse an existing seam (the
   sync-wave fixture is shared with the `dhat-heap` leak gate) rather than copying
   construction.
2. An `async` operation (like `Wave::run`) is driven by a runtime built **once,
   outside** the measured region; the routine only does `runtime.block_on(...)`.
3. Add the bench in `benches/hot_paths.rs` with a size matrix. The high-scale ÷
   small ratio is the signal that survives runner noise — cover it.
4. Set a per-benchmark threshold in `scripts/perf-thresholds.json` with a
   one-line rationale. Start at 10%; tighten only with three clean same-runner
   runs of evidence, and never below ~3% (hosted-runner noise floor).
5. Add an `inject_slowdown("<group>")` call in the routine so the oracle
   self-check can force that group to regress:
   `./scripts/run-rust-perf-gate.sh --calibrate <group>`.

## Determinism notes

- Only same-runner, same-job A/B deltas are meaningful. Committed `target/criterion`
  data from another host is never a comparison oracle.
- Instruction-count benching (iai-callgrind) would be noise-immune but needs
  valgrind, which does not run on the Apple-silicon macOS runner; hence the
  wall-clock A/B plus the INCONCLUSIVE band.
- A benchmark for a seam this story introduces (the PTY prepare-spawn core, absent
  on an older base) is baselined forward-only, not compared against a base that
  predates it.

## Known limitations (by design)

- **`INCONCLUSIVE` is non-gating.** A regression whose confidence interval straddles
  the threshold is reported but does not fail the build — the deliberate cost of a
  generous band on a noisy runner. Escalate a persistently-inconclusive bench by
  raising `sample_size` / `measurement_time`, not by lowering the threshold blindly.
- **Forward-only benches start gating once merged.** A bench whose seam did not exist
  on the compared base reports `FORWARD_BASELINE` until this harness is on `main`;
  the first scheduled run after merge is its first real base-vs-candidate comparison.
- **Oracle injection is fixed-magnitude.** `--calibrate <group>` adds a fixed delay to
  every size of the group; the verdict aggregates by group, so any one size regressing
  marks the group REGRESSION. It proves the comparator can turn red, not a specific
  per-size sensitivity.
- **`--calibrate` forces `base = candidate`** and ignores `--base`.
