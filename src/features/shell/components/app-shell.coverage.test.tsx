// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";

import { AppShell } from "./app-shell";

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ startDragging: vi.fn() }),
}));

describe("AppShell", () => {
  it("renders the TitleBar at the top of the shell", () => {
    const { container } = render(
      <AppShell>
        <div data-testid="body">body</div>
      </AppShell>,
    );
    const shell = container.firstChild as HTMLElement;
    expect(shell).toHaveClass("flex", "flex-col", "h-screen");
    // TitleBar is the first flex child.
    expect(shell.firstElementChild).toHaveAttribute("data-testid", "title-bar");
  });

  it("renders children after the TitleBar", () => {
    render(
      <AppShell>
        <div data-testid="body">body-content</div>
      </AppShell>,
    );
    expect(screen.getByTestId("body")).toBeInTheDocument();
    expect(screen.getByTestId("body")).toHaveTextContent("body-content");
  });

  it("applies the shell background/foreground tokens", () => {
    const { container } = render(
      <AppShell>
        <div />
      </AppShell>,
    );
    expect(container.firstChild).toHaveClass("bg-background", "text-foreground");
  });
});
