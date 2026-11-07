// SPDX-License-Identifier: MIT
import { useEffect } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useOrgStore } from "@/shared/stores/org-store";
import { logger } from "@/shared/lib/logger";

const BASE_TITLE = "CodeSense";

// SF-8 (AC-1): keep the Tauri window title in sync with the active org +
// team chrome. Mounted once at the App.tsx top level alongside
// `useWorkspaceViewerSync`. Reads `activeOrgSlug` / `activeTeamName` /
// `orgs` directly from `useOrgStore` so the effect fires the instant the
// user clicks a team — no waiting on `loadAttachmentsMap` or
// `initWatchedCache`.

export function useWindowTitle(): void {
  const activeOrgSlug = useOrgStore((s) => s.activeOrgSlug);
  const activeTeamName = useOrgStore((s) => s.activeTeamName);
  const orgs = useOrgStore((s) => s.orgs);

  useEffect(() => {
    const title = computeTitle(activeOrgSlug, activeTeamName, orgs);
    getCurrentWindow()
      .setTitle(title)
      .catch((e) => {
        logger.warn("window-title", `setTitle failed: ${e}`);
      });
  }, [activeOrgSlug, activeTeamName, orgs]);
}

function computeTitle(
  activeOrgSlug: string | null,
  activeTeamName: string | null,
  orgs: Array<{ slug: string; display_name: string }>,
): string {
  if (activeOrgSlug === null) return BASE_TITLE;
  const org = orgs.find((o) => o.slug === activeOrgSlug);
  // Fall back to the slug if `display_name` is empty/missing — matches
  // the identity-menu convention.
  // SF-8 review iter-1 (BH#14): trim once and reuse the trimmed value so a
  // `display_name` with leading/trailing whitespace doesn't render padded
  // in the window title.
  const trimmedDisplayName = org?.display_name?.trim() ?? "";
  const orgLabel = trimmedDisplayName !== "" ? trimmedDisplayName : activeOrgSlug;
  const team = activeTeamName ?? "default";
  return `${BASE_TITLE} — ${orgLabel} / ${team}`;
}
