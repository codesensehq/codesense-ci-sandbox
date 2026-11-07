// SPDX-License-Identifier: MIT
import { useCallback } from "react";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { PipelineStatus } from "@/shared/lib/pipeline-graph/types";
import { openInEditorFromCwd } from "@/shared/lib/open-in-editor";

/**
 * Workspace callback wiring — binds store actions to a specific `teamKey`.
 * Consumed by `team-workspace-view` (threaded into the in-body terminal-tile
 * chrome + the bottom tray) to avoid duplicating the identical `useCallback`
 * wrappers.
 *
 * T.3: signature switched from `(repoPath)` to `(teamKey)`, so this hook
 * no longer derives its team from a `repoPath` prop. `onOpenStoryDoc` routes
 * through the store's `openStoryDocForTeam`, which resolves the team's content
 * anchor internally — so this hook no longer plumbs a `repoPath` at all.
 */
export function useWorkspaceBarCallbacks(teamKey: string) {
  const switchWorkspace = useViewerStore((s) => s.switchWorkspace);
  const requestCloseWorkspace = useViewerStore((s) => s.requestCloseWorkspace);
  const closeWorkspace = useViewerStore((s) => s.closeWorkspace);
  const changeStoryStatus = useViewerStore((s) => s.changeStoryStatus);

  const onSelectWorkspace = useCallback(
    (workspaceId: string) => switchWorkspace(teamKey, workspaceId),
    [teamKey, switchWorkspace],
  );
  const onCloseWorkspace = useCallback(
    (workspaceId: string) => requestCloseWorkspace(teamKey, workspaceId),
    [teamKey, requestCloseWorkspace],
  );
  const onForceCloseWorkspace = useCallback(
    (workspaceId: string) => closeWorkspace(teamKey, workspaceId),
    [teamKey, closeWorkspace],
  );
  const onStoryStatusChange = useCallback(
    (workspaceId: string, newStatus: PipelineStatus) =>
      changeStoryStatus(teamKey, workspaceId, newStatus),
    [teamKey, changeStoryStatus],
  );
  // SW-FX-10: route the CLI workspace-tab double-click through the shared
  // open-and-focus action (was the incidental `openFile`-only path that
  // re-centered only as a side-effect of an active-tab change). Now all three
  // story-bearing surfaces share one code path, and the already-active case
  // re-focuses here too. AC-6. SW-AM-2: via `openStoryDocForTeam`, which resolves
  // the team's content anchor internally (was an inline repoPath useMemo here).
  const onOpenStoryDoc = useCallback(
    (storyFilePath: string) =>
      useViewerStore.getState().openStoryDocForTeam(teamKey, storyFilePath),
    [teamKey],
  );

  return {
    onSelectWorkspace,
    onCloseWorkspace,
    onForceCloseWorkspace,
    onStoryStatusChange,
    onOpenStoryDoc,
    onOpenInEditor: openInEditorFromCwd,
  };
}
