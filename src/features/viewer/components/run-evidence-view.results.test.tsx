// SPDX-License-Identifier: MIT
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { Attempt, PerNodeResult, RunEvidence, StepExecution } from "@/shared/lib/run-evidence";
import type { RunEvidenceState } from "@/features/viewer/hooks/use-run-evidence";
import { RunEvidenceView, StepExecutionRow } from "@/features/viewer/components/run-evidence-view";

const readRunTranscript = vi.fn();
const readRunArtifact = vi.fn();
vi.mock("@/tauri/commands", () => ({
  readRunTranscript: (...args: unknown[]) => readRunTranscript(...args),
  readRunArtifact: (...args: unknown[]) => readRunArtifact(...args),
}));

beforeEach(() => {
  readRunTranscript.mockReset().mockResolvedValue(null);
  readRunArtifact.mockReset().mockResolvedValue(null);
});

function attempt(runId = "run-1"): Attempt {
  return { attemptId: `${runId}::dev#1`, attemptOrdinal: 1, runId, result: "passed" };
}

function step(nodeId = "dev", attempts: Attempt[] = [attempt()]): StepExecution {
  return { stepExecutionId: `run-1::${nodeId}`, nodeId, attempts };
}

function result(over: Partial<PerNodeResult> = {}): PerNodeResult {
  return { nodeId: "dev", ...over };
}

function row(step?: StepExecution, result?: PerNodeResult) {
  return render(
    <ul>
      <StepExecutionRow step={step} result={result} />
    </ul>,
  );
}

describe("PerNodeResultView — honest empty (AC-1)", () => {
  it("renders one honest line when a step reported no structured result, never a blank scaffold", () => {
    row(step(), undefined);
    expect(screen.getByTestId("node-result-none")).toHaveTextContent(
      "No structured results reported",
    );
    expect(screen.queryByTestId("node-result")).not.toBeInTheDocument();
  });
});

describe("Summary is the default view (AC-2)", () => {
  it("shows the summary and moves the transcript one click down, collapsed", () => {
    row(step(), result({ summary: "created the PR and touched 3 files" }));
    expect(screen.getByTestId("node-result-summary")).toHaveTextContent("created the PR");
    // Transcript is behind a disclosure, not inline.
    expect(screen.getByTestId("transcript-disclosure")).toBeInTheDocument();
    expect(screen.queryByTestId("attempt-evidence")).not.toBeInTheDocument();
  });

  it("does not fetch the transcript until the disclosure is opened (AC-8c)", async () => {
    row(step(), result({ summary: "did the thing" }));
    expect(readRunTranscript).not.toHaveBeenCalled();
    expect(readRunArtifact).not.toHaveBeenCalled();

    const details = screen.getByTestId("transcript-disclosure") as HTMLDetailsElement;
    details.open = true;
    fireEvent(details, new Event("toggle", { bubbles: true }));

    await waitFor(() => expect(readRunTranscript).toHaveBeenCalledWith("run-1"));
  });

  it("moves the transcript down whenever any structured result is reported (not just a summary)", () => {
    row(step(), result({ outputs: [{ key: "k", value: 1 }] }));
    expect(screen.getByTestId("transcript-disclosure")).toBeInTheDocument();
    expect(screen.queryByTestId("attempt-evidence")).not.toBeInTheDocument();
  });

  it("keeps the transcript inline when the step reported nothing (behavior-preserving)", () => {
    row(step(), undefined);
    expect(screen.queryByTestId("transcript-disclosure")).not.toBeInTheDocument();
    expect(screen.getByTestId("attempt-evidence")).toBeInTheDocument();
  });
});

describe("Attempt index is clamped when a reused row gets fewer attempts", () => {
  it("does not surface 'No attempt evidence' when the selected index outruns a smaller step", () => {
    const threeAttempts = step("dev", [attempt("run-a"), attempt("run-b"), attempt("run-c")]);
    const oneAttempt = step("dev", [attempt("run-a")]);
    const { rerender } = render(
      <ul>
        <StepExecutionRow step={threeAttempts} result={undefined} />
      </ul>,
    );
    // Same instance (no key) now receives a step with one attempt — the prior index (2) must clamp.
    rerender(
      <ul>
        <StepExecutionRow step={oneAttempt} result={undefined} />
      </ul>,
    );
    expect(screen.queryByTestId("step-no-attempts")).toBeNull();
    expect(screen.getByTestId("attempt-evidence")).toBeInTheDocument();
  });
});

