// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act, cleanup } from "@testing-library/react";
import { NoteLinkPicker } from "./note-link-picker";
import type { NoteLinkPickerState } from "../lib/note-link-suggestion";
import type { PickerItem } from "../lib/note-link-picker-items";

const items: PickerItem[] = [
  { key: "note:n1", label: "Prompts", kind: "note", link: { kind: "note", target: "n1" } },
  {
    key: "story:SW-SN-2",
    label: "The rail",
    sublabel: "SW-SN-2",
    kind: "story",
    link: { kind: "story", target: "SW-SN-2" },
  },
];

const RECT = { left: 10, bottom: 20 } as DOMRect;

function makeState(over: Partial<NoteLinkPickerState> = {}): NoteLinkPickerState {
  return { items, clientRect: () => RECT, select: vi.fn(), ...over };
}

beforeEach(() => cleanup());

describe("NoteLinkPicker (SW-SN-9 Task 2 / AC-1, AC-7)", () => {
  it("renders a listbox of options with the first selected", () => {
    render(<NoteLinkPicker state={makeState()} registerKeyHandler={() => {}} />);
    expect(screen.getByRole("listbox")).toBeInTheDocument();
    const opts = screen.getAllByRole("option");
    expect(opts).toHaveLength(2);
    expect(opts[0]).toHaveAttribute("aria-selected", "true");
    expect(opts[1]).toHaveAttribute("aria-selected", "false");
  });

  it("ArrowDown + Enter selects the highlighted item via the forwarded keydown", () => {
    const state = makeState();
    let keydown: ((e: KeyboardEvent) => boolean) | null = null;
    render(<NoteLinkPicker state={state} registerKeyHandler={(fn) => (keydown = fn)} />);

    act(() => {
      expect(keydown!(new KeyboardEvent("keydown", { key: "ArrowDown" }))).toBe(true);
    });
    act(() => {
      expect(keydown!(new KeyboardEvent("keydown", { key: "Enter" }))).toBe(true);
    });
    expect(state.select).toHaveBeenCalledWith(items[1]);
  });

  it("clicking an option selects it", () => {
    const state = makeState();
    render(<NoteLinkPicker state={state} registerKeyHandler={() => {}} />);
    fireEvent.click(screen.getAllByRole("option")[0]);
    expect(state.select).toHaveBeenCalledWith(items[0]);
  });

  it("does not consume Escape or printable keys (the suggestion plugin owns exit)", () => {
    const state = makeState();
    let keydown: ((e: KeyboardEvent) => boolean) | null = null;
    render(<NoteLinkPicker state={state} registerKeyHandler={(fn) => (keydown = fn)} />);
    // Non-nav keys fall through so the editor keeps handling them; Escape is left
    // to the plugin (which exits and unmounts this popup via onChange(null)).
    expect(keydown!(new KeyboardEvent("keydown", { key: "a" }))).toBe(false);
    expect(keydown!(new KeyboardEvent("keydown", { key: "Escape" }))).toBe(false);
  });

  it("renders nothing when there are no items or no caret rect", () => {
    const { container: c1 } = render(
      <NoteLinkPicker state={makeState({ items: [] })} registerKeyHandler={() => {}} />,
    );
    expect(c1.querySelector('[role="listbox"]')).toBeNull();
    cleanup();
    const { container: c2 } = render(
      <NoteLinkPicker
        state={makeState({ clientRect: () => null })}
        registerKeyHandler={() => {}}
      />,
    );
    expect(c2.querySelector('[role="listbox"]')).toBeNull();
  });
});
