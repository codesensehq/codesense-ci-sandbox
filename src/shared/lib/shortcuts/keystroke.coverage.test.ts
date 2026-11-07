// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, afterEach } from "vitest";

import {
  ariaKeyshortcuts,
  formatKeystroke,
  formatKeystrokeDisplay,
  HOST_RESERVED_CHORDS,
  isReservedForDashboardCommand,
  isReservedKeystroke,
  keystrokeFromEvent,
  matchesKeystroke,
  parseKeystroke,
} from "./keystroke";

// ---------- Platform stubbing ----------

const originalPlatform = navigator.platform;

function setPlatform(value: string): void {
  Object.defineProperty(navigator, "platform", {
    value,
    configurable: true,
    writable: true,
  });
}

function restorePlatform(): void {
  Object.defineProperty(navigator, "platform", {
    value: originalPlatform,
    configurable: true,
    writable: true,
  });
}

afterEach(() => {
  restorePlatform();
});

// ---------- parseKeystroke ----------

describe("parseKeystroke", () => {
  it("parses a simple CmdOrCtrl+symbol combo", () => {
    expect(parseKeystroke("CmdOrCtrl+,")).toEqual({
      cmdOrCtrl: true,
      shift: false,
      alt: false,
      key: ",",
    });
  });

  it("parses CmdOrCtrl+Shift+letter and uppercases the letter", () => {
    expect(parseKeystroke("CmdOrCtrl+Shift+p")).toEqual({
      cmdOrCtrl: true,
      shift: true,
      alt: false,
      key: "P",
    });
  });

  it("parses CmdOrCtrl+Alt+letter", () => {
    expect(parseKeystroke("CmdOrCtrl+Alt+X")).toEqual({
      cmdOrCtrl: true,
      shift: false,
      alt: true,
      key: "X",
    });
  });

  it("parses a backslash key", () => {
    expect(parseKeystroke("CmdOrCtrl+\\")).toEqual({
      cmdOrCtrl: true,
      shift: false,
      alt: false,
      key: "\\",
    });
  });

  it("parses a named key (Enter)", () => {
    expect(parseKeystroke("CmdOrCtrl+Enter")).toEqual({
      cmdOrCtrl: true,
      shift: false,
      alt: false,
      key: "Enter",
    });
  });

  it("parses a bare non-modifier key (no modifiers)", () => {
    expect(parseKeystroke("Enter")).toEqual({
      cmdOrCtrl: false,
      shift: false,
      alt: false,
      key: "Enter",
    });
  });

  it("throws on missing key (only modifiers)", () => {
    expect(() => parseKeystroke("CmdOrCtrl+Shift")).toThrow(/missing key/);
  });

  it("throws on multiple non-modifier keys", () => {
    expect(() => parseKeystroke("CmdOrCtrl+A+B")).toThrow(/multiple non-modifier/);
  });
});

// ---------- formatKeystroke ----------

describe("formatKeystroke", () => {
  it("round-trips a full combo", () => {
    const input = "CmdOrCtrl+Shift+Alt+P";
    expect(formatKeystroke(parseKeystroke(input))).toBe(input);
  });

  it("emits segments in fixed order (CmdOrCtrl, Shift, Alt, key)", () => {
    // Feed parts out-of-order-conceptually; format enforces the order
    const out = formatKeystroke({ cmdOrCtrl: true, shift: true, alt: true, key: "P" });
    expect(out).toBe("CmdOrCtrl+Shift+Alt+P");
  });

  it("omits modifiers that aren't pressed", () => {
    expect(formatKeystroke({ cmdOrCtrl: true, shift: false, alt: false, key: "," })).toBe(
      "CmdOrCtrl+,",
    );
  });
});

// ---------- formatKeystrokeDisplay ----------

