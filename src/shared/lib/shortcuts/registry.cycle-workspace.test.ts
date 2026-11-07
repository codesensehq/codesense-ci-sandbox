// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

// OC-7 consolidated the capture-phase intercept to delegate to these run() bodies,
// and the intercept test mocks the registry — so without this file the
// cycle-workspace / flip / jump run() wrappers (team resolution + scope guard +
// store action + focus) are unpinned. This asserts that wiring directly, including
// the Task-0 requirement that team resolution flows through getActiveWorkspaceContext.
// OC-16 adds the blocked-move half: when the store action reports `false`, run()
// flashes the active tile instead of focusing.
const getActiveWorkspaceContextMock = vi.fn();
vi.mock("@/shared/hooks/use-active-workspace-context", () => ({
  getActiveWorkspaceContext: () => getActiveWorkspaceContextMock(),
  // The CLI shows the active team in these tests.
}));

const cycleWorkspaceMock = vi.fn();
const flipLastWorkspaceMock = vi.fn();
const jumpToTrayIndexMock = vi.fn();
const jumpToRootWorkspaceMock = vi.fn();
let trayOrderByTeam: Record<string, string[]> = {};
vi.mock("@/shared/stores/viewer-store", () => ({
  useViewerStore: {
    subscribe: () => () => {},
    getState: () => ({
      trayOrderByTeam,
      cycleWorkspace: cycleWorkspaceMock,
      flipLastWorkspace: flipLastWorkspaceMock,
      jumpToTrayIndex: jumpToTrayIndexMock,
      jumpToRootWorkspace: jumpToRootWorkspaceMock,
    }),
  },
}));

const focusActiveWorkspaceTerminalMock = vi.fn();
const revealCollapsedTerminalMock = vi.fn(() => false);
vi.mock("@/features/viewer/lib/focus-cascade", async (importActual) => ({
  ...(await importActual<typeof import("@/features/viewer/lib/focus-cascade")>()),
  focusActiveWorkspaceTerminal: (...args: unknown[]) => focusActiveWorkspaceTerminalMock(...args),
  revealCollapsedTerminal: () => revealCollapsedTerminalMock(),
}));

const flashBlockedNavForWorkspaceMock = vi.fn();
vi.mock("@/features/viewer/lib/blocked-nav-cue", () => ({
  flashBlockedNavForWorkspace: (...args: unknown[]) => flashBlockedNavForWorkspaceMock(...args),
}));

import { SHORTCUT_ACTIONS } from "./registry";
import { NO_TEAM_SENTINEL } from "@/shared/lib/team-key";

const TEAM = "acme:default";
function run(id: string): void {
  const action = SHORTCUT_ACTIONS.find((a) => a.id === id);
  if (!action) throw new Error(`${id} not registered`);
  action.run();
}

describe("viewer.cycle-workspace-next/prev run() (OC-5/OC-7/OC-16)", () => {
  beforeEach(() => vi.clearAllMocks());

  it("next: on a move, delegates then focuses the terminal (no cue)", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM, workspace: { id: "w" } });
    cycleWorkspaceMock.mockReturnValue(true);
    run("viewer.cycle-workspace-next");
    expect(cycleWorkspaceMock).toHaveBeenCalledWith(TEAM, "next");
    expect(focusActiveWorkspaceTerminalMock).toHaveBeenCalledWith(TEAM);
    expect(flashBlockedNavForWorkspaceMock).not.toHaveBeenCalled();
  });

  it("prev delegates to cycleWorkspace(teamKey, 'prev')", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM, workspace: { id: "w" } });
    cycleWorkspaceMock.mockReturnValue(true);
    run("viewer.cycle-workspace-prev");
    expect(cycleWorkspaceMock).toHaveBeenCalledWith(TEAM, "prev");
  });

  it("blocked move: flashes the active tile (pulse) and does NOT focus", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM, workspace: { id: "w" } });
    cycleWorkspaceMock.mockReturnValue(false);
    run("viewer.cycle-workspace-next");
    expect(flashBlockedNavForWorkspaceMock).toHaveBeenCalledWith("w", "pulse");
    expect(focusActiveWorkspaceTerminalMock).not.toHaveBeenCalled();
  });

  it("no-ops (workspace scope) when no workspace is focused", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM, workspace: undefined });
    run("viewer.cycle-workspace-next");
    expect(cycleWorkspaceMock).not.toHaveBeenCalled();
    expect(focusActiveWorkspaceTerminalMock).not.toHaveBeenCalled();
    expect(flashBlockedNavForWorkspaceMock).not.toHaveBeenCalled();
  });
});

describe("viewer.flip-last-workspace run() (team scope, OC-16)", () => {
  beforeEach(() => vi.clearAllMocks());

  it("on a flip, delegates to flipLastWorkspace(teamKey) then focuses (no cue)", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM, workspace: { id: "w" } });
    flipLastWorkspaceMock.mockReturnValue(true);
    run("viewer.flip-last-workspace");
    // team scope: fires even with no focused workspace (empty/minimized roster).
    expect(flipLastWorkspaceMock).toHaveBeenCalledWith(TEAM);
    expect(focusActiveWorkspaceTerminalMock).toHaveBeenCalledWith(TEAM);
    expect(flashBlockedNavForWorkspaceMock).not.toHaveBeenCalled();
  });

  it("blocked flip (no distinct prior workspace): flashes the active tile (pulse)", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM, workspace: { id: "w" } });
    flipLastWorkspaceMock.mockReturnValue(false);
    run("viewer.flip-last-workspace");
    expect(flashBlockedNavForWorkspaceMock).toHaveBeenCalledWith("w", "pulse");
    expect(focusActiveWorkspaceTerminalMock).not.toHaveBeenCalled();
  });

  it("no-ops when there is no active team", () => {
    getActiveWorkspaceContextMock.mockReturnValue({
      teamKey: NO_TEAM_SENTINEL,
      workspace: undefined,
    });
    run("viewer.flip-last-workspace");
    expect(flipLastWorkspaceMock).not.toHaveBeenCalled();
    expect(flashBlockedNavForWorkspaceMock).not.toHaveBeenCalled();
  });
});

