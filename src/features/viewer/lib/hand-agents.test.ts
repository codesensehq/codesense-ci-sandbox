// SPDX-License-Identifier: MIT
import { act } from "@/test-utils";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { describe, expect, it } from "vitest";

import { detectedAgentCli, isAgentSession, isRunningAgent } from "./hand-agents";
import type { SessionAgentRecord } from "@/shared/lib/agent-state";
import type { AgentSessionRow } from "@/tauri/commands";

const rec = (over: Partial<SessionAgentRecord>): SessionAgentRecord => ({
  t1: null,
  t2: null,
  state: "idle",
  agent: null,
  ...over,
});

const bare = (sessionId: string, over: Partial<AgentSessionRow> = {}): AgentSessionRow => ({
  handle: sessionId,
  sessionId,
  kind: "live-pty",
  alive: true,
  backgrounded: false,
  startedWith: null,
  startedAtMs: null,
  launcher: null,
  lastOutputMs: null,
  ...over,
});

describe("detectedAgentCli", () => {
  it("detects an established agent, or one mid-turn through its signals", () => {
    expect(detectedAgentCli(rec({ agent: "claude", state: "done" }), undefined)).toBeNull();
    expect(detectedAgentCli(rec({ state: "working" }), "codex")).toEqual({
      cli: "codex",
      processOnly: false,
    });
  });

  it("detects an agent CLI running in the tab's shell with no hook record at all", () => {
    // A reattached Claude (its establishing hook fired before this app started), a Codex
    // and OpenCode (no hooks): the process tree alone says an agent runs.
    expect(detectedAgentCli(undefined, undefined, "claude")).toEqual({
      cli: "claude",
      processOnly: true,
    });
    expect(detectedAgentCli(undefined, undefined, "codex")).toEqual({
      cli: "codex",
      processOnly: true,
    });
    expect(detectedAgentCli(undefined, undefined, "opencode")).toEqual({
      cli: "opencode",
      processOnly: true,
    });
    // A record whose agent ended earlier does not hide one running again now.
    expect(
      detectedAgentCli(
        rec({ state: "done", t2: { signal: { kind: "exited", code: 0 }, seq: 2 } }),
        undefined,
        "codex",
      ),
    ).toEqual({ cli: "codex", processOnly: true });
  });

  it("a plain shell (no agent process, no record) is not an agent", () => {
    expect(detectedAgentCli(undefined, undefined, null)).toBeNull();
    expect(detectedAgentCli(rec({ state: "idle" }), undefined, undefined)).toBeNull();
  });

  it("hooks speak over the process: the record names the agent and its live state", () => {
    expect(
      detectedAgentCli(rec({ agent: "claude", state: "needs-permission" }), undefined, "claude"),
    ).toEqual({ cli: "claude", processOnly: false });
  });

  it("finds none in a plain shell or once the agent ended", () => {
    expect(detectedAgentCli(undefined, undefined)).toBeNull();
    expect(detectedAgentCli(rec({ state: "idle" }), undefined)).toBeNull();
    expect(
      detectedAgentCli(
        rec({
          state: "done",
          t2: { signal: { kind: "exited", code: 0 }, seq: 2 },
        }),
        undefined,
      ),
    ).toBeNull();
  });
});

describe("live lifecycle through the listed roster", () => {
  it("requires current host evidence on restored and remote sessions", () => {
    const snapshot = {
      session: "remote",
      identity: { hostBootId: "host", sessionIncarnation: "shell", invocationId: "agent" },
      revision: 1,
      turnGeneration: 0,
      provider: "codex" as const,
      state: "idle" as const,
      observed: true,
      limited: false,
      exit: null,
      asks: [],
    };
    useAgentStateStore.setState({
      bySession: {},
      exitBySession: {},
      activityBySession: {},
      processAgentBySession: {},
    });
    const rows = [
      bare("remote", { kind: "dashboard-launched", handle: "work#remote", machineId: "box" }),
    ];
    const result = { current: rows };
    expect(isRunningAgent(useAgentStateStore.getState(), "remote")).toBe(false);
    act(() => {
      useAgentStateStore.getState().recordActivity(snapshot, 0);
    });
    expect(isRunningAgent(useAgentStateStore.getState(), "remote")).toBe(true);
    act(() => useAgentStateStore.getState().activityUnavailable(["remote"]));
    expect(isRunningAgent(useAgentStateStore.getState(), "remote")).toBe(false);
    expect(result.current![0].handle).toBe("work#remote");
    act(() => {
      useAgentStateStore.getState().recordActivity({ ...snapshot, revision: 2 }, 0);
    });
    expect(isRunningAgent(useAgentStateStore.getState(), "remote")).toBe(true);
    act(() => {
      useAgentStateStore
        .getState()
        .recordActivity(
          { ...snapshot, revision: 3, exit: { code: 0, signal: null, requested: false } },
          0,
        );
    });
    expect(isRunningAgent(useAgentStateStore.getState(), "remote")).toBe(false);
  });

  it("keeps saved instances separate, idle agents eligible, and launcher identity after exit", () => {
    useAgentStateStore.setState({
      bySession: {},
      exitBySession: {},
      activityBySession: {},
      processAgentBySession: { first: "codex", second: "codex", manual: "claude" },
    });
    const rows = [
      bare("first", {
        handle: "work#saved",
        kind: "dashboard-launched",
        launchedBy: "shell",
        role: "builder",
      }),
      bare("second", { handle: "work#saved-2", kind: "dashboard-launched" }),
      bare("manual"),
      bare("shell"),
    ];
    const result = { current: rows };
    expect(
      result
        .current!.filter(
          (r) => !!r.sessionId && isRunningAgent(useAgentStateStore.getState(), r.sessionId),
        )
        .map((r) => r.sessionId),
    ).toEqual(["first", "second", "manual"]);
    act(() =>
      useAgentStateStore.getState().recordSignal("manual", {
        tier: 1,
        signal: { kind: "claude-hook", event: "SessionEnd" },
      }),
    );
    expect(
      result
        .current!.filter(
          (r) => !!r.sessionId && isRunningAgent(useAgentStateStore.getState(), r.sessionId),
        )
        .map((r) => r.sessionId),
    ).toEqual(["first", "second"]);
    act(() => useAgentStateStore.getState().setProcessAgent("first", null));
    const first = result.current!.find((r) => r.sessionId === "first")!;
    expect(isRunningAgent(useAgentStateStore.getState(), "first")).toBe(false);
    expect(isAgentSession(first)).toBe(true);
    expect(first).toMatchObject({ handle: "work#saved", launchedBy: "shell", role: "builder" });
    act(() => useAgentStateStore.getState().setProcessAgent("first", "codex"));
    expect(isRunningAgent(useAgentStateStore.getState(), "first")).toBe(true);
  });

  it("an OpenCode session exposes the inbox like Claude and Codex", () => {
    useAgentStateStore.setState({
      bySession: {},
      exitBySession: {},
      activityBySession: {},
      processAgentBySession: { open: "opencode" },
    });
    const rows = [bare("open", { handle: "work#open", kind: "dashboard-launched" })];
    const eligible = rows
      .filter((r) => !!r.sessionId && isRunningAgent(useAgentStateStore.getState(), r.sessionId))
      .map((r) => r.sessionId);
    expect(eligible).toEqual(["open"]);
  });
});
