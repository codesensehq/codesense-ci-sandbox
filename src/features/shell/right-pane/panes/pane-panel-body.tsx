// SPDX-License-Identifier: MIT
import { useRetiringFrames } from "@/features/dashboard/components/use-retiring-frames";
import { useDashboardProfileStore } from "@/features/dashboard/lib/dashboard-profile";
import { inboxOfListing } from "../../slots/reveal-slot";
import { SlotDashboardHost } from "../../slots/slot-dashboard-host";
import { dashboardIdentity } from "../../slots/slot-fallback-strip";
/** Render discovered panes through the shared slot host. The panel registry
 * owns placement and chrome; the host owns liveness and native fallback. */
import { useCallback, useLayoutEffect, useMemo, useRef } from "react";
import { dashboardRefKey } from "@/features/dashboard/lib/dashboard-ref";

import { type DocBadge } from "@/features/dashboard/components/html-viewer";

import { useTerminalSplitInset } from "@/features/inbox/hooks/use-terminal-split-inset";

import { buildTeamKey, parseTeamKey } from "@/shared/lib/team-key";
import { OwningScopeProvider, useOwnsScope, useOwningScope } from "@/shared/lib/warm-scopes";
import { useOrgStore } from "@/shared/stores/org-store";

import { usePaneOnScreen } from "./pane-visibility";

import { INBOX_PANEL_ID } from "../right-pane-layout";
import { PanelMaximizeBoundary } from "../panel-maximize-boundary";
import { useOptionalRightPaneHost } from "../right-pane-host-context";
import { PaneBadge, paneIcon } from "./pane-chrome";
import {
  paneInView,
  paneListingCache,
  panePathLabel,
  parsePanePanelId,
  usePaneStore,
} from "./pane-store";

