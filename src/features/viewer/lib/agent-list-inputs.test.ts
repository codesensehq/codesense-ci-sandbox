// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ViewerState, Workspace } from "@/shared/stores/viewer-store";
const transport = vi.hoisted(() => ({
  read: vi.fn(),
  write: vi.fn(),
  mirror: vi.fn(),
  listen: vi.fn(),
}));
const ui = vi.hoisted(() => ({ read: vi.fn(), write: vi.fn(), watch: vi.fn() }));
vi.mock("@/tauri/commands", () => ({
  agentListIdentity: transport.read,
  agentListSetIdentity: transport.write,
  pushViewerState: transport.mirror,
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: transport.listen }));
vi.mock("@/features/dashboard/lib/live-hub", () => ({
  liveHub: { snapshot: ui.read, writeUi: ui.write, watch: ui.watch },
}));
vi.mock("@/shared/stores/viewer-store", async () => {
  const { create } = await import("zustand");
  return {
    useViewerStore: create(() => ({})),
    // A viewer field the agent list's own push never listed.
    viewerMirrorFields: () => ({ workspaceTraySeenCoachmark: true }),
    selectFocusedSession: (s: { focused?: Record<string, string> }, teamKey: string) =>
      s.focused?.[teamKey],
  };
});
vi.mock("@/shared/stores/org-store", async () => {
  const { create } = await import("zustand");
  return { useOrgStore: create(() => ({ orgs: [] as unknown[] })) };
});
const { useViewerStore } = await import("@/shared/stores/viewer-store");
const { useOrgStore } = await import("@/shared/stores/org-store");
const { watchAgentListInputs } = await import("./agent-list-inputs");
beforeEach(() => {
  vi.clearAllMocks();
  transport.read.mockResolvedValue({});
  transport.write.mockResolvedValue({});
  transport.mirror.mockResolvedValue(undefined);
  transport.listen.mockResolvedValue(() => {});
  ui.watch.mockReturnValue(() => {});
  ui.read.mockResolvedValue({
    topic: "ui:dock",
    viewKey: "all",
    rev: 0,
    snapshot: [{ id: "ui:dock", arrange: "teams", pins: {} }],
  });
  ui.write.mockResolvedValue({});
  useViewerStore.setState({
    stateLoaded: false,
    openedTeams: [],
    workspaces: {},
    softClosedWorkspaces: {},
    workspaceOrderByTeam: {},
    trayBaseOrderByTeam: {},
    railKeyByWorkspaceId: {},
    agentIdentity: {},
    agentPins: {},
    agentsPaneArrange: "teams",
    agentsPaneFolds: {},
  } as Partial<ViewerState>);
  useOrgStore.setState({ orgs: [], activeOrgSlug: null, activeTeamName: null } as never);
});
describe("agentList input ownership", () => {
  it("waits for restoration, then flushes workspace and shared-order actions immediately", async () => {
    const stop = watchAgentListInputs();
    try {
      expect(transport.mirror).not.toHaveBeenCalled();
      useViewerStore.setState({ stateLoaded: true, openedTeams: ["org:team"] });
      await vi.waitFor(() => expect(transport.read).toHaveBeenCalledTimes(1));
      transport.mirror.mockClear();
      useViewerStore.setState({
        workspaces: {
          "org:team": [
            { id: "ws", kind: "generic", status: "ready", cwd: "/wt/work" } as Workspace,
          ],
        },
      });
      expect(transport.mirror).toHaveBeenCalledTimes(1);
      expect(transport.mirror.mock.lastCall?.[0].resolvedWorkspaceOrderByTeam["org:team"]).toEqual([
        "ws",
      ]);
      // A push replaces the whole native mirror, so it carries the viewer's fields too.
      expect(transport.mirror.mock.lastCall?.[0].workspaceTraySeenCoachmark).toBe(true);
      useViewerStore.setState({ activeTab: { "org:team": "doc" } });
      expect(transport.mirror).toHaveBeenCalledTimes(1);
      useViewerStore.setState({ workspaceOrderByTeam: { "org:team": ["ws"] } });
      expect(transport.mirror).toHaveBeenCalledTimes(2);
    } finally {
      stop();
    }
  });
  it("migrates identity to the Rust owner and does not echo its reply as another write", async () => {
    const legacy = { "org:team": { worker: { manager: "lead" } } };
    transport.read.mockResolvedValue(legacy);
    useViewerStore.setState({ stateLoaded: true, agentIdentity: legacy });
    const stop = watchAgentListInputs();
    try {
      await vi.waitFor(() => expect(transport.read).toHaveBeenCalledWith(legacy));
      expect(transport.write).not.toHaveBeenCalled();
      useViewerStore.setState({ agentIdentity: { "org:team": { worker: { manager: "other" } } } });
      await vi.waitFor(() =>
        expect(transport.write).toHaveBeenCalledWith("org:team", "worker", { manager: "other" }),
      );
      expect(transport.write).toHaveBeenCalledTimes(1);
    } finally {
      stop();
    }
  });
  it("writes arrangement and pins to the shared dock state", async () => {
    useViewerStore.setState({ stateLoaded: true });
    const stop = watchAgentListInputs();
    try {
      await ui.read.mock.results[0]?.value;
      ui.write.mockClear();
      useViewerStore.setState({ agentsPaneArrange: "manager" });
      expect(ui.write).toHaveBeenCalledWith(
        "ui:dock",
        "all",
        expect.objectContaining({
          arrange: "manager",
          pins: {},
          folds: {},
        }),
      );
    } finally {
      stop();
    }
  });
  it("shares the dock's node folds both ways: the built-in list and a dock dashboard", async () => {
    let onDock: (snapshot: { snapshot: Array<Record<string, unknown>> }) => void = () => {};
    ui.watch.mockImplementation((_topic, _key, cb) => {
      onDock = cb;
      return () => {};
    });
    useViewerStore.setState({ stateLoaded: true });
    const stop = watchAgentListInputs();
    try {
      await ui.read.mock.results[0]?.value;
      // The saved state predates folds: the host fills them in.
      await vi.waitFor(() =>
        expect(ui.write).toHaveBeenCalledWith(
          "ui:dock",
          "all",
          expect.objectContaining({ folds: {} }),
        ),
      );
      ui.write.mockClear();
      // Folding a nested manager in the built-in list writes its node key.
      const manager = "#manager-fold:acme:a|lead/worker";
      useViewerStore.setState({ agentsPaneFolds: { [manager]: true } });
      expect(ui.write).toHaveBeenCalledWith(
        "ui:dock",
        "all",
        expect.objectContaining({ folds: { [manager]: true } }),
      );
      // A dashboard's fold reaches the built-in list without being written back.
      ui.write.mockClear();
      const team = "#team-fold:acme:b";
      onDock({
        snapshot: [{ id: "ui:dock", arrange: "teams", pins: {}, folds: { [team]: true } }],
      });
      expect(useViewerStore.getState().agentsPaneFolds).toEqual({ [team]: true });
      expect(ui.write).not.toHaveBeenCalled();
    } finally {
      stop();
    }
  });
  it("mirrors the CLI's focused session into ui:dock.selected", async () => {
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "a" } as never);
    useViewerStore.setState({ stateLoaded: true, focused: { "acme:a": "sid-1" } } as never);
    const stop = watchAgentListInputs();
    try {
      await ui.read.mock.results[0]?.value;
      useViewerStore.setState({ focused: { "acme:a": "sid-2" } } as never);
      expect(ui.write).toHaveBeenCalledWith("ui:dock", "all", { selected: "sid-2" });
      ui.write.mockClear();
      // Unrelated changes do not rewrite it.
      useViewerStore.setState({ activeTab: {} } as never);
      expect(ui.write).not.toHaveBeenCalledWith("ui:dock", "all", { selected: "sid-2" });
      // A team switch follows that team's CLI focus.
      useViewerStore.setState({ focused: { "acme:a": "sid-2", "acme:b": "sid-9" } } as never);
      ui.write.mockClear();
      useOrgStore.setState({ activeTeamName: "b" } as never);
      expect(ui.write).toHaveBeenCalledWith("ui:dock", "all", { selected: "sid-9" });
    } finally {
      stop();
    }
  });
});

