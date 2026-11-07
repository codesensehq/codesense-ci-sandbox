// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, act, cleanup, fireEvent, screen, waitFor, within } from "@/test-utils";
import { DndContext } from "@dnd-kit/core";

// Seam: every item a tab's context menu shows does something real. The real
// tile (WorkspaceTerminalPanels, its chrome and tab menu), the real viewer
// store with its layout verbs answered by the test terminal host, the real close
// and restart verbs; only the xterm leaf and the Tauri commands are stubbed.
// For each kind of tab the menu's items are read off the rendered menu, and
// each one is then clicked on a fresh tile and its effect checked — an item
// with no known effect fails the test, so no item can be shown dead.

const probe = vi.hoisted(() => ({ running: new Map<string, string>() }));

vi.mock("./shell-terminal-panel", () => ({
  ShellTerminalPanel: ({ sessionId }: { sessionId: string }) => (
    <textarea data-sid={sessionId} aria-label={`terminal ${sessionId}`} />
  ),
}));

vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
  openEditor: vi.fn().mockResolvedValue(undefined),
  wvRosterForSession: vi.fn().mockResolvedValue(null),
  dashboardKillByHandle: vi.fn().mockResolvedValue(undefined),
  detectTools: vi.fn().mockResolvedValue({ editors: [{ id: "code", label: "VS Code" }] }),
  sessionsRunningProcesses: vi.fn(async (ids: string[]) =>
    ids.map((id) => ({ foreground: probe.running.get(id) ?? null, background: [] })),
  ),
}));
vi.mock("./source-editor-inset", () => ({ WorktreeEditorSurface: () => null }));
vi.mock("@/shared/lib/default-agent", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/shared/lib/default-agent")>()),
  resolveAgentCliPath: () => Promise.resolve(null),
}));
vi.mock("@/features/dashboard/components/markdown-viewer", () => ({ MarkdownViewer: () => null }));
vi.mock("@/shared/lib/pipeline-graph/use-story-details", () => ({
  useStoryDetails: vi.fn(() => ({ data: { title: null, storyText: "" }, loading: false })),
}));
vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn(), info: vi.fn() }),
}));

import { openEditor, dashboardKillByHandle, wvRosterForSession } from "@/tauri/commands";
import {
  selectHiddenTerminalTabs,
  selectWorkspaceTabs,
  useViewerStore,
  workspacePanedTabIds,
  type TerminalTab,
  type Workspace,
} from "@/shared/stores/viewer-store";
import {
  installTerminalLayoutHarness,
  seedTerminalTabs,
  terminalHost,
} from "@/test/terminal-layout-harness";
import { useOrgStore } from "@/shared/stores/org-store";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { PositionedContextMenu } from "@/shared/components/positioned-context-menu";
import { useEndConfirmStore, hideTab, settleEndConfirm } from "../lib/terminal-close";
import { resetDetectedEditorsCache } from "@/shared/hooks/use-detected-editors";
import { WorkspaceTerminalPanels } from "./workspace-terminal-panels";
import { HiddenTabMenuItems } from "./hidden-tab-menu-items";

const TEAM = "acme:default";
const WS = "ws-a";
const DOC = "/ctx/stories/SW-1.md";

installTerminalLayoutHarness();

const s = () => useViewerStore.getState();
const tabsIn = () => selectWorkspaceTabs(s(), TEAM, WS).map((t) => t.id);
const hiddenIn = () => selectHiddenTerminalTabs(s(), TEAM, WS).map((t) => t.id);
const panedIn = () => workspacePanedTabIds(s(), TEAM, WS);
const settle = () => act(async () => new Promise((r) => setTimeout(r, 30)));
const onOpenStoryDoc = vi.fn();

interface Kind {
  workspace: Partial<Workspace>;
  tabs: { session: string; name: string; role?: string }[];
  paned?: string[];
  /** The editor chosen in Settings (default: VS Code). */
  editor?: string;
  /** Put the tile into the state this kind is about. */
  arrange?: () => void;
  /** The items its menu shows, in order. */
  menu: string[];
}

