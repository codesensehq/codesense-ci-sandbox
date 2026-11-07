// SPDX-License-Identifier: MIT
import { projectSessionStatus } from "@/test/session-projection-fixture";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@/test-utils";
import { act, fireEvent, renderHook, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import type { RunOutcome } from "@/shared/lib/run-outcome";
import { useAgentResultStore } from "@/shared/stores/use-agent-result-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useOrgStore } from "@/shared/stores/org-store";

import {
  AgentResultContent,
  AgentRunOutputBody,
  closeAgentResult,
  openAgentResult,
  useAgentResult,
  useRawTerminalPresence,
} from "./agent-result-panel";
import { useLocalProfileStore } from "@/shared/stores/local-profile-store";
import { liveHub } from "@/features/dashboard/lib/live-hub";
import type { RunEvidence } from "@/shared/lib/run-evidence";

const runReads = vi.hoisted(() => ({
  outcome: vi.fn().mockResolvedValue(null),
  artifact: vi.fn().mockResolvedValue(null),
  transcript: vi.fn().mockResolvedValue(null),
  workProduct: vi.fn().mockResolvedValue(null),
  rawPresence: vi.fn().mockResolvedValue(false),
  raw: vi.fn().mockResolvedValue(null),
  evidence: vi.fn().mockResolvedValue(null),
}));

beforeEach(() => {
  liveHub.dispose();
  useLocalProfileStore.setState({
    status: {
      active: { principal: "42", displayName: "Tester", machineOwner: null },
      profiles: [],
      privateAuthority: null,
    },
  });
});

const opener = vi.hoisted(() => ({ openUrl: vi.fn().mockResolvedValue(undefined) }));
vi.mock("@tauri-apps/plugin-opener", () => opener);

// The opener drives the viewer store, which persists layout via these commands — stub them so the
// store actions don't reach the real Tauri bridge. `readRun*` are the panel's own read bindings.
vi.mock("@/tauri/commands", () => ({
  readRunOutcome: runReads.outcome,
  readRunArtifact: runReads.artifact,
  readRunTranscript: runReads.transcript,
  readRunWorkProduct: runReads.workProduct,
  readRunEvidence: runReads.evidence,
  liveHubSubscribe: async (topic: string, viewKey: string) => {
    const [, , story, run] = JSON.parse(viewKey.slice("runReview:".length));
    const evidence = topic === "runEvidence" ? await runReads.evidence(story, run) : null;
    return { topic, viewKey, rev: 0, snapshot: evidence ? [{ id: "evidence", evidence }] : [] };
  },
  liveHubUnsubscribe: vi.fn(async () => {}),
  hasRawTerminalSnapshot: runReads.rawPresence,
  readRawTerminalSnapshot: runReads.raw,
  rebuildMenu: vi.fn().mockResolvedValue(undefined),
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  saveScratchNote: vi.fn().mockResolvedValue(undefined),
  loadScratchNote: vi.fn().mockResolvedValue(""),
  // SW-WV-F4: RunDetailViews now mounts the review header (reads pending assertions).
  readWeftPendingAssertions: vi.fn().mockResolvedValue([]),
  wvRatify: vi.fn().mockResolvedValue({ status: "ok" }),
  wvRevoke: vi.fn().mockResolvedValue({ status: "ok" }),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: () => Promise.resolve(() => {}) }));
const toast = vi.hoisted(() => vi.fn());
vi.mock("sonner", () => ({ toast }));

describe("useAgentResult run isolation", () => {
  it("resolves the primary result without awaiting raw-presence discovery", async () => {
    let resolvePresence!: (available: boolean) => void;
    runReads.outcome.mockResolvedValue(null);
    runReads.artifact.mockResolvedValue("primary output");
    runReads.transcript.mockResolvedValue(null);
    runReads.rawPresence.mockImplementation(
      () => new Promise<boolean>((resolve) => (resolvePresence = resolve)),
    );
    const primary = renderHook(() => useAgentResult("run-primary", null));
    renderHook(() => useRawTerminalPresence("run-primary"));
    await waitFor(() => expect(primary.result.current.body).toBe("primary output"));
    expect(primary.result.current.loading).toBe(false);
    await act(async () => resolvePresence(true));
  });

  it("rechecks raw presence once when the durable outcome materializes without polling", async () => {
    const timer = vi.spyOn(globalThis, "setTimeout");
    runReads.rawPresence.mockReset();
    runReads.rawPresence.mockResolvedValueOnce(false).mockResolvedValueOnce(true);
    const presence = renderHook(
      ({ materialized }: { materialized: boolean }) =>
        useRawTerminalPresence("run-race", materialized),
      { initialProps: { materialized: false } },
    );
    await waitFor(() => expect(runReads.rawPresence).toHaveBeenCalledTimes(1));
    expect(presence.result.current.available).toBe(false);
    timer.mockClear();
    presence.rerender({ materialized: true });
    expect(timer).not.toHaveBeenCalled();
    await waitFor(() => expect(presence.result.current.available).toBe(true));
    expect(runReads.rawPresence).toHaveBeenCalledTimes(2);
    timer.mockRestore();
  });

  it("keeps presence errors distinct and succeeds after an explicit retry", async () => {
    runReads.rawPresence
      .mockRejectedValueOnce(new Error("ipc unavailable"))
      .mockResolvedValue(true);
    const presence = renderHook(() => useRawTerminalPresence("run-presence-retry", true));
    await waitFor(() => expect(presence.result.current.error).toBe(true));
    expect(presence.result.current.available).toBe(false);
    const callsBeforeRetry = runReads.rawPresence.mock.calls.length;
    act(() => presence.result.current.retry());
    await waitFor(() => expect(presence.result.current.available).toBe(true));
    expect(runReads.rawPresence.mock.calls.length).toBeGreaterThan(callsBeforeRetry);
  });

  it("hides and ignores an earlier run while the next run loads", async () => {
    let resolveArtifactA!: (body: string | null) => void;
    let resolveArtifactB!: (body: string | null) => void;
    runReads.outcome.mockResolvedValue(null);
    runReads.transcript.mockResolvedValue(null);
    runReads.artifact.mockImplementation(
      (runId: string) =>
        new Promise<string | null>((resolve) => {
          if (runId === "run-a") resolveArtifactA = resolve;
          else resolveArtifactB = resolve;
        }),
    );
    const { result, rerender } = renderHook(
      ({ runId }: { runId: string }) => useAgentResult(runId, null),
      { initialProps: { runId: "run-a" } },
    );

    rerender({ runId: "run-b" });
    expect(result.current.body).toBeNull();
    expect(result.current.loading).toBe(true);
    await act(async () => resolveArtifactA("output from A"));
    expect(result.current.body).toBeNull();
    expect(result.current.loading).toBe(true);
    await act(async () => resolveArtifactB("output from B"));
    await waitFor(() => expect(result.current.body).toBe("output from B"));
  });
});

describe("raw terminal secondary view", () => {
  beforeEach(() => {
    runReads.raw.mockReset();
  });

  it("does not read raw content until the raw tab is selected", async () => {
    runReads.raw.mockResolvedValue("raw bytes");
    render(
      <AgentResultContent
        runId="run-raw"
        outcome={outcome()}
        body="primary"
        loading={false}
        sessionId={null}
        rawAvailable
      />,
    );
    expect(runReads.raw).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("tab", { name: "Raw terminal" }));
    await waitFor(() => expect(runReads.raw).toHaveBeenCalledWith("run-raw"));
    await waitFor(() => expect(screen.getByText("raw bytes")).toBeInTheDocument());
  });

  it("ignores a raw read that resolves after the target changes", async () => {
    let resolveOld!: (body: string | null) => void;
    runReads.raw.mockImplementation((runId: string) =>
      runId === "run-old"
        ? new Promise<string | null>((resolve) => (resolveOld = resolve))
        : Promise.resolve("new raw"),
    );
    const view = render(
      <AgentResultContent
        runId="run-old"
        outcome={outcome()}
        body="old"
        loading={false}
        sessionId={null}
        rawAvailable
      />,
    );
    fireEvent.click(screen.getByRole("tab", { name: "Raw terminal" }));
    view.rerender(
      <AgentResultContent
        runId="run-new"
        outcome={outcome()}
        body="new"
        loading={false}
        sessionId={null}
        rawAvailable
      />,
    );
    fireEvent.click(screen.getByRole("tab", { name: "Raw terminal" }));
    await waitFor(() => expect(screen.getByText("new raw")).toBeInTheDocument());
    await act(async () => resolveOld("stale raw"));
    expect(screen.queryByText("stale raw")).not.toBeInTheDocument();
  });

  it("implements roving keyboard tabs with linked tabpanels", () => {
    runReads.raw.mockImplementation(() => new Promise<string | null>(() => undefined));
    render(
      <AgentResultContent
        runId="run-keys"
        outcome={outcome()}
        body="primary"
        loading={false}
        sessionId={null}
        rawAvailable
      />,
    );
    const resultTab = screen.getByRole("tab", { name: "Transcript" });
    const rawTab = screen.getByRole("tab", { name: "Raw terminal" });
    const resultPanel = screen.getByRole("tabpanel", { name: "Transcript" });
    expect(resultTab).toHaveAttribute("tabindex", "0");
    expect(rawTab).toHaveAttribute("tabindex", "-1");
    expect(resultTab).toHaveAttribute("aria-controls", resultPanel.id);

    fireEvent.keyDown(resultTab, { key: "ArrowRight" });
    expect(rawTab).toHaveFocus();
    expect(rawTab).toHaveAttribute("aria-selected", "true");
    expect(rawTab).toHaveAttribute("tabindex", "0");
    const rawPanel = screen.getByRole("tabpanel", { name: "Raw terminal" });
    expect(rawTab).toHaveAttribute("aria-controls", rawPanel.id);

    fireEvent.keyDown(rawTab, { key: "Home" });
    expect(resultTab).toHaveFocus();
    expect(resultPanel).not.toHaveAttribute("hidden");
    expect(rawPanel).toHaveAttribute("hidden");
  });

  it("keeps both panels mounted so each view retains its scroll state", async () => {
    runReads.raw.mockResolvedValue("raw bytes");
    render(
      <AgentResultContent
        runId="run-scroll"
        outcome={outcome()}
        body="primary"
        loading={false}
        sessionId={null}
        rawAvailable
      />,
    );
    const resultPanel = screen.getByRole("tabpanel", { name: "Transcript" });
    resultPanel.scrollTop = 37;
    fireEvent.click(screen.getByRole("tab", { name: "Raw terminal" }));
    await screen.findByText("raw bytes");
    fireEvent.click(screen.getByRole("tab", { name: "Transcript" }));
    expect(screen.getByRole("tabpanel", { name: "Transcript" })).toBe(resultPanel);
    expect(resultPanel.scrollTop).toBe(37);
  });

  it("shows local loading copy and retries a failed raw read", async () => {
    let rejectFirst!: (reason?: unknown) => void;
    runReads.raw
      .mockImplementationOnce(() => new Promise((_, reject) => (rejectFirst = reject)))
      .mockResolvedValueOnce("recovered raw");
    render(
      <AgentResultContent
        runId="run-retry"
        outcome={outcome()}
        body="primary"
        loading={false}
        sessionId={null}
        rawAvailable
      />,
    );
    fireEvent.click(screen.getByRole("tab", { name: "Raw terminal" }));
    expect(screen.getByText("Loading raw terminal…")).toBeInTheDocument();
    await act(async () => rejectFirst(new Error("read failed")));
    const retry = await screen.findByRole("button", { name: "Retry raw terminal snapshot" });
    fireEvent.click(retry);
    await screen.findByText("recovered raw");
    expect(runReads.raw).toHaveBeenCalledTimes(2);
  });

  it("treats a snapshot missing after presence as a local retryable error", async () => {
    runReads.raw.mockResolvedValue(null);
    render(
      <AgentResultContent
        runId="run-missing"
        outcome={outcome()}
        body="primary"
        loading={false}
        sessionId={null}
        rawAvailable
      />,
    );
    fireEvent.click(screen.getByRole("tab", { name: "Raw terminal" }));
    expect(
      await screen.findByRole("button", { name: "Retry raw terminal snapshot" }),
    ).toBeInTheDocument();
  });

  it("coalesces repeated raw activations while the read is pending", async () => {
    runReads.raw.mockImplementation(() => new Promise<string | null>(() => undefined));
    render(
      <AgentResultContent
        runId="run-pending"
        outcome={outcome()}
        body="primary"
        loading={false}
        sessionId={null}
        rawAvailable
      />,
    );
    const rawTab = screen.getByRole("tab", { name: "Raw terminal" });
    fireEvent.click(rawTab);
    fireEvent.click(rawTab);
    expect(runReads.raw).toHaveBeenCalledTimes(1);
  });

  it("falls back to the mounted transcript and removes orphan tabpanel relationships", () => {
    runReads.raw.mockImplementation(() => new Promise<string | null>(() => undefined));
    const view = render(
      <AgentResultContent
        runId="run-disappears"
        outcome={outcome()}
        body="primary"
        loading={false}
        sessionId={null}
        rawAvailable
      />,
    );
    const transcript = screen.getByRole("tabpanel", { name: "Transcript" });
    transcript.scrollTop = 29;
    fireEvent.click(screen.getByRole("tab", { name: "Raw terminal" }));
    view.rerender(
      <AgentResultContent
        runId="run-disappears"
        outcome={outcome()}
        body="primary"
        loading={false}
        sessionId={null}
        rawAvailable={false}
      />,
    );
    expect(screen.queryByRole("tablist")).not.toBeInTheDocument();
    expect(transcript).not.toHaveAttribute("hidden");
    expect(transcript).not.toHaveAttribute("role");
    expect(transcript).not.toHaveAttribute("aria-labelledby");
    expect(transcript.scrollTop).toBe(29);
  });

  it("offers a local presence retry without blanking the transcript", () => {
    const retry = vi.fn();
    render(
      <AgentResultContent
        runId="run-presence-error"
        outcome={outcome()}
        body="primary"
        loading={false}
        sessionId={null}
        rawPresenceError
        retryRawPresence={retry}
      />,
    );
    expect(screen.getByText("primary")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Retry raw terminal availability" }));
    expect(retry).toHaveBeenCalledTimes(1);
  });

  it("does not remount the primary body when raw availability appears", () => {
    const view = render(
      <AgentResultContent
        runId="run-mount"
        outcome={outcome()}
        body="primary"
        loading={false}
        sessionId={null}
        rawAvailable={false}
      />,
    );
    const primary = screen.getByTestId("agent-result-transcript");
    primary.scrollTop = 41;
    view.rerender(
      <AgentResultContent
        runId="run-mount"
        outcome={outcome()}
        body="primary"
        loading={false}
        sessionId={null}
        rawAvailable
      />,
    );
    expect(screen.getByTestId("agent-result-transcript")).toBe(primary);
    expect(primary.scrollTop).toBe(41);
  });
});

function outcome(over: Partial<RunOutcome> = {}): RunOutcome {
  return {
    story_id: "SW-A",
    run_id: "run-1",
    status: "success",
    summary: "Opened PR #1234: add the thing",
    started_at: "2026-08-26T00:00:00Z",
    ended_at: "2026-08-26T00:00:12Z",
    ...over,
  };
}

describe("AgentResultContent — the four first-class states (AC-6)", () => {
  it("success + output → completed chip, summary, and the transcript body", () => {
    render(
      <AgentResultContent
        runId="run-1"
        outcome={outcome()}
        validated="completed"
        body={"line one\nline two"}
        loading={false}
        sessionId={null}
      />,
    );
    expect(screen.getByRole("img", { name: "Status: ended" })).toBeInTheDocument();
    expect(screen.getByText("Opened PR #1234: add the thing")).toBeInTheDocument();
    expect(screen.getByTestId("agent-result-transcript")).toHaveTextContent("line one");
    expect(screen.getByTestId("agent-result-work-product")).toHaveTextContent(
      "Finding linked change",
    );
    expect(screen.getByTestId("agent-result-timestamps")).toBeInTheDocument();
  });

  it('success + NO output → "No output captured" as a BENIGN success (no transcript, no error state)', () => {
    render(
      <AgentResultContent
        runId="run-1"
        outcome={outcome()}
        validated="completed"
        body={null}
        loading={false}
        sessionId={null}
      />,
    );
    expect(screen.getByRole("img", { name: "Status: ended" })).toBeInTheDocument();
    expect(screen.getByTestId("agent-result-no-output")).toHaveTextContent("No output captured");
    expect(screen.queryByTestId("agent-result-transcript")).not.toBeInTheDocument();
  });

  it('SW-OR-15 (AC-9): declared structured capture + NO turns → "structured unavailable", not misblamed on the agent', () => {
    // An interactive run whose agent CLI DECLARES structured capture but landed no parseable turns (crash /
    // torn / locked session log). No `.log` floor exists for interactive, so this must NOT read as the
    // benign "No output captured" — the copy attributes the shortfall to the run, not the agent.
    render(
      <AgentResultContent
        runId="run-1"
        outcome={outcome({ structured_declared: true })}
        validated="completed"
        body={null}
        transcript={null}
        loading={false}
        sessionId={null}
      />,
    );
    expect(screen.getByTestId("agent-result-structured-unavailable")).toHaveTextContent(
      "Structured transcript unavailable for this run",
    );
    expect(screen.queryByTestId("agent-result-no-output")).not.toBeInTheDocument();
  });

  it('SW-OR-15 (AC-9): NO declared capability + no turns keeps the benign "No output captured"', () => {
    render(
      <AgentResultContent
        runId="run-1"
        outcome={outcome({ structured_declared: null })}
        validated="completed"
        body={null}
        transcript={null}
        loading={false}
        sessionId={null}
      />,
    );
    expect(screen.getByTestId("agent-result-no-output")).toHaveTextContent("No output captured");
    expect(screen.queryByTestId("agent-result-structured-unavailable")).not.toBeInTheDocument();
  });

  // SW-OR-20: a hard failure reads its OWN red `failed` chip on the durable result-panel
  // header, split out of the orange `dropped` a needs-attention run still reads.
  it("failure → failed chip + the transcript tail (the error lives in the tail)", () => {
    render(
      <AgentResultContent
        runId="run-1"
        outcome={outcome({ status: "failed", summary: "agent exited non-zero" })}
        validated="failed"
        body={"…[3 bytes truncated]\nboom: exit 1"}
        loading={false}
        sessionId={null}
      />,
    );
    expect(screen.getByRole("img", { name: "Status: error" })).toBeInTheDocument();
    expect(screen.getByTestId("agent-result-transcript")).toHaveTextContent("boom: exit 1");
  });

  it('in-progress (no durable outcome, sessionId present) → "available on completion" + live enrichment', () => {
    render(
      <AgentResultContent
        runId="run-1"
        outcome={null}
        body={null}
        loading={false}
        sessionId="sess-9"
        liveSummary="working on it"
        liveStatus="running"
      />,
    );
    expect(screen.getByTestId("agent-result-in-progress")).toBeInTheDocument();
    // live summary enriches the header when a sessionId is present.
    expect(screen.getByText("working on it")).toBeInTheDocument();
    expect(screen.getByRole("img", { name: "Status: working" })).toBeInTheDocument();
  });

  it("shows the error from an attested live state before the durable outcome lands", () => {
    render(
      <AgentResultContent
        runId="run-1"
        outcome={null}
        body={null}
        loading={false}
        sessionId="sess-9"
        liveStatus="failed"
      />,
    );
    expect(screen.getByRole("img", { name: "Status: error" })).toBeInTheDocument();
    expect(screen.queryByRole("img", { name: "Status: working" })).not.toBeInTheDocument();
  });

  it("no run targeted → a first-class empty prompt, never a blank body", () => {
    render(
      <AgentResultContent
        runId={null}
        outcome={null}
        body={null}
        loading={false}
        sessionId={null}
      />,
    );
    expect(screen.getByTestId("agent-result-empty")).toBeInTheDocument();
  });
});

describe("AgentResultContent — linked work product", () => {
  beforeEach(() => {
    runReads.workProduct.mockReset().mockResolvedValue(null);
    opener.openUrl.mockReset().mockResolvedValue(undefined);
  });

  it("renders the legacy/none state without consulting mutable story data", async () => {
    render(
      <AgentResultContent
        runId="legacy"
        outcome={outcome()}
        body={null}
        loading={false}
        sessionId={null}
      />,
    );
    await waitFor(() =>
      expect(screen.getByTestId("agent-result-work-product")).toHaveTextContent(
        "No linked code change",
      ),
    );
    expect(runReads.workProduct).toHaveBeenCalledTimes(1);
  });

  it("renders a branch-only result as a copyable native button", async () => {
    const writeText = vi.spyOn(navigator.clipboard, "writeText").mockResolvedValue(undefined);
    runReads.workProduct.mockResolvedValue({ branch: "feat/run-1" });
    render(
      <AgentResultContent
        runId="run-1"
        outcome={outcome()}
        body={null}
        loading={false}
        sessionId={null}
      />,
    );
    const copy = await screen.findByRole("button", { name: "Copy branch feat/run-1" });
    expect(screen.getByText("Working branch")).toBeInTheDocument();
    await userEvent.click(copy);
    expect(writeText).toHaveBeenCalledWith("feat/run-1");
    expect(await screen.findByText("Branch copied")).toHaveAttribute("aria-live", "polite");
    writeText.mockRestore();
  });

  it.each(["http://example.test/pull/7", "not a url"])(
    "does not open an untrusted change URL: %s",
    async (url) => {
      runReads.workProduct.mockResolvedValue({
        branch: "feat/run-1",
        changeRequest: {
          noun: "PR",
          number: 7,
          url,
          state: "open",
        },
      });
      render(
        <AgentResultContent
          runId="run-1"
          outcome={outcome()}
          body={null}
          loading={false}
          sessionId={null}
        />,
      );
      await userEvent.click(await screen.findByRole("button", { name: "Open PR #7 in browser" }));
      expect(opener.openUrl).not.toHaveBeenCalled();
    },
  );

  it("keeps in-flight copy non-animated and does not request enrichment", () => {
    render(
      <AgentResultContent
        runId="run-1"
        outcome={null}
        body={null}
        loading={false}
        sessionId="session-1"
      />,
    );
    expect(screen.getByTestId("agent-result-work-product")).toHaveTextContent(
      "Linked change available when the run completes",
    );
    expect(runReads.workProduct).not.toHaveBeenCalled();
  });

  it("renders a neutral change request and exact zero diff stats and opens HTTPS", async () => {
    runReads.workProduct.mockResolvedValue({
      branch: "feat/run-1",
      changeRequest: {
        noun: "MR",
        number: 42,
        url: "https://gitlab.com/acme/app/-/merge_requests/42",
        state: "open",
        diffStat: { changedFiles: 0, additions: 0, deletions: 0 },
      },
    });
    render(
      <AgentResultContent
        runId="run-1"
        outcome={outcome()}
        body={null}
        loading={false}
        sessionId={null}
      />,
    );
    const link = await screen.findByRole("button", { name: "Open MR #42 in browser" });
    expect(screen.getByTestId("agent-result-work-product")).toHaveTextContent("0 files · +0 · −0");
    link.click();
    expect(opener.openUrl).toHaveBeenCalledWith("https://gitlab.com/acme/app/-/merge_requests/42");
  });

  it("contains browser opener failure with restrained feedback", async () => {
    opener.openUrl.mockRejectedValue(new Error("browser unavailable"));
    runReads.workProduct.mockResolvedValue({
      branch: "feat/run-1",
      changeRequest: {
        noun: "PR",
        number: 7,
        url: "https://github.com/acme/app/pull/7",
        state: "open",
      },
    });
    render(
      <AgentResultContent
        runId="run-1"
        outcome={outcome()}
        body={null}
        loading={false}
        sessionId={null}
      />,
    );

    await userEvent.click(await screen.findByRole("button", { name: "Open PR #7 in browser" }));
    expect(await screen.findByText("Could not open linked change")).toHaveAttribute(
      "aria-live",
      "polite",
    );
  });

  it("keeps stale enrichment from a previous run out of the next run", async () => {
    let resolveOld!: (value: { branch: string }) => void;
    runReads.workProduct.mockImplementation((runId: string) =>
      runId === "old" ? new Promise((resolve) => (resolveOld = resolve)) : Promise.resolve(null),
    );
    const { rerender } = render(
      <AgentResultContent
        runId="old"
        outcome={outcome()}
        body={null}
        loading={false}
        sessionId={null}
      />,
    );
    rerender(
      <AgentResultContent
        runId="new"
        outcome={outcome({ run_id: "new" })}
        body={null}
        loading={false}
        sessionId={null}
      />,
    );
    await act(async () => resolveOld({ branch: "stale-branch" }));
    await waitFor(() =>
      expect(screen.getByTestId("agent-result-work-product")).toHaveTextContent(
        "No linked code change",
      ),
    );
    expect(screen.queryByText("stale-branch")).not.toBeInTheDocument();
  });
});

describe("AgentResultContent — enrichment discipline (AC-5)", () => {
  it("does NOT fold in live enrichment for a completed run (durable outcome is primary)", () => {
    render(
      <AgentResultContent
        runId="run-1"
        outcome={outcome({ summary: "durable summary" })}
        validated="completed"
        body={"x"}
        loading={false}
        // no sessionId → a completed run; liveSummary must be ignored.
        sessionId={null}
        liveSummary="stale live summary"
      />,
    );
    expect(screen.getByText("durable summary")).toBeInTheDocument();
    expect(screen.queryByText("stale live summary")).not.toBeInTheDocument();
  });
});

describe("AgentResultContent — structured transcript rich mode (SW-OR-16 AC-3)", () => {
  it("renders a captured remote transcript without a story outcome", () => {
    render(
      <AgentResultContent
        runId="remote-request"
        outcome={null}
        body={null}
        transcript={{
          session_id: "native-thread",
          turns: [{ role: "assistant", text: "Remote result", tool_calls: [] }],
        }}
        loading={false}
        sessionId={null}
      />,
    );
    expect(screen.getByTestId("agent-result-turns")).toHaveTextContent("Remote result");
  });

  it("renders rich turns (collapsible tool call + usage badge) when a transcript is present, not the plain <pre>", () => {
    render(
      <AgentResultContent
        runId="run-1"
        outcome={outcome()}
        body={"raw stdout fallback"}
        transcript={{
          turns: [
            {
              role: "assistant",
              text: "Reading the file.",
              tool_calls: [{ name: "Read", input: '{"path":"auth.py"}', result: "contents" }],
              usage: { input_tokens: 12, output_tokens: 34 },
            },
          ],
        }}
        loading={false}
        sessionId={null}
      />,
    );
    // Rich mode wins over the plain body: turns render, the <pre> fallback does not.
    expect(screen.getByTestId("agent-result-turns")).toBeInTheDocument();
    expect(screen.queryByTestId("agent-result-transcript")).not.toBeInTheDocument();
    expect(screen.getByTestId("tool-call")).toHaveTextContent("Read");
    expect(screen.getByTestId("turn-usage-badge")).toHaveTextContent("↓12");
    expect(screen.getByTestId("turn-usage-badge")).toHaveTextContent("↑34");
  });

  it("degrades to the plain-text body with a quiet, non-alarming caption when there are no turns", () => {
    render(
      <AgentResultContent
        runId="run-1"
        outcome={outcome()}
        body={"just plain text"}
        transcript={null}
        loading={false}
        sessionId={null}
      />,
    );
    expect(screen.getByTestId("agent-result-transcript")).toHaveTextContent("just plain text");
    // The caption reads as a MODE, never an error — no alert role, muted styling.
    expect(screen.getByTestId("agent-result-plain-caption")).toHaveTextContent(
      "doesn't provide structured turns",
    );
    expect(screen.queryByTestId("agent-result-turns")).not.toBeInTheDocument();
  });

  it("an empty turns list is treated as no-turns → text fallback (never a blank rich body)", () => {
    render(
      <AgentResultContent
        runId="run-1"
        outcome={outcome()}
        body={"fallback body"}
        transcript={{ turns: [] }}
        loading={false}
        sessionId={null}
      />,
    );
    expect(screen.getByTestId("agent-result-transcript")).toHaveTextContent("fallback body");
    expect(screen.queryByTestId("agent-result-turns")).not.toBeInTheDocument();
  });
});

describe("AgentResultContent — robustness", () => {
  it("renders a persisted transcript even when the durable outcome record is absent", () => {
    // A transcript on disk must never be hidden behind a missing/failed outcome write.
    render(
      <AgentResultContent
        runId="run-1"
        outcome={null}
        body={"orphan transcript"}
        loading={false}
        sessionId={null}
      />,
    );
    expect(screen.getByTestId("agent-result-transcript")).toHaveTextContent("orphan transcript");
    expect(screen.queryByTestId("agent-result-no-outcome")).not.toBeInTheDocument();
  });

  it("shows a running chip for an in-flight run whose live state has not landed yet", () => {
    render(
      <AgentResultContent
        runId="run-1"
        outcome={null}
        body={null}
        loading={false}
        sessionId="sess-9"
        // no liveState yet — the chip must still resolve to running, never "No status".
      />,
    );
    expect(screen.getByRole("img", { name: "Status: working" })).toBeInTheDocument();
    expect(screen.queryByText("No status")).not.toBeInTheDocument();
  });

  it("never surfaces a literal 'Invalid Date' when a timestamp is unparseable", () => {
    render(
      <AgentResultContent
        runId="run-1"
        outcome={outcome({ started_at: null, ended_at: "not-a-date" })}
        body={"x"}
        loading={false}
        sessionId={null}
      />,
    );
    expect(screen.queryByText(/Invalid Date/)).not.toBeInTheDocument();
  });

  it("carries the seconds slot at the minute boundary (2m 00s, never 1m 60s)", () => {
    // 119.6s span: rounding the seconds slot alone would render "1m 60s".
    render(
      <AgentResultContent
        runId="run-1"
        outcome={outcome({
          started_at: "2026-08-26T00:00:00.000Z",
          ended_at: "2026-08-26T00:01:59.600Z",
        })}
        body={"x"}
        loading={false}
        sessionId={null}
      />,
    );
    const line = screen.getByTestId("agent-result-timestamps");
    expect(line).toHaveTextContent("2m 00s");
    expect(line).not.toHaveTextContent("1m 60s");
  });

  it("never renders a 60-second slot in the sub-minute branch (59.96s → 1m 00s, not 60.0s)", () => {
    render(
      <AgentResultContent
        runId="run-1"
        outcome={outcome({
          started_at: "2026-08-26T00:00:00.000Z",
          ended_at: "2026-08-26T00:00:59.960Z",
        })}
        body={"x"}
        loading={false}
        sessionId={null}
      />,
    );
    const line = screen.getByTestId("agent-result-timestamps");
    expect(line).not.toHaveTextContent("60.0s");
    expect(line).toHaveTextContent("1m 00s");
  });
});

describe("AgentResultContent — SW-OR-13 live tail + validator-aware completion swap", () => {
  it("in-flight → renders the live tail body from streamed chunks", () => {
    render(
      <AgentResultContent
        runId="run-1"
        outcome={null}
        body={null}
        liveTail={"compiling…\nlinking…"}
        loading={false}
        sessionId="sess-9"
      />,
    );
    // The live tail IS the body while running — same element the persisted artifact later fills.
    expect(screen.getByTestId("agent-result-transcript")).toHaveTextContent("compiling…");
    expect(screen.queryByTestId("agent-result-in-progress")).not.toBeInTheDocument();
  });

  it("on completion the persisted artifact wins over the live tail (the in-place swap)", () => {
    // Once the outcome + persisted body land, the SAME body element renders the artifact, not the
    // now-stale live tail — the live→final swap (AC-1), no separate element / no flicker.
    render(
      <AgentResultContent
        runId="run-1"
        outcome={outcome()}
        validated="completed"
        body={"final persisted transcript"}
        liveTail={"stale live tail"}
        loading={false}
        sessionId="sess-9"
      />,
    );
    const body = screen.getByTestId("agent-result-transcript");
    expect(body).toHaveTextContent("final persisted transcript");
    expect(body).not.toHaveTextContent("stale live tail");
  });

  it("renders the heartbeat alongside the chip while in-flight", () => {
    render(
      <AgentResultContent
        runId="run-1"
        outcome={null}
        body={null}
        liveTail={"working"}
        loading={false}
        sessionId="sess-9"
        heartbeat="last output 3s ago"
      />,
    );
    expect(screen.getByTestId("agent-result-heartbeat")).toHaveTextContent("last output 3s ago");
  });
});

describe("openAgentResult — shows the run in the active team's agent area", () => {
  const TEAM = "acme:backend";
  const emptyLayout = { sections: [] };
  beforeEach(() => {
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "backend" });
    useViewerStore.setState({
      rightPaneLayout: emptyLayout,
      rightPaneOpen: false,
      layoutMode: "viewer",
      terminalVisible: false,
      lastTerminalMode: "console",
    });
    useAgentResultStore.getState().close();
  });

  it("targets the run for the active team and reveals the terminal column", () => {
    openAgentResult({ run_id: "run-7", sessionId: "sess-1" });

    expect(useAgentResultStore.getState()).toMatchObject({
      runId: "run-7",
      sessionId: "sess-1",
      teamKey: TEAM,
    });
    const viewer = useViewerStore.getState();
    expect(viewer.layoutMode).toBe("shared");
    expect(viewer.terminalVisible).toBe(true);
    // The user's saved terminal mode is not rewritten by an open.
    expect(viewer.lastTerminalMode).toBe("console");
  });

  it("leaves the right pane untouched", () => {
    openAgentResult({ run_id: "run-9" });
    const viewer = useViewerStore.getState();
    expect(viewer.rightPaneOpen).toBe(false);
    expect(viewer.rightPaneLayout).toBe(emptyLayout);
    expect(useAgentResultStore.getState()).toMatchObject({ runId: "run-9", sessionId: null });
  });

  it("threads story_id and defaultView so the Evidence view can resolve", () => {
    openAgentResult({ run_id: "run-e", story_id: "SW-A", defaultView: "evidence" });
    expect(useAgentResultStore.getState()).toMatchObject({
      runId: "run-e",
      storyId: "SW-A",
      defaultView: "evidence",
    });
  });

  it("defaults to Output with no story_id when the opener omits them", () => {
    openAgentResult({ run_id: "run-o" });
    expect(useAgentResultStore.getState()).toMatchObject({
      runId: "run-o",
      storyId: null,
      defaultView: "output",
    });
  });
});

