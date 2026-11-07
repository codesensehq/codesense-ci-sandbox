// SPDX-License-Identifier: MIT
import { createElement } from "react";
import { AgentRunOutputHost } from "@/features/viewer/components/agent-run-output-host";
import { useAgentResultStore } from "@/shared/stores/use-agent-result-store";
import { act, render, renderHook } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { HTML_BRIDGE_ALLOWLIST as bridge } from "./html-bridge";
import * as showAgentActions from "@/features/viewer/lib/show-agent";
import { useViewerStore, selectActiveTerminalTab } from "@/shared/stores/viewer-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { agentListMirrorInputs } from "@/features/viewer/lib/agent-list-mirror";
import { useLaunchSheetStore } from "@/features/agents/lib/launch-sheet";
import { useAgentInboxRailStore } from "@/features/viewer/stores/agent-inbox-rail-store";
import { useMcpActions } from "@/shared/hooks/use-mcp-actions";
import { installTerminalLayoutHarness, seedTerminalTabs } from "@/test/terminal-layout-harness";
import type { AgentSessionRow } from "@/tauri/commands";
import { frameFixture } from "../../../../tests/support/fixtures/frame";

const h = vi.hoisted(() => ({
  focus: vi.fn(),
  handlers: new Map<string, (event: { payload: unknown }) => void>(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (event, handler) => {
    h.handlers.set(event, handler);
    return () => h.handlers.delete(event);
  }),
}));
vi.mock("@/features/viewer/lib/focus-cascade", async (original) => ({
  ...(await original<typeof import("@/features/viewer/lib/focus-cascade")>()),
  focusActiveWorkspaceTerminal: h.focus,
}));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  saveAppState: vi.fn(async () => {}),
  pushViewerState: vi.fn(async () => {}),
}));
vi.mock("@/features/viewer/components/agent-result-panel", async (original) => ({
  ...(await original<typeof import("@/features/viewer/components/agent-result-panel")>()),
  AgentRunOutputBody: () => null,
}));
installTerminalLayoutHarness();
const team = "acme:core";
const row: AgentSessionRow = {
  id: "agent-row",
  handle: "lead",
  sessionId: "sid",
  kind: "dashboard-launched",
  alive: true,
  backgrounded: false,
  startedWith: null,
  startedAtMs: null,
  launcher: null,
  lastOutputMs: null,
  story: "S-1",
  launchTeamKey: team,
};

beforeEach(() => {
  vi.clearAllMocks();
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "core" });
  useViewerStore.setState({
    workspaces: {
      [team]: ["a", "b", "c"].map((id) => ({
        id,
        kind: "generic",
        cwd: `/${id}`,
        status: "ready",
      })),
    },
    openedTeams: [team],
    activeWorkspace: { [team]: "a" },
    softClosedWorkspaces: {},
    workspaceOrderByTeam: {},
    trayBaseOrderByTeam: { [team]: ["a", "b", "c"] },
    railKeyByWorkspaceId: {},
  });
  useAgentListProjectionStore.setState({
    sessions: [row],
    references: [
      {
        id: "ref",
        sessionRef: row.id!,
        inboxRef: "inbox",
        teamKey: team,
        parentId: null,
        parentSource: null,
        order: 0,
        workspaceRef: "b",
        storyRef: null,
        workspaceRank: 0,
      },
    ],
  });
  useAgentStateStore.setState({
    bySession: {},
    activityBySession: {},
    exitBySession: {},
    processAgentBySession: { sid: "claude" },
  });
  useAgentInboxRailStore.setState({ bySession: {} });
  useLaunchSheetStore.getState().close();
});

it.each([{ before: "a" }, { after: "c" }])(
  "uses native workspace order and mirrors the same move %j",
  async (place) => {
    useViewerStore.getState().reorderWorkspace(team, "b", place);
    const expected = useViewerStore.getState().workspaceOrderByTeam;
    useViewerStore.setState({ workspaceOrderByTeam: {} });
    await expect(
      bridge.moveWorkspace({ teamKey: team, workspaceId: "b", ...place }),
    ).resolves.toEqual({ moved: true });
    expect(useViewerStore.getState().workspaceOrderByTeam).toEqual(expected);
    expect(
      agentListMirrorInputs(useViewerStore.getState()).resolvedWorkspaceOrderByTeam[team],
    ).toEqual(expected[team]);
    await expect(
      bridge.moveWorkspace({ teamKey: team, workspaceId: "b", ...place }),
    ).resolves.toEqual({ moved: false });
    await expect(
      bridge.moveWorkspace({ teamKey: team, workspaceId: "missing", ...place }),
    ).resolves.toEqual({ moved: false });
  },
);

