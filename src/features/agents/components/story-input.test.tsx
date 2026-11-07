// SPDX-License-Identifier: MIT
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { StoryInput } from "./agent-launch-sheet";

afterEach(cleanup);
const story = (storyId: string, title = "A story") => ({ storyId, title });
const crowded = [
  ...Array.from({ length: 10 }, (_, i) => story(`SV-PBF-${30 + i}`)),
  story("SV-PBF-3"),
];
function setup(stories = crowded, value = "") {
  const onChange = vi.fn();
  const view = render(<StoryInput value={value} stories={stories} onChange={onChange} />);
  const input = screen.getByRole("combobox");
  fireEvent.focus(input);
  return { ...view, input, onChange };
}

describe("story picker", () => {
  it.each(["sv-pbf-3", "SV-PBF-3"])(
    "shows the exact id first for %s even after ten prefixes",
    (value) => {
      setup(crowded, value);
      const rows = screen.getAllByRole("option");
      expect(rows).toHaveLength(8);
      expect(rows[0]).toHaveTextContent(/^SV-PBF-3A story$/);
      expect(rows[0]).toHaveAttribute("aria-selected", "true");
    },
  );
  it("ranks id prefixes before titles and caps after ranking", () => {
    setup(
      [
        ...Array.from({ length: 10 }, (_, i) => story(`X-${i}`, "SV-PBF-3 improvements")),
        story("SV-PBF-30"),
      ],
      "sv-pbf-3",
    );
    expect(screen.getAllByRole("option")[0]).toHaveTextContent("SV-PBF-30");
  });
  it("finds a title fragment and picks it with Enter", () => {
    const { input, onChange } = setup(
      [story("SV-2", "Story workspace picker finds the right stories")],
      "workspace picker",
    );
    expect(screen.getByRole("option")).toHaveTextContent("SV-2");
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onChange).toHaveBeenCalledWith("SV-2");
    expect(screen.queryByRole("listbox")).toBeNull();
  });
  it("keeps a deliberate highlight by identity after reordering, then falls back when removed", () => {
    const { input, rerender, onChange } = setup([story("A"), story("B"), story("C")]);
    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "ArrowDown" });
    rerender(<StoryInput value="" stories={[story("C"), story("A")]} onChange={onChange} />);
    expect(screen.getByRole("option", { selected: true })).toHaveTextContent("CA story");
    rerender(<StoryInput value="" stories={[story("A")]} onChange={onChange} />);
    expect(screen.getByRole("option", { selected: true })).toHaveTextContent("AA story");
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onChange).toHaveBeenCalledWith("A");
  });
  it("Escape closes results and consumes the key", () => {
    const { input } = setup();
    expect(fireEvent.keyDown(input, { key: "Escape" })).toBe(false);
    expect(screen.queryByRole("listbox")).toBeNull();
  });
});
