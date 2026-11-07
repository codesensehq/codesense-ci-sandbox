// SPDX-License-Identifier: MIT
import { dropWorkspace } from "./tray-reorder-drag";
import { tileZoneId } from "./terminal-drag";
import { useTileTabActions } from "../components/workspace-terminal-panels";
import { act } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

// Seam: the real viewer store over the test terminal host, the real org store and the real
// Show. Only the focus helper, the run-output panel, toasts and the Tauri boundary are faked.

const h = vi.hoisted(() => ({ focus: vi.fn() }));
vi.mock("@/features/viewer/components/agent-result-panel", () => ({ openAgentResult: vi.fn() }));
vi.mock("@/features/viewer/lib/focus-cascade", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/features/viewer/lib/focus-cascade")>()),
  focusActiveWorkspaceTerminal: h.focus,
}));
vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn(), info: vi.fn() }),
}));
vi.mock("@/shared/lib/private-terminal-authority", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/shared/lib/private-terminal-authority")>()),
  acceptsPrivateAuthority: () => true,
  currentPrivateAuthorityEpoch: () => "7",
}));
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  dashboardForegroundSession: vi.fn(async () => {}),
  saveAppState: vi.fn(async () => {}),
  pushViewerState: vi.fn(async () => {}),
  saveOrgState: vi.fn(async () => {}),
}));

import {
  CONTEXT_HISTORY_CAP,
  contextBack,
  contextForward,
  switchContext,
  switchContextToOrg,
  switchContextToTeamName,
  currentContextNavigationRevision,
  currentContextEntry,
} from "./context-switch";
import { currentActivationGeneration, shouldFocusTerminalOnReveal } from "./focus-cascade";
import { dispatchMenuAction } from "@/app/menu-action-dispatch";
import { openFileInTeam } from "@/shared/lib/open-file-in-team";
import { revealOpenedWorkspace } from "@/shared/hooks/use-open-workspace-reveal";
import { useEnterTeam } from "@/features/home-base/lib/enter-team";
import { renderHook } from "@testing-library/react";
import { showAgent, runShowPlan, type ShowPlan } from "./show-agent";
import { useAgentDrill } from "../hooks/use-agent-drill";
import { openAgentResult } from "../components/agent-result-panel";
import { registerMachineLookup } from "@/features/machines/lib/machine-offline";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { getActiveTeamKey } from "@/shared/lib/team-key";
import { teamFoldKey } from "@/shared/lib/workspace-key";
import { useOrgStore } from "@/shared/stores/org-store";
import {
  EMPTY_CONTEXT_HISTORY,
  cliChromeOf,
  selectActiveTerminalTab,
  selectExpandedWorkspaceId,
  useViewerStore,
  type Workspace,
} from "@/shared/stores/viewer-store";
import {
  connectTestRemoteHost,
  installTerminalLayoutHarness,
  refetchTestLayout,
  seedTerminalTabs,
} from "@/test/terminal-layout-harness";
import { splitTerminal, showAllTabs, showOneTab, zoomTerminalCell } from "./terminal-tab-keys";
import { installUserNavigationCapture } from "@/shared/lib/context-navigation";
import { terminalLayoutSourceMock } from "@/test/terminal-layout-source-mock";
import type { AgentSessionRow, MachineSummary } from "@/tauri/commands";
import type { Org } from "@/shared/types/domain";

installTerminalLayoutHarness();

const A = "acme:core";
const B = "acme:ops";
const C = "acme:docs";
const ws = (id: string): Workspace => ({ id, kind: "generic", cwd: `/${id}`, status: "ready" });
const s = () => useViewerStore.getState();
const settle = () => act(async () => {});

function org(slug: string, teams: string[]): Org {
  return {
    slug,
    display_name: slug,
    created_at: "2026-01-01T00:00:00Z",
    remote_url: null,
    config_flags: {},
    people: [],
    teams: teams.map((name) => ({ name, source_repos: [] })),
    org_root: `/orgs/${slug}`,
  } as unknown as Org;
}

function row(over: Partial<AgentSessionRow> & { sessionId: string }): AgentSessionRow {
  return {
    handle: over.sessionId,
    kind: "dashboard-launched",
    alive: true,
    backgrounded: false,
    startedWith: null,
    startedAtMs: null,
    launcher: null,
    lastOutputMs: null,
    displayName: "Reviewer",
    ...over,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  registerMachineLookup(() => null);
  useAgentListProjectionStore.setState({ sessions: null, error: null });
  useOrgStore.setState({
    orgs: [org("acme", ["core", "ops", "docs"]), org("beta", ["main"])],
    activeOrgSlug: "acme",
    activeTeamName: "core",
    lastTeamByOrgSlug: { beta: "main" },
    shownTeamsByOrgSlug: {},
  });
  useViewerStore.setState({
    openedTeams: [A, B, C, "beta:main"],
    workspaces: {
      [A]: [ws("a1"), ws("a2")],
      [B]: [ws("o1"), ws("o2")],
      [C]: [ws("d1")],
      "beta:main": [ws("m1")],
    },
    softClosedWorkspaces: {},
    activeWorkspace: { [A]: "a2", [B]: "o1", [C]: "d1", "beta:main": "m1" },
    contextHistory: EMPTY_CONTEXT_HISTORY,
    agentsWorkspaceReveal: null,
    agentsPaneHiddenTeams: [],
    agentsPaneFolds: {},
    cliExpanded: false,
    foregroundLayout: {},
    trayMode: {},
  });
});

