// SPDX-License-Identifier: MIT
import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";

import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { useManagedSessionStore } from "@/shared/stores/use-managed-session-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { USER_HIDDEN_TAG } from "@/shared/lib/terminal-layout-projection";
import { installTerminalLayoutHarness, seedTerminalTabs } from "@/test/terminal-layout-harness";
import { useSessionsNeedingYou, waitingReason } from "./use-sessions-needing-you";

installTerminalLayoutHarness();

const TEAM = "acme:core";
const tab = (id: string) => ({ session: id, name: `tab ${id}` });

beforeEach(() => {
  useAgentStateStore.setState({ bySession: {}, exitBySession: {}, stateSince: {}, seenAt: {} });
  useManagedSessionStore.setState({ pendingAsks: {} });
  useViewerStore.setState({
    aiStatusBySession: { blocked: "needs-permission" },
    workspaces: {
      [TEAM]: [{ id: "w1", kind: "story", cwd: "/w1", status: "ready", storyId: "ST-1" }],
    },
    softClosedWorkspaces: {},
  });
  seedTerminalTabs(TEAM, "w1", [
    tab("blocked"),
    tab("claude"),
    { ...tab("hidden"), placement: "background", tags: { [USER_HIDDEN_TAG]: "tab" } },
  ]);
});

describe("Weft's needs-you list reads the one definition", () => {
  it("names the current waiting kind, regardless of a retained progress note", () => {
    expect(waitingReason("permission", { phase: "blocked", note: "old block" })).toBe(
      "Needs approval",
    );
    expect(waitingReason("blocked", { phase: "blocked", note: "disk full" })).toBe(
      "Blocked: disk full",
    );
    expect(waitingReason("blocked", { phase: "dev", note: "old work" })).toBe("Blocked");
    expect(waitingReason("attention", undefined)).toBe("Needs you");
    expect(waitingReason("startup", undefined)).toBe("Waiting on a startup screen");
  });
  it("includes a crash and a Hidden tab's question, labelled, and drops them when resolved", () => {
    const { result } = renderHook(() => useSessionsNeedingYou(TEAM));
    expect(result.current.map((s) => [s.sessionId, s.kind, s.label, s.tabName])).toEqual([
      ["blocked", "permission", "ST-1", "tab blocked"],
    ]);

    act(() => {
      const agents = useAgentStateStore.getState();
      agents.recordSignal("claude", {
        tier: 1,
        signal: { kind: "claude-hook", event: "UserPromptSubmit" },
      });
      agents.recordCommandEnd("claude", 137);
      useManagedSessionStore.setState({
        pendingAsks: { q1: { correlationId: "q1", sessionId: "hidden", question: "Ship?" } },
      });
    });
    const rows = result.current.map((s) => [s.sessionId, s.kind, s.hidden]);
    expect(rows).toContainEqual(["claude", "crashed", false]);
    expect(rows).toContainEqual(["hidden", "question", true]);
    expect(rows).toHaveLength(3);

    act(() => useManagedSessionStore.setState({ pendingAsks: {} }));
    expect(result.current.map((s) => s.sessionId).sort()).toEqual(["blocked", "claude"]);
  });
});
