// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, act, waitFor } from "@testing-library/react";

import userEvent from "@testing-library/user-event";

import {
  MetadataDateField,
  MetadataOptionSelect,
  MetadataSourceRepoPicker,
  MetadataStatusCombobox,
} from "./metadata-field-controls";
import type { SourceRepoOption, TagSchema } from "@/tauri/commands";
import { useStatusTaxonomyStore } from "@/shared/stores/status-taxonomy-store";
import { tagDisplay } from "@/shared/lib/tag-schema/tag-display";

const OPTIONS: SourceRepoOption[] = [
  { slug: "acme/api", attached: true },
  { slug: "acme/web", attached: false },
];

describe("MetadataSourceRepoPicker (AC-5)", () => {
  it("pins the invalid current value at the top, flagged + still selectable", () => {
    const onChange = vi.fn();
    render(
      <MetadataSourceRepoPicker
        ariaLabel="source_repo"
        value="acme/old"
        options={OPTIONS}
        invalid
        invalidFlag="not recognized"
        onChange={onChange}
      />,
    );

    // Trigger shows the invalid value + the flag.
    const trigger = screen.getByLabelText("source_repo");
    expect(trigger).toHaveTextContent("acme/old");
    expect(trigger).toHaveTextContent(/not recognized/i);

    // Open the picker.
    fireEvent.click(trigger);

    // The invalid value is pinned (appears again as a menu button) and remains
    // selectable — never dropped.
    const pinned = screen
      .getAllByText("acme/old")
      .find((el) => el.closest("button") && el.closest("button") !== trigger);
    expect(pinned).toBeTruthy();

    // Configured repos are listed; attached sorted-first is caller's job but
    // both appear.
    expect(screen.getByText("acme/api")).toBeInTheDocument();
    expect(screen.getByText("acme/web")).toBeInTheDocument();
    // Attached marker present.
    expect(screen.getByText("attached")).toBeInTheDocument();

    // Selecting a valid replacement fires onChange.
    fireEvent.click(screen.getByText("acme/api"));
    expect(onChange).toHaveBeenCalledWith("acme/api");
  });

  it("clears the field (removes the key) via Clear", () => {
    const onChange = vi.fn();
    render(
      <MetadataSourceRepoPicker
        ariaLabel="source_repo"
        value="acme/api"
        options={OPTIONS}
        invalid={false}
        onChange={onChange}
      />,
    );
    fireEvent.click(screen.getByLabelText("source_repo"));
    fireEvent.click(screen.getByText(/Clear \(remove field\)/i));
    expect(onChange).toHaveBeenCalledWith(null);
  });

  it("disables the trigger in read-only mode", () => {
    render(
      <MetadataSourceRepoPicker
        ariaLabel="source_repo"
        value="acme/api"
        options={OPTIONS}
        invalid={false}
        disabled
        onChange={vi.fn()}
      />,
    );
    expect(screen.getByLabelText("source_repo")).toBeDisabled();
  });
});

describe("MetadataDateField (FIX A — ISO datetime seeding)", () => {
  it("shows the date portion of a stored ISO-8601 timestamp (not blank)", () => {
    render(<MetadataDateField ariaLabel="due" value="2026-01-01T00:00:00Z" onChange={vi.fn()} />);
    const input = screen.getByLabelText("due") as HTMLInputElement;
    // Full timestamp would render blank in a date input; we normalize to the
    // date portion so an existing declared date still displays.
    expect(input.value).toBe("2026-01-01");
  });

  it("passes a bare YYYY-MM-DD through unchanged", () => {
    render(<MetadataDateField ariaLabel="due" value="2026-03-15" onChange={vi.fn()} />);
    expect((screen.getByLabelText("due") as HTMLInputElement).value).toBe("2026-03-15");
  });

  it("editing commits a bare YYYY-MM-DD value (does not corrupt on load)", () => {
    const onChange = vi.fn();
    render(<MetadataDateField ariaLabel="due" value="2026-01-01T00:00:00Z" onChange={onChange} />);
    // Load alone must not fire onChange (no silent downgrade of the timestamp).
    expect(onChange).not.toHaveBeenCalled();

    const input = screen.getByLabelText("due");
    fireEvent.change(input, { target: { value: "2026-02-02" } });
    expect(onChange).toHaveBeenCalledWith("2026-02-02");
  });

  it("clearing the date commits null", () => {
    const onChange = vi.fn();
    render(<MetadataDateField ariaLabel="due" value="2026-01-01" onChange={onChange} />);
    fireEvent.change(screen.getByLabelText("due"), { target: { value: "" } });
    expect(onChange).toHaveBeenCalledWith(null);
  });
});

