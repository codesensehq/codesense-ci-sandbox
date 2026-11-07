// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { traceWaveReaction } from "@/shared/lib/sr0-wave-trace";
import { VIEWER_PERF } from "@/shared/lib/viewer-perf-config";
import { __resetInstanceCountsForTest, acquireInstance } from "@/shared/lib/fe-instance-registry";

describe("traceWaveReaction (SR-0 Q3)", () => {
  beforeEach(() => {
    __resetInstanceCountsForTest();
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
    // Restore the default OFF state if a test flipped it.
    (VIEWER_PERF as { traceWaveReaction: boolean }).traceWaveReaction = false;
  });

  it("is a no-op when the flag is off (default) — nothing logged, no timer scheduled", () => {
    const debug = vi.spyOn(console, "debug").mockImplementation(() => {});
    traceWaveReaction(1);
    // Even after the settle window, nothing runs.
    vi.advanceTimersByTime(1000);
    expect(debug).not.toHaveBeenCalled();
  });

  it("is a no-op when wave_id is undefined even with the flag on", () => {
    (VIEWER_PERF as { traceWaveReaction: boolean }).traceWaveReaction = true;
    const debug = vi.spyOn(console, "debug").mockImplementation(() => {});
    traceWaveReaction(undefined);
    vi.advanceTimersByTime(1000);
    expect(debug).not.toHaveBeenCalled();
  });

  it("emits a wave_id-keyed instance-count delta after the settle window when on", () => {
    (VIEWER_PERF as { traceWaveReaction: boolean }).traceWaveReaction = true;
    const debug = vi.spyOn(console, "debug").mockImplementation(() => {});

    traceWaveReaction(42);
    // Simulate the wave's reaction allocating native backing between the two
    // snapshots (e.g. a mermaid iframe + an editor mount).
    const releaseIframe = acquireInstance("iframe");
    acquireInstance("editor");

    // Nothing logged until the settle window elapses.
    expect(debug).not.toHaveBeenCalled();
    vi.advanceTimersByTime(250);

    expect(debug).toHaveBeenCalledTimes(1);
    const line = debug.mock.calls[0][0] as string;
    expect(line).toContain("[sr0-wave-reaction]");
    expect(line).toContain("wave_id=42");
    expect(line).toContain("d_iframes=1");
    expect(line).toContain("d_editors=1");
    expect(line).toContain("d_teams=0");
    // Keep the closure referenced so the release fn isn't flagged unused.
    expect(typeof releaseIframe).toBe("function");
  });
});
