// SPDX-License-Identifier: MIT
// SW-XS-10: the setup line on a real workspace record, driven through the
// store the progress listener feeds — the no-flash rule (AC2), the failure
// line and its Retry (AC3 + review r1), two agents in one workspace, and the
// screen-reader contract (AC5).

import { readFileSync } from "fs";
import { resolve } from "path";
import { act, fireEvent, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { render } from "@/test-utils";
import {
  reducePrepareProgress,
  type AgentPrepareState,
} from "@/features/agents/lib/agent-prepare-progress";
import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";
import type { AgentPrepareProgressPayload, RemoteWorkspaceProgressPayload } from "@/tauri/events";

import { clearAgentLaunchRetries, rememberAgentLaunch } from "../lib/agent-launch-retry";
import { step1StateFromProgress, TERMINAL_SERVER_OUTDATED } from "../lib/workspace-setup-state";
import { WorkspaceAnnouncer, WorkspaceSetupLine } from "./setup-line";

const updateServer = vi.hoisted(() => vi.fn());
const cancelled = vi.hoisted(
  () =>
    class MachineActionCancelled extends Error {
      constructor() {
        super("cancelled");
      }
    },
);
vi.mock("@/features/machines/lib/machine-actions", () => ({
  updateMachineTerminalServer: updateServer,
  MachineActionCancelled: cancelled,
}));
const openSheet = vi.hoisted(() => vi.fn());
vi.mock("@/features/agents/lib/launch-sheet", () => ({ openLaunchSheet: openSheet }));

const TEAM = "acme:default";
const WS = "SW-1";

const step1Fixture = JSON.parse(
  readFileSync(
    resolve(__dirname, "../../../tauri/fixtures/remote-workspace-progress.golden.json"),
    "utf-8",
  ),
) as { sequence: RemoteWorkspaceProgressPayload[]; failed: RemoteWorkspaceProgressPayload };

const REMOTE = {
  cwd: "",
  location: {
    kind: "machine" as const,
    machineId: "studio",
    requestId: "r",
    handle: "h-1",
    cwd: "/home/dev/wt",
  },
};

function seed(over: Partial<Workspace> = {}): void {
  useViewerStore.setState({
    workspaces: {
      [TEAM]: [
        {
          id: WS,
          kind: "story",
          cwd: "/wt/SW-1",
          status: "ready",
          storyId: WS,
          storyFilePath: "/ctx/SW-1.md",
          ...over,
        } as Workspace,
      ],
    },
  });
}

const current = () => useViewerStore.getState().workspaces[TEAM][0];

function Line() {
  const ws = useViewerStore((s) => s.workspaces[TEAM][0]);
  return (
    <>
      <WorkspaceAnnouncer ws={ws} />
      <WorkspaceSetupLine ws={ws} teamKey={TEAM} variant="corner" />
    </>
  );
}

let folded = new Map<string, AgentPrepareState>();
function step2(event: Partial<AgentPrepareProgressPayload>): void {
  const agent = event.agent ?? "Barb";
  const next = reducePrepareProgress(folded.get(agent), {
    workspace: "/wt/SW-1",
    agent,
    phase: "diff",
    files: 0,
    bytes: 0,
    total: 3,
    ...event,
  });
  folded.set(agent, next);
  act(() => useViewerStore.getState().applyAgentPrepareProgress(next));
}

const failedSummary = (path: string, why: string) => ({
  status: "failed" as const,
  wanted: 3,
  written: 0,
  bytesWritten: 0,
  unchanged: 0,
  keptModified: [],
  keptModifiedCount: 0,
  keptUnmanaged: [],
  keptUnmanagedCount: 0,
  skippedTracked: [],
  skippedTrackedCount: 0,
  skippedOther: [],
  skippedOtherCount: 0,
  pinnedDrift: [],
  failed: [[path, why]] as [string, string][],
  failedCount: 1,
  leftovers: 0,
  elapsedMs: 10,
});

beforeEach(() => {
  vi.useFakeTimers({ now: 1_000_000, toFake: ["setTimeout", "clearTimeout", "Date"] });
  folded = new Map();
  clearAgentLaunchRetries();
  updateServer.mockReset();
  openSheet.mockClear();
});
afterEach(() => {
  vi.useRealTimers();
});

describe("SW-XS-10 AC2: a no-op Step 2 finishing in 200 ms paints no Preparing label", () => {
  it("during a workspace's setup it goes straight to Ready", () => {
    seed({ setupState: { kind: "awaitingAgent", since: Date.now() } });
    render(<Line />);
    const paints: string[] = [];
    const sample = () => paints.push(document.body.textContent ?? "");
    step2({ phase: "diff" });
    sample();
    for (let t = 0; t < 200; t += 50) {
      act(() => vi.advanceTimersByTime(50));
      sample();
    }
    step2({ phase: "ready" });
    sample();
    for (let t = 0; t < 500; t += 50) {
      act(() => vi.advanceTimersByTime(50));
      sample();
    }
    expect(paints.some((t) => t.includes("Preparing Barb"))).toBe(false);
    expect(screen.getByTestId("setup-line-text").textContent).toBe("Ready");
    act(() => vi.advanceTimersByTime(4000));
    expect(screen.queryByTestId("setup-line-text")).toBeNull();
  });

  it("a slow Step 2 shows Preparing {agent} with its count once 400 ms have passed", () => {
    seed({ setupState: { kind: "awaitingAgent", since: Date.now() } });
    render(<Line />);
    step2({ phase: "diff" });
    act(() => vi.advanceTimersByTime(399));
    expect(screen.getByTestId("setup-line-text").textContent).toContain("Preparing agent context");
    act(() => vi.advanceTimersByTime(2));
    expect(screen.getByTestId("setup-line-text").textContent).toContain("Preparing Barb");
    step2({ phase: "transfer", files: 12, bytes: 4_300_000, total: 8_600_000 });
    expect(screen.getByTestId("setup-line-count").textContent).toBe("12 files · 4.1 MB");
    expect(screen.getByTestId("setup-ring").getAttribute("data-fraction")).toBe("0.500");
  });
});

describe("SW-XS-10 AC3: a failed Step 2 renders the failure line; Retry re-runs Step 2 only", () => {
  it("is an alert with Retry and Details, and Retry runs that agent's launch again", async () => {
    const relaunch = vi.fn().mockResolvedValue({ sessionId: "s" });
    rememberAgentLaunch(TEAM, WS, ["Barb", "builtin/barb"], relaunch);
    const retryCreate = vi.fn();
    useViewerStore.setState({ retryWorkspaceCreate: retryCreate });
    seed({ setupState: { kind: "awaitingAgent", since: Date.now() } });
    render(<Line />);
    step2({ phase: "diff" });
    step2({ phase: "failed", summary: failedSummary("CLAUDE.md", "disk full") });
    expect(screen.getByRole("alert").textContent).toContain(
      "Couldn't prepare Barb — CLAUDE.md couldn't be written: disk full",
    );
    fireEvent.click(screen.getByRole("button", { name: "Details" }));
    expect(screen.getByTestId("setup-line-details").textContent).toContain("disk full");
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    });
    expect(relaunch).toHaveBeenCalledTimes(1);
    expect(retryCreate).not.toHaveBeenCalled();
    expect(current().agentRuns?.Barb).toBeUndefined();
    expect(current().setupState).toMatchObject({ kind: "awaitingAgent" });
  });

  it("review r1: with no recorded launch of that agent, it offers Launch Barb again", () => {
    rememberAgentLaunch(TEAM, WS, ["Mia"], vi.fn());
    seed();
    render(<Line />);
    step2({ phase: "diff" });
    step2({ phase: "failed", note: "boom" });
    expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Launch Barb again" }));
    expect(openSheet).toHaveBeenCalledWith({ teamKey: TEAM, storyId: WS });
  });

  it("review r1 (§11.13): a typed outdated-server failure offers Update terminal server, busy then done, then Retry", async () => {
    let resolveUpdate: () => void = () => {};
    updateServer.mockReturnValue(new Promise<void>((r) => (resolveUpdate = r)));
    rememberAgentLaunch(TEAM, WS, ["Barb"], vi.fn().mockResolvedValue({}));
    seed({
      ...REMOTE,
      agentRuns: {
        Barb: { kind: "failed", agent: "Barb", reason: "too old", code: TERMINAL_SERVER_OUTDATED },
      },
    });
    render(<Line />);
    expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Update terminal server" }));
    expect(updateServer).toHaveBeenCalledWith("studio");
    expect((screen.getByTestId("setup-line-update-server") as HTMLButtonElement).disabled).toBe(
      true,
    );
    expect(screen.getByTestId("setup-line-update-server").textContent).toBe("Updating…");
    await act(async () => resolveUpdate());
    expect(screen.getByText("Terminal server updated")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Retry" })).toBeTruthy();
  });

  it("SW-AGR-19: closing the update confirmation is not an update — the line goes back to offering it", async () => {
    let rejectUpdate: (e: unknown) => void = () => {};
    updateServer.mockReturnValue(new Promise<void>((_, reject) => (rejectUpdate = reject)));
    seed({
      ...REMOTE,
      agentRuns: {
        Barb: { kind: "failed", agent: "Barb", reason: "too old", code: TERMINAL_SERVER_OUTDATED },
      },
    });
    render(<Line />);
    fireEvent.click(screen.getByRole("button", { name: "Update terminal server" }));
    expect(screen.getByTestId("setup-line-update-server").textContent).toBe("Updating…");
    await act(async () => rejectUpdate(new cancelled()));
    expect(screen.queryByText("Terminal server updated")).toBeNull();
    expect(screen.queryByText(/Couldn.t update/)).toBeNull();
    expect(screen.getByTestId("setup-line-update-server").textContent).toBe(
      "Update terminal server",
    );
  });

  it('SW-AGR-19: a failed update says why, never "updated"', async () => {
    updateServer.mockRejectedValue(new Error("the old terminal server was left running"));
    seed({
      ...REMOTE,
      agentRuns: {
        Barb: { kind: "failed", agent: "Barb", reason: "too old", code: TERMINAL_SERVER_OUTDATED },
      },
    });
    render(<Line />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Update terminal server" }));
    });
    expect(screen.queryByText("Terminal server updated")).toBeNull();
    expect(
      screen.getByText(/Couldn.t update: the old terminal server was left running/),
    ).toBeTruthy();
  });

  it("the same words without the typed code offer no server update", () => {
    seed({
      ...REMOTE,
      agentRuns: {
        Barb: { kind: "failed", agent: "Barb", reason: "Update the terminal server on studio" },
      },
    });
    render(<Line />);
    expect(screen.queryByRole("button", { name: "Update terminal server" })).toBeNull();
  });
});

