// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, renderHook } from "@testing-library/react";
import {
  connectTestRemoteHost,
  installTerminalLayoutHarness,
  seedTerminalTabs,
} from "@/test/terminal-layout-harness";
import { selectWorkspaceTabs, useViewerStore } from "@/shared/stores/viewer-store";
import {
  agentInputSeq,
  resetPrunedAgentSessionsForTests,
  useAgentStateStore,
} from "@/shared/stores/agent-state-store";
import {
  resetManagedSessionStoreForTests,
  useManagedSessionStore,
} from "@/shared/stores/use-managed-session-store";
import { useMachinesStore } from "@/features/machines/stores/machines-store";
import { projectSessionStatus } from "@/test/session-projection-fixture";
import {
  projectedSession,
  useAgentListProjectionStore,
} from "@/features/viewer/lib/agent-list-projection";
import { useAgentStatusOf } from "./use-agent-status-of";
import {
  useScopeMark,
  useStoryMarks,
} from "@/features/viewer/lib/cli-workspace/store/terminal-tab-selectors";
import { inLens } from "@/shared/lib/nav-lens";
import { markOfAgentStatus } from "@/shared/lib/status-mark";
import type { AgentSessionRow, MachineSummary } from "@/tauri/commands";

vi.mock("@/shared/lib/private-terminal-authority", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/shared/lib/private-terminal-authority")>()),
  acceptsPrivateAuthority: () => true,
  currentPrivateAuthorityEpoch: () => "7",
}));

installTerminalLayoutHarness();
const TEAM = "acme:default";
const WS = "SW-1";
const story = { filePath: "/stories/SW-1.md", storyId: WS };
const stories = [story];