describe("formatKeystrokeDisplay", () => {
  describe("on macOS", () => {
    beforeEach(() => setPlatform("MacIntel"));

    it("uses compact glyph form for CmdOrCtrl (⌘)", () => {
      expect(formatKeystrokeDisplay("CmdOrCtrl+,")).toBe("⌘,");
    });

    it("uses ⌘⇧ for CmdOrCtrl+Shift", () => {
      expect(formatKeystrokeDisplay("CmdOrCtrl+Shift+P")).toBe("⌘⇧P");
    });

    it("uses ⌘⇧⌥ for CmdOrCtrl+Shift+Alt", () => {
      expect(formatKeystrokeDisplay("CmdOrCtrl+Shift+Alt+P")).toBe("⌘⇧⌥P");
    });

    it("displays Enter as ⏎", () => {
      expect(formatKeystrokeDisplay("CmdOrCtrl+Enter")).toBe("⌘⏎");
    });

    it("displays Escape as Esc", () => {
      expect(formatKeystrokeDisplay("CmdOrCtrl+Escape")).toBe("⌘Esc");
    });
  });

  describe("on non-Mac", () => {
    beforeEach(() => setPlatform("Linux x86_64"));

    it("uses Ctrl+ prefix for CmdOrCtrl", () => {
      expect(formatKeystrokeDisplay("CmdOrCtrl+,")).toBe("Ctrl+,");
    });

    it("joins modifiers with +", () => {
      expect(formatKeystrokeDisplay("CmdOrCtrl+Shift+P")).toBe("Ctrl+Shift+P");
    });
  });
});

// ---------- keystrokeFromEvent ----------

describe("ariaKeyshortcuts", () => {
  it("names the modifiers as aria-keyshortcuts does, per platform", () => {
    setPlatform("MacIntel");
    expect(ariaKeyshortcuts("CmdOrCtrl+Shift+T")).toBe("Meta+Shift+T");
    setPlatform("Win32");
    expect(ariaKeyshortcuts("CmdOrCtrl+Alt+D")).toBe("Control+Alt+D");
    expect(ariaKeyshortcuts("F2")).toBe("F2");
    expect(ariaKeyshortcuts("")).toBe("");
  });
});

