// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import {
  foregroundTilesFor,
  hasForegroundTile,
  isSplitActive,
  SPLIT_MIN_TILES,
  epicForegroundTiles,
  scopedForegroundTiles,
} from "./foreground-tiles";
import type { Workspace } from "@/shared/stores/viewer-store";

const ws = (id: string): Workspace => ({ id, kind: "story", cwd: `/${id}`, status: "ready" });

describe("foregroundTilesFor", () => {
  it("returns workspaces sorted by id when the team has no order yet", () => {
    const out = foregroundTilesFor([ws("c"), ws("a"), ws("b")], []);
    expect(out.map((w) => w.id)).toEqual(["a", "b", "c"]);
  });

  it("follows the team's shared workspace order when given (the pills' order)", () => {
    const tiles = [ws("AID-2"), ws("AID-3"), ws("AID-4")];
    expect(foregroundTilesFor(tiles, ["AID-4", "AID-3", "AID-2"]).map((w) => w.id)).toEqual([
      "AID-4",
      "AID-3",
      "AID-2",
    ]);
    // Grouped mode keeps that order within the selected epic's slice.
    const rail = { "AID-2": "e", "AID-3": "e", "AID-4": "e" };
    expect(
      epicForegroundTiles(tiles, rail, "e", ["AID-4", "AID-3", "AID-2"]).map((w) => w.id),
    ).toEqual(["AID-4", "AID-3", "AID-2"]);
    // A tile the order doesn't know yet sits right after its id-order predecessor.
    expect(
      foregroundTilesFor([...tiles, ws("AID-5")], ["AID-4", "AID-3", "AID-2"]).map((w) => w.id),
    ).toEqual(["AID-4", "AID-5", "AID-3", "AID-2"]);
  });

  it("does not mutate the input", () => {
    const input = [ws("b"), ws("a")];
    foregroundTilesFor(input, []);
    expect(input.map((w) => w.id)).toEqual(["b", "a"]);
  });

  it("includes every kind of the 4-kind model (root/story/epic/generic)", () => {
    const root: Workspace = { id: "root:a/app", kind: "root", cwd: "/", status: "ready" };
    const epic: Workspace = { id: "epic-x", kind: "epic", cwd: "/x", status: "ready" };
    const generic: Workspace = {
      id: "generic:/ctx",
      kind: "generic",
      cwd: "/ctx",
      status: "ready",
    };
    const out = foregroundTilesFor([root, ws("a"), epic, generic, ws("b")], []);
    expect(out.map((w) => w.id)).toEqual(["a", "b", "epic-x", "generic:/ctx", "root:a/app"]);
  });
});

describe("hasForegroundTile (SW-FX-94)", () => {
  it("is false for an empty set", () => {
    expect(hasForegroundTile([])).toBe(false);
  });

  it("is true when at least one workspace tiles (matches foregroundTilesFor membership)", () => {
    expect(hasForegroundTile([ws("a")])).toBe(true);
    const many = [ws("a"), ws("b")];
    expect(hasForegroundTile(many)).toBe(foregroundTilesFor(many, []).length > 0);
  });
});

describe("epicForegroundTiles (SW-EW-40)", () => {
  // Rail-key index mirrors what `railKeyByWorkspaceId` holds: ws id → rail key
  // (ROOTS_RAIL_KEY " roots" for roots, epic dirName for epic-members, "" for
  // Ungrouped). The filter composes OVER foregroundTilesFor.
  const index: Record<string, string> = {
    "story-a": "epic-1",
    "story-b": "epic-1",
    "story-c": "epic-2",
    "root:acme/app": " roots",
    "generic:/scratch": "",
  };

  it("returns only the selected epic's tiles, still id-sorted", () => {
    const out = epicForegroundTiles(
      [ws("story-b"), ws("story-c"), ws("story-a")],
      index,
      "epic-1",
      [],
    );
    expect(out.map((w) => w.id)).toEqual(["story-a", "story-b"]);
  });

  it("partitions roots vs Ungrouped even though both are non-epic", () => {
    const all = [ws("root:acme/app"), ws("generic:/scratch"), ws("story-a")];
    expect(epicForegroundTiles(all, index, " roots", []).map((w) => w.id)).toEqual([
      "root:acme/app",
    ]);
    expect(epicForegroundTiles(all, index, "", []).map((w) => w.id)).toEqual(["generic:/scratch"]);
  });

  it("returns [] for an epic with no open members (empty-state input)", () => {
    expect(epicForegroundTiles([ws("story-a"), ws("story-b")], index, "epic-9", [])).toEqual([]);
  });

  it("parks a workspace absent from the index (no match, never a crash)", () => {
    const out = epicForegroundTiles([ws("story-a"), ws("unknown")], index, "epic-1", []);
    expect(out.map((w) => w.id)).toEqual(["story-a"]);
  });

  it("tolerates an undefined index (whole team pre-first-sync) → no tiles", () => {
    expect(epicForegroundTiles([ws("story-a")], undefined, "epic-1", [])).toEqual([]);
  });

  it("composes over foregroundTilesFor membership (does not re-include non-tiles)", () => {
    // Every current kind tiles, so membership identity holds: the epic filter is
    // a strict subset of foregroundTilesFor for the same inputs.
    const all = [ws("story-a"), ws("story-b"), ws("story-c")];
    const full = new Set(foregroundTilesFor(all, []).map((w) => w.id));
    for (const w of epicForegroundTiles(all, index, "epic-1", []))
      expect(full.has(w.id)).toBe(true);
  });
});

describe("scopedForegroundTiles (SW-EW-40 mode-aware membership + cold-start fallback)", () => {
  const index: Record<string, string> = { "story-a": "epic-1", "story-b": "epic-2" };

  it("flat mode → the full set (index ignored)", () => {
    const all = [ws("story-a"), ws("story-b")];
    expect(scopedForegroundTiles(all, false, index, "epic-1", []).map((w) => w.id)).toEqual([
      "story-a",
      "story-b",
    ]);
  });

  it("grouped WITH a built index → the selected epic's slice", () => {
    const all = [ws("story-a"), ws("story-b")];
    expect(scopedForegroundTiles(all, true, index, "epic-1", []).map((w) => w.id)).toEqual([
      "story-a",
    ]);
  });

  it("grouped but index NOT yet built (undefined) → full set (cold-start: show terminals, don't flash empty)", () => {
    const all = [ws("story-a"), ws("story-b")];
    expect(scopedForegroundTiles(all, true, undefined, "epic-1", []).map((w) => w.id)).toEqual([
      "story-a",
      "story-b",
    ]);
  });

  it("grouped with a BUILT index and a genuinely empty epic → [] (fallback does NOT mask a real empty epic)", () => {
    const all = [ws("story-a"), ws("story-b")];
    expect(scopedForegroundTiles(all, true, index, "epic-none", [])).toEqual([]);
  });
});

describe("isSplitActive", () => {
  it("is false for 0 or 1 tiles (1-up, no regression)", () => {
    expect(isSplitActive(0)).toBe(false);
    expect(isSplitActive(1)).toBe(false);
  });

  it("is true at the split threshold and above", () => {
    expect(isSplitActive(SPLIT_MIN_TILES)).toBe(true);
    expect(isSplitActive(5)).toBe(true);
  });
});
