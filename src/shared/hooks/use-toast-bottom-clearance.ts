// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";

import { TOAST_BOTTOM_CLEARANCE_VAR } from "@/components/ui/toaster-placement";

const heights = new Map<object, number>();

function publish(): void {
  const clearance = Math.max(0, ...heights.values());
  document.documentElement.style.setProperty(TOAST_BOTTOM_CLEARANCE_VAR, `${clearance}px`);
}

/**
 * Registers an element pinned to the window's bottom edge, so toasts sit above
 * it. Returns a callback ref. A hidden element measures 0; the tallest one wins.
 */
export function useToastBottomClearance(): (el: HTMLElement | null) => void {
  const [el, setEl] = useState<HTMLElement | null>(null);
  useEffect(() => {
    if (!el) return;
    const key = {};
    const measure = () => {
      heights.set(key, el.offsetHeight);
      publish();
    };
    measure();
    const ro = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    ro?.observe(el);
    return () => {
      ro?.disconnect();
      heights.delete(key);
      publish();
    };
  }, [el]);
  return setEl;
}
