// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { readdirSync, readFileSync, statSync } from "fs";
import { join, relative, resolve } from "path";

import {
  createTerminalTab,
  selectHiddenTerminalTabs,
  selectRoleTab,
  selectWorkspaceTabs,
  useViewerStore,
} from "@/shared/stores/viewer-store";
import { connectAgentSession, makeRealLaunchDeps } from "@/shared/lib/agent-session-launcher";
import { hideTab } from "@/features/viewer/lib/terminal-close";
import { makeRealDashboardLaunchDeps } from "@/features/dashboard/components/dashboard-launch-controller";
import {
  seedTerminalTabs,
  terminalHost,
  installTerminalLayoutHarness,
} from "@/test/terminal-layout-harness";

installTerminalLayoutHarness();

const TEAM = "acme/app:core";
const s = () => useViewerStore.getState();
const settle = () => new Promise((r) => setTimeout(r));

/** The sessions the host was asked to open, and whether each is in its layout. */
function openedInHost(): Array<[string, boolean]> {
  return terminalHost().opened.map((r) => [r.session, terminalHost().contains(r.session)]);
}

beforeEach(() => {
  useViewerStore.setState({
    workspaces: {
      [TEAM]: [
        { id: "w1", kind: "generic", cwd: "/w/w1", status: "ready" },
        { id: "test-agent", kind: "generic", cwd: "/ctx", status: "ready" },
      ],
    },
    activeWorkspace: { [TEAM]: "w1" },
    softClosedWorkspaces: {},
  });
});

