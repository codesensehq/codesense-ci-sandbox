// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import {
  TOAST_BOTTOM_GAP_PX,
  TOAST_EDGE_GAP_PX,
  TOASTER_OFFSET,
  TOASTER_POSITION,
} from "@/components/ui/toaster-placement";
import { PERSISTENT_TOAST_DROP_PX, TOAST_CHIP_HEIGHT_PX } from "@/components/ui/persistent-toast";
import { TERMINAL_FONT_PX_MAP } from "@/shared/lib/font-sizes";
import { defaultForegroundLayout, effectiveMosaicRoot } from "@/shared/stores/viewer-store";

import type { MosaicNode } from "./mosaic-tree";

// Toasts against the real tile arrangement. Each tile carries its controls (tabs,
// ×, menus) in a chrome bar along its top edge; a toast must never land on one.
// Geometry the app applies: the title bar is `h-10`, a tile's chrome bar `h-8`,
// each workspace-tray row `h-8` (up to three rows), and the shared-mode terminal
// band is 55% of the column by default. Sonner draws a toast 356 px wide; with
// three visible, the two behind the front one peek 14 px each above it. A front
// toast is at most ~96 px tall (title, a two-line description, an action).
const TITLE_BAR_H = 40;
const TILE_CHROME_H = 32;
const TRAY_ROW_H = 32;
const SHARED_TERMINAL_RATIO = 0.55;
const TOAST_W = 356;
const TOAST_FRONT_MAX_H = 96;
const TOAST_STACK_H = TOAST_FRONT_MAX_H + 2 * 14;
// A terminal row at the default font (xterm's cell is ~1.2× the font size).
const TERMINAL_ROW_H = Math.ceil(TERMINAL_FONT_PX_MAP.large * 1.2);

interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

const overlaps = (a: Rect, b: Rect) =>
  a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;

function tileRects(n: number, preset: "columns" | "grid", region: Rect): Rect[] {
  const ids = Array.from({ length: n }, (_, i) => `w${i}`);
  const root = effectiveMosaicRoot(defaultForegroundLayout({ preset }), ids);
  const out: Rect[] = [];
  const walk = (node: MosaicNode | null, r: Rect) => {
    if (!node) return;
    if (node.kind === "leaf") {
      out.push(r);
      return;
    }
    let at = 0;
    node.children.forEach((child, i) => {
      const share = node.sizes[i] / 100;
      if (node.axis === "row") {
        walk(child, { x: r.x + at, y: r.y, w: r.w * share, h: r.h });
        at += r.w * share;
      } else {
        walk(child, { x: r.x, y: r.y + at, w: r.w, h: r.h * share });
        at += r.h * share;
      }
    });
  };
  walk(root, region);
  return out;
}

/** Where the toast stack sits for a toaster position, as the app places it:
 *  the bottom edge rides on the measured bottom chrome plus the edge gap. */
function toastRect(
  position: "bottom-right" | "bottom-center" | "bottom-left",
  win: { w: number; h: number },
  bottomChrome: number,
): Rect {
  const bottom = win.h - bottomChrome - TOAST_BOTTOM_GAP_PX;
  const x =
    position === "bottom-right"
      ? win.w - TOAST_EDGE_GAP_PX - TOAST_W
      : position === "bottom-left"
        ? TOAST_EDGE_GAP_PX
        : (win.w - TOAST_W) / 2;
  return { x, y: bottom - TOAST_STACK_H, w: TOAST_W, h: TOAST_STACK_H };
}

/** A persistent toast collapsed to its chip, as the toaster places it: at the
 *  bottom gap, dropped by the persistent drop, {@link TOAST_CHIP_HEIGHT_PX} tall
 *  and at most a toast wide (its title decides; this is the widest). */
function chipRect(win: { w: number; h: number }, bottomChrome: number): Rect {
  const bottom = win.h - bottomChrome - TOAST_BOTTOM_GAP_PX + PERSISTENT_TOAST_DROP_PX;
  return {
    x: win.w - TOAST_EDGE_GAP_PX - TOAST_W,
    y: bottom - TOAST_CHIP_HEIGHT_PX,
    w: TOAST_W,
    h: TOAST_CHIP_HEIGHT_PX,
  };
}

function chromeBars(tiles: Rect[]): Rect[] {
  return tiles.map((t) => ({ x: t.x, y: t.y, w: t.w, h: TILE_CHROME_H }));
}

