// SPDX-License-Identifier: MIT
import { projectSessionStatus, resetSessionProjection } from "@/test/session-projection-fixture";
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, act, cleanup, fireEvent, screen, waitFor, within } from "@/test-utils";
import { DndContext } from "@dnd-kit/core";

// Seam: the real tile frame + terminal body (WorkspaceTerminalPanels), the real
// tile chrome and tab menu, the real viewer store and its layout verbs answered by
// the test terminal host, the real close verbs and the real shortcut registry.
// Only the xterm leaf and the Tauri commands (process probe, persistence) are
// stubbed. A terminal's display lives in the terminal manager, keyed by its
// session: a cell that moves re-attaches that display. The fake terminal counts
// its mounts, so a move that happens more often than the layout changes shows.

const probe = vi.hoisted(() => ({
  running: new Map<string, string>(),
  mounts: new Map<string, number>(),
}));

vi.mock("./shell-terminal-panel", async () => {
  const { useEffect } = await import("react");
  return {
    ShellTerminalPanel: ({ sessionId, visible }: { sessionId: string; visible: boolean }) => {
      useEffect(() => {
        probe.mounts.set(sessionId, (probe.mounts.get(sessionId) ?? 0) + 1);
      }, [sessionId]);
      return (
        <div className="xterm" style={{ display: visible ? "block" : "none" }}>
          <textarea data-sid={sessionId} aria-label={`terminal ${sessionId}`} />
        </div>
      );
    },
  };
});

vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
  sessionsRunningProcesses: vi.fn(async (ids: string[]) =>
    ids.map((id) => ({ foreground: probe.running.get(id) ?? null, background: [] })),
  ),
  // SW-TP CAP-11: lazy cwd resolver — return null so the caption keeps using
  // the tab name as its identity fallback for this seam's assertions.
  sessionCwd: vi.fn().mockResolvedValue(null),
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
  selectHiddenTerminalTabs,
  selectWorkspaceTabs,
  shouldRenderTabGrid,
  useViewerStore,
  workspacePanedTabIds,
} from "@/shared/stores/viewer-store";
import {
  installTerminalLayoutHarness,
  seedTerminalTabs,
  terminalHost,
} from "@/test/terminal-layout-harness";
import { useOrgStore } from "@/shared/stores/org-store";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { SHORTCUT_ACTIONS } from "@/shared/lib/shortcuts/registry";
import { WorkspaceTerminalPanels } from "./workspace-terminal-panels";
import { GRID_CAPTION_HEIGHT_PX, MIN_CELL_HEIGHT_PX, gridFitsCells } from "../lib/tab-grid-layout";
import {
  SPLIT_NOT_PLACED,
  SPLIT_NO_ROOM,
  resetSplitChainsForTests,
  resetZoomedCellsForTests,
} from "../lib/terminal-tab-keys";
import { toast } from "sonner";
import userEvent from "@testing-library/user-event";

const TEAM = "acme:default";
const WS = "ws-a";
const TABS = ["t1", "t2", "t3", "t4"];
const NAMES: Record<string, string> = {
  t1: "claude SW-UX-CLI-10",
  t2: "claude SW-UX-CLI-12",
  t3: "build",
  t4: "shell",
};

installTerminalLayoutHarness();

function seed() {
  useViewerStore.setState({
    foregroundLayout: {},
    activeWorkspace: { [TEAM]: WS },
    terminalVisible: true,
    aiStatusBySession: {},
    workspaces: { [TEAM]: [{ id: WS, kind: "story", cwd: "/a", status: "ready" }] },
  });
  seedTerminalTabs(
    TEAM,
    WS,
    TABS.map((session) => ({ session, name: NAMES[session] })),
    { active: "t1", paned: TABS },
  );
}

const s = () => useViewerStore.getState();
const tabsIn = () => selectWorkspaceTabs(s(), TEAM, WS).map((t) => t.id);
const activeIn = () => selectActiveTerminalTab(s(), TEAM, WS);
const panedIn = () => workspacePanedTabIds(s(), TEAM, WS);
const caption = (id: string) => screen.queryByTestId(`terminal-grid-caption-${id}`);
const captionFocus = (id: string) => screen.getByTestId(`terminal-grid-caption-focus-${id}`);
const terminal = (id: string) => document.querySelector<HTMLElement>(`[data-sid="${id}"]`)!;
const settle = () => act(async () => new Promise((r) => setTimeout(r, 30)));

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

