// SPDX-License-Identifier: MIT
import { afterEach, expect, it, vi } from "vitest";
import { registerFrame, acknowledgeFlush } from "./frame-lifecycle";
import { flushBeforeQuit } from "./quit-flush";
import { useViewerStore } from "@/shared/stores/viewer-store";
const off: Array<() => void> = [];
afterEach(() => {
  off.splice(0).forEach((dispose) => dispose());
  vi.restoreAllMocks();
  vi.useRealTimers();
});
it("runs frames, notes and layout together and bounds a stuck durability write at 500ms", async () => {
  vi.useFakeTimers();
  const persist = vi
    .spyOn(useViewerStore.getState(), "flushPersist")
    .mockImplementation(() => new Promise(() => {}));
  const notes = vi.spyOn(useViewerStore.getState(), "flushNotes").mockResolvedValue();
  const post = vi.fn();
  off.push(registerFrame({ frameId: "quit", dashboard: "Quit", post, hide: vi.fn() }));
  const done = vi.fn();
  const pending = flushBeforeQuit().then(done);
  expect(persist).toHaveBeenCalledOnce();
  expect(notes).toHaveBeenCalledOnce();
  acknowledgeFlush("quit", post.mock.calls[0][0].id);
  await vi.advanceTimersByTimeAsync(499);
  expect(done).not.toHaveBeenCalled();
  await vi.advanceTimersByTimeAsync(1);
  await pending;
  expect(done).toHaveBeenCalledOnce();
});
