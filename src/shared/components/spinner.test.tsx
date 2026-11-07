// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { LoadingState, Spinner, WAIT_ESCAPE_AFTER_MS } from "./spinner";

describe("Spinner", () => {
  it("is a polite status with its label", () => {
    render(<Spinner label="Cloning acme/context…" />);
    const status = screen.getByRole("status");
    expect(status).toHaveTextContent("Cloning acme/context…");
    expect(status).toHaveAttribute("data-slot", "spinner");
  });

  it("spins only when motion is allowed", () => {
    render(<Spinner label="Loading…" />);
    const svg = screen.getByRole("status").querySelector("svg");
    expect(svg?.getAttribute("class")).toContain("motion-safe:animate-spin");
    expect(svg?.getAttribute("class")).not.toMatch(/(^|\s)animate-spin/);
  });

  it("names itself for assistive tech when it has no visible label", () => {
    render(<Spinner />);
    expect(screen.getByRole("status")).toHaveAccessibleName("Loading…");
  });

  it("can be just a hidden icon inside a control that already names its work", () => {
    render(
      <button type="button">
        <Spinner decorative testId="s" />
        Sign in
      </button>,
    );
    expect(screen.getByRole("button")).toHaveAccessibleName("Sign in");
    expect(screen.queryByRole("status")).toBeNull();
    expect(screen.getByTestId("s").getAttribute("class")).toContain("motion-safe:animate-spin");
  });

  it("writes an ellipsis, never three dots", () => {
    render(<Spinner label="Loading..." />);
    expect(screen.getByRole("status")).toHaveTextContent("Loading…");
  });
});

describe("LoadingState", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("shows the label and offers no way out before the wait gets long", () => {
    render(<LoadingState label="Loading…" escape={{ label: "Cancel", onClick: () => {} }} />);
    expect(screen.getByRole("status")).toHaveTextContent("Loading…");
    expect(screen.queryByRole("button", { name: "Cancel" })).toBeNull();
  });

  it("offers the way out once the wait passes 30 seconds", async () => {
    const onEscape = vi.fn();
    render(<LoadingState label="Loading…" escape={{ label: "Cancel", onClick: onEscape }} />);
    act(() => {
      vi.advanceTimersByTime(WAIT_ESCAPE_AFTER_MS);
    });
    expect(WAIT_ESCAPE_AFTER_MS).toBe(30_000);
    vi.useRealTimers();
    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onEscape).toHaveBeenCalledTimes(1);
  });

  it("marks itself as the shared loading state", () => {
    render(<LoadingState label="Loading…" testId="ls" />);
    expect(screen.getByTestId("ls")).toHaveAttribute("data-slot", "loading-state");
  });
});
