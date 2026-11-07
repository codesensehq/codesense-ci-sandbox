// SPDX-License-Identifier: MIT
import { useEffect, useMemo, useState } from "react";

import type { ConversationMessage } from "../api";
import { arrivalNote, msgKey, newArrivals, unseenBelow, type Directory, type Self } from "../model";

/** What a list holds, cheaply: it only ever grows at the end or is trimmed at the start. */
const shapeOf = (messages: readonly ConversationMessage[]): string => {
  const last = messages.at(-1);
  return `${messages.length}:${last ? msgKey(last) : ""}`;
};

/** The newest time a list shows. */
const newestOf = (messages: readonly ConversationMessage[]): number =>
  messages.at(-1)?.createdAtMs ?? -Infinity;

/**
 * Mail landing within this long of the first arrival is announced as one note, so a burst of
 * refreshes reads "5 new messages" once instead of five times.
 */
export const ARRIVAL_COALESCE_MS = 1500;

/**
 * How many lines someone else sent below a reader who scrolled up (`away`): counted from the
 * moment they left the bottom, cleared when they are back, or when `resetKey` changes (another
 * conversation, another filter).
 */
export function useUnseenBelow(
  messages: readonly ConversationMessage[],
  self: Self,
  away: boolean,
  resetKey: unknown,
): number {
  const [seen, setSeen] = useState<{
    away: boolean;
    key: unknown;
    keys: ReadonlySet<string> | null;
    since: number;
  }>({ away, key: resetKey, keys: null, since: -Infinity });
  // Snapshot what was there as the reader leaves the bottom: set while rendering, not after.
  if (seen.away !== away || seen.key !== resetKey) {
    setSeen({
      away,
      key: resetKey,
      keys: away ? new Set(messages.map(msgKey)) : null,
      since: newestOf(messages),
    });
  }
  const current = seen.away === away && seen.key === resetKey;
  const keys = current ? seen.keys : null;
  const since = current ? seen.since : -Infinity;
  return useMemo(() => unseenBelow(messages, keys, self, since), [messages, keys, self, since]);
}

interface ArrivalState {
  key: unknown;
  shape: string;
  keys: ReadonlySet<string>;
  since: number;
  /** Arrivals waiting for the coalescing window to close. */
  pending: ConversationMessage[];
  note: string;
  /** Notes announced so far: a repeat of the same note is still a change to the region. */
  n: number;
}

/**
 * The text for a polite live region when someone else's mail lands: "New message from …" or
 * "N new messages", coalesced over {@link ARRIVAL_COALESCE_MS}. Nothing on the first draw, for
 * older history filled in, or when `resetKey` changes.
 */
export function useArrivalNote(
  messages: readonly ConversationMessage[],
  self: Self,
  dir: Directory,
  resetKey: unknown,
): string {
  const shape = shapeOf(messages);
  // Seeded with what is there on mount (the lenses mount once the inbox has loaded), so the
  // first draw announces nothing, while a first message into an empty inbox is news.
  const [st, setSt] = useState<ArrivalState>(() => ({
    key: resetKey,
    shape,
    keys: new Set(messages.map(msgKey)),
    since: newestOf(messages),
    pending: [],
    note: "",
    n: 0,
  }));
  if (st.key !== resetKey || st.shape !== shape) {
    const fresh = st.key !== resetKey;
    const arrived = fresh ? [] : newArrivals(st.keys, messages, self, st.since);
    setSt({
      ...st,
      key: resetKey,
      shape,
      keys: new Set(messages.map(msgKey)),
      since: newestOf(messages),
      pending: fresh ? [] : [...st.pending, ...arrived],
      note: fresh ? "" : st.note,
    });
  }

  // The window opens with the first pending arrival and closes once, whatever lands meanwhile.
  const waiting = st.pending.length > 0;
  useEffect(() => {
    if (!waiting) return;
    const t = setTimeout(() => {
      setSt((s) =>
        s.pending.length ? { ...s, pending: [], note: arrivalNote(s.pending, dir), n: s.n + 1 } : s,
      );
    }, ARRIVAL_COALESCE_MS);
    return () => clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- the window is not reset by `dir`
  }, [waiting]);

  return st.note && st.n % 2 === 0 ? `${st.note}\u200b` : st.note;
}
