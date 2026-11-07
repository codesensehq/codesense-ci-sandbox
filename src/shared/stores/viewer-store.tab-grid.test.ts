// SPDX-License-Identifier: MIT
import "@/test/prefs-state-fixture";
import { describe, it, expect, beforeEach, vi } from "vitest";

// SW-EW-46: the tab grid — the D2 dial + D8 preset, kept in the workspace's
// terminal host. Verifies the grid verbs, the shared grid-mode selectors,
// teardown pruning, and that the viewer never persists a grid.

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  listNotes: vi.fn().mockResolvedValue([]),
  readNote: vi
    .fn()
    .mockResolvedValue({ id: "n1", title: "Scratch", pinned: false, lastOpenedAt: 0, content: "" }),
  createNote: vi.fn().mockResolvedValue({
    id: "n2",
    title: "Untitled",
    pinned: false,
    lastOpenedAt: 0,
    content: "",
  }),
  writeNote: vi.fn().mockResolvedValue(undefined),
  updateNoteMeta: vi
    .fn()
    .mockResolvedValue({ id: "n1", title: "Scratch", pinned: false, lastOpenedAt: 0 }),
  deleteNote: vi.fn().mockResolvedValue([]),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
}));

vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: vi.fn(), debug: vi.fn() },
  initLogging: vi.fn().mockResolvedValue(undefined),
}));

import {
  useViewerStore,
  selectActiveTerminalTab,
  workspacePanedTabIds,
  workspaceTabGridPreset,
  expandedWorkspaceId,
  shouldRenderTabGrid,
  selectWorkspaceKey,
} from "./viewer-store";
import { buildPresetTree, setNodeSizes, type MosaicNode } from "@/features/viewer/lib/mosaic-tree";
import { seedTerminalTabs, installTerminalLayoutHarness } from "@/test/terminal-layout-harness";
import type { Workspace } from "@/features/viewer/lib/cli-workspace/store/viewer-access";
import * as commands from "@/tauri/commands";
import { useOrgStore } from "./org-store";

const saveAppStateMock = commands.saveAppState as ReturnType<typeof vi.fn>;

const TEAM = "acme:default";
const WS = "story:acme/repo/SW-EW-46";

const mkWs = (id: string): Workspace =>
  ({ id, kind: "story" as const, cwd: "", status: "ready" as const }) as Workspace;

/** Seed one workspace with three terminal tabs, tab mode (nothing paned). */
function seedWorkspace(tabIds: string[] = ["a", "b", "c"]): void {
  useViewerStore.setState({
    workspaces: { [TEAM]: [mkWs(WS)] },
    activeWorkspace: { [TEAM]: WS },
    foregroundLayout: {},
    foregroundLayoutByRail: {},
    cliExpanded: false,
  });
  seedTerminalTabs(TEAM, WS, tabIds, { active: tabIds[0] });
}

const paned = () => workspacePanedTabIds(useViewerStore.getState(), TEAM, WS);
const active = () => selectActiveTerminalTab(useViewerStore.getState(), TEAM, WS);

installTerminalLayoutHarness();

beforeEach(() => {
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
  seedWorkspace();
  vi.clearAllMocks();
});

