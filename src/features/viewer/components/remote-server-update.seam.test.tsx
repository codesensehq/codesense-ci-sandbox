// SPDX-License-Identifier: MIT
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import type { MachineStatus, MachineSummary } from "@/tauri/commands";
import { useMachinesStore } from "@/features/machines/stores/machines-store";
import { describeMachineStatus } from "@/features/machines/lib/machine-status";
import { runMachineAction } from "@/features/machines/lib/machine-actions";
import { closeMachineTerminalServerPrompt } from "@/features/shell/lib/machine-terminal-server";
import { TerminalServerPrompt } from "@/features/shell/components/terminal-server-prompt";
import { useUiStore } from "@/shared/stores/ui-store";
import {
  resetPrunedAgentSessionsForTests,
  useAgentStateStore,
} from "@/shared/stores/agent-state-store";
import { RemoteServerUpdateAction } from "./remote-server-update";

// The remote tab's origin, as its layout has it.
vi.mock("@/shared/stores/viewer-store", async (original) => {
  const actual = await original<typeof import("@/shared/stores/viewer-store")>();
  return {
    ...actual,
    selectTabBySessionId: (state: Parameters<typeof actual.selectTabBySessionId>[0], id: string) =>
      id === "remote-agent"
        ? ({ origin: { machineId: "build-box", member: "m", handle: "h" } } as ReturnType<
            typeof actual.selectTabBySessionId
          >)
        : actual.selectTabBySessionId(state, id),
  };
});
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));
vi.mock("@/features/shell/machines-virtual-tab", () => ({ openMachines: vi.fn() }));

const TEAM = "acme:default";
const outdated: MachineStatus = {
  state: "connected",
  workspaces: 1,
  terminalServerUpdate: true,
  terminalServerSessions: 2,
};

function machine(mode: "private-account" | "ssh"): MachineSummary {
  return {
    machineId: "build-box",
    kind: "ssh",
    label: "build-box",
    scope: { kind: "personal" },
    declaration: mode === "private-account" ? ({ mode } as MachineSummary["declaration"]) : null,
    binding: null,
    canPublish: true,
    status: outdated,
    attention: 0,
    capabilities: { agentCallbacks: true },
  };
}

function outOfDate(summary: MachineSummary) {
  act(() => {
    useMachinesStore.setState({ byTeam: { [TEAM]: [summary] } });
    useAgentStateStore.getState().setProcessAgent("remote-agent", "claude");
    useAgentStateStore.getState().activityUnavailable(["remote-agent"], "upgrade-required");
  });
}

afterEach(() => {
  cleanup();
  act(() => closeMachineTerminalServerPrompt());
  useAgentStateStore.getState().remove(["remote-agent", "local-agent"]);
  resetPrunedAgentSessionsForTests();
  useMachinesStore.setState({ byTeam: {} });
  useUiStore.getState().setMachinesPanel(null);
});

describe("an out-of-date machine terminal server, from where its agent shows unknown", () => {
  it("one click opens that machine's update, worded as what it does to running sessions", async () => {
    outOfDate(machine("private-account"));
    render(
      <>
        <RemoteServerUpdateAction sessionId="remote-agent" />
        <TerminalServerPrompt />
      </>,
    );
    await userEvent.click(
      screen.getByRole("button", { name: "Update terminal server on build-box" }),
    );
    const prompt = await screen.findByTestId("terminal-server-prompt");
    expect(prompt).toHaveTextContent("Update the terminal server on build-box?");
    expect(prompt).toHaveTextContent(
      "Updating restarts the terminal server on build-box. The 2 terminals and agents running on it now will end",
    );
    expect(screen.getByRole("button", { name: "Update terminal server" })).toBeInTheDocument();
  });

  it("is the same action the machine's Machines entry offers", async () => {
    for (const mode of ["private-account", "ssh"] as const) {
      const summary = machine(mode);
      outOfDate(summary);
      const offered = describeMachineStatus(summary, 0).actions[0];
      expect(offered?.label).toBe("Update terminal server");
      render(
        <>
          <RemoteServerUpdateAction sessionId="remote-agent" />
          <TerminalServerPrompt />
        </>,
      );
      await userEvent.click(screen.getByTestId("remote-server-update"));
      // SW-AGR-19 (AC5): every mode updates in place through the confirmation —
      // never only set-up.
      expect(await screen.findByTestId("terminal-server-prompt")).toHaveTextContent(
        "Update the terminal server on build-box?",
      );
      expect(useUiStore.getState().machinesPanel).toBeNull();
      act(() => closeMachineTerminalServerPrompt());
      expect(screen.queryByTestId("terminal-server-prompt")).toBeNull();
      // The Machines entry's action opens the very same confirmation.
      void runMachineAction(offered!.id, summary, TEAM);
      expect(await screen.findByTestId("terminal-server-prompt")).toHaveTextContent(
        "Update the terminal server on build-box?",
      );
      act(() => closeMachineTerminalServerPrompt());
      cleanup();
    }
  });

  it("offers nothing for a local session or one whose status reads", () => {
    outOfDate(machine("private-account"));
    act(() =>
      useAgentStateStore.getState().activityUnavailable(["local-agent"], "upgrade-required"),
    );
    render(<RemoteServerUpdateAction sessionId="local-agent" />);
    expect(screen.queryByTestId("remote-server-update")).toBeNull();
    act(() => useAgentStateStore.getState().activityUnavailable(["remote-agent"], "recovering"));
    render(<RemoteServerUpdateAction sessionId="remote-agent" />);
    expect(screen.queryByTestId("remote-server-update")).toBeNull();
  });
});
