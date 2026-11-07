// SPDX-License-Identifier: MIT
import { projectSessionStatus, resetSessionProjection } from "@/test/session-projection-fixture";
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, screen, within, act, fireEvent, waitFor } from "@/test-utils";
import userEvent from "@testing-library/user-event";

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  listNotes: vi.fn().mockResolvedValue([]),
  readNote: vi
    .fn()
    .mockResolvedValue({ id: "n1", title: "Scratch", pinned: false, lastOpenedAt: 0, content: "" }),
  createNote: vi.fn().mockResolvedValue({
    id: "n2",
    title: "Untitled",
    pinned: false,
    lastOpenedAt: 0,
    content: "",
  }),
  writeNote: vi.fn().mockResolvedValue(undefined),
  updateNoteMeta: vi
    .fn()
    .mockResolvedValue({ id: "n1", title: "Scratch", pinned: false, lastOpenedAt: 0 }),
  deleteNote: vi.fn().mockResolvedValue([]),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
}));
vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: vi.fn(), debug: vi.fn() },
  initLogging: vi.fn().mockResolvedValue(undefined),
}));
vi.mock("../lib/terminal-close", async (orig) => ({
  ...(await orig<typeof import("../lib/terminal-close")>()),
  closeWorkspace: vi.fn(),
}));
const { mockActivate } = vi.hoisted(() => ({ mockActivate: vi.fn() }));
vi.mock("../lib/focus-cascade", async (orig) => ({
  ...(await orig<typeof import("../lib/focus-cascade")>()),
  activateAndFocusTrayWorkspace: mockActivate,
}));

import { WorkspaceTray } from "./workspace-tray";
import { focusActiveWorkspaceTerminal } from "../lib/focus-cascade";
import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";
import { installTerminalLayoutHarness, seedTerminalTabs } from "@/test/terminal-layout-harness";
import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";

// Both placements use the same single-row strip. happy-dom has no layout:
// the synthetic chip/strip rectangles let us exercise hidden counts and focus.

const TEAM = "acme:default";
const IDS = ["w-a", "w-b", "w-c", "w-d", "w-e"];
const CHIP_W = 100;
let STRIP_W = 250;
/** Per-chip width overrides (a chip growing in place). */
let chipWidth: Record<string, number> = {};
/** Epic-rail pill order (grouped mode), each RAIL_W wide. */
let RAILS: string[] = [];
const RAIL_W = 100;

// A ResizeObserver the test can fire for one element, as the browser would
// when that element's box changes.
const observers = new Set<{ cb: ResizeObserverCallback; els: Set<Element> }>();
class FakeResizeObserver {
  private entry: { cb: ResizeObserverCallback; els: Set<Element> };
  constructor(cb: ResizeObserverCallback) {
    this.entry = { cb, els: new Set() };
    observers.add(this.entry);
  }
  observe(el: Element) {
    this.entry.els.add(el);
  }
  unobserve(el: Element) {
    this.entry.els.delete(el);
  }
  disconnect() {
    observers.delete(this.entry);
  }
}
function fireResize(el: Element) {
  act(() => {
    for (const o of observers) if (o.els.has(el)) o.cb([], {} as ResizeObserver);
  });
}

/** Left edge of chip `id`: the widths of the chips before it. */
function chipLeft(id: string): number {
  let left = 0;
  for (const other of IDS) {
    if (other === id) return left;
    left += chipWidth[other] ?? CHIP_W;
  }
  return left;
}

installTerminalLayoutHarness();

/** A running terminal tab `sess-${id}` in workspace `id`, held by its host. */
const seedTab = (id: string) => seedTerminalTabs(TEAM, id, [`sess-${id}`]);
const story = (id: string): Workspace => ({ id, kind: "story", cwd: `/${id}`, status: "ready" });

function rect(left: number, width: number): DOMRect {
  return {
    left,
    right: left + width,
    top: 0,
    bottom: 32,
    width,
    height: 32,
    x: left,
    y: 0,
  } as DOMRect;
}

beforeEach(() => {
  resetSessionProjection();
  STRIP_W = 250;
  chipWidth = {};
  RAILS = [];
  observers.clear();
  vi.stubGlobal("ResizeObserver", FakeResizeObserver);
  useViewerStore.setState({
    foregroundLayout: {},
    activeWorkspace: { [TEAM]: "w-a" },
    workspaces: { [TEAM]: IDS.map(story) },
    softClosedWorkspaces: { [TEAM]: {} },
    mruWorkspaceStack: {},
    trayOrderByTeam: {},
    workspaceTraySeenCoachmark: true,
    layoutMode: null,
    terminalVisible: false,
    lastTerminalMode: "shared",
    aiStatusBySession: {},
  });
  IDS.forEach(seedTab);
  projectSessionStatus("sess-w-b", "unknown");
  projectSessionStatus("sess-w-e", "paused");
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (
    this: HTMLElement,
  ) {
    const chip = this.dataset.trayChip;
    if (chip) return rect(chipLeft(chip), chipWidth[chip] ?? CHIP_W);
    const railPill = this.dataset.epicRailPill;
    if (railPill !== undefined) return rect(RAILS.indexOf(railPill) * RAIL_W, RAIL_W);
    return rect(0, STRIP_W);
  });
  // Unscaled: an element's layout width is its drawn width (zoom 1).
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockImplementation(function (
    this: HTMLElement,
  ) {
    return this.getBoundingClientRect().width;
  });
  vi.clearAllMocks();
});
afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  useViewerStore.setState({ aiStatusBySession: {} });
  setBarSize("normal");
});

