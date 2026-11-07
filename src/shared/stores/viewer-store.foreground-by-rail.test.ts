// SPDX-License-Identifier: MIT
import "@/test/prefs-state-fixture";
import { describe, it, expect, beforeEach, vi } from "vitest";

// SW-EW-40: per-rail foreground mosaic. `foregroundLayoutByRail` (PERSISTED, per
// (team, rail) arrangement) + `railKeyByWorkspaceId` (RUNTIME-ONLY ws→rail index,
// rebuilt by syncWorkspaceFrontmatter from the tray's own grouping) +
// `activeForegroundSlot` (the one flat-vs-grouped read/write resolver). Flat mode
// stays byte-identical: the resolver returns the per-team foregroundLayout slot.

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  saveScratchNote: vi.fn().mockResolvedValue(undefined),
  loadScratchNote: vi.fn().mockResolvedValue(""),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
}));

vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: vi.fn(), debug: vi.fn() },
  initLogging: vi.fn().mockResolvedValue(undefined),
}));

import {
  useViewerStore,
  activeForegroundSlot,
  activeForegroundLayout,
  activeForegroundTiles,
  activeRailKey,
  buildRailKeyIndex,
  defaultForegroundLayout,
  type Workspace,
} from "./viewer-store";
import { trayWorkspaceOrder, railKeyOf } from "@/features/viewer/lib/group-workspaces-by-epic";
import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";
import * as commands from "@/tauri/commands";

const loadAppStateMock = commands.loadAppState as ReturnType<typeof vi.fn>;
const saveAppStateMock = commands.saveAppState as ReturnType<typeof vi.fn>;

const TEAM = "acme:default";

const story = (id: string, storyFilePath?: string): Workspace => ({
  id,
  kind: "story",
  cwd: `/wt/${id}`,
  storyFilePath,
  storyId: id,
  status: "ready",
});

function node(partial: Partial<DerivedPipelineTree>): DerivedPipelineTree {
  return {
    dirPath: "",
    dirName: "",
    absoluteDirPath: "",
    stories: [],
    subEpics: [],
    orderError: null,
    mainDoc: null,
    ...partial,
  };
}
const storyRef = (filePath: string) => ({
  fileName: filePath.split("/").pop()!,
  filePath,
  dependsOn: null,
  status: null,
  documentType: null,
  supportingDocs: null,
  sourceRepo: null,
  team: null,
});
const tree = node({
  subEpics: [
    node({
      dirName: "alpha",
      dirPath: "in-progress/alpha",
      stories: [storyRef("/r/alpha/A-1.md"), storyRef("/r/alpha/A-2.md")],
    }),
    node({ dirName: "beta", dirPath: "in-progress/beta", stories: [storyRef("/r/beta/B-1.md")] }),
  ],
});
// Workspaces whose storyFilePath matches the tree, so the grouping resolves.
const wsAlpha1 = story("A-1", "/r/alpha/A-1.md");
const wsAlpha2 = story("A-2", "/r/alpha/A-2.md");
const wsBeta1 = story("B-1", "/r/beta/B-1.md");
const ALL = [wsAlpha1, wsAlpha2, wsBeta1];
const RAIL_INDEX = { "A-1": "alpha", "A-2": "alpha", "B-1": "beta" };

beforeEach(() => {
  useViewerStore.setState({
    workspaces: {},
    activeWorkspace: {},
    foregroundLayout: {},
    foregroundLayoutByRail: {},
    railKeyByWorkspaceId: {},
    trayMode: {},
    selectedEpicDirName: {},
    softClosedWorkspaces: {},
  });
  vi.clearAllMocks();
});

describe("activeRailKey", () => {
  it("is null in flat mode (unset or explicit flat)", () => {
    useViewerStore.setState({ trayMode: {} });
    expect(activeRailKey(useViewerStore.getState(), TEAM)).toBeNull();
    useViewerStore.setState({ trayMode: { [TEAM]: "flat" } });
    expect(activeRailKey(useViewerStore.getState(), TEAM)).toBeNull();
  });

  it("is the selected rail key in grouped mode (defaulting to '' = Ungrouped)", () => {
    useViewerStore.setState({ trayMode: { [TEAM]: "grouped" } });
    expect(activeRailKey(useViewerStore.getState(), TEAM)).toBe("");
    useViewerStore.setState({ selectedEpicDirName: { [TEAM]: "alpha" } });
    expect(activeRailKey(useViewerStore.getState(), TEAM)).toBe("alpha");
  });
});

