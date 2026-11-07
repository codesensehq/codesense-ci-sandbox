// SPDX-License-Identifier: MIT
import { projectSessionStatus, resetSessionProjection } from "@/test/session-projection-fixture";
import { seedAgentListFixture } from "../lib/agent-list-test-fixture";
import { describe, expect, it, vi, beforeEach } from "vitest";
import { act, renderHook } from "@testing-library/react";

import type { ActivitySnapshot, AgentSessionRow, MachineSummary } from "@/tauri/commands";
import { projectedAgentStatus } from "./use-agent-status-of";
import type { TerminalTabIndex, Workspace } from "@/shared/stores/viewer-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { useMachinesStore } from "@/features/machines/stores/machines-store";

type Tab = { id: string; sessionId: string; name: string; origin?: { machineId: string } };
let tabIndex: Record<string, Record<string, Tab[]>> = {};
let workspaces: Record<string, { id: string; kind: string }[]> = {};
let agentIdentity: Record<string, Record<string, { face?: string }>> = {};
vi.mock("@/features/viewer/lib/cli-workspace/store/terminal-selectors", () => ({
  useTerminalTabIndex: () => tabIndex,
  useRunningTerminalTabIndex: () => tabIndex,
  useMinimizedTerminalTabIndex: () => ({}),
}));
vi.mock("@/shared/stores/viewer-store", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/shared/stores/viewer-store")>();
  return {
    findTabLocationBySessionId: actual.findTabLocationBySessionId,
    selectHiddenTerminalTabIndex: () => ({}),
    normalizeWorkspaceDir: actual.normalizeWorkspaceDir,
    useViewerStore: (sel: (s: unknown) => unknown) =>
      sel({ workspaces, agentIdentity, softClosedWorkspaces: {}, terminalLayout: { byKey: {} } }),
  };
});
vi.mock("@/features/machines/stores/machines-store", async () => {
  const { create } = await import("zustand");
  return {
    useMachinesStore: create<{ byTeam: Record<string, MachineSummary[]> }>(() => ({ byTeam: {} })),
  };
});

const { crewsFor, memberMarksFor, useWorkspaceCrews, useWorkspaceTeamHue } =
  await import("./use-workspace-crews");
const { BUILTIN_FACE_SOURCE } = await import("@/shared/lib/avatar-packs");
const { useAgentStateStore, resetPrunedAgentSessionsForTests } =
  await import("@/shared/stores/agent-state-store");
const { useAgentListProjectionStore } = await import("@/features/viewer/lib/agent-list-projection");
const { resetManagedSessionStoreForTests } =
  await import("@/shared/stores/use-managed-session-store");

function setProjectionFixture(next: {
  sessions?: AgentSessionRow[] | null;
  error?: string | null;
}) {
  if (!next.sessions) {
    useAgentListProjectionStore.setState(next);
    return;
  }
  const placements = new Map(
    next.sessions.map((row) => {
      const location = Object.entries(tabIndex)
        .flatMap(([teamKey, workspaces]) =>
          Object.entries(workspaces).map(([workspaceId, tabs]) => ({ teamKey, workspaceId, tabs })),
        )
        .find((location) => location.tabs.some((tab) => tab.sessionId === row.sessionId));
      const teamKey = location?.teamKey ?? "t";
      const parent = next.sessions?.find(
        (parent) => parent.handle === row.launchedBy || parent.sessionId === row.launchedBy,
      );
      const parentLocation = parent?.sessionId
        ? Object.entries(tabIndex[teamKey] ?? {}).find(([, tabs]) =>
            tabs.some((tab) => tab.sessionId === parent.sessionId),
          )?.[0]
        : undefined;
      return [
        row.handle,
        {
          teamKey,
          workspaceId: location?.workspaceId,
          parentHandle: parent?.handle,
          crewParentHandle: parentLocation === location?.workspaceId ? parent?.handle : null,
        },
      ];
    }),
  );
  const sessions = next.sessions.map((session) =>
    projectSessionStatus(
      session.sessionId ?? session.handle,
      session.effectiveStatus ?? "unknown",
      session,
    ),
  );
  seedAgentListFixture(sessions, placements);
}

