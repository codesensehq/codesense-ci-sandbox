// SPDX-License-Identifier: MIT
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { PendingRemoteOpen } from "../lib/remote-tab-open";
const fake = vi.hoisted(() => ({
  rows: [] as PendingRemoteOpen[],
  recheck: vi.fn(),
  restart: vi.fn(),
}));
vi.mock("../lib/remote-tab-open", () => ({
  usePendingRemoteOpens: () => fake.rows,
  recheckRemoteOpen: fake.recheck,
  restartRefusedRemoteOpen: fake.restart,
  relaunchRefusedRemoteOpen: vi.fn(),
  dismissRemoteOpen: vi.fn(),
  STALLED_OPEN_CODES: ["noAnswer", "timedOut"],
}));
vi.mock("@/features/machines/lib/machine-actions", () => ({
  allowAgentsAndRelaunch: vi.fn(),
  openMachinesPanel: vi.fn(),
}));
vi.mock("@/features/shell/lib/machine-terminal-server", () => ({
  openMachineTerminalServerPrompt: vi.fn(),
}));
import { PendingRemoteOpens } from "./pending-remote-opens";
beforeEach(() => {
  fake.recheck.mockReset();
  fake.restart.mockReset();
});
afterEach(cleanup);
function show(patch: Partial<PendingRemoteOpen>) {
  fake.rows = [
    {
      requestId: "request",
      teamKey: "acme/default",
      workspaceId: "ws",
      machineId: "box",
      label: "Box",
      launch: "codex",
      agentKey: "acme/default/builder",
      status: "refused",
      ...patch,
    },
  ];
  render(<PendingRemoteOpens teamKey="acme/default" workspaceId="ws" />);
}
it("offers same-intent preparation retry for retryable refusals", () => {
  show({ code: "preparationIncomplete", retryable: true });
  fireEvent.click(screen.getByRole("button", { name: "Retry preparation" }));
  expect(fake.recheck).toHaveBeenCalledWith("request");
  expect(screen.queryByRole("button", { name: "Start a new launch" })).toBeNull();
});
it("offers a fresh intent only for definitive newIntentRequired", () => {
  show({ code: "newIntentRequired", retryable: false });
  fireEvent.click(screen.getByRole("button", { name: "Start a new launch" }));
  expect(fake.restart).toHaveBeenCalledWith("request");
});
it("unknown outcomes can only be rechecked", () => {
  show({ status: "unknown", code: "preparationFailed" });
  fireEvent.click(screen.getByRole("button", { name: "Check again" }));
  expect(fake.recheck).toHaveBeenCalledWith("request");
  expect(screen.queryByRole("button", { name: "Start a new launch" })).toBeNull();
});
it("a saved Agent whose CLI is missing names that CLI and its install command", () => {
  show({
    code: "agentCliMissing",
    launch: "shell",
    message: "OpenAI Codex isn't installed on this machine",
  });
  expect(screen.getByText("Codex isn't installed on Box")).toBeTruthy();
  expect(screen.getByTestId("pending-remote-open-install").textContent).toBe(
    "npm install -g @openai/codex",
  );
});