const originalPlatform = navigator.platform;
beforeEach(() => {
  resetSessionProjection();
  vi.clearAllMocks();
  resetSplitChainsForTests();
  resetZoomedCellsForTests();
  Object.defineProperty(navigator, "platform", { value: "MacIntel", configurable: true });
  probe.running = new Map();
  probe.mounts = new Map();
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
  useAgentStateStore.setState({ bySession: {}, exitBySession: {}, stateSince: {}, seenAt: {} });
  seed();
});

afterEach(async () => {
  vi.useRealTimers();
  await settle();
  cleanup();
  Object.defineProperty(navigator, "platform", { value: originalPlatform, configurable: true });
  vi.restoreAllMocks();
});

describe("grid cell captions", () => {
  it("tabs-mode caption focuses its own terminal with an earlier warm neighbour", async () => {
    const user = userEvent.setup();
    renderApp();
    act(() => s().collapseTabGridToTabs(TEAM, WS));
    act(() => s().setActiveTerminalTab(TEAM, "t2", WS));
    await settle();
    expect(terminal("t1").closest("[inert]")).not.toBeNull();
    const name = captionFocus("t2");
    fireEvent.click(name);
    expect(document.activeElement).toBe(terminal("t2"));
    for (const key of ["Enter", " "]) {
      act(() => name.focus());
      await user.keyboard(key === "Enter" ? "{Enter}" : " ");
      expect(document.activeElement).toBe(terminal("t2"));
    }
  });

  it("every cell carries a caption naming its tab, badged with its status in words", () => {
    projectSessionStatus("t2", "paused");
    renderApp();
    expect(shouldRenderTabGrid(s(), TEAM, WS)).toBe(true);
    const names = TABS.map((id) => {
      const cell = screen.getByTestId(`terminal-grid-cell-${id}`);
      const cap = caption(id)!;
      // The caption sits in its own cell, above that cell's terminal.
      expect(cell.contains(cap)).toBe(true);
      expect(cap.compareDocumentPosition(terminal(id)) & Node.DOCUMENT_POSITION_FOLLOWING).toBe(
        Node.DOCUMENT_POSITION_FOLLOWING,
      );
      expect(cap.textContent).toContain(NAMES[id]);
      // Badged even when idle: a status dot with the state in words.
      expect(within(cap).getByTestId(`terminal-grid-caption-status-${id}`)).toBeTruthy();
      return captionFocus(id).textContent + "|" + accessibleName(captionFocus(id));
    });
    expect(new Set(names).size).toBe(4);
    expect(accessibleName(captionFocus("t2"))).toBe(`${NAMES.t2} Needs approval`);
    expect(accessibleName(captionFocus("t3"))).toBe(`${NAMES.t3} Ready`);
    expect(within(caption("t2")!).getByRole("img", { name: /Needs approval/ })).toBeTruthy();
    // A ready cell's badge is the visible grey ring, never an invisible no-fill dot.
    expect(screen.getByTestId("terminal-grid-caption-status-t3")).toHaveAttribute(
      "data-mark",
      "ready",
    );
  });

  it("an ended cell reads as its tab does: the ended square and words, the exit named once", () => {
    renderApp();
    act(() => terminalHost().exit("t3", 0));
    act(() => terminalHost().exit("t4", null, "SIGKILL"));
    const ended = screen.getByTestId("terminal-grid-caption-status-t3");
    expect(ended).toHaveAttribute("data-mark", "ended");
    expect(ended).toHaveAccessibleName("Ended · exit 0");
    expect(accessibleName(captionFocus("t3"))).toBe(`${NAMES.t3} Ended, exit 0`);
    const crashed = screen.getByTestId("terminal-grid-caption-status-t4");
    expect(crashed).toHaveAttribute("data-mark", "crashed");
    expect(accessibleName(captionFocus("t4"))).toBe(`${NAMES.t4} Crashed, SIGKILL`);
    // No visible exit badge beside the name: the exit is only in the words.
    expect(captionFocus("t4").textContent).toBe(`Crashed, SIGKILL${NAMES.t4}`);
  });

  it("marks the focused cell's caption, and only it", () => {
    renderApp();
    expect(caption("t1")!.getAttribute("data-active")).toBe("true");
    expect(captionFocus("t1").getAttribute("aria-current")).toBe("true");
    for (const id of ["t2", "t3", "t4"]) {
      expect(caption(id)!.getAttribute("data-active")).toBe("false");
      expect(captionFocus(id).getAttribute("aria-current")).toBeNull();
    }
  });

  it("tabs-mode keeps the active tab's caption (the per-tab toolbar), and all tabs ⇄ one tab re-attaches the terminals, never rebuilds them", async () => {
    renderApp();
    const active = activeIn()!;
    act(() => s().collapseTabGridToTabs(TEAM, WS));
    // Only the active toolbar is mounted; its neighbour retains its terminal.
    await waitFor(() => expect(caption(active)).not.toBeNull());
    for (const id of TABS) {
      const shouldRender = id === active || id === "t2";
      expect(!!caption(id)).toBe(id === active);
      expect(!!document.querySelector(`[data-sid="${id}"]`)).toBe(shouldRender);
      if (shouldRender) expect(!!terminal(id).closest("[inert]")).toBe(id !== active);
    }
    act(() => s().gridAllTerminalTabs(TEAM, WS));
    await waitFor(() => expect(caption("t1")).not.toBeNull());
    // Each cell shows its own running session again: one slot each, nothing
    // opened anew, every tab still the same session.
    for (const id of TABS) {
      expect(document.querySelectorAll(`[data-sid="${id}"]`)).toHaveLength(1);
      expect(screen.getByTestId(`terminal-grid-cell-${id}`).contains(terminal(id))).toBe(true);
    }
    expect(terminalHost().opened).toHaveLength(0);
    expect(tabsIn()).toEqual(TABS);
    // The retained tab panes move to the tab body and back; the others return once.
    for (const id of TABS) expect(probe.mounts.get(id)).toBe(id === active || id === "t2" ? 3 : 2);
  });

  it("a layout change that keeps the cells' shape moves no terminal", async () => {
    renderApp();
    await settle();
    // Re-picking the preset the cells already have, and an unrelated store change.
    act(() => s().setTabGridPreset(TEAM, WS, "columns"));
    act(() => s().setActiveTerminalTab(TEAM, "t3", WS));
    await settle();
    for (const id of TABS) expect(probe.mounts.get(id)).toBe(1);
  });

  it("the × takes the tab's close verb: a running tab is Hidden, an idle one Ended", async () => {
    probe.running.set("t2", "vim");
    renderApp();
    fireEvent.click(screen.getByTestId("terminal-grid-caption-close-t2"));
    await waitFor(() => expect(tabsIn()).not.toContain("t2"));
    expect(selectHiddenTerminalTabs(s(), TEAM, WS).map((t) => t.id)).toEqual(["t2"]);
    fireEvent.click(screen.getByTestId("terminal-grid-caption-close-t3"));
    await waitFor(() => expect(tabsIn()).not.toContain("t3"));
    expect(selectHiddenTerminalTabs(s(), TEAM, WS).map((t) => t.id)).toEqual(["t2"]);
  });

  it("the × is mouse-only (out of the tab order); the caption's name is one stop, Delete closes", async () => {
    renderApp();
    const close = screen.getByTestId("terminal-grid-caption-close-t3");
    expect(close.getAttribute("tabindex")).toBe("-1");
    expect(close.getAttribute("aria-hidden")).toBe("true");
    expect(captionFocus("t3").tabIndex).toBe(0);
    captionFocus("t3").focus();
    fireEvent.keyDown(captionFocus("t3"), { key: "Backspace" });
    await waitFor(() => expect(tabsIn()).not.toContain("t3"));
  });
});

