// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import {
  advanceSessionAgent,
  type SessionAgentInput,
  type SessionAgentRecord,
} from "@/shared/lib/agent-state";
import { selectAgentDots } from "./agent-state-store";

const hook = (
  event: "UserPromptSubmit" | "PreToolUse" | "Stop" | "SessionEnd",
): SessionAgentInput => ({
  tier: 1,
  signal: { kind: "claude-hook", event },
});
const of = (bySession: Record<string, SessionAgentRecord>) => ({ bySession, exitBySession: {} });
const alive: SessionAgentInput = { tier: 2, signal: { kind: "alive" } };

describe("selectAgentDots — the projected dot per session", () => {
  it("lists only the sessions whose record decides the dot", () => {
    const thinking = advanceSessionAgent(undefined, hook("UserPromptSubmit"), 1);
    const ended = advanceSessionAgent(thinking, hook("SessionEnd"), 2);
    expect(selectAgentDots(of({ a: thinking, b: ended }))).toEqual({ a: "thinking" });
    expect(selectAgentDots(of({}))).toEqual({});
  });

  it("keeps its identity while no session's dot changes, and changes when one does", () => {
    const r1 = advanceSessionAgent(undefined, hook("UserPromptSubmit"), 1);
    const first = selectAgentDots(of({ a: r1 }));
    // A heartbeat moves the record but not its dot.
    const r2 = advanceSessionAgent(r1, alive, 2);
    expect(r2).not.toBe(r1);
    expect(selectAgentDots(of({ a: r2 }))).toBe(first);
    // A tool call moves the dot.
    const r3 = advanceSessionAgent(r2, hook("PreToolUse"), 3);
    const moved = selectAgentDots(of({ a: r3 }));
    expect(moved).not.toBe(first);
    expect(moved).toEqual({ a: "running" });
    // A session leaving the map is a change too.
    expect(selectAgentDots(of({}))).not.toBe(moved);
  });

  it("a record the session's own exit outlived has no say; one that ended on it does", () => {
    const progress = advanceSessionAgent(
      undefined,
      { tier: 1, signal: { kind: "signal-progress", phase: "dev" } },
      1,
    );
    const exit = { code: 0, signal: null, requested: false, outcome: "done" as const };
    expect(selectAgentDots({ bySession: { a: progress }, exitBySession: {} })).toEqual({
      a: "running",
    });
    expect(selectAgentDots({ bySession: { a: progress }, exitBySession: { a: exit } })).toEqual({});
    const crashed = advanceSessionAgent(
      advanceSessionAgent(undefined, hook("UserPromptSubmit"), 1),
      { tier: 2, signal: { kind: "exited", code: null, signal: "SIGKILL" }, source: "pty" },
      2,
    );
    expect(
      selectAgentDots({
        bySession: { a: crashed },
        exitBySession: { a: { ...exit, code: null, signal: "SIGKILL", outcome: "crashed" } },
      }),
    ).toEqual({ a: "crashed" });
  });
});

describe("the agent CLI a session's process runs", () => {
  it("rejects stale process writes and retires only the captured process identity", async () => {
    const { useAgentStateStore, agentInputSeq, snapshotAgentSession, retireAgentSession } =
      await import("./agent-state-store");
    const store = () => useAgentStateStore.getState();
    const sentAt = agentInputSeq();
    store().setProcessAgent("reused-process", "claude");
    store().reset("reused-process");
    store().setProcessAgent("reused-process", "codex", sentAt);
    expect(store().processAgentBySession["reused-process"]).toBeUndefined();
    store().setProcessAgent("reused-process", "claude");
    const snapshot = snapshotAgentSession("reused-process");
    retireAgentSession("reused-process", snapshot);
    expect(store().processAgentBySession["reused-process"]).toBeUndefined();
    store().remove(["reused-process"]);
    store().setProcessAgent("reused-process", "codex");
    expect(store().processAgentBySession["reused-process"]).toBeUndefined();
  });

  it("preserves a fresh same-CLI observation when retiring an older snapshot", async () => {
    const { useAgentStateStore, snapshotAgentSession, retireAgentSession } =
      await import("./agent-state-store");
    const store = () => useAgentStateStore.getState();
    for (const reset of [false, true]) {
      store().setProcessAgent("same-cli", "codex");
      const snapshot = snapshotAgentSession("same-cli");
      if (reset) store().reset("same-cli");
      store().setProcessAgent("same-cli", "codex");
      retireAgentSession("same-cli", snapshot);
      expect(store().processAgentBySession["same-cli"]).toBe("codex");
    }
    store().reset("same-cli");
  });

  it("is kept per session, and dropped with the tab or a respawn", async () => {
    const { useAgentStateStore } = await import("./agent-state-store");
    const store = () => useAgentStateStore.getState();
    store().setProcessAgent("p1", "claude");
    store().setProcessAgent("p2", "codex");
    const before = store().processAgentBySession;
    store().setProcessAgent("p1", "claude");
    expect(store().processAgentBySession).toBe(before);
    expect(before).toEqual({ p1: "claude", p2: "codex" });

    store().reset("p1");
    expect(store().processAgentBySession).toEqual({ p2: "codex" });
    store().remove(["p2"]);
    expect(store().processAgentBySession).toEqual({});
  });
});
