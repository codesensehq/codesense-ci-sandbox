// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { derivedTreeToReactFlow } from "./derived-tree-to-react-flow";
import { buildFullGroupIndex } from "./build-group-index";
import { findScopedSubtree } from "./pipeline-utils";
import type { GroupNodeData, ResolvedDep } from "./types";
import {
  buildTree,
  buildStoryRef,
  fixtureEmptyTree,
  fixtureTwoStoriesNoOrder,
  fixtureNestedEpics,
  fixtureThreeLevelsDeep,
} from "./test-fixtures/derived-tree";

describe("derivedTreeToReactFlow", () => {
  it("emits nothing for an empty scope root", () => {
    const { nodes, edges } = derivedTreeToReactFlow(fixtureEmptyTree);
    expect(nodes).toEqual([]);
    expect(edges).toEqual([]);
  });

  it("emits two story nodes at top level for scope root with two stories, no order", () => {
    const { nodes, edges } = derivedTreeToReactFlow(fixtureTwoStoriesNoOrder);
    expect(nodes).toHaveLength(2);
    for (const n of nodes) {
      expect(n.type).toBe("story");
      expect(n.parentId).toBeUndefined();
    }
    expect(nodes.map((n) => n.id).sort()).toEqual(["1-alpha.md", "2-beta.md"]);
    expect(edges).toEqual([]);
  });

  it("assigns increasing depth to nested groups starting at 0 below the scope root", () => {
    // Top-level epic = depth 0, its sub-epic = depth 1, sub-sub = 2…
    // PipelineGroupNode alternates bg color by depth parity, so this
    // is the load-bearing piece behind the "nested epics look
    // different from their parents" UX.
    const tree = buildTree("", {
      subEpics: [
        buildTree("epic-a", {
          dirPath: "epic-a",
          subEpics: [
            buildTree("epic-a/sub1", {
              dirPath: "epic-a/sub1",
              subEpics: [
                buildTree("epic-a/sub1/deep", {
                  dirPath: "epic-a/sub1/deep",
                  stories: [
                    buildStoryRef("leaf.md", {
                      absolutePath: "/ctx/pipe/epic-a/sub1/deep/leaf.md",
                    }),
                  ],
                }),
              ],
            }),
          ],
        }),
      ],
    });
    const { nodes } = derivedTreeToReactFlow(tree);
    const depthOf = (id: string) =>
      (nodes.find((n) => n.id === id)!.data as { depth?: number }).depth;
    expect(depthOf("group:epic-a")).toBe(0);
    expect(depthOf("group:epic-a/sub1")).toBe(1);
    expect(depthOf("group:epic-a/sub1/deep")).toBe(2);
  });

  it("populates GroupNodeData.absoluteDirPath from the derived tree", () => {
    // Drill-in affordance needs the absolute path for each emitted
    // group so the canvas can hand it to `onDrillIn`. Verify the
    // transform threads it through from the scanner's output.
    const tree = buildTree("", {
      subEpics: [
        buildTree("epic-a", {
          dirPath: "epic-a",
          absoluteDirPath: "/abs/repo/pipeline/epic-a",
          stories: [buildStoryRef("a.md", { absolutePath: "/abs/repo/pipeline/epic-a/a.md" })],
        }),
      ],
    });
    const { nodes } = derivedTreeToReactFlow(tree);
    const group = nodes.find((n) => n.id === "group:epic-a");
    expect(group).toBeDefined();
    expect((group!.data as { absoluteDirPath?: string }).absoluteDirPath).toBe(
      "/abs/repo/pipeline/epic-a",
    );
  });

  it("populates GroupNodeData.dirPath (pipeline-root-relative)", () => {
    const tree = buildTree("", {
      subEpics: [
        buildTree("epic-a", {
          dirPath: "in-progress/epic-a",
          stories: [buildStoryRef("a.md", { absolutePath: "/ctx/pipe/in-progress/epic-a/a.md" })],
        }),
      ],
    });
    const { nodes } = derivedTreeToReactFlow(tree);
    const group = nodes.find((n) => n.id === "group:in-progress/epic-a");
    expect(group).toBeDefined();
    expect((group!.data as { dirPath?: string }).dirPath).toBe("in-progress/epic-a");
  });

  it("scope root pseudo-epic is invisible (no 'group:' root node)", () => {
    const tree = buildTree("", {
      stories: [buildStoryRef("root.md")],
      subEpics: [
        buildTree("epic-a", {
          dirPath: "epic-a",
          stories: [buildStoryRef("a.md", { absolutePath: "/ctx/pipe/epic-a/a.md" })],
        }),
      ],
    });
    const { nodes } = derivedTreeToReactFlow(tree);
    expect(nodes.find((n) => n.id === "group:")).toBeUndefined();
    expect(nodes.find((n) => n.id === "group:epic-a")).toBeDefined();
  });

  it("story node data.storyFile is always absolute (AC-21 invariant)", () => {
    const { nodes } = derivedTreeToReactFlow(fixtureNestedEpics);
    const storyNodes = nodes.filter((n) => n.type === "story");
    expect(storyNodes.length).toBeGreaterThan(0);
    for (const n of storyNodes) {
      const sf = (n.data as { storyFile?: string }).storyFile;
      expect(sf).toBeDefined();
      expect(sf!.startsWith("/")).toBe(true);
    }
  });

  it("story node title falls back to fileName.replace(/\\.md$/i, '')", () => {
    const tree = buildTree("", { stories: [buildStoryRef("no-title.md")] });
    const { nodes } = derivedTreeToReactFlow(tree);
    expect((nodes[0].data as { title: string }).title).toBe("no-title");
  });

  it("story node title uses StoryRef.title when present", () => {
    const tree = buildTree("", {
      stories: [buildStoryRef("x.md", { title: "Custom Title" })],
    });
    const { nodes } = derivedTreeToReactFlow(tree);
    expect((nodes[0].data as { title: string }).title).toBe("Custom Title");
  });

  it("story nodes are parented to their group", () => {
    const epic = buildTree("epic-a", {
      dirPath: "epic-a",
      stories: [buildStoryRef("a.md"), buildStoryRef("b.md")],
    });
    const tree = buildTree("", { subEpics: [epic] });
    const { nodes } = derivedTreeToReactFlow(tree);
    expect(nodes.find((n) => n.id === "group:epic-a")).toBeDefined();

    const a = nodes.find((n) => n.id === "epic-a/a.md");
    expect(a?.parentId).toBe("group:epic-a");
    const b = nodes.find((n) => n.id === "epic-a/b.md");
    expect(b?.parentId).toBe("group:epic-a");
  });

  it("epic without ordering has no subtitle (P.3 AC-1)", () => {
    const epic = buildTree("epic-a", {
      dirPath: "epic-a",
      stories: [buildStoryRef("a.md")],
    });
    const tree = buildTree("", { subEpics: [epic] });
    const { nodes } = derivedTreeToReactFlow(tree);
    const group = nodes.find((n) => n.id === "group:epic-a");
    const data = group!.data as { subtitle?: string };
    expect(data.subtitle).toBeUndefined();
  });

  it("epic with order.steps = [] emits no edges (AC-26)", () => {
    const epic = buildTree("epic-a", {
      dirPath: "epic-a",
      stories: [buildStoryRef("a.md")],
    });
    const tree = buildTree("", { subEpics: [epic] });
    const { edges } = derivedTreeToReactFlow(tree);
    expect(edges).toHaveLength(0);
  });

  it("empty group (no actionable stories, no sub-epics) is not emitted (P.12 merge)", () => {
    const empty = buildTree("epic-a", { dirPath: "epic-a" });
    const tree = buildTree("", { subEpics: [empty] });
    const { nodes } = derivedTreeToReactFlow(tree);
    expect(nodes.find((n) => n.id === "group:epic-a")).toBeUndefined();
  });

  it("group with only non-actionable docs and no sub-epics is not emitted (P.12 merge)", () => {
    const docOnly = buildTree("epic-a", {
      dirPath: "epic-a",
      stories: [
        buildStoryRef("notes.md", {
          documentType: "doc",
          absolutePath: "/ctx/pipe/epic-a/notes.md",
        }),
      ],
    });
    const tree = buildTree("", { subEpics: [docOnly] });
    const { nodes } = derivedTreeToReactFlow(tree);
    expect(nodes.find((n) => n.id === "group:epic-a")).toBeUndefined();
  });

  it("orderError is propagated onto the group node data", () => {
    const epic = buildTree("epic-a", {
      dirPath: "epic-a",
      stories: [buildStoryRef("a.md")],
      orderError: "Invalid JSON: expected object",
    });
    const tree = buildTree("", { subEpics: [epic] });
    const { nodes } = derivedTreeToReactFlow(tree);
    const group = nodes.find((n) => n.id === "group:epic-a")!;
    expect((group.data as { orderError?: string }).orderError).toBe(
      "Invalid JSON: expected object",
    );
  });

  it("handles three-level-deep nesting correctly", () => {
    const { nodes } = derivedTreeToReactFlow(fixtureThreeLevelsDeep);
    // group:level-1, group:level-1/level-2, group:level-1/level-2/level-3, leaf
    const ids = new Set(nodes.map((n) => n.id));
    expect(ids.has("group:level-1")).toBe(true);
    expect(ids.has("group:level-1/level-2")).toBe(true);
    expect(ids.has("group:level-1/level-2/level-3")).toBe(true);
    expect(ids.has("level-1/level-2/level-3/leaf.md")).toBe(true);

    const leaf = nodes.find((n) => n.id === "level-1/level-2/level-3/leaf.md");
    expect(leaf?.parentId).toBe("group:level-1/level-2/level-3");
    const mid = nodes.find((n) => n.id === "group:level-1/level-2/level-3");
    expect(mid?.parentId).toBe("group:level-1/level-2");
  });

  it("drilling into a sub-directory scope uses scope-relative ids", () => {
    // When the scanner is called with scopePath="epic-a", tree.dirPath = "epic-a".
    const scoped = buildTree("epic-a", {
      dirPath: "epic-a",
      stories: [buildStoryRef("a.md")],
    });
    const { nodes } = derivedTreeToReactFlow(scoped);
    // Scope root invisible; story emits at top-level with bare id.
    expect(nodes).toHaveLength(1);
    expect(nodes[0].id).toBe("a.md");
    expect(nodes[0].parentId).toBeUndefined();
  });

  // ---------------------------------------------------------------------------
  // Story 30.9: frontmatter-mode edge derivation
  // ---------------------------------------------------------------------------

  describe("frontmatter mode (dependsOn)", () => {
    it("emits one edge per (predecessor, story) pair from dependsOn (linear chain)", () => {
      const tree = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [
          buildStoryRef("a.md"),
          buildStoryRef("b.md", { dependsOn: ["a"] }),
          buildStoryRef("c.md", { dependsOn: ["b"] }),
        ],
      });
      const { edges } = derivedTreeToReactFlow(tree);
      const pairs = edges.map((e) => [e.source, e.target]);
      expect(pairs).toEqual(
        expect.arrayContaining([
          ["a.md", "b.md"],
          ["b.md", "c.md"],
        ]),
      );
      expect(edges).toHaveLength(2);
    });

    it("supports fan-out (one predecessor → multiple downstream stories)", () => {
      const tree = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [
          buildStoryRef("a.md"),
          buildStoryRef("b.md", { dependsOn: ["a"] }),
          buildStoryRef("c.md", { dependsOn: ["a"] }),
        ],
      });
      const { edges } = derivedTreeToReactFlow(tree);
      const pairs = edges.map((e) => [e.source, e.target]);
      expect(pairs).toContainEqual(["a.md", "b.md"]);
      expect(pairs).toContainEqual(["a.md", "c.md"]);
      expect(edges).toHaveLength(2);
    });

    it("supports fan-in (one story with multiple dependsOn entries)", () => {
      const tree = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [
          buildStoryRef("a.md"),
          buildStoryRef("b.md"),
          buildStoryRef("c.md", { dependsOn: ["a", "b"] }),
        ],
      });
      const { edges } = derivedTreeToReactFlow(tree);
      const pairs = edges.map((e) => [e.source, e.target]);
      expect(pairs).toContainEqual(["a.md", "c.md"]);
      expect(pairs).toContainEqual(["b.md", "c.md"]);
      expect(edges).toHaveLength(2);
    });

    it("drops unresolved references silently (no edge, no error)", () => {
      const tree = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [
          buildStoryRef("a.md"),
          // References a non-existent sibling — dropped.
          buildStoryRef("b.md", { dependsOn: ["does-not-exist"] }),
        ],
      });
      const { edges } = derivedTreeToReactFlow(tree);
      expect(edges).toEqual([]);
    });

    it("accepts a `.md`-suffixed reference defensively", () => {
      const tree = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [buildStoryRef("a.md"), buildStoryRef("b.md", { dependsOn: ["a.md"] })],
      });
      const { edges } = derivedTreeToReactFlow(tree);
      expect(edges).toHaveLength(1);
      expect(edges[0].source).toBe("a.md");
      expect(edges[0].target).toBe("b.md");
    });

    it("group with frontmatter edges has no subtitle (P.3 AC-1)", () => {
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [buildStoryRef("a.md"), buildStoryRef("b.md", { dependsOn: ["a"] })],
          }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const group = nodes.find((n) => n.id === "group:epic-a");
      expect((group!.data as { subtitle?: string }).subtitle).toBeUndefined();
    });

    it("emits edge ids matching `${source}->${target}` convention", () => {
      const tree = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [buildStoryRef("a.md"), buildStoryRef("b.md", { dependsOn: ["a"] })],
      });
      const { edges } = derivedTreeToReactFlow(tree);
      expect(edges[0].id).toBe("a.md->b.md");
    });

    it("sub-epic frontmatter edges don't escape to parent scope", () => {
      // Inner sub-epic uses frontmatter mode; outer scope has no edges.
      const tree = buildTree("", {
        stories: [buildStoryRef("outer.md")],
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [
              buildStoryRef("a.md", {
                absolutePath: "/ctx/pipe/epic-a/a.md",
              }),
              buildStoryRef("b.md", {
                absolutePath: "/ctx/pipe/epic-a/b.md",
                dependsOn: ["a"],
              }),
            ],
          }),
        ],
      });
      const { edges } = derivedTreeToReactFlow(tree);
      const pairs = edges.map((e) => [e.source, e.target]);
      // Inner frontmatter-mode edge only.
      expect(pairs).toContainEqual(["epic-a/a.md", "epic-a/b.md"]);
      expect(edges).toHaveLength(1);
    });

    it("empty dependsOn arrays produce no edges", () => {
      const tree = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [
          buildStoryRef("a.md", { dependsOn: [] }),
          buildStoryRef("b.md", { dependsOn: [] }),
        ],
      });
      const { edges } = derivedTreeToReactFlow(tree);
      expect(edges).toHaveLength(0);
    });

    it("null dependsOn on every story produces no edges", () => {
      const tree = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [buildStoryRef("a.md"), buildStoryRef("b.md")],
      });
      const { edges } = derivedTreeToReactFlow(tree);
      expect(edges).toHaveLength(0);
    });

    // --- Post-review hardening (code review round 1) ----------------------

    it("drops self-references (story listing its own stem in dependsOn)", () => {
      // A typo where a story depends on itself produces `a→a` if not
      // guarded — dagre lays self-loops awkwardly and the edge carries
      // no meaning.
      const tree = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [
          buildStoryRef("a.md", { dependsOn: ["a"] }),
          buildStoryRef("b.md", { dependsOn: ["a"] }),
        ],
      });
      const { edges } = derivedTreeToReactFlow(tree);
      const pairs = edges.map((e) => [e.source, e.target]);
      expect(pairs).not.toContainEqual(["a.md", "a.md"]);
      expect(pairs).toContainEqual(["a.md", "b.md"]);
      expect(edges).toHaveLength(1);
    });

    it("dedupes duplicate references after stem normalization", () => {
      // `depends_on: [a, a.md]` would otherwise emit two edges with the
      // same id `a.md->b.md` — React Flow warns on duplicate ids.
      const tree = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [buildStoryRef("a.md"), buildStoryRef("b.md", { dependsOn: ["a", "a.md", "a"] })],
      });
      const { edges } = derivedTreeToReactFlow(tree);
      expect(edges).toHaveLength(1);
      expect(edges[0].id).toBe("a.md->b.md");
    });

    it("orderError always propagates regardless of frontmatter edges (P.3)", () => {
      // After P.3, orderError always surfaces — it indicates a malformed
      // pipeline.json file, which is useful feedback regardless of whether
      // frontmatter edges are also present.
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [buildStoryRef("a.md"), buildStoryRef("b.md", { dependsOn: ["a"] })],
            orderError: "Invalid JSON: missing 'steps' field",
          }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const group = nodes.find((n) => n.id === "group:epic-a");
      expect((group!.data as { orderError?: string }).orderError).toBe(
        "Invalid JSON: missing 'steps' field",
      );
    });

    it("emits frontmatter edges at the scope root (no enclosing group)", () => {
      // Every other frontmatter-mode test puts stories inside a named
      // sub-epic. The pseudo scope root is exercised separately so a
      // regression that gated frontmatter mode on `!isPseudoScopeRoot`
      // would surface here.
      const tree = buildTree("", {
        stories: [buildStoryRef("a.md"), buildStoryRef("b.md", { dependsOn: ["a"] })],
      });
      const { nodes, edges } = derivedTreeToReactFlow(tree);
      // No group node for the scope root.
      expect(nodes.find((n) => n.id === "group:")).toBeUndefined();
      // Story nodes use bare ids (scope-relative path is empty).
      expect(edges).toHaveLength(1);
      expect(edges[0].source).toBe("a.md");
      expect(edges[0].target).toBe("b.md");
    });
  });

  // ---------------------------------------------------------------------------
  // Story 34.4: status and storyText flow
  // ---------------------------------------------------------------------------

  describe("status and storyText (34.4)", () => {
    it("uses frontmatter status when valid", () => {
      const tree = buildTree("", {
        stories: [buildStoryRef("a.md", { status: "review" })],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      expect((nodes[0].data as { status: string }).status).toBe("review");
    });

    it("falls back to the taxonomy default (planning) for null status", () => {
      const tree = buildTree("", {
        stories: [buildStoryRef("a.md")],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      expect((nodes[0].data as { status: string }).status).toBe("planning");
    });

    it("falls back to the taxonomy default (planning) for invalid status string", () => {
      const tree = buildTree("", {
        stories: [buildStoryRef("a.md", { status: "bogus-status" })],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      expect((nodes[0].data as { status: string }).status).toBe("planning");
    });

    it("validates all 7 PipelineStatus values (including blocked)", () => {
      const statuses = ["planning", "todo", "in-progress", "blocked", "review", "qa", "done"];
      for (const s of statuses) {
        const tree = buildTree("", {
          stories: [buildStoryRef("a.md", { status: s })],
        });
        const { nodes } = derivedTreeToReactFlow(tree);
        expect((nodes[0].data as { status: string }).status).toBe(s);
      }
    });

    it("populates storyText when present", () => {
      const tree = buildTree("", {
        stories: [buildStoryRef("a.md", { storyText: "As a user, I want X." })],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      expect((nodes[0].data as { storyText?: string }).storyText).toBe("As a user, I want X.");
    });

    it("omits storyText when null", () => {
      const tree = buildTree("", {
        stories: [buildStoryRef("a.md")],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      expect((nodes[0].data as { storyText?: string }).storyText).toBeUndefined();
    });
  });

  // ----- M.4: sourceRepo ----------------------------------------------------

  describe("sourceRepo (M.4)", () => {
    it("populates sourceRepo when present", () => {
      const tree = buildTree("", {
        stories: [buildStoryRef("a.md", { sourceRepo: "alderco/codesense" })],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      expect((nodes[0].data as { sourceRepo?: string }).sourceRepo).toBe("alderco/codesense");
    });

    it("omits sourceRepo when null", () => {
      const tree = buildTree("", {
        stories: [buildStoryRef("a.md")],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      expect((nodes[0].data as { sourceRepo?: string }).sourceRepo).toBeUndefined();
    });
  });

  // ----- F.3: documentType --------------------------------------------------

  describe("documentType (F.3)", () => {
    it("defaults to 'story' when scanner returns null", () => {
      const tree = buildTree("", {
        stories: [buildStoryRef("a.md")],
      });
      const { nodes, unknownDocumentTypes } = derivedTreeToReactFlow(tree);
      expect((nodes[0].data as { documentType: string }).documentType).toBe("story");
      expect(unknownDocumentTypes).toEqual([]);
    });

    it("passes through known actionable document types (bug, spike)", () => {
      const tree = buildTree("", {
        stories: [
          buildStoryRef("bug.md", { documentType: "bug" }),
          buildStoryRef("spike.md", { documentType: "spike" }),
        ],
      });
      const { nodes, unknownDocumentTypes } = derivedTreeToReactFlow(tree);
      const types = nodes.map((n) => (n.data as { documentType: string }).documentType);
      expect(types).toEqual(["bug", "spike"]);
      expect(unknownDocumentTypes).toEqual([]);
    });

    it("non-actionable types (doc, retro) are excluded from nodes but still normalized", () => {
      const tree = buildTree("", {
        stories: [
          buildStoryRef("story.md"),
          buildStoryRef("doc.md", { documentType: "doc" }),
          buildStoryRef("retro.md", { documentType: "retro" }),
        ],
      });
      const { nodes, unknownDocumentTypes } = derivedTreeToReactFlow(tree);
      // Only story remains as a node; doc and retro are non-actionable.
      expect(nodes).toHaveLength(1);
      expect(unknownDocumentTypes).toEqual([]);
    });

    it("normalizes unrecognized type to 'doc' and collects unknown (non-actionable, excluded)", () => {
      const tree = buildTree("", {
        stories: [buildStoryRef("story.md"), buildStoryRef("a.md", { documentType: "research" })],
      });
      const { nodes, unknownDocumentTypes } = derivedTreeToReactFlow(tree);
      // "research" normalizes to "doc" which is non-actionable — excluded from nodes.
      expect(nodes).toHaveLength(1);
      expect(unknownDocumentTypes).toEqual(["research"]);
    });

    it("treats empty string type as 'story'", () => {
      const tree = buildTree("", {
        stories: [buildStoryRef("a.md", { documentType: "" })],
      });
      const { nodes, unknownDocumentTypes } = derivedTreeToReactFlow(tree);
      expect((nodes[0].data as { documentType: string }).documentType).toBe("story");
      expect(unknownDocumentTypes).toEqual([]);
    });

    it("treats whitespace-only type as 'story'", () => {
      const tree = buildTree("", {
        stories: [buildStoryRef("a.md", { documentType: "   " })],
      });
      const { nodes, unknownDocumentTypes } = derivedTreeToReactFlow(tree);
      expect((nodes[0].data as { documentType: string }).documentType).toBe("story");
      expect(unknownDocumentTypes).toEqual([]);
    });

    it("normalizes capitalized type values case-insensitively", () => {
      const tree = buildTree("", {
        stories: [
          buildStoryRef("a.md", { documentType: "Bug" }),
          buildStoryRef("b.md", { documentType: "SPIKE" }),
          buildStoryRef("c.md", { documentType: "Story" }),
        ],
      });
      const { nodes, unknownDocumentTypes } = derivedTreeToReactFlow(tree);
      const types = nodes.map((n) => (n.data as { documentType: string }).documentType);
      expect(types).toEqual(["bug", "spike", "story"]);
      expect(unknownDocumentTypes).toEqual([]);
    });

    it("trims whitespace-padded type values", () => {
      const tree = buildTree("", {
        stories: [buildStoryRef("a.md", { documentType: "  bug  " })],
      });
      const { nodes, unknownDocumentTypes } = derivedTreeToReactFlow(tree);
      expect((nodes[0].data as { documentType: string }).documentType).toBe("bug");
      expect(unknownDocumentTypes).toEqual([]);
    });

    it("deduplicates unknown types in the returned list", () => {
      const tree = buildTree("", {
        stories: [
          buildStoryRef("a.md", { documentType: "custom" }),
          buildStoryRef("b.md", { documentType: "custom" }),
        ],
      });
      const { unknownDocumentTypes } = derivedTreeToReactFlow(tree);
      expect(unknownDocumentTypes).toEqual(["custom"]);
    });

    it("document with depends_on produces edges", () => {
      const tree = buildTree("", {
        stories: [
          buildStoryRef("story-a.md"),
          buildStoryRef("bug-b.md", {
            documentType: "bug",
            dependsOn: ["story-a"],
          }),
        ],
      });
      const { edges } = derivedTreeToReactFlow(tree);
      expect(edges).toHaveLength(1);
      expect(edges[0].source).toBe("story-a.md");
      expect(edges[0].target).toBe("bug-b.md");
    });
  });

  // ---------------------------------------------------------------------------
  // Story F.4: supporting docs resolution and exclusion
  // ---------------------------------------------------------------------------

  describe("supporting docs (F.4)", () => {
    it("resolves supporting_docs and attaches resolvedSupportingDocs to story node", () => {
      const tree = buildTree("", {
        stories: [
          buildStoryRef("story.md", { supportingDocs: ["./bug.md"] }),
          buildStoryRef("bug.md", { documentType: "bug" }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const story = nodes.find((n) => n.id === "story.md");
      const data = story!.data as { resolvedSupportingDocs?: unknown[] };
      expect(data.resolvedSupportingDocs).toHaveLength(1);
      expect(data.resolvedSupportingDocs![0]).toEqual(
        expect.objectContaining({ fileName: "bug.md", documentType: "bug" }),
      );
    });

    it("excludes referenced non-story docs from node emission", () => {
      const tree = buildTree("", {
        stories: [
          buildStoryRef("story.md", { supportingDocs: ["./bug.md"] }),
          buildStoryRef("bug.md", { documentType: "bug" }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      expect(nodes.find((n) => n.id === "bug.md")).toBeUndefined();
      expect(nodes).toHaveLength(1);
      expect(nodes[0].id).toBe("story.md");
    });

    // SW-SC-7: images resolve as first-class supporting docs from node.docs[]
    // (not node.stories[]), with documentType synthesized from the extension.
    it("resolves a same-dir image supporting doc from docs[] (SW-SC-7 AC-2)", () => {
      const tree = buildTree("", {
        stories: [buildStoryRef("story.md", { supportingDocs: ["./shot.png"] })],
        docs: [{ fileName: "shot.png", filePath: "/ctx/pipe/shot.png" }],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const story = nodes.find((n) => n.id === "story.md");
      const data = story!.data as { resolvedSupportingDocs?: Array<{ documentType: string }> };
      expect(data.resolvedSupportingDocs).toHaveLength(1);
      expect(data.resolvedSupportingDocs![0]).toEqual(
        expect.objectContaining({ fileName: "shot.png", documentType: "image" }),
      );
    });

    it("resolves a cross-dir image supporting doc (SW-SC-7 AC-2)", () => {
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            absoluteDirPath: "/ctx/pipe/epic-a",
            stories: [
              buildStoryRef("story.md", {
                absolutePath: "/ctx/pipe/epic-a/story.md",
                supportingDocs: ["../assets/diagram.svg"],
              }),
            ],
          }),
          buildTree("assets", {
            dirPath: "assets",
            absoluteDirPath: "/ctx/pipe/assets",
            docs: [{ fileName: "diagram.svg", filePath: "/ctx/pipe/assets/diagram.svg" }],
          }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      // Nested story node id is scope-qualified: `${ownerScopeRel}/${fileName}`.
      const story = nodes.find((n) => n.id === "epic-a/story.md");
      const data = story!.data as { resolvedSupportingDocs?: Array<{ documentType: string }> };
      expect(data.resolvedSupportingDocs).toHaveLength(1);
      expect(data.resolvedSupportingDocs![0]).toEqual(
        expect.objectContaining({ fileName: "diagram.svg", documentType: "image" }),
      );
    });

    it("never resolves a derived *.excalidraw.png render as a supporting doc (SW-SC-7 AC-9)", () => {
      // The Rust scanner excludes `*.excalidraw.png` from docs[] (is_supporting_doc),
      // so a story referencing it finds nothing to resolve → unresolved, not surfaced.
      const tree = buildTree("", {
        stories: [buildStoryRef("story.md", { supportingDocs: ["./design.excalidraw.png"] })],
        // docs[] is empty: the scanner would have filtered the derived render out.
      });
      const { nodes, unresolvedSupportingDocRefs } = derivedTreeToReactFlow(tree);
      const story = nodes.find((n) => n.id === "story.md");
      const data = story!.data as { resolvedSupportingDocs?: unknown[] };
      expect(data.resolvedSupportingDocs).toBeUndefined();
      expect(unresolvedSupportingDocRefs).toContain("design.excalidraw.png");
    });

    it("handles multiple supporting docs on a single story", () => {
      const tree = buildTree("", {
        stories: [
          buildStoryRef("story.md", {
            supportingDocs: ["./bug.md", "./spike.md"],
          }),
          buildStoryRef("bug.md", { documentType: "bug" }),
          buildStoryRef("spike.md", { documentType: "spike" }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      // Only the story node remains.
      expect(nodes).toHaveLength(1);
      const data = nodes[0].data as { resolvedSupportingDocs?: unknown[] };
      expect(data.resolvedSupportingDocs).toHaveLength(2);
    });

    it("same doc referenced by multiple stories appears in both popovers", () => {
      const tree = buildTree("", {
        stories: [
          buildStoryRef("story-a.md", { supportingDocs: ["./bug.md"] }),
          buildStoryRef("story-b.md", { supportingDocs: ["./bug.md"] }),
          buildStoryRef("bug.md", { documentType: "bug" }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      // bug.md excluded; two story nodes remain.
      expect(nodes).toHaveLength(2);
      for (const n of nodes) {
        const data = n.data as { resolvedSupportingDocs?: unknown[] };
        expect(data.resolvedSupportingDocs).toHaveLength(1);
      }
    });

    it("collects unresolved references", () => {
      const tree = buildTree("", {
        stories: [buildStoryRef("story.md", { supportingDocs: ["./nonexistent.md"] })],
      });
      const { unresolvedSupportingDocRefs } = derivedTreeToReactFlow(tree);
      // Stored as stripped value so ./ghost.md and ghost.md deduplicate.
      expect(unresolvedSupportingDocRefs).toEqual(["nonexistent.md"]);
    });

    it("ignores story-type references in supporting_docs", () => {
      const tree = buildTree("", {
        stories: [
          buildStoryRef("story-a.md", { supportingDocs: ["./story-b.md"] }),
          buildStoryRef("story-b.md"), // type defaults to "story"
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      // story-b is NOT excluded (it's story-type).
      expect(nodes).toHaveLength(2);
      const storyA = nodes.find((n) => n.id === "story-a.md");
      const data = storyA!.data as { resolvedSupportingDocs?: unknown[] };
      expect(data.resolvedSupportingDocs).toBeUndefined();
    });

    it("accepts bare filename without ./ prefix", () => {
      const tree = buildTree("", {
        stories: [
          buildStoryRef("story.md", { supportingDocs: ["bug.md"] }),
          buildStoryRef("bug.md", { documentType: "bug" }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      expect(nodes).toHaveLength(1);
      const data = nodes[0].data as { resolvedSupportingDocs?: unknown[] };
      expect(data.resolvedSupportingDocs).toHaveLength(1);
    });

    it("6.21: cross-directory refs attempt resolution (no longer silently dropped)", () => {
      // P.2: After Task 4, cross-dir refs resolve via groupIndex. When
      // the target directory is NOT in the group index (scope-root stories
      // have no sibling groups), the ref becomes unresolved.
      const tree = buildTree("", {
        stories: [
          buildStoryRef("story.md", {
            supportingDocs: ["../other-epic/bug.md"],
          }),
        ],
      });
      const { nodes, unresolvedSupportingDocRefs } = derivedTreeToReactFlow(tree);
      expect(nodes).toHaveLength(1);
      // Now tracked as unresolved (target dir not in groupIndex).
      expect(unresolvedSupportingDocRefs).toContain("../other-epic/bug.md");
    });

    it("ignores supportingDocs on non-story documents", () => {
      const tree = buildTree("", {
        stories: [
          buildStoryRef("bug.md", {
            documentType: "bug",
            supportingDocs: ["./spike.md"],
          }),
          buildStoryRef("spike.md", { documentType: "spike" }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      // Both remain as nodes — bug's supportingDocs is ignored.
      expect(nodes).toHaveLength(2);
    });

    it("deduplicates repeated refs within a single story's supportingDocs", () => {
      const tree = buildTree("", {
        stories: [
          buildStoryRef("story.md", {
            supportingDocs: ["./bug.md", "bug.md", "./bug.md"],
          }),
          buildStoryRef("bug.md", { documentType: "bug" }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const data = nodes[0].data as {
        resolvedSupportingDocs?: Array<{ fileName: string }>;
      };
      // Three refs to the same file collapse to one entry.
      expect(data.resolvedSupportingDocs).toHaveLength(1);
      expect(data.resolvedSupportingDocs![0].fileName).toBe("bug.md");
    });

    it("empty supportingDocs array produces no badge", () => {
      const tree = buildTree("", {
        stories: [buildStoryRef("story.md", { supportingDocs: [] })],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const data = nodes[0].data as { resolvedSupportingDocs?: unknown[] };
      expect(data.resolvedSupportingDocs).toBeUndefined();
    });

    it("preserves frontmatter declaration order in resolvedSupportingDocs", () => {
      const tree = buildTree("", {
        stories: [
          buildStoryRef("story.md", {
            supportingDocs: ["./retro.md", "./bug.md", "./spike.md"],
          }),
          buildStoryRef("bug.md", { documentType: "bug" }),
          buildStoryRef("spike.md", { documentType: "spike" }),
          buildStoryRef("retro.md", { documentType: "retro" }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const data = nodes[0].data as {
        resolvedSupportingDocs?: Array<{ fileName: string }>;
      };
      expect(data.resolvedSupportingDocs!.map((d) => d.fileName)).toEqual([
        "retro.md",
        "bug.md",
        "spike.md",
      ]);
    });

    it("unreferenced actionable docs (bug, spike) remain as standalone nodes", () => {
      const tree = buildTree("", {
        stories: [
          buildStoryRef("story.md", { supportingDocs: ["./bug.md"] }),
          buildStoryRef("bug.md", { documentType: "bug" }),
          buildStoryRef("orphan-spike.md", { documentType: "spike" }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      // story.md + orphan-spike.md remain; bug.md excluded (supporting doc).
      expect(nodes).toHaveLength(2);
      expect(nodes.find((n) => n.id === "orphan-spike.md")).toBeDefined();
    });

    it("non-actionable types (doc, retro) are excluded from graph entirely", () => {
      const tree = buildTree("", {
        stories: [
          buildStoryRef("story.md"),
          buildStoryRef("notes.md", { documentType: "doc" }),
          buildStoryRef("retro.md", { documentType: "retro" }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      // Only story.md remains — doc and retro are non-actionable.
      expect(nodes).toHaveLength(1);
      expect(nodes[0].id).toBe("story.md");
    });

    it("actionable types (bug, spike) render as standalone nodes with type icons", () => {
      const tree = buildTree("", {
        stories: [
          buildStoryRef("story.md"),
          buildStoryRef("bug.md", { documentType: "bug" }),
          buildStoryRef("spike.md", { documentType: "spike" }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      expect(nodes).toHaveLength(3);
      const bugNode = nodes.find((n) => n.id === "bug.md");
      expect((bugNode!.data as { documentType: string }).documentType).toBe("bug");
      const spikeNode = nodes.find((n) => n.id === "spike.md");
      expect((spikeNode!.data as { documentType: string }).documentType).toBe("spike");
    });
  });

  // ---------------------------------------------------------------------------
  // SW-SC-2: `.excalidraw` supporting docs resolve from docs[] as type "canvas"
  // ---------------------------------------------------------------------------

  describe("supporting docs — .excalidraw canvases (SW-SC-2)", () => {
    it("resolves a same-dir .excalidraw ref from docs[] with documentType 'canvas'", () => {
      const tree = buildTree("", {
        stories: [buildStoryRef("story.md", { supportingDocs: ["./design.excalidraw"] })],
        docs: [{ fileName: "design.excalidraw", filePath: "/ctx/pipe/design.excalidraw" }],
      });
      const { nodes, unresolvedSupportingDocRefs } = derivedTreeToReactFlow(tree);
      const story = nodes.find((n) => n.id === "story.md");
      const data = story!.data as {
        resolvedSupportingDocs?: Array<{
          fileName: string;
          filePath: string;
          documentType: string;
        }>;
      };
      expect(data.resolvedSupportingDocs).toHaveLength(1);
      expect(data.resolvedSupportingDocs![0]).toEqual({
        fileName: "design.excalidraw",
        filePath: "/ctx/pipe/design.excalidraw",
        documentType: "canvas",
      });
      expect(unresolvedSupportingDocRefs).toEqual([]);
    });

    it("accepts a bare (no ./) .excalidraw ref", () => {
      const tree = buildTree("", {
        stories: [buildStoryRef("story.md", { supportingDocs: ["design.excalidraw"] })],
        docs: [{ fileName: "design.excalidraw", filePath: "/ctx/pipe/design.excalidraw" }],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const data = nodes[0].data as { resolvedSupportingDocs?: unknown[] };
      expect(data.resolvedSupportingDocs).toHaveLength(1);
    });

    it("resolves a cross-directory .excalidraw ref via the group index", () => {
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [
              buildStoryRef("story.md", {
                absolutePath: "/ctx/pipe/epic-a/story.md",
                supportingDocs: ["../epic-b/canvas.excalidraw"],
              }),
            ],
          }),
          buildTree("epic-b", {
            dirPath: "epic-b",
            docs: [
              { fileName: "canvas.excalidraw", filePath: "/ctx/pipe/epic-b/canvas.excalidraw" },
            ],
          }),
        ],
      });
      const { nodes, unresolvedSupportingDocRefs } = derivedTreeToReactFlow(tree);
      const story = nodes.find((n) => n.id === "epic-a/story.md");
      const data = story!.data as {
        resolvedSupportingDocs?: Array<{ fileName: string; documentType: string }>;
      };
      expect(data.resolvedSupportingDocs).toHaveLength(1);
      expect(data.resolvedSupportingDocs![0]).toEqual(
        expect.objectContaining({ fileName: "canvas.excalidraw", documentType: "canvas" }),
      );
      expect(unresolvedSupportingDocRefs).toEqual([]);
    });

    it("a .excalidraw in docs[] is never emitted as a pipeline node", () => {
      const tree = buildTree("", {
        stories: [buildStoryRef("story.md", { supportingDocs: ["./design.excalidraw"] })],
        docs: [{ fileName: "design.excalidraw", filePath: "/ctx/pipe/design.excalidraw" }],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      expect(nodes).toHaveLength(1);
      expect(nodes[0].id).toBe("story.md");
    });
  });

  // ----- P.1: epic-level main doc and supporting docs --------------------
  describe("P.1: epic main doc and supporting docs", () => {
    it("wires purposeText and mainDocFilePath onto the group node when mainDoc is present", () => {
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [buildStoryRef("story.md", { absolutePath: "/ctx/pipe/epic-a/story.md" })],
            mainDoc: {
              filePath: "/ctx/pipe/epic-a/README.md",
              supportingDocs: null,
              purposeText: "This epic does X.",
            },
          }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const gd = nodes.find((n) => n.id === "group:epic-a")!.data as GroupNodeData;
      expect(gd.purposeText).toBe("This epic does X.");
      expect(gd.mainDocFilePath).toBe("/ctx/pipe/epic-a/README.md");
    });

    it("auto-discovers non-actionable docs as epic supporting docs", () => {
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [
              buildStoryRef("story.md", { absolutePath: "/ctx/pipe/epic-a/story.md" }),
              buildStoryRef("notes.md", {
                absolutePath: "/ctx/pipe/epic-a/notes.md",
                documentType: "doc",
              }),
              buildStoryRef("retro.md", {
                absolutePath: "/ctx/pipe/epic-a/retro.md",
                documentType: "retro",
              }),
            ],
            mainDoc: {
              filePath: "/ctx/pipe/epic-a/README.md",
              supportingDocs: null,
              purposeText: null,
            },
          }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const gd = nodes.find((n) => n.id === "group:epic-a")!.data as GroupNodeData;
      const docs = gd.resolvedSupportingDocs;
      expect(docs).toHaveLength(2);
      expect(docs!.map((d: { fileName: string }) => d.fileName).sort()).toEqual([
        "notes.md",
        "retro.md",
      ]);
    });

    it("story-claimed docs do NOT float up to epic (AC-5)", () => {
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [
              buildStoryRef("story.md", {
                absolutePath: "/ctx/pipe/epic-a/story.md",
                supportingDocs: ["notes.md"],
              }),
              buildStoryRef("notes.md", {
                absolutePath: "/ctx/pipe/epic-a/notes.md",
                documentType: "doc",
              }),
              buildStoryRef("unclaimed.md", {
                absolutePath: "/ctx/pipe/epic-a/unclaimed.md",
                documentType: "doc",
              }),
            ],
            mainDoc: {
              filePath: "/ctx/pipe/epic-a/README.md",
              supportingDocs: null,
              purposeText: null,
            },
          }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const gd = nodes.find((n) => n.id === "group:epic-a")!.data as GroupNodeData;
      const docs = gd.resolvedSupportingDocs;
      // notes.md claimed by story → only unclaimed.md on the epic.
      expect(docs).toHaveLength(1);
      expect(docs![0].fileName).toBe("unclaimed.md");
    });

    it("resolves cross-directory supporting docs from main doc frontmatter (AC-6)", () => {
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            absoluteDirPath: "/ctx/pipe/epic-a",
            stories: [buildStoryRef("story.md", { absolutePath: "/ctx/pipe/epic-a/story.md" })],
            mainDoc: {
              filePath: "/ctx/pipe/epic-a/README.md",
              supportingDocs: ["../shared-specs/api-contract.md"],
              purposeText: null,
            },
          }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const gd = nodes.find((n) => n.id === "group:epic-a")!.data as GroupNodeData;
      const docs = gd.resolvedSupportingDocs;
      expect(docs).toHaveLength(1);
      expect(docs![0].fileName).toBe("api-contract.md");
      expect(docs![0].filePath).toBe("/ctx/pipe/shared-specs/api-contract.md");
      expect(docs![0].documentType).toBe("doc");
    });

    it("deduplicates by resolved file path — local + explicit same doc (AC-7)", () => {
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            absoluteDirPath: "/ctx/pipe/epic-a",
            stories: [
              buildStoryRef("story.md", { absolutePath: "/ctx/pipe/epic-a/story.md" }),
              buildStoryRef("notes.md", {
                absolutePath: "/ctx/pipe/epic-a/notes.md",
                documentType: "doc",
              }),
            ],
            mainDoc: {
              filePath: "/ctx/pipe/epic-a/README.md",
              supportingDocs: ["./notes.md"],
              purposeText: null,
            },
          }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const gd = nodes.find((n) => n.id === "group:epic-a")!.data as GroupNodeData;
      const docs = gd.resolvedSupportingDocs;
      // notes.md auto-discovered + explicitly listed → dedup → 1 entry.
      expect(docs).toHaveLength(1);
      expect(docs![0].fileName).toBe("notes.md");
    });

    it("keeps two cross-dir docs with same basename from different dirs", () => {
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            absoluteDirPath: "/ctx/pipe/epic-a",
            stories: [buildStoryRef("story.md", { absolutePath: "/ctx/pipe/epic-a/story.md" })],
            mainDoc: {
              filePath: "/ctx/pipe/epic-a/README.md",
              supportingDocs: ["../specs/overview.md", "../contracts/overview.md"],
              purposeText: null,
            },
          }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const gd = nodes.find((n) => n.id === "group:epic-a")!.data as GroupNodeData;
      const docs = gd.resolvedSupportingDocs;
      // Same basename but different absolute paths → both kept.
      expect(docs).toHaveLength(2);
      const paths = docs!.map((d: { filePath: string }) => d.filePath).sort();
      expect(paths).toEqual(["/ctx/pipe/contracts/overview.md", "/ctx/pipe/specs/overview.md"]);
    });

    it("group node has no supporting docs when mainDoc is absent (AC-9)", () => {
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [
              buildStoryRef("story.md", { absolutePath: "/ctx/pipe/epic-a/story.md" }),
              buildStoryRef("notes.md", {
                absolutePath: "/ctx/pipe/epic-a/notes.md",
                documentType: "doc",
              }),
            ],
            // No mainDoc — legacy behavior.
          }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const gd = nodes.find((n) => n.id === "group:epic-a")!.data as GroupNodeData;
      expect(gd.resolvedSupportingDocs).toBeUndefined();
      expect(gd.purposeText).toBeUndefined();
      expect(gd.mainDocFilePath).toBeUndefined();
    });
  });

  // ---------------------------------------------------------------------------
  // P.2: Cross-epic dependency edges
  // ---------------------------------------------------------------------------

  describe("cross-epic dependency edges (P.2)", () => {
    /** Helper: scope root with two sibling epics. */
    function twoSiblingEpics(opts?: {
      epicAStories?: ReturnType<typeof buildStoryRef>[];
      epicBStories?: ReturnType<typeof buildStoryRef>[];
    }) {
      return buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: opts?.epicAStories ?? [
              buildStoryRef("A-1-alpha.md", {
                title: "Story A.1 — Alpha",
                absolutePath: "/ctx/pipe/epic-a/A-1-alpha.md",
              }),
              buildStoryRef("A-2-bravo.md", {
                title: "Story A.2 — Bravo",
                absolutePath: "/ctx/pipe/epic-a/A-2-bravo.md",
              }),
            ],
          }),
          buildTree("epic-b", {
            dirPath: "epic-b",
            stories: opts?.epicBStories ?? [
              buildStoryRef("B-1-charlie.md", {
                title: "Story B.1 — Charlie",
                absolutePath: "/ctx/pipe/epic-b/B-1-charlie.md",
                dependsOn: ["../epic-a/A-1-alpha"],
              }),
            ],
          }),
        ],
      });
    }

    it("6.1: cross-epic ref emits a solid edge from the sibling epic's group node", () => {
      const tree = twoSiblingEpics();
      const { edges } = derivedTreeToReactFlow(tree);

      const crossEdges = edges.filter((e) => e.type === "crossEpic");
      expect(crossEdges).toHaveLength(1);
      const ce = crossEdges[0];
      expect(ce.source).toBe("group:epic-a");
      expect(ce.target).toBe("group:epic-b");
      expect(ce.type).toBe("crossEpic");
    });

    // --- SW-SB-9: location-independent (story_id) cross-epic resolution -----

    it("SB-9: a bare story_id resolves cross-epic (same edge as the ../path form)", () => {
      // Identical to 6.1 but B-1 depends on the bare story_id "A-1", not a path.
      const tree = twoSiblingEpics({
        epicAStories: [
          buildStoryRef("A-1-alpha.md", {
            storyId: "A-1",
            absolutePath: "/ctx/pipe/epic-a/A-1-alpha.md",
          }),
        ],
        epicBStories: [
          buildStoryRef("B-1-charlie.md", {
            storyId: "B-1",
            absolutePath: "/ctx/pipe/epic-b/B-1-charlie.md",
            dependsOn: ["A-1"],
          }),
        ],
      });
      const { edges } = derivedTreeToReactFlow(tree);
      const crossEdges = edges.filter((e) => e.type === "crossEpic");
      expect(crossEdges).toHaveLength(1);
      expect(crossEdges[0].source).toBe("group:epic-a");
      expect(crossEdges[0].target).toBe("group:epic-b");
    });

    it("SB-9: a bare story_id resolves an intra-epic (same-group) sibling", () => {
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [
              buildStoryRef("A-1-alpha.md", {
                storyId: "A-1",
                absolutePath: "/ctx/pipe/epic-a/A-1-alpha.md",
              }),
              buildStoryRef("A-2-bravo.md", {
                storyId: "A-2",
                absolutePath: "/ctx/pipe/epic-a/A-2-bravo.md",
                dependsOn: ["A-1"], // bare story_id of the sibling
              }),
            ],
          }),
        ],
      });
      const { edges } = derivedTreeToReactFlow(tree);
      // Intra-epic edge (NOT crossEpic) from A-1 → A-2.
      expect(edges.filter((e) => e.type === "crossEpic")).toHaveLength(0);
      const pairs = edges.map((e) => [e.source, e.target]);
      expect(pairs).toContainEqual(["epic-a/A-1-alpha.md", "epic-a/A-2-bravo.md"]);
    });

    it("SB-9: a duplicate story_id is poisoned — no edge is drawn", () => {
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [
              buildStoryRef("one.md", { storyId: "DUP", absolutePath: "/ctx/pipe/epic-a/one.md" }),
              buildStoryRef("two.md", { storyId: "DUP", absolutePath: "/ctx/pipe/epic-a/two.md" }),
            ],
          }),
          buildTree("epic-b", {
            dirPath: "epic-b",
            stories: [
              buildStoryRef("b.md", {
                storyId: "B",
                absolutePath: "/ctx/pipe/epic-b/b.md",
                dependsOn: ["DUP"],
              }),
            ],
          }),
        ],
      });
      const { edges } = derivedTreeToReactFlow(tree);
      expect(edges.filter((e) => e.type === "crossEpic")).toHaveLength(0);
    });

    it("6.2: cross-epic ref to a non-existent directory is silently dropped", () => {
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [
              buildStoryRef("a.md", {
                absolutePath: "/ctx/pipe/epic-a/a.md",
                dependsOn: ["../no-such-epic/story-x"],
              }),
            ],
          }),
        ],
      });
      const { edges } = derivedTreeToReactFlow(tree);
      // No cross-epic edges, no crash.
      const crossEdges = edges.filter((e) => e.type === "crossEpic");
      expect(crossEdges).toHaveLength(0);
    });

    it("6.3: cross-epic ref to valid directory but non-existent story → unresolvedRefsSet", () => {
      const tree = twoSiblingEpics({
        epicBStories: [
          buildStoryRef("B-1-charlie.md", {
            absolutePath: "/ctx/pipe/epic-b/B-1-charlie.md",
            dependsOn: ["../epic-a/does-not-exist"],
          }),
        ],
      });
      const { edges, unresolvedSupportingDocRefs } = derivedTreeToReactFlow(tree);
      const crossEdges = edges.filter((e) => e.type === "crossEpic");
      expect(crossEdges).toHaveLength(0);
      expect(unresolvedSupportingDocRefs).toContain("../epic-a/does-not-exist");
    });

    it("6.4: self-referencing own epic directory produces no self-loop edge", () => {
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [
              buildStoryRef("A-1-alpha.md", {
                absolutePath: "/ctx/pipe/epic-a/A-1-alpha.md",
              }),
              buildStoryRef("A-2-bravo.md", {
                absolutePath: "/ctx/pipe/epic-a/A-2-bravo.md",
                dependsOn: ["../epic-a/A-1-alpha"],
              }),
            ],
          }),
        ],
      });
      const { edges } = derivedTreeToReactFlow(tree);
      const crossEdges = edges.filter((e) => e.type === "crossEpic");
      expect(crossEdges).toHaveLength(0);
    });

    it("6.5: mixed intra-epic + cross-epic deps emit both edges", () => {
      const tree = twoSiblingEpics({
        epicBStories: [
          buildStoryRef("B-1-charlie.md", {
            absolutePath: "/ctx/pipe/epic-b/B-1-charlie.md",
          }),
          buildStoryRef("B-2-delta.md", {
            absolutePath: "/ctx/pipe/epic-b/B-2-delta.md",
            dependsOn: ["B-1-charlie", "../epic-a/A-1-alpha"],
          }),
        ],
      });
      const { edges } = derivedTreeToReactFlow(tree);
      // Intra-epic edge: B-1 → B-2
      const intra = edges.find(
        (e) => e.source === "epic-b/B-1-charlie.md" && e.target === "epic-b/B-2-delta.md",
      );
      expect(intra).toBeDefined();
      // Cross-epic edge: group:epic-a → group:epic-b (P.8 group→group)
      const cross = edges.find((e) => e.source === "group:epic-a" && e.target === "group:epic-b");
      expect(cross).toBeDefined();
      expect(cross?.type).toBe("crossEpic");
    });

    it("6.9: cross-epic edge carries tooltip and type; story node has resolvedDeps", () => {
      const tree = twoSiblingEpics();
      const { edges, nodes } = derivedTreeToReactFlow(tree);
      const ce = edges.find((e) => e.type === "crossEpic");
      expect(ce).toBeDefined();
      expect(ce!.data).toMatchObject({ tooltip: "B.1 depends on A.1 — Story A.1 — Alpha" });
      expect(ce!.type).toBe("crossEpic");
      // Label is on the story card footer, not the edge.
      expect(ce!.label).toBeUndefined();
      // Story node carries resolvedDeps for footer badge rendering.
      const storyNode = nodes.find((n) => n.id === "epic-b/B-1-charlie.md");
      const rd = (storyNode!.data as { resolvedDeps?: ResolvedDep[] }).resolvedDeps;
      expect(rd).toBeDefined();
      expect(rd!.map((d) => d.label)).toContain("A.1");
      // P.4: source story has no status (null → "todo") → met is false.
      expect(rd![0].met).toBe(false);
    });

    it("6.10: ./sibling-story ref strips to bare stem and resolves intra-epic", () => {
      const tree = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [
          buildStoryRef("a.md", { absolutePath: "/ctx/pipe/epic-a/a.md" }),
          buildStoryRef("b.md", {
            absolutePath: "/ctx/pipe/epic-a/b.md",
            dependsOn: ["./a"],
          }),
        ],
      });
      const { edges } = derivedTreeToReactFlow(tree);
      expect(edges).toHaveLength(1);
      expect(edges[0].source).toBe("a.md");
      expect(edges[0].target).toBe("b.md");
      // No cross-epic edge type — it's a normal intra-epic edge.
      expect(edges[0].type).toBeUndefined();
    });

    it("6.11: two deps to different stories in same target epic → bundled into one edge", () => {
      const tree = twoSiblingEpics({
        epicBStories: [
          buildStoryRef("B-1-charlie.md", {
            absolutePath: "/ctx/pipe/epic-b/B-1-charlie.md",
            dependsOn: ["../epic-a/A-1-alpha", "../epic-a/A-2-bravo"],
          }),
        ],
      });
      const { edges, nodes } = derivedTreeToReactFlow(tree);
      const crossEdges = edges.filter((e) => e.type === "crossEpic");
      // Bundled into a single edge (no label — deps shown on card footer).
      expect(crossEdges).toHaveLength(1);
      expect(crossEdges[0].label).toBeUndefined();
      // Tooltip lists both deps (newline-separated).
      expect(crossEdges[0].data).toMatchObject({
        tooltip: "B.1 depends on A.1 — Story A.1 — Alpha\nB.1 depends on A.2 — Story A.2 — Bravo",
      });
      // Story node footer has both deps.
      const storyNode = nodes.find((n) => n.id === "epic-b/B-1-charlie.md");
      const rd = (storyNode!.data as { resolvedDeps?: ResolvedDep[] }).resolvedDeps;
      expect(rd!.map((d) => d.label)).toEqual(["A.1", "A.2"]);
      // P.4: both source stories have null status → met is false.
      expect(rd!.every((d) => d.met === false)).toBe(true);
    });

    it("6.12: later-processed sibling can reference earlier-processed sibling (two-pass)", () => {
      // epic-b is processed after epic-a in subEpics array order.
      // A story in epic-a referencing epic-b should still resolve
      // because the group index is populated in a separate pass.
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [
              buildStoryRef("A-1.md", {
                absolutePath: "/ctx/pipe/epic-a/A-1.md",
                dependsOn: ["../epic-b/B-1"],
              }),
            ],
          }),
          buildTree("epic-b", {
            dirPath: "epic-b",
            stories: [
              buildStoryRef("B-1.md", {
                title: "Story B.1",
                absolutePath: "/ctx/pipe/epic-b/B-1.md",
              }),
            ],
          }),
        ],
      });
      const { edges } = derivedTreeToReactFlow(tree);
      const ce = edges.find((e) => e.type === "crossEpic");
      expect(ce).toBeDefined();
      expect(ce!.source).toBe("group:epic-b");
      expect(ce!.target).toBe("group:epic-a");
    });

    it("6.13: existing intra-epic edge tests still pass (regression guard)", () => {
      // Simple linear chain — same test as existing, just validating
      // cross-epic changes didn't break normal flow.
      const tree = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [buildStoryRef("a.md"), buildStoryRef("b.md", { dependsOn: ["a"] })],
      });
      const { edges } = derivedTreeToReactFlow(tree);
      expect(edges).toHaveLength(1);
      expect(edges[0].source).toBe("a.md");
      expect(edges[0].target).toBe("b.md");
      // No type override — intra-epic edge uses default smoothstep.
      expect(edges[0].type).toBeUndefined();
    });

    it("6.7: identical cross-epic refs after normalization are deduplicated in bundle", () => {
      const tree = twoSiblingEpics({
        epicBStories: [
          buildStoryRef("B-1.md", {
            absolutePath: "/ctx/pipe/epic-b/B-1.md",
            dependsOn: ["../epic-a/A-1-alpha", "../epic-a/A-1-alpha.md"],
          }),
        ],
      });
      const { edges, nodes } = derivedTreeToReactFlow(tree);
      const crossEdges = edges.filter((e) => e.type === "crossEpic");
      expect(crossEdges).toHaveLength(1);
      // Both refs resolve to same story — footer should NOT duplicate.
      const storyNode = nodes.find((n) => n.id === "epic-b/B-1.md");
      const rd = (storyNode!.data as { resolvedDeps?: ResolvedDep[] }).resolvedDeps;
      expect(rd).toHaveLength(1);
      expect(rd![0].label).toBe("A.1");
      // P.4: source story has null status → met is false.
      expect(rd![0].met).toBe(false);
    });

    it("8.1: two stories in target epic with deps to same source → single group→group edge", () => {
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [
              buildStoryRef("A-1-alpha.md", {
                title: "Story A.1 — Alpha",
                absolutePath: "/ctx/pipe/epic-a/A-1-alpha.md",
              }),
              buildStoryRef("A-2-bravo.md", {
                title: "Story A.2 — Bravo",
                absolutePath: "/ctx/pipe/epic-a/A-2-bravo.md",
              }),
            ],
          }),
          buildTree("epic-b", {
            dirPath: "epic-b",
            stories: [
              buildStoryRef("B-1-charlie.md", {
                absolutePath: "/ctx/pipe/epic-b/B-1-charlie.md",
                dependsOn: ["../epic-a/A-1-alpha"],
              }),
              buildStoryRef("B-2-delta.md", {
                absolutePath: "/ctx/pipe/epic-b/B-2-delta.md",
                dependsOn: ["../epic-a/A-2-bravo"],
              }),
            ],
          }),
        ],
      });
      const { edges, nodes } = derivedTreeToReactFlow(tree);
      // P.8: exactly one group→group edge, not two story-level edges.
      const crossEdges = edges.filter((e) => e.type === "crossEpic");
      expect(crossEdges).toHaveLength(1);
      expect(crossEdges[0].source).toBe("group:epic-a");
      expect(crossEdges[0].target).toBe("group:epic-b");
      // Tooltip aggregates both dep pairs with dependent labels.
      expect(crossEdges[0].data).toMatchObject({
        tooltip: "B.1 depends on A.1 — Story A.1 — Alpha\nB.2 depends on A.2 — Story A.2 — Bravo",
      });
      // Each story node carries its own resolvedDeps.
      const b1 = nodes.find((n) => n.id === "epic-b/B-1-charlie.md");
      const b1rd = (b1!.data as { resolvedDeps?: ResolvedDep[] }).resolvedDeps;
      expect(b1rd).toHaveLength(1);
      expect(b1rd![0].label).toBe("A.1");
      const b2 = nodes.find((n) => n.id === "epic-b/B-2-delta.md");
      const b2rd = (b2!.data as { resolvedDeps?: ResolvedDep[] }).resolvedDeps;
      expect(b2rd).toHaveLength(1);
      expect(b2rd![0].label).toBe("A.2");
    });

    it("6.9a: edge label derivation patterns", () => {
      // Build a tree where epic-a has stories with various naming patterns.
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [
              buildStoryRef("W-5-source-repo.md", {
                title: "Source Repo",
                absolutePath: "/ctx/pipe/epic-a/W-5-source-repo.md",
              }),
              buildStoryRef("F-12-workspace-tabs.md", {
                title: "Workspace Tabs",
                absolutePath: "/ctx/pipe/epic-a/F-12-workspace-tabs.md",
              }),
              buildStoryRef("no-prefix.md", {
                title: "No Prefix",
                absolutePath: "/ctx/pipe/epic-a/no-prefix.md",
              }),
            ],
          }),
          buildTree("epic-b", {
            dirPath: "epic-b",
            stories: [
              buildStoryRef("b.md", {
                absolutePath: "/ctx/pipe/epic-b/b.md",
                dependsOn: [
                  "../epic-a/W-5-source-repo",
                  "../epic-a/F-12-workspace-tabs",
                  "../epic-a/no-prefix",
                ],
              }),
            ],
          }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      // Labels are on the story card footer, not the edge.
      const storyNode = nodes.find((n) => n.id === "epic-b/b.md");
      const rd = (storyNode!.data as { resolvedDeps?: ResolvedDep[] }).resolvedDeps;
      expect(rd).toBeDefined();
      const labels = rd!.map((d) => d.label);
      expect(labels).toEqual(["W.5", "F.12", "no-prefix"]);
      // P.4: all source stories have null status → all met=false.
      expect(rd!.every((d) => d.met === false)).toBe(true);
    });

    it("6.8: resolveScopeRelativePath via cross-epic ref with over-pop returns null", () => {
      // A ref that tries to go above the scope root should be silently dropped.
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [
              buildStoryRef("a.md", {
                absolutePath: "/ctx/pipe/epic-a/a.md",
                dependsOn: ["../../outside-scope/story-x"],
              }),
            ],
          }),
        ],
      });
      const { edges } = derivedTreeToReactFlow(tree);
      const crossEdges = edges.filter((e) => e.type === "crossEpic");
      expect(crossEdges).toHaveLength(0);
    });

    it("6.6: cross-epic ref outside the current pipeline scope is silently dropped", () => {
      // Scope into just epic-a — epic-b is not in scope.
      const tree = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [
          buildStoryRef("a.md", {
            absolutePath: "/ctx/pipe/epic-a/a.md",
            dependsOn: ["../epic-b/B-1"],
          }),
        ],
      });
      const { edges } = derivedTreeToReactFlow(tree);
      const crossEdges = edges.filter((e) => e.type === "crossEpic");
      expect(crossEdges).toHaveLength(0);
    });

    it("6.13a: out-of-scope cross-epic dep shows badge and flags hasExternalDeps", () => {
      // Scope into just epic-a — epic-b is outside scope.
      const tree = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [
          buildStoryRef("a.md", {
            absolutePath: "/ctx/pipe/epic-a/a.md",
            dependsOn: ["../epic-b/B-1"],
          }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const storyNode = nodes.find((n) => n.id === "a.md");
      expect(storyNode).toBeDefined();
      const data = storyNode!.data as {
        hasExternalDeps?: boolean;
        resolvedDeps?: ResolvedDep[];
      };
      // hasExternalDeps still set (AC3)
      expect(data.hasExternalDeps).toBe(true);
      // Badge shows for the out-of-scope dep (AC1)
      expect(data.resolvedDeps).toBeDefined();
      expect(data.resolvedDeps).toHaveLength(1);
      expect(data.resolvedDeps![0].label).toBe("B.1");
      expect(data.resolvedDeps![0].met).toBeUndefined();
      expect(data.resolvedDeps![0].outOfScope).toBe(true);
      expect(data.resolvedDeps![0].filePath).toBe("/ctx/pipe/epic-b/B-1.md");
    });

    it("6.13b: resolved cross-epic dep does NOT flag hasExternalDeps", () => {
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [
              buildStoryRef("A-1.md", {
                title: "A1",
                absolutePath: "/ctx/pipe/epic-a/A-1.md",
              }),
            ],
          }),
          buildTree("epic-b", {
            dirPath: "epic-b",
            stories: [
              buildStoryRef("B-1.md", {
                absolutePath: "/ctx/pipe/epic-b/B-1.md",
                dependsOn: ["../epic-a/A-1"],
              }),
            ],
          }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const storyNode = nodes.find((n) => n.id === "epic-b/B-1.md");
      expect(storyNode).toBeDefined();
      expect((storyNode!.data as { hasExternalDeps?: boolean }).hasExternalDeps).toBeUndefined();
    });

    it("6.13c: multiple out-of-scope deps from different epics produce multiple badges", () => {
      const tree = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [
          buildStoryRef("a.md", {
            absolutePath: "/ctx/pipe/epic-a/a.md",
            dependsOn: ["../epic-b/B-1", "../epic-c/C-2-feature"],
          }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const storyNode = nodes.find((n) => n.id === "a.md");
      expect(storyNode).toBeDefined();
      const data = storyNode!.data as {
        hasExternalDeps?: boolean;
        resolvedDeps?: ResolvedDep[];
      };
      expect(data.hasExternalDeps).toBe(true);
      expect(data.resolvedDeps).toHaveLength(2);
      expect(data.resolvedDeps![0].label).toBe("B.1");
      expect(data.resolvedDeps![0].outOfScope).toBe(true);
      expect(data.resolvedDeps![1].label).toBe("C.2");
      expect(data.resolvedDeps![1].outOfScope).toBe(true);
    });

    it("6.13f: SW-FX-1 — out-of-scope dep resolves met from the full index when drilled", () => {
      // Full tree: epic-a/a.md depends cross-epic on epic-b/B-1 (done).
      const fullTree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [
              buildStoryRef("a.md", {
                absolutePath: "/ctx/pipe/epic-a/a.md",
                dependsOn: ["../epic-b/B-1"],
              }),
            ],
          }),
          buildTree("epic-b", {
            dirPath: "epic-b",
            stories: [
              buildStoryRef("B-1.md", { absolutePath: "/ctx/pipe/epic-b/B-1.md", status: "done" }),
            ],
          }),
        ],
      });
      const fullGroupIndex = buildFullGroupIndex(fullTree);
      const scoped = findScopedSubtree(fullTree, "epic-a")!;

      // Drilled into epic-a: epic-b is out of scope, so the dep has no edge and
      // stays outOfScope — but its met state now comes from the full index.
      const { nodes } = derivedTreeToReactFlow(scoped, { fullGroupIndex });
      const data = nodes.find((n) => n.id === "a.md")!.data as {
        hasExternalDeps?: boolean;
        resolvedDeps?: ResolvedDep[];
      };
      expect(data.hasExternalDeps).toBe(true);
      expect(data.resolvedDeps).toHaveLength(1);
      expect(data.resolvedDeps![0].outOfScope).toBe(true);
      expect(data.resolvedDeps![0].met).toBe(true);
      // External marker: the badge carries the target epic's color (from the
      // full index) so it's visually distinct from internal deps.
      expect(data.resolvedDeps![0].epicColor).toBeDefined();
      expect(data.resolvedDeps![0].epicBadgeBg).toBeDefined();

      // Without the full index, met + color stay unknown (legacy preserved).
      const { nodes: legacyNodes } = derivedTreeToReactFlow(scoped);
      const legacy = legacyNodes.find((n) => n.id === "a.md")!.data as {
        resolvedDeps?: ResolvedDep[];
      };
      expect(legacy.resolvedDeps![0].met).toBeUndefined();
      expect(legacy.resolvedDeps![0].epicColor).toBeUndefined();
      expect(legacy.resolvedDeps![0].outOfScope).toBe(true);
    });

    // --- SW-SB-17: out-of-scope badge for the story_id form in a drilled view --

    /** Full tree: epic-a/a.md depends on epic-b/B-1 (done) — expressed as a bare
     *  story_id, and (for the contrast case) as a `../path`. */
    function sb17FullTree(depRef: string) {
      return buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [
              buildStoryRef("a.md", {
                storyId: "A-1",
                absolutePath: "/ctx/pipe/epic-a/a.md",
                dependsOn: [depRef],
              }),
            ],
          }),
          buildTree("epic-b", {
            dirPath: "epic-b",
            stories: [
              buildStoryRef("B-1-charlie.md", {
                storyId: "B-1",
                absolutePath: "/ctx/pipe/epic-b/B-1-charlie.md",
                status: "done",
              }),
            ],
          }),
        ],
      });
    }

    it("SB-17: a bare story_id dep out of drill scope renders an out-of-scope badge", () => {
      const fullTree = sb17FullTree("B-1"); // bare story_id, not ../epic-b/...
      const fullGroupIndex = buildFullGroupIndex(fullTree);
      const scoped = findScopedSubtree(fullTree, "epic-a")!;

      const { nodes, edges } = derivedTreeToReactFlow(scoped, { fullGroupIndex });
      const data = nodes.find((n) => n.id === "a.md")!.data as {
        hasExternalDeps?: boolean;
        resolvedDeps?: ResolvedDep[];
      };
      expect(data.hasExternalDeps).toBe(true);
      expect(data.resolvedDeps).toHaveLength(1);
      const badge = data.resolvedDeps![0];
      expect(badge.outOfScope).toBe(true);
      expect(badge.met).toBe(true); // B-1 is done, resolved from the full index
      expect(badge.epicColor).toBeDefined();
      expect(badge.epicBadgeBg).toBeDefined();
      // AC-3: no edge is drawn to an out-of-scope target (badge only).
      expect(edges.filter((e) => e.type === "crossEpic")).toHaveLength(0);
    });

    it("SB-17: the story_id badge matches the ../path badge byte-for-byte", () => {
      const fgiId = buildFullGroupIndex(sb17FullTree("B-1"));
      const fgiPath = buildFullGroupIndex(sb17FullTree("../epic-b/B-1-charlie"));
      const byId = derivedTreeToReactFlow(findScopedSubtree(sb17FullTree("B-1"), "epic-a")!, {
        fullGroupIndex: fgiId,
      });
      const byPath = derivedTreeToReactFlow(
        findScopedSubtree(sb17FullTree("../epic-b/B-1-charlie"), "epic-a")!,
        { fullGroupIndex: fgiPath },
      );
      const idBadge = (
        byId.nodes.find((n) => n.id === "a.md")!.data as { resolvedDeps?: ResolvedDep[] }
      ).resolvedDeps![0];
      const pathBadge = (
        byPath.nodes.find((n) => n.id === "a.md")!.data as { resolvedDeps?: ResolvedDep[] }
      ).resolvedDeps![0];
      // Same visual outcome: outOfScope, met, epic color/bg, title.
      expect(idBadge.outOfScope).toBe(pathBadge.outOfScope);
      expect(idBadge.met).toBe(pathBadge.met);
      expect(idBadge.epicColor).toBe(pathBadge.epicColor);
      expect(idBadge.epicBadgeBg).toBe(pathBadge.epicBadgeBg);
      expect(idBadge.title).toBe(pathBadge.title);
    });

    it("SB-17: without a fullGroupIndex the out-of-scope story_id dep drops (unchanged)", () => {
      const scoped = findScopedSubtree(sb17FullTree("B-1"), "epic-a")!;
      const { nodes } = derivedTreeToReactFlow(scoped); // no fullGroupIndex
      const data = nodes.find((n) => n.id === "a.md")!.data as {
        hasExternalDeps?: boolean;
        resolvedDeps?: ResolvedDep[];
      };
      // Same as SW-SB-9 behavior: dropped, no badge, no external flag.
      expect(data.hasExternalDeps).toBeUndefined();
      expect(data.resolvedDeps ?? []).toHaveLength(0);
    });

    it("SB-17: a poisoned (duplicate) out-of-scope story_id renders no badge", () => {
      const fullTree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [
              buildStoryRef("a.md", {
                storyId: "A-1",
                absolutePath: "/ctx/pipe/epic-a/a.md",
                dependsOn: ["DUP"],
              }),
            ],
          }),
          buildTree("epic-b", {
            dirPath: "epic-b",
            stories: [
              buildStoryRef("one.md", { storyId: "DUP", absolutePath: "/ctx/pipe/epic-b/one.md" }),
              buildStoryRef("two.md", { storyId: "DUP", absolutePath: "/ctx/pipe/epic-b/two.md" }),
            ],
          }),
        ],
      });
      const fullGroupIndex = buildFullGroupIndex(fullTree);
      const scoped = findScopedSubtree(fullTree, "epic-a")!;
      const { nodes } = derivedTreeToReactFlow(scoped, { fullGroupIndex });
      const data = nodes.find((n) => n.id === "a.md")!.data as {
        hasExternalDeps?: boolean;
        resolvedDeps?: ResolvedDep[];
      };
      expect(data.resolvedDeps ?? []).toHaveLength(0);
    });

    it("6.13h: out-of-scope dep badge matches the drilled-out badge (same epic color)", () => {
      const fullTree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [
              buildStoryRef("a.md", {
                absolutePath: "/ctx/pipe/epic-a/a.md",
                dependsOn: ["../epic-b/B-1"],
              }),
            ],
          }),
          buildTree("epic-b", {
            dirPath: "epic-b",
            stories: [
              buildStoryRef("B-1.md", { absolutePath: "/ctx/pipe/epic-b/B-1.md", status: "done" }),
            ],
          }),
        ],
      });
      const fullGroupIndex = buildFullGroupIndex(fullTree);

      // Drilled OUT (full): epic-b is in scope → in-scope cross-epic badge.
      const out = derivedTreeToReactFlow(fullTree, { fullGroupIndex });
      const outDep = (
        out.nodes.find((n) => n.id === "epic-a/a.md")!.data as { resolvedDeps?: ResolvedDep[] }
      ).resolvedDeps![0];

      // Drilled IN (epic-a): epic-b is out of scope → out-of-scope badge.
      const scoped = findScopedSubtree(fullTree, "epic-a")!;
      const inn = derivedTreeToReactFlow(scoped, { fullGroupIndex });
      const inDep = (
        inn.nodes.find((n) => n.id === "a.md")!.data as { resolvedDeps?: ResolvedDep[] }
      ).resolvedDeps![0];

      // Same label, title, met, and epic color — drill-independent. The only
      // difference is the out-of-scope flag (drives drill-to-root click).
      expect(inDep.label).toBe(outDep.label);
      expect(inDep.title).toBe(outDep.title);
      expect(inDep.epicColor).toBe(outDep.epicColor);
      expect(inDep.met).toBe(outDep.met);
      expect(outDep.outOfScope).toBeUndefined();
      expect(inDep.outOfScope).toBe(true);
    });

    it("6.13g: SW-FX-1 — out-of-scope dep that is NOT done resolves met:false from the full index", () => {
      const fullTree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [
              buildStoryRef("a.md", {
                absolutePath: "/ctx/pipe/epic-a/a.md",
                dependsOn: ["../epic-b/B-1"],
              }),
            ],
          }),
          buildTree("epic-b", {
            dirPath: "epic-b",
            stories: [
              buildStoryRef("B-1.md", {
                absolutePath: "/ctx/pipe/epic-b/B-1.md",
                status: "in-progress",
              }),
            ],
          }),
        ],
      });
      const scoped = findScopedSubtree(fullTree, "epic-a")!;
      const { nodes } = derivedTreeToReactFlow(scoped, {
        fullGroupIndex: buildFullGroupIndex(fullTree),
      });
      const data = nodes.find((n) => n.id === "a.md")!.data as { resolvedDeps?: ResolvedDep[] };
      expect(data.resolvedDeps![0].outOfScope).toBe(true);
      expect(data.resolvedDeps![0].met).toBe(false);
    });

    it("6.13e: root-level story dep produces edge and full-resolution badge", () => {
      // Drilled into "install" scope — root-level story + sub-epic story that depends on it.
      const tree = buildTree("install", {
        dirPath: "install",
        stories: [
          buildStoryRef("I-1-root-story.md", {
            title: "Root Story",
            absolutePath: "/ctx/pipe/install/I-1-root-story.md",
            status: "done",
          }),
        ],
        subEpics: [
          buildTree("convention", {
            dirPath: "install/convention",
            stories: [
              buildStoryRef("A-1.md", {
                title: "Convention Story",
                absolutePath: "/ctx/pipe/install/convention/A-1.md",
                dependsOn: ["../I-1-root-story"],
              }),
            ],
          }),
        ],
      });
      const { nodes, edges } = derivedTreeToReactFlow(tree);

      // Badge on the sub-epic story with full resolution
      const storyNode = nodes.find((n) => n.id === "convention/A-1.md");
      expect(storyNode).toBeDefined();
      const data = storyNode!.data as {
        resolvedDeps?: ResolvedDep[];
        hasExternalDeps?: boolean;
      };
      expect(data.resolvedDeps).toBeDefined();
      expect(data.resolvedDeps).toHaveLength(1);
      expect(data.resolvedDeps![0].label).toBe("I.1");
      expect(data.resolvedDeps![0].title).toBe("Root Story");
      expect(data.resolvedDeps![0].met).toBe(true);
      expect(data.resolvedDeps![0].outOfScope).toBeUndefined();
      // Not flagged as external — target IS in scope
      expect(data.hasExternalDeps).toBeUndefined();

      // Edge from root story to sub-epic group (group→story edge)
      const crossEdges = edges.filter((e) => e.type === "crossEpic");
      expect(crossEdges).toHaveLength(1);
      expect(crossEdges[0].source).toBe("I-1-root-story.md");
      expect(crossEdges[0].target).toBe("group:convention");
    });

    it("6.13d: malformed ref with no file portion sets hasExternalDeps without badge", () => {
      const tree = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [
          buildStoryRef("a.md", {
            absolutePath: "/ctx/pipe/epic-a/a.md",
            dependsOn: ["../epic-b/"],
          }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const storyNode = nodes.find((n) => n.id === "a.md");
      expect(storyNode).toBeDefined();
      const data = storyNode!.data as {
        hasExternalDeps?: boolean;
        resolvedDeps?: ResolvedDep[];
      };
      // hasExternalDeps set (the ref escapes scope)
      expect(data.hasExternalDeps).toBe(true);
      // No badge — ref has no file portion
      expect(data.resolvedDeps).toBeUndefined();
    });
  });

  // ---------------------------------------------------------------------------
  // P.2: Cross-directory supporting_docs resolution
  // ---------------------------------------------------------------------------

  describe("cross-directory supporting_docs (P.2)", () => {
    it("6.14: cross-dir supporting_docs ref resolves to resolvedSupportingDocs", () => {
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [
              buildStoryRef("story.md", {
                absolutePath: "/ctx/pipe/epic-a/story.md",
                supportingDocs: ["../epic-b/doc-file.md"],
              }),
            ],
          }),
          buildTree("epic-b", {
            dirPath: "epic-b",
            stories: [
              buildStoryRef("doc-file.md", {
                absolutePath: "/ctx/pipe/epic-b/doc-file.md",
                documentType: "doc",
                title: "Some Doc",
              }),
            ],
          }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const storyNode = nodes.find((n) => n.id === "epic-a/story.md");
      expect(storyNode).toBeDefined();
      const data = storyNode!.data as {
        resolvedSupportingDocs?: Array<{ fileName: string; documentType: string }>;
      };
      expect(data.resolvedSupportingDocs).toHaveLength(1);
      expect(data.resolvedSupportingDocs![0].fileName).toBe("doc-file.md");
      expect(data.resolvedSupportingDocs![0].documentType).toBe("doc");
    });

    it("6.15: cross-dir supporting_docs ref to a story-type file is silently ignored", () => {
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [
              buildStoryRef("story.md", {
                absolutePath: "/ctx/pipe/epic-a/story.md",
                supportingDocs: ["../epic-b/other-story.md"],
              }),
            ],
          }),
          buildTree("epic-b", {
            dirPath: "epic-b",
            stories: [
              buildStoryRef("other-story.md", {
                absolutePath: "/ctx/pipe/epic-b/other-story.md",
                // Default type is "story" — should be skipped.
              }),
            ],
          }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const storyNode = nodes.find((n) => n.id === "epic-a/story.md");
      const data = storyNode!.data as { resolvedSupportingDocs?: unknown[] };
      expect(data.resolvedSupportingDocs).toBeUndefined();
    });

    it("6.16: cross-dir supporting_docs ref to non-existent file → unresolvedRefsSet", () => {
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [
              buildStoryRef("story.md", {
                absolutePath: "/ctx/pipe/epic-a/story.md",
                supportingDocs: ["../epic-b/no-such-file.md"],
              }),
            ],
          }),
          buildTree("epic-b", {
            dirPath: "epic-b",
            stories: [],
          }),
        ],
      });
      const { unresolvedSupportingDocRefs } = derivedTreeToReactFlow(tree);
      expect(unresolvedSupportingDocRefs).toContain("../epic-b/no-such-file.md");
    });

    it("6.17: cross-dir supporting_docs ref to non-existent directory → unresolvedRefsSet", () => {
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [
              buildStoryRef("story.md", {
                absolutePath: "/ctx/pipe/epic-a/story.md",
                supportingDocs: ["../no-such-epic/doc.md"],
              }),
            ],
          }),
        ],
      });
      const { unresolvedSupportingDocRefs } = derivedTreeToReactFlow(tree);
      expect(unresolvedSupportingDocRefs).toContain("../no-such-epic/doc.md");
    });

    it("6.18: ./sibling-doc resolves via existing sibling lookup, not cross-directory", () => {
      const tree = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [
          buildStoryRef("story.md", {
            absolutePath: "/ctx/pipe/epic-a/story.md",
            supportingDocs: ["./bug.md"],
          }),
          buildStoryRef("bug.md", {
            absolutePath: "/ctx/pipe/epic-a/bug.md",
            documentType: "bug",
          }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      // story.md should have bug.md as a resolved supporting doc.
      const storyNode = nodes.find((n) => n.id === "story.md");
      const data = storyNode!.data as { resolvedSupportingDocs?: Array<{ fileName: string }> };
      expect(data.resolvedSupportingDocs).toHaveLength(1);
      expect(data.resolvedSupportingDocs![0].fileName).toBe("bug.md");
    });

    it("6.19: multi-level cross-dir ref resolves correctly", () => {
      // epic-a/sub-a story references a doc 2 levels up then into epic-b.
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            subEpics: [
              buildTree("sub-a", {
                dirPath: "epic-a/sub-a",
                stories: [
                  buildStoryRef("story.md", {
                    absolutePath: "/ctx/pipe/epic-a/sub-a/story.md",
                    supportingDocs: ["../../epic-b/deep-doc.md"],
                  }),
                ],
              }),
            ],
          }),
          buildTree("epic-b", {
            dirPath: "epic-b",
            stories: [
              buildStoryRef("deep-doc.md", {
                absolutePath: "/ctx/pipe/epic-b/deep-doc.md",
                documentType: "doc",
              }),
            ],
          }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const storyNode = nodes.find((n) => n.id === "epic-a/sub-a/story.md");
      expect(storyNode).toBeDefined();
      const data = storyNode!.data as { resolvedSupportingDocs?: Array<{ fileName: string }> };
      expect(data.resolvedSupportingDocs).toHaveLength(1);
      expect(data.resolvedSupportingDocs![0].fileName).toBe("deep-doc.md");
    });

    it("6.20: existing sibling supporting_docs behavior unchanged", () => {
      // Same test as existing F.4 — sibling doc resolution still works.
      const tree = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [
          buildStoryRef("story.md", {
            absolutePath: "/ctx/pipe/epic-a/story.md",
            supportingDocs: ["bug.md"],
          }),
          buildStoryRef("bug.md", {
            absolutePath: "/ctx/pipe/epic-a/bug.md",
            documentType: "bug",
          }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      // Only story.md should be emitted (bug.md excluded as supporting doc).
      expect(nodes).toHaveLength(1);
      const data = nodes[0].data as { resolvedSupportingDocs?: unknown[] };
      expect(data.resolvedSupportingDocs).toHaveLength(1);
    });
  });

  // ---------------------------------------------------------------------------
  // P.4: Met dependency badge — resolvedDeps.met field
  // ---------------------------------------------------------------------------

  describe("met dependency badge (P.4)", () => {
    it("7.1: intra-epic dep has met=true when depended-on story status is done", () => {
      const tree = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [
          buildStoryRef("a.md", { status: "done" }),
          buildStoryRef("b.md", { dependsOn: ["a"] }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const bNode = nodes.find((n) => n.id === "b.md");
      const rd = (bNode!.data as { resolvedDeps?: ResolvedDep[] }).resolvedDeps;
      expect(rd).toHaveLength(1);
      expect(rd![0].met).toBe(true);
    });

    it("7.2: intra-epic dep has met=false when depended-on story status is not done", () => {
      const tree = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [
          buildStoryRef("a.md", { status: "in-progress" }),
          buildStoryRef("b.md", { dependsOn: ["a"] }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const bNode = nodes.find((n) => n.id === "b.md");
      const rd = (bNode!.data as { resolvedDeps?: ResolvedDep[] }).resolvedDeps;
      expect(rd).toHaveLength(1);
      expect(rd![0].met).toBe(false);
    });

    it("7.3: intra-epic dep has met=false when depended-on story has null status", () => {
      const tree = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [
          buildStoryRef("a.md"), // null status → normalizes to "todo"
          buildStoryRef("b.md", { dependsOn: ["a"] }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const bNode = nodes.find((n) => n.id === "b.md");
      const rd = (bNode!.data as { resolvedDeps?: ResolvedDep[] }).resolvedDeps;
      expect(rd).toHaveLength(1);
      expect(rd![0].met).toBe(false);
    });

    it("7.4: mixed met/unmet intra-epic deps", () => {
      const tree = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [
          buildStoryRef("a.md", { status: "done" }),
          buildStoryRef("b.md", { status: "review" }),
          buildStoryRef("c.md", { dependsOn: ["a", "b"] }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const cNode = nodes.find((n) => n.id === "c.md");
      const rd = (cNode!.data as { resolvedDeps?: ResolvedDep[] }).resolvedDeps;
      expect(rd).toHaveLength(2);
      // a is done → met, b is review → not met
      const metByLabel = Object.fromEntries(rd!.map((d) => [d.label, d.met]));
      expect(metByLabel["a"]).toBe(true);
      expect(metByLabel["b"]).toBe(false);
    });

    it("7.5: cross-epic dep has met=true when target story status is done", () => {
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [
              buildStoryRef("A-1-alpha.md", {
                title: "Alpha",
                absolutePath: "/ctx/pipe/epic-a/A-1-alpha.md",
                status: "done",
              }),
            ],
          }),
          buildTree("epic-b", {
            dirPath: "epic-b",
            stories: [
              buildStoryRef("B-1-beta.md", {
                absolutePath: "/ctx/pipe/epic-b/B-1-beta.md",
                dependsOn: ["../epic-a/A-1-alpha"],
              }),
            ],
          }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const bNode = nodes.find((n) => n.id === "epic-b/B-1-beta.md");
      const rd = (bNode!.data as { resolvedDeps?: ResolvedDep[] }).resolvedDeps;
      expect(rd).toHaveLength(1);
      expect(rd![0].label).toBe("A.1");
      expect(rd![0].met).toBe(true);
    });

    it("7.6: cross-epic dep has met=false when target story status is not done", () => {
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [
              buildStoryRef("A-1-alpha.md", {
                title: "Alpha",
                absolutePath: "/ctx/pipe/epic-a/A-1-alpha.md",
                status: "in-progress",
              }),
            ],
          }),
          buildTree("epic-b", {
            dirPath: "epic-b",
            stories: [
              buildStoryRef("B-1-beta.md", {
                absolutePath: "/ctx/pipe/epic-b/B-1-beta.md",
                dependsOn: ["../epic-a/A-1-alpha"],
              }),
            ],
          }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const bNode = nodes.find((n) => n.id === "epic-b/B-1-beta.md");
      const rd = (bNode!.data as { resolvedDeps?: ResolvedDep[] }).resolvedDeps;
      expect(rd).toHaveLength(1);
      expect(rd![0].met).toBe(false);
    });

    it("7.7: cross-epic bundled deps carry independent met values", () => {
      const tree = buildTree("", {
        subEpics: [
          buildTree("epic-a", {
            dirPath: "epic-a",
            stories: [
              buildStoryRef("A-1-alpha.md", {
                title: "Alpha",
                absolutePath: "/ctx/pipe/epic-a/A-1-alpha.md",
                status: "done",
              }),
              buildStoryRef("A-2-bravo.md", {
                title: "Bravo",
                absolutePath: "/ctx/pipe/epic-a/A-2-bravo.md",
                status: "todo",
              }),
            ],
          }),
          buildTree("epic-b", {
            dirPath: "epic-b",
            stories: [
              buildStoryRef("B-1.md", {
                absolutePath: "/ctx/pipe/epic-b/B-1.md",
                dependsOn: ["../epic-a/A-1-alpha", "../epic-a/A-2-bravo"],
              }),
            ],
          }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const bNode = nodes.find((n) => n.id === "epic-b/B-1.md");
      const rd = (bNode!.data as { resolvedDeps?: ResolvedDep[] }).resolvedDeps;
      expect(rd).toHaveLength(2);
      const metByLabel = Object.fromEntries(rd!.map((d) => [d.label, d.met]));
      expect(metByLabel["A.1"]).toBe(true);
      expect(metByLabel["A.2"]).toBe(false);
    });
  });

  // ---------------------------------------------------------------------------
  // P.11: statusCounts on group nodes
  // ---------------------------------------------------------------------------
  describe("P.11: statusCounts", () => {
    it("computes statusCounts for a group with mixed statuses", () => {
      const epic = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [
          buildStoryRef("s1.md", { status: "done", absolutePath: "/ctx/pipe/epic-a/s1.md" }),
          buildStoryRef("s2.md", { status: "done", absolutePath: "/ctx/pipe/epic-a/s2.md" }),
          buildStoryRef("s3.md", { status: "in-progress", absolutePath: "/ctx/pipe/epic-a/s3.md" }),
          buildStoryRef("s4.md", { status: "blocked", absolutePath: "/ctx/pipe/epic-a/s4.md" }),
          buildStoryRef("s5.md", { status: "todo", absolutePath: "/ctx/pipe/epic-a/s5.md" }),
        ],
      });
      const tree = buildTree("", { subEpics: [epic] });
      const { nodes } = derivedTreeToReactFlow(tree);
      const group = nodes.find((n) => n.id === "group:epic-a")!;
      const sc = (group.data as GroupNodeData).statusCounts;
      expect(sc).toEqual({ total: 5, done: 2, active: 1, blocked: 1, remaining: 1 });
    });

    it("aggregates review and qa into active", () => {
      const epic = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [
          buildStoryRef("s1.md", { status: "review", absolutePath: "/ctx/pipe/epic-a/s1.md" }),
          buildStoryRef("s2.md", { status: "qa", absolutePath: "/ctx/pipe/epic-a/s2.md" }),
          buildStoryRef("s3.md", { status: "in-progress", absolutePath: "/ctx/pipe/epic-a/s3.md" }),
        ],
      });
      const tree = buildTree("", { subEpics: [epic] });
      const { nodes } = derivedTreeToReactFlow(tree);
      const sc = (nodes.find((n) => n.id === "group:epic-a")!.data as GroupNodeData).statusCounts;
      expect(sc).toEqual({ total: 3, done: 0, active: 3, blocked: 0, remaining: 0 });
    });

    it("excludes non-actionable document types (doc, retro)", () => {
      const epic = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [
          buildStoryRef("s1.md", { status: "done", absolutePath: "/ctx/pipe/epic-a/s1.md" }),
          buildStoryRef("notes.md", {
            status: "done",
            documentType: "doc",
            absolutePath: "/ctx/pipe/epic-a/notes.md",
          }),
          buildStoryRef("retro.md", {
            status: "done",
            documentType: "retro",
            absolutePath: "/ctx/pipe/epic-a/retro.md",
          }),
        ],
      });
      const tree = buildTree("", { subEpics: [epic] });
      const { nodes } = derivedTreeToReactFlow(tree);
      const sc = (nodes.find((n) => n.id === "group:epic-a")!.data as GroupNodeData).statusCounts;
      expect(sc).toEqual({ total: 1, done: 1, active: 0, blocked: 0, remaining: 0 });
    });

    it("counts bug and spike types as actionable", () => {
      const epic = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [
          buildStoryRef("bug.md", {
            status: "in-progress",
            documentType: "bug",
            absolutePath: "/ctx/pipe/epic-a/bug.md",
          }),
          buildStoryRef("spike.md", {
            status: "todo",
            documentType: "spike",
            absolutePath: "/ctx/pipe/epic-a/spike.md",
          }),
        ],
      });
      const tree = buildTree("", { subEpics: [epic] });
      const { nodes } = derivedTreeToReactFlow(tree);
      const sc = (nodes.find((n) => n.id === "group:epic-a")!.data as GroupNodeData).statusCounts;
      expect(sc).toEqual({ total: 2, done: 0, active: 1, blocked: 0, remaining: 1 });
    });

    it("does not set statusCounts when group has no actionable stories", () => {
      const epic = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [buildStoryRef("s1.md", { absolutePath: "/ctx/pipe/epic-a/s1.md" })],
        subEpics: [
          buildTree("sub", {
            dirPath: "epic-a/sub",
            stories: [
              buildStoryRef("notes.md", {
                documentType: "doc",
                absolutePath: "/ctx/pipe/epic-a/sub/notes.md",
              }),
              buildStoryRef("leaf.md", { absolutePath: "/ctx/pipe/epic-a/sub/leaf.md" }),
            ],
          }),
        ],
      });
      const tree = buildTree("", { subEpics: [epic] });
      const { nodes } = derivedTreeToReactFlow(tree);
      // Sub-epic with only a doc + a story: the sub-epic itself has statusCounts
      // but it should NOT count the doc.
      const sub = nodes.find((n) => n.id === "group:epic-a/sub")!;
      const sc = (sub.data as GroupNodeData).statusCounts;
      expect(sc).toEqual({ total: 1, done: 0, active: 0, blocked: 0, remaining: 1 });
    });

    it("planning status counts as remaining", () => {
      const epic = buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [
          buildStoryRef("s1.md", { status: "planning", absolutePath: "/ctx/pipe/epic-a/s1.md" }),
          buildStoryRef("s2.md", { status: "todo", absolutePath: "/ctx/pipe/epic-a/s2.md" }),
        ],
      });
      const tree = buildTree("", { subEpics: [epic] });
      const { nodes } = derivedTreeToReactFlow(tree);
      const sc = (nodes.find((n) => n.id === "group:epic-a")!.data as GroupNodeData).statusCounts;
      expect(sc).toEqual({ total: 2, done: 0, active: 0, blocked: 0, remaining: 2 });
    });

    it("parent epic aggregates descendant sub-epic stories recursively", () => {
      const tree = buildTree("", {
        subEpics: [
          buildTree("parent", {
            dirPath: "parent",
            subEpics: [
              buildTree("sub-a", {
                dirPath: "parent/sub-a",
                stories: [
                  buildStoryRef("s1.md", {
                    status: "done",
                    absolutePath: "/ctx/pipe/parent/sub-a/s1.md",
                  }),
                  buildStoryRef("s2.md", {
                    status: "in-progress",
                    absolutePath: "/ctx/pipe/parent/sub-a/s2.md",
                  }),
                ],
              }),
              buildTree("sub-b", {
                dirPath: "parent/sub-b",
                stories: [
                  buildStoryRef("s3.md", {
                    status: "blocked",
                    absolutePath: "/ctx/pipe/parent/sub-b/s3.md",
                  }),
                ],
              }),
            ],
          }),
        ],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const parent = nodes.find((n) => n.id === "group:parent")!;
      const sc = (parent.data as GroupNodeData).statusCounts;
      expect(sc).toEqual({ total: 3, done: 1, active: 1, blocked: 1, remaining: 0 });
    });
  });

  // ----- R.7: mixed-extension stem resolution ----------------------------

  describe("mixed-extension resolution (R.7)", () => {
    it("dependency from .md to .html sibling resolves via stem", () => {
      const tree = buildTree("", {
        stories: [
          buildStoryRef("A-1-alpha.md", { dependsOn: ["B-2-beta"] }),
          buildStoryRef("B-2-beta.html"),
        ],
      });
      const { edges } = derivedTreeToReactFlow(tree);
      // Should have an edge from B-2-beta.html → A-1-alpha.md
      expect(edges.length).toBe(1);
    });

    it("dependency with explicit .html extension resolves via exact match", () => {
      const tree = buildTree("", {
        stories: [
          buildStoryRef("A-1-alpha.md", { dependsOn: ["B-2-beta.html"] }),
          buildStoryRef("B-2-beta.html"),
        ],
      });
      const { edges } = derivedTreeToReactFlow(tree);
      expect(edges.length).toBe(1);
    });

    it("ambiguous stem (both .md and .html) with bare-stem ref is unresolved", () => {
      // Control: a non-ambiguous dep in the same tree DOES resolve (proves
      // resolution works in general, and only ambiguity blocks it).
      const tree = buildTree("", {
        stories: [
          buildStoryRef("A-1-alpha.md", { dependsOn: ["B-2-beta", "C-3-gamma"] }),
          buildStoryRef("B-2-beta.md"),
          buildStoryRef("B-2-beta.html"),
          buildStoryRef("C-3-gamma.md"), // unambiguous control
        ],
      });
      const { edges } = derivedTreeToReactFlow(tree);
      // Ambiguous "B-2-beta" → no edge
      const betaEdges = edges.filter((e) => e.source.includes("B-2-beta"));
      expect(betaEdges.length).toBe(0);
      // Unambiguous "C-3-gamma" → edge emitted (control case)
      const gammaEdges = edges.filter((e) => e.source.includes("C-3-gamma"));
      expect(gammaEdges.length).toBe(1);
    });

    it("ambiguous stem with explicit extension resolves", () => {
      const tree = buildTree("", {
        stories: [
          buildStoryRef("A-1-alpha.md", { dependsOn: ["B-2-beta.html"] }),
          buildStoryRef("B-2-beta.md"),
          buildStoryRef("B-2-beta.html"),
        ],
      });
      const { edges } = derivedTreeToReactFlow(tree);
      const depEdges = edges.filter((e) => e.source.includes("B-2-beta.html"));
      expect(depEdges.length).toBe(1);
    });

    it("HTML story titles display with extension stripped", () => {
      const tree = buildTree("", {
        stories: [buildStoryRef("C-3-gamma.html")],
      });
      const { nodes } = derivedTreeToReactFlow(tree);
      const storyNode = nodes.find((n) => n.type === "story")!;
      expect((storyNode.data as { title: string }).title).toBe("C-3-gamma");
    });
  });
});

