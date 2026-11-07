// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";

const { mockInvoke } = vi.hoisted(() => ({ mockInvoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));

import { RESTART_TIMEOUT_MS, RestartToUpdateButton } from "./RestartToUpdateButton";

describe("RestartToUpdateButton (SW-PB-23)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    mockInvoke.mockReset();
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === "license_headless_runs" ? 0 : null,
    );
  });
  afterEach(() => vi.useRealTimers());

  it("resets and says so when the app is still open after about 30 seconds", async () => {
    render(<RestartToUpdateButton testId="b" />);
    await act(async () => {
      fireEvent.click(screen.getByTestId("b"));
    });
    expect(mockInvoke).toHaveBeenCalledWith("license_restart_to_update");
    expect(screen.getByTestId("b")).toHaveTextContent("Restarting...");
    expect(screen.getByTestId("b")).toBeDisabled();
    act(() => {
      vi.advanceTimersByTime(RESTART_TIMEOUT_MS - 1);
    });
    expect(screen.getByTestId("b")).toBeDisabled();
    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(screen.getByTestId("b")).toHaveTextContent("Restart to update");
    expect(screen.getByTestId("b")).not.toBeDisabled();
    expect(screen.getByTestId("b-error")).toHaveTextContent("CodeSense didn't restart.");
    expect(mockInvoke).toHaveBeenCalledWith("license_cancel_restart");
    expect(screen.getByTestId("b")).toHaveFocus();
    expect(RESTART_TIMEOUT_MS).toBe(30_000);
  });

  it("shows the refusal at once and resets", async () => {
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "license_headless_runs") return 0;
      throw "No update is ready to install.";
    });
    render(<RestartToUpdateButton testId="b" />);
    await act(async () => {
      fireEvent.click(screen.getByTestId("b"));
    });
    expect(screen.getByTestId("b-error")).toHaveTextContent("No update is ready to install.");
    expect(screen.getByTestId("b")).not.toBeDisabled();
  });

  it("asks first when agent runs without a terminal would stop, and restarts only on yes", async () => {
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === "license_headless_runs" ? 2 : null,
    );
    render(<RestartToUpdateButton testId="b" />);
    await act(async () => {
      fireEvent.click(screen.getByTestId("b"));
    });
    expect(screen.getByTestId("b-confirm")).toHaveTextContent(
      "2 agent runs without a terminal will stop. Restart anyway?",
    );
    expect(mockInvoke).not.toHaveBeenCalledWith("license_restart_to_update");
    fireEvent.click(screen.getByTestId("b-confirm-no"));
    expect(screen.queryByTestId("b-confirm")).not.toBeInTheDocument();
    expect(mockInvoke).not.toHaveBeenCalledWith("license_restart_to_update");
    await act(async () => {
      fireEvent.click(screen.getByTestId("b"));
    });
    await act(async () => {
      fireEvent.click(screen.getByTestId("b-confirm-yes"));
    });
    expect(mockInvoke).toHaveBeenCalledWith("license_restart_to_update");
  });

  it("says one run in the singular", async () => {
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === "license_headless_runs" ? 1 : null,
    );
    render(<RestartToUpdateButton testId="b" />);
    await act(async () => {
      fireEvent.click(screen.getByTestId("b"));
    });
    expect(screen.getByTestId("b-confirm")).toHaveTextContent(
      "1 agent run without a terminal will stop. Restart anyway?",
    );
  });

  it("fails closed: a count that can't be read still asks", async () => {
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "license_headless_runs") throw "no";
      return null;
    });
    render(<RestartToUpdateButton testId="b" />);
    await act(async () => {
      fireEvent.click(screen.getByTestId("b"));
    });
    const dialog = screen.getByRole("alertdialog", {
      name: "Agent runs without a terminal may stop. Restart anyway?",
    });
    expect(dialog).toBeInTheDocument();
    expect(screen.getByTestId("b-confirm-no")).toHaveFocus();
    expect(mockInvoke).not.toHaveBeenCalledWith("license_restart_to_update");
  });

  it("counts again on yes, and asks again when more runs started", async () => {
    let runs = 1;
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === "license_headless_runs" ? runs : null,
    );
    render(<RestartToUpdateButton testId="b" />);
    await act(async () => {
      fireEvent.click(screen.getByTestId("b"));
    });
    expect(screen.getByTestId("b")).toBeDisabled();
    runs = 3;
    await act(async () => {
      fireEvent.click(screen.getByTestId("b-confirm-yes"));
    });
    expect(screen.getByTestId("b-confirm")).toHaveTextContent(
      "3 agent runs without a terminal will stop. Restart anyway?",
    );
    expect(mockInvoke).not.toHaveBeenCalledWith("license_restart_to_update");
    await act(async () => {
      fireEvent.click(screen.getByTestId("b-confirm-yes"));
    });
    expect(mockInvoke).toHaveBeenCalledWith("license_restart_to_update");
  });

  it("is disabled while it counts", async () => {
    let finish: (n: number) => void = () => {};
    mockInvoke.mockImplementation((cmd: string) =>
      cmd === "license_headless_runs"
        ? new Promise<number>((resolve) => {
            finish = resolve;
          })
        : Promise.resolve(null),
    );
    render(<RestartToUpdateButton testId="b" />);
    act(() => {
      fireEvent.click(screen.getByTestId("b"));
    });
    expect(screen.getByTestId("b")).toBeDisabled();
    await act(async () => {
      finish(0);
    });
    expect(mockInvoke).toHaveBeenCalledWith("license_restart_to_update");
  });

  it("Escape dismisses the question and gives focus back to the button", async () => {
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === "license_headless_runs" ? 2 : null,
    );
    render(<RestartToUpdateButton testId="b" />);
    await act(async () => {
      fireEvent.click(screen.getByTestId("b"));
    });
    fireEvent.keyDown(screen.getByTestId("b-confirm"), { key: "Escape" });
    expect(screen.queryByTestId("b-confirm")).not.toBeInTheDocument();
    expect(screen.getByTestId("b")).toHaveFocus();
    expect(mockInvoke).not.toHaveBeenCalledWith("license_restart_to_update");
  });

  it("a cancel during the recount wins", async () => {
    let finish: (n: number) => void = () => {};
    let calls = 0;
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd !== "license_headless_runs") return Promise.resolve(null);
      calls += 1;
      if (calls === 1) return Promise.resolve(1);
      return new Promise<number>((resolve) => {
        finish = resolve;
      });
    });
    render(<RestartToUpdateButton testId="b" />);
    await act(async () => {
      fireEvent.click(screen.getByTestId("b"));
    });
    act(() => {
      fireEvent.click(screen.getByTestId("b-confirm-yes"));
    });
    fireEvent.click(screen.getByTestId("b-confirm-no"));
    await act(async () => {
      finish(0);
    });
    expect(mockInvoke).not.toHaveBeenCalledWith("license_restart_to_update");
  });

  it("announces a changed count", async () => {
    let runs = 1;
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === "license_headless_runs" ? runs : null,
    );
    render(<RestartToUpdateButton testId="b" />);
    await act(async () => {
      fireEvent.click(screen.getByTestId("b"));
    });
    runs = 3;
    await act(async () => {
      fireEvent.click(screen.getByTestId("b-confirm-yes"));
    });
    const text = screen.getByText("3 agent runs without a terminal will stop. Restart anyway?");
    expect(text).toHaveAttribute("aria-live", "polite");
  });

  it("gives focus back to the button after a refused restart", async () => {
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "license_headless_runs") return 0;
      throw "No update is ready to install.";
    });
    render(<RestartToUpdateButton testId="b" />);
    await act(async () => {
      fireEvent.click(screen.getByTestId("b"));
    });
    expect(screen.getByTestId("b")).toHaveFocus();
  });
});
