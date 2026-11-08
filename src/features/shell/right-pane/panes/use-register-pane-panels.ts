// SPDX-License-Identifier: MIT
import { liveHub, type HubSnapshot } from "@/features/dashboard/lib/live-hub";
import { applyPaneSources } from "@/features/dashboard/lib/pane-sources";
import { liveHubUnsubscribe, type DashboardRef, type PaneSourceRow } from "@/tauri/commands";
import { retireAfterFlush } from "@/features/dashboard/lib/frame-lifecycle";
/**
 * Register the active team's custom right panes (`<space>/panes/<id>.html`) as
 * right-pane panels, keep them live, and honour an agent's `pane_open`.
 *
 * - Every pane `list_panes` returns registers as `pane:<org>:<team>/<id>` (or
 *   `pane:private:<org>/<id>`), so it is offered in View → Panels and the command
 *   palette like any panel.
 * - A `pane:` placement in the layout whose file the listing no longer has (and
 *   whose layer is in the active team's view) still registers, rendering the
 *   "moved or deleted" placeholder. Placements are never evicted from here.
 * - The hub's pane source view updates affected entries and source revisions.
 * - `pane-open` refreshes its target through that owner, then places and reveals it.
 */
import {
  declaresSlot,
  selectedRefs,
  resolveSlot,
  revealSlot,
  servesSurface,
} from "../../slots/reveal-slot";
import { nativeSurface, SLOT_REGISTRY } from "../../slots/slot-registry";
import { listen } from "@tauri-apps/api/event";
import type { LucideIcon } from "lucide-react";
import { createElement, useEffect, useRef, useState } from "react";
import { toast } from "sonner";

import { logger } from "@/shared/lib/logger";
import { warmScopes } from "@/shared/lib/warm-scopes";
import { buildTeamKey } from "@/shared/lib/team-key";
import { createListenerGroup } from "@/shared/lib/tauri-listeners";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { listPanes, type PaneEntry, type PaneListing } from "@/tauri/commands";
import {
  isInboxDashboard,
  useDashboardProfileStore,
} from "@/features/dashboard/lib/dashboard-profile";
import { PANE_OPEN_EVENT, type PaneOpenPayload } from "@/tauri/events";

import {
  getPanel,
  getScopedPanel,
  listScopedPanels,
  registerPanel,
  unregisterPanel,
  usePanelRegistry,
  type PanelDefinition,
} from "../panel-registry";
import {
  isDurablePaneId,
  replacePanel,
  migrateInboxPlacements,
  INBOX_PANEL_ID,
  type RightPaneLayout,
} from "../right-pane-layout";
import { PaneBadge, paneIcon } from "./pane-chrome";
import { PanePanelBody } from "./pane-panel-body";
import { paneInView, paneListingCache, parsePanePanelId, usePaneStore } from "./pane-store";

/** A pane's panel title: its `<title>`, with the layer it comes from unless it
 *  is the active team's own, so two panes of one id in two layers read apart. */
export function panePanelTitle(entry: PaneEntry, activeTeam: string): string {
  // A built-in lists in the private layer but is the app's own, not the user's private pane.
  if (entry.dashboardRef?.scope === "built-in") return entry.title;
  if (entry.scope === "private") return `${entry.title} · Private`;
  return entry.team && entry.team !== activeTeam ? `${entry.title} · ${entry.team}` : entry.title;
}

function paneDefinition(panelId: string, title: string, icon: LucideIcon): PanelDefinition {
  return {
    id: panelId,
    title,
    icon,
    render: () => createElement(PanePanelBody, { panelId }),
    badge: () => createElement(PaneBadge, { panelId }),
    maximizable: true,
  };
}

/** The `pane:` ids placed in `layout` (none before a layout is hydrated). */
function placedPaneIds(layout: RightPaneLayout | undefined): string[] {
  return (layout?.sections ?? []).flatMap((s) => s.group.panelIds).filter(isDurablePaneId);
}

/** Right-pane surfaces Settings chooses between a native panel and a pane serving it. */
const SURFACES = Object.keys(SLOT_REGISTRY["right-pane"].nativeDefault);

/** The Inbox and surface panes take the place of their native panel instead of being offered
 *  beside it: Settings and the surface's own entry point choose which one shows. */
const chosenBySurface = (p: PaneEntry) =>
  isInboxDashboard(p) || SURFACES.some((surface) => servesSurface(p, surface));

