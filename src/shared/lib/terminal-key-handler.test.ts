// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import {
  createTerminalKeyHandler,
  keyToSequence,
  REPEAT_DELAY,
  REPEAT_INTERVAL,
  NATIVE_REPEAT_THRESHOLD,
} from "./terminal-key-handler";

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

// ---------------------------------------------------------------------------
// Helper: create a KeyboardEvent with sensible defaults
// ---------------------------------------------------------------------------
function makeKey(
  type: "keydown" | "keyup",
  overrides: Partial<KeyboardEventInit> & { key: string; code?: string },
): KeyboardEvent {
  const code = overrides.code ?? `Key${overrides.key.toUpperCase()}`;
  return new KeyboardEvent(type, { ...overrides, code });
}

// ═══════════════════════════════════════════════════════════════════════════
// keyToSequence unit tests
// ═══════════════════════════════════════════════════════════════════════════

describe("keyToSequence", () => {
  it("returns the character for printable keys", () => {
    const event = makeKey("keydown", { key: "j" });
    expect(keyToSequence(event)).toBe("j");
  });

  it("returns shifted character for Shift+key", () => {
    const event = makeKey("keydown", { key: "J", shiftKey: true });
    expect(keyToSequence(event)).toBe("J");
  });

  it("returns normal-mode escape sequence for arrow keys", () => {
    expect(keyToSequence(makeKey("keydown", { key: "ArrowUp", code: "ArrowUp" }))).toBe("\x1b[A");
    expect(keyToSequence(makeKey("keydown", { key: "ArrowDown", code: "ArrowDown" }))).toBe(
      "\x1b[B",
    );
    expect(keyToSequence(makeKey("keydown", { key: "ArrowRight", code: "ArrowRight" }))).toBe(
      "\x1b[C",
    );
    expect(keyToSequence(makeKey("keydown", { key: "ArrowLeft", code: "ArrowLeft" }))).toBe(
      "\x1b[D",
    );
  });

  it("returns application-mode sequence for arrow keys when mode callback returns true", () => {
    const appMode = () => true;
    expect(keyToSequence(makeKey("keydown", { key: "ArrowUp", code: "ArrowUp" }), appMode)).toBe(
      "\x1bOA",
    );
    expect(
      keyToSequence(makeKey("keydown", { key: "ArrowDown", code: "ArrowDown" }), appMode),
    ).toBe("\x1bOB");
    expect(
      keyToSequence(makeKey("keydown", { key: "ArrowRight", code: "ArrowRight" }), appMode),
    ).toBe("\x1bOC");
    expect(
      keyToSequence(makeKey("keydown", { key: "ArrowLeft", code: "ArrowLeft" }), appMode),
    ).toBe("\x1bOD");
  });

  it("returns null for Shift+Arrow (modified special key)", () => {
    expect(
      keyToSequence(makeKey("keydown", { key: "ArrowLeft", code: "ArrowLeft", shiftKey: true })),
    ).toBeNull();
  });

  it("returns correct sequence for Backspace and Delete", () => {
    expect(keyToSequence(makeKey("keydown", { key: "Backspace", code: "Backspace" }))).toBe("\x7f");
    expect(keyToSequence(makeKey("keydown", { key: "Delete", code: "Delete" }))).toBe("\x1b[3~");
  });

  it("returns Tab sequence for Tab key", () => {
    expect(keyToSequence(makeKey("keydown", { key: "Tab", code: "Tab" }))).toBe("\t");
  });

  it("returns null for modifier-only keys", () => {
    expect(keyToSequence(makeKey("keydown", { key: "Shift", code: "ShiftLeft" }))).toBeNull();
    expect(keyToSequence(makeKey("keydown", { key: "Control", code: "ControlLeft" }))).toBeNull();
    expect(keyToSequence(makeKey("keydown", { key: "Alt", code: "AltLeft" }))).toBeNull();
    expect(keyToSequence(makeKey("keydown", { key: "Meta", code: "MetaLeft" }))).toBeNull();
    expect(keyToSequence(makeKey("keydown", { key: "CapsLock", code: "CapsLock" }))).toBeNull();
  });

  it("returns null for Ctrl+key combinations", () => {
    expect(keyToSequence(makeKey("keydown", { key: "c", ctrlKey: true }))).toBeNull();
  });

  it("returns null for Alt+key combinations", () => {
    expect(keyToSequence(makeKey("keydown", { key: "b", altKey: true }))).toBeNull();
  });

  it("returns null for Cmd+key combinations", () => {
    expect(keyToSequence(makeKey("keydown", { key: "a", metaKey: true }))).toBeNull();
  });
});