describe("caption gestures", () => {
  it("a click on the caption focuses that cell: it becomes active and its terminal takes the keyboard", async () => {
    renderApp();
    fireEvent.click(captionFocus("t3"));
    await settle();
    expect(activeIn()).toBe("t3");
    expect(document.activeElement).toBe(terminal("t3"));
  });

  it("Enter on the focused caption does the same", async () => {
    renderApp();
    captionFocus("t4").focus();
    // A focused <button> activates on Enter natively (a click).
    fireEvent.keyDown(captionFocus("t4"), { key: "Enter" });
    fireEvent.click(captionFocus("t4"));
    await settle();
    expect(activeIn()).toBe("t4");
    expect(document.activeElement).toBe(terminal("t4"));
  });

  it("a double-click zooms the cell: it fills the tile, the other cells kept warm behind it", async () => {
    renderApp();
    fireEvent.doubleClick(captionFocus("t3"));
    await settle();
    expect(shouldRenderTabGrid(s(), TEAM, WS)).toBe(false);
    expect(activeIn()).toBe("t3");
    expect(panedIn()).toEqual(TABS);
    expect(terminal("t3").closest("[style]")!.getAttribute("style")).toContain("display: block");
    // The zoomed cell's terminal moves to the tile; the others leave the screen,
    // still running — none of them is opened anew.
    for (const id of TABS) {
      const pane = document.querySelector(`[data-sid="${id}"]`);
      expect(pane).not.toBeNull();
      expect(!!pane?.closest("[inert]")).toBe(id !== "t3");
    }
    expect(terminalHost().opened).toHaveLength(0);
    expect(tabsIn()).toEqual(TABS);
  });

  it("the × never zooms or focuses its cell on the way", async () => {
    renderApp();
    const close = screen.getByTestId("terminal-grid-caption-close-t3");
    fireEvent.doubleClick(close);
    await settle();
    expect(shouldRenderTabGrid(s(), TEAM, WS)).toBe(true);
  });
});