describe("SW-XS-10 review r1 (MUST 2): two agents in one workspace", () => {
  it("retrying Barb never wipes Mia's failure", async () => {
    const barb = vi.fn().mockResolvedValue({});
    const mia = vi.fn().mockResolvedValue({});
    rememberAgentLaunch(TEAM, WS, ["Barb"], barb);
    rememberAgentLaunch(TEAM, WS, ["Mia"], mia);
    seed();
    render(<Line />);
    step2({ agent: "Barb", phase: "diff" });
    step2({ agent: "Mia", phase: "diff" });
    step2({ agent: "Barb", phase: "failed", note: "barb broke" });
    step2({ agent: "Mia", phase: "failed", note: "mia broke" });
    expect(screen.getByRole("alert").textContent).toContain("Couldn't prepare Barb");
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    });
    expect(barb).toHaveBeenCalledTimes(1);
    expect(mia).not.toHaveBeenCalled();
    expect(current().agentRuns?.Mia).toMatchObject({ kind: "failed" });
    expect(screen.getByRole("alert").textContent).toContain("Couldn't prepare Mia — mia broke");
  });

  it("a retry whose launch rejects without an event puts that agent's reason back", async () => {
    rememberAgentLaunch(TEAM, WS, ["Barb"], vi.fn().mockRejectedValue(new Error("still broken")));
    seed();
    render(<Line />);
    step2({ agent: "Barb", phase: "diff" });
    step2({ agent: "Barb", phase: "failed", note: "first" });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    });
    expect(current().agentRuns?.Barb).toMatchObject({ kind: "failed", reason: "still broken" });
  });
});