const PLAIN = [
  { session: "t1", name: "shell" },
  { session: "t2", name: "other" },
];
const CLOSE = ["Hide", "End", "End Others", "End All"];
const TOP = ["Add to all tabs", "Rename", "New terminal", "New terminal on…", "Split"];
/** Every kind here has its directory and VS Code configured. */
const EDITOR = "Open in VS Code";
const EPIC_DOC = "/ctx/epics/E1/README.md";

const KINDS: Record<string, Kind> = {
  shell: { workspace: { kind: "root" }, tabs: PLAIN, menu: [...TOP, EDITOR, ...CLOSE] },
  epic: {
    workspace: { kind: "epic", epicDocPath: EPIC_DOC },
    tabs: PLAIN,
    menu: [...TOP, "Open Epic Doc", EDITOR, ...CLOSE],
  },
  story: {
    workspace: { kind: "story", storyFilePath: DOC, storyId: "SW-1" },
    tabs: PLAIN,
    menu: [...TOP, "Open Story Doc", EDITOR, ...CLOSE],
  },
  command: {
    workspace: { kind: "root" },
    tabs: PLAIN,
    arrange: () => probe.running.set("t1", "npm test"),
    menu: [...TOP, EDITOR, ...CLOSE],
  },
  ended: {
    workspace: { kind: "root" },
    tabs: PLAIN,
    arrange: () => act(() => terminalHost().exit("t1", 0)),
    menu: [...TOP, EDITOR, "Restart", ...CLOSE],
  },
  // An agent CodeSense launched: its Restart keeps the launch's shell rules.
  "ended agent": {
    workspace: { kind: "root" },
    tabs: [{ session: "t1", name: "claude", role: "agent" }, PLAIN[1]],
    arrange: () => {
      useAgentStateStore.getState().recordLaunch("t1", {
        line: "claude",
        agent: "claude",
        spec: {
          agentPath: "/usr/local/bin/claude",
          shell: { cwd: "/agent-cwd", envRemove: ["ANTHROPIC_API_KEY"] },
        },
      });
      act(() => terminalHost().exit("t1", null, "SIGKILL"));
    },
    menu: [...TOP, EDITOR, "Restart", ...CLOSE],
  },
  // An agent found running in the tab: its Role and Face, as on its agents-list row.
  agent: {
    workspace: { kind: "root" },
    tabs: PLAIN,
    arrange: () =>
      useAgentListProjectionStore.setState({
        sessions: [
          {
            handle: "dev",
            rowNames: { primary: "shell", secondary: null, full: "shell" },
            sessionId: "t1",
            kind: "dashboard-launched",
            alive: true,
            backgrounded: false,
            startedWith: null,
            startedAtMs: null,
            launcher: null,
            lastOutputMs: null,
          },
        ],
      }),
    menu: [
      ...TOP,
      "Role",
      "Face",
      "Remind this agent…",
      EDITOR,
      "Hide",
      "Stop…",
      "End",
      "End Others",
      "End All",
    ],
  },
  "no editor set": {
    workspace: { kind: "story", storyFilePath: DOC, storyId: "SW-1" },
    tabs: PLAIN,
    editor: "",
    menu: [...TOP, "Open Story Doc", ...CLOSE],
  },
  "workspace not ready": {
    workspace: { kind: "root", status: "creating" },
    tabs: PLAIN,
    menu: [...TOP, ...CLOSE],
  },
  paned: {
    workspace: { kind: "root" },
    tabs: PLAIN,
    paned: ["t1", "t2"],
    menu: [
      "Remove from all tabs",
      "Rename",
      "New terminal",
      "New terminal on…",
      "Split",
      EDITOR,
      ...CLOSE,
    ],
  },
};

async function mount(kind: Kind) {
  useViewerStore.setState({
    foregroundLayout: {},
    activeWorkspace: { [TEAM]: WS },
    terminalVisible: true,
    aiStatusBySession: {},
    settings: { ...s().settings, editor: kind.editor ?? "code", editorCustomCmd: "" },
    workspaces: {
      [TEAM]: [
        { id: WS, cwd: "/w", status: "ready", kind: "root", ...kind.workspace } as Workspace,
      ],
    },
  });
  seedTerminalTabs(TEAM, WS, kind.tabs, { active: "t1", paned: kind.paned });
  kind.arrange?.();
  render(
    <DndContext>
      <div data-xterm-host data-repo-path="/repo">
        <WorkspaceTerminalPanels teamKey={TEAM} repoPath="/repo" onOpenStoryDoc={onOpenStoryDoc} />
      </div>
    </DndContext>,
  );
  await settle();
}

