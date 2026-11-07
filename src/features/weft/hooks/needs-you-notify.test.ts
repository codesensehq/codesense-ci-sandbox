// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

// ── Window seam ──
const unminimize = vi.fn(() => Promise.resolve());
const show = vi.fn(() => Promise.resolve());
const setFocus = vi.fn(() => Promise.resolve());
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ unminimize, show, setFocus }),
}));

// ── Reveal seam ──
const revealSlot = vi.fn();
vi.mock("@/features/shell/slots/reveal-slot", () => ({
  revealSlot: (...args: unknown[]) => revealSlot(...args),
}));

import { focusAndRevealBoard } from "./needs-you-notify";

describe("SW-WA-E11 · the needs-you landing target", () => {
  beforeEach(() => vi.clearAllMocks());

  it("brings the window forward and reveals the Activity board", async () => {
    await focusAndRevealBoard();
    expect(unminimize).toHaveBeenCalled();
    expect(show).toHaveBeenCalled();
    expect(setFocus).toHaveBeenCalled();
    expect(revealSlot).toHaveBeenCalledWith("right-pane", {
      surface: "inbox",
      intent: { focusComposer: false },
    });
  });

  it("still reveals the board when a window call fails", async () => {
    setFocus.mockImplementationOnce(() => Promise.reject(new Error("no")));
    await focusAndRevealBoard();
    expect(revealSlot).toHaveBeenCalledWith("right-pane", {
      surface: "inbox",
      intent: { focusComposer: false },
    });
  });
});