describe("SW-XS-10 review r1: a Step 1 phase failure renders Retry", () => {
  it("names the phase, and Retry re-runs the create", async () => {
    const retryCreate = vi.fn().mockResolvedValue(undefined);
    useViewerStore.setState({ retryWorkspaceCreate: retryCreate });
    seed({
      status: "creating",
      setupState: step1StateFromProgress(step1Fixture.failed),
    });
    render(<Line />);
    expect(screen.getByRole("alert").textContent).toContain("while copying");
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    });
    expect(retryCreate).toHaveBeenCalledWith(TEAM, WS);
  });
});

describe("SW-XS-10 AC5: one always-mounted status region; each phase once; failures alert", () => {
  it("the region changes per phase, never per count", () => {
    seed({ setupState: step1StateFromProgress(step1Fixture.sequence[0]) });
    render(<Line />);
    const region = screen.getByTestId(`workspace-announcer-${WS}`);
    expect(region.getAttribute("role")).toBe("status");
    const announced: string[] = [region.textContent ?? ""];
    for (const payload of step1Fixture.sequence.slice(1)) {
      act(() =>
        useViewerStore.getState().setWorkspaceSetupState(TEAM, WS, step1StateFromProgress(payload)),
      );
      const text = screen.getByTestId(`workspace-announcer-${WS}`).textContent ?? "";
      if (announced.at(-1) !== text) announced.push(text);
    }
    // Two copying readings (0 and 41 MB) are one announcement.
    expect(announced).toEqual([
      "Fetching",
      "Bundling",
      "Copying",
      "Verifying",
      "Creating worktree",
    ]);
    // The same element the whole time: always mounted.
    expect(screen.getByTestId(`workspace-announcer-${WS}`)).toBe(region);
  });

  it("a failure line is role=alert with real buttons, and the region doesn't repeat it", () => {
    seed({ agentRuns: { Barb: { kind: "failed", agent: "Barb", reason: "boom" } } });
    render(<Line />);
    expect(screen.getByRole("alert").textContent).toContain("Couldn't prepare Barb — boom");
    expect(screen.getByTestId(`workspace-announcer-${WS}`).textContent).toBe("");
    for (const b of screen.getAllByRole("button")) expect(b.tagName).toBe("BUTTON");
  });
});

