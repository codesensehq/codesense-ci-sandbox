// SPDX-License-Identifier: MIT
import { useMemo } from "react";
import type { RunEvidence, PerNodeResult } from "@/shared/lib/run-evidence";
import { useRunReview, type RunReviewScope } from "./use-run-review";

/**
 * SW-AU-82A — the fetch state of a run's evidence aggregate, as a discriminated union so the view
 * renders each honestly (never a flat enum that collapses cases):
 * - `loading` — first fetch in flight;
 * - `error` — the store read failed / the source was unreadable (distinct from empty);
 * - `unavailable` — the command returned `null`: no recorded outcome (unknown / in-flight);
 * - `permissionGated` — the capability∧ownership gate denied (AD-2). Not produced on the current store
 *   (viewing is always-permit today) but modelled so the view is future-`Principal`-ready and testable;
 * - `ready` — the shared evidence snapshot; retained during a hub resync.
 */
export type RunEvidenceState =
  | { kind: "loading" }
  | { kind: "error"; message: string }
  | { kind: "unavailable" }
  | { kind: "permissionGated"; reason: string }
  | { kind: "ready"; evidence: RunEvidence; stale: boolean };

export interface UseRunEvidence {
  state: RunEvidenceState;
  reload: () => void;
}

/** Evidence, Findings and the review header share one run-scoped hub revision. */
export function useRunEvidence(
  storyId: string | null,
  runId: string | null,
  scope: RunReviewScope,
): UseRunEvidence {
  const { snapshot, error, reload } = useRunReview(
    "runEvidence",
    storyId ? runId : null,
    storyId,
    scope,
  );
  const state = useMemo<RunEvidenceState>(() => {
    if (!storyId || !runId) return { kind: "unavailable" };
    if (error) return { kind: "error", message: error };
    if (!snapshot) return { kind: "loading" };
    const failed = snapshot.snapshot.find((row) => row.error);
    if (failed) return { kind: "error", message: String(failed.error) };
    const base = snapshot.snapshot.find((row) => row.id === "evidence")?.evidence as
      | Omit<RunEvidence, "perNodeResults">
      | undefined;
    if (!base) return { kind: "unavailable" };
    const perNodeResults = snapshot.snapshot
      .filter((row) => row.result)
      .sort((a, b) => Number(a.order) - Number(b.order))
      .map((row) => row.result as PerNodeResult);
    return { kind: "ready", evidence: { ...base, perNodeResults }, stale: false };
  }, [storyId, runId, snapshot, error]);
  return { state, reload };
}
