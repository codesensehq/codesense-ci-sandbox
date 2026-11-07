// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { readFileSync } from "fs";
import { resolve } from "path";

// Controllable ack command so the test can count invocations without touching
// the real Tauri IPC bridge.
const commandsMock = vi.hoisted(() => ({
  rendererHeartbeat: vi.fn(
    (
      _heapBytes?: number,
      _unhandledRejections?: number,
      _init?: boolean,
      _renderTick?: number,
      _visible?: boolean,
      _instanceCounts?: unknown,
      _rootEmpty?: boolean,
    ) => Promise.resolve(),
  ),
}));
vi.mock("@/tauri/commands", () => commandsMock);

import {
  startRendererHeartbeat,
  __resetRendererHeartbeatForTest,
  readHeapBytes,
  readPaintTick,
  isDocumentVisible,
  isAppRootEmpty,
  RENDERER_HEARTBEAT_INTERVAL_MS,
} from "./renderer-heartbeat";

describe("renderer heartbeat", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    commandsMock.rendererHeartbeat.mockClear();
    __resetRendererHeartbeatForTest();
  });

  afterEach(() => {
    __resetRendererHeartbeatForTest();
    vi.useRealTimers();
  });

  it("acks immediately on start, then on each interval", () => {
    startRendererHeartbeat(1000);
    // Immediate first ack so the watchdog sees liveness right after boot.
    expect(commandsMock.rendererHeartbeat).toHaveBeenCalledTimes(1);

    vi.advanceTimersByTime(1000);
    expect(commandsMock.rendererHeartbeat).toHaveBeenCalledTimes(2);

    vi.advanceTimersByTime(3000);
    expect(commandsMock.rendererHeartbeat).toHaveBeenCalledTimes(5);
  });

  it("is idempotent — a second start does not add a second interval", () => {
    startRendererHeartbeat(1000);
    startRendererHeartbeat(1000);
    // Only one immediate ack (second call is a no-op).
    expect(commandsMock.rendererHeartbeat).toHaveBeenCalledTimes(1);

    vi.advanceTimersByTime(1000);
    // One tick → one ack (not two), proving a single interval is live.
    expect(commandsMock.rendererHeartbeat).toHaveBeenCalledTimes(2);
  });

  it("swallows a rejected ack without an unhandled rejection", async () => {
    commandsMock.rendererHeartbeat.mockReturnValueOnce(Promise.reject(new Error("ipc down")));
    // Must not throw synchronously and the rejection must be caught internally.
    expect(() => startRendererHeartbeat(1000)).not.toThrow();
    await Promise.resolve();
    expect(commandsMock.rendererHeartbeat).toHaveBeenCalledTimes(1);
  });

  it("stops acking after reset", () => {
    startRendererHeartbeat(1000);
    expect(commandsMock.rendererHeartbeat).toHaveBeenCalledTimes(1);
    __resetRendererHeartbeatForTest();
    vi.advanceTimersByTime(5000);
    // No further acks after the interval is cleared.
    expect(commandsMock.rendererHeartbeat).toHaveBeenCalledTimes(1);
  });

  it("defaults to the 30s interval constant", () => {
    expect(RENDERER_HEARTBEAT_INTERVAL_MS).toBe(30_000);
    startRendererHeartbeat();
    vi.advanceTimersByTime(RENDERER_HEARTBEAT_INTERVAL_MS);
    expect(commandsMock.rendererHeartbeat).toHaveBeenCalledTimes(2);
  });

  // RF-perf-5 (AC-4): the first beat of a fresh renderer instance announces
  // itself (`init: true`) so Rust logs a cause-tagged re-init breadcrumb; every
  // subsequent beat is `false`.
  it("marks only the first beat as init", () => {
    startRendererHeartbeat(1000);
    vi.advanceTimersByTime(2000);
    // Beat 1 (immediate) is init; beats 2 and 3 are not.
    expect(commandsMock.rendererHeartbeat.mock.calls[0][2]).toBe(true);
    expect(commandsMock.rendererHeartbeat.mock.calls[1][2]).toBe(false);
    expect(commandsMock.rendererHeartbeat.mock.calls[2][2]).toBe(false);
  });

  // RF-perf-5 (AC-2/AC-4): the beat forwards a heap reading (or undefined) and a
  // numeric unhandled-rejection count.
  it("forwards heap + unhandled-rejection count on the beat", () => {
    startRendererHeartbeat(1000);
    const [heap, rejections] = commandsMock.rendererHeartbeat.mock.calls[0];
    expect(heap === undefined || typeof heap === "number").toBe(true);
    expect(typeof rejections).toBe("number");
  });

  // RF-perf-4 (AC-1): the beat forwards the per-resource FE instance-count snapshot
  // (arg index 5) so the gated Rust memory line can attribute the renderer footprint
  // by surface class. With no components mounted in this test the tally is all-zeros.
  it("forwards the instance-count snapshot on the beat", () => {
    startRendererHeartbeat(1000);
    const counts = commandsMock.rendererHeartbeat.mock.calls[0][5];
    expect(counts).toEqual({
      editors: expect.any(Number),
      iframes: expect.any(Number),
      webglTerminals: expect.any(Number),
      teams: expect.any(Number),
    });
  });

  // The beat says whether the app root is empty (arg index 6), so a blank window
  // is logged with its signals instead of leaving nothing to diagnose.
  it("forwards whether the app root is empty on the beat", () => {
    const root = document.createElement("div");
    root.id = "root";
    document.body.appendChild(root);
    try {
      startRendererHeartbeat(1000);
      expect(commandsMock.rendererHeartbeat.mock.calls[0][6]).toBe(true);
      root.appendChild(document.createElement("main"));
      vi.advanceTimersByTime(1000);
      expect(commandsMock.rendererHeartbeat.mock.calls[1][6]).toBe(false);
    } finally {
      root.remove();
    }
  });

  // RF-perf-5 (AC-4): if the FIRST beat's ack is lost (IPC not ready at boot),
  // the `init` flag must NOT be consumed — the next beat re-announces the fresh
  // renderer, else the cause-tagged breadcrumb never fires for this lifetime.
  it("re-sends init on the next beat when the first ack fails", async () => {
    commandsMock.rendererHeartbeat.mockReturnValueOnce(Promise.reject(new Error("ipc down")));
    startRendererHeartbeat(1000);
    // Flush the rejected ack's `.catch` so `firstBeat` is re-armed.
    await Promise.resolve();
    await Promise.resolve();
    vi.advanceTimersByTime(1000);
    const calls = commandsMock.rendererHeartbeat.mock.calls;
    expect(calls[0][2]).toBe(true); // first (failed) beat carried init
    expect(calls[1][2]).toBe(true); // retry re-announces init — not lost
  });

  // Cross-language drift guard: the FE ack cadence must not exceed the Rust
  // watchdog's stale window (3× the ack interval). If someone bumps the FE
  // interval past that, healthy renderers would false-trip a reload. Assert the
  // two source-of-truth constants stay in lockstep (same tripwire style as the
  // Tauri IPC contract test reading lib.rs).
  it("stays in sync with the Rust RENDERER_ACK_INTERVAL_MS constant", () => {
    const rust = readFileSync(
      resolve(__dirname, "../../src-tauri/src/renderer_watchdog.rs"),
      "utf-8",
    );
    const match = rust.match(/RENDERER_ACK_INTERVAL_MS:\s*u64\s*=\s*([\d_]+)/);
    expect(match, "RENDERER_ACK_INTERVAL_MS not found in renderer_watchdog.rs").toBeTruthy();
    const rustMs = Number(match![1].replace(/_/g, ""));
    expect(rustMs).toBe(RENDERER_HEARTBEAT_INTERVAL_MS);
  });
});