/** Settings → "Workspace bar size". */
function setBarSize(workspaceBarSize: "small" | "normal" | "large") {
  act(() =>
    useViewerStore.setState({
      settings: { ...useViewerStore.getState().settings, workspaceBarSize },
    }),
  );
}

function renderTray(topChrome = false) {
  const slot = document.createElement("div");
  document.body.appendChild(slot);
  const onNewWorkspace = vi.fn();
  render(
    <WorkspaceTray
      teamKey={TEAM}
      tree={null}
      isLightTheme={false}
      onNewWorkspace={onNewWorkspace}
      topChrome={topChrome}
      consoleRailSlot={topChrome ? slot : null}
    />,
  );
  return onNewWorkspace;
}

describe("WorkspaceTray — every workspace visible or counted", () => {
  // The pinned overflow control counts every chip outside the scrolling strip.
  it(`bottom tray: chips out of view surface as "+N" carrying the hidden status`, () => {
    renderTray(false);
    // 250px strip, 100px chips → c (200–300), d, e are cut off.
    const plus = screen.getByTestId("workspace-tray-overflow");
    expect(plus).toHaveTextContent("+3");
    const dot = screen.getByTestId("workspace-tray-overflow-status");
    expect(dot).toHaveAttribute("data-mark", "permission");
    expect(dot.querySelector(".cs-mark-halo")).not.toBeNull();
    expect(plus).toHaveAccessibleName(/^3 more workspaces: Needs approval$/);
    // It sits OUTSIDE the scrolling chip area, so it can't scroll away itself.
    expect(plus.closest(".overflow-x-auto, .overflow-y-auto")).toBeNull();
  });

  it(`the "+N" row names a root by its repo slug, as its pill does`, async () => {
    // Not in IDS: drawn past the strip's end, so it is counted by the "+N".
    const root: Workspace = {
      id: "root-widgets",
      kind: "root",
      cwd: "/r/widgets",
      status: "ready",
      sourceRepoSlug: "acme/widgets",
    };
    useViewerStore.setState({ workspaces: { [TEAM]: [...IDS.map(story), root] } });
    seedTab(root.id);
    renderTray(false);
    await userEvent.click(screen.getByTestId("workspace-tray-overflow"));
    const menu = await screen.findByTestId("workspace-tray-overflow-menu");
    expect(within(menu).getByTestId(`workspace-tray-hidden-row-${root.id}`)).toHaveTextContent(
      "acme/widgets",
    );
  });

  it(`bottom tray: the "+N" lists the hidden workspaces and activates one`, async () => {
    const onNew = renderTray(false);
    await userEvent.click(screen.getByTestId("workspace-tray-overflow"));
    const menu = await screen.findByTestId("workspace-tray-overflow-menu");
    for (const id of ["w-c", "w-d", "w-e"]) {
      expect(within(menu).getByTestId(`workspace-tray-hidden-row-${id}`)).toBeInTheDocument();
    }
    expect(within(menu).queryByTestId("workspace-tray-hidden-row-w-a")).toBeNull();
    useViewerStore.setState({ workspacePanelCollapsed: true, agentsWorkspaceReveal: null });
    await userEvent.click(within(menu).getByTestId("workspace-tray-hidden-row-w-e"));
    expect(mockActivate).toHaveBeenCalledWith(TEAM, "w-e");
    // A workspace click preserves the closed pane and queues no reveal.
    expect(useViewerStore.getState().workspacePanelCollapsed).toBe(true);
    expect(useViewerStore.getState().agentsWorkspaceReveal).toBeNull();
    // The launcher may be scrolled away with the chips; the list offers it too.
    await userEvent.click(screen.getByTestId("workspace-tray-overflow"));
    await userEvent.click(await screen.findByText("New workspace"));
    expect(onNew).toHaveBeenCalledTimes(1);
  });

  it(`bottom tray: no "+N" when every chip fits`, () => {
    STRIP_W = 600;
    renderTray(false);
    expect(screen.queryByTestId("workspace-tray-overflow")).toBeNull();
  });

  // Console placement uses the same scrolling, compacting and overflow behavior.
  it(`top (title-bar) tray: scrolls one row and counts hidden workspaces`, () => {
    renderTray(true);
    expect(screen.getByTestId("workspace-tray-overflow")).toHaveTextContent("+3");
    const chip = document.querySelector('[data-tray-chip="w-a"]')!;
    const chipArea = chip.parentElement!;
    expect(chipArea.className).not.toContain("flex-wrap");
    expect(chipArea.className).toContain("overflow-x-auto");
  });

  it(`top (title-bar) tray: no "+N" when every chip fits`, () => {
    STRIP_W = 600;
    renderTray(true);
    expect(screen.queryByTestId("workspace-tray-overflow")).toBeNull();
  });

  it("status of hidden chips only: a needs-permission chip in view doesn't badge the +N", () => {
    resetSessionProjection();
    projectSessionStatus("sess-w-a", "paused");
    renderTray();
    expect(screen.getByTestId("workspace-tray-overflow")).toHaveTextContent("+3");
    expect(screen.queryByTestId("workspace-tray-overflow-status")).toBeNull();
  });

  it("a chip that grows in place (goes bold, label arrives) re-measures: '+N' appears", () => {
    STRIP_W = 500; // five 100px chips fit exactly
    renderTray();
    expect(screen.queryByTestId("workspace-tray-overflow")).toBeNull();
    chipWidth = { "w-b": 160 }; // w-b widens → w-e now ends at 560
    fireResize(document.querySelector('[data-tray-chip="w-b"]')!);
    expect(screen.getByTestId("workspace-tray-overflow")).toHaveTextContent("+1");
    expect(screen.getByTestId("workspace-tray-overflow-status")).toHaveAttribute(
      "data-mark",
      "permission",
    );
  });
});

