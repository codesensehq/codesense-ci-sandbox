// SPDX-License-Identifier: MIT
import { openWorkspaces } from "@/features/org/lib/open-workspaces";
import { useOrgStore } from "@/shared/stores/org-store";
import { useUiStore } from "@/shared/stores/ui-store";

/**
 * WebDriver-only control door for org / team / workspace switching (SW-OC-B11).
 *
 * The identity menu (SW-OC-28) opens on a plain WebDriver click, so journeys can
 * drive it through the real pointer path. This door stays for deterministic setup:
 * it drives the SAME store-controlled open flags (`orgSwitcherOpen` /
 * `teamSwitcherOpen`, which open the menu at its Context repo / Team section) and
 * the selection actions the real UI uses, so the harness can switch or create a
 * 2nd org/team without depending on pointer timing.
 *
 * Behind App.tsx's `MODE === "webdriver"` guard (the same block that arms the
 * other `__CODESENSE_*_E2E__` hooks), so it is tree-shaken from — and proven
 * absent (`scripts/verify-webdriver-absence.ts`) in — the shipped prod build.
 * Never present in a normal (non-e2e) build.
 */
export interface OrgTeamE2eControl {
  /** Open the org switcher popover via its store-controlled flag. */
  openOrgSwitcher: () => void;
  /** Open the team switcher popover via its store-controlled flag. */
  openTeamSwitcher: () => void;
  /** Open the Workspaces tab on the create surface (the "create workspace" leg). */
  createWorkspace: () => void;
  /** Switch the active org through the real store action (no popover click). */
  setActiveOrg: (slug: string | null) => void;
  /** Switch the active team through the real store action (no popover click). */
  setActiveTeam: (name: string | null) => void;
  /** Create + switch into a team through the real store action (2nd-team leg). */
  createTeam: (name: string) => Promise<void>;
  /** Read-only oracle: the current { activeOrgSlug, activeTeamName } selection. */
  activeSelection: () => { activeOrgSlug: string | null; activeTeamName: string | null };
}

declare global {
  interface Window {
    __CODESENSE_ORG_E2E__?: OrgTeamE2eControl;
  }
}

export function registerOrgTeamE2eControl(): void {
  window.__CODESENSE_ORG_E2E__ = {
    openOrgSwitcher: () => useUiStore.getState().setOrgSwitcherOpen(true),
    openTeamSwitcher: () => useUiStore.getState().setTeamSwitcherOpen(true),
    createWorkspace: () => openWorkspaces("create"),
    setActiveOrg: (slug) => useOrgStore.getState().setActiveOrg(slug),
    setActiveTeam: (name) => useOrgStore.getState().setActiveTeam(name),
    createTeam: (name) => useOrgStore.getState().createTeam(name),
    activeSelection: () => {
      const { activeOrgSlug, activeTeamName } = useOrgStore.getState();
      return { activeOrgSlug, activeTeamName };
    },
  };
}
