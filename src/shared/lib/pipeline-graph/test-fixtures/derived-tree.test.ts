import { describe, it, expect } from "vitest";
import {
  buildTree,
  buildStoryRef,
  fixtureEmptyTree,
  fixtureTwoStoriesNoOrder,
  fixtureNestedEpics,
  fixtureThreeLevelsDeep,
} from "./derived-tree";

describe("derived-tree fixture builders", () => {
  it("buildStoryRef defaults title to null and constructs absolute path", () => {
    const ref = buildStoryRef("x.md");
    expect(ref.fileName).toBe("x.md");
    expect(ref.title).toBeNull();
    expect(ref.filePath.startsWith("/")).toBe(true);
    expect(ref.filePath.endsWith("/x.md")).toBe(true);
  });

  it("buildStoryRef accepts explicit title and absolutePath", () => {
    const ref = buildStoryRef("y.md", { title: "Y", absolutePath: "/abs/y.md" });
    expect(ref.title).toBe("Y");
    expect(ref.filePath).toBe("/abs/y.md");
  });

  it("buildTree defaults empty stories and subEpics", () => {
    const t = buildTree("root");
    expect(t.dirName).toBe("root");
    expect(t.dirPath).toBe("");
    expect(t.stories).toEqual([]);
    expect(t.subEpics).toEqual([]);
    expect(t.orderError).toBeNull();
  });

  it("buildTree composes stories, subEpics, orderError", () => {
    const t = buildTree("epic", {
      dirPath: "epic",
      stories: [buildStoryRef("a.md")],
      subEpics: [buildTree("child", { dirPath: "epic/child" })],
      orderError: "bad pipeline.json",
    });
    expect(t.dirPath).toBe("epic");
    expect(t.stories).toHaveLength(1);
    expect(t.subEpics).toHaveLength(1);
    expect(t.subEpics[0].dirPath).toBe("epic/child");
    expect(t.orderError).toBe("bad pipeline.json");
  });

  it("fixtureEmptyTree is a valid empty tree", () => {
    expect(fixtureEmptyTree.stories).toEqual([]);
    expect(fixtureEmptyTree.subEpics).toEqual([]);
  });

  it("fixtureTwoStoriesNoOrder has two stories and no edges", () => {
    expect(fixtureTwoStoriesNoOrder.stories).toHaveLength(2);
  });

  it("fixtureNestedEpics has sub-epics", () => {
    expect(fixtureNestedEpics.subEpics).toHaveLength(2);
  });

  it("fixtureThreeLevelsDeep goes three levels deep with a leaf story", () => {
    const l1 = fixtureThreeLevelsDeep.subEpics[0];
    const l2 = l1.subEpics[0];
    const l3 = l2.subEpics[0];
    expect(l1.dirName).toBe("level-1");
    expect(l2.dirName).toBe("level-2");
    expect(l3.dirName).toBe("level-3");
    expect(l3.stories[0].fileName).toBe("leaf.md");
    expect(l3.dirPath).toBe("level-1/level-2/level-3");
  });

  it("every leaf storyRef has an absolute filePath starting with /", () => {
    function walk(t: typeof fixtureNestedEpics): string[] {
      return [...t.stories.map((s) => s.filePath), ...t.subEpics.flatMap(walk)];
    }
    const paths = walk(fixtureNestedEpics);
    expect(paths.length).toBeGreaterThan(0);
    for (const p of paths) {
      expect(p.startsWith("/")).toBe(true);
    }
  });
});
