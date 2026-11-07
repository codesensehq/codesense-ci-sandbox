// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import { storyFilePathFromTree } from "./resolve-story-file-path";
import type { DerivedPipelineTree, StoryRef } from "./types";

function makeStory(fileName: string, overrides?: Partial<StoryRef>): StoryRef {
  return {
    fileName,
    filePath: `/repo/${fileName}`,
    title: null,
    dependsOn: null,
    status: null,
    storyText: null,
    documentType: null,
    supportingDocs: null,
    sourceRepo: null,
    team: null,
    ...overrides,
  };
}

function makeTree(overrides?: Partial<DerivedPipelineTree>): DerivedPipelineTree {
  return {
    dirPath: "",
    dirName: "",
    absoluteDirPath: "/repo",
    stories: [],
    subEpics: [],
    orderError: null,
    mainDoc: null,
    ...overrides,
  };
}

describe("storyFilePathFromTree", () => {
  it("resolves a root-level story_id to its absolute file path", () => {
    const tree = makeTree({
      stories: [
        makeStory("SW-AM-7-foo.md", { storyId: "SW-AM-7", filePath: "/repo/SW-AM-7-foo.md" }),
        makeStory("SW-AM-1-bar.md", { storyId: "SW-AM-1", filePath: "/repo/SW-AM-1-bar.md" }),
      ],
    });
    expect(storyFilePathFromTree(tree, "SW-AM-7")).toBe("/repo/SW-AM-7-foo.md");
  });

  it("resolves a story_id nested in a sub-epic directory", () => {
    const tree = makeTree({
      subEpics: [
        makeTree({
          dirPath: "SE-2",
          dirName: "SE-2",
          absoluteDirPath: "/repo/SE-2",
          stories: [
            makeStory("SW-AM-7-foo.md", {
              storyId: "SW-AM-7",
              filePath: "/repo/SE-2/SW-AM-7-foo.md",
            }),
          ],
        }),
      ],
    });
    expect(storyFilePathFromTree(tree, "SW-AM-7")).toBe("/repo/SE-2/SW-AM-7-foo.md");
  });

  it("returns null for a story_id absent from the tree", () => {
    const tree = makeTree({
      stories: [makeStory("SW-AM-1-bar.md", { storyId: "SW-AM-1" })],
    });
    expect(storyFilePathFromTree(tree, "SW-AM-7")).toBeNull();
  });

  it("returns null for a duplicate (ambiguous) story_id — never an arbitrary target", () => {
    const tree = makeTree({
      stories: [makeStory("a.md", { storyId: "DUP" })],
      subEpics: [
        makeTree({
          dirPath: "SE-2",
          dirName: "SE-2",
          absoluteDirPath: "/repo/SE-2",
          stories: [makeStory("b.md", { storyId: "DUP", filePath: "/repo/SE-2/b.md" })],
        }),
      ],
    });
    expect(storyFilePathFromTree(tree, "DUP")).toBeNull();
  });
});
