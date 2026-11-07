// SPDX-License-Identifier: MIT
import { useSyncExternalStore } from "react";

function subscribe(onChange: () => void): () => void {
  document.addEventListener("visibilitychange", onChange);
  return () => document.removeEventListener("visibilitychange", onChange);
}

const visibleNow = (): boolean => document.visibilityState !== "hidden";

/**
 * Whether the app's window is showing (not minimized or hidden). Mail that arrives in an open
 * conversation while it is hidden has not been seen, so nothing marks it read.
 */
export function useDocumentVisible(): boolean {
  return useSyncExternalStore(subscribe, visibleNow, () => true);
}
