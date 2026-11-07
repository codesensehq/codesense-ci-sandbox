// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { buildGroupIndex } from "./build-group-index";
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

describe("buildGroupIndex", () => {
  it("indexes root-level stories at key ''", () => {
    const tree = makeTree({
      stories: [
        makeStory("a.md", { status: "todo", title: "Alpha" }),
        makeStory("b.md", { status: "done" }),
      ],
    });
    const index = buildGroupIndex(tree);
    expect(index.has("")).toBe(true);
    const root = index.get("")!;
    expect(root.files.size).toBe(2);
    expect(root.files.get("a.md")!.status).toBe("todo");
    expect(root.files.get("a.md")!.title).toBe("Alpha");
    expect(root.files.get("b.md")!.status).toBe("done");
    expect(root.files.get("b.md")!.title).toBe("b"); // fallback: stripPipelineExtension
  });

  it("indexes a single sub-epic", () => {
    const tree = makeTree({
      subEpics: [
        makeTree({
          dirPath: "auth",
          dirName: "auth",
          absoluteDirPath: "/repo/auth",
          stories: [makeStory("x.md", { status: "in-progress", documentType: "bug" })],
        }),
      ],
    });
    const index = buildGroupIndex(tree);
    expect(index.has("auth")).toBe(true);
    const entry = index.get("auth")!;
    expect(entry.dirName).toBe("auth");
    expect(entry.files.get("x.md")!.status).toBe("in-progress");
    expect(entry.files.get("x.md")!.documentType).toBe("bug"); // raw, not normalized
  });

  it("indexes nested sub-epics with correct scope-relative keys", () => {
    const tree = makeTree({
      subEpics: [
        makeTree({
          dirPath: "epic-a",
          dirName: "epic-a",
          absoluteDirPath: "/repo/epic-a",
          subEpics: [
            makeTree({
              dirPath: "epic-a/sub1",
              dirName: "sub1",
              absoluteDirPath: "/repo/epic-a/sub1",
              stories: [makeStory("s.md", { status: "done" })],
            }),
          ],
          stories: [makeStory("a.md", { status: "todo" })],
        }),
      ],
    });
    const index = buildGroupIndex(tree);
    expect(index.has("epic-a")).toBe(true);
    expect(index.has("epic-a/sub1")).toBe(true);
    expect(index.get("epic-a/sub1")!.files.get("s.md")!.status).toBe("done");
  });

  it("returns empty root entry for empty tree", () => {
    const tree = makeTree();
    const index = buildGroupIndex(tree);
    expect(index.size).toBe(1); // just the root
    expect(index.get("")!.files.size).toBe(0);
  });

  it("keys stories by fileName", () => {
    const tree = makeTree({
      subEpics: [
        makeTree({
          dirPath: "fixes",
          dirName: "fixes",
          absoluteDirPath: "/repo/fixes",
          stories: [
            makeStory("F-1-auth-bug.md", { status: "done", title: "Auth Bug" }),
            makeStory("F-2-login-fix.md", { status: "todo", title: "Login Fix" }),
          ],
        }),
      ],
    });
    const index = buildGroupIndex(tree);
    const entry = index.get("fixes")!;
    expect(entry.files.has("F-1-auth-bug.md")).toBe(true);
    expect(entry.files.has("F-2-login-fix.md")).toBe(true);
    expect(entry.files.get("F-1-auth-bug.md")!.title).toBe("Auth Bug");
  });

  it("stores raw documentType without normalization", () => {
    const tree = makeTree({
      stories: [
        makeStory("a.md", { documentType: "SPIKE" }),
        makeStory("b.md", { documentType: null }),
      ],
    });
    const index = buildGroupIndex(tree);
    expect(index.get("")!.files.get("a.md")!.documentType).toBe("SPIKE");
    expect(index.get("")!.files.get("b.md")!.documentType).toBeNull();
  });

  it("does not include groupId (React Flow-specific)", () => {
    const tree = makeTree({
      subEpics: [
        makeTree({
          dirPath: "auth",
          dirName: "auth",
          absoluteDirPath: "/repo/auth",
          stories: [makeStory("x.md")],
        }),
      ],
    });
    const index = buildGroupIndex(tree);
    const entry = index.get("auth")!;
    expect(entry).not.toHaveProperty("groupId");
  });
});
