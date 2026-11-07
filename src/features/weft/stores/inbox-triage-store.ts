// SPDX-License-Identifier: MIT
import { create } from "zustand";

/**
 * SLW-TL-7 — the Inbox's client-side triage state for housekeeping actions
 * (Dismiss/Snooze, Reprioritize) that the human takes with their own hands from the
 * row's ONE explicit overflow. It is a per-viewer view preference, not a durable board
 * mutation (the board is the system of record; the Inbox SHOWS, it does not CONTROL — §6):
 *  - **Dismiss/Snooze** removes an item from the viewer's Inbox view (a `restore` undo
 *    brings it back). A future arrival of the same item re-posts under a fresh key.
 *  - **Reprioritize** pins an item to the TOP of its lane so the thing you care about
 *    leads, without reordering anyone else.
 *
 * A module-singleton zustand store, so the state survives the panel's unmount/remount
 * within the session (mirrors the lane-collapse store's stickiness intent).
 */
interface InboxTriageState {
  /** Item keys the viewer has dismissed/snoozed out of their Inbox view. */
  dismissed: Record<string, boolean>;
  /** Item keys the viewer has pinned to the top of their lane. */
  pinned: Record<string, boolean>;
  isDismissed: (key: string) => boolean;
  isPinned: (key: string) => boolean;
  dismiss: (key: string) => void;
  restore: (key: string) => void;
  pin: (key: string) => void;
  unpin: (key: string) => void;
}

export const useInboxTriageStore = create<InboxTriageState>((set, get) => ({
  dismissed: {},
  pinned: {},
  isDismissed: (key) => get().dismissed[key] === true,
  isPinned: (key) => get().pinned[key] === true,
  dismiss: (key) => set((s) => ({ dismissed: { ...s.dismissed, [key]: true } })),
  restore: (key) =>
    set((s) => {
      const next = { ...s.dismissed };
      delete next[key];
      return { dismissed: next };
    }),
  pin: (key) => set((s) => ({ pinned: { ...s.pinned, [key]: true } })),
  unpin: (key) =>
    set((s) => {
      const next = { ...s.pinned };
      delete next[key];
      return { pinned: next };
    }),
}));

/** Snapshot of the triage sets — what {@link applyTriage} reads (pure, testable). */
export interface TriageSnapshot {
  dismissed: Record<string, boolean>;
  pinned: Record<string, boolean>;
}

/**
 * Apply the viewer's triage to an ordered list: drop dismissed items, then float pinned
 * items to the front (stable — everything else keeps its relative order). Pure.
 */
export function applyTriage<T>(
  items: T[],
  keyOf: (item: T) => string,
  triage: TriageSnapshot,
): T[] {
  const kept = items.filter((it) => triage.dismissed[keyOf(it)] !== true);
  const pinned = kept.filter((it) => triage.pinned[keyOf(it)] === true);
  const rest = kept.filter((it) => triage.pinned[keyOf(it)] !== true);
  return [...pinned, ...rest];
}
