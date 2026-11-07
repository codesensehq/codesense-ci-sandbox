// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import {
  type MosaicNode,
  seedTreeFromFlat,
  setNodeSizes,
  leafWorkspaceIds,
  findLeafPath,
  splitAt,
  moveLeaf,
  pruneLeaf,
  reconcileMosaic,
  appendLeaf,
  buildBalancedGrid,
  buildPresetTree,
  clampedGridShape,
  reconcilePresetTree,
  parseMosaicNode,
  dedupeLeaves,
  replaceLeafWorkspaceId,
  sameMosaicStructure,
  placementChangesLayout,
} from "./mosaic-tree";

const leaf = (id: string): MosaicNode => ({ kind: "leaf", workspaceId: id });

describe("seedTreeFromFlat", () => {
  it("0 ids → null", () => {
    expect(seedTreeFromFlat([], "row")).toBeNull();
  });
  it("1 id → single leaf", () => {
    expect(seedTreeFromFlat(["a"], "row")).toEqual(leaf("a"));
  });
  it("≥2 ids → one depth-1 split on the given axis, even sizes", () => {
    expect(seedTreeFromFlat(["a", "b"], "row")).toEqual({
      kind: "split",
      axis: "row",
      children: [leaf("a"), leaf("b")],
      sizes: [50, 50],
    });
    const col = seedTreeFromFlat(["a", "b", "c"], "column");
    expect(col).toMatchObject({ kind: "split", axis: "column" });
    expect((col as Extract<MosaicNode, { kind: "split" }>).sizes).toEqual([
      100 / 3,
      100 / 3,
      100 / 3,
    ]);
  });
  it("honors matching persisted sizes; ignores a stale-length array", () => {
    expect(
      (seedTreeFromFlat(["a", "b"], "row", [70, 30]) as Extract<MosaicNode, { kind: "split" }>)
        .sizes,
    ).toEqual([70, 30]);
    // length mismatch → even fallback
    expect(
      (seedTreeFromFlat(["a", "b"], "row", [50]) as Extract<MosaicNode, { kind: "split" }>).sizes,
    ).toEqual([50, 50]);
  });
});

describe("setNodeSizes", () => {
  it("sets root-level sizes (path [])", () => {
    const row = seedTreeFromFlat(["a", "b"], "row")!;
    expect(
      (setNodeSizes(row, [], [70, 30]) as Extract<MosaicNode, { kind: "split" }>).sizes,
    ).toEqual([70, 30]);
  });
  it("sets sizes on a nested node", () => {
    const tree: MosaicNode = {
      kind: "split",
      axis: "column",
      children: [
        { kind: "split", axis: "row", children: [leaf("a"), leaf("b")], sizes: [50, 50] },
        leaf("c"),
      ],
      sizes: [60, 40],
    };
    const out = setNodeSizes(tree, [0], [80, 20]) as Extract<MosaicNode, { kind: "split" }>;
    expect((out.children[0] as Extract<MosaicNode, { kind: "split" }>).sizes).toEqual([80, 20]);
    expect(out.sizes).toEqual([60, 40]); // root untouched
  });
  it("no-ops on a length mismatch or a leaf", () => {
    const row = seedTreeFromFlat(["a", "b"], "row")!;
    expect(setNodeSizes(row, [], [100])).toEqual(row);
    expect(setNodeSizes(leaf("a"), [], [100])).toEqual(leaf("a"));
  });
});

describe("leafWorkspaceIds + findLeafPath", () => {
  const tree: MosaicNode = {
    kind: "split",
    axis: "column",
    children: [
      { kind: "split", axis: "row", children: [leaf("a"), leaf("b")], sizes: [50, 50] },
      leaf("c"),
    ],
    sizes: [60, 40],
  };
  it("lists leaves in order", () => {
    expect(leafWorkspaceIds(tree)).toEqual(["a", "b", "c"]);
  });
  it("finds index path to a leaf", () => {
    expect(findLeafPath(tree, "a")).toEqual([0, 0]);
    expect(findLeafPath(tree, "b")).toEqual([0, 1]);
    expect(findLeafPath(tree, "c")).toEqual([1]);
    expect(findLeafPath(tree, "zzz")).toBeNull();
  });
  it("a single leaf root has the empty path", () => {
    expect(findLeafPath(leaf("a"), "a")).toEqual([]);
  });
});