describe("keystrokeFromEvent", () => {
  beforeEach(() => setPlatform("MacIntel"));

  it("returns null for a bare modifier press (no key chosen yet)", () => {
    expect(keystrokeFromEvent(new KeyboardEvent("keydown", { key: "Meta" }))).toBeNull();
    expect(keystrokeFromEvent(new KeyboardEvent("keydown", { key: "Shift" }))).toBeNull();
    expect(keystrokeFromEvent(new KeyboardEvent("keydown", { key: "Control" }))).toBeNull();
    expect(keystrokeFromEvent(new KeyboardEvent("keydown", { key: "Alt" }))).toBeNull();
  });

  it("builds CmdOrCtrl+Shift+P on macOS from a ⌘⇧P keydown", () => {
    const ev = new KeyboardEvent("keydown", {
      key: "p",
      metaKey: true,
      shiftKey: true,
    });
    expect(keystrokeFromEvent(ev)).toBe("CmdOrCtrl+Shift+P");
  });

  it("builds CmdOrCtrl+, on macOS from ⌘,", () => {
    const ev = new KeyboardEvent("keydown", { key: ",", metaKey: true });
    expect(keystrokeFromEvent(ev)).toBe("CmdOrCtrl+,");
  });

  it("uses ctrlKey (not metaKey) as CmdOrCtrl on non-Mac", () => {
    setPlatform("Linux x86_64");
    const ev = new KeyboardEvent("keydown", { key: ",", ctrlKey: true });
    expect(keystrokeFromEvent(ev)).toBe("CmdOrCtrl+,");
    const ev2 = new KeyboardEvent("keydown", { key: ",", metaKey: true });
    expect(keystrokeFromEvent(ev2)).toBe(",");
  });

  it("round-trips Cmd+Opt+] through capture → matcher on macOS", () => {
    // When the Settings UI captures a rebind of Cmd+Opt+], macOS rewrites
    // event.key to '’'. Without the code-fallback in keystrokeFromEvent,
    // the persisted shortcut would be "CmdOrCtrl+Alt+’" — which then
    // never matches the user's keystroke. Symmetric with matchesKeystroke.
    const captureEvent = new KeyboardEvent("keydown", {
      key: "’",
      code: "BracketRight",
      metaKey: true,
      altKey: true,
    });
    const captured = keystrokeFromEvent(captureEvent);
    expect(captured).toBe("CmdOrCtrl+Alt+]");

    // And the captured string then matches a fresh keydown of the same combo.
    const replayEvent = new KeyboardEvent("keydown", {
      key: "’",
      code: "BracketRight",
      metaKey: true,
      altKey: true,
    });
    expect(matchesKeystroke(replayEvent, captured!)).toBe(true);
  });

  it("matches Cmd+Opt+digit / letter by the physical key when Option rewrites the character", () => {
    // macOS: ⌥2 → "™", ⌥G → "©".
    setPlatform("MacIntel");
    const digit = new KeyboardEvent("keydown", {
      key: "™",
      code: "Digit2",
      metaKey: true,
      altKey: true,
    });
    expect(matchesKeystroke(digit, "CmdOrCtrl+Alt+2")).toBe(true);
    expect(matchesKeystroke(digit, "CmdOrCtrl+Alt+3")).toBe(false);
    const letter = new KeyboardEvent("keydown", {
      key: "©",
      code: "KeyG",
      metaKey: true,
      altKey: true,
    });
    expect(matchesKeystroke(letter, "CmdOrCtrl+Alt+G")).toBe(true);
  });

  it("the digit / letter code fallback applies to Alt chords only", () => {
    // Without Option the produced character is authoritative: ⌘2 must stay ⌘2.
    const ev = new KeyboardEvent("keydown", { key: "2", code: "Digit2", metaKey: true });
    expect(matchesKeystroke(ev, "CmdOrCtrl+Alt+2")).toBe(false);
    expect(matchesKeystroke(ev, "CmdOrCtrl+2")).toBe(true);
  });

  it("the digit / letter physical-key fallback is macOS ⌘⌥ only (Ctrl+Alt elsewhere is AltGr)", () => {
    setPlatform("Win32");
    const altGr = new KeyboardEvent("keydown", {
      key: "@",
      code: "KeyQ",
      ctrlKey: true,
      altKey: true,
    });
    expect(matchesKeystroke(altGr, "CmdOrCtrl+Alt+Q")).toBe(false);
    setPlatform("MacIntel");
    const ctrlOpt = new KeyboardEvent("keydown", {
      key: "œ",
      code: "KeyQ",
      ctrlKey: true,
      altKey: true,
    });
    expect(matchesKeystroke(ctrlOpt, "CmdOrCtrl+Alt+Q")).toBe(false);
  });

  it("captures Cmd+Opt+digit as the digit, so a rebind round-trips", () => {
    setPlatform("MacIntel");
    const ev = new KeyboardEvent("keydown", {
      key: "¡",
      code: "Digit1",
      metaKey: true,
      altKey: true,
    });
    const captured = keystrokeFromEvent(ev);
    expect(captured).toBe("CmdOrCtrl+Alt+1");
    expect(matchesKeystroke(ev, captured!)).toBe(true);
  });

  it("does not rewrite event.key when Alt is not held (preserves Cmd+Shift+] unchanged)", () => {
    // Code-fallback in keystrokeFromEvent only kicks in under altKey.
    // A regular Cmd+Shift+] keydown should produce "CmdOrCtrl+Shift+]"
    // straight from event.key — no code lookup.
    const ev = new KeyboardEvent("keydown", {
      key: "}",
      code: "BracketRight",
      metaKey: true,
      shiftKey: true,
    });
    expect(keystrokeFromEvent(ev)).toBe("CmdOrCtrl+Shift+}");
  });
});

