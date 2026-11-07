// SPDX-License-Identifier: MIT
import { describe, it, expect, afterEach } from "vitest";
import {
  ROW_POPUP_ATTR,
  TERMINAL_POPUP_ATTR,
  getActivePaneType,
  isCmdW,
  isRowPopupFocused,
  isTerminalFocused,
  isTerminalPopupFocused,
} from "./focus-utils";

describe("focus-utils", () => {
  afterEach(() => {
    document.body.innerHTML = "";
  });

  describe("getActivePaneType", () => {
    it("returns 'terminal' when active element is inside .xterm", () => {
      document.body.innerHTML = '<div class="xterm"><textarea></textarea></div>';
      const textarea = document.querySelector("textarea")!;
      textarea.focus();
      expect(getActivePaneType()).toBe("terminal");
    });

    it("returns 'scratch-note' when active element is inside .scratch-note-editor", () => {
      document.body.innerHTML = '<div class="scratch-note-editor"><textarea></textarea></div>';
      document.querySelector("textarea")!.focus();
      expect(getActivePaneType()).toBe("scratch-note");
    });

    it("returns 'file' when active element is not inside .xterm or .scratch-note-editor", () => {
      document.body.innerHTML = "<div><input /></div>";
      const input = document.querySelector("input")!;
      input.focus();
      expect(getActivePaneType()).toBe("file");
    });

    it("returns 'file' when no element is focused", () => {
      expect(getActivePaneType()).toBe("file");
    });

    it("a terminal popup's xterm is no CLI terminal", () => {
      document.body.innerHTML = `<div ${TERMINAL_POPUP_ATTR}><div class="xterm"><textarea></textarea></div></div>`;
      document.querySelector("textarea")!.focus();
      expect(getActivePaneType()).toBe("file");
      expect(isTerminalFocused()).toBe(false);
      expect(isTerminalPopupFocused()).toBe(true);
    });
  });

  describe("isTerminalPopupFocused", () => {
    it("is false for a CLI terminal and with nothing focused", () => {
      expect(isTerminalPopupFocused()).toBe(false);
      document.body.innerHTML = '<div class="xterm"><textarea></textarea></div>';
      document.querySelector("textarea")!.focus();
      expect(isTerminalPopupFocused()).toBe(false);
    });
  });

  describe("isTerminalFocused", () => {
    it("returns true when terminal is focused", () => {
      document.body.innerHTML = '<div class="xterm"><textarea></textarea></div>';
      document.querySelector("textarea")!.focus();
      expect(isTerminalFocused()).toBe(true);
    });

    it("returns false when terminal is not focused", () => {
      expect(isTerminalFocused()).toBe(false);
    });
  });

  describe("isRowPopupFocused", () => {
    it("is true with focus anywhere in a row popup, false elsewhere", () => {
      expect(isRowPopupFocused()).toBe(false);
      document.body.innerHTML = `<div ${ROW_POPUP_ATTR}><textarea></textarea></div><input />`;
      document.querySelector("textarea")!.focus();
      expect(isRowPopupFocused()).toBe(true);
      document.querySelector("input")!.focus();
      expect(isRowPopupFocused()).toBe(false);
    });
  });

  describe("isCmdW", () => {
    const press = (init: Partial<KeyboardEventInit>) =>
      isCmdW(new KeyboardEvent("keydown", { metaKey: true, ...init }));

    it.each([
      ["QWERTY ⌘W", { key: "w", code: "KeyW" }, true],
      ["Dvorak ⌘W: its w sits on the comma's place", { key: "w", code: "KeyComma" }, true],
      ["Dvorak ⌘, on W's place is Settings, not ⌘W", { key: ",", code: "KeyW" }, false],
      ["AZERTY ⌘W: its w sits on Z's place", { key: "w", code: "KeyZ" }, true],
      ["AZERTY ⌘Z on W's place is undo, not ⌘W", { key: "z", code: "KeyW" }, false],
      ["a Cyrillic layout's ⌘W: ц on W's place, no w anywhere", { key: "ц", code: "KeyW" }, true],
      ["a Greek layout's ⌘W: ς on W's place", { key: "ς", code: "KeyW" }, true],
      ["Lithuanian ž on W's place is its own letter, not ⌘W", { key: "ž", code: "KeyW" }, false],
      ["a Latin letter with a mark on W's place is not ⌘W", { key: "é", code: "KeyW" }, false],
      ["a Cyrillic letter on another key is not ⌘W", { key: "ц", code: "KeyQ" }, false],
      ["⇧⌘W", { key: "W", code: "KeyW", shiftKey: true }, false],
      ["⌥⌘W", { key: "∑", code: "KeyW", altKey: true }, false],
      ["⌃⌘W", { key: "w", code: "KeyW", ctrlKey: true }, false],
    ])("%s", (_name, init, expected) => {
      expect(press(init)).toBe(expected);
    });

    it("needs ⌘", () => {
      expect(isCmdW(new KeyboardEvent("keydown", { key: "w", code: "KeyW" }))).toBe(false);
    });

    it("reads an event with no key as no ⌘W, without throwing", () => {
      const noKey = { metaKey: true, ctrlKey: false, altKey: false, shiftKey: false, code: "KeyW" };
      expect(isCmdW({ ...noKey, key: undefined as unknown as string })).toBe(false);
    });
  });
});
