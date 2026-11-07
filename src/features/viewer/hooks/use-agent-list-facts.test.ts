// SPDX-License-Identifier: MIT
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { AgentSessionRow } from "@/tauri/commands";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("@/shared/hooks/use-avatar-packs", async () => {
  const { BUILTIN_FACE_SOURCE } = await import("@/shared/lib/avatar-packs");
  const of = () => ({ faces: BUILTIN_FACE_SOURCE });
  return { useTeamAvatarPacks: () => of };
});
vi.mock("./use-pane-workspace-wait", () => ({
  usePaneWorkspaceWait: () => ({ workspaceWait: null }),
}));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  routineListRows: async () => [],
  rolesList: async () => [{ id: "builder", name: "Builder", icon: "builtin-chess:rook" }],
  agentReadRoles: async () => ({
    roles: [{ role: "builder", registrations: [{ instanceId: "s1", generation: 1, live: true }] }],
  }),
}));
const { useViewerStore } = await import("@/shared/stores/viewer-store");
const { useAgentListProjectionStore } = await import("../lib/agent-list-projection");
const { useAgentListFacts } = await import("../lib/agent-list-facts");
const { useAgentListFactsMirror } = await import("./use-agent-list-facts");
const { useRoutinesStore } = await import("@/features/routines/stores/routines-store");
const TEAM = "acme:dev";
afterEach(cleanup);

it("projects native identity, role holdings and wake facts once; returned fields do not echo", async () => {
  const rows = [1, 2, 3].map((n) => ({
    id: `s${n}`,
    sessionId: `s${n}`,
    handle: `worker-${n}`,
    kind: "headless",
    alive: true,
    machineId: "local",
    startedAtMs: n,
    provenance: { routineKey: "wake" },
    detected: { cli: "codex" },
    avatar: n === 3 ? "#64748b" : "builtin-funny:01",
    rowNames: { primary: "Worker", secondary: null, full: "Worker" },
  })) as AgentSessionRow[];
  useViewerStore.setState({
    stateLoaded: true,
    agentIdentity: { [TEAM]: { "worker-1": { role: "manager", face: "builtin-chess:rook" } } },
  });
  useViewerStore.setState((s) => ({
    terminalLayout: {
      ...s.terminalLayout,
      byKey: {
        [`${TEAM}|ws`]: {
          tabs: [],
          hiddenTabs: [{ id: "tab", sessionId: "s1", title: "Worker", kind: "terminal" }],
          active: null,
        } as never,
      },
    },
  }));
  useAgentListProjectionStore.setState({
    sessions: rows,
    references: rows.map((r, i) => ({
      id: `r${i}`,
      sessionRef: r.id!,
      teamKey: TEAM,
      parentId: i ? "r0" : null,
    })) as never,
  });
  useRoutinesStore.setState({
    rows: [
      {
        key: "wake",
        def: { name: "Inbox sweep" },
        queued: { sessionId: "s1", machineId: "local", deadlineMs: 100000 },
      },
    ] as never,
  });
  const changed = vi.fn(),
    stop = useAgentListFacts.subscribe(changed);
  const hook = renderHook(useAgentListFactsMirror);
  await waitFor(() =>
    expect(useAgentListFacts.getState().sessions.s1).toMatchObject({
      headless: true,
      tabHidden: true,
      dueWake: { name: "Inbox sweep" },
      rolePiece: { piece: "manager" },
      face: { kind: "image", src: "/kit/v1/avatars/builtin-chess/rook.svg" },
      roleHoldings: [{ role: "builder", name: "Builder", teamKey: TEAM }],
    }),
  );
  expect(useAgentListFacts.getState().sessions.s3).toMatchObject({
    face: { kind: "mark", text: "WO", color: "#64748b" },
  });
  expect((useAgentListFacts.getState().sessions.s1 as { searchText: string }).searchText).toContain(
    "Inbox sweep",
  );
  expect((useAgentListFacts.getState().sessions.s1 as { searchText: string }).searchText).toContain(
    "codex",
  );
  changed.mockClear();
  act(() =>
    useAgentListProjectionStore.setState({
      sessions: rows.map((r) => ({
        ...r,
        ...(useAgentListFacts.getState().sessions[r.id!] as object),
      })),
    }),
  );
  expect(changed).not.toHaveBeenCalled();
  act(() =>
    useViewerStore.setState({
      agentIdentity: { [TEAM]: { "worker-1": { role: "worker", face: "builtin-chess:king" } } },
    }),
  );
  await waitFor(() =>
    expect(useAgentListFacts.getState().sessions.s1).toMatchObject({
      rolePiece: { piece: "worker" },
      face: { src: "/kit/v1/avatars/builtin-chess/king.svg" },
    }),
  );
  hook.unmount();
  stop();
});

it("resolves saved pins by session after team placement changes, once per session", async () => {
  const row = {
    id: "moved",
    sessionId: "moved",
    handle: "worker",
    alive: true,
    kind: "headless",
    startedAtMs: 1,
  } as AgentSessionRow;
  useViewerStore.setState({
    stateLoaded: true,
    agentPins: {
      [TEAM]: [{ kind: "session", teamKey: TEAM, handle: "worker", sessionId: "moved" }],
      "acme:ops": [{ kind: "session", teamKey: "acme:ops", handle: "worker", sessionId: "moved" }],
    } as never,
  });
  useAgentListProjectionStore.setState({
    sessions: [row],
    references: [
      {
        id: "moved-ref",
        sessionRef: "moved",
        teamKey: "acme:ops",
        parentId: null,
        order: 0,
        workspaceRef: null,
      },
    ] as never,
  });
  renderHook(useAgentListFactsMirror);
  await waitFor(() => expect(useAgentListFacts.getState().pane.pinnedRefs).toEqual(["moved-ref"]));
  expect(useAgentListFacts.getState().sessions.moved).toMatchObject({ pinned: true });
});
