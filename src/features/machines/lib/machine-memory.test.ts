// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it } from "vitest";

import { MOCK_ORG_SLUG, MOCK_TEAM_NAME } from "@/app/mock/fixtures";
import { useLocalProfileStore } from "@/shared/stores/local-profile-store";
import { EMPTY_TERMINAL_LAYOUT } from "@/shared/lib/terminal-layout-projection";
import type { TerminalLayoutProjection } from "@/shared/lib/terminal-layout-projection";
import type { MachineSummary } from "@/tauri/commands";

import {
  installMachinesBackend,
  resetMachinesUi,
  TEAM_KEY,
} from "../__test-utils__/machines-backend";
import { loadTeamMachines } from "../stores/machines-store";
import {
  __resetMachineMemoryForTest,
  censusOf,
  machineMemory,
  noteMachines,
  recordCensus,
  restoreMachineMemory,
  stampConnectedMachines,
  useMachineMemoryStore,
  type MachineCensus,
} from "./machine-memory";
import { startMachineMemory } from "./machine-memory-boot";
import { describeMachineStatus } from "./machine-status";

const OWNER = "41";
const STORAGE = `codesense.machine-memory.${OWNER}`;

function machine(id: string, over: Partial<MachineSummary> = {}): MachineSummary {
  return {
    machineId: id,
    kind: "ssh",
    label: id,
    scope: { kind: "team", orgSlug: MOCK_ORG_SLUG, team: MOCK_TEAM_NAME },
    declaration: {
      mode: "private-account",
      host: `${id}.int`,
      port: 22,
      jumpHost: null,
      defaultBaseDir: null,
      pinnedHostKeyFp: "SHA256:pin",
      pinnedServerId: null,
    },
    binding: {
      username: "quinn",
      keyPath: null,
      credentialRef: null,
      autoConnect: false,
      hidden: false,
    },
    canPublish: true,
    remoteMcpControl: false,
    status: { state: "idle" },
    attention: 0,
    capabilities: { agentCallbacks: false },
    ...over,
  } as MachineSummary;
}

function census(tabs: number, workspaces = ["auth-fix"]): MachineCensus {
  return { tabs, workspaces, at: 1 };
}

/** Relaunch as the profile, with what the last session saved. */
function relaunch(saved: string | null) {
  if (saved === null) localStorage.removeItem(STORAGE);
  else localStorage.setItem(STORAGE, saved);
  restoreMachineMemory(null);
  useLocalProfileStore.setState({
    status: {
      active: { principal: OWNER, displayName: "quinn", machineOwner: null },
      profiles: [],
      privateAuthority: null,
    },
  });
}

const failed = (id: string) =>
  machine(id, { status: { state: "connectionFailed", lastError: "Operation timed out" } });

startMachineMemory();

beforeEach(() => {
  resetMachinesUi();
  localStorage.clear();
  __resetMachineMemoryForTest();
  useLocalProfileStore.setState({ status: null });
});

describe("Nothing here connects a Machine", () => {
  it("loading a list with a Machine that had open tabs sends no connect: that is the backend's", async () => {
    const { calls } = installMachinesBackend("none", {
      list_machines: () => [
        machine("gpu-box", {
          binding: {
            username: "quinn",
            keyPath: null,
            credentialRef: null,
            autoConnect: false,
            hidden: false,
            reconnectAtLaunch: true,
          },
        }),
      ],
    });
    relaunch(JSON.stringify({ census: { "gpu-box": census(2) }, lastReached: {} }));
    await loadTeamMachines(TEAM_KEY);
    await new Promise((r) => setTimeout(r, 0));
    expect(calls.some((c) => c.cmd === "connect_machine")).toBe(false);
  });
});

describe("Offline since…", () => {
  it("is when the Machine was last reached, kept across a relaunch", () => {
    relaunch(null);
    noteMachines([machine("gpu-box", { status: { state: "connected", workspaces: 1 } })], 1_000);
    noteMachines([failed("gpu-box")], 5_000);
    expect(machineMemory("gpu-box").offlineSince).toBe(5_000);

    relaunch(localStorage.getItem(STORAGE));
    noteMachines([failed("gpu-box")], 90_000);
    expect(machineMemory("gpu-box").offlineSince).toBe(5_000);
    const view = describeMachineStatus(failed("gpu-box"), 90_000);
    expect(view.word).toBe("Offline");
    expect(view.detail).toMatch(/^since /);
    expect(view.tone).toBe("muted");
    expect(view.actions.map((a) => a.id)).toContain("retry");
  });

  it("a Machine still connected when the app goes away was reached until then", () => {
    relaunch(null);
    noteMachines([machine("gpu-box", { status: { state: "connected", workspaces: 1 } })], 1_000);
    stampConnectedMachines(7_000);
    relaunch(localStorage.getItem(STORAGE));
    expect(machineMemory("gpu-box").offlineSince).toBe(7_000);
  });

  it("with no last-reached time, is when this launch first saw it unreachable", () => {
    relaunch(null);
    noteMachines([failed("gpu-box")], 3_000);
    noteMachines([failed("gpu-box")], 9_000);
    expect(machineMemory("gpu-box").offlineSince).toBe(3_000);
  });

  it("applies to an unreachable Machine, from the backend's last-reached time", () => {
    const view = describeMachineStatus(
      machine("gpu-box", {
        status: { state: "unreachable", lastReachedAtMs: 4_000, attempt: 2, retryInSecs: 15 },
      }),
      60_000,
      {},
    );
    expect(view.word).toBe("Offline");
    expect(view.detail).toMatch(/^since /);
  });
});

