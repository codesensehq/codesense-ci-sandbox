// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { FileDeletedPlaceholder } from "./file-deleted-placeholder";

describe("FileDeletedPlaceholder", () => {
  it("renders the file name", () => {
    render(<FileDeletedPlaceholder fileName="test.html" onClose={() => {}} />);
    expect(screen.getByText("test.html")).toBeInTheDocument();
  });

  it("shows 'removed from disk' message", () => {
    render(<FileDeletedPlaceholder fileName="file.md" onClose={() => {}} />);
    expect(screen.getByText("This file was removed from disk")).toBeInTheDocument();
  });

  it("renders Close Tab button", () => {
    render(<FileDeletedPlaceholder fileName="file.md" onClose={() => {}} />);
    expect(screen.getByRole("button", { name: "Close Tab" })).toBeInTheDocument();
  });

  it("calls onClose when Close Tab is clicked", async () => {
    const onClose = vi.fn();
    render(<FileDeletedPlaceholder fileName="file.md" onClose={onClose} />);
    await userEvent.click(screen.getByRole("button", { name: "Close Tab" }));
    expect(onClose).toHaveBeenCalledOnce();
  });
});
