// SPDX-License-Identifier: MIT
/**
 * SR-0 (Q3) — measurement-only tracing of the `sync-wave-completed` FE
 * reaction. Answers "does a wave allocate native backing (xterm WebGL atlas,
 * Monaco/CodeMirror, iframes, mermaid SVG) that bmalloc
 * never returns?" by snapshotting the {@link snapshotInstanceCounts} tally at
 * wave receipt and again after the reaction's 150 ms leading debounce settles,
 * then emitting the `wave_id`-keyed delta.
 *
 * Entirely gated behind {@link VIEWER_PERF.traceWaveReaction} (off by default):
 * when off, {@link traceWaveReaction} returns on its first line, so the wave
 * listener does ZERO extra work. When on, the only cost is a synchronous
 * module-level counter read plus one deferred read — never a new IPC channel,
 * never a re-render, never synchronous work in the listener itself.
 *
 * Output goes to `console.debug` with a stable `[sr0-wave-reaction]` prefix the
 * SR-0 soak harness greps from the renderer console (captured via Playwright
 * while driving the dev build). The aggregate RAM ratchet is measured
 * separately from the Rust `memory` line's `fe_*` fields — no FE code needed.
 */
import {
  snapshotInstanceCounts,
  type InstanceCountsSnapshot,
} from "@/shared/lib/fe-instance-registry";
import { VIEWER_PERF } from "@/shared/lib/viewer-perf-config";

/**
 * Milliseconds to wait before the second (settled) snapshot. Must exceed the
 * `useCompositeOverlayRefresh` 150 ms leading debounce so the reaction's
 * re-renders (and any native-backing allocation they drive) have committed.
 */
const SETTLE_MS = 200;

/** Stable log prefix — a wire contract with `scripts/perf/sr0-scale-soak.mjs`. */
const PREFIX = "[sr0-wave-reaction]";

function delta(
  before: InstanceCountsSnapshot,
  after: InstanceCountsSnapshot,
): InstanceCountsSnapshot {
  return {
    editors: after.editors - before.editors,
    iframes: after.iframes - before.iframes,
    webglTerminals: after.webglTerminals - before.webglTerminals,
    teams: after.teams - before.teams,
  };
}

/**
 * Begin tracing one wave's FE reaction. No-op unless
 * {@link VIEWER_PERF.traceWaveReaction} is on. Safe to call for every wave
 * (including no-op waves) — the caller need not pre-check the flag.
 */
export function traceWaveReaction(waveId: number | undefined): void {
  if (!VIEWER_PERF.traceWaveReaction || waveId === undefined) return;
  // `performance`/`setTimeout` are always present in the renderer; guarded only
  // so a non-DOM test importing this module can't throw.
  if (typeof performance === "undefined" || typeof setTimeout === "undefined") {
    return;
  }

  const before = snapshotInstanceCounts();
  const startMark = `sr0-wave-${waveId}-start`;
  const settledMark = `sr0-wave-${waveId}-settled`;
  performance.mark(startMark);

  setTimeout(() => {
    const after = snapshotInstanceCounts();
    performance.mark(settledMark);
    try {
      performance.measure(`sr0-wave-${waveId}`, startMark, settledMark);
    } catch {
      // Marks can be evicted under buffer pressure — the delta below is the
      // load-bearing signal, so a missing measure is non-fatal.
    }
    const d = delta(before, after);
    console.debug(
      `${PREFIX} wave_id=${waveId} d_editors=${d.editors} d_iframes=${d.iframes} ` +
        `d_webgl=${d.webglTerminals} d_teams=${d.teams}`,
    );
  }, SETTLE_MS);
}
