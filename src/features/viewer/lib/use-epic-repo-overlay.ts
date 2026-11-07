// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useRef } from "react";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { workspaceGroupKey } from "@/shared/lib/workspace-key";
import type { Workspace } from "@/shared/stores/viewer-store";

/** The slice of the active workspace the overlay controller needs. */
type ActiveEpicWorkspace = Pick<Workspace, "id" | "sourceRepoSlug" | "epicDirPath" | "epicDocPath">;

export interface EpicRepoOverlayController {
  /** Is the repo-config overlay open for this workspace id? */
  isOpen: (workspaceId: string) => boolean;
  /** Toggle the overlay (manual interaction — suppresses re-auto-show). */
  toggle: (workspaceId: string) => void;
  /** Close the overlay (manual — suppresses re-auto-show while it stays open). */
  close: (workspaceId: string) => void;
}

/**
 * SW-EW-15 (Work-stream B) — owns the epic repo-config overlay's open/evaluated
 * state for the active epic workspace, decoupled from the heavy team view so it
 * is unit-testable.
 *
 * SW-EW-18 AC-3: the OPEN state now lives in the viewer store (`epicReposOpen`,
 * keyed by team and workspace id) so the bottom-bar terminal zone in the global
 * `status-bar.tsx` can toggle it for the active epic-integration workspace. This
 * hook still owns the auto-show EVALUATION (the async repo read + the
 * evaluated/in-flight bookkeeping); it just reads/writes the open-set through the
 * store instead of local React state.
 *
 * Behavior:
 * - Auto-shows once when an epic workspace (slug-less integration with an
 *   `epicDirPath`) with ZERO repos becomes active. NON-BLOCKING: the repo count
 *   is read async, AFTER the terminal is already mounted.
 * - "Evaluated" is tracked in a REF, not a reactive effect dep — marking it can
 *   never retrigger the effect (and so can never cancel the in-flight read that
 *   reveals the overlay). Cancellation is tied to component UNMOUNT only.
 * - Prunes its evaluated set to `presentWorkspaceIds`, so closing an epic
 *   workspace forgets its evaluation → reopening a still-empty epic re-nudges.
 * - A manual toggle/close marks the workspace evaluated so auto-show won't
 *   re-fight the user while that workspace stays open.
 */
export function useEpicRepoOverlay(
  activeWorkspace: ActiveEpicWorkspace | null,
  presentWorkspaceIds: readonly string[],
  readEpicRepos: (epicDirPath: string) => Promise<string[]>,
  teamKey = "",
): EpicRepoOverlayController {
  // Subscribe to the store's open-set so a toggle from anywhere (this hook's
  // auto-show OR the bottom-bar zone) re-renders the overlay host.
  const keyOf = useCallback(
    (id: string) => (teamKey ? workspaceGroupKey(teamKey, id) : id),
    [teamKey],
  );
  const openMap = useViewerStore((s) => s.epicReposOpen);
  const evaluatedRef = useRef<Set<string>>(new Set());
  const inFlightRef = useRef<Set<string>>(new Set());
  const mountedRef = useRef(true);
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  // Forget the EVALUATED marker for workspaces that no longer exist, so a later
  // reopen (same deterministic id) re-evaluates the empty-state. Pruning a ref
  // (not state) means this never triggers a render. The store `open` entry isn't
  // pruned here — a stale id is harmless (nothing renders a closed workspace's
  // overlay, and `isOpen` is a membership check) and it's re-keyed on reopen.
  useEffect(() => {
    const present = new Set(presentWorkspaceIds.map(keyOf));
    for (const id of evaluatedRef.current) {
      if (teamKey && !id.startsWith(`${teamKey}|`)) continue;
      if (!present.has(id)) evaluatedRef.current.delete(id);
    }
  }, [presentWorkspaceIds, keyOf, teamKey]);

  const close = useCallback(
    (workspaceId: string) => {
      evaluatedRef.current.add(keyOf(workspaceId));
      useViewerStore.getState().setEpicReposOpen(keyOf(workspaceId), false);
    },
    [keyOf],
  );

  const toggle = useCallback(
    (workspaceId: string) => {
      evaluatedRef.current.add(keyOf(workspaceId));
      useViewerStore.getState().toggleEpicReposOpen(keyOf(workspaceId));
    },
    [keyOf],
  );

  useEffect(() => {
    const ws = activeWorkspace;
    // Only epic-level (slug-less) integration workspaces auto-show. Require
    // `epicDocPath` — without it `readEpicRepos` can't resolve the doc and would
    // throw, so there's nothing to evaluate (the row can't read its repos yet).
    if (!ws || ws.sourceRepoSlug || !ws.epicDirPath || !ws.epicDocPath) return;
    const workspaceId = keyOf(ws.id);
    // `evaluated` = a decision was reached; `inFlight` = a read is pending.
    // Skip if either holds so we never double-read or re-decide.
    if (evaluatedRef.current.has(workspaceId) || inFlightRef.current.has(workspaceId)) return;
    inFlightRef.current.add(workspaceId);
    readEpicRepos(ws.epicDirPath)
      .then((repos) => {
        if (!mountedRef.current) return;
        // Mark evaluated only on SUCCESS, so a transient read failure can retry
        // on a later render rather than permanently suppressing the nudge.
        evaluatedRef.current.add(workspaceId);
        // The evaluation is AUTHORITATIVE for this epoch: open when empty, and
        // clear any stale entry when not — so reopening a now-non-empty epic
        // (id pruned then re-evaluated) doesn't show a leftover overlay.
        const store = useViewerStore.getState();
        const isOpenNow = store.epicReposOpen[workspaceId] ?? false;
        if (repos.length === 0 && !isOpenNow) store.setEpicReposOpen(workspaceId, true);
        else if (repos.length > 0 && isOpenNow) store.setEpicReposOpen(workspaceId, false);
      })
      .catch(() => {
        // Read failed → leave UN-evaluated (retries later) and the overlay
        // as-is; the bottom-bar zone button is the manual fallback.
      })
      .finally(() => {
        inFlightRef.current.delete(workspaceId);
      });
  }, [activeWorkspace, readEpicRepos, keyOf]);

  const isOpen = useCallback(
    (workspaceId: string) => openMap[keyOf(workspaceId)] ?? false,
    [openMap, keyOf],
  );
  return { isOpen, toggle, close };
}
