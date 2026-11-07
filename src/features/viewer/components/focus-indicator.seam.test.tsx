// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { readFileSync } from "fs";
import { resolve } from "path";
import { useLayoutEffect, useRef } from "react";
import { render, act, cleanup, fireEvent } from "@/test-utils";
import { DndContext } from "@dnd-kit/core";

// The one focus indicator, end to end through the real panels, tile chrome,
// focus cascade and stylesheet: the blue line follows REAL keyboard focus (not
// store selection), there is exactly one per level, it dims when focus leaves
// the terminal, and it is there at N=1. Only the xterm leaf is faked: a
// `.xterm textarea`, focused on reveal like the real panel.

vi.mock("./shell-terminal-panel", () => ({
  ShellTerminalPanel: ({
    sessionId,
    visible,
    autoFocus,
  }: {
    sessionId: string;
    visible: boolean;
    autoFocus: boolean;
  }) => {
    const ref = useRef<HTMLTextAreaElement>(null);
    const wasVisible = useRef(false);
    useLayoutEffect(() => {
      if (visible && !wasVisible.current && autoFocus) ref.current?.focus();
      wasVisible.current = visible;
    }, [autoFocus, visible]);
    return (
      <div className="xterm">
        <textarea ref={ref} data-sid={sessionId} />
      </div>
    );
  },
}));

vi.mock("@/tauri/commands", () => ({
  // Each team draws its agents with its own faces.
  avatarPacksListForTeam: vi.fn().mockResolvedValue({ packs: [], warnings: [] }),
  LOCAL_MACHINE_ID: "local",
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
  // The workspace's terminal chooser loads the team's machines.
  listMachines: vi.fn().mockResolvedValue([]),
  sessionsRunningProcesses: vi.fn((ids: string[]) =>
    Promise.resolve(ids.map(() => ({ foreground: null, background: [] }))),
  ),
}));

vi.mock("./source-editor-inset", () => ({ WorktreeEditorSurface: () => null }));

vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: vi.fn(), debug: vi.fn() },
  initLogging: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("@/shared/lib/pipeline-graph/use-story-details", () => ({
  useStoryDetails: vi.fn(() => ({ data: { title: null, storyText: "summary" }, loading: false })),
}));

import { useViewerStore, selectWorkspaceTabs } from "@/shared/stores/viewer-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { WorkspaceTerminalPanels } from "./workspace-terminal-panels";
import { focusWorkspaceTerminal } from "../lib/focus-cascade";
import { newTerminalTab } from "../lib/terminal-tab-keys";
import { seedTerminalTabs, installTerminalLayoutHarness } from "@/test/terminal-layout-harness";

installTerminalLayoutHarness();

const TEAM = "acme:default";
const A = "e-1-a";
const B = "e-1-b";

const BLUE = "#388bfd";
const GREY = "#30363d";

// The real stylesheet, so a colour assertion is an assertion on what paints.
const css = document.createElement("style");
css.textContent = readFileSync(resolve(__dirname, "../../../styles/focus-indicator.css"), "utf8");
document.head.appendChild(css);

function seed(ids: string[], sessions: Record<string, string[]>) {
  useViewerStore.setState({
    foregroundLayout: {},
    activeWorkspace: { [TEAM]: ids[0] },
    terminalVisible: true,
    workspaces: {
      [TEAM]: ids.map((id) => ({ id, kind: "story" as const, cwd: `/${id}`, status: "ready" })),
    },
  });
  for (const id of ids) seedTerminalTabs(TEAM, id, sessions[id]!);
}

const renderPanels = () =>
  render(
    <div data-xterm-host>
      <DndContext>
        <WorkspaceTerminalPanels teamKey={TEAM} repoPath="/repo" />
      </DndContext>
    </div>,
  );

const textarea = (c: HTMLElement, sid: string) =>
  c.querySelector<HTMLTextAreaElement>(`textarea[data-sid="${sid}"]`)!;
const chrome = (c: HTMLElement, ws: string) =>
  c.querySelector<HTMLElement>(`[data-testid="tile-chrome-${ws}"]`)!;