function row(handle: string, over: Partial<AgentSessionRow> = {}): AgentSessionRow {
  return {
    handle,
    sessionId: `sid-${handle}`,
    kind: "dashboard-launched",
    alive: true,
    backgrounded: false,
    startedWith: null,
    startedAtMs: 1,
    launcher: null,
    lastOutputMs: null,
    ...over,
  };
}
const tab = (handle: string): Tab => ({
  id: `sid-${handle}`,
  sessionId: `sid-${handle}`,
  name: handle,
});

beforeEach(() => {
  resetSessionProjection();
  useMachinesStore.setState({ byTeam: {} });
  resetManagedSessionStoreForTests();
  // Session ids are reused across tests; a prior test's closed tab must not
  // tombstone the next test's session.
  resetPrunedAgentSessionsForTests();
  useAgentStateStore.setState({
    bySession: {},
    activityBySession: {},
    exitBySession: {},
    processAgentBySession: {},
  });
  agentIdentity = {};
  workspaces = {
    t: [
      { id: "ws-ctl", kind: "generic" },
      { id: "ws-login", kind: "story" },
      { id: "ws-shell", kind: "generic" },
    ],
  };
  tabIndex = {
    t: {
      "ws-ctl": [tab("ctl")],
      "ws-login": [tab("lead"), tab("a"), tab("b"), tab("c"), tab("d")],
      "ws-shell": [{ id: "pty", sessionId: "pty", name: "zsh" }],
    },
  };
  setProjectionFixture({
    sessions: [
      row("ctl"),
      row("lead", { launchedBy: "ctl" }),
      ...["a", "b", "c", "d"].map((h) => row(h, { launchedBy: "lead" })),
    ],
    error: null,
  });
});

