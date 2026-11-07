// SPDX-License-Identifier: MIT
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";

import { safeUnlisten } from "@/shared/lib/tauri-listeners";
import { WORKTREE_RUN_COMPLETED_EVENT, type WorktreeRunCompletedPayload } from "@/tauri/events";

import {
  deriveEpicBatchStatus,
  formatEpicFailureSummary,
  useEpicAssemblyStore,
} from "../stores/epic-assembly-store";
import { useSurfaceSyncStore } from "../stores/surface-sync-store";

/** Mirror an epic's settled failure set (create + install) onto its chip — the
 *  ambient echo (AC-4) for a backgrounded epic you're not currently viewing.
 *  Runs only once every install for the epic is terminal so the chip flips to a
 *  single combined message, not once per repo. Create failures were surfaced
 *  eagerly at open; this supersedes them with the full picture (or clears the
 *  chip if nothing failed after all). Reuses the repository-only `deriveEpicBatchStatus` so the "all terminal" + failure-set
 *  logic lives in ONE place. Exported so the seed wiring can reconcile the chip
 *  after draining any pre-seed-buffered completions (which the listener returned
 *  early for). */
export function syncEpicChipEcho(cwd: string): void {
  const entry = useEpicAssemblyStore.getState().byCwd[cwd];
  if (!entry) return;
  const status = deriveEpicBatchStatus(entry);
  if (!status.allInstallsTerminal) return; // wait until the batch settles
  const chip = useSurfaceSyncStore.getState();
  if (status.failures.length > 0) {
    chip.markAssemblyFailure(cwd, formatEpicFailureSummary(status.failures));
  } else {
    chip.clearAssemblyFailure(cwd);
  }
}

/**
 * SLW-CO-8 (LD-B): mount once at the app root. Subscribes to the shared
 * `worktree-run-completed` terminal events and routes each into the epic-scoped
 * {@link useEpicAssemblyStore} by `run_id`, then mirrors the settled failure set
 * onto the epic's chip. A completion whose `run_id` isn't a tracked epic install
 * (e.g. a story-worktree run, which learns completion from its own invoke
 * resolution) is ignored.
 */
export function useEpicAssemblyListener(): void {
  const markEpicInstall = useEpicAssemblyStore((s) => s.markEpicInstall);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;

    void listen<WorktreeRunCompletedPayload>(WORKTREE_RUN_COMPLETED_EVENT, (event) => {
      const { run_id, ok } = event.payload;
      const cwd = markEpicInstall(run_id, ok);
      if (cwd) syncEpicChipEcho(cwd);
    }).then((u) => {
      if (disposed) safeUnlisten(u);
      else unlisten = u;
    });

    return () => {
      disposed = true;
      if (unlisten) safeUnlisten(unlisten);
    };
  }, [markEpicInstall]);
}
