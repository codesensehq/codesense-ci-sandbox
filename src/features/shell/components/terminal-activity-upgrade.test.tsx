// SPDX-License-Identifier: MIT
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  TerminalActivityUpgradeNotice,
  TerminalActivityUpgradePrompt,
} from "./terminal-activity-upgrade";
import {
  checkActivityUpgrade,
  resetActivityUpgradeForTests,
} from "../lib/terminal-activity-upgrade";
const api = vi.hoisted(() => ({
  terminalActivityUpgradeStatus: vi.fn(),
  updateTerminalActivityServer: vi.fn(),
}));
vi.mock("@/tauri/commands", () => api);
vi.mock("sonner", () => ({ toast: { warning: vi.fn() } }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
const target = { serverId: "old", epoch: "e", pid: 123, sessionIds: ["a", "b"] };
function mount() {
  return render(
    <>
      <TerminalActivityUpgradePrompt />
      <TerminalActivityUpgradeNotice />
    </>,
  );
}
beforeEach(() => {
  resetActivityUpgradeForTests();
  vi.clearAllMocks();
  api.terminalActivityUpgradeStatus.mockResolvedValue(target);
  api.updateTerminalActivityServer.mockResolvedValue(undefined);
});
describe("compatible terminal server update", () => {
  it("discovers the update without stopping terminals and focuses the safe choice", async () => {
    mount();
    await userEvent.click(await screen.findByRole("button", { name: "Review update" }));
    expect(
      await screen.findByRole("button", { name: "Close 2 terminals and update" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Keep working" })).toHaveFocus();
    expect(api.updateTerminalActivityServer).not.toHaveBeenCalled();
    await userEvent.keyboard("{Escape}");
    expect(await screen.findByRole("button", { name: "Review update" })).toHaveClass(
      "text-foreground",
    );
  });
  it("updates only on explicit confirmation with the captured identity and verifies completion", async () => {
    mount();
    await userEvent.click(await screen.findByRole("button", { name: "Review update" }));
    await userEvent.click(
      await screen.findByRole("button", { name: "Close 2 terminals and update" }),
    );
    expect(api.updateTerminalActivityServer).toHaveBeenCalledWith(target);
    expect(await screen.findByText("Terminal server updated")).toBeInTheDocument();
    expect(screen.getByText(/Open a new managed Claude or Codex/)).toBeInTheDocument();
  });
  it("requires refreshed confirmation when sessions changed, never blindly retries", async () => {
    api.updateTerminalActivityServer.mockRejectedValueOnce({
      oldStopped: false,
      message: "changed",
    });
    mount();
    await userEvent.click(await screen.findByRole("button", { name: "Review update" }));
    await userEvent.click(
      await screen.findByRole("button", { name: "Close 2 terminals and update" }),
    );
    const check = await screen.findByRole("button", { name: "Check again" });
    api.terminalActivityUpgradeStatus.mockResolvedValue({ ...target, sessionIds: ["a", "b", "c"] });
    await userEvent.click(check);
    expect(
      await screen.findByRole("button", { name: "Close 3 terminals and update" }),
    ).toBeInTheDocument();
    expect(api.updateTerminalActivityServer).toHaveBeenCalledTimes(1);
  });
  it("offers a bounded non-destructive retry after the old server stopped", async () => {
    api.updateTerminalActivityServer.mockRejectedValueOnce({
      oldStopped: true,
      message: "start failed",
    });
    mount();
    await userEvent.click(await screen.findByRole("button", { name: "Review update" }));
    await userEvent.click(
      await screen.findByRole("button", { name: "Close 2 terminals and update" }),
    );
    await userEvent.click(await screen.findByRole("button", { name: "Retry update" }));
    expect(await screen.findByText("Terminal server updated")).toBeInTheDocument();
    expect(api.updateTerminalActivityServer).toHaveBeenNthCalledWith(2, target);
  });
  it("never offers a destructive action while the host cannot be checked", async () => {
    api.terminalActivityUpgradeStatus.mockRejectedValue(new Error("offline"));
    mount();
    await act(async () => {
      await checkActivityUpgrade(true);
    });
    expect(await screen.findByRole("button", { name: "Check again" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /and update/ })).not.toBeInTheDocument();
    expect(api.updateTerminalActivityServer).not.toHaveBeenCalled();
  });
  it("has a non-destructive update action when no terminals are running", async () => {
    api.terminalActivityUpgradeStatus.mockResolvedValue({ ...target, sessionIds: [] });
    mount();
    await userEvent.click(await screen.findByRole("button", { name: "Review update" }));
    expect(
      await screen.findByRole("button", { name: "Update terminal server" }),
    ).toBeInTheDocument();
    await waitFor(() => expect(api.updateTerminalActivityServer).not.toHaveBeenCalled());
  });
});