describe("a row click on another team's agent (G1)", () => {
  it("switches the whole context to that team, focused on the agent's workspace and tab", async () => {
    seedTerminalTabs(B, "o2", ["s-o", "s-p"], { active: "s-o" });
    await settle();
    const outcome = await act(() => showAgent(row({ sessionId: "s-p" }), { teamKey: A }));
    expect(outcome).toEqual({ shown: "tab" });
    expect(getActiveTeamKey()).toBe(B);
    expect(s().activeWorkspace[B]).toBe("o2");
    expect(selectActiveTerminalTab(s(), B, "o2")).toBe("s-p");
    // Its terminal takes the typing (D43), and the team's other workspace is still a tile.
    expect(h.focus).toHaveBeenCalledWith(B);
    expect(s().workspaces[B].map((w) => w.id)).toEqual(["o1", "o2"]);
  });

  it("the first frame of the new team already has the agent's workspace and tab", async () => {
    seedTerminalTabs(B, "o2", ["s-o", "s-p"], { active: "s-o" });
    await settle();
    const seen: { focused?: string; tab?: string }[] = [];
    const stop = useOrgStore.subscribe((next, prev) => {
      if (next.activeTeamName !== prev.activeTeamName) {
        seen.push({
          focused: s().activeWorkspace[B],
          tab: selectActiveTerminalTab(s(), B, "o2"),
        });
      }
    });
    await act(() => showAgent(row({ sessionId: "s-p" }), { teamKey: A }));
    stop();
    expect(seen).toEqual([{ focused: "o2", tab: "s-p" }]);
  });

  it("an agent in another workspace of the team on screen records the workspace left", async () => {
    seedTerminalTabs(A, "a1", ["s-a"], { active: "s-a" });
    await settle();
    await act(() => showAgent(row({ sessionId: "s-a" }), { teamKey: A }));
    expect(getActiveTeamKey()).toBe(A);
    expect(s().contextHistory.back).toEqual([{ teamKey: A, workspaceId: "a2" }]);
    expect(s().activeWorkspace[A]).toBe("a1");
  });

  it("an agent on an offline machine still switches the team; its tile shows as it last was (G6)", async () => {
    const box = {
      machineId: "box",
      label: "Studio box",
      status: { state: "unreachable", lastReachedAtMs: 0 },
    } as unknown as MachineSummary;
    registerMachineLookup((id) => (id === "box" ? box : null));
    const remote = connectTestRemoteHost("box", "7");
    remote.seed(
      `${B}|o2`,
      { displayName: "o2", kind: "generic", org: "acme", team: "ops", worktreePath: "/o2" },
      { session: "remote-shell", name: "usage-claude", tags: { remoteHandle: "h-1" } },
    );
    refetchTestLayout();
    await settle();
    const outcome = await act(() =>
      showAgent(row({ sessionId: "remote-shell", handle: "usage-claude" }), { teamKey: A }),
    );
    expect(outcome).toMatchObject({ shown: "tab", notice: expect.stringContaining("offline") });
    expect(getActiveTeamKey()).toBe(B);
    expect(s().activeWorkspace[B]).toBe("o2");
  });
});

describe("the team switcher and the other routes (G4)", () => {
  it("land on the team's remembered workspace", () => {
    act(() => {
      switchContextToTeamName("ops", "switcher");
    });
    expect(getActiveTeamKey()).toBe(B);
    expect(s().activeWorkspace[B]).toBe("o1");
    // The team left keeps its own focus for the way back.
    expect(s().activeWorkspace[A]).toBe("a2");
  });

  it("an org switch lands on the team that org remembers", () => {
    act(() => {
      switchContextToOrg("beta", "switcher");
    });
    expect(getActiveTeamKey()).toBe("beta:main");
    expect(s().contextHistory.back).toEqual([{ teamKey: A, workspaceId: "a2" }]);
  });

  it("a named workspace and tab are made the team's before it shows", async () => {
    seedTerminalTabs(B, "o2", ["t-1", "t-2"], { active: "t-1" });
    await settle();
    act(() => {
      switchContext({ teamKey: B, workspaceId: "o2", tabId: "t-2" }, "nav");
    });
    expect(s().activeWorkspace[B]).toBe("o2");
    expect(selectActiveTerminalTab(s(), B, "o2")).toBe("t-2");
  });
});

describe("pane state on a context switch", () => {
  it("preserves dock visibility, folds and filters without requesting a workspace reveal", () => {
    useViewerStore.setState({
      workspacePanelCollapsed: true,
      agentsPaneHiddenTeams: [B],
      agentsPaneFolds: { [teamFoldKey(B)]: true },
    });
    act(() => {
      switchContextToTeamName("ops", "switcher");
    });
    expect(s().workspacePanelCollapsed).toBe(true);
    expect(s().agentsPaneHiddenTeams).toEqual([B]);
    expect(s().agentsPaneFolds[teamFoldKey(B)]).toBe(true);
    expect(s().agentsWorkspaceReveal).toBeNull();
  });
});

describe("back and forward over teams and workspaces (⌘[ / ⌘])", () => {
  it("⌘[ returns to the team and workspace left, and ⌘] goes forward again", () => {
    act(() => {
      switchContextToTeamName("ops", "switcher");
    });
    act(() => {
      expect(contextBack()).toBe(true);
    });
    expect(getActiveTeamKey()).toBe(A);
    expect(s().activeWorkspace[A]).toBe("a2");
    act(() => {
      expect(contextForward()).toBe(true);
    });
    expect(getActiveTeamKey()).toBe(B);
    act(() => {
      expect(contextForward()).toBe(false);
    });
    expect(getActiveTeamKey()).toBe(B);
  });

  it("back lands on the workspace focused when the team was left", () => {
    act(() => {
      switchContextToTeamName("ops", "switcher");
    });
    // A background open moved team A's memory meanwhile.
    act(() => s().switchWorkspace(A, "a1"));
    act(() => {
      contextBack();
    });
    expect(getActiveTeamKey()).toBe(A);
    expect(s().activeWorkspace[A]).toBe("a2");
  });

  it("a gone workspace lands on the team's remembered focus", () => {
    act(() => {
      switchContextToTeamName("ops", "switcher");
    });
    useViewerStore.setState({
      workspaces: { ...s().workspaces, [A]: [ws("a1")] },
      activeWorkspace: { ...s().activeWorkspace, [A]: "a1" },
    });
    act(() => {
      contextBack();
    });
    expect(getActiveTeamKey()).toBe(A);
    expect(s().activeWorkspace[A]).toBe("a1");
  });

  it("skips a team closed since, and a new switch drops the way forward", () => {
    act(() => {
      switchContextToTeamName("ops", "switcher");
    });
    act(() => {
      switchContextToTeamName("docs", "switcher");
    });
    act(() => s().closeTeam(B));
    act(() => {
      contextBack();
    });
    expect(getActiveTeamKey()).toBe(A);
    expect(s().contextHistory.forward).toEqual([{ teamKey: C, workspaceId: "d1" }]);
    act(() => {
      switchContextToTeamName("docs", "switcher");
    });
    expect(s().contextHistory.forward).toEqual([]);
  });

  it("the terminal takes focus on the way back only while the CLI is shown", () => {
    useViewerStore.setState({ terminalVisible: false, layoutMode: "viewer" });
    act(() => {
      switchContextToTeamName("ops", "switcher");
    });
    act(() => {
      contextBack();
    });
    expect(h.focus).not.toHaveBeenCalled();
    useViewerStore.setState({ terminalVisible: true, layoutMode: "shared" });
    act(() => {
      contextForward();
    });
    expect(h.focus).toHaveBeenCalledWith(B);
  });

  it("keeps at most the cap of places", () => {
    for (let i = 0; i < CONTEXT_HISTORY_CAP + 5; i++) {
      act(() => {
        switchContextToTeamName(i % 2 === 0 ? "ops" : "core", "switcher");
      });
    }
    expect(s().contextHistory.back).toHaveLength(CONTEXT_HISTORY_CAP);
  });

  it("a renamed team's places follow it", () => {
    act(() => {
      switchContextToTeamName("ops", "switcher");
    });
    act(() => s().rekeyTeam("acme", "core", "core-2"));
    expect(s().contextHistory.back).toEqual([{ teamKey: "acme:core-2", workspaceId: "a2" }]);
  });
});

