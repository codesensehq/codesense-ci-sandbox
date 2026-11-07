// SPDX-License-Identifier: MIT
/**
 * Shared scroll-intent parser for keyboard shortcuts.
 *
 * Extracts the key-binding → scroll-action mapping used by both the
 * markdown viewer and the terminal. Each consumer maps the returned
 * intent to its own scroll backend (DOM scrollTo vs xterm scrollLines).
 */

export type ScrollIntent =
  | { action: "top" }
  | { action: "bottom" }
  | { action: "lines"; delta: number }
  | { action: "pages"; delta: number };

/**
 * Parse a keyboard event into a scroll intent.
 *
 * Returns `null` for non-scroll keys — including plain arrows,
 * Home/End, and any Shift-modified combo. Consumers handle null
 * with their own logic (e.g., markdown adds plain-arrow scrolling;
 * terminal passes through to the shell).
 *
 * Uses `metaKey || ctrlKey` for cross-platform Cmd/Ctrl support,
 * matching the markdown viewer's pattern.
 */
export function parseScrollIntent(e: KeyboardEvent): ScrollIntent | null {
  if (e.shiftKey) return null;

  const cmd = e.metaKey || e.ctrlKey;

  // Cmd/Ctrl+arrows — highest priority
  if (cmd && e.key === "ArrowUp") return { action: "top" };
  if (cmd && e.key === "ArrowDown") return { action: "bottom" };

  // Alt+arrows (only without cmd — Cmd+Alt+Up → "top", not "lines")
  if (!cmd && e.altKey && e.key === "ArrowUp") return { action: "lines", delta: -10 };
  if (!cmd && e.altKey && e.key === "ArrowDown") return { action: "lines", delta: 10 };

  // Page keys (only without cmd — Cmd+PageUp is not intercepted)
  if (!cmd && e.key === "PageUp") return { action: "pages", delta: -1 };
  if (!cmd && e.key === "PageDown") return { action: "pages", delta: 1 };

  return null;
}