describe("Typed outputs — table/JSON toggle + large value (AC-4/AC-8b)", () => {
  it("renders a key/value table and toggles to a JSON view", () => {
    row(step(), result({ outputs: [{ key: "count", value: 42 }] }));
    expect(screen.getByTestId("outputs-table")).toHaveTextContent("count");
    fireEvent.click(screen.getByRole("button", { name: "JSON" }));
    const json = screen.getByTestId("outputs-json");
    expect(json).toHaveTextContent('"count"');
    expect(json).toHaveTextContent('"value": 42');
  });

  it("preserves duplicate output keys in the JSON view (no silent collapse)", () => {
    row(
      step(),
      result({
        outputs: [
          { key: "dup", value: "first" },
          { key: "dup", value: "second" },
        ],
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "JSON" }));
    const json = screen.getByTestId("outputs-json");
    expect(json).toHaveTextContent("first");
    expect(json).toHaveTextContent("second");
  });

  it("collapses a large value body behind a disclosure (no eager DOM)", () => {
    const big = "x".repeat(500);
    row(step(), result({ outputs: [{ key: "blob", value: big }] }));
    const disclosure = screen.getByTestId("output-value-json");
    expect(disclosure.tagName.toLowerCase()).toBe("details");
    expect(disclosure).not.toHaveAttribute("open");
  });
});

describe("Artifacts + footprint — kind-driven affordance (AC-3/AC-4)", () => {
  it("links a resolvable pr artifact, labels an entity branch, and shows a file path (no I/O)", () => {
    row(
      step(),
      result({
        summary: "s",
        artifacts: [
          { kind: "pr", reference: "https://github.com/o/r/pull/7", label: "PR #7" },
          { kind: "entity", reference: "o/r@feat-branch", label: "run work product (branch)" },
          { kind: "file", reference: "src/app/App.tsx" },
        ],
      }),
    );
    const artifacts = screen.getByTestId("node-result-artifacts");
    const link = within(artifacts).getByTestId("reference-link");
    expect(link).toHaveAttribute("href", "https://github.com/o/r/pull/7");
    const labels = within(artifacts).getAllByTestId("reference-label");
    expect(labels.some((el) => el.textContent?.includes("run work product (branch)"))).toBe(true);
    expect(labels.some((el) => el.textContent?.includes("src/app/App.tsx"))).toBe(true);
    // The file affordance performs no artifact I/O.
    expect(readRunArtifact).not.toHaveBeenCalled();
  });

  it("renders a file footprint entry as a repo-relative path with a change badge (AC-4)", () => {
    row(
      step(),
      result({
        summary: "s",
        footprint: [{ kind: "file", reference: "src/lib/x.ts", change: "created" }],
      }),
    );
    const fp = screen.getByTestId("footprint-row");
    expect(fp).toHaveAttribute("data-kind", "file");
    expect(fp).toHaveAttribute("data-change", "created");
    expect(within(fp).getByTestId("reference-label")).toHaveTextContent("src/lib/x.ts");
  });

  it("deep-links a resolvable entity footprint entry", () => {
    row(
      step(),
      result({
        summary: "s",
        footprint: [{ kind: "entity", reference: "#run/run-2", change: "modified" }],
      }),
    );
    expect(
      within(screen.getByTestId("footprint-row")).getByTestId("reference-link"),
    ).toHaveAttribute("href", "#run/run-2");
  });

  it("renders an unsafe-scheme reference as a label, never a clickable link (XSS-safe)", () => {
    row(
      step(),
      result({
        summary: "s",
        artifacts: [{ kind: "pr", reference: "javascript:alert(1)" }],
      }),
    );
    const artifacts = screen.getByTestId("node-result-artifacts");
    expect(within(artifacts).queryByTestId("reference-link")).toBeNull();
    expect(within(artifacts).getByTestId("reference-label")).toBeInTheDocument();
  });
});

