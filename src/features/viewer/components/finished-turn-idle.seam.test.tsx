// SPDX-License-Identifier: MIT
/**
 * Seam test: an agent whose turn finished reads Idle everywhere it shows, at once
 * and whether or not anyone has looked at it. A Claude `Stop` hook goes through the
 * real status listener into the real stores. Explicit sessions projection fixtures
 * deliver Rust's result (raw-input parity lives in Rust), and the agents list, the
 * tab pill, the workspace pill and the left nav's story row all draw the grey Idle
 * ring (never a check), and the roll-ups over the session rank it as idle.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, render, screen } from "@/test-utils";

const h = vi.hoisted(() => ({
  handlers: new Map<string, Set<(e: { payload: unknown }) => void>>(),
  doneCalls: [] as unknown[],
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, cb: (e: { payload: unknown }) => void) => {
    let set = h.handlers.get(name);
    if (!set) h.handlers.set(name, (set = new Set()));
    set.add(cb);
    return () => set.delete(cb);
  }),
}));
vi.mock("@/shared/hooks/use-terminal-activity", () => ({ useTerminalActivity: () => {} }));
vi.mock("@/shared/lib/private-terminal-authority", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/shared/lib/private-terminal-authority")>()),
  acceptsPrivateAuthority: () => true,
  currentPrivateAuthorityEpoch: () => "7",
}));
vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  listNotes: vi.fn().mockResolvedValue([]),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  killPty: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
  sessionsHaveLiveAgent: vi.fn(async (queries: { ptyId: string }[]) =>
    queries.map((q) => ({ ptyId: q.ptyId, alive: true })),
  ),
  listAgentSignals: vi.fn().mockResolvedValue([]),
}));
vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: vi.fn(), debug: vi.fn() },
  initLogging: vi.fn().mockResolvedValue(undefined),
}));

import { renderHook } from "@testing-library/react";
import {
  connectTestRemoteHost,
  installTerminalLayoutHarness,
  seedTerminalTabs,
} from "@/test/terminal-layout-harness";
import { TerminalTileChrome } from "./terminal-tile-chrome";
import { WorkspacePill } from "./workspace-pill";
import { StoryAgentMark } from "@/features/dashboard/components/status-nav/nav-agent-mark";
import { useAgentStatusOf } from "@/features/viewer/hooks/use-agent-status-of";
import { useTerminalStatusEvents } from "@/shared/hooks/use-terminal-status-events";
import { useManagedSessionBridge } from "@/shared/hooks/use-managed-session-bridge";
import { resetLivenessPollerForTests } from "@/shared/hooks/use-agent-liveness-monitor";
import {
  resetDotSinceForTests,
  selectSessionMark,
  useScopeMark,
  watchDotSince,
} from "@/features/viewer/lib/cli-workspace/store/terminal-tab-selectors";
import {
  agentInputSeq,
  markSessionSeen,
  resetPrunedAgentSessionsForTests,
  selectAgentDots,
  useAgentStateStore,
} from "@/shared/stores/agent-state-store";
import {
  selectWorkspaceTabs,
  useViewerStore,
  type TerminalTab,
  type Workspace,
} from "@/shared/stores/viewer-store";
import {
  AGENT_STATUS_WORDS,
  isNeedsYouStatus,
  type AgentStatus,
} from "@/shared/lib/agent-status-config";
import { agentCrashSeq } from "@/shared/lib/agent-state";
import { markOfAgentStatus, topMark } from "@/shared/lib/status-mark";
import type { AgentSessionRow } from "@/tauri/commands";
import type { ActivitySnapshot, MachineSummary } from "@/tauri/commands";
import { useMachinesStore } from "@/features/machines/stores/machines-store";
import { useManagedSessionStore } from "@/shared/stores/use-managed-session-store";
import { projectSessionStatus } from "@/test/session-projection-fixture";
import { projectedSession } from "@/features/viewer/lib/agent-list-projection";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { selectNeedsYouKind } from "@/features/viewer/lib/session-status-signals";
import { useDashboardStream } from "@/app/hooks/use-dashboard-stream";
import { blockedLine, activityLine } from "./agents-tree-view";

class FakeResizeObserver {
  constructor(private cb: ResizeObserverCallback) {}
  observe() {
    this.cb([{ contentRect: { width: 600 } } as ResizeObserverEntry], this as never);
  }
  unobserve() {}
  disconnect() {}
}

const TEAM = "acme:default";
const WS = "LN-1";
const S = "sess-agent";
const BUSY = "sess-busy";
const STORY = { filePath: "/ctx/27-nav/LN-1.md", storyId: "LN-1" };
const workspace: Workspace = {
  id: WS,
  kind: "story",
  cwd: `/wt/${WS}`,
  status: "ready",
  storyId: STORY.storyId,
  storyFilePath: STORY.filePath,
} as Workspace;
const tab = (id: string, sessionId: string): TerminalTab => ({ id, name: id, sessionId });
const listRow: AgentSessionRow = {
  get effectiveStatus() {
    return projectedSession(S)?.effectiveStatus;
  },
  handle: "agent",
  sessionId: S,
  kind: "dashboard-launched",
  alive: true,
  backgrounded: false,
  startedWith: null,
  startedAtMs: null,
  launcher: null,
  lastOutputMs: null,
};

const projectedStatuses: Record<string, AgentStatus> = {
  ready: "idle",
  ended: "done",
  crashed: "crashed",
  error: "failed",
  question: "awaiting-answer",
  permission: "paused",
  blocked: "blocked",
  tool: "tool-running",
  thinking: "thinking",
  stalled: "stalled",
  unknown: "unknown",
  offline: "thinking",
};
function receiveStatus(status: AgentStatus, sessionId = S) {
  act(() =>
    projectSessionStatus(sessionId, status, {
      statusSinceMs:
        projectedSession(sessionId)?.effectiveStatus === status
          ? projectedSession(sessionId)?.statusSinceMs
          : Date.now(),
      subagents:
        useAgentStateStore.getState().activityBySession[sessionId]?.snapshot?.subagentsOpen ?? 0,
    }),
  );
}
function receiveMark(mark: string, sessionId = S) {
  receiveStatus(projectedStatuses[mark], sessionId);
}

function emit(name: string, payload: unknown) {
  const set = h.handlers.get(name);
  if (!set?.size) throw new Error(`no handler registered for ${name}`);
  act(() => {
    for (const cb of [...set]) {
      if (name === "mcp:agent-done") h.doneCalls.push(cb);
      cb({ payload });
    }
  });
}
const hook = (hook_event: string, status: string, session = S) => {
  emit("terminal-status-update", { tab_session_id: session, status, hook_event });
};

function renderSurfaces(sessionId = S) {
  const noop = vi.fn();
  render(
    <>
      <TerminalTileChrome
        workspace={workspace}
        isFocused
        tabs={[tab("p1", sessionId)]}
        activeTabId="p1"
        onCloseTab={noop}
        onHideTab={noop}
        onEndTab={noop}
        onEndOtherTabs={noop}
        onEndAllTabs={noop}
        onReorderTab={noop}
        onRenameTab={noop}
        onFocusTile={noop}
        onNewShell={noop}
        onToggleZoom={noop}
        onHideTile={noop}
        onCloseTile={noop}
        onSelectTab={noop}
      />
      <WorkspacePill
        workspace={workspace}
        isActive={false}
        dimmed
        navRow
        markScope={{ teamKey: TEAM, wsIds: [WS] }}
        isStatusPopoverOpen={false}
        onStatusPopoverOpenChange={noop}
        onSelect={noop}
        onClose={noop}
      />
      <StoryAgentMark teamKey={TEAM} story={STORY} storyLabel="LN-1" />
    </>,
  );
  const list = renderHook(() => useAgentStatusOf([listRow]));
  const rollup = renderHook(() => useScopeMark({ teamKey: TEAM, story: STORY }));
  return { list, rollup };
}

installTerminalLayoutHarness();

let stopDotSince: () => void = () => {};
let stopManagedBridge: () => void = () => {};
beforeEach(() => {
  useAgentListProjectionStore.setState({ sessions: [] });
  h.handlers.clear();
  h.doneCalls = [];
  vi.stubGlobal("ResizeObserver", FakeResizeObserver);
  resetLivenessPollerForTests();
  resetPrunedAgentSessionsForTests();
  resetDotSinceForTests();
  useAgentStateStore.setState({
    bySession: {},
    activityBySession: {},
    exitBySession: {},
    stateSince: {},
    seenAt: {},
    outputAtBySession: {},
    completionSeqBySession: {},
    completionActivityBySession: {},
  });
  useManagedSessionStore.setState({ states: {}, pendingAsks: {} });
  useMachinesStore.setState({ byTeam: {} });
  let now = 1_000;
  vi.spyOn(Date, "now").mockImplementation(() => ++now);
  useViewerStore.setState({
    foregroundLayout: {},
    activeWorkspace: { [TEAM]: WS },
    workspaces: { [TEAM]: [workspace] },
    softClosedWorkspaces: { [TEAM]: {} },
    aiStatusBySession: {},
    progressTickBySession: {},
    progressBySession: {},
  });
  seedTerminalTabs(TEAM, WS, [S, BUSY]);
  renderHook(() => useTerminalStatusEvents());
  stopManagedBridge = renderHook(() => useManagedSessionBridge()).unmount;
  stopDotSince = watchDotSince();
});
afterEach(() => {
  stopDotSince();
  vi.restoreAllMocks();
  vi.useRealTimers();
  vi.unstubAllGlobals();
  resetLivenessPollerForTests();
  useAgentStateStore.setState({ bySession: {}, exitBySession: {} });
  useViewerStore.setState({ aiStatusBySession: {} });
});

/** Every surface draws the Idle ring for the finished agent, and nothing draws a check. */
function expectIdleEverywhere(
  list: { result: { current: (row: AgentSessionRow) => AgentStatus } },
  rollup: { result: { current: { mark?: string } } },
) {
  const status = list.result.current(listRow);
  expect(status, "list row").toBe("idle");
  expect(markOfAgentStatus(status), "list row mark").toBe("ready");
  expect(AGENT_STATUS_WORDS[status]).toBe("Ready");
  expect(screen.getByTestId("tab-p1-ai-status"), "tab").toHaveAttribute("data-mark", "ready");
  expect(screen.getByTestId(`workspace-tab-${WS}-soft-closed-dot`), "pill").toHaveAttribute(
    "data-mark",
    "ready",
  );
  expect(screen.getByTestId("nav-story-agent-mark"), "nav story row").toHaveAttribute(
    "data-mark",
    "ready",
  );
  expect(rollup.result.current.mark, "roll-up").toBe("ready");
  expect(document.querySelector('[data-mark="done"], .cs-mark-check')).toBeNull();
}