describe("viewer.jump-workspace-N run() (team scope, OC-16)", () => {
  beforeEach(() => vi.clearAllMocks());

  it("jump-workspace-1: on a move, delegates to jumpToTrayIndex(teamKey, 0) then focuses", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM, workspace: { id: "w" } });
    jumpToTrayIndexMock.mockReturnValue(true);
    run("viewer.jump-workspace-1");
    expect(jumpToTrayIndexMock).toHaveBeenCalledWith(TEAM, 0); // 1-based label → 0-based index
    expect(focusActiveWorkspaceTerminalMock).toHaveBeenCalledWith(TEAM);
    expect(flashBlockedNavForWorkspaceMock).not.toHaveBeenCalled();
  });

  it("jump-workspace-9 delegates to jumpToTrayIndex(teamKey, 8)", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM, workspace: { id: "w" } });
    jumpToTrayIndexMock.mockReturnValue(true);
    run("viewer.jump-workspace-9");
    expect(jumpToTrayIndexMock).toHaveBeenCalledWith(TEAM, 8);
  });

  it("blocked jump (already on that chip): flashes the active tile and does NOT focus", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM, workspace: { id: "w" } });
    jumpToTrayIndexMock.mockReturnValue(false);
    run("viewer.jump-workspace-1");
    expect(flashBlockedNavForWorkspaceMock).toHaveBeenCalledWith("w", "pulse");
    expect(focusActiveWorkspaceTerminalMock).not.toHaveBeenCalled();
  });

  it("blocked jump in the empty roster (no workspace): cues with undefined (safe no-op)", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM, workspace: undefined });
    jumpToTrayIndexMock.mockReturnValue(false);
    run("viewer.jump-workspace-1");
    // No focused workspace → the helper resolves no tile and no-ops internally.
    expect(flashBlockedNavForWorkspaceMock).toHaveBeenCalledWith(undefined, "pulse");
    expect(focusActiveWorkspaceTerminalMock).not.toHaveBeenCalled();
  });

  it("no-ops when there is no active team", () => {
    getActiveWorkspaceContextMock.mockReturnValue({
      teamKey: NO_TEAM_SENTINEL,
      workspace: undefined,
    });
    run("viewer.jump-workspace-1");
    expect(jumpToTrayIndexMock).not.toHaveBeenCalled();
    expect(flashBlockedNavForWorkspaceMock).not.toHaveBeenCalled();
  });
});

describe("viewer.jump-root-workspace run() (OC-16)", () => {
  beforeEach(() => vi.clearAllMocks());

  it("on a move, delegates to jumpToRootWorkspace(teamKey) with no cue", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM, workspace: { id: "w" } });
    jumpToRootWorkspaceMock.mockReturnValue(true);
    run("viewer.jump-root-workspace");
    expect(jumpToRootWorkspaceMock).toHaveBeenCalledWith(TEAM);
    expect(flashBlockedNavForWorkspaceMock).not.toHaveBeenCalled();
  });

  it("blocked (already at root): flashes the active tile (pulse)", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM, workspace: { id: "w" } });
    jumpToRootWorkspaceMock.mockReturnValue(false);
    run("viewer.jump-root-workspace");
    expect(flashBlockedNavForWorkspaceMock).toHaveBeenCalledWith("w", "pulse");
  });

  it("no-ops (workspace scope) when no workspace is focused", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM, workspace: undefined });
    run("viewer.jump-root-workspace");
    expect(jumpToRootWorkspaceMock).not.toHaveBeenCalled();
    expect(flashBlockedNavForWorkspaceMock).not.toHaveBeenCalled();
  });
});

describe("viewer.jump-workspace-N with the CLI closed", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    trayOrderByTeam = { [TEAM]: ["w-1", "w-2"] };
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM, workspace: { id: "w-1" } });
  });

  it("reopens the panel and focuses the jumped-to workspace", () => {
    revealCollapsedTerminalMock.mockReturnValueOnce(true);
    jumpToTrayIndexMock.mockReturnValue(true);
    run("viewer.jump-workspace-2");
    expect(revealCollapsedTerminalMock).toHaveBeenCalled();
    expect(jumpToTrayIndexMock).toHaveBeenCalledWith(TEAM, 1);
    expect(focusActiveWorkspaceTerminalMock).toHaveBeenCalledWith(TEAM);
  });

  it("the already-focused chip's number still reopens (and focuses), no blocked cue", () => {
    revealCollapsedTerminalMock.mockReturnValueOnce(true);
    jumpToTrayIndexMock.mockReturnValue(false);
    run("viewer.jump-workspace-1");
    expect(focusActiveWorkspaceTerminalMock).toHaveBeenCalledWith(TEAM);
    expect(flashBlockedNavForWorkspaceMock).not.toHaveBeenCalled();
  });

  it("a number past the last chip reopens nothing", () => {
    jumpToTrayIndexMock.mockReturnValue(false);
    run("viewer.jump-workspace-5");
    expect(revealCollapsedTerminalMock).not.toHaveBeenCalled();
    expect(flashBlockedNavForWorkspaceMock).toHaveBeenCalled();
  });
});
