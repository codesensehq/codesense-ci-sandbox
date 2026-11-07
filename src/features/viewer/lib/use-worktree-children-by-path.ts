// SPDX-License-Identifier: MIT
import {
  useChildrenByPath,
  type ChildrenByPathResult,
} from "@/features/dashboard/lib/use-children-by-path";
import type { FileEntry } from "@/tauri/commands";

interface UseWorktreeChildrenByPathArgs {
  /** Absolute paths of directories currently expanded in the worktree tree. */
  expandedPaths: Set<string>;
  /** Bump to force a re-fetch of every expanded path (e.g. a manual refresh). */
  refreshKey?: number;
  /** A remote machine's lister; default the local `list_directory`. */
  list?: (dirPath: string) => Promise<FileEntry[]>;
}

export type WorktreeChildren = ChildrenByPathResult;

/** Identity transform: the worktree browser lists the WHOLE tree (Rust drops
 *  only OS cruft, secrets, and VCS-metadata dirs, and orders dirs-first
 *  natural-sorted), so no FE filter or re-sort — unlike the dashboard's pipeline
 *  filter (§A). Heavy build/dep dirs are de-emphasized by `isNoiseDir`, not
 *  dropped here. */
function keepAll(entries: FileEntry[]): FileEntry[] {
  return entries;
}

/**
 * SLW-WDE-1 (AC-2, §A) — lazy per-directory children fetch for the
 * `WorktreeFileTree`. A thin wrapper over the shared `useChildrenByPath` with an
 * identity transform (no viewer-file filter — that would hide most source
 * files). Returns `erroredPaths` too so the tree can render an inline error row
 * (AC-5) instead of a silent empty.
 */
export function useWorktreeChildrenByPath({
  expandedPaths,
  refreshKey,
  list,
}: UseWorktreeChildrenByPathArgs): WorktreeChildren {
  return useChildrenByPath({ expandedPaths, refreshKey, transform: keepAll, list });
}
