// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";

/** True when the window both has OS focus AND the document is visible. A hidden
 *  window (minimize/hide) can fire `visibilitychange` with NO window `blur`, so
 *  focus alone would keep live work running while backgrounded — we gate on both. */
export function computeFocused(): boolean {
  if (typeof document === "undefined") return true;
  return document.hasFocus() && document.visibilityState !== "hidden";
}

/**
 * Whether the app window is currently foreground-active — has OS focus AND is
 * visible (not minimized/hidden). Seeds from {@link computeFocused} and tracks
 * `window` focus/blur + `document` visibilitychange.
 */
export function useWindowFocused(): boolean {
  const [focused, setFocused] = useState<boolean>(computeFocused);

  useEffect(() => {
    let cancelled = false;
    const sync = () => {
      if (!cancelled) setFocused(computeFocused());
    };
    window.addEventListener("focus", sync);
    window.addEventListener("blur", sync);
    document.addEventListener("visibilitychange", sync);
    // Close the seed→subscribe race: re-read once now that the listeners are
    // attached, so a focus/visibility flip between the render seed and this
    // subscription isn't stranded. Deferred out of the effect body (not a
    // synchronous setState-in-effect); the `cancelled` guard prevents a
    // setState after a fast mount→unmount.
    queueMicrotask(sync);
    return () => {
      cancelled = true;
      window.removeEventListener("focus", sync);
      window.removeEventListener("blur", sync);
      document.removeEventListener("visibilitychange", sync);
    };
  }, []);

  return focused;
}

function computeVisible(): boolean {
  return typeof document === "undefined" || document.visibilityState !== "hidden";
}

/** Whether the app window is on screen (not minimized or hidden), focused or not: a window
 *  the user can see keeps its live views live even while they type elsewhere. */
export function useWindowVisible(): boolean {
  const [visible, setVisible] = useState<boolean>(computeVisible);
  useEffect(() => {
    const sync = () => setVisible(computeVisible());
    document.addEventListener("visibilitychange", sync);
    sync();
    return () => document.removeEventListener("visibilitychange", sync);
  }, []);
  return visible;
}
