// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";

import { Field } from "./field";
import { Input } from "./input";

describe("Field", () => {
  it("labels its control and joins the hint to it", () => {
    render(
      <Field label="Name" hint="Shown in the list">
        <Input />
      </Field>,
    );
    const input = screen.getByLabelText("Name");
    expect(input).toHaveAccessibleDescription("Shown in the list");
  });

  it("shows an error at the control and marks it invalid", () => {
    render(
      <Field label="Name" error="Name is required">
        <Input />
      </Field>,
    );
    const input = screen.getByLabelText("Name");
    expect(input).toHaveAttribute("aria-invalid", "true");
    expect(input).toHaveAccessibleDescription("Name is required");
    expect(screen.getByRole("alert")).toHaveTextContent("Name is required");
  });

  it("has no alert and no invalid mark without an error", () => {
    render(
      <Field label="Name">
        <Input />
      </Field>,
    );
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByLabelText("Name")).not.toHaveAttribute("aria-invalid");
  });
});
