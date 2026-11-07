// SPDX-License-Identifier: MIT
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type {
  Attempt,
  RunAuthority,
  RunEvidence,
  StepExecution,
  TriggerFact,
} from "@/shared/lib/run-evidence";
import type { RunEvidenceState } from "@/features/viewer/hooks/use-run-evidence";
import { RunEvidenceView } from "@/features/viewer/components/run-evidence-view";

const readRunTranscript = vi.fn();
const readRunArtifact = vi.fn();
vi.mock("@/tauri/commands", () => ({
  readRunTranscript: (...args: unknown[]) => readRunTranscript(...args),
  readRunArtifact: (...args: unknown[]) => readRunArtifact(...args),
}));

function attempt(ordinal: number, runId: string): Attempt {
  return {
    attemptId: `run-1::dev#${ordinal}`,
    attemptOrdinal: ordinal,
    runId,
    result: "passed",
  };
}

function step(attempts: Attempt[]): StepExecution {
  return { stepExecutionId: "run-1::dev", nodeId: "dev", attempts };
}

function evidence(
  trigger: TriggerFact = { kind: "attributed", source: "watch:ci" },
  steps: StepExecution[] = [step([attempt(1, "run-1")])],
  summary?: string,
  opts: {
    authority?: RunAuthority;
    parentRunId?: string;
    endedAt?: string | undefined;
    status?: RunEvidence["header"]["status"];
  } = {},
): RunEvidence {
  return {
    header: {
      runId: "run-1",
      storyId: "SW-AU-82A",
      automationId: "todo->done",
      trigger,
      firedAt: "2026-09-02T00:00:00Z",
      endedAt: "endedAt" in opts ? opts.endedAt : "2026-09-02T00:01:00Z",
      status: "status" in opts ? opts.status : "success",
      summary,
    },
    run: {
      runId: "run-1",
      parentRunId: opts.parentRunId,
      correlationId: "run-1",
      steps,
    },
    perNodeResults: [],
    authority: opts.authority ?? "currentStoreFallback",
  };
}

function ready(ev: RunEvidence, stale = false): RunEvidenceState {
  return { kind: "ready", evidence: ev, stale };
}

beforeEach(() => {
  readRunTranscript.mockReset().mockResolvedValue(null);
  readRunArtifact.mockReset().mockResolvedValue(null);
});

describe("RunEvidenceView fetch states", () => {
  it("renders loading", () => {
    render(<RunEvidenceView state={{ kind: "loading" }} />);
    expect(screen.getByTestId("run-evidence-loading")).toBeInTheDocument();
  });

  it("renders error distinct from empty, with the reason and retry", () => {
    const onReload = vi.fn();
    render(
      <RunEvidenceView
        state={{ kind: "error", message: "3 records skipped" }}
        onReload={onReload}
      />,
    );
    expect(screen.getByTestId("run-evidence-error")).toHaveTextContent("3 records skipped");
    fireEvent.click(screen.getByText("Retry"));
    expect(onReload).toHaveBeenCalled();
  });

  it("renders unavailable (no evidence / in-flight)", () => {
    render(<RunEvidenceView state={{ kind: "unavailable" }} />);
    expect(screen.getByTestId("run-evidence-unavailable")).toBeInTheDocument();
  });

  it("renders permission-gated with the WHY reason (not a bare forbidden)", () => {
    render(<RunEvidenceView state={{ kind: "permissionGated", reason: "read-only source" }} />);
    expect(screen.getByTestId("run-evidence-gated")).toHaveTextContent("read-only source");
  });
});

describe("RunEvidenceView ready", () => {
  it("deep-links the run by id and omits a fabricated event on the pre-envelope store", () => {
    render(<RunEvidenceView state={ready(evidence())} />);
    const region = screen.getByTestId("run-evidence");
    expect(region).toHaveAttribute("data-run-id", "run-1");
    // correlationId == runId today (pre-envelope) → no fabricated `event` deep-link (AC-11 honesty).
    expect(region).not.toHaveAttribute("data-event");
    expect(screen.getByTestId("run-evidence-status")).toHaveTextContent("success");
    expect(screen.getByTestId("run-evidence-header")).toHaveTextContent("SW-AU-82A");
  });

  it("surfaces the event deep-link only when a real correlation id exists", () => {
    const ev = evidence();
    ev.run.correlationId = "evt-42";
    render(<RunEvidenceView state={ready(ev)} />);
    expect(screen.getByTestId("run-evidence")).toHaveAttribute("data-event", "evt-42");
  });

  it("renders an attributed trigger step with its source", () => {
    render(<RunEvidenceView state={ready(evidence({ kind: "attributed", source: "watch:ci" }))} />);
    expect(screen.getByTestId("trigger-attributed")).toHaveTextContent("watch:ci");
  });

  it("renders an honest pre-envelope trigger step, never a fabricated one", () => {
    render(<RunEvidenceView state={ready(evidence({ kind: "preEnvelope" }))} />);
    expect(screen.getByTestId("trigger-pre-envelope")).toBeInTheDocument();
    expect(screen.queryByTestId("trigger-attributed")).not.toBeInTheDocument();
  });

  it("deep-links each step execution by node and step_execution id", () => {
    render(<RunEvidenceView state={ready(evidence())} />);
    const stepEl = screen.getByTestId("step-execution");
    expect(stepEl).toHaveAttribute("data-node-id", "dev");
    expect(stepEl).toHaveAttribute("data-step-execution-id", "run-1::dev");
  });

  it("shows a redaction badge when the summary carries scrubbed content", () => {
    render(<RunEvidenceView state={ready(evidence(undefined, undefined, "leaked [redacted]"))} />);
    expect(screen.getByTestId("redacted-badge-summary")).toBeInTheDocument();
  });

  it("shows the stale banner while refreshing over last-known evidence", () => {
    render(<RunEvidenceView state={ready(evidence(), true)} />);
    expect(screen.getByTestId("run-evidence-stale")).toBeInTheDocument();
  });
});

