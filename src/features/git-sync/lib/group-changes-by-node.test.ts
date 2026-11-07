// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import type { Node } from "@xyflow/react";

import type { DerivedPipelineTree, StoryRef } from "@/shared/lib/pipeline-graph/types";

import {
  buildNodeIndex,
  compactSingleChildChains,
  groupChangesByNode,
  joinOrgPath,
} from "./group-changes-by-node";
import type { NodeChangeGroup } from "./group-changes-by-node";
import type { PublishSet } from "./types";

const ORG = "/ctx";
const abs = (rel: string) => `${ORG}/${rel}`;

// --- fixtures ---------------------------------------------------------------

const README = abs("_bmad-output/epic-a/README.md");
const SW1 = abs("_bmad-output/epic-a/SW-1.md");
const SW2 = abs("_bmad-output/epic-a/SW-2.md");
const DIAGRAM = abs("_bmad-output/epic-a/diagram.excalidraw");

function storyRef(fileName: string, filePath: string, docs: string[]): StoryRef {
  return {
    fileName,
    filePath,
    dependsOn: null,
    status: null,
    documentType: "story",
    supportingDocs: docs,
    sourceRepo: null,
    team: null,
  };
}

const TREE: DerivedPipelineTree = {
  dirPath: "",
  dirName: "",
  absoluteDirPath: abs("_bmad-output"),
  stories: [],
  subEpics: [
    {
      dirPath: "epic-a",
      dirName: "epic-a",
      absoluteDirPath: abs("_bmad-output/epic-a"),
      stories: [
        storyRef("SW-1.md", SW1, ["./diagram.excalidraw"]),
        storyRef("SW-2.md", SW2, ["./diagram.excalidraw"]),
      ],
      subEpics: [],
      orderError: null,
      mainDoc: { filePath: README, supportingDocs: null, purposeText: null },
    },
  ],
  orderError: null,
  mainDoc: null,
};

const sharedDoc = [
  { fileName: "diagram.excalidraw", filePath: DIAGRAM, documentType: "excalidraw" },
];

const NODES = [
  {
    id: "g-epic-a",
    type: "group",
    position: { x: 0, y: 0 },
    data: {
      title: "Epic A",
      dirPath: "epic-a",
      absoluteDirPath: abs("_bmad-output/epic-a"),
      mainDocFilePath: README,
    },
  },
  {
    id: "s-1",
    type: "story",
    position: { x: 0, y: 0 },
    data: {
      title: "SW-1",
      status: "todo",
      documentType: "story",
      storyFile: SW1,
      resolvedSupportingDocs: sharedDoc,
    },
  },
  {
    id: "s-2",
    type: "story",
    position: { x: 0, y: 0 },
    data: {
      title: "SW-2",
      status: "todo",
      documentType: "story",
      storyFile: SW2,
      resolvedSupportingDocs: sharedDoc,
    },
  },
] as unknown as Node[];

function publishSet(files: PublishSet["general"]["files"]): PublishSet {
  return { buckets: [], general: { count: files.length, files } };
}

// --- tests ------------------------------------------------------------------

describe("joinOrgPath", () => {
  it("joins without double slashes", () => {
    expect(joinOrgPath("/ctx", "a/b.md")).toBe("/ctx/a/b.md");
    expect(joinOrgPath("/ctx/", "/a/b.md")).toBe("/ctx/a/b.md");
  });
});

describe("buildNodeIndex", () => {
  it("counts a shared doc as owned by both stories", () => {
    const idx = buildNodeIndex(NODES);
    expect(idx.ownerCountByPath.get(DIAGRAM)).toBe(2);
    expect(idx.ownerCountByPath.get(SW1)).toBe(1);
    expect(idx.storyByFile.has(SW1)).toBe(true);
    expect(idx.groupByDirPath.get("epic-a")?.id).toBe("g-epic-a");
  });

  it("counts a node owning the same path via own-file and a doc as ONE owner", () => {
    const selfRef = [
      {
        id: "s-x",
        type: "story",
        position: { x: 0, y: 0 },
        data: {
          title: "X",
          status: "todo",
          documentType: "story",
          storyFile: SW1,
          resolvedSupportingDocs: [{ fileName: "SW-1.md", filePath: SW1, documentType: "story" }],
        },
      },
    ] as unknown as Node[];
    expect(buildNodeIndex(selfRef).ownerCountByPath.get(SW1)).toBe(1);
  });
});

