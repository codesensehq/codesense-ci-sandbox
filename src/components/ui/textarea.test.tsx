// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { Textarea } from "./textarea";

describe("Textarea", () => {
  it("is a textbox that takes typing and the usual props", async () => {
    render(<Textarea aria-label="Prompt" placeholder="Say it" rows={3} />);
    const box = screen.getByRole("textbox", { name: "Prompt" });
    await userEvent.type(box, "hello");
    expect(box).toHaveValue("hello");
    expect(box).toHaveAttribute("rows", "3");
    expect(box).toHaveAttribute("data-slot", "textarea");
  });

  it("marks an invalid value through aria-invalid", () => {
    render(<Textarea aria-label="Prompt" aria-invalid />);
    expect(screen.getByRole("textbox")).toHaveAttribute("aria-invalid", "true");
  });
});
