// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, render, screen } from "@testing-library/react";
import { RemoteTerminalPane, forgetRemoteAttaches } from "./remote-terminal-pane";
import { useViewerStore, selectWorkspaceTabs } from "@/shared/stores/viewer-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { useMachinesStore } from "@/features/machines/stores/machines-store";
import { useAgentListProjectionStore } from "../lib/agent-list-projection";
import { HTML_BRIDGE_ALLOWLIST as bridge } from "@/features/dashboard/components/html-bridge";
import { activateAndFocus, currentActivationGeneration } from "../lib/focus-cascade";
import { terminalManager } from "@/shared/terminal/terminal-manager";
import {
  connectTestRemoteHost,
  installTerminalLayoutHarness,
  refetchTestLayout,
} from "@/test/terminal-layout-harness";
import { workspaceKey } from "@/shared/lib/workspace-key";
import { BOX, TEAM, WS } from "./remote-tabs.seam.fixture";
import type { AgentSessionRow } from "@/tauri/commands";

vi.mock("@/shared/lib/private-terminal-authority", async (original) => ({
  ...(await original<typeof import("@/shared/lib/private-terminal-authority")>()),
  acceptsPrivateAuthority: () => true,
  currentPrivateAuthorityEpoch: () => "7",
}));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  saveAppState: vi.fn(async () => {}),
  pushViewerState: vi.fn(async () => {}),
}));
vi.mock(
  "@/features/viewer/lib/cli-workspace/transport/terminal-launch-source",
  async (original) => ({
    ...(await original<
      typeof import("@/features/viewer/lib/cli-workspace/transport/terminal-launch-source")
    >()),
    remoteShellTab: vi.fn(async () => {
      const { testRemoteHost } = await import("@/test/terminal-layout-source-mock");
      return {
        status: "alreadyLive",
        headed: false,
        workspaceKey: `${TEAM}|${WS}`,
        target: { ...testRemoteHost!.target(), session: "remote-focus" },
      };
    }),
  }),
);

installTerminalLayoutHarness();
let textarea: HTMLTextAreaElement;
beforeEach(() => {
  forgetRemoteAttaches();
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
  useViewerStore.setState({
    workspaces: { [TEAM]: [{ id: WS, kind: "generic", cwd: "/w", status: "ready" }] },
    activeWorkspace: { [TEAM]: WS },
    softClosedWorkspaces: {},
  });
  useMachinesStore.setState({ byTeam: { [TEAM]: [BOX] } });
  const remote = connectTestRemoteHost("box", "7");
  remote.seed(
    workspaceKey(TEAM, { id: WS }),
    { kind: "generic", displayName: WS, org: "acme", team: "default" },
    {
      session: "remote-focus",
      tags: { remoteHandle: "remote-handle", remoteRequestId: "request-focus" },
    },
  );
  refetchTestLayout();
  useAgentListProjectionStore.setState({
    sessions: [
      {
        handle: "remote-handle",
        sessionId: "remote-focus",
        alive: true,
        kind: "dashboard-launched",
        backgrounded: false,
        startedWith: null,
        startedAtMs: null,
        launcher: null,
        lastOutputMs: null,
      },
    ] as AgentSessionRow[],
  });
  textarea = document.createElement("textarea");
  textarea.setAttribute("aria-label", "remote xterm");
  // Keep the real receiver and TerminalSlot; replace only the terminal display manager boundary.
  vi.spyOn(terminalManager, "ensure").mockResolvedValue(null as never);
  vi.spyOn(terminalManager, "attach").mockImplementation((_reference, slot, opts) => {
    slot.append(textarea);
    queueMicrotask(() => {
      if (typeof opts?.focus === "function" ? opts.focus() : opts?.focus) textarea.focus();
    });
  });
  vi.spyOn(terminalManager, "detach").mockImplementation(() => {});
  vi.spyOn(terminalManager, "setVisible").mockImplementation(() => {});
  vi.spyOn(terminalManager, "focus").mockImplementation((_reference, policy = true) => {
    if (typeof policy === "function" ? policy() : policy) textarea.focus();
  });
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function mountReceiver() {
  const tab = selectWorkspaceTabs(useViewerStore.getState(), TEAM, WS).find(
    (tab) => tab.sessionId === "remote-focus",
  )!;
  return render(
    <RemoteTerminalPane
      tab={tab}
      teamKey={TEAM}
      workspaceId={WS}
      workspaceKey={workspaceKey(TEAM, { id: WS })}
      visible
      autoFocus
      attachGeneration={currentActivationGeneration(TEAM)}
      fontSizePx={13}
    />,
  );
}

it("keeps focus on the calling frame through a real remote terminal reveal", async () => {
  const frame = document.createElement("iframe");
  document.body.append(frame);
  frame.focus();
  try {
    await act(async () => {
      await bridge.gotoAgent({ agentId: "remote-handle", preserveFocus: true });
    });
    mountReceiver();
    await screen.findByLabelText("remote xterm");
    await act(async () => {});
    expect(document.activeElement).toBe(frame);
  } finally {
    frame.remove();
  }
});

it("allows the normal remote reveal when no other surface holds focus", async () => {
  activateAndFocus({ teamKey: TEAM, workspaceId: WS });
  mountReceiver();
  await screen.findByLabelText("remote xterm");
  await act(async () => {});
  expect(document.activeElement).toBe(textarea);
});

it("focuses a cold remote attach after the default Show focus frame has already run", async () => {
  const frames: FrameRequestCallback[] = [];
  vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
    frames.push(callback);
    return frames.length;
  });
  const frame = document.createElement("iframe");
  document.body.append(frame);
  frame.focus();
  try {
    await act(async () => {
      await bridge.gotoAgent({ agentId: "remote-handle" });
    });
    expect(frames.length).toBeGreaterThan(0);
    expect(screen.queryByLabelText("remote xterm")).toBeNull();
    act(() => frames.splice(0).forEach((callback) => callback(0)));
    expect(document.activeElement).toBe(frame);

    mountReceiver();
    await screen.findByLabelText("remote xterm");
    await act(async () => {});
    expect(document.activeElement).toBe(textarea);
  } finally {
    frame.remove();
  }
});

it("vetoes an attach superseded by a preserve-focus activation", async () => {
  const since = currentActivationGeneration(TEAM);
  activateAndFocus({ teamKey: TEAM, workspaceId: WS, focus: "preserve-focus" });
  const tab = selectWorkspaceTabs(useViewerStore.getState(), TEAM, WS)[0];
  render(
    <RemoteTerminalPane
      tab={tab}
      teamKey={TEAM}
      workspaceId={WS}
      workspaceKey={workspaceKey(TEAM, { id: WS })}
      visible
      autoFocus
      attachGeneration={since}
      fontSizePx={13}
    />,
  );
  await screen.findByLabelText("remote xterm");
  await act(async () => {});
  expect(document.activeElement).not.toBe(textarea);
});