describe("the CLI chrome is one setting across a switch (G2)", () => {
  it("hidden, split size and full screen in one team hold in the next", () => {
    useViewerStore.setState({ layoutMode: "shared", terminalVisible: true });
    act(() => s().setLayoutMode("viewer"));
    act(() => s().setTerminalSplitRatio(0.3));
    act(() => {
      switchContextToTeamName("ops", "switcher");
    });
    expect(s()).toMatchObject({
      layoutMode: "viewer",
      terminalVisible: false,
      terminalSplitRatio: 0.3,
    });
    act(() => s().setLayoutMode("console"));
    act(() => {
      contextBack();
    });
    expect(getActiveTeamKey()).toBe(A);
    expect(s()).toMatchObject({ layoutMode: "console", terminalVisible: true });
  });

  it("expanded before a row click lands expanded on the clicked agent's workspace", async () => {
    seedTerminalTabs(B, "o2", ["s-o", "s-p"], { active: "s-o" });
    await settle();
    act(() => s().setCliExpanded(true));
    await act(() => showAgent(row({ sessionId: "s-p" }), { teamKey: A }));
    expect(getActiveTeamKey()).toBe(B);
    expect(selectExpandedWorkspaceId(s(), B)).toBe("o2");
  });

  it("the grid before a switch lands on the team's own preset, its focused workspace focused", () => {
    act(() => s().setForegroundPreset(B, "rows"));
    act(() => s().setForegroundPreset(A, "grid"));
    act(() => {
      switchContextToTeamName("ops", "switcher");
    });
    expect(selectExpandedWorkspaceId(s(), B)).toBeNull();
    expect(s().foregroundLayout[B]).toMatchObject({ preset: "rows" });
    expect(s().activeWorkspace[B]).toBe("o1");
  });
});

describe("navigation regressions", () => {
  it("walks same-team workspace entries in both directions", () => {
    switchContext({ teamKey: A, workspaceId: "a1" }, "nav");
    expect(contextBack()).toBe(true);
    expect(s().activeWorkspace[A]).toBe("a2");
    expect(contextForward()).toBe(true);
    expect(s().activeWorkspace[A]).toBe("a1");
  });

  it("skips hidden and disabled destinations without reporting a switch", () => {
    useOrgStore.setState({ shownTeamsByOrgSlug: { acme: ["core"] }, disabledOrgSlugs: ["beta"] });
    useViewerStore.setState({
      contextHistory: {
        back: [
          { teamKey: B, workspaceId: "o2" },
          { teamKey: "beta:main", workspaceId: "m1" },
        ],
        forward: [],
      },
    });
    expect(contextBack()).toBe(false);
    expect(getActiveTeamKey()).toBe(A);
    expect(s().activeWorkspace[B]).toBe("o1");
    expect(s().contextHistory.forward).toEqual([]);
    expect(s().contextHistory.back).toHaveLength(2);
    useOrgStore.setState({ disabledOrgSlugs: [] });
  });

  it("an empty destination leaves the pane alone", () => {
    useViewerStore.setState({
      activeWorkspace: { ...s().activeWorkspace, [B]: "" },
      workspaces: { ...s().workspaces, [B]: [] },
    });
    switchContext({ teamKey: B }, "switcher");
    expect(s().agentsWorkspaceReveal).toBeNull();
  });

  it("page scripts open in another team's memory; clicked links switch and record history", () => {
    openFileInTeam(
      { org: "acme", team: "ops", path: "/script.md" },
      { keepLayout: true, newTab: true },
    );
    expect(getActiveTeamKey()).toBe(A);
    expect(s().contextHistory.back).toEqual([]);
    expect(s().activeTab[B]).toBe("/script.md");
    openFileInTeam({ org: "acme", team: "ops", path: "/link.md" });
    expect(getActiveTeamKey()).toBe(B);
    expect(s().contextHistory.back).toEqual([{ teamKey: A, workspaceId: "a2" }]);
  });

  it("team-entry files use the target org and stop when selection is refused", () => {
    const { result } = renderHook(() => useEnterTeam());
    act(() => result.current("main", { orgSlug: "beta", filePath: "/b.md" }));
    expect(getActiveTeamKey()).toBe("beta:main");
    expect(s().activeTab["beta:main"]).toBe("/b.md");
    useOrgStore.setState({ disabledOrgSlugs: ["acme"] });
    act(() => result.current("ops", { orgSlug: "acme", filePath: "/refused.md" }));
    expect(getActiveTeamKey()).toBe("beta:main");
    expect(s().activeTab[B]).not.toBe("/refused.md");
    useOrgStore.setState({ disabledOrgSlugs: [] });
  });

  it("offline agents with no tile still enter their team before the notice", async () => {
    registerMachineLookup((id) =>
      id === "box"
        ? ({
            machineId: "box",
            label: "Studio box",
            status: { state: "unreachable", lastReachedAtMs: 0 },
          } as unknown as MachineSummary)
        : null,
    );
    const outcome = await showAgent(
      row({ sessionId: "untiled", machineId: "box", launchTeamKey: B }),
      {
        teamKey: A,
      },
    );
    expect(outcome).toMatchObject({ notShown: expect.stringContaining("offline") });
    expect(getActiveTeamKey()).toBe(B);
  });
});

