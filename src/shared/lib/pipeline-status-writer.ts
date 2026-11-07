// SPDX-License-Identifier: MIT
import { updatePipelineStatus } from "@/tauri/commands";
import type { PipelineStatus } from "@/shared/lib/pipeline-graph/types";

/**
 * Serialize story-status disk writes PER FILE so concurrent status changes land
 * in call order — the last intent wins on disk, not the last IPC to complete.
 *
 * Three surfaces write a story's `status` frontmatter to disk: the pipeline
 * board (`pipeline-viewer-panel`), the nav (`team-workspace-view`), and a
 * workspace card's popover (`viewer-store.changeStoryStatus`). Firing two writes
 * for the SAME file concurrently is a last-writer-wins-by-completion-order race:
 * disk could end at the older intent, and the next tree reconcile then pulls
 * that stale value back into memory — silently undoing the user's last action.
 *
 * Every writer routes through {@link writePipelineStatus}, which chains writes
 * for a given path so they run strictly in call order. Writes to DIFFERENT files
 * still run in parallel (the chain is keyed by path).
 */
const writeChains = new Map<string, Promise<unknown>>();

/**
 * Write `newStatus` to `storyFilePath`'s frontmatter, serialized against any
 * other in-flight write to the same file. Resolves/rejects with this write's own
 * outcome (so callers keep their optimistic-revert / toast handling); a failed
 * write never blocks a later intent for the same file.
 *
 * SW-SB-22: an optional `rank` is set in the SAME atomic Rust cycle (one
 * line-based status and rank write), so a
 * cross-column board drop persists status + position together (D-2). Omit `rank`
 * for a plain status write (today's behavior).
 */
export function writePipelineStatus(
  storyFilePath: string,
  newStatus: PipelineStatus,
  rank?: string,
): Promise<void> {
  const prior = writeChains.get(storyFilePath) ?? Promise.resolve();
  // Pass `rank` only when set, so a plain status write stays a 2-arg call — the
  // exact shipped behavior; only a cross-column drop adds the atomic rank.
  const write = () =>
    rank === undefined
      ? updatePipelineStatus(storyFilePath, newStatus)
      : updatePipelineStatus(storyFilePath, newStatus, rank);
  // Run after the prior write SETTLES (success OR failure) — both continuations
  // are `write`, so one write's rejection can't stall the queue for this file.
  const run = prior.then(write, write);
  writeChains.set(storyFilePath, run);
  // Drop the entry once this settles as the tail so the Map can't grow unbounded.
  void run
    .catch(() => {})
    .finally(() => {
      if (writeChains.get(storyFilePath) === run) writeChains.delete(storyFilePath);
    });
  return run;
}
