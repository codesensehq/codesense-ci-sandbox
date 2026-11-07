// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

// The action resolves the active team via getActiveWorkspaceContext (the shared
// team-scoped idiom); mock it to drive both the happy path and the NO_TEAM guard.
const getActiveWorkspaceContextMock = vi.fn();
vi.mock("@/shared/hooks/use-active-workspace-context", () => ({
  getActiveWorkspaceContext: () => getActiveWorkspaceContextMock(),
  // The CLI shows the active team in these tests.
}));

import { NO_TEAM_SENTINEL } from "@/shared/lib/team-key";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { SHORTCUT_ACTIONS } from "./registry";

function runEditFile() {
  const action = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.edit-file");
  if (!action) throw new Error("viewer.edit-file not registered");
  action.run();
}

describe("SW-VE-8 viewer.edit-file shortcut action", () => {
  beforeEach(() => {
    useViewerStore.setState({ editFileRequest: null });
    vi.clearAllMocks();
  });

  it("is registered as a rebindable, menu-visible ⌘E action", () => {
    const action = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.edit-file");
    expect(action).toBeDefined();
    expect(action?.defaultKeystroke).toBe("CmdOrCtrl+E");
    expect(action?.menuPath).toEqual(["File", "Edit File"]);
    expect(action?.scope).toBe("team");
  });

  it("run() requests edit on the active team's file", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: "org:team-a" });
    runEditFile();
    expect(useViewerStore.getState().editFileRequest).toEqual({ nonce: 1, teamKey: "org:team-a" });
  });

  it("run() is a no-op when no team is active (NO_TEAM guard)", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: NO_TEAM_SENTINEL });
    runEditFile();
    expect(useViewerStore.getState().editFileRequest).toBeNull();
  });

  it("does NOT collide with the export-pdf shortcut (⌘⇧E)", () => {
    const exportPdf = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.export-pdf");
    expect(exportPdf?.defaultKeystroke).toBe("CmdOrCtrl+Shift+E");
    // Edit is plain ⌘E; export is ⌘⇧E — distinct keystrokes.
    expect(exportPdf?.defaultKeystroke).not.toBe("CmdOrCtrl+E");
  });
});