// ── The bottom bar stays one line at any width ──
// It never wraps: crowded, every chip but the focused one drops to a compact
// pill (status mark + short label), the focused chip keeps its full pill and is
// kept in view, and whatever still doesn't fit is counted by the "+N".
describe("WorkspaceTray — one line at any width (bottom bar)", () => {
  const chipEl = (id: string) => document.querySelector<HTMLElement>(`[data-tray-chip="${id}"]`)!;

  it("never wraps: the chip area scrolls sideways on a single row", () => {
    renderTray(false);
    const area = chipEl("w-a").parentElement!;
    expect(area.className).toContain("overflow-x-auto");
    expect(area.className).not.toContain("flex-wrap");
  });

  it("the row is as tall as its chips: floors, never a fixed height that clips them", () => {
    renderTray(false);
    const strip = screen.getByTestId("workspace-tray");
    expect(strip.className).toMatch(/(^|\s)min-h-8(\s|$)/);
    expect(strip.className).not.toMatch(/(^|\s)h-8(\s|$)/);
    // Each chip has a 28px floor (the epic rail pill's height), never a fixed height either.
    expect(chipEl("w-a").className).toMatch(/(^|\s)min-h-7(\s|$)/);
    expect(chipEl("w-a").className).not.toMatch(/(^|\s)h-7(\s|$)/);
  });

  it("chips read at the bar's own (larger) text size again, not a shrunken text-xs", () => {
    renderTray(false);
    expect(chipEl("w-a").parentElement!.className).not.toContain("text-xs");
  });

  it("crowded: every chip but the focused one goes compact; the focused keeps its full pill", () => {
    renderTray(false); // 250px strip, five 100px chips
    expect(chipEl("w-a")).not.toHaveAttribute("data-compact");
    for (const id of ["w-b", "w-c", "w-d", "w-e"]) {
      expect(chipEl(id)).toHaveAttribute("data-compact", "true");
    }
  });

  it("a compact chip still shows its status: needs-you stays visible", () => {
    renderTray(false);
    expect(chipEl("w-e")).toHaveAttribute("data-compact", "true");
    expect(screen.getByTestId("workspace-tab-w-e-ai-status-dot")).toHaveAttribute(
      "data-mark",
      "permission",
    );
    // The crew stack is the part a compact pill drops; the label stays.
    expect(within(chipEl("w-e")).getByText("w-e")).toBeInTheDocument();
  });

  it.each([false, true])(
    "fades whichever end has chips scrolled past it (topChrome=%s)",
    (topChrome) => {
      renderTray(topChrome);
      const area = chipEl("w-a").parentElement!;
      const box = { scrollLeft: 0, clientWidth: 250, scrollWidth: 500 };
      for (const k of Object.keys(box) as (keyof typeof box)[]) {
        Object.defineProperty(area, k, { configurable: true, get: () => box[k] });
      }
      act(() => area.dispatchEvent(new Event("scroll")));
      expect(area).toHaveAttribute("data-scroll-edges", "end");
      expect(area.style.maskImage).toContain("linear-gradient");
      box.scrollLeft = 100;
      act(() => area.dispatchEvent(new Event("scroll")));
      expect(area).toHaveAttribute("data-scroll-edges", "both");
      box.scrollLeft = 250;
      act(() => area.dispatchEvent(new Event("scroll")));
      expect(area).toHaveAttribute("data-scroll-edges", "start");
      box.scrollWidth = 250;
      box.scrollLeft = 0;
      act(() => area.dispatchEvent(new Event("scroll")));
      expect(area).toHaveAttribute("data-scroll-edges", "none");
      expect(area.style.maskImage).toBe("");
    },
  );

  it("roomy: full pills, nothing compact", () => {
    STRIP_W = 600;
    renderTray(false);
    expect(document.querySelector("[data-compact]")).toBeNull();
  });

  it("widening past what the full pills need restores them", () => {
    renderTray(false);
    expect(chipEl("w-b")).toHaveAttribute("data-compact", "true");
    STRIP_W = 600;
    fireResize(chipEl("w-a").parentElement!);
    expect(document.querySelector("[data-compact]")).toBeNull();
  });

  /** Each `scrollBy` on an element: which element, by how much. */
  function spyScrollBy() {
    const calls: { el: Element; left: number }[] = [];
    vi.spyOn(Element.prototype, "scrollBy").mockImplementation(function (
      this: Element,
      opts?: ScrollToOptions | number,
    ) {
      calls.push({ el: this, left: typeof opts === "object" ? (opts.left ?? 0) : 0 });
    });
    return calls;
  }

  it("keeps the focused chip in view when focus moves to one out of view (scrolling only the strip)", () => {
    const calls = spyScrollBy();
    const intoView = vi.spyOn(Element.prototype, "scrollIntoView");
    renderTray(false);
    calls.length = 0;
    act(() => useViewerStore.setState({ activeWorkspace: { [TEAM]: "w-e" } }));
    // w-e spans 400–500 in a 250px strip: scroll right by 250, plus the 16px
    // edge fade so the chip isn't under it.
    expect(calls).toEqual([{ el: chipEl("w-a").parentElement, left: 266 }]);
    // Never scrollIntoView, which would also scroll the page around the bar.
    expect(intoView).not.toHaveBeenCalled();
  });

  it.each([false, true])(
    "stops following chip size changes once the user scrolls the strip (topChrome=%s)",
    (topChrome) => {
      const calls = spyScrollBy();
      useViewerStore.setState({ activeWorkspace: { [TEAM]: "w-e" } });
      renderTray(topChrome);
      const area = chipEl("w-a").parentElement!;
      area.dispatchEvent(new Event("wheel"));
      calls.length = 0;
      chipWidth = { "w-a": 180 };
      fireResize(chipEl("w-a"));
      expect(calls).toEqual([]);
    },
  );

  for (const topChrome of [false, true]) {
    for (const gesture of ["wheel", "touchstart"]) {
      it(`keeps ${gesture} browsing across membership and bar-size measurements (topChrome=${topChrome})`, () => {
        const calls = spyScrollBy();
        useViewerStore.setState({ activeWorkspace: { [TEAM]: "w-e" } });
        renderTray(topChrome);
        const area = chipEl("w-e").parentElement!;
        fireEvent(area, new Event(gesture));
        calls.length = 0;
        act(() =>
          useViewerStore.setState({
            workspaces: { [TEAM]: IDS.filter((id) => id !== "w-b").map(story) },
          }),
        );
        act(() =>
          useViewerStore.setState({ workspaces: { [TEAM]: [...IDS.map(story), story("w-f")] } }),
        );
        act(() => setBarSize("large"));
        fireResize(area);
        expect(calls).toEqual([]);
        // A genuine active-id change resumes the same shared follow owner.
        act(() => useViewerStore.setState({ activeWorkspace: { [TEAM]: "w-d" } }));
        expect(calls.length).toBeGreaterThan(0);
      });
    }
    for (const keyboard of [false, true]) {
      it(`same-active overflow selection resumes follow (topChrome=${topChrome}, keyboard=${keyboard})`, async () => {
        const calls = spyScrollBy();
        useViewerStore.setState({ activeWorkspace: { [TEAM]: "w-e" } });
        renderTray(topChrome);
        const area = chipEl("w-e").parentElement!;
        fireEvent.wheel(area);
        // Keep activation a same-id store no-op, but use the real canonical
        // coordinator, as production does even for an already-active target.
        mockActivate.mockImplementationOnce(() => focusActiveWorkspaceTerminal(TEAM));
        await userEvent.click(screen.getByTestId("workspace-tray-overflow"));
        const row = screen.getByTestId("workspace-tray-hidden-row-w-e");
        calls.length = 0;
        if (keyboard) {
          row.focus();
          await userEvent.keyboard("{Enter}");
        } else await userEvent.click(row);
        expect(screen.queryByTestId("workspace-tray-overflow-menu")).toBeNull();
        expect(calls.length).toBeGreaterThan(0);
        calls.length = 0;
        chipWidth = { "w-a": 180 };
        fireResize(chipEl("w-a"));
        expect(calls.length).toBeGreaterThan(0);
      });
    }
  }

  it("does not flip back to full pills on a sliver of extra width (the '+N' it loses)", () => {
    renderTray(false); // full pills needed 500px
    STRIP_W = 520;
    fireResize(chipEl("w-a").parentElement!);
    expect(chipEl("w-b")).toHaveAttribute("data-compact", "true");
  });

  it.each([false, true])(
    "follows the focused chip when the chips settle (a late label pushes it out) (topChrome=%s)",
    (topChrome) => {
      const calls = spyScrollBy();
      useViewerStore.setState({ activeWorkspace: { [TEAM]: "w-e" } });
      renderTray(topChrome);
      calls.length = 0;
      chipWidth = { "w-a": 180 };
      fireResize(chipEl("w-a"));
      expect(calls.map((c) => c.left)).toEqual([346]);
    },
  );

  it("the title-bar switcher uses the same compact pills and keeps the focused workspace full", () => {
    renderTray(true);
    expect(chipEl("w-a").parentElement!.className).not.toContain("flex-wrap");
    expect(chipEl("w-a")).not.toHaveAttribute("data-compact");
    expect(chipEl("w-b")).toHaveAttribute("data-compact", "true");
  });

  it("the title-bar overflow menu can select a hidden workspace and keeps its unscaled size", async () => {
    setBarSize("large");
    renderTray(true);
    await userEvent.click(screen.getByTestId("workspace-tray-overflow"));
    const menu = screen.getByTestId("workspace-tray-overflow-menu");
    expect(menu.style.zoom).toBe("");
    await userEvent.click(within(menu).getByTestId("workspace-tray-hidden-row-w-e"));
    expect(mockActivate).toHaveBeenCalledWith(TEAM, "w-e");
  });

  it("the title-bar switcher follows focus to a hidden workspace", () => {
    const calls = spyScrollBy();
    renderTray(true);
    calls.length = 0;
    act(() => useViewerStore.setState({ activeWorkspace: { [TEAM]: "w-e" } }));
    expect(calls).toEqual([{ el: chipEl("w-e").parentElement, left: 266 }]);
  });
});

