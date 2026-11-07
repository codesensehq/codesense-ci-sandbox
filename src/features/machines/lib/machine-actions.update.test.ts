// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

const prompt = vi.hoisted(() => ({ promptMachineTerminalServerUpdate: vi.fn() }));
vi.mock("@/features/shell/lib/machine-terminal-server", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/features/shell/lib/machine-terminal-server")>()),
  ...prompt,
}));

import { useMachinesStore } from "../stores/machines-store";
import { MachineActionCancelled, updateMachineTerminalServer } from "./machine-actions";
import type { MachineSummary } from "@/tauri/commands";

const box: MachineSummary = {
  machineId: "box",
  kind: "ssh",
  label: "Box",
  scope: { kind: "personal" },
  declaration: null,
  binding: null,
  canPublish: true,
  status: {
    state: "connected",
    workspaces: 1,
    terminalServerUpdate: true,
    terminalServerSessions: 3,
  },
  attention: 0,
  capabilities: { agentCallbacks: true },
};

beforeEach(() => {
  prompt.promptMachineTerminalServerUpdate.mockReset();
  useMachinesStore.setState({ byTeam: { "acme:default": [box] } });
});

describe("updateMachineTerminalServer (SW-AGR-19)", () => {
  it("resolves only once the update finished", async () => {
    prompt.promptMachineTerminalServerUpdate.mockResolvedValue({ kind: "updated" });
    await expect(updateMachineTerminalServer("box")).resolves.toBeUndefined();
    expect(prompt.promptMachineTerminalServerUpdate).toHaveBeenCalledWith(
      expect.objectContaining({ machineId: "box", count: 3, update: true }),
    );
  });

  it("a closed confirmation is a cancel, never an update", async () => {
    prompt.promptMachineTerminalServerUpdate.mockResolvedValue({ kind: "closed" });
    await expect(updateMachineTerminalServer("box")).rejects.toBeInstanceOf(MachineActionCancelled);
  });

  it("a failed update says why", async () => {
    prompt.promptMachineTerminalServerUpdate.mockResolvedValue({
      kind: "failed",
      repair: "the old terminal server did not stop",
    });
    await expect(updateMachineTerminalServer("box")).rejects.toThrow(
      "the old terminal server did not stop",
    );
  });

  it("a machine that is not connected is sent to set-up, and says so", async () => {
    useMachinesStore.setState({
      byTeam: { "acme:default": [{ ...box, status: { state: "idle" } }] },
    });
    await expect(updateMachineTerminalServer("box")).rejects.toThrow(/Connect Box in Machines/);
    expect(prompt.promptMachineTerminalServerUpdate).not.toHaveBeenCalled();
  });
});
