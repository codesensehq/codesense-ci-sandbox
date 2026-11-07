// SPDX-License-Identifier: MIT
/**
 * Seam test for done-vs-crashed: the `pty-exit` payloads the Rust side emits
 * for REAL children (the golden its cross-host exit test asserts, in-process and
 * through the session-server sidecar), and the shell's command-end status for an
 * agent run inside a tab's shell, replayed through the real status listener, the
 * real liveness poller and the real stores — asserting the agent record, the
 * dot and the per-session exit.
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

import {
  LIVENESS_POLL_MS,
  resetLivenessPollerForTests,
  useAgentDotFloor,
} from "@/shared/hooks/use-agent-liveness-monitor";
import { useTerminalStatusEvents } from "@/shared/hooks/use-terminal-status-events";
import {
  pruneAgentSessions,
  retireAgentSession,
  snapshotAgentSession,
  resetPrunedAgentSessionsForTests,
  selectSessionExit,
  useAgentStateStore,
} from "@/shared/stores/agent-state-store";
import { useViewerStore } from "@/shared/stores/viewer-store";

interface PtyExitPayload {
  session_id: string;
  exit_code: number | null;
  signal: string | null;
  requested: boolean;
}
const golden = JSON.parse(
  readFileSync(
    resolve(__dirname, "../../../src-tauri/src/fixtures/agent-state/pty-exit-payloads.json"),
    "utf-8",
  ),
) as { payloads: Record<"clean" | "failed" | "sigkill" | "stopped", PtyExitPayload> };

function emit(name: string, payload: unknown) {
  const cb = h.handlers.get(name);
  if (!cb) throw new Error(`no handler registered for ${name}`);
  act(() => cb({ payload }));
}

function hook(session: string, hook_event: string, status: string) {
  emit("terminal-status-update", { tab_session_id: session, status, hook_event });
}
const submit = (s: string) => hook(s, "UserPromptSubmit", "running");
const tool = (s: string) => hook(s, "PreToolUse", "running");
const stop = (s: string) => hook(s, "Stop", "done");
const sessionEnd = (s: string) => hook(s, "SessionEnd", "idle");
function mcp(session: string, agent_signal: unknown, status: string) {
  emit("terminal-status-update", { tab_session_id: session, status, source: "mcp", agent_signal });
}
/** A raw `pty-exit` for a shell exiting with `code`. */
function shellExit(session: string, code: number) {
  emit("pty-exit", { session_id: session, exit_code: code, signal: null, requested: false });
}

/** The golden payload for a real child's end, delivered as `session`'s exit. */
function ptyExit(session: string, which: keyof typeof golden.payloads) {
  emit("pty-exit", { ...golden.payloads[which], session_id: session });
}
function commandEnd(session: string, exit_code: number) {
  emit("pty-osc133", { session_id: session, kind: "command-end", exit_code });
}

const state = (s: string) => useAgentStateStore.getState().bySession[s]?.state;
const exitOf = (s: string) => selectSessionExit(s)(useAgentStateStore.getState());
const dot = (s: string) => useViewerStore.getState().aiStatusBySession[s];

async function heartbeat(n: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(LIVENESS_POLL_MS * n);
  });
}

const mounted: { unmount: () => void }[] = [];
function mount(floorFor?: string) {
  mounted.push(renderHook(() => useTerminalStatusEvents()));
  if (floorFor) {
    mounted.push(
      renderHook(() =>
        useAgentDotFloor({ active: true, sessionId: floorFor, agentBasename: "claude" }),
      ),
    );
  }
}

beforeEach(() => {
  resetLivenessPollerForTests();
  vi.useFakeTimers();
  h.handlers.clear();
  h.alive = true;
  useViewerStore.setState({ aiStatusBySession: {}, progressBySession: {} });
  useAgentStateStore.setState({ bySession: {}, exitBySession: {} });
  resetPrunedAgentSessionsForTests();
});
afterEach(() => {
  vi.restoreAllMocks();
  for (const m of mounted.splice(0)) m.unmount();
  resetLivenessPollerForTests();
  vi.useRealTimers();
});

