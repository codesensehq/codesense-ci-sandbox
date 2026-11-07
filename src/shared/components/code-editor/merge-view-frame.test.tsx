// SPDX-License-Identifier: MIT
/**
 * SLW-WD-7 — the shared `<MergeViewFrame>` scroll contract. Both consumers
 * (`<CodeDiffReview>` read-only diff + `<CodeMergeView>` editable conflict)
 * mount into this one frame, so asserting the outer style here covers both
 * (AC-4/AC-5). happy-dom does no layout, so we assert the applied style
 * contract, not measured overflow — real no-clip scrolling is the story's
 * characterization step.
 */
import { render, screen } from "@testing-library/react";
import { describe, it, expect } from "vitest";
import { MergeViewFrame } from "./merge-view-frame";

const baseProps = {
  containerRef: () => {},
  retry: () => {},
  testId: "frame",
  ariaLabel: "merge frame",
  loadingLabel: "Loading…",
  errorText: "boom",
};

describe("MergeViewFrame — scroll contract (SLW-WD-7 AC-4/AC-5)", () => {
  it("owns VERTICAL scroll (overflowY:auto) while keeping horizontal clipped (overflowX:hidden)", () => {
    render(<MergeViewFrame {...baseProps} loadState="ready" />);
    const frame = screen.getByTestId("frame");
    // The overgrown split MergeView (its inner scrollers disabled by the lib)
    // scrolls in this frame instead of being clipped by the old overflow:hidden.
    expect(frame.style.overflowY).toBe("auto");
    // Per-pane horizontal stays inside each editor's own scroller.
    expect(frame.style.overflowX).toBe("hidden");
    // Bounded height is what makes overflowY:auto bite.
    expect(frame.style.height).toBe("100%");
  });

  it("keeps the scroll contract across loading + error states (shared by both consumers)", () => {
    const { rerender } = render(<MergeViewFrame {...baseProps} loadState="loading" />);
    expect(screen.getByTestId("frame").style.overflowY).toBe("auto");
    rerender(<MergeViewFrame {...baseProps} loadState="error" />);
    expect(screen.getByTestId("frame").style.overflowY).toBe("auto");
  });

  it("respects a custom height while preserving the scroll contract", () => {
    render(<MergeViewFrame {...baseProps} loadState="ready" height="400px" />);
    const frame = screen.getByTestId("frame");
    expect(frame.style.height).toBe("400px");
    expect(frame.style.overflowY).toBe("auto");
  });
});
