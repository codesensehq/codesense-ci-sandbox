// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, act, cleanup, fireEvent, screen } from "@/test-utils";
import { DndContext } from "@dnd-kit/core";

// Seam: ⌘⇧M pressed in a terminal goes through the real capture-phase intercept
// and the real shortcut registry into the real viewer store, rendered by the real
// tile frames and chrome (WorkspaceTerminalPanels), answered by the test terminal
// host. Only the xterm leaf and the Tauri commands are stubbed.

vi.mock("./shell-terminal-panel", () => ({
  ShellTerminalPanel: ({ sessionId, visible }: { sessionId: string; visible: boolean }) => (
    <div className="xterm" style={{ display: visible ? "block" : "none" }}>
      <textarea data-sid={sessionId} aria-label={`terminal ${sessionId}`} />
    </div>
  ),
}));
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
  sessionsRunningProcesses: vi.fn(async (ids: string[]) =>
    ids.map(() => ({ foreground: null, background: [] })),
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
vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn() }) }));

import {
  selectActiveTerminalTab,
  selectExpandedWorkspaceId,
  expandedWorkspaceId,
  activeForegroundTiles,
  shouldRenderTabGrid,
  useViewerStore,
  type Workspace,
} from "@/shared/stores/viewer-store";
import { installTerminalLayoutHarness, seedTerminalTabs } from "@/test/terminal-layout-harness";
import { useOrgStore } from "@/shared/stores/org-store";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { SHORTCUT_ACTIONS } from "@/shared/lib/shortcuts/registry";
import { useShortcutHandler } from "@/shared/hooks/use-shortcut-handler";
import { useWorkspaceCycleIntercept } from "../hooks/use-workspace-cycle-intercept";
import { WorkspaceTerminalPanels } from "./workspace-terminal-panels";
import {
  EXPAND_STEP_LABELS,
  resetExpandStepForTests,
  resetSplitChainsForTests,
  resetZoomedCellsForTests,
} from "../lib/terminal-tab-keys";
import { toast } from "sonner";

const TEAM = "acme:default";
const REPO = "/repo";
const WS = "ws-a";
const OTHER = "ws-b";

installTerminalLayoutHarness();

const story = (id: string): Workspace => ({ id, kind: "story", cwd: `/${id}`, status: "ready" });

/** WS runs `tabs` (the first active) beside OTHER, unless `alone`. */
function seed(tabs: string[], opts: { alone?: boolean } = {}) {
  useViewerStore.setState({
    foregroundLayout: {},
    foregroundLayoutByRail: {},
    customGroupsByTeam: {},
    railKeyByWorkspaceId: {},
    selectedEpicDirName: {},
    trayMode: {},
    activeWorkspace: { [TEAM]: WS },
    cliExpanded: false,
    terminalVisible: true,
    aiStatusBySession: {},
    softClosedWorkspaces: { [TEAM]: {} },
    workspaces: { [TEAM]: opts.alone ? [story(WS)] : [story(WS), story(OTHER)] },
  });
  seedTerminalTabs(
    TEAM,
    WS,
    tabs.map((session) => ({ session, name: session })),
    { active: tabs[0] },
  );
  if (!opts.alone) seedTerminalTabs(TEAM, OTHER, [{ session: "o1", name: "o1" }]);
}

const s = () => useViewerStore.getState();

type View = "workspaces" | "expanded" | "allTabs";
function view(): View {
  if (shouldRenderTabGrid(s(), TEAM, WS)) return "allTabs";
  return selectExpandedWorkspaceId(s(), TEAM) === WS ? "expanded" : "workspaces";
}

/** The tile chrome's ⛶: the expanded tile goes back to the grid, another is expanded. */
function toggleZoom(teamKey: string, workspaceId: string): void {
  if (selectExpandedWorkspaceId(s(), teamKey) === workspaceId) s().setCliExpanded(false);
  else s().expandWorkspace(teamKey, workspaceId);
}
const lastToast = () => vi.mocked(toast).mock.calls.at(-1)?.[0];

