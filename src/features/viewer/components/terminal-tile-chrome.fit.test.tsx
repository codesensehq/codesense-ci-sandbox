// SPDX-License-Identifier: MIT
import { projectSessionStatus, resetSessionProjection } from "@/test/session-projection-fixture";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, within } from "@/test-utils";
import userEvent from "@testing-library/user-event";
import { TerminalTileChrome } from "./terminal-tile-chrome";
import type { TerminalTab, Workspace } from "@/shared/stores/viewer-store";
import { tileWidths } from "../lib/tile-widths.test-util";

// The chrome fits itself to its measured width. happy-dom has no layout, so a
// fake ResizeObserver reports the tile width the REAL mosaic math gives each
// layout; the chrome then renders exactly what it would at that width, and the
// geometry is checked from the sizes it declares inline.

let tileWidth = 0;
/** Width reported for a tab pill's name slot (0 = unmeasured). */
let tabNameWidth = 0;
class FakeResizeObserver {
  constructor(private cb: ResizeObserverCallback) {}
  observe(el: Element) {
    const width = el.getAttribute("data-testid")?.startsWith("tile-chrome-")
      ? tileWidth
      : el.closest("[data-testid='terminal-tab-strip']")
        ? tabNameWidth
        : 0;
    this.cb([{ contentRect: { width } } as ResizeObserverEntry], this as unknown as ResizeObserver);
  }
  unobserve() {}
  disconnect() {}
}

const ws: Workspace = {
  id: "qh6",
  kind: "story",
  cwd: "/wt/qh6",
  status: "ready",
  storyId: "QH6-PARALLEL-3",
  storyFilePath: "/ctx/qh6.md",
};
const tab = (id: string): TerminalTab => ({ id, name: `claude ${id}`, sessionId: `s-${id}` });
const TABS = [tab("t1"), tab("t2"), tab("t3")];

function renderAt(
  width: number,
  overrides: Partial<React.ComponentProps<typeof TerminalTileChrome>> = {},
) {
  tileWidth = Math.round(width);
  const handlers = {
    onNewShell: vi.fn(),
    onToggleZoom: vi.fn(),
    onHideTile: vi.fn(),
    onCloseTile: vi.fn(),
    onSelectTab: vi.fn(),
  };
  const noop = vi.fn();
  render(
    <TerminalTileChrome
      workspace={ws}
      isFocused
      tabs={TABS}
      activeTabId="t1"
      onCloseTab={noop}
      onHideTab={noop}
      onEndTab={noop}
      onEndOtherTabs={noop}
      onEndAllTabs={noop}
      onReorderTab={noop}
      onRenameTab={noop}
      onFocusTile={noop}
      {...handlers}
      {...overrides}
    />,
  );
  return handlers;
}

const px = (v: string) => (v.endsWith("px") ? Number.parseFloat(v) : null);

/** The width an element can't shrink below, from the sizes the chrome declares:
 *  an inline width/min-width, 0 for a box that clips (overflow-hidden), else
 *  its children laid side by side plus its inline padding. */
function minWidthOf(el: HTMLElement): number {
  const declared = px(el.style.width) ?? px(el.style.minWidth);
  if (declared !== null) return declared;
  if (el.classList.contains("overflow-hidden")) return 0;
  const pad = px(el.style.paddingInline) ?? 0;
  return (
    Array.from(el.children).reduce((sum, c) => sum + minWidthOf(c as HTMLElement), 0) + pad * 2
  );
}

/** A control is hit-testable when nothing between it and the chrome root can
 *  clip it, and the chrome's unshrinkable content fits inside the tile — so
 *  flex never pushes it under a sibling or out of the tile. */
function expectHitTestable(control: HTMLElement, width: number) {
  const chrome = screen.getByTestId(`tile-chrome-${ws.id}`);
  for (let el = control.parentElement; el && el !== chrome; el = el.parentElement) {
    expect(el.classList.contains("overflow-hidden"), `clipped by ${el.className}`).toBe(false);
  }
  const occupied = Array.from(chrome.children).reduce(
    (sum, c) => sum + minWidthOf(c as HTMLElement),
    0,
  );
  expect(occupied).toBeLessThanOrEqual(width);
}

