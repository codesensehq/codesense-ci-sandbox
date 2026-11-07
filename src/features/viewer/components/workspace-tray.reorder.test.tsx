// SPDX-License-Identifier: MIT
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, renderHook, screen } from "@/test-utils";
import { DndContext, PointerSensor, useSensor, useSensors } from "@dnd-kit/core";

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
}));
vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: vi.fn(), debug: vi.fn() },
  initLogging: vi.fn().mockResolvedValue(undefined),
}));

import { WorkspaceTray } from "./workspace-tray";
import { teamTileOrder, useViewerStore, type Workspace } from "@/shared/stores/viewer-store";
import { seedTerminalTabs, installTerminalLayoutHarness } from "@/test/terminal-layout-harness";
import {
  consumeTrayReorderDrop,
  setTrayReorderIntent,
  takeTrayReorderIntent,
} from "../lib/tray-reorder-drag";
import { useShortcutsStore } from "@/shared/stores/shortcuts-store";
import { recordTrayMove, useLastTrayMove } from "@/shared/lib/tray-move-announcement";
import { teamWorkspaceOrder } from "../lib/workspace-order";
import { ROOTS_RAIL_KEY } from "../lib/group-workspaces-by-epic";
import { BAND_SLACK_X, BAND_SLACK_Y, pointerInBand } from "../hooks/use-tray-reorder";
import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";

const TEAM = "acme:default";
const A = "e-1-a";
const B = "e-1-b";
const C = "e-1-c";
const M = "e-1-min";
const story = (id: string): Workspace => ({ id, kind: "story", cwd: `/${id}`, status: "ready" });

installTerminalLayoutHarness();

const chipIds = () =>
  Array.from(document.querySelectorAll("[data-tray-chip]")).map((el) =>
    el.getAttribute("data-tray-chip"),
  );

/** The pills sit in one row, 100px apart, inside a strip whose band is the row's height. The
 *  strip's top is `stripTop`: the bottom of the window in the shared layout, the top (title bar)
 *  in the full-screen CLI. */
let stripTop = 600;
/** How far the strip is scrolled: the pills' drawn rects move left by it. */
let scrolled = 0;
const rect = (left: number, top: number, width: number, height: number) =>
  ({
    x: left,
    y: top,
    left,
    top,
    width,
    height,
    right: left + width,
    bottom: top + height,
    toJSON: () => ({}),
  }) as DOMRect;
const original = HTMLElement.prototype.getBoundingClientRect;
beforeAll(() => {
  HTMLElement.prototype.getBoundingClientRect = function (this: HTMLElement) {
    const chip = this.getAttribute("data-tray-chip");
    if (chip !== null) {
      const index = chipIds().indexOf(chip);
      return rect(10 + index * 100 - scrolled, stripTop + 2, 90, 28);
    }
    if (this.querySelector(":scope > [data-tray-chip]")) return rect(0, stripTop, 600, 32);
    const zone = this.getAttribute("data-test-rect");
    if (zone) {
      const [l, t, w, h] = zone.split(",").map(Number);
      return rect(l!, t!, w!, h!);
    }
    return rect(0, 0, 0, 0);
  };
});
afterAll(() => {
  HTMLElement.prototype.getBoundingClientRect = original;
});

beforeEach(() => {
  useViewerStore.setState({
    foregroundLayout: {},
    activeWorkspace: { [TEAM]: A },
    workspaces: { [TEAM]: [story(A), story(B), story(C)] },
    softClosedWorkspaces: { [TEAM]: { [M]: { workspace: story(M) } } },
    mruWorkspaceStack: {},
    trayOrderByTeam: {},
    workspaceOrderByTeam: {},
    // As the team view's tree sync leaves it: the tray's computed order is known.
    trayBaseOrderByTeam: { [TEAM]: [A, B, C, M] },
    railKeyByWorkspaceId: {},
    workspaceTraySeenCoachmark: true,
  });
  scrolled = 0;
  document.querySelectorAll("[data-test-rect]").forEach((el) => el.remove());
  [A, B, C, M].forEach((id) => seedTerminalTabs(TEAM, id, [`sess-${id}`]));
  takeTrayReorderIntent();
});

/** The team view's DndContext, reduced to its drag end's first step. */
function Harness({ children }: { children: React.ReactNode }) {
  const sensors = useSensors(useSensor(PointerSensor, { activationConstraint: { distance: 6 } }));
  return (
    <DndContext
      sensors={sensors}
      onDragEnd={() => {
        if (!consumeTrayReorderDrop()) split();
      }}
      onDragCancel={() => takeTrayReorderIntent()}
    >
      {children}
    </DndContext>
  );
}
const split = vi.fn();

function renderTray(layout: "shared" | "console") {
  const slot = document.createElement("div");
  document.body.appendChild(slot);
  stripTop = layout === "shared" ? 600 : 4;
  return render(
    <Harness>
      <WorkspaceTray
        teamKey={TEAM}
        tree={null}
        isLightTheme={false}
        onNewWorkspace={vi.fn()}
        topChrome={layout === "console"}
        consoleRailSlot={layout === "console" ? slot : null}
      />
    </Harness>,
  );
}

