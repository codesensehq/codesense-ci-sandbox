// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render } from "@/test-utils";
import { useState } from "react";

// The hook reads the dragged pill from dnd-kit's context: stand it in with what a test drags.
const dnd = vi.hoisted(() => ({
  active: null as { id: string } | null,
  activatorEvent: null as Event | null,
}));
vi.mock("@dnd-kit/core", () => ({ useDndContext: () => dnd }));

import type { TrayWorkspaceEntry } from "../lib/group-workspaces-by-epic";
import { trayChipDragId } from "../lib/terminal-drag";
import { takeTrayReorderIntent, useTrayReorderInBand } from "../lib/tray-reorder-drag";
import { useTrayReorder } from "./use-tray-reorder";

const TEAM = "acme:default";
const IDS = ["a", "b", "c"];
const entries = IDS.map((id) => ({ workspace: { id } }) as unknown as TrayWorkspaceEntry);
// The strip: three 90px pills 10px apart, at y 600–628; the strip's box 0–600 × 598–630.
const TOP = 598;
const Y = TOP + 14;

let zoom = 1;
let frames: FrameRequestCallback[] = [];
const runFrame = () =>
  act(() => {
    const due = frames;
    frames = [];
    due.forEach((f) => f(0));
  });

const rect = (left: number, top: number, width: number, height: number) =>
  ({
    left,
    top,
    width,
    height,
    right: left + width,
    bottom: top + height,
    x: left,
    y: top,
    toJSON: () => ({}),
  }) as DOMRect;

function Strip({ base }: { base?: readonly string[] }) {
  const [area, setArea] = useState<HTMLDivElement | null>(null);
  const insertion = useTrayReorder(TEAM, area, entries, base);
  const inBand = useTrayReorderInBand();
  return (
    <div>
      <div ref={setArea} data-testid="strip">
        {IDS.map((id) => (
          <span key={id} data-tray-chip={id} />
        ))}
      </div>
      <output
        data-testid="line"
        data-x={insertion?.x ?? ""}
        data-home={insertion?.atHome ? "" : undefined}
      />
      {/* The team view's split ghost, hidden while the pointer is in the band. */}
      {!inBand && <div data-testid="ghost" />}
    </div>
  );
}

const original = HTMLElement.prototype.getBoundingClientRect;
beforeEach(() => {
  zoom = 1;
  frames = [];
  vi.spyOn(window, "requestAnimationFrame").mockImplementation((f) => {
    frames.push(f);
    return frames.length;
  });
  vi.spyOn(window, "cancelAnimationFrame").mockImplementation(() => {});
  HTMLElement.prototype.getBoundingClientRect = function (this: HTMLElement) {
    const chip = this.getAttribute("data-tray-chip");
    const strip = this.closest<HTMLElement>("[data-testid='strip']");
    const scroll = strip?.scrollLeft ?? 0;
    if (chip !== null) {
      return rect((10 + IDS.indexOf(chip) * 100 - scroll) * zoom, TOP + 2, 90 * zoom, 28);
    }
    if (this.dataset.testid === "strip") return rect(0, TOP, 600 * zoom, 32);
    const zone = this.getAttribute("data-test-rect");
    if (zone) {
      const [l, t, w, h] = zone.split(",").map(Number);
      return rect(l!, t!, w!, h!);
    }
    return rect(0, 0, 0, 0);
  };
  takeTrayReorderIntent();
});
afterEach(() => {
  HTMLElement.prototype.getBoundingClientRect = original;
  vi.restoreAllMocks();
  document.querySelectorAll("[data-test-rect]").forEach((el) => el.remove());
  dnd.active = null;
  dnd.activatorEvent = null;
  takeTrayReorderIntent();
});

/** Mount with `id` dragged from (x, y), the strip sized to scroll. */
function mount(id = trayChipDragId("c"), start = { x: 255, y: Y }, base?: readonly string[]) {
  dnd.active = { id };
  dnd.activatorEvent = new PointerEvent("pointerdown", { clientX: start.x, clientY: start.y });
  const view = render(<Strip base={base} />);
  const strip = view.getByTestId("strip");
  Object.defineProperty(strip, "offsetWidth", { configurable: true, value: 600 });
  Object.defineProperty(strip, "clientWidth", { configurable: true, value: 600 });
  Object.defineProperty(strip, "scrollWidth", { configurable: true, value: 900 });
  // The mount's own measure is dropped at the next frame, as it is in the app.
  runFrame();
  return { ...view, strip };
}

const move = (x: number, y = Y) =>
  act(() => {
    fireEvent.pointerMove(window, { clientX: x, clientY: y, pointerId: 1 });
  });
const intent = () => takeTrayReorderIntent();

