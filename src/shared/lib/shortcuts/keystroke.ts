// SPDX-License-Identifier: MIT
/**
 * Keystroke parsing, matching, display, and reserved-key detection.
 *
 * Canonical format: `CmdOrCtrl+Shift+Alt+<key>` (segments joined by `+`,
 * modifier order is fixed). The `key` field matches `KeyboardEvent.key`
 * values — letters are uppercased, symbols kept as-is (`","`, `"\\"`),
 * named keys kept verbatim (`"Enter"`, `"ArrowUp"`).
 *
 * `CmdOrCtrl` is platform-aware: on macOS it maps to the `Cmd` (meta)
 * modifier; on other platforms to `Ctrl`. This matches Tauri's
 * accelerator convention, even though we do not use Tauri accelerators
 * in the menu (menu labels render without shortcut hints — see C.16
 * story for rationale).
 */

export interface KeystrokeParts {
  cmdOrCtrl: boolean;
  shift: boolean;
  alt: boolean;
  /** `KeyboardEvent.key` value; letters uppercased. */
  key: string;
}

/**
 * Map of symbol-key characters → their layout-independent `event.code`
 * values. macOS rewrites `event.key` for these symbols when Option is
 * held (e.g. `Opt+]` → `’`, `Opt+[` → `“`, `Opt+\` → `«`), so matching
 * by `event.key` alone misses any `Cmd+Opt+symbol` keystroke. The
 * matcher falls back to `event.code` for any expected key that has an
 * entry here. Letters and digits are covered separately by
 * `alnumCode`, but only for macOS ⌘⌥ chords (Option rewrites them too:
 * `Opt+1` → `¡`, `Opt+G` → `©`).
 */
const SYMBOL_KEY_TO_CODE: Record<string, string> = {
  "[": "BracketLeft",
  "]": "BracketRight",
  "{": "BracketLeft",
  "}": "BracketRight",
  "\\": "Backslash",
  "|": "Backslash",
  ";": "Semicolon",
  ":": "Semicolon",
  "'": "Quote",
  '"': "Quote",
  ",": "Comma",
  "<": "Comma",
  ".": "Period",
  ">": "Period",
  "/": "Slash",
  "?": "Slash",
  "`": "Backquote",
  "~": "Backquote",
  "=": "Equal",
  "+": "Equal",
  "-": "Minus",
  _: "Minus",
};

/**
 * Reverse of `SYMBOL_KEY_TO_CODE`: maps an `event.code` to the
 * unshifted canonical key character for that physical key. Used by
 * `keystrokeFromEvent` to recover the canonical key when macOS Option
 * rewrites the produced character. Only unshifted variants are listed
 * — shift state is preserved separately via `KeystrokeParts.shift`.
 */
const SYMBOL_CODE_TO_KEY: Record<string, string> = {
  BracketLeft: "[",
  BracketRight: "]",
  Backslash: "\\",
  Semicolon: ";",
  Quote: "'",
  Comma: ",",
  Period: ".",
  Slash: "/",
  Backquote: "`",
  Equal: "=",
  Minus: "-",
};

/**
 * The layout-independent `event.code` of a letter or digit key
 * (`"G"` → `"KeyG"`, `"1"` → `"Digit1"`), or undefined for any other key.
 */
function alnumCode(key: string): string | undefined {
  if (/^[A-Z]$/.test(key)) return `Key${key}`;
  if (/^[0-9]$/.test(key)) return `Digit${key}`;
  return undefined;
}

/** Reverse of `alnumCode`: the canonical letter/digit for a `KeyX` / `DigitN` code. */
function alnumFromCode(code: string): string | undefined {
  const m = /^(?:Key([A-Z])|Digit([0-9]))$/.exec(code);
  return m ? (m[1] ?? m[2]) : undefined;
}

/**
 * True when running on macOS. Evaluated at call time so tests can
 * override `navigator.platform` per-case (the const form would bake the
 * test-runner's platform in at module load).
 */
