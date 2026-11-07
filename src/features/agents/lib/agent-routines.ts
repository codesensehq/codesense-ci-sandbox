// SPDX-License-Identifier: MIT
import type {
  AgentRoutineItem,
  AgentRow,
  QueuedWake,
  RoutineDef,
  RoutineMode,
  RoutineRow,
  RoutineRun,
  RoutineRunsOn,
} from "@/tauri/commands";
import {
  relativeTime,
  runsOn,
  scheduleLabel,
  routineListLabel,
} from "@/features/routines/lib/routine-display";
import { eventTriggerWords, firstSchedule } from "@/features/routines/lib/routine-sentence";

/**
 * SW-AGR-5 (design §4.2, §4.4): an agent's routines as the Agents tab reads them — the
 * row's next-run chip, the Routines section's order and each routine's one-line
 * sentence. Routines belong to agents, so everything here is keyed by the agent.
 */

/** The agent's routines (`agent_key` is the agent they live under): enabled first, then
 *  by next fire, then by name. The order never depends on selection. */
export function routinesOf(rows: readonly RoutineRow[], agentKey: string): RoutineRow[] {
  return rows
    .filter((r) => r.agent_key === agentKey && !r.unassigned)
    .sort(
      (a, b) =>
        Number(b.enabled) - Number(a.enabled) ||
        (a.next_run_at ?? Infinity) - (b.next_run_at ?? Infinity) ||
        a.def.name.localeCompare(b.def.name, undefined, { sensitivity: "base" }) ||
        a.key.localeCompare(b.key),
    );
}

const DAY = 24 * 60 * 60_000;

