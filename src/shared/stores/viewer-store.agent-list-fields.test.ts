// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

const pushViewerState = vi.hoisted(() => vi.fn().mockResolvedValue(undefined));
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState,
}));

const { applyGlobalViewerPrefs, useViewerStore, viewerMirrorFields } =
  await import("./viewer-store");
const { useAgentListFacts } = await import("@/features/viewer/lib/agent-list-facts");

describe("full viewer-state pushes keep the agent-list fields", () => {
  beforeEach(() => {
    pushViewerState.mockClear();
    useViewerStore.setState({ stateLoaded: true });
    useAgentListFacts.setState({
      sessions: { s1: { face: "avatar:ada", faceTag: "ada", pinned: true } },
      pane: { showFaces: true },
    });
  });

  it("a global-prefs apply (the debounced-save path) still carries agentSessionFacts", () => {
    applyGlobalViewerPrefs({});
    const pushed = pushViewerState.mock.lastCall?.[0];
    expect(pushed.agentSessionFacts).toEqual({
      s1: { face: "avatar:ada", faceTag: "ada", pinned: true },
    });
    expect(pushed).toHaveProperty("agentSessionModels");
    expect(pushed).toHaveProperty("agentPendingAsks");
    expect(pushed).toHaveProperty("agentStageStates");
    expect(pushed.agentPane).toMatchObject({ showFaces: true });
  });
});

describe("the viewer's mirror fields", () => {
  it("carry the one-time tips, the workspaces and the pinned status paths", () => {
    useViewerStore.setState({
      stateLoaded: true,
      workspaceTraySeenCoachmark: true,
      statusNavPinnedPaths: { "org:team": ["stories"] },
    });
    const fields = viewerMirrorFields(useViewerStore.getState());
    expect(fields.workspaceTraySeenCoachmark).toBe(true);
    expect(fields.statusNavPinnedPaths).toEqual({ "org:team": ["stories"] });
    expect(fields).toHaveProperty("workspaces");
    expect(fields).toHaveProperty("softClosedWorkspaces");
    expect(fields).toHaveProperty("activeTab");
  });

  it("are what a full push carries beside the agent-list fields", () => {
    pushViewerState.mockClear();
    useViewerStore.setState({ stateLoaded: true, workspaceTraySeenCoachmark: true });
    applyGlobalViewerPrefs({});
    const pushed = pushViewerState.mock.lastCall?.[0];
    expect(pushed).toMatchObject({ workspaceTraySeenCoachmark: true });
    for (const key of Object.keys(viewerMirrorFields(useViewerStore.getState()))) {
      expect(pushed).toHaveProperty(key);
    }
  });
});
