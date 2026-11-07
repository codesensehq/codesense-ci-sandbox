// SPDX-License-Identifier: MIT
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { RightPaneLayout } from "../right-pane-layout";

/** Whether the pane is on screen: the right pane is open and the pane is the
 *  active panel of an expanded section in the layout on screen. */
export function paneOnScreen(panelId: string, open: boolean, layout: RightPaneLayout): boolean {
  const section = layout.sections.find((s) => s.group.panelIds.includes(panelId));
  return open && !!section && !section.collapsed && section.group.activePanelId === panelId;
}

export function usePaneOnScreen(panelId: string): boolean {
  const open = useViewerStore((s) => s.rightPaneOpen);
  const layout = useViewerStore((s) => s.rightPaneLayout);
  return paneOnScreen(panelId, open, layout);
}
