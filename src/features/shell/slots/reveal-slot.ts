// SPDX-License-Identifier: MIT
import { requestFrameFocus } from "@/features/dashboard/lib/frame-lifecycle";
import { dashboardRefKey } from "@/features/dashboard/lib/dashboard-ref";
import { DOCK_DEFAULT_HTML_AGENT_LIST } from "@/features/dashboard/lib/dock-default";
import {
  builtinAgentListRef,
  isInboxDashboard,
  profileOf,
  useDashboardProfileStore,
} from "@/features/dashboard/lib/dashboard-profile";
import { buildTeamKey } from "@/shared/lib/team-key";
import { revealCollapsedTerminal } from "@/features/viewer/lib/focus-cascade";
import {
  focusAgentsList,
  requestAgentsFocus,
  takeAgentsFocus,
} from "@/features/viewer/lib/agents-list-focus";
import { isTerminalColumnShown } from "@/features/viewer/lib/terminal-column";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { DashboardRef, DashboardSlotChoice, PaneEntry } from "@/tauri/commands";
import { INBOX_PANEL_ID } from "../right-pane/right-pane-layout";
import { paneOnScreen, usePaneOnScreen } from "../right-pane/panes/pane-visibility";
import {
  paneInView,
  panePanelId,
  usePaneStore,
  type PaneIntent,
} from "../right-pane/panes/pane-store";
import {
  canonicalSurface,
  DOCK_FRAME_PANEL_ID,
  FILE_NAV_FRAME_PANEL_ID,
  nativeSurface,
  type SlotId,
  type SlotPresentation,
} from "./slot-registry";

export interface RevealSlotOptions {
  focus?: boolean;
  dashboard?: DashboardRef | string;
  surface?: string;
  intent?: PaneIntent;
  presentation?: SlotPresentation;
}
const warnedMissingChoices = new Set<string>();
function choiceRefs(choice: DashboardSlotChoice | undefined): DashboardRef[] {
  return Array.isArray(choice) ? choice : choice && choice !== "native" ? [choice] : [];
}
function matchesRef(selected: DashboardRef, target: DashboardRef, includeSource = false): boolean {
  return (
    dashboardRefKey(selected) === dashboardRefKey(target) ||
    !!(
      includeSource &&
      selected.installedFrom &&
      dashboardRefKey(selected.installedFrom) === dashboardRefKey(target)
    )
  );
}
export function selectedRefs(slot: SlotId, org: string): DashboardRef[] {
  const profile = profileOf(org);
  const choice = profile?.slots[slot];
  if (slot === "dock" && choice === undefined && DOCK_DEFAULT_HTML_AGENT_LIST)
    return [builtinAgentListRef(org)];
  const refs = choiceRefs(choice);
  return slot === "right-pane"
    ? [
        ...refs,
        ...Object.values(profile?.rightPaneSurfaces ?? {}).filter(
          (r): r is DashboardRef => r !== "native",
        ),
      ]
    : refs;
}
export function dashboardSelected(
  ref: DashboardRef,
  includeInstalledSource = false,
  slot: SlotId = "right-pane",
): boolean {
  const profile = profileOf(ref.org);
  const choice = profile?.slots[slot];
  if (slot === "right-pane") {
    const panes = slotPanes() ?? [];
    const pane = panes.find(
      (p) => p.dashboardRef && matchesRef(p.dashboardRef, ref, includeInstalledSource),
    );
    if (!pane) return false;
    const surface = canonicalSurface(
      pane.surface ?? pane.declaration?.surface ?? pane.dashboardRef?.installedFrom?.id ?? pane.id,
    )!;
    if (surface === "inbox") {
      const inbox = currentInbox();
      return inbox?.dashboardRef
        ? matchesRef(inbox.dashboardRef, ref, includeInstalledSource)
        : selectedRefs(slot, ref.org).some((r) => matchesRef(r, ref, includeInstalledSource));
    }
    const own = profile?.rightPaneSurfaces?.[surface];
    if (own === "native" || (!own && choice === "native")) return false;
    const refs = choiceRefs(own ?? choice);
    const hasSurfaceRef = panes.some(
      (p) =>
        servesSurface(p, surface) &&
        p.dashboardRef &&
        refs.some((r) => matchesRef(r, p.dashboardRef!)),
    );
    if (hasSurfaceRef) return refs.some((r) => matchesRef(r, ref, includeInstalledSource));
    return false;
  }
  return selectedRefs(slot, ref.org).some((r) => matchesRef(r, ref, includeInstalledSource));
}