// ── CLI closed: the bar is the way back ──
describe("WorkspaceTray — clicking a pill while the CLI is closed", () => {
  it("reopens the panel to that workspace", async () => {
    useViewerStore.setState({
      layoutMode: "viewer",
      terminalVisible: false,
      lastTerminalMode: "shared",
    });
    STRIP_W = 600;
    renderTray(false);
    await userEvent.click(screen.getByRole("button", { name: /^Workspace w-b/ }));
    // It waits out the double-click window first (see the doc test below).
    await waitFor(() => expect(mockActivate).toHaveBeenCalledWith(TEAM, "w-b"));
    expect(useViewerStore.getState().layoutMode).toBe("shared");
    expect(useViewerStore.getState().terminalVisible).toBe(true);
  });

  it("reopens in place even when the last mode was full-screen, so the viewer stays visible", async () => {
    useViewerStore.setState({
      layoutMode: "viewer",
      terminalVisible: false,
      lastTerminalMode: "console",
    });
    STRIP_W = 600;
    renderTray(false);
    await userEvent.click(screen.getByRole("button", { name: /^Workspace w-b/ }));
    await waitFor(() => expect(mockActivate).toHaveBeenCalledWith(TEAM, "w-b"));
    expect(useViewerStore.getState().layoutMode).toBe("shared");
  });

  it("Enter/Space on a pill reopens at once: a keyboard press can't become a double-click", () => {
    vi.useFakeTimers();
    try {
      useViewerStore.setState({
        layoutMode: "viewer",
        terminalVisible: false,
        lastTerminalMode: "shared",
      });
      STRIP_W = 600;
      renderTray(false);
      // Keyboard activation of a button is a click with `detail` 0.
      fireEvent.click(screen.getByRole("button", { name: /^Workspace w-b/ }), { detail: 0 });
      // No timer advanced: it acted synchronously.
      expect(mockActivate).toHaveBeenCalledWith(TEAM, "w-b");
      expect(useViewerStore.getState().layoutMode).toBe("shared");
      expect(useViewerStore.getState().terminalVisible).toBe(true);
    } finally {
      vi.useRealTimers();
    }
  });

  it("Tabbing onto a chip scrolls it clear of the edge fade", () => {
    const calls: number[] = [];
    vi.spyOn(Element.prototype, "scrollBy").mockImplementation(function (
      this: Element,
      opts?: ScrollToOptions | number,
    ) {
      calls.push(typeof opts === "object" ? (opts.left ?? 0) : 0);
    });
    renderTray(false); // w-a focused; w-d spans 300–400 in a 250px strip
    calls.length = 0;
    act(() => screen.getByRole("button", { name: /^Workspace w-d/ }).focus());
    expect(calls).toEqual([166]);
  });

  it("a double-click opens the workspace's doc without reopening the CLI under it", async () => {
    vi.useFakeTimers();
    try {
      useViewerStore.setState({
        layoutMode: "viewer",
        terminalVisible: false,
        lastTerminalMode: "shared",
        workspaces: { [TEAM]: IDS.map((id) => ({ ...story(id), storyFilePath: `/r/${id}.md` })) },
      });
      STRIP_W = 600;
      const onOpenStoryDoc = vi.fn();
      render(
        <WorkspaceTray
          teamKey={TEAM}
          tree={null}
          isLightTheme={false}
          onNewWorkspace={vi.fn()}
          onOpenStoryDoc={onOpenStoryDoc}
        />,
      );
      const pill = screen.getByRole("button", { name: /^Workspace w-b/ });
      fireEvent.click(pill, { detail: 1 });
      fireEvent.click(pill, { detail: 2 });
      act(() => vi.advanceTimersByTime(1000));
      expect(onOpenStoryDoc).toHaveBeenCalledWith("/r/w-b.md");
      expect(mockActivate).not.toHaveBeenCalled();
      expect(useViewerStore.getState().layoutMode).toBe("viewer");
    } finally {
      vi.useRealTimers();
    }
  });

  it("a hidden workspace picked from the '+N' reopens the panel too", async () => {
    useViewerStore.setState({
      layoutMode: "viewer",
      terminalVisible: false,
      lastTerminalMode: "shared",
    });
    renderTray(false);
    await userEvent.click(screen.getByTestId("workspace-tray-overflow"));
    const menu = await screen.findByTestId("workspace-tray-overflow-menu");
    await userEvent.click(within(menu).getByTestId("workspace-tray-hidden-row-w-e"));
    expect(useViewerStore.getState().terminalVisible).toBe(true);
    expect(mockActivate).toHaveBeenCalledWith(TEAM, "w-e");
  });

  it("the closed state persists until the user acts: mounting the bar reopens nothing", () => {
    useViewerStore.setState({
      layoutMode: "viewer",
      terminalVisible: false,
    });
    renderTray(false);
    expect(useViewerStore.getState().layoutMode).toBe("viewer");
    expect(useViewerStore.getState().terminalVisible).toBe(false);
  });

  it("with the CLI open, a pill click leaves the layout alone (full-screen stays full-screen)", async () => {
    useViewerStore.setState({
      layoutMode: "console",
      terminalVisible: true,
      lastTerminalMode: "console",
    });
    STRIP_W = 600;
    renderTray(false);
    await userEvent.click(screen.getByRole("button", { name: /^Workspace w-b/ }));
    expect(useViewerStore.getState().layoutMode).toBe("console");
  });
});

