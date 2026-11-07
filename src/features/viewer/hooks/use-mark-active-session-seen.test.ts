// SPDX-License-Identifier: MIT
import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useMarkActiveSessionSeen } from "./use-mark-active-session-seen";
import { resetDotSinceForTests } from "@/features/viewer/lib/cli-workspace/store/terminal-tab-selectors";
import { isSessionSeen, useAgentStateStore } from "@/shared/stores/agent-state-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import {
  installTerminalLayoutHarness,
  seedTerminalTabs,
  terminalHost,
} from "@/test/terminal-layout-harness";

installTerminalLayoutHarness();

const TEAM = "acme:core";
let KEY = "";
/** Make `session`'s tab the workspace's active one (the host's focus verb). */
const activate = (session: string) => act(() => terminalHost().setActive(KEY, session));
let focused = true;
let now = 1_000;

function crash(sessionId: string) {
  act(() => {
    const store = useAgentStateStore.getState();
    store.recordSignal(sessionId, {
      tier: 1,
      signal: { kind: "claude-hook", event: "UserPromptSubmit" },
    });
    store.recordCommandEnd(sessionId, 137);
  });
}
const seen = (sessionId: string) => isSessionSeen(useAgentStateStore.getState(), sessionId);

beforeEach(() => {
  focused = true;
  vi.spyOn(document, "hasFocus").mockImplementation(() => focused);
  vi.spyOn(Date, "now").mockImplementation(() => ++now);
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "core" });
  resetDotSinceForTests();
  useAgentStateStore.setState({ bySession: {}, exitBySession: {}, stateSince: {}, seenAt: {} });
  useViewerStore.setState({
    activeWorkspace: { [TEAM]: "ws" },
    workspaces: {},
    terminalVisible: true,
    softClosedWorkspaces: {},
    aiStatusBySession: {},
  });
  KEY = seedTerminalTabs(TEAM, "ws", ["s-a", "s-b"], { active: "s-a" });
});
afterEach(() => vi.restoreAllMocks());

describe("useMarkActiveSessionSeen", () => {
  it("a crash in the tab being watched is seen at once; another tab's is not", () => {
    renderHook(() => useMarkActiveSessionSeen());
    crash("s-a");
    crash("s-b");
    expect(seen("s-a")).toBe(true);
    expect(seen("s-b")).toBe(false);
  });

  it("switching to the tab sees it", () => {
    renderHook(() => useMarkActiveSessionSeen());
    crash("s-b");
    expect(seen("s-b")).toBe(false);
    activate("s-b");
    expect(seen("s-b")).toBe(true);
  });

  it("an unfocused window sees nothing until it regains focus", async () => {
    focused = false;
    renderHook(() => useMarkActiveSessionSeen());
    crash("s-a");
    expect(seen("s-a")).toBe(false);
    focused = true;
    await act(async () => {
      window.dispatchEvent(new Event("focus"));
    });
    expect(seen("s-a")).toBe(true);
  });

  it("a hidden terminal pane sees nothing; reopening it sees the crash", () => {
    act(() => useViewerStore.setState({ terminalVisible: false }));
    renderHook(() => useMarkActiveSessionSeen());
    crash("s-a");
    expect(seen("s-a")).toBe(false);
    act(() => useViewerStore.setState({ terminalVisible: true }));
    expect(seen("s-a")).toBe(true);
  });

  it("watches the active team's tab only; another open team's focused tab is not watched", () => {
    const OTHER = "acme:other";
    seedTerminalTabs(OTHER, "ws-o", ["s-o"], { active: "s-o" });
    act(() =>
      useViewerStore.setState({
        openedTeams: [TEAM, OTHER],
        activeWorkspace: { [TEAM]: "ws", [OTHER]: "ws-o" },
        workspaces: { [OTHER]: [{ id: "ws-o", kind: "story", cwd: "/o", status: "ready" }] },
        terminalVisible: true,
      }),
    );
    renderHook(() => useMarkActiveSessionSeen());
    crash("s-o");
    crash("s-a");
    expect(seen("s-a")).toBe(true);
    expect(seen("s-o")).toBe(false);
    // Switching to that team watches its tab.
    act(() => useOrgStore.setState({ activeTeamName: "other" }));
    expect(seen("s-o")).toBe(true);
  });

  it("a minimized workspace's tab is not being watched", () => {
    act(() =>
      useViewerStore.setState({
        softClosedWorkspaces: {
          [TEAM]: { ws: { workspace: { id: "ws" } } as never },
        },
      }),
    );
    renderHook(() => useMarkActiveSessionSeen());
    crash("s-a");
    expect(seen("s-a")).toBe(false);
  });

  it("a stored-only status is seeable: its dot start is stamped too", () => {
    renderHook(() => useMarkActiveSessionSeen());
    act(() => useViewerStore.getState().setSessionAiStatus("s-b", "error"));
    expect(seen("s-b")).toBe(false);
    activate("s-b");
    expect(seen("s-b")).toBe(true);
  });

  it("a watcher restart keeps a seen crash seen", () => {
    const first = renderHook(() => useMarkActiveSessionSeen());
    crash("s-b");
    activate("s-b");
    expect(seen("s-b")).toBe(true);
    // Look away, stop every watcher, start again.
    activate("s-a");
    first.unmount();
    renderHook(() => useMarkActiveSessionSeen());
    expect(seen("s-b")).toBe(true);
  });
});