export function isMac(): boolean {
  return (
    typeof navigator !== "undefined" &&
    typeof navigator.platform === "string" &&
    /Mac/i.test(navigator.platform)
  );
}

/** Parse a canonical keystroke string into its parts. Throws on invalid input. */
export function parseKeystroke(str: string): KeystrokeParts {
  const segments = str.split("+").map((s) => s.trim());
  const parts: KeystrokeParts = { cmdOrCtrl: false, shift: false, alt: false, key: "" };
  let keySet = false;

  for (const seg of segments) {
    if (seg === "CmdOrCtrl") {
      parts.cmdOrCtrl = true;
    } else if (seg === "Shift") {
      parts.shift = true;
    } else if (seg === "Alt") {
      parts.alt = true;
    } else if (seg.length > 0) {
      if (keySet) {
        throw new Error(`Invalid keystroke "${str}": multiple non-modifier keys`);
      }
      parts.key = normalizeKey(seg);
      keySet = true;
    }
  }

  if (!keySet) {
    throw new Error(`Invalid keystroke "${str}": missing key`);
  }
  return parts;
}

/** Format parts back into canonical `CmdOrCtrl+Shift+Alt+X` form. */
export function formatKeystroke(parts: KeystrokeParts): string {
  const segments: string[] = [];
  if (parts.cmdOrCtrl) segments.push("CmdOrCtrl");
  if (parts.shift) segments.push("Shift");
  if (parts.alt) segments.push("Alt");
  segments.push(parts.key);
  return segments.join("+");
}

/**
 * Render a keystroke for human display. On macOS, compact glyph form
 * (`⌘⇧P`); on other platforms, `Ctrl+Shift+P`. An empty input returns
 * an empty string so callers that resolve from an unknown action id
 * (e.g., stale persisted state after a downgrade) don't crash.
 */
export function formatKeystrokeDisplay(str: string): string {
  if (!str) return "";
  const parts = parseKeystroke(str);
  const key = displayKey(parts.key);

  if (isMac()) {
    const mods = [parts.cmdOrCtrl ? "⌘" : "", parts.shift ? "⇧" : "", parts.alt ? "⌥" : ""].join(
      "",
    );
    return mods + key;
  }

  const mods: string[] = [];
  if (parts.cmdOrCtrl) mods.push("Ctrl");
  if (parts.shift) mods.push("Shift");
  if (parts.alt) mods.push("Alt");
  return [...mods, key].join("+");
}

/**
 * A keystroke as an `aria-keyshortcuts` value (`Meta+Shift+P` on macOS,
 * `Control+Shift+P` elsewhere), so assistive tech announces it apart from the
 * control's name. An empty input returns an empty string.
 */
export function ariaKeyshortcuts(str: string): string {
  if (!str) return "";
  const parts = parseKeystroke(str);
  const mods: string[] = [];
  if (parts.cmdOrCtrl) mods.push(isMac() ? "Meta" : "Control");
  if (parts.shift) mods.push("Shift");
  if (parts.alt) mods.push("Alt");
  return [...mods, parts.key].join("+");
}

/**
 * Build a canonical keystroke string from a `KeyboardEvent`. Used by the
 * settings capture UI when the user presses a key combination to bind.
 * Returns `null` if the event is a bare modifier press (no key yet).
 *
 * Symmetric with `matchesKeystroke`'s `event.code` fallback: when Alt
 * is held AND the rewritten character is not a symbol the matcher
 * already understands, fall back to the canonical key char derived
 * from `event.code` so the captured rebind round-trips. Without this,
 * rebinding to `Cmd+Opt+]` would write `CmdOrCtrl+Alt+’` to settings
 * and the matcher would never fire against the user's keystroke.
 */
