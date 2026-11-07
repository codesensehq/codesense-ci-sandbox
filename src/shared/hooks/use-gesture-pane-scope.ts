// SPDX-License-Identifier: MIT
import { useEffect } from "react";

/**
 * Marker attribute placed on each Excalidraw canvas container so the
 * gesture-scope guard can tell whether a trackpad-pinch gesture originated
 * inside a canvas. Exported and shared by both the canvas and the guard so
 * the attribute name can never drift between them.
 */
export const EXCALIDRAW_CANVAS_ATTR = "data-excalidraw-canvas";

const CANVAS_SELECTOR = `[${EXCALIDRAW_CANVAS_ATTR}]`;

/**
 * True when a gesture that originated at `target` is outside every Excalidraw
 * canvas — i.e. it must NOT reach Excalidraw's document-level pinch handler.
 */
export function isGestureOutsideCanvas(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) return true;
  return target.closest(CANVAS_SELECTOR) === null;
}

// One shared set of document listeners backs every caller (the hook mounts
// once per open team). Module-level stable refs mean a single listener
// identity and a single global decision; `refCount` keeps the add/remove
// lifecycle correct when several callers overlap.
let refCount = 0;

// The pinch's disposition is latched at `gesturestart` and held for the whole
// gesture, so one continuous pinch is handled atomically even if WebKit
// retargets `gesturechange`/`gestureend` across a pane boundary mid-pinch —
// which would otherwise split a gesture's start/change/end across the decision
// line and desync Excalidraw's pinch state.
//   - "pass":     inside an Excalidraw canvas → let Excalidraw handle it.
//   - "suppress": outside any canvas → kill it.
type GestureMode = "pass" | "suppress";
let mode: GestureMode = "pass";

function suppress(e: Event): void {
  // preventDefault stops WebKit's native full-page pinch-zoom (the gesture no
  // longer reaches any handler that would call it); stopPropagation keeps it
  // from Excalidraw's bubble-phase `document` handler. Not
  // stopImmediatePropagation: nothing else consumes `gesture*`, and staying
  // within-phase avoids coupling correctness to listener registration order.
  e.preventDefault();
  e.stopPropagation();
}

function onGestureStart(e: Event): void {
  if (!isGestureOutsideCanvas(e.target)) {
    // Inside a canvas → pass straight through to Excalidraw, untouched.
    mode = "pass";
    return;
  }
  mode = "suppress";
  suppress(e);
}

function onGestureChange(e: Event): void {
  if (mode === "pass") return;
  suppress(e);
}

function onGestureEnd(e: Event): void {
  if (mode !== "pass") suppress(e);
  // Reset to the neutral (allow) state so a gesture that never delivered a
  // `gesturestart` to this listener — guard mounted mid-pinch, or a dropped
  // first event — can't inherit this gesture's decision. Fail-safe direction
  // is "allow": never a canvas pinch hard-killed by a stale latch +
  // capture-phase stopPropagation.
  mode = "pass";
}

// WebKit-only trackpad pinch events, absent from the TS DOM lib.
function addGuard(): void {
  document.addEventListener("gesturestart", onGestureStart, true);
  document.addEventListener("gesturechange", onGestureChange, true);
  document.addEventListener("gestureend", onGestureEnd, true);
}

function removeGuard(): void {
  document.removeEventListener("gesturestart", onGestureStart, true);
  document.removeEventListener("gesturechange", onGestureChange, true);
  document.removeEventListener("gestureend", onGestureEnd, true);
}

/**
 * SW-SC-10: scope trackpad-pinch gestures to the pane under the pointer on
 * WebKit, where a bare pinch arrives as `gesturestart/change/end`.
 *
 * Excalidraw registers its `gesture*` handlers on `document` (verified in the
 * bundle), so without scoping a pinch zooms the canvas no matter where the
 * pointer is. Wheel and scroll are already container-scoped, so they need no
 * guarding.
 *
 * The guard is ONE capture-phase `document` listener set, shared across all
 * callers. Capture on `document` runs before Excalidraw's bubble-phase handler.
 * The disposition is global and latched at gesture start (never per-canvas
 * instance), so keep-alive hidden canvases can neither suppress the visible
 * one's zoom nor split a boundary-crossing pinch.
 */
export function useGesturePaneScope(): void {
  useEffect(() => {
    if (refCount++ === 0) addGuard();
    return () => {
      if (--refCount === 0) {
        removeGuard();
        // Last caller gone → no pinch can be in flight. Clear any latch a
        // dropped `gestureend` or an unmount mid-pinch would otherwise leave
        // dirty for the next mount.
        mode = "pass";
      }
    };
  }, []);
}
