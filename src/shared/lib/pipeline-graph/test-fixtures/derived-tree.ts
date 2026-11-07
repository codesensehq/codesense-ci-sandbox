import type { DerivedPipelineTree, MainDoc, StoryRef } from "../types";

// Shared builders for tests that exercise derivedTreeToReactFlow, usePipelineTree
// and the nav (story 30.7).
//
// Every builder is pure, deterministic, and returns plain objects — no
// store state, no side effects. Prefer these helpers over hand-constructed
// literals so test fixtures stay in sync with schema changes.

const DEFAULT_ABS_ROOT = "/ctx/pipe";

function absPathFor(dirPath: string, fileName: string): string {
  return dirPath.length === 0
    ? `${DEFAULT_ABS_ROOT}/${fileName}`
    : `${DEFAULT_ABS_ROOT}/${dirPath}/${fileName}`;
}

export function buildStoryRef(
  fileName: string,
  opts: {
    /** @deprecated R-14: title is no longer produced by the scanner.
     *  Threaded through for test compat — production code never sees this
     *  populated. Tests that need a title should fetch it via
     *  `useStoryDetails` instead. */
    title?: string | null;
    absolutePath?: string;
    dependsOn?: string[] | null;
    status?: string | null;
    /** @deprecated R-14: storyText is no longer produced by the scanner.
     *  Threaded through for test compat — production code never sees this
     *  populated. */
    storyText?: string | null;
    documentType?: string | null;
    /** YAML field is `supporting_docs` (snake_case); camelCase here per Rust `rename_all`. */
    supportingDocs?: string[] | null;
    sourceRepo?: string | null;
    team?: string | null;
    /** SW-SB-1: sprint tag — the field the board/pipeline sprint filters read. */
    sprint?: string | null;
    /** Stable display id — the location-independent `depends_on` key (SW-SB-9). */
    storyId?: string;
  } = {},
): StoryRef {
  return {
    fileName,
    filePath: opts.absolutePath ?? `${DEFAULT_ABS_ROOT}/${fileName}`,
    title: opts.title ?? null,
    dependsOn: opts.dependsOn ?? null,
    status: opts.status ?? null,
    storyText: opts.storyText ?? null,
    documentType: opts.documentType ?? null,
    supportingDocs: opts.supportingDocs ?? null,
    sourceRepo: opts.sourceRepo ?? null,
    team: opts.team ?? null,
    ...(opts.sprint !== undefined ? { sprint: opts.sprint } : {}),
    ...(opts.storyId !== undefined ? { storyId: opts.storyId } : {}),
  };
}

export type BuildTreeOptions = {
  stories?: StoryRef[];
  /** PTS-2 follow-up: non-pipeline md/html docs at this dir. Default []. */
  docs?: DerivedPipelineTree["docs"];
  subEpics?: DerivedPipelineTree[];
  orderError?: string | null;
  /** Path relative to pipelineRoot. Defaults to "". */
  dirPath?: string;
  /** Absolute path for this directory. Defaults to `${DEFAULT_ABS_ROOT}/<dirPath>`. */
  absoluteDirPath?: string;
  /** Epic main doc metadata. Defaults to null. */
  mainDoc?: MainDoc | null;
};

export function buildTree(dirName: string, opts: BuildTreeOptions = {}): DerivedPipelineTree {
  const dirPath = opts.dirPath ?? "";
  const absoluteDirPath =
    opts.absoluteDirPath ??
    (dirPath.length === 0 ? DEFAULT_ABS_ROOT : `${DEFAULT_ABS_ROOT}/${dirPath}`);
  return {
    dirPath,
    dirName,
    absoluteDirPath,
    stories: opts.stories ?? [],
    docs: opts.docs ?? [],
    subEpics: opts.subEpics ?? [],
    orderError: opts.orderError ?? null,
    mainDoc: opts.mainDoc ?? null,
  };
}

// ----- Ready-made fixtures -------------------------------------------------

export const fixtureEmptyTree: DerivedPipelineTree = buildTree("");

export const fixtureTwoStoriesNoOrder: DerivedPipelineTree = buildTree("", {
  stories: [
    buildStoryRef("1-alpha.md", { title: "Alpha" }),
    buildStoryRef("2-beta.md", { title: "Beta" }),
  ],
});

export const fixtureNestedEpics: DerivedPipelineTree = buildTree("", {
  stories: [buildStoryRef("root.md", { title: "Root Story" })],
  subEpics: [
    buildTree("epic-a", {
      dirPath: "epic-a",
      stories: [
        buildStoryRef("a1.md", {
          title: "A1",
          absolutePath: absPathFor("epic-a", "a1.md"),
        }),
      ],
    }),
    buildTree("epic-b", {
      dirPath: "epic-b",
      stories: [
        buildStoryRef("b1.md", {
          title: "B1",
          absolutePath: absPathFor("epic-b", "b1.md"),
        }),
      ],
    }),
  ],
});

export const fixtureThreeLevelsDeep: DerivedPipelineTree = buildTree("", {
  subEpics: [
    buildTree("level-1", {
      dirPath: "level-1",
      subEpics: [
        buildTree("level-2", {
          dirPath: "level-1/level-2",
          subEpics: [
            buildTree("level-3", {
              dirPath: "level-1/level-2/level-3",
              stories: [
                buildStoryRef("leaf.md", {
                  title: "Leaf",
                  absolutePath: absPathFor("level-1/level-2/level-3", "leaf.md"),
                }),
              ],
            }),
          ],
        }),
      ],
    }),
  ],
});