describe("activeForegroundSlot / tiles / layout — flat mode (byte-identical)", () => {
  beforeEach(() => {
    useViewerStore.setState({ workspaces: { [TEAM]: ALL } });
  });

  it("tiles = full foregroundTilesFor set, id-sorted", () => {
    expect(activeForegroundTiles(useViewerStore.getState(), TEAM).map((w) => w.id)).toEqual([
      "A-1",
      "A-2",
      "B-1",
    ]);
  });

  it("layout = the per-team foregroundLayout slot; write patches foregroundLayout", () => {
    const layout = defaultForegroundLayout({ preset: "grid" });
    useViewerStore.setState({ foregroundLayout: { [TEAM]: layout } });
    const slot = activeForegroundSlot(useViewerStore.getState(), TEAM);
    expect(slot.layout).toBe(layout);
    expect(activeForegroundLayout(useViewerStore.getState(), TEAM)).toBe(layout);
    const patch = slot.write(defaultForegroundLayout({ preset: "rows" }));
    expect(patch.foregroundLayout?.[TEAM]?.preset).toBe("rows");
    expect(patch.foregroundLayoutByRail).toBeUndefined();
  });
});

describe("activeForegroundSlot / tiles / layout — grouped mode (per-rail)", () => {
  beforeEach(() => {
    useViewerStore.setState({
      workspaces: { [TEAM]: ALL },
      trayMode: { [TEAM]: "grouped" },
      selectedEpicDirName: { [TEAM]: "alpha" },
      railKeyByWorkspaceId: { [TEAM]: RAIL_INDEX },
    });
  });

  it("tiles = ONLY the selected epic's members (others parked)", () => {
    expect(activeForegroundTiles(useViewerStore.getState(), TEAM).map((w) => w.id)).toEqual([
      "A-1",
      "A-2",
    ]);
  });

  it("switching the selected rail swaps the rendered tile set", () => {
    useViewerStore.setState({ selectedEpicDirName: { [TEAM]: "beta" } });
    expect(activeForegroundTiles(useViewerStore.getState(), TEAM).map((w) => w.id)).toEqual([
      "B-1",
    ]);
  });

  it("layout = the selected rail's slot; write patches foregroundLayoutByRail only", () => {
    const alphaLayout = defaultForegroundLayout({ preset: "grid" });
    useViewerStore.setState({ foregroundLayoutByRail: { [TEAM]: { alpha: alphaLayout } } });
    const slot = activeForegroundSlot(useViewerStore.getState(), TEAM);
    expect(slot.layout).toBe(alphaLayout);
    const patch = slot.write(defaultForegroundLayout({ preset: "rows" }));
    expect(patch.foregroundLayoutByRail?.[TEAM]?.alpha?.preset).toBe("rows");
    expect(patch.foregroundLayout).toBeUndefined();
  });

  it("an empty selected epic yields zero tiles (empty-state input, no crash)", () => {
    useViewerStore.setState({ selectedEpicDirName: { [TEAM]: "epic-none" } });
    expect(activeForegroundTiles(useViewerStore.getState(), TEAM)).toEqual([]);
  });

  it("grouped before the index is built (cold start) falls back to the full set, not empty", () => {
    // Index not yet built for the team (runtime-only, undefined pre-first-sync).
    useViewerStore.setState({ railKeyByWorkspaceId: {} });
    expect(activeForegroundTiles(useViewerStore.getState(), TEAM).map((w) => w.id)).toEqual([
      "A-1",
      "A-2",
      "B-1",
    ]);
  });
});

