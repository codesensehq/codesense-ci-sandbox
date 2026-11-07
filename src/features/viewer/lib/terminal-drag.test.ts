// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import {
  TERMINAL_GRID_DROPPABLE_ID,
  TILE_EDGE_SIDES,
  tileEdgePlacement,
  dragEndToReopen,
  tileZoneId,
  parseTileZoneId,
  resolveTilePlacement,
  trayChipDragId,
  unwrapDragWorkspaceId,
  isTrayChipDragId,
} from "./terminal-drag";

describe("dragEndToReopen (PT-13 AC-2)", () => {
  it("returns the workspace id when dropped on the terminal grid", () => {
    expect(dragEndToReopen("30-10", TERMINAL_GRID_DROPPABLE_ID)).toBe("30-10");
  });

  it("coerces a non-string active id", () => {
    expect(dragEndToReopen(42, TERMINAL_GRID_DROPPABLE_ID)).toBe("42");
  });

  it("returns null when dropped anywhere but the grid", () => {
    expect(dragEndToReopen("30-10", "some-other-droppable")).toBeNull();
    expect(dragEndToReopen("30-10", null)).toBeNull();
    expect(dragEndToReopen("30-10", undefined)).toBeNull();
  });

  it("returns null when there is no active draggable", () => {
    expect(dragEndToReopen(null, TERMINAL_GRID_DROPPABLE_ID)).toBeNull();
    expect(dragEndToReopen("", TERMINAL_GRID_DROPPABLE_ID)).toBeNull();
  });
});

describe("tileZoneId / parseTileZoneId (SW-EW-17)", () => {
  it("round-trips a zone id", () => {
    expect(tileZoneId("30-10", "left")).toBe("tile:30-10:left");
    expect(parseTileZoneId("tile:30-10:left")).toEqual({ workspaceId: "30-10", side: "left" });
  });
  it("tolerates ids containing dashes (the last colon delimits the side)", () => {
    expect(parseTileZoneId("tile:epic-3-story-7:bottom")).toEqual({
      workspaceId: "epic-3-story-7",
      side: "bottom",
    });
  });
  it("returns null for non-tile ids", () => {
    expect(parseTileZoneId("terminal-grid")).toBeNull();
    expect(parseTileZoneId("tile:onlyone")).toBeNull();
    expect(parseTileZoneId("tile:ws:nonsense")).toBeNull();
    expect(parseTileZoneId(null)).toBeNull();
  });
});

describe("tileEdgePlacement (SW-EW-33 — single source for side→split)", () => {
  it("maps each edge to its axis + before/after", () => {
    expect(tileEdgePlacement("left")).toEqual({ axis: "row", side: "before" });
    expect(tileEdgePlacement("right")).toEqual({ axis: "row", side: "after" });
    expect(tileEdgePlacement("top")).toEqual({ axis: "column", side: "before" });
    expect(tileEdgePlacement("bottom")).toEqual({ axis: "column", side: "after" });
  });
  it("TILE_EDGE_SIDES is the four edges (no center)", () => {
    expect(TILE_EDGE_SIDES).toEqual(["left", "right", "top", "bottom"]);
  });
  it("agrees with resolveTilePlacement for every edge (the shared mapping)", () => {
    for (const side of TILE_EDGE_SIDES) {
      const resolved = resolveTilePlacement("a", tileZoneId("b", side));
      expect(resolved).toEqual({
        workspaceId: "a",
        target: { targetWorkspaceId: "b", ...tileEdgePlacement(side) },
      });
    }
  });
});

describe("resolveTilePlacement (SW-EW-17 AC-1)", () => {
  it("left edge → vertical split (row axis), before", () => {
    expect(resolveTilePlacement("a", tileZoneId("b", "left"))).toEqual({
      workspaceId: "a",
      target: { targetWorkspaceId: "b", axis: "row", side: "before" },
    });
  });
  it("right edge → row axis, after", () => {
    expect(resolveTilePlacement("a", tileZoneId("b", "right"))).toEqual({
      workspaceId: "a",
      target: { targetWorkspaceId: "b", axis: "row", side: "after" },
    });
  });
  it("top edge → horizontal split (column axis), before", () => {
    expect(resolveTilePlacement("a", tileZoneId("b", "top"))).toEqual({
      workspaceId: "a",
      target: { targetWorkspaceId: "b", axis: "column", side: "before" },
    });
  });
  it("bottom edge → column axis, after", () => {
    expect(resolveTilePlacement("a", tileZoneId("b", "bottom"))).toEqual({
      workspaceId: "a",
      target: { targetWorkspaceId: "b", axis: "column", side: "after" },
    });
  });
  it("center → focus-only target (defensive: SW-EW-33 no longer RENDERS a center drop zone)", () => {
    // The resolver keeps this branch as a safe fallback, but the tile UI renders
    // only the 4 edge zones now — a center release isn't a drop target, it snaps
    // back (you can't drop a workspace into another). So this id is never
    // produced in practice; the pure-fn contract is just preserved.
    expect(resolveTilePlacement("a", tileZoneId("b", "center"))).toEqual({
      workspaceId: "a",
      target: { center: true },
    });
  });
  it("coerces a numeric active id", () => {
    expect(resolveTilePlacement(42, tileZoneId("b", "right"))).toMatchObject({ workspaceId: "42" });
  });
  it("returns null for the legacy grid droppable or empty active", () => {
    expect(resolveTilePlacement("a", TERMINAL_GRID_DROPPABLE_ID)).toBeNull();
    expect(resolveTilePlacement("", tileZoneId("b", "left"))).toBeNull();
    expect(resolveTilePlacement(null, tileZoneId("b", "left"))).toBeNull();
  });
});

describe("tray chip drag id namespacing (SW-EW-33)", () => {
  it("round-trips a workspace id through the tray namespace", () => {
    expect(trayChipDragId("e-1-a")).toBe("tray:e-1-a");
    expect(unwrapDragWorkspaceId(trayChipDragId("e-1-a"))).toBe("e-1-a");
  });
  it("passes a bare (in-mosaic tile) id through unchanged", () => {
    expect(unwrapDragWorkspaceId("e-1-a")).toBe("e-1-a");
  });
  it("workspace ids containing colons survive the unwrap", () => {
    expect(unwrapDragWorkspaceId(trayChipDragId("acme:default:e-1"))).toBe("acme:default:e-1");
  });
  it("resolveTilePlacement unwraps a tray-namespaced active id", () => {
    expect(resolveTilePlacement(trayChipDragId("m-1"), tileZoneId("a", "right"))).toEqual({
      workspaceId: "m-1",
      target: { targetWorkspaceId: "a", axis: "row", side: "after" },
    });
  });
  it("dragEndToReopen unwraps a tray-namespaced active id on the grid", () => {
    expect(dragEndToReopen(trayChipDragId("m-1"), TERMINAL_GRID_DROPPABLE_ID)).toBe("m-1");
  });
});

describe("isTrayChipDragId", () => {
  it("tells a tray chip's drag id from a tile's", () => {
    expect(isTrayChipDragId(trayChipDragId("e-1-a"))).toBe(true);
    expect(isTrayChipDragId("e-1-a")).toBe(false);
    expect(isTrayChipDragId(null)).toBe(false);
    expect(isTrayChipDragId(7)).toBe(false);
  });
});
