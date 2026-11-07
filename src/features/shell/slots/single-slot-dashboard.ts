// SPDX-License-Identifier: MIT
import { useCallback, useMemo, useRef } from "react";
import { dashboardRefKey } from "@/features/dashboard/lib/dashboard-ref";
import { useDashboardProfileStore } from "@/features/dashboard/lib/dashboard-profile";
import { pageRevision } from "@/features/dashboard/lib/page-revision";
import { useSourceRevision } from "@/features/dashboard/lib/pane-sources";
import { useOrgStore } from "@/shared/stores/org-store";
import { usePaneStore } from "../right-pane/panes/pane-store";
import { buildTeamKey } from "@/shared/lib/team-key";
import type { DashboardRef, PaneEntry } from "@/tauri/commands";
import { refPanelId, selectedRefs } from "./reveal-slot";
import type { SlotId } from "./slot-registry";

/** The active org profile's slot choice, or null for the built-in list. */
export function singleSlotChoice(slot: SlotId): DashboardRef | null {
  const org = useOrgStore.getState().activeOrgSlug;
  return (org && selectedRefs(slot, org)[0]) || null;
}

/** A placeholder entry for a chosen dashboard that is not there, so the strip can say so. */
function missingEntry(ref: DashboardRef): PaneEntry {
  return {
    id: ref.id,
    title: ref.id,
    scope: ref.scope === "private" ? "private" : "team",
    team: ref.team,
    dashboardRef: ref,
    path: `${ref.scope === "private" ? "private" : (ref.team ?? "")}/panes/${ref.id}.html`,
    panesDir: "",
    panelId: refPanelId(ref),
  };
}

/** Source identity stays fixed while a singleton slot rebinds its active-team grant. */
export function useSingleSlotDashboard(slot: SlotId, visible: boolean) {
  useDashboardProfileStore((s) => s.profile);
  useDashboardProfileStore((s) => s.org);
  const activeOrg = useOrgStore((s) => s.activeOrgSlug);
  const activeTeam = useOrgStore((s) => s.activeTeamName);
  const activeTeamKey = activeOrg && activeTeam ? buildTeamKey(activeOrg, activeTeam) : null;
  const readerContentRoot = usePaneStore((s) =>
    activeTeamKey && s.teamKey === activeTeamKey ? s.listing?.contentRoot : undefined,
  );
  const ref = activeOrg ? singleSlotChoice(slot) : null;
  const refKey = ref ? dashboardRefKey(ref) : null;
  const source = useSourceRevision(refKey, visible);
  const held = useRef<{ key: string; entry: PaneEntry; documentRevision: number } | null>(null);
  if (held.current?.key !== refKey) held.current = null;
  // A source withheld while hidden is not a missing one: only a visible read builds a placeholder.
  if (ref && refKey && (source || (visible && readerContentRoot && !held.current)))
    held.current = {
      key: refKey,
      entry: source?.entry ?? held.current?.entry ?? missingEntry(ref),
      documentRevision: source ? pageRevision(source) : 0,
    };
  const entry = held.current?.entry;
  // The active reader's root; the host keeps a shown attempt's own root until its next attempt.
  const contentRoot = readerContentRoot ?? source?.contentRoot ?? "";
  const reload = useCallback(async () => {
    const current = singleSlotChoice(slot);
    if (!current) return null;
    await usePaneStore.getState().refresh?.(current);
    return usePaneStore.getState().sourceRows[dashboardRefKey(current)]?.entry ?? null;
  }, [slot]);
  return useMemo(
    () =>
      held.current && entry && refKey && (source || readerContentRoot)
        ? {
            key: refKey,
            entry,
            contentRoot,
            missing: !source?.entry,
            refreshKey: source ? pageRevision(source) : held.current.documentRevision,
            reload,
          }
        : null,
    [source, entry, refKey, contentRoot, readerContentRoot, reload],
  );
}
