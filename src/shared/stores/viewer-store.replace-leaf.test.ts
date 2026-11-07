// SPDX-License-Identifier: MIT
import { useOrgStore } from "./org-store";
import { describe, it, expect, beforeEach, vi } from "vitest";

// SW-EW-33 — replaceFocusedLeafWorkspace: click-to-replace swaps the focused
// mosaic leaf's occupant for a minimized workspace IN PLACE (pane count + slot
// invariant), the displaced workspace soft-closes (PTYs kept), the incoming
// surfaces in that leaf.

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
  effectiveMosaicRoot,
  selectMinimizedTerminalTabIndex,
  selectWorkspaceKey,
  teamTileOrder,
  type SoftClosedWorkspace,
  type Workspace,
} from "./viewer-store";
import {
  seedTerminalTabs,
  terminalHost,
  installTerminalLayoutHarness,
} from "@/test/terminal-layout-harness";
import { foregroundTilesFor } from "@/features/viewer/lib/foreground-tiles";
import { leafWorkspaceIds, findLeafPath } from "@/features/viewer/lib/mosaic-tree";

const TEAM = "acme:default";
const A = "e-1-a";
const B = "e-1-b";
const M = "e-1-minimized";

installTerminalLayoutHarness();

const session = (id: string) => `sess-${id}`;
const ws = (id: string, kind: Workspace["kind"] = "story"): Workspace => ({
  id,
  kind,
  cwd: `/${id}`,
  status: "ready",
});
const shadow = (id: string): SoftClosedWorkspace => ({ workspace: ws(id) });

const renderedLeaves = (): string[] => {
  const s = useViewerStore.getState();
  const orderedIds = foregroundTilesFor(s.workspaces[TEAM] ?? [], teamTileOrder(s, TEAM)).map(
    (w) => w.id,
  );
  return leafWorkspaceIds(effectiveMosaicRoot(s.foregroundLayout[TEAM], orderedIds));
};

beforeEach(() => {
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
  useViewerStore.setState({
    foregroundLayout: {},
    activeWorkspace: { [TEAM]: A },
    workspaces: { [TEAM]: [ws(A), ws(B)] },
    softClosedWorkspaces: { [TEAM]: { [M]: shadow(M) } },
  });
  for (const id of [A, B, M]) seedTerminalTabs(TEAM, id, [session(id)]);
  vi.clearAllMocks();
});

describe("replaceFocusedLeafWorkspace — default preset (root === null)", () => {
  it("swaps the focused leaf in place: pane count + slot invariant", () => {
    expect(renderedLeaves()).toEqual([A, B]); // baseline
    useViewerStore.getState().replaceFocusedLeafWorkspace(TEAM, M);

    const leaves = renderedLeaves();
    expect(leaves).toHaveLength(2); // pane count invariant
    expect(leaves).toEqual([M, B]); // M took A's exact slot (position invariant)
    expect(findLeafPath(useViewerStore.getState().foregroundLayout[TEAM]!.root, M)).toEqual([0]);
  });

  it("incoming becomes the active workspace and is on-screen; F is soft-closed", () => {
    useViewerStore.getState().replaceFocusedLeafWorkspace(TEAM, M);
    const s = useViewerStore.getState();
    expect(s.activeWorkspace[TEAM]).toBe(M);
    expect(s.workspaces[TEAM]?.some((w) => w.id === M)).toBe(true);
    expect(s.workspaces[TEAM]?.some((w) => w.id === A)).toBe(false);
    // A is now a tray chip: shadow present, M's shadow consumed.
    expect(s.softClosedWorkspaces[TEAM]?.[A]).toBeDefined();
    expect(s.softClosedWorkspaces[TEAM]?.[M]).toBeUndefined();
  });

  it("displaced workspace keeps its terminals running in their host", () => {
    useViewerStore.getState().replaceFocusedLeafWorkspace(TEAM, M);
    expect(terminalHost().contains(session(A))).toBe(true);
    // A's tabs now read as minimized.
    expect(
      selectMinimizedTerminalTabIndex(useViewerStore.getState())[TEAM]?.[A]?.map((t) => t.id),
    ).toEqual([session(A)]);
  });
});

describe("replaceFocusedLeafWorkspace — custom split", () => {
  it("preserves arrangement, swapping only the focused leaf", () => {
    // Make a custom split [A, B] then focus B.
    useViewerStore
      .getState()
      .placeWorkspaceTile(TEAM, B, { targetWorkspaceId: A, axis: "row", side: "after" });
    useViewerStore.setState({ activeWorkspace: { [TEAM]: B } });

    useViewerStore.getState().replaceFocusedLeafWorkspace(TEAM, M);
    expect(renderedLeaves()).toEqual([A, M]); // only B's slot changed
  });
});

