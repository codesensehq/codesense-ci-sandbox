// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

// Verify the ⌘I shortcut routes to the agent inbox rail store with the focused
// tile's active tab's sessionId, and ⌘⇧I toggles the full-screen rollup.

const getActiveWorkspaceContextMock = vi.fn();
vi.mock("@/shared/hooks/use-active-workspace-context", () => ({
  getActiveWorkspaceContext: () => getActiveWorkspaceContextMock(),
  // The CLI shows the active team in these tests.
}));

const selectActiveTerminalTabMock = vi.fn();
vi.mock("@/shared/stores/viewer-store", async () => {
  const actual = await vi.importActual<typeof import("@/shared/stores/viewer-store")>(
    "@/shared/stores/viewer-store",
  );
  return {
    ...actual,
    selectActiveTerminalTab: (...args: unknown[]) => selectActiveTerminalTabMock(...args),
  };
});

import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { SHORTCUT_ACTIONS } from "./registry";
import { useAgentInboxRailStore } from "@/features/viewer/stores/agent-inbox-rail-store";

function run() {
  const action = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.toggle-agent-inbox");
  if (!action) throw new Error("viewer.toggle-agent-inbox not registered");
  action.run();
}

const ws = { id: "w1", kind: "story" as const };

describe("SW-TP CAP-8 viewer.toggle-agent-inbox shortcut action", () => {
  beforeEach(() => {
    useAgentInboxRailStore.setState({ bySession: {} });
    useAgentStateStore.setState({
      bySession: {},
      activityBySession: {},
      exitBySession: {},
      processAgentBySession: { sess1: "codex" },
    });
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: "t1", workspace: ws });
    selectActiveTerminalTabMock.mockReturnValue("sess1");
    vi.clearAllMocks();
  });

  it("is registered with the ⌘I default (⌘ only on macOS) and the View menu entry", () => {
    const action = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.toggle-agent-inbox");
    expect(action).toBeDefined();
    expect(action!.defaultKeystroke).toBe("CmdOrCtrl+I");
    expect(action!.macCmdOnly).toBe(true);
    expect(action!.menuPath).toEqual(["View", "Toggle Agent Inbox"]);
    expect(action!.scope).toBe("workspace");
  });

  it("opens the inbox for the active tab's session when closed", () => {
    run();
    expect(useAgentInboxRailStore.getState().bySession.sess1).toBeDefined();
  });

  it("closes the inbox for the active tab's session when already open", () => {
    useAgentInboxRailStore.getState().open("sess1");
    expect(useAgentInboxRailStore.getState().bySession.sess1).toBeDefined();
    run();
    expect(useAgentInboxRailStore.getState().bySession.sess1).toBeUndefined();
  });

  it("closing with focus inside the inbox hands focus to that tab's terminal", () => {
    useAgentInboxRailStore.getState().open("sess1");
    const pane = document.createElement("div");
    pane.setAttribute("data-pane-id", "sess1");
    pane.innerHTML =
      '<div class="xterm"><textarea data-testid="term"></textarea></div>' +
      '<aside data-testid="inbox-rail" data-session-id="sess1"><textarea data-testid="box"></textarea></aside>';
    document.body.appendChild(pane);
    pane.querySelector<HTMLTextAreaElement>('[data-testid="box"]')!.focus();
    run();
    expect(document.activeElement).toBe(pane.querySelector('[data-testid="term"]'));
    expect(useAgentInboxRailStore.getState().bySession.sess1).toBeUndefined();
    pane.remove();
  });

  it("does not open for a shell, including a shell with launch history", () => {
    useAgentStateStore.getState().setProcessAgent("sess1", null);
    run();
    expect(useAgentInboxRailStore.getState().bySession).toEqual({});
  });

  it("is a no-op when no active tab resolves", () => {
    selectActiveTerminalTabMock.mockReturnValue(null);
    run();
    expect(useAgentInboxRailStore.getState().bySession).toEqual({});
  });

  it("is a no-op when there is no focused workspace", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: null, workspace: null });
    run();
    expect(useAgentInboxRailStore.getState().bySession).toEqual({});
  });

  it("opens group chat for the focused agent and registers its View command", () => {
    const action = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.open-group-chat")!;
    expect(action.defaultKeystroke).toBe("CmdOrCtrl+Alt+G");
    expect(action.menuPath).toEqual(["View", "Open Group Chat"]);
    expect(action.macCmdOnly).toBe(true);
    action.run();
    expect(useAgentInboxRailStore.getState().bySession.sess1?.groupRequest).toBe(1);
    action.run();
    expect(useAgentInboxRailStore.getState().bySession.sess1?.groupRequest).toBe(2);
    useAgentStateStore.getState().setProcessAgent("sess1", null);
    action.run();
    expect(useAgentInboxRailStore.getState().bySession.sess1).toBeUndefined();
  });
});