it("native menu workspace navigation records the workspace left", () => {
  useViewerStore.setState({ trayOrderByTeam: { [A]: ["a1", "a2"] } });
  dispatchMenuAction("viewer.jump-workspace-1");
  expect(s().activeWorkspace[A]).toBe("a1");
  expect(s().contextHistory.back).toEqual([{ teamKey: A, workspaceId: "a2" }]);
});

it("restoring another team's hidden agent prepares that tab for the first frame", async () => {
  seedTerminalTabs(B, "o2", ["hidden-agent", "other"], { active: "other" });
  await settle();
  s().hideTerminalTab(B, "hidden-agent", "o2");
  await settle();
  const seen: Array<{ focused: string; tab: string | null | undefined }> = [];
  const stop = useOrgStore.subscribe((next, prev) => {
    if (next.activeTeamName !== prev.activeTeamName)
      seen.push({ focused: s().activeWorkspace[B], tab: selectActiveTerminalTab(s(), B, "o2") });
  });
  try {
    await showAgent(row({ sessionId: "hidden-agent" }), { teamKey: A });
    expect(seen).toEqual([{ focused: "o2", tab: "hidden-agent" }]);
    expect(s().contextHistory.back).toEqual([{ teamKey: A, workspaceId: "a2" }]);
  } finally {
    stop();
  }
});

describe("round-two switching regressions", () => {
  it("same-team Hidden Show records the workspace before restoration", async () => {
    seedTerminalTabs(A, "a1", ["restore-me", "other"], { active: "other" });
    await settle();
    s().hideTerminalTab(A, "restore-me", "a1");
    await settle();
    expect(s().activeWorkspace[A]).toBe("a2");
    await showAgent(row({ sessionId: "restore-me" }), { teamKey: A });
    expect(s().activeWorkspace[A]).toBe("a1");
    expect(s().contextHistory.back).toEqual([{ teamKey: A, workspaceId: "a2" }]);
  });

  it("same-team adoption records focus before an awaited workspace open", async () => {
    const ensure = vi.spyOn(s(), "ensureStoryWorkspaceOpen").mockImplementation(async () => {
      s().switchWorkspace(A, "a1");
      return "a1";
    });
    const adopt = vi.spyOn(s(), "adoptTerminalSession").mockResolvedValue(undefined);
    try {
      await runShowPlan(
        {
          kind: "adopt",
          teamKey: A,
          sessionId: "untiled-local",
          storyId: "S-1",
          tabDir: null,
          dir: null,
          confirm: false,
          ended: { runId: null, storyId: null },
        },
        { handle: "untiled-local" },
        { teamKey: A, name: "Agent" },
      );
      expect(s().activeWorkspace[A]).toBe("a1");
      expect(s().contextHistory.back).toEqual([{ teamKey: A, workspaceId: "a2" }]);
    } finally {
      ensure.mockRestore();
      adopt.mockRestore();
    }
  });

  it.each<ShowPlan>([
    { kind: "tab", teamKey: B, workspaceId: "o2", tabId: "x", minimized: false },
    { kind: "restore", teamKey: B, workspaceId: "o2", tabId: "x" },
    {
      kind: "adopt",
      teamKey: B,
      sessionId: "x",
      storyId: null,
      tabDir: null,
      dir: "/o2",
      confirm: false,
      ended: { runId: null, storyId: null },
    },
  ])(
    "refused $kind Show does not mutate a background target or focus/report it shown",
    async (plan) => {
      useOrgStore.setState({ shownTeamsByOrgSlug: { acme: ["core"] } });
      const restore = vi.spyOn(s(), "restoreHiddenTerminalTab");
      const adopt = vi.spyOn(s(), "adoptTerminalSession");
      try {
        expect(
          await runShowPlan(plan, { handle: "x" }, { teamKey: A, name: "Agent" }),
        ).toMatchObject({ notShown: expect.stringContaining("unavailable") });
        expect(s().activeWorkspace[B]).toBe("o1");
        expect(getActiveTeamKey()).toBe(A);
        expect(restore).not.toHaveBeenCalled();
        expect(adopt).not.toHaveBeenCalled();
        expect(h.focus).not.toHaveBeenCalled();
      } finally {
        restore.mockRestore();
        adopt.mockRestore();
      }
    },
  );

  it("stayInTeam background Show only updates its team's memory", async () => {
    seedTerminalTabs(B, "o2", ["background-agent"]);
    await settle();
    useViewerStore.setState({ layoutMode: "viewer", terminalVisible: false });
    await showAgent(row({ sessionId: "background-agent" }), { teamKey: A, stayInTeam: true });
    expect(s().activeWorkspace[B]).toBe("o2");
    expect(getActiveTeamKey()).toBe(A);
    expect(s().terminalVisible).toBe(false);
    expect(s().contextHistory.back).toEqual([]);
    expect(h.focus).not.toHaveBeenCalled();
  });

  it("history restores a minimized workspace without changing chrome even transiently", async () => {
    seedTerminalTabs(A, "a1", ["running"]);
    await settle();
    s().requestCloseWorkspace(A, "a1");
    await settle();
    expect(s().softClosedWorkspaces[A]?.a1).toBeDefined();
    useViewerStore.setState({
      activeWorkspace: { ...s().activeWorkspace, [A]: "a2" },
      layoutMode: "viewer",
      terminalVisible: false,
      contextHistory: { back: [{ teamKey: A, workspaceId: "a1" }], forward: [] },
    });
    const chrome = cliChromeOf(s());
    const observed: unknown[] = [];
    const stop = useViewerStore.subscribe((next) => observed.push(cliChromeOf(next)));
    try {
      expect(contextBack()).toBe(true);
      expect(s().activeWorkspace[A]).toBe("a1");
      expect(observed.length).toBeGreaterThan(0);
      expect(observed.every((c) => JSON.stringify(c) === JSON.stringify(chrome))).toBe(true);
    } finally {
      stop();
    }
  });

  it("Cmd+0's menu route records the previous workspace and clears forward history", () => {
    useViewerStore.setState({
      workspaces: {
        ...s().workspaces,
        [A]: [
          { id: "root:acme/repo", kind: "root", cwd: "/repo", status: "ready" },
          ...s().workspaces[A],
        ],
      },
      contextHistory: { back: [], forward: [{ teamKey: B, workspaceId: "o1" }] },
    });
    dispatchMenuAction("viewer.jump-root-workspace");
    expect(s().activeWorkspace[A]).toBe("root:acme/repo");
    expect(s().contextHistory).toEqual({ back: [{ teamKey: A, workspaceId: "a2" }], forward: [] });
  });

  it("plain-shell drill selection switches with workspace history", async () => {
    seedTerminalTabs(B, "o2", ["shell-target"]);
    await settle();
    const { result } = renderHook(() => useAgentDrill(B));
    act(() => result.current.onRevealTab("o2", "shell-target"));
    expect(getActiveTeamKey()).toBe(B);
    expect(s().activeWorkspace[B]).toBe("o2");
    expect(s().contextHistory.back).toEqual([{ teamKey: A, workspaceId: "a2" }]);
  });

  it.each(["headless", "ended"])(
    "cross-team %s output enters its owning team before opening",
    async (kind) => {
      const output = vi.mocked(openAgentResult);
      output.mockImplementationOnce(() => {
        expect(getActiveTeamKey()).toBe(B);
      });
      await showAgent(
        row({
          sessionId: "run-session",
          kind: kind === "headless" ? "headless" : "dashboard-launched",
          alive: kind !== "ended",
          launchTeamKey: B,
          provenance: { runId: "run-1", storyId: "" },
        }),
        { teamKey: A },
      );
      expect(getActiveTeamKey()).toBe(B);
      expect(output).toHaveBeenCalled();
      expect(s().contextHistory.back).toEqual([{ teamKey: A, workspaceId: "a2" }]);
    },
  );
});