describe("setters route through the slot (AC-5)", () => {
  it("flat mode: setForegroundPreset patches foregroundLayout, leaves foregroundLayoutByRail empty", () => {
    useViewerStore.setState({ workspaces: { [TEAM]: ALL } });
    useViewerStore.getState().setForegroundPreset(TEAM, "grid");
    expect(useViewerStore.getState().foregroundLayout[TEAM]?.preset).toBe("grid");
    expect(useViewerStore.getState().foregroundLayoutByRail[TEAM]).toBeUndefined();
  });

  it("grouped mode: a preset patches ONLY the selected rail — arranging A never touches B", () => {
    useViewerStore.setState({
      workspaces: { [TEAM]: ALL },
      trayMode: { [TEAM]: "grouped" },
      selectedEpicDirName: { [TEAM]: "alpha" },
      railKeyByWorkspaceId: { [TEAM]: RAIL_INDEX },
    });
    useViewerStore.getState().setForegroundPreset(TEAM, "grid");
    // Switch to beta and arrange it differently.
    useViewerStore.setState({ selectedEpicDirName: { [TEAM]: "beta" } });
    useViewerStore.getState().setForegroundPreset(TEAM, "rows");

    const byRail = useViewerStore.getState().foregroundLayoutByRail[TEAM];
    expect(byRail.alpha.preset).toBe("grid");
    expect(byRail.beta.preset).toBe("rows");
    // Flat slot never written in grouped mode.
    expect(useViewerStore.getState().foregroundLayout[TEAM]).toBeUndefined();
  });

  it("grouped mode: cycleForegroundPreset advances ONLY the selected rail", () => {
    useViewerStore.setState({
      workspaces: { [TEAM]: ALL },
      trayMode: { [TEAM]: "grouped" },
      selectedEpicDirName: { [TEAM]: "alpha" },
      railKeyByWorkspaceId: { [TEAM]: RAIL_INDEX },
    });
    // alpha reads as default "columns" → cycle → "rows".
    useViewerStore.getState().cycleForegroundPreset(TEAM);

    const byRail = useViewerStore.getState().foregroundLayoutByRail[TEAM];
    expect(byRail.alpha.preset).toBe("rows");
    expect(byRail.beta).toBeUndefined(); // sibling rail untouched
    // Flat slot never written in grouped mode.
    expect(useViewerStore.getState().foregroundLayout[TEAM]).toBeUndefined();
  });

  it("grouped mode: placeWorkspaceTile (drag-split) writes the selected rail's custom root", () => {
    useViewerStore.setState({
      workspaces: { [TEAM]: ALL },
      trayMode: { [TEAM]: "grouped" },
      selectedEpicDirName: { [TEAM]: "alpha" },
      railKeyByWorkspaceId: { [TEAM]: RAIL_INDEX },
    });
    // Split A-2 to the right of A-1 (both in epic alpha).
    useViewerStore
      .getState()
      .placeWorkspaceTile(TEAM, "A-2", { targetWorkspaceId: "A-1", axis: "row", side: "after" });
    const alpha = useViewerStore.getState().foregroundLayoutByRail[TEAM].alpha;
    expect(alpha.preset).toBe("custom");
    expect(alpha.root).not.toBeNull();
    expect(useViewerStore.getState().foregroundLayout[TEAM]).toBeUndefined();
  });

  it("grouped mode: setForegroundNodeSizes writes the selected rail's depth-1 sizes", () => {
    useViewerStore.setState({
      workspaces: { [TEAM]: ALL },
      trayMode: { [TEAM]: "grouped" },
      selectedEpicDirName: { [TEAM]: "alpha" },
      railKeyByWorkspaceId: { [TEAM]: RAIL_INDEX },
    });
    useViewerStore.getState().setForegroundPanelSizes(TEAM, [0.7, 0.3]);
    expect(useViewerStore.getState().foregroundLayoutByRail[TEAM].alpha.sizes).toEqual([0.7, 0.3]);
    expect(useViewerStore.getState().foregroundLayout[TEAM]).toBeUndefined();
  });
});

