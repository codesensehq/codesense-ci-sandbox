// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import {
  isOpenTimingEnabled,
  startOpenTimer,
  beginOpenSpan,
  OPEN_TIMING_KEY,
} from "./open-path-timing";
import { logger } from "@/shared/lib/logger";

describe("open-path-timing (SW-SO-2 ① — AC-1 measurement gate)", () => {
  let infoSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    localStorage.clear();
    infoSpy = vi.spyOn(logger, "info").mockImplementation(() => {});
  });
  afterEach(() => {
    infoSpy.mockRestore();
    localStorage.clear();
  });

  it("is OFF by default — zero-cost, no logging, no timer/span", () => {
    expect(isOpenTimingEnabled()).toBe(false);
    expect(startOpenTimer("s")).toBeNull();
    // beginOpenSpan returns null so the caller's `done?.()` is a no-op.
    const clock = vi.fn(() => 0);
    expect(beginOpenSpan("x", clock)).toBeNull();
    expect(clock).not.toHaveBeenCalled();
    expect(infoSpy).not.toHaveBeenCalled();
  });

  it("enables via the localStorage flag", () => {
    localStorage.setItem(OPEN_TIMING_KEY, "1");
    expect(isOpenTimingEnabled()).toBe(true);
    expect(startOpenTimer("s")).not.toBeNull();
  });

  it("reports a ranked (slowest-first) per-phase breakdown + total", () => {
    localStorage.setItem(OPEN_TIMING_KEY, "1");
    // Injected clock: readFile 5ms, findOnDisk 30ms, config 2ms.
    const ticks = [0, 5, 35, 37];
    let i = 0;
    const now = () => ticks[i++];
    const timer = startOpenTimer("story.md", now)!;
    timer.mark("readFile");
    timer.mark("findWorktreeOnDisk");
    timer.mark("probeWorktreeConfig");
    timer.report();

    expect(infoSpy).toHaveBeenCalledTimes(1);
    const [, line] = infoSpy.mock.calls[0] as [string, string];
    expect(line).toContain("open(story.md)");
    expect(line).toContain("total=37.0ms");
    // Slowest first: findWorktreeOnDisk (30) > readFile (5) > probeWorktreeConfig (2).
    expect(line.indexOf("findWorktreeOnDisk=30.0ms")).toBeLessThan(line.indexOf("readFile=5.0ms"));
    expect(line.indexOf("readFile=5.0ms")).toBeLessThan(line.indexOf("probeWorktreeConfig=2.0ms"));
  });

  it("beginOpenSpan logs the elapsed span duration when done() is called", () => {
    localStorage.setItem(OPEN_TIMING_KEY, "1");
    const ticks = [100, 112];
    let i = 0;
    const done = beginOpenSpan("xterm construction", () => ticks[i++]);
    expect(done).not.toBeNull();
    done!();
    expect(infoSpy).toHaveBeenCalledWith("open-timing", "xterm construction=12.0ms");
  });
});
