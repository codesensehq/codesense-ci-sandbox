// SPDX-License-Identifier: MIT
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { applyFieldPatch } from "@/shared/lib/field-patch";
import {
  currentContextNavigationRevision,
  installUserNavigationCapture,
} from "@/shared/lib/context-navigation";
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, act, cleanup, fireEvent, screen, within } from "@/test-utils";
import {
  __resetMachineAppearanceForTest,
  setMachineColor,
  useMachineAppearanceStore,
} from "@/features/machines/lib/machine-appearance";
import { DndContext } from "@dnd-kit/core";

// Seam: the real panel tree, tab bar and remote pane, the real viewer store and
// its layout projection, the real remote open path — against This Mac's test
// host and a fake remote Machine's terminal server (a second host admitted as
// the Machine's `remote` member under a private authority). Only the xterm leaf
// and the Tauri boundary are faked.

const fake = vi.hoisted(() => ({
  authority: "7",
  attaches: [] as string[],
  closes: [] as string[],
  opened: 0,
  /** While set, an attach waits for it before the Machine answers. */
  attachGate: null as Promise<void> | null,
}));

vi.mock("./shell-terminal-panel", () => ({
  ShellTerminalPanel: ({ sessionId }: { sessionId: string }) => <div data-sid={sessionId} />,
}));
vi.mock("@/shared/terminal/terminal-slot", () => ({
  TerminalSlot: ({
    sessionId,
    display,
    active,
  }: {
    sessionId: string | { target: { session: string; namespace: { generation: string } } };
    display?: { themeConfig?: { theme: { background?: string } } };
    active: boolean;
  }) =>
    typeof sessionId === "string" ? null : (
      <div
        className="xterm"
        data-remote-sid={sessionId.target.session}
        data-generation={sessionId.target.namespace.generation}
        data-bg={display?.themeConfig?.theme.background}
        data-active={active}
      >
        <textarea aria-label={`remote terminal ${sessionId.target.session}`} />
      </div>
    ),
}));
vi.mock("@/shared/lib/private-terminal-authority", () => ({
  acceptsPrivateAuthority: (epoch: string | null | undefined) =>
    epoch == null || epoch === fake.authority,
  currentPrivateAuthorityEpoch: () => fake.authority,
  onPrivateAuthorityRetired: () => () => {},
  observePrivateAuthority: () => {},
  retirePrivateAuthority: () => {},
  listenForPrivateAuthorityRetirement: () => Promise.resolve(),
}));
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
  listMachines: vi.fn().mockResolvedValue([]),
  agentReadInbox: vi.fn().mockResolvedValue({ messages: [], humanUnread: 0 }),
  sessionsRunningProcesses: vi.fn(async (ids: string[]) =>
    ids.map(() => ({ foreground: null, background: [] })),
  ),
  connectMachine: vi.fn(async () => {
    const { reconnect } = await import("./remote-tabs.seam.fixture");
    return reconnect();
  }),
}));
vi.mock(
  "@/features/viewer/lib/cli-workspace/transport/terminal-launch-source",
  async (importOriginal) => ({
    ...(await importOriginal<
      typeof import("@/features/viewer/lib/cli-workspace/transport/terminal-launch-source")
    >()),
    remoteShellTab: vi.fn(async (request: import("@/tauri/commands").RemoteShellTabRequest) => {
      const { testRemoteHost: host } = await import("@/test/terminal-layout-source-mock");
      const { workspaceKey } = await import("@/shared/lib/workspace-key");
      if (!host?.online) return { status: "unknown", headed: false, message: "offline" };
      if (request.operation === "create") {
        fake.opened += 1;
        const session = `remote-shell-${fake.opened}`;
        const key = workspaceKey(request.teamKey, { id: request.workspaceId });
        host.openTab({
          machineId: host.machineId,
          workspace: key,
          session,
          descriptor: { displayName: "ws", kind: "generic", org: "acme", team: "default" },
          placement: "tab",
          activate: false,
          // As the backend tags a remote tab: its launch and agent.
          tags: {
            remoteHandle: `handle-${session}`,
            remoteRequestId: request.requestId,
            remoteWorkspaceId: request.workspaceId,
            remoteLaunch: JSON.stringify(request.launch ?? "shell"),
            ...(request.agentKey ? { remoteAgentKey: request.agentKey } : {}),
          },
          spawn: { kind: "warm", targetCwd: "/home/me", size: { cols: 80, rows: 24 } },
        });
        return {
          status: "opened",
          headed: true,
          handle: `handle-${session}`,
          requestId: request.requestId,
          workspaceId: request.workspaceId,
          workspaceKey: key,
          target: { ...host.target(), session },
        };
      }
      if (fake.attachGate) await fake.attachGate;
      const reference = request.reference;
      const handle = "handle" in reference ? reference.handle : "";
      const snapshot = host.snapshot();
      for (const [key, layout] of Object.entries(snapshot.workspaces)) {
        const tab = layout.tabs.find((t) => t.tags.remoteHandle === handle);
        if (!tab) continue;
        fake.attaches.push(`${tab.session}@${host.generation}`);
        return {
          status: "alreadyLive",
          headed: false,
          handle,
          workspaceKey: key,
          target: { ...host.target(), session: tab.session },
        };
      }
      return { status: "unknown", headed: false, message: "not found" };
    }),
  }),
);
vi.mock("./source-editor-inset", () => ({ WorktreeEditorSurface: () => null }));
vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn(), info: vi.fn() }),
}));
vi.mock("@/features/viewer/lib/blocked-nav-cue", () => ({
  flashBlockedNavForWorkspace: vi.fn(),
  flashBlockedNav: vi.fn(),
}));
vi.mock("@/shared/lib/default-agent", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/shared/lib/default-agent")>()),
  resolveAgentCliPath: () => Promise.resolve(null),
}));
vi.mock("@/features/dashboard/components/markdown-viewer", () => ({ MarkdownViewer: () => null }));
vi.mock("@/shared/lib/pipeline-graph/use-story-details", () => ({
  useStoryDetails: vi.fn(() => ({ data: { title: null, storyText: "" }, loading: false })),
}));