function startOfDay(at: number): number {
  const d = new Date(at);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/** A clock time in the system locale's short form ("9:00", or "9:00 AM"). */
export function clockTime(at: number, locale?: string): string {
  return new Intl.DateTimeFormat(locale, { hour: "numeric", minute: "2-digit" }).format(at);
}

/** A fire time as a chip shows it: today the time, within 6 days the weekday and time,
 *  later the date (design §6). */
export function chipTime(at: number, now: number, locale?: string): string {
  const days = Math.round((startOfDay(at) - startOfDay(now)) / DAY);
  if (days === 0) return clockTime(at, locale);
  if (Math.abs(days) < 7) {
    const weekday = new Intl.DateTimeFormat(locale, { weekday: "short" }).format(at);
    return `${weekday} ${clockTime(at, locale)}`;
  }
  return new Intl.DateTimeFormat(locale, { month: "short", day: "numeric" }).format(at);
}

/** What the next-run chip needs of one routine, from a list row or from `agent_get`. */
export interface ChipRoutine {
  name: string;
  enabled: boolean;
  /** Epoch ms, or null with no scheduled fire. */
  nextRunAt: number | null;
  queued: QueuedWake | null;
  /** Its schedule in words, for the tooltip. */
  words: string | null;
  /** An event-only routine's trigger ("after Barb"). */
  after: string | null;
}

/** "after Barb" for the first event trigger, or null. */
export function afterWords(def: Pick<RoutineDef, "triggers">): string | null {
  for (const t of def.triggers ?? []) {
    if (!("event" in t)) continue;
    if (t.event.kind === "agent_done") return `after ${t.event.from_agent ?? "an agent"}`;
    if (t.event.kind === "inbox_idle") return "on unread mail";
    return `on ${t.event.kind}`;
  }
  return null;
}

export function chipOfRow(row: RoutineRow): ChipRoutine {
  return {
    name: row.def.name,
    enabled: row.enabled,
    nextRunAt: row.next_run_at,
    queued: row.queued ?? null,
    words: firstSchedule(row.def) ? row.schedule_in_words : null,
    after: row.def.after ? row.schedule_in_words : afterWords(row.def),
  };
}

export function chipOfItem(item: AgentRoutineItem): ChipRoutine {
  const at = item.nextRunAt ? Date.parse(item.nextRunAt) : NaN;
  return {
    name: item.name,
    enabled: item.enabled,
    nextRunAt: Number.isFinite(at) ? at : null,
    queued: item.queued,
    words: null,
    after: null,
  };
}

export type NextRunChipView = {
  state: "next" | "due" | "after";
  label: string;
  /** The tooltip and accessible name. */
  tooltip: string;
};

/** The agent row's quiet `⏰ 9:00` chip: `due` while a wake waits for a busy session,
 *  else the next scheduled fire, else an event-only routine's trigger; none when every
 *  routine is off or there are none. */
export function nextRunChip(
  routines: readonly ChipRoutine[],
  now: number,
  locale?: string,
  agent = "the agent",
): NextRunChipView | null {
  const queued = routines.find((r) => r.queued);
  if (queued?.queued) {
    const deadline = queued.queued.deadlineMs;
    const until = deadline ? ` (until ${chipTime(deadline, now, locale)})` : "";
    const verb = queued.queued.unconfirmed ? "may be waiting" : "is waiting";
    return {
      state: "due",
      label: "due",
      tooltip: `${queued.name} ${verb} for ${agent} to be idle${until}`,
    };
  }
  const on = routines.filter((r) => r.enabled);
  const next = on
    .filter((r) => r.nextRunAt !== null)
    .sort((a, b) => a.nextRunAt! - b.nextRunAt! || a.name.localeCompare(b.name))[0];
  if (next && next.nextRunAt !== null) {
    const words = next.words ? `, ${next.words}` : "";
    return {
      state: "next",
      label: chipTime(next.nextRunAt, now, locale),
      tooltip: `Next: ${next.name}${words} (${relativeTime(next.nextRunAt, now)})`,
    };
  }
  const after = on.find((r) => r.after);
  if (after?.after) {
    return { state: "after", label: after.after, tooltip: `${after.name} runs ${after.after}` };
  }
  return null;
}

/** The mode word of a read-only sentence: "wake" or "start a new", the approved
 *  wording, the same as the sentence form's. */
export function modeWord(mode: RoutineMode): string {
  return mode === "wake" ? "wake" : "start a new";
}

/** Where a routine wakes or starts its agent, in words. */
export function machineWords(
  machine: string | undefined,
  labels: ReadonlyMap<string, string>,
  mode: RoutineRunsOn = "one_mac",
): string {
  if (mode === "each_mac") return "on each teammate's Mac";
  if (!machine || machine === "local") return "on This Mac";
  if (machine === "any") return "on any connected machine";
  return `on ${labels.get(machine) ?? machine}`;
}

/** The longest a task runs in a one-line sentence. */
const TASK_CHARS = 60;

/** A task's first line, quoted, cut at 60 characters. */
export function taskWords(task: string): string {
  const first = task.trim().split("\n")[0]?.trim() ?? "";
  const cut = first.length > TASK_CHARS ? `${first.slice(0, TASK_CHARS - 1)}…` : first;
  return `“${cut}”`;
}

export interface RoutineSentence {
  when: string;
  mode: string;
  task: string;
  machine: string;
}

/** A routine in one line: schedule or trigger · mode word · "task" · on ‹machine›. */
export function routineSentence(
  row: RoutineRow,
  labels: ReadonlyMap<string, string>,
  compact = false,
): RoutineSentence {
  const events = (row.def.triggers ?? []).flatMap((t) =>
    "event" in t ? [eventTriggerWords(t.event)] : [],
  );
  const eventWords = events.join("; ");
  const when = compact
    ? routineListLabel(row)
    : !firstSchedule(row.def) && eventWords
      ? eventWords.charAt(0).toUpperCase() + eventWords.slice(1)
      : scheduleLabel(row);
  return {
    when,
    mode: modeWord(row.effective_mode),
    task: taskWords(row.def.task ?? ""),
    machine: machineWords(row.def.where.machine, labels, runsOn(row.def)),
  };
}

/** The agent row's routines link: "2 routines", none when it has no routines. */
export function routinesLabel(
  row: Pick<AgentRow, "routines" | "archived_routines" | "archived">,
): string | null {
  const plural = (n: number) => (n === 1 ? "1 routine" : `${n} routines`);
  if (row.archived) {
    const n = row.archived_routines?.length ?? 0;
    if (n > 0) return `archived · ${plural(n)} archived`;
    // Routines from before they belonged to agents can outlive its archive.
    const active = row.routines?.length ?? 0;
    return active > 0 ? `archived · ${plural(active)}` : null;
  }
  const n = row.routines?.length ?? 0;
  return n > 0 ? plural(n) : null;
}

/** A list row's last run in the run shape its Details popover reads. */
export function lastRunAsRun(row: RoutineRow): RoutineRun | null {
  const last = row.last_run;
  if (!last) return null;
  return {
    run_id: last.run_id,
    routine_key: row.key,
    revision: row.def.revision,
    source: "schedule",
    occurrence_key: null,
    phase: last.phase,
    reason: last.reason,
    linked_run_id: null,
    session_id: last.session_id ?? null,
    created_at: last.at,
    started_at: last.at,
    ended_at: last.at,
    result_text: last.result_text,
    detail: last.detail,
    agent: null,
    ...(last.delivered_at !== undefined ? { delivered_at: last.delivered_at } : {}),
    ...(last.wake_machine !== undefined ? { wake_machine: last.wake_machine } : {}),
  };
}

/** The routine whose wake is queued for the session `sessionId` on `machineId` (`local`
 *  for This Mac), if any: the sidebar's `due` marker. Session ids are matched with their
 *  machine, so a remote session never wears a local one's marker. */
export function queuedWakeFor(
  rows: readonly RoutineRow[] | null,
  sessionId: string | null,
  machineId: string,
): RoutineRow | null {
  if (!rows || !sessionId) return null;
  return (
    rows.find(
      (r) => r.queued?.sessionId === sessionId && (r.queued.machineId || "local") === machineId,
    ) ?? null
  );
}
