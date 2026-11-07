// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";

import { ReadOnlyChip } from "./read-only-chip";

describe("ReadOnlyChip (SW-VE-2 AC-4)", () => {
  it("renders a visible read-only affordance with an accessible label", () => {
    render(<ReadOnlyChip />);
    const chip = screen.getByLabelText("Read-only");
    expect(chip).toBeInTheDocument();
    expect(chip).toHaveTextContent("Read-only");
  });

  it("is an inert badge (no button) when the file is not editable", () => {
    render(<ReadOnlyChip />);
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });

  it("passes through a className for chip-cluster positioning", () => {
    const { container } = render(<ReadOnlyChip className="relative" />);
    expect(container.firstChild).toHaveClass("relative");
  });
});

describe("ReadOnlyChip (SW-VE-3 — deliberate edit switch)", () => {
  it("SW-VE-16 (AC-8): canEdit splits the chip into a 'Read-only' badge + a distinct 'Edit' button", () => {
    const onEdit = vi.fn();
    render(<ReadOnlyChip canEdit onEdit={onEdit} />);
    expect(screen.getByLabelText("Read-only")).toBeInTheDocument();
    const btn = screen.getByRole("button", { name: "Edit" });
    expect(btn).toHaveTextContent("Edit");
    expect(btn).not.toHaveTextContent("Read-only");
    fireEvent.click(btn);
    expect(onEdit).toHaveBeenCalledTimes(1);
  });

  it("SW-VE-16 R5 N-R4-1: Edit button hover resolves to `--ibx-lifted` (dual-theme elevation step)", () => {
    // Locks the token-palette honesty fix. Using `--ibx-raise` here (R4 state) regressed in
    // light theme — `--ibx-raise` COLLIDES with `--ibx-muted` (the toolbar background) in
    // light, making hover invisible. `--ibx-lifted` is defined ABOVE `--ibx-muted` in both
    // themes. A regression that reverted to `--ibx-raise` (or minted a `color-mix` recipe)
    // would fail this assertion and signal the drift back into reviewer-flagged territory.
    render(<ReadOnlyChip canEdit onEdit={vi.fn()} />);
    const btn = screen.getByRole("button", { name: "Edit" });
    expect(btn.className).toMatch(/hover:bg-\[var\(--ibx-lifted\)\]/);
  });

  it("stays inert (no button) when canEdit but no onEdit wired", () => {
    render(<ReadOnlyChip canEdit />);
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
    expect(screen.getByLabelText("Read-only")).toBeInTheDocument();
  });

  it("reads 'Editing' with a dirty-dot slot when editing + dirty", () => {
    render(<ReadOnlyChip editing dirty onEdit={vi.fn()} />);
    expect(screen.getByLabelText("Editing")).toHaveTextContent("Editing");
    expect(screen.getByTestId("read-only-chip-dirty-dot")).toBeInTheDocument();
    // no Edit button while editing
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });

  it("reads 'Editing' without a dot when the buffer is clean", () => {
    render(<ReadOnlyChip editing onEdit={vi.fn()} />);
    expect(screen.getByLabelText("Editing")).toBeInTheDocument();
    expect(screen.queryByTestId("read-only-chip-dirty-dot")).not.toBeInTheDocument();
  });
});
