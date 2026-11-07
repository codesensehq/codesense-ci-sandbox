// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, act, cleanup, fireEvent, screen } from "@/test-utils";
import { DndContext } from "@dnd-kit/core";

// Seam: the real terminal area (WorkspaceTerminalPanels) and the real CLI bottom
// chrome, on the real panel library, over the real viewer store and its layout
// verbs answered by the test terminal host. Only the xterm leaf and the Tauri
// commands are stubbed; the test DOM is given flex geometry so a divider can be
// dragged as a pointer would.

vi.mock("./shell-terminal-panel", () => ({
  ShellTerminalPanel: ({ sessionId }: { sessionId: string }) => (
    <div className="xterm">
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
  selectWorkspaceKey,
  shouldRenderTabGrid,
  useViewerStore,
  workspaceTabGridPreset,
} from "@/shared/stores/viewer-store";
import { installTerminalLayoutHarness, seedTerminalTabs } from "@/test/terminal-layout-harness";
import { dragSeparator, installPanelGeometry, VIEWPORT } from "@/test/panel-geometry";
import { useOrgStore } from "@/shared/stores/org-store";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { SHORTCUT_ACTIONS } from "@/shared/lib/shortcuts/registry";
import { WorkspaceTerminalPanels } from "./workspace-terminal-panels";
import { CliBottomChrome } from "./cli-bottom-chrome";

const TEAM = "acme:default";
const WS = "ws-a";

installTerminalLayoutHarness();
installPanelGeometry();

const s = () => useViewerStore.getState();
const settle = () => act(async () => new Promise((r) => setTimeout(r, 30)));
const gridKey = () => selectWorkspaceKey(s(), TEAM, WS);

function seed(tabs: string[], opts: { preset?: "columns" | "rows" | "grid" } = {}) {
  useViewerStore.setState({
    foregroundLayout: {},
    openedTeams: [TEAM],
    activeWorkspace: { [TEAM]: WS },
    terminalVisible: true,
    layoutMode: "shared",
    aiStatusBySession: {},
    workspaces: { [TEAM]: [{ id: WS, kind: "generic", cwd: "/a", status: "ready" }] },
  });
  seedTerminalTabs(TEAM, WS, tabs, { active: tabs[0], paned: tabs, ...opts });
}

function renderApp() {
  return render(
    <DndContext>
      <div data-xterm-host data-repo-path="/repo">
        <WorkspaceTerminalPanels teamKey={TEAM} repoPath="/repo" />
      </div>
      <CliBottomChrome />
    </DndContext>,
  );
}

/** The panel holding `tabId`'s grid cell. */
const panelOf = (tabId: string) =>
  screen.getByTestId(`terminal-grid-cell-${tabId}`).closest<HTMLElement>("[data-panel]")!;
/** A panel's share of its group, as the library lays it out. */
const shareOf = (tabId: string) =>
  Number(/flex-grow:\s*([\d.]+)/.exec(panelOf(tabId).getAttribute("style") ?? "")![1]);
/** The divider right after `tabId`'s panel. */
const dividerAfter = (tabId: string) =>
  panelOf(tabId).nextElementSibling as HTMLElement & { dataset: { separator?: string } };
/** The saved sizes of the workspace's tab grid's root split. */
const savedRootSizes = () => {
  const root = s().tabGridLayout[gridKey()]?.root;
  return root?.kind === "split" ? root.sizes : undefined;
};

beforeEach(() => {
  vi.clearAllMocks();
  Object.defineProperty(navigator, "platform", { value: "MacIntel", configurable: true });
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
  useAgentStateStore.setState({ bySession: {}, exitBySession: {}, stateSince: {}, seenAt: {} });
});

afterEach(async () => {
  await settle();
  cleanup();
});

describe("Expanded · all tabs resizes like the workspace mosaic", () => {
  it("the cells sit on the mosaic's resizable dividers, evenly at first", async () => {
    seed(["t1", "t2", "t3"]);
    renderApp();
    await settle();
    expect(shouldRenderTabGrid(s(), TEAM, WS)).toBe(true);
    for (const id of ["t1", "t2", "t3"]) expect(shareOf(id)).toBeCloseTo(100 / 3);
    expect(dividerAfter("t1").hasAttribute("data-separator")).toBe(true);
    expect(dividerAfter("t2").hasAttribute("data-separator")).toBe(true);
  });

  it("dragging a divider resizes the pair, is saved, and survives a remount", async () => {
    seed(["t1", "t2"]);
    const view = renderApp();
    await settle();
    // 240px of a 1200px row is 20% of it.
    act(() => dragSeparator(dividerAfter("t1"), 0.2 * VIEWPORT.width, 0));
    await settle();
    expect(shareOf("t1")).toBeCloseTo(70);
    expect(shareOf("t2")).toBeCloseTo(30);
    expect(savedRootSizes()?.[0]).toBeCloseTo(70);
    expect(s().tabGridLayout[gridKey()]?.preset).toBe("columns");

    view.unmount();
    renderApp();
    await settle();
    expect(shareOf("t1")).toBeCloseTo(70);
    expect(shareOf("t2")).toBeCloseTo(30);
  });

  it("a drag keeps every cell above the readable floor", async () => {
    seed(["t1", "t2"]);
    renderApp();
    await settle();
    act(() => dragSeparator(dividerAfter("t1"), VIEWPORT.width, 0));
    await settle();
    // 220px of 1200px: the second cell stops at the floor.
    expect(shareOf("t2")).toBeGreaterThanOrEqual((220 / VIEWPORT.width) * 100 - 0.5);
  });

  it("double-clicking a divider evens that pair, and only that pair", async () => {
    seed(["t1", "t2", "t3"]);
    renderApp();
    await settle();
    act(() => dragSeparator(dividerAfter("t1"), 0.1 * VIEWPORT.width, 0));
    await settle();
    expect(shareOf("t1")).toBeCloseTo(100 / 3 + 10);
    expect(shareOf("t2")).toBeCloseTo(100 / 3 - 10);
    fireEvent.doubleClick(dividerAfter("t1"));
    await settle();
    expect(shareOf("t1")).toBeCloseTo(100 / 3);
    expect(shareOf("t2")).toBeCloseTo(100 / 3);
    expect(shareOf("t3")).toBeCloseTo(100 / 3);
    expect(savedRootSizes()?.[0]).toBeCloseTo(100 / 3);
  });

  it("picking a preset — even the current one — lays the cells out evenly again", async () => {
    seed(["t1", "t2"]);
    renderApp();
    await settle();
    act(() => dragSeparator(dividerAfter("t1"), 0.2 * VIEWPORT.width, 0));
    await settle();
    expect(shareOf("t1")).toBeCloseTo(70);

    // The bottom chrome's layout control arranges the tabs here, not the mosaic.
    expect(screen.getByTestId("cli-chrome-layout")).toHaveAttribute("data-target", "tabs");
    fireEvent.click(screen.getByTestId("cli-chrome-layout"));
    fireEvent.click(screen.getByTestId("cli-chrome-layout-columns"));
    await settle();
    expect(shareOf("t1")).toBeCloseTo(50);
    expect(shareOf("t2")).toBeCloseTo(50);
    expect(s().tabGridLayout[gridKey()]).toBeUndefined();
  });

  it("⌘⇧L cycles the tabs' preset and evens it; cycling back never brings old sizes back", async () => {
    seed(["t1", "t2"]);
    renderApp();
    await settle();
    act(() => dragSeparator(dividerAfter("t1"), 0.2 * VIEWPORT.width, 0));
    await settle();
    const cycle = () =>
      act(() => SHORTCUT_ACTIONS.find((a) => a.id === "viewer.cycle-foreground-layout")!.run());
    cycle();
    await settle();
    expect(workspaceTabGridPreset(s(), TEAM, WS)).toBe("rows");
    expect(panelOf("t1").parentElement!.getAttribute("style")).toContain("flex-direction: column");
    expect(shareOf("t1")).toBeCloseTo(50);
    cycle(); // grid
    cycle(); // columns again
    await settle();
    expect(workspaceTabGridPreset(s(), TEAM, WS)).toBe("columns");
    expect(shareOf("t1")).toBeCloseTo(50);
  });

  it("a short last row spans the width, and its rows resize on their own divider", async () => {
    seed(["t1", "t2", "t3"], { preset: "grid" });
    renderApp();
    await settle();
    // Two rows: [t1 t2] over [t3]; t3 fills its row alone.
    const topRow = panelOf("t1").parentElement!;
    expect(panelOf("t2").parentElement).toBe(topRow);
    const bottom = screen.getByTestId("terminal-grid-cell-t3").closest("[data-panel]")!;
    expect(bottom.parentElement).not.toBe(topRow);
    const rowDivider = topRow.closest("[data-panel]")!.nextElementSibling as HTMLElement;
    act(() => dragSeparator(rowDivider, 0, 0.1 * VIEWPORT.height));
    await settle();
    expect(savedRootSizes()?.[0]).toBeCloseTo(60);
  });

  it("closing a cell keeps the others' proportions; a new cell lays out evenly", async () => {
    seed(["t1", "t2", "t3"]);
    renderApp();
    await settle();
    act(() => dragSeparator(dividerAfter("t2"), 0.1 * VIEWPORT.width, 0));
    await settle();
    // t1:t2:t3 = 33⅓ : 43⅓ : 23⅓; dropping t1 leaves t2:t3 in that ratio.
    act(() => s().demoteTerminalPane(TEAM, WS, "t1"));
    await settle();
    expect(shareOf("t2")).toBeCloseTo((130 / 200) * 100, 0);
    // The same cell coming back (a tab returned, a Machine reconnected) takes its
    // place again in the sizes it was dragged to.
    act(() => s().paneOutTerminalTab(TEAM, WS, "t1"));
    await settle();
    expect(shareOf("t2")).toBeCloseTo(100 / 3 + 10);
    // A cell that was never there lays every cell out evenly.
    act(() => {
      seedTerminalTabs(TEAM, WS, ["t4"]);
    });
    await settle();
    act(() => s().paneOutTerminalTab(TEAM, WS, "t4"));
    await settle();
    for (const id of ["t1", "t2", "t3", "t4"]) expect(shareOf(id)).toBeCloseTo(25);
  });
});

describe("only the user's resizes are kept", () => {
  it("sizes a preset cleared never come back when a cell returns", async () => {
    seed(["t1", "t2", "t3"]);
    renderApp();
    await settle();
    act(() => dragSeparator(dividerAfter("t1"), 0.2 * VIEWPORT.width, 0));
    await settle();
    expect(shareOf("t1")).toBeCloseTo(100 / 3 + 20);
    act(() => s().demoteTerminalPane(TEAM, WS, "t3"));
    await settle();
    fireEvent.click(screen.getByTestId("cli-chrome-layout"));
    fireEvent.click(screen.getByTestId("cli-chrome-layout-columns"));
    await settle();
    expect(shareOf("t1")).toBeCloseTo(50);
    expect(s().tabGridLayout[gridKey()]).toBeUndefined();
    // The panel set the group held before comes back: it lays out evenly, and
    // nothing the group remembered reaches the saved sizes.
    act(() => s().paneOutTerminalTab(TEAM, WS, "t3"));
    await settle();
    for (const id of ["t1", "t2", "t3"]) expect(shareOf(id)).toBeCloseTo(100 / 3);
    expect(s().tabGridLayout[gridKey()]).toBeUndefined();
  });

  it("in the workspace mosaic too: a tile coming back after a preset lays out evenly", async () => {
    const three = ["ws-a", "ws-b", "ws-c"].map((id) => ({
      id,
      kind: "generic" as const,
      cwd: `/${id}`,
      status: "ready" as const,
    }));
    useViewerStore.setState({
      foregroundLayout: {},
      activeWorkspace: { [TEAM]: "ws-a" },
      terminalVisible: true,
      layoutMode: "shared",
      workspaces: { [TEAM]: three },
    });
    renderApp();
    await settle();
    const tile = (id: string) => document.querySelector<HTMLElement>(`[data-panel][id="${id}"]`)!;
    const share = (id: string) =>
      Number(/flex-grow:\s*([\d.]+)/.exec(tile(id).getAttribute("style") ?? "")![1]);
    act(() => dragSeparator(tile("ws-a").nextElementSibling as HTMLElement, 240, 0));
    await settle();
    expect(share("ws-a")).toBeCloseTo(100 / 3 + 20);
    act(() => useViewerStore.setState({ workspaces: { [TEAM]: three.slice(0, 2) } }));
    await settle();
    fireEvent.click(screen.getByTestId("cli-chrome-layout"));
    fireEvent.click(screen.getByTestId("cli-chrome-layout-columns"));
    await settle();
    expect(share("ws-a")).toBeCloseTo(50);
    act(() => useViewerStore.setState({ workspaces: { [TEAM]: three } }));
    await settle();
    for (const id of ["ws-a", "ws-b", "ws-c"]) expect(share(id)).toBeCloseTo(100 / 3);
    expect(s().foregroundLayout[TEAM]?.sizes).toEqual([]);
  });
});

describe("the workspace mosaic: a preset evens it too", () => {
  it("re-picking the preset after a drag evens the tiles", async () => {
    useViewerStore.setState({
      foregroundLayout: {},
      activeWorkspace: { [TEAM]: "ws-a" },
      terminalVisible: true,
      layoutMode: "shared",
      workspaces: {
        [TEAM]: [
          { id: "ws-a", kind: "generic", cwd: "/a", status: "ready" },
          { id: "ws-b", kind: "generic", cwd: "/b", status: "ready" },
        ],
      },
    });
    renderApp();
    await settle();
    const tile = (id: string) => document.querySelector<HTMLElement>(`[data-panel][id="${id}"]`)!;
    const share = (id: string) =>
      Number(/flex-grow:\s*([\d.]+)/.exec(tile(id).getAttribute("style") ?? "")![1]);
    expect(share("ws-a")).toBeCloseTo(50);
    act(() => dragSeparator(tile("ws-a").nextElementSibling as HTMLElement, 240, 0));
    await settle();
    expect(share("ws-a")).toBeCloseTo(70);
    expect(s().foregroundLayout[TEAM]?.sizes[0]).toBeCloseTo(70);

    expect(screen.getByTestId("cli-chrome-layout")).toHaveAttribute("data-target", "workspaces");
    fireEvent.click(screen.getByTestId("cli-chrome-layout"));
    fireEvent.click(screen.getByTestId("cli-chrome-layout-columns"));
    await settle();
    expect(share("ws-a")).toBeCloseTo(50);
    expect(share("ws-b")).toBeCloseTo(50);
  });
});
