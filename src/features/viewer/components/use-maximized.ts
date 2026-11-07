// SPDX-License-Identifier: MIT
import { contentTeamKey } from "@/shared/lib/content-team-key";
import { useViewerStore } from "@/shared/stores/viewer-store";

import { useFullscreenOverlay } from "./use-fullscreen-overlay";

/**
 * SW-WG-9: generic "is THIS panel maximized?" hook — the single replacement for the
 * per-panel `usePipelineFullscreen` / `useBoardFullscreen` / `useAutomationsFullscreen`
 * hooks (and the pipeline-config inline read).
 *
 * Reads the generic `maximizedPanel` slice (keyed by the view's OWN team) and maps it
 * into the shared {@link useFullscreenOverlay} keying rule: this panel is maximized iff
 * the team's maximized panel id equals `panelId` AND the team is foregrounded. The
 * foreground gate + own-identity derivation live in `useFullscreenOverlay` (consumed
 * unchanged). Returns `ownTeamKey` too, since several consumers key team-scoped render
 * on it (drill controls, sprint selector, display tree, toolbar).
 */
export function useMaximized(
  panelId: string,
  repoPath: string,
): {
  ownTeamKey: string | null;
  isFullscreen: boolean;
} {
  const ownTeamKey = contentTeamKey(repoPath);
  // The team key for which THIS panel is the maximized one, else null — fed into the
  // generic keying rule (a stable primitive, so the selector stays cheap/equal).
  const maximizedTeamKey = useViewerStore((s) =>
    ownTeamKey && s.maximizedPanel[ownTeamKey] === panelId ? ownTeamKey : null,
  );
  return useFullscreenOverlay(repoPath, maximizedTeamKey);
}