describe("splitAt — cross-axis", () => {
  it("splitting a single-leaf root wraps it in a 2-child split", () => {
    expect(splitAt(leaf("a"), "a", "row", "after", "b")).toEqual({
      kind: "split",
      axis: "row",
      children: [leaf("a"), leaf("b")],
      sizes: [50, 50],
    });
  });
  it("side before puts the new leaf first", () => {
    expect(splitAt(leaf("a"), "a", "column", "before", "b")).toEqual({
      kind: "split",
      axis: "column",
      children: [leaf("b"), leaf("a")],
      sizes: [50, 50],
    });
  });
  it("dropping on a different axis than the parent wraps the target in a nested split", () => {
    const row = seedTreeFromFlat(["a", "b"], "row")!; // row split [a,b]
    // drop on TOP of b → vertical (column) split wrapping b
    const out = splitAt(row, "b", "column", "before", "c") as Extract<
      MosaicNode,
      { kind: "split" }
    >;
    expect(out.axis).toBe("row");
    expect(out.children[0]).toEqual(leaf("a"));
    expect(out.children[1]).toEqual({
      kind: "split",
      axis: "column",
      children: [leaf("c"), leaf("b")],
      sizes: [50, 50],
    });
  });
});

describe("splitAt — same-axis flatten", () => {
  it("dropping on the same axis as the parent inserts a sibling (no nesting), splitting the target's slot", () => {
    const row = seedTreeFromFlat(["a", "b"], "row")!; // sizes [50,50]
    // drop to the RIGHT of a → insert c after a, same row
    const out = splitAt(row, "a", "row", "after", "c") as Extract<MosaicNode, { kind: "split" }>;
    expect(out.axis).toBe("row");
    expect(out.children).toEqual([leaf("a"), leaf("c"), leaf("b")]);
    // a's 50 slot is halved between a and c; b untouched
    expect(out.sizes).toEqual([25, 25, 50]);
  });
  it("side before inserts ahead of the target", () => {
    const row = seedTreeFromFlat(["a", "b"], "row")!;
    const out = splitAt(row, "b", "row", "before", "c") as Extract<MosaicNode, { kind: "split" }>;
    expect(out.children).toEqual([leaf("a"), leaf("c"), leaf("b")]);
    expect(out.sizes).toEqual([50, 25, 25]);
  });
});

describe("pruneLeaf", () => {
  it("removing one of three keeps a split of two and renormalizes sizes to 100", () => {
    const row: MosaicNode = {
      kind: "split",
      axis: "row",
      children: [leaf("a"), leaf("b"), leaf("c")],
      sizes: [20, 30, 50],
    };
    const out = pruneLeaf(row, "b") as Extract<MosaicNode, { kind: "split" }>;
    expect(out.children).toEqual([leaf("a"), leaf("c")]);
    expect(out.sizes[0] + out.sizes[1]).toBeCloseTo(100);
    expect(out.sizes).toEqual([(20 / 70) * 100, (50 / 70) * 100]);
  });
  it("removing down to one child collapses the split to that child", () => {
    const row = seedTreeFromFlat(["a", "b"], "row")!;
    expect(pruneLeaf(row, "a")).toEqual(leaf("b"));
  });
  it("removing the last leaf yields null", () => {
    expect(pruneLeaf(leaf("a"), "a")).toBeNull();
  });
  it("collapses nested single-child splits recursively", () => {
    const tree: MosaicNode = {
      kind: "split",
      axis: "column",
      children: [
        { kind: "split", axis: "row", children: [leaf("a"), leaf("b")], sizes: [50, 50] },
        leaf("c"),
      ],
      sizes: [60, 40],
    };
    // remove b → inner row collapses to leaf a → outer column [a, c]
    const out = pruneLeaf(tree, "b") as Extract<MosaicNode, { kind: "split" }>;
    expect(out.axis).toBe("column");
    expect(out.children).toEqual([leaf("a"), leaf("c")]);
  });
  it("pruning an absent id is a no-op", () => {
    const row = seedTreeFromFlat(["a", "b"], "row")!;
    expect(pruneLeaf(row, "zzz")).toEqual(row);
  });
});