describe("Scrubbed evidence surfaced, not hidden (AC-5)", () => {
  it("badges a redacted summary and a redacted output value", () => {
    row(
      step(),
      result({
        summary: "token is [redacted]",
        outputs: [{ key: "auth", value: "Bearer [redacted]" }],
      }),
    );
    expect(screen.getByTestId("redacted-badge-result-summary")).toBeInTheDocument();
    expect(screen.getByTestId("redacted-badge-outputs")).toBeInTheDocument();
  });

  it("badges the outputs channel when a redacted value scrolled past the row cap (AC-5 completeness)", () => {
    const many = Array.from({ length: 60 }, (_, i) => ({ key: `k${i}`, value: `v${i}` }));
    // The redacted value is at row 55 — beyond MAX_RENDERED_RESULT_ROWS, so it is not itself rendered.
    many[55] = { key: "secret", value: "Bearer [redacted]" };
    row(step(), result({ outputs: many }));
    expect(screen.getByTestId("redacted-badge-outputs")).toBeInTheDocument();
    expect(screen.getByTestId("outputs-table").textContent).not.toContain("[redacted]");
  });
});

describe("Large channel lists never jank the render (AC-8a)", () => {
  it("windows outputs to the ceiling and shows a visible N-more notice", () => {
    const many = Array.from({ length: 63 }, (_, i) => ({ key: `k${i}`, value: i }));
    row(step(), result({ outputs: many }));
    const rows = within(screen.getByTestId("outputs-table")).getAllByRole("row");
    expect(rows).toHaveLength(50);
    expect(screen.getByTestId("result-more-notice")).toHaveTextContent("13 more outputs");
  });

  it("windows footprint entries and pluralizes the notice correctly (entries, not entrys)", () => {
    const many = Array.from({ length: 52 }, (_, i) => ({
      kind: "file" as const,
      reference: `src/f${i}.ts`,
      change: "created" as const,
    }));
    row(step(), result({ summary: "s", footprint: many }));
    expect(
      within(screen.getByTestId("node-result-footprint")).getAllByTestId("footprint-row"),
    ).toHaveLength(50);
    expect(screen.getByTestId("result-more-notice")).toHaveTextContent("2 more entries not shown");
  });
});

describe("JSON output mode surfaces redaction too (AC-5 consistency)", () => {
  it("keeps the channel redaction badge visible in JSON mode", () => {
    row(step(), result({ outputs: [{ key: "auth", value: "Bearer [redacted]" }] }));
    expect(screen.getByTestId("redacted-badge-outputs")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "JSON" }));
    expect(screen.getByTestId("outputs-json")).toHaveTextContent("[redacted]");
    expect(screen.getByTestId("redacted-badge-outputs")).toBeInTheDocument();
  });
});

// ---- The join reaches the Evidence tab: an orphan result is never dropped (AC-7 / drop landmine) ----

function evidence(steps: StepExecution[], perNodeResults: PerNodeResult[]): RunEvidence {
  return {
    header: { runId: "run-1", storyId: "SW-AU-97", trigger: { kind: "preEnvelope" } },
    run: { runId: "run-1", correlationId: "run-1", steps },
    perNodeResults,
    authority: "durable",
  };
}

function ready(ev: RunEvidence): RunEvidenceState {
  return { kind: "ready", evidence: ev, stale: false };
}

describe("Evidence tab join (AC-7)", () => {
  it("renders a result-bearing nodeId with no matching step as its own orphan row", () => {
    const ev = evidence([step("dev")], [{ nodeId: "agent", summary: "work product" }]);
    render(<RunEvidenceView state={ready(ev)} />);
    const rows = screen.getAllByTestId("step-execution");
    expect(rows).toHaveLength(2);
    const orphan = rows.find((r) => r.getAttribute("data-orphan-result") === "true");
    expect(orphan).toBeDefined();
    expect(orphan).toHaveAttribute("data-node-id", "agent");
    expect(within(orphan as HTMLElement).getByTestId("node-result-summary")).toHaveTextContent(
      "work product",
    );
  });

  it("attaches a matching result to its step by nodeId", () => {
    const ev = evidence([step("dev")], [{ nodeId: "dev", summary: "on the spine" }]);
    render(<RunEvidenceView state={ready(ev)} />);
    const rows = screen.getAllByTestId("step-execution");
    expect(rows).toHaveLength(1);
    expect(screen.getByTestId("node-result-summary")).toHaveTextContent("on the spine");
  });
});
