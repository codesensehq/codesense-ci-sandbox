// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { ScratchSelectionMenu } from "./scratch-selection-menu";

const rect = { top: 100, left: 50, width: 40, height: 18 };
const noMarks = { bold: false, italic: false, code: false };

function setup(overrides: Partial<Parameters<typeof ScratchSelectionMenu>[0]> = {}) {
  const handlers = {
    onToggleBold: vi.fn(),
    onToggleItalic: vi.fn(),
    onToggleCode: vi.fn(),
    onSend: vi.fn(),
    onDismiss: vi.fn(),
  };
  render(<ScratchSelectionMenu open rect={rect} marks={noMarks} {...handlers} {...overrides} />);
  return handlers;
}

describe("ScratchSelectionMenu (SW-SN-8 AC-4)", () => {
  it("renders the four formatting/send actions when open", () => {
    setup();
    expect(screen.getByLabelText("Bold")).toBeInTheDocument();
    expect(screen.getByLabelText("Italic")).toBeInTheDocument();
    expect(screen.getByLabelText("Code")).toBeInTheDocument();
    expect(screen.getByLabelText("Send to…")).toBeInTheDocument();
  });

  it("does not render content when closed", () => {
    render(
      <ScratchSelectionMenu
        open={false}
        rect={rect}
        marks={noMarks}
        onToggleBold={vi.fn()}
        onToggleItalic={vi.fn()}
        onToggleCode={vi.fn()}
        onSend={vi.fn()}
        onDismiss={vi.fn()}
      />,
    );
    expect(screen.queryByLabelText("Send to…")).not.toBeInTheDocument();
  });

  it("does not render when there is no anchor rect", () => {
    render(
      <ScratchSelectionMenu
        open
        rect={null}
        marks={noMarks}
        onToggleBold={vi.fn()}
        onToggleItalic={vi.fn()}
        onToggleCode={vi.fn()}
        onSend={vi.fn()}
        onDismiss={vi.fn()}
      />,
    );
    expect(screen.queryByLabelText("Bold")).not.toBeInTheDocument();
  });

  it("fires onSend when Send to… is clicked", () => {
    const h = setup();
    fireEvent.click(screen.getByLabelText("Send to…"));
    expect(h.onSend).toHaveBeenCalledTimes(1);
  });

  it("fires onToggleBold when Bold is clicked", () => {
    const h = setup();
    fireEvent.click(screen.getByLabelText("Bold"));
    expect(h.onToggleBold).toHaveBeenCalledTimes(1);
  });

  it("reflects active marks via aria-pressed", () => {
    setup({ marks: { bold: true, italic: false, code: false } });
    expect(screen.getByLabelText("Bold")).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByLabelText("Italic")).toHaveAttribute("aria-pressed", "false");
  });
});
