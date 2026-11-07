// SPDX-License-Identifier: MIT
import { rendererHeartbeat } from "@/tauri/commands";
import { snapshotInstanceCounts } from "@/shared/lib/fe-instance-registry";
import { unhandledRejectionCount } from "./install-global-error-handlers";

/**
 * SW-FX-64 Layer 1 — frontend liveness ack for the Rust webview watchdog.
 *
 * The recurring "whole window blanks after hours, restart-only" symptom is the
 * WKWebView renderer process being OOM-killed by macOS — a death *below* React
 * that no error boundary or `window.onerror` can catch. Recovery is driven from
 * Rust (`renderer_watchdog.rs`), which reloads the webview when the renderer
 * stops proving it's alive. This module is that proof: a bare interval that
 * calls the `renderer_heartbeat` command every {@link RENDERER_HEARTBEAT_INTERVAL_MS}.
 *
 * Mounted at **module scope from `main.tsx`, outside the React tree**, on
 * purpose: a wedged/crashed subtree or an infinite render loop cannot starve a
 * timer that lives on the event loop rather than in a component. And a renderer
 * that is genuinely CPU-pegged will stop the timer too — which is exactly the
 * signal we want the watchdog to act on.
 *
 * Kept in sync with the Rust constant `RENDERER_ACK_INTERVAL_MS`.
 */
export const RENDERER_HEARTBEAT_INTERVAL_MS = 30_000;

let started = false;
let timer: ReturnType<typeof setInterval> | null = null;

/**
 * RF-perf-5 (AC-2): the FE JS-heap reading (`performance.memory.usedJSHeapSize`)
 * to ride the heartbeat, or `undefined` when the engine gates `performance.memory`
 * (WebKit historically does). This is only the JS-heap *subset* of the WebContent
 * renderer RSS — the Rust sampler derives the load-bearing non-JS gap from it.
 *
 * Deliberately defensive and total: every access is optional-chained and wrapped,
 * so a missing API or an unexpected shape yields `undefined` and never throws. The
 * heartbeat must stay infallible — a heap read that threw would break the liveness
 * ack and ironically trip the watchdog it feeds.
 */
export function readHeapBytes(): number | undefined {
  try {
    const mem = (performance as unknown as { memory?: { usedJSHeapSize?: number } }).memory;
    const bytes = mem?.usedJSHeapSize;
    return typeof bytes === "number" && Number.isFinite(bytes) ? bytes : undefined;
  } catch {
    return undefined;
  }
}

/**
 * RF-perf-7 (AC-2): the paint tick — a monotonic counter advanced by a
 * `requestAnimationFrame` probe, proving the renderer is still *painting*, not
 * merely that JS runs. `readHeapBytes` (heap) is `unavailable` on WebKit, so this
 * is the FE half of the blank-but-alive signal: a wedged / CPU-pegged tree stops
 * servicing rAF, so the tick freezes while the liveness ack keeps flowing —
 * exactly the case the ack-only watchdog cannot see.
 *
 * Exactly one probe is scheduled **per beat** (see {@link startRendererHeartbeat}),
 * never a self-sustaining 60 fps loop, so it adds no steady paint/battery cost —
 * a single rAF resolves within one frame on any healthy renderer, so by the next
 * 30 s beat the tick has advanced. It is only scheduled while the document is
 * visible: a hidden/occluded tab pauses rAF, and a paused tick there is expected,
 * not a stall (the Rust side gates render-stall on `visible`).
 */
let paintTick = 0;

/** RF-perf-7 (AC-2): the current paint tick, for the heartbeat payload. */
export function readPaintTick(): number {
  return paintTick;
}

/**
 * RF-perf-7 (AC-2): is the document currently visible? A hidden window pauses
 * rAF, so the watchdog must not read the (legitimately) frozen paint tick as a
 * stall. Defensive: any failure/absence reports `true` — combined with the Rust
 * `render_tick == 0` guard, an environment without `document`/rAF is treated as
 * "no paint proof", never as a stall.
 */
export function isDocumentVisible(): boolean {
  try {
    return typeof document === "undefined" || document.visibilityState === "visible";
  } catch {
    return true;
  }
}

