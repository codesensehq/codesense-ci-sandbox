// SPDX-License-Identifier: MIT
/**
 * What a new terminal needs to know before its pane exists. A tab is opened
 * through its terminal host (spawned + placed) BEFORE its pane mounts, so the
 * spawn runs at the last size a pane measured (120×32 until one has), and the
 * pane learns that the session it shows is one this viewer just opened (any
 * other session is being restored).
 */

export interface TerminalSize {
  cols: number;
  rows: number;
}

/** The spawn size before any pane has measured one. */
export const FALLBACK_TERMINAL_SIZE: TerminalSize = Object.freeze({ cols: 120, rows: 32 });

let measured: TerminalSize | null = null;

/** A pane fitted to `cols`×`rows`: new terminals spawn at this size. */
export function rememberTerminalSize(cols: number, rows: number): void {
  if (cols > 0 && rows > 0) measured = { cols, rows };
}

/** The size a new terminal spawns at. */
export function terminalSpawnSize(): TerminalSize {
  return measured ?? FALLBACK_TERMINAL_SIZE;
}

const freshOpens = new Set<string>();

/** `sessionId` is being opened fresh by this viewer: its pane starts a new
 *  terminal rather than restoring one that ran before. */
export function noteFreshOpen(sessionId: string): void {
  freshOpens.add(sessionId);
}

/** The open of `sessionId` did not happen. */
export function forgetFreshOpen(sessionId: string): void {
  freshOpens.delete(sessionId);
}

/** Whether this viewer opened `sessionId` fresh (else its pane restores it). */
export function isFreshOpen(sessionId: string): boolean {
  return freshOpens.has(sessionId);
}