/** The panel definitions for `listing` plus placeholders for in-view placements
 *  whose file is gone. */
export function paneDefinitions(
  org: string,
  team: string,
  listing: PaneListing,
  layout: RightPaneLayout,
): PanelDefinition[] {
  const defs = listing.panes
    .filter((p) => declaresSlot(p, "right-pane"))
    .map((p) => ({
      ...paneDefinition(
        p.panelId,
        isInboxDashboard(p) ? "Inbox" : panePanelTitle(p, team),
        paneIcon(p.icon),
      ),
      ...(chosenBySurface(p) ? { offerable: false } : {}),
    }));
  const listed = new Set(defs.map((d) => d.id));
  const chainKeys = new Set([team, ...listing.chain].map((t) => buildTeamKey(org, t)));
  for (const panelId of placedPaneIds(layout)) {
    if (listed.has(panelId) || !paneInView(panelId, org, chainKeys)) continue;
    const ref = parsePanePanelId(panelId);
    const known = usePaneStore.getState().knownEntries[panelId];
    if (known && !declaresSlot(known, "right-pane")) continue;
    defs.push({
      ...paneDefinition(
        panelId,
        known && isInboxDashboard(known)
          ? "Inbox"
          : known
            ? panePanelTitle(known, team)
            : `${ref?.id ?? panelId} (missing)`,
        paneIcon(known?.icon),
      ),
      ...(known && chosenBySurface(known) ? { offerable: false } : {}),
    });
  }
  return defs;
}

/** A team's panel whose pane left its listing: unregistered once its page has flushed. */
function retireScopedPanel(scope: string, def: PanelDefinition): void {
  void retireAfterFlush(
    (f) => f.panelId === def.id && f.teamKey === scope,
    () => {
      if (getScopedPanel(scope, def.id) === def) unregisterPanel(def.id, scope);
    },
  );
}

