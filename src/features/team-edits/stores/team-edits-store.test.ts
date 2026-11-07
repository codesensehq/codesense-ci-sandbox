// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { TeamFileEditsStatus } from "@/tauri/commands";

import { useTeamEditsStore } from "./team-edits-store";

const h = vi.hoisted(() => ({ status: vi.fn() }));
vi.mock("@/tauri/commands", () => ({ teamFileEditsStatus: h.status }));

const status = (total: number): TeamFileEditsStatus => ({
  total,
  cleanWorkspaces: 0,
  workspaces: [],
});

function deferred<T>() {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => (resolve = r));
  return { promise, resolve };
}

beforeEach(() => {
  useTeamEditsStore.setState({ byTeam: {} });
  h.status.mockReset();
});

describe("team edits store", () => {
  it("an older reply never overwrites a newer one; a stale machine count is read back", async () => {
    const machine = deferred<TeamFileEditsStatus>();
    const plain = deferred<TeamFileEditsStatus>();
    h.status
      .mockReturnValueOnce(machine.promise)
      .mockReturnValueOnce(plain.promise)
      .mockResolvedValueOnce(status(1));
    const store = useTeamEditsStore.getState();
    const target = { kind: "remote" as const, machineId: "pi", workspaceHandle: "h" };
    const first = store.refresh("t", target);
    const second = store.refresh("t");
    plain.resolve(status(2));
    await second;
    expect(useTeamEditsStore.getState().byTeam.t.status?.total).toBe(2);
    machine.resolve(status(9));
    await first;
    // The stale machine reply was dropped and the last counts read again.
    expect(h.status).toHaveBeenCalledTimes(3);
    expect(useTeamEditsStore.getState().byTeam.t.status?.total).toBe(1);
    expect(useTeamEditsStore.getState().byTeam.t.counting).toEqual([]);
    expect(useTeamEditsStore.getState().byTeam.t.loading).toBe(false);
  });

  it("a failed read keeps the last good status and says so", async () => {
    h.status.mockResolvedValueOnce(status(3)).mockRejectedValueOnce(new Error("store busy"));
    await useTeamEditsStore.getState().refresh("t");
    await useTeamEditsStore.getState().refresh("t");
    const t = useTeamEditsStore.getState().byTeam.t;
    expect(t.status?.total).toBe(3);
    expect(t.error).toContain("store busy");
  });

  it("refreshAll counts each connected machine workspace whose count is old, clean ones too", async () => {
    const machine = (handle: string, online: boolean, checkedAtMs: number) => ({
      target: { kind: "remote" as const, machineId: "pi", workspaceHandle: handle },
      name: handle,
      machineOnline: online,
      checkedAtMs,
    });
    const now = Date.now();
    h.status.mockResolvedValue({
      ...status(0),
      machineWorkspaces: [
        machine("old-clean", true, now - 5 * 60_000),
        machine("fresh", true, now - 1_000),
        machine("offline", false, now - 5 * 60_000),
      ],
    });
    await useTeamEditsStore.getState().refreshAll("t");
    const counted = h.status.mock.calls.map((c) => c[0]?.workspaceHandle).filter(Boolean);
    expect(counted).toEqual(["old-clean"]);
  });
});
