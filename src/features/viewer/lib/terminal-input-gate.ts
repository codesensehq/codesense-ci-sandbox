// SPDX-License-Identifier: MIT
/**
 * What may reach a terminal's shell from its xterm. Both input funnels go
 * through here: the custom key handler's writes (keydown) and xterm's own
 * `onData` (paste, IME composition, bracketed paste).
 *
 * - `locked`: the agent is driving the terminal; nothing the user types reaches
 *   it.
 * - `exited`: the shell is gone. Nothing is forwarded to the dead session, and
 *   nothing typed here starts a new one: a restart only comes from the ended bar
 *   (its Restart button, or Enter while the bar has focus).
 *
 * Returns whether `data` was forwarded.
 */
export function forwardTerminalInput(
  data: string,
  gate: { exited: boolean; locked: boolean },
  write: (data: string) => void,
): boolean {
  if (gate.exited || gate.locked) return false;
  write(data);
  return true;
}

export type ScrollAction = "page-up" | "page-down" | "line-up" | "line-down" | "top" | "bottom";

/** The keys that move around the scrollback of a pane whose shell is gone. */
const SCROLL_KEYS: Readonly<Record<string, ScrollAction>> = {
  "\x1b[5~": "page-up",
  "\x1b[6~": "page-down",
  "\x1b[A": "line-up",
  "\x1bOA": "line-up",
  "\x1b[B": "line-down",
  "\x1bOB": "line-down",
  "\x1b[H": "top",
  "\x1bOH": "top",
  "\x1b[1~": "top",
  "\x1b[F": "bottom",
  "\x1bOF": "bottom",
  "\x1b[4~": "bottom",
};

/** A key in a pane whose shell is gone: a scroll key scrolls; anything else
 *  goes back to the ended bar, which says how to go on. */
export function endedPaneKey(data: string): { scroll: ScrollAction } | "to-bar" {
  const scroll = SCROLL_KEYS[data];
  return scroll ? { scroll } : "to-bar";
}

export interface ScrollableTerminal {
  scrollPages(n: number): void;
  scrollLines(n: number): void;
  scrollToTop(): void;
  scrollToBottom(): void;
}

export function applyScroll(term: ScrollableTerminal, action: ScrollAction): void {
  if (action === "page-up") term.scrollPages(-1);
  else if (action === "page-down") term.scrollPages(1);
  else if (action === "line-up") term.scrollLines(-1);
  else if (action === "line-down") term.scrollLines(1);
  else if (action === "top") term.scrollToTop();
  else term.scrollToBottom();
}

/**
 * Route one chunk of terminal input: to the shell while it runs (unless
 * locked); once it is gone, scroll keys scroll the scrollback and any other key
 * hands keyboard focus back to the ended bar (never to a new shell). Copying
 * never reaches here (xterm handles the copy chord itself).
 */
export function routeTerminalInput(
  data: string,
  gate: { exited: boolean; locked: boolean },
  io: {
    write: (data: string) => void;
    term: ScrollableTerminal | null;
    toBar: () => void;
  },
): void {
  if (!gate.exited) {
    forwardTerminalInput(data, gate, io.write);
    return;
  }
  const key = endedPaneKey(data);
  if (key === "to-bar") io.toBar();
  else if (io.term) applyScroll(io.term, key.scroll);
}