describe("moveLeaf", () => {
  it("moves an on-screen tile to a new edge (prune then split)", () => {
    const row = seedTreeFromFlat(["a", "b", "c"], "row")!;
    // move c to the TOP of a → c leaves the row, wraps a in a column split
    const out = moveLeaf(row, "c", {
      targetWorkspaceId: "a",
      axis: "column",
      side: "before",
    }) as Extract<MosaicNode, { kind: "split" }>;
    expect(leafWorkspaceIds(out).sort()).toEqual(["a", "b", "c"]);
    expect(out.axis).toBe("row");
    expect(out.children[0]).toEqual({
      kind: "split",
      axis: "column",
      children: [leaf("c"), leaf("a")],
      sizes: [50, 50],
    });
    expect(out.children[1]).toEqual(leaf("b"));
  });
  it("moving a leaf onto itself is a no-op", () => {
    const row = seedTreeFromFlat(["a", "b"], "row")!;
    expect(moveLeaf(row, "a", { targetWorkspaceId: "a", axis: "row", side: "after" })).toEqual(row);
  });
});

describe("appendLeaf", () => {
  it("null root → leaf", () => {
    expect(appendLeaf(null, "a")).toEqual(leaf("a"));
  });
  it("leaf root → row split of [existing, new]", () => {
    expect(appendLeaf(leaf("a"), "b")).toEqual(seedTreeFromFlat(["a", "b"], "row"));
  });
  it("split root → pushes a child and renormalizes evenly", () => {
    const row = seedTreeFromFlat(["a", "b"], "row")!;
    const out = appendLeaf(row, "c") as Extract<MosaicNode, { kind: "split" }>;
    expect(out.children).toEqual([leaf("a"), leaf("b"), leaf("c")]);
    expect(out.sizes).toEqual([100 / 3, 100 / 3, 100 / 3]);
  });
});

describe("reconcileMosaic", () => {
  it("prunes leaves whose workspace is gone and collapses", () => {
    const row = seedTreeFromFlat(["a", "b"], "row")!;
    expect(reconcileMosaic(row, ["a"])).toEqual(leaf("a"));
  });
  it("appends live workspaces missing from the tree in the given order", () => {
    const out = reconcileMosaic(leaf("a"), ["a", "b", "c"]) as Extract<
      MosaicNode,
      { kind: "split" }
    >;
    expect(leafWorkspaceIds(out)).toEqual(["a", "b", "c"]);
  });
  it("null tree + live ids → seeded by append", () => {
    expect(leafWorkspaceIds(reconcileMosaic(null, ["a", "b"]) ?? leaf("x"))).toEqual(["a", "b"]);
  });
  it("empty live set → null", () => {
    expect(reconcileMosaic(seedTreeFromFlat(["a", "b"], "row")!, [])).toBeNull();
  });
  it("stable when tree already matches the live set", () => {
    const row = seedTreeFromFlat(["a", "b"], "row")!;
    expect(reconcileMosaic(row, ["a", "b"])).toEqual(row);
  });
});

describe("dedupeLeaves", () => {
  it("removes a duplicated workspace id, keeping one occurrence", () => {
    // A corrupt persisted root with `a` in two leaves.
    const corrupt: MosaicNode = {
      kind: "split",
      axis: "row",
      children: [leaf("a"), leaf("b"), leaf("a")],
      sizes: [33, 34, 33],
    };
    const out = dedupeLeaves(corrupt)!;
    expect(leafWorkspaceIds(out).sort()).toEqual(["a", "b"]);
  });
  it("collapses when dedupe leaves a single child", () => {
    const corrupt: MosaicNode = {
      kind: "split",
      axis: "row",
      children: [leaf("a"), leaf("a")],
      sizes: [50, 50],
    };
    expect(dedupeLeaves(corrupt)).toEqual(leaf("a"));
  });
  it("passes through a clean tree and null", () => {
    const clean = seedTreeFromFlat(["a", "b"], "row")!;
    expect(dedupeLeaves(clean)).toEqual(clean);
    expect(dedupeLeaves(null)).toBeNull();
  });
});