describe("RunEvidenceView durable authority (SW-AU-82B)", () => {
  it("renders an identical layout with NO provenance cue when the durable authority served it", () => {
    render(
      <RunEvidenceView
        state={ready(evidence(undefined, undefined, undefined, { authority: "durable" }))}
      />,
    );
    expect(screen.getByTestId("run-evidence")).toHaveAttribute("data-authority", "durable");
    expect(screen.queryByTestId("run-evidence-authority-cue")).not.toBeInTheDocument();
  });

  it("adds a quiet 'durable authority: unavailable' cue on the current-store fallback (AC-12)", () => {
    render(
      <RunEvidenceView
        state={ready(
          evidence(undefined, undefined, undefined, { authority: "currentStoreFallback" }),
        )}
      />,
    );
    const cue = screen.getByTestId("run-evidence-authority-cue");
    expect(cue).toHaveTextContent(/durable authority: unavailable/i);
  });

  it("renders a live (non-terminal) durable Run honestly — 'in progress', no Ended row (AC-6)", () => {
    render(
      <RunEvidenceView
        state={ready(
          evidence(undefined, undefined, undefined, {
            authority: "durable",
            status: undefined,
            endedAt: undefined,
          }),
        )}
      />,
    );
    expect(screen.getByTestId("run-evidence-status")).toHaveTextContent(/in progress/i);
    expect(screen.getByTestId("run-evidence-header")).not.toHaveTextContent("Ended");
  });

  it("shows the 'Started by' causation lane under its own heading, linked by parent_run_id (ARCH-INV-20)", () => {
    render(
      <RunEvidenceView
        state={ready(evidence(undefined, undefined, undefined, { parentRunId: "parent-1" }))}
      />,
    );
    const lane = screen.getByTestId("run-evidence-started-by");
    // A distinct, labeled region — causation, not an assignment/subject region.
    expect(lane).toHaveTextContent(/started by/i);
    const link = screen.getByTestId("started-by-link");
    expect(link).toHaveAttribute("data-parent-run-id", "parent-1");
    // Accessible name states kind + target (visible affordance, not hover-only).
    expect(link).toHaveAccessibleName(/parent automation run parent-1/i);
  });

  it("omits the causation lane entirely when there is no parent Run (never a fabricated parent)", () => {
    render(<RunEvidenceView state={ready(evidence())} />);
    expect(screen.queryByTestId("run-evidence-started-by")).not.toBeInTheDocument();
  });
});

describe("attempt selector (retry evidence never lost, AC-3)", () => {
  it("hides the selector for a single attempt", () => {
    render(<RunEvidenceView state={ready(evidence())} />);
    expect(screen.queryByTestId("attempt-selector")).not.toBeInTheDocument();
  });

  it("defaults to the final attempt and labels an older attempt superseded on switch", () => {
    const multi = evidence(undefined, [step([attempt(1, "run-1"), attempt(2, "run-1")])]);
    render(<RunEvidenceView state={ready(multi)} />);
    // Defaults to the final (2 of 2) — the latest attempt is not superseded.
    expect(screen.getByTestId("attempt-position")).toHaveTextContent("Attempt 2 of 2");
    expect(screen.queryByTestId("attempt-superseded")).not.toBeInTheDocument();
    // Switch to the prior attempt — it is honestly labelled superseded, not deleted.
    fireEvent.click(screen.getByLabelText("Previous attempt"));
    expect(screen.getByTestId("attempt-position")).toHaveTextContent("Attempt 1 of 2");
    expect(screen.getByTestId("attempt-superseded")).toBeInTheDocument();
  });
});

describe("per-attempt evidence completeness (AC-8)", () => {
  it("labels a missing body as 'Not captured', never a silent blank", async () => {
    render(<RunEvidenceView state={ready(evidence())} />);
    await waitFor(() =>
      expect(screen.getByTestId("attempt-completeness")).toHaveTextContent("Not captured"),
    );
  });

  it("labels a bounded/truncated + redacted text body and shows the scoped redaction badge", async () => {
    readRunArtifact.mockResolvedValue("output …[512 bytes truncated] token [redacted]\n");
    render(<RunEvidenceView state={ready(evidence())} />);
    await waitFor(() =>
      expect(screen.getByTestId("attempt-completeness")).toHaveTextContent(
        "Bounded — truncated · redacted",
      ),
    );
    expect(screen.getByTestId("redacted-badge-attempt")).toBeInTheDocument();
  });

  it("renders a structured transcript body with its completeness and an in-turn redaction", async () => {
    readRunTranscript.mockResolvedValue({
      turns: [{ role: "assistant", text: "did the thing token [redacted]" }],
    });
    render(<RunEvidenceView state={ready(evidence())} />);
    await waitFor(() =>
      expect(screen.getByTestId("attempt-completeness")).toHaveTextContent(
        "Complete · structured transcript",
      ),
    );
    expect(screen.getByTestId("attempt-completeness")).toHaveTextContent("redacted");
    expect(screen.getByTestId("redacted-badge-attempt")).toBeInTheDocument();
    expect(screen.getByTestId("agent-result-turns")).toBeInTheDocument();
  });
});
