// SPDX-License-Identifier: MIT
import { create } from "zustand";

/**
 * SW-BSI-2 — the human Inbox's draft store. One text + revision per
 * `(signed-in user, team, conversation root)`. The revision is bumped on every edit
 * and captured by Send at click time, so a successful acknowledgement clears ONLY the
 * exact submitted revision — an in-flight send that overlaps a newer edit leaves the
 * newer text intact (UI-11). Selection changes NEVER clear a draft: A → B → A restores
 * A's text. Identical text in two threads lives in two separate keys and is cleared
 * independently (UI-03/UI-11 "no text-equality-only clearing").
 *
 * Request-id for retry idempotency is the caller's responsibility to compute as
 * `${userId}:${teamKey}:${conversationRootId}:${revision}`.
 */

/** The three identity axes of a draft. Any null → the draft is anonymous and ignored
 *  (the Inbox requires a signed-in user + a team to accept a Reply). */
export interface InboxDraftKey {
  userId: string;
  teamKey: string;
  conversationRootId: number;
}

/** One draft record. `revision` is per-draft, monotonic, starting at 1 for the first edit. */
export interface InboxDraft {
  text: string;
  revision: number;
}

interface State {
  drafts: Record<string, InboxDraft>;
}

interface Actions {
  /** Write/overwrite the draft at `key` with `text`. Returns the NEW revision. Setting
   *  the same text again still bumps the revision — a later edit-to-identical-text makes
   *  a distinct deliberate submission (per the UI contract's Round-4 seam correction). */
  setDraft: (key: InboxDraftKey, text: string) => number;
  /** The current draft at `key`, or `null`. */
  getDraft: (key: InboxDraftKey) => InboxDraft | null;
  /** Clear the draft at `key` ONLY if its current revision matches `revision`. A newer
   *  revision in the store means the user edited after Send captured — keep that newer
   *  text. Returns whether it cleared. */
  clearIfRevision: (key: InboxDraftKey, revision: number) => boolean;
  /** Debug / test — drop every draft. */
  _reset: () => void;
}

/** Internal: fold the three key axes into one stable string. `::` is a delimiter
 *  reserved by this module — team keys are `org:team` and userIds are plain numeric
 *  ids, so `::` can never appear in an input axis. */
export function draftStoreKey(key: InboxDraftKey): string {
  return `${key.userId}::${key.teamKey}::${key.conversationRootId}`;
}

export const useInboxDraftStore = create<State & Actions>((set, get) => ({
  drafts: {},
  setDraft: (key, text) => {
    const k = draftStoreKey(key);
    const prev = get().drafts[k];
    const revision = prev ? prev.revision + 1 : 1;
    set((s) => ({ drafts: { ...s.drafts, [k]: { text, revision } } }));
    return revision;
  },
  getDraft: (key) => get().drafts[draftStoreKey(key)] ?? null,
  clearIfRevision: (key, revision) => {
    const k = draftStoreKey(key);
    const current = get().drafts[k];
    if (!current || current.revision !== revision) return false;
    set((s) => {
      const next = { ...s.drafts };
      delete next[k];
      return { drafts: next };
    });
    return true;
  },
  _reset: () => set({ drafts: {} }),
}));

/** Request-id for retry idempotency — the caller passes this to the backend so an
 *  ambiguous timeout can be retried without duplicating a send (per UI contract AC5). */
export function inboxReplyRequestId(key: InboxDraftKey, revision: number): string {
  return `${key.userId}:${key.teamKey}:${key.conversationRootId}:${revision}`;
}