// SW-TP CAP-12: the focus-blue bottom-border moved from the chrome row to the
// per-tab caption. These helpers return the caption element so colour / line
// assertions still have a target without re-threading every test with ids.
const captionFor = (c: HTMLElement, sid: string) =>
  c.querySelector<HTMLElement>(`[data-testid="terminal-grid-caption-${sid}"]`)!;
const captionTestId = (sid: string) => `terminal-grid-caption-${sid}`;
const lineColor = (el: HTMLElement) => getComputedStyle(el).borderBottomColor;
/** Every painted focus indicator at one level, as `[testid, state]`. */
const tileLines = (c: HTMLElement) =>
  [...c.querySelectorAll<HTMLElement>(".cs-focus-line[data-focus-indicator]")].map((el) => [
    el.dataset.testid,
    el.dataset.focusIndicator,
  ]);
const paneLines = (c: HTMLElement) =>
  [...c.querySelectorAll<HTMLElement>(".cs-focus-pane[data-focus-indicator]")].map((el) => [
    el.dataset.testid,
    el.dataset.focusIndicator,
  ]);
const nextFrame = () =>
  act(async () => {
    await new Promise((r) => requestAnimationFrame(() => r(undefined)));
  });

let outside: HTMLButtonElement;
beforeEach(() => {
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
  outside = document.body.appendChild(document.createElement("button"));
});
afterEach(() => {
  cleanup();
  outside.remove();
  act(() => window.dispatchEvent(new Event("focus")));
});

describe("one focus indicator — follows real focus", () => {
  it("is present at N=1 and painted blue", () => {
    seed([A], { [A]: ["sid-a"] });
    const { container } = renderPanels();
    act(() => textarea(container, "sid-a").focus());

    expect(tileLines(container)).toEqual([[captionTestId("sid-a"), "on"]]);
    expect(lineColor(captionFor(container, "sid-a"))).toBe(BLUE);
  });

  it("moves with focus, exactly one tile line at a time", () => {
    seed([A, B], { [A]: ["sid-a"], [B]: ["sid-b"] });
    const { container } = renderPanels();
    act(() => textarea(container, "sid-a").focus());
    expect(tileLines(container)).toEqual([[captionTestId("sid-a"), "on"]]);

    // A click in B's terminal: the pointerdown selects B, xterm takes focus.
    act(() => {
      fireEvent.pointerDown(textarea(container, "sid-b"));
      textarea(container, "sid-b").focus();
    });
    expect(tileLines(container)).toEqual([[captionTestId("sid-b"), "on"]]);
    expect(lineColor(captionFor(container, "sid-b"))).toBe(BLUE);
    expect(lineColor(captionFor(container, "sid-a"))).toBe(GREY);
  });

  it("is not painted by store selection", () => {
    seed([A, B], { [A]: ["sid-a"], [B]: ["sid-b"] });
    const { container } = renderPanels();
    act(() => textarea(container, "sid-a").focus());

    act(() => useViewerStore.getState().switchWorkspace(TEAM, B));

    expect(chrome(container, B)).toHaveAttribute("data-focused");
    expect(tileLines(container)).toEqual([[captionTestId("sid-a"), "on"]]);
  });

  it("a press on a tile's chrome moves keyboard focus into its terminal", async () => {
    seed([A, B], { [A]: ["sid-a"], [B]: ["sid-b"] });
    const { container } = renderPanels();
    act(() => textarea(container, "sid-a").focus());

    act(() => {
      fireEvent.mouseDown(container.querySelector(`[data-testid="tile-label-${B}"]`)!);
    });
    await nextFrame();

    expect(document.activeElement).toBe(textarea(container, "sid-b"));
    expect(tileLines(container)).toEqual([[captionTestId("sid-b"), "on"]]);
  });
});