beforeEach(() => {
  useAgentListProjectionStore.setState({ sessions: [] });
  resetPrunedAgentSessionsForTests();
  resetManagedSessionStoreForTests();
  useAgentStateStore.setState({
    bySession: {},
    activityBySession: {},
    exitBySession: {},
    stateSince: {},
    outputAtBySession: {},
    completionSeqBySession: {},
  });
  useMachinesStore.setState({ byTeam: {} });
  useViewerStore.setState({
    aiStatusBySession: {},
    progressTickBySession: {},
    workspaces: {
      [TEAM]: [
        {
          id: WS,
          kind: "story",
          cwd: "/wt",
          status: "ready",
          storyId: WS,
          storyFilePath: story.filePath,
        },
      ],
    },
  });
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

function start(sessionId: string) {
  act(() => projectSessionStatus(sessionId, "thinking"));
  act(() =>
    useAgentStateStore.getState().recordSignal(sessionId, {
      tier: 1,
      signal: { kind: "claude-hook", event: "UserPromptSubmit" },
    }),
  );
}

describe("shared agent status surfaces", () => {
  it.each([
    ["done", 0, "ended"],
    ["crashed", 137, "crashed"],
  ] as const)(
    "retains a provider's %s after disconnect while its shell stays alive",
    (state, code, mark) => {
      const machine = {
        machineId: "box",
        label: "Build box",
        kind: "ssh",
        status: { state: "connected" },
      } as MachineSummary;
      useMachinesStore.setState({ byTeam: { [TEAM]: [machine] } });
      const remote = connectTestRemoteHost("box", "7");
      remote.seed(
        seedTerminalTabs(TEAM, WS, []),
        { displayName: WS, kind: "story", org: "acme", team: "default" },
        { session: "agent-shell", tags: { remoteHandle: "agent-shell" } },
      );
      const shell = selectWorkspaceTabs(useViewerStore.getState(), TEAM, WS)[0];
      const sessionId = shell.sessionId;
      useAgentStateStore.getState().recordActivity(
        {
          session: sessionId,
          identity: { hostBootId: "boot", sessionIncarnation: "shell", invocationId: "agent" },
          revision: 1,
          turnGeneration: 1,
          provider: "claude",
          state,
          observed: true,
          limited: false,
          exit: { code, signal: null, requested: false },
          asks: [],
          toolOpen: false,
          activityAgeMs: 0,
        },
        agentInputSeq(),
      );
      const row = projectSessionStatus(sessionId, state);
      const list = renderHook(() => useAgentStatusOf([row]));
      const tab = renderHook(() => useScopeMark({ sessionIds: [sessionId] }));
      const pill = renderHook(() => useScopeMark({ teamKey: TEAM, wsIds: [WS] }));
      const nav = renderHook(() => useStoryMarks(TEAM, stories));
      const marks = () => [
        markOfAgentStatus(list.result.current(row)),
        tab.result.current.mark,
        pill.result.current.mark,
        nav.result.current[story.filePath],
      ];
      expect(shell.exit).toBeUndefined();
      expect(useAgentStateStore.getState().exitBySession[sessionId]).toBeUndefined();
      expect(marks()).toEqual([mark, mark, mark, mark]);
      act(() =>
        useMachinesStore.setState({
          byTeam: { [TEAM]: [{ ...machine, status: { state: "disconnected" } }] },
        }),
      );
      expect(marks()).toEqual([mark, mark, mark, mark]);
    },
  );

  it.each(["live-pty", "registered"])(
    "a projected %s shell has no agent mark in its tab, workspace or story rollup",
    (kind) => {
      seedTerminalTabs(TEAM, WS, ["plain"]);
      projectSessionStatus("plain", "unknown", { hasAgent: false, kind, agentRevision: null });
      const tab = renderHook(() => useScopeMark({ sessionIds: ["plain"] }));
      const pill = renderHook(() => useScopeMark({ teamKey: TEAM, wsIds: [WS] }));
      const nav = renderHook(() => useStoryMarks(TEAM, stories));
      expect(tab.result.current.mark).toBeUndefined();
      expect(pill.result.current.mark).toBeUndefined();
      expect(nav.result.current[story.filePath]).toBeUndefined();
    },
  );

  it("selects managed outcomes and asks without rerendering unrelated scopes", () => {
    seedTerminalTabs(TEAM, WS, ["s"]);
    start("s");
    const renders = { tab: 0, pill: 0, nav: 0 };
    const tab = renderHook(() => {
      renders.tab++;
      return useScopeMark({ sessionIds: ["s"] });
    });
    const pill = renderHook(() => {
      renders.pill++;
      return useScopeMark({ teamKey: TEAM, wsIds: [WS] });
    });
    const nav = renderHook(() => {
      renders.nav++;
      return useStoryMarks(TEAM, stories);
    });
    const baseline = { ...renders };
    act(() =>
      useManagedSessionStore.getState().addPendingAsk({
        sessionId: "unrelated",
        correlationId: "unrelated-ask",
        question: "Which target?",
      }),
    );
    expect(renders).toEqual(baseline);
    act(() => useManagedSessionStore.getState().applyDoneStatus("unrelated", "success"));
    expect(renders).toEqual(baseline);
    act(() => {
      useManagedSessionStore.getState().applyDoneStatus("s", "success");
      projectSessionStatus("s", "completed");
    });
    expect(tab.result.current.mark).toBe("ended");
    expect(pill.result.current.mark).toBe("ended");
    expect(nav.result.current[story.filePath]).toBe("ended");
    for (const scope of ["tab", "pill", "nav"] as const)
      expect(renders[scope]).toBeGreaterThan(baseline[scope]);
    act(() =>
      useManagedSessionStore.getState().addPendingAsk({
        sessionId: "s",
        correlationId: "own-ask",
        question: "Which target?",
      }),
    );
    expect(tab.result.current.mark).toBe("question");
    expect(pill.result.current.mark).toBe("question");
    expect(nav.result.current[story.filePath]).toBe("question");
  });

  it("crosses the stall threshold together when list, tab and story hooks mount at different times", () => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
    seedTerminalTabs(TEAM, WS, ["s"]);
    start("s");
    const row = {
      sessionId: "s",
      alive: true,
      get effectiveStatus() {
        return projectedSession("s")?.effectiveStatus;
      },
    } as AgentSessionRow;
    const rows = [row];
    const list = renderHook(() => useAgentStatusOf(rows));
    act(() => vi.advanceTimersByTime(30_000));
    const tab = renderHook(() => useScopeMark({ sessionIds: ["s"] }));
    act(() => vi.advanceTimersByTime(15_000));
    const nav = renderHook(() => useStoryMarks(TEAM, stories));
    expect(nav.result.current[story.filePath]).toBe("thinking");
    act(() => vi.advanceTimersByTime(555_000 - 1));
    expect(list.result.current(row)).toBe("thinking");
    expect(tab.result.current.mark).toBe("thinking");
    expect(nav.result.current[story.filePath]).toBe("thinking");
    act(() => {
      vi.advanceTimersByTime(1);
      projectSessionStatus("s", "stalled", { activityAtMs: 1_000_000, statusSinceMs: 1_600_000 });
    });
    expect(list.result.current(row)).toBe("stalled");
    expect(tab.result.current.mark).toBe("stalled");
    expect(tab.result.current.label).toBe("Stalled · 0s");
    expect(nav.result.current[story.filePath]).toBe("stalled");
  });

  it("keeps a disconnected bare shell out of Agents on it while retaining terminal availability", () => {
    const machine = {
      machineId: "box",
      label: "Build box",
      kind: "ssh",
      status: { state: "connected" },
    } as MachineSummary;
    useMachinesStore.setState({ byTeam: { [TEAM]: [machine] } });
    const remote = connectTestRemoteHost("box", "7");
    remote.seed(
      seedTerminalTabs(TEAM, WS, []),
      { displayName: WS, kind: "story", org: "acme", team: "default" },
      { session: "shell", tags: { remoteHandle: "shell" } },
    );
    const sessionId = selectWorkspaceTabs(useViewerStore.getState(), TEAM, WS)[0].sessionId;
    const tab = renderHook(() => useScopeMark({ sessionIds: [sessionId] }));
    const nav = renderHook(() => useStoryMarks(TEAM, stories));
    const hasAgent = () =>
      inLens("agents", { role: "active", mark: nav.result.current[story.filePath] });
    expect(hasAgent()).toBe(false);
    act(() =>
      useMachinesStore.setState({
        byTeam: { [TEAM]: [{ ...machine, status: { state: "disconnected" } }] },
      }),
    );
    expect(tab.result.current.mark).toBe("offline");
    expect(nav.result.current[story.filePath]).toBeUndefined();
    expect(hasAgent()).toBe(false);
    start(sessionId);
    expect(nav.result.current[story.filePath]).toBe("offline");
    expect(hasAgent()).toBe(true);
    act(() => useMachinesStore.setState({ byTeam: { [TEAM]: [machine] } }));
    expect(tab.result.current.mark).toBe("thinking");
    expect(nav.result.current[story.filePath]).toBe("thinking");
  });

  it.each([
    [null, "ended"],
    ["SIGKILL", "crashed"],
  ] as const)("does not turn an exited bare shell (%s) into an offline agent", (signal, mark) => {
    const machine = {
      machineId: "box",
      label: "Build box",
      kind: "ssh",
      status: { state: "connected" },
    } as MachineSummary;
    useMachinesStore.setState({ byTeam: { [TEAM]: [machine] } });
    const remote = connectTestRemoteHost("box", "7");
    remote.seed(
      seedTerminalTabs(TEAM, WS, []),
      { displayName: WS, kind: "story", org: "acme", team: "default" },
      { session: "shell", tags: { remoteHandle: "shell" }, exit: { code: 0, signal } },
    );
    const sessionId = selectWorkspaceTabs(useViewerStore.getState(), TEAM, WS)[0].sessionId;
    const tab = renderHook(() => useScopeMark({ sessionIds: [sessionId] }));
    const nav = renderHook(() => useStoryMarks(TEAM, stories));
    expect(tab.result.current.mark).toBe(mark);
    const exitLabel = tab.result.current.label;
    expect(nav.result.current[story.filePath]).toBe(mark);
    act(() =>
      useMachinesStore.setState({
        byTeam: { [TEAM]: [{ ...machine, status: { state: "disconnected" } }] },
      }),
    );
    expect(nav.result.current[story.filePath]).toBe(mark);
    expect(tab.result.current.mark).toBe(mark);
    expect(tab.result.current.label).toBe(exitLabel);
    expect(inLens("agents", { role: "active", mark: nav.result.current[story.filePath] })).toBe(
      false,
    );
  });

  it("only rerenders marks whose selected status changes when machines update", () => {
    const machine = {
      machineId: "box",
      label: "Build box",
      kind: "ssh",
      status: { state: "connected" },
    } as MachineSummary;
    useMachinesStore.setState({ byTeam: { [TEAM]: [machine] } });
    const remote = connectTestRemoteHost("box", "7");
    remote.seed(
      seedTerminalTabs(TEAM, WS, ["local-agent"]),
      { displayName: WS, kind: "story", org: "acme", team: "default" },
      { session: "remote-agent", tags: { remoteHandle: "remote-agent" } },
    );
    const remoteId = selectWorkspaceTabs(useViewerStore.getState(), TEAM, WS).find(
      (tab) => tab.origin?.machineId === "box",
    )!.sessionId;
    start("local-agent");
    start(remoteId);
    let localRenders = 0;
    let remoteRenders = 0;
    const local = renderHook(() => {
      localRenders++;
      return useScopeMark({ sessionIds: ["local-agent"] });
    });
    const remoteMark = renderHook(() => {
      remoteRenders++;
      return useScopeMark({ sessionIds: [remoteId] });
    });
    const baseline = [localRenders, remoteRenders];
    act(() =>
      useMachinesStore.setState({
        byTeam: {
          [TEAM]: [{ ...machine }],
          "other:team": [{ ...machine, machineId: "unrelated", status: { state: "disconnected" } }],
        },
      }),
    );
    expect([localRenders, remoteRenders]).toEqual(baseline);
    act(() =>
      useMachinesStore.setState({
        byTeam: { [TEAM]: [{ ...machine, status: { state: "disconnected" } }] },
      }),
    );
    expect(localRenders).toBe(baseline[0]);
    expect(local.result.current.mark).toBe("thinking");
    expect(remoteRenders).toBeGreaterThan(baseline[1]);
    expect(remoteMark.result.current.mark).toBe("offline");
  });
});