/**
 * Whether the app root has nothing mounted: JS runs but the window is blank. The
 * watchdog logs it (never reloads on it), so the next blank window is diagnosable.
 * Total: a missing `document` reports `undefined` (no report), a missing root `true`.
 */
export function isAppRootEmpty(): boolean | undefined {
  try {
    if (typeof document === "undefined") return undefined;
    return (document.getElementById("root")?.childElementCount ?? 0) === 0;
  } catch {
    return undefined;
  }
}

/**
 * RF-perf-7 (AC-2): schedule one paint probe (the caller gates on visibility). The
 * rAF callback bumps {@link paintTick}; a wedged renderer never runs it, so the
 * tick freezes. Deliberately **stateless** — no "pending" latch: a probe whose
 * callback is silently dropped (e.g. a coalesced frame on an occluded-but-visible
 * window) must not wedge all future probes, which a sticky flag would do. One rAF
 * per beat, so a healthy renderer never accumulates more than one in flight; a
 * wedged one queues a few harmless callbacks that all resolve (advancing the tick =
 * healthy) once it recovers. Total and infallible — a missing/throwing rAF simply
 * leaves the tick where it is (Rust reads a frozen-but-nonzero tick as a stall only
 * while visible, and a never-advanced `0` tick as "no proof").
 */
function schedulePaintProbe(): void {
  if (typeof requestAnimationFrame !== "function") return;
  try {
    requestAnimationFrame(() => {
      paintTick += 1;
    });
  } catch {
    // A throwing rAF must never break the beat — the frozen tick is the signal.
  }
}

/**
 * Start the liveness heartbeat. Idempotent — a second call (StrictMode / HMR)
 * is a no-op, so only one interval is ever live. Fires an immediate ack so the
 * watchdog sees liveness right after boot, then repeats on the interval.
 */
export function startRendererHeartbeat(intervalMs: number = RENDERER_HEARTBEAT_INTERVAL_MS): void {
  if (started) return;
  started = true;

  // AC-4: the first beat of a fresh renderer instance announces itself (`init`)
  // so Rust can log a cause-tagged re-init breadcrumb. Every reload re-runs this
  // module (fresh JS context), so this fires once per renderer lifetime.
  let firstBeat = true;

  // Fire-and-forget: a rejected ack (e.g. IPC not ready in the first ms of
  // boot) must never surface as an unhandled rejection. The next tick retries.
  // The unhandled-rejection count (AC-4) is read from the single observer in
  // `install-global-error-handlers` (installed before this at boot) — one
  // listener, one counter, rather than a duplicate here.
  const beat = (): void => {
    const init = firstBeat;
    firstBeat = false;
    // RF-perf-7 (AC-2): report the paint tick reached by any probe scheduled on a
    // prior beat, plus current visibility, THEN schedule the next probe. Reporting
    // before scheduling means a healthy renderer shows an advanced tick next beat.
    const visible = isDocumentVisible();
    // RF-perf-4 (AC-1): an O(1) snapshot of the per-resource instance tally — the
    // per-class attribution the coarse RSS/heap number can't give. A plain struct
    // copy off the module-level counters; no tree traversal, no re-render.
    void rendererHeartbeat(
      readHeapBytes(),
      unhandledRejectionCount(),
      init,
      readPaintTick(),
      visible,
      snapshotInstanceCounts(),
      isAppRootEmpty(),
    ).catch(() => {
      // AC-4: if THIS beat's ack was lost (e.g. IPC not ready in the first ms of
      // boot — the exact cold-boot / post-reload case the breadcrumb exists to
      // observe), re-arm so the next beat re-announces the fresh renderer.
      // Otherwise the `init` flag is consumed by a beat that never reached Rust
      // and the cause-tagged re-init breadcrumb never fires for this lifetime.
      if (init) firstBeat = true;
    });
    // Only probe while visible: a hidden tab pauses rAF, and the Rust side already
    // treats a paused-while-hidden tick as expected, not a stall (R2).
    if (visible) schedulePaintProbe();
  };

  beat();
  timer = setInterval(beat, intervalMs);
}

/** Test-only: stop the interval and reset the idempotency guard + paint tick. */
export function __resetRendererHeartbeatForTest(): void {
  if (timer !== null) {
    clearInterval(timer);
    timer = null;
  }
  started = false;
  paintTick = 0;
}