describe("SW-EW-46 tab-grid dial (D2)", () => {
  it("paneOutTerminalTab promotes one tab to a cell and focuses it", () => {
    useViewerStore.getState().paneOutTerminalTab(TEAM, WS, "b");
    expect(paned()).toEqual(["b"]);
    expect(active()).toBe("b");
  });

  it("keeps cells in tab-strip order regardless of pane-out order", () => {
    useViewerStore.getState().paneOutTerminalTab(TEAM, WS, "c");
    useViewerStore.getState().paneOutTerminalTab(TEAM, WS, "a");
    expect(paned()).toEqual(["a", "c"]);
  });

  it("paneOut is idempotent and a no-op for an unknown tab", () => {
    useViewerStore.getState().paneOutTerminalTab(TEAM, WS, "b");
    useViewerStore.getState().paneOutTerminalTab(TEAM, WS, "b");
    useViewerStore.getState().paneOutTerminalTab(TEAM, WS, "zzz");
    expect(paned()).toEqual(["b"]);
  });

  it("paneOut of an unknown tab never moves focus to a ghost id", () => {
    useViewerStore.getState().paneOutTerminalTab(TEAM, WS, "zzz");
    // Active tab must stay on the seeded 'a', not jump to the nonexistent 'zzz'.
    expect(active()).toBe("a");
    expect(paned()).toEqual([]);
  });

  it("gridAllTerminalTabs panes every tab in order", () => {
    useViewerStore.getState().gridAllTerminalTabs(TEAM, WS);
    expect(paned()).toEqual(["a", "b", "c"]);
  });

  it("collapseTabGridToTabs hides the grid but KEEPS the paned set warm (re-grid restores it)", () => {
    const s = () => useViewerStore.getState();
    useViewerStore.getState().gridAllTerminalTabs(TEAM, WS);
    expect(shouldRenderTabGrid(s(), TEAM, WS)).toBe(true);
    useViewerStore.getState().collapseTabGridToTabs(TEAM, WS);
    // SW-EW-46: paned membership is RETAINED (the cell terminals stay mounted/warm) —
    // collapsing only flips the grid off, so toggling never disposes+respawns the PTYs.
    expect(paned()).toEqual(["a", "b", "c"]);
    expect(shouldRenderTabGrid(s(), TEAM, WS)).toBe(false);
    // Re-gridding restores grid mode instantly against the SAME remembered cells.
    useViewerStore.getState().gridAllTerminalTabs(TEAM, WS);
    expect(shouldRenderTabGrid(s(), TEAM, WS)).toBe(true);
  });

  it("paning out a NEW tab clears a remembered collapse (the grid shows again)", () => {
    const s = () => useViewerStore.getState();
    useViewerStore.getState().paneOutTerminalTab(TEAM, WS, "a"); // grid with one cell
    useViewerStore.getState().collapseTabGridToTabs(TEAM, WS);
    expect(shouldRenderTabGrid(s(), TEAM, WS)).toBe(false);
    useViewerStore.getState().paneOutTerminalTab(TEAM, WS, "b"); // pane out a not-yet-paned tab
    expect(shouldRenderTabGrid(s(), TEAM, WS)).toBe(true);
  });

  it("demoteTerminalPane returns a cell to the bar", () => {
    useViewerStore.getState().gridAllTerminalTabs(TEAM, WS);
    useViewerStore.getState().demoteTerminalPane(TEAM, WS, "b");
    expect(paned()).toEqual(["a", "c"]);
  });

  it("demoting the focused cell moves focus to a surviving cell", () => {
    useViewerStore.getState().gridAllTerminalTabs(TEAM, WS);
    useViewerStore.getState().setActiveTerminalTab(TEAM, "b", WS);
    useViewerStore.getState().demoteTerminalPane(TEAM, WS, "b");
    expect(active()).toBe("a");
  });
});

describe("SW-EW-46 tab-grid preset (D8) reuses PRESET_RING", () => {
  it("defaults to columns and cycles columns→rows→grid→columns", () => {
    const preset = () => workspaceTabGridPreset(useViewerStore.getState(), TEAM, WS);
    expect(preset()).toBe("columns");
    useViewerStore.getState().cycleTabGridPreset(TEAM, WS);
    expect(preset()).toBe("rows");
    useViewerStore.getState().cycleTabGridPreset(TEAM, WS);
    expect(preset()).toBe("grid");
    useViewerStore.getState().cycleTabGridPreset(TEAM, WS);
    expect(preset()).toBe("columns");
  });
});

