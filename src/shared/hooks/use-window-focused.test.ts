// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { renderHook, act } from "@testing-library/react";

import { computeFocused, useWindowFocused } from "./use-window-focused";

// Drive the two inputs `computeFocused` reads.
function setHasFocus(v: boolean): void {
  Object.defineProperty(document, "hasFocus", { configurable: true, value: () => v });
}
function setVisibility(state: "visible" | "hidden"): void {
  Object.defineProperty(document, "visibilityState", { configurable: true, get: () => state });
}

describe("computeFocused", () => {
  afterEach(() => {
    setHasFocus(true);
    setVisibility("visible");
  });

  it("is true only when the doc has focus AND is visible", () => {
    setHasFocus(true);
    setVisibility("visible");
    expect(computeFocused()).toBe(true);
  });

  it("is FALSE when hasFocus is true but the document is HIDDEN (minimize/hide)", () => {
    setHasFocus(true);
    setVisibility("hidden");
    expect(computeFocused()).toBe(false);
  });

  it("is false when the doc lacks focus even if visible", () => {
    setHasFocus(false);
    setVisibility("visible");
    expect(computeFocused()).toBe(false);
  });
});

describe("useWindowFocused", () => {
  beforeEach(() => {
    setHasFocus(true);
    setVisibility("visible");
  });
  afterEach(() => {
    setHasFocus(true);
    setVisibility("visible");
  });

  it("seeds from computeFocused (focused + visible → true)", () => {
    const { result } = renderHook(() => useWindowFocused());
    expect(result.current).toBe(true);
  });

  it("flips to false on window blur and back to true on focus", () => {
    const { result } = renderHook(() => useWindowFocused());
    expect(result.current).toBe(true);

    act(() => {
      setHasFocus(false);
      window.dispatchEvent(new Event("blur"));
    });
    expect(result.current).toBe(false);

    act(() => {
      setHasFocus(true);
      window.dispatchEvent(new Event("focus"));
    });
    expect(result.current).toBe(true);
  });

  it("flips to false on a visibilitychange to hidden (no window blur fired)", () => {
    const { result } = renderHook(() => useWindowFocused());
    expect(result.current).toBe(true);

    act(() => {
      // hasFocus stays true — only visibility changes (the minimize/hide case).
      setVisibility("hidden");
      document.dispatchEvent(new Event("visibilitychange"));
    });
    expect(result.current).toBe(false);

    act(() => {
      setVisibility("visible");
      document.dispatchEvent(new Event("visibilitychange"));
    });
    expect(result.current).toBe(true);
  });
});
