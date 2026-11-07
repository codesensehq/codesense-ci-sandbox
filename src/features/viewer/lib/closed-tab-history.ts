// SPDX-License-Identifier: MIT
import type { TerminalTab } from "@/features/viewer/lib/cli-workspace/store/viewer-access";

/**
 * The terminal tabs most recently taken off screen, newest last — what "Reopen
 * Closed Tab" brings back. A Hidden tab is still running, so reopening it restores
 * the same live session. An Ended tab's process is gone, so reopening it opens a
 * fresh shell in the same workspace (and so the same cwd), under the same name —
 * or, when an agent ran in it, a plain shell name.
 *
 * In-memory only: nothing here survives a restart, and nothing needs to — Hidden
 * tabs are listed in the tray regardless, and an Ended tab has nothing to resume.
 */

export type ClosedTabKind = "hidden" | "ended";

export interface ClosedTabEntry {
  kind: ClosedTabKind;
  teamKey: string;
  workspaceId: string;
  tab: TerminalTab;
  /** An agent ran in it. Its name named the agent, so an Ended one reopens as a
   *  plainly named shell. */
  wasAgent?: boolean;
  /** It was its tile's last tab, so ending it opened a fresh shell in its place. */
  wasLastTab?: boolean;
}

/** How many closes are remembered. */
export const CLOSED_TAB_HISTORY_LIMIT = 20;

let entries: ClosedTabEntry[] = [];

/** Remember a tab that was just Hidden or Ended. */
export function recordClosedTab(entry: ClosedTabEntry): void {
  // A tab closed twice (Hidden, then Ended from the tray) keeps only its latest close.
  entries = entries.filter((e) => e.tab.id !== entry.tab.id);
  entries.push(entry);
  if (entries.length > CLOSED_TAB_HISTORY_LIMIT) entries.shift();
}

/** Take the most recent close for `teamKey` off the history. */
export function popClosedTab(teamKey: string): ClosedTabEntry | undefined {
  for (let i = entries.length - 1; i >= 0; i--) {
    if (entries[i].teamKey === teamKey) return entries.splice(i, 1)[0];
  }
  return undefined;
}

/** Test-only reset. */
export function resetClosedTabHistoryForTests(): void {
  entries = [];
}
