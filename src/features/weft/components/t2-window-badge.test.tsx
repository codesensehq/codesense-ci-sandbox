// SPDX-License-Identifier: MIT
import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@/test-utils";

import { T2WindowBadge } from "./t2-window-badge";

const base = {
  tier: "T2" as const,
  windowState: "open" as const,
  ttlS: 60,
  acquiredAt: 1000,
  pauseAccumulatedS: 0,
  notSurfacedButExpired: false,
};

describe("SW-WV-F5 · T2WindowBadge (AC-3)", () => {
  afterEach(() => vi.useRealTimers());

  it("returns null for a T1 row", () => {
    const { container } = render(<T2WindowBadge {...base} tier="T1" />);
    expect(container).toBeEmptyDOMElement();
  });

  it("returns null for a T3 row", () => {
    const { container } = render(<T2WindowBadge {...base} tier="T3" />);
    expect(container).toBeEmptyDOMElement();
  });

  it('open → "closes in Xs" countdown', () => {
    vi.useFakeTimers();
    vi.setSystemTime(1030 * 1000); // 30s after acquiredAt=1000, ttl 60 → 30s remaining
    render(<T2WindowBadge {...base} />);
    expect(screen.getByTestId("t2-window-badge")).toHaveAttribute("data-window", "open");
    expect(screen.getByTestId("t2-window-badge")).toHaveTextContent("closes in 30s");
  });

  it('paused → "paused (away)"', () => {
    render(<T2WindowBadge {...base} windowState="paused" />);
    expect(screen.getByTestId("t2-window-badge")).toHaveAttribute("data-window", "paused");
    expect(screen.getByTestId("t2-window-badge")).toHaveTextContent("paused (away)");
  });

  it('notSurfacedButExpired → "carries to next session" (takes precedence)', () => {
    render(<T2WindowBadge {...base} notSurfacedButExpired windowState="open" />);
    expect(screen.getByTestId("t2-window-badge")).toHaveAttribute("data-window", "carried");
    expect(screen.getByTestId("t2-window-badge")).toHaveTextContent("carries to next session");
  });

  it("expired → renders nothing", () => {
    const { container } = render(<T2WindowBadge {...base} windowState="expired" />);
    expect(container).toBeEmptyDOMElement();
  });

  it("clears the countdown interval on unmount (no leaked timer)", () => {
    vi.useFakeTimers();
    const clearSpy = vi.spyOn(globalThis, "clearInterval");
    const { unmount } = render(<T2WindowBadge {...base} />);
    unmount();
    expect(clearSpy).toHaveBeenCalled();
    clearSpy.mockRestore();
  });
});
