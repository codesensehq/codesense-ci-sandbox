// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

/**
 * Where a named agent's terminal lands. A tab renders only inside a viewer workspace tile, so
 * a headed launch must open its tab in one and focus it, while a headless launch runs out of
 * sight. Driven through the open-workspace dispatch and the controller's REAL launch deps,
 * with the REAL viewer store and the test terminal host; only the IPC is faked (the backend's
 * `agent_launch_prepare` by the RF-29 mock backend's handler).
 */

const { invoke, bindCalls, prepareCalls } = vi.hoisted(() => ({
  invoke: { router: null as null | ((cmd: string, args?: Record<string, unknown>) => unknown) },
  bindCalls: [] as Array<{ handle: string; sessionId: string }>,
  prepareCalls: [] as Array<Record<string, unknown>>,
}));

vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  saveAppState: vi.fn(async () => undefined),
  dashboardRegisterSession: vi.fn(async () => undefined),
  dashboardLaunchBegin: vi.fn(async () => true),
  dashboardLaunchAbort: vi.fn(async () => undefined),
  dashboardResolveSession: vi.fn(async () => null),
  dashboardProbeSessionsAlive: vi.fn(async () => []),
  dashboardLaunchBind: vi.fn(async (handle: string, sessionId: string) => {
    bindCalls.push({ handle, sessionId });
  }),
  agentLaunchPrepare: vi.fn(async (p: Record<string, unknown>) => {
    prepareCalls.push(p);
    return invoke.router!("agent_launch_prepare", p);
  }),
  ensureBranchWorkspace: vi.fn(async (p: Record<string, unknown>) => ({
    slug: p.repoSlug,
    dirPath: "/wt/agent-branch",
    registered: true,
  })),
}));

vi.mock("@/shared/lib/agent-session-launcher", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/shared/lib/agent-session-launcher")>()),
  waitForAgentLive: vi.fn(async () => true),
}));

vi.mock("@/shared/lib/agent-session-injection", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/shared/lib/agent-session-injection")>()),
  // The test host runs no shell: the launch line and the seed land at once.
  injectWhenReady: vi.fn(async () => ({ ok: true })),
  injectAgentLine: vi.fn(async () => ({ ok: true })),
}));

vi.mock("@/shared/lib/default-agent", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/shared/lib/default-agent")>()),
  resolveAgentCliPath: async (basename = "claude") => `/opt/bin/${basename}`,
}));

vi.mock("@/shared/lib/team-key", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/shared/lib/team-key")>()),
  getActiveTeamKey: () => "acme:backend",
}));

vi.mock("./dashboard-launch-status", () => ({ armLaunchStatus: () => undefined }));

import { createInvokeRouter } from "@/app/mock/mock-router";
import { agentLaunchHandlers } from "@/app/mock/handlers/agents-launch";
import { installTerminalLayoutHarness, seedTerminalTabs } from "@/test/terminal-layout-harness";
import {
  genericWorkspaceId,
  selectActiveTerminalTab,
  selectOpenTabLocation,
  selectWorkspaceTabs,
  useViewerStore,
} from "@/shared/stores/viewer-store";
import {
  dispatchOpenWorkspace,
  makeRealBranchLaunchDeps,
  makeRealDashboardLaunchDeps,
  makeRealHeadlessLaunchers,
} from "./dashboard-launch-controller";
import type { BranchLaunchDeps, OpenWorkspaceArg } from "./dashboard-launch";
import {
  dashboardResolveSession,
  dashboardLaunchBegin,
  dashboardProbeSessionsAlive,
} from "@/tauri/commands";

installTerminalLayoutHarness();

const DOC = "/ctx/boards/agents.html";
const TEAM = "acme:backend";
const DIR = "/wt/existing";
const DIR_TILE = genericWorkspaceId(DIR);

function branchDeps(): BranchLaunchDeps {
  return {
    ...makeRealBranchLaunchDeps(DOC),
    resolveBranch: async () => ({
      repoPath: "/src/api",
      teamKey: TEAM,
      codesenseHome: "/home/.codesense",
      githubUserId: 1,
      orgSlug: "acme",
      team: "backend",
    }),
  };
}

function open(arg: OpenWorkspaceArg) {
  return dispatchOpenWorkspace(
    arg,
    DOC,
    makeRealDashboardLaunchDeps(DOC),
    branchDeps(),
    { openGenericWorkspace: vi.fn() },
    makeRealHeadlessLaunchers(DOC),
  );
}