function TerminalArea() {
  useWorkspaceCycleIntercept(REPO);
  useShortcutHandler();
  return (
    <DndContext>
      <div data-xterm-host data-repo-path={REPO}>
        <WorkspaceTerminalPanels teamKey={TEAM} repoPath={REPO} />
      </div>
    </DndContext>
  );
}

const settle = () => act(async () => new Promise((r) => setTimeout(r, 30)));

/** ⌘⇧M in WS's active terminal. */
async function pressExpand() {
  const active = selectActiveTerminalTab(s(), TEAM, WS);
  const el = document.querySelector<HTMLElement>(`[data-sid="${active}"]`)!;
  el.focus();
  fireEvent.keyDown(el, { key: "M", code: "KeyM", metaKey: true, shiftKey: true });
  await settle();
}

const originalPlatform = navigator.platform;
beforeEach(() => {
  vi.clearAllMocks();
  resetSplitChainsForTests();
  resetZoomedCellsForTests();
  resetExpandStepForTests();
  Object.defineProperty(navigator, "platform", { value: "MacIntel", configurable: true });
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
  useAgentStateStore.setState({ bySession: {}, exitBySession: {}, stateSince: {}, seenAt: {} });
});

afterEach(() => {
  cleanup();
  Object.defineProperty(navigator, "platform", { value: originalPlatform, configurable: true });
});

describe("⌘⇧M goes in, in, out, out", () => {
  it("with three tabs, four presses pass through each state and end where they started", async () => {
    seed(["t1", "t2", "t3"]);
    render(<TerminalArea />);
    expect(view()).toBe("workspaces");

    const seen: [View, unknown][] = [];
    for (let i = 0; i < 4; i++) {
      await pressExpand();
      seen.push([view(), lastToast()]);
    }
    expect(seen).toEqual([
      ["expanded", EXPAND_STEP_LABELS.expanded],
      ["allTabs", EXPAND_STEP_LABELS.allTabs],
      ["expanded", EXPAND_STEP_LABELS.expanded],
      ["workspaces", EXPAND_STEP_LABELS.workspaces],
    ]);

    // And round again: the direction turns at each end, not on a stale flag.
    await pressExpand();
    expect(view()).toBe("expanded");
    await pressExpand();
    expect(view()).toBe("allTabs");
  });

  it("with one tab, it's a plain expand and restore", async () => {
    seed(["t1"]);
    render(<TerminalArea />);
    await pressExpand();
    expect([view(), lastToast()]).toEqual(["expanded", EXPAND_STEP_LABELS.expanded]);
    await pressExpand();
    expect([view(), lastToast()]).toEqual(["workspaces", EXPAND_STEP_LABELS.workspaces]);
    await pressExpand();
    expect(view()).toBe("expanded");
  });

  it("all tabs entered from the tile's toggle: two presses step back out to the workspace view", async () => {
    seed(["t1", "t2", "t3"]);
    render(<TerminalArea />);
    act(() => toggleZoom(TEAM, WS)); // the tile's ⛶
    await settle();
    fireEvent.click(screen.getByTestId(`tile-grid-toggle-${WS}`));
    await settle();
    expect(view()).toBe("allTabs");

    await pressExpand();
    expect(view()).toBe("expanded");
    await pressExpand();
    expect(view()).toBe("workspaces");
  });

  it("back to one tab from the toggle, the next press heads out", async () => {
    seed(["t1", "t2", "t3"]);
    render(<TerminalArea />);
    await pressExpand();
    await pressExpand();
    expect(view()).toBe("allTabs");
    fireEvent.click(screen.getByTestId(`tile-grid-toggle-${WS}`));
    await settle();
    expect(view()).toBe("expanded");
    await pressExpand();
    expect(view()).toBe("workspaces");
  });

  it("a fresh expand heads in, whatever the last trip out left behind", async () => {
    seed(["t1", "t2", "t3"]);
    render(<TerminalArea />);
    for (let i = 0; i < 3; i++) await pressExpand(); // in, in, out: heading out
    act(() => toggleZoom(TEAM, WS)); // restored by the ⛶
    act(() => toggleZoom(TEAM, WS)); // and expanded again
    await settle();
    await pressExpand();
    expect(view()).toBe("allTabs");
  });

  it("a tile alone in the area toggles one tab ⇄ all tabs", async () => {
    seed(["t1", "t2"], { alone: true });
    render(<TerminalArea />);
    await pressExpand();
    expect([shouldRenderTabGrid(s(), TEAM, WS), lastToast()]).toEqual([
      true,
      EXPAND_STEP_LABELS.allTabs,
    ]);
    await pressExpand();
    expect([shouldRenderTabGrid(s(), TEAM, WS), lastToast()]).toEqual([
      false,
      EXPAND_STEP_LABELS.expanded,
    ]);
    await pressExpand();
    expect(shouldRenderTabGrid(s(), TEAM, WS)).toBe(true);
  });
});

