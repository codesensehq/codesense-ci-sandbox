// SPDX-License-Identifier: MIT
import { readFileSync } from "fs";
import { resolve } from "path";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, renderHook } from "@testing-library/react";

// The MCP explicit signals (`signal_done`, `signal_progress`, `ask_user`) reach
// the dot over the same `terminal-status-update` wire as the Claude hooks
// (`src-tauri/src/mcp/agent_dot.rs`). These payloads are exactly what that
// bridge emits (asserted on the Rust side); this drives them through the real
// listener into the store, and checks the liveness floor never down-ranks them.
const h = vi.hoisted(() => ({
  handlers: new Map<string, (e: { payload: unknown }) => void>(),
  alive: true,
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, cb: (e: { payload: unknown }) => void) => {
    h.handlers.set(name, cb);
    return () => h.handlers.delete(name);
  }),
}));
// The liveness poller's process probe: every queried agent is `h.alive`.
// This suite exercises other inputs; ActivityV1 recovery has its own seam tests.
vi.mock("@/shared/hooks/use-terminal-activity", () => ({ useTerminalActivity: () => {} }));
vi.mock("@/tauri/commands", () => ({
  sessionsHaveLiveAgent: vi.fn(async (queries: { ptyId: string; agentName: string }[]) =>
    queries.map((q) => ({ ptyId: q.ptyId, alive: h.alive })),
  ),
}));

import { useTerminalStatusEvents } from "@/shared/hooks/use-terminal-status-events";
import {
  floorDotStatus,
  LIVENESS_POLL_MS,
  resetLivenessPollerForTests,
} from "@/shared/hooks/use-agent-liveness-monitor";
import { sessionsHaveLiveAgent } from "@/tauri/commands";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import {
  agentStateToTerminalDot,
  explicitSignalState,
  resolveAgentState,
  SIGNAL_DONE_OUTCOMES,
} from "@/shared/lib/agent-state";
import { PHASES } from "@/shared/lib/roles";

function emitBridge(sessionId: string, status: string) {
  const cb = h.handlers.get("terminal-status-update");
  if (!cb) throw new Error("terminal-status-update listener not registered");
  cb({ payload: { tab_session_id: sessionId, status } });
}

const dot = (sessionId: string) => useViewerStore.getState().aiStatusBySession[sessionId];

beforeEach(() => {
  resetLivenessPollerForTests();
  h.handlers.clear();
  h.alive = true;
  vi.mocked(sessionsHaveLiveAgent).mockClear();
  useViewerStore.setState({ aiStatusBySession: {}, progressBySession: {} });
  useAgentStateStore.setState({ bySession: {}, exitBySession: {} });
});

afterEach(() => {
  resetLivenessPollerForTests();
  vi.useRealTimers();
});

/** Let the real liveness poller run one probe. */
async function probeTick() {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(LIVENESS_POLL_MS);
  });
}

interface GoldenStep {
  step: string;
  emitted: Record<string, unknown> | null;
  dot: string | undefined;
  agent_state: string;
}
const golden = JSON.parse(
  readFileSync(
    resolve(__dirname, "../../../src-tauri/src/mcp/fixtures/agent-dot-sequence.json"),
    "utf-8",
  ),
) as { tab_session_id: string; steps: GoldenStep[] };

const agentState = (sessionId: string) => useAgentStateStore.getState().bySession[sessionId]?.state;

