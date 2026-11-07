// SPDX-License-Identifier: MIT
import { create } from "zustand";

/**
 * SW-IB1-A5 — the shared compose box's draft slice. The sibling
 * `inbox-draft-store` carries the human-inbox reply flow's three-axis keys
 * with revision semantics for retry idempotency (SW-BSI-2); the shared
 * `InboxCompose` primitive instead takes a single opaque `draftKey: string`
 * from its mount site (push pane uses `${tabId}:${sessionId}`, human inbox
 * uses `${conversationId}` or `${newDraftId}`), so a separate flat slice
 * keeps the two surfaces from stepping on each other and lets the primitive
 * stay a pure render + text management component.
 *
 * Blur-persists; hydrates on mount; clears on successful send or user
 * discard. Switching tabs preserves the per-target draft — a tab A ⇆ B ⇆ A
 * trip restores A's text verbatim (same contract as the sibling store's
 * selection-persistence rule).
 */

export interface InboxComposeDraft {
  text: string;
  updatedAt: number;
}

interface State {
  drafts: Record<string, InboxComposeDraft>;
}

interface Actions {
  /** Write/overwrite the draft at `draftKey`. Empty-string `text` is a signal
   *  the slot is intentionally cleared — the record is removed. */
  setDraft: (draftKey: string, text: string) => void;
  /** Current draft at `draftKey`, or `null`. */
  getDraft: (draftKey: string) => InboxComposeDraft | null;
  /** Remove the draft at `draftKey`. No-op if absent. */
  clearDraft: (draftKey: string) => void;
  /** Debug / test — drop every draft. */
  _reset: () => void;
}

export const useInboxComposeDraftStore = create<State & Actions>((set, get) => ({
  drafts: {},
  setDraft: (draftKey, text) => {
    if (text === "") {
      // Empty is a clear — mirror the "no record" view of absence so a reload
      // doesn't hydrate an empty textarea placeholder-over-nothing.
      set((s) => {
        if (!(draftKey in s.drafts)) return s;
        const next = { ...s.drafts };
        delete next[draftKey];
        return { drafts: next };
      });
      return;
    }
    set((s) => ({
      drafts: { ...s.drafts, [draftKey]: { text, updatedAt: Date.now() } },
    }));
  },
  getDraft: (draftKey) => get().drafts[draftKey] ?? null,
  clearDraft: (draftKey) => {
    set((s) => {
      if (!(draftKey in s.drafts)) return s;
      const next = { ...s.drafts };
      delete next[draftKey];
      return { drafts: next };
    });
  },
  _reset: () => set({ drafts: {} }),
}));
