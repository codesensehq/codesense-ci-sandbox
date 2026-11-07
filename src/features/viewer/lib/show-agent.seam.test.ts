// SPDX-License-Identifier: MIT
import { act } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

// Seam: the real viewer store and its terminal-layout projection over the test terminal host,
// the real org store and roster store. Only the run-output panel, the focus helper, toasts and
// the Tauri boundary are faked.

const h = vi.hoisted(() => ({
  openAgentResult: vi.fn(),
  focus: vi.fn(),
  // What a fresh read of This Mac's live sessions lists.
  listed: [] as import("@/tauri/commands").AgentSessionRow[],
}));
vi.mock("@/features/viewer/components/agent-result-panel", () => ({
  openAgentResult: h.openAgentResult,
}));
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
  dashboardListSessions: vi.fn(async () => h.listed),
  saveAppState: vi.fn(async () => {}),
  pushViewerState: vi.fn(async () => {}),
}));

import { toast } from "sonner";

import { resolveAgentShow, setRunHistoryOpener, showAgent, type ShowInputs } from "./show-agent";
import { registerMachineLookup } from "@/features/machines/lib/machine-offline";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { useOrgStore } from "@/shared/stores/org-store";
import {
  selectActiveTerminalTab,
  selectHiddenTerminalTabs,
  selectWorkspaceTabs,
  useViewerStore,
  type TerminalTab,
} from "@/shared/stores/viewer-store";
import {
  connectTestRemoteHost,
  installTerminalLayoutHarness,
  refetchTestLayout,
  seedTerminalTabs,
  terminalHost,
} from "@/test/terminal-layout-harness";
import type { AgentSessionRow, MachineSummary } from "@/tauri/commands";

installTerminalLayoutHarness();

const TEAM = "acme:core";
const OTHER = "acme:ops";

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

const s = () => useViewerStore.getState();
const tabsIn = (team: string, ws: string) => selectWorkspaceTabs(s(), team, ws).map((t) => t.id);
const settle = () => act(async () => {});

beforeEach(() => {
  vi.clearAllMocks();
  h.listed = [];
  registerMachineLookup(() => null);
  useViewerStore.setState({
    workspaces: {
      [TEAM]: [
        { id: "w1", kind: "generic", cwd: "/w1", status: "ready" },
        { id: "w2", kind: "generic", cwd: "/w2", status: "ready" },
      ],
      [OTHER]: [{ id: "o1", kind: "generic", cwd: "/o1", status: "ready" }],
    },
    softClosedWorkspaces: {},
    activeWorkspace: { [TEAM]: "w1" },
  });
  useAgentListProjectionStore.setState({ sessions: null, error: null });
  useOrgStore.setState({
    activeOrgSlug: "acme",
    activeTeamName: "core",
    orgs: [{ slug: "acme", teams: [{ name: "core" }, { name: "ops" }] }] as ReturnType<
      typeof useOrgStore.getState
    >["orgs"],
    shownTeamsByOrgSlug: {},
    disabledOrgSlugs: [],
    setActiveTeamIn: vi.fn((orgSlug, name) =>
      useOrgStore.setState({ activeOrgSlug: orgSlug, activeTeamName: name }),
    ),
  });
});

