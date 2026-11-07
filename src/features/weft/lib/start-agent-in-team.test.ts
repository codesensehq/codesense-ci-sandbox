// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// The empty CLI's "Start an agent", over the real viewer store; the terminal host's open and
// the focus are faked.
vi.mock("@/features/viewer/lib/focus-cascade", () => ({ focusActiveWorkspaceTerminal: vi.fn() }));
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  saveAppState: vi.fn(async () => {}),
  pushViewerState: vi.fn(async () => {}),
}));

import { genericWorkspaceId, useViewerStore, type Workspace } from "@/shared/stores/viewer-store";

import { startAgentInTeam } from "./open-fresh-agent";

const TEAM = "acme:ops";
const real = useViewerStore.getState();
const addTerminalTab = vi.fn(() => Promise.resolve());
const ws = (id: string): Workspace => ({ id, kind: "generic", cwd: `/${id}`, status: "ready" });

beforeEach(() => {
  addTerminalTab.mockClear();
  useViewerStore.setState({
    openedTeams: [TEAM],
    workspaces: { [TEAM]: [] },
    activeWorkspace: {},
    softClosedWorkspaces: {},
    addTerminalTab,
  });
});
afterEach(() => useViewerStore.setState({ addTerminalTab: real.addTerminalTab }));

describe("startAgentInTeam", () => {
  it("in a team with no workspace, opens a tile for the team's folder and the agent in it", () => {
    const sessionId = startAgentInTeam(TEAM, "/orgs/acme/content/ops");
    const s = useViewerStore.getState();
    const id = genericWorkspaceId("/orgs/acme/content/ops");
    expect(s.workspaces[TEAM].map((w) => w.id)).toEqual([id]);
    expect(s.activeWorkspace[TEAM]).toBe(id);
    expect(addTerminalTab).toHaveBeenCalledWith(TEAM, expect.objectContaining({ sessionId }));
  });

  it("in a team with a focused workspace, starts the agent there", () => {
    useViewerStore.setState({
      workspaces: { [TEAM]: [ws("o1")] },
      activeWorkspace: { [TEAM]: "o1" },
    });
    startAgentInTeam(TEAM, "/orgs/acme/content/ops");
    expect(useViewerStore.getState().workspaces[TEAM].map((w) => w.id)).toEqual(["o1"]);
    expect(addTerminalTab).toHaveBeenCalledTimes(1);
  });

  it("with neither a workspace nor a folder, starts nothing", () => {
    expect(startAgentInTeam(TEAM, "")).toBeNull();
    expect(addTerminalTab).not.toHaveBeenCalled();
  });
});
