// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import type { RoutineRun } from "@/tauri/commands";

import {
  UNKNOWN_COPY,
  canRetry,
  queuedCopy,
  runLine,
  runStatus,
  unknownCause,
  type RunContext,
} from "./run-status";

// SW-AGR-5 (design §4.7): one status vocabulary for every run row, drawn with the shared
// status mark, and the word always sits beside it.

const CTX: RunContext = { agent: "Barb", machine: "codesense-test-1", maxWait: "30 min" };

function run(overrides: Partial<RoutineRun> = {}): RoutineRun {
  return {
    run_id: "run_8f2c",
    routine_key: "acme/backend/inbox-sweep",
    revision: 1,
    source: "schedule",
    occurrence_key: null,
    phase: "succeeded",
    reason: null,
    linked_run_id: null,
    session_id: "s-barb",
    created_at: 1,
    started_at: 2,
    ended_at: 3,
    result_text: null,
    agent: null,
    ...overrides,
  };
}

describe("runStatus", () => {
  it.each([
    // A running routine proves activity, not an open tool.
    ["delivered", "Delivered", "ended"],
    ["queued", "Queued", "ready"],
    ["running", "Working", "thinking"],
    ["coalesced", "Merged", "ended"],
    ["succeeded", "Succeeded", "ended"],
    ["failed", "Error", "error"],
    ["timed_out", "Timed out", "error"],
    ["needs_you", "Needs you", "question"],
    ["skipped", "Skipped", "ended"],
    ["cancelled", "Cancelled", "ended"],
    ["ended", "Ended", "ended"],
    ["undelivered", "Undelivered", "error"],
    ["unknown", "Unknown", "unknown"],
  ] as const)("%s reads %s, the %s mark", (phase, word, mark) => {
    expect(runStatus({ phase })).toMatchObject({ word, mark });
  });

  it("a woken run that is still open reads Delivered, not Working", () => {
    expect(runStatus({ phase: "running", delivered_at: 5 })).toMatchObject({
      status: "delivered",
      word: "Delivered",
      mark: "ended",
    });
  });

  it("each phase draws the shared status mark", () => {
    expect(runStatus({ phase: "running" })).toMatchObject({ word: "Working", mark: "thinking" });
    expect(runStatus({ phase: "needs_you" }).mark).toBe("question");
    expect(runStatus({ phase: "unknown" }).mark).toBe("unknown");
    expect(runStatus({ phase: "undelivered" }).mark).toBe("error");
    expect(runStatus({ phase: "failed" }).mark).toBe("error");
    expect(runStatus({ phase: "succeeded" }).mark).toBe("ended");
    expect(runStatus({ phase: "skipped" }).mark).toBe("ended");
  });
});

describe("runLine (the honest copy under a status)", () => {
  it("unknown is the binding sentence, verbatim", () => {
    expect(UNKNOWN_COPY).toBe("May have been delivered. Check the terminal before re-sending.");
    expect(runLine(run({ phase: "unknown" }), CTX)).toBe(UNKNOWN_COPY);
  });

  it.each([
    ["busy_timeout", "Barb stayed busy for 30 min"],
    ["needs_preparation", "The workspace on codesense-test-1 isn't prepared yet"],
    ["machine_offline", "codesense-test-1 was offline"],
    ["machine_unknown", "codesense-test-1 isn't shared with this team"],
    ["session_ended", "Barb's session ended before it was idle"],
    ["unobservable", "CodeSense can't tell when Barb is idle"],
    ["agent_failed", "Barb's session had stopped with an error"],
    ["archived", "Barb was archived"],
    ["server_restarted", "The terminal server on codesense-test-1 restarted"],
    ["limit_reached", "Barb's session has had all the wakes it can take"],
    ["draft_pending", "Someone was typing in Barb's session the whole time"],
    ["unavailable", "codesense-test-1 stopped taking wakes"],
    ["denied", "Barb runs without permission checks and doesn't allow wakes"],
  ])("undelivered(%s) reads %j", (reason, copy) => {
    expect(runLine(run({ phase: "undelivered", reason }), CTX)).toBe(copy);
  });

  it("an undelivered reason it does not know still says something honest", () => {
    expect(runLine(run({ phase: "undelivered", reason: "new_reason" }), CTX)).toBe(
      "Undelivered — new_reason",
    );
  });

  it("a merged fire points at the wake already waiting", () => {
    expect(runLine(run({ phase: "coalesced", linked_run_id: "run_1" }), CTX)).toBe(
      "Merged into the wake already waiting",
    );
  });

  it("queued names who it waits for and until when", () => {
    expect(queuedCopy("Barb", "9:30")).toBe("Waiting for Barb to be idle · until 9:30");
    expect(queuedCopy("Barb", null)).toBe("Waiting for Barb to be idle");
  });

  it("a fresh run keeps the incumbent reason copy", () => {
    expect(runLine(run({ phase: "skipped", reason: "already_running" }), CTX)).toBe(
      "Skipped — previous run still running",
    );
    expect(runLine(run({ phase: "succeeded" }), CTX)).toBeNull();
  });
});

describe("unknownCause (the Details popover's Result, never an enum)", () => {
  it("a remote wake: the machine didn't confirm the paste", () => {
    expect(unknownCause(run({ phase: "unknown", wake_machine: "codesense-test-1" }))).toBe(
      "The machine didn't confirm the paste.",
    );
  });
  it("a local wake: the paste wasn't confirmed", () => {
    expect(unknownCause(run({ phase: "unknown", wake_machine: "local" }))).toBe(
      "The paste wasn't confirmed.",
    );
  });
  it("an evicted delivery: the machine no longer remembers it", () => {
    expect(
      unknownCause(
        run({
          phase: "unknown",
          wake_machine: "codesense-test-1",
          result_text: "idempotency key evicted",
        }),
      ),
    ).toBe("The machine no longer remembers this delivery.");
  });
});

describe("canRetry (R5)", () => {
  it("never for unknown: the text may already be in the session", () => {
    expect(canRetry(run({ phase: "unknown" }))).toBe(false);
  });
  it("never where trying again cannot help", () => {
    for (const reason of [
      "machine_unknown",
      "archived",
      "limit_reached",
      "machine_offline",
      "unobservable",
    ]) {
      expect(canRetry(run({ phase: "undelivered", reason }))).toBe(false);
    }
  });
  it("an undelivered wake that could land next time may run again", () => {
    for (const reason of ["busy_timeout", "session_ended", "server_restarted"]) {
      expect(canRetry(run({ phase: "undelivered", reason }))).toBe(true);
    }
  });
});