describe("MetadataStatusCombobox (FIX B — blur timer cleanup)", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("does not fire a deferred commit after the component unmounts", () => {
    vi.useFakeTimers();
    const onChange = vi.fn();
    const { unmount } = render(
      <MetadataStatusCombobox ariaLabel="status" value={null} onChange={onChange} />,
    );

    const input = screen.getByLabelText("status");
    fireEvent.focus(input);
    fireEvent.change(input, { target: { value: "in-progress" } });
    fireEvent.blur(input);

    // Unmount within the 100ms deferral window.
    unmount();
    act(() => {
      vi.advanceTimersByTime(200);
    });

    // The pending timer was cleared on unmount — no stale commit.
    expect(onChange).not.toHaveBeenCalled();
  });

  it("still commits the draft on blur when it stays mounted", () => {
    vi.useFakeTimers();
    const onChange = vi.fn();
    render(<MetadataStatusCombobox ariaLabel="status" value={null} onChange={onChange} />);

    const input = screen.getByLabelText("status");
    fireEvent.focus(input);
    fireEvent.change(input, { target: { value: "in-progress" } });
    fireEvent.blur(input);
    act(() => {
      vi.advanceTimersByTime(200);
    });

    expect(onChange).toHaveBeenCalledWith("in-progress");
  });
});

