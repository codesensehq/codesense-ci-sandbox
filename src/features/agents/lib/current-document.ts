// SPDX-License-Identifier: MIT
import { getActiveTeamKey } from "@/shared/lib/team-key";
import { isVirtualTabPath } from "@/shared/lib/virtual-viewer-registry";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { isUnderContentRoot, toContentRelative } from "@/features/viewer/lib/note-links";

/**
 * The document the user was last viewing in the active team, as a content-relative path: the
 * newest entry of the team's navigation history, up to where they are, that is a file under
 * `contentRoot` (the Agents tab itself and other virtual tabs are skipped). `null` when there is
 * none, or when no content root is given.
 */
export function useCurrentDocument(contentRoot: string): string | null {
  return useViewerStore((s) => {
    if (!contentRoot) return null;
    const teamKey = getActiveTeamKey();
    const history = s.navHistory[teamKey] ?? [];
    const index = s.navIndex[teamKey] ?? history.length - 1;
    for (let i = Math.min(index, history.length - 1); i >= 0; i -= 1) {
      const entry = history[i];
      if (isVirtualTabPath(entry) || !isUnderContentRoot(entry, contentRoot)) continue;
      const relative = toContentRelative(entry, contentRoot);
      if (relative) return relative;
    }
    return null;
  });
}
