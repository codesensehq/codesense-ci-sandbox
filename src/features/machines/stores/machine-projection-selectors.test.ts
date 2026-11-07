// SPDX-License-Identifier: MIT
import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  listMachines: vi.fn(),
  loadAppState: vi.fn(),
  saveAppState: vi.fn(),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
}));
import {
  listMachines,
  loadAppState,
  saveAppState,
  type MachineSummary,
  type RuntimeLayoutReply,
} from "@/tauri/commands";
import {
  listenForPrivateAuthorityRetirement,
  observePrivateAuthority,
  retirePrivateAuthority,
} from "@/shared/lib/private-terminal-authority";
import { EMPTY_TERMINAL_LAYOUT, sessionTarget } from "@/shared/lib/terminal-layout-projection";
import { workspaceKey } from "@/shared/lib/workspace-key";
import { useViewerStore } from "@/shared/stores/viewer-store";
import {
  __resetMachineLoadsForTest,
  getTeamMachines,
  invalidateTeamMachines,
  loadTeamMachines,
  useMachinesStore,
  useTeamMachines,
} from "./machines-store";
import {
  machineSessionTarget,
  machineTargets,
  machineWorkspace,
} from "./machine-projection-selectors";

const A = "acme:a",
  B = "acme:b";
const WA = workspaceKey(A, { id: "same" }),
  WB = workspaceKey(B, { id: "same" });
let epoch = 0;
let disk: string;
function machine(id: string): MachineSummary {
  return {
    machineId: id,
    kind: id === "local" ? "local" : "ssh",
    label: id,
    scope: id === "local" ? null : { kind: "personal" },
    declaration: null,
    binding: null,
    canPublish: true,
    status: { state: "connected", workspaces: 1 },
    attention: 0,
    capabilities: { agentCallbacks: false },
  };
}
function reply(id: string, revision = "1"): Extract<RuntimeLayoutReply, { mode: "live" }> {
  const member =
    id === "local"
      ? { machineId: "local" as const, slot: "inProcess" as const }
      : { machineId: id, slot: "remote" as const };
  return {
    mode: "live",
    member,
    admissionRevision: revision,
    privateAuthorityEpoch: id === "local" ? undefined : String(epoch),
    namespace: { serverId: "colliding-server", epoch: "same-epoch", generation: revision },
    snapshot: {
      schema: 1,
      serverId: "colliding-server",
      serverName: id,
      epoch: "same-epoch",
      rev: 1,
      workspaces: Object.fromEntries(
        [
          [WA, "a"],
          [WB, "b"],
        ].map(([key, team]) => [
          key,
          {
            descriptor: {
              displayName: `private-${id}`,
              kind: "generic",
              org: "acme",
              team,
              worktreePath: `/private/${id}`,
            },
            tabs: [
              {
                session: "same-session",
                name: `private-${id}`,
                role: null,
                placement: "tab",
                state: { kind: "live" },
                tags: {},
              },
            ],
            lastActive: "same-session",
            grid: { paned: [], preset: "columns", collapsed: false },
            nextOrdinal: 2,
          },
        ]),
      ),
      viewer: { active: {}, views: {}, hidden: [] },
    },
  };
}
beforeEach(async () => {
  await listenForPrivateAuthorityRetirement();
  observePrivateAuthority({ epoch: String(++epoch), available: true });
  __resetMachineLoadsForTest();
  useMachinesStore.setState({ byTeam: {} });
  useViewerStore.setState({
    terminalLayout: EMPTY_TERMINAL_LAYOUT,
    openedTeams: [],
    workspaces: {},
    stateLoaded: true,
  });
  disk = "{}";
  vi.mocked(loadAppState).mockImplementation(async () => disk);
  vi.mocked(saveAppState).mockImplementation(async (value) => {
    disk = value;
  });
  vi.mocked(listMachines).mockImplementation(async (_org, team) => [
    machine("local"),
    machine(team === "a" ? "host-a" : "host-b"),
  ]);
});