describe("railKeyByWorkspaceId — rebuilt by syncWorkspaceFrontmatter (identity with the tray)", () => {
  it("matches buildRailKeyIndex over the same tray grouping (trayWorkspaceOrder + railKeyOf)", () => {
    useViewerStore.setState({ workspaces: { [TEAM]: ALL } });
    useViewerStore.getState().syncWorkspaceFrontmatter(TEAM, tree);

    const expected = buildRailKeyIndex(ALL, {}, tree);
    expect(useViewerStore.getState().railKeyByWorkspaceId[TEAM]).toEqual(expected);
    // …and the index equals what the tray itself would compute, per workspace.
    const fromTray: Record<string, string> = {};
    for (const e of trayWorkspaceOrder(ALL, {}, tree)) fromTray[e.workspace.id] = railKeyOf(e);
    expect(useViewerStore.getState().railKeyByWorkspaceId[TEAM]).toEqual(fromTray);
    expect(fromTray).toEqual({ "A-1": "alpha", "A-2": "alpha", "B-1": "beta" });
  });

  it("is RUNTIME-ONLY — never written to the serialized persist blob", async () => {
    useViewerStore.setState({ workspaces: { [TEAM]: ALL } });
    useViewerStore.getState().syncWorkspaceFrontmatter(TEAM, tree);
    expect(useViewerStore.getState().railKeyByWorkspaceId[TEAM]).toBeTruthy();

    let captured: string | null = null;
    saveAppStateMock.mockImplementation(async (payload: string) => {
      captured = payload;
    });
    useViewerStore.getState().persistState();
    await new Promise((r) => setTimeout(r, 400));
    expect(captured).not.toBeNull();
    expect(JSON.parse(captured!).viewer.railKeyByWorkspaceId).toBeUndefined();
  });
});

describe("foregroundLayoutByRail — persistence (AC-3) + closeTeam cleanup (AC-8)", () => {
  it("round-trips through persist → reload, each rail's arrangement included", async () => {
    useViewerStore.setState({
      workspaces: { [TEAM]: ALL },
      foregroundLayoutByRail: {
        [TEAM]: {
          alpha: defaultForegroundLayout({ preset: "grid" }),
          beta: defaultForegroundLayout({ preset: "rows" }),
        },
      },
    });

    let captured: string | null = null;
    saveAppStateMock.mockImplementation(async (payload: string) => {
      captured = payload;
    });
    useViewerStore.getState().persistState();
    await new Promise((r) => setTimeout(r, 400));
    expect(captured).not.toBeNull();

    useViewerStore.setState({ foregroundLayoutByRail: {} });
    loadAppStateMock.mockResolvedValue(captured!);
    await useViewerStore.getState().loadPersistedState();

    const byRail = useViewerStore.getState().foregroundLayoutByRail[TEAM];
    expect(byRail.alpha.preset).toBe("grid");
    expect(byRail.beta.preset).toBe("rows");
  });

  it("absent slice loads as {} (backward-compatible, no migration)", async () => {
    loadAppStateMock.mockResolvedValue(JSON.stringify({ viewer: { schemaVersion: 9 } }));
    await useViewerStore.getState().loadPersistedState();
    expect(useViewerStore.getState().foregroundLayoutByRail).toEqual({});
  });

  it("closeTeam drops foregroundLayoutByRail + railKeyByWorkspaceId for the closed team only", () => {
    useViewerStore.setState({
      foregroundLayoutByRail: {
        [TEAM]: { alpha: defaultForegroundLayout() },
        "acme:other": { beta: defaultForegroundLayout() },
      },
      railKeyByWorkspaceId: { [TEAM]: RAIL_INDEX, "acme:other": { X: "beta" } },
    });
    useViewerStore.getState().closeTeam(TEAM);
    expect(useViewerStore.getState().foregroundLayoutByRail[TEAM]).toBeUndefined();
    expect(useViewerStore.getState().railKeyByWorkspaceId[TEAM]).toBeUndefined();
    expect(useViewerStore.getState().foregroundLayoutByRail["acme:other"]).toBeDefined();
    expect(useViewerStore.getState().railKeyByWorkspaceId["acme:other"]).toEqual({ X: "beta" });
  });
});

// Persisting is a no-op until the saved state has loaded (SW-FU-B2).
beforeEach(() => {
  useViewerStore.setState({ stateLoaded: true });
});