// ═══════════════════════════════════════════════════════════════════════════
// Existing custom key handler tests (Shift+Enter, Cmd+Arrow)
// ═══════════════════════════════════════════════════════════════════════════

describe("createTerminalKeyHandler", () => {
  it("intercepts Shift+Enter and writes ESC then newline to PTY", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    const event = new KeyboardEvent("keydown", {
      key: "Enter",
      shiftKey: true,
    });

    const result = handler(event);

    expect(result).toBe(false);
    // ESC sent immediately
    expect(writeToPty).toHaveBeenCalledWith("\x1b");

    // Newline sent after delay
    vi.advanceTimersByTime(10);
    expect(writeToPty).toHaveBeenCalledWith("\n");
    expect(writeToPty).toHaveBeenCalledTimes(2);
  });

  it("does NOT intercept plain Enter (no shift)", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    const event = new KeyboardEvent("keydown", {
      key: "Enter",
      shiftKey: false,
    });

    const result = handler(event);

    expect(result).toBe(true);
    expect(writeToPty).not.toHaveBeenCalled();
  });

  it("does NOT intercept other Shift+key combinations", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    const event = new KeyboardEvent("keydown", {
      key: "a",
      shiftKey: true,
    });

    const result = handler(event);

    expect(result).toBe(true);
    expect(writeToPty).not.toHaveBeenCalled();
  });

  it("does NOT intercept Ctrl+Shift+Enter", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    const event = new KeyboardEvent("keydown", {
      key: "Enter",
      shiftKey: true,
      ctrlKey: true,
    });

    const result = handler(event);

    expect(result).toBe(true);
    expect(writeToPty).not.toHaveBeenCalled();
  });

  it("does NOT intercept Cmd+Shift+Enter", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    const event = new KeyboardEvent("keydown", {
      key: "Enter",
      shiftKey: true,
      metaKey: true,
    });

    const result = handler(event);

    expect(result).toBe(true);
    expect(writeToPty).not.toHaveBeenCalled();
  });

  it("passes through during IME composition", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    const event = new KeyboardEvent("keydown", {
      key: "Enter",
      shiftKey: true,
    });
    Object.defineProperty(event, "isComposing", { value: true });

    const result = handler(event);

    expect(result).toBe(true);
    expect(writeToPty).not.toHaveBeenCalled();
  });

  it("blocks rapid Shift+Enter until pending newline completes", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    const makeEvent = () => new KeyboardEvent("keydown", { key: "Enter", shiftKey: true });

    // First press — should work
    handler(makeEvent());
    expect(writeToPty).toHaveBeenCalledWith("\x1b");

    // Second press within 5ms — should be suppressed
    const result = handler(makeEvent());
    expect(result).toBe(false);
    expect(writeToPty).toHaveBeenCalledTimes(1); // Only the first ESC

    // After timer fires, newline sent and guard cleared
    vi.advanceTimersByTime(10);
    expect(writeToPty).toHaveBeenCalledWith("\n");
    expect(writeToPty).toHaveBeenCalledTimes(2);

    // Third press — should work again
    handler(makeEvent());
    expect(writeToPty).toHaveBeenCalledTimes(3);
  });

  // --- Cmd+Arrow line navigation ---

  it("intercepts Cmd+Left and sends Ctrl+A (beginning of line)", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    const event = new KeyboardEvent("keydown", {
      key: "ArrowLeft",
      metaKey: true,
    });

    const result = handler(event);

    expect(result).toBe(false);
    expect(writeToPty).toHaveBeenCalledWith("\x01");
    expect(writeToPty).toHaveBeenCalledTimes(1);
  });

  it("intercepts Cmd+Right and sends Ctrl+E (end of line)", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    const event = new KeyboardEvent("keydown", {
      key: "ArrowRight",
      metaKey: true,
    });

    const result = handler(event);

    expect(result).toBe(false);
    expect(writeToPty).toHaveBeenCalledWith("\x05");
    expect(writeToPty).toHaveBeenCalledTimes(1);
  });

  it("does NOT intercept Cmd+Shift+Left (passes through)", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    const event = new KeyboardEvent("keydown", {
      key: "ArrowLeft",
      metaKey: true,
      shiftKey: true,
    });

    const result = handler(event);

    expect(result).toBe(true);
    expect(writeToPty).not.toHaveBeenCalled();
  });

  it("does NOT intercept Cmd+Shift+Right (passes through)", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    const event = new KeyboardEvent("keydown", {
      key: "ArrowRight",
      metaKey: true,
      shiftKey: true,
    });

    const result = handler(event);

    expect(result).toBe(true);
    expect(writeToPty).not.toHaveBeenCalled();
  });

  it("does NOT intercept Cmd+Arrow during IME composition", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    const event = new KeyboardEvent("keydown", {
      key: "ArrowLeft",
      metaKey: true,
    });
    Object.defineProperty(event, "isComposing", { value: true });

    const result = handler(event);

    expect(result).toBe(true);
    expect(writeToPty).not.toHaveBeenCalled();
  });

  it("does NOT intercept keyup events for Shift+Enter", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    const event = new KeyboardEvent("keyup", {
      key: "Enter",
      shiftKey: true,
    });

    const result = handler(event);

    expect(result).toBe(true);
    expect(writeToPty).not.toHaveBeenCalled();
  });
});