/** Right-click the tab and return its menu. */
async function openMenu(tabId: string): Promise<HTMLElement> {
  const tab = screen.getByTestId(`terminal-tab-${tabId}`);
  fireEvent.contextMenu(tab, { clientX: 10, clientY: 10 });
  await settle();
  return screen.getByRole("menu", { name: "Tab actions" });
}

/** Each item's name — its text without the drawn shortcut (hidden from AT). */
const itemNames = (menu: HTMLElement) =>
  within(menu)
    .getAllByRole("menuitem")
    .map((item) => {
      const copy = item.cloneNode(true) as HTMLElement;
      copy.querySelectorAll("[aria-hidden]").forEach((hint) => hint.remove());
      return copy.textContent ?? "";
    });

/** What each item must have done, checked after it was clicked on tab t1. */
const EFFECTS: Record<string, (kind: Kind) => Promise<void>> = {
  "Add to all tabs": async () => expect(panedIn()).toContain("t1"),
  "Remove from all tabs": async () => expect(panedIn()).not.toContain("t1"),
  Rename: async () => {
    expect(await screen.findByRole("textbox", { name: "Tab name" })).toBeInTheDocument();
  },
  // SW-AGR-6: the Remind form opens on the tab, its message ready to type.
  "Remind this agent…": async () => {
    const remind = await screen.findByTestId("remind-popover");
    expect(within(remind).getByLabelText("Message")).toBeInTheDocument();
  },
  // A quick tab on this Mac, no chooser.
  "New terminal": async (kind) => {
    await waitFor(() => expect(tabsIn()).toHaveLength(kind.tabs.length + 1));
    expect(screen.queryByRole("form", { name: "New terminal" })).toBeNull();
  },
  // Opens the workspace's Where chooser; opening it on this Mac adds the tab.
  "New terminal on…": async (kind) => {
    const chooser = await screen.findByRole("dialog", { name: "New terminal" });
    fireEvent.click(within(chooser).getByRole("button", { name: "This Mac" }));
    await waitFor(() => expect(tabsIn()).toHaveLength(kind.tabs.length + 1));
  },
  Split: async (kind) => waitFor(() => expect(tabsIn()).toHaveLength(kind.tabs.length + 1)),
  "Open Story Doc": async () => expect(onOpenStoryDoc).toHaveBeenCalledWith(DOC),
  "Open Epic Doc": async () => expect(onOpenStoryDoc).toHaveBeenCalledWith(EPIC_DOC),
  [EDITOR]: async () => expect(openEditor).toHaveBeenCalledWith("code", "", "/w"),
  // In the launch's own directory when CodeSense launched an agent there.
  Restart: async (kind) =>
    waitFor(() =>
      expect(terminalHost().respawnTab).toHaveBeenCalledWith(
        "local",
        "t1",
        expect.objectContaining({
          cwd: kind.tabs[0].role === "agent" ? "/agent-cwd" : "/w",
        }),
        undefined,
      ),
    ),
  // A submenu: open it and pick; the look is kept with the tab's team, by the agent.
  Role: async () => {
    fireEvent.pointerDown(screen.getByRole("menuitem", { name: "Role" }), {
      button: 0,
      pointerType: "mouse",
    });
    fireEvent.click(await screen.findByRole("menuitemradio", { name: "Reviewer" }));
    expect(s().agentIdentity?.[TEAM]?.dev?.role).toBe("reviewer");
    await waitFor(() => expect(screen.queryByRole("menu", { name: "Tab actions" })).toBeNull());
  },
  Face: async () => {
    fireEvent.pointerDown(screen.getByRole("menuitem", { name: "Face" }), {
      button: 0,
      pointerType: "mouse",
    });
    fireEvent.click(await screen.findByRole("menuitem", { name: "Funny originals" }));
    fireEvent.click(await screen.findByRole("menuitemradio", { name: "Raccoon in a hard hat" }));
    expect(s().agentIdentity?.[TEAM]?.dev?.face).toBe("builtin-funny:hardhat-raccoon");
  },
  "Stop…": async () => {
    expect(await screen.findByRole("alertdialog", { name: "Stop shell?" })).toBeInTheDocument();
    expect(dashboardKillByHandle).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Stop" }));
    await waitFor(() => expect(dashboardKillByHandle).toHaveBeenCalledWith("dev"));
  },
  Hide: async () => expect(hiddenIn()).toContain("t1"),
  // A running tab asks first, and stays until the answer; an idle one goes.
  End: async () => {
    if (probe.running.has("t1")) {
      await waitFor(() => expect(useEndConfirmStore.getState().request?.confirmLabel).toBe("End"));
      expect(tabsIn()).toContain("t1");
      act(() => settleEndConfirm(true));
    }
    await waitFor(() => expect(tabsIn()).not.toContain("t1"));
  },
  "End Others": async () => waitFor(() => expect(tabsIn().filter((id) => id !== "t1")).toEqual([])),
  // (Ending a tile's last tab leaves it a fresh terminal, so only the seeded ones count.)
  "End All": async (kind) =>
    waitFor(() =>
      probe.running.size > 0
        ? expect(useEndConfirmStore.getState().request).not.toBeNull()
        : expect(tabsIn().filter((id) => kind.tabs.some((t) => t.session === id))).toEqual([]),
    ),
};