// ── grouped mode: the epic rail gets the same "+N" ──
const sref = (filePath: string) => ({
  fileName: filePath.split("/").pop()!,
  filePath,
  dependsOn: null,
  status: null,
  documentType: null,
  supportingDocs: null,
  sourceRepo: null,
  team: null,
});
const EPICS = ["alpha", "beta", "gamma", "delta"];
const groupedTree: DerivedPipelineTree = {
  dirPath: "",
  dirName: "",
  absoluteDirPath: "",
  stories: [],
  subEpics: EPICS.map((e) => ({
    dirPath: e,
    dirName: e,
    absoluteDirPath: "",
    stories: [sref(`/r/${e}/S.md`)],
    subEpics: [],
    orderError: null,
    mainDoc: null,
  })),
  orderError: null,
  mainDoc: null,
};

describe("WorkspaceTray — grouped mode epic rail '+N'", () => {
  function renderGrouped(topChrome = false, minimizeDelta = false) {
    const ws = EPICS.map(
      (e): Workspace => ({
        id: `ws-${e}`,
        kind: "story",
        cwd: `/${e}`,
        status: "ready",
        storyFilePath: `/r/${e}/S.md`,
      }),
    );
    useViewerStore.setState({
      activeWorkspace: { [TEAM]: "ws-alpha" },
      workspaces: { [TEAM]: minimizeDelta ? ws.slice(0, -1) : ws },
      softClosedWorkspaces: { [TEAM]: minimizeDelta ? { "ws-delta": { workspace: ws[3] } } : {} },
      trayMode: { [TEAM]: "grouped" },
      selectedEpicDirName: {},
      aiStatusBySession: {},
    });
    ws.forEach((w) => seedTab(w.id));
    projectSessionStatus("sess-ws-delta", "paused");
    projectSessionStatus("sess-ws-beta", "unknown");
    const slot = document.createElement("div");
    document.body.appendChild(slot);
    render(
      <WorkspaceTray
        teamKey={TEAM}
        tree={groupedTree}
        isLightTheme={false}
        onNewWorkspace={vi.fn()}
        topChrome={topChrome}
        consoleRailSlot={topChrome ? slot : null}
      />,
    );
    RAILS = Array.from(document.querySelectorAll("[data-epic-rail-pill]")).map(
      (el) => el.getAttribute("data-epic-rail-pill")!,
    );
  }

  it("keeps a minimized epic's status and label outside opacity dimming", () => {
    STRIP_W = 600;
    renderGrouped(false, true);
    fireResize(document.querySelector("[data-epic-rail-pill]")!.parentElement!);
    const pill = screen.getByRole("button", { name: /^Epic Delta,/ });
    expect(pill).toHaveClass("cs-workspace-card-minimized");
    expect(pill).not.toHaveClass("opacity-60");
    const mark = pill.querySelector(".cs-mark")!;
    expect(mark).toHaveAttribute("data-mark", "permission");
    for (let ancestor: Element | null = mark; ancestor; ancestor = ancestor.parentElement) {
      expect(Number(getComputedStyle(ancestor).opacity || 1)).toBe(1);
    }
  });

  for (const topChrome of [false, true]) {
    it(`${topChrome ? "title-bar" : "bottom"} rail: hidden epics get a badged '+N' that selects them`, async () => {
      renderGrouped(topChrome);
      expect(RAILS).toHaveLength(4);
      // Rects are assigned after render: re-measure as a resize would.
      fireResize(document.querySelector("[data-epic-rail-pill]")!.parentElement!);
      const plus = screen.getByTestId("workspace-epic-rail-overflow");
      expect(plus).toHaveTextContent("+2"); // pills 3 & 4 sit past 250px
      expect(plus).toHaveAccessibleName(/^2 more epics: Needs approval$/);
      expect(plus.getAttribute("title")).toMatch(/^2 more epics: Needs approval( · \d+s)?$/);
      expect(
        screen.getByTestId("workspace-epic-rail-overflow-status").querySelector(".cs-mark-halo"),
      ).not.toBeNull();
      await userEvent.click(plus);
      const menu = await screen.findByTestId("workspace-epic-rail-overflow-menu");
      const hiddenKeys = RAILS.slice(2);
      for (const k of hiddenKeys) {
        expect(within(menu).getByTestId(`workspace-epic-rail-hidden-row-${k}`)).toBeInTheDocument();
      }
      await userEvent.click(within(menu).getByTestId(`workspace-epic-rail-hidden-row-${RAILS[3]}`));
      expect(useViewerStore.getState().selectedEpicDirName[TEAM]).toBe(RAILS[3]);
    });
  }

  it("a size change alone re-counts the rail's '+N' (a zoom change fires no resize)", () => {
    renderGrouped();
    fireResize(document.querySelector("[data-epic-rail-pill]")!.parentElement!);
    expect(screen.getByTestId("workspace-epic-rail-overflow")).toHaveTextContent("+2");
    STRIP_W = 400; // drawn smaller: every pill fits now
    setBarSize("small");
    expect(screen.queryByTestId("workspace-epic-rail-overflow")).toBeNull();
  });

  for (const topChrome of [false, true]) {
    it(`${topChrome ? "title-bar" : "bottom"} rail: the '+N' menu is drawn at ${topChrome ? "the title bar's size" : "the bar's size"}`, async () => {
      setBarSize("small");
      renderGrouped(topChrome);
      fireResize(document.querySelector("[data-epic-rail-pill]")!.parentElement!);
      await userEvent.click(screen.getByTestId("workspace-epic-rail-overflow"));
      const menu = await screen.findByTestId("workspace-epic-rail-overflow-menu");
      expect(menu.style.zoom).toBe(topChrome ? "" : "0.875");
      // A long list scrolls inside the window: capped at the space Radix measures.
      expect(menu.style.maxHeight).toContain("--radix-popover-content-available-height");
      expect(menu.className).toContain("overflow-y-auto");
    });
  }

  it("no rail '+N' when every epic pill fits", () => {
    STRIP_W = 400;
    renderGrouped();
    fireResize(document.querySelector("[data-epic-rail-pill]")!.parentElement!);
    expect(screen.queryByTestId("workspace-epic-rail-overflow")).toBeNull();
  });
});