// ═══════════════════════════════════════════════════════════════════════════
// Key repeat polyfill tests
// ═══════════════════════════════════════════════════════════════════════════

describe("key repeat polyfill", () => {
  it("starts repeating after REPEAT_DELAY when native repeat is absent", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    // Initial keydown
    handler(makeKey("keydown", { key: "j" }));
    expect(writeToPty).not.toHaveBeenCalled(); // Polyfill doesn't emit on initial press

    // Advance past the delay
    vi.advanceTimersByTime(REPEAT_DELAY);

    // First repeat should fire immediately after delay
    vi.advanceTimersByTime(REPEAT_INTERVAL);
    expect(writeToPty).toHaveBeenCalledWith("j");

    // More repeats
    vi.advanceTimersByTime(REPEAT_INTERVAL * 3);
    expect(writeToPty).toHaveBeenCalledTimes(4); // 1 + 3 more
  });

  it("stops repeating on keyup of the held key", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    handler(makeKey("keydown", { key: "j" }));
    vi.advanceTimersByTime(REPEAT_DELAY + REPEAT_INTERVAL);
    expect(writeToPty).toHaveBeenCalledTimes(1);

    // Release the key
    handler(makeKey("keyup", { key: "j" }));

    // No more repeats
    vi.advanceTimersByTime(REPEAT_INTERVAL * 10);
    expect(writeToPty).toHaveBeenCalledTimes(1);
  });

  it("cancels polyfill when consistent native repeat events arrive", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    // Initial keydown
    handler(makeKey("keydown", { key: "j" }));

    // Enough native repeat events to reach threshold
    for (let i = 0; i < NATIVE_REPEAT_THRESHOLD; i++) {
      const repeatEvent = makeKey("keydown", { key: "j" });
      Object.defineProperty(repeatEvent, "repeat", { value: true });
      handler(repeatEvent);
    }

    // Advance well past the delay — polyfill should NOT fire
    vi.advanceTimersByTime(REPEAT_DELAY + REPEAT_INTERVAL * 10);
    expect(writeToPty).not.toHaveBeenCalled();
  });

  it("does NOT start polyfill for modifier-only keys", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    handler(makeKey("keydown", { key: "Shift", code: "ShiftLeft" }));
    vi.advanceTimersByTime(REPEAT_DELAY + REPEAT_INTERVAL * 5);
    expect(writeToPty).not.toHaveBeenCalled();

    handler(makeKey("keydown", { key: "Control", code: "ControlLeft" }));
    vi.advanceTimersByTime(REPEAT_DELAY + REPEAT_INTERVAL * 5);
    expect(writeToPty).not.toHaveBeenCalled();

    handler(makeKey("keydown", { key: "Alt", code: "AltLeft" }));
    vi.advanceTimersByTime(REPEAT_DELAY + REPEAT_INTERVAL * 5);
    expect(writeToPty).not.toHaveBeenCalled();

    handler(makeKey("keydown", { key: "Meta", code: "MetaLeft" }));
    vi.advanceTimersByTime(REPEAT_DELAY + REPEAT_INTERVAL * 5);
    expect(writeToPty).not.toHaveBeenCalled();
  });

  it("repeats arrow keys with correct escape sequences", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    handler(makeKey("keydown", { key: "ArrowDown", code: "ArrowDown" }));
    vi.advanceTimersByTime(REPEAT_DELAY + REPEAT_INTERVAL);
    expect(writeToPty).toHaveBeenCalledWith("\x1b[B");
  });

  it("repeats Backspace with correct sequence", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    handler(makeKey("keydown", { key: "Backspace", code: "Backspace" }));
    vi.advanceTimersByTime(REPEAT_DELAY + REPEAT_INTERVAL);
    expect(writeToPty).toHaveBeenCalledWith("\x7f");
  });

  it("repeats Delete with correct escape sequence", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    handler(makeKey("keydown", { key: "Delete", code: "Delete" }));
    vi.advanceTimersByTime(REPEAT_DELAY + REPEAT_INTERVAL);
    expect(writeToPty).toHaveBeenCalledWith("\x1b[3~");
  });

  it("switches repeat target when a new key is pressed", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    // Hold 'j'
    handler(makeKey("keydown", { key: "j" }));
    vi.advanceTimersByTime(REPEAT_DELAY + REPEAT_INTERVAL);
    expect(writeToPty).toHaveBeenCalledWith("j");

    // Press 'k' (without releasing 'j')
    writeToPty.mockClear();
    handler(makeKey("keydown", { key: "k" }));
    vi.advanceTimersByTime(REPEAT_DELAY + REPEAT_INTERVAL);
    expect(writeToPty).toHaveBeenCalledWith("k");
    expect(writeToPty).not.toHaveBeenCalledWith("j");
  });

  it("clearRepeat stops all active timers", () => {
    const writeToPty = vi.fn();
    const { handler, clearRepeat } = createTerminalKeyHandler(writeToPty);

    handler(makeKey("keydown", { key: "j" }));
    vi.advanceTimersByTime(REPEAT_DELAY + REPEAT_INTERVAL);
    expect(writeToPty).toHaveBeenCalledTimes(1);

    clearRepeat();
    vi.advanceTimersByTime(REPEAT_INTERVAL * 10);
    expect(writeToPty).toHaveBeenCalledTimes(1); // No more repeats
  });

  it("clearRepeat cancels the delay timer before repeat starts", () => {
    const writeToPty = vi.fn();
    const { handler, clearRepeat } = createTerminalKeyHandler(writeToPty);

    handler(makeKey("keydown", { key: "j" }));

    // Clear before delay fires
    clearRepeat();
    vi.advanceTimersByTime(REPEAT_DELAY + REPEAT_INTERVAL * 5);
    expect(writeToPty).not.toHaveBeenCalled();
  });

  it("does not repeat Shift+Enter (handled by custom handler)", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    handler(new KeyboardEvent("keydown", { key: "Enter", shiftKey: true }));
    // Shift+Enter sends ESC immediately
    expect(writeToPty).toHaveBeenCalledWith("\x1b");
    vi.advanceTimersByTime(10); // newline
    expect(writeToPty).toHaveBeenCalledWith("\n");

    // No polyfill repeat should fire
    writeToPty.mockClear();
    vi.advanceTimersByTime(REPEAT_DELAY + REPEAT_INTERVAL * 5);
    expect(writeToPty).not.toHaveBeenCalled();
  });

  it("does not repeat Cmd+Arrow (handled by custom handler)", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    handler(new KeyboardEvent("keydown", { key: "ArrowLeft", metaKey: true }));
    expect(writeToPty).toHaveBeenCalledWith("\x01");

    // No polyfill repeat
    writeToPty.mockClear();
    vi.advanceTimersByTime(REPEAT_DELAY + REPEAT_INTERVAL * 5);
    expect(writeToPty).not.toHaveBeenCalled();
  });

  it("keyup of a different key does not cancel the held key repeat", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    // Hold 'j'
    handler(makeKey("keydown", { key: "j" }));

    // Release a different key (e.g., from a previous press)
    handler(makeKey("keyup", { key: "k" }));

    // 'j' repeat should still work
    vi.advanceTimersByTime(REPEAT_DELAY + REPEAT_INTERVAL);
    expect(writeToPty).toHaveBeenCalledWith("j");
  });

  it("does not emit on initial keypress (only xterm handles it)", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    handler(makeKey("keydown", { key: "j" }));
    // No calls — xterm handles the initial press via its normal flow
    expect(writeToPty).not.toHaveBeenCalled();

    // Only starts after delay
    vi.advanceTimersByTime(REPEAT_DELAY - 1);
    expect(writeToPty).not.toHaveBeenCalled();
  });

  // --- Review finding fixes ---

  it("non-modifier unmapped keydown cancels active repeat (#3 modifier cancel)", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    // Hold 'j' — start polyfill
    handler(makeKey("keydown", { key: "j" }));

    // Press Ctrl+c (unmapped non-modifier) while holding 'j'
    handler(makeKey("keydown", { key: "c", ctrlKey: true }));

    // 'j' repeat should have been cancelled
    vi.advanceTimersByTime(REPEAT_DELAY + REPEAT_INTERVAL * 5);
    expect(writeToPty).not.toHaveBeenCalled();
  });

  it("modifier-only keydown does NOT cancel active repeat", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    // Hold 'j'
    handler(makeKey("keydown", { key: "j" }));

    // Press Shift alone — should not cancel 'j' repeat
    handler(makeKey("keydown", { key: "Shift", code: "ShiftLeft" }));

    // 'j' repeat should still fire
    vi.advanceTimersByTime(REPEAT_DELAY + REPEAT_INTERVAL);
    expect(writeToPty).toHaveBeenCalledWith("j");
  });

  it("repeats Tab key with correct sequence (#5 Tab)", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    handler(makeKey("keydown", { key: "Tab", code: "Tab" }));
    vi.advanceTimersByTime(REPEAT_DELAY + REPEAT_INTERVAL);
    expect(writeToPty).toHaveBeenCalledWith("\t");
  });

  it("uses application cursor mode sequences when mode callback returns true (#1 app cursor)", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty, {
      isApplicationCursorMode: () => true,
    });

    handler(makeKey("keydown", { key: "ArrowUp", code: "ArrowUp" }));
    vi.advanceTimersByTime(REPEAT_DELAY + REPEAT_INTERVAL);
    expect(writeToPty).toHaveBeenCalledWith("\x1bOA");
  });

  it("uses normal-mode arrow sequences when mode callback returns false", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty, {
      isApplicationCursorMode: () => false,
    });

    handler(makeKey("keydown", { key: "ArrowUp", code: "ArrowUp" }));
    vi.advanceTimersByTime(REPEAT_DELAY + REPEAT_INTERVAL);
    expect(writeToPty).toHaveBeenCalledWith("\x1b[A");
  });

  it("does not polyfill Shift+Arrow (#2 shiftKey guard)", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    handler(makeKey("keydown", { key: "ArrowLeft", code: "ArrowLeft", shiftKey: true }));
    vi.advanceTimersByTime(REPEAT_DELAY + REPEAT_INTERVAL * 5);
    expect(writeToPty).not.toHaveBeenCalled();
  });
});

