// SPDX-License-Identifier: MIT
import { expect } from "vitest";

export type Counters = Record<string, number>;

export interface FlatAcrossOptions {
  /** Round trips run before the baseline is read (default 2): the first visits are allowed to mint. */
  warmup?: number;
  /** How many round trips to measure (default 50). */
  repeat?: number;
  /** Counters that may grow by at most this much over the measured round trips. */
  allowed?: Partial<Record<string, number>>;
}

/**
 * The shared "flat across switches" assertion. `roundTrip` performs one full switch cycle (A→B then
 * B→A, or A→C→A across orgs); `read` returns the counters that must not grow (mounts, mints, hub
 * watches, empty-state renders, terminal attaches, held disposers). After `warmup` round trips the
 * counters are snapshotted, `repeat` more run, and every counter must equal its baseline plus its
 * allowance. Returns the baseline and final counters so a test can print a before/after table.
 */
export async function expectFlatAcross(
  roundTrip: () => void | Promise<void>,
  read: () => Counters,
  opts: FlatAcrossOptions = {},
): Promise<{ baseline: Counters; final: Counters }> {
  for (let i = 0; i < (opts.warmup ?? 2); i++) await roundTrip();
  const baseline = read();
  for (let i = 0; i < (opts.repeat ?? 50); i++) await roundTrip();
  const final = read();
  const expected = Object.fromEntries(
    Object.entries(baseline).map(([name, value]) => [name, value + (opts.allowed?.[name] ?? 0)]),
  );
  expect(final).toEqual(expected);
  return { baseline, final };
}
