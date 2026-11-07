// SPDX-License-Identifier: MIT
import { createRef } from "react";
import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";

import { Button } from "@/components/ui/button";
import { AppButton } from "./app-button";

describe("AppButton", () => {
  it("is the one shared button: the same component as the base Button", () => {
    expect(AppButton).toBe(Button);
  });

  it("defaults to the primary (default) variant with a pointer cursor", () => {
    render(<AppButton>Join team</AppButton>);
    const button = screen.getByRole("button", { name: "Join team" });
    expect(button).toHaveAttribute("data-variant", "default");
    expect(button.className).toContain("bg-primary");
    expect(button.className).toContain("cursor-pointer");
  });

  it("renders secondary as outline and destructive as destructive", () => {
    render(
      <>
        <AppButton variant="outline">Cancel</AppButton>
        <AppButton variant="destructive">Remove</AppButton>
      </>,
    );
    expect(screen.getByRole("button", { name: "Cancel" })).toHaveAttribute(
      "data-variant",
      "outline",
    );
    expect(screen.getByRole("button", { name: "Remove" })).toHaveAttribute(
      "data-variant",
      "destructive",
    );
  });

  it("forwards a ref to the button element", () => {
    const ref = createRef<HTMLButtonElement>();
    render(<AppButton ref={ref}>x</AppButton>);
    expect(ref.current).toBeInstanceOf(HTMLButtonElement);
  });
});
