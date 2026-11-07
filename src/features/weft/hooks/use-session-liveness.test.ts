// SPDX-License-Identifier: MIT
import { projectSessionStatus } from "@/test/session-projection-fixture";
import { act } from "@testing-library/react";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { beforeEach, describe, expect, it } from "vitest";
import { renderHook } from "@testing-library/react";

import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { useManagedSessionStore } from "@/shared/stores/use-managed-session-store";
import type { AgentSessionRow } from "@/tauri/commands";

import { useSessionLiveness } from "./use-session-liveness";

const row = (over: Partial<AgentSessionRow>): AgentSessionRow => ({
  handle: "SW-A",
  sessionId: "s-A",
  kind: "agent",
  alive: true,
  backgrounded: false,
  startedWith: null,
  startedAtMs: null,
  launcher: null,
  lastOutputMs: null,
  ...over,
});

beforeEach(() => {
  useAgentStateStore.setState({ exitBySession: {} });
  useAgentListProjectionStore.setState({ sessions: null, error: null });
  useManagedSessionStore.setState({ states: {}, pendingAsks: {}, summaries: {} });
});

describe("SW-AV-8 · useSessionLiveness", () => {
  it("reads NOT-loaded (no roster, no managed state) — so a row never falsely reads dead pre-load", () => {
    const { result } = renderHook(() => useSessionLiveness("s-dead"));
    expect(result.current.loaded).toBe(false);
    expect(result.current.alive).toBe(false);
  });

  it("an empty roster snapshot is a SETTLED read — an absent session is loaded + not alive (dead-eligible)", () => {
    useAgentListProjectionStore.setState({ sessions: [] });
    const { result } = renderHook(() => useSessionLiveness("s-dead"));
    expect(result.current.loaded).toBe(true);
    expect(result.current.alive).toBe(false);
  });

  it("an alive roster row reads alive and surfaces its provenance run/story ids", () => {
    useAgentListProjectionStore.setState({
      sessions: [
        row({
          sessionId: "s-live",
          alive: true,
          provenance: { runId: "run-1", storyId: "SW-A" } as AgentSessionRow["provenance"],
        }),
      ],
    });
    const { result } = renderHook(() => useSessionLiveness("s-live"));
    expect(result.current.alive).toBe(true);
    expect(result.current.runId).toBe("run-1");
    expect(result.current.storyId).toBe("SW-A");
  });

  it("a PTY-less headless run (roster alive:false) reads ALIVE while its managed state is non-terminal", () => {
    useAgentListProjectionStore.setState({
      sessions: [row({ sessionId: "s-headless", alive: false })],
    });
    useManagedSessionStore.setState({ states: { "s-headless": "running" } });
    const { result } = renderHook(() => useSessionLiveness("s-headless"));
    expect(result.current.alive).toBe(true);
  });

  it("a crashed run (terminal managed state) is loaded + NOT alive — dead-eligible even with no roster row", () => {
    useAgentListProjectionStore.setState({ sessions: [] });
    useManagedSessionStore.setState({ states: { "s-crashed": "failed" } });
    const { result } = renderHook(() => useSessionLiveness("s-crashed"));
    expect(result.current.loaded).toBe(true);
    expect(result.current.alive).toBe(false);
  });

  it("a Rust death outcome retires a stale non-terminal managed state", () => {
    projectSessionStatus("s-gone", "thinking", { alive: false, managedState: "running" });
    const { result } = renderHook(() => useSessionLiveness("s-gone"));
    expect(result.current.alive).toBe(true);
    act(() =>
      projectSessionStatus("s-gone", "dropped", { alive: false, managedState: "needs-attention" }),
    );
    expect(result.current.alive).toBe(false);
  });

  it("a null session id is never alive", () => {
    useAgentListProjectionStore.setState({ sessions: [row({ sessionId: "s-live", alive: true })] });
    const { result } = renderHook(() => useSessionLiveness(null));
    expect(result.current.alive).toBe(false);
  });
});