// ---------- matchesKeystroke ----------

describe("matchesKeystroke", () => {
  beforeEach(() => setPlatform("MacIntel"));

  it("matches when all modifiers + key align", () => {
    const ev = new KeyboardEvent("keydown", { key: ",", metaKey: true });
    expect(matchesKeystroke(ev, "CmdOrCtrl+,")).toBe(true);
  });

  it("does not match when CmdOrCtrl expected but not pressed", () => {
    const ev = new KeyboardEvent("keydown", { key: "," });
    expect(matchesKeystroke(ev, "CmdOrCtrl+,")).toBe(false);
  });

  it("does not match when extra modifier pressed", () => {
    const ev = new KeyboardEvent("keydown", { key: ",", metaKey: true, shiftKey: true });
    expect(matchesKeystroke(ev, "CmdOrCtrl+,")).toBe(false);
  });

  it("normalizes letter case — event 'p' matches 'CmdOrCtrl+Shift+P'", () => {
    const ev = new KeyboardEvent("keydown", { key: "p", metaKey: true, shiftKey: true });
    expect(matchesKeystroke(ev, "CmdOrCtrl+Shift+P")).toBe(true);
  });

  it("returns false for an invalid keystroke string (does not throw)", () => {
    const ev = new KeyboardEvent("keydown", { key: "p", metaKey: true });
    expect(matchesKeystroke(ev, "not a valid string")).toBe(false);
  });

  it("falls back to event.code for symbol keys rewritten by macOS Option (Cmd+Opt+])", () => {
    // macOS Option+] produces `’` instead of `]` in event.key. Without
    // the code-fallback the workspace-cycle shortcut from story 30.10
    // never fires on US-layout Macs. Issue surfaced during the 30.10
    // demo.
    const ev = new KeyboardEvent("keydown", {
      key: "’",
      code: "BracketRight",
      metaKey: true,
      altKey: true,
    });
    expect(matchesKeystroke(ev, "CmdOrCtrl+Alt+]")).toBe(true);
  });

  it("falls back to event.code for Cmd+Opt+[", () => {
    const ev = new KeyboardEvent("keydown", {
      key: "“",
      code: "BracketLeft",
      metaKey: true,
      altKey: true,
    });
    expect(matchesKeystroke(ev, "CmdOrCtrl+Alt+[")).toBe(true);
  });

  it("code-fallback does NOT widen non-symbol keystrokes", () => {
    // Letter keystrokes must still require event.key to match — a code
    // fallback for them would let any "KeyA" keydown match "CmdOrCtrl+A"
    // regardless of the produced character (e.g. dead-key composition).
    const ev = new KeyboardEvent("keydown", {
      key: "x",
      code: "KeyA",
      metaKey: true,
    });
    expect(matchesKeystroke(ev, "CmdOrCtrl+A")).toBe(false);
  });

  it("code-fallback does NOT fire when modifiers disagree", () => {
    // A bracket keystroke without Cmd should not match "CmdOrCtrl+Alt+]"
    // just because event.code === "BracketRight".
    const ev = new KeyboardEvent("keydown", {
      key: "]",
      code: "BracketRight",
      altKey: true,
    });
    expect(matchesKeystroke(ev, "CmdOrCtrl+Alt+]")).toBe(false);
  });

  it("macCmdOnly: on macOS a Control chord never matches CmdOrCtrl; ⌘ does", () => {
    setPlatform("MacIntel");
    const ctrlT = new KeyboardEvent("keydown", { key: "t", ctrlKey: true });
    const cmdT = new KeyboardEvent("keydown", { key: "t", metaKey: true });
    const both = new KeyboardEvent("keydown", { key: "t", metaKey: true, ctrlKey: true });
    expect(matchesKeystroke(ctrlT, "CmdOrCtrl+T", { macCmdOnly: true })).toBe(false);
    expect(matchesKeystroke(both, "CmdOrCtrl+T", { macCmdOnly: true })).toBe(false);
    expect(matchesKeystroke(cmdT, "CmdOrCtrl+T", { macCmdOnly: true })).toBe(true);
    // Without the option the Tauri convention stands.
    expect(matchesKeystroke(ctrlT, "CmdOrCtrl+T")).toBe(true);
  });

  it("macCmdOnly leaves Ctrl as the primary modifier off macOS", () => {
    setPlatform("Win32");
    const ctrlT = new KeyboardEvent("keydown", { key: "t", ctrlKey: true });
    expect(matchesKeystroke(ctrlT, "CmdOrCtrl+T", { macCmdOnly: true })).toBe(true);
  });

  it("treats CmdOrCtrl as Meta OR Ctrl on any platform", () => {
    // Match semantics are intentionally platform-agnostic (Tauri /
    // Electron's `CommandOrControl` convention). A strict isMac()
    // gate breaks under Playwright (Desktop Chrome's UA reports
    // Win32 regardless of host OS) and is user-hostile on remapped
    // keyboards. Capture UI still displays per-platform glyphs.
    setPlatform("Linux x86_64");
    const evCtrl = new KeyboardEvent("keydown", { key: ",", ctrlKey: true });
    expect(matchesKeystroke(evCtrl, "CmdOrCtrl+,")).toBe(true);
    const evMeta = new KeyboardEvent("keydown", { key: ",", metaKey: true });
    expect(matchesKeystroke(evMeta, "CmdOrCtrl+,")).toBe(true);

    setPlatform("MacIntel");
    expect(matchesKeystroke(evCtrl, "CmdOrCtrl+,")).toBe(true);
    expect(matchesKeystroke(evMeta, "CmdOrCtrl+,")).toBe(true);

    // When no modifier is expected, neither Meta nor Ctrl may be present.
    const evBare = new KeyboardEvent("keydown", { key: "," });
    expect(matchesKeystroke(evBare, "CmdOrCtrl+,")).toBe(false);
  });
});