it("composes colliding origins, edits, replacement and access loss without local fallback", async () => {
  await loadTeamMachines(A);
  await loadTeamMachines(B);
  const store = useViewerStore.getState();
  const local = reply("local"),
    a = reply("host-a"),
    b = reply("host-b");
  store.applyLayout({ kind: "snapshot", replies: [local, a, b] });
  expect(sessionTarget(useViewerStore.getState().terminalLayout, "same-session")).toBeUndefined();
  expect(machineSessionTarget(A, a, WA, "same-session")?.member.machineId).toBe("host-a");
  expect(machineWorkspace(B, a, WB)).toBeUndefined(); // Visible to A, never B, even with a valid B grouping.
  expect(machineWorkspace(A, a, WB)).toBeUndefined();
  expect(machineWorkspace(A, b, WA)).toBeUndefined();
  useMachinesStore.getState().apply({
    ...machine("host-a"),
    label: "renamed",
    declaration: {
      host: "new-address",
      port: 22,
      jumpHost: null,
      defaultBaseDir: null,
      pinnedHostKeyFp: null,
      pinnedServerId: null,
    },
  });
  // A renamed / re-addressed declaration keeps the same admitted origin.
  expect(machineTargets(A, "host-a")).toEqual([
    { member: a.member, admissionRevision: "1", namespace: a.namespace },
  ]);
  const replacement = reply("host-a", "2");
  store.applyLayout({ kind: "snapshot", replies: [local, replacement, b] });
  expect(machineWorkspace(A, a, WA)).toBeUndefined();
  expect(machineWorkspace(A, replacement, WA)?.descriptor.worktreePath).toBe("/private/host-a");
  vi.mocked(listMachines).mockResolvedValue([machine("local")]);
  await loadTeamMachines(A);
  expect(machineTargets(A, "host-a")).toEqual([]);
  expect(machineWorkspace(A, replacement, WA)).toBeUndefined();
  expect(machineWorkspace(A, local, WA)?.tabs[0].session).toBe("same-session");
  expect(useViewerStore.getState().terminalLayout.byKey[WA].tabs[0].id).toBe("same-session");
  expect(machineSessionTarget(A, replacement, WA, "same-session")).toBeUndefined();
});

it("requires current private epochs on both admission and cached member", async () => {
  await loadTeamMachines(A);
  const a = reply("host-a");
  useViewerStore.getState().applyLayout({ kind: "snapshot", replies: [a] });
  expect(machineWorkspace(A, a, WA)).toBeDefined();
  retirePrivateAuthority({ epoch: String(epoch), available: false });
  expect(machineWorkspace(A, a, WA)).toBeUndefined();
  expect(machineTargets(A, "host-a")).toEqual([]);
  useMachinesStore.setState({ byTeam: { [A]: [machine("host-a")] } });
  expect(machineWorkspace(A, a, WA)).toBeUndefined();
});

it("refuses unadmitted, offline, and authority-less remote output", async () => {
  await loadTeamMachines(A);
  expect(machineTargets(A, "host-a")).toEqual([]);
  const a = reply("host-a");
  delete a.privateAuthorityEpoch;
  useViewerStore.getState().applyLayout({ kind: "snapshot", replies: [a] });
  expect(machineTargets(A, "host-a")).toEqual([]);
  useViewerStore.getState().applyLayout({ kind: "snapshot", replies: [reply("host-a", "2")] });
  useMachinesStore.getState().apply({ ...machine("host-a"), status: { state: "connecting" } });
  expect(machineTargets(A, "host-a")).toEqual([]);
});

it.each(["retire", "close", "rename", "invalidate"])(
  "rejects a late registry completion after %s",
  async (reason) => {
    let finish!: (rows: MachineSummary[]) => void;
    vi.mocked(listMachines).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    const pending = loadTeamMachines(A);
    if (reason === "retire") retirePrivateAuthority({ epoch: String(epoch), available: false });
    if (reason === "close") useViewerStore.getState().closeTeam(A);
    if (reason === "rename") useViewerStore.getState().rekeyTeam("acme", "a", "renamed");
    if (reason === "invalidate") invalidateTeamMachines(A);
    finish([machine("local"), machine("host-a")]);
    expect(await pending).toBe(false);
    expect(getTeamMachines(A)).toBeNull();
  },
);

it("keeps a newer registry response when an earlier refresh finishes last", async () => {
  let finish!: (rows: MachineSummary[]) => void;
  vi.mocked(listMachines).mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const pending = loadTeamMachines(A);
  vi.mocked(listMachines).mockResolvedValueOnce([machine("local")]);
  await loadTeamMachines(A);
  finish([machine("local"), machine("host-a")]);
  expect(await pending).toBe(false);
  expect(getTeamMachines(A)?.map((m) => m.machineId)).toEqual(["local"]);
});

