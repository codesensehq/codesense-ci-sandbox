// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";

import { closeActiveFileTab } from "./close-active-file-tab";

const mockGetActiveWorkspaceContext = vi.fn();
vi.mock("@/shared/hooks/use-active-workspace-context", () => ({
  getActiveWorkspaceContext: () => mockGetActiveWorkspaceContext(),
}));

const mockCloseTab = vi.fn();
vi.mock("@/shared/stores/viewer-store", () => ({
  useViewerStore: { getState: () => ({ closeTab: mockCloseTab }) },
}));

beforeEach(() => {
  vi.clearAllMocks();
});

describe("closeActiveFileTab (SW-FX-38 single source of truth)", () => {
  it("closes with the resolved synthetic repoPath when the workspace has one", () => {
    mockGetActiveWorkspaceContext.mockReturnValue({
      teamKey: "acme:default",
      repoPath: "workspace:acme:default:acme/x",
    });

    closeActiveFileTab("readme.md");

    expect(mockCloseTab).toHaveBeenCalledWith(
      "acme:default",
      "workspace:acme:default:acme/x",
      "readme.md",
      { user: true },
    );
  });

  it("falls back to '' repoPath in a slug-less workspace (repoPath null)", () => {
    mockGetActiveWorkspaceContext.mockReturnValue({ teamKey: "acme:default", repoPath: null });

    closeActiveFileTab("notes.md");

    // repoPath is NEVER a precondition — the tab still closes.
    expect(mockCloseTab).toHaveBeenCalledWith("acme:default", "", "notes.md", { user: true });
  });

  it("no-ops on an empty/absent filePath", () => {
    mockGetActiveWorkspaceContext.mockReturnValue({ teamKey: "acme:default", repoPath: null });

    closeActiveFileTab("");
    closeActiveFileTab(null);
    closeActiveFileTab(undefined);

    expect(mockCloseTab).not.toHaveBeenCalled();
  });
});
