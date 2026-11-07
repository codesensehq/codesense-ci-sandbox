// SPDX-License-Identifier: MIT
/**
 * RF-perf-4 (AC-1): a read-only, O(1) frontend instance registry — the per-resource
 * attribution the RF-perf-5 coarse RSS/heap number cannot give. Each heavy surface
 * class (editor, iframe, WebGL-backed terminal, mounted team) increments a
 * counter on mount and decrements on unmount via {@link useInstanceCount}; the
 * heartbeat reads a {@link snapshotInstanceCounts} snapshot once per beat and pushes it
 * to Rust, where the gated `memory` line (RF-perf-5) surfaces it beside the renderer RSS.
 *
 * Deliberately a plain module-level tally, NOT a store: counting must never trigger a
 * React re-render or subscribe a component, and a read must never traverse the tree.
 * Increment/decrement are O(1); the snapshot is a struct copy. Off the hot path by
 * construction — the only reader is the 30 s heartbeat (`renderer-heartbeat.ts`).
 *
 * This is the "cheap always-on counters, gated emit" guardrail: the counters run in
 * every build (their cost is negligible), while the *emission* stays behind RF-perf-5's
 * `memory` log gate on the Rust side — so production pays nothing but a few integers on
 * an interval that already fires.
 */

/** The heavy FE resource classes attributed at launch + steady state (RF-perf-4 AC-1), plus the
 *  one global CLI (counted, not emitted: it must stay one for the app's lifetime). */
export type InstanceKind = "editor" | "iframe" | "webglTerminal" | "team" | "cli";

const counts: Record<InstanceKind, number> = {
  editor: 0,
  iframe: 0,
  webglTerminal: 0,
  team: 0,
  cli: 0,
};

/**
 * Register one live instance of `kind`; returns an idempotent release to call on
 * unmount. The release is single-shot — a double invocation (defensive against a
 * caller that releases twice, e.g. a race between StrictMode cleanup and a manual
 * teardown) decrements only once, so the tally can never drift negative from a single
 * acquire.
 */
export function acquireInstance(kind: InstanceKind): () => void {
  counts[kind] += 1;
  let released = false;
  return () => {
    if (released) return;
    released = true;
    counts[kind] -= 1;
  };
}

/** The heartbeat payload shape (camelCase mirrors the Rust `FeInstanceCounts` serde). */
export interface InstanceCountsSnapshot {
  editors: number;
  iframes: number;
  webglTerminals: number;
  teams: number;
}

/** A point-in-time copy of the live counts for the heartbeat payload. */
export function snapshotInstanceCounts(): InstanceCountsSnapshot {
  return {
    editors: counts.editor,
    iframes: counts.iframe,
    webglTerminals: counts.webglTerminal,
    teams: counts.team,
  };
}

/** The live count of one kind (a test reads the CLI's, which must stay one). */
export function liveInstanceCount(kind: InstanceKind): number {
  return counts[kind];
}

/** Test-only: zero every counter between cases. */
export function __resetInstanceCountsForTest(): void {
  counts.editor = 0;
  counts.iframe = 0;
  counts.webglTerminal = 0;
  counts.team = 0;
  counts.cli = 0;
}
