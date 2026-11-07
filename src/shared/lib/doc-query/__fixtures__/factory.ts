import type { DerivedPipelineTree, DocRef, StoryRef } from "../../pipeline-graph/types";

/**
 * Compact factories for doc-query tests. Deliberately local to doc-query so the
 * query suite isn't coupled to the pipeline-graph scanner fixtures.
 */

/** A plain doc / binary leaf (`node.docs` entry) — a `supporting_docs` target
 *  that is NOT a pipeline story (arch/README/mockup/image/pdf/.excalidraw). */
export function makeDoc(absoluteDir: string, fileName: string): DocRef {
  return { fileName, filePath: `${absoluteDir}/${fileName}` };
}

export function makeStory(
  absoluteDir: string,
  fileName: string,
  overrides: Partial<StoryRef> = {},
): StoryRef {
  return {
    fileName,
    filePath: `${absoluteDir}/${fileName}`,
    dependsOn: null,
    status: null,
    documentType: "story",
    supportingDocs: null,
    sourceRepo: null,
    team: null,
    openTags: null,
    ...overrides,
  };
}

export function makeEpic(
  absoluteDirPath: string,
  dirName: string,
  stories: StoryRef[],
  subEpics: DerivedPipelineTree[] = [],
  docs: DocRef[] = [],
): DerivedPipelineTree {
  return {
    dirPath: dirName,
    dirName,
    absoluteDirPath,
    stories,
    subEpics,
    docs,
    orderError: null,
    mainDoc: null,
  };
}

/**
 * A canonical two-epic tree exercising: open tags + schema tag (`context_level`),
 * forward supporting_docs (to a story, a resolved plain-doc/leaf in `docs`, and
 * an unresolved dead ref), backlinks, same-dir depends_on, cross-epic
 * depends_on, and every status class. Reused by the cross-language golden (AC-12).
 */
export function sampleTree(): DerivedPipelineTree {
  const A = "/ctx/epic-a";
  const B = "/ctx/epic-b";

  const epicA = makeEpic(
    A,
    "epic-a",
    [
      makeStory(A, "a1-foundation.md", {
        storyId: "A-1",
        status: "todo",
        openTags: { context_level: ["team"], area: ["query"] },
        dependsOn: [],
        // → a story (resolved), a real leaf in docs (resolved), a dead ref (unresolved).
        supportingDocs: ["./shared-notes.md", "./arch-notes.excalidraw", "./diagram.bin"],
        // SW-DX-9 (v2): freshness fields — mtime in NANOSECONDS on StoryRef
        // (surfaces as ms on DocSummary via the Rust query_docs), sourceRepo present.
        sourceRepo: "alderco/codesense",
        mtime: 1730000000000000000,
        // SW-SB-1 (v3): board metadata — sprint + assignedTo (strings) set here
        // so the cross-language golden exercises present values; other docs
        // leave them null. `points` is deliberately NOT exercised in the golden:
        // an integer-valued f64 serializes as `5` from JS (JSON.stringify) but
        // `5.0` from Rust, and serde_json::Value treats those as unequal — so
        // points value-threading is covered by unit tests on both sides instead
        // (see the Rust scanner/to_summary tests). The
        // `points` KEY still rides the golden (null) for shape parity.
        sprint: "2026-08-A",
        assignedTo: "octocat-dev",
      }),
      makeStory(A, "a2-consumer.md", {
        storyId: "A-2",
        status: "in-progress",
        openTags: { context_level: ["org"] },
        dependsOn: ["a1-foundation"],
        supportingDocs: ["./a1-foundation.md"],
        // sourceRepo present but no mtime → DocSummary.mtime === null (mixed case).
        sourceRepo: "alderco/codesense",
      }),
      makeStory(A, "shared-notes.md", {
        storyId: "A-S",
        status: "done",
        documentType: "doc",
        openTags: { area: ["query", "docs"] },
      }),
    ],
    [], // no sub-epics
    // A resolved non-story supporting-doc target (a binary leaf in `docs`).
    [makeDoc(A, "arch-notes.excalidraw")],
  );

  const epicB = makeEpic(B, "epic-b", [
    makeStory(B, "b1-downstream.md", {
      storyId: "B-1",
      status: "blocked",
      openTags: { context_level: ["team"] },
      dependsOn: ["../epic-a/a1-foundation"],
    }),
    makeStory(B, "b2-custom.md", {
      storyId: "B-2",
      status: "shipped", // off-vocabulary status — must be retained literally
      openTags: { context_level: ["galaxy"] }, // off-vocabulary schema value
    }),
  ]);

  return makeEpic("/ctx", "", [], [epicA, epicB]);
}
