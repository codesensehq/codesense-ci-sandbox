// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

const commands = vi.hoisted(() => ({
  reviewTerminalServerUpdate: vi.fn(),
  startTerminalServersUpdate: vi.fn(),
  terminalServersUpdateStatus: vi.fn(),
}));
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  ...commands,
}));

import { mockTerminalServer } from "@/app/mock/handlers/machines";
import type { MachineSummary, TerminalWorkspaceLayout } from "@/tauri/commands";

const shell = vi.hoisted(() => ({ refreshMachine: vi.fn(async () => undefined) }));
vi.mock("@/features/machines/stores/machines-store", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/features/machines/stores/machines-store")>()),
  refreshMachine: shell.refreshMachine,
}));

import {
  onMachineTerminalServerReplaced,
  promptMachineTerminalServerUpdate,
  resetMachineTerminalServerPromptForTests,
} from "@/features/shell/lib/machine-terminal-server";

import {
  agentOfTags,
  confirmedCount,
  outcomeCounts,
  resetUpdateAllRunForTests,
  resumeUpdateAllRun,
  reviewUpdateAll,
  startUpdateAll,
  updateAllEntries,
  useUpdateAllRun,
  type UpdateAllEntry,
  type UpdateAllReview,
} from "./update-all";

const entry = (id: string): UpdateAllEntry => ({ machine: machine(id), terminals: [] });
const remote = (sessions: number): UpdateAllReview => ({
  expected: { serverId: "s", epoch: "e", sessions },
});
const local = (sessionIds: string[]): UpdateAllReview => ({
  expected: { serverId: "s", epoch: "e", sessions: sessionIds.length, pid: 1, sessionIds },
});
void useUpdateAllRun;

function machine(id: string, relation: "older" | "same" = "older"): MachineSummary {
  return {
    machineId: id,
    kind: id === "local" ? "local" : "ssh",
    label: id === "local" ? "This Mac" : id,
    scope: null,
    declaration: null,
    binding: null,
    canPublish: true,
    status: { state: "connected", workspaces: 1 },
    attention: 0,
    capabilities: { agentCallbacks: true },
    terminalServer: mockTerminalServer(id === "local", { relation, upToDate: relation === "same" }),
  };
}

const workspace = (
  name: string,
  tabs: [string, Record<string, string>, "live" | "exited"][],
): TerminalWorkspaceLayout =>
  ({
    descriptor: { displayName: name, kind: "story" },
    tabs: tabs.map(([tabName, tags, state], i) => ({
      session: `${name}-${i}`,
      name: tabName,
      role: null,
      placement: "tab",
      state: state === "live" ? { kind: "live" } : { kind: "exited", exitCode: 0, signal: null },
      tags,
    })),
    lastActive: null,
    grid: { paned: [], preset: "columns", collapsed: false },
    nextOrdinal: 1,
  }) as unknown as TerminalWorkspaceLayout;

beforeEach(() => {
  for (const fn of Object.values(commands)) fn.mockReset();
  shell.refreshMachine.mockClear();
  resetUpdateAllRunForTests();
  resetMachineTerminalServerPromptForTests();
});

