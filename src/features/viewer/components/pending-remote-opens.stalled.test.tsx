// SPDX-License-Identifier: MIT
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

// Seam: the real remote open list behind the real notice. The machine's
// answer never comes; "Opening shell on X…" must turn into recovery.

const backend = vi.hoisted(() => ({
  requests: [] as { operation: string; reference?: unknown }[],
  answers: [] as ((value: unknown) => void)[],
}));

vi.mock("@/features/viewer/lib/cli-workspace/transport/terminal-launch-source", () => ({
  remoteShellTab: vi.fn((request: { operation: string; reference?: unknown }) => {
    backend.requests.push(request);
    return new Promise((resolve) => backend.answers.push(resolve));
  }),
}));
vi.mock("@/features/viewer/lib/cli-workspace/transport/terminal-layout-source", () => ({
  terminalLayoutMemberSnapshots: vi.fn(async () => []),
}));
vi.mock("@/shared/lib/private-terminal-authority", () => ({
  currentPrivateAuthorityEpoch: () => "7",
  onPrivateAuthorityRetired: () => () => {},
}));
const inspect = vi.hoisted(() => vi.fn());
vi.mock("@/features/machines/lib/machine-actions", () => ({
  allowAgentsAndRelaunch: vi.fn(),
  openMachinesPanel: inspect,
}));
vi.mock("@/features/shell/lib/machine-terminal-server", () => ({
  openMachineTerminalServerPrompt: vi.fn(),
  onMachineTerminalServerReplaced: () => () => {},
}));
vi.mock("@/shared/lib/get-current-user", () => ({
  getCurrentUser: () => null,
  useCurrentUser: () => null,
}));

import { clearRemoteOpens, openRemoteTab, REMOTE_OPEN_SETTLE_MS } from "../lib/remote-tab-open";
import { PendingRemoteOpens } from "./pending-remote-opens";

beforeEach(() => {
  vi.useFakeTimers();
  backend.requests = [];
  backend.answers = [];
  inspect.mockReset();
  clearRemoteOpens();
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

function openShell(): string {
  const id = openRemoteTab({
    teamKey: "acme/default",
    workspaceId: "ws",
    machineId: "box",
    label: "Box",
  });
  render(<PendingRemoteOpens teamKey="acme/default" workspaceId="ws" />);
  return id;
}

it("a shell open the machine never answers turns into Inspect / Attach instead of spinning", async () => {
  const requestId = openShell();
  expect(screen.getByText("Opening shell on Box…")).toBeTruthy();

  await act(() => vi.advanceTimersByTimeAsync(REMOTE_OPEN_SETTLE_MS));

  expect(screen.queryByText("Opening shell on Box…")).toBeNull();
  expect(screen.getByText(/Shell on Box isn't confirmed/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Inspect" }));
  expect(inspect).toHaveBeenCalled();

  fireEvent.click(screen.getByRole("button", { name: "Attach" }));
  expect(backend.requests.map((r) => r.operation)).toEqual(["create", "attach"]);
  expect(backend.requests[1]!.reference).toEqual({ requestId });
  expect(screen.getByText("Opening shell on Box…")).toBeTruthy();

  // The attach hangs too: recovery again, never a second create.
  await act(() => vi.advanceTimersByTimeAsync(REMOTE_OPEN_SETTLE_MS));
  expect(screen.getByRole("button", { name: "Attach" })).toBeTruthy();
  expect(backend.requests.filter((r) => r.operation === "create")).toHaveLength(1);
});

it("a late answer still lands on the stalled row", async () => {
  openShell();
  await act(() => vi.advanceTimersByTimeAsync(REMOTE_OPEN_SETTLE_MS));
  expect(screen.getByRole("button", { name: "Attach" })).toBeTruthy();

  await act(async () => {
    backend.answers[0]!({
      status: "refused",
      headed: true,
      code: "unavailable",
      message: "the machine is offline",
    });
  });

  expect(screen.getByText("Couldn't open on Box: the machine is offline")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Attach" })).toBeNull();
});

it("a caller awaiting a slow open still gets its late answer", async () => {
  const { openRemoteTabSettled } = await import("../lib/remote-tab-open");
  let settled: unknown = null;
  void openRemoteTabSettled({
    teamKey: "acme/default",
    workspaceId: "ws",
    machineId: "box",
    label: "Box",
  }).then((value) => (settled = value));
  await act(() => vi.advanceTimersByTimeAsync(REMOTE_OPEN_SETTLE_MS));
  expect(settled).toBeNull();

  await act(async () => {
    backend.answers[0]!({ status: "refused", headed: true, code: "unavailable", message: "no" });
  });
  expect(settled).toMatchObject({ status: "refused", message: "no" });
});

it("after a stalled Attach, only the Attach's late reply lands, not the original send's", async () => {
  openShell();
  await act(() => vi.advanceTimersByTimeAsync(REMOTE_OPEN_SETTLE_MS));
  fireEvent.click(screen.getByRole("button", { name: "Attach" }));
  await act(() => vi.advanceTimersByTimeAsync(REMOTE_OPEN_SETTLE_MS));
  expect(screen.getByRole("button", { name: "Attach" })).toBeTruthy();

  // The original create answers late: the row belongs to the Attach now.
  await act(async () => {
    backend.answers[0]!({ status: "refused", headed: true, code: "unavailable", message: "old" });
  });
  expect(screen.queryByText(/Couldn't open on Box: old/)).toBeNull();

  await act(async () => {
    backend.answers[1]!({ status: "refused", headed: true, code: "unavailable", message: "new" });
  });
  expect(screen.getByText("Couldn't open on Box: new")).toBeTruthy();
});
