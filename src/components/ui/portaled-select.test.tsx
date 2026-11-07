// SPDX-License-Identifier: MIT
import { useState } from "react";

import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { PortaledSelect, type PortaledSelectOption } from "./portaled-select";

const OPTIONS: PortaledSelectOption[] = [
  { value: "a", label: "Apple" },
  { value: "b", label: "Banana" },
  { value: "c", label: "Cherry (unavailable)", disabled: true },
];

function Harness({
  onValueChange,
  initial = "",
}: {
  onValueChange: (v: string) => void;
  initial?: string;
}): React.JSX.Element {
  const [value, setValue] = useState(initial);
  return (
    <PortaledSelect
      value={value}
      onValueChange={(v) => {
        setValue(v);
        onValueChange(v);
      }}
      options={OPTIONS}
      ariaLabel="Fruit"
      placeholder="Choose a fruit…"
    />
  );
}

describe("PortaledSelect", () => {
  it("renders the placeholder when no value matches, and the label when one does", () => {
    const { rerender } = render(
      <PortaledSelect
        value=""
        onValueChange={() => {}}
        options={OPTIONS}
        ariaLabel="Fruit"
        placeholder="Choose a fruit…"
      />,
    );
    expect(screen.getByRole("combobox", { name: "Fruit" })).toHaveTextContent("Choose a fruit…");
    rerender(
      <PortaledSelect
        value="b"
        onValueChange={() => {}}
        options={OPTIONS}
        ariaLabel="Fruit"
        placeholder="Choose a fruit…"
      />,
    );
    expect(screen.getByRole("combobox", { name: "Fruit" })).toHaveTextContent("Banana");
  });

  it("opens on click and lists the options", async () => {
    const user = userEvent.setup();
    render(<Harness onValueChange={() => {}} />);
    const combobox = screen.getByRole("combobox", { name: "Fruit" });
    expect(combobox).toHaveAttribute("aria-expanded", "false");
    await user.click(combobox);
    expect(combobox).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByRole("option", { name: "Apple" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "Banana" })).toBeInTheDocument();
  });

  it("selecting an option calls onValueChange with its value", async () => {
    const user = userEvent.setup();
    const onValueChange = vi.fn();
    render(<Harness onValueChange={onValueChange} />);
    await user.click(screen.getByRole("combobox", { name: "Fruit" }));
    await user.click(screen.getByRole("option", { name: "Banana" }));
    expect(onValueChange).toHaveBeenCalledWith("b");
    // The trigger now reflects the selection and the popup closed.
    expect(screen.getByRole("combobox", { name: "Fruit" })).toHaveTextContent("Banana");
    expect(screen.getByRole("combobox", { name: "Fruit" })).toHaveAttribute(
      "aria-expanded",
      "false",
    );
  });

  it("marks the selected option with aria-selected", async () => {
    const user = userEvent.setup();
    render(<Harness onValueChange={() => {}} initial="a" />);
    await user.click(screen.getByRole("combobox", { name: "Fruit" }));
    expect(screen.getByRole("option", { name: "Apple" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("option", { name: "Banana" })).toHaveAttribute(
      "aria-selected",
      "false",
    );
  });

  it("selects via the keyboard (open, arrow, Enter)", async () => {
    const user = userEvent.setup();
    const onValueChange = vi.fn();
    render(<Harness onValueChange={onValueChange} />);
    const combobox = screen.getByRole("combobox", { name: "Fruit" });
    combobox.focus();
    // Open with ArrowDown → cursor on the first option (Apple), ArrowDown → Banana, Enter selects.
    await user.keyboard("{ArrowDown}");
    expect(combobox).toHaveAttribute("aria-expanded", "true");
    await user.keyboard("{ArrowDown}{Enter}");
    expect(onValueChange).toHaveBeenCalledWith("b");
  });

  it("does not select a disabled option", async () => {
    const user = userEvent.setup();
    const onValueChange = vi.fn();
    render(<Harness onValueChange={onValueChange} />);
    await user.click(screen.getByRole("combobox", { name: "Fruit" }));
    const disabled = screen.getByRole("option", { name: "Cherry (unavailable)" });
    expect(disabled).toHaveAttribute("aria-disabled", "true");
    await user.click(disabled);
    expect(onValueChange).not.toHaveBeenCalled();
  });

  it("supports grouped options", async () => {
    const user = userEvent.setup();
    const onValueChange = vi.fn();
    render(
      <PortaledSelect
        value=""
        onValueChange={onValueChange}
        groups={[
          { label: "Fruits", options: [{ value: "a", label: "Apple" }] },
          { label: "Veg", options: [{ value: "z", label: "Zucchini" }] },
        ]}
        ariaLabel="Produce"
      />,
    );
    await user.click(screen.getByRole("combobox", { name: "Produce" }));
    await user.click(screen.getByRole("option", { name: "Zucchini" }));
    expect(onValueChange).toHaveBeenCalledWith("z");
  });
});

describe("PortaledSelect keyboard cursor", () => {
  it("scrolls the highlighted option into view as the arrows move it", async () => {
    const scrolled: Element[] = [];
    const original = Element.prototype.scrollIntoView;
    Element.prototype.scrollIntoView = function (this: Element, arg?: unknown) {
      expect(arg).toEqual({ block: "nearest" });
      scrolled.push(this);
    };
    try {
      render(
        <PortaledSelect
          value="a"
          onValueChange={() => {}}
          options={Array.from({ length: 30 }, (_, i) => ({ value: `o${i}`, label: `Option ${i}` }))}
          ariaLabel="Many"
        />,
      );
      const user = userEvent.setup();
      screen.getByRole("combobox", { name: "Many" }).focus();
      await user.keyboard("{ArrowDown}");
      scrolled.length = 0;
      await user.keyboard("{ArrowDown}{ArrowDown}");
      const active = scrolled.at(-1) as HTMLElement;
      expect(active.getAttribute("role")).toBe("option");
      expect(active.dataset.value).toBe("o2");
    } finally {
      Element.prototype.scrollIntoView = original;
    }
  });
});
