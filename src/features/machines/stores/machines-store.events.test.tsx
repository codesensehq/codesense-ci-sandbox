// SPDX-License-Identifier: MIT
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { MachineSummary } from "@/tauri/commands";
import type { MachinesChangedPayload } from "@/tauri/events";
import { machineHandlers } from "@/app/mock/handlers/machines";
import { MOCK_ORG_SLUG, MOCK_TEAM_NAME } from "@/app/mock/fixtures";

// Seam: the real registry subscription and the real list reads (the mock
// backend's contract). Only the event channel is driven by hand.
const bus = vi.hoisted(() => ({
  handlers: [] as ((event: { payload: unknown }) => void)[],
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_name: string, handler: (event: { payload: unknown }) => void) => {
    bus.handlers.push(handler);
    return () => {
      bus.handlers = bus.handlers.filter((h) => h !== handler);
    };
  }),
}));

import {
  installMachinesBackend,
  resetMachinesUi,
  TEAM_KEY,
  ZERO_LATENCY,
} from "../__test-utils__/machines-backend";
import { MACHINES_CHANGED_COALESCE_MS, useTeamMachines } from "./machines-store";

const TEAM_SCOPE = { kind: "team" as const, orgSlug: MOCK_ORG_SLUG, team: MOCK_TEAM_NAME };

function emit(payload: MachinesChangedPayload) {
  for (const handler of bus.handlers) handler({ payload });
}

/** Mount a Machines consumer, let its first reads land, and count list reads
 *  from then on. */
async function mounted(extra: () => MachineSummary[] = () => []) {
  const base = machineHandlers("demo", ZERO_LATENCY);
  const { calls } = installMachinesBackend("demo", {
    list_machines: async (args) => [
      ...((await base.list_machines!(args)) as MachineSummary[]),
      ...extra(),
    ],
  });
  const hook = renderHook(() => useTeamMachines(TEAM_KEY));
  await act(() => vi.advanceTimersByTimeAsync(MACHINES_CHANGED_COALESCE_MS * 2));
  expect(hook.result.current).not.toBeNull();
  const reads = () => calls.filter((c) => c.cmd === "list_machines").length;
  const before = reads();
  return { hook, readsSince: () => reads() - before };
}

beforeEach(() => {
  vi.useFakeTimers();
  bus.handlers = [];
  resetMachinesUi();
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("machines-changed", () => {
  it("one connect's burst of events re-reads the team's list once", async () => {
    const { readsSince } = await mounted();
    // The save, then connecting, then connected — as one connect sends them.
    emit({ scope: TEAM_SCOPE, machineId: "devbox-01" });
    emit({ scope: TEAM_SCOPE, machineId: "devbox-01" });
    emit({ scope: { kind: "personal" }, machineId: "devbox-01" });
    emit({ scope: TEAM_SCOPE, machineId: "devbox-01" });
    await act(() => vi.advanceTimersByTimeAsync(MACHINES_CHANGED_COALESCE_MS * 2));
    expect(readsSince()).toBe(1);
  });

  it("a change in another team's layer re-reads nothing here", async () => {
    const { readsSince } = await mounted();
    emit({
      scope: { kind: "team", orgSlug: MOCK_ORG_SLUG, team: "elsewhere" },
      machineId: "x-123456",
    });
    await act(() => vi.advanceTimersByTimeAsync(MACHINES_CHANGED_COALESCE_MS * 2));
    expect(readsSince()).toBe(0);
  });

  it("a teammate's machine arriving through git shows without a reload", async () => {
    let arrived = false;
    const { hook } = await mounted(() =>
      arrived
        ? [
            {
              machineId: "arrived-123456",
              kind: "ssh",
              label: "Arrived",
              scope: TEAM_SCOPE,
            } as MachineSummary,
          ]
        : [],
    );
    expect(hook.result.current!.some((m) => m.machineId === "arrived-123456")).toBe(false);

    arrived = true;
    emit({ scope: TEAM_SCOPE, machineId: "arrived-123456" });
    await act(() => vi.advanceTimersByTimeAsync(MACHINES_CHANGED_COALESCE_MS * 2));

    expect(hook.result.current!.some((m) => m.machineId === "arrived-123456")).toBe(true);
  });
});