export function keystrokeFromEvent(event: KeyboardEvent): string | null {
  if (
    event.key === "Control" ||
    event.key === "Meta" ||
    event.key === "Shift" ||
    event.key === "Alt"
  ) {
    return null;
  }
  const normalizedKey = normalizeKey(event.key);
  const key =
    event.altKey && SYMBOL_KEY_TO_CODE[normalizedKey] === undefined
      ? (SYMBOL_CODE_TO_KEY[event.code] ??
        (event.metaKey && isMac() ? alnumFromCode(event.code) : undefined) ??
        normalizedKey)
      : normalizedKey;
  const parts: KeystrokeParts = {
    cmdOrCtrl: isMac() ? event.metaKey : event.ctrlKey,
    shift: event.shiftKey,
    alt: event.altKey,
    key,
  };
  return formatKeystroke(parts);
}

export interface MatchOptions {
  /** On macOS, `CmdOrCtrl` means ⌘ only: a Control chord never matches. For keys
   *  claimed from a terminal, where ⌃-chords are shell input (⌃T, ⌃F, ⌃K…). */
  macCmdOnly?: boolean;
}

/** Whether a `KeyboardEvent` matches the parsed keystroke. */
export function matchesKeystroke(
  event: KeyboardEvent,
  keystroke: string,
  options: MatchOptions = {},
): boolean {
  let expected: KeystrokeParts;
  try {
    expected = parseKeystroke(keystroke);
  } catch {
    return false;
  }
  // `CmdOrCtrl` matches either Meta or Ctrl — Tauri / Electron's
  // `CommandOrControl` accelerator convention. A strict per-platform
  // check via `isMac()` breaks under Playwright (Desktop Chrome's UA
  // reports `Win32` regardless of host OS) and is also user-hostile
  // for users on remapped keyboards.
  const actualCmdOrCtrl = event.metaKey || event.ctrlKey;
  if (actualCmdOrCtrl !== expected.cmdOrCtrl) return false;
  if (options.macCmdOnly && expected.cmdOrCtrl && isMac() && (!event.metaKey || event.ctrlKey))
    return false;
  if (event.shiftKey !== expected.shift) return false;
  if (event.altKey !== expected.alt) return false;
  if (normalizeKey(event.key) === expected.key) return true;
  // Layout-independent fallback for symbol keys that macOS rewrites
  // under Option (Cmd+Opt+] etc).
  const expectedCode = SYMBOL_KEY_TO_CODE[expected.key];
  if (expectedCode !== undefined && event.code === expectedCode) return true;
  // macOS Option rewrites letters and digits as well (`Cmd+Opt+1` reports `¡`), so
  // a ⌘⌥ chord on one falls back to its physical key. Only there: elsewhere Ctrl+Alt
  // is AltGr, whose produced character is authoritative.
  const alnum = expected.alt && event.metaKey && isMac() ? alnumCode(expected.key) : undefined;
  return alnum !== undefined && event.code === alnum;
}

/** Reserved-key reason, surfaced by the settings UI inline error. */
export interface ReservedKeyReason {
  readonly message: string;
}

/**
 * Blocked only without additional modifiers (`⌘⇧Q` is NOT blocked).
 *
 * Platform-specific (macOS only): Q / M / H / Tab / Space — reserved by the OS.
 * Universal (all platforms): W (app-bound tab/Settings close), and
 * C / V / X / Z / A — clipboard + undo + select-all. These are so
 * near-universally expected that rebinding them is almost certainly
 * a mistake.
 *
 * `⌘F/S/P/N` are NOT blocked — they're legitimately contested across
 * apps and the user may want to override them.
 */