describe("a finished turn reads Idle on every surface", () => {
  const expectEveryMark = (surfaces: ReturnType<typeof renderSurfaces>, mark: string) => {
    expect(markOfAgentStatus(surfaces.list.result.current(listRow))).toBe(mark);
    expect(surfaces.rollup.result.current.mark).toBe(mark);
    for (const id of [
      "tab-p1-ai-status",
      `workspace-tab-${WS}-soft-closed-dot`,
      "nav-story-agent-mark",
    ])
      expect(screen.getByTestId(id)).toHaveAttribute("data-mark", mark);
  };
  const hostSnapshot = (patch: Partial<ActivitySnapshot> = {}): ActivitySnapshot => ({
    session: S,
    identity: { hostBootId: "boot", sessionIncarnation: "pty", invocationId: "run" },
    revision: 1,
    turnGeneration: 1,
    provider: "claude",
    state: "thinking",
    observed: true,
    limited: false,
    exit: null,
    asks: [],
    activityAgeMs: 0,
    toolOpen: false,
    ...patch,
  });
  const host = (patch: Partial<ActivitySnapshot> = {}) =>
    act(() => {
      useAgentStateStore.getState().recordActivity(hostSnapshot(patch), agentInputSeq());
    });

  it.each([
    ["thinking", null, false, "thinking"],
    ["working", null, true, "tool"],
    ["needs-permission", "blocked", false, "blocked"],
    ["needs-permission", "question", false, "question"],
    ["needs-permission", null, false, "permission"],
    ["done", null, false, "ready"],
    ["errored", null, false, "error"],
  ] as const)(
    "retains %s (%s) consistently while connected activity recovers",
    (state, reason, toolOpen, mark) => {
      const surfaces = renderSurfaces();
      host({ state, reason, toolOpen });
      receiveMark(mark);
      expectEveryMark(surfaces, mark);
      const record = useAgentStateStore.getState().bySession[S];
      const since = useAgentStateStore.getState().stateSince[S];
      for (const freshness of ["recovering", "unavailable"] as const) {
        act(() => useAgentStateStore.getState().activityUnavailable([S], freshness));
        receiveMark(mark);
        expectEveryMark(surfaces, mark);
        const qualifier =
          freshness === "recovering" ? "Restoring agent activity" : "Agent activity unavailable";
        for (const id of [
          "tab-p1-ai-status",
          `workspace-tab-${WS}-soft-closed-dot`,
          "nav-story-agent-mark",
        ]) {
          expect(screen.getByTestId(id)).toHaveAccessibleName(new RegExp(qualifier));
          expect(screen.getByTestId(id)).toHaveAccessibleName(/last observation may be stale/);
        }
        expect(surfaces.rollup.result.current.label).toContain(qualifier);
        expect(useAgentStateStore.getState().bySession[S]).toBe(record);
        expect(useAgentStateStore.getState().stateSince[S]).toBe(since);
      }
      host({ state, reason, toolOpen });
      receiveMark(mark);
      expectEveryMark(surfaces, mark);
      expect(surfaces.rollup.result.current.label).not.toMatch(/Restoring|stale|unavailable/);
      expect(useAgentStateStore.getState().bySession[S]).toBe(record);
    },
  );

  it("keeps an unobserved recovering invocation Unknown across surfaces", () => {
    const surfaces = renderSurfaces();
    host({ observed: false, state: "idle", activityAgeMs: undefined });
    act(() => useAgentStateStore.getState().activityUnavailable([S], "recovering"));
    receiveMark("unknown");
    expectEveryMark(surfaces, "unknown");
    expect(surfaces.rollup.result.current.label).toContain("Restoring agent activity");
    expect(surfaces.rollup.result.current.label).not.toContain("last observation");
  });

  it("qualifies a multi-agent rollup even when a different member owns its top mark", () => {
    const surfaces = renderSurfaces();
    host();
    host({ session: BUSY, state: "working", toolOpen: true });
    receiveStatus("tool-running", BUSY);
    act(() => useAgentStateStore.getState().activityUnavailable([S], "recovering"));
    expect(surfaces.rollup.result.current.mark).toBe("tool");
    expect(surfaces.rollup.result.current.label).toContain("Restoring agent activity");
    for (const id of [`workspace-tab-${WS}-soft-closed-dot`, "nav-story-agent-mark"]) {
      expect(screen.getByTestId(id)).toHaveAttribute("data-mark", "tool");
      expect(screen.getByTestId(id)).toHaveAccessibleName(/Restoring agent activity/);
    }
  });

  it("clears a legacy self-reported block on PreToolUse without clearing a real permission", () => {
    const surfaces = renderSurfaces();
    emit("terminal-status-update", {
      tab_session_id: S,
      status: "needs-permission",
      source: "mcp",
      agent_signal: { kind: "signal-progress", phase: "blocked" },
    });
    receiveMark("blocked");
    expectEveryMark(surfaces, "blocked");
    hook("PreToolUse", "running");
    receiveMark("tool");
    expectEveryMark(surfaces, "tool");
    expect(useViewerStore.getState().aiStatusBySession[S]).toBe("running");
    hook("PermissionRequest", "needs-permission");
    hook("PreToolUse", "running");
    receiveMark("permission");
    expectEveryMark(surfaces, "permission");
  });

  it("preserves StopFailure after a clean agent exit while its shell stays alive", () => {
    const surfaces = renderSurfaces();
    hook("UserPromptSubmit", "running");
    hook("StopFailure", "error");
    act(() =>
      useAgentStateStore.getState().recordProcess(S, { kind: "exited", code: 0 }, agentInputSeq()),
    );
    receiveMark("error");
    expectEveryMark(surfaces, "error");
    expect(isNeedsYouStatus(surfaces.list.result.current(listRow))).toBe(true);
  });

  it("keeps a completed task intact but shows a genuinely new legacy turn, then its exit", () => {
    const surfaces = renderSurfaces();
    hook("UserPromptSubmit", "running");
    emit("mcp:agent-done", { sessionId: S, status: "success" });
    hook("PreToolUse", "running");
    receiveMark("ended");
    expectEveryMark(surfaces, "ended");
    hook("PostToolUse", "running");
    receiveMark("ended");
    expectEveryMark(surfaces, "ended");
    hook("UserPromptSubmit", "running");
    receiveMark("thinking");
    expectEveryMark(surfaces, "thinking");
    hook("PreToolUse", "running");
    receiveMark("tool");
    expectEveryMark(surfaces, "tool");
    hook("Stop", "done");
    receiveMark("ready");
    expectEveryMark(surfaces, "ready");
    expect(useManagedSessionStore.getState().states[S]).toBe("done");
    act(() =>
      useAgentStateStore.getState().recordProcess(S, { kind: "exited", code: 0 }, agentInputSeq()),
    );
    receiveMark("ended");
    expectEveryMark(surfaces, "ended");
  });

  it("keeps a pending question visible after subprocess exit until the ask closes", () => {
    const surfaces = renderSurfaces();
    hook("UserPromptSubmit", "running");
    act(() =>
      useManagedSessionStore.getState().addPendingAsk({
        correlationId: "pending-exit",
        sessionId: S,
        question: "Which target?",
      }),
    );
    act(() =>
      useAgentStateStore.getState().recordProcess(S, { kind: "exited", code: 0 }, agentInputSeq()),
    );
    receiveMark("question");
    expectEveryMark(surfaces, "question");
    receiveStatus("awaiting-answer");
    expect(surfaces.list.result.current(listRow)).toBe("awaiting-answer");
    expect(isNeedsYouStatus(surfaces.list.result.current(listRow))).toBe(true);
    act(() => useManagedSessionStore.getState().resolvePendingAsk("pending-exit"));
    receiveMark("ended");
    expectEveryMark(surfaces, "ended");
  });

  it("keeps an unresolved attention outcome visible after clean exit", () => {
    const surfaces = renderSurfaces();
    hook("UserPromptSubmit", "running");
    emit("mcp:agent-done", { sessionId: S, status: "needs-attention" });
    act(() =>
      useAgentStateStore.getState().recordProcess(S, { kind: "exited", code: 0 }, agentInputSeq()),
    );
    receiveStatus("dropped");
    expect(surfaces.list.result.current(listRow)).toBe("dropped");
    receiveMark("question");
    expectEveryMark(surfaces, "question");
    expect(isNeedsYouStatus(surfaces.list.result.current(listRow))).toBe(true);
  });

  it("uses admitted legacy tool evidence after the previous host invocation exited", () => {
    const surfaces = renderSurfaces();
    host({ state: "done", toolOpen: false, exit: { code: 0, signal: null, requested: false } });
    receiveMark("ended");
    expectEveryMark(surfaces, "ended");
    hook("UserPromptSubmit", "running");
    receiveMark("thinking");
    expectEveryMark(surfaces, "thinking");
    hook("PreToolUse", "running");
    receiveMark("tool");
    expectEveryMark(surfaces, "tool");
  });

  it("recognizes a newer host turn generation without mistaking same-turn output for a restart", () => {
    const surfaces = renderSurfaces();
    host();
    emit("mcp:agent-done", { sessionId: S, status: "success" });
    host({ revision: 2 });
    receiveMark("ended");
    expectEveryMark(surfaces, "ended");
    // SessionEnd also advances the host generation, but does not start a turn.
    host({ revision: 3, turnGeneration: 2, state: "idle" });
    receiveMark("ended");
    expectEveryMark(surfaces, "ended");
    host({ revision: 4, turnGeneration: 3 });
    receiveMark("thinking");
    expectEveryMark(surfaces, "thinking");
    host({ revision: 5, turnGeneration: 3, state: "working", toolOpen: true });
    receiveMark("tool");
    expectEveryMark(surfaces, "tool");
    host({
      revision: 6,
      turnGeneration: 3,
      state: "done",
      toolOpen: false,
      exit: { code: 0, signal: null, requested: false },
    });
    receiveMark("ended");
    expectEveryMark(surfaces, "ended");
  });

  it("shows positively observed host idle as Ready and leaves an ordinary shell unmarked", () => {
    const surfaces = renderSurfaces();
    expect(screen.queryByTestId("tab-p1-ai-status")).toBeNull();
    host({ state: "idle" });
    receiveMark("ready");
    expectEveryMark(surfaces, "ready");
  });

  it("reads Working · N subagents while a finished turn's subagents run, then Ready", () => {
    const surfaces = renderSurfaces();
    host({ state: "done", subagentsOpen: 2 });
    receiveMark("thinking");
    expectEveryMark(surfaces, "thinking");
    expect(screen.getByTestId("tab-p1-ai-status")).toHaveAccessibleName(/^Working · 2 subagents/);
    host({ revision: 2, state: "done", subagentsOpen: 1 });
    receiveStatus("running");
    expect(screen.getByTestId("tab-p1-ai-status")).toHaveAccessibleName(/^Working · 1 subagent ·/);
    host({ revision: 3, state: "done" });
    receiveMark("ready");
    expectEveryMark(surfaces, "ready");
  });

  it("anchors the first host invocation after reset without treating recovery as a new turn", () => {
    const surfaces = renderSurfaces();
    host();
    emit("mcp:agent-done", { sessionId: S, status: "success" });
    receiveMark("ended");
    expectEveryMark(surfaces, "ended");
    act(() => useAgentStateStore.getState().reset(S));
    host({ identity: { ...hostSnapshot().identity, invocationId: "next" } });
    receiveMark("ended");
    expectEveryMark(surfaces, "ended");
    host({
      identity: { ...hostSnapshot().identity, invocationId: "next" },
      revision: 2,
      turnGeneration: 2,
    });
    receiveMark("thinking");
    expectEveryMark(surfaces, "thinking");
    expect(useManagedSessionStore.getState().states[S]).toBe("done");
  });

  it("accepts a new tool start after a finished legacy turn without requiring a prompt hook", () => {
    const surfaces = renderSurfaces();
    hook("UserPromptSubmit", "running");
    emit("mcp:agent-done", { sessionId: S, status: "success" });
    hook("Stop", "done");
    receiveMark("ended");
    expectEveryMark(surfaces, "ended");
    hook("PreToolUse", "running");
    receiveMark("tool");
    expectEveryMark(surfaces, "tool");
  });

  const terminalResults = [
    ["success", "done", "ended"],
    ["failed", "failed", "error"],
    ["needs-attention", "needs-attention", "question"],
  ] as const;

  const completionBoundary = {
    identity: { hostBootId: "boot", sessionIncarnation: "pty", invocationId: "run" },
    turnGeneration: 2,
    revision: 5,
  };

  async function mountDashboard(dashboardFirst: boolean) {
    if (dashboardFirst) {
      stopManagedBridge();
      await act(async () => {});
      expect(h.handlers.get("mcp:agent-done")?.size ?? 0).toBe(0);
    }
    let managedDone = [...(h.handlers.get("mcp:agent-done") ?? [])][0];
    const dashboard = renderHook(() => useDashboardStream());
    await act(async () => {});
    const dashboardDone = [...h.handlers.get("mcp:agent-done")!].find((cb) => cb !== managedDone)!;
    if (dashboardFirst) {
      stopManagedBridge = renderHook(() => useManagedSessionBridge()).unmount;
      await act(async () => {});
      managedDone = [...h.handlers.get("mcp:agent-done")!].find((cb) => cb !== dashboardDone)!;
    }
    const order = dashboardFirst ? [dashboardDone, managedDone] : [managedDone, dashboardDone];
    expect(h.handlers.get("mcp:agent-done")?.size).toBe(2);
    expect([...h.handlers.get("mcp:agent-done")!]).toEqual(order);
    return { ...dashboard, expectDoneOrder: () => expect(h.doneCalls.slice(-2)).toEqual(order) };
  }

  it.each(
    [false, true].flatMap((dashboardFirst) =>
      [2, 3].map((generation) => ({ dashboardFirst, generation })),
    ),
  )(
    "rejects an older completion without regressing any result (generation=$generation, dashboardFirst=$dashboardFirst)",
    async ({ dashboardFirst, generation }) => {
      const dashboard = await mountDashboard(dashboardFirst);
      host({ revision: 4, turnGeneration: 2 });
      emit("mcp:agent-done", {
        sessionId: S,
        status: "failed",
        summary: "Earlier",
        activityCompletion: completionBoundary,
      });
      const newer = { ...completionBoundary, turnGeneration: generation, revision: 9 };
      emit("mcp:agent-done", {
        sessionId: S,
        status: "success",
        summary: "Latest",
        activityCompletion: newer,
      });
      dashboard.expectDoneOrder();
      const agent = useAgentStateStore.getState();
      const progress = useViewerStore.getState().progressBySession[S];
      const tick = useViewerStore.getState().progressTickBySession[S];
      act(() =>
        useManagedSessionStore.getState().addPendingAsk({
          correlationId: "later",
          sessionId: S,
          question: "Current?",
          options: [],
        }),
      );
      const managed = useManagedSessionStore.getState();
      emit("mcp:agent-done", {
        sessionId: S,
        status: "failed",
        summary: "Stale",
        activityCompletion: completionBoundary,
      });
      dashboard.expectDoneOrder();
      expect(useAgentStateStore.getState().completionActivityBySession[S]).toEqual(newer);
      expect(useAgentStateStore.getState().completionSeqBySession[S]).toBe(
        agent.completionSeqBySession[S],
      );
      expect(useManagedSessionStore.getState()).toBe(managed);
      expect(useViewerStore.getState().progressBySession[S]).toBe(progress);
      expect(useViewerStore.getState().progressTickBySession[S]).toBe(tick);
    },
  );

  it.each([false, true])(
    "admits newer, equal, metadata-free and unrelated completion boundaries (dashboardFirst=%s)",
    async (dashboardFirst) => {
      const dashboard = await mountDashboard(dashboardFirst);
      host({ revision: 4, turnGeneration: 2 });
      const boundaries = [
        completionBoundary,
        { ...completionBoundary, revision: 6 },
        { ...completionBoundary, revision: 6 },
        {
          ...completionBoundary,
          identity: { ...completionBoundary.identity, invocationId: "other" },
          revision: 1,
          turnGeneration: 1,
        },
        undefined,
        completionBoundary,
      ];
      for (const [index, activityCompletion] of boundaries.entries()) {
        emit("mcp:agent-done", {
          sessionId: S,
          status: "success",
          summary: `Result ${index}`,
          activityCompletion,
        });
        dashboard.expectDoneOrder();
        expect(useManagedSessionStore.getState().summaries[S]).toBe(`Result ${index}`);
        expect(useViewerStore.getState().progressBySession[S]?.note).toBe(`Result ${index}`);
      }
      const seq = useAgentStateStore.getState().completionSeqBySession[S];
      const progress = useViewerStore.getState().progressBySession[S];
      const tick = useViewerStore.getState().progressTickBySession[S];
      emit("mcp:agent-done", {
        sessionId: S,
        status: "success",
        summary: "Result 5",
        activityCompletion: boundaries.at(-1),
      });
      expect(useAgentStateStore.getState().completionSeqBySession[S]).toBe(seq);
      expect(useViewerStore.getState().progressBySession[S]).toBe(progress);
      expect(useViewerStore.getState().progressTickBySession[S]).toBe(tick);
      emit("mcp:agent-done", {
        sessionId: S,
        status: "success",
        activityCompletion: boundaries.at(-1),
      });
      expect(useManagedSessionStore.getState().summaries[S]).toBe("Result 5");
      expect(useViewerStore.getState().progressBySession[S]?.note).toBe("Result 5");
    },
  );

  it.each(
    terminalResults.flatMap(([result, outcome]) =>
      [false, true].map((dashboardFirst) => ({ result, outcome, dashboardFirst })),
    ),
  )(
    "$result gives a new turn its own progress lifetime (dashboardFirst=$dashboardFirst)",
    async ({ result, outcome, dashboardFirst }) => {
      const dashboard = await mountDashboard(dashboardFirst);
      const surfaces = renderSurfaces();
      host({ revision: 4, turnGeneration: 2 });
      emit("mcp:agent-done", {
        sessionId: S,
        status: result,
        summary: "Old result",
        activityCompletion: completionBoundary,
      });
      expect(useViewerStore.getState().progressBySession[S]?.settled).toBe(true);
      dashboard.expectDoneOrder();
      emit("mcp:agent-progress", { sessionId: S, phase: "blocked", note: "Trailing old tick" });
      expect(useViewerStore.getState().progressBySession[S]?.note).toBe("Old result");
      host({ revision: 6, turnGeneration: 3 });
      expect(useViewerStore.getState().progressBySession[S]).toBeUndefined();
      expect(useViewerStore.getState().progressTickBySession[S]).toBeUndefined();
      expect(activityLine(useViewerStore.getState().progressBySession[S])).toBeNull();
      expect(useManagedSessionStore.getState().summaries[S]).toBe("Old result");
      host({ revision: 7, turnGeneration: 3, state: "needs-permission", reason: "blocked" });
      emit("mcp:agent-progress", { sessionId: S, phase: "blocked", note: "New obstacle" });
      receiveMark("blocked");
      expectEveryMark(surfaces, "blocked");
      expect(blockedLine(useViewerStore.getState().progressBySession[S])).toBe(
        "Blocked: New obstacle",
      );
      host({ revision: 8, turnGeneration: 3 });
      emit("mcp:agent-progress", { sessionId: S, phase: "dev", note: "Continuing" });
      receiveMark("thinking");
      expectEveryMark(surfaces, "thinking");
      expect(activityLine(useViewerStore.getState().progressBySession[S])).toBe("Continuing");
      expect(useManagedSessionStore.getState().states[S]).toBe(outcome);
    },
  );

  it.each(
    terminalResults.flatMap(([result, outcome]) =>
      [false, true].map((dashboardFirst) => ({ result, outcome, dashboardFirst })),
    ),
  )(
    "$result delayed Done preserves newer progress (dashboardFirst=$dashboardFirst)",
    async ({ result, outcome, dashboardFirst }) => {
      const dashboard = await mountDashboard(dashboardFirst);
      const surfaces = renderSurfaces();
      host({ revision: 6, turnGeneration: 3 });
      emit("mcp:agent-progress", { sessionId: S, phase: "dev", note: "Current detail" });
      const progress = useViewerStore.getState().progressBySession[S];
      const tick = useViewerStore.getState().progressTickBySession[S];
      vi.spyOn(Date, "now").mockReturnValue((tick ?? Date.now()) + 1000);
      emit("mcp:agent-done", {
        sessionId: S,
        status: result,
        summary: "Old result",
        activityCompletion: completionBoundary,
      });
      expect(useViewerStore.getState().progressBySession[S]).toBe(progress);
      dashboard.expectDoneOrder();
      expect(useViewerStore.getState().progressTickBySession[S]).toBe(tick);
      expect(useManagedSessionStore.getState().summaries[S]).toBe("Old result");
      expect(useManagedSessionStore.getState().states[S]).toBe(outcome);
      host({ revision: 7, turnGeneration: 3, state: "needs-permission", asks: ["current"] });
      receiveMark("question");
      expectEveryMark(surfaces, "question");
      host({ revision: 8, turnGeneration: 3, state: "needs-permission" });
      receiveMark("permission");
      expectEveryMark(surfaces, "permission");
    },
  );

  it.each(
    terminalResults.flatMap(([result]) => [false, true].map((metadata) => ({ result, metadata }))),
  )(
    "$result without a retained summary rejects trailing progress (metadata=$metadata)",
    async ({ result, metadata }) => {
      await mountDashboard(false);
      host({ revision: 4, turnGeneration: 2 });
      emit("mcp:agent-done", {
        sessionId: S,
        status: result,
        ...(metadata ? { activityCompletion: completionBoundary } : {}),
      });
      host({ revision: 5, turnGeneration: 2, state: "done" });
      act(() => useAgentStateStore.getState().activityUnavailable([S], "recovering"));
      host({ revision: 5, turnGeneration: 2, state: "done" });
      emit("mcp:agent-progress", { sessionId: S, phase: "dev", note: "Trailing" });
      expect(useViewerStore.getState().progressBySession[S]).toBeUndefined();
      expect(useViewerStore.getState().progressTickBySession[S]).toBeUndefined();
    },
  );

  it("reconciles settled detail on mount and releases the subscription on unmount", async () => {
    host({ revision: 4, turnGeneration: 2 });
    emit("mcp:agent-done", {
      sessionId: S,
      status: "success",
      activityCompletion: completionBoundary,
    });
    host({ revision: 6, turnGeneration: 3 });
    act(() =>
      useViewerStore
        .getState()
        .setSessionProgress(S, { phase: "done", note: "Old", settled: true }),
    );
    const dashboard = await mountDashboard(false);
    expect(useViewerStore.getState().progressBySession[S]).toBeUndefined();
    expect(useViewerStore.getState().progressTickBySession[S]).toBeUndefined();
    dashboard.unmount();
    act(() =>
      useViewerStore
        .getState()
        .setSessionProgress(S, { phase: "done", note: "After unmount", settled: true }),
    );
    const progress = useViewerStore.getState().progressBySession[S];
    host({ revision: 7, turnGeneration: 3 });
    expect(useViewerStore.getState().progressBySession[S]).toBe(progress);
  });

  it("preserves a settled summary and its age on PTY cleanup", async () => {
    await mountDashboard(false);
    host({ revision: 4, turnGeneration: 2 });
    emit("mcp:agent-done", {
      sessionId: S,
      status: "success",
      summary: "Result",
      activityCompletion: completionBoundary,
    });
    const progress = useViewerStore.getState().progressBySession[S];
    const tick = useViewerStore.getState().progressTickBySession[S];
    emit("pty-exit", { session_id: S, exit_code: 0, signal: null, requested: false });
    expect(useViewerStore.getState().progressBySession[S]).toBe(progress);
    expect(useViewerStore.getState().progressTickBySession[S]).toBe(tick);
  });

  it.each(
    terminalResults.flatMap(([result, outcome, mark]) =>
      [false, true].flatMap((snapshotFirst) =>
        [false, true].map((metadata) => ({ result, outcome, mark, snapshotFirst, metadata })),
      ),
    ),
  )(
    "$result retires same-turn host asks (snapshotFirst=$snapshotFirst metadata=$metadata)",
    ({ result, outcome, mark, snapshotFirst, metadata }) => {
      const surfaces = renderSurfaces();
      const needs = () => {
        const agent = useAgentStateStore.getState();
        const managed = useManagedSessionStore.getState();
        return selectNeedsYouKind(
          {
            aiStatusBySession: {},
            agentDots: selectAgentDots(agent),
            crashes: agent,
            pendingAsks: managed.pendingAsks,
            outcomes: managed.states,
          },
          S,
        );
      };
      host({ revision: 4, turnGeneration: 2, state: "needs-permission", asks: ["old"] });
      emit("mcp:ask-user", { sessionId: S, correlationId: "old", question: "Old question?" });
      const doneSnapshot = () =>
        host({
          revision: 5,
          turnGeneration: 2,
          asks: ["old"],
          state:
            result === "success" ? "done" : result === "failed" ? "errored" : "needs-permission",
        });
      if (snapshotFirst) doneSnapshot();
      emit("mcp:agent-done", {
        sessionId: S,
        status: result,
        ...(metadata ? { activityCompletion: completionBoundary } : {}),
      });
      const expectedKind =
        result === "success" ? "permission" : result === "failed" ? "failed" : "attention";
      // An older NeedsPermission record may remain until the Done snapshot,
      // but it must no longer resurrect the retired answer controls.
      expect(needs()).not.toBe("question");
      receiveMark(mark);
      expectEveryMark(surfaces, mark);
      if (!snapshotFirst) doneSnapshot();
      expect(needs()).toBe(result === "success" ? null : expectedKind);
      receiveMark(mark);
      expectEveryMark(surfaces, mark);
      expect(useManagedSessionStore.getState().pendingAsks.old).toBeUndefined();
      host({
        revision: 4,
        turnGeneration: 2,
        state: "needs-permission",
        reason: "question",
        asks: ["old"],
      });
      expect(needs()).not.toBe("question");
      receiveMark(mark);
      expectEveryMark(surfaces, mark);
      // New owner turn proof releases only the display of the old result.
      host({ revision: 6, turnGeneration: 3 });
      const generation = snapshotFirst && !metadata ? 4 : 3;
      if (generation === 4) {
        // With no atomic completion proof the first later snapshot only
        // anchors recovery; a subsequent ordered prompt proves the new turn.
        receiveMark(mark);
        expectEveryMark(surfaces, mark);
        host({ revision: 7, turnGeneration: generation });
      }
      host({ revision: 8, turnGeneration: generation, state: "needs-permission", asks: ["new"] });
      expect(needs()).toBe("question");
      receiveMark("question");
      expectEveryMark(surfaces, "question");
      host({ revision: 9, turnGeneration: generation, state: "needs-permission", asks: [] });
      expect(needs()).toBe("permission");
      receiveMark("permission");
      expectEveryMark(surfaces, "permission");
      expect(useManagedSessionStore.getState().states[S]).toBe(outcome);
      // A genuinely new actionable UI ask always wins, even in legacy fallback.
      emit("mcp:ask-user", {
        sessionId: S,
        correlationId: "current-ui",
        question: "New question?",
      });
      expect(needs()).toBe("question");
      receiveMark("question");
      expectEveryMark(surfaces, "question");
    },
  );

  it.each(terminalResults)(
    "%s preserves an already newer host question when completion arrives late",
    (result, outcome) => {
      const surfaces = renderSurfaces();
      host({ revision: 6, turnGeneration: 3 });
      host({ revision: 7, turnGeneration: 3, state: "needs-permission", asks: ["new"] });
      emit("mcp:agent-done", {
        sessionId: S,
        status: result,
        activityCompletion: completionBoundary,
      });
      receiveMark("question");
      expectEveryMark(surfaces, "question");
      host({ revision: 8, turnGeneration: 3, state: "needs-permission", asks: [] });
      receiveMark("permission");
      expectEveryMark(surfaces, "permission");
      expect(useManagedSessionStore.getState().states[S]).toBe(outcome);
    },
  );

  it.each(terminalResults)(
    "%s shows the first proven new host turn, rejecting pre-completion generations and cleanup",
    (result, outcome, mark) => {
      const surfaces = renderSurfaces();
      host();
      emit("mcp:agent-done", {
        sessionId: S,
        status: result,
        activityCompletion: completionBoundary,
      });
      receiveMark(mark);
      expectEveryMark(surfaces, mark);
      host({ revision: 2, turnGeneration: 1 });
      // Even after anchoring an old generation, a delayed pre-completion gen2 is not new.
      host({ revision: 3, turnGeneration: 2 });
      host({ revision: 4, turnGeneration: 2, state: "working", toolOpen: true });
      host({ revision: 5, turnGeneration: 2, state: "done" });
      host({ revision: 6, turnGeneration: 2, state: "working", toolOpen: true });
      receiveMark(mark);
      expectEveryMark(surfaces, mark);
      host({ revision: 7, turnGeneration: 3 });
      receiveMark("thinking");
      expectEveryMark(surfaces, "thinking");
      expect(useManagedSessionStore.getState().states[S]).toBe(outcome);
      const activityAt = useAgentStateStore.getState().bySession[S].activityAt;
      host({ revision: 7, turnGeneration: 3, activityAgeMs: 0 });
      expect(useAgentStateStore.getState().bySession[S].activityAt).toBe(activityAt);
      host({ revision: 8, turnGeneration: 3, state: "needs-permission", asks: ["current"] });
      receiveMark("question");
      expectEveryMark(surfaces, "question");
      host({ revision: 9, turnGeneration: 3, state: "needs-permission" });
      receiveMark("permission");
      expectEveryMark(surfaces, "permission");
      host({ revision: 10, turnGeneration: 3, state: "needs-permission", reason: "blocked" });
      receiveMark("blocked");
      expectEveryMark(surfaces, "blocked");
      host({ revision: 11, turnGeneration: 3, state: "done" });
      receiveMark("ready");
      expectEveryMark(surfaces, "ready");
      host({
        revision: 12,
        turnGeneration: 3,
        state: "done",
        exit: { code: 0, signal: null, requested: false },
      });
      receiveMark("ended");
      expectEveryMark(surfaces, "ended");
      expect(useManagedSessionStore.getState().states[S]).toBe(outcome);
    },
  );

  it.each(
    terminalResults.flatMap(([result, outcome]) =>
      ["thinking", "done", "question", "permission", "blocked"].map((latest) => ({
        result,
        outcome,
        latest,
      })),
    ),
  )(
    "$result cannot hide an already observed later turn now $latest",
    ({ result, outcome, latest }) => {
      const surfaces = renderSurfaces();
      host({ revision: 6, turnGeneration: 3 });
      const state =
        latest === "question" || latest === "permission" || latest === "blocked"
          ? "needs-permission"
          : (latest as "thinking" | "done");
      host({
        revision: 7,
        turnGeneration: 3,
        state,
        reason: latest === "blocked" ? "blocked" : undefined,
      });
      if (latest === "question")
        emit("mcp:ask-user", { sessionId: S, correlationId: "new-ask", question: "Which target?" });
      const activityAt = useAgentStateStore.getState().bySession[S].activityAt;
      emit("mcp:agent-done", {
        sessionId: S,
        status: result,
        activityCompletion: completionBoundary,
      });
      receiveMark(latest === "done" ? "ready" : latest);
      expectEveryMark(surfaces, latest === "done" ? "ready" : latest);
      expect(useManagedSessionStore.getState().states[S]).toBe(outcome);
      expect(useAgentStateStore.getState().bySession[S].activityAt).toBe(activityAt);
      if (latest === "question") {
        expect(useManagedSessionStore.getState().pendingAsks["new-ask"]?.question).toBe(
          "Which target?",
        );
        emit("mcp:ask-user-closed", {
          sessionId: S,
          correlationId: "new-ask",
          reason: "cancelled",
        });
        receiveMark("permission");
        expectEveryMark(surfaces, "permission");
      }
    },
  );

  it.each(["recovering", "unavailable"] as const)(
    "retains owner ordering proof through %s without freshening replay",
    (mode) => {
      const surfaces = renderSurfaces();
      host({ revision: 5, turnGeneration: 2, state: "done" });
      emit("mcp:agent-done", {
        sessionId: S,
        status: "success",
        activityCompletion: completionBoundary,
      });
      act(() => useAgentStateStore.getState().activityUnavailable([S], mode));
      host({ revision: 5, turnGeneration: 2, state: "done" });
      receiveMark("ended");
      expectEveryMark(surfaces, "ended");
      host({ revision: 6, turnGeneration: 3 });
      receiveMark("thinking");
      expectEveryMark(surfaces, "thinking");
      expect(useManagedSessionStore.getState().states[S]).toBe("done");
    },
  );

  it("does not treat a replacement identity or a SessionEnd-only generation increment as proof", () => {
    const surfaces = renderSurfaces();
    host({ revision: 4, turnGeneration: 2 });
    host({ revision: 6, turnGeneration: 3, state: "idle" });
    emit("mcp:agent-done", {
      sessionId: S,
      status: "success",
      activityCompletion: completionBoundary,
    });
    receiveMark("ended");
    expectEveryMark(surfaces, "ended");
    host({
      identity: { ...completionBoundary.identity, invocationId: "other" },
      revision: 50,
      turnGeneration: 10,
    });
    receiveMark("ended");
    expectEveryMark(surfaces, "ended");
  });

  it.each([
    ["quiet default", false, "needs-attention", "question"],
    ["quiet single turn", true, "done", "ended"],
    ["session death", false, "needs-attention", "question"],
  ] as const)(
    "%s completion follows a later explicit turn while retaining its outcome",
    (producer, _singleTurn, outcome, mark) => {
      vi.mocked(Date.now).mockRestore();
      vi.useFakeTimers();
      vi.setSystemTime(1_000_000);
      const surfaces = renderSurfaces();
      act(() => useManagedSessionStore.getState().markRunning(S));
      hook("UserPromptSubmit", "running");
      if (producer === "session death") {
        emit("pty-exit", { session_id: S, exit_code: 0, signal: null, requested: false });
        act(() => {
          useAgentStateStore.getState().reset(S);
          useManagedSessionStore.setState((s) => ({ states: { ...s.states, [S]: outcome } }));
        });
      } else {
        hook("Stop", "done");
        act(() => {
          vi.advanceTimersByTime(1);
          useManagedSessionStore.setState((s) => ({ states: { ...s.states, [S]: outcome } }));
        });
      }
      receiveMark(mark);
      expectEveryMark(surfaces, mark);
      hook("UserPromptSubmit", "running");
      receiveMark("thinking");
      expectEveryMark(surfaces, "thinking");
      hook("PreToolUse", "running");
      receiveMark("tool");
      expectEveryMark(surfaces, "tool");
      hook("PermissionRequest", "needs-permission");
      receiveMark("permission");
      expectEveryMark(surfaces, "permission");
      hook("Stop", "done");
      receiveMark("ready");
      expectEveryMark(surfaces, "ready");
      expect(useManagedSessionStore.getState().states[S]).toBe(outcome);
    },
  );

  it.each(terminalResults)(
    "%s stays historical through delayed first snapshots and escapes on a later ordered host turn",
    (result, outcome, mark) => {
      const surfaces = renderSurfaces();
      host();
      emit("mcp:agent-done", { sessionId: S, status: result });
      receiveMark(mark);
      expectEveryMark(surfaces, mark);
      // An equal-revision reread does not establish a post-completion anchor.
      host();
      // Generation 2 may have been captured before completion and delivered late.
      host({ revision: 2, turnGeneration: 2 });
      receiveMark(mark);
      expectEveryMark(surfaces, mark);
      host({ revision: 2, turnGeneration: 2 });
      host({ revision: 3, turnGeneration: 2, state: "working", toolOpen: true });
      receiveMark(mark);
      expectEveryMark(surfaces, mark);
      host({ revision: 4, turnGeneration: 3 });
      receiveMark("thinking");
      expectEveryMark(surfaces, "thinking");
      expect(useManagedSessionStore.getState().states[S]).toBe(outcome);
      host({ revision: 5, turnGeneration: 3, state: "working", toolOpen: true });
      receiveMark("tool");
      expectEveryMark(surfaces, "tool");
      host({ revision: 6, turnGeneration: 3, state: "done", toolOpen: false });
      receiveMark("ready");
      expectEveryMark(surfaces, "ready");
      host({
        revision: 7,
        turnGeneration: 3,
        state: "done",
        exit: { code: 0, signal: null, requested: false },
      });
      receiveMark("ended");
      expectEveryMark(surfaces, "ended");
      expect(useManagedSessionStore.getState().states[S]).toBe(outcome);
    },
  );

  it.each(terminalResults)(
    "%s requires a fresh host anchor after recovery or reset",
    (result, outcome, mark) => {
      const surfaces = renderSurfaces();
      host();
      emit("mcp:agent-done", { sessionId: S, status: result });
      host({ revision: 2 }); // Connected post-completion anchor.
      act(() => useAgentStateStore.getState().activityUnavailable([S], "recovering"));
      host({ revision: 2 }); // A replay cannot reestablish that anchor.
      host({ revision: 3, turnGeneration: 2 }); // Recovery must only reanchor.
      receiveMark(mark);
      expectEveryMark(surfaces, mark);
      host({ revision: 4, turnGeneration: 2 });
      receiveMark(mark);
      expectEveryMark(surfaces, mark);
      act(() => useAgentStateStore.getState().reset(S));
      const identity = { ...hostSnapshot().identity, invocationId: "replacement" };
      host({ identity, turnGeneration: 9 });
      receiveMark(mark);
      expectEveryMark(surfaces, mark);
      host({ identity, revision: 2, turnGeneration: 9 });
      receiveMark(mark);
      expectEveryMark(surfaces, mark);
      host({ identity, revision: 3, turnGeneration: 10 });
      receiveMark("thinking");
      expectEveryMark(surfaces, "thinking");
      expect(useManagedSessionStore.getState().states[S]).toBe(outcome);
    },
  );

  it.each(terminalResults)(
    "%s retains uncertainty after reset and first tool, then follows an explicit legacy prompt and live holds",
    (result, outcome, mark) => {
      const surfaces = renderSurfaces();
      hook("UserPromptSubmit", "running");
      emit("mcp:agent-done", { sessionId: S, status: result });
      act(() => useAgentStateStore.getState().reset(S));
      hook("PreToolUse", "running");
      receiveMark(mark);
      expectEveryMark(surfaces, mark);
      hook("PostToolUse", "running");
      hook("PreToolUse", "running");
      receiveMark(mark);
      expectEveryMark(surfaces, mark);
      hook("UserPromptSubmit", "running");
      receiveMark("thinking");
      expectEveryMark(surfaces, "thinking");
      hook("PermissionRequest", "needs-permission");
      receiveMark("permission");
      expectEveryMark(surfaces, "permission");
      hook("PostToolUse", "running");
      emit("terminal-status-update", {
        tab_session_id: S,
        status: "needs-permission",
        source: "mcp",
        agent_signal: { kind: "signal-progress", phase: "blocked" },
      });
      receiveMark("blocked");
      expectEveryMark(surfaces, "blocked");
      hook("UserPromptSubmit", "running");
      receiveMark("thinking");
      expectEveryMark(surfaces, "thinking");
      expect(useManagedSessionStore.getState().states[S]).toBe(outcome);
    },
  );

  it.each(terminalResults)(
    "%s never hides a genuine host question, permission or block on the escaped turn",
    (result, outcome) => {
      const surfaces = renderSurfaces();
      host();
      emit("mcp:agent-done", { sessionId: S, status: result });
      host({ revision: 2 });
      host({ revision: 3, turnGeneration: 2 });
      receiveMark("thinking");
      expectEveryMark(surfaces, "thinking");
      host({ revision: 4, turnGeneration: 2, state: "needs-permission", asks: ["held"] });
      receiveMark("question");
      expectEveryMark(surfaces, "question");
      host({ revision: 5, turnGeneration: 2, state: "needs-permission" });
      receiveMark("permission");
      expectEveryMark(surfaces, "permission");
      host({ revision: 6, turnGeneration: 2, state: "needs-permission", reason: "blocked" });
      receiveMark("blocked");
      expectEveryMark(surfaces, "blocked");
      expect(useManagedSessionStore.getState().states[S]).toBe(outcome);
    },
  );

  it("retains headless output freshness before the first provider record without inventing lifecycle", () => {
    vi.mocked(Date.now).mockRestore();
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
    const surfaces = renderSurfaces();
    act(() => useManagedSessionStore.getState().markRunning(S));
    act(() =>
      useAgentListProjectionStore
        .getState()
        .setSessions([{ ...listRow, lastOutputMs: Date.now() }]),
    );
    expect(useAgentStateStore.getState().bySession[S]).toBeUndefined();
    receiveMark("thinking");
    expectEveryMark(surfaces, "thinking");
    act(() => vi.advanceTimersByTime(600_000));
    receiveMark("stalled");
    expectEveryMark(surfaces, "stalled");
    act(() =>
      useAgentListProjectionStore
        .getState()
        .setSessions([{ ...listRow, lastOutputMs: Date.now() }]),
    );
    receiveMark("thinking");
    expectEveryMark(surfaces, "thinking");
    expect(useAgentStateStore.getState().bySession[S]).toBeUndefined();
  });

  it("cold hydration of an aged open turn is Stalled across surfaces, not fresh work", () => {
    vi.mocked(Date.now).mockReturnValue(2_000_000);
    const { list, rollup } = renderSurfaces();
    act(() =>
      useAgentStateStore.getState().recordActivity(
        {
          session: S,
          identity: { hostBootId: "boot", sessionIncarnation: "pty", invocationId: "run" },
          revision: 1,
          turnGeneration: 1,
          provider: "claude",
          state: "working",
          observed: true,
          limited: false,
          exit: null,
          asks: [],
          toolOpen: true,
          activityAgeMs: 700_000,
        },
        agentInputSeq(),
      ),
    );
    receiveStatus("stalled");
    expect(list.result.current(listRow)).toBe("stalled");
    expect(rollup.result.current.mark).toBe("stalled");
    for (const id of [
      "tab-p1-ai-status",
      `workspace-tab-${WS}-soft-closed-dot`,
      "nav-story-agent-mark",
    ]) {
      expect(screen.getByTestId(id)).toHaveAttribute("data-mark", "stalled");
    }
  });

  it("machine disconnect/reconnect updates the tab, pill and nav mark without changing the agent", () => {
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
      { displayName: WS, kind: "test", org: "acme", team: "default" },
      { session: "remote-agent", tags: { remoteHandle: "agent" } },
    );
    const remoteTab = selectWorkspaceTabs(useViewerStore.getState(), TEAM, WS).find(
      (t) => t.origin?.machineId === "box",
    )!;
    expect(remoteTab).toBeDefined();
    hook("UserPromptSubmit", "running", remoteTab.sessionId);
    host({ session: remoteTab.sessionId });
    receiveStatus("thinking", remoteTab.sessionId);
    renderSurfaces(remoteTab.sessionId);
    render(
      <WorkspacePill
        workspace={workspace}
        isActive={false}
        markScope={{ teamKey: TEAM, wsIds: [WS] }}
        isStatusPopoverOpen={false}
        onStatusPopoverOpenChange={vi.fn()}
        onSelect={vi.fn()}
        onClose={vi.fn()}
      />,
    );
    const expectMarks = (mark: string) => {
      expect(screen.getByTestId("tab-p1-ai-status")).toHaveAttribute("data-mark", mark);
      expect(screen.getByTestId(`workspace-tab-${WS}-soft-closed-dot`)).toHaveAttribute(
        "data-mark",
        mark,
      );
      expect(screen.getByTestId("nav-story-agent-mark")).toHaveAttribute("data-mark", mark);
    };
    expectMarks("thinking");
    act(() =>
      useMachinesStore.setState({
        byTeam: { [TEAM]: [{ ...machine, status: { state: "disconnected" } }] },
      }),
    );
    expectMarks("offline");
    for (const freshness of ["recovering", "unavailable"] as const) {
      act(() =>
        useAgentStateStore.getState().activityUnavailable([remoteTab.sessionId], freshness),
      );
      expectMarks("offline");
    }
    expect(screen.getByTestId(`workspace-tab-${WS}-ai-status-dot`)).toHaveAttribute(
      "data-mark",
      "offline",
    );
    expect(screen.getByTestId("tab-p1-ai-status")).toHaveAccessibleName(/Machine offline/);
    act(() => useMachinesStore.setState({ byTeam: { [TEAM]: [machine] } }));
    host({ session: remoteTab.sessionId });
    receiveStatus("thinking", remoteTab.sessionId);
    expectMarks("thinking");
  });
  it("a real hook remains Thinking at 3m, stalls at 10m, refreshes on same-state activity, and Stop stays Ready", () => {
    vi.mocked(Date.now).mockRestore();
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
    const { list, rollup } = renderSurfaces();
    const expectMarks = (mark: string) => {
      expect(markOfAgentStatus(list.result.current(listRow))).toBe(mark);
      expect(screen.getByTestId("tab-p1-ai-status")).toHaveAttribute("data-mark", mark);
      expect(screen.getByTestId(`workspace-tab-${WS}-soft-closed-dot`)).toHaveAttribute(
        "data-mark",
        mark,
      );
      expect(screen.getByTestId("nav-story-agent-mark")).toHaveAttribute("data-mark", mark);
      expect(rollup.result.current.mark).toBe(mark);
    };
    hook("UserPromptSubmit", "running");
    act(() => vi.advanceTimersByTime(3 * 60_000));
    receiveMark("thinking");
    expectMarks("thinking");
    act(() => vi.advanceTimersByTime(7 * 60_000 - 1));
    receiveMark("thinking");
    expectMarks("thinking");
    act(() => vi.advanceTimersByTime(1));
    receiveMark("stalled");
    expectMarks("stalled");
    const agent = useAgentStateStore.getState();
    expect(
      selectSessionMark(useViewerStore.getState(), selectAgentDots(agent), agent, { sessionId: S })
        ?.since,
    ).toBe(1_600_000);
    hook("PostToolUse", "running");
    receiveMark("thinking");
    expectMarks("thinking");
    act(() => vi.advanceTimersByTime(9 * 60_000));
    act(() => useAgentStateStore.getState().recordOutput(S));
    act(() => vi.advanceTimersByTime(2 * 60_000));
    receiveMark("thinking");
    expectMarks("thinking");
    act(() => vi.advanceTimersByTime(8 * 60_000));
    receiveMark("stalled");
    expectMarks("stalled");
    act(() =>
      useAgentListProjectionStore
        .getState()
        .setSessions([{ ...listRow, lastOutputMs: Date.now() }]),
    );
    receiveMark("thinking");
    expectMarks("thinking");
    hook("Stop", "done");
    act(() => vi.advanceTimersByTime(11 * 60_000));
    receiveStatus("idle");
    expectIdleEverywhere(list, rollup);
  });

  it("an agent exit is Ended on every surface while its containing shell stays alive", () => {
    const { list, rollup } = renderSurfaces();
    hook("Stop", "done");
    receiveStatus("idle");
    expectIdleEverywhere(list, rollup);
    act(() =>
      useAgentStateStore
        .getState()
        .recordProcess(S, { kind: "exited", code: 0, signal: null }, agentInputSeq()),
    );
    receiveStatus("done");
    expect(markOfAgentStatus(list.result.current(listRow))).toBe("ended");
    expect(screen.getByTestId("tab-p1-ai-status")).toHaveAttribute("data-mark", "ended");
    expect(screen.getByTestId(`workspace-tab-${WS}-soft-closed-dot`)).toHaveAttribute(
      "data-mark",
      "ended",
    );
    expect(screen.getByTestId("nav-story-agent-mark")).toHaveAttribute("data-mark", "ended");
    expect(rollup.result.current.mark).toBe("ended");
  });

  it("dismissing an exited agent crash removes the row from Needs you", () => {
    const surfaces = renderSurfaces();
    const { list } = surfaces;
    hook("UserPromptSubmit", "running");
    act(() =>
      useAgentStateStore.getState().recordProcess(S, { kind: "exited", code: 1 }, agentInputSeq()),
    );
    receiveStatus("crashed");
    expect(list.result.current(listRow)).toBe("crashed");
    const seq = agentCrashSeq(useAgentStateStore.getState().bySession[S]);
    expect(seq).not.toBeNull();
    receiveStatus("done");
    receiveStatus("done");
    expect(list.result.current(listRow)).toBe("done");
    expect(isNeedsYouStatus(list.result.current(listRow))).toBe(false);
    receiveMark("ended");
    expectEveryMark(surfaces, "ended");
  });

  it("timestamps fine transitions without resetting them on same-state activity", () => {
    const signal = (phase: "blocked" | "dev") =>
      emit("terminal-status-update", {
        tab_session_id: S,
        status: phase === "blocked" ? "needs-permission" : "running",
        source: "mcp",
        agent_signal: { kind: "signal-progress", phase },
      });
    const since = () => projectedSession(S)?.statusSinceMs ?? 0;
    signal("blocked");
    receiveStatus("blocked");
    const blockedAt = since();
    hook("PermissionRequest", "needs-permission");
    receiveStatus("paused");
    expect(since()).toBeGreaterThan(blockedAt);
    signal("dev");
    receiveStatus("thinking");
    const thinkingAt = since();
    hook("PreToolUse", "running");
    receiveStatus("tool-running");
    expect(since()).toBeGreaterThan(thinkingAt);
    const toolAt = since();
    signal("dev");
    act(() => useAgentStateStore.getState().recordOutput(S, Date.now() + 2_000));
    expect(since()).toBe(toolAt);
  });

  it("host blocked, permission, generic progress and tool snapshots agree on tab, pill, list and nav", () => {
    const { list, rollup } = renderSurfaces();
    let revision = 0;
    const snapshot = (state: ActivitySnapshot["state"], reason: string | null, toolOpen: boolean) =>
      act(() => {
        useAgentStateStore.getState().recordActivity(
          {
            session: S,
            identity: { hostBootId: "boot", sessionIncarnation: "pty", invocationId: "run" },
            revision: ++revision,
            turnGeneration: 1,
            provider: "claude",
            observed: true,
            limited: false,
            state,
            reason,
            toolOpen,
            activityAgeMs: 0,
            asks: [],
            exit: null,
          },
          agentInputSeq(),
        );
      });
    for (const [state, reason, mark, words, toolOpen] of [
      ["needs-permission", "blocked", "blocked", "Blocked", false],
      ["needs-permission", null, "permission", "Needs approval", false],
      ["working", "progress", "thinking", "Thinking", false],
      ["working", null, "tool", "Running a tool", true],
    ] as const) {
      snapshot(state, reason, toolOpen);
      receiveMark(mark);
      expect(markOfAgentStatus(list.result.current(listRow))).toBe(mark);
      expect(screen.getByTestId("tab-p1-ai-status")).toHaveAttribute("data-mark", mark);
      expect(screen.getByTestId(`workspace-tab-${WS}-soft-closed-dot`)).toHaveAttribute(
        "data-mark",
        mark,
      );
      expect(screen.getByTestId("nav-story-agent-mark")).toHaveAttribute("data-mark", mark);
      expect(rollup.result.current.mark).toBe(mark);
      expect(rollup.result.current.words).toBe(words);
    }
  });
  it("list row, tab, pill and nav story row show Idle at once, unseen, and still once seen", () => {
    const { list, rollup } = renderSurfaces();
    hook("UserPromptSubmit", "running");
    // a UserPromptSubmit hook sets agent-state `thinking` (the finer
    // state the agent record carries); the shared mark reads as `thinking` —
    // the breathing dots, the model is reasoning without a tool open.
    receiveStatus("thinking");
    expect(screen.getByTestId("tab-p1-ai-status")).toHaveAttribute("data-mark", "thinking");
    expect(rollup.result.current.mark).toBe("thinking");

    hook("Stop", "done");
    receiveStatus("idle");
    expectIdleEverywhere(list, rollup);

    act(() => markSessionSeen(S));
    receiveStatus("idle");
    expectIdleEverywhere(list, rollup);
  });

  it("its roll-up ranks as idle: below another agent's work, and quiet", () => {
    const { rollup } = renderSurfaces();
    hook("UserPromptSubmit", "running");
    hook("Stop", "done");
    hook("UserPromptSubmit", "running", BUSY);
    receiveStatus("thinking", BUSY);
    expect(rollup.result.current.mark).toBe("thinking");
    expect(screen.getByTestId("nav-story-agent-mark")).toHaveAttribute("data-mark", "thinking");
    expect(topMark(["ready", "thinking"])).toBe("thinking");

    hook("Stop", "done", BUSY);
    receiveStatus("idle", BUSY);
    receiveStatus("idle");
    expect(rollup.result.current.mark).toBe("ready");
    expect(screen.getByTestId("nav-story-agent-mark")).toHaveAttribute("data-mark", "ready");
  });
});
