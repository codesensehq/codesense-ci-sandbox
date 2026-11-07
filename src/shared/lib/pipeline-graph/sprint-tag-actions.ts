// SPDX-License-Identifier: MIT
// ---------------------------------------------------------------------------
// SW-SB-15 · The ONE sprint-tag action seam.
//
// Adding/removing/moving a story's `sprint:` is a single metadata write. This
// module is the single home for that write + its undo toast + its failure
// handling, so the two trigger surfaces — the left-nav context menu and the board
// card menu — can NEVER diverge on wording or behavior (AC-4). The board and nav
// differ only in the menu *primitive* they render; the action is identical.
//
// The actual write goes through the shared `writeStorySprint`/`clearStorySprint`
// field writers (the same layer `writeStoryPoints` lives in), so this seam only
// orchestrates: pick the write, toast, and undo by re-writing the prior value.
//
// No optimistic overlay: unlike a drag (SW-SB-10), a menu click doesn't need
// instant re-bucketing — the write is async + off the UI thread, and the caller's
// `refresh` (a watcher-suppressed `bumpFileRefresh`) re-scans within tens of ms.
// On failure nothing was mutated visually, so there is nothing to roll back — we
// simply toast the error and leave state untouched (AC-6: never a half-applied set).
// ---------------------------------------------------------------------------
import { toast } from "sonner";
import { getErrorMessage } from "@/shared/lib/error-message";
import { toastError } from "@/shared/lib/with-error-toast";
import { writeStorySprint, clearStorySprint } from "@/shared/lib/story-field-writer";
import { toastWithUndo, runUndo } from "@/shared/lib/undo-toast";

/** Write a story's sprint to a concrete id, or clear it when `value` is null —
 *  the single write primitive both the forward change and its undo go through. */
function writeSprintValue(filePath: string, value: string | null): Promise<void> {
  return value ? writeStorySprint(filePath, value) : clearStorySprint(filePath);
}

/**
 * Pure: the human toast label for a sprint change. `target` is the sprint id to
 * set, or `null` to remove. Trivially unit-tested.
 */
export function sprintToastLabel(prevSprint: string | null, target: string | null): string {
  if (!target) return "Removed from sprint";
  return prevSprint ? `Moved to ${target}` : `Added to ${target}`;
}

/**
 * Add / move / remove a SINGLE story's sprint (AC-1/AC-3/AC-5). Instant, no
 * confirm; on success shows a low-stakes toast with an Undo action (which re-writes
 * the prior sprint via the shared best-effort {@link runUndo}), on failure a plain
 * error toast (state unchanged). Resolves once the write settles.
 */
export function applySprintChange(
  filePath: string,
  prevSprint: string | null,
  target: string | null,
  refresh: () => void,
): Promise<void> {
  return writeSprintValue(filePath, target)
    .then(() => {
      refresh();
      toastWithUndo(
        sprintToastLabel(prevSprint, target),
        () =>
          void runUndo(
            () => writeSprintValue(filePath, prevSprint),
            refresh,
            "Couldn't undo sprint change",
          ),
      );
    })
    .catch((e: unknown) => {
      toastError("Couldn't update sprint: ", e);
    });
}

/** One story in a bulk operation: its path + the sprint it currently sits in. */
export interface BulkStoryEntry {
  filePath: string;
  prevSprint: string | null;
}

/** The outcome of a bulk tag — mirrors the `SwitchResult` succeeded/failed shape
 *  so a partial failure is reported honestly, never silently half-applied (AC-6). */
export interface SprintBulkResult {
  succeeded: string[];
  failed: { filePath: string; error: string }[];
}

/**
 * Tag many stories into `target` (AC-2 epic bulk). Writes SEQUENTIALLY — each
 * write is async + off the UI thread, so even a large epic never blocks the
 * renderer, and sequential ordering gives deterministic partial-failure reporting.
 * A story already in `target` is a no-op success. Refreshes ONCE at the end (never
 * one rescan per file) and shows a single summary toast whose Undo reverts ONLY the
 * stories that actually changed (each back to its own prior sprint).
 */
export async function bulkTagStories(
  entries: BulkStoryEntry[],
  target: string,
  refresh: () => void,
): Promise<SprintBulkResult> {
  const succeeded: string[] = [];
  const failed: { filePath: string; error: string }[] = [];
  const undos: BulkStoryEntry[] = [];

  for (const { filePath, prevSprint } of entries) {
    if (prevSprint === target) {
      succeeded.push(filePath); // already there — nothing to write
      continue;
    }
    try {
      await writeStorySprint(filePath, target);
      succeeded.push(filePath);
      undos.push({ filePath, prevSprint });
    } catch (e) {
      failed.push({ filePath, error: getErrorMessage(e) });
    }
  }

  if (succeeded.length > 0) refresh();

  if (undos.length > 0) {
    const label =
      failed.length === 0
        ? `Added ${undos.length} to ${target}`
        : `Added ${undos.length} to ${target} · ${failed.length} failed`;
    toastWithUndo(label, () => {
      // Revert every changed story to its own prior sprint, then one refresh.
      void Promise.allSettled(
        undos.map(({ filePath, prevSprint }) => writeSprintValue(filePath, prevSprint)),
      ).then(() => refresh());
    });
  } else if (failed.length > 0) {
    toast.error(`Couldn't add to ${target}: all ${failed.length} failed`);
  }

  return { succeeded, failed };
}