describe("the one Show", () => {
  it("opens captured remote output after its tab closes, without story provenance", () => {
    const plan = resolveAgentShow(
      row({ sessionId: "remote", alive: false, outputRunId: "captured-request" }),
      { teamKey: TEAM },
    );
    expect(plan).toMatchObject({ kind: "ended", runId: "captured-request", storyId: null });
  });

  it("focuses a visible tab in this team", async () => {
    seedTerminalTabs(TEAM, "w1", ["s-a", "s-b"], { active: "s-a" });
    await settle();
    const outcome = await act(() => showAgent(row({ sessionId: "s-b" }), { teamKey: TEAM }));
    expect(outcome).toEqual({ shown: "tab" });
    expect(selectActiveTerminalTab(s(), TEAM, "w1")).toBe("s-b");
    expect(h.focus).toHaveBeenCalledWith(TEAM);
    expect(useOrgStore.getState().setActiveTeamIn).not.toHaveBeenCalled();
  });

  it("puts a Hidden tab back on its tile, the same session, then focuses it", async () => {
    seedTerminalTabs(TEAM, "w1", ["s-a", "s-b"], { active: "s-a" });
    await settle();
    expect(s().hideTerminalTab(TEAM, "s-b", "w1")).toBe(true);
    await settle();
    expect(selectHiddenTerminalTabs(s(), TEAM, "w1").map((t) => t.id)).toEqual(["s-b"]);

    const outcome = await act(() => showAgent(row({ sessionId: "s-b" }), { teamKey: TEAM }));
    await settle();
    expect(outcome).toEqual({ shown: "restored" });
    expect(tabsIn(TEAM, "w1")).toEqual(["s-a", "s-b"]);
    expect(selectHiddenTerminalTabs(s(), TEAM, "w1")).toEqual([]);
  });

  it("brings a tile minimized into the tray back and focuses its tab", async () => {
    seedTerminalTabs(TEAM, "w2", ["s-m"]);
    await settle();
    act(() => s().requestCloseWorkspace(TEAM, "w2"));
    await settle();
    expect(s().softClosedWorkspaces[TEAM]?.w2).toBeDefined();

    const outcome = await act(() => showAgent(row({ sessionId: "s-m" }), { teamKey: TEAM }));
    await settle();
    expect(outcome).toEqual({ shown: "restored" });
    expect(s().workspaces[TEAM].some((w) => w.id === "w2")).toBe(true);
    expect(tabsIn(TEAM, "w2")).toEqual(["s-m"]);
  });

  it("adopts a live session no tile shows into a tab, never a silent no-op", async () => {
    seedTerminalTabs(TEAM, "w1", ["s-a"], { active: "s-a" });
    // Running out of sight in a launch workspace with no tile.
    seedTerminalTabs(TEAM, "launch-ws", ["s-bg"]);
    await settle();
    const outcome = await act(() => showAgent(row({ sessionId: "s-bg" }), { teamKey: TEAM }));
    await settle();
    expect(outcome).toEqual({ shown: "adopted" });
    expect(tabsIn(TEAM, "w1")).toContain("s-bg");
  });

  it("switches to the other team an agent runs in, then focuses its tab", async () => {
    seedTerminalTabs(OTHER, "o1", ["s-o", "s-p"], { active: "s-o" });
    await settle();
    const outcome = await act(() => showAgent(row({ sessionId: "s-p" }), { teamKey: TEAM }));
    expect(outcome).toEqual({ shown: "tab" });
    expect(useOrgStore.getState().setActiveTeamIn).toHaveBeenCalledWith("acme", "ops");
    expect(selectActiveTerminalTab(s(), OTHER, "o1")).toBe("s-p");
  });

  it("opens a headless run's output instead of saying it isn't running", async () => {
    const outcome = await showAgent(
      row({
        sessionId: "headless-1",
        kind: "headless",
        provenance: { runId: "run-headless-1", storyId: "SW-9" },
      }),
      { teamKey: TEAM },
    );
    expect(outcome).toEqual({ shown: "runOutput" });
    expect(h.openAgentResult).toHaveBeenCalledWith({
      run_id: "run-headless-1",
      sessionId: "headless-1",
      story_id: "SW-9",
    });
    expect(toast).not.toHaveBeenCalled();
  });

  it("opens the saved output of an agent that ended since the list refreshed, with no toast", async () => {
    seedTerminalTabs(TEAM, "w1", ["s-a"]);
    await settle();
    const ended = row({ sessionId: "s-a", provenance: { runId: "run-7", storyId: "SW-3" } });
    useAgentListProjectionStore.setState({ sessions: [{ ...ended, alive: false }], error: null });
    const outcome = await showAgent(ended, { teamKey: TEAM });
    expect(outcome).toEqual({ shown: "runOutput" });
    expect(h.openAgentResult).toHaveBeenCalledWith({
      run_id: "run-7",
      sessionId: "s-a",
      story_id: "SW-3",
    });
    expect(toast).not.toHaveBeenCalled();
  });

  it("adopts a session the host kept in a workspace no record keys into the team's workspace in its directory", async () => {
    seedTerminalTabs(TEAM, "w1", ["s-a"], { active: "s-a" });
    // A fresh launch: the host still runs it under a directory key no record mints.
    terminalHost().seed(
      `${TEAM}|generic:/w2`,
      { displayName: "generic:/w2", kind: "generic", worktreePath: "/w2" },
      { session: "s-kept" },
    );
    await settle();
    const outcome = await act(() => showAgent(row({ sessionId: "s-kept" }), { teamKey: TEAM }));
    await settle();
    expect(outcome).toEqual({ shown: "adopted" });
    expect(tabsIn(TEAM, "w2")).toEqual(["s-kept"]);
    // The workspace the agents pane lists it under, not a second tile for the same directory.
    expect(s().workspaces[TEAM].map((w) => w.id)).toEqual(["w1", "w2"]);
  });

  describe("bringing up the workspace in its directory when the tray's rule does nothing", () => {
    async function showKeptWith(over: Partial<ReturnType<typeof s>>) {
      seedTerminalTabs(TEAM, "w1", ["s-a"], { active: "s-a" });
      terminalHost().seed(
        `${TEAM}|generic:/w2`,
        { displayName: "generic:/w2", kind: "generic", worktreePath: "/w2" },
        { session: "s-kept" },
      );
      await settle();
      const saved = s();
      useViewerStore.setState({ activateTrayWorkspace: () => false, ...over });
      try {
        const outcome = await act(() => showAgent(row({ sessionId: "s-kept" }), { teamKey: TEAM }));
        await settle();
        return outcome;
      } finally {
        useViewerStore.setState({
          activateTrayWorkspace: saved.activateTrayWorkspace,
          openWorkspace: saved.openWorkspace,
        });
      }
    }

    it("adopts into it once opening it as a tile focuses it", async () => {
      expect(await showKeptWith({})).toEqual({ shown: "adopted" });
      expect(s().activeWorkspace[TEAM]).toBe("w2");
      expect(tabsIn(TEAM, "w2")).toEqual(["s-kept"]);
    });

    it("does not claim it when opening it left it unfocused, and adopts into the focused tile", async () => {
      const openWorkspace = vi.fn();
      expect(await showKeptWith({ openWorkspace })).toEqual({ shown: "adopted" });
      expect(openWorkspace).toHaveBeenCalled();
      expect(s().activeWorkspace[TEAM]).toBe("w1");
      expect(tabsIn(TEAM, "w2")).toEqual([]);
      expect(tabsIn(TEAM, "w1")).toEqual(["s-a", "s-kept"]);
    });
  });

  it("adopts a session in the directory of a workspace minimized into the tray by bringing that workspace back", async () => {
    seedTerminalTabs(TEAM, "w1", ["s-a"], { active: "s-a" });
    // A running shell keeps the closed tile minimized in the tray.
    seedTerminalTabs(TEAM, "w2", ["s-w2"]);
    await settle();
    act(() => s().requestCloseWorkspace(TEAM, "w2"));
    await settle();
    expect(s().softClosedWorkspaces[TEAM]?.w2).toBeDefined();
    terminalHost().seed(
      `${TEAM}|generic:/w2/`,
      { displayName: "generic:/w2/", kind: "generic", worktreePath: "/w2/" },
      { session: "s-kept" },
    );
    await settle();
    const outcome = await act(() => showAgent(row({ sessionId: "s-kept" }), { teamKey: TEAM }));
    await settle();
    expect(outcome).toEqual({ shown: "adopted" });
    expect(s().softClosedWorkspaces[TEAM]?.w2).toBeUndefined();
    expect(s().workspaces[TEAM].some((w) => w.id === "w2")).toBe(true);
    expect(tabsIn(TEAM, "w2")).toEqual(["s-w2", "s-kept"]);
    // The minimized workspace came back: no second tile for its directory.
    expect(s().workspaces[TEAM].some((w) => w.id.startsWith("generic:"))).toBe(false);
  });

  it("an ended agent with no run to link to says it has no saved output, with no action", async () => {
    seedTerminalTabs(TEAM, "w1", ["s-a"]);
    await settle();
    const ended = row({ sessionId: "s-a" });
    useAgentListProjectionStore.setState({ sessions: [{ ...ended, alive: false }], error: null });
    const outcome = await showAgent(ended, { teamKey: TEAM });
    const reason = "Reviewer has ended. There's no saved output for this run.";
    expect(outcome).toEqual({ notShown: reason });
    expect(toast).toHaveBeenCalledWith(reason, undefined);
  });

  it("an ended agent with no run of its own offers Run history while a surface shows it", async () => {
    const openHistory = vi.fn();
    setRunHistoryOpener(openHistory);
    try {
      const ended = row({ sessionId: "s-gone", alive: false });
      await showAgent(ended, { teamKey: TEAM });
      const [message, options] = vi.mocked(toast).mock.calls[0] as [
        string,
        { action: { label: string; onClick: () => void } },
      ];
      expect(message).toBe("Reviewer has ended. There's no saved output for this run.");
      expect(options.action.label).toBe("Run history");
      options.action.onClick();
      expect(openHistory).toHaveBeenCalled();
    } finally {
      setRunHistoryOpener(null);
    }
  });

  it("adopts a session into the workspace in its tab's directory before its story's, as the agents pane lists it", async () => {
    useViewerStore.setState({
      workspaces: {
        ...s().workspaces,
        [TEAM]: [
          ...s().workspaces[TEAM],
          {
            id: "SW-1-story",
            kind: "story",
            storyId: "SW-1",
            storyFilePath: "/stories/SW-1-story.md",
            cwd: "/st",
            status: "ready",
          },
        ],
      },
    });
    seedTerminalTabs(TEAM, "w1", ["s-a"], { active: "s-a" });
    terminalHost().seed(
      `${TEAM}|generic:/w2`,
      { displayName: "generic:/w2", kind: "generic", worktreePath: "/w2" },
      { session: "s-kept" },
    );
    await settle();
    const outcome = await act(() =>
      showAgent(row({ sessionId: "s-kept", handle: "SW-1#dev" }), { teamKey: TEAM }),
    );
    await settle();
    expect(outcome).toEqual({ shown: "adopted" });
    expect(tabsIn(TEAM, "w2")).toEqual(["s-kept"]);
    expect(tabsIn(TEAM, "SW-1-story")).toEqual([]);
  });
});

