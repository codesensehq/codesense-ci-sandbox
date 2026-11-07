// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import type { MouseEvent } from "react";
import { cn } from "@/lib/utils";
import { startDragging } from "@/shared/lib/titlebar";

// Mock Tauri window API
const mockStartDragging = vi.fn();
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ startDragging: mockStartDragging }),
}));

// ── cn() ─────────────────────────────────────────────────────────────

describe("cn() @P2 — P2-024 class merging", () => {
  it("merges multiple class strings", () => {
    // Given two class strings
    // When merged with cn()
    const result = cn("px-2 py-1", "text-sm");

    // Then both classes are present
    expect(result).toContain("px-2");
    expect(result).toContain("py-1");
    expect(result).toContain("text-sm");
  });

  it("handles conditional classes via clsx", () => {
    // Given a falsy condition
    // When passed to cn()
    const isHidden = false;
    const result = cn("base", isHidden && "hidden", "visible");

    // Then falsy value is excluded
    expect(result).toContain("base");
    expect(result).toContain("visible");
    expect(result).not.toContain("hidden");
  });

  it("tailwind-merge resolves conflicting utilities", () => {
    // Given conflicting tailwind padding classes
    // When merged with cn()
    const result = cn("px-2", "px-4");

    // Then the later value wins (tailwind-merge behavior)
    expect(result).toBe("px-4");
    expect(result).not.toContain("px-2");
  });

  it("tailwind-merge resolves conflicting text colors", () => {
    // Given conflicting text color classes
    const result = cn("text-red-500", "text-blue-500");

    // Then the later value wins
    expect(result).toBe("text-blue-500");
  });

  it("handles undefined and null inputs gracefully", () => {
    // Given undefined/null inputs
    const result = cn(undefined, null, "flex");

    // Then only valid classes remain
    expect(result).toBe("flex");
  });

  it("handles array inputs", () => {
    // Given an array of classes
    const result = cn(["flex", "items-center"]);

    // Then all classes are present
    expect(result).toContain("flex");
    expect(result).toContain("items-center");
  });

  it("returns empty string for no inputs", () => {
    // Given no inputs
    const result = cn();

    // Then result is empty string
    expect(result).toBe("");
  });
});

// SS-9: the statusConfig describe block retired with sync-store's
// install-health half. workspaceStatusConfig survives but has only 2
// entries with no business logic to test — covered transitively by the
// repo-picker-view + quick-repo-dialog rendering tests.

// ── startDragging ────────────────────────────────────────────────────

describe("startDragging @P2 — P2-023 excludes interactive elements", () => {
  function makeFakeMouseEvent(target: HTMLElement, button = 0) {
    return {
      button,
      target,
    } as unknown as MouseEvent;
  }

  beforeEach(() => {
    mockStartDragging.mockClear();
  });

  it("calls startDragging on a plain div target", () => {
    // Given a plain div (not interactive)
    const div = document.createElement("div");

    // When startDragging is called
    startDragging(makeFakeMouseEvent(div));

    // Then native drag is initiated
    expect(mockStartDragging).toHaveBeenCalled();
  });

  it("does NOT drag when target is a button", () => {
    // Given a button element
    const btn = document.createElement("button");

    // When startDragging is called
    startDragging(makeFakeMouseEvent(btn));

    // Then native drag is NOT initiated
    expect(mockStartDragging).not.toHaveBeenCalled();
  });

  it("does NOT drag when target is inside a button", () => {
    // Given a span nested inside a button
    const btn = document.createElement("button");
    const span = document.createElement("span");
    btn.appendChild(span);
    document.body.appendChild(btn);

    // When startDragging is called with the span as target
    startDragging(makeFakeMouseEvent(span));

    // Then native drag is NOT initiated (closest finds the button)
    expect(mockStartDragging).not.toHaveBeenCalled();

    document.body.removeChild(btn);
  });

  it("does NOT drag when target is an input", () => {
    // Given an input element
    const input = document.createElement("input");

    // When startDragging is called
    startDragging(makeFakeMouseEvent(input));

    // Then native drag is NOT initiated
    expect(mockStartDragging).not.toHaveBeenCalled();
  });

  it("does NOT drag when target is an anchor", () => {
    // Given an anchor element
    const a = document.createElement("a");

    // When startDragging is called
    startDragging(makeFakeMouseEvent(a));

    // Then native drag is NOT initiated
    expect(mockStartDragging).not.toHaveBeenCalled();
  });

  it("does NOT drag when target has role='button'", () => {
    // Given a div with role="button"
    const div = document.createElement("div");
    div.setAttribute("role", "button");

    // When startDragging is called
    startDragging(makeFakeMouseEvent(div));

    // Then native drag is NOT initiated
    expect(mockStartDragging).not.toHaveBeenCalled();
  });

  it("ignores non-left-button clicks", () => {
    // Given a plain div but right-click (button=2)
    const div = document.createElement("div");

    // When startDragging is called with button=2
    startDragging(makeFakeMouseEvent(div, 2));

    // Then native drag is NOT initiated
    expect(mockStartDragging).not.toHaveBeenCalled();
  });
});