it("mirrors pending question text and model changes at the host seam", async () => {
  const { useManagedSessionStore } = await import("@/shared/stores/use-managed-session-store");
  const { useAgentStateStore } = await import("@/shared/stores/agent-state-store");
  useViewerStore.setState({ stateLoaded: true });
  const stop = watchAgentListInputs();
  try {
    useManagedSessionStore
      .getState()
      .addPendingAsk({ correlationId: "ask-1", sessionId: "s1", question: "Which branch?" });
    await vi.waitFor(() =>
      expect(transport.mirror).toHaveBeenLastCalledWith(
        expect.objectContaining({ agentPendingAsks: { s1: "Which branch?" } }),
      ),
    );
    useAgentStateStore.setState({ bySession: { s1: { agent: "claude" } } } as never);
    await vi.waitFor(() =>
      expect(transport.mirror).toHaveBeenLastCalledWith(
        expect.objectContaining({ agentSessionModels: { s1: expect.any(String) } }),
      ),
    );
    useManagedSessionStore.getState().resolvePendingAsk("ask-1");
    await vi.waitFor(() =>
      expect(transport.mirror).toHaveBeenLastCalledWith(
        expect.objectContaining({ agentPendingAsks: {} }),
      ),
    );
  } finally {
    stop();
  }
});

