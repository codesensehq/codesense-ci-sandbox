// SPDX-License-Identifier: MIT
import { Button } from "@/components/ui/button";
import { openManagementTab } from "@/shared/lib/open-management-tab";
import { registerVirtualTab, type VirtualTabComponent } from "@/shared/lib/virtual-viewer-registry";
import { getActiveTeamKey, teamKeyHasTeam } from "@/shared/lib/team-key";
import { useUiStore } from "@/shared/stores/ui-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { dashboardPublishingEnabled } from "./capability";
import { DashboardsTab } from "./dashboards-tab";

export const DASHBOARDS_VIRTUAL_PATH = "virtual://dashboards";
export function registerDashboardsTab(): void {
  if (dashboardPublishingEnabled())
    registerVirtualTab(DASHBOARDS_VIRTUAL_PATH, "Dashboards", DashboardsTab);
}
export function openDashboards(): void {
  if (!dashboardPublishingEnabled()) return;
  registerDashboardsTab();
  useUiStore.getState().closeSettings();
  openManagementTab(DASHBOARDS_VIRTUAL_PATH);
}
function Unavailable() {
  return (
    <div className="space-y-4 p-6 text-sm">
      <p>Dashboard publishing isn't part of this build.</p>
      <Button
        variant="outline"
        size="sm"
        onClick={() => {
          const team = getActiveTeamKey();
          if (teamKeyHasTeam(team))
            useViewerStore.getState().closeTab(team, "", DASHBOARDS_VIRTUAL_PATH);
          else useUiStore.getState().closeNoTeamTab(DASHBOARDS_VIRTUAL_PATH);
        }}
      >
        Close tab
      </Button>
    </div>
  );
}
export function dashboardPublishingFallback(path: string): VirtualTabComponent | null {
  return path === DASHBOARDS_VIRTUAL_PATH && !dashboardPublishingEnabled() ? Unavailable : null;
}
