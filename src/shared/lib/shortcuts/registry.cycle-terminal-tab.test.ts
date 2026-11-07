// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

// The tab-cycle actions' run() bodies resolve the active team via
// getActiveWorkspaceContext, guard on a focused workspace, delegate to the store's
// cycleTerminalTab, then re-focus the newly-active tab's terminal. Mock the seams
// so we can assert that wiring (team + direction + guard + re-focus) — the same
// run() the capture-phase intercept dispatches. OC-16: a ≤1-tab no-op reports
// `false`, and run() then flashes the active tile instead of focusing.
const getActiveWorkspaceContextMock = vi.fn();
vi.mock("@/shared/hooks/use-active-workspace-context", () => ({
  getActiveWorkspaceContext: () => getActiveWorkspaceContextMock(),
  // The CLI shows the active team in these tests.
}));

const cycleTerminalTabMock = vi.fn();
vi.mock("@/shared/stores/viewer-store", () => ({
  useViewerStore: {
    subscribe: () => () => {},
    getState: () => ({ cycleTerminalTab: cycleTerminalTabMock }),
  },
}));

const focusActiveWorkspaceTerminalMock = vi.fn();
vi.mock("@/features/viewer/lib/focus-cascade", async (importActual) => ({
  ...(await importActual<typeof import("@/features/viewer/lib/focus-cascade")>()),
  focusActiveWorkspaceTerminal: (...args: unknown[]) => focusActiveWorkspaceTerminalMock(...args),
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

describe("viewer.cycle-terminal-tab-next/prev run() (OC-7/OC-16)", () => {
  beforeEach(() => vi.clearAllMocks());

  it("next: on a move, delegates then re-focuses the terminal (no cue)", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM, workspace: { id: "w" } });
    cycleTerminalTabMock.mockReturnValue(true);
    run("viewer.cycle-terminal-tab-next");
    expect(cycleTerminalTabMock).toHaveBeenCalledWith(TEAM, "next");
    expect(focusActiveWorkspaceTerminalMock).toHaveBeenCalledWith(TEAM);
    expect(flashBlockedNavForWorkspaceMock).not.toHaveBeenCalled();
  });

  it("prev delegates to cycleTerminalTab(teamKey, 'prev')", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM, workspace: { id: "w" } });
    cycleTerminalTabMock.mockReturnValue(true);
    run("viewer.cycle-terminal-tab-prev");
    expect(cycleTerminalTabMock).toHaveBeenCalledWith(TEAM, "prev");
  });

  it("blocked (≤1 tab): flashes the active tile (pulse) and does NOT focus", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM, workspace: { id: "w" } });
    cycleTerminalTabMock.mockReturnValue(false);
    run("viewer.cycle-terminal-tab-next");
    expect(flashBlockedNavForWorkspaceMock).toHaveBeenCalledWith("w", "pulse");
    expect(focusActiveWorkspaceTerminalMock).not.toHaveBeenCalled();
  });

  it("no-ops when no workspace is focused (the !workspace guard)", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM, workspace: undefined });
    run("viewer.cycle-terminal-tab-next");
    run("viewer.cycle-terminal-tab-prev");
    expect(cycleTerminalTabMock).not.toHaveBeenCalled();
    expect(focusActiveWorkspaceTerminalMock).not.toHaveBeenCalled();
    expect(flashBlockedNavForWorkspaceMock).not.toHaveBeenCalled();
  });
});