it("a newer team selection supersedes a Show awaiting adoption", async () => {
  let finish!: () => void;
  const waiting = new Promise<void>((resolve) => {
    finish = resolve;
  });
  const adopt = vi.spyOn(s(), "adoptTerminalSession").mockImplementation(() => waiting);
  try {
    const pending = runShowPlan(
      {
        kind: "adopt",
        teamKey: A,
        sessionId: "slow-agent",
        storyId: null,
        tabDir: null,
        dir: null,
        confirm: false,
        ended: { runId: null, storyId: null },
      },
      { handle: "slow-agent" },
      { teamKey: A, name: "Agent" },
    );
    expect(adopt).toHaveBeenCalledOnce();
    switchContext({ teamKey: B, workspaceId: "o2" }, "switcher");
    useViewerStore.setState({ layoutMode: "viewer", terminalVisible: false });
    const history = s().contextHistory;
    finish();
    expect(await pending).toMatchObject({ notShown: expect.stringContaining("superseded") });
    expect(getActiveTeamKey()).toBe(B);
    expect(s().activeWorkspace[B]).toBe("o2");
    expect(s().contextHistory).toEqual(history);
    expect(s().layoutMode).toBe("viewer");
    expect(h.focus).not.toHaveBeenCalled();
  } finally {
    finish();
    adopt.mockRestore();
  }
});

it("a stale story-open callback cannot select or reveal over newer same-team navigation", async () => {
  let finish!: () => void;
  const waiting = new Promise<void>((resolve) => {
    finish = resolve;
  });
  const ensure = vi
    .spyOn(s(), "ensureWorktree")
    .mockImplementation(async (_team, _file, _start, _slug, opts) => {
      await waiting;
      opts?.onCreating?.();
      return "/new-story";
    });
  const revision = currentContextNavigationRevision();
  try {
    const pending = s().openOrFocusStoryWorkspace(
      A,
      "/stories/new-story.md",
      undefined,
      undefined,
      true,
      false,
      undefined,
      () => currentContextNavigationRevision() === revision,
    );
    switchContext({ teamKey: A, workspaceId: "a1" }, "nav");
    useViewerStore.setState({ layoutMode: "viewer", terminalVisible: false });
    finish();
    await pending;
    expect(s().activeWorkspace[A]).toBe("a1");
    expect(s().layoutMode).toBe("viewer");
    expect(s().terminalVisible).toBe(false);
  } finally {
    finish();
    ensure.mockRestore();
  }
});

it.each(["session", "runOutput"] as const)(
  "MCP %s reveal keeps the real user's team, history and focus",
  async (target) => {
    seedTerminalTabs(B, "o2", ["mcp-session"]);
    await settle();
    useViewerStore.setState({ layoutMode: "viewer", terminalVisible: false });
    const history = s().contextHistory;
    const chrome = cliChromeOf(s());
    await revealOpenedWorkspace({
      target,
      sessionId: "mcp-session",
      runId: "mcp-run",
      handle: "mcp-agent",
      teamKey: B,
      focus: true,
    });
    expect(getActiveTeamKey()).toBe(A);
    expect(s().contextHistory).toEqual(history);
    expect(cliChromeOf(s())).toEqual(chrome);
    expect(h.focus).not.toHaveBeenCalled();
    expect(openAgentResult).not.toHaveBeenCalled();
    if (target === "session") expect(s().activeWorkspace[B]).toBe("o2");
  },
);

it("a clicked cross-team preview reveals the viewer while script previews preserve console", () => {
  useViewerStore.setState({
    layoutMode: "console",
    terminalVisible: true,
    lastTerminalMode: "console",
  });
  openFileInTeam({ org: "acme", team: "ops", path: "/script-preview.md" }, { keepLayout: true });
  expect(getActiveTeamKey()).toBe(A);
  expect(s().layoutMode).toBe("console");
  openFileInTeam({ org: "acme", team: "ops", path: "/clicked-preview.md" });
  expect(getActiveTeamKey()).toBe(B);
  expect(s().activeTab[B]).toBe("/clicked-preview.md");
  expect(s().openTabs[B].find((tab) => tab.path === "/clicked-preview.md")?.preview).toBe(true);
  expect(s().layoutMode).toBe("shared");
  expect(s().terminalVisible).toBe(true);
  expect(s().lastTerminalMode).toBe("console");
});

