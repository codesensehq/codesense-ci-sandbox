// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";

import { BOOT_SPLASH_WATCHDOG_MS, BootProblemView, WatchedSplash } from "./boot-wait";

const { revealItemInDir, getLogFilePath } = vi.hoisted(() => ({
  revealItemInDir: vi.fn().mockResolvedValue(undefined),
  getLogFilePath: vi.fn().mockResolvedValue("/logs/codesense.log"),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ revealItemInDir }));
vi.mock("@/tauri/commands", () => ({ getLogFilePath }));

describe("SW-FU-B3: no splash without a watchdog", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("shows the splash, then after the deadline says what it waits on with a way out", () => {
    render(<WatchedSplash stage="discovery" />);
    expect(screen.getByText("CodeSense")).toBeInTheDocument();
    expect(screen.queryByTestId("boot-problem")).not.toBeInTheDocument();

    act(() => {
      vi.advanceTimersByTime(BOOT_SPLASH_WATCHDOG_MS);
    });

    expect(screen.getByRole("heading", { level: 2 })).toHaveTextContent("Still loading your teams");
    expect(screen.getByRole("button", { name: "Try again" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Open logs" })).toBeInTheDocument();
  });

  it("the deadline spans both backend gates (discovery, then the first-run create)", () => {
    const { rerender } = render(<WatchedSplash stage="discovery" />);
    act(() => {
      vi.advanceTimersByTime(BOOT_SPLASH_WATCHDOG_MS - 1000);
    });
    rerender(<WatchedSplash stage="workspace" />);
    act(() => {
      vi.advanceTimersByTime(1000);
    });
    expect(screen.getByRole("heading", { level: 2 })).toHaveTextContent(
      "Still setting up your team",
    );
  });
});

describe("BootProblemView", () => {
  it("Try again runs the retry; Open logs reveals the log file", async () => {
    const onRetry = vi.fn();
    render(
      <BootProblemView heading="Couldn't set up your team" body="disk full" onRetry={onRetry} />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(onRetry).toHaveBeenCalledTimes(1);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Open logs" }));
    });
    expect(revealItemInDir).toHaveBeenCalledWith("/logs/codesense.log");
  });
});

describe("SW-FU-C1: boot states use the shared pieces", () => {
  it("the splash shows the shared brand mark and the problem view is the shared empty state", () => {
    const { unmount } = render(<WatchedSplash stage="startup" />);
    expect(document.querySelector('[data-slot="brand-mark"]')).not.toBeNull();
    unmount();
    render(
      <BootProblemView heading="Couldn't start" body="Something went wrong." onRetry={() => {}} />,
    );
    const problem = screen.getByTestId("boot-problem");
    expect(problem.querySelector('[data-slot="empty-state"]')).not.toBeNull();
    expect(screen.getByRole("button", { name: "Try again" })).toHaveAttribute(
      "data-variant",
      "default",
    );
  });
});
