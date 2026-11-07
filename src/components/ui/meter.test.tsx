// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";

import { Meter } from "./meter";

describe("Meter", () => {
  it("exposes the value, the max and a label", () => {
    render(<Meter value={3} max={10} label="Spend" />);
    const m = screen.getByRole("meter", { name: "Spend" });
    expect(m).toHaveAttribute("aria-valuenow", "3");
    expect(m).toHaveAttribute("aria-valuemax", "10");
  });

  it("is plain below 80%, hot from 80% and full at the max", () => {
    const { rerender } = render(<Meter value={5} max={10} label="s" />);
    expect(screen.getByRole("meter")).toHaveAttribute("data-level", "ok");
    rerender(<Meter value={8} max={10} label="s" />);
    expect(screen.getByRole("meter")).toHaveAttribute("data-level", "hot");
    rerender(<Meter value={12} max={10} label="s" />);
    expect(screen.getByRole("meter")).toHaveAttribute("data-level", "full");
  });

  it("fills in proportion and never past the track", () => {
    render(<Meter value={15} max={10} label="s" />);
    expect(screen.getByRole("meter").firstElementChild).toHaveStyle({ width: "100%" });
  });

  it("handles a zero max without dividing by it", () => {
    render(<Meter value={0} max={0} label="s" />);
    expect(screen.getByRole("meter").firstElementChild).toHaveStyle({ width: "0%" });
  });
});