describe("every terminal the app starts goes through terminal_open_tab into its host's layout", () => {
  it("a new shell (Cmd+T, the tab strip's +, the empty state's New terminal)", async () => {
    const tab = createTerminalTab();
    await s().addTerminalTab(TEAM, tab);
    expect(openedInHost()).toEqual([[tab.sessionId, true]]);
    expect(terminalHost().opened[0].workspace).toBe(`${TEAM}|w1`);
    expect(selectWorkspaceTabs(s(), TEAM, "w1").map((t) => t.sessionId)).toEqual([tab.sessionId]);
  });

  it("an addressable tab opened by a dashboard or MCP verb, carrying its role", async () => {
    const sessionId = s().openTabInWorkspace(TEAM, "w1", { role: "agent:x", name: "Review" });
    await settle();
    expect(openedInHost()).toEqual([[sessionId, true]]);
    expect(terminalHost().opened[0]).toMatchObject({ role: "agent:x", name: "Review" });
    expect(selectRoleTab(s(), TEAM, "agent:x")?.tab.sessionId).toBe(sessionId);
  });

  it("a role agent's owned tab, placed with its role so a relaunch adopts it", async () => {
    const deps = {
      ...makeRealLaunchDeps(vi.fn(async () => {})),
      launchAgentLine: vi.fn(async () => ({ ok: true }) as const),
      projectScopeConfig: vi.fn(async () => ({ extraArgs: [], environmentBinding: null })),
    };
    const res = await connectAgentSession(
      {
        teamKey: TEAM,
        orgRoot: "/ctx",
        agentPath: "/bin/claude",
        role: "test-agent",
        workspaceKey: "test-agent",
      },
      deps,
    );
    expect(openedInHost()).toEqual([[res.sessionId, true]]);
    expect(terminalHost().opened[0]).toMatchObject({
      workspace: `${TEAM}|test-agent`,
      role: "test-agent",
      spawn: { kind: "cold", cwd: "/ctx", envRemove: ["ANTHROPIC_API_KEY"] },
    });
    expect(selectRoleTab(s(), TEAM, "test-agent")?.tab.sessionId).toBe(res.sessionId);
  });

  it("a busy role agent Hidden by its × keeps running — reconnecting re-binds it", async () => {
    seedTerminalTabs(TEAM, "test-agent", ["shell", { session: "agent", role: "test-agent" }]);
    // The × on a busy tab Hides it (the verb decided by `defaultCloseVerb`).
    hideTab(TEAM, "test-agent", "agent");
    await settle();
    expect(selectWorkspaceTabs(s(), TEAM, "test-agent").map((t) => t.id)).toEqual(["shell"]);
    expect(selectHiddenTerminalTabs(s(), TEAM, "test-agent").map((t) => t.id)).toEqual(["agent"]);

    const res = await connectAgentSession(
      {
        teamKey: TEAM,
        orgRoot: "/ctx",
        agentPath: "/bin/claude",
        role: "test-agent",
        workspaceKey: "test-agent",
      },
      makeRealLaunchDeps(vi.fn(async () => {})),
    );
    expect(res.sessionId).toBe("agent");
    expect(terminalHost().opened).toEqual([]);
  });

  it("a relaunch's evicted stale agent is never adopted back, though it runs until the heal ends it", async () => {
    seedTerminalTabs(TEAM, "test-agent", [{ session: "stale", role: "test-agent" }]);
    terminalHost().setBusy("stale", true);
    await makeRealDashboardLaunchDeps("/ctx/dash.md").evictStaleTab("stale");
    expect(terminalHost().contains("stale")).toBe(true);

    const res = await connectAgentSession(
      {
        teamKey: TEAM,
        orgRoot: "/ctx",
        agentPath: "/bin/claude",
        role: "test-agent",
        workspaceKey: "test-agent",
      },
      {
        ...makeRealLaunchDeps(vi.fn(async () => {})),
        launchAgentLine: vi.fn(async () => ({ ok: true }) as const),
        projectScopeConfig: vi.fn(async () => ({ extraArgs: [], environmentBinding: null })),
      },
    );
    expect(res.sessionId).not.toBe("stale");
    expect(terminalHost().opened.map((r) => r.session)).toEqual([res.sessionId]);
  });

  it("an exited role tab is not the agent: reconnecting starts a fresh one beside it", async () => {
    seedTerminalTabs(TEAM, "test-agent", [{ session: "ended", role: "test-agent" }]);
    terminalHost().exit("ended", 0);
    expect(selectRoleTab(s(), TEAM, "test-agent")).toBeUndefined();

    const res = await connectAgentSession(
      {
        teamKey: TEAM,
        orgRoot: "/ctx",
        agentPath: "/bin/claude",
        role: "test-agent",
        workspaceKey: "test-agent",
      },
      {
        ...makeRealLaunchDeps(vi.fn(async () => {})),
        launchAgentLine: vi.fn(async () => ({ ok: true }) as const),
        projectScopeConfig: vi.fn(async () => ({ extraArgs: [], environmentBinding: null })),
      },
    );
    expect(res.sessionId).not.toBe("ended");
    expect(selectRoleTab(s(), TEAM, "test-agent")?.tab.sessionId).toBe(res.sessionId);
  });

  it("the fresh shell that replaces a closed last tab", async () => {
    seedTerminalTabs(TEAM, "w1", ["only"]);
    s().removeTerminalTab(TEAM, "only", "w1");
    await settle();
    const [replacement] = terminalHost().opened;
    expect(openedInHost()).toEqual([[replacement.session, true]]);
    expect(selectWorkspaceTabs(s(), TEAM, "w1").map((t) => t.sessionId)).toEqual([
      replacement.session,
    ]);
  });

  it("no other production code opens or spawns a terminal around the host", () => {
    const src = resolve(__dirname, "../..");
    const files = (dir: string): string[] =>
      readdirSync(dir).flatMap((name) => {
        const path = join(dir, name);
        if (statSync(path).isDirectory()) return name === "test" ? [] : files(path);
        return /\.(ts|tsx)$/.test(name) && !/\.test\./.test(name) ? [path] : [];
      });
    const openers = files(src)
      .filter((path) => {
        const code = readFileSync(path, "utf-8").replace(/\/\*[\s\S]*?\*\/|\/\/.*$/gm, "");
        return /\bterminalOpenTab\(|["'](spawn_pty|attach_warm_pty|terminal_open_tab)["']/.test(
          code,
        );
      })
      .map((path) => relative(src, path))
      .sort();
    expect(openers).toEqual(["shared/stores/viewer-store.ts", "tauri/commands.ts"]);
  });
});