describe("⌘⌥G belongs to group chat", () => {
  it("has no tab-grid action, and the chord does not change the view", async () => {
    expect(SHORTCUT_ACTIONS.some((a) => a.id === "viewer.toggle-tab-grid")).toBe(false);
    expect(SHORTCUT_ACTIONS.filter((a) => a.defaultKeystroke === "CmdOrCtrl+Alt+G")).toEqual([
      expect.objectContaining({ id: "viewer.open-group-chat", label: "Open Group Chat" }),
    ]);
    seed(["t1", "t2", "t3"]);
    render(<TerminalArea />);
    const el = document.querySelector<HTMLElement>(`[data-sid="t1"]`)!;
    el.focus();
    fireEvent.keyDown(el, { key: "©", code: "KeyG", metaKey: true, altKey: true });
    await settle();
    expect(view()).toBe("workspaces");
  });
});

describe("expansion within a custom workspace group", () => {
  it("uses the selected group's lone foreground tile, then keeps flat mode unchanged", async () => {
    seed(["t1", "t2", "t3"]);
    act(() =>
      s().applyWorkspaceGroups({
        teamKey: TEAM,
        revision: 700,
        groups: [{ id: "one", name: "Release checks", workspaceIds: [WS] }],
      }),
    );
    useViewerStore.setState({
      trayMode: { [TEAM]: "grouped" },
      selectedEpicDirName: { [TEAM]: " group:one" },
    });
    render(<TerminalArea />);
    expect(activeForegroundTiles(s(), TEAM).map((w) => w.id)).toEqual([WS]);
    expect(expandedWorkspaceId(s(), TEAM)).toBe(WS);
    await pressExpand();
    expect(shouldRenderTabGrid(s(), TEAM, WS)).toBe(true);
    await pressExpand();
    expect(shouldRenderTabGrid(s(), TEAM, WS)).toBe(false);
    expect(activeForegroundTiles(s(), TEAM).map((w) => w.id)).toEqual([WS]);
    act(() => s().setTrayMode(TEAM, "flat"));
    expect(expandedWorkspaceId(s(), TEAM)).toBeNull();
    expect(activeForegroundTiles(s(), TEAM).map((w) => w.id)).toEqual([WS, OTHER]);
  });
  it("steps workspace, expanded and all-tabs views inside a two-member group", async () => {
    seed(["t1", "t2", "t3"]);
    const outside = story("outside");
    useViewerStore.setState({ workspaces: { [TEAM]: [...s().workspaces[TEAM], outside] } });
    seedTerminalTabs(TEAM, outside.id, ["outside-tab"]);
    act(() =>
      s().applyWorkspaceGroups({
        teamKey: TEAM,
        revision: 701,
        groups: [{ id: "two", name: "Checks", workspaceIds: [WS, OTHER] }],
      }),
    );
    useViewerStore.setState({
      trayMode: { [TEAM]: "grouped" },
      selectedEpicDirName: { [TEAM]: " group:two" },
    });
    render(<TerminalArea />);
    for (const expected of ["expanded", "allTabs", "expanded", "workspaces"]) {
      await pressExpand();
      expect(view()).toBe(expected);
      expect(activeForegroundTiles(s(), TEAM).map((w) => w.id)).toEqual([WS, OTHER]);
    }
    expect(s().workspaces[TEAM].some((w) => w.id === "outside")).toBe(true);
  });
});