// ═══════════════════════════════════════════════════════════════════════════
// F.9 regression tests — sporadic native repeat tolerance
// ═══════════════════════════════════════════════════════════════════════════

describe("F.9 — native repeat threshold", () => {
  /** Helper: create a native repeat event for a given key. */
  function makeNativeRepeat(key: string, code?: string): KeyboardEvent {
    const event = makeKey("keydown", { key, code: code ?? `Key${key.toUpperCase()}` });
    Object.defineProperty(event, "repeat", { value: true });
    return event;
  }

  it("1.1 polyfill recovers when native repeat fires once then stops (core regression)", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    // Hold key 'j'
    handler(makeKey("keydown", { key: "j" }));

    // WKWebView fires ONE native repeat event then stops
    handler(makeNativeRepeat("j"));

    // Advance past the delay — polyfill should still fire
    vi.advanceTimersByTime(REPEAT_DELAY);
    vi.advanceTimersByTime(REPEAT_INTERVAL);
    expect(writeToPty).toHaveBeenCalledWith("j");
  });

  it("1.2 consistent native repeat (>= threshold) cancels polyfill — no double emit", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    // Hold key 'j'
    handler(makeKey("keydown", { key: "j" }));

    // Fire NATIVE_REPEAT_THRESHOLD native repeat events
    for (let i = 0; i < NATIVE_REPEAT_THRESHOLD; i++) {
      handler(makeNativeRepeat("j"));
    }

    // Advance well past delay — polyfill should NOT fire
    vi.advanceTimersByTime(REPEAT_DELAY + REPEAT_INTERVAL * 10);
    expect(writeToPty).not.toHaveBeenCalled();
  });

  it("1.3 threshold boundary — exactly threshold-1 native repeats still allows polyfill", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    // Hold key 'j'
    handler(makeKey("keydown", { key: "j" }));

    // Fire exactly threshold-1 native repeat events
    for (let i = 0; i < NATIVE_REPEAT_THRESHOLD - 1; i++) {
      handler(makeNativeRepeat("j"));
    }

    // Advance past delay — polyfill should start
    vi.advanceTimersByTime(REPEAT_DELAY);
    vi.advanceTimersByTime(REPEAT_INTERVAL);
    expect(writeToPty).toHaveBeenCalledWith("j");
  });

  it("1.4 rapid key switch under sporadic native repeat — new key polyfills correctly", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    // Hold key 'j' — gets 1 sporadic native repeat
    handler(makeKey("keydown", { key: "j" }));
    handler(makeNativeRepeat("j"));

    // Quickly press 'k' (without releasing 'j') — no native repeat for 'k'
    handler(makeKey("keydown", { key: "k" }));

    // Advance past delay — 'k' should polyfill-repeat
    vi.advanceTimersByTime(REPEAT_DELAY + REPEAT_INTERVAL);
    expect(writeToPty).toHaveBeenCalledWith("k");
    expect(writeToPty).not.toHaveBeenCalledWith("j");
  });

  it("1.5 same key re-pressed after sporadic native repeat — second hold polyfills", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    // First hold of 'j' — gets 1 sporadic native repeat
    handler(makeKey("keydown", { key: "j" }));
    handler(makeNativeRepeat("j"));

    // Release 'j'
    handler(makeKey("keyup", { key: "j" }));

    // Re-press 'j' — no native repeat this time
    handler(makeKey("keydown", { key: "j" }));

    // Advance past delay — polyfill should start
    vi.advanceTimersByTime(REPEAT_DELAY + REPEAT_INTERVAL);
    expect(writeToPty).toHaveBeenCalledWith("j");
  });

  it("1.6 late native repeat events don't kill a committed polyfill", () => {
    const writeToPty = vi.fn();
    const { handler } = createTerminalKeyHandler(writeToPty);

    // Hold key 'j' — 1 sporadic native repeat during delay (below threshold)
    handler(makeKey("keydown", { key: "j" }));
    handler(makeNativeRepeat("j"));

    // Advance past delay — polyfill commits
    vi.advanceTimersByTime(REPEAT_DELAY + REPEAT_INTERVAL);
    expect(writeToPty).toHaveBeenCalledWith("j");
    const countAfterFirst = writeToPty.mock.calls.length;

    // WKWebView fires more sporadic native repeat events AFTER polyfill committed
    handler(makeNativeRepeat("j"));
    handler(makeNativeRepeat("j"));
    handler(makeNativeRepeat("j"));

    // Polyfill should STILL be running — more repeats fire
    vi.advanceTimersByTime(REPEAT_INTERVAL * 5);
    expect(writeToPty.mock.calls.length).toBeGreaterThan(countAfterFirst);
  });
});

