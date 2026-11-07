// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { collectAllStories } from "./collect-all-stories";
import { buildStoryRef, buildTree } from "./test-fixtures/derived-tree";

describe("collectAllStories", () => {
  it("returns empty array for empty tree", () => {
    const tree = buildTree("");
    expect(collectAllStories(tree)).toEqual([]);
  });

  it("collects stories from a single-level tree", () => {
    const s1 = buildStoryRef("a.md");
    const s2 = buildStoryRef("b.md");
    const tree = buildTree("", { stories: [s1, s2] });

    const result = collectAllStories(tree);
    expect(result).toHaveLength(2);
    expect(result).toContain(s1);
    expect(result).toContain(s2);
  });

  it("collects stories from nested sub-epics", () => {
    const root = buildStoryRef("root.md");
    const child = buildStoryRef("child.md");
    const tree = buildTree("", {
      stories: [root],
      subEpics: [
        buildTree("epic-a", {
          dirPath: "epic-a",
          stories: [child],
        }),
      ],
    });

    const result = collectAllStories(tree);
    expect(result).toHaveLength(2);
    expect(result).toContain(root);
    expect(result).toContain(child);
  });

  it("collects stories at multiple depths", () => {
    const s1 = buildStoryRef("level-0.md");
    const s2 = buildStoryRef("level-1.md");
    const s3 = buildStoryRef("level-2.md");
    const tree = buildTree("", {
      stories: [s1],
      subEpics: [
        buildTree("a", {
          dirPath: "a",
          stories: [s2],
          subEpics: [
            buildTree("b", {
              dirPath: "a/b",
              stories: [s3],
            }),
          ],
        }),
      ],
    });

    const result = collectAllStories(tree);
    expect(result).toHaveLength(3);
    expect(result[0]).toBe(s1);
    expect(result[1]).toBe(s2);
    expect(result[2]).toBe(s3);
  });

  it("handles tree with sub-epics but no stories", () => {
    const tree = buildTree("", {
      subEpics: [buildTree("empty", { dirPath: "empty" })],
    });
    expect(collectAllStories(tree)).toEqual([]);
  });

  it("collects from multiple sibling sub-epics", () => {
    const s1 = buildStoryRef("a.md");
    const s2 = buildStoryRef("b.md");
    const tree = buildTree("", {
      subEpics: [
        buildTree("epic-a", { dirPath: "epic-a", stories: [s1] }),
        buildTree("epic-b", { dirPath: "epic-b", stories: [s2] }),
      ],
    });

    const result = collectAllStories(tree);
    expect(result).toHaveLength(2);
    expect(result).toContain(s1);
    expect(result).toContain(s2);
  });
});
