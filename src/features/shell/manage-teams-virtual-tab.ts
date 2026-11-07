// SPDX-License-Identifier: MIT
import { createElement } from "react";

import { openWorkspaces } from "@/features/org/lib/open-workspaces";
import { openManagementTab } from "@/shared/lib/open-management-tab";
import { registerVirtualTab } from "@/shared/lib/virtual-viewer-registry";
import { activeTeams } from "@/shared/lib/user-teams";
import { useOrgStore } from "@/shared/stores/org-store";
import { useUiStore } from "@/shared/stores/ui-store";

import { ManageTeamsTab } from "./components/manage-teams-tab";

/**
 * SW-UX (app-flow redesign) — team management opens as a virtual viewer tab
 * (`virtual://manage-teams`), never a dialog. It folds switcher-visibility + create +
 * archive into one home reached from the team switcher's "Browse all teams…" /
 * "Create team".
 *
 * `openManageTeams("create")` opens the tab and focuses the new-team field; the
 * plain form just opens/focuses the tab.
 */
export const MANAGE_TEAMS_VIRTUAL_PATH = "virtual://manage-teams";

let registered = false;

/** Register the team-management surface under its virtual path. Idempotent. */
export function registerManageTeamsTab(): void {
  if (registered) return;
  registered = true;
  // The tab mount drives focus from the store-backed `manageTeamsIntent`, so it
  // takes no props — wrap it to satisfy the registry's component contract.
  registerVirtualTab(MANAGE_TEAMS_VIRTUAL_PATH, "Teams", () => createElement(ManageTeamsTab));
}

/**
 * Open (or focus) the team-management tab. `intent: "create"` focuses the
 * new-team field.
 *
 * "Create a team" must WORK when there is no team yet — the surface that creates
 * the FIRST team can't itself be team-gated, or onboarding self-locks — so with no
 * team this opens in the no-team tab host (SW-FU-B7).
 */
export function openManageTeams(intent: "list" | "create" = "list"): void {
  registerManageTeamsTab();
  useUiStore.getState().setManageTeamsIntent(intent);
  useUiStore.getState().closeSettings();
  openManagementTab(MANAGE_TEAMS_VIRTUAL_PATH);
}

/**
 * SW-FU-B7: a TEAM-scoped surface (source repos, the sample run, Git Sync) was
 * requested with no team active. Land on the tab that gets the user a team instead
 * of doing nothing: Workspaces when no workspace is open, else Teams (create when
 * the workspace has no active teams, browse when it has some but none is open).
 */
export function openTeamRequired(): void {
  const { orgs, activeOrgSlug } = useOrgStore.getState();
  const org = activeOrgSlug === null ? null : orgs.find((o) => o.slug === activeOrgSlug);
  if (!org) {
    openWorkspaces("home");
    return;
  }
  openManageTeams(activeTeams(org).length === 0 ? "create" : "list");
}
