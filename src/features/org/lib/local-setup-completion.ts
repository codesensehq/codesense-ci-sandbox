// SPDX-License-Identifier: MIT
import { switchContextToOrg } from "@/features/viewer/lib/context-switch";
import { create } from "zustand";
import { getCurrentUser } from "@/shared/lib/get-current-user";
import { useLocalProfileStore } from "@/shared/stores/local-profile-store";
import { LOCAL_GUEST_USER_ID, useFirstRunStore } from "@/shared/stores/first-run-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { openManageTeams } from "@/features/shell/manage-teams-virtual-tab";
import type { ConnectOrgResult } from "@/tauri/commands";

/** Setup mutation lifetime outlives a discovery-driven component unmount. */
export const useLocalSetupActivity = create<{ busy: boolean }>(() => ({ busy: false }));
export function beginLocalSetup(): boolean {
  if (useLocalSetupActivity.getState().busy) return false;
  useLocalSetupActivity.setState({ busy: true });
  return true;
}
export function endLocalSetup(): void {
  useLocalSetupActivity.setState({ busy: false });
}

/** Command completion owns selection/continuation. Discovery events only refresh. */
export function completeLocalSetup(
  principal: string,
  result: ConnectOrgResult,
  created: boolean,
): boolean {
  if (getCurrentUser()?.principal !== principal || useLocalProfileStore.getState().busy)
    return false;
  const org = result.loaded_org.org;
  if (!org)
    throw new Error(
      "The context repo could not be loaded. Repair its registry files and use Connect a context repo… to retry.",
    );
  if (principal === String(LOCAL_GUEST_USER_ID)) useFirstRunStore.getState().adoptBackendGuest();
  const store = useOrgStore.getState();
  store.setOrgs([...store.orgs.filter((o) => o.slug !== org.slug), { ...org, is_attached: true }]);
  switchContextToOrg(org.slug, "nav");
  // A new empty context repo has no team yet; one started from the example does.
  if (created && org.teams.length === 0) openManageTeams("create");
  return true;
}