describe("SW-EW-46 teardown pruning", () => {
  it("removeTerminalTab drops a closed tab from the paned set", async () => {
    useViewerStore.getState().gridAllTerminalTabs(TEAM, WS);
    useViewerStore.getState().removeTerminalTab(TEAM, "b", WS);
    await new Promise((r) => setTimeout(r));
    expect(paned()).toEqual(["a", "c"]);
  });

  it("closing a workspace and its terminals drops its grid so a re-open never re-grids stale ids", () => {
    useViewerStore.getState().gridAllTerminalTabs(TEAM, WS);
    useViewerStore.getState().closeWorkspace(TEAM, WS, { killTerminals: true });
    expect(paned()).toEqual([]);
  });
});

describe("SW-EW-46 grid-mode selectors (D1)", () => {
  it("expandedWorkspaceId resolves the lone open workspace", () => {
    expect(expandedWorkspaceId(useViewerStore.getState(), TEAM)).toBe(WS);
  });

  it("expandedWorkspaceId resolves the expanded (focused) tile when several are open", () => {
    useViewerStore.setState({
      workspaces: { [TEAM]: [mkWs(WS), mkWs("other")] },
    });
    expect(expandedWorkspaceId(useViewerStore.getState(), TEAM)).toBeNull();
    useViewerStore.getState().expandWorkspace(TEAM, WS);
    expect(expandedWorkspaceId(useViewerStore.getState(), TEAM)).toBe(WS);
  });

  it("shouldRenderTabGrid is true only when expanded AND paned", () => {
    const s = () => useViewerStore.getState();
    expect(shouldRenderTabGrid(s(), TEAM, WS)).toBe(false); // lone but no panes
    useViewerStore.getState().gridAllTerminalTabs(TEAM, WS);
    expect(shouldRenderTabGrid(s(), TEAM, WS)).toBe(true);
    // Open a second, non-zoomed workspace → no longer expanded → renders as tabs.
    useViewerStore.setState({
      workspaces: { [TEAM]: [mkWs(WS), mkWs("other")] },
    });
    expect(shouldRenderTabGrid(s(), TEAM, WS)).toBe(false);
  });
});

describe("SW-EW-46 persistence exclusion (AC-7)", () => {
  it("persistState never serializes a grid — the host keeps it", async () => {
    useViewerStore.getState().gridAllTerminalTabs(TEAM, WS);
    useViewerStore.getState().collapseTabGridToTabs(TEAM, WS);
    useViewerStore.getState().cycleTabGridPreset(TEAM, WS);

    let captured: string | null = null;
    saveAppStateMock.mockImplementation(async (payload: string) => {
      captured = payload;
    });
    useViewerStore.getState().persistState();
    await new Promise((resolve) => setTimeout(resolve, 400));

    expect(captured).not.toBeNull();
    const viewer = JSON.parse(captured!).viewer;
    expect(viewer.terminalLayout).toBeUndefined();
    expect(captured).not.toContain('"paned"');
  });
});