describe("MetadataStatusCombobox keyboard suggestions", () => {
  beforeEach(() => {
    useStatusTaxonomyStore.getState().setStates([
      {
        id: "design",
        label: "Needs design",
        role: "not_started",
        order: 0,
        color: "red",
        icon: "Circle",
      },
      { id: "build", label: "Under way", role: "active", order: 1, color: "blue", icon: "Circle" },
      {
        id: "build-review",
        label: "Review",
        role: "active",
        order: 2,
        color: "blue",
        icon: "Circle",
      },
    ]);
  });

  it("wraps both arrows from no highlight and keeps focus on the input", async () => {
    const user = userEvent.setup();
    render(<MetadataStatusCombobox ariaLabel="status" value={null} onChange={vi.fn()} />);
    const input = screen.getByRole("combobox");
    await user.click(input);
    const options = screen.getAllByRole("option");
    expect(input).not.toHaveAttribute("aria-activedescendant");
    expect(options.every((row) => row.getAttribute("aria-selected") === "false")).toBe(true);

    for (const [key, index] of [
      ["{ArrowDown}", 0],
      ["{ArrowUp}", 2],
      ["{ArrowDown}", 0],
      ["{ArrowDown}", 1],
      ["{ArrowDown}", 2],
      ["{ArrowDown}", 0],
    ] as const) {
      await user.keyboard(key);
      expect(input).toHaveFocus();
      expect(options[index].id).not.toBe("");
      expect(input).toHaveAttribute("aria-activedescendant", options[index].id);
      expect(screen.getByRole("option", { selected: true })).toBe(options[index]);
    }

    await user.type(input, "x");
    await user.clear(input);
    expect(input).not.toHaveAttribute("aria-activedescendant");
    await user.keyboard("{ArrowUp}");
    expect(input).toHaveAttribute("aria-activedescendant", options[2].id);
  });

  it("seeds the saved row on open and reopens with ArrowDown after closing", async () => {
    const user = userEvent.setup();
    render(<MetadataStatusCombobox ariaLabel="status" value="build" onChange={vi.fn()} />);
    const input = screen.getByRole("combobox");
    await user.click(input);
    const saved = screen.getByRole("option", { name: /Under way/ });
    const savedId = saved.id;
    expect(input).toHaveAttribute("aria-activedescendant", savedId);
    expect(saved).toHaveAttribute("aria-selected", "true");
    await user.keyboard("{ArrowDown}");
    expect(saved).toHaveAttribute("aria-selected", "false");
    expect(saved.querySelector("[data-menu-check] svg")).toBeInTheDocument();
    expect(
      screen.getByRole("option", { name: /Review/ }).querySelector("[data-menu-check] svg"),
    ).toBeNull();
    await user.keyboard("{Escape}");
    expect(input).not.toHaveAttribute("aria-activedescendant");
    expect(input).toHaveAttribute("aria-expanded", "false");
    await user.keyboard("{ArrowDown}");
    expect(input).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByRole("option", { selected: true }).id).toBe(savedId);
    expect(input).toHaveFocus();
  });

  it("seeds the current saved row after the value changes while closed", async () => {
    const user = userEvent.setup();
    const { rerender } = render(
      <MetadataStatusCombobox ariaLabel="status" value="design" onChange={vi.fn()} />,
    );
    rerender(<MetadataStatusCombobox ariaLabel="status" value="build" onChange={vi.fn()} />);
    const input = screen.getByRole("combobox");
    await user.click(input);
    expect(input).toHaveValue("build");
    const saved = screen.getByRole("option", { name: /Under way/ });
    expect(saved).toHaveAttribute("aria-selected", "true");
    expect(input).toHaveAttribute("aria-activedescendant", saved.id);
  });

  it("commits the highlighted state id instead of the draft or label", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<MetadataStatusCombobox ariaLabel="status" value={null} onChange={onChange} />);
    const input = screen.getByRole("combobox");
    await user.click(input);
    await user.type(input, "Under");
    await user.keyboard("{ArrowDown}{Enter}");
    expect(onChange).toHaveBeenCalledExactlyOnceWith("build");
    expect(input).not.toHaveAttribute("aria-activedescendant");
    expect(input).toHaveAttribute("aria-expanded", "false");
  });

  it.each(["Enter", "click"])("keeps a picked id after %s and a later blur", (method) => {
    vi.useFakeTimers();
    try {
      const onChange = vi.fn();
      const { rerender } = render(
        <MetadataStatusCombobox ariaLabel="status" value={null} onChange={onChange} />,
      );
      const input = screen.getByRole("combobox");
      fireEvent.focus(input);
      if (method === "Enter") {
        fireEvent.keyDown(input, { key: "ArrowDown" });
        fireEvent.keyDown(input, { key: "Enter" });
      } else {
        fireEvent.click(screen.getByRole("option", { name: /Needs design/ }));
      }
      expect(onChange).toHaveBeenCalledExactlyOnceWith("design");
      rerender(<MetadataStatusCombobox ariaLabel="status" value="design" onChange={onChange} />);
      expect(input).toHaveValue("design");
      fireEvent.keyDown(input, { key: "Enter" });
      fireEvent.blur(input);
      act(() => {
        vi.advanceTimersByTime(200);
      });
      expect(onChange).toHaveBeenCalledExactlyOnceWith("design");
    } finally {
      vi.useRealTimers();
    }
  });

  it("scrolls the listbox to keep the active option visible when arrows wrap", () => {
    useStatusTaxonomyStore.getState().setStates(
      Array.from({ length: 20 }, (_, index) => ({
        id: `status-${index}`,
        label: `Status ${index}`,
        role: "active",
        order: index,
        color: "blue",
        icon: "Circle",
      })),
    );
    render(<MetadataStatusCombobox ariaLabel="status" value={null} onChange={vi.fn()} />);
    const input = screen.getByRole("combobox");
    fireEvent.focus(input);
    const list = screen.getByRole("listbox");
    const options = screen.getAllByRole("option");
    vi.spyOn(list, "getBoundingClientRect").mockReturnValue(new DOMRect(0, 0, 220, 240));
    vi.spyOn(options[19], "getBoundingClientRect").mockReturnValue(new DOMRect(0, 260, 220, 30));
    vi.spyOn(options[0], "getBoundingClientRect").mockReturnValue(new DOMRect(0, -50, 220, 30));
    fireEvent.keyDown(input, { key: "ArrowUp" });
    expect(list.scrollTop).toBe(50);
    fireEvent.keyDown(input, { key: "ArrowDown" });
    expect(list.scrollTop).toBe(0);
  });

  it("clears the highlight on typing so Enter commits free text", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<MetadataStatusCombobox ariaLabel="status" value={null} onChange={onChange} />);
    const input = screen.getByRole("combobox");
    await user.click(input);
    await user.keyboard("{ArrowDown}");
    await user.type(input, "d");
    expect(input).not.toHaveAttribute("aria-activedescendant");
    expect(screen.queryByRole("option", { selected: true })).toBeNull();
    await user.clear(input);
    await user.type(input, "custom-status");
    expect(input).not.toHaveAttribute("aria-activedescendant");
    expect(screen.getByText(/No matches/)).toBeInTheDocument();
    await user.keyboard("{ArrowDown}{ArrowUp}");
    expect(input).not.toHaveAttribute("aria-activedescendant");
    await user.keyboard("{Enter}");
    expect(onChange).toHaveBeenCalledExactlyOnceWith("custom-status");
  });

  it("reverts on Escape after arrow navigation without writing", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<MetadataStatusCombobox ariaLabel="status" value="build" onChange={onChange} />);
    const input = screen.getByRole("combobox");
    await user.click(input);
    await user.clear(input);
    await user.keyboard("{ArrowDown}");
    expect(input).toHaveAttribute("aria-activedescendant");
    await user.keyboard("{Escape}");
    expect(input).toHaveValue("build");
    expect(input).not.toHaveAttribute("aria-activedescendant");
    expect(screen.queryByRole("listbox")).toBeNull();
    expect(onChange).not.toHaveBeenCalled();
  });

  it("Tab leaves the input and commits the draft rather than the highlighted row", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(
      <>
        <MetadataStatusCombobox ariaLabel="status" value={null} onChange={onChange} />
        <button>Next field</button>
      </>,
    );
    const input = screen.getByRole("combobox");
    await user.click(input);
    await user.type(input, "Under");
    await user.keyboard("{ArrowDown}");
    await user.tab();
    expect(screen.getByRole("button", { name: "Next field" })).toHaveFocus();
    await waitFor(() => expect(onChange).toHaveBeenCalledExactlyOnceWith("Under"));
    expect(input).not.toHaveAttribute("aria-activedescendant");
  });
});