describe("Update all terminal servers (SW-AGR-19 PR-5)", () => {
  it("lists every older server, remote first and This Mac last, with the terminals and agents that end", () => {
    const entries = updateAllEntries(
      [machine("local"), machine("box"), machine("fresh", "same")],
      (id) =>
        id === "box"
          ? [
              [
                "acme|SW-1",
                workspace("SW-1", [
                  ["shell", {}, "live"],
                  ["Barb", { remoteAgentKey: "team/barb" }, "live"],
                  ["done", {}, "exited"],
                ]),
              ],
            ]
          : [
              [
                "acme|mac",
                workspace("Mac work", [["claude", { remoteLaunch: '"claude"' }, "live"]]),
              ],
            ],
      (session) => (session === "SW-1-1" ? "working" : undefined),
    );
    expect(entries.map((e) => e.machine.machineId)).toEqual(["box", "local"]);
    expect(entries[0].terminals).toEqual([
      { workspaceKey: "acme|SW-1", workspace: "SW-1", name: "shell" },
      {
        workspaceKey: "acme|SW-1",
        workspace: "SW-1",
        name: "Barb",
        agent: "team/barb",
        working: true,
      },
    ]);
    expect(entries[1].terminals[0].agent).toBe("claude");
    expect(agentOfTags({ launchAgentRevision: "team/mia@7" })).toBe("team/mia");
    expect(agentOfTags({ remoteLaunch: '"shell"' })).toBeUndefined();
  });

  it("reviews each machine: what its update binds to, or why not", async () => {
    commands.reviewTerminalServerUpdate.mockImplementation(async (id: string) => {
      if (id === "gone") throw { message: "connect the machine first" };
      return id === "local" ? local(["a", "b"]).expected : remote(3).expected;
    });
    const reviews = await reviewUpdateAll([entry("box"), entry("gone"), entry("local")]);
    expect(reviews.box).toEqual(remote(3));
    expect(reviews.gone).toEqual({ problem: "connect the machine first" });
    expect(reviews.local).toEqual(local(["a", "b"]));
    expect(confirmedCount(entry("box"), reviews.box)).toBe(3);
    expect(
      confirmedCount(
        { ...entry("x"), terminals: [{ workspaceKey: "k", workspace: "w", name: "t" }] },
        undefined,
      ),
    ).toBe(1);
  });

  it("starts the backend run with exactly what was reviewed, and follows it to the end", async () => {
    vi.useFakeTimers();
    try {
      const run = (running: boolean, outcomes: unknown[]) => ({
        opId: "op1",
        running,
        machines: ["box", "local"],
        current: running ? "local" : undefined,
        outcomes,
      });
      const boxDone = {
        machineId: "box",
        label: "box",
        kind: "updated",
        message: "box: 1 tab reopened.",
      };
      const macDone = {
        machineId: "local",
        label: "This Mac",
        kind: "skipped",
        message: "its sessions changed",
      };
      commands.startTerminalServersUpdate.mockResolvedValue(run(true, [boxDone]));
      commands.terminalServersUpdateStatus.mockResolvedValue(run(false, [boxDone, macDone]));
      const replaced: string[] = [];
      const stop = onMachineTerminalServerReplaced((id) => replaced.push(id));
      const started = await startUpdateAll([entry("box"), entry("gone"), entry("local")], (e) =>
        e.machine.machineId === "box"
          ? remote(1)
          : e.machine.machineId === "local"
            ? local(["a"])
            : { problem: "connect the machine first" },
      );
      // A machine that could not be reviewed is not sent.
      expect(commands.startTerminalServersUpdate).toHaveBeenCalledWith([
        { machineId: "box", expected: remote(1).expected },
        { machineId: "local", expected: local(["a"]).expected },
      ]);
      expect(started?.running).toBe(true);
      // Single machine updates wait while it runs.
      expect(
        await promptMachineTerminalServerUpdate({ machineId: "other", label: "other", count: 0 }),
      ).toMatchObject({ kind: "failed", repair: expect.stringContaining("Update all") });
      await vi.advanceTimersByTimeAsync(500);
      expect(commands.terminalServersUpdateStatus).toHaveBeenCalledWith("op1");
      stop();
      expect(shell.refreshMachine).toHaveBeenCalledWith("box");
      expect(shell.refreshMachine).toHaveBeenCalledWith("local");
      expect(replaced).toEqual(["box"]);
      expect(outcomeCounts([boxDone, macDone] as never)).toBe("1 updated, 1 skipped");
    } finally {
      vi.useRealTimers();
    }
  });

  it("starts nothing while a single machine's update is open, or when nothing was reviewed", async () => {
    expect(await startUpdateAll([entry("gone")], () => ({ problem: "x" }))).toBeNull();
    expect(commands.startTerminalServersUpdate).not.toHaveBeenCalled();
  });

  it("picks up a run going on in the app, never a finished one", async () => {
    commands.terminalServersUpdateStatus.mockResolvedValueOnce({
      opId: "op8",
      running: false,
      machines: ["box"],
      outcomes: [{ machineId: "box", label: "box", kind: "updated", message: "done" }],
    });
    await resumeUpdateAllRun();
    expect(shell.refreshMachine).not.toHaveBeenCalled();
    commands.terminalServersUpdateStatus.mockResolvedValue({
      opId: "op9",
      running: true,
      machines: ["box", "local"],
      current: "local",
      outcomes: [
        { machineId: "box", label: "box", kind: "failed", message: "disk full", oldStopped: true },
      ],
    });
    await resumeUpdateAllRun();
    expect(commands.terminalServersUpdateStatus).toHaveBeenCalledWith(null);
    expect(shell.refreshMachine).toHaveBeenCalledWith("box");
  });
});