it("a failed visibility refresh immediately denies cached runtime data", async () => {
  await loadTeamMachines(A);
  const a = reply("host-a");
  useViewerStore.getState().applyLayout({ kind: "snapshot", replies: [a] });
  expect(machineWorkspace(A, a, WA)).toBeDefined();
  vi.mocked(listMachines).mockRejectedValueOnce(new Error("no access"));
  expect(await loadTeamMachines(A)).toBe(false);
  expect(machineWorkspace(A, a, WA)).toBeUndefined();
  expect(machineTargets(A, "host-a")).toEqual([]);
});

it("filters a native-shaped team-A-only inventory row returned to team B, even with an admitted B workspace", async () => {
  const a = reply("host-a");
  const scoped: MachineSummary = {
    ...machine("host-a"),
    scope: { kind: "team", orgSlug: "acme", team: "a" },
  };
  vi.mocked(listMachines).mockResolvedValue([machine("local"), scoped]);
  await loadTeamMachines(A);
  await loadTeamMachines(B);
  useViewerStore.getState().applyLayout({ kind: "snapshot", replies: [a] });
  expect(machineWorkspace(A, a, WA)).toBeDefined();
  expect(getTeamMachines(B)?.map((row) => row.machineId)).toEqual(["local"]);
  expect(machineWorkspace(B, a, WB)).toBeUndefined();
  expect(machineSessionTarget(B, a, WB, "same-session")).toBeUndefined();
  // The selector is a separate fence even if stale/unfiltered cache data is supplied.
  useMachinesStore.setState({ byTeam: { [B]: [scoped] } });
  expect(machineWorkspace(B, a, WB)).toBeUndefined();
});

it("scope narrowing removes a personal machine from other teams and gates its cached sessions", async () => {
  const a = reply("host-a");
  vi.mocked(listMachines).mockResolvedValue([
    machine("local"),
    { ...machine("host-a"), scope: { kind: "personal" } },
  ]);
  await loadTeamMachines(A);
  await loadTeamMachines(B);
  useViewerStore.getState().applyLayout({ kind: "snapshot", replies: [a] });
  expect(machineWorkspace(B, a, WB)).toBeDefined();
  let finish!: (rows: MachineSummary[]) => void;
  vi.mocked(listMachines).mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const pending = loadTeamMachines(B);
  useMachinesStore
    .getState()
    .apply({ ...machine("host-a"), scope: { kind: "team", orgSlug: "acme", team: "a" } });
  expect(getTeamMachines(B)?.map((row) => row.machineId)).toEqual(["local"]);
  expect(machineWorkspace(B, a, WB)).toBeUndefined();
  expect(machineSessionTarget(B, a, WB, "same-session")).toBeUndefined();
  expect(machineWorkspace(A, a, WA)).toBeDefined();
  finish([machine("local"), machine("host-a")]);
  expect(await pending).toBe(false);
  expect(machineWorkspace(B, a, WB)).toBeUndefined();
});

it("reloads a mounted initial registry read after authority retirement and rejects its old reply", async () => {
  let finishOld!: (rows: MachineSummary[]) => void;
  let finishSetup!: (rows: MachineSummary[]) => void;
  let finishNew!: (rows: MachineSummary[]) => void;
  vi.mocked(listMachines)
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finishOld = resolve;
        }),
    )
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finishSetup = resolve;
        }),
    )
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finishNew = resolve;
        }),
    );
  const { result, unmount } = renderHook(() => useTeamMachines(A));
  expect(result.current).toBeNull();
  await waitFor(() => expect(finishSetup).toBeDefined());
  await act(async () => {
    retirePrivateAuthority({ epoch: String(epoch), available: false });
    observePrivateAuthority({ epoch: String(++epoch), available: true });
  });
  await waitFor(() => expect(finishNew).toBeDefined());
  await act(async () => {
    finishOld([machine("old-owner")]);
    finishSetup([machine("old-owner")]);
  });
  expect(result.current).toBeNull();
  await act(async () => finishNew([machine("new-owner")]));
  expect(result.current?.map((row) => row.machineId)).toEqual(["new-owner"]);
  unmount();
});
