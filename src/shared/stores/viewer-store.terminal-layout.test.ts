// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";

import {
  selectActiveTerminalTab,
  selectRoleTab,
  selectTerminalTabIndex,
  selectMinimizedTerminalTabIndex,
  selectWorkspaceKey,
  selectWorkspaceTabs,
  useViewerStore,
  type Workspace,
} from "@/shared/stores/viewer-store";
import { makeRealLaunchDeps } from "@/shared/lib/agent-session-launcher";
import {
  seedTerminalTabs,
  terminalHost,
  installTerminalLayoutHarness,
} from "@/test/terminal-layout-harness";

installTerminalLayoutHarness();

const TEAM = "acme/app:core";
const OTHER_TEAM = "acme/app:infra";

function ws(id: string, fields: Partial<Workspace> = {}): Workspace {
  return { id, kind: "generic", cwd: `/w/${id}`, status: "ready", ...fields };
}

const s = () => useViewerStore.getState();
const tabs = (teamKey: string, id: string) =>
  selectWorkspaceTabs(s(), teamKey, id).map((t) => t.name);

beforeEach(() => {
  useViewerStore.setState({
    workspaces: { [TEAM]: [ws("w1"), ws("w2")] },
    activeWorkspace: { [TEAM]: "w1" },
    softClosedWorkspaces: {},
  });
});

