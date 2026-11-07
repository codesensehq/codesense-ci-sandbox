// SPDX-License-Identifier: MIT
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { MouseEvent } from "react";

const INTERACTIVE = "button, a, input, select, textarea, [role='button']";

/**
 * onMouseDown handler for titlebar drag regions.
 * Starts native window dragging unless the click target is an interactive element.
 */
export function startDragging(e: MouseEvent) {
  if (e.button !== 0) return;
  const target = e.target as HTMLElement;
  // SW-EW-41: workspace-tray chips (the console switcher pills) are dnd-kit
  // draggables — drag one onto a terminal tile to split the mosaic. Let their
  // pointer-down reach dnd-kit instead of starting a window move. Empty strip
  // area still falls through to startDragging so the window stays draggable.
  if (target.closest(INTERACTIVE) || target.closest("[data-tray-chip]")) return;
  getCurrentWindow().startDragging();
}
