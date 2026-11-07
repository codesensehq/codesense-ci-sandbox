// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@/test-utils";

import type { RunOutcomeRead } from "@/shared/lib/run-outcome";

const listRunOutcomes = vi.fn<() => Promise<RunOutcomeRead[]>>();
vi.mock("@/tauri/commands", () => ({ listRunOutcomes: () => listRunOutcomes() }));
const openAgentResult = vi.fn();
vi.mock("@/features/viewer/components/agent-result-panel", () => ({
  openAgentResult: (...args: unknown[]) => openAgentResult(...args),
}));

const { RunsView, useRunHistory } = await import("./runs-view");

/** The Runs view as the Agents pane mounts it: the history fetched while it shows. */
function RecentRuns({ teamKey }: { teamKey: string }) {
  return <RunsView teamKey={teamKey} runs={useRunHistory(true)} />;
}
const { useManagedSessionStore } = await import("@/shared/stores/use-managed-session-store");
const { useAgentResultStore } = await import("@/shared/stores/use-agent-result-store");

function read(
  runId: string,
  endedAt: string,
  storyId: string,
  artifact: string | null = `${runId}.log`,
): RunOutcomeRead {
  return {
    outcome: {
      story_id: storyId,
      run_id: runId,
      status: "success",
      ended_at: endedAt,
      artifact_ref: artifact,
    },
    validated: "completed",
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  useManagedSessionStore.setState({ states: {} });
  useAgentResultStore.getState().close();
});

describe("RunsView", () => {
  it("lists finished runs with output, newest first, with outcome and kind, and opens one on click", async () => {
    listRunOutcomes.mockResolvedValue([
      read("r-old", "2026-10-01T10:00:00Z", "routine:acme/backend/weekly-digest@r-old"),
      read("r-quiet", "2026-10-02T11:00:00Z", "SW-Q", null),
      read("r-new", "2026-10-02T10:00:00Z", "routine:acme/backend/morning-triage@r-new"),
    ]);
    render(<RecentRuns teamKey="t" />);

    const group = await screen.findByRole("region", { name: "Run history" });
    const list = await within(group).findByRole("list");
    const rows = within(list).getAllByRole("button");
    expect(rows.map((r) => r.textContent)).toEqual([
      expect.stringContaining("morning-triage"),
      expect.stringContaining("weekly-digest"),
    ]);
    expect(rows[0]).toHaveTextContent(/Routine/);

    fireEvent.click(rows[0]);
    expect(openAgentResult).toHaveBeenCalledWith({
      run_id: "r-new",
      story_id: "routine:acme/backend/morning-triage@r-new",
    });
  });

  it("says so when no finished run left output", async () => {
    listRunOutcomes.mockResolvedValue([read("r-quiet", "2026-10-02T11:00:00Z", "SW-Q", null)]);
    render(<RecentRuns teamKey="t" />);
    expect(await screen.findByText("No finished runs yet")).toBeInTheDocument();
  });

  it("narrows to runs matching a search, and clears back", async () => {
    listRunOutcomes.mockResolvedValue([
      read("r-1", "2026-10-02T10:00:00Z", "routine:acme/backend/weekly-digest@r-1"),
      read("r-2", "2026-10-02T11:00:00Z", "SW-A"),
    ]);
    render(<RecentRuns teamKey="t" />);
    await screen.findByRole("button", { name: /weekly-digest/ });
    fireEvent.change(screen.getByRole("searchbox", { name: "Find runs" }), {
      target: { value: "digest" },
    });
    expect(screen.queryByRole("button", { name: /SW-A/ })).toBeNull();
    fireEvent.change(screen.getByRole("searchbox", { name: "Find runs" }), {
      target: { value: "nothing-like-it" },
    });
    expect(screen.getByText("No matching runs")).toBeInTheDocument();
    fireEvent.click(screen.getAllByRole("button", { name: "Clear search" })[0]);
    expect(screen.getByRole("button", { name: /SW-A/ })).toBeInTheDocument();
  });

  it("marks the run whose output is open", async () => {
    listRunOutcomes.mockResolvedValue([read("r-1", "2026-10-02T10:00:00Z", "SW-A")]);
    act(() => useAgentResultStore.getState().setTarget("r-1", null, null, "output", "t"));
    render(<RecentRuns teamKey="t" />);
    const row = await screen.findByRole("button", { name: /SW-A/ });
    expect(row).toHaveAttribute("aria-current", "true");
  });

  it("marks the open run only in the team showing it", async () => {
    listRunOutcomes.mockResolvedValue([read("r-1", "2026-10-02T10:00:00Z", "SW-A")]);
    act(() => useAgentResultStore.getState().setTarget("r-1", null, null, "output", "other"));
    render(<RecentRuns teamKey="t" />);
    const row = await screen.findByRole("button", { name: /SW-A/ });
    expect(row).not.toHaveAttribute("aria-current");
  });

  it("refetches when a different session settles even if the settled count is unchanged", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      useManagedSessionStore.setState({ states: { "sess-1": "done" } });
      listRunOutcomes.mockResolvedValue([]);
      render(<RecentRuns teamKey="t" />);
      await waitFor(() => expect(listRunOutcomes).toHaveBeenCalledTimes(1));

      act(() => useManagedSessionStore.setState({ states: { "sess-2": "done" } }));
      await act(async () => {
        await vi.advanceTimersByTimeAsync(1_000);
      });
      expect(listRunOutcomes).toHaveBeenCalledTimes(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it("refetches after a session settles so a just-finished run appears", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      listRunOutcomes.mockResolvedValueOnce([]);
      render(<RecentRuns teamKey="t" />);
      await waitFor(() => expect(listRunOutcomes).toHaveBeenCalledTimes(1));

      listRunOutcomes.mockResolvedValueOnce([read("r-2", "2026-10-02T10:00:00Z", "SW-B")]);
      act(() => useManagedSessionStore.setState({ states: { "sess-2": "done" } }));
      await act(async () => {
        await vi.advanceTimersByTimeAsync(1_000);
      });
      expect(listRunOutcomes).toHaveBeenCalledTimes(2);
      expect(await screen.findByRole("button", { name: /SW-B/ })).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });
});
