// SPDX-License-Identifier: MIT
import { useOrgStore } from "./org-store";
import { describe, it, expect, beforeEach, vi } from "vitest";

// SW-EW-17 — store behavior for the 2D mosaic: directional placement,
// per-node size commits, and the effective-root derivation.

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

import { useViewerStore, effectiveMosaicRoot, type ForegroundLayout } from "./viewer-store";
import { leafWorkspaceIds, type MosaicNode } from "@/features/viewer/lib/mosaic-tree";

const TEAM = "acme:default";
const A = "e-1-a";
const B = "e-1-b";
const C = "e-1-c";

beforeEach(() => {
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
  useViewerStore.setState({
    foregroundLayout: {},
    activeWorkspace: {},
    workspaces: {
      [TEAM]: [
        { id: A, kind: "story", cwd: "/a", status: "ready" },
        { id: B, kind: "story", cwd: "/b", status: "ready" },
        { id: C, kind: "story", cwd: "/c", status: "ready" },
      ],
    },
  });
  vi.clearAllMocks();
});

const layout = (): ForegroundLayout | undefined => useViewerStore.getState().foregroundLayout[TEAM];

describe("placeWorkspaceTile — directional", () => {
  it("moves an on-screen tile to a target edge and marks the mosaic custom", () => {
    useViewerStore.getState().placeWorkspaceTile(TEAM, C, {
      targetWorkspaceId: A,
      axis: "row",
      side: "before",
    });
    const fl = layout()!;
    expect(fl.preset).toBe("custom");
    expect(fl.root).not.toBeNull();
    expect(leafWorkspaceIds(fl.root)).toEqual([C, A, B]);
  });

  it("a top-edge drop creates a column (horizontal) split nesting the target", () => {
    useViewerStore.getState().placeWorkspaceTile(TEAM, C, {
      targetWorkspaceId: A,
      axis: "column",
      side: "before",
    });
    const root = layout()!.root as Extract<MosaicNode, { kind: "split" }>;
    // outer row [ (column[c,a]) , b ]
    expect(root.axis).toBe("row");
    expect(root.children[0]).toMatchObject({ kind: "split", axis: "column" });
    expect(leafWorkspaceIds(root)).toEqual([C, A, B]);
  });

  it("a tile dropped on its OWN edge is a no-op (preset not flipped to custom)", () => {
    useViewerStore.getState().setForegroundPreset(TEAM, "grid");
    useViewerStore.getState().placeWorkspaceTile(TEAM, A, {
      targetWorkspaceId: A,
      axis: "row",
      side: "after",
    });
    expect(layout()!.preset).toBe("grid"); // unchanged
    expect(layout()!.root).toBeNull(); // still derived, not frozen to custom
  });

  it("leaves expanded on a directional split", () => {
    useViewerStore.getState().expandWorkspace(TEAM, A);
    expect(useViewerStore.getState().cliExpanded).toBe(true);
    useViewerStore.getState().placeWorkspaceTile(TEAM, C, {
      targetWorkspaceId: B,
      axis: "row",
      side: "after",
    });
    expect(useViewerStore.getState().cliExpanded).toBe(false);
  });
});

describe("placeWorkspaceTile — center", () => {
  it("focuses the workspace without changing the arrangement", () => {
    useViewerStore.getState().placeWorkspaceTile(TEAM, B, { center: true });
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(B);
    // no custom mosaic created
    expect(layout()?.root ?? null).toBeNull();
  });
});

describe("setForegroundNodeSizes", () => {
  it("writes nested node sizes without touching the root", () => {
    const root: MosaicNode = {
      kind: "split",
      axis: "column",
      children: [
        {
          kind: "split",
          axis: "row",
          children: [
            { kind: "leaf", workspaceId: A },
            { kind: "leaf", workspaceId: B },
          ],
          sizes: [50, 50],
        },
        { kind: "leaf", workspaceId: C },
      ],
      sizes: [60, 40],
    };
    useViewerStore.setState({
      foregroundLayout: {
        [TEAM]: { sizes: [], direction: "row", preset: "custom", root },
      },
    });
    useViewerStore.getState().setForegroundNodeSizes(TEAM, [0], [70, 30]);
    const out = layout()!.root as Extract<MosaicNode, { kind: "split" }>;
    expect((out.children[0] as Extract<MosaicNode, { kind: "split" }>).sizes).toEqual([70, 30]);
    expect(out.sizes).toEqual([60, 40]);
  });

  it("depth-1 (no custom root) still writes the flat sizes (unchanged behavior)", () => {
    useViewerStore.getState().setForegroundNodeSizes(TEAM, [], [70, 30]);
    expect(layout()!.sizes).toEqual([70, 30]);
    expect(layout()!.root).toBeNull();
  });

  it("a grid resize materializes a concrete root so the commit sticks (was silently dropped)", () => {
    useViewerStore.getState().setForegroundPreset(TEAM, "grid");
    expect(layout()!.root).toBeNull(); // grid is derived until resized
    // Resize an inner (nested) grid divider — a non-root path.
    useViewerStore.getState().setForegroundNodeSizes(TEAM, [0], [70, 30]);
    const root = layout()!.root as Extract<MosaicNode, { kind: "split" }>;
    expect(root).not.toBeNull(); // materialized
    expect((root.children[0] as Extract<MosaicNode, { kind: "split" }>).sizes).toEqual([70, 30]);
    expect(layout()!.preset).toBe("grid"); // still grid (re-clicking Grid re-balances)
  });
});

describe("effectiveMosaicRoot", () => {
  it("derives a flat depth-1 tree from columns/rows when root is null", () => {
    const root = effectiveMosaicRoot(undefined, [A, B, C]);
    expect(root).toMatchObject({ kind: "split", axis: "row" });
    expect(leafWorkspaceIds(root)).toEqual([A, B, C]);
  });

  it("derives a balanced grid when preset is grid", () => {
    const fl: ForegroundLayout = {
      sizes: [],
      direction: "row",
      preset: "grid",
      root: null,
    };
    const root = effectiveMosaicRoot(fl, [A, B, C, "e-1-d"]) as Extract<
      MosaicNode,
      { kind: "split" }
    >;
    expect(root.axis).toBe("column"); // a column of rows
    expect(root.children).toHaveLength(2);
  });

  it("honors a stored custom root and reconciles to the live set", () => {
    const fl: ForegroundLayout = {
      sizes: [],
      direction: "row",
      preset: "custom",
      root: {
        kind: "split",
        axis: "row",
        children: [
          { kind: "leaf", workspaceId: A },
          { kind: "leaf", workspaceId: B },
        ],
        sizes: [50, 50],
      },
    };
    // C is live but absent from the stored root → appended by reconcile.
    expect(leafWorkspaceIds(effectiveMosaicRoot(fl, [A, B, C]))).toEqual([A, B, C]);
  });
});
