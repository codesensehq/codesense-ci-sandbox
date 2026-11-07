// SPDX-License-Identifier: MIT
import { useEffect, useRef } from "react";
import { workspaceDocPath, type WorkspaceDocTarget } from "@/features/viewer/lib/story-doc-gesture";
import type { Workspace } from "@/shared/stores/viewer-store";

/** The active workspace as this hook reads it: the doc-source fields
 *  ({@link WorkspaceDocTarget}) plus the workspace `id`. */
type ActiveWorkspaceDoc = WorkspaceDocTarget & Pick<Workspace, "id">;

/** Resolve the change-key + doc for the active workspace. The key flips when
 *  EITHER the active workspace `id` OR its resolved doc changes — so re-focus
 *  fires on a genuine workspace switch even when two workspaces resolve to the
 *  same doc (the same story projected into two source repos → one
 *  `storyFilePath`, distinct ids), and on an in-place doc change (epic
 *  `epicDocPath` repoint / story `storyFilePath` rename / late resolution). It
 *  is `undefined` for "no active workspace" and for doc-less kinds, which never
 *  fire. The `\n` separator is unambiguous — neither a workspace id nor a
 *  filesystem path contains a newline, so the composite key can never collide. */
function resolveFocus(ws: ActiveWorkspaceDoc | null): {
  key: string | undefined;
  docPath: string | undefined;
} {
  const docPath = ws ? workspaceDocPath(ws) : undefined;
  return { key: ws && docPath ? [ws.id, docPath].join("\n") : undefined, docPath };
}

/**
 * SW-CW-5 — keep the viewer's focused doc in sync with the ACTIVE workspace.
 *
 * Opening/switching a workspace sets `activeWorkspace` and spawns its terminal
 * tab, but nothing touches the viewer `activeTab` — so a prior workspace's
 * README/story lingers. This effect closes that gap: whenever the active
 * workspace (or its own doc — `epicDocPath` for an epic, `storyFilePath` for a
 * story) changes, it opens that doc through the caller's `onOpenStoryDoc`
 * (→ `openFile`; the team view's opener passes `keepLayout`, so full-screen CLI
 * stays on).
 *
 * The guard keys on {@link resolveFocus}'s composite of workspace `id` +
 * resolved doc path, so it re-focuses on a real activation change (even between
 * two workspaces that share a doc) AND on an in-place doc change (repoint /
 * rename / a path that resolves shortly after the workspace becomes active).
 * Doc-less kinds (`root`/`generic` → `undefined`) never fire,
 * and an additive background open (SW-FX-69, which leaves `activeWorkspace`
 * unchanged) never steals the viewer.
 *
 * `lastFocusKeyRef` is seeded with the key active at mount, so a cold-boot /
 * team-switch restore does NOT clobber a persisted `activeTab`. Within a single
 * continuous activation the user can navigate to other files without being
 * yanked back (the key is unchanged); switching away and returning re-focuses
 * the workspace's own doc (AC-5). Close/selection policy stays owned by
 * SW-CW-4 — this only reacts to whichever workspace is active.
 *
 * Gated behind the opt-in `enabled` flag (the `focusWorkspaceDocOnActivation`
 * user setting, default OFF). The key is tracked even while disabled, so turning
 * the setting on applies from the NEXT activation rather than retroactively
 * yanking the viewer to the current workspace's doc.
 */
export function useFocusActiveWorkspaceDoc(
  activeWorkspace: ActiveWorkspaceDoc | null,
  onOpenStoryDoc: (filePath: string) => void,
  enabled: boolean,
): void {
  const lastFocusKeyRef = useRef(resolveFocus(activeWorkspace).key);
  useEffect(() => {
    const { key, docPath } = resolveFocus(activeWorkspace);
    if (key === lastFocusKeyRef.current) return;
    lastFocusKeyRef.current = key;
    if (enabled && docPath) onOpenStoryDoc(docPath);
  }, [enabled, activeWorkspace, onOpenStoryDoc]);
}