// Full-screen CLI, grouped: the epic rail goes up into the title bar and the
// selected epic's story row stays at the bottom as a one-row strip — chips that
// scroll out of it must be counted and badged like the shared bar's.
describe("WorkspaceTray — full-screen CLI, grouped: the bottom story row's '+N'", () => {
  const alphaTree: DerivedPipelineTree = {
    dirPath: "",
    dirName: "",
    absoluteDirPath: "",
    stories: [],
    subEpics: [
      {
        dirPath: "alpha",
        dirName: "alpha",
        absoluteDirPath: "",
        stories: IDS.map((id) => sref(`/r/alpha/${id}.md`)),
        subEpics: [],
        orderError: null,
        mainDoc: null,
      },
    ],
    orderError: null,
    mainDoc: null,
  };

  it("hidden stories surface as a '+N' carrying the most urgent status", async () => {
    useViewerStore.setState({
      workspaces: {
        [TEAM]: IDS.map((id) => ({ ...story(id), storyFilePath: `/r/alpha/${id}.md` })),
      },
      trayMode: { [TEAM]: "grouped" },
      selectedEpicDirName: {},
      layoutMode: "console",
    });
    const slot = document.createElement("div");
    document.body.appendChild(slot);
    render(
      <WorkspaceTray
        teamKey={TEAM}
        tree={alphaTree}
        isLightTheme={false}
        onNewWorkspace={vi.fn()}
        topChrome
        consoleRailSlot={slot}
      />,
    );
    const row = screen.getByTestId("workspace-tray-children");
    expect(slot.contains(row)).toBe(false);
    const plus = within(row).getByTestId("workspace-tray-overflow");
    expect(plus).toHaveTextContent("+3"); // 250px row, five 100px chips
    expect(plus).toHaveAccessibleName(/^3 more workspaces: Needs approval$/);
    expect(within(row).getByTestId("workspace-tray-overflow-status")).toHaveAttribute(
      "data-mark",
      "permission",
    );
    await userEvent.click(plus);
    const menu = await screen.findByTestId("workspace-tray-overflow-menu");
    await userEvent.click(within(menu).getByTestId("workspace-tray-hidden-row-w-e"));
    expect(mockActivate).toHaveBeenCalledWith(TEAM, "w-e");
  });

  it("the bottom story row follows the bar size; the rail up in the title bar does not", async () => {
    setBarSize("large");
    useViewerStore.setState({
      workspaces: {
        [TEAM]: IDS.map((id) => ({ ...story(id), storyFilePath: `/r/alpha/${id}.md` })),
      },
      trayMode: { [TEAM]: "grouped" },
      selectedEpicDirName: {},
      layoutMode: "console",
    });
    const slot = document.createElement("div");
    document.body.appendChild(slot);
    render(
      <WorkspaceTray
        teamKey={TEAM}
        tree={alphaTree}
        isLightTheme={false}
        onNewWorkspace={vi.fn()}
        topChrome
        consoleRailSlot={slot}
      />,
    );
    const scaled = document.querySelector<HTMLElement>("[data-bar-size]")!;
    expect(scaled.style.zoom).toBe("1.25");
    expect(scaled).toContainElement(screen.getByTestId("workspace-tray-children"));
    expect(slot.contains(scaled)).toBe(false);
    expect(scaled).not.toContainElement(screen.getByTestId("workspace-epic-rail"));
    // The row's "+N" menu (portaled out of the box) is drawn at the bar's size too.
    await userEvent.click(screen.getByTestId("workspace-tray-overflow"));
    const menu = await screen.findByTestId("workspace-tray-overflow-menu");
    expect(menu.style.zoom).toBe("1.25");
  });
});