/** The Inbox `org` shows among a team's listed `panes`: its chosen dashboard, else the first by
 *  layer (team, private, built-in). The listing is the owning team's, not necessarily the active one. */
export function inboxOfListing(org: string, panes: readonly PaneEntry[]): PaneEntry | undefined {
  const inboxes = panes.filter((p) => p.dashboardRef && servesSurface(p, "inbox"));
  const own = profileOf(org)?.rightPaneSurfaces?.inbox;
  const refs = own && own !== "native" ? [own] : selectedRefs("right-pane", org);
  const chosen = refs
    .map((ref) => {
      const matches = inboxes.filter(
        (pane) => dashboardRefKey(pane.dashboardRef!) === dashboardRefKey(ref),
      );
      return matches.find((pane) => canonicalSurface(pane.surface) === "inbox") ?? matches[0];
    })
    .find((pane) => pane !== undefined);
  return (
    chosen ??
    ["team", "private", "built-in"]
      .map((layer) =>
        inboxes.find((pane) => isInboxDashboard(pane) && pane.dashboardRef?.scope === layer),
      )
      .find((pane) => pane !== undefined) ??
    inboxes[0]
  );
}

/** Host-owned scope seam; a later story can add all-team or fixed-team scopes. */
export type InboxScope = "active-team";
export function resolveInboxDashboard(
  activeTeam: { org: string; team: string },
  scope: InboxScope = "active-team",
): PaneEntry | undefined {
  const { org, team } = activeTeam;
  const state = usePaneStore.getState();
  if (scope !== "active-team" || state.teamKey !== buildTeamKey(org, team)) return;
  return inboxOfListing(org, state.listing?.panes ?? []);
}
function currentInbox(): PaneEntry | undefined {
  const { activeOrgSlug: org, activeTeamName: team } = useOrgStore.getState();
  return org && team ? resolveInboxDashboard({ org, team }) : undefined;
}

export function servesSurface(
  pane: Pick<PaneEntry, "id" | "surface" | "declaration" | "dashboardRef">,
  surface?: string,
) {
  return (
    !surface ||
    canonicalSurface(
      pane.surface ?? pane.declaration?.surface ?? pane.dashboardRef?.installedFrom?.id ?? pane.id,
    ) === canonicalSurface(surface)
  );
}
/** Whether `pane` may be placed in `slot`: a pane with declared slots only goes where it says. */
export function declaresSlot(
  pane: Pick<PaneEntry, "declaration" | "dashboardRef">,
  slot: SlotId,
): boolean {
  const slots = pane.declaration?.slots;
  // Bundled files without slots are tab-only; ordinary undeclared panes retain their default.
  return pane.dashboardRef?.scope === "built-in"
    ? !!slots?.includes(slot)
    : !slots?.length || slots.includes(slot);
}
/** The panes the active team can place in a slot: its listing, then known entries
 *  still in view. Null while the listing belongs to another team. */
function slotPanes(): PaneEntry[] | null {
  const { activeOrgSlug: org, activeTeamName: team } = useOrgStore.getState();
  const state = usePaneStore.getState();
  if (!org || !team || state.teamKey !== buildTeamKey(org, team)) return null;
  const listed = state.listing?.panes ?? [];
  const listedIds = new Set(listed.map((p) => p.panelId));
  const chain = new Set(
    [team, ...(state.listing?.chain ?? [])].map((name) => buildTeamKey(org, name)),
  );
  return [
    ...listed,
    ...Object.values(state.knownEntries).filter(
      (p) => !listedIds.has(p.panelId) && paneInView(p.panelId, org, chain),
    ),
  ];
}