// ---------- isReservedKeystroke ----------

describe("isReservedKeystroke", () => {
  describe("on macOS", () => {
    beforeEach(() => setPlatform("MacIntel"));

    it.each([
      ["CmdOrCtrl+Q", /Quit/],
      ["CmdOrCtrl+W", /close tabs/],
      ["CmdOrCtrl+M", /Minimize/],
      ["CmdOrCtrl+H", /Hide/],
      ["CmdOrCtrl+C", /Copy/],
      ["CmdOrCtrl+V", /Paste/],
      ["CmdOrCtrl+X", /Cut/],
      ["CmdOrCtrl+Z", /Undo/],
      ["CmdOrCtrl+A", /Select All/],
    ])("blocks %s", (str, matcher) => {
      const reason = isReservedKeystroke(str);
      expect(reason).not.toBeNull();
      expect(reason!.message).toMatch(matcher);
    });

    it("renders the ⌘ glyph in the error message", () => {
      expect(isReservedKeystroke("CmdOrCtrl+C")!.message).toMatch(/⌘C/);
    });
  });

  describe("on Windows/Linux", () => {
    beforeEach(() => setPlatform("Win32"));

    it.each([
      ["CmdOrCtrl+W", /close tabs/],
      ["CmdOrCtrl+C", /Copy/],
      ["CmdOrCtrl+V", /Paste/],
      ["CmdOrCtrl+X", /Cut/],
      ["CmdOrCtrl+Z", /Undo/],
      ["CmdOrCtrl+A", /Select All/],
    ])("blocks %s with cross-platform messaging", (str, matcher) => {
      const reason = isReservedKeystroke(str);
      expect(reason).not.toBeNull();
      expect(reason!.message).toMatch(matcher);
      expect(reason!.message).not.toMatch(/macOS/);
      expect(reason!.message).toMatch(/Ctrl\+/);
    });

    it.each(["CmdOrCtrl+Q", "CmdOrCtrl+M", "CmdOrCtrl+H"])(
      "does NOT block %s (macOS-only)",
      (str) => {
        expect(isReservedKeystroke(str)).toBeNull();
      },
    );
  });

  it("allows CmdOrCtrl+Shift+Q (additional modifier escapes the reserved set)", () => {
    expect(isReservedKeystroke("CmdOrCtrl+Shift+Q")).toBeNull();
  });

  it("allows CmdOrCtrl+Alt+W", () => {
    expect(isReservedKeystroke("CmdOrCtrl+Alt+W")).toBeNull();
  });

  it("allows CmdOrCtrl+Shift+C (additional modifier escapes Copy)", () => {
    expect(isReservedKeystroke("CmdOrCtrl+Shift+C")).toBeNull();
  });

  it("allows CmdOrCtrl+P (not a reserved letter)", () => {
    expect(isReservedKeystroke("CmdOrCtrl+P")).toBeNull();
  });

  it("allows CmdOrCtrl+F (legitimately contested, left unblocked)", () => {
    expect(isReservedKeystroke("CmdOrCtrl+F")).toBeNull();
  });

  it("allows CmdOrCtrl+, (Settings default)", () => {
    expect(isReservedKeystroke("CmdOrCtrl+,")).toBeNull();
  });

  it("returns null (not throws) for an invalid keystroke string", () => {
    expect(isReservedKeystroke("invalid")).toBeNull();
  });

  it("does not block bare Q (no CmdOrCtrl)", () => {
    expect(isReservedKeystroke("Q")).toBeNull();
  });
});