it("mirrors live taxonomy roles for pane stage rails", async () => {
  const { useStatusTaxonomyStore } = await import("@/shared/stores/status-taxonomy-store");
  useViewerStore.setState({ stateLoaded: true });
  const stop = watchAgentListInputs();
  try {
    const states = [
      { id: "checking", label: "Checking", role: "review", color: "#888", icon: "Eye", order: 0 },
    ] as const;
    useStatusTaxonomyStore.getState().setStates([...states]);
    expect(transport.mirror).toHaveBeenLastCalledWith(
      expect.objectContaining({ agentStageStates: states }),
    );
  } finally {
    stop();
    useStatusTaxonomyStore.getState().resetForTest();
  }
});

it("shares search, hidden teams and both chips without echoing pane writes", async () => {
  let onDock: (value: { snapshot: Record<string, unknown>[] }) => void = () => {};
  ui.watch.mockImplementation((_topic, _key, cb) => {
    onDock = cb;
    return () => {};
  });
  useViewerStore.setState({ stateLoaded: true });
  const stop = watchAgentListInputs();
  try {
    await ui.read.mock.results[0]?.value;
    ui.write.mockClear();
    useViewerStore.setState({
      agentsPaneHiddenTeams: ["acme:b"],
      agentsPaneSearch: { query: "lead", status: "needs" },
    });
    expect(ui.write).toHaveBeenLastCalledWith(
      "ui:dock",
      "all",
      expect.objectContaining({
        hiddenTeams: ["acme:b"],
        search: { query: "lead", status: "needs" },
      }),
    );
    ui.write.mockClear();
    onDock({
      snapshot: [
        {
          id: "ui:dock",
          arrange: "teams",
          pins: {},
          hiddenTeams: [],
          search: { query: "worker", status: "working" },
          workspaceFilter: { teamKey: "acme:a", workspaceId: "ws" },
          agentFilter: { agentKey: "a", name: "Worker" },
          teamFocus: null,
        },
      ],
    });
    expect(useViewerStore.getState()).toMatchObject({
      agentsPaneHiddenTeams: [],
      agentsPaneSearch: { query: "worker", status: "working" },
      agentsWorkspaceFilter: { teamKey: "acme:a", workspaceId: "ws" },
      agentsListFilter: { agentKey: "a", name: "Worker" },
      agentsPaneTeamFocusRestore: null,
    });
    expect(ui.write).not.toHaveBeenCalled();
  } finally {
    stop();
  }
});
