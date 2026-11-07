// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

// The epic-nav actions' run() bodies resolve the active team via
// getActiveWorkspaceContext, guard on a focused workspace, then delegate to
// navigateEpicRail. Mock both so we can assert the wiring (team + direction +
// guard) that the capture-phase intercept relies on when it dispatches run().
// OC-16: a blocked rail move (non-grouped / no other eligible rail) reports
// `false`, and run() then flashes the active tile.
const getActiveWorkspaceContextMock = vi.fn();
vi.mock("@/shared/hooks/use-active-workspace-context", () => ({
  getActiveWorkspaceContext: () => getActiveWorkspaceContextMock(),
  // The CLI shows the active team in these tests.
}));

const navigateEpicRailMock = vi.fn();
vi.mock("@/features/viewer/lib/focus-cascade", async (importActual) => ({
  ...(await importActual<typeof import("@/features/viewer/lib/focus-cascade")>()),
  navigateEpicRail: (...args: unknown[]) => navigateEpicRailMock(...args),
}));

const flashBlockedNavForWorkspaceMock = vi.fn();
vi.mock("@/features/viewer/lib/blocked-nav-cue", () => ({
  flashBlockedNavForWorkspace: (...args: unknown[]) => flashBlockedNavForWorkspaceMock(...args),
}));

import { SHORTCUT_ACTIONS } from "./registry";

const TEAM = "acme:default";
function run(id: string): void {
  const action = SHORTCUT_ACTIONS.find((a) => a.id === id);
  if (!action) throw new Error(`${id} not registered`);
  action.run();
}

describe("viewer.cycle-epic-next/prev run() (OC-6/OC-16)", () => {
  beforeEach(() => vi.clearAllMocks());

  it("next: on a move, delegates to navigateEpicRail(teamKey, 'next') with no cue", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM, workspace: { id: "w" } });
    navigateEpicRailMock.mockReturnValue(true);
    run("viewer.cycle-epic-next");
    // Direction-sensitive: swapping the entries or the literal would fail here.
    expect(navigateEpicRailMock).toHaveBeenCalledWith(TEAM, "next");
    expect(flashBlockedNavForWorkspaceMock).not.toHaveBeenCalled();
  });

  it("prev delegates to navigateEpicRail(teamKey, 'prev')", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM, workspace: { id: "w" } });
    navigateEpicRailMock.mockReturnValue(true);
    run("viewer.cycle-epic-prev");
    expect(navigateEpicRailMock).toHaveBeenCalledWith(TEAM, "prev");
  });

  it("blocked rail move: flashes the active tile (pulse)", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM, workspace: { id: "w" } });
    navigateEpicRailMock.mockReturnValue(false);
    run("viewer.cycle-epic-next");
    expect(flashBlockedNavForWorkspaceMock).toHaveBeenCalledWith("w", "pulse");
  });

  it("no-ops when no workspace is focused (the !workspace guard)", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM, workspace: undefined });
    run("viewer.cycle-epic-next");
    run("viewer.cycle-epic-prev");
    // Removing the guard would fire navigateEpicRail in the empty/minimized state.
    expect(navigateEpicRailMock).not.toHaveBeenCalled();
    expect(flashBlockedNavForWorkspaceMock).not.toHaveBeenCalled();
  });
});
