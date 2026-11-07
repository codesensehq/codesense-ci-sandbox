// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
}));

import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";

import { TERMINAL_GRID_DROPPABLE_ID, tileZoneId, trayChipDragId } from "./terminal-drag";
import { act, renderHook } from "@/test-utils";

import {
  clearTrayMoveRefused,
  dropWorkspace,
  setTrayReorderIntent,
  takeTrayReorderIntent,
  useTrayMoveRefused,
} from "./tray-reorder-drag";

// The team view's drag end, as the team view runs it: a release in the tray's band reorders,
// one on a tile's edge splits there, in the view's team.

const ACTIVE = "acme:default";
const OTHER = "acme:other";
const story = (id: string): Workspace => ({ id, kind: "story", cwd: `/${id}`, status: "ready" });

const real = useViewerStore.getState();
const placeWorkspaceTile = vi.fn();
const reopenSoftClosedWorkspace = vi.fn();
const reorderWorkspace = vi.fn(() => true);

beforeEach(() => {
  vi.clearAllMocks();
  useViewerStore.setState({
    openedTeams: [ACTIVE, OTHER],
    workspaces: { [ACTIVE]: [story("a-1"), story("a-2")], [OTHER]: [story("o-1"), story("o-2")] },
    softClosedWorkspaces: {},
    placeWorkspaceTile,
    reopenSoftClosedWorkspace,
    reorderWorkspace,
  });
});

afterEach(() => {
  setTrayReorderIntent(null);
  useViewerStore.setState({
    placeWorkspaceTile: real.placeWorkspaceTile,
    reopenSoftClosedWorkspace: real.reopenSoftClosedWorkspace,
    reorderWorkspace: real.reorderWorkspace,
  });
});

describe("dropWorkspace", () => {
  it("a pill released in the tray's band reorders the tray and never splits a tile", () => {
    // The pointer was last in the band, though the drop resolves over a tile's edge.
    setTrayReorderIntent({ teamKey: ACTIVE, workspaceId: "a-2", place: { before: "a-1" } });
    dropWorkspace(ACTIVE, trayChipDragId("a-2"), tileZoneId("a-1", "left"));
    expect(reorderWorkspace).toHaveBeenCalledWith(ACTIVE, "a-2", { before: "a-1" }, undefined);
    expect(placeWorkspaceTile).not.toHaveBeenCalled();
  });

  it("a release in the band whose move can't be made is announced, and splits nothing", () => {
    reorderWorkspace.mockReturnValueOnce(false);
    const { result } = renderHook(() => useTrayMoveRefused(ACTIVE));
    setTrayReorderIntent({ teamKey: ACTIVE, workspaceId: "a-2", place: { before: "a-1" } });
    act(() => dropWorkspace(ACTIVE, trayChipDragId("a-2"), tileZoneId("a-1", "left")));
    expect(placeWorkspaceTile).not.toHaveBeenCalled();
    expect(result.current?.workspaceId).toBe("a-2");
    act(() => clearTrayMoveRefused(ACTIVE));
    expect(result.current).toBeNull();
  });

  it("an intent for another dragged workspace, or another team, is not taken as this drop's", () => {
    setTrayReorderIntent({ teamKey: ACTIVE, workspaceId: "a-1", place: { after: "a-2" } });
    dropWorkspace(ACTIVE, trayChipDragId("a-2"), tileZoneId("a-1", "left"));
    expect(reorderWorkspace).not.toHaveBeenCalled();
    expect(placeWorkspaceTile).toHaveBeenCalled();
    placeWorkspaceTile.mockClear();
    setTrayReorderIntent({ teamKey: OTHER, workspaceId: "a-2", place: { before: "a-1" } });
    dropWorkspace(ACTIVE, trayChipDragId("a-2"), tileZoneId("a-1", "left"));
    expect(reorderWorkspace).not.toHaveBeenCalled();
    // Either way the stale intent is gone.
    expect(takeTrayReorderIntent()).toBeNull();
  });

  it("a tile (a bare workspace id) dragged across the band never reorders", () => {
    setTrayReorderIntent({ teamKey: ACTIVE, workspaceId: "a-2", place: { before: "a-1" } });
    dropWorkspace(ACTIVE, "a-2", tileZoneId("a-1", "left"));
    expect(reorderWorkspace).not.toHaveBeenCalled();
    expect(takeTrayReorderIntent()).toBeNull();
  });

  it("two trays keep their own intents: one leaving its band does not clear the other's", () => {
    const trayA = {};
    const trayB = {};
    setTrayReorderIntent({ teamKey: ACTIVE, workspaceId: "a-2", place: { before: "a-1" } }, trayA);
    setTrayReorderIntent(null, trayB);
    dropWorkspace(ACTIVE, trayChipDragId("a-2"), tileZoneId("a-1", "left"));
    expect(reorderWorkspace).toHaveBeenCalledWith(ACTIVE, "a-2", { before: "a-1" }, undefined);
    expect(placeWorkspaceTile).not.toHaveBeenCalled();
  });

  it("the drawn base travels with the intent to the store's move", () => {
    setTrayReorderIntent({
      teamKey: ACTIVE,
      workspaceId: "a-2",
      place: { before: "a-1" },
      base: ["a-1", "a-2"],
    });
    dropWorkspace(ACTIVE, trayChipDragId("a-2"), TERMINAL_GRID_DROPPABLE_ID);
    expect(reorderWorkspace).toHaveBeenCalledWith(ACTIVE, "a-2", { before: "a-1" }, ["a-1", "a-2"]);
  });

  it("a drop on a tile's edge splits it in the view's team", () => {
    dropWorkspace(ACTIVE, trayChipDragId("a-2"), tileZoneId("a-1", "right"));
    expect(placeWorkspaceTile).toHaveBeenCalledWith(ACTIVE, "a-2", {
      targetWorkspaceId: "a-1",
      axis: "row",
      side: "after",
    });
  });

  it("a workspace of another team is not dropped among the team's tiles", () => {
    dropWorkspace(ACTIVE, "o-2", tileZoneId("a-1", "left"));
    expect(placeWorkspaceTile).not.toHaveBeenCalled();
    expect(reopenSoftClosedWorkspace).not.toHaveBeenCalled();
  });

  it("a minimized workspace of the team is restored where it is dropped", () => {
    useViewerStore.setState({
      softClosedWorkspaces: { [ACTIVE]: { "a-3": { workspace: story("a-3") } } },
    });
    dropWorkspace(ACTIVE, trayChipDragId("a-3"), tileZoneId("a-1", "bottom"));
    expect(reopenSoftClosedWorkspace).toHaveBeenCalledWith(ACTIVE, "a-3", {
      targetWorkspaceId: "a-1",
      axis: "column",
      side: "after",
    });
  });

  it("on the grid, a minimized workspace of the team is reopened; another team's is not", () => {
    useViewerStore.setState({
      softClosedWorkspaces: {
        [ACTIVE]: { "a-3": { workspace: story("a-3") } },
        [OTHER]: { "o-3": { workspace: story("o-3") } },
      },
    });
    dropWorkspace(ACTIVE, trayChipDragId("a-3"), TERMINAL_GRID_DROPPABLE_ID);
    expect(reopenSoftClosedWorkspace).toHaveBeenCalledWith(ACTIVE, "a-3");
    reopenSoftClosedWorkspace.mockClear();
    dropWorkspace(ACTIVE, trayChipDragId("o-3"), TERMINAL_GRID_DROPPABLE_ID);
    expect(reopenSoftClosedWorkspace).not.toHaveBeenCalled();
    expect(placeWorkspaceTile).not.toHaveBeenCalled();
  });
});
