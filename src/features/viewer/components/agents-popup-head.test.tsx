// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";

import { fireEvent, render, screen } from "@/test-utils";
import { PopupHead } from "./agents-popup-head";

describe("PopupHead", () => {
  it("names the agent, its status and its workspace, with the question it waits on above", () => {
    const { container } = render(
      <PopupHead
        name="Worker"
        statusWord="Working · 2 subagents"
        workspace="Auth flow"
        ask="Ship it?"
        onClose={() => {}}
      />,
    );
    const ask = screen.getByRole("note", { name: "Worker asks" });
    expect(ask).toHaveTextContent("Ship it?");
    expect(container.querySelector(".ag-quick-send-name")).toHaveTextContent("Worker");
    expect(container.querySelector(".ag-quick-send-meta")).toHaveTextContent(
      "Working · 2 subagents · Auth flow",
    );
    // The question comes first, before the head.
    expect(ask.compareDocumentPosition(container.querySelector(".ag-quick-send-head")!)).toBe(
      Node.DOCUMENT_POSITION_FOLLOWING,
    );
  });

  it("without a workspace or a question, shows neither", () => {
    const { container } = render(
      <PopupHead name="Worker" statusWord="Ready" workspace={null} onClose={() => {}} />,
    );
    expect(screen.queryByRole("note")).toBeNull();
    expect(container.querySelector(".ag-quick-send-meta")!.textContent).toBe("Ready");
  });

  it("draws its actions before the close button, which closes it", () => {
    const onClose = vi.fn();
    render(
      <PopupHead name="Worker" statusWord="Ready" workspace={null} onClose={onClose}>
        <button type="button">Open in CLI</button>
      </PopupHead>,
    );
    const buttons = screen.getAllByRole("button");
    expect(buttons.map((b) => b.getAttribute("aria-label") ?? b.textContent)).toEqual([
      "Open in CLI",
      "Close",
    ]);
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