describe("WorkspaceTray — Workspace bar size setting", () => {
  const chipEl = (id: string) => document.querySelector<HTMLElement>(`[data-tray-chip="${id}"]`)!;
  const setSize = setBarSize;
  /** Give every element a layout width `1/zoom` of its drawn width, as a
   *  browser reports for an element inside a CSS-zoomed ancestor. */
  function drawAtZoom(zoom: number) {
    vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockImplementation(function (
      this: HTMLElement,
    ) {
      return this.getBoundingClientRect().width / zoom;
    });
  }
  afterEach(() => {
    setSize("normal");
    useViewerStore.setState({ trayMode: {} });
  });

  it.each([
    ["small", "0.875"],
    ["normal", "1"],
    ["large", "1.25"],
  ] as const)(
    "%s scales the whole bar by %s (pills, avatars, badges, Agents, height)",
    (size, zoom) => {
      setSize(size);
      renderTray(false);
      const scaled = document.querySelector<HTMLElement>("[data-bar-size]")!;
      expect(scaled).toHaveAttribute("data-bar-size", size);
      expect(scaled.style.zoom).toBe(zoom);
      // Parts with a floor (badge text, the 24px hit target) read it (agents.css).
      expect(scaled.style.getPropertyValue("--bar-zoom")).toBe(zoom);
      // Every strip of the bar sits inside the one scaled box.
      expect(scaled).toContainElement(screen.getByTestId("workspace-tray"));
    },
  );

  it("the grouped (two-row) bar scales both rows together", () => {
    setSize("large");
    useViewerStore.setState({ trayMode: { [TEAM]: "grouped" } });
    renderTray(false);
    const scaled = document.querySelector<HTMLElement>("[data-bar-size]")!;
    expect(scaled).toContainElement(screen.getByTestId("workspace-tray-children"));
    expect(scaled).toContainElement(screen.getByTestId("workspace-epic-rail"));
  });

  it("the bar never shrinks in its column (a squeezed bar would push its row off the bottom)", () => {
    renderTray(false);
    const bar = document.querySelector<HTMLElement>("[data-workspace-bar]")!;
    expect(bar.className).toContain("flex-shrink-0");
  });

  it("the chip menu opens outside the scaled box, so it lands at the pointer at every size", () => {
    setSize("small");
    renderTray(false);
    fireEvent.contextMenu(chipEl("w-b").querySelector("button")!, { clientX: 40, clientY: 50 });
    const menu = screen.getByRole("menu", { name: "Workspace actions" });
    expect(document.querySelector("[data-bar-size]")).not.toContainElement(menu);
  });

  it("the title-bar switcher (full-screen CLI) keeps the title bar's size", () => {
    setSize("large");
    renderTray(true);
    expect(document.querySelector("[data-bar-size]")).toBeNull();
  });

  it("changing the size re-measures the crowding at once (a zoom change resizes nothing in layout px)", () => {
    renderTray(false); // 250px strip, five 100px chips: crowded
    expect(chipEl("w-b")).toHaveAttribute("data-compact", "true");
    STRIP_W = 700; // drawn smaller now: everything fits, but no ResizeObserver fires
    setSize("small");
    expect(document.querySelector("[data-compact]")).toBeNull();
  });

  it("the back-to-full margin is measured at the bar's size: no flip-flop when scaled up", () => {
    drawAtZoom(2);
    renderTray(false); // full pills needed 500 drawn px
    // 56 layout px of margin is 112 drawn px at zoom 2: 600 is not enough room.
    STRIP_W = 600;
    fireResize(chipEl("w-a").parentElement!);
    expect(chipEl("w-b")).toHaveAttribute("data-compact", "true");
    STRIP_W = 620;
    fireResize(chipEl("w-a").parentElement!);
    expect(document.querySelector("[data-compact]")).toBeNull();
  });

  it("keeps the focused chip in view at any size: scrolls the strip in its own (layout) px", () => {
    drawAtZoom(2);
    const calls: number[] = [];
    vi.spyOn(Element.prototype, "scrollBy").mockImplementation(function (
      opts?: ScrollToOptions | number,
    ) {
      calls.push(typeof opts === "object" ? (opts.left ?? 0) : 0);
    });
    renderTray(false);
    calls.length = 0;
    act(() => useViewerStore.setState({ activeWorkspace: { [TEAM]: "w-e" } }));
    // w-e ends at 500 drawn px in a 250px strip; the 16px fade is 32 drawn px at
    // zoom 2: 282 drawn px is 141 layout px of scroll.
    expect(calls).toEqual([141]);
  });
});