const frame = () =>
  act(async () => {
    await new Promise((r) => setTimeout(r, 40));
  });

async function dragChip(id: string, to: { x: number; y: number }, release = true) {
  const chip = document.querySelector<HTMLElement>(`[data-tray-chip="${id}"]`)!;
  const r = chip.getBoundingClientRect();
  const start = { clientX: r.left + 45, clientY: r.top + 14 };
  const opts = { pointerId: 1, isPrimary: true, button: 0 };
  fireEvent.pointerDown(chip, { ...opts, ...start });
  fireEvent.pointerMove(document, { ...opts, clientX: start.clientX - 10, clientY: start.clientY });
  await frame();
  fireEvent.pointerMove(document, { ...opts, clientX: to.x, clientY: to.y });
  await frame();
  if (release) fireEvent.pointerUp(document, { ...opts, clientX: to.x, clientY: to.y });
  await frame();
}

describe.each(["shared", "console"] as const)("tray pill reorder — %s CLI layout", (layout) => {
  beforeEach(() => split.mockClear());

  it("a drag along the tray shows the insertion line and moves the pill", async () => {
    renderTray(layout);
    expect(chipIds()).toEqual([A, B, C, M]);
    await dragChip(C, { x: 20, y: stripTop + 14 }, false);
    expect(screen.getByTestId("workspace-tray-insertion")).toBeInTheDocument();
    fireEvent.pointerUp(document, {
      pointerId: 1,
      isPrimary: true,
      clientX: 20,
      clientY: stripTop + 14,
    });
    await frame();
    expect(chipIds()).toEqual([C, A, B, M]);
    expect(useViewerStore.getState().workspaceOrderByTeam[TEAM]).toEqual([C, A, B, M]);
    expect(split).not.toHaveBeenCalled();
  });

  it("a drag out of the tray's band into the grid is a split, not a reorder", async () => {
    renderTray(layout);
    const gridY = layout === "shared" ? 200 : 400;
    await dragChip(C, { x: 20, y: gridY });
    expect(chipIds()).toEqual([A, B, C, M]);
    expect(split).toHaveBeenCalledTimes(1);
    expect(screen.queryByTestId("workspace-tray-insertion")).toBeNull();
  });

  it("released at its own place, nothing moves and nothing splits", async () => {
    renderTray(layout);
    await dragChip(B, { x: 150, y: stripTop + 14 });
    expect(chipIds()).toEqual([A, B, C, M]);
    expect(useViewerStore.getState().workspaceOrderByTeam[TEAM]).toBeUndefined();
    expect(split).not.toHaveBeenCalled();
  });

  it("Escape cancels a tray drag", async () => {
    renderTray(layout);
    await dragChip(C, { x: 20, y: stripTop + 14 }, false);
    fireEvent.keyDown(document, { key: "Escape", code: "Escape" });
    await frame();
    fireEvent.pointerUp(document, {
      pointerId: 1,
      isPrimary: true,
      clientX: 20,
      clientY: stripTop + 14,
    });
    await frame();
    expect(chipIds()).toEqual([A, B, C, M]);
  });
});

describe("the shared order: the tray follows a move made anywhere", () => {
  it("a move through the store (as the Agents pane makes it) reorders the pills", () => {
    renderTray("shared");
    act(() => {
      useViewerStore.getState().reorderWorkspace(TEAM, B, { before: A });
    });
    expect(chipIds()).toEqual([B, A, C, M]);
  });

  it("minimising and restoring a pill does not move it", () => {
    renderTray("shared");
    act(() => {
      useViewerStore.getState().reorderWorkspace(TEAM, C, { before: A });
    });
    expect(chipIds()).toEqual([C, A, B, M]);
    act(() => {
      useViewerStore.setState((s) => ({
        workspaces: { [TEAM]: s.workspaces[TEAM].filter((w) => w.id !== A) },
        softClosedWorkspaces: {
          [TEAM]: { [M]: { workspace: story(M) }, [A]: { workspace: story(A) } },
        },
      }));
    });
    expect(chipIds()).toEqual([C, A, B, M]);
    act(() => {
      useViewerStore.setState((s) => ({
        workspaces: { [TEAM]: [...s.workspaces[TEAM], story(A), story(M)] },
        softClosedWorkspaces: { [TEAM]: {} },
      }));
    });
    expect(chipIds()).toEqual([C, A, B, M]);
    // The pane reads the same order from the store.
    const s = useViewerStore.getState();
    expect(
      teamWorkspaceOrder({
        base: s.trayBaseOrderByTeam[TEAM],
        workspaces: s.workspaces[TEAM],
        softClosed: s.softClosedWorkspaces[TEAM],
        stored: s.workspaceOrderByTeam[TEAM],
      }),
    ).toEqual([C, A, B, M]);
  });

  it("Cmd+1..9 indexes the order shown", () => {
    renderTray("shared");
    act(() => {
      useViewerStore.getState().reorderWorkspace(TEAM, C, { before: A });
    });
    expect(useViewerStore.getState().trayOrderByTeam[TEAM]).toEqual([C, A, B, M]);
  });
});

