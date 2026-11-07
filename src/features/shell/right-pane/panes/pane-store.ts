// SPDX-License-Identifier: MIT
/**
 * Custom right panes (`<space>/panes/<id>.html`): what the active team sees and
 * when each open pane last changed on disk, plus the badge each pane set.
 *
 * `listing` and `sourceRows` mirror the hub's current native view. `reloadKeys`
 * adapts source revisions for right-pane hosts. This state is runtime-only:
 * placements live in the right-pane layout, never here.
 */
import { create } from "zustand";

import type { DocBadge } from "@/features/dashboard/components/html-viewer";
import type { DashboardRef, PaneEntry, PaneListing, PaneSourceRow } from "@/tauri/commands";

import { pageRevision } from "@/features/dashboard/lib/page-revision";
import { createWarmCache } from "@/shared/lib/warm-scopes";

import { PANE_PANEL_ID_PREFIX } from "../right-pane-layout";

/** A parsed pane panel id. */
export interface PanePanelRef {
  scope: "team" | "private";
  /** The team key (`<org>:<team>`) of a team pane. */
  teamKey?: string;
  /** The org of a private pane (a private space is per org). */
  org?: string;
  id: string;
}

/** The panel id a pane registers as. Mirrors Rust `panes::pane_panel_id`. */
export function panePanelId(
  scope: "team" | "private",
  org: string,
  team: string | undefined,
  id: string,
): string {
  return scope === "team" && team
    ? `${PANE_PANEL_ID_PREFIX}${org}:${team}/${id}`
    : `${PANE_PANEL_ID_PREFIX}private:${org}/${id}`;
}

/** Parse a `pane:` panel id; `null` for anything else. */
export function parsePanePanelId(panelId: string): PanePanelRef | null {
  if (!panelId.startsWith(PANE_PANEL_ID_PREFIX)) return null;
  const rest = panelId.slice(PANE_PANEL_ID_PREFIX.length);
  const slash = rest.lastIndexOf("/");
  if (slash <= 0 || slash === rest.length - 1) return null;
  const layer = rest.slice(0, slash);
  const id = rest.slice(slash + 1);
  if (layer.startsWith("private:")) {
    const org = layer.slice("private:".length);
    return org ? { scope: "private", org, id } : null;
  }
  return { scope: "team", teamKey: layer, id };
}

/** Whether a pane belongs to the active org's private space or team chain. */
export function paneInView(panelId: string, org: string, chainTeamKeys: Set<string>): boolean {
  const ref = parsePanePanelId(panelId);
  if (!ref) return false;
  if (ref.scope === "private") return ref.org === org;
  return !!ref.teamKey && ref.teamKey.startsWith(`${org}:`) && chainTeamKeys.has(ref.teamKey);
}

/** Where a pane's file lives, for a placeholder that names it. */
export function panePathLabel(ref: PanePanelRef): string {
  if (ref.scope === "private") return `Private/panes/${ref.id}.html`;
  const team = ref.teamKey?.slice(ref.teamKey.indexOf(":") + 1) ?? "";
  return `${team}/panes/${ref.id}.html`;
}

export type PaneIntent = Record<string, unknown>;

/** What a team's last `list_panes` answered, with the source rows that came with it. */
interface TeamListing {
  listing: PaneListing;
  sourceRows: Record<string, PaneSourceRow>;
}

/** The last listing per team, kept while the team is at least suspended. A team shown again
 *  renders its panes from this at once and revalidates behind it; the store's `listing` below
 *  stays the one the hub's current view feeds. A rename drops the entry: its pane paths, panel ids
 *  and chain name the old team, so the renamed team lists afresh instead of adopting them. */
export const paneListingCache = createWarmCache<string, TeamListing>({
  kind: "pane-listing",
  scopeOf: (teamKey) => teamKey,
  tier: "suspended",
});

interface PaneStoreState {
  refresh: ((reference?: DashboardRef) => Promise<PaneListing | null>) | null;
  sourceRows: Record<string, PaneSourceRow>;
  pendingIntents: Record<string, PaneIntent>;
  setIntent: (panelId: string, intent: PaneIntent) => void;
  clearIntent: (panelId: string, intent: PaneIntent) => void;
  /** The active team's key the listing belongs to, or null when teamless. */
  teamKey: string | null;
  listing: PaneListing | null;
  /** Last scanned identity/surface for a placed pane whose file disappears. */
  knownEntries: Record<string, PaneEntry>;
  /** Why the last `list_panes` for `teamKey` failed, or null. A pane shows it in
   *  place rather than waiting on a listing that never comes. */
  listError: { teamKey: string; message: string } | null;
  reloadKeys: Record<string, number>;
  /** The badge each pane last set (`codesense.setBadge`), by panel id. */
  badges: Record<string, DocBadge>;
  setListing: (
    teamKey: string | null,
    listing: PaneListing | null,
    sourceRows?: Record<string, PaneSourceRow>,
  ) => void;
  setListError: (teamKey: string, message: string) => void;
  setBadge: (panelId: string, badge: DocBadge | null) => void;
}

export const usePaneStore = create<PaneStoreState>((set) => ({
  refresh: null,
  sourceRows: {},
  pendingIntents: {},
  setIntent: (panelId, intent) => {
    if (new TextEncoder().encode(JSON.stringify(intent)).length > 4096)
      throw new Error("Pane intent must be at most 4 KB");
    set((s) => ({ pendingIntents: { ...s.pendingIntents, [panelId]: intent } }));
  },
  clearIntent: (panelId, intent) =>
    set((s) => {
      if (s.pendingIntents[panelId] !== intent) return s;
      const pendingIntents = { ...s.pendingIntents };
      delete pendingIntents[panelId];
      return { pendingIntents };
    }),
  teamKey: null,
  listing: null,
  knownEntries: {},
  listError: null,
  reloadKeys: {},
  badges: {},
  setListing: (teamKey, listing, sourceRows) => {
    if (teamKey && listing)
      paneListingCache.set(teamKey, {
        listing,
        sourceRows: sourceRows ?? paneListingCache.get(teamKey)?.sourceRows ?? {},
      });
    set((s) => {
      const rows = sourceRows ?? s.sourceRows;
      const reloadKeys = sourceRows
        ? {
            ...s.reloadKeys,
            ...Object.fromEntries(
              Object.values(sourceRows).flatMap((row) =>
                row.entry ? [[row.entry.panelId, pageRevision(row)]] : [],
              ),
            ),
          }
        : s.reloadKeys;
      return {
        teamKey,
        listing,
        sourceRows: rows,
        reloadKeys,
        listError: null,
        knownEntries: listing
          ? {
              ...s.knownEntries,
              ...Object.fromEntries(listing.panes.map((entry) => [entry.panelId, entry])),
            }
          : {},
      };
    });
  },
  setListError: (teamKey, message) => set({ listError: { teamKey, message } }),
  setBadge: (panelId, badge) =>
    set((s) => {
      const current = s.badges[panelId];
      if (!badge) {
        if (!current) return s;
        const badges = { ...s.badges };
        delete badges[panelId];
        return { badges };
      }
      if (current && current.count === badge.count && current.severity === badge.severity) return s;
      return { badges: { ...s.badges, [panelId]: badge } };
    }),
}));
