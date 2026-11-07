// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act } from "@testing-library/react";

// SW-FX-52 (AC-3): the monitor now batches via `sessionsHaveLiveAgent`. It
// resolves alive for every queried pty by default (still-connected).
const h = vi.hoisted(() => ({
  sessionsHaveLiveAgent: vi.fn(async (queries: { ptyId: string; agentName: string }[]) =>
    queries.map((q) => ({ ptyId: q.ptyId, alive: true as boolean | null })),
  ),
}));
vi.mock("@/tauri/commands", () => ({ sessionsHaveLiveAgent: h.sessionsHaveLiveAgent }));

import {
  useAgentLivenessMonitor,
  useAgentDotFloor,
  floorDotStatus,
  LIVENESS_POLL_MS,
  collectTabSessionIds,
  isPromptShaped,
  lastOutputLine,
  resetLivenessPollerForTests,
  stepIdleAttention,
  watchTerminalTabAgents,
} from "@/shared/hooks/use-agent-liveness-monitor";
import {
  useAgentStateStore,
  resetPrunedAgentSessionsForTests,
} from "@/shared/stores/agent-state-store";
import type { AgentSignalInfo, SessionLivenessQuery } from "@/tauri/commands";
import { MockTerminalHost } from "@/app/mock/terminal-layout-host";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { KNOWN_AGENT_CLI_NAMES } from "@/shared/lib/agent-clis";
import {
  seedTerminalTabs,
  terminalHost,
  installTerminalLayoutHarness,
} from "@/test/terminal-layout-harness";

installTerminalLayoutHarness();

/** Make the batch command report the given ptyIds as dead, all others alive. */
function markDead(...deadPtyIds: string[]) {
  const dead = new Set(deadPtyIds);
  h.sessionsHaveLiveAgent.mockImplementation(async (queries) =>
    queries.map((q) => ({ ptyId: q.ptyId, alive: !dead.has(q.ptyId) })),
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  resetPrunedAgentSessionsForTests();
  useAgentStateStore.setState({ bySession: {}, exitBySession: {} });
  h.sessionsHaveLiveAgent.mockImplementation(async (queries) =>
    queries.map((q) => ({ ptyId: q.ptyId, alive: true })),
  );
});

