// SPDX-License-Identifier: MIT
import type { ToasterProps } from "sonner";

/**
 * Where toasts sit. Terminal tiles carry their controls (tabs, ×, menus) along
 * their TOP edge, so toasts rise from the bottom of the window, above any
 * bottom chrome (the workspace tray), and never over a tile's chrome bar.
 */
export const TOASTER_POSITION: NonNullable<ToasterProps["position"]> = "bottom-right";

/** Gap between a toast and the window's side edges. */
export const TOAST_EDGE_GAP_PX = 16;
/** Gap between a toast and the bottom chrome it sits on (or the window's bottom
 *  edge): kept tight so the toast stays low, clear of the tile chrome above it. */
export const TOAST_BOTTOM_GAP_PX = 8;

/** The measured height of the chrome pinned to the window's bottom edge. */
export const TOAST_BOTTOM_CLEARANCE_VAR = "--cs-toast-bottom-clearance";

export const TOASTER_OFFSET = {
  top: TOAST_EDGE_GAP_PX,
  right: TOAST_EDGE_GAP_PX,
  left: TOAST_EDGE_GAP_PX,
  bottom: `calc(var(${TOAST_BOTTOM_CLEARANCE_VAR}, 0px) + ${TOAST_BOTTOM_GAP_PX}px)`,
} as const;