describe("closeAgentResult — restores the terminal column the output opened over", () => {
  beforeEach(() => {
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "backend" });
    useAgentResultStore.getState().close();
  });

  function layoutOf() {
    const v = useViewerStore.getState();
    return {
      mode: v.layoutMode,
      visible: v.terminalVisible,
      last: v.lastTerminalMode,
    };
  }

  it("hides the column again when opening the output had to reveal it", () => {
    useViewerStore.setState({
      layoutMode: "viewer",
      terminalVisible: false,
      lastTerminalMode: "console",
    });
    openAgentResult({ run_id: "run-1" });
    expect(layoutOf()).toEqual({ mode: "shared", visible: true, last: "console" });

    closeAgentResult();
    expect(useAgentResultStore.getState().runId).toBeNull();
    expect(layoutOf()).toEqual({ mode: "viewer", visible: false, last: "console" });
  });

  it("leaves an already-visible column as it was", () => {
    useViewerStore.setState({
      layoutMode: "shared",
      terminalVisible: true,
      lastTerminalMode: "shared",
    });
    openAgentResult({ run_id: "run-1" });
    closeAgentResult();
    expect(layoutOf()).toEqual({ mode: "shared", visible: true, last: "shared" });
  });

  it("keeps the first open's answer when another run is opened over it", () => {
    useViewerStore.setState({
      layoutMode: "viewer",
      terminalVisible: false,
    });
    openAgentResult({ run_id: "run-1" });
    openAgentResult({ run_id: "run-2" });
    closeAgentResult();
    expect(layoutOf()).toMatchObject({ mode: "viewer", visible: false });
  });

  it("does not collapse a column the user hid and reopened while the output was open", () => {
    useViewerStore.setState({
      layoutMode: "viewer",
      terminalVisible: false,
    });
    openAgentResult({ run_id: "run-1" });
    useViewerStore.getState().setLayoutMode("viewer");
    useViewerStore.getState().setLayoutMode("shared");
    closeAgentResult();
    expect(layoutOf()).toMatchObject({ mode: "shared", visible: true });
  });

  it("still restores the column as it was before any output when the output moves to another team", () => {
    const OTHER = "acme:frontend";
    useViewerStore.setState({ layoutMode: "viewer", terminalVisible: false });
    openAgentResult({ run_id: "run-a" });
    expect(layoutOf()).toMatchObject({ mode: "shared", visible: true });

    // The column is one layout for every team: the move keeps it shown (it never hides in
    // between) and carries the first open's restore.
    const seen: boolean[] = [];
    const stop = useViewerStore.subscribe((s) => seen.push(s.terminalVisible));
    useOrgStore.setState({ activeTeamName: "frontend" });
    openAgentResult({ run_id: "run-b" });
    stop();
    expect(seen).not.toContain(false);
    expect(useAgentResultStore.getState()).toMatchObject({ runId: "run-b", teamKey: OTHER });
    expect(layoutOf()).toMatchObject({ mode: "shared", visible: true });

    closeAgentResult();
    expect(layoutOf()).toMatchObject({ mode: "viewer", visible: false });
  });

  it("restores a column hidden in shared mode, not only in viewer mode", () => {
    useViewerStore.setState({
      layoutMode: "shared",
      terminalVisible: false,
    });
    openAgentResult({ run_id: "run-1" });
    expect(layoutOf()).toMatchObject({ mode: "shared", visible: true });
    closeAgentResult();
    expect(layoutOf()).toMatchObject({ mode: "shared", visible: false });
  });

  it("restores a never-settled hidden terminal layout without settling it", () => {
    useViewerStore.setState({ layoutMode: null, terminalVisible: false });
    openAgentResult({ run_id: "run-1" });
    expect(layoutOf()).toMatchObject({ mode: "shared", visible: true });
    closeAgentResult();
    expect(layoutOf()).toMatchObject({ mode: null, visible: false });
  });

  it("with no team active, says so instead of opening anything", () => {
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: null });
    const before = useViewerStore.getState();
    openAgentResult({ run_id: "run-1" });
    expect(toast).toHaveBeenCalledWith("Open a team to view this run's output.");
    expect(useAgentResultStore.getState().runId).toBeNull();
    const after = useViewerStore.getState();
    expect(after.layoutMode).toBe(before.layoutMode);
    expect(after.terminalVisible).toBe(before.terminalVisible);
  });

  it("respects a layout the user changed while the output was open", () => {
    useViewerStore.setState({
      layoutMode: "viewer",
      terminalVisible: false,
    });
    openAgentResult({ run_id: "run-1" });
    useViewerStore.getState().setLayoutMode("console");
    closeAgentResult();
    expect(layoutOf()).toMatchObject({ mode: "console", visible: true });
  });
});