// ---------------------------------------------------------------------------
// SW-DX-1 (AC-10): a schema-declared select/ordered/status tag reuses the
// EXISTING MetadataOptionSelect, fed the tag's `allowedValues` as `options` —
// no new picker widget. (multi_select stays the free-text MetadataListChipEditor
// with constraint via the validation badge; a constrained multi-picker is a
// follow-up.) The schema also feeds `tagDisplay` for chip color/icon.
// ---------------------------------------------------------------------------

const TAG_SCHEMA: TagSchema = {
  tags: [
    {
      name: "context_level",
      type: "ordered",
      allowedValues: ["machine", "team", "management", "leadership"],
    },
    { name: "priority", type: "select", allowedValues: ["low", "high"] },
  ],
};

describe("MetadataOptionSelect fed a schema tag's allowedValues (SW-DX-1 AC-10)", () => {
  it("renders the ordered tag's allowedValues as the constrained options", () => {
    const onChange = vi.fn();
    const def = TAG_SCHEMA.tags.find((t) => t.name === "context_level")!;
    render(
      <MetadataOptionSelect
        ariaLabel="context_level"
        value={null}
        options={def.allowedValues}
        onChange={onChange}
      />,
    );

    // Open the select.
    fireEvent.click(screen.getByLabelText("context_level"));

    // Every allowed value renders as an option, in authored (ladder) order.
    for (const v of def.allowedValues) {
      expect(screen.getByText(v)).toBeInTheDocument();
    }

    // Selecting a schema value fires onChange with that value.
    fireEvent.click(screen.getByText("management"));
    expect(onChange).toHaveBeenCalledWith("management");
  });

  it("supplies chip color/icon for the same tag from the one tagDisplay source", () => {
    // The dropdown option source (allowedValues) and the chip render config
    // (color/icon) come from ONE schema — no forked maps.
    const d = tagDisplay(TAG_SCHEMA, "context_level");
    expect(d.color).toBe("var(--cs-tag-ordered)");
    expect(d.icon).toBe("ArrowUpNarrowWide");
    expect(d.label).toBe("Context level");
  });
});

it("status suggestions use live labels, filter by id and label, and commit ids or free text", () => {
  useStatusTaxonomyStore.getState().setStates([
    {
      id: "design",
      label: "Needs design",
      role: "not_started",
      order: 0,
      color: "red",
      icon: "Circle",
    },
    { id: "building", label: "Under way", role: "active", order: 1, color: "blue", icon: "Circle" },
  ]);
  try {
    const onChange = vi.fn();
    const { container } = render(
      <MetadataStatusCombobox ariaLabel="status" value="triage" onChange={onChange} />,
    );
    expect(container.querySelector('[data-rail-role="off"]')).toBeInTheDocument();
    const input = screen.getByLabelText("status");
    fireEvent.focus(input);
    fireEvent.change(input, { target: { value: "" } });
    expect(screen.getAllByRole("option").map((row) => row.textContent)).toEqual([
      "Needs designdesign",
      "Under waybuilding",
    ]);
    fireEvent.change(input, { target: { value: "Under" } });
    expect(screen.getAllByRole("option")).toHaveLength(1);
    fireEvent.change(input, { target: { value: "design" } });
    fireEvent.click(screen.getByRole("option", { name: /Needs design/ }));
    expect(onChange).toHaveBeenCalledWith("design");
    fireEvent.focus(input);
    fireEvent.change(input, { target: { value: "triage-new" } });
    expect(screen.getByText(/No matches/)).toBeInTheDocument();
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onChange).toHaveBeenLastCalledWith("triage-new");
  } finally {
    useStatusTaxonomyStore.getState().resetForTest();
  }
});
