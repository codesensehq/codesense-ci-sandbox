// SPDX-License-Identifier: MIT
/**
 * The terminal display's fixed numbers, shared by the TerminalManager, its
 * slots and the settings UI. The theme palette lives apart, in
 * `features/viewer/components/terminal-constants.ts`.
 */

/**
 * Lines of history a new terminal keeps. Every session's buffer is held twice
 * (its always-alive display and the backend emulator) and live displays are
 * never trimmed, so the default stays at 5,000: a full 120-column session is
 * about 7 MiB per side. A value the user saved is kept.
 */
export const DEFAULT_SCROLLBACK = 5_000;

/** Hard ceiling on any terminal's history. The "Maximum" setting (`0`)
 *  resolves here, and explicit values are clamped to it. */
export const MAX_SCROLLBACK = 100_000;

/**
 * The history length for a new terminal from the saved setting:
 * - never set, or not a finite number → {@link DEFAULT_SCROLLBACK}
 * - `0` ("Maximum") → {@link MAX_SCROLLBACK}
 * - negative (a corrupted file) → {@link DEFAULT_SCROLLBACK}
 * - otherwise floored and clamped to `[1, MAX_SCROLLBACK]`
 */
export function scrollbackLines(setting: number | null | undefined): number {
  if (setting == null || !Number.isFinite(setting) || setting < 0) return DEFAULT_SCROLLBACK;
  if (setting === 0) return MAX_SCROLLBACK;
  return Math.max(1, Math.min(Math.floor(setting), MAX_SCROLLBACK));
}

/** The terminal web font's primary family. The `@font-face` in `src/index.css`
 *  uses the same name; keep the two in sync. */
export const TERMINAL_FONT_FAMILY = "MesloLGS NF";

/** ⌥ is sent to the shell as Meta, so readline word motion (⌥←/→, ⌥⌫) works
 *  like a native terminal, at the cost of ⌥-composed characters. */
export const DEFAULT_MAC_OPTION_IS_META = true;

/** Wheel multiplier: one notch travels a native-feeling distance. */
export const TERMINAL_SCROLL_SENSITIVITY = 2;
/** A short scroll glide, like iTerm2 / Terminal.app, kept brief so large
 *  streams stay responsive. */
export const TERMINAL_SMOOTH_SCROLL_DURATION_MS = 80;

/**
 * Past either limit, the least recently seen hidden displays are released
 * (their sessions keep running in the host) and rebuilt from the host snapshot
 * when shown again. Shown and focused displays are never released.
 */
export const EVICT_MAX_DISPLAYS = 64;
export const EVICT_MAX_BYTES = 512 * 1024 * 1024;
/** What xterm keeps per buffer cell, for the estimate `cols × lines × 12`. */
export const BUFFER_BYTES_PER_CELL = 12;
