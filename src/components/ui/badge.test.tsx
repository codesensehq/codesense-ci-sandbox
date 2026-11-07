// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";

import { Badge } from "./badge";

describe("Badge", () => {
  it("renders its content and defaults to the neutral tone", () => {
    render(<Badge data-testid="b">3</Badge>);
    const el = screen.getByTestId("b");
    expect(el).toHaveTextContent("3");
    expect(el).toHaveAttribute("data-tone", "neutral");
  });

  it.each(["needs", "busy", "failed", "warn", "quiet"] as const)("sets the %s tone", (tone) => {
    render(
      <Badge tone={tone} data-testid="b">
        x
      </Badge>,
    );
    expect(screen.getByTestId("b")).toHaveAttribute("data-tone", tone);
  });

  it("uses app tokens only, never palette colours or hex", () => {
    const { container } = render(
      <>
        {(["needs", "busy", "failed", "warn", "quiet", "neutral"] as const).map((t) => (
          <Badge key={t} tone={t}>
            x
          </Badge>
        ))}
      </>,
    );
    expect(container.innerHTML).not.toMatch(/amber-|emerald-|sky-|#[0-9a-f]{3,6}/i);
  });
});