describe("one focus indicator — dims when focus leaves", () => {
  it("dims (not removed) when focus moves to app chrome, and returns on refocus", () => {
    seed([A], { [A]: ["sid-a"] });
    const { container } = renderPanels();
    act(() => textarea(container, "sid-a").focus());

    act(() => outside.focus());
    expect(tileLines(container)).toEqual([[captionTestId("sid-a"), "dim"]]);
    expect(lineColor(captionFor(container, "sid-a"))).not.toBe(BLUE);
    expect(lineColor(captionFor(container, "sid-a"))).not.toBe(GREY);

    act(() => textarea(container, "sid-a").focus());
    expect(tileLines(container)).toEqual([[captionTestId("sid-a"), "on"]]);
  });

  it("dims when the window loses focus, and returns when it regains it", () => {
    seed([A], { [A]: ["sid-a"] });
    const { container } = renderPanels();
    act(() => textarea(container, "sid-a").focus());

    act(() => window.dispatchEvent(new Event("blur")));
    expect(tileLines(container)).toEqual([[captionTestId("sid-a"), "dim"]]);

    act(() => window.dispatchEvent(new Event("focus")));
    expect(tileLines(container)).toEqual([[captionTestId("sid-a"), "on"]]);
  });
});

describe("one focus indicator — pane level (tab grid)", () => {
  const gridded = () => {
    seed([A], { [A]: ["sid-1", "sid-2", "sid-3"] });
    act(() => useViewerStore.getState().gridAllTerminalTabs(TEAM, A));
    return selectWorkspaceTabs(useViewerStore.getState(), TEAM, A).map((t) => t.id);
  };

  it("outlines only the cell holding focus, one pane line and one tile line", () => {
    const [, t2] = gridded();
    const { container } = renderPanels();
    act(() => textarea(container, "sid-2").focus());

    expect(paneLines(container)).toEqual([[`terminal-grid-cell-${t2}`, "on"]]);
    // SW-TP CAP-12: in grid-mode only the active tab's caption carries the
    // focus-blue bottom-border. Non-active cell captions stay neutral.
    expect(tileLines(container)).toEqual([[captionTestId("sid-2"), "on"]]);
    const cell = container.querySelector<HTMLElement>(`[data-testid="terminal-grid-cell-${t2}"]`)!;
    expect(getComputedStyle(cell).outlineColor).toBe(BLUE);
  });

  it("selecting a tab in the chrome moves focus into its cell, and the line follows", async () => {
    const [, , t3] = gridded();
    const { container } = renderPanels();
    act(() => textarea(container, "sid-1").focus());

    act(() => {
      fireEvent.click(container.querySelector(`[data-testid="terminal-tab-${t3}"]`)!, {
        detail: 1,
      });
    });
    await nextFrame();

    expect(document.activeElement).toBe(textarea(container, "sid-3"));
    expect(paneLines(container)).toEqual([[`terminal-grid-cell-${t3}`, "on"]]);
  });

  it("focusing a gridded workspace lands in its ACTIVE cell, not the first", () => {
    const [, t2] = gridded();
    const { container } = renderPanels();
    act(() => useViewerStore.getState().setActiveTerminalTab(TEAM, t2!, A));
    act(() => outside.focus());

    act(() => void focusWorkspaceTerminal(A));

    expect(document.activeElement).toBe(textarea(container, "sid-2"));
  });

  it("dims the pane line when focus moves to the tile's own chrome", () => {
    const [, t2] = gridded();
    const { container } = renderPanels();
    act(() => textarea(container, "sid-2").focus());

    act(() => container.querySelector<HTMLElement>(`[data-testid="terminal-tab-${t2}"]`)!.focus());

    expect(paneLines(container)).toEqual([[`terminal-grid-cell-${t2}`, "dim"]]);
    // SW-TP CAP-12: in grid-mode only the active tab's caption carries the
    // focus-blue bottom-border; it stays "on" while focus is on the tile chrome.
    expect(tileLines(container)).toEqual([[captionTestId("sid-2"), "on"]]);
  });
});