const runSplit = () =>
  act(() => SHORTCUT_ACTIONS.find((a) => a.id === "viewer.split-terminal")!.run());

describe("Split", () => {
  it("⌘D opens a new terminal beside the focused cell, as a cell, with the keyboard in it", async () => {
    renderApp();
    fireEvent.click(captionFocus("t2"));
    await settle();
    runSplit();
    await settle();
    expect(terminalHost().opened).toHaveLength(1);
    const added = terminalHost().opened[0].session;
    expect(tabsIn()).toEqual(["t1", "t2", added, "t3", "t4"]);
    expect(panedIn()).toEqual(["t1", "t2", added, "t3", "t4"]);
    expect(activeIn()).toBe(added);
    expect(caption(added)).not.toBeNull();
    expect(caption(added)!.getAttribute("data-active")).toBe("true");
    await waitFor(() => expect(document.activeElement).toBe(terminal(added)));
  });

  it("from tabs, shows the focused terminal and the new one side by side (not the cells remembered)", async () => {
    renderApp();
    act(() => s().collapseTabGridToTabs(TEAM, WS));
    await settle();
    expect(shouldRenderTabGrid(s(), TEAM, WS)).toBe(false);
    runSplit();
    await settle();
    const added = terminalHost().opened[0].session;
    expect(shouldRenderTabGrid(s(), TEAM, WS)).toBe(true);
    expect(panedIn()).toEqual(["t1", added]);
    expect(tabsIn()).toEqual(["t1", added, "t2", "t3", "t4"]);
    expect(caption("t1")).not.toBeNull();
    expect(caption(added)!.getAttribute("data-active")).toBe("true");
    expect(caption("t2")).toBeNull();
  });

  it("the tab menu's Split splits beside the right-clicked tab, and shows its key", async () => {
    renderApp();
    fireEvent.contextMenu(screen.getByTestId("terminal-tab-t4"));
    const item = screen.getByRole("menuitem", { name: "Split" });
    expect(item).toHaveAttribute("aria-keyshortcuts", "Meta+D");
    expect(item).toHaveTextContent("Split⌘D");
    fireEvent.click(item);
    await settle();
    const added = terminalHost().opened[0].session;
    expect(tabsIn()).toEqual(["t1", "t2", "t3", "t4", added]);
    expect(panedIn()).toEqual(["t1", "t2", "t3", "t4", added]);
    expect(activeIn()).toBe(added);
  });
});

/** Size the tile's terminal pane as it would lay out on screen. */
function paneOf(width: number, height: number) {
  const pane = screen.getByTestId(`workspace-panels-${WS}`);
  vi.spyOn(pane, "getBoundingClientRect").mockReturnValue({
    width,
    height,
    top: 0,
    left: 0,
    right: width,
    bottom: height,
    x: 0,
    y: 0,
    toJSON: () => ({}),
  } as DOMRect);
}
const runExpandKey = () =>
  act(() => SHORTCUT_ACTIONS.find((a) => a.id === "viewer.toggle-terminal-zoom")!.run());

describe("Split has a floor (review: cap)", () => {
  it("refuses another cell when none would keep its readable rows under its caption: a pulse and a word", async () => {
    renderApp();
    fireEvent.click(captionFocus("t2"));
    await settle();
    // 2 columns × 2 rows fit at the floor (134px a row: caption + ~6 rows): 4 cells.
    paneOf(500, 300);
    runSplit();
    await settle();
    expect(terminalHost().opened).toEqual([]);
    expect(tabsIn()).toEqual(TABS);
    expect(toast).toHaveBeenCalledWith(SPLIT_NO_ROOM);
    expect(document.querySelector(`[data-pane-id="${WS}"]`)!.className).toMatch(/blocked-nav-/);
  });

  it("splits while a fifth cell still fits", async () => {
    renderApp();
    paneOf(500, 420); // 2 × 3 at the floor
    runSplit();
    await settle();
    expect(terminalHost().opened).toHaveLength(1);
    expect(toast).not.toHaveBeenCalled();
  });

  it("the floor counts the caption", () => {
    expect(MIN_CELL_HEIGHT_PX).toBe(110 + GRID_CAPTION_HEIGHT_PX);
    expect(gridFitsCells(500, 300, 4)).toBe(true);
    expect(gridFitsCells(500, 300, 5)).toBe(false);
    expect(gridFitsCells(0, 0, 50)).toBe(true); // unmeasured never refuses
  });
});

