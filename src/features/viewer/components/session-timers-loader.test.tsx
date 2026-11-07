// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, waitFor } from "@/test-utils";

// The app-level loader: every agent tab's timers — shown, Hidden or in a
// minimized workspace, local or remote — and never a plain shell's.

const h = vi.hoisted(() => ({
  sessionTimers: vi.fn(),
  running: {} as Record<string, Record<string, unknown[]>>,
  minimized: {} as Record<string, Record<string, unknown[]>>,
  agents: new Map<string, unknown>(),
}));

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  sessionTimers: h.sessionTimers,
}));
vi.mock("@/features/viewer/lib/cli-workspace/store/terminal-selectors", async (importOriginal) => ({
  ...(await importOriginal<
    typeof import("@/features/viewer/lib/cli-workspace/store/terminal-selectors")
  >()),
  useRunningTerminalTabIndex: () => h.running,
  useMinimizedTerminalTabIndex: () => h.minimized,
}));
vi.mock("@/features/viewer/hooks/use-tab-timer-agent", () => ({
  useTabTimerAgents: () => h.agents,
  useTabTimerAgent: (sid: string) => h.agents.get(sid) ?? null,
}));

import { SessionTimersLoader } from "./session-timers-loader";
import {
  resetSessionTimersForTests,
  useSessionTimersStore,
} from "@/features/viewer/stores/session-timers-store";

const AGENT = { name: "Barb", agentKey: "acme/core/barb", saved: true, runtime: "claude" };

beforeEach(() => {
  resetSessionTimersForTests();
  h.sessionTimers.mockReset().mockResolvedValue([]);
  h.agents = new Map([
    ["s-shown", AGENT],
    ["s-hidden", AGENT],
    ["s-remote", AGENT],
    ["s-min", AGENT],
  ]);
  h.running = {
    "acme:core": {
      ws1: [
        { id: "a", sessionId: "s-shown", name: "barb" },
        { id: "b", sessionId: "s-shell", name: "zsh" },
        { id: "c", sessionId: "s-hidden", name: "hidden barb" },
        {
          id: "d",
          sessionId: "s-remote",
          name: "remote",
          origin: { machineId: "codesense-test-1", member: "m", handle: "h" },
        },
      ],
    },
  };
  h.minimized = { "acme:core": { ws2: [{ id: "e", sessionId: "s-min", name: "min" }] } };
});
afterEach(() => {
  cleanup();
  resetSessionTimersForTests();
});

describe("SessionTimersLoader", () => {
  it("reads every agent tab's timers once, local and remote, and never a plain shell's", async () => {
    render(<SessionTimersLoader />);
    await waitFor(() => expect(h.sessionTimers).toHaveBeenCalledTimes(4));
    const asked = h.sessionTimers.mock.calls.map(
      ([sid, machine]) => `${machine ?? "local"}|${sid}`,
    );
    expect(asked.sort()).toEqual(
      ["codesense-test-1|s-remote", "local|s-hidden", "local|s-min", "local|s-shown"].sort(),
    );
    expect(Object.keys(useSessionTimersStore.getState().bySession)).not.toContain("local|s-shell");
  });

  it("re-reads a remote tab when its machine reconnects", async () => {
    const view = render(<SessionTimersLoader />);
    await waitFor(() => expect(h.sessionTimers).toHaveBeenCalledTimes(4));
    const remote = h.running["acme:core"].ws1[3] as { origin: Record<string, unknown> };
    h.running = {
      "acme:core": {
        ws1: [
          ...h.running["acme:core"].ws1.slice(0, 3),
          { ...remote, origin: { ...remote.origin, offlineSince: 1 } },
        ],
      },
    };
    view.rerender(<SessionTimersLoader />);
    h.running = { "acme:core": { ws1: [...h.running["acme:core"].ws1.slice(0, 3), remote] } };
    h.sessionTimers.mockClear();
    view.rerender(<SessionTimersLoader />);
    await waitFor(() =>
      expect(h.sessionTimers).toHaveBeenCalledWith("s-remote", "codesense-test-1"),
    );
  });

  it("re-reads once a fire comes due, and does not re-arm for a fire already due", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const now = Date.now();
    h.agents = new Map([["s-shown", AGENT]]);
    h.running = { "acme:core": { ws1: [{ id: "a", sessionId: "s-shown", name: "barb" }] } };
    h.minimized = {};
    h.sessionTimers.mockResolvedValue([
      { timerId: "t1", nextAt: new Date(now + 60_000).toISOString(), name: "x", prompt: "x" },
    ]);
    render(<SessionTimersLoader />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(h.sessionTimers).toHaveBeenCalledTimes(1);
    // Due now, waiting for idle: the read after the server's tick sees it still due.
    h.sessionTimers.mockResolvedValue([
      { timerId: "t1", nextAt: new Date(now + 60_000).toISOString(), name: "x", prompt: "x" },
    ]);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(60_000 + 16_000);
    });
    expect(h.sessionTimers).toHaveBeenCalledTimes(2);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5 * 60_000);
    });
    expect(h.sessionTimers).toHaveBeenCalledTimes(2);
    vi.useRealTimers();
  });
});
