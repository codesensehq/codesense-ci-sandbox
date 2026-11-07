// SPDX-License-Identifier: MIT
import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { listeners, agentReadRoles } = vi.hoisted(() => ({
  listeners: new Set<() => void>(),
  agentReadRoles: vi.fn(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: (_name: string, fn: () => void) => {
    listeners.add(fn);
    return Promise.resolve(() => listeners.delete(fn));
  },
}));
vi.mock("@/tauri/commands", () => ({
  agentReadRoles: (...args: unknown[]) => agentReadRoles(...args),
  releaseRole: vi.fn(() => Promise.resolve(0)),
}));
vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn() }) }));

const { ROLE_READ_DEBOUNCE_MS, useRoleHoldings } = await import("./use-role-holdings");

/** One live registration of `role` by `instanceId`. */
const rolesOf = (role: string, instanceId: string) => ({
  roles: [
    {
      role,
      roleName: null,
      registrations: [{ instanceId, generation: 1, live: true }],
    },
  ],
});

beforeEach(() => {
  vi.useFakeTimers();
  listeners.clear();
  agentReadRoles.mockReset();
  agentReadRoles.mockImplementation((team: { org: string; team: string }) =>
    Promise.resolve(
      team.team === "ops" ? rolesOf("reviewer", "sid-ops") : rolesOf("builder", "sid-dev"),
    ),
  );
});
afterEach(() => vi.useRealTimers());

/** Lets the reads' promises settle. */
const settle = () => act(async () => {});

describe("useRoleHoldings over several teams", () => {
  it("reads every listed team and says what each team's sessions hold, in their own team", async () => {
    const { result } = renderHook(() => useRoleHoldings(["acme:ops", "acme:dev"], true));
    await settle();
    expect(agentReadRoles).toHaveBeenCalledTimes(2);
    const ops = result.current.holdsOf("acme:ops", { sessionId: "sid-ops", handle: "h" });
    expect(ops.map((h) => [h.role, h.teamKey])).toEqual([["reviewer", "acme:ops"]]);
    expect(result.current.holdsOf("acme:dev", { sessionId: "sid-dev", handle: "h" })).toHaveLength(
      1,
    );
    // A session of one team holds nothing in the other.
    expect(result.current.holdsOf("acme:dev", { sessionId: "sid-ops", handle: "h" })).toEqual([]);
  });

  it("merges the teams' answers into one update", async () => {
    let renders = 0;
    renderHook(() => {
      renders += 1;
      return useRoleHoldings(["acme:ops", "acme:dev"], true);
    });
    await settle();
    const before = renders;
    act(() => listeners.forEach((fn) => fn()));
    await act(async () => {
      vi.advanceTimersByTime(ROLE_READ_DEBOUNCE_MS);
    });
    await settle();
    // Both teams answered; the hook drew once for the pair, not once per team.
    expect(renders - before).toBe(1);
  });

  it("reads a burst of role-state changes once", async () => {
    renderHook(() => useRoleHoldings(["acme:ops", "acme:dev"], true));
    await settle();
    agentReadRoles.mockClear();
    act(() => {
      for (let i = 0; i < 5; i += 1) listeners.forEach((fn) => fn());
    });
    expect(agentReadRoles).not.toHaveBeenCalled();
    await act(async () => {
      vi.advanceTimersByTime(ROLE_READ_DEBOUNCE_MS);
    });
    expect(agentReadRoles).toHaveBeenCalledTimes(2);
  });

  it("drops a team that is no longer listed, and keeps the answer of one listed again only once read", async () => {
    const { result, rerender } = renderHook(
      ({ teams }: { teams: string[] }) => useRoleHoldings(teams, true),
      { initialProps: { teams: ["acme:ops", "acme:dev"] } },
    );
    await settle();
    rerender({ teams: ["acme:dev"] });
    await settle();
    expect(result.current.holdsOf("acme:ops", { sessionId: "sid-ops", handle: "h" })).toEqual([]);
    // Listed again, its holdings come from a fresh read, not the dropped one.
    agentReadRoles.mockImplementation((team: { team: string }) =>
      Promise.resolve(team.team === "ops" ? { roles: [] } : rolesOf("builder", "sid-dev")),
    );
    rerender({ teams: ["acme:dev", "acme:ops"] });
    await settle();
    expect(result.current.holdsOf("acme:ops", { sessionId: "sid-ops", handle: "h" })).toEqual([]);
  });
});