it.each([
  [
    "terminal cycle",
    () => {
      seedTerminalTabs(A, "a2", ["key-one", "key-two"], { active: "key-one" });
      s().cycleTerminalTab(A, "next");
    },
  ],
  [
    "generic terminal cycle",
    () => {
      seedTerminalTabs(A, "a2", ["key-one", "key-two"], { active: "key-one" });
      s().cycleTab(A, "terminal", 1);
    },
  ],
  [
    "menu terminal cycle",
    () => {
      seedTerminalTabs(A, "a2", ["key-one", "key-two"], { active: "key-one" });
      dispatchMenuAction("viewer.cycle-terminal-tab-next");
    },
  ],
  [
    "terminal jump",
    () => {
      seedTerminalTabs(A, "a2", ["key-one", "key-two"], { active: "key-one" });
      dispatchMenuAction("viewer.jump-terminal-tab-2");
    },
  ],
  [
    "same-current terminal jump",
    () => {
      seedTerminalTabs(A, "a2", ["key-one", "key-two"], { active: "key-one" });
      dispatchMenuAction("viewer.jump-terminal-tab-1");
    },
  ],
  ["plain roving ArrowDown", () => userGesture("keydown", "ArrowDown")],
  ["plain roving Home", () => userGesture("keydown", "Home")],
  ["plain typeahead", () => userGesture("keydown", "r")],
  ["agent overlay keyboard", () => userGesture("keydown", "m")],
  ...[
    "nav.focus-left-search",
    "nav.open-command-palette",
    "viewer.toggle-code-editor",
    "viewer.toggle-source-diff",
  ].map(
    (id) =>
      [
        id,
        () => {
          seedTerminalTabs(A, "a2", ["menu-terminal"]);
          dispatchMenuAction(id);
        },
      ] as const,
  ),
  ["editor pointer entry", () => userGesture("pointerdown")],
  ["keyboard pane cycle", () => userGesture("keydown", "`")],
  ["menu pane focus", () => dispatchMenuAction("viewer.focus-pane-right")],
  ["overlay keyboard entry", () => userGesture("keydown")],
  [
    "native panel removal with the pane still open",
    () => {
      useViewerStore.setState({
        rightPaneOpen: true,
        rightPaneLayout: {
          sections: [
            {
              id: "native-panels",
              group: { panelIds: ["scratch", "weft-board"], activePanelId: "scratch" },
              collapsed: false,
              size: 1,
            },
          ],
        },
      });
      dispatchMenuAction("panel:scratch");
      expect(s().rightPaneOpen).toBe(true);
      expect(s().rightPaneLayout.sections[0].group.panelIds).toEqual(["weft-board"]);
    },
  ],
  ["right pane toggle", () => s().toggleRightPane()],
  ["document close", () => s().closeTab(A, "", "/closed.md", { user: true })],
  ["close other documents", () => s().closeOtherTabs(A, "", "/kept.md")],
  ["close all documents", () => s().closeAllTabs(A)],
  ["foreground divider", () => s().setForegroundNodeSizes(A, [], [30, 70])],
  [
    "workspace move",
    () =>
      s().placeWorkspaceTile(
        A,
        "a1",
        { targetWorkspaceId: "a2", axis: "column", side: "after" },
        { user: true },
      ),
  ],
  ["tab preset", () => s().setTabGridPreset(A, "a2", "rows")],
  [
    "tab divider",
    () =>
      s().setTabGridSizes(
        A,
        "a2",
        "columns",
        {
          kind: "split",
          axis: "row",
          children: [
            { kind: "leaf", workspaceId: "one" },
            { kind: "leaf", workspaceId: "two" },
          ],
          sizes: [50, 50],
        },
        [],
        [30, 70],
      ),
  ],
  [
    "terminal split",
    () => {
      seedTerminalTabs(A, "a2", ["split-one", "split-two"]);
      splitTerminal(A);
    },
  ],
  ["show all terminal cells", () => showAllTabs(A, "a2")],
  ["show one terminal cell", () => showOneTab(A, "a2")],
  ["zoom terminal cell", () => zoomTerminalCell(A, "a2", "one")],
  ["tile drag", () => dropWorkspace(A, "a1", tileZoneId("a2", "bottom"))],
  ["workspace reorder", () => s().reorderWorkspace(A, "a1", { after: "a2" })],
  [
    "terminal reorder",
    () => {
      const hook = renderHook(() => useTileTabActions(A, "a2", () => {}));
      act(() => hook.result.current.onReorderTab(0, 1));
      hook.unmount();
    },
  ],
  ["pane out cell", () => cellArrangement("onPaneOutTab")],
  ["demote cell", () => cellArrangement("onDemotePane")],
  ["file open", () => s().openFile(A, "", "/chosen.md")],
  ["file preview", () => s().openFilePreview(A, "", "/chosen.md")],
  ["existing file tab", () => s().setActiveTab(A, null, "/chosen.md")],
  [
    "file tab cycle",
    () => {
      useViewerStore.setState({
        openTabs: {
          [A]: [
            { path: "/one.md", name: "one" },
            { path: "/two.md", name: "two" },
          ],
        },
        activeTab: { [A]: "/one.md" },
      });
      s().cycleTab(A, "file", 1);
    },
  ],
  [
    "file Back",
    () => {
      useViewerStore.setState({
        navHistory: { [A]: ["/one.md", "/two.md"] },
        navIndex: { [A]: 1 },
      });
      s().navigateBack(A, "");
    },
  ],
  [
    "file Forward",
    () => {
      useViewerStore.setState({
        navHistory: { [A]: ["/one.md", "/two.md"] },
        navIndex: { [A]: 0 },
      });
      s().navigateForward(A, "");
    },
  ],
  ["layout toggle", () => s().setLayoutMode("console")],
  ["CLI collapse", () => s().setLayoutMode("viewer")],
  ["CLI visibility", () => s().setTerminalVisible(false)],
  ["CLI expansion", () => s().setCliExpanded(true)],
  ["CLI preset", () => s().setForegroundPreset(A, "rows")],
  ["split resize", () => s().setTerminalSplitRatio(0.7)],
] as const)("a user's %s supersedes a Show awaiting adoption", async (_label, navigate) => {
  let finish!: () => void;
  const waiting = new Promise<void>((resolve) => {
    finish = resolve;
  });
  const adopt = vi.spyOn(s(), "adoptTerminalSession").mockImplementation(() => waiting);
  try {
    const pending = runShowPlan(
      {
        kind: "adopt",
        teamKey: A,
        sessionId: "slow-agent",
        storyId: null,
        tabDir: null,
        dir: null,
        confirm: false,
        ended: { runId: null, storyId: null },
      },
      { handle: "slow-agent" },
      { teamKey: A, name: "Agent" },
    );
    expect(adopt).toHaveBeenCalledOnce();
    navigate();
    await settle();
    h.focus.mockClear();
    const chrome = cliChromeOf(s());
    finish();
    expect(await pending).toMatchObject({ notShown: expect.stringContaining("superseded") });
    expect(cliChromeOf(s())).toEqual(chrome);
    expect(h.focus).not.toHaveBeenCalled();
  } finally {
    finish();
    adopt.mockRestore();
  }
});

