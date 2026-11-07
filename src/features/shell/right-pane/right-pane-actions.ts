// SPDX-License-Identifier: MIT
/**
 * The ONE add/remove seam every right-pane management surface shares — the
 * native View → Panels menu (`panel-menu.ts`) and the pane opener. The placement
 * guard (retired/non-offerable) and the open-pane / close-on-last behavior live
 * here ONCE, so the surfaces can never diverge on "what's present" or on which
 * placements are legal.
 *
 * Both actions are synchronous, in-memory operations over the viewer store
 * (`getState()`), never disk or a Tauri command — so a click can
 * never block the render thread. The layout mutations themselves are the pure
 * ops from `right-pane-layout.ts`.
 */
import { useViewerStore } from "@/shared/stores/viewer-store";

import { getPanel } from "./panel-registry";
import { addPanelAsTopSection, findPanelLocation, removePanel } from "./right-pane-layout";

/**
 * Add a registered panel to the right-pane layout, opening the pane if it was
 * closed. No-op (and reports success) when the panel is already present —
 * `addPanelAsTopSection` de-dupes and `coerceRightPaneLayout` is the backstop.
 *
 * Refuses — returns `false`, no mutation — a retired (`offerable: false`) panel.
 * This is defense in depth: both callers already filter these out, but a
 * stale/forged id must never re-add a retired panel. Returns `true` when the
 * panel is present after the call (added or already there).
 */
export function addPanelToRightPane(panelId: string): boolean {
  const store = useViewerStore.getState();
  const layout = store.rightPaneLayout;
  if (findPanelLocation(layout, panelId) !== null) return true; // already present
  if (getPanel(panelId)?.offerable === false) return false;
  store.setRightPaneLayout(addPanelAsTopSection(layout, panelId));
  if (!store.rightPaneOpen) store.toggleRightPane();
  return true;
}

/**
 * Remove a panel from the right-pane layout, closing the whole pane when the
 * last panel goes (nothing left to show). No-op-safe on an absent panel.
 */
export function removePanelFromRightPane(panelId: string): void {
  const store = useViewerStore.getState();
  const next = removePanel(store.rightPaneLayout, panelId);
  store.setRightPaneLayout(next);
  if (next.sections.length === 0 && store.rightPaneOpen) store.toggleRightPane();
}
