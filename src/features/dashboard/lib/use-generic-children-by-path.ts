// SPDX-License-Identifier: MIT
import type { FileEntry } from "@/tauri/commands";
import { isDerivedRenderArtifact } from "@/shared/lib/viewer-file-type";
import { compareNavNames } from "./nav-ordering";
import { useChildrenByPath } from "./use-children-by-path";

interface UseGenericChildrenByPathArgs {
  /** Absolute paths of generic directories that are currently expanded. */
  expandedPaths: Set<string>;
  /** Bumped by the filesystem watcher to force a re-fetch of every expanded path. */
  refreshKey?: number;
}

/** Stable empty array — the pipeline nav keeps its legacy "errored dir reads as
 *  empty" contract (vs. the worktree tree, which surfaces an error row). */
const EMPTY: FileEntry[] = [];

/**
 * Pipeline nav's per-directory transform (SW-VE-1): lists EVERY real child —
 * directories and files of any extension — minus the intentional noise
 * exclusions (agent-derived `.excalidraw.png` renders, `CLAUDE.md`),
 * natural-sorted.
 *
 * The `isViewerFile` gate was deliberately removed here: **listing is not
 * rendering.** Whether a listed file can be shown prettily is the viewer's
 * decision (a non-viewable file lists in the tree and shows a "no preview"
 * placeholder on select) — never the tree's. Re-adding a viewability filter here
 * resurrects the `bmad/_config`-shows-nothing empty-folder bug for whatever
 * extension it excludes. Exported for direct unit coverage.
 */
export function pipelineTransform(entries: FileEntry[]): FileEntry[] {
  return entries
    .filter((e) => !isDerivedRenderArtifact(e.name) && e.name !== "CLAUDE.md")
    .sort((a, b) => compareNavNames(a.name, b.name));
}

/**
 * Lifts the per-directory children fetch that used to live inside the recursive
 * `GenericFileEntry` component to a `PipelineStatusNav`-level keyed Map. Returns
 * `Map<absolutePath, FileEntry[]>` containing only currently-expanded paths, each
 * filtered/sorted to the pipeline nav's rules (viewer files + directories, no
 * CLAUDE.md). Result preserves Map identity across content-equal updates.
 *
 * The delta-fetch / prune / stale mechanics live in the shared `useChildrenByPath`
 * (SLW-WDE-1 refactor); this hook is the pipeline-filter wrapper.
 */
export function useGenericChildrenByPath({
  expandedPaths,
  refreshKey,
}: UseGenericChildrenByPathArgs): Map<string, FileEntry[]> {
  return useChildrenByPath({
    expandedPaths,
    refreshKey,
    transform: pipelineTransform,
    errorValue: EMPTY,
  }).childrenByPath;
}
