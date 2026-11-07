// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";
import { Inbox } from "lucide-react";
import { TooltipProvider } from "@/components/ui/tooltip";
import { SlotDashboardHost } from "@/features/shell/slots/slot-dashboard-host";
import { useNativeSlot } from "@/features/shell/slots/native-slot-context";
import { PanelMaximizeBoundary } from "@/features/shell/right-pane/panel-maximize-boundary";
import { registerPanel } from "@/features/shell/right-pane/panel-registry";
import { registerBuiltinPanels } from "@/features/shell/right-pane/builtin-panels";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { liveHub } from "@/features/dashboard/lib/live-hub";
import { dashboardIdentity } from "@/features/shell/slots/slot-fallback-strip";
import {
  defaultRightPaneLayout,
  revealPanel,
  SCRATCH_PANEL_ID,
} from "@/features/shell/right-pane/right-pane-layout";
import { listPanes, type PaneEntry } from "@/tauri/commands";
import { mockOrg } from "./fixtures";
import type { SlotId } from "@/features/shell/slots/slot-registry";
interface Mount {
  visible?: boolean;
  id: string;
  slot?: SlotId;
  surface?: string;
  width?: number;
  scope?: "private" | "team";
  conversation?: string;
}
function NativeSelection() {
  const slot = useNativeSlot();
  return (
    <>
      <output data-testid="native-selection" className="sr-only">
        {String(slot?.state.conversation ?? slot?.intent?.conversation ?? "")}
      </output>
    </>
  );
}
export function SlotHarness() {
  const [mount, setMount] = useState<Mount>();
  const [entry, setEntry] = useState<PaneEntry>();
  const [generation, setGeneration] = useState(0);
  useEffect(() => {
    registerBuiltinPanels();
    registerPanel({
      id: SCRATCH_PANEL_ID,
      title: "Notes",
      icon: Inbox,
      render: () => <NativeSelection />,
    });
    useOrgStore.setState({
      orgs: [mockOrg],
      activeOrgSlug: mockOrg.slug,
      activeTeamName: "default",
    });
    Object.assign(window, {
      __slotHarness: {
        show: (visible: boolean) =>
          setMount((current) => (current ? { ...current, visible } : current)),
        mount: async (next: Mount) => {
          const listing = await listPanes(mockOrg.slug, "default");
          const pane = listing.panes.find((p) => p.id === next.id);
          if (!pane) throw new Error("Fixture not found");
          registerPanel({
            id: pane.panelId,
            title: pane.title,
            icon: Inbox,
            render: () => null,
            maximizable: true,
          });
          if (next.conversation)
            await liveHub.writeUi(`ui:${next.slot ?? "right-pane"}`, "all", {
              conversation: next.conversation,
            });
          setEntry({
            ...pane,
            surface: next.surface ?? pane.surface,
            scope: next.scope ?? pane.scope,
          });
          setMount(next);
          setGeneration((n) => n + 1);
          useViewerStore.setState({
            rightPaneOpen: true,
            rightPaneLayout: revealPanel(defaultRightPaneLayout(), pane.panelId),
            maximizedPanel: {},
          });
        },
        maximize: () => {
          const panel = useViewerStore
            .getState()
            .rightPaneLayout.sections.find((s) => s.group.activePanelId.startsWith("pane:"))
            ?.group.activePanelId;
          if (panel) useViewerStore.getState().setMaximized(`${mockOrg.slug}:default`, panel);
        },
      },
    });
  }, []);
  return (
    <TooltipProvider>
      <main className="min-h-screen bg-background p-6 text-foreground">
        <span data-testid="slot-harness-ready" className="sr-only">
          Ready
        </span>
        {entry && mount && (
          <section
            style={{ width: mount.width ?? 420, height: 600 }}
            className="flex flex-col overflow-hidden rounded-md border bg-background"
            data-testid="slot-preview"
          >
            <div
              data-panel-header-row
              tabIndex={-1}
              className="flex h-8 shrink-0 items-center gap-2 border-b bg-card px-3 text-xs font-medium"
            >
              {entry.title}
              <button
                className="ml-auto"
                onClick={() =>
                  useViewerStore.getState().setMaximized(`${mockOrg.slug}:default`, entry.panelId)
                }
              >
                Maximize
              </button>
            </div>
            <PanelMaximizeBoundary
              panelId={entry.panelId}
              repoPath={`team:${mockOrg.slug}:default`}
              title={entry.title}
              icon={Inbox}
              scopeLabel={dashboardIdentity(entry.dashboardRef, entry).label}
              scopeIcon={dashboardIdentity(entry.dashboardRef, entry).icon}
            >
              <SlotDashboardHost
                key={generation}
                slotId={mount.slot ?? "right-pane"}
                entry={entry}
                contentRoot="/mock-ctx/default/content"
                visible={mount.visible ?? true}
                onReload={async () => {
                  const pane =
                    (await listPanes(mockOrg.slug, "default")).panes.find(
                      (p) => p.id === entry.id,
                    ) ?? null;
                  if (pane) setEntry({ ...pane, surface: mount.surface ?? pane.surface });
                  return pane;
                }}
              />
            </PanelMaximizeBoundary>
          </section>
        )}
      </main>
    </TooltipProvider>
  );
}
