// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { FolderOpen } from "lucide-react";

import { EmptyState } from "./empty-state";

describe("EmptyState", () => {
  it("renders icon, title, body and the one primary action", () => {
    render(
      <EmptyState
        testId="es"
        icon={<FolderOpen data-testid="es-icon" />}
        title="No repos yet"
        body="Add one to start."
        action={{ label: "Add repo", onClick: () => {} }}
      />,
    );
    expect(screen.getByTestId("es-icon")).toBeInTheDocument();
    expect(screen.getByText("No repos yet")).toBeInTheDocument();
    expect(screen.getByText("Add one to start.")).toBeInTheDocument();
    const button = screen.getByRole("button", { name: "Add repo" });
    // The primary look is the shared button's default variant.
    expect(button).toHaveAttribute("data-slot", "button");
    expect(button).toHaveAttribute("data-variant", "default");
  });

  it("fires the primary action", async () => {
    const onClick = vi.fn();
    render(<EmptyState title="t" action={{ label: "Go", onClick, testId: "go" }} />);
    await userEvent.click(screen.getByTestId("go"));
    expect(onClick).toHaveBeenCalledTimes(1);
  });

  it("renders the optional secondary action as a link-style button, not a second primary", async () => {
    const onSecondary = vi.fn();
    render(
      <EmptyState
        title="t"
        action={{ label: "Go", onClick: () => {} }}
        secondaryAction={{ label: "Open logs", onClick: onSecondary }}
      />,
    );
    const secondary = screen.getByRole("button", { name: "Open logs" });
    expect(secondary).toHaveAttribute("data-variant", "link");
    await userEvent.click(secondary);
    expect(onSecondary).toHaveBeenCalledTimes(1);
    expect(document.querySelectorAll('[data-variant="default"]')).toHaveLength(1);
  });

  it("renders a short row of secondary links, still one primary", () => {
    render(
      <EmptyState
        title="t"
        action={{ label: "Open workspaces", onClick: () => {} }}
        secondaryAction={[
          { label: "Join your team", onClick: () => {} },
          { label: "Create new workspace", onClick: () => {} },
        ]}
      />,
    );
    expect(screen.getByRole("button", { name: "Join your team" })).toHaveAttribute(
      "data-variant",
      "link",
    );
    expect(screen.getByRole("button", { name: "Create new workspace" })).toHaveAttribute(
      "data-variant",
      "link",
    );
    expect(document.querySelectorAll('[data-variant="default"]')).toHaveLength(1);
  });

  it("never renders nothing: a title alone is a valid state", () => {
    render(<EmptyState testId="bare" title="Nothing needs you right now." />);
    expect(screen.getByTestId("bare")).toHaveTextContent("Nothing needs you right now.");
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("marks itself with a shared slot so surfaces can be checked for it", () => {
    render(<EmptyState testId="slot" title="t" />);
    expect(screen.getByTestId("slot")).toHaveAttribute("data-slot", "empty-state");
  });

  it("disables the primary action when asked", () => {
    render(<EmptyState title="t" action={{ label: "Go", onClick: () => {}, disabled: true }} />);
    expect(screen.getByRole("button", { name: "Go" })).toBeDisabled();
  });

  it("renders extra content below the actions", () => {
    render(
      <EmptyState title="t">
        <span data-testid="extra">more</span>
      </EmptyState>,
    );
    expect(screen.getByTestId("extra")).toBeInTheDocument();
  });
});