describe("groupChangesByNode", () => {
  it("groups a story's own change + its supporting doc under the story, nested in its epic", () => {
    const set = publishSet([
      { path: "_bmad-output/epic-a/SW-1.md", status: "modified" },
      { path: "_bmad-output/epic-a/diagram.excalidraw", status: "modified" },
    ]);
    const grouped = groupChangesByNode(set, TREE, NODES, ORG);

    expect(grouped.nodes).toHaveLength(1);
    const epic = grouped.nodes[0];
    expect(epic.node.kind).toBe("epic");
    expect(epic.self).toBeNull(); // README unchanged

    const sw1 = epic.children.find((c) => c.node.id === "s-1")!;
    expect(sw1.self?.status).toBe("modified");
    expect(sw1.docs.map((d) => d.path)).toContain("_bmad-output/epic-a/diagram.excalidraw");
  });

  it("shows a shared doc under EVERY owner and counts it once (option B)", () => {
    const set = publishSet([
      { path: "_bmad-output/epic-a/SW-1.md", status: "modified" },
      { path: "_bmad-output/epic-a/diagram.excalidraw", status: "modified" },
    ]);
    const grouped = groupChangesByNode(set, TREE, NODES, ORG);
    const epic = grouped.nodes[0];

    // SW-2 has no own change but owns the shared diagram → appears with no self.
    const sw1 = epic.children.find((c) => c.node.id === "s-1")!;
    const sw2 = epic.children.find((c) => c.node.id === "s-2")!;
    expect(sw2.self).toBeNull();
    const inBoth = (g: typeof sw1) => g.docs.find((d) => d.path.endsWith("diagram.excalidraw"));
    expect(inBoth(sw1)?.shared).toBe(true);
    expect(inBoth(sw2)?.shared).toBe(true);

    // Dedup: SW-1 + the shared diagram = 2 unique paths, not 3.
    expect(grouped.totalFiles).toBe(2);
  });

  it("dir-attributes a deleted unowned file to its enclosing epic (option A)", () => {
    const set = publishSet([{ path: "_bmad-output/epic-a/old-draft.md", status: "deleted" }]);
    const grouped = groupChangesByNode(set, TREE, NODES, ORG);

    expect(grouped.documentation).toHaveLength(0);
    const epic = grouped.nodes[0];
    const del = epic.docs.find((d) => d.path.endsWith("old-draft.md"));
    expect(del?.status).toBe("deleted");
  });

  it("routes a file owned by no node to Documentation", () => {
    const set = publishSet([{ path: "_bmad-output/notes.md", status: "added" }]);
    const grouped = groupChangesByNode(set, TREE, NODES, ORG);

    expect(grouped.nodes).toHaveLength(0);
    expect(grouped.documentation.map((d) => d.path)).toEqual(["_bmad-output/notes.md"]);
    expect(grouped.totalFiles).toBe(1);
  });

  it("prunes nodes with no changes", () => {
    const grouped = groupChangesByNode(publishSet([]), TREE, NODES, ORG);
    expect(grouped.nodes).toHaveLength(0);
    expect(grouped.documentation).toHaveLength(0);
    expect(grouped.totalFiles).toBe(0);
  });

  it("carries renamedFrom into the ChangedFile (SW-GS-14) without inflating the count", () => {
    // SW-1.md was renamed from an old path AND edited.
    const set = publishSet([
      {
        path: "_bmad-output/epic-a/SW-1.md",
        status: "modified",
        renamedFrom: "_bmad-output/epic-a/SW-1-old.md",
      },
    ]);
    const grouped = groupChangesByNode(set, TREE, NODES, ORG);
    const sw1 = grouped.nodes[0].children.find((c) => c.node.id === "s-1")!;
    expect(sw1.self?.renamedFrom).toBe("_bmad-output/epic-a/SW-1-old.md");
    // The old path is consumed by the rename server-side → not a separate entry.
    expect(grouped.totalFiles).toBe(1);
  });

  it("leaves renamedFrom undefined for a non-rename", () => {
    const set = publishSet([{ path: "_bmad-output/epic-a/SW-1.md", status: "modified" }]);
    const grouped = groupChangesByNode(set, TREE, NODES, ORG);
    const sw1 = grouped.nodes[0].children.find((c) => c.node.id === "s-1")!;
    expect(sw1.self?.renamedFrom).toBeUndefined();
  });
});