// RF-perf-5 (AC-2): the FE heap read must be defensive — WebKit historically
// gates `performance.memory`, so a missing/odd shape must degrade to `undefined`
// and never throw (the beat stays infallible).
describe("readHeapBytes", () => {
  const perf = performance as unknown as { memory?: unknown };

  afterEach(() => {
    delete perf.memory;
  });

  it("returns undefined when performance.memory is unavailable", () => {
    delete perf.memory;
    expect(readHeapBytes()).toBeUndefined();
  });

  it("returns usedJSHeapSize when present", () => {
    perf.memory = { usedJSHeapSize: 4242 };
    expect(readHeapBytes()).toBe(4242);
  });

  it("returns undefined for a non-numeric or missing heap value", () => {
    perf.memory = { usedJSHeapSize: "big" };
    expect(readHeapBytes()).toBeUndefined();
    perf.memory = {};
    expect(readHeapBytes()).toBeUndefined();
  });
});

// RF-perf-7 (AC-2): the paint-tick probe proves the renderer is still painting,
// distinct from "JS runs". A wedged tree stops servicing rAF, so the tick freezes
// while the ack keeps flowing — the blank-but-alive signal. The probe is
// visibility-gated so a hidden tab (rAF paused) never reads as a stall (R2).
describe("renderer heartbeat paint probe (RF-perf-7)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    commandsMock.rendererHeartbeat.mockClear();
    __resetRendererHeartbeatForTest();
  });

  afterEach(() => {
    __resetRendererHeartbeatForTest();
    vi.unstubAllGlobals();
    vi.useRealTimers();
    delete (globalThis as { __vis?: unknown }).__vis;
  });

  // Force `document.visibilityState` via a configurable getter the helper reads.
  function setVisibility(state: "visible" | "hidden"): void {
    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      get: () => state,
    });
  }

  it("advances the paint tick across beats when rAF fires", () => {
    // rAF that runs its callback immediately → the probe completes within the beat.
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      cb(0);
      return 1;
    });
    setVisibility("visible");

    startRendererHeartbeat(1000); // beat 1: reports tick 0, schedules → tick 1
    vi.advanceTimersByTime(1000); // beat 2: reports tick 1, schedules → tick 2
    vi.advanceTimersByTime(1000); // beat 3: reports tick 2

    const calls = commandsMock.rendererHeartbeat.mock.calls;
    // Payload arg order: [heap, rejections, init, renderTick, visible].
    expect(calls[0][3]).toBe(0);
    expect(calls[1][3]).toBe(1);
    expect(calls[2][3]).toBe(2);
    expect(calls[0][4]).toBe(true);
    expect(readPaintTick()).toBe(3);
  });

  it("freezes the paint tick when rAF never fires (wedged renderer)", () => {
    // rAF that never invokes its callback → the tick cannot advance.
    vi.stubGlobal(
      "requestAnimationFrame",
      vi.fn(() => 1),
    );
    setVisibility("visible");

    startRendererHeartbeat(1000);
    vi.advanceTimersByTime(3000);

    const calls = commandsMock.rendererHeartbeat.mock.calls;
    // Every beat reports a frozen tick — the signal the watchdog acts on.
    for (const call of calls) expect(call[3]).toBe(0);
    expect(readPaintTick()).toBe(0);
  });

  it("reports visible=false and schedules no probe while hidden", () => {
    const raf = vi.fn((cb: FrameRequestCallback) => {
      cb(0);
      return 1;
    });
    vi.stubGlobal("requestAnimationFrame", raf);
    setVisibility("hidden");

    startRendererHeartbeat(1000);
    vi.advanceTimersByTime(2000);

    const calls = commandsMock.rendererHeartbeat.mock.calls;
    for (const call of calls) expect(call[4]).toBe(false);
    // A hidden tab pauses rAF — the probe must not be scheduled, so no advance.
    expect(raf).not.toHaveBeenCalled();
    expect(readPaintTick()).toBe(0);
  });

  it("stays infallible when rAF throws", () => {
    vi.stubGlobal("requestAnimationFrame", () => {
      throw new Error("raf exploded");
    });
    setVisibility("visible");

    expect(() => startRendererHeartbeat(1000)).not.toThrow();
    // The ack still went out despite the probe blowing up.
    expect(commandsMock.rendererHeartbeat).toHaveBeenCalledTimes(1);
  });
});

// RF-perf-7 (AC-2): visibility read is defensive — any failure/absence reports
// `true`, which (with the Rust `render_tick == 0` guard) degrades to "no proof",
// never a fabricated stall.
describe("isDocumentVisible", () => {
  afterEach(() => {
    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      get: () => "visible",
    });
  });

  it("is true when the document is visible", () => {
    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      get: () => "visible",
    });
    expect(isDocumentVisible()).toBe(true);
  });

  it("is false when the document is hidden", () => {
    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      get: () => "hidden",
    });
    expect(isDocumentVisible()).toBe(false);
  });
});

describe("isAppRootEmpty", () => {
  it("is true when there is no app root", () => {
    expect(isAppRootEmpty()).toBe(true);
  });

  it("is false once something is mounted in the app root", () => {
    const root = document.createElement("div");
    root.id = "root";
    root.appendChild(document.createElement("main"));
    document.body.appendChild(root);
    try {
      expect(isAppRootEmpty()).toBe(false);
    } finally {
      root.remove();
    }
  });
});
