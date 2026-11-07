// SPDX-License-Identifier: MIT
/**
 * The terminal options that change how bytes are parsed into cells, shared by
 * the display (`@xterm/xterm`) and the backend emulator worker
 * (`@xterm/headless`). Both sides must build their terminals from this one
 * module: a width table or line-feed mode that differs between them would make
 * the backend's screen differ from the one the user saw.
 *
 * Imports nothing outside `@xterm/*` (no DOM, no React) so the emulator worker
 * can bundle it as is.
 *
 * `windowsMode` and `convertEol` are left at xterm's defaults on both sides, so
 * they are not set here.
 */
import { Unicode11Addon } from "@xterm/addon-unicode11";

/** Constructor options that affect parsing. `allowProposedApi` is what makes
 *  `term.unicode` (and with it the Unicode 11 width table) available. */
export const TERMINAL_PARSER_OPTIONS = { allowProposedApi: true } as const;

/** The Unicode version whose character widths both sides use. */
export const TERMINAL_UNICODE_VERSION = "11";

/** The surface of a display or headless terminal the parser setup touches. The
 *  two packages type their addons against their own `Terminal`, so this is
 *  structural. */
export interface ParserConfigurableTerminal {
  loadAddon(addon: never): void;
  readonly unicode: { activeVersion: string };
}

/** Load the Unicode 11 width table into `term` and make it the active one. Call
 *  before the first write, on the display and in the worker alike. */
export function activateTerminalParser(term: ParserConfigurableTerminal): void {
  term.loadAddon(new Unicode11Addon() as never);
  term.unicode.activeVersion = TERMINAL_UNICODE_VERSION;
}
