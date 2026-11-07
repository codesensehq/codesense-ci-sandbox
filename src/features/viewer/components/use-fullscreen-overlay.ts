// SPDX-License-Identifier: MIT
import { useActiveTeamKey } from "@/shared/lib/team-key";
import { contentTeamKey } from "@/shared/lib/content-team-key";

/**
 * SW-SB-14: shared "is THIS right-pane view maximized?" rule, extracted from the
 * SW-PP-5 pipeline hook so the pipeline and the board resolve maximize identically
 * (one keying rule, two independent store scalars — AC-4).
 *
 * Derives the view's OWN team key — `contentTeamKey(repoPath)`,
 * NEVER the globally-active team key as the *key* (which resolves identically for
 * every mounted view) — and compares it to the caller-supplied maximized-team value.
 *
 * The overlay is honored ONLY for the FOREGROUND team (`ownTeamKey ===
 * useActiveTeamKey()`, both `orgSlug:team`). Every team's right pane stays mounted
 * across a switch (App.tsx / right-pane-stack `display:none` keep-alive), so without
 * this gate a backgrounded maximized team would keep a live document-level Esc
 * listener and an (invisible) `position:fixed` overlay. Gating on foreground means
 * only the visible view holds the overlay; the store scalar is left intact, so
 * returning to the team restores its maximize.
 *
 * Callers subscribe to the reactive store slice themselves (so the selector stays
 * in the caller) and pass the resolved team value in — see `useMaximized`, which maps
 * the generic `maximizedPanel` slice into this rule.
 */
export function useFullscreenOverlay(
  repoPath: string,
  fullscreenTeamKey: string | null,
): {
  ownTeamKey: string | null;
  isFullscreen: boolean;
} {
  const ownTeamKey = contentTeamKey(repoPath);
  const activeTeamKey = useActiveTeamKey();
  return {
    ownTeamKey,
    isFullscreen:
      ownTeamKey != null && fullscreenTeamKey === ownTeamKey && ownTeamKey === activeTeamKey,
  };
}
