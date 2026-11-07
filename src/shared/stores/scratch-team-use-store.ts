// SPDX-License-Identifier: MIT
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { create } from "zustand";

import { getScratchTeamUse, setScratchTeamUse } from "@/tauri/commands";
import { SCRATCH_USE_CHANGED_EVENT } from "@/tauri/events";
import { logger } from "@/shared/lib/logger";
import { safeUnlisten } from "@/shared/lib/tauri-listeners";

/**
 * SLW-PT-20 — a read-through cache of "Use my scratch items in ‹Team›".
 *
 * The truth is the backend switch stored in the user's scratch repo (read by
 * every fold: overlay, agents, routines). This
 * store only mirrors it for the UI, keyed `org:team`, and writes through the
 * Tauri command — nothing here is persisted in the browser.
 */
interface ScratchTeamUseState {
  /** `org:team` → on/off, once read. Absent = not read yet; a failed read is stored as off
   *  — the UI never claims "on" it could not confirm. */
  byTeam: Record<string, boolean>;
  /** Teams with a write in flight. */
  saving: Record<string, true>;
  load: (orgSlug: string, team: string) => Promise<void>;
  set: (orgSlug: string, team: string, on: boolean) => Promise<void>;
}

export function scratchTeamKey(orgSlug: string, team: string): string {
  return `${orgSlug}:${team}`;
}

export const useScratchTeamUseStore = create<ScratchTeamUseState>((set, get) => ({
  byTeam: {},
  saving: {},

  load: async (orgSlug, team) => {
    try {
      const on = await getScratchTeamUse(orgSlug, team);
      set((s) => ({ byTeam: { ...s.byTeam, [scratchTeamKey(orgSlug, team)]: on } }));
    } catch (err) {
      logger.warn("scratchTeamUse", `read failed for ${orgSlug}/${team} — ${err}`);
      set((s) => ({ byTeam: { ...s.byTeam, [scratchTeamKey(orgSlug, team)]: false } }));
    }
  },

  set: async (orgSlug, team, on) => {
    const key = scratchTeamKey(orgSlug, team);
    const before = get().byTeam[key];
    set((s) => ({ byTeam: { ...s.byTeam, [key]: on }, saving: { ...s.saving, [key]: true } }));
    try {
      await setScratchTeamUse(orgSlug, team, on);
    } catch (err) {
      // Roll back to what the backend still holds.
      set((s) => {
        const byTeam = { ...s.byTeam };
        if (before === undefined) delete byTeam[key];
        else byTeam[key] = before;
        return { byTeam };
      });
      throw err;
    } finally {
      set((s) => {
        const { [key]: _done, ...saving } = s.saving;
        return { saving };
      });
    }
  },
}));

/** The team's switch as last read: `true`/`false`, or `undefined` before the first read
 *  (a failed read reads `false`). Re-reads whenever any view changes a team's switch
 *  (`scratch-use-changed`). */
export function useScratchTeamUseState(
  orgSlug: string | null,
  team: string | null,
): boolean | undefined {
  const key = orgSlug && team ? scratchTeamKey(orgSlug, team) : null;
  const on = useScratchTeamUseStore((s) => (key ? s.byTeam[key] : undefined));
  const load = useScratchTeamUseStore((s) => s.load);
  useEffect(() => {
    if (!orgSlug || !team) return;
    void load(orgSlug, team);
    const unlisten = listen<{ orgSlug: string; team: string }>(SCRATCH_USE_CHANGED_EVENT, (e) => {
      if (e.payload.orgSlug === orgSlug && e.payload.team === team) void load(orgSlug, team);
    }).catch(() => null);
    return () => {
      void unlisten.then((off) => {
        if (off) safeUnlisten(off);
      });
    };
  }, [orgSlug, team, load]);
  return on;
}

/** Whether the user's scratch items apply in `orgSlug`/`team` — `true` only once the backend
 *  confirms it; unknown and a failed read both read off (never a claim of "on"). */
export function useScratchTeamUse(orgSlug: string | null, team: string | null): boolean {
  return useScratchTeamUseState(orgSlug, team) === true;
}