describe("reconcileMosaic — dedupe", () => {
  it("de-duplicates a corrupt persisted root against the live set", () => {
    const corrupt: MosaicNode = {
      kind: "split",
      axis: "row",
      children: [leaf("a"), leaf("b"), leaf("a")],
      sizes: [33, 34, 33],
    };
    expect(leafWorkspaceIds(reconcileMosaic(corrupt, ["a", "b"])).sort()).toEqual(["a", "b"]);
  });
});

describe("buildBalancedGrid", () => {
  it("N=1 → leaf", () => {
    expect(buildBalancedGrid(["a"])).toEqual(leaf("a"));
  });
  it("N=2 → a single row of two (degenerate outer collapses)", () => {
    expect(buildBalancedGrid(["a", "b"])).toEqual(seedTreeFromFlat(["a", "b"], "row"));
  });
  it("N=4 → 2×2 (a column of two rows)", () => {
    const out = buildBalancedGrid(["a", "b", "c", "d"]) as Extract<MosaicNode, { kind: "split" }>;
    expect(out.axis).toBe("column");
    expect(out.children).toHaveLength(2);
    expect(leafWorkspaceIds(out)).toEqual(["a", "b", "c", "d"]);
    expect(out.children.every((c) => c.kind === "split")).toBe(true);
  });
  it("N=3 → first row of two, second a lone leaf", () => {
    const out = buildBalancedGrid(["a", "b", "c"]) as Extract<MosaicNode, { kind: "split" }>;
    expect(out.axis).toBe("column");
    expect(out.children[0]).toMatchObject({ kind: "split", axis: "row" });
    expect(out.children[1]).toEqual(leaf("c"));
  });
  it("N=0 → null", () => {
    expect(buildBalancedGrid([])).toBeNull();
  });
});

describe("clampedGridShape — the readable floor wraps instead of shrinking", () => {
  it("columns preset WRAPS to rows when the column budget is exceeded", () => {
    // 7 cells, only 3 columns fit → 3 cols × ⌈7/3⌉=3 rows (never 7 slivers in one row)
    expect(clampedGridShape("columns", 7, 3, Infinity)).toEqual({ cols: 3, rows: 3 });
    // uncapped → the raw single row
    expect(clampedGridShape("columns", 7, Infinity, Infinity)).toEqual({ cols: 7, rows: 1 });
  });

  it("rows preset WRAPS to columns when the row budget is exceeded", () => {
    expect(clampedGridShape("rows", 7, Infinity, 3)).toEqual({ cols: 3, rows: 3 });
  });

  it("grid preset caps its balanced columns to the budget", () => {
    // 9 → balanced 3×3; a 2-col budget → 2 cols × ⌈9/2⌉=5 rows
    expect(clampedGridShape("grid", 9, 2, Infinity)).toEqual({ cols: 2, rows: 5 });
  });

  it("never returns a zero/sub-1 track even with a zero budget", () => {
    expect(clampedGridShape("columns", 4, 0, 0)).toEqual({ cols: 1, rows: 4 });
  });
});

describe("buildPresetTree", () => {
  const ids = ["a", "b", "c", "d", "e"];
  const split = (n: MosaicNode | null) => n as Extract<MosaicNode, { kind: "split" }>;

  it("0 ids → null, 1 → a leaf", () => {
    expect(buildPresetTree("grid", [])).toBeNull();
    expect(buildPresetTree("columns", ["a"])).toEqual(leaf("a"));
  });

  it("columns → one even row; rows → one even column", () => {
    expect(buildPresetTree("columns", ids)).toEqual(seedTreeFromFlat(ids, "row"));
    expect(buildPresetTree("rows", ids)).toEqual(seedTreeFromFlat(ids, "column"));
  });

  it("grid → a column of full rows, the short last row spanning the width", () => {
    const out = split(buildPresetTree("grid", ids));
    expect(out.axis).toBe("column");
    expect(out.sizes).toEqual([50, 50]);
    expect(out.children[0]).toEqual(seedTreeFromFlat(["a", "b", "c"], "row"));
    expect(out.children[1]).toEqual(seedTreeFromFlat(["d", "e"], "row"));
  });

  it("a last row of one is a leaf filling the row", () => {
    const out = split(buildPresetTree("grid", ["a", "b", "c"]));
    expect(out.children[1]).toEqual(leaf("c"));
  });

  it("the floor wraps columns into rows, cells in order", () => {
    const out = split(buildPresetTree("columns", ids, 3, Infinity));
    expect(out.axis).toBe("column");
    expect(out.children.map(leafWorkspaceIds)).toEqual([
      ["a", "b", "c"],
      ["d", "e"],
    ]);
  });

  it("the floor wraps rows into columns of rows", () => {
    const out = split(buildPresetTree("rows", ids, Infinity, 2));
    // ⌈5/2⌉ = 3 columns → rows of three, filled in order
    expect(out.children.map(leafWorkspaceIds)).toEqual([
      ["a", "b", "c"],
      ["d", "e"],
    ]);
  });

  it("buildBalancedGrid is the grid preset", () => {
    expect(buildBalancedGrid(ids)).toEqual(buildPresetTree("grid", ids));
  });
});

