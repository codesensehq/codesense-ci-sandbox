// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import {
  computeSelectionMenu,
  readActiveMarks,
  type SelectionReadableEditor,
} from "./scratch-selection";

function fakeEditor(
  from: number,
  to: number,
  text: string,
  active: Record<string, boolean> = {},
): SelectionReadableEditor {
  return {
    state: {
      selection: { from, to },
      doc: { textBetween: () => text },
    },
    isActive: (name: string) => active[name] ?? false,
  };
}

describe("computeSelectionMenu (SW-SN-8 AC-4)", () => {
  it("returns null for a collapsed selection (typing case → popover hidden)", () => {
    expect(computeSelectionMenu(fakeEditor(5, 5, ""))).toBeNull();
  });

  it("returns null for a whitespace-only selection", () => {
    expect(computeSelectionMenu(fakeEditor(1, 4, "   \n "))).toBeNull();
  });

  it("returns the target for a real selection", () => {
    expect(computeSelectionMenu(fakeEditor(1, 6, "hello"))).toEqual({
      from: 1,
      to: 6,
      text: "hello",
    });
  });
});

describe("readActiveMarks (SW-SN-8 AC-4)", () => {
  it("reflects active inline marks", () => {
    expect(readActiveMarks(fakeEditor(1, 6, "x", { bold: true, code: true }))).toEqual({
      bold: true,
      italic: false,
      code: true,
    });
  });
});