describe("an agent session's own process exits (pty-exit)", () => {
  it("exit 0 resolves done; the dot reads done and the floor keeps it", async () => {
    mount("a");
    submit("a");
    tool("a");
    ptyExit("a", "clean");
    expect(state("a")).toBe("done");
    expect(dot("a")).toBe("done");
    h.alive = false;
    await heartbeat(2);
    expect(dot("a")).toBe("done");
  });

  it("a nonzero exit resolves crashed; the dot reads error and the floor keeps it", async () => {
    mount("a");
    submit("a");
    ptyExit("a", "failed");
    expect(state("a")).toBe("crashed");
    expect(dot("a")).toBe("error");
    h.alive = false;
    await heartbeat(2);
    expect(dot("a")).toBe("error");
  });

  it("a kill -9 mid-turn resolves crashed — not done, not a lingering working", async () => {
    mount("a");
    submit("a");
    tool("a");
    expect(state("a")).toBe("working");
    ptyExit("a", "sigkill");
    expect(state("a")).toBe("crashed");
    expect(dot("a")).toBe("error");
    // A tick that still reports the process alive (a stale probe) cannot revive it.
    await heartbeat(2);
    expect(state("a")).toBe("crashed");
  });

  it("after Stop the shell's own death is not the agent's: it stays done and the dot clears", () => {
    mount();
    submit("a");
    stop("a");
    ptyExit("a", "sigkill");
    expect(state("a")).toBe("done");
    expect(dot("a")).toBeUndefined();
    expect(exitOf("a")?.outcome).toBe("crashed");
  });

  it("CodeSense's own kill of a settled or running agent reads done, never crashed", () => {
    mount();
    submit("a");
    stop("a");
    ptyExit("a", "stopped");
    expect(state("a")).toBe("done");
    expect(dot("a")).toBeUndefined();

    submit("b");
    tool("b");
    ptyExit("b", "stopped");
    expect(state("b")).toBe("done");
  });

  it("SessionEnd then a clean exit stays idle and clears the dot", () => {
    mount();
    submit("a");
    stop("a");
    sessionEnd("a");
    ptyExit("a", "clean");
    expect(state("a")).toBe("idle");
    expect(dot("a")).toBeUndefined();
  });

  it("errored (declared, still alive) is distinct from crashed (died)", () => {
    mount();
    submit("a");
    hook("a", "StopFailure", "error");
    expect(state("a")).toBe("errored");
    // The declared failure settled the turn: the shell dying later is not a crash.
    ptyExit("a", "sigkill");
    expect(state("a")).toBe("errored");

    submit("b");
    ptyExit("b", "sigkill");
    expect(state("b")).toBe("crashed");
  });
});

describe("a finished agent's shell exiting is not the agent crashing", () => {
  it("an MCP-only agent (signal_done) then `false; exit` (1) is not crashed", () => {
    mount();
    mcp("codex", { kind: "signal-progress", phase: "dev" }, "running");
    mcp("codex", { kind: "signal-done", outcome: "success" }, "done");
    expect(state("codex")).toBe("done");
    commandEnd("codex", 1); // `false`
    shellExit("codex", 1); // `exit` carries the last status
    expect(state("codex")).toBe("done");
    expect(dot("codex")).toBeUndefined();
    expect(exitOf("codex")).toMatchObject({ code: 1, outcome: "done" });
  });

  it("codex quit with Ctrl-C (130), then Ctrl-D (exit 130) is not crashed", () => {
    mount();
    mcp("codex", { kind: "signal-progress", phase: "dev" }, "running");
    commandEnd("codex", 130); // codex ends on Ctrl-C
    shellExit("codex", 130); // Ctrl-D: the shell exits with its last status
    expect(state("codex")).toBe("working");
    expect(dot("codex")).toBeUndefined();
  });

  it("Claude after Stop and SessionEnd, then the shell exits 130: idle, not crashed", () => {
    mount();
    submit("a");
    stop("a");
    sessionEnd("a");
    shellExit("a", 130);
    expect(state("a")).toBe("idle");
    expect(dot("a")).toBeUndefined();
  });
});

describe("the per-session exit (for the ended/crashed bar)", () => {
  it("records exit code, signal, requested and outcome for every session, agent or plain shell", () => {
    mount();
    ptyExit("shell-ok", "clean");
    ptyExit("shell-fail", "failed");
    ptyExit("shell-killed", "sigkill");
    ptyExit("shell-closed", "stopped");
    expect(exitOf("shell-ok")).toEqual({
      code: 0,
      signal: null,
      requested: false,
      outcome: "done",
      at: expect.any(Number),
    });
    // A plain nonzero exit just ended; a signal death is the crash.
    expect(exitOf("shell-fail")).toMatchObject({ code: 5, outcome: "done" });
    expect(exitOf("shell-killed")).toMatchObject({
      code: null,
      signal: "Killed: 9",
      outcome: "crashed",
    });
    expect(exitOf("shell-closed")).toMatchObject({ requested: true, outcome: "done" });
    // A plain shell has no agent record, and its dot clears as before.
    expect(state("shell-fail")).toBeUndefined();
    expect(dot("shell-fail")).toBeUndefined();
  });

  it("a plain shell's `false; exit` or `exit` after a Ctrl-C ended; only `kill -9` crashed", () => {
    mount();
    shellExit("false-exit", 1);
    shellExit("ctrl-c-exit", 130);
    ptyExit("kill-9", "sigkill");
    expect(exitOf("false-exit")?.outcome).toBe("done");
    expect(exitOf("ctrl-c-exit")?.outcome).toBe("done");
    expect(exitOf("kill-9")?.outcome).toBe("crashed");
  });

  it("the shell dying under an agent's open turn is a crash, whatever its code", () => {
    mount();
    submit("a");
    shellExit("a", 1);
    expect(state("a")).toBe("crashed");
    expect(exitOf("a")?.outcome).toBe("crashed");
  });

  it("a prune (tab close) drops the exit with the record, and a later pty-exit is ignored", () => {
    mount();
    submit("a");
    ptyExit("a", "failed");
    pruneAgentSessions(["a"]);
    expect(exitOf("a")).toBeUndefined();
    expect(state("a")).toBeUndefined();

    // A closed tab's PTY exits after the prune: nothing is recorded.
    pruneAgentSessions(["closed"]);
    ptyExit("closed", "stopped");
    expect(useAgentStateStore.getState().exitBySession).toEqual({});
  });

  it("a respawn retires the session, and its next exit is recorded", () => {
    mount();
    ptyExit("tab", "failed");
    retireAgentSession("tab", snapshotAgentSession("tab"));
    expect(exitOf("tab")).toBeUndefined();
    ptyExit("tab", "clean");
    expect(exitOf("tab")).toMatchObject({ code: 0, outcome: "done" });
  });
});

