// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import {
  collectUnmetDeps,
  countDependencies,
  formatBlockedDep,
  readyTitle,
  resolveReadiness,
  waitingTitle,
} from "./resolve-nav-readiness";
import { buildGroupIndex, buildFullGroupIndex, buildGroupStoryIdIndex } from "./build-group-index";
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

describe("collectUnmetDeps — cross-epic group exists but file missing", () => {
  it("reports the dep as blocking when the target group exists but the file does not", () => {
    const tree = makeTree({
      subEpics: [
        makeTree({
          dirPath: "workspaces",
          dirName: "workspaces",
          absoluteDirPath: "/repo/workspaces",
          stories: [makeStory("W-5.md", { status: "done" })],
        }),
        makeTree({
          dirPath: "nav",
          dirName: "nav",
          absoluteDirPath: "/repo/nav",
          stories: [
            makeStory("N-4.md", { status: "todo", dependsOn: ["../workspaces/W-99-missing"] }),
          ],
        }),
      ],
    });
    const index = buildGroupIndex(tree);
    const story = tree.subEpics[1].stories[0];
    expect(collectUnmetDeps(story, "nav", index, buildGroupStoryIdIndex(index))).not.toEqual([]);
  });
});

describe("collectUnmetDeps / resolveReadiness — blocker reasons (SW-FX-1)", () => {
  function navTree(depRef: string, targetStatus: string) {
    return makeTree({
      subEpics: [
        makeTree({
          dirPath: "workspaces",
          dirName: "workspaces",
          absoluteDirPath: "/repo/workspaces",
          stories: [makeStory("W-5.md", { status: targetStatus })],
        }),
        makeTree({
          dirPath: "nav",
          dirName: "nav",
          absoluteDirPath: "/repo/nav",
          stories: [makeStory("N-4.md", { status: "todo", dependsOn: [depRef] })],
        }),
      ],
    });
  }

  it("AC-3: a resolved dep with status ≠ done is an `unmet` blocker carrying the status", () => {
    const tree = navTree("../workspaces/W-5", "in-progress");
    const index = buildFullGroupIndex(tree);
    const story = tree.subEpics[1].stories[0];

    const blocked = collectUnmetDeps(story, "nav", index, buildGroupStoryIdIndex(index));
    expect(blocked).toEqual([{ ref: "../workspaces/W-5", reason: "unmet", status: "in-progress" }]);
    expect(formatBlockedDep(blocked[0])).toBe("../workspaces/W-5 (in-progress)");
  });

  it("AC-2: a present group with a missing file is an `unresolved` blocker (still not met)", () => {
    const tree = navTree("../workspaces/W-99-missing", "done");
    const index = buildFullGroupIndex(tree);
    const story = tree.subEpics[1].stories[0];

    const blocked = collectUnmetDeps(story, "nav", index, buildGroupStoryIdIndex(index));
    expect(blocked).toEqual([{ ref: "../workspaces/W-99-missing", reason: "unresolved" }]);
    expect(resolveReadiness(story, "nav", index, buildGroupStoryIdIndex(index)).ready).toBe(false);
    expect(formatBlockedDep(blocked[0])).toBe("../workspaces/W-99-missing (unresolved)");
  });

  it("resolveReadiness: ready with an empty blocker list when all deps are done", () => {
    const tree = navTree("../workspaces/W-5", "done");
    const index = buildFullGroupIndex(tree);
    const story = tree.subEpics[1].stories[0];

    expect(resolveReadiness(story, "nav", index, buildGroupStoryIdIndex(index))).toEqual({
      ready: true,
      blockedBy: [],
    });
  });

  it("skips a path-qualified self-reference (a story can't block itself)", () => {
    // N-4.md in epic `nav` lists its own qualified path. The bare-stem self-ref
    // guard wouldn't catch this; the identity check must.
    const tree = makeTree({
      subEpics: [
        makeTree({
          dirPath: "nav",
          dirName: "nav",
          absoluteDirPath: "/repo/nav",
          stories: [makeStory("N-4.md", { status: "todo", dependsOn: ["../nav/N-4"] })],
        }),
      ],
    });
    const index = buildFullGroupIndex(tree);
    const story = tree.subEpics[0].stories[0];

    expect(collectUnmetDeps(story, "nav", index, buildGroupStoryIdIndex(index))).toEqual([]);
    expect(resolveReadiness(story, "nav", index, buildGroupStoryIdIndex(index))).toEqual({
      ready: true,
      blockedBy: [],
    });
  });
});