describe("replaceFocusedLeafWorkspace — fallbacks (no replaceable leaf)", () => {
  it("empty mosaic (no focused workspace) → surfaces the incoming via center reopen", () => {
    useViewerStore.setState({
      workspaces: { [TEAM]: [] },
      activeWorkspace: {},
    });
    useViewerStore.getState().replaceFocusedLeafWorkspace(TEAM, M);
    const s = useViewerStore.getState();
    expect(s.workspaces[TEAM]?.some((w) => w.id === M)).toBe(true); // surfaced
    expect(s.activeWorkspace[TEAM]).toBe(M);
    expect(terminalHost().contains(session(M))).toBe(true);
  });

  it("focused tile is a root → soft-closes it like any workspace (root is now closable — SW-GS-2a)", () => {
    useViewerStore.setState({
      workspaces: { [TEAM]: [ws("root-1", "root"), ws(B)] },
      activeWorkspace: { [TEAM]: "root-1" },
    });
    seedTerminalTabs(TEAM, "root-1", [session("root-1")]);
    useViewerStore.getState().replaceFocusedLeafWorkspace(TEAM, M);
    const s = useViewerStore.getState();
    // root no longer special: it is shadowed (PTYs kept) and incoming swaps in.
    expect(s.softClosedWorkspaces[TEAM]?.["root-1"]).toBeDefined();
    expect(s.workspaces[TEAM]?.some((w) => w.id === M)).toBe(true);
  });

  it("incoming is not minimized (no shadow) → no-op", () => {
    useViewerStore.setState({ softClosedWorkspaces: { [TEAM]: {} } });
    const before = useViewerStore.getState().activeWorkspace[TEAM];
    useViewerStore.getState().replaceFocusedLeafWorkspace(TEAM, M);
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(before); // unchanged
  });

  it("a swap never destroys the displaced workspace, even with zero terminals", () => {
    // F (=A) has no live terminals — a REPLACE must still retain it as a chip
    // (unlike closeWorkspace, where 0 tabs hard-closes).
    terminalHost().closeWorkspace(selectWorkspaceKey(useViewerStore.getState(), TEAM, A));
    useViewerStore.getState().replaceFocusedLeafWorkspace(TEAM, M);
    const s = useViewerStore.getState();
    expect(s.softClosedWorkspaces[TEAM]?.[A]).toBeDefined(); // A retained as a tray chip
    expect(s.workspaces[TEAM]?.some((w) => w.id === A)).toBe(false);
    expect(s.activeWorkspace[TEAM]).toBe(M);
  });
});

describe("replaceFocusedLeafWorkspace — SW-FX-35 tray pair coherence (AC-3b)", () => {
  it("restores into a visible tray from a cold viewer layoutMode", () => {
    useViewerStore.setState({ layoutMode: "viewer", lastTerminalMode: "shared" });
    useViewerStore.getState().replaceFocusedLeafWorkspace(TEAM, M);
    const s = useViewerStore.getState();
    expect(s.terminalVisible).toBe(true);
    expect(s.layoutMode).not.toBe("viewer");
  });

  it("the tray-chip / keyboard route (activateTrayWorkspace) also satisfies the pair", () => {
    // M is soft-closed (seeded in beforeEach) → activateTrayWorkspace, the entry
    // for chip click / Cmd+1..9 / flip-to-last, routes a minimized workspace to
    // replaceFocusedLeafWorkspace. From a cold viewer state it must show the tray.
    useViewerStore.setState({
      layoutMode: "viewer",
      lastTerminalMode: "console",
    });
    useViewerStore.getState().activateTrayWorkspace(TEAM, M);
    const s = useViewerStore.getState();
    expect(s.terminalVisible).toBe(true);
    expect(s.layoutMode).toBe("console"); // AC-4: respects lastTerminalMode
  });

  it("does not clobber the split that replaceFocusedLeafWorkspace builds (foregroundLayout intact)", () => {
    useViewerStore.setState({ layoutMode: "viewer", lastTerminalMode: "shared" });
    useViewerStore.getState().replaceFocusedLeafWorkspace(TEAM, M);
    // The in-place swap + preset are preserved (setLayoutMode touches only the
    // layout-mode/visibility slices, never foregroundLayout).
    expect(renderedLeaves()).toEqual([M, B]);
  });
});

describe("replaceFocusedLeafWorkspace — MRU integration", () => {
  it("records the displaced workspace so Cmd+Alt+; flips back to it", () => {
    useViewerStore.getState().switchWorkspace(TEAM, A); // seed MRU with A focused
    useViewerStore.getState().replaceFocusedLeafWorkspace(TEAM, M); // A displaced, M active
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(M);
    useViewerStore.getState().flipLastWorkspace(TEAM); // → back to the just-displaced A
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(A);
  });
});
