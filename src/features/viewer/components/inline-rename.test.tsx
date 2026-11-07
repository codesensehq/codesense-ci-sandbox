// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { InlineRename } from "@/shared/components/inline-rename";

describe("InlineRename (SW-SN-2)", () => {
  it("commits a changed value on Enter", () => {
    const onSubmit = vi.fn();
    const onCancel = vi.fn();
    render(<InlineRename value="old" onSubmit={onSubmit} onCancel={onCancel} ariaLabel="Rename" />);
    const input = screen.getByLabelText("Rename");
    fireEvent.change(input, { target: { value: "new" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onSubmit).toHaveBeenCalledWith("new");
    expect(onCancel).not.toHaveBeenCalled();
  });

  it("cancels on Escape without submitting", () => {
    const onSubmit = vi.fn();
    const onCancel = vi.fn();
    render(<InlineRename value="old" onSubmit={onSubmit} onCancel={onCancel} ariaLabel="Rename" />);
    const input = screen.getByLabelText("Rename");
    fireEvent.change(input, { target: { value: "new" } });
    fireEvent.keyDown(input, { key: "Escape" });
    expect(onCancel).toHaveBeenCalled();
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("commits on blur", () => {
    const onSubmit = vi.fn();
    render(<InlineRename value="old" onSubmit={onSubmit} onCancel={vi.fn()} ariaLabel="Rename" />);
    const input = screen.getByLabelText("Rename");
    fireEvent.change(input, { target: { value: "renamed" } });
    fireEvent.blur(input);
    expect(onSubmit).toHaveBeenCalledWith("renamed");
  });

  it("treats an unchanged or empty value as a cancel (no submit)", () => {
    const onSubmit = vi.fn();
    const onCancel = vi.fn();
    render(
      <InlineRename value="keep" onSubmit={onSubmit} onCancel={onCancel} ariaLabel="Rename" />,
    );
    const input = screen.getByLabelText("Rename");
    fireEvent.change(input, { target: { value: "   " } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onSubmit).not.toHaveBeenCalled();
    expect(onCancel).toHaveBeenCalled();
  });

  it("does not double-commit when blur follows Enter", () => {
    const onSubmit = vi.fn();
    render(<InlineRename value="old" onSubmit={onSubmit} onCancel={vi.fn()} ariaLabel="Rename" />);
    const input = screen.getByLabelText("Rename");
    fireEvent.change(input, { target: { value: "new" } });
    fireEvent.keyDown(input, { key: "Enter" });
    fireEvent.blur(input);
    expect(onSubmit).toHaveBeenCalledTimes(1);
  });
});