describe("reconcilePresetTree — saved sizes kept only while the shape holds", () => {
  const resized = (preset: "columns" | "rows" | "grid", ids: string[]) => {
    const even = buildPresetTree(preset, ids)!;
    return setNodeSizes(even, [], even.kind === "split" ? even.children.map((_, i) => i + 1) : []);
  };

  it("nothing saved → the even preset tree", () => {
    expect(reconcilePresetTree(undefined, "grid", ["a", "b", "c"])).toEqual(
      buildPresetTree("grid", ["a", "b", "c"]),
    );
  });

  it("the same preset and cells → the saved sizes", () => {
    const root = resized("columns", ["a", "b", "c"]);
    expect(reconcilePresetTree({ preset: "columns", root }, "columns", ["a", "b", "c"])).toEqual(
      root,
    );
  });

  it("a different preset voids the saved sizes", () => {
    const root = resized("columns", ["a", "b"]);
    expect(reconcilePresetTree({ preset: "columns", root }, "rows", ["a", "b"])).toEqual(
      buildPresetTree("rows", ["a", "b"]),
    );
  });

  it("closing a cell keeps the others' proportions", () => {
    const root = resized("columns", ["a", "b", "c"]); // 1:2:3
    const out = reconcilePresetTree({ preset: "columns", root }, "columns", ["b", "c"]);
    expect(out).toMatchObject({ kind: "split", axis: "row" });
    const sizes = (out as Extract<MosaicNode, { kind: "split" }>).sizes;
    expect(sizes[0]).toBeCloseTo(40);
    expect(sizes[1]).toBeCloseTo(60);
  });

  it("closing a cell of a grid keeps the rows' sizes when the shape still matches", () => {
    const root = resized("grid", ["a", "b", "c", "d"]); // rows 1:2
    const out = reconcilePresetTree({ preset: "grid", root }, "grid", ["a", "b", "c"]);
    expect(sameMosaicStructure(out, buildPresetTree("grid", ["a", "b", "c"]))).toBe(true);
    expect((out as Extract<MosaicNode, { kind: "split" }>).sizes[0]).toBeCloseTo(100 / 3);
  });

  it("a new cell, a reorder or a floor-forced reshape lays out evenly", () => {
    const root = resized("columns", ["a", "b"]);
    const saved = { preset: "columns" as const, root };
    expect(reconcilePresetTree(saved, "columns", ["a", "b", "c"])).toEqual(
      buildPresetTree("columns", ["a", "b", "c"]),
    );
    expect(reconcilePresetTree(saved, "columns", ["b", "a"])).toEqual(
      buildPresetTree("columns", ["b", "a"]),
    );
    const wide = resized("columns", ["a", "b", "c", "d"]);
    expect(
      reconcilePresetTree({ preset: "columns", root: wide }, "columns", ["a", "b", "c", "d"], 2),
    ).toEqual(buildPresetTree("columns", ["a", "b", "c", "d"], 2));
  });
});

