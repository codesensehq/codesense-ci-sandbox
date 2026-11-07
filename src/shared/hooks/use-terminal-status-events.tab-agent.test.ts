// SPDX-License-Identifier: MIT
/**
 * Seam test: a Claude running in a plain terminal tab dies without saying so
 * (a `kill -9` at the prompt, no `SessionEnd`, no command-end status). The real
 * status listener, the real liveness poller (its per-tab agent probe) and the
 * real stores must agree afterwards: the record no longer names a `claude`, the
 * dot and the record say the same thing, and a later command in that shell is
 * not taken for the agent's exit.
 */
import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const h = vi.hoisted(() => ({
  handlers: new Map<string, (e: { payload: unknown }) => void>(),
  claudeAlive: true,
  codexAlive: false,
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, cb: (e: { payload: unknown }) => void) => {
    h.handlers.set(name, cb);
    return () => h.handlers.delete(name);
  }),
}));
// This suite exercises legacy inputs; ActivityV1 recovery has its own seam tests.
vi.mock("@/shared/hooks/use-terminal-activity", () => ({ useTerminalActivity: () => {} }));
vi.mock("@/tauri/commands", () => ({
  sessionsHaveLiveAgent: vi.fn(async (queries: { ptyId: string; agentName: string }[]) =>
    queries.map((q) => ({
      ptyId: q.ptyId,
      alive:
        (q.agentName === "claude" && h.claudeAlive) || (q.agentName === "codex" && h.codexAlive),
    })),
  ),
  listAgentSignals: vi.fn(async () => [
    { basename: "claude", attention: "hooks", quietSecs: 10 },
    { basename: "codex", attention: "mcp", quietSecs: 10 },
  ]),
}));

import { getSessionAiStatus } from "@/features/viewer/lib/cli-workspace/store/terminal-tab-selectors";
import {
  LIVENESS_POLL_MS,
  resetLivenessPollerForTests,
} from "@/shared/hooks/use-agent-liveness-monitor";
import { useTerminalStatusEvents } from "@/shared/hooks/use-terminal-status-events";
import {
  resetPrunedAgentSessionsForTests,
  useAgentStateStore,
} from "@/shared/stores/agent-state-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { installTerminalLayoutHarness, seedTerminalTabs } from "@/test/terminal-layout-harness";

installTerminalLayoutHarness();

const S = "tab-1";

function emit(name: string, payload: unknown) {
  const cb = h.handlers.get(name);
  if (!cb) throw new Error(`no handler registered for ${name}`);
  act(() => cb({ payload }));
}
const hook = (hook_event: string, status: string) =>
  emit("terminal-status-update", { tab_session_id: S, status, hook_event });
const commandEnd = (exit_code: number) =>
  emit("pty-osc133", { session_id: S, kind: "command-end", exit_code });

const record = () => useAgentStateStore.getState().bySession[S];
const stored = () => useViewerStore.getState().aiStatusBySession[S];

async function ticks(n: number) {
  for (let i = 0; i < n; i++) {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(LIVENESS_POLL_MS);
    });
  }
}

let unmount: () => void = () => {};

beforeEach(async () => {
  resetLivenessPollerForTests();
  resetPrunedAgentSessionsForTests();
  vi.useFakeTimers();
  h.handlers.clear();
  h.claudeAlive = true;
  h.codexAlive = false;
  useAgentStateStore.setState({ bySession: {}, exitBySession: {} });
  useViewerStore.setState({
    aiStatusBySession: {},
    progressBySession: {},
    softClosedWorkspaces: {},
  });
  seedTerminalTabs("team", "ws", [{ session: S, name: "zsh" }]);
  unmount = renderHook(() => useTerminalStatusEvents()).unmount;
  // Let the agent list load and the tab set register.
  await act(async () => {});
});
afterEach(() => {
  unmount();
  resetLivenessPollerForTests();
  vi.useRealTimers();
});

describe("a Claude in a plain tab dies silently", () => {
  it("after a finished turn: the record lets go of claude, and the dot agrees with it", async () => {
    hook("UserPromptSubmit", "running");
    hook("Stop", "done");
    await ticks(1);
    expect(record()?.state).toBe("done");
    expect(record()?.agent).toBe("claude");
    expect(getSessionAiStatus(S)).toBe("done");

    // kill -9 at the prompt: nothing reports it but the tab's agent probe.
    h.claudeAlive = false;
    await ticks(1);
    expect(record()?.agent).toBeNull();
    expect(stored()).toBeUndefined();
    expect(getSessionAiStatus(S)).toBeUndefined();

    // A failing command in the same shell is the shell's, not the dead agent's.
    commandEnd(1);
    expect(record()?.state).toBe("done");
    expect(getSessionAiStatus(S)).toBeUndefined();
  });

  it("mid-turn, before any heartbeat saw it: the stale busy dot clears with the record", async () => {
    h.claudeAlive = false;
    hook("UserPromptSubmit", "running");
    expect(stored()).toBe("running");
    expect(getSessionAiStatus(S)).toBe("thinking");

    await ticks(1);
    expect(record()?.agent).toBeNull();
    expect(stored()).toBeUndefined();
    expect(getSessionAiStatus(S)).toBeUndefined();
  });

  it("seen running, then gone mid-turn: crashed on the record and on the dot", async () => {
    hook("UserPromptSubmit", "running");
    await ticks(1);
    h.claudeAlive = false;
    await ticks(1);
    expect(record()?.state).toBe("crashed");
    expect(stored()).toBe("error");
    expect(getSessionAiStatus(S)).toBe("crashed");
  });

  it("an MCP-only agent (no hooks) that finished, then left: the floor ends its record too", async () => {
    h.claudeAlive = false;
    h.codexAlive = true;
    await ticks(1);
    emit("terminal-status-update", {
      tab_session_id: S,
      status: "done",
      source: "mcp",
      agent_signal: { kind: "signal-done", outcome: "success" },
    });
    expect(record()?.agent).toBeNull();
    expect(getSessionAiStatus(S)).toBe("done");

    h.codexAlive = false;
    await ticks(1);
    expect(stored()).toBeUndefined();
    expect(getSessionAiStatus(S)).toBeUndefined();
  });
});