export function isReservedKeystroke(str: string): ReservedKeyReason | null {
  let parts: KeystrokeParts;
  try {
    parts = parseKeystroke(str);
  } catch {
    return null;
  }
  if (!parts.cmdOrCtrl || parts.shift || parts.alt) return null;

  const primary = isMac() ? "⌘" : "Ctrl+";
  const osReservedPrefix = isMac() ? "macOS reserves" : "reserved by the OS:";

  switch (parts.key) {
    case "Q":
      if (!isMac()) return null;
      return { message: `${osReservedPrefix} ${primary}Q for Quit` };
    case "M":
      if (!isMac()) return null;
      return { message: `${osReservedPrefix} ${primary}M for Minimize` };
    case "H":
      if (!isMac()) return null;
      return { message: `${osReservedPrefix} ${primary}H for Hide` };
    case "W":
      return { message: `${primary}W is used by the app to close tabs and Settings` };
    case "C":
      return { message: `${primary}C is reserved for Copy` };
    case "V":
      return { message: `${primary}V is reserved for Paste` };
    case "X":
      return { message: `${primary}X is reserved for Cut` };
    case "Z":
      return { message: `${primary}Z is reserved for Undo` };
    case "A":
      return { message: `${primary}A is reserved for Select All` };
    case "Tab":
      if (!isMac()) return null;
      return { message: `${osReservedPrefix} ${primary}Tab for switching apps` };
    case "Space":
      if (!isMac()) return null;
      return { message: `${osReservedPrefix} ${primary}Space for Spotlight` };
    default:
      return null;
  }
}

/** A chord a host keeps from dashboard commands, as a default and as a rebind. */
export interface ReservedChord {
  keystroke: string;
  /** Who keeps it: the OS or the app around the dashboard. */
  owner: "os" | "app";
  /** What the chord does there, as Settings names it. */
  reason: string;
}

/** The environment hosting dashboards; each keeps its own chords. */
export type DashboardHost = "desktop";

/** The chords each host keeps from dashboard commands. Data, so another host (a
 *  browser, say) brings its own list. Only the desktop app exists today. */
export const HOST_RESERVED_CHORDS: Record<DashboardHost, readonly ReservedChord[]> = {
  desktop: [
    { keystroke: "CmdOrCtrl+Q", owner: "os", reason: "Quit" },
    { keystroke: "CmdOrCtrl+W", owner: "app", reason: "Close Tab" },
    { keystroke: "CmdOrCtrl+H", owner: "os", reason: "Hide" },
    { keystroke: "CmdOrCtrl+M", owner: "os", reason: "Minimize" },
    { keystroke: "CmdOrCtrl+Tab", owner: "os", reason: "Switch Apps" },
    { keystroke: "CmdOrCtrl+Space", owner: "os", reason: "Spotlight" },
    { keystroke: "CmdOrCtrl+A", owner: "os", reason: "Select All" },
    { keystroke: "CmdOrCtrl+C", owner: "os", reason: "Copy" },
    { keystroke: "CmdOrCtrl+V", owner: "os", reason: "Paste" },
    { keystroke: "CmdOrCtrl+X", owner: "os", reason: "Cut" },
    { keystroke: "CmdOrCtrl+Z", owner: "os", reason: "Undo" },
  ],
};

/** The host dashboards run in now. */
export const ACTIVE_DASHBOARD_HOST: DashboardHost = "desktop";

/** Why the active host keeps `str` from dashboard commands, or null. */
export function isReservedForDashboardCommand(str: string): ReservedKeyReason | null {
  const chord = HOST_RESERVED_CHORDS[ACTIVE_DASHBOARD_HOST].find((c) => c.keystroke === str);
  if (!chord) return null;
  const owner = chord.owner === "app" ? "CodeSense" : isMac() ? "macOS" : "The system";
  return { message: `${owner} uses ${formatKeystrokeDisplay(str)} for ${chord.reason}` };
}

/** Uppercase single ASCII letters and name the space bar (`" "` can't survive the
 *  `+`-joined format); leave other keys as-is. */
function normalizeKey(key: string): string {
  if (key.length === 1 && /[a-z]/.test(key)) return key.toUpperCase();
  if (key === " ") return "Space";
  return key;
}

/** Render a single key for human display. */
function displayKey(key: string): string {
  const MAP: Record<string, string> = {
    Enter: "⏎",
    Escape: "Esc",
    Tab: "⇥",
    Backspace: "⌫",
    Delete: "⌦",
    ArrowUp: "↑",
    ArrowDown: "↓",
    ArrowLeft: "←",
    ArrowRight: "→",
  };
  return MAP[key] ?? key;
}
