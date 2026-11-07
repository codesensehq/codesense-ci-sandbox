// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { closeTab, cycleTab, openTab, tabLabels, tabsOutOfView } from "./editor-tabs";

const clean = () => false;

describe("openTab", () => {
  it("inserts after the active tab and keeps an already-open tab in place", () => {
    expect(openTab(["a", "b"], "c", { active: "a", isDirty: clean })).toEqual(["a", "c", "b"]);
    expect(openTab(["a", "b"], "b", { active: "a", isDirty: clean })).toEqual(["a", "b"]);
    expect(openTab([], "a", { active: null, isDirty: clean })).toEqual(["a"]);
  });

  it("evicts the oldest clean inactive tab past the cap, never a dirty one", () => {
    const dirty = (p: string) => p === "a";
    expect(openTab(["a", "b", "c"], "d", { active: "c", isDirty: dirty, max: 3 })).toEqual([
      "a",
      "c",
      "d",
    ]);
  });

  it("overflows rather than drop edits when every other tab is dirty", () => {
    expect(openTab(["a", "b"], "c", { active: "b", isDirty: () => true, max: 2 })).toEqual([
      "a",
      "b",
      "c",
    ]);
  });
});

describe("closeTab", () => {
  it("moves focus to the right neighbour, or the left one at the end", () => {
    expect(closeTab(["a", "b", "c"], "b", "b")).toEqual({ tabs: ["a", "c"], active: "c" });
    expect(closeTab(["a", "b", "c"], "c", "c")).toEqual({ tabs: ["a", "b"], active: "b" });
    expect(closeTab(["a"], "a", "a")).toEqual({ tabs: [], active: null });
  });

  it("leaves focus alone when an inactive tab closes", () => {
    expect(closeTab(["a", "b"], "a", "b")).toEqual({ tabs: ["b"], active: "b" });
  });
});

describe("cycleTab", () => {
  it("wraps in both directions", () => {
    expect(cycleTab(["a", "b", "c"], "c", 1)).toBe("a");
    expect(cycleTab(["a", "b", "c"], "a", -1)).toBe("c");
    expect(cycleTab([], null, 1)).toBeNull();
  });
});

describe("tabLabels", () => {
  it("disambiguates duplicate basenames with their parent folder", () => {
    const labels = tabLabels(["/r/src/a/index.ts", "/r/src/b/index.ts", "/r/README.md"]);
    expect(labels.get("/r/src/a/index.ts")).toEqual({ name: "index.ts", hint: "a" });
    expect(labels.get("/r/src/b/index.ts")).toEqual({ name: "index.ts", hint: "b" });
    expect(labels.get("/r/README.md")).toEqual({ name: "README.md", hint: null });
  });
});

describe("tabsOutOfView", () => {
  const spans = [
    { left: 0, right: 100 },
    { left: 100, right: 200 },
    { left: 200, right: 300 },
    { left: 300, right: 400 },
  ];
  it("counts tabs hidden (even partly) on each side of the visible strip", () => {
    expect(tabsOutOfView(spans, 0, 400)).toEqual({ before: 0, after: 0 });
    expect(tabsOutOfView(spans, 0, 250)).toEqual({ before: 0, after: 2 });
    expect(tabsOutOfView(spans, 150, 200)).toEqual({ before: 2, after: 1 });
  });
  it("ignores sub-pixel overlap at the edges", () => {
    expect(tabsOutOfView(spans, 0.5, 399)).toEqual({ before: 0, after: 0 });
  });
});

describe("tabLabels — a root-level file sharing a nested file's name", () => {
  it("gives the root file no folder hint and the nested one its folder", () => {
    const labels = tabLabels(["index.ts", "src/index.ts"]);
    expect(labels.get("index.ts")).toEqual({ name: "index.ts", hint: null });
    expect(labels.get("src/index.ts")).toEqual({ name: "index.ts", hint: "src" });
  });
});
