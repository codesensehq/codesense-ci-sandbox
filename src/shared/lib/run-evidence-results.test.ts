// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import type { PerNodeResult, StepExecution } from "@/shared/lib/run-evidence";
import {
  hasRedactionMarker,
  hasReportedResult,
  isExternalUrl,
  isResolvableReference,
  joinStepsWithResults,
  resultCounts,
} from "@/shared/lib/run-evidence-results";

function step(nodeId: string): StepExecution {
  return { stepExecutionId: `run-1::${nodeId}`, nodeId, attempts: [] };
}

function result(nodeId: string, over: Partial<PerNodeResult> = {}): PerNodeResult {
  return { nodeId, ...over };
}

describe("joinStepsWithResults", () => {
  it("matches a result to its step by nodeId, preserving step order", () => {
    const steps = [step("trigger"), step("dev")];
    const results = [result("dev", { summary: "did the thing" })];

    const rows = joinStepsWithResults(steps, results);

    expect(rows).toHaveLength(2);
    expect(rows[0]).toEqual({ step: steps[0], result: undefined });
    expect(rows[1]).toEqual({ step: steps[1], result: results[0] });
  });

  it("keeps an orphan result (nodeId matches no step) as its own row — never dropped", () => {
    const steps = [step("dev")];
    // The agent auto-capture defaults to nodeId "agent", which is not the spine step "dev".
    const orphan = result("agent", { summary: "work product on a branch" });

    const rows = joinStepsWithResults(steps, [orphan]);

    expect(rows).toHaveLength(2);
    expect(rows[0]).toEqual({ step: steps[0], result: undefined });
    expect(rows[1]).toEqual({ step: undefined, result: orphan });
  });

  it("does not double-render a claimed nodeId as an orphan", () => {
    const steps = [step("dev")];
    const rows = joinStepsWithResults(steps, [result("dev")]);
    expect(rows).toHaveLength(1);
    expect(rows[0].step).toBe(steps[0]);
  });

  it("dedupes multiple orphan results sharing a nodeId", () => {
    const rows = joinStepsWithResults(
      [],
      [result("agent", { summary: "a" }), result("agent", { summary: "b" })],
    );
    expect(rows).toHaveLength(1);
  });

  it("returns a row per step even when no results were reported", () => {
    const steps = [step("a"), step("b")];
    const rows = joinStepsWithResults(steps, []);
    expect(rows.map((r) => r.step)).toEqual(steps);
    expect(rows.every((r) => r.result === undefined)).toBe(true);
  });
});

describe("resultCounts", () => {
  it("counts each channel; treats undefined and absent channels as zero", () => {
    expect(resultCounts(undefined)).toEqual({ artifacts: 0, outputs: 0, footprint: 0 });
    expect(
      resultCounts(
        result("dev", {
          artifacts: [{ kind: "pr", reference: "https://x/1" }],
          outputs: [{ key: "k", value: 1 }],
          footprint: [
            { kind: "file", reference: "a.ts", change: "created" },
            { kind: "file", reference: "b.ts", change: "modified" },
          ],
        }),
      ),
    ).toEqual({ artifacts: 1, outputs: 1, footprint: 2 });
  });
});

describe("hasReportedResult", () => {
  it("is false for undefined or a fully-empty result, true for any reported channel", () => {
    expect(hasReportedResult(undefined)).toBe(false);
    expect(hasReportedResult(result("dev"))).toBe(false);
    expect(hasReportedResult(result("dev", { summary: "x" }))).toBe(true);
    expect(hasReportedResult(result("dev", { outputs: [{ key: "k", value: 0 }] }))).toBe(true);
  });

  it("treats a whitespace-only summary as nothing reported (no empty Summary block)", () => {
    expect(hasReportedResult(result("dev", { summary: "   " }))).toBe(false);
    expect(hasReportedResult(result("dev", { summary: "\n\t" }))).toBe(false);
  });
});

describe("hasRedactionMarker", () => {
  it("detects the scrub sentinel and tolerates null/undefined", () => {
    expect(hasRedactionMarker("token: [redacted]")).toBe(true);
    expect(hasRedactionMarker("nothing here")).toBe(false);
    expect(hasRedactionMarker(undefined)).toBe(false);
    expect(hasRedactionMarker(null)).toBe(false);
  });
});

describe("isResolvableReference", () => {
  it("links http(s) URLs and in-app hash deep-links, labels branch refs and file paths", () => {
    expect(isResolvableReference("https://github.com/o/r/pull/1")).toBe(true);
    expect(isResolvableReference("http://example.com")).toBe(true);
    expect(isResolvableReference("#run/run-2")).toBe(true);
    expect(isResolvableReference("owner/repo@feat-branch")).toBe(false);
    expect(isResolvableReference("src/features/x.ts")).toBe(false);
  });

  it("rejects a bare/empty hash and unsafe schemes, and is case-insensitive on http", () => {
    expect(isResolvableReference("#")).toBe(false);
    expect(isResolvableReference("# ")).toBe(false);
    expect(isResolvableReference("HTTPS://EXAMPLE.COM")).toBe(true);
    expect(isResolvableReference("javascript:alert(1)")).toBe(false);
    expect(isResolvableReference("data:text/html;base64,x")).toBe(false);
    expect(isResolvableReference("//evil.example.com")).toBe(false);
  });
});

describe("isExternalUrl", () => {
  it("matches only absolute http(s) URLs, case-insensitive", () => {
    expect(isExternalUrl("http://x")).toBe(true);
    expect(isExternalUrl("HTTPS://x")).toBe(true);
    expect(isExternalUrl("#run/x")).toBe(false);
    expect(isExternalUrl("httpfoo://x")).toBe(false);
  });
});
