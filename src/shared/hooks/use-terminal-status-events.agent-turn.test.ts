// SPDX-License-Identifier: MIT
/**
 * Seam test for turn tracking: the `terminal-status-update` payloads the Rust
 * receiver emits for real Claude hook sequences (the golden file its own test
 * asserts against), replayed through the real status listener, the real
 * liveness poller and the real store.
 */
import { readFileSync } from "fs";
import { resolve } from "path";
import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

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
// This suite exercises legacy inputs; ActivityV1 recovery has its own seam tests.
vi.mock("@/shared/hooks/use-terminal-activity", () => ({ useTerminalActivity: () => {} }));
vi.mock("@/tauri/commands", () => ({
  sessionsHaveLiveAgent: vi.fn(async (queries: { ptyId: string }[]) =>
    queries.map((q) => ({ ptyId: q.ptyId, alive: h.alive })),
  ),
}));

import { sessionsHaveLiveAgent } from "@/tauri/commands";
import {
  LIVENESS_POLL_MS,
  resetLivenessPollerForTests,
  useAgentDotFloor,
} from "@/shared/hooks/use-agent-liveness-monitor";
import { useTerminalStatusEvents } from "@/shared/hooks/use-terminal-status-events";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { useViewerStore } from "@/shared/stores/viewer-store";

const probe = vi.mocked(sessionsHaveLiveAgent);

interface Step {
  hook: string;
  emitted: { tab_session_id: string; status: string; hook_event: string };
  agent_state: string;
}
const golden = JSON.parse(
  readFileSync(
    resolve(
      __dirname,
      "../../../src-tauri/src/http/routes/fixtures/claude-hooks/turns/turn-sequences.json",
    ),
    "utf-8",
  ),
) as { tab_session_id: string; turns: Record<string, Step[]> };
const SESSION = golden.tab_session_id;
const silentTurn = golden.turns["silent-turn"];
const toolTurn = golden.turns["tool-turn"];

function emit(name: string, payload: unknown) {
  const cb = h.handlers.get(name);
  if (!cb) throw new Error(`no handler registered for ${name}`);
  cb({ payload });
}

const agentState = (id = SESSION) => useAgentStateStore.getState().bySession[id]?.state;
const dot = () => useViewerStore.getState().aiStatusBySession[SESSION];

/** Let the liveness poller run `n` ticks. */
async function heartbeat(n: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(LIVENESS_POLL_MS * n);
  });
}

function mountSeam({ dotFloor = true, agentBasename = "claude" } = {}) {
  const hooks = [renderHook(() => useTerminalStatusEvents())];
  if (dotFloor) {
    hooks.push(
      renderHook(() => useAgentDotFloor({ active: true, sessionId: SESSION, agentBasename })),
    );
  }
  mounted.push(...hooks);
}
const mounted: { unmount: () => void }[] = [];

beforeEach(() => {
  resetLivenessPollerForTests();
  vi.useFakeTimers();
  h.handlers.clear();
  h.alive = true;
  useViewerStore.setState({ aiStatusBySession: {}, progressBySession: {} });
  useAgentStateStore.setState({ bySession: {}, exitBySession: {} });
  probe.mockClear();
});
afterEach(() => {
  for (const m of mounted.splice(0)) m.unmount();
  resetLivenessPollerForTests();
  vi.useRealTimers();
});

