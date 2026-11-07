// SPDX-License-Identifier: MIT
/**
 * SW-RP-1: bridges the panel registry + right-pane layout to the native
 * View → Panels submenu. The registry is the single source of truth — the menu
 * is rebuilt (wholesale) whenever the set of registered panels or their
 * layout membership changes, so a newly-registered panel (e.g. the Agent from
 * SW-GS-10) appears automatically with no per-panel menu code.
 */
import { useEffect } from "react";

import { logger } from "@/shared/lib/logger";
import { NAV_MENU_ITEMS } from "@/shared/lib/shortcuts/registry";
import { getEffectiveShortcut, useShortcutsStore } from "@/shared/stores/shortcuts-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { rebuildMenu, type NavMenuEntry, type PanelMenuEntry } from "@/tauri/commands";

import {
  isPanelListable,
  listPanels,
  usePanelRegistry,
  type PanelDefinition,
} from "./panel-registry";
import { addPanelToRightPane, removePanelFromRightPane } from "./right-pane-actions";
import { findPanelLocation } from "./right-pane-layout";

/** Menu-item id prefix marking a panel toggle (vs a shortcut action). */
export const PANEL_MENU_ID_PREFIX = "panel:";

/** The registered panels OFFERED in the menu. The single basis shared by the
 *  menu entries AND the rebuild signature, so the two can never disagree about
 *  which panels are checkable. SW-AU-38 (AC-0): drops panels flagged
 *  `offerable: false` — a retired surface that stays registered so saved layouts
 *  render but is no longer offered for re-adding — UNLESS it is currently present
 *  in the layout. A retired panel can still land in the layout from a saved
 *  layout; since the menu uncheck is the ONLY removal vector (panels have no
 *  close button), a present retired panel must keep its checked entry so the
 *  user can remove it. Once removed it drops from the menu again (can't be
 *  re-added — the add-branch refuses a non-offerable id). */
function listedPanels(): PanelDefinition[] {
  const layout = useViewerStore.getState().rightPaneLayout;
  return listPanels().filter((p) => isPanelListable(p, findPanelLocation(layout, p.id) !== null));
}

/** Build the checkable Panels entries from the registry + the layout, so the
 *  check marks reflect the pane the user is looking at. */
export function buildPanelMenuEntries(): PanelMenuEntry[] {
  const layout = useViewerStore.getState().rightPaneLayout;
  return listedPanels().map((p) => ({
    id: p.id,
    label: p.title,
    checked: findPanelLocation(layout, p.id) !== null,
  }));
}

/** Build the curated nav entries for the native Go submenu: the registry's
 *  authoritative {@link NAV_MENU_ITEMS} with each id's live effective accelerator
 *  resolved at build time, so a rebind/relabel in Settings flows through on the
 *  next rebuild with no Rust change and no duplicated keystroke. */
export function buildNavMenuEntries(): NavMenuEntry[] {
  return NAV_MENU_ITEMS.map((item) => ({
    id: item.id,
    label: item.label,
    accelerator: getEffectiveShortcut(item.id),
  }));
}

/** Rebuild the native menu with the current shortcut overrides + panels + nav.
 *  The single caller of the raw command — the shortcut rebind path, the
 *  panel-sync hook, and the nav section all route through here so none wipes the
 *  others' items. */
export function rebuildNativeMenu(): void {
  const overrides = useShortcutsStore.getState().shortcuts;
  rebuildMenu(overrides, buildPanelMenuEntries(), buildNavMenuEntries()).catch((e) => {
    logger.warn("panel-menu", `rebuildNativeMenu failed — ${e}`);
  });
}

/** Handle a `menu:action` whose id is a panel toggle. Returns false if the id
 *  is not a panel id (caller falls back to the shortcut registry). The
 *  present→remove / absent→add toggle and every placement guard live in the
 *  shared `right-pane-actions` seam (SW-WG-3 AC-9) that the gallery also calls,
 *  so the two surfaces can never diverge. */
export function handlePanelMenuAction(menuId: string): boolean {
  if (!menuId.startsWith(PANEL_MENU_ID_PREFIX)) return false;
  const panelId = menuId.slice(PANEL_MENU_ID_PREFIX.length);
  const layout = useViewerStore.getState().rightPaneLayout;
  const present = findPanelLocation(layout, panelId) !== null;
  if (present) removePanelFromRightPane(panelId);
  else addPanelToRightPane(panelId);
  return true;
}

/** Signature of what the menu depends on: the registered panel set + which of
 *  them are present in the layout. Excludes sizes/collapse/active tab so the
 *  menu isn't rebuilt on every resize/collapse/tab switch. */
function menuMembershipSignature(): string {
  const layout = useViewerStore.getState().rightPaneLayout;
  // The same `listedPanels` basis the menu is built from.
  const available = listedPanels();
  // Include id+title (a re-register with a new label must refresh the menu) and
  // which panels are present (the check marks). Excludes size/collapse/active so
  // resizes and tab switches don't churn the native menu.
  const labels = available.map((p) => `${p.id}:${p.title}`);
  const present = available
    .filter((p) => findPanelLocation(layout, p.id) !== null)
    .map((p) => p.id);
  return `${labels.slice().sort().join(",")}|${present.slice().sort().join(",")}`;
}

/** Keep the native Panels submenu in sync with the registry + layout
 *  membership. Mount once near the app root. */
export function useRightPaneMenuSync(): void {
  useEffect(() => {
    // Defensive: in some test harnesses the stores are mocked without the
    // zustand `subscribe` API — no-op rather than crash the app shell.
    if (
      typeof useViewerStore.subscribe !== "function" ||
      typeof usePanelRegistry.subscribe !== "function"
    ) {
      return;
    }
    let sig = menuMembershipSignature();
    rebuildNativeMenu();
    const onChange = () => {
      const next = menuMembershipSignature();
      if (next !== sig) {
        sig = next;
        rebuildNativeMenu();
      }
    };
    const unsubRegistry = usePanelRegistry.subscribe(onChange);
    const unsubViewer = useViewerStore.subscribe(onChange);
    return () => {
      unsubRegistry();
      unsubViewer();
    };
  }, []);
}
