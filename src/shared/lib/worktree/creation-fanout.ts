// SPDX-License-Identifier: MIT
// SLW-RI-6 (AC-2/AC-6): bound concurrent worktree CREATIONS.
//
// Opening 10–20 story workspaces at once fires N `setupWorktree` calls, each a
// `git fetch` + `git worktree add` — network- and disk-bound. Left unbounded, a
// burst thrashes the network and the backend blocking pool (the "creating…"
// storm behind the batch-open freeze). This module-level gate mirrors the
// backend width discipline (`LAUNCH_DERIVE_FANOUT_CAP` / `fanout_parallelism`,
// SLW-FX-5 / SLW-RI-5): at most `cap` creations run at once; the rest queue in
// FIFO order — still overlapping, never serial-of-one.
//
// The single-open path is unchanged: one open takes the first slot immediately
// (no queueing, no extra tick). The gate wraps ONLY the creation; the derive /
// materialize wave it triggers is backend-async and is not held under a slot.
//
// RF-perf-14: the FIFO-gate mechanism is now the shared `createFanoutGate`
// factory (fanout-gate.ts) — the background-SETUP gate is a second instance.

import { createFanoutGate } from "./fanout-gate";

/** Default concurrent-creation ceiling. Mirrors `LAUNCH_DERIVE_FANOUT_CAP`. */
export const DEFAULT_CREATION_FANOUT_CAP = 8;

const gate = createFanoutGate(DEFAULT_CREATION_FANOUT_CAP);

/**
 * SLW-RI-6 (AC-6): the CS_* escape-hatch analogue for the renderer. The backend
 * reads `CS_LAUNCH_PARALLELISM` from the environment; the renderer has no env,
 * so the knob is a setter (seed it from a setting if one is added). Raising the
 * cap wakes queued creations so they re-check against the new ceiling; never
 * drops below 1.
 */
export function setCreationFanoutCap(cap: number): void {
  gate.setCap(cap);
}

/** Test/observability hook: creations currently holding a slot. */
export function activeCreationCount(): number {
  return gate.activeCount();
}

/** Test hook: reset to defaults between cases. */
export function __resetCreationFanoutForTest(): void {
  gate.resetForTest();
}

/**
 * Run `fn` under a creation slot: block until fewer than `cap` creations are in
 * flight, then run it, releasing the slot (and waking the next waiter) when it
 * settles — success OR failure, so an aborted/failed setup never leaks a slot.
 */
export async function withCreationSlot<T>(fn: () => Promise<T>): Promise<T> {
  return gate.withSlot(fn);
}
