// SPDX-License-Identifier: MIT
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, screen } from "@testing-library/react";

import type { MosaicNode } from "../lib/mosaic-tree";
import { dragSeparator, installPanelGeometry } from "@/test/panel-geometry";
import { MosaicView } from "./mosaic-view";

// Seam: the real MosaicView on the real panel library, the test DOM given flex
// geometry — including what the browser reports for a `display:none` element
// (a zoom-collapsed tile, a hidden divider): a 0×0 box at the window origin.
//
// The library finds its resize targets from panel geometry on every document
// pointermove, not from the separators' own hit boxes. A zoomed mosaic keeps its
// collapsed tiles mounted but hidden, so they sort first and the "gap" between
// them and the zoomed tile reads as a separator spanning from the window edge to
// the tile — e.g. the strip at the top of a full-screen CLI — which showed the ↕
// cursor (a window-wide `* { cursor }` rule) and would start a drag there.

installPanelGeometry();

afterEach(() => {
  // The panel library keeps its pointer state module-wide: release and move
  // away so no test starts mid-hover or mid-drag.
  pointer("pointerup", -100, -100);
  pointer("pointermove", -100, -100);
  cleanup();
  for (const sheet of document.adoptedStyleSheets) {
    while (sheet.cssRules.length) sheet.deleteRule(0);
  }
});

const leaf = (id: string): MosaicNode => ({ kind: "leaf", workspaceId: id });

/** Three workspaces stacked top to bottom. */
const stacked: MosaicNode = {
  kind: "split",
  axis: "column",
  sizes: [30, 40, 30],
  children: [leaf("a"), leaf("b"), leaf("c")],
};

/** A row whose right column is itself a stack — a nested split to hide. */
const nested: MosaicNode = {
  kind: "split",
  axis: "row",
  sizes: [50, 50],
  children: [
    leaf("a"),
    { kind: "split", axis: "column", sizes: [50, 50], children: [leaf("b"), leaf("c")] },
  ],
};

function renderMosaic(node: MosaicNode, zoomPath: number[] | null) {
  const onSizesCommit = vi.fn();
  render(
    <MosaicView
      node={node}
      zoomPath={zoomPath}
      onSizesCommit={onSizesCommit}
      renderLeaf={(id) => <div data-testid={`leaf-${id}`} />}
    />,
  );
  return onSizesCommit;
}

/** The window-wide cursor the panel library forces while a divider is hovered. */
const forcedCursor = () =>
  document.adoptedStyleSheets
    .flatMap((sheet) => [...sheet.cssRules].map((rule) => rule.cssText))
    .find((text) => /cursor/.test(text));

const pointer = (type: string, x: number, y: number, buttons = 0) =>
  act(() => {
    document.body.dispatchEvent(
      new PointerEvent(type, {
        bubbles: true,
        clientX: x,
        clientY: y,
        buttons,
        button: 0,
        pointerId: 1,
        pointerType: "mouse",
      }),
    );
  });

const panelOf = (id: string) =>
  screen.getByTestId(`leaf-${id}`).closest<HTMLElement>("[data-panel]")!;
const growOf = (id: string) =>
  /flex-grow:\s*([\d.]+)/.exec(panelOf(id).getAttribute("style") ?? "")?.[1];

describe("a zoomed mosaic offers no resizing", () => {
  it("the top edge of a zoomed stack shows no resize cursor", () => {
    renderMosaic(stacked, [1]);
    pointer("pointermove", 600, 2);
    expect(forcedCursor()).toBeUndefined();
  });

  it("pressing and dragging there starts no resize", () => {
    const onSizesCommit = renderMosaic(stacked, [1]);
    const before = ["a", "b", "c"].map(growOf);
    pointer("pointermove", 600, 2);
    pointer("pointerdown", 600, 2, 1);
    pointer("pointermove", 600, 200, 1);
    // A drag in progress holds the window-wide resize cursor until release.
    expect(forcedCursor()).toBeUndefined();
    pointer("pointerup", 600, 200);
    expect(["a", "b", "c"].map(growOf)).toEqual(before);
    expect(onSizesCommit).not.toHaveBeenCalled();
  });

  it("a split hidden inside a collapsed tile offers nothing either", () => {
    renderMosaic(nested, [0]);
    for (const [x, y] of [
      [2, 2],
      [600, 2],
      [2, 400],
    ]) {
      pointer("pointermove", x, y);
      expect(forcedCursor()).toBeUndefined();
    }
  });

  it("un-zoomed, the dividers still resize", () => {
    const onSizesCommit = renderMosaic(stacked, null);
    const divider = panelOf("a").nextElementSibling as HTMLElement;
    const r = divider.getBoundingClientRect();
    pointer("pointermove", r.x + r.width / 2, r.y + r.height / 2);
    expect(forcedCursor()).toMatch(/resize/);
    dragSeparator(divider, 0, 80);
    expect(onSizesCommit).toHaveBeenCalledWith([], expect.any(Array));
  });
});