// Console: the terminal fills the window under the title bar; the grouped tray
// keeps a story row at the bottom. Shared: the terminal band sits below the
// viewer, the tray along its bottom — one or two rows, and three (its wrap cap)
// from a 900 px window; below that a three-row tray leaves 6-up grid tiles
// ~150 px tall, too short to hold a full toast stack under their chrome.
function layouts(win: { w: number; h: number }) {
  const column = win.h - TITLE_BAR_H;
  const out: { name: string; tiles: Rect; bottomChrome: number }[] = [];
  for (const bottomChrome of [0, TRAY_ROW_H]) {
    out.push({
      name: `console, bottom chrome ${bottomChrome}px`,
      tiles: { x: 0, y: TITLE_BAR_H, w: win.w, h: column - bottomChrome },
      bottomChrome,
    });
  }
  const band = column * SHARED_TERMINAL_RATIO;
  for (const rows of win.h >= 900 ? [1, 2, 3] : [1, 2]) {
    const tray = rows * TRAY_ROW_H;
    out.push({
      name: `shared, ${rows}-row tray`,
      tiles: { x: 0, y: win.h - band, w: win.w, h: band - tray },
      bottomChrome: tray,
    });
  }
  return out;
}

const WINDOWS = [
  { w: 1024, h: 768 },
  { w: 1280, h: 800 },
  { w: 1440, h: 900 },
  { w: 1920, h: 1080 },
];
const POSITIONS = ["bottom-right", "bottom-center", "bottom-left"] as const;

describe("toasts never cover tile controls", () => {
  it("the toaster rises from the bottom, above the measured bottom chrome", () => {
    expect(TOASTER_POSITION).toBe("bottom-right");
    expect(TOASTER_OFFSET.bottom).toContain("--cs-toast-bottom-clearance");
    expect(TOASTER_OFFSET.bottom).toContain(`${TOAST_BOTTOM_GAP_PX}px`);
  });

  for (const win of WINDOWS) {
    for (const preset of ["columns", "grid"] as const) {
      for (const layout of layouts(win)) {
        it(`6-up ${preset} @${win.w}×${win.h}, ${layout.name}: no toast on a tile's chrome or the tray`, () => {
          const bars = chromeBars(tileRects(6, preset, layout.tiles));
          expect(bars).toHaveLength(6);
          const tray: Rect = {
            x: 0,
            y: win.h - layout.bottomChrome,
            w: win.w,
            h: layout.bottomChrome,
          };
          for (const position of POSITIONS) {
            const toast = toastRect(position, win, layout.bottomChrome);
            for (const bar of bars) expect(overlaps(toast, bar)).toBe(false);
            expect(overlaps(toast, tray)).toBe(false);
          }
        });
      }
    }
  }

  for (const win of WINDOWS) {
    for (const preset of ["columns", "grid"] as const) {
      for (const layout of layouts(win)) {
        it(`6-up ${preset} @${win.w}×${win.h}, ${layout.name}: a collapsed persistent toast covers at most the last terminal row`, () => {
          const tiles = tileRects(6, preset, layout.tiles);
          const chip = chipRect(win, layout.bottomChrome);
          const tray: Rect = {
            x: 0,
            y: win.h - layout.bottomChrome,
            w: win.w,
            h: layout.bottomChrome,
          };
          expect(overlaps(chip, tray)).toBe(false);
          for (const bar of chromeBars(tiles)) expect(overlaps(chip, bar)).toBe(false);
          // It sits on the bottom-right tile, and any tile it touches loses at
          // most its last row.
          const touched = tiles.filter((t) => overlaps(chip, t));
          expect(touched.some((t) => Math.abs(t.x + t.w - win.w) < 1)).toBe(true);
          for (const t of touched) {
            expect(t.y + t.h - chip.y).toBeLessThanOrEqual(TERMINAL_ROW_H);
          }
        });
      }
    }
  }

  it("the chip fits in a terminal row; a full toast would cover several", () => {
    expect(TOAST_CHIP_HEIGHT_PX).toBeLessThanOrEqual(TERMINAL_ROW_H);
    expect(TOAST_FRONT_MAX_H).toBeGreaterThan(3 * TERMINAL_ROW_H);
  });

  it("the old top-right placement landed on the top-right tile's controls", () => {
    const win = { w: 1440, h: 900 };
    const bars = chromeBars(
      tileRects(6, "grid", { x: 0, y: TITLE_BAR_H, w: win.w, h: win.h - TITLE_BAR_H }),
    );
    const sonnerDefaultTopRight: Rect = {
      x: win.w - 24 - TOAST_W,
      y: 24,
      w: TOAST_W,
      h: TOAST_STACK_H,
    };
    expect(bars.some((bar) => overlaps(sonnerDefaultTopRight, bar))).toBe(true);
  });

  it("without the bottom-chrome clearance a toast sits on the tray", () => {
    const win = { w: 1440, h: 900 };
    const tray: Rect = { x: 0, y: win.h - TRAY_ROW_H, w: win.w, h: TRAY_ROW_H };
    expect(overlaps(toastRect("bottom-right", win, 0), tray)).toBe(true);
  });
});
