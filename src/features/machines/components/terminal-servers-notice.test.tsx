// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { toast } from "sonner";

import { machineHandlers, mockTerminalServer } from "@/app/mock/handlers/machines";
import { TooltipProvider } from "@/components/ui/tooltip";
import type { MachineSummary } from "@/tauri/commands";
import { useViewerStore } from "@/shared/stores/viewer-store";

import {
  installMachinesBackend,
  resetMachinesUi,
  TEAM_KEY,
  ZERO_LATENCY,
} from "../__test-utils__/machines-backend";
import { reloadMachines } from "../stores/machines-store";
import { MachinesGroup } from "./machines-group";
import { TerminalServersTitleBarNotice } from "./terminal-servers-notice";

const { openMachines } = vi.hoisted(() => ({ openMachines: vi.fn() }));
vi.mock("@/features/shell/machines-virtual-tab", () => ({ openMachines }));

const stale = (digest: string) =>
  mockTerminalServer(false, {
    serverVersion: undefined,
    upToDate: false,
    relation: "older",
    buildDigest: digest,
  });

/** The real mock list, with stale servers on the named machines; other
 *  overrides change a machine's status. */
function installWithStale(
  staleBuilds: Record<string, string>,
  statuses: Record<string, MachineSummary["status"]> = {},
) {
  const real = machineHandlers("demo", ZERO_LATENCY);
  return installMachinesBackend("demo", {
    list_machines: async (args) =>
      ((await real.list_machines!(args)) as MachineSummary[]).map((m) => {
        const status = statuses[m.machineId];
        if (status) {
          const { terminalServer: _gone, ...rest } = m;
          return { ...rest, status };
        }
        if (staleBuilds[m.machineId])
          return {
            ...m,
            status: { state: "connected" as const, workspaces: 1 },
            terminalServer: stale(staleBuilds[m.machineId]),
          };
        return m;
      }),
  });
}

beforeEach(() => {
  resetMachinesUi();
  useViewerStore.setState({ dismissedTerminalServers: [] });
  openMachines.mockClear();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("SW-AGR-19 PR-2: out-of-date terminal servers notice", () => {
  it("shows one calm, dismissible indicator in Machines and the title bar — never a toast", async () => {
    const toasts = [
      vi.spyOn(toast, "warning"),
      vi.spyOn(toast, "error"),
      vi.spyOn(toast, "info"),
      vi.spyOn(toast, "message"),
    ];
    installWithStale({ "gpu-box-2": "aaaaaaaaaaaa", "build-farm": "bbbbbbbbbbbb" });
    render(
      <TooltipProvider>
        <MachinesGroup teamKey={TEAM_KEY} />
        <TerminalServersTitleBarNotice teamKey={TEAM_KEY} />
      </TooltipProvider>,
    );
    expect(await screen.findByTestId("terminal-servers-out-of-date")).toHaveTextContent(
      "2 terminal servers out of date",
    );
    const chrome = screen.getByTestId("titlebar-terminal-servers-out-of-date");
    expect(chrome).toHaveAccessibleName("2 terminal servers out of date — open Machines");
    expect(chrome).toHaveTextContent("2 terminal servers out of date");

    // The title bar opens Machines.
    await userEvent.click(chrome);
    await vi.waitFor(() => expect(openMachines).toHaveBeenCalledTimes(1));

    // Dismissed: both go, and stay gone after a reload of the same set.
    await userEvent.click(screen.getByTestId("terminal-servers-out-of-date-dismiss"));
    expect(screen.queryByTestId("terminal-servers-out-of-date")).toBeNull();
    expect(screen.queryByTestId("titlebar-terminal-servers-out-of-date")).toBeNull();
    await reloadMachines(TEAM_KEY);
    expect(screen.queryByTestId("terminal-servers-out-of-date")).toBeNull();
    for (const spy of toasts) expect(spy).not.toHaveBeenCalled();
  });

  it("stays dismissed when a stale server goes offline or is updated; returns for a new one", async () => {
    installWithStale({ "gpu-box-2": "aaaaaaaaaaaa", "build-farm": "bbbbbbbbbbbb" });
    render(<MachinesGroup teamKey={TEAM_KEY} />);
    await userEvent.click(await screen.findByTestId("terminal-servers-out-of-date-dismiss"));

    // build-farm drops offline: the rest was dismissed, so nothing returns.
    installWithStale(
      { "gpu-box-2": "aaaaaaaaaaaa" },
      { "build-farm": { state: "reconnecting", attempt: 1 } },
    );
    await reloadMachines(TEAM_KEY);
    expect(screen.queryByTestId("terminal-servers-out-of-date")).toBeNull();

    // gpu-box-2 now runs another stale build: that is new.
    installWithStale({ "gpu-box-2": "cccccccccccc", "build-farm": "bbbbbbbbbbbb" });
    await reloadMachines(TEAM_KEY);
    expect(await screen.findByTestId("terminal-servers-out-of-date")).toHaveTextContent(
      "2 terminal servers out of date",
    );
  });

  it("says nothing while every server is up to date", async () => {
    installMachinesBackend("demo");
    render(
      <TooltipProvider>
        <MachinesGroup teamKey={TEAM_KEY} />
        <TerminalServersTitleBarNotice teamKey={TEAM_KEY} />
      </TooltipProvider>,
    );
    await screen.findByTestId("machine-terminal-server-local");
    expect(screen.queryByTestId("terminal-servers-out-of-date")).toBeNull();
    expect(screen.queryByTestId("titlebar-terminal-servers-out-of-date")).toBeNull();
  });
});