describe("an agent inside the tab's shell (command-end status)", () => {
  it("killed mid-turn: the shell reports 137 and the dot reads crashed", async () => {
    mount("a");
    submit("a");
    tool("a");
    commandEnd("a", 137);
    expect(state("a")).toBe("crashed");
    expect(dot("a")).toBe("error");
    // The floor then sees `claude` gone and keeps the crash.
    h.alive = false;
    await heartbeat(2);
    expect(dot("a")).toBe("error");
    // The shell's next commands are not the agent's.
    commandEnd("a", 0);
    expect(state("a")).toBe("crashed");
    expect(exitOf("a")).toBeUndefined();
  });

  it("a clean exit reports 0: done", () => {
    mount();
    submit("a");
    stop("a");
    commandEnd("a", 0);
    expect(state("a")).toBe("done");
    expect(dot("a")).toBe("done");
  });

  it("after SessionEnd the status is a later shell command's: ignored", () => {
    mount();
    submit("a");
    stop("a");
    sessionEnd("a");
    commandEnd("a", 1);
    expect(state("a")).toBe("idle");
    expect(dot("a")).toBeUndefined();
  });

  it("a shell command in a tab with no agent touches nothing", () => {
    mount();
    commandEnd("plain", 1);
    expect(state("plain")).toBeUndefined();
    expect(dot("plain")).toBeUndefined();
  });

  it("the probe saw it vanish first: the status that follows says how", async () => {
    mount();
    submit("a");
    await heartbeat(1); // seen running
    h.alive = false;
    await heartbeat(1);
    expect(state("a")).toBe("crashed");
    commandEnd("a", 0);
    expect(state("a")).toBe("done");
  });

  it("a later exit of the shell keeps the agent's verdict", () => {
    mount();
    submit("a");
    commandEnd("a", 137);
    ptyExit("a", "clean");
    expect(state("a")).toBe("crashed");
    expect(dot("a")).toBe("error");
    expect(exitOf("a")?.outcome).toBe("done");
  });

  it.each([
    ["macOS", "MacIntel", 146],
    ["Linux", "Linux x86_64", 148],
  ])(
    "Ctrl-Z on %s is not an exit: the agent stays live, and its real end after `fg` counts",
    (_os, platform, suspended) => {
      vi.spyOn(navigator, "platform", "get").mockReturnValue(platform);
      mount();
      submit("a");
      tool("a");
      commandEnd("a", suspended);
      expect(state("a")).toBe("working");
      commandEnd("a", 137); // `fg`, then killed
      expect(state("a")).toBe("crashed");

      submit("b");
      commandEnd("b", suspended);
      commandEnd("b", 0); // `fg`, then a clean exit
      expect(state("b")).toBe("done");
    },
  );

  it("a closed tab stays closed: an agent signal after the prune does not revive its exit", () => {
    mount();
    submit("a");
    pruneAgentSessions(["a"]);
    submit("a"); // the closed tab's agent keeps running
    expect(state("a")).toBeUndefined();
    useViewerStore.getState().setSessionAiStatus("a", undefined);
    submit("a");
    expect(useViewerStore.getState().aiStatusBySession.a).toBeUndefined();
    ptyExit("a", "failed");
    expect(exitOf("a")).toBeUndefined();
    expect(state("a")).toBeUndefined();
    expect(useAgentStateStore.getState().exitBySession).toEqual({});
    submit("a"); // even after its exit arrived
    ptyExit("a", "failed");
    expect(state("a")).toBeUndefined();
    expect(exitOf("a")).toBeUndefined();
  });
});
