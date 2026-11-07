// SPDX-License-Identifier: MIT
/**
 * The bottom workspace bar's size (Settings → General → "Workspace bar size").
 *
 * One factor scales the whole bar — pills, crew avatars, badges, the Agents
 * control, the "+N", the CLI controls at its right end and the row's height —
 * so nothing drifts out of proportion at any size. It is applied as CSS `zoom`
 * on the bar's box: the bar's parts are drawn in fixed px (icon and avatar
 * sizes, paddings), which a font-size scale alone would leave behind. The "+N"
 * menus (portaled out of the box) get the same zoom; the chip's right-click
 * menu and the first-run hint stay at the app's size (the menu is placed at the
 * pointer's viewport coordinates, which a zoomed ancestor would scale away).
 *
 * Normal is the bar as it was before #2094 shrank it (labels at the inherited
 * 16px). Small is one gentle step down (14px labels); it shrinks the rows,
 * avatars and badges with them, but badge text never draws smaller than at
 * Normal and the Agents control keeps a 24px hit target (`--bar-floor` in
 * agents.css). Large is a step up (20px labels).
 */

export type WorkspaceBarSize = "small" | "normal" | "large";

export const WORKSPACE_BAR_SIZES: readonly WorkspaceBarSize[] = ["small", "normal", "large"];

export const DEFAULT_WORKSPACE_BAR_SIZE: WorkspaceBarSize = "normal";

const WORKSPACE_BAR_ZOOM: Record<WorkspaceBarSize, number> = {
  small: 0.875,
  normal: 1,
  large: 1.25,
};

/** A persisted value as a real size: anything unknown (a corrupted blob, a
 *  value from a newer build) reads as the default. */
export function resolveWorkspaceBarSize(value: unknown): WorkspaceBarSize {
  return WORKSPACE_BAR_SIZES.includes(value as WorkspaceBarSize)
    ? (value as WorkspaceBarSize)
    : DEFAULT_WORKSPACE_BAR_SIZE;
}

/** The zoom factor the bar is drawn at for `size`. */
export function workspaceBarZoom(size: unknown): number {
  return WORKSPACE_BAR_ZOOM[resolveWorkspaceBarSize(size)];
}

/**
 * The zoom `el` is drawn at, measured: its drawn width (`getBoundingClientRect`,
 * in viewport px) over its layout width (`offsetWidth`, in its own px). Code that
 * mixes the two — a viewport-px distance turned into a `scrollBy`, or a px margin
 * added to a measured width — converts with this. 1 with no layout to measure.
 */
export function measuredZoom(el: HTMLElement | null): number {
  if (!el) return 1;
  const layout = el.offsetWidth;
  if (!layout) return 1;
  const drawn = el.getBoundingClientRect().width;
  return drawn > 0 ? drawn / layout : 1;
}