const tiles = () => (useViewerStore.getState().workspaces[TEAM] ?? []).map((w) => w.id);
const tabsOf = (workspaceId: string) =>
  selectWorkspaceTabs(useViewerStore.getState(), TEAM, workspaceId).map((t) => t.sessionId);

beforeEach(() => {
  invoke.router = createInvokeRouter(agentLaunchHandlers());
  bindCalls.length = 0;
  prepareCalls.length = 0;
  useViewerStore.setState({
    workspaces: {},
    softClosedWorkspaces: {},
    activeWorkspace: {},
    terminalVisible: false,
    layoutMode: null,
  });
});

describe("a named agent's terminal", () => {
  it("dir, headed: opens a focused tab in the dir's generic workspace tile, bound to the agent session", async () => {
    const result = await open({
      target: { kind: "dir", path: DIR, agent: "triage" },
      headed: true,
      initialPrompt: "/bmad-help",
    });

    const sessionId = result.sessionId!;
    // The agent was prepared for exactly this session, in the dir.
    expect(prepareCalls).toEqual([
      expect.objectContaining({ agentKey: "triage", cwd: DIR, sessionId, prompt: "/bmad-help" }),
    ]);
    expect(bindCalls).toEqual([{ handle: DIR, sessionId }]);
    // The tab lives in a visible tile, alone (no stray shell), focused, terminal shown.
    const state = useViewerStore.getState();
    expect(state.workspaces[TEAM]).toEqual([
      expect.objectContaining({ id: DIR_TILE, kind: "generic", cwd: DIR }),
    ]);
    expect(tabsOf(DIR_TILE)).toEqual([sessionId]);
    expect(selectActiveTerminalTab(state, TEAM, DIR_TILE)).toBe(sessionId);
    expect(state.terminalVisible).toBe(true);
    expect(result.promptDelivered).toBe(true);
  });

  it("dir, headed: a launch that fails leaves no empty tile behind", async () => {
    const unhandled = vi.fn();
    process.on("unhandledRejection", unhandled);
    try {
      await expect(
        open({ target: { kind: "dir", path: DIR, agent: "nobody" }, headed: true }),
      ).rejects.toThrow('no agent "nobody"');
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(unhandled).not.toHaveBeenCalled();
      expect(tiles()).toEqual([]);
      expect(bindCalls).toEqual([]);
    } finally {
      process.off("unhandledRejection", unhandled);
    }
  });

  it("dir, headless: runs the agent in the background with no tile", async () => {
    const result = await open({
      target: { kind: "dir", path: DIR, agent: "triage" },
      headed: false,
    });

    const sessionId = result.sessionId!;
    expect(bindCalls).toEqual([{ handle: DIR, sessionId }]);
    expect(tiles()).toEqual([]);
    expect(selectOpenTabLocation(useViewerStore.getState(), sessionId)?.workspaceId).toBe(DIR);
    // The CLI chrome is untouched: still hidden, its layout never settled.
    expect(useViewerStore.getState()).toMatchObject({ terminalVisible: false, layoutMode: null });
  });

  it("dir, headed: a re-open of an agent running out of sight brings it into the tile", async () => {
    seedTerminalTabs(TEAM, DIR, [{ session: "bg-1", role: `dir-agent:${DIR}` }]);
    vi.mocked(dashboardLaunchBegin).mockResolvedValueOnce(false);
    vi.mocked(dashboardResolveSession).mockResolvedValueOnce("bg-1");
    vi.mocked(dashboardProbeSessionsAlive).mockResolvedValueOnce(["bg-1"]);
    // The out-of-sight workspace's descriptor names its dir, as a launch records it.
    useViewerStore.setState((s) => {
      const key = Object.keys(s.terminalLayout.byKey).find((k) => k.endsWith(DIR))!;
      const byKey = { ...s.terminalLayout.byKey };
      byKey[key] = { ...byKey[key], descriptor: { ...byKey[key].descriptor, worktreePath: DIR } };
      return { terminalLayout: { ...s.terminalLayout, byKey } };
    });

    const result = await open({
      target: { kind: "dir", path: DIR, agent: "triage" },
      headed: true,
    });

    expect(result.sessionId).toBe("bg-1");
    expect(prepareCalls).toEqual([]);
    await vi.waitFor(() => expect(tabsOf(DIR_TILE)).toEqual(["bg-1"]));
    expect(selectActiveTerminalTab(useViewerStore.getState(), TEAM, DIR_TILE)).toBe("bg-1");
    expect(useViewerStore.getState().terminalVisible).toBe(true);
  });

  it("dir, headed: a re-open of an agent the user hid puts it back on its tile, focused", async () => {
    useViewerStore.getState().openWorkspace({
      teamKey: TEAM,
      workspaceId: DIR_TILE,
      kind: "generic",
      cwd: DIR,
      spawn: false,
    });
    seedTerminalTabs(TEAM, DIR_TILE, ["shell-1", { session: "bg-2", role: `dir-agent:${DIR}` }]);
    expect(useViewerStore.getState().hideTerminalTab(TEAM, "bg-2", DIR_TILE)).toBe(true);
    await vi.waitFor(() => expect(tabsOf(DIR_TILE)).toEqual(["shell-1"]));
    vi.mocked(dashboardLaunchBegin).mockResolvedValueOnce(false);
    vi.mocked(dashboardResolveSession).mockResolvedValueOnce("bg-2");
    vi.mocked(dashboardProbeSessionsAlive).mockResolvedValueOnce(["bg-2"]);

    await open({ target: { kind: "dir", path: DIR, agent: "triage" }, headed: true });

    await vi.waitFor(() => expect(tabsOf(DIR_TILE)).toContain("bg-2"));
    expect(selectActiveTerminalTab(useViewerStore.getState(), TEAM, DIR_TILE)).toBe("bg-2");
    expect(prepareCalls).toEqual([]);
  });

  it("branch, headed: opens a focused tab in the worktree's tile, bound to the agent session", async () => {
    const result = await open({
      target: {
        kind: "branch",
        repo: "acme/api",
        branch: "feat/fresh",
        agent: "reviewer",
        checkout: "new",
      },
      headed: true,
      reviewPrompt: "start",
    });

    const sessionId = result.sessionId!;
    const tile = genericWorkspaceId("/wt/agent-branch");
    expect(prepareCalls).toEqual([
      expect.objectContaining({ agentKey: "reviewer", cwd: "/wt/agent-branch", sessionId }),
    ]);
    expect(bindCalls).toEqual([{ handle: "acme/api::feat/fresh", sessionId }]);
    expect(tiles()).toEqual([tile]);
    expect(tabsOf(tile)).toEqual([sessionId]);
    expect(selectActiveTerminalTab(useViewerStore.getState(), TEAM, tile)).toBe(sessionId);
    expect(useViewerStore.getState().terminalVisible).toBe(true);
  });

  it("branch, headless: runs the agent in the background with no tile", async () => {
    const result = await open({
      target: { kind: "branch", repo: "acme/api", branch: "feat/x", agent: "reviewer" },
      headed: false,
      reviewPrompt: "start",
    });

    expect(tiles()).toEqual([]);
    expect(selectOpenTabLocation(useViewerStore.getState(), result.sessionId!)).toBeDefined();
  });
});