/** The dashboards the profile assigns to `slot`, in assignment order (the profile's
 *  slot list), as the active team lists them. */
export function slotDashboards(slot: SlotId): PaneEntry[] {
  const org = useOrgStore.getState().activeOrgSlug;
  const order = (org ? selectedRefs(slot, org) : []).map(dashboardRefKey);
  const rank = (p: PaneEntry) => {
    const at = order.indexOf(dashboardRefKey(p.dashboardRef!));
    return at < 0 ? order.length : at;
  };
  if (slot === "dock" || slot === "file-nav")
    return order.flatMap((key) => {
      const pane = usePaneStore.getState().sourceRows[key]?.entry;
      return pane && pane.status !== "rejected" && declaresSlot(pane, slot) ? [pane] : [];
    });
  return (slotPanes() ?? [])
    .filter(
      (p) =>
        p.dashboardRef &&
        declaresSlot(p, slot) &&
        p.status !== "rejected" &&
        dashboardSelected(p.dashboardRef, false, slot),
    )
    .sort((a, b) => rank(a) - rank(b))
    .map((pane) =>
      slot === "right-pane" && pane === currentInbox()
        ? { ...pane, panelId: INBOX_PANEL_ID }
        : pane,
    );
}

export function resolveSlot(slot: SlotId, options: RevealSlotOptions = {}) {
  const { activeOrgSlug: org, activeTeamName: team } = useOrgStore.getState();
  const listed = slotPanes();
  const current = listed !== null;
  const panes = (listed ?? []).filter((pane) => declaresSlot(pane, slot));
  const explicit = options.dashboard;
  const inbox = slot === "right-pane" && canonicalSurface(options.surface) === "inbox";
  const targetOrg = options.intent?.org;
  const targetTeam = options.intent?.team;
  const crossTeam = targetOrg && targetTeam && (targetOrg !== org || targetTeam !== team);
  const synced =
    !crossTeam ||
    useOrgStore
      .getState()
      .orgs.some((o) => o.slug === targetOrg && o.teams.some((t) => t.name === targetTeam));
  const candidates = panes.filter(
    (p) =>
      servesSurface(p, options.surface) &&
      p.dashboardRef &&
      dashboardSelected(p.dashboardRef, false, slot) &&
      (!crossTeam || servesSurface(p, "inbox")),
  );
  const dashboard = explicit
    ? panes.find((p) =>
        typeof explicit === "string"
          ? p.panelId === explicit
          : p.dashboardRef && matchesRef(p.dashboardRef, explicit),
      )
    : synced
      ? ((inbox ? currentInbox() : undefined) ??
        candidates.find((p) => canonicalSurface(p.surface) === canonicalSurface(options.surface)) ??
        candidates[0])
      : undefined;
  // Architecture: unresolved refs cannot identify a pane; use the surface default and
  // report missing choices once so an absent Notes page remains diagnosable.
  if (current && !explicit && !dashboard && import.meta.env.DEV) {
    const missing = selectedRefs(slot, org!)
      .map(dashboardRefKey)
      .filter(
        (key) =>
          !panes.some((pane) => pane.dashboardRef && dashboardRefKey(pane.dashboardRef) === key),
      )
      .sort();
    const warning = JSON.stringify([org, slot, missing]);
    if (missing.length && !warnedMissingChoices.has(warning)) {
      warnedMissingChoices.add(warning);
      console.warn(`Unresolved dashboard choices: ${missing.join(", ")}`);
    }
  }
  const native = nativeSurface(slot, options.surface);
  const explicitId =
    typeof explicit === "string"
      ? explicit
      : explicit
        ? // A listed built-in (the bundled inbox) mounts under its listing's panel id.
          (explicit.scope === "built-in" && dashboard?.panelId) || refPanelId(explicit)
        : undefined;
  return {
    dashboard,
    panelId:
      inbox && (!explicit || dashboard === currentInbox())
        ? INBOX_PANEL_ID
        : (explicitId ?? dashboard?.panelId ?? native?.panelId ?? null),
    native,
  };
}
/** The panel id a dashboard ref mounts under. */
export function refPanelId(ref: DashboardRef): string {
  return ref.scope === "built-in" ? ref.id : panePanelId(ref.scope, ref.org, ref.team, ref.id);
}
/** Shows the dock as the tray's Agents control does: the CLI column comes back and the dock
 *  unfolds. With `focus`, focus moves into the chosen dashboard's page or the built-in list. */
