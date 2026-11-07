// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";

import * as commands from "@/tauri/commands";
import { logger } from "@/shared/lib/logger";

/**
 * SW-TP CAP-11 / CAP-13: lazy resolver for the per-tab toolbar's cwd slot.
 * Calls `session_cwd_cmd` once when the hook mounts (or `sessionId` changes)
 * and once per `revision` bump; returns the display path, or `null` when the
 * host doesn't know yet.
 *
 * SW-VE-14 wires the bump: pair with {@link useSessionCwdRevision}, which
 * subscribes to the backend's `pty-osc7` event (OSC 7 cwd mutations, deduped
 * at the PTY batcher) and bumps a per-session counter on every real `cd`.
 * The hook stays lazy — it never schedules its own timer.
 */
export function useSessionCwd(sessionId: string | undefined, revision = 0): string | null {
  const [cwd, setCwd] = useState<string | null>(null);
  useEffect(() => {
    // Reset to null on sessionId change so the previous session's cwd never
    // flashes under a new identity while the next fetch is in flight. The
    // caption falls back to tab.name in that transition window. This is a
    // deliberate state transition on identity-swap, not a cascade.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setCwd(null);
    if (!sessionId) return;
    // Resolve the invoke wrapper defensively: vitest's module mock throws on
    // access to a key the mock omits (TypeError with a direct property read).
    // Many trees that render the caption mock @/tauri/commands without this
    // export, so swallow the throw and leave the cwd at its initial null.
    let fn: ((id: string) => Promise<string | null>) | undefined;
    try {
      fn = (commands as { sessionCwd?: (id: string) => Promise<string | null> }).sessionCwd;
    } catch {
      return;
    }
    if (typeof fn !== "function") return;
    let cancelled = false;
    fn(sessionId)
      .then((result) => {
        if (cancelled) return;
        setCwd(result);
      })
      .catch((err) => {
        if (cancelled) return;
        logger.warn("useSessionCwd", `resolve failed for ${sessionId} — ${err}`);
        setCwd(null);
      });
    return () => {
      cancelled = true;
    };
  }, [sessionId, revision]);
  // When the hook isn't bound to a session, surface null directly so stale cwd
  // from a prior session never leaks into the current render.
  return sessionId ? cwd : null;
}
