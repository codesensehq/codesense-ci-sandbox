// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { MARK_WORDS, STATUS_MARKS } from "./status-mark";
import {
  exitWords,
  formatAge,
  formatEndedLabel,
  agentStatusWord,
  agentStatusWords,
  shownSubagents,
  subagentCountWords,
  formatMarkLabel,
  msUntilAgeChanges,
} from "./terminal-dot-words";

const S = 1000;
const M = 60 * S;
const H = 60 * M;

describe("a subagent count", () => {
  it("is a whole number above zero: anything else reads as no subagents, the same in every word", () => {
    for (const bad of [Number.NaN, -2, 1.5, Number.POSITIVE_INFINITY, 0]) {
      expect(shownSubagents("running", bad), String(bad)).toBe(0);
      expect(agentStatusWord("running", bad), String(bad)).toBe(agentStatusWords("running"));
      expect(agentStatusWords("running", bad), String(bad)).toBe(agentStatusWords("running"));
      expect(formatMarkLabel({ mark: "thinking", subagents: bad }), String(bad)).toBe("Thinking");
    }
    expect(shownSubagents("running", 2)).toBe(2);
    expect(agentStatusWord("running", 2)).toBe("Working");
    expect(agentStatusWords("running", 2)).toBe("Working · 2 subagents");
  });
});

describe("formatAge", () => {
  it("phrases whole seconds, then minutes, hours and days, rounding down", () => {
    expect(formatAge(0)).toBe("0s");
    expect(formatAge(14_900)).toBe("14s");
    expect(formatAge(59_999)).toBe("59s");
    expect(formatAge(M)).toBe("1m");
    expect(formatAge(2 * M + 59 * S)).toBe("2m");
    expect(formatAge(H - 1)).toBe("59m");
    expect(formatAge(3 * H + 30 * M)).toBe("3h");
    expect(formatAge(24 * H)).toBe("1d");
  });

  it("a clock that ran backwards reads 0s, not a negative age", () => {
    expect(formatAge(-5_000)).toBe("0s");
  });
});

describe("msUntilAgeChanges", () => {
  it("is the time until the phrase next changes, in the current unit", () => {
    expect(msUntilAgeChanges(0)).toBe(S);
    expect(msUntilAgeChanges(14_250)).toBe(750);
    expect(msUntilAgeChanges(59_500)).toBe(500);
    expect(msUntilAgeChanges(M)).toBe(M);
    expect(msUntilAgeChanges(2 * M + 30 * S)).toBe(30 * S);
    expect(msUntilAgeChanges(H + M)).toBe(H - M);
  });

  it("waking at that time always changes the phrase", () => {
    for (const age of [0, 999, 1_000, 59_001, 61_000, 3_599_999, 3_600_000, 86_399_000]) {
      expect(formatAge(age + msUntilAgeChanges(age)), String(age)).not.toBe(formatAge(age));
    }
  });
});

describe("exitWords", () => {
  it("names a signal, else the exit code, else nothing", () => {
    expect(exitWords({ code: 137, signal: null })).toBe("exit 137");
    expect(exitWords({ code: null, signal: "SIGKILL" })).toBe("SIGKILL");
    expect(exitWords({ code: 1, signal: "SIGTERM" })).toBe("SIGTERM");
    expect(exitWords({ code: null })).toBeNull();
    // A platform's signal text is named by its number.
    expect(exitWords({ code: null, signal: "Killed: 9" })).toBe("SIGKILL");
    expect(exitWords({ code: null, signal: "Segmentation fault: 11" })).toBe("SIGSEGV");
  });
});

describe("formatEndedLabel", () => {
  it("says how a tab's own process ended, the same everywhere", () => {
    expect(formatEndedLabel("crashed", { code: null, signal: "Killed: 9" })).toBe(
      "Crashed · SIGKILL",
    );
    expect(formatEndedLabel("ended", { code: 0, signal: null })).toBe("Ended · exit 0");
    expect(formatEndedLabel("ended", { code: 130, signal: null }, ", ")).toBe("Ended, exit 130");
    expect(formatEndedLabel("ended", null)).toBe("Ended");
  });
});

describe("formatMarkLabel", () => {
  it("every mark has distinct words, and none is empty", () => {
    const words = STATUS_MARKS.map((m) => MARK_WORDS[m]);
    expect(new Set(words).size).toBe(STATUS_MARKS.length);
    for (const w of words) expect(w.trim()).not.toBe("");
  });

  it("each finer mark has its own words — the state, what qualifies it, then its age", () => {
    expect(formatMarkLabel({ mark: "permission", status: "needs-permission", ageMs: 2 * M })).toBe(
      "Needs approval · 2m",
    );
    expect(formatMarkLabel({ mark: "question", asking: true })).toBe("Needs answer");
    expect(formatMarkLabel({ mark: "blocked", ageMs: 90 * S })).toBe("Blocked · 1m");
    expect(formatMarkLabel({ mark: "thinking", subagents: 2, ageMs: 2 * M })).toBe(
      "Working · 2 subagents · 2m",
    );
    expect(formatMarkLabel({ mark: "tool", subagents: 1 }, ", ")).toBe("Working, 1 subagent");
    // A session that needs you or has stalled says that, whatever runs under it.
    expect(formatMarkLabel({ mark: "permission", subagents: 2 })).toBe("Needs approval");
    expect(agentStatusWords("running", 3)).toBe("Working · 3 subagents");
    // The row's word, with the count carried by its mark beside it.
    expect(agentStatusWord("thinking", 2)).toBe("Working");
    expect(agentStatusWord("thinking")).toBe("Thinking");
    expect(shownSubagents("running", 2)).toBe(2);
    expect(shownSubagents("awaiting-answer", 2)).toBe(0);
    expect(shownSubagents("running", 0)).toBe(0);
    expect(subagentCountWords(1)).toBe("1 subagent");
    expect(agentStatusWords("stalled", 3)).toBe("Stalled");
    expect(agentStatusWords("idle")).toBe("Ready");
    expect(formatMarkLabel({ mark: "crashed", ageMs: 5 * M, exit: "exit 137" })).toBe(
      "Crashed · exit 137 · 5m",
    );
    expect(formatMarkLabel({ mark: "thinking", ageMs: 30 * S })).toBe("Thinking · 30s");
    expect(formatMarkLabel({ mark: "tool", ageMs: 3 * S })).toBe("Running a tool · 3s");
    expect(formatMarkLabel({ mark: "error", ageMs: H })).toBe("Error · 1h");
    expect(formatMarkLabel({ mark: "ready", ageMs: 14 * S })).toBe("Ready · 14s");
    expect(formatMarkLabel({ mark: "stalled", ageMs: 11 * M })).toBe("Stalled · 11m");
    expect(formatMarkLabel({ mark: "offline" })).toBe("Machine offline");
    expect(formatMarkLabel({ mark: "unknown" })).toBe("Activity unknown");
    expect(formatMarkLabel({ mark: "ended", exit: "exit 0" })).toBe("Ended · exit 0");
  });

  it("an accessible name joins the words with commas", () => {
    expect(formatMarkLabel({ mark: "crashed", exit: "exit 137" }, ", ")).toBe("Crashed, exit 137");
    expect(formatMarkLabel({ mark: "unknown" }, ", ")).toBe("Activity unknown");
  });

  it("an unknown exit is left out; only an end carries one", () => {
    expect(formatMarkLabel({ mark: "ready" })).toBe("Ready");
    expect(formatMarkLabel({ mark: "crashed", exit: null })).toBe("Crashed");
    expect(formatMarkLabel({ mark: "error", exit: "exit 1" })).toBe("Error");
  });
});
