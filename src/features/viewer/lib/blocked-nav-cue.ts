// SPDX-License-Identifier: MIT
// Blocked-navigation visible cue. When a keyboard-nav command has nowhere to go
// (a boundary no-op), the command's dispatch site flashes a brief, purely
// presentational cue on the active tile so the press reads as "you're at the
// edge" rather than a frozen app or a broken keybinding.
//
// The motion vs reduced-motion split lives entirely in `index.css` (the cue
// classes animate transform on the compositor; the reduced-motion `@media` guard
// swaps them for a static momentary outline). This module only toggles the class
// and owns the coalescing, so the JS never branches on the motion preference and
// stays trivially testable.

import { shownPaneFrame } from "./shown-pane-frame";

export type BlockedNavCue = "pulse" | "nudge-left" | "nudge-right" | "nudge-up" | "nudge-down";

const CUE_CLASS: Record<BlockedNavCue, string> = {
  pulse: "blocked-nav-pulse",
  "nudge-left": "blocked-nav-nudge-left",
  "nudge-right": "blocked-nav-nudge-right",
  "nudge-up": "blocked-nav-nudge-up",
  "nudge-down": "blocked-nav-nudge-down",
};

const ALL_CUE_CLASSES = Object.values(CUE_CLASS);

// The class stays applied for this long, then is removed. MUST equal the
// `animation-duration` on the `.blocked-nav-*` cue classes in `index.css` so the
// class clears exactly as the animation settles back to identity — drift either
// cuts the animation short or leaves the class lingering. This timer is also the
// coalescing handle, not a second clock racing the animation. Exported so a test
// can enforce the parity with the CSS `animation-duration`.
export const CUE_DURATION_MS = 240;

// One in-flight cue per element. A rapid repeat cancels the pending removal and
// restarts a single animation instead of stacking a backlog.
const activeTimers = new WeakMap<Element, ReturnType<typeof setTimeout>>();

/**
 * Flash a brief blocked-nav cue on `el`. No-ops when `el` is null (the caller
 * could not resolve a target element). Safe to call on every keypress — rapid
 * repeats coalesce.
 */
export function flashBlockedNav(el: HTMLElement | null, cue: BlockedNavCue): void {
  if (!el) return;

  const pending = activeTimers.get(el);
  if (pending !== undefined) clearTimeout(pending);

  el.classList.remove(...ALL_CUE_CLASSES);
  // Force a reflow so re-adding the class restarts the CSS animation from the
  // first frame. A single layout read on one small element per discrete no-op
  // keypress — not on a scroll or key-repeat-batched path.
  void el.offsetWidth;
  el.classList.add(CUE_CLASS[cue]);

  const timer = setTimeout(() => {
    el.classList.remove(CUE_CLASS[cue]);
    activeTimers.delete(el);
  }, CUE_DURATION_MS);
  activeTimers.set(el, timer);
}

/**
 * The visible tile frame for `workspaceId`, or null when it isn't on screen.
 * Matches the same `[data-pane-id]` frame `readPaneRects` /
 * `focusWorkspaceTerminal` key off, so the cue lands on the one hook present in
 * every layout mode.
 */
export function activeTileEl(workspaceId: string): HTMLElement | null {
  // The empty-roster path passes "": no frame.
  return shownPaneFrame(workspaceId);
}

/**
 * Flash the blocked-nav cue on a workspace's active tile. The one op the nav
 * command dispatch sites call — it resolves the tile and no-ops safely when the
 * workspace isn't on screen (an empty/minimized roster passes `undefined`), so
 * callers never repeat the tile lookup or the null guard.
 */
export function flashBlockedNavForWorkspace(
  workspaceId: string | undefined,
  cue: BlockedNavCue,
): void {
  flashBlockedNav(activeTileEl(workspaceId ?? ""), cue);
}