it.each([
  "pinned",
  "preview",
  "background",
  "settlement",
  "document-close",
  "grid-reconcile",
  "tree-sync",
  "automatic-placement",
  "passive-focus",
  "Shift",
  "Control",
  "Alt",
  "Meta",
  "CapsLock",
  "AltGraph",
  "Fn",
  "FnLock",
  "NumLock",
  "ScrollLock",
  "Symbol",
  "SymbolLock",
  "Hyper",
  "Super",
  "dirty-document-close",
] as const)("automatic %s navigation preserves a pending Show", async (kind) => {
  let finish!: () => void;
  const waiting = new Promise<void>((resolve) => {
    finish = resolve;
  });
  const adopt = vi.spyOn(s(), "adoptTerminalSession").mockImplementation(() => waiting);
  try {
    const pending = runShowPlan(
      {
        kind: "adopt",
        teamKey: A,
        sessionId: "slow-agent",
        storyId: null,
        tabDir: null,
        dir: null,
        confirm: false,
        ended: { runId: null, storyId: null },
      },
      { handle: "slow-agent" },
      { teamKey: A, name: "Agent" },
    );
    if (
      [
        "Shift",
        "Control",
        "Alt",
        "Meta",
        "CapsLock",
        "AltGraph",
        "Fn",
        "FnLock",
        "NumLock",
        "ScrollLock",
        "Symbol",
        "SymbolLock",
        "Hyper",
        "Super",
      ].includes(kind)
    ) {
      const before = currentContextNavigationRevision();
      userGesture("keydown", kind);
      expect(currentContextNavigationRevision()).toBe(before);
    } else if (kind === "passive-focus") {
      const stop = installUserNavigationCapture(document);
      const editor = document.createElement("textarea");
      document.body.append(editor);
      editor.focus();
      editor.blur();
      editor.remove();
      stop();
    } else if (kind === "dirty-document-close") {
      s().setEditingDirtyPaths(A, ["/dirty.md"]);
      s().closeTab(A, "", "/dirty.md");
      s().setEditingDirtyPaths(A, []);
    } else if (kind === "tree-sync") {
      s().syncWorkspaceFrontmatter(A, { stories: [], subEpics: [] } as never);
    } else if (kind === "document-close") s().closeTab(A, "", "/auto.md");
    else if (kind === "grid-reconcile") s().collapseTabGridToTabs(A, "a2");
    else if (kind === "automatic-placement")
      s().placeWorkspaceTile(A, "a1", { targetWorkspaceId: "a2", axis: "row", side: "after" });
    else if (kind === "settlement") {
      useViewerStore.setState({ layoutMode: null, terminalVisible: false });
      const revision = currentContextNavigationRevision();
      s().settleInitialLayout(A);
      expect(currentContextNavigationRevision()).toBe(revision);
      expect(s().layoutMode).toBe("shared");
    } else if (kind === "preview")
      openFileInTeam({ org: "acme", team: "core", path: "/auto.md" }, { keepLayout: true });
    else s().openFile(kind === "background" ? B : A, "", "/auto.md", { keepLayout: true });
    finish();
    expect(await pending).toEqual({ shown: "adopted" });
    expect(h.focus).toHaveBeenCalledWith(A);
  } finally {
    finish();
    adopt.mockRestore();
  }
});

it("automatic CLI revelation does not invalidate its own pending navigation", () => {
  useViewerStore.setState({
    layoutMode: "viewer",
    terminalVisible: false,
    lastTerminalMode: "console",
  });
  const revision = currentContextNavigationRevision();
  s().expandTerminalOnOpen(A);
  expect(currentContextNavigationRevision()).toBe(revision);
  expect(cliChromeOf(s())).toMatchObject({
    layoutMode: "console",
    visible: true,
    lastTerminalMode: "console",
  });
});

it.each(["empty", "all-hidden"] as const)(
  "history skips an org with %s teams while preserving Back",
  (kind) => {
    useOrgStore.setState({
      orgs: [...useOrgStore.getState().orgs, org("empty", kind === "empty" ? [] : ["hidden"])],
      shownTeamsByOrgSlug: { empty: [] },
    });
    expect(switchContextToOrg("empty", "switcher")).toBe(true);
    expect(currentContextEntry()).toBeNull();
    expect(s().contextHistory).toEqual({ back: [{ teamKey: A, workspaceId: "a2" }], forward: [] });
    expect(contextBack()).toBe(true);
    expect(getActiveTeamKey()).toBe(A);
    expect(s().contextHistory).toEqual({ back: [], forward: [] });
    expect(contextForward()).toBe(false);
    switchContextToOrg("empty", "switcher");
    switchContext({ teamKey: B }, "switcher");
    expect(s().contextHistory.back).toEqual([{ teamKey: A, workspaceId: "a2" }]);
  },
);

it.each(["file", "pointer", "keyboard"] as const)(
  "first adoption records revocable focus intent before %s navigation",
  async (gesture) => {
    const teamKey = `first:adoption-${gesture}`;
    useOrgStore.setState({
      orgs: [org("first", [`adoption-${gesture}`])],
      activeOrgSlug: "first",
      activeTeamName: `adoption-${gesture}`,
    });
    useViewerStore.setState({
      openedTeams: [teamKey],
      workspaces: { [teamKey]: [ws("cold")] },
      activeWorkspace: { [teamKey]: "cold" },
    });
    expect(currentActivationGeneration(teamKey)).toBe(0);
    let finish!: () => void;
    const wait = new Promise<void>((resolve) => {
      finish = resolve;
    });
    const adopt = vi.spyOn(s(), "adoptTerminalSession").mockImplementation(() => wait);
    try {
      const pending = runShowPlan(
        {
          kind: "adopt",
          teamKey,
          sessionId: "cold-agent",
          storyId: null,
          tabDir: null,
          dir: null,
          confirm: false,
          ended: { runId: null, storyId: null },
        },
        { handle: "cold-agent" },
        { teamKey, name: "Agent" },
      );
      expect(adopt).toHaveBeenCalledOnce();
      const attachedAt = currentActivationGeneration(teamKey);
      expect(attachedAt).toBeGreaterThan(0);
      if (gesture === "file") s().openFile(teamKey, "", "/chosen.md");
      else userGesture(gesture === "pointer" ? "pointerdown" : "keydown");
      expect(shouldFocusTerminalOnReveal(teamKey, "cold", attachedAt)).toBe(false);
      finish();
      expect(await pending).toMatchObject({ notShown: expect.stringContaining("superseded") });
      expect(h.focus).not.toHaveBeenCalled();
    } finally {
      finish();
      adopt.mockRestore();
    }
  },
);