// ───────────── a base workspace and two epics: any pill moves anywhere in the team ─────────────

const R = "root-1";
const A1 = "alpha-A-1";
const A2 = "alpha-A-2";
const B1 = "beta-B-1";
const rootWs = (): Workspace => ({
  id: R,
  kind: "root",
  cwd: "/r",
  status: "ready",
  sourceRepoSlug: "repo",
});
const epicStory = (id: string, filePath: string): Workspace => ({
  ...story(id),
  storyFilePath: filePath,
});
const epicNode = (dirName: string, storyPaths: string[]): DerivedPipelineTree => ({
  dirPath: dirName,
  dirName,
  absoluteDirPath: "",
  stories: storyPaths.map((filePath) => ({
    fileName: filePath.split("/").pop()!,
    filePath,
    dependsOn: null,
    status: null,
    documentType: null,
    supportingDocs: null,
    sourceRepo: null,
    team: null,
  })),
  subEpics: [],
  orderError: null,
  mainDoc: null,
});
const epicTree: DerivedPipelineTree = {
  ...epicNode("", []),
  subEpics: [
    epicNode("alpha", ["/r/alpha/A-1.md", "/r/alpha/A-2.md"]),
    epicNode("beta", ["/r/beta/B-1.md"]),
  ],
};

/** The strip's children in order: a pill's id, or "|" for an epic divider. */
const stripSequence = () => {
  const strip = document.querySelector("[data-tray-chip]")!.parentElement!;
  return Array.from(strip.children)
    .map((el) => (el.hasAttribute("data-tray-divider") ? "|" : el.getAttribute("data-tray-chip")))
    .filter((x) => x !== null);
};

function renderEpicTray(layout: "shared" | "console", mode: "flat" | "grouped" = "flat") {
  useViewerStore.setState({
    activeWorkspace: { [TEAM]: A2 },
    workspaces: {
      [TEAM]: [
        rootWs(),
        epicStory(A1, "/r/alpha/A-1.md"),
        epicStory(A2, "/r/alpha/A-2.md"),
        epicStory(B1, "/r/beta/B-1.md"),
      ],
    },
    softClosedWorkspaces: { [TEAM]: {} },
    trayMode: { [TEAM]: mode },
    selectedEpicDirName: {},
    trayBaseOrderByTeam: {},
  });
  [R, A1, A2, B1].forEach((id) => seedTerminalTabs(TEAM, id, [`sess-${id}`]));
  // The team view syncs the tree it hands the tray.
  useViewerStore.getState().syncWorkspaceFrontmatter(TEAM, epicTree);
  const slot = document.createElement("div");
  document.body.appendChild(slot);
  stripTop = layout === "shared" ? 600 : 4;
  render(
    <Harness>
      <WorkspaceTray
        teamKey={TEAM}
        tree={epicTree}
        isLightTheme={false}
        onNewWorkspace={vi.fn()}
        topChrome={layout === "console"}
        consoleRailSlot={layout === "console" ? slot : null}
      />
    </Harness>,
  );
}

describe.each(["shared", "console"] as const)(
  "tray pill reorder across epics — %s CLI layout",
  (layout) => {
    beforeEach(() => split.mockClear());

    it("the top pill, alone in its group, can be dragged below the others", async () => {
      renderEpicTray(layout);
      expect(stripSequence()).toEqual([R, "|", A1, A2, "|", B1]);
      // Past A2's middle (255), before B1's (355).
      await dragChip(R, { x: 300, y: stripTop + 14 });
      expect(chipIds()).toEqual([A1, A2, R, B1]);
      expect(useViewerStore.getState().workspaceOrderByTeam[TEAM]).toEqual([A1, A2, R, B1]);
      expect(split).not.toHaveBeenCalled();
      // A rule wherever the epic changes, never two in a row.
      expect(stripSequence()).toEqual([A1, A2, "|", R, "|", B1]);
      // Cmd+1..9 follows the order shown.
      expect(useViewerStore.getState().trayOrderByTeam[TEAM]).toEqual([A1, A2, R, B1]);
      const shortcuts = Array.from(document.querySelectorAll("[data-tray-shortcut]")).map(
        (el) => el.textContent,
      );
      expect(shortcuts).toEqual(["⌘1", "⌘2", "⌘3", "⌘4"]);
    });

    it("a pill dragged to the far end goes past every group", async () => {
      renderEpicTray(layout);
      await dragChip(A1, { x: 560, y: stripTop + 14 });
      expect(chipIds()).toEqual([R, A2, B1, A1]);
      expect(stripSequence()).toEqual([R, "|", A2, "|", B1, "|", A1]);
    });
  },
);

