// SPDX-License-Identifier: MIT
import { getActiveWorkspaceContext } from "@/shared/hooks/use-active-workspace-context";
import { useViewerStore } from "@/shared/stores/viewer-store";

/**
 * Close a file tab in the active team, threading the active workspace's
 * best-effort synthetic `repoPath` for the post-close nav reveal.
 *
 * SW-FX-38: `closeTab` keys off `(teamKey, filePath)` — `repoPath` is used
 * ONLY by the post-close `revealOpenedFile`, which safely no-ops on an
 * unresolvable/`""` repo (`resolveRepo("") → undefined → early return`). So a
 * slug-less active workspace (`kind:"epic"`) or no focused
 * workspace — where `getActiveWorkspaceContext().repoPath` derives to `null` —
 * must STILL close the tab. `repoPath` is therefore NEVER a precondition; it
 * falls back to `""`.
 *
 * This is the single source of truth for the "close the active file tab"
 * operation. Route every close affordance (⌘W, the file-deleted placeholder,
 * …) through it so none can reintroduce the over-strict `&& repoPath` guard
 * that silently no-op'd the close in slug-less workspaces.
 *
 * No-ops on an empty/absent `filePath` so callers can pass a nullable tab path
 * without a local guard. (Note: `getActiveWorkspaceContext().repoPath` is the
 * synthetic key `workspace:{org}:{team}:{slug}`, NOT `workspace.sourceRepoPath`
 * — the M.2 on-disk clone path, which misses `workspaceRepos[...]` lookups.)
 */
export function closeActiveFileTab(filePath: string | null | undefined): void {
  if (!filePath) return;
  const { teamKey, repoPath } = getActiveWorkspaceContext();
  useViewerStore.getState().closeTab(teamKey, repoPath ?? "", filePath, { user: true });
}
