// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

// The action's run() resolves the active team via getActiveWorkspaceContext;
// mock it so we can drive both the happy path and the NO_TEAM guard.
const getActiveWorkspaceContextMock = vi.fn();
vi.mock("@/shared/hooks/use-active-workspace-context", () => ({
  getActiveWorkspaceContext: () => getActiveWorkspaceContextMock(),
  // The CLI shows the active team in these tests.
}));

import { NO_TEAM_SENTINEL } from "@/shared/lib/team-key";
import { useViewerStore, workspaceTabGridPreset } from "@/shared/stores/viewer-store";
import { seedTerminalTabs, installTerminalLayoutHarness } from "@/test/terminal-layout-harness";
import { SHORTCUT_ACTIONS } from "./registry";

installTerminalLayoutHarness();

const TEAM = "acme:default";

function runCycle() {
  const action = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.cycle-foreground-layout");
  if (!action) throw new Error("viewer.cycle-foreground-layout not registered");
  action.run();
}

describe("viewer.cycle-foreground-layout run() (SW-CW-7)", () => {
  beforeEach(() => {
    useViewerStore.setState({
      foregroundLayout: {},
      foregroundLayoutByRail: {},
      trayMode: {},
      selectedEpicDirName: {},
    });
    vi.clearAllMocks();
  });

  it("advances the active team's foreground preset (⌘⇧L dispatch)", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM });
    const background = {
      preset: "grid" as const,
      direction: "row" as const,
      sizes: [],
      root: null,
    };
    useViewerStore.setState({ foregroundLayout: { "acme:background": background } });

    runCycle(); // absent slot reads as "columns" → lands on "rows"

    expect(useViewerStore.getState().foregroundLayout[TEAM]?.preset).toBe("rows");
    expect(useViewerStore.getState().foregroundLayout["acme:background"]).toEqual(background);
  });

  it("no-ops when no team is active (NO_TEAM_SENTINEL guard)", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: NO_TEAM_SENTINEL });

    runCycle();

    // The guard returns before any store write: if `teamKey === NO_TEAM_SENTINEL`
    // early-return were removed, run() would create a slot under the sentinel key.
    expect(useViewerStore.getState().foregroundLayout[NO_TEAM_SENTINEL]).toBeUndefined();
  });

  // SW-EW-46 (D4/AC-6): when the expanded workspace is gridded, ⌘⇧L cycles the
  // TAB-grid preset, not the workspace mosaic.
  it("cycles the tab-grid preset (not the mosaic) when the expanded workspace is gridded", () => {
    const WS = "story:acme/repo/x";
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: TEAM });
    // A lone (→ expanded) workspace with a non-empty paned set = grid mode active.
    useViewerStore.setState({
      workspaces: {
        [TEAM]: [{ id: WS, kind: "story", cwd: "", status: "ready" }],
      } as never,
    });
    seedTerminalTabs(TEAM, WS, ["a"], { paned: ["a"] });

    runCycle();

    // The tab-grid preset advanced (columns → rows) …
    expect(workspaceTabGridPreset(useViewerStore.getState(), TEAM, WS)).toBe("rows");
    // … and the workspace mosaic layout was left untouched.
    expect(useViewerStore.getState().foregroundLayout[TEAM]).toBeUndefined();
  });
});
