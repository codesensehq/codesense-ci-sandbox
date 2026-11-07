// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { ProgressChipRow } from "./progress-chip-row";

describe("ProgressChipRow", () => {
  it("renders nothing when total is 0", () => {
    const { container } = render(<ProgressChipRow total={0} done={0} />);
    expect(container.innerHTML).toBe("");
  });

  it("renders the done/total fraction and spells it out for screen readers and the tooltip", () => {
    render(<ProgressChipRow total={10} done={3} />);
    expect(screen.getByText("3/10")).toBeInTheDocument();
    expect(screen.getByText("3 of 10 stories done")).toHaveClass("sr-only");
    expect(screen.getByTestId("nav-story-progress")).toHaveAttribute(
      "title",
      "3 of 10 stories done",
    );
  });

  it("always draws the bar so the fraction never reads as a match count", () => {
    render(<ProgressChipRow total={10} done={5} />);
    const bar = screen.getByTestId("nav-story-progress-bar");
    expect(bar).not.toHaveClass("hidden");
    expect(bar.firstElementChild).toHaveStyle({ width: "50%" });
  });

  it("caps the bar at 100%", () => {
    render(<ProgressChipRow total={5} done={7} />);
    expect(screen.getByTestId("nav-story-progress-bar").firstElementChild).toHaveStyle({
      width: "100%",
    });
  });

  it("handles none done (0%)", () => {
    render(<ProgressChipRow total={5} done={0} />);
    expect(screen.getByText("0/5")).toBeInTheDocument();
  });
});
