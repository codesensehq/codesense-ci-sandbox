// SPDX-License-Identifier: MIT
import { create } from "zustand";

/**
 * SW-AV-5 — the imperative bridge from the keyboard-shortcut registry (and, SW-AV-6,
 * the header "N need you" count button) to the Activity board's "jump to the next thing
 * that needs you" cycle: the board PUBLISHES its bound `jumpToNextNeedsYou` closure
 * here on mount and the caller invokes whatever is published. When the board isn't
 * mounted the shortcut runs the terminal-tab "what needs you" jump instead.
 *
 * The closure lives on the board because the cycle needs the LIVE YOU-OWE rows and
 * the shipped `useAgentDrill` (both hook state), which the pure registry can't hold.
 * Each press advances a cursor over the drillable YOU-OWE rows and drills the next
 * one's agent through the exact same path a row click / → go takes.
 */
interface WeftJumpState {
  /** Advance to the next needs-you row and focus its agent. `null` unless the board
   *  is mounted (→ the hotkey falls back to the terminal-tab jump). */
  jumpToNextNeedsYou: (() => void) | null;
  publish: (fn: (() => void) | null) => void;
  clear: () => void;
}

export const useWeftJumpStore = create<WeftJumpState>((set) => ({
  jumpToNextNeedsYou: null,
  publish: (fn) => set({ jumpToNextNeedsYou: fn }),
  clear: () => set({ jumpToNextNeedsYou: null }),
}));
