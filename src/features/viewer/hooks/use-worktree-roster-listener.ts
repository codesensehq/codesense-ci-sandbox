// SPDX-License-Identifier: MIT
import { useEffect } from "react";

import { listen } from "@tauri-apps/api/event";

import { createTrailingCoalescer } from "@/features/viewer/lib/session-events";
import { createListenerGroup } from "@/shared/lib/tauri-listeners";
import { WORKTREE_CHANGED_EVENT } from "@/tauri/events";

import { useWorktreeRosterStore } from "../stores/worktree-roster-store";

/** Trailing-edge coalesce window (ms) for worktree roster refetches — matches the agent
 *  roster's, so a burst (a teardown sweep) collapses into ONE enumeration. */
export const WORKTREE_ROSTER_COALESCE_MS = 200;

/**
 * SW-WU-9: the ONE subscriber to the worktree roster lifecycle events. Mounted exactly
 * once at the app root (in `App.tsx`, beside `useAgentRosterListener`), it warms
 * {@link useWorktreeRosterStore} on mount and keeps it live — so the Worktrees pane reads
 * one always-fresh array with no manual Refresh, mirroring SW-AM-9/AM-12's agent roster.
 *
 * `worktree-changed` is a payloadless nudge, so it fires a re-snapshot (event →
 * coalesced refetch, not event → patch): a worktree left/joined the managed set (manual
 * remove, or the SW-WU-14 `.git/worktrees/` watcher catching an external
 * `git worktree add`).
 *
 * Because the store's `reload` is gen-guarded, the StrictMode double-mount's second fetch
 * simply supersedes the first.
 */
export function useWorktreeRosterListener(): void {
  useEffect(() => {
    const group = createListenerGroup();
    const coalescer = createTrailingCoalescer(
      () => void useWorktreeRosterStore.getState().reload(),
      WORKTREE_ROSTER_COALESCE_MS,
    );

    // Warm the store on mount so a cold-opened pane shows current truth.
    void useWorktreeRosterStore.getState().reload();

    group.track(listen(WORKTREE_CHANGED_EVENT, () => coalescer.schedule()));

    return () => {
      coalescer.cancel();
      group.dispose();
    };
  }, []);
}