describe("parseMosaicNode", () => {
  it("reads back a saved tree, normalizing its sizes", () => {
    const tree = { kind: "split", axis: "row", children: [leaf("a"), leaf("b")], sizes: [1, 3] };
    expect(parseMosaicNode(JSON.parse(JSON.stringify(tree)))).toEqual({ ...tree, sizes: [25, 75] });
  });

  it("refuses what isn't a tree", () => {
    expect(parseMosaicNode(null)).toBeNull();
    expect(parseMosaicNode({ kind: "leaf" })).toBeNull();
    expect(
      parseMosaicNode({ kind: "split", axis: "row", children: [leaf("a")], sizes: [100] }),
    ).toBeNull();
    expect(
      parseMosaicNode({
        kind: "split",
        axis: "diag",
        children: [leaf("a"), leaf("b")],
        sizes: [50, 50],
      }),
    ).toBeNull();
    expect(
      parseMosaicNode({
        kind: "split",
        axis: "row",
        children: [leaf("a"), leaf("b")],
        sizes: [50],
      }),
    ).toBeNull();
  });
});

describe("replaceLeafWorkspaceId (SW-EW-33 click-to-replace)", () => {
  it("single-leaf root → swaps the occupant, stays a leaf", () => {
    expect(replaceLeafWorkspaceId(leaf("a"), "a", "b")).toEqual(leaf("b"));
  });
  it("swaps a leaf inside a split IN PLACE — position, axis, sizes all unchanged", () => {
    const root = seedTreeFromFlat(["a", "b", "c"], "row")!;
    const out = replaceLeafWorkspaceId(root, "b", "z") as Extract<MosaicNode, { kind: "split" }>;
    expect(leafWorkspaceIds(out)).toEqual(["a", "z", "c"]); // b→z, same slot
    expect(out.axis).toBe("row");
    expect(out.sizes).toEqual([100 / 3, 100 / 3, 100 / 3]);
  });
  it("preserves a nested split's shape (only the target leaf changes)", () => {
    const root = splitAt(seedTreeFromFlat(["a", "b"], "row")!, "b", "column", "after", "c");
    const before = JSON.stringify(findLeafPath(root, "b"));
    const out = replaceLeafWorkspaceId(root, "b", "z");
    expect(leafWorkspaceIds(out)).toEqual(["a", "z", "c"]);
    expect(JSON.stringify(findLeafPath(out, "z"))).toEqual(before); // z occupies b's exact path
  });
  it("absent old id → tree returned unchanged", () => {
    const root = seedTreeFromFlat(["a", "b"], "row")!;
    expect(replaceLeafWorkspaceId(root, "missing", "z")).toEqual(root);
  });
  it("null root → null", () => {
    expect(replaceLeafWorkspaceId(null, "a", "b")).toBeNull();
  });
});

describe("sameMosaicStructure (SW-EW-33 — ignores sizes)", () => {
  it("equal structure with different sizes is the same", () => {
    const a = {
      kind: "split",
      axis: "row",
      children: [leaf("x"), leaf("y")],
      sizes: [30, 70],
    } as MosaicNode;
    const b = {
      kind: "split",
      axis: "row",
      children: [leaf("x"), leaf("y")],
      sizes: [50, 50],
    } as MosaicNode;
    expect(sameMosaicStructure(a, b)).toBe(true);
  });
  it("different leaf order is NOT the same", () => {
    const a = seedTreeFromFlat(["x", "y"], "row");
    const b = seedTreeFromFlat(["y", "x"], "row");
    expect(sameMosaicStructure(a, b)).toBe(false);
  });
  it("different axis is NOT the same", () => {
    expect(
      sameMosaicStructure(
        seedTreeFromFlat(["x", "y"], "row"),
        seedTreeFromFlat(["x", "y"], "column"),
      ),
    ).toBe(false);
  });
  it("null handling", () => {
    expect(sameMosaicStructure(null, null)).toBe(true);
    expect(sameMosaicStructure(null, leaf("x"))).toBe(false);
  });
});