it("routes the MCP event through the same order writer as the page", async () => {
  const hook = renderHook(() => useMcpActions());
  const args = { teamKey: team, workspaceId: "b", before: "a" };
  await bridge.moveWorkspace(args);
  const expected = useViewerStore.getState().workspaceOrderByTeam;
  useViewerStore.setState({ workspaceOrderByTeam: {} });
  act(() => h.handlers.get("mcp:reorder-workspace")!({ payload: args }));
  expect(useViewerStore.getState().workspaceOrderByTeam).toEqual(expected);
  hook.unmount();
});

it("opens the launch sheet with the row's placement team, manager, story and optional task", async () => {
  useOrgStore.setState({ activeTeamName: "other" });
  await bridge.launchUnder({ handle: "lead", task: "Review this" });
  expect(useLaunchSheetStore.getState().request).toEqual({
    teamKey: team,
    managerHandle: "lead",
    storyId: "S-1",
    task: "Review this",
  });
  await expect(bridge.launchUnder({ handle: "missing" })).rejects.toThrow(
    "unavailable: no such agent",
  );
});

it.each([
  { args: { org: "beta", team: "ops" }, expected: "beta:ops" },
  { args: { team: "ops" }, expected: "acme:ops" },
])("openLauncher resolves $args to $expected", async ({ args, expected }) => {
  useOrgStore.setState({
    activeOrgSlug: "acme",
    activeTeamName: "ops",
    orgs: ["acme", "beta"].map((slug) => ({ slug, teams: [{ name: "ops" }] })) as never,
  });
  const frame = frameFixture({ documentPath: "builtin:agents", contentRoot: "", slot: "dock" });
  const message = { org: "beta", team: "ops", messageId: 1, sender: "human" };
  await expect(
    bridge.openLauncher({ ...args, task: "Review" }, { frame, message }),
  ).resolves.toEqual({
    opened: true,
  });
  expect(useLaunchSheetStore.getState().request).toEqual({ teamKey: expected, task: "Review" });
});

it.each(["beta", "unknown"])("openLauncher refuses an unadmitted %s team", async (org) => {
  useOrgStore.setState({
    orgs: ["acme", "beta"].map((slug) => ({ slug, teams: [{ name: "ops" }] })) as never,
  });
  const frame = frameFixture({ documentPath: "/agents.html", contentRoot: "", slot: "dock" });
  frame.grant = { ...frame.grant, scope: "team", team: { org: "acme", team: "ops" } };
  if (org === "unknown") frame.grant.scope = "open";
  await expect(bridge.openLauncher({ org, team: "ops", task: "" }, { frame })).rejects.toThrow(
    `openLauncher: unknown or unadmitted team ${org}:ops`,
  );
  expect(useLaunchSheetStore.getState().request).toBeNull();
});

it("reveals the running agent and opens its inbox rail; rejects a non-running handle", async () => {
  seedTerminalTabs(team, "b", ["sid"]);
  await bridge.openAgentInbox({ handle: "lead" });
  expect(useViewerStore.getState().activeWorkspace[team]).toBe("b");
  expect(useAgentInboxRailStore.getState().bySession.sid).toBeDefined();
  await expect(bridge.openAgentInbox({ handle: "missing" })).rejects.toThrow(
    "unavailable: agent is not running",
  );
  useAgentListProjectionStore.setState({ sessions: [{ ...row, alive: false }] });
  await expect(bridge.openAgentInbox({ handle: "lead" })).rejects.toThrow(
    "unavailable: agent is not running",
  );
});

it.each(
  ["lead", "sid"].flatMap((agentId) =>
    [true, false, undefined].map((preserveFocus) => ({ agentId, preserveFocus })),
  ),
)(
  "gotoAgent $agentId preserveFocus=$preserveFocus selects and reveals the tab with the expected DOM focus",
  async ({ agentId, preserveFocus }) => {
    if (agentId === "sid") {
      useAgentListProjectionStore.setState({
        sessions: [
          row,
          {
            ...row,
            id: "other-row",
            sessionId: "other-sid",
            launchTeamKey: "acme:other",
            startedAtMs: Date.now(),
          },
        ],
      });
    }
    const show = vi.spyOn(showAgentActions, "showAgent");
    seedTerminalTabs(team, "b", ["first", "sid"], { active: "first" });
    const frame = document.createElement("iframe");
    const terminal = document.createElement("textarea");
    document.body.append(frame, terminal);
    h.focus.mockImplementation(() => terminal.focus());
    frame.focus();
    expect(document.activeElement).toBe(frame);
    try {
      await expect(
        bridge.gotoAgent({
          agentId,
          ...(preserveFocus === undefined ? {} : { preserveFocus }),
        }),
      ).resolves.toMatchObject({ located: true });
      await act(async () => {});
      expect(show).toHaveBeenCalledWith(row, { preserveFocus: preserveFocus === true });
      expect(useViewerStore.getState().activeWorkspace[team]).toBe("b");
      expect(selectActiveTerminalTab(useViewerStore.getState(), team, "b")).toBe("sid");
      expect(document.activeElement).toBe(preserveFocus ? frame : terminal);
      expect(h.focus).toHaveBeenCalledTimes(preserveFocus ? 0 : 1);
    } finally {
      show.mockRestore();
      frame.remove();
      terminal.remove();
    }
  },
);