describe("agent turn tracking through the status seam", () => {
  it("replays each captured turn step by step: submit → thinking, tool → working, Stop → done", () => {
    mountSeam();
    for (const [name, steps] of Object.entries(golden.turns)) {
      for (const step of steps) {
        emit("terminal-status-update", step.emitted);
        expect(agentState(), `${name}: ${step.hook}`).toBe(step.agent_state);
        expect(dot(), `${name}: ${step.hook} dot`).toBe(step.emitted.status);
      }
    }
    expect(toolTurn.map((s) => s.agent_state)).toEqual([
      "thinking",
      "working",
      "thinking",
      "working",
      "thinking",
      "done",
    ]);
  });

  it("a long silent turn stays thinking under the heartbeat, distinct from done at the prompt", async () => {
    mountSeam();
    const [submit, stop] = silentTurn;

    emit("terminal-status-update", submit.emitted);
    await heartbeat(15); // a minute of silence: no hook, no tool
    expect(agentState()).toBe("thinking");
    expect(dot()).toBe("running");

    emit("terminal-status-update", stop.emitted);
    await heartbeat(15); // idle at the prompt, process still alive
    expect(agentState()).toBe("done");
    expect(dot()).toBe("done");
  });

  it("the process vanishing mid-turn with no SessionEnd reads crashed, and the floor keeps it", async () => {
    mountSeam();
    emit("terminal-status-update", silentTurn[0].emitted);
    await heartbeat(2);
    expect(agentState()).toBe("thinking");

    h.alive = false;
    await heartbeat(1);
    expect(agentState()).toBe("crashed");
    expect(dot()).toBe("error");
    await heartbeat(3);
    expect(dot()).toBe("error");
  });

  it("a Claude that ends with SessionEnd and is then gone clears, as before", async () => {
    mountSeam();
    for (const step of silentTurn) emit("terminal-status-update", step.emitted);
    emit("terminal-status-update", {
      tab_session_id: SESSION,
      status: "idle",
      hook_event: "SessionEnd",
    });
    h.alive = false;
    await heartbeat(2);
    expect(agentState()).toBe("idle");
    expect(dot()).toBeUndefined();
  });

  it("the heartbeat never overrides needs-permission or errored", async () => {
    mountSeam();
    for (const [hook_event, status, state] of [
      ["PermissionRequest", "needs-permission", "needs-permission"],
      ["StopFailure", "error", "errored"],
    ] as const) {
      emit("terminal-status-update", silentTurn[0].emitted);
      emit("terminal-status-update", { tab_session_id: SESSION, status, hook_event });
      await heartbeat(5);
      expect(agentState()).toBe(state);
      expect(dot()).toBe(status);
    }
  });

  it("the floor fills an empty dot but never replaces a hook-set one", async () => {
    mountSeam();
    await heartbeat(1);
    expect(dot()).toBe("running");
    // No agent declared itself, so the tick creates no record.
    expect(agentState()).toBeUndefined();

    emit("terminal-status-update", silentTurn[1].emitted);
    await heartbeat(3);
    expect(dot()).toBe("done");
  });

  it("a pty-exit ends the record for good: a later tick neither changes nor recreates it", async () => {
    mountSeam();
    emit("terminal-status-update", silentTurn[0].emitted);
    emit("pty-exit", { session_id: SESSION, exit_code: 0, signal: null, requested: false });
    expect(agentState()).toBe("done");
    expect(dot()).toBe("done");
    h.alive = true;
    await heartbeat(2);
    expect(agentState()).toBe("done");
    expect(dot()).toBe("done");

    emit("pty-exit", { session_id: "plain-shell", exit_code: 1, signal: null, requested: false });
    expect(agentState("plain-shell")).toBeUndefined();
  });

  it("a Claude in a plain CLI tab (no dot-floor registration) gets the heartbeat while its turn is open", async () => {
    const CLI = "shell-plain-cli";
    mountSeam({ dotFloor: false });
    emit("terminal-status-update", { ...silentTurn[0].emitted, tab_session_id: CLI });
    await heartbeat(3);
    expect(agentState(CLI)).toBe("thinking");
    expect(probe).toHaveBeenCalledWith([{ ptyId: CLI, agentName: "claude" }]);

    // `claude` is killed, the shell survives: no hook, no pty-exit.
    h.alive = false;
    await heartbeat(1);
    expect(agentState(CLI)).toBe("crashed");

    // The turn is closed: the session drops out of the probe.
    probe.mockClear();
    await heartbeat(3);
    expect(probe).not.toHaveBeenCalled();
  });

  it("a closed turn in a CLI tab is not probed", async () => {
    mountSeam({ dotFloor: false });
    for (const step of silentTurn) emit("terminal-status-update", step.emitted);
    await heartbeat(3);
    expect(probe).not.toHaveBeenCalled();
    expect(agentState()).toBe("done");
  });

  it("a PreToolUse/PostToolUse pair notifies the viewer store zero times when the dot is unchanged", () => {
    mountSeam();
    emit("terminal-status-update", toolTurn[0].emitted);
    const notify = vi.fn();
    const unsubscribe = useViewerStore.subscribe(notify);
    emit("terminal-status-update", toolTurn[1].emitted);
    emit("terminal-status-update", toolTurn[2].emitted);
    unsubscribe();
    expect(notify).not.toHaveBeenCalled();
    expect(agentState()).toBe("thinking");
  });

  it("a sibling PreToolUse keeps needs-permission on the dot and the record; PostToolUse clears it", () => {
    mountSeam();
    emit("terminal-status-update", toolTurn[0].emitted);
    emit("terminal-status-update", {
      tab_session_id: SESSION,
      status: "needs-permission",
      hook_event: "PermissionRequest",
    });
    emit("terminal-status-update", toolTurn[1].emitted); // PreToolUse
    expect(dot()).toBe("needs-permission");
    expect(agentState()).toBe("needs-permission");
    emit("terminal-status-update", toolTurn[2].emitted); // PostToolUse
    expect(dot()).toBe("running");
    expect(agentState()).toBe("thinking");
  });

  it("a probe result that lands after a newer hook is dropped; the next probe still ends the turn", async () => {
    mountSeam({ dotFloor: false });
    emit("terminal-status-update", silentTurn[0].emitted);

    // The previous `claude` exits and a new one submits while the probe is in
    // flight: the "gone" answers the old process, not the turn that just opened.
    probe.mockImplementationOnce(async (queries) => {
      emit("terminal-status-update", silentTurn[0].emitted);
      return queries.map((q) => ({ ptyId: q.ptyId, alive: false }));
    });
    await heartbeat(1);
    expect(probe).toHaveBeenCalledTimes(1);
    expect(agentState()).toBe("thinking");

    // A probe sent after that hook is current: gone ends the turn.
    h.alive = false;
    await heartbeat(1);
    expect(probe).toHaveBeenCalledTimes(2);
    expect(agentState()).toBe("done");
  });

  it("one session probed under two agent names: each answer goes to its own query, the open turn's name decides the record", async () => {
    // A `codex` dot-floor registration on the same tab as an open `claude` turn.
    mountSeam({ agentBasename: "codex" });
    emit("terminal-status-update", silentTurn[0].emitted);
    probe.mockImplementationOnce(async (queries) =>
      queries.map((q) => ({ ptyId: q.ptyId, alive: q.agentName === "claude" })),
    );

    await heartbeat(1);
    expect(probe).toHaveBeenCalledWith([
      { ptyId: SESSION, agentName: "codex" },
      { ptyId: SESSION, agentName: "claude" },
    ]);
    // `claude` is alive, so its turn stays open…
    expect(agentState()).toBe("thinking");
    // …while the `codex` registration's own answer (gone) clears its dot.
    expect(dot()).toBeUndefined();
  });

  it("a tick is skipped while the previous probe is still in flight", async () => {
    mountSeam({ dotFloor: false });
    emit("terminal-status-update", silentTurn[0].emitted);
    probe.mockImplementationOnce(() => new Promise(() => {}));

    await heartbeat(3);
    expect(probe).toHaveBeenCalledTimes(1);
  });

  it("an event without a known hook name leaves the agent record alone", () => {
    mountSeam();
    emit("terminal-status-update", { tab_session_id: SESSION, status: "running" });
    emit("terminal-status-update", {
      tab_session_id: SESSION,
      status: "running",
      hook_event: "Nope",
    });
    expect(agentState()).toBeUndefined();
    expect(dot()).toBe("running");
  });
});
