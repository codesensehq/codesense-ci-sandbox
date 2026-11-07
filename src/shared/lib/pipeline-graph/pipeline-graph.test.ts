// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { buildPipelineGraph, buildFullGroupIndex } from "./pipeline-graph";
import { buildGroupIndex, buildGroupStoryIdIndex } from "./build-group-index";
import { collectUnmetDeps, resolveReadiness, formatBlockedDep } from "./resolve-nav-readiness";
import { findScopedSubtree, scopeRelative } from "./pipeline-utils";
import { buildStoryRef, buildTree } from "./test-fixtures/derived-tree";
import type { StoryRef } from "./types";

/**
 * Full team-content tree with two sibling epics: `seamless-workspaces/CP-1`
 * depends cross-epic on `seamless-sync/SS-3`. Mirrors the SW-FX-1 worked
 * example.
 */
function buildFullTree(ss3Status: string) {
  return buildTree("", {
    absoluteDirPath: "/repo",
    subEpics: [
      buildTree("seamless-sync", {
        dirPath: "seamless-sync",
        absoluteDirPath: "/repo/seamless-sync",
        stories: [buildStoryRef("SS-3.md", { status: ss3Status })],
      }),
      buildTree("seamless-workspaces", {
        dirPath: "seamless-workspaces",
        absoluteDirPath: "/repo/seamless-workspaces",
        stories: [
          buildStoryRef("CP-1.md", {
            status: "todo",
            dependsOn: ["../seamless-sync/SS-3"],
          }),
        ],
      }),
    ],
  });
}

function cp1Of(tree: ReturnType<typeof buildFullTree>): StoryRef {
  return tree.subEpics[1].stories[0];
}

describe("PipelineGraph — full-graph readiness (SW-FX-1)", () => {
  it("AC-1: a cross-epic dep that is done makes the story ready at the full root", () => {
    const tree = buildFullTree("done");
    const graph = buildPipelineGraph(tree);
    expect(graph.readiness(cp1Of(tree), "seamless-workspaces").ready).toBe(true);
  });

  it("AC-5/AC-8: readiness is identical whether scanned at the full root or drilled into the epic", () => {
    const tree = buildFullTree("done");
    const cp1 = cp1Of(tree);
    const graph = buildPipelineGraph(tree);

    // Drilled display scope: the nav would render only this subtree.
    const scoped = findScopedSubtree(tree, "seamless-workspaces")!;
    expect(scoped.dirPath).toBe("seamless-workspaces");

    // The OLD bug: a group index built from the *scoped* subtree can't resolve
    // the cross-epic dep, so readiness flips to false purely on drill.
    const scopedIndex = buildGroupIndex(scoped, scoped.dirPath);
    const scopedOwner = scopeRelative(scoped.dirPath, scoped.dirPath); // ""
    expect(
      collectUnmetDeps(cp1, scopedOwner, scopedIndex, buildGroupStoryIdIndex(scopedIndex)),
    ).not.toHaveLength(0);

    // The FIX: the graph resolves against the full index regardless of drill,
    // so readiness is the same as at the full root.
    expect(graph.readiness(cp1, "seamless-workspaces").ready).toBe(true);
  });

  it("AC-5 both directions: readiness is identical at full-root vs drilled display", () => {
    // Compute readiness for the SAME story via its full-tree display AND via
    // its drilled-subtree display, for both the ready and not-ready directions,
    // and assert they are equal — the ⇔ invariant, not asserted by construction.
    for (const ss3Status of ["done", "in-progress"] as const) {
      const tree = buildFullTree(ss3Status);
      const graph = buildPipelineGraph(tree);

      const fromFullDisplay = cp1Of(tree);
      const scoped = findScopedSubtree(tree, "seamless-workspaces")!;
      const fromDrilledDisplay = scoped.stories[0];

      const atFull = graph.readiness(fromFullDisplay, "seamless-workspaces");
      const atDrilled = graph.readiness(fromDrilledDisplay, scoped.dirPath);

      expect(atDrilled).toEqual(atFull);
      expect(atFull.ready).toBe(ss3Status === "done");
    }
  });

  it("AC-3/AC-7: a not-done cross-epic dep blocks readiness and names the off-screen blocker", () => {
    const tree = buildFullTree("in-progress");
    const graph = buildPipelineGraph(tree);

    const result = graph.readiness(cp1Of(tree), "seamless-workspaces");
    expect(result.ready).toBe(false);
    expect(result.blockedBy).toHaveLength(1);
    expect(result.blockedBy[0]).toMatchObject({
      ref: "../seamless-sync/SS-3",
      reason: "unmet",
      status: "in-progress",
    });
    expect(formatBlockedDep(result.blockedBy[0])).toBe("../seamless-sync/SS-3 (in-progress)");
  });

  it("exposes the full-tree dirPath as the owner-key scope root", () => {
    const tree = buildFullTree("done");
    expect(buildPipelineGraph(tree).rootScopeDirPath).toBe(tree.dirPath);
  });

  it("buildFullGroupIndex indexes every epic group under the root", () => {
    const index = buildFullGroupIndex(buildFullTree("done"));
    expect(index.has("seamless-sync")).toBe(true);
    expect(index.has("seamless-workspaces")).toBe(true);
  });

  it("SW-FX-70: graph.readiness resolves a cross-epic story_id dep (id ≠ stem)", () => {
    // `depends_on: ["SW-OB-1"]` is a global story_id, and its target file
    // (`SW-OB-1-create-team-local.md`) lives in a different epic. The graph
    // resolves it against the full index the same way the edges do.
    function tree(dep1Status: string) {
      return buildTree("", {
        absoluteDirPath: "/repo",
        subEpics: [
          buildTree("teams", {
            dirPath: "teams",
            absoluteDirPath: "/repo/teams",
            stories: [
              buildStoryRef("SW-OB-1-create-team-local.md", {
                status: dep1Status,
                storyId: "SW-OB-1",
              }),
            ],
          }),
          buildTree("onboarding", {
            dirPath: "onboarding",
            absoluteDirPath: "/repo/onboarding",
            stories: [
              buildStoryRef("SW-OB-2-choose-your-team-state.md", {
                status: "todo",
                storyId: "SW-OB-2",
                dependsOn: ["SW-OB-1"],
              }),
            ],
          }),
        ],
      });
    }
    const ready = (dep1Status: string) => {
      const t = tree(dep1Status);
      return buildPipelineGraph(t).readiness(t.subEpics[1].stories[0], "onboarding");
    };
    expect(ready("done").ready).toBe(true);
    expect(ready("in-progress").ready).toBe(false);
    expect(ready("in-progress").blockedBy[0]).toMatchObject({ ref: "SW-OB-1", reason: "unmet" });
  });

  it("AC-6: a raw/scoped group index is rejected by the readiness query at compile time", () => {
    // Type-level assertions only — never executed at runtime. `tsc --noEmit`
    // validates each @ts-expect-error.
    const typecheckOnly = () => {
      const rawIndex = buildGroupIndex(buildFullTree("done"));
      const story = buildStoryRef("X-1.md", { status: "todo" });

      // The brand has teeth: `buildGroupIndex` returns a plain Map that is NOT
      // assignable to `FullGroupIndex`, so the readiness query rejects a
      // scoped/raw index. (Deleting the brand would make this line compile.)
      // @ts-expect-error — plain Map is not a FullGroupIndex.
      resolveReadiness(story, "", rawIndex, new Map());
    };
    expect(typeof typecheckOnly).toBe("function");
  });
});
