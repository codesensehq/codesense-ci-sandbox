// SPDX-License-Identifier: MIT
/**
 * SW-AGR-6: session timers as a tab shows them (design §4.11–§4.13): the
 * countdown chip, the timer rows' words, and "Remind this agent…" turned into
 * the ask the terminal server takes. Pure: every clock read is passed in.
 */

import type { SessionTimerAsk, SessionTimerRow } from "@/tauri/commands";

/** The tab a timer belongs to: its session, on a connected machine or (no
 *  `machineId`) on this Mac. */
export interface TimerTarget {
  sessionId: string;
  machineId?: string;
}

/** The local machine's id, as the backend names it in events. */
export const LOCAL_TIMER_MACHINE = "local";

/** One key per tab across machines: session ids are only unique per server. */
export function timerTargetKey(target: TimerTarget): string {
  return `${target.machineId ?? LOCAL_TIMER_MACHINE}|${target.sessionId}`;
}

/** The most timers a session may hold (SPEC §5). */
export const MAX_TIMERS_PER_SESSION = 10;

const MIN = 60_000;
const HOUR = 60 * MIN;

/** The chip's countdown (§4.12): `<1m`, `18m`, `1h 40m`, `14h`, or `due` once
 *  the fire time passed and it waits for the agent to be idle. */
export function countdownLabel(nextAtMs: number, nowMs: number): string {
  const left = nextAtMs - nowMs;
  if (left <= 0) return "due";
  if (left < MIN) return "<1m";
  if (left < HOUR) return `${Math.floor(left / MIN)}m`;
  const hours = Math.floor(left / HOUR);
  if (hours < 10) return `${hours}h ${Math.floor((left % HOUR) / MIN)}m`;
  return `${hours}h`;
}

/** The countdown read aloud: "18 minutes", "1 hour 40 minutes", "due". */
export function countdownWords(nextAtMs: number, nowMs: number): string {
  const left = nextAtMs - nowMs;
  if (left <= 0) return "now, waiting for the agent to be idle";
  if (left < MIN) return "in less than a minute";
  const minutes = Math.floor(left / MIN);
  if (minutes < 60) return `in ${minutes} ${minutes === 1 ? "minute" : "minutes"}`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  const h = `${hours} ${hours === 1 ? "hour" : "hours"}`;
  return rest && hours < 10 ? `in ${h} ${rest} ${rest === 1 ? "minute" : "minutes"}` : `in ${h}`;
}

/** The parts of a duration such as `1h30m` (units `d`, `h`, `m`, `s`). */
function durationParts(raw: string): Array<[number, string]> | null {
  const parts: Array<[number, string]> = [];
  const re = /(\d+)([dhms])/g;
  let consumed = 0;
  for (let m = re.exec(raw); m; m = re.exec(raw)) {
    parts.push([Number(m[1]), m[2]]);
    consumed += m[0].length;
  }
  return parts.length && consumed === raw.length ? parts : null;
}

/** A repeat interval in words: `15m` → "15 min", `1h` → "1 hour", `1h30m` → "1h 30m". */
export function durationWords(raw: string): string {
  const parts = durationParts(raw.trim());
  if (!parts) return raw;
  if (parts.length > 1) return parts.map(([n, u]) => `${n}${u}`).join(" ");
  const [[n, unit]] = parts;
  switch (unit) {
    case "d":
      return `${n} ${n === 1 ? "day" : "days"}`;
    case "h":
      return `${n} ${n === 1 ? "hour" : "hours"}`;
    case "m":
      return `${n} min`;
    default:
      return `${n} s`;
  }
}

/** How a timer repeats: "Once", "Every 15 min", "Every 15 min until 12:00",
 *  "Every 15 min · 3 of 8 left". `time` formats an instant as a short time. */
export function recurrenceWords(
  row: Pick<SessionTimerRow, "every" | "until" | "count" | "remaining">,
  time: (ms: number) => string,
): string {
  if (!row.every) return "Once";
  const every = `Every ${durationWords(row.every)}`;
  if (row.until) return `${every} until ${time(Date.parse(row.until))}`;
  if (row.count != null) return `${every} · ${row.remaining ?? row.count} of ${row.count} left`;
  return every;
}

/** Who set a timer: the person is "you"; an agent is `agent:<name>`. */
export function authorWords(createdBy: string): string {
  if (createdBy === "you") return "set by you";
  if (createdBy.startsWith("agent:")) return `set by ${createdBy.slice("agent:".length)}`;
  return "set by an agent";
}

/** The locale's short time for an instant ("9:00" or "9:00 AM"). */
export function shortTime(ms: number): string {
  return new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" }).format(ms);
}

/** `ms` as RFC 3339 in this Mac's zone, with its offset (the server wants one). */
export function rfc3339Local(ms: number): string {
  const d = new Date(ms);
  const pad = (n: number) => String(Math.abs(n)).padStart(2, "0");
  const offset = -d.getTimezoneOffset();
  const sign = offset >= 0 ? "+" : "-";
  return (
    `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}` +
    `T${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}` +
    `${sign}${pad(Math.trunc(offset / 60))}:${pad(offset % 60)}`
  );
}

/** What the Remind form holds, as its inputs hold it. */
export interface ReminderInput {
  kind: "in" | "at" | "every";
  amount: string;
  unit: "minutes" | "hours";
  /** `HH:MM` (the `at` time). */
  time: string;
  stop: "tab" | "time" | "count";
  stopTime: string;
  stopCount: string;
  message: string;
  /** The tab's active timers now (the limit is per session). */
  timerCount: number;
}

export type ReminderField = "amount" | "time" | "stopTime" | "stopCount" | "message" | "form";

