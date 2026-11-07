// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
import { renderHook } from "@testing-library/react";

import type { AgentSessionRow } from "@/tauri/commands";

const dashboardForegroundSession = vi.fn((_handle: string) => Promise.resolve());
vi.mock("@/tauri/commands", () => ({
  dashboardForegroundSession: (handle: string) => dashboardForegroundSession(handle),
}));

type Tab = { id: string; sessionId: string };
let tabIndex: Record<string, Record<string, Tab[]>> = {};
vi.mock("@/features/viewer/lib/cli-workspace/store/terminal-selectors", () => ({
  useTerminalTabIndex: () => tabIndex,
}));

const { useAttendedAgentRelease } = await import("./use-attended-agent-release");
const { useAgentListProjectionStore } = await import("@/features/viewer/lib/agent-list-projection");

function row(over: Partial<AgentSessionRow> & { handle: string }): AgentSessionRow {
  return {
    sessionId: `sid-${over.handle}`,
    kind: "dashboard-launched",
    alive: true,
    backgrounded: false,
    startedWith: null,
    startedAtMs: null,
    launcher: null,
    lastOutputMs: null,
    ...over,
  };
}

beforeEach(() => {
  dashboardForegroundSession.mockClear();
  tabIndex = {};
  useAgentListProjectionStore.setState({ sessions: null, error: null });
});

describe("useAttendedAgentRelease", () => {
  it("foregrounds a backgrounded agent that has a live tab, so it frees its slot", () => {
    tabIndex = { "team-1": { ws: [{ id: "t1", sessionId: "sid-watched" }] } };
    useAgentListProjectionStore.setState({
      sessions: [
        row({ handle: "watched", backgrounded: true }),
        row({ handle: "unattended", backgrounded: true }),
        row({ handle: "foreground" }),
      ],
      error: null,
    });
    renderHook(() => useAttendedAgentRelease());
    expect(dashboardForegroundSession).toHaveBeenCalledTimes(1);
    expect(dashboardForegroundSession).toHaveBeenCalledWith("watched");
  });

  it("does nothing before the roster has loaded", () => {
    renderHook(() => useAttendedAgentRelease());
    expect(dashboardForegroundSession).not.toHaveBeenCalled();
  });
});