describe("The census", () => {
  const remote = (machineId: string) => ({ machineId, slot: "remote" as const });
  function layout(
    machineId: string,
    workspaces: Record<string, string[]>,
    state: "live" | "removed" = "live",
    offlineSince?: number,
  ): TerminalLayoutProjection {
    const key = `${machineId}:remote`;
    return {
      ...EMPTY_TERMINAL_LAYOUT,
      admission: {
        [key]: {
          member: remote(machineId),
          revision: "1",
          state,
          namespace: state === "live" ? { serverId: "s", epoch: "e", generation: "1" } : null,
          historical: null,
        } as never,
      },
      members: {
        [key]: {
          target: {
            member: remote(machineId),
            admissionRevision: "1",
            namespace: { serverId: "s", epoch: "e", generation: "1" },
          },
          rev: 1,
          workspaces: Object.fromEntries(
            Object.entries(workspaces).map(([name, tabs]) => [
              `${TEAM_KEY}|${name}`,
              {
                descriptor: { displayName: name, kind: "root" },
                tabs: tabs.map((session) => ({ session, name: session })),
                lastActive: null,
                grid: {},
                nextOrdinal: 1,
              },
            ]),
          ),
          active: {},
          dismissed: [],
          offlineSince,
        } as never,
      },
    };
  }

  it("counts a live Machine's tabs and workspaces, shows them while connecting, and is kept", () => {
    relaunch(null);
    recordCensus(layout("gpu-box", { "auth-fix": ["t1", "t2"], "api-v2": ["t3"], idle: [] }), 5);
    const expected = { tabs: 3, workspaces: ["api-v2", "auth-fix"], at: 5 };
    expect(useMachineMemoryStore.getState().census["gpu-box"]).toEqual(expected);
    relaunch(localStorage.getItem(STORAGE));
    const view = describeMachineStatus(machine("gpu-box", { status: { state: "connecting" } }), 0);
    expect(view.word).toBe("Connecting");
    expect(view.detail).toBe("last seen with 3 tabs in 2 workspaces");
  });

  it("an offline Machine keeps its last census; a removed one is forgotten", () => {
    relaunch(JSON.stringify({ census: { "gpu-box": census(2) }, lastReached: { "gpu-box": 9 } }));
    expect(censusOf(layout("gpu-box", {}, "live", 10), 5)).toEqual({});
    recordCensus(layout("gpu-box", { a: ["t1"] }, "removed"), 5);
    expect(useMachineMemoryStore.getState().census["gpu-box"]).toBeUndefined();
  });

  it.each([
    ["missing", null],
    ["not JSON", "{not json"],
    ["the wrong shape", JSON.stringify({ census: ["x"], lastReached: { a: "soon" } })],
    ["entries of the wrong shape", JSON.stringify({ census: { a: { tabs: "2" } } })],
  ])("storage that is %s says less, and breaks nothing", (_what, saved) => {
    relaunch(saved);
    expect(useMachineMemoryStore.getState().census).toEqual({});
    expect(useMachineMemoryStore.getState().lastReached).toEqual({});
    const view = describeMachineStatus(machine("a", { status: { state: "connecting" } }), 0);
    expect(view.detail).toBeNull();
  });

  it("blocked storage says less, and breaks nothing", () => {
    const original = Storage.prototype.getItem;
    Storage.prototype.getItem = () => {
      throw new Error("blocked");
    };
    try {
      relaunch(null);
      expect(useMachineMemoryStore.getState().census).toEqual({});
      recordCensus(layout("gpu-box", { a: ["t1"] }), 5);
      expect(useMachineMemoryStore.getState().census["gpu-box"]?.tabs).toBe(1);
    } finally {
      Storage.prototype.getItem = original;
    }
  });
});