// --- SW-GS-15: breadcrumb compaction ---------------------------------------

describe("compactSingleChildChains (SW-GS-15)", () => {
  function epicGroup(
    title: string,
    children: NodeChangeGroup[],
    count: number,
    opts?: { self?: NodeChangeGroup["self"]; docs?: NodeChangeGroup["docs"] },
  ): NodeChangeGroup {
    return {
      node: { id: `g-${title}`, title, kind: "epic", filePath: "" },
      self: opts?.self ?? null,
      docs: opts?.docs ?? [],
      children,
      count,
    };
  }

  function storyGroup(title: string): NodeChangeGroup {
    return {
      node: { id: `s-${title}`, title, kind: "story", filePath: `/ctx/${title}.md` },
      self: { path: `${title}.md`, status: "modified", shared: false },
      docs: [],
      children: [],
      count: 1,
    };
  }

  it("folds a 5-deep single-child epic chain into one joined-title node, subtree + count intact", () => {
    const leaf = epicGroup("e", [storyGroup("SW-1"), storyGroup("SW-2")], 2);
    const chain = epicGroup(
      "a",
      [epicGroup("b", [epicGroup("c", [epicGroup("d", [leaf], 2)], 2)], 2)],
      2,
    );

    const [folded] = compactSingleChildChains([chain]);

    expect(folded.node.title).toBe("a / b / c / d / e");
    expect(folded.node.kind).toBe("epic");
    // Carries the deepest node's subtree + count VERBATIM (not recomputed).
    expect(folded.count).toBe(2);
    expect(folded.children.map((c) => c.node.title)).toEqual(["SW-1", "SW-2"]);
  });

  it("does NOT fold a node with its own changes (self set)", () => {
    const withSelf = epicGroup("a", [epicGroup("b", [storyGroup("SW-1")], 1)], 2, {
      self: { path: "a/README.md", status: "modified", shared: false },
    });
    const [result] = compactSingleChildChains([withSelf]);
    expect(result.node.title).toBe("a");
    expect(result.children).toHaveLength(1);
  });

  it("does NOT fold a node with its own docs", () => {
    const withDocs = epicGroup("a", [epicGroup("b", [storyGroup("SW-1")], 1)], 2, {
      docs: [{ path: "a/old.md", status: "deleted", shared: false }],
    });
    const [result] = compactSingleChildChains([withDocs]);
    expect(result.node.title).toBe("a");
  });

  it("does NOT fold a 2-child node", () => {
    const twoChild = epicGroup("a", [epicGroup("b", [], 0), epicGroup("c", [], 0)], 0);
    const [result] = compactSingleChildChains([twoChild]);
    expect(result.node.title).toBe("a");
    expect(result.children).toHaveLength(2);
  });

  it("does NOT fold an epic whose single child is a STORY (keeps the owning epic its own row)", () => {
    const epicWithStory = epicGroup("workflows", [storyGroup("SW-GS-3")], 1);
    const [result] = compactSingleChildChains([epicWithStory]);
    expect(result.node.title).toBe("workflows");
    expect(result.children[0].node.kind).toBe("story");
  });

  it("compacts a nested single-child chain that sits under a branch point", () => {
    // root ⊃ [chain(a⊃b⊃leafEpic), storyChildEpic] — the a/b chain folds, the sibling stays.
    const chain = epicGroup("a", [epicGroup("b", [storyGroup("SW-1")], 1)], 1);
    const sibling = epicGroup("other", [storyGroup("SW-2")], 1);
    const root = epicGroup("root", [chain, sibling], 2);

    const [result] = compactSingleChildChains([root]);
    // root has 2 children → not folded itself; its chain child folds to "a / b".
    expect(result.node.title).toBe("root");
    const foldedChain = result.children.find((c) => c.node.title.includes("a / b"));
    expect(foldedChain?.node.title).toBe("a / b");
    expect(foldedChain?.children[0].node.title).toBe("SW-1");
  });
});
