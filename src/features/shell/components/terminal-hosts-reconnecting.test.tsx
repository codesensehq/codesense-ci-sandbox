// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";

import { resetTerminalServerForTests } from "../lib/terminal-server";
import { HOSTS_ESCALATE_AFTER_MS, TerminalHostsReconnecting } from "./terminal-server-notice";

const commands = vi.hoisted(() => ({
  terminalServerStatus: vi.fn(),
  restartTerminalServer: vi.fn(),
}));
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  ...commands,
}));

const advance = (ms: number) => act(() => void vi.advanceTimersByTime(ms));

beforeEach(() => {
  vi.useFakeTimers();
  resetTerminalServerForTests();
  commands.restartTerminalServer.mockReset();
});
afterEach(() => vi.useRealTimers());

describe("an unanswering terminal server escalates", () => {
  it("reconnects quietly first, then says it is still not answering, counting", () => {
    render(<TerminalHostsReconnecting />);
    expect(screen.getByText("Reconnecting to the terminal server…")).toBeInTheDocument();
    advance(HOSTS_ESCALATE_AFTER_MS - 1_000);
    expect(screen.queryByText(/still not answering/)).toBeNull();
    expect(screen.queryByRole("button", { name: /Restart/ })).toBeNull();
    advance(1_000);
    expect(screen.getByText("Terminal server still not answering (30s)")).toBeInTheDocument();
    advance(5_000);
    expect(screen.getByText("Terminal server still not answering (35s)")).toBeInTheDocument();
  });

  it("asks before restarting, saying what it does to running terminals; Cancel restarts nothing", () => {
    render(<TerminalHostsReconnecting />);
    advance(HOSTS_ESCALATE_AFTER_MS);
    fireEvent.click(screen.getByRole("button", { name: "Restart terminal server…" }));
    expect(screen.getByText(/closes the terminals it's running/)).toBeInTheDocument();
    expect(screen.getByText(/scrollback is lost/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(commands.restartTerminalServer).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Restart terminal server…" })).toBeInTheDocument();
  });

  it("confirmed, it restarts the terminal server; a failure says so", async () => {
    commands.restartTerminalServer.mockRejectedValue({ oldStopped: false, message: "hung" });
    render(<TerminalHostsReconnecting />);
    advance(HOSTS_ESCALATE_AFTER_MS);
    fireEvent.click(screen.getByRole("button", { name: "Restart terminal server…" }));
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Restart terminal server" }));
    });
    expect(commands.restartTerminalServer).toHaveBeenCalledOnce();
    expect(screen.getByRole("alert")).toHaveTextContent("Couldn't restart the terminal server");
  });

  it("when the old server stopped but no new one started, it says the terminals already stopped", async () => {
    commands.restartTerminalServer.mockRejectedValue({ oldStopped: true, message: "launch" });
    render(<TerminalHostsReconnecting />);
    advance(HOSTS_ESCALATE_AFTER_MS);
    fireEvent.click(screen.getByRole("button", { name: "Restart terminal server…" }));
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Restart terminal server" }));
    });
    const alert = screen.getByRole("alert");
    expect(alert).toHaveTextContent("stopped, and its terminals with it");
    expect(alert).not.toHaveTextContent("Couldn't restart");
  });
});
