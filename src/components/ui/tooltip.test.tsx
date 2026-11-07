// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { InfoTip, TooltipProvider } from "./tooltip";

function renderTip(
  ui: React.ReactNode = (
    <InfoTip label="Fit pipeline to view">
      <button type="button" aria-label="Fit">
        F
      </button>
    </InfoTip>
  ),
) {
  return render(<TooltipProvider>{ui}</TooltipProvider>);
}

describe("InfoTip (SW-FX-80 AC-1)", () => {
  it("renders the trigger with its own accessible name (tip supplements, not replaces)", () => {
    renderTip();
    expect(screen.getByRole("button", { name: "Fit" })).toBeInTheDocument();
    // No tooltip mounted until interaction.
    expect(screen.queryByRole("tooltip")).not.toBeInTheDocument();
  });

  it("reveals the label in a portaled Radix tooltip on hover", async () => {
    const user = userEvent.setup();
    renderTip();
    await user.hover(screen.getByRole("button", { name: "Fit" }));
    expect(await screen.findByRole("tooltip")).toHaveTextContent("Fit pipeline to view");
  });

  it("reveals the label on keyboard focus", async () => {
    renderTip();
    screen.getByRole("button", { name: "Fit" }).focus();
    expect(await screen.findByRole("tooltip")).toHaveTextContent("Fit pipeline to view");
  });

  it("passes through to the child (no tooltip) when the label is falsy", async () => {
    const user = userEvent.setup();
    renderTip(
      <InfoTip label={undefined}>
        <button type="button" aria-label="Bare">
          B
        </button>
      </InfoTip>,
    );
    const btn = screen.getByRole("button", { name: "Bare" });
    await user.hover(btn);
    expect(screen.queryByRole("tooltip")).not.toBeInTheDocument();
  });

  it("forwards the requested side to the tooltip content", async () => {
    const user = userEvent.setup();
    renderTip(
      <InfoTip label="Bottom tip" side="bottom">
        <button type="button" aria-label="Hint">
          H
        </button>
      </InfoTip>,
    );
    await user.hover(screen.getByRole("button", { name: "Hint" }));
    await screen.findByRole("tooltip");
    // The content is portaled to document.body, outside the render container.
    expect(document.querySelector('[data-slot="tooltip-content"]')).toHaveAttribute(
      "data-side",
      "bottom",
    );
  });
});