export function useRegisterPanePanels(): void {
  const org = useOrgStore((s) => s.activeOrgSlug);
  const team = useOrgStore((s) => s.activeTeamName);
  // The scopes this hook registered panels for: a team's panels stay registered while it is warm,
  // so a refresh replaces in place and drops only what left, and an unmount drops them all.
  const [registeredScopes] = useState(() => new Set<string>());
  const lastOrg = useRef<string | null>(null);
  const sourceKey = useRef<string | null>(null);
  const sourceMounted = useRef(false);
  const sourceOwner = useRef(crypto.randomUUID());
  const sourceSequence = useRef(0);
  const sourceReads = useRef(new Set<Promise<PaneListing>>());
  const sourceKeys = useRef(new Set<string>());

  useEffect(() => {
    if (!org || !team) {
      usePanelRegistry.getState().setActiveScope(null);
      usePaneStore.getState().setListing(null, null);
      return;
    }
    const teamKey = buildTeamKey(org, team);
    usePanelRegistry.getState().setActiveScope(teamKey);
    registeredScopes.add(teamKey);
    let active = true;

    let lastListing: PaneListing | null = null;
    // Whether a live listing has applied: the first one reconciles placements, even after the
    // cached listing this team came back with.
    let live = false;
    const apply = (
      listing: PaneListing,
      sync = true,
      sourceRows?: Record<string, PaneSourceRow>,
      registerOnly = false,
    ) => {
      if (!active) return;
      lastListing = listing;
      usePaneStore.getState().setListing(teamKey, listing, sourceRows);
      const viewer = useViewerStore.getState();
      let layout = viewer.rightPaneLayout;
      // A listing cached from another org registers its panels but leaves the layout alone: the
      // dashboard profile that picks the selected copies is still the previous org's until it
      // lands, and the live listing reconciles the placements.
      if (!registerOnly) {
        const inboxIds = listing.panes.filter(isInboxDashboard).map((pane) => pane.panelId);
        layout = migrateInboxPlacements(layout, inboxIds);
        const maximizedInbox = viewer.maximizedPanel[teamKey];
        if (maximizedInbox && inboxIds.includes(maximizedInbox))
          viewer.setMaximized(teamKey, INBOX_PANEL_ID);
      }
      const chain = new Set([team, ...listing.chain].map((t) => buildTeamKey(org, t)));
      const entries = [
        ...listing.panes,
        ...Object.values(usePaneStore.getState().knownEntries).filter((p) =>
          paneInView(p.panelId, org, chain),
        ),
      ];
      for (const surface of registerOnly ? [] : SURFACES) {
        const nativeId = nativeSurface("right-pane", surface)!.panelId!;
        const resolved = resolveSlot("right-pane", { surface });
        const native = getPanel(nativeId);
        if (native && native.offerable !== !resolved.dashboard)
          registerPanel({ ...native, offerable: !resolved.dashboard });
        const ids = new Set([
          nativeId,
          ...entries.filter((p) => servesSurface(p, surface)).map((p) => p.panelId),
        ]);
        const target = resolved.panelId ?? nativeId;
        // Architecture: late discovery of the selected copy also reconciles a placed native body.
        const lateNative =
          !!resolved.dashboard &&
          layout.sections.some((section) => section.group.panelIds.includes(nativeId));
        if (sync || lateNative) {
          for (const id of ids) layout = replacePanel(layout, id, target);
          const maximized = viewer.maximizedPanel[teamKey];
          if (maximized && ids.has(maximized)) viewer.setMaximized(teamKey, target);
        }
      }
      // Architecture: the regular layout setter flushes outgoing pages before swapping bodies.
      if (layout !== viewer.rightPaneLayout) viewer.setRightPaneLayout(layout);
      const next = paneDefinitions(org, team, listing, layout);
      const nextIds = new Set(next.map((d) => d.id));
      for (const def of next) {
        const current = getScopedPanel(teamKey, def.id);
        // Same id, title and icon: keep the registered def so the body never remounts.
        if (
          current &&
          current.title === def.title &&
          current.icon === def.icon &&
          current.offerable === def.offerable
        ) {
          continue;
        }
        registerPanel(def, teamKey);
      }
      for (const def of listScopedPanels(teamKey)) {
        if (!nextIds.has(def.id)) retireScopedPanel(teamKey, def);
      }
      usePanelRegistry.getState().settleScope(teamKey);
    };

    const applySnapshot = (snapshot: HubSnapshot) => {
      const { listing, sourceRows } = applyPaneSources(
        snapshot,
        usePaneStore.getState().sourceRows,
      );
      apply(listing, !live, sourceRows);
      live = true;
      return listing;
    };

    // Overlapping refreshes may answer out of order: a listing is applied only
    // when no newer request's listing has been applied already.
    let issued = 0;
    let applied = 0;
    let releaseSource: (() => void) | undefined;
    const choices = () => [
      ...selectedRefs("right-pane", org),
      ...selectedRefs("dock", org),
      ...selectedRefs("file-nav", org),
    ];
    const refresh = (refreshRef?: DashboardRef): Promise<PaneListing | null> => {
      const seq = ++issued;
      const sourceSeq = ++sourceSequence.current;
      const read = listPanes(org, team, {
        sourceView: sourceOwner.current,
        sourceSeq,
        selectedRefs: choices(),
        refreshRef,
      });
      sourceReads.current.add(read);
      return read
        .then(async (listing) => {
          if (listing.sources) sourceKeys.current.add(listing.sources.viewKey);
          if (!active || seq <= applied || sourceSeq !== sourceSequence.current) return listing;
          applied = seq;
          if (listing.sources) {
            const snapshot = await liveHub.resync("paneSources", listing.sources.viewKey);
            if (!active || sourceSeq !== sourceSequence.current) return listing;
            const previousKey = sourceKey.current;
            sourceKey.current = snapshot.viewKey;
            if (previousKey && previousKey !== snapshot.viewKey) {
              void liveHubUnsubscribe(previousKey);
              sourceKeys.current.delete(previousKey);
            }
            // One watch per team: a re-acquire replaces (disposes) the old one, so repeated
            // switches and refreshes never stack subscriptions.
            releaseSource?.();
            const stop = liveHub.watch("paneSources", snapshot.viewKey, (value) => {
              if (active && sourceKey.current === snapshot.viewKey) applySnapshot(value);
            });
            releaseSource = warmScopes.hold(teamKey, "pane-sources", "watch", stop);
            return applySnapshot(snapshot);
          } else apply(listing);

          return listing;
        })
        .catch((e: unknown) => {
          const message = e instanceof Error ? e.message : String(e);
          logger.warn("panes", `list_panes failed: ${message}`);
          if (active && seq > applied) {
            applied = seq;
            usePaneStore.getState().setListError(teamKey, message);
          }
          return null;
        })
        .finally(() => sourceReads.current.delete(read));
    };
    usePaneStore.setState({ refresh });
    // A team shown again renders from its last listing at once; the read below revalidates. The
    // cache is keyed by org and team, so a return from another org registers the same way. The
    // dock and profile follow the org with a hold of their own, so only the registration is cached.
    const cached = paneListingCache.get(teamKey);
    if (cached) apply(cached.listing, true, cached.sourceRows, lastOrg.current !== org);
    lastOrg.current = org;
    void refresh();

    const openRequests = new Map<string, number>();
    const group = createListenerGroup();
    group.track(
      listen<PaneOpenPayload>(PANE_OPEN_EVENT, (event) => {
        const { panelId, intent } = event.payload;
        const seq = (openRequests.get(panelId) ?? 0) + 1;
        openRequests.set(panelId, seq);
        const ref = parsePanePanelId(panelId);
        const reference: DashboardRef | undefined = ref
          ? {
              scope: ref.scope,
              org,
              team: ref.teamKey?.slice(ref.teamKey.indexOf(":") + 1),
              id: ref.id,
            }
          : undefined;
        void refresh(reference).then((listing) => {
          if (!active || openRequests.get(panelId) !== seq) return;
          // Only a pane this team shows is placed: a placement nothing registers
          // would be invisible and could not be removed from the Panels menu.
          if (!listing?.panes.some((p) => p.panelId === panelId) || !getPanel(panelId)) {
            toast.info("That pane isn't shown in this team; switch to its team to open it.", {
              id: `pane-open:${panelId}`,
            });
            return;
          }
          const entry = listing.panes.find((pane) => pane.panelId === panelId)!;
          revealSlot("right-pane", {
            ...(isInboxDashboard(entry) ? { surface: "inbox" } : { dashboard: panelId }),
            intent,
          });
        });
      }),
    );

    // A placement made or restored after the listing landed (a layout hydrated
    // late, a placed pane whose file is gone) still gets its panel.
    const placedKey = (layout: RightPaneLayout | undefined) =>
      placedPaneIds(layout).sort().join("\n");
    let placed = placedKey(useViewerStore.getState().rightPaneLayout);
    let selected = JSON.stringify(choices());
    const unsubscribeProfile = useDashboardProfileStore.subscribe(() => {
      const next = JSON.stringify(choices());
      if (lastListing) apply(lastListing);
      if (selected !== next) {
        selected = next;
        void refresh();
      }
    });
    const unsubscribeLayout = useViewerStore.subscribe((state) => {
      const next = placedKey(state.rightPaneLayout);
      const needsSwap = SURFACES.some(
        (surface) =>
          !!resolveSlot("right-pane", { surface }).dashboard &&
          state.rightPaneLayout.sections.some((s) =>
            s.group.panelIds.includes(nativeSurface("right-pane", surface)!.panelId!),
          ),
      );
      if (next === placed && !needsSwap) return;
      placed = next;
      if (lastListing) apply(lastListing, needsSwap);
    });

    return () => {
      active = false;
      if (usePaneStore.getState().refresh === refresh) usePaneStore.setState({ refresh: null });
      releaseSource?.();
      unsubscribeLayout();
      unsubscribeProfile();
      group.dispose();
    };
  }, [org, team]);

  // Unmount only: a torn-down host leaves no pane panels behind.
  useEffect(() => {
    sourceMounted.current = true;
    return () => {
      sourceMounted.current = false;
      for (const scope of registeredScopes)
        for (const def of listScopedPanels(scope)) retireScopedPanel(scope, def);
      registeredScopes.clear();
      void Promise.allSettled([...sourceReads.current]).then(() => {
        if (sourceMounted.current) return;
        for (const key of sourceKeys.current) void liveHubUnsubscribe(key);
        sourceKeys.current.clear();
      });
      usePaneStore.setState({ sourceRows: {} });
      usePaneStore.getState().setListing(null, null);
    };
  }, []);
}

/** The one owner of pane registration, mounted once at the app root: a team
 *  view is kept mounted through a switch, so a per-view hook would race the
 *  incoming team's and clear its listing on unmount. */
export function PanePanelsMount(): null {
  useRegisterPanePanels();
  return null;
}