describe("useTrayReorder", () => {
  it("in the band, a slot between pills is the reorder; out of it, nothing is", () => {
    const { getByTestId } = mount();
    move(100);
    expect(getByTestId("line").dataset.x).toBe("104");
    move(100, TOP + 32 + 40);
    expect(getByTestId("line").dataset.x).toBe("");
    move(100);
    expect(intent()).toEqual({ teamKey: TEAM, workspaceId: "c", place: { after: "a" } });
  });

  it("a tile's drop zone inside the band's slack is a split, never a reorder", () => {
    const zone = document.createElement("div");
    zone.setAttribute("data-drop-side", "top");
    zone.setAttribute("data-test-rect", `0,${TOP - 12},600,12`);
    document.body.appendChild(zone);
    mount();
    move(100, TOP - 6);
    expect(intent()).toBeNull();
  });

  it("a zoomed bar draws the line in the strip's own px", () => {
    zoom = 2;
    const { getByTestId } = mount(trayChipDragId("c"), { x: 510, y: Y });
    move(200);
    // The gap before b is at 105 unzoomed (210 on screen).
    expect(getByTestId("line").dataset.x).toBe("104");
  });

  it("a scrolled strip draws the line inside its scrolled content", () => {
    const { getByTestId, strip } = mount();
    strip.scrollLeft = 50;
    act(() => {
      fireEvent.scroll(strip);
    });
    // The pointer at 55 is past a's middle once a sits at -40..50.
    move(55);
    expect(getByTestId("line").dataset.x).toBe("104");
  });

  it("the release is read where it is let go, before the drop takes the intent", () => {
    mount();
    move(240);
    act(() => {
      fireEvent.pointerUp(window, { clientX: 20, clientY: Y, pointerId: 1 });
    });
    expect(intent()?.place).toEqual({ before: "a" });
  });

  it("a quick release with no move yet still reads the pointer from the drag's start", () => {
    mount(trayChipDragId("c"), { x: 255, y: Y });
    // At the pill's own place: held, so the release neither splits nor moves.
    expect(intent()).toEqual({ teamKey: TEAM, workspaceId: "c", place: null });
  });

  it("a bare tile id dragged into the band never reorders", () => {
    const { getByTestId } = mount("c");
    move(100);
    expect(getByTestId("line").dataset.x).toBe("");
    expect(intent()).toBeNull();
  });

  it("near an end of the strip, in the band, the strip scrolls; over a tile's zone it stays", () => {
    const { strip } = mount();
    move(590);
    runFrame();
    expect(strip.scrollLeft).toBeGreaterThan(0);
    // In the strip's row, but over a tile's top edge zone: a split, so no scroll under it.
    const zone = document.createElement("div");
    zone.setAttribute("data-drop-side", "top");
    zone.setAttribute("data-test-rect", `0,${TOP - 12},600,12`);
    document.body.appendChild(zone);
    runFrame();
    move(590, TOP - 6);
    strip.scrollLeft = 0;
    runFrame();
    runFrame();
    expect(strip.scrollLeft).toBe(0);
  });

  it("the team view's ghost is hidden while the pointer is in the band", () => {
    const { queryByTestId } = mount();
    move(100);
    expect(queryByTestId("ghost")).toBeNull();
    move(100, TOP + 32 + 40);
    expect(queryByTestId("ghost")).not.toBeNull();
  });

  it("the order the pills were drawn from travels with the intent", () => {
    mount(trayChipDragId("c"), { x: 255, y: Y }, ["a", "b", "c"]);
    move(100);
    expect(intent()?.base).toEqual(["a", "b", "c"]);
  });

  it("leaving the app mid-drag leaves no intent behind, and tracking resumes when it is back", () => {
    const { getByTestId } = mount();
    move(100);
    act(() => {
      fireEvent.blur(window);
    });
    expect(getByTestId("line").dataset.x).toBe("");
    // Nothing held while away: a release on a tile then splits, it is not eaten as a reorder.
    expect(takeTrayReorderIntent()).toBeNull();
    act(() => {
      fireEvent.focus(window);
    });
    expect(getByTestId("line").dataset.x).toBe("");
    move(100);
    expect(getByTestId("line").dataset.x).toBe("104");
    act(() => {
      fireEvent.pointerUp(window, { clientX: 20, clientY: Y, pointerId: 1 });
    });
    expect(intent()).toEqual({ teamKey: TEAM, workspaceId: "c", place: { before: "a" } });
  });

  it("a release that comes straight back after the app was left is read where it lands", () => {
    mount();
    move(100);
    act(() => {
      fireEvent.blur(window);
    });
    act(() => {
      fireEvent.pointerUp(window, { clientX: 20, clientY: Y, pointerId: 1 });
    });
    expect(intent()?.place).toEqual({ before: "a" });
  });

  it("the app-wide pointer listeners go when the last tray unmounts", () => {
    const remove = vi.spyOn(window, "removeEventListener");
    const { unmount } = mount();
    unmount();
    const removed = remove.mock.calls.filter(([type]) => String(type) === "pointerdown");
    expect(removed.length).toBe(1);
  });
});
