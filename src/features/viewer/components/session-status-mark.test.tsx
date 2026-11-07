// SPDX-License-Identifier: MIT
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@/test-utils";
import { useMachinesStore } from "@/features/machines/stores/machines-store";
import type { MachineSummary } from "@/tauri/commands";

vi.mock("@/shared/stores/viewer-store", async (original) => {
  const actual = await original<typeof import("@/shared/stores/viewer-store")>();
  return {
    ...actual,
    selectTabBySessionId: (state: Parameters<typeof actual.selectTabBySessionId>[0], id: string) =>
      id === "remote-agent"
        ? ({ origin: { machineId: "box", member: "box", handle: "h" } } as ReturnType<
            typeof actual.selectTabBySessionId
          >)
        : actual.selectTabBySessionId(state, id),
  };
});

import { SessionStatusMark } from "./session-status-mark";
import {
  resetPrunedAgentSessionsForTests,
  useAgentStateStore,
} from "@/shared/stores/agent-state-store";

const UNAVAILABLE = "Agent activity unavailable";

function dot(sessionId: string) {
  render(<SessionStatusMark sessionId={sessionId} data-testid="dot" />);
  return screen.queryByTestId("dot");
}

afterEach(() => {
  useAgentStateStore.getState().remove(["plain", "agent", "remote-agent"]);
  useMachinesStore.setState({ byTeam: {} });
  resetPrunedAgentSessionsForTests();
});

describe("SessionStatusMark — activity freshness", () => {
  it("names the Codex inbox update without blocking its running session", () => {
    useAgentStateStore.getState().recordActivity(
      {
        session: "agent",
        identity: { hostBootId: "boot", sessionIncarnation: "pty", invocationId: "old-codex" },
        revision: 1,
        turnGeneration: 0,
        provider: "codex",
        state: "idle",
        observed: false,
        limited: true,
        exit: null,
        asks: [],
        reason: "inbox-update-required",
      },
      Infinity,
    );
    expect(dot("agent")?.getAttribute("aria-label")).toContain(
      "Codex is out of date for inbox wake. Update to 0.159.3+",
    );
  });

  it("a running Hermes preview reports unknown activity instead of Ready", () => {
    act(() => {
      useAgentStateStore.getState().setProcessAgent("agent", "hermes");
      useAgentStateStore.getState().activityUnavailable(["agent"], "unavailable");
    });
    const label = dot("agent")?.getAttribute("aria-label") ?? "";
    expect(label).not.toMatch(/Ready/i);
    expect(label).toMatch(/Unknown|Hooks not connected|unavailable/i);
  });

  it("a plain shell has no agent activity to be stale about: no dot", () => {
    act(() => useAgentStateStore.getState().activityUnavailable(["plain"], "unavailable"));
    expect(dot("plain")).toBeNull();
  });

  it("a plain shell shows no restoring dot while its host recovers", () => {
    act(() => useAgentStateStore.getState().activityUnavailable(["plain"], "recovering"));
    expect(dot("plain")).toBeNull();
  });

  it("an agent found by its running process says its activity is unavailable", () => {
    act(() => {
      useAgentStateStore.getState().setProcessAgent("agent", "claude");
      useAgentStateStore.getState().activityUnavailable(["agent"], "unavailable");
    });
    const mark = dot("agent");
    expect(mark).toHaveAttribute("aria-label", `Activity unknown · ${UNAVAILABLE}`);
    expect(mark).toHaveAttribute("data-mark", "unknown");
  });

  it("an out-of-date terminal server: a local tab points at the panel, a remote one at its machine", () => {
    act(() => {
      useAgentStateStore.getState().setProcessAgent("agent", "claude");
      useAgentStateStore.getState().setProcessAgent("remote-agent", "claude");
      for (const session of ["agent", "remote-agent"]) {
        useAgentStateStore.getState().recordActivity(
          {
            session,
            identity: { hostBootId: "boot", sessionIncarnation: "pty", invocationId: "run" },
            revision: 1,
            turnGeneration: 1,
            provider: "claude",
            state: "thinking",
            observed: true,
            limited: false,
            exit: null,
            asks: [],
            activityAgeMs: 0,
          },
          Infinity,
        );
      }
      useAgentStateStore
        .getState()
        .activityUnavailable(["agent", "remote-agent"], "upgrade-required");
      useMachinesStore.setState({
        byTeam: { "acme:default": [{ machineId: "box", label: "build-box" } as MachineSummary] },
      });
    });
    expect(dot("agent")?.getAttribute("aria-label")).toContain(
      "Review the update in the terminal panel",
    );
    render(<SessionStatusMark sessionId="remote-agent" data-testid="remote-dot" />);
    const label = screen.getByTestId("remote-dot").getAttribute("aria-label");
    expect(label).toContain("The terminal server on build-box needs an update");
    expect(label).toContain("Update terminal server");
    expect(label).not.toContain("terminal panel");
  });
});
