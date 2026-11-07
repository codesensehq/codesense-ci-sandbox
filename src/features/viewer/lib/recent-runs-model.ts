// SPDX-License-Identifier: MIT
import { formatSessionHandle } from "@/features/viewer/lib/agent-row-presentation";
import type { RunOutcomeRead } from "@/shared/lib/run-outcome";

/** How many finished runs the Agents pane's run history keeps. */
export const RECENT_RUNS_CAP = 50;

/** A run has output to show when it persisted a transcript (plain text or structured turns). */
function runHasOutput(read: RunOutcomeRead): boolean {
  return Boolean(read.outcome.artifact_ref) || read.has_structured_transcript === true;
}

function endedMs(read: RunOutcomeRead): number {
  const ms = Date.parse(read.outcome.ended_at);
  return Number.isFinite(ms) ? ms : Number.NEGATIVE_INFINITY;
}

/**
 * The finished runs worth reopening: those that left output, newest first, capped. Ties break on
 * `run_id` so a refetch never shuffles rows that ended in the same millisecond.
 */
export function selectRecentRuns(
  outcomes: readonly RunOutcomeRead[],
  cap: number = RECENT_RUNS_CAP,
): RunOutcomeRead[] {
  return outcomes
    .filter(runHasOutput)
    .sort((a, b) => endedMs(b) - endedMs(a) || a.outcome.run_id.localeCompare(b.outcome.run_id))
    .slice(0, cap);
}

const ROUTINE_HANDLE = /^routine:(.+)@[^@]*$/;

/**
 * A readable name for a run from the handle it ran under: a routine run reads as its routine's
 * id, anything else through the same handle formatter the live agent rows use.
 */
export function runLabel(storyId: string | null | undefined): string | null {
  const handle = storyId?.trim();
  if (!handle) return null;
  const routine = ROUTINE_HANDLE.exec(handle);
  if (routine) return routine[1].split("/").filter(Boolean).pop() ?? routine[1];
  return formatSessionHandle(handle);
}

/** What kind of run a finished run was. */
export type RunKind = "Routine" | "Agent" | "Headless";

/**
 * A finished run's kind: a routine's run (by its handle or automation), an interactive agent
 * session that declared a structured transcript, or a headless run.
 */
export function runKind(read: RunOutcomeRead): RunKind {
  const { outcome } = read;
  if (outcome.automation_id || ROUTINE_HANDLE.test(outcome.story_id?.trim() ?? "")) {
    return "Routine";
  }
  if (outcome.structured_declared || read.has_structured_transcript) return "Agent";
  return "Headless";
}