describe("SW-XS-10 (§11.2): a preparation's kept-modified files persist on the workspace", () => {
  const summary = (keptModified: string[]) => ({
    ...failedSummary("x", "y"),
    status: "ready" as const,
    failed: [],
    failedCount: 0,
    keptModified,
    keptModifiedCount: keptModified.length,
  });

  it("records N files kept (modified) until the next run settles without any", async () => {
    seed();
    const { WorkspaceKeptModified } = await import("./kept-modified-note");
    function Note() {
      const ws = useViewerStore((s) => s.workspaces[TEAM][0]);
      return <WorkspaceKeptModified ws={ws} teamKey={TEAM} />;
    }
    render(<Note />);
    step2({ phase: "diff" });
    step2({ phase: "ready", summary: summary(["CLAUDE.md", "AGENTS.md"]) });
    expect(current().keptModified).toEqual({
      agent: "Barb",
      paths: ["CLAUDE.md", "AGENTS.md"],
      count: 2,
    });
    expect(screen.getByTestId("kept-modified-label").textContent).toBe("2 files kept (modified)");
    step2({ phase: "diff" });
    step2({ phase: "ready", summary: summary([]) });
    expect(current().keptModified).toBeUndefined();
    expect(screen.queryByTestId("kept-modified-note")).toBeNull();
  });
});

describe("SW-XS-10 review r1 (LOW): the region never repeats the operation's verb", () => {
  it("a line turning into a failure leaves the region on what it last said", () => {
    seed({ setupState: { kind: "awaitingAgent", since: Date.now() } });
    function Both() {
      const ws = useViewerStore((s) => s.workspaces[TEAM][0]);
      return <WorkspaceAnnouncer ws={ws} operation="Hydrating" />;
    }
    render(<Both />);
    const region = () => screen.getByTestId(`workspace-announcer-${WS}`).textContent;
    expect(region()).toBe("Preparing agent context");
    step2({ phase: "diff" });
    step2({ phase: "failed", note: "boom" });
    // Not "Hydrating" again: the failure is its own alert.
    expect(region()).toBe("Preparing agent context");
  });
});
