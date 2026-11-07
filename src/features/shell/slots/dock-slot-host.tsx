// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useState, useSyncExternalStore } from "react";
import { OwningScopeProvider } from "@/shared/lib/warm-scopes";
import { useDocumentVisible } from "@/features/inbox/hooks/use-document-visible";
import { requestFrameFocus, type LiveFrame } from "@/features/dashboard/lib/frame-lifecycle";
import { useRetiringFrames } from "@/features/dashboard/components/use-retiring-frames";
import { AgentsList } from "@/features/viewer/components/agents-list";
import {
  PaneCollapseDivider,
  type PaneSize,
} from "@/features/viewer/components/pane-collapse-divider";
import {
  agentsListId,
  hideAgentsDock,
  requestAgentsFocus,
  takeAgentsFocus,
} from "@/features/viewer/lib/agents-list-focus";
import type { DragResizeOptions } from "@/features/viewer/views/use-drag-resize";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { dashboardRefKey } from "@/features/dashboard/lib/dashboard-ref";
import { buildTeamKey } from "@/shared/lib/team-key";
import { warmScopes } from "@/shared/lib/warm-scopes";
import { listBuiltinPanes, type PaneEntry, type PaneListing } from "@/tauri/commands";
import { usePaneStore } from "../right-pane/panes/pane-store";
import { singleSlotChoice, useSingleSlotDashboard } from "./single-slot-dashboard";
import { SlotDashboardHost } from "./slot-dashboard-host";
import { DOCK_FRAME_PANEL_ID, SLOT_REGISTRY } from "./slot-registry";

/** The dock's width when none is saved, and the range a drag keeps it in. */
const DOCK_DEFAULT_WIDTH_PX = 268;
const DOCK_MIN_WIDTH_PX = 236;
const DOCK_MAX_WIDTH_PX = 400;
/** The border's click/drag zone: narrower than the shared default, because
 *  agent rows end in hover actions just left of it and terminal lines start
 *  just right of it — so a stray click there doesn't hide the list. */
const EDGE_HIT_PADDING = { start: 5, end: 2 };
/** Folded, the grip is the terminal row's first child at the window's left edge
 *  (overflow hidden): it sits flush there, its pill fully drawn 4px in, with a
 *  13px grab area reaching over the terminal's first columns. */
const FOLDED_HIT_PADDING = { start: 4, end: 8 };
/** A folded dock has nothing to drag; its border only reopens it. */
const NO_RESIZE: Omit<DragResizeOptions, "onTap"> = {
  getContainerSize: () => 0,
  getStartRatio: () => 0,
  getPos: () => 0,
  computeRatio: ({ startRatio }) => startRatio,
  onVisualUpdate: () => {},
  onCommit: () => {},
};
const DOCK_READY_TIMEOUT_MS = 3_000;

function clampWidth(px: number): number {
  if (!Number.isFinite(px)) return DOCK_DEFAULT_WIDTH_PX;
  return Math.min(DOCK_MAX_WIDTH_PX, Math.max(DOCK_MIN_WIDTH_PX, Math.round(px)));
}

export const dockChoice = () => singleSlotChoice("dock");

/**
 * The dock beside the CLI (`SLOT_REGISTRY.dock`): one region, mounted once per window. It owns
 * the chrome (width, fold and the resize border) and picks the body: the org's chosen dock
 * dashboard through the shared slot host, or the built-in Agents list.
 */