it.each(["hidden", "minimized", "adopt"])(
  "preserves frame focus when the target tab needs to be %s",
  async (state) => {
    const workspace = state === "adopt" ? "launch-ws" : "b";
    seedTerminalTabs(team, workspace, ["sid"]);
    useAgentListProjectionStore.setState({ sessions: [{ ...row, story: null }] });
    if (state === "hidden") useViewerStore.getState().hideTerminalTab(team, "sid", "b");
    if (state === "minimized") useViewerStore.getState().requestCloseWorkspace(team, "b");
    const frame = document.createElement("iframe");
    document.body.append(frame);
    frame.focus();
    try {
      await expect(
        bridge.gotoAgent({ agentId: "lead", preserveFocus: true }),
      ).resolves.toMatchObject({ located: true });
      await act(async () => {});
      expect(document.activeElement).toBe(frame);
      expect(h.focus).not.toHaveBeenCalled();
      expect(
        selectActiveTerminalTab(useViewerStore.getState(), team, state === "adopt" ? "a" : "b"),
      ).toBe("sid");
    } finally {
      frame.remove();
    }
  },
);

it.each([true, false])(
  "keeps frame focus on run output when preserveFocus=true (alive=%s)",
  async (alive) => {
    useAgentResultStore.getState().close();
    useAgentListProjectionStore.setState({
      sessions: [
        {
          ...row,
          alive,
          kind: "headless",
          provenance: { runId: "frame-run", storyId: "S-1" } as AgentSessionRow["provenance"],
        },
      ],
    });
    const view = render(createElement(AgentRunOutputHost, { teamKey: team, repoPath: "/repo" }));
    const frame = document.createElement("iframe");
    document.body.append(frame);
    frame.focus();
    try {
      await act(async () => {
        await bridge.gotoAgent({ agentId: "lead", preserveFocus: true });
      });
      expect(view.getByRole("region", { name: "Run output" })).toBeDefined();
      expect(document.activeElement).toBe(frame);
    } finally {
      view.unmount();
      useAgentResultStore.getState().close();
      frame.remove();
    }
  },
);

it("still focuses run output when preserveFocus is omitted", async () => {
  useAgentResultStore.getState().close();
  useAgentListProjectionStore.setState({
    sessions: [
      {
        ...row,
        kind: "headless",
        provenance: { runId: "default-run", storyId: "S-1" } as AgentSessionRow["provenance"],
      },
    ],
  });
  const view = render(createElement(AgentRunOutputHost, { teamKey: team, repoPath: "/repo" }));
  try {
    await act(async () => {
      await bridge.gotoAgent({ agentId: "lead" });
    });
    expect(document.activeElement).toBe(view.getByRole("region", { name: "Run output" }));
  } finally {
    view.unmount();
    useAgentResultStore.getState().close();
  }
});

it("keeps frame focus when gotoAgent replaces an open run with preserveFocus", async () => {
  useAgentResultStore.getState().close();
  useAgentListProjectionStore.setState({
    sessions: [
      {
        ...row,
        kind: "headless",
        provenance: { runId: "run-a", storyId: "S-1" } as AgentSessionRow["provenance"],
      },
      {
        ...row,
        id: "agent-b",
        handle: "other",
        sessionId: "sid-b",
        kind: "headless",
        provenance: { runId: "run-b", storyId: "S-1" } as AgentSessionRow["provenance"],
      },
    ],
  });
  const view = render(createElement(AgentRunOutputHost, { teamKey: team, repoPath: "/repo" }));
  const terminal = document.createElement("textarea");
  const frame = document.createElement("iframe");
  document.body.append(terminal, frame);
  terminal.focus();
  try {
    await act(async () => {
      await bridge.gotoAgent({ agentId: "lead" });
    });
    const outgoing = view.getByRole("region", { name: "Run output" });
    expect(document.activeElement).toBe(outgoing);
    frame.focus();
    await act(async () => {
      await bridge.gotoAgent({ agentId: "other", preserveFocus: true });
    });
    expect(useAgentResultStore.getState().runId).toBe("run-b");
    expect(view.getByRole("region", { name: "Run output" })).not.toBe(outgoing);
    expect(document.activeElement).toBe(frame);
  } finally {
    view.unmount();
    useAgentResultStore.getState().close();
    frame.remove();
    terminal.remove();
  }
});
