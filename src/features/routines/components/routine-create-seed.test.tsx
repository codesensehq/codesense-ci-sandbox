// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@/test-utils";

// SW-AGR-6 "Keep this…": the New routine form opens pre-filled from a timer,
// and once the routine is saved it offers to cancel the timer.

const h = vi.hoisted(() => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn() }),
  cancelSessionTimer: vi.fn(),
}));
vi.mock("sonner", () => ({ toast: h.toast }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  agentList: vi.fn().mockResolvedValue([]),
  cancelSessionTimer: h.cancelSessionTimer,
}));

import { resetRoutinesStoreForTests, useRoutinesStore } from "../stores/routines-store";
import { RoutineDetailPane } from "./routine-detail";

beforeEach(() => {
  resetRoutinesStoreForTests();
  h.toast.mockReset();
  h.cancelSessionTimer.mockReset().mockResolvedValue(true);
});
afterEach(() => {
  cleanup();
  resetRoutinesStoreForTests();
});

const SEED = {
  draft: {
    name: "Inbox nudge",
    when: "every 15m",
    task: "Read your inbox and answer anything addressed to you",
    agent: "acme/core/barb",
    mode: "wake" as const,
  },
  mode: "wake" as const,
  machineId: "codesense-test-1",
  timer: { sessionId: "s-a", machineId: "codesense-test-1", timerId: "t2", name: "Inbox nudge" },
};

describe("a routine seeded from a timer", () => {
  it("opens the New routine form filled in from the timer", () => {
    useRoutinesStore.getState().startCreate(SEED);
    render(<RoutineDetailPane now={Date.now()} />);
    expect(screen.getByLabelText("Name")).toHaveValue("Inbox nudge");
    expect(screen.getByLabelText("When")).toHaveValue("every 15m");
    expect(screen.getByLabelText("Task")).toHaveValue(
      "Read your inbox and answer anything addressed to you",
    );
  });

  it("a second Keep this… asks before dropping the filled form, then refills it", async () => {
    useRoutinesStore.getState().startCreate(SEED);
    render(<RoutineDetailPane now={Date.now()} />);
    await waitFor(() => expect(useRoutinesStore.getState().formDirty).toBe(true));
    useRoutinesStore.getState().startCreate({ draft: { ...SEED.draft, name: "Check the build" } });
    expect(useRoutinesStore.getState().pendingLeave).toEqual({ kind: "create" });
    expect(screen.getByLabelText("Name")).toHaveValue("Inbox nudge");
    act(() => useRoutinesStore.getState().confirmLeave());
    await waitFor(() => expect(screen.getByLabelText("Name")).toHaveValue("Check the build"));
  });

  it("after the save, asks whether to cancel the timer, and cancels it on the tab's machine", async () => {
    const saveRoutine = vi
      .fn()
      .mockResolvedValue({ outcome: "saved", key: "k", revision: 1, warnings: [] });
    useRoutinesStore.getState().startCreate(SEED);
    useRoutinesStore.setState({ saveRoutine });
    render(<RoutineDetailPane now={Date.now()} />);
    fireEvent.click(screen.getByRole("button", { name: "Create routine" }));
    await waitFor(() => expect(saveRoutine).toHaveBeenCalled());
    await waitFor(() =>
      expect(h.toast).toHaveBeenCalledWith(
        "Inbox nudge is now a routine. Cancel the timer?",
        expect.objectContaining({
          action: expect.objectContaining({ label: "Cancel timer" }),
          cancel: expect.objectContaining({ label: "Keep both" }),
        }),
      ),
    );
    const options = h.toast.mock.calls[0][1] as { action: { onClick: () => void } };
    options.action.onClick();
    expect(h.cancelSessionTimer).toHaveBeenCalledWith("s-a", "t2", "codesense-test-1");
  });

  it("saves a WAKE routine for this agent on the timer's machine", async () => {
    const saveRoutine = vi
      .fn()
      .mockResolvedValue({ outcome: "saved", key: "k", revision: 1, warnings: [] });
    useRoutinesStore.getState().startCreate({ ...SEED, machineLabel: "codesense-test-1" });
    useRoutinesStore.setState({ saveRoutine });
    render(<RoutineDetailPane now={Date.now()} />);
    expect(screen.getByTestId("routine-seed-mode")).toHaveTextContent(
      "each run wakes the agent in its running session on codesense-test-1",
    );
    fireEvent.click(screen.getByRole("button", { name: "Create routine" }));
    await waitFor(() => expect(saveRoutine).toHaveBeenCalled());
    expect(saveRoutine.mock.calls[0][0]).toMatchObject({
      agent: "acme/core/barb",
      when: "every 15m",
      mode: "wake",
      machine: "codesense-test-1",
    });
  });

  it("a routine saved as a draft does not offer to cancel the timer (the timer still runs)", async () => {
    const saveRoutine = vi
      .fn()
      .mockResolvedValue({ outcome: "saved", key: "k", revision: 1, warnings: [] });
    useRoutinesStore.getState().startCreate(SEED);
    useRoutinesStore.setState({ saveRoutine });
    render(<RoutineDetailPane now={Date.now()} />);
    fireEvent.click(screen.getByRole("switch", { name: "Enabled" }));
    fireEvent.click(screen.getByRole("button", { name: "Create routine" }));
    await waitFor(() => expect(saveRoutine).toHaveBeenCalled());
    expect(h.toast).not.toHaveBeenCalled();
  });
});
