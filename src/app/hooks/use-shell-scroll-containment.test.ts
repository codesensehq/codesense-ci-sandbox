// SPDX-License-Identifier: MIT
import { renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useShellScrollContainment } from "./use-shell-scroll-containment";

describe("useShellScrollContainment", () => {
  afterEach(() => {
    // Drop the override so the real (jsdom) scrollingElement is restored.
    Reflect.deleteProperty(document, "scrollingElement");
  });

  function withScrollingElement(el: { scrollTop: number; scrollLeft: number; scrollTo: unknown }) {
    Object.defineProperty(document, "scrollingElement", { value: el, configurable: true });
  }

  it("snaps the document back to the origin when it scrolls (the shell can never displace)", () => {
    const scrollTo = vi.fn();
    withScrollingElement({ scrollTop: 120, scrollLeft: 0, scrollTo });
    renderHook(() => useShellScrollContainment());
    window.dispatchEvent(new Event("scroll"));
    expect(scrollTo).toHaveBeenCalledWith(0, 0);
  });

  it("no-ops when the document is already at the origin — inner-pane scrolls don't trigger it", () => {
    const scrollTo = vi.fn();
    withScrollingElement({ scrollTop: 0, scrollLeft: 0, scrollTo });
    renderHook(() => useShellScrollContainment());
    window.dispatchEvent(new Event("scroll"));
    expect(scrollTo).not.toHaveBeenCalled();
  });

  it("removes the scroll listener on unmount", () => {
    const scrollTo = vi.fn();
    withScrollingElement({ scrollTop: 80, scrollLeft: 0, scrollTo });
    const { unmount } = renderHook(() => useShellScrollContainment());
    unmount();
    window.dispatchEvent(new Event("scroll"));
    expect(scrollTo).not.toHaveBeenCalled();
  });
});