it("reveals the new team's focused pill after manual scrolling in the old team, even with identical IDs", () => {
  const other = "acme:other";
  const s = useViewerStore.getState();
  useViewerStore.setState({
    activeWorkspace: { [TEAM]: "w-e", [other]: "w-e" },
    workspaces: { ...s.workspaces, [other]: IDS.map(story) },
  });
  for (const id of IDS) seedTerminalTabs(other, id, [`other-${id}`]);
  const calls: number[] = [];
  vi.spyOn(Element.prototype, "scrollBy").mockImplementation((opts?: ScrollToOptions | number) => {
    calls.push(typeof opts === "object" ? (opts.left ?? 0) : 0);
  });
  const draw = (teamKey: string) => (
    <WorkspaceTray teamKey={teamKey} tree={null} isLightTheme={false} onNewWorkspace={vi.fn()} />
  );
  const { rerender } = render(draw(TEAM));
  fireEvent.wheel(document.querySelector<HTMLElement>('[data-tray-chip="w-a"]')!.parentElement!);
  calls.length = 0;
  rerender(draw(other));
  expect(calls).toContain(266);
});

describe("workspace pill keyboard overflow in both CLI placements", () => {
  it.each([false, true])(
    "keeps compact labels and lets the keyboard select hidden work (topChrome=%s)",
    async (topChrome) => {
      renderTray(topChrome);
      const active = document.querySelector<HTMLElement>('[data-tray-chip="w-a"]')!;
      expect(active).not.toHaveAttribute("data-compact");
      const compact = document.querySelector<HTMLElement>('[data-tray-chip="w-e"]')!;
      expect(compact).toHaveAttribute("data-compact", "true");
      expect(within(compact).getByText("w-e")).toBeVisible();
      const overflow = screen.getByTestId("workspace-tray-overflow");
      overflow.focus();
      await userEvent.keyboard("{Enter}");
      const menu = await screen.findByTestId("workspace-tray-overflow-menu");
      const target = within(menu).getByTestId("workspace-tray-hidden-row-w-e");
      target.focus();
      await userEvent.keyboard("{Enter}");
      expect(mockActivate).toHaveBeenCalledWith(TEAM, "w-e");
    },
  );
});