describe("useWorkspaceCrews", () => {
  it.each([
    ["thinking", undefined, false, "thinking", "thinking"],
    ["working", undefined, true, "tool", "tool-running"],
    ["needs-permission", undefined, false, "permission", "paused"],
    ["needs-permission", "question", false, "question", "awaiting-answer"],
    ["needs-permission", "blocked", false, "blocked", "blocked"],
    ["done", undefined, false, "ready", "idle"],
  ] as const)(
    "qualifies retained %s/%s crew observations through recovery without refreshing activity",
    (state, reason, toolOpen, mark, effectiveStatus) => {
      tabIndex.t["ws-login"] = [tab("lead"), tab("a")];
      setProjectionFixture({ sessions: [row("lead"), row("a", { launchedBy: "lead" })] });
      const snapshot: ActivitySnapshot = {
        session: "sid-lead",
        identity: { hostBootId: "boot", sessionIncarnation: "pty", invocationId: "run" },
        revision: 1,
        turnGeneration: 1,
        provider: "claude",
        state,
        reason,
        observed: true,
        limited: false,
        exit: null,
        asks: [],
        toolOpen,
        activityAgeMs: 180_000,
      };
      for (const id of ["sid-lead", "sid-a"])
        useAgentStateStore.getState().recordActivity({ ...snapshot, session: id }, Infinity);
      for (const id of ["sid-lead", "sid-a"]) projectSessionStatus(id, effectiveStatus);
      const activityAt = useAgentStateStore.getState().bySession["sid-lead"].activityAt;
      const { result } = renderHook(() => useWorkspaceCrews("t"));
      const originalSummary = result.current.get("ws-login")!.summary;
      for (const freshness of ["recovering", "unavailable"] as const) {
        act(() =>
          useAgentStateStore.getState().activityUnavailable(["sid-lead", "sid-a"], freshness),
        );
        const qualifier =
          freshness === "recovering"
            ? "Restoring agent activity; last observation may be stale"
            : "Agent activity unavailable; last observation may be stale";
        expect(result.current.get("ws-login")).toMatchObject({
          mark,
          summary: `${originalSummary}, ${qualifier}`,
        });
        expect(useAgentStateStore.getState().bySession["sid-lead"].activityAt).toBe(activityAt);
      }
      act(() => {
        for (const id of ["sid-lead", "sid-a"])
          useAgentStateStore.getState().recordActivity({ ...snapshot, session: id }, Infinity);
      });
      expect(result.current.get("ws-login")!.summary).toBe(originalSummary);
      expect(useAgentStateStore.getState().bySession["sid-lead"].activityAt).toBe(activityAt);
    },
  );

  it("qualifies unknown recovery and ignores unrelated activity changes", () => {
    tabIndex.t["ws-login"] = [tab("lead")];
    setProjectionFixture({ sessions: [row("lead")] });
    let renders = 0;
    const { result } = renderHook(() => {
      renders++;
      return useWorkspaceCrews("t");
    });
    const initialRenders = renders;
    act(() => useAgentStateStore.getState().activityUnavailable(["other"], "recovering"));
    expect(renders).toBe(initialRenders);
    act(() => useAgentStateStore.getState().activityUnavailable(["sid-lead"], "recovering"));
    expect(result.current.get("ws-login")).toMatchObject({
      mark: "unknown",
      summary: "1 agent, led by lead, Restoring agent activity",
    });
  });

  it.each([0, 137])("does not qualify a known terminal exit (%s) as recovering", (code) => {
    tabIndex.t["ws-login"] = [tab("lead")];
    setProjectionFixture({ sessions: [row("lead")] });
    useAgentStateStore.getState().recordSignal("sid-lead", {
      tier: 1,
      signal: { kind: "claude-hook", event: "UserPromptSubmit" },
    });
    useAgentStateStore.getState().recordProcess("sid-lead", { kind: "exited", code }, Infinity);
    projectSessionStatus("sid-lead", code === 0 ? "done" : "crashed");
    const { result } = renderHook(() => useWorkspaceCrews("t"));
    const original = result.current.get("ws-login");
    act(() => useAgentStateStore.getState().activityUnavailable(["sid-lead"], "recovering"));
    expect(result.current.get("ws-login")).toEqual(original);
    expect(result.current.get("ws-login")?.mark).toBe(code === 0 ? "ended" : "crashed");
  });

  it("updates the crew mark and accessible summary when its remote host disconnects and reconnects", () => {
    const machine: MachineSummary = {
      machineId: "build-box",
      kind: "ssh",
      label: "Build box",
      scope: null,
      declaration: null,
      binding: null,
      canPublish: false,
      status: { state: "connected", workspaces: 1 },
      attention: 0,
      capabilities: { agentCallbacks: true },
    };
    tabIndex.t["ws-login"] = [{ ...tab("lead"), origin: { machineId: machine.machineId } }];
    setProjectionFixture({ sessions: [row("lead")] });
    useMachinesStore.setState({ byTeam: { t: [machine] } });
    useAgentStateStore.getState().recordSignal("sid-lead", {
      tier: 1,
      signal: { kind: "claude-hook", event: "UserPromptSubmit" },
    });
    projectSessionStatus("sid-lead", "thinking");
    const record = useAgentStateStore.getState().bySession["sid-lead"];
    const { result, rerender } = renderHook(() => useWorkspaceCrews("t"));
    expect(result.current.get("ws-login")).toMatchObject({
      mark: "thinking",
      summary: "1 agent, led by lead: 1 thinking",
    });
    act(() =>
      useMachinesStore.setState({
        byTeam: { t: [{ ...machine, status: { state: "disconnected" } }] },
      }),
    );
    expect(result.current.get("ws-login")).toMatchObject({
      mark: "offline",
      summary: "1 agent, led by lead: 1 offline",
    });
    act(() => useAgentStateStore.getState().activityUnavailable(["sid-lead"], "recovering"));
    expect(result.current.get("ws-login")?.summary).toBe("1 agent, led by lead: 1 offline");
    act(() => useMachinesStore.setState({ byTeam: { t: [machine] } }));
    expect(result.current.get("ws-login")).toMatchObject({
      mark: "thinking",
      summary: "1 agent, led by lead: 1 thinking, Restoring agent activity",
    });
    expect(useAgentStateStore.getState().bySession["sid-lead"]).toBe(record);
    // The containing shell remains alive, but its agent's known exit wins even
    // when the machine subsequently disconnects.
    act(() =>
      useAgentStateStore
        .getState()
        .recordProcess("sid-lead", { kind: "exited", code: 0 }, Infinity),
    );
    act(() => {
      projectSessionStatus("sid-lead", "done");
    });
    rerender(); // This suite's viewer hook is a snapshot mock, not a subscription.
    expect(result.current.get("ws-login")?.mark).toBe("ended");
    act(() =>
      useMachinesStore.setState({
        byTeam: { t: [{ ...machine, status: { state: "disconnected" } }] },
      }),
    );
    expect(result.current.get("ws-login")?.mark).toBe("ended");
  });

  it("counts an agent started by hand in a plain tab, but never a bare or registered shell", () => {
    const bare = row("pty", {
      sessionId: "pty",
      kind: "hand-started",
      rowNames: { primary: "Claude", secondary: null, full: "Claude" },
    });
    tabIndex.t["ws-shell"].push({ id: "pty-2", sessionId: "pty-2", name: "zsh 2" });
    tabIndex.t["ws-shell"].push({ id: "pty-3", sessionId: "pty-3", name: "db reset" });
    setProjectionFixture({
      sessions: [
        bare,
        row("pty-2", { sessionId: "pty-2", kind: "live-pty" }),
        row("tab-3", { sessionId: "pty-3", kind: "registered", agentRevision: null }),
      ],
      error: null,
    });
    useAgentStateStore.getState().recordSignal("pty", {
      tier: 1,
      signal: { kind: "claude-hook", event: "PermissionRequest" },
    });
    projectSessionStatus("pty", "paused");
    const { result } = renderHook(() => useWorkspaceCrews("t"));
    const shell = result.current.get("ws-shell");
    expect(shell?.summary).toBe("1 agent, led by Claude: 1 needs approval");
    expect(shell?.leadPiece).toBe("worker");
    useAgentStateStore.getState().remove(["pty"]);
  });

  it("counts a Codex found only by its running process, with no hook", () => {
    setProjectionFixture({
      sessions: [
        row("pty", {
          sessionId: "pty",
          kind: "hand-started",
          rowNames: { primary: "Codex", secondary: null, full: "Codex" },
        }),
      ],
      error: null,
    });
    useAgentStateStore.getState().setProcessAgent("pty", "codex");
    projectSessionStatus("pty", "unknown");
    const { result } = renderHook(() => useWorkspaceCrews("t"));
    expect(result.current.get("ws-shell")?.summary).toBe("1 agent, led by Codex");
    useAgentStateStore.getState().setProcessAgent("pty", null);
    setProjectionFixture({ sessions: [] });
    const { result: after } = renderHook(() => useWorkspaceCrews("t"));
    expect(after.current.get("ws-shell")).toBeUndefined();
  });

  it("leads the team folder's pill with an agent named controller like any agent", () => {
    const ctl = "/c/content#controller";
    useOrgStore.setState({
      workspaceRepos: {
        t: { teamContentPath: "/c/content" } as ReturnType<
          typeof useOrgStore.getState
        >["workspaceRepos"][string],
      },
    });
    tabIndex = { t: { "ws-ctl": [tab(ctl)] } };
    setProjectionFixture({
      sessions: [row(ctl, { agentRevision: "acme/t/controller@1" })],
      error: null,
    });
    const { result } = renderHook(() => useWorkspaceCrews("t"));
    expect(result.current.get("ws-ctl")?.leadPiece).toBe("worker");
    expect(result.current.get("ws-ctl")?.leadFace.kind).toBe("image");

    // A face a person picks for it shows, as for any agent.
    agentIdentity = { t: { [ctl]: { face: "builtin-funny:tie-cat" } } };
    const { result: chosen } = renderHook(() => useWorkspaceCrews("t"));
    expect(chosen.current.get("ws-ctl")?.leadFace).toMatchObject({
      kind: "image",
      ref: "builtin-funny:tie-cat",
    });
  });

  it("numbers the faces two of the team's running agents share, the same in every pill", () => {
    setProjectionFixture({
      sessions: [
        row("ctl"),
        row("lead", { launchedBy: "ctl", startedAtMs: 2, avatar: "builtin-funny:tie-cat" }),
        row("a", { launchedBy: "lead", startedAtMs: 3, avatar: "builtin-funny:tie-cat" }),
        row("b", { launchedBy: "lead", startedAtMs: 4, avatar: "builtin-funny:fine-dog" }),
      ],
      error: null,
    });
    const { result } = renderHook(() => useWorkspaceCrews("t"));
    const login = result.current.get("ws-login");
    expect(login?.leadTag).toBe(1);
    expect(Object.fromEntries(login?.crew.map((m) => [m.key, m.tag]) ?? [])).toEqual({
      a: 2,
      b: undefined,
    });
  });

  it("leads each workspace's pill with its highest agent, stacks the rest and counts the overflow", () => {
    projectSessionStatus("sid-lead", "thinking");
    // Hooks report the lead mid-turn: it reads as working.
    useAgentStateStore.getState().recordSignal("sid-lead", {
      tier: 1,
      signal: { kind: "claude-hook", event: "UserPromptSubmit" },
    });
    const { result } = renderHook(() => useWorkspaceCrews("t"));
    const login = result.current.get("ws-login");
    expect(login?.leadPiece).toBe("manager");
    // The lead plus four others: the five portraits the card shows.
    expect(login?.crew.map((m) => m.key)).toEqual(["a", "b", "c", "d"]);
    expect(login?.more).toBe(0);
    // a UserPromptSubmit record (no tool open) reads `thinking`, the
    // finer sibling of the pre-WC-7 generic `working` mark; the summary carries
    // the matching finer verb.
    expect(login?.mark).toBe("thinking");
    expect(login?.summary).toMatch(/^5 agents, led by lead: .*thinking/);
    // The Controller's launcher role reads the whole tree, across workspaces.
    expect(result.current.get("ws-ctl")?.leadPiece).toBe("manager");
    // A workspace of plain shells has no crew.
    expect(result.current.has("ws-shell")).toBe(false);
  });

  it("reads unknown when nothing reports what its agents are doing", () => {
    const { result } = renderHook(() => useWorkspaceCrews("t"));
    expect(result.current.get("ws-login")?.mark).toBe("unknown");
  });

  it("turns red for an agent stuck on an error, over working, under a wait", () => {
    const hook = (sid: string, event: "UserPromptSubmit" | "StopFailure" | "PermissionRequest") =>
      useAgentStateStore
        .getState()
        .recordSignal(sid, { tier: 1, signal: { kind: "claude-hook", event } });
    hook("sid-lead", "UserPromptSubmit");
    hook("sid-a", "StopFailure");
    projectSessionStatus("sid-lead", "thinking");
    projectSessionStatus("sid-a", "failed");
    const { result, rerender } = renderHook(() => useWorkspaceCrews("t"));
    expect(result.current.get("ws-login")?.mark).toBe("error");
    hook("sid-b", "PermissionRequest");
    act(() => {
      projectSessionStatus("sid-b", "paused");
    });
    rerender();
    expect(result.current.get("ws-login")?.mark).toBe("permission");
  });

  it("turns amber when anyone inside waits on a person", () => {
    projectSessionStatus("sid-c", "awaiting-answer");
    const { result } = renderHook(() => useWorkspaceCrews("t"));
    // `awaiting-answer` maps to the specific `question` mark.
    expect(result.current.get("ws-login")?.mark).toBe("question");
    expect(result.current.get("ws-login")?.summary).toMatch(/: 1 needs you/);
  });

  it("gives each workspace its own team colour, matched by tray position", () => {
    tabIndex.t["ws-shell"] = [tab("solo")];
    setProjectionFixture({
      sessions: [...(useAgentListProjectionStore.getState().sessions ?? []), row("solo")],
    });
    const { result } = renderHook(() => useWorkspaceCrews("t"));
    expect(result.current.get("ws-shell")?.leadPiece).toBe("worker");
    expect(result.current.get("ws-shell")?.hue).not.toBe(result.current.get("ws-login")?.hue);
  });
});

