// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { MachinesTab } from "./machines-tab";
import { useUiStore } from "@/shared/stores/ui-store";
import { act } from "@testing-library/react";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { liveHub } from "@/features/dashboard/lib/live-hub";
import { mockMachineStats } from "@/app/mock/handlers/machines";
import {
  installMachinesBackend,
  resetMachinesUi,
  TEAM_KEY,
} from "@/features/machines/__test-utils__/machines-backend";
const { getDependencyStatus, events } = vi.hoisted(() => ({
  getDependencyStatus: vi.fn(),
  events: new Map<string, Set<(event: { payload: unknown }) => void>>(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (event: string, cb: (event: { payload: unknown }) => void) => {
    const listeners = events.get(event) ?? new Set();
    listeners.add(cb);
    events.set(event, listeners);
    return () => listeners.delete(cb);
  },
}));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  getDependencyStatus,
}));
beforeEach(() => {
  resetMachinesUi();
  installMachinesBackend("none");
  getDependencyStatus.mockResolvedValue({ agents: {} });
});
afterEach(() => {
  cleanup();
  liveHub.dispose();
  events.clear();
});
it("keeps machine setup available without service account controls", async () => {
  render(<MachinesTab />);
  expect(await screen.findByTestId("connections-machines-group")).toBeInTheDocument();
  expect(screen.queryByText("Code hosts")).toBeNull();
  expect(screen.queryByRole("button", { name: "Add connection" })).toBeNull();
});
it("retains the existing direct Add-machine reserve entry", async () => {
  useUiStore.getState().setMachinesPanel({ mode: "add" });
  render(<MachinesTab />);
  expect(await screen.findByTestId("add-machine-panel")).toBeInTheDocument();
});
it("paints the app's own surface, so its text never sits on the document viewer's theme", () => {
  render(<MachinesTab />);
  const tab = screen.getByTestId("machines-tab");
  // The tab is hosted inside the viewer (whose prose theme may be dark while the
  // app is light): it must bring its own background with its foreground.
  expect(tab.className).toContain("bg-background");
  expect(tab.className).toContain("text-foreground");
});

it("unsubscribes a kept-mounted Machines tab when another tab is selected", async () => {
  const stop = vi.fn();
  const watch = vi.spyOn(liveHub, "watch").mockReturnValue(stop);
  useViewerStore.setState({
    activeTab: { [TEAM_KEY]: "virtual://machines" },
    layoutMode: "shared",
  });
  const view = render(<MachinesTab hostTeamKey={TEAM_KEY} />);
  expect(watch).toHaveBeenCalledWith("machineStats", "all", expect.any(Function));
  act(() => useViewerStore.setState({ activeTab: { [TEAM_KEY]: "README.md" } }));
  expect(stop).toHaveBeenCalledOnce();
  act(() => useViewerStore.setState({ activeTab: { [TEAM_KEY]: "virtual://machines" } }));
  expect(watch).toHaveBeenCalledTimes(2);
  act(() => useViewerStore.setState({ layoutMode: "console" }));
  expect(stop).toHaveBeenCalledTimes(2);
  act(() => useViewerStore.setState({ layoutMode: "shared" }));
  expect(watch).toHaveBeenCalledTimes(3);
  view.unmount();
  expect(stop).toHaveBeenCalledTimes(3);
  watch.mockRestore();
});

it("subscribes the visible no-team Machines tab with a saved console layout", () => {
  const stop = vi.fn();
  const watch = vi.spyOn(liveHub, "watch").mockReturnValue(stop);
  useViewerStore.setState({ layoutMode: "console" });
  const view = render(<MachinesTab />);
  expect(watch).toHaveBeenCalledWith("machineStats", "all", expect.any(Function));
  view.unmount();
  expect(stop).toHaveBeenCalledOnce();
  watch.mockRestore();
});

it("renders the hub snapshot and deltas, and removes a retired server's cached resources", async () => {
  const sample = mockMachineStats();
  const { calls } = installMachinesBackend("none", {
    live_hub_subscribe: () => ({
      topic: "machineStats",
      viewKey: "all",
      rev: 0,
      snapshot: [{ id: "local", ...sample }],
    }),
  });
  render(<MachinesTab />);
  expect(await screen.findByText(/Load 38% of 8 cores/)).toBeVisible();
  const subscriptionId = calls.find(({ cmd }) => cmd === "live_hub_subscribe")!.args.subscriptionId;
  const listReads = calls.filter(({ cmd }) => cmd === "list_machines").length;
  const deliver = (baseRev: number, upserts: unknown[], removes: string[] = []) => {
    for (const cb of events.get("codesense:subscription-delta") ?? [])
      cb({
        payload: {
          subscriptionId,
          baseRev,
          delta: { topic: "machineStats", viewKey: "all", rev: baseRev + 1, upserts, removes },
        },
      });
  };
  act(() => deliver(0, [{ id: "local", ...sample, load1: 4 }]));
  expect(await screen.findByText(/Load 50% of 8 cores/)).toBeVisible();
  expect(calls.filter(({ cmd }) => cmd === "list_machines")).toHaveLength(listReads);
  act(() => deliver(1, [], ["local"]));
  await waitFor(() => expect(screen.queryByText(/Load .* of 8 cores/)).toBeNull());
  act(() => deliver(2, [{ id: "local", machineStatsSupported: false }]));
  expect(
    await screen.findByText("Load, memory and disk unknown · update the terminal server"),
  ).toBeVisible();
});