beforeEach(() => {
  vi.clearAllMocks();
  probe.running = new Map();
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
  useAgentStateStore.setState({
    bySession: {},
    exitBySession: {},
    stateSince: {},
    seenAt: {},
    relaunchBySession: {},
    processAgentBySession: {},
  });
  useAgentListProjectionStore.setState({ sessions: null });
  useViewerStore.setState({ agentIdentity: {} });
  useEndConfirmStore.setState({ request: null, resolve: null });
  resetDetectedEditorsCache();
  vi.spyOn(terminalHost(), "respawnTab");
});

afterEach(async () => {
  await settle();
  cleanup();
  vi.restoreAllMocks();
});

describe("every item a tab's menu shows does something (AC-5)", () => {
  for (const [name, kind] of Object.entries(KINDS)) {
    it(`a ${name} tab: its menu is ${kind.menu.join(" · ")}`, async () => {
      await mount(kind);
      expect(itemNames(await openMenu("t1"))).toEqual(kind.menu);
    });

    for (const item of kind.menu) {
      it(`a ${name} tab: “${item}” does what it says`, async () => {
        await mount(kind);
        const menu = await openMenu("t1");
        fireEvent.click(within(menu).getByRole("menuitem", { name: item }));
        await settle();
        const effect = EFFECTS[item];
        expect(effect, `no known effect for “${item}”`).toBeDefined();
        await effect(kind);
      });
    }
  }
});

describe("a Hidden tab's items in its tray chip's menu", () => {
  function renderHidden(tab: TerminalTab) {
    render(
      <PositionedContextMenu position={{ x: 0, y: 0 }} onClose={() => {}} label="Hidden tabs">
        <HiddenTabMenuItems teamKey={TEAM} workspaceId={WS} tab={tab} onDone={() => {}} />
      </PositionedContextMenu>,
    );
  }

  async function hidden(ended: boolean): Promise<TerminalTab> {
    await mount(KINDS.shell);
    act(() => hideTab(TEAM, WS, "t1"));
    await settle();
    if (ended) act(() => terminalHost().exit("t1", 0));
    cleanup();
    return selectHiddenTerminalTabs(s(), TEAM, WS).find((t) => t.id === "t1")!;
  }

  const HIDDEN: Record<string, { ended: boolean; menu: string[] }> = {
    running: { ended: false, menu: ["Restore “shell”", "End “shell”"] },
    ended: {
      ended: true,
      menu: ["Restore “shell”", "Restart “shell”", "Dismiss “shell”"],
    },
  };
  const HIDDEN_EFFECTS: Record<string, () => Promise<void>> = {
    "Restore “shell”": async () => expect(tabsIn()).toContain("t1"),
    "End “shell”": async () => waitFor(() => expect(hiddenIn()).not.toContain("t1")),
    "Restart “shell”": async () =>
      waitFor(() => expect(terminalHost().respawnTab).toHaveBeenCalled()),
    "Dismiss “shell”": async () => waitFor(() => expect(hiddenIn()).not.toContain("t1")),
  };

  for (const [name, { ended, menu }] of Object.entries(HIDDEN)) {
    it(`a ${name} Hidden tab: ${menu.join(" · ")}, each doing what it says`, async () => {
      for (const item of menu) {
        const tab = await hidden(ended);
        renderHidden(tab);
        const names = screen.getAllByRole("menuitem").map((i) => i.textContent);
        expect(names).toEqual(menu);
        fireEvent.click(screen.getByRole("menuitem", { name: item }));
        await settle();
        const effect = HIDDEN_EFFECTS[item];
        expect(effect, `no known effect for “${item}”`).toBeDefined();
        await effect();
        cleanup();
      }
    });
  }
});

