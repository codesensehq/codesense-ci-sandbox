// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach } from "vitest";
import { selectWorkspaceTabs, useViewerStore } from "./viewer-store";
import {
  seedTerminalTabs,
  terminalHost,
  installTerminalLayoutHarness,
} from "@/test/terminal-layout-harness";

// SW-GW-5: `removeTerminalTabBySession` is the session-keyed tab close the dashboard
// heal uses to evict a superseded (parked/dead) session's tab when it holds only a
// sessionId. It must find the tab in an open OR a minimized workspace, and still
// reach the host when the tab isn't shown.

installTerminalLayoutHarness();

const TEAM = "org:default";
const WS = "story:SW-GW-5";
// A role-tagged agent tab: closing it never opens a replacement shell.
const agentTab = (session: string) => ({ session, name: "Agent", role: "story-dev:X" });
const settle = () => new Promise((r) => setTimeout(r));
const tabs = () => selectWorkspaceTabs(useViewerStore.getState(), TEAM, WS);

beforeEach(() => {
  useViewerStore.setState({
    workspaces: { [TEAM]: [{ id: WS, kind: "generic", cwd: "/w", status: "ready" }] },
    activeWorkspace: {},
    softClosedWorkspaces: {},
  });
});

describe("removeTerminalTabBySession (SW-GW-5)", () => {
  it("closes an open workspace's tab found only by its sessionId", async () => {
    seedTerminalTabs(TEAM, WS, [agentTab("s-1")]);
    useViewerStore.getState().removeTerminalTabBySession("s-1");
    await settle();
    expect(tabs()).toHaveLength(0);
    expect(terminalHost().contains("s-1")).toBe(false);
  });

  it("closes a MINIMIZED workspace's tab by sessionId", async () => {
    seedTerminalTabs(TEAM, WS, [agentTab("s-2")]);
    useViewerStore.getState().closeWorkspace(TEAM, WS);
    expect(useViewerStore.getState().softClosedWorkspaces[TEAM]?.[WS]).toBeDefined();
    useViewerStore.getState().removeTerminalTabBySession("s-2");
    await settle();
    expect(tabs()).toHaveLength(0);
  });

  it("reaches the host for a session the viewer doesn't show (kill ends it)", async () => {
    terminalHost().seed(
      `${TEAM}|elsewhere`,
      { displayName: "x", kind: "test" },
      {
        session: "s-3",
        placement: "background",
      },
    );
    useViewerStore.getState().removeTerminalTabBySession("s-3", { kill: true });
    await settle();
    expect(terminalHost().contains("s-3")).toBe(false);
  });

  it("reports a host refusal when no tab carries that session", async () => {
    seedTerminalTabs(TEAM, WS, [agentTab("s-1")]);
    await expect(
      useViewerStore.getState().removeTerminalTabBySession("no-such-session"),
    ).rejects.toBeDefined();
    expect(tabs()).toHaveLength(1);
  });
});
