// SPDX-License-Identifier: MIT
/**
 * Pure decision for how to handle an external write that lands while a
 * `.excalidraw` canvas is open (SW-SC-3). Kept in its own Excalidraw-free
 * module so the policy is unit-testable without importing the heavy engine.
 *
 * - `noop`  — the incoming scene equals what we believe is on disk, so nothing
 *   actually diverged (covers a self-save echo that slipped past the watcher
 *   suppression, AC-4 defense b).
 * - `apply` — content differs and the user has no unsaved edits → live-reload
 *   in place (AC-2).
 * - `nudge` — content differs AND the user has local edits → never clobber;
 *   surface a "reload?" affordance instead (AC-3).
 */
export function decideCanvasReload({
  externalSig,
  diskSig,
  dirty,
}: {
  externalSig: string;
  diskSig: string;
  dirty: boolean;
}): "noop" | "apply" | "nudge" {
  if (externalSig === diskSig) return "noop";
  return dirty ? "nudge" : "apply";
}