describe("the space bar and the dashboard-reserved chords", () => {
  it("names the space bar so it survives the +-joined format", () => {
    const e = new KeyboardEvent("keydown", { key: " ", code: "Space", metaKey: true });
    expect(keystrokeFromEvent(e)).toBe("CmdOrCtrl+Space");
    expect(matchesKeystroke(e, "CmdOrCtrl+Space")).toBe(true);
    setPlatform("MacIntel");
    expect(formatKeystrokeDisplay("CmdOrCtrl+Space")).toBe("⌘Space");
  });

  it("reserves ⌘Tab and ⌘Space on macOS only", () => {
    setPlatform("MacIntel");
    expect(isReservedKeystroke("CmdOrCtrl+Tab")?.message).toBe(
      "macOS reserves ⌘Tab for switching apps",
    );
    expect(isReservedKeystroke("CmdOrCtrl+Space")?.message).toBe(
      "macOS reserves ⌘Space for Spotlight",
    );
    expect(isReservedKeystroke("CmdOrCtrl+Alt+Tab")).toBeNull();
    setPlatform("Win32");
    expect(isReservedKeystroke("CmdOrCtrl+Tab")).toBeNull();
  });
});

describe("the chords a dashboard host keeps", () => {
  it("reads the active host's list, with who keeps each chord and why", () => {
    setPlatform("MacIntel");
    expect(HOST_RESERVED_CHORDS.desktop.map((c) => c.keystroke)).toEqual(
      ["Q", "W", "H", "M", "Tab", "Space", "A", "C", "V", "X", "Z"].map((k) => `CmdOrCtrl+${k}`),
    );
    expect(isReservedForDashboardCommand("CmdOrCtrl+C")?.message).toBe("macOS uses ⌘C for Copy");
    expect(isReservedForDashboardCommand("CmdOrCtrl+W")?.message).toBe(
      "CodeSense uses ⌘W for Close Tab",
    );
    // Anything off the list is the dashboard's to declare, unlike the built-ins' wider set.
    expect(isReservedForDashboardCommand("CmdOrCtrl+Shift+C")).toBeNull();
    expect(isReservedKeystroke("CmdOrCtrl+Q")).not.toBeNull();
  });
});
