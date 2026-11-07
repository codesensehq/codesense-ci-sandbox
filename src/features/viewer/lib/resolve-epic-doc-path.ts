// SPDX-License-Identifier: MIT
import type { Workspace } from "@/shared/stores/viewer-store";

/** The record fields the resolver needs (a slice of `Workspace`). */
type EpicDocRecord = Pick<Workspace, "epicDirPath" | "epicDocPath">;

/**
 * SW-EW-15: resolve an epic's doc path, preferring the workspace RECORD's
 * `epicDocPath` over the nav-tree index.
 *
 * For a slug-less epic workspace the nav tree never scans (empty repoPath →
 * null tree → empty `epicDocPathIndex`), so the record's `epicDocPath` —
 * threaded on at open time — is the source of truth. Tree-driven (story/root)
 * callers have no matching record, so they fall back to the tree index; a
 * genuine tree miss still throws (the repo card surfaces it as an error state).
 */
export function resolveEpicDocPath(
  records: readonly EpicDocRecord[],
  treeIndex: ReadonlyMap<string, string>,
  epicDirPath: string,
): string {
  const fromRecord = records.find(
    (w) => w.epicDirPath === epicDirPath && w.epicDocPath,
  )?.epicDocPath;
  if (fromRecord) return fromRecord;
  const docPath = treeIndex.get(epicDirPath);
  if (!docPath) throw new Error(`No epic doc found for "${epicDirPath}"`);
  return docPath;
}
