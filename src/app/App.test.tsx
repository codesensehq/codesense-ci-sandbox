// SPDX-License-Identifier: MIT
vi.mock("@/features/viewer/hooks/use-agent-roster-listener", () => ({
  useAgentRosterListener: () => {},
}));
import { describe, it, expect, vi } from "vitest";
import { currentContextNavigationRevision } from "@/shared/lib/context-navigation";
import { render, screen, fireEvent } from "@testing-library/react";
import App from "./App";

// Mock Tauri APIs
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));
// The rendered-document bridge listens on the main webview window.
vi.mock("@tauri-apps/api/webviewWindow", () => ({
  getCurrentWebviewWindow: () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }),
}));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    startDragging: vi.fn(),
    show: vi.fn(() => Promise.resolve()),
    close: vi.fn(() => Promise.resolve()),
    onCloseRequested: vi.fn(() => Promise.resolve(() => {})),
    // SF-8 (AC-1): `useWindowTitle` calls `setTitle` on chrome change.
    setTitle: vi.fn(() => Promise.resolve()),
    // SW-WA-E11: useNeedsYouNotifications reads focus + subscribes to focus changes.
    isFocused: vi.fn(() => Promise.resolve(true)),
    onFocusChanged: vi.fn(() => Promise.resolve(() => {})),
  }),
}));
// This suite exercises other inputs; ActivityV1 recovery has its own seam tests.
vi.mock("@/shared/hooks/use-terminal-activity", () => ({ useTerminalActivity: () => {} }));
vi.mock("@/tauri/commands", () => ({
  // Custom right panes: the app-wide pane registration lists them.
  listPanes: vi.fn().mockResolvedValue({ contentRoot: "", chain: [], panes: [] }),
  // Agent tabs follow their agents' names: the app reads the agents on mount.
  agentsList: vi.fn().mockResolvedValue([]),
  // SW-LN-B20: the backend guest-session declaration.
  setGuestSession: vi.fn(() => Promise.resolve()),
  getGuestSession: vi.fn(() => Promise.resolve(false)),
  // The shell's terminal-server prompt: a compatible server.
  terminalServerStatus: vi.fn(() => Promise.resolve(null)),
  restartTerminalServer: vi.fn(() => Promise.resolve(null)),
  startWatching: vi.fn(() => Promise.resolve()),
  stopWatching: vi.fn(() => Promise.resolve()),
  detectTools: vi.fn(() =>
    Promise.resolve({ terminals: [], editors: [], default_terminal: "", default_editor: "" }),
  ),
  reconfigureWarmPool: vi.fn(() => Promise.resolve()),
  setTerminalShellOverride: vi.fn(() => Promise.resolve()),
  // SW-DX-17d: App mounts useBackgroundAgentLimit, which pushes the limit to Rust.
  dashboardSetBackgroundLimit: vi.fn(() => Promise.resolve()),
  // SW-AU-34: App mounts useQuitOnCloseSetting, which pushes the preference to Rust.
  setQuitOnClose: vi.fn(() => Promise.resolve()),
  // SV-8: App mounts useTerminalServerMenuBarSetting, which pushes the setting to Rust.
  setTerminalServerMenuBar: vi.fn(() => Promise.resolve()),
  // SW-RP-1: App mounts useRightPaneMenuSync, which rebuilds the native menu.
  rebuildMenu: vi.fn(() => Promise.resolve()),
  // SW-AM-9: App mounts useAgentRosterListener, which warms the roster store on mount.
  dashboardListSessions: vi.fn(() => Promise.resolve([])),
  // SW-WV-B3: the roster store now warms from the durable `wv_roster` ledger.
  wvListRoster: vi.fn(() => Promise.resolve([])),
  // SW-WU-9: App mounts useWorktreeRosterListener, which warms the worktree store on mount.
  listManagedWorktrees: vi.fn(() => Promise.resolve({ rows: [], reaped: 0 })),
  // SW-WA-E11: App mounts useNeedsYouNotifications, which reads the attention board.
  listAttentionBoard: vi.fn(() =>
    Promise.resolve({ youOwe: [], owedToYou: [], onAgent: [], parked: [], doneUnacked: [] }),
  ),
  getLeaseStatus: vi.fn(() =>
    Promise.resolve({ held: false, holderSessionId: null, heartbeatAt: null }),
  ),
}));

describe("App", () => {
  it("renders the CodeSense splash on initial load", () => {
    render(<App />);
    expect(screen.getByText("CodeSense")).toBeInTheDocument();
  });
});

it("App capture revokes pending navigation from a portaled gesture and ignores passive focus", () => {
  const mounted = render(<App />);
  const editor = document.createElement("textarea");
  document.body.append(editor);
  const pendingRevision = currentContextNavigationRevision();
  try {
    editor.focus();
    editor.blur();
    expect(currentContextNavigationRevision()).toBe(pendingRevision);
    fireEvent.pointerDown(editor);
    expect(currentContextNavigationRevision()).toBeGreaterThan(pendingRevision);
    const pointerRevision = currentContextNavigationRevision();
    fireEvent.keyDown(editor, { key: "ArrowDown" });
    expect(currentContextNavigationRevision()).toBeGreaterThan(pointerRevision);
    mounted.unmount();
    const unmountedRevision = currentContextNavigationRevision();
    fireEvent.pointerDown(editor);
    expect(currentContextNavigationRevision()).toBe(unmountedRevision);
  } finally {
    editor.remove();
  }
});