describe("derivedTreeToReactFlow — ready indicator (isReady)", () => {
  // epic-a/a.md (todo) depends cross-epic on epic-b/B-1; readiness toggles with
  // B-1's status, resolved against the full index (scope-independent).
  function twoEpicTree(b1Status: string, aStatus = "todo") {
    return buildTree("", {
      subEpics: [
        buildTree("epic-a", {
          dirPath: "epic-a",
          stories: [
            buildStoryRef("a.md", {
              absolutePath: "/ctx/pipe/epic-a/a.md",
              status: aStatus,
              dependsOn: ["../epic-b/B-1"],
            }),
          ],
        }),
        buildTree("epic-b", {
          dirPath: "epic-b",
          stories: [
            buildStoryRef("B-1.md", { absolutePath: "/ctx/pipe/epic-b/B-1.md", status: b1Status }),
          ],
        }),
      ],
    });
  }
  const readyOf = (
    tree: ReturnType<typeof twoEpicTree>,
    opts: Parameters<typeof derivedTreeToReactFlow>[1],
    id = "epic-a/a.md",
  ) =>
    (
      derivedTreeToReactFlow(tree, opts).nodes.find((n) => n.id === id)!.data as {
        isReady?: boolean;
      }
    ).isReady;

  it("todo story with all deps met → isReady true", () => {
    const tree = twoEpicTree("done");
    expect(readyOf(tree, { fullGroupIndex: buildFullGroupIndex(tree) })).toBe(true);
  });

  it("todo story with an unmet dep → isReady false", () => {
    const tree = twoEpicTree("in-progress");
    expect(readyOf(tree, { fullGroupIndex: buildFullGroupIndex(tree) })).toBe(false);
  });

  it("non-todo story → isReady false even with deps met", () => {
    const tree = twoEpicTree("done", "in-progress");
    expect(readyOf(tree, { fullGroupIndex: buildFullGroupIndex(tree) })).toBe(false);
  });

  it("planning story with all deps met → isReady true (ready to plan)", () => {
    const tree = twoEpicTree("done", "planning");
    expect(readyOf(tree, { fullGroupIndex: buildFullGroupIndex(tree) })).toBe(true);
  });

  it("planning story with an unmet dep → isReady false", () => {
    const tree = twoEpicTree("in-progress", "planning");
    expect(readyOf(tree, { fullGroupIndex: buildFullGroupIndex(tree) })).toBe(false);
  });

  it("no full index supplied → isReady undefined (legacy/no indicator)", () => {
    const tree = twoEpicTree("done");
    expect(readyOf(tree, undefined)).toBeUndefined();
  });

  it("is drill-independent: a drilled story keeps the same isReady as at full root", () => {
    const tree = twoEpicTree("done");
    const fullGroupIndex = buildFullGroupIndex(tree);
    const scoped = findScopedSubtree(tree, "epic-a")!;
    // Drilled into epic-a (epic-b off-screen); the full index still resolves the
    // cross-epic dep → ready, matching the full-root computation.
    expect(readyOf(scoped, { fullGroupIndex }, "a.md")).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// SW-FX-48 AC-2: the id→node lookup is a single incremental map, so the build
// stays linear instead of rescanning the whole nodes array per group. This
// large-input case exercises the shared-map path (group patching + per-story
// resolvedDeps) across many groups; under the old O(groups × nodes) rebuild it
// was pathologically slow, and it verifies the output is still correct at scale.
// ---------------------------------------------------------------------------
describe("SW-FX-48: linear node construction at scale", () => {
  it("builds a wide graph with intra-epic deps correctly (shared nodeById map)", () => {
    const EPICS = 40;
    const STORIES = 30;
    const subEpics = Array.from({ length: EPICS }, (_, e) =>
      buildTree(`epic-${e}`, {
        dirPath: `epic-${e}`,
        stories: Array.from({ length: STORIES }, (_, s) =>
          buildStoryRef(`e${e}-s${s}.md`, {
            // Each story (after the first in its epic) depends on the previous
            // one → forces the per-story resolvedDeps map lookup for every node.
            dependsOn: s > 0 ? [`e${e}-s${s - 1}.md`] : null,
            absolutePath: `/ctx/pipe/epic-${e}/e${e}-s${s}.md`,
          }),
        ),
      }),
    );
    const tree = buildTree("", { subEpics });

    const { nodes, edges } = derivedTreeToReactFlow(tree);

    // EPICS group nodes + EPICS*STORIES story nodes.
    expect(nodes).toHaveLength(EPICS + EPICS * STORIES);
    // One intra-epic edge per dependent story: (STORIES - 1) per epic.
    expect(edges).toHaveLength(EPICS * (STORIES - 1));

    // Spot-check a dep was resolved via the shared map (proves lookups landed).
    const dependent = nodes.find((n) => n.id.includes("e5-s7.md"));
    expect((dependent?.data as { resolvedDeps?: unknown[] }).resolvedDeps).toHaveLength(1);
  });
});
