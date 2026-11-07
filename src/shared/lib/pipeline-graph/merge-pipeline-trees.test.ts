// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { filterByTeam, mergePipelineTrees, type RepoScanResult } from "./merge-pipeline-trees";
import { buildStoryRef, buildTree } from "./test-fixtures/derived-tree";

describe("mergePipelineTrees", () => {
  it("wraps each repo as a top-level sub-epic under a synthetic root", () => {
    const scans: RepoScanResult[] = [
      { repoSlug: "acme/api", tree: buildTree("", { stories: [buildStoryRef("s1.md")] }) },
      { repoSlug: "acme/web", tree: buildTree("", { stories: [buildStoryRef("s2.md")] }) },
    ];
    const merged = mergePipelineTrees(scans);

    // Synthetic root
    expect(merged.dirPath).toBe("");
    expect(merged.dirName).toBe("");
    expect(merged.stories).toEqual([]);

    // Two repo-level groups
    expect(merged.subEpics).toHaveLength(2);
    expect(merged.subEpics[0].dirPath).toBe("acme/api");
    expect(merged.subEpics[0].dirName).toBe("api");
    expect(merged.subEpics[1].dirPath).toBe("acme/web");
    expect(merged.subEpics[1].dirName).toBe("web");
  });

  it("prefixes dirPath recursively through nested sub-epics", () => {
    const inner = buildTree("epic-a", {
      dirPath: "epic-a",
      subEpics: [
        buildTree("sub-b", {
          dirPath: "epic-a/sub-b",
          stories: [buildStoryRef("leaf.md")],
        }),
      ],
    });
    const scans: RepoScanResult[] = [
      { repoSlug: "org/repo", tree: buildTree("", { subEpics: [inner] }) },
    ];
    const merged = mergePipelineTrees(scans);

    const repoGroup = merged.subEpics[0];
    expect(repoGroup.dirPath).toBe("org/repo");
    expect(repoGroup.subEpics[0].dirPath).toBe("org/repo/epic-a");
    expect(repoGroup.subEpics[0].subEpics[0].dirPath).toBe("org/repo/epic-a/sub-b");
  });

  it("backfills sourceRepo on stories that lack it", () => {
    const scans: RepoScanResult[] = [
      {
        repoSlug: "alderco/codesense",
        tree: buildTree("", {
          stories: [
            buildStoryRef("a.md"), // sourceRepo defaults to null
            buildStoryRef("b.md", { sourceRepo: "alderco/codesense" }), // already set
          ],
        }),
      },
    ];
    const merged = mergePipelineTrees(scans);
    const stories = merged.subEpics[0].stories;
    expect(stories[0].sourceRepo).toBe("alderco/codesense");
    expect(stories[1].sourceRepo).toBe("alderco/codesense");
  });

  it("does not overwrite existing sourceRepo values", () => {
    const scans: RepoScanResult[] = [
      {
        repoSlug: "org/main",
        tree: buildTree("", {
          stories: [buildStoryRef("a.md", { sourceRepo: "org/other" })],
        }),
      },
    ];
    const merged = mergePipelineTrees(scans);
    expect(merged.subEpics[0].stories[0].sourceRepo).toBe("org/other");
  });

  it("empty repos still appear as groups", () => {
    const scans: RepoScanResult[] = [{ repoSlug: "org/empty", tree: buildTree("") }];
    const merged = mergePipelineTrees(scans);
    expect(merged.subEpics).toHaveLength(1);
    expect(merged.subEpics[0].dirPath).toBe("org/empty");
    expect(merged.subEpics[0].stories).toEqual([]);
    expect(merged.subEpics[0].subEpics).toEqual([]);
  });

  it("single-repo produces same structure as multi", () => {
    const tree = buildTree("", { stories: [buildStoryRef("s.md")] });
    const single = mergePipelineTrees([{ repoSlug: "org/solo", tree }]);
    expect(single.dirPath).toBe("");
    expect(single.subEpics).toHaveLength(1);
    expect(single.subEpics[0].dirPath).toBe("org/solo");
    expect(single.subEpics[0].stories).toHaveLength(1);
  });

  it("backfills sourceRepo in nested sub-epic stories", () => {
    const scans: RepoScanResult[] = [
      {
        repoSlug: "org/deep",
        tree: buildTree("", {
          subEpics: [
            buildTree("epic", {
              dirPath: "epic",
              stories: [buildStoryRef("nested.md")],
            }),
          ],
        }),
      },
    ];
    const merged = mergePipelineTrees(scans);
    const nestedStory = merged.subEpics[0].subEpics[0].stories[0];
    expect(nestedStory.sourceRepo).toBe("org/deep");
  });
});

describe("filterByTeam", () => {
  it("prunes stories that don't match the active team", () => {
    const tree = buildTree("", {
      stories: [
        buildStoryRef("a.md", { team: "alpha" }),
        buildStoryRef("b.md", { team: "beta" }),
        buildStoryRef("c.md", { team: "alpha" }),
      ],
    });
    const filtered = filterByTeam(tree, "alpha");
    expect(filtered.stories).toHaveLength(2);
    expect(filtered.stories.map((s) => s.fileName)).toEqual(["a.md", "c.md"]);
  });

  it("passes through stories with null team (backward compat)", () => {
    const tree = buildTree("", {
      stories: [buildStoryRef("a.md", { team: null }), buildStoryRef("b.md", { team: "alpha" })],
    });
    const filtered = filterByTeam(tree, "alpha");
    expect(filtered.stories).toHaveLength(2);
  });

  it("recurses through sub-epics", () => {
    const tree = buildTree("", {
      subEpics: [
        buildTree("epic", {
          dirPath: "epic",
          stories: [
            buildStoryRef("inner.md", { team: "alpha" }),
            buildStoryRef("other.md", { team: "beta" }),
          ],
        }),
      ],
    });
    const filtered = filterByTeam(tree, "alpha");
    expect(filtered.subEpics[0].stories).toHaveLength(1);
    expect(filtered.subEpics[0].stories[0].fileName).toBe("inner.md");
  });

  it("returns empty stories when none match", () => {
    const tree = buildTree("", {
      stories: [buildStoryRef("a.md", { team: "other" })],
    });
    const filtered = filterByTeam(tree, "alpha");
    expect(filtered.stories).toHaveLength(0);
  });
});