describe("a named agent's tab title", () => {
  const titleOf = (dir: string, sessionId: string) =>
    selectWorkspaceTabs(useViewerStore.getState(), TEAM, genericWorkspaceId(dir)).find(
      (t) => t.sessionId === sessionId,
    )?.name;

  it("titles a renamed agent's next tab with the new name", async () => {
    const triageAt = (path: string) => ({
      target: { kind: "dir" as const, path, agent: "triage", session: "triage" },
      headed: true,
    });
    const first = await open(triageAt(DIR));
    expect(titleOf(DIR, first.sessionId!)).toBe("Slack triage");

    // The team renamed the agent: the backend now prepares it under the new name.
    const base = invoke.router!;
    invoke.router = async (cmd, args) => {
      const reply = await base(cmd, args);
      return cmd === "agent_launch_prepare" ? { ...(reply as object), name: "Ricky" } : reply;
    };
    const other = "/wt/other-team";
    const second = await open(triageAt(other));
    expect(titleOf(other, second.sessionId!)).toBe("Ricky");
  });

  it("titles any named agent's tab with its display name", async () => {
    const result = await open({
      target: { kind: "dir", path: DIR, agent: "triage" },
      headed: true,
    });
    expect(titleOf(DIR, result.sessionId!)).toBe("Slack triage");
  });
});