describe("the tray with an order that mixes epics", () => {
  it("draws one rule at each epic change, the same epic split in two included", () => {
    useViewerStore.setState({ workspaceOrderByTeam: { [TEAM]: [A1, B1, A2, R] } });
    renderEpicTray("shared");
    expect(stripSequence()).toEqual([A1, "|", B1, "|", A2, "|", R]);
    expect(useViewerStore.getState().trayOrderByTeam[TEAM]).toEqual([A1, B1, A2, R]);
  });

  it("grouped by epic: each epic's pills follow the order, numbered as shown", () => {
    useViewerStore.setState({ workspaceOrderByTeam: { [TEAM]: [A2, R, B1, A1] } });
    renderEpicTray("shared", "grouped");
    // A2 is active, so alpha is the selected epic: its pills in the person's order, no rules.
    expect(stripSequence()).toEqual([A2, A1]);
    expect(useViewerStore.getState().trayOrderByTeam[TEAM]).toEqual([A2, A1]);
    // The rail lists each epic where its first pill sits in the order, the roots always first.
    const rail = Array.from(document.querySelectorAll("[data-epic-rail-pill]")).map((el) =>
      el.getAttribute("data-epic-rail-pill"),
    );
    expect(rail).toEqual([ROOTS_RAIL_KEY, "alpha", "beta"]);
  });
});

// ───────────── the band, the release and the strip under a live drag ─────────────

const PTR = { pointerId: 1, isPrimary: true, button: 0 };
const strip = () => document.querySelector("[data-tray-chip]")!.parentElement!;

describe.each(["shared", "console"] as const)("a live tray drag — %s CLI layout", (layout) => {
  beforeEach(() => split.mockClear());

  it("a release just past the band's edge splits, though no frame ran after the last move", async () => {
    renderTray(layout);
    await dragChip(C, { x: 20, y: stripTop + 14 }, false);
    // Straight from the band to well outside it, released at once.
    const outside = layout === "shared" ? stripTop - 40 : stripTop + 80;
    fireEvent.pointerMove(document, { ...PTR, clientX: 20, clientY: outside });
    fireEvent.pointerUp(document, { ...PTR, clientX: 20, clientY: outside });
    await frame();
    expect(split).toHaveBeenCalledTimes(1);
    expect(chipIds()).toEqual([A, B, C, M]);
  });

  it("a release lands where it is let go, not where the last frame drew the line", async () => {
    renderTray(layout);
    await dragChip(A, { x: 20, y: stripTop + 14 }, false);
    // Past B's middle (155) and released in the same breath.
    fireEvent.pointerMove(document, { ...PTR, clientX: 160, clientY: stripTop + 14 });
    fireEvent.pointerUp(document, { ...PTR, clientX: 160, clientY: stripTop + 14 });
    await frame();
    expect(chipIds()).toEqual([B, A, C, M]);
    expect(split).not.toHaveBeenCalled();
  });

  it("a scroll of the strip under a still pointer moves the slot", async () => {
    renderTray(layout);
    await dragChip(A, { x: 200, y: stripTop + 14 }, false);
    // Scrolled 60px, the pointer at 200 is past C's middle (195) too.
    scrolled = 60;
    fireEvent.scroll(strip());
    expect(takeTrayReorderIntent()?.place).toEqual({ after: C });
  });

  it("a chip the list no longer holds keeps its slot but is never an anchor", async () => {
    renderTray(layout);
    const ghost = document.createElement("div");
    ghost.setAttribute("data-tray-chip", "gone");
    strip().insertBefore(ghost, strip().firstChild);
    // gone 10–100, A 110–200, B 210–300, C 310–400: 250 is before B, where the line is drawn.
    await dragChip(C, { x: 250, y: stripTop + 14 });
    expect(useViewerStore.getState().workspaceOrderByTeam[TEAM]).toEqual([A, C, B, M]);
    ghost.remove();
  });

  it("leaving the tray mid-drag leaves no reorder behind for a later drop", async () => {
    const slot = document.createElement("div");
    document.body.appendChild(slot);
    stripTop = layout === "shared" ? 600 : 4;
    const tray = (shown: boolean) => (
      <Harness>
        {shown && (
          <WorkspaceTray
            teamKey={TEAM}
            tree={null}
            isLightTheme={false}
            onNewWorkspace={vi.fn()}
            topChrome={layout === "console"}
            consoleRailSlot={layout === "console" ? slot : null}
          />
        )}
      </Harness>
    );
    const { rerender } = render(tray(true));
    await dragChip(C, { x: 20, y: stripTop + 14 }, false);
    rerender(tray(false));
    expect(takeTrayReorderIntent()).toBeNull();
    // The drag itself is still the team view's: let it go.
    fireEvent.pointerUp(document, { ...PTR, clientX: 20, clientY: stripTop + 14 });
    await frame();
  });

  it("a release over a tile's edge zone inside the band's slack splits there", async () => {
    renderTray(layout);
    // The tiles' edge zone right next to the strip: above it in the shared layout, below the
    // title bar's strip in the full-screen CLI.
    const zone = document.createElement("div");
    zone.setAttribute("data-drop-side", layout === "shared" ? "bottom" : "top");
    zone.setAttribute("data-test-rect", layout === "shared" ? "0,540,600,55" : "0,38,600,60");
    document.body.appendChild(zone);
    // 5px past the strip's edge: in the band's slack, and over the zone.
    const y = layout === "shared" ? stripTop - 5 : stripTop + 32 + 5;
    await dragChip(C, { x: 20, y });
    expect(split).toHaveBeenCalledTimes(1);
    expect(chipIds()).toEqual([A, B, C, M]);
    zone.remove();
  });

  it("at the pill's own place the line stays, dimmed", async () => {
    renderTray(layout);
    await dragChip(B, { x: 150, y: stripTop + 14 }, false);
    expect(screen.getByTestId("workspace-tray-insertion")).toHaveAttribute("data-at-home");
    fireEvent.pointerMove(document, { ...PTR, clientX: 20, clientY: stripTop + 14 });
    await frame();
    expect(screen.getByTestId("workspace-tray-insertion")).not.toHaveAttribute("data-at-home");
    fireEvent.keyDown(document, { key: "Escape", code: "Escape" });
    await frame();
  });
});

