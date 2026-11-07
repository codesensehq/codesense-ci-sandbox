// SPDX-License-Identifier: MIT
/**
 * Custom key event handler for xterm.js terminals.
 *
 * Intercepts specific key combinations before they reach the PTY:
 * - Shift+Enter → sends ESC + newline (\x1b\n) instead of \r (submit)
 *   This matches Ghostty's keybind behavior for multi-line input.
 *
 * Includes a key repeat polyfill for environments where the browser/OS
 * doesn't fire repeated keydown events (e.g., macOS WKWebView in Tauri
 * with ApplePressAndHoldEnabled). The polyfill auto-detects: if native
 * repeat events arrive, it stays dormant; if they don't, it takes over
 * after the initial delay.
 *
 * Usage:
 *   const keyHandler = createTerminalKeyHandler(writeFn, {
 *     isApplicationCursorMode: () => term.modes.applicationCursorKeysMode,
 *   });
 *   term.attachCustomKeyEventHandler(keyHandler.handler);
 *   // Wire keyHandler.clearRepeat to window blur / compositionstart for cleanup.
 */

import { parseScrollIntent } from "./scroll-keys";

// ---------------------------------------------------------------------------
// Key repeat polyfill constants
// ---------------------------------------------------------------------------

/** Delay before repeat starts (ms). Matches typical macOS default. */
export const REPEAT_DELAY = 500;
/** Interval between repeats (ms). ~30 Hz, matching typical OS repeat rate. */
export const REPEAT_INTERVAL = 33;
/** Number of consecutive native repeat events required before the polyfill
 *  yields to native repeat. Prevents WKWebView's sporadic single-event
 *  native repeat from killing the polyfill. */
export const NATIVE_REPEAT_THRESHOLD = 3;

// ---------------------------------------------------------------------------
// Key-to-sequence mapping for the polyfill
// ---------------------------------------------------------------------------

const MODIFIER_KEYS = new Set([
  "Shift",
  "Control",
  "Alt",
  "Meta",
  "CapsLock",
  "NumLock",
  "ScrollLock",
  "Fn",
  "Symbol",
  "Hyper",
  "Super",
]);

/** Arrow key → normal-mode suffix. Application cursor mode uses 'O' prefix instead of '['. */
const ARROW_KEY_SUFFIX: Record<string, string> = {
  ArrowUp: "A",
  ArrowDown: "B",
  ArrowRight: "C",
  ArrowLeft: "D",
};

const SPECIAL_KEY_SEQUENCES: Record<string, string> = {
  Backspace: "\x7f",
  Delete: "\x1b[3~",
  Home: "\x1b[H",
  End: "\x1b[F",
  PageUp: "\x1b[5~",
  PageDown: "\x1b[6~",
  Tab: "\t",
};

/**
 * Map a KeyboardEvent to the terminal data it should produce.
 * Returns null for modifier-only keys or unmappable combinations.
 *
 * Arrow keys are handled separately via `arrowKeySequence()` since they
 * depend on the terminal's application cursor mode.
 */
export function keyToSequence(
  event: KeyboardEvent,
  isApplicationCursorMode?: () => boolean,
): string | null {
  if (MODIFIER_KEYS.has(event.key)) return null;

  // Special keys (unmodified only — modified combos rely on native repeat)
  if (!event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey) {
    // Arrow keys — mode-aware
    const arrowSuffix = ARROW_KEY_SUFFIX[event.key];
    if (arrowSuffix) {
      const prefix = isApplicationCursorMode?.() ? "\x1bO" : "\x1b[";
      return prefix + arrowSuffix;
    }

    const seq = SPECIAL_KEY_SEQUENCES[event.key];
    if (seq) return seq;
  }

  // Printable characters (with or without shift — shift changes event.key)
  if (event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey) {
    return event.key;
  }

  // Not mappable by polyfill — rely on native repeat for these
  return null;
}

// ---------------------------------------------------------------------------
// Public interface
// ---------------------------------------------------------------------------

export interface TerminalKeyHandlerOptions {
  /** Returns true when the terminal is in application cursor keys mode (DECCKM). */
  isApplicationCursorMode?: () => boolean;
  /** Returns true when the terminal is in normal screen (not alternate/TUI). */
  isNormalScreen?: () => boolean;
  /** Scroll to the very top of the buffer. */
  scrollToTop?: () => void;
  /** Scroll to the very bottom of the buffer. */
  scrollToBottom?: () => void;
  /** Scroll by n lines (negative = up). */
  scrollLines?: (n: number) => void;
  /** Scroll by n pages (negative = up). */
  scrollPages?: (n: number) => void;
}

