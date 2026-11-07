// SPDX-License-Identifier: MIT
import { isInboxDashboard } from "@/features/dashboard/lib/dashboard-profile";
import { parseTeamKey } from "@/shared/lib/team-key";
import { useOrgStore } from "@/shared/stores/org-store";
import { listPanes } from "@/tauri/commands";
import { parsePanePanelId } from "./panes/pane-store";
import { coerceRightPaneLayout } from "./right-pane-layout";

/** Saved layouts contain ids, not installedFrom metadata. Discover renamed
 * Inbox copies once at hydration, before the pure migration and reconcile. */
export async function loadSavedInboxPanelIds(
  layout: unknown,
  fallbackTeamKey?: string | null,
): Promise<string[]> {
  const ids =
    coerceRightPaneLayout(layout)?.sections.flatMap((section) => section.group.panelIds) ?? [];
  const sources = new Map<string, { org: string; team: string }>();
  const fallback = fallbackTeamKey ? parseTeamKey(fallbackTeamKey) : null;
  const state = useOrgStore.getState();
  for (const id of ids) {
    const ref = parsePanePanelId(id);
    if (!ref || ref.id === "inbox") continue;
    const parsed = ref.teamKey ? parseTeamKey(ref.teamKey) : null;
    const org = parsed?.orgSlug ?? ref.org;
    const team =
      parsed?.teamName ??
      (fallback?.orgSlug === org ? fallback?.teamName : undefined) ??
      state.orgs.find((entry) => entry.slug === org)?.teams[0]?.name;
    if (org && team) sources.set(`${org}:${team}`, { org, team });
  }
  const results = await Promise.allSettled(
    [...sources.values()].map(async ({ org, team }) => listPanes(org, team)),
  );
  return results.flatMap((result) =>
    result.status === "fulfilled"
      ? (result.value?.panes ?? []).filter(isInboxDashboard).map((pane) => pane.panelId)
      : [],
  );
}
