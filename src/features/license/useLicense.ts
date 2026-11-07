// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";

import { licenseState, onLicenseState, type LicenseView } from "./api";

/** How often a failed state read is tried again. */
export const READ_RETRY_MS = 3_000;

export interface LicenseViewState {
  /** The current view, or null until the first read. */
  view: LicenseView | null;
}

/**
 * Reads the update view and follows `license://state`. Only call this in a beta
 * channel: the commands do not exist in the internal build.
 */
export function useLicenseView(): LicenseViewState {
  const [view, setView] = useState<LicenseView | null>(null);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    onLicenseState((next) => {
      if (!cancelled) setView(next);
    })
      .then((un) => {
        if (cancelled) un();
        else unlisten = un;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // Keeps reading until a view arrives, from this read or from `license://state`.
  const needsRead = view === null;
  useEffect(() => {
    if (!needsRead) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const read = async () => {
      try {
        const next = await licenseState();
        if (!cancelled) setView((current) => current ?? next);
      } catch {
        if (!cancelled) timer = setTimeout(read, READ_RETRY_MS);
      }
    };
    read();
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [needsRead]);

  return { view };
}