describe("workspace selection acknowledgements respect request order", () => {
  it.each(["local-current", "local-next", "numeric-current"] as const)(
    "a newer %s choice retires a pending remote ACK",
    async (chosen) => {
      const key = `${A}|a2`;
      seedTerminalTabs(A, "a2", ["local-current", "local-next"], { active: "local-current" });
      const remote = connectTestRemoteHost();
      remote.seed(
        key,
        { displayName: "a2", kind: "generic", org: "acme", team: "core" },
        {
          session: "remote-late",
          tags: { remoteHandle: "late", remoteRequestId: "late" },
        },
      );
      refetchTestLayout();
      let finish!: () => void;
      const request = vi
        .spyOn(terminalLayoutSourceMock, "terminalSetActive")
        .mockImplementationOnce(
          () =>
            new Promise<void>((resolve) => {
              finish = resolve;
            }),
        );
      try {
        s().setActiveTerminalTab(A, "remote-late", "a2");
        if (chosen === "numeric-current") dispatchMenuAction("viewer.jump-terminal-tab-1");
        else s().setActiveTerminalTab(A, chosen, "a2");
        await settle();
        finish();
        await settle();
        expect(selectActiveTerminalTab(s(), A, "a2")).toBe(
          chosen === "numeric-current" ? "local-current" : chosen,
        );
        expect(s().terminalLayout.focus).toEqual({});
        expect(request).toHaveBeenCalledTimes(chosen === "local-next" ? 2 : 1);
      } finally {
        finish();
        request.mockRestore();
      }
    },
  );
  it.each([
    ["both succeed", false, false, false],
    ["first fails", true, false, false],
    ["latest fails", false, true, false],
    ["completed success survives a later failure", false, true, true],
  ] as const)("%s", async (_name, failFirst, failLatest, firstCompletesBeforeNextRequest) => {
    const key = `${A}|a2`;
    seedTerminalTabs(A, "a2", ["local"]);
    const remote = connectTestRemoteHost();
    for (const session of ["remote-a", "remote-b", "remote-initial"]) {
      remote.seed(
        key,
        { displayName: "a2", kind: "generic", org: "acme", team: "core" },
        {
          session,
          tags: { remoteHandle: session, remoteRequestId: session },
        },
      );
    }
    refetchTestLayout();
    s().applyLayout({ kind: "focused", key, session: "remote-initial" });
    let finishFirst!: () => void;
    let finishLatest!: () => void;
    const held = (fail: boolean, finish: (done: () => void) => void) =>
      new Promise<void>((resolve, reject) =>
        finish(() => (fail ? reject(new Error("refused")) : resolve())),
      );
    const request = vi
      .spyOn(terminalLayoutSourceMock, "terminalSetActive")
      .mockImplementationOnce(() =>
        held(failFirst, (done) => {
          finishFirst = done;
        }),
      )
      .mockImplementationOnce(() =>
        held(failLatest, (done) => {
          finishLatest = done;
        }),
      );
    try {
      s().setActiveTerminalTab(A, "remote-a", "a2");
      if (firstCompletesBeforeNextRequest) {
        finishFirst();
        await settle();
      }
      s().setActiveTerminalTab(A, "remote-b", "a2");
      finishLatest();
      await settle();
      finishFirst();
      await settle();
      const wanted = failLatest
        ? firstCompletesBeforeNextRequest
          ? "remote-a"
          : "remote-initial"
        : "remote-b";
      expect(selectActiveTerminalTab(s(), A, "a2")).toBe(wanted);
      expect(s().terminalLayout.focus).toEqual({ [key]: wanted });
    } finally {
      request.mockRestore();
    }
  });
});

function userGesture(kind: "pointerdown" | "keydown", key = "Tab"): void {
  const stop = installUserNavigationCapture(document);
  const editor = document.createElement("textarea");
  // Portaled surfaces are siblings of the workbench, directly in the document.
  document.body.append(editor);
  try {
    editor.dispatchEvent(
      kind === "keydown"
        ? new KeyboardEvent("keydown", { key, metaKey: key === "`", bubbles: true })
        : new MouseEvent("pointerdown", { bubbles: true }),
    );
  } finally {
    stop();
    editor.remove();
  }
}

it("programmatic focus and blur preserve pending navigation", () => {
  const stop = installUserNavigationCapture(document);
  const editor = document.createElement("textarea");
  document.body.append(editor);
  const revision = currentContextNavigationRevision();
  try {
    editor.focus();
    editor.blur();
    editor.focus();
    expect(currentContextNavigationRevision()).toBe(revision);
  } finally {
    stop();
    editor.remove();
  }
});

function cellArrangement(action: "onPaneOutTab" | "onDemotePane"): void {
  seedTerminalTabs(A, "a2", ["arrange-one", "arrange-two"], {
    paned: ["arrange-one", "arrange-two"],
  });
  const hook = renderHook(() => useTileTabActions(A, "a2", () => {}));
  act(() => hook.result.current[action]("arrange-two"));
  hook.unmount();
}

it("a remote ACK stands down after newer file navigation", async () => {
  const key = seedTerminalTabs(A, "a2", ["local-initial"]);
  const remote = connectTestRemoteHost();
  remote.seed(
    key,
    { displayName: "a2", kind: "generic", org: "acme", team: "core" },
    {
      session: "remote-late",
      tags: { remoteHandle: "late", remoteRequestId: "late" },
    },
  );
  refetchTestLayout();
  let finish!: () => void;
  const request = vi.spyOn(terminalLayoutSourceMock, "terminalSetActive").mockImplementationOnce(
    () =>
      new Promise<void>((resolve) => {
        finish = resolve;
      }),
  );
  try {
    s().setActiveTerminalTab(A, "remote-late", "a2");
    s().openFile(A, "", "/newer.md");
    finish();
    await settle();
    expect(selectActiveTerminalTab(s(), A, "a2")).toBe("local-initial");
    expect(s().terminalLayout.focus).toEqual({});
    expect(s().activeTab[A]).toBe("/newer.md");
  } finally {
    finish();
    request.mockRestore();
  }
});
