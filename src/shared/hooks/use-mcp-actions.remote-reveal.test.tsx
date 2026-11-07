// SPDX-License-Identifier: MIT
import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

// An agent's remote tab reveal (MCP open_remote_tab, headed) is an open the user did not click:
// it switches no team. It focuses the workspace and its tab in that team's memory and waits for
// the user to go there. Faked: the Tauri event bus and commands, the machine stores and the
// private-terminal authority.

type Handler = (event: { payload: unknown }) => void;
const handlers = new Map<string, Handler>();
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((event: string, handler: Handler) => {
    handlers.set(event, handler);
    return Promise.resolve(() => {});
  }),
}));
vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
  terminalLayoutMemberSnapshots: vi.fn().mockResolvedValue([]),
}));
vi.mock("@/shared/lib/private-terminal-authority", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/shared/lib/private-terminal-authority")>()),
  currentPrivateAuthorityEpoch: () => "7",
  onPrivateAuthorityRetired: () => () => {},
}));
vi.mock("@/features/machines/stores/machines-store", () => ({
  ensureTeamMachines: vi.fn().mockResolvedValue(true),
}));
vi.mock("@/features/machines/stores/machine-projection-selectors", () => ({
  machineWorkspace: () => ({
    tabs: [
      {
        session: "remote-1",
        state: { kind: "running" },
        tags: { remoteHandle: "h-1", remoteRequestId: "r-1" },
      },
    ],
  }),
}));

import { REMOTE_TERMINAL_REVEAL_EVENT } from "@/tauri/events";
import { workspaceKey } from "@/shared/lib/workspace-key";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";

import { useMcpActions } from "./use-mcp-actions";

const A = "acme:core";
const B = "acme:ops";
const ws = (id: string): Workspace => ({ id, kind: "generic", cwd: `/${id}`, status: "ready" });

beforeEach(() => {
  handlers.clear();
  useOrgStore.setState({
    orgs: [
      {
        slug: "acme",
        teams: [{ name: "core" }, { name: "ops" }],
      } as unknown as ReturnType<typeof useOrgStore.getState>["orgs"][number],
    ],
    activeOrgSlug: "acme",
    activeTeamName: "core",
  });
  useViewerStore.setState({
    openedTeams: [A, B],
    workspaces: { [A]: [ws("a1")], [B]: [ws("o1"), ws("o2")] },
    activeWorkspace: { [A]: "a1", [B]: "o1" },
    softClosedWorkspaces: {},
  });
});

describe("a remote tab reveal in a team that is not active", () => {
  it("focuses the workspace in that team's memory and switches no team", async () => {
    const setActiveTerminalTab = vi.fn();
    const revealTerminalInPlace = vi.fn();
    const real = useViewerStore.getState();
    useViewerStore.setState({ setActiveTerminalTab, revealTerminalInPlace });
    try {
      renderHook(() => useMcpActions());
      const handler = handlers.get(REMOTE_TERMINAL_REVEAL_EVENT)!;
      expect(handler).toBeTypeOf("function");
      await act(async () => {
        handler({
          payload: {
            teamKey: B,
            workspaceId: "o2",
            privateAuthorityEpoch: "7",
            outcome: {
              status: "opened",
              headed: true,
              workspaceId: "o2",
              workspaceKey: workspaceKey(B, ws("o2")),
              handle: "h-1",
              requestId: "r-1",
              target: { member: { slot: "remote", machineId: "box" }, session: "remote-1" },
            },
          },
        });
      });
      await vi.waitFor(() =>
        expect(setActiveTerminalTab).toHaveBeenCalledWith(B, "remote-1", "o2"),
      );
      expect(useOrgStore.getState().activeTeamName).toBe("core");
      expect(useViewerStore.getState().activeWorkspace).toEqual({ [A]: "a1", [B]: "o2" });
      expect(revealTerminalInPlace).not.toHaveBeenCalled();
    } finally {
      useViewerStore.setState({
        setActiveTerminalTab: real.setActiveTerminalTab,
        revealTerminalInPlace: real.revealTerminalInPlace,
      });
    }
  });
});

function payload(teamKey: string) {
  return {
    teamKey,
    workspaceId: "o2",
    privateAuthorityEpoch: "7",
    outcome: {
      status: "opened",
      headed: true,
      workspaceId: "o2",
      workspaceKey: workspaceKey(teamKey, ws("o2")),
      handle: "h-1",
      requestId: "r-1",
      target: { member: { slot: "remote", machineId: "box" }, session: "remote-1" },
    },
  };
}

it("an active-team remote reveal shows the CLI", async () => {
  useOrgStore.setState({ activeTeamName: "ops" });
  useViewerStore.setState({ layoutMode: "viewer", terminalVisible: false });
  const real = useViewerStore.getState().setActiveTerminalTab;
  const select = vi.fn();
  useViewerStore.setState({ setActiveTerminalTab: select });
  try {
    renderHook(() => useMcpActions());
    await act(async () => handlers.get(REMOTE_TERMINAL_REVEAL_EVENT)!({ payload: payload(B) }));
    await vi.waitFor(() => expect(select).toHaveBeenCalled());
    expect(useViewerStore.getState()).toMatchObject({
      layoutMode: "shared",
      terminalVisible: true,
    });
  } finally {
    useViewerStore.setState({ setActiveTerminalTab: real });
  }
});

it("another org's reveal remembers its workspace without opening its team", async () => {
  const other = "else:ops";
  useOrgStore.setState({
    orgs: [
      ...useOrgStore.getState().orgs,
      {
        slug: "else",
        teams: [{ name: "ops" }],
      } as ReturnType<typeof useOrgStore.getState>["orgs"][number],
    ],
  });
  useViewerStore.setState({
    workspaces: { ...useViewerStore.getState().workspaces, [other]: [ws("o2")] },
  });
  const open = vi.spyOn(useViewerStore.getState(), "openTeam");
  const real = useViewerStore.getState().setActiveTerminalTab;
  useViewerStore.setState({ setActiveTerminalTab: vi.fn() });
  try {
    renderHook(() => useMcpActions());
    await act(async () => handlers.get(REMOTE_TERMINAL_REVEAL_EVENT)!({ payload: payload(other) }));
    await vi.waitFor(() => expect(useViewerStore.getState().activeWorkspace[other]).toBe("o2"));
    expect(open).not.toHaveBeenCalled();
  } finally {
    open.mockRestore();
    useViewerStore.setState({ setActiveTerminalTab: real });
  }
});

it("does not focus a workspace removed while opening its team", async () => {
  const real = useViewerStore.getState();
  const focus = vi.fn();
  useViewerStore.setState({
    switchWorkspace: focus,
    openTeam: () => useViewerStore.setState({ workspaces: { [A]: [ws("a1")] } }),
  });
  try {
    renderHook(() => useMcpActions());
    await act(async () => handlers.get(REMOTE_TERMINAL_REVEAL_EVENT)!({ payload: payload(B) }));
    expect(focus).not.toHaveBeenCalled();
  } finally {
    useViewerStore.setState({ switchWorkspace: real.switchWorkspace, openTeam: real.openTeam });
  }
});