import {
  EMPTY_TERMINAL_LAYOUT,
  type TerminalLayoutProjection,
} from "@/shared/lib/terminal-layout-projection";
import {
  selectActiveTerminalTab,
  selectHiddenTerminalTabs,
  selectTabGridCollapsed,
  selectWorkspaceTabs,
  shouldRenderTabGrid,
  useViewerStore,
  workspacePanedTabIds,
  workspaceTabGridPreset,
} from "@/shared/stores/viewer-store";
import { useMachinesStore } from "@/features/machines/stores/machines-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { saveAppState } from "@/tauri/commands";
import {
  connectTestRemoteHost,
  installTerminalLayoutHarness,
  refetchTestLayout,
  seedTerminalTabs,
  terminalHost,
} from "@/test/terminal-layout-harness";
import { testRemoteHost } from "@/test/terminal-layout-source-mock";
import { openRemoteTab } from "../lib/remote-tab-open";
import { remoteShellTab } from "../lib/cli-workspace/transport/terminal-launch-source";
import { remoteStoryWorkspaceDriver } from "@/features/machines/lib/remote-story-workspace";
import { showAllTabs } from "../lib/terminal-tab-keys";
import {
  closeActiveTerminalTab,
  closeTab,
  endTabs,
  endWorkspace,
  settleEndConfirm,
  useEndConfirmStore,
} from "../lib/terminal-close";
import { WorkspaceTerminalPanels } from "./workspace-terminal-panels";
import { showAgent } from "../lib/show-agent";
import { routeToAttention } from "../lib/attention-alerts-controller";
import type { AgentSessionRow } from "@/tauri/commands";

/** The agents list's row for an agent running in a remote tab. */
function remoteRow(sessionId: string): AgentSessionRow {
  return {
    handle: `handle-${sessionId}`,
    sessionId,
    kind: "dashboard-launched",
    alive: true,
    backgrounded: false,
    startedWith: null,
    startedAtMs: null,
    launcher: null,
    lastOutputMs: null,
  };
}
import { forgetRemoteAttaches } from "./remote-terminal-pane";
import {
  closeAgentInboxRail,
  openAgentInboxRail,
} from "@/features/viewer/stores/agent-inbox-rail-store";
import { workspaceKey } from "@/shared/lib/workspace-key";
import { toast } from "sonner";
import { requestNewWorkspaceTerminal } from "../lib/new-workspace-terminal";
import { NEW_TAB_NO_ROOM } from "../lib/terminal-tab-keys";
import { SHORTCUT_ACTIONS } from "@/shared/lib/shortcuts/registry";
import { flashBlockedNavForWorkspace } from "@/features/viewer/lib/blocked-nav-cue";
import { resetSplitChainsForTests, resetZoomedCellsForTests } from "../lib/terminal-tab-keys";
import { dragSeparator, installPanelGeometry, VIEWPORT } from "@/test/panel-geometry";
import { terminalManager } from "@/shared/terminal/terminal-manager";
import { BOX, TEAM, WS, disconnect } from "./remote-tabs.seam.fixture";

installTerminalLayoutHarness();

const s = () => useViewerStore.getState();
const tabsIn = () => selectWorkspaceTabs(s(), TEAM, WS).map((t) => t.id);
const settle = () => act(async () => new Promise((r) => setTimeout(r, 30)));
const remotePane = (session: string) =>
  document.querySelector<HTMLElement>(`[data-remote-sid="${session}"]`);

function renderApp() {
  return render(
    <DndContext>
      {/* The terminal area, as the team view marks it. */}
      <div data-xterm-host data-repo-path="/repo">
        <WorkspaceTerminalPanels teamKey={TEAM} repoPath="/repo" />
      </div>
    </DndContext>,
  );
}

const panedIn = () => workspacePanedTabIds(s(), TEAM, WS);
const cells = () =>
  [...document.querySelectorAll<HTMLElement>('[data-testid^="terminal-grid-cell-"]')].map((el) =>
    el.dataset.testid!.replace("terminal-grid-cell-", ""),
  );
const runAction = (id: string) => act(() => SHORTCUT_ACTIONS.find((a) => a.id === id)!.run());

beforeEach(() => {
  vi.clearAllMocks();
  resetSplitChainsForTests();
  resetZoomedCellsForTests();
  Object.defineProperty(navigator, "platform", { value: "MacIntel", configurable: true });
  fake.authority = "7";
  forgetRemoteAttaches();
  fake.closes = [];
  fake.attaches = [];
  fake.opened = 0;
  fake.attachGate = null;
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
  useViewerStore.setState({
    foregroundLayout: {},
    activeWorkspace: { [TEAM]: WS },
    terminalVisible: true,
    workspaces: { [TEAM]: [{ id: WS, kind: "generic", cwd: "/a", status: "ready" }] },
  });
  useMachinesStore.setState({ byTeam: { [TEAM]: [BOX] } });
  seedTerminalTabs(TEAM, WS, ["local-1"], { active: "local-1" });
  const remote = connectTestRemoteHost("box", "7");
  const close = remote.closeTab.bind(remote);
  remote.closeTab = (session, mode) => {
    fake.closes.push(`${session}:${mode}`);
    return close(session, mode);
  };
});

afterEach(() => {
  cleanup();
  __resetMachineAppearanceForTest();
});

/** Open a remote tab beside `local-1` and let it attach. */
async function openBeside() {
  act(() => {
    openRemoteTab({ teamKey: TEAM, workspaceId: WS, machineId: "box", label: BOX.label });
  });
  await settle();
}