describe("Show of an agent on a remote machine", () => {
  // A remote agent launched into a workspace with no tile here: its machine's host keeps its tab
  // under a key no record on this Mac mints, in a directory on the machine.
  const KEY = `${TEAM}|generic:/home/codesense/worktrees/w9`;
  function seedRemoteAgent() {
    const remote = connectTestRemoteHost("box", "7");
    remote.seed(
      KEY,
      {
        displayName: "w9",
        kind: "generic",
        org: "acme",
        team: "core",
        worktreePath: "/home/codesense/worktrees/w9",
      },
      { session: "remote-shell-9", name: "usage-claude", tags: { remoteHandle: "h-9" } },
    );
    refetchTestLayout();
    return remote;
  }
  const remoteRow = () =>
    row({ sessionId: "remote-shell-9", handle: "usage-claude", displayName: "usage-claude" });

  describe("a session no read places yet (a roster row with no launch record)", () => {
    // After a restart, a remote launch's record comes back only once its machine's layout is
    // read: until then its durable roster row says nothing of where it runs.
    const unplaced = () =>
      row({ sessionId: "s-unplaced", handle: "usage-claude", displayName: "usage-claude" });

    it("is adopted only once This Mac's own host is seen holding it", async () => {
      seedTerminalTabs(TEAM, "w1", ["s-a"], { active: "s-a" });
      await settle();
      h.listed = [row({ sessionId: "s-unplaced", handle: "usage-claude", machineId: null })];
      const adopt = vi.spyOn(s(), "adoptTerminalSession");
      const outcome = await act(() => showAgent(unplaced(), { teamKey: TEAM }));
      await settle();
      expect(outcome).toEqual({ shown: "adopted" });
      expect(adopt).toHaveBeenCalledWith(TEAM, "s-unplaced", undefined, {
        isCurrent: expect.any(Function),
      });
    });

    it("is never adopted when This Mac's hosts don't hold it: its machine may be reconnecting", async () => {
      const adopt = vi.spyOn(s(), "adoptTerminalSession");
      const before = s().workspaces;
      const outcome = await act(() => showAgent(unplaced(), { teamKey: TEAM }));
      await settle();
      const reason =
        "Can't find usage-claude on this Mac. If it runs on another machine, that machine may still be reconnecting.";
      expect(outcome).toEqual({ notShown: reason });
      expect(toast).toHaveBeenCalledWith(reason, undefined);
      expect(adopt).not.toHaveBeenCalled();
      expect(s().workspaces).toBe(before);
    });

    it("says it has ended when This Mac's host held it and it no longer runs, and adopts nothing", async () => {
      h.listed = [
        row({ sessionId: "s-unplaced", handle: "usage-claude", machineId: null, alive: false }),
      ];
      const adopt = vi.spyOn(s(), "adoptTerminalSession");
      const openHistory = vi.fn();
      setRunHistoryOpener(openHistory);
      try {
        const outcome = await act(() => showAgent(unplaced(), { teamKey: TEAM }));
        const reason = "usage-claude has ended. There's no saved output for this run.";
        expect(outcome).toEqual({ notShown: reason });
        // As any ended agent with no run of its own: the toast offers Run history.
        expect(toast).toHaveBeenCalledWith(reason, {
          action: { label: "Run history", onClick: openHistory },
        });
      } finally {
        setRunHistoryOpener(null);
      }
      expect(adopt).not.toHaveBeenCalled();
    });

    it("found ended on a machine, shows its run's saved output as any ended agent does", async () => {
      h.listed = [
        row({ sessionId: "s-unplaced", handle: "usage-claude", machineId: "box", alive: false }),
      ];
      const adopt = vi.spyOn(s(), "adoptTerminalSession");
      const outcome = await act(() =>
        showAgent(
          { ...unplaced(), provenance: { runId: "run-7", storyId: "S-1" } as never },
          { teamKey: TEAM },
        ),
      );
      expect(outcome).toEqual({ shown: "runOutput" });
      expect(h.openAgentResult).toHaveBeenCalledWith({
        run_id: "run-7",
        sessionId: "s-unplaced",
        story_id: "S-1",
      });
      expect(adopt).not.toHaveBeenCalled();
    });

    it("adopts nothing when the fresh read fails, and says it can't be found here", async () => {
      const { dashboardListSessions } = await import("@/tauri/commands");
      vi.mocked(dashboardListSessions).mockRejectedValueOnce(new Error("no host"));
      const adopt = vi.spyOn(s(), "adoptTerminalSession");
      const outcome = await act(() => showAgent(unplaced(), { teamKey: TEAM }));
      expect(outcome).toEqual({
        notShown:
          "Can't find usage-claude on this Mac. If it runs on another machine, that machine may still be reconnecting.",
      });
      expect(adopt).not.toHaveBeenCalled();
    });

    it("names the machine a fresh read says it runs on, and adopts nothing", async () => {
      registerMachineLookup((id) =>
        id === "box" ? ({ machineId: "box", label: "Studio box" } as MachineSummary) : null,
      );
      h.listed = [row({ sessionId: "s-unplaced", handle: "usage-claude", machineId: "box" })];
      const adopt = vi.spyOn(s(), "adoptTerminalSession");
      const outcome = await act(() => showAgent(unplaced(), { teamKey: TEAM }));
      expect(outcome).toEqual({
        notShown:
          "usage-claude runs on Studio box, and its tab hasn't reached this Mac yet, so there is nothing to show.",
      });
      expect(adopt).not.toHaveBeenCalled();
    });

    it("is not asked about when the row says it runs on This Mac (null), whatever the roster lists", async () => {
      const { dashboardListSessions } = await import("@/tauri/commands");
      seedTerminalTabs(TEAM, "w1", ["s-a"], { active: "s-a" });
      await settle();
      useAgentListProjectionStore.setState({
        sessions: [row({ sessionId: "s-unplaced", handle: "usage-claude", machineId: "box" })],
        error: null,
      });
      const outcome = await act(() =>
        showAgent({ ...unplaced(), machineId: null }, { teamKey: TEAM }),
      );
      await settle();
      expect(outcome).toEqual({ shown: "adopted" });
      expect(dashboardListSessions).not.toHaveBeenCalled();
    });
  });

  it("trusts a local tab over a roster row that says the session is remote", async () => {
    registerMachineLookup((id) =>
      id === "box" ? ({ machineId: "box", label: "Studio box" } as MachineSummary) : null,
    );
    seedTerminalTabs(TEAM, "w1", ["s-a"], { active: "s-a" });
    // Its tab is on This Mac's host, in a launch workspace with no tile.
    seedTerminalTabs(TEAM, "launch-ws", ["s-bg"]);
    await settle();
    const outcome = await act(() =>
      showAgent(row({ sessionId: "s-bg", machineId: "box" }), { teamKey: TEAM }),
    );
    await settle();
    expect(outcome).toEqual({ shown: "adopted" });
    expect(tabsIn(TEAM, "w1")).toContain("s-bg");
  });

  it("says why a remote agent in a workspace with no tile here can't be shown, and opens no local tile for its machine's directory", async () => {
    const box = {
      machineId: "box",
      label: "Studio box",
      status: { state: "connected" },
    } as unknown as MachineSummary;
    registerMachineLookup((id) => (id === "box" ? box : null));
    seedRemoteAgent();
    await settle();
    const before = s().workspaces;
    const outcome = await act(() => showAgent(remoteRow(), { teamKey: TEAM }));
    await settle();
    const reason =
      "usage-claude runs on Studio box, in a workspace that isn't open on this Mac, so there is no tab to show.";
    expect(outcome).toEqual({ notShown: reason });
    expect(toast).toHaveBeenCalledWith(reason, undefined);
    // Nothing was adopted into a tile on This Mac, and no tile was opened for a directory that
    // is on the machine.
    expect(s().workspaces).toBe(before);
    expect(tabsIn(TEAM, "w1")).toEqual([]);
  });

  describe("a remote session whose tab has not arrived yet", () => {
    // Its machine's layout has not loaded: no host holds its tab, only the roster's launch
    // record says where it runs.
    const tablessRow = () =>
      row({
        sessionId: "remote-shell-7",
        handle: "usage-claude",
        displayName: "usage-claude",
        machineId: "box",
      });
    const lookupBox = (state: string) => {
      const box = {
        machineId: "box",
        label: "Studio box",
        status: { state, lastReachedAtMs: 0 },
      } as unknown as MachineSummary;
      registerMachineLookup((id) => (id === "box" ? box : null));
    };

    it("is never adopted on This Mac: its machine is named instead", async () => {
      lookupBox("connected");
      const adopt = vi.spyOn(s(), "adoptTerminalSession");
      const before = s().workspaces;
      const outcome = await act(() => showAgent(tablessRow(), { teamKey: TEAM }));
      await settle();
      expect(outcome).toEqual({
        notShown:
          "usage-claude runs on Studio box, and its tab hasn't reached this Mac yet, so there is nothing to show.",
      });
      expect(adopt).not.toHaveBeenCalled();
      expect(s().workspaces).toBe(before);
      expect(tabsIn(TEAM, "w1")).toEqual([]);
    });

    it("reads the machine from the roster when the caller's row does not carry it", async () => {
      lookupBox("connected");
      useAgentListProjectionStore.setState({ sessions: [tablessRow()], error: null });
      const adopt = vi.spyOn(s(), "adoptTerminalSession");
      const { machineId: _, ...bare } = tablessRow();
      const outcome = await act(() => showAgent(bare, { teamKey: TEAM }));
      expect(outcome).toEqual({
        notShown:
          "usage-claude runs on Studio box, and its tab hasn't reached this Mac yet, so there is nothing to show.",
      });
      expect(adopt).not.toHaveBeenCalled();
    });

    it("says its machine is offline when it is", async () => {
      lookupBox("unreachable");
      const adopt = vi.spyOn(s(), "adoptTerminalSession");
      const outcome = await act(() => showAgent(tablessRow(), { teamKey: TEAM }));
      expect(outcome).toEqual({
        notShown:
          "Studio box is offline, so usage-claude can't be opened; the agent may still be running there.",
      });
      expect(adopt).not.toHaveBeenCalled();
    });
  });

  it("says its machine's terminals can't be read while its tab is held as it last was, naming a machine no list knows readably", async () => {
    const remote = seedRemoteAgent();
    await settle();
    remote.online = false;
    refetchTestLayout();
    await settle();
    const outcome = await act(() => showAgent(remoteRow(), { teamKey: TEAM }));
    const reason =
      "CodeSense can't read a remote machine's terminals right now, so usage-claude can't be opened; the agent may still be running there.";
    expect(outcome).toEqual({ notShown: reason });
    expect(toast).toHaveBeenCalledWith(reason, undefined);
    expect(s().workspaces[TEAM].map((w) => w.id)).toEqual(["w1", "w2"]);
  });

  it("shows a tab on a tile whose machine is offline as it last was, and says so", async () => {
    const box = {
      machineId: "box",
      label: "Studio box",
      status: { state: "unreachable", lastReachedAtMs: 0 },
    } as unknown as MachineSummary;
    registerMachineLookup((id) => (id === "box" ? box : null));
    const remote = connectTestRemoteHost("box", "7");
    remote.seed(
      `${TEAM}|w2`,
      { displayName: "w2", kind: "generic", org: "acme", team: "core", worktreePath: "/w2" },
      { session: "remote-shell-2", name: "usage-claude", tags: { remoteHandle: "h-2" } },
    );
    refetchTestLayout();
    await settle();
    const outcome = await act(() =>
      showAgent(
        row({ sessionId: "remote-shell-2", handle: "usage-claude", displayName: "usage-claude" }),
        { teamKey: TEAM },
      ),
    );
    const notice =
      "Studio box is offline, so usage-claude's tab shows how it last was; the agent may still be running there.";
    expect(outcome).toEqual({ shown: "tab", notice });
    expect(toast).toHaveBeenCalledWith(notice, undefined);
  });
  it("shows a tab on a tile whose machine's terminals can't be read as it last was, naming the machine", async () => {
    const box = {
      machineId: "box",
      label: "Studio box",
      status: { state: "connected" },
    } as unknown as MachineSummary;
    registerMachineLookup((id) => (id === "box" ? box : null));
    const remote = connectTestRemoteHost("box", "7");
    remote.seed(
      `${TEAM}|w2`,
      { displayName: "w2", kind: "generic", org: "acme", team: "core", worktreePath: "/w2" },
      { session: "remote-shell-2", name: "usage-claude", tags: { remoteHandle: "h-2" } },
    );
    refetchTestLayout();
    await settle();
    remote.online = false;
    refetchTestLayout();
    await settle();
    const outcome = await act(() =>
      showAgent(
        row({ sessionId: "remote-shell-2", handle: "usage-claude", displayName: "usage-claude" }),
        { teamKey: TEAM },
      ),
    );
    const notice =
      "CodeSense can't read Studio box's terminals right now, so usage-claude's tab shows how it last was; the agent may still be running there.";
    expect(outcome).toEqual({ shown: "tab", notice });
    expect(toast).toHaveBeenCalledWith(notice, undefined);
  });
});