describe("useWorkspaceTeamHue", () => {
  it("wears the same team colour as the workspace's pill, and none for an unlisted workspace", () => {
    const crews = renderHook(() => useWorkspaceCrews("t")).result.current;
    const hue = (id: string) => renderHook(() => useWorkspaceTeamHue("t", id)).result.current;
    expect(hue("ws-login")).toBe(crews.get("ws-login")?.hue);
    expect(hue("ws-ctl")).toBe(crews.get("ws-ctl")?.hue);
    expect(hue("ws-gone")).toBeNull();
    expect(renderHook(() => useWorkspaceTeamHue("other", "ws-login")).result.current).toBeNull();
  });
});

const TEAM = "acme:ops";
const crewWorkspaces = [{ id: "ws-a" }] as unknown as Workspace[];
const BOX = {
  machineId: "box",
  label: "Studio Box",
  kind: "ssh",
  status: { state: "connected" },
} as unknown as MachineSummary;

describe("crewsFor", () => {
  it("counts a workspace's agents and how many of them wait on a person", () => {
    const live = [
      projectSessionStatus("sid-lead", "thinking", row("lead")),
      projectSessionStatus("sid-worker", "awaiting-answer", row("worker", { launchedBy: "lead" })),
    ];
    const tabIndex = {
      [TEAM]: {
        "ws-a": [
          { id: "t-lead", sessionId: "sid-lead" },
          { id: "t-worker", sessionId: "sid-worker" },
        ],
      },
    } as unknown as TerminalTabIndex;
    const statusOf = projectedAgentStatus;
    seedAgentListFixture(
      live,
      new Map([
        ["lead", { teamKey: TEAM, workspaceId: "ws-a", crewLead: true }],
        [
          "worker",
          { teamKey: TEAM, workspaceId: "ws-a", parentHandle: "lead", crewParentHandle: "lead" },
        ],
      ]),
    );
    const memberMarks = memberMarksFor({
      teamKey: TEAM,
      workspaces: crewWorkspaces,
      tabIndex,
      live,
      statusOf,
      machines: {},
    });
    const crew = crewsFor({
      teamKey: TEAM,
      workspaces: crewWorkspaces,
      tabIndex,
      live,
      statusOf,
      machines: {},
      choices: {},
      faces: BUILTIN_FACE_SOURCE,
      tags: new Map(),
      memberMarks,
      qualifiers: {},
    }).get("ws-a");
    expect(crew).toMatchObject({ count: 2, needCount: 1, leadLabel: "Manager · launched 1" });
    expect(crew?.crew.map((m) => m.key)).toEqual(["worker"]);
  });
});

