// SPDX-License-Identifier: MIT
import { afterEach, expect, it, vi } from "vitest";
import { ReloadStateCapture } from "./reload-state";
import { logger } from "@/shared/lib/logger";

afterEach(() => {
  vi.restoreAllMocks();
  vi.useRealTimers();
});
it("resolves absent state immediately without waiting for the deadline", async () => {
  vi.useFakeTimers();
  const post = vi.fn();
  const capture = new ReloadStateCapture(post, "frame-a");
  const result = capture.capture();
  capture.receive(post.mock.calls[0][0].id, null);
  expect(await result).toBeNull();
  expect(vi.getTimerCount()).toBe(0);
});
it.each(["oversize", "late"])(
  "drops %s state once and logs both host and frame warning with document identity",
  async (mode) => {
    vi.useFakeTimers();
    const log = vi.spyOn(logger, "warn").mockImplementation(() => {});
    log.mockClear();
    const post = vi.fn();
    const capture = new ReloadStateCapture(post, "frame-a/document-a");
    const result = capture.capture();
    const id = post.mock.calls[0][0].id;
    if (mode === "late") await vi.advanceTimersByTimeAsync(100);
    else capture.receive(id, { draft: "é".repeat(32768) });
    expect(await result).toBeNull();
    capture.receive(id, { late: true });
    expect(log).toHaveBeenCalledTimes(1);
    expect(log).toHaveBeenCalledWith("pane-reload", expect.stringContaining("frame-a/document-a"));
    expect(post.mock.calls.at(-1)?.[0]).toMatchObject({ type: "codesense:reload-warning", id });
    expect(vi.getTimerCount()).toBe(0);
  },
);
it("unmount cancellation settles capture and clears its timer without a warning", async () => {
  vi.useFakeTimers();
  const log = vi.spyOn(logger, "warn").mockImplementation(() => {});
  log.mockClear();
  const capture = new ReloadStateCapture(vi.fn(), "frame-a");
  const result = capture.capture();
  capture.cancel();
  expect(await result).toBeNull();
  expect(vi.getTimerCount()).toBe(0);
  expect(log).not.toHaveBeenCalled();
});