describe("Split, queued and honest (review nits)", () => {
  it("two quick ⌘Ds: the second splits beside the first one's new terminal", async () => {
    renderApp();
    fireEvent.click(captionFocus("t2"));
    await settle();
    runSplit();
    runSplit();
    await settle();
    await settle();
    const [first, second] = terminalHost().opened.map((o) => o.session);
    expect(tabsIn()).toEqual(["t1", "t2", first, second, "t3", "t4"]);
    expect(activeIn()).toBe(second);
  });

  it("a host that never announces the new terminal gets a word after 10 s", async () => {
    const real = s().addTerminalTab;
    useViewerStore.setState({ addTerminalTab: () => Promise.resolve() });
    renderApp();
    vi.useFakeTimers();
    runSplit();
    await act(async () => vi.advanceTimersByTimeAsync(10_000));
    expect(toast).toHaveBeenCalledWith(SPLIT_NOT_PLACED);
    vi.useRealTimers();
    useViewerStore.setState({ addTerminalTab: real });
  });
});

describe("zoom and back (review)", () => {
  it("⌘⇧M after a caption's zoom brings back the cells it was zoomed from, not every tab", async () => {
    act(() => s().demoteTerminalPane(TEAM, WS, "t4"));
    renderApp();
    await waitFor(() => expect(caption("t4")).toBeNull());
    fireEvent.doubleClick(captionFocus("t2"));
    await settle();
    expect(shouldRenderTabGrid(s(), TEAM, WS)).toBe(false);
    runExpandKey();
    await settle();
    expect(shouldRenderTabGrid(s(), TEAM, WS)).toBe(true);
    expect(panedIn()).toEqual(["t1", "t2", "t3"]);
    expect(caption("t4")).toBeNull();
  });

  it("the tile's Expanded · all tabs toggle does the same", async () => {
    act(() => s().demoteTerminalPane(TEAM, WS, "t1"));
    renderApp();
    await waitFor(() => expect(caption("t1")).toBeNull());
    fireEvent.doubleClick(captionFocus("t3"));
    await settle();
    fireEvent.click(screen.getByTestId(`tile-grid-toggle-${WS}`));
    await settle();
    expect(panedIn()).toEqual(["t2", "t3", "t4"]);
  });

  it("without a zoom, Expanded · all tabs still shows every tab", async () => {
    act(() => s().demoteTerminalPane(TEAM, WS, "t4"));
    renderApp();
    await waitFor(() => expect(caption("t4")).toBeNull());
    runExpandKey(); // back to tabs
    await settle();
    runExpandKey();
    await settle();
    expect(panedIn()).toEqual(TABS);
  });
});

describe("caption hints and keyboard close (review nits)", () => {
  it("hovering a caption pops nothing over the tile (no hover card)", async () => {
    renderApp();
    await userEvent.hover(captionFocus("t3"));
    await settle();
    expect(screen.queryByRole("tooltip")).toBeNull();
  });

  it("Delete on a caption moves the keyboard to the next cell's caption", async () => {
    renderApp();
    captionFocus("t3").focus();
    fireEvent.keyDown(captionFocus("t3"), { key: "Backspace" });
    await waitFor(() => expect(tabsIn()).not.toContain("t3"));
    await waitFor(() => expect(document.activeElement).toBe(captionFocus("t4")));
  });

  it("…and to the one before when it was the last", async () => {
    renderApp();
    captionFocus("t4").focus();
    fireEvent.keyDown(captionFocus("t4"), { key: "Backspace" });
    await waitFor(() => expect(tabsIn()).not.toContain("t4"));
    await waitFor(() => expect(document.activeElement).toBe(captionFocus("t3")));
  });
});

function accessibleName(el: HTMLElement): string {
  return (el.getAttribute("aria-labelledby") ?? "")
    .split(" ")
    .map((id) => document.getElementById(id)?.textContent ?? "")
    .join(" ")
    .trim();
}
