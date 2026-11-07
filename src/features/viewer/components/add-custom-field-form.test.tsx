// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";

import { AddCustomFieldForm } from "./add-custom-field-form";

function setup(props: Partial<React.ComponentProps<typeof AddCustomFieldForm>> = {}) {
  const onDeclare = vi.fn().mockResolvedValue(undefined);
  const onClose = vi.fn();
  render(<AddCustomFieldForm onDeclare={onDeclare} onClose={onClose} {...props} />);
  return { onDeclare, onClose };
}

describe("AddCustomFieldForm (SW-AU-77E)", () => {
  it("AC-6: declares a text field with the typed value and closes on success", async () => {
    const { onDeclare, onClose } = setup();
    fireEvent.change(screen.getByLabelText("new field name"), { target: { value: "risk" } });
    fireEvent.change(screen.getByLabelText("new field value"), { target: { value: "high" } });
    await act(async () => {
      fireEvent.click(screen.getByText("Add field"));
    });
    expect(onDeclare).toHaveBeenCalledWith({
      name: "risk",
      type: "text",
      allowedValues: [],
      value: "high",
    });
    expect(onClose).toHaveBeenCalled();
  });

  it("AC-1: a choice field requires at least one allowed value before it can be added", () => {
    setup();
    fireEvent.change(screen.getByLabelText("new field name"), { target: { value: "tier" } });
    fireEvent.change(screen.getByLabelText("new field type"), { target: { value: "choice" } });
    // The choice vocabulary editor appears, and Add stays disabled until it has a value.
    expect(screen.getByLabelText("allowed values")).toBeInTheDocument();
    expect(screen.getByText("Add field")).toBeDisabled();
  });

  it("AC-5: disabled with a truthful reason when declaring is not allowed", () => {
    setup({ disabledReason: "team 'x' is a read-only projected/installed view" });
    expect(screen.getByText(/read-only projected/i)).toBeInTheDocument();
    expect(screen.getByLabelText("new field name")).toBeDisabled();
    expect(screen.getByText("Add field")).toBeDisabled();
  });

  it("AC-1: rejects a non-identifier name without calling the backend", async () => {
    const { onDeclare } = setup();
    fireEvent.change(screen.getByLabelText("new field name"), { target: { value: "Risk Tier" } });
    await act(async () => {
      fireEvent.click(screen.getByText("Add field"));
    });
    expect(onDeclare).not.toHaveBeenCalled();
    expect(screen.getByRole("alert")).toHaveTextContent(/simple name/i);
  });

  it("AC-3: surfaces a backend rejection (invalid value) as an inline error", async () => {
    const onDeclare = vi.fn().mockRejectedValue("'ten' is not a valid number value");
    render(<AddCustomFieldForm onDeclare={onDeclare} onClose={vi.fn()} />);
    fireEvent.change(screen.getByLabelText("new field name"), { target: { value: "count" } });
    fireEvent.change(screen.getByLabelText("new field type"), { target: { value: "number" } });
    fireEvent.change(screen.getByLabelText("new field value"), { target: { value: "ten" } });
    await act(async () => {
      fireEvent.click(screen.getByText("Add field"));
    });
    expect(screen.getByRole("alert")).toHaveTextContent("not a valid number value");
  });
});
