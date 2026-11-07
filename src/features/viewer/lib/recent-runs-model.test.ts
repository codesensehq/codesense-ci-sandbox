// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import type { RunOutcomeRead } from "@/shared/lib/run-outcome";

import { RECENT_RUNS_CAP, runLabel, selectRecentRuns } from "./recent-runs-model";

function read(
  runId: string,
  endedAt: string,
  over: Partial<RunOutcomeRead["outcome"]> = {},
  structured = false,
): RunOutcomeRead {
  return {
    outcome: {
      story_id: `story-${runId}`,
      run_id: runId,
      status: "success",
      ended_at: endedAt,
      artifact_ref: `${runId}.log`,
      ...over,
    },
    validated: "completed",
    has_structured_transcript: structured,
  };
}

describe("selectRecentRuns", () => {
  it("keeps only runs that left output, newest first", () => {
    const runs = selectRecentRuns([
      read("old", "2026-10-01T10:00:00Z"),
      read("silent", "2026-10-02T09:00:00Z", { artifact_ref: null }),
      read("turns", "2026-10-02T08:00:00Z", { artifact_ref: null }, true),
      read("new", "2026-10-02T10:00:00Z"),
    ]);
    expect(runs.map((r) => r.outcome.run_id)).toEqual(["new", "turns", "old"]);
  });

  it("caps the list and breaks equal end times by run id", () => {
    const same = "2026-10-02T10:00:00Z";
    const many = Array.from({ length: RECENT_RUNS_CAP + 5 }, (_, i) =>
      read(`run-${String(i).padStart(2, "0")}`, same),
    );
    const runs = selectRecentRuns(many);
    expect(runs).toHaveLength(RECENT_RUNS_CAP);
    expect(runs[0].outcome.run_id).toBe("run-00");
  });

  it("sorts an unparseable end time last instead of dropping it", () => {
    const runs = selectRecentRuns([read("bad", "not-a-date"), read("ok", "2026-10-02T10:00:00Z")]);
    expect(runs.map((r) => r.outcome.run_id)).toEqual(["ok", "bad"]);
  });
});

describe("runLabel", () => {
  it("names a routine run by its routine id", () => {
    expect(runLabel("routine:acme/backend/weekly-digest@digest-run-3")).toBe("weekly-digest");
  });

  it("formats any other handle like a live agent row", () => {
    expect(runLabel("SW-DX-3#attempt-2")).toBe("SW-DX-3 · Attempt 2");
  });

  it("returns null with no handle", () => {
    expect(runLabel(null)).toBeNull();
    expect(runLabel("  ")).toBeNull();
  });
});