export interface TerminalKeyHandler {
  /** Pass to term.attachCustomKeyEventHandler(). */
  handler: (event: KeyboardEvent) => boolean;
  /** Clear any active repeat timers. Call on blur / compositionstart / dispose. */
  clearRepeat: () => void;
}

export function createTerminalKeyHandler(
  writeToPty: (data: string) => void,
  options?: TerminalKeyHandlerOptions,
): TerminalKeyHandler {
  // Shift+Enter rapid-press guard
  let pendingNewline = false;

  // Key repeat polyfill state
  let repeatCode: string | null = null;
  let repeatData: string | null = null;
  let delayTimer: ReturnType<typeof setTimeout> | null = null;
  let repeatTimer: ReturnType<typeof setInterval> | null = null;
  let nativeRepeatSeen = false;
  let nativeRepeatCount = 0;

  function clearRepeat(): void {
    if (delayTimer) {
      clearTimeout(delayTimer);
      delayTimer = null;
    }
    if (repeatTimer) {
      clearInterval(repeatTimer);
      repeatTimer = null;
    }
    repeatCode = null;
    repeatData = null;
    nativeRepeatSeen = false;
    nativeRepeatCount = 0;
  }

  function handler(event: KeyboardEvent): boolean {
    if (event.isComposing) return true;

    // ── keyup: clear repeat if it's the held key ──────────────────────
    if (event.type === "keyup") {
      if (repeatCode !== null && event.code === repeatCode) {
        clearRepeat();
      }
      return true;
    }

    // ── Scroll shortcuts (Cmd+Up/Down, Alt+Up/Down, PageUp/PageDown) ──
    if (event.type === "keydown") {
      const intent = parseScrollIntent(event);
      if (intent && options?.isNormalScreen?.()) {
        if (!event.repeat) clearRepeat();
        event.preventDefault();
        switch (intent.action) {
          case "top":
            options.scrollToTop?.();
            break;
          case "bottom":
            options.scrollToBottom?.();
            break;
          case "lines":
            options.scrollLines?.(intent.delta);
            break;
          case "pages":
            options.scrollPages?.(intent.delta);
            break;
        }
        return false;
      }
    }

    // ── Native repeat detected: yield to native only after threshold ──
    if (event.type === "keydown" && event.repeat) {
      // If polyfill is already running, suppress native events to avoid
      // double-emit. WKWebView may fire sporadic native repeats long after
      // the polyfill committed — those must not kill the running interval.
      if (repeatTimer) {
        return false;
      }
      nativeRepeatCount++;
      if (nativeRepeatCount >= NATIVE_REPEAT_THRESHOLD) {
        nativeRepeatSeen = true;
        if (delayTimer) {
          clearTimeout(delayTimer);
          delayTimer = null;
        }
      }
      return true;
    }

    // ── Shift+Enter → ESC + newline ──────────────────────────────────
    if (
      event.type === "keydown" &&
      event.key === "Enter" &&
      event.shiftKey &&
      !event.ctrlKey &&
      !event.metaKey &&
      !event.altKey
    ) {
      clearRepeat();
      if (pendingNewline) return false;

      pendingNewline = true;
      event.preventDefault();
      writeToPty("\x1b");
      setTimeout(() => {
        writeToPty("\n");
        pendingNewline = false;
      }, 5);
      return false;
    }

    // ── Cmd+Left/Right → Ctrl+A / Ctrl+E ─────────────────────────────
    if (
      event.type === "keydown" &&
      event.metaKey &&
      !event.shiftKey &&
      !event.altKey &&
      !event.ctrlKey &&
      (event.key === "ArrowLeft" || event.key === "ArrowRight")
    ) {
      clearRepeat();
      event.preventDefault();
      writeToPty(event.key === "ArrowLeft" ? "\x01" : "\x05");
      return false;
    }

    // ── Key repeat polyfill: start tracking on initial keydown ────────
    if (event.type === "keydown" && !event.repeat) {
      const data = keyToSequence(event, options?.isApplicationCursorMode);
      if (data !== null) {
        // New repeatable key — replace any existing repeat
        clearRepeat();
        repeatCode = event.code;
        repeatData = data;
        delayTimer = setTimeout(() => {
          delayTimer = null;
          if (repeatData !== null && !nativeRepeatSeen) {
            repeatTimer = setInterval(() => {
              if (repeatData !== null) writeToPty(repeatData);
            }, REPEAT_INTERVAL);
          }
        }, REPEAT_DELAY);
      } else if (MODIFIER_KEYS.has(event.key)) {
        // Modifier-only keys: don't touch repeat state (e.g., pressing
        // Shift while holding 'j' shouldn't kill the repeat).
      } else {
        // Non-repeatable, non-modifier key (e.g., Ctrl+c): cancel repeat
        clearRepeat();
      }
    }

    return true;
  }

  return { handler, clearRepeat };
}
