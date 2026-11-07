// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

import { useViewerStore } from "./viewer-store";

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
}));

// SLW-DV-7 (AC-2): the per-workspace working-diff comparison basis.
describe("setSourceDiffBaseMode (SLW-DV-7)", () => {
  beforeEach(() => {
    useViewerStore.setState({ sourceDiffBaseMode: {}, sourceDiffShownBySession: {} });
    vi.clearAllMocks();
  });

  it("defaults to branch-base (absent) and flips to head", () => {
    const set = useViewerStore.getState().setSourceDiffBaseMode;
    expect(useViewerStore.getState().sourceDiffBaseMode.w1).toBeUndefined();
    set("w1", "head");
    expect(useViewerStore.getState().sourceDiffBaseMode.w1).toBe("head");
    set("w1", "branchBase");
    expect(useViewerStore.getState().sourceDiffBaseMode.w1).toBe("branchBase");
  });

  it("is a change-only no-op when the mode is unchanged", () => {
    const set = useViewerStore.getState().setSourceDiffBaseMode;
    set("w1", "head");
    const before = useViewerStore.getState();
    set("w1", "head");
    // Same object identity → no re-render churn (mirrors the sibling setters).
    expect(useViewerStore.getState()).toBe(before);
  });

  it("keeps modes independent per workspace", () => {
    const set = useViewerStore.getState().setSourceDiffBaseMode;
    set("w1", "head");
    expect(useViewerStore.getState().sourceDiffBaseMode.w2).toBeUndefined();
  });

  it("persists across a hide (session memory, like sourceDiffHistoryMode)", () => {
    const s = useViewerStore.getState();
    s.setSourceDiffBaseMode("w1", "head");
    useViewerStore.setState({ sourceDiffShownBySession: { sid1: true } });
    s.toggleSourceDiffShown({
      sessionId: "sid1",
      workspaceId: "w1",
      teamKey: "team-a",
      shown: false,
    });
    expect(useViewerStore.getState().sourceDiffShownBySession.sid1).toBe(false);
    // Hiding does NOT reset the basis — a re-open resumes it (cleared only on close).
    expect(useViewerStore.getState().sourceDiffBaseMode.w1).toBe("head");
  });
});
