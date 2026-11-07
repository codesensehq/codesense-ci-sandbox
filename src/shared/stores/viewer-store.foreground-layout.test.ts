// SPDX-License-Identifier: MIT
import "@/test/prefs-state-fixture";
import { describe, it, expect, beforeEach, vi } from "vitest";

// SW-EW-3 Task 1: `foregroundLayout` slice — the split-terminal tiling
// arrangement (react-resizable-panels sizes + axis + EW-4 zoom seam),
// persisted team-keyed. New additive slice (absent → {}, no migration),
// mirroring the SW-EW-2 `workspacePanelCollapsed`/`workspacePanelWidth` pattern.

vi.mock("@/tauri/commands", () => ({
  LOCAL_MACHINE_ID: "local",
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
  selectActiveTerminalTab,
  selectExpandedWorkspaceId,
  selectWorkspaceTabs,
  useViewerStore,
} from "./viewer-store";
import { seedTerminalTabs, installTerminalLayoutHarness } from "@/test/terminal-layout-harness";
import * as commands from "@/tauri/commands";
import { useOrgStore } from "./org-store";

const loadAppStateMock = commands.loadAppState as ReturnType<typeof vi.fn>;
const saveAppStateMock = commands.saveAppState as ReturnType<typeof vi.fn>;

const TEAM = "acme:default";

installTerminalLayoutHarness();

beforeEach(() => {
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
  useViewerStore.setState({ foregroundLayout: {}, activeWorkspace: {}, workspaces: {} });
  vi.clearAllMocks();
});

describe("foregroundLayout — setters", () => {
  it("setForegroundPanelSizes stores per-team sizes", () => {
    useViewerStore.getState().setForegroundPanelSizes(TEAM, [60, 40]);
    expect(useViewerStore.getState().foregroundLayout[TEAM]?.sizes).toEqual([60, 40]);
  });

  it("setForegroundDirection stores per-team axis", () => {
    useViewerStore.getState().setForegroundDirection(TEAM, "column");
    expect(useViewerStore.getState().foregroundLayout[TEAM]?.direction).toBe("column");
  });

  it("sizes and direction coexist, and a new direction lays out evenly", () => {
    useViewerStore.getState().setForegroundDirection(TEAM, "column");
    useViewerStore.getState().setForegroundPanelSizes(TEAM, [30, 70]);
    let fl = useViewerStore.getState().foregroundLayout[TEAM];
    expect(fl?.sizes).toEqual([30, 70]);
    expect(fl?.direction).toBe("column");
    // A direction is a preset: picking it drops the dragged sizes.
    useViewerStore.getState().setForegroundDirection(TEAM, "row");
    fl = useViewerStore.getState().foregroundLayout[TEAM];
    expect(fl?.sizes).toEqual([]);
    expect(fl?.direction).toBe("row");
  });

  it("is independent per team", () => {
    useViewerStore.getState().setForegroundPanelSizes("acme:a", [70, 30]);
    useViewerStore.getState().setForegroundPanelSizes("acme:b", [25, 75]);
    expect(useViewerStore.getState().foregroundLayout["acme:a"]?.sizes).toEqual([70, 30]);
    expect(useViewerStore.getState().foregroundLayout["acme:b"]?.sizes).toEqual([25, 75]);
  });

  it("is undefined when unset (read site applies its own default)", () => {
    expect(useViewerStore.getState().foregroundLayout["never:set"]).toBeUndefined();
  });
});

describe("expandTerminalOnOpen (SW-FX-35) — pair-safe show-the-terminal", () => {
  it("from a cold viewer layoutMode, sets the gate pair (visible + non-viewer)", () => {
    useViewerStore.setState({ layoutMode: "viewer", lastTerminalMode: "shared" });
    useViewerStore.getState().expandTerminalOnOpen();
    const s = useViewerStore.getState();
    expect(s.terminalVisible).toBe(true);
    expect(s.layoutMode).toBe("shared"); // unset lastTerminalMode → shared
  });

  it("restores the user's last terminal mode ('console')", () => {
    useViewerStore.setState({
      layoutMode: "viewer",
      lastTerminalMode: "console",
    });
    useViewerStore.getState().expandTerminalOnOpen();
    expect(useViewerStore.getState().layoutMode).toBe("console");
  });

  it("is a no-downgrade when already shared (idempotent for the already-open case)", () => {
    useViewerStore.setState({
      layoutMode: "shared",
      lastTerminalMode: "shared",
    });
    useViewerStore.getState().expandTerminalOnOpen();
    const s = useViewerStore.getState();
    expect(s.layoutMode).toBe("shared");
    expect(s.terminalVisible).toBe(true);
  });
});

describe("foregroundLayout — persistence", () => {
  it("round-trips sizes + direction through persist → reload", async () => {
    useViewerStore.getState().setForegroundDirection(TEAM, "column");
    useViewerStore.getState().setForegroundPanelSizes(TEAM, [60, 40]);

    let captured: string | null = null;
    saveAppStateMock.mockImplementation(async (payload: string) => {
      captured = payload;
    });
    useViewerStore.getState().persistState();
    await new Promise((r) => setTimeout(r, 400));
    expect(captured).not.toBeNull();

    useViewerStore.setState({ foregroundLayout: {} });
    loadAppStateMock.mockResolvedValue(captured!);
    await useViewerStore.getState().loadPersistedState();

    const fl = useViewerStore.getState().foregroundLayout[TEAM];
    expect(fl?.sizes).toEqual([60, 40]);
    expect(fl?.direction).toBe("column");
  });

  it("absent slice loads as {} (backward-compatible, no migration)", async () => {
    loadAppStateMock.mockResolvedValue(JSON.stringify({ viewer: { schemaVersion: 9 } }));
    await useViewerStore.getState().loadPersistedState();
    expect(useViewerStore.getState().foregroundLayout).toEqual({});
  });
});

describe("foregroundLayout — expanded (global, D68) + preset", () => {
  const WS_A = "epic-x-story-a";
  const WS_B = "epic-x-story-b";

  beforeEach(() => {
    useViewerStore.setState({
      foregroundLayout: {},
      workspaces: {
        [TEAM]: [
          { id: WS_A, kind: "story", cwd: "/a", status: "ready" },
          { id: WS_B, kind: "story", cwd: "/b", status: "ready" },
        ],
      },
      activeWorkspace: { [TEAM]: WS_A },
      cliExpanded: false,
    });
  });

  const expanded = () => selectExpandedWorkspaceId(useViewerStore.getState(), TEAM);

  it("setCliExpanded and toggleCliExpanded zoom the focused tile and go back to the grid", () => {
    useViewerStore.getState().setCliExpanded(true);
    expect(expanded()).toBe(WS_A);
    useViewerStore.getState().toggleCliExpanded();
    expect(expanded()).toBeNull();
    useViewerStore.getState().toggleCliExpanded();
    expect(expanded()).toBe(WS_A);
  });

  it("expandWorkspace focuses another tile, then expands it", () => {
    useViewerStore.getState().expandWorkspace(TEAM, WS_B);
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(WS_B);
    expect(expanded()).toBe(WS_B);
  });

  it("setForegroundPreset sets preset + direction and leaves expanded", () => {
    useViewerStore.getState().expandWorkspace(TEAM, WS_A);
    useViewerStore.getState().setForegroundPreset(TEAM, "rows");
    let fl = useViewerStore.getState().foregroundLayout[TEAM];
    expect(fl?.preset).toBe("rows");
    expect(fl?.direction).toBe("column");
    expect(useViewerStore.getState().cliExpanded).toBe(false);

    useViewerStore.getState().setForegroundPreset(TEAM, "columns");
    fl = useViewerStore.getState().foregroundLayout[TEAM];
    expect(fl?.preset).toBe("columns");
    expect(fl?.direction).toBe("row");

    useViewerStore.getState().setForegroundPreset(TEAM, "grid");
    expect(useViewerStore.getState().foregroundLayout[TEAM]?.preset).toBe("grid");
  });

  it("cycleForegroundPreset advances columns → rows → grid → columns (wraps), syncing direction", () => {
    const cycle = () => useViewerStore.getState().cycleForegroundPreset(TEAM);
    // Default (absent) slot reads as "columns" → first press lands on "rows".
    cycle();
    let fl = useViewerStore.getState().foregroundLayout[TEAM];
    expect(fl?.preset).toBe("rows");
    expect(fl?.direction).toBe("column");

    cycle();
    expect(useViewerStore.getState().foregroundLayout[TEAM]?.preset).toBe("grid");

    cycle(); // wraps back to columns
    fl = useViewerStore.getState().foregroundLayout[TEAM];
    expect(fl?.preset).toBe("columns");
    expect(fl?.direction).toBe("row");
  });

  it("cycleForegroundPreset from custom lands on columns and resets the custom root", () => {
    useViewerStore.setState({
      foregroundLayout: {
        [TEAM]: {
          sizes: [50, 50],
          direction: "row",
          preset: "custom",
          root: {
            kind: "split",
            axis: "row",
            children: [
              { kind: "leaf", workspaceId: WS_A },
              { kind: "leaf", workspaceId: WS_B },
            ],
            sizes: [50, 50],
          },
        },
      },
    });
    useViewerStore.getState().cycleForegroundPreset(TEAM);
    const fl = useViewerStore.getState().foregroundLayout[TEAM];
    expect(fl?.preset).toBe("columns");
    expect(fl?.root).toBeNull();
  });

  it("cycleForegroundPreset leaves expanded (delegates to setForegroundPreset)", () => {
    useViewerStore.getState().expandWorkspace(TEAM, WS_A);
    useViewerStore.getState().cycleForegroundPreset(TEAM);
    expect(useViewerStore.getState().cliExpanded).toBe(false);
  });

  it("setForegroundDirection syncs preset", () => {
    useViewerStore.getState().setForegroundDirection(TEAM, "column");
    const fl = useViewerStore.getState().foregroundLayout[TEAM];
    expect(fl?.direction).toBe("column");
    expect(fl?.preset).toBe("rows");
  });

  it("switchWorkspace moves the zoom with the focus", () => {
    useViewerStore.getState().expandWorkspace(TEAM, WS_A);
    useViewerStore.getState().switchWorkspace(TEAM, WS_B);
    expect(expanded()).toBe(WS_B);
    useViewerStore.getState().switchWorkspace(TEAM, WS_B);
    expect(expanded()).toBe(WS_B);
  });

  it("a close down to one tile shows no zoom, and expanded stays set for the next tile", () => {
    useViewerStore.getState().expandWorkspace(TEAM, WS_B);
    useViewerStore.getState().closeWorkspace(TEAM, WS_B);
    expect(expanded()).toBeNull();
    expect(useViewerStore.getState().cliExpanded).toBe(true);
  });

  it("defaultForegroundLayout derives preset from direction (legacy entries)", () => {
    useViewerStore.getState().setForegroundPanelSizes(TEAM, [50, 50]);
    // sizes-only setter leaves direction "row" → preset "columns".
    expect(useViewerStore.getState().foregroundLayout[TEAM]?.preset).toBe("columns");
  });
});

describe("foregroundLayout — preset persistence, expanded in the CLI chrome", () => {
  const ws = (id: string) => ({
    id,
    kind: "story" as const,
    cwd: `/${id}`,
    status: "ready" as const,
  });

  it("restores the preset and the global expanded on load, on the focused workspace", async () => {
    useViewerStore.setState({
      workspaces: { [TEAM]: [ws("tile-a"), ws("tile-b")] },
      activeWorkspace: { [TEAM]: "tile-a" },
      cliExpanded: false,
    });
    useViewerStore.getState().setForegroundPreset(TEAM, "grid");
    useViewerStore.getState().expandWorkspace(TEAM, "tile-b");

    let captured: string | null = null;
    saveAppStateMock.mockImplementation(async (payload: string) => {
      captured = payload;
    });
    useViewerStore.getState().persistState();
    await new Promise((r) => setTimeout(r, 400));
    expect(captured).not.toBeNull();
    expect(JSON.parse(captured!).viewer.cliChrome.expanded).toBe(true);

    useViewerStore.setState({ foregroundLayout: {}, cliExpanded: false });
    loadAppStateMock.mockResolvedValue(captured!);
    await useViewerStore.getState().loadPersistedState();

    expect(useViewerStore.getState().foregroundLayout[TEAM]?.preset).toBe("grid");
    expect(selectExpandedWorkspaceId(useViewerStore.getState(), TEAM)).toBe("tile-b");
  });

  it("cycleForegroundPreset persists the advanced preset across reload", async () => {
    let captured: string | null = null;
    saveAppStateMock.mockImplementation(async (payload: string) => {
      captured = payload;
    });

    useViewerStore.getState().cycleForegroundPreset(TEAM); // columns → rows
    // flushPersist deterministically awaits the debounced write (no wall-clock sleep).
    await useViewerStore.getState().flushPersist();
    expect(captured).not.toBeNull();

    useViewerStore.setState({ foregroundLayout: {} });
    loadAppStateMock.mockResolvedValue(captured!);
    await useViewerStore.getState().loadPersistedState();

    expect(useViewerStore.getState().foregroundLayout[TEAM]?.preset).toBe("rows");
  });
});

describe("per-tile tab actions — explicit workspaceId targeting (SW-EW-3 correctness)", () => {
  const WS_A = "epic-x-story-a";
  const WS_B = "epic-x-story-b";

  beforeEach(() => {
    useViewerStore.setState({
      workspaces: {
        [TEAM]: [
          { id: WS_A, kind: "story", cwd: "/a", status: "ready" },
          { id: WS_B, kind: "story", cwd: "/b", status: "ready" },
        ],
      },
      // WS_A is the ACTIVE workspace; actions targeting WS_B must NOT hit WS_A.
      activeWorkspace: { [TEAM]: WS_A },
    });
    seedTerminalTabs(TEAM, WS_A, [{ session: "a1", name: "a1" }]);
  });

  const ids = (ws: string) =>
    selectWorkspaceTabs(useViewerStore.getState(), TEAM, ws).map((t) => t.id);

  it("addTerminalTab(workspaceId) adds to the named workspace, not the active one", () => {
    void useViewerStore.getState().addTerminalTab(TEAM, { sessionId: "b1", name: "b1" }, WS_B);
    expect(ids(WS_B)).toEqual(["b1"]);
    expect(ids(WS_A)).toEqual(["a1"]); // active untouched
  });

  it("removeTerminalTab(workspaceId) removes from the named workspace (safe after an async exit)", async () => {
    seedTerminalTabs(TEAM, WS_B, [
      { session: "b1", name: "b1" },
      { session: "b2", name: "b2" },
    ]);
    useViewerStore.getState().removeTerminalTab(TEAM, "b1", WS_B);
    await new Promise((r) => setTimeout(r));
    expect(ids(WS_B)).toEqual(["b2"]);
    expect(ids(WS_A)).toEqual(["a1"]); // active untouched
  });

  it("setActiveTerminalTab(workspaceId) sets the named workspace's active tab", () => {
    seedTerminalTabs(TEAM, WS_B, [
      { session: "b1", name: "b1" },
      { session: "b2", name: "b2" },
    ]);
    useViewerStore.getState().setActiveTerminalTab(TEAM, "b2", WS_B);
    const st = useViewerStore.getState();
    expect(selectActiveTerminalTab(st, TEAM, WS_B)).toBe("b2");
    expect(selectActiveTerminalTab(st, TEAM, WS_A)).toBe("a1"); // active workspace's selection untouched
  });

  it("omitting workspaceId still targets the active workspace (backward-compatible)", () => {
    void useViewerStore.getState().addTerminalTab(TEAM, { sessionId: "a2", name: "a2" });
    expect(ids(WS_A)).toEqual(["a1", "a2"]);
    expect(ids(WS_B)).toEqual([]); // non-active untouched
  });
});

// Persisting is a no-op until the saved state has loaded (SW-FU-B2).
beforeEach(() => {
  useViewerStore.setState({ stateLoaded: true });
});