const LAYOUTS: { name: string; width: number; split: boolean }[] = [
  { name: "1-up", width: tileWidths(1, "columns", 900)[0], split: false },
  { name: "2-up", width: tileWidths(2, "columns", 900)[0], split: true },
  { name: "4-up grid", width: tileWidths(4, "grid", 900)[0], split: true },
  { name: "4-up columns", width: tileWidths(4, "columns", 900)[0], split: true },
  { name: "6-up grid", width: tileWidths(6, "grid", 900)[0], split: true },
  { name: "6-up columns @1200", width: tileWidths(6, "columns", 1200)[0], split: true },
  { name: "6-up columns @900", width: tileWidths(6, "columns", 900)[0], split: true },
];

beforeEach(() => {
  resetSessionProjection();
  vi.stubGlobal("ResizeObserver", FakeResizeObserver);
  projectSessionStatus("s-t2", "tool-running");
  projectSessionStatus("s-t3", "paused");
});
afterEach(() => {
  tabNameWidth = 0;
  vi.unstubAllGlobals();
});

describe("tile chrome at every layout — every control reachable", () => {
  for (const { name, width, split } of LAYOUTS) {
    it(`${name} (${Math.round(width)}px): "+" is visible and hit-testable, and clicks`, async () => {
      const h = renderAt(width, split ? {} : { onToggleZoom: undefined });
      const plus = screen.getByRole("button", { name: "New terminal" });
      expectHitTestable(plus, Math.round(width));
      await userEvent.click(plus);
      expect(h.onNewShell).toHaveBeenCalledTimes(1);
    });

    it(`${name}: zoom / Hide / close are inline or in ⋮, and each works`, async () => {
      const h = renderAt(width, split ? {} : { onToggleZoom: undefined });
      const user = userEvent.setup();
      const inlineClose = screen.queryByTestId(`tile-close-${ws.id}`);
      if (inlineClose) {
        expectHitTestable(inlineClose, Math.round(width));
        expectHitTestable(screen.getByTestId(`tile-hide-${ws.id}`), Math.round(width));
        if (split) expectHitTestable(screen.getByTestId(`tile-zoom-${ws.id}`), Math.round(width));
        await user.click(inlineClose);
        expect(h.onCloseTile).toHaveBeenCalledTimes(1);
        return;
      }
      // Folded: nothing inline, all three in the ⋮ menu, which is hit-testable.
      expect(screen.queryByTestId(`tile-zoom-${ws.id}`)).toBeNull();
      expect(screen.queryByTestId(`tile-hide-${ws.id}`)).toBeNull();
      const kebab = screen.getByTestId(`tile-view-options-${ws.id}`);
      expectHitTestable(kebab, Math.round(width));
      for (const [key, handler] of [
        ["zoom", h.onToggleZoom],
        ["hide", h.onHideTile],
        ["close", h.onCloseTile],
      ] as const) {
        await user.click(screen.getByTestId(`tile-view-options-${ws.id}`));
        await user.click(await screen.findByTestId(`tile-menu-${key}-${ws.id}`));
        expect(handler).toHaveBeenCalledTimes(1);
      }
    });

    it(`${name}: a needs-permission tab is badged wherever it is`, () => {
      renderAt(width, split ? {} : { onToggleZoom: undefined });
      const pill = screen.queryByTestId("tab-t3-ai-status");
      if (pill) {
        // On its own pill — and the pill is really on screen: nothing clips it,
        // and the strip reserves a full-width slot for every pill plus the "+".
        expect(pill).toHaveAttribute("data-mark", "permission");
        const strip = screen.getByTestId("terminal-tab-strip");
        const pills = Array.from(strip.children).filter((c) => c.tagName === "DIV").length;
        expect(px(strip.style.minWidth)).toBeGreaterThanOrEqual(pills * 84 + 24);
        expectHitTestable(pill, Math.round(width));
        return;
      }
      // In overflow: the "+N" carries it, with the tab's own mark and its halo…
      const badge = screen.getByTestId(`tile-overflow-status-${ws.id}`);
      expect(badge).toHaveAttribute("data-mark", "permission");
      expect(badge.querySelector(".cs-mark-halo")).not.toBeNull();
      const trigger = screen.getByTestId(`tile-overflow-${ws.id}`);
      expect(trigger).toHaveAccessibleName(
        /more tabs?: (Needs approval|1 needs approval(, \d+ running a tool)?)/,
      );
      // The tooltip says the same, its parts joined by "·" where the name uses commas.
      expect(trigger.getAttribute("title")?.split(" · ").join(", ")).toBe(
        trigger.getAttribute("aria-label"),
      );
      expectHitTestable(trigger, Math.round(width));
      // A visible "+N" already carries it: no duplicate roll-up beside it.
      expect(screen.queryByTestId(`tile-rollup-${ws.id}`)).toBeNull();
    });
  }

  it("zoomed 6-up tile: + reachable, hidden-tile hint compacts but keeps its badge", () => {
    renderAt(tileWidths(6, "columns", 900)[0], {
      isZoomed: true,
      hiddenTileCount: 5,
      hiddenAttentionScope: { sessionIds: ["s-t3"] },
    });
    expectHitTestable(screen.getByRole("button", { name: "New terminal" }), 150);
    expect(screen.getByTestId(`tile-hidden-hint-${ws.id}`)).toHaveTextContent("+5");
    expect(screen.getByTestId(`tile-hidden-attention-${ws.id}`)).toBeInTheDocument();
  });

  it("grid-active tile at 6-up: the grid toggle folds into ⋮ and still works", async () => {
    const onShowOneTab = vi.fn();
    renderAt(tileWidths(6, "columns", 1200)[0], {
      canGrid: true,
      gridActive: true,
      onShowOneTab,
      onShowAllTabs: vi.fn(),
    });
    expect(screen.queryByTestId(`tile-grid-toggle-${ws.id}`)).toBeNull();
    await userEvent.click(screen.getByTestId(`tile-view-options-${ws.id}`));
    await userEvent.click(await screen.findByTestId(`tile-menu-grid-${ws.id}`));
    expect(onShowOneTab).toHaveBeenCalledTimes(1);
  });

  it("narrower than the '+N' trigger: tabs list in ⋮ with dots, roll-up keeps status", async () => {
    const h = renderAt(80);
    expect(screen.queryByTestId(`tile-overflow-${ws.id}`)).toBeNull();
    expectHitTestable(screen.getByRole("button", { name: "New terminal" }), 80);
    expect(
      within(screen.getByTestId(`tile-rollup-${ws.id}`)).getByRole("img", {
        name: /^1 needs approval · 1 running a tool$/,
      }),
    ).toBeInTheDocument();
    await userEvent.click(screen.getByTestId(`tile-view-options-${ws.id}`));
    // The active tab is marked in the list.
    expect(await screen.findByTestId("tile-menu-tab-t1")).toHaveAttribute("aria-current", "true");
    expect(screen.getByTestId("tile-menu-tab-t3")).not.toHaveAttribute("aria-current");
    await userEvent.click(await screen.findByTestId("tile-menu-tab-t3"));
    expect(h.onSelectTab).toHaveBeenCalledWith("t3");
  });

  it("a shown pill keeps room for its name and reserves that width in the strip", () => {
    renderAt(tileWidths(6, "grid", 900)[0]); // 300px → one pill
    const strip = screen.getByTestId("terminal-tab-strip");
    const pills = Array.from(screen.getByRole("tablist").children) as HTMLElement[];
    expect(pills).toHaveLength(1);
    expect(px(pills[0].style.minWidth)).toBeGreaterThanOrEqual(98);
    // The strip reserves every pill it holds plus the "+", and never clips.
    expect(px(strip.style.minWidth)).toBe(98 + 24);
    expect(strip.classList.contains("overflow-hidden")).toBe(false);
  });

  it("the tile label middle-truncates so 6-up siblings stay distinct", () => {
    renderAt(tileWidths(6, "columns", 900)[0]);
    const label = screen.getByTestId(`tile-label-${ws.id}`);
    expect(label.lastElementChild?.textContent).toBe("LLEL-3");
    expect(label.lastElementChild?.className).toContain("flex-shrink-0");
  });

  it("a long tab name in a pill at its minimum width never spills its tail", () => {
    tabNameWidth = 21; // the name slot of an 84px pill
    renderAt(tileWidths(6, "grid", 900)[0], {
      tabs: [{ id: "t1", name: "claude-code-review-SW-UX-CLI-2", sessionId: "s-t1" }],
    });
    const strip = screen.getByTestId("terminal-tab-strip");
    const name = strip.querySelector<HTMLElement>("button span.cursor-pointer")!;
    const tail = name.lastElementChild;
    expect(name.textContent).toBe("claude-code-review-SW-UX-CLI-2");
    expect(tail?.textContent).toBe("2");
    // head (≥1 char) + "…" + tail ≤ the 3 characters that fit.
    expect(1 + 1 + (tail?.textContent?.length ?? 0)).toBeLessThanOrEqual(3);
  });
});