export function DockSlotHost({ teamKey, visible }: { teamKey: string; visible: boolean }) {
  const collapsed = useViewerStore((s) => s.workspacePanelCollapsed);
  const setCollapsed = useViewerStore((s) => s.setWorkspacePanelCollapsed);
  const savedWidth = useViewerStore((s) => s.workspacePanelWidth ?? undefined);
  const setSavedWidth = useViewerStore((s) => s.setWorkspacePanelWidth);
  const width = clampWidth(savedWidth ?? DOCK_DEFAULT_WIDTH_PX);
  const [dragWidth, setDragWidth] = useState<number | null>(null);
  const shownWidth = dragWidth ?? width;
  const narrow = shownWidth < SLOT_REGISTRY.dock.narrowBelowPx;
  const open = visible && !collapsed;
  const documentVisible = useDocumentVisible();
  const dashboard = useSingleSlotDashboard("dock", open && documentVisible);
  const activeOrg = useOrgStore((s) => s.activeOrgSlug);
  const activeTeam = useOrgStore((s) => s.activeTeamName);
  const activeTeamKey = activeOrg && activeTeam ? buildTeamKey(activeOrg, activeTeam) : null;
  const listing = usePaneStore((s) =>
    activeTeamKey && s.teamKey === activeTeamKey ? s.listing : null,
  );
  const listedBuiltinAgentList = listing?.panes.find(
    (entry) => entry.id === "agent-list" && entry.dashboardRef?.scope === "built-in",
  );
  const [fallbackListing, setFallbackListing] = useState<{
    teamKey: string;
    listing: PaneListing;
  } | null>(null);
  useEffect(() => {
    if (listedBuiltinAgentList || !activeOrg || !activeTeam || !activeTeamKey) return;
    if (fallbackListing?.teamKey === activeTeamKey) return;
    let current = true;
    void listBuiltinPanes(activeOrg, activeTeam)
      .then((next) => {
        if (current) setFallbackListing({ teamKey: activeTeamKey, listing: next });
      })
      .catch((error) => console.warn("Built-in agent list unavailable", error));
    return () => {
      current = false;
    };
  }, [listedBuiltinAgentList, activeOrg, activeTeam, activeTeamKey, fallbackListing?.teamKey]);
  const fallbackData = fallbackListing?.teamKey === activeTeamKey ? fallbackListing.listing : null;
  const builtinAgentList =
    listedBuiltinAgentList ??
    fallbackData?.panes.find(
      (entry) => entry.id === "agent-list" && entry.dashboardRef?.scope === "built-in",
    );
  const resolvedDashboard = dashboard?.entry ? dashboard : null;
  const [intent, setIntent] = useState<Record<string, unknown>>();
  useEffect(() => {
    if (!open || !dashboard) return;
    return useViewerStore.subscribe((state, previous) => {
      const reveal = state.agentsWorkspaceReveal;
      if (!reveal || reveal === previous.agentsWorkspaceReveal) return;
      setIntent({ ...reveal, kind: "reveal-workspace" });
      state.clearAgentsWorkspaceReveal();
    });
  }, [open, dashboard]);
  const panelId = resolvedDashboard?.entry.panelId;
  // One dock page per warm org, kept mounted and hidden while another org is on screen, so a
  // cross-org switch (and the switch back) shows each org's chosen dock without a rebuild. A page
  // another org shares by identity stays one host, following the active team.
  useSyncExternalStore(warmScopes.subscribe, warmScopes.getVersion);
  type Host = NonNullable<typeof resolvedDashboard>;
  const [orgDocks] = useState(() => new Map<string, Host>());
  if (activeOrg) {
    // A page whose source is momentarily unresolved (a listing re-read) stays; a changed or
    // removed choice drops it.
    const choice = dockChoice();
    if (resolvedDashboard) orgDocks.set(activeOrg, resolvedDashboard);
    else if (!choice || orgDocks.get(activeOrg)?.key !== dashboardRefKey(choice))
      orgDocks.delete(activeOrg);
  }
  for (const org of [...orgDocks.keys()])
    if (org !== activeOrg && warmScopes.orgTier(org) === "cold") orgDocks.delete(org);
  // Each page once, by identity; the active org's entry wins a page two orgs share.
  const dockOwners = new Map<string, { org: string; host: Host }>();
  for (const [org, host] of orgDocks)
    if (org === activeOrg || !dockOwners.has(host.key)) dockOwners.set(host.key, { org, host });
  // While the active org's source lookup catches up, its cached page is still the one shown.
  const shownDock = resolvedDashboard ?? (activeOrg ? orgDocks.get(activeOrg) : undefined);
  const activeHosts = [...dockOwners.values()].map((owner) => owner.host);
  // An outgoing dock page (choice changed, Built-in, a cold org) stays mounted, hidden and
  // inert, until its flush is acknowledged or times out, as right-pane panes do.
  const hosts = useRetiringFrames(activeHosts, dockHostKey, ownsDockFrame);
  const fallbackWanted = !!shownDock && shownDock.entry.dashboardRef?.scope !== "built-in";
  const lookedUpFallback =
    fallbackWanted && builtinAgentList
      ? {
          entry: builtinAgentList,
          contentRoot: listing?.contentRoot ?? fallbackData?.contentRoot ?? "",
          readyWithinMs: DOCK_READY_TIMEOUT_MS,
        }
      : undefined;
  // A shown fallback outlives its lookup, per org: a switch un-keys the team's listing until the
  // next one lands, and the mounted list rebinds like the primary instead of being rebuilt.
  const [heldFallbacks] = useState(() => new Map<string, NonNullable<typeof lookedUpFallback>>());
  if (activeOrg) {
    if (lookedUpFallback) heldFallbacks.set(activeOrg, lookedUpFallback);
    else if (!fallbackWanted) heldFallbacks.delete(activeOrg);
  }
  for (const org of [...heldFallbacks.keys()]) if (!orgDocks.has(org)) heldFallbacks.delete(org);
  const fallbackConfig = lookedUpFallback ?? (activeOrg ? heldFallbacks.get(activeOrg) : undefined);

  // Unfolded from the tray: focus moves into the dashboard's first control. The built-in list
  // takes the same request itself.
  useEffect(() => {
    if (open && panelId && takeAgentsFocus(teamKey, "list")) requestFrameFocus(DOCK_FRAME_PANEL_ID);
  }, [open, panelId, teamKey]);

  const renderNative = useCallback(
    () => <AgentsList teamKey={teamKey} visible={visible} narrow={narrow} />,
    [teamKey, visible, narrow],
  );

  // With the CLI column closed there is nothing here at all; a dashboard's frame stays
  // mounted (hidden) so a team switch or a fold never reloads it.
  if (!visible && !hosts.length) return null;

  const resize: Omit<DragResizeOptions, "onTap"> = {
    getContainerSize: () => window.innerWidth,
    getStartRatio: () => width,
    getPos: (e) => e.clientX,
    // Delta-based, like the nav: the pane edge stays under the grab point.
    computeRatio: ({ startRatio, startPos, currentPos }) =>
      clampWidth(startRatio + currentPos - startPos),
    onVisualUpdate: setDragWidth,
    onCommit: (w) => setSavedWidth(w),
    onCleanup: () => setDragWidth(null),
  };
  const size: PaneSize = {
    now: shownWidth,
    min: DOCK_MIN_WIDTH_PX,
    max: DOCK_MAX_WIDTH_PX,
    step: 16,
    largeStep: 32,
    format: (w) => `${w} pixels`,
    onChange: (w) => setSavedWidth(clampWidth(w)),
  };

  return (
    <OwningScopeProvider scope={teamKey}>
      <div
        data-surface="sidebar"
        role="region"
        aria-label="Agents"
        id={agentsListId(teamKey)}
        data-testid="agents-dock"
        data-narrow={narrow || undefined}
        data-resizing={dragWidth !== null || undefined}
        tabIndex={-1}
        className="relative flex min-h-0 shrink-0 flex-col bg-[var(--cs-sidebar-bg,var(--background))] outline-none [&[data-resizing]_iframe]:pointer-events-none"
        // Folding hides the body but never moves or unmounts it (moving an iframe reloads it).
        style={{ width: shownWidth, display: open ? undefined : "none" }}
      >
        {!shownDock && renderNative()}
        {hosts.map((host) => {
          const owner = dockOwners.get(host.key);
          const pinnedTeam = !!owner && owner.org !== activeOrg;
          const outgoing = host.key !== shownDock?.key;
          const hostFallback = !owner
            ? undefined
            : owner.org === activeOrg
              ? host.key === shownDock?.key
                ? fallbackConfig
                : undefined
              : heldFallbacks.get(owner.org);
          return (
            <div
              key={host.key}
              data-dock-current={outgoing ? undefined : "true"}
              className={
                outgoing
                  ? "pointer-events-none invisible absolute inset-0"
                  : "flex min-h-0 flex-1 flex-col"
              }
              aria-hidden={outgoing || undefined}
              inert={outgoing || undefined}
            >
              <SlotDashboardHost
                slotId="dock"
                keepTeam={pinnedTeam}
                intent={outgoing ? undefined : intent}
                entry={host.entry}
                contentRoot={host.contentRoot}
                visible={open && documentVisible && !outgoing}
                // A failed page falls back to the built-in HTML list when its pane is listed; with
                // no such pane to show, the native list stands in, so the dock is never blank.
                renderNative={
                  host.entry.dashboardRef?.scope === "built-in" || !hostFallback
                    ? renderNative
                    : undefined
                }
                stripClassName="max-h-[60%] shrink-0 overflow-y-auto"
                headerless
                unavailable={
                  host.missing
                    ? { kind: "missing", technical: `not found: ${host.entry.path}` }
                    : undefined
                }
                refreshKey={host.refreshKey}
                onReload={host.reload}
                fallback={hostFallback}
              />
            </div>
          );
        })}
      </div>
      {visible &&
        (collapsed ? (
          // Folded, the body is hidden but its border stays, like every other pane's:
          // its grip (or the tray's Agents control) brings it back.
          <PaneCollapseDivider
            edge="left"
            collapsed
            onToggle={() => {
              requestAgentsFocus(teamKey, "list");
              setCollapsed(false);
            }}
            paneLabel="agents"
            resize={NO_RESIZE}
            size={{ ...size, now: width, onChange: () => {} }}
            hitPadding={FOLDED_HIT_PADDING}
            flushStartWhenCollapsed
            borderColor="var(--cs-sidebar-border, var(--border))"
            data-testid="agents-width-divider"
          />
        ) : (
          // The dock's right edge is the same border control as every other pane:
          // drag (or arrow keys) to resize, a click hides the dock.
          <PaneCollapseDivider
            edge="left"
            collapsed={false}
            onToggle={() => hideAgentsDock(teamKey)}
            paneLabel="agents"
            resize={resize}
            size={size}
            hitPadding={EDGE_HIT_PADDING}
            borderColor="var(--cs-sidebar-border, var(--border))"
            data-testid="agents-width-divider"
          />
        ))}
    </OwningScopeProvider>
  );
}

const dockHostKey = (host: { key: string }) => host.key;
const ownsDockFrame = (frame: LiveFrame, host: { entry: PaneEntry }) =>
  frame.slotId === "dock" && frame.documentPath === host.entry.path;
