// SPDX-License-Identifier: MIT
import type { PerNodeResult, StepExecution } from "@/shared/lib/run-evidence";

/**
 * One row of a run's result surface: a step (with its matched reported result, if any), OR an orphan
 * reported result whose `nodeId` joined no step. SW-AU-96 defaults agent reports to `nodeId: "agent"`,
 * which may not be a spine step (e.g. the spine step is `"dev"`) — such a result must still render, so
 * both surfaces render this shape rather than iterating `run.steps` alone.
 */
export interface ResultRow {
  step?: StepExecution;
  result?: PerNodeResult;
}

/**
 * Join reported per-node results onto the run's steps by `nodeId`, preserving step order, then append
 * any result whose `nodeId` matched no step as its own row. A step with no reported result still gets a
 * row (its transcript is its evidence); an orphan result renders on its own so it is never dropped.
 */
export function joinStepsWithResults(
  steps: StepExecution[],
  perNodeResults: PerNodeResult[],
): ResultRow[] {
  const byNode = new Map<string, PerNodeResult>();
  for (const result of perNodeResults) byNode.set(result.nodeId, result);

  const claimed = new Set<string>();
  const rows: ResultRow[] = steps.map((step) => {
    const result = byNode.get(step.nodeId);
    if (result) claimed.add(step.nodeId);
    return { step, result };
  });

  const seenOrphan = new Set<string>();
  for (const result of perNodeResults) {
    if (claimed.has(result.nodeId) || seenOrphan.has(result.nodeId)) continue;
    seenOrphan.add(result.nodeId);
    rows.push({ result });
  }
  return rows;
}

/** The per-channel counts a node card / affordance shows (SW-AU-97 AC-1). */
export interface ResultCounts {
  artifacts: number;
  outputs: number;
  footprint: number;
}

export function resultCounts(result: PerNodeResult | undefined): ResultCounts {
  return {
    artifacts: result?.artifacts?.length ?? 0,
    outputs: result?.outputs?.length ?? 0,
    footprint: result?.footprint?.length ?? 0,
  };
}

/** True when a result carries any reported channel — otherwise the honest "no results" line renders. */
export function hasReportedResult(result: PerNodeResult | undefined): boolean {
  if (!result) return false;
  const counts = resultCounts(result);
  return (
    Boolean(result.summary?.trim()) ||
    counts.artifacts > 0 ||
    counts.outputs > 0 ||
    counts.footprint > 0
  );
}

/**
 * Whether a reference renders as a clickable link vs a labeled (non-clickable) reference. Absolute
 * http(s) URLs and in-app hash deep-links are resolvable; a work-product branch ref (`owner/repo@branch`)
 * or a repo-relative file path is not — those render as a labeled reference (AC-3/AC-4). A `file` kind
 * always renders as a path label regardless of this check. Case-insensitive on the scheme; a bare `"#"`
 * (or `"# "`) is not a usable deep-link, so require at least one non-space char after it.
 */
export function isResolvableReference(reference: string): boolean {
  return isExternalUrl(reference) || /^#\S/.test(reference);
}

/** An absolute http(s) URL (case-insensitive) — the only reference we open in a new tab. */
export function isExternalUrl(reference: string): boolean {
  return /^https?:\/\//i.test(reference);
}

/**
 * The scrub sentinels SW-AU-96 writes into evidence at write time (mirrors Rust
 * `secret_scan::REDACTION_PLACEHOLDER` and the run-request bounding marker). The FE only SURFACES these —
 * it never re-scrubs (AD-2). One authoritative copy so no channel drifts from the write contract.
 */
export const REDACTION_MARKER = "[redacted]";
export const TRUNCATION_MARKER = "[truncated]";

/** Whether already-scrubbed text carries the redaction sentinel — surfaced as a visible badge (AC-5). */
export function hasRedactionMarker(text: string | null | undefined): boolean {
  return text?.includes(REDACTION_MARKER) ?? false;
}
