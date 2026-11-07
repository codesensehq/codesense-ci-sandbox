// SPDX-License-Identifier: MIT
/**
 * Structural benchmark per AC-17 + D-14. Captures timing numbers and logs
 * them, but the *assertions* are only "the benchmark ran, captured a
 * number, did not throw" — numeric pass/fail is local-only because CI is
 * variance-heavy GitHub Actions Linux (D-14).
 *
 * Run locally with `pnpm test --run src/shared/components/code-editor/benchmark.test.ts`
 * and visually inspect the logged numbers to validate the budgets.
 */
import { describe, it, expect } from "vitest";
import { getCodeMirror, _resetCmLoaderForTests } from "./cm-loader";

describe("CodeEditor — benchmarks (informational / local-only per D-14)", () => {
  it("captures CM6 cold-load duration (AC-13 — budget < 300ms locally)", async () => {
    _resetCmLoaderForTests();
    const start = performance.now();
    await getCodeMirror();
    const elapsed = performance.now() - start;
    console.log(`[SS-14 perf] CM6 cold-load: ${elapsed.toFixed(2)}ms (local budget < 300ms)`);
    expect(elapsed).toBeGreaterThan(0);
    expect(Number.isFinite(elapsed)).toBe(true);
  });

  it("captures CM6 warm-load duration (AC-13 — budget < 50ms locally)", async () => {
    // cm-loader is a singleton — first call did the work; subsequent calls
    // return the cached promise and resolve nearly instantly.
    const start = performance.now();
    await getCodeMirror();
    const elapsed = performance.now() - start;
    console.log(`[SS-14 perf] CM6 warm-load: ${elapsed.toFixed(2)}ms (local budget < 50ms)`);
    expect(elapsed).toBeGreaterThanOrEqual(0);
    expect(Number.isFinite(elapsed)).toBe(true);
  });
});