describe("placementChangesLayout (SW-EW-33 — drop-zone no-op detection)", () => {
  // [A | B] (A left, B right).
  const AB = seedTreeFromFlat(["A", "B"], "row")!;

  it("dropping B onto A's RIGHT (where B already is) → no change", () => {
    expect(
      placementChangesLayout(AB, "B", { targetWorkspaceId: "A", axis: "row", side: "after" }),
    ).toBe(false);
  });
  it("dropping B onto A's LEFT → change (B moves to the left)", () => {
    expect(
      placementChangesLayout(AB, "B", { targetWorkspaceId: "A", axis: "row", side: "before" }),
    ).toBe(true);
  });
  it("dropping B onto A's TOP/BOTTOM → change (side-by-side → stacked)", () => {
    expect(
      placementChangesLayout(AB, "B", { targetWorkspaceId: "A", axis: "column", side: "before" }),
    ).toBe(true);
    expect(
      placementChangesLayout(AB, "B", { targetWorkspaceId: "A", axis: "column", side: "after" }),
    ).toBe(true);
  });
  it("dropping A onto B's LEFT (where A already is) → no change", () => {
    expect(
      placementChangesLayout(AB, "A", { targetWorkspaceId: "B", axis: "row", side: "before" }),
    ).toBe(false);
  });
  it("a not-in-tree (minimized) workspace always changes the layout (it gets added)", () => {
    expect(
      placementChangesLayout(AB, "M", { targetWorkspaceId: "A", axis: "row", side: "after" }),
    ).toBe(true);
  });
  it("empty tree → any placement changes it", () => {
    expect(
      placementChangesLayout(null, "A", { targetWorkspaceId: "X", axis: "row", side: "after" }),
    ).toBe(true);
  });
});

describe("placementChangesLayout — multiple rows / nested grid (SW-EW-33)", () => {
  // A 2×2 grid: two stacked rows.
  //   column [ row[A, B] , row[C, D] ]
  //   ┌───┬───┐
  //   │ A │ B │   (top row)
  //   ├───┼───┤
  //   │ C │ D │   (bottom row)
  //   └───┴───┘
  const grid: MosaicNode = {
    kind: "split",
    axis: "column",
    children: [
      { kind: "split", axis: "row", children: [leaf("A"), leaf("B")], sizes: [50, 50] },
      { kind: "split", axis: "row", children: [leaf("C"), leaf("D")], sizes: [50, 50] },
    ],
    sizes: [50, 50],
  };

  it("B onto A's RIGHT — already adjacent in the top row → no change (the row collapses then re-wraps identically)", () => {
    expect(
      placementChangesLayout(grid, "B", { targetWorkspaceId: "A", axis: "row", side: "after" }),
    ).toBe(false);
  });
  it("D onto C's RIGHT — already adjacent in the bottom row → no change", () => {
    expect(
      placementChangesLayout(grid, "D", { targetWorkspaceId: "C", axis: "row", side: "after" }),
    ).toBe(false);
  });
  it("B onto A's LEFT — reorders the top row → change", () => {
    expect(
      placementChangesLayout(grid, "B", { targetWorkspaceId: "A", axis: "row", side: "before" }),
    ).toBe(true);
  });
  it("B down onto C's TOP — moves B out of the top row into the bottom → change", () => {
    expect(
      placementChangesLayout(grid, "B", { targetWorkspaceId: "C", axis: "column", side: "before" }),
    ).toBe(true);
  });
  it("B onto A's BOTTOM — stacks within the top-left cell → change", () => {
    expect(
      placementChangesLayout(grid, "B", { targetWorkspaceId: "A", axis: "column", side: "after" }),
    ).toBe(true);
  });

  // A 3-wide top row stacked over a single bottom tile.
  //   column [ row[A, B, E] , D ]
  const wideTop: MosaicNode = {
    kind: "split",
    axis: "column",
    children: [
      {
        kind: "split",
        axis: "row",
        children: [leaf("A"), leaf("B"), leaf("E")],
        sizes: [34, 33, 33],
      },
      leaf("D"),
    ],
    sizes: [60, 40],
  };

  it("B onto A's RIGHT in a 3-wide row — already there → no change", () => {
    expect(
      placementChangesLayout(wideTop, "B", { targetWorkspaceId: "A", axis: "row", side: "after" }),
    ).toBe(false);
  });
  it("B onto E's LEFT in a 3-wide row — already between A and E → no change", () => {
    expect(
      placementChangesLayout(wideTop, "B", { targetWorkspaceId: "E", axis: "row", side: "before" }),
    ).toBe(false);
  });
  it("E onto A's RIGHT in a 3-wide row — E jumps from the end to the middle → change", () => {
    expect(
      placementChangesLayout(wideTop, "E", { targetWorkspaceId: "A", axis: "row", side: "after" }),
    ).toBe(true);
  });
});
