// SPDX-License-Identifier: MIT
/** The slice of viewer state that decides whether the terminal column (the CLI
 *  panel with its agents list) is on screen. One setting for every team (D39). */
export interface TerminalColumnState {
  /** The store's `LayoutMode` ("console" | "shared" | "viewer"), kept structural so this
   *  seam file needs no store import. */
  layoutMode: string | null;
  terminalVisible: boolean;
}

/**
 * Whether the terminal column is on screen: always in full-screen CLI,
 * never in the collapsed ("viewer") layout, otherwise as `terminalVisible` says.
 * The one predicate behind "the CLI is closed" — the Agents control, the agents
 * list and the bar's reopen all read it, so they can't disagree.
 */
export function isTerminalColumnShown(s: TerminalColumnState, hasTeam = true): boolean {
  if (!hasTeam) return false;
  if (s.layoutMode === "console") return true;
  if (s.layoutMode === "viewer") return false;
  return s.terminalVisible;
}

/**
 * Whether the terminal shows as a split below the viewer: the terminal is
 * visible and the layout is not full-screen CLI (which has no split). The workbench
 * sizes the viewer and the terminal section by this predicate, so anything laid out
 * against the split agrees with what is on screen.
 */
export function isTerminalSplitShown(s: TerminalColumnState, hasTeam = true): boolean {
  return !isConsoleLayout(s, hasTeam) && isTerminalColumnShown(s, hasTeam);
}

/** Full-screen CLI needs a shown team; saved chrome alone cannot hide the empty-state ladder. */
export function isConsoleLayout(
  s: Pick<TerminalColumnState, "layoutMode">,
  hasTeam = true,
): boolean {
  return hasTeam && s.layoutMode === "console";
}
