// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

// The directional pane-focus actions' run() bodies resolve the active team via
// getActiveWorkspaceContext, guard on a focused workspace, then delegate to
// focusAdjacentPane. OC-16: a blocked move (one-tile / edge / zoomed) reports
// `false`, and run() then nudges the active tile toward the pressed direction.
const getActiveWorkspaceContextMock = vi.fn();
vi.mock("@/shared/hooks/use-active-workspace-context", () => ({
  getActiveWorkspaceContext: () => getActiveWorkspaceContextMock(),
  // The CLI shows the active team in these tests.
}));

const focusAdjacentPaneMock = vi.fn();
vi.mock("@/features/viewer/lib/directional-focus", async (importActual) => ({
  ...(await importActual<typeof import("@/features/viewer/lib/directional-focus")>()),
  focusAdjacentPane: (...args: unknown[]) => focusAdjacentPaneMock(...args),
}));

const switchWorkspaceMock = vi.fn();
const setActiveTerminalTabMock = vi.fn();
const expandedGridWorkspaceIdMock = vi.fn((): string | null => null);
const storeState = {
  switchWorkspace: switchWorkspaceMock,
  setActiveTerminalTab: setActiveTerminalTabMock,
  activeTabByWorkspace: {} as Record<string, Record<string, string>>,
  panedByWorkspace: {} as Record<string, Record<string, string[]>>,
};
vi.mock("@/shared/stores/viewer-store", () => ({
  useViewerStore: { subscribe: () => () => {}, getState: () => storeState },
  expandedGridWorkspaceId: () => expandedGridWorkspaceIdMock(),
  workspacePanedTabIds: (
    st: { panedByWorkspace: Record<string, Record<string, string[]>> },
    teamKey: string,
    workspaceId: string,
  ) => st.panedByWorkspace[teamKey]?.[workspaceId] ?? [],
  selectActiveTerminalTab: (
    st: { activeTabByWorkspace: Record<string, Record<string, string>> },
    teamKey: string,
    workspaceId: string,
  ) => st.activeTabByWorkspace[teamKey]?.[workspaceId],
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

const DIRS = [
  { id: "viewer.focus-pane-left", dir: "left" },
  { id: "viewer.focus-pane-right", dir: "right" },
  { id: "viewer.focus-pane-up", dir: "up" },
  { id: "viewer.focus-pane-down", dir: "down" },
] as const;

describe("viewer.focus-pane-* run() (SW-EW-33/OC-16)", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    expandedGridWorkspaceIdMock.mockReturnValue(null);
    storeState.activeTabByWorkspace = {};
    storeState.panedByWorkspace = {};
  });

  for (const { id, dir } of DIRS) {
    it(`${id}: on a move, delegates to focusAdjacentPane(${dir}) with no cue`, () => {
      getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM, workspace: { id: "w" } });
      focusAdjacentPaneMock.mockReturnValue(true);
      run(id);
      expect(focusAdjacentPaneMock).toHaveBeenCalledWith(TEAM, "w", dir, expect.any(Function));
      expect(flashBlockedNavForWorkspaceMock).not.toHaveBeenCalled();
    });

    it(`${id}: blocked → nudges the active tile toward ${dir}`, () => {
      getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM, workspace: { id: "w" } });
      focusAdjacentPaneMock.mockReturnValue(false);
      run(id);
      expect(flashBlockedNavForWorkspaceMock).toHaveBeenCalledWith("w", `nudge-${dir}`);
    });
  }

  it("no-ops when no workspace is focused (the !workspace guard)", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM, workspace: undefined });
    run("viewer.focus-pane-left");
    expect(focusAdjacentPaneMock).not.toHaveBeenCalled();
    expect(flashBlockedNavForWorkspaceMock).not.toHaveBeenCalled();
  });

  // SW-EW-46 (D6): when the expanded workspace is gridded, directional nav traverses
  // the CELLS — the active tab id is the pane id, and the switch sets the active tab.
  it("grid mode: traverses cells (active tab id as pane id) and switches the active tab", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM, workspace: { id: "w" } });
    expandedGridWorkspaceIdMock.mockReturnValue("w");
    storeState.activeTabByWorkspace = { [TEAM]: { w: "a" } };
    storeState.panedByWorkspace = { [TEAM]: { w: ["a", "b"] } };
    focusAdjacentPaneMock.mockReturnValue(true);

    run("viewer.focus-pane-right");

    // Pane id is the active cell's tab id, not the workspace id.
    expect(focusAdjacentPaneMock).toHaveBeenCalledWith(TEAM, "a", "right", expect.any(Function));
    // The injected switch sets the target tab active within the expanded workspace.
    const switchFn = focusAdjacentPaneMock.mock.calls[0][3] as (tk: string, id: string) => void;
    switchFn(TEAM, "b");
    expect(setActiveTerminalTabMock).toHaveBeenCalledWith(TEAM, "b", "w");
  });

  it("grid mode blocked → flashes the active CELL, not the workspace", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM, workspace: { id: "w" } });
    expandedGridWorkspaceIdMock.mockReturnValue("w");
    storeState.activeTabByWorkspace = { [TEAM]: { w: "a" } };
    storeState.panedByWorkspace = { [TEAM]: { w: ["a", "b"] } };
    focusAdjacentPaneMock.mockReturnValue(false);

    run("viewer.focus-pane-up");

    expect(flashBlockedNavForWorkspaceMock).toHaveBeenCalledWith("a", "nudge-up");
  });
});