describe("the viewer store is a projection of the host's terminal layout", () => {
  it("a new terminal opens through its host and shows once the host announces it", async () => {
    await s().addTerminalTab(TEAM, { sessionId: "s1" }, "w1");
    expect(terminalHost().opened.map((r) => r.workspace)).toEqual([`${TEAM}|w1`]);
    expect(tabs(TEAM, "w1")).toEqual(["Terminal 1"]);
    expect(selectActiveTerminalTab(s(), TEAM, "w1")).toBe("s1");
  });

  it("opens at the workspace's directory, a warm shell unless the env must be stripped", async () => {
    await s().addTerminalTab(TEAM, { sessionId: "warm" }, "w1");
    await s().addTerminalTab(TEAM, { sessionId: "cold" }, "w1", { envRemove: ["KEY"] });
    const [warm, cold] = terminalHost().opened.map((r) => r.spawn);
    expect(warm).toMatchObject({ kind: "warm", targetCwd: "/w/w1" });
    expect(cold).toMatchObject({ kind: "cold", cwd: "/w/w1", envRemove: ["KEY"] });
  });

  it("a direct exec reaches the host as a cold spawn of its argv, with no shell typing", async () => {
    const execution = { argv: ["/bin/hermes", "chat", "--tui"], nativeLaunch: "hermes" as const };
    // The launcher's real open, which the dashboard's launch and the Launch sheet share.
    await makeRealLaunchDeps(async () => {}).openTab(
      TEAM,
      "w1",
      { sessionId: "direct", role: "agent" } as never,
      {
        cwd: "/w/w1",
        envRemove: [],
        execution,
      },
    );
    const [direct] = terminalHost().opened.map((r) => r.spawn);
    expect(direct).toMatchObject({
      kind: "cold",
      commandArgv: execution.argv,
      nativeLaunch: "hermes",
    });
  });

  it("a respawn hands back the fresh execution its host started", async () => {
    seedTerminalTabs(TEAM, "w1", ["gone"]);
    terminalHost().exit("gone", 1);
    const execution = { argv: ["/fresh/hermes", "chat", "--tui"], nativeLaunch: "hermes" as const };
    const host = terminalHost();
    const real = host.respawnTab.bind(host);
    vi.spyOn(host, "respawnTab").mockImplementation((...args) => ({ ...real(...args), execution }));
    const respawned = await s().respawnTerminalTab(TEAM, "gone", "w1", {});
    expect(respawned.execution).toEqual(execution);
  });

  it("every tab action is a host verb: rename, reorder, focus, close", async () => {
    seedTerminalTabs(TEAM, "w1", ["a", "b", "c"]);
    s().renameTerminalTab(TEAM, "b", "build");
    s().reorderTerminalTab(TEAM, 2, 0, "w1");
    s().setActiveTerminalTab(TEAM, "c", "w1");
    await Promise.resolve();
    expect(tabs(TEAM, "w1")).toEqual(["Terminal 3", "Terminal 1", "build"]);
    expect(selectActiveTerminalTab(s(), TEAM, "w1")).toBe("c");
    s().removeTerminalTab(TEAM, "a", "w1");
    await new Promise((r) => setTimeout(r));
    expect(tabs(TEAM, "w1")).toEqual(["Terminal 3", "build"]);
  });

  it("a busy terminal closed with × keeps running out of sight; an idle one ends", async () => {
    seedTerminalTabs(TEAM, "w1", ["busy", "idle", "other"]);
    terminalHost().setBusy("busy", true);
    s().removeTerminalTab(TEAM, "busy", "w1");
    s().removeTerminalTab(TEAM, "idle", "w1");
    await new Promise((r) => setTimeout(r));
    expect(terminalHost().contains("busy")).toBe(true);
    expect(terminalHost().contains("idle")).toBe(false);
    expect(tabs(TEAM, "w1")).toEqual(["Terminal 3"]);
  });

  it("closing the last ordinary tab opens a fresh one; a role tab's does not", async () => {
    seedTerminalTabs(TEAM, "w1", ["only"]);
    await s().removeTerminalTab(TEAM, "only", "w1");
    await new Promise((r) => setTimeout(r));
    expect(selectWorkspaceTabs(s(), TEAM, "w1")).toHaveLength(1);
    expect(terminalHost().opened).toHaveLength(1);

    seedTerminalTabs(TEAM, "w2", [{ session: "agent", role: "test-agent" }]);
    await s().removeTerminalTab(TEAM, "agent", "w2");
    await new Promise((r) => setTimeout(r));
    expect(selectWorkspaceTabs(s(), TEAM, "w2")).toHaveLength(0);
    expect(terminalHost().opened).toHaveLength(1);
  });

  it("an exited tab stays read-only with its real exit status until it is closed", async () => {
    seedTerminalTabs(TEAM, "w1", ["build", "shell"]);
    terminalHost().exit("build", 2);
    expect(selectWorkspaceTabs(s(), TEAM, "w1")[0].exit).toEqual({ code: 2, signal: null });
    s().removeTerminalTab(TEAM, "build", "w1");
    await new Promise((r) => setTimeout(r));
    expect(tabs(TEAM, "w1")).toEqual(["Terminal 2"]);
  });

  it("the grid dial sends the workspace's grid to its host", () => {
    seedTerminalTabs(TEAM, "w1", ["a", "b", "c"]);
    s().paneOutTerminalTab(TEAM, "w1", "c");
    s().paneOutTerminalTab(TEAM, "w1", "a");
    expect(s().terminalLayout.byKey[`${TEAM}|w1`].grid.paned).toEqual(["a", "c"]);
    s().collapseTabGridToTabs(TEAM, "w1");
    s().cycleTabGridPreset(TEAM, "w1");
    expect(s().terminalLayout.byKey[`${TEAM}|w1`].grid).toMatchObject({
      paned: ["a", "c"],
      collapsed: true,
      preset: "rows",
    });
    s().demoteTerminalPane(TEAM, "w1", "a");
    expect(s().terminalLayout.byKey[`${TEAM}|w1`].grid.paned).toEqual(["c"]);
  });
});