function revealDock(options: RevealSlotOptions): void {
  const { activeOrgSlug: org, activeTeamName: team } = useOrgStore.getState();
  if (!org || !team) return;
  const teamKey = buildTeamKey(org, team);
  const viewer = useViewerStore.getState();
  const shown = !viewer.workspacePanelCollapsed && isTerminalColumnShown(viewer);
  const dashboard = selectedRefs("dock", org)[0];
  // The built-in list takes this request as it opens (the dock host takes it for a page).
  if (options.focus && !shown) requestAgentsFocus(teamKey, "list");
  revealCollapsedTerminal();
  if (viewer.workspacePanelCollapsed) viewer.setWorkspacePanelCollapsed(false);
  if (!options.focus) return;
  if (dashboard) {
    takeAgentsFocus(teamKey, "list");
    requestFrameFocus(DOCK_FRAME_PANEL_ID);
  } else if (shown) focusAgentsList(teamKey);
}
export function revealSlot(slot: SlotId, options: RevealSlotOptions = {}): void {
  if (slot === "file-nav") {
    const viewer = useViewerStore.getState();
    const { activeOrgSlug: org, activeTeamName: team } = useOrgStore.getState();
    if (org && team) viewer.setNavCollapsed(buildTeamKey(org, team), false);
    if (options.focus) {
      const org = useOrgStore.getState().activeOrgSlug;
      if (org && selectedRefs("file-nav", org).length) requestFrameFocus(FILE_NAV_FRAME_PANEL_ID);
      else document.querySelector<HTMLElement>('[data-file-nav-active="true"] input')?.focus();
    }
    return;
  }
  if (slot === "dock") {
    revealDock(options);
    return;
  }
  const { panelId } = resolveSlot(slot, options);
  const viewer = useViewerStore.getState();
  const inbox = canonicalSurface(options.surface) === "inbox";
  if (!panelId) return;
  if (inbox && !options.intent && !options.dashboard && options.presentation !== "overlay") {
    if (paneOnScreen(panelId, viewer.rightPaneOpen, viewer.rightPaneLayout)) {
      viewer.toggleRightPane();
      return;
    }
  }
  if (inbox || options.intent)
    usePaneStore.getState().setIntent(
      panelId,
      inbox
        ? {
            focusComposer: !!(options.intent?.conversation || options.intent?.messageId),
            ...options.intent,
            focusList: !options.intent?.conversation && !options.intent?.messageId,
          }
        : options.intent!,
    );
  viewer.revealPanelById(panelId);
  if (options.focus) requestFrameFocus(panelId);
  if (options.presentation === "overlay") {
    const { activeOrgSlug: org, activeTeamName: team } = useOrgStore.getState();
    if (org && team) viewer.setMaximized(buildTeamKey(org, team), panelId);
  }
}
export function useSlotTarget(slot: SlotId, options: RevealSlotOptions) {
  useDashboardProfileStore((s) => s.profile);
  useOrgStore((s) => s.activeOrgSlug);
  useOrgStore((s) => s.activeTeamName);
  usePaneStore((s) => s.listing);
  usePaneStore((s) => s.teamKey);
  const resolved = resolveSlot(slot, options);
  const panelId = resolved.panelId;
  const paneOpen = usePaneOnScreen(panelId ?? "");
  return {
    panelId,
    open: paneOpen,
    activate: () => revealSlot(slot, options),
  };
}