describe("remote terminals are ordinary tabs", () => {
  it("a remote tab opens next to a local one, badged with its machine, focused and attached", async () => {
    renderApp();
    await openBeside();
    expect(tabsIn()).toEqual(["local-1", "remote-shell-1"]);
    expect(selectActiveTerminalTab(s(), TEAM, WS)).toBe("remote-shell-1");
    const mark = screen.getByTestId("tab-remote-shell-1-machine");
    // The machine shows as a server glyph in its color; its name is in the
    // tab's accessible name, so color is never the only cue.
    expect(mark.getAttribute("aria-label")).toBe(`on ${BOX.label}`);
    expect(mark.style.color).not.toBe("");
    expect(
      screen.getByTestId("terminal-tab-remote-shell-1").getAttribute("aria-labelledby"),
    ).toContain(mark.id);
    expect(
      screen.getByTestId("terminal-tab-remote-shell-1").hasAttribute("data-machine-tint"),
    ).toBe(true);
    expect(screen.queryByTestId("tab-local-1-machine")).toBeNull();
    expect(screen.getByTestId("terminal-tab-local-1").hasAttribute("data-machine-tint")).toBe(
      false,
    );
    const pane = remotePane("remote-shell-1");
    expect(pane).not.toBeNull();
    // The remote pane wears one faint watermark naming its machine; the local
    // one stays plain.
    const paneRoot = pane!.closest<HTMLElement>('[data-testid="remote-terminal-pane"]')!;
    const watermark = within(paneRoot).getByTestId("machine-watermark-box");
    // Its terminal paints a background tinted toward the machine's color (the
    // default dark theme's own is #0d1117).
    expect(pane!.dataset.bg).toMatch(/^#[0-9a-f]{6}$/);
    expect(pane!.dataset.bg).not.toBe("#0d1117");
    expect(watermark.textContent).toBe(BOX.label);
    expect(watermark.closest("[aria-hidden]")).not.toBeNull();
    expect(document.querySelectorAll('[data-testid^="machine-canvas-"]')).toHaveLength(1);
    expect(fake.attaches).toEqual(["remote-shell-1@1"]);
  });

  it("a remote tab's menu opens its machine's appearance; a local tab's has no such item", async () => {
    renderApp();
    await openBeside();
    fireEvent.contextMenu(screen.getByTestId("terminal-tab-local-1"));
    expect(screen.queryByText("Machine appearance…")).toBeNull();
    fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
    fireEvent.contextMenu(screen.getByTestId("terminal-tab-remote-shell-1"));
    fireEvent.click(await screen.findByText("Machine appearance…"));
    expect(useMachineAppearanceStore.getState().requested).toBe("box");
  });

  it("a remote tab follows its machine's new color", async () => {
    renderApp();
    await openBeside();
    act(() => setMachineColor("box", "#123456"));
    expect(screen.getByTestId("tab-remote-shell-1-machine").style.color).toMatch(
      /#123456|rgb\(18, 52, 86\)/,
    );
    expect(screen.getByTestId("terminal-tab-remote-shell-1").style.backgroundImage).toMatch(
      /#123456|rgb\(18, 52, 86\)/,
    );
  });

  it("both show side by side in one grid", async () => {
    renderApp();
    await openBeside();
    act(() => showAllTabs(TEAM, WS));
    await settle();
    expect(workspacePanedTabIds(s(), TEAM, WS)).toEqual(["local-1", "remote-shell-1"]);
    const key = workspaceKey(TEAM, { id: WS });
    expect(terminalHost().snapshot().workspaces[key].grid.paned).toEqual(["local-1"]);
    // The Machine's own grid is never arranged: the remote cell is this viewer's.
    expect(testRemoteHost!.snapshot().workspaces[key].grid.paned).toEqual([]);
    expect(s().terminalLayout.grids?.[key]?.paned).toEqual([
      { session: "remote-shell-1", member: "box/remote" },
    ]);
    expect(screen.getByTestId("terminal-grid-cell-local-1")).toBeTruthy();
    expect(screen.getByTestId("terminal-grid-cell-remote-shell-1")).toBeTruthy();
    expect(document.querySelector('[data-sid="local-1"]')).not.toBeNull();
    expect(remotePane("remote-shell-1")).not.toBeNull();
  });

  it("after a restart both tabs come back from their hosts and the remote one reattaches", async () => {
    const first = renderApp();
    await openBeside();
    first.unmount();
    // A relaunch: nothing is kept in the viewer, every host is read again.
    useViewerStore.setState({ terminalLayout: EMPTY_TERMINAL_LAYOUT as TerminalLayoutProjection });
    forgetRemoteAttaches();
    act(() => refetchTestLayout());
    act(() => s().setActiveTerminalTab(TEAM, "local-1", WS));
    renderApp();
    await settle();
    expect(fake.attaches).toEqual(["remote-shell-1@1"]);
    act(() => s().setActiveTerminalTab(TEAM, "remote-shell-1", WS));
    await settle();
    expect(tabsIn()).toEqual(["local-1", "remote-shell-1"]);
    expect(remotePane("remote-shell-1")).not.toBeNull();
    expect(fake.attaches).toEqual(["remote-shell-1@1", "remote-shell-1@1"]);
  });

  it("tab selections autosave remote focus and local clearing without a test-triggered save", async () => {
    await openBeside();
    // Drain setup saves so only each selection can schedule the next write.
    useViewerStore.setState({ stateLoaded: true });
    await s().flushPersist();
    const save = vi.mocked(saveAppState);
    const key = workspaceKey(TEAM, { id: WS });
    let committedFocus = s().terminalLayout.focus ?? {};
    try {
      for (const [tabId, focus] of [
        ["local-1", {}],
        ["remote-shell-1", { [key]: "remote-shell-1" }],
      ] as const) {
        save.mockClear();
        const expectedPatch = { [key]: tabId === "local-1" ? null : tabId };
        let resolveMatchingSave!: () => void;
        const matchingSave = new Promise<void>((resolve) => {
          resolveMatchingSave = resolve;
        });
        save.mockImplementation(async (payload: string) => {
          const savedFocusPatch = JSON.parse(payload).viewer.terminalViewerFocus;
          if (JSON.stringify(savedFocusPatch) === JSON.stringify(expectedPatch))
            resolveMatchingSave();
        });
        act(() => s().setActiveTerminalTab(TEAM, tabId, WS));
        await act(async () => matchingSave);
        expect(save).toHaveBeenCalled();
        const patch = JSON.parse(save.mock.calls.at(-1)![0]).viewer.terminalViewerFocus;
        committedFocus = applyFieldPatch(committedFocus, patch) as Record<string, string>;
        expect(committedFocus).toEqual(focus);
      }
    } finally {
      await s().flushPersist();
      useViewerStore.setState({ stateLoaded: false });
    }
  });

  it("pressing the already-displayed local grid cell clears saved remote focus before reconnect", async () => {
    await openBeside();
    seedTerminalTabs(TEAM, WS, ["local-2"]);
    const key = workspaceKey(TEAM, { id: WS });
    useViewerStore.setState({ terminalLayout: EMPTY_TERMINAL_LAYOUT as TerminalLayoutProjection });
    s().applyLayout({ kind: "focused", key, session: "remote-shell-1" });
    s().applyLayout({ kind: "snapshot", replies: [terminalHost().reply()] });
    act(() => s().gridAllTerminalTabs(TEAM, WS));
    renderApp();
    expect(selectActiveTerminalTab(s(), TEAM, WS)).toBe("local-1");
    // The same focus event an automatically mounted local fallback emits must
    // retain the saved remote selection; only the deliberate press clears it.
    const cell = screen.getByTestId("terminal-grid-cell-local-1");
    act(() => {
      cell.tabIndex = 0;
      cell.focus();
    });
    await settle();
    expect(s().terminalLayout.focus?.[key]).toBe("remote-shell-1");
    fireEvent.pointerDown(cell);
    await settle();
    expect(s().terminalLayout.focus?.[key]).toBeUndefined();
    act(() => refetchTestLayout());
    expect(selectActiveTerminalTab(s(), TEAM, WS)).toBe("local-1");
  });

  it.each(["pointer", "keyboard"] as const)(
    "a newer %s grid-cell selection supersedes notification focus",
    async (via) => {
      const stopCapture = installUserNavigationCapture(document);
      try {
        await openBeside();
        act(() => s().gridAllTerminalTabs(TEAM, WS));
        renderApp();
        const cell = screen.getByTestId("terminal-grid-cell-local-1");
        await act(async () => {
          const pending = routeToAttention({ sessionId: "remote-shell-1", kind: "finished" }, {
            bringWindowForward: async () => {},
          } as Parameters<typeof routeToAttention>[1]);
          await Promise.resolve();
          if (via === "pointer") fireEvent.pointerDown(cell);
          else fireEvent.keyDown(cell, { key: "Tab" });
          cell.tabIndex = 0;
          cell.focus();
          expect(await pending).toBe(true);
        });
        expect(selectActiveTerminalTab(s(), TEAM, WS)).toBe("local-1");
        expect(document.activeElement).toBe(cell);
      } finally {
        stopCapture();
      }
    },
  );

  it.each(["tabs", "grid"] as const)(
    "keyboard caption activation in %s mode supersedes pending notification focus",
    async (mode) => {
      await openBeside();
      if (mode === "grid") act(() => s().gridAllTerminalTabs(TEAM, WS));
      renderApp();
      await settle();
      const caption = screen.getByTestId("terminal-grid-caption-focus-remote-shell-1");
      await act(async () => {
        const pending = routeToAttention({ sessionId: "remote-shell-1", kind: "finished" }, {
          bringWindowForward: async () => {},
        } as Parameters<typeof routeToAttention>[1]);
        await Promise.resolve();
        caption.focus();
        // Native Enter/Space dispatches a click with no pointerdown beforehand.
        fireEvent.click(caption, { detail: 0 });
        expect(await pending).toBe(true);
      });
      expect(document.activeElement).toBe(remotePane("remote-shell-1")?.querySelector("textarea"));
    },
  );

  it("a local host's focus push autosaves the cleared remote override", async () => {
    await openBeside();
    useViewerStore.setState({ stateLoaded: true });
    await s().flushPersist();
    const save = vi.mocked(saveAppState);
    const key = workspaceKey(TEAM, { id: WS });
    try {
      expect(s().terminalLayout.focus?.[key]).toBe("remote-shell-1");
      save.mockClear();
      seedTerminalTabs(TEAM, WS, ["local-2"]);
      terminalHost().setActive(key, "local-2");
      await new Promise((resolve) => setTimeout(resolve, 450));
      expect(s().terminalLayout.focus?.[key]).toBeUndefined();
      expect(save).toHaveBeenCalled();
      const patch = JSON.parse(save.mock.calls.at(-1)![0]);
      expect(patch.viewer.terminalViewerFocus).toEqual({ [key]: null });
      const committed = applyFieldPatch(
        { viewer: { terminalViewerFocus: { [key]: "remote-shell-1" } } },
        patch,
      ) as { viewer: { terminalViewerFocus: Record<string, string> } };
      expect(committed.viewer.terminalViewerFocus).toEqual({});
    } finally {
      await s().flushPersist();
      useViewerStore.setState({ stateLoaded: false });
    }
  });

  it("a disconnect keeps the tab in place, offline with Connect; reconnect reattaches it, no duplicate", async () => {
    renderApp();
    await openBeside();
    act(() => disconnect());
    await settle();
    expect(tabsIn()).toEqual(["local-1", "remote-shell-1"]);
    expect(screen.getByTestId("remote-terminal-offline").textContent).toContain(
      `${BOX.label} offline since`,
    );
    expect(remotePane("remote-shell-1")).toBeNull();
    // The session keeps running on the Machine.
    expect(testRemoteHost!.contains("remote-shell-1")).toBe(true);

    fireEvent.click(screen.getByTestId("remote-terminal-connect"));
    await settle();
    expect(tabsIn()).toEqual(["local-1", "remote-shell-1"]);
    expect(screen.queryByTestId("remote-terminal-offline")).toBeNull();
    expect(remotePane("remote-shell-1")?.dataset.generation).toBe("2");
    expect(fake.attaches).toEqual(["remote-shell-1@1", "remote-shell-1@2"]);
  });

  it("closing a remote tab hides it: it keeps running, and Show puts the same session back", async () => {
    renderApp();
    await openBeside();
    let closed: Promise<boolean> | undefined;
    act(() => {
      closed = closeTab(TEAM, WS, "remote-shell-1");
    });
    expect(await closed).toBe(true);
    await settle();
    // Hidden, not ended: no confirmation, off the strip, still running on its machine.
    expect(useEndConfirmStore.getState().request).toBeNull();
    expect(tabsIn()).toEqual(["local-1"]);
    expect(selectHiddenTerminalTabs(s(), TEAM, WS).map((t) => t.id)).toEqual(["remote-shell-1"]);
    expect(testRemoteHost!.contains("remote-shell-1")).toBe(true);

    let outcome: Awaited<ReturnType<typeof showAgent>> | undefined;
    await act(async () => {
      outcome = await showAgent(remoteRow("remote-shell-1"), { teamKey: TEAM });
    });
    await settle();
    expect(outcome).toEqual({ shown: "restored" });
    expect(tabsIn()).toEqual(["local-1", "remote-shell-1"]);
    expect(selectActiveTerminalTab(s(), TEAM, WS)).toBe("remote-shell-1");
    expect(testRemoteHost!.contains("remote-shell-1")).toBe(true);
  });

  it("End on a remote tab stays explicit and asks first", async () => {
    renderApp();
    await openBeside();
    let ended: Promise<boolean> | undefined;
    act(() => {
      ended = endTabs(TEAM, WS, { tabId: "remote-shell-1" });
    });
    await settle();
    expect(useEndConfirmStore.getState().request?.running).toEqual([
      "Terminal 1 — whatever is running on its machine",
    ]);
    act(() => settleEndConfirm(true));
    expect(await ended).toBe(true);
    await settle();
    expect(tabsIn()).toEqual(["local-1"]);
    expect(testRemoteHost!.contains("remote-shell-1")).toBe(false);
  });

  it("Show on an agent on a tile whose machine is offline shows its tab as it last was and says why", async () => {
    renderApp();
    await openBeside();
    act(() => disconnect());
    await settle();
    let outcome: Awaited<ReturnType<typeof showAgent>> | undefined;
    await act(async () => {
      outcome = await showAgent(remoteRow("remote-shell-1"), { teamKey: TEAM });
    });
    const notice = `${BOX.label} is offline, so handle-remote-shell-1's tab shows how it last was; the agent may still be running there.`;
    expect(outcome).toEqual({ shown: "tab", notice });
    expect(toast).toHaveBeenCalledWith(notice, undefined);
  });

  it("a pane that moves while attaching waits on the same attach, never a second", async () => {
    const first = renderApp();
    await openBeside();
    act(() => s().gridAllTerminalTabs(TEAM, WS));
    await settle();
    first.unmount();
    // A relaunch whose attach is slow to answer.
    forgetRemoteAttaches();
    let answer: () => void = () => {};
    fake.attachGate = new Promise<void>((resolve) => (answer = resolve));
    renderApp();
    act(() => s().setActiveTerminalTab(TEAM, "remote-shell-1", WS));
    await settle();
    expect(screen.getByTestId("terminal-grid-cell-remote-shell-1")).toBeTruthy();
    // Still attaching: one tab at a time, then all tabs again — the pane moves twice.
    act(() => s().collapseTabGridToTabs(TEAM, WS));
    await settle();
    act(() => showAllTabs(TEAM, WS));
    await settle();
    act(() => answer());
    await settle();
    expect(remotePane("remote-shell-1")).not.toBeNull();
    // The open's attach, then the relaunch's one — not one per move.
    expect(fake.attaches).toEqual(["remote-shell-1@1", "remote-shell-1@1"]);
  });

  it("switching to another tab and back shows the display again without a second attach", async () => {
    renderApp();
    await openBeside();
    const pane = remotePane("remote-shell-1");
    act(() => s().setActiveTerminalTab(TEAM, "local-1", WS));
    await settle();
    expect(remotePane("remote-shell-1")).toBe(pane);
    expect(pane).toHaveAttribute("data-active", "false");
    expect(pane?.closest("[inert]")).not.toBeNull();
    act(() => s().setActiveTerminalTab(TEAM, "remote-shell-1", WS));
    await settle();
    expect(remotePane("remote-shell-1")).toBe(pane);
    expect(pane).toHaveAttribute("data-active", "true");
    expect(fake.attaches).toEqual(["remote-shell-1@1"]);
  });

  it("keeps a successful retry attached across tab switches", async () => {
    const first = renderApp();
    await openBeside();
    first.unmount();
    forgetRemoteAttaches();
    fake.attaches = [];
    vi.mocked(remoteShellTab).mockRejectedValueOnce(new Error("Connection interrupted"));
    renderApp();
    await settle();
    expect(screen.getByTestId("remote-terminal-attach-failed")).toHaveTextContent(
      "Connection interrupted",
    );
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await settle();
    const pane = remotePane("remote-shell-1");
    expect(pane).not.toBeNull();
    for (let i = 0; i < 2; i++) {
      act(() => s().setActiveTerminalTab(TEAM, "local-1", WS));
      await settle();
      act(() => s().setActiveTerminalTab(TEAM, "remote-shell-1", WS));
      expect(remotePane("remote-shell-1")).toBe(pane);
      await settle();
    }
    expect(fake.attaches).toEqual(["remote-shell-1@1"]);
  });
});

describe("teardown never ends a remote Machine's session", () => {
  it.each([
    ["closing (evicting) the team", () => s().closeTeam(TEAM)],
    ["a root teardown", () => s().closeWorkspace(TEAM, WS, { bypassRootGuard: true })],
    ["an internal kill", () => s().closeWorkspace(TEAM, WS, { killTerminals: true })],
  ])("%s only detaches it", async (_name, teardown) => {
    await openBeside();
    act(() => teardown());
    await settle();
    expect(fake.closes).toEqual([]);
    expect(testRemoteHost!.contains("remote-shell-1")).toBe(true);
    // This Mac's own tab was ended as before.
    expect(terminalHost().contains("local-1")).toBe(false);
  });

  it("a hidden tile's reap leaves its remote session running; only a confirmed End ends it", async () => {
    await openBeside();
    act(() => s().requestCloseWorkspace(TEAM, WS));
    await settle();
    expect(s().softClosedWorkspaces[TEAM]?.[WS]).toBeDefined();
    act(() => s().killTerminalsForSoftClosedWorkspace(TEAM, WS));
    await settle();
    expect(fake.closes).toEqual([]);
    expect(testRemoteHost!.contains("remote-shell-1")).toBe(true);
  });

  it("ending the tile, once confirmed, ends its remote session too", async () => {
    await openBeside();
    let ended: Promise<boolean> | undefined;
    act(() => {
      ended = endWorkspace(TEAM, WS);
    });
    await settle();
    expect(fake.closes).toEqual([]);
    act(() => settleEndConfirm(true));
    await ended;
    await settle();
    expect(fake.closes).toEqual(["remote-shell-1:kill"]);
  });
});

describe("a retired private authority", () => {
  it("a snapshot read before the owner changed never shows the old owner's remote tabs", async () => {
    await openBeside();
    expect(tabsIn()).toEqual(["local-1", "remote-shell-1"]);
    // A read in flight under the old authority…
    const inFlight = [terminalHost().reply(), testRemoteHost!.reply()];
    // …lands after it retired.
    fake.authority = "8";
    act(() => {
      s().applyLayout({ kind: "private-retired" });
      s().applyLayout({ kind: "snapshot", replies: inFlight });
    });
    expect(tabsIn()).toEqual(["local-1"]);
  });
});

/** Three local tabs and three remote ones, in one workspace. */
async function threeAndThree() {
  seedTerminalTabs(TEAM, WS, ["local-2", "local-3"]);
  for (let i = 0; i < 3; i++) await openBeside();
  expect(tabsIn()).toEqual([
    "local-1",
    "local-2",
    "local-3",
    "remote-shell-1",
    "remote-shell-2",
    "remote-shell-3",
  ]);
}

describe("one grid for a workspace with local and remote tabs", () => {
  it("Grid all, preset, Collapse, drag out, restart and disconnect behave as with local tabs only", async () => {
    const view = renderApp();
    await threeAndThree();
    const all = tabsIn();

    // Grid all: every tab is a cell, in strip order.
    act(() => s().gridAllTerminalTabs(TEAM, WS));
    await settle();
    expect(panedIn()).toEqual(all);
    expect(cells()).toEqual(all);
    const pills = all.filter((id) => screen.queryByTestId(`terminal-tab-${id}`));
    expect(pills).toContain("remote-shell-3");

    // The preset dial: one shape for every cell, held by This Mac's host.
    const before = workspaceTabGridPreset(s(), TEAM, WS);
    act(() => s().cycleTabGridPreset(TEAM, WS));
    await settle();
    const preset = workspaceTabGridPreset(s(), TEAM, WS);
    expect(preset).not.toBe(before);
    const key = workspaceKey(TEAM, { id: WS });
    expect(terminalHost().snapshot().workspaces[key].grid.preset).toBe(preset);
    expect(panedIn()).toEqual(all);

    // Collapse back to tabs keeps the cells; showing side by side restores them.
    act(() => s().collapseTabGridToTabs(TEAM, WS));
    await settle();
    expect(shouldRenderTabGrid(s(), TEAM, WS)).toBe(false);
    expect(cells()).toEqual([]);
    act(() => showAllTabs(TEAM, WS));
    await settle();
    expect(selectTabGridCollapsed(s(), TEAM, WS)).toBe(false);
    expect(cells()).toEqual(all);

    // Drag one remote tab out of the grid: five cells left.
    act(() => s().demoteTerminalPane(TEAM, WS, "remote-shell-2"));
    await settle();
    const five = all.filter((id) => id !== "remote-shell-2");
    expect(panedIn()).toEqual(five);
    expect(cells()).toEqual(five);
    // A Machine's own grid is never arranged.
    expect(testRemoteHost!.snapshot().workspaces[key].grid.paned).toEqual([]);

    // A relaunch: the projection starts empty, the saved viewer grid and every
    // host's layout come back — the same grid.
    const saved = s().terminalLayout.grids!;
    view.unmount();
    useViewerStore.setState({ terminalLayout: EMPTY_TERMINAL_LAYOUT as TerminalLayoutProjection });
    forgetRemoteAttaches();
    act(() => {
      s().applyLayout({ kind: "viewer-grids", grids: structuredClone(saved) });
      refetchTestLayout();
    });
    renderApp();
    await settle();
    expect(panedIn()).toEqual(five);
    expect(workspaceTabGridPreset(s(), TEAM, WS)).toBe(preset);
    expect(cells()).toEqual(five);

    // Disconnect: the remote cells stay, each showing its Machine offline.
    act(() => disconnect());
    await settle();
    expect(panedIn()).toEqual(five);
    expect(cells()).toEqual(five);
    expect(screen.getAllByTestId("remote-terminal-offline")).toHaveLength(2);
  });

  it("a workspace whose only tab is remote grids it and cycles its shape like a local one", async () => {
    renderApp();
    await openBeside();
    act(() => {
      terminalHost().closeTab("local-1", "kill");
      refetchTestLayout();
    });
    await settle();
    expect(tabsIn()).toEqual(["remote-shell-1"]);
    act(() => s().gridAllTerminalTabs(TEAM, WS));
    await settle();
    expect(panedIn()).toEqual(["remote-shell-1"]);
    const before = workspaceTabGridPreset(s(), TEAM, WS);
    act(() => s().cycleTabGridPreset(TEAM, WS));
    await settle();
    expect(workspaceTabGridPreset(s(), TEAM, WS)).not.toBe(before);
    expect(panedIn()).toEqual(["remote-shell-1"]);
    act(() => s().collapseTabGridToTabs(TEAM, WS));
    await settle();
    expect(selectTabGridCollapsed(s(), TEAM, WS)).toBe(true);
  });

  it("a remote tab that ends on its Machine leaves the grid", async () => {
    renderApp();
    await threeAndThree();
    act(() => s().gridAllTerminalTabs(TEAM, WS));
    await settle();
    act(() => {
      testRemoteHost!.closeTab("remote-shell-3", "kill");
      refetchTestLayout();
    });
    await settle();
    expect(panedIn()).not.toContain("remote-shell-3");
    expect(s().terminalLayout.grids?.[workspaceKey(TEAM, { id: WS })]?.paned).toHaveLength(2);
  });
});

describe("local and remote cells share one resizable arrangement", () => {
  installPanelGeometry();

  const panelOf = (id: string) =>
    screen.getByTestId(`terminal-grid-cell-${id}`).closest<HTMLElement>("[data-panel]")!;
  /** The panel holding the row a cell sits in. */
  const rowPanelOf = (id: string) =>
    panelOf(id).parentElement!.closest<HTMLElement>("[data-panel]")!;
  const shareOf = (el: Element) =>
    Number(/flex-grow:\s*([\d.]+)/.exec(el.getAttribute("style") ?? "")![1]);

  it("lays out by the preset, resizes, and keeps its sizes across a relaunch and a disconnect", async () => {
    const view = renderApp();
    await threeAndThree();
    const all = tabsIn();
    act(() => {
      s().gridAllTerminalTabs(TEAM, WS);
      s().setTabGridPreset(TEAM, WS, "grid");
    });
    await settle();
    // Grid: two rows of three, in strip order — the local row over the remote one.
    expect(cells()).toEqual(all);
    const topRow = panelOf("local-1").parentElement!;
    for (const id of ["local-2", "local-3"]) expect(panelOf(id).parentElement).toBe(topRow);
    const bottomRow = panelOf("remote-shell-1").parentElement!;
    expect(bottomRow).not.toBe(topRow);
    for (const id of ["remote-shell-2", "remote-shell-3"]) {
      expect(panelOf(id).parentElement).toBe(bottomRow);
    }

    // Drag the divider between a local and a remote cell's rows, then between two
    // remote cells.
    const rowDivider = rowPanelOf("local-1").nextElementSibling as HTMLElement;
    act(() => dragSeparator(rowDivider, 0, 0.1 * VIEWPORT.height));
    await settle();
    act(() => dragSeparator(panelOf("remote-shell-1").nextElementSibling as HTMLElement, 120, 0));
    await settle();
    expect(shareOf(rowPanelOf("local-1"))).toBeCloseTo(60);
    expect(shareOf(panelOf("remote-shell-1"))).toBeCloseTo(100 / 3 + 10);

    // A relaunch: the saved grid and sizes come back with every host's layout.
    const savedGrids = s().terminalLayout.grids!;
    const savedSizes = s().tabGridLayout;
    view.unmount();
    useViewerStore.setState({
      terminalLayout: EMPTY_TERMINAL_LAYOUT as TerminalLayoutProjection,
      tabGridLayout: structuredClone(savedSizes),
    });
    forgetRemoteAttaches();
    act(() => {
      s().applyLayout({ kind: "viewer-grids", grids: structuredClone(savedGrids) });
      refetchTestLayout();
    });
    renderApp();
    await settle();
    expect(cells()).toEqual(all);
    expect(shareOf(rowPanelOf("local-1"))).toBeCloseTo(60);
    expect(shareOf(panelOf("remote-shell-1"))).toBeCloseTo(100 / 3 + 10);

    // Disconnect: the remote cells stay where they were, at their sizes.
    act(() => disconnect());
    await settle();
    expect(cells()).toEqual(all);
    expect(shareOf(panelOf("remote-shell-1"))).toBeCloseTo(100 / 3 + 10);

    // A preset lays the mixed cells out evenly again.
    act(() => s().setTabGridPreset(TEAM, WS, "grid"));
    await settle();
    expect(shareOf(panelOf("remote-shell-1"))).toBeCloseTo(100 / 3);
  });
});

describe("a focused remote cell is the focused terminal for shortcuts", () => {
  /** An expanded workspace side by side — two local cells and one remote —
   *  with the keyboard in the remote cell. */
  async function focusedRemoteCell() {
    seedTerminalTabs(TEAM, WS, ["local-2"]);
    renderApp();
    await openBeside();
    act(() => s().gridAllTerminalTabs(TEAM, WS));
    await settle();
    expect(cells()).toEqual(["local-1", "local-2", "remote-shell-1"]);
    act(() => screen.getByLabelText("remote terminal remote-shell-1").focus());
    await settle();
    expect(selectActiveTerminalTab(s(), TEAM, WS)).toBe("remote-shell-1");
  }

  it("find opens the remote cell's find bar", async () => {
    await focusedRemoteCell();
    runAction("viewer.find-in-terminal");
    await settle();
    const bar = screen.getByRole("search", { name: "Terminal search" });
    expect(bar.closest('[data-terminal-session-id="remote-shell-1"]')).not.toBeNull();
    expect(flashBlockedNavForWorkspace).not.toHaveBeenCalled();
  });

  it("clear clears the remote cell's terminal", async () => {
    await focusedRemoteCell();
    const clear = vi.fn();
    const terminal = vi
      .spyOn(terminalManager, "terminal")
      .mockReturnValue({ buffer: { active: { type: "normal" } }, clear } as never);
    runAction("viewer.clear-terminal");
    terminal.mockRestore();
    expect(clear).toHaveBeenCalledTimes(1);
    expect(flashBlockedNavForWorkspace).not.toHaveBeenCalled();
  });

  it("jump to a tab and the layout cycle work from it", async () => {
    await focusedRemoteCell();
    const before = workspaceTabGridPreset(s(), TEAM, WS);
    runAction("viewer.cycle-foreground-layout");
    await settle();
    expect(workspaceTabGridPreset(s(), TEAM, WS)).not.toBe(before);
    expect(cells()).toEqual(["local-1", "local-2", "remote-shell-1"]);
    runAction("viewer.jump-terminal-tab-1");
    await settle();
    expect(selectActiveTerminalTab(s(), TEAM, WS)).toBe("local-1");
    expect(flashBlockedNavForWorkspace).not.toHaveBeenCalled();
  });

  it("close (⌘W) hides the focused remote tab without asking", async () => {
    await focusedRemoteCell();
    await act(async () => {
      await closeActiveTerminalTab(TEAM);
    });
    await settle();
    expect(useEndConfirmStore.getState().request).toBeNull();
    expect(selectHiddenTerminalTabs(s(), TEAM, WS).map((t) => t.id)).toEqual(["remote-shell-1"]);
    expect(testRemoteHost!.contains("remote-shell-1")).toBe(true);
  });

  it("rename from its tab pill", async () => {
    await focusedRemoteCell();
    act(() => screen.getByTestId("terminal-tab-remote-shell-1").focus());
    runAction("viewer.rename-terminal-tab");
    await settle();
    expect(screen.getByLabelText("Tab name")).toBeTruthy();
  });
});

describe("a new tab opened while the tabs are side by side joins them", () => {
  /** An expanded workspace showing two local cells. */
  async function twoLocalCells() {
    seedTerminalTabs(TEAM, WS, ["local-2"]);
    renderApp();
    act(() => s().gridAllTerminalTabs(TEAM, WS));
    await settle();
    expect(cells()).toEqual(["local-1", "local-2"]);
  }

  it("a remote tab opened from the Where chooser becomes a third cell, focused", async () => {
    await twoLocalCells();
    act(() => requestNewWorkspaceTerminal(TEAM, WS));
    fireEvent.click(await screen.findByTestId("new-terminal-where-box"));
    await settle();
    await settle();
    expect(cells()).toEqual(["local-1", "local-2", "remote-shell-1"]);
    expect(selectActiveTerminalTab(s(), TEAM, WS)).toBe("remote-shell-1");
    expect(toast).not.toHaveBeenCalled();
  });

  it("a local tab opened with ⌘T becomes a third cell, focused", async () => {
    await twoLocalCells();
    runAction("viewer.new-terminal-tab");
    await settle();
    expect(terminalHost().opened).toHaveLength(1);
    const added = terminalHost().opened[0].session;
    expect(cells()).toEqual(["local-1", "local-2", added]);
    expect(selectActiveTerminalTab(s(), TEAM, WS)).toBe(added);
  });

  it("with no room for another readable cell it shows the tabs, says so, and shows the new one", async () => {
    await twoLocalCells();
    const pane = screen.getByTestId(`workspace-panels-${WS}`);
    vi.spyOn(pane, "getBoundingClientRect").mockReturnValue({
      width: 300,
      height: 120,
      top: 0,
      left: 0,
      right: 300,
      bottom: 120,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    } as DOMRect);
    act(() => requestNewWorkspaceTerminal(TEAM, WS));
    fireEvent.click(await screen.findByTestId("new-terminal-where-box"));
    const revision = currentContextNavigationRevision();
    await settle();
    await settle();
    expect(currentContextNavigationRevision()).toBe(revision);
    expect(shouldRenderTabGrid(s(), TEAM, WS)).toBe(false);
    expect(selectActiveTerminalTab(s(), TEAM, WS)).toBe("remote-shell-1");
    expect(toast).toHaveBeenCalledWith(NEW_TAB_NO_ROOM);
  });
});

describe("an agent on a machine is one agent", () => {
  const location = {
    kind: "machine" as const,
    machineId: "box",
    requestId: "r",
    handle: "remote-workspace-h",
    cwd: "/home/me/codesense/worktrees/default/SW-9",
  };
  const claude = {
    teamKey: TEAM,
    workspaceId: WS,
    location,
    agentKey: "builtin/claude",
  };

  it("a second open focuses the agent already running there, never a second one", async () => {
    renderApp();
    let first = "";
    await act(async () => {
      first = (await remoteStoryWorkspaceDriver.launchAgent(claude)).sessionId;
    });
    await settle();
    expect(fake.opened).toBe(1);
    act(() => s().setActiveTerminalTab(TEAM, "local-1", WS));
    let second = "";
    await act(async () => {
      second = (await remoteStoryWorkspaceDriver.launchAgent(claude)).sessionId;
    });
    expect(second).toBe(first);
    expect(fake.opened).toBe(1);
    expect(selectActiveTerminalTab(s(), TEAM, WS)).toBe(first);
    // An explicit new session is its own agent.
    await act(async () => {
      await remoteStoryWorkspaceDriver.launchAgent({ ...claude, sessionMode: "new" });
    });
    expect(fake.opened).toBe(2);
  });

  it("a background open starts it without taking the focus", async () => {
    renderApp();
    let started = "";
    await act(async () => {
      started = (await remoteStoryWorkspaceDriver.launchAgent({ ...claude, headed: false }))
        .sessionId;
    });
    await settle();
    expect(fake.opened).toBe(1);
    expect(tabsIn()).toContain(started);
    expect(selectActiveTerminalTab(s(), TEAM, WS)).toBe("local-1");
  });
});

/** Every ancestor of `el` up to the document. */
function ancestors(el: Element): HTMLElement[] {
  const out: HTMLElement[] = [];
  for (let n = el.parentElement; n; n = n.parentElement) out.push(n);
  return out;
}

function liveInboxAgent(session: string, provider: "claude" | "codex") {
  useAgentStateStore.getState().reset(session);
  useAgentStateStore.getState().recordActivity(
    {
      session,
      identity: {
        hostBootId: "remote-inbox-host",
        sessionIncarnation: session,
        invocationId: crypto.randomUUID(),
      },
      revision: 1,
      turnGeneration: 0,
      provider,
      state: "idle",
      observed: true,
      limited: false,
      exit: null,
      asks: [],
    },
    Infinity,
  );
}

describe("a tab's agent inbox lives in that tab", () => {
  it("two cells with their inboxes open show two panes, each inside its own cell", async () => {
    renderApp();
    await openBeside();
    act(() => showAllTabs(TEAM, WS));
    await settle();
    act(() => {
      liveInboxAgent("local-1", "claude");
      liveInboxAgent("remote-shell-1", "codex");
      openAgentInboxRail("local-1");
      openAgentInboxRail("remote-shell-1");
    });
    const panes = await screen.findAllByTestId("inbox-rail");
    expect(panes).toHaveLength(2);
    const local = screen.getByTestId("terminal-grid-cell-local-1");
    const remote = screen.getByTestId("terminal-grid-cell-remote-shell-1");
    const inLocal = within(local).getByTestId("inbox-rail");
    const inRemote = within(remote).getByTestId("inbox-rail");
    expect(inLocal.getAttribute("data-session-id")).toBe("local-1");
    expect(inRemote.getAttribute("data-session-id")).toBe("remote-shell-1");
    // Each pane sits under its own cell's caption, never over it.
    const caption = screen.getByTestId("terminal-grid-caption-remote-shell-1");
    expect(
      caption.compareDocumentPosition(inRemote) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    expect(caption.contains(inRemote)).toBe(false);
    // Each cell's terminal (not the whole grid) makes room for its own pane: the pane docks
    // beside that cell's terminal, in that cell's own split.
    const region = within(remote).getByTestId("inbox-split-remote-shell-1");
    expect(region.contains(inRemote)).toBe(true);
    expect(region.querySelector(".ibx-split-term")).not.toBeNull();

    // Closing one leaves the other where it is.
    act(() => closeAgentInboxRail("local-1"));
    expect(within(local).queryByTestId("inbox-rail")).toBeNull();
    expect(within(remote).getByTestId("inbox-rail")).toBeTruthy();
    act(() => closeAgentInboxRail("remote-shell-1"));
  });

  it("one tab at a time, the pane opens inside the shown tab's body, under its caption", async () => {
    renderApp();
    await openBeside();
    act(() => {
      liveInboxAgent("remote-shell-1", "codex");
      openAgentInboxRail("remote-shell-1");
    });
    const pane = await screen.findByTestId("inbox-rail");
    const body = pane.closest<HTMLElement>("[data-pane-active]")!;
    expect(body).not.toBeNull();
    expect(within(body).getByTestId("terminal-grid-caption-remote-shell-1")).toBeTruthy();
    act(() => closeAgentInboxRail("remote-shell-1"));
  });
});

describe("opening a remote tab never scrolls the tile's tab bar away", () => {
  it("the shown tab's body is a clipped flex column, so caption + terminal never overflow the tile", async () => {
    renderApp();
    await openBeside();
    const tabBar = screen.getByTestId("terminal-tab-remote-shell-1");
    const frame = document.querySelector<HTMLElement>(`[data-pane-id="${WS}"]`)!;
    expect(frame.contains(tabBar)).toBe(true);
    // The frame is not a scroll container: a focused terminal (the new tab's
    // autofocus) cannot be "scrolled into view" by sliding the tile up.
    expect(frame.className).toMatch(/\boverflow-clip\b/);
    expect(frame.className).not.toMatch(/\boverflow-(hidden|auto|scroll)\b/);

    // The shown tab: caption above, a terminal region that takes only the rest.
    const body = remotePane("remote-shell-1")!.closest<HTMLElement>("[data-pane-active]")!;
    expect(body.className).toMatch(/\bflex\b/);
    expect(body.className).toMatch(/\bflex-col\b/);
    expect(body.className).toMatch(/\bmin-h-0\b/);
    expect(body.className).not.toMatch(/\bh-full\b/);
    const region = within(body).getByTestId("inbox-split-remote-shell-1");
    expect(region.className).toMatch(/\bflex-1\b/);
    expect(region.className).toMatch(/\bmin-h-0\b/);
    expect(region.className).not.toMatch(/\bh-full\b/);

    // Nothing between the tab bar and the document ended up scrolled.
    for (const el of [tabBar, ...ancestors(tabBar)]) expect(el.scrollTop).toBe(0);
  });

  it("switching back and forth between the local and remote tab keeps the same contract", async () => {
    renderApp();
    await openBeside();
    act(() => s().setActiveTerminalTab(TEAM, "local-1", WS));
    await settle();
    const local = document.querySelector<HTMLElement>("[data-pane-active]")!;
    expect(local.className).toMatch(/\bflex-col\b/);
    act(() => s().setActiveTerminalTab(TEAM, "remote-shell-1", WS));
    await settle();
    const remote = remotePane("remote-shell-1")!.closest<HTMLElement>("[data-pane-active]")!;
    expect(remote.className).toMatch(/\bflex-col\b/);
    expect(screen.getByTestId("terminal-tab-remote-shell-1")).toBeTruthy();
  });
});