describe("useAgentLivenessMonitor (SW-GS-2d / SW-FX-52 AC-3)", () => {
  it("calls onDead ONCE when the agent process disappears while active (agent-agnostic basename)", async () => {
    vi.useFakeTimers();
    try {
      const onDead = vi.fn();
      renderHook(() =>
        useAgentLivenessMonitor({ active: true, sessionId: "s1", agentBasename: "codex", onDead }),
      );
      await act(async () => {
        await vi.advanceTimersByTimeAsync(LIVENESS_POLL_MS + 10);
      });
      expect(onDead).not.toHaveBeenCalled(); // still alive
      // agent-agnostic: passes the caller's basename, not a "claude" literal.
      expect(h.sessionsHaveLiveAgent).toHaveBeenCalledWith([{ ptyId: "s1", agentName: "codex" }]);

      markDead("s1");
      await act(async () => {
        await vi.advanceTimersByTimeAsync(LIVENESS_POLL_MS * 2);
      });
      expect(onDead).toHaveBeenCalledTimes(1); // once, then auto-unregisters
    } finally {
      vi.useRealTimers();
    }
  });

  it("collapses N monitors into ONE interval + ONE batched call per tick", async () => {
    vi.useFakeTimers();
    try {
      const onDead = vi.fn();
      for (const sid of ["s1", "s2", "s3"]) {
        renderHook(() =>
          useAgentLivenessMonitor({
            active: true,
            sessionId: sid,
            agentBasename: "codex",
            onDead,
          }),
        );
      }
      await act(async () => {
        await vi.advanceTimersByTimeAsync(LIVENESS_POLL_MS + 10);
      });
      // ONE call for the whole tick, carrying ALL three sessions (not 3 calls).
      expect(h.sessionsHaveLiveAgent).toHaveBeenCalledTimes(1);
      expect(h.sessionsHaveLiveAgent).toHaveBeenCalledWith([
        { ptyId: "s1", agentName: "codex" },
        { ptyId: "s2", agentName: "codex" },
        { ptyId: "s3", agentName: "codex" },
      ]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("fires onDead only for the flipped session, not the still-alive ones", async () => {
    vi.useFakeTimers();
    try {
      const onDead1 = vi.fn();
      const onDead2 = vi.fn();
      renderHook(() =>
        useAgentLivenessMonitor({
          active: true,
          sessionId: "s1",
          agentBasename: "codex",
          onDead: onDead1,
        }),
      );
      renderHook(() =>
        useAgentLivenessMonitor({
          active: true,
          sessionId: "s2",
          agentBasename: "codex",
          onDead: onDead2,
        }),
      );
      markDead("s2"); // only s2 dies
      await act(async () => {
        await vi.advanceTimersByTimeAsync(LIVENESS_POLL_MS + 10);
      });
      expect(onDead1).not.toHaveBeenCalled();
      expect(onDead2).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not poll when inactive or sessionId is null", async () => {
    vi.useFakeTimers();
    try {
      const onDead = vi.fn();
      renderHook(() =>
        useAgentLivenessMonitor({ active: false, sessionId: "s1", agentBasename: "codex", onDead }),
      );
      renderHook(() =>
        useAgentLivenessMonitor({ active: true, sessionId: null, agentBasename: "codex", onDead }),
      );
      await act(async () => {
        await vi.advanceTimersByTimeAsync(LIVENESS_POLL_MS * 2);
      });
      expect(h.sessionsHaveLiveAgent).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("a batch probe error never false-positives a disconnect", async () => {
    vi.useFakeTimers();
    try {
      const onDead = vi.fn();
      h.sessionsHaveLiveAgent.mockRejectedValue(new Error("ipc failed"));
      renderHook(() =>
        useAgentLivenessMonitor({ active: true, sessionId: "s1", agentBasename: "codex", onDead }),
      );
      await act(async () => {
        await vi.advanceTimersByTimeAsync(LIVENESS_POLL_MS + 10);
      });
      expect(onDead).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("unmount unregisters: no onDead after unmount and the interval stops when empty", async () => {
    vi.useFakeTimers();
    try {
      const onDead = vi.fn();
      const { unmount } = renderHook(() =>
        useAgentLivenessMonitor({ active: true, sessionId: "s1", agentBasename: "codex", onDead }),
      );
      markDead("s1");
      unmount();
      h.sessionsHaveLiveAgent.mockClear();
      await act(async () => {
        await vi.advanceTimersByTimeAsync(LIVENESS_POLL_MS * 3);
      });
      // Registry emptied → shared interval cleared → no further batched calls,
      // and onDead is never invoked after unmount.
      expect(h.sessionsHaveLiveAgent).not.toHaveBeenCalled();
      expect(onDead).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });
});

// ---- SLW-FX-17: reliability floor for the per-tab dot ----

describe("floorDotStatus (SLW-FX-17 push/pull merge)", () => {
  it("process gone → clears the dot regardless of current state", () => {
    expect(floorDotStatus(undefined, false)).toBeUndefined();
    expect(floorDotStatus("running", false)).toBeUndefined();
    expect(floorDotStatus("done", false)).toBeUndefined();
    expect(floorDotStatus("needs-permission", false)).toBeUndefined();
  });

  it("process present + empty dot → floors to running (backfills unwired dirs)", () => {
    expect(floorDotStatus(undefined, true)).toBe("running");
  });

  it("process present + a hook-set state → never overrides it (hook owns precision)", () => {
    expect(floorDotStatus("running", true)).toBe("running");
    expect(floorDotStatus("done", true)).toBe("done");
    expect(floorDotStatus("error", true)).toBe("error");
    expect(floorDotStatus("needs-permission", true)).toBe("needs-permission");
  });
});

describe("activity observation admission", () => {
  it("rejects a legacy result when its id became ambiguously held while awaiting it", async () => {
    vi.useFakeTimers();
    try {
      let resolveProbe!: (value: { ptyId: string; alive: boolean }[]) => void;
      h.sessionsHaveLiveAgent.mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            resolveProbe = resolve;
          }),
      );
      const onDead = vi.fn();
      useViewerStore.setState({ aiStatusBySession: { "newly-held": "done" } });
      useAgentStateStore.getState().recordSignal("newly-held", {
        tier: 1,
        signal: { kind: "claude-hook", event: "UserPromptSubmit" },
      });
      renderHook(() => {
        useAgentLivenessMonitor({
          active: true,
          sessionId: "newly-held",
          agentBasename: "codex",
          onDead,
        });
        useAgentDotFloor({ active: true, sessionId: "newly-held", agentBasename: "codex" });
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(LIVENESS_POLL_MS + 10);
      });
      const remote = new MockTerminalHost(() => {}, "remote-epoch");
      remote.seed(
        "remote-workspace",
        { displayName: "Remote", kind: "generic" },
        { session: "newly-held" },
      );
      seedTerminalTabs("t", "local", ["newly-held"]);
      useViewerStore.getState().applyLayout({
        kind: "snapshot",
        replies: [
          terminalHost().reply(),
          { ...remote.reply(), member: { machineId: "box", slot: "remote" } },
        ],
      });
      await act(async () => {
        resolveProbe([{ ptyId: "newly-held", alive: false }]);
      });
      expect(onDead).not.toHaveBeenCalled();
      expect(useViewerStore.getState().aiStatusBySession["newly-held"]).toBe("done");
      expect(useAgentStateStore.getState().bySession["newly-held"]?.state).toBe("thinking");
    } finally {
      resetLivenessPollerForTests();
      vi.useRealTimers();
    }
  });

  it("never probes an ambiguous remote/local wire id as a legacy local session", async () => {
    vi.useFakeTimers();
    try {
      const remote = new MockTerminalHost(() => {}, "remote-epoch");
      remote.seed(
        "remote-workspace",
        { displayName: "Remote", kind: "generic" },
        { session: "same-id" },
      );
      seedTerminalTabs("t", "local", ["same-id"]);
      useViewerStore.getState().applyLayout({
        kind: "snapshot",
        replies: [
          terminalHost().reply(),
          { ...remote.reply(), member: { machineId: "box", slot: "remote" } },
        ],
      });
      const onDead = vi.fn();
      renderHook(() =>
        useAgentLivenessMonitor({
          active: true,
          sessionId: "same-id",
          agentBasename: "codex",
          onDead,
        }),
      );
      await act(async () => {
        await vi.advanceTimersByTimeAsync(LIVENESS_POLL_MS + 10);
      });
      expect(h.sessionsHaveLiveAgent).not.toHaveBeenCalled();
      expect(onDead).not.toHaveBeenCalled();
    } finally {
      resetLivenessPollerForTests();
      vi.useRealTimers();
    }
  });

  it("keeps polling and reconciling local sessions while a remote host stalls", async () => {
    vi.useFakeTimers();
    try {
      const remote = new MockTerminalHost(() => {}, "remote-epoch");
      remote.seed(
        "remote-workspace",
        { displayName: "Remote", kind: "generic" },
        { session: "remote-stalled" },
      );
      seedTerminalTabs("t", "local", ["local-responsive"]);
      useViewerStore.getState().applyLayout({
        kind: "snapshot",
        replies: [
          terminalHost().reply(),
          { ...remote.reply(), member: { machineId: "box", slot: "remote" } },
        ],
      });
      let localPolls = 0;
      h.sessionsHaveLiveAgent.mockImplementation((queries) => {
        if (queries.some((q) => q.ptyId === "remote-stalled")) return new Promise(() => {});
        localPolls++;
        return Promise.resolve(queries.map((q) => ({ ptyId: q.ptyId, alive: localPolls === 1 })));
      });
      const localDead = vi.fn();
      const remoteDead = vi.fn();
      renderHook(() => {
        useAgentLivenessMonitor({
          active: true,
          sessionId: "remote-stalled",
          agentBasename: "codex",
          onDead: remoteDead,
        });
        useAgentLivenessMonitor({
          active: true,
          sessionId: "local-responsive",
          agentBasename: "codex",
          onDead: localDead,
        });
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(LIVENESS_POLL_MS * 2 + 10);
      });
      expect(localPolls).toBe(2);
      expect(localDead).toHaveBeenCalledOnce();
      expect(remoteDead).not.toHaveBeenCalled();
      expect(
        h.sessionsHaveLiveAgent.mock.calls.filter(([qs]) =>
          qs.some((q) => q.ptyId === "remote-stalled"),
        ),
      ).toHaveLength(1);
    } finally {
      resetLivenessPollerForTests();
      vi.useRealTimers();
    }
  });

  it("keeps unknown remote activity while accepting local evidence in the same batch", async () => {
    vi.useFakeTimers();
    try {
      const remoteDead = vi.fn();
      const localDead = vi.fn();
      h.sessionsHaveLiveAgent.mockImplementation(async (queries) =>
        queries.map((q) => ({
          ptyId: q.ptyId,
          alive: q.ptyId === "remote-unknown" ? null : false,
        })),
      );
      useViewerStore.setState({ aiStatusBySession: { "remote-unknown": "done" } });
      renderHook(() => {
        useAgentLivenessMonitor({
          active: true,
          sessionId: "remote-unknown",
          agentBasename: "codex",
          onDead: remoteDead,
        });
        useAgentLivenessMonitor({
          active: true,
          sessionId: "local-ended",
          agentBasename: "codex",
          onDead: localDead,
        });
        useAgentDotFloor({ active: true, sessionId: "remote-unknown", agentBasename: "codex" });
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(LIVENESS_POLL_MS + 10);
      });
      expect(remoteDead).not.toHaveBeenCalled();
      expect(localDead).toHaveBeenCalledOnce();
      expect(useViewerStore.getState().aiStatusBySession["remote-unknown"]).toBe("done");
    } finally {
      vi.useRealTimers();
    }
  });

  it("rejects an in-flight dot result after a same-id reset", async () => {
    vi.useFakeTimers();
    try {
      let resolveProbe!: (value: { ptyId: string; alive: boolean }[]) => void;
      h.sessionsHaveLiveAgent.mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            resolveProbe = resolve;
          }),
      );
      useViewerStore.setState({ aiStatusBySession: {} });
      renderHook(() =>
        useAgentDotFloor({ active: true, sessionId: "replaced", agentBasename: "codex" }),
      );
      await act(async () => {
        await vi.advanceTimersByTimeAsync(LIVENESS_POLL_MS + 10);
      });
      useAgentStateStore.getState().reset("replaced");
      await act(async () => {
        resolveProbe([{ ptyId: "replaced", alive: true }]);
      });
      expect(useViewerStore.getState().aiStatusBySession.replaced).toBeUndefined();
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("useAgentDotFloor (SLW-FX-17)", () => {
  beforeEach(() => {
    useViewerStore.setState({ aiStatusBySession: {} });
  });

  it("floors an empty dot to running while the agent is alive", async () => {
    vi.useFakeTimers();
    try {
      renderHook(() => useAgentDotFloor({ active: true, sessionId: "s1", agentBasename: "codex" }));
      await act(async () => {
        await vi.advanceTimersByTimeAsync(LIVENESS_POLL_MS + 10);
      });
      expect(useViewerStore.getState().aiStatusBySession.s1).toBe("running");
    } finally {
      vi.useRealTimers();
    }
  });

  it("never overrides a precise hook-set state while alive", async () => {
    vi.useFakeTimers();
    try {
      useViewerStore.setState({ aiStatusBySession: { s1: "needs-permission" } });
      renderHook(() => useAgentDotFloor({ active: true, sessionId: "s1", agentBasename: "codex" }));
      await act(async () => {
        await vi.advanceTimersByTimeAsync(LIVENESS_POLL_MS + 10);
      });
      expect(useViewerStore.getState().aiStatusBySession.s1).toBe("needs-permission");
    } finally {
      vi.useRealTimers();
    }
  });

  it("clears the dot when the agent process is gone (self-heals a stuck dot)", async () => {
    vi.useFakeTimers();
    try {
      useViewerStore.setState({ aiStatusBySession: { s1: "running" } });
      markDead("s1");
      renderHook(() => useAgentDotFloor({ active: true, sessionId: "s1", agentBasename: "codex" }));
      await act(async () => {
        await vi.advanceTimersByTimeAsync(LIVENESS_POLL_MS + 10);
      });
      expect(useViewerStore.getState().aiStatusBySession.s1).toBeUndefined();
    } finally {
      vi.useRealTimers();
    }
  });

  it("is persistent: keeps clearing across ticks (no auto-unregister on death)", async () => {
    vi.useFakeTimers();
    try {
      markDead("s1");
      renderHook(() => useAgentDotFloor({ active: true, sessionId: "s1", agentBasename: "codex" }));
      await act(async () => {
        await vi.advanceTimersByTimeAsync(LIVENESS_POLL_MS + 10);
      });
      // A late hook post lands under the dead id...
      act(() => {
        useViewerStore.getState().setSessionAiStatus("s1", "running");
      });
      // ...and the next tick clears it again — the floor never stopped watching.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(LIVENESS_POLL_MS + 10);
      });
      expect(useViewerStore.getState().aiStatusBySession.s1).toBeUndefined();
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("idle-output attention (pure)", () => {
  it("reads the last non-blank line of the screen text", () => {
    expect(lastOutputLine("done\nApply patch? (y/n) \n\n")).toBe("Apply patch? (y/n)");
    expect(lastOutputLine("Name: ")).toBe("Name:");
    expect(lastOutputLine("")).toBe("");
  });

  it("treats questions, input prompts and yes/no choices as prompt-shaped", () => {
    for (const line of ["Continue?", "Name:", "> ", "› ", "Overwrite [y/N]", "Proceed (yes/no)"]) {
      expect(isPromptShaped(line), line).toBe(true);
    }
    for (const line of ["", "Compiling crate", "Wrote 3 files.", "50%"]) {
      expect(isPromptShaped(line), line).toBe(false);
    }
  });

  it("raises needs-you once after the quiet window on a prompt, and resumes on new output", () => {
    const ask = { seq: 10, tail: "Apply? (y/n) " };
    let step = stepIdleAttention(undefined, ask, 0, 5_000);
    expect(step.change).toBeNull();
    step = stepIdleAttention(step.track, ask, 4_000, 5_000);
    expect(step.change).toBeNull();
    step = stepIdleAttention(step.track, ask, 5_000, 5_000);
    expect(step.change).toBe("needs-you");
    step = stepIdleAttention(step.track, ask, 9_000, 5_000);
    expect(step.change).toBeNull();
    step = stepIdleAttention(step.track, { seq: 42, tail: "applying..." }, 9_500, 5_000);
    expect(step.change).toBe("resumed");
  });

  it("stays quiet when the silent last line is not a prompt", () => {
    const busy = { seq: 7, tail: "Compiling crate" };
    const first = stepIdleAttention(undefined, busy, 0, 1_000);
    expect(stepIdleAttention(first.track, busy, 60_000, 1_000).change).toBeNull();
  });

  it("collects live and soft-closed tab sessions", () => {
    const ws = (id: string) => ({
      id,
      kind: "generic" as const,
      cwd: `/${id}`,
      status: "ready" as const,
    });
    useViewerStore.setState({
      workspaces: { t: [ws("a"), ws("b"), ws("c")] },
      softClosedWorkspaces: {},
    });
    seedTerminalTabs("t", "a", ["s1"]);
    seedTerminalTabs("t", "b", ["s2"]);
    seedTerminalTabs("t", "c", ["s3"]);
    useViewerStore.getState().closeWorkspace("t", "c");
    expect(collectTabSessionIds(useViewerStore.getState()).sort()).toEqual(["s1", "s2", "s3"]);
  });
});

describe("watchTerminalTabAgents (dot for any agent in any terminal tab)", () => {
  const AGENTS: AgentSignalInfo[] = [
    { basename: "claude", attention: "hooks", quietSecs: 10 },
    { basename: "codex", attention: "mcp", quietSecs: 10 },
    { basename: "aider", attention: "idle-output", quietSecs: 8 },
  ];

  beforeEach(() => {
    resetLivenessPollerForTests();
    useViewerStore.setState({
      aiStatusBySession: {},
      workspaces: { team: [{ id: "story", kind: "generic", cwd: "/story", status: "ready" }] },
      softClosedWorkspaces: {},
    });
    seedTerminalTabs("team", "story", ["s1"]);
  });

  /** Report `running` as the only live agent in s1, with `output` as its tail. */
  function agentInTab(running: string, output?: () => { seq: number; tail: string }) {
    h.sessionsHaveLiveAgent.mockImplementation(async (queries: SessionLivenessQuery[]) =>
      queries.map((q) => ({
        ptyId: q.ptyId,
        alive: q.agentName === running,
        ...(q.tail && output ? { output: output() } : {}),
      })),
    );
  }

  async function ticks(n: number) {
    for (let i = 0; i < n; i++) {
      await act(async () => {
        await vi.advanceTimersByTimeAsync(LIVENESS_POLL_MS);
      });
    }
  }

  it("floors the dot for a known agent in a plain story tab and clears it on exit", async () => {
    vi.useFakeTimers();
    const stop = watchTerminalTabAgents(async () => AGENTS);
    try {
      agentInTab("codex");
      await ticks(1);
      expect(useViewerStore.getState().aiStatusBySession.s1).toBe("running");
      const queried = h.sessionsHaveLiveAgent.mock.calls.at(-1)?.[0] as SessionLivenessQuery[];
      // The built-in agents, then every other known agent CLI.
      expect(queried.map((q) => q.agentName).sort()).toEqual([...KNOWN_AGENT_CLI_NAMES].sort());
      expect(queried.find((q) => q.agentName === "codex")?.tail).toBeUndefined();
      expect(queried.find((q) => q.agentName === "aider")?.tail).toBe(true);

      agentInTab("none");
      await ticks(1);
      expect(useViewerStore.getState().aiStatusBySession.s1).toBeUndefined();
    } finally {
      stop();
      vi.useRealTimers();
    }
  });

  it("records the agent CLI running in a tab, with no hook, and forgets it when the agent exits", async () => {
    vi.useFakeTimers();
    const stop = watchTerminalTabAgents(async () => AGENTS);
    try {
      // An unobserved Codex and nothing in the agent-state record: the process says it runs.
      agentInTab("codex");
      await ticks(1);
      expect(useAgentStateStore.getState().bySession.s1).toBeUndefined();
      expect(useAgentStateStore.getState().processAgentBySession.s1).toBe("codex");

      // Back to a bare shell: the tab is a shell again.
      agentInTab("none");
      await ticks(1);
      expect(useAgentStateStore.getState().processAgentBySession.s1).toBeUndefined();
    } finally {
      stop();
      vi.useRealTimers();
    }
  });

  it("does not restore process identity from a tab probe sent before reset", async () => {
    vi.useFakeTimers();
    let reply!: () => void;
    h.sessionsHaveLiveAgent.mockImplementationOnce(
      (queries) =>
        new Promise((resolve) => {
          reply = () =>
            resolve(queries.map((q) => ({ ptyId: q.ptyId, alive: q.agentName === "codex" })));
        }),
    );
    const stop = watchTerminalTabAgents(async () => AGENTS);
    try {
      await ticks(1);
      useAgentStateStore.getState().reset("s1");
      await act(async () => reply());
      expect(useAgentStateStore.getState().processAgentBySession.s1).toBeUndefined();
      expect(useViewerStore.getState().aiStatusBySession.s1).toBeUndefined();
    } finally {
      stop();
      vi.useRealTimers();
    }
  });

  it("keeps process identity when another known CLI probe is unavailable", async () => {
    vi.useFakeTimers();
    const stop = watchTerminalTabAgents(async () => AGENTS);
    try {
      agentInTab("codex");
      await ticks(1);
      h.sessionsHaveLiveAgent.mockImplementation(async (queries) =>
        queries.map((q) => ({
          ptyId: q.ptyId,
          alive: q.agentName === "codex" ? false : null,
        })),
      );
      await ticks(1);
      expect(useAgentStateStore.getState().processAgentBySession.s1).toBe("codex");
      expect(useViewerStore.getState().aiStatusBySession.s1).toBe("running");
      agentInTab("none");
      await ticks(1);
      expect(useAgentStateStore.getState().processAgentBySession.s1).toBeUndefined();
    } finally {
      stop();
      vi.useRealTimers();
    }
  });

  it("finds a known CLI the signal list does not name, and still probes when the signal list fails", async () => {
    vi.useFakeTimers();
    const stop = watchTerminalTabAgents(() => Promise.reject(new Error("no packs")));
    try {
      agentInTab("gemini");
      await ticks(1);
      expect(useAgentStateStore.getState().processAgentBySession.s1).toBe("gemini");
      expect(useViewerStore.getState().aiStatusBySession.s1).toBe("running");
    } finally {
      stop();
      vi.useRealTimers();
    }
    // Stopping the watch forgets what it found.
    expect(useAgentStateStore.getState().processAgentBySession.s1).toBeUndefined();
  });

  it("an idle-output agent quiet on a prompt shows needs-you, then running when it resumes", async () => {
    vi.useFakeTimers();
    const stop = watchTerminalTabAgents(async () => AGENTS);
    try {
      let out = { seq: 100, tail: "Add these files to the chat? (Y)es/(N)o [Yes]: " };
      agentInTab("aider", () => out);
      await ticks(2);
      expect(useViewerStore.getState().aiStatusBySession.s1).toBe("running");
      await ticks(2);
      expect(useViewerStore.getState().aiStatusBySession.s1).toBe("needs-permission");

      out = { seq: 180, tail: "Applied edit to main.py" };
      await ticks(1);
      expect(useViewerStore.getState().aiStatusBySession.s1).toBe("running");
    } finally {
      stop();
      vi.useRealTimers();
    }
  });

  it("an MCP-attention agent's needs-you (signal_progress blocked) is never cleared by the floor", async () => {
    vi.useFakeTimers();
    const stop = watchTerminalTabAgents(async () => AGENTS);
    try {
      agentInTab("codex");
      useViewerStore.getState().setSessionAiStatus("s1", "needs-permission");
      await ticks(3);
      expect(useViewerStore.getState().aiStatusBySession.s1).toBe("needs-permission");
    } finally {
      stop();
      vi.useRealTimers();
    }
  });

  it("never clears a dot for an agent it cannot see (unlisted or outside the tab's shell)", async () => {
    vi.useFakeTimers();
    const stop = watchTerminalTabAgents(async () => AGENTS);
    try {
      agentInTab("acme");
      useViewerStore.getState().setSessionAiStatus("s1", "needs-permission");
      await ticks(3);
      expect(useViewerStore.getState().aiStatusBySession.s1).toBe("needs-permission");
    } finally {
      stop();
      vi.useRealTimers();
    }
  });

  it("resuming output never clears a needs-you another channel raised", async () => {
    vi.useFakeTimers();
    const stop = watchTerminalTabAgents(async () => AGENTS);
    try {
      let out = { seq: 1, tail: "working" };
      agentInTab("aider", () => out);
      await ticks(1);
      useViewerStore.getState().setSessionAiStatus("s1", "needs-permission");
      out = { seq: 2, tail: "more output" };
      await ticks(2);
      expect(useViewerStore.getState().aiStatusBySession.s1).toBe("needs-permission");
    } finally {
      stop();
      vi.useRealTimers();
    }
  });

  it("stops watching a tab once it is closed", async () => {
    vi.useFakeTimers();
    const stop = watchTerminalTabAgents(async () => AGENTS);
    try {
      agentInTab("codex");
      await ticks(1);
      act(() => {
        terminalHost().closeTab("s1", "kill");
      });
      h.sessionsHaveLiveAgent.mockClear();
      await ticks(2);
      expect(h.sessionsHaveLiveAgent).not.toHaveBeenCalled();
    } finally {
      stop();
      vi.useRealTimers();
    }
  });
});