describe("resolveAgentShow", () => {
  const tab = (id: string, extra: Partial<TerminalTab> = {}) =>
    ({ id, sessionId: id, name: id, ...extra }) as TerminalTab;
  const at = (teamKey: string, workspaceId: string, t: TerminalTab) => ({
    teamKey,
    workspaceId,
    tab: t,
  });
  const base: ShowInputs = { teamKey: TEAM };
  const live = row({ sessionId: "s-1" });

  it("names a machine that is offline before adopting a tab no tile shows", () => {
    expect(
      resolveAgentShow(live, {
        ...base,
        open: { ...at(TEAM, "generic:/x", tab("s-1")), tile: "none", dir: "/x" },
        offlineMachine: "Studio Mac",
      }),
    ).toEqual({ kind: "offline", machine: "Studio Mac", teamKey: TEAM });
  });

  it("calls a session ended only when it has ended", () => {
    expect(resolveAgentShow({ ...live, alive: false }, base).kind).toBe("ended");
    expect(resolveAgentShow(live, { ...base, rosterAlive: false }).kind).toBe("ended");
    expect(
      resolveAgentShow(live, {
        ...base,
        open: {
          ...at(TEAM, "w1", tab("s-1", { exit: { code: 0 } } as never)),
          tile: "live",
          dir: null,
        },
      }).kind,
    ).toBe("ended");
    // A live session nobody shows is adopted, a reserved one is still starting.
    expect(resolveAgentShow(live, base).kind).toBe("adopt");
    expect(resolveAgentShow({ ...live, sessionId: null }, base).kind).toBe("starting");
  });

  it("never adopts a remote tab with no tile into a tile on This Mac", () => {
    const remote = { machineId: "box", member: "box:remote", handle: "h" };
    const untiled = { ...at(TEAM, "generic:/home/x", tab("s-1", { origin: remote })) };
    expect(
      resolveAgentShow(live, {
        ...base,
        open: { ...untiled, tile: "none", dir: "/home/x" },
        remoteMachine: "box",
      }),
    ).toEqual({ kind: "remote-untiled", machine: "box" });
    expect(
      resolveAgentShow(live, {
        ...base,
        open: { ...untiled, tile: "none", dir: "/home/x" },
        frozenMachine: "box",
        remoteMachine: "box",
      }),
    ).toEqual({ kind: "frozen", machine: "box" });
    // On a tile it shows, frozen or not: the tile says how it last was.
    expect(
      resolveAgentShow(live, {
        ...base,
        open: { ...untiled, tile: "live", dir: null },
        frozenMachine: "box",
        remoteMachine: "box",
      }).kind,
    ).toBe("tab");
  });

  it("offline and frozen follow one rule: on a tile shown with a notice, anywhere else named", () => {
    const remote = { machineId: "box", member: "box:remote", handle: "h" };
    const t = tab("s-1", { origin: remote });
    for (const held of [{ offlineMachine: "box" }, { frozenMachine: "box" }]) {
      const offline = "offlineMachine" in held;
      expect(
        resolveAgentShow(live, {
          ...base,
          ...held,
          open: { ...at(TEAM, "w1", t), tile: "live", dir: null },
        }),
      ).toMatchObject({ kind: "tab", held: { machine: "box", offline } });
      expect(
        resolveAgentShow(live, { ...base, ...held, minimized: at(TEAM, "w1", t) }),
      ).toMatchObject({ kind: "tab", minimized: true, held: { machine: "box", offline } });
      // A Hidden tab is on no tile: its machine is named rather than a restore that can't attach.
      expect(resolveAgentShow(live, { ...base, ...held, hidden: at(TEAM, "w1", t) })).toEqual({
        kind: offline ? "offline" : "frozen",
        machine: "box",
        ...(offline ? { teamKey: TEAM } : {}),
      });
      // Named before the remote-untiled reason, which a held machine's tab also has.
      expect(
        resolveAgentShow(live, {
          ...base,
          ...held,
          open: { ...at(TEAM, "generic:/x", t), tile: "none", dir: null },
          remoteMachine: "box",
        }).kind,
      ).toBe(offline ? "offline" : "frozen");
    }
    // A tab read live carries no notice.
    expect(
      resolveAgentShow(live, {
        ...base,
        open: { ...at(TEAM, "w1", tab("s-1")), tile: "live", dir: null },
      }),
    ).not.toHaveProperty("held");
  });

  it("adopts a tabless session into its story's team, else the team Show runs from", () => {
    expect(resolveAgentShow(live, { ...base, storyTeamKey: OTHER })).toMatchObject({
      kind: "adopt",
      teamKey: OTHER,
    });
    expect(resolveAgentShow({ ...live, workspace: "/repo/x" }, base)).toMatchObject({
      kind: "adopt",
      teamKey: TEAM,
      dir: "/repo/x",
    });
  });
});
