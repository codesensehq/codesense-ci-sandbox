// SPDX-License-Identifier: MIT
import { useState } from "react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { StatusSelectorPopover } from "./status-selector-popover";

describe("StatusSelectorPopover", () => {
  it("renders children as trigger", () => {
    render(
      <StatusSelectorPopover
        currentStatus="todo"
        onSelect={vi.fn()}
        open={false}
        onOpenChange={vi.fn()}
      >
        <span>trigger</span>
      </StatusSelectorPopover>,
    );
    expect(screen.getByText("trigger")).toBeInTheDocument();
  });

  it("does not show menu when closed", () => {
    render(
      <StatusSelectorPopover
        currentStatus="todo"
        onSelect={vi.fn()}
        open={false}
        onOpenChange={vi.fn()}
      >
        <span>trigger</span>
      </StatusSelectorPopover>,
    );
    expect(screen.queryByText("In progress")).not.toBeInTheDocument();
  });

  it("shows all status options when open", () => {
    render(
      <StatusSelectorPopover
        currentStatus="todo"
        onSelect={vi.fn()}
        open={true}
        onOpenChange={vi.fn()}
      >
        <span>trigger</span>
      </StatusSelectorPopover>,
    );
    expect(screen.getByText("Planning")).toBeInTheDocument();
    expect(screen.getByText("Todo")).toBeInTheDocument();
    expect(screen.getByText("In progress")).toBeInTheDocument();
    expect(screen.getByText("Blocked")).toBeInTheDocument();
    expect(screen.getByText("Review")).toBeInTheDocument();
    expect(screen.getByText("QA")).toBeInTheDocument();
    expect(screen.getByText("Done")).toBeInTheDocument();
  });

  it("announces the trigger as opening its status menu", () => {
    render(
      <StatusSelectorPopover
        currentStatus="todo"
        onSelect={vi.fn()}
        open
        onOpenChange={vi.fn()}
        triggerAriaLabel="Change status"
      >
        <span>trigger</span>
      </StatusSelectorPopover>,
    );
    const trigger = screen.getByRole("button", { name: "Change status" });
    const menu = screen.getByRole("menu", { name: "Status" });
    expect(trigger).toHaveAttribute("aria-haspopup", "menu");
    expect(trigger).toHaveAttribute("aria-controls", menu.id);
  });

  it("calls onSelect when a different status is clicked", () => {
    const onSelect = vi.fn();
    render(
      <StatusSelectorPopover
        currentStatus="todo"
        onSelect={onSelect}
        open={true}
        onOpenChange={vi.fn()}
      >
        <span>trigger</span>
      </StatusSelectorPopover>,
    );
    fireEvent.click(screen.getByText("In progress"));
    expect(onSelect).toHaveBeenCalledWith("in-progress");
  });

  it("does not call onSelect when clicking current status", () => {
    const onSelect = vi.fn();
    render(
      <StatusSelectorPopover
        currentStatus="todo"
        onSelect={onSelect}
        open={true}
        onOpenChange={vi.fn()}
      >
        <span>trigger</span>
      </StatusSelectorPopover>,
    );
    fireEvent.click(screen.getByText("Todo"));
    expect(onSelect).not.toHaveBeenCalled();
  });

  it("calls onOpenChange when trigger is clicked", () => {
    const onOpenChange = vi.fn();
    render(
      <StatusSelectorPopover
        currentStatus="todo"
        onSelect={vi.fn()}
        open={false}
        onOpenChange={onOpenChange}
      >
        <span>trigger</span>
      </StatusSelectorPopover>,
    );
    fireEvent.click(screen.getByText("trigger"));
    expect(onOpenChange).toHaveBeenCalledWith(true);
  });

  it("does not open when disabled", () => {
    const onOpenChange = vi.fn();
    render(
      <StatusSelectorPopover
        currentStatus="todo"
        onSelect={vi.fn()}
        open={false}
        onOpenChange={onOpenChange}
        disabled
      >
        <span>trigger</span>
      </StatusSelectorPopover>,
    );
    fireEvent.click(screen.getByText("trigger"));
    expect(onOpenChange).not.toHaveBeenCalled();
  });

  it("closes on Escape key", () => {
    const onOpenChange = vi.fn();
    render(
      <StatusSelectorPopover
        currentStatus="todo"
        onSelect={vi.fn()}
        open={true}
        onOpenChange={onOpenChange}
      >
        <span>trigger</span>
      </StatusSelectorPopover>,
    );
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  // SW-FX-81: the trigger's contract — an accessible, keyboard-activable button
  // that hugs its child (never full-width) and swallows its own click so the
  // surrounding row doesn't also react. Guarded here at the source so a future
  // change to the shared trigger can't silently break any of the 5 consumers.
  describe("SW-FX-81 — trigger is an accessible, hugging button", () => {
    it("stays a focusable, aria-labelled <button> that hugs its child", () => {
      render(
        <StatusSelectorPopover
          currentStatus="todo"
          onSelect={vi.fn()}
          open={false}
          onOpenChange={vi.fn()}
          triggerAriaLabel="Change status"
        >
          <span>trigger</span>
        </StatusSelectorPopover>,
      );
      const trigger = screen.getByRole("button", { name: "Change status" });
      // A real <button type="button"> — native Enter/Space activation preserved.
      expect(trigger.tagName).toBe("BUTTON");
      expect(trigger).toHaveAttribute("type", "button");
      // Hugs its child, never full-width: a w-full trigger takes the whole
      // flex-basis and starves an adjacent flex-1 label to zero (the nav bug).
      // `shrink-0` is load-bearing too — it stops the icon-sized trigger from
      // compressing, so the title (not the icon) absorbs any truncation.
      expect(trigger.className).toContain("inline-flex");
      expect(trigger.className).toContain("shrink-0");
      expect(trigger.className).not.toContain("w-full");
    });

    it("stops trigger clicks from propagating to ancestor row handlers", () => {
      const onAncestorClick = vi.fn();
      const onOpenChange = vi.fn();
      render(
        <div onClick={onAncestorClick}>
          <StatusSelectorPopover
            currentStatus="todo"
            onSelect={vi.fn()}
            open={false}
            onOpenChange={onOpenChange}
            triggerAriaLabel="Change status"
          >
            <span>trigger</span>
          </StatusSelectorPopover>
        </div>,
      );
      fireEvent.click(screen.getByRole("button", { name: "Change status" }));
      // Positive assertion first: the click actually reached an interactive
      // trigger (toggled the popover) — so the negative below can't pass
      // vacuously via a disabled/non-firing trigger.
      expect(onOpenChange).toHaveBeenCalledWith(true);
      expect(onAncestorClick).not.toHaveBeenCalled();
    });
  });

  describe("portal behavior (SW-FX-79 — always portaled)", () => {
    it("renders dropdown in a portal outside the trigger container", () => {
      const { container } = render(
        <StatusSelectorPopover
          currentStatus="todo"
          onSelect={vi.fn()}
          open={true}
          onOpenChange={vi.fn()}
        >
          <span>trigger</span>
        </StatusSelectorPopover>,
      );
      // Statuses are visible (rendered into a Radix portal at document.body).
      expect(screen.getByText("Planning")).toBeInTheDocument();
      expect(screen.getByText("Done")).toBeInTheDocument();
      // The menu lives in the portal, NOT inside the trigger container — so it can
      // never be clipped by an overflow ancestor.
      expect(container.querySelector("[role='dialog']")).toBeNull();
      expect(document.body.textContent).toContain("In progress");
    });

    it("calls onSelect in portal mode", () => {
      const onSelect = vi.fn();
      render(
        <StatusSelectorPopover
          currentStatus="todo"
          onSelect={onSelect}
          open={true}
          onOpenChange={vi.fn()}
        >
          <span>trigger</span>
        </StatusSelectorPopover>,
      );
      fireEvent.click(screen.getByText("In progress"));
      expect(onSelect).toHaveBeenCalledWith("in-progress");
    });

    it("closes on Escape in portal mode", () => {
      const onOpenChange = vi.fn();
      render(
        <StatusSelectorPopover
          currentStatus="todo"
          onSelect={vi.fn()}
          open={true}
          onOpenChange={onOpenChange}
        >
          <span>trigger</span>
        </StatusSelectorPopover>,
      );
      fireEvent.keyDown(document, { key: "Escape" });
      expect(onOpenChange).toHaveBeenCalledWith(false);
    });

    it("does not render the menu when closed", () => {
      render(
        <StatusSelectorPopover
          currentStatus="todo"
          onSelect={vi.fn()}
          open={false}
          onOpenChange={vi.fn()}
        >
          <span>trigger</span>
        </StatusSelectorPopover>,
      );
      expect(screen.queryByText("In progress")).not.toBeInTheDocument();
    });
  });
});

it("names an off-taxonomy value and marks only committed taxonomy rows", () => {
  const { rerender } = render(
    <StatusSelectorPopover
      currentStatus="needs-triage"
      onSelect={vi.fn()}
      open
      onOpenChange={vi.fn()}
    >
      <span>trigger</span>
    </StatusSelectorPopover>,
  );
  expect(screen.getByRole("menu", { name: "Status, current: needs-triage" })).toHaveTextContent(
    "Current: needs-triage",
  );
  expect(
    screen
      .getAllByRole("menuitemradio")
      .every((row) => row.getAttribute("aria-checked") === "false"),
  ).toBe(true);
  expect(document.querySelector('[data-rail-role="off"]')).toBeInTheDocument();
  const order = screen.getAllByRole("menuitemradio").map((row) => row.textContent);
  rerender(
    <StatusSelectorPopover
      currentStatus="in-progress"
      onSelect={vi.fn()}
      open
      onOpenChange={vi.fn()}
    >
      <span>trigger</span>
    </StatusSelectorPopover>,
  );
  const rows = screen.getAllByRole("menuitemradio");
  expect(rows.map((row) => row.textContent)).toEqual(order);
  expect(screen.getByRole("menuitemradio", { name: "In progress" })).toHaveAttribute(
    "aria-checked",
    "true",
  );
  for (const row of rows) {
    expect(row.querySelector("[data-rail-role]")).toBeInTheDocument();
    expect(row.querySelector("[data-menu-check]")).toBeInTheDocument();
    expect(row.className).toContain(
      "hover:bg-[color-mix(in_srgb,var(--foreground)_9%,transparent)]",
    );
    expect(row.className).toContain("focus-visible:outline-[1.5px]");
    expect(row.querySelector("[data-rail-ready]")).toBeNull();
  }
});

it("commits the focused row with Tab/Enter and dismisses an outside click without writing", async () => {
  const onSelect = vi.fn();
  function Menu() {
    const [open, setOpen] = useState(false);
    return (
      <>
        <button>Outside</button>
        <StatusSelectorPopover
          currentStatus="in-progress"
          onSelect={onSelect}
          open={open}
          onOpenChange={setOpen}
          triggerAriaLabel="Change status"
        >
          <span>trigger</span>
        </StatusSelectorPopover>
      </>
    );
  }
  render(<Menu />);
  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "Change status" }));
  expect(screen.getByRole("menuitemradio", { name: "Planning" })).toHaveFocus();
  await user.tab();
  expect(screen.getByRole("menuitemradio", { name: "Todo" })).toHaveFocus();
  await user.keyboard("{Enter}");
  expect(onSelect).toHaveBeenCalledExactlyOnceWith("todo");
  onSelect.mockClear();
  await user.click(screen.getByRole("button", { name: "Change status" }));
  await user.click(screen.getByRole("button", { name: "Outside" }));
  expect(screen.queryByRole("menu")).toBeNull();
  expect(onSelect).not.toHaveBeenCalled();
});
