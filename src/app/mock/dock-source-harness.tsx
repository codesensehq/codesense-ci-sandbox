// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";
import { TooltipProvider } from "@/components/ui/tooltip";
import { DockSlotHost } from "@/features/shell/slots/dock-slot-host";
import { PanePanelsMount } from "@/features/shell/right-pane/panes/use-register-pane-panels";
import { useDashboardCommandsSync } from "@/features/dashboard/lib/dashboard-commands";
import { useDashboardProfileStore } from "@/features/dashboard/lib/dashboard-profile";
import { useOrgStore } from "@/shared/stores/org-store";
import { usePaneStore } from "@/features/shell/right-pane/panes/pane-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { writeDashboardProfile } from "@/tauri/commands";
import { mockOrg } from "./fixtures";

function Commands() {
  useDashboardCommandsSync();
  return null;
}

export function DockSourceHarness() {
  const [ready, setReady] = useState(false);
  const team = useOrgStore((s) => s.activeTeamName);
  const sourceTeam = usePaneStore((s) => s.teamKey);
  useEffect(() => {
    const profile = {
      id: "default",
      name: "Default",
      posture: "",
      slots: {
        dock: { scope: "team" as const, org: mockOrg.slug, team: "a", id: "people" },
      },
      cliLayout: "default",
      chrome: {},
    };
    useOrgStore.setState({ orgs: [mockOrg], activeOrgSlug: mockOrg.slug, activeTeamName: "a" });
    useViewerStore.setState({ workspacePanelCollapsed: false });
    void writeDashboardProfile(mockOrg.slug, profile).then(() => {
      useDashboardProfileStore.setState({ org: mockOrg.slug, profile });
      setReady(true);
    });
  }, []);
  return (
    <TooltipProvider>
      <main style={{ display: "flex", height: 600 }}>
        {ready && (
          <>
            <output data-testid="source-team">{sourceTeam}</output>
            <PanePanelsMount />
            <Commands />
            <DockSlotHost teamKey={`${mockOrg.slug}:${team}`} visible />
            <button
              onClick={() => useOrgStore.setState({ activeTeamName: team === "a" ? "b" : "a" })}
            >
              Switch team
            </button>
            <button
              onClick={() =>
                useViewerStore.setState((s) => ({
                  workspacePanelCollapsed: !s.workspacePanelCollapsed,
                }))
              }
            >
              Fold dock
            </button>
          </>
        )}
      </main>
    </TooltipProvider>
  );
}