describe("a tray drag measures once a frame", () => {
  const opts = { pointerId: 1, isPrimary: true, button: 0 };

  it("moves within one frame share one measure of the tiles' zones", async () => {
    renderTray("shared");
    await dragChip(C, { x: 250, y: stripTop + 14 }, false);
    const spy = vi.spyOn(document, "querySelectorAll");
    const zoneReads = () => spy.mock.calls.filter(([sel]) => sel === "[data-drop-side]").length;
    try {
      for (const x of [240, 230, 220]) {
        fireEvent.pointerMove(document, { ...opts, clientX: x, clientY: stripTop + 14 });
      }
      expect(zoneReads()).toBe(1);
      await frame();
      fireEvent.pointerMove(document, { ...opts, clientX: 210, clientY: stripTop + 14 });
      expect(zoneReads()).toBe(2);
    } finally {
      spy.mockRestore();
    }
    fireEvent.pointerUp(document, { ...opts, clientX: 210, clientY: stripTop + 14 });
    await frame();
  });

  it("the release measures afresh, so it lands where the pills are now", async () => {
    renderTray("shared");
    await dragChip(C, { x: 20, y: stripTop + 14 }, false);
    fireEvent.pointerMove(document, { ...opts, clientX: 20, clientY: stripTop + 14 });
    // The pills shift under the pointer within the same frame, with no scroll event.
    scrolled = 100;
    fireEvent.pointerUp(document, { ...opts, clientX: 20, clientY: stripTop + 14 });
    await frame();
    // Measured now, A's middle is left of the pointer: C lands after A, not first.
    expect(chipIds()).toEqual([A, C, B, M]);
  });
});

describe("pointerInBand", () => {
  const box = { left: 0, right: 600, top: 4, bottom: 36 };
  it("holds the strip and its slack", () => {
    expect(pointerInBand(300, 20, box, [])).toBe(true);
    expect(pointerInBand(300, 36 + BAND_SLACK_Y, box, [])).toBe(true);
    expect(pointerInBand(300, 36 + BAND_SLACK_Y + 1, box, [])).toBe(false);
    expect(pointerInBand(-BAND_SLACK_X - 1, 20, box, [])).toBe(false);
  });
  it("gives a tile's edge zone the pointer wherever the two meet", () => {
    const zone = { left: 0, right: 600, top: 40, bottom: 100 };
    expect(pointerInBand(300, 39, box, [zone])).toBe(true);
    expect(pointerInBand(300, 40, box, [zone])).toBe(false);
    expect(pointerInBand(300, 36 + BAND_SLACK_Y, box, [zone])).toBe(false);
  });
});