describe("minimize keeps the terminals in their host", () => {
  it("a minimized workspace's tabs stay, read as minimized, and come back on reopen", () => {
    seedTerminalTabs(TEAM, "w1", ["a", "b"]);
    s().closeWorkspace(TEAM, "w1");
    expect(terminalHost().contains("a")).toBe(true);
    expect(s().softClosedWorkspaces[TEAM].w1).toEqual({
      workspace: ws("w1"),
    });
    expect(selectTerminalTabIndex(s())[TEAM]?.w1).toBeUndefined();
    expect(selectMinimizedTerminalTabIndex(s())[TEAM].w1.map((t) => t.id)).toEqual(["a", "b"]);
    s().reopenSoftClosedWorkspace(TEAM, "w1");
    expect(selectTerminalTabIndex(s())[TEAM].w1.map((t) => t.id)).toEqual(["a", "b"]);
  });

  it("close & kill ends the minimized workspace's terminals in their host", () => {
    seedTerminalTabs(TEAM, "w1", ["a"]);
    s().closeWorkspace(TEAM, "w1");
    s().killTerminalsForSoftClosedWorkspace(TEAM, "w1");
    expect(terminalHost().contains("a")).toBe(false);
    expect(s().softClosedWorkspaces[TEAM]?.w1).toBeUndefined();
  });
});

describe("AC-9: workspace keys are team-scoped", () => {
  it("two teams' same-named workspaces keep separate host layouts", () => {
    useViewerStore.setState({
      workspaces: {
        [TEAM]: [ws("test-agent", { kind: "generic" }), ws("root:acme/app", { kind: "root" })],
        [OTHER_TEAM]: [
          ws("test-agent", { kind: "generic" }),
          ws("root:acme/app", { kind: "root" }),
        ],
      },
    });
    seedTerminalTabs(TEAM, "test-agent", [{ session: "gs-core", role: "test-agent" }]);
    seedTerminalTabs(OTHER_TEAM, "test-agent", [{ session: "gs-infra", role: "test-agent" }]);
    seedTerminalTabs(TEAM, "root:acme/app", ["root-core"]);
    seedTerminalTabs(OTHER_TEAM, "root:acme/app", ["root-infra"]);
    expect(selectWorkspaceTabs(s(), TEAM, "test-agent").map((t) => t.id)).toEqual(["gs-core"]);
    expect(selectWorkspaceTabs(s(), OTHER_TEAM, "test-agent").map((t) => t.id)).toEqual([
      "gs-infra",
    ]);
    expect(selectWorkspaceTabs(s(), OTHER_TEAM, "root:acme/app").map((t) => t.id)).toEqual([
      "root-infra",
    ]);
    expect(selectRoleTab(s(), OTHER_TEAM, "test-agent")?.tab.id).toBe("gs-infra");
  });

  it("a story workspace keys by its frontmatter story_id, the key an MCP open lands in", () => {
    const story = ws("SW-9-long-file-stem", { kind: "story", storyId: "SW-9" });
    useViewerStore.setState({ workspaces: { [TEAM]: [story] } });
    expect(selectWorkspaceKey(s(), TEAM, story.id)).toBe(`${TEAM}|SW-9`);
    // What `open_workspace` (MCP) places: `{teamKey}|{story_id}`.
    terminalHost().seed(`${TEAM}|SW-9`, { displayName: "SW-9", kind: "story" }, { session: "mcp" });
    expect(selectWorkspaceTabs(s(), TEAM, story.id).map((t) => t.id)).toEqual(["mcp"]);
  });

  it("a story whose story_id arrives moves its terminals to the new key", async () => {
    const story = ws("SW-9-long-file-stem", { kind: "story", storyFilePath: "/c/SW-9.md" });
    useViewerStore.setState({ workspaces: { [TEAM]: [story] } });
    seedTerminalTabs(TEAM, story.id, ["t1"]);
    s().syncWorkspaceFrontmatter(TEAM, {
      stories: [{ filePath: "/c/SW-9.md", storyId: "SW-9", status: "ready" }],
      subEpics: [],
    } as never);
    await new Promise((r) => setTimeout(r));
    expect(selectWorkspaceKey(s(), TEAM, story.id)).toBe(`${TEAM}|SW-9`);
    expect(selectWorkspaceTabs(s(), TEAM, story.id).map((t) => t.id)).toEqual(["t1"]);
  });
});
