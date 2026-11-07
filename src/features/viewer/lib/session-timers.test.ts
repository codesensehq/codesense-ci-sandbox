// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import {
  authorWords,
  countdownLabel,
  countdownWords,
  durationWords,
  planReminder,
  recurrenceWords,
  reminderPreview,
  rfc3339Local,
  timerTargetKey,
  type ReminderInput,
} from "./session-timers";

const MIN = 60_000;
const HOUR = 60 * MIN;
// 2026-10-05 09:00 local.
const NOW = new Date(2026, 9, 5, 9, 0, 0).getTime();
const time = (ms: number) => {
  const d = new Date(ms);
  return `${d.getHours()}:${String(d.getMinutes()).padStart(2, "0")}`;
};

function input(patch: Partial<ReminderInput> = {}): ReminderInput {
  return {
    kind: "in",
    amount: "20",
    unit: "minutes",
    time: "",
    stop: "tab",
    stopTime: "",
    stopCount: "",
    message: "Check whether the build finished",
    timerCount: 0,
    ...patch,
  };
}

describe("countdownLabel (design §4.12)", () => {
  it.each([
    [30 * 1000, "<1m"],
    [18 * MIN, "18m"],
    [59 * MIN + 59_000, "59m"],
    [HOUR + 40 * MIN, "1h 40m"],
    [9 * HOUR + 5 * MIN, "9h 5m"],
    [14 * HOUR, "14h"],
    [0, "due"],
    [-5 * MIN, "due"],
  ])("%d ms left reads %s", (left, label) => {
    expect(countdownLabel(NOW + left, NOW)).toBe(label);
  });
});

describe("countdownWords (accessible names)", () => {
  it.each([
    [20 * 1000, "in less than a minute"],
    [MIN, "in 1 minute"],
    [18 * MIN, "in 18 minutes"],
    [HOUR + 40 * MIN, "in 1 hour 40 minutes"],
    [14 * HOUR, "in 14 hours"],
  ])("%d ms reads %s", (left, words) => {
    expect(countdownWords(NOW + left, NOW)).toBe(words);
  });
});

describe("timer words", () => {
  it.each([
    ["15m", "15 min"],
    ["1m", "1 min"],
    ["1h", "1 hour"],
    ["2h", "2 hours"],
    ["1h30m", "1h 30m"],
    ["1d", "1 day"],
    ["90s", "90 s"],
  ])("durationWords(%s) = %s", (raw, words) => {
    expect(durationWords(raw)).toBe(words);
  });

  it("recurrence: once, every, every until, every with fires left", () => {
    const base = { every: null, until: null, count: null, remaining: null };
    expect(recurrenceWords(base, time)).toBe("Once");
    expect(recurrenceWords({ ...base, every: "15m" }, time)).toBe("Every 15 min");
    const noon = new Date(2026, 9, 5, 12, 0).toISOString();
    expect(recurrenceWords({ ...base, every: "15m", until: noon }, time)).toBe(
      "Every 15 min until 12:00",
    );
    expect(recurrenceWords({ ...base, every: "15m", count: 8, remaining: 3 }, time)).toBe(
      "Every 15 min · 3 of 8 left",
    );
  });

  it("author: the person is you, an agent by its name", () => {
    expect(authorWords("you")).toBe("set by you");
    expect(authorWords("agent:Barb")).toBe("set by Barb");
    expect(authorWords("agent")).toBe("set by an agent");
  });

  it("a target key names the machine, local by default", () => {
    expect(timerTargetKey({ sessionId: "s1" })).toBe("local|s1");
    expect(timerTargetKey({ sessionId: "s1", machineId: "codesense-test-1" })).toBe(
      "codesense-test-1|s1",
    );
  });

  it("rfc3339Local carries the local offset and round-trips", () => {
    const at = rfc3339Local(NOW);
    expect(at).toMatch(/^2026-10-05T09:00:00[+-]\d\d:\d\d$/);
    expect(new Date(at).getTime()).toBe(NOW);
  });
});