describe("moving a pill from the keyboard, and hearing it", () => {
  it("Alt+Left and Alt+Right on a focused pill move it one place, and keep its focus", async () => {
    renderTray("shared");
    const pill = document.querySelector<HTMLElement>(`[data-tray-chip="${C}"] [data-ws-row]`)!;
    pill.focus();
    fireEvent.keyDown(pill, { key: "ArrowLeft", altKey: true });
    expect(chipIds()).toEqual([A, C, B, M]);
    expect(screen.getByTestId("workspace-tray-move-status")).toHaveTextContent(
      /^Moved .+ before .+$/,
    );
    fireEvent.keyDown(pill, { key: "ArrowRight", altKey: true });
    expect(chipIds()).toEqual([A, B, C, M]);
    expect(screen.getByTestId("workspace-tray-move-status")).toHaveTextContent(
      /^Moved .+ after .+$/,
    );
    await frame();
    expect(document.activeElement).toBe(pill);
  });

  it("an arrow without Alt, or at the strip's end, moves nothing", () => {
    renderTray("shared");
    const first = document.querySelector<HTMLElement>(`[data-tray-chip="${A}"] [data-ws-row]`)!;
    fireEvent.keyDown(first, { key: "ArrowLeft" });
    fireEvent.keyDown(first, { key: "ArrowLeft", altKey: true });
    expect(chipIds()).toEqual([A, B, C, M]);
    expect(useViewerStore.getState().workspaceOrderByTeam[TEAM]).toBeUndefined();
  });

  it("Alt+arrows from the pill's other controls (its close ×) move nothing", () => {
    renderTray("shared");
    const close = screen.getByTestId(`workspace-tab-${C}-close`);
    close.focus();
    fireEvent.keyDown(close, { key: "ArrowLeft", altKey: true });
    expect(chipIds()).toEqual([A, B, C, M]);
  });

  it("the pill's description names the keys that move it (Alt, off a Mac)", () => {
    const platform = Object.getOwnPropertyDescriptor(Navigator.prototype, "platform");
    try {
      Object.defineProperty(navigator, "platform", { configurable: true, value: "Win32" });
      renderTray("shared");
      const pill = document.querySelector<HTMLElement>(`[data-tray-chip="${C}"] [data-ws-row]`)!;
      expect(pill).toHaveAccessibleDescription(/Alt\+Left or Alt\+Right moves it/);
    } finally {
      delete (navigator as { platform?: string }).platform;
      if (platform) Object.defineProperty(Navigator.prototype, "platform", platform);
    }
  });

  it("the jump number reads ⌘ on a Mac and Ctrl+ elsewhere", () => {
    const platform = Object.getOwnPropertyDescriptor(Navigator.prototype, "platform");
    try {
      Object.defineProperty(navigator, "platform", { configurable: true, value: "MacIntel" });
      const mac = renderTray("shared");
      expect(document.querySelector("[data-tray-shortcut]")).toHaveTextContent("⌘1");
      mac.unmount();
      Object.defineProperty(navigator, "platform", { configurable: true, value: "Win32" });
      renderTray("shared");
      expect(document.querySelector("[data-tray-shortcut]")).toHaveTextContent("Ctrl+1");
    } finally {
      delete (navigator as { platform?: string }).platform;
      if (platform) Object.defineProperty(Navigator.prototype, "platform", platform);
    }
  });

  it("the jump number follows a rebind from Settings → Shortcuts", () => {
    const platform = Object.getOwnPropertyDescriptor(Navigator.prototype, "platform");
    useShortcutsStore.setState({ shortcuts: { "viewer.jump-workspace-1": "CmdOrCtrl+Shift+1" } });
    try {
      Object.defineProperty(navigator, "platform", { configurable: true, value: "Win32" });
      renderTray("shared");
      expect(document.querySelector("[data-tray-shortcut]")).toHaveTextContent("Ctrl+Shift+1");
    } finally {
      delete (navigator as { platform?: string }).platform;
      if (platform) Object.defineProperty(Navigator.prototype, "platform", platform);
      useShortcutsStore.setState({ shortcuts: {} });
    }
  });

  it("on a Mac the move keys are named Option, in the pill's words and the coachmark", () => {
    const platform = Object.getOwnPropertyDescriptor(Navigator.prototype, "platform");
    try {
      Object.defineProperty(navigator, "platform", { configurable: true, value: "MacIntel" });
      useViewerStore.setState({ workspaceTraySeenCoachmark: false });
      renderTray("shared");
      const pill = document.querySelector<HTMLElement>(`[data-tray-chip="${C}"] [data-ws-row]`)!;
      expect(pill).toHaveAccessibleDescription(/Option\+Left or Option\+Right moves it/);
      expect(screen.getByTestId("workspace-tray-coachmark")).toHaveTextContent("⌥←/→");
      expect(screen.getByTestId("workspace-tray-coachmark")).not.toHaveTextContent("Alt");
    } finally {
      delete (navigator as { platform?: string }).platform;
      if (platform) Object.defineProperty(Navigator.prototype, "platform", platform);
    }
  });

  it("a drop in the band whose move can't be made is said, not eaten", () => {
    renderTray("shared");
    act(() => {
      setTrayReorderIntent({ teamKey: TEAM, workspaceId: C, place: { before: "gone-away" } });
      consumeTrayReorderDrop({ teamKey: TEAM, workspaceId: C });
    });
    expect(screen.getByTestId("workspace-tray-move-status")).toHaveTextContent(
      /^Couldn't move .+: the bar changed during the drag$/,
    );
  });

  it("the live region belongs to its team: another team's tray does not inherit its words", () => {
    const OTHER = "acme:other";
    useViewerStore.setState((st) => ({
      workspaces: { ...st.workspaces, [OTHER]: [story("o-1"), story("o-2")] },
      softClosedWorkspaces: { ...st.softClosedWorkspaces, [OTHER]: {} },
    }));
    const view = renderTray("shared");
    act(() => recordTrayMove(TEAM, C, { before: A }));
    expect(screen.getByTestId("workspace-tray-move-status")).toHaveTextContent(/^Moved /);
    act(() => recordTrayMove(OTHER, "o-2", { before: "o-1" }));
    view.rerender(
      <Harness>
        <WorkspaceTray teamKey={OTHER} tree={null} isLightTheme={false} onNewWorkspace={vi.fn()} />
      </Harness>,
    );
    // The other team's move stood before its region mounted: not this region's to say.
    expect(screen.getByTestId("workspace-tray-move-status")).toHaveTextContent("");
  });

  it("a move whose anchor the strip no longer draws is still said, by the workspace's name", () => {
    renderTray("shared");
    act(() => recordTrayMove(TEAM, C, { before: "gone-away" }));
    expect(screen.getByTestId("workspace-tray-move-status")).toHaveTextContent(
      /^Moved .+ before gone-away$/,
    );
  });

  it("a tray that mounts again does not repeat the last move, nor does a team that opens again", () => {
    const first = renderTray("shared");
    const pill = document.querySelector<HTMLElement>(`[data-tray-chip="${C}"] [data-ws-row]`)!;
    fireEvent.keyDown(pill, { key: "ArrowLeft", altKey: true });
    expect(screen.getByTestId("workspace-tray-move-status")).toHaveTextContent(/^Moved /);
    first.unmount();
    const second = renderTray("shared");
    expect(screen.getByTestId("workspace-tray-move-status")).toHaveTextContent("");
    // A move, then the team closes: it is gone with the team.
    const again = document.querySelector<HTMLElement>(`[data-tray-chip="${C}"] [data-ws-row]`)!;
    fireEvent.keyDown(again, { key: "ArrowRight", altKey: true });
    expect(screen.getByTestId("workspace-tray-move-status")).toHaveTextContent(/^Moved /);
    act(() => useViewerStore.getState().closeTeam(TEAM));
    expect(renderHook(() => useLastTrayMove(TEAM)).result.current).toBeNull();
    second.unmount();
  });

  it("a move's words are cleared once announced", () => {
    vi.useFakeTimers();
    try {
      renderTray("shared");
      const pill = document.querySelector<HTMLElement>(`[data-tray-chip="${C}"] [data-ws-row]`)!;
      fireEvent.keyDown(pill, { key: "ArrowLeft", altKey: true });
      expect(screen.getByTestId("workspace-tray-move-status")).toHaveTextContent(/^Moved /);
      act(() => vi.advanceTimersByTime(3000));
      expect(screen.getByTestId("workspace-tray-move-status")).toHaveTextContent("");
    } finally {
      vi.useRealTimers();
    }
  });

  it("a pointer drop is announced in the tray's live region", async () => {
    renderTray("shared");
    await dragChip(C, { x: 20, y: stripTop + 14 });
    expect(screen.getByTestId("workspace-tray-move-status")).toHaveTextContent(
      /^Moved .+ before .+$/,
    );
  });
});