export function PanePanelBody({ panelId }: { panelId: string }) {
  const inbox = panelId === INBOX_PANEL_ID;
  useDashboardProfileStore((s) => (inbox ? s.profile : null));
  const activeOrg = useOrgStore((s) => s.activeOrgSlug);
  const activeTeam = useOrgStore((s) => s.activeTeamName);
  // The page belongs to the team whose right pane renders it, not to the team on screen: a warm
  // team's hidden pane keeps its own page while another team is active.
  const owningScope = useOwningScope();
  const ownerActive = useOwnsScope();
  const activeTeamKey =
    owningScope ?? (activeOrg && activeTeam ? buildTeamKey(activeOrg, activeTeam) : null);
  const owner = activeTeamKey ? parseTeamKey(activeTeamKey) : null;
  // The team's listing: from the cache, so a team shown again renders in its first frame, else the
  // one the hub's current view holds for it.
  const cachedListing = paneListingCache.useValue(activeTeamKey ?? "");
  const storeListing = usePaneStore((s) =>
    s.teamKey !== null && s.teamKey === activeTeamKey ? s.listing : null,
  );
  const teamListing = cachedListing ?? (storeListing ? { listing: storeListing } : undefined);
  const listError = usePaneStore((s) =>
    s.listError?.teamKey === activeTeamKey ? s.listError.message : null,
  );
  // A cached listing counts once the registration has caught up to it (the store names the team) or
  // it lists this panel: until then the panel may be the previous team's, which it does not list.
  const registered = usePaneStore((s) => s.teamKey !== null && s.teamKey === activeTeamKey);
  const listed = !!teamListing?.listing.panes.some((p) => p.panelId === panelId);
  // A panel the shown team's listing neither lists nor places is the previous team's, still
  // registered until its page flushes: it shows the loading state rather than turning "missing".
  const outgoing =
    registered &&
    !inbox &&
    !listed &&
    !!teamListing &&
    !!owner?.teamName &&
    !paneInView(
      panelId,
      owner.orgSlug,
      new Set(
        [owner.teamName, ...teamListing.listing.chain].map((t) => buildTeamKey(owner.orgSlug, t)),
      ),
    );
  const current =
    !outgoing &&
    (listError !== null || (teamListing !== undefined && (registered || inbox || listed)));
  // The Inbox is the owning team's: its own listing decides, not the team on screen.
  const entry = inbox
    ? owner && teamListing
      ? inboxOfListing(owner.orgSlug, teamListing.listing.panes)
      : undefined
    : teamListing?.listing.panes.find((p) => p.panelId === panelId);
  const known = usePaneStore((s) => (inbox ? undefined : s.knownEntries[panelId]));
  const contentRoot = teamListing?.listing.contentRoot ?? "";
  const reloadKey = usePaneStore(
    (s) =>
      (s.reloadKeys[panelId] ?? 0) +
      (entry && entry.panelId !== panelId ? (s.reloadKeys[entry.panelId] ?? 0) : 0),
  );
  // An intent is for the page on screen; a hidden team's copy of the panel leaves it be.
  const intent = usePaneStore((s) => (ownerActive ? s.pendingIntents[panelId] : undefined));
  const onIntentDelivered = useCallback(() => {
    if (intent) usePaneStore.getState().clearIntent(panelId, intent);
  }, [panelId, intent]);
  const ownerOrg = owner?.orgSlug;
  const onReload = useCallback(async () => {
    const state = usePaneStore.getState();
    const ownInbox = () => {
      const panes = (
        paneListingCache.get(activeTeamKey ?? "")?.listing ??
        (state.teamKey === activeTeamKey ? state.listing : null)
      )?.panes;
      return ownerOrg && panes ? inboxOfListing(ownerOrg, panes) : undefined;
    };
    const ref = parsePanePanelId(panelId);
    const reference =
      (inbox ? ownInbox()?.dashboardRef : state.knownEntries[panelId]?.dashboardRef) ??
      (ref
        ? {
            scope: ref.scope,
            org: ref.org ?? useOrgStore.getState().activeOrgSlug!,
            team: ref.teamKey?.slice(ref.teamKey.indexOf(":") + 1),
            id: ref.id,
          }
        : undefined);
    const listing = await state.refresh?.(reference);
    return inbox
      ? (ownInbox() ?? null)
      : (listing?.panes.find((p) => p.panelId === panelId) ?? null);
  }, [panelId, inbox, activeTeamKey, ownerOrg]);
  const host = useOptionalRightPaneHost();
  const bottomInset = useTerminalSplitInset();
  const onScreen = usePaneOnScreen(panelId);
  const missing = useMemo(() => {
    const ref = parsePanePanelId(panelId);
    return {
      dashboardRef: undefined,
      id: inbox ? "inbox" : (ref?.id ?? panelId),
      title: inbox ? "Inbox" : (ref?.id ?? panelId),
      scope: ref?.scope ?? ("team" as const),
      team: ref?.teamKey?.slice(ref.teamKey.indexOf(":") + 1),
      path: ref ? panePathLabel(ref) : panelId,
      panesDir: "",
      panelId,
    };
  }, [panelId, inbox]);
  const dashboard = entry ?? known ?? missing;
  const badgeKey = (teamKey: string | null, pane: typeof dashboard) =>
    `${teamKey}:${pane.dashboardRef ? dashboardRefKey(pane.dashboardRef) : pane.path}`;
  const badgeId = badgeKey(activeTeamKey, dashboard);
  const badgeOwner = useRef<string | null>(null);
  // The badge is the one shown on the panel's tab: only the team on screen writes it.
  useLayoutEffect(() => {
    if (!ownerActive) return;
    badgeOwner.current = badgeId;
    usePaneStore.getState().setBadge(panelId, null);
    return () => {
      badgeOwner.current = null;
      usePaneStore.getState().setBadge(panelId, null);
    };
  }, [badgeId, panelId, reloadKey, ownerActive]);
  const candidates = useMemo(
    () =>
      current ? [{ dashboard, contentRoot, teamKey: activeTeamKey ?? "", listError, entry }] : [],
    [current, listError, dashboard, contentRoot, activeTeamKey, entry],
  );
  const retained = useRetiringFrames(
    candidates,
    (value) => `${value.teamKey}:${value.dashboard.path}`,
    (f, value) => f.panelId === panelId && f.teamKey === value.teamKey,
  );
  // The page on screen: this team's own, once its listing is current.
  const shown = (value: (typeof retained)[number]) =>
    current && value.dashboard === dashboard && value.teamKey === activeTeamKey;
  if (!retained.length)
    return (
      <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
        Loading…
      </div>
    );
  return (
    <PanelMaximizeBoundary
      panelId={panelId}
      repoPath={host?.repoPath ?? ""}
      title={dashboard.title}
      icon={paneIcon(entry?.icon)}
      badge={<PaneBadge panelId={panelId} />}
      scope="app"
      scopeLabel={dashboardIdentity(dashboard.dashboardRef, dashboard).label}
      scopeIcon={dashboardIdentity(dashboard.dashboardRef, dashboard).icon}
      bottomInset={bottomInset}
    >
      <div className="flex h-full min-h-0 w-full flex-col" data-testid="pane-body">
        {retained.map((value) => (
          <div
            key={`${value.teamKey}:${value.dashboard.path}`}
            className="flex h-full min-h-0 flex-1 flex-col"
            style={shown(value) ? undefined : { display: "none" }}
            inert={!shown(value)}
          >
            {/* A retiring page keeps its own team as owner while another is shown. */}
            <OwningScopeProvider scope={value.teamKey}>
              <SlotDashboardHost
                key={panelId}
                slotId="right-pane"
                panelId={panelId}
                refreshKey={reloadKey}
                entry={value.dashboard}
                contentRoot={value.contentRoot}
                teamKey={value.teamKey}
                visible={onScreen && ownerActive && shown(value)}
                intent={intent}
                onIntentDelivered={onIntentDelivered}
                onBadge={(badge: DocBadge | null) => {
                  if (badgeOwner.current === badgeKey(value.teamKey, value.dashboard))
                    usePaneStore.getState().setBadge(panelId, badge);
                }}
                onReload={onReload}
                onNavigate={host?.onFileSelect}
                unavailable={
                  value.listError && value.dashboard.dashboardRef?.scope !== "built-in"
                    ? { kind: "connection", technical: `pane list: ${value.listError}` }
                    : !value.entry
                      ? { kind: "missing", technical: `not found: ${missing.path}` }
                      : undefined
                }
              />
            </OwningScopeProvider>
          </div>
        ))}
      </div>
    </PanelMaximizeBoundary>
  );
}