describe("memberMarksFor", () => {
  it("reads a tab held as it last was as offline, as the agents pane's row does", () => {
    const live = [projectSessionStatus("sid-far", "thinking", row("far"))];
    seedAgentListFixture(
      live,
      new Map([["far", { teamKey: TEAM, workspaceId: "ws-a", crewLead: true }]]),
    );
    const tabIndex = {
      [TEAM]: {
        "ws-a": [
          {
            id: "t-far",
            sessionId: "sid-far",
            origin: { machineId: "box", member: "remote:box", handle: "h-1", offlineSince: 1 },
          },
        ],
      },
    } as unknown as TerminalTabIndex;
    const marks = memberMarksFor({
      teamKey: TEAM,
      workspaces: crewWorkspaces,
      tabIndex,
      live,
      statusOf: projectedAgentStatus,
      // The machine itself reads connected: only the held tab says otherwise.
      machines: { [TEAM]: [BOX] },
    });
    expect(marks["sid-far"]).toBe("offline");
  });

  it("reads the machine as its own team lists it, as the card does, though another team lists it otherwise", () => {
    const live = [projectSessionStatus("sid-far", "thinking", row("far"))];
    seedAgentListFixture(
      live,
      new Map([["far", { teamKey: TEAM, workspaceId: "ws-a", crewLead: true }]]),
    );
    const tabIndex = {
      [TEAM]: {
        "ws-a": [
          {
            id: "t-far",
            sessionId: "sid-far",
            origin: { machineId: "box", member: "remote:box", handle: "h-1" },
          },
        ],
      },
    } as unknown as TerminalTabIndex;
    const unreachable = {
      ...BOX,
      status: { state: "unreachable", lastReachedAtMs: 1 },
    } as unknown as MachineSummary;
    const marksWith = (machines: Record<string, MachineSummary[]>) =>
      memberMarksFor({
        teamKey: TEAM,
        workspaces: crewWorkspaces,
        tabIndex,
        live,
        statusOf: projectedAgentStatus,
        machines,
      });
    // Another team, listed first, has it connected: this team's own list says unreachable.
    expect(marksWith({ "other:team": [BOX], [TEAM]: [unreachable] })["sid-far"]).toBe("offline");
    // And the other way round: this team has it connected.
    expect(marksWith({ "other:team": [unreachable], [TEAM]: [BOX] })["sid-far"]).not.toBe(
      "offline",
    );
  });
});
