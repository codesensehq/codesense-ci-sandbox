// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import type { PublishFile } from "@/features/git-sync/lib/types";
import {
  buildFileTree,
  diffTotals,
  filterFiles,
  flatRows,
  navigationOrder,
  prefersTree,
  splitPath,
  stepFile,
  treeRows,
} from "./source-diff-model";

const f = (path: string, extra: Partial<PublishFile> = {}): PublishFile => ({
  path,
  status: "modified",
  ...extra,
});

const FILES = [
  f("src/features/viewer/components/file-row.tsx", { added: 3, removed: 1 }),
  f("src/features/viewer/lib/model.ts", { status: "added", added: 9, removed: 0 }),
  f("src-tauri/src/lib.rs", { added: 1 }),
  f("README.md"),
  f("src/features/viewer/lib/row.ts", { renamedFrom: "src/features/viewer/lib/format.ts" }),
];

describe("source diff model", () => {
  it("totals only counted files and says when nothing was countable", () => {
    expect(diffTotals(FILES)).toEqual({ added: 13, removed: 1, counted: true });
    expect(diffTotals([f("a.png", { status: "added" })]).counted).toBe(false);
  });

  it("filters on every term, matching the pre-rename path too", () => {
    expect(filterFiles(FILES, "viewer row").map((x) => x.path)).toEqual([
      "src/features/viewer/components/file-row.tsx",
      "src/features/viewer/lib/row.ts",
    ]);
    expect(filterFiles(FILES, "FORMAT").map((x) => x.path)).toEqual([
      "src/features/viewer/lib/row.ts",
    ]);
    expect(filterFiles(FILES, "  ")).toHaveLength(FILES.length);
  });

  it("builds a tree with compacted single-child directories, dirs before files", () => {
    const tree = buildFileTree(FILES);
    expect(tree.map((n) => n.name)).toEqual(["src/features/viewer", "src-tauri/src", "README.md"]);
    const viewer = tree[0];
    expect(viewer.kind === "dir" && viewer.fileCount).toBe(3);
    expect(viewer.kind === "dir" && viewer.children.map((c) => c.name)).toEqual([
      "components",
      "lib",
    ]);
  });

  it("sorts names with numbers in numeric order", () => {
    const tree = buildFileTree([
      f("m/module-10/a.ts"),
      f("m/module-2/a.ts"),
      f("m/x/file-10.ts"),
      f("m/x/file-2.ts"),
      f("m/y.ts"),
    ]);
    const m = tree[0];
    expect(m.kind === "dir" && m.children.map((c) => c.name)).toEqual([
      "module-2",
      "module-10",
      "x",
      "y.ts",
    ]);
    const x = m.kind === "dir" ? m.children[2] : null;
    expect(x?.kind === "dir" && x.children.map((c) => c.name)).toEqual(["file-2.ts", "file-10.ts"]);
  });

  it("orders numerically equal names the same whatever the input order", () => {
    const names = (paths: string[]) => {
      const x = buildFileTree(paths.map((p) => f(p)))[0];
      return x.kind === "dir" ? x.children.map((c) => c.name) : [];
    };
    const a = names(["x/file-1.ts", "x/file-01.ts", "x/d-1/a.ts", "x/d-01/a.ts"]);
    const b = names(["x/file-01.ts", "x/file-1.ts", "x/d-01/a.ts", "x/d-1/a.ts"]);
    expect(a).toEqual(b);
  });

  it("orders navigation by what is on screen and skips collapsed directories", () => {
    const tree = buildFileTree(FILES);
    expect(navigationOrder(treeRows(tree, new Set()))).toEqual([
      "src/features/viewer/components/file-row.tsx",
      "src/features/viewer/lib/model.ts",
      "src/features/viewer/lib/row.ts",
      "src-tauri/src/lib.rs",
      "README.md",
    ]);
    const folded = treeRows(tree, new Set(["src/features/viewer"]));
    expect(navigationOrder(folded)).toEqual(["src-tauri/src/lib.rs", "README.md"]);
    expect(folded.find((r) => r.kind === "dir" && r.path === "src/features/viewer")).toMatchObject({
      collapsed: true,
      fileCount: 3,
    });
    expect(navigationOrder(flatRows(FILES))).toEqual(FILES.map((x) => x.path));
  });

  it("steps through files clamped at both ends", () => {
    const order = ["a", "b", "c"];
    expect(stepFile(order, null, 1)).toBe("a");
    expect(stepFile(order, "a", 1)).toBe("b");
    expect(stepFile(order, "c", 1)).toBe("c");
    expect(stepFile(order, "a", -1)).toBe("a");
    expect(stepFile(order, "gone", -1)).toBe("a");
    expect(stepFile([], "a", 1)).toBeNull();
  });

  it("prefers the tree only for a larger, multi-directory change set", () => {
    expect(prefersTree(FILES)).toBe(false);
    expect(prefersTree([...FILES, f("docs/a.md")])).toBe(true);
    expect(prefersTree(Array.from({ length: 8 }, (_, i) => f(`src/${i}.ts`)))).toBe(false);
  });

  it("splits a path into directory and name", () => {
    expect(splitPath("src/a/b.ts")).toEqual({ dir: "src/a/", name: "b.ts" });
    expect(splitPath("b.ts")).toEqual({ dir: "", name: "b.ts" });
  });
});
