// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { markOfAgentStatus } from "./status-mark";
import {
  AGENT_STATUS_WORDS,
  AGENT_STATUSES,
  RUST_AGENT_STATUSES,
  type AgentStatus,
} from "./agent-status-config";
describe("AGENT_STATUS_WORDS — the single vocabulary", () => {
  it("has exactly one entry per status, and AGENT_STATUSES lists them all", () => {
    expect(AGENT_STATUSES).toEqual([
      "running",
      "thinking",
      "tool-running",
      "paused",
      "blocked",
      "awaiting-answer",
      "idle",
      "stalled",
      "unknown",
      "completed",
      "dropped",
      "failed",
      "crashed",
      "done",
      "errored",
    ]);
    expect(Object.keys(AGENT_STATUS_WORDS).sort()).toEqual([...AGENT_STATUSES].sort());
  });

  it("keeps the Rust-mirrored base states a subset of the FE vocabulary", () => {
    expect(RUST_AGENT_STATUSES).toEqual(["running", "paused", "done", "errored"]);
    for (const status of RUST_AGENT_STATUSES) {
      expect(AGENT_STATUSES).toContain(status);
    }
  });

  it("locks the words and the shared mark of every status (one table every surface reads)", () => {
    const contract: Record<AgentStatus, [string, string]> = {
      running: ["Working", "thinking"],
      thinking: ["Thinking", "thinking"],
      "tool-running": ["Running a tool", "tool"],
      paused: ["Needs approval", "permission"],
      blocked: ["Blocked", "blocked"],
      "awaiting-answer": ["Needs answer", "question"],
      idle: ["Ready", "ready"],
      stalled: ["Stalled", "stalled"],
      unknown: ["Activity unknown", "unknown"],
      completed: ["Ended", "ended"],
      dropped: ["Needs you", "question"],
      failed: ["Error", "error"],
      crashed: ["Crashed", "crashed"],
      done: ["Ended", "ended"],
      errored: ["Error", "error"],
    };
    for (const status of AGENT_STATUSES) {
      expect([AGENT_STATUS_WORDS[status], markOfAgentStatus(status)]).toEqual(contract[status]);
    }
  });
});