describe("collectUnmetDeps — story_id resolution (SW-FX-70)", () => {
  // The real regression shape (SW-SB-18 codemod): `depends_on` carries a global
  // `story_id` that is NOT the target's filename stem — e.g. story_id `SW-OB-1`,
  // file `SW-OB-1-create-team-local.md`. Every fixture below models story_id ≠ stem.
  function onboardingTree(dep1Status: string, extra: StoryRef[] = []): DerivedPipelineTree {
    return makeTree({
      subEpics: [
        makeTree({
          dirPath: "onboarding",
          dirName: "onboarding",
          absoluteDirPath: "/repo/onboarding",
          stories: [
            makeStory("SW-OB-1-create-team-local.md", {
              status: dep1Status,
              storyId: "SW-OB-1",
            }),
            makeStory("SW-OB-2-choose-your-team-state.md", {
              status: "todo",
              storyId: "SW-OB-2",
              dependsOn: ["SW-OB-1"],
            }),
            ...extra,
          ],
        }),
      ],
    });
  }

  /** The story-under-test (SW-OB-2) plus a prebuilt full + story_id index. */
  function fixture(tree: DerivedPipelineTree) {
    const index = buildFullGroupIndex(tree);
    return {
      story: tree.subEpics[0].stories[1],
      index,
      sidx: buildGroupStoryIdIndex(index),
    };
  }

  it("AC-1: a story_id dep whose target is `done` is met (no blocker, ready)", () => {
    const { story, index, sidx } = fixture(onboardingTree("done"));
    expect(collectUnmetDeps(story, "onboarding", index, sidx)).toEqual([]);
    expect(resolveReadiness(story, "onboarding", index, sidx)).toEqual({
      ready: true,
      blockedBy: [],
    });
  });

  it("AC-1: a story_id dep whose target is not `done` is `unmet` carrying that status", () => {
    const { story, index, sidx } = fixture(onboardingTree("in-progress"));
    expect(collectUnmetDeps(story, "onboarding", index, sidx)).toEqual([
      { ref: "SW-OB-1", reason: "unmet", status: "in-progress" },
    ]);
    expect(resolveReadiness(story, "onboarding", index, sidx).ready).toBe(false);
  });

  it("AC-4: a bare ref matching no stem AND no story_id stays `unresolved`", () => {
    const tree = onboardingTree("done");
    tree.subEpics[0].stories[1].dependsOn = ["SW-OB-404"];
    const { story, index, sidx } = fixture(tree);
    expect(collectUnmetDeps(story, "onboarding", index, sidx)).toEqual([
      { ref: "SW-OB-404", reason: "unresolved" },
    ]);
  });

  it("AC-4: a poisoned (duplicate-claimed) story_id resolves to nothing → `unresolved`", () => {
    // Two stories claim `SW-DUP` → the shared index poisons it to null, so the
    // dep can never bind to an arbitrary target.
    const tree = onboardingTree("done", [
      makeStory("SW-DUP-a.md", { status: "done", storyId: "SW-DUP" }),
      makeStory("SW-DUP-b.md", { status: "done", storyId: "SW-DUP" }),
    ]);
    tree.subEpics[0].stories[1].dependsOn = ["SW-DUP"];
    const { story, index, sidx } = fixture(tree);
    expect(collectUnmetDeps(story, "onboarding", index, sidx)).toEqual([
      { ref: "SW-DUP", reason: "unresolved" },
    ]);
  });

  it("AC-4: a story listing its OWN story_id is skipped (can't block itself)", () => {
    const tree = onboardingTree("done");
    tree.subEpics[0].stories[1].dependsOn = ["SW-OB-2"]; // its own story_id
    const { story, index, sidx } = fixture(tree);
    expect(collectUnmetDeps(story, "onboarding", index, sidx)).toEqual([]);
    expect(resolveReadiness(story, "onboarding", index, sidx).ready).toBe(true);
  });

  it("AC-5: a story_id dep whose target lives in ANOTHER epic still resolves (full index)", () => {
    const tree = makeTree({
      subEpics: [
        makeTree({
          dirPath: "teams",
          dirName: "teams",
          absoluteDirPath: "/repo/teams",
          stories: [
            makeStory("SW-OB-1-create-team-local.md", { status: "done", storyId: "SW-OB-1" }),
          ],
        }),
        makeTree({
          dirPath: "onboarding",
          dirName: "onboarding",
          absoluteDirPath: "/repo/onboarding",
          stories: [
            makeStory("SW-OB-2-choose-your-team-state.md", {
              status: "todo",
              storyId: "SW-OB-2",
              dependsOn: ["SW-OB-1"],
            }),
          ],
        }),
      ],
    });
    const index = buildFullGroupIndex(tree);
    const sidx = buildGroupStoryIdIndex(index);
    const story = tree.subEpics[1].stories[0];
    expect(resolveReadiness(story, "onboarding", index, sidx)).toEqual({
      ready: true,
      blockedBy: [],
    });
  });

  it("AC-3: a sibling filename stem still wins over story_id (precedence: stem → story_id)", () => {
    // `N-1` is BOTH a sibling stem (file N-1.md, not done) AND could collide with
    // a story_id elsewhere. Stem resolves first → the sibling's status is what blocks.
    const tree = makeTree({
      subEpics: [
        makeTree({
          dirPath: "onboarding",
          dirName: "onboarding",
          absoluteDirPath: "/repo/onboarding",
          stories: [
            makeStory("N-1.md", { status: "in-progress", storyId: "SW-OB-9" }),
            makeStory("N-2.md", { status: "todo", storyId: "SW-OB-2", dependsOn: ["N-1"] }),
          ],
        }),
      ],
    });
    const index = buildFullGroupIndex(tree);
    const sidx = buildGroupStoryIdIndex(index);
    const story = tree.subEpics[0].stories[1];
    expect(collectUnmetDeps(story, "onboarding", index, sidx)).toEqual([
      { ref: "N-1", reason: "unmet", status: "in-progress" },
    ]);
  });
});

describe("readiness wording", () => {
  it("counts each distinct dependency once and never the story itself", () => {
    const story = makeStory("S-2.md", {
      storyId: "S-2",
      dependsOn: ["S-1", "./S-1.md", "S-2", "../other/O-1"],
    });
    expect(countDependencies(story)).toBe(2);
    expect(readyTitle(countDependencies(story))).toBe("Ready to start · all 2 dependencies done");
    expect(readyTitle(1)).toBe("Ready to start · its dependency is done");
    expect(readyTitle(0)).toBe("Ready to start · no dependencies");
  });

  it("names what a waiting story waits on", () => {
    expect(
      waitingTitle([
        { ref: "S-1", reason: "unmet", status: "in-progress" },
        { ref: "X-9", reason: "unresolved" },
      ]),
    ).toBe("Waiting on S-1 (in-progress), X-9 (unresolved)");
  });
});