it.each(["offline", "ended"])("disables the agent tab Stop action while %s", async (state) => {
  await mount(KINDS.agent);
  if (state === "offline") {
    act(() =>
      useViewerStore.setState((current) => ({
        terminalLayout: {
          ...current.terminalLayout,
          byKey: Object.fromEntries(
            Object.entries(current.terminalLayout.byKey).map(([key, ws]) => [
              key,
              {
                ...ws,
                tabs: ws.tabs.map((tab) =>
                  tab.id === "t1"
                    ? {
                        ...tab,
                        origin: {
                          machineId: "box",
                          member: "box/remote",
                          handle: "t1",
                          offlineSince: Date.now(),
                        },
                      }
                    : tab,
                ),
              },
            ]),
          ),
        },
      })),
    );
  } else {
    act(() => {
      useAgentListProjectionStore.setState((current) => ({
        sessions: current.sessions!.map((row) => ({ ...row, alive: false })),
      }));
      terminalHost().exit("t1", 0);
    });
  }
  const menu = await openMenu("t1");
  const stop = within(menu).getByRole("menuitem", { name: "Stop…" });
  expect(stop).toBeDisabled();
  fireEvent.click(stop);
  expect(wvRosterForSession).not.toHaveBeenCalled();
  expect(screen.queryByRole("alertdialog")).toBeNull();
  expect(dashboardKillByHandle).not.toHaveBeenCalled();
});

it("the tab Stop uses the shared undeclared-worker confirmation and selected launch handle", async () => {
  await mount(KINDS.agent);
  vi.mocked(wvRosterForSession).mockResolvedValueOnce({ status: "dispatch" } as Awaited<
    ReturnType<typeof wvRosterForSession>
  >);
  const menu = await openMenu("t1");
  fireEvent.click(within(menu).getByRole("menuitem", { name: "Stop…" }));
  expect(
    await screen.findByRole("alertdialog", { name: "Worker has not declared done" }),
  ).toBeInTheDocument();
  expect(wvRosterForSession).toHaveBeenCalledWith("t1");
  expect(dashboardKillByHandle).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  expect(dashboardKillByHandle).not.toHaveBeenCalled();
});

it("the tab Stop ends only the chosen hand-started agent tab", async () => {
  await mount(KINDS.shell);
  act(() => {
    useAgentListProjectionStore.setState({
      sessions: [
        {
          handle: "t1",
          sessionId: "t1",
          kind: "hand-started",
          alive: true,
          backgrounded: false,
          startedWith: null,
          startedAtMs: null,
          launcher: null,
          lastOutputMs: null,
          rowNames: { primary: "shell", secondary: null, full: "shell" },
          detected: { cli: "claude", dot: null, processOnly: true },
        },
      ],
    });
    useAgentStateStore.setState({ processAgentBySession: { t1: "claude" } });
  });
  const menu = await openMenu("t1");
  fireEvent.click(within(menu).getByRole("menuitem", { name: "Stop…" }));
  const dialog = await screen.findByRole("alertdialog");
  expect(dialog).toHaveTextContent(
    "This ends the agent and its shell and closes this terminal tab.",
  );
  expect(tabsIn()).toEqual(["t1", "t2"]);
  fireEvent.click(within(dialog).getByRole("button", { name: "Stop" }));
  await waitFor(() => expect(tabsIn()).toEqual(["t2"]));
  expect(dashboardKillByHandle).not.toHaveBeenCalled();
});
