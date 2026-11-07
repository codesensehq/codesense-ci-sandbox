// SPDX-License-Identifier: MIT
import { useState } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@/test-utils";

import type { RunOutcomeRead } from "@/shared/lib/run-outcome";
import type { AgentSessionRow } from "@/tauri/commands";

const reads = vi.hoisted(() => ({
  outcome: vi.fn(),
  artifact: vi.fn(),
}));
vi.mock("@/tauri/commands", () => ({
  readRunOutcome: reads.outcome,
  readRunArtifact: reads.artifact,
  readRunTranscript: vi.fn().mockResolvedValue(null),
  readRunWorkProduct: vi.fn().mockResolvedValue(null),
  readRunEvidence: vi.fn().mockResolvedValue(null),
  hasRawTerminalSnapshot: vi.fn().mockResolvedValue(false),
  readRawTerminalSnapshot: vi.fn().mockResolvedValue(null),
  readWeftPendingAssertions: vi.fn().mockResolvedValue([]),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: () => Promise.resolve(() => {}) }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));

const onLocate = vi.fn();
vi.mock("@/features/viewer/hooks/use-agent-drill", () => ({
  useAgentDrill: () => ({ onLocate }),
}));

const { AgentRunOutputHost } = await import("./agent-run-output-host");
const { warmScopes } = await import("@/shared/lib/warm-scopes");
const { useAgentResultStore } = await import("@/shared/stores/use-agent-result-store");
const { useAgentListProjectionStore } = await import("@/features/viewer/lib/agent-list-projection");

const TEAM = "acme:backend";

function outcome(runId: string, storyId: string): RunOutcomeRead {
  return {
    outcome: {
      story_id: storyId,
      run_id: runId,
      status: "success",
      summary: "Triaged 3 issues.",
      ended_at: "2026-10-02T10:00:00Z",
      artifact_ref: `${runId}.log`,
    },
    validated: "completed",
  };
}

function session(over: Partial<AgentSessionRow>): AgentSessionRow {
  return {
    handle: "SW-A",
    sessionId: "sess-1",
    kind: "dashboard-launched",
    alive: true,
    backgrounded: false,
    startedWith: null,
    startedAtMs: null,
    launcher: null,
    lastOutputMs: null,
    ...over,
  };
}

/** A stand-in for a live terminal tile: local state that a remount would lose. */
function Tile() {
  const [typed, setTyped] = useState("");
  return (
    <input
      aria-label="terminal"
      value={typed}
      onChange={(e) => setTyped(e.currentTarget.value)}
      data-testid="tile"
    />
  );
}

function renderArea(teamKey = TEAM) {
  return render(
    <div className="relative">
      <Tile />
      <AgentRunOutputHost teamKey={teamKey} repoPath="/repo" />
    </div>,
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  reads.outcome.mockImplementation((runId: string) =>
    Promise.resolve(outcome(runId, "routine:acme/backend/morning-triage@" + runId)),
  );
  reads.artifact.mockResolvedValue("triage transcript");
  useAgentResultStore.getState().close();
  useAgentListProjectionStore.setState({ sessions: [], error: null });
});

describe("AgentRunOutputHost", () => {
  it("renders nothing until a run is opened for this team", () => {
    renderArea();
    expect(screen.queryByTestId("agent-run-output")).toBeNull();
    act(() => useAgentResultStore.getState().setTarget("run-1", null, null, "output", "other:t"));
    expect(screen.queryByTestId("agent-run-output")).toBeNull();
  });

  it("shows the run's name, id and transcript over the tiles", async () => {
    renderArea();
    act(() => useAgentResultStore.getState().setTarget("run-1", null, null, "output", TEAM));

    const region = screen.getByRole("region", { name: "Run output" });
    expect(region).toHaveFocus();
    expect(await screen.findByTestId("agent-run-output-name")).toHaveTextContent("morning-triage");
    expect(region).toHaveTextContent("run-1");
    expect(await screen.findByText("triage transcript")).toBeInTheDocument();
  });

  it("closes back to the same tiles with their state intact", async () => {
    renderArea();
    const tile = screen.getByTestId("tile");
    tile.focus();
    fireEvent.change(tile, { target: { value: "ls -la" } });

    act(() => useAgentResultStore.getState().setTarget("run-1", null, null, "output", TEAM));
    expect(await screen.findByText("triage transcript")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Close run output" }));

    expect(screen.queryByTestId("agent-run-output")).toBeNull();
    expect(useAgentResultStore.getState().runId).toBeNull();
    // Same element, same state, focus handed back: the tile was covered, never remounted.
    expect(screen.getByTestId("tile")).toBe(tile);
    expect(tile).toHaveValue("ls -la");
    expect(tile).toHaveFocus();
  });

  it("takes focus again when its team is shown again, so Escape still closes it", async () => {
    warmScopes.activate(TEAM);
    renderArea();
    screen.getByTestId("tile").focus();
    act(() => useAgentResultStore.getState().setTarget("run-1", null, null, "output", TEAM));
    const region = screen.getByRole("region", { name: "Run output" });
    expect(region).toHaveFocus();
    act(() => warmScopes.activate("other:t"));
    screen.getByTestId("tile").focus();
    act(() => warmScopes.activate(TEAM));
    expect(region).toHaveFocus();
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    expect(screen.queryByTestId("agent-run-output")).toBeNull();
    warmScopes.forget(TEAM);
    warmScopes.forget("other:t");
  });

  it("closes on Escape", async () => {
    renderArea();
    act(() => useAgentResultStore.getState().setTarget("run-1", null, null, "output", TEAM));
    expect(await screen.findByText("triage transcript")).toBeInTheDocument();
    fireEvent.keyDown(screen.getByRole("region", { name: "Run output" }), { key: "Escape" });
    expect(screen.queryByTestId("agent-run-output")).toBeNull();
  });

  it("offers the terminal of a live run and jumps there through the agent reveal", async () => {
    const live = session({ handle: "SW-A", sessionId: "sess-1" });
    useAgentListProjectionStore.setState({ sessions: [live], error: null });
    reads.outcome.mockResolvedValue(null);
    renderArea();
    act(() => useAgentResultStore.getState().setTarget("run-1", "sess-1", null, "output", TEAM));

    fireEvent.click(await screen.findByRole("button", { name: "Go to terminal" }));
    expect(onLocate).toHaveBeenCalledWith(live);
    expect(screen.queryByTestId("agent-run-output")).toBeNull();
  });

  it("offers no terminal for a headless or finished run", async () => {
    useAgentListProjectionStore.setState({
      sessions: [
        session({ sessionId: "sess-h", kind: "headless" }),
        session({ sessionId: "sess-x", alive: false }),
      ],
      error: null,
    });
    renderArea();
    act(() => useAgentResultStore.getState().setTarget("run-h", "sess-h", null, "output", TEAM));
    expect(await screen.findByText("triage transcript")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Go to terminal" })).toBeNull();

    act(() => useAgentResultStore.getState().setTarget("run-x", "sess-x", null, "output", TEAM));
    await waitFor(() => expect(reads.artifact).toHaveBeenCalledWith("run-x"));
    expect(await screen.findByText("triage transcript")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Go to terminal" })).toBeNull();
  });
});