const PANEL_EVIDENCE: RunEvidence = {
  header: {
    runId: "run-x",
    storyId: "SW-A",
    trigger: { kind: "attributed", source: "watch:ci" },
    endedAt: "2026-09-02T00:01:00Z",
    status: "success",
  },
  run: { runId: "run-x", correlationId: "run-x", steps: [] },
  perNodeResults: [],
  authority: "currentStoreFallback",
};

describe("AgentRunOutputBody — Output | Evidence mount", () => {
  beforeEach(() => {
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "backend" });
    Object.values(runReads).forEach((m) => m.mockReset());
    runReads.outcome.mockResolvedValue(null);
    runReads.artifact.mockResolvedValue(null);
    runReads.transcript.mockResolvedValue(null);
    runReads.workProduct.mockResolvedValue(null);
    runReads.rawPresence.mockResolvedValue(false);
    runReads.raw.mockResolvedValue(null);
    runReads.evidence.mockResolvedValue(PANEL_EVIDENCE);
    useAgentResultStore.setState({
      runId: null,
      sessionId: null,
      storyId: null,
      defaultView: "output",
    });
  });

  it("the live output header follows its projected session row", async () => {
    useAgentResultStore.getState().setTarget("run-live", "s-live", null, "output");
    projectSessionStatus("s-live", "thinking");
    render(<AgentRunOutputBody teamKey="acme:backend" repoPath="/repo" />);
    expect(await screen.findByText("Thinking")).toBeInTheDocument();
    act(() => projectSessionStatus("s-live", "tool-running"));
    expect(screen.getByText("Running a tool")).toBeInTheDocument();
    act(() => projectSessionStatus("s-live", "awaiting-answer"));
    expect(screen.getByText("Needs answer")).toBeInTheDocument();
  });

  it("threads storyId from the store through the body and resolves Evidence through the shared hub", async () => {
    const user = userEvent.setup();
    useAgentResultStore.getState().setTarget("run-x", null, "SW-A", "output");
    render(<AgentRunOutputBody teamKey="acme:backend" repoPath="/repo" />);

    await user.click(await screen.findByRole("tab", { name: "Evidence" }));

    expect(runReads.evidence).toHaveBeenCalledWith("SW-A", "run-x");
    expect(await screen.findByTestId("run-evidence-header")).toBeInTheDocument();
  });
});
