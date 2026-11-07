// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook } from "@testing-library/react";

const mockOpenStoryDocForTeam = vi.fn();

vi.mock("@/shared/stores/viewer-store", () => ({
  useViewerStore: Object.assign(
    (selector: (s: Record<string, unknown>) => unknown) =>
      selector({
        switchWorkspace: vi.fn(),
        requestCloseWorkspace: vi.fn(),
        closeWorkspace: vi.fn(),
        changeStoryStatus: vi.fn(),
      }),
    { getState: () => ({ openStoryDocForTeam: mockOpenStoryDocForTeam }) },
  ),
}));

vi.mock("@/shared/lib/open-in-editor", () => ({
  openInEditorFromCwd: vi.fn(),
}));

import { useWorkspaceBarCallbacks } from "./use-workspace-bar-callbacks";

beforeEach(() => {
  vi.clearAllMocks();
});

describe("useWorkspaceBarCallbacks (SW-FX-10)", () => {
  it("AC-6: onOpenStoryDoc routes the CLI workspace-tab double-click through openStoryDocForTeam", () => {
    const { result } = renderHook(() => useWorkspaceBarCallbacks("org:default"));

    result.current.onOpenStoryDoc("/repo/stories/W-5.md");

    // Delegates to the shared store action (which resolves the team's content
    // anchor internally) — so all story-bearing surfaces share one code path.
    expect(mockOpenStoryDocForTeam).toHaveBeenCalledWith("org:default", "/repo/stories/W-5.md");
  });
});
