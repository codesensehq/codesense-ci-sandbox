// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";

import { detectTools, type Tool } from "@/tauri/commands";

/**
 * Session-level cache for detected external editors. `detect_tools` scans PATH
 * for editor binaries — a stable result for the app session — so run it ONCE and
 * share across every consumer (settings' EditorRow, the WDE-5 external-editor
 * button) instead of re-spawning the detect on each mount/overlay-open. A cached
 * promise also dedupes concurrent first-callers.
 */
let cachedEditors: Promise<Tool[]> | null = null;

function loadDetectedEditors(): Promise<Tool[]> {
  if (!cachedEditors) {
    cachedEditors = detectTools()
      .then((tools) => tools.editors)
      .catch((e) => {
        // Don't poison the cache on a failed detect — clear it so a later mount
        // can retry rather than being stuck with the rejection forever.
        cachedEditors = null;
        throw e;
      });
  }
  return cachedEditors;
}

/** Test-only: drop the session cache so a fresh detect runs on the next mount. */
export function resetDetectedEditorsCache(): void {
  cachedEditors = null;
}

/**
 * Detected external editors (`{id,label}`) for the current session, backed by a
 * module-level cached promise (detect runs once per session, not per mount).
 *
 * `enabled=false` skips the detect entirely and returns `[]` — for callers that
 * don't need the friendly-label map (no editor configured, or a custom command
 * whose label is the command string itself). This is the SLW-WDE-5 R1 gate:
 * the common unconfigured/custom paths never spawn the backend detect.
 *
 * `refresh=true` forces a fresh detect on every mount (bypassing the session
 * cache) — for CONFIG surfaces like the Settings editor picker, which mounts on
 * each open and must reflect a newly-installed editor rather than a stale cache.
 * It also repopulates the shared cache, so cheap (non-refresh) consumers still
 * benefit. Frequent, cosmetic consumers (the WDE-5 button) leave it `false`.
 */
export function useDetectedEditors(enabled = true, refresh = false): Tool[] {
  const [editors, setEditors] = useState<Tool[]>([]);

  useEffect(() => {
    if (!enabled) return;
    // Config surfaces opt into a fresh detect per open (see `refresh` above).
    if (refresh) resetDetectedEditorsCache();
    let cancelled = false;
    loadDetectedEditors()
      .then((eds) => {
        if (!cancelled) setEditors(eds);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [enabled, refresh]);

  return enabled ? editors : [];
}
