// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

/**
 * The story "Launch agent" menu launches a named agent through the app's REAL open-workspace
 * dispatch and launch deps, with only the IPC and the terminal host faked; the backend's
 * `agent_launch_prepare` is the RF-29 mock handler's stub agent. The seams checked: the
 * prepare names the agent (never a pack default), the spawn runs the prepared launch, and
 * the bind records the agent revision.
 */

const { invoke, bindCalls, prepareCalls, terminal, agentRows, live } = vi.hoisted(() => ({
  live: { session: null as string | null, rows: [] as unknown[] },
  invoke: { router: null as null | ((cmd: string, args?: Record<string, unknown>) => unknown) },
  bindCalls: [] as Array<{ handle: string; sessionId: string; scope: unknown }>,
  prepareCalls: [] as Array<Record<string, unknown>>,
  agentRows: { current: [] as unknown[] },
  terminal: {
    shells: [] as Array<{ sessionId: string; shell: Record<string, unknown> }>,
    lines: [] as Array<{ sessionId: string; line: string }>,
    seeds: [] as Array<{ sessionId: string; text: string }>,
  },
}));

vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  dashboardLaunchBegin: vi.fn(async () => true),
  dashboardLaunchAbort: vi.fn(async () => undefined),
  dashboardResolveSession: vi.fn(async () => live.session),
  dashboardListSessions: vi.fn(async () => live.rows),
  dashboardProbeSessionsAlive: vi.fn(async () => []),
  terminalCloseTab: vi.fn(async () => undefined),
  dashboardLaunchBind: vi.fn(
    async (handle: string, sessionId: string, _launcher?: string, scope?: unknown) => {
      bindCalls.push({ handle, sessionId, scope });
    },
  ),
  agentLaunchPrepare: vi.fn(async (p: Record<string, unknown>) => {
    prepareCalls.push(p);
    return invoke.router!("agent_launch_prepare", p);
  }),
}));

vi.mock("@/shared/lib/agent-session-launcher", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/shared/lib/agent-session-launcher")>();
  return {
    ...actual,
    waitForAgentLive: vi.fn(async () => true),
    makeRealLaunchDeps: () => ({
      findExisting: async () => undefined,
      provision: async () => undefined,
      openTab: async (
        _teamKey: string,
        _workspaceId: string,
        tab: { sessionId: string },
        shell: Record<string, unknown>,
      ) => {
        terminal.shells.push({ sessionId: tab.sessionId, shell });
      },
      closeTab: () => undefined,
      launchAgentLine: async (sessionId: string, line: string) => {
        terminal.lines.push({ sessionId, line });
        return { ok: true };
      },
      projectScopeConfig: async () => ({ extraArgs: [], environmentBinding: null }),
    }),
  };
});

vi.mock("@/shared/lib/default-agent", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/shared/lib/default-agent")>()),
  resolveAgentCliPath: async (basename = "claude") => `/opt/bin/${basename}`,
}));

vi.mock("@/shared/lib/agent-session-injection", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/shared/lib/agent-session-injection")>()),
  injectAgentLine: vi.fn(async (sessionId: string, text: string) => {
    terminal.seeds.push({ sessionId, text });
    return { ok: true };
  }),
}));

vi.mock("@/shared/lib/team-key", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/shared/lib/team-key")>()),
  getActiveTeamKey: () => "acme:backend",
}));

vi.mock("@/features/dashboard/components/dashboard-launch-status", () => ({
  armLaunchStatus: () => undefined,
}));

// The dialog opens through the app's controller; here that is the real dispatch below.
const controller = vi.hoisted(() => ({
  open: null as null | ((arg: unknown) => Promise<unknown>),
}));
vi.mock("@/features/dashboard/components/dashboard-launch", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/features/dashboard/components/dashboard-launch")>()),
  getDashboardLaunchController: () => ({ openWorkspace: (arg: unknown) => controller.open!(arg) }),
}));

vi.mock("@/features/agents/lib/agent-names", () => ({
  useAgentRows: () => ({ rows: agentRows.current, error: null }),
}));

import { createInvokeRouter } from "@/app/mock/mock-router";
import { agentLaunchHandlers } from "@/app/mock/handlers/agents-launch";
import {
  dispatchOpenWorkspace,
  makeRealBranchLaunchDeps,
  makeRealDashboardLaunchDeps,
  makeRealHeadlessLaunchers,
} from "@/features/dashboard/components/dashboard-launch-controller";
import type { DashboardLaunchDeps } from "@/features/dashboard/components/dashboard-launch";
import { launchAgentInStoryWorktree, storyLaunchPrompt } from "./story-agent-launch";

const TEAM_KEY = "acme:backend";
const STORY = "/ctx/stories/SW-1.md";

function storyDeps(launcher: string): DashboardLaunchDeps {
  return {
    ...makeRealDashboardLaunchDeps(launcher),
    resolveStory: async () => ({ storyFilePath: STORY, teamKey: TEAM_KEY }),
    ensureWorktree: async () => "/wt/SW-1",
    awaitEstablished: undefined,
    awaitContext: undefined,
    focus: vi.fn(),
  };
}

beforeEach(() => {
  invoke.router = createInvokeRouter(agentLaunchHandlers());
  bindCalls.length = 0;
  prepareCalls.length = 0;
  terminal.shells.length = 0;
  terminal.lines.length = 0;
  terminal.seeds.length = 0;
  live.session = null;
  live.rows = [];
  controller.open = (arg) =>
    dispatchOpenWorkspace(
      arg as Parameters<typeof dispatchOpenWorkspace>[0],
      "virtual://story-launch",
      storyDeps("virtual://story-launch"),
      makeRealBranchLaunchDeps("virtual://story-launch"),
      { openGenericWorkspace: vi.fn() },
      makeRealHeadlessLaunchers("virtual://story-launch"),
    );
});

describe("the story Launch agent menu launches the chosen agent", () => {
  it("prepares that agent for the spawned session and binds its revision", async () => {
    const launched = await launchAgentInStoryWorktree(
      TEAM_KEY,
      { storyFilePath: STORY, storyId: "SW-1" },
      {
        agentKey: "acme/backend/triage",
        name: "triage",
        runtime: "claude",
        binary: "claude",
        place: null,
        isDefault: true,
        availability: "available",
      },
      { deps: storyDeps("virtual://story-launch") },
    );

    const [shell] = terminal.shells;
    expect(prepareCalls).toEqual([
      expect.objectContaining({
        agentKey: "acme/backend/triage",
        team: TEAM_KEY,
        cwd: "/wt/SW-1",
        sessionId: shell.sessionId,
        prompt: storyLaunchPrompt(STORY),
      }),
    ]);
    expect(terminal.lines[0].line).toContain("'/opt/bin/claude' '--model' 'opus' '--settings'");
    expect(bindCalls).toEqual([
      {
        handle: "SW-1",
        sessionId: shell.sessionId,
        scope: { teamKey: TEAM_KEY, agentRevision: "acme/backend/triage@3" },
      },
    ]);
    expect(terminal.seeds.map((s) => s.text)).toEqual([storyLaunchPrompt(STORY)]);
    expect(launched.sessionId).toBe(shell.sessionId);
  });
});
