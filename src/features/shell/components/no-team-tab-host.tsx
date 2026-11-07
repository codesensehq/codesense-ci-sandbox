// SPDX-License-Identifier: MIT
import { dashboardPublishingFallback } from "@/features/dashboard-publishing/dashboards-virtual-tab";
import { useEffect, type ReactElement, type ReactNode } from "react";
import { X } from "lucide-react";

import { resolveVirtualTab, resolveVirtualTabTitle } from "@/shared/lib/virtual-viewer-registry";
import { useUiStore } from "@/shared/stores/ui-store";

/**
 * SW-FU-B7 — the minimal tab host for the no-team state (no org, or an org with no
 * active team: first run, a failed join, a workspace whose teams were all removed).
 *
 * A team's management tabs (Workspaces, Teams, Settings, Connections, …) open in
 * that team's content area. With no team there is no content area, so the openers
 * route here instead (`openManagementTab`) and the SAME registered tab surfaces
 * render in a plain tab strip — the same tab UI as everywhere else, never a dialog
 * and never nothing (CONSISTENCY-SPEC Rule 1).
 *
 * `children` is the no-team empty state; it shows when no tab is active.
 */
export function NoTeamTabHost({ children }: { children: ReactNode }) {
  const tabs = useUiStore((s) => s.noTeamTabs);
  const activePath = useUiStore((s) => s.noTeamActiveTab);
  const setActive = useUiStore((s) => s.setNoTeamActiveTab);
  const closeTab = useUiStore((s) => s.closeNoTeamTab);

  // The host unmounts once a team is active; its tabs belong to the no-team state
  // only, so they must not reappear the next time there is no team.
  useEffect(() => () => useUiStore.setState({ noTeamTabs: [], noTeamActiveTab: null }), []);

  if (tabs.length === 0) return <>{children}</>;

  const activeSurface = activePath ? renderTab(activePath) : null;

  return (
    <div data-testid="no-team-tab-host" className="flex h-full min-h-0 flex-col bg-background">
      <div
        role="tablist"
        aria-label="Open tabs"
        className="flex h-9 shrink-0 items-end gap-0.5 overflow-x-auto border-b border-border px-2"
      >
        {tabs.map((path) => {
          const isActive = path === activePath;
          const title = resolveVirtualTabTitle(path) ?? path;
          return (
            <div
              key={path}
              onPointerDown={(e) => {
                if (e.button === 1) {
                  e.preventDefault();
                  closeTab(path);
                }
              }}
              className={`group flex h-8 min-w-0 max-w-[14rem] items-center rounded-t-md border-x border-t text-xs ${
                isActive
                  ? "-mb-px border-border bg-background font-medium text-foreground"
                  : "border-transparent text-muted-foreground hover:text-foreground"
              }`}
            >
              <button
                type="button"
                role="tab"
                aria-selected={isActive}
                data-testid={`no-team-tab-${path}`}
                onClick={() => setActive(path)}
                className="min-w-0 truncate py-1 pl-3 pr-1"
              >
                {title}
              </button>
              <button
                type="button"
                aria-label={`Close ${title}`}
                onClick={() => closeTab(path)}
                className="mr-1.5 rounded p-0.5 opacity-60 hover:bg-accent hover:opacity-100 group-hover:opacity-100"
              >
                <X size={12} aria-hidden="true" />
              </button>
            </div>
          );
        })}
      </div>
      <div role="tabpanel" className="min-h-0 flex-1 overflow-hidden">
        {activeSurface ?? children}
      </div>
    </div>
  );
}

/** Render the registered surface for a virtual tab path (the same registry the team
 *  content area's `renderViewer` resolves through), or `null` if none is registered. */
function renderTab(path: string): ReactElement | null {
  const Tab = resolveVirtualTab(path) ?? dashboardPublishingFallback(path);
  return Tab ? <Tab key={path} filePath={path} teamContentPath="" /> : null;
}