describe("one focus indicator — the focus blue is not a status colour", () => {
  // The terminal chrome and pane surfaces: the only blue left in them is a
  // control's own keyboard-focus ring (`focus-visible:`), which is focus too.
  const SURFACES = [
    "terminal-tile-chrome.tsx",
    "terminal-tab-bar.tsx",
    "terminal-find-bar.tsx",
    "pane-startup-overlay.tsx",
    "tile-needs-you-slot.tsx",
    "terminal-grid-caption.tsx",
    "terminal-ended-bar.tsx",
    "session-status-mark.tsx",
    "hidden-tab-menu-items.tsx",
  ];
  const blueUses = (file: string) =>
    [
      ...readFileSync(resolve(__dirname, file), "utf8").matchAll(
        /[^\s"'`]*(?:#388bfd|#58a6ff|#1f6feb|--cs-accent)[^\s"'`]*/gi,
      ),
    ]
      .map((m) => m[0])
      .filter((token) => !token.startsWith("focus-visible:"));

  it.each(SURFACES)("%s paints no focus blue outside a focus ring", (file) => {
    expect(blueUses(file)).toEqual([]);
  });

  it("the grid caption's own focus rings use the ring theme token, not a hardcoded colour", () => {
    // Themes v0: the identity cluster AND each icon-cluster button carry their own
    // focus ring, and it is the `ring` theme token — exactly ONE focus colour
    // across the file, never a hardcoded or status colour.
    const src = readFileSync(resolve(__dirname, "terminal-grid-caption.tsx"), "utf8");
    const rings = [...src.matchAll(/focus-visible:ring-(?!inset\b|\d)[^\s"'`]+/g)].map((m) => m[0]);
    expect(rings.length).toBeGreaterThanOrEqual(1);
    expect(new Set(rings)).toEqual(new Set(["focus-visible:ring-ring"]));
  });

  it("workspace-terminal-panels.tsx uses it only for the drop-zone highlight and the New terminal button", () => {
    expect(blueUses("workspace-terminal-panels.tsx")).toEqual([
      // A tile edge drop zone under the drag (transient, not a status).
      "bg-[var(--cs-accent,#388bfd)]/30",
      "outline-[var(--cs-accent,#388bfd)]",
      // The empty pane's primary "New terminal" button.
      "bg-[#1f6feb]",
      "hover:bg-[#388bfd]",
    ]);
  });
});

describe("one focus indicator — the store follows focus", () => {
  it("keyboard focus reaching another tile without a click selects it, and ⌘T acts there", async () => {
    seed([A, B], { [A]: ["sid-a"], [B]: ["sid-b"] });
    const { container } = renderPanels();
    act(() => textarea(container, "sid-a").focus());

    // Tab / a menu returning focus: no pointerdown, just focus.
    act(() => textarea(container, "sid-b").focus());

    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(B);
    expect(tileLines(container)).toEqual([[captionTestId("sid-b"), "on"]]);
    await act(async () => {
      expect(newTerminalTab(TEAM)).toBe(true);
      await Promise.resolve();
    });
    await vi.waitFor(() =>
      expect(selectWorkspaceTabs(useViewerStore.getState(), TEAM, B)).toHaveLength(2),
    );
    expect(selectWorkspaceTabs(useViewerStore.getState(), TEAM, A)).toHaveLength(1);
  });

  it("focus reaching another tile's chrome selects that tile", () => {
    seed([A, B], { [A]: ["sid-a"], [B]: ["sid-b"] });
    const { container } = renderPanels();
    act(() => textarea(container, "sid-a").focus());
    const [tabB] = selectWorkspaceTabs(useViewerStore.getState(), TEAM, B);

    act(() =>
      container.querySelector<HTMLElement>(`[data-testid="terminal-tab-${tabB!.id}"]`)!.focus(),
    );

    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(B);
    expect(tileLines(container)).toEqual([[captionTestId("sid-b"), "on"]]);
  });
});

describe("one focus indicator — tab bar interplay", () => {
  const gridded = () => {
    seed([A], { [A]: ["sid-1", "sid-2", "sid-3"] });
    act(() => useViewerStore.getState().gridAllTerminalTabs(TEAM, A));
    return selectWorkspaceTabs(useViewerStore.getState(), TEAM, A).map((t) => t.id);
  };
  const tabButton = (c: HTMLElement, id: string) =>
    c.querySelector<HTMLElement>(`[data-testid="terminal-tab-${id}"]`)!;

  it("focus reaching a grid cell without a click selects its tab", () => {
    const [, , t3] = gridded();
    const { container } = renderPanels();
    act(() => textarea(container, "sid-1").focus());

    act(() => textarea(container, "sid-3").focus());

    expect(tabButton(container, t3!)).toHaveAttribute("data-active", "true");
    expect(paneLines(container)).toEqual([[`terminal-grid-cell-${t3}`, "on"]]);
  });

  it("Enter/Space on a tab (a click with detail 0) selects it and keeps focus on the tab", async () => {
    const [, t2] = gridded();
    const { container } = renderPanels();
    act(() => tabButton(container, t2!).focus());

    act(() => {
      fireEvent.click(tabButton(container, t2!), { detail: 0 });
    });
    await nextFrame();

    expect(tabButton(container, t2!)).toHaveAttribute("data-active", "true");
    expect(document.activeElement).toBe(tabButton(container, t2!));
  });

  it("a double-click rename keeps its input (the click's terminal focus yields to it)", async () => {
    const [, t2] = gridded();
    const { container } = renderPanels();
    act(() => textarea(container, "sid-1").focus());
    const tab = tabButton(container, t2!);

    act(() => {
      fireEvent.click(tab, { detail: 1 });
      fireEvent.click(tab, { detail: 2 });
      fireEvent.doubleClick(tab.querySelector('[id$="-name"]')!);
    });
    await nextFrame();
    await nextFrame();

    const input = container.querySelector<HTMLInputElement>('input[aria-label="Tab name"]');
    expect(input).not.toBeNull();
    expect(document.activeElement).toBe(input);
  });

  it("opening a tab's context menu keeps the tile line on and dims only the pane line", async () => {
    const [, t2] = gridded();
    const { container } = renderPanels();
    act(() => textarea(container, "sid-2").focus());

    act(() => {
      fireEvent.contextMenu(tabButton(container, t2!), { clientX: 10, clientY: 10 });
    });
    await nextFrame();

    const menu = container.querySelector('[role="menu"]');
    expect(menu).not.toBeNull();
    expect(menu!.contains(document.activeElement)).toBe(true);
    // SW-TP CAP-12: in grid-mode the focus-blue bottom-border lives on the
    // active tab's caption (t2 was focused via sid-2).
    expect(tileLines(container)).toEqual([[captionTestId("sid-2"), "on"]]);
    expect(paneLines(container)).toEqual([[`terminal-grid-cell-${t2}`, "dim"]]);
  });
});

describe("one focus indicator — grid captions", () => {
  it("a focused caption selects its cell and shows exactly one blue per level", () => {
    seed([A], { [A]: ["sid-1", "sid-2", "sid-3"] });
    act(() => useViewerStore.getState().gridAllTerminalTabs(TEAM, A));
    const [, t2] = selectWorkspaceTabs(useViewerStore.getState(), TEAM, A).map((t) => t.id);
    const { container } = renderPanels();
    act(() => textarea(container, "sid-1").focus());

    const caption = container.querySelector<HTMLElement>(
      `[data-testid="terminal-grid-caption-focus-${t2}"]`,
    )!;
    act(() => caption.focus());

    expect(
      container.querySelector(`[data-testid="terminal-grid-cell-${t2}"]`)!.contains(caption),
    ).toBe(true);
    expect(paneLines(container)).toEqual([[`terminal-grid-cell-${t2}`, "on"]]);
    // SW-TP CAP-12: after the caption focus selects the cell, t2 (sid-2) is
    // the active tab; its caption carries the focus-blue bottom-border.
    expect(tileLines(container)).toEqual([[captionTestId("sid-2"), "on"]]);
    expect(container.querySelector(`[data-testid="terminal-tab-${t2}"]`)).toHaveAttribute(
      "data-active",
      "true",
    );
    expect(caption.className).not.toMatch(/--ring|--accent|#388bfd|#58a6ff/);
  });
});