describe("tab-grid sizes: this viewer's own, persisted", () => {
  const key = () => selectWorkspaceKey(useViewerStore.getState(), TEAM, WS);
  const dragged = (): MosaicNode =>
    setNodeSizes(buildPresetTree("columns", ["a", "b", "c"])!, [], [20, 30, 50]);

  it("a released divider saves the shown arrangement's new sizes under its preset", () => {
    const root = buildPresetTree("columns", ["a", "b", "c"])!;
    useViewerStore.getState().setTabGridSizes(TEAM, WS, "columns", root, [], [20, 30, 50]);
    expect(useViewerStore.getState().tabGridLayout[key()]).toEqual({
      preset: "columns",
      root: dragged(),
    });
  });

  it("a preset — picked or cycled — drops the saved sizes", () => {
    useViewerStore.setState({ tabGridLayout: { [key()]: { preset: "columns", root: dragged() } } });
    useViewerStore.getState().setTabGridPreset(TEAM, WS, "columns");
    expect(useViewerStore.getState().tabGridLayout[key()]).toBeUndefined();
    useViewerStore.setState({ tabGridLayout: { [key()]: { preset: "columns", root: dragged() } } });
    useViewerStore.getState().cycleTabGridPreset(TEAM, WS);
    expect(useViewerStore.getState().tabGridLayout[key()]).toBeUndefined();
    expect(workspaceTabGridPreset(useViewerStore.getState(), TEAM, WS)).toBe("rows");
  });

  it("round-trips through persist → reload, dropping malformed entries", async () => {
    useViewerStore.setState({ tabGridLayout: { [key()]: { preset: "columns", root: dragged() } } });
    let captured: string | null = null;
    saveAppStateMock.mockImplementation(async (payload: string) => {
      captured = payload;
    });
    useViewerStore.getState().persistState();
    await new Promise((resolve) => setTimeout(resolve, 400));
    const saved = JSON.parse(captured!);
    saved.viewer.terminalTabGridSizes["bad|key"] = { preset: "diagonal", root: dragged() };
    useViewerStore.setState({ tabGridLayout: {} });
    (commands.loadAppState as ReturnType<typeof vi.fn>).mockResolvedValueOnce(
      JSON.stringify(saved),
    );
    await useViewerStore.getState().loadPersistedState();
    expect(useViewerStore.getState().tabGridLayout).toEqual({
      [key()]: { preset: "columns", root: dragged() },
    });
  });
});

describe("tab-grid sizes don't outlive what they size", () => {
  const key = () => selectWorkspaceKey(useViewerStore.getState(), TEAM, WS);
  const saved = () => useViewerStore.getState().tabGridLayout[key()];
  const seedSizes = () =>
    useViewerStore.setState({
      tabGridLayout: {
        [key()]: {
          preset: "columns",
          root: setNodeSizes(buildPresetTree("columns", ["a", "b", "c"])!, [], [20, 30, 50]),
        },
      },
    });

  it("removing a tab drops its cell and keeps the others' proportions", async () => {
    seedSizes();
    await useViewerStore.getState().removeTerminalTab(TEAM, "a", WS, { replace: false });
    const root = saved()?.root;
    expect(
      root?.kind === "split" && root.children.map((c) => c.kind === "leaf" && c.workspaceId),
    ).toEqual(["b", "c"]);
    expect(root?.kind === "split" && root.sizes[0]).toBeCloseTo(37.5);
  });

  it("removing all but one cell leaves nothing to keep", async () => {
    seedSizes();
    await useViewerStore.getState().removeTerminalTab(TEAM, "a", WS, { replace: false });
    await useViewerStore.getState().removeTerminalTab(TEAM, "b", WS, { replace: false });
    expect(saved()).toBeUndefined();
  });

  it("closing a workspace with its terminals drops its sizes", () => {
    seedSizes();
    useViewerStore.getState().closeWorkspace(TEAM, WS, { killTerminals: true });
    expect(saved()).toBeUndefined();
  });

  it("a reload keeps sizes only for workspaces that came back", async () => {
    seedSizes();
    let captured: string | null = null;
    saveAppStateMock.mockImplementation(async (payload: string) => {
      captured = payload;
    });
    useViewerStore.getState().persistState();
    await new Promise((resolve) => setTimeout(resolve, 400));
    const blob = JSON.parse(captured!);
    blob.viewer.terminalTabGridSizes[`${TEAM}|gone`] = blob.viewer.terminalTabGridSizes[key()];
    useViewerStore.setState({ tabGridLayout: {} });
    (commands.loadAppState as ReturnType<typeof vi.fn>).mockResolvedValueOnce(JSON.stringify(blob));
    await useViewerStore.getState().loadPersistedState();
    expect(Object.keys(useViewerStore.getState().tabGridLayout)).toEqual([key()]);
  });
});

// Persisting is a no-op until the saved state has loaded (SW-FU-B2).
beforeEach(() => {
  useViewerStore.setState({ stateLoaded: true });
});
