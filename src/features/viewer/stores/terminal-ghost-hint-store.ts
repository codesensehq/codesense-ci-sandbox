// SPDX-License-Identifier: MIT
import { create } from "zustand";

/**
 * SLW-TL-7 — ghosted placeholder hints for FRESH agent terminals, keyed by session id.
 * When the Inbox's "Open an agent" control opens a new terminal it registers a hint here
 * ("Tell the agent what to do…"); the terminal pane reads its own session's hint and paints
 * a ghost overlay so the empty prompt reads as an invitation, not a void (north-star §8
 * rule 11). The first keystroke clears it. Kept OUT of the terminal-layout engine (the tab
 * open path carries only name/role), so it is fully additive — a terminal with no hint is
 * wholly unaffected.
 */
interface TerminalGhostHintState {
  hints: Record<string, string>;
  /** Register the ghost hint for a fresh session. */
  set: (sessionId: string, hint: string) => void;
  /** Clear the hint (first keystroke, or the tab closing). */
  clear: (sessionId: string) => void;
}

export const useTerminalGhostHintStore = create<TerminalGhostHintState>((set) => ({
  hints: {},
  set: (sessionId, hint) => set((s) => ({ hints: { ...s.hints, [sessionId]: hint } })),
  clear: (sessionId) =>
    set((s) => {
      if (!(sessionId in s.hints)) return s;
      const next = { ...s.hints };
      delete next[sessionId];
      return { hints: next };
    }),
}));

/** Scalar selector for one session's ghost hint (re-renders the pane only when its own
 *  hint appears/clears). `undefined` = no hint (the common case). */
export const selectGhostHint = (sessionId: string) => (s: TerminalGhostHintState) =>
  s.hints[sessionId];
