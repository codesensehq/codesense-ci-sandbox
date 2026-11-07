// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { countByStatus, rollupEpicStatus, rollupEpicStatusForDoc } from "./count-by-status";
import { buildStoryRef, buildTree } from "./test-fixtures/derived-tree";
import type { MainDoc } from "./types";

const mainDoc = (filePath: string): MainDoc => ({
  filePath,
  supportingDocs: null,
  purposeText: null,
});

describe("countByStatus", () => {
  it("returns zero totals for empty array", () => {
    expect(countByStatus([])).toEqual({ total: 0, done: 0 });
  });

  it("counts done stories correctly", () => {
    const stories = [
      buildStoryRef("a.md", { status: "done" }),
      buildStoryRef("b.md", { status: "in-progress" }),
      buildStoryRef("c.md", { status: "done" }),
      buildStoryRef("d.md", { status: "todo" }),
    ];
    expect(countByStatus(stories)).toEqual({ total: 4, done: 2 });
  });

  it("treats null status as not-done", () => {
    const stories = [buildStoryRef("a.md"), buildStoryRef("b.md")];
    expect(countByStatus(stories)).toEqual({ total: 2, done: 0 });
  });

  it("treats unrecognized status as not-done", () => {
    const stories = [
      buildStoryRef("a.md", { status: "garbage" }),
      buildStoryRef("b.md", { status: "done" }),
    ];
    expect(countByStatus(stories)).toEqual({ total: 2, done: 1 });
  });

  it("handles all-done list", () => {
    const stories = [
      buildStoryRef("a.md", { status: "done" }),
      buildStoryRef("b.md", { status: "done" }),
    ];
    expect(countByStatus(stories)).toEqual({ total: 2, done: 2 });
  });
});

describe("rollupEpicStatus", () => {
  it("returns planning for an empty epic (no child stories)", () => {
    expect(rollupEpicStatus([])).toBe("planning");
  });

  it("returns blocked when ANY child is blocked, even alongside done", () => {
    const stories = [
      buildStoryRef("a.md", { status: "done" }),
      buildStoryRef("b.md", { status: "blocked" }),
      buildStoryRef("c.md", { status: "in-progress" }),
    ];
    expect(rollupEpicStatus(stories)).toBe("blocked");
  });

  it("returns done only when ALL children are done", () => {
    const stories = [
      buildStoryRef("a.md", { status: "done" }),
      buildStoryRef("b.md", { status: "done" }),
    ];
    expect(rollupEpicStatus(stories)).toBe("done");
  });

  it("returns in-progress when ANY child is started", () => {
    for (const started of ["in-progress", "review", "qa"]) {
      const stories = [
        buildStoryRef("a.md", { status: "todo" }),
        buildStoryRef("b.md", { status: started }),
      ];
      expect(rollupEpicStatus(stories)).toBe("in-progress");
    }
  });

  it("returns in-progress when some (but not all) children are done", () => {
    const stories = [
      buildStoryRef("a.md", { status: "done" }),
      buildStoryRef("b.md", { status: "todo" }),
    ];
    expect(rollupEpicStatus(stories)).toBe("in-progress");
  });

  it("rolls all-unstarted children up to the not-started default (planning)", () => {
    // SW-GT-3: the not-started rollup is now the representative not-started state —
    // the first state by `order` (planning in the default seed), not literal `todo`.
    const stories = [
      buildStoryRef("a.md", { status: "todo" }),
      buildStoryRef("b.md", { status: "todo" }),
    ];
    expect(rollupEpicStatus(stories)).toBe("planning");
  });

  it("normalizes off-taxonomy frontmatter to the default before rolling up", () => {
    // `ready-for-dev` / `backlog` are off-taxonomy → normalizeStatus → planning.
    const stories = [
      buildStoryRef("a.md", { status: "ready-for-dev" }),
      buildStoryRef("b.md", { status: "backlog" }),
    ];
    expect(rollupEpicStatus(stories)).toBe("planning");
  });

  it("treats planning children as not-yet-started (planning rollup)", () => {
    const stories = [
      buildStoryRef("a.md", { status: "planning" }),
      buildStoryRef("b.md", { status: "todo" }),
    ];
    expect(rollupEpicStatus(stories)).toBe("planning");
  });
});

describe("rollupEpicStatusForDoc", () => {
  // An epic node (`mainDoc !== null`) holding two stories, nested under the root.
  const tree = buildTree("", {
    absoluteDirPath: "/ctx/pipe",
    subEpics: [
      buildTree("epic-a", {
        dirPath: "epic-a",
        absoluteDirPath: "/ctx/pipe/epic-a",
        mainDoc: mainDoc("/ctx/pipe/epic-a/README.md"),
        stories: [
          buildStoryRef("a1.md", { status: "in-progress", absolutePath: "/ctx/pipe/epic-a/a1.md" }),
          buildStoryRef("a2.md", { status: "todo", absolutePath: "/ctx/pipe/epic-a/a2.md" }),
        ],
      }),
    ],
  });

  // The nav opens an epic terminal with the epic node's OWN mainDoc.filePath,
  // so that exact path is what flows in as epicDocPath.
  it("rolls up the located epic's child stories", () => {
    expect(rollupEpicStatusForDoc(tree, "/ctx/pipe/epic-a/README.md")).toBe("in-progress");
  });

  it("returns planning for a located but empty epic", () => {
    const emptyEpicTree = buildTree("", {
      absoluteDirPath: "/ctx/pipe",
      subEpics: [
        buildTree("epic-empty", {
          dirPath: "epic-empty",
          absoluteDirPath: "/ctx/pipe/epic-empty",
          mainDoc: mainDoc("/ctx/pipe/epic-empty/README.md"),
        }),
      ],
    });
    expect(rollupEpicStatusForDoc(emptyEpicTree, "/ctx/pipe/epic-empty/README.md")).toBe(
      "planning",
    );
  });

  // The guard: a doc path that resolves to NO enclosing epic must NOT roll up
  // the whole tree — it returns null so the chip is omitted.
  it("returns null when the epic doc path isn't in the tree", () => {
    expect(rollupEpicStatusForDoc(tree, "/somewhere/else/gone.md")).toBeNull();
  });

  // Regression (round-2 review): the fallback can land on an ANCESTOR/ROOT epic
  // that itself carries a mainDoc. Exact-identity matching must still return
  // null there — never the ancestor's/whole-pipeline's status.
  it("returns null when the path resolves only to an ancestor/root epic, not its own mainDoc", () => {
    const rootEpicTree = buildTree("", {
      absoluteDirPath: "/ctx/pipe",
      mainDoc: mainDoc("/ctx/pipe/README.md"), // the root itself is an epic
      subEpics: [
        buildTree("epic-a", {
          dirPath: "epic-a",
          absoluteDirPath: "/ctx/pipe/epic-a",
          mainDoc: mainDoc("/ctx/pipe/epic-a/README.md"),
          stories: [
            buildStoryRef("a1.md", { status: "blocked", absolutePath: "/ctx/pipe/epic-a/a1.md" }),
          ],
        }),
      ],
    });
    // A stale doc path enclosed by the root epic but matching no epic's mainDoc.
    expect(rollupEpicStatusForDoc(rootEpicTree, "/ctx/pipe/deleted/README.md")).toBeNull();
  });
});
