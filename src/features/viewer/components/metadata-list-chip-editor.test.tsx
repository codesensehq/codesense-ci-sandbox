// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import { holdTextMenuFocus } from "@/shared/lib/text-menu-focus";

import { MetadataListChipEditor } from "./metadata-list-chip-editor";

describe("MetadataListChipEditor (AC-3 / AC-6)", () => {
  it("keeps an empty Add field for its text menu and commits the pasted value on outside dismissal", async () => {
    const onChange = vi.fn();
    render(<MetadataListChipEditor items={[]} onChange={onChange} ariaLabel="depends_on" />);
    fireEvent.click(screen.getByRole("button", { name: /add/i }));
    const field = screen.getByLabelText("Add depends_on") as HTMLInputElement;
    const finish = holdTextMenuFocus(field);
    fireEvent.blur(field);
    expect(field.isConnected).toBe(true);
    expect(onChange).not.toHaveBeenCalled();
    fireEvent.change(field, { target: { value: "SV-PLG-13" } });
    field.blur();
    await act(async () => {
      finish(false);
      await new Promise(requestAnimationFrame);
    });
    expect(onChange).toHaveBeenCalledExactlyOnceWith(["SV-PLG-13"]);
    expect(screen.queryByLabelText("Add depends_on")).toBeNull();
  });
  it("rejects a chip value containing a comma with inline feedback", () => {
    const onChange = vi.fn();
    render(<MetadataListChipEditor items={[]} onChange={onChange} ariaLabel="depends_on" />);

    fireEvent.click(screen.getByRole("button", { name: /add/i }));
    const input = screen.getByLabelText(/add depends_on/i);
    fireEvent.change(input, { target: { value: "a,b" } });
    fireEvent.keyDown(input, { key: "Enter" });

    // No commit; inline error shown.
    expect(onChange).not.toHaveBeenCalled();
    expect(screen.getByText(/can't contain a comma/i)).toBeInTheDocument();
  });

  it("adds a valid chip and removes a chip", () => {
    const onChange = vi.fn();
    const { rerender } = render(
      <MetadataListChipEditor items={["SLW-RI-1"]} onChange={onChange} ariaLabel="depends_on" />,
    );

    fireEvent.click(screen.getByRole("button", { name: /add/i }));
    const input = screen.getByLabelText(/add depends_on/i);
    fireEvent.change(input, { target: { value: "SLW-RI-2" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onChange).toHaveBeenCalledWith(["SLW-RI-1", "SLW-RI-2"]);

    onChange.mockClear();
    rerender(
      <MetadataListChipEditor
        items={["SLW-RI-1", "SLW-RI-2"]}
        onChange={onChange}
        ariaLabel="depends_on"
      />,
    );
    fireEvent.click(screen.getByLabelText("Remove SLW-RI-1"));
    expect(onChange).toHaveBeenCalledWith(["SLW-RI-2"]);
  });

  it("hides remove buttons + the add affordance when disabled", () => {
    render(
      <MetadataListChipEditor items={["x"]} onChange={vi.fn()} ariaLabel="depends_on" disabled />,
    );
    expect(screen.queryByLabelText("Remove x")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /add/i })).not.toBeInTheDocument();
  });
});