export type ReminderPlan =
  | {
      ok: true;
      ask: SessionTimerAsk;
      kind: ReminderInput["kind"];
      firstAtMs: number;
      /** The first fire is on a later day than now (an `at` time already
       *  past today, or an interval that crosses midnight). */
      tomorrow: boolean;
      /** The stop time is on a later day than now. */
      untilTomorrow?: boolean;
      every?: string;
      untilMs?: number;
      count?: number;
    }
  | { ok: false; errors: Partial<Record<ReminderField, string>> };

function sameDay(a: number, b: number): boolean {
  return new Date(a).toDateString() === new Date(b).toDateString();
}

/** The next `HH:MM` at or after `nowMs` (today, else tomorrow), or null. */
function nextClockTime(hhmm: string, nowMs: number): { ms: number; tomorrow: boolean } | null {
  const m = /^(\d{1,2}):(\d{2})$/.exec(hhmm.trim());
  if (!m) return null;
  const at = new Date(nowMs);
  at.setHours(Number(m[1]), Number(m[2]), 0, 0);
  if (at.getTime() > nowMs) return { ms: at.getTime(), tomorrow: false };
  at.setDate(at.getDate() + 1);
  return { ms: at.getTime(), tomorrow: true };
}

/** The Remind form as the ask `remind_session` takes, or what to fix (the
 *  design's copy, §4.13). Only durations and offset times leave this Mac. */
export function planReminder(
  input: ReminderInput,
  nowMs: number,
  time: (ms: number) => string = shortTime,
): ReminderPlan {
  const errors: Partial<Record<ReminderField, string>> = {};
  const prompt = input.message.trim();
  if (!prompt) errors.message = "Add a message.";
  if (input.timerCount >= MAX_TIMERS_PER_SESSION) {
    errors.form = `This tab already has ${MAX_TIMERS_PER_SESSION} timers, the most it can have. Cancel one first.`;
  }

  let duration: string | null = null;
  let durationMs = 0;
  if (input.kind !== "at") {
    const n = Number(input.amount.trim());
    if (!input.amount.trim() || !Number.isInteger(n) || n < 1) {
      errors.amount = "At least 1 minute.";
    } else {
      duration = input.unit === "hours" ? `${n}h` : `${n}m`;
      durationMs = n * (input.unit === "hours" ? HOUR : MIN);
    }
  }

  let at: { ms: number; tomorrow: boolean } | null = null;
  if (input.kind === "at") {
    at = nextClockTime(input.time, nowMs);
    if (!at) errors.time = "Pick a time.";
  }

  const firstAtMs = at ? at.ms : nowMs + durationMs;
  let untilMs: number | undefined;
  let count: number | undefined;
  if (input.kind === "every") {
    if (input.stop === "time") {
      const until = nextClockTime(input.stopTime, nowMs);
      if (!until) errors.stopTime = "Pick a time.";
      else if (until.ms <= firstAtMs)
        errors.stopTime = `Pick a time after the first reminder at ${time(firstAtMs)}.`;
      else untilMs = until.ms;
    } else if (input.stop === "count") {
      const n = Number(input.stopCount.trim());
      if (!Number.isInteger(n) || n < 1) errors.stopCount = "At least once.";
      else if (n > 500) errors.stopCount = "At most 500 times.";
      else count = n;
    }
  }

  if (Object.keys(errors).length) return { ok: false, errors };

  const ask: SessionTimerAsk =
    input.kind === "at" ? { at: rfc3339Local(at!.ms), prompt } : { in: duration!, prompt };
  if (input.kind === "every") {
    ask.every = duration!;
    if (untilMs !== undefined) ask.until = rfc3339Local(untilMs);
    if (count !== undefined) ask.count = count;
  }
  return {
    ok: true,
    ask,
    kind: input.kind,
    firstAtMs,
    tomorrow: !sameDay(firstAtMs, nowMs),
    untilTomorrow: untilMs !== undefined ? !sameDay(untilMs, nowMs) : undefined,
    every: input.kind === "every" ? duration! : undefined,
    untilMs,
    count,
  };
}

/** The Remind form's preview line (§4.13). */
export function reminderPreview(
  plan: Extract<ReminderPlan, { ok: true }>,
  agentName: string,
  time: (ms: number) => string,
): string {
  const idle = `when ${agentName} is idle`;
  if (plan.kind === "every" && plan.every) {
    const end =
      plan.untilMs !== undefined
        ? `until ${plan.untilTomorrow ? "tomorrow " : ""}${time(plan.untilMs)}`
        : plan.count !== undefined
          ? `${plan.count} ${plan.count === 1 ? "time" : "times"}`
          : "until the tab ends";
    const from = `${plan.tomorrow ? "tomorrow " : ""}${time(plan.firstAtMs)}`;
    return `Goes in every ${durationWords(plan.every)} from ${from} ${idle}, ${end}.`;
  }
  return `Goes in ${plan.tomorrow ? "tomorrow " : ""}at ${time(plan.firstAtMs)} ${idle}.`;
}

/** A routine's `when` for a timer kept as a routine: its interval as the
 *  routine form reads it (`every 15m`), or for a one-shot the time it fires
 *  as a daily slot. */
export function routineWhenOf(row: Pick<SessionTimerRow, "every" | "nextAt">): string {
  if (row.every) return `every ${row.every}`;
  const at = new Date(Date.parse(row.nextAt));
  if (Number.isNaN(at.getTime())) return "";
  return `daily ${String(at.getHours()).padStart(2, "0")}:${String(at.getMinutes()).padStart(2, "0")}`;
}
