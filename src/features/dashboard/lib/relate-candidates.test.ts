// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import { buildTree, buildStoryRef } from "@/shared/lib/pipeline-graph/test-fixtures/derived-tree";

import { collectRelateCandidates } from "./relate-candidates";

function tree() {
  return buildTree("", {
    absoluteDirPath: "/repo",
    stories: [
      buildStoryRef("a.md", { absolutePath: "/repo/a.md" }),
      buildStoryRef("b.md", { absolutePath: "/repo/b.md" }),
    ],
    docs: [{ filePath: "/repo/notes.md", fileName: "notes.md" }],
    subEpics: [
      buildTree("sub", {
        absoluteDirPath: "/repo/sub",
        stories: [buildStoryRef("c.md", { absolutePath: "/repo/sub/c.md" })],
      }),
    ],
  });
}

describe("collectRelateCandidates", () => {
  it("collects stories + docs recursively, sorted by file name", () => {
    const got = collectRelateCandidates(tree(), new Set());
    expect(got.map((c) => c.fileName)).toEqual(["a.md", "b.md", "c.md", "notes.md"]);
    expect(got.find((c) => c.fileName === "c.md")?.filePath).toBe("/repo/sub/c.md");
  });

  it("excludes the open doc and already-related paths", () => {
    const got = collectRelateCandidates(tree(), new Set(["/repo/a.md", "/repo/notes.md"]));
    expect(got.map((c) => c.filePath)).toEqual(["/repo/b.md", "/repo/sub/c.md"]);
  });

  it("returns [] for a null tree (no crash before the scan lands)", () => {
    expect(collectRelateCandidates(null, new Set())).toEqual([]);
  });
});
