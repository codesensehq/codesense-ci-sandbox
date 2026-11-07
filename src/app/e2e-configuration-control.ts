// SPDX-License-Identifier: MIT
import { openSettingsTab } from "@/features/settings/lib/open-settings-tab";
import type { SectionKey } from "@/features/settings/lib/types";
import { usePrefsStore } from "@/features/dashboard/lib/prefs-client";
import { useDashboardProfileStore } from "@/features/dashboard/lib/dashboard-profile";
import { getEffectiveShortcut } from "@/shared/stores/shortcuts-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useConfigStore } from "@/shared/stores/config-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { getActiveTeamKey } from "@/shared/lib/team-key";
import { resolveTeamContentContext } from "@/shared/lib/team-content-context";

/** WebDriver-only observations of the real consumers; no snapshot or store injection. */
export function registerConfigurationE2eControl() {
  window.__CODESENSE_CONFIGURATION_E2E__ = {
    openSettings: openSettingsTab,
    snapshot: () => {
      const prefs = usePrefsStore.getState();
      const viewer = useViewerStore.getState();
      const org = useOrgStore.getState();
      const teamKey = getActiveTeamKey();
      const context = resolveTeamContentContext(teamKey, org.orgs, org.discoveryComplete);
      return {
        principal: prefs.principal,
        ready: prefs.globalReady && prefs.ownedReady && viewer.stateLoaded,
        org: org.activeOrgSlug,
        teamKey,
        teamContentPath: context.kind === "ready" ? context.source.teamContentPath : null,
        editorLineWrap: viewer.settings.editorLineWrap,
        chord: getEffectiveShortcut("settings.open"),
        policy: prefs.owned.dashboardPolicy,
        fileNav: useDashboardProfileStore.getState().profile?.slots["file-nav"],
        mainDoc: useConfigStore.getState().teamMainDocPreferences[teamKey]?.mainDoc,
        timeOrigin: performance.timeOrigin,
      };
    },
  };
}

declare global {
  interface Window {
    __CODESENSE_CONFIGURATION_E2E__?: {
      openSettings: (section: SectionKey) => void;
      snapshot: () => {
        principal: string | null;
        ready: boolean;
        org: string | null;
        teamKey: string;
        teamContentPath: string | null;
        editorLineWrap: string;
        chord: string;
        policy: unknown;
        fileNav: unknown;
        mainDoc: string | null | undefined;
        timeOrigin: number;
      };
    };
  }
}