describe("a tree whose earlier epic holds the later-sorting ids", () => {
  // omega comes first in the pipeline, but its ids sort after alpha's.
  const W1 = "w-1";
  const W2 = "w-2";
  const A0 = "a-0";
  const tree: DerivedPipelineTree = {
    ...epicNode("", []),
    subEpics: [
      epicNode("omega", ["/r/omega/w-1.md", "/r/omega/w-2.md"]),
      epicNode("alpha", ["/r/alpha/a-0.md"]),
    ],
  };
  const shared = () => {
    const s = useViewerStore.getState();
    return teamWorkspaceOrder({
      base: s.trayBaseOrderByTeam[TEAM],
      workspaces: s.workspaces[TEAM],
      softClosed: s.softClosedWorkspaces[TEAM],
      stored: s.workspaceOrderByTeam[TEAM],
    });
  };

  beforeEach(() => {
    split.mockClear();
    useViewerStore.setState({
      activeWorkspace: { [TEAM]: W1 },
      workspaces: {
        [TEAM]: [
          epicStory(A0, "/r/alpha/a-0.md"),
          epicStory(W1, "/r/omega/w-1.md"),
          epicStory(W2, "/r/omega/w-2.md"),
        ],
      },
      softClosedWorkspaces: { [TEAM]: {} },
      trayMode: { [TEAM]: "flat" },
      trayBaseOrderByTeam: {},
    });
    [A0, W1, W2].forEach((id) => seedTerminalTabs(TEAM, id, [`sess-${id}`]));
  });

  it("after the sync the pane's order and the tiles' order match the pills", () => {
    // Before any sync the store has only the tree-less order (by id).
    expect(shared()).toEqual([A0, W1, W2]);
    useViewerStore.getState().syncWorkspaceFrontmatter(TEAM, tree);
    renderTray("shared");
    expect(chipIds()).toEqual([W1, W2, A0]);
    expect(shared()).toEqual([W1, W2, A0]);
    expect(teamTileOrder(useViewerStore.getState(), TEAM)).toEqual([W1, W2, A0]);
  });

  it("one drag stores an order that keeps the other pills where the tree put them", async () => {
    useViewerStore.getState().syncWorkspaceFrontmatter(TEAM, tree);
    render(
      <Harness>
        <WorkspaceTray teamKey={TEAM} tree={tree} isLightTheme={false} onNewWorkspace={vi.fn()} />
      </Harness>,
    );
    stripTop = 600;
    // a-0 (210–300) to before w-2 (mid 155): past w-1's middle (55).
    await dragChip(A0, { x: 100, y: stripTop + 14 });
    expect(useViewerStore.getState().workspaceOrderByTeam[TEAM]).toEqual([W1, A0, W2]);
    expect(chipIds()).toEqual([W1, A0, W2]);
  });

  it("a move made before the tree ever synced is made again over the tree's order", () => {
    // The pane shows the tree-less order (by id); w-1 is moved after w-2 there.
    expect(shared()).toEqual([A0, W1, W2]);
    expect(useViewerStore.getState().reorderWorkspace(TEAM, W1, { after: W2 })).toBe(true);
    expect(shared()).toEqual([A0, W2, W1]);
    // The sync replays the move over the real base: omega's pills stay ahead of alpha's.
    useViewerStore.getState().syncWorkspaceFrontmatter(TEAM, tree);
    expect(shared()).toEqual([W2, W1, A0]);
    expect(useViewerStore.getState().workspaceOrderByTeam[TEAM]).toEqual([W2, W1, A0]);
    expect(teamTileOrder(useViewerStore.getState(), TEAM)).toEqual([W2, W1, A0]);
  });

  it("an early move the real base already satisfies leaves no stored order", () => {
    // w-2 before a-0 holds in the tree's order as it is.
    useViewerStore.getState().reorderWorkspace(TEAM, W2, { before: A0 });
    useViewerStore.getState().syncWorkspaceFrontmatter(TEAM, tree);
    expect(useViewerStore.getState().workspaceOrderByTeam[TEAM]).toBeUndefined();
    expect(shared()).toEqual([W1, W2, A0]);
  });

  it("a newly opened workspace lands beside its own epic's pills, wherever they were moved", () => {
    const grown: DerivedPipelineTree = {
      ...tree,
      subEpics: [tree.subEpics[0]!, epicNode("alpha", ["/r/alpha/a+0.md", "/r/alpha/a-0.md"])],
    };
    useViewerStore.getState().syncWorkspaceFrontmatter(TEAM, tree);
    // The person put alpha's a-0 first.
    useViewerStore.setState({ workspaceOrderByTeam: { [TEAM]: [A0, W1, W2] } });
    // alpha's a+0 opens: first in alpha (its id sorts first), so right after omega's w-2 in the
    // computed order.
    useViewerStore.setState((s) => ({
      workspaces: { [TEAM]: [...s.workspaces[TEAM]!, epicStory("a+0", "/r/alpha/a+0.md")] },
    }));
    useViewerStore.getState().syncWorkspaceFrontmatter(TEAM, grown);
    render(
      <Harness>
        <WorkspaceTray teamKey={TEAM} tree={grown} isLightTheme={false} onNewWorkspace={vi.fn()} />
      </Harness>,
    );
    expect(chipIds()).toEqual(["a+0", A0, W1, W2]);
    // The tiles (and the pane, given the same groups) read the store's order the same way.
    expect(teamTileOrder(useViewerStore.getState(), TEAM)).toEqual(["a+0", A0, W1, W2]);
  });

  it("a queued move is made on the first sync even when its base is already in place", () => {
    expect(useViewerStore.getState().reorderWorkspace(TEAM, W1, { after: W2 })).toBe(true);
    // A base that already matches the tree's order arrives before the sync (as a load leaves it).
    useViewerStore.setState({ trayBaseOrderByTeam: { [TEAM]: [W1, W2, A0] } });
    useViewerStore.getState().syncWorkspaceFrontmatter(TEAM, tree);
    expect(useViewerStore.getState().workspaceOrderByTeam[TEAM]).toEqual([W2, W1, A0]);
    // Taken once: a later base change does not make the move again over a newer order.
    useViewerStore.setState({ workspaceOrderByTeam: {}, trayBaseOrderByTeam: { [TEAM]: [] } });
    useViewerStore.getState().syncWorkspaceFrontmatter(TEAM, tree);
    expect(useViewerStore.getState().workspaceOrderByTeam[TEAM]).toBeUndefined();
  });

  it("a restart before the tree syncs keeps the saved base, so nothing reshuffles", () => {
    useViewerStore.getState().syncWorkspaceFrontmatter(TEAM, tree);
    const base = useViewerStore.getState().trayBaseOrderByTeam[TEAM];
    // As the load leaves it: the base saved, the tree not loaded yet (the tray has none).
    useViewerStore.setState({ trayBaseOrderByTeam: { [TEAM]: base! } });
    renderTray("shared");
    expect(chipIds()).toEqual([W1, W2, A0]);
    expect(shared()).toEqual([W1, W2, A0]);
  });
});
