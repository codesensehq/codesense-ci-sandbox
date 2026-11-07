// SPDX-License-Identifier: MIT
import { create } from "zustand";
import { getCurrentUser } from "@/shared/lib/get-current-user";
import {
  writeDashboardProfile,
  type DashboardProfile,
  type DashboardRef,
  type PaneEntry,
} from "@/tauri/commands";
import { serializeAppStateWrite } from "@/shared/lib/app-state-write";
import { dashboardRefKey } from "./dashboard-ref";
import { servesSurface } from "@/features/shell/slots/reveal-slot";
import { usePaneStore } from "@/features/shell/right-pane/panes/pane-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { warmScopes } from "@/shared/lib/warm-scopes";
import { buildTeamKey } from "@/shared/lib/team-key";
import type { SlotId } from "@/features/shell/slots/slot-registry";

/** The HTML agent list the app ships with, as a dock choice. */
export function builtinAgentListRef(org: string): DashboardRef {
  return { scope: "built-in", org, id: "agent-list" };
}

export function isInboxDashboard(pane: PaneEntry): boolean {
  return pane.id === "inbox" || pane.dashboardRef?.installedFrom?.id === "inbox";
}

export const useDashboardProfileStore = create<{
  /** The org whose profile `profile` is (the one the configuration connection follows). */
  org: string | null;
  profile: DashboardProfile | null;
  /** The last profile of every other warm org, so a switch back resolves the dock in its first
   *  frame instead of falling to the built-in list until the profile topic answers. */
  warm: Record<string, DashboardProfile>;
}>(() => ({ org: null, profile: null, warm: {} }));

/** An org's profile, now: the connected org's, else its warm one, else none. */
export function profileOf(org: string | null | undefined): DashboardProfile | null {
  const state = useDashboardProfileStore.getState();
  if (!org) return null;
  return (state.org === org ? state.profile : state.warm[org]) ?? null;
}

/** The connection follows `org`: its profile shows at once if warm, and the profile of the org it
 *  leaves is kept while that org is warm. Cold orgs drop out, so the cache stays bounded. */
export function followOrgProfile(org: string | null): void {
  useDashboardProfileStore.setState((state) => {
    if (state.org === org) return state;
    const warm = { ...state.warm };
    if (state.org && state.profile) warm[state.org] = state.profile;
    for (const key of Object.keys(warm))
      if (key === org || warmScopes.orgTier(key) === "cold") delete warm[key];
    return { org, profile: (org && state.warm[org]) || null, warm };
  });
}

/** Forget every cached profile (a different principal, or signed out). */
export function resetProfiles(): void {
  useDashboardProfileStore.setState({ org: null, profile: null, warm: {} });
}

/** Persist only the edited fields. The local draft is replaced by the hub's committed view. */
async function updateProfile(
  org: string,
  patch: Partial<DashboardProfile>,
  canWrite: () => boolean = () => true,
): Promise<void> {
  const principal = getCurrentUser()?.principal;
  const activeOrg = useOrgStore.getState().activeOrgSlug;
  if ((activeOrg !== null && activeOrg !== org) || !canWrite()) return;
  return serializeAppStateWrite(async () => {
    if (getCurrentUser()?.principal !== principal) return;
    const previous = profileOf(org);
    const base = previous ?? {
      id: "default",
      name: "Default",
      posture: "",
      slots: {},
      cliLayout: "default",
      chrome: {},
    };
    const draft: DashboardProfile = {
      ...base,
      ...patch,
      slots: { ...base.slots, ...patch.slots },
      rightPaneSurfaces: { ...base.rightPaneSurfaces, ...patch.rightPaneSurfaces },
    };
    const showingOwner = useOrgStore.getState().activeOrgSlug === org;
    if (showingOwner) {
      followOrgProfile(org);
      useDashboardProfileStore.setState({ profile: draft });
    }
    try {
      await writeDashboardProfile(org, patch, principal);
    } catch (error) {
      if (showingOwner && useDashboardProfileStore.getState().profile === draft)
        useDashboardProfileStore.setState({ profile: previous });
      throw error;
    }
    if (getCurrentUser()?.principal !== principal || useOrgStore.getState().activeOrgSlug !== org)
      return;
    // Installation discovery remains owned by paneSources; it cannot overwrite the profile view.
    let refreshTimeout: ReturnType<typeof setTimeout> | undefined;
    await Promise.race([
      usePaneStore.getState().refresh?.(),
      new Promise<void>((resolve) => {
        refreshTimeout = setTimeout(resolve, 3000);
      }),
    ])
      .catch(console.error)
      .finally(() => clearTimeout(refreshTimeout));
  });
}

/** Chooses the dashboard a one-body slot shows, or its built-in. */
export function setSlotDashboard(
  org: string,
  slot: SlotId,
  ref: DashboardRef | "native",
): Promise<void> {
  return updateProfile(org, { slots: { [slot]: ref } });
}

export function setSurfaceDashboard(
  org: string,
  surface: string,
  ref: DashboardRef,
  enabled: boolean,
): Promise<void> {
  const canWrite = () => {
    const context = useOrgStore.getState();
    const panes = usePaneStore.getState();
    // Validate the selected ref against its owning team before enqueueing the write.
    return (
      context.activeOrgSlug === org &&
      !!context.activeTeamName &&
      panes.teamKey === buildTeamKey(org, context.activeTeamName) &&
      !!panes.listing?.panes.some(
        (pane) =>
          servesSurface(pane, surface) &&
          pane.dashboardRef &&
          dashboardRefKey(pane.dashboardRef) === dashboardRefKey(ref),
      )
    );
  };
  return updateProfile(
    org,
    { rightPaneSurfaces: { [surface]: enabled ? ref : "native" } },
    canWrite,
  );
}