// ═══════════════════════════════════════════════════════════════════════════
// C.2 — Scroll shortcut tests
// ═══════════════════════════════════════════════════════════════════════════

describe("scroll shortcuts (C.2)", () => {
  function makeScrollOptions() {
    return {
      isNormalScreen: vi.fn(() => true),
      scrollToTop: vi.fn(),
      scrollToBottom: vi.fn(),
      scrollLines: vi.fn(),
      scrollPages: vi.fn(),
    };
  }

  it("Cmd+Up calls scrollToTop and returns false", () => {
    const writeToPty = vi.fn();
    const scrollOpts = makeScrollOptions();
    const { handler } = createTerminalKeyHandler(writeToPty, scrollOpts);

    const result = handler(new KeyboardEvent("keydown", { key: "ArrowUp", metaKey: true }));

    expect(result).toBe(false);
    expect(scrollOpts.scrollToTop).toHaveBeenCalledOnce();
    expect(writeToPty).not.toHaveBeenCalled();
  });

  it("Cmd+Down calls scrollToBottom and returns false", () => {
    const writeToPty = vi.fn();
    const scrollOpts = makeScrollOptions();
    const { handler } = createTerminalKeyHandler(writeToPty, scrollOpts);

    const result = handler(new KeyboardEvent("keydown", { key: "ArrowDown", metaKey: true }));

    expect(result).toBe(false);
    expect(scrollOpts.scrollToBottom).toHaveBeenCalledOnce();
  });

  it("Alt+Up calls scrollLines(-10) and returns false", () => {
    const writeToPty = vi.fn();
    const scrollOpts = makeScrollOptions();
    const { handler } = createTerminalKeyHandler(writeToPty, scrollOpts);

    const result = handler(new KeyboardEvent("keydown", { key: "ArrowUp", altKey: true }));

    expect(result).toBe(false);
    expect(scrollOpts.scrollLines).toHaveBeenCalledWith(-10);
  });

  it("Alt+Down calls scrollLines(10) and returns false", () => {
    const writeToPty = vi.fn();
    const scrollOpts = makeScrollOptions();
    const { handler } = createTerminalKeyHandler(writeToPty, scrollOpts);

    const result = handler(new KeyboardEvent("keydown", { key: "ArrowDown", altKey: true }));

    expect(result).toBe(false);
    expect(scrollOpts.scrollLines).toHaveBeenCalledWith(10);
  });

  it("PageUp calls scrollPages(-1) and returns false", () => {
    const writeToPty = vi.fn();
    const scrollOpts = makeScrollOptions();
    const { handler } = createTerminalKeyHandler(writeToPty, scrollOpts);

    const result = handler(new KeyboardEvent("keydown", { key: "PageUp" }));

    expect(result).toBe(false);
    expect(scrollOpts.scrollPages).toHaveBeenCalledWith(-1);
  });

  it("PageDown calls scrollPages(1) and returns false", () => {
    const writeToPty = vi.fn();
    const scrollOpts = makeScrollOptions();
    const { handler } = createTerminalKeyHandler(writeToPty, scrollOpts);

    const result = handler(new KeyboardEvent("keydown", { key: "PageDown" }));

    expect(result).toBe(false);
    expect(scrollOpts.scrollPages).toHaveBeenCalledWith(1);
  });

  it("plain Up returns true (not intercepted)", () => {
    const writeToPty = vi.fn();
    const scrollOpts = makeScrollOptions();
    const { handler } = createTerminalKeyHandler(writeToPty, scrollOpts);

    const result = handler(new KeyboardEvent("keydown", { key: "ArrowUp" }));

    expect(result).toBe(true);
    expect(scrollOpts.scrollToTop).not.toHaveBeenCalled();
    expect(scrollOpts.scrollLines).not.toHaveBeenCalled();
  });

  it("Home returns true (not intercepted)", () => {
    const writeToPty = vi.fn();
    const scrollOpts = makeScrollOptions();
    const { handler } = createTerminalKeyHandler(writeToPty, scrollOpts);

    const result = handler(new KeyboardEvent("keydown", { key: "Home" }));

    expect(result).toBe(true);
    expect(scrollOpts.scrollToTop).not.toHaveBeenCalled();
  });

  it("End returns true (not intercepted)", () => {
    const writeToPty = vi.fn();
    const scrollOpts = makeScrollOptions();
    const { handler } = createTerminalKeyHandler(writeToPty, scrollOpts);

    const result = handler(new KeyboardEvent("keydown", { key: "End" }));

    expect(result).toBe(true);
    expect(scrollOpts.scrollToBottom).not.toHaveBeenCalled();
  });

  it("Cmd+Up in alternate screen returns true (passthrough)", () => {
    const writeToPty = vi.fn();
    const scrollOpts = makeScrollOptions();
    scrollOpts.isNormalScreen.mockReturnValue(false);
    const { handler } = createTerminalKeyHandler(writeToPty, scrollOpts);

    const result = handler(new KeyboardEvent("keydown", { key: "ArrowUp", metaKey: true }));

    expect(result).toBe(true);
    expect(scrollOpts.scrollToTop).not.toHaveBeenCalled();
  });

  it("repeat Alt+Up still calls scrollLines (continuous scrolling)", () => {
    const writeToPty = vi.fn();
    const scrollOpts = makeScrollOptions();
    const { handler } = createTerminalKeyHandler(writeToPty, scrollOpts);

    // Initial press
    handler(new KeyboardEvent("keydown", { key: "ArrowUp", altKey: true }));
    expect(scrollOpts.scrollLines).toHaveBeenCalledTimes(1);

    // Repeat event
    const repeatEvent = new KeyboardEvent("keydown", { key: "ArrowUp", altKey: true });
    Object.defineProperty(repeatEvent, "repeat", { value: true });
    const result = handler(repeatEvent);

    expect(result).toBe(false);
    expect(scrollOpts.scrollLines).toHaveBeenCalledTimes(2);
    expect(scrollOpts.scrollLines).toHaveBeenCalledWith(-10);
  });

  it("initial scroll keydown cancels active polyfill repeat timer", () => {
    const writeToPty = vi.fn();
    const scrollOpts = makeScrollOptions();
    const { handler } = createTerminalKeyHandler(writeToPty, scrollOpts);

    // Start holding 'j' — polyfill starts
    handler(makeKey("keydown", { key: "j" }));
    vi.advanceTimersByTime(REPEAT_DELAY + REPEAT_INTERVAL);
    expect(writeToPty).toHaveBeenCalledWith("j");

    // Now press Cmd+Up — should cancel 'j' polyfill
    writeToPty.mockClear();
    handler(new KeyboardEvent("keydown", { key: "ArrowUp", metaKey: true }));
    expect(scrollOpts.scrollToTop).toHaveBeenCalledOnce();

    // 'j' polyfill should no longer fire
    vi.advanceTimersByTime(REPEAT_INTERVAL * 10);
    expect(writeToPty).not.toHaveBeenCalled();
  });

  it("scroll shortcuts without callbacks are backward-compatible (no crash)", () => {
    const writeToPty = vi.fn();
    // No scroll options — old-style call
    const { handler } = createTerminalKeyHandler(writeToPty);

    const result = handler(new KeyboardEvent("keydown", { key: "ArrowUp", metaKey: true }));

    // Falls through — isNormalScreen not provided, so scroll block is skipped
    expect(result).toBe(true);
    expect(writeToPty).not.toHaveBeenCalled();
  });
});
