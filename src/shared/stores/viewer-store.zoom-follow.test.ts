// SPDX-License-Identifier: MIT
import { useOrgStore } from "./org-store";
import { beforeEach, describe, expect, it, vi } from "vitest";

// D68: "expanded" is one CLI setting for every team, and it always zooms the active team's
// focused workspace. These are the store-integration proofs: the zoom follows the focus on
// every path that moves it (a pick, a cycle, a close, a minimise, a replace), it survives a slot
// with fewer than two tiles and shows again once a second one opens, a preset change or an
// edge split leaves it, and one value holds across teams.

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
}));

vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: vi.fn(), debug: vi.fn() },
  initLogging: vi.fn().mockResolvedValue(undefined),
}));

import {
  selectExpandedWorkspaceId,
  useViewerStore,
  type SoftClosedWorkspace,
  type Workspace,
} from "./viewer-store";

const TEAM = "acme:default";
const OTHER = "acme:other";

const story = (id: string): Workspace => ({
  id,
  kind: "story",
  cwd: `/wt/${id}`,
  storyId: id,
  status: "ready",
});
const shadow = (id: string): SoftClosedWorkspace => ({ workspace: story(id) });

const s = () => useViewerStore.getState();
const zoomOf = (team = TEAM) => selectExpandedWorkspaceId(s(), team);

function reset(over: Partial<Parameters<typeof useViewerStore.setState>[0]> = {}) {
  useViewerStore.setState({
    workspaces: {},
    activeWorkspace: {},
    cliExpanded: false,
    foregroundLayout: {},
    foregroundLayoutByRail: {},
    railKeyByWorkspaceId: {},
    trayMode: {},
    selectedEpicDirName: {},
    softClosedWorkspaces: {},
    trayFullOrderByTeam: {},
    trayOrderByTeam: {},
    mruWorkspaceStack: {},
    workspaceOrderByTeam: {},
    trayBaseOrderByTeam: {},
    ...over,
  });
}

beforeEach(() => {
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
  reset();
  vi.clearAllMocks();
});

const A = "A-1";
const B = "A-2";
const C = "A-3";
const flat = () => ({
  workspaces: { [TEAM]: [story(A), story(B), story(C)] },
  activeWorkspace: { [TEAM]: A },
  trayFullOrderByTeam: { [TEAM]: [A, B, C] },
  trayOrderByTeam: { [TEAM]: [A, B, C] },
});

describe("expanded zooms the focused workspace", () => {
  it("expandWorkspace focuses the tile, then expands it", () => {
    reset(flat());
    s().expandWorkspace(TEAM, C);
    expect(s().activeWorkspace[TEAM]).toBe(C);
    expect(s().cliExpanded).toBe(true);
    expect(zoomOf()).toBe(C);
  });

  it("a pick, a cycle and rapid repeats carry the zoom with the focus", () => {
    reset(flat());
    s().setCliExpanded(true);
    s().switchWorkspace(TEAM, C);
    expect(zoomOf()).toBe(C);
    s().cycleWorkspace(TEAM, "next");
    expect(zoomOf()).toBe(s().activeWorkspace[TEAM]);
    for (const target of [B, C, A, C, B]) s().switchWorkspace(TEAM, target);
    expect(zoomOf()).toBe(B);
  });

  it("a pick while the grid shows starts no zoom", () => {
    reset(flat());
    s().switchWorkspace(TEAM, B);
    expect(zoomOf()).toBeNull();
  });

  it("closing the expanded tile zooms the neighbour the close focuses", () => {
    reset(flat());
    s().expandWorkspace(TEAM, B);
    s().closeWorkspace(TEAM, B);
    expect(zoomOf()).toBe(s().activeWorkspace[TEAM]);
    expect(zoomOf()).not.toBeNull();
  });

  it("replacing the focused tile with a minimised one zooms the incoming tile", () => {
    const M = "e-1-min";
    reset({
      workspaces: { [TEAM]: [story(A), story(B)] },
      activeWorkspace: { [TEAM]: A },
      softClosedWorkspaces: { [TEAM]: { [M]: shadow(M) } },
    });
    s().setCliExpanded(true);
    s().replaceFocusedLeafWorkspace(TEAM, M);
    expect(zoomOf()).toBe(M);
  });

  it("is orthogonal to the layout mode: a shared ↔ full-screen round-trip keeps it", () => {
    reset(flat());
    s().expandWorkspace(TEAM, B);
    s().setLayoutMode("console");
    expect(zoomOf()).toBe(B);
    s().setLayoutMode("shared");
    expect(zoomOf()).toBe(B);
  });
});

describe("expanded with fewer than two tiles", () => {
  it("stays set but shows nothing, and zooms again once a second tile opens", () => {
    reset({
      workspaces: { [TEAM]: [story(A), story(B)] },
      activeWorkspace: { [TEAM]: A },
    });
    s().expandWorkspace(TEAM, A);
    s().closeWorkspace(TEAM, B);
    expect(s().cliExpanded).toBe(true);
    expect(zoomOf()).toBeNull();
    useViewerStore.setState({ workspaces: { [TEAM]: [story(A), story(C)] } });
    expect(zoomOf()).toBe(A);
  });
});

describe("leaving expanded", () => {
  it("a preset change shows the grid", () => {
    reset(flat());
    s().expandWorkspace(TEAM, A);
    s().setForegroundPreset(TEAM, "grid");
    expect(s().cliExpanded).toBe(false);
  });

  it("a split on a tile's edge shows the grid; a drop on its center keeps expanded", () => {
    reset(flat());
    s().expandWorkspace(TEAM, A);
    s().placeWorkspaceTile(TEAM, B, { center: true });
    expect(s().cliExpanded).toBe(true);
    s().placeWorkspaceTile(TEAM, C, { targetWorkspaceId: A, axis: "row", side: "after" });
    expect(s().cliExpanded).toBe(false);
  });
});

describe("one value for every team", () => {
  it("expanded in one team is expanded in another, on that team's focused workspace", () => {
    reset({
      workspaces: { [TEAM]: [story(A), story(B)], [OTHER]: [story("O-1"), story("O-2")] },
      activeWorkspace: { [TEAM]: A, [OTHER]: "O-2" },
    });
    s().expandWorkspace(TEAM, B);
    expect(zoomOf(OTHER)).toBe("O-2");
    s().setCliExpanded(false);
    expect(zoomOf(TEAM)).toBeNull();
    expect(zoomOf(OTHER)).toBeNull();
  });
});

describe("grouped tray", () => {
  const A1 = "A-1";
  const A2 = "A-2";
  const B1 = "B-1";
  const B2 = "B-2";
  const RAIL_INDEX = { [A1]: "alpha", [A2]: "alpha", [B1]: "beta", [B2]: "beta" };

  it("zooms the focused workspace inside the selected epic, and nothing when it is in another", () => {
    reset({
      workspaces: { [TEAM]: [story(A1), story(A2), story(B1), story(B2)] },
      activeWorkspace: { [TEAM]: A1 },
      trayMode: { [TEAM]: "grouped" as const },
      selectedEpicDirName: { [TEAM]: "alpha" },
      railKeyByWorkspaceId: { [TEAM]: RAIL_INDEX },
    });
    s().expandWorkspace(TEAM, A2);
    expect(zoomOf()).toBe(A2);
    // Focus moves to beta while alpha is still the selected epic: alpha's tiles show as the grid.
    s().switchWorkspace(TEAM, B1);
    expect(zoomOf()).toBeNull();
    useViewerStore.setState({ selectedEpicDirName: { [TEAM]: "beta" } });
    expect(zoomOf()).toBe(B1);
  });
});
