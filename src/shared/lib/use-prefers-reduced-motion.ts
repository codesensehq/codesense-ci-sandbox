// SPDX-License-Identifier: MIT
/**
 * `prefers-reduced-motion` as a live React value.
 *
 * The CSS half of reduced-motion is handled in `index.css` (`@media
 * (prefers-reduced-motion: reduce)` guards next to each keyframe). This hook is
 * ONLY for the JS branches that a media query can't express — e.g. deciding
 * whether an exiting row should linger for a collapse transition or drop
 * instantly. Prefer the CSS guard for anything purely visual.
 */
import { useEffect, useState } from "react";

const QUERY = "(prefers-reduced-motion: reduce)";

/** Read the current value once (safe when `matchMedia` is absent). */
export function prefersReducedMotion(): boolean {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") return false;
  return window.matchMedia(QUERY).matches;
}

/** Subscribe to `prefers-reduced-motion`, re-rendering when the OS setting flips. */
export function usePrefersReducedMotion(): boolean {
  const [reduced, setReduced] = useState(prefersReducedMotion);
  useEffect(() => {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function") return;
    const mql = window.matchMedia(QUERY);
    const onChange = () => setReduced(mql.matches);
    onChange();
    // `addEventListener` on MediaQueryList is unsupported on older WebKit
    // (the Tauri webview on older macOS) — fall back to the deprecated
    // `addListener` there.
    if (typeof mql.addEventListener === "function") {
      mql.addEventListener("change", onChange);
      return () => mql.removeEventListener("change", onChange);
    }
    mql.addListener(onChange);
    return () => mql.removeListener(onChange);
  }, []);
  return reduced;
}