describe("MCP signals → agent-state record (the Rust ledger's golden sequence)", () => {
  it("replays every emit through the real listener; suppressed hooks touch nothing", async () => {
    vi.useFakeTimers();
    renderHook(() => useTerminalStatusEvents());
    const sid = golden.tab_session_id;
    for (const step of golden.steps) {
      if (step.step.startsWith("liveness ")) {
        // The real poller probes the open turn; the process is alive or gone.
        h.alive = step.step === "liveness alive";
        await probeTick();
      } else if (step.emitted) {
        // A hook step with no emit was suppressed by the ledger: nothing reaches the FE.
        const cb = h.handlers.get("terminal-status-update")!;
        cb({ payload: step.emitted });
      }
      expect({ step: step.step, dot: dot(sid), state: agentState(sid) }).toEqual({
        step: step.step,
        dot: step.dot,
        state: step.agent_state,
      });
    }
  });

  it("a turn a hook opened is still probed after an MCP signal, and crashes when Claude vanishes", async () => {
    vi.useFakeTimers();
    renderHook(() => useTerminalStatusEvents());
    const cb = h.handlers.get("terminal-status-update")!;
    cb({
      payload: {
        tab_session_id: "cli",
        status: "running",
        hook_event: "UserPromptSubmit",
      },
    });
    cb({
      payload: {
        tab_session_id: "cli",
        status: "running",
        source: "mcp",
        agent_signal: { kind: "signal-progress", phase: "dev" },
      },
    });
    expect(agentState("cli")).toBe("working");
    await probeTick();
    expect(vi.mocked(sessionsHaveLiveAgent)).toHaveBeenLastCalledWith([
      { ptyId: "cli", agentName: "claude" },
    ]);
    expect(agentState("cli")).toBe("working");
    // Gone mid-turn with no `SessionEnd`: Claude died without announcing it.
    h.alive = false;
    await probeTick();
    expect(agentState("cli")).toBe("crashed");
    expect(dot("cli")).toBe("error");
  });

  it("an MCP-only record names no agent, so it is never probed", async () => {
    vi.useFakeTimers();
    renderHook(() => useTerminalStatusEvents());
    h.handlers.get("terminal-status-update")!({
      payload: {
        tab_session_id: "mcp-only",
        status: "running",
        source: "mcp",
        agent_signal: { kind: "ask-resolved" },
      },
    });
    await probeTick();
    expect(vi.mocked(sessionsHaveLiveAgent)).not.toHaveBeenCalled();
    expect(agentState("mcp-only")).toBe("working");
  });

  it("an off-shape MCP signal still sets the dot but never reaches the record", () => {
    renderHook(() => useTerminalStatusEvents());
    const cb = h.handlers.get("terminal-status-update")!;
    for (const agent_signal of [
      { kind: "signal-done", outcome: "bogus" },
      { kind: "signal-progress", phase: "nope" },
      { kind: "claude-hook", event: "Stop" },
      null,
    ]) {
      cb({ payload: { tab_session_id: "s1", status: "running", source: "mcp", agent_signal } });
    }
    expect(dot("s1")).toBe("running");
    expect(agentState("s1")).toBeUndefined();
  });
});

describe("MCP explicit signals → dot", () => {
  it("signal_done success/failed/needs-attention sets done/error/done", () => {
    renderHook(() => useTerminalStatusEvents());
    const expected = { success: "done", failed: "error", "needs-attention": "done" };
    for (const outcome of SIGNAL_DONE_OUTCOMES) {
      const wire = agentStateToTerminalDot(explicitSignalState({ kind: "signal-done", outcome }));
      expect(wire).toBe(expected[outcome]);
      emitBridge(`s-${outcome}`, wire!);
      expect(dot(`s-${outcome}`)).toBe(expected[outcome]);
    }
  });

  it("an open ask_user reads needs-permission and its resolution restores running", () => {
    useViewerStore.setState({ aiStatusBySession: { s1: "running" } });
    renderHook(() => useTerminalStatusEvents());
    emitBridge("s1", agentStateToTerminalDot(explicitSignalState({ kind: "ask-user" }))!);
    expect(dot("s1")).toBe("needs-permission");
    emitBridge("s1", agentStateToTerminalDot("working")!);
    expect(dot("s1")).toBe("running");
  });

  it("signal_progress blocked reads needs-permission; every other phase reads running", () => {
    renderHook(() => useTerminalStatusEvents());
    for (const phase of PHASES) {
      const wire = agentStateToTerminalDot(explicitSignalState({ kind: "signal-progress", phase }));
      emitBridge("s1", wire!);
      expect(dot("s1")).toBe(phase === "blocked" ? "needs-permission" : "running");
    }
  });

  it("a signal_done done is not overridden by a later liveness tick", () => {
    renderHook(() => useTerminalStatusEvents());
    emitBridge("s1", "done");
    // The alive floor keeps any set dot and only fills an empty one.
    expect(floorDotStatus(dot("s1"), true)).toBe("done");
    emitBridge("s2", "needs-permission");
    expect(floorDotStatus(dot("s2"), true)).toBe("needs-permission");
    // The same rule in the canonical resolver: a later alive tick never beats Tier-1.
    const done = { signal: { kind: "signal-done", outcome: "success" }, seq: 1 } as const;
    expect(resolveAgentState(done, { signal: { kind: "alive" }, seq: 2 }, null, null)).toBe("done");
  });
});
