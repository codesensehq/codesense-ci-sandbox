// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { findNavNodeByPath, visitNavTree } from "./find-nav-node-by-path";
import type { DerivedPipelineTree, DocRef, StoryRef } from "./types";

function baseTree(over: Partial<DerivedPipelineTree> = {}): DerivedPipelineTree {
  return {
    dirPath: "",
    dirName: "",
    absoluteDirPath: "/root",
    stories: [],
    docs: [],
    subEpics: [],
    orderError: null,
    mainDoc: null,
    ...over,
  };
}

function story(filePath: string, over: Partial<StoryRef> = {}): StoryRef {
  return {
    fileName: filePath.split("/").pop() ?? "",
    filePath,
    dependsOn: null,
    status: null,
    documentType: "story",
    supportingDocs: null,
    sourceRepo: null,
    team: null,
    ...over,
  };
}

function doc(filePath: string, over: Partial<DocRef> = {}): DocRef {
  return { fileName: filePath.split("/").pop() ?? "", filePath, ...over };
}

describe("findNavNodeByPath", () => {
  it("matches a story row", () => {
    const t = baseTree({ stories: [story("/root/s.md")] });
    const m = findNavNodeByPath(t, "/root/s.md");
    expect(m?.story?.filePath).toBe("/root/s.md");
    expect(m?.doc).toBeUndefined();
  });

  it("matches a doc row", () => {
    const t = baseTree({ docs: [doc("/root/d.md")] });
    const m = findNavNodeByPath(t, "/root/d.md");
    expect(m?.doc?.filePath).toBe("/root/d.md");
    expect(m?.story).toBeUndefined();
  });

  it("recurses into sub-epics", () => {
    const t = baseTree({
      subEpics: [baseTree({ docs: [doc("/root/sub/x.md")] })],
    });
    expect(findNavNodeByPath(t, "/root/sub/x.md")?.doc?.filePath).toBe("/root/sub/x.md");
  });

  it("returns null for unknown paths, null tree, and empty path", () => {
    const t = baseTree({ stories: [story("/root/s.md")] });
    expect(findNavNodeByPath(t, "/root/nope.md")).toBeNull();
    expect(findNavNodeByPath(null, "/root/s.md")).toBeNull();
    expect(findNavNodeByPath(t, "")).toBeNull();
  });
});

describe("visitNavTree", () => {
  it("returns the first non-null pick, depth-first", () => {
    const t = baseTree({
      subEpics: [
        baseTree({ dirName: "a", docs: [doc("/root/a/x.md")] }),
        baseTree({ dirName: "b", docs: [doc("/root/b/x.md")] }),
      ],
    });
    const firstDir = visitNavTree(t, (node) =>
      (node.docs ?? []).length > 0 ? node.dirName : null,
    );
    expect(firstDir).toBe("a");
  });

  it("returns null on a null tree", () => {
    expect(visitNavTree(null, () => "x")).toBeNull();
  });
});
