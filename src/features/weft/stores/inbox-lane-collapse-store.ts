// SPDX-License-Identifier: MIT
import { create } from "zustand";

import type { InboxLaneId } from "../lib/inbox-model";

/**
 * SLW-TL-4 — sticky Inbox lane-collapse state. A lane the user collapses stays
 * collapsed across the panel's unmount/remount AND across reloads (§ AC: "lane
 * collapse state persists across remount; do not reset on remount"). Modeled on a
 * small localStorage-backed store rather than
 * component `useState`, whose value dies on unmount — a survivor of the F1-era board
 * where collapse "resets on panel unmount" (the exact bug this fixes).
 *
 * Only an EXPLICIT user toggle is stored. A lane the user has never touched reads
 * `undefined`, so the panel falls back to the lane's calming default (running starts
 * collapsed; needs-you + done start open).
 */
interface InboxLaneCollapseState {
  /** Explicit per-lane collapse — absent = never toggled (use the lane default). */
  collapsed: Partial<Record<InboxLaneId, boolean>>;
  /** Record an explicit toggle for a lane (persisted). */
  setCollapsed: (lane: InboxLaneId, collapsed: boolean) => void;
}

const STORAGE_KEY = "codesense.inbox-lane-collapse";

function load(): Partial<Record<InboxLaneId, boolean>> {
  try {
    const raw = globalThis.localStorage?.getItem(STORAGE_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as Partial<Record<InboxLaneId, boolean>>;
    const out: Partial<Record<InboxLaneId, boolean>> = {};
    for (const id of ["needs-you", "running", "done"] as const) {
      if (typeof parsed[id] === "boolean") out[id] = parsed[id];
    }
    return out;
  } catch {
    return {};
  }
}

function persist(collapsed: Partial<Record<InboxLaneId, boolean>>): void {
  try {
    globalThis.localStorage?.setItem(STORAGE_KEY, JSON.stringify(collapsed));
  } catch {
    // Best-effort — a private-mode / quota failure just loses stickiness.
  }
}

export const useInboxLaneCollapseStore = create<InboxLaneCollapseState>((set, get) => ({
  collapsed: load(),
  setCollapsed: (lane, collapsed) => {
    const next = { ...get().collapsed, [lane]: collapsed };
    set({ collapsed: next });
    persist(next);
  },
}));

/**
 * Resolve a lane's effective collapse: the user's explicit choice if any, else the
 * lane's calming default. Pure — safe to unit-test without a render.
 */
export function resolveLaneCollapsed(
  stored: Partial<Record<InboxLaneId, boolean>>,
  lane: InboxLaneId,
  defaultCollapsed: boolean,
): boolean {
  const explicit = stored[lane];
  return typeof explicit === "boolean" ? explicit : defaultCollapsed;
}
