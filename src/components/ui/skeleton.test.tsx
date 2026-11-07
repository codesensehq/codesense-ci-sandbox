// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";

import { Skeleton } from "./skeleton";

describe("Skeleton", () => {
  it("is a decorative block with the size it is given", () => {
    render(<Skeleton data-testid="s" className="h-11 w-full" />);
    const el = screen.getByTestId("s");
    expect(el).toHaveAttribute("aria-hidden", "true");
    expect(el.className).toContain("h-11");
  });

  it("can be round", () => {
    render(<Skeleton round data-testid="s" />);
    expect(screen.getByTestId("s").className).toContain("rounded-full");
  });
});