describe("planReminder (Remind this agent…)", () => {
  it("in 20 minutes is the default ask", () => {
    const plan = planReminder(input(), NOW);
    expect(plan.ok).toBe(true);
    if (!plan.ok) return;
    expect(plan.ask).toEqual({ in: "20m", prompt: "Check whether the build finished" });
    expect(plan.firstAtMs).toBe(NOW + 20 * MIN);
  });

  it("in hours", () => {
    const plan = planReminder(input({ amount: "2", unit: "hours" }), NOW);
    expect(plan.ok && plan.ask.in).toBe("2h");
  });

  it("at a time later today, or tomorrow once it has passed", () => {
    const later = planReminder(input({ kind: "at", time: "14:30" }), NOW);
    expect(later.ok).toBe(true);
    if (!later.ok) return;
    expect(new Date(later.ask.at!).getTime()).toBe(new Date(2026, 9, 5, 14, 30).getTime());
    expect(later.tomorrow).toBe(false);
    const passed = planReminder(input({ kind: "at", time: "07:30" }), NOW);
    expect(passed.ok).toBe(true);
    if (!passed.ok) return;
    expect(new Date(passed.ask.at!).getTime()).toBe(new Date(2026, 9, 6, 7, 30).getTime());
    expect(passed.tomorrow).toBe(true);
  });

  it("every N first fires one interval from now, stopping when the tab ends", () => {
    const plan = planReminder(input({ kind: "every", amount: "15" }), NOW);
    expect(plan.ok && plan.ask).toEqual({
      in: "15m",
      every: "15m",
      prompt: "Check whether the build finished",
    });
  });

  it("every N stops at a time or after N times", () => {
    const until = planReminder(
      input({ kind: "every", amount: "15", stop: "time", stopTime: "12:00" }),
      NOW,
    );
    expect(until.ok).toBe(true);
    if (!until.ok) return;
    expect(new Date(until.ask.until!).getTime()).toBe(new Date(2026, 9, 5, 12, 0).getTime());
    const count = planReminder(
      input({ kind: "every", amount: "15", stop: "count", stopCount: "8" }),
      NOW,
    );
    expect(count.ok && count.ask.count).toBe(8);
  });

  it.each([
    [{ message: "  " }, "message", "Add a message."],
    [{ kind: "every" as const, amount: "0" }, "amount", "At least 1 minute."],
    [{ amount: "" }, "amount", "At least 1 minute."],
    [{ kind: "at" as const, time: "" }, "time", "Pick a time."],
    [
      { kind: "every" as const, stop: "count" as const, stopCount: "0" },
      "stopCount",
      "At least once.",
    ],
    [
      { timerCount: 10 },
      "form",
      "This tab already has 10 timers, the most it can have. Cancel one first.",
    ],
  ])("refuses %o on %s", (patch, field, copy) => {
    const plan = planReminder(input(patch), NOW);
    expect(plan.ok).toBe(false);
    if (plan.ok) return;
    expect(plan.errors[field as keyof typeof plan.errors]).toBe(copy);
  });

  it("previews when it goes in, in the design's words", () => {
    const fmt = time;
    const at = (patch: Partial<ReminderInput>) => {
      const plan = planReminder(input(patch), NOW);
      if (!plan.ok) throw new Error("plan");
      return reminderPreview(plan, "Barb", fmt);
    };
    expect(at({})).toBe("Goes in at 9:20 when Barb is idle.");
    expect(at({ kind: "at", time: "07:30" })).toBe("Goes in tomorrow at 7:30 when Barb is idle.");
    expect(at({ kind: "every", amount: "15" })).toBe(
      "Goes in every 15 min from 9:15 when Barb is idle, until the tab ends.",
    );
    expect(at({ kind: "every", amount: "15", stop: "time", stopTime: "12:00" })).toBe(
      "Goes in every 15 min from 9:15 when Barb is idle, until 12:00.",
    );
    expect(at({ kind: "every", amount: "1", unit: "hours", stop: "count", stopCount: "3" })).toBe(
      "Goes in every 1 hour from 10:00 when Barb is idle, 3 times.",
    );
  });

  it("says tomorrow when the first fire or the stop crosses midnight", () => {
    const late = new Date(2026, 9, 5, 23, 50).getTime();
    const plan = planReminder(input({ amount: "20" }), late);
    if (!plan.ok) throw new Error("plan");
    expect(reminderPreview(plan, "Barb", time)).toBe("Goes in tomorrow at 0:10 when Barb is idle.");
    const every = planReminder(
      input({ kind: "every", amount: "15", stop: "time", stopTime: "01:00" }),
      late,
    );
    if (!every.ok) throw new Error("plan");
    expect(reminderPreview(every, "Barb", time)).toBe(
      "Goes in every 15 min from tomorrow 0:05 when Barb is idle, until tomorrow 1:00.",
    );
  });

  it("a stop before the first reminder names the first reminder", () => {
    const plan = planReminder(
      input({ kind: "every", amount: "2", unit: "hours", stop: "time", stopTime: "10:00" }),
      NOW,
      time,
    );
    expect(plan.ok).toBe(false);
    if (plan.ok) return;
    expect(plan.errors.stopTime).toBe("Pick a time after the first reminder at 11:00.");
  });
});
