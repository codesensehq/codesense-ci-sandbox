// SPDX-License-Identifier: MIT
import type { DerivedPipelineTree } from "./types";
import {
  stripPipelineExtension,
  scopeRelative as scopeRelativeUtil,
  docTypeFromFileName,
} from "./pipeline-utils";
import { buildStoryIdIndex } from "./story-id-index";

/**
 * Metadata for a single file in the group index. Stores raw scanner values —
 * consumers normalize as needed (e.g. `derived-tree-to-react-flow.ts`
 * normalizes `documentType` via its local closure to track unknown types).
 */
export type GroupIndexFile = {
  filePath: string;
  /** Raw `type` from YAML frontmatter, or null when absent. */
  documentType: string | null;
  title: string;
  /** Raw `status` from YAML frontmatter, or null when absent. */
  status: string | null;
  /** SW-SB-9: stable `story_id` (stories only; docs carry none). Drives
   *  location-independent `depends_on` resolution. */
  storyId?: string;
};

export type GroupIndexEntry = {
  dirName: string;
  files: Map<string, GroupIndexFile>;
};

/**
 * Build an index of all groups (sub-epic directories) in the pipeline tree,
 * keyed by scope-relative directory path.
 *
 * Unlike the former `indexGroups()` inside `derivedTreeToReactFlow`, this
 * shared version also indexes the root node at key `""` so root-level
 * stories can be looked up by sibling stem reference.
 *
 * @param tree - The pipeline tree to index.
 * @param scopeDirPath - The scope root's `dirPath` (defaults to `tree.dirPath`).
 */
export function buildGroupIndex(
  tree: DerivedPipelineTree,
  scopeDirPath?: string,
): Map<string, GroupIndexEntry> {
  const scope = scopeDirPath ?? tree.dirPath;
  const index = new Map<string, GroupIndexEntry>();

  function walk(node: DerivedPipelineTree): void {
    const ownerScopeRel = scopeRelativeUtil(node.dirPath, scope);
    const files = new Map<string, GroupIndexFile>();
    for (const s of node.stories) {
      files.set(s.fileName, {
        filePath: s.filePath,
        documentType: s.documentType,
        // R-14: scanner no longer populates `title` — production falls
        // through to filename-stripped stem. Deprecated test fixtures may
        // pass `title`; honour both paths.
        title: s.title ?? stripPipelineExtension(s.fileName),
        status: s.status,
        ...(s.storyId ? { storyId: s.storyId } : {}),
      });
    }
    // SW-SC-2: index `docs[]` (DocRefs — e.g. `.excalidraw` supporting docs,
    // and non-pipeline md/html) so cross-directory `supporting_docs:` refs to
    // them resolve. DocRefs carry no frontmatter, so synthesize `documentType`
    // from the extension and leave `status` null. Stories win on a name clash.
    for (const d of node.docs ?? []) {
      if (files.has(d.fileName)) continue;
      files.set(d.fileName, {
        filePath: d.filePath,
        documentType: docTypeFromFileName(d.fileName),
        title: stripPipelineExtension(d.fileName),
        status: null,
      });
    }
    index.set(ownerScopeRel, {
      dirName: node.dirName,
      files,
    });
    for (const sub of node.subEpics) walk(sub);
  }

  walk(tree);
  return index;
}

declare const FULL_GROUP_INDEX_BRAND: unique symbol;

/**
 * A group index proven to be built from the FULL pipeline tree, not a drilled
 * subtree. The brand is unforgeable outside `buildFullGroupIndex`, so any query
 * that requires a `FullGroupIndex` cannot be handed a scoped subset — a scoped
 * index is a compile-time error (SW-FX-1 AC-6). `buildGroupIndex` returns a
 * plain `Map` that is NOT assignable here without the sanctioned constructor.
 */
export type FullGroupIndex = Map<string, GroupIndexEntry> & {
  readonly [FULL_GROUP_INDEX_BRAND]: true;
};

/** The single sanctioned construction of a `FullGroupIndex`. Callers pass the
 *  full team-content tree (the scope-`""` scan), never a scoped subtree. */
export function buildFullGroupIndex(fullTree: DerivedPipelineTree): FullGroupIndex {
  return buildGroupIndex(fullTree, fullTree.dirPath) as FullGroupIndex;
}

/** Where a story lives in a group index: its owner group's scope-relative key
 *  plus its file-map key. The value type of a `buildGroupStoryIdIndex` entry. */
export type StoryIdLocation = { scopeRel: string; fileName: string };

/** A prebuilt `story_id → location` index (duplicate ids poisoned to `null`).
 *  Built ONCE per (full) group index and passed into readiness resolution —
 *  never rebuilt inside a per-story call (SW-FX-70 AC-7). */
export type StoryIdIndex = Map<string, StoryIdLocation | null>;

/**
 * SW-SB-9/SW-FX-70: build the `story_id → {scopeRel, fileName}` index for a group
 * index, poisoning duplicate ids to `null` via the shared `buildStoryIdIndex`
 * primitive (an ambiguous id resolves to nothing, never an arbitrary target).
 *
 * The ONE builder every story_id lookup flows through — the pipeline edge
 * transform (`derived-tree-to-react-flow.ts`) and the readiness resolver
 * (`resolve-nav-readiness.ts`) alike — so the `no-resolver-fork` guarantee holds:
 * edges and readiness can never disagree on which story an id maps to. Stories
 * with an empty/absent `storyId` are skipped by `buildStoryIdIndex`.
 */
export function buildGroupStoryIdIndex(
  groupIndex: ReadonlyMap<string, GroupIndexEntry>,
): StoryIdIndex {
  return buildStoryIdIndex(
    Array.from(groupIndex).flatMap(([scopeRel, entry]) =>
      Array.from(entry.files)
        .filter(([, file]) => file.storyId)
        .map(([fileName, file]) => [file.storyId, { scopeRel, fileName }] as const),
    ),
  );
}
