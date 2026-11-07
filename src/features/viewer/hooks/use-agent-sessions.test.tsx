// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, renderHook, screen, act } from "@testing-library/react";

import type { AgentSessionRow } from "@/tauri/commands";

// The store imports `dashboardListSessions`; these selector tests never call
// `reload`, but mock the command so importing the store never touches Tauri.
vi.mock("@/tauri/commands", () => ({
  dashboardListSessions: () => Promise.resolve([]),
}));

import { useAgentSessions } from "./use-agent-sessions";
import { useAgentListProjectionStore } from "../lib/agent-list-projection";

const row = (over: Partial<AgentSessionRow>): AgentSessionRow => ({
  handle: "SW-A",
  sessionId: "s-A",
  kind: "shell",
  alive: true,
  backgrounded: false,
  startedWith: null,
  startedAtMs: 1,
  launcher: null,
  lastOutputMs: null,
  ...over,
});

function Probe() {
  const { sessions, error } = useAgentSessions();
  return (
    <>
      <span data-testid="total">{sessions === null ? "…" : sessions.length}</span>
      <span data-testid="bg">
        {sessions === null ? "…" : sessions.filter((s) => s.backgrounded).length}
      </span>
      <span data-testid="err">{error ?? "none"}</span>
    </>
  );
}

beforeEach(() => {
  useAgentListProjectionStore.setState({ sessions: null, error: null });
});

describe("useAgentSessions — app-root store selector (SW-AM-9)", () => {
  it("distinguishes not-loaded (null) vs empty ([]) vs populated, live", () => {
    render(<Probe />);
    expect(screen.getByTestId("total").textContent).toBe("…"); // null = not loaded

    act(() => useAgentListProjectionStore.setState({ sessions: [] }));
    expect(screen.getByTestId("total").textContent).toBe("0"); // empty ≠ null

    act(() =>
      useAgentListProjectionStore.setState({
        sessions: [row({}), row({ handle: "SW-B", backgrounded: true })],
      }),
    );
    expect(screen.getByTestId("total").textContent).toBe("2");
    expect(screen.getByTestId("bg").textContent).toBe("1");
  });

  it("surfaces the store error", () => {
    render(<Probe />);
    act(() => useAgentListProjectionStore.setState({ error: "boom" }));
    expect(screen.getByTestId("err").textContent).toBe("boom");
  });

  it("exposes the store's reload + setSessions (the same stable references)", () => {
    const { result } = renderHook(() => useAgentSessions());
    expect(result.current.reload).toBe(useAgentListProjectionStore.getState().reload);
    expect(result.current.setSessions).toBe(useAgentListProjectionStore.getState().setSessions);
  });

  it("INVARIANT (AC-2): every consumer observes the SAME array reference", () => {
    const arr = [row({})];
    act(() => useAgentListProjectionStore.setState({ sessions: arr }));
    const { result: a } = renderHook(() => useAgentSessions());
    const { result: b } = renderHook(() => useAgentSessions());
    expect(a.current.sessions).toBe(b.current.sessions);
    expect(a.current.sessions).toBe(arr);
  });
});
